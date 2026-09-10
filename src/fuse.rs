// SPDX-License-Identifier: GPL-3.0-only
use super::*;
use fuser::*;
use std::{
    collections::{HashMap, VecDeque},
    ffi::OsStr,
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

fn errno(e: FsError) -> Errno {
    match e.kind {
        FsErrorKind::NotFound => Errno::ENOENT,
        FsErrorKind::PermissionDenied => Errno::EACCES,
        FsErrorKind::AlreadyExists => Errno::EEXIST,
        FsErrorKind::NotDirectory => Errno::ENOTDIR,
        FsErrorKind::IsDirectory => Errno::EISDIR,
        FsErrorKind::NotEmpty => Errno::ENOTEMPTY,
        FsErrorKind::InvalidInput => Errno::EINVAL,
        FsErrorKind::Unsupported => Errno::EOPNOTSUPP,
        FsErrorKind::ReadOnly => Errno::EROFS,
        FsErrorKind::Offline => Errno::ENOTCONN,
        FsErrorKind::TimedOut => Errno::ETIMEDOUT,
        FsErrorKind::Io => Errno::EIO,
    }
}
type Result<T> = std::result::Result<T, Errno>;
fn kind(m: FsKind) -> FileType {
    match m {
        FsKind::Directory => FileType::Directory,
        FsKind::Symlink => FileType::Symlink,
        _ => FileType::RegularFile,
    }
}
fn attr(ino: INodeNo, m: FsMetadata, uid: u32, gid: u32) -> FileAttr {
    let modified = UNIX_EPOCH + Duration::from_secs(m.modified.unwrap_or(0));
    FileAttr {
        ino,
        size: m.size,
        blocks: m.size.div_ceil(512),
        atime: UNIX_EPOCH + Duration::from_secs(m.accessed.unwrap_or(0)),
        mtime: modified,
        ctime: modified,
        crtime: modified,
        kind: kind(m.kind),
        perm: (m.permissions.unwrap_or(if m.kind == FsKind::Directory {
            0o700
        } else {
            0o600
        }) & 0o777) as u16,
        nlink: if m.kind == FsKind::Directory { 2 } else { 1 },
        uid,
        gid,
        rdev: 0,
        blksize: 4096,
        flags: 0,
    }
}
fn options(flags: i32, create: bool) -> FsOpenOptions {
    let mode = flags & libc::O_ACCMODE;
    FsOpenOptions {
        read: mode != libc::O_WRONLY,
        write: mode != libc::O_RDONLY || create,
        create: if !create {
            FsCreate::OpenExisting
        } else if flags & libc::O_EXCL != 0 {
            FsCreate::CreateNew
        } else {
            FsCreate::OpenOrCreate
        },
        truncate: flags & libc::O_TRUNC != 0,
    }
}
fn seconds(time: Option<TimeOrNow>) -> Option<u64> {
    time.and_then(|t| {
        match t {
            TimeOrNow::SpecificTime(t) => t,
            TimeOrNow::Now => SystemTime::now(),
        }
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|t| t.as_secs())
    })
}
struct Node {
    path: Option<MountPath>,
    lookups: u64,
    opens: u64,
}
struct Directory {
    ino: u64,
    path: MountPath,
    remote: Option<u64>,
    offset: u64,
    pending: VecDeque<FsDirectoryEntry>,
    eof: bool,
}
impl Directory {
    fn prepare(
        &mut self,
        offset: u64,
        mut call: impl FnMut(Operation) -> Result<Value>,
    ) -> Result<()> {
        if self.remote.is_some() && offset >= self.offset {
            return Ok(());
        }
        // Closing retires the server handle even if its close operation fails.
        // Clear ownership before sending it; a subsequent seek/read must reopen
        // rather than use the retired handle or buffered entries from that stream.
        self.offset = 0;
        self.pending.clear();
        self.eof = false;
        if let Some(handle) = self.remote.take() {
            call(Operation::CloseDirectory { handle })?;
        }
        self.remote = Some(
            match call(Operation::OpenDirectory {
                path: self.path.clone(),
            })? {
                Value::Handle(handle) => handle,
                _ => return Err(Errno::EIO),
            },
        );
        Ok(())
    }
}
#[cfg(test)]
mod directory_tests {
    use super::*;
    fn cursor() -> Directory {
        Directory {
            ino: 2,
            path: MountPath::root().child("directory").unwrap(),
            remote: Some(11),
            offset: 12,
            pending: VecDeque::from([FsDirectoryEntry {
                name: "stale-buffered-entry".into(),
                metadata: FsMetadata {
                    kind: FsKind::File,
                    size: 0,
                    accessed: None,
                    modified: None,
                    permissions: None,
                },
            }]),
            eof: true,
        }
    }
    #[test]
    fn failed_rewind_reopens_even_when_next_seek_uses_a_later_cookie() {
        // Failed close, failed reopen, and malformed reopen reply must all leave
        // the cursor retryable. A later seek need not repeat the rewind's offset.
        for failure in 0..3 {
            let mut dir = cursor();
            let mut calls = Vec::new();
            let result = dir.prepare(0, |op| match op {
                Operation::CloseDirectory { handle } => {
                    assert_eq!(handle, 11);
                    calls.push("close");
                    if failure == 0 {
                        Err(Errno::EIO)
                    } else {
                        Ok(Value::Unit)
                    }
                }
                Operation::OpenDirectory { .. } => {
                    calls.push("open");
                    if failure == 1 {
                        Err(Errno::EACCES)
                    } else {
                        Ok(Value::Unit)
                    }
                }
                _ => panic!("Unexpected operation"),
            });
            assert!(result.is_err());
            assert!(dir.remote.is_none());
            assert!(dir.pending.is_empty());
            assert_eq!(
                calls,
                if failure == 0 {
                    vec!["close"]
                } else {
                    vec!["close", "open"]
                }
            );
            // Rename may have happened since the failure. Use the current path.
            dir.path = MountPath::root().child("renamed").unwrap();
            dir.prepare(40, |op| match op {
                Operation::OpenDirectory { path } => {
                    assert_eq!(path.components(), &["renamed"]);
                    Ok(Value::Handle(42))
                }
                _ => panic!("Retried a retired handle"),
            })
            .unwrap();
            assert_eq!(dir.remote, Some(42));
            assert_eq!(dir.offset, 0);
            assert!(!dir.eof);
        }
    }
    #[test]
    fn forward_read_retains_stream_and_rewind_starts_fresh() {
        let mut dir = cursor();
        dir.prepare(12, |_| panic!("Sequential read reopened the stream"))
            .unwrap();
        assert_eq!(dir.pending.len(), 1);
        let mut closed = false;
        dir.prepare(0, |op| match op {
            Operation::CloseDirectory { handle: 11 } => {
                closed = true;
                Ok(Value::Unit)
            }
            Operation::OpenDirectory { .. } => {
                assert!(closed);
                Ok(Value::Handle(22))
            }
            _ => panic!("Unexpected operation"),
        })
        .unwrap();
        assert_eq!(dir.remote, Some(22));
        assert_eq!(dir.offset, 0);
        assert!(dir.pending.is_empty());
        assert!(!dir.eof);
    }
}
struct State {
    next: u64,
    nodes: HashMap<u64, Node>,
    paths: HashMap<MountPath, u64>,
    files: HashMap<u64, u64>,
    dirs: HashMap<u64, Directory>,
}
impl State {
    fn new() -> Self {
        Self {
            next: 1,
            nodes: HashMap::from([(
                1,
                Node {
                    path: Some(MountPath::root()),
                    lookups: 1,
                    opens: 0,
                },
            )]),
            paths: HashMap::from([(MountPath::root(), 1)]),
            files: HashMap::new(),
            dirs: HashMap::new(),
        }
    }
    fn lookup(&mut self, path: MountPath) -> Result<INodeNo> {
        let id = if let Some(id) = self.paths.get(&path) {
            *id
        } else {
            self.next = self.next.checked_add(1).ok_or(Errno::EOVERFLOW)?;
            self.nodes.insert(
                self.next,
                Node {
                    path: Some(path.clone()),
                    lookups: 0,
                    opens: 0,
                },
            );
            self.paths.insert(path, self.next);
            self.next
        };
        self.nodes.get_mut(&id).ok_or(Errno::ESTALE)?.lookups += 1;
        Ok(INodeNo(id))
    }
    fn collect(&mut self, id: u64) {
        if id == 1
            || !self
                .nodes
                .get(&id)
                .is_some_and(|n| n.lookups == 0 && n.opens == 0)
        {
            return;
        }
        if let Some(path) = self.nodes.remove(&id).and_then(|node| node.path) {
            self.paths.remove(&path);
        }
    }
    fn retire_path(&mut self, path: &MountPath) {
        if let Some(id) = self.paths.remove(path) {
            if let Some(node) = self.nodes.get_mut(&id) {
                node.path = None;
            }
            self.collect(id);
        }
    }
    fn forget(&mut self, id: u64, count: u64) {
        if let Some(node) = self.nodes.get_mut(&id) {
            node.lookups = node.lookups.saturating_sub(count);
        }
        self.collect(id);
    }
    fn release(&mut self, id: u64) {
        if let Some(node) = self.nodes.get_mut(&id) {
            node.opens = node.opens.saturating_sub(1);
        }
        self.collect(id);
    }
}
#[cfg(test)]
mod inode_tests {
    use super::*;
    #[test]
    fn inode_survives_until_both_kernel_and_open_references_are_gone() {
        for forget_first in [false, true] {
            let mut state = State::new();
            let path = MountPath::root().child("held").unwrap();
            let id = state.lookup(path.clone()).unwrap().0;
            assert_eq!(state.lookup(path.clone()).unwrap().0, id);
            state.nodes.get_mut(&id).unwrap().opens = 2;
            state.release(id);
            state.forget(id, 1);
            assert!(state.nodes.contains_key(&id));
            if forget_first {
                state.forget(id, 1);
            } else {
                state.release(id);
            }
            assert!(state.nodes.contains_key(&id));
            if forget_first {
                state.release(id);
            } else {
                state.forget(id, 1);
            }
            assert!(!state.nodes.contains_key(&id));
            assert!(!state.paths.contains_key(&path));
        }
    }
    #[test]
    fn retired_open_inode_cannot_remove_recreated_path_when_forgotten() {
        let mut state = State::new();
        let path = MountPath::root().child("reused").unwrap();
        let old = state.lookup(path.clone()).unwrap().0;
        state.nodes.get_mut(&old).unwrap().opens = 1;
        state.retire_path(&path);
        let new = state.lookup(path.clone()).unwrap().0;
        assert_ne!(old, new);
        assert!(state.nodes[&old].path.is_none());
        state.forget(old, 1);
        assert!(state.nodes.contains_key(&old));
        state.release(old);
        assert!(!state.nodes.contains_key(&old));
        assert_eq!(state.paths.get(&path), Some(&new));
        assert_eq!(state.nodes[&new].path.as_ref(), Some(&path));
        state.forget(1, u64::MAX);
        state.release(1);
        assert!(state.nodes.contains_key(&1));
        assert_eq!(state.paths.get(&MountPath::root()), Some(&1));
    }
}
struct Fs {
    pipe: Arc<Pipe>,
    state: Mutex<State>,
    uid: u32,
    gid: u32,
}
impl Fs {
    fn state(&self) -> Result<std::sync::MutexGuard<'_, State>> {
        self.state.lock().map_err(|_| Errno::EIO)
    }
    fn call(&self, operation: Operation) -> Result<Value> {
        self.pipe.call(operation).map_err(errno)
    }
    fn path(&self, ino: INodeNo) -> Result<MountPath> {
        self.state()?
            .nodes
            .get(&ino.0)
            .and_then(|n| n.path.clone())
            .ok_or(Errno::ESTALE)
    }
    fn child(&self, ino: INodeNo, name: &OsStr) -> Result<MountPath> {
        self.path(ino)?
            .child(name.to_str().ok_or(Errno::EILSEQ)?)
            .map_err(errno)
    }
    fn metadata(&self, ino: INodeNo, handle: Option<FileHandle>) -> Result<FileAttr> {
        let operation = if let Some(handle) =
            handle.filter(|h| self.state().is_ok_and(|s| s.files.contains_key(&h.0)))
        {
            Operation::FileMetadata { handle: handle.0 }
        } else {
            Operation::Metadata {
                path: self.path(ino)?,
            }
        };
        match self.call(operation)? {
            Value::Metadata(m) => Ok(attr(ino, m, self.uid, self.gid)),
            _ => Err(Errno::EIO),
        }
    }
    fn lookup_path(&self, path: MountPath) -> Result<FileAttr> {
        let m = match self.call(Operation::Metadata { path: path.clone() })? {
            Value::Metadata(m) => m,
            _ => return Err(Errno::EIO),
        };
        let ino = self.state()?.lookup(path)?;
        Ok(attr(ino, m, self.uid, self.gid))
    }
    fn open_file(
        &self,
        ino: INodeNo,
        path: MountPath,
        options: FsOpenOptions,
    ) -> Result<FileHandle> {
        let handle = match self.call(Operation::Open { path, options })? {
            Value::Handle(h) => h,
            _ => return Err(Errno::EIO),
        };
        let mut state = self.state()?;
        state.files.insert(handle, ino.0);
        state.nodes.get_mut(&ino.0).ok_or(Errno::ESTALE)?.opens += 1;
        Ok(FileHandle(handle))
    }
    fn remove(&self, ino: INodeNo, name: &OsStr, directory: bool) -> Result<()> {
        let path = self.child(ino, name)?;
        self.call(Operation::Remove {
            path: path.clone(),
            directory,
        })?;
        self.state()?.retire_path(&path);
        Ok(())
    }
}
macro_rules! empty_reply {
    ($reply:expr, $result:expr) => {
        match $result {
            Ok(_) => $reply.ok(),
            Err(e) => $reply.error(e),
        }
    };
}
impl Filesystem for Fs {
    fn init(&mut self, _: &Request, config: &mut KernelConfig) -> std::io::Result<()> {
        // Keep ordinary reads/writes direct. Modern Linux can separately permit
        // shared mappings, whose dirty pages reach us on msync/fsync/unmap.
        // Do not enable generic writeback caching for a remotely mutable tree.
        #[cfg(target_os = "linux")]
        if config
            .capabilities()
            .contains(InitFlags::FUSE_DIRECT_IO_ALLOW_MMAP)
        {
            config
                .add_capabilities(InitFlags::FUSE_DIRECT_IO_ALLOW_MMAP)
                .map_err(|_| std::io::Error::other("Kernel rejected shared mapping capability"))?;
            eprintln!("SHELLCANVAS_BRIDGE_SHARED_MMAP_ENABLED");
        }
        #[cfg(not(target_os = "linux"))]
        let _ = config;
        Ok(())
    }
    fn lookup(&self, _: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        match self.child(parent, name).and_then(|p| self.lookup_path(p)) {
            Ok(a) => reply.entry(&Duration::ZERO, &a, Generation(1)),
            Err(e) => reply.error(e),
        }
    }
    fn forget(&self, _: &Request, ino: INodeNo, count: u64) {
        if let Ok(mut state) = self.state() {
            state.forget(ino.0, count);
        }
    }
    fn getattr(&self, _: &Request, ino: INodeNo, fh: Option<FileHandle>, reply: ReplyAttr) {
        match self.metadata(ino, fh) {
            Ok(a) => reply.attr(&Duration::ZERO, &a),
            Err(e) => reply.error(e),
        }
    }
    fn setattr(
        &self,
        _: &Request,
        ino: INodeNo,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<TimeOrNow>,
        mtime: Option<TimeOrNow>,
        _: Option<SystemTime>,
        fh: Option<FileHandle>,
        _: Option<SystemTime>,
        _: Option<SystemTime>,
        _: Option<SystemTime>,
        flags: Option<BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        let result = (|| {
            if uid.is_some_and(|u| u != self.uid)
                || gid.is_some_and(|g| g != self.gid)
                || flags.is_some_and(|f| !f.is_empty())
            {
                return Err(Errno::EOPNOTSUPP);
            }
            let metadata = FsSetMetadata {
                size,
                accessed: seconds(atime),
                modified: seconds(mtime),
                permissions: mode,
            };
            if let Some(fh) = fh {
                self.call(Operation::SetFileMetadata {
                    handle: fh.0,
                    metadata,
                })?;
            } else {
                self.call(Operation::SetMetadata {
                    path: self.path(ino)?,
                    metadata,
                })?;
            }
            self.metadata(ino, fh)
        })();
        match result {
            Ok(a) => reply.attr(&Duration::ZERO, &a),
            Err(e) => reply.error(e),
        }
    }
    fn open(&self, _: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        match self
            .path(ino)
            .and_then(|p| self.open_file(ino, p, options(flags.0, false)))
        {
            Ok(h) => reply.opened(h, FopenFlags::FOPEN_DIRECT_IO),
            Err(e) => reply.error(e),
        }
    }
    fn create(
        &self,
        _: &Request,
        parent: INodeNo,
        name: &OsStr,
        mode: u32,
        umask: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        let result = (|| {
            let path = self.child(parent, name)?;
            let handle = match self.call(Operation::Open {
                path: path.clone(),
                options: options(flags, true),
            })? {
                Value::Handle(h) => h,
                _ => return Err(Errno::EIO),
            };
            let outcome = (|| {
                self.call(Operation::SetFileMetadata {
                    handle,
                    metadata: FsSetMetadata {
                        permissions: Some(mode & !umask),
                        ..Default::default()
                    },
                })?;
                let attr = self.lookup_path(path)?;
                let mut state = self.state()?;
                state.files.insert(handle, attr.ino.0);
                state.nodes.get_mut(&attr.ino.0).ok_or(Errno::ESTALE)?.opens += 1;
                Ok((attr, FileHandle(handle)))
            })();
            if outcome.is_err() {
                let _ = self.call(Operation::Close { handle });
            }
            outcome
        })();
        match result {
            Ok((a, h)) => reply.created(
                &Duration::ZERO,
                &a,
                Generation(1),
                h,
                FopenFlags::FOPEN_DIRECT_IO,
            ),
            Err(e) => reply.error(e),
        }
    }
    fn read(
        &self,
        _: &Request,
        _: INodeNo,
        fh: FileHandle,
        offset: u64,
        size: u32,
        _: OpenFlags,
        _: Option<LockOwner>,
        reply: ReplyData,
    ) {
        let result = (|| {
            let mut bytes = Vec::with_capacity(size as usize);
            while bytes.len() < size as usize {
                let length = ((size as usize - bytes.len()).min(MOUNT_IO_CHUNK)) as u32;
                match self.call(Operation::Read {
                    handle: fh.0,
                    offset: offset
                        .checked_add(bytes.len() as u64)
                        .ok_or(Errno::EOVERFLOW)?,
                    length,
                })? {
                    Value::Data(part) if part.len() <= length as usize => {
                        if part.is_empty() {
                            break;
                        }
                        bytes.extend(part);
                    }
                    _ => return Err(Errno::EIO),
                }
            }
            Ok(bytes)
        })();
        match result {
            Ok(bytes) => reply.data(&bytes),
            Err(e) => reply.error(e),
        }
    }
    fn write(
        &self,
        _: &Request,
        _: INodeNo,
        fh: FileHandle,
        offset: u64,
        bytes: &[u8],
        _: WriteFlags,
        _: OpenFlags,
        _: Option<LockOwner>,
        reply: ReplyWrite,
    ) {
        let result = (|| {
            let mut done = 0;
            for part in bytes.chunks(MOUNT_IO_CHUNK) {
                self.call(Operation::Write {
                    handle: fh.0,
                    offset: offset.checked_add(done).ok_or(Errno::EOVERFLOW)?,
                    bytes: part.to_vec(),
                })?;
                done += part.len() as u64;
            }
            Ok(done as u32)
        })();
        match result {
            Ok(size) => reply.written(size),
            Err(e) => reply.error(e),
        }
    }
    fn flush(&self, _: &Request, _: INodeNo, fh: FileHandle, _: LockOwner, reply: ReplyEmpty) {
        empty_reply!(reply, self.call(Operation::Flush { handle: fh.0 }));
    }
    fn fsync(&self, _: &Request, _: INodeNo, fh: FileHandle, _: bool, reply: ReplyEmpty) {
        empty_reply!(reply, self.call(Operation::Flush { handle: fh.0 }));
    }
    fn release(
        &self,
        _: &Request,
        ino: INodeNo,
        fh: FileHandle,
        _: OpenFlags,
        _: Option<LockOwner>,
        _: bool,
        reply: ReplyEmpty,
    ) {
        let result = self.call(Operation::Close { handle: fh.0 });
        if let Ok(mut state) = self.state() {
            state.files.remove(&fh.0);
            state.release(ino.0);
        }
        empty_reply!(reply, result);
    }
    fn mkdir(
        &self,
        _: &Request,
        parent: INodeNo,
        name: &OsStr,
        mode: u32,
        umask: u32,
        reply: ReplyEntry,
    ) {
        let result = (|| {
            let path = self.child(parent, name)?;
            self.call(Operation::Mkdir { path: path.clone() })?;
            self.call(Operation::SetMetadata {
                path: path.clone(),
                metadata: FsSetMetadata {
                    permissions: Some(mode & !umask),
                    ..Default::default()
                },
            })?;
            self.lookup_path(path)
        })();
        match result {
            Ok(a) => reply.entry(&Duration::ZERO, &a, Generation(1)),
            Err(e) => reply.error(e),
        }
    }
    fn unlink(&self, _: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        empty_reply!(reply, self.remove(parent, name, false));
    }
    fn rmdir(&self, _: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        empty_reply!(reply, self.remove(parent, name, true));
    }
    fn rename(
        &self,
        _: &Request,
        parent: INodeNo,
        name: &OsStr,
        newparent: INodeNo,
        newname: &OsStr,
        flags: RenameFlags,
        reply: ReplyEmpty,
    ) {
        let result = (|| {
            if flags.bits() & !1 != 0 {
                return Err(Errno::EOPNOTSUPP);
            }
            let from = self.child(parent, name)?;
            let to = self.child(newparent, newname)?;
            self.call(Operation::Rename {
                from: from.clone(),
                to: to.clone(),
                replace: flags.bits() & 1 == 0,
            })?;
            if from == to {
                return Ok(());
            }
            let mut state = self.state()?;
            state.retire_path(&to);
            let moved: Vec<_> = state
                .paths
                .iter()
                .filter(|(p, _)| p.components().starts_with(from.components()))
                .map(|(p, id)| (p.clone(), *id))
                .collect();
            for (old, id) in moved {
                let new = old.components()[from.components().len()..]
                    .iter()
                    .try_fold(to.clone(), |p, n| p.child(n))
                    .map_err(errno)?;
                state.paths.remove(&old);
                state.paths.insert(new.clone(), id);
                state.nodes.get_mut(&id).ok_or(Errno::ESTALE)?.path = Some(new);
            }
            for directory in state.dirs.values_mut() {
                if directory.path.components().starts_with(from.components()) {
                    directory.path = directory.path.components()[from.components().len()..]
                        .iter()
                        .try_fold(to.clone(), |p, n| p.child(n))
                        .map_err(errno)?;
                }
            }
            Ok(())
        })();
        empty_reply!(reply, result);
    }
    fn opendir(&self, _: &Request, ino: INodeNo, _: OpenFlags, reply: ReplyOpen) {
        let result = (|| {
            let path = self.path(ino)?;
            let remote = match self.call(Operation::OpenDirectory { path: path.clone() })? {
                Value::Handle(h) => h,
                _ => return Err(Errno::EIO),
            };
            let mut state = self.state()?;
            state.nodes.get_mut(&ino.0).ok_or(Errno::ESTALE)?.opens += 1;
            state.dirs.insert(
                remote,
                Directory {
                    ino: ino.0,
                    path,
                    remote: Some(remote),
                    offset: 0,
                    pending: VecDeque::new(),
                    eof: false,
                },
            );
            Ok(FileHandle(remote))
        })();
        match result {
            Ok(h) => reply.opened(h, FopenFlags::empty()),
            Err(e) => reply.error(e),
        }
    }
    fn readdir(
        &self,
        _: &Request,
        _: INodeNo,
        fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let result = (|| {
            let mut state = self.state()?;
            let dir = state.dirs.get_mut(&fh.0).ok_or(Errno::EBADF)?;
            dir.prepare(offset, |op| self.call(op))?;
            loop {
                if dir.pending.is_empty() && !dir.eof {
                    match self.call(Operation::ReadDirectory {
                        handle: dir.remote.ok_or(Errno::EBADF)?,
                    })? {
                        Value::Entries(entries) => {
                            dir.eof = entries.is_empty();
                            dir.pending.extend(entries);
                        }
                        _ => return Err(Errno::EIO),
                    }
                }
                let Some(entry) = dir.pending.front() else {
                    break;
                };
                if dir.offset >= offset
                    && reply.add(
                        INodeNo(0),
                        dir.offset + 1,
                        kind(entry.metadata.kind),
                        &entry.name,
                    )
                {
                    break;
                }
                dir.pending.pop_front();
                dir.offset += 1;
            }
            Ok(())
        })();
        empty_reply!(reply, result);
    }
    fn releasedir(&self, _: &Request, _: INodeNo, fh: FileHandle, _: OpenFlags, reply: ReplyEmpty) {
        let result = (|| {
            let dir = self.state()?.dirs.remove(&fh.0).ok_or(Errno::EBADF)?;
            let result = dir.remote.map_or(Ok(Value::Unit), |handle| {
                self.call(Operation::CloseDirectory { handle })
            });
            let mut state = self.state()?;
            state.release(dir.ino);
            result
        })();
        empty_reply!(reply, result);
    }
    fn statfs(&self, _: &Request, ino: INodeNo, reply: ReplyStatfs) {
        match self
            .path(ino)
            .and_then(|path| self.call(Operation::Space { path }))
        {
            Ok(Value::Space(s)) => reply.statfs(
                s.blocks,
                s.blocks_free,
                s.blocks_available,
                s.files,
                s.files_free,
                s.block_size.min(u32::MAX as u64) as u32,
                s.name_max.min(u32::MAX as u64) as u32,
                s.block_size.min(u32::MAX as u64) as u32,
            ),
            Ok(_) => reply.error(Errno::EIO),
            Err(e) => reply.error(e),
        }
    }
}
pub fn run(pipe: Arc<Pipe>, caps: FsCapabilities, target: &OsStr) -> anyhow::Result<()> {
    let target = std::path::Path::new(target);
    anyhow::ensure!(
        target.is_absolute() && target.is_dir() && target.read_dir()?.next().is_none(),
        "Choose an existing empty local directory"
    );
    let mut config = Config::default();
    config.mount_options = vec![
        MountOption::FSName("ShellCanvas".into()),
        MountOption::NoDev,
        MountOption::NoSuid,
        if caps.writable {
            MountOption::RW
        } else {
            MountOption::RO
        },
    ];
    // Default session ACL restricts access to the mounting user. The remote
    // account enforces file permissions; do not enable allow_other implicitly.
    let fs = Fs {
        pipe: pipe.clone(),
        state: Mutex::new(State::new()),
        uid: unsafe { libc::getuid() },
        gid: unsafe { libc::getgid() },
    };
    let session = fuser::spawn_mount(fs, target, &config)?;
    pipe.call(Operation::Report {
        event: BridgeEvent::Ready,
    })?;
    eprintln!("SHELLCANVAS_BRIDGE_READY");
    let failure = loop {
        std::thread::sleep(Duration::from_secs(1));
        if session.guard.is_finished() {
            session.join()?;
            pipe.call(Operation::Report {
                event: BridgeEvent::Detached,
            })?;
            return Ok(());
        }
        match pipe.call(Operation::Poll) {
            Ok(Value::Directive(BridgeDirective::Continue)) => {}
            Ok(Value::Directive(BridgeDirective::Detach)) => match ordinary_unmount(target) {
                Ok(()) => {
                    session.join()?;
                    pipe.call(Operation::Report {
                        event: BridgeEvent::Detached,
                    })?;
                    return Ok(());
                }
                Err(message) => {
                    pipe.call(Operation::Report {
                        event: BridgeEvent::DetachFailed { message },
                    })?;
                }
            },
            Err(error) => break format!("Filesystem connection lost: {error}"),
            Ok(_) => break "Invalid filesystem lifecycle reply".to_owned(),
        }
    };
    drop(session);
    anyhow::bail!(failure)
}

/// Preserve a busy mount. Never pass force or lazy-detach flags.
fn ordinary_unmount(target: &std::path::Path) -> std::result::Result<(), String> {
    use std::process::{Command, Stdio};
    #[cfg(target_os = "linux")]
    let mut command = {
        let helper = [
            "/usr/bin/fusermount3",
            "/bin/fusermount3",
            "/usr/bin/fusermount",
            "/bin/fusermount",
        ]
        .into_iter()
        .find(|path| std::path::Path::new(path).is_file())
        .ok_or("Install your distribution's FUSE mount helper before detaching")?;
        let mut command = Command::new(helper);
        command.args(["-u", "--"]);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = Command::new("/sbin/umount");
    let mut child = command
        .arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Cannot start the system unmount helper: {e}"))?;
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) if status.success() => return Ok(()),
            Some(status) => {
                return Err(format!(
                    "The system did not confirm detach ({status}). Close local files and folders using it, then try again."
                ));
            }
            None if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                return Err("Unmount helper timed out. Check local file use and the mount's status before retrying.".into());
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}
