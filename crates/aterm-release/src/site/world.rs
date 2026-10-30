// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real [`World`] implementation for site engine builds, verification, and deployment
//! (Firebase Hosting's REST API, [`super::hosting`]).

use std::io::{BufRead as _, BufReader, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use aterm_uds::spawnfd::{Command, Stdio, command};
use aterm_update_core::github_rest::Spawn;
use aterm_verify::wasm_glue::WASM_OPT_FLAGS;

use super::engine::{STEM, TARGET};
use super::{ENGINE_UNPROVEN, Launch, Ran, World, hosting, output};

/// Environment variables stripped from the engine cargo build.
pub const ENGINE_BUILD_UNSET: [&str; 12] = [
    "CARGO_TARGET_DIR",
    "CARGO_BUILD_TARGET_DIR",
    "CARGO_BUILD_BUILD_DIR",
    "RUSTC",
    "CARGO_BUILD_RUSTC",
    "RUSTC_WRAPPER",
    "CARGO_BUILD_RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
    "RUSTFLAGS",
    "CARGO_ENCODED_RUSTFLAGS",
    "CARGO_BUILD_RUSTFLAGS",
];

/// Builds the command for `pin`'s `cargo` (STOCK EXCEPTION: Trust ships only its host std) on `aterm-wasm` in `clone`.
///
/// # Errors
/// A flag [`encoded_rustflags`] cannot carry.
pub fn module_command(
    cargo: &Path,
    rustc: &Path,
    pin: &str,
    clone: &Path,
    rustflags: &[String],
    unset: &[String],
) -> Result<Command, String> {
    let encoded = encoded_rustflags(rustflags)?;
    let mut build = command(cargo);
    build.current_dir(clone);
    for name in ENGINE_BUILD_UNSET
        .iter()
        .copied()
        .chain(unset.iter().map(String::as_str))
    {
        build.env_remove(name);
    }
    build
        .env("RUSTC", rustc)
        .env("RUSTUP_TOOLCHAIN", pin)
        .env("CARGO_TARGET_DIR", clone.join("target"))
        .env("CARGO_ENCODED_RUSTFLAGS", encoded)
        .args([
            "build",
            "--release",
            "--locked",
            "--target",
            TARGET,
            "-p",
            "aterm-wasm",
        ]);
    Ok(build)
}

/// Joins `flags` with `0x1f` for `CARGO_ENCODED_RUSTFLAGS`.
///
/// # Errors
/// A flag that holds 0x1f itself, which cargo would split in two.
pub fn encoded_rustflags(flags: &[String]) -> Result<String, String> {
    match flags.iter().find(|f| f.contains('\x1f')) {
        Some(flag) => Err(format!(
            "the rustflag {flag:?} holds 0x1f, which CARGO_ENCODED_RUSTFLAGS splits on"
        )),
        None => Ok(flags.join("\x1f")),
    }
}

/// Where [`module_command`] leaves the CPU module.
#[must_use]
pub fn module_path(clone: &Path) -> PathBuf {
    clone
        .join("target")
        .join(TARGET)
        .join("release")
        .join(format!("{STEM}.wasm"))
}

fn keep_rust_homes(cmd: &mut Command) {
    for name in ["RUSTUP_HOME", "CARGO_HOME"] {
        if let Some(v) = std::env::var_os(name) {
            cmd.env(name, v);
        }
    }
}

fn wasm_opt_program() -> PathBuf {
    aterm_uds::host::find_tool("wasm-opt").unwrap_or_else(|| "wasm-opt".into())
}

const DRAIN_AFTER_EXIT: std::time::Duration = std::time::Duration::from_secs(5);

enum Event {
    Line(Vec<u8>),
    End,
    Exited(std::io::Result<std::process::ExitStatus>),
}

fn streamed(
    launch: &dyn Launch,
    cmd: &mut Command,
    out: &mut dyn Write,
    drain: std::time::Duration,
) -> Result<Ran, String> {
    let (reader, writer) = std::io::pipe().map_err(|e| format!("cannot make a pipe: {e}"))?;
    let second = writer
        .try_clone()
        .map_err(|e| format!("cannot make a pipe: {e}"))?;
    cmd.stdin(Stdio::null()).stdout(writer).stderr(second);
    let spawned = launch.spawn(cmd);
    cmd.stdout(Stdio::null()).stderr(Stdio::null());
    let (mut child, guard) =
        spawned.map_err(|e| format!("cannot run {:?}: {e}", cmd.get_program()))?;
    let (tx, rx) = std::sync::mpsc::channel::<Event>();
    let lines = tx.clone();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        loop {
            let mut line = Vec::new();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if lines.send(Event::Line(line)).is_err() {
                        return;
                    }
                }
            }
        }
        let _ = lines.send(Event::End);
    });
    let waiter = std::thread::spawn(move || {
        let _ = tx.send(Event::Exited(child.wait()));
    });
    let mut log = String::new();
    let (mut status, mut ended, mut until) = (None, false, None);
    while status.is_none() || !ended {
        let event = match until {
            None => rx.recv().ok(),
            Some(until) => rx
                .recv_timeout(until - std::time::Instant::now().min(until))
                .ok(),
        };
        match event {
            Some(Event::Line(line)) => {
                let text = String::from_utf8_lossy(&line);
                let text = text.strip_suffix('\n').unwrap_or(&text);
                let _ = writeln!(out, "{text}");
                log.push_str(text);
                log.push('\n');
            }
            Some(Event::End) => ended = true,
            Some(Event::Exited(s)) => {
                status = Some(s);
                until = Some(std::time::Instant::now() + drain);
            }
            None => break,
        }
    }
    let _ = waiter.join();
    drop(guard);
    let status = status
        .ok_or_else(|| "wait: the child's status was never read".to_string())?
        .map_err(|e| format!("wait: {e}"))?;
    Ok(Ran {
        code: status.code().unwrap_or(-1),
        log,
    })
}

/// The machine.
pub struct Real {
    pub unset: Vec<String>,
    pub launch: Rc<dyn Launch>,
    /// How the REST calls start curl: GitHub's, and Firebase Hosting's and Google's token
    /// endpoint's for the deploy.
    pub curl: Arc<dyn Spawn>,
}

impl World for Real {
    fn launch(&self) -> Rc<dyn Launch> {
        Rc::clone(&self.launch)
    }

    fn github(&mut self, path: &str, paginate: bool) -> Result<String, String> {
        if let Some(signal) = self.launch.interrupted() {
            return Err(format!(
                "not asked: the sync was interrupted by signal {signal}"
            ));
        }
        let spawn = Some(&self.curl);
        let answered = if paginate {
            crate::github::pages_via(spawn, path, 100).map(aterm_json::Value::Array)
        } else {
            crate::github::get_via(spawn, path)
        };
        answered.map(|v| v.to_string()).map_err(|e| e.to_string())
    }

    fn download(&mut self, url: &str, limit: u64) -> Result<Vec<u8>, String> {
        if let Some(path) = url.strip_prefix("file://") {
            let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
            if bytes.len() as u64 > limit {
                return Err(format!("larger than {limit} bytes"));
            }
            return Ok(bytes);
        }
        aterm_update_core::download_bytes(url, limit)
    }

    fn live(&mut self, url: &str) -> Result<Vec<u8>, String> {
        let mut curl = command(aterm_uds::os_path!("curl"));
        curl.args([
            "-fsSL",
            "--max-time",
            "30",
            "-H",
            "Cache-Control: no-cache",
            "--",
            url,
        ]);
        for name in &self.unset {
            curl.env_remove(name);
        }
        let out = output(&*self.launch, &mut curl).map_err(|e| format!("cannot run curl: {e}"))?;
        if out.status.success() {
            Ok(out.stdout)
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }

    fn build_module(
        &mut self,
        clone: &Path,
        pin: &str,
        rustflags: &[String],
        out: &mut dyn Write,
    ) -> Result<Vec<u8>, String> {
        let launch = &*self.launch;
        let unset = &self.unset;
        let plain = |program: &Path| {
            let mut cmd = command(program);
            for name in unset {
                cmd.env_remove(name);
            }
            keep_rust_homes(&mut cmd);
            cmd
        };
        let mut probe = plain(&wasm_opt_program());
        probe.arg("--version");
        if !output(launch, &mut probe).is_ok_and(|o| o.status.success()) {
            return Err(format!(
                "wasm-opt not found — the engine never ships without its -O3 pass: {}",
                aterm_uds::host::tool("wasm-opt").remedy()
            ));
        }
        let rustup = aterm_uds::host::tool("rustup").locate().map_err(|absent| {
            format!(
                "{} — the wasm32 pin {pin} is a stock channel rustup installs: {}",
                absent.why, absent.fix
            )
        })?;
        let mut add = plain(&rustup);
        add.args(["target", "add", "--toolchain", pin, TARGET]);
        let added = streamed(launch, &mut add, out, DRAIN_AFTER_EXIT)?;
        if added.code != 0 {
            return Err(format!(
                "rustup could not add {TARGET} to {pin} (exit {}) — rustup toolchain install \
                 {pin} --target {TARGET}",
                added.code
            ));
        }
        let which = |tool: &str| -> Result<PathBuf, String> {
            let mut ask = plain(&rustup);
            ask.args(["which", "--toolchain", pin, tool]);
            let o = output(launch, &mut ask).map_err(|e| format!("cannot run rustup: {e}"))?;
            let path = PathBuf::from(String::from_utf8_lossy(&o.stdout).trim());
            if o.status.success() && path.is_file() {
                Ok(path)
            } else {
                Err(format!(
                    "`rustup which --toolchain {pin} {tool}` names no file: {}",
                    String::from_utf8_lossy(&o.stderr).trim()
                ))
            }
        };
        let (cargo, rustc) = (which("cargo")?, which("rustc")?);
        let mut build = module_command(&cargo, &rustc, pin, clone, rustflags, unset)?;
        keep_rust_homes(&mut build);
        let ran = streamed(launch, &mut build, out, DRAIN_AFTER_EXIT)
            .map_err(|e| format!("{ENGINE_UNPROVEN}: {e}"))?;
        if ran.code != 0 {
            // Cargo waits for each build script it starts: only its own exit, with no signal
            // forwarded to them, shows they ran.
            let unproven = ran.code < 0 || launch.interrupted().is_some();
            return Err(format!(
                "{}the wasm32 build of aterm-wasm on {pin} exited {}",
                if unproven {
                    format!("{ENGINE_UNPROVEN}: ")
                } else {
                    String::new()
                },
                ran.code
            ));
        }
        let module = module_path(clone);
        std::fs::read(&module).map_err(|e| format!("{}: {e}", module.display()))
    }

    fn optimise(&mut self, wasm: &Path, out: &mut dyn Write) -> Result<(), String> {
        let mut opt = command(wasm_opt_program());
        opt.args(WASM_OPT_FLAGS).arg("-o").arg(wasm).arg(wasm);
        for name in &self.unset {
            opt.env_remove(name);
        }
        let ran = streamed(&*self.launch, &mut opt, out, DRAIN_AFTER_EXIT)?;
        if ran.code == 0 {
            Ok(())
        } else {
            Err(format!("wasm-opt exited {}", ran.code))
        }
    }

    fn deploy(
        &mut self,
        plan: &hosting::Plan,
        key: Option<&Path>,
        message: &str,
        out: &mut dyn Write,
    ) -> Result<hosting::Released, hosting::Failed> {
        let key = key.ok_or_else(|| {
            hosting::Failed::Credential(
                "no service-account key is named (FIREBASE_SA_KEY, or publish/site.toml's \
                 firebase_sa_key)"
                    .into(),
            )
        })?;
        let key = hosting::Key::read(key).map_err(hosting::Failed::Credential)?;
        let client = hosting::Client::new(
            hosting::Origins::google(),
            Arc::clone(&self.curl),
            &std::env::temp_dir(),
        )
        .map_err(|why| hosting::Failed::NotReleased {
            why: format!("cannot make a scratch directory: {why}"),
            version: None,
        })?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        hosting::deploy(&client, &key, plan, message, now, out)
    }

    fn sleep(&mut self, secs: u64) {
        std::thread::sleep(std::time::Duration::from_secs(secs));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

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
}
