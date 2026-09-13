// SPDX-License-Identifier: GPL-3.0-only
//! Reserve remembered network drive letters without probing remote filesystems.
pub fn reserved(target: &str) -> anyhow::Result<bool> {
    use windows::{
        Win32::{
            Foundation::*, NetworkManagement::WNet::WNetGetConnectionW,
            Storage::FileSystem::GetLogicalDrives,
        },
        core::{HSTRING, PWSTR},
    };
    let value = target.as_bytes();
    anyhow::ensure!(
        value.len() == 2 && value[0].is_ascii_uppercase() && value[1] == b':',
        "Expected a drive letter"
    );
    let mask = unsafe { GetLogicalDrives() };
    anyhow::ensure!(
        mask != 0,
        "Unable to enumerate drives: {}",
        std::io::Error::last_os_error()
    );
    if mask & (1 << (value[0] - b'A')) != 0 {
        return Ok(true);
    }
    let mut remote = [0u16; 256];
    let mut length = remote.len() as u32;
    let status = unsafe {
        WNetGetConnectionW(
            &HSTRING::from(target),
            Some(PWSTR(remote.as_mut_ptr())),
            &mut length,
        )
    };
    match status {
        NO_ERROR | ERROR_MORE_DATA | ERROR_CONNECTION_UNAVAIL => Ok(true),
        ERROR_NOT_CONNECTED => Ok(false),
        other => anyhow::bail!(
            "Cannot check network drive reservation: {}",
            std::io::Error::from_raw_os_error(other.0 as i32)
        ),
    }
}
