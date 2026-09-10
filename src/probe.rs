// SPDX-License-Identifier: GPL-3.0-only
//! Invoked only by an explicit disposable-root acceptance harness.
use super::*;
use anyhow::ensure;
pub fn run(pipe: Pipe) -> anyhow::Result<()> {
    let path = MountPath::root().child("bridge-test.txt")?;
    let capabilities = pipe.call(Operation::Capabilities)?;
    ensure!(
        matches!(
            capabilities,
            Value::Capabilities(FsCapabilities { writable: true, .. })
        ),
        "Writable disposable grant required"
    );
    let handle = match pipe.call(Operation::Open {
        path: path.clone(),
        options: FsOpenOptions {
            read: true,
            write: true,
            create: FsCreate::CreateNew,
            truncate: false,
        },
    })? {
        Value::Handle(id) => id,
        _ => anyhow::bail!("Expected a handle"),
    };
    let offset = (1_u64 << 32) + 19;
    pipe.call(Operation::Write {
        handle,
        offset,
        bytes: b"pipe roundtrip".to_vec(),
    })?;
    pipe.call(Operation::Flush { handle })?;
    ensure!(
        matches!(pipe.call(Operation::Read { handle, offset, length: 14 })?, Value::Data(ref bytes) if bytes == b"pipe roundtrip"),
        "Read/write bytes differ"
    );
    pipe.call(Operation::SetFileMetadata {
        handle,
        metadata: FsSetMetadata {
            size: Some(12),
            ..Default::default()
        },
    })?;
    let destination = MountPath::root().child("renamed.txt")?;
    pipe.call(Operation::Rename {
        from: path,
        to: destination.clone(),
        replace: false,
    })?;
    ensure!(
        matches!(
            pipe.call(Operation::FileMetadata { handle })?,
            Value::Metadata(FsMetadata { size: 12, .. })
        ),
        "Handle metadata differs after rename"
    );
    pipe.call(Operation::Close { handle })?;
    ensure!(
        pipe.call(Operation::Read {
            handle,
            offset: 0,
            length: 1
        })
        .is_err(),
        "Closed handle accepted"
    );
    pipe.call(Operation::Remove {
        path: destination,
        directory: false,
    })?;
    eprintln!("SHELLCANVAS_BRIDGE_TRANSPORT_PASS");
    Ok(())
}
