// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Store layout paths for [`crate::lease`] and [`crate::discovery`].

use std::path::{Path, PathBuf};

/// `<prefix>/store` — the versioned, immutable builds, `store/<program>/<build>`.
pub const STORE_DIR: &str = "store";

/// `<prefix>/compat` — the store's exec roots, one per build.
pub const COMPAT_DIR: &str = "compat";

/// `<prefix>/rustup` — the seam's views, one per rustup toolchain name.
pub const VIEWS_DIR: &str = "rustup";

/// `<prefix>/store/<program>/<build>`.
#[inline]
#[must_use]
pub fn build_dir(prefix: &Path, program: &str, build: u64) -> PathBuf {
    prefix.join(STORE_DIR).join(program).join(build.to_string())
}

/// `<prefix>/rustup/<name>` — the view the seam `name` targets.
#[inline]
#[must_use]
pub fn view_dir(prefix: &Path, name: &str) -> PathBuf {
    prefix.join(VIEWS_DIR).join(name)
}

/// Return `true` when `build` is a valid non-zero store build identifier.
#[inline]
#[must_use]
pub const fn is_valid_build_id(build: u64) -> bool {
    build != 0
}

/// Encode a toolchain lease record (`build_id`, `holder_pid`, `epoch_secs`) into 16 LE bytes.
#[inline]
#[must_use]
pub const fn encode_lease_record_le(build_id: u64, holder_pid: u32, epoch_secs: u32) -> [u8; 16] {
    [
        build_id as u8,
        (build_id >> 8) as u8,
        (build_id >> 16) as u8,
        (build_id >> 24) as u8,
        (build_id >> 32) as u8,
        (build_id >> 40) as u8,
        (build_id >> 48) as u8,
        (build_id >> 56) as u8,
        holder_pid as u8,
        (holder_pid >> 8) as u8,
        (holder_pid >> 16) as u8,
        (holder_pid >> 24) as u8,
        epoch_secs as u8,
        (epoch_secs >> 8) as u8,
        (epoch_secs >> 16) as u8,
        (epoch_secs >> 24) as u8,
    ]
}

/// Decode 16 LE bytes into a toolchain lease record (`build_id`, `holder_pid`, `epoch_secs`).
#[inline]
#[must_use]
pub const fn decode_lease_record_le(bytes: [u8; 16]) -> (u64, u32, u32) {
    let build_id = (bytes[0] as u64)
        | ((bytes[1] as u64) << 8)
        | ((bytes[2] as u64) << 16)
        | ((bytes[3] as u64) << 24)
        | ((bytes[4] as u64) << 32)
        | ((bytes[5] as u64) << 40)
        | ((bytes[6] as u64) << 48)
        | ((bytes[7] as u64) << 56);
    let holder_pid = (bytes[8] as u32)
        | ((bytes[9] as u32) << 8)
        | ((bytes[10] as u32) << 16)
        | ((bytes[11] as u32) << 24);
    let epoch_secs = (bytes[12] as u32)
        | ((bytes[13] as u32) << 8)
        | ((bytes[14] as u32) << 16)
        | ((bytes[15] as u32) << 24);
    (build_id, holder_pid, epoch_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lease_record_le_round_trips() {
        assert!(!is_valid_build_id(0));
        assert!(is_valid_build_id(9192));
        let enc = encode_lease_record_le(9192, 4242, 1_700_000_000);
        assert_eq!(decode_lease_record_le(enc), (9192, 4242, 1_700_000_000));
    }
}
