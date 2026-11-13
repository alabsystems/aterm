// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::Proven;

/// What tells [`a_stand_in_agent_when_asked`] to stand in.
const STAND_IN_ENV: &str = "ATERM_SELF_CLAIM_TEST_STAND_IN";

/// THE STAND-IN'S BODY: this test binary, which waits for its parent's
/// end of the pipe to close.
#[test]
fn a_stand_in_agent_when_asked() {
    use std::io::Read as _;
    if std::env::var_os(STAND_IN_ENV).is_none() {
        return;
    }
    let _ = std::io::stdin().read(&mut [0u8; 1]);
}

/// Pid of the shared stand-in child process started under a scratch `HOME`.
pub(crate) fn pid() -> u32 {
    use aterm_uds::spawnfd::{Child, Stdio, command};
    static STAND_IN: std::sync::OnceLock<Child> = std::sync::OnceLock::new();
    STAND_IN
        .get_or_init(|| {
            let root = aterm_types::dirs::test_root().expect("this test binary's root");
            let home = root.join("stand-in");
            std::fs::create_dir_all(&home).expect("the stand-in's home");
            command(std::env::current_exe().expect("the test binary"))
                .args([
                    "self_claim::stand_in::a_stand_in_agent_when_asked",
                    "--exact",
                ])
                .env_clear()
                .env(STAND_IN_ENV, "1")
                .env("HOME", &home)
                .env("CODEX_HOME", home.join(".codex"))
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("the stand-in")
        })
        .id()
}

/// [`Proven`] identity of the running stand-in child process.
pub(crate) fn running() -> Proven {
    let pid = pid();
    Proven {
        pid,
        start: aterm_agent::harness::restart_stamp::kernel_start(pid).expect("its start"),
    }
}

/// A codex-alab stand-in of a test's own: the kept signed `sleep`, run in place with
/// `CODEX_HOME` the test's directory, whose session record the test plants
/// ([`Self::plant`]). Killed and reaped when dropped.
#[cfg(target_vendor = "apple")]
pub(crate) struct Sleeper {
    child: aterm_uds::spawnfd::Child,
    home: std::path::PathBuf,
    /// Its pid and kernel start.
    pub(crate) proven: Proven,
}

#[cfg(target_vendor = "apple")]
impl Sleeper {
    /// Starts one under `home` (canonical) and waits until it runs from the stand-in.
    pub(crate) fn under(home: &std::path::Path) -> Self {
        let sleep = std::fs::canonicalize(aterm_uds::deadline::stand_in(std::path::Path::new(
            "/bin/sleep",
        )))
        .expect("the kept stand-in");
        let sleep_s = sleep.to_string_lossy().into_owned();
        let child = aterm_uds::spawnfd::command(&sleep)
            .arg("60")
            .env("CODEX_HOME", home)
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !atpkg::caller_shell::process_args(pid)
            .is_some_and(|a| a.argv.first() == Some(&sleep_s))
        {
            assert!(std::time::Instant::now() < deadline, "sleep never ran");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let start = aterm_agent::harness::restart_stamp::kernel_start(pid).expect("its start");
        Self {
            child,
            home: home.to_path_buf(),
            proven: Proven { pid, start },
        }
    }

    /// Writes its session record naming `session` (`None`: `sessionId` null), busy and with
    /// no exit word, as codex-alab's writer does with or without a claim.
    pub(crate) fn plant(&self, session: Option<&str>) {
        self.plant_as(self.proven.pid, session, "busy");
    }

    /// Writes, at its own record's path, a record that says it is `pid`'s, names `session`
    /// and says `status`, with no exit word.
    pub(crate) fn plant_as(&self, pid: u32, session: Option<&str>, status: &str) {
        let path = self.record();
        std::fs::create_dir_all(path.parent().expect("dir")).expect("sessions dir");
        let session = session.map_or("null".to_string(), |s| format!("\"{s}\""));
        std::fs::write(
            &path,
            format!(
                r#"{{"v":1,"pid":{pid},"sessionId":{session},"status":"{status}","exit":null}}"#
            ),
        )
        .expect("the record");
    }

    /// Removes its session record, as codex-alab does at its end.
    pub(crate) fn unplant(&self) {
        std::fs::remove_file(self.record()).expect("the record removed");
    }

    fn record(&self) -> std::path::PathBuf {
        aterm_agent::harness::self_claimed::record_path(&self.home, self.proven.pid)
    }

    /// Kills it and waits for its end.
    pub(crate) fn end(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(target_vendor = "apple")]
impl Drop for Sleeper {
    fn drop(&mut self) {
        self.end();
    }
}
