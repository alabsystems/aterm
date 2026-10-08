// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm fabric on|off|doctor` — the fabric turns on in one command and proves
//! itself.
//!
//! ```text
//! aterm fabric on  [--dry-run] [--fleet <F>] [--service launchd|systemd|none]
//!                  [--tcp <host:port> --key-file <k> [--allow-remote]]
//! aterm fabric off [--dry-run] [--service launchd|systemd|none]
//! aterm fabric doctor
//! ```
//!
//! `on` is `tools/fabric-enable.sh --enable` in Rust, plus three things the
//! script could not do: record where the fabric is so every client finds it
//! without arguments (the rendezvous file), arm the instances already running,
//! and PROVE the result with a message that round-trips through the broker.
//! Every step is idempotent and says whether it changed anything; `--dry-run`
//! prints every step and touches nothing.
//!
//! ## The steps, in order
//!
//! 1. The binary: `aterm` on `$PATH`, else this executable (a development
//!    build's `ATERM_BIN` seam first). It has to carry `link serve` and
//!    `link broker`, and every word of the bridge command is checked for
//!    whitespace, `"` and `\` FIRST: aterm splits `[fabric] command` on
//!    whitespace with no quoting grammar (`fabric_launch.rs`,
//!    `configured_command`), so a path with a space becomes two argv words.
//! 2. The root, `~/.local/share/aterm-fabric`, 0700 (the `$ATERM_FABRIC_HOME`
//!    development seam redirects it). The broker's socket lives in it, and its
//!    directory IS the security boundary: `aterm link broker` checks no
//!    capability and no peer uid. The socket path must fit `sun_path` (104
//!    bytes on macOS, one of them the NUL) and is refused before anything is
//!    written when it does not.
//! 3. The node id, `<root>/link-state/node`: PROVISIONED once, never re-minted.
//!    It is baked into six of the eight grants, so a new id abandons this node's
//!    mail lane. `off` keeps it for the same reason.
//! 4. The mint secret, `<root>/mint.secret`, 32 random bytes, 0600. An existing
//!    short one is refused, never replaced: HMAC accepts any key length, and a
//!    truncated secret mints capabilities that look well-formed and seal nothing.
//! 5. The cap file, `<root>/node.cap`: the eight grants of §8.2's node ring,
//!    minted for the node id ACTUALLY in the state dir — a cap file that names
//!    another id is re-minted. The mint is a TRANSACTION: every grant into a
//!    staging file, then one rename; a refused mint leaves the previous cap
//!    byte-identical and stops before broker supervision is touched.
//!    `crates/aterm-spec`'s `fabric_capability_publication_model` is the model
//!    of that order and `tests/world/fabric_on.rs` runs the real command's
//!    effect trace through it.
//! 6. The broker, supervised: a launchd job on macOS (`KeepAlive`), a
//!    `systemd --user` unit on Linux (`Restart=always`), or `--service none` for
//!    a broker something else keeps alive. THE LABEL IS DERIVED FROM THE ROOT:
//!    the default root keeps the plain `systems.alab.astream-broker`, any other
//!    root gets `.<cksum>` appended, so a run under another root can never boot
//!    out the machine's real broker. "The default root" is the LOGIN user's
//!    (`<passwd home>/.local/share/aterm-fabric`), not `$HOME`'s: launchd's
//!    `gui/<uid>` domain is per uid, so a run under a redirected `$HOME` must
//!    not carry the plain label. Two roots never share a label; an unknown login
//!    home means every root derives one. THE PLIST FOLLOWS THE ROOT TOO:
//!    `~/Library/LaunchAgents/<label>.plist` under the default root,
//!    `<root>/<label>.plist` under any other, so a redirected run leaves no
//!    persistent job in the operator's LaunchAgents dir. `bootstrap` goes by
//!    file path and `bootout` by label, so nothing a redirected run does reaches
//!    the real install. The job is PROBED, not assumed (connect, hello, every
//!    cap attached, the head query); a broker that answers while launchd has no
//!    such job is NOT bootstrapped over (that would put a second broker on the
//!    same socket and log): `on` names its pid and stops. The plist runs the
//!    broker DIRECTLY — ProgramArguments are the words, no `/bin/sh -c`, every
//!    value XML-escaped — and the broker verb unlinks a stale socket itself. An
//!    install carrying the script's `/bin/sh -c "rm -f …; exec …"` argv is read
//!    as CURRENT, not restarted for its spelling.
//! 7. `[fabric] command` in aterm.toml: replacing only the `command` key of an
//!    existing `[fabric]` table, in any spelling TOML allows (`[ "fabric" ]`, a
//!    quoted `"command"`), and leaving everything else alone. The edit is read
//!    back through the parser the app loads the file with and refused, the file
//!    untouched, unless it means exactly that ([`set_fabric_command`]). It is
//!    written as the config service writes it (`ConfigFile`): through a
//!    final-file link, under the service's write lock, never over bytes that
//!    changed since the read, the previous file saved as `.bak` beside it.
//!    `--accept-from <node>` is part of the command: without it a `task`
//!    between two sessions of this node arrives demoted (`kind=note
//!    demoted=task`), which `await inbox` skips by default.
//! 8. The rendezvous file, `fabric.toml` beside the instance control sockets
//!    (`$XDG_RUNTIME_DIR/aterm`, else `~/Library/Application Support/aterm`),
//!    0600: fleet, broker, node, cap file, state dir and the bridge command.
//!    `aterm link ls` defaults its `--fleet --broker` from it, and its
//!    `--cap-file --state` only with the defaulted broker ([`fill_defaults`]);
//!    `aterm fabric` reads its command when aterm.toml has none.
//! 9. Every running instance `aterm ctl instances` lists is armed with
//!    `fabric attach <argv>` — the argv, because `[fabric] command` is read
//!    once, at launch, so a bare attach on an instance launched before step 7
//!    answers `ERR fabric no command`. An instance already supervised with this
//!    command is left alone; one supervised with a DIFFERENT command is named
//!    (a supervisor arms once per process; only a relaunch changes it).
//! 10. The proof: a `note` posted from a session to ITSELF, `--wait` for the
//!     broker's landing, then `await inbox` for its delivery, within 5 s. The
//!     session is the caller's own (`$ATERM_PARENT_SESSION_ID`) when an armed
//!     instance hosts it, else the first session of the first armed instance;
//!     no session is spawned. A failed proof is exit 1; no running instance
//!     means no proof, and the line says so.
//! 11. The undo, spelled WITH the environment that scoped this run (the
//!     `ATERM_FABRIC_HOME` seam, `XDG_CONFIG_HOME`, `XDG_RUNTIME_DIR`, a
//!     development build's `ATERM_BIN`, a `HOME` that is not the login home),
//!     so running it as printed never reaches the real install.
//! 12. `aterm fabric` status, printed last.
//!
//! ## `off`
//!
//! Stops and removes the broker job, removes the `[fabric]` table AND every
//! `[fabric.<sub>]` table under it (with a `.bak`; a subtable left behind
//! re-creates the `fabric` key; the line names each header it removed; a
//! `fabric` it cannot remove, a dotted key or an inline table, FAILS the step,
//! as does a file that is not TOML), removes the rendezvous file — and KEEPS the
//! node id, the secret and the cap: identity is provisioned, never discarded.
//! The bus log stays. A running instance keeps its bridge until it is
//! relaunched, because a supervisor has no stop handle; the next launch starts
//! none.
//!
//! THE ROOT `off` ACTS ON IS THE ONE THE FABRIC IS ON: the `--broker` the
//! `[fabric] command` in aterm.toml dials (else the rendezvous file's), and
//! only then the `$ATERM_FABRIC_HOME` seam or the default — so `on` under one
//! root and `off` under another cannot leave the first root's `KeepAlive`
//! broker running.
//!
//! AND IT NAMES EVERY HELD SESSION FIRST. A fleet halt is lifted only by a
//! bridge that reconnects and reads it withdrawn; once the broker is gone no
//! bridge will, `hold <sid> off` from the Owner token is `ERR denied` for a
//! fleet hold, and the session answers `ERR halted` until its instance is
//! relaunched. `off` cannot lift it (it would be a halt lifted by turning the
//! fabric off), so it says which sessions are held and what does lift them.
//!
//! ## `doctor`
//!
//! `aterm fabric`'s WARNINGS, each with the fix for it ([`fix_for`]), plus the
//! one check status does not make: whether the rendezvous file is there.
//!
//! ## `--tcp <host:port> --key-file <k> [--allow-remote]`
//!
//! The broker is served on the SEALED TCP wire as well as the Unix socket, so
//! a second host can join the fleet (`aterm fabric join`, [`crate::join`]).
//! Only in a `sealed` build ([`crate::transport::SEALED`]); a default build
//! refuses the flags, before anything is written. The steps differ in three
//! ways:
//!
//! * the KEY: an existing `<k>` must be 64 hex characters, mode 0600, and is
//!   never replaced (a joined host holds it); a missing one is minted, 0600;
//! * the BROKER runs `link broker --tcp <bind> --key-file <k> --secret-file
//!   <root>/mint.secret [--allow-remote] --unix <root>/bus.sock <root>/bus.log`
//!   — always GUARDED with the secret the caps are minted under, because the
//!   key is one secret every host holds and only a capability says which node
//!   may publish what — under the SAME root-derived label and plist and
//!   idempotence rules (a job that ran the Unix socket alone is "the job
//!   changed" and restarted); `--service none` probes a broker something else
//!   runs and, when none answers, prints the argv to run;
//! * the broker serves the root's Unix socket AS WELL, and THIS host's bridges
//!   dial that. The TCP port is for joining hosts only: astream admits 64
//!   connections into the sealed handshake at once, and a peer that can reach
//!   the port can hold every one without the key; on the socket, this host's
//!   own bridges, `aterm fabric` and `aterm link ls` never wait on those slots.
//!   The `wire` step proves the sealed wire separately (handshake, grants
//!   attached, a read, at `<bind>` or its loopback twin when `<bind>` is
//!   `0.0.0.0`/`[::]`), and the rendezvous file records it as `serves_tcp` and
//!   `serves_key_file` — what `mint-for` names in the `join` it prints.
//!
//! The port must be FIXED (it is written into every joining host's bridge
//! command), and a non-loopback bind is refused without `--allow-remote`.

use aterm_uds::spawnfd::Stdio;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use crate::ctl::Ctl;
use crate::fabric::{self, kv};
use crate::render::safe;

/// The rendezvous file's name, beside the instance control sockets.
pub const RENDEZVOUS_FILE: &str = "fabric.toml";

/// The fleet `on` joins unless `--fleet` says otherwise — the name
/// `tools/fabric-enable.sh` used.
pub const DEFAULT_FLEET: &str = "local";

/// The launchd label (and systemd unit name) of the broker under the DEFAULT
/// root. Any other root derives its own — see [`Paths::label`].
pub const LABEL: &str = "systems.alab.astream-broker";

/// `sockaddr_un.sun_path` on macOS: 104 bytes, one of them the NUL.
pub const SUN_PATH_MAX: usize = 104;

/// A mint secret is 32 bytes. Shorter is refused (`aterm link mint` refuses
/// the same length, for the same reason).
pub const SECRET_LEN: usize = 32;

/// How long the proof may take: post, landing, delivery.
const PROOF_DEADLINE: Duration = Duration::from_secs(5);

/// How long a freshly started broker may take to bind its socket.
const BIND_DEADLINE: Duration = Duration::from_secs(5);

/// How long a freshly armed instance may take to report `fabric=connected`.
const ARM_DEADLINE: Duration = Duration::from_secs(5);

/// The per-request bound on every control socket this command opens.
const IO_TIMEOUT: Duration = Duration::from_secs(8);

/// TEST-ONLY (a development seam, compiled out of a release build): append one
/// effect word per line (`Mint`, `Reject`, `Publish`, `Supervise`) to this file,
/// so a test can run the real command's effects through the
/// capability-publication model.
const TRACE_ENV: &str = "ATERM_FABRIC_TRACE";

/// TEST-ONLY fault injection (a development seam): refuse the `<n>`th grant's
/// mint, which cannot otherwise be made to fail from outside.
const FAIL_MINT_ENV: &str = "ATERM_FABRIC_FAIL_MINT_AT";

/// TEST-ONLY (a development seam): the fabric root, in place of the login
/// user's `~/.local/share/aterm-fabric`. Every test runs under its own, so the
/// derived label and plist ([`Paths::label`]) keep it off the machine's real
/// broker; a shipped binary has the one root.
const ROOT_SEAM: &str = "ATERM_FABRIC_HOME";

/// [`ROOT_SEAM`], when a build that compiles seams has it set (and not empty).
fn root_seam() -> Option<PathBuf> {
    aterm_types::dev_seam!(ROOT_SEAM)
        .filter(|r| !r.is_empty())
        .map(PathBuf::from)
}

// ---------------------------------------------------------------------------
// options
// ---------------------------------------------------------------------------

/// How the broker is kept alive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    /// A `~/Library/LaunchAgents` job with `KeepAlive` (macOS).
    Launchd,
    /// A `systemd --user` unit with `Restart=always` (Linux).
    Systemd,
    /// Nobody here: the broker is expected to be running already.
    None,
}

impl Service {
    /// Parse the `--service` word.
    ///
    /// # Errors
    ///
    /// A word that is not one of the three.
    pub fn parse(word: &str) -> Result<Self, String> {
        match word {
            "launchd" => Ok(Service::Launchd),
            "systemd" => Ok(Service::Systemd),
            "none" => Ok(Service::None),
            other => Err(format!(
                "--service {}: launchd, systemd or none",
                safe(other, 32)
            )),
        }
    }

    /// The platform's supervisor.
    #[must_use]
    pub fn default_here() -> Self {
        if cfg!(target_os = "macos") {
            Service::Launchd
        } else if cfg!(target_os = "linux") {
            Service::Systemd
        } else {
            Service::None
        }
    }

    fn name(self) -> &'static str {
        match self {
            Service::Launchd => "launchd",
            Service::Systemd => "systemd",
            Service::None => "none",
        }
    }
}

/// `aterm fabric on`'s flags.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OnOpts {
    /// `--dry-run`: print every step, touch nothing.
    pub dry_run: bool,
    /// `--service`; `None` is the platform's default.
    pub service: Option<Service>,
    /// `--fleet`; `None` is [`DEFAULT_FLEET`].
    pub fleet: Option<String>,
    /// `--tcp <bind>`: serve the broker on the SEALED TCP wire at `<host:port>`
    /// as well as the Unix socket (a `sealed` build; module doc).
    pub tcp: Option<String>,
    /// `--key-file <path>`: the pre-shared key for `--tcp` — minted here, 64
    /// hex characters in a 0600 file, when the file does not exist yet.
    pub key_file: Option<String>,
    /// `--allow-remote`: let `--tcp` bind a non-loopback address.
    pub allow_remote: bool,
}

/// `aterm fabric off`'s flags.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OffOpts {
    /// `--dry-run`: print every step, touch nothing.
    pub dry_run: bool,
    /// `--service`; `None` is the platform's default.
    pub service: Option<Service>,
}

// ---------------------------------------------------------------------------
// paths
// ---------------------------------------------------------------------------

/// Where everything is, resolved once.
#[derive(Debug, Clone)]
pub struct Paths {
    /// `$HOME`.
    pub home: PathBuf,
    /// The login user's home from the password database, which is where the
    /// DEFAULT root — the one with the plain [`LABEL`] — lives. `None` when
    /// it cannot be read, and then no root is the default one.
    pub login_home: Option<PathBuf>,
    /// The fabric root.
    pub root: PathBuf,
    /// The aterm binary the bridge and the broker run from.
    pub aterm: String,
    /// The fleet.
    pub fleet: String,
    /// The aterm config file, if `$XDG_CONFIG_HOME`/`$HOME` resolve one.
    pub config: Option<PathBuf>,
    /// The rendezvous file, if the control-socket dir resolves.
    pub rendezvous: Option<PathBuf>,
    /// How this host's bridges reach the broker, and whether this host serves
    /// it: [`Wire::Unix`] unless `on --tcp` or `join` said otherwise.
    pub wire: Wire,
}

/// How this host's bridges reach the broker — and whether THIS host serves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wire {
    /// `<root>/bus.sock`, served here: the one-host fabric.
    Unix,
    /// The SEALED TCP wire: astream's XChaCha20-Poly1305 record
    /// layer under a pre-shared key, to a broker that is always guarded.
    Sealed {
        /// `Some(<host:port>)` when THIS host serves the broker (`on --tcp`):
        /// on the sealed wire at `<bind>` for the hosts that join, and on the
        /// root's `bus.sock` for its own bridges ([`Paths::endpoint`]). `None`
        /// on a host that joined another's (`join`).
        bind: Option<String>,
        /// The sealed endpoint: on the host that serves it, the address its
        /// `wire` probe dials and the rendezvous file records for joiners —
        /// the loopback twin of an unspecified bind
        /// ([`crate::transport::dial_for_bind`]), else the bind itself; on a
        /// joined host, the remote broker its bridges dial.
        dial: String,
        /// The pre-shared key file, absolute.
        key_file: PathBuf,
        /// `--allow-remote` on the broker this host serves.
        allow_remote: bool,
    },
}

impl Paths {
    /// Resolve from the environment.
    ///
    /// # Errors
    ///
    /// No `$HOME`, or no aterm binary anywhere.
    pub fn resolve(fleet: Option<&str>) -> Result<Self, String> {
        let home = std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| "$HOME is not set".to_string())?;
        let root = root_seam().unwrap_or_else(|| default_root(&home));
        // A development seam (`aterm_types::dev_seam!`): a shipped binary installs
        // the `aterm` on PATH, else itself — no environment variable repoints it.
        let seam = aterm_types::dev_seam!("ATERM_BIN")
            .map(|b| b.to_string_lossy().into_owned())
            .filter(|b| !b.trim().is_empty());
        let aterm = match seam {
            Some(b) => b,
            None => match which("aterm") {
                Some(p) => p.to_string_lossy().into_owned(),
                None => std::env::current_exe()
                    .map_err(|e| format!("no aterm on $PATH and no current exe: {e}"))?
                    .to_string_lossy()
                    .into_owned(),
            },
        };
        let fleet = fleet
            .map(str::to_string)
            .filter(|f| !f.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_FLEET.to_string());
        if !crate::subject::is_fleet(&fleet) {
            return Err(format!(
                "fleet {} is not a subject segment ([a-z0-9-]{{1,32}})",
                safe(&fleet, 64)
            ));
        }
        Ok(Self {
            home,
            login_home: login_home(),
            root,
            aterm,
            fleet,
            config: fabric::config_path(),
            rendezvous: rendezvous_path(),
            wire: Wire::Unix,
        })
    }

    /// [`Paths::resolve`] for `off`: the root is the one the fabric IS ON —
    /// the `--broker` the config's `[fabric] command` dials, else the
    /// rendezvous file's — and only with neither the environment's (module
    /// doc, `off`). Answers where the root came from beside the paths.
    ///
    /// # Errors
    ///
    /// As [`Paths::resolve`].
    pub fn resolve_for_off() -> Result<(Self, &'static str), String> {
        let mut p = Self::resolve(None)?;
        let configured = fabric::resolve_command(None, p.config.as_deref())
            .ok()
            .flatten()
            .and_then(|(_, cmd)| fabric::bridge_config(&cmd).ok())
            .and_then(|cfg| root_of(&cfg.transport, &cfg.broker, cfg.cap_files.first()))
            .map(|root| (root, "[fabric] command"));
        let found = configured.or_else(|| {
            Rendezvous::read()
                .ok()
                .flatten()
                .and_then(|r| {
                    let transport = if r.key_file.is_some() {
                        crate::transport::Transport::Tcp
                    } else {
                        crate::transport::Transport::Unix
                    };
                    root_of(&transport, &r.broker, Some(&r.cap_file))
                })
                .map(|root| (root, "the rendezvous file"))
        });
        let Some((root, source)) = found else {
            return Ok((p, "the environment"));
        };
        p.root = root;
        Ok((p, source))
    }

    /// Whether this root is the default one, whose label is the plain
    /// [`LABEL`]: the login user's `~/.local/share/aterm-fabric`, and NOT
    /// `$HOME`'s (module doc, step 6).
    fn default_root(&self) -> bool {
        self.login_home
            .as_deref()
            .is_some_and(|login| self.root == default_root(login))
    }

    /// The launchd label / systemd unit name: [`LABEL`] under the default
    /// root, `LABEL.<cksum of the root>` under any other (module doc, step 6).
    /// A function of the ROOT alone: two roots never share a label.
    #[must_use]
    pub fn label(&self) -> String {
        if self.default_root() {
            LABEL.to_string()
        } else {
            format!("{LABEL}.{}", cksum(self.root.to_string_lossy().as_bytes()))
        }
    }

    fn sock(&self) -> String {
        self.root.join("bus.sock").to_string_lossy().into_owned()
    }
    /// What this host's bridges dial: the root's socket on the Unix wire AND
    /// on the sealed wire this host serves (its broker serves both; the TCP
    /// port is the joining hosts'), the remote broker on a joined host.
    pub(crate) fn endpoint(&self) -> String {
        match &self.wire {
            Wire::Unix | Wire::Sealed { bind: Some(_), .. } => self.sock(),
            Wire::Sealed { dial, .. } => dial.clone(),
        }
    }
    /// The `--tcp --key-file <k>` words a bridge carries on a JOINED host —
    /// none where the bridges dial the socket.
    fn wire_flags(&self) -> Vec<String> {
        match &self.wire {
            Wire::Unix | Wire::Sealed { bind: Some(_), .. } => Vec::new(),
            Wire::Sealed { key_file, .. } => vec![
                "--tcp".to_string(),
                "--key-file".to_string(),
                key_file.to_string_lossy().into_owned(),
            ],
        }
    }
    fn log(&self) -> String {
        self.root.join("bus.log").to_string_lossy().into_owned()
    }
    pub(crate) fn cap(&self) -> PathBuf {
        self.root.join("node.cap")
    }
    pub(crate) fn secret(&self) -> PathBuf {
        self.root.join("mint.secret")
    }
    pub(crate) fn state(&self) -> PathBuf {
        self.root.join("link-state")
    }
    /// Where `join` installs the pre-shared key it was given.
    pub(crate) fn installed_key(&self) -> PathBuf {
        self.root.join("fleet.key")
    }
    /// The launchd job's plist: `~/Library/LaunchAgents/<label>.plist` under
    /// the default root, `<root>/<label>.plist` under any other (module doc,
    /// step 6).
    pub(crate) fn plist(&self) -> PathBuf {
        let name = format!("{}.plist", self.label());
        if self.default_root() {
            self.home.join("Library/LaunchAgents").join(name)
        } else {
            self.root.join(name)
        }
    }
    pub(crate) fn unit(&self) -> PathBuf {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|x| !x.is_empty())
            .map_or_else(|| self.home.join(".config"), PathBuf::from);
        base.join("systemd/user")
            .join(format!("{}.service", self.label()))
    }

    /// Whether the binary is the `aterm-link` shim (a test build) rather than
    /// the front door, which decides the spelling of every command written.
    pub(crate) fn is_link_shim(&self) -> bool {
        Path::new(&self.aterm)
            .file_name()
            .is_some_and(|n| n == "aterm-link")
    }

    /// The bridge command aterm launches: `<aterm> link serve …`, or
    /// `<aterm-link> serve …` for the shim. `--accept-from` carries the node.
    pub(crate) fn bridge_command(&self, node: &str) -> Vec<String> {
        self.bridge_command_accepting(node, &[])
    }

    /// [`Paths::bridge_command`] whose `--accept-from` also lists `others` —
    /// the principals a JOINED host takes tasks from (the first host's node,
    /// say), after its own node. On the sealed wire it carries `--tcp
    /// --key-file <k>` after `--broker`.
    pub(crate) fn bridge_command_accepting(&self, node: &str, others: &[String]) -> Vec<String> {
        let mut argv = vec![self.aterm.clone()];
        if !self.is_link_shim() {
            argv.push("link".to_string());
        }
        argv.extend(
            [
                "serve",
                "--fleet",
                &self.fleet,
                "--broker",
                &self.endpoint(),
            ]
            .iter()
            .map(|s| (*s).to_string()),
        );
        argv.extend(self.wire_flags());
        let mut accept = vec![node.to_string()];
        accept.extend(others.iter().filter(|o| *o != node).cloned());
        argv.extend(
            [
                "--cap-file",
                &self.cap().to_string_lossy(),
                "--state",
                &self.state().to_string_lossy(),
                "--accept-from",
                &accept.join(","),
            ]
            .iter()
            .map(|s| (*s).to_string()),
        );
        argv
    }

    /// The broker's argv as launchd runs it: the words, no shell. On the
    /// sealed wire THIS host serves, `broker --tcp <bind> --key-file <k>
    /// --secret-file <root>/mint.secret [--allow-remote] --unix
    /// <root>/bus.sock <log>` — guarded with the secret the caps were minted
    /// under, and serving the root's socket for this host's own bridges.
    fn broker_argv(&self) -> Vec<String> {
        let mut argv = vec![self.aterm.clone()];
        if !self.is_link_shim() {
            argv.push("link".to_string());
        }
        argv.push("broker".to_string());
        match &self.wire {
            Wire::Sealed {
                bind: Some(bind),
                key_file,
                allow_remote,
                ..
            } => {
                argv.extend([
                    "--tcp".to_string(),
                    bind.clone(),
                    "--key-file".to_string(),
                    key_file.to_string_lossy().into_owned(),
                    "--secret-file".to_string(),
                    self.secret().to_string_lossy().into_owned(),
                ]);
                if *allow_remote {
                    argv.push("--allow-remote".to_string());
                }
                argv.push("--unix".to_string());
                argv.push(self.sock());
                argv.push(self.log());
            }
            _ => argv.extend([self.sock(), self.log()]),
        }
        argv
    }

    /// The argv `tools/fabric-enable.sh` wrote: a shell that unlinks the
    /// socket and execs the broker. An install carrying it is CURRENT (the
    /// broker verb unlinks a stale socket itself) and is not restarted for its
    /// spelling.
    fn legacy_broker_argv(&self) -> Vec<String> {
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            format!("rm -f '{}'; exec {}", self.sock(), self.broker_words()),
        ]
    }

    /// Whether `text`, an installed plist, already runs THIS broker under THIS
    /// label with `KeepAlive` — by what it says, not by its bytes: the direct
    /// argv [`Paths::plist_text`] writes, or the script's shell form, in any
    /// layout (plistlib's tabs and line breaks included).
    #[must_use]
    pub fn plist_is_current(&self, text: &str) -> bool {
        let flat = collapse_between_tags(text);
        let label = format!(
            "<key>Label</key><string>{}</string>",
            xml_escape(&self.label())
        );
        if !flat.contains(&label) || !flat.contains("<key>KeepAlive</key><true/>") {
            return false;
        }
        plist_program_arguments(&flat)
            .is_some_and(|argv| argv == self.broker_argv() || argv == self.legacy_broker_argv())
    }

    /// The broker command a shell-quoted supervisor line runs (the systemd
    /// unit, and the script's legacy plist argv).
    fn broker_words(&self) -> String {
        if matches!(self.wire, Wire::Sealed { .. }) {
            // Every word single-quoted; `argv_safe` already refused a word
            // with a quote in it, so none can close its quote early.
            return self
                .broker_argv()
                .iter()
                .map(|w| format!("'{w}'"))
                .collect::<Vec<_>>()
                .join(" ");
        }
        if self.is_link_shim() {
            format!("'{}' broker '{}' '{}'", self.aterm, self.sock(), self.log())
        } else {
            format!(
                "'{}' link broker '{}' '{}'",
                self.aterm,
                self.sock(),
                self.log()
            )
        }
    }

    /// The launchd job. ProgramArguments are the broker's words, one
    /// `<string>` each — no `/bin/sh -c` — and every value is XML-escaped, so a
    /// `&` or `<` in a path cannot produce a plist launchd cannot parse.
    #[must_use]
    pub fn plist_text(&self) -> String {
        let root = xml_escape(&self.root.to_string_lossy());
        let args: String = self
            .broker_argv()
            .iter()
            .map(|a| format!("    <string>{}</string>\n", xml_escape(a)))
            .collect();
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n\
             <dict>\n\
             \x20 <key>Label</key><string>{label}</string>\n\
             \x20 <key>ProgramArguments</key>\n\
             \x20 <array>\n\
             {args}\
             \x20 </array>\n\
             \x20 <key>RunAtLoad</key><true/>\n\
             \x20 <key>KeepAlive</key><true/>\n\
             \x20 <key>ProcessType</key><string>Background</string>\n\
             \x20 <key>WorkingDirectory</key><string>{root}</string>\n\
             \x20 <key>StandardOutPath</key><string>{root}/broker.out</string>\n\
             \x20 <key>StandardErrorPath</key><string>{root}/broker.err</string>\n\
             </dict>\n\
             </plist>\n",
            label = xml_escape(&self.label()),
        )
    }

    /// The `systemd --user` unit. The stale-socket `rm -f` is the Unix wire's
    /// alone: a TCP broker has no socket file.
    #[must_use]
    pub fn unit_text(&self) -> String {
        let pre = match self.wire {
            Wire::Unix => format!("ExecStartPre=/bin/rm -f {}\n", self.sock()),
            Wire::Sealed { .. } => String::new(),
        };
        format!(
            "[Unit]\n\
             Description=aterm fabric broker ({label})\n\
             \n\
             [Service]\n\
             {pre}\
             ExecStart=/bin/sh -c \"exec {broker}\"\n\
             WorkingDirectory={root}\n\
             Restart=always\n\
             RestartSec=1\n\
             \n\
             [Install]\n\
             WantedBy=default.target\n",
            label = self.label(),
            broker = self.broker_words(),
            root = self.root.to_string_lossy(),
        )
    }
}

fn default_root(home: &Path) -> PathBuf {
    home.join(".local/share/aterm-fabric")
}

/// The fabric ROOT a bridge command (or a rendezvous file) points into: the
/// socket's directory on the Unix wire, and on the sealed wire — whose broker
/// is a `<host:port>` and says nothing about a directory — the cap file's,
/// when it is `<root>/node.cap`, the name `on` and `join` install it under.
/// `None` for anything else: a hand-written command whose files live
/// elsewhere has no root this command may act on.
fn root_of(
    transport: &crate::transport::Transport,
    broker: &str,
    cap_file: Option<&String>,
) -> Option<PathBuf> {
    let root = match transport {
        crate::transport::Transport::Unix => Path::new(broker).parent()?.to_path_buf(),
        _ => {
            let cap = Path::new(cap_file?);
            if cap.file_name()? != "node.cap" {
                return None;
            }
            cap.parent()?.to_path_buf()
        }
    };
    root.is_absolute().then_some(root)
}

/// `&`, `<` and `>` as XML character data: what plistlib does for every value.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// `text` with the whitespace BETWEEN tags dropped, so a plist reads the same
/// whether it was written on one line per key, or with plistlib's tabs.
fn collapse_between_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending = String::new();
    let mut after_close = false;
    for c in text.chars() {
        if after_close && c.is_whitespace() {
            pending.push(c);
            continue;
        }
        if after_close && c != '<' {
            out.push_str(&pending);
        }
        pending.clear();
        after_close = c == '>';
        out.push(c);
    }
    out
}

/// The `<string>` values of a plist's `ProgramArguments` array, unescaped —
/// `None` when the key or its array is not there.
fn plist_program_arguments(flat: &str) -> Option<Vec<String>> {
    let at = flat.find("<key>ProgramArguments</key>")?;
    let rest = &flat[at..];
    let start = rest.find("<array>")? + "<array>".len();
    let end = rest.find("</array>")?;
    let mut block = &rest[start..end];
    let mut out = Vec::new();
    while let Some(s) = block.find("<string>") {
        let after = &block[s + "<string>".len()..];
        let e = after.find("</string>")?;
        out.push(xml_unescape(&after[..e]));
        block = &after[e + "</string>".len()..];
    }
    Some(out)
}

/// What to read when launchd did not bring the socket up: `broker.err` when
/// the job ran and wrote it, else launchd's own view of the job.
fn launchd_failure_hint(root: &Path, label: &str, uid: &str) -> String {
    let err = root.join("broker.err");
    if std::fs::metadata(&err).is_ok_and(|m| m.len() > 0) {
        format!("read {}", err.display())
    } else {
        format!(
            "{} was not written; `launchctl print gui/{uid}/{label}` says why",
            err.display()
        )
    }
}

/// The `off` that undoes a run, spelled with the environment that scoped it —
/// only the variables that are set. `program` is the front door's word
/// (`aterm`, or `aterm-link` for the shim).
fn undo_command(program: &str, scope: &[(&str, Option<String>)]) -> String {
    let mut s = String::new();
    for (name, value) in scope {
        if let Some(v) = value.as_deref().filter(|v| !v.is_empty()) {
            s.push_str(name);
            s.push('=');
            s.push_str(v);
            s.push(' ');
        }
    }
    s.push_str(program);
    s.push_str(" fabric off");
    s
}

/// The login user's home directory FROM THE PASSWORD DATABASE — `~<user>` as
/// `sh` expands it, which POSIX defines through `getpwnam()`, never `$HOME`.
/// `None` when `id -un` or the shell cannot answer, or answers something that
/// is not an absolute path (an unexpanded `~user` is "no such user").
///
/// Two subprocesses rather than a `libc` dependency: this crate counts its
/// third-party packages (Cargo.toml), `uid()` already shells out for the same
/// reason, and it runs once per `on`/`off`.
fn login_home() -> Option<PathBuf> {
    let user = aterm_uds::spawnfd::command("id")
        .arg("-un")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|u| {
            !u.is_empty()
                && u.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.')
        })?;
    let expanded = aterm_uds::spawnfd::command("sh")
        .arg("-c")
        .arg(format!("printf '%s' ~{user}"))
        .env_remove("HOME")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())?;
    let home = Path::new(&expanded);
    (home.is_absolute() && !expanded.starts_with('~')).then(|| home.to_path_buf())
}

/// `<control socket dir>/fabric.toml`.
#[must_use]
pub fn rendezvous_path() -> Option<PathBuf> {
    aterm_uds::control_socket_dir().map(|d| d.join(RENDEZVOUS_FILE))
}

/// The first executable `name` on `$PATH`.
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// POSIX `cksum`'s CRC — the number `tools/fabric-enable.sh` derived a
/// non-default label from (`printf '%s' "$ROOT" | cksum`), reproduced so a job
/// the script installed under a custom root is the job this command finds.
#[must_use]
pub fn cksum(data: &[u8]) -> u32 {
    fn feed(crc: &mut u32, byte: u8) {
        *crc ^= u32::from(byte) << 24;
        for _ in 0..8 {
            *crc = if *crc & 0x8000_0000 != 0 {
                (*crc << 1) ^ 0x04C1_1DB7
            } else {
                *crc << 1
            };
        }
    }
    let mut crc: u32 = 0;
    for &b in data {
        feed(&mut crc, b);
    }
    let mut len = data.len();
    while len != 0 {
        feed(&mut crc, (len & 0xff) as u8);
        len >>= 8;
    }
    !crc
}

/// The eight grants of a node's ring (§8.2), for `node` on `fleet`.
///
/// THE SET IS EXACT: [`crate::join::node_ring_of`] refuses a cap file whose
/// grants are not exactly these, so adding, dropping or rewording one entry
/// invalidates every cap already minted on every host of a fleet.
///
/// That is why the last two stay although nothing this node runs reads
/// `ro:/f/<F>/term/<node>/>` or writes `rw,p=<node>:/f/<F>/term/<node>/*/screen`
/// any more: an older node may still publish on those subjects, the read/write
/// split is the §8.2 property that stops a node forging a driver into its own
/// sessions, and narrowing the ring would refuse every cap in the field.
#[must_use]
pub fn node_grants(fleet: &str, node: &str) -> Vec<String> {
    vec![
        format!("rw,p={node}:/f/{fleet}/pub/{node}/>"),
        format!("ro:/f/{fleet}/pub/>"),
        format!("ro:/f/{fleet}/fleet/>"),
        format!("rw,p={node}:/f/{fleet}/in/*/*/{node}/*"),
        format!("ro:/f/{fleet}/in/{node}/>"),
        format!("rw,p={node}:/f/{fleet}/cur/{node}/>"),
        format!("ro:/f/{fleet}/term/{node}/>"),
        format!("rw,p={node}:/f/{fleet}/term/{node}/*/screen"),
    ]
}

/// Refuse a word aterm's whitespace split would break.
///
/// # Errors
///
/// The word, and why.
pub fn argv_safe(word: &str) -> Result<(), String> {
    if word.chars().any(char::is_whitespace) || word.contains('"') || word.contains('\\') {
        return Err(format!(
            "`{}` contains whitespace, a quote or a backslash; [fabric] command is split on \
             whitespace, so the fabric root, the aterm path and --fleet must each be one plain \
             word",
            safe(word, 256)
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// the rendezvous file
// ---------------------------------------------------------------------------

/// What `fabric.toml` records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendezvous {
    /// The fleet.
    pub fleet: String,
    /// The broker's Unix socket.
    pub broker: String,
    /// This machine's node id.
    pub node: String,
    /// The cap file.
    pub cap_file: String,
    /// The bridge's state dir.
    pub state: String,
    /// The whole bridge command, as `[fabric] command` has it.
    pub command: String,
    /// The pre-shared key file when this host's bridges dial the SEALED TCP
    /// wire (`join`); `None` for the Unix socket — which is what they dial on
    /// the host that serves the sealed wire, too.
    pub key_file: Option<String>,
    /// On the host that SERVES the sealed wire (`on --tcp`): the endpoint a
    /// joining host dials (the bind, or its loopback twin) — what `mint-for`
    /// names in the `join` it prints, and what `aterm fabric` shows beside the
    /// socket. `None` everywhere else.
    pub serves_tcp: Option<String>,
    /// With [`Rendezvous::serves_tcp`]: the pre-shared key file that wire is
    /// sealed under — the file a joining host is given a copy of.
    pub serves_key_file: Option<String>,
}

impl Rendezvous {
    /// Render the file.
    #[must_use]
    pub fn render(&self) -> String {
        let mut wire = self.key_file.as_ref().map_or_else(String::new, |k| {
            format!(
                "transport = \"tcp+sealed\"\n\
                 key_file = {}\n",
                toml_str(k)
            )
        });
        if let Some(tcp) = &self.serves_tcp {
            wire.push_str(&format!("serves_tcp = {}\n", toml_str(tcp)));
        }
        if let Some(k) = &self.serves_key_file {
            wire.push_str(&format!("serves_key_file = {}\n", toml_str(k)));
        }
        format!(
            "# Written by `aterm fabric on` — where this machine's fabric is. `aterm link\n\
             # ls` and `aterm fabric` read it when their flags are omitted;\n\
             # `aterm fabric off` removes it. Not the source of truth: aterm launches its\n\
             # bridge from `[fabric] command` in aterm.toml, which `command` below mirrors.\n\
             fleet = {}\n\
             broker = {}\n\
             node = {}\n\
             cap_file = {}\n\
             state = {}\n\
             command = {}\n\
             {wire}",
            toml_str(&self.fleet),
            toml_str(&self.broker),
            toml_str(&self.node),
            toml_str(&self.cap_file),
            toml_str(&self.state),
            toml_str(&self.command),
        )
    }

    /// Parse one.
    ///
    /// # Errors
    ///
    /// Not TOML, or a key missing or not a string.
    pub fn parse(text: &str) -> Result<Self, String> {
        let table: aterm_toml::Table =
            aterm_toml::from_str(text).map_err(|e| format!("not valid TOML: {e}"))?;
        let get = |k: &str| -> Result<String, String> {
            table
                .get(k)
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .ok_or_else(|| format!("`{k}` is missing or not a string"))
        };
        // `key_file` and the `serves_*` pair are OPTIONAL — absent is the Unix
        // socket, and a host that serves no sealed wire — but present and not
        // a string is as malformed as any required key.
        let optional = |k: &str| -> Result<Option<String>, String> {
            match table.get(k) {
                None => Ok(None),
                Some(v) => v
                    .as_str()
                    .map(|s| Some(s.to_string()))
                    .ok_or_else(|| format!("`{k}` is not a string")),
            }
        };
        Ok(Self {
            fleet: get("fleet")?,
            broker: get("broker")?,
            node: get("node")?,
            cap_file: get("cap_file")?,
            state: get("state")?,
            command: get("command")?,
            key_file: optional("key_file")?,
            serves_tcp: optional("serves_tcp")?,
            serves_key_file: optional("serves_key_file")?,
        })
    }

    /// The rendezvous file on this machine: `Ok(None)` when there is none.
    ///
    /// # Errors
    ///
    /// A file that exists and cannot be read or parsed, naming it.
    pub fn read() -> Result<Option<Self>, String> {
        let Some(path) = rendezvous_path() else {
            return Ok(None);
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text)
                .map(Some)
                .map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }
}

/// A TOML basic string.
fn toml_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\u{:04X}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `args` with `--fleet --broker --cap-file --state` filled from the rendezvous
/// file for each one that is absent — `--cap-file`/`--state` only with the
/// broker ([`fill_defaults`]). No file, or an unreadable one, leaves
/// `args` as given (the parser's own "required" refusal then names the flags,
/// except for `ls` with neither flag and no file, which `cli::dispatch` answers
/// first); an unreadable one is said on stderr first.
#[must_use]
pub fn with_rendezvous_defaults(args: &[String]) -> Vec<String> {
    let r = match Rendezvous::read() {
        Ok(Some(r)) => r,
        Ok(None) => return args.to_vec(),
        Err(e) => {
            eprintln!(
                "aterm-link: the rendezvous file is unreadable, so no flag is defaulted: {e}"
            );
            return args.to_vec();
        }
    };
    fill_defaults(args, &r)
}

/// [`with_rendezvous_defaults`]'s pure half: `args` with `--fleet` and
/// `--broker` prepended from `r` when absent, and the rest of the node's
/// flags with the broker.
///
/// EVERYTHING BOUND TO THE BROKER COMES WITH IT, as one unit: only when
/// `--broker` is itself defaulted are `--cap-file <cap>` and `--state
/// <state>` defaulted (each unless given) and, from a rendezvous file that
/// names a `key_file`, `--tcp --key-file <k>`. A `--broker` the caller
/// spelled out gets none of them:
///
/// - the cap is minted against the rendezvous broker's secret, so presenting
///   it to a broker the caller named hands that broker this node's
///   credential — and the state holds the node identity the cap was minted
///   for (owner ruling 2026-09-29);
/// - a socket path handed `--tcp` would be dialed as a `<host>:<port>`.
///
/// `--fleet` is a name, not a credential, and is defaulted either way.
#[must_use]
pub fn fill_defaults(args: &[String], r: &Rendezvous) -> Vec<String> {
    let has = |flag: &str| args.iter().any(|a| a == flag);
    let with_broker = !has("--broker");
    let mut out = Vec::with_capacity(args.len() + 11);
    for (flag, value, bound_to_broker) in [
        ("--fleet", &r.fleet, false),
        ("--broker", &r.broker, false),
        ("--cap-file", &r.cap_file, true),
        ("--state", &r.state, true),
    ] {
        if !has(flag) && (with_broker || !bound_to_broker) {
            out.push(flag.to_string());
            out.push(value.clone());
        }
    }
    if let (Some(key), true) = (&r.key_file, with_broker) {
        if !has("--tcp") {
            out.push("--tcp".to_string());
        }
        if !has("--key-file") {
            out.push("--key-file".to_string());
            out.push(key.clone());
        }
    }
    out.extend_from_slice(args);
    out
}

// ---------------------------------------------------------------------------
// aterm.toml's [fabric] table
// ---------------------------------------------------------------------------

/// `repr` read as a TOML key path by the parser itself, so `fabric`,
/// `"fabric"`, `'fabric'` and ` fabric . presence ` all name what the app
/// reads; `None` when it is not a key.
fn key_path(repr: &str) -> Option<Vec<String>> {
    let keys = aterm_toml::edit::Key::parse(repr.trim()).ok()?;
    Some(keys.iter().map(|k| k.get().to_string()).collect())
}

/// The table header on `line` — `[fabric]`, `[ "fabric" . presence ]`,
/// `[[fabric.peers]]` — as its text (comment dropped) and key path; `None` for
/// any other line. The header ends at the first close bracket whose text
/// before it is a key and after it is nothing or a comment: a `]` or `#`
/// inside a quoted segment leaves an unterminated string before it.
fn table_header(line: &str) -> Option<(&str, Vec<String>)> {
    let t = line.trim_start();
    let (open, close) = if t.starts_with("[[") {
        ("[[", "]]")
    } else {
        ("[", "]")
    };
    let inner = t.strip_prefix(open)?;
    inner.match_indices(close).find_map(|(at, _)| {
        let rest = inner[at + close.len()..].trim_start();
        if !(rest.is_empty() || rest.starts_with('#')) {
            return None;
        }
        let path = key_path(&inner[..at])?;
        Some((&t[..open.len() + at + close.len()], path))
    })
}

/// The key path a `key = value` line sets (`command`, `"command"`,
/// `fabric.command`); `None` for any other line.
fn line_key(line: &str) -> Option<Vec<String>> {
    line.match_indices('=')
        .find_map(|(at, _)| key_path(&line[..at]))
}

/// Every `fabric` table in `lines` — `[fabric]`, each `[fabric.<sub>]` and
/// `[[fabric.<sub>]]`, in any spelling TOML allows — as `(header, path,
/// start, end)`, in file order.
fn fabric_table_spans<'a>(lines: &[&'a str]) -> Vec<(&'a str, Vec<String>, usize, usize)> {
    let mut out = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let Some((h, path)) = table_header(l) else {
            continue;
        };
        if path.first().is_none_or(|k| k != "fabric") {
            continue;
        }
        let end = lines[i + 1..]
            .iter()
            .position(|l| table_header(l).is_some())
            .map_or(lines.len(), |n| i + 1 + n);
        out.push((h, path, i, end));
    }
    out
}

/// Whether `line` starts the `command` key of a table.
fn is_command_line(line: &str) -> bool {
    line_key(line).is_some_and(|k| k == ["command"])
}

/// `edited` read back through the parser the app loads aterm.toml with, held
/// to what the edit meant: `text`'s meaning with `fabric` applied to it. The
/// line editors here know every spelling of a header and a key, not the rest
/// of TOML's grammar — a header-shaped line inside a multi-line string fools
/// them — so nothing they produce is written unless it reads back as meant.
/// Both editors refuse a `text` that does not parse before they edit it.
///
/// # Errors
///
/// Why it does not read back: `text` or `edited` is not TOML, or `edited`
/// means something else.
fn reads_back(
    text: &str,
    edited: &str,
    fabric: impl FnOnce(&mut aterm_toml::Table),
) -> Result<(), String> {
    let mut want: aterm_toml::Table =
        aterm_toml::from_str(text).map_err(|e| format!("not valid TOML: {e}"))?;
    // The parse error is not named: its line is in text that never existed.
    let after: aterm_toml::Table = aterm_toml::from_str(edited)
        .map_err(|_| "the edited file would not be valid TOML".to_string())?;
    fabric(&mut want);
    if want == after {
        Ok(())
    } else {
        Err("the edited file would not read back as meant".to_string())
    }
}

/// The line range `[at, end)` of the `command = …` value starting at `lines[at]`:
/// one line, or up to the closing `"""`/`'''` of a multi-line string.
fn command_value_end(lines: &[&str], at: usize) -> usize {
    let line = lines[at];
    let value = line.split_once('=').map_or("", |(_, v)| v.trim_start());
    for quote in ["\"\"\"", "'''"] {
        if let Some(rest) = value.strip_prefix(quote) {
            if rest.contains(quote) {
                return at + 1;
            }
            return lines[at + 1..]
                .iter()
                .position(|l| l.contains(quote))
                .map_or(lines.len(), |i| at + 2 + i);
        }
    }
    at + 1
}

/// `text` with `[fabric] command` set to `cmd`: the `command` key of an
/// existing table replaced (every other key and table kept), or a new table
/// appended. `false` when the file already says exactly that.
///
/// AND `receipts = true` WRITTEN ONCE, when the table has no `receipts` key
/// at all, so a fresh `aterm fabric on` acks senders out of the box. A key the
/// operator has set — either way — is never touched, which is what makes
/// `receipts = false` a setting rather than a race with the next `on`.
///
/// # Errors
///
/// A file that is not valid TOML (it is left alone, and the error named), a
/// `fabric` written as a dotted key or an inline table at the top level — this
/// editor only knows the `[fabric]` table form, and appending a second
/// definition would make the file invalid TOML — or an edit that does not
/// read back as exactly this change (`reads_back`); the message says to
/// edit it by hand.
pub fn set_fabric_command(text: &str, cmd: &str) -> Result<(String, bool), String> {
    // A file the app itself could not parse is not edited line by line into a
    // file it still cannot parse: the refusal names the parse error instead.
    let receipts_missing = fabric::receipts_in_toml(text)?.is_none();
    if let Some(current) = fabric::command_in_toml(text)? {
        if current == cmd && !receipts_missing {
            return Ok((text.to_string(), false));
        }
    }
    let lines: Vec<&str> = text.lines().collect();
    // The line range `[start, end)` of the `[fabric]` table itself.
    let table = fabric_table_spans(&lines)
        .into_iter()
        .find_map(|(_, path, start, end)| (path == ["fabric"]).then_some((start, end)));
    if table.is_none() && has_dotted_fabric(&lines) {
        return Err(
            "aterm.toml defines `fabric` as a dotted key or an inline table, not a `[fabric]` \
             table; edit its `command` by hand (or rewrite it as a [fabric] table) and run \
             this again"
                .to_string(),
        );
    }
    let new_line = format!("command = {}", toml_str(cmd));
    let receipts_line = "receipts = true";
    let mut out: Vec<String> = Vec::with_capacity(lines.len() + 5);
    match table {
        Some((start, end)) => {
            out.extend(lines[..=start].iter().map(|l| (*l).to_string()));
            out.push(new_line);
            if receipts_missing {
                out.push(receipts_line.to_string());
            }
            let mut i = start + 1;
            while i < end {
                if is_command_line(lines[i]) {
                    i = command_value_end(&lines, i);
                } else {
                    out.push(lines[i].to_string());
                    i += 1;
                }
            }
            out.extend(lines[end..].iter().map(|l| (*l).to_string()));
        }
        None => {
            out.extend(lines.iter().map(|l| (*l).to_string()));
            if out.iter().any(|l| !l.trim().is_empty()) {
                while out.last().is_some_and(|l| l.trim().is_empty()) {
                    out.pop();
                }
                out.push(String::new());
            }
            out.push("[fabric]".to_string());
            out.push("# Written by `aterm fabric on`. Remove with `aterm fabric off`.".to_string());
            out.push(new_line);
            out.push(receipts_line.to_string());
        }
    }
    let mut joined = out.join("\n");
    joined.push('\n');
    reads_back(text, &joined, |t| {
        use aterm_toml::Value;
        let mut f = match t.remove("fabric") {
            Some(Value::Table(f)) => f,
            _ => aterm_toml::Table::new(),
        };
        f.insert("command".to_string(), Value::String(cmd.to_string()));
        if f.get("receipts").is_none() {
            f.insert("receipts".to_string(), Value::Boolean(true));
        }
        t.insert("fabric".to_string(), Value::Table(f));
    })
    .map_err(|why| {
        format!(
            "{why}: aterm.toml is left as it was; set `command = {}` in its [fabric] table by \
             hand and run this again",
            toml_str(cmd)
        )
    })?;
    Ok((joined, true))
}

/// Whether the top level spells `fabric` as `fabric.command = …` or
/// `fabric = { … }`, in any key spelling — forms [`set_fabric_command`] does
/// not edit.
fn has_dotted_fabric(lines: &[&str]) -> bool {
    lines
        .iter()
        .take_while(|l| table_header(l).is_none())
        .any(|l| line_key(l).is_some_and(|k| k.first().is_some_and(|k| k == "fabric")))
}

/// `text` with the `[fabric]` table AND every `[fabric.<sub>]` table removed,
/// every other table kept, and the headers removed in file order — empty when
/// there was none. A subtable left behind would re-create the `fabric` key.
///
/// # Errors
///
/// A file that is not valid TOML (it is left alone, and the error named:
/// which lines are fabric's cannot be read in it — a malformed `[keys` ends
/// no table — so a removal could take the person's own lines with it), or a
/// result that still defines `fabric` (a dotted `fabric.` key or an inline
/// table at the top level, which this editor does not remove) or does not
/// read back as `text` without it (`reads_back`) — `off` must not call a
/// fabric it left configured absent.
pub fn remove_fabric_table(text: &str) -> Result<(String, Vec<String>), String> {
    let table: aterm_toml::Table = aterm_toml::from_str(text).map_err(|e| {
        // The instruction before the error, whose snippet runs to several lines.
        format!(
            "not valid TOML, so aterm.toml is left as it was; fix it (or remove its [fabric] \
             table by hand) and run this again: {e}"
        )
    })?;
    let lines: Vec<&str> = text.lines().collect();
    let spans = fabric_table_spans(&lines);
    if spans.is_empty() {
        if table.get("fabric").is_some() {
            return Err(
                "aterm.toml defines `fabric` as a dotted key or an inline table, which this \
                 editor does not remove; remove it by hand and run this again"
                    .to_string(),
            );
        }
        return Ok((text.to_string(), Vec::new()));
    }
    let removed: Vec<String> = spans.iter().map(|(h, ..)| (*h).to_string()).collect();
    let mut out: Vec<&str> = Vec::with_capacity(lines.len());
    let mut at = 0;
    for (_, _, start, end) in spans {
        out.extend_from_slice(&lines[at..start]);
        at = end;
    }
    out.extend_from_slice(&lines[at..]);
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    let joined = if out.is_empty() {
        String::new()
    } else {
        let mut joined = out.join("\n");
        joined.push('\n');
        joined
    };
    reads_back(text, &joined, |t| {
        t.remove("fabric");
    })
    .map_err(|why| {
        format!(
            "{why}: aterm.toml is left as it was; remove its [fabric] table and every \
             `fabric.` key by hand and run this again"
        )
    })?;
    Ok((joined, removed))
}

// ---------------------------------------------------------------------------
// files, written the careful way
// ---------------------------------------------------------------------------

/// Write `bytes` to `path` with `mode`: a temp beside it, fsync, rename.
///
/// THE TEMP IS CREATED, NEVER OPENED ([`create_temp_beside`]): the operator
/// names `path` (`mint-for --out`, `on --key-file`), and it can sit in a
/// directory other users write, so a symlink planted at a predictable temp
/// name must not be written through. The name is unpredictable, the open
/// refuses anything already there, and the mode is set on the descriptor.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    write_atomic_checked(path, bytes, mode, || Ok(()))
}

/// [`write_atomic`], with `check` run between the temp's fsync and the rename:
/// an `Err` from it removes the temp and leaves `path` as it was.
fn write_atomic_checked(
    path: &Path,
    bytes: &[u8],
    mode: u32,
    check: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let (tmp, mut f) = create_temp_beside(path, mode)?;
    let result = (|| {
        f.write_all(bytes)?;
        // `create` with a mode is masked by the umask; the rename keeps
        // whatever the temp got, so say the mode again — on the DESCRIPTOR,
        // which is the file this process created and nothing else.
        f.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(mode))?;
        f.sync_all()?;
        drop(f);
        check()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// A NEW file beside `path` to write it through: `.<name>.tmp.<pid>.<8 hex>`,
/// opened `O_CREAT|O_EXCL` (`create_new`) with `mode`. The exclusive create
/// fails on anything already at the name — a symlink included, dangling or
/// not (POSIX `open(2)`: with `O_CREAT` and `O_EXCL`, a symbolic link is not
/// followed) — so nothing planted there is ever written through; a clash is
/// retried under fresh random hex, a few times, then refused.
///
/// # Errors
///
/// No entropy for the name, a clash on every try, or the create's own error.
fn create_temp_beside(path: &Path, mode: u32) -> io::Result<(PathBuf, std::fs::File)> {
    use std::os::unix::fs::OpenOptionsExt;
    let name = path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    for _ in 0..8 {
        let salt = aterm_uds::rand::hex_token::<4>()?;
        let tmp = path.with_file_name(format!(".{name}.tmp.{}.{salt}", std::process::id()));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&tmp)
        {
            Ok(f) => return Ok((tmp, f)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!(
            "every temporary name tried beside {} was already taken",
            path.display()
        ),
    ))
}

/// aterm.toml, read for one edit and written the way the config service
/// writes a Settings save (`aterm-gui`'s `native_document_host`: read by
/// `read_config_atomic_file` with no lock, then `commit_atomic_bytes` under
/// it), so `on`/`off` and the window are two writers of one file rather than
/// two files:
///
/// - A FINAL-FILE LINK IS FOLLOWED, NOT REPLACED. aterm.toml may be a link into
///   a dotfiles checkout, a layout the service binds and keeps. The text, the
///   `.bak` and the rename all land at the file the link names; a rename over
///   the link itself (what [`write_atomic`] does to a cap or key file, on
///   purpose) would turn it into a local copy every later save went to.
/// - THE SERVICE'S WRITE LOCK IS TAKEN FOR A WRITE, NEVER A READ
///   ([`Self::lock`]): `.<target name>.aterm-write.lock` beside the target,
///   opened no-follow, `flock` exclusive. A run that changes nothing creates
///   and holds nothing, so a no-op through a link into a directory nobody may
///   write (home-manager's into `/nix/store`) still reads `already`. A
///   Settings save that meets the lock retries for about half a second — its
///   25 ms lock budget inside a 500 ms preflight retry — and then fails as
///   busy, so the hold is the write alone: the `.bak` and the text, never the
///   parse or a line printed to a pipe nobody drains.
/// - NOTHING IS WRITTEN OVER BYTES THAT CHANGED since they were read: checked
///   under the lock before the `.bak` (an older `.bak` is not replaced for an
///   edit that never lands) and again just before the rename, so a
///   writer that ignores the lock (an editor) makes the step refuse rather
///   than have its change reverted.
///
/// A dry run only reads.
struct ConfigFile {
    /// The file the text lives in: the config path, or what a link there names.
    target: PathBuf,
    /// Its bytes and mode when read; `None` when there was no file.
    before: Option<(Vec<u8>, u32)>,
}

/// A [`ConfigFile`] under the config service's write lock, for one
/// [`Self::replace`]; the lock is let go when that returns.
struct ConfigEdit<'a> {
    file: &'a ConfigFile,
    _lock: ConfigWriteLock,
}

/// How long `on`/`off` wait for the config service's write lock. The window
/// holds it for one save, whose two `F_FULLFSYNC`s were measured at
/// 82.5-279.7 ms on a busy volume (`native_document_host`'s
/// `PREFLIGHT_RETRY_BUDGET`), so this is several of the longest holds; past
/// it the step refuses and says to run again.
const CONFIG_LOCK_WAIT: Duration = Duration::from_secs(2);

/// `O_NOFOLLOW | O_NONBLOCK` (std adds `O_CLOEXEC`), the flags the config
/// service opens its lock with: a link planted at the lock's name is refused,
/// not followed, and a FIFO is not waited on.
#[cfg(target_vendor = "apple")]
const LOCK_OPEN_FLAGS: i32 = 0x0100 | 0x0004;
/// arm and arm64 Linux spell `O_NOFOLLOW` 0o100000 (`aterm_uds::ownerlock`).
#[cfg(all(target_os = "linux", any(target_arch = "aarch64", target_arch = "arm")))]
const LOCK_OPEN_FLAGS: i32 = 0o100_000 | 0o4000;
#[cfg(all(
    target_os = "linux",
    not(any(target_arch = "aarch64", target_arch = "arm"))
))]
const LOCK_OPEN_FLAGS: i32 = 0o400_000 | 0o4000;
/// No audited values: the pre-open check and the descriptor check below
/// still refuse anything at the lock's name that is not a regular file.
#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
const LOCK_OPEN_FLAGS: i32 = 0;

/// The config service's write lock on one config file, let go by `LOCK_UN`
/// before the close (`clippy.toml`'s flock release rule): a child forked
/// while it is held keeps a copy of the descriptor until it execs, and a lock
/// released by the close alone would stay taken on that copy.
struct ConfigWriteLock(std::fs::File);

impl ConfigWriteLock {
    /// Take the lock beside `target`, waiting up to [`CONFIG_LOCK_WAIT`].
    fn take(target: &Path) -> io::Result<Self> {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let name = target
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let path = target.with_file_name(format!(".{name}.aterm-write.lock"));
        let not_a_file = || {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "the config write lock {} is not a regular file",
                    path.display()
                ),
            )
        };
        if std::fs::symlink_metadata(&path).is_ok_and(|m| !m.file_type().is_file()) {
            return Err(not_a_file());
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(LOCK_OPEN_FLAGS)
            .open(&path)?;
        if !file.metadata()?.is_file() {
            return Err(not_a_file());
        }
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        let deadline = Instant::now() + CONFIG_LOCK_WAIT;
        let mut backoff = Duration::from_millis(1);
        loop {
            #[expect(
                clippy::disallowed_methods,
                reason = "the guard's own take: its Drop releases by LOCK_UN"
            )]
            let taken = file.try_lock();
            match taken {
                Ok(()) => return Ok(Self(file)),
                Err(std::fs::TryLockError::Error(e)) => return Err(e),
                Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(backoff);
                    backoff = (backoff * 2).min(Duration::from_millis(50));
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        format!(
                            "another writer holds {} (a Settings save); run this again",
                            path.display()
                        ),
                    ))
                }
            }
        }
    }
}

impl Drop for ConfigWriteLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

impl ConfigFile {
    /// Read `config` for an edit. No lock: a read changes nothing.
    ///
    /// # Errors
    ///
    /// A link at `config` that names no file (it is not written through to a
    /// file nobody made), or the read's own error.
    fn open(config: &Path) -> io::Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        let target = match std::fs::symlink_metadata(config) {
            Ok(m) if m.file_type().is_symlink() => std::fs::canonicalize(config).map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!(
                        "a link to {}, which does not resolve ({e})",
                        std::fs::read_link(config)
                            .map_or_else(|_| "?".into(), |t| t.display().to_string())
                    ),
                )
            })?,
            _ => config.to_path_buf(),
        };
        let before = match std::fs::read(&target) {
            Ok(bytes) => {
                let mode = std::fs::metadata(&target)?.permissions().mode() & 0o777;
                Some((bytes, mode))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        Ok(Self { target, before })
    }

    /// The text as read; `None` when there was no file.
    ///
    /// # Errors
    ///
    /// A file that is not UTF-8.
    fn text(&self) -> io::Result<Option<&str>> {
        self.before
            .as_ref()
            .map(|(bytes, _)| {
                std::str::from_utf8(bytes)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
            })
            .transpose()
    }

    /// Take the config service's write lock for a write, waiting up to
    /// [`CONFIG_LOCK_WAIT`]; the target's directory is made first (a first
    /// `on` writes a file nobody made).
    ///
    /// # Errors
    ///
    /// The lock held past the wait or not takeable, or the directory's own
    /// error.
    fn lock(&self) -> io::Result<ConfigEdit<'_>> {
        if let Some(dir) = self.target.parent() {
            std::fs::create_dir_all(dir)?;
        }
        Ok(ConfigEdit {
            file: self,
            _lock: ConfigWriteLock::take(&self.target)?,
        })
    }

    /// `Ok` while the target holds the bytes [`Self::open`] read.
    ///
    /// # Errors
    ///
    /// It changed — or appeared, or went — since; or the read's own error.
    fn unchanged(&self) -> io::Result<()> {
        let now = match std::fs::read(&self.target) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        if now.as_deref() == self.before.as_ref().map(|(b, _)| b.as_slice()) {
            Ok(())
        } else {
            Err(io::Error::other(
                "aterm.toml changed while fabric was editing it; run again",
            ))
        }
    }
}

impl ConfigEdit<'_> {
    /// Replace the text, keeping what was read as `<target>.bak` (only when
    /// something was there), and answer the `.bak`. An existing file keeps its
    /// own mode — an operator's 0644 aterm.toml is not silently made private —
    /// and a new one is 0600.
    ///
    /// # Errors
    ///
    /// The target's bytes changed since [`ConfigFile::open`] read them —
    /// found before the `.bak` is written, or before the rename — or a
    /// write's own error; in every case the target is left as it is.
    fn replace(self, text: &str) -> io::Result<Option<PathBuf>> {
        let file = self.file;
        file.unchanged()?;
        let (bak, mode) = match &file.before {
            Some((old, mode)) => {
                let mut bak = file.target.clone().into_os_string();
                bak.push(".bak");
                let bak = PathBuf::from(bak);
                write_atomic(&bak, old, *mode)?;
                (Some(bak), *mode)
            }
            None => (None, 0o600),
        };
        write_atomic_checked(&file.target, text.as_bytes(), mode, || file.unchanged())?;
        Ok(bak)
    }
}

/// `n` random bytes through the ONE audited entropy helper
/// (`aterm_uds::rand`): a hand-rolled device read is the incident the B4 guard
/// exists for.
pub(crate) fn random_bytes(n: usize) -> io::Result<Vec<u8>> {
    let mut buf = vec![0u8; n];
    aterm_uds::rand::fill(&mut buf)?;
    Ok(buf)
}

/// The node id a bridge with this state dir answers as, if one is provisioned.
pub(crate) fn read_node(state: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(state.join("node")).ok()?;
    let node = raw.trim();
    crate::join::is_node_id(node).then(|| node.to_string())
}

/// Whether the cap file at `path` was minted for `node`: readable, and at
/// least one grant carries `p=<node>:`.
fn cap_is_for(path: &Path, node: &str) -> Result<usize, String> {
    let caps = crate::bridge::read_cap_file(&path.to_string_lossy()).map_err(|e| e.to_string())?;
    let mark = format!("p={node}:");
    if caps.iter().any(|c| c.grant.contains(&mark)) {
        Ok(caps.len())
    } else {
        Err(format!("minted for a different node id, not {node}"))
    }
}

/// TEST-ONLY effect trace (module doc). A write that fails is ignored: the
/// trace is evidence for a test, never a step of the command.
fn trace(word: &str) {
    if let Some(path) = aterm_types::dev_seam!(TRACE_ENV) {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)
        {
            let _ = writeln!(f, "{word}");
        }
    }
}

/// Mint the eight grants into `<root>/node.cap` as ONE transaction (module
/// doc, step 5). Answers the grant count.
///
/// # Errors
///
/// A short secret, a refused mint, or a staging/rename failure — in every case
/// the previous cap file is untouched and the staging file is gone.
fn mint_caps(p: &Paths, node: &str) -> Result<usize, String> {
    let secret_path = p.secret();
    let key = std::fs::read(&secret_path).map_err(|e| format!("{}: {e}", secret_path.display()))?;
    if key.len() < SECRET_LEN {
        trace("Reject");
        return Err(format!(
            "{} holds {} bytes; a mint secret is at least {SECRET_LEN} (a short key mints a \
             capability that seals nothing) — move it aside and run again",
            secret_path.display(),
            key.len()
        ));
    }
    let fail_at: Option<usize> =
        aterm_types::dev_seam!(FAIL_MINT_ENV).and_then(|v| v.to_str()?.parse().ok());
    let cap = p.cap();
    let staged = p.root.join(format!(".node.cap.{}", std::process::id()));
    let result = (|| -> Result<usize, String> {
        let mut lines = String::new();
        let grants = node_grants(&p.fleet, node);
        for (i, grant) in grants.iter().enumerate() {
            let minted = if fail_at == Some(i + 1) {
                Err("injected mint refusal".to_string())
            } else {
                astream_cap::mint(&key, grant).map_err(|e| e.to_string())
            };
            match minted {
                Ok(c) => {
                    lines.push_str(&astream_cap::capfile::format_line(&c.filter, &c.tag));
                    lines.push('\n');
                    trace("Mint");
                }
                Err(e) => {
                    trace("Reject");
                    return Err(format!(
                        "capability mint failed on `{grant}`: {e}; {} was left unchanged and \
                         broker supervision was not touched",
                        cap.display()
                    ));
                }
            }
        }
        write_atomic(&staged, lines.as_bytes(), 0o600)
            .map_err(|e| format!("cannot stage a capability in {}: {e}", p.root.display()))?;
        std::fs::rename(&staged, &cap)
            .map_err(|e| format!("cannot publish the capability at {}: {e}", cap.display()))?;
        trace("Publish");
        Ok(grants.len())
    })();
    let _ = std::fs::remove_file(&staged);
    result
}

// ---------------------------------------------------------------------------
// the report
// ---------------------------------------------------------------------------

/// The step lines, printed as they happen.
pub(crate) struct Out {
    pub(crate) dry_run: bool,
    pub(crate) changed: usize,
    pub(crate) failed: bool,
}

impl Out {
    pub(crate) fn new(dry_run: bool) -> Self {
        Self {
            dry_run,
            changed: 0,
            failed: false,
        }
    }

    pub(crate) fn line(&self, name: &str, verdict: &str, detail: &str) {
        let mut out = io::stdout().lock();
        let _ = writeln!(out, "  {name:<11} {verdict:<9} {detail}");
        let _ = out.flush();
    }

    /// Nothing to do: it was already so.
    pub(crate) fn already(&self, name: &str, detail: &str) {
        self.line(name, "already", detail);
    }

    /// Something was (or, dry, would be) done. An empty `did` prints `would`
    /// rather than an empty detail.
    pub(crate) fn done(&mut self, name: &str, would: &str, did: &str) {
        self.changed += 1;
        if self.dry_run {
            self.line(name, "would", would);
        } else {
            self.line(name, "done", if did.is_empty() { would } else { did });
        }
    }

    /// Something the operator has to know that is not this run's failure.
    pub(crate) fn warn(&self, name: &str, detail: &str) {
        self.line(name, "WARNING", detail);
    }

    pub(crate) fn ok(&self, name: &str, detail: &str) {
        self.line(name, "ok", detail);
    }

    pub(crate) fn fail(&mut self, name: &str, detail: &str) {
        self.failed = true;
        self.line(name, "FAILED", detail);
    }

    pub(crate) fn note(&self, name: &str, detail: &str) {
        self.line(name, "-", detail);
    }
}

/// Step 7 of `on` and `join`: `[fabric] command` in aterm.toml set to `command`
/// ([`set_fabric_command`]), written as the config service writes it
/// ([`ConfigFile`]) with the previous file kept as a `.bak` — `already` when
/// the file says it, and a dry run writes nothing. `false` stops the run.
fn config_step(config: Option<&Path>, command: &str, out: &mut Out) -> bool {
    let Some(config) = config else {
        out.fail(
            "config",
            "no config path: neither $XDG_CONFIG_HOME nor $HOME is set",
        );
        return false;
    };
    let fail = |out: &mut Out, e: &dyn std::fmt::Display| {
        out.fail("config", &format!("{}: {e}", config.display()));
        false
    };
    let file = match ConfigFile::open(config) {
        Ok(file) => file,
        Err(e) => return fail(out, &e),
    };
    let (text, existed) = match file.text() {
        Ok(text) => (text.unwrap_or(""), text.is_some()),
        Err(e) => return fail(out, &e),
    };
    let new_text = match set_fabric_command(text, command) {
        Ok((_, false)) => {
            out.already(
                "config",
                &format!("[fabric] command in {} is this command", config.display()),
            );
            return true;
        }
        Ok((new_text, true)) => new_text,
        Err(e) => return fail(out, &e),
    };
    let (word, did) = if matches!(fabric::command_in_toml(text), Ok(Some(_))) {
        ("update", "updated")
    } else {
        ("add", "added")
    };
    out.done(
        "config",
        &format!(
            "{word} [fabric] command in {}{}",
            config.display(),
            if existed {
                " (previous saved as .bak)"
            } else {
                ""
            }
        ),
        "",
    );
    if out.dry_run {
        return true;
    }
    // The lock is let go when `replace` returns, before the line is printed.
    match file.lock().and_then(|edit| edit.replace(&new_text)) {
        Ok(bak) => {
            out.ok(
                "config",
                &format!(
                    "{did} [fabric] command in {}{}",
                    config.display(),
                    bak.map_or_else(String::new, |b| format!(
                        " (previous saved as {})",
                        b.display()
                    ))
                ),
            );
            true
        }
        Err(e) => fail(out, &e),
    }
}

/// Step 2 of `on` and `join`: the fabric root and its state directory, both 0700 — `already`
/// when both exist, else created (a dry run writes nothing). `false` stops the run.
pub(crate) fn root_step(p: &Paths, state: &Path, out: &mut Out) -> bool {
    if p.root.is_dir() && state.is_dir() {
        out.already("root", &format!("{} (0700)", p.root.display()));
        return true;
    }
    out.done(
        "root",
        &format!("create {} and {} (0700)", p.root.display(), state.display()),
        &format!("created {} (0700)", p.root.display()),
    );
    if !out.dry_run {
        if let Err(e) = std::fs::create_dir_all(state).and_then(|()| {
            for d in [p.root.as_path(), state] {
                std::fs::set_permissions(d, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
            }
            Ok(())
        }) {
            out.fail("root", &format!("{}: {e}", state.display()));
            return false;
        }
    }
    true
}

/// Record `node` into `<state>/node` (a dry run writes nothing) — `on`'s minted id and
/// `join`'s id from the cap alike. `false` stops the run.
pub(crate) fn record_node(state: &Path, node: &str, out: &mut Out) -> bool {
    if !out.dry_run {
        let write =
            crate::state::StateDir::open(state).and_then(|s| s.node_id(|| node.to_string()));
        if let Err(e) = write {
            out.fail("node", &format!("{}/node: {e}", state.display()));
            return false;
        }
    }
    true
}

/// Whether the binary carries `link` — `<aterm> link` prints the bridge's
/// usage, which names `aterm-link`; the shim prints it with no argument.
///
/// And on the sealed wire, whether it CARRIES the sealed transport: that
/// usage ends with the build line (`cli.rs`'s `build_line!`), and a default
/// build's says it has none. Written into a bridge command anyway, a
/// default-build binary would dial nothing but `Unsupported` —
/// `fabric=stalled` for ever, with the reason in a log.
pub(crate) fn binary_has_link(p: &Paths) -> Result<(), String> {
    if !is_executable(Path::new(&p.aterm)) {
        return Err(format!(
            "no executable at {} (install aterm on PATH)",
            safe(&p.aterm, 256)
        ));
    }
    let mut cmd = aterm_uds::spawnfd::command(&p.aterm);
    if !p.is_link_shim() {
        cmd.arg("link");
    }
    let out = cmd
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{}: {e}", safe(&p.aterm, 256)))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if !text.contains("aterm-link") {
        Err(format!(
            "{} has no `link` verb; update aterm",
            safe(&p.aterm, 256)
        ))
    } else if matches!(p.wire, Wire::Sealed { .. }) && !text.contains(crate::cli::SEALED_BUILD_MARK)
    {
        Err(format!(
            "{}: {}; put that build first on PATH",
            safe(&p.aterm, 256),
            crate::transport::SEALED_UNAVAILABLE
        ))
    } else {
        Ok(())
    }
}

/// Reach the broker for real, through the report's own probe; `procs` is the
/// process table that names its pid.
pub(crate) fn probe(
    p: &Paths,
    node: &str,
    procs: impl FnOnce() -> Vec<fabric::Proc>,
) -> fabric::BrokerView {
    let cmd = p.bridge_command(node).join(" ");
    match fabric::bridge_config(&cmd) {
        // The process table only where there is a broker config to match it
        // against: a `ps` spawn an unparsable command would throw away.
        Ok(cfg) => fabric::probe_broker(&cfg, &procs()).0,
        Err(e) => fabric::BrokerView {
            endpoint: p.endpoint(),
            transport: if matches!(p.wire, Wire::Sealed { bind: None, .. }) {
                "tcp+sealed"
            } else {
                "unix"
            },
            error: Some(e),
            ..fabric::BrokerView::default()
        },
    }
}

/// On the sealed wire THIS host serves: the TCP listener answers too, with
/// the key — the handshake, the node's own grants attached and a read, at the
/// endpoint a joining host dials. The broker step probes the SOCKET (what
/// this host's bridges use), so without this nothing in `on` would notice a
/// broker whose TCP listener never came up. `true` elsewhere.
fn wire_step(p: &Paths, out: &mut Out) -> bool {
    let Wire::Sealed {
        bind: Some(_),
        dial,
        key_file,
        ..
    } = &p.wire
    else {
        return true;
    };
    let key_path = key_file.to_string_lossy();
    let key = match crate::transport::read_private_key_file(&key_path) {
        Ok(k) => k,
        Err(e) => {
            out.fail("wire", &crate::transport::describe(&key_path, &e));
            return false;
        }
    };
    let cfg = crate::bridge::Config {
        fleet: p.fleet.clone(),
        broker: dial.clone(),
        transport: crate::transport::Transport::Sealed(Box::new(key)),
        cap_files: vec![p.cap().to_string_lossy().into_owned()],
        state_dir: String::new(),
        accept_from: Vec::new(),
        sock: None,
        token: None,
        presence: crate::presence::Mode::Meta,
        receipts: false,
    };
    let (view, _) = fabric::probe_broker(&cfg, &[]);
    if view.read_ok() {
        out.ok(
            "wire",
            &format!(
                "the sealed TCP wire answers at {dial} ({} ms), for joining hosts",
                view.rtt_ms.unwrap_or(0)
            ),
        );
        true
    } else {
        out.fail(
            "wire",
            &format!(
                "the broker answers on {} but not on the sealed wire at {dial}: {}",
                p.sock(),
                safe(view.error.as_deref().unwrap_or("no answer"), 256)
            ),
        );
        false
    }
}

/// Wait for the socket file to appear, then for the broker behind it to answer.
/// The process table (a `ps -A`) is read once, when it answers — not on every
/// 100 ms retry.
fn wait_for_broker(p: &Paths, node: &str) -> Result<fabric::BrokerView, String> {
    let started = Instant::now();
    loop {
        let mut view = probe(p, node, Vec::new);
        if view.read_ok() {
            view.locate(&fabric::process_table());
            return Ok(view);
        }
        if started.elapsed() > BIND_DEADLINE {
            return Err(view.error.unwrap_or_else(|| "no answer".to_string()));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub(crate) fn uid() -> String {
    aterm_uds::spawnfd::command("id")
        .arg("-u")
        .stdin(Stdio::null())
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "501".to_string())
}

/// `launchctl <args>`; `Ok(())` on exit 0.
pub(crate) fn launchctl(args: &[&str]) -> Result<(), String> {
    let out = aterm_uds::spawnfd::command("launchctl")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("launchctl: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "launchctl {} exited {}: {}",
            args.join(" "),
            out.status.code().unwrap_or(-1),
            safe(String::from_utf8_lossy(&out.stderr).trim(), 256)
        ))
    }
}

/// `launchctl list`'s `PID  Status  Label` rows; empty when it cannot run.
pub(crate) fn launchctl_list() -> String {
    aterm_uds::spawnfd::command("launchctl")
        .arg("list")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// The job `label` in `launchctl list`: `None` when launchd has no such job,
/// `Some(pid)` when it has — the pid only while the job runs.
fn launchd_job(label: &str) -> Option<Option<u32>> {
    launchctl_list().lines().find_map(|line| {
        let mut cols = line.split_whitespace();
        let pid = cols.next()?;
        let _status = cols.next()?;
        (cols.next()? == label).then(|| pid.parse().ok())
    })
}

pub(crate) fn launchd_loaded(label: &str) -> bool {
    launchd_job(label).is_some()
}

pub(crate) fn systemctl(args: &[&str]) -> Result<(), String> {
    let out = aterm_uds::spawnfd::command("systemctl")
        .arg("--user")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("systemctl: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "systemctl --user {} exited {}: {}",
            args.join(" "),
            out.status.code().unwrap_or(-1),
            safe(String::from_utf8_lossy(&out.stderr).trim(), 256)
        ))
    }
}

/// Step 6: the broker, supervised. `Ok(true)` when it answers.
fn broker_step(p: &Paths, node: &str, service: Service, out: &mut Out) -> bool {
    trace("Supervise");
    let sock = p.endpoint();
    let label = p.label();
    match service {
        Service::None => {
            let view = probe(p, node, fabric::process_table);
            if view.read_ok() {
                out.already(
                    "broker",
                    &format!(
                        "answers on {sock} (unmanaged: --service none){}",
                        view.pid
                            .map_or_else(String::new, |pid| format!(", pid {pid}"))
                    ),
                );
                true
            } else {
                out.fail(
                    "broker",
                    &format!(
                        "no broker answers on {sock} ({}) and --service none starts none: run \
                         `{}` yourself, or drop --service none",
                        safe(view.error.as_deref().unwrap_or("-"), 256),
                        p.broker_argv().join(" ")
                    ),
                );
                false
            }
        }
        Service::Launchd => {
            let plist = p.plist();
            let text = p.plist_text();
            let same = std::fs::read_to_string(&plist).is_ok_and(|old| p.plist_is_current(&old));
            let running = launchd_job(&label).flatten();
            let view = probe(p, node, fabric::process_table);
            if same && running.is_some() && view.read_ok() {
                out.already("plist", &plist.display().to_string());
                out.already(
                    "broker",
                    &format!(
                        "launchd {label} pid {} answers on {sock}",
                        running.unwrap_or(0)
                    ),
                );
                return true;
            }
            // A BROKER LAUNCHD DOES NOT MANAGE (a hand-run `aterm link
            // broker`, or a job under another label): bootstrapping over it
            // would start a second broker on the same socket AND bus log.
            if running.is_none() && view.read_ok() {
                out.fail(
                    "broker",
                    &format!(
                        "an unmanaged broker (pid {}) answers on {sock}; stop it and run `on` \
                         again, or keep it with `on --service none`",
                        view.pid
                            .map_or_else(|| "-".to_string(), |pid| pid.to_string())
                    ),
                );
                return false;
            }
            if same {
                out.already("plist", &plist.display().to_string());
            } else {
                out.done(
                    "plist",
                    &format!("write {}", plist.display()),
                    &format!("wrote {}", plist.display()),
                );
                if !out.dry_run {
                    if let Err(e) = write_atomic(&plist, text.as_bytes(), 0o644) {
                        out.fail("plist", &format!("{}: {e}", plist.display()));
                        return false;
                    }
                }
            }
            let why = if running.is_none() {
                "not loaded".to_string()
            } else if !same {
                "the job changed".to_string()
            } else {
                format!(
                    "loaded but not answering ({})",
                    safe(view.error.as_deref().unwrap_or("-"), 128)
                )
            };
            out.done(
                "broker",
                &format!("(re)start launchd {label} ({why}) and wait for {sock}"),
                &format!("(re)started launchd {label} ({why})"),
            );
            if out.dry_run {
                return true;
            }
            let domain = format!("gui/{}/{label}", uid());
            let _ = launchctl(&["bootout", &domain]);
            let plist_s = plist.to_string_lossy().into_owned();
            if let Err(first) = launchctl(&["bootstrap", &format!("gui/{}", uid()), &plist_s]) {
                if let Err(second) = launchctl(&["load", &plist_s]) {
                    out.fail("broker", &format!("{first}; {second}"));
                    return false;
                }
            }
            match wait_for_broker(p, node) {
                Ok(view) => {
                    out.ok(
                        "broker",
                        &format!(
                            "launchd {label} pid {} answers on {sock} ({} ms)",
                            launchd_job(&label)
                                .flatten()
                                .or(view.pid)
                                .map_or_else(|| "-".to_string(), |pid| pid.to_string()),
                            view.rtt_ms.unwrap_or(0)
                        ),
                    );
                    true
                }
                Err(e) => {
                    out.fail(
                        "broker",
                        &format!(
                            "launchd {label} did not bring {sock} up: {e} — {}",
                            launchd_failure_hint(&p.root, &label, &uid())
                        ),
                    );
                    false
                }
            }
        }
        Service::Systemd => {
            let unit = p.unit();
            let text = p.unit_text();
            let same = std::fs::read(&unit).is_ok_and(|old| old == text.as_bytes());
            let view = probe(p, node, fabric::process_table);
            if same && view.read_ok() {
                out.already(
                    "broker",
                    &format!("systemd --user {label} answers on {sock}"),
                );
                return true;
            }
            if same {
                out.already("unit", &unit.display().to_string());
            } else {
                out.done(
                    "unit",
                    &format!("write {}", unit.display()),
                    &format!("wrote {}", unit.display()),
                );
                if !out.dry_run {
                    if let Err(e) = write_atomic(&unit, text.as_bytes(), 0o644) {
                        out.fail("unit", &format!("{}: {e}", unit.display()));
                        return false;
                    }
                }
            }
            out.done(
                "broker",
                &format!("systemctl --user daemon-reload; enable --now {label}; wait for {sock}"),
                &format!("systemd --user {label} (re)started"),
            );
            if out.dry_run {
                return true;
            }
            // `enable` then `restart`, not `enable --now` (which would start the
            // unit twice): restart starts a stopped unit too.
            let steps: [&[&str]; 3] = [
                &["daemon-reload"],
                &["enable", &label],
                &["restart", &label],
            ];
            for args in steps {
                if let Err(e) = systemctl(args) {
                    out.fail("broker", &e);
                    return false;
                }
            }
            match wait_for_broker(p, node) {
                Ok(view) => {
                    out.ok(
                        "broker",
                        &format!(
                            "systemd --user {label} answers on {sock} ({} ms)",
                            view.rtt_ms.unwrap_or(0)
                        ),
                    );
                    true
                }
                Err(e) => {
                    out.fail(
                        "broker",
                        &format!(
                            "systemd --user {label} did not bring {sock} up: {e} — \
                             `journalctl --user -u {label}`"
                        ),
                    );
                    false
                }
            }
        }
    }
}

/// One running instance, as `on` sees it. It keeps no token: each connection
/// to it reads the one beside its socket once connected ([`Ctl::open`]), so
/// an update's successor at the same socket is asked with its own.
struct Instance {
    pid: u32,
    sock: String,
    supervised: bool,
    state: String,
    command: Option<String>,
}

/// Every instance the rendezvous walk lists that answered `fabric status`, and
/// how many listed instances are running: those, plus each live one that
/// could not be asked (its line says why). `None` when the walk itself failed,
/// so nobody can say none is running.
fn instances(out: &mut Out) -> (Vec<Instance>, Option<usize>) {
    let listed = match aterm_ctl::local_instances() {
        Ok(l) => l,
        Err(e) => {
            out.note(
                "instances",
                &format!("cannot be listed ({}); none armed", safe(&e.reason, 256)),
            );
            return (Vec::new(), None);
        }
    };
    let mut found = Vec::new();
    let mut unasked = 0;
    for (pid, sock) in listed {
        match ask_instance(pid, sock) {
            Ok(inst) => found.push(inst),
            Err(Some(why)) => {
                out.note("instance", &why);
                unasked += 1;
            }
            Err(None) => {}
        }
    }
    let running = found.len() + unasked;
    (found, Some(running))
}

/// Ask the instance `pid` listening on `sock` for its `fabric status`. `Err`
/// is an instance that could not be asked: `Some` the line that says why, for
/// a live one; `None` for a listing whose process is gone.
fn ask_instance(pid: u32, sock: String) -> Result<Instance, Option<String>> {
    let (stream, token) = match aterm_ctl::connect_then_read_token(&sock) {
        Ok(connected) => connected,
        Err(e) => {
            return Err(aterm_uds::process::pid_alive(pid).then(|| {
                format!(
                    "{pid}: {} does not answer ({e}); not armed",
                    safe(&sock, 256)
                )
            }));
        }
    };
    let token = match token {
        Ok(t) => t,
        Err(e) => {
            return Err(aterm_uds::process::pid_alive(pid).then(|| {
                format!(
                    "{pid}: no Owner token ({}); not armed",
                    safe(&e.to_string(), 128)
                )
            }));
        }
    };
    let mut ctl = Ctl::authenticate(stream, &token).map_err(|e| {
        aterm_uds::process::pid_alive(pid).then(|| {
            format!(
                "{pid}: {} does not answer ({e}); not armed",
                safe(&sock, 256)
            )
        })
    })?;
    let _ = ctl.get_ref().set_read_timeout(Some(IO_TIMEOUT));
    let _ = ctl.get_ref().set_write_timeout(Some(IO_TIMEOUT));
    let reply = match ctl.request("fabric status") {
        Ok(r) if r.ok() => r,
        Ok(r) => {
            return Err(Some(format!(
                "{pid}: fabric status answered {}",
                safe(r.header(), 128)
            )));
        }
        Err(e) => return Err(Some(format!("{pid}: fabric status: {e}"))),
    };
    let h = reply.header();
    Ok(Instance {
        pid,
        sock,
        supervised: kv(h, "supervised") == Some("1"),
        state: kv(h, "state").unwrap_or("-").to_string(),
        command: kv(h, "command")
            .filter(|c| *c != "-")
            .map(crate::pct::decode),
    })
}

/// Step 9: arm every running instance. Answers how many are running (`None`
/// when they could not be listed), and the ones that carry this command
/// (armed now or before), in the order they were listed.
fn arm_instances(argv: &[String], out: &mut Out) -> (Option<usize>, Vec<Instance>) {
    let ours = fabric::serve_flags(&argv.join(" "));
    let (found, running) = instances(out);
    if running == Some(0) {
        out.note(
            "instances",
            "none running (a later launch reads [fabric] command itself)",
        );
    }
    let mut armed = Vec::new();
    for mut inst in found {
        let pid = inst.pid;
        if inst.supervised {
            if inst.command.as_deref().and_then(fabric::serve_flags) == ours {
                out.already(
                    "instance",
                    &format!(
                        "{pid} is armed with this command (fabric={})",
                        safe(&inst.state, 16)
                    ),
                );
                armed.push(inst);
            } else {
                out.note(
                    "instance",
                    &format!(
                        "{pid} is armed with a different command ({}); relaunch that aterm to \
                         change it",
                        safe(inst.command.as_deref().unwrap_or("-"), 256)
                    ),
                );
            }
            continue;
        }
        out.done(
            "instance",
            &arm_command(pid, &inst.sock, argv),
            &format!("armed {pid}"),
        );
        if out.dry_run {
            // Listed as armed so the dry run's PROOF line names the session it
            // would post through, rather than a skip — the instance is
            // untouched either way.
            armed.push(inst);
            continue;
        }
        let Ok(mut ctl) = Ctl::open(&inst.sock) else {
            out.fail("instance", &format!("{pid}: the control socket went away"));
            continue;
        };
        let _ = ctl.get_ref().set_read_timeout(Some(IO_TIMEOUT));
        let line = format!("fabric attach {}", argv.join(" "));
        match ctl.request(&line) {
            Ok(r) if r.ok() => {}
            Ok(r) => {
                out.fail(
                    "instance",
                    &format!("{pid}: fabric attach answered {}", safe(r.header(), 256)),
                );
                continue;
            }
            Err(e) => {
                out.fail("instance", &format!("{pid}: fabric attach: {e}"));
                continue;
            }
        }
        // The bridge connects asynchronously; wait for the instance to say so.
        let started = Instant::now();
        let (state, reason) = loop {
            let header = ctl
                .request("fabric status")
                .ok()
                .map(|r| r.header().to_string())
                .unwrap_or_default();
            let state = kv(&header, "state").unwrap_or("-").to_string();
            let reason = kv(&header, "reason").unwrap_or("-").to_string();
            if state == "connected" || started.elapsed() > ARM_DEADLINE {
                break (state, reason);
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        if state == "connected" {
            out.ok(
                "instance",
                &format!(
                    "{pid} fabric=connected after {} ms",
                    started.elapsed().as_millis()
                ),
            );
            inst.supervised = true;
            inst.state = state;
            armed.push(inst);
        } else if state == "stalled" && reason == "starting" {
            // Attached, and the bridge has said nothing about its link: an
            // older bridge never does, and reads `connected` only at the first
            // record it delivers. The proof is that record, so it decides.
            out.note(
                "instance",
                &format!(
                    "{pid} armed; its bridge has not reported its broker link after \
                     {ARM_DEADLINE:?} — the proof decides"
                ),
            );
            inst.supervised = true;
            inst.state = state;
            armed.push(inst);
        } else if state == "stalled" {
            out.fail(
                "instance",
                &format!(
                    "{pid} armed, but its broker link is down after {ARM_DEADLINE:?} \
                     (reason={})",
                    safe(&reason, 16)
                ),
            );
        } else {
            out.fail(
                "instance",
                &format!(
                    "{pid} armed, but its bridge did not connect within {ARM_DEADLINE:?} \
                     (fabric={})",
                    safe(&state, 16)
                ),
            );
        }
    }
    (running, armed)
}

/// The proof line when nothing is armed to post through. Launching aterm is
/// the fix only when the walk listed none running; otherwise (an instance that
/// could not be asked, one armed with another command, a failed arm or
/// bridge, a walk that failed) the lines above say why.
fn proof_skipped(running: Option<usize>) -> &'static str {
    if running == Some(0) {
        "skipped: no aterm is running — launch one, then run this again"
    } else {
        "skipped: no running aterm has a connected bridge for this command (see above)"
    }
}

/// Every `(instance pid, socket, sid, origin)` whose `status` says `hold=1`,
/// across the instances the rendezvous walk lists — the socket only when
/// `--pid` does not reach it ([`aterm_ctl::pid_reaches`]), the origin read off
/// the session's `timeline` ([`fabric::last_hold`]), `None` when it is no
/// longer there. An instance that cannot be asked is skipped silently: `off`
/// names what it can see, and the `instances` line at its end says what is
/// running.
fn held_sessions() -> Vec<(u32, Option<String>, String, Option<String>)> {
    let mut held = Vec::new();
    for (pid, sock) in aterm_ctl::local_instances().unwrap_or_default() {
        let Ok(Some(mut ctl)) = fabric::open_instance(pid, &sock, IO_TIMEOUT) else {
            continue;
        };
        for sid in session_sids(&mut ctl) {
            let is_held = ctl
                .request(&format!("@{sid} status"))
                .ok()
                .filter(crate::ctl::Reply::ok)
                .is_some_and(|r| kv(r.header(), "hold") == Some("1"));
            if is_held {
                let origin = ctl
                    .request(&format!("@{sid} timeline"))
                    .ok()
                    .filter(crate::ctl::Reply::ok)
                    .and_then(|t| fabric::last_hold(t.rows()))
                    .map(|(_, origin)| origin);
                let explicit = (!aterm_ctl::pid_reaches(pid, &sock)).then(|| sock.clone());
                held.push((pid, explicit, sid, origin));
            }
        }
    }
    held
}

/// The sessions one instance hosts, in its own order.
fn sessions_of(inst: &Instance) -> Vec<String> {
    let Ok(mut ctl) = Ctl::open(&inst.sock) else {
        return Vec::new();
    };
    let _ = ctl.get_ref().set_read_timeout(Some(IO_TIMEOUT));
    session_sids(&mut ctl)
}

/// The `s-` session ids the instance behind `ctl` lists, in its own order;
/// none when it cannot be asked.
fn session_sids(ctl: &mut Ctl) -> Vec<String> {
    ctl.request("sessions")
        .ok()
        .filter(crate::ctl::Reply::ok)
        .map(|r| {
            fabric::session_ids(r.rows())
                .filter(|s| s.starts_with("s-"))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Step 10: the proof. `Ok(ms)` for a round trip that completed.
fn prove(armed: &[Instance], out: &mut Out) -> Result<u64, String> {
    // The caller's own session when an armed instance hosts it, else the first
    // session of the first armed instance. No session is spawned.
    let own = std::env::var("ATERM_PARENT_SESSION_ID").ok();
    let mut choice: Option<(&Instance, String)> = None;
    for inst in armed {
        let sids = sessions_of(inst);
        if let Some(o) = own.as_deref() {
            if sids.iter().any(|s| s == o) {
                choice = Some((inst, o.to_string()));
                break;
            }
        }
        if choice.is_none() {
            if let Some(first) = sids.first() {
                choice = Some((inst, first.clone()));
            }
        }
    }
    let Some((inst, sid)) = choice else {
        return Err("no session on any armed instance to post through".to_string());
    };
    if out.dry_run {
        out.done(
            "proof",
            &format!(
                "post a note from @{sid} (instance {}) to itself and wait up to {PROOF_DEADLINE:?} \
                 for it to come back through the broker",
                inst.pid
            ),
            "",
        );
        return Ok(0);
    }
    let mut ctl = Ctl::open(&inst.sock).map_err(|e| format!("{}: {e}", inst.sock))?;
    let _ = ctl.get_ref().set_read_timeout(Some(IO_TIMEOUT));
    let _ = ctl.get_ref().set_write_timeout(Some(IO_TIMEOUT));
    let ask = |ctl: &mut Ctl, line: &str| -> Result<crate::ctl::Reply, String> {
        let r = ctl.request(line).map_err(|e| format!("`{line}`: {e}"))?;
        if r.ok() {
            Ok(r)
        } else {
            Err(format!("`{line}` answered {}", safe(r.header(), 256)))
        }
    };
    // The newest row id before the post, so `await inbox` latches on the
    // delivery and on nothing older.
    let before = ask(&mut ctl, &format!("@{sid} inbox 1 --peek --meta"))?;
    let since: u64 = before
        .rows()
        .iter()
        .filter(|r| r.starts_with("msg "))
        .filter_map(|r| r.split_whitespace().nth(1)?.parse().ok())
        .max()
        .unwrap_or(0);
    let nonce = format!("{:x}", crate::now_ms());
    let started = Instant::now();
    let wait_ms = PROOF_DEADLINE.as_millis();
    let posted = ask(
        &mut ctl,
        &format!("@{sid} post to=@{sid} kind=note --wait={wait_ms} aterm fabric on: proof {nonce}"),
    )
    .map_err(|e| {
        format!(
            "the post did not land on the bus: {e}; `aterm fabric` says whether the broker \
             answers"
        )
    })?;
    let off = kv(posted.header(), "off")
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or_else(|| {
            format!(
                "the post answered {} without a bus offset",
                safe(posted.header(), 128)
            )
        })?;
    let left = PROOF_DEADLINE
        .saturating_sub(started.elapsed())
        .as_millis()
        .max(1);
    ask(
        &mut ctl,
        &format!("@{sid} await inbox since={since} kinds=note timeout={left}"),
    )
    .map_err(|e| {
        format!(
            "landed at @{off} but was not delivered back within {PROOF_DEADLINE:?}: {e}; check \
             the cap file's `ro:/f/<F>/in/<node>/>` grant and `--accept-from`"
        )
    })?;
    let rows = ask(&mut ctl, &format!("@{sid} inbox --peek --meta"))?;
    let delivered = rows.rows().iter().any(|r| {
        r.starts_with("msg ")
            && kv(r, "off") == Some(&off.to_string())
            && kv(r, "kind") == Some("note")
    });
    if !delivered {
        return Err(format!(
            "landed at @{off}, but another sender's note was delivered first; run `on` again"
        ));
    }
    let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    out.ok(
        "proof",
        &format!(
            "a note from @{sid} to itself landed at @{off} and came back through the broker in \
             {ms} ms (left in its inbox as a note)"
        ),
    );
    Ok(ms)
}

// ---------------------------------------------------------------------------
// on
// ---------------------------------------------------------------------------

/// `on --tcp <bind> --key-file <k> [--allow-remote]`, validated into the
/// [`Wire`] THIS host serves — before anything is written. `Ok(Wire::Unix)`
/// with neither flag.
///
/// # Errors
///
/// A usage-level refusal (exit 2): one flag without the other, a default build
/// ([`crate::transport::SEALED_UNAVAILABLE`]), a bind that does not resolve or
/// has no fixed port (the port is written into every bridge command, so an
/// ephemeral one is refused), or a non-loopback bind without
/// `--allow-remote`.
pub fn wire_for_on(opts: &OnOpts) -> Result<Wire, String> {
    let (bind, key) = match (&opts.tcp, &opts.key_file) {
        (None, None) => {
            if opts.allow_remote {
                return Err(
                    "--allow-remote needs --tcp <bind> (a Unix socket is never remote)".to_string(),
                );
            }
            return Ok(Wire::Unix);
        }
        (Some(_), None) => {
            return Err(
                "--tcp needs --key-file: the broker serves TCP only sealed (a missing file is \
                 minted there, 0600)"
                    .to_string(),
            );
        }
        (None, Some(_)) => {
            return Err(
                "--key-file needs --tcp <bind> (the sealed wire is a TCP transport)".to_string(),
            );
        }
        (Some(b), Some(k)) => (b, k),
    };
    if !crate::transport::SEALED {
        return Err(format!("--tcp: {}", crate::transport::SEALED_UNAVAILABLE));
    }
    match crate::transport::endpoint_port(bind) {
        Some(0) | None => {
            return Err(format!(
                "--tcp {}: needs <host>:<port> with a fixed port, not 0",
                safe(bind, 128)
            ));
        }
        Some(_) => {}
    }
    match crate::transport::is_loopback_endpoint(bind) {
        Ok(true) => {}
        Ok(false) if opts.allow_remote => {}
        Ok(false) => {
            return Err(format!(
                "--tcp {}: not a loopback address, and every host of the fleet holds the one key; \
                 add --allow-remote to serve it on the network",
                safe(bind, 128)
            ));
        }
        Err(e) => return Err(format!("--tcp {}: {e}", safe(bind, 128))),
    }
    let key_file = absolute(key);
    Ok(Wire::Sealed {
        bind: Some(bind.clone()),
        dial: crate::transport::dial_for_bind(bind),
        key_file,
        allow_remote: opts.allow_remote,
    })
}

/// `path` as an absolute path: itself, or joined to the working directory. A
/// relative path written into a plist or a bridge command would resolve
/// against whatever directory THAT process starts in.
pub(crate) fn absolute(path: &str) -> PathBuf {
    let p = PathBuf::from(path);
    if p.is_absolute() {
        p
    } else {
        std::env::current_dir().map_or(p.clone(), |cwd| cwd.join(&p))
    }
}

/// The pre-shared key THIS host's broker serves under (`on --tcp`): an
/// existing file is CHECKED — 64 hex characters, mode 0600 — and never
/// replaced (a key another host already holds is the fleet's, and a silent
/// re-key would cut every joined host off with a handshake failure); a missing
/// one is minted, 32 random bytes as 64 hex, 0600. `false` stops the run.
fn key_step(key_file: &Path, out: &mut Out) -> bool {
    let shown = key_file.display().to_string();
    let path = key_file.to_string_lossy();
    if key_file.exists() {
        return match crate::transport::read_private_key_file(&path) {
            Ok(_) => {
                out.already(
                    "key",
                    &format!("{shown} (0600, 64 hex: the fleet's pre-shared key)"),
                );
                true
            }
            Err(e) => {
                out.fail(
                    "key",
                    &format!(
                        "{}: not replaced (joined hosts hold it); fix it, or name a new \
                         --key-file and re-join every host",
                        safe(&crate::transport::describe(&path, &e), 256)
                    ),
                );
                false
            }
        };
    }
    out.done(
        "key",
        &format!("mint a pre-shared key into {shown} (64 hex, 0600)"),
        &format!("minted {shown} (0600) — the file a joining host copies"),
    );
    if out.dry_run {
        return true;
    }
    let minted = random_bytes(32).and_then(|b| {
        let mut key = [0u8; 32];
        key.copy_from_slice(&b);
        write_atomic(
            key_file,
            crate::transport::key_file_text(&key).as_bytes(),
            0o600,
        )
    });
    if let Err(e) = minted {
        out.fail("key", &format!("{shown}: {e}"));
        return false;
    }
    true
}

/// `aterm fabric on`.
#[must_use]
pub fn on(opts: &OnOpts) -> ExitCode {
    let wire = match wire_for_on(opts) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("aterm fabric on: {e}");
            return ExitCode::from(2);
        }
    };
    let mut p = match Paths::resolve(opts.fleet.as_deref()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("aterm fabric on: {e}");
            return ExitCode::from(2);
        }
    };
    p.wire = wire;
    let service = opts.service.unwrap_or_else(Service::default_here);
    let mut out = Out::new(opts.dry_run);
    println!(
        "aterm fabric on — fleet {} · root {} · service {}{}{}",
        safe(&p.fleet, 64),
        p.root.display(),
        service.name(),
        match &p.wire {
            Wire::Sealed {
                bind: Some(bind), ..
            } => format!(" · broker on the SEALED TCP wire at {}", safe(bind, 128)),
            _ => String::new(),
        },
        if opts.dry_run {
            " · DRY RUN: every step printed, nothing touched"
        } else {
            ""
        }
    );

    // The refusals that must come before anything is written.
    // The socket is the broker's on BOTH wires `on` serves: the sealed one
    // serves it beside the TCP port, for this host's own bridges.
    let sock = p.endpoint();
    if sock.len() >= SUN_PATH_MAX {
        out.fail(
            "socket",
            &format!(
                "{sock} is {} bytes, over sun_path's {SUN_PATH_MAX}: the fabric root's path is \
                 too long",
                sock.len()
            ),
        );
        return ExitCode::from(2);
    }
    let state = p.state();
    let key_word = match &p.wire {
        Wire::Sealed { key_file, .. } => key_file.to_string_lossy().into_owned(),
        Wire::Unix => String::new(),
    };
    for word in [
        p.aterm.as_str(),
        p.fleet.as_str(),
        sock.as_str(),
        &p.cap().to_string_lossy(),
        &state.to_string_lossy(),
        key_word.as_str(),
    ] {
        if let Err(e) = argv_safe(word) {
            out.fail("argv", &e);
            return ExitCode::from(2);
        }
    }
    match binary_has_link(&p) {
        Ok(()) => out.ok(
            "binary",
            &format!(
                "{} ({}serve + {}broker{})",
                p.aterm,
                if p.is_link_shim() { "" } else { "link " },
                if p.is_link_shim() { "" } else { "link " },
                if matches!(p.wire, Wire::Sealed { .. }) {
                    ", sealed TCP compiled in"
                } else {
                    ""
                }
            ),
        ),
        Err(e) => {
            out.fail("binary", &e);
            return ExitCode::from(2);
        }
    }

    // 2. the root
    if !root_step(&p, &state, &mut out) {
        return ExitCode::from(2);
    }

    // 3. the node id
    let node = match read_node(&state) {
        Some(n) => {
            out.already("node", &format!("{n} ({}/node)", state.display()));
            n
        }
        None => {
            let minted = match aterm_uds::rand::hex_token::<8>() {
                Ok(hex) => format!("n-{hex}"),
                Err(e) => {
                    out.fail("node", &format!("no entropy for a node id: {e}"));
                    return ExitCode::from(2);
                }
            };
            out.done(
                "node",
                &format!("provision a node id into {}/node", state.display()),
                &format!("provisioned {minted}"),
            );
            if !record_node(&state, &minted, &mut out) {
                return ExitCode::from(2);
            }
            minted
        }
    };

    // 4. the mint secret
    let secret = p.secret();
    match std::fs::metadata(&secret) {
        Ok(m) if m.len() >= SECRET_LEN as u64 => {
            out.already("secret", &format!("{} (0600)", secret.display()));
        }
        Ok(m) => {
            out.fail(
                "secret",
                &format!(
                    "{} holds {} bytes; a mint secret is at least {SECRET_LEN} and is never \
                     replaced: move it aside and run again",
                    secret.display(),
                    m.len()
                ),
            );
            return ExitCode::from(2);
        }
        Err(_) => {
            out.done(
                "secret",
                &format!(
                    "mint {SECRET_LEN} random bytes into {} (0600)",
                    secret.display()
                ),
                &format!("minted {}", secret.display()),
            );
            if !opts.dry_run {
                if let Err(e) =
                    random_bytes(SECRET_LEN).and_then(|b| write_atomic(&secret, &b, 0o600))
                {
                    out.fail("secret", &format!("{}: {e}", secret.display()));
                    return ExitCode::from(2);
                }
            }
        }
    }

    // 4b. the pre-shared key, on the sealed wire THIS host serves
    if let Wire::Sealed { key_file, .. } = &p.wire {
        let key_file = key_file.clone();
        if !key_step(&key_file, &mut out) {
            return ExitCode::from(2);
        }
    }

    // 5. the cap file
    let cap = p.cap();
    let cap_ok = if cap.exists() {
        cap_is_for(&cap, &node)
    } else {
        Err("absent".to_string())
    };
    match cap_ok {
        Ok(n) => out.already("cap", &format!("{} ({n} grants for {node})", cap.display())),
        Err(why) => {
            out.done(
                "cap",
                &format!("mint 8 grants for {node} into {} ({why})", cap.display()),
                "",
            );
            if !opts.dry_run {
                match mint_caps(&p, &node) {
                    Ok(n) => out.ok(
                        "cap",
                        &format!("minted {n} grants for {node} into {}", cap.display()),
                    ),
                    Err(e) => {
                        out.fail("cap", &e);
                        return ExitCode::FAILURE;
                    }
                }
            }
        }
    }

    // 6. the broker. A dry run keeps going past one that does not answer —
    // the point of a dry run is to see EVERY step — and exits 1 at the end.
    // On the sealed wire, a broker that answers on its socket must answer on
    // the TCP port too (6b).
    let broker_ok = broker_step(&p, &node, service, &mut out);
    if !broker_ok && !opts.dry_run {
        return ExitCode::FAILURE;
    }
    if broker_ok && !wire_step(&p, &mut out) && !opts.dry_run {
        return ExitCode::FAILURE;
    }

    // 7–12: the config, the rendezvous file, the instances, the proof, the
    // undo and the status — the same tail `join` runs.
    let argv = p.bridge_command(&node);
    finish(&p, &node, &argv, &mut out, opts.dry_run, Verb::On)
}

/// Which command [`finish`] is finishing — the words its last lines use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verb {
    /// `aterm fabric on`.
    On,
    /// `aterm fabric join` (`crate::join`).
    Join,
}

/// Steps 7–12 of `on`, and the same steps of `join`: write `[fabric] command`
/// (atomically, with a `.bak`), write the rendezvous file, arm every running
/// instance with `argv`, PROVE the round trip, print the undo scoped like this
/// run, then the status. The exit code is this run's: the steps and the proof.
pub(crate) fn finish(
    p: &Paths,
    node: &str,
    argv: &[String],
    out: &mut Out,
    dry_run: bool,
    verb: Verb,
) -> ExitCode {
    let node = node.to_string();
    let state = p.state();
    let sock = p.endpoint();
    let cap = p.cap();
    // 7. aterm.toml
    let command = argv.join(" ");
    if !config_step(p.config.as_deref(), &command, out) {
        return ExitCode::FAILURE;
    }

    // 8. the rendezvous file: a joined host's key, or the sealed wire this
    // host serves and its key.
    let (key_file, serves_tcp, serves_key_file) = match &p.wire {
        Wire::Unix => (None, None, None),
        Wire::Sealed {
            bind: None,
            key_file,
            ..
        } => (Some(key_file.to_string_lossy().into_owned()), None, None),
        Wire::Sealed {
            bind: Some(_),
            dial,
            key_file,
            ..
        } => (
            None,
            Some(dial.clone()),
            Some(key_file.to_string_lossy().into_owned()),
        ),
    };
    let rendezvous = Rendezvous {
        fleet: p.fleet.clone(),
        broker: sock.clone(),
        node: node.clone(),
        cap_file: cap.to_string_lossy().into_owned(),
        state: state.to_string_lossy().into_owned(),
        command: command.clone(),
        key_file,
        serves_tcp,
        serves_key_file,
    };
    match p.rendezvous.clone() {
        None => out.fail(
            "rendezvous",
            "no control-socket dir: neither $XDG_RUNTIME_DIR nor $HOME is set",
        ),
        Some(path) => {
            let want = rendezvous.render();
            if std::fs::read(&path).is_ok_and(|old| old == want.as_bytes()) {
                out.already("rendezvous", &format!("{} (0600)", path.display()));
            } else {
                out.done(
                    "rendezvous",
                    &format!("write {} (0600)", path.display()),
                    &format!("wrote {} (0600)", path.display()),
                );
                if !dry_run {
                    if let Some(dir) = path.parent() {
                        let _ = std::fs::create_dir_all(dir).and_then(|()| {
                            std::fs::set_permissions(
                                dir,
                                std::os::unix::fs::PermissionsExt::from_mode(0o700),
                            )
                        });
                    }
                    if let Err(e) = write_atomic(&path, want.as_bytes(), 0o600) {
                        out.fail("rendezvous", &format!("{}: {e}", path.display()));
                    }
                }
            }
        }
    }

    // 9. the running instances
    let (running, armed) = arm_instances(argv, out);

    // 10. the proof
    let (proof_failed, proof_ran) = if armed.is_empty() {
        out.note("proof", proof_skipped(running));
        (false, false)
    } else {
        match prove(&armed, out) {
            Ok(_) => (false, true),
            Err(e) => {
                out.fail("proof", &e);
                (true, true)
            }
        }
    };

    // 11. the undo, scoped like this run was.
    if !dry_run {
        let program = if p.is_link_shim() {
            "aterm-link"
        } else {
            "aterm"
        };
        let env = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        let home = p
            .login_home
            .as_deref()
            .filter(|login| *login != p.home.as_path())
            .map(|_| p.home.to_string_lossy().into_owned());
        let scope = [
            (
                ROOT_SEAM,
                root_seam().map(|r| r.to_string_lossy().into_owned()),
            ),
            ("XDG_CONFIG_HOME", env("XDG_CONFIG_HOME")),
            ("XDG_RUNTIME_DIR", env("XDG_RUNTIME_DIR")),
            (
                "ATERM_BIN",
                aterm_types::dev_seam!("ATERM_BIN")
                    .map(|b| b.to_string_lossy().into_owned())
                    .filter(|v| !v.is_empty()),
            ),
            ("HOME", home),
        ];
        out.note("undo", &undo_command(program, &scope));
    }

    if dry_run {
        println!(
            "\n{} step(s) would change something; run without --dry-run to apply.{}",
            out.changed,
            if out.failed {
                " A step FAILED above, and would fail for real."
            } else {
                ""
            }
        );
        return if out.failed {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        };
    }
    if out.changed == 0 {
        println!(
            "\nnothing changed: {}, and the proof {}.",
            match verb {
                Verb::On => "the fabric was already on",
                Verb::Join => "this host had already joined",
            },
            if proof_ran {
                "ran again"
            } else {
                "was skipped"
            }
        );
    }

    // 12. status. THE EXIT CODE IS THIS RUN'S: the proof and the steps. The
    // status's own warnings are printed with it, but a fabric that turned on
    // and proved itself is not a failed `on` because a session has unread
    // mail — the last line says so.
    println!();
    match fabric::gather() {
        Ok(report) => {
            print!("{}", fabric::render_text(&report));
            if proof_failed || out.failed {
                ExitCode::FAILURE
            } else {
                if let Some(line) = done_despite(verb, report.warnings.len()) {
                    println!("\n{line}");
                }
                ExitCode::SUCCESS
            }
        }
        Err(off) => {
            println!("{}", off.message());
            ExitCode::FAILURE
        }
    }
}

/// The verdict a successful run prints under a status that has warnings:
/// they are the fabric's, not this run's failure. `None` with no warnings.
fn done_despite(verb: Verb, warnings: usize) -> Option<String> {
    let name = match verb {
        Verb::On => "on",
        Verb::Join => "join",
    };
    match warnings {
        0 => None,
        1 => Some(format!("{name}: done; the warning above is not this run's")),
        n => Some(format!(
            "{name}: done; the {n} warnings above are not this run's"
        )),
    }
}

// ---------------------------------------------------------------------------
// off
// ---------------------------------------------------------------------------

/// `off`'s config step: every `fabric` table removed from aterm.toml
/// ([`remove_fabric_table`]), written as `on` writes it ([`ConfigFile`]) with
/// the previous file kept as a `.bak` — `already` when there is none, and a
/// dry run writes nothing.
fn off_config_step(config: Option<&Path>, out: &mut Out) {
    let Some(config) = config else {
        out.note(
            "config",
            "no config path: neither $XDG_CONFIG_HOME nor $HOME is set",
        );
        return;
    };
    let fail = |out: &mut Out, e: &dyn std::fmt::Display| {
        out.fail("config", &format!("{}: {e}", config.display()));
    };
    let file = match ConfigFile::open(config) {
        Ok(file) => file,
        // A link that names no file: the app reads no command from it.
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            out.already("config", &format!("{}: {e}", config.display()));
            return;
        }
        Err(e) => return fail(out, &e),
    };
    let text = match file.text() {
        Ok(Some(text)) => text,
        Ok(None) => {
            out.already("config", &format!("{} does not exist", config.display()));
            return;
        }
        Err(e) => return fail(out, &e),
    };
    let (new_text, removed) = match remove_fabric_table(text) {
        Ok((_, removed)) if removed.is_empty() => {
            out.already(
                "config",
                &format!("no [fabric] table in {}", config.display()),
            );
            return;
        }
        Ok(edit) => edit,
        Err(e) => return fail(out, &e),
    };
    let headers = removed.join(", ");
    out.done(
        "config",
        &format!(
            "remove {headers} from {} (previous saved as .bak)",
            config.display()
        ),
        "",
    );
    if out.dry_run {
        return;
    }
    match file.lock().and_then(|edit| edit.replace(&new_text)) {
        Ok(bak) => out.ok(
            "config",
            &format!(
                "{headers} removed from {}{}",
                config.display(),
                bak.map_or_else(String::new, |b| format!(
                    " (previous saved as {})",
                    b.display()
                ))
            ),
        ),
        Err(e) => fail(out, &e),
    }
}

/// `aterm fabric off`.
#[must_use]
pub fn off(opts: &OffOpts) -> ExitCode {
    let (p, root_from) = match Paths::resolve_for_off() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("aterm fabric off: {e}");
            return ExitCode::from(2);
        }
    };
    let service = opts.service.unwrap_or_else(Service::default_here);
    let mut out = Out::new(opts.dry_run);
    println!(
        "aterm fabric off — root {} (from {root_from}) · service {}{}",
        p.root.display(),
        service.name(),
        if opts.dry_run {
            " · DRY RUN: every step printed, nothing touched"
        } else {
            ""
        }
    );
    // THE HELD SESSIONS, BEFORE ANYTHING STOPS (module doc, `off`).
    for (pid, sock, sid, origin) in held_sessions() {
        let detail = if origin.as_deref() == Some("local") {
            format!(
                "@{sid} (instance {pid}) is HELD (hold=1 origin=local); off does not lift it \
                 — {} does",
                lift_hold(&sid, pid, sock.as_deref())
            )
        } else {
            format!(
                "@{sid} (instance {pid}) is HELD (hold=1 origin={}); off does not lift it — \
                 relaunch instance {pid} after this",
                origin
                    .as_deref()
                    .map_or_else(|| "?".to_string(), |o| safe(o, 16))
            )
        };
        out.warn("held", &detail);
    }
    let label = p.label();
    match service {
        Service::Launchd => {
            let plist = p.plist();
            let loaded = launchd_loaded(&label);
            if !loaded && !plist.exists() {
                out.already("broker", &format!("launchd {label} is not installed"));
            } else {
                out.done(
                    "broker",
                    &format!("bootout launchd {label} and remove {}", plist.display()),
                    &format!("launchd {label} stopped and {} removed", plist.display()),
                );
                if !opts.dry_run {
                    if loaded {
                        let domain = format!("gui/{}/{label}", uid());
                        if let Err(e) = launchctl(&["bootout", &domain]) {
                            let plist_s = plist.to_string_lossy().into_owned();
                            if launchctl(&["unload", &plist_s]).is_err() {
                                out.fail("broker", &e);
                            }
                        }
                    }
                    if let Err(e) = std::fs::remove_file(&plist) {
                        if e.kind() != io::ErrorKind::NotFound {
                            out.fail("broker", &format!("{}: {e}", plist.display()));
                        }
                    }
                }
            }
        }
        Service::Systemd => {
            let unit = p.unit();
            if !unit.exists() {
                out.already(
                    "broker",
                    &format!("systemd --user {label} is not installed"),
                );
            } else {
                out.done(
                    "broker",
                    &format!("disable --now {label} and remove {}", unit.display()),
                    &format!(
                        "systemd --user {label} stopped and {} removed",
                        unit.display()
                    ),
                );
                if !opts.dry_run {
                    if let Err(e) = systemctl(&["disable", "--now", &label]) {
                        out.fail("broker", &e);
                    }
                    let _ = std::fs::remove_file(&unit);
                    let _ = systemctl(&["daemon-reload"]);
                }
            }
        }
        Service::None => out.note(
            "broker",
            &if Path::new(&p.log()).exists() {
                format!(
                    "not managed here: stop this root's `link broker` yourself (its log is {})",
                    p.log()
                )
            } else {
                "this root runs no broker of its own".to_string()
            },
        ),
    }

    off_config_step(p.config.as_deref(), &mut out);

    match p.rendezvous.clone() {
        Some(path) if path.exists() => {
            out.done(
                "rendezvous",
                &format!("remove {}", path.display()),
                &format!("removed {}", path.display()),
            );
            if !opts.dry_run {
                if let Err(e) = std::fs::remove_file(&path) {
                    out.fail("rendezvous", &format!("{}: {e}", path.display()));
                }
            }
        }
        Some(path) => out.already("rendezvous", &format!("{} is absent", path.display())),
        None => out.note("rendezvous", "no control-socket dir resolves"),
    }

    let kept: Vec<&str> = [
        ("mint.secret", p.secret()),
        ("node.cap", p.cap()),
        ("fleet.key", p.installed_key()),
    ]
    .iter()
    .filter(|(_, path)| path.exists())
    .map(|(name, _)| *name)
    .collect();
    out.note(
        "kept",
        &format!(
            "{}: node id{}{}{}",
            p.root.display(),
            read_node(&p.state()).map_or_else(String::new, |n| format!(" {n}")),
            if kept.is_empty() { "" } else { ", " },
            kept.join(", ")
        ),
    );
    out.note(
        "bus log",
        &if Path::new(&p.log()).exists() {
            format!("{} stays", p.log())
        } else {
            format!(
                "none in {} (it is on the host that serves the broker)",
                p.root.display()
            )
        },
    );
    let running: BTreeSet<u32> = aterm_ctl::local_instances()
        .unwrap_or_default()
        .into_iter()
        .map(|(pid, _)| pid)
        .filter(|pid| aterm_uds::process::pid_alive(*pid))
        .collect();
    out.note(
        "instances",
        &if running.is_empty() {
            "none running; the next launch starts no bridge".to_string()
        } else {
            format!(
                "{} running ({}) keep their bridge until relaunched; the next launch starts none",
                running.len(),
                running
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        },
    );
    if opts.dry_run {
        println!(
            "\n{} step(s) would change something; run without --dry-run to apply.",
            out.changed
        );
        return ExitCode::SUCCESS;
    }
    if out.failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

// ---------------------------------------------------------------------------
// doctor
// ---------------------------------------------------------------------------

/// The fix for one of `aterm fabric`'s WARNINGS, matched on the words the
/// warning is built from (`fabric::warnings`).
#[must_use]
pub fn fix_for(warning: &str) -> String {
    let w = warning;
    let s = |t: &str| w.contains(t);
    if s("does not answer") && s("no record lands") || s("does not answer this report's own probe")
    {
        "start the broker: `aterm fabric on`".to_string()
    } else if s("has not reported its broker link") {
        "nothing, if its mail moves (`aterm ctl @<sid> post to=@<sid> kind=note --wait=5000 \
         ping` lands); if it does not, check the broker: `aterm fabric on`"
            .to_string()
    } else if s("broker link is down") {
        // The warning carries the bridge's own `reason=`, so the one cause it
        // names picks the one remedy.
        let reason = w
            .split("reason=")
            .nth(1)
            .and_then(|r| r.split([',', ' ', ')']).next())
            .unwrap_or("");
        let cause = match reason {
            "no-socket" | "refused" => {
                "nothing listens at the broker's socket: `aterm fabric on` starts it"
            }
            "no-ack" => "the broker accepts and never answers: `aterm fabric on` restarts it",
            "attach" => {
                "the broker refuses this node's cap: move <root>/node.cap aside and run `aterm \
                 fabric on`"
            }
            "denied" => "the broker's socket refuses this user: check its owner and mode",
            _ => "check the broker: `aterm fabric on`",
        };
        format!("{cause}; the bridge then reconnects on its own")
    } else if s("bus could not be read") || s("could not be read:") {
        "the cap file's grants do not cover this fleet, or the broker died mid-read: `aterm fabric \
         on` re-mints the cap for the node id in the state dir and probes again; check `--fleet`"
            .to_string()
    } else if s("no [fabric] command") {
        "`aterm fabric on` writes [fabric] command (and the rendezvous file) so the next launch \
         starts a bridge on its own"
            .to_string()
    } else if s("no node id") {
        "`aterm fabric on` provisions one and mints the cap for it".to_string()
    } else if s("cap file") {
        "`aterm fabric on` re-mints the cap file for the node id in the state dir".to_string()
    } else if s("has no bridge") {
        "`aterm fabric on` arms every running instance".to_string()
    } else if s("bridge is down") {
        "aterm relaunches its bridge with back-off; if it stays down, the bridge cannot start — \
         run the [fabric] command by hand to see its error, then `aterm fabric on`"
            .to_string()
    } else if s("has not answered for the work") {
        "kill that instance's bridge (`kill <bridge pid>` from the BRIDGES row); aterm relaunches \
         it"
        .to_string()
    } else if s("armed with a different command") {
        "relaunch that aterm instance: its supervisor reads [fabric] command once, at launch"
            .to_string()
    } else if s("ONE state dir") {
        "give each instance its own --state dir (each is then its own node with its own cap), or \
         run one instance"
            .to_string()
    } else if s("the bus's own presence") {
        "kill that instance's bridge (`kill <bridge pid>` from the BRIDGES row); aterm relaunches \
         it and publishes state=live"
            .to_string()
    } else if s("is HELD") {
        // The warning names the hold's origin and reason when the timeline
        // still has them, and that decides which remedy applies. `hold` acts
        // only on the instance it reaches, so the command names that instance,
        // as `off` does.
        let lift = lift_hold_in(w);
        if s("origin=local") {
            lift
        } else if s("reason=fabric-lost origin=fleet") {
            "it lifts when the bridge reconnects".to_string()
        } else if s("origin=fleet") {
            "only the human who set it can withdraw it".to_string()
        } else {
            format!(
                "a local hold lifts with {lift}; a fleet hold lifts when a bridge reconnects \
                 (reason=fabric-lost) or the human who set it withdraws it"
            )
        }
    } else if s("lost") && s("evicted") {
        "read mail sooner (`aterm ctl @<sid> await inbox`) and `inbox seen` what is handled; the \
         evicted records are still on the bus"
            .to_string()
    } else if s("passed its deadline") {
        "the session it was sent to has not answered: on that session's host, `aterm ctl @<sid> \
         inbox` shows it, then `inbox seen <id> handled` (or `refused`)"
            .to_string()
    } else if s("unhandled task/ask") {
        "that session was given work: `aterm ctl @<sid> inbox`, handle it, then `inbox seen <id> \
         handled` (or `refused`)"
            .to_string()
    } else if s("queued post") {
        "arm a bridge (`aterm fabric on`); the outbox drains the moment one attaches".to_string()
    } else if s("did not answer") {
        "that instance's control thread is wedged or its socket is stale: `aterm ctl instances`, \
         and relaunch it"
            .to_string()
    } else if s("cannot list this machine's aterm instances") {
        "the control-socket dir could not be read: check $XDG_RUNTIME_DIR / $HOME and the \
         directory's permissions"
            .to_string()
    } else if s("the bus advertises") {
        // NOT "relaunch and it clears". A bridge retires only what it can PROVE
        // unhosted — its own incarnation's rows, and one whose will it
        // witnessed — so a row left by an incarnation that died without a will
        // survives every relaunch. That is §10's residual, and the flag below
        // is the only thing that ends it.
        "`aterm fabric doctor --retire-ghosts` lists the row; `--yes` publishes `exited` for it \
         (a relaunch does not clear it)"
            .to_string()
    } else if s("fleet halt standing") {
        "only the human who set the halt can withdraw it (their `/f/<F>/fleet/<h>/halt` row); ask \
         them"
            .to_string()
    } else if s("no rendezvous file") {
        "`aterm fabric on` writes it".to_string()
    } else {
        "see `aterm help fabric`".to_string()
    }
}

/// The command that lifts a local hold, for the HELD warning `warning` —
/// `@<sid> (instance <pid>) is HELD …`, or `@<sid> (instance <pid>, --sock
/// <sock>) is HELD …` for an instance only its socket reaches.
fn lift_hold_in(warning: &str) -> String {
    let (sid, at) = warning
        .strip_prefix('@')
        .and_then(|w| w.split_once(" (instance "))
        .and_then(|(sid, rest)| Some((sid, rest.split_once(") is HELD")?.0)))
        .unwrap_or_default();
    let (pid, sock) = at
        .split_once(", --sock ")
        .map_or((at, None), |(pid, sock)| (pid, Some(sock)));
    lift_hold(sid, pid.parse().unwrap_or(0), sock)
}

/// What `on` says it does to one instance: the `fabric attach` it sends, spelled as a
/// person would run it — by `--sock` when `--pid` does not reach the instance
/// ([`aterm_ctl::pid_reaches`]), as `off` and doctor name it.
fn arm_command(pid: u32, sock: &str, argv: &[String]) -> String {
    let reach = instance_flag(pid, (!aterm_ctl::pid_reaches(pid, sock)).then_some(sock));
    format!(
        "arm {pid} (`aterm ctl {reach} fabric attach {}`)",
        argv.join(" ")
    )
}

/// `aterm ctl <instance> hold <sid> off`, the instance named by [`instance_flag`].
/// A sid a command line cannot carry as it is stays a placeholder.
fn lift_hold(sid: &str, pid: u32, sock: Option<&str>) -> String {
    let sid = if plain_arg(sid, b"") { sid } else { "<sid>" };
    format!("`aterm ctl {} hold {sid} off`", instance_flag(pid, sock))
}

/// Whether `t` is a command-line word as it is: ASCII letters, digits, `-_.` and `more`.
fn plain_arg(t: &str, more: &[u8]) -> bool {
    !t.is_empty()
        && t.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.') || more.contains(&b)
        })
}

/// How `aterm ctl` names an instance: `--pid <pid>`, or `--sock <sock>` when only its
/// socket reaches it (`sock` is `Some`). A socket path with a space is single-quoted;
/// one that cannot be quoted (a quote, a backslash, a control character), or pid 0,
/// stays a placeholder.
fn instance_flag(pid: u32, sock: Option<&str>) -> String {
    match sock {
        Some(sock) if plain_arg(sock, b"/+~@%,:=") => format!("--sock {sock}"),
        Some(sock)
            if !sock.is_empty()
                && !sock.contains(['\'', '\\'])
                && !sock.chars().any(char::is_control) =>
        {
            format!("--sock '{sock}'")
        }
        Some(_) => "--sock <sock>".to_string(),
        None if pid != 0 => format!("--pid {pid}"),
        None => "--pid <pid>".to_string(),
    }
}

/// `--retire-ghosts`: publish `exited` for every presence row this node
/// advertises LIVE that no local instance hosts.
///
/// The bridge's own sweep ([`crate::bridge::Bridge::retire_ghost_presence`])
/// retires only what it can PROVE unhosted — its own incarnation's rows and
/// one whose will it witnessed — so a row left by an incarnation that died
/// without a will stays live for ever and mail to it is never delivered. Only
/// the operator can supply the missing fact.
///
/// DRY BY DEFAULT: it lists and exits. With a `y` (or `--yes`) it publishes.
///
/// A RUNNING LOCAL BRIDGE PUBLISHES IT: a record goes out under the NODE's
/// producer id at a sequence reserved through the state dir, and
/// `StateDir::reserve_sequence` is an unlocked file write. A CLI racing a live
/// bridge for the next sequence would hand the broker a `(producer_id, seq)`
/// pair it has already seen, which it answers with the original offset and
/// appends NOTHING — a retire that reports success and did not happen.
fn retire_ghosts_action(yes: bool) -> ExitCode {
    let report = match fabric::gather() {
        Ok(r) => r,
        Err(off) => {
            println!("{}", off.message());
            return ExitCode::from(2);
        }
    };
    // THE SAME GATE `status` USES, and for the same reason. An instance that
    // did not answer is in the report with no sessions, so every live row on
    // this node reads unhosted — and retiring one that instance is hosting
    // perfectly well is the worst thing this command could do. `status`
    // refuses to NAME such a row; this refuses to touch any of them.
    let silent = fabric::unanswered(&report);
    if !silent.is_empty() {
        println!(
            "refused: {} local instance(s) did not answer, so no row on this node can be shown \
             unhosted:",
            silent.len()
        );
        for why in &silent {
            println!("  ! {why}");
        }
        println!(
            "  fix: make every instance answerable (`aterm fabric status` names each one), \
             then re-run"
        );
        return ExitCode::from(2);
    }
    let local = fabric::local_nodes(&report.instances, report.node.as_deref());
    let ghosts: Vec<(String, String)> = fabric::ghost_rows(&report.sessions, &local)
        .into_iter()
        .map(|s| (s.sid.clone(), s.node.clone()))
        .collect();
    if ghosts.is_empty() {
        println!("no ghost rows: every live row on this node is hosted here");
        return ExitCode::SUCCESS;
    }
    println!(
        "{} presence row(s) this node advertises LIVE that no local instance hosts:",
        ghosts.len()
    );
    for (sid, node) in &ghosts {
        println!("  @{} on {}", safe(sid, 64), safe(node, 64));
    }
    // A RUNNING BRIDGE OWNS THE PUBLISH, SO IT IS ASKED TO DO IT. The listing
    // is the whole value of a dry run and costs nothing either way.
    let bridge = running_bridge(&report.instances);
    if !yes && !confirmed() {
        if let Some(pid) = bridge {
            println!("  note: with --yes, instance {pid}'s bridge is asked to publish them");
        }
        println!("  nothing published (dry). Re-run with --yes, or answer y.");
        return ExitCode::SUCCESS;
    }
    if let Some(pid) = bridge {
        return retire_through_bridge(&report, pid, &ghosts);
    }
    match fabric::publish_exited(&report, &ghosts) {
        Ok(n) => {
            println!("  published `exited` for {n} row(s)");
            ExitCode::SUCCESS
        }
        Err(e) => {
            println!("  could not publish: {e}");
            ExitCode::from(2)
        }
    }
}

/// THE BRIDGE-UP PATH: ask the instance at `pid` — over its Owner token, on a
/// connection of its own — to retire the rows through its bridge (`fabric
/// retire <sid>…`), then read the bus back until every row says `exited` or
/// [`RETIRE_WAIT`] is up. The CLI writes no sequence: the bridge publishes
/// under its own, which is what makes the two safe together. A row the bridge
/// REFUSED (a sibling instance hosts the session; the node could not be asked)
/// is named with the reason its `presence-retire-refused` event gives; any other
/// row still unretired at the deadline is reported as exactly that.
///
/// An instance too old for the verb answers `ERR usage`; that is reported
/// with its remedy, and nothing is published.
fn retire_through_bridge(
    report: &fabric::Report,
    pid: u32,
    ghosts: &[(String, String)],
) -> ExitCode {
    let Some(instance) = report.instances.iter().find(|i| i.pid == pid) else {
        println!("  could not find the instance at pid {pid} in the report");
        return ExitCode::from(2);
    };
    let sids: Vec<&str> = ghosts.iter().map(|(sid, _)| sid.as_str()).collect();
    let reply = (|| -> Result<String, String> {
        let mut ctl = crate::ctl::Ctl::open(&instance.sock).map_err(|e| e.to_string())?;
        let reply = ctl
            .request(&format!("fabric retire {}", sids.join(" ")))
            .map_err(|e| e.to_string())?;
        Ok(reply.header().to_string())
    })();
    let header = match reply {
        Ok(h) => h,
        Err(e) => {
            println!("  could not ask the instance at pid {pid}: {e}");
            return ExitCode::from(2);
        }
    };
    if !header.starts_with("OK") {
        println!(
            "  the instance at pid {pid} answered: {}",
            safe(&header, 160)
        );
        if header.starts_with("ERR usage") {
            println!(
                "  its bridge is too old for `fabric retire`: quit that instance and run this \
                 again"
            );
        }
        return ExitCode::from(2);
    }
    println!("  asked the bridge at pid {pid}: {}", safe(&header, 80));
    let mut done = 0;
    let deadline = std::time::Instant::now() + RETIRE_WAIT;
    let mut pending: Vec<&str> = sids.clone();
    // The report's bus head predates the request, so a refusal at or after it
    // answers this run and never an earlier one.
    let since = report.broker.head;
    let mut refused = BTreeMap::new();
    // Whether the LAST poll read the bridge's refusals through to the head: only
    // then does "has not refused it" rest on something read.
    let mut refusals_read = false;
    while !pending.is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(200));
        let Ok(progress) = fabric::retire_progress(report, &pending, since) else {
            refusals_read = false;
            continue;
        };
        refusals_read = progress.refusals_read;
        pending.retain(|sid| {
            let retired = progress.states.get(*sid).is_some_and(|s| s == "exited");
            if retired {
                done += 1;
            }
            !retired && !progress.refused.contains_key(*sid)
        });
        refused.extend(progress.refused);
    }
    for (sid, why) in &refused {
        println!("  @{}: the bridge refused it: {why}", safe(sid, 64));
    }
    for sid in &pending {
        let queued = if refusals_read {
            " and not refused; the request is still queued at the bridge"
        } else {
            "; the request may still be queued at the bridge"
        };
        println!(
            "  @{}: not retired within {RETIRE_WAIT:?}{queued} (`aterm fabric tail` shows the \
             outcome)",
            safe(sid, 64)
        );
    }
    println!("  published `exited` for {done} row(s) through the bridge");
    if pending.is_empty() && refused.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    }
}

/// How long the bridge-up path waits for the bus to show the retirements.
const RETIRE_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// The pid of a local instance whose bridge is up, if one is.
fn running_bridge(instances: &[fabric::InstanceView]) -> Option<u32> {
    instances
        .iter()
        .find(|i| {
            // A BRIDGE PROCESS IS A BRIDGE, whatever its link is doing.
            // `bridge_pids` is the observed truth — a child that exists — and
            // it is what settles the two cases the state words miss: a
            // `stale` link (connected, owing an answer) and a bridge whose
            // `fabric status` this report could not read. Either one still
            // owns this node's publish sequence.
            !i.bridge_pids.is_empty()
                || i.supervised == Some(true)
                || matches!(i.fabric.as_deref(), Some("connected" | "stalled" | "stale"))
        })
        .map(|i| i.pid)
}

/// A y/N on stdin. Anything but `y`/`yes` is no, and a stdin that cannot be
/// read (a pipe, a cron) is NO — an unattended run must never retire a row.
fn confirmed() -> bool {
    use std::io::{IsTerminal as _, Write as _};
    // A TTY OR NOTHING: a piped `y` is not consent, and an inherited idle
    // pipe (a cron, a CI step) would park `read_line` for ever. Without a
    // terminal, `--yes` in the argv is the one way to say yes.
    if !std::io::stdin().is_terminal() {
        println!("  stdin is not a terminal: taking that as NO (pass --yes to mean yes)");
        return false;
    }
    print!("  publish `exited` for these rows? [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// `aterm fabric doctor`.
#[must_use]
pub fn doctor(retire_ghosts: bool, yes: bool) -> ExitCode {
    if retire_ghosts {
        return retire_ghosts_action(yes);
    }
    match fabric::gather() {
        Err(off) => {
            // The state, then the remedy once, in the `fix:` shape every finding uses.
            println!("{}", off.state());
            println!("  fix: `aterm fabric on` (`--dry-run` first shows every step)");
            ExitCode::from(2)
        }
        Ok(report) => {
            let mut warnings = report.warnings.clone();
            match Rendezvous::read() {
                Ok(Some(_)) => {}
                Ok(None) => warnings.push(format!(
                    "no rendezvous file at {}: `aterm link ls` needs its flags \
                     spelled out here",
                    rendezvous_path().map_or_else(|| "-".to_string(), |p| p.display().to_string())
                )),
                Err(e) => warnings.push(format!("the rendezvous file is unreadable: {e}")),
            }
            println!(
                "aterm fabric doctor — fleet {} · node {}",
                safe(&report.cfg.fleet, 64),
                report.node.as_deref().unwrap_or("-")
            );
            if warnings.is_empty() {
                println!("  no warnings");
                return ExitCode::SUCCESS;
            }
            println!("  {} warning(s)", warnings.len());
            for w in &warnings {
                println!("  ! {w}");
                println!("    fix: {}", fix_for(w));
            }
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A BRIDGE PROCESS IS A BRIDGE, whatever its link is doing.** The
    /// refusal exists because a running bridge owns this node's publish
    /// sequence, and that is true of a bridge whose link went `stale` and of
    /// one whose `fabric status` this report could not read. `bridge_pids` is
    /// the observed truth — a child that exists — so it decides first.
    #[test]
    fn running_bridge_sees_a_stale_one_and_a_bare_pid() {
        let inst = |fabric: Option<&str>, supervised, pids: Vec<u32>| fabric::InstanceView {
            pid: 42,
            fabric: fabric.map(str::to_string),
            supervised,
            bridge_pids: pids,
            ..fabric::InstanceView::default()
        };
        assert_eq!(
            running_bridge(&[inst(None, None, vec![99])]),
            Some(42),
            "a bridge child exists: that is a running bridge"
        );
        assert_eq!(
            running_bridge(&[inst(Some("stale"), Some(false), vec![])]),
            Some(42),
            "a stale link is a bridge that still owns the sequence"
        );
        assert_eq!(
            running_bridge(&[inst(Some("connected"), None, vec![])]),
            Some(42)
        );
        assert_eq!(
            running_bridge(&[inst(Some("absent"), Some(false), vec![])]),
            None
        );
        assert_eq!(running_bridge(&[]), None);
    }

    /// **A PLANTED NAME IS NEVER WRITTEN THROUGH.** `mint-for --out` into a
    /// directory another user writes: a symlink planted at a guessable temp
    /// name and its target are untouched, the cap lands as a regular 0600
    /// file, and no temp is left behind. A symlink AT the path itself is
    /// replaced by the rename — its target is never written either.
    #[test]
    fn write_atomic_never_writes_through_a_planted_name() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("atfo-plant-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let shared = dir.join("shared");
        std::fs::create_dir_all(&shared).expect("scratch");
        let mode = |p: &Path| std::fs::metadata(p).expect("meta").permissions().mode() & 0o777;
        let victim = dir.join("victim.txt");
        std::fs::write(&victim, "the operator's own file\n").expect("victim");
        std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        let out = shared.join("m7.cap");
        let planted = shared.join(format!("m7.tmp.{}", std::process::id()));
        std::os::unix::fs::symlink(&victim, &planted).expect("plant");
        write_atomic(&out, b"eight grant lines\n", 0o600).expect("write");
        assert_eq!(
            std::fs::read_to_string(&victim).expect("victim"),
            "the operator's own file\n"
        );
        assert_eq!(
            mode(&victim),
            0o644,
            "the victim's mode was changed through the link"
        );
        let meta = std::fs::symlink_metadata(&out).expect("out");
        assert!(meta.file_type().is_file(), "{meta:?}");
        assert_eq!(mode(&out), 0o600);
        assert_eq!(std::fs::read(&out).expect("out"), b"eight grant lines\n");
        assert!(std::fs::symlink_metadata(&planted)
            .expect("planted")
            .file_type()
            .is_symlink());
        let mut names: Vec<String> = std::fs::read_dir(&shared)
            .expect("list")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "m7.cap".to_string(),
                format!("m7.tmp.{}", std::process::id())
            ],
            "a temp was left behind"
        );
        // A symlink AT the path: replaced, its target never written.
        let target = dir.join("elsewhere");
        std::fs::write(&target, "untouched\n").expect("target");
        let linked = shared.join("fleet.key");
        std::os::unix::fs::symlink(&target, &linked).expect("link");
        write_atomic(&linked, b"key\n", 0o600).expect("write over the link");
        assert_eq!(
            std::fs::read_to_string(&target).expect("target"),
            "untouched\n"
        );
        assert!(std::fs::symlink_metadata(&linked)
            .expect("linked")
            .file_type()
            .is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// THE LABEL RULE: the default root keeps the plain label; any other root
    /// derives one from POSIX `cksum` of its path — the number
    /// `tools/fabric-enable.sh` computed, measured on this machine:
    /// `printf '%s' /Users//example/.local/share/aterm-fabric | cksum` →
    /// `2100958200 39`.
    #[test]
    fn the_label_is_derived_from_the_root_with_posix_cksum() {
        assert_eq!(
            cksum(b"/Users//example/.local/share/aterm-fabric"),
            2_100_958_200
        );
        assert_eq!(cksum(b""), 0xFFFF_FFFF);
        let home = PathBuf::from("/Users//example");
        let p = |root: &str| Paths {
            home: home.clone(),
            login_home: Some(home.clone()),
            root: PathBuf::from(root),
            aterm: "/usr/local/bin/aterm".to_string(),
            fleet: "local".to_string(),
            config: None,
            rendezvous: None,
            wire: Wire::Unix,
        };
        assert_eq!(p("/Users//example/.local/share/aterm-fabric").label(), LABEL);
        assert_eq!(
            p("/tmp/atfo-x/root").label(),
            format!("{LABEL}.{}", cksum(b"/tmp/atfo-x/root"))
        );
        assert_ne!(p("/tmp/atfo-x/root").label(), LABEL);
    }

    /// TWO ROOTS NEVER SHARE A LABEL. The plain label belongs to the LOGIN
    /// user's default root, so a redirected `$HOME`'s default root derives its
    /// own; an unknown login home makes every root derive its own.
    #[test]
    fn review_two_roots_never_share_a_label() {
        let paths = |home: &str, login: Option<&str>| Paths {
            home: PathBuf::from(home),
            login_home: login.map(PathBuf::from),
            root: PathBuf::from(home).join(".local/share/aterm-fabric"),
            aterm: "/usr/local/bin/aterm".to_string(),
            fleet: "local".to_string(),
            config: None,
            rendezvous: None,
            wire: Wire::Unix,
        };
        let live = paths("/Users//example", Some("/Users//example"));
        let redirected = paths("/tmp/x", Some("/Users//example"));
        assert_eq!(live.label(), LABEL);
        assert_ne!(redirected.label(), live.label());
        assert_eq!(
            redirected.label(),
            format!("{LABEL}.{}", cksum(b"/tmp/x/.local/share/aterm-fabric"))
        );
        // The login home unknown: nothing is the default root.
        assert_ne!(paths("/Users//example", None).label(), LABEL);
        // And the real machine's answer, measured: the login home is an
        // absolute path and `$HOME` does not decide it.
        let measured = login_home();
        assert!(
            measured.as_deref().is_some_and(Path::is_absolute),
            "login_home on this machine: {measured:?}"
        );
    }

    /// THE PLIST RUNS THE BROKER DIRECTLY — the words, one `<string>` each,
    /// no `/bin/sh -c` — and is valid XML for any path; the shell form the
    /// script wrote is CURRENT all the same, so `on` does not restart a
    /// healthy broker for its spelling; a foreign argv is not.
    #[test]
    fn the_plist_runs_the_broker_directly_and_the_scripts_form_reads_as_current() {
        let p = Paths {
            home: PathBuf::from("/Users//example"),
            login_home: Some(PathBuf::from("/Users//example")),
            root: PathBuf::from("/Users//example/.local/share/aterm-fabric"),
            aterm: "/Users//example/.local/bin/aterm".to_string(),
            fleet: "local".to_string(),
            config: None,
            rendezvous: None,
            wire: Wire::Unix,
        };
        let text = p.plist_text();
        assert!(
            text.contains(
                "  <array>\n\
             \x20   <string>/Users//example/.local/bin/aterm</string>\n\
             \x20   <string>link</string>\n\
             \x20   <string>broker</string>\n\
             \x20   <string>/Users//example/.local/share/aterm-fabric/bus.sock</string>\n\
             \x20   <string>/Users//example/.local/share/aterm-fabric/bus.log</string>\n\
             \x20 </array>\n"
            ),
            "{text}"
        );
        assert!(
            !text.contains("/bin/sh") && !text.contains("rm -f"),
            "{text}"
        );
        assert!(text.contains("  <key>Label</key><string>systems.alab.astream-broker</string>\n"));
        assert!(text.contains("  <key>KeepAlive</key><true/>\n"));
        assert!(text.ends_with("</dict>\n</plist>\n"));
        assert!(p.plist_is_current(&text));
        assert_eq!(
            plist_program_arguments(&text).as_deref(),
            Some(p.broker_argv().as_slice())
        );
        // The script's form, byte for byte.
        let legacy = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n\
             <dict>\n\
             \x20 <key>Label</key><string>systems.alab.astream-broker</string>\n\
             \x20 <key>ProgramArguments</key>\n\
             \x20 <array>\n\
             \x20   <string>/bin/sh</string><string>-c</string>\n\
             \x20   <string>rm -f '/Users//example/.local/share/aterm-fabric/bus.sock'; exec '/Users//example/.local/bin/aterm' link broker '/Users//example/.local/share/aterm-fabric/bus.sock' '/Users//example/.local/share/aterm-fabric/bus.log'</string>\n\
             \x20 </array>\n\
             \x20 <key>RunAtLoad</key><true/>\n\
             \x20 <key>KeepAlive</key><true/>\n\
             \x20 <key>ProcessType</key><string>Background</string>\n\
             \x20 <key>WorkingDirectory</key><string>/Users//example/.local/share/aterm-fabric</string>\n\
             \x20 <key>StandardOutPath</key><string>/Users//example/.local/share/aterm-fabric/broker.out</string>\n\
             \x20 <key>StandardErrorPath</key><string>/Users//example/.local/share/aterm-fabric/broker.err</string>\n\
             </dict>\n\
             </plist>\n";
        assert!(
            p.plist_is_current(legacy),
            "the script's install is current, not restarted"
        );
        // plistlib's layout (tabs, a line break between key and value, the
        // keys sorted) with the direct argv: current too.
        let plistlib = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict>\n\
             \t<key>KeepAlive</key>\n\t<true/>\n\
             \t<key>Label</key>\n\t<string>systems.alab.astream-broker</string>\n\
             \t<key>ProgramArguments</key>\n\t<array>\n\
             \t\t<string>/Users//example/.local/bin/aterm</string>\n\t\t<string>link</string>\n\t\t<string>broker</string>\n\
             \t\t<string>/Users//example/.local/share/aterm-fabric/bus.sock</string>\n\
             \t\t<string>/Users//example/.local/share/aterm-fabric/bus.log</string>\n\t</array>\n\
             \t<key>RunAtLoad</key>\n\t<true/>\n</dict>\n</plist>\n";
        assert!(p.plist_is_current(plistlib));
        // Another broker, another label, or no KeepAlive: not this job.
        assert!(!p.plist_is_current(&text.replace("bus.log", "other.log")));
        assert!(!p.plist_is_current(
            &text.replace("astream-broker</string>", "astream-broker.1</string>")
        ));
        assert!(!p.plist_is_current(&text.replace(
            "<key>KeepAlive</key><true/>",
            "<key>KeepAlive</key><false/>"
        )));
        assert!(!p.plist_is_current("<plist/>"));
        // A `&` in a path is XML character data, not a broken plist — and it
        // reads back as itself.
        let odd = Paths {
            root: PathBuf::from("/tmp/a&b<c"),
            login_home: None,
            ..p.clone()
        };
        let odd_text = odd.plist_text();
        assert!(
            odd_text.contains("<string>/tmp/a&amp;b&lt;c/bus.sock</string>"),
            "{odd_text}"
        );
        assert!(odd_text.contains("<key>WorkingDirectory</key><string>/tmp/a&amp;b&lt;c</string>"));
        assert!(!odd_text.contains("a&b"), "{odd_text}");
        assert!(odd.plist_is_current(&odd_text));
        assert_eq!(
            plist_program_arguments(&collapse_between_tags(&odd_text)).expect("argv")[3],
            "/tmp/a&b<c/bus.sock"
        );
        // The shim spells the broker without `link`.
        let shim = Paths {
            aterm: "/x/target/debug/aterm-link".to_string(),
            ..p.clone()
        };
        assert_eq!(
            shim.broker_argv(),
            [
                "/x/target/debug/aterm-link",
                "broker",
                "/Users//example/.local/share/aterm-fabric/bus.sock",
                "/Users//example/.local/share/aterm-fabric/bus.log"
            ]
        );
        assert_eq!(shim.bridge_command("n-a")[1], "serve");
        assert_eq!(
            shim.label(),
            LABEL,
            "the root, not the binary, decides the label"
        );
    }

    /// THE PLIST FOLLOWS THE ROOT: the default root's job lives in
    /// `~/Library/LaunchAgents`, any other root's inside that root.
    #[test]
    fn the_plist_follows_the_root() {
        let p = |root: &str, login: Option<&str>| Paths {
            home: PathBuf::from("/Users//example"),
            login_home: login.map(PathBuf::from),
            root: PathBuf::from(root),
            aterm: "/usr/local/bin/aterm".to_string(),
            fleet: "local".to_string(),
            config: None,
            rendezvous: None,
            wire: Wire::Unix,
        };
        assert_eq!(
            p(
                "/Users//example/.local/share/aterm-fabric",
                Some("/Users//example")
            )
            .plist(),
            PathBuf::from("/Users//example/Library/LaunchAgents/systems.alab.astream-broker.plist")
        );
        let redirected = p("/tmp/atfo-x/root", Some("/Users//example"));
        assert_eq!(
            redirected.plist(),
            PathBuf::from(format!("/tmp/atfo-x/root/{}.plist", redirected.label()))
        );
        assert!(!redirected.plist().starts_with("/Users//example/Library"));
        // A root that is `$HOME`'s default but not the LOGIN home's is not
        // the default root, and its plist stays with it.
        let foreign = Paths {
            home: PathBuf::from("/tmp/x"),
            ..p("/tmp/x/.local/share/aterm-fabric", Some("/Users//example"))
        };
        assert!(foreign
            .plist()
            .starts_with("/tmp/x/.local/share/aterm-fabric/"));
    }

    /// WHEN LAUNCHD DID NOT BRING THE SOCKET UP, the hint names what exists: a
    /// broker.err the job wrote, else launchd's own view of the job.
    #[test]
    fn the_launchd_failure_hint_names_what_was_written() {
        let dir = std::env::temp_dir().join(format!("atfo-hint-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        let absent = launchd_failure_hint(&dir, "systems.alab.astream-broker.7", "501");
        assert!(
            absent.contains("broker.err was not written")
                && absent.contains("launchctl print gui/501/systems.alab.astream-broker.7"),
            "{absent}"
        );
        std::fs::write(dir.join("broker.err"), "").expect("empty");
        assert!(launchd_failure_hint(&dir, "l", "501").contains("was not written"));
        std::fs::write(dir.join("broker.err"), "bind: address in use\n").expect("err");
        let written = launchd_failure_hint(&dir, "l", "501");
        assert_eq!(
            written,
            format!("read {}", dir.join("broker.err").display())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// THE UNDO CARRIES THE SCOPE: every variable that redirected this run,
    /// none that did not, and the front door's own word.
    #[test]
    fn the_undo_carries_the_environment_that_scoped_the_run() {
        assert_eq!(undo_command("aterm", &[]), "aterm fabric off");
        assert_eq!(
            undo_command(
                "aterm-link",
                &[
                    ("ATERM_FABRIC_HOME", Some("/tmp/x/root".to_string())),
                    ("XDG_CONFIG_HOME", None),
                    ("XDG_RUNTIME_DIR", Some(String::new())),
                    ("ATERM_BIN", Some("/x/aterm-link".to_string())),
                ]
            ),
            "ATERM_FABRIC_HOME=/tmp/x/root ATERM_BIN=/x/aterm-link aterm-link fabric off"
        );
    }

    /// THE CONFIG EDIT keeps every other table and key, replaces only the
    /// `command` key (single- or multi-line) — adding `receipts = true` once
    /// when the table has no such key — appends a table when
    /// there is none, and is a no-op when the file already says the command
    /// and has a `receipts` key of its own, whichever way it is set.
    #[test]
    fn the_config_edit_touches_only_the_command_key() {
        let cmd = "aterm link serve --fleet local --broker /b.sock";
        let before = "font_px = 12.0\n\n[fabric]\n# a comment\ncommand = \"old\"\npresence = \"meta\"\n\n[keys]\nx = 1\n";
        let (after, changed) = set_fabric_command(before, cmd).expect("ok");
        assert!(changed);
        assert_eq!(
            after,
            format!(
                "font_px = 12.0\n\n[fabric]\ncommand = \"{cmd}\"\nreceipts = true\n# a comment\npresence = \"meta\"\n\n[keys]\nx = 1\n"
            )
        );
        assert_eq!(fabric::command_in_toml(&after), Ok(Some(cmd.to_string())));
        assert_eq!(fabric::receipts_in_toml(&after), Ok(Some(true)));
        // Idempotent.
        assert!(!set_fabric_command(&after, cmd).expect("ok").1);
        // AN OPERATOR'S `receipts = false` STANDS: the same command over it is
        // a no-op, and a changed command keeps it.
        let off = format!("[fabric]\ncommand = \"{cmd}\"\nreceipts = false\n");
        assert!(!set_fabric_command(&off, cmd).expect("ok").1);
        let (after, _) =
            set_fabric_command(&off, "aterm link serve --fleet x --broker /y").expect("ok");
        assert_eq!(fabric::receipts_in_toml(&after), Ok(Some(false)));
        assert_eq!(after.matches("receipts").count(), 1);
        // A multi-line command is replaced whole.
        let multi = "[fabric]\ncommand = \"\"\"aterm link serve --fleet x \\\n   --broker /y\"\"\"\nk = 1\n[z]\n";
        let (after, _) = set_fabric_command(multi, cmd).expect("ok");
        assert_eq!(
            after,
            format!("[fabric]\ncommand = \"{cmd}\"\nreceipts = true\nk = 1\n[z]\n")
        );
        // No table: appended after a blank line, with the note.
        let (after, changed) = set_fabric_command("font_px = 1.0\n", cmd).expect("ok");
        assert!(changed);
        assert!(after.starts_with("font_px = 1.0\n\n[fabric]\n# Written by `aterm fabric on`"));
        assert_eq!(fabric::command_in_toml(&after), Ok(Some(cmd.to_string())));
        assert_eq!(fabric::receipts_in_toml(&after), Ok(Some(true)));
        let (after, _) = set_fabric_command("", cmd).expect("ok");
        assert!(after.starts_with("[fabric]\n"));
        // A file that is not TOML is refused, not edited.
        assert!(set_fabric_command("[fabric\ncommand = \"x\"\n", cmd).is_err());
        // A dotted spelling is refused, not doubled.
        assert!(set_fabric_command("fabric.command = \"x\"\n", cmd).is_err());
        assert!(set_fabric_command("fabric = { command = \"x\" }\n", cmd).is_err());
        // But a dotted key INSIDE another table is that table's business.
        let (after, _) = set_fabric_command("[other]\nfabric.command = \"x\"\n", cmd).expect("ok");
        assert!(after.contains("[other]\nfabric.command = \"x\"\n\n[fabric]\n"));

        // REMOVAL drops the table and nothing else.
        let (after, removed) = remove_fabric_table(before).expect("ok");
        assert_eq!(removed, ["[fabric]"]);
        assert_eq!(after, "font_px = 12.0\n\n[keys]\nx = 1\n");
        assert!(remove_fabric_table(&after).expect("ok").1.is_empty());
        assert_eq!(
            remove_fabric_table("[fabric]\ncommand = \"x\"\n"),
            Ok((String::new(), vec!["[fabric]".to_string()]))
        );
        let (after, _) =
            remove_fabric_table("[fabric]\ncommand = \"x\"\n[keys]\nx = 1\n").expect("ok");
        assert_eq!(after, "[keys]\nx = 1\n");
        // AND EVERY `[fabric.<sub>]` TABLE, wherever it sits: a subtable left
        // behind re-creates the `fabric` key. Each header removed is named, in
        // file order.
        let subs = "[fabric.presence]\nx = 1\n[a]\ny = 2\n[fabric] # on\ncommand = \"x\"\n[fabric.more]\nz = 3\n[keys]\nk = 1\n";
        let (after, removed) = remove_fabric_table(subs).expect("ok");
        assert_eq!(removed, ["[fabric.presence]", "[fabric]", "[fabric.more]"]);
        assert_eq!(after, "[a]\ny = 2\n[keys]\nk = 1\n");
        assert!(fabric::command_in_toml(&after).expect("valid").is_none());
        // A subtable alone is a fabric table too; `[fabrics]` is not.
        assert_eq!(
            remove_fabric_table("[fabric.presence]\nx = 1\n"),
            Ok((String::new(), vec!["[fabric.presence]".to_string()]))
        );
        assert!(remove_fabric_table("[fabrics]\nx = 1\n")
            .expect("ok")
            .1
            .is_empty());
        // A file that is not TOML is refused, not edited: which lines are
        // fabric's cannot be read in it (a malformed `[keys` ends no table), so
        // a removal would take the person's own lines with it.
        for broken in [
            "[fabric]\ncommand = \"x\"\n[keys\nbind = \"y\"\n[update]\nenabled = false\n",
            "[fabric]\ncommand = \"x\"\n[[keys]\nbind = \"y\"\n[update]\nenabled = false\n",
        ] {
            assert!(remove_fabric_table(broken).is_err(), "{broken}");
        }
    }

    /// EVERY SPELLING TOML ALLOWS names the table the app reads: `[ fabric ]`,
    /// `["fabric"]` and a quoted `"command"` key are edited in place, never
    /// doubled into a file the app cannot load (which reads automatic updates
    /// back ON against the person's `enabled = false`), and `off` removes them.
    /// What the editor cannot place — a header-shaped line inside a string, a
    /// dotted `fabric.command` — is refused, never written, and `off` never
    /// calls a `fabric` it left behind absent.
    #[test]
    fn the_config_edit_reads_headers_and_keys_the_way_toml_does() {
        let cmd = "aterm link serve --fleet local --broker /b.sock";
        let person = "[update]\nenabled = false\n";
        for table in [
            "[ fabric ]\ncommand = \"old\"\n",
            "[\"fabric\"] # on\ncommand = \"old\"\n",
            "[fabric]\n\"command\" = \"old\"\n",
            "[ 'fabric' . presence ]\nx = 1\n[fabric]\ncommand = \"old\"\n",
        ] {
            let before = format!("{person}\n{table}");
            let (after, changed) = set_fabric_command(&before, cmd).expect(&before);
            assert!(changed);
            assert_eq!(
                fabric::command_in_toml(&after),
                Ok(Some(cmd.to_string())),
                "{after}"
            );
            assert!(after.starts_with(person), "{after}");
            let (off, removed) = remove_fabric_table(&after).expect(&after);
            assert!(!removed.is_empty(), "{after}");
            assert_eq!(off, person, "{after}");
        }
        let in_a_string = "note = \"\"\"\n[fabric]\n\"\"\"\n";
        assert!(set_fabric_command(in_a_string, cmd).is_err());
        assert!(remove_fabric_table(in_a_string).is_err());
        assert!(remove_fabric_table("fabric.command = \"x\"\n").is_err());
        assert!(remove_fabric_table("fabric = { command = \"x\" }\n").is_err());
    }

    /// ATERM.TOML IS WRITTEN WHERE THE CONFIG SERVICE WRITES IT: through a
    /// final-file link into a dotfiles checkout (the link kept, the `.bak` beside
    /// the file it names), under the service's own write lock — taken for the
    /// write, let go when it returns — and never over bytes that changed since
    /// the read, an older `.bak` included.
    #[test]
    fn the_config_is_written_through_its_link_under_the_service_lock() {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let dotfiles = dir.path().join("dotfiles");
        let cfg = dir.path().join("cfg/aterm");
        std::fs::create_dir_all(&dotfiles).expect("dotfiles");
        std::fs::create_dir_all(&cfg).expect("cfg");
        let target = dotfiles.join("aterm.toml");
        std::fs::write(&target, "font_px = 12.0\n").expect("seed");
        let config = cfg.join("aterm.toml");
        std::os::unix::fs::symlink("../../dotfiles/aterm.toml", &config).expect("link");
        let cmd = "aterm link serve --fleet local --broker /b.sock";

        assert!(config_step(Some(&config), cmd, &mut Out::new(false)));
        let linked = |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_symlink());
        assert!(linked(&config), "the link was replaced by a file");
        let text = std::fs::read_to_string(&target).expect("target");
        assert_eq!(fabric::command_in_toml(&text), Ok(Some(cmd.to_string())));
        assert_eq!(
            std::fs::read_to_string(dotfiles.join("aterm.toml.bak")).expect("bak"),
            "font_px = 12.0\n"
        );

        let lock = dotfiles.join(".aterm.toml.aterm-write.lock");
        let file = ConfigFile::open(&config).expect("open");
        let edit = file.lock().expect("lock");
        assert_eq!(
            aterm_uds::ownerlock::probe(&lock),
            aterm_uds::ownerlock::Owner::Live
        );
        // A writer that ignores the lock lands between the read and the write.
        std::fs::write(&target, "font_px = 18.0\n").expect("the other writer");
        let err = edit
            .replace("font_px = 9.0\n")
            .expect_err("changed under it");
        assert!(
            err.to_string().contains("changed while fabric was editing"),
            "{err}"
        );
        assert_eq!(
            std::fs::read_to_string(&target).expect("target"),
            "font_px = 18.0\n"
        );
        // Refused before the `.bak` too: the older one is kept, not replaced
        // for an edit that never lands.
        assert_eq!(
            std::fs::read_to_string(dotfiles.join("aterm.toml.bak")).expect("bak"),
            "font_px = 12.0\n"
        );
        // `replace` let the lock go when it returned, refused or not.
        assert_eq!(
            aterm_uds::ownerlock::probe(&lock),
            aterm_uds::ownerlock::Owner::Dead
        );
        assert!(linked(&config));
        let entries = std::fs::read_dir(&dotfiles).expect("list").count();
        assert_eq!(
            entries, 3,
            "a temp was left behind: aterm.toml, its .bak, the lock"
        );
    }

    /// A RUN THAT CHANGES NOTHING WRITES NOTHING, NOT EVEN A LOCK: `on` over a
    /// file that already says its command and `off` over one with no `fabric`
    /// read `already` through a link into a directory nobody may write — a
    /// home-manager link into the read-only `/nix/store` — and leave nothing
    /// in it. The config service's lock is taken for a write, never a read.
    #[test]
    fn a_no_op_through_a_link_into_a_read_only_dir_reads_already() {
        use std::os::unix::fs::PermissionsExt;
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let store = dir.path().join("store");
        std::fs::create_dir_all(&store).expect("store");
        let cmd = "aterm link serve --fleet local --broker /b.sock";
        let (configured, _) = set_fabric_command("font_px = 12.0\n", cmd).expect("ok");
        let link = |name: &str, text: &str| {
            let file = store.join(format!("{name}-aterm.toml"));
            std::fs::write(&file, text).expect("seed");
            let cfg = dir.path().join(name).join("aterm");
            std::fs::create_dir_all(&cfg).expect("cfg");
            std::os::unix::fs::symlink(&file, cfg.join("aterm.toml")).expect("link");
            cfg.join("aterm.toml")
        };
        let (on_config, off_config) = (link("on", &configured), link("off", "font_px = 12.0\n"));
        let chmod = |mode| {
            std::fs::set_permissions(&store, std::fs::Permissions::from_mode(mode)).expect("chmod");
        };
        chmod(0o555);
        let (mut on, mut off) = (Out::new(false), Out::new(false));
        let on_ran = config_step(Some(&on_config), cmd, &mut on);
        off_config_step(Some(&off_config), &mut off);
        chmod(0o755);
        assert!(
            on_ran && !on.failed && on.changed == 0,
            "on did not read `already`"
        );
        assert!(
            !off.failed && off.changed == 0,
            "off did not read `already`"
        );
        assert_eq!(
            std::fs::read_dir(&store).expect("list").count(),
            2,
            "a lock or a temp was left in the store"
        );
    }

    /// THE RENDEZVOUS FILE round-trips, and a value with a quote in it still
    /// parses back.
    #[test]
    fn the_rendezvous_file_round_trips() {
        let r = Rendezvous {
            fleet: "local".to_string(),
            broker: "/tmp/x/bus.sock".to_string(),
            node: "n-0123456789abcdef".to_string(),
            cap_file: "/tmp/x/node.cap".to_string(),
            state: "/tmp/x/link-state".to_string(),
            command: "/x/aterm link serve --fleet local --broker /tmp/x/bus.sock".to_string(),
            key_file: None,
            serves_tcp: None,
            serves_key_file: None,
        };
        let text = r.render();
        assert!(text.starts_with("# Written by `aterm fabric on`"));
        assert_eq!(Rendezvous::parse(&text), Ok(r.clone()));
        let odd = Rendezvous {
            command: "a \"quoted\" \\ back".to_string(),
            ..r
        };
        assert_eq!(Rendezvous::parse(&odd.render()), Ok(odd));
        assert!(Rendezvous::parse("fleet = 1\n").is_err());
        assert!(Rendezvous::parse("[x\n").is_err());
    }

    /// THE GRANTS are the script's eight, in its order, with the node in six.
    #[test]
    fn the_eight_grants_are_the_node_ring() {
        let g = node_grants("local", "n-a");
        assert_eq!(g.len(), 8);
        assert_eq!(g[0], "rw,p=n-a:/f/local/pub/n-a/>");
        assert_eq!(g[3], "rw,p=n-a:/f/local/in/*/*/n-a/*");
        assert_eq!(g[7], "rw,p=n-a:/f/local/term/n-a/*/screen");
        assert_eq!(g.iter().filter(|x| x.contains("p=n-a:")).count(), 4);
        assert_eq!(g.iter().filter(|x| x.starts_with("ro:")).count(), 4);
        // Six of the eight NAME the node (the module doc's number): the four
        // it writes as, and its own inbox and drive-face read lanes.
        assert_eq!(g.iter().filter(|x| x.contains("n-a")).count(), 6);
    }

    /// ARGV SAFETY: whitespace, a quote or a backslash in any word is refused.
    #[test]
    fn a_word_the_whitespace_split_would_break_is_refused() {
        assert!(argv_safe("/Users//u/.local/share/aterm-fabric").is_ok());
        assert!(argv_safe("/Users//u/Caf\u{e9}").is_ok());
        for bad in [
            "/Users//u/fab ric",
            "/Users//u/fab\"ric",
            "/Users//u/fab\\ric",
            "a\tb",
        ] {
            assert!(argv_safe(bad).is_err(), "{bad}");
        }
    }

    /// EVERY WARNING HAS A FIX, and the unknown one points at the manual.
    #[test]
    fn every_warning_shape_has_a_fix() {
        for w in [
            "the broker at /x does not answer (connect: x): no record lands and none is delivered",
            "instance 1 says fabric=connected (its bridge's last ack was 3s ago), but the broker at \
             /tmp/b.sock does not answer this report's own probe",
            "instance 1's bridge is attached but its broker link is down (fabric=stalled \
             reason=refused, last ack 3s ago)",
            "instance 1's bridge is attached and has not reported its broker link (fabric=stalled \
             reason=starting)",
            "the broker at /x answers but the bus could not be read: head query",
            "no [fabric] command in /c: this report read instance 1's running bridge",
            "no node id in /s/node",
            "cap file /c: the tag is not hex",
            "instance 1 has no bridge (fabric=absent, not supervised)",
            "instance 1's bridge is down (fabric=disconnected)",
            "instance 1's bridge was armed with a different command than x",
            "instances 1, 2 run bridges on ONE state dir (/s)",
            "instance 1 says fabric=connected, but the bus's own presence for its node n-a says state=gone",
            "instance 1 says fabric=connected, but the bus's own presence for its node n-a says \
             state=live fabric=disconnected",
            "instance 1's bridge has not answered for the work it was given (fabric=stale, last ack \
             never)",
            "ask off=40 from h-andrew to @s-one@n-a passed its deadline 10m ago (dl=600000 ms) with \
             no answer, report or ack — not yet recorded expired by the asker's bridge",
            "@s-a (instance 1) is HELD (reason=x origin=local)",
            "@s-a (instance 1) lost 2 message(s): its inbox ring evicted rows",
            "@s-a (instance 1) has 1 unhandled task/ask older than 10 min",
            "@s-a (instance 1) has 3 queued post(s) and no working bridge",
            "instance 1 did not answer (x)",
            "cannot list this machine's aterm instances: x",
            "the bus advertises @s-g live on n-a",
            "h-andrew has a fleet halt standing (reason=stop)",
            "no rendezvous file at /x",
        ] {
            let fix = fix_for(w);
            assert_ne!(fix, "see `aterm help fabric`", "no fix for: {w}");
        }
        assert_eq!(fix_for("something new"), "see `aterm help fabric`");
    }

    /// **ONE REMEDY WHERE THE WARNING ALREADY SAYS WHICH APPLIES.** A hold's
    /// origin and reason, and a down link's `reason=`, are in the warning's own
    /// words, so the fix names the one remedy for that cause.
    #[test]
    fn a_fix_picks_the_remedy_the_warning_names() {
        let held = |why: &str| fix_for(&format!("@s-a (instance 1) is HELD ({why})"));
        // A hold is lifted on the instance that hosts the session: a flagless
        // `aterm ctl` reaches the caller's own (or the newest) instance, whose
        // store answers `ERR no such session` for another's.
        assert_eq!(
            held("reason=x origin=local"),
            "`aterm ctl --pid 1 hold s-a off`"
        );
        assert_eq!(
            fix_for("a session is HELD (reason=x origin=local)"),
            "`aterm ctl --pid <pid> hold <sid> off`"
        );
        assert!(held("reason and origin no longer in its timeline")
            .contains("`aterm ctl --pid 1 hold s-a off`"));
        // An explicit-`--control-sock` instance: only its socket reaches it.
        assert_eq!(
            fix_for("@s-a (instance 1, --sock /tmp/at.x/a.sock) is HELD (reason=x origin=local)"),
            "`aterm ctl --sock /tmp/at.x/a.sock hold s-a off`"
        );
        // A socket path with a space is quoted, so the command still names it.
        assert_eq!(
            fix_for("@s-a (instance 0, --sock /tmp/a b.sock) is HELD (reason=x origin=local)"),
            "`aterm ctl --sock '/tmp/a b.sock' hold s-a off`"
        );
        // One that cannot be quoted stays a placeholder.
        assert_eq!(
            fix_for("@s-a (instance 0, --sock /tmp/a'b.sock) is HELD (reason=x origin=local)"),
            "`aterm ctl --sock <sock> hold s-a off`"
        );
        // `on --dry-run` names an explicit-`--control-sock` instance the same way: by the
        // socket `--pid` cannot reach.
        assert_eq!(
            arm_command(
                4242,
                "/tmp/at.x/a.sock",
                &["--node".to_string(), "n1".to_string()]
            ),
            "arm 4242 (`aterm ctl --sock /tmp/at.x/a.sock fabric attach --node n1`)"
        );
        // A legacy graph entry's pid 0 reaches nothing by `--pid`.
        assert_eq!(
            fix_for("@s-a (instance 0) is HELD (reason=x origin=local)"),
            "`aterm ctl --pid <pid> hold s-a off`"
        );
        assert_eq!(
            held("reason=fabric-lost origin=fleet"),
            "it lifts when the bridge reconnects"
        );
        assert_eq!(
            held("reason=stop origin=fleet"),
            "only the human who set it can withdraw it"
        );
        assert!(held("reason and origin no longer in its timeline").contains("a fleet hold"));

        let down = |reason: &str| {
            fix_for(&format!(
                "instance 1's bridge is attached but its broker link is down (fabric=stalled \
                 reason={reason}, last ack 3s ago)"
            ))
        };
        assert!(
            down("refused").starts_with("nothing listens"),
            "{}",
            down("refused")
        );
        assert!(
            down("no-socket").starts_with("nothing listens"),
            "{}",
            down("no-socket")
        );
        assert!(down("no-ack").contains("restarts it"), "{}", down("no-ack"));
        assert!(
            down("attach").contains("move <root>/node.cap aside"),
            "{}",
            down("attach")
        );
        assert!(
            down("denied").contains("check its owner and mode") && !down("denied").contains("cap"),
            "{}",
            down("denied")
        );
        assert!(
            down("closed").starts_with("check the broker"),
            "{}",
            down("closed")
        );
        assert!(
            down("read").ends_with("the bridge then reconnects on its own"),
            "{}",
            down("read")
        );
    }

    /// THE SEALED WIRE this host serves is one argv to launchd and a unit with
    /// no socket to unlink — the broker always guarded with the root's own
    /// mint secret, and serving the root's socket BESIDE the port, which is
    /// what this host's own bridges dial. A JOINED host's bridges carry `--tcp
    /// --key-file` to the remote broker.
    #[test]
    fn the_sealed_wire_is_one_broker_argv_and_one_set_of_bridge_flags() {
        let p = Paths {
            home: PathBuf::from("/Users//u"),
            login_home: Some(PathBuf::from("/Users//u")),
            root: PathBuf::from("/tmp/r"),
            aterm: "/usr/local/bin/aterm".to_string(),
            fleet: "local".to_string(),
            config: None,
            rendezvous: None,
            wire: Wire::Sealed {
                bind: Some("0.0.0.0:7000".to_string()),
                dial: crate::transport::dial_for_bind("0.0.0.0:7000"),
                key_file: PathBuf::from("/k/fleet.key"),
                allow_remote: true,
            },
        };
        assert_eq!(
            p.broker_argv().join(" "),
            "/usr/local/bin/aterm link broker --tcp 0.0.0.0:7000 --key-file /k/fleet.key \
             --secret-file /tmp/r/mint.secret --allow-remote --unix /tmp/r/bus.sock \
             /tmp/r/bus.log"
        );
        assert_eq!(
            p.bridge_command("n-a").join(" "),
            "/usr/local/bin/aterm link serve --fleet local --broker /tmp/r/bus.sock \
             --cap-file /tmp/r/node.cap --state /tmp/r/link-state --accept-from n-a",
            "this host's own bridges dial the socket, not the port"
        );
        let joined = Paths {
            wire: Wire::Sealed {
                bind: None,
                dial: "m100.local:7000".to_string(),
                key_file: PathBuf::from("/tmp/r/fleet.key"),
                allow_remote: false,
            },
            ..p.clone()
        };
        assert_eq!(
            joined.bridge_command("n-b").join(" "),
            "/usr/local/bin/aterm link serve --fleet local --broker m100.local:7000 --tcp \
             --key-file /tmp/r/fleet.key --cap-file /tmp/r/node.cap --state \
             /tmp/r/link-state --accept-from n-b"
        );
        assert!(
            p.bridge_command_accepting("n-a", &["n-b".to_string(), "n-a".to_string()])
                .join(" ")
                .ends_with("--accept-from n-a,n-b"),
            "the node first, each principal once"
        );
        let unit = p.unit_text();
        assert!(!unit.contains("rm -f"), "{unit}");
        assert!(
            unit.contains("'--tcp' '0.0.0.0:7000'") && unit.contains("'--secret-file'"),
            "{unit}"
        );
        let plist = p.plist_text();
        assert!(p.plist_is_current(&plist), "{plist}");
        // The Unix plist of the SAME root is not this job: switching a root
        // to the sealed wire restarts its broker.
        let unix = Paths {
            wire: Wire::Unix,
            ..p.clone()
        };
        assert!(!p.plist_is_current(&unix.plist_text()));
        assert_eq!(
            p.label(),
            unix.label(),
            "the label follows the root, not the wire"
        );
        // Every flag `on --tcp` refuses, it refuses before a path is resolved.
        let on = |tcp: Option<&str>, key: Option<&str>, remote: bool| {
            wire_for_on(&OnOpts {
                tcp: tcp.map(str::to_string),
                key_file: key.map(str::to_string),
                allow_remote: remote,
                ..OnOpts::default()
            })
        };
        assert_eq!(on(None, None, false), Ok(Wire::Unix));
        assert!(on(None, None, true).is_err(), "--allow-remote alone");
        assert!(on(Some("127.0.0.1:7000"), None, false).is_err());
        assert!(on(None, Some("/k"), false).is_err());
        if crate::transport::SEALED {
            assert!(
                on(Some("127.0.0.1:0"), Some("/k"), false).is_err(),
                "port 0"
            );
            assert!(
                on(Some("0.0.0.0:7000"), Some("/k"), false).is_err(),
                "remote"
            );
            assert_eq!(
                on(Some("0.0.0.0:7000"), Some("/k"), true),
                Ok(Wire::Sealed {
                    bind: Some("0.0.0.0:7000".to_string()),
                    dial: "127.0.0.1:7000".to_string(),
                    key_file: PathBuf::from("/k"),
                    allow_remote: true,
                })
            );
        } else {
            let e = on(Some("127.0.0.1:7000"), Some("/k"), false).expect_err("default build");
            assert!(e.contains("`sealed` cargo feature"), "{e}");
        }
    }

    /// THE RENDEZVOUS FILE CARRIES THE WIRE: a joined host's round-trips with
    /// its key file, and `ls` defaults `--tcp --key-file` (and the node's
    /// `--cap-file`/`--state`) WITH the broker — never onto a `--broker` the
    /// caller spelled out. The host that
    /// SERVES the sealed wire records it as `serves_tcp`/`serves_key_file`
    /// beside its socket, and its own tools default to the socket.
    #[test]
    fn a_sealed_rendezvous_defaults_its_wire_flags_with_its_broker() {
        let served = Rendezvous {
            fleet: "f".to_string(),
            broker: "/r/bus.sock".to_string(),
            node: "n-a".to_string(),
            cap_file: "/r/node.cap".to_string(),
            state: "/r/link-state".to_string(),
            command: "x".to_string(),
            key_file: None,
            serves_tcp: Some("127.0.0.1:7000".to_string()),
            serves_key_file: Some("/k/fleet.key".to_string()),
        };
        let text = served.render();
        assert!(
            text.contains("serves_tcp = \"127.0.0.1:7000\"")
                && text.contains("serves_key_file = \"/k/fleet.key\"")
                && !text.contains("transport ="),
            "{text}"
        );
        assert_eq!(Rendezvous::parse(&text), Ok(served.clone()));
        assert_eq!(
            fill_defaults(&[], &served).join(" "),
            "--fleet f --broker /r/bus.sock --cap-file /r/node.cap --state /r/link-state"
        );
        assert!(Rendezvous::parse(&text.replace("\"127.0.0.1:7000\"", "7000")).is_err());
        let r = Rendezvous {
            fleet: "f".to_string(),
            broker: "127.0.0.1:7000".to_string(),
            node: "n-a".to_string(),
            cap_file: "/r/node.cap".to_string(),
            state: "/r/link-state".to_string(),
            command: "x".to_string(),
            key_file: Some("/r/fleet.key".to_string()),
            serves_tcp: None,
            serves_key_file: None,
        };
        let text = r.render();
        assert!(text.contains("transport = \"tcp+sealed\""), "{text}");
        assert_eq!(Rendezvous::parse(&text), Ok(r.clone()));
        let fill = |args: &[&str]| -> String {
            let args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
            fill_defaults(&args, &r).join(" ")
        };
        assert_eq!(
            fill(&[]),
            "--fleet f --broker 127.0.0.1:7000 --cap-file /r/node.cap --state /r/link-state \
             --tcp --key-file /r/fleet.key"
        );
        assert_eq!(
            fill(&["--broker", "/tmp/b.sock"]),
            "--fleet f --broker /tmp/b.sock",
            "a spelled-out broker gets no wire flags, cap or state it did not ask for"
        );
        assert!(
            Rendezvous::parse(&text.replace("key_file = \"/r/fleet.key\"", "key_file = 1"))
                .is_err()
        );
        // The root is found from the cap on the sealed wire.
        assert_eq!(
            root_of(
                &crate::transport::Transport::Tcp,
                "127.0.0.1:7000",
                Some(&"/r/node.cap".to_string())
            ),
            Some(PathBuf::from("/r"))
        );
        assert_eq!(
            root_of(
                &crate::transport::Transport::Tcp,
                "127.0.0.1:7000",
                Some(&"/elsewhere/m7.cap".to_string())
            ),
            None,
            "a hand-written command's files have no root this command may act on"
        );
    }

    /// THE DEFAULTS are filled only for flags that are absent.
    #[test]
    fn rendezvous_defaults_fill_only_absent_flags() {
        let r = Rendezvous {
            fleet: "f".to_string(),
            broker: "/b".to_string(),
            node: "n-a".to_string(),
            cap_file: "/c".to_string(),
            state: "/s".to_string(),
            command: "x".to_string(),
            key_file: None,
            serves_tcp: None,
            serves_key_file: None,
        };
        let fill = |args: &[&str]| -> Vec<String> {
            let args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
            fill_defaults(&args, &r)
        };
        assert_eq!(
            fill(&["--attention"]).join(" "),
            "--fleet f --broker /b --cap-file /c --state /s --attention"
        );
        assert_eq!(
            fill(&["--fleet", "g", "--state", "/t"]).join(" "),
            "--broker /b --cap-file /c --fleet g --state /t"
        );
    }

    /// `aterm fabric on` lists an instance, then arms it and proves it on
    /// connections of their own, seconds later. Each of those reads the
    /// instance's token once it is connected: an instance replaced after the
    /// listing — its token file rewritten, then a new listener bound at the
    /// same socket, an update successor's order — is asked with ITS token.
    #[test]
    fn an_instance_replaced_after_the_listing_is_asked_with_its_own_token() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;
        let dir = std::env::temp_dir().join(format!("atlk-replaced-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        let sock = dir.join("a.sock");
        let token_file = dir.join("a.sock.token");
        std::fs::write(&token_file, "deadbeef\n").expect("token");
        // One connection: its first two lines, then `reply`.
        let serve = |listener: UnixListener, reply: &'static str| {
            std::thread::spawn(move || {
                let (conn, _) = listener.accept().expect("accept");
                let mut lines = BufReader::new(conn.try_clone().expect("clone"));
                let mut seen = Vec::new();
                for _ in 0..2 {
                    let mut line = String::new();
                    lines.read_line(&mut line).expect("a request line");
                    seen.push(line);
                }
                (&conn).write_all(reply.as_bytes()).expect("reply");
                seen
            })
        };
        let listing = serve(
            UnixListener::bind(&sock).expect("bind"),
            "OK supervised=0 state=- command=-\n",
        );
        let path = sock.to_str().expect("utf8").to_string();
        let inst = ask_instance(std::process::id(), path).expect("listed");
        assert_eq!(
            listing.join().expect("server"),
            ["AUTH deadbeef\n", "fabric status\n"]
        );
        std::fs::remove_file(&token_file).expect("unlink the old token");
        std::fs::write(&token_file, "cafef00d\n").expect("the new token");
        std::fs::remove_file(&sock).expect("unlink the old socket");
        let asked = serve(
            UnixListener::bind(&sock).expect("rebind"),
            "OK 1\n0 s-1 - alive zsh\n",
        );
        assert_eq!(sessions_of(&inst), ["s-1"]);
        assert_eq!(
            asked.join().expect("server"),
            ["AUTH cafef00d\n", "sessions\n"]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// THE NODE'S CAP NEVER LEAVES FOR A BROKER THE CALLER NAMED. A cap is
    /// minted against ONE broker's secret; defaulting the rendezvous file's
    /// `--cap-file` (and the `--state` holding the node identity it belongs
    /// to) onto an explicit `--broker` would present this node's credential to
    /// whatever the caller pointed at. So the pair comes with the broker, the
    /// rule `--key-file` already follows — on either wire — while `--fleet`
    /// (a name, not a secret) still defaults, and a cap or state the caller
    /// spelled out is kept as given.
    #[test]
    fn an_explicit_broker_gets_no_rendezvous_cap_or_state() {
        for (broker, key_file) in [
            ("/r/bus.sock", None),
            ("127.0.0.1:7000", Some("/r/fleet.key".to_string())),
        ] {
            let r = Rendezvous {
                fleet: "f".to_string(),
                broker: broker.to_string(),
                node: "n-a".to_string(),
                cap_file: "/r/node.cap".to_string(),
                state: "/r/link-state".to_string(),
                command: "x".to_string(),
                key_file,
                serves_tcp: None,
                serves_key_file: None,
            };
            let fill = |args: &[&str]| -> String {
                let args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
                fill_defaults(&args, &r).join(" ")
            };
            let got = fill(&["--broker", "/elsewhere/b.sock"]);
            assert_eq!(got, "--fleet f --broker /elsewhere/b.sock", "{broker}");
            assert!(
                !got.contains("/r/node.cap") && !got.contains("/r/link-state"),
                "{broker}: the node's cap/state followed a named broker: {got}"
            );
            assert_eq!(
                fill(&[
                    "--broker",
                    "/e.sock",
                    "--cap-file",
                    "/mine.cap",
                    "--state",
                    "/s"
                ]),
                "--fleet f --broker /e.sock --cap-file /mine.cap --state /s",
                "{broker}: the caller's own cap and state are kept"
            );
        }
    }
}
