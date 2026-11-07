// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The stand-in drivers this crate's tests run, written so a run of one never meets a writer.

use std::path::Path;

/// Make `path` an executable script holding `body` (mode 0755), or rewrite it in place, so
/// every hard link to it reads the new body.
///
/// On Unix a child process writes it. Linux refuses to exec a file anything holds open for
/// writing (`ETXTBSY`), and a child another test thread forks while this process holds `path`
/// open keeps that writer until its own exec, so a probe of a script this process wrote can
/// fail to start and read as no compiler at all. The child opens `path` after its fork and
/// has exited before this returns, so no process holds a writer of it then. `body` reaches the
/// child as its stdin, a file this process writes whole first, unnamed before the child starts
/// (that file is never run, so a fork holding it open costs nothing): a pipe would hold no more
/// than its capacity before the child exists to read it.
#[track_caller]
pub(crate) fn script(path: &Path, body: &str) {
    #[cfg(unix)]
    {
        use std::io::{Seek as _, Write as _};
        use std::os::unix::fs::PermissionsExt as _;
        let mut staged = {
            let scratch = aterm_tempfile::tempdir().expect("a scratch folder for the body");
            let at = scratch.path().join("body");
            let file = std::fs::File::options()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&at)
                .unwrap_or_else(|e| panic!("stage {}: {e}", at.display()));
            std::fs::remove_file(&at).unwrap_or_else(|e| panic!("unname {}: {e}", at.display()));
            file
        };
        staged
            .write_all(body.as_bytes())
            .and_then(|()| staged.rewind())
            .expect("the script's bytes");
        let mut sh = aterm_uds::spawnfd::command("/bin/sh");
        sh.args(["-c", "exec cat >\"$1\"", "sh"]).arg(path);
        let wrote = aterm_uds::deadline::output_within_from(
            &mut sh,
            staged.into(),
            aterm_spec::wait::MUST_HAPPEN,
        )
        .unwrap_or_else(|e| panic!("write {}: {e:?}", path.display()));
        assert!(
            wrote.status.success(),
            "write {}: {wrote:?}",
            path.display()
        );
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .unwrap_or_else(|e| panic!("chmod {}: {e}", path.display()));
    }
    #[cfg(not(unix))]
    std::fs::write(path, body).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

/// A body past a pipe's capacity (64 KiB on Linux) is written whole, within the hang detector.
/// NEGATIVE CONTROL: feed `script`'s child through a pipe written whole before the child starts
/// and the write never ends.
#[cfg(unix)]
#[test]
fn a_body_past_a_pipes_capacity_is_written_whole() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().join("long");
    let body = format!(
        "#!/bin/sh\n{}exit 0\n",
        "# a line of padding\n".repeat(50_000)
    );
    let (wrote, written) = std::sync::mpsc::channel();
    let (at, text) = (path.clone(), body.clone());
    std::thread::spawn(move || {
        script(&at, &text);
        let _ = wrote.send(());
    });
    written
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the script written within the hang detector");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
}
