use bincode::{deserialize, serialize};
use env_logger;
use hyper::body::Bytes;
use hyper::http::{HeaderName, HeaderValue, StatusCode};
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Client, HeaderMap, Request, Response, Server, Uri};
use log::{debug, error, info};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io::Error as IoError;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs::{read_dir, File};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::RwLock;

#[derive(Debug)]
struct ResponseEntry {
    headers: HeaderMap,
    body: Bytes,
}

#[derive(Debug, Serialize, Deserialize)]
struct FileEntry {
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

type Cache = RwLock<HashMap<String, ResponseEntry>>;

async fn fetch_from_server(
    forward_uri: Uri,
    mut original_req: Request<Body>,
) -> Result<Response<Body>, hyper::Error> {
    let client = Client::new();
    // Convert the original body to bytes
    let original_body_bytes = hyper::body::to_bytes(original_req.body_mut()).await?;
    let new_body = Body::from(original_body_bytes.clone());

    // Clone all headers from the original request
    let mut req_builder = Request::builder()
        .method(original_req.method())
        .uri(forward_uri);

    let headers = req_builder.headers_mut().unwrap();
    headers.extend(original_req.headers().clone());

    // Build the new request with the new body
    let new_req = req_builder.body(new_body).unwrap();

    info!("new request {:?}", new_req);
    let res = client.request(new_req).await?;
    info!("got response {:?}", res);
    Ok(res)
}

async fn serve_from_cache_or_fallback(
    cache: Arc<Cache>,
    cache_path: &str,
    forward_uri: Uri,
    original_req: Request<Body>,
) -> Result<Response<Body>, hyper::Error> {
    {
        let read_guard = cache.read().await;
        if let Some(cached_data) = read_guard.get(cache_path) {
            info!(
                "cache exists for the url: {:?} - key {:?}",
                forward_uri,
                &cache_path.to_lowercase()
            );
            let mut new_res = Response::builder()
                .body(Body::from(cached_data.body.clone()))
                .unwrap();
            *new_res.headers_mut() = cached_data.headers.clone();
            return Ok(new_res);
        }
    }

    {
        info!("cache miss with uri: {:?}", &forward_uri);
        match fetch_from_server(forward_uri, original_req).await {
            Ok(mut original_res) => {
                let bytes_result = hyper::body::to_bytes(original_res.body_mut()).await?;
                info!("response bytes length {}", &bytes_result.len());
                if !bytes_result.is_empty() {
                    let mut write_guard = cache.write().await;
                    debug!("got the write lock");
                    let response_entry = ResponseEntry {
                        headers: original_res.headers().clone(),
                        body: bytes_result.clone(),
                    };
                    write_guard.insert(cache_path.to_lowercase(), response_entry);

                    debug!(
                        "written to cache key {} - bytes {}",
                        cache_path,
                        bytes_result.len()
                    );

                    // -------------- file system write start ------------
                    let file_entry = FileEntry {
                        headers: header_map_to_hash_map(&original_res.headers()),
                        body: bytes_result.clone().to_vec(),
                    };
                    debug!("creation of in memory file entry successful");
                    let encoded: Vec<u8> = serialize(&file_entry).unwrap();
                    debug!("serialized struct into Vec<u8> - {:?}", encoded.len());
                    tokio::fs::create_dir_all("cache").await.unwrap();
                    // Asynchronously write to a file
                    let file_path = format!("cache/{cache_path}.bin");
                    debug!("the file path is {file_path}");
                    let mut file = File::create(&file_path).await.unwrap();
                    debug!("created file with name {}", file_path);
                    file.write_all(&encoded).await.unwrap();

                    info!("writing cache into file://{cache_path} system success!");
                    // --------------- file system write end -------------
                } else {
                    info!("caching not attempted as response is empty!");
                }

                let mut new_res = Response::builder()
                    .status(original_res.status())
                    .version(original_res.version())
                    .body(Body::from(bytes_result.clone()))
                    .unwrap();
                // let h = original_res.headers().clone();
                *new_res.headers_mut() = original_res.headers().clone();
                Ok(new_res)
            }
            Err(_) => {
                error!("Failed to fetch from server.");
                Ok(Response::builder()
                    .status(StatusCode::BAD_GATEWAY)
                    .body(Body::from("Bad Gateway"))
                    .unwrap())
            }
        }
    }
}

#[tokio::main]
async fn main() {
    env_logger::init();

    let cache: Arc<Cache> = Arc::new(RwLock::new(HashMap::new()));

    _ = read_fs_into_cache(cache.clone()).await;

    let service = make_service_fn(move |_| {
        let cache = cache.clone();
        async { Ok::<_, hyper::Error>(service_fn(move |req| proxy(cache.clone(), req))) }
    });
    let addr = ([127, 0, 0, 1], 8080).into();
    let server = Server::bind(&addr).serve(service);

    info!("Server running on http://{}", addr);

    if let Err(e) = server.await {
        eprintln!("server error: {}", e);
    }
}
async fn proxy(cache: Arc<Cache>, req: Request<Body>) -> Result<Response<Body>, hyper::Error> {
    info!("---------------------------------------------------------------------------");
    info!("Received a new request: {:?}", req);
    let path_and_query = match req.uri().path_and_query() {
        Some(pq) => pq.to_string(),
        None => String::from("/"),
    };

    let forward_uri: Uri = format!("http://localhost:3000{}", path_and_query)
        .parse()
        .unwrap();
    info!("the forward uri: {:?}", forward_uri);
    let hash_code = compute_hash(&forward_uri.to_string().to_lowercase());
    let cache_key = format!("{}", hash_code);
    info!("generated cache key: {:?}", &cache_key);
    serve_from_cache_or_fallback(cache, &cache_key, forward_uri, req).await
}

async fn read_fs_into_cache(cache: Arc<Cache>) -> Result<(), IoError> {
    debug!("attempting to read cache files");
    let mut dir = read_dir("cache").await?;
    while let Some(entry) = dir.next_entry().await? {
        debug!("reading fs cache file: {:?}", entry);
        let path: PathBuf = entry.path();
        let path_clone = path.clone();
        if path.extension().unwrap_or_default() == "bin" {
            let decoded: FileEntry = read_and_deserialize_file(path).await.unwrap();
            debug!("Decoded struct body from fs len: {:?}", decoded.body.len());
            {
                let mut write_guard = cache.write().await;
                debug!("got the write lock for writing from fs to cache");
                let response_entry = ResponseEntry {
                    headers: convert_hashmap_to_headermap(decoded.headers).unwrap(),
                    body: Bytes::from(decoded.body),
                };
                if let Some(stem) = path_clone.file_stem() {
                    if let Some(stem_str) = stem.to_str() {
                        debug!("inserting cache entry: {}", stem_str);
                        write_guard.insert(stem_str.to_lowercase(), response_entry);
                    }
                }
            }
        }
    }
    Ok(())
}

async fn read_and_deserialize_file(path: PathBuf) -> Result<FileEntry, IoError> {
    let mut file = File::open(path).await.unwrap();
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer).await.unwrap();
    let decoded: FileEntry = deserialize(&buffer).unwrap();
    Ok(decoded)
}

fn compute_hash<T: Hash>(data: &T) -> String {
    let mut s = DefaultHasher::new();
    data.hash(&mut s);
    s.finish().to_string()
}

// Convert hyper::HeaderMap to HashMap<String, String>
fn header_map_to_hash_map(header_map: &HeaderMap) -> HashMap<String, String> {
    header_map
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap().to_string()))
        .collect()
}

fn convert_hashmap_to_headermap(
    hash_map: HashMap<String, String>,
) -> Result<HeaderMap, hyper::http::Error> {
    let mut header_map = HeaderMap::new();
    for (k, v) in hash_map {
        let key = HeaderName::try_from(k)?;
        let value = HeaderValue::try_from(v)?;
        header_map.insert(key, value);
    }
    Ok(header_map)
}
