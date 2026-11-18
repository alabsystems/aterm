// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The stand-in drivers this crate's tests run, written so a run of one never meets a writer.

use std::path::Path;

/// Make `path` an executable script (mode 0755), or rewrite it in place, so every hard link
/// reads the new body. A leading `/bin/sh` shebang names the retained signed interpreter on
/// Apple hosts; every byte after that line is kept.
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
        let shell = aterm_uds::deadline::stand_in(Path::new("/bin/sh"));
        let cat = aterm_uds::deadline::stand_in(Path::new("/bin/cat"));
        let rewritten = body
            .strip_prefix("#!/bin/sh\n")
            .map(|rest| format!("#!{}\n{rest}", shell.display()));
        let body = rewritten.as_deref().unwrap_or(body);
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
        let mut sh = aterm_uds::spawnfd::command(shell);
        sh.args(["-c", "exec \"$1\" >\"$2\"", "sh"])
            .arg(cat)
            .arg(path);
        let wrote = aterm_uds::deadline::output_from_while_process_progressing_complete_with_limit(
            &mut sh,
            staged.into(),
            aterm_spec::wait::MUST_HAPPEN,
            "writing the stand-in driver's script",
            4096,
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
    const STAGE: &str = "ATERM_TOOLCHAIN_LARGE_SCRIPT_WRITER";
    const NAME: &str = "test_script::a_body_past_a_pipes_capacity_is_written_whole";
    const RECEIPT: &str = "SCRIPT_BODY_WRITTEN";
    if std::env::var(STAGE).as_deref() != Ok("1") {
        let mut command =
            aterm_uds::spawnfd::command(std::env::current_exe().expect("this test binary"));
        command
            .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
            .env(STAGE, "1");
        let output = aterm_uds::deadline::output_while_process_progressing_complete_with_limit(
            &mut command,
            aterm_spec::wait::MUST_HAPPEN,
            "the large-script writer's isolated process",
            4096,
        )
        .expect("the isolated writer makes progress or its hang detector ends it");
        assert!(output.status.success(), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line == RECEIPT),
            "{output:?}"
        );
        return;
    }
    let scratch = aterm_tempfile::tempdir().unwrap();
    let path = scratch.path().join("long");
    let body = format!(
        "#!/bin/sh\n{}exit 0\n",
        "# a line of padding\n".repeat(50_000)
    );
    script(&path, &body);
    let actual = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        actual.split_once('\n').expect("interpreter line").1,
        body.split_once('\n').expect("interpreter line").1
    );
    println!("\n{RECEIPT}");
}
