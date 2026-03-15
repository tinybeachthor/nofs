import errno
import logging
import os
import stat as stat_m
from collections import defaultdict
from collections.abc import Sequence
from os import fsdecode, fsencode
from typing import cast

import pyfuse3
from pyfuse3 import (
    EntryAttributes,
    FileHandleT,
    FileInfo,
    FUSEError,
    InodeT,
    ReaddirToken,
    RequestContext,
    SetattrFields,
    StatvfsData,
)

log = logging.getLogger(__name__)


class PassthroughFS(pyfuse3.Operations):
    def __init__(self, source: str) -> None:
        super().__init__()
        self._source = os.path.realpath(source)
        self._inode_path_map: dict[InodeT, str | set[str]] = {
            pyfuse3.ROOT_INODE: self._source,
        }
        self._lookup_cnt: defaultdict[InodeT, int] = defaultdict(lambda: 0)
        self._fd_inode_map: dict[int, InodeT] = {}
        self._inode_fd_map: dict[InodeT, int] = {}
        self._fd_open_count: dict[int, int] = {}

    def _inode_to_path(self, inode: InodeT) -> str:
        try:
            val = self._inode_path_map[inode]
        except KeyError:
            raise FUSEError(errno.ENOENT)
        if isinstance(val, set):
            val = next(iter(val))
        return val

    def _add_path(self, inode: InodeT, path: str) -> None:
        self._lookup_cnt[inode] += 1
        if inode not in self._inode_path_map:
            self._inode_path_map[inode] = path
            return
        val = self._inode_path_map[inode]
        if isinstance(val, set):
            val.add(path)
        elif val != path:
            self._inode_path_map[inode] = {path, val}

    def _forget_path(self, inode: InodeT, path: str) -> None:
        val = self._inode_path_map[inode]
        if isinstance(val, set):
            val.remove(path)
            if len(val) == 1:
                self._inode_path_map[inode] = next(iter(val))
        else:
            del self._inode_path_map[inode]

    def _getattr(
        self, path: str | None = None, fd: int | None = None
    ) -> EntryAttributes:
        assert fd is None or path is None
        assert not (fd is None and path is None)
        try:
            if fd is None:
                assert path is not None
                stat = os.lstat(path)
            else:
                stat = os.fstat(fd)
        except OSError as exc:
            assert exc.errno is not None
            raise FUSEError(exc.errno)

        entry = EntryAttributes()
        for attr in (
            "st_ino",
            "st_mode",
            "st_nlink",
            "st_uid",
            "st_gid",
            "st_rdev",
            "st_size",
            "st_atime_ns",
            "st_mtime_ns",
            "st_ctime_ns",
        ):
            setattr(entry, attr, getattr(stat, attr))
        entry.generation = 0
        entry.entry_timeout = 0
        entry.attr_timeout = 0
        entry.st_blksize = 512
        entry.st_blocks = (entry.st_size + entry.st_blksize - 1) // entry.st_blksize
        return entry

    # Lifecycle

    async def forget(
        self, inode_list: Sequence[tuple[InodeT, int]]
    ) -> None:
        for inode, nlookup in inode_list:
            if self._lookup_cnt[inode] > nlookup:
                self._lookup_cnt[inode] -= nlookup
                continue
            assert inode not in self._inode_fd_map
            del self._lookup_cnt[inode]
            try:
                del self._inode_path_map[inode]
            except KeyError:
                pass

    # Filesystem metadata

    async def lookup(
        self, parent_inode: InodeT, name: bytes, ctx: RequestContext
    ) -> EntryAttributes:
        name_str = fsdecode(name)
        path = os.path.join(self._inode_to_path(parent_inode), name_str)
        attr = self._getattr(path=path)
        if name_str != "." and name_str != "..":
            self._add_path(InodeT(attr.st_ino), path)
        return attr

    async def getattr(
        self, inode: InodeT, ctx: RequestContext | None = None
    ) -> EntryAttributes:
        if inode in self._inode_fd_map:
            return self._getattr(fd=self._inode_fd_map[inode])
        else:
            return self._getattr(path=self._inode_to_path(inode))

    async def setattr(
        self,
        inode: InodeT,
        attr: EntryAttributes,
        fields: SetattrFields,
        fh: FileHandleT | None,
        ctx: RequestContext,
    ) -> EntryAttributes:
        try:
            if fields.update_size:
                if fh is None:
                    os.truncate(self._inode_to_path(inode), attr.st_size)
                else:
                    os.ftruncate(fh, attr.st_size)

            if fields.update_mode:
                assert not stat_m.S_ISLNK(attr.st_mode)
                if fh is None:
                    os.chmod(
                        self._inode_to_path(inode), stat_m.S_IMODE(attr.st_mode)
                    )
                else:
                    os.fchmod(fh, stat_m.S_IMODE(attr.st_mode))

            if fields.update_uid and fields.update_gid:
                if fh is None:
                    os.chown(
                        self._inode_to_path(inode),
                        attr.st_uid,
                        attr.st_gid,
                        follow_symlinks=False,
                    )
                else:
                    os.fchown(fh, attr.st_uid, attr.st_gid)
            elif fields.update_uid:
                if fh is None:
                    os.chown(
                        self._inode_to_path(inode),
                        attr.st_uid,
                        -1,
                        follow_symlinks=False,
                    )
                else:
                    os.fchown(fh, attr.st_uid, -1)
            elif fields.update_gid:
                if fh is None:
                    os.chown(
                        self._inode_to_path(inode),
                        -1,
                        attr.st_gid,
                        follow_symlinks=False,
                    )
                else:
                    os.fchown(fh, -1, attr.st_gid)

            if fields.update_atime and fields.update_mtime:
                if fh is None:
                    os.utime(
                        self._inode_to_path(inode),
                        None,
                        follow_symlinks=False,
                        ns=(attr.st_atime_ns, attr.st_mtime_ns),
                    )
                else:
                    os.utime(fh, None, ns=(attr.st_atime_ns, attr.st_mtime_ns))
            elif fields.update_atime or fields.update_mtime:
                if fh is None:
                    path = self._inode_to_path(inode)
                    oldstat = os.stat(path, follow_symlinks=False)
                else:
                    oldstat = os.fstat(fh)
                if not fields.update_atime:
                    attr.st_atime_ns = oldstat.st_atime_ns
                else:
                    attr.st_mtime_ns = oldstat.st_mtime_ns
                if fh is None:
                    os.utime(
                        path,
                        None,
                        follow_symlinks=False,
                        ns=(attr.st_atime_ns, attr.st_mtime_ns),
                    )
                else:
                    os.utime(fh, None, ns=(attr.st_atime_ns, attr.st_mtime_ns))

        except OSError as exc:
            assert exc.errno is not None
            raise FUSEError(exc.errno)

        return await self.getattr(inode, ctx)

    async def access(self, inode: InodeT, mode: int, ctx: RequestContext) -> bool:
        path = self._inode_to_path(inode)
        return os.access(path, mode)

    async def readlink(self, inode: InodeT, ctx: RequestContext) -> bytes:
        path = self._inode_to_path(inode)
        try:
            target = os.readlink(path)
        except OSError as exc:
            assert exc.errno is not None
            raise FUSEError(exc.errno)
        return fsencode(target)

    async def statfs(self, ctx: RequestContext) -> StatvfsData:
        root = self._inode_path_map[pyfuse3.ROOT_INODE]
        assert isinstance(root, str)
        try:
            statfs = os.statvfs(root)
        except OSError as exc:
            assert exc.errno is not None
            raise FUSEError(exc.errno)
        stat_ = StatvfsData()
        for attr in (
            "f_bsize",
            "f_frsize",
            "f_blocks",
            "f_bfree",
            "f_bavail",
            "f_files",
            "f_ffree",
            "f_favail",
        ):
            setattr(stat_, attr, getattr(statfs, attr))
        stat_.f_namemax = statfs.f_namemax - (len(root) + 1)
        return stat_

    # Directory operations

    async def opendir(self, inode: InodeT, ctx: RequestContext) -> FileHandleT:
        return FileHandleT(inode)

    async def readdir(
        self, fh: FileHandleT, start_id: int, token: ReaddirToken
    ) -> None:
        path = self._inode_to_path(InodeT(fh))
        entries: list[tuple[InodeT, str, EntryAttributes]] = []
        for name in os.listdir(path):
            if name == "." or name == "..":
                continue
            attr = self._getattr(path=os.path.join(path, name))
            entries.append((InodeT(attr.st_ino), name, attr))

        for ino, name, attr in sorted(entries):
            if ino <= start_id:
                continue
            if not pyfuse3.readdir_reply(token, fsencode(name), attr, ino):
                break
            self._add_path(attr.st_ino, os.path.join(path, name))

    async def mkdir(
        self,
        parent_inode: InodeT,
        name: bytes,
        mode: int,
        ctx: RequestContext,
    ) -> EntryAttributes:
        path = os.path.join(self._inode_to_path(parent_inode), fsdecode(name))
        try:
            os.mkdir(path, mode=(mode & ~ctx.umask))
            os.chown(path, ctx.uid, ctx.gid)
        except OSError as exc:
            assert exc.errno is not None
            raise FUSEError(exc.errno)
        attr = self._getattr(path=path)
        self._add_path(attr.st_ino, path)
        return attr

    async def rmdir(
        self, parent_inode: InodeT, name: bytes, ctx: RequestContext
    ) -> None:
        parent = self._inode_to_path(parent_inode)
        path = os.path.join(parent, fsdecode(name))
        try:
            inode = os.lstat(path).st_ino
            os.rmdir(path)
        except OSError as exc:
            assert exc.errno is not None
            raise FUSEError(exc.errno)
        if inode in self._lookup_cnt:
            self._forget_path(InodeT(inode), path)

    # File creation and removal

    async def mknod(
        self,
        parent_inode: InodeT,
        name: bytes,
        mode: int,
        rdev: int,
        ctx: RequestContext,
    ) -> EntryAttributes:
        path = os.path.join(self._inode_to_path(parent_inode), fsdecode(name))
        try:
            os.mknod(path, mode=(mode & ~ctx.umask), device=rdev)
            os.chown(path, ctx.uid, ctx.gid)
        except OSError as exc:
            assert exc.errno is not None
            raise FUSEError(exc.errno)
        attr = self._getattr(path=path)
        self._add_path(attr.st_ino, path)
        return attr

    async def unlink(
        self, parent_inode: InodeT, name: bytes, ctx: RequestContext
    ) -> None:
        parent = self._inode_to_path(parent_inode)
        path = os.path.join(parent, fsdecode(name))
        try:
            inode = os.lstat(path).st_ino
            os.unlink(path)
        except OSError as exc:
            assert exc.errno is not None
            raise FUSEError(exc.errno)
        if inode in self._lookup_cnt:
            self._forget_path(InodeT(inode), path)

    async def create(
        self,
        parent_inode: InodeT,
        name: bytes,
        mode: int,
        flags: int,
        ctx: RequestContext,
    ) -> tuple[FileInfo, EntryAttributes]:
        path = os.path.join(self._inode_to_path(parent_inode), fsdecode(name))
        try:
            fd = os.open(path, flags | os.O_CREAT | os.O_TRUNC)
        except OSError as exc:
            assert exc.errno is not None
            raise FUSEError(exc.errno)
        attr = self._getattr(fd=fd)
        self._add_path(attr.st_ino, path)
        self._inode_fd_map[attr.st_ino] = fd
        self._fd_inode_map[fd] = attr.st_ino
        self._fd_open_count[fd] = 1
        return (FileInfo(fh=cast(FileHandleT, fd)), attr)

    # Path operations

    async def symlink(
        self,
        parent_inode: InodeT,
        name: bytes,
        target: bytes,
        ctx: RequestContext,
    ) -> EntryAttributes:
        parent = self._inode_to_path(parent_inode)
        path = os.path.join(parent, fsdecode(name))
        try:
            os.symlink(fsdecode(target), path)
            os.lchown(path, ctx.uid, ctx.gid)
        except OSError as exc:
            assert exc.errno is not None
            raise FUSEError(exc.errno)
        inode = InodeT(os.lstat(path).st_ino)
        self._add_path(inode, path)
        return await self.getattr(inode, ctx)

    async def link(
        self,
        inode: InodeT,
        new_parent_inode: InodeT,
        new_name: bytes,
        ctx: RequestContext,
    ) -> EntryAttributes:
        parent = self._inode_to_path(new_parent_inode)
        path = os.path.join(parent, fsdecode(new_name))
        try:
            os.link(
                self._inode_to_path(inode), path, follow_symlinks=False
            )
        except OSError as exc:
            assert exc.errno is not None
            raise FUSEError(exc.errno)
        self._add_path(inode, path)
        return await self.getattr(inode, ctx)

    async def rename(
        self,
        parent_inode_old: InodeT,
        name_old: bytes,
        parent_inode_new: InodeT,
        name_new: bytes,
        flags: int,
        ctx: RequestContext,
    ) -> None:
        if flags != 0:
            raise FUSEError(errno.EINVAL)

        parent_old = self._inode_to_path(parent_inode_old)
        parent_new = self._inode_to_path(parent_inode_new)
        path_old = os.path.join(parent_old, fsdecode(name_old))
        path_new = os.path.join(parent_new, fsdecode(name_new))
        try:
            os.rename(path_old, path_new)
            inode = cast(InodeT, os.lstat(path_new).st_ino)
        except OSError as exc:
            assert exc.errno is not None
            raise FUSEError(exc.errno)
        if inode not in self._lookup_cnt:
            return

        val = self._inode_path_map[inode]
        if isinstance(val, set):
            assert len(val) > 1
            val.add(path_new)
            val.remove(path_old)
        else:
            assert val == path_old
            self._inode_path_map[inode] = path_new

    # File I/O

    async def open(
        self, inode: InodeT, flags: int, ctx: RequestContext
    ) -> FileInfo:
        if inode in self._inode_fd_map:
            fd = self._inode_fd_map[inode]
            self._fd_open_count[fd] += 1
            return FileInfo(fh=FileHandleT(fd))
        assert flags & os.O_CREAT == 0
        try:
            fd = os.open(self._inode_to_path(inode), flags)
        except OSError as exc:
            assert exc.errno is not None
            raise FUSEError(exc.errno)
        self._inode_fd_map[inode] = fd
        self._fd_inode_map[fd] = inode
        self._fd_open_count[fd] = 1
        return FileInfo(fh=cast(FileHandleT, fd))

    async def read(self, fh: FileHandleT, off: int, size: int) -> bytes:
        os.lseek(fh, off, os.SEEK_SET)
        return os.read(fh, size)

    async def write(self, fh: FileHandleT, off: int, buf: bytes) -> int:
        os.lseek(fh, off, os.SEEK_SET)
        return os.write(fh, buf)

    async def flush(self, fh: FileHandleT) -> None:
        os.close(os.dup(fh))

    async def release(self, fh: FileHandleT) -> None:
        if self._fd_open_count[fh] > 1:
            self._fd_open_count[fh] -= 1
            return
        del self._fd_open_count[fh]
        inode = self._fd_inode_map[fh]
        del self._inode_fd_map[inode]
        del self._fd_inode_map[fh]
        try:
            os.close(fh)
        except OSError as exc:
            assert exc.errno is not None
            raise FUSEError(exc.errno)

    async def fsync(self, fh: FileHandleT, datasync: bool) -> None:
        try:
            if datasync:
                os.fdatasync(fh)
            else:
                os.fsync(fh)
        except OSError as exc:
            assert exc.errno is not None
            raise FUSEError(exc.errno)
