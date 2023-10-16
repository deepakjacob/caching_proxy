use env_logger;
use hyper::http::StatusCode;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Client, Request, Response, Server, Uri};
use log::{error, info};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn read_cache(cache_file_path: &Path) -> Option<Vec<u8>> {
    if let Ok(mut file) = File::open(cache_file_path).await {
        let mut buffer = Vec::new();
        if file.read_to_end(&mut buffer).await.is_ok() {
            return Some(buffer);
        }
    }
    None
}

async fn write_cache(cache_file_path: &Path, data: &[u8]) {
    info!("the size of data to be writtin is {:?}", data.len());
    if let Ok(mut file) = File::create(cache_file_path).await {
        if file.write_all(data).await.is_err() {
            error!("Failed to write to cache file.");
        }
    }
}

fn compute_hash<T: Hash>(data: &T) -> String {
    let mut s = DefaultHasher::new();
    data.hash(&mut s);
    s.finish().to_string()
}

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
        .uri(forward_uri)
        .version(original_req.version());

    let headers = req_builder.headers_mut().unwrap();
    headers.extend(original_req.headers().clone());

    // Build the new request with the new body
    let new_req = req_builder.body(new_body).unwrap();

    let res = client.request(new_req).await?;
    Ok(res)
}

async fn serve_from_cache_or_fallback(
    cache_file_path: &Path,
    forward_uri: Uri,
    original_req: Request<Body>,
) -> Result<Response<Body>, hyper::Error> {
    if let Some(cached_data) = read_cache(cache_file_path).await {
        info!("cache hit with the key: {:?}", &cache_file_path);
        println!("cache hit with the key: {:?}", &cache_file_path);
        Ok(Response::new(Body::from(cached_data)))
    } else {
        match fetch_from_server(forward_uri, original_req).await {
            Ok(mut original_res) => {
                let bytes_result = hyper::body::to_bytes(original_res.body_mut()).await?;
                write_cache(cache_file_path, &bytes_result).await;

                let mut new_res = Response::builder()
                    .status(original_res.status())
                    .version(original_res.version())
                    .body(Body::from(bytes_result.clone()))
                    .unwrap();

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

async fn handle_request(req: Request<Body>) -> Result<Response<Body>, hyper::Error> {
    info!("Received a new request: {:?}", req);

    let path_and_query = match req.uri().path_and_query() {
        Some(pq) => pq.to_string(),
        None => String::from("/"),
    };

    let forward_uri: Uri = format!("http://localhost:3000{}", path_and_query)
        .parse()
        .unwrap();

    let hash_code = compute_hash(&forward_uri.to_string());

    let cache_path = format!("cache_{}", hash_code);
    let cache_file_path = Path::new(&cache_path);

    serve_from_cache_or_fallback(&cache_file_path, forward_uri, req).await
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    env_logger::init();

    let addr = "127.0.0.1:8080".parse()?;
    let service =
        make_service_fn(|_conn| async { Ok::<_, hyper::Error>(service_fn(handle_request)) });
    let server = Server::bind(&addr).serve(service);

    info!("Server running on http://{}", addr);

    server.await?;

    Ok(())
}
