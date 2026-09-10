// SPDX-License-Identifier: GPL-3.0-only
//! Real WinFsp callbacks against a disposable local provider. No SSH credentials
//! or normal ShellCanvas profile; this tests the bridge, not SFTP or the desktop UI.
#![cfg(windows)]
use anyhow::{Context, Result, ensure};
use async_trait::async_trait;
use shellcanvas_filesystem_sdk::{
    bridge_control::{BridgeControl, BridgePhase},
    wire::Server,
    *,
};
use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, UNIX_EPOCH},
};

fn error(e: std::io::Error) -> FsError {
    use std::io::ErrorKind::*;
    let kind = match e.kind() {
        NotFound => FsErrorKind::NotFound,
        PermissionDenied => FsErrorKind::PermissionDenied,
        AlreadyExists => FsErrorKind::AlreadyExists,
        NotADirectory => FsErrorKind::NotDirectory,
        IsADirectory => FsErrorKind::IsDirectory,
        DirectoryNotEmpty => FsErrorKind::NotEmpty,
        InvalidInput => FsErrorKind::InvalidInput,
        _ => FsErrorKind::Io,
    };
    FsError::new(kind, e.to_string())
}
fn metadata(m: fs::Metadata) -> FsMetadata {
    FsMetadata {
        kind: if m.is_dir() {
            FsKind::Directory
        } else {
            FsKind::File
        },
        size: m.len(),
        accessed: m
            .accessed()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs()),
        modified: m
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs()),
        permissions: None,
    }
}
fn set(file: &fs::File, m: FsSetMetadata) -> std::io::Result<()> {
    if let Some(size) = m.size {
        file.set_len(size)?;
    }
    if m.accessed.is_some() || m.modified.is_some() {
        let mut times = fs::FileTimes::new();
        if let Some(t) = m.accessed {
            times = times.set_accessed(UNIX_EPOCH + Duration::from_secs(t));
        }
        if let Some(t) = m.modified {
            times = times.set_modified(UNIX_EPOCH + Duration::from_secs(t));
        }
        file.set_times(times)?;
    }
    Ok(())
}
struct LocalFile(Mutex<Option<fs::File>>, String);
impl LocalFile {
    fn with<T>(&self, op: impl FnOnce(&mut fs::File) -> std::io::Result<T>) -> FsResult<T> {
        let mut guard = self.0.lock().unwrap();
        let file = guard
            .as_mut()
            .ok_or_else(|| FsError::new(FsErrorKind::Offline, "Fixture handle closed"))?;
        op(file).map_err(error)
    }
}
#[async_trait]
impl MountedFile for LocalFile {
    async fn metadata(&self) -> FsResult<FsMetadata> {
        self.with(|f| f.metadata().map(metadata))
    }
    async fn read_at(&self, offset: u64, length: u32) -> FsResult<Vec<u8>> {
        self.with(|f| {
            f.seek(SeekFrom::Start(offset))?;
            let mut data = vec![0; length as usize];
            let n = f.read(&mut data)?;
            data.truncate(n);
            Ok(data)
        })
    }
    async fn write_at(&self, offset: u64, bytes: &[u8]) -> FsResult<()> {
        let fault = match self.1.as_str() {
            "write-failed.bin" => Some(FsErrorKind::Io),
            "write-offline.bin" => Some(FsErrorKind::Offline),
            "write-timeout.bin" => Some(FsErrorKind::TimedOut),
            "write-readonly.bin" => Some(FsErrorKind::ReadOnly),
            _ => None,
        };
        if let Some(kind) = fault {
            return Err(FsError::new(kind, "Injected write failure"));
        }
        self.with(|f| {
            f.seek(SeekFrom::Start(offset))?;
            f.write_all(bytes)
        })
    }
    async fn set_metadata(&self, m: FsSetMetadata) -> FsResult<()> {
        self.with(|f| set(f, m))
    }
    async fn flush(&self) -> FsResult<()> {
        if self.1 == "flush-failed.bin" {
            return Err(FsError::new(FsErrorKind::Io, "Injected flush failure"));
        }
        self.with(|f| f.sync_all())
    }
    async fn close(&self) -> FsResult<()> {
        self.0.lock().unwrap().take();
        if self.1 == "close-failed.bin" {
            return Err(FsError::new(FsErrorKind::Io, "Injected close failure"));
        }
        Ok(())
    }
}
struct LocalDirectory(Option<fs::ReadDir>);
#[async_trait]
impl MountedDirectory for LocalDirectory {
    async fn next(&mut self) -> FsResult<Vec<FsDirectoryEntry>> {
        self.0
            .as_mut()
            .ok_or_else(|| FsError::new(FsErrorKind::Offline, "Fixture directory closed"))?
            .take(7)
            .map(|entry| {
                let entry = entry.map_err(error)?;
                Ok(FsDirectoryEntry {
                    name: entry.file_name().to_string_lossy().into_owned(),
                    metadata: metadata(entry.metadata().map_err(error)?),
                })
            })
            .collect()
    }
    async fn close(&mut self) -> FsResult<()> {
        self.0.take();
        Ok(())
    }
}
struct LocalRoot(PathBuf);
impl LocalRoot {
    fn path(&self, path: &MountPath) -> PathBuf {
        path.components()
            .iter()
            .fold(self.0.clone(), |p, part| p.join(part))
    }
}
#[async_trait]
impl MountedFileSystem for LocalRoot {
    fn capabilities(&self) -> FsCapabilities {
        FsCapabilities {
            writable: true,
            atomic_replace: true,
            durable_flush: true,
        }
    }
    async fn space(&self, _: &MountPath) -> FsResult<FsSpace> {
        Ok(FsSpace {
            block_size: 4096,
            blocks: 1_000_000,
            blocks_free: 800_000,
            blocks_available: 750_000,
            files: 0,
            files_free: 0,
            name_max: 255,
        })
    }
    async fn metadata(&self, path: &MountPath) -> FsResult<FsMetadata> {
        fs::metadata(self.path(path)).map(metadata).map_err(error)
    }
    async fn open(
        &self,
        path: &MountPath,
        options: FsOpenOptions,
    ) -> FsResult<Arc<dyn MountedFile>> {
        options.validate()?;
        if path.components().last().is_some_and(|p| p == "denied.txt") {
            return Err(FsError::new(
                FsErrorKind::PermissionDenied,
                "Fixture denies this file",
            ));
        }
        let mut file = fs::OpenOptions::new();
        file.read(options.read)
            .write(options.write)
            .truncate(options.truncate);
        match options.create {
            FsCreate::OpenExisting => {}
            FsCreate::CreateNew => {
                file.create_new(true);
            }
            FsCreate::OpenOrCreate => {
                file.create(true);
            }
        }
        Ok(Arc::new(LocalFile(
            Mutex::new(Some(file.open(self.path(path)).map_err(error)?)),
            path.components().last().cloned().unwrap_or_default(),
        )))
    }
    async fn open_directory(&self, path: &MountPath) -> FsResult<Box<dyn MountedDirectory>> {
        Ok(Box::new(LocalDirectory(Some(
            fs::read_dir(self.path(path)).map_err(error)?,
        ))))
    }
    async fn set_metadata(&self, path: &MountPath, m: FsSetMetadata) -> FsResult<()> {
        let file = fs::OpenOptions::new()
            .write(true)
            .open(self.path(path))
            .map_err(error)?;
        set(&file, m).map_err(error)
    }
    async fn mkdir(&self, path: &MountPath) -> FsResult<()> {
        fs::create_dir(self.path(path)).map_err(error)
    }
    async fn remove(&self, path: &MountPath, directory: bool) -> FsResult<()> {
        if path.components().last().is_some_and(|p| p == "delete-failed.bin") {
            return Err(FsError::new(FsErrorKind::PermissionDenied, "Injected cleanup deletion failure"));
        }
        if directory {
            fs::remove_dir(self.path(path))
        } else {
            fs::remove_file(self.path(path))
        }
        .map_err(error)
    }
    async fn rename(&self, from: &MountPath, to: &MountPath, replace: bool) -> FsResult<()> {
        let to = self.path(to);
        if !replace && to.exists() {
            return Err(FsError::new(
                FsErrorKind::AlreadyExists,
                "Fixture destination exists",
            ));
        }
        fs::rename(self.path(from), to).map_err(error)
    }
}

async fn phase(control: &BridgeControl, expected: BridgePhase) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let state = control.snapshot()?;
            if state.phase == expected {
                return Ok(());
            }
            ensure!(
                state.phase != BridgePhase::Failed,
                "Bridge failed: {:?}",
                state.message
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("Bridge lifecycle deadline")?
}
async fn warning(control: &BridgeControl, fragment: &str) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if control.snapshot()?.message.as_deref().is_some_and(|m| m.contains(fragment)) {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.context("Expected cleanup warning was not delivered")?
}

fn failed_writes(target: &Path, source: &Path) -> Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::{Foundation::*, Storage::FileSystem::FILE_FLAG_WRITE_THROUGH};
    for (name, code) in [
        ("write-failed.bin", ERROR_IO_DEVICE),
        ("write-offline.bin", ERROR_DEVICE_NOT_CONNECTED),
        ("write-timeout.bin", ERROR_SEM_TIMEOUT),
        ("write-readonly.bin", ERROR_WRITE_PROTECT),
    ] {
        let mut file = fs::OpenOptions::new().write(true)
            .custom_flags(FILE_FLAG_WRITE_THROUGH.0).open(target.join(name))?;
        // Write-through makes success depend on the provider, not just dirtying
        // a Windows cache page. Never replay the failed mutation automatically.
        let error = file.write_all(b"must not be acknowledged").expect_err("Failed write reported success");
        ensure!(error.raw_os_error() == Some(code.0 as i32), "{name}: wrong native status: {error}");
        drop(file);
        ensure!(fs::read(source.join(name))? == b"original", "{name}: failed write changed source");
    }
    let file = fs::OpenOptions::new().write(true).open(target.join("flush-failed.bin"))?;
    let error = file.sync_all().expect_err("Failed durable flush reported success");
    ensure!(error.raw_os_error() == Some(ERROR_IO_DEVICE.0 as i32), "Wrong flush status: {error}");
    drop(file);
    // Per-operation failure must not poison unrelated handles or the attachment.
    fs::write(target.join("healthy-after-error.bin"), b"healthy")?;
    ensure!(fs::read(source.join("healthy-after-error.bin"))? == b"healthy", "Attachment did not recover from operation failure");
    println!("NATIVE_WINDOWS_FAILURE_PASS: failed/offline/timed-out/read-only writes and flush errors reach Windows; source preserved; unrelated I/O remains usable");
    Ok(())
}
struct MappedView {
    mapping: windows::Win32::Foundation::HANDLE,
    view: windows::Win32::System::Memory::MEMORY_MAPPED_VIEW_ADDRESS,
}
impl MappedView {
    fn new(
        file: &fs::File,
        protect: windows::Win32::System::Memory::PAGE_PROTECTION_FLAGS,
        access: windows::Win32::System::Memory::FILE_MAP,
    ) -> Result<Self> {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::{
            Foundation::{CloseHandle, HANDLE},
            System::Memory::*,
        };
        let mapping =
            unsafe { CreateFileMappingW(HANDLE(file.as_raw_handle()), None, protect, 0, 0, None)? };
        let view = unsafe { MapViewOfFile(mapping, access, 0, 0, 8192) };
        if view.Value.is_null() {
            let error = std::io::Error::last_os_error();
            unsafe {
                CloseHandle(mapping)?;
            }
            return Err(error.into());
        }
        Ok(Self { mapping, view })
    }
}
impl Drop for MappedView {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::System::Memory::UnmapViewOfFile(self.view);
            let _ = windows::Win32::Foundation::CloseHandle(self.mapping);
        }
    }
}
fn mapped_files(target: &Path, source: &Path) -> Result<()> {
    use windows::Win32::System::Memory::*;
    let path = target.join("mapped.bin");
    fs::write(&path, vec![0u8; 8192])?;
    let file = fs::OpenOptions::new().read(true).write(true).open(&path)?;
    let shared = MappedView::new(&file, PAGE_READWRITE, FILE_MAP_WRITE)?;
    drop(file); // Mapping must keep its file object alive without this descriptor.
    unsafe {
        let bytes = std::slice::from_raw_parts_mut(shared.view.Value.cast::<u8>(), 8192);
        bytes[4093..4103].copy_from_slice(b"cross-page");
        FlushViewOfFile(shared.view.Value, 8192)?;
    }
    drop(shared);
    // FlushViewOfFile flushes dirty pages; FlushFileBuffers requests durable file flush.
    fs::OpenOptions::new().write(true).open(&path)?.sync_all()?;
    let expected = fs::read(source.join("mapped.bin"))?;
    ensure!(
        &expected[4093..4103] == b"cross-page",
        "Mapped write did not reach provider"
    );
    let file = fs::File::open(&path)?;
    let readonly = MappedView::new(&file, PAGE_READONLY, FILE_MAP_READ)?;
    unsafe {
        ensure!(
            std::slice::from_raw_parts(readonly.view.Value.cast::<u8>(), 8192)
                == expected.as_slice(),
            "Read-only mapping mismatch"
        );
    }
    drop(readonly);
    let private = MappedView::new(&file, PAGE_WRITECOPY, FILE_MAP_COPY)?;
    unsafe {
        std::slice::from_raw_parts_mut(private.view.Value.cast::<u8>(), 8192)[..7]
            .copy_from_slice(b"private");
    }
    drop(private);
    drop(file);
    ensure!(
        fs::read(source.join("mapped.bin"))? == expected,
        "Private mapping changed source"
    );
    println!(
        "NATIVE_WINDOWS_MMAP_PASS: shared cross-page flush, descriptor-close lifetime, read-only and private copy-on-write mappings"
    );
    Ok(())
}
fn exercise(target: &Path, source: &Path) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(target.join("seek.bin"))?;
    file.write_all(b"begin")?;
    file.seek(SeekFrom::Start((1 << 32) + 19))?;
    file.write_all(b"end")?;
    file.sync_all()?;
    ensure!(
        file.metadata()?.len() == (1 << 32) + 22,
        "Sparse size mismatch"
    );
    file.set_len(5)?;
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    ensure!(bytes == b"begin", "Truncate/read mismatch");
    drop(file);
    fs::write(target.join("save.tmp"), b"replacement")?;
    fs::rename(target.join("save.tmp"), target.join("seek.bin"))?;
    ensure!(
        fs::read(source.join("seek.bin"))? == b"replacement",
        "Replace did not reach provider"
    );
    fs::create_dir(target.join("directory"))?;
    for i in 0..70 {
        fs::write(target.join("directory").join(format!("item-{i:03}")), [i])?;
    }
    for _ in 0..2 {
        let actual = fs::read_dir(target.join("directory"))?
            .map(|entry| entry.map(|e| e.file_name().to_string_lossy().into_owned()))
            .collect::<std::io::Result<std::collections::BTreeSet<_>>>()?;
        let expected = (0..70).map(|i| format!("item-{i:03}")).collect();
        ensure!(
            actual == expected,
            "Directory paging lost or duplicated names"
        );
    }
    fs::rename(target.join("directory"), target.join("renamed"))?;
    ensure!(
        fs::read(target.join("renamed/item-000"))? == [0],
        "Directory rename failed"
    );
    ensure!(
        fs::remove_dir(target.join("renamed")).is_err(),
        "Nonempty directory removed"
    );
    let missing = fs::read(target.join("missing")).unwrap_err();
    ensure!(
        missing.kind() == std::io::ErrorKind::NotFound,
        "Missing-file status mismatch: {missing}"
    );
    let denied = fs::File::open(target.join("denied.txt")).unwrap_err();
    ensure!(
        denied.kind() == std::io::ErrorKind::PermissionDenied,
        "Permission status mismatch: {denied}"
    );
    fs::remove_file(target.join("renamed/item-000"))?;
    ensure!(
        !source.join("renamed/item-000").exists(),
        "Delete did not reach provider"
    );
    println!(
        "NATIVE_WINDOWS_IO_PASS: create, offsets above 4 GiB, flush, truncate, replacement save, directory paging/rename, deletion and errors"
    );
    let mut free = 0;
    let mut total = 0;
    unsafe {
        windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
            &windows::core::HSTRING::from(target.as_os_str()),
            Some(&mut free),
            Some(&mut total),
            None,
        )?;
    }
    ensure!(
        total == 4096 * 1_000_000 && free == 4096 * 750_000,
        "Capacity does not match provider values"
    );
    println!("NATIVE_WINDOWS_CAPACITY_PASS: Windows API reports provider capacity");
    mapped_files(target, source)?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "Requires installed WinFsp and SHELLCANVAS_NATIVE_WINDOWS_TEST=1. Uses an unused drive letter and disposable backing files."]
async fn native_windows_mount() -> Result<()> {
    ensure!(
        std::env::var("SHELLCANVAS_NATIVE_WINDOWS_TEST").as_deref() == Ok("1"),
        "Explicit native-test opt-in required"
    );
    let drives = unsafe { windows::Win32::Storage::FileSystem::GetLogicalDrives() };
    ensure!(drives != 0, "Cannot enumerate local drives");
    let letter = (b'D'..=b'Z')
        .rev()
        .find(|c| drives & (1 << (c - b'A')) == 0)
        .context("No free test drive")?;
    let mount = format!("{}:", letter as char);
    let target = PathBuf::from(format!("{mount}\\"));
    let backing = tempfile::tempdir()?;
    fs::write(backing.path().join("denied.txt"), b"fixture")?;
    for name in ["write-failed.bin", "write-offline.bin", "write-timeout.bin", "write-readonly.bin", "flush-failed.bin", "close-failed.bin", "delete-failed.bin"] {
        fs::write(backing.path().join(name), b"original")?;
    }
    let control = Arc::new(BridgeControl::default());
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_shellcanvas-drive-bridge"))
        .args(["--mount", &mount])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()?;
    let serving = tokio::spawn(
        Server::with_control(
            Arc::new(LocalRoot(backing.path().to_owned())),
            control.clone(),
        )
        .serve(child.stdout.take().unwrap(), child.stdin.take().unwrap()),
    );
    let result: Result<()> = async {
        phase(&control, BridgePhase::Attached).await?;
        let p = target.clone(); let source = backing.path().to_owned();
        tokio::task::spawn_blocking(move || exercise(&p, &source)).await??;
        let p = target.clone(); let source = backing.path().to_owned();
        tokio::task::spawn_blocking(move || failed_writes(&p, &source)).await??;
        drop(fs::File::open(target.join("close-failed.bin"))?);
        warning(&control, "Remote file close was not confirmed").await?;
        // Windows cleanup has no error-return channel. The failure must reach
        // the parent as a warning and leave the source file intact.
        let _ = fs::remove_file(target.join("delete-failed.bin"));
        warning(&control, "Remote deletion failed during Windows cleanup").await?;
        ensure!(backing.path().join("delete-failed.bin").exists(), "Failed cleanup deletion lost source");
        println!("NATIVE_WINDOWS_WARNING_PASS: close and cleanup deletion failures delivered to the parent");
        let held = fs::File::open(target.join("seek.bin"))?;
        control.request_detach()?;
        phase(&control, BridgePhase::Attached).await?;
        ensure!(control.snapshot()?.message.is_some(), "Busy detach had no explanation");
        ensure!(target.exists(), "Busy attachment disappeared");
        drop(held);
        control.request_detach()?;
        phase(&control, BridgePhase::Detached).await?;
        ensure!(tokio::time::timeout(Duration::from_secs(10), child.wait()).await??.success(), "Bridge exit failed");
        ensure!(unsafe { windows::Win32::Storage::FileSystem::GetLogicalDrives() } & (1 << (letter - b'A')) == 0, "Drive remained after detach");
        println!("NATIVE_WINDOWS_DETACH_PASS: busy file preserved mapping; ordinary detach removed drive after close");
        Ok(())
    }.await;
    if result.is_err() {
        let _ = child.kill().await;
    }
    serving.abort();
    let _ = serving.await;
    result
}
