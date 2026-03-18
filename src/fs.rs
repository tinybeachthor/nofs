use fuser::{
    FileAttr, FileType, Filesystem, ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory, ReplyEmpty,
    ReplyEntry, ReplyOpen, ReplyStatfs, ReplyWrite, Request, TimeOrNow,
};
use libc::{self, c_int};

use std::collections::HashMap;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const TTL: Duration = Duration::from_secs(0);
const FUSE_ROOT_INODE: u64 = 1;

#[derive(Debug, Clone)]
enum InodePaths {
    Single(PathBuf),
    Multi(Vec<PathBuf>),
}

pub struct PassthroughFS {
    inode_path_map: HashMap<u64, InodePaths>,
    lookup_cnt: HashMap<u64, u64>,
    fd_inode_map: HashMap<i32, u64>,
    inode_fd_map: HashMap<u64, i32>,
    fd_open_count: HashMap<i32, u64>,
}

impl PassthroughFS {
    pub fn new(source: PathBuf) -> Self {
        let mut inode_path_map = HashMap::new();
        inode_path_map.insert(FUSE_ROOT_INODE, InodePaths::Single(source));
        PassthroughFS {
            inode_path_map,
            lookup_cnt: HashMap::new(),
            fd_inode_map: HashMap::new(),
            inode_fd_map: HashMap::new(),
            fd_open_count: HashMap::new(),
        }
    }

    fn inode_to_path(&self, inode: u64) -> Result<PathBuf, c_int> {
        match self.inode_path_map.get(&inode) {
            Some(InodePaths::Single(p)) => Ok(p.clone()),
            Some(InodePaths::Multi(set)) => Ok(set.iter().next().unwrap().clone()),
            None => Err(libc::ENOENT),
        }
    }

    fn add_path(&mut self, inode: u64, path: PathBuf) {
        *self.lookup_cnt.entry(inode).or_insert(0) += 1;
        match self.inode_path_map.get_mut(&inode) {
            None => {
                self.inode_path_map
                    .insert(inode, InodePaths::Single(path));
            }
            Some(entry) => match entry {
                InodePaths::Multi(set) => {
                    if !set.contains(&path) {
                        set.push(path);
                    }
                }
                InodePaths::Single(existing) => {
                    if *existing != path {
                        let old = existing.clone();
                        *entry = InodePaths::Multi(vec![old, path]);
                    }
                }
            },
        }
    }

    fn forget_path(&mut self, inode: u64, path: &Path) {
        match self.inode_path_map.get_mut(&inode) {
            Some(InodePaths::Multi(set)) => {
                set.retain(|p| p != path);
                if set.len() == 1 {
                    let remaining = set[0].clone();
                    self.inode_path_map
                        .insert(inode, InodePaths::Single(remaining));
                }
            }
            Some(InodePaths::Single(_)) => {
                self.inode_path_map.remove(&inode);
            }
            None => {}
        }
    }

    fn get_attr_path(&self, path: &Path) -> Result<FileAttr, c_int> {
        let meta = std::fs::symlink_metadata(path).map_err(|e| e.raw_os_error().unwrap_or(libc::EIO))?;
        Ok(metadata_to_file_attr(&meta))
    }

    fn get_attr_fd(&self, fd: i32) -> Result<FileAttr, c_int> {
        let meta = fd_metadata(fd)?;
        Ok(metadata_to_file_attr(&meta))
    }

    fn get_attr(&self, inode: u64) -> Result<FileAttr, c_int> {
        if let Some(&fd) = self.inode_fd_map.get(&inode) {
            self.get_attr_fd(fd)
        } else {
            let path = self.inode_to_path(inode)?;
            self.get_attr_path(&path)
        }
    }
}

fn metadata_to_file_attr(meta: &std::fs::Metadata) -> FileAttr {
    let kind = mode_to_file_type(meta.mode());
    let blksize = 512u64;
    FileAttr {
        ino: meta.ino(),
        size: meta.size(),
        blocks: (meta.size() + blksize - 1) / blksize,
        atime: UNIX_EPOCH + Duration::from_nanos(meta.atime() as u64 * 1_000_000_000 + meta.atime_nsec() as u64),
        mtime: UNIX_EPOCH + Duration::from_nanos(meta.mtime() as u64 * 1_000_000_000 + meta.mtime_nsec() as u64),
        ctime: UNIX_EPOCH + Duration::from_nanos(meta.ctime() as u64 * 1_000_000_000 + meta.ctime_nsec() as u64),
        crtime: UNIX_EPOCH,
        kind,
        perm: (meta.mode() & 0o7777) as u16,
        nlink: meta.nlink() as u32,
        uid: meta.uid(),
        gid: meta.gid(),
        rdev: meta.rdev() as u32,
        blksize: blksize as u32,
        flags: 0,
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

fn fd_metadata(fd: i32) -> Result<std::fs::Metadata, c_int> {
    use std::os::unix::io::FromRawFd;
    // We use File::from_raw_fd to get metadata, then forget the file so we don't close the fd
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    let meta = file.metadata().map_err(|e| e.raw_os_error().unwrap_or(libc::EIO));
    std::mem::forget(file);
    meta
}

fn time_or_now_to_timespec(t: TimeOrNow) -> libc::timespec {
    match t {
        TimeOrNow::SpecificTime(st) => {
            let d = st.duration_since(UNIX_EPOCH).unwrap_or_default();
            libc::timespec {
                tv_sec: d.as_secs() as i64,
                tv_nsec: d.subsec_nanos() as i64,
            }
        }
        TimeOrNow::Now => {
            let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
            libc::timespec {
                tv_sec: d.as_secs() as i64,
                tv_nsec: d.subsec_nanos() as i64,
            }
        }
    }
}

fn system_time_to_timespec(st: SystemTime) -> libc::timespec {
    let d = st.duration_since(UNIX_EPOCH).unwrap_or_default();
    libc::timespec {
        tv_sec: d.as_secs() as i64,
        tv_nsec: d.subsec_nanos() as i64,
    }
}

impl Filesystem for PassthroughFS {
    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let parent_path = match self.inode_to_path(parent) {
            Ok(p) => p,
            Err(e) => return reply.error(e),
        };
        let path = parent_path.join(name);
        let attr = match self.get_attr_path(&path) {
            Ok(a) => a,
            Err(e) => return reply.error(e),
        };
        let name_bytes = name.as_bytes();
        if name_bytes != b"." && name_bytes != b".." {
            self.add_path(attr.ino, path);
        }
        reply.entry(&TTL, &attr, 0);
    }

    fn forget(&mut self, _req: &Request, ino: u64, nlookup: u64) {
        if let Some(count) = self.lookup_cnt.get_mut(&ino) {
            if *count > nlookup {
                *count -= nlookup;
                return;
            }
        }
        self.lookup_cnt.remove(&ino);
        self.inode_path_map.remove(&ino);
    }

    fn getattr(&mut self, _req: &Request, ino: u64, _fh: Option<u64>, reply: ReplyAttr) {
        match self.get_attr(ino) {
            Ok(attr) => reply.attr(&TTL, &attr),
            Err(e) => reply.error(e),
        }
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
        let path = match self.inode_to_path(ino) {
            Ok(p) => p,
            Err(e) => return reply.error(e),
        };

        // Truncate
        if let Some(new_size) = size {
            let rc = if let Some(fd) = fh {
                unsafe { libc::ftruncate(fd as c_int, new_size as libc::off_t) }
            } else {
                let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
                unsafe { libc::truncate(c_path.as_ptr(), new_size as libc::off_t) }
            };
            if rc != 0 {
                return reply.error(std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO));
            }
        }

        // chmod
        if let Some(new_mode) = mode {
            let rc = if let Some(fd) = fh {
                unsafe { libc::fchmod(fd as c_int, new_mode) }
            } else {
                let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
                unsafe { libc::chmod(c_path.as_ptr(), new_mode) }
            };
            if rc != 0 {
                return reply.error(std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO));
            }
        }

        // chown
        let new_uid = uid.map(|u| u as libc::uid_t).unwrap_or(u32::MAX);
        let new_gid = gid.map(|g| g as libc::gid_t).unwrap_or(u32::MAX);
        if uid.is_some() || gid.is_some() {
            let rc = if let Some(fd) = fh {
                unsafe { libc::fchown(fd as c_int, new_uid, new_gid) }
            } else {
                let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
                unsafe { libc::lchown(c_path.as_ptr(), new_uid, new_gid) }
            };
            if rc != 0 {
                return reply.error(std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO));
            }
        }

        // utimes
        if atime.is_some() || mtime.is_some() {
            // Get current times for any not being set
            let current_attr = match self.get_attr(ino) {
                Ok(a) => a,
                Err(e) => return reply.error(e),
            };

            let atime_spec = match atime {
                Some(t) => time_or_now_to_timespec(t),
                None => system_time_to_timespec(current_attr.atime),
            };
            let mtime_spec = match mtime {
                Some(t) => time_or_now_to_timespec(t),
                None => system_time_to_timespec(current_attr.mtime),
            };
            let times = [atime_spec, mtime_spec];

            let rc = if let Some(fd) = fh {
                unsafe { libc::futimens(fd as c_int, times.as_ptr()) }
            } else {
                let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
                unsafe { libc::utimensat(libc::AT_FDCWD, c_path.as_ptr(), times.as_ptr(), libc::AT_SYMLINK_NOFOLLOW) }
            };
            if rc != 0 {
                return reply.error(std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO));
            }
        }

        match self.get_attr(ino) {
            Ok(attr) => reply.attr(&TTL, &attr),
            Err(e) => reply.error(e),
        }
    }

    fn readlink(&mut self, _req: &Request, ino: u64, reply: ReplyData) {
        let path = match self.inode_to_path(ino) {
            Ok(p) => p,
            Err(e) => return reply.error(e),
        };
        match std::fs::read_link(&path) {
            Ok(target) => reply.data(target.as_os_str().as_bytes()),
            Err(e) => reply.error(e.raw_os_error().unwrap_or(libc::EIO)),
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
        let parent_path = match self.inode_to_path(parent) {
            Ok(p) => p,
            Err(e) => return reply.error(e),
        };
        let path = parent_path.join(name);
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        let rc = unsafe { libc::mknod(c_path.as_ptr(), mode, rdev as libc::dev_t) };
        if rc != 0 {
            return reply.error(std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO));
        }
        let rc = unsafe { libc::chown(c_path.as_ptr(), req.uid(), req.gid()) };
        if rc != 0 {
            return reply.error(std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO));
        }
        let attr = match self.get_attr_path(&path) {
            Ok(a) => a,
            Err(e) => return reply.error(e),
        };
        self.add_path(attr.ino, path);
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
        let parent_path = match self.inode_to_path(parent) {
            Ok(p) => p,
            Err(e) => return reply.error(e),
        };
        let path = parent_path.join(name);
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        let rc = unsafe { libc::mkdir(c_path.as_ptr(), mode) };
        if rc != 0 {
            return reply.error(std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO));
        }
        let rc = unsafe { libc::chown(c_path.as_ptr(), req.uid(), req.gid()) };
        if rc != 0 {
            return reply.error(std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO));
        }
        let attr = match self.get_attr_path(&path) {
            Ok(a) => a,
            Err(e) => return reply.error(e),
        };
        self.add_path(attr.ino, path);
        reply.entry(&TTL, &attr, 0);
    }

    fn unlink(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let parent_path = match self.inode_to_path(parent) {
            Ok(p) => p,
            Err(e) => return reply.error(e),
        };
        let path = parent_path.join(name);
        let ino = match std::fs::symlink_metadata(&path) {
            Ok(m) => m.ino(),
            Err(e) => return reply.error(e.raw_os_error().unwrap_or(libc::EIO)),
        };
        match std::fs::remove_file(&path) {
            Ok(()) => {
                if self.lookup_cnt.contains_key(&ino) {
                    self.forget_path(ino, &path);
                }
                reply.ok();
            }
            Err(e) => reply.error(e.raw_os_error().unwrap_or(libc::EIO)),
        }
    }

    fn rmdir(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let parent_path = match self.inode_to_path(parent) {
            Ok(p) => p,
            Err(e) => return reply.error(e),
        };
        let path = parent_path.join(name);
        let ino = match std::fs::symlink_metadata(&path) {
            Ok(m) => m.ino(),
            Err(e) => return reply.error(e.raw_os_error().unwrap_or(libc::EIO)),
        };
        match std::fs::remove_dir(&path) {
            Ok(()) => {
                if self.lookup_cnt.contains_key(&ino) {
                    self.forget_path(ino, &path);
                }
                reply.ok();
            }
            Err(e) => reply.error(e.raw_os_error().unwrap_or(libc::EIO)),
        }
    }

    fn symlink(
        &mut self,
        _req: &Request,
        parent: u64,
        link_name: &OsStr,
        target: &Path,
        reply: ReplyEntry,
    ) {
        let parent_path = match self.inode_to_path(parent) {
            Ok(p) => p,
            Err(e) => return reply.error(e),
        };
        let path = parent_path.join(link_name);
        match std::os::unix::fs::symlink(target, &path) {
            Ok(()) => {}
            Err(e) => return reply.error(e.raw_os_error().unwrap_or(libc::EIO)),
        }
        let attr = match self.get_attr_path(&path) {
            Ok(a) => a,
            Err(e) => return reply.error(e),
        };
        self.add_path(attr.ino, path);
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
        let parent_path = match self.inode_to_path(parent) {
            Ok(p) => p,
            Err(e) => return reply.error(e),
        };
        let new_parent_path = match self.inode_to_path(newparent) {
            Ok(p) => p,
            Err(e) => return reply.error(e),
        };
        let old_path = parent_path.join(name);
        let new_path = new_parent_path.join(newname);
        match std::fs::rename(&old_path, &new_path) {
            Ok(()) => {}
            Err(e) => return reply.error(e.raw_os_error().unwrap_or(libc::EIO)),
        }
        let ino = match std::fs::symlink_metadata(&new_path) {
            Ok(m) => m.ino(),
            Err(e) => return reply.error(e.raw_os_error().unwrap_or(libc::EIO)),
        };
        if self.lookup_cnt.contains_key(&ino) {
            match self.inode_path_map.get_mut(&ino) {
                Some(InodePaths::Multi(set)) => {
                    set.retain(|p| p != &old_path);
                    if !set.contains(&new_path) {
                        set.push(new_path);
                    }
                }
                Some(entry @ InodePaths::Single(_)) => {
                    *entry = InodePaths::Single(new_path);
                }
                None => {}
            }
        }
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
        let src_path = match self.inode_to_path(ino) {
            Ok(p) => p,
            Err(e) => return reply.error(e),
        };
        let new_parent_path = match self.inode_to_path(newparent) {
            Ok(p) => p,
            Err(e) => return reply.error(e),
        };
        let new_path = new_parent_path.join(newname);
        match std::fs::hard_link(&src_path, &new_path) {
            Ok(()) => {}
            Err(e) => return reply.error(e.raw_os_error().unwrap_or(libc::EIO)),
        }
        self.add_path(ino, new_path);
        match self.get_attr(ino) {
            Ok(attr) => reply.entry(&TTL, &attr, 0),
            Err(e) => reply.error(e),
        }
    }

    fn open(&mut self, _req: &Request, ino: u64, flags: i32, reply: ReplyOpen) {
        if let Some(&fd) = self.inode_fd_map.get(&ino) {
            *self.fd_open_count.get_mut(&fd).unwrap() += 1;
            return reply.opened(fd as u64, 0);
        }
        let path = match self.inode_to_path(ino) {
            Ok(p) => p,
            Err(e) => return reply.error(e),
        };
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // Strip O_CREAT and O_TRUNC since open() should only open existing files
        let open_flags = flags & !(libc::O_CREAT | libc::O_TRUNC);
        let fd = unsafe { libc::open(c_path.as_ptr(), open_flags) };
        if fd < 0 {
            return reply.error(std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO));
        }
        self.inode_fd_map.insert(ino, fd);
        self.fd_inode_map.insert(fd, ino);
        self.fd_open_count.insert(fd, 1);
        reply.opened(fd as u64, 0);
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
        let fd = fh as c_int;
        unsafe { libc::lseek(fd, offset, libc::SEEK_SET) };
        let mut buf = vec![0u8; size as usize];
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, size as usize) };
        if n < 0 {
            return reply.error(std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO));
        }
        buf.truncate(n as usize);
        reply.data(&buf);
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
        let fd = fh as c_int;
        unsafe { libc::lseek(fd, offset, libc::SEEK_SET) };
        let n = unsafe { libc::write(fd, data.as_ptr() as *const libc::c_void, data.len()) };
        if n < 0 {
            return reply.error(std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO));
        }
        reply.written(n as u32);
    }

    fn flush(&mut self, _req: &Request, _ino: u64, fh: u64, _lock_owner: u64, reply: ReplyEmpty) {
        let fd = fh as c_int;
        let dup_fd = unsafe { libc::dup(fd) };
        if dup_fd >= 0 {
            unsafe { libc::close(dup_fd) };
        }
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
        let fd = fh as c_int;
        if let Some(count) = self.fd_open_count.get_mut(&fd) {
            if *count > 1 {
                *count -= 1;
                return reply.ok();
            }
        }
        self.fd_open_count.remove(&fd);
        if let Some(ino) = self.fd_inode_map.remove(&fd) {
            self.inode_fd_map.remove(&ino);
        }
        unsafe { libc::close(fd) };
        reply.ok();
    }

    fn fsync(&mut self, _req: &Request, _ino: u64, fh: u64, datasync: bool, reply: ReplyEmpty) {
        let fd = fh as c_int;
        let rc = if datasync {
            unsafe { libc::fdatasync(fd) }
        } else {
            unsafe { libc::fsync(fd) }
        };
        if rc != 0 {
            reply.error(std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO));
        } else {
            reply.ok();
        }
    }

    fn opendir(&mut self, _req: &Request, ino: u64, _flags: i32, reply: ReplyOpen) {
        // Just verify the inode exists
        match self.inode_to_path(ino) {
            Ok(_) => reply.opened(ino, 0),
            Err(e) => reply.error(e),
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
        let path = match self.inode_to_path(ino) {
            Ok(p) => p,
            Err(e) => return reply.error(e),
        };
        let entries = match std::fs::read_dir(&path) {
            Ok(rd) => rd,
            Err(e) => return reply.error(e.raw_os_error().unwrap_or(libc::EIO)),
        };
        // Collect and sort by inode like the Python version
        let mut entry_list: Vec<(u64, std::ffi::OsString, FileType)> = Vec::new();
        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            let ft = mode_to_file_type(meta.mode());
            entry_list.push((meta.ino(), entry.file_name(), ft));
        }
        entry_list.sort_by_key(|(ino, _, _)| *ino);

        for (i, (entry_ino, name, ft)) in entry_list.iter().enumerate() {
            let entry_offset = (i + 1) as i64;
            if entry_offset <= offset {
                continue;
            }
            let full_path = path.join(&name);
            if reply.add(*entry_ino, entry_offset, *ft, &name) {
                break;
            }
            self.add_path(*entry_ino, full_path);
        }
        reply.ok();
    }

    fn access(&mut self, _req: &Request, ino: u64, mask: i32, reply: ReplyEmpty) {
        let path = match self.inode_to_path(ino) {
            Ok(p) => p,
            Err(e) => return reply.error(e),
        };
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        let rc = unsafe { libc::access(c_path.as_ptr(), mask) };
        if rc == 0 {
            reply.ok();
        } else {
            reply.error(std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EACCES));
        }
    }

    fn create(
        &mut self,
        _req: &Request,
        parent: u64,
        name: &OsStr,
        mode: u32,
        _umask: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        let parent_path = match self.inode_to_path(parent) {
            Ok(p) => p,
            Err(e) => return reply.error(e),
        };
        let path = parent_path.join(name);
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        let fd = unsafe { libc::open(c_path.as_ptr(), flags | libc::O_CREAT | libc::O_TRUNC, mode) };
        if fd < 0 {
            return reply.error(std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO));
        }
        let attr = match self.get_attr_fd(fd) {
            Ok(a) => a,
            Err(e) => {
                unsafe { libc::close(fd) };
                return reply.error(e);
            }
        };
        self.add_path(attr.ino, path);
        self.inode_fd_map.insert(attr.ino, fd);
        self.fd_inode_map.insert(fd, attr.ino);
        self.fd_open_count.insert(fd, 1);
        reply.created(&TTL, &attr, 0, fd as u64, 0);
    }

    fn statfs(&mut self, _req: &Request, _ino: u64, reply: ReplyStatfs) {
        let root = match self.inode_path_map.get(&FUSE_ROOT_INODE) {
            Some(InodePaths::Single(p)) => p.clone(),
            _ => return reply.error(libc::ENOENT),
        };
        let c_path = std::ffi::CString::new(root.as_os_str().as_bytes()).unwrap();
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
        if rc != 0 {
            return reply.error(std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO));
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
