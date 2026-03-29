use crate::reed_solomon;
use fastcdc::v2020::FastCDC;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const CDC_MIN_SIZE: u32 = 512 * 1024;      // 512 KB
const CDC_AVG_SIZE: u32 = 1024 * 1024;     // 1 MB
const CDC_MAX_SIZE: u32 = 2 * 1024 * 1024; // 2 MB

pub fn blob_path(blob_dir: &Path, hash: &str) -> PathBuf {
    blob_dir.join(&hash[..2]).join(&hash[2..])
}

pub fn read_blob(blob_dir: &Path, hash: &str) -> Vec<u8> {
    let raw = std::fs::read(blob_path(blob_dir, hash)).unwrap_or_default();
    if raw.is_empty() {
        return raw;
    }
    reed_solomon::decode(&raw)
}

pub fn write_blob(blob_dir: &Path, data: &[u8]) -> String {
    let hash = hex::encode(Sha256::digest(data));
    let path = blob_path(blob_dir, &hash);
    if !path.exists() {
        std::fs::create_dir_all(path.parent().unwrap()).ok();
        let encoded = reed_solomon::encode(data);
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &encoded).expect("Failed to write blob");
        std::fs::rename(&tmp, &path).expect("Failed to rename blob");
    }
    hash
}

pub fn cdc_chunk_and_write(blob_dir: &Path, data: &[u8]) -> Vec<String> {
    if data.is_empty() {
        return vec![];
    }
    FastCDC::new(data, CDC_MIN_SIZE, CDC_AVG_SIZE, CDC_MAX_SIZE)
        .map(|chunk| write_blob(blob_dir, &data[chunk.offset..chunk.offset + chunk.length]))
        .collect()
}
