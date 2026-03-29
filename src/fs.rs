use fuser::{
    FileAttr, FileType, Filesystem, KernelConfig, ReplyAttr, ReplyCreate, ReplyData,
    ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen, ReplyStatfs, ReplyWrite, Request, TimeOrNow,
};
use libc;
use sha2::{Digest, Sha256};
use fastcdc::v2020::FastCDC;

use crate::reed_solomon;

use serde::{Deserialize, Serialize};

use std::collections::HashMap;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const TTL: Duration = Duration::from_secs(1);
const FUSE_ROOT_INODE: u64 = 1;
const FLUSH_INTERVAL: Duration = Duration::from_secs(30);
const CDC_MIN_SIZE: u32 = 512 * 1024;      // 512 KB
const CDC_AVG_SIZE: u32 = 1024 * 1024;     // 1 MB
const CDC_MAX_SIZE: u32 = 2 * 1024 * 1024; // 2 MB

fn now_timespec() -> (i64, u32) {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (d.as_secs() as i64, d.subsec_nanos())
}

fn file_type_to_u8(ft: FileType) -> u8 {
    match ft {
        FileType::RegularFile => 0,
        FileType::Directory => 1,
        FileType::Symlink => 2,
        FileType::BlockDevice => 3,
        FileType::CharDevice => 4,
        FileType::NamedPipe => 5,
        FileType::Socket => 6,
    }
}

fn u8_to_file_type(v: u8) -> FileType {
    match v {
        1 => FileType::Directory,
        2 => FileType::Symlink,
        3 => FileType::BlockDevice,
        4 => FileType::CharDevice,
        5 => FileType::NamedPipe,
        6 => FileType::Socket,
        _ => FileType::RegularFile,
    }
}

fn mode_to_file_type(mode: u32) -> FileType {
    match mode & libc::S_IFMT as u32 {
        x if x == libc::S_IFREG as u32 => FileType::RegularFile,
        x if x == libc::S_IFDIR as u32 => FileType::Directory,
        x if x == libc::S_IFLNK as u32 => FileType::Symlink,
        x if x == libc::S_IFBLK as u32 => FileType::BlockDevice,
        x if x == libc::S_IFCHR as u32 => FileType::CharDevice,
        x if x == libc::S_IFIFO as u32 => FileType::NamedPipe,
        x if x == libc::S_IFSOCK as u32 => FileType::Socket,
        _ => FileType::RegularFile,
    }
}

#[derive(Clone, Debug)]
struct InodeMeta {
    inode: u64,
    file_type: FileType,
    permissions: u16,
    uid: u32,
    gid: u32,
    size: u64,
    nlink: u32,
    atime_secs: i64,
    atime_nsecs: u32,
    mtime_secs: i64,
    mtime_nsecs: u32,
    ctime_secs: i64,
    ctime_nsecs: u32,
    blob_hashes: Vec<String>,
    symlink_target: Option<String>,
    rdev: u32,
}

impl InodeMeta {
    fn to_file_attr(&self) -> FileAttr {
        let blksize = 512u64;
        FileAttr {
            ino: self.inode,
            size: self.size,
            blocks: (self.size + blksize - 1) / blksize,
            atime: UNIX_EPOCH + Duration::new(self.atime_secs as u64, self.atime_nsecs),
            mtime: UNIX_EPOCH + Duration::new(self.mtime_secs as u64, self.mtime_nsecs),
            ctime: UNIX_EPOCH + Duration::new(self.ctime_secs as u64, self.ctime_nsecs),
            crtime: UNIX_EPOCH,
            kind: self.file_type,
            perm: self.permissions,
            nlink: self.nlink,
            uid: self.uid,
            gid: self.gid,
            rdev: self.rdev,
            blksize: blksize as u32,
            flags: 0,
        }
    }
}

struct OpenFile {
    inode: u64,
    data: Vec<u8>,
    writable: bool,
    dirty: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct TreeEntry {
    name: String,
    inode: u64,
    file_type: u8,
    permissions: u16,
    uid: u32,
    gid: u32,
    size: u64,
    nlink: u32,
    rdev: u32,
    atime_secs: i64,
    atime_nsecs: u32,
    mtime_secs: i64,
    mtime_nsecs: u32,
    ctime_secs: i64,
    ctime_nsecs: u32,
    blob_hashes: Vec<String>,
    symlink_target: Option<String>,
    subtree_hash: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct TreeObject {
    inode: u64,
    permissions: u16,
    uid: u32,
    gid: u32,
    size: u64,
    nlink: u32,
    atime_secs: i64,
    atime_nsecs: u32,
    mtime_secs: i64,
    mtime_nsecs: u32,
    ctime_secs: i64,
    ctime_nsecs: u32,
    entries: Vec<TreeEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CommitObject {
    parent_hash: Option<String>,
    root_tree_hash: String,
    timestamp_secs: i64,
    next_inode: u64,
    next_fh: u64,
}

#[derive(Debug, Clone, Default)]
struct ChangelogManager {
    latest_commit: std::sync::Arc<std::sync::Mutex<Option<String>>>,
}

impl okaywal::LogManager for ChangelogManager {
    fn recover(&mut self, entry: &mut okaywal::Entry<'_>) -> std::io::Result<()> {
        if let Some(chunks) = entry.read_all_chunks()? {
            let mut guard = self.latest_commit.lock().unwrap();
            for chunk in chunks {
                *guard = Some(String::from_utf8_lossy(&chunk).into_owned());
            }
        }
        Ok(())
    }

    fn checkpoint_to(
        &mut self,
        _last_checkpointed_id: okaywal::EntryId,
        _checkpointed_entries: &mut okaywal::SegmentReader,
        wal: &okaywal::WriteAheadLog,
    ) -> std::io::Result<()> {
        if let Some(hash) = self.latest_commit.lock().unwrap().clone() {
            let mut entry = wal.begin_entry()?;
            entry.write_chunk(hash.as_bytes())?;
            entry.commit()?;
        }
        Ok(())
    }
}

pub struct NofsFS {
    data_dir: PathBuf,
    blob_dir: PathBuf,
    trees_dir: PathBuf,
    commits_dir: PathBuf,
    wal: Option<okaywal::WriteAheadLog>,

    current_commit: Option<String>,

    inode_meta: HashMap<u64, InodeMeta>,
    dir_entries: HashMap<(u64, String), u64>,

    next_inode: u64,
    next_fh: u64,
    open_files: HashMap<u64, OpenFile>,
    lookup_cnt: HashMap<u64, u64>,
    dirty: bool,
    last_flush: Instant,
}

impl NofsFS {
    pub fn new(data_dir: PathBuf) -> Self {
        let blob_dir = data_dir.join("blobs");
        let objects_dir = data_dir.join("objects");
        let trees_dir = objects_dir.join("trees");
        let commits_dir = objects_dir.join("commits");
        NofsFS {
            data_dir,
            blob_dir,
            trees_dir,
            commits_dir,
            wal: None,
            current_commit: None,
            inode_meta: HashMap::new(),
            dir_entries: HashMap::new(),
            next_inode: 2,
            next_fh: 1,
            open_files: HashMap::new(),
            lookup_cnt: HashMap::new(),
            dirty: false,
            last_flush: Instant::now(),
        }
    }

    fn alloc_inode(&mut self) -> u64 {
        let ino = self.next_inode;
        self.next_inode += 1;
        ino
    }

    fn alloc_fh(&mut self) -> u64 {
        let fh = self.next_fh;
        self.next_fh += 1;
        fh
    }

    fn parent_of(&self, ino: u64) -> u64 {
        if ino == FUSE_ROOT_INODE {
            return FUSE_ROOT_INODE;
        }
        for ((parent, _), &child) in &self.dir_entries {
            if child == ino {
                return *parent;
            }
        }
        FUSE_ROOT_INODE
    }

    fn mark_dirty(&mut self) {
        self.dirty = true;
        self.maybe_flush();
    }

    fn maybe_flush(&mut self) {
        if self.dirty && self.last_flush.elapsed() >= FLUSH_INTERVAL {
            self.flush_metadata();
            self.dirty = false;
            self.last_flush = Instant::now();
        }
    }

    // --- Blob helpers ---

    fn blob_path(&self, hash: &str) -> PathBuf {
        self.blob_dir.join(&hash[..2]).join(&hash[2..])
    }

    fn read_blob(&self, hash: &str) -> Vec<u8> {
        let raw = std::fs::read(self.blob_path(hash)).unwrap_or_default();
        if raw.is_empty() {
            return raw;
        }
        reed_solomon::decode(&raw)
    }

    fn write_blob(&self, data: &[u8]) -> String {
        let hash = hex::encode(Sha256::digest(data));
        let path = self.blob_path(&hash);
        if !path.exists() {
            std::fs::create_dir_all(path.parent().unwrap()).ok();
            let encoded = reed_solomon::encode(data);
            let tmp = path.with_extension("tmp");
            std::fs::write(&tmp, &encoded).expect("Failed to write blob");
            std::fs::rename(&tmp, &path).expect("Failed to rename blob");
        }
        hash
    }

    fn cdc_chunk_and_write(&self, data: &[u8]) -> Vec<String> {
        if data.is_empty() {
            return vec![];
        }
        FastCDC::new(data, CDC_MIN_SIZE, CDC_AVG_SIZE, CDC_MAX_SIZE)
            .map(|chunk| self.write_blob(&data[chunk.offset..chunk.offset + chunk.length]))
            .collect()
    }

    // --- Object store (trees + commits) ---

    fn tree_path(&self, hash: &str) -> PathBuf {
        self.trees_dir.join(&hash[..2]).join(&hash[2..])
    }

    fn commit_path(&self, hash: &str) -> PathBuf {
        self.commits_dir.join(&hash[..2]).join(&hash[2..])
    }

    fn write_tree_object(&self, tree: &TreeObject) -> String {
        let json = serde_json::to_vec(tree).expect("serialize tree");
        let hash = hex::encode(Sha256::digest(&json));
        let path = self.tree_path(&hash);
        if !path.exists() {
            std::fs::create_dir_all(path.parent().unwrap()).ok();
            let tmp = path.with_extension("tmp");
            std::fs::write(&tmp, &json).expect("write tree");
            std::fs::rename(&tmp, &path).expect("rename tree");
        }
        hash
    }

    fn read_tree_object(&self, hash: &str) -> TreeObject {
        let json = std::fs::read(self.tree_path(hash)).expect("read tree");
        serde_json::from_slice(&json).expect("deserialize tree")
    }

    fn write_commit_object(&self, commit: &CommitObject) -> String {
        let json = serde_json::to_vec(commit).expect("serialize commit");
        let hash = hex::encode(Sha256::digest(&json));
        let path = self.commit_path(&hash);
        if !path.exists() {
            std::fs::create_dir_all(path.parent().unwrap()).ok();
            let tmp = path.with_extension("tmp");
            std::fs::write(&tmp, &json).expect("write commit");
            std::fs::rename(&tmp, &path).expect("rename commit");
        }
        hash
    }

    fn read_commit_object(&self, hash: &str) -> CommitObject {
        let json = std::fs::read(self.commit_path(hash)).expect("read commit");
        serde_json::from_slice(&json).expect("deserialize commit")
    }

    fn write_wal_entry(&mut self, commit_hash: &str) {
        let wal = self.wal.as_mut().expect("WAL not initialized");
        let mut entry = wal.begin_entry().expect("begin WAL entry");
        entry.write_chunk(commit_hash.as_bytes()).expect("write WAL chunk");
        entry.commit().expect("commit WAL entry");
    }

    fn build_tree_for_dir(&self, dir_inode: u64) -> String {
        let dir_meta = self.inode_meta.get(&dir_inode).unwrap().clone();

        let mut entries: Vec<TreeEntry> = self
            .dir_entries
            .iter()
            .filter(|((parent, _), _)| *parent == dir_inode)
            .map(|((_, name), &child_inode)| {
                let child_meta = self.inode_meta.get(&child_inode).unwrap();
                let subtree_hash = if child_meta.file_type == FileType::Directory {
                    Some(self.build_tree_for_dir(child_inode))
                } else {
                    None
                };
                TreeEntry {
                    name: name.clone(),
                    inode: child_inode,
                    file_type: file_type_to_u8(child_meta.file_type),
                    permissions: child_meta.permissions,
                    uid: child_meta.uid,
                    gid: child_meta.gid,
                    size: child_meta.size,
                    nlink: child_meta.nlink,
                    rdev: child_meta.rdev,
                    atime_secs: child_meta.atime_secs,
                    atime_nsecs: child_meta.atime_nsecs,
                    mtime_secs: child_meta.mtime_secs,
                    mtime_nsecs: child_meta.mtime_nsecs,
                    ctime_secs: child_meta.ctime_secs,
                    ctime_nsecs: child_meta.ctime_nsecs,
                    blob_hashes: child_meta.blob_hashes.clone(),
                    symlink_target: child_meta.symlink_target.clone(),
                    subtree_hash,
                }
            })
            .collect();

        entries.sort_by(|a, b| a.name.cmp(&b.name));

        let tree = TreeObject {
            inode: dir_meta.inode,
            permissions: dir_meta.permissions,
            uid: dir_meta.uid,
            gid: dir_meta.gid,
            size: dir_meta.size,
            nlink: dir_meta.nlink,
            atime_secs: dir_meta.atime_secs,
            atime_nsecs: dir_meta.atime_nsecs,
            mtime_secs: dir_meta.mtime_secs,
            mtime_nsecs: dir_meta.mtime_nsecs,
            ctime_secs: dir_meta.ctime_secs,
            ctime_nsecs: dir_meta.ctime_nsecs,
            entries,
        };

        self.write_tree_object(&tree)
    }

    fn create_commit(&mut self) {
        let root_tree_hash = self.build_tree_for_dir(FUSE_ROOT_INODE);
        let (ts, _) = now_timespec();
        let commit = CommitObject {
            parent_hash: self.current_commit.clone(),
            root_tree_hash,
            timestamp_secs: ts,
            next_inode: self.next_inode,
            next_fh: self.next_fh,
        };
        let commit_hash = self.write_commit_object(&commit);
        self.write_wal_entry(&commit_hash);
        self.current_commit = Some(commit_hash);
    }

    fn load_dir_from_tree(&mut self, parent_inode: u64, tree_hash: &str) {
        let tree = self.read_tree_object(tree_hash);

        self.inode_meta.insert(
            tree.inode,
            InodeMeta {
                inode: tree.inode,
                file_type: FileType::Directory,
                permissions: tree.permissions,
                uid: tree.uid,
                gid: tree.gid,
                size: tree.size,
                nlink: tree.nlink,
                rdev: 0,
                atime_secs: tree.atime_secs,
                atime_nsecs: tree.atime_nsecs,
                mtime_secs: tree.mtime_secs,
                mtime_nsecs: tree.mtime_nsecs,
                ctime_secs: tree.ctime_secs,
                ctime_nsecs: tree.ctime_nsecs,
                blob_hashes: vec![],
                symlink_target: None,
            },
        );

        for entry in &tree.entries {
            self.dir_entries
                .insert((parent_inode, entry.name.clone()), entry.inode);
            self.inode_meta.insert(
                entry.inode,
                InodeMeta {
                    inode: entry.inode,
                    file_type: u8_to_file_type(entry.file_type),
                    permissions: entry.permissions,
                    uid: entry.uid,
                    gid: entry.gid,
                    size: entry.size,
                    nlink: entry.nlink,
                    rdev: entry.rdev,
                    atime_secs: entry.atime_secs,
                    atime_nsecs: entry.atime_nsecs,
                    mtime_secs: entry.mtime_secs,
                    mtime_nsecs: entry.mtime_nsecs,
                    ctime_secs: entry.ctime_secs,
                    ctime_nsecs: entry.ctime_nsecs,
                    blob_hashes: entry.blob_hashes.clone(),
                    symlink_target: entry.symlink_target.clone(),
                },
            );

            if let Some(subtree_hash) = &entry.subtree_hash {
                self.load_dir_from_tree(entry.inode, subtree_hash);
            }
        }
    }

    fn load_from_commit(&mut self, commit_hash: &str) {
        let commit = self.read_commit_object(commit_hash);
        self.next_inode = commit.next_inode;
        self.next_fh = commit.next_fh;
        self.current_commit = Some(commit_hash.to_string());
        self.load_dir_from_tree(FUSE_ROOT_INODE, &commit.root_tree_hash);
    }

    // --- Metadata serialization ---

    fn load_metadata(&mut self) {
        if let Some(hash) = self.current_commit.clone() {
            self.load_from_commit(&hash);
            return;
        }

        // Fresh filesystem: bootstrap root inode
        let (secs, nsecs) = now_timespec();
        self.inode_meta.insert(
            FUSE_ROOT_INODE,
            InodeMeta {
                inode: FUSE_ROOT_INODE,
                file_type: FileType::Directory,
                permissions: 0o755,
                uid: unsafe { libc::getuid() },
                gid: unsafe { libc::getgid() },
                size: 0,
                nlink: 2,
                atime_secs: secs,
                atime_nsecs: nsecs,
                mtime_secs: secs,
                mtime_nsecs: nsecs,
                ctime_secs: secs,
                ctime_nsecs: nsecs,
                blob_hashes: vec![],
                symlink_target: None,
                rdev: 0,
            },
        );
        self.next_inode = 2;
    }

    fn flush_metadata(&mut self) {
        self.create_commit();
    }


    // --- Open file helpers ---

    fn flush_open_file(&mut self, fh: u64) {
        let (inode, data) = {
            let open_file = match self.open_files.get(&fh) {
                Some(f) if f.dirty => f,
                _ => return,
            };
            (open_file.inode, open_file.data.clone())
        };

        let new_hashes = self.cdc_chunk_and_write(&data);

        if let Some(meta) = self.inode_meta.get_mut(&inode) {
            meta.blob_hashes = new_hashes;
            meta.size = data.len() as u64;
            let (secs, nsecs) = now_timespec();
            meta.mtime_secs = secs;
            meta.mtime_nsecs = nsecs;
            meta.ctime_secs = secs;
            meta.ctime_nsecs = nsecs;
        }

        if let Some(of) = self.open_files.get_mut(&fh) {
            of.dirty = false;
        }

        self.dirty = true;
    }
}

impl Filesystem for NofsFS {
    fn init(
        &mut self,
        _req: &Request<'_>,
        _config: &mut KernelConfig,
    ) -> Result<(), libc::c_int> {
        std::fs::create_dir_all(&self.blob_dir).map_err(|_| libc::EIO)?;
        std::fs::create_dir_all(&self.trees_dir).map_err(|_| libc::EIO)?;
        std::fs::create_dir_all(&self.commits_dir).map_err(|_| libc::EIO)?;
        let wal_dir = self.data_dir.join("wal");
        std::fs::create_dir_all(&wal_dir).map_err(|_| libc::EIO)?;
        let manager = ChangelogManager::default();
        let latest_commit = manager.latest_commit.clone();
        let wal = okaywal::WriteAheadLog::recover(&wal_dir, manager)
            .map_err(|_| libc::EIO)?;
        self.wal = Some(wal);
        self.current_commit = latest_commit.lock().unwrap().clone();
        self.load_metadata();
        Ok(())
    }

    fn destroy(&mut self) {
        // Flush any open dirty files
        let fhs: Vec<u64> = self.open_files.keys().cloned().collect();
        for fh in fhs {
            self.flush_open_file(fh);
        }
        self.flush_metadata();
    }

    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let name_str = match name.to_str() {
            Some(s) => s,
            None => return reply.error(libc::ENOENT),
        };

        let child_ino = match self.dir_entries.get(&(parent, name_str.to_string())) {
            Some(&ino) => ino,
            None => return reply.error(libc::ENOENT),
        };

        let meta = match self.inode_meta.get(&child_ino) {
            Some(m) => m,
            None => return reply.error(libc::ENOENT),
        };

        *self.lookup_cnt.entry(child_ino).or_insert(0) += 1;
        reply.entry(&TTL, &meta.to_file_attr(), 0);
    }

    fn forget(&mut self, _req: &Request, ino: u64, nlookup: u64) {
        if let Some(count) = self.lookup_cnt.get_mut(&ino) {
            if *count > nlookup {
                *count -= nlookup;
                return;
            }
        }
        self.lookup_cnt.remove(&ino);
    }

    fn getattr(&mut self, _req: &Request, ino: u64, _fh: Option<u64>, reply: ReplyAttr) {
        let meta = match self.inode_meta.get(&ino) {
            Some(m) => m,
            None => return reply.error(libc::ENOENT),
        };
        let mut attr = meta.to_file_attr();

        // If open for writing, use buffer size
        for of in self.open_files.values() {
            if of.inode == ino && of.writable {
                attr.size = of.data.len() as u64;
                attr.blocks = (attr.size + 511) / 512;
                break;
            }
        }

        reply.attr(&TTL, &attr);
    }

    fn setattr(
        &mut self,
        _req: &Request,
        ino: u64,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<TimeOrNow>,
        mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        fh: Option<u64>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        if !self.inode_meta.contains_key(&ino) {
            return reply.error(libc::ENOENT);
        }

        let (secs, nsecs) = now_timespec();

        // Truncate
        if let Some(new_size) = size {
            // Check if there's an open writable file handle
            let open_fh = fh.filter(|f| self.open_files.contains_key(f)).or_else(|| {
                self.open_files
                    .iter()
                    .find(|(_, of)| of.inode == ino && of.writable)
                    .map(|(&fh, _)| fh)
            });

            if let Some(fh) = open_fh {
                if let Some(of) = self.open_files.get_mut(&fh) {
                    of.data.resize(new_size as usize, 0);
                    of.dirty = true;
                }
            } else {
                // Load all chunks, truncate, re-chunk and write back
                let old_hashes = self
                    .inode_meta
                    .get(&ino)
                    .map(|m| m.blob_hashes.clone())
                    .unwrap_or_default();
                let mut data: Vec<u8> = {
                    let mut buf = Vec::new();
                    for hash in &old_hashes {
                        buf.extend_from_slice(&self.read_blob(hash));
                    }
                    buf
                };
                data.resize(new_size as usize, 0);
                let new_hashes = self.cdc_chunk_and_write(&data);
                if let Some(meta) = self.inode_meta.get_mut(&ino) {
                    meta.blob_hashes = new_hashes;
                }
            }

            if let Some(meta) = self.inode_meta.get_mut(&ino) {
                meta.size = new_size;
            }
        }

        if let Some(new_mode) = mode {
            if let Some(meta) = self.inode_meta.get_mut(&ino) {
                meta.permissions = (new_mode & 0o7777) as u16;
            }
        }

        if let Some(new_uid) = uid {
            if let Some(meta) = self.inode_meta.get_mut(&ino) {
                meta.uid = new_uid;
            }
        }

        if let Some(new_gid) = gid {
            if let Some(meta) = self.inode_meta.get_mut(&ino) {
                meta.gid = new_gid;
            }
        }

        if let Some(t) = atime {
            let (s, ns) = match t {
                TimeOrNow::SpecificTime(st) => {
                    let d = st.duration_since(UNIX_EPOCH).unwrap_or_default();
                    (d.as_secs() as i64, d.subsec_nanos())
                }
                TimeOrNow::Now => now_timespec(),
            };
            if let Some(meta) = self.inode_meta.get_mut(&ino) {
                meta.atime_secs = s;
                meta.atime_nsecs = ns;
            }
        }

        if let Some(t) = mtime {
            let (s, ns) = match t {
                TimeOrNow::SpecificTime(st) => {
                    let d = st.duration_since(UNIX_EPOCH).unwrap_or_default();
                    (d.as_secs() as i64, d.subsec_nanos())
                }
                TimeOrNow::Now => now_timespec(),
            };
            if let Some(meta) = self.inode_meta.get_mut(&ino) {
                meta.mtime_secs = s;
                meta.mtime_nsecs = ns;
            }
        }

        // Update ctime for any metadata change
        if let Some(meta) = self.inode_meta.get_mut(&ino) {
            meta.ctime_secs = secs;
            meta.ctime_nsecs = nsecs;
        }

        self.mark_dirty();

        match self.inode_meta.get(&ino) {
            Some(m) => {
                let mut attr = m.to_file_attr();
                // Reflect open buffer size
                for of in self.open_files.values() {
                    if of.inode == ino && of.writable {
                        attr.size = of.data.len() as u64;
                        attr.blocks = (attr.size + 511) / 512;
                        break;
                    }
                }
                reply.attr(&TTL, &attr);
            }
            None => reply.error(libc::ENOENT),
        }
    }

    fn readlink(&mut self, _req: &Request, ino: u64, reply: ReplyData) {
        match self.inode_meta.get(&ino) {
            Some(m) if m.symlink_target.is_some() => {
                reply.data(m.symlink_target.as_ref().unwrap().as_bytes());
            }
            _ => reply.error(libc::ENOENT),
        }
    }

    fn mknod(
        &mut self,
        req: &Request,
        parent: u64,
        name: &OsStr,
        mode: u32,
        _umask: u32,
        rdev: u32,
        reply: ReplyEntry,
    ) {
        let name_str = match name.to_str() {
            Some(s) => s,
            None => return reply.error(libc::EINVAL),
        };

        if !self.inode_meta.contains_key(&parent) {
            return reply.error(libc::ENOENT);
        }

        let ino = self.alloc_inode();
        let (secs, nsecs) = now_timespec();
        let ft = mode_to_file_type(mode);

        self.inode_meta.insert(
            ino,
            InodeMeta {
                inode: ino,
                file_type: ft,
                permissions: (mode & 0o7777) as u16,
                uid: req.uid(),
                gid: req.gid(),
                size: 0,
                nlink: 1,
                atime_secs: secs,
                atime_nsecs: nsecs,
                mtime_secs: secs,
                mtime_nsecs: nsecs,
                ctime_secs: secs,
                ctime_nsecs: nsecs,
                blob_hashes: vec![],
                symlink_target: None,
                rdev,
            },
        );

        self.dir_entries
            .insert((parent, name_str.to_string()), ino);

        // Update parent mtime
        if let Some(pmeta) = self.inode_meta.get_mut(&parent) {
            pmeta.mtime_secs = secs;
            pmeta.mtime_nsecs = nsecs;
            pmeta.ctime_secs = secs;
            pmeta.ctime_nsecs = nsecs;
        }

        *self.lookup_cnt.entry(ino).or_insert(0) += 1;
        self.mark_dirty();

        let attr = self.inode_meta.get(&ino).unwrap().to_file_attr();
        reply.entry(&TTL, &attr, 0);
    }

    fn mkdir(
        &mut self,
        req: &Request,
        parent: u64,
        name: &OsStr,
        mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        let name_str = match name.to_str() {
            Some(s) => s,
            None => return reply.error(libc::EINVAL),
        };

        if !self.inode_meta.contains_key(&parent) {
            return reply.error(libc::ENOENT);
        }

        let ino = self.alloc_inode();
        let (secs, nsecs) = now_timespec();

        self.inode_meta.insert(
            ino,
            InodeMeta {
                inode: ino,
                file_type: FileType::Directory,
                permissions: (mode & 0o7777) as u16,
                uid: req.uid(),
                gid: req.gid(),
                size: 0,
                nlink: 2,
                atime_secs: secs,
                atime_nsecs: nsecs,
                mtime_secs: secs,
                mtime_nsecs: nsecs,
                ctime_secs: secs,
                ctime_nsecs: nsecs,
                blob_hashes: vec![],
                symlink_target: None,
                rdev: 0,
            },
        );

        self.dir_entries
            .insert((parent, name_str.to_string()), ino);

        // Increment parent nlink
        if let Some(pmeta) = self.inode_meta.get_mut(&parent) {
            pmeta.nlink += 1;
            pmeta.mtime_secs = secs;
            pmeta.mtime_nsecs = nsecs;
            pmeta.ctime_secs = secs;
            pmeta.ctime_nsecs = nsecs;
        }

        *self.lookup_cnt.entry(ino).or_insert(0) += 1;
        self.mark_dirty();

        let attr = self.inode_meta.get(&ino).unwrap().to_file_attr();
        reply.entry(&TTL, &attr, 0);
    }

    fn unlink(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let name_str = match name.to_str() {
            Some(s) => s,
            None => return reply.error(libc::EINVAL),
        };

        let child_ino = match self.dir_entries.remove(&(parent, name_str.to_string())) {
            Some(ino) => ino,
            None => return reply.error(libc::ENOENT),
        };

        let (secs, nsecs) = now_timespec();

        // Update parent mtime
        if let Some(pmeta) = self.inode_meta.get_mut(&parent) {
            pmeta.mtime_secs = secs;
            pmeta.mtime_nsecs = nsecs;
            pmeta.ctime_secs = secs;
            pmeta.ctime_nsecs = nsecs;
        }

        // Decrement nlink
        let should_remove = if let Some(meta) = self.inode_meta.get_mut(&child_ino) {
            meta.nlink = meta.nlink.saturating_sub(1);
            meta.ctime_secs = secs;
            meta.ctime_nsecs = nsecs;
            meta.nlink == 0
        } else {
            false
        };

        if should_remove {
            self.inode_meta.remove(&child_ino);
        }

        self.mark_dirty();
        reply.ok();
    }

    fn rmdir(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let name_str = match name.to_str() {
            Some(s) => s,
            None => return reply.error(libc::EINVAL),
        };

        let child_ino = match self.dir_entries.get(&(parent, name_str.to_string())) {
            Some(&ino) => ino,
            None => return reply.error(libc::ENOENT),
        };

        // Check if directory is empty
        let has_children = self.dir_entries.keys().any(|(p, _)| *p == child_ino);
        if has_children {
            return reply.error(libc::ENOTEMPTY);
        }

        self.dir_entries.remove(&(parent, name_str.to_string()));
        self.inode_meta.remove(&child_ino);

        let (secs, nsecs) = now_timespec();

        // Decrement parent nlink
        if let Some(pmeta) = self.inode_meta.get_mut(&parent) {
            pmeta.nlink = pmeta.nlink.saturating_sub(1);
            pmeta.mtime_secs = secs;
            pmeta.mtime_nsecs = nsecs;
            pmeta.ctime_secs = secs;
            pmeta.ctime_nsecs = nsecs;
        }

        self.mark_dirty();
        reply.ok();
    }

    fn symlink(
        &mut self,
        req: &Request,
        parent: u64,
        link_name: &OsStr,
        target: &Path,
        reply: ReplyEntry,
    ) {
        let name_str = match link_name.to_str() {
            Some(s) => s,
            None => return reply.error(libc::EINVAL),
        };

        if !self.inode_meta.contains_key(&parent) {
            return reply.error(libc::ENOENT);
        }

        let target_str = match target.to_str() {
            Some(s) => s.to_string(),
            None => return reply.error(libc::EINVAL),
        };

        let ino = self.alloc_inode();
        let (secs, nsecs) = now_timespec();

        self.inode_meta.insert(
            ino,
            InodeMeta {
                inode: ino,
                file_type: FileType::Symlink,
                permissions: 0o777,
                uid: req.uid(),
                gid: req.gid(),
                size: target_str.len() as u64,
                nlink: 1,
                atime_secs: secs,
                atime_nsecs: nsecs,
                mtime_secs: secs,
                mtime_nsecs: nsecs,
                ctime_secs: secs,
                ctime_nsecs: nsecs,
                blob_hashes: vec![],
                symlink_target: Some(target_str),
                rdev: 0,
            },
        );

        self.dir_entries
            .insert((parent, name_str.to_string()), ino);

        if let Some(pmeta) = self.inode_meta.get_mut(&parent) {
            pmeta.mtime_secs = secs;
            pmeta.mtime_nsecs = nsecs;
            pmeta.ctime_secs = secs;
            pmeta.ctime_nsecs = nsecs;
        }

        *self.lookup_cnt.entry(ino).or_insert(0) += 1;
        self.mark_dirty();

        let attr = self.inode_meta.get(&ino).unwrap().to_file_attr();
        reply.entry(&TTL, &attr, 0);
    }

    fn rename(
        &mut self,
        _req: &Request,
        parent: u64,
        name: &OsStr,
        newparent: u64,
        newname: &OsStr,
        _flags: u32,
        reply: ReplyEmpty,
    ) {
        let old_name = match name.to_str() {
            Some(s) => s.to_string(),
            None => return reply.error(libc::EINVAL),
        };
        let new_name = match newname.to_str() {
            Some(s) => s.to_string(),
            None => return reply.error(libc::EINVAL),
        };

        let child_ino = match self.dir_entries.remove(&(parent, old_name)) {
            Some(ino) => ino,
            None => return reply.error(libc::ENOENT),
        };

        // If target exists, remove it
        if let Some(replaced_ino) = self.dir_entries.remove(&(newparent, new_name.clone())) {
            if let Some(meta) = self.inode_meta.get_mut(&replaced_ino) {
                meta.nlink = meta.nlink.saturating_sub(1);
                if meta.nlink == 0 {
                    self.inode_meta.remove(&replaced_ino);
                }
            }
        }

        self.dir_entries.insert((newparent, new_name), child_ino);

        let (secs, nsecs) = now_timespec();
        if let Some(pmeta) = self.inode_meta.get_mut(&parent) {
            pmeta.mtime_secs = secs;
            pmeta.mtime_nsecs = nsecs;
            pmeta.ctime_secs = secs;
            pmeta.ctime_nsecs = nsecs;
        }
        if newparent != parent {
            if let Some(pmeta) = self.inode_meta.get_mut(&newparent) {
                pmeta.mtime_secs = secs;
                pmeta.mtime_nsecs = nsecs;
                pmeta.ctime_secs = secs;
                pmeta.ctime_nsecs = nsecs;
            }
        }

        self.mark_dirty();
        reply.ok();
    }

    fn link(
        &mut self,
        _req: &Request,
        ino: u64,
        newparent: u64,
        newname: &OsStr,
        reply: ReplyEntry,
    ) {
        let name_str = match newname.to_str() {
            Some(s) => s,
            None => return reply.error(libc::EINVAL),
        };

        if !self.inode_meta.contains_key(&ino) || !self.inode_meta.contains_key(&newparent) {
            return reply.error(libc::ENOENT);
        }

        let (secs, nsecs) = now_timespec();

        if let Some(meta) = self.inode_meta.get_mut(&ino) {
            meta.nlink += 1;
            meta.ctime_secs = secs;
            meta.ctime_nsecs = nsecs;
        }

        self.dir_entries
            .insert((newparent, name_str.to_string()), ino);

        if let Some(pmeta) = self.inode_meta.get_mut(&newparent) {
            pmeta.mtime_secs = secs;
            pmeta.mtime_nsecs = nsecs;
            pmeta.ctime_secs = secs;
            pmeta.ctime_nsecs = nsecs;
        }

        *self.lookup_cnt.entry(ino).or_insert(0) += 1;
        self.mark_dirty();

        let attr = self.inode_meta.get(&ino).unwrap().to_file_attr();
        reply.entry(&TTL, &attr, 0);
    }

    fn open(&mut self, _req: &Request, ino: u64, flags: i32, reply: ReplyOpen) {
        let meta = match self.inode_meta.get(&ino) {
            Some(m) => m,
            None => return reply.error(libc::ENOENT),
        };

        let access_mode = flags & libc::O_ACCMODE;
        let writable = access_mode == libc::O_WRONLY || access_mode == libc::O_RDWR;

        let data = {
            let hashes = meta.blob_hashes.clone();
            let mut buf = Vec::with_capacity(meta.size as usize);
            for hash in &hashes {
                buf.extend_from_slice(&self.read_blob(hash));
            }
            buf
        };

        let fh = self.alloc_fh();
        self.open_files.insert(
            fh,
            OpenFile {
                inode: ino,
                data,
                writable,
                dirty: false,
            },
        );

        reply.opened(fh, 0);
    }

    fn read(
        &mut self,
        _req: &Request,
        _ino: u64,
        fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        let open_file = match self.open_files.get(&fh) {
            Some(f) => f,
            None => return reply.error(libc::EBADF),
        };

        let offset = offset as usize;
        if offset >= open_file.data.len() {
            return reply.data(&[]);
        }

        let end = std::cmp::min(offset + size as usize, open_file.data.len());
        reply.data(&open_file.data[offset..end]);
    }

    fn write(
        &mut self,
        _req: &Request,
        _ino: u64,
        fh: u64,
        offset: i64,
        data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyWrite,
    ) {
        let open_file = match self.open_files.get_mut(&fh) {
            Some(f) => f,
            None => return reply.error(libc::EBADF),
        };

        let offset = offset as usize;
        let end = offset + data.len();

        if end > open_file.data.len() {
            open_file.data.resize(end, 0);
        }

        open_file.data[offset..end].copy_from_slice(data);
        open_file.dirty = true;

        reply.written(data.len() as u32);
    }

    fn flush(
        &mut self,
        _req: &Request,
        _ino: u64,
        fh: u64,
        _lock_owner: u64,
        reply: ReplyEmpty,
    ) {
        self.flush_open_file(fh);
        reply.ok();
    }

    fn release(
        &mut self,
        _req: &Request,
        _ino: u64,
        fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        self.flush_open_file(fh);
        self.open_files.remove(&fh);
        reply.ok();
    }

    fn opendir(&mut self, _req: &Request, ino: u64, _flags: i32, reply: ReplyOpen) {
        if self.inode_meta.contains_key(&ino) {
            reply.opened(ino, 0);
        } else {
            reply.error(libc::ENOENT);
        }
    }

    fn readdir(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        if !self.inode_meta.contains_key(&ino) {
            return reply.error(libc::ENOENT);
        }

        let parent_ino = self.parent_of(ino);

        let mut entries: Vec<(i64, u64, FileType, String)> = Vec::new();
        entries.push((1, ino, FileType::Directory, ".".to_string()));
        entries.push((2, parent_ino, FileType::Directory, "..".to_string()));

        let mut children: Vec<(String, u64)> = self
            .dir_entries
            .iter()
            .filter(|((p, _), _)| *p == ino)
            .map(|((_, name), &child)| (name.clone(), child))
            .collect();
        children.sort_by(|a, b| a.0.cmp(&b.0));

        for (i, (name, child_ino)) in children.into_iter().enumerate() {
            let ft = self
                .inode_meta
                .get(&child_ino)
                .map(|m| m.file_type)
                .unwrap_or(FileType::RegularFile);
            entries.push(((i + 3) as i64, child_ino, ft, name));
        }

        for (entry_offset, entry_ino, ft, name) in entries {
            if entry_offset <= offset {
                continue;
            }
            if reply.add(entry_ino, entry_offset, ft, &name) {
                break;
            }
        }
        reply.ok();
    }

    fn create(
        &mut self,
        req: &Request,
        parent: u64,
        name: &OsStr,
        mode: u32,
        _umask: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        let name_str = match name.to_str() {
            Some(s) => s,
            None => return reply.error(libc::EINVAL),
        };

        if !self.inode_meta.contains_key(&parent) {
            return reply.error(libc::ENOENT);
        }

        let ino = self.alloc_inode();
        let (secs, nsecs) = now_timespec();
        let access_mode = flags & libc::O_ACCMODE;
        let writable = access_mode == libc::O_WRONLY || access_mode == libc::O_RDWR;

        self.inode_meta.insert(
            ino,
            InodeMeta {
                inode: ino,
                file_type: FileType::RegularFile,
                permissions: (mode & 0o7777) as u16,
                uid: req.uid(),
                gid: req.gid(),
                size: 0,
                nlink: 1,
                atime_secs: secs,
                atime_nsecs: nsecs,
                mtime_secs: secs,
                mtime_nsecs: nsecs,
                ctime_secs: secs,
                ctime_nsecs: nsecs,
                blob_hashes: vec![],
                symlink_target: None,
                rdev: 0,
            },
        );

        self.dir_entries
            .insert((parent, name_str.to_string()), ino);

        if let Some(pmeta) = self.inode_meta.get_mut(&parent) {
            pmeta.mtime_secs = secs;
            pmeta.mtime_nsecs = nsecs;
            pmeta.ctime_secs = secs;
            pmeta.ctime_nsecs = nsecs;
        }

        let fh = self.alloc_fh();
        self.open_files.insert(
            fh,
            OpenFile {
                inode: ino,
                data: Vec::new(),
                writable,
                dirty: false,
            },
        );

        *self.lookup_cnt.entry(ino).or_insert(0) += 1;
        self.mark_dirty();

        let attr = self.inode_meta.get(&ino).unwrap().to_file_attr();
        reply.created(&TTL, &attr, 0, fh, 0);
    }

    fn access(&mut self, _req: &Request, ino: u64, _mask: i32, reply: ReplyEmpty) {
        if self.inode_meta.contains_key(&ino) {
            reply.ok();
        } else {
            reply.error(libc::ENOENT);
        }
    }

    fn statfs(&mut self, _req: &Request, _ino: u64, reply: ReplyStatfs) {
        let c_path =
            std::ffi::CString::new(self.data_dir.as_os_str().as_bytes()).unwrap();
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
        if rc != 0 {
            return reply.error(
                std::io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(libc::EIO),
            );
        }
        reply.statfs(
            stat.f_blocks,
            stat.f_bfree,
            stat.f_bavail,
            stat.f_files,
            stat.f_ffree,
            stat.f_bsize as u32,
            stat.f_namemax as u32,
            stat.f_frsize as u32,
        );
    }
}
