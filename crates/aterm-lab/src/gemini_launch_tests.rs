// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::io::Read as _;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::{Child, Stdio};

use super::{Agent, Launch, launch, put_text};
use crate::run::Run;
use crate::util::shell_quote;

// Each outer test owns a whole child process group and its scratch directory.
// The child runs only our scripts: no installed agent or aterm is launched.
fn check_launch(agent: Agent, test: &str) {
    const CHILD: &str = "ATERM_LAB_LAUNCH_FIXTURE";
    if let Some(root) = std::env::var_os(CHILD) {
        launch_fixture(agent, Path::new(&root));
        println!("launch fixture completed");
        return;
    }
    let root = aterm_tempfile::Builder::new()
        .prefix("at.launch-")
        .tempdir_in("/tmp")
        .unwrap();
    let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    cmd.args(["--exact", test, "--nocapture"])
        .env_clear()
        .env("HOME", root.path())
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env("XDG_RUNTIME_DIR", root.path())
        .env("ATERM_STATE_HOME", root.path().join("state"))
        .current_dir(root.path())
        .env(CHILD, root.path());
    let out = aterm_uds::deadline::output_until(&mut cmd, Instant::now() + Duration::from_secs(60))
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

fn launch_fixture(agent: Agent, root: &Path) {
    // Door requires the lexical /tmp/at.* socket name; the scratch writer
    // requires a physical path because /tmp itself is a system symlink.
    let socket = root.join("a.sock");
    let sockdir = root.to_string_lossy().into_owned();
    let physical = std::fs::canonicalize(root).unwrap();
    let root = physical.as_path();
    let bin = root.join("bin");
    let calls = root.join("calls");
    let refuse = root.join("refuse-enter");
    let executed = root.join("agent-executed");
    let sampler = bin.join("aterm-lab");
    put_text(&sampler, 0o700, "#!/bin/sh\nprintf R\nIFS= read -r line\n").unwrap();
    let mut witness = Witness(
        aterm_uds::spawnfd::command(&sampler)
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
    let fake_agent = bin.join("agent");
    put_text(
        &fake_agent,
        0o700,
        &format!(
            "#!/bin/sh\n: > {}\nexit 99\n",
            shell_quote(&executed.to_string_lossy())
        ),
    )
    .unwrap();
    let fake_ctl = bin.join("aterm");
    put_text(
        &fake_ctl,
        0o700,
        &format!(
            "#!/bin/sh\n[ \"$1\" = ctl ] && [ \"$2\" = --sock ] && [ \"$3\" = {socket} ] && [ \"$4\" = -- ] || exit 90\nshift 4\ncase \"$1\" in\n  send) [ \"$#\" = 2 ] || exit 91; printf 'send\\n' >> {calls};;\n  key) [ \"$#\" = 2 ] && [ \"$2\" = enter ] || exit 92; printf 'key enter\\n' >> {calls}; if [ -f {refuse} ]; then printf 'ERR fixture-enter-refused\\n' >&2; exit 17; fi;;\n  *) exit 93;;\nesac\nprintf 'OK\\n'\n",
            socket = shell_quote(&socket.to_string_lossy()),
            calls = shell_quote(&calls.to_string_lossy()),
            refuse = shell_quote(&refuse.to_string_lossy()),
        ),
    )
    .unwrap();
    let _listener = UnixListener::bind(&socket).unwrap();
    let run = Run {
        tag: "launch-fixture".into(),
        run: root.into(),
        w: root.join("scratch"),
        sockdir,
        sock: socket.to_string_lossy().into_owned(),
        proj: root.to_string_lossy().into_owned(),
        sampler_pid: i32::try_from(witness.0.id()).unwrap(),
        aterm: fake_ctl.to_string_lossy().into_owned(),
        gemini_bin: fake_agent.to_string_lossy().into_owned(),
        agy_bin: fake_agent.to_string_lossy().into_owned(),
        ..Run::default()
    };
    let accepted = launch(&run, agent, &Launch::default()).unwrap();
    assert!(accepted.starts_with("launched:"));
    assert_eq!(
        std::fs::read_to_string(&calls).unwrap(),
        "send\nkey enter\n"
    );
    assert!(
        !executed.exists(),
        "the fake ctl must not execute the launch line"
    );
    std::fs::write(&refuse, b"refuse").unwrap();
    let rejected = launch(&run, agent, &Launch::default());
    assert_eq!(
        std::fs::read_to_string(&calls).unwrap(),
        "send\nkey enter\nsend\nkey enter\n",
        "the refused Enter must actually reach the transport"
    );
    assert!(!executed.exists());
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
fn gemini_launch_refuses_a_rejected_enter() {
    check_launch(
        Agent::Gemini,
        "gemini::launch_tests::gemini_launch_refuses_a_rejected_enter",
    );
}

#[test]
fn agy_launch_refuses_a_rejected_enter() {
    check_launch(
        Agent::Agy,
        "gemini::launch_tests::agy_launch_refuses_a_rejected_enter",
    );
}
