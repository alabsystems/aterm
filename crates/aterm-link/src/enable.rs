// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm fabric on|off|doctor` — the fabric turns on in one command and proves
//! itself.
//!
//! ```text
//! aterm fabric on  [--dry-run] [--fleet <F>] [--service launchd|systemd|none]
//! aterm fabric off [--dry-run] [--service launchd|systemd|none]
//! aterm fabric doctor
//! ```
//!
//! `on` is `tools/fabric-enable.sh --enable` in Rust, inside the one binary an
//! installed aterm has, plus the three things the script could not do: record
//! where the fabric is so every client finds it WITHOUT arguments (the
//! rendezvous file), arm the instances that are already running, and PROVE the
//! result with a message that round-trips through the broker. Every step is
//! idempotent and says whether it changed anything, so a second `on` changes
//! nothing and says so; `--dry-run` prints every step and touches nothing.
//!
//! ## The steps, in order
//!
//! 1. The binary: `$ATERM_BIN`, else `aterm` on `$PATH`, else this executable.
//!    It has to carry `link serve` and `link broker`, and every word of the
//!    bridge command it will be written into is checked for whitespace, `"` and
//!    `\` FIRST: aterm splits `[fabric] command` on whitespace with no quoting
//!    grammar (`fabric_launch.rs`, `configured_command`), so a path with a space
//!    becomes two argv words and the bridge dials the wrong broker.
//! 2. The root, `$ATERM_FABRIC_HOME` or `~/.local/share/aterm-fabric`, 0700.
//!    The broker's socket lives in it, and its directory IS the security
//!    boundary: `aterm link broker` checks no capability and no peer uid.
//!    The socket path has to fit `sun_path` (104 bytes on macOS, one of them the
//!    NUL) and is refused before anything is written when it does not — measured
//!    2026-09-10, a root under a long `/private/tmp/...` path never bound.
//! 3. The node id, `<root>/link-state/node`: PROVISIONED once, never re-minted.
//!    It is baked into six of the eight grants, so a new id abandons this node's
//!    mail lane. `off` keeps it for the same reason.
//! 4. The mint secret, `<root>/mint.secret`, 32 random bytes, 0600. An existing
//!    short one is refused, never replaced: HMAC accepts any key length, and a
//!    truncated secret mints capabilities that look well-formed and seal nothing.
//! 5. The cap file, `<root>/node.cap`: the eight grants of §8.2's node ring,
//!    minted for the node id ACTUALLY in the state dir — a cap file that names
//!    another id (a wiped state dir, a copied root) is re-minted. The mint is a
//!    TRANSACTION: every grant into a staging file beside the cap, then one
//!    rename; a refused mint leaves the previous cap byte-identical and stops
//!    before broker supervision is touched. `crates/aterm-spec`'s
//!    `fabric_capability_publication_model` is the model of that order and
//!    `tests/fabric_on.rs` runs the real command's effect trace through it.
//! 6. The broker, supervised: a launchd job on macOS (`KeepAlive`), a
//!    `systemd --user` unit on Linux (`Restart=always`), or `--service none` for
//!    a broker something else keeps alive (the tests' own, or a hand-run
//!    `aterm link broker`). THE LABEL IS DERIVED FROM THE ROOT: the default root
//!    keeps the plain `systems.alab.astream-broker`, any other root gets
//!    `.<cksum>` appended — so a run under a different `$ATERM_FABRIC_HOME`
//!    (every test) can never boot out the machine's real broker. Learned on
//!    2026-09-10, when a `--disable` under a redirected config stopped the real
//!    broker and deleted its plist. "THE DEFAULT ROOT" IS THE LOGIN USER'S —
//!    `<passwd home>/.local/share/aterm-fabric`, the home the password
//!    database gives this uid — and NOT `$HOME/…`: launchd's `gui/<uid>`
//!    domain is per uid, not per `$HOME`, so a run under a redirected `$HOME`
//!    (`HOME=/tmp/x aterm fabric on`, the round-13 review's case) whose root
//!    is `$HOME/.local/share/aterm-fabric` would otherwise carry the plain
//!    label and boot the real broker out. Two roots never share a label; an
//!    unknown login home means every root derives one. The job is then
//!    PROBED, not assumed: a connect, the broker's hello, every cap attached
//!    and the head query — and a broker that answers on the socket while
//!    launchd has no such job is NOT bootstrapped over: bootstrapping the
//!    plist would put a second broker on the same socket and the same log, so
//!    `on` names its pid and stops (or `--service none`). THE PLIST FOLLOWS
//!    THE ROOT TOO: `~/Library/LaunchAgents/<label>.plist` under the default
//!    root, `<root>/<label>.plist` under any other — with only the label
//!    derived, a run under a redirected `$ATERM_FABRIC_HOME` still dropped a
//!    persistent plist into the real LaunchAgents dir and loaded a second
//!    `KeepAlive` broker into the live gui domain, outliving the scratch dir
//!    (the audit's finding on the script). `bootstrap` goes by file path and
//!    `bootout` by label, so nothing a redirected run does reaches the
//!    operator's real install. The plist runs the broker DIRECTLY — its
//!    ProgramArguments are the words, no `/bin/sh -c`, no quoting, and every
//!    value is XML-escaped, so a `&` or `<` in a path cannot produce a plist
//!    launchd cannot parse — and the stale-socket `rm -f` the script's shell
//!    did is the broker verb's own bind now (it unlinks a socket nobody
//!    answers on). An install carrying the script's `/bin/sh -c "rm -f …;
//!    exec …"` argv is read as CURRENT, not restarted for its spelling.
//! 7. `[fabric] command` in aterm.toml: written atomically with the previous file
//!    saved as `aterm.toml.bak`, replacing only the `command` key of an existing
//!    `[fabric]` table and leaving every other table and key alone.
//!    `--accept-from <node>` is part of the command, not decoration: without it a
//!    `task` between two sessions of this node arrives demoted (`kind=note
//!    demoted=task`), which `await inbox` skips by default.
//! 8. The rendezvous file, `fabric.toml` beside the instance control sockets
//!    (`$XDG_RUNTIME_DIR/aterm`, else `~/Library/Application Support/aterm`),
//!    0600: fleet, broker, node, cap file, state dir and the bridge command.
//!    `aterm link ls|glance|tui` default their `--fleet --broker --cap-file
//!    --state` from it, `aterm link mirror` its `--sock`, and `aterm fabric`
//!    reads its command when aterm.toml has none.
//! 9. Every running instance `aterm ctl instances` lists is armed with
//!    `fabric attach <argv>` — the argv, because `[fabric] command` is recorded
//!    once, at launch, and a bare attach on an instance launched before step 7
//!    answers `ERR fabric no command`. An instance already supervised with this
//!    command is left alone; one supervised with a DIFFERENT command is named
//!    (a supervisor arms once per process; only a relaunch changes it).
//! 10. The proof: a `note` posted from a session to ITSELF, `--wait` for the
//!     broker's landing, then `await inbox` for its delivery — one record all
//!     the way out to the bus and back through the bridge, within 5 s. The
//!     session is the caller's own (`$ATERM_PARENT_SESSION_ID`) when an armed
//!     instance hosts it, else the first session of the first armed instance;
//!     no session is spawned. A failed proof is exit 1 and says what to check;
//!     no running instance means no proof, and the line says so.
//! 11. The undo, spelled WITH the environment that scoped this run
//!     (`ATERM_FABRIC_HOME`, `XDG_CONFIG_HOME`, `XDG_RUNTIME_DIR`, `ATERM_BIN`,
//!     a `HOME` that is not the login home): a sandboxed `on` used to print a
//!     bare `--disable` that, run as printed, reached the real install (the
//!     audit's finding on the script).
//! 12. `aterm fabric` status, printed last.
//!
//! ## `off`
//!
//! Stops and removes the broker job, removes the `[fabric]` table AND every
//! `[fabric.<sub>]` table under it (with a `.bak`; a subtable left behind
//! re-creates the `fabric` key, and the line names each header it removed),
//! removes the rendezvous file — and KEEPS the node id, the secret and
//! the cap: identity is provisioned, never discarded. The bus log stays where it
//! is and the line says where. A running instance keeps its bridge until it is
//! relaunched, because a supervisor has no stop handle; the next launch starts
//! none.
//!
//! THE ROOT `off` ACTS ON IS THE ONE THE FABRIC IS ON: the `--broker` the
//! `[fabric] command` in aterm.toml dials (else the rendezvous file's), and
//! only then `$ATERM_FABRIC_HOME` or the default. `on` under one root and
//! `off` under another (a different `$ATERM_FABRIC_HOME`, or none) used to
//! answer "not installed" and leave the first root's `KeepAlive` broker
//! running for ever (the round-13 review).
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
//! ## What `--tcp --key-file` does here
//!
//! Nothing yet, and it says so rather than storing a flag the bridge could not
//! dial: `aterm link broker` serves the Unix socket only, and the sealed TCP
//! listener is behind astream's `aead` feature that a default build (and the
//! shipped binary) does not carry. Round 16 adds it; until then the flags are
//! refused by name with that reason.

use std::collections::BTreeSet;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use crate::ctl::Ctl;
use crate::fabric::{self, kv};
use crate::tui::safe;

/// The rendezvous file's name, beside the instance control sockets.
pub const RENDEZVOUS_FILE: &str = "fabric.toml";

/// The fleet `on` joins unless `--fleet` (or `$ATERM_FABRIC_FLEET`) says
/// otherwise — the name `tools/fabric-enable.sh` used.
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

/// TEST-ONLY: append one effect word per line (`Mint`, `Reject`, `Publish`,
/// `Supervise`) to this file, so a test can run the real command's effects
/// through the capability-publication model. Never set by anything shipped.
const TRACE_ENV: &str = "ATERM_FABRIC_TRACE";

/// TEST-ONLY fault injection: refuse the `<n>`th grant's mint. The same shape
/// as the bridge's `ATERM_LINK_FAULT` — a refusal that has to be reachable to be
/// tested, and the mint itself cannot be made to fail from outside.
const FAIL_MINT_ENV: &str = "ATERM_FABRIC_FAIL_MINT_AT";

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
    /// `--fleet`; `None` is `$ATERM_FABRIC_FLEET`, else [`DEFAULT_FLEET`].
    pub fleet: Option<String>,
    /// `--tcp <bind>` — refused for now (module doc).
    pub tcp: Option<String>,
    /// `--key-file <path>` — refused for now (module doc).
    pub key_file: Option<String>,
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
        let root = std::env::var_os("ATERM_FABRIC_HOME")
            .filter(|r| !r.is_empty())
            .map_or_else(|| default_root(&home), PathBuf::from);
        let aterm = match std::env::var("ATERM_BIN") {
            Ok(b) if !b.trim().is_empty() => b,
            _ => match which("aterm") {
                Some(p) => p.to_string_lossy().into_owned(),
                None => std::env::current_exe()
                    .map_err(|e| format!("no aterm on $PATH and no current exe: {e}"))?
                    .to_string_lossy()
                    .into_owned(),
            },
        };
        let fleet = fleet
            .map(str::to_string)
            .or_else(|| std::env::var("ATERM_FABRIC_FLEET").ok())
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
            .filter(|cfg| matches!(cfg.transport, crate::transport::Transport::Unix))
            .map(|cfg| (cfg.broker, "[fabric] command"));
        let found = configured.or_else(|| {
            Rendezvous::read()
                .ok()
                .flatten()
                .map(|r| (r.broker, "the rendezvous file"))
        });
        let Some((broker, source)) = found else {
            return Ok((p, "the environment"));
        };
        match Path::new(&broker).parent() {
            Some(root) if root.is_absolute() => {
                p.root = root.to_path_buf();
                Ok((p, source))
            }
            _ => Ok((p, "the environment")),
        }
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
    /// root, `LABEL.<cksum of the root>` under any other — the rule
    /// `tools/fabric-enable.sh` learned the hard way (module doc). A function
    /// of the ROOT alone: two roots never share a label.
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
    fn log(&self) -> String {
        self.root.join("bus.log").to_string_lossy().into_owned()
    }
    fn cap(&self) -> PathBuf {
        self.root.join("node.cap")
    }
    fn secret(&self) -> PathBuf {
        self.root.join("mint.secret")
    }
    fn state(&self) -> PathBuf {
        self.root.join("link-state")
    }
    /// The launchd job's plist: `~/Library/LaunchAgents/<label>.plist` under
    /// the default root, `<root>/<label>.plist` under any other — THE PLIST
    /// FOLLOWS THE ROOT, like the label (module doc, step 6): a redirected run
    /// keeps its persistent job inside its own root instead of the operator's
    /// LaunchAgents dir.
    fn plist(&self) -> PathBuf {
        let name = format!("{}.plist", self.label());
        if self.default_root() {
            self.home.join("Library/LaunchAgents").join(name)
        } else {
            self.root.join(name)
        }
    }
    fn unit(&self) -> PathBuf {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|x| !x.is_empty())
            .map_or_else(|| self.home.join(".config"), PathBuf::from);
        base.join("systemd/user")
            .join(format!("{}.service", self.label()))
    }

    /// Whether the binary is the `aterm-link` shim (a test build) rather than
    /// the front door, which decides the spelling of every command written.
    fn is_link_shim(&self) -> bool {
        Path::new(&self.aterm)
            .file_name()
            .is_some_and(|n| n == "aterm-link")
    }

    /// The bridge command aterm launches: `<aterm> link serve …`, or
    /// `<aterm-link> serve …` for the shim. `--accept-from` carries the node.
    fn bridge_command(&self, node: &str) -> Vec<String> {
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
                &self.sock(),
                "--cap-file",
                &self.cap().to_string_lossy(),
                "--state",
                &self.state().to_string_lossy(),
                "--accept-from",
                node,
            ]
            .iter()
            .map(|s| (*s).to_string()),
        );
        argv
    }

    /// The broker's argv as launchd runs it: the words, no shell.
    fn broker_argv(&self) -> Vec<String> {
        let mut argv = vec![self.aterm.clone()];
        if !self.is_link_shim() {
            argv.push("link".to_string());
        }
        argv.extend(["broker".to_string(), self.sock(), self.log()]);
        argv
    }

    /// The argv `tools/fabric-enable.sh` wrote before the audit, and round
    /// 13's `on` copied byte for byte: a shell that unlinks the socket and
    /// execs the broker. An install carrying it is CURRENT (the broker verb
    /// unlinks a stale socket itself, so the shell did nothing the direct
    /// argv does not) and is not restarted for its spelling.
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
    /// `<string>` each — no `/bin/sh -c`, no quoting grammar — and every value
    /// is XML-escaped, so a `&` or `<` in a path cannot produce a plist launchd
    /// cannot parse (module doc, step 6). An install the script made before
    /// the audit differs in bytes and is read as current all the same
    /// ([`Paths::plist_is_current`]).
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

    /// The `systemd --user` unit.
    #[must_use]
    pub fn unit_text(&self) -> String {
        format!(
            "[Unit]\n\
             Description=aterm fabric broker ({label})\n\
             \n\
             [Service]\n\
             ExecStartPre=/bin/rm -f {sock}\n\
             ExecStart=/bin/sh -c \"exec {broker}\"\n\
             WorkingDirectory={root}\n\
             Restart=always\n\
             RestartSec=1\n\
             \n\
             [Install]\n\
             WantedBy=default.target\n",
            label = self.label(),
            sock = self.sock(),
            broker = self.broker_words(),
            root = self.root.to_string_lossy(),
        )
    }
}

fn default_root(home: &Path) -> PathBuf {
    home.join(".local/share/aterm-fabric")
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
/// the job ran and wrote it, else launchd's own view of the job — the script
/// used to send the operator to a broker.err that was never written.
fn launchd_failure_hint(root: &Path, label: &str, uid: &str) -> String {
    let err = root.join("broker.err");
    if std::fs::metadata(&err).is_ok_and(|m| m.len() > 0) {
        format!("read {}", err.display())
    } else {
        format!(
            "{} was not written, so launchd did not run the broker at all: `launchctl print \
             gui/{uid}/{label}` says why",
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
    let user = Command::new("id")
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
    let expanded = Command::new("sh")
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
            "`{}` contains whitespace, a quote or a backslash; aterm's [fabric] command is \
             split on whitespace with no quoting, so the bridge argv would break — set a \
             different ATERM_FABRIC_HOME / ATERM_BIN / --fleet",
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
}

impl Rendezvous {
    /// Render the file.
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "# Written by `aterm fabric on` — where this machine's fabric is. `aterm link\n\
             # ls|glance|tui|mirror` and `aterm fabric` read it when their flags are omitted;\n\
             # `aterm fabric off` removes it. Not the source of truth: aterm launches its\n\
             # bridge from `[fabric] command` in aterm.toml, which `command` below mirrors.\n\
             fleet = {}\n\
             broker = {}\n\
             node = {}\n\
             cap_file = {}\n\
             state = {}\n\
             command = {}\n",
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
        Ok(Self {
            fleet: get("fleet")?,
            broker: get("broker")?,
            node: get("node")?,
            cap_file: get("cap_file")?,
            state: get("state")?,
            command: get("command")?,
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
/// file for each one that is absent. No file, or an unreadable one, leaves
/// `args` as given (the parser's own "required" refusal then names the flags);
/// an unreadable one is said on stderr first.
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

/// [`with_rendezvous_defaults`]'s pure half: `args` with each of the four
/// flags prepended from `r` when absent.
#[must_use]
pub fn fill_defaults(args: &[String], r: &Rendezvous) -> Vec<String> {
    let has = |flag: &str| args.iter().any(|a| a == flag);
    let mut out = Vec::with_capacity(args.len() + 8);
    for (flag, value) in [
        ("--fleet", &r.fleet),
        ("--broker", &r.broker),
        ("--cap-file", &r.cap_file),
        ("--state", &r.state),
    ] {
        if !has(flag) {
            out.push(flag.to_string());
            out.push(value.clone());
        }
    }
    out.extend_from_slice(args);
    out
}

/// `args` with `--sock <dir>/aterm.sock` filled in for `mirror` when the
/// rendezvous file exists and that socket does — the `latest` alias every
/// flagless `aterm ctl` call dials.
#[must_use]
pub fn with_default_sock(args: &[String]) -> Vec<String> {
    if args.iter().any(|a| a == "--sock") || !matches!(Rendezvous::read(), Ok(Some(_))) {
        return args.to_vec();
    }
    let Some(sock) = aterm_uds::control_socket_dir().map(|d| d.join("aterm.sock")) else {
        return args.to_vec();
    };
    if !sock.exists() {
        return args.to_vec();
    }
    let mut out = vec!["--sock".to_string(), sock.to_string_lossy().into_owned()];
    out.extend_from_slice(args);
    out
}

// ---------------------------------------------------------------------------
// aterm.toml's [fabric] table
// ---------------------------------------------------------------------------

/// The table header on `line` (`[fabric]`, `[fabric.presence]`), comment
/// stripped; `None` for any other line.
fn table_header(line: &str) -> Option<&str> {
    let t = line.split('#').next().unwrap_or("").trim();
    (t.starts_with('[') && t.ends_with(']')).then_some(t)
}

/// The line range `[start, end)` of the table whose header is `header` in
/// `lines`: from it to the next table header or the end.
fn table_span(lines: &[&str], header: &str) -> Option<(usize, usize)> {
    let start = lines.iter().position(|l| table_header(l) == Some(header))?;
    let end = lines[start + 1..]
        .iter()
        .position(|l| l.trim_start().starts_with('['))
        .map_or(lines.len(), |i| start + 1 + i);
    Some((start, end))
}

/// The line range of the `[fabric]` table itself.
fn fabric_table_span(lines: &[&str]) -> Option<(usize, usize)> {
    table_span(lines, "[fabric]")
}

/// Every `fabric` table in `lines` — `[fabric]` and each `[fabric.<sub>]` —
/// as `(header, start, end)`, in file order.
fn fabric_table_spans<'a>(lines: &[&'a str]) -> Vec<(&'a str, usize, usize)> {
    let mut out = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let Some(h) = table_header(l) else { continue };
        if h != "[fabric]" && !(h.starts_with("[fabric.") && h.len() > "[fabric.]".len()) {
            continue;
        }
        let end = lines[i + 1..]
            .iter()
            .position(|l| l.trim_start().starts_with('['))
            .map_or(lines.len(), |n| i + 1 + n);
        out.push((h, i, end));
    }
    out
}

/// Whether `line` starts the `command` key of a table.
fn is_command_line(line: &str) -> bool {
    let t = line.trim_start();
    t.strip_prefix("command")
        .is_some_and(|rest| rest.trim_start().starts_with('='))
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
/// at all: round 15's receipts (R8) are ON in the config this command writes,
/// so a fresh `aterm fabric on` acks senders out of the box. A key the
/// operator has set — either way — is never touched, which is what makes
/// `receipts = false` a setting rather than a race with the next `on`.
///
/// # Errors
///
/// A file that is not valid TOML (it is left alone, and the error named), or a
/// `fabric` written as a dotted key or an inline table at the top level — this
/// editor only knows the `[fabric]` table form, and appending a second
/// definition would make the file invalid TOML; the message says to edit it.
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
    let table = fabric_table_span(&lines);
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
    Ok((joined, true))
}

/// Whether the top level spells `fabric` as `fabric.command = …` or
/// `fabric = { … }` — forms [`set_fabric_command`] does not edit.
fn has_dotted_fabric(lines: &[&str]) -> bool {
    let mut in_table = false;
    for l in lines {
        let t = l.trim_start();
        if t.starts_with('[') {
            in_table = true;
            continue;
        }
        if in_table {
            continue;
        }
        if t.starts_with("fabric.")
            || t.strip_prefix("fabric")
                .is_some_and(|r| r.trim_start().starts_with('='))
        {
            return true;
        }
    }
    false
}

/// `text` with the `[fabric]` table AND every `[fabric.<sub>]` table removed,
/// every other table kept, and the headers removed in file order — empty when
/// there was none. A subtable left behind would re-create the `fabric` key
/// (the audit's finding on the script's `--disable`).
#[must_use]
pub fn remove_fabric_table(text: &str) -> (String, Vec<String>) {
    let lines: Vec<&str> = text.lines().collect();
    let spans = fabric_table_spans(&lines);
    if spans.is_empty() {
        return (text.to_string(), Vec::new());
    }
    let removed: Vec<String> = spans.iter().map(|(h, _, _)| (*h).to_string()).collect();
    let mut out: Vec<&str> = Vec::with_capacity(lines.len());
    let mut at = 0;
    for (_, start, end) in spans {
        out.extend_from_slice(&lines[at..start]);
        at = end;
    }
    out.extend_from_slice(&lines[at..]);
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    if out.is_empty() {
        return (String::new(), removed);
    }
    let mut joined = out.join("\n");
    joined.push('\n');
    (joined, removed)
}

// ---------------------------------------------------------------------------
// files, written the careful way
// ---------------------------------------------------------------------------

/// Write `bytes` to `path` with `mode`: a temp beside it, fsync, rename.
fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let result = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        // `create` with a mode is masked by the umask; the rename keeps
        // whatever the temp got, so say the mode again explicitly.
        std::fs::set_permissions(&tmp, std::os::unix::fs::PermissionsExt::from_mode(mode))?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Replace `path`'s text, keeping what was there as `<path>.bak` (only when
/// something was there). An existing file keeps its own mode — an operator's
/// 0644 aterm.toml is not silently made private — and a new one gets `mode`.
fn write_with_backup(path: &Path, text: &str, mode: u32) -> io::Result<Option<PathBuf>> {
    use std::os::unix::fs::PermissionsExt;
    let (bak, mode) = match std::fs::read(path) {
        Ok(old) => {
            let mode = std::fs::metadata(path).map_or(mode, |m| m.permissions().mode() & 0o777);
            let bak = PathBuf::from(format!("{}.bak", path.display()));
            write_atomic(&bak, &old, mode)?;
            (Some(bak), mode)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => (None, mode),
        Err(e) => return Err(e),
    };
    write_atomic(path, text.as_bytes(), mode)?;
    Ok(bak)
}

/// `n` random bytes through the ONE audited entropy helper
/// (`aterm_uds::rand`): a hand-rolled device read is the incident the B4 guard
/// exists for.
fn random_bytes(n: usize) -> io::Result<Vec<u8>> {
    let mut buf = vec![0u8; n];
    aterm_uds::rand::fill(&mut buf)?;
    Ok(buf)
}

fn read_node(state: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(state.join("node")).ok()?;
    let node = raw.trim();
    (node.starts_with("n-") && crate::subject::is_principal(node)).then(|| node.to_string())
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
    if let Some(path) = std::env::var_os(TRACE_ENV) {
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
    let fail_at: Option<usize> = std::env::var(FAIL_MINT_ENV)
        .ok()
        .and_then(|v| v.parse().ok());
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
                    let tag: String = c.tag.iter().map(|b| format!("{b:02x}")).collect();
                    lines.push_str(&format!("{} {tag}\n", c.filter));
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
struct Out {
    dry_run: bool,
    changed: usize,
    failed: bool,
}

impl Out {
    fn new(dry_run: bool) -> Self {
        Self {
            dry_run,
            changed: 0,
            failed: false,
        }
    }

    fn line(&self, name: &str, verdict: &str, detail: &str) {
        let mut out = io::stdout().lock();
        let _ = writeln!(out, "  {name:<11} {verdict:<9} {detail}");
        let _ = out.flush();
    }

    /// Nothing to do: it was already so.
    fn already(&self, name: &str, detail: &str) {
        self.line(name, "already", detail);
    }

    /// Something was (or, dry, would be) done. An empty `did` prints `would`
    /// — the step's description — rather than an empty line (a `cap done`
    /// with nothing after it was the review's reading of it).
    fn done(&mut self, name: &str, would: &str, did: &str) {
        self.changed += 1;
        if self.dry_run {
            self.line(name, "would", would);
        } else {
            self.line(name, "done", if did.is_empty() { would } else { did });
        }
    }

    /// Something the operator has to know that is not this run's failure.
    fn warn(&self, name: &str, detail: &str) {
        self.line(name, "WARNING", detail);
    }

    fn ok(&self, name: &str, detail: &str) {
        self.line(name, "ok", detail);
    }

    fn fail(&mut self, name: &str, detail: &str) {
        self.failed = true;
        self.line(name, "FAILED", detail);
    }

    fn note(&self, name: &str, detail: &str) {
        self.line(name, "-", detail);
    }
}

/// Whether the binary carries `link` — `<aterm> link` prints the bridge's
/// usage, which names `aterm-link`; the shim prints it with no argument.
fn binary_has_link(p: &Paths) -> Result<(), String> {
    if !is_executable(Path::new(&p.aterm)) {
        return Err(format!(
            "no executable at {} (set ATERM_BIN, or install aterm)",
            safe(&p.aterm, 256)
        ));
    }
    let mut cmd = Command::new(&p.aterm);
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
    if text.contains("aterm-link") {
        Ok(())
    } else {
        Err(format!(
            "{} has no `link` verb — it predates the in-tree fabric bridge; update aterm",
            safe(&p.aterm, 256)
        ))
    }
}

/// Reach the broker for real, through the report's own probe.
fn probe(p: &Paths, node: &str) -> fabric::BrokerView {
    let cmd = p.bridge_command(node).join(" ");
    match fabric::bridge_config(&cmd) {
        Ok(cfg) => fabric::probe_broker(&cfg, &fabric::process_table()).0,
        Err(e) => fabric::BrokerView {
            endpoint: p.sock(),
            transport: "unix",
            error: Some(e),
            ..fabric::BrokerView::default()
        },
    }
}

/// Wait for the socket file to appear, then for the broker behind it to answer.
fn wait_for_broker(p: &Paths, node: &str) -> Result<fabric::BrokerView, String> {
    let started = Instant::now();
    loop {
        let view = probe(p, node);
        if view.read_ok() {
            return Ok(view);
        }
        if started.elapsed() > BIND_DEADLINE {
            return Err(view.error.unwrap_or_else(|| "no answer".to_string()));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn uid() -> String {
    Command::new("id")
        .arg("-u")
        .stdin(Stdio::null())
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "501".to_string())
}

/// `launchctl <args>`; `Ok(())` on exit 0.
fn launchctl(args: &[&str]) -> Result<(), String> {
    let out = Command::new("launchctl")
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

/// The pid launchd reports for `label`, if the job is loaded and running.
fn launchd_pid(label: &str) -> Option<u32> {
    let out = Command::new("launchctl")
        .arg("list")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|line| {
            let mut cols = line.split_whitespace();
            let pid = cols.next()?;
            let _status = cols.next()?;
            (cols.next()? == label).then(|| pid.parse().ok()).flatten()
        })
}

fn launchd_loaded(label: &str) -> bool {
    let out = Command::new("launchctl")
        .arg("list")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    out.is_ok_and(|o| {
        String::from_utf8_lossy(&o.stdout)
            .lines()
            .any(|l| l.split_whitespace().nth(2) == Some(label))
    })
}

fn systemctl(args: &[&str]) -> Result<(), String> {
    let out = Command::new("systemctl")
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
    let sock = p.sock();
    let label = p.label();
    match service {
        Service::None => {
            let view = probe(p, node);
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
                         `{} {}broker {sock} {}` yourself, or drop --service none",
                        safe(view.error.as_deref().unwrap_or("-"), 256),
                        p.aterm,
                        if p.is_link_shim() { "" } else { "link " },
                        p.log()
                    ),
                );
                false
            }
        }
        Service::Launchd => {
            let plist = p.plist();
            let text = p.plist_text();
            let same = std::fs::read_to_string(&plist).is_ok_and(|old| p.plist_is_current(&old));
            let running = launchd_pid(&label);
            let view = probe(p, node);
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
            // A BROKER LAUNCHD DOES NOT MANAGE. Something answers on the
            // socket and launchd has no job by this label: a hand-run
            // `aterm link broker`, or a job under another label. Bootstrapping
            // over it would start a second broker on the same socket path AND
            // the same bus log — and the old step then claimed it as `launchd
            // … pid 0`.
            if running.is_none() && view.read_ok() {
                out.fail(
                    "broker",
                    &format!(
                        "a broker launchd does not manage answers on {sock} (pid {}): launchd \
                         {label} is not loaded, and bootstrapping it would start a second broker \
                         on that socket and on {} — stop that broker and run `on` again, or run \
                         `on --service none` to keep it as it is",
                        view.pid
                            .map_or_else(|| "-".to_string(), |pid| pid.to_string()),
                        p.log()
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
                            launchd_pid(&label)
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
            let view = probe(p, node);
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
            // `enable` then `restart` (not `enable --now` then `restart`, which
            // started the unit twice): restart starts a stopped unit too.
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

/// One running instance, as `on` sees it.
struct Instance {
    pid: u32,
    sock: String,
    token: String,
    supervised: bool,
    state: String,
    command: Option<String>,
}

/// Every instance the rendezvous walk lists that answered `fabric status`.
fn instances(out: &mut Out) -> Vec<Instance> {
    let listed = match aterm_ctl::local_instances() {
        Ok(l) => l,
        Err(e) => {
            out.note(
                "instances",
                &format!("cannot be listed ({}); none armed", safe(&e.reason, 256)),
            );
            return Vec::new();
        }
    };
    let mut found = Vec::new();
    for (pid, sock) in listed {
        let token = match aterm_ctl::instance_token(&sock) {
            Ok(t) => t,
            Err(e) => {
                if aterm_uds::process::pid_alive(pid) {
                    out.note(
                        "instance",
                        &format!(
                            "{pid}: no Owner token ({}); not armed",
                            safe(&e.to_string(), 128)
                        ),
                    );
                }
                continue;
            }
        };
        let mut ctl = match Ctl::connect(&sock, &token) {
            Ok(c) => c,
            Err(e) => {
                if aterm_uds::process::pid_alive(pid) {
                    out.note(
                        "instance",
                        &format!(
                            "{pid}: {} does not answer ({e}); not armed",
                            safe(&sock, 256)
                        ),
                    );
                }
                continue;
            }
        };
        let _ = ctl.get_ref().set_read_timeout(Some(IO_TIMEOUT));
        let _ = ctl.get_ref().set_write_timeout(Some(IO_TIMEOUT));
        let reply = match ctl.request("fabric status") {
            Ok(r) if r.ok() => r,
            Ok(r) => {
                out.note(
                    "instance",
                    &format!("{pid}: fabric status answered {}", safe(r.header(), 128)),
                );
                continue;
            }
            Err(e) => {
                out.note("instance", &format!("{pid}: fabric status: {e}"));
                continue;
            }
        };
        let h = reply.header();
        found.push(Instance {
            pid,
            sock,
            token,
            supervised: kv(h, "supervised") == Some("1"),
            state: kv(h, "state").unwrap_or("-").to_string(),
            command: kv(h, "command")
                .filter(|c| *c != "-")
                .map(crate::pct::decode),
        });
    }
    found
}

/// Step 9: arm every running instance. Answers the ones that carry this
/// command (armed now or before), in the order they were listed.
fn arm_instances(p: &Paths, node: &str, out: &mut Out) -> Vec<Instance> {
    let argv = p.bridge_command(node);
    let ours = fabric::serve_flags(&argv.join(" "));
    let found = instances(out);
    if found.is_empty() {
        out.note(
            "instances",
            "none running (a later launch reads [fabric] command itself)",
        );
        return Vec::new();
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
                        "{pid} is armed with a DIFFERENT command ({}): a supervisor arms once \
                         per process, so only a relaunch of that aterm changes it",
                        safe(inst.command.as_deref().unwrap_or("-"), 256)
                    ),
                );
            }
            continue;
        }
        out.done(
            "instance",
            &format!(
                "arm {pid} (`aterm ctl --pid {pid} fabric attach {}`)",
                argv.join(" ")
            ),
            &format!("armed {pid}"),
        );
        if out.dry_run {
            continue;
        }
        let Ok(mut ctl) = Ctl::connect(&inst.sock, &inst.token) else {
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
            // Attached, and the bridge has said nothing about its link: a
            // bridge from before the link report (0.85 and earlier) never
            // does, and its instance reads `connected` at the first record it
            // delivers. The proof is that record, so the proof decides.
            out.note(
                "instance",
                &format!(
                    "{pid} armed; its bridge attached and has not reported its broker link after \
                     {ARM_DEADLINE:?} (a bridge older than the link report never does) — the \
                     proof decides"
                ),
            );
            inst.supervised = true;
            inst.state = state;
            armed.push(inst);
        } else {
            out.fail(
                "instance",
                &format!(
                    "{pid} armed but fabric={} after {ARM_DEADLINE:?}: the bridge did not attach \
                     (or attached and its broker link is down — `stalled`, with the reason on \
                     `aterm ctl --pid {pid} fabric status`); the instance's log says which",
                    safe(&state, 16)
                ),
            );
        }
    }
    armed
}

/// Every `(instance pid, sid)` whose `status` says `hold=1`, across the
/// instances the rendezvous walk lists. An instance that cannot be asked is
/// skipped silently: `off` names what it can see, and the `instances` line at
/// its end says what is running.
fn held_sessions() -> Vec<(u32, String)> {
    let mut held = Vec::new();
    for (pid, sock) in aterm_ctl::local_instances().unwrap_or_default() {
        let Ok(token) = aterm_ctl::instance_token(&sock) else {
            continue;
        };
        let Ok(mut ctl) = Ctl::connect(&sock, &token) else {
            continue;
        };
        let _ = ctl.get_ref().set_read_timeout(Some(IO_TIMEOUT));
        let _ = ctl.get_ref().set_write_timeout(Some(IO_TIMEOUT));
        let sids: Vec<String> = ctl
            .request("sessions")
            .ok()
            .filter(crate::ctl::Reply::ok)
            .map(|r| {
                r.rows()
                    .iter()
                    .filter_map(|row| {
                        row.split_whitespace()
                            .nth(1)
                            .filter(|s| s.starts_with("s-") && crate::subject::is_principal(s))
                            .map(str::to_string)
                    })
                    .collect()
            })
            .unwrap_or_default();
        for sid in sids {
            let is_held = ctl
                .request(&format!("@{sid} status"))
                .ok()
                .filter(crate::ctl::Reply::ok)
                .is_some_and(|r| kv(r.header(), "hold") == Some("1"));
            if is_held {
                held.push((pid, sid));
            }
        }
    }
    held
}

/// The sessions one instance hosts, in its own order.
fn sessions_of(inst: &Instance) -> Vec<String> {
    let Ok(mut ctl) = Ctl::connect(&inst.sock, &inst.token) else {
        return Vec::new();
    };
    let _ = ctl.get_ref().set_read_timeout(Some(IO_TIMEOUT));
    ctl.request("sessions")
        .ok()
        .filter(crate::ctl::Reply::ok)
        .map(|r| {
            r.rows()
                .iter()
                .filter_map(|row| {
                    row.split_whitespace()
                        .nth(1)
                        .filter(|s| s.starts_with("s-") && crate::subject::is_principal(s))
                        .map(str::to_string)
                })
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
    let mut ctl =
        Ctl::connect(&inst.sock, &inst.token).map_err(|e| format!("{}: {e}", inst.sock))?;
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
            "the post did not land on the bus: {e} — the bridge is attached but the broker did \
             not acknowledge; `aterm fabric` says whether the broker answers"
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
            "landed at @{off} but was not delivered back within {PROOF_DEADLINE:?}: {e} — the \
             bridge publishes but does not read its own node's inbox lane; check the cap file's \
             `ro:/f/<F>/in/<node>/>` grant and `--accept-from`"
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
            "landed at @{off} and something was delivered, but not that record — another \
             sender's note arrived first; run `on` again"
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

/// `aterm fabric on`.
#[must_use]
pub fn on(opts: &OnOpts) -> ExitCode {
    if opts.tcp.is_some() || opts.key_file.is_some() {
        eprintln!(
            "aterm fabric on: --tcp/--key-file are refused in this build: `aterm link broker` \
             serves the Unix socket only, and the sealed TCP listener is behind astream's `aead` \
             feature, which a default build (and the shipped binary) does not carry — so the \
             flags would be written into a config the bridge could not dial. Round 16 adds the \
             listener; until then the fabric is one host over the Unix socket."
        );
        return ExitCode::from(2);
    }
    let p = match Paths::resolve(opts.fleet.as_deref()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("aterm fabric on: {e}");
            return ExitCode::from(2);
        }
    };
    let service = opts.service.unwrap_or_else(Service::default_here);
    let mut out = Out::new(opts.dry_run);
    println!(
        "aterm fabric on — fleet {} · root {} · service {}{}",
        safe(&p.fleet, 64),
        p.root.display(),
        service.name(),
        if opts.dry_run {
            " · DRY RUN: every step printed, nothing touched"
        } else {
            ""
        }
    );

    // The refusals that must come before anything is written.
    let sock = p.sock();
    if sock.len() >= SUN_PATH_MAX {
        out.fail(
            "socket",
            &format!(
                "{sock} is {} bytes and sun_path holds under {SUN_PATH_MAX} — the broker cannot \
                 bind it; set a shorter ATERM_FABRIC_HOME",
                sock.len()
            ),
        );
        return ExitCode::from(2);
    }
    let state = p.state();
    for word in [
        p.aterm.as_str(),
        p.fleet.as_str(),
        sock.as_str(),
        &p.cap().to_string_lossy(),
        &state.to_string_lossy(),
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
                "{} ({}serve + {}broker)",
                p.aterm,
                if p.is_link_shim() { "" } else { "link " },
                if p.is_link_shim() { "" } else { "link " }
            ),
        ),
        Err(e) => {
            out.fail("binary", &e);
            return ExitCode::from(2);
        }
    }

    // 2. the root
    if p.root.is_dir() && state.is_dir() {
        out.already("root", &format!("{} (0700)", p.root.display()));
    } else {
        out.done(
            "root",
            &format!("create {} and {} (0700)", p.root.display(), state.display()),
            &format!("created {} (0700)", p.root.display()),
        );
        if !opts.dry_run {
            if let Err(e) = std::fs::create_dir_all(&state).and_then(|()| {
                for d in [&p.root, &state] {
                    std::fs::set_permissions(
                        d,
                        std::os::unix::fs::PermissionsExt::from_mode(0o700),
                    )?;
                }
                Ok(())
            }) {
                out.fail("root", &format!("{}: {e}", state.display()));
                return ExitCode::from(2);
            }
        }
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
            if !opts.dry_run {
                let write =
                    crate::state::StateDir::open(&state).and_then(|s| s.node_id(|| minted.clone()));
                if let Err(e) = write {
                    out.fail("node", &format!("{}/node: {e}", state.display()));
                    return ExitCode::from(2);
                }
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
                    "{} holds {} bytes; a mint secret is at least {SECRET_LEN} — it is never \
                     replaced silently: move it aside and run again",
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
    if !broker_step(&p, &node, service, &mut out) && !opts.dry_run {
        return ExitCode::FAILURE;
    }

    // 7. aterm.toml
    let argv = p.bridge_command(&node);
    let command = argv.join(" ");
    let Some(config) = p.config.clone() else {
        out.fail(
            "config",
            "no config path: neither $XDG_CONFIG_HOME nor $HOME is set",
        );
        return ExitCode::FAILURE;
    };
    let text = match std::fs::read_to_string(&config) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            out.fail("config", &format!("{}: {e}", config.display()));
            return ExitCode::FAILURE;
        }
    };
    match set_fabric_command(&text, &command) {
        Ok((_, false)) => out.already(
            "config",
            &format!("[fabric] command in {} is this command", config.display()),
        ),
        Ok((new_text, true)) => {
            let (verb, did) = if text.contains("[fabric]") {
                ("update", "updated")
            } else {
                ("add", "added")
            };
            out.done(
                "config",
                &format!(
                    "{verb} [fabric] command in {} (previous saved as .bak)",
                    config.display()
                ),
                "",
            );
            if !opts.dry_run {
                match write_with_backup(&config, &new_text, 0o600) {
                    Ok(bak) => out.ok(
                        "config",
                        &format!(
                            "{did} [fabric] command in {}{}",
                            config.display(),
                            bak.map_or_else(String::new, |b| format!(
                                " (previous saved as {})",
                                b.display()
                            ))
                        ),
                    ),
                    Err(e) => {
                        out.fail("config", &format!("{}: {e}", config.display()));
                        return ExitCode::FAILURE;
                    }
                }
            }
        }
        Err(e) => {
            out.fail("config", &format!("{}: {e}", config.display()));
            return ExitCode::FAILURE;
        }
    }

    // 8. the rendezvous file
    let rendezvous = Rendezvous {
        fleet: p.fleet.clone(),
        broker: sock.clone(),
        node: node.clone(),
        cap_file: cap.to_string_lossy().into_owned(),
        state: state.to_string_lossy().into_owned(),
        command: command.clone(),
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
                if !opts.dry_run {
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
    let armed = arm_instances(&p, &node, &mut out);

    // 10. the proof
    let proof_failed = if armed.is_empty() {
        out.note(
            "proof",
            "skipped: no armed instance hosts a session to post through — launch aterm and run \
             `aterm fabric doctor`",
        );
        false
    } else {
        match prove(&armed, &mut out) {
            Ok(_) => false,
            Err(e) => {
                out.fail("proof", &e);
                true
            }
        }
    };

    // 11. the undo, scoped like this run was.
    if !opts.dry_run {
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
            ("ATERM_FABRIC_HOME", env("ATERM_FABRIC_HOME")),
            ("XDG_CONFIG_HOME", env("XDG_CONFIG_HOME")),
            ("XDG_RUNTIME_DIR", env("XDG_RUNTIME_DIR")),
            ("ATERM_BIN", env("ATERM_BIN")),
            ("HOME", home),
        ];
        let scoped = scope
            .iter()
            .any(|(_, v)| v.as_deref().is_some_and(|v| !v.is_empty()));
        out.note(
            "undo",
            &format!(
                "{}{}",
                undo_command(program, &scope),
                if scoped {
                    " (with the environment that scoped this run: a bare `off` acts on the \
                     config and root it finds without it)"
                } else {
                    ""
                }
            ),
        );
    }

    if opts.dry_run {
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
        println!("\nnothing changed: the fabric was already on, and the proof ran again.");
    }

    // 11. status. THE EXIT CODE IS THIS RUN'S: the proof and the steps. The
    // status's own warnings are printed with it, and a fabric that turned on
    // and proved itself is not a failed `on` because a session has unread
    // mail (the live machine had three such warnings; `on` exited 1 on them).
    println!();
    match fabric::gather() {
        Ok(report) => {
            print!("{}", fabric::render_text(&report));
            if proof_failed || out.failed {
                ExitCode::FAILURE
            } else {
                if !report.warnings.is_empty() {
                    println!(
                        "\n{} warning(s) above: none is this run's failure — `aterm fabric \
                         doctor` names the fix for each",
                        report.warnings.len()
                    );
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

// ---------------------------------------------------------------------------
// off
// ---------------------------------------------------------------------------

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
    for (pid, sid) in held_sessions() {
        out.warn(
            "held",
            &format!(
                "@{sid} (instance {pid}) is HELD (status hold=1) and off does not lift it: a \
                 fleet hold lifts only when a bridge reads it withdrawn on the bus, which no \
                 bridge will once the broker is stopped, and the session answers `ERR halted` \
                 until its instance is relaunched — withdraw the halt first (the human who \
                 set it: `aterm link` halt off, or `aterm ctl @{sid} hold off` for a local \
                 one), or relaunch instance {pid} after this"
            ),
        );
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
            &format!(
                "--service none: whatever runs `link broker {}` is yours to stop",
                p.sock()
            ),
        ),
    }

    match p.config.clone() {
        None => out.note(
            "config",
            "no config path: neither $XDG_CONFIG_HOME nor $HOME is set",
        ),
        Some(config) => match std::fs::read_to_string(&config) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                out.already("config", &format!("{} does not exist", config.display()));
            }
            Err(e) => out.fail("config", &format!("{}: {e}", config.display())),
            Ok(text) => {
                let (new_text, removed) = remove_fabric_table(&text);
                if removed.is_empty() {
                    out.already(
                        "config",
                        &format!("no [fabric] table in {}", config.display()),
                    );
                } else {
                    let headers = removed.join(", ");
                    out.done(
                        "config",
                        &format!(
                            "remove {headers} from {} (previous saved as .bak)",
                            config.display()
                        ),
                        "",
                    );
                    if !opts.dry_run {
                        match write_with_backup(&config, &new_text, 0o600) {
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
                            Err(e) => out.fail("config", &format!("{}: {e}", config.display())),
                        }
                    }
                }
            }
        },
    }

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

    out.note(
        "kept",
        &format!(
            "{}: node id{}, mint.secret and node.cap — identity is provisioned, never discarded",
            p.root.display(),
            read_node(&p.state()).map_or_else(String::new, |n| format!(" {n}"))
        ),
    );
    out.note(
        "bus log",
        &format!(
            "{} stays; delete it yourself if you want the log gone",
            p.log()
        ),
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
                "{} running ({}) keep their bridge until relaunched — a supervisor has no stop \
                 handle — and the next launch starts none",
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
        "start the broker: `aterm fabric on` installs and (re)starts it under launchd / systemd \
         --user and probes it; if the job is installed and flapping, read <root>/broker.err \
         (a stale socket file, or a socket path over sun_path's 104 bytes)"
            .to_string()
    } else if s("has not reported its broker link") {
        "nothing, if its mail moves: a bridge older than the link report (0.85 and earlier) \
         never sends one and reads connected at its first delivery — `aterm ctl @<sid> post \
         to=@<sid> kind=note --wait=5000 ping` proves it either way; a bridge that is new enough \
         and still says nothing cannot reach the broker, so check it (`aterm fabric on`)"
            .to_string()
    } else if s("broker link is down") {
        "the bridge is alive and cannot reach the broker (reason= says how the dial or the last \
         exchange failed: no-socket / refused = nothing listens at --broker, `aterm fabric on` \
         starts it; no-ack = it accepts and never answers, restart the job; attach / denied = the \
         cap or the socket's permissions): fix the broker and the bridge reconnects on its own \
         within its 5 s back-off — nothing to restart on the aterm side"
            .to_string()
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
        "`aterm fabric on` arms every running instance (`aterm ctl --pid <pid> fabric attach \
         <command>` is the verb it uses)"
            .to_string()
    } else if s("bridge is down") {
        "aterm relaunches its bridge with back-off; if it stays down, the bridge cannot start — \
         run the [fabric] command by hand to see its error, then `aterm fabric on`"
            .to_string()
    } else if s("armed with a different command") {
        "relaunch that aterm instance: a supervisor arms once per process and reads [fabric] \
         command at launch, so nothing else changes what it runs"
            .to_string()
    } else if s("ONE state dir") {
        "give each instance its own --state dir (each is then its own node with its own cap), or \
         run one instance"
            .to_string()
    } else if s("state=gone") {
        "the node's presence on the bus is stale: kill that instance's bridge (`kill <bridge pid>` \
         from the BRIDGES row) — aterm relaunches it and the new incarnation publishes state=live. \
         Know what that costs: every session the bridge governed is held `fabric-lost` from the \
         kill until the relaunch reads the fleet's halt (about 200 ms), and a driver typing into \
         one in that window is refused `ERR halted`"
            .to_string()
    } else if s("is HELD") {
        "a local hold lifts with `aterm ctl hold <sid> off`; a fleet hold lifts when a bridge \
         reconnects (reason=fabric-lost) or the human who set it withdraws it"
            .to_string()
    } else if s("lost") && s("evicted") {
        "read mail sooner (`aterm ctl @<sid> await inbox`) and `inbox seen` what is handled; the \
         evicted records are still on the bus"
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
        "a presence row from an instance that is gone: it clears when a bridge of that node \
         republishes; relaunch the instance that hosted the session, or ignore it"
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

/// `aterm fabric doctor`.
#[must_use]
pub fn doctor() -> ExitCode {
    match fabric::gather() {
        Err(off) => {
            println!("{}", off.message());
            println!("  fix: `aterm fabric on` (`--dry-run` first shows every step)");
            ExitCode::from(2)
        }
        Ok(report) => {
            let mut warnings = report.warnings.clone();
            match Rendezvous::read() {
                Ok(Some(_)) => {}
                Ok(None) => warnings.push(format!(
                    "no rendezvous file at {}: `aterm link ls|glance|tui` need their flags \
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
                println!(
                    "  no warnings: the broker answers, every bridge is supervised and connected, \
                     no mail is dropped, held or waiting, and the rendezvous file is there"
                );
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
        };
        assert_eq!(p("/Users//example/.local/share/aterm-fabric").label(), LABEL);
        assert_eq!(
            p("/tmp/atfo-x/root").label(),
            format!("{LABEL}.{}", cksum(b"/tmp/atfo-x/root"))
        );
        assert_ne!(p("/tmp/atfo-x/root").label(), LABEL);
    }

    /// TWO ROOTS NEVER SHARE A LABEL — the round-13 review's finding 2. The
    /// plain label belongs to the LOGIN user's default root; a redirected
    /// `$HOME` whose root is `$HOME/.local/share/aterm-fabric` used to read as
    /// "the default root" and carry the live label, so `HOME=/tmp/x aterm
    /// fabric on` booted the machine's real broker out. An unknown login home
    /// makes every root derive its own.
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
    /// script wrote before the audit (the plist this machine's live install
    /// carries, read on 2026-09-14) is CURRENT all the same, so `on` does not
    /// restart a healthy broker for its spelling; a foreign argv is not.
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
        // The script's form, byte for byte what the live machine's
        // ~/Library/LaunchAgents/systems.alab.astream-broker.plist holds.
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
    /// `~/Library/LaunchAgents`, any other root's inside that root — so a
    /// redirected run cannot leave a persistent job in the operator's
    /// LaunchAgents dir (the audit's finding on the script).
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
    /// broker.err the job wrote, else launchd's own view of the job — the
    /// script used to send the operator to a broker.err that was never written.
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
    /// when the table has no such key (round 15, R8) — appends a table when
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
            format!("font_px = 12.0\n\n[fabric]\ncommand = \"{cmd}\"\nreceipts = true\n# a comment\npresence = \"meta\"\n\n[keys]\nx = 1\n")
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

        // REMOVAL drops the table and nothing else — the regex the script had
        // once swallowed every table after it.
        let (after, removed) = remove_fabric_table(before);
        assert_eq!(removed, ["[fabric]"]);
        assert_eq!(after, "font_px = 12.0\n\n[keys]\nx = 1\n");
        assert!(remove_fabric_table(&after).1.is_empty());
        assert_eq!(
            remove_fabric_table("[fabric]\ncommand = \"x\"\n"),
            (String::new(), vec!["[fabric]".to_string()])
        );
        let (after, _) = remove_fabric_table("[fabric]\ncommand = \"x\"\n[keys]\nx = 1\n");
        assert_eq!(after, "[keys]\nx = 1\n");
        // AND EVERY `[fabric.<sub>]` TABLE, wherever it sits: a subtable left
        // behind re-creates the `fabric` key (the audit's finding on the
        // script's --disable). Each header removed is named, in file order.
        let subs = "[fabric.presence]\nx = 1\n[a]\ny = 2\n[fabric] # on\ncommand = \"x\"\n[fabric.more]\nz = 3\n[keys]\nk = 1\n";
        let (after, removed) = remove_fabric_table(subs);
        assert_eq!(removed, ["[fabric.presence]", "[fabric]", "[fabric.more]"]);
        assert_eq!(after, "[a]\ny = 2\n[keys]\nk = 1\n");
        assert!(fabric::command_in_toml(&after).expect("valid").is_none());
        // A subtable alone is a fabric table too; `[fabrics]` is not.
        assert_eq!(
            remove_fabric_table("[fabric.presence]\nx = 1\n"),
            (String::new(), vec!["[fabric.presence]".to_string()])
        );
        assert!(remove_fabric_table("[fabrics]\nx = 1\n").1.is_empty());
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
}
