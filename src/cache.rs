use bincode::{deserialize, serialize};
use hyper::body::Bytes;
use hyper::http::{HeaderName, HeaderValue};
use hyper::HeaderMap;
use hyper::{Body, Response};
use log::{debug, info};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Error as IoError;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs::{read_dir, File};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::RwLock;

pub type Cache = Arc<RwLock<HashMap<String, ResponseEntry>>>;

#[derive(Debug)]
pub struct ResponseEntry {
    pub headers: HeaderMap,
    pub body: Bytes,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FileEntry {
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

pub async fn read_and_deserialize_file(path: PathBuf) -> Result<FileEntry, IoError> {
    let mut file = File::open(path).await.unwrap();
    let mut buffer = Vec::new();
    file.read_to_end(&mut buffer).await.unwrap();
    let decoded: FileEntry = deserialize(&buffer).unwrap();
    Ok(decoded)
}

pub async fn write_cache_to_fs(
    original_res: &Response<Body>,
    bytes_result: &Bytes,
    cache_path: &str,
) {
    let file_entry = FileEntry {
        headers: header_map_to_hash_map(&original_res.headers()),
        body: bytes_result.clone().to_vec(),
    };
    let encoded: Vec<u8> = serialize(&file_entry).unwrap();
    debug!("serialized struct into Vec<u8> - {:?}", encoded.len());
    tokio::fs::create_dir_all("cache").await.unwrap();
    // Asynchronously write to a file
    let file_path = format!("cache/{cache_path}.bin");
    let mut file = File::create(&file_path).await.unwrap();
    file.write_all(&encoded).await.unwrap();
    info!("writing cache into file://{cache_path} system success!");
}

pub async fn read_fs_into_cache(cache: Cache) -> Result<(), IoError> {
    debug!("attempting to read cache files");
    let mut dir = read_dir("cache").await?;
    while let Some(entry) = dir.next_entry().await? {
        debug!("reading fs cache file: {:?}", entry);
        let path: PathBuf = entry.path();
        let path_clone = path.clone();
        if path.extension().unwrap_or_default() == "bin" {
            let decoded: FileEntry = read_and_deserialize_file(path).await.unwrap();
            debug!("decoded struct body, len: {:?}", decoded.body.len());
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
