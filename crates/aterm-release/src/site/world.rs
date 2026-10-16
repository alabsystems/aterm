// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The machine's [`World`]: `gh`, the release download host, `curl` for the live
//! site, the site's own scripts and `firebase`.

use std::io::{BufRead as _, BufReader, Write};
use std::path::Path;
use std::rc::Rc;

use aterm_uds::spawnfd::{Command, Stdio, command};

use super::{Launch, Ran, World, output};

/// The variables that relocate cargo's output or replace its compiler or flags: the
/// engine's inputs are the fresh clone, its pin and the site's script, never the
/// caller's shell (a `CARGO_TARGET_DIR` makes the site's script read an empty
/// target directory; a `RUSTC_WRAPPER` that caps lints changes what compiles).
pub const ENGINE_BUILD_UNSET: [&str; 10] = [
    "CARGO_TARGET_DIR",
    "CARGO_BUILD_TARGET_DIR",
    "RUSTC",
    "CARGO_BUILD_RUSTC",
    "RUSTC_WRAPPER",
    "CARGO_BUILD_RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
    "CARGO_ENCODED_RUSTFLAGS",
    "CARGO_BUILD_RUSTFLAGS",
];

/// The site's `build-terminal-wasm.sh` over `clone`, from the site directory, handed
/// the wasm32 pin, with [`ENGINE_BUILD_UNSET`] and `unset` removed.
#[must_use]
pub fn engine_command(script: &Path, clone: &Path, pin: Option<&str>, unset: &[String]) -> Command {
    let mut build = command(script);
    build.arg(clone);
    if let Some(dir) = script.parent() {
        build.current_dir(dir);
    }
    for name in ENGINE_BUILD_UNSET
        .iter()
        .copied()
        .chain(unset.iter().map(String::as_str))
    {
        build.env_remove(name);
    }
    if let Some(pin) = pin {
        build.env("TERMINAL_WASM_TOOLCHAIN", pin);
    }
    build
}

/// The site's `deploy.sh`, run in `site`, with `unset` removed.
#[must_use]
pub fn deploy_command(site: &Path, unset: &[String]) -> Command {
    let mut deploy = command("./deploy.sh");
    deploy.current_dir(site);
    for name in unset {
        deploy.env_remove(name);
    }
    deploy
}

/// `firebase projects:list` with the credential deploy.sh would use: the service-account
/// key when it exists (and the site's headless config), else the pinned account.
#[must_use]
pub fn firebase_probe_command(
    site: &Path,
    account: Option<&str>,
    sa_key: Option<&Path>,
) -> Command {
    let mut probe = command("firebase");
    match sa_key.filter(|k| k.is_file()) {
        Some(key) => {
            probe
                .env("GOOGLE_APPLICATION_CREDENTIALS", key)
                .env("XDG_CONFIG_HOME", site.join(".firebase/headless-config"));
        }
        None => {
            if let Some(account) = account {
                probe.args(["--account", account]);
            }
        }
    }
    probe
        .args(["projects:list", "--non-interactive"])
        .stdin(Stdio::null());
    probe
}

/// `cmd` started through `launch` with its stdout and stderr on one pipe, each line
/// copied to `out` as it arrives: its exit code and everything it printed. Bytes that
/// are not UTF-8 are shown replaced and the pipe is read to its end, so the child never
/// meets a closed pipe mid-deploy.
fn streamed(launch: &dyn Launch, cmd: &mut Command, out: &mut dyn Write) -> Result<Ran, String> {
    let (reader, writer) = std::io::pipe().map_err(|e| format!("cannot make a pipe: {e}"))?;
    let second = writer
        .try_clone()
        .map_err(|e| format!("cannot make a pipe: {e}"))?;
    cmd.stdin(Stdio::null()).stdout(writer).stderr(second);
    let spawned = launch.spawn(cmd);
    // the command's copies of the write end go, so the reader sees the end
    cmd.stdout(Stdio::null()).stderr(Stdio::null());
    let (mut child, guard) =
        spawned.map_err(|e| format!("cannot run {:?}: {e}", cmd.get_program()))?;
    let mut log = String::new();
    let mut reader = BufReader::new(reader);
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let text = String::from_utf8_lossy(&line);
                let text = text.strip_suffix('\n').unwrap_or(&text);
                let _ = writeln!(out, "{text}");
                log.push_str(text);
                log.push('\n');
            }
        }
    }
    let status = child.wait().map_err(|e| format!("wait: {e}"));
    drop(guard);
    Ok(Ran {
        code: status?.code().unwrap_or(-1),
        log,
    })
}

/// The machine. `unset` names the variables no child it starts may inherit: the
/// launcher's own when `targo ... ship` runs it, or the release's whole scrub list;
/// `launch` starts every child.
pub struct Real {
    pub unset: Vec<String>,
    pub launch: Rc<dyn Launch>,
}

impl World for Real {
    fn launch(&self) -> Rc<dyn Launch> {
        Rc::clone(&self.launch)
    }

    fn gh_api(&mut self, path: &str, paginate: bool) -> Result<String, String> {
        let mut gh = command("gh");
        gh.args(["api", path]);
        if paginate {
            gh.arg("--paginate");
        }
        for name in &self.unset {
            gh.env_remove(name);
        }
        let out = output(&*self.launch, &mut gh).map_err(|e| format!("cannot run gh: {e}"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
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
        let mut curl = command("curl");
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

    fn build_engine(
        &mut self,
        script: &Path,
        clone: &Path,
        pin: Option<&str>,
        out: &mut dyn Write,
    ) -> Result<(), String> {
        let ran = streamed(
            &*self.launch,
            &mut engine_command(script, clone, pin, &self.unset),
            out,
        )?;
        if ran.code == 0 {
            Ok(())
        } else {
            Err(format!("exit {}", ran.code))
        }
    }

    fn deploy(&mut self, site: &Path, out: &mut dyn Write) -> Ran {
        streamed(&*self.launch, &mut deploy_command(site, &self.unset), out)
            .unwrap_or_else(|e| Ran { code: -1, log: e })
    }

    fn firebase_refuses(
        &mut self,
        site: &Path,
        account: Option<&str>,
        sa_key: Option<&Path>,
    ) -> Option<String> {
        let mut probe = firebase_probe_command(site, account, sa_key);
        for name in &self.unset {
            probe.env_remove(name);
        }
        match output(&*self.launch, &mut probe) {
            Ok(o) if o.status.success() => None,
            Ok(o) => Some(format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            )),
            Err(e) => Some(format!("cannot run firebase: {e}")),
        }
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
    fn the_callers_cargo_never_steers_the_engine_build_and_the_pin_is_handed_over() {
        let unset = vec!["CARGO_MANIFEST_DIR".to_string()];
        let cmd = engine_command(
            Path::new("/site/build-terminal-wasm.sh"),
            Path::new("/scratch/clone"),
            Some("1.97.1"),
            &unset,
        );
        let gone = removed(&cmd);
        for name in ENGINE_BUILD_UNSET
            .iter()
            .chain(["CARGO_MANIFEST_DIR"].iter())
        {
            assert!(
                gone.iter().any(|g| g == name),
                "{name} reaches the engine build"
            );
        }
        assert_eq!(
            set(&cmd, "TERMINAL_WASM_TOOLCHAIN"),
            Some(OsStr::new("1.97.1"))
        );
        assert_eq!(
            cmd.get_args().collect::<Vec<_>>(),
            [OsStr::new("/scratch/clone")]
        );
        assert_eq!(cmd.get_current_dir(), Some(Path::new("/site")));
        // no pin: nothing is handed, the script keeps its own default
        let cmd = engine_command(Path::new("/site/b.sh"), Path::new("/c"), None, &[]);
        assert_eq!(set(&cmd, "TERMINAL_WASM_TOOLCHAIN"), None);
    }

    /// A line that is not UTF-8 is shown replaced, and the pipe is read to its end: the
    /// child's later output arrives and it ends on its own terms, never on a closed pipe.
    #[cfg(unix)]
    #[test]
    fn streamed_output_survives_bytes_that_are_not_utf8() {
        let mut sh = command("/bin/sh");
        sh.args(["-c", "printf 'a\\377b\\n'; printf 'after\\n'; exit 3"]);
        let mut shown = Vec::new();
        let ran = streamed(&super::super::Plain, &mut sh, &mut shown).unwrap();
        assert_eq!(ran.code, 3);
        assert_eq!(ran.log, "a\u{fffd}b\nafter\n");
        assert_eq!(String::from_utf8(shown).unwrap(), ran.log);
    }

    #[test]
    fn the_login_probe_asks_with_the_credential_deploy_would_use() {
        let site = Path::new("/no/such/site");
        let cmd = firebase_probe_command(site, Some("owner@example.invalid"), None);
        assert_eq!(
            cmd.get_args().collect::<Vec<_>>(),
            [
                "--account",
                "owner@example.invalid",
                "projects:list",
                "--non-interactive"
            ]
        );
        // a key that exists wins, with the site's headless config
        let dir = aterm_tempfile::Builder::new()
            .prefix("aterm-site-key-")
            .tempdir()
            .unwrap();
        let key = dir.path().join("deployer.json");
        std::fs::write(&key, "{}").unwrap();
        let cmd = firebase_probe_command(site, Some("owner@example.invalid"), Some(&key));
        assert_eq!(
            cmd.get_args().collect::<Vec<_>>(),
            ["projects:list", "--non-interactive"]
        );
        assert_eq!(
            set(&cmd, "GOOGLE_APPLICATION_CREDENTIALS"),
            Some(key.as_os_str())
        );
        assert_eq!(
            set(&cmd, "XDG_CONFIG_HOME"),
            Some(site.join(".firebase/headless-config").as_os_str())
        );
        // a key named but absent falls back to the account
        let cmd = firebase_probe_command(site, Some("a@b.invalid"), Some(&dir.path().join("gone")));
        assert_eq!(cmd.get_args().next(), Some(OsStr::new("--account")));
    }
}
