use env_logger;
use hyper::body::Bytes;
use hyper::http::StatusCode;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Client, Request, Response, Server, Uri};
use log::{error, info};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

// TODO: tokio has an implementation of RwLock - need to check
use std::sync::{Arc, RwLock};

type Cache = Arc<RwLock<HashMap<String, Bytes>>>;

fn read_cache1<'a>(cache: &'a Cache, cache_key: String) -> Option<&'a Bytes> {
    let read_guard = cache.read().unwrap();
    //TODO: check why we need to provide the reference here?
    // cache_key itself is &str
    read_guard.get(&cache_key.to_lowercase())
}

fn write_cache1(cache: Cache, cache_key: &str, bytes: &Bytes) {
    let mut write_guard = cache.write().unwrap();
    write_guard.insert(cache_key.to_lowercase(), bytes.clone());
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
    cache: Cache,
    cache_path: &str,
    forward_uri: Uri,
    original_req: Request<Body>,
) -> Result<Response<Body>, hyper::Error> {
    let read_guard = cache.read().unwrap();
    if let Some(cached_data) = read_guard.get(cache_path) {
        info!("cache hit with the key: {:?}", &cache_path);
        Ok(Response::new(Body::from(cached_data.clone())))
    } else {
        match fetch_from_server(forward_uri, original_req).await {
            Ok(mut original_res) => {
                let bytes_result = hyper::body::to_bytes(original_res.body_mut()).await?;

                let mut write_guard = cache.write().unwrap();
                write_guard.insert(cache_path.to_lowercase(), bytes_result.clone());

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

#[tokio::main]
async fn main() {
    env_logger::init();

    let cache = Arc::new(RwLock::new(HashMap::new()));
    let service = make_service_fn(move |_conn| {
        let cache = cache.clone();
        async { Ok::<_, hyper::Error>(service_fn(move |req| proxy(cache, req))) }
    });
    let addr = ([127, 0, 0, 1], 3000).into();
    let server = Server::bind(&addr).serve(service);

    info!("Server running on http://{}", addr);

    if let Err(e) = server.await {
        eprintln!("server error: {}", e);
    }
}
async fn proxy(cache: Cache, req: Request<Body>) -> Result<Response<Body>, hyper::Error> {
    info!("Received a new request: {:?}", req);
    let path_and_query = match req.uri().path_and_query() {
        Some(pq) => pq.to_string(),
        None => String::from("/"),
    };

    let forward_uri: Uri = format!("http://localhost:3000{}", path_and_query)
        .parse()
        .unwrap();

    let hash_code = compute_hash(&forward_uri.to_string().to_lowercase());
    let cache_key = format!("{}", hash_code);
    serve_from_cache_or_fallback(cache, &cache_key, forward_uri, req).await
}

fn compute_hash<T: Hash>(data: &T) -> String {
    let mut s = DefaultHasher::new();
    data.hash(&mut s);
    s.finish().to_string()
}
