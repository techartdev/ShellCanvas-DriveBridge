// SPDX-License-Identifier: GPL-3.0-only
//! Shared bookkeeping for WinFsp descriptors. Native callbacks use CoarseGuard.
use shellcanvas_filesystem_sdk::{FsError, FsErrorKind, FsResult, MountPath};
use std::sync::{Arc, Mutex, Weak};

/// Browsing directory handles do not retain file data or writable mappings.
/// Keep every file context and all mutation-capable directory contexts blocking.
pub fn blocks_detach(is_directory: bool, granted_access: u32) -> bool {
    const DIRECTORY_MUTATION: u32 = 0x2 | 0x4 | 0x10 | 0x100 | 0x10000 | 0x40000 | 0x80000;
    !is_directory || granted_access & DIRECTORY_MUTATION != 0
}

pub struct OpenHandle {
    pub path: Arc<Mutex<MountPath>>,
    pub file: Mutex<Option<u64>>,
    pub writable: bool,
}
#[derive(Default)]
pub struct OpenHandles(Mutex<Vec<Weak<OpenHandle>>>);
fn poisoned() -> FsError {
    FsError::new(FsErrorKind::Io, "Open handle bookkeeping unavailable")
}
impl OpenHandles {
    pub fn register(&self, handle: &Arc<OpenHandle>) -> FsResult<()> {
        let mut handles = self.0.lock().map_err(|_| poisoned())?;
        handles.retain(|item| item.strong_count() != 0);
        handles.push(Arc::downgrade(handle));
        Ok(())
    }
    /// Called with the native coarse operation guard held. Compute all path
    /// changes first; publish them only after the provider confirms the rename.
    pub fn rename(
        &self,
        from: &MountPath,
        to: &MountPath,
        rename: impl FnOnce() -> FsResult<()>,
    ) -> FsResult<()> {
        let mut handles = self.0.lock().map_err(|_| poisoned())?;
        handles.retain(|item| item.strong_count() != 0);
        let mut changes = Vec::new();
        for handle in handles.iter().filter_map(Weak::upgrade) {
            let path = handle.path.lock().map_err(|_| poisoned())?;
            if path.components().starts_with(from.components()) {
                let mut replacement = to.clone();
                for component in &path.components()[from.components().len()..] {
                    replacement = replacement.child(component)?;
                }
                changes.push((handle.path.clone(), replacement));
            }
        }
        rename()?;
        for (path, replacement) in changes {
            *path.lock().map_err(|_| poisoned())? = replacement;
        }
        Ok(())
    }
    /// Keep each descriptor locked until its flush is acknowledged. Close uses
    /// the same lock, so a concurrent close cannot recycle a handle mid-flush.
    /// Attempt all writable handles even when one flush fails; return the first error.
    pub fn flush(&self, mut flush: impl FnMut(u64) -> FsResult<()>) -> FsResult<()> {
        let handles: Vec<_> = self
            .0
            .lock()
            .map_err(|_| poisoned())?
            .iter()
            .filter_map(Weak::upgrade)
            .collect();
        let mut error = None;
        for handle in handles {
            if !handle.writable {
                continue;
            }
            let file = handle.file.lock().map_err(|_| poisoned())?;
            if let Some(id) = *file
                && let Err(e) = flush(id)
                && error.is_none()
            {
                error = Some(e);
            }
        }
        error.map_or(Ok(()), Err)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn passive_directory_handles_do_not_block_but_files_and_mutators_do() {
        assert!(!blocks_detach(
            true,
            0x1 | 0x8 | 0x20 | 0x80 | 0x20000 | 0x100000
        ));
        assert!(blocks_detach(false, 0));
        assert!(blocks_detach(false, 0x1));
        for access in [0x2, 0x4, 0x10, 0x100, 0x10000, 0x40000, 0x80000] {
            assert!(blocks_detach(true, access));
        }
    }
    fn path(parts: &[&str]) -> MountPath {
        parts
            .iter()
            .fold(MountPath::root(), |p, c| p.child(c).unwrap())
    }
    fn opened(
        registry: &OpenHandles,
        parts: &[&str],
        id: Option<u64>,
        writable: bool,
    ) -> Arc<OpenHandle> {
        let handle = Arc::new(OpenHandle {
            path: Arc::new(Mutex::new(path(parts))),
            file: Mutex::new(id),
            writable,
        });
        registry.register(&handle).unwrap();
        handle
    }
    #[test]
    fn rename_updates_aliases_and_descendants_only_after_success() {
        let registry = OpenHandles::default();
        let directory = opened(&registry, &["old"], None, false);
        let alias = opened(&registry, &["old"], None, false);
        let child = opened(&registry, &["old", "nested", "file"], Some(1), true);
        let sibling = opened(&registry, &["older", "file"], Some(2), true);
        let fail = registry.rename(&path(&["old"]), &path(&["new"]), || {
            Err(FsError::new(FsErrorKind::PermissionDenied, "denied"))
        });
        assert!(fail.is_err());
        assert_eq!(
            *child.path.lock().unwrap(),
            path(&["old", "nested", "file"])
        );
        registry
            .rename(&path(&["old"]), &path(&["new"]), || Ok(()))
            .unwrap();
        assert_eq!(*directory.path.lock().unwrap(), path(&["new"]));
        assert_eq!(*alias.path.lock().unwrap(), path(&["new"]));
        assert_eq!(
            *child.path.lock().unwrap(),
            path(&["new", "nested", "file"])
        );
        assert_eq!(*sibling.path.lock().unwrap(), path(&["older", "file"]));
        assert_eq!(*child.file.lock().unwrap(), Some(1));
    }
    #[test]
    fn volume_flush_attempts_all_writers_and_reports_failures() {
        let registry = OpenHandles::default();
        let _first = opened(&registry, &["first"], Some(1), true);
        let _read = opened(&registry, &["reader"], Some(2), false);
        let _second = opened(&registry, &["second"], Some(3), true);
        let closed = opened(&registry, &["closed"], Some(4), true);
        *closed.file.lock().unwrap() = None;
        drop(opened(&registry, &["dropped"], Some(5), true));
        let mut seen = Vec::new();
        let result = registry.flush(|id| {
            seen.push(id);
            if id == 1 {
                Err(FsError::new(FsErrorKind::Io, "flush failed"))
            } else {
                Ok(())
            }
        });
        assert_eq!(seen, vec![1, 3]);
        assert_eq!(result.unwrap_err().message, "flush failed");
    }
}
