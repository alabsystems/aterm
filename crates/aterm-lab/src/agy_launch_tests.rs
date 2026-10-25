// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::io::Read as _;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::time::Instant;

use aterm_spec::wait::{MUST_HAPPEN, first_exec};
use aterm_uds::spawnfd::{Child, Stdio};

use super::{Launch, launch, put_text};
use crate::run::Run;
use crate::util::shell_quote;

fn check_launch(test: &str) {
    const CHILD: &str = "ATERM_LAB_LAUNCH_FIXTURE";
    if let Some(root) = std::env::var_os(CHILD) {
        launch_fixture(&Fixture::at(Path::new(&root)));
        println!("launch fixture completed");
        return;
    }
    let root = aterm_tempfile::Builder::new()
        .prefix("at.launch-")
        .tempdir_in("/tmp")
        .unwrap();
    // Run `first_exec` on the two fixture scripts before starting the timed
    // child so macOS's first-exec assessment does not count against `MUST_HAPPEN`.
    let fixture = Fixture::at(root.path());
    fixture.write_programs();
    first_exec(&fixture.sampler, &[]);
    first_exec(&fixture.fake_ctl, &[]);
    let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    cmd.args(["--exact", test, "--nocapture"])
        .env_clear()
        .env("HOME", root.path())
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("XDG_RUNTIME_DIR", root.path())
        .env("ATERM_STATE_HOME", root.path().join("state"))
        .current_dir(root.path())
        .env(CHILD, root.path());
    let out = aterm_uds::deadline::output_until(&mut cmd, Instant::now() + MUST_HAPPEN)
        .expect("the owned launch fixture must finish within the hang detector");
    assert!(
        out.status.success(),
        "fixture status {:?}\n{}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("launch fixture completed"));
}

struct Fixture {
    /// The control socket, under the lexical `/tmp/at.*` directory the door
    /// requires.
    socket: PathBuf,
    /// That lexical directory.
    sockdir: String,
    /// The same directory's physical path (`/tmp` itself is a system link,
    /// and the scratch writer requires a physical one).
    root: PathBuf,
    calls: PathBuf,
    refuse: PathBuf,
    executed: PathBuf,
    denied: PathBuf,
    profile: PathBuf,
    /// The egress witness's stand-in: says `R`, then blocks in a read.
    sampler: PathBuf,
    /// The agent the launch line names; running it leaves `executed`.
    fake_agent: PathBuf,
    /// The aterm client the door runs: records each verb in `calls`.
    fake_ctl: PathBuf,
}

impl Fixture {
    fn at(lexical: &Path) -> Self {
        let root = std::fs::canonicalize(lexical).unwrap();
        let bin = root.join("bin");
        Self {
            socket: lexical.join("a.sock"),
            sockdir: lexical.to_string_lossy().into_owned(),
            calls: root.join("calls"),
            refuse: root.join("refuse-enter"),
            executed: root.join("agent-executed"),
            denied: root.join("account-canary"),
            profile: root.join("jail.sb"),
            sampler: bin.join("aterm-lab"),
            fake_agent: bin.join("agent"),
            fake_ctl: bin.join("aterm"),
            root,
        }
    }

    fn write_programs(&self) {
        put_text(
            &self.sampler,
            0o700,
            "#!/bin/sh\nprintf R\nIFS= read -r line\n",
        )
        .unwrap();
        put_text(
            &self.fake_agent,
            0o700,
            &format!(
                "#!/bin/sh\n: > {}\nexit 99\n",
                shell_quote(&self.executed.to_string_lossy())
            ),
        )
        .unwrap();
        put_text(
            &self.fake_ctl,
            0o700,
            &format!(
                "#!/bin/sh\n[ \"$1\" = ctl ] && [ \"$2\" = --sock ] && [ \"$3\" = {socket} ] && [ \"$4\" = -- ] || exit 90\nshift 4\nif /bin/cat {denied} >/dev/null 2>&1; then exit 94; fi\ncase \"$1\" in\n  send) [ \"$#\" = 2 ] || exit 91; printf 'send\\n' >> {calls};;\n  key) [ \"$#\" = 2 ] && [ \"$2\" = enter ] || exit 92; printf 'key enter\\n' >> {calls}; if [ -f {refuse} ]; then printf 'ERR fixture-enter-refused\\n' >&2; exit 17; fi;;\n  *) exit 93;;\nesac\nprintf 'OK\\n'\n",
                socket = shell_quote(&self.socket.to_string_lossy()),
                denied = shell_quote(&self.denied.to_string_lossy()),
                calls = shell_quote(&self.calls.to_string_lossy()),
                refuse = shell_quote(&self.refuse.to_string_lossy()),
            ),
        )
        .unwrap();
    }
}

struct Witness(Child);

impl Drop for Witness {
    fn drop(&mut self) {
        // The only stand-in child blocks in a shell builtin. SIGKILL cannot
        // leave a grandchild; wait reaps the exact handle even on assertion failure.
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn launch_fixture(f: &Fixture) {
    let root = f.root.as_path();
    let (calls, refuse, executed, profile) = (&f.calls, &f.refuse, &f.executed, &f.profile);
    std::fs::write(&f.denied, b"owned-account-canary").unwrap();
    let mut control = aterm_uds::spawnfd::command("/bin/cat");
    control.arg(&f.denied);
    let outside = aterm_uds::deadline::output_until(&mut control, Instant::now() + MUST_HAPPEN)
        .expect("the in-place cat reads the owned canary outside the jail");
    assert!(outside.status.success(), "{outside:?}");
    assert_eq!(outside.stdout, b"owned-account-canary");
    std::fs::write(
        profile,
        format!(
            "(version 1)\n(allow default)\n(deny file-read-data (literal {:?}))\n",
            f.denied.to_str().unwrap()
        ),
    )
    .unwrap();
    let mut witness = Witness(
        aterm_uds::spawnfd::command(&f.sampler)
            .arg("sampler")
            .arg(root)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut ready = [0];
    witness
        .0
        .stdout
        .as_mut()
        .unwrap()
        .read_exact(&mut ready)
        .unwrap();
    assert_eq!(ready, [b'R']);
    let _listener = UnixListener::bind(&f.socket).unwrap();
    let run = Run {
        tag: "launch-fixture".into(),
        run: root.into(),
        w: root.join("scratch"),
        sockdir: f.sockdir.clone(),
        sock: f.socket.to_string_lossy().into_owned(),
        proj: root.to_string_lossy().into_owned(),
        sampler_pid: i32::try_from(witness.0.id()).unwrap(),
        aterm: f.fake_ctl.to_string_lossy().into_owned(),
        agy_bin: f.fake_agent.to_string_lossy().into_owned(),
        ..Run::default()
    };
    let accepted = launch(&run, &Launch::default()).unwrap();
    assert!(accepted.starts_with("launched:"));
    assert_eq!(std::fs::read_to_string(calls).unwrap(), "send\nkey enter\n");
    assert!(
        !executed.exists(),
        "the fake ctl must not execute the launch line"
    );
    std::fs::write(refuse, b"refuse").unwrap();
    let rejected = launch(&run, &Launch::default());
    assert_eq!(
        std::fs::read_to_string(calls).unwrap(),
        "send\nkey enter\nsend\nkey enter\n",
        "the refused Enter must actually reach the transport"
    );
    assert!(!executed.exists());
    std::fs::remove_file(refuse).unwrap();
    std::fs::remove_file(profile).unwrap();
    assert!(
        launch(&run, &Launch::default()).is_err(),
        "a missing recorded jail must not launch an uncontained client"
    );
    assert_eq!(
        std::fs::read_to_string(calls).unwrap(),
        "send\nkey enter\nsend\nkey enter\n",
        "the client must not run without its profile"
    );
    let pid = witness.0.id();
    witness.0.kill().unwrap();
    let status = witness.0.wait().unwrap();
    assert!(!status.success());
    println!("owned witness {pid} killed and reaped");
    assert!(
        rejected.is_err(),
        "refused Enter reported success: {rejected:?}"
    );
}

#[test]
fn agy_launch_refuses_a_rejected_enter() {
    check_launch("agy::launch_tests::agy_launch_refuses_a_rejected_enter");
}
