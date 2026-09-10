// SPDX-License-Identifier: GPL-3.0-only
use super::*;
use crate::windows_attributes::attributes;
use ::windows::Win32::Foundation::*;
use std::{
    collections::VecDeque,
    ffi::{OsStr, c_void},
    sync::Mutex,
    time::Duration,
};
use winfsp::{
    FspError, U16CStr,
    filesystem::{
        DirInfo, DirMarker, FileInfo, FileSecurity, FileSystemContext, OpenFileInfo, VolumeInfo,
        WideNameInfo,
    },
    host::{FileSystemHost, VolumeParams},
};

fn failure(e: FsError) -> FspError {
    match e.kind {
        FsErrorKind::NotFound => STATUS_OBJECT_NAME_NOT_FOUND,
        FsErrorKind::PermissionDenied => STATUS_ACCESS_DENIED,
        FsErrorKind::AlreadyExists => STATUS_OBJECT_NAME_COLLISION,
        FsErrorKind::NotDirectory => STATUS_NOT_A_DIRECTORY,
        FsErrorKind::IsDirectory => STATUS_FILE_IS_A_DIRECTORY,
        FsErrorKind::NotEmpty => STATUS_DIRECTORY_NOT_EMPTY,
        FsErrorKind::InvalidInput => STATUS_OBJECT_NAME_INVALID,
        FsErrorKind::Unsupported => STATUS_NOT_SUPPORTED,
        FsErrorKind::ReadOnly => STATUS_MEDIA_WRITE_PROTECTED,
        FsErrorKind::Offline => STATUS_DEVICE_NOT_CONNECTED,
        FsErrorKind::TimedOut => STATUS_IO_TIMEOUT,
        FsErrorKind::Io => STATUS_IO_DEVICE_ERROR,
    }
    .into()
}
fn invalid() -> FspError {
    STATUS_INVALID_PARAMETER.into()
}
fn lock<T>(value: &Mutex<T>) -> winfsp::Result<std::sync::MutexGuard<'_, T>> {
    value.lock().map_err(|_| STATUS_INTERNAL_ERROR.into())
}
fn path(name: &U16CStr) -> winfsp::Result<MountPath> {
    let name = name.to_string().map_err(|_| invalid())?;
    name.trim_start_matches('\\')
        .split('\\')
        .filter(|s| !s.is_empty())
        .try_fold(MountPath::root(), |path, name| {
            path.child(name).map_err(failure)
        })
}
fn timestamp(seconds: Option<u64>) -> u64 {
    seconds
        .unwrap_or(0)
        .saturating_add(11_644_473_600)
        .saturating_mul(10_000_000)
}
fn unix_timestamp(time: u64) -> Option<u64> {
    (time != 0 && time != u64::MAX).then(|| (time / 10_000_000).saturating_sub(11_644_473_600))
}
fn fill(info: &mut FileInfo, meta: &FsMetadata) {
    *info = FileInfo::default();
    info.file_attributes = attributes(meta);
    info.file_size = meta.size;
    info.allocation_size = meta.size.div_ceil(4096).saturating_mul(4096);
    info.creation_time = timestamp(meta.modified);
    info.last_write_time = timestamp(meta.modified);
    info.change_time = timestamp(meta.modified);
    info.last_access_time = timestamp(meta.accessed);
}
struct Directory {
    handle: Option<u64>,
    pending: VecDeque<FsDirectoryEntry>,
    last: Option<String>,
    eof: bool,
}
pub struct Context {
    _open: crate::mount_gate::OpenGuard,
    path: Arc<Mutex<MountPath>>,
    file: Option<u64>,
    directory: Mutex<Directory>,
    is_dir: bool,
    tracked: Arc<crate::windows_handles::OpenHandle>,
    pipe: Arc<Pipe>,
}
struct Fs {
    handles: crate::windows_handles::OpenHandles,
    gate: Arc<crate::mount_gate::MountGate>,
    pipe: Arc<Pipe>,
    caps: FsCapabilities,
}
impl Drop for Context {
    fn drop(&mut self) {
        // Also runs when Create/Open acquired a remote handle but a later
        // metadata or bookkeeping step failed before WinFsp accepted Context.
        let mut file = self.tracked.file.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(handle) = file.take()
            && let Err(error) = self.pipe.call(Operation::Close { handle })
        {
            let message = format!("Remote file close was not confirmed: {error}");
            eprintln!("{message}");
            let _ = self.pipe.call(Operation::Report {
                event: BridgeEvent::Warning { message },
            });
        }
        drop(file);
        let directory = self.directory.get_mut().unwrap_or_else(|e| e.into_inner());
        if let Some(handle) = directory.handle.take()
            && let Err(error) = self.pipe.call(Operation::CloseDirectory { handle })
        {
            let message = format!("Remote directory close was not confirmed: {error}");
            eprintln!("{message}");
            let _ = self.pipe.call(Operation::Report {
                event: BridgeEvent::Warning { message },
            });
        }
    }
}
impl Fs {
    fn warn(&self, message: String) {
        eprintln!("{message}");
        let _ = self.pipe.call(Operation::Report {
            event: BridgeEvent::Warning { message },
        });
    }
    fn call(&self, operation: Operation) -> winfsp::Result<Value> {
        self.pipe.call(operation).map_err(failure)
    }
    fn metadata(&self, context: &Context) -> winfsp::Result<FsMetadata> {
        let value = if let Some(handle) = context.file {
            self.call(Operation::FileMetadata { handle })?
        } else {
            self.call(Operation::Metadata {
                path: lock(&context.path)?.clone(),
            })?
        };
        match value {
            Value::Metadata(m) => Ok(m),
            _ => Err(invalid()),
        }
    }
    fn path_metadata(&self, path: MountPath) -> winfsp::Result<FsMetadata> {
        match self.call(Operation::Metadata { path })? {
            Value::Metadata(m) => Ok(m),
            _ => Err(invalid()),
        }
    }
    fn open_context(
        &self,
        path: MountPath,
        access: u32,
        create: FsCreate,
        is_dir: bool,
        open: crate::mount_gate::OpenGuard,
    ) -> winfsp::Result<Context> {
        let file = if is_dir {
            None
        } else {
            // Attribute-only handles need a remote read handle for fstat and
            // stable object identity; the account still controls access.
            let requested_write = access & (0x2 | 0x4) != 0;
            // Creating an empty file requires write on the SFTP handle even if
            // the caller requested only read/attribute access. WinFsp enforces
            // the caller's GrantedAccess; the root must still permit writes.
            let write = requested_write || create != FsCreate::OpenExisting;
            let read = access & 0x1 != 0 || !requested_write;
            match self.call(Operation::Open {
                path: path.clone(),
                options: FsOpenOptions {
                    read,
                    write,
                    create,
                    truncate: false,
                },
            })? {
                Value::Handle(id) => Some(id),
                _ => return Err(invalid()),
            }
        };
        let path = Arc::new(Mutex::new(path));
        let tracked = Arc::new(crate::windows_handles::OpenHandle {
            path: path.clone(),
            file: Mutex::new(file),
            writable: access & (0x2 | 0x4) != 0 || create != FsCreate::OpenExisting,
        });
        let context = Context {
            _open: open,
            path,
            file,
            directory: Mutex::new(Directory {
                handle: None,
                pending: VecDeque::new(),
                last: None,
                eof: false,
            }),
            is_dir,
            tracked,
            pipe: self.pipe.clone(),
        };
        self.handles.register(&context.tracked).map_err(failure)?;
        Ok(context)
    }
}
impl FileSystemContext for Fs {
    type FileContext = Context;
    fn get_security_by_name(
        &self,
        name: &U16CStr,
        _: Option<&mut [c_void]>,
        _: impl FnOnce(&U16CStr) -> Option<FileSecurity>,
    ) -> winfsp::Result<FileSecurity> {
        let m = self.path_metadata(path(name)?)?;
        Ok(FileSecurity {
            reparse: false,
            sz_security_descriptor: 0,
            attributes: attributes(&m),
        })
    }
    fn open(
        &self,
        name: &U16CStr,
        options: u32,
        access: u32,
        info: &mut OpenFileInfo,
    ) -> winfsp::Result<Context> {
        let open = self
            .gate
            .enter()
            .map_err(|_| FspError::from(STATUS_DEVICE_NOT_CONNECTED))?;
        let path = path(name)?;
        let m = self.path_metadata(path.clone())?;
        let is_dir = m.kind == FsKind::Directory;
        if options & 1 != 0 && !is_dir {
            return Err(STATUS_NOT_A_DIRECTORY.into());
        }
        if options & 0x40 != 0 && is_dir {
            return Err(STATUS_FILE_IS_A_DIRECTORY.into());
        }
        let context = self.open_context(path, access, FsCreate::OpenExisting, is_dir, open)?;
        fill(info.as_mut(), &m);
        Ok(context)
    }
    fn create(
        &self,
        name: &U16CStr,
        options: u32,
        access: u32,
        _: u32,
        _: Option<&[c_void]>,
        _: u64,
        _: Option<&[u8]>,
        _: bool,
        info: &mut OpenFileInfo,
    ) -> winfsp::Result<Context> {
        if !self.caps.writable {
            return Err(STATUS_MEDIA_WRITE_PROTECTED.into());
        }
        let open = self
            .gate
            .enter()
            .map_err(|_| FspError::from(STATUS_DEVICE_NOT_CONNECTED))?;
        let path = path(name)?;
        let is_dir = options & 1 != 0;
        if is_dir {
            self.call(Operation::Mkdir { path: path.clone() })?;
        }
        let context = self.open_context(path, access, FsCreate::CreateNew, is_dir, open)?;
        fill(info.as_mut(), &self.metadata(&context)?);
        Ok(context)
    }
    fn close(&self, context: Context) {
        drop(context);
    }
    fn get_file_info(&self, context: &Context, info: &mut FileInfo) -> winfsp::Result<()> {
        fill(info, &self.metadata(context)?);
        Ok(())
    }
    fn read(&self, context: &Context, buffer: &mut [u8], offset: u64) -> winfsp::Result<u32> {
        let handle = context.file.ok_or_else(invalid)?;
        let mut done = 0;
        while done < buffer.len() {
            let length = (buffer.len() - done).min(MOUNT_IO_CHUNK) as u32;
            let at = offset.checked_add(done as u64).ok_or_else(invalid)?;
            let data = match self.call(Operation::Read {
                handle,
                offset: at,
                length,
            })? {
                Value::Data(d) if d.len() <= length as usize => d,
                _ => return Err(invalid()),
            };
            if data.is_empty() {
                break;
            }
            buffer[done..done + data.len()].copy_from_slice(&data);
            done += data.len();
        }
        Ok(done as u32)
    }
    fn write(
        &self,
        context: &Context,
        buffer: &[u8],
        mut offset: u64,
        eof: bool,
        constrained: bool,
        info: &mut FileInfo,
    ) -> winfsp::Result<u32> {
        let handle = context.file.ok_or_else(invalid)?;
        let size = self.metadata(context)?.size;
        if eof {
            offset = size;
        }
        let length = if constrained {
            buffer.len().min(size.saturating_sub(offset) as usize)
        } else {
            buffer.len()
        };
        let mut done = 0;
        for bytes in buffer[..length].chunks(MOUNT_IO_CHUNK) {
            self.call(Operation::Write {
                handle,
                offset: offset.checked_add(done).ok_or_else(invalid)?,
                bytes: bytes.to_vec(),
            })?;
            done += bytes.len() as u64;
        }
        fill(info, &self.metadata(context)?);
        Ok(done as u32)
    }
    fn flush(&self, context: Option<&Context>, info: &mut FileInfo) -> winfsp::Result<()> {
        if let Some(context) = context {
            if let Some(handle) = context.file {
                self.call(Operation::Flush { handle })?;
            }
            fill(info, &self.metadata(context)?);
        } else {
            self.handles
                .flush(|handle| self.pipe.call(Operation::Flush { handle }).map(|_| ()))
                .map_err(failure)?;
        }
        Ok(())
    }
    fn set_file_size(
        &self,
        context: &Context,
        size: u64,
        allocation: bool,
        info: &mut FileInfo,
    ) -> winfsp::Result<()> {
        let handle = context.file.ok_or_else(invalid)?;
        if !allocation || size < self.metadata(context)?.size {
            self.call(Operation::SetFileMetadata {
                handle,
                metadata: FsSetMetadata {
                    size: Some(size),
                    ..Default::default()
                },
            })?;
        }
        fill(info, &self.metadata(context)?);
        Ok(())
    }
    fn overwrite(
        &self,
        context: &Context,
        _: u32,
        _: bool,
        _: u64,
        _: Option<&[u8]>,
        info: &mut FileInfo,
    ) -> winfsp::Result<()> {
        self.set_file_size(context, 0, false, info)
    }
    fn set_basic_info(
        &self,
        context: &Context,
        requested_attributes: u32,
        _: u64,
        access: u64,
        write: u64,
        _: u64,
        info: &mut FileInfo,
    ) -> winfsp::Result<()> {
        let metadata = FsSetMetadata {
            accessed: unix_timestamp(access),
            modified: unix_timestamp(write),
            permissions: crate::windows_attributes::permissions(&self.metadata(context)?, requested_attributes)
                .map_err(failure)?,
            ..Default::default()
        };
        if metadata.accessed.is_some() || metadata.modified.is_some() || metadata.permissions.is_some() {
            if !self.caps.writable {
                return Err(STATUS_MEDIA_WRITE_PROTECTED.into());
            }
            if let Some(handle) = context.file {
                self.call(Operation::SetFileMetadata { handle, metadata })?;
            } else {
                self.call(Operation::SetMetadata {
                    path: lock(&context.path)?.clone(),
                    metadata,
                })?;
            }
        }
        fill(info, &self.metadata(context)?);
        Ok(())
    }
    fn rename(
        &self,
        _context: &Context,
        old: &U16CStr,
        new: &U16CStr,
        replace: bool,
    ) -> winfsp::Result<()> {
        let to = path(new)?;
        let from = path(old)?;
        self.handles
            .rename(&from, &to, || {
                self.pipe
                    .call(Operation::Rename {
                        from: from.clone(),
                        to: to.clone(),
                        replace,
                    })
                    .map(|_| ())
            })
            .map_err(failure)
    }
    fn set_delete(&self, context: &Context, _: &U16CStr, delete: bool) -> winfsp::Result<()> {
        if delete {
            if !self.caps.writable {
                return Err(STATUS_MEDIA_WRITE_PROTECTED.into());
            }
            if lock(&context.path)?.components().is_empty() {
                return Err(STATUS_ACCESS_DENIED.into());
            }
            if context.is_dir {
                let handle = match self.call(Operation::OpenDirectory {
                    path: lock(&context.path)?.clone(),
                })? {
                    Value::Handle(id) => id,
                    _ => return Err(invalid()),
                };
                let entries = self.call(Operation::ReadDirectory { handle });
                let _ = self.call(Operation::CloseDirectory { handle });
                if !matches!(entries?, Value::Entries(ref entries) if entries.is_empty()) {
                    return Err(STATUS_DIRECTORY_NOT_EMPTY.into());
                }
            }
        }
        Ok(())
    }
    fn cleanup(&self, context: &Context, name: Option<&U16CStr>, flags: u32) {
        if flags & 1 != 0 {
            let target = name
                .map(path)
                .unwrap_or_else(|| lock(&context.path).map(|p| p.clone()));
            if let Ok(path) = target
                && let Err(e) = self.call(Operation::Remove {
                    path,
                    directory: context.is_dir,
                })
            {
                self.warn(format!(
                    "Remote deletion failed during Windows cleanup: {e}"
                ));
            }
        }
    }
    fn read_directory(
        &self,
        context: &Context,
        _: Option<&U16CStr>,
        marker: DirMarker,
        buffer: &mut [u8],
    ) -> winfsp::Result<u32> {
        let marker = marker
            .inner_as_cstr()
            .map(|s| s.to_string())
            .transpose()
            .map_err(|_| invalid())?;
        let mut dir = lock(&context.directory)?;
        if dir.handle.is_none() || marker != dir.last {
            if let Some(handle) = dir.handle.take() {
                self.call(Operation::CloseDirectory { handle })?;
            }
            dir.handle = Some(
                match self.call(Operation::OpenDirectory {
                    path: lock(&context.path)?.clone(),
                })? {
                    Value::Handle(id) => id,
                    _ => return Err(invalid()),
                },
            );
            dir.pending.clear();
            dir.eof = false;
            dir.last = None;
        }
        let mut seeking = marker.is_some() && marker != dir.last;
        let mut cursor = 0;
        loop {
            if dir.pending.is_empty() && !dir.eof {
                match self.call(Operation::ReadDirectory {
                    handle: dir.handle.ok_or_else(invalid)?,
                })? {
                    Value::Entries(entries) => {
                        dir.eof = entries.is_empty();
                        dir.pending.extend(entries);
                    }
                    _ => return Err(invalid()),
                }
            }
            let Some(entry) = dir.pending.front() else {
                DirInfo::<255>::finalize_buffer(buffer, &mut cursor);
                break;
            };
            if seeking {
                if Some(&entry.name) == marker.as_ref() {
                    seeking = false;
                }
                dir.pending.pop_front();
                continue;
            }
            let mut info = DirInfo::<255>::new();
            fill(info.file_info_mut(), &entry.metadata);
            info.set_name(&entry.name)?;
            if !info.append_to_buffer(buffer, &mut cursor) {
                break;
            }
            dir.last = Some(entry.name.clone());
            dir.pending.pop_front();
        }
        Ok(cursor)
    }
    fn get_volume_info(&self, info: &mut VolumeInfo) -> winfsp::Result<()> {
        let stats = match self.call(Operation::Space {
            path: MountPath::root(),
        })? {
            Value::Space(stats) => stats,
            _ => return Err(invalid()),
        };
        info.total_size = stats.blocks.saturating_mul(stats.block_size);
        info.free_size = stats.blocks_available.saturating_mul(stats.block_size);
        info.set_volume_label("ShellCanvas");
        Ok(())
    }
}
pub fn run(pipe: Arc<Pipe>, caps: FsCapabilities, target: &OsStr) -> anyhow::Result<()> {
    // Build against the bundled SDK; load only the separately installed runtime
    // from the machine registry. Never search a writable current directory.
    let setup = |error| anyhow::anyhow!("WinFsp is unavailable. Install or repair the WinFsp runtime from https://winfsp.dev/rel/, then try attaching again. Driver setup requires administrator approval. Details: {error}");
    let installation = windows_registry::LOCAL_MACHINE
        .open("SOFTWARE\\WOW6432Node\\WinFsp")
        .or_else(|_| windows_registry::LOCAL_MACHINE.open("SOFTWARE\\WinFsp"))
        .map_err(&setup)?
        .get_string("InstallDir").map_err(&setup)?;
    let dll = if cfg!(target_arch = "aarch64") {
        "winfsp-a64.dll"
    } else {
        "winfsp-x64.dll"
    };
    let dll = std::path::Path::new(&installation).join("bin").join(dll);
    anyhow::ensure!(dll.is_absolute(), "Invalid WinFsp installation directory");
    let name = ::windows::core::HSTRING::from(dll.as_os_str());
    unsafe {
        ::windows::Win32::System::LibraryLoader::LoadLibraryExW(
            &name,
            None,
            ::windows::Win32::System::LibraryLoader::LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR
                | ::windows::Win32::System::LibraryLoader::LOAD_LIBRARY_SEARCH_SYSTEM32,
        ).map_err(|error| anyhow::anyhow!("Cannot load the installed WinFsp runtime at {}. Install or repair WinFsp for this computer from https://winfsp.dev/rel/. Details: {error}", dll.display()))?;
    }
    let _runtime = winfsp::winfsp_init().map_err(|e| {
        anyhow::anyhow!(
            "Install WinFsp from https://winfsp.dev/rel/ before attaching a drive: {e:?}"
        )
    })?;
    let mut params = VolumeParams::new();
    params
        .filesystem_name("ShellCanvas")
        .sector_size(512)
        .sectors_per_allocation_unit(8)
        .max_component_length(255)
        .case_sensitive_search(true)
        .case_preserved_names(true)
        .unicode_on_disk(true)
        .persistent_acls(false)
        .read_only_volume(!caps.writable)
        .file_info_timeout(0)
        .flush_and_purge_on_cleanup(true)
        .irp_timeout(60000);
    let gate = Arc::new(crate::mount_gate::MountGate::default());
    let mut host = FileSystemHost::<_, winfsp::host::CoarseGuard>::new(
        params,
        Fs {
            handles: crate::windows_handles::OpenHandles::default(),
            gate: gate.clone(),
            pipe: pipe.clone(),
            caps,
        },
    )?;
    host.mount(target)?;
    host.start()?;
    pipe.call(Operation::Report {
        event: BridgeEvent::Ready,
    })?;
    eprintln!("SHELLCANVAS_BRIDGE_READY");
    let failure = loop {
        std::thread::sleep(Duration::from_secs(1));
        match pipe.call(Operation::Poll) {
            Ok(Value::Directive(BridgeDirective::Continue)) => {}
            Ok(Value::Directive(BridgeDirective::Detach)) => match gate.begin_detach() {
                Ok(()) => {
                    host.unmount();
                    host.stop();
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
    host.unmount();
    host.stop();
    anyhow::bail!(failure)
}
