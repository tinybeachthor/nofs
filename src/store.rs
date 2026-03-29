use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TreeEntry {
    pub name: String,
    pub inode: u64,
    pub file_type: u8,
    pub permissions: u16,
    pub uid: u32,
    pub gid: u32,
    pub size: u64,
    pub nlink: u32,
    pub rdev: u32,
    pub atime_secs: i64,
    pub atime_nsecs: u32,
    pub mtime_secs: i64,
    pub mtime_nsecs: u32,
    pub ctime_secs: i64,
    pub ctime_nsecs: u32,
    pub blob_hashes: Vec<String>,
    pub symlink_target: Option<String>,
    pub subtree_hash: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TreeObject {
    pub inode: u64,
    pub permissions: u16,
    pub uid: u32,
    pub gid: u32,
    pub size: u64,
    pub nlink: u32,
    pub atime_secs: i64,
    pub atime_nsecs: u32,
    pub mtime_secs: i64,
    pub mtime_nsecs: u32,
    pub ctime_secs: i64,
    pub ctime_nsecs: u32,
    pub entries: Vec<TreeEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommitObject {
    pub parent_hash: Option<String>,
    pub root_tree_hash: String,
    pub timestamp_secs: i64,
    pub next_inode: u64,
    pub next_fh: u64,
}

fn tree_path(trees_dir: &Path, hash: &str) -> PathBuf {
    trees_dir.join(&hash[..2]).join(&hash[2..])
}

fn commit_path(commits_dir: &Path, hash: &str) -> PathBuf {
    commits_dir.join(&hash[..2]).join(&hash[2..])
}

pub fn write_tree_object(trees_dir: &Path, tree: &TreeObject) -> String {
    let json = serde_json::to_vec(tree).expect("serialize tree");
    let hash = hex::encode(Sha256::digest(&json));
    let path = tree_path(trees_dir, &hash);
    if !path.exists() {
        std::fs::create_dir_all(path.parent().unwrap()).ok();
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &json).expect("write tree");
        std::fs::rename(&tmp, &path).expect("rename tree");
    }
    hash
}

pub fn read_tree_object(trees_dir: &Path, hash: &str) -> TreeObject {
    let json = std::fs::read(tree_path(trees_dir, hash)).expect("read tree");
    serde_json::from_slice(&json).expect("deserialize tree")
}

pub fn write_commit_object(commits_dir: &Path, commit: &CommitObject) -> String {
    let json = serde_json::to_vec(commit).expect("serialize commit");
    let hash = hex::encode(Sha256::digest(&json));
    let path = commit_path(commits_dir, &hash);
    if !path.exists() {
        std::fs::create_dir_all(path.parent().unwrap()).ok();
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &json).expect("write commit");
        std::fs::rename(&tmp, &path).expect("rename commit");
    }
    hash
}

pub fn read_commit_object(commits_dir: &Path, hash: &str) -> CommitObject {
    let json = std::fs::read(commit_path(commits_dir, hash)).expect("read commit");
    serde_json::from_slice(&json).expect("deserialize commit")
}
