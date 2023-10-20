mod cache;
mod proxy;

use crate::cache::read_fs_into_cache;
use cache::Cache;
use env_logger;
use hyper::service::{make_service_fn, service_fn};
use hyper::Server;
use log::info;
use proxy::proxy;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

#[tokio::main]
async fn main() {
    env_logger::init();

    // cache is Arc, so that it can be shared between threads
    // RWLock allows many reading threads and one writing thread
    let cache: Cache = Arc::new(RwLock::new(HashMap::new()));
    // when the server is starting reading from disk cache
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
