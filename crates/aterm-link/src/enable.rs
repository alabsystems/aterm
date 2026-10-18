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
//! Beyond installing the broker and the bridge, `on` records where the fabric
//! is so every client finds it without arguments (the rendezvous file), arms
//! the instances already running, and PROVES the result with a message that
//! round-trips through the broker.
//! Every step is idempotent and says whether it changed anything; `--dry-run`
//! prints every step and touches nothing.
//!
//! FIRST, WHAT THE ROOT ALREADY IS ([`Role`], read back from the configured
//! `[fabric] command`, the rendezvous file and the installed broker job). A
//! root that JOINED another host's fleet is refused, exit 2, before anything
//! is written: `on` would move the host onto a broker of its own. On a root
//! whose job serves the sealed wire, a bare `on` keeps that wire (the bind,
//! key and `--allow-remote` the job runs), so re-running `on` restarts the
//! broker the joined hosts dial instead of dropping its TCP listener. And an
//! `on` with no `--fleet` keeps the fleet the configured bridge command names
//! (else the rendezvous file's) — `local` only on a root that records none —
//! and a `--fleet` that differs is refused until `off`.
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
//!    development seam redirects it) — an existing one another user can reach
//!    is made 0700, and the line names the mode it had. The broker's socket
//!    lives in it, and the directory bounds WHO can connect (`aterm link
//!    broker` checks no peer uid); the guard (step 6) bounds what a
//!    connection may SAY. The socket path must fit `sun_path` (104 bytes on
//!    macOS, one of them the NUL) and is refused before anything is written
//!    when it does not.
//! 3. The node id, `<root>/link-state/node`: PROVISIONED once, never re-minted.
//!    It is baked into six of the eight grants, so a new id abandons this node's
//!    mail lane. `off` keeps it for the same reason.
//! 4. The mint secret, `<root>/mint.secret`, 32 random bytes, 0600. An existing
//!    one is read through the ONE reader every minting and guarding verb uses
//!    ([`crate::transport::read_mint_secret`]), and a short one or one another
//!    user can read is refused, never replaced or chmodded: HMAC accepts any key
//!    length, so a truncated secret mints capabilities that look well-formed and
//!    seal nothing, and a secret that sat readable may already have been read.
//!    0600 bounds it to this uid, not to aterm: every process running as this
//!    uid can read it and mint any cap, so the guard (step 6) stops a process
//!    holding NO cap, never one that can read the secret — this host's uid is
//!    the fleet's root of trust (`docs/FABRIC-SECOND-HOST.md`).
//! 5. The cap file, `<root>/node.cap`: the eight grants of §8.2's node ring,
//!    minted for the node id ACTUALLY in the state dir under step 4's secret —
//!    a cap file that names another id, or whose tags that secret does not
//!    re-mint (the broker step 6 guards with it would refuse them), is
//!    re-minted. The mint is a TRANSACTION: every grant into a
//!    staging file, then one rename; a refused mint leaves the previous cap
//!    byte-identical and stops before broker supervision is touched.
//!    `crates/aterm-spec`'s `fabric_capability_publication_model` is the model
//!    of that order and `crates/aterm/tests/fabric_world/fabric_on.rs` runs the real command's
//!    effect trace through it.
//! 6. The broker, supervised: a launchd job on macOS (`KeepAlive`), a
//!    `systemd --user` unit on Linux (`Restart=always`), or `--service none` for
//!    a broker something else keeps alive. BOTH SUPERVISORS ARE THE LOGIN'S:
//!    they restart a broker that exits while the user is logged in, and
//!    neither keeps it past a logout (the LaunchAgent stops at logout; the unit,
//!    without `loginctl enable-linger`, shortly after the last session ends)
//!    or starts it at boot before a login. A LaunchAgent in the login user's
//!    `~/Library/LaunchAgents` starts at the next login; a redirected root's
//!    plist stays inside that root and needs `on` again after login. On one
//!    host that is every client's lifetime too; a host that serves others
//!    over `--tcp` is told so (the
//!    `scope` line, and `doctor`). The launchd job lives in `gui/<uid>`, which
//!    exists only while the user is logged in at the console, so the job is
//!    asked BY THAT DOMAIN (`launchctl print gui/<uid>/<label>`, never the
//!    legacy `list`, which reads the caller's — over SSH, not that one), and a
//!    uid with no GUI login is refused by name before the plist is written; a
//!    login with no `systemd --user` manager is refused the same way. THE
//!    LABEL IS DERIVED FROM THE ROOT:
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
//!    the real install. THE BROKER IS GUARDED: `link broker <sock> <log>
//!    --secret-file <root>/mint.secret`, so every attach must carry a
//!    capability minted under step 4's secret and every request is authorized
//!    against the grants attached — what makes the bridge's cap-forced `<src>`,
//!    `trust=human` and per-human fleet halts true rather than a convention.
//!    The job is PROBED, not assumed (connect, hello, every cap attached, the
//!    head query, and a cap minted under ANOTHER secret, which a guarded broker
//!    refuses); a broker that answers while the supervisor has no such job (a
//!    launchd label with no pid, a systemd unit with no `MainPID`) is NOT
//!    started over (that would put a second broker on the same socket and
//!    log): `on` names its pid and stops. The plist runs the broker DIRECTLY —
//!    ProgramArguments are the words, no `/bin/sh -c`, every value XML-escaped
//!    — and the unit runs the same words single-quoted; the broker verb unlinks
//!    a stale socket itself and refuses a live one, so neither unlinks it first.
//!    An installed job whose argv says anything else — an install from before
//!    the guard, whose argv has no `--secret-file`, included — is rewritten and
//!    restarted, and so is a current job whose broker checks no capability or
//!    RUNS A REPLACED BUILD (an update swapped the file its argv names after it
//!    started; [`crate::fabric::BrokerView::replaced`]). A broker of this build
//!    restarts itself on a swap: it exits when its executable is replaced, and
//!    `KeepAlive` / `Restart=always` start the new one. A job in the deleted
//!    fabric-enable.sh's `/bin/sh -c "rm -f …; exec …"` form is first READ
//!    for the broker it execs — what this root serves, `--tcp` included — and
//!    then rewritten and restarted like any other; an installed job of any
//!    other shape names no broker this can read, and `on` and `doctor` refuse
//!    it until it is fixed by hand ([`Role::of`]).
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
//!    (`aterm_uds::control_socket_dir`: `$XDG_RUNTIME_DIR/aterm`, else the
//!    per-user default, `~/Library/Application Support/aterm` on macOS),
//!    0600: fleet, broker, node, cap file, state dir and the bridge command.
//!    `aterm link ls` defaults its `--fleet --broker` from it, and its
//!    `--cap-file --state` only with the defaulted broker ([`fill_defaults`]);
//!    `aterm fabric` reads its command when aterm.toml has none.
//! 9. Every running instance `aterm ctl instances` lists is armed with
//!    `fabric attach <argv>` — the argv step 7 wrote, which an instance arms
//!    only because its aterm.toml now names it (it reads the file again at the
//!    attach and refuses any other argv, `ERR fabric not configured`); an
//!    instance from before that re-read records `[fabric] command` once, at
//!    launch, and takes the argv as given. An instance already supervised with this
//!    command — its flags AND its program, resolved ([`fabric::bridge_program`]) —
//!    is left alone and its program named; one supervised with a DIFFERENT command,
//!    or these flags under another program, is named (a supervisor arms once per
//!    process; only a relaunch changes it).
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
//! Every fix is spelled for what this host is ([`Role`]): the command that
//! re-applies it is `on --tcp …` on the host that serves the sealed wire and
//! the `join` on a joined one, and a joined host's broker is fixed on the host
//! that serves it. On the serving host a `scope` line says how long the
//! joined hosts' broker lives.
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

/// The fleet `on` joins unless `--fleet` says otherwise.
pub const DEFAULT_FLEET: &str = "local";

/// The launchd label (and systemd unit name) of the broker under the DEFAULT
/// root. Any other root derives its own — see [`Paths::label`].
pub const LABEL: &str = "systems.alab.astream-broker";

/// `sockaddr_un.sun_path` on macOS: 104 bytes, one of them the NUL.
pub const SUN_PATH_MAX: usize = 104;

/// The mint secret's name in the fabric root — and beside the socket, where
/// `aterm link broker` finds it when it is given no `--secret-file`
/// (`cli.rs`'s `broker`): the root's secret guards the root's socket.
pub const MINT_SECRET: &str = "mint.secret";

/// A mint secret is 32 bytes. Shorter is refused by the one reader every
/// minting and guarding verb uses ([`crate::transport::read_mint_secret`]).
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
    /// A `~/Library/LaunchAgents` job with `KeepAlive` (macOS), in launchd's
    /// `gui/<uid>` domain: restarted while the user is logged in at the
    /// console, stopped at logout. Started at the next login only when the
    /// plist is in the login user's `~/Library/LaunchAgents`, never at boot.
    Launchd,
    /// A `systemd --user` unit with `Restart=always` (Linux): restarted while
    /// the user has a session; without `loginctl enable-linger`, stopped
    /// shortly after the last one ends and started at the next login.
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
    /// `--fleet`; `None` keeps the configured fleet, or [`DEFAULT_FLEET`] on a new root.
    pub fleet: Option<String>,
    /// `--tcp <bind>`: serve the broker on the SEALED TCP wire at `<host:port>`
    /// as well as the Unix socket (a `sealed` build; module doc).
    pub tcp: Option<String>,
    /// `--key-file <path>`: the pre-shared key for `--tcp` — minted here, 64
    /// hex characters in a 0600 file, when the file does not exist yet.
    pub key_file: Option<String>,
    /// `--allow-remote`: let `--tcp` bind a non-loopback address.
    pub allow_remote: bool,
    /// `--durability strict|ordered|relaxed`: what the broker's ack promises
    /// ([`crate::durability`]), NAMED in the job's argv. `None` keeps the tier
    /// the installed job names ([`kept_durability`]), else none: the build's
    /// default.
    pub durability: Option<crate::durability::Durability>,
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
    /// The tier the broker's argv NAMES (`on --durability`, or the one the
    /// installed job names); `None` names none, and the broker runs the
    /// build's default ([`crate::durability::resolve`] says which).
    pub durability: Option<crate::durability::Durability>,
}

impl Paths {
    /// The tier the broker this root runs acks under: the named one, else the
    /// build's default.
    #[must_use]
    pub fn tier(&self) -> crate::durability::Durability {
        crate::durability::resolve(self.durability)
    }
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
            durability: None,
        })
    }

    /// [`Paths::resolve`] for `off`: the root is the one the fabric IS ON —
    /// the `--broker` the `[fabric] command` dials (a development build's
    /// `ATERM_FABRIC_COMMAND` seam first, as `aterm fabric` reads it), else
    /// the rendezvous file's — and only with none of them the environment's
    /// (module doc, `off`). So `doctor` reads the role of the fabric its
    /// report is about. Answers where the root came from beside the paths.
    ///
    /// # Errors
    ///
    /// As [`Paths::resolve`].
    pub fn resolve_for_off() -> Result<(Self, &'static str), String> {
        let mut p = Self::resolve(None)?;
        let configured =
            fabric::resolve_command(fabric::command_seam().as_deref(), p.config.as_deref())
                .ok()
                .flatten()
                .and_then(|(source, cmd)| {
                    let cfg = fabric::bridge_config(&cmd).ok()?;
                    let root = root_of(&cfg.transport, &cfg.broker, cfg.cap_files.first())?;
                    let source = match source {
                        fabric::Source::Env => "$ATERM_FABRIC_COMMAND",
                        _ => "[fabric] command",
                    };
                    Some((root, source))
                });
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
        self.root.join(MINT_SECRET)
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

    /// Whether the installed job is discovered at the next login. launchd
    /// loads only the login user's LaunchAgents directory, not a redirected
    /// root's plist; the systemd unit is enabled in its user manager.
    fn starts_at_login(&self, service: Service) -> bool {
        match service {
            Service::Launchd => self.login_home.as_ref().is_some_and(|home| {
                self.plist().parent() == Some(home.join("Library/LaunchAgents").as_path())
            }),
            Service::Systemd => true,
            Service::None => false,
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

    /// The broker's argv as launchd runs it: the words, no shell — ALWAYS
    /// GUARDED with the secret the caps were minted under, so every attach is
    /// checked and every request authorized against the grants attached (the
    /// cap-forced `<src>`, `trust=human` and per-human halts the bridge reads
    /// rest on that). On the Unix wire `broker <root>/bus.sock <log>
    /// --secret-file <root>/mint.secret`: the flag AFTER the socket, so the
    /// process table still reads `broker <sock>` ([`crate::fabric::broker_pid`]).
    /// On the sealed wire THIS host serves, `broker --tcp <bind> --key-file
    /// <k> --secret-file <root>/mint.secret [--allow-remote] --unix
    /// <root>/bus.sock <log>`, serving the root's socket for this host's own
    /// bridges too. A NAMED tier rides after the secret (`--durability
    /// relaxed`, and `--durability strict` too, so a job that chose strict
    /// keeps it under a build whose default differs); a job that names none
    /// carries nothing and runs the build's default, so the argv every earlier
    /// strict `on` wrote is still this one.
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
                argv.extend(crate::durability::argv_words(self.durability));
                if *allow_remote {
                    argv.push("--allow-remote".to_string());
                }
                argv.push("--unix".to_string());
                argv.push(self.sock());
                argv.push(self.log());
            }
            Wire::Unix | Wire::Sealed { bind: None, .. } => {
                argv.extend([
                    self.sock(),
                    self.log(),
                    "--secret-file".to_string(),
                    self.secret().to_string_lossy().into_owned(),
                ]);
                argv.extend(crate::durability::argv_words(self.durability));
            }
        }
        argv
    }

    /// Whether `text`, an installed plist, already runs THIS broker under THIS
    /// label with `KeepAlive` — by what it says, not by its bytes: the direct
    /// argv [`Paths::plist_text`] writes, in any layout (plistlib's tabs and
    /// line breaks included).
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
        plist_program_arguments(&flat).is_some_and(|argv| argv == self.broker_argv())
    }

    /// The broker command a shell-quoted supervisor line runs (the systemd
    /// unit): [`Paths::broker_argv`], every word single-quoted — `argv_safe`
    /// already refused a word with a quote in it, so none can close its quote
    /// early — so the unit and the plist cannot run different brokers.
    fn broker_words(&self) -> String {
        self.broker_argv()
            .iter()
            .map(|w| format!("'{w}'"))
            .collect::<Vec<_>>()
            .join(" ")
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

    /// The `systemd --user` unit. NO `ExecStartPre=rm -f <sock>`: the broker
    /// verb unlinks a STALE socket itself and refuses a LIVE one
    /// (`astream_broker::Broker::serve`), which an unconditional `rm` would
    /// get around — unlinking a running broker's socket out from under its
    /// clients before the second broker failed on the log's lock.
    ///
    /// ITS STDERR IS APPENDED TO `<root>/broker.err`, the file the launchd
    /// job's `StandardErrorPath` writes, so the broker's own reason it cannot
    /// run — the last line [`supervisor_last_word`] reads — reaches `aterm
    /// fabric` and doctor on Linux too, not only the journal. `append:` needs
    /// systemd 240 (2018-12) or later; an older one ignores the line with a
    /// warning and keeps writing to the journal, where `journalctl --user -u
    /// <label>` reads it.
    #[must_use]
    pub fn unit_text(&self) -> String {
        format!(
            "[Unit]\n\
             Description=aterm fabric broker ({label})\n\
             \n\
             [Service]\n\
             ExecStart=/bin/sh -c \"exec {broker}\"\n\
             WorkingDirectory={root}\n\
             StandardError=append:{root}/broker.err\n\
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
pub(crate) fn root_of(
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
    failure_hint(root, &format!("`launchctl print gui/{uid}/{label}`"))
}

/// What to read when systemd did not bring the socket up: `broker.err` when
/// the unit ran and wrote it ([`Paths::unit_text`]'s `StandardError=`), else
/// the journal — a unit written before that line, or a systemd older than
/// 240, sends the broker's stderr there.
fn systemd_failure_hint(root: &Path, label: &str) -> String {
    failure_hint(root, &format!("`journalctl --user -u {label}`"))
}

/// The supervisor's `broker.err` and its last word when there is one, else
/// `otherwise` — the supervisor's own view of the job.
fn failure_hint(root: &Path, otherwise: &str) -> String {
    let err = root.join("broker.err");
    match supervisor_last_word(root) {
        Some(last) => format!("read {} ({last})", err.display()),
        None => format!("{} was not written; {otherwise} says why", err.display()),
    }
}

/// How much of the END of `broker.err` [`supervisor_last_word`] reads: the
/// file is appended to by every start of the job and never trimmed, so its
/// last line is read from its tail, never by reading the whole.
const LAST_WORD_TAIL: u64 = 4096;

/// THE BROKER'S OWN LAST WORD: the last non-empty line its supervisor
/// captured from its stderr into `<root>/broker.err` (the plist's
/// `StandardErrorPath`), with how long ago the file was written — `written
/// <age> ago: <line>`. It is the broker's own reason it is not running (a log
/// it refuses to open, an address in use, a key it cannot read), which
/// `aterm fabric` and doctor show beside "does not answer" rather than only
/// prescribing a restart into the same refusal; `fabric on` reads it through
/// [`launchd_failure_hint`]. The age is the honest caveat: the file outlives
/// the failure it records, so an old line beside a broker that does not
/// answer for another reason says how old it is. `None` when the file is
/// missing or holds nothing but whitespace. Control characters are dropped:
/// the line goes to a terminal.
pub(crate) fn supervisor_last_word(root: &Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(root.join("broker.err")).ok()?;
    let meta = f.metadata().ok()?;
    let from = meta.len().saturating_sub(LAST_WORD_TAIL);
    f.seek(SeekFrom::Start(from)).ok()?;
    let mut tail = Vec::new();
    f.take(LAST_WORD_TAIL).read_to_end(&mut tail).ok()?;
    let text = String::from_utf8_lossy(&tail);
    let line: String = text
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())?
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    let age_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.elapsed().ok())
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    Some(format!(
        "written {} ago: {line}",
        crate::fabric::age(age_ms)
    ))
}

/// The broker words of a unit [`Paths::unit_text`] wrote: its `ExecStart`'s
/// `/bin/sh -c "exec '<word>' …"` script, read by [`sh_c_words`] — so a unit
/// in the deleted fabric-enable.sh's `rm -f …; exec …` form reads too.
/// `None` for a unit of another shape.
fn unit_argv(text: &str) -> Option<Vec<String>> {
    let line = text
        .lines()
        .find_map(|l| l.strip_prefix("ExecStart=/bin/sh -c \""))?;
    sh_c_words(line.strip_suffix('"')?)
}

/// The command words of a supervisor's `sh -c` script, for the ONE shape aterm
/// (or the deleted fabric-enable.sh) ever wrote: an optional `rm -f <word>;`,
/// then `exec` and one or more words. A word is a single-quoted run with no
/// quote in it (`''` is the empty word) or a bare run of characters no shell
/// gives a meaning to; words are separated by spaces or tabs, and the whole
/// script may carry leading and trailing whitespace.
///
/// NOT A SHELL PARSER: any other shape — another leading command, a second
/// `;`, `&&`, a double quote, a backslash, `$`, a glob, no `exec`, nothing
/// after `exec`, two words run together — is `None`, so an installed job this
/// cannot read for certain stays an unknown role ([`Role::of`] refuses it)
/// rather than being guessed at.
fn sh_c_words(script: &str) -> Option<Vec<String>> {
    /// One shell token: a word, or the `;` that ends a command.
    enum Tok {
        Word(String),
        Semi,
    }
    let bare =
        |c: char| !c.is_whitespace() && !c.is_control() && !"'\"\\$`;&|<>()*?![]{}~#".contains(c);
    let mut toks = Vec::new();
    let mut chars = script.trim().chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            ' ' | '\t' => {
                chars.next();
                continue;
            }
            ';' => {
                chars.next();
                toks.push(Tok::Semi);
            }
            '\'' => {
                chars.next();
                let mut word = String::new();
                loop {
                    match chars.next()? {
                        '\'' => break,
                        c => word.push(c),
                    }
                }
                toks.push(Tok::Word(word));
            }
            c if bare(c) => {
                let mut word = String::new();
                while let Some(&c) = chars.peek().filter(|&&c| bare(c)) {
                    word.push(c);
                    chars.next();
                }
                toks.push(Tok::Word(word));
            }
            _ => return None,
        }
        // A word ends at a separator, a `;`, or the end: `a'b'` is
        // two runs of one shell word, a shape no writer of these jobs made.
        if matches!(toks.last(), Some(Tok::Word(_)))
            && chars
                .peek()
                .is_some_and(|&c| !matches!(c, ' ' | '\t' | ';'))
        {
            return None;
        }
    }
    let rest = match toks.as_slice() {
        [Tok::Word(rm), Tok::Word(f), Tok::Word(_), Tok::Semi, rest @ ..]
            if rm == "rm" && f == "-f" =>
        {
            rest
        }
        rest => rest,
    };
    let [Tok::Word(exec), words @ ..] = rest else {
        return None;
    };
    if exec != "exec" || words.is_empty() {
        return None;
    }
    words
        .iter()
        .map(|t| match t {
            Tok::Word(w) => Some(w.clone()),
            Tok::Semi => None,
        })
        .collect()
}

/// The command a supervisor job's argv runs. The deleted fabric-enable.sh
/// installed `/bin/sh -c "rm -f '<sock>'; exec '<aterm>' link broker …"`;
/// that job is READ for the broker words it execs, so the role it serves is
/// known, while [`Paths::plist_is_current`] still calls it not current, so
/// `on` rewrites it to the direct argv and restarts it (module doc, step 6).
/// `None` for an `sh -c` script [`sh_c_words`] cannot read for certain; any
/// other argv is the command itself.
fn supervised_argv(argv: Vec<String>) -> Option<Vec<String>> {
    match argv.as_slice() {
        [sh, c, script] if Path::new(sh).file_name().is_some_and(|n| n == "sh") && c == "-c" => {
            sh_c_words(script)
        }
        _ => Some(argv),
    }
}

// ---------------------------------------------------------------------------
// what this root is to its fleet
// ---------------------------------------------------------------------------

/// What this host is to its fleet, READ BACK from what `on` and `join` left:
/// the configured `[fabric] command` (else the rendezvous file) and this
/// root's installed broker job. A bare `on` is the ONE-HOST command: on a
/// root that JOINED another host's fleet it would move the host onto a broker
/// of its own, out of the fleet, and on a root that SERVES the sealed wire it
/// would rewrite the job without `--tcp`, cutting every joined host off. So
/// `on` reads this before it writes anything, and every remedy `doctor`
/// prints is spelled for it ([`fix_for`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Role {
    /// The one-host fabric, or nothing configured yet.
    OneHost {
        /// The configured fleet; absent only before one was recorded.
        fleet: Option<String>,
    },
    /// This root serves the fleet's broker on the sealed wire as well as its
    /// socket (`on --tcp`).
    Serves(Served),
    /// This root JOINED the fleet whose broker another host serves (`join`).
    Joined(Joined),
}

/// The sealed wire a root serves, as `on --tcp` spelled it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Served {
    /// The existing bridge's fleet, when configuration records one.
    pub fleet: Option<String>,
    /// `--tcp <bind>` as the installed job runs it; `None` when only the
    /// rendezvous file says so (a broker something else supervises), which
    /// records the address a joining host dials and not the bind.
    pub bind: Option<String>,
    /// The rendezvous file's `serves_tcp`, when the bind is not known.
    pub dial: Option<String>,
    /// `--key-file <k>`.
    pub key_file: String,
    /// `--allow-remote`.
    pub allow_remote: bool,
    /// The supervisor whose job says so; `None` for the rendezvous file.
    pub job: Option<Service>,
    /// Whether the job's installation location is discovered at login.
    pub starts_at_login: bool,
}

/// The fleet a root joined, as its bridge command dials it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Joined {
    /// The fleet named by the installed bridge command.
    pub fleet: String,
    /// The remote broker, `<host:port>`.
    pub broker: String,
    /// The installed key, as the bridge command names it.
    pub key_file: Option<String>,
    /// The installed cap, as the bridge command names it.
    pub cap_file: Option<String>,
    /// The bridge command's `--accept-from`, as written.
    pub accept_from: Option<String>,
}

/// The value after `name` in a flag list.
fn flag_value(flags: &[String], name: &str) -> Option<String> {
    flags
        .windows(2)
        .rfind(|w| w[0] == name)
        .map(|w| w[1].clone())
}

/// Why [`Role::of`] refused a root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoleError {
    /// The installed job runs a command that is not the broker.
    NotABroker,
    /// The installed job's command line has a shape aterm never wrote:
    /// `field` (`ProgramArguments`, `ExecStart`) of the file at `path`.
    UnreadableJob {
        path: std::path::PathBuf,
        field: &'static str,
    },
    /// Anything else: unreadable or invalid configuration or rendezvous, or
    /// a broker job missing a flag it needs.
    Invalid(String),
}

impl std::fmt::Display for RoleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotABroker => f.write_str(
                "the installed job does not name a recognized broker command; fix it before \
                 running `on`",
            ),
            Self::UnreadableJob { path, field } => {
                write!(f, "{}: invalid broker {field}", path.display())
            }
            Self::Invalid(why) => f.write_str(why),
        }
    }
}

impl From<String> for RoleError {
    fn from(why: String) -> Self {
        Self::Invalid(why)
    }
}

impl Role {
    /// What the root `p` is to its fleet. Existing metadata that cannot be
    /// read or understood is a refusal, never evidence of a fresh root.
    ///
    /// # Errors
    ///
    /// Unreadable or invalid configuration, rendezvous, or installed job.
    pub fn of(p: &Paths) -> Result<Self, RoleError> {
        let rendezvous = p
            .rendezvous
            .as_deref()
            .map(role_text)
            .transpose()?
            .flatten()
            .map(|text| Rendezvous::parse(&text).map_err(|e| format!("the rendezvous file: {e}")))
            .transpose()?;
        let (fleet, joined) = joined_fleet(p.config.as_deref(), rendezvous.as_ref())?;
        if let Some(joined) = joined {
            return Ok(Role::Joined(joined));
        }
        Ok(served_wire(p, fleet.clone(), rendezvous.as_ref())?
            .map_or(Role::OneHost { fleet }, Role::Serves))
    }

    fn fleet(&self) -> Option<&str> {
        match self {
            Role::OneHost { fleet } => fleet.as_deref(),
            Role::Serves(s) => s.fleet.as_deref(),
            Role::Joined(j) => Some(&j.fleet),
        }
    }

    /// The command that re-applies this root AS IT IS — every remedy that
    /// re-runs `on` is spelled with it: `on` on a one-host root, `on --tcp …`
    /// as this root serves the wire, and on a joined root the `join` that
    /// made it, from the files `join` installed.
    #[must_use]
    pub fn again(&self) -> String {
        match self {
            Role::OneHost { fleet } => format!("`aterm fabric on{}`", fleet_flag(fleet.as_deref())),
            Role::Serves(s) => format!("`{}`", s.on_command()),
            Role::Joined(j) => format!("`{}`", j.join_command(None)),
        }
    }
}

impl Served {
    /// `aterm fabric on --tcp <bind> --key-file <k> [--allow-remote]`, as
    /// this root serves it (`<bind>` when only the dial address is known),
    /// with `--service none` when no job of `on`'s supervises the broker.
    fn on_command(&self) -> String {
        let mut c = format!(
            "aterm fabric on{} --tcp {} --key-file {}",
            fleet_flag(self.fleet.as_deref()),
            self.bind
                .as_deref()
                .map_or_else(|| "<bind>".to_string(), |b| safe(b, 128)),
            safe(&self.key_file, 256)
        );
        if self.allow_remote {
            c.push_str(" --allow-remote");
        }
        if self.job.is_none() {
            c.push_str(" --service none");
        }
        c
    }

    /// How long the broker the joined hosts dial lives, and what they see
    /// while it is down.
    fn scope(&self) -> String {
        let lives = self
            .job
            .and_then(|service| broker_scope(service, self.starts_at_login))
            .unwrap_or_else(|| {
                "run by a supervisor of your own (this root has no job of `on`'s): it lives as \
                 long as that keeps it"
                    .to_string()
            });
        let restart = if self.job == Some(Service::Launchd) && !self.starts_at_login {
            format!("; after login, run `{}` again", self.on_command())
        } else {
            String::new()
        };
        format!(
            "the broker every joined host dials is {lives}{restart}; while it is down they read \
             fabric=stalled and queue their posts, and nothing is lost"
        )
    }
}

impl Joined {
    /// The `join` that re-applies this root, from the files `join` installed
    /// — `cap` in place of the installed cap when a new one is to be joined.
    fn join_command(&self, cap: Option<&str>) -> String {
        let mut c = format!(
            "aterm fabric join --broker {} --tcp --key-file {} --cap-file {}",
            safe(&self.broker, 128),
            self.key_file
                .as_deref()
                .map_or_else(|| "<root>/fleet.key".to_string(), |k| safe(k, 256)),
            cap.map(str::to_string)
                .or_else(|| self.cap_file.as_deref().map(|c| safe(c, 256)))
                .unwrap_or_else(|| "<root>/node.cap".to_string())
        );
        if let Some(accept) = &self.accept_from {
            c.push_str(" --accept-from ");
            c.push_str(&safe(accept, 256));
        }
        c
    }

    /// The remedy for a broker that does not answer: it is the fleet's, so it
    /// is fixed on the host that serves it, never here.
    fn broker_fix(&self) -> String {
        format!(
            "the broker at {} is the fleet's, on the host that serves it: `aterm fabric \
             doctor` there names the fix (a broker supervised by `on` can stop when that host's \
             user logs out; log in there and follow that doctor's remedy). Nothing to run here: \
             this host joined that fleet, its bridges redial on their own, and a one-host `on` here \
             would leave the fleet",
            safe(&self.broker, 128)
        )
    }

    /// The remedy for a LAPSED cap ([`renew_remedy`]): the same ring minted
    /// again on the host that serves the fleet, with a later expiry, and
    /// joined here over the one that lapsed. Nothing restarts: the bridge reads
    /// its cap file again before every dial.
    fn renew_fix(&self) -> String {
        format!(
            "renew it on the host that serves the fleet: `aterm fabric mint-for <this node> \
             --out <cap> --expires <N>d` there mints the same ring with a later `exp=`; then \
             here `{}` replaces {} with it, and the running bridge attaches it at its next \
             redial (it reads its cap file again before every dial), with no restart",
            self.join_command(Some("<cap>")),
            self.cap_file
                .as_deref()
                .map_or_else(|| "<root>/node.cap".to_string(), |c| safe(c, 256))
        )
    }

    /// The remedy for a cap the broker will not take: a joined host's cap is
    /// minted on the host that serves the fleet, then joined here.
    fn cap_fix(&self) -> String {
        format!(
            "a joined host's cap is minted on the host that serves the fleet: `aterm fabric \
             mint-for <this node> --out <cap>` there, then here `{}`",
            self.join_command(Some("<cap>"))
        )
    }
}

/// The fleet flag a repair must carry. The default remains the usual short
/// spelling, while a named fleet never silently becomes `local`.
fn fleet_flag(fleet: Option<&str>) -> String {
    fleet
        .filter(|f| *f != DEFAULT_FLEET)
        .map_or_else(String::new, |f| format!(" --fleet {f}"))
}

/// Read one existing metadata file. Only absence means it records no role.
fn role_text(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e)
            if e.kind() == io::ErrorKind::NotFound
                && std::fs::symlink_metadata(path)
                    .is_err_and(|e| e.kind() == io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// The raw role flags preserve recognition of a sealed bridge in a default
/// build. No key or cap is opened merely to identify the configured role.
fn joined_fleet(
    config: Option<&Path>,
    rendezvous: Option<&Rendezvous>,
) -> Result<(Option<String>, Option<Joined>), String> {
    let configured = config
        .map(|path| {
            role_text(path)?
                .map(|text| {
                    fabric::command_in_toml(&text).map_err(|e| format!("{}: {e}", path.display()))
                })
                .transpose()
                .map(Option::flatten)
        })
        .transpose()?
        .flatten();
    let command = configured
        .as_deref()
        .or_else(|| rendezvous.map(|r| r.command.as_str()));
    let Some(command) = command else {
        return Ok((None, None));
    };
    let flags = fabric::serve_flags(command)
        .ok_or_else(|| "the existing [fabric] command is not a recognized bridge; fix it or run `aterm fabric off` before replacing it".to_string())?;
    let required = |name| {
        flag_value(&flags, name)
            .filter(|v| !v.is_empty() && !v.starts_with("--"))
            .ok_or_else(|| {
                format!(
                    "the existing bridge command has no valid {name}; fix it before running `on`"
                )
            })
    };
    let fleet = required("--fleet")?;
    if !crate::subject::is_fleet(&fleet) {
        return Err(format!(
            "the existing bridge command's fleet {} is invalid",
            safe(&fleet, 64)
        ));
    }
    let broker = required("--broker")?;
    if !flags.iter().any(|f| f == "--tcp") {
        if flags.iter().any(|f| f == "--key-file") {
            return Err("the existing bridge command has --key-file without --tcp".to_string());
        }
        return Ok((Some(fleet), None));
    }
    let key_file = if flags.iter().any(|f| f == "--key-file") {
        Some(required("--key-file")?)
    } else {
        None
    };
    Ok((
        Some(fleet.clone()),
        Some(Joined {
            fleet,
            broker,
            key_file,
            cap_file: flag_value(&flags, "--cap-file"),
            accept_from: flag_value(&flags, "--accept-from"),
        }),
    ))
}

/// The broker job this root has installed — its supervisor and its argv, the
/// plist's `ProgramArguments` or the unit's `ExecStart` words, the platform's
/// own first — or `None` when neither file is there. A file that is there and
/// does not parse is an error, never "no job".
fn installed_job(p: &Paths) -> Result<Option<(Service, Vec<String>)>, RoleError> {
    let plist = role_text(&p.plist())?
        .map(|text| {
            plist_program_arguments(&collapse_between_tags(&text))
                .and_then(supervised_argv)
                .filter(|argv| !argv.is_empty())
                .ok_or_else(|| RoleError::UnreadableJob {
                    path: p.plist(),
                    field: "ProgramArguments",
                })
        })
        .transpose()?;
    let unit = role_text(&p.unit())?
        .map(|text| {
            unit_argv(&text)
                .filter(|argv| !argv.is_empty())
                .ok_or_else(|| RoleError::UnreadableJob {
                    path: p.unit(),
                    field: "ExecStart",
                })
        })
        .transpose()?;
    Ok(if cfg!(target_os = "linux") {
        unit.map(|argv| (Service::Systemd, argv))
            .or_else(|| plist.map(|argv| (Service::Launchd, argv)))
    } else {
        plist
            .map(|argv| (Service::Launchd, argv))
            .or_else(|| unit.map(|argv| (Service::Systemd, argv)))
    })
}

/// The durability `on` runs when its flags name none: the tier the installed
/// job's broker argv NAMES ([`crate::durability::Durability::named_of_argv`]),
/// with the job it was read from — so re-running `on` to repair a relaxed root
/// restarts the broker it had, not a strict one, and a job that named strict
/// stays named under a build whose default differs — else none, kept from
/// nothing: the build's default.
///
/// # Errors
///
/// An installed job that cannot be read ([`installed_job`]).
fn kept_durability(
    p: &Paths,
) -> Result<(Option<crate::durability::Durability>, Option<String>), String> {
    let Some((service, argv)) = installed_job(p).map_err(|e| e.to_string())? else {
        return Ok((None, None));
    };
    let job = match service {
        Service::Systemd => format!("the systemd --user unit {}", p.unit().display()),
        _ => format!("the launchd job {}", p.plist().display()),
    };
    let named = crate::durability::Durability::named_of_argv(&argv)
        .ok_or_else(|| format!("{job}: cannot determine the broker durability from its argv"))?;
    Ok((named, Some(job)))
}

/// Read the installed job rather than treating a parse or I/O failure as a
/// Unix-only root. A missing job can still leave a served wire in rendezvous.
fn served_wire(
    p: &Paths,
    fleet: Option<String>,
    rendezvous: Option<&Rendezvous>,
) -> Result<Option<Served>, RoleError> {
    if let Some((service, argv)) = installed_job(p)? {
        let broker = argv.get(1).is_some_and(|word| word == "broker")
            || (argv.get(1).is_some_and(|word| word == "link")
                && argv.get(2).is_some_and(|word| word == "broker"));
        if !broker {
            return Err(RoleError::NotABroker);
        }
        if argv.iter().any(|word| word == "--tcp") {
            let required = |name| {
                flag_value(&argv, name)
                    .filter(|v| !v.is_empty() && !v.starts_with("--"))
                    .ok_or_else(|| {
                        format!(
                        "the installed broker job has no valid {name}; fix it before running `on`"
                    )
                    })
            };
            return Ok(Some(Served {
                fleet,
                bind: Some(required("--tcp")?),
                dial: None,
                key_file: required("--key-file")?,
                allow_remote: argv.iter().any(|w| w == "--allow-remote"),
                job: Some(service),
                starts_at_login: p.starts_at_login(service),
            }));
        }
        if argv.iter().any(|word| word == "--key-file") {
            return Err("the installed broker job has --key-file without --tcp"
                .to_string()
                .into());
        }
    }
    let Some(r) = rendezvous else {
        return Ok(None);
    };
    if !crate::subject::is_fleet(&r.fleet) {
        return Err("the rendezvous file's fleet is invalid".to_string().into());
    }
    match (&r.serves_tcp, &r.serves_key_file) {
        (None, None) => Ok(None),
        (Some(dial), Some(key_file)) => Ok(Some(Served {
            fleet,
            bind: None,
            dial: Some(dial.clone()),
            key_file: key_file.clone(),
            allow_remote: false,
            job: None,
            starts_at_login: false,
        })),
        _ => Err("the rendezvous file has an incomplete served wire; fix serves_tcp and serves_key_file before running `on`".to_string().into()),
    }
}

/// How long a broker the supervisor `on` installs lives — the joined hosts'
/// broker, on a host that serves them. `None` for `--service none`.
fn broker_scope(service: Service, starts_at_login: bool) -> Option<String> {
    match service {
        Service::Launchd => {
            let restart = if starts_at_login {
                "started again at the next login (not at boot)"
            } else {
                "its plist is outside the login user's Library/LaunchAgents, so re-run the same \
                 `on` after login (it is not loaded automatically at login or boot)"
            };
            Some(format!(
                "a LaunchAgent in launchd's gui/{} domain: restarted while this user is logged in \
                 at the console, stopped at logout; {restart}",
                uid()
            ))
        }
        Service::Systemd => Some(
            "a systemd --user unit: restarted while this user has a session and, unless \
             `loginctl enable-linger` is on for them, stopped shortly after the last one ends \
             and started again at the next login (not at boot)"
                .to_string(),
        ),
        Service::None => None,
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
    which_in(std::ffi::OsStr::new(name), &path)
}

/// Resolve a bare invocation name on the supplied PATH, retaining symlinks.
pub(crate) fn which_in(name: &std::ffi::OsStr, path: &std::ffi::OsStr) -> Option<PathBuf> {
    std::env::split_paths(path)
        .map(|dir| dir.join(name))
        .find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// POSIX `cksum`'s CRC: the suffix of a non-default root's label
/// ([`Paths::label`]), the number `printf '%s' "$ROOT" | cksum` prints, so a
/// shell can name the job a custom root runs.
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
/// grants' AUTHORITY (mode, principal, filter) is not exactly these, so
/// adding, dropping or rewording one entry invalidates every cap already
/// minted on every host of a fleet. Their LIFETIME is free: `mint-for
/// --expires` stamps an `exp=` on each, and that is the same ring.
///
/// That is why the last two stay although nothing this node runs reads
/// `ro:/f/<F>/term/<node>/>` or writes `rw,p=<node>:/f/<F>/term/<node>/*/screen`
/// any more: an older node may still publish on those subjects, the read/write
/// split is the §8.2 property that stops a node forging a driver into its own
/// sessions, and narrowing the ring would refuse every cap in the field.
#[must_use]
pub fn node_grants(fleet: &str, node: &str) -> Vec<String> {
    vec![
        // its own `pub/<node>/` subtree: presence, ev, say, ack, control
        format!("rw,p={node}:/f/{fleet}/pub/{node}/>"),
        // every member's presence and digest
        format!("ro:/f/{fleet}/pub/>"),
        // the fleet broadcast face, READ ONLY: a node holds no halt authority
        format!("ro:/f/{fleet}/fleet/>"),
        // talk to anyone, AS this node, any kind; the `*` kind segment is what
        // pins an `in` subject to seven segments
        format!("rw,p={node}:/f/{fleet}/in/*/*/{node}/*"),
        // read its own inbox lanes
        format!("ro:/f/{fleet}/in/{node}/>"),
        // its consumer-group NAMES (never published to, never delivered)
        format!("rw,p={node}:/f/{fleet}/cur/{node}/>"),
        // the drive face of the sessions it hosts, READ ONLY (see above)
        format!("ro:/f/{fleet}/term/{node}/>"),
        // the screen face of the sessions it hosts, write only (see above)
        format!("rw,p={node}:/f/{fleet}/term/{node}/*/screen"),
    ]
}

/// The grants of a PERSON's ring (§8.2's human ring), for `person` — an `h-`
/// human or an `a-` agent that holds its own cap rather than speaking as a
/// session behind a node — on `fleet`.
///
/// §8.2's seven, minus the drive grant `rw,p=<h>:/f/<F>/term/*/*/in/<h>`: no
/// bus record reaches a PTY (`bridge.rs`, "The path to a PTY: there is none"),
/// so nothing reads that face and a person's cap does not carry a write onto
/// it. What the rest buy is exactly what `aterm fabric halt|inbox|post`
/// ([`crate::person`]) use: a HUMAN's own halt face (the only fleet-face
/// write anybody is minted; an `a-` agent gets no fleet write), a send lane to
/// any session in the person's name (`<src>` cap-forced, so it arrives
/// `trust=human` for an `h-`), the read
/// lane every node's `post to=<person>` lands on, and the name of the inbox
/// cursor over it. `ro:/f/<F>/fleet/>` is NOT here, as it is not in §8.2: a
/// human withdraws only their own halt, and reads another's with node.cap.
///
/// Never a node's ring: [`crate::join::node_ring_of`] refuses this set, so a
/// person's cap cannot be joined as a host.
///
/// THE HALT GRANT IS A HUMAN'S ALONE. Every bridge applies a halt row only
/// under an `h-` principal (§5.3: "a human publishes
/// `/f/<F>/fleet/h-<name>/halt`"; `bridge.rs` drops any other at the tail and
/// at the attach-time read), so an `a-` service's ring carries no fleet-face
/// write: a halt it could publish would hold nothing while `aterm fabric`
/// and `notify --on halt` reported it standing.
#[must_use]
pub fn person_grants(fleet: &str, person: &str) -> Vec<String> {
    let mut ring = Vec::with_capacity(6);
    if person.starts_with("h-") {
        // the halt: `fleet/<person>/halt`, and nobody else's
        ring.push(format!("rw,p={person}:/f/{fleet}/fleet/{person}/>"));
    }
    ring.extend([
        // talk to any session, AS this person, any kind (the `*` kind segment
        // pins an `in` subject to seven segments, as for a node)
        format!("rw,p={person}:/f/{fleet}/in/*/*/{person}/*"),
        // its own presence face, under the literal owner `p`
        format!("rw,p={person}:/f/{fleet}/pub/p/{person}/>"),
        // its own mail: every node's posts, receipts and notices to it
        format!("ro:/f/{fleet}/in/p/{person}/>"),
        // its inbox cursor's NAME (never published to, never delivered)
        format!("rw,p={person}:/f/{fleet}/cur/p/{person}/>"),
        // every member's presence, and each node's halt ack
        format!("ro:/f/{fleet}/pub/>"),
    ]);
    ring
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
        #[expect(
            clippy::disallowed_methods,
            reason = "aterm-link's flushes are the Ordered-tier work's to classify; the full flush is kept until then"
        )]
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
pub(crate) fn create_temp_beside(path: &Path, mode: u32) -> io::Result<(PathBuf, std::fs::File)> {
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
pub(crate) const LOCK_OPEN_FLAGS: i32 = 0x0100 | 0x0004;
/// arm and arm64 Linux spell `O_NOFOLLOW` 0o100000 (`aterm_uds::ownerlock`).
#[cfg(all(target_os = "linux", any(target_arch = "aarch64", target_arch = "arm")))]
pub(crate) const LOCK_OPEN_FLAGS: i32 = 0o100_000 | 0o4000;
#[cfg(all(
    target_os = "linux",
    not(any(target_arch = "aarch64", target_arch = "arm"))
))]
pub(crate) const LOCK_OPEN_FLAGS: i32 = 0o400_000 | 0o4000;
/// No audited values: the pre-open check and the descriptor check below
/// still refuse anything at the lock's name that is not a regular file.
#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
pub(crate) const LOCK_OPEN_FLAGS: i32 = 0;

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
                    ));
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

/// Whether the cap file at `path` was minted for `node` under the current
/// secret: every tag verifies, and at least one grant binds `node` as its
/// principal. A recreated secret requires fresh caps even for the same node.
fn cap_is_for(path: &Path, node: &str, secret: &[u8]) -> Result<usize, String> {
    let caps = crate::bridge::read_cap_file(&path.to_string_lossy()).map_err(|e| e.to_string())?;
    if !caps.iter().any(|c| {
        astream_cap::Grant::parse(&c.grant).is_ok_and(|g| g.principal.as_deref() == Some(node))
    }) {
        return Err(format!("minted for a different node id, not {node}"));
    }
    if !caps.iter().all(|c| {
        if astream_cap::Grant::parse(&c.grant).is_err() {
            return false;
        }
        let Ok(tag) = c.tag.as_slice().try_into() else {
            return false;
        };
        astream_cap::verify(
            secret,
            &astream_cap::Capability {
                filter: c.grant.clone(),
                tag,
            },
        )
    }) {
        return Err("not minted under the current mint secret".to_string());
    }
    Ok(caps.len())
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
/// A secret the one reader refuses, a refused mint, or a staging/rename
/// failure — in every case the previous cap file is untouched and the staging
/// file is gone.
fn mint_caps(p: &Paths, node: &str) -> Result<usize, String> {
    let secret_path = p.secret().to_string_lossy().into_owned();
    let key = crate::transport::read_mint_secret(&secret_path).map_err(|e| {
        trace("Reject");
        format!(
            "{} — fix it, or move it aside and run again",
            crate::transport::describe(&secret_path, &e)
        )
    })?;
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
/// when both exist at that mode, else created (a dry run writes nothing). An existing one
/// another user can reach (a restore, a `cp -r`) is made 0700 and the line names the mode it
/// had: the root holds the socket, the mint secret and the cap, so it is never reported 0700
/// without its mode having been read. `false` stops the run.
pub(crate) fn root_step(p: &Paths, state: &Path, out: &mut Out) -> bool {
    if p.root.is_dir() && state.is_dir() {
        use std::os::unix::fs::PermissionsExt;
        let mut wide = Vec::new();
        for d in [p.root.as_path(), state] {
            match std::fs::metadata(d) {
                Ok(m) if m.permissions().mode() & 0o077 == 0 => {}
                Ok(m) => wide.push((d, m.permissions().mode() & 0o777)),
                Err(e) => {
                    out.fail("root", &format!("{}: {e}", d.display()));
                    return false;
                }
            }
        }
        if wide.is_empty() {
            out.already("root", &format!("{} (0700)", p.root.display()));
            return true;
        }
        let was: Vec<String> = wide
            .iter()
            .map(|(d, mode)| format!("{} was {mode:04o}", d.display()))
            .collect();
        out.done(
            "root",
            &format!("chmod 700 — {}", was.join(", ")),
            &format!("made 0700 ({})", was.join(", ")),
        );
        if !out.dry_run {
            for (d, _) in &wide {
                if let Err(e) = std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700))
                {
                    out.fail("root", &format!("{}: {e}", d.display()));
                    return false;
                }
            }
        }
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
        log: crate::mirror::LogMode::Off,
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
#[cfg(target_os = "macos")]
pub(crate) fn launchctl_list() -> String {
    aterm_uds::spawnfd::command("launchctl")
        .arg("list")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// What launchd's `gui/<uid>` domain — the one `on` bootstraps the broker
/// into and `off` boots it out of — says of a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LaunchdJob {
    /// The domain holds no such job.
    Absent,
    /// There is no `gui/<uid>` domain: this uid has no GUI login (an SSH
    /// session alone opens none), so no job can run in it.
    NoGuiLogin,
    /// Loaded; `Some(pid)` while it runs.
    Loaded(Option<u32>),
}

/// `launchctl`'s exit status for "Could not find specified domain"
/// (`launchctl error 112`): `print gui/<uid>/…` for a uid with no GUI login.
const LAUNCHCTL_NO_DOMAIN: i32 = 112;

/// The job `label`, asked BY DOMAIN: `launchctl print gui/<uid>/<label>`.
/// Not the legacy `launchctl list`, which reads the CALLER's domain — over
/// SSH the background `user/<uid>` session, not the `gui/<uid>` every write
/// here names — so a job the console session runs would read absent there:
/// `on` would call launchd's own broker unmanaged, and `off` would skip its
/// bootout and say it stopped. Presence is the exit status; the pid is the
/// `pid = ` line, read as `aterm-keeper` reads it.
pub(crate) fn launchd_job(label: &str) -> LaunchdJob {
    let Ok(out) = aterm_uds::spawnfd::command("launchctl")
        .args(["print", &format!("gui/{}/{label}", uid())])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return LaunchdJob::Absent;
    };
    match out.status.code() {
        Some(0) => LaunchdJob::Loaded(String::from_utf8_lossy(&out.stdout).lines().find_map(|l| {
            l.trim()
                .strip_prefix("pid = ")
                .and_then(|p| p.trim().parse().ok())
        })),
        Some(LAUNCHCTL_NO_DOMAIN) => LaunchdJob::NoGuiLogin,
        _ => LaunchdJob::Absent,
    }
}

pub(crate) fn launchd_loaded(label: &str) -> bool {
    matches!(launchd_job(label), LaunchdJob::Loaded(_))
}

/// The refusal for a uid launchd has no GUI domain for — before the plist
/// is written, so a refused run leaves no unused broker job. Always `false`.
fn refuse_no_gui_login(out: &mut Out) -> bool {
    let uid = uid();
    out.fail(
        "broker",
        &format!(
            "uid {uid} has no GUI login: launchd's gui/{uid} domain, where the broker's job \
             runs, exists only while this user is logged in at the console (an SSH session \
             alone opens none) — log in there once, or keep the broker yourself with `on \
             --service none` (it prints the argv to run)"
        ),
    );
    false
}

/// Whether a `systemd --user` manager answers this login; the named refusal
/// when none does (`su`, `sudo -u`, an SSH session without `pam_systemd`),
/// which would otherwise surface as systemctl's own bus error.
fn systemd_user_manager() -> Result<(), String> {
    systemctl(&["show-environment"]).map_err(|e| {
        format!(
            "no systemd user manager answers this login ({e}): a login without one (`su`, \
             `sudo -u`, an SSH session without pam_systemd) cannot supervise the broker — log \
             in normally, or keep the broker yourself with `on --service none`"
        )
    })
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

/// The `MainPID` systemd runs `label` under, `None` when the unit is not
/// running (systemd answers `0`) or systemctl cannot say.
fn systemd_main_pid(label: &str) -> Option<u32> {
    let out = aterm_uds::spawnfd::command("systemctl")
        .args(["--user", "show", "-p", "MainPID", "--value", label])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|&pid| pid != 0)
}

/// Why a broker that ANSWERS is still not the one `on` installs — `None` when
/// it is — so the supervisor restarts it. One that runs a REPLACED BUILD
/// predates the file its job runs (an update swapped it), so the job's argv
/// alone cannot tell; one that acknowledged a capability minted under another
/// secret checks none (a broker started before its job was rewritten
/// guarded), so the cap-forced `<src>`, `trust=human` and the per-human halts
/// rest on nothing. One whose process runs another `--durability` than the
/// job's acks under a promise the operator did not choose (a job rewritten
/// while its broker kept running, or a hand-run one under `--service none`).
fn not_current(p: &Paths, view: &fabric::BrokerView) -> Option<&'static str> {
    use crate::durability::Durability;
    if view.replaced.is_some() {
        Some("it runs a replaced build")
    } else if view.guarded == Some(false) {
        Some("it checks no capability")
    } else {
        match view.durability {
            Some(running) if running != p.tier() => Some(match running {
                Durability::Strict => "it runs at durability strict",
                Durability::Ordered => "it runs at durability ordered",
                Durability::Relaxed => "it runs at durability relaxed",
            }),
            _ => None,
        }
    }
}

/// The refusal for a broker the supervisor does not manage (a hand-run `aterm
/// link broker`, or a job under another label): starting the job over it
/// would put a second broker on the same socket AND bus log. Always `false`.
fn refuse_unmanaged(view: &fabric::BrokerView, sock: &str, out: &mut Out) -> bool {
    out.fail(
        "broker",
        &format!(
            "an unmanaged broker (pid {}) answers on {sock}; stop it and run `on` again, or keep \
             it with `on --service none`",
            view.pid
                .map_or_else(|| "-".to_string(), |pid| pid.to_string())
        ),
    );
    false
}

/// Step 6: the broker, supervised. `Ok(true)` when it answers. `named`:
/// `on --durability` chose the tier, rather than keeping one.
fn broker_step(p: &Paths, node: &str, service: Service, named: bool, out: &mut Out) -> bool {
    trace("Supervise");
    let sock = p.endpoint();
    let label = p.label();
    match service {
        Service::None => {
            let view = probe(p, node, fabric::process_table);
            // THE BROKER IS THE OPERATOR'S under `--service none`, run by hand
            // (the manual's step 1, `--durability relaxed` and all): no job
            // says its tier, so a bare `on` keeps the one it runs, read off
            // its process. Only a tier `--durability` names is one to restart
            // it for — and every restart line below carries the tier kept.
            let kept;
            let p = match view.durability {
                Some(running) if !named && running != p.tier() => {
                    kept = Paths {
                        durability: Some(running),
                        ..p.clone()
                    };
                    out.note(
                        "durability",
                        &format!("{}, kept from the unmanaged broker", running.name()),
                    );
                    &kept
                }
                _ => p,
            };
            if view.read_ok() {
                out.already(
                    "broker",
                    &format!(
                        "answers on {sock} (unmanaged: --service none){}",
                        view.pid
                            .map_or_else(String::new, |pid| format!(", pid {pid}"))
                    ),
                );
                // Nothing here restarts it, so the line says how.
                if let Some(why) = not_current(p, &view) {
                    out.warn(
                        "broker",
                        &format!(
                            "{why}: restart it as `{}` (or drop --service none)",
                            p.broker_argv().join(" ")
                        ),
                    );
                }
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
            let running = match launchd_job(&label) {
                // No domain to bootstrap into: refused before anything is
                // written, dry or not (the probe is read-only).
                LaunchdJob::NoGuiLogin => return refuse_no_gui_login(out),
                LaunchdJob::Loaded(pid) => pid,
                LaunchdJob::Absent => None,
            };
            let view = probe(p, node, fabric::process_table);
            let stale = not_current(p, &view);
            if same && running.is_some() && view.read_ok() && stale.is_none() {
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
            // A BROKER LAUNCHD DOES NOT MANAGE is never bootstrapped over.
            if running.is_none() && view.read_ok() {
                return refuse_unmanaged(&view, &sock, out);
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
            } else if let (true, Some(stale)) = (view.read_ok(), stale) {
                stale.to_string()
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
            // No legacy `load` fallback: it acts on the CALLER's domain, and
            // exits 0 on anything but bad usage, so it would hide the
            // bootstrap's own error behind a 5 s wait for a socket.
            if let Err(e) = launchctl(&["bootstrap", &format!("gui/{}", uid()), &plist_s]) {
                out.fail("broker", &e);
                return false;
            }
            match wait_for_broker(p, node) {
                Ok(view) => {
                    out.ok(
                        "broker",
                        &format!(
                            "launchd {label} pid {} answers on {sock} ({} ms)",
                            match launchd_job(&label) {
                                LaunchdJob::Loaded(pid) => pid,
                                _ => None,
                            }
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
            let running = systemd_main_pid(&label);
            let view = probe(p, node, fabric::process_table);
            if same && running.is_some() && view.read_ok() && not_current(p, &view).is_none() {
                out.already(
                    "broker",
                    &format!(
                        "systemd --user {label} pid {} answers on {sock}",
                        running.unwrap_or(0)
                    ),
                );
                return true;
            }
            // No user manager to ask: refused before the unit is written, dry
            // or not (the probe is read-only) — and BEFORE the unmanaged
            // check, as the launchd arm asks for its domain first: with no
            // manager `MainPID` cannot be read, so the unit's own broker
            // answering would otherwise be called unmanaged.
            if let Err(e) = systemd_user_manager() {
                out.fail("broker", &e);
                return false;
            }
            // The launchd arm's refusal: a broker the unit does not run is
            // never restarted over (a second broker on its socket and log).
            if running.is_none() && view.read_ok() {
                return refuse_unmanaged(&view, &sock, out);
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
            let why = if running.is_none() {
                "not running"
            } else if !same {
                "the unit changed"
            } else {
                not_current(p, &view).unwrap_or("running but not answering")
            };
            out.done(
                "broker",
                &format!(
                    "systemctl --user daemon-reload; enable --now {label} ({why}); wait for {sock}"
                ),
                &format!("systemd --user {label} (re)started ({why})"),
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
                            "systemd --user {label} did not bring {sock} up: {e} — {}",
                            systemd_failure_hint(&p.root, &label)
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
    /// `fabric status`'s `reason=`: `state-locked` is a bridge standing by.
    reason: String,
    command: Option<String>,
}

/// What `on` says of an instance whose bridge STANDS BY
/// (`bridge::hold_state_dir`): armed, but not one the proof can post through.
const STANDBY_NOTE: &str = "its bridge stands by: another instance's bridge holds this node's \
                            state dir, and this one carries no mail until that one exits";

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
        reason: kv(h, "reason").unwrap_or("-").to_string(),
        command: kv(h, "command")
            .filter(|c| *c != "-")
            .map(crate::pct::decode),
    })
}

/// Step 9: arm every running instance. Answers how many are running (`None`
/// when they could not be listed), and the ones that carry this command
/// (armed now or before), in the order they were listed.
fn arm_instances(argv: &[String], out: &mut Out) -> (Option<usize>, Vec<Instance>) {
    let command = argv.join(" ");
    let ours = fabric::serve_flags(&command);
    // THE PROGRAM IS COMPARED WITH THE FLAGS ([`fabric::bridge_program`]): an
    // instance armed with these flags and another program holds its bridge
    // lane with a process `on` did not choose, and is named, not counted
    // (r4-17).
    let our_program = fabric::bridge_program(&command);
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
            let flags_match = inst.command.as_deref().and_then(fabric::serve_flags) == ours;
            let program = inst.command.as_deref().and_then(fabric::bridge_program);
            let shown = safe(program.as_deref().unwrap_or("-"), 256);
            if flags_match && program != our_program {
                out.note(
                    "instance",
                    &format!(
                        "{pid} is armed with these flags but the program {shown} (not {}); \
                         relaunch that aterm to change it",
                        safe(our_program.as_deref().unwrap_or("-"), 256)
                    ),
                );
            } else if flags_match && inst.reason == "state-locked" {
                // A RE-RUN finds the standby the first run armed: still not an
                // instance the proof can post through (its `post --wait`
                // answers `ERR fabric stalled`), whichever one the caller sits in.
                out.note(
                    "instance",
                    &format!("{pid} is armed with this command ({shown}); {STANDBY_NOTE}"),
                );
            } else if flags_match {
                out.already(
                    "instance",
                    &format!(
                        "{pid} is armed with this command ({shown}, fabric={})",
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
            // `state-locked` is final for as long as the holder runs.
            if state == "connected" || reason == "state-locked" || started.elapsed() > ARM_DEADLINE
            {
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
        } else if state == "stalled" && reason == "state-locked" {
            // ONE STATE DIR IS ONE BRIDGE (`bridge::hold_state_dir`): this
            // command names one `--state`, and another instance's bridge
            // holds it. Not a failure of the arming — this one carries the
            // node the moment that one exits — and not an instance the proof
            // can post through either.
            out.note("instance", &format!("{pid} armed; {STANDBY_NOTE}"));
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

/// The wire `on` runs, given what the root `p` already is ([`Role`]) and
/// `wire`, the one its flags say — with, when it was KEPT, the job it was
/// read from.
///
/// * A root that JOINED another host's fleet is refused, whatever the flags:
///   `on` would move this host onto a broker of its own (and re-mint the cap
///   the serving host issued under a secret of its own). A broker that does
///   not answer is fixed on the host that serves it; `off` leaves the fleet
///   on purpose.
/// * On a root whose job serves the sealed wire, a bare `on` (no `--tcp`,
///   no `--key-file`) keeps it — the bind, the key and `--allow-remote` the
///   job runs — so re-running `on` there restarts the broker every joined
///   host dials instead of rewriting it without `--tcp`. A build without
///   `sealed` refuses it by name rather than serve the socket alone.
/// * A root only the rendezvous file says serves it (no job of `on`'s: a
///   broker something else runs) has no bind to keep: refused, naming the
///   flags.
///
/// # Errors
///
/// The refusals above, each a usage-level exit 2 before anything is written.
fn kept_wire(
    p: &Paths,
    opts: &OnOpts,
    wire: Wire,
    role: Role,
) -> Result<(Wire, Option<String>), String> {
    match role {
        Role::Joined(j) => Err(format!(
            "this host JOINED the fleet whose broker {} serves, and `on` here would move it \
             onto a broker of its own, out of that fleet. A broker that does not answer is \
             fixed on the host that serves it (`aterm fabric doctor` there), and this host's \
             bridges redial on their own; `{}` re-applies the join; `aterm fabric off` first \
             leaves the fleet on purpose",
            safe(&j.broker, 128),
            j.join_command(None)
        )),
        Role::Serves(s) if wire == Wire::Unix => {
            let Some(bind) = s.bind.clone() else {
                return Err(format!(
                    "this root serves the sealed wire (the rendezvous file's serves_tcp = {}), \
                     and no job of `on`'s says at which bind: run `{}` with the bind its broker \
                     serves, or `aterm fabric off` first to serve the socket alone",
                    safe(s.dial.as_deref().unwrap_or("-"), 128),
                    s.on_command()
                ));
            };
            let job = match s.job {
                Some(Service::Systemd) => format!("the systemd --user unit {}", p.unit().display()),
                _ => format!("the launchd job {}", p.plist().display()),
            };
            let kept = OnOpts {
                tcp: Some(bind),
                key_file: Some(s.key_file.clone()),
                allow_remote: opts.allow_remote || s.allow_remote,
                ..opts.clone()
            };
            wire_for_on(&kept)
                .map(|w| (w, Some(job.clone())))
                .map_err(|e| {
                    format!(
                        "{job} serves the sealed wire, and a bare `on` keeps it: {e}; `aterm \
                         fabric off` first, to serve the socket alone"
                    )
                })
        }
        _ => Ok((wire, None)),
    }
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
    let role = match Role::of(&p) {
        Ok(role) => role,
        Err(e) => {
            eprintln!("aterm fabric on: cannot determine the existing role: {e}");
            return ExitCode::from(2);
        }
    };
    if let Some(fleet) = role.fleet() {
        if opts.fleet.as_deref().is_some_and(|asked| asked != fleet) {
            eprintln!(
                "aterm fabric on: this root belongs to fleet {fleet}; `aterm fabric off` first before choosing another fleet"
            );
            return ExitCode::from(2);
        }
        p.fleet = fleet.to_string();
    }
    // WHAT THIS ROOT ALREADY IS, before anything is written ([`Role`]): a
    // joined root is refused, and a bare `on` keeps the sealed wire the
    // root's job serves.
    let (wire, kept) = match kept_wire(&p, opts, wire, role) {
        Ok(kept) => kept,
        Err(e) => {
            eprintln!("aterm fabric on: {e}");
            return ExitCode::from(2);
        }
    };
    p.wire = wire;
    // THE TIER: `--durability`, else the one the installed job NAMES, so a
    // bare `on` that repairs a relaxed root restarts the broker it had, and a
    // job that named strict keeps naming it. A job that names none runs the
    // build's default. A host whose kernel names no boot runs relaxed too:
    // astream moves the log's generation after every unclean stop there, the
    // safe answer to not knowing ([`crate::durability`]).
    let (job_durability, durability_job) = match kept_durability(&p) {
        Ok(kept) => kept,
        Err(e) => {
            eprintln!("aterm fabric on: cannot read the installed broker job: {e}");
            return ExitCode::from(2);
        }
    };
    p.durability = opts.durability.or(job_durability);
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
    if let Some(job) = kept {
        out.note(
            "wire",
            &format!(
                "kept from {job}: this root serves the sealed wire to the hosts that joined it \
                 (`aterm fabric off` first, to serve the socket alone)"
            ),
        );
    }
    // A relaxed broker is said every run, with what its ack does not promise,
    // and so is an ordered one, with the tier this build serves it at; a
    // switch back to strict is said once, as the job it rewrites.
    let kept_from = match (opts.durability, &durability_job) {
        (None, Some(job)) => format!(", kept from {job}"),
        _ => String::new(),
    };
    match (p.tier(), crate::durability::resolve(job_durability)) {
        (crate::durability::Durability::Relaxed, _) => out.note(
            "durability",
            &format!(
                "relaxed{kept_from} — {}; `--durability strict` returns to acks on the disk",
                crate::durability::Durability::Relaxed.promise()
            ),
        ),
        (crate::durability::Durability::Ordered, _) => out.note(
            "durability",
            &format!(
                "ordered{kept_from}, {}",
                crate::durability::describe_own(crate::durability::Durability::Ordered)
            ),
        ),
        (crate::durability::Durability::Strict, crate::durability::Durability::Relaxed) => {
            out.note(
                "durability",
                &format!(
                    "strict, was relaxed — {}",
                    crate::durability::Durability::Strict.promise()
                ),
            );
        }
        _ => {}
    }

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

    // 4. the mint secret — read through the one reader the broker and every
    // mint use, so `already … (0600)` is a mode that was read, never assumed.
    let secret = p.secret();
    let secret_shown = secret.to_string_lossy().into_owned();
    match crate::transport::read_mint_secret(&secret_shown) {
        Ok(_) => {
            out.already("secret", &format!("{} (0600)", secret.display()));
        }
        Err(e) if e.kind() != io::ErrorKind::NotFound => {
            out.fail(
                "secret",
                &format!(
                    "{}; a mint secret is never replaced (every cap minted under it would stop \
                     verifying): fix it, or move it aside and run again",
                    crate::transport::describe(&secret_shown, &e)
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

    // 5. the cap file — this node's, under step 4's secret (a dry run that
    // has yet to mint the secret re-mints any cap: it cannot be under it)
    let cap = p.cap();
    let cap_ok = if cap.exists() {
        crate::transport::read_mint_secret(&secret_shown)
            .map_err(|e| crate::transport::describe(&secret_shown, &e))
            .and_then(|key| cap_is_for(&cap, &node, &key))
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
    let broker_ok = broker_step(&p, &node, service, opts.durability.is_some(), &mut out);
    if !broker_ok && !opts.dry_run {
        return ExitCode::FAILURE;
    }
    if broker_ok && !wire_step(&p, &mut out) && !opts.dry_run {
        return ExitCode::FAILURE;
    }
    // A host that serves others: their broker lives as long as this host's
    // supervisor keeps it, and that is said once, here.
    if let (true, Wire::Sealed { bind: Some(_), .. }, Some(lives)) = (
        broker_ok,
        &p.wire,
        broker_scope(service, p.starts_at_login(service)),
    ) {
        out.note(
            "scope",
            &format!(
                "the broker every joined host dials is {lives}; while it is down they read \
                 fabric=stalled and queue their posts — to serve while logged out, keep it \
                 under a supervisor of your own (`on --tcp … --service none` prints its argv)"
            ),
        );
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
            // `already … (0600)` only for a mode that was read: the same bytes
            // at a wider mode are written again, 0600.
            let private = {
                use std::os::unix::fs::PermissionsExt;
                std::fs::metadata(&path).is_ok_and(|m| m.permissions().mode() & 0o077 == 0)
            };
            if private && std::fs::read(&path).is_ok_and(|old| old == want.as_bytes()) {
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
        // Both slots: the owner's own hold stands beside the fleet's, and
        // outlives its withdrawal.
        let beside = if origin.as_deref() == Some("fleet,local") {
            format!(
                "; the owner's local hold beside it stays until {}",
                lift_hold(&sid, pid, sock.as_deref())
            )
        } else {
            String::new()
        };
        let detail = if origin.as_deref() == Some("local") {
            format!(
                "@{sid} (instance {pid}) is HELD (hold=1 origin=local); off does not lift it \
                 — {} does",
                lift_hold(&sid, pid, sock.as_deref())
            )
        } else {
            format!(
                "@{sid} (instance {pid}) is HELD (hold=1 origin={}); off does not lift it — \
                 relaunch instance {pid} after this, which frees it while the fabric stays off \
                 (the bus keeps the halt row, and the next `fabric on` holds it again); `aterm \
                 fabric halt --cap-file <the halter's cap> off` withdraws the halt itself{beside}",
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
                    // By domain, with no legacy `unload` fallback: that acts
                    // on the caller's domain and exits 0 whatever it did.
                    if loaded {
                        let domain = format!("gui/{}/{label}", uid());
                        if let Err(e) = launchctl(&["bootout", &domain]) {
                            out.fail("broker", &e);
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

/// The one remedy for a broker and a client in DIFFERENT astream protocols —
/// the probe's mismatch and the bridge's `reason=protocol` alike. Two aterm
/// releases are talking. On a host that runs the broker it is the older
/// process (an update replaced the file it runs), and re-running `on`
/// restarts it from the aterm on PATH; a JOINED host's broker is another
/// host's, which needs the same release as this one.
fn protocol_remedy(role: &Role, again: &str, also: &str) -> String {
    match role {
        Role::Joined(_) => "the broker this host joined speaks another astream protocol than \
                            this aterm: the two hosts run different aterm releases — build both \
                            from the same commit (`aterm help fabric`, A SECOND HOST)"
            .to_string(),
        _ => format!(
            "the broker and this aterm are different releases speaking different astream \
             protocols: {again} restarts the broker from the aterm on PATH{also}; a broker on \
             another host needs the same aterm release there"
        ),
    }
}

/// The one remedy for a LAPSED cap — the bridge's `reason=expired`, and the
/// report's own probe refused the same way ([`fabric::CAP_LAPSED`]). The grants
/// are genuine and only their `exp=` passed, so the cure is the SAME ring
/// minted again with a later one, written over the cap file the bridge names:
/// not a wider ring, a new secret or a restart. On a joined host that is
/// `mint-for --expires` on the host that serves the fleet and a `join` here
/// ([`Joined`]). A ring `on` mints carries no `exp=`, so on the host that
/// holds the secret a lapsed one was minted by hand, and `on` mints the node's
/// ring again once it is moved aside.
fn renew_remedy(role: &Role, again: &str) -> String {
    let lapsed = "this node's cap has lapsed (its grants' `exp=` passed) and the broker refuses it";
    match role {
        Role::Joined(j) => format!("{lapsed}: {}", j.renew_fix()),
        _ => format!(
            "{lapsed}: the ring `on` mints never lapses, so this one was minted by hand — move \
             <root>/node.cap aside and run {again}, which mints this node's ring again under \
             the root's secret; the running bridge attaches it at its next redial (it reads its \
             cap file again before every dial), with no restart"
        ),
    }
}

/// The fix the broker's OWN LAST WORD names, when warning `w` quotes it from
/// its supervisor's `broker.err` (`fabric::warnings`: "its supervisor's
/// broker.err, …") — read the one way for every warning that carries it, the
/// broker's own and a bridge's dial nothing answered alike. A line aterm's
/// broker writes for a log it refuses ([`crate::cli::log_refusal_remedy`])
/// picks its remedy by aterm's own words: never a repair for a log a NEWER
/// aterm wrote, which a truncation would destroy; the shipped repair for a
/// corrupt record. Any other quoted line is the cause to fix before a restart.
/// `None` when nothing is quoted.
fn quoted_broker_fix(
    w: &str,
    again: &str,
    also: &str,
    at_broker: &dyn Fn(String) -> String,
) -> Option<String> {
    let s = |t: &str| w.contains(t);
    if !s("supervisor's broker.err") {
        return None;
    }
    Some(if s(crate::cli::NEWER_LOG_REMEDY) {
        // THE BROKER'S OWN LINE said a NEWER aterm wrote its log: never a
        // repair — it would destroy records that release reads — and never a
        // bare restart into the same refusal.
        at_broker(format!(
            "the broker's log holds records in a format newer than this aterm reads: install \
             the aterm release that wrote them (or a later one), then {again} restarts the \
             broker{also}. No repair applies, and the log is untouched"
        ))
    } else if s(crate::cli::CORRUPT_LOG_REMEDY) {
        // THE BROKER'S OWN LINE said its log has a corrupt record, and named
        // the shipped repair with the log's path.
        at_broker(format!(
            "the broker refuses its log for a corrupt record, and restarting it meets the same \
             refusal: run the `aterm link broker --repair <log>` the quoted line names — it \
             truncates the log at that record, dropping the records from there on (possibly \
             acked mail) and printing how many bytes went — then {again} restarts the \
             broker{also}"
        ))
    } else {
        at_broker(format!(
            "the broker's own last word is quoted: when it says why the broker cannot run (a log \
             it refuses to open, an address in use, a file it cannot read), fix that first, then \
             {again} restarts it{also} — a restart alone meets the same refusal"
        ))
    })
}

/// The fix for one of `aterm fabric`'s WARNINGS, matched on the words the
/// warning is built from (`fabric::warnings`), and spelled for what this
/// host is to its fleet ([`Role`]): every remedy that re-runs `on` is
/// [`Role::again`] — `on --tcp …` on the host that serves the sealed wire,
/// the `join` on a joined one — and on a joined host a broker that does not
/// answer is fixed on the host that serves it, never here.
#[must_use]
pub fn fix_for(warning: &str, role: &Role) -> String {
    let w = warning;
    let s = |t: &str| w.contains(t);
    let again = role.again();
    // What re-running `on` does on the host that serves the joined ones.
    let also = if matches!(role, Role::Serves(_)) {
        " (it serves this host's socket and the sealed wire; every joined host redials on its \
         own)"
    } else {
        ""
    };
    // A remedy AT THE BROKER: this host's, unless it joined another's fleet.
    let at_broker = |here: String| match role {
        Role::Joined(j) => j.broker_fix(),
        _ => here,
    };
    // THE BROKER'S OWN LAST WORD, wherever a warning quotes it: the broker's
    // own warning, and a bridge's dial nothing answered on that broker.
    let quoted = quoted_broker_fix(w, &again, also, &at_broker);
    if s("speaks astream protocol") {
        protocol_remedy(role, &again, also)
    } else if s(fabric::CAP_LAPSED) {
        renew_remedy(role, &again)
    } else if s("does not answer") && quoted.is_some() {
        quoted.unwrap_or_default()
    } else if s("does not answer") && s("no record lands")
        || s("does not answer this report's own probe")
    {
        at_broker(format!("start the broker: {again}{also}"))
    } else if s("runs a replaced build") {
        at_broker(format!(
            "{again} restarts it on the build on disk{also} (a broker of this build or later \
             restarts itself within a minute of its executable being replaced)"
        ))
    } else if s("checks no capability") {
        at_broker(format!(
            "{again} rewrites the broker's job and restarts it guarded with the root's mint \
             secret (a broker run by hand: restart it with `--secret-file <root>/mint.secret`)"
        ))
    } else if s("has not reported its broker link") {
        format!(
            "nothing, if its mail moves (`aterm ctl @<sid> post to=@<sid> kind=note --wait=5000 \
             ping` lands); if it does not, {}",
            at_broker(format!("check the broker: {again}"))
        )
    } else if s("broker link is down") {
        // The warning carries the bridge's own `reason=`, so the one cause it
        // names picks the one remedy.
        let reason = w
            .split("reason=")
            .nth(1)
            .and_then(|r| r.split([',', ' ', ')']).next())
            .unwrap_or("");
        let cause = match reason {
            // Nothing listens — and when the broker's supervisor said why
            // (`fabric::warnings` quotes its last word here, as on the
            // broker's own warning), that line's remedy: a broker refusing its
            // log meets the same refusal at every start.
            "no-socket" | "refused" => quoted.unwrap_or_else(|| {
                at_broker(format!(
                    "nothing listens at the broker's socket: {again} starts it{also}"
                ))
            }),
            "no-ack" => at_broker(format!(
                "the broker accepts and never answers: {again} restarts it{also}"
            )),
            "expired" => renew_remedy(role, &again),
            "attach" => match role {
                Role::Joined(j) => format!(
                    "the broker refuses this node's cap (minted under another secret, or for \
                     another fleet): {}",
                    j.cap_fix()
                ),
                _ => format!(
                    "the broker refuses this node's cap: move <root>/node.cap aside and run \
                     {again}"
                ),
            },
            "busy" => "the broker is at its connection limit (`too many connections`) and \
                       nothing on this node is wrong, so a re-minted cap cannot help: the bridge \
                       redials under its back-off; if it stays, find what holds the broker's \
                       connections"
                .to_string(),
            "record" => "the broker refuses this node's own will or presence row for good (a \
                         sequence in its reserved half): the cap is fine and a re-mint cannot \
                         help — the bridge's stderr names the refusal"
                .to_string(),
            "denied" => {
                "the broker's socket refuses this user: check its owner and mode".to_string()
            }
            // The bridge asked the broker before it reported: it answered.
            cut if cut.starts_with("cut-") => {
                "the broker is up and ended one subscription (the face after `cut-`), as astream \
                 does to a reader that leaves its socket unread for the 30 s stream write bound: \
                 a bridge stopped or starved that long, or a link that lost one connection — \
                 nothing on the broker needs a restart"
                    .to_string()
            }
            "protocol" => protocol_remedy(role, &again, also),
            "endpoint-grammar" => "the bridge is a build this aterm cannot serve (it lacks a \
                                   verb the bridge needs): `[fabric] command` names another \
                                   aterm than this instance's; quit and relaunch this aterm, or \
                                   point [fabric] command at its build"
                .to_string(),
            _ => at_broker(format!("check the broker: {again}{also}")),
        };
        format!("{cause}; the bridge then reconnects on its own")
    } else if s("bus could not be read") || s("could not be read:") {
        match role {
            Role::Joined(j) => format!(
                "the cap file's grants do not cover this fleet, or the broker died mid-read: \
                 {}; check `--fleet`",
                j.cap_fix()
            ),
            _ => format!(
                "the cap file's grants do not cover this fleet, or the broker died mid-read: \
                 {again} re-mints the cap for the node id in the state dir and probes again; \
                 check `--fleet`"
            ),
        }
    } else if s("no [fabric] command") {
        format!(
            "{again} writes [fabric] command (and the rendezvous file) so the next launch starts \
             a bridge on its own"
        )
    } else if s("no node id") {
        match role {
            Role::Joined(_) => format!("{again} records the node id its cap was minted for"),
            _ => format!("{again} provisions one and mints the cap for it"),
        }
    } else if s("cap file") {
        match role {
            Role::Joined(j) => j.cap_fix(),
            _ => format!("{again} re-mints the cap file for the node id in the state dir"),
        }
    } else if s("has no bridge") {
        format!("{again} arms every running instance")
    } else if s("bridge is down") {
        format!(
            "aterm relaunches its bridge with back-off; if it stays down, the bridge cannot \
             start — run the [fabric] command by hand to see its error, then {again}"
        )
    } else if s("has not answered for the work") {
        "kill that instance's bridge (`kill <bridge pid>` from the BRIDGES row); aterm relaunches \
         it"
        .to_string()
    } else if s("armed with a different command") {
        "relaunch that aterm instance: its supervisor reads [fabric] command once, at launch"
            .to_string()
    } else if s("stands by") {
        format!(
            "{}; it takes the node over on its own once the holder exits",
            crate::bridge::STATE_LOCKED_REMEDY
        )
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
        if s("origin=fleet,local") {
            // Both slots: the fleet's remedy, and then the owner's own hold,
            // which a fleet lift leaves standing.
            let fleet = if s("reason=fabric-lost origin=fleet") {
                "the fleet's lifts when the bridge reconnects"
            } else {
                FLEET_HALT_LIFT
            };
            format!("{fleet}; the owner's local hold beside it lifts with {lift}")
        } else if s("origin=local") {
            lift
        } else if s("reason=fabric-lost origin=fleet") {
            "it lifts when the bridge reconnects".to_string()
        } else if s("origin=fleet") {
            FLEET_HALT_LIFT.to_string()
        } else {
            format!(
                "a local hold lifts with {lift}; a fleet hold lifts when a bridge reconnects \
                 (reason=fabric-lost) or when the person who set it runs `aterm fabric halt \
                 --cap-file <their cap> off`"
            )
        }
    } else if s("lost") && s("evicted") {
        "read mail sooner (`aterm ctl @<sid> await inbox`) and `inbox seen` what is handled; the \
         evicted records are still on the bus"
            .to_string()
    } else if s("passed its deadline") && s("@p passed") {
        // A PERSON was asked (`post to=h-<name>`): no session holds it, so no
        // `aterm ctl` reaches it.
        "a person was asked: `aterm fabric inbox --cap-file <their cap>` shows it, and `aterm \
         fabric post --cap-file <their cap> to=@<sid>@<node> kind=answer re=<off> <text>` \
         answers it (`aterm link notify --on ask:<person>` pages them next time)"
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
        format!("arm a bridge ({again}); the outbox drains the moment one attaches")
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
    } else if s("fleet halt standing") || s("stands on this node with no row on the bus") {
        FLEET_HALT_LIFT.to_string()
    } else if s("no rendezvous file") {
        format!("{again} writes it")
    } else if s("is another build than the instance") || s("could not parse") {
        format!(
            "point [fabric] command at this instance's own aterm ({again} from a shell inside \
             it) and relaunch, or update the other build; until then mail a line the instance \
             cannot parse carries waits on the bus"
        )
    } else {
        "see `aterm help fabric`".to_string()
    }
}

/// What lifts a fleet halt, in the one sentence every surface that reports one
/// uses: the person whose halt it is withdraws it, with the cap their halt was
/// published under. The Owner's `hold <sid> off` is `ERR denied` for it, and
/// `aterm fabric off` and a relaunch free a session only until the next `on`
/// re-reads the row.
pub const FLEET_HALT_LIFT: &str = "the person who set it withdraws it: `aterm fabric halt \
     --cap-file <their cap> off` (a cap from `aterm fabric mint-for h-<name>`). `aterm fabric off` \
     and a relaunch free a session only until the next `fabric on` re-reads the halt row";

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
/// producer id at a sequence reserved through the state dir, which has one
/// writer — the holder of `StateDir::lock_bridge`. With no bridge up the CLI
/// takes that lock itself (`fabric::publish_exited`) and refuses when one
/// holds it: a CLI racing a live bridge for the next sequence would hand the
/// broker a `(producer_id, seq)` pair it has already seen, which it answers
/// with the original offset and appends NOTHING — a retire that reports
/// success and did not happen.
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
            //
            // EXCEPT ONE THAT STANDS BY (`bridge::hold_state_dir`): it owns
            // nothing until the holder exits, and a `fabric retire` asked of it
            // is taken, never published, and waited out to "unretired".
            i.reason.as_deref() != Some("state-locked")
                && (!i.bridge_pids.is_empty()
                    || i.supervised == Some(true)
                    || matches!(i.fabric.as_deref(), Some("connected" | "stalled" | "stale")))
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
    // What this host is to its fleet, read from the root the fabric is on —
    // every remedy below is spelled for it.
    let role = match Paths::resolve_for_off()
        .and_then(|(p, _)| Role::of(&p).map_err(|e| e.to_string()))
    {
        Ok(role) => role,
        Err(e) => {
            eprintln!(
                "aterm fabric doctor: cannot determine the existing role: {e}; fix the metadata before reapplying the fabric"
            );
            return ExitCode::from(2);
        }
    };
    match fabric::gather() {
        Err(off) => {
            // The state, then the remedy once, in the `fix:` shape every finding uses.
            println!("{}", off.state());
            println!(
                "  fix: {} (`--dry-run` first shows every step)",
                role.again()
            );
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
            // A fact, not a warning: the joined hosts' broker lives as long as
            // this host's supervisor keeps it.
            if let Role::Serves(s) = &role {
                println!("  scope: {}", s.scope());
            }
            // A fact too: what an ack from this host's broker promises — the
            // tier its process names ([`fabric::BrokerView::durability`]) as
            // the broker itself says it serves it ([`fabric::BrokerView::served`]).
            if let Some(d) = report.broker.tier() {
                println!(
                    "  durability: {} — {}",
                    d.name(),
                    crate::durability::describe_served(d, report.broker.served)
                );
            }
            if warnings.is_empty() {
                println!("  no warnings");
                return ExitCode::SUCCESS;
            }
            println!("  {} warning(s)", warnings.len());
            for w in &warnings {
                println!("  ! {w}");
                println!("    fix: {}", fix_for(w, &role));
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
        // A bridge standing by for the state dir owns no sequence: the
        // holder is asked, wherever the two sit in the listing.
        let standby = fabric::InstanceView {
            pid: 7,
            reason: Some("state-locked".to_string()),
            ..inst(Some("stalled"), Some(true), vec![98])
        };
        assert_eq!(
            running_bridge(&[
                standby.clone(),
                inst(Some("connected"), Some(true), vec![99])
            ]),
            Some(42)
        );
        assert_eq!(running_bridge(&[standby]), None);
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
    /// derives one from POSIX `cksum` of its path — the number `cksum`
    /// prints, measured on this machine:
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
            durability: None,
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
            durability: None,
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

    /// THE TIER IS IN THE JOB. A NAMED tier rides after the secret, on both
    /// wires — `--durability relaxed`, and `--durability strict` too, so a job
    /// that chose strict keeps it under a build whose default differs — and a
    /// job that names none carries nothing, so every strict job an earlier
    /// `on` wrote is still current and runs the build's default. A switch is
    /// "the job changed" and restarts the broker. The socket still follows
    /// `broker`, which is how the process table finds it. A bare `on` keeps the
    /// tier the installed job NAMES (none, strict, ordered or relaxed), and
    /// with no job it names none.
    #[test]
    fn the_tier_is_in_the_job_and_a_bare_on_keeps_it() {
        use crate::durability::Durability;
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let bare = Paths {
            home: dir.path().to_path_buf(),
            login_home: None,
            root: dir.path().join("root"),
            aterm: "/usr/local/bin/aterm".to_string(),
            fleet: "local".to_string(),
            config: None,
            rendezvous: None,
            wire: Wire::Unix,
            durability: None,
        };
        let named = |tier| Paths {
            durability: Some(tier),
            ..bare.clone()
        };
        let relaxed = named(Durability::Relaxed);
        let argv = relaxed.broker_argv();
        assert_eq!(argv[argv.len() - 2..], ["--durability", "relaxed"]);
        assert!(!bare.broker_argv().iter().any(|w| w == "--durability"));
        assert_eq!(bare.broker_argv()[..], argv[..argv.len() - 2]);
        assert_eq!(bare.tier(), Durability::Strict, "this build's default");
        let strict = named(Durability::Strict);
        let words = strict.broker_argv();
        assert_eq!(
            words[words.len() - 2..],
            ["--durability", "strict"],
            "a named strict is written"
        );
        assert!(!bare.plist_is_current(&relaxed.plist_text()));
        assert!(!relaxed.plist_is_current(&bare.plist_text()));
        assert!(!bare.plist_is_current(&strict.plist_text()));
        assert!(relaxed.plist_is_current(&relaxed.plist_text()));
        let sock = bare.sock();
        let ps = fabric::Proc {
            pid: 7,
            ppid: 1,
            cmd: argv.join(" "),
        };
        assert_eq!(fabric::broker_pid(&[ps], &sock), Some(7));
        assert_eq!(Durability::of_argv(&argv), Durability::Relaxed);
        let sealed = Paths {
            wire: Wire::Sealed {
                bind: Some("127.0.0.1:7000".to_string()),
                dial: "127.0.0.1:7000".to_string(),
                key_file: PathBuf::from("/k"),
                allow_remote: false,
            },
            ..relaxed.clone()
        };
        let words = sealed.broker_argv();
        let at = words
            .iter()
            .position(|w| w == "--durability")
            .expect("the flag");
        assert_eq!(words[at + 1], "relaxed");
        assert!(at < words.iter().position(|w| w == "--unix").expect("--unix"));
        assert!(unit_argv(&sealed.unit_text()).is_some_and(|w| w == words));

        // Kept: no job names nothing, kept from nothing; each job names what
        // it was written with, a bare one nothing.
        assert_eq!(kept_durability(&bare), Ok((None, None)));
        std::fs::create_dir_all(&bare.root).unwrap();
        for tier in [Durability::Relaxed, Durability::Ordered, Durability::Strict] {
            std::fs::write(bare.plist(), named(tier).plist_text()).unwrap();
            let (kept, job) = kept_durability(&bare).expect("read");
            assert_eq!(kept, Some(tier));
            assert!(job.is_some_and(|j| j.contains(&bare.plist().display().to_string())));
        }
        std::fs::write(bare.plist(), bare.plist_text()).unwrap();
        assert_eq!(kept_durability(&bare).expect("read").0, None);
    }

    /// A BROKER RUNNING ANOTHER TIER IS NOT CURRENT: one whose process runs
    /// another `--durability` than the tier `on` asks for — a switch either
    /// way that the running broker never took — is named, so the supervisor
    /// restarts it (or, under `--service none`, the warning says how). One
    /// running the tier asked for, or whose process this host cannot read
    /// (no tier: a remote broker, no `ps`), is current on that count; a bare
    /// job and a named strict one ask for the same tier in this build. A
    /// replaced build and an unguarded broker are named first.
    #[test]
    fn a_broker_running_another_tier_is_not_current() {
        use crate::durability::Durability;
        let p = |durability| Paths {
            home: PathBuf::from("/h"),
            login_home: None,
            root: PathBuf::from("/h/root"),
            aterm: "/usr/local/bin/aterm".to_string(),
            fleet: "local".to_string(),
            config: None,
            rendezvous: None,
            wire: Wire::Unix,
            durability: Some(durability),
        };
        let view = |durability| fabric::BrokerView {
            guarded: Some(true),
            durability,
            ..fabric::BrokerView::default()
        };
        let (strict, ordered, relaxed) =
            (Durability::Strict, Durability::Ordered, Durability::Relaxed);
        assert_eq!(
            not_current(&p(strict), &view(Some(relaxed))),
            Some("it runs at durability relaxed")
        );
        assert_eq!(
            not_current(&p(relaxed), &view(Some(strict))),
            Some("it runs at durability strict")
        );
        assert_eq!(
            not_current(&p(strict), &view(Some(ordered))),
            Some("it runs at durability ordered")
        );
        assert_eq!(not_current(&p(relaxed), &view(Some(relaxed))), None);
        assert_eq!(not_current(&p(strict), &view(Some(strict))), None);
        assert_eq!(not_current(&p(ordered), &view(Some(ordered))), None);
        let bare = Paths {
            durability: None,
            ..p(strict)
        };
        assert_eq!(
            not_current(&bare, &view(Some(strict))),
            None,
            "bare is strict here"
        );
        assert_eq!(not_current(&p(relaxed), &view(None)), None, "no tier read");
        let replaced = fabric::BrokerView {
            replaced: Some("/usr/local/bin/aterm".to_string()),
            ..view(Some(strict))
        };
        assert_eq!(
            not_current(&p(relaxed), &replaced),
            Some("it runs a replaced build")
        );
    }

    /// THE PLIST RUNS THE BROKER DIRECTLY — the words, one `<string>` each,
    /// no `/bin/sh -c` — and is valid XML for any path; it reads as CURRENT in
    /// any layout, so `on` does not restart a healthy broker for its spelling;
    /// a foreign argv is not current.
    #[test]
    fn the_plist_runs_the_broker_directly_and_reads_as_current_in_any_layout() {
        let p = Paths {
            home: PathBuf::from("/Users//example"),
            login_home: Some(PathBuf::from("/Users//example")),
            root: PathBuf::from("/Users//example/.local/share/aterm-fabric"),
            aterm: "/Users//example/.local/bin/aterm".to_string(),
            fleet: "local".to_string(),
            config: None,
            rendezvous: None,
            wire: Wire::Unix,
            durability: None,
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
             \x20   <string>--secret-file</string>\n\
             \x20   <string>/Users//example/.local/share/aterm-fabric/mint.secret</string>\n\
             \x20 </array>\n"
            ),
            "{text}"
        );
        // AN INSTALL FROM BEFORE THE GUARD — the same job with no
        // `--secret-file` — is not current, so the next `on` rewrites and
        // restarts it guarded.
        let unguarded = text.replace(
            "    <string>--secret-file</string>\n    \
             <string>/Users//example/.local/share/aterm-fabric/mint.secret</string>\n",
            "",
        );
        assert_ne!(unguarded, text);
        assert!(!p.plist_is_current(&unguarded), "{unguarded}");
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
        // plistlib's layout (tabs, a line break between key and value, the
        // keys sorted) with the direct argv: current too.
        let plistlib = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict>\n\
             \t<key>KeepAlive</key>\n\t<true/>\n\
             \t<key>Label</key>\n\t<string>systems.alab.astream-broker</string>\n\
             \t<key>ProgramArguments</key>\n\t<array>\n\
             \t\t<string>/Users//example/.local/bin/aterm</string>\n\t\t<string>link</string>\n\t\t<string>broker</string>\n\
             \t\t<string>/Users//example/.local/share/aterm-fabric/bus.sock</string>\n\
             \t\t<string>/Users//example/.local/share/aterm-fabric/bus.log</string>\n\
             \t\t<string>--secret-file</string>\n\
             \t\t<string>/Users//example/.local/share/aterm-fabric/mint.secret</string>\n\t</array>\n\
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
                "/Users//example/.local/share/aterm-fabric/bus.log",
                "--secret-file",
                "/Users//example/.local/share/aterm-fabric/mint.secret"
            ]
        );
        // The systemd unit runs the same guarded words, and unlinks no
        // socket first: the broker refuses a live one and unlinks a stale one.
        let unit = p.unit_text();
        assert!(!unit.contains("rm -f"), "{unit}");
        // Its stderr lands where the launchd job's does, the file whose last
        // line `aterm fabric` and doctor quote (r2-02).
        assert!(
            unit.contains(
                "\nStandardError=append:/Users//example/.local/share/aterm-fabric/broker.err\n"
            ),
            "{unit}"
        );
        assert!(
            p.plist_text().contains(
                "<key>StandardErrorPath</key><string>/Users//example/.local/share/aterm-fabric/\
                 broker.err</string>"
            ),
            "the same file"
        );
        assert!(
            unit.contains(
                "ExecStart=/bin/sh -c \"exec '/Users//example/.local/bin/aterm' 'link' 'broker' \
                 '/Users//example/.local/share/aterm-fabric/bus.sock' \
                 '/Users//example/.local/share/aterm-fabric/bus.log' '--secret-file' \
                 '/Users//example/.local/share/aterm-fabric/mint.secret'\"\n"
            ),
            "{unit}"
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
            durability: None,
        };
        let installed = p(
            "/Users//example/.local/share/aterm-fabric",
            Some("/Users//example"),
        );
        assert_eq!(
            installed.plist(),
            PathBuf::from("/Users//example/Library/LaunchAgents/systems.alab.astream-broker.plist")
        );
        assert!(installed.starts_at_login(Service::Launchd));
        let redirected = p("/tmp/atfo-x/root", Some("/Users//example"));
        assert_eq!(
            redirected.plist(),
            PathBuf::from(format!("/tmp/atfo-x/root/{}.plist", redirected.label()))
        );
        assert!(!redirected.plist().starts_with("/Users//example/Library"));
        assert!(!redirected.starts_at_login(Service::Launchd));
        assert!(redirected.starts_at_login(Service::Systemd));
        assert!(!redirected.starts_at_login(Service::None));
        // A root that is `$HOME`'s default but not the LOGIN home's is not
        // the default root, and its plist stays with it.
        let foreign = Paths {
            home: PathBuf::from("/tmp/x"),
            ..p("/tmp/x/.local/share/aterm-fabric", Some("/Users//example"))
        };
        assert!(foreign
            .plist()
            .starts_with("/tmp/x/.local/share/aterm-fabric/"));
        assert!(!foreign.starts_at_login(Service::Launchd));
        // Even the default root is not auto-loaded when HOME redirected the
        // plist away from the login user's LaunchAgents directory.
        let redirected_home = Paths {
            home: PathBuf::from("/tmp/x"),
            ..installed
        };
        assert!(!redirected_home.starts_at_login(Service::Launchd));
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
        // systemd's falls back to the journal, where an older unit wrote.
        let absent = systemd_failure_hint(&dir, "astream-broker-7");
        assert!(
            absent.contains("broker.err was not written")
                && absent.contains("journalctl --user -u astream-broker-7"),
            "{absent}"
        );
        std::fs::write(dir.join("broker.err"), "").expect("empty");
        assert!(launchd_failure_hint(&dir, "l", "501").contains("was not written"));
        std::fs::write(dir.join("broker.err"), " \n\n").expect("blank");
        assert_eq!(supervisor_last_word(&dir), None, "whitespace is not a word");
        // THE LAST LINE, of a file every start of the job appended to.
        std::fs::write(
            dir.join("broker.err"),
            "bind: address in use\naterm link broker: open bus.log: refused\n\n",
        )
        .expect("err");
        let written = launchd_failure_hint(&dir, "l", "501");
        assert!(
            written.starts_with(&format!(
                "read {} (written ",
                dir.join("broker.err").display()
            )) && written.ends_with("ago: aterm link broker: open bus.log: refused)"),
            "{written}"
        );
        assert_eq!(
            systemd_failure_hint(&dir, "l").split(" (written ").next(),
            written.split(" (written ").next(),
            "a unit that writes broker.err is read the same way"
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
        edit.replace("font_px = 9.0\n")
            .expect_err("changed under it");
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
        assert_eq!(Rendezvous::parse(&text), Ok(r.clone()));
        let odd = Rendezvous {
            command: "a \"quoted\" \\ back".to_string(),
            ..r
        };
        assert_eq!(Rendezvous::parse(&odd.render()), Ok(odd));
        assert!(Rendezvous::parse("fleet = 1\n").is_err());
        assert!(Rendezvous::parse("[x\n").is_err());
    }

    /// THE GRANTS are the node ring's eight, in order, with the node in six.
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

    /// EVERY WARNING HAS A FIX, under every [`Role`], and the unknown one
    /// points at the manual. And no fix on a host of a cross-host fleet is the
    /// bare one-host `on`: on a JOINED host it would leave the fleet, and on
    /// the host that SERVES it, its broker job would lose the `--tcp` every
    /// joined host dials.
    #[test]
    fn every_warning_shape_has_a_fix() {
        let serves = "`aterm fabric on --tcp 0.0.0.0:7000 --key-file /k/fleet.key --allow-remote`";
        let roles = [
            Role::OneHost { fleet: None },
            Role::Serves(Served {
                fleet: Some(DEFAULT_FLEET.to_string()),
                bind: Some("0.0.0.0:7000".to_string()),
                dial: None,
                key_file: "/k/fleet.key".to_string(),
                allow_remote: true,
                job: Some(Service::Launchd),
                starts_at_login: true,
            }),
            Role::Joined(Joined {
                fleet: DEFAULT_FLEET.to_string(),
                broker: "m100.local:7000".to_string(),
                key_file: Some("/r/fleet.key".to_string()),
                cap_file: Some("/r/node.cap".to_string()),
                accept_from: Some("n-b,n-a".to_string()),
            }),
        ];
        for w in [
            "the broker at /x does not answer (connect: x): no record lands and none is delivered",
            "instance 1 says fabric=connected (its bridge's last ack was 3s ago), but the broker at \
             /tmp/b.sock does not answer this report's own probe",
            "instance 1's bridge is attached but its broker link is down (fabric=stalled \
             reason=refused, last ack 3s ago)",
            "instance 1's bridge is attached but its broker link is down (fabric=stalled \
             reason=expired, last ack 3s ago)",
            "the broker at /x refuses this node's cap as lapsed (its grants' `exp=` has passed): \
             no record of this node lands and none is delivered to it — the broker refused the \
             ring in /c: x",
            "instance 1's bridge is attached and has not reported its broker link (fabric=stalled \
             reason=starting)",
            "the broker at /x answers but the bus could not be read: head query",
            "the broker at /x speaks astream protocol 3 and this aterm speaks 2: neither can read \
             what the other sends, so no record lands and none is delivered",
            "the broker at /x does not answer (connect: x): no record lands and none is \
             delivered; its supervisor's broker.err, written 3m ago: open bus.log: refused",
            "no [fabric] command in /c: this report read instance 1's running bridge",
            "no node id in /s/node",
            "cap file /c: the tag is not hex",
            "instance 1 has no bridge (fabric=absent, not supervised)",
            "instance 1's bridge is down (fabric=disconnected)",
            "instance 1's bridge was armed with a different command than x",
            "instance 2's bridge stands by (fabric=stalled reason=state-locked): instance 1's \
             bridge holds its state dir /s",
            "instance 1 says fabric=connected, but the bus's own presence for its node n-a says state=gone",
            "instance 1 says fabric=connected, but the bus's own presence for its node n-a says \
             state=live fabric=disconnected",
            "instance 1's bridge has not answered for the work it was given (fabric=stale, last ack \
             never)",
            "ask off=40 from h-andrew to @s-one@n-a passed its deadline 10m ago (dl=600000 ms) with \
             no answer, report or ack — not yet recorded expired by the asker's bridge",
            "ask off=41 from s-one@n-a to @h-andrew@p passed its deadline 10m ago (dl=600000 ms) \
             with no answer, report or ack — the asker's bridge recorded it expired",
            "h-andrew's fleet halt (reason=stop) stands on this node with no row on the bus: it \
             was read on a bus log since replaced",
            "@s-a (instance 1) is HELD (reason=x origin=local)",
            "@s-a (instance 1) lost 2 message(s): its inbox ring evicted rows",
            "@s-a (instance 1) has 1 unhandled task/ask older than 10 min",
            "@s-a (instance 1) has 3 queued post(s) and no working bridge",
            "instance 1 did not answer (x)",
            "cannot list this machine's aterm instances: x",
            "the bus advertises @s-g live on n-a",
            "h-andrew has a fleet halt standing (reason=stop)",
            "no rendezvous file at /x",
            "instance 1's bridge is another build than the instance: bridge a, instance b",
            "instance 1 could not parse 3 of its bridge's lines (lane_faults=3)",
        ] {
            for role in &roles {
                let fix = fix_for(w, role);
                assert_ne!(fix, "see `aterm help fabric`", "no fix for: {w}");
                let on = fix.matches("`aterm fabric on").count();
                match role {
                    Role::OneHost { .. } => {}
                    Role::Serves(_) => assert_eq!(on, fix.matches(serves).count(), "{w}: {fix}"),
                    Role::Joined(_) => {
                        assert_eq!(on, 0, "a joined host is sent to `on`: {w}: {fix}")
                    }
                }
            }
        }
        assert_eq!(
            fix_for("something new", &Role::OneHost { fleet: None }),
            "see `aterm help fabric`"
        );
    }

    /// **A LOG THE BROKER REFUSES IS ANSWERED BY ITS OWN REMEDY** (audit 51,
    /// r2-02): the broker's last line ([`crate::cli::log_refusal_remedy`]),
    /// quoted from broker.err, picks the fix — the shipped repair for a
    /// corrupt record, and NEVER a repair for a log a newer aterm wrote, which
    /// a truncation would destroy. A refusal with no remedy of aterm's own
    /// keeps the general "fix the quoted cause first". A BRIDGE'S DIAL nothing
    /// answered on that broker (`reason=no-socket|refused`) quotes the same
    /// line and gets the same remedy, never "`on` starts it" into the refusal;
    /// with nothing quoted it keeps that one.
    #[test]
    fn a_refused_log_gets_its_own_remedy_and_a_newer_one_never_a_repair() {
        use std::io::ErrorKind as K;
        let warning = |last: &str| {
            format!(
                "the broker at /r/bus.sock does not answer (connect: x): no record lands and none \
                 is delivered; its supervisor's broker.err, written 1s ago: aterm link broker: \
                 {last}"
            )
        };
        let bridge = |reason: &str, last: Option<&str>| {
            format!(
                "instance 1's bridge is attached but its broker link is down (fabric=stalled \
                 reason={reason}, last ack never): its posts queue and no mail arrives from the \
                 broker at /r/bus.sock{}",
                last.map_or_else(String::new, |l| format!(
                    "; its supervisor's broker.err, written 1s ago: aterm link broker: {l}"
                ))
            )
        };
        let reconnects = "; the bridge then reconnects on its own";
        let line = |kind| crate::cli::log_refusal_remedy(kind, "/r/bus.log").expect("a remedy");
        for role in [
            Role::OneHost { fleet: None },
            Role::Serves(Served {
                fleet: None,
                bind: Some("0.0.0.0:7000".to_string()),
                dial: None,
                key_file: "/k/fleet.key".to_string(),
                allow_remote: true,
                job: Some(Service::Launchd),
                starts_at_login: true,
            }),
        ] {
            let corrupt = fix_for(&warning(&line(K::InvalidData)), &role);
            assert!(corrupt.contains("`aterm link broker --repair"), "{corrupt}");
            let newer = fix_for(&warning(&line(K::Unsupported)), &role);
            assert!(!newer.contains("--repair"), "{newer}");
            assert_ne!(newer, corrupt);
            let other = fix_for(&warning("open /r/bus.log: address in use"), &role);
            assert!(other.contains("fix that first"), "{other}");
            for reason in ["no-socket", "refused"] {
                let from = |last: Option<&str>| fix_for(&bridge(reason, last), &role);
                assert_eq!(
                    from(Some(&line(K::InvalidData))),
                    format!("{corrupt}{reconnects}")
                );
                assert_eq!(
                    from(Some(&line(K::Unsupported))),
                    format!("{newer}{reconnects}")
                );
                assert_eq!(
                    from(Some("open /r/bus.log: address in use")),
                    format!("{other}{reconnects}")
                );
                // NEGATIVE CONTROL: nothing quoted, nothing listens — `on`
                // starts it.
                let bare = from(None);
                assert!(
                    bare.starts_with("nothing listens at the broker's socket")
                        && bare.contains("starts it")
                        && !bare.contains("--repair"),
                    "{bare}"
                );
            }
        }
        assert_eq!(
            crate::cli::log_refusal_remedy(K::AlreadyExists, "/r/bus.log"),
            None
        );
    }

    /// **A LAPSED CAP IS RENEWED, NEVER RE-MINTED FROM SCRATCH** (audit 16).
    /// On a joined host the bridge's `reason=expired` and the report's lapsed
    /// ring get one remedy: the same ring minted again on the host that serves
    /// the fleet with `mint-for --expires`, joined here over the installed cap,
    /// which the running bridge attaches at its next redial — not the generic
    /// refused-cap fix, and never a one-host `on`. NEGATIVE CONTROL: a cap the
    /// broker refuses for another reason (`reason=attach`) keeps that fix.
    #[test]
    fn a_lapsed_cap_is_renewed_where_a_refused_one_is_minted_again() {
        let role = Role::Joined(Joined {
            fleet: DEFAULT_FLEET.to_string(),
            broker: "m100.local:7000".to_string(),
            key_file: Some("/r/fleet.key".to_string()),
            cap_file: Some("/r/node.cap".to_string()),
            accept_from: None,
        });
        let down = |reason: &str| {
            fix_for(
                &format!(
                    "instance 1's bridge is attached but its broker link is down (fabric=stalled \
                     reason={reason}, last ack 3s ago)"
                ),
                &role,
            )
        };
        let renew = down("expired");
        assert!(
            renew.contains("`aterm fabric mint-for <this node> --out <cap> --expires <N>d` there")
                && renew.contains(
                    "`aterm fabric join --broker m100.local:7000 --tcp --key-file /r/fleet.key \
                     --cap-file <cap>` replaces /r/node.cap"
                )
                && renew.contains("next redial")
                && !renew.contains("aterm fabric on"),
            "{renew}"
        );
        assert_eq!(
            fix_for(
                &format!("the broker at m100.local:7000 {}: x", fabric::CAP_LAPSED),
                &role
            ),
            renew_remedy(&role, &role.again())
        );
        assert!(renew.starts_with(&renew_remedy(&role, &role.again())));
        let refused = down("attach");
        assert!(
            !refused.contains("--expires") && refused.contains("another secret"),
            "{refused}"
        );
    }

    /// **ONE REMEDY WHERE THE WARNING ALREADY SAYS WHICH APPLIES.** A hold's
    /// origin and reason, and a down link's `reason=`, are in the warning's own
    /// words, so the fix names the one remedy for that cause. A hold's fix is a
    /// command that reaches the hosting instance: a flagless `aterm ctl`
    /// reaches the caller's own (or the newest) instance, whose store answers
    /// `ERR no such session` for another's, so the fix names the instance by
    /// pid — or by socket for an explicit `--control-sock` one, shell-quoted
    /// when it can be and a placeholder when it cannot.
    #[test]
    fn a_fix_picks_the_remedy_the_warning_names() {
        fn fix_for(warning: &str) -> String {
            super::fix_for(warning, &Role::OneHost { fleet: None })
        }
        let held = |why: &str| fix_for(&format!("@s-a (instance 1) is HELD ({why})"));
        assert_eq!(
            held("reason=x origin=local"),
            "`aterm ctl --pid 1 hold s-a off`"
        );
        assert_eq!(
            fix_for("a session is HELD (reason=x origin=local)"),
            "`aterm ctl --pid <pid> hold <sid> off`"
        );
        assert_eq!(
            fix_for("@s-a (instance 1, --sock /tmp/at.x/a.sock) is HELD (reason=x origin=local)"),
            "`aterm ctl --sock /tmp/at.x/a.sock hold s-a off`"
        );
        assert_eq!(
            fix_for("@s-a (instance 0, --sock /tmp/a b.sock) is HELD (reason=x origin=local)"),
            "`aterm ctl --sock '/tmp/a b.sock' hold s-a off`"
        );
        assert_eq!(
            fix_for("@s-a (instance 0, --sock /tmp/a'b.sock) is HELD (reason=x origin=local)"),
            "`aterm ctl --sock <sock> hold s-a off`"
        );
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
        // A fleet hold's remedy is its halter's `halt off`, never the Owner's
        // `hold off` (that is `ERR denied` for a fleet hold).
        let fleet = held("reason=stop origin=fleet");
        assert!(
            fleet.contains("aterm fabric halt") && !fleet.contains("hold s-a off"),
            "{fleet}"
        );
        // BOTH SLOTS: the fleet's remedy, and the owner's `hold off` for the
        // local hold the fleet's lift leaves standing.
        let both = held("reason=stop origin=fleet,local");
        assert!(
            both.contains("aterm fabric halt") && both.contains("`aterm ctl --pid 1 hold s-a off`"),
            "{both}"
        );
        let lost = held("reason=fabric-lost origin=fleet,local");
        assert!(
            lost.contains("bridge reconnects") && lost.contains("hold s-a off"),
            "{lost}"
        );

        let down = |reason: &str| {
            fix_for(&format!(
                "instance 1's bridge is attached but its broker link is down (fabric=stalled \
                 reason={reason}, last ack 3s ago)"
            ))
        };
        // A LAPSED cap is renewed, not re-minted for another secret: its own
        // remedy, the same from the bridge's `reason=expired` as from the
        // report's own probe ([`fabric::CAP_LAPSED`]).
        assert!(
            down("expired").contains("has lapsed") && down("expired") != down("attach"),
            "{}",
            down("expired")
        );
        assert!(
            fix_for(&format!("the broker at /x {}: x", fabric::CAP_LAPSED)).starts_with(
                &renew_remedy(&Role::OneHost { fleet: None }, "`aterm fabric on`")
            ),
            "{}",
            fix_for(&format!("the broker at /x {}: x", fabric::CAP_LAPSED))
        );
        // A broker in another astream protocol gets one remedy, the same from
        // the bridge's `reason=` as from the report's own probe — a release,
        // spelled for the host: here `on` restarts the broker, and a joined
        // host is sent to build both hosts alike, not to restart anything.
        let skew = "the broker at /x speaks astream protocol 3 and this aterm speaks 2";
        let here = protocol_remedy(&Role::OneHost { fleet: None }, "`aterm fabric on`", "");
        assert!(down("protocol").starts_with(&here), "{}", down("protocol"));
        assert_eq!(fix_for(skew), here);
        // A full broker, a refused row and another protocol are not a refused
        // cap: none is sent to move the cap aside.
        for reason in ["busy", "record", "protocol"] {
            assert!(
                !down(reason).contains("node.cap aside") && down(reason) != down("other"),
                "{reason}: {}",
                down(reason)
            );
        }
        // A cut is the broker answering: it is not sent to the broker.
        assert!(
            down("cut-say") != down("closed") && down("cut-inbox") == down("cut-say"),
            "{}",
            down("cut-say")
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
            durability: None,
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
        // WHAT A BARE `on` KEEPS is read back from the job it wrote
        // (`served_wire`): the plist's words and the unit's are the broker
        // argv, word for word, and a Unix job's carry no `--tcp`.
        assert_eq!(
            plist_program_arguments(&collapse_between_tags(&plist)),
            Some(p.broker_argv())
        );
        assert_eq!(unit_argv(&unit), Some(p.broker_argv()));
        assert_eq!(unit_argv(&unix.unit_text()), Some(unix.broker_argv()));
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
            on(Some("127.0.0.1:7000"), Some("/k"), false).expect_err("default build");
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

    /// **THE DELETED SCRIPT'S SHELL JOB IS READ FOR ITS BROKER, AND NOTHING
    /// ELSE IS.** fabric-enable.sh installed `/bin/sh -c "rm -f …; exec …"`;
    /// [`sh_c_words`] reads that shape and the one [`Paths::unit_text`]
    /// writes, and refuses every other script rather than guess at it.
    #[test]
    fn the_deleted_scripts_shell_job_is_read_for_its_broker_and_nothing_else_is() {
        let words = |w: &[&str]| Some(w.iter().map(ToString::to_string).collect::<Vec<_>>());
        let legacy = "rm -f '/r/bus.sock'; exec '/u/aterm' link broker '/r/bus.sock' '/r/bus.log'";
        let want = words(&["/u/aterm", "link", "broker", "/r/bus.sock", "/r/bus.log"]);
        assert_eq!(sh_c_words(legacy), want);
        assert_eq!(
            sh_c_words("exec '/u/aterm' 'link' 'broker' '/r/bus.sock' '/r/bus.log'"),
            want,
            "the unit's every-word-quoted form"
        );
        assert_eq!(
            sh_c_words(" \trm -f /r/bus.sock;exec\t/u/aterm link broker /r/bus.sock /r/bus.log \n"),
            want,
            "bare words, tabs, no space after `;`, whitespace around the script"
        );
        assert_eq!(sh_c_words("exec /u/aterm ''"), words(&["/u/aterm", ""]));
        for bad in [
            "curl x; exec /u/aterm link broker a b",
            "rm -f a; exec /u/aterm link broker a b && true",
            "rm -f a; exec /u/aterm link broker a b; true",
            "rm -f a b; exec /u/aterm link broker a b",
            "rm -f a; rm -f b; exec /u/aterm link broker a b",
            "exec \"/u/aterm\" link broker a b",
            "exec $X broker a b",
            "exec /u/aterm link broker a\\ b",
            "exec /u/aterm link broker /r/*.sock",
            "exec /u/aterm link broker 'a'b",
            "exec /u/aterm link broker 'a",
            "exec /u/aterm link broker a\nb",
            "rm -f a; /u/aterm link broker a b",
            "/u/aterm link broker a b",
            "exec",
            "rm -f a; exec",
            "",
        ] {
            assert_eq!(sh_c_words(bad), None, "{bad:?}");
        }

        let sh = |script: &str| vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()];
        assert_eq!(supervised_argv(sh(legacy)), want, "the shell job unwraps");
        assert_eq!(supervised_argv(sh("curl x; exec a")), None);
        let direct = want.clone().expect("words");
        assert_eq!(
            supervised_argv(direct.clone()),
            Some(direct),
            "a direct argv is the command"
        );
        let zsh = vec!["/bin/zsh".to_string(), "-c".to_string(), legacy.to_string()];
        assert_eq!(
            supervised_argv(zsh.clone()),
            Some(zsh),
            "only `sh` is unwrapped: another shell is an argv `Role::of` refuses"
        );
    }

    /// **A ROOT WHOSE JOB THE DELETED SCRIPT INSTALLED HAS A KNOWN ROLE, AND
    /// ITS JOB IS STILL NOT CURRENT** — so `doctor` reports and `on` rewrites
    /// it, instead of both refusing the host's own broker as an unknown role.
    #[test]
    fn role_of_a_root_with_the_scripts_plist_is_known_and_not_current() {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let root = dir.path().join("fabric");
        std::fs::create_dir_all(&root).expect("root");
        let p = Paths {
            home: dir.path().join("home"),
            login_home: Some(dir.path().join("login")),
            root: root.clone(),
            aterm: "/u/aterm".to_string(),
            fleet: DEFAULT_FLEET.to_string(),
            config: None,
            rendezvous: None,
            wire: Wire::Unix,
            durability: None,
        };
        assert!(
            p.plist().starts_with(&root),
            "a non-default root keeps its plist"
        );
        let r = root.to_string_lossy().into_owned();
        let plist = |script: &str| {
            format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict>\n\
                 \t<key>Label</key>\n\t<string>{label}</string>\n\
                 \t<key>ProgramArguments</key>\n\t<array>\n\
                 \t\t<string>/bin/sh</string>\n\t\t<string>-c</string>\n\
                 \t\t<string>{script}</string>\n\t</array>\n\
                 \t<key>KeepAlive</key>\n\t<true/>\n</dict>\n</plist>\n",
                label = p.label(),
                script = xml_escape(script),
            )
        };
        let install = |text: &str| std::fs::write(p.plist(), text).expect("plist");

        let legacy = plist(&format!(
            "rm -f '{r}/bus.sock'; exec '/u/aterm' link broker '{r}/bus.sock' '{r}/bus.log'"
        ));
        install(&legacy);
        assert_eq!(Role::of(&p), Ok(Role::OneHost { fleet: None }));
        assert!(!p.plist_is_current(&legacy), "`on` still rewrites it");

        // The script's sealed job quoted every word: it SERVES the fleet.
        install(&plist(&format!(
            "rm -f '{r}/bus.sock'; exec '/u/aterm' 'link' 'broker' '--tcp' '127.0.0.1:7000' \
             '--key-file' '/k' '--unix' '{r}/bus.sock' '{r}/bus.log'"
        )));
        match Role::of(&p) {
            Ok(Role::Serves(served)) => {
                assert_eq!(served.bind.as_deref(), Some("127.0.0.1:7000"));
                assert_eq!(served.key_file, "/k");
                assert_eq!(served.job, Some(Service::Launchd));
            }
            other => panic!("the legacy sealed job serves: {other:?}"),
        }

        // NEGATIVE CONTROLS: a readable script that runs no broker, and one
        // of a shape nothing aterm wrote, are still refused.
        install(&plist("exec /usr/bin/true"));
        assert_eq!(Role::of(&p), Err(RoleError::NotABroker));
        install(&plist(&format!(
            "curl x; exec '/u/aterm' link broker '{r}/bus.sock' '{r}/bus.log'"
        )));
        assert_eq!(
            Role::of(&p),
            Err(RoleError::UnreadableJob {
                path: p.plist(),
                field: "ProgramArguments",
            })
        );
    }
}
