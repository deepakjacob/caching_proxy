use async_lock::RwLock;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Client, Request, Response, Server, Uri};
use std::collections::HashMap;
use std::sync::Arc;

type Cache = Arc<RwLock<HashMap<String, Vec<u8>>>>;

async fn forward_request(
    cache: Cache,
    mut req: Request<Body>,
) -> Result<Response<Body>, hyper::Error> {
    // Only consider the path (ignoring the query string)
    let path = req.uri().path().to_string();

    // Check if the cache already contains the response
    {
        let read_guard = cache.read().await;
        if let Some(cached_body) = read_guard.get(&path) {
            return Ok(Response::new(Body::from(cached_body.clone())));
        }
    }

    // Rewrite the URI to forward to httpbin.org
    let new_uri_str = format!("http://httpbin.org{}", req.uri().path());
    let new_uri: Uri = new_uri_str.parse().unwrap();
    *req.uri_mut() = new_uri;

    // Forward the request to httpbin.org
    let client = Client::new();
    let mut res = client.request(req).await?;

    let mut body_bytes = hyper::body::to_bytes(res.body_mut()).await?;

    // Cache the response
    {
        let mut write_guard = cache.write().await;
        write_guard.insert(path, body_bytes.to_vec());
    }

    Ok(Response::new(Body::from(body_bytes)))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let cache = Arc::new(RwLock::new(HashMap::new()));
    let make_svc = make_service_fn(move |_conn| {
        let cache = cache.clone();
        async move {
            Ok::<_, hyper::Error>(service_fn(move |req| {
                let cache = cache.clone();
                async move { forward_request(cache, req).await }
            }))
        }
    });

    let addr = ([127, 0, 0, 1], 3000).into();
    let server = Server::bind(&addr).serve(make_svc);

    println!("Listening on http://{}", addr);

    server.await?;

    Ok(())
}
