// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Guarded lab fabric broker in scratch state: its layout ([`lay`]), the proofs' unjailed broker
//! ([`Fabric::start`]) and the wait for a broker's socket both share ([`await_listening`]).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::Stdio;

/// The fleet a lab's bus names (`/f/lab/…`).
pub const FLEET: &str = "lab";

/// How long a broker the lab started may take to listen: a hang detector, never a latency
/// budget ([`await_listening`]).
pub const LISTEN_HANG: Duration = Duration::from_secs(60);

/// A mint: `aterm link mint <grant> --secret-file <secret>`'s argv in, its standard output (one
/// `<grant> <tag-hex>` line) out, or why it failed ([`lay`]).
pub type Mint<'a> = dyn FnMut(&[String]) -> Result<Vec<u8>, String> + 'a;

/// A lab's bus laid out ([`lay`]): the keys a bridge and an observer read, and the live dir its
/// broker and bridge write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Laid {
    /// The keys dir (0700): the program link, the mint secret and the caps.
    pub keys: PathBuf,
    /// `<keys>/aterm`, a link to the aterm under test (the broker's and the bridge's program).
    pub program: PathBuf,
    /// `<keys>/mint.secret` (32 bytes, 0600).
    pub secret: PathBuf,
    /// `<keys>/node.cap` (0600): the node's ring, its ceiling's rules in order
    /// ([`astream_cap::GrantCeiling::aterm_node_ring`]).
    pub cap: PathBuf,
    /// `<keys>/observer.cap` (0600): an observer's ring, its ceiling's rules
    /// ([`astream_cap::GrantCeiling::aterm_person_ring`]), when one was asked for.
    pub observer: Option<PathBuf>,
    /// The live dir (0700): the broker's socket and log, the bridge's state.
    pub live: PathBuf,
    /// `<live>/b.sock`, the broker's socket.
    pub sock: PathBuf,
    /// `<live>/state` (0700), the bridge's `--state` dir, holding `node`.
    pub state: PathBuf,
    /// The node id (`n-` and 16 hex digits).
    pub node: String,
    /// The `[fabric]` table the instance's `aterm.toml` gets.
    pub table: String,
}

impl Laid {
    /// The bridge's argv, as the table's `command` names it.
    #[must_use]
    pub fn command(&self) -> Vec<String> {
        [
            self.program.to_string_lossy().as_ref(),
            "link",
            "serve",
            "--fleet",
            FLEET,
            "--broker",
            self.sock.to_string_lossy().as_ref(),
            "--cap-file",
            self.cap.to_string_lossy().as_ref(),
            "--state",
            self.state.to_string_lossy().as_ref(),
            "--accept-from",
            &self.node,
        ]
        .map(str::to_string)
        .to_vec()
    }

    /// The broker's argv: `<program> link broker <sock> <live>/b.log --secret-file <secret>`.
    #[must_use]
    pub fn broker_argv(&self) -> Vec<String> {
        [
            self.program.to_string_lossy().as_ref(),
            "link",
            "broker",
            self.sock.to_string_lossy().as_ref(),
            self.live.join("b.log").to_string_lossy().as_ref(),
            "--secret-file",
            self.secret.to_string_lossy().as_ref(),
        ]
        .map(str::to_string)
        .to_vec()
    }

    /// What `fabric.toml` records of this bus, for `aterm link ls` and the other readers of the
    /// rendezvous file ([`aterm_link::enable::Rendezvous`]).
    #[must_use]
    pub fn rendezvous(&self) -> aterm_link::enable::Rendezvous {
        aterm_link::enable::Rendezvous {
            fleet: FLEET.to_string(),
            broker: self.sock.to_string_lossy().into_owned(),
            node: self.node.clone(),
            cap_file: self.cap.to_string_lossy().into_owned(),
            state: self.state.to_string_lossy().into_owned(),
            command: self.command(),
        }
    }
}

/// Lay a bus out: the keys under `keys` (the program link, a 32-byte mint secret, `node.cap`
/// minted from the node ceiling's rules in order ([`astream_cap::GrantCeiling::aterm_node_ring`]),
/// and with `observer` an `observer.cap` from its person ceiling's
/// ([`astream_cap::GrantCeiling::aterm_person_ring`])), the bridge's state with its node id
/// under `live`, each dir 0700 and each secret or cap 0600. Every grant goes through `mint`.
/// Nothing is started.
/// # Errors
/// A file could not be made, `observer` is no person id, a word of the bridge's command is not
/// plain ([`aterm_link::enable::argv_safe`]), or a grant could not be minted (naming it; no cap
/// is written then).
pub fn lay(
    aterm: &Path,
    keys: &Path,
    live: &Path,
    observer: Option<&str>,
    mint: &mut Mint<'_>,
) -> Result<Laid, String> {
    use std::os::unix::fs::PermissionsExt as _;
    if let Some(who) = observer
        && !aterm_link::person::is_person_id(who)
    {
        return Err(format!(
            "the bus's observer {who} is no person id (h-… or a-…)"
        ));
    }
    let state = live.join("state");
    for d in [keys, state.as_path()] {
        crate::util::create_private_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    for d in [keys, live, state.as_path()] {
        crate::util::set_dir_mode_nofollow(d, 0o700)
            .map_err(|e| format!("{}: {e}", d.display()))?;
    }
    let mut random = [0u8; 40];
    aterm_uds::rand::fill(&mut random).map_err(|e| format!("/dev/urandom: {e}"))?;
    let secret = keys.join("mint.secret");
    let write_private = |path: &Path, bytes: &[u8]| -> Result<(), String> {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if !f
            .metadata()
            .map_err(|e| format!("{}: {e}", path.display()))?
            .is_file()
        {
            return Err(format!("{}: not a regular file", path.display()));
        }
        f.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        f.set_len(0)
            .and_then(|()| f.write_all(bytes))
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
    let program = keys.join("aterm");
    std::os::unix::fs::symlink(aterm, &program)
        .map_err(|e| format!("{}: {e}", program.display()))?;
    let mut laid = Laid {
        keys: keys.to_path_buf(),
        cap: keys.join("node.cap"),
        observer: None,
        live: live.to_path_buf(),
        sock: live.join("b.sock"),
        state,
        table: String::new(),
        program,
        secret,
        node,
    };
    let command = laid.command();
    for word in &command {
        aterm_link::enable::argv_safe(word)?;
    }
    laid.table = format!("[fabric]\ncommand = \"{}\"\n", command.join(" "));
    let mut minted = |grants: Vec<String>| -> Result<Vec<u8>, String> {
        let mut caps = Vec::new();
        for grant in grants {
            let argv = [
                laid.program.to_string_lossy().as_ref(),
                "link",
                "mint",
                &grant,
                "--secret-file",
                laid.secret.to_string_lossy().as_ref(),
            ]
            .map(str::to_string);
            caps.extend(mint(&argv).map_err(|e| format!("aterm link mint {grant}: {e}"))?);
        }
        Ok(caps)
    };
    let node_caps = minted(astream_cap::GrantCeiling::aterm_node_ring(FLEET, &laid.node)?.rules())?;
    let observer_caps = observer
        .map(|who| minted(astream_cap::GrantCeiling::aterm_person_ring(FLEET, who)?.rules()))
        .transpose()?;
    write_private(&laid.cap, &node_caps)?;
    if let Some(caps) = observer_caps {
        let path = keys.join("observer.cap");
        write_private(&path, &caps)?;
        laid.observer = Some(path);
    }
    Ok(laid)
}

/// Wait for a broker's socket at `sock`: a hang detector of `hang` ([`LISTEN_HANG`]), never a
/// latency budget.
/// # Errors
/// The broker exited first (`gone`), or `hang` passed: naming what it said (`said`).
pub fn await_listening(
    sock: &Path,
    hang: Duration,
    mut gone: impl FnMut() -> bool,
    said: impl FnOnce() -> String,
) -> Result<(), String> {
    let end = Instant::now() + hang;
    while !sock.exists() {
        if Instant::now() >= end || gone() {
            return Err(format!(
                "the lab's broker never listened: {}",
                said().trim()
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

/// Why [`Fabric::start`] started no bus.
#[derive(Debug, PartialEq, Eq)]
pub enum Unstarted {
    /// The broker refused its lifeline as an aterm older than the lifeline every lab launch is
    /// armed with does ([`unstarted`]).
    NoLifeline,
    /// Any other reason, said.
    Failed(String),
}

/// Why the lifeline-armed broker [`Fabric::start`] waited for never listened: its exit status
/// (`exited`, `None` while it runs or after a signal), what it said (`said`) and the wait's own
/// words (`why`). It is [`Unstarted::NoLifeline`] only for exit 2 with its parser's refusal of
/// [`aterm_uds::lifeline::FLAG`] as an unknown flag and the flag named nowhere else: what a broker
/// that predates the lifeline prints. A current broker names the flag in the usage it prints after
/// every usage error and in its own lifeline errors, so a refusal that names it elsewhere too is a
/// current broker failing, [`Unstarted::Failed`] like every other ending.
fn unstarted(exited: Option<i32>, said: &str, why: String) -> Unstarted {
    let flag = aterm_uds::lifeline::FLAG;
    if exited == Some(2)
        && said.contains(&format!("unknown flag `{flag}`"))
        && said.matches(flag).count() == 1
    {
        Unstarted::NoLifeline
    } else {
        Unstarted::Failed(why)
    }
}

impl From<String> for Unstarted {
    fn from(why: String) -> Self {
        Self::Failed(why)
    }
}

impl Unstarted {
    /// How `verb`, driving `aterm`, ends: NOT RUN (exit 3) for an aterm with no lifeline, else a
    /// failure.
    #[must_use]
    pub fn fail(self, verb: &str, aterm: &Path) -> crate::Fail {
        match self {
            Self::NoLifeline => crate::Fail::exit(
                3,
                format!(
                    "NOT RUN: {verb}: {} refuses {}: it predates the lifeline every lab launch \
                     is armed with",
                    aterm.display(),
                    aterm_uds::lifeline::FLAG
                ),
            ),
            Self::Failed(why) => {
                crate::Fail::die(format!("{verb}: cannot start the lab's bus: {why}"))
            }
        }
    }
}

/// A lab's bus: its broker, the instance's `[fabric]` table and the bridge's state dir.
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
    /// Lay the bus out under `dir` ([`lay`], keys and live dir alike) and start its broker with
    /// `aterm`'s own `link broker`.
    /// # Errors
    /// [`Unstarted::NoLifeline`] for a broker older than its lifeline ([`unstarted`]), else
    /// [`Unstarted::Failed`]: a file could not be made, a grant could not be minted, or the broker
    /// never listened (within a minute: a hang detector).
    pub fn start(aterm: &Path, dir: &Path) -> Result<Fabric, Unstarted> {
        let laid = lay(aterm, dir, dir, None, &mut |argv: &[String]| {
            let out = aterm_uds::spawnfd::command(&argv[0])
                .args(&argv[1..])
                .stdin(Stdio::null())
                .output()
                .map_err(|e| e.to_string())?;
            if out.status.success() {
                Ok(out.stdout)
            } else {
                Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
            }
        })?;
        let said = dir.join("broker.out");
        let log = crate::util::create_stdio_file(&said).map_err(|e| format!("broker.out: {e}"))?;
        let err = log.try_clone().map_err(|e| format!("broker.out: {e}"))?;
        let argv = laid.broker_argv();
        let mut broker = aterm_uds::spawnfd::command(&argv[0]);
        broker
            .args(&argv[1..])
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
            table: laid.table,
            state: laid.state,
        };
        let listened = await_listening(
            &laid.sock,
            LISTEN_HANG,
            || {
                fabric
                    .broker
                    .as_mut()
                    .is_some_and(|b| matches!(b.try_wait(), Ok(Some(_))))
            },
            || crate::util::read_regular_text(&said).unwrap_or_default(),
        );
        if let Err(e) = listened {
            let exited = fabric
                .broker
                .as_mut()
                .and_then(|b| b.try_wait().ok().flatten())
                .and_then(|status| status.code());
            let said = crate::util::read_regular_text(&said).unwrap_or_default();
            fabric.stop();
            return Err(unstarted(exited, &said, e));
        }
        Ok(fabric)
    }

    /// Append [`Fabric::table`] to the `aterm.toml` at `config`.
    /// # Errors
    /// The file could not be opened or written.
    pub fn name_in(&self, config: &Path) -> Result<(), String> {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .append(true)
            .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS)
            .open(config)
            .and_then(|mut f| {
                if !f.metadata()?.is_file() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "not a regular file",
                    ));
                }
                f.write_all(self.table.as_bytes())
            })
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

#[cfg(test)]
#[path = "bus_tests.rs"]
mod tests;
