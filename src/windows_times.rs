// SPDX-License-Identifier: GPL-3.0-only
use shellcanvas_filesystem_sdk::{FsError, FsErrorKind, FsResult};

const EPOCH: u64 = 116_444_736_000_000_000;
const TICKS_PER_SECOND: u64 = 10_000_000;

/// Zero means unavailable. Never invent a Unix-epoch date or saturate to an
/// invalid FILETIME when a provider has no representable timestamp.
pub fn filetime(seconds: Option<u64>) -> u64 {
    seconds
        .and_then(|value| value.checked_mul(TICKS_PER_SECOND))
        .and_then(|value| value.checked_add(EPOCH))
        .filter(|value| *value <= i64::MAX as u64)
        .unwrap_or(0)
}

pub fn unix_seconds(time: u64) -> FsResult<Option<u64>> {
    if time == 0 {
        return Ok(None);
    }
    if !(EPOCH..=i64::MAX as u64).contains(&time) {
        return Err(unsupported(
            "Timestamp is outside the portable provider's Unix time range",
        ));
    }
    // The portable contract stores whole seconds. Round down consistently with
    // its precision; the provider may have a narrower representable range.
    Ok(Some((time - EPOCH) / TICKS_PER_SECOND))
}

pub fn validate_unsupported_times(created: u64, changed: u64) -> FsResult<()> {
    if created != 0 || changed != 0 {
        return Err(unsupported(
            "This provider cannot set file creation or metadata-change time",
        ));
    }
    Ok(())
}

fn unsupported(message: &str) -> FsError {
    FsError::new(FsErrorKind::Unsupported, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absent_and_out_of_range_times_are_not_fabricated() {
        assert_eq!(filetime(None), 0);
        assert_eq!(filetime(Some(u64::MAX)), 0);
        assert_eq!(filetime(Some(0)), EPOCH);
        assert_eq!(unix_seconds(0).unwrap(), None);
        assert_eq!(unix_seconds(EPOCH).unwrap(), Some(0));
        assert!(unix_seconds(EPOCH - 1).is_err());
        assert!(unix_seconds(u64::MAX).is_err());
    }
    #[test]
    fn seconds_roundtrip_and_unsupported_fields_fail() {
        let seconds = 1_600_000_123;
        assert_eq!(
            unix_seconds(filetime(Some(seconds))).unwrap(),
            Some(seconds)
        );
        assert_eq!(
            unix_seconds(filetime(Some(seconds)) + 9_999_999).unwrap(),
            Some(seconds)
        );
        assert!(validate_unsupported_times(0, 0).is_ok());
        assert!(validate_unsupported_times(EPOCH, 0).is_err());
        assert!(validate_unsupported_times(0, EPOCH).is_err());
    }
}
