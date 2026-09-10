// SPDX-License-Identifier: GPL-3.0-only
//! Windows read-only projection for providers exposing POSIX permissions.
use shellcanvas_filesystem_sdk::{FsError, FsErrorKind, FsKind, FsMetadata, FsResult};

const READONLY: u32 = 0x1;
const DIRECTORY: u32 = 0x10;
const NORMAL: u32 = 0x80;

/// Windows may supply ARCHIVE when creating/overwriting ordinary files. The
/// portable provider has no backup-archive state; do not turn that advisory bit
/// into a failure for ordinary saves, or pretend it is persisted on readback.
pub fn creation_attributes(requested: u32, directory: bool) -> FsResult<u32> {
    let requested = requested & !0x20;
    validate_attributes(requested)?;
    if directory && requested & READONLY != 0 {
        return Err(unsupported("Read-only creation is supported only for regular files"));
    }
    Ok(requested)
}

fn validate_attributes(requested: u32) -> FsResult<()> {
    if requested & !(READONLY | DIRECTORY | NORMAL) != 0 {
        return Err(unsupported(
            "This provider cannot persist Windows hidden, system, archive or other DOS attributes",
        ));
    }
    Ok(())
}

pub fn attributes(meta: &FsMetadata) -> u32 {
    if meta.kind == FsKind::Directory {
        DIRECTORY
    } else if meta.kind == FsKind::File && meta.permissions.is_some_and(|p| p & 0o222 == 0) {
        READONLY
    } else {
        NORMAL
    }
}

pub fn permissions(meta: &FsMetadata, requested: u32) -> FsResult<Option<u32>> {
    // WinFsp uses INVALID_FILE_ATTRIBUTES for a timestamp-only update.
    if requested == u32::MAX {
        return Ok(None);
    }
    validate_attributes(requested)?;
    let readonly = requested & READONLY != 0;
    if meta.kind != FsKind::File {
        return if readonly {
            Err(unsupported(
                "Read-only attribute changes are supported only for regular files",
            ))
        } else {
            Ok(None)
        };
    }
    if readonly == (attributes(meta) & READONLY != 0) {
        return Ok(None);
    }
    let current = meta
        .permissions
        .ok_or_else(|| unsupported("Remote permissions are unavailable"))?;
    // The portable core currently supports ordinary permission bits only. Never
    // silently clear existing special mode bits while changing a Windows flag.
    if current & 0o7000 != 0 {
        return Err(unsupported(
            "Change special Unix permissions on the remote host",
        ));
    }
    Ok(Some(if readonly {
        current & !0o222
    } else {
        current | 0o200
    }))
}

fn unsupported(message: &str) -> FsError {
    FsError::new(FsErrorKind::Unsupported, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn file(mode: Option<u32>) -> FsMetadata {
        FsMetadata {
            kind: FsKind::File,
            size: 3,
            accessed: None,
            modified: None,
            permissions: mode,
        }
    }
    #[test]
    fn readonly_roundtrip_never_grants_group_or_other_write() {
        assert_eq!(attributes(&file(Some(0o755))), NORMAL);
        assert_eq!(
            permissions(&file(Some(0o755)), READONLY).unwrap(),
            Some(0o555)
        );
        assert_eq!(attributes(&file(Some(0o555))), READONLY);
        assert_eq!(
            permissions(&file(Some(0o555)), NORMAL).unwrap(),
            Some(0o755)
        );
        assert_eq!(
            permissions(&file(Some(0o444)), NORMAL).unwrap(),
            Some(0o644)
        );
        assert_eq!(permissions(&file(Some(0o444)), u32::MAX).unwrap(), None);
        assert_eq!(permissions(&file(Some(0o644)), NORMAL).unwrap(), None);
    }
    #[test]
    fn unsupported_changes_fail_before_changing_permissions() {
        for flag in [2, 4, 32, 256, 4096] {
            assert_eq!(
                permissions(&file(Some(0o644)), flag | READONLY)
                    .unwrap_err()
                    .kind,
                FsErrorKind::Unsupported
            );
        }
        assert!(permissions(&file(None), READONLY).is_err());
        assert!(permissions(&file(Some(0o4755)), READONLY).is_err());
        let mut directory = file(Some(0o555));
        directory.kind = FsKind::Directory;
        assert_eq!(attributes(&directory), DIRECTORY);
        assert!(permissions(&directory, DIRECTORY | READONLY).is_err());
    }
}
