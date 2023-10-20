use crate::cache::{write_cache_to_fs, Cache, ResponseEntry};
use hyper::{Body, Client, Request, Response, StatusCode, Uri};
use log::{debug, error, info};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

pub async fn proxy(cache: Cache, req: Request<Body>) -> Result<Response<Body>, hyper::Error> {
    info!("received a new request: {:?}", req);
    let path_and_query = match req.uri().path_and_query() {
        Some(pq) => pq.to_string(),
        None => String::from("/"),
    };

    let forward_uri: Uri = format!("http://localhost:3000{}", path_and_query)
        .parse()
        .unwrap();
    debug!("the forward uri: {:?}", forward_uri);
    let hash_code = compute_hash(&forward_uri.to_string().to_lowercase());
    let cache_key = format!("{}", hash_code);
    debug!("generated cache key: {:?}", &cache_key);
    serve_from_cache_or_fallback(cache, &cache_key, forward_uri, req, &path_and_query).await
}

async fn serve_from_cache_or_fallback(
    cache: Cache,
    cache_path: &str,
    forward_uri: Uri,
    original_req: Request<Body>,
    path_and_query: &str,
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

                _ = write_cache_to_fs(&original_res, &bytes_result, cache_path).await;

                if !bytes_result.is_empty() {
                    debug!("caching an empty response for {:?}", path_and_query);
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

fn compute_hash<T: Hash>(data: &T) -> String {
    let mut s = DefaultHasher::new();
    data.hash(&mut s);
    s.finish().to_string()
}
