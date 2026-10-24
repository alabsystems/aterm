// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Guarded lab fabric broker in scratch state.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::Stdio;

/// The fleet a lab's bus names (`/f/lab/…`).
pub const FLEET: &str = "lab";

/// The bus: its broker.
pub struct Fabric {
    broker: Option<aterm_uds::spawnfd::Child>,
    /// The broker's lifeline (`aterm_uds::lifeline`): it ends itself once this is
    /// dropped or the lab dies, a SIGKILL included, instead of serving on under
    /// launchd.
    _lifeline: aterm_uds::lifeline::Lifeline,
    /// The `[fabric]` table the instance's `aterm.toml` gets.
    pub table: String,
    /// The bridge's `--state` dir (its `topics/<sid>` cursors among it).
    pub state: PathBuf,
}

impl Fabric {
    /// Lay the bus out under `dir` and start its broker with `aterm`'s own `link broker`.
    /// # Errors
    /// A file could not be made, a grant could not be minted, or the broker never listened (within
    /// a minute: a hang detector).
    pub fn start(aterm: &Path, dir: &Path) -> Result<Fabric, String> {
        use std::os::unix::fs::PermissionsExt as _;
        let state = dir.join("state");
        std::fs::create_dir_all(&state).map_err(|e| format!("{}: {e}", state.display()))?;
        for d in [dir, state.as_path()] {
            std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| format!("{}: {e}", d.display()))?;
        }
        let mut random = [0u8; 40];
        {
            use std::io::Read as _;
            std::fs::File::open("/dev/urandom")
                .and_then(|mut f| f.read_exact(&mut random))
                .map_err(|e| format!("/dev/urandom: {e}"))?;
        }
        let secret = dir.join("mint.secret");
        let write_private = |path: &Path, bytes: &[u8]| {
            std::fs::write(path, bytes)
                .and_then(|()| {
                    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                })
                .map_err(|e| format!("{}: {e}", path.display()))
        };
        write_private(&secret, &random[..32])?;
        let node = format!(
            "n-{}",
            random[32..]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        write_private(&state.join("node"), format!("{node}\n").as_bytes())?;
        let program = dir.join("aterm");
        std::os::unix::fs::symlink(aterm, &program)
            .map_err(|e| format!("{}: {e}", program.display()))?;
        let mut caps = String::new();
        for grant in aterm_link::enable::node_grants(FLEET, &node) {
            let out = aterm_uds::spawnfd::command(&program)
                .args(["link", "mint", &grant, "--secret-file"])
                .arg(&secret)
                .stdin(Stdio::null())
                .output()
                .map_err(|e| format!("aterm link mint: {e}"))?;
            if !out.status.success() {
                return Err(format!(
                    "aterm link mint {grant}: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            caps.push_str(&String::from_utf8_lossy(&out.stdout));
        }
        let cap = dir.join("node.cap");
        write_private(&cap, caps.as_bytes())?;
        let sock = dir.join("b.sock");
        let log = std::fs::File::create(dir.join("broker.out"))
            .map_err(|e| format!("broker.out: {e}"))?;
        let err = log.try_clone().map_err(|e| format!("broker.out: {e}"))?;
        let mut broker = aterm_uds::spawnfd::command(&program);
        broker
            .args(["link", "broker"])
            .arg(&sock)
            .arg(dir.join("b.log"))
            .arg("--secret-file")
            .arg(&secret)
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(err));
        let lifeline = aterm_uds::lifeline::Lifeline::arm(&mut broker, dir)
            .map_err(|e| format!("the broker's lifeline in {}: {e}", dir.display()))?;
        let broker = broker
            .spawn()
            .map_err(|e| format!("aterm link broker: {e}"))?;
        let mut fabric = Fabric {
            broker: Some(broker),
            _lifeline: lifeline,
            table: format!(
                "[fabric]\ncommand = \"{program} link serve --fleet {FLEET} --broker {sock} \
                 --cap-file {cap} --state {state} --accept-from {node}\"\n",
                program = program.display(),
                sock = sock.display(),
                cap = cap.display(),
                state = state.display(),
            ),
            state,
        };
        let end = Instant::now() + Duration::from_secs(60);
        while !sock.exists() {
            if Instant::now() >= end
                || fabric
                    .broker
                    .as_mut()
                    .is_some_and(|b| matches!(b.try_wait(), Ok(Some(_))))
            {
                fabric.stop();
                let said = std::fs::read_to_string(dir.join("broker.out")).unwrap_or_default();
                return Err(format!("the lab's broker never listened: {}", said.trim()));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(fabric)
    }

    /// Append [`Fabric::table`] to the `aterm.toml` at `config`.
    /// # Errors
    /// The file could not be opened or written.
    pub fn name_in(&self, config: &Path) -> Result<(), String> {
        use std::io::Write as _;
        std::fs::OpenOptions::new()
            .append(true)
            .open(config)
            .and_then(|mut f| f.write_all(self.table.as_bytes()))
            .map_err(|e| format!("cannot name the bridge in {}: {e}", config.display()))
    }

    /// End the broker (`SIGTERM`, then `SIGKILL`).
    pub fn stop(&mut self) {
        let Some(mut broker) = self.broker.take() else {
            return;
        };
        if !matches!(broker.try_wait(), Ok(None)) {
            return;
        }
        if let Ok(pid) = i32::try_from(broker.id()) {
            let _ = crate::sys::kill(pid, libc::SIGTERM);
            let end = Instant::now() + Duration::from_secs(10);
            loop {
                if !matches!(broker.try_wait(), Ok(None)) {
                    return;
                }
                if Instant::now() >= end {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let _ = broker.kill();
        let _ = broker.wait();
    }
}

impl Drop for Fabric {
    fn drop(&mut self) {
        self.stop();
    }
}
