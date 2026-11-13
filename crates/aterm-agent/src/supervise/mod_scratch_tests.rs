// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// NEGATIVE CONTROL: an uncapped `kind`/`tag` prefix plus `aterm_tempfile`'s
/// `{pid}_{16-hex}_{count}.noindex` suffix overruns macOS's 104-byte `sun_path` limit when a
/// fixture binds a Unix control socket inside `ScratchDir`; `short_scratch_stem` bounds both
/// UTF-8 prefixes on char boundaries so socket binds stay below `SUN_LEN`.
#[test]
fn scratch_dir_stays_under_unix_sun_len_even_for_long_tags() {
    let long_kind = "supervise-kind-overflow-ééé";
    let long_tag = "first-move-onto-a-build-found-exiting-with-extra-prose-ééé";
    let dir = ScratchDir::new(long_kind, long_tag);
    assert!(
        dir.to_string_lossy().ends_with(".noindex"),
        "Spotlight exclusion suffix missing: {}",
        dir.display()
    );
    let raw_path = test_scratch_path(long_kind, long_tag);
    assert!(
        raw_path.to_string_lossy().ends_with(".noindex"),
        "Spotlight exclusion suffix missing: {}",
        raw_path.display()
    );
    #[cfg(unix)]
    {
        let sock = dir.join("aterm-9999999.sock");
        assert!(
            sock.as_os_str().len() < 103,
            "socket path {} ({} bytes) must fit inside BSD sun_path",
            sock.display(),
            sock.as_os_str().len()
        );
        let _listener = std::os::unix::net::UnixListener::bind(&sock)
            .expect("bounded ScratchDir path must bind UnixListener");
    }
}
