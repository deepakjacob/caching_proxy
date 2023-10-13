use async_lock::RwLock;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Client, HeaderMap, Method, Request, Response, Server, Uri};
use serde::{Deserialize, Serialize};
use serde_json::{from_str, to_string};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::prelude::*;
use std::path::Path;
use std::sync::Arc;

type Cache = Arc<RwLock<HashMap<(Method, String), Vec<u8>>>>;

#[derive(Serialize, Deserialize)]
struct CacheEntry {
    complete_url: String,
    path: String,
    method: String,
    query_string: Option<String>,
    request_headers: HashMap<String, String>,
    response_headers: HashMap<String, String>,
    response_status: u16,
    response_body: String,
}

fn header_map_to_hash_map(headers: &HeaderMap) -> HashMap<String, String> {
    headers
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap().to_string()))
        .collect()
}

async fn forward_request(
    cache: Cache,
    mut req: Request<Body>,
) -> Result<Response<Body>, hyper::Error> {
    let path = req.uri().path().to_string();
    let cloned_headers = req.headers().clone(); // Clone headers here
    let method = req.method().clone(); // Clone the Method

    {
        let read_guard = cache.read().await;
        if let Some(cached_body) = read_guard.get(&(method.clone(), path.clone())) {
            return Ok(Response::new(Body::from(cached_body.clone())));
        }
    }
    let new_uri_str = format!(
        "http://httpbin.org{}{}",
        req.uri().path(),
        req.uri()
            .query()
            .map_or(String::new(), |v| format!("?{}", v))
    );
    let new_uri: Uri = new_uri_str.parse().unwrap();

    // let new_uri_str = format!("http://httpbin.org{}", req.uri().path());
    // let new_uri: Uri = new_uri_str.parse().unwrap();
    *req.uri_mut() = new_uri.clone();

    let client = Client::new();
    let mut res = client.request(req).await?;
    let mut body_bytes = hyper::body::to_bytes(res.body_mut()).await?;

    let cache_entry = CacheEntry {
        complete_url: new_uri_str.clone(),
        path: path.clone(),
        method: method.to_string(),
        query_string: new_uri.query().map(|s| s.to_string()),
        request_headers: header_map_to_hash_map(&cloned_headers), // Use the cloned headers
        response_headers: header_map_to_hash_map(res.headers()),
        response_status: res.status().as_u16(),
        response_body: String::from_utf8_lossy(&body_bytes).to_string(),
    };

    let json_entry = to_string(&cache_entry).unwrap();
    let file_path = Path::new("cache").join(path.trim_matches('/'));
    let mut file = File::create(file_path).unwrap();
    file.write_all(json_entry.as_bytes()).unwrap();

    {
        let mut write_guard = cache.write().await;
        write_guard.insert((method, path), body_bytes.to_vec());
    }

    Ok(Response::new(Body::from(body_bytes)))
}
async fn populate_cache_from_folder(cache: &Cache, folder_name: &str) -> std::io::Result<()> {
    match fs::read_dir(folder_name) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                let path = entry.path();
                if path.is_file() {
                    let mut file = File::open(&path)?;
                    let mut contents = String::new();
                    file.read_to_string(&mut contents)?;
                    let cache_entry: CacheEntry = from_str(&contents).unwrap();
                    let method = Method::from_bytes(cache_entry.method.as_bytes()).unwrap();
                    let cache_key = (method, cache_entry.path.clone());

                    // Print the cache key
                    println!("Populating cache with key: {:?}", cache_key);

                    {
                        let mut write_guard = cache.write().await;
                        write_guard.insert(cache_key, cache_entry.response_body.into_bytes());
                    }
                }
            }
        }
        Err(_) => {
            println!("Cache folder not found. Skipping cache population.");
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    std::fs::create_dir_all("cache").unwrap();

    let cache = Arc::new(RwLock::new(HashMap::new()));

    let _ = populate_cache_from_folder(&cache, "./cache").await;
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
