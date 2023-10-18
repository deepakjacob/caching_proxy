use env_logger;
use hyper::body::Bytes;
use hyper::http::StatusCode;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Client, HeaderMap, Request, Response, Server, Uri};
use log::{error, info};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use tokio::sync::RwLock;

use std::sync::Arc;

#[derive(Debug)]
struct ResponseEntry {
    headers: HeaderMap,
    body: Bytes,
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
            info!("cache hit with the key: {:?}", &cache_path.to_lowercase());
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
                info!("bytes came from response {}", bytes_result.len());

                let mut write_guard = cache.write().await;
                info!("got the write lock");
                let response_entry = ResponseEntry {
                    headers: original_res.headers().clone(),
                    body: bytes_result.clone(),
                };
                write_guard.insert(cache_path.to_lowercase(), response_entry);

                info!(
                    "writing the cache ->  key {} - bytes {}",
                    cache_path,
                    bytes_result.len()
                );
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
    info!("\n\n---------------------------------------------------------------------------");
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
    info!("forwarding the request, cache key: {:?}", &cache_key);
    serve_from_cache_or_fallback(cache, &cache_key, forward_uri, req).await
}

fn compute_hash<T: Hash>(data: &T) -> String {
    let mut s = DefaultHasher::new();
    data.hash(&mut s);
    s.finish().to_string()
}
