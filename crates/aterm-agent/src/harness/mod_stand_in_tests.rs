// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

/// Writes the owner-only `/bin/sh` stand-in `path` running `body`, then runs it once with
/// no arguments until it writes `ran` ([`aterm_uds::deadline::prints`]); `body` runs only
/// when it is given arguments. macOS's program check assesses a new file at its first exec,
/// and a loaded host holds that exec for seconds, which a bounded run of the stand-in (agy's
/// usage fetch, a version probe) would count against its budget: the bounded run must meet
/// a stand-in that has already run, and the stand-in's directory may go once this returns.
/// `body` runs on the `PATH` the bounded run gives it, which may hold nothing (on Linux an agy
/// read's opener-free `PATH`, `agy_gate::off_screen`), so it names each program it starts by
/// its absolute path (`/bin/cat`); the shell's builtins (`echo`, `printf`) need none.
pub(crate) fn proven(path: &std::path::Path, body: &str) {
    use aterm_uds::spawnfd::Stdio;
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::write(
        path,
        format!("#!/bin/sh\nif [ $# -eq 0 ]; then echo ran; exit 0; fi\n{body}\n"),
    )
    .expect("write the stand-in");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let mut child = aterm_uds::spawnfd::command(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the stand-in");
    aterm_uds::deadline::prints(&mut child, "ran", aterm_uds::deadline::ECHO_HANG);
    aterm_uds::deadline::kill_within(&mut child, aterm_uds::deadline::ECHO_HANG);
}
