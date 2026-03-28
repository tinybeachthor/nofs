use fuser::{
    FileAttr, FileType, Filesystem, KernelConfig, ReplyAttr, ReplyCreate, ReplyData,
    ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen, ReplyStatfs, ReplyWrite, Request, TimeOrNow,
};
use libc;
use polars::prelude::*;
use polars::io::avro::{AvroReader, AvroWriter};
use sha2::{Digest, Sha256};
use fastcdc::v2020::FastCDC;

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

pub struct NofsFS {
    data_dir: PathBuf,
    blob_dir: PathBuf,
    metadata_path: PathBuf,

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
        let metadata_path = data_dir.join("metadata.avro");
        NofsFS {
            data_dir,
            blob_dir,
            metadata_path,
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
        std::fs::read(self.blob_path(hash)).unwrap_or_default()
    }

    fn write_blob(&self, data: &[u8]) -> String {
        let hash = hex::encode(Sha256::digest(data));
        let path = self.blob_path(&hash);
        if !path.exists() {
            std::fs::create_dir_all(path.parent().unwrap()).ok();
            let tmp = path.with_extension("tmp");
            std::fs::write(&tmp, data).expect("Failed to write blob");
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

    fn delete_blob_if_unreferenced(&self, hash: &str) {
        let referenced = self
            .inode_meta
            .values()
            .any(|m| m.blob_hashes.iter().any(|h| h == hash));
        if !referenced {
            std::fs::remove_file(self.blob_path(hash)).ok();
        }
    }

    // --- Metadata serialization ---

    fn load_metadata(&mut self) {
        if !self.metadata_path.exists() {
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
            return;
        }

        let file = std::fs::File::open(&self.metadata_path).expect("Failed to open metadata");
        let df = AvroReader::new(file)
            .finish()
            .expect("Failed to read AVRO");

        let col_inode = df.column("inode").unwrap().i64().unwrap();
        let col_parent = df.column("parent_inode").unwrap().i64().unwrap();
        let col_name = df.column("name").unwrap().str().unwrap();
        let col_ft = df.column("file_type").unwrap().i32().unwrap();
        let col_perm = df.column("permissions").unwrap().i32().unwrap();
        let col_uid = df.column("uid").unwrap().i32().unwrap();
        let col_gid = df.column("gid").unwrap().i32().unwrap();
        let col_size = df.column("size").unwrap().i64().unwrap();
        let col_nlink = df.column("nlink").unwrap().i32().unwrap();
        let col_atime_s = df.column("atime_secs").unwrap().i64().unwrap();
        let col_atime_ns = df.column("atime_nsecs").unwrap().i32().unwrap();
        let col_mtime_s = df.column("mtime_secs").unwrap().i64().unwrap();
        let col_mtime_ns = df.column("mtime_nsecs").unwrap().i32().unwrap();
        let col_ctime_s = df.column("ctime_secs").unwrap().i64().unwrap();
        let col_ctime_ns = df.column("ctime_nsecs").unwrap().i32().unwrap();
        let col_blob = df.column("blob_hashes").unwrap().str().unwrap();
        let col_symlink = df.column("symlink_target").unwrap().str().unwrap();
        let col_rdev = df.column("rdev").unwrap().i32().unwrap();

        let mut max_inode: u64 = 0;
        for i in 0..df.height() {
            let inode = col_inode.get(i).unwrap() as u64;
            let parent_inode = col_parent.get(i).unwrap() as u64;
            let name = col_name.get(i).unwrap();

            if parent_inode > 0 && !name.is_empty() {
                self.dir_entries
                    .insert((parent_inode, name.to_string()), inode);
            }

            if !self.inode_meta.contains_key(&inode) {
                self.inode_meta.insert(
                    inode,
                    InodeMeta {
                        inode,
                        file_type: u8_to_file_type(col_ft.get(i).unwrap() as u8),
                        permissions: col_perm.get(i).unwrap() as u16,
                        uid: col_uid.get(i).unwrap() as u32,
                        gid: col_gid.get(i).unwrap() as u32,
                        size: col_size.get(i).unwrap() as u64,
                        nlink: col_nlink.get(i).unwrap() as u32,
                        atime_secs: col_atime_s.get(i).unwrap(),
                        atime_nsecs: col_atime_ns.get(i).unwrap() as u32,
                        mtime_secs: col_mtime_s.get(i).unwrap(),
                        mtime_nsecs: col_mtime_ns.get(i).unwrap() as u32,
                        ctime_secs: col_ctime_s.get(i).unwrap(),
                        ctime_nsecs: col_ctime_ns.get(i).unwrap() as u32,
                        blob_hashes: col_blob.get(i)
                            .filter(|s| !s.is_empty())
                            .map(|s| s.split(',').map(str::to_string).collect())
                            .unwrap_or_default(),
                        symlink_target: col_symlink.get(i).map(|s: &str| s.to_string()),
                        rdev: col_rdev.get(i).unwrap() as u32,
                    },
                );
            }

            max_inode = max_inode.max(inode);
        }

        self.next_inode = max_inode + 1;
    }

    fn flush_metadata(&self) {
        // Collect rows: root inode + all dir entries
        struct Row {
            inode: i64,
            parent_inode: i64,
            name: String,
            file_type: i32,
            permissions: i32,
            uid: i32,
            gid: i32,
            size: i64,
            nlink: i32,
            atime_secs: i64,
            atime_nsecs: i32,
            mtime_secs: i64,
            mtime_nsecs: i32,
            ctime_secs: i64,
            ctime_nsecs: i32,
            blob_hashes: String,
            symlink_target: Option<String>,
            rdev: i32,
        }

        fn meta_to_row(meta: &InodeMeta, parent: u64, name: String) -> Row {
            Row {
                inode: meta.inode as i64,
                parent_inode: parent as i64,
                name,
                file_type: file_type_to_u8(meta.file_type) as i32,
                permissions: meta.permissions as i32,
                uid: meta.uid as i32,
                gid: meta.gid as i32,
                size: meta.size as i64,
                nlink: meta.nlink as i32,
                atime_secs: meta.atime_secs,
                atime_nsecs: meta.atime_nsecs as i32,
                mtime_secs: meta.mtime_secs,
                mtime_nsecs: meta.mtime_nsecs as i32,
                ctime_secs: meta.ctime_secs,
                ctime_nsecs: meta.ctime_nsecs as i32,
                blob_hashes: meta.blob_hashes.join(","),
                symlink_target: meta.symlink_target.clone(),
                rdev: meta.rdev as i32,
            }
        }

        let mut rows: Vec<Row> = Vec::new();

        // Root inode row (parent=0, name="")
        if let Some(meta) = self.inode_meta.get(&FUSE_ROOT_INODE) {
            rows.push(meta_to_row(meta, 0, String::new()));
        }

        // One row per dir entry
        for ((parent, name), &child_inode) in &self.dir_entries {
            if let Some(meta) = self.inode_meta.get(&child_inode) {
                rows.push(meta_to_row(meta, *parent, name.clone()));
            }
        }

        let v_inode: Vec<i64> = rows.iter().map(|r| r.inode).collect();
        let v_parent: Vec<i64> = rows.iter().map(|r| r.parent_inode).collect();
        let v_name: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        let v_ft: Vec<i32> = rows.iter().map(|r| r.file_type).collect();
        let v_perm: Vec<i32> = rows.iter().map(|r| r.permissions).collect();
        let v_uid: Vec<i32> = rows.iter().map(|r| r.uid).collect();
        let v_gid: Vec<i32> = rows.iter().map(|r| r.gid).collect();
        let v_size: Vec<i64> = rows.iter().map(|r| r.size).collect();
        let v_nlink: Vec<i32> = rows.iter().map(|r| r.nlink).collect();
        let v_atime_s: Vec<i64> = rows.iter().map(|r| r.atime_secs).collect();
        let v_atime_ns: Vec<i32> = rows.iter().map(|r| r.atime_nsecs).collect();
        let v_mtime_s: Vec<i64> = rows.iter().map(|r| r.mtime_secs).collect();
        let v_mtime_ns: Vec<i32> = rows.iter().map(|r| r.mtime_nsecs).collect();
        let v_ctime_s: Vec<i64> = rows.iter().map(|r| r.ctime_secs).collect();
        let v_ctime_ns: Vec<i32> = rows.iter().map(|r| r.ctime_nsecs).collect();
        let v_blob: Vec<&str> = rows.iter().map(|r| r.blob_hashes.as_str()).collect();
        let v_symlink: Vec<Option<&str>> =
            rows.iter().map(|r| r.symlink_target.as_deref()).collect();
        let v_rdev: Vec<i32> = rows.iter().map(|r| r.rdev).collect();

        let mut df = DataFrame::new(vec![
            Series::new("inode".into(), &v_inode).into(),
            Series::new("parent_inode".into(), &v_parent).into(),
            Series::new("name".into(), &v_name).into(),
            Series::new("file_type".into(), &v_ft).into(),
            Series::new("permissions".into(), &v_perm).into(),
            Series::new("uid".into(), &v_uid).into(),
            Series::new("gid".into(), &v_gid).into(),
            Series::new("size".into(), &v_size).into(),
            Series::new("nlink".into(), &v_nlink).into(),
            Series::new("atime_secs".into(), &v_atime_s).into(),
            Series::new("atime_nsecs".into(), &v_atime_ns).into(),
            Series::new("mtime_secs".into(), &v_mtime_s).into(),
            Series::new("mtime_nsecs".into(), &v_mtime_ns).into(),
            Series::new("ctime_secs".into(), &v_ctime_s).into(),
            Series::new("ctime_nsecs".into(), &v_ctime_ns).into(),
            Series::new("blob_hashes".into(), &v_blob).into(),
            Series::new("symlink_target".into(), &v_symlink).into(),
            Series::new("rdev".into(), &v_rdev).into(),
        ])
        .expect("Failed to create DataFrame");

        let tmp_path = self.metadata_path.with_extension("avro.tmp");
        let mut file =
            std::fs::File::create(&tmp_path).expect("Failed to create temp metadata file");
        AvroWriter::new(&mut file)
            .finish(&mut df)
            .expect("Failed to write AVRO");
        std::fs::rename(&tmp_path, &self.metadata_path).expect("Failed to rename metadata file");
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

        let old_hashes = self
            .inode_meta
            .get(&inode)
            .map(|m| m.blob_hashes.clone())
            .unwrap_or_default();

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

        for hash in old_hashes {
            self.delete_blob_if_unreferenced(&hash);
        }
    }
}

impl Filesystem for NofsFS {
    fn init(
        &mut self,
        _req: &Request<'_>,
        _config: &mut KernelConfig,
    ) -> Result<(), libc::c_int> {
        std::fs::create_dir_all(&self.blob_dir).map_err(|_| libc::EIO)?;
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
                for hash in old_hashes {
                    self.delete_blob_if_unreferenced(&hash);
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
            let old_hashes = self
                .inode_meta
                .remove(&child_ino)
                .map(|m| m.blob_hashes)
                .unwrap_or_default();
            for hash in old_hashes {
                self.delete_blob_if_unreferenced(&hash);
            }
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
                    let old_hashes = self
                        .inode_meta
                        .remove(&replaced_ino)
                        .map(|m| m.blob_hashes)
                        .unwrap_or_default();
                    for hash in old_hashes {
                        self.delete_blob_if_unreferenced(&hash);
                    }
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
