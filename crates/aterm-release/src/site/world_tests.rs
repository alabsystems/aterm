// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::ffi::OsStr;
use std::sync::Mutex;

fn removed(cmd: &Command) -> Vec<String> {
    cmd.get_envs()
        .filter(|(_, v)| v.is_none())
        .map(|(k, _)| k.to_string_lossy().into_owned())
        .collect()
}

fn set<'c>(cmd: &'c Command, name: &str) -> Option<&'c OsStr> {
    cmd.get_envs()
        .find(|(k, _)| *k == name)
        .and_then(|(_, v)| v)
}

#[derive(Default)]
struct RecordingSpawn(Mutex<Vec<Vec<String>>>);

impl Spawn for RecordingSpawn {
    fn run(
        &self,
        cmd: &mut Command,
        _stdin: Option<&[u8]>,
    ) -> std::io::Result<std::process::Output> {
        self.0.lock().unwrap().push(removed(cmd));
        let mut answer = command(aterm_uds::os_path!("true")).output()?;
        answer.stdout = b"live bytes".to_vec();
        Ok(answer)
    }
}

#[test]
fn a_live_read_runs_through_the_sites_child_owner_with_its_environment_removed() {
    let recorder = Arc::new(RecordingSpawn::default());
    let mut world = Real {
        unset: vec!["RELEASE_LAUNCHER_SECRET".to_string()],
        launch: Rc::new(super::super::Plain),
        curl: recorder.clone(),
        reader: aterm_update_core::release_asset::Reader::anonymous(),
    };
    assert_eq!(
        world.live("https://example.invalid/live").unwrap(),
        b"live bytes"
    );
    let seen = recorder.0.lock().unwrap();
    assert_eq!(seen.len(), 1, "the site's child owner ran the one attempt");
    for name in ["RELEASE_LAUNCHER_SECRET", "CURL_HOME", "XDG_CONFIG_HOME"] {
        assert!(seen[0].iter().any(|removed| removed == name), "{name}");
    }
}

#[test]
fn the_callers_cargo_never_steers_the_engine_build_and_the_pin_builds_it() {
    let unset = vec!["CARGO_MANIFEST_DIR".to_string()];
    let clone = Path::new("/scratch/a \"clone\"");
    let flags = super::super::engine::rustflags(clone, Some(Path::new("/Users//b")));
    let cmd = module_command(
        Path::new("/rustup/1.97.1/bin/cargo"),
        Path::new("/rustup/1.97.1/bin/rustc"),
        "1.97.1",
        clone,
        &flags,
        &unset,
    )
    .unwrap();
    assert_eq!(cmd.get_program(), "/rustup/1.97.1/bin/cargo");
    assert_eq!(cmd.get_current_dir(), Some(clone));
    let own = ["RUSTC", "CARGO_TARGET_DIR", "CARGO_ENCODED_RUSTFLAGS"];
    let gone = removed(&cmd);
    for name in ENGINE_BUILD_UNSET
        .iter()
        .chain(["CARGO_MANIFEST_DIR"].iter())
        .filter(|n| !own.contains(n))
    {
        assert!(
            gone.iter().any(|g| g == name),
            "{name} reaches the engine build"
        );
    }
    assert_eq!(
        set(&cmd, "RUSTC"),
        Some(OsStr::new("/rustup/1.97.1/bin/rustc"))
    );
    assert_eq!(set(&cmd, "RUSTUP_TOOLCHAIN"), Some(OsStr::new("1.97.1")));
    assert_eq!(
        set(&cmd, "CARGO_TARGET_DIR"),
        Some(clone.join("target").as_os_str())
    );
    let encoded = set(&cmd, "CARGO_ENCODED_RUSTFLAGS")
        .unwrap()
        .to_str()
        .unwrap();
    assert_eq!(encoded.split('\x1f').collect::<Vec<_>>(), flags);
    assert_eq!(
        cmd.get_args().collect::<Vec<_>>(),
        [
            "build",
            "--release",
            "--locked",
            "--target",
            "wasm32-unknown-unknown",
            "-p",
            "aterm-wasm"
        ]
    );
    assert_eq!(
        module_path(clone),
        clone.join("target/wasm32-unknown-unknown/release/aterm_wasm.wasm")
    );
    let split = vec!["--cfg".to_string(), "a\x1fb".to_string()];
    let why = module_command(Path::new("c"), Path::new("r"), "p", clone, &split, &[])
        .err()
        .unwrap();
    assert!(why.contains("0x1f"), "{why}");
}

/// A line that is not UTF-8 is shown replaced, and the pipe is read to its end: the
/// child's later output arrives and it ends on its own terms, never on a closed pipe.
#[cfg(unix)]
#[test]
fn streamed_output_survives_bytes_that_are_not_utf8() {
    let mut sh = command("/bin/sh");
    sh.args(["-c", "printf 'a\\377b\\n'; printf 'after\\n'; exit 3"]);
    let mut shown = Vec::new();
    let ran = streamed(
        &super::super::Plain,
        &mut sh,
        &mut shown,
        aterm_spec::wait::MUST_HAPPEN,
    )
    .unwrap();
    assert_eq!(ran.code, 3);
    assert_eq!(ran.log, "a\u{fffd}b\nafter\n");
    assert_eq!(String::from_utf8(shown).unwrap(), ran.log);
}

struct Daemon(std::path::PathBuf);

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Ok(pid) = std::fs::read_to_string(&self.0) {
            let _ = command("/bin/kill")
                .args(["-KILL", pid.trim()])
                .stdin(Stdio::null())
                .status();
        }
    }
}

/// What `streamed` copies out, which makes `flag` once it holds `after`: the child's exit
/// waits for it, so both lines are read before the exit and the drain bounds only the end
/// of a pipe the grandchild keeps open.
struct Flagging {
    seen: Vec<u8>,
    flag: std::path::PathBuf,
}

impl Write for Flagging {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.seen.extend_from_slice(buf);
        if self.seen.ends_with(b"after\n") {
            std::fs::write(&self.flag, b"")?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_grandchild_holding_the_pipe_is_not_waited_on() {
    const DRAIN: std::time::Duration = std::time::Duration::from_millis(100);
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-site-daemon-")
        .tempdir()
        .unwrap();
    let _daemon = Daemon(dir.path().join("pid"));
    let flag = dir.path().join("shown");
    let mut cmd = command("/bin/sh");
    // The exit waits (up to the hang detector) until `after` was copied out.
    cmd.args([
        "-c",
        "echo before; sleep 600 & echo \"$!\" > \"$1\"; echo after; i=0; \
         while [ ! -e \"$2\" ] && [ $i -lt 6000 ]; do sleep 0.01; i=$((i+1)); done; exit 3",
        "sh",
    ])
    .arg(dir.path().join("pid"))
    .arg(&flag);
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut out = Flagging {
            seen: Vec::new(),
            flag,
        };
        let started = std::time::Instant::now();
        let ran = streamed(&super::super::Plain, &mut cmd, &mut out, DRAIN);
        let _ = done.send((ran, out.seen, started.elapsed()));
    });
    let (ran, out, took) = finished
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("streamed returned once the child exited, the daemon notwithstanding");
    let ran = ran.expect("it ran");
    assert_eq!(ran.code, 3, "{ran:?}");
    assert_eq!(ran.log, "before\nafter\n");
    assert_eq!(
        String::from_utf8_lossy(&out),
        ran.log,
        "each line copied out"
    );
    assert!(took >= DRAIN, "the drain is bounded, not skipped: {took:?}");
}
