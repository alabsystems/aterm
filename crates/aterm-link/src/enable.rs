// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm fabric on|off|doctor`: provision, tear down, or diagnose the local fabric.

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

/// The launchd label (and systemd unit name) of the broker under the default root.
pub const LABEL: &str = "systems.alab.astream-broker";

/// `sockaddr_un.sun_path` on macOS.
pub const SUN_PATH_MAX: usize = 104;

/// The mint secret's name in the fabric root, and beside the socket.
pub const MINT_SECRET: &str = "mint.secret";

/// A mint secret is 32 bytes.
pub use astream_cap::capfile::SECRET_MIN as SECRET_LEN;

/// The record `aterm fabric off` leaves in the fabric root, empty, and beside aterm.toml
/// ([`kept_off_record`]): the person turned the fabric off, and the converge pass never turns it
/// on beside either ([`unset_state`]); `aterm fabric on` removes both.
pub const OFF_RECORD: &str = "fabric-off";

/// [`OFF_RECORD`] beside aterm.toml, outside the fabric root: an off keeps the root (the node id,
/// the secret, the grants, the bus log), so a person who tidies it away after has not turned the
/// fabric back on. `None` where no config path resolves.
fn kept_off_record(p: &Paths) -> Option<PathBuf> {
    p.config
        .as_deref()
        .and_then(Path::parent)
        .map(|dir| dir.join(OFF_RECORD))
}

/// The off record that stands, in `root` or at `kept` ([`kept_off_record`]).
fn off_recorded(root: &Path, kept: Option<&Path>) -> Option<PathBuf> {
    std::iter::once(root.join(OFF_RECORD))
        .chain(kept.map(Path::to_path_buf))
        .find(|record| std::fs::symlink_metadata(record).is_ok())
}

/// The record the converge pass leaves in the fabric root before it turns the fabric on by
/// default ([`turn_on_by_default`]), empty: a root it provisioned and did not finish (a broker
/// that did not start, say) reads as never turned off, so a later pass finishes it, where a root
/// an older `off` left reads as the person's off. The turn-on that finishes takes it out, the
/// pass's or a person's `aterm fabric on`, so a command the person clears after reads as their
/// off.
pub const DEFAULT_RECORD: &str = "fabric-by-default";

/// The record `aterm fabric on --service none` leaves in the fabric root: the person runs the
/// broker, so the converge pass renews no job an earlier `on` installed there. An `on` that hands
/// the broker to launchd or systemd takes it out.
pub const HAND_RECORD: &str = "fabric-service-none";

/// The lock in the fabric root that `aterm fabric on`, `aterm fabric off`, the converge pass's
/// turn-on and its broker renewal hold, one at a time: an off never lands midway through a turn-on
/// (whose steps remove its record and write `[fabric] command` back over it) or a renewal (whose
/// step would start the broker the off stopped), and two turn-ons never write the root at once.
const SWITCH_LOCK: &str = "fabric-switch.lock";

/// How long a person's `aterm fabric on` or `off` waits for the switch: a hang bound, well past
/// the broker step's own waits.
const SWITCH_WAIT: Duration = Duration::from_secs(300);

/// Take [`SWITCH_LOCK`] in `root` (made 0700 where it is missing), waiting at most `within`; a
/// lock still held then is `WouldBlock`.
fn switch_lock(root: &Path, within: Duration) -> io::Result<aterm_uds::private_file::PrivateLock> {
    aterm_uds::private_file::ensure_private_dir(root)?;
    aterm_uds::private_file::PrivateLock::take(root, SWITCH_LOCK, within, "held")
}

/// [`SWITCH_LOCK`] in `root` for a person's `aterm fabric {verb}`, waiting at most `within` for a
/// turn-on, renewal, `on` or `off` that holds it; `None` for a dry run, or where the lock cannot
/// be taken (said on `out`, and the command goes on without it). A switch still held then fails
/// the command, its line naming what to do.
fn persons_switch(
    root: &Path,
    verb: &str,
    within: Duration,
    out: &mut Out,
) -> Result<Option<aterm_uds::private_file::PrivateLock>, ExitCode> {
    if out.dry_run {
        return Ok(None);
    }
    match switch_lock(root, within) {
        Ok(lock) => Ok(Some(lock)),
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
            out.fail(
                "record",
                &format!(
                    "{} was held for {}s (the converge pass turning the fabric on or renewing its \
                     broker, or another `aterm fabric on` or `off`): run `aterm fabric {verb}` \
                     again once it ends",
                    root.join(SWITCH_LOCK).display(),
                    within.as_secs()
                ),
            );
            Err(ExitCode::FAILURE)
        }
        Err(e) => {
            out.warn(
                "record",
                &format!(
                    "{}: {e}; {verb} goes on without it",
                    root.join(SWITCH_LOCK).display()
                ),
            );
            Ok(None)
        }
    }
}

/// Take [`DEFAULT_RECORD`] out of `root`: a turn-on finished there.
fn clear_default_record(root: &Path) -> Result<(), String> {
    aterm_uds::private_file::remove_private(root, DEFAULT_RECORD)
        .map(|_| ())
        .map_err(|e| format!("{}: {e}", root.join(DEFAULT_RECORD).display()))
}

/// The proof waits for what must happen once the broker answers and the bridge connected (the
/// steps before it): a hang bound, not a latency budget a busy machine's working fabric misses.
const PROOF_DEADLINE: Duration = Duration::from_secs(60);

const BIND_DEADLINE: Duration = Duration::from_secs(5);

const ARM_DEADLINE: Duration = Duration::from_secs(5);

const IO_TIMEOUT: Duration = Duration::from_secs(8);

const TRACE_ENV: &str = "ATERM_FABRIC_TRACE";

const FAIL_MINT_ENV: &str = "ATERM_FABRIC_FAIL_MINT_AT";

const ROOT_SEAM: &str = "ATERM_FABRIC_HOME";

fn root_seam() -> Option<PathBuf> {
    aterm_types::dev_seam!(ROOT_SEAM)
        .filter(|r| !r.is_empty())
        .map(PathBuf::from)
}

/// How the broker is kept alive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    /// A `~/Library/LaunchAgents` job with `KeepAlive` (macOS), in launchd's `gui/<uid>` domain.
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
    pub const fn default_here() -> Self {
        if cfg!(target_os = "macos") {
            Service::Launchd
        } else if cfg!(target_os = "linux") {
            Service::Systemd
        } else {
            Service::None
        }
    }

    const fn name(self) -> &'static str {
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
    /// `--durability strict|ordered|relaxed`.
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

/// Where everything is, resolved.
#[derive(Debug, Clone)]
pub struct Paths {
    /// `$HOME`.
    pub home: PathBuf,
    /// The login user's home from the password database.
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
    /// The tier the broker's argv names (`on --durability`, or the one the installed job names).
    pub durability: Option<crate::durability::Durability>,
}

impl Paths {
    /// The tier the broker this root runs acks under.
    #[must_use]
    pub fn tier(&self) -> crate::durability::Durability {
        crate::durability::resolve(self.durability, true)
    }
}

impl Paths {
    /// Resolve from the environment.
    ///
    /// # Errors
    /// No `$HOME`, or no aterm binary anywhere.
    pub fn resolve(fleet: Option<&str>) -> Result<Self, String> {
        let home = std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| "$HOME is not set".to_string())?;
        let root = root_seam().unwrap_or_else(|| default_root(&home));
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
            durability: None,
        })
    }

    /// [`Paths::resolve`] for `off`.
    ///
    /// # Errors
    /// As [`Paths::resolve`].
    pub fn resolve_for_off() -> Result<(Self, &'static str), String> {
        let mut p = Self::resolve(None)?;
        let configured =
            fabric::resolve_command(fabric::command_seam().as_deref(), p.config.as_deref())
                .ok()
                .flatten()
                .and_then(|(source, cmd)| {
                    let cfg = fabric::bridge_config(&cmd).ok()?;
                    let root = root_of(&cfg.broker)?;
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
                .and_then(|r| root_of(&r.broker))
                .map(|root| (root, "the rendezvous file"))
        });
        let Some((root, source)) = found else {
            return Ok((p, "the environment"));
        };
        p.root = root;
        Ok((p, source))
    }

    fn default_root(&self) -> bool {
        self.login_home
            .as_deref()
            .is_some_and(|login| self.root == default_root(login))
    }

    /// The launchd label / systemd unit name.
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
    pub(crate) fn cap(&self) -> PathBuf {
        self.root.join("node.cap")
    }
    pub(crate) fn secret(&self) -> PathBuf {
        self.root.join(MINT_SECRET)
    }
    pub(crate) fn state(&self) -> PathBuf {
        self.root.join("link-state")
    }
    /// The launchd job's plist.
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

    /// Whether the binary is the `aterm-link` shim (a test build) rather than the front door.
    pub(crate) fn is_link_shim(&self) -> bool {
        Path::new(&self.aterm)
            .file_name()
            .is_some_and(|n| n == "aterm-link")
    }

    /// The program word every command `on` writes runs.
    #[must_use]
    pub fn program(&self) -> String {
        if argv_safe(&self.aterm).is_ok() {
            self.aterm.clone()
        } else {
            self.program_link().to_string_lossy().into_owned()
        }
    }

    /// `<root>/bin/<file name>`, where `on` links a non-plain aterm path.
    #[must_use]
    pub fn program_link(&self) -> PathBuf {
        let name = Path::new(&self.aterm)
            .file_name()
            .map_or_else(|| std::ffi::OsString::from("aterm"), ToOwned::to_owned);
        self.root.join("bin").join(name)
    }

    /// The bridge command aterm launches.
    pub(crate) fn bridge_command(&self, node: &str) -> Vec<String> {
        let mut argv = vec![self.program()];
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

    fn broker_argv(&self) -> Vec<String> {
        let mut argv = vec![self.program()];
        if !self.is_link_shim() {
            argv.push("link".to_string());
        }
        argv.extend([
            "broker".to_string(),
            self.sock(),
            self.log(),
            "--secret-file".to_string(),
            self.secret().to_string_lossy().into_owned(),
        ]);
        argv.extend(crate::durability::argv_words(self.durability));
        argv
    }

    /// Whether `text` already runs [`Paths::broker_argv`] under [`Paths::label`] with `KeepAlive`.
    #[must_use]
    pub fn plist_is_current(&self, text: &str) -> bool {
        let flat = collapse_between_tags(text);
        let label = format!(
            "<key>Label</key><string>{}</string>",
            aterm_codec::xml::escape(&self.label())
        );
        if !flat.contains(&label) || !flat.contains("<key>KeepAlive</key><true/>") {
            return false;
        }
        plist_program_arguments(&flat).is_some_and(|argv| argv == self.broker_argv())
    }

    fn broker_words(&self) -> String {
        self.broker_argv()
            .iter()
            .map(|w| format!("'{w}'"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Render the launchd plist (`ProgramArguments` as XML-escaped words, no `/bin/sh -c`).
    #[must_use]
    pub fn plist_text(&self) -> String {
        let root = aterm_codec::xml::escape(&self.root.to_string_lossy());
        let pkg_bin = self.home.join("Library/Application Support/aterm/pkg/bin");
        let path = aterm_codec::xml::escape(&format!(
            "{}:/usr/bin:/bin:/usr/sbin:/sbin",
            pkg_bin.to_string_lossy()
        ));
        let args: String = self
            .broker_argv()
            .iter()
            .map(|a| format!("    <string>{}</string>\n", aterm_codec::xml::escape(a)))
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
             \x20 <key>EnvironmentVariables</key>\n\
             \x20 <dict>\n\
             \x20   <key>PATH</key><string>{path}</string>\n\
             \x20 </dict>\n\
             \x20 <key>StandardOutPath</key><string>{root}/broker.out</string>\n\
             \x20 <key>StandardErrorPath</key><string>{root}/broker.err</string>\n\
             </dict>\n\
             </plist>\n",
            label = aterm_codec::xml::escape(&self.label()),
        )
    }

    /// Render the `systemd --user` unit.
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

pub(crate) fn default_root(home: &Path) -> PathBuf {
    home.join(".local/share/aterm-fabric")
}

/// The fabric ROOT a bridge command (or a rendezvous file) points into.
pub(crate) fn root_of(broker: &str) -> Option<PathBuf> {
    let root = Path::new(broker).parent()?.to_path_buf();
    root.is_absolute().then_some(root)
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

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

fn launchd_failure_hint(root: &Path, label: &str, uid: &str) -> String {
    failure_hint(root, &format!("`launchctl print gui/{uid}/{label}`"))
}

fn systemd_failure_hint(root: &Path, label: &str) -> String {
    failure_hint(root, &format!("`journalctl --user -u {label}`"))
}

fn failure_hint(root: &Path, otherwise: &str) -> String {
    let err = root.join("broker.err");
    match supervisor_last_word(root) {
        Some(last) => format!("read {} ({last})", err.display()),
        None => format!("{} was not written; {otherwise} says why", err.display()),
    }
}

const LAST_WORD_TAIL: u64 = 4096;

/// Last non-empty stderr line from `<root>/broker.err` as `written <age> ago: <line>`.
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

fn unit_argv(text: &str) -> Option<Vec<String>> {
    let line = text
        .lines()
        .find_map(|l| l.strip_prefix("ExecStart=/bin/sh -c \""))?;
    sh_c_words(line.strip_suffix('"')?)
}

fn sh_c_words(script: &str) -> Option<Vec<String>> {
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

fn supervised_argv(argv: Vec<String>) -> Option<Vec<String>> {
    match argv.as_slice() {
        [sh, c, script] if Path::new(sh).file_name().is_some_and(|n| n == "sh") && c == "-c" => {
            sh_c_words(script)
        }
        _ => Some(argv),
    }
}

/// What this root is to its fleet, read back from what `on` left.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Role {
    /// The configured fleet.
    pub fleet: Option<String>,
}

/// Why [`Role::of`] refused a root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoleError {
    /// The installed job runs a command that is not the broker.
    NotABroker,
    /// The installed job's command line has a shape aterm never wrote.
    UnreadableJob {
        path: std::path::PathBuf,
        field: &'static str,
    },
    /// Anything else: unreadable or invalid configuration or rendezvous.
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
    /// What the root `p` is to its fleet.
    ///
    /// # Errors
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
        if let Some(r) = &rendezvous {
            if !crate::subject::is_fleet(&r.fleet) {
                return Err("the rendezvous file's fleet is invalid".to_string().into());
            }
        }
        let fleet = configured_fleet(p.config.as_deref(), rendezvous.as_ref())?;
        installed_job_is_a_broker(p)?;
        Ok(Role { fleet })
    }

    fn fleet(&self) -> Option<&str> {
        self.fleet.as_deref()
    }

    /// The command that re-applies this root as it is — every remedy that re-runs `on` is spelled with it.
    #[must_use]
    pub fn again(&self) -> String {
        format!("`aterm fabric on{}`", fleet_flag(self.fleet.as_deref()))
    }
}

fn fleet_flag(fleet: Option<&str>) -> String {
    fleet
        .filter(|f| *f != DEFAULT_FLEET)
        .map_or_else(String::new, |f| format!(" --fleet {f}"))
}

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

fn configured_fleet(
    config: Option<&Path>,
    rendezvous: Option<&Rendezvous>,
) -> Result<Option<String>, String> {
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
        .or_else(|| rendezvous.map(|r| r.command.as_slice()));
    let Some(command) = command else {
        return Ok(None);
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
    required("--broker")?;
    Ok(Some(fleet))
}

fn flag_value(flags: &[String], name: &str) -> Option<String> {
    flags
        .windows(2)
        .rfind(|w| w[0] == name)
        .map(|w| w[1].clone())
}

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

fn installed_job_is_a_broker(p: &Paths) -> Result<(), RoleError> {
    if let Some((_, argv)) = installed_job(p)? {
        let broker = argv.get(1).is_some_and(|word| word == "broker")
            || (argv.get(1).is_some_and(|word| word == "link")
                && argv.get(2).is_some_and(|word| word == "broker"));
        if !broker {
            return Err(RoleError::NotABroker);
        }
    }
    Ok(())
}

fn undo_command(program: &str, scope: &[(&str, Option<String>)]) -> String {
    let mut s = String::new();
    for (name, value) in scope {
        if let Some(v) = value.as_deref().filter(|v| !v.is_empty()) {
            s.push_str(name);
            s.push('=');
            s.push_str(&fabric::shell_word(v));
            s.push(' ');
        }
    }
    s.push_str(program);
    s.push_str(" fabric off");
    s
}

fn login_home() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        aterm_uds::account::home_of(aterm_uds::peer::our_uid()).filter(|h| h.is_absolute())
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// `<control socket dir>/fabric.toml`.
#[must_use]
pub fn rendezvous_path() -> Option<PathBuf> {
    aterm_uds::control_socket_dir().map(|d| d.join(RENDEZVOUS_FILE))
}

/// Whether [`Paths::resolve`] names an `aterm` on `$PATH` (or the test seam's) as the broker's
/// program, rather than this process's own executable.
#[must_use]
pub fn names_aterm_on_path() -> bool {
    names_aterm_on(
        aterm_types::dev_seam!("ATERM_BIN").as_deref(),
        std::env::var_os("PATH").as_deref(),
    )
}

/// [`names_aterm_on_path`] over the seam's value `seam` and `$PATH`'s `path`: a seam that is not
/// blank, or an executable `aterm` in one of `path`'s directories.
fn names_aterm_on(seam: Option<&std::ffi::OsStr>, path: Option<&std::ffi::OsStr>) -> bool {
    seam.is_some_and(|b| !b.to_string_lossy().trim().is_empty())
        || path.is_some_and(|p| which_in(std::ffi::OsStr::new("aterm"), p).is_some())
}

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

/// POSIX `cksum`'s crc: the suffix of a non-default root's label ([`Paths::label`]).
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

/// The eight grants of a node's capability ring for `node` on `fleet` (`<root>/node.cap`).
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

/// The grants of a person's capability ring (`h-` human or `a-` service) on `fleet`.
#[must_use]
pub fn person_grants(fleet: &str, person: &str) -> Vec<String> {
    let mut ring = Vec::with_capacity(6);
    if person.starts_with("h-") {
        ring.push(format!("rw,p={person}:/f/{fleet}/fleet/{person}/>"));
    }
    ring.extend([
        format!("rw,p={person}:/f/{fleet}/in/*/*/{person}/*"),
        format!("rw,p={person}:/f/{fleet}/pub/p/{person}/>"),
        format!("ro:/f/{fleet}/in/p/{person}/>"),
        format!("rw,p={person}:/f/{fleet}/cur/p/{person}/>"),
        format!("ro:/f/{fleet}/pub/>"),
    ]);
    ring
}

/// Refuse a bridge or broker argv word containing whitespace, quotes, `\`, `%`, `$`.
pub fn argv_safe(word: &str) -> Result<(), String> {
    let bad = word
        .chars()
        .find(|&c| c.is_whitespace() || c.is_control() || "'\"\\%$".contains(c));
    if let Some(c) = bad {
        return Err(format!(
            "`{}` holds {}; every word of the fabric's commands must be plain — no whitespace \
             (aterm splits `[fabric] command` on it, and `ps` is how the broker is found), no \
             quote or backslash (the systemd unit quotes the broker's words in a `sh -c` \
             script), no `%` or `$` (systemd expands them) and no control character",
            safe(word, 256),
            match c {
                c if c.is_whitespace() => "whitespace".to_string(),
                c if c.is_control() => "a control character".to_string(),
                c => format!("a `{c}`"),
            }
        ));
    }
    Ok(())
}

/// What `fabric.toml` records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendezvous {
    /// The fleet.
    pub fleet: String,
    /// The broker the bridges dial.
    pub broker: String,
    /// This machine's node id.
    pub node: String,
    /// The cap file.
    pub cap_file: String,
    /// The bridge's state dir.
    pub state: String,
    /// The whole bridge command, as the argv `[fabric] command` names.
    pub command: Vec<String>,
}

impl Rendezvous {
    /// Render the file.
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "# Written by `aterm fabric on` — where this machine's fabric is. `aterm link\n\
             # ls` reads it for flags not given, and `aterm fabric` when aterm.toml has no\n\
             # [fabric] command; `aterm fabric off` removes it. Not the source of truth: aterm\n\
             # launches its bridge from `[fabric] command` in aterm.toml, which `command`\n\
             # below mirrors.\n\
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
            toml_command(&self.command),
        )
    }

    /// Parse one. `command` is read as `[fabric] command` is ([`fabric::command_in_toml`]).
    ///
    /// # Errors
    /// Not TOML, a key missing or not a string, or a `command` that is neither form.
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
            command: table
                .get("command")
                .ok_or_else(|| "`command` is missing".to_string())
                .and_then(fabric::command_of)?
                .unwrap_or_default(),
        })
    }

    /// The rendezvous file on this machine.
    ///
    /// # Errors
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

fn toml_command(argv: &[String]) -> String {
    toml_str(&argv.join(" "))
}

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

/// `args` with `--fleet --broker --cap-file --state` filled from the rendezvous file.
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

/// Prepend absent `--fleet` and `--broker` from `r`, and absent `--cap-file` and `--state` too when `--broker` was not given.
#[must_use]
pub fn fill_defaults(args: &[String], r: &Rendezvous) -> Vec<String> {
    let has = |flag: &str| args.iter().any(|a| a == flag);
    let with_broker = !has("--broker");
    let mut out = Vec::with_capacity(args.len() + 8);
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
    out.extend_from_slice(args);
    out
}

fn key_path(repr: &str) -> Option<Vec<String>> {
    let keys = aterm_toml::edit::Key::parse(repr.trim()).ok()?;
    Some(keys.iter().map(|k| k.get().to_string()).collect())
}

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

fn line_key(line: &str) -> Option<Vec<String>> {
    line.match_indices('=')
        .find_map(|(at, _)| key_path(&line[..at]))
}

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

fn is_command_line(line: &str) -> bool {
    line_key(line).is_some_and(|k| k == ["command"])
}

/// `edited` read back through the parser the app loads aterm.toml with, held to what the edit meant.
///
/// # Errors
/// `text` or `edited` is not TOML, or `edited` means something else.
fn reads_back(
    text: &str,
    edited: &str,
    fabric: impl FnOnce(&mut aterm_toml::Table),
) -> Result<(), String> {
    let mut want: aterm_toml::Table =
        aterm_toml::from_str(text).map_err(|e| format!("not valid TOML: {e}"))?;
    let after: aterm_toml::Table = aterm_toml::from_str(edited)
        .map_err(|_| "the edited file would not be valid TOML".to_string())?;
    fabric(&mut want);
    if want == after {
        Ok(())
    } else {
        Err("the edited file would not read back as meant".to_string())
    }
}

fn command_value_end(lines: &[&str], at: usize) -> usize {
    let line = lines[at];
    let value = line.split_once('=').map_or("", |(_, v)| v.trim_start());
    if value.starts_with('[') {
        return array_end(lines, at, line.len() - value.len()).map_or(lines.len(), |end| end + 1);
    }
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

fn array_end(lines: &[&str], at: usize, from: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut string: Option<&'static str> = None;
    for (n, line) in lines.iter().enumerate().skip(at) {
        let mut rest = if n == at { line.get(from..)? } else { *line };
        while let Some(c) = rest.chars().next() {
            if let Some(close) = string {
                if c == '\\' && matches!(close, "\"" | "\"\"\"") {
                    let mut after = rest.chars();
                    after.next();
                    after.next();
                    rest = after.as_str();
                } else if let Some(after) = rest.strip_prefix(close) {
                    string = None;
                    rest = after;
                } else {
                    rest = &rest[c.len_utf8()..];
                }
                continue;
            }
            if let Some(open) = ["\"\"\"", "'''", "\"", "'"]
                .into_iter()
                .find(|q| rest.starts_with(q))
            {
                string = Some(open);
                rest = &rest[open.len()..];
                continue;
            }
            match c {
                '#' => break,
                '[' => depth = depth.saturating_add(1),
                ']' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return Some(n);
                    }
                }
                _ => {}
            }
            rest = &rest[c.len_utf8()..];
        }
        if matches!(string, Some("\"" | "'")) {
            string = None;
        }
    }
    None
}

/// Set `[fabric] command` in `text` to `cmd` (formatted via [`toml_command`]).
pub fn set_fabric_command(text: &str, cmd: &[String]) -> Result<(String, bool), String> {
    if !aterm_types::fabric_command::line_spells(cmd) {
        return Err(format!(
            "the command {} has a word that is empty or holds whitespace, which `[fabric] \
             command`'s line cannot spell; aterm.toml is left as it was (an argv array would \
             make an older aterm reject the whole file)",
            safe(&fabric::shown_argv(cmd), 512)
        ));
    }
    let receipts_missing = fabric::receipts_in_toml(text)?.is_none();
    if let Some(current) = fabric::command_in_toml(text)? {
        if current == cmd && !receipts_missing {
            return Ok((text.to_string(), false));
        }
    }
    let lines: Vec<&str> = text.lines().collect();
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
    let new_line = format!("command = {}", toml_command(cmd));
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
        f.insert("command".to_string(), Value::String(cmd.join(" ")));
        if f.get("receipts").is_none() {
            f.insert("receipts".to_string(), Value::Boolean(true));
        }
        t.insert("fabric".to_string(), Value::Table(f));
    })
    .map_err(|why| {
        format!(
            "{why}: aterm.toml is left as it was; set `command = {}` in its [fabric] table by \
             hand and run this again",
            toml_command(cmd)
        )
    })?;
    Ok((joined, true))
}

fn has_dotted_fabric(lines: &[&str]) -> bool {
    lines
        .iter()
        .take_while(|l| table_header(l).is_none())
        .any(|l| line_key(l).is_some_and(|k| k.first().is_some_and(|k| k == "fabric")))
}

/// `text` with the `[fabric]` table and every `[fabric.<sub>]` table removed.
///
/// # Errors
/// A file that is not valid TOML, a result that still defines `fabric` (a dotted key or an
/// inline table, which this editor does not remove), or one that does not read back as `text`
/// without it (`reads_back`).
pub fn remove_fabric_table(text: &str) -> Result<(String, Vec<String>), String> {
    let table: aterm_toml::Table = aterm_toml::from_str(text).map_err(|e| {
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

pub(crate) use aterm_uds::atomic_file::{
    create_temp_beside, write as write_atomic, ConfigFile, EditLanded, LOCK_OPEN_FLAGS,
};

/// `n` random bytes through the one audited entropy helper (`aterm_uds::rand`).
pub(crate) fn random_bytes(n: usize) -> io::Result<Vec<u8>> {
    let mut buf = vec![0u8; n];
    aterm_uds::rand::fill(&mut buf)?;
    Ok(buf)
}

/// The node id a bridge with this state dir answers as, if one is provisioned.
pub(crate) fn read_node(state: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(state.join("node")).ok()?;
    let node = raw.trim();
    is_node_id(node).then(|| node.to_string())
}

/// Whether `node` is a node id.
#[must_use]
pub fn is_node_id(node: &str) -> bool {
    node.starts_with("n-") && crate::subject::is_principal(node)
}

/// Why `<root>/node.cap` is not `node`'s ring on the fleet under the root's secret, and whether
/// `on` keeps it aside before minting over it.
#[derive(Debug, PartialEq, Eq)]
struct CapMiss {
    why: String,
    /// A ring genuine under the secret that no `on` here mints again: one for another node id
    /// than the state dir names, or one off this fleet that is not the ring `on` mints for
    /// another.
    keep: bool,
}

impl From<String> for CapMiss {
    fn from(why: String) -> Self {
        Self { why, keep: false }
    }
}

fn cap_is_for(path: &Path, fleet: &str, node: &str, secret: &[u8]) -> Result<usize, CapMiss> {
    let caps = crate::bridge::read_cap_file(&path.to_string_lossy()).map_err(|e| e.to_string())?;
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
        return Err("not minted under the current mint secret"
            .to_string()
            .into());
    }
    if !caps.iter().any(|c| {
        astream_cap::Grant::parse(&c.grant).is_ok_and(|g| g.principal.as_deref() == Some(node))
    }) {
        return Err(CapMiss {
            why: format!("minted for a different node id, not {node}"),
            keep: true,
        });
    }
    let fleet_of = |grant: &str| {
        astream_cap::Grant::parse(grant)
            .ok()
            .and_then(|g| Some(g.filter.strip_prefix("/f/")?.split('/').next()?.to_string()))
    };
    if caps
        .iter()
        .all(|c| fleet_of(&c.grant).as_deref() == Some(fleet))
    {
        return Ok(caps.len());
    }
    // Every tag verifies, and a tag is the secret's MAC of its grant: the ring `on` mints for
    // another fleet comes back grant for grant from `on --fleet <it>`, any other ring would not.
    let mut held: Vec<&str> = caps.iter().map(|c| c.grant.as_str()).collect();
    held.sort_unstable();
    let others: BTreeSet<String> = caps
        .iter()
        .filter_map(|c| fleet_of(&c.grant))
        .filter(|f| f != fleet)
        .collect();
    let regenerable = others.iter().any(|other| {
        let mut ring = node_grants(other, node);
        ring.sort_unstable();
        ring.iter().map(String::as_str).eq(held.iter().copied())
    });
    Err(CapMiss {
        why: if others.is_empty() {
            format!("some of its grants lie outside /f/{fleet}/")
        } else {
            format!(
                "minted for fleet {}, not {fleet}",
                safe(&others.into_iter().collect::<Vec<_>>().join(", "), 64)
            )
        },
        keep: !regenerable,
    })
}

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

/// Mint the eight grants of [`node_grants`] into `<root>/node.cap` as one transaction.
///
/// # Errors
/// A secret the reader refuses, a refused mint, or a staging/rename failure.
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

/// The step lines, printed as they happen, or kept for a caller that reports them its own way
/// ([`Self::kept`]).
pub(crate) struct Out {
    pub(crate) dry_run: bool,
    pub(crate) changed: usize,
    pub(crate) failed: bool,
    /// The WARNING lines said ([`Self::warn`]).
    #[cfg(test)]
    pub(crate) warned: std::cell::Cell<usize>,
    /// Where the lines go in place of stdout, each as `name verdict detail`.
    kept: Option<std::cell::RefCell<Vec<String>>>,
}

impl Out {
    pub(crate) const fn new(dry_run: bool) -> Self {
        Self {
            dry_run,
            changed: 0,
            failed: false,
            #[cfg(test)]
            warned: std::cell::Cell::new(0),
            kept: None,
        }
    }

    /// An `Out` that changes things (not a dry run) and keeps its lines ([`Self::said`]) rather
    /// than print them.
    pub(crate) fn kept() -> Self {
        Self {
            kept: Some(std::cell::RefCell::new(Vec::new())),
            ..Self::new(false)
        }
    }

    /// The lines a [`Self::kept`] `Out` kept, joined by `; `.
    pub(crate) fn said(&self) -> String {
        self.kept
            .as_ref()
            .map(|kept| kept.borrow().join("; "))
            .unwrap_or_default()
    }

    pub(crate) fn line(&self, name: &str, verdict: &str, detail: &str) {
        if let Some(kept) = &self.kept {
            kept.borrow_mut().push(format!("{name} {verdict} {detail}"));
            return;
        }
        let mut out = io::stdout().lock();
        let _ = writeln!(out, "  {name:<11} {verdict:<9} {detail}");
        let _ = out.flush();
    }

    /// Nothing to do: it was already so.
    pub(crate) fn already(&self, name: &str, detail: &str) {
        self.line(name, "already", detail);
    }

    /// Something was (or, dry, would be) done.
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
        #[cfg(test)]
        self.warned.set(self.warned.get() + 1);
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

fn config_step(config: Option<&Path>, command: &[String], out: &mut Out) -> bool {
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
    said_replaced(
        out,
        config,
        file.lock().and_then(|edit| edit.replace(&new_text)),
        |saved| format!("{did} [fabric] command in {}{saved}", config.display()),
    )
}

/// What a `[fabric]` edit of aterm.toml (`replaced`, from `ConfigEdit::replace`) did, said on
/// `out` as `config`'s step, with `did` naming it beside where the previous text was saved:
/// `true` when the edit is in effect.
///
/// An edit whose directory flush failed after its rename ([`EditLanded`]) is in effect: the
/// step is ok, beside a WARNING that a power loss may take it back, and the run goes on. A
/// FAILED would have the operator believe aterm.toml unchanged while its `[fabric]` table
/// was what the next start reads.
fn said_replaced(
    out: &mut Out,
    config: &Path,
    replaced: io::Result<Option<PathBuf>>,
    did: impl FnOnce(&str) -> String,
) -> bool {
    let (bak, unflushed) = match replaced {
        Ok(bak) => (bak, None),
        Err(e) => match EditLanded::of(&e) {
            Some(landed) => (landed.bak.clone(), Some(landed.flush.to_string())),
            None => {
                out.fail("config", &format!("{}: {e}", config.display()));
                return false;
            }
        },
    };
    out.ok(
        "config",
        &did(&bak.map_or_else(String::new, |b| {
            format!(" (previous saved as {})", b.display())
        })),
    );
    if let Some(flush) = unflushed {
        out.warn(
            "config",
            &format!(
                "the edit is in effect, but flushing {}'s folder failed, so a power loss may \
                 take it back: {flush}",
                config.display()
            ),
        );
    }
    true
}

fn root_step(p: &Paths, state: &Path, out: &mut Out) -> bool {
    use aterm_uds::private_file::{ensure_private_dir, judge_private_dir, PrivateDir};
    let failed = |out: &mut Out, d: &Path, e: &io::Error| {
        if e.raw_os_error().is_some() {
            out.fail("root", &format!("{}: {e}", d.display()));
        } else {
            out.fail("root", &e.to_string());
        }
    };
    let mut missing = Vec::new();
    let mut wide = Vec::new();
    for d in [p.root.as_path(), state] {
        match judge_private_dir(d) {
            Ok(PrivateDir::Private) => {}
            Ok(PrivateDir::Missing) => missing.push(d),
            Ok(PrivateDir::Open(mode)) => wide.push((d, mode)),
            Err(e) => {
                failed(out, d, &e);
                return false;
            }
        }
    }
    if missing.is_empty() && wide.is_empty() {
        out.already("root", &format!("{} (0700)", p.root.display()));
        return true;
    }
    let made: Vec<String> = missing.iter().map(|d| d.display().to_string()).collect();
    let was: Vec<String> = wide
        .iter()
        .map(|(d, mode)| format!("{} was {mode:04o}", d.display()))
        .collect();
    let (mut would, mut did) = (Vec::new(), Vec::new());
    if !made.is_empty() {
        would.push(format!("create {} (0700)", made.join(" and ")));
        did.push(format!("created {} (0700)", made.join(" and ")));
    }
    if !was.is_empty() {
        would.push(format!("chmod 700 — {}", was.join(", ")));
        did.push(format!("made 0700 ({})", was.join(", ")));
    }
    if !out.dry_run {
        for d in [p.root.as_path(), state] {
            if let Err(e) = ensure_private_dir(d) {
                failed(out, d, &e);
                return false;
            }
        }
    }
    out.done("root", &would.join("; "), &did.join("; "));
    true
}

/// Remove `aterm fabric off`'s records ([`OFF_RECORD`]) from `p`'s root and beside aterm.toml:
/// `on` turns the fabric on again, and the converge pass may keep it on after. Said only where
/// there was one.
fn record_step(p: &Paths, service: Service, out: &mut Out) -> bool {
    let kept = kept_off_record(p);
    let root = remove_off_record(&p.root.join(OFF_RECORD), out);
    let kept = kept.is_none_or(|record| remove_off_record(&record, out));
    hand_record_step(p, service, out) && kept && root
}

/// Leave [`HAND_RECORD`] in `p`'s root where `on` runs with `--service none` (`service`), and
/// take it out where it hands the broker to launchd or systemd.
fn hand_record_step(p: &Paths, service: Service, out: &mut Out) -> bool {
    let record = p.root.join(HAND_RECORD);
    let there = match std::fs::symlink_metadata(&record) {
        Ok(_) => true,
        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
        Err(e) => {
            out.fail("record", &format!("{}: {e}", record.display()));
            return false;
        }
    };
    let written = match (service, there) {
        (Service::None, true) => {
            out.already(
                "record",
                &format!(
                    "{} (you run the broker: the converge pass renews no job of this root)",
                    record.display()
                ),
            );
            return true;
        }
        (Service::None, false) => {
            out.done(
                "record",
                &format!(
                    "write {} (you run the broker: the converge pass renews no job of this root)",
                    record.display()
                ),
                &format!("wrote {} (you run the broker)", record.display()),
            );
            true
        }
        (_, true) => {
            out.done(
                "record",
                &format!("remove {} (`on --service none`'s record)", record.display()),
                &format!(
                    "removed {} (`on --service none`'s record)",
                    record.display()
                ),
            );
            false
        }
        (_, false) => return true,
    };
    if out.dry_run {
        return true;
    }
    let done = if written {
        aterm_uds::private_file::write_private(&p.root, HAND_RECORD, b"").map(|()| true)
    } else {
        aterm_uds::private_file::remove_private(&p.root, HAND_RECORD)
    };
    match done {
        Ok(_) => true,
        Err(e) => {
            out.fail("record", &format!("{}: {e}", record.display()));
            false
        }
    }
}

/// [`record_step`] for one record.
fn remove_off_record(record: &Path, out: &mut Out) -> bool {
    match std::fs::symlink_metadata(record) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return true,
        Err(e) => {
            out.fail("record", &format!("{}: {e}", record.display()));
            return false;
        }
    }
    out.done(
        "record",
        &format!(
            "remove {} (`aterm fabric off`'s record, which keeps the converge pass from turning \
             the fabric on)",
            record.display()
        ),
        &format!("removed {} (`aterm fabric off`'s record)", record.display()),
    );
    if out.dry_run {
        return true;
    }
    match std::fs::remove_file(record) {
        Ok(()) => true,
        Err(e) if e.kind() == io::ErrorKind::NotFound => true,
        Err(e) => {
            out.fail("record", &format!("{}: {e}", record.display()));
            false
        }
    }
}

/// Write `aterm fabric off`'s records ([`OFF_RECORD`]) in `p`'s root, made 0700 where it is
/// missing, and beside aterm.toml where its folder is there ([`kept_off_record`]): the off's first
/// writes, so one stopped short keeps the converge pass from turning the fabric on again too.
fn off_record_step(p: &Paths, out: &mut Out) {
    write_off_record(&p.root.join(OFF_RECORD), Some(&p.root), out);
    let kept = kept_off_record(p).filter(|record| record.parent().is_some_and(Path::is_dir));
    if let Some(record) = kept {
        write_off_record(&record, None, out);
    }
}

/// [`off_record_step`] for `record`; `root` is the fabric root it is written in, made private
/// first, where it is that record. aterm.toml's folder is left as it is (the record is empty).
fn write_off_record(record: &Path, root: Option<&Path>, out: &mut Out) {
    if std::fs::symlink_metadata(record).is_ok() {
        out.already(
            "record",
            &format!(
                "{} (the converge pass leaves the fabric off)",
                record.display()
            ),
        );
        return;
    }
    out.done(
        "record",
        &format!(
            "write {} (the converge pass leaves the fabric off until `aterm fabric on`)",
            record.display()
        ),
        "",
    );
    if out.dry_run {
        return;
    }
    let written = match root {
        Some(root) => aterm_uds::private_file::ensure_private_dir(root)
            .and_then(|_| aterm_uds::private_file::write_private(root, OFF_RECORD, b"")),
        None => aterm_uds::atomic_file::write(record, b"", 0o600),
    };
    if let Err(e) = written {
        out.fail("record", &format!("{}: {e}", record.display()));
    }
}

/// What the fabric reads as where no `[fabric] command` turns it on ([`unset_state`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unset {
    /// The person turned it off: why, as one line naming what says so.
    PersonOff(String),
    /// Off, and nothing says the person ever turned it off: what was found.
    Never(String),
}

/// Whether the fabric is unset here, and why: `None` where `[fabric] command` (or
/// `$ATERM_FABRIC_COMMAND`) names a bridge, so the fabric is on. Otherwise the person turned it
/// off where `aterm fabric off`'s record stands in the fabric root, or where the root holds what
/// an earlier `on` left (a node id, the mint secret or the node grants) beside no command and no
/// [`DEFAULT_RECORD`]: an `off` from before the record, which keeps them; a command the person
/// cleared from aterm.toml after a turn-on finished; or a person's `on` that stopped short, which
/// `aterm fabric on` finishes. The converge pass leaves each as it is. Anything else (a root the
/// pass began turning on included) is a fabric nobody turned off, which the pass turns on.
///
/// # Errors
/// aterm.toml or the command cannot be read, or `$HOME` is not set.
pub fn unset_state() -> Result<Option<Unset>, String> {
    let command = configured_command()?;
    let p = Paths::resolve(None)?;
    Ok(unset_in(
        &p.root,
        kept_off_record(&p).as_deref(),
        command.is_some(),
    ))
}

/// [`unset_state`] for the fabric root `root` and the off record kept beside aterm.toml at `kept`
/// ([`kept_off_record`]), `command` saying whether a bridge command is configured.
fn unset_in(root: &Path, kept: Option<&Path>, command: bool) -> Option<Unset> {
    if command {
        return None;
    }
    if let Some(record) = off_recorded(root, kept) {
        return Some(Unset::PersonOff(format!(
            "off by your choice: `aterm fabric off` recorded it ({}); `aterm fabric on` turns it \
             on",
            record.display()
        )));
    }
    let by_default = std::fs::symlink_metadata(root.join(DEFAULT_RECORD)).is_ok();
    let left = read_node(&root.join("link-state"))
        .map(|node| format!("node {node}"))
        .or_else(|| {
            [MINT_SECRET, "node.cap"]
                .into_iter()
                .find(|name| std::fs::symlink_metadata(root.join(name)).is_ok())
                .map(str::to_owned)
        });
    match left {
        Some(what) if !by_default => Some(Unset::PersonOff(format!(
            "off: {} keeps {what} beside no [fabric] command, as an earlier `aterm fabric off` \
             leaves it (or a command cleared from aterm.toml, or an `aterm fabric on` that \
             stopped short); `aterm fabric on` turns it on",
            root.display()
        ))),
        Some(_) => Some(Unset::Never(
            "off: an earlier pass began turning it on and did not finish".to_string(),
        )),
        None => Some(Unset::Never(
            "off: no [fabric] command, and it was never turned off here".to_string(),
        )),
    }
}

/// What [`turn_on_by_default`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ByDefault {
    /// `[fabric] command` was set already: nothing was done.
    On,
    /// The person turned it off ([`Unset::PersonOff`]): left as it is, and why.
    Left(String),
    /// `aterm fabric on`'s steps turned it on: what they said.
    TurnedOn(String),
}

/// Turn the fabric on as `aterm fabric on` does with no flags (the platform's broker job, the
/// fleet and durability the root keeps), its step lines kept rather than printed, and no proof
/// (which would leave a note in a session's inbox nobody asked for): the converge pass's
/// `sync.fabric` where the fabric was never turned off ([`unset_state`] reads [`Unset::Never`]),
/// read again under the off's lock ([`by_default_in`]) so a person's `aterm fabric off` since the
/// pass read it, or running now, stands. Nothing asks: `on` reads nothing from a terminal.
///
/// # Errors
/// The state cannot be read, `on` cannot resolve its paths, or a step failed (what the steps
/// said).
pub fn turn_on_by_default() -> Result<ByDefault, String> {
    match unset_state()? {
        None => return Ok(ByDefault::On),
        Some(Unset::PersonOff(why)) => return Ok(ByDefault::Left(why)),
        Some(Unset::Never(_)) => {}
    }
    let opts = OnOpts::default();
    let prepared = prepare_on(&opts)?;
    by_default_in(
        &prepared.p.root,
        kept_off_record(&prepared.p).as_deref(),
        &|| configured_command().map(|command| command.is_some()),
        |out| on_steps(&prepared, &opts, Service::default_here(), out, Proof::Skip).map(|_| ()),
    )
}

/// [`turn_on_by_default`] in the fabric root `root`, `steps` its run of `on`'s steps: under
/// [`SWITCH_LOCK`], tried once (an `off` or `on` that holds it leaves the fabric as it is), the
/// state read again (`command` says whether a bridge command is configured now),
/// [`DEFAULT_RECORD`] left before the steps and taken out once they wrote `[fabric] command`
/// (they returned `Ok`).
fn by_default_in(
    root: &Path,
    kept: Option<&Path>,
    command: &dyn Fn() -> Result<bool, String>,
    steps: impl FnOnce(&mut Out) -> Result<(), ExitCode>,
) -> Result<ByDefault, String> {
    let _switch = match switch_lock(root, Duration::ZERO) {
        Ok(lock) => lock,
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
            return Ok(ByDefault::Left(
                "an `aterm fabric off` or `on` runs now: the next pass reads what it left"
                    .to_owned(),
            ));
        }
        Err(e) => return Err(format!("{}: {e}", root.join(SWITCH_LOCK).display())),
    };
    match unset_in(root, kept, command()?) {
        None => return Ok(ByDefault::On),
        Some(Unset::PersonOff(why)) => return Ok(ByDefault::Left(why)),
        Some(Unset::Never(_)) => {}
    }
    aterm_uds::private_file::write_private(root, DEFAULT_RECORD, b"")
        .map_err(|e| format!("{}: {e}", root.join(DEFAULT_RECORD).display()))?;
    let mut out = Out::kept();
    let done = steps(&mut out);
    if done.is_ok() {
        clear_default_record(root).map_err(|why| format!("{}{why}", out.said()))?;
    }
    match done {
        Ok(()) if !out.failed => Ok(ByDefault::TurnedOn(out.said())),
        Ok(()) | Err(_) => Err(out.said()),
    }
}

fn program_step(p: &Paths, out: &mut Out) -> bool {
    if p.program() == p.aterm {
        return true;
    }
    let link = p.program_link();
    let bin = link.parent().expect("the program link has a bin directory");
    let make_bin = match std::fs::symlink_metadata(bin) {
        Ok(m) if m.file_type().is_dir() => false,
        Ok(_) => {
            out.fail(
                "program",
                &format!("{} is not a physical directory", bin.display()),
            );
            return false;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => true,
        Err(e) => {
            out.fail("program", &format!("{}: {e}", bin.display()));
            return false;
        }
    };
    let target = std::path::absolute(&p.aterm).unwrap_or_else(|_| PathBuf::from(&p.aterm));
    let shown = format!(
        "{} → {}",
        link.display(),
        safe(&target.to_string_lossy(), 256)
    );
    match std::fs::symlink_metadata(&link) {
        Ok(m) if !m.file_type().is_symlink() => {
            out.fail(
                "program",
                &format!(
                    "{} is not a link `on` made; the aterm path {} is not a plain word, so `on` \
                     runs it through a link there: move that file aside and run this again",
                    link.display(),
                    safe(&p.aterm, 256)
                ),
            );
            return false;
        }
        Ok(_) if std::fs::read_link(&link).is_ok_and(|t| t == target) => {
            out.already("program", &shown);
            return true;
        }
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => {
            out.fail("program", &format!("{}: {e}", link.display()));
            return false;
        }
    }
    out.done(
        "program",
        &format!(
            "link {shown} (the aterm path is not a plain word, and `[fabric] command` is one \
             plain line)"
        ),
        &format!("linked {shown}"),
    );
    if out.dry_run {
        return true;
    }
    let made = (if make_bin {
        std::fs::create_dir(bin)
    } else {
        Ok(())
    })
    .and_then(|()| {
        std::fs::set_permissions(bin, std::os::unix::fs::PermissionsExt::from_mode(0o700))
    });
    if let Err(e) = made {
        out.fail("program", &format!("{}: {e}", bin.display()));
        return false;
    }
    let staged = link.with_file_name(format!(
        ".{}.{}.new",
        link.file_name()
            .map_or_else(|| "aterm".into(), |n| n.to_string_lossy().into_owned()),
        std::process::id()
    ));
    let _ = std::fs::remove_file(&staged);
    let linked =
        std::os::unix::fs::symlink(&target, &staged).and_then(|()| std::fs::rename(&staged, &link));
    if let Err(e) = linked {
        let _ = std::fs::remove_file(&staged);
        out.fail("program", &format!("{}: {e}", link.display()));
        return false;
    }
    true
}

/// Record `node` in the state dir, and say where a node file that held something else was kept.
fn record_node(state: &Path, node: &str) -> Result<(String, Option<PathBuf>), String> {
    crate::state::StateDir::open(state)
        .and_then(|s| s.provision_node(|| node.to_string()))
        .map_err(|e| format!("{}/node: {e}", state.display()))
}

fn binary_has_link(p: &Paths) -> Result<(), String> {
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
    if text.contains("aterm-link") {
        Ok(())
    } else {
        Err(format!(
            "{} has no `link` verb; update aterm",
            safe(&p.aterm, 256)
        ))
    }
}

/// Reach the broker for real, through the report's own probe.
pub(crate) fn probe(p: &Paths, node: &str) -> fabric::BrokerView {
    reach_with(p, node, |cfg| fabric::probe_broker(cfg).0)
}

/// The report's probe through `reach`, or the view of a bridge command that does not parse.
fn reach_with(
    p: &Paths,
    node: &str,
    reach: impl FnOnce(&crate::bridge::Config) -> fabric::BrokerView,
) -> fabric::BrokerView {
    match fabric::bridge_config(&p.bridge_command(node)) {
        Ok(cfg) => reach(&cfg),
        Err(e) => fabric::BrokerView {
            endpoint: p.sock(),
            error: Some(e),
            ..fabric::BrokerView::default()
        },
    }
}

fn wait_for_broker(p: &Paths, node: &str) -> Result<fabric::BrokerView, Box<fabric::BrokerView>> {
    poll_broker(
        || poll_reach(p, node),
        fabric::BrokerView::locate,
        BIND_DEADLINE,
    )
}

/// One poll of the wait: the report's probe naming no process ([`fabric::reach_broker`]).
fn poll_reach(p: &Paths, node: &str) -> fabric::BrokerView {
    reach_with(p, node, |cfg| fabric::reach_broker(cfg).0)
}

/// `reach` every 100 ms until a broker answers, then `name` its process once: the poll itself
/// connects and names nothing, so no process table is scanned while nothing listens yet. Past
/// `deadline`, the last poll's view.
fn poll_broker(
    mut reach: impl FnMut() -> fabric::BrokerView,
    name: impl FnOnce(&mut fabric::BrokerView),
    deadline: Duration,
) -> Result<fabric::BrokerView, Box<fabric::BrokerView>> {
    let started = Instant::now();
    loop {
        let mut view = reach();
        if view.read_ok() {
            name(&mut view);
            return Ok(view);
        }
        if started.elapsed() > deadline {
            return Err(Box::new(view));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// This process's effective uid, as `id -u` prints it.
#[cfg(unix)]
pub(crate) fn uid() -> String {
    aterm_uds::peer::our_uid().to_string()
}

/// No uid off Unix: the launchd steps that name one are macOS's alone.
#[cfg(not(unix))]
pub(crate) fn uid() -> String {
    "501".to_string()
}

fn launchctl_program() -> std::ffi::OsString {
    #[cfg(any(test, debug_assertions))]
    if let Some(stand_in) = std::env::var_os("ATERM_TEST_LAUNCHCTL").filter(|p| !p.is_empty()) {
        return stand_in;
    }
    aterm_uds::os_path!("launchctl").into()
}

/// `launchctl <args>`.
pub(crate) fn launchctl(args: &[&str]) -> Result<(), String> {
    let out = aterm_uds::spawnfd::command(launchctl_program())
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

/// `launchctl list`'s `PID Status Label` rows.
#[cfg(target_os = "macos")]
pub(crate) fn launchctl_list() -> String {
    aterm_uds::spawnfd::command(launchctl_program())
        .arg("list")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// What launchd's `gui/<uid>` domain says of a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LaunchdJob {
    /// The domain holds no such job (`print` exits 113).
    Absent,
    /// There is no `gui/<uid>` domain (`print` exits 112).
    NoGuiLogin,
    /// Loaded; `Some(pid)` while it runs.
    Loaded(Option<u32>),
    /// `print` could not run, or exited otherwise: launchd said nothing of the job.
    Unread,
}

const LAUNCHCTL_NO_DOMAIN: i32 = 112;
const LAUNCHCTL_NO_SERVICE: i32 = 113;

/// Query `launchctl print gui/<uid>/<label>`.
pub(crate) fn launchd_job(label: &str) -> LaunchdJob {
    let (code, stdout) = launchd_print(label);
    job_of(code, &stdout)
}

/// `launchctl print gui/<uid>/<label>`'s exit code and stdout; `(None, "")` where it could not
/// run, which [`job_of`] reads as unread.
fn launchd_print(label: &str) -> (Option<i32>, String) {
    match aterm_uds::spawnfd::command(launchctl_program())
        .args(["print", &format!("gui/{}/{label}", uid())])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    {
        Ok(out) => (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        ),
        Err(_) => (None, String::new()),
    }
}

/// The arguments launchd loaded a job with, as `print`'s `stdout` lists them (one to a line
/// between `arguments = {` and its `}`, each trimmed); `None` where it lists none.
fn printed_arguments(stdout: &str) -> Option<Vec<String>> {
    let mut lines = stdout
        .lines()
        .map(str::trim)
        .skip_while(|line| *line != "arguments = {");
    lines.next()?;
    let mut args = Vec::new();
    for line in lines {
        if line == "}" {
            return Some(args);
        }
        args.push(line.to_string());
    }
    None
}

/// What the broker step rewrites of `p`'s plist, `old` the one on disk (`None`: no file) and
/// `printed` what `print` said of the job (`loaded`: launchd holds it): the plist `on` writes
/// where `old` is not it, and also where launchd holds the job loaded from other arguments than
/// it (a plist written while the job stayed loaded), since a kickstart restarts what launchd
/// loaded, not the file. A `print` that lists no arguments is read as the file's.
fn launchd_rewrite(p: &Paths, old: Option<String>, loaded: bool, printed: &str) -> Option<Rewrite> {
    let same = old.as_deref().is_some_and(|old| p.plist_is_current(old));
    let argv = p.broker_argv();
    let older = loaded
        && printed_arguments(printed).is_some_and(|args| {
            !args
                .iter()
                .map(String::as_str)
                .eq(argv.iter().map(|a| a.trim()))
        });
    (!same || older).then(|| Rewrite {
        text: p.plist_text(),
        old,
    })
}

/// What `print`'s exit `code` (`None`: killed by a signal) and `stdout` say of the job: only 113
/// is "no such job".
fn job_of(code: Option<i32>, stdout: &str) -> LaunchdJob {
    match code {
        Some(0) => LaunchdJob::Loaded(stdout.lines().find_map(|l| {
            l.trim()
                .strip_prefix("pid = ")
                .and_then(|p| p.trim().parse().ok())
        })),
        Some(LAUNCHCTL_NO_DOMAIN) => LaunchdJob::NoGuiLogin,
        Some(LAUNCHCTL_NO_SERVICE) => LaunchdJob::Absent,
        _ => LaunchdJob::Unread,
    }
}

/// Whether `launchctl print-disabled gui/<uid>` names `label` disabled (a person's `launchctl
/// disable`, or its switch in Login Items turned off): its rows read `"<label>" => disabled`
/// (`=> true` before macOS 13).
///
/// # Errors
/// `print-disabled` could not run or failed: whether the job is disabled is not known.
pub(crate) fn launchd_disabled(label: &str) -> Result<bool, String> {
    let out = aterm_uds::spawnfd::command(launchctl_program())
        .args(["print-disabled", &format!("gui/{}", uid())])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("`launchctl print-disabled` could not run: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "`launchctl print-disabled gui/{}` exited {}",
            uid(),
            out.status.code().unwrap_or(-1)
        ));
    }
    Ok(disabled_in(&String::from_utf8_lossy(&out.stdout), label))
}

/// Whether `print-disabled`'s `text` names `label` disabled.
fn disabled_in(text: &str, label: &str) -> bool {
    let quoted = format!("\"{label}\"");
    text.lines().any(|line| {
        line.trim()
            .strip_prefix(&quoted)
            .and_then(|rest| rest.trim().strip_prefix("=>"))
            .is_some_and(|state| matches!(state.trim(), "disabled" | "true"))
    })
}

/// launchd as the broker step drives its job: the live `launchctl` ([`LiveLaunchd`]), or a test's
/// fake, so the order of the step's verbs is proved with no launchd.
pub(crate) trait Launchd {
    /// What `launchctl print gui/<uid>/<label>` exits with and prints.
    fn print(&self, label: &str) -> (Option<i32>, String);
    /// What that `print` says of the job ([`job_of`]).
    fn job(&self, label: &str) -> LaunchdJob {
        let (code, printed) = self.print(label);
        job_of(code, &printed)
    }
    /// `launchctl <args>`.
    ///
    /// # Errors
    /// launchctl could not run or exited non-zero: the verb, its exit status and its stderr.
    fn run(&self, args: &[&str]) -> Result<(), String>;
    /// Write `text` to the plist at `plist` (0644, atomically: a failed write leaves the file as
    /// it was).
    ///
    /// # Errors
    /// The write failed: the path and the error.
    fn write(&self, plist: &Path, text: &str) -> Result<(), String>;
    /// Whether process `pid` still runs.
    fn running(&self, pid: u32) -> bool;
    /// Wait `pause` before the next look at a job launchd is still letting go.
    fn pause(&self, pause: Duration);
}

/// The live `launchctl` (`$ATERM_TEST_LAUNCHCTL`'s stand-in in a test build).
pub(crate) struct LiveLaunchd;

impl Launchd for LiveLaunchd {
    fn print(&self, label: &str) -> (Option<i32>, String) {
        launchd_print(label)
    }
    fn run(&self, args: &[&str]) -> Result<(), String> {
        launchctl(args)
    }
    fn write(&self, plist: &Path, text: &str) -> Result<(), String> {
        write_atomic(plist, text.as_bytes(), 0o644).map_err(|e| format!("{}: {e}", plist.display()))
    }
    fn running(&self, pid: u32) -> bool {
        aterm_uds::process::pid_alive(pid)
    }
    fn pause(&self, pause: Duration) {
        std::thread::sleep(pause);
    }
}

/// The most looks, [`GONE_POLL`] apart, the broker step takes at a job it booted out while it
/// waits for launchd to let the job go, or at the process a `kickstart -k` killed: thirty
/// seconds, past launchd's default `ExitTimeOut` (20 s from its SIGTERM to its SIGKILL; `on`'s
/// plist sets none). launchd's `bootout` returns
/// before the job is gone, and a `bootstrap` of the same label until then fails (`5:
/// Input/output error`, `37: Operation already in progress`), which left the job unloaded.
const GONE_POLLS: u32 = 300;

const GONE_POLL: Duration = Duration::from_millis(100);

/// The pause before the one bootstrap the step tries again: the label can stay busy a moment
/// after `print` stops naming the job.
const RETRY_PAUSE: Duration = Duration::from_secs(1);

/// How [`launchd_up`] brought the job up.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Up {
    /// `kickstart -k`: the loaded job restarted in place.
    InPlace,
    /// `bootstrap` loaded the plist; `Some(errors)` where it did only after what failed first
    /// (a kickstart, or the first bootstrap), as those verbs said it.
    Loaded(Option<String>),
    /// A `bootstrap` failed, and launchd holds the job after it all the same: what failed. The job
    /// it let go was gone before that bootstrap, so this one is the plist's.
    Held(String),
}

/// Wait until `gone` holds, [`GONE_POLLS`] looks [`GONE_POLL`] apart at most: whether it did.
fn until_gone(lc: &dyn Launchd, gone: impl Fn() -> bool) -> bool {
    for _ in 0..GONE_POLLS {
        if gone() {
            return true;
        }
        lc.pause(GONE_POLL);
    }
    gone()
}

/// Wait while launchd still holds `label`: whether it let it go. Only `print`'s "no such job"
/// is gone: one it could not read ([`LaunchdJob::Unread`]) is waited out like a held one.
fn let_go(lc: &dyn Launchd, label: &str) -> bool {
    until_gone(lc, || {
        matches!(lc.job(label), LaunchdJob::Absent | LaunchdJob::NoGuiLogin)
    })
}

/// What launchd holds of the broker's job `label`, read once through `lc`: whether it is loaded,
/// the pid it runs, and the rewrite the step makes ([`launchd_rewrite`] over the plist on disk,
/// `old`, and the arguments that `print` lists). `Err` is a job the step leaves as it is: no GUI
/// login, or a `print` that did not say.
fn launchd_read(
    lc: &dyn Launchd,
    p: &Paths,
    label: &str,
    old: Option<String>,
) -> Result<(bool, Option<u32>, Option<Rewrite>), LaunchdJob> {
    let (code, printed) = lc.print(label);
    let (loaded, running) = match job_of(code, &printed) {
        LaunchdJob::Loaded(pid) => (true, pid),
        LaunchdJob::Absent => (false, None),
        other => return Err(other),
    };
    Ok((loaded, running, launchd_rewrite(p, old, loaded, &printed)))
}

/// The plist `on` writes, where the one on disk is not it: its text, and the text it replaces
/// (`None` where there was no file), put back and loaded again where the new one does not load.
pub(crate) struct Rewrite {
    text: String,
    old: Option<String>,
}

/// Bring launchd's job `label` up from `plist`, `loaded` saying whether launchd holds the job now
/// and `running` the pid it runs, with `rewrite` where the plist on disk is not the one `on`
/// writes. A loaded job whose plist is unchanged restarts in place (`kickstart -k`, which starts
/// the file now at the plist's program path), with no moment it is unloaded, and the step waits
/// for the process it killed to exit: `kickstart` returns before it has, and until then that
/// process (the old build) can still answer on the socket. A kickstart that fails while launchd
/// still holds the job leaves it loaded as it was. Otherwise a loaded job is booted out and waited
/// for until launchd lets it go ([`let_go`]); only then is the plist written (so the plist on disk
/// is never newer than a job launchd still holds) and bootstrapped, and a bootstrap that fails is
/// tried once more, [`RETRY_PAUSE`] later. No job is stranded by the step: one launchd never lets
/// go is never bootstrapped over or rewritten; one launchd holds after a failed bootstrap is that
/// bootstrap's ([`Up::Held`]), never booted out again, which the wait for the broker judges; and
/// where the rewritten plist does not load and `print` says launchd holds no job of the label,
/// the one it replaced is put back and loaded, [`RETRY_PAUSE`] later.
///
/// # Errors
/// What launchd holds now (the old job it did not let go or a kickstart did not restart, a killed
/// process that did not exit, the replaced plist loaded again, or an installed plist it did not
/// load), then each step that failed, in order, with what it said: the last one ends the error.
fn launchd_up(
    lc: &dyn Launchd,
    label: &str,
    plist: &Path,
    (loaded, running): (bool, Option<u32>),
    rewrite: Option<&Rewrite>,
) -> Result<Up, String> {
    let uid = uid();
    let job = format!("gui/{uid}/{label}");
    let domain = format!("gui/{uid}");
    let plist_s = plist.to_string_lossy();
    let shown = plist.display();
    let mut failed = Vec::new();
    let mut loaded = loaded;
    if loaded && rewrite.is_none() {
        match lc.run(&["kickstart", "-k", &job]) {
            Ok(()) => {
                return match running {
                    Some(old) if !until_gone(lc, || !lc.running(old)) => Err(format!(
                        "pid {old} of {job} still ran {} s after `launchctl kickstart -k` \
                         killed it, so no broker of this build was started",
                        (GONE_POLL * GONE_POLLS).as_secs()
                    )),
                    _ => Ok(Up::InPlace),
                };
            }
            // A kickstart fails where launchd let the job go meanwhile: load it.
            Err(e) if lc.job(label) == LaunchdJob::Absent => {
                failed.push(e);
                loaded = false;
            }
            Err(e) => {
                return Err(format!(
                    "launchd holds {job} (or did not say), and its `kickstart -k` failed, so it is \
                     left loaded as it was: {e}"
                ));
            }
        }
    }
    if loaded {
        // A bootout of a job launchd let go meanwhile fails: the wait says whether it holds it.
        let bootout = lc.run(&["bootout", &job]).err();
        if !let_go(lc, label) {
            failed.extend(bootout);
            failed.push(format!(
                "launchd still held {job} {} s after its bootout",
                (GONE_POLL * GONE_POLLS).as_secs()
            ));
            return Err(format!(
                "launchd still holds {job} as it was loaded before, so {shown} was neither \
                 rewritten nor loaded: {}",
                failed.join("; then ")
            ));
        }
    }
    let wrote = match rewrite {
        Some(rewrite) => match lc.write(plist, &rewrite.text) {
            Ok(()) => true,
            // The write left the plist as it was: load that one again.
            Err(e) => {
                failed.push(e);
                false
            }
        },
        None => false,
    };
    // What `print` said after the last bootstrap that failed.
    let mut after = LaunchdJob::Absent;
    for attempt in 0..2 {
        if attempt == 1 {
            lc.pause(RETRY_PAUSE);
        }
        let up = match lc.run(&["bootstrap", &domain, &plist_s]) {
            Ok(()) => Up::Loaded((!failed.is_empty()).then(|| failed.join("; then "))),
            Err(e) => {
                failed.push(e);
                after = lc.job(label);
                if !matches!(after, LaunchdJob::Loaded(_)) {
                    continue;
                }
                Up::Held(failed.join("; then "))
            }
        };
        // Loaded or held, the job is the plist on disk's: as it was, where the write failed.
        if rewrite.is_some() && !wrote {
            return Err(format!(
                "{shown} could not be written, so launchd loaded it as it was: {}",
                failed.join("; then ")
            ));
        }
        return Ok(up);
    }
    // The plist it replaced goes back only where launchd says it holds no job of the label: one
    // it did not say of may hold the new plist's.
    let mut put_back = false;
    if let (true, Some(Rewrite { old: Some(old), .. }), LaunchdJob::Absent) =
        (wrote, rewrite, &after)
    {
        lc.pause(RETRY_PAUSE);
        match lc.write(plist, old) {
            Ok(()) => {
                put_back = true;
                let replaced = format!(
                    "{shown} as `aterm fabric on` writes it did not load, so the plist it replaced \
                     was put back"
                );
                match lc.run(&["bootstrap", &domain, &plist_s]) {
                    Ok(()) => {
                        return Err(format!(
                            "{replaced} and launchd loaded that: {}",
                            failed.join("; then ")
                        ));
                    }
                    Err(e) => {
                        failed.push(e);
                        after = lc.job(label);
                        if matches!(after, LaunchdJob::Loaded(_)) {
                            return Err(format!(
                                "{replaced}, and launchd holds the job after a bootstrap of it \
                                 that failed: {}",
                                failed.join("; then ")
                            ));
                        }
                    }
                }
            }
            Err(e) => failed.push(e),
        }
    }
    Err(format!(
        "{shown}{} is installed but launchd has not loaded it{}, so nothing keeps the broker \
         until `aterm fabric on` (or the next converge pass) loads it: {}",
        if put_back {
            " (the plist it replaced, put back)"
        } else {
            ""
        },
        if after == LaunchdJob::Unread {
            " (or `launchctl print` did not say)"
        } else {
            ""
        },
        failed.join("; then ")
    ))
}

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

/// What `systemctl --user show -p MainPID -p UnitFileState <label>` says of the unit: the pid it
/// runs, and whether a person turned it off (`disable` or `mask`).
///
/// # Errors
/// No systemd user manager answered (a login with no user bus): nothing is known of the unit.
fn systemd_unit(label: &str) -> Result<(Option<u32>, bool), String> {
    let out = aterm_uds::spawnfd::command("systemctl")
        .args([
            "--user",
            "show",
            "-p",
            "MainPID",
            "-p",
            "UnitFileState",
            label,
        ])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("systemctl: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "no systemd user manager answers this login (`systemctl --user show` exited {}: {})",
            out.status.code().unwrap_or(-1),
            safe(String::from_utf8_lossy(&out.stderr).trim(), 256)
        ));
    }
    Ok(systemd_shown(&String::from_utf8_lossy(&out.stdout)))
}

/// `systemctl show`'s `MainPID=` (0: none) and whether its `UnitFileState=` is one a person set
/// to keep the unit from starting.
fn systemd_shown(text: &str) -> (Option<u32>, bool) {
    let value = |key: &str| {
        text.lines()
            .find_map(|line| line.trim().strip_prefix(key)?.strip_prefix('='))
            .map(str::trim)
    };
    (
        value("MainPID")
            .and_then(|pid| pid.parse::<u32>().ok())
            .filter(|&pid| pid != 0),
        value("UnitFileState")
            .is_some_and(|state| matches!(state, "disabled" | "masked" | "masked-runtime")),
    )
}

/// Bring systemd's user unit `label` up: write `text` (by `write`) where the unit on disk (`old`,
/// `None`: no file) is not it, then `daemon-reload`, `enable` and `restart` it (by `systemctl`).
/// A step that fails after the write puts `old` back, so the next pass reads the unit as changed
/// and runs the step again, rather than reading the new file beside the process the old one
/// started as current. A unit that had no file before is kept: it is `on`'s job either way.
///
/// # Errors
/// The step that failed, with what it said, and whether the unit it replaced went back.
fn systemd_up(
    systemctl: &dyn Fn(&[&str]) -> Result<(), String>,
    write: &dyn Fn(&[u8]) -> Result<(), String>,
    label: &str,
    old: Option<&[u8]>,
    text: &[u8],
) -> Result<(), String> {
    let rewrite = old != Some(text);
    if rewrite {
        write(text)?;
    }
    let steps: [&[&str]; 3] = [&["daemon-reload"], &["enable", label], &["restart", label]];
    for args in steps {
        if let Err(e) = systemctl(args) {
            return Err(match old.filter(|_| rewrite) {
                Some(old) => match write(old) {
                    Ok(()) => format!(
                        "{e}; the unit it replaced was put back, so the next pass writes it again"
                    ),
                    Err(back) => format!("{e}; then putting back the unit it replaced: {back}"),
                },
                None => e,
            });
        }
    }
    Ok(())
}

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

/// The broker step's failure for a broker that answered but whose bus this root's caps cannot
/// read; `None` for one that did not answer.
fn unread_broker(sock: &str, view: &fabric::BrokerView) -> Option<String> {
    let why = safe(view.error.as_deref().unwrap_or("-"), 256);
    view.reachable.then(|| {
        if view.cap_expired {
            format!(
                "the broker on {sock} answers but refuses the ring as lapsed ({why}): {}",
                renew_remedy("`aterm fabric on`")
            )
        } else {
            format!(
                "the broker on {sock} answers, but its bus cannot be read with this root's caps \
                 ({why})"
            )
        }
    })
}

/// A broker that did not answer: what the last poll saw.
fn no_answer(view: &fabric::BrokerView) -> String {
    safe(view.error.as_deref().unwrap_or("no answer"), 256)
}

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

fn broker_step(p: &Paths, node: &str, service: Service, named: bool, out: &mut Out) -> bool {
    trace("Supervise");
    let sock = p.sock();
    let label = p.label();
    match service {
        Service::None => {
            let view = probe(p, node);
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
                if let Some(why) = not_current(p, &view) {
                    out.warn(
                        "broker",
                        &format!(
                            "{why}: restart it as `{}` (or drop --service none)",
                            fabric::shown_argv(&p.broker_argv())
                        ),
                    );
                }
                true
            } else if let Some(line) = unread_broker(&sock, &view) {
                out.fail("broker", &line);
                false
            } else {
                out.fail(
                    "broker",
                    &format!(
                        "no broker answers on {sock} ({}) and --service none starts none: run \
                         `{}` yourself, or drop --service none",
                        safe(view.error.as_deref().unwrap_or("-"), 256),
                        fabric::shown_argv(&p.broker_argv())
                    ),
                );
                false
            }
        }
        Service::Launchd => {
            let plist = p.plist();
            let old = std::fs::read_to_string(&plist).ok();
            let same = old.as_deref().is_some_and(|old| p.plist_is_current(old));
            // The plist is written inside the step, once launchd let go of the job it replaces.
            let (loaded, running, rewrite) = match launchd_read(&LiveLaunchd, p, &label, old) {
                Ok(read) => read,
                Err(LaunchdJob::NoGuiLogin) => return refuse_no_gui_login(out),
                Err(_) => {
                    out.fail(
                        "broker",
                        &format!(
                            "`launchctl print gui/{}/{label}` did not say whether launchd holds \
                             the job, so it is left as it is",
                            uid()
                        ),
                    );
                    return false;
                }
            };
            let view = probe(p, node);
            let stale = not_current(p, &view);
            if rewrite.is_none() && running.is_some() && view.read_ok() && stale.is_none() {
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
            if running.is_none() && view.read_ok() {
                return refuse_unmanaged(&view, &sock, out);
            }
            if same {
                out.already("plist", &plist.display().to_string());
            } else if out.dry_run {
                out.done("plist", &format!("write {}", plist.display()), "");
            }
            let why = if !loaded {
                "not loaded".to_string()
            } else if !same {
                "the job changed".to_string()
            } else if rewrite.is_some() {
                format!(
                    "launchd runs it from other arguments than {}",
                    plist.display()
                )
            } else if running.is_none() {
                "loaded but not running".to_string()
            } else if let (true, Some(stale)) = (view.read_ok(), stale) {
                stale.to_string()
            } else {
                format!(
                    "loaded but not answering ({})",
                    safe(view.error.as_deref().unwrap_or("-"), 128)
                )
            };
            if out.dry_run {
                let how = if loaded && rewrite.is_none() {
                    "restart launchd {label} in place (`launchctl kickstart -k`)"
                } else if loaded {
                    "reload launchd {label} (`bootout`, then `bootstrap` once launchd lets the job \
                     go)"
                } else {
                    "load launchd {label} (`launchctl bootstrap`)"
                }
                .replace("{label}", &label);
                out.done("broker", &format!("{how} ({why}) and wait for {sock}"), "");
                return true;
            }
            let up = launchd_up(
                &LiveLaunchd,
                &label,
                &plist,
                (loaded, running),
                rewrite.as_ref(),
            );
            if !same && up.is_ok() {
                out.done("plist", "", &format!("wrote {}", plist.display()));
            }
            match up {
                Ok(Up::InPlace) => out.done(
                    "broker",
                    "",
                    &format!("restarted launchd {label} in place ({why})"),
                ),
                Ok(Up::Loaded(first)) => out.done(
                    "broker",
                    "",
                    &format!(
                        "{} launchd {label} ({why}){}",
                        // A kickstart of a job launchd let go meanwhile booted nothing out.
                        if loaded && rewrite.is_some() {
                            "reloaded"
                        } else {
                            "loaded"
                        },
                        first.map_or_else(String::new, |first| format!(
                            ", once these failed: {}",
                            safe(&first, 512)
                        ))
                    ),
                ),
                Ok(Up::Held(failed)) => out.done(
                    "broker",
                    "",
                    &format!(
                        "launchd holds {label} ({why}) after a bootstrap that failed: {}",
                        safe(&failed, 512)
                    ),
                ),
                Err(e) => {
                    out.fail("broker", &e);
                    return false;
                }
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
                Err(view) => {
                    let line = unread_broker(&sock, &view).unwrap_or_else(|| {
                        format!(
                            "launchd {label} did not bring {sock} up: {} — {}",
                            no_answer(&view),
                            launchd_failure_hint(&p.root, &label, &uid())
                        )
                    });
                    out.fail("broker", &line);
                    false
                }
            }
        }
        Service::Systemd => {
            let unit = p.unit();
            let text = p.unit_text();
            let old = std::fs::read(&unit).ok();
            let same = old.as_deref() == Some(text.as_bytes());
            let running = systemd_main_pid(&label);
            let view = probe(p, node);
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
            if let Err(e) = systemd_user_manager() {
                out.fail("broker", &e);
                return false;
            }
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
                    "systemctl --user daemon-reload, enable and restart {label} ({why}); wait \
                     for {sock}"
                ),
                &format!("systemd --user {label} (re)started ({why})"),
            );
            if out.dry_run {
                return true;
            }
            let write = |bytes: &[u8]| {
                write_atomic(&unit, bytes, 0o644).map_err(|e| format!("{}: {e}", unit.display()))
            };
            if let Err(e) = systemd_up(&systemctl, &write, &label, old.as_deref(), text.as_bytes())
            {
                out.fail("broker", &e);
                return false;
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
                Err(view) => {
                    let line = unread_broker(&sock, &view).unwrap_or_else(|| {
                        format!(
                            "systemd --user {label} did not bring {sock} up: {} — {}",
                            no_answer(&view),
                            systemd_failure_hint(&p.root, &label)
                        )
                    });
                    out.fail("broker", &line);
                    false
                }
            }
        }
    }
}

/// What [`renew_replaced_broker`] found, and did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrokerRenewal {
    /// No `[fabric] command`: the fabric is off, and nothing of it was read or changed.
    Off,
    /// The broker runs the build on disk: what was found.
    Current(String),
    /// The broker runs a build an update replaced, or none holds its socket, and `aterm fabric
    /// on`'s job does not keep it (the person runs it: `--service none`, or a broker that is not
    /// the job `on` installed; the command names another socket than its root's; no gui domain
    /// here to load the job in; or `aterm fabric off`'s record stands): it is left as it is, and
    /// why.
    Kept(String),
    /// The broker runs a build an update replaced, or none holds its socket while the job `on`
    /// installed is not loaded or runs no broker, and a look ([`Renew::Look`]) leaves it: which.
    Due(String),
    /// It was due, and `aterm fabric on`'s broker step renewed it: what that step said.
    Renewed(String),
}

/// Whether [`renew_replaced_broker`] renews a replaced or absent broker, or only looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Renew {
    /// Read, and change nothing.
    Look,
    /// Renew it with `aterm fabric on`'s broker step.
    Apply,
}

/// The fabric root `[fabric] command` names, as `aterm fabric on` would resolve it, and its
/// broker as the report names it.
struct Found {
    p: Paths,
    /// The root's node id; `None` where it holds none (the step then cannot run).
    node: Option<String>,
    /// The supervisor `on`'s installed job names; [`Service::None`] where it installed none.
    service: Service,
    /// The pid that job runs now; `None` where it is not loaded or not running.
    job: Option<u32>,
    /// Whether launchd holds that job (systemd: whether its unit runs a process), or why launchd
    /// cannot be asked here (no gui domain in an SSH login, or a `print` that said nothing), so
    /// nothing here loads it.
    loaded: Result<bool, String>,
    /// Whether launchd has that job disabled ([`launchd_disabled`]).
    disabled: bool,
    /// Whether a broker answers on the socket ([`fabric::answers`]), or why none does.
    answers: Result<(), String>,
    view: fabric::BrokerView,
}

/// Renew this Mac's fabric broker where it runs a build an update replaced, or where no broker
/// answers on its socket while the job `on` installed is not loaded, runs none, or runs one that
/// does not answer (an update's restart that left it unloaded, say), with `aterm fabric on`'s broker step alone (its job restarted or
/// loaded as `on` does it, then waited for), nothing else of `on`: the converge pass's
/// `sync.fabric`. Where `[fabric] command` is unset nothing is read and nothing changes, so it
/// never turns the fabric on; a broker the person keeps (`--service none`, or one that is not the
/// job `on` installed), one at another socket than the command's root's, one whose job launchd
/// has disabled or cannot be asked about here, and any broker of a root where `aterm fabric
/// off`'s record stands are left as they are. [`Renew::Look`] reads alone.
///
/// # Errors
/// aterm.toml, the root's role or installed job, or the command cannot be read, or the step
/// failed (what it said).
pub fn renew_replaced_broker(renew: Renew) -> Result<BrokerRenewal, String> {
    renew_with(configured_command()?, renew, look_at, step_on)
}

/// The configured fabric command as the app reads it (`[fabric] command` in aterm.toml), `None`
/// where there is no file or no command.
fn configured_command() -> Result<Option<Vec<String>>, String> {
    Ok(fabric::resolve_command(
        fabric::command_seam().as_deref(),
        fabric::config_path().as_deref(),
    )?
    .map(|(_, command)| command))
}

/// [`renew_replaced_broker`] over the configured `command`, with `look` resolving its root and
/// naming its broker, and `step` running the broker step: nothing is looked at without a
/// command, and the step runs only to [`Renew::Apply`] a replaced or missing broker `on`'s job
/// keeps. An apply holds [`SWITCH_LOCK`], tried once, from before it reads the off's record to
/// the step's end: an `aterm fabric off` or `on` running now leaves the broker for the next pass,
/// and one that starts meanwhile waits for the step. A lock that cannot be taken (not held: the
/// root cannot be made, say) is said beside the step's lines, and the apply goes on without it,
/// as `off` does.
fn renew_with(
    command: Option<Vec<String>>,
    renew: Renew,
    look: impl FnOnce(&[String]) -> Result<Found, String>,
    step: impl FnOnce(&Found) -> Result<String, String>,
) -> Result<BrokerRenewal, String> {
    let Some(command) = command else {
        return Ok(BrokerRenewal::Off);
    };
    let found = look(&command)?;
    // The root is never made here: a renewal whose root is gone steps without the lock, saying
    // so, and leaves no empty root behind.
    let (_switch, unlocked) = if renew == Renew::Apply {
        match aterm_uds::private_file::PrivateLock::take(
            &found.p.root,
            SWITCH_LOCK,
            Duration::ZERO,
            "held",
        ) {
            Ok(lock) => (Some(lock), String::new()),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                return Ok(BrokerRenewal::Kept(
                    "an `aterm fabric off` or `on` runs now: the next pass reads what it left"
                        .to_owned(),
                ));
            }
            Err(e) => (
                None,
                format!(
                    " (without {}: {e})",
                    found.p.root.join(SWITCH_LOCK).display()
                ),
            ),
        }
    } else {
        (None, String::new())
    };
    if let Some(record) = off_recorded(&found.p.root, kept_off_record(&found.p).as_deref()) {
        return Ok(BrokerRenewal::Kept(format!(
            "off by your choice: `aterm fabric off` recorded it ({}); `aterm fabric on` turns it \
             on",
            record.display()
        )));
    }
    let hand = found.p.root.join(HAND_RECORD);
    if std::fs::symlink_metadata(&hand).is_ok() {
        return Ok(BrokerRenewal::Kept(format!(
            "you run the broker: `aterm fabric on --service none` recorded it ({}); `aterm \
             fabric on` hands it to launchd or systemd",
            hand.display()
        )));
    }
    let pid = found
        .view
        .pid
        .map_or_else(|| "-".to_string(), |pid| pid.to_string());
    let which = match (found.view.replaced.as_deref(), &found.answers) {
        (Some(exe), _) => format!(
            "the broker (pid {pid}) runs a replaced build ({} is another file now)",
            safe(exe, 256)
        ),
        (None, Ok(())) => {
            return Ok(BrokerRenewal::Current(format!(
                "the broker (pid {pid}) answers on {}, on the build on disk",
                safe(&found.view.endpoint, 256)
            )));
        }
        (None, Err(why)) => unserved(&found, why),
    };
    if let Some(why) = kept(&found) {
        return Ok(BrokerRenewal::Kept(format!("{which}; {why}")));
    }
    match renew {
        Renew::Look => Ok(BrokerRenewal::Due(which)),
        Renew::Apply => {
            step(&found).map(|said| BrokerRenewal::Renewed(format!("{said}{unlocked}")))
        }
    }
}

/// `found`'s socket, on which no broker answers (`why`), and what the job `on` installed is doing
/// meanwhile.
fn unserved(found: &Found, why: &str) -> String {
    let sock = safe(&found.view.endpoint, 256);
    let label = found.p.label();
    let job = match (found.service, &found.loaded, found.job) {
        (Service::None, ..) | (_, Err(_), _) => String::new(),
        (Service::Launchd, Ok(false), _) => {
            format!(", and launchd has not loaded {label}, the job `aterm fabric on` installed")
        }
        (Service::Launchd, Ok(true), None) => {
            format!(", and launchd holds {label} but runs no process of it")
        }
        (Service::Systemd, _, None) => format!(", and systemd --user {label} runs none"),
        (service, Ok(_), Some(pid)) => format!(
            ", though {} {label} runs pid {pid}",
            if service == Service::Systemd {
                "systemd --user"
            } else {
                "launchd"
            }
        ),
    };
    format!("no broker answers on {sock} ({}){job}", safe(why, 256))
}

/// Why `on`'s broker step leaves `found`'s broker as it is, where it does: no job of `on`'s, a
/// socket other than its root's, a job launchd has disabled (the person's choice) or cannot be
/// asked about here, or a broker that is not the process `on`'s job runs (one the person started
/// by hand, with that job not loaded or running another process), which the step would refuse
/// as unmanaged, or start the job over.
fn kept(found: &Found) -> Option<String> {
    if found.service == Service::None {
        return Some(format!(
            "no job of `aterm fabric on`'s keeps it (it runs as `--service none`): {} it yourself",
            if found.view.pid.is_some() {
                "restart"
            } else {
                "start"
            }
        ));
    }
    let sock = found.p.sock();
    if found.view.endpoint != sock {
        return Some(format!(
            "[fabric] command names {}, not its root's {sock}: `aterm fabric on` renews it",
            safe(&found.view.endpoint, 256)
        ));
    }
    let label = found.p.label();
    if found.disabled {
        return Some(if found.service == Service::Systemd {
            format!(
                "systemd --user has {label} disabled or masked, your choice: `systemctl --user \
                 enable {label}` (after `unmask`, where masked) turns it back on"
            )
        } else {
            format!(
                "launchd has {label} disabled (`launchctl disable`, or its switch in Login Items \
                 turned off), your choice: `launchctl enable gui/{}/{label}` (or that switch) \
                 turns it back on",
                uid()
            )
        });
    }
    if let Err(why) = &found.loaded {
        return Some(format!(
            "{why}, so nothing here loads {label}: `aterm fabric on` from a desktop login here does"
        ));
    }
    match found.view.pid {
        Some(pid) if Some(pid) != found.job => {
            let job = found.job.map_or_else(
                || "is not running".to_string(),
                |pid| format!("runs pid {pid}"),
            );
            Some(format!(
                "it is not the job `aterm fabric on` installed ({label} {job}): you run it, so \
                 restart it yourself, or stop it and run `aterm fabric on`"
            ))
        }
        _ => None,
    }
}

/// The root `command` points into, resolved as `on` resolves it (fleet, durability and node
/// kept), its broker named as `aterm fabric` names it (the process holding the socket and
/// whether an update replaced its build), the pid the job `on` installed runs now and whether a
/// person turned that job off in launchd or systemd, and whether a broker answers on the socket
/// ([`asked_twice`]; nothing is attached or read). A supervisor that does not say is an error in
/// `loaded`. The program is the one the installed job runs, not `on`'s (the `aterm` on `$PATH`):
/// the renewal restarts the job onto the file an update put at its path and never points it at
/// another aterm, so the plist `on` wrote stays as it is.
fn look_at(command: &[String]) -> Result<Found, String> {
    let cfg = fabric::bridge_config(command)?;
    let mut p = Paths::resolve(None)?;
    if let Some(root) = root_of(&cfg.broker) {
        p.root = root;
    }
    if let Some(fleet) = Role::of(&p).map_err(|e| e.to_string())?.fleet() {
        p.fleet = fleet.to_string();
    }
    p.durability = kept_durability(&p)?.0;
    let node = read_node(&p.state());
    let service = match installed_job(&p).map_err(|e| e.to_string())? {
        Some((service, argv)) => {
            if let Some(program) = argv.into_iter().next() {
                p.aterm = program;
            }
            service
        }
        None => Service::None,
    };
    let label = p.label();
    let (job, loaded, disabled) = match service {
        Service::Launchd => with_disabled(
            launchd_state(launchd_job(&label), &label),
            launchd_disabled(&label),
        ),
        Service::Systemd => systemd_state(systemd_unit(&label)),
        Service::None => (None, Ok(false), false),
    };
    let mut view = fabric::BrokerView {
        endpoint: cfg.broker,
        ..fabric::BrokerView::default()
    };
    view.locate();
    let answers = asked_twice(
        view.pid.is_some(),
        || fabric::answers(&view.endpoint),
        || std::thread::sleep(RETRY_PAUSE),
    );
    Ok(Found {
        p,
        node,
        service,
        job,
        loaded,
        disabled,
        answers,
        view,
    })
}

/// The pid launchd's `job` runs and whether it is loaded, or why that is not known.
fn launchd_state(job: LaunchdJob, label: &str) -> (Option<u32>, Result<bool, String>) {
    match job {
        LaunchdJob::Loaded(pid) => (pid, Ok(true)),
        LaunchdJob::Absent => (None, Ok(false)),
        LaunchdJob::NoGuiLogin => (
            None,
            Err(format!("launchd has no gui/{} domain in this login", uid())),
        ),
        LaunchdJob::Unread => (
            None,
            Err(format!(
                "`launchctl print` did not say whether launchd holds {label}"
            )),
        ),
    }
}

/// launchd's `state` of the job with whether a person disabled it: where `print-disabled` did not
/// say, the job reads as not known (`loaded` an error), never as enabled.
fn with_disabled(
    (job, loaded): (Option<u32>, Result<bool, String>),
    disabled: Result<bool, String>,
) -> (Option<u32>, Result<bool, String>, bool) {
    match disabled {
        Ok(disabled) => (job, loaded, disabled),
        Err(why) => (job, Err(why), false),
    }
}

/// The pid systemd's unit runs, whether it runs, and whether a person turned it off, from what
/// `systemctl --user show` said ([`systemd_unit`]).
fn systemd_state(
    read: Result<(Option<u32>, bool), String>,
) -> (Option<u32>, Result<bool, String>, bool) {
    match read {
        Ok((pid, disabled)) => (pid, Ok(pid.is_some()), disabled),
        Err(why) => (None, Err(why), false),
    }
}

/// Whether a broker answers, by `ask`: a socket a process holds (`held`) is asked once more,
/// after `pause`, before it reads as unanswered, so a broker busy past one `Hello`'s timeout is
/// not restarted for it.
fn asked_twice(
    held: bool,
    ask: impl Fn() -> Result<(), String>,
    pause: impl FnOnce(),
) -> Result<(), String> {
    match ask() {
        Err(first) if held => {
            pause();
            ask().map_err(|then| format!("{first}; asked again: {then}"))
        }
        answered => answered,
    }
}

/// `aterm fabric on`'s broker step over `found`, its lines kept: what it said, an error where it
/// failed.
fn step_on(found: &Found) -> Result<String, String> {
    let node = found.node.as_deref().ok_or_else(|| {
        format!(
            "{} holds no node id: `aterm fabric on` provisions one",
            found.p.state().display()
        )
    })?;
    let mut out = Out::kept();
    let ok = broker_step(&found.p, node, found.service, false, &mut out);
    if ok {
        Ok(out.said())
    } else {
        Err(out.said())
    }
}

struct Instance {
    pid: u32,
    sock: String,
    supervised: bool,
    state: String,
    /// `fabric status`'s `reason=`.
    reason: String,
    /// The argv `fabric status` names ([`fabric::status_argv`]).
    command: Option<Vec<String>>,
}

const STANDBY_NOTE: &str = "its bridge stands by: another instance's bridge holds this node's \
                            state dir, and this one carries no mail until that one exits";

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
        command: fabric::status_argv(h),
    })
}

fn arm_instances(argv: &[String], out: &mut Out) -> (Option<usize>, Vec<Instance>) {
    let ours = fabric::serve_flags(argv);
    let our_program = fabric::bridge_program(argv);
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
                        safe(
                            &inst
                                .command
                                .as_deref()
                                .map_or_else(|| "-".to_string(), fabric::shown_argv),
                            256
                        )
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
        let started = Instant::now();
        let (state, reason) = loop {
            let header = ctl
                .request("fabric status")
                .ok()
                .map(|r| r.header().to_string())
                .unwrap_or_default();
            let state = kv(&header, "state").unwrap_or("-").to_string();
            let reason = kv(&header, "reason").unwrap_or("-").to_string();
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

fn proof_skipped(running: Option<usize>) -> &'static str {
    if running == Some(0) {
        "skipped: no aterm is running — launch one, then run this again"
    } else {
        "skipped: no running aterm has a connected bridge for this command (see above)"
    }
}

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

fn sessions_of(inst: &Instance) -> Vec<String> {
    let Ok(mut ctl) = Ctl::open(&inst.sock) else {
        return Vec::new();
    };
    let _ = ctl.get_ref().set_read_timeout(Some(IO_TIMEOUT));
    session_sids(&mut ctl)
}

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

fn prove(armed: &[Instance], out: &mut Out) -> Result<u64, String> {
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
    // The post and the await each block up to the proof's deadline before they answer.
    let _ = ctl
        .get_ref()
        .set_read_timeout(Some(PROOF_DEADLINE + IO_TIMEOUT));
    let _ = ctl.get_ref().set_write_timeout(Some(IO_TIMEOUT));
    let ask = |ctl: &mut Ctl, line: &str| -> Result<crate::ctl::Reply, String> {
        let r = ctl.request(line).map_err(|e| format!("`{line}`: {e}"))?;
        if r.ok() {
            Ok(r)
        } else {
            Err(format!("`{line}` answered {}", safe(r.header(), 256)))
        }
    };
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
    let not_back = |why: &str| {
        format!(
            "landed at @{off} but was not delivered back within {PROOF_DEADLINE:?}: {why}; check \
             the cap file's `ro:/f/<F>/in/<node>/>` grant and `--accept-from`"
        )
    };
    let mut since = since;
    loop {
        let left = PROOF_DEADLINE
            .saturating_sub(started.elapsed())
            .as_millis()
            .max(1);
        let woke = ask(
            &mut ctl,
            &format!("@{sid} await inbox since={since} kinds=note timeout={left}"),
        )
        .map_err(|e| not_back(&e))?;
        // `OK timeout` is the await's deadline passing: an OK that delivered nothing.
        let Some(newest) = woke
            .header()
            .strip_prefix("OK inbox ")
            .and_then(|id| id.trim().parse::<u64>().ok())
        else {
            return Err(not_back(&format!(
                "the wait answered {}",
                safe(woke.header(), 128)
            )));
        };
        let rows = ask(
            &mut ctl,
            &format!("@{sid} inbox --peek --meta since={since}"),
        )?;
        let delivered = rows.rows().iter().any(|r| {
            r.starts_with("msg ")
                && kv(r, "off") == Some(&off.to_string())
                && kv(r, "kind") == Some("note")
        });
        if delivered {
            break;
        }
        // Another sender's note came first: wait past it for this one.
        since = since.max(newest);
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

/// What `on` resolves before its first step: the paths (fleet and durability kept from the root
/// and its installed job), and the tier that job names.
struct Prepared {
    p: Paths,
    job_durability: Option<crate::durability::Durability>,
    durability_job: Option<String>,
}

/// Resolve `on`'s paths for `opts`: the fleet the root already belongs to (a different
/// `--fleet` refused) and the durability its installed job names.
fn prepare_on(opts: &OnOpts) -> Result<Prepared, String> {
    let mut p = Paths::resolve(opts.fleet.as_deref())?;
    let role = Role::of(&p).map_err(|e| format!("cannot determine the existing role: {e}"))?;
    if let Some(fleet) = role.fleet() {
        if opts.fleet.as_deref().is_some_and(|asked| asked != fleet) {
            return Err(format!(
                "this root belongs to fleet {fleet}; run `aterm fabric off` before choosing \
                 another fleet"
            ));
        }
        p.fleet = fleet.to_string();
    }
    let (job_durability, durability_job) =
        kept_durability(&p).map_err(|e| format!("cannot read the installed broker job: {e}"))?;
    p.durability = opts.durability.or(job_durability);
    Ok(Prepared {
        p,
        job_durability,
        durability_job,
    })
}

/// `aterm fabric on`.
#[must_use]
pub fn on(opts: &OnOpts) -> ExitCode {
    let prepared = match prepare_on(opts) {
        Ok(prepared) => prepared,
        Err(e) => {
            eprintln!("aterm fabric on: {e}");
            return ExitCode::from(2);
        }
    };
    let service = opts.service.unwrap_or_else(Service::default_here);
    let mut out = Out::new(opts.dry_run);
    println!(
        "aterm fabric on — fleet {} · root {} · service {}{}",
        safe(&prepared.p.fleet, 64),
        prepared.p.root.display(),
        service.name(),
        if opts.dry_run {
            " · DRY RUN: every step printed, nothing touched"
        } else {
            ""
        }
    );
    on_prepared(&prepared, &mut out, SWITCH_WAIT, |out| {
        on_steps(&prepared, opts, service, out, Proof::Run)
    })
}

/// `aterm fabric on` once its paths are `prepared`: `steps` (its run of `on`'s steps) under
/// [`SWITCH_LOCK`], waited for at most `within`, so a turn-on or renewal the converge pass is
/// making, or another `on` or `off`, finishes first and this one lands after it whole.
fn on_prepared(
    prepared: &Prepared,
    out: &mut Out,
    within: Duration,
    steps: impl FnOnce(&mut Out) -> Result<Proved, ExitCode>,
) -> ExitCode {
    let _switch = match persons_switch(&prepared.p.root, "on", within, out) {
        Ok(lock) => lock,
        Err(code) => return code,
    };
    match steps(out) {
        Ok(proof) => {
            // The command is written: a turn-on the converge pass began and did not finish is
            // finished.
            if !out.dry_run {
                if let Err(why) = clear_default_record(&prepared.p.root) {
                    out.fail("record", &why);
                }
            }
            finish_report(&prepared.p, out, out.dry_run, proof)
        }
        Err(code) => code,
    }
}

/// Whether `on`'s steps end with the delivery proof (a note a session posts to itself, left in
/// its inbox), or skip it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Proof {
    Run,
    Skip,
}

/// How `on`'s proof went: whether it failed, and whether it ran.
#[derive(Debug, Clone, Copy)]
struct Proved {
    failed: bool,
    ran: bool,
}

/// Every step of `on` for `prepared` on `out`, from the socket check to the armed instances and,
/// where `proof` says, the proof: how the proof went, or the exit a step that stopped the run
/// ends it with (its line already said why).
fn on_steps(
    prepared: &Prepared,
    opts: &OnOpts,
    service: Service,
    out: &mut Out,
    proof: Proof,
) -> Result<Proved, ExitCode> {
    let Prepared {
        p,
        job_durability,
        durability_job,
    } = prepared;
    let job_durability = *job_durability;
    let kept_from = match (opts.durability, durability_job) {
        (None, Some(job)) => format!(", kept from {job}"),
        _ => String::new(),
    };
    match (p.tier(), crate::durability::resolve(job_durability, true)) {
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
                "ordered{kept_from} — {}; `--durability strict` returns to acks on the disk",
                crate::durability::describe_own(crate::durability::Durability::Ordered)
            ),
        ),
        (
            crate::durability::Durability::Strict,
            was @ (crate::durability::Durability::Relaxed | crate::durability::Durability::Ordered),
        ) => {
            out.note(
                "durability",
                &format!(
                    "strict, was {} — {}",
                    was.name(),
                    crate::durability::Durability::Strict.promise()
                ),
            );
        }
        _ => {}
    }

    let sock = p.sock();
    if sock.len() >= SUN_PATH_MAX {
        out.fail(
            "socket",
            &format!(
                "{sock} is {} bytes, over sun_path's {SUN_PATH_MAX}: the fabric root's path is \
                 too long",
                sock.len()
            ),
        );
        return Err(ExitCode::from(2));
    }
    let state = p.state();
    let checked = [
        sock.as_str(),
        &p.cap().to_string_lossy(),
        &state.to_string_lossy(),
        &p.program(),
    ]
    .into_iter()
    .try_for_each(argv_safe);
    if let Err(e) = checked {
        out.fail("argv", &e);
        return Err(ExitCode::from(2));
    }
    match binary_has_link(p) {
        Ok(()) => out.ok(
            "binary",
            &format!(
                "{} ({}serve + {}broker)",
                p.aterm,
                if p.is_link_shim() { "" } else { "link " },
                if p.is_link_shim() { "" } else { "link " },
            ),
        ),
        Err(e) => {
            out.fail("binary", &e);
            return Err(ExitCode::from(2));
        }
    }

    if !root_step(p, &state, out) || !record_step(p, service, out) || !program_step(p, out) {
        return Err(ExitCode::from(2));
    }

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
                    return Err(ExitCode::from(2));
                }
            };
            let file = state.join("node");
            let holds = std::fs::symlink_metadata(&file)
                .map_or_else(|e| e.kind() != io::ErrorKind::NotFound, |m| m.len() > 0);
            let would = if !holds {
                format!("provision a node id into {}", file.display())
            } else {
                let held = std::fs::read(&file)
                    .map(|b| safe(String::from_utf8_lossy(&b).trim(), 64))
                    .unwrap_or_default();
                format!(
                    "provision a node id into {}, keeping {} aside beside it",
                    file.display(),
                    if held.is_empty() {
                        "that file".to_string()
                    } else {
                        format!("what it holds (`{held}`)")
                    }
                )
            };
            if opts.dry_run {
                out.done("node", &would, "");
                minted
            } else {
                match record_node(&state, &minted) {
                    Ok((node, aside)) => {
                        let kept = aside.map_or_else(String::new, |to| {
                            format!(
                                " (what {} held is kept as {})",
                                file.display(),
                                to.display()
                            )
                        });
                        out.done("node", &would, &format!("provisioned {node}{kept}"));
                        node
                    }
                    Err(e) => {
                        out.fail("node", &e);
                        return Err(ExitCode::from(2));
                    }
                }
            }
        }
    };

    let secret = p.secret();
    let secret_shown = secret.to_string_lossy().into_owned();
    let read_secret = if opts.dry_run {
        crate::transport::read_mint_secret_narrowed
    } else {
        crate::transport::read_mint_secret
    };
    match read_secret(&secret_shown) {
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
            return Err(ExitCode::from(2));
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
                if let Err(e) = random_bytes(SECRET_LEN)
                    .and_then(|b| aterm_uds::private_file::write_private(&p.root, MINT_SECRET, &b))
                {
                    out.fail("secret", &format!("{}: {e}", secret.display()));
                    return Err(ExitCode::from(2));
                }
            }
        }
    }

    let cap = p.cap();
    let cap_ok = if cap.exists() {
        read_secret(&secret_shown)
            .map_err(|e| CapMiss::from(crate::transport::describe(&secret_shown, &e)))
            .and_then(|key| cap_is_for(&cap, &p.fleet, &node, &key))
    } else {
        Err(CapMiss::from("absent".to_string()))
    };
    match cap_ok {
        Ok(n) => out.already("cap", &format!("{} ({n} grants for {node})", cap.display())),
        Err(CapMiss { why, keep }) => {
            let mint = |kept: &str| {
                format!(
                    "mint 8 grants for {node} into {} ({why}{kept})",
                    cap.display()
                )
            };
            let would = mint(if keep {
                "; it would be kept aside beside it"
            } else {
                ""
            });
            out.done("cap", &would, &mint(""));
            if !opts.dry_run {
                let aside = if keep {
                    match crate::state::keep_aside(&cap) {
                        Ok(to) => Some(to),
                        Err(e) => {
                            out.fail("cap", &e.to_string());
                            return Err(ExitCode::FAILURE);
                        }
                    }
                } else {
                    None
                };
                match mint_caps(p, &node) {
                    Ok(n) => {
                        out.ok(
                            "cap",
                            &format!("minted {n} grants for {node} into {}", cap.display()),
                        );
                        if let Some(to) = aside {
                            out.ok("cap", &format!("kept the old ring as {}", to.display()));
                        }
                    }
                    Err(e) => {
                        if let Some(to) = &aside {
                            crate::state::take_back_aside(&cap, to);
                        }
                        out.fail("cap", &e);
                        return Err(ExitCode::FAILURE);
                    }
                }
            }
        }
    }

    let broker_ok = broker_step(p, &node, service, opts.durability.is_some(), out);
    if !broker_ok && !opts.dry_run {
        return Err(ExitCode::FAILURE);
    }
    let argv = p.bridge_command(&node);
    finish(p, &node, &argv, out, opts.dry_run, proof)
}

/// `on`'s steps after the broker: `[fabric] command`, the rendezvous file, the armed instances
/// and, where `proof` says, the proof.
fn finish(
    p: &Paths,
    node: &str,
    argv: &[String],
    out: &mut Out,
    dry_run: bool,
    proof: Proof,
) -> Result<Proved, ExitCode> {
    let node = node.to_string();
    let state = p.state();
    let sock = p.sock();
    let cap = p.cap();
    if !config_step(p.config.as_deref(), argv, out) {
        return Err(ExitCode::FAILURE);
    }

    let rendezvous = Rendezvous {
        fleet: p.fleet.clone(),
        broker: sock.clone(),
        node: node.clone(),
        cap_file: cap.to_string_lossy().into_owned(),
        state: state.to_string_lossy().into_owned(),
        command: argv.to_vec(),
    };
    match p.rendezvous.clone() {
        None => out.fail(
            "rendezvous",
            "no control-socket dir: neither $XDG_RUNTIME_DIR nor $HOME is set",
        ),
        Some(path) => {
            let want = rendezvous.render();
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

    let (running, armed) = arm_instances(argv, out);

    Ok(if proof == Proof::Skip {
        out.note(
            "proof",
            "skipped: it would post a note into a session's inbox that nobody asked for \
             (`aterm fabric on` proves delivery; `aterm fabric` reports the broker)",
        );
        Proved {
            failed: false,
            ran: false,
        }
    } else if armed.is_empty() {
        out.note("proof", proof_skipped(running));
        Proved {
            failed: false,
            ran: false,
        }
    } else {
        match prove(&armed, out) {
            Ok(_) => Proved {
                failed: false,
                ran: true,
            },
            Err(e) => {
                out.fail("proof", &e);
                Proved {
                    failed: true,
                    ran: true,
                }
            }
        }
    })
}

/// What `on` prints after its steps: the undo command, the summary, and the status report; the
/// exit, a failed step or proof failing it.
fn finish_report(p: &Paths, out: &mut Out, dry_run: bool, proof: Proved) -> ExitCode {
    let Proved {
        failed: proof_failed,
        ran: proof_ran,
    } = proof;
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
            "\nnothing changed: the fabric was already on, and the proof {}.",
            if proof_ran {
                "ran again"
            } else {
                "was skipped"
            }
        );
    }

    println!();
    match fabric::gather() {
        Ok(report) => {
            print!("{}", fabric::render_text(&report));
            if proof_failed || out.failed {
                ExitCode::FAILURE
            } else {
                if let Some(line) = done_despite(report.warnings.len()) {
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

fn done_despite(warnings: usize) -> Option<String> {
    match warnings {
        0 => None,
        1 => Some("on: done; the warning above is not this run's".to_string()),
        n => Some(format!(
            "on: done; the {n} warnings above are not this run's"
        )),
    }
}

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
    said_replaced(
        out,
        config,
        file.lock().and_then(|edit| edit.replace(&new_text)),
        |saved| format!("{headers} removed from {}{saved}", config.display()),
    );
}

/// `aterm fabric off`'s launchd step: boot `label` out of the GUI domain and remove its `plist`.
/// A `print` that said nothing ([`LaunchdJob::Unread`]) is read as a job launchd may hold: the
/// bootout runs, and its failure stands unless `print` then says launchd holds no such job.
fn off_launchd(lc: &dyn Launchd, label: &str, plist: &Path, out: &mut Out) {
    let job = lc.job(label);
    let held = matches!(job, LaunchdJob::Loaded(_) | LaunchdJob::Unread);
    if !held && !plist.exists() {
        out.already("broker", &format!("launchd {label} is not installed"));
        return;
    }
    out.done(
        "broker",
        &format!("bootout launchd {label} and remove {}", plist.display()),
        &format!("launchd {label} stopped and {} removed", plist.display()),
    );
    if out.dry_run {
        return;
    }
    // By domain, with no legacy `unload` fallback: that acts on the caller's domain and exits 0
    // whatever it did.
    if held {
        let domain = format!("gui/{}/{label}", uid());
        if let Err(e) = lc.run(&["bootout", &domain]) {
            if !(job == LaunchdJob::Unread && lc.job(label) == LaunchdJob::Absent) {
                out.fail("broker", &e);
            }
        }
    }
    if let Err(e) = std::fs::remove_file(plist) {
        if e.kind() != io::ErrorKind::NotFound {
            out.fail("broker", &format!("{}: {e}", plist.display()));
        }
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
    for (pid, sock, sid, origin) in held_sessions() {
        let shown = origin
            .as_deref()
            .map_or_else(|| "?".to_string(), |o| safe(o, 24));
        let slots = HoldSlots::of(origin.as_deref().unwrap_or(""));
        let lift = lift_hold(&sid, pid, sock.as_deref());
        let mut remedies: Vec<String> = Vec::new();
        if slots.person {
            remedies.push(format!(
                "the person at the window took it over (Take Over): their Hand Back there lifts \
                 it, and so does a deliberate quit and relaunch of instance {pid} (a cold launch \
                 restores no hold; a crash the PTY keeper recovers keeps it)"
            ));
        }
        if slots.fleet || !(slots.person || slots.local) {
            remedies.push(format!(
                "relaunch instance {pid} after this, which frees it while the fabric stays off \
                 (the bus keeps the halt row, and the next `fabric on` holds it again); `aterm \
                 fabric halt --cap-file <the halter's cap> off` withdraws the halt itself"
            ));
        }
        if slots.local {
            remedies.push(if remedies.is_empty() {
                format!("{lift} does")
            } else {
                format!("the owner's local hold beside it stays until {lift}")
            });
        }
        let detail = format!(
            "@{sid} (instance {pid}) is HELD (hold=1 origin={shown}); off does not lift it — {}",
            remedies.join("; ")
        );
        out.warn("held", &detail);
    }
    // A turn-on or renewal the converge pass is making, or another `on` or `off`, finishes
    // first, so this off lands after it whole.
    let _switch = match persons_switch(&p.root, "off", SWITCH_WAIT, &mut out) {
        Ok(lock) => lock,
        Err(code) => return code,
    };
    off_record_step(&p, &mut out);
    let label = p.label();
    match service {
        Service::Launchd => off_launchd(&LiveLaunchd, &label, &p.plist(), &mut out),
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

    let kept: Vec<&str> = [("mint.secret", p.secret()), ("node.cap", p.cap())]
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
            format!("none in {}", p.root.display())
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

fn protocol_remedy(again: &str) -> String {
    format!(
        "the broker and this aterm are different releases speaking different astream \
         protocols: {again} restarts the broker from the aterm on PATH"
    )
}

fn renew_remedy(again: &str) -> String {
    format!(
        "this node's cap has lapsed (its grants' `exp=` passed) and the broker refuses it: the \
         ring `on` mints never lapses, so this one was minted by hand — move <root>/node.cap \
         aside and run {again}, which mints this node's ring again under the root's secret; the \
         running bridge attaches it at its next redial (it reads its cap file again before every \
         dial), with no restart"
    )
}

const POISONED_LOG_REMEDY: &str = "the broker's log is poisoned and refuses every write until \
     it is reopened, which a broker of this build does itself: it exits within seconds and its \
     supervisor starts it again. One that stays poisoned predates this build: stop it (`kill \
     <pid>`, the broker pid `aterm fabric --json` names) and its supervisor starts it on the \
     reopened log (one you run by hand, start again yourself); if it poisons again at once, the \
     disk under its log is failing flushes";

fn quoted_broker_fix(w: &str, again: &str) -> Option<String> {
    let s = |t: &str| w.contains(t);
    if !s("supervisor's broker.err") {
        return None;
    }
    Some(if s(crate::cli::NEWER_LOG_REMEDY) {
        format!(
            "the broker's log holds records in a format newer than this aterm reads: install \
             the aterm release that wrote them (or a later one), then {again} restarts the \
             broker. No repair applies, and the log is untouched"
        )
    } else if s(&format!(": {}", crate::broker_health::POISONED_EXIT_WORDS)) {
        // The broker left its poisoned log ([`crate::broker_health::poisoned_line`],
        // whose opening words these are — they survive the quote's cut, which
        // astream's reason at the line's end does not), and it is not back.
        if s(crate::broker_health::UNSUPERVISED_WORDS) {
            "the broker exited over a poisoned log, and no `aterm fabric on` job runs it: unless \
             something else restarts it, start it again with the command its line names (the \
             whole line is the last of broker.err); if it poisons again at once, the disk under \
             its log is failing flushes"
                .to_string()
        } else {
            format!(
                "the broker exited over a poisoned log for its supervisor to reopen, and it is \
                 not back yet: launchd starts it again within ten seconds, systemd within one; \
                 if it stays down, {again} starts it again; if it poisons again at once, the \
                 disk under its log is failing flushes"
            )
        }
    } else if s(crate::cli::CORRUPT_LOG_REMEDY) {
        format!(
            "the broker refuses its log for a corrupt record, and restarting it meets the same \
             refusal: run the `aterm link broker --repair <log>` the quoted line names — it \
             truncates the log at that record, dropping the records from there on (possibly \
             acked mail) and printing how many bytes went — then {again} restarts the broker"
        )
    } else {
        format!(
            "the broker's own last word is quoted: when it says why the broker cannot run (a log \
             it refuses to open, an address in use, a file it cannot read), fix that first, then \
             {again} restarts it — a restart alone meets the same refusal"
        )
    })
}

/// The fix for one of `aterm fabric`'s warnings, matched on the words the warning is built from.
#[must_use]
pub fn fix_for(warning: &str, role: &Role) -> String {
    let w = warning;
    let s = |t: &str| w.contains(t);
    let again = role.again();
    let quoted = quoted_broker_fix(w, &again);
    if s("speaks astream protocol") {
        protocol_remedy(&again)
    } else if s(fabric::CAP_LAPSED) {
        renew_remedy(&again)
    } else if s("does not answer") && quoted.is_some() {
        quoted.unwrap_or_default()
    } else if s("does not answer") && s("no record lands")
        || s("does not answer this report's own probe")
    {
        format!("start the broker: {again}")
    } else if s("runs a replaced build") {
        format!(
            "{again} restarts it on the build on disk (a broker of this build or later \
             restarts itself within a minute of its executable being replaced)"
        )
    } else if s(fabric::POISONED_WARNING) {
        POISONED_LOG_REMEDY.to_string()
    } else if s(fabric::FLUSH_FAILED_WARNING) {
        "check the disk under the broker's log (Disk Utility's First Aid, or the system log's \
         I/O errors). A failed flush that put acked records at risk poisons the log, which a \
         broker of this build then exits to reopen: run `aterm fabric` again to see whether \
         this one did. The count starts again at zero when the broker next opens its log"
            .to_string()
    } else if s("checks no capability") {
        format!(
            "{again} rewrites the broker's job and restarts it guarded with the root's mint \
             secret (a broker run by hand: restart it with `--secret-file <root>/mint.secret`)"
        )
    } else if s("has not reported its broker link") {
        format!(
            "nothing, if its mail moves (`aterm ctl @<sid> post to=@<sid> kind=note --wait=5000 \
             ping` lands); if it does not, check the broker: {again}"
        )
    } else if s("broker link is down") {
        let reason = w
            .split("reason=")
            .nth(1)
            .and_then(|r| r.split([',', ' ', ')']).next())
            .unwrap_or("");
        let cause = match reason {
            "no-socket" | "refused" => quoted.unwrap_or_else(|| {
                format!("nothing listens at the broker's socket: {again} starts it")
            }),
            "no-ack" => format!("the broker accepts and never answers: {again} restarts it"),
            "expired" => renew_remedy(&again),
            "poisoned" => POISONED_LOG_REMEDY.to_string(),
            "attach" => format!(
                "the broker refuses this node's cap: move <root>/node.cap aside and run {again}"
            ),
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
            cut if cut.starts_with("cut-") => {
                "the broker is up and one subscription (the face after `cut-`) ended: the broker \
                 ended it, as astream does to a reader that leaves its socket unread past its 30 s \
                 stream write bound (a bridge stopped or starved that long); or it read nothing \
                 for its 30 s live read deadline (a broker stalled that long, behind a slow disk \
                 say); or the link lost that one connection. The bridge's line in the log names \
                 which end it saw — nothing on the broker needs a restart"
                    .to_string()
            }
            "protocol" => protocol_remedy(&again),
            "endpoint-grammar" => "the bridge is a build this aterm cannot serve (it lacks a \
                                   verb the bridge needs): `[fabric] command` names another \
                                   aterm than this instance's; quit and relaunch this aterm, or \
                                   point [fabric] command at its build"
                .to_string(),
            _ => format!("check the broker: {again}"),
        };
        format!("{cause}; the bridge then reconnects on its own")
    } else if s("bus could not be read") || s("could not be read:") {
        format!(
            "the cap file's grants do not cover this fleet, or the broker died mid-read: {again} \
             re-mints the root's cap when it was minted for another fleet or node id, and probes \
             again; it leaves a ring minted under the root's secret for this fleet and node id as \
             it is, so first move <root>/node.cap aside when it was minted by hand and lacks a \
             grant"
        )
    } else if s("no [fabric] command") {
        format!(
            "{again} writes [fabric] command (and the rendezvous file) so the next launch starts \
             a bridge on its own"
        )
    } else if s("no node id") {
        format!("{again} provisions one and mints the cap for it")
    } else if s("cap file") {
        format!("{again} re-mints the cap file for the node id in the state dir")
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
        let lift = lift_hold_in(w);
        let origin = w
            .split_whitespace()
            .find_map(|t| t.strip_prefix("origin="))
            .map(|o| o.trim_end_matches([')', ':', ';']));
        let slots = HoldSlots::of(origin.unwrap_or(""));
        let lost = s("reason=fabric-lost");
        let mut remedies: Vec<String> = Vec::new();
        if slots.person {
            remedies.push(PERSON_HOLD_LIFT.to_string());
        }
        if slots.remote {
            remedies.push(format!(
                "{REMOTE_HOLD_LIFT}, and once its place answers under another fleet's secret or \
                 key, so does {lift}"
            ));
        }
        if slots.fleet {
            remedies.push(
                match (lost, remedies.is_empty() && !slots.local) {
                    (true, true) => "it lifts when the bridge reconnects",
                    (true, false) => "the fleet's lifts when the bridge reconnects",
                    (false, _) => FLEET_HALT_LIFT,
                }
                .to_string(),
            );
        }
        if slots.local {
            remedies.push(if remedies.is_empty() {
                lift.clone()
            } else {
                format!("the owner's local hold beside it lifts with {lift}")
            });
        }
        if !remedies.is_empty() {
            remedies.join("; ")
        } else {
            format!(
                "a local hold lifts with {lift}; a fleet hold lifts when a bridge reconnects \
                 (reason=fabric-lost) or when the person who set it runs `aterm fabric halt \
                 --cap-file <their cap> off`"
            )
        }
    } else if s(fabric::RETENTION_GAVE_UP) {
        format!(
            "nothing brings those records back: aterm's broker rules keep unread mail {days} \
             days within {} MiB of mail lanes and every other record (a broadcast too) {days} \
             days within {} MiB, and let read mail go with the bulk stream's cut, so a bridge \
             kept away that long, mail a session left unhandled across its restart, or a topic \
             followed from far back lost what went. Each session told names the range (`aterm \
             ctl @<sid> inbox --peek --meta`, `topic ls`) and marks it in its log (`aterm ctl \
             @<sid> log kinds=mail-gap`): ask the senders again where it matters, and `inbox \
             seen` what is handled. The node's line lasts while the instance runs, a session's \
             while it names the range",
            crate::broker_rules::MAIL_BYTES >> 20,
            crate::broker_rules::DEFAULT_BYTES >> 20,
            days = crate::broker_rules::MAX_AGE.as_secs() / 86_400,
        )
    } else if s("lost") && s("evicted") {
        "read mail sooner (`aterm ctl @<sid> await inbox since=<id>`) and `inbox seen` what is \
         handled; the evicted records are still on the bus"
            .to_string()
    } else if s(fabric::OVERDUE_UNJUDGED) {
        format!(
            "nothing on the bus is broken: the broker let a reply on the asker's lane go, so \
             status cannot tell whether it answered these asks. The asker's inbox shows the \
             answers it had, `re=` naming the ask's offset (`aterm ctl @<sid> inbox --peek` for \
             a session, `aterm fabric inbox --cap-file <their cap> --peek` for a person); an ask \
             still owed one is answered by whoever it was sent to, as an overdue one is. Nothing \
             else clears the count: it falls as the broker lets those asks go (unread mail \
             within {days} days) or the bus moves {span} records past them",
            days = crate::broker_rules::MAX_AGE.as_secs() / 86_400,
            span = fabric::OVERDUE_SCAN_SPAN,
        )
    } else if s("passed its deadline") && s("@p passed") {
        "a person was asked: `aterm fabric inbox --cap-file <their cap>` shows it, and `aterm \
         fabric post --cap-file <their cap> to=@<sid>@<node> kind=answer re=<off> <text>` \
         answers it (`aterm link notify --on ask:<person>` pages them next time)"
            .to_string()
    } else if s("passed its deadline") {
        "the session it was sent to has not answered: `aterm ctl @<sid> inbox` shows it, then \
         `inbox seen <id> handled` (or `refused`)"
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
        "`aterm fabric doctor --retire-ghosts` lists the row; `--yes` publishes `exited` for it \
         now (a bridge on the node's own state dir retires a row its own earlier runs in \
         this boot left after five minutes, and one a copy of the state dir left only once its \
         attach finds that copy's writer-marked will on the bus; unmarked rows need the doctor)"
            .to_string()
    } else if s("fleet halt standing") || s("stands on this node with no row on the bus") {
        FLEET_HALT_LIFT.to_string()
    } else if s("distinct subjects one node may hold at once") {
        format!(
            "close the sessions you do not need: five minutes after one leaves, its bridge gives \
             its presence row and the lanes it was mailed on back. If what stays holds the \
             budget (live sessions, and a subject for each topic a session ever broadcast on), \
             move <root>/link-state/node aside and run {again}: it provisions a new \
             node id, re-mints the cap for it and writes the new [fabric] command; then quit \
             and reopen each aterm instance (`on` names every one still armed with the old \
             command), whose bridge starts on the new id with a fresh budget (a killed bridge \
             would come back on the command its aterm armed at launch); mail still queued for \
             the old id is abandoned with it"
        )
    } else if s("presence roster has more than") && s("truncated") {
        "nothing on the bus is wrong: one roster read takes at most that many rows and every \
         session keeps one until five minutes after it leaves; a session past the cut is still \
         listed by `aterm ctl ls`"
            .to_string()
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

/// What lifts a fleet halt, in the one sentence every surface that reports one uses.
pub const FLEET_HALT_LIFT: &str = "the person who set it withdraws it: `aterm fabric halt \
     --cap-file <their cap> off` (a cap from `aterm fabric mint-for h-<name>`). `aterm fabric off` \
     and a relaunch free a session only until the next `fabric on` re-reads the halt row";

/// What lifts the person's hold (`origin=person`, the window's Take Over).
pub const PERSON_HOLD_LIFT: &str = "the person at the window took it over (Take Over): their \
     Hand Back there lifts it, and no command does";

/// What lifts another Mac's hold (`origin=remote`, the step-in).
pub const REMOTE_HOLD_LIFT: &str = "another Mac holds it over the fleet: that Mac's `aterm ctl \
     --host <this Mac> hold <sid> off` lifts it, or the person here takes it with Take Over and \
     hands it back, or lowers `aterm fleet allow` here; once that Mac left the fleet, `aterm \
     fleet peers rm <its node>` (the survey's `held_from=`) lifts it too, run here where no \
     \"*\" grant stands here (the rm ends a grant here that names the node), and under the \
     \"*\" grant run on the fleet's issuer, once this Mac reads that book again (`aterm fleet \
     peers` reads it now)";

/// Which hold slots an `origin=` / `hold_origin=` list names (`person`,
/// `remote`, `fleet`, `local`, comma-joined), each read as a whole word.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct HoldSlots {
    person: bool,
    remote: bool,
    fleet: bool,
    local: bool,
}

impl HoldSlots {
    fn of(origins: &str) -> Self {
        let has = |slot: &str| origins.split(',').any(|o| o == slot);
        Self {
            person: has("person"),
            remote: has("remote"),
            fleet: has("fleet"),
            local: has("local"),
        }
    }
}

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

fn arm_command(pid: u32, sock: &str, argv: &[String]) -> String {
    let reach = instance_flag(pid, (!aterm_ctl::pid_reaches(pid, sock)).then_some(sock));
    format!(
        "arm {pid} (`aterm ctl {reach} fabric attach {}`)",
        attach_words(argv)
    )
}

fn attach_words(argv: &[String]) -> String {
    argv.iter()
        .map(|w| fabric::shell_word(&aterm_codec::pct::encode_word(w)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn lift_hold(sid: &str, pid: u32, sock: Option<&str>) -> String {
    let sid = if plain_arg(sid, b"") { sid } else { "<sid>" };
    format!("`aterm ctl {} hold {sid} off`", instance_flag(pid, sock))
}

fn plain_arg(t: &str, more: &[u8]) -> bool {
    !t.is_empty()
        && t.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.') || more.contains(&b)
        })
}

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

fn retire_ghosts_action(yes: bool) -> ExitCode {
    let report = match fabric::gather() {
        Ok(r) => r,
        Err(off) => {
            println!("{}", off.message());
            return ExitCode::from(2);
        }
    };
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
    let mut by_node: BTreeMap<&str, Vec<(String, String)>> = BTreeMap::new();
    for (sid, node) in &ghosts {
        by_node
            .entry(node.as_str())
            .or_default()
            .push((sid.clone(), node.clone()));
    }
    if !yes && !confirmed() {
        for node in by_node.keys() {
            match publisher_for(&report, node) {
                Publisher::Bridge(pid) => println!(
                    "  note: with --yes, instance {pid}'s bridge is asked to publish {}'s",
                    safe(node, 64)
                ),
                Publisher::Itself => {}
                Publisher::Nobody => println!("  note: {}", nobody_publishes(node)),
            }
        }
        println!("  nothing published (dry). Re-run with --yes, or answer y.");
        return ExitCode::SUCCESS;
    }
    let mut all = true;
    for (node, rows) in &by_node {
        all &= match publisher_for(&report, node) {
            Publisher::Bridge(pid) => retire_through_bridge(&report, pid, node, rows),
            Publisher::Itself => match fabric::publish_exited(&report, rows) {
                Ok(n) => {
                    println!("  published `exited` for {n} row(s)");
                    true
                }
                Err(e) => {
                    println!("  could not publish: {e}");
                    false
                }
            },
            Publisher::Nobody => {
                for (sid, _) in rows {
                    println!(
                        "  @{}: not retired: {}",
                        safe(sid, 64),
                        nobody_publishes(node)
                    );
                }
                false
            }
        };
    }
    if all {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    }
}

/// Who may publish `exited` over a row of `node` ([`publisher_for`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Publisher {
    /// The running bridge of the instance at this pid, which answers as the node.
    Bridge(u32),
    /// This command: the node is its state dir's, and no bridge of it is up.
    Itself,
    /// Nothing here: no bridge of the node is up, and this command's state
    /// dir is another node's.
    Nobody,
}

fn publisher_for(report: &fabric::Report, node: &str) -> Publisher {
    match running_bridge(&report.instances, node) {
        Some(pid) => Publisher::Bridge(pid),
        None if report.node.as_deref() == Some(node) => Publisher::Itself,
        None => Publisher::Nobody,
    }
}

fn nobody_publishes(node: &str) -> String {
    format!(
        "no bridge of {} is up here and this command's state dir is another node's: run it \
         under a [fabric] command naming {}'s own state dir and cap",
        safe(node, 64),
        safe(node, 64)
    )
}

fn retire_through_bridge(
    report: &fabric::Report,
    pid: u32,
    node: &str,
    ghosts: &[(String, String)],
) -> bool {
    let Some(instance) = report.instances.iter().find(|i| i.pid == pid) else {
        println!("  could not find the instance at pid {pid} in the report");
        return false;
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
            return false;
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
        return false;
    }
    println!("  asked the bridge at pid {pid}: {}", safe(&header, 80));
    let mut done = 0;
    let deadline = std::time::Instant::now() + RETIRE_WAIT;
    let mut pending: Vec<&str> = sids.clone();
    let since = report.broker.head;
    let mut refused = BTreeMap::new();
    let mut refusals_read = false;
    while !pending.is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(200));
        let Ok(progress) = fabric::retire_progress(report, node, &pending, since) else {
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
    pending.is_empty() && refused.is_empty()
}

const RETIRE_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

fn running_bridge(instances: &[fabric::InstanceView], node: &str) -> Option<u32> {
    instances
        .iter()
        .find(|i| {
            i.node.as_deref() == Some(node)
                && i.reason.as_deref() != Some("state-locked")
                && (!i.bridge_pids.is_empty()
                    || i.supervised == Some(true)
                    || matches!(i.fabric.as_deref(), Some("connected" | "stalled" | "stale")))
        })
        .map(|i| i.pid)
}

fn confirmed() -> bool {
    use std::io::{IsTerminal as _, Write as _};
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
            if let Some(d) = report.broker.tier() {
                println!(
                    "  durability: {} — {}",
                    d.name(),
                    crate::durability::describe_served(
                        d,
                        report.broker.served,
                        report.broker.guarded
                    )
                );
            }
            if let Some(row) = fabric::health_row(&report.broker) {
                println!("  health: {row}");
            }
            if let Some(row) = fabric::holds_row(&report.broker, report.now_ms) {
                println!("  holds: {row}");
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

    /// A [`Found`] of a root under a path nothing holds, its broker (pid 4242) at the root's own
    /// socket, kept by `service`'s job (which runs it), an update having replaced its build
    /// where `replaced` says.
    fn found(service: Service, replaced: Option<&str>) -> Found {
        let p = Paths {
            home: PathBuf::from("/nonexistent/home"),
            login_home: None,
            root: PathBuf::from("/nonexistent/home/.local/share/aterm-fabric"),
            aterm: "/nonexistent/bin/aterm".to_string(),
            fleet: DEFAULT_FLEET.to_string(),
            config: None,
            rendezvous: None,
            durability: None,
        };
        Found {
            view: fabric::BrokerView {
                endpoint: p.sock(),
                pid: Some(4242),
                replaced: replaced.map(str::to_string),
                ..fabric::BrokerView::default()
            },
            p,
            node: Some("n-0123456789abcdef".to_string()),
            job: (service != Service::None).then_some(4242),
            loaded: Ok(service != Service::None),
            disabled: false,
            answers: Ok(()),
            service,
        }
    }

    /// A root `p` under `dir`, nothing configured.
    fn scratch_paths(dir: &Path) -> Paths {
        Paths {
            home: dir.to_path_buf(),
            login_home: None,
            root: dir.join("root"),
            aterm: "/nonexistent/bin/aterm".to_string(),
            fleet: DEFAULT_FLEET.to_string(),
            config: None,
            rendezvous: None,
            durability: None,
        }
    }

    /// `aterm fabric off`'s record is written once (a second off says it stands already), a
    /// configured command reads on over it, and `aterm fabric on` removes it and says nothing
    /// where there is none.
    /// NEGATIVE CONTROL: an `unset_in` that reads the record before the command reads a
    /// configured command as the person's off.
    #[test]
    fn an_off_record_is_written_once_and_an_on_removes_it() {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let p = scratch_paths(dir.path());
        let mut out = Out::kept();
        off_record_step(&p, &mut out);
        assert!(!out.failed, "{}", out.said());
        assert!(p.root.join(OFF_RECORD).exists(), "off records it");
        assert_eq!(
            unset_in(&p.root, None, true),
            None,
            "a command still reads on"
        );
        let mut out = Out::kept();
        off_record_step(&p, &mut out);
        assert!(out.said().contains("already"), "{}", out.said());
        let mut out = Out::kept();
        assert!(
            record_step(&p, Service::Launchd, &mut out),
            "{}",
            out.said()
        );
        assert!(!p.root.join(OFF_RECORD).exists(), "on removes it");
        let mut out = Out::kept();
        assert!(record_step(&p, Service::Launchd, &mut out));
        assert!(out.said().is_empty(), "nothing said with no record");
    }

    /// AN OFF OUTLIVES ITS FABRIC ROOT: `aterm fabric off` keeps its record beside aterm.toml too,
    /// so a person who deletes the fabric root after an off (it keeps the node id, the secret, the
    /// grants and the bus log) still reads off, and the converge pass leaves it; `aterm fabric on`
    /// removes both records. aterm.toml's folder keeps its mode (the record is empty).
    /// NEGATIVE CONTROL: with `off_recorded` reading the root's record alone, the deleted root
    /// reads as never turned off and the pass turns the fabric on over the person's off.
    #[test]
    fn an_off_outlives_the_fabric_root_it_was_recorded_in() {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let config_dir = dir.path().join("cfg/aterm");
        std::fs::create_dir_all(&config_dir).expect("aterm.toml's folder");
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            let mode = || {
                std::fs::metadata(&config_dir)
                    .expect("mode")
                    .permissions()
                    .mode()
            };
            std::fs::set_permissions(&config_dir, std::fs::Permissions::from_mode(0o755))
                .expect("0755");
            move || mode() & 0o777
        };
        let p = Paths {
            config: Some(config_dir.join("aterm.toml")),
            ..scratch_paths(dir.path())
        };
        let kept = kept_off_record(&p).expect("beside aterm.toml");
        let mut out = Out::kept();
        off_record_step(&p, &mut out);
        assert!(!out.failed, "{}", out.said());
        assert!(p.root.join(OFF_RECORD).exists(), "in the root");
        assert!(kept.exists(), "beside aterm.toml");
        #[cfg(unix)]
        assert_eq!(mode(), 0o755, "aterm.toml's folder keeps its mode");
        std::fs::remove_dir_all(&p.root).expect("the person tidies the root away");
        let off = unset_in(&p.root, Some(&kept), false);
        assert!(
            matches!(&off, Some(Unset::PersonOff(why)) if why.contains(&*kept.to_string_lossy())),
            "{off:?}"
        );
        let left = by_default_in(&p.root, Some(&kept), &|| Ok(false), |_| {
            panic!("no steps over their off")
        });
        assert!(matches!(left, Ok(ByDefault::Left(_))), "{left:?}");
        let mut out = Out::kept();
        off_record_step(&p, &mut out);
        assert!(!out.failed, "{}", out.said());
        let mut out = Out::kept();
        assert!(
            record_step(&p, Service::Launchd, &mut out),
            "{}",
            out.said()
        );
        assert!(!p.root.join(OFF_RECORD).exists(), "on removes the root's");
        assert!(!kept.exists(), "on removes the one beside aterm.toml");
        assert!(
            matches!(unset_in(&p.root, Some(&kept), false), Some(Unset::Never(_))),
            "on again: the pass may turn it on"
        );
    }

    /// A root an earlier `on` left without a node id (one that predates it, or set up by hand)
    /// still reads as the person's off where it keeps the mint secret or the node grants.
    /// NEGATIVE CONTROL: a read that looks for the node id alone turns such a root on over an off.
    #[test]
    fn a_root_with_no_node_id_that_an_earlier_on_left_reads_as_the_persons_off() {
        for kept in [MINT_SECRET, "node.cap"] {
            let dir = aterm_tempfile::tempdir().expect("scratch");
            let root = dir.path().join("root");
            std::fs::create_dir_all(root.join("link-state")).expect("state");
            std::fs::write(root.join("link-state/lock"), b"").expect("lock");
            assert!(
                matches!(unset_in(&root, None, false), Some(Unset::Never(_))),
                "{kept}: state alone is no off"
            );
            std::fs::write(root.join(kept), b"inert").expect("kept");
            let read = unset_in(&root, None, false);
            assert!(
                matches!(&read, Some(Unset::PersonOff(why)) if why.contains(kept)),
                "{kept}: {read:?}"
            );
            std::fs::write(root.join(DEFAULT_RECORD), b"").expect("by default");
            assert!(
                matches!(unset_in(&root, None, false), Some(Unset::Never(_))),
                "{kept}: a root the pass began turning on"
            );
        }
    }

    /// The node id an `on`'s steps write, in `root`.
    fn node_written(root: &Path) {
        std::fs::create_dir_all(root.join("link-state")).expect("state");
        std::fs::write(root.join("link-state/node"), "n-0123456789abcdef\n").expect("node");
    }

    /// The converge pass's turn-on, its steps a stand-in for `on`'s: the record stands while they
    /// run and goes once they finished, so a command the person clears after reads as their off;
    /// steps that fail leave it, so the root reads as never turned off and the next pass finishes
    /// it.
    /// NEGATIVE CONTROL: a turn-on that leaves no record before its steps reads its own failed
    /// steps as the person's off for good, and one that never takes it out turns the fabric on
    /// again over a command the person cleared.
    #[test]
    fn a_turn_on_by_default_records_itself_until_its_steps_finish() {
        let no_command = || Ok(false);
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let root = dir.path().join("root");
        let done = by_default_in(&root, None, &no_command, |out| {
            assert!(root.join(DEFAULT_RECORD).exists(), "left before the steps");
            node_written(&root);
            out.ok("node", "n-0123456789abcdef");
            Ok(())
        })
        .expect("turned on");
        assert!(
            matches!(&done, ByDefault::TurnedOn(said) if said.contains("node")),
            "{done:?}"
        );
        assert!(
            !root.join(DEFAULT_RECORD).exists(),
            "taken out once it finished"
        );
        let read = unset_in(&root, None, false);
        assert!(
            matches!(&read, Some(Unset::PersonOff(_))),
            "a command the person cleared after: {read:?}"
        );
        let left = by_default_in(&root, None, &no_command, |_| {
            panic!("no steps over their off")
        });
        assert!(matches!(left, Ok(ByDefault::Left(_))), "{left:?}");

        let dir = aterm_tempfile::tempdir().expect("scratch");
        let root = dir.path().join("root");
        let failed = by_default_in(&root, None, &no_command, |out| {
            node_written(&root);
            out.fail("broker", "did not start");
            Err(ExitCode::FAILURE)
        });
        assert!(
            matches!(&failed, Err(said) if said.contains("did not start")),
            "{failed:?}"
        );
        assert!(root.join(DEFAULT_RECORD).exists(), "kept for the next pass");
        assert!(
            matches!(unset_in(&root, None, false), Some(Unset::Never(_))),
            "the next pass finishes it"
        );
    }

    /// The turn-on reads the state again under the off's lock: an off recorded since the pass read
    /// it, an off running now (holding the lock) and a command set since each leave the steps
    /// unrun.
    /// NEGATIVE CONTROL: a turn-on that trusts the pass's first read runs its steps (and removes
    /// the off's record) over an off made since; one that skips the lock runs them midway through
    /// an off.
    #[test]
    fn a_turn_on_by_default_stands_aside_for_an_off_made_since_or_running() {
        let no_command = || Ok(false);
        let unrun = |_: &mut Out| -> Result<(), ExitCode> { panic!("no steps") };

        let dir = aterm_tempfile::tempdir().expect("scratch");
        let root = dir.path().join("root");
        let p = scratch_paths(dir.path());
        let mut out = Out::kept();
        off_record_step(&p, &mut out);
        let left = by_default_in(&root, None, &no_command, unrun);
        assert!(
            matches!(&left, Ok(ByDefault::Left(why)) if why.contains("aterm fabric off")),
            "{left:?}"
        );
        assert!(root.join(OFF_RECORD).exists(), "the off's record stands");

        let dir = aterm_tempfile::tempdir().expect("scratch");
        let root = dir.path().join("root");
        let off = switch_lock(&root, Duration::ZERO).expect("the off's lock");
        let left = by_default_in(&root, None, &no_command, unrun);
        assert!(
            matches!(&left, Ok(ByDefault::Left(why)) if why.contains("runs now")),
            "{left:?}"
        );
        assert!(!root.join(DEFAULT_RECORD).exists(), "nothing written");
        drop(off);

        let on = by_default_in(&root, None, &|| Ok(true), unrun);
        assert!(matches!(on, Ok(ByDefault::On)), "{on:?}");
        assert!(!root.join(DEFAULT_RECORD).exists(), "nothing written");
    }

    /// A dry run says the record's step and writes nothing.
    #[test]
    fn a_dry_run_off_or_on_writes_and_removes_no_record() {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let p = scratch_paths(dir.path());
        let mut out = Out::new(true);
        out.kept = Some(std::cell::RefCell::new(Vec::new()));
        off_record_step(&p, &mut out);
        assert!(out.said().contains("would"), "{}", out.said());
        assert!(!p.root.exists(), "a dry off makes nothing");
        std::fs::create_dir_all(&p.root).expect("root");
        std::fs::write(p.root.join(OFF_RECORD), b"").expect("record");
        let mut out = Out::new(true);
        out.kept = Some(std::cell::RefCell::new(Vec::new()));
        assert!(record_step(&p, Service::Launchd, &mut out));
        assert!(p.root.join(OFF_RECORD).exists(), "a dry on keeps it");
    }

    /// An absent broker on `service`'s root (nothing answers on its socket and its job unloaded).
    fn absent(service: Service) -> Found {
        let mut at = found(service, None);
        at.view.pid = None;
        at.job = None;
        at.loaded = Ok(false);
        at.answers = Err("connect: Connection refused (os error 61)".to_string());
        at
    }

    /// `at` with its fabric root at `root`, and its broker's endpoint with it where it named the
    /// root's socket: an apply takes the switch lock in the root.
    fn rooted(mut at: Found, root: &Path) -> Found {
        let ours = at.view.endpoint == at.p.sock();
        at.p.root = root.to_path_buf();
        if ours {
            at.view.endpoint = at.p.sock();
        }
        at
    }

    /// The converge pass's broker renewal: with no `[fabric] command` nothing is looked at and
    /// nothing runs; a broker on the build on disk reads current; one on a replaced build is
    /// renewed once by the broker step (a look leaves it due); one the person keeps
    /// (`--service none`, or one started by hand that is not the process `on`'s job runs, with
    /// that job not loaded or running another), or one at another socket, is left as it is.
    /// NEGATIVE CONTROLS: a renewal that reads no command as due runs the step with the fabric
    /// off; one that takes any broker of a root with `on`'s job for that job's steps a broker the
    /// person started by hand. Absent sockets, `--service none` with no broker and an unavailable
    /// supervisor are checked by
    /// `a_broker_that_does_not_answer_is_due_where_the_job_on_installed_serves_none`.
    #[test]
    fn a_broker_is_renewed_only_where_the_fabric_is_on_and_it_runs_a_replaced_build() {
        use std::cell::Cell;
        let command = || Some(words("aterm link serve --broker /nonexistent/bus.sock"));
        let (looks, steps) = (Cell::new(0), Cell::new(0));
        let dir = aterm_tempfile::tempdir().expect("scratch");
        aterm_uds::private_file::ensure_private_dir(&dir.path().join("root")).expect("the root");
        let renew = |command: Option<Vec<String>>, renew: Renew, at: Found| {
            renew_with(
                command,
                renew,
                |_: &[String]| {
                    looks.set(looks.get() + 1);
                    Ok(rooted(at, &dir.path().join("root")))
                },
                |_: &Found| {
                    steps.set(steps.get() + 1);
                    Ok("broker done (re)started launchd".to_string())
                },
            )
        };
        let off = renew(
            None,
            Renew::Apply,
            found(Service::Launchd, Some("/old/aterm")),
        );
        assert_eq!(off, Ok(BrokerRenewal::Off));
        assert_eq!(
            (looks.get(), steps.get()),
            (0, 0),
            "command unset: nothing read or run"
        );

        let current = renew(command(), Renew::Apply, found(Service::Launchd, None));
        assert!(
            matches!(current, Ok(BrokerRenewal::Current(_))),
            "{current:?}"
        );
        assert_eq!((looks.get(), steps.get()), (1, 0), "current: no step");

        let due = renew(
            command(),
            Renew::Look,
            found(Service::Launchd, Some("/old/aterm")),
        );
        assert!(matches!(due, Ok(BrokerRenewal::Due(_))), "{due:?}");
        assert_eq!(steps.get(), 0, "a look runs no step");

        let renewed = renew(
            command(),
            Renew::Apply,
            found(Service::Launchd, Some("/old/aterm")),
        );
        assert_eq!(
            renewed,
            Ok(BrokerRenewal::Renewed(
                "broker done (re)started launchd".to_string()
            ))
        );
        assert_eq!(steps.get(), 1, "replaced: one renewal");

        let theirs = renew(
            command(),
            Renew::Apply,
            found(Service::None, Some("/old/aterm")),
        );
        assert!(matches!(theirs, Ok(BrokerRenewal::Kept(_))), "{theirs:?}");
        let mut elsewhere = found(Service::Launchd, Some("/old/aterm"));
        elsewhere.view.endpoint = "/elsewhere/bus.sock".to_string();
        let elsewhere = renew(command(), Renew::Apply, elsewhere);
        assert!(
            matches!(elsewhere, Ok(BrokerRenewal::Kept(_))),
            "{elsewhere:?}"
        );
        for job in [None, Some(4343)] {
            let mut by_hand = found(Service::Launchd, Some("/old/aterm"));
            by_hand.job = job;
            let by_hand = renew(command(), Renew::Apply, by_hand);
            assert!(
                matches!(&by_hand, Ok(BrokerRenewal::Kept(why)) if why.contains("not the job")),
                "{job:?}: {by_hand:?}"
            );
        }
        assert_eq!(steps.get(), 1, "a broker the person keeps is left as it is");
    }

    /// The update measured on m22 left `on`'s launchd job unloaded, and the next pass read the
    /// missing broker as current. Where no broker answers on the root's socket, the job `on`
    /// installed is due (a look) and stepped once (an apply) where launchd has not loaded it,
    /// holds it running nothing, or runs a broker that does not answer, and where systemd runs
    /// none. A broker that answers reads current whether or not the listener scan named it. It is
    /// left as it is under `--service none`, at another socket, for a process the person started
    /// that holds the socket, where launchd or systemd has the job disabled (or masked) or cannot
    /// be asked about it, and where `aterm fabric off`'s record stands in the root, which keeps a
    /// replaced broker too.
    /// NEGATIVE CONTROLS: a renewal that reads a broker nobody answers for as current (as
    /// measured), one blind to the off record, to a disabled job, to a login launchd cannot be
    /// asked in, or to an answering broker the scan missed each fail it.
    #[test]
    fn a_broker_that_does_not_answer_is_due_where_the_job_on_installed_serves_none() {
        use std::cell::Cell;
        let steps = Cell::new(0);
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let root = dir.path().join("root");
        aterm_uds::private_file::ensure_private_dir(&root).expect("the root");
        let renew = |renew: Renew, at: Found| {
            renew_with(
                Some(words("aterm link serve --broker /nonexistent/bus.sock")),
                renew,
                |_: &[String]| Ok(rooted(at, &root)),
                |_: &Found| {
                    steps.set(steps.get() + 1);
                    Ok("broker done loaded launchd".to_string())
                },
            )
        };
        let silent = |service: Service, loaded: bool, job: Option<u32>, pid: Option<u32>| {
            let mut at = found(service, None);
            at.view.pid = pid;
            at.answers = Err("connect: Connection refused (os error 61)".to_string());
            at.loaded = Ok(loaded);
            at.job = job;
            at
        };
        let sock = rooted(found(Service::Launchd, None), &root).p.sock();
        for (service, loaded, job, pid, says) in [
            (
                Service::Launchd,
                false,
                None,
                None,
                "launchd has not loaded",
            ),
            (Service::Launchd, true, None, None, "runs no process of it"),
            (Service::Launchd, true, Some(4343), None, "though launchd"),
            (
                Service::Launchd,
                true,
                Some(4242),
                Some(4242),
                "though launchd",
            ),
            (Service::Systemd, false, None, None, "systemd --user"),
        ] {
            let case = format!("{service:?} loaded={loaded} job={job:?} pid={pid:?}");
            let look = renew(Renew::Look, silent(service, loaded, job, pid));
            assert!(
                matches!(&look, Ok(BrokerRenewal::Due(why))
                    if why.starts_with(&format!("no broker answers on {sock} (connect: "))
                        && why.contains(says)),
                "{case}: {look:?}"
            );
            let before = steps.get();
            let applied = renew(Renew::Apply, silent(service, loaded, job, pid));
            assert!(
                matches!(&applied, Ok(BrokerRenewal::Renewed(_))),
                "{case}: {applied:?}"
            );
            assert_eq!(steps.get(), before + 1, "{case}: one step");
        }
        let stepped = steps.get();

        let mut unnamed = found(Service::Launchd, None);
        unnamed.view.pid = None;
        let unnamed = renew(Renew::Apply, unnamed);
        assert!(
            matches!(&unnamed, Ok(BrokerRenewal::Current(_))),
            "a broker that answers, though the scan named no process: {unnamed:?}"
        );
        let theirs = renew(Renew::Apply, silent(Service::None, false, None, None));
        assert!(
            matches!(&theirs, Ok(BrokerRenewal::Kept(why)) if why.contains("start it yourself")),
            "{theirs:?}"
        );
        let by_hand = renew(
            Renew::Apply,
            silent(Service::Launchd, false, None, Some(999)),
        );
        assert!(
            matches!(&by_hand, Ok(BrokerRenewal::Kept(why)) if why.contains("not the job")),
            "{by_hand:?}"
        );
        let mut disabled = silent(Service::Launchd, false, None, None);
        disabled.disabled = true;
        let disabled = renew(Renew::Apply, disabled);
        assert!(
            matches!(&disabled, Ok(BrokerRenewal::Kept(why)) if why.contains("disabled")),
            "{disabled:?}"
        );
        let mut masked = silent(Service::Systemd, false, None, None);
        (masked.job, masked.loaded, masked.disabled) =
            systemd_state(Ok(systemd_shown("MainPID=0\nUnitFileState=masked\n")));
        let masked = renew(Renew::Apply, masked);
        assert!(
            matches!(&masked, Ok(BrokerRenewal::Kept(why)) if why.contains("systemctl --user enable")),
            "{masked:?}"
        );
        let mut no_bus = silent(Service::Systemd, false, None, None);
        (no_bus.job, no_bus.loaded, no_bus.disabled) =
            systemd_state(Err("no systemd user manager answers this login".to_string()));
        let no_bus = renew(Renew::Apply, no_bus);
        assert!(
            matches!(&no_bus, Ok(BrokerRenewal::Kept(why)) if why.contains("no systemd user manager")),
            "{no_bus:?}"
        );
        let mut unasked = silent(Service::Launchd, false, None, None);
        unasked.loaded = Err("launchd has no gui/501 domain in this login".to_string());
        let unasked = renew(Renew::Apply, unasked);
        assert!(
            matches!(&unasked, Ok(BrokerRenewal::Kept(why)) if why.contains("no gui/501 domain")),
            "{unasked:?}"
        );
        let mut elsewhere = silent(Service::Launchd, false, None, None);
        elsewhere.view.endpoint = "/elsewhere/bus.sock".to_string();
        let elsewhere = renew(Renew::Apply, elsewhere);
        assert!(
            matches!(&elsewhere, Ok(BrokerRenewal::Kept(why)) if why.contains("not its root's")),
            "{elsewhere:?}"
        );
        std::fs::write(root.join(OFF_RECORD), b"").expect("the person's off");
        for off in [
            silent(Service::Launchd, false, None, None),
            found(Service::Launchd, Some("/old/aterm")),
        ] {
            let off = renew(Renew::Apply, off);
            assert!(
                matches!(&off, Ok(BrokerRenewal::Kept(why)) if why.starts_with("off by your choice")),
                "{off:?}"
            );
        }
        assert_eq!(
            steps.get(),
            stepped,
            "what the person keeps is never stepped"
        );
    }

    /// A renewal's apply steps only under the switch `aterm fabric off` and `on` hold: one that
    /// holds it now leaves the broker as it is (a look still reads it due), and one that starts
    /// while the step runs finds it held. A lock that cannot be made (the root is a file, or gone,
    /// which the renewal does not make) is said beside the step's lines.
    /// NEGATIVE CONTROLS: a renewal that takes no lock steps midway through an off (which stops
    /// the broker the step starts); one that lets it go before the step lets an off land midway
    /// through it; one that reads any lock failure as held never renews a broker whose root
    /// cannot hold the lock.
    #[test]
    fn a_broker_renewal_steps_only_under_the_switch_off_and_on_hold() {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let root = dir.path().join("root");
        let command = || Some(words("aterm link serve"));
        let replaced = |root: &Path| rooted(found(Service::Launchd, Some("/old/aterm")), root);

        let off = switch_lock(&root, Duration::ZERO).expect("an off's lock");
        let kept = renew_with(
            command(),
            Renew::Apply,
            |_: &[String]| Ok(replaced(&root)),
            |_: &Found| panic!("no step while an off holds the switch"),
        );
        assert!(
            matches!(&kept, Ok(BrokerRenewal::Kept(why)) if why.contains("runs now")),
            "{kept:?}"
        );
        let due = renew_with(
            command(),
            Renew::Look,
            |_: &[String]| Ok(replaced(&root)),
            |_: &Found| panic!("a look runs no step"),
        );
        assert!(matches!(due, Ok(BrokerRenewal::Due(_))), "{due:?}");
        drop(off);

        let renewed = renew_with(
            command(),
            Renew::Apply,
            |_: &[String]| Ok(replaced(&root)),
            |at: &Found| {
                let off = switch_lock(&at.p.root, Duration::ZERO);
                assert!(
                    matches!(&off, Err(e) if e.kind() == io::ErrorKind::WouldBlock),
                    "an off that starts during the step waits for it: {:?}",
                    off.as_ref().err()
                );
                Ok("broker done (re)started launchd".to_string())
            },
        );
        assert_eq!(
            renewed,
            Ok(BrokerRenewal::Renewed(
                "broker done (re)started launchd".to_string()
            ))
        );
        assert!(
            switch_lock(&root, Duration::ZERO).is_ok(),
            "let go once the step ends"
        );

        let file = dir.path().join("a-file");
        std::fs::write(&file, b"").expect("a file where the root goes");
        let unlocked = renew_with(
            command(),
            Renew::Apply,
            |_: &[String]| Ok(replaced(&file)),
            |_: &Found| Ok("broker done (re)started launchd".to_string()),
        );
        assert!(
            matches!(&unlocked, Ok(BrokerRenewal::Renewed(said))
                if said.starts_with("broker done (re)started launchd (without ")),
            "{unlocked:?}"
        );
        let gone = dir.path().join("gone");
        let unlocked = renew_with(
            command(),
            Renew::Apply,
            |_: &[String]| Ok(replaced(&gone)),
            |_: &Found| Ok("broker done (re)started launchd".to_string()),
        );
        assert!(
            matches!(&unlocked, Ok(BrokerRenewal::Renewed(said)) if said.contains("(without ")),
            "{unlocked:?}"
        );
        assert!(!gone.exists(), "a root that is gone is not made");
    }

    /// A person's `aterm fabric on` runs its steps under the switch: one the converge pass's
    /// turn-on or renewal, or another `on` or `off`, holds past the wait fails the command with
    /// what to do and runs no step; one free is held while the steps run. A dry run takes none.
    /// NEGATIVE CONTROLS: an `on` that takes no lock writes the root midway through an off or a
    /// turn-on (its steps run while the switch is held), and one that lets it go before its steps
    /// lets an off land midway through them.
    #[test]
    fn a_persons_fabric_on_runs_its_steps_under_the_switch() {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let prepared = Prepared {
            p: scratch_paths(dir.path()),
            job_durability: None,
            durability_job: None,
        };
        let root = prepared.p.root.clone();

        let held = switch_lock(&root, Duration::ZERO).expect("a turn-on's lock");
        let mut out = Out::kept();
        let code = on_prepared(&prepared, &mut out, Duration::ZERO, |_| {
            panic!("no step while the switch is held")
        });
        assert_eq!(code, ExitCode::FAILURE);
        assert!(
            out.said().contains("run `aterm fabric on` again"),
            "{}",
            out.said()
        );
        drop(held);

        let mut out = Out::kept();
        let code = on_prepared(&prepared, &mut out, Duration::ZERO, |_| {
            let off = switch_lock(&root, Duration::ZERO);
            assert!(
                matches!(&off, Err(e) if e.kind() == io::ErrorKind::WouldBlock),
                "an off that starts during the steps waits for them: {:?}",
                off.as_ref().err()
            );
            Err(ExitCode::from(7))
        });
        assert_eq!(code, ExitCode::from(7), "the steps' own exit");
        assert!(
            switch_lock(&root, Duration::ZERO).is_ok(),
            "let go once the steps end"
        );

        let dry = aterm_tempfile::tempdir().expect("scratch");
        let mut out = Out::new(true);
        let lock = persons_switch(&dry.path().join("root"), "on", Duration::ZERO, &mut out);
        assert!(matches!(lock, Ok(None)), "a dry run takes no lock");
        assert!(!dry.path().join("root").exists(), "a dry run makes no root");
    }

    /// `aterm fabric on --service none` records that the person runs the broker, and the renewal
    /// keeps a root with that record as it is, even where a plist an earlier `on` installed is
    /// still there; an `on` that hands the broker to launchd takes the record out, and a dry run
    /// writes none.
    /// NEGATIVE CONTROLS: an `on --service none` that records nothing, or a renewal blind to the
    /// record, loads the leftover plist over the broker the person keeps; an `on` that leaves the
    /// record keeps a job it installed from ever being renewed.
    #[test]
    fn a_broker_the_person_keeps_with_service_none_is_never_renewed() {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let p = scratch_paths(dir.path());
        aterm_uds::private_file::ensure_private_dir(&p.root).expect("root, 0700 as `on` makes it");
        let record = p.root.join(HAND_RECORD);

        let mut out = Out::new(true);
        out.kept = Some(std::cell::RefCell::new(Vec::new()));
        assert!(hand_record_step(&p, Service::None, &mut out));
        assert!(out.said().contains("would"), "{}", out.said());
        assert!(!record.exists(), "a dry run writes no record");

        let mut out = Out::kept();
        assert!(
            hand_record_step(&p, Service::None, &mut out),
            "{}",
            out.said()
        );
        assert!(record.exists(), "`on --service none` records it");

        let renew = |renew: Renew| {
            renew_with(
                Some(words("aterm link serve")),
                renew,
                |_: &[String]| Ok(rooted(absent(Service::Launchd), &p.root)),
                |_: &Found| panic!("no step for a broker the person keeps"),
            )
        };
        for how in [Renew::Look, Renew::Apply] {
            let kept = renew(how);
            assert!(
                matches!(&kept, Ok(BrokerRenewal::Kept(why)) if why.contains("--service none")),
                "{how:?}: {kept:?}"
            );
        }

        let mut out = Out::kept();
        assert!(
            record_step(&p, Service::Launchd, &mut out),
            "{}",
            out.said()
        );
        assert!(!record.exists(), "an `on` under launchd takes it out");
        let due = renew_with(
            Some(words("aterm link serve")),
            Renew::Look,
            |_: &[String]| Ok(rooted(absent(Service::Launchd), &p.root)),
            |_: &Found| panic!("a look runs no step"),
        );
        assert!(matches!(due, Ok(BrokerRenewal::Due(_))), "{due:?}");
    }

    /// `aterm fabric off` boots out a job `print` did not say of: a print that said nothing is
    /// not read as an unloaded job (whose plist the off would remove while launchd kept its
    /// broker running), and the bootout's failure is excused only where `print` then says
    /// launchd holds no such job. A job print says is absent, with no plist, is not installed.
    /// NEGATIVE CONTROLS: an off that reads an unread print as unloaded runs no bootout of the
    /// loaded job; one that excuses every failed bootout hides a job it did not stop; one that
    /// excuses none fails an off of a job that was not loaded.
    #[test]
    fn an_off_boots_out_a_job_print_did_not_say_of() {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let plist = dir.path().join("job.plist");

        std::fs::write(&plist, "old").expect("plist");
        let loaded = FakeLaunchd::new(true, 0);
        loaded.unread.set(1);
        let mut out = Out::kept();
        off_launchd(&loaded, FAKE_LABEL, &plist, &mut out);
        assert_eq!(loaded.calls(), ["print", "bootout"]);
        assert!(!loaded.held(), "booted out");
        assert!(!out.failed, "{}", out.said());
        assert!(!plist.exists(), "removed");

        std::fs::write(&plist, "old").expect("plist");
        let unloaded = FakeLaunchd::new(false, 0);
        unloaded.unread.set(1);
        let mut out = Out::kept();
        off_launchd(&unloaded, FAKE_LABEL, &plist, &mut out);
        assert_eq!(unloaded.calls(), ["print", "bootout", "print"]);
        assert!(!out.failed, "no such job: {}", out.said());
        assert!(!plist.exists(), "removed");

        let silent = FakeLaunchd::new(false, 0);
        silent.unread.set(2);
        let mut out = Out::kept();
        off_launchd(&silent, FAKE_LABEL, &plist, &mut out);
        assert!(
            out.failed && out.said().contains("No such process"),
            "print never said: {}",
            out.said()
        );

        let absent = FakeLaunchd::new(false, 0);
        let mut out = Out::kept();
        off_launchd(&absent, FAKE_LABEL, &plist, &mut out);
        assert_eq!(absent.calls(), ["print"]);
        assert!(out.said().contains("not installed"), "{}", out.said());
    }

    /// The broker step reloads a job launchd loaded from other arguments than the plist on disk
    /// (one written while the job stayed loaded), since a kickstart restarts what launchd loaded:
    /// the reload boots it out, writes the plist again and bootstraps it. A job loaded from the
    /// plist's arguments, one not loaded, or a `print` that lists none is read as the file's; a
    /// plist that is not `on`'s is rewritten as before. The step reads launchd through the seam
    /// (`launchd_read`), so the arguments it judges are the ones `print` listed.
    /// NEGATIVE CONTROLS: a step that reads only the file kickstarts the older definition; one
    /// whose read hands the rewrite no printed arguments does too.
    #[test]
    fn a_job_loaded_from_older_arguments_is_reloaded_not_kickstarted() {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let p = scratch_paths(dir.path());
        let current = p.plist_text();
        let argv = p.broker_argv();
        let printed = |args: &[String]| {
            format!(
                "gui/501/{} = {{\n\tstate = running\n\tprogram = {}\n\targuments = {{\n{}\t}}\n\n\t\
                 inherited environment = {{\n\t\tA => b\n\t}}\n}}\n",
                p.label(),
                args[0],
                args.iter().map(|a| format!("\t\t{a}\n")).collect::<String>()
            )
        };
        assert_eq!(printed_arguments(&printed(&argv)), Some(argv.clone()));
        assert_eq!(
            printed_arguments("gui/501/x = {\n\tstate = running\n}\n"),
            None
        );

        let older = argv[..argv.len() - 2].to_vec();
        let reload = launchd_rewrite(&p, Some(current.clone()), true, &printed(&older))
            .expect("loaded from older arguments: reloaded");
        assert_eq!(reload.text, current);
        assert_eq!(reload.old.as_deref(), Some(current.as_str()));
        for (old, loaded, said, case) in [
            (
                Some(current.clone()),
                true,
                printed(&argv),
                "the plist's arguments",
            ),
            (Some(current.clone()), false, printed(&older), "not loaded"),
            (
                Some(current.clone()),
                true,
                "state = running\n".to_string(),
                "none listed",
            ),
        ] {
            assert!(launchd_rewrite(&p, old, loaded, &said).is_none(), "{case}");
        }
        assert!(
            launchd_rewrite(&p, Some("old".to_string()), true, &printed(&argv)).is_some(),
            "a plist that is not on's"
        );
        assert!(launchd_rewrite(&p, None, false, "").is_some(), "no plist");

        let lc = FakeLaunchd::new(true, 1);
        *lc.plist.borrow_mut() = current.clone();
        let up = launchd_up(
            &lc,
            FAKE_LABEL,
            Path::new(FAKE_PLIST),
            (true, None),
            Some(&reload),
        );
        assert_eq!(up, Ok(Up::Loaded(None)), "{:?}", lc.calls());
        assert_eq!(lc.count("kickstart"), 0, "{:?}", lc.calls());
        assert_eq!(lc.running_from().as_deref(), Some(current.as_str()));

        // The step's own read of launchd (`launchd_read`, the live one's `print` behind the seam):
        // the arguments the job was loaded from are the ones the rewrite is judged on.
        assert_eq!(plist_arguments(&current), argv);
        let older_plist = argv[argv.len() - 2..]
            .iter()
            .fold(current.clone(), |text, a| {
                text.replace(&format!("    <string>{a}</string>\n"), "")
            });
        assert_eq!(plist_arguments(&older_plist), older);
        let lc = FakeLaunchd::new(true, 0);
        *lc.loaded_from.borrow_mut() = older_plist;
        let read = launchd_read(&lc, &p, FAKE_LABEL, Some(current.clone()));
        assert!(
            matches!(&read, Ok((true, None, Some(r))) if r.text == current),
            "a job loaded from older arguments is reloaded"
        );
        *lc.loaded_from.borrow_mut() = current.clone();
        let read = launchd_read(&lc, &p, FAKE_LABEL, Some(current.clone()));
        assert!(
            matches!(read, Ok((true, None, None))),
            "one loaded from the plist's"
        );
        lc.loaded.set(false);
        let read = launchd_read(&lc, &p, FAKE_LABEL, Some(current.clone()));
        assert!(matches!(read, Ok((false, None, None))), "one not loaded");
        lc.unread.set(1);
        let read = launchd_read(&lc, &p, FAKE_LABEL, Some(current.clone()));
        assert!(
            matches!(read, Err(LaunchdJob::Unread)),
            "a print that did not say"
        );
        assert_eq!(lc.count("print"), 4, "{:?}", lc.calls());
    }

    /// systemd's unit is written before `daemon-reload`, `enable` and `restart`; a step that fails
    /// after the write puts the unit it replaced back, so the next pass reads the unit as changed
    /// and runs the step again; a unit with no file before is kept, and one already on disk is
    /// not written.
    /// NEGATIVE CONTROL: a step with no put-back leaves the new unit beside the process the old
    /// one started, which the next pass reads as current.
    #[test]
    fn a_systemd_restart_that_fails_puts_the_unit_it_replaced_back() {
        use std::cell::RefCell;
        let (calls, disk) = (
            RefCell::new(Vec::<String>::new()),
            RefCell::new(b"old".to_vec()),
        );
        let (calls, disk) = (&calls, &disk);
        let failing = |verb: &'static str| {
            move |args: &[&str]| {
                calls.borrow_mut().push(args[0].to_string());
                if args[0] == verb {
                    Err(format!(
                        "systemctl --user {} exited 1: failed",
                        args.join(" ")
                    ))
                } else {
                    Ok(())
                }
            }
        };
        let write = |bytes: &[u8]| {
            calls.borrow_mut().push("write".to_string());
            *disk.borrow_mut() = bytes.to_vec();
            Ok::<(), String>(())
        };

        let why = systemd_up(&failing("restart"), &write, "u", Some(b"old"), b"new")
            .expect_err("the restart failed");
        assert!(
            why.contains("restart u exited 1") && why.contains("put back"),
            "{why}"
        );
        assert_eq!(
            *calls.borrow(),
            ["write", "daemon-reload", "enable", "restart", "write"]
        );
        assert_eq!(*disk.borrow(), b"old", "the next pass reads it as changed");

        calls.borrow_mut().clear();
        *disk.borrow_mut() = Vec::new();
        let why = systemd_up(&failing("enable"), &write, "u", None, b"new").expect_err("failed");
        assert!(!why.contains("put back"), "{why}");
        assert_eq!(*calls.borrow(), ["write", "daemon-reload", "enable"]);
        assert_eq!(*disk.borrow(), b"new", "no unit before: kept");

        calls.borrow_mut().clear();
        assert_eq!(
            systemd_up(&failing("none"), &write, "u", Some(b"new"), b"new"),
            Ok(())
        );
        assert_eq!(*calls.borrow(), ["daemon-reload", "enable", "restart"]);
    }

    /// `print-disabled`'s rows name a label disabled as macOS 13 and later print it (`=>
    /// disabled`) and as earlier ones did (`=> true`); an enabled row, another label, or one that
    /// only starts with it, is not.
    #[test]
    fn a_disabled_job_is_read_off_print_disabled() {
        let text = "\tdisabled services = {\n\
                    \t\t\"com.example.other\" => disabled\n\
                    \t\t\"systems.alab.astream-broker.n-1\" => enabled\n\
                    \t\t\"systems.alab.astream-broker.n-2\" => disabled\n\
                    \t\t\"systems.alab.astream-broker.n-3\" => true\n\
                    \t\t\"systems.alab.astream-broker.n-4x\" => disabled\n\
                    \t}\n";
        assert!(!disabled_in(text, "systems.alab.astream-broker.n-1"));
        assert!(disabled_in(text, "systems.alab.astream-broker.n-2"));
        assert!(disabled_in(text, "systems.alab.astream-broker.n-3"));
        assert!(!disabled_in(text, "systems.alab.astream-broker.n-4"));
        assert!(!disabled_in(text, "systems.alab.astream-broker.n-5"));
    }

    /// What launchd and systemd say of the job, read fail-closed: only `print`'s 113 is "no such
    /// job", any other failure says nothing; a unit a person disabled or masked is theirs; a
    /// systemd that does not answer, like a launchd `print` or `print-disabled` that says
    /// nothing, is not "stopped" or "enabled".
    /// NEGATIVE CONTROLS: `print`'s other failures read as absent (the first review's finding),
    /// `mask` not read as a person's choice, and a `MainPID=0` read as a pid each fail it.
    #[test]
    fn a_supervisor_that_does_not_say_is_not_read_as_a_stopped_job() {
        let running = "\tstate = running\n\tpid = 4242\n";
        assert_eq!(job_of(Some(0), running), LaunchdJob::Loaded(Some(4242)));
        assert_eq!(
            job_of(Some(0), "\tstate = not running\n"),
            LaunchdJob::Loaded(None)
        );
        assert_eq!(job_of(Some(113), ""), LaunchdJob::Absent);
        assert_eq!(job_of(Some(112), ""), LaunchdJob::NoGuiLogin);
        for code in [Some(1), Some(5), Some(37), Some(125), None] {
            assert_eq!(job_of(code, ""), LaunchdJob::Unread, "{code:?}");
        }
        let label = "systems.alab.astream-broker.x";
        assert_eq!(launchd_state(LaunchdJob::Absent, label), (None, Ok(false)));
        assert_eq!(
            launchd_state(LaunchdJob::Loaded(Some(7)), label),
            (Some(7), Ok(true))
        );
        for job in [LaunchdJob::Unread, LaunchdJob::NoGuiLogin] {
            let (pid, loaded) = launchd_state(job, label);
            assert!(pid.is_none() && loaded.is_err(), "{job:?}: {loaded:?}");
        }

        assert_eq!(
            systemd_shown("MainPID=4242\nUnitFileState=enabled\n"),
            (Some(4242), false)
        );
        assert_eq!(
            systemd_shown("UnitFileState=disabled\nMainPID=0\n"),
            (None, true)
        );
        for state in ["masked", "masked-runtime"] {
            assert_eq!(
                systemd_shown(&format!("MainPID=0\nUnitFileState={state}\n")),
                (None, true),
                "{state}"
            );
        }
        for state in ["static", "linked", ""] {
            assert_eq!(
                systemd_shown(&format!("MainPID=0\nUnitFileState={state}\n")),
                (None, false),
                "{state}"
            );
        }
        assert_eq!(
            with_disabled((Some(7), Ok(true)), Ok(true)),
            (Some(7), Ok(true), true)
        );
        let (_, loaded, disabled) = with_disabled(
            (Some(7), Ok(true)),
            Err("`launchctl print-disabled gui/501` exited 1".to_string()),
        );
        assert!(loaded.is_err() && !disabled, "{loaded:?}");
        assert_eq!(
            systemd_state(Ok((Some(9), false))),
            (Some(9), Ok(true), false)
        );
        let (pid, loaded, disabled) = systemd_state(Err("no user bus".to_string()));
        assert!(pid.is_none() && loaded.is_err() && !disabled);
    }

    /// A socket a process holds is asked twice before it reads as unanswered; one nobody holds
    /// once. NEGATIVE CONTROL: asking once fails it.
    #[test]
    fn a_held_socket_is_asked_again_before_it_reads_unanswered() {
        use std::cell::Cell;
        let asks = Cell::new(0);
        let pauses = Cell::new(0);
        let busy_once = || {
            asks.set(asks.get() + 1);
            if asks.get() == 1 {
                Err("hello: Resource temporarily unavailable".to_string())
            } else {
                Ok(())
            }
        };
        assert_eq!(
            asked_twice(true, busy_once, || pauses.set(pauses.get() + 1)),
            Ok(())
        );
        assert_eq!((asks.get(), pauses.get()), (2, 1));

        asks.set(0);
        let never = || {
            asks.set(asks.get() + 1);
            Err(format!("connect: refused {}", asks.get()))
        };
        assert_eq!(
            asked_twice(true, never, || ()),
            Err("connect: refused 1; asked again: connect: refused 2".to_string())
        );
        asks.set(0);
        assert_eq!(
            asked_twice(false, never, || panic!(
                "nothing holds the socket: no pause"
            )),
            Err("connect: refused 1".to_string())
        );
        assert_eq!(asks.get(), 1);
    }

    /// A kept `Out` prints nothing and hands back what it said; a step it ran without a node id
    /// fails, naming the remedy.
    #[test]
    fn a_kept_out_hands_back_its_lines_and_a_root_with_no_node_is_not_stepped() {
        let mut out = Out::kept();
        out.already("plist", "/x.plist");
        out.done("broker", "would restart", "restarted");
        assert_eq!(out.said(), "plist already /x.plist; broker done restarted");
        let mut nodeless = found(Service::Launchd, Some("/old/aterm"));
        nodeless.node = None;
        let refused = step_on(&nodeless);
        assert!(
            refused
                .as_ref()
                .is_err_and(|why| why.contains("`aterm fabric on` provisions one")),
            "{refused:?}"
        );
    }

    fn words(line: &str) -> Vec<String> {
        aterm_types::fabric_command::split_line(line)
    }

    /// launchd as the measured Mac's behaved, in memory: `bootout` returns at once while the job
    /// stays held for `lingers` more `print`s, a `bootstrap` of a label launchd still holds fails
    /// as launchd fails it (`5: Input/output error`), and `kickstart -k` restarts only a loaded
    /// job, the process it killed running `lingers` more looks. The plist on disk starts as
    /// `old`. Each verb is recorded by its first word, a plist write as `write`.
    struct FakeLaunchd {
        loaded: std::cell::Cell<bool>,
        /// The plist text the loaded job was loaded from.
        loaded_from: std::cell::RefCell<String>,
        /// The plist on disk.
        plist: std::cell::RefCell<String>,
        /// The `print`s a booted-out job is still held for.
        letting_go: std::cell::Cell<u32>,
        /// How many `print`s each bootout leaves it held for.
        lingers: u32,
        /// The `print`s that say nothing ([`LaunchdJob::Unread`]).
        unread: std::cell::Cell<u32>,
        /// Bootstraps that fail even with the label free, before one loads.
        refusals: std::cell::Cell<u32>,
        /// The refused bootstrap (counted from 1) that loads the job all the same; 0 for none.
        loads_anyway: std::cell::Cell<u32>,
        /// The bootstraps refused so far.
        refused: std::cell::Cell<u32>,
        /// Whether a kickstart fails even for a loaded job (a disabled one: `119`).
        kick_refused: std::cell::Cell<bool>,
        /// Whether a plist write fails.
        write_fails: std::cell::Cell<bool>,
        /// The looks the process a `kickstart -k` killed still runs for.
        exiting: std::cell::Cell<u32>,
        calls: std::cell::RefCell<Vec<String>>,
        pauses: std::cell::Cell<u32>,
    }

    impl FakeLaunchd {
        fn new(loaded: bool, lingers: u32) -> Self {
            Self {
                loaded: std::cell::Cell::new(loaded),
                loaded_from: std::cell::RefCell::new("old".to_string()),
                plist: std::cell::RefCell::new("old".to_string()),
                letting_go: std::cell::Cell::new(0),
                lingers,
                unread: std::cell::Cell::new(0),
                refusals: std::cell::Cell::new(0),
                loads_anyway: std::cell::Cell::new(0),
                refused: std::cell::Cell::new(0),
                kick_refused: std::cell::Cell::new(false),
                write_fails: std::cell::Cell::new(false),
                exiting: std::cell::Cell::new(0),
                calls: std::cell::RefCell::new(Vec::new()),
                pauses: std::cell::Cell::new(0),
            }
        }

        fn held(&self) -> bool {
            self.loaded.get() || self.letting_go.get() > 0
        }

        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }

        fn count(&self, verb: &str) -> usize {
            self.calls.borrow().iter().filter(|c| *c == verb).count()
        }

        fn on_disk(&self) -> String {
            self.plist.borrow().clone()
        }

        fn running_from(&self) -> Option<String> {
            self.loaded.get().then(|| self.loaded_from.borrow().clone())
        }
    }

    /// The `ProgramArguments` of plist `text`, as `on` writes them (one `<string>` a line); none
    /// for a text that is no such plist.
    fn plist_arguments(text: &str) -> Vec<String> {
        let Some((_, rest)) = text.split_once("<key>ProgramArguments</key>") else {
            return Vec::new();
        };
        let array = rest.split_once("</array>").map_or(rest, |(array, _)| array);
        array
            .lines()
            .filter_map(|l| l.trim().strip_prefix("<string>")?.strip_suffix("</string>"))
            .map(|a| {
                a.replace("&lt;", "<")
                    .replace("&gt;", ">")
                    .replace("&quot;", "\"")
                    .replace("&apos;", "'")
                    .replace("&amp;", "&")
            })
            .collect()
    }

    impl Launchd for FakeLaunchd {
        fn print(&self, label: &str) -> (Option<i32>, String) {
            self.calls.borrow_mut().push("print".to_string());
            if self.unread.get() > 0 {
                self.unread.set(self.unread.get() - 1);
                return (None, String::new());
            }
            let held = self.held();
            self.letting_go.set(self.letting_go.get().saturating_sub(1));
            if !held {
                return (Some(LAUNCHCTL_NO_SERVICE), String::new());
            }
            // launchd lists the arguments of the plist it loaded the job from, where it has some.
            let args = plist_arguments(&self.loaded_from.borrow());
            let listed = if args.is_empty() {
                String::new()
            } else {
                format!(
                    "\targuments = {{\n{}\t}}\n",
                    args.iter()
                        .map(|a| format!("\t\t{a}\n"))
                        .collect::<String>()
                )
            };
            (
                Some(0),
                format!("gui/501/{label} = {{\n\tstate = running\n{listed}}}\n"),
            )
        }

        fn run(&self, args: &[&str]) -> Result<(), String> {
            self.calls.borrow_mut().push(args[0].to_string());
            let failed = |code: i32, why: &str| {
                Err(format!("launchctl {} exited {code}: {why}", args.join(" ")))
            };
            let load = || {
                self.loaded.set(true);
                *self.loaded_from.borrow_mut() = self.on_disk();
            };
            match args[0] {
                "bootout" if self.loaded.get() => {
                    self.loaded.set(false);
                    self.letting_go.set(self.lingers);
                    Ok(())
                }
                "bootout" => failed(3, "Boot-out failed: 3: No such process"),
                "bootstrap" if self.held() => failed(5, "Bootstrap failed: 5: Input/output error"),
                "bootstrap" if self.refusals.get() > 0 => {
                    self.refusals.set(self.refusals.get() - 1);
                    self.refused.set(self.refused.get() + 1);
                    if self.loads_anyway.get() == self.refused.get() {
                        load();
                    }
                    failed(5, "Bootstrap failed: 5: Input/output error")
                }
                "bootstrap" => {
                    load();
                    Ok(())
                }
                "kickstart" if self.kick_refused.get() => failed(119, "Service is disabled"),
                "kickstart" if self.loaded.get() => {
                    self.exiting.set(self.lingers);
                    Ok(())
                }
                "kickstart" => failed(113, "Could not find service"),
                other => panic!("launchctl {other} is not the broker step's"),
            }
        }

        fn write(&self, plist: &Path, text: &str) -> Result<(), String> {
            self.calls.borrow_mut().push("write".to_string());
            if self.write_fails.get() {
                return Err(format!(
                    "{}: Permission denied (os error 13)",
                    plist.display()
                ));
            }
            *self.plist.borrow_mut() = text.to_string();
            Ok(())
        }

        fn running(&self, pid: u32) -> bool {
            assert_eq!(pid, 4242, "only the pid the job ran is waited for");
            let running = self.exiting.get() > 0;
            self.exiting.set(self.exiting.get().saturating_sub(1));
            running
        }

        fn pause(&self, _pause: Duration) {
            self.pauses.set(self.pauses.get() + 1);
        }
    }

    const FAKE_PLIST: &str = "/nonexistent/LaunchAgents/systems.alab.astream-broker.x.plist";
    const FAKE_LABEL: &str = "systems.alab.astream-broker.x";

    fn rewrite(old: Option<&str>) -> Rewrite {
        Rewrite {
            text: "new".to_string(),
            old: old.map(str::to_string),
        }
    }

    /// The renewal measured on m22 after an update: a loaded job booted out and bootstrapped at
    /// once failed (launchd's bootout returns before the job is gone) and left the job unloaded.
    /// The step now boots a job whose plist changed out, waits for launchd to let it go, and only
    /// then writes the plist and bootstraps it, once more after a failed bootstrap; an unchanged
    /// plist restarts in place with `kickstart -k`, never unloaded, once the process it killed
    /// exits, and a kickstart of a job launchd let go meanwhile loads the plist; a job is never
    /// booted out twice, so one a failed bootstrap loaded all the same stays loaded.
    /// NEGATIVE CONTROLS: a step that bootstraps with no wait (`let_go` answering `true` at once)
    /// bootstraps against a job launchd still holds; one that boots the job out again before its
    /// retry (as the handoff's did) unloads the job a failed bootstrap loaded; one that takes a
    /// kickstart's return for the old process's exit does not wait for it; one that retries with
    /// no pause, or writes the plist before the bootout, is caught by the calls.
    #[test]
    fn a_reload_waits_for_launchd_to_let_the_job_go_and_an_unchanged_job_restarts_in_place() {
        let (plist, label) = (Path::new(FAKE_PLIST), FAKE_LABEL);

        let changed = FakeLaunchd::new(true, 3);
        assert_eq!(
            launchd_up(
                &changed,
                label,
                plist,
                (true, Some(4242)),
                Some(&rewrite(Some("old")))
            ),
            Ok(Up::Loaded(None))
        );
        assert_eq!(
            changed.calls(),
            [
                "bootout",
                "print",
                "print",
                "print",
                "print",
                "write",
                "bootstrap"
            ],
            "written and bootstrapped once launchd let the job go"
        );
        assert_eq!(changed.pauses.get(), 3, "the wait polled");
        assert_eq!(changed.running_from().as_deref(), Some("new"));

        // `print` stops naming the job before launchd takes the label again: the first bootstrap
        // fails, and the step tries once more, a pause later, never booting the job out again.
        let busy = FakeLaunchd::new(true, 1);
        busy.refusals.set(1);
        let up = launchd_up(
            &busy,
            label,
            plist,
            (true, Some(4242)),
            Some(&rewrite(Some("old"))),
        );
        assert!(
            matches!(&up, Ok(Up::Loaded(Some(first))) if first.contains("launchctl bootstrap")
                && first.contains("5: Input/output error")),
            "{up:?}"
        );
        assert_eq!(
            (busy.count("bootout"), busy.count("bootstrap")),
            (1, 2),
            "{:?}",
            busy.calls()
        );
        assert_eq!(
            busy.pauses.get(),
            2,
            "the wait's look, then the retry's pause"
        );
        assert_eq!(busy.running_from().as_deref(), Some("new"));

        // A bootstrap that fails and loads the job all the same: launchd holds the plist's job,
        // which stays loaded for the wait for the broker to judge.
        let loaded_anyway = FakeLaunchd::new(true, 1);
        loaded_anyway.refusals.set(1);
        loaded_anyway.loads_anyway.set(1);
        let up = launchd_up(
            &loaded_anyway,
            label,
            plist,
            (true, Some(4242)),
            Some(&rewrite(Some("old"))),
        );
        assert!(
            matches!(&up, Ok(Up::Held(failed)) if failed.contains("5: Input/output error")),
            "{up:?}"
        );
        assert_eq!(
            (
                loaded_anyway.count("bootout"),
                loaded_anyway.count("bootstrap")
            ),
            (1, 1),
            "never booted out twice: {:?}",
            loaded_anyway.calls()
        );
        assert_eq!(loaded_anyway.running_from().as_deref(), Some("new"));

        // `kickstart -k` returns while the process it killed still runs: the step waits for it.
        let unchanged = FakeLaunchd::new(true, 3);
        assert_eq!(
            launchd_up(&unchanged, label, plist, (true, Some(4242)), None),
            Ok(Up::InPlace)
        );
        assert_eq!(unchanged.calls(), ["kickstart"], "never unloaded");
        assert_eq!(
            unchanged.pauses.get(),
            3,
            "the killed process was waited for"
        );
        let lingering = FakeLaunchd::new(true, 3 * GONE_POLLS);
        let why = launchd_up(&lingering, label, plist, (true, Some(4242)), None)
            .expect_err("the killed process never exits");
        assert!(
            why.contains("still ran") && why.contains("kickstart -k"),
            "{why}"
        );
        assert_eq!(lingering.calls(), ["kickstart"], "never unloaded: {why}");

        // The job went between the look and the kickstart: the plist is loaded, nothing booted out.
        let went = FakeLaunchd::new(false, 3);
        let up = launchd_up(&went, label, plist, (true, Some(4242)), None);
        assert!(
            matches!(&up, Ok(Up::Loaded(Some(first))) if first.contains("launchctl kickstart -k")),
            "{up:?}"
        );
        assert_eq!(went.calls(), ["kickstart", "print", "bootstrap"]);

        let unloaded = FakeLaunchd::new(false, 3);
        assert_eq!(
            launchd_up(&unloaded, label, plist, (false, None), None),
            Ok(Up::Loaded(None))
        );
        assert_eq!(unloaded.calls(), ["bootstrap"], "nothing to boot out");
        let unloaded = FakeLaunchd::new(false, 3);
        assert_eq!(
            launchd_up(
                &unloaded,
                label,
                plist,
                (false, None),
                Some(&rewrite(Some("old")))
            ),
            Ok(Up::Loaded(None))
        );
        assert_eq!(unloaded.calls(), ["write", "bootstrap"]);
        assert_eq!(unloaded.running_from().as_deref(), Some("new"));
    }

    /// The step leaves no job it found loaded unloaded, nor a plist newer than the job launchd
    /// holds: a job launchd never lets go is neither rewritten nor bootstrapped over; a plist
    /// that does not load is replaced by the one it replaced, loaded again; a write that fails
    /// loads the plist as it was; a kickstart refused for a loaded job (a disabled one) leaves it
    /// loaded; a `print` that says nothing is waited out, never read as the job gone; and a
    /// bootstrap that keeps failing names each verb's error and says the plist is not loaded.
    /// NEGATIVE CONTROLS: a step with no rollback, one that writes the plist before the job is
    /// gone, one that boots out a job whose kickstart was refused, and one that reads a silent
    /// `print` as the job gone each fail it.
    #[test]
    fn a_load_that_fails_never_strands_the_job() {
        let (plist, label) = (Path::new(FAKE_PLIST), FAKE_LABEL);
        let eio = "Bootstrap failed: 5: Input/output error";

        let refused = FakeLaunchd::new(true, 2);
        refused.refusals.set(2);
        let why = launchd_up(
            &refused,
            label,
            plist,
            (true, Some(4242)),
            Some(&rewrite(Some("old"))),
        )
        .expect_err("the new plist does not load");
        assert!(why.contains("put back") && why.contains(eio), "{why}");
        assert!(
            why.contains(&format!(
                "launchctl bootstrap gui/{} {} exited 5: {eio}",
                uid(),
                plist.display()
            )),
            "the failing verb and its stderr: {why}"
        );
        assert_eq!(refused.on_disk(), "old", "{why}");
        assert_eq!(refused.running_from().as_deref(), Some("old"), "{why}");
        assert_eq!(
            (refused.count("bootout"), refused.count("bootstrap")),
            (1, 3),
            "{:?}",
            refused.calls()
        );
        assert_eq!(
            refused.pauses.get(),
            4,
            "two looks, the retry's pause and the put-back's"
        );

        // The plist put back does not load either: said so, and never called the new one.
        let refused_again = FakeLaunchd::new(true, 2);
        refused_again.refusals.set(3);
        let why = launchd_up(
            &refused_again,
            label,
            plist,
            (true, Some(4242)),
            Some(&rewrite(Some("old"))),
        )
        .expect_err("neither plist loads");
        assert!(
            why.contains("(the plist it replaced, put back) is installed but launchd has not")
                && why.ends_with(eio),
            "{why}"
        );
        assert_eq!(refused_again.on_disk(), "old", "{why}");
        assert_eq!(refused_again.running_from(), None, "{why}");

        // The plist put back fails to bootstrap and loads all the same: launchd holds that one.
        let held_old = FakeLaunchd::new(true, 2);
        held_old.refusals.set(3);
        held_old.loads_anyway.set(3);
        let why = launchd_up(
            &held_old,
            label,
            plist,
            (true, Some(4242)),
            Some(&rewrite(Some("old"))),
        )
        .expect_err("the new plist does not load");
        assert!(
            why.contains("put back, and launchd holds the job after a bootstrap of it"),
            "{why}"
        );
        assert_eq!(held_old.running_from().as_deref(), Some("old"), "{why}");

        // `print` does not say after the bootstraps fail: launchd may hold the new plist's job,
        // so the old one is not written over it.
        let unsaid = FakeLaunchd::new(false, 0);
        unsaid.refusals.set(2);
        unsaid.unread.set(2);
        let why = launchd_up(
            &unsaid,
            label,
            plist,
            (false, None),
            Some(&rewrite(Some("old"))),
        )
        .expect_err("the new plist does not load");
        assert!(why.contains("(or `launchctl print` did not say)"), "{why}");
        assert_eq!(
            unsaid.calls(),
            ["write", "bootstrap", "print", "bootstrap", "print"],
            "{why}"
        );
        assert_eq!(unsaid.on_disk(), "new", "{why}");

        let fresh = FakeLaunchd::new(false, 0);
        fresh.refusals.set(2);
        let why = launchd_up(&fresh, label, plist, (false, None), Some(&rewrite(None)))
            .expect_err("nothing to put back");
        assert!(
            why.contains("is installed but launchd has not loaded it") && why.ends_with(eio),
            "the last verb's error ends it: {why}"
        );
        assert_eq!(
            fresh.calls(),
            ["write", "bootstrap", "print", "bootstrap", "print"]
        );

        let unwritable = FakeLaunchd::new(true, 1);
        unwritable.write_fails.set(true);
        let why = launchd_up(
            &unwritable,
            label,
            plist,
            (true, Some(4242)),
            Some(&rewrite(Some("old"))),
        )
        .expect_err("the plist could not be written");
        assert!(
            why.contains("could not be written") && why.contains("Permission denied"),
            "{why}"
        );
        assert_eq!(unwritable.running_from().as_deref(), Some("old"), "{why}");
        // The plist as it was fails to bootstrap and loads all the same: still not written.
        let unwritable = FakeLaunchd::new(true, 1);
        unwritable.write_fails.set(true);
        unwritable.refusals.set(1);
        unwritable.loads_anyway.set(1);
        let why = launchd_up(
            &unwritable,
            label,
            plist,
            (true, Some(4242)),
            Some(&rewrite(Some("old"))),
        )
        .expect_err("the plist could not be written");
        assert!(
            why.contains("could not be written") && why.contains("5: Input/output error"),
            "{why}"
        );
        assert_eq!(unwritable.running_from().as_deref(), Some("old"), "{why}");

        let disabled = FakeLaunchd::new(true, 3);
        disabled.kick_refused.set(true);
        let why = launchd_up(&disabled, label, plist, (true, Some(4242)), None)
            .expect_err("kickstart refused");
        assert!(
            why.contains("left loaded as it was") && why.contains("119: Service is disabled"),
            "{why}"
        );
        assert_eq!(
            disabled.calls(),
            ["kickstart", "print"],
            "no bootout: {why}"
        );
        assert!(disabled.loaded.get());
        // A kickstart refused while `print` says nothing: the job may be held, so it is left.
        let unsaid = FakeLaunchd::new(true, 3);
        unsaid.kick_refused.set(true);
        unsaid.unread.set(1);
        let why = launchd_up(&unsaid, label, plist, (true, Some(4242)), None)
            .expect_err("kickstart refused");
        assert!(why.contains("left loaded as it was"), "{why}");
        assert_eq!(unsaid.calls(), ["kickstart", "print"], "no bootout: {why}");

        // `print` says nothing twice while launchd still lets the job go: waited out.
        let silent = FakeLaunchd::new(true, 1);
        silent.unread.set(2);
        assert_eq!(
            launchd_up(
                &silent,
                label,
                plist,
                (true, Some(4242)),
                Some(&rewrite(Some("old")))
            ),
            Ok(Up::Loaded(None)),
            "{:?}",
            silent.calls()
        );
        assert_eq!(silent.count("bootstrap"), 1);

        let stuck = FakeLaunchd::new(true, 3 * GONE_POLLS);
        let why = launchd_up(
            &stuck,
            label,
            plist,
            (true, Some(4242)),
            Some(&rewrite(Some("old"))),
        )
        .expect_err("launchd never let it go");
        assert!(
            why.contains("launchd still held") && why.contains("neither rewritten nor loaded"),
            "{why}"
        );
        assert_eq!(
            (stuck.count("write"), stuck.count("bootstrap")),
            (0, 0),
            "no write, no bootstrap over a job launchd holds: {why}"
        );
        assert_eq!(stuck.on_disk(), "old");
        assert_eq!(stuck.pauses.get(), GONE_POLLS, "a bounded wait");
    }

    /// The wait for a starting broker connects until one answers and names its process once,
    /// after it answers: no poll that finds nothing names anything.
    #[test]
    fn the_wait_names_the_broker_once_it_answers() {
        let answered = || fabric::BrokerView {
            reachable: true,
            attached: true,
            head: Some(0),
            ..fabric::BrokerView::default()
        };
        let polls = std::cell::Cell::new(0);
        let named = std::cell::RefCell::new(Vec::new());
        let view = poll_broker(
            || {
                polls.set(polls.get() + 1);
                if polls.get() < 3 {
                    fabric::BrokerView::default()
                } else {
                    answered()
                }
            },
            |view: &mut fabric::BrokerView| {
                named.borrow_mut().push(polls.get());
                view.pid = Some(71);
            },
            Duration::from_secs(60),
        )
        .expect("an answer on the third poll");
        assert_eq!(
            named.take(),
            [3],
            "named once, after the poll that answered"
        );
        assert_eq!(view.pid, Some(71));
        let silent = poll_broker(
            || fabric::BrokerView {
                error: Some("connect: refused".to_string()),
                ..fabric::BrokerView::default()
            },
            |_: &mut fabric::BrokerView| panic!("a broker that never answered was named"),
            Duration::ZERO,
        );
        assert_eq!(
            silent.err().and_then(|view| view.error).as_deref(),
            Some("connect: refused")
        );
    }

    /// The wait's poll connects and names no process, though the listener scan finds this
    /// process holding the socket it reached. The test reads the scan's holders rather than the
    /// one broker it names: a child another test is spawning can hold a copy of the listener
    /// until its exec closes it.
    #[cfg(unix)]
    #[test]
    fn the_waits_poll_names_no_process() {
        let dir = aterm_tempfile::Builder::new()
            .tempdir_in("/tmp")
            .expect("a scratch folder short enough for sun_path");
        let p = paths_at(&dir.path().join("root"));
        std::fs::create_dir(&p.root).expect("the root");
        let listener = std::os::unix::net::UnixListener::bind(p.sock()).expect("bind");
        let closer = std::thread::spawn(move || {
            drop(listener.accept());
            listener
        });
        let polled = poll_reach(&p, "n-a");
        assert!(!polled.read_ok(), "{polled:?}");
        assert_eq!(polled.pid, None, "the poll names nothing: {polled:?}");
        let scan = aterm_uds::holders::scan_listener_holders(Path::new(&p.sock()), &[])
            .expect("the listener scan");
        assert!(
            scan.holders.contains(&std::process::id()),
            "the scan finds this process holding the listener the poll reached: {scan:?}"
        );
        drop(closer.join().expect("the closer"));
    }

    #[test]
    fn running_bridge_sees_a_stale_one_and_a_bare_pid() {
        let inst = |fabric: Option<&str>, supervised, pids: Vec<u32>| fabric::InstanceView {
            pid: 42,
            fabric: fabric.map(str::to_string),
            supervised,
            bridge_pids: pids,
            node: Some("n-a".to_string()),
            ..fabric::InstanceView::default()
        };
        assert_eq!(
            running_bridge(&[inst(None, None, vec![99])], "n-a"),
            Some(42),
            "a bridge child exists: that is a running bridge"
        );
        assert_eq!(
            running_bridge(&[inst(Some("stale"), Some(false), vec![])], "n-a"),
            Some(42),
            "a stale link is a bridge that still owns the sequence"
        );
        assert_eq!(
            running_bridge(&[inst(Some("connected"), None, vec![])], "n-a"),
            Some(42)
        );
        assert_eq!(
            running_bridge(&[inst(Some("absent"), Some(false), vec![])], "n-a"),
            None
        );
        assert_eq!(running_bridge(&[], "n-a"), None);
        assert_eq!(
            running_bridge(&[inst(Some("connected"), Some(true), vec![99])], "n-ghost"),
            None,
            "n-a's bridge cannot retire n-ghost's rows"
        );
        let unread = fabric::InstanceView {
            node: None,
            ..inst(Some("connected"), Some(true), vec![99])
        };
        assert_eq!(
            running_bridge(&[unread], "n-a"),
            None,
            "a bridge whose node was not read is nobody's: the state dir's lock decides"
        );
        let standby = fabric::InstanceView {
            pid: 7,
            reason: Some("state-locked".to_string()),
            ..inst(Some("stalled"), Some(true), vec![98])
        };
        assert_eq!(
            running_bridge(
                &[
                    standby.clone(),
                    inst(Some("connected"), Some(true), vec![99])
                ],
                "n-a"
            ),
            Some(42)
        );
        assert_eq!(running_bridge(&[standby], "n-a"), None);
    }

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
        let target = dir.join("elsewhere");
        std::fs::write(&target, "untouched\n").expect("target");
        let linked = shared.join("fabric.toml");
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

    fn paths_at(root: &Path) -> Paths {
        Paths {
            home: root.parent().expect("a parent").to_path_buf(),
            login_home: None,
            root: root.to_path_buf(),
            aterm: "/usr/local/bin/aterm".to_string(),
            fleet: "local".to_string(),
            config: None,
            rendezvous: None,
            durability: None,
        }
    }

    fn dir_mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::symlink_metadata(path)
            .expect("stat")
            .permissions()
            .mode()
            & 0o777
    }

    #[test]
    fn the_root_step_makes_narrows_or_refuses_by_the_private_directory_rule() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let root = dir.path().join("root");
        std::fs::create_dir(&root).expect("root");
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let p = paths_at(&root);
        let state = p.state();

        let mut dry = Out::new(true);
        assert!(root_step(&p, &state, &mut dry));
        assert_eq!((dry.changed, dry.failed), (1, false));
        assert_eq!(dir_mode(&root), 0o755, "a dry run narrows nothing");
        assert!(!state.exists(), "a dry run makes nothing");

        let mut run = Out::new(false);
        assert!(root_step(&p, &state, &mut run));
        assert_eq!((run.changed, run.failed), (1, false));
        assert_eq!(dir_mode(&root), 0o700);
        assert_eq!(dir_mode(&state), 0o700);

        let mut again = Out::new(false);
        assert!(root_step(&p, &state, &mut again));
        assert_eq!((again.changed, again.failed), (0, false));

        let linked = dir.path().join("linked");
        std::os::unix::fs::symlink(&root, &linked).expect("link");
        let p = paths_at(&linked);
        let mut out = Out::new(false);
        assert!(!root_step(&p, &p.state(), &mut out));
        assert!(out.failed);
    }

    /// On macOS a 0700 root carrying an `everyone allow read` entry is open to every user
    /// though its mode says otherwise: the root step refuses it, dry or not, and makes no state
    /// directory in it; the mint secret's writer refuses that root as well.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_root_step_refuses_a_root_an_access_list_opens_to_others() {
        use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let root = dir.path().join("root");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .expect("root");
        let chmod = |args: &[&str]| {
            let ran = aterm_uds::spawnfd::command("/bin/chmod")
                .args(args)
                .arg(&root)
                .status()
                .expect("run chmod");
            assert!(ran.success(), "chmod {args:?}");
        };
        chmod(&["+a", "everyone allow read"]);
        let p = paths_at(&root);
        let state = p.state();
        for dry_run in [true, false] {
            let mut out = Out::new(dry_run);
            assert!(!root_step(&p, &state, &mut out), "dry_run={dry_run}");
            assert!(out.failed);
            assert!(!state.exists(), "dry_run={dry_run}");
        }
        assert_eq!(
            aterm_uds::private_file::write_private(&root, MINT_SECRET, b"secret")
                .expect_err("a root an access list opens")
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(!p.secret().exists());

        chmod(&["-N"]);
        let mut out = Out::new(false);
        assert!(root_step(&p, &state, &mut out));
        assert_eq!(dir_mode(&state), 0o700);
        aterm_uds::private_file::write_private(&root, MINT_SECRET, b"secret").expect("write");
        let meta = std::fs::symlink_metadata(p.secret()).expect("secret");
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }

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
            durability: None,
        };
        assert_eq!(p("/Users//example/.local/share/aterm-fabric").label(), LABEL);
        assert_eq!(
            p("/tmp/atfo-x/root").label(),
            format!("{LABEL}.{}", cksum(b"/tmp/atfo-x/root"))
        );
        assert_ne!(p("/tmp/atfo-x/root").label(), LABEL);
    }

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
        assert_ne!(paths("/Users//example", None).label(), LABEL);
    }

    /// THE LOGIN HOME AND THE UID ARE THE ONES `id` AND THE SHELL ANSWER, on
    /// the live machine, though no child is run for them: [`login_home`] is
    /// `~$(id -un)` as `sh` expands it with no `$HOME` to read, and [`uid`] is
    /// `id -u`.
    #[cfg(unix)]
    #[test]
    fn the_login_home_and_uid_are_what_id_and_the_shell_answer() {
        let run = |program: &str, args: &[&str]| {
            let out = aterm_uds::spawnfd::command(program)
                .args(args)
                .env_remove("HOME")
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output()
                .unwrap_or_else(|e| panic!("run {program}: {e}"));
            assert!(out.status.success(), "{program} {args:?}: {out:?}");
            String::from_utf8(out.stdout)
                .expect("utf-8")
                .trim()
                .to_string()
        };
        let user = run("id", &["-un"]);
        let expanded = run("sh", &["-c", &format!("printf '%s' ~{user}")]);
        assert!(
            Path::new(&expanded).is_absolute(),
            "the shell expanded ~{user} to {expanded:?}"
        );
        assert_eq!(login_home(), Some(PathBuf::from(&expanded)), "~{user}");
        assert_eq!(uid(), run("id", &["-u"]));
    }

    #[test]
    fn the_generated_broker_command_selects_the_open_policy() {
        use crate::durability::Durability;

        let paths = Paths {
            home: PathBuf::from("/fixture-home"),
            login_home: None,
            root: PathBuf::from("/fixture-root"),
            aterm: "/fixture-bin/aterm".to_string(),
            fleet: "local".to_string(),
            config: None,
            rendezvous: None,
            durability: None,
        };
        let expected = Durability::Ordered.options();
        let tier = expected.durability;
        for named in [None, Some(Durability::Ordered)] {
            let paths = Paths {
                durability: named,
                ..paths.clone()
            };
            let argv = paths.broker_argv();
            let serving = Durability::try_of_argv(&argv).expect("a serving broker command");
            let options = serving.options();
            assert_eq!(options.durability, tier);
            assert_eq!(options.escalation, expected.escalation);
        }

        let strict = Paths {
            durability: Some(Durability::Strict),
            ..paths.clone()
        };
        assert_ne!(
            Durability::try_of_argv(&strict.broker_argv())
                .unwrap()
                .tier(),
            tier
        );
        let mut unguarded = paths.broker_argv();
        let guard_at = unguarded
            .iter()
            .position(|word| word == "--secret-file")
            .unwrap();
        unguarded.drain(guard_at..guard_at + 2);
        assert_ne!(Durability::try_of_argv(&unguarded).unwrap().tier(), tier);
    }

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
        assert_eq!(
            bare.tier(),
            Durability::Ordered,
            "this build's default for `on`'s guarded broker"
        );
        assert_eq!(bare.tier(), crate::durability::default_for(true));
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
        assert!(
            crate::broker_args::BrokerArgs::from_invocation(&argv).is_some_and(|args| args.serves(
                &aterm_uds::holders::SocketPath::of(std::path::Path::new(&sock))
            )),
            "the job's argv serves the broker socket: {argv:?}"
        );
        assert_eq!(Durability::of_argv(&argv), Durability::Relaxed);
        assert!(unit_argv(&relaxed.unit_text()).is_some_and(|w| w == argv));

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
        let default = crate::durability::default_for(true);
        assert_eq!(not_current(&bare, &view(Some(default))), None);
        if default != strict {
            assert_eq!(
                not_current(&bare, &view(Some(strict))),
                Some("it runs at durability strict"),
                "a bare job no longer asks for strict"
            );
        }
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
        assert!(
            text.contains(
                "  <key>EnvironmentVariables</key>\n\
             \x20 <dict>\n\
             \x20   <key>PATH</key><string>/Users//example/Library/Application Support/aterm/pkg/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>\n\
             \x20 </dict>\n"
            ),
            "{text}"
        );
        assert!(text.ends_with("</dict>\n</plist>\n"));
        assert!(p.plist_is_current(&text));
        assert_eq!(
            plist_program_arguments(&text).as_deref(),
            Some(p.broker_argv().as_slice())
        );
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
        assert!(!p.plist_is_current(&text.replace("bus.log", "other.log")));
        assert!(!p.plist_is_current(
            &text.replace("astream-broker</string>", "astream-broker.1</string>")
        ));
        assert!(!p.plist_is_current(&text.replace(
            "<key>KeepAlive</key><true/>",
            "<key>KeepAlive</key><false/>"
        )));
        assert!(!p.plist_is_current("<plist/>"));
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
        let unit = p.unit_text();
        assert!(!unit.contains("rm -f"), "{unit}");
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
        let redirected = p("/tmp/atfo-x/root", Some("/Users//example"));
        assert_eq!(
            redirected.plist(),
            PathBuf::from(format!("/tmp/atfo-x/root/{}.plist", redirected.label()))
        );
        assert!(!redirected.plist().starts_with("/Users//example/Library"));
        let foreign = Paths {
            home: PathBuf::from("/tmp/x"),
            ..p("/tmp/x/.local/share/aterm-fabric", Some("/Users//example"))
        };
        assert!(foreign
            .plist()
            .starts_with("/tmp/x/.local/share/aterm-fabric/"));
        let redirected_home = Paths {
            home: PathBuf::from("/tmp/x"),
            ..installed
        };
        assert!(!redirected_home
            .plist()
            .starts_with("/Users//example/Library/LaunchAgents"));
    }

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
        assert_eq!(
            undo_command(
                "aterm",
                &[("ATERM_BIN", Some("/a b/aterm (dev)/aterm".to_string()))]
            ),
            "ATERM_BIN='/a b/aterm (dev)/aterm' aterm fabric off",
            "a value with a space in it is one shell word"
        );
    }

    #[test]
    fn the_config_edit_touches_only_the_command_key() {
        let line = "aterm link serve --fleet local --broker /b.sock";
        let cmd = &words(line);
        let before = "font_px = 12.0\n\n[fabric]\n# a comment\ncommand = \"old\"\npresence = \"meta\"\n\n[keys]\nx = 1\n";
        let (after, changed) = set_fabric_command(before, cmd).expect("ok");
        assert!(changed);
        assert_eq!(
            after,
            format!(
                "font_px = 12.0\n\n[fabric]\ncommand = \"{line}\"\nreceipts = true\n# a comment\npresence = \"meta\"\n\n[keys]\nx = 1\n"
            )
        );
        assert_eq!(fabric::command_in_toml(&after), Ok(Some(cmd.clone())));
        assert_eq!(fabric::receipts_in_toml(&after), Ok(Some(true)));
        assert!(!set_fabric_command(&after, cmd).expect("ok").1);
        let off = format!("[fabric]\ncommand = \"{line}\"\nreceipts = false\n");
        assert!(!set_fabric_command(&off, cmd).expect("ok").1);
        let (after, _) =
            set_fabric_command(&off, &words("aterm link serve --fleet x --broker /y")).expect("ok");
        assert_eq!(fabric::receipts_in_toml(&after), Ok(Some(false)));
        assert_eq!(after.matches("receipts").count(), 1);
        let multi = "[fabric]\ncommand = \"\"\"aterm link serve --fleet x \\\n   --broker /y\"\"\"\nk = 1\n[z]\n";
        let (after, _) = set_fabric_command(multi, cmd).expect("ok");
        assert_eq!(
            after,
            format!("[fabric]\ncommand = \"{line}\"\nreceipts = true\nk = 1\n[z]\n")
        );
        let (after, changed) = set_fabric_command("font_px = 1.0\n", cmd).expect("ok");
        assert!(changed);
        assert!(after.starts_with("font_px = 1.0\n\n[fabric]\n# Written by `aterm fabric on`"));
        assert_eq!(fabric::command_in_toml(&after), Ok(Some(cmd.clone())));
        assert_eq!(fabric::receipts_in_toml(&after), Ok(Some(true)));
        let (after, _) = set_fabric_command("", cmd).expect("ok");
        assert!(after.starts_with("[fabric]\n"));
        assert!(set_fabric_command("[fabric\ncommand = \"x\"\n", cmd).is_err());
        assert!(set_fabric_command("fabric.command = \"x\"\n", cmd).is_err());
        assert!(set_fabric_command("fabric = { command = \"x\" }\n", cmd).is_err());
        let (after, _) = set_fabric_command("[other]\nfabric.command = \"x\"\n", cmd).expect("ok");
        assert!(after.contains("[other]\nfabric.command = \"x\"\n\n[fabric]\n"));

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
        let subs = "[fabric.presence]\nx = 1\n[a]\ny = 2\n[fabric] # on\ncommand = \"x\"\n[fabric.more]\nz = 3\n[keys]\nk = 1\n";
        let (after, removed) = remove_fabric_table(subs).expect("ok");
        assert_eq!(removed, ["[fabric.presence]", "[fabric]", "[fabric.more]"]);
        assert_eq!(after, "[a]\ny = 2\n[keys]\nk = 1\n");
        assert!(fabric::command_in_toml(&after).expect("valid").is_none());
        assert_eq!(
            remove_fabric_table("[fabric.presence]\nx = 1\n"),
            Ok((String::new(), vec!["[fabric.presence]".to_string()]))
        );
        assert!(remove_fabric_table("[fabrics]\nx = 1\n")
            .expect("ok")
            .1
            .is_empty());
        for broken in [
            "[fabric]\ncommand = \"x\"\n[keys\nbind = \"y\"\n[update]\nenabled = false\n",
            "[fabric]\ncommand = \"x\"\n[[keys]\nbind = \"y\"\n[update]\nenabled = false\n",
        ] {
            assert!(remove_fabric_table(broken).is_err(), "{broken}");
        }
    }

    #[test]
    fn the_config_edit_reads_headers_and_keys_the_way_toml_does() {
        let cmd = &words("aterm link serve --fleet local --broker /b.sock");
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
                Ok(Some(cmd.clone())),
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

    /// A `[fabric]` edit whose folder flush fails after its rename is in effect: `on`'s and
    /// `off`'s config steps say ok beside a WARNING, not FAILED, and the run goes on. A flush
    /// that fails before the edit (its `.bak`'s) still fails the step, aterm.toml unchanged.
    /// With every replace error said as FAILED, as before, the step returned false.
    #[test]
    fn an_edit_in_effect_whose_folder_flush_failed_is_ok_beside_a_warning() {
        use aterm_durable::seam::{FailingDirs, EIO};
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let config = dir.path().join("aterm.toml");
        std::fs::write(&config, "font_px = 12.0\n").expect("seed");
        let command = |config: &Path| {
            fabric::command_in_toml(&std::fs::read_to_string(config).expect("config"))
        };
        let cmd = &words("aterm link serve --fleet local --broker /b.sock");

        let mut on = Out::new(false);
        {
            let _edits = FailingDirs::after(1, EIO);
            assert!(config_step(Some(&config), cmd, &mut on), "the step is done");
        }
        assert!(!on.failed, "nothing FAILED");
        assert_eq!(
            on.warned.get(),
            1,
            "one WARNING: a power loss may take it back"
        );
        assert_eq!(
            command(&config),
            Ok(Some(cmd.clone())),
            "the edit is in effect"
        );

        let mut off = Out::new(false);
        {
            let _edits = FailingDirs::after(1, EIO);
            off_config_step(Some(&config), &mut off);
        }
        assert!(!off.failed);
        assert_eq!(off.warned.get(), 1);
        assert_eq!(command(&config), Ok(None), "the removal is in effect");

        let mut early = Out::new(false);
        {
            let _baks = FailingDirs::with(EIO);
            assert!(!config_step(Some(&config), cmd, &mut early));
        }
        assert!(early.failed, "the .bak's flush failed before any edit");
        assert_eq!(early.warned.get(), 0);
        assert_eq!(command(&config), Ok(None), "aterm.toml unchanged");
    }

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
        let cmd = &words("aterm link serve --fleet local --broker /b.sock");

        assert!(config_step(Some(&config), cmd, &mut Out::new(false)));
        let linked = |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_symlink());
        assert!(linked(&config), "the link was replaced by a file");
        let text = std::fs::read_to_string(&target).expect("target");
        assert_eq!(fabric::command_in_toml(&text), Ok(Some(cmd.clone())));
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
        std::fs::write(&target, "font_px = 18.0\n").expect("the other writer");
        edit.replace("font_px = 9.0\n")
            .expect_err("changed under it");
        assert_eq!(
            std::fs::read_to_string(&target).expect("target"),
            "font_px = 18.0\n"
        );
        assert_eq!(
            std::fs::read_to_string(dotfiles.join("aterm.toml.bak")).expect("bak"),
            "font_px = 12.0\n"
        );
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

    #[test]
    fn a_no_op_through_a_link_into_a_read_only_dir_reads_already() {
        use std::os::unix::fs::PermissionsExt;
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let store = dir.path().join("store");
        std::fs::create_dir_all(&store).expect("store");
        let cmd = &words("aterm link serve --fleet local --broker /b.sock");
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

    #[test]
    fn the_rendezvous_file_round_trips() {
        let r = Rendezvous {
            fleet: "local".to_string(),
            broker: "/tmp/x/bus.sock".to_string(),
            node: "n-0123456789abcdef".to_string(),
            cap_file: "/tmp/x/node.cap".to_string(),
            state: "/tmp/x/link-state".to_string(),
            command: words("/x/aterm link serve --fleet local --broker /tmp/x/bus.sock"),
        };
        let text = r.render();
        assert!(
            text.contains(
                "command = \"/x/aterm link serve --fleet local --broker /tmp/x/bus.sock\"\n"
            ),
            "a plain argv is written as the line it always was:\n{text}"
        );
        assert_eq!(Rendezvous::parse(&text), Ok(r.clone()));
        assert!(
            Rendezvous::parse(&text.replace("node = \"n-0123456789abcdef\"", "node = 1")).is_err(),
            "a field of the wrong type"
        );
        let odd = Rendezvous {
            command: words("a \"quoted\" \\ back"),
            ..r.clone()
        };
        assert_eq!(Rendezvous::parse(&odd.render()), Ok(odd));
        let text = r.render().replace(
            "command = \"/x/aterm link serve --fleet local --broker /tmp/x/bus.sock\"",
            "command = [\"/a b/aterm\", \"link\", \"serve\"]",
        );
        assert_eq!(
            Rendezvous::parse(&text).map(|r| r.command),
            Ok(vec![
                "/a b/aterm".to_string(),
                "link".to_string(),
                "serve".to_string()
            ])
        );
        assert!(Rendezvous::parse("fleet = 1\n").is_err());
        assert!(Rendezvous::parse("[x\n").is_err());
    }

    #[test]
    fn the_eight_grants_are_the_node_ring() {
        let g = node_grants("local", "n-a");
        assert_eq!(
            g,
            [
                "rw,p=n-a:/f/local/pub/n-a/>",
                "ro:/f/local/pub/>",
                "ro:/f/local/fleet/>",
                "rw,p=n-a:/f/local/in/*/*/n-a/*",
                "ro:/f/local/in/n-a/>",
                "rw,p=n-a:/f/local/cur/n-a/>",
                "ro:/f/local/term/n-a/>",
                "rw,p=n-a:/f/local/term/n-a/*/screen",
            ]
        );
        let other = node_grants("lab", "n-b");
        assert_eq!(
            other,
            g.iter()
                .map(|x| x.replace("/local/", "/lab/").replace("n-a", "n-b"))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_word_no_supervisor_job_can_carry_is_refused() {
        for ok in [
            "/Users//u/.local/share/aterm-fabric",
            "/Users//u/Caf\u{e9}",
            "/x/aterm-link",
        ] {
            assert!(argv_safe(ok).is_ok(), "{ok}");
        }
        for (bad, why) in [
            ("/Users//u/Applications/aterm (dev).app/aterm", "whitespace"),
            ("/a\tb", "whitespace"),
            ("/nb\u{a0}sp", "whitespace"),
            ("/Users//Bob s", "whitespace"),
            ("/Users//Bob's", "a `'`"),
            ("/Users//u/fab\"ric", "a `\"`"),
            ("/Users//u/fab\\ric", "a `\\`"),
            ("/s/100%41", "a `%`"),
            ("/s/$HOME", "a `$`"),
            ("/a\u{7}b", "a control character"),
        ] {
            let err = argv_safe(bad).expect_err(bad);
            assert!(err.contains(why), "{bad:?}: {err}");
            let fixed: String = bad
                .chars()
                .filter(|&c| !(c.is_whitespace() || c.is_control() || "'\"\\%$".contains(c)))
                .collect();
            assert!(argv_safe(&fixed).is_ok(), "{fixed:?}");
        }
    }

    #[test]
    fn an_aterm_path_that_is_not_plain_runs_through_a_link_in_the_root() {
        let p = |aterm: &str| Paths {
            home: PathBuf::from("/Users//u"),
            login_home: Some(PathBuf::from("/Users//u")),
            root: PathBuf::from("/Users//u/.local/share/aterm-fabric"),
            aterm: aterm.to_string(),
            fleet: "local".to_string(),
            config: None,
            rendezvous: None,
            durability: None,
        };
        let link = "/Users//u/.local/share/aterm-fabric/bin/aterm";
        for aterm in [
            "/Users//u/Applications/aterm (dev).app/Contents/MacOS/aterm",
            "/Users//Bob's apps/aterm",
            "/Users//u/100%/aterm",
        ] {
            let p = p(aterm);
            assert_eq!(p.program(), link, "{aterm}");
            assert_eq!(p.program_link(), PathBuf::from(link));
            let bridge = p.bridge_command("n-a");
            let broker = p.broker_argv();
            assert_eq!(bridge[0], link);
            assert_eq!(broker[0], link);
            for word in bridge.iter().chain(&broker) {
                assert_eq!(argv_safe(word), Ok(()), "{aterm}: {word}");
            }
            assert!(aterm_types::fabric_command::line_spells(&bridge));
            let unit = p.unit_text();
            assert_eq!(
                unit_argv(&unit).as_deref(),
                Some(broker.as_slice()),
                "{unit}"
            );
            assert!(!unit.contains(aterm) && !p.plist_text().contains(aterm));
        }
        let shim = p("/x y/target/debug/aterm-link");
        assert!(shim.is_link_shim());
        assert_eq!(
            shim.program(),
            "/Users//u/.local/share/aterm-fabric/bin/aterm-link"
        );
        assert_eq!(shim.bridge_command("n-a")[1], "serve");
        let plain = p("/Users//u/.local/bin/aterm");
        assert_eq!(plain.program(), "/Users//u/.local/bin/aterm");
        assert_eq!(plain.broker_argv()[0], "/Users//u/.local/bin/aterm");
    }

    /// `Paths::resolve` names an `aterm` other than this process for the broker where the seam is
    /// not blank or `$PATH` holds an executable `aterm`: a blank seam, a file that is not
    /// executable, a folder named `aterm`, a `$PATH` that does not hold it, and no `$PATH` name
    /// none (`converge` then turns the fabric on only from the installed app).
    /// NEGATIVE CONTROLS: a read that takes a blank seam for one names a program for any build; one
    /// that takes any `aterm` entry for a program names one a `$PATH` cannot run.
    #[test]
    fn an_aterm_is_named_on_path_only_where_one_runs_there() {
        use std::ffi::OsStr;
        use std::os::unix::fs::PermissionsExt as _;
        let scratch = aterm_tempfile::tempdir().expect("exclusive scratch");
        let dir = scratch.path();
        let (runs, plain, folder, none) = (
            dir.join("runs"),
            dir.join("plain"),
            dir.join("folder"),
            dir.join("none"),
        );
        for d in [&runs, &plain, &folder, &none] {
            std::fs::create_dir(d).expect("scratch");
        }
        for (d, mode) in [(&runs, 0o755), (&plain, 0o644)] {
            std::fs::write(d.join("aterm"), "#!/bin/sh\n").expect("an aterm");
            std::fs::set_permissions(d.join("aterm"), std::fs::Permissions::from_mode(mode))
                .expect("chmod");
        }
        std::fs::create_dir(folder.join("aterm")).expect("a folder named aterm");
        let path = |dirs: &[&PathBuf]| std::env::join_paths(dirs).expect("a $PATH");

        assert!(names_aterm_on(None, Some(&path(&[&none, &runs]))));
        assert!(names_aterm_on(Some(OsStr::new("/x/aterm")), None));
        assert!(!names_aterm_on(
            None,
            Some(&path(&[&none, &plain, &folder]))
        ));
        assert!(!names_aterm_on(None, None));
        for blank in ["", "  "] {
            assert!(
                !names_aterm_on(Some(OsStr::new(blank)), Some(&path(&[&none]))),
                "{blank:?}"
            );
        }
    }

    #[test]
    fn the_program_step_links_a_path_that_is_not_plain_and_replaces_no_file() {
        let scratch = aterm_tempfile::tempdir().expect("exclusive scratch");
        let dir = scratch.path();
        let spaced = dir.join("a b");
        std::fs::create_dir_all(&spaced).expect("scratch");
        let aterm = spaced.join("aterm");
        std::fs::write(&aterm, "#!/bin/sh\necho linked \"$@\"\n").expect("a program");
        std::fs::set_permissions(&aterm, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .expect("chmod");
        let p = Paths {
            home: dir.to_path_buf(),
            login_home: None,
            root: dir.join("root"),
            aterm: aterm.to_string_lossy().into_owned(),
            fleet: "local".to_string(),
            config: None,
            rendezvous: None,
            durability: None,
        };
        std::fs::create_dir(&p.root).expect("the root step precedes the program step");
        let link = p.program_link();
        assert!(program_step(&p, &mut Out::new(true)));
        assert!(std::fs::symlink_metadata(&link).is_err());
        let mut out = Out::new(false);
        assert!(program_step(&p, &mut out));
        assert_eq!(out.changed, 1);
        assert_eq!(std::fs::read_link(&link).expect("a link"), aterm);
        assert_eq!(
            std::fs::read_to_string(&link).expect("read the linked file"),
            "#!/bin/sh\necho linked \"$@\"\n"
        );
        let mut again = Out::new(false);
        assert!(program_step(&p, &mut again));
        assert_eq!(again.changed, 0, "already");
        std::fs::remove_file(&link).expect("unlink");
        std::os::unix::fs::symlink("/elsewhere", &link).expect("a stale link");
        assert!(program_step(&p, &mut Out::new(false)));
        assert_eq!(std::fs::read_link(&link).expect("a link"), aterm);
        std::fs::remove_file(&link).expect("unlink");
        std::fs::write(&link, "mine").expect("a file");
        let mut refused = Out::new(false);
        assert!(!program_step(&p, &mut refused));
        assert!(refused.failed);
        assert_eq!(std::fs::read_to_string(&link).expect("kept"), "mine");
        std::fs::remove_file(&link).expect("unlink");
        let bin = link.parent().expect("bin");
        std::fs::remove_dir(bin).expect("empty physical bin");
        let outside = dir.join("outside");
        std::fs::create_dir(&outside).expect("owned external directory");
        std::fs::set_permissions(
            &outside,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .expect("external mode");
        let marker = outside.join("mine");
        std::fs::write(&marker, "external contents").expect("external contents");
        let external_link = outside.join("aterm");
        std::os::unix::fs::symlink(&aterm, &external_link).expect("matching external link");
        std::os::unix::fs::symlink(&outside, bin).expect("aliased bin");
        let mode = std::os::unix::fs::PermissionsExt::mode(
            &std::fs::metadata(&outside)
                .expect("external metadata")
                .permissions(),
        );
        let mut matching = Out::new(false);
        let matching_refused = !program_step(&p, &mut matching);
        let mut dry = Out::new(true);
        let dry_refused = !program_step(&p, &mut dry);
        std::fs::remove_file(&external_link).expect("replace owned external link");
        let other = std::path::Path::new("/not-the-selected-program");
        std::os::unix::fs::symlink(other, &external_link).expect("different external link");
        let mut modifying = Out::new(false);
        let modifying_refused = !program_step(&p, &mut modifying);
        assert!(matching_refused && matching.failed && dry_refused && dry.failed);
        assert!(modifying_refused && modifying.failed);
        assert_eq!(std::fs::read_link(bin).expect("bin kept"), outside);
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(
                &std::fs::metadata(&outside)
                    .expect("external metadata")
                    .permissions(),
            ),
            mode,
            "the external directory's mode is untouched"
        );
        assert_eq!(
            std::fs::read_to_string(&marker).expect("external file kept"),
            "external contents"
        );
        assert_eq!(
            std::fs::read_link(&external_link).expect("external link kept"),
            other
        );
        assert_eq!(
            std::fs::read_dir(&outside)
                .expect("external listing")
                .count(),
            2
        );
        std::fs::remove_file(bin).expect("remove our bin alias");
        std::fs::create_dir(bin).expect("restore physical bin");
        let plain = Paths {
            aterm: "/usr/bin/true".to_string(),
            ..p
        };
        let mut nothing = Out::new(false);
        assert!(program_step(&plain, &mut nothing));
        assert_eq!(nothing.changed, 0);
        assert_eq!(
            std::fs::read_dir(link.parent().expect("bin"))
                .expect("bin")
                .count(),
            0,
            "nothing linked"
        );
    }

    #[test]
    fn a_spaced_word_is_never_written_as_an_array() {
        let dev = "/Users//u/Applications/aterm (dev).app/Contents/MacOS/aterm";
        let mut argv = vec![dev.to_string()];
        argv.extend(words("link serve --fleet local --broker /b.sock"));
        let err = set_fabric_command("font_px = 12.0\n", &argv).expect_err("refused");
        assert!(err.contains("whitespace"), "{err}");
        let line = format!("[fabric]\ncommand = \"{}\"\n", argv.join(" "));
        let read = fabric::command_in_toml(&line).expect("ok").expect("some");
        assert_ne!(read, argv, "the line splits the spaced path");
        argv[0] = "/r/bin/aterm".to_string();
        let (after, changed) = set_fabric_command("font_px = 12.0\n", &argv).expect("ok");
        assert!(changed);
        assert!(
            after
                .contains("command = \"/r/bin/aterm link serve --fleet local --broker /b.sock\"\n"),
            "{after}"
        );
        assert_eq!(fabric::command_in_toml(&after), Ok(Some(argv.clone())));
        let plain = words("/usr/bin/aterm link serve --fleet local --broker /b.sock");
        let hand = "[fabric]\ncommand = [\n  \"/a b/aterm\", # the program\n  \"link\", 'serve]',\n]\nreceipts = false\n[keys]\nx = 1\n";
        let (back, changed) = set_fabric_command(hand, &plain).expect("ok");
        assert!(changed);
        assert_eq!(
            back,
            "[fabric]\ncommand = \"/usr/bin/aterm link serve --fleet local --broker /b.sock\"\nreceipts = false\n[keys]\nx = 1\n"
        );
        let same = format!(
            "[fabric]\ncommand = [{}]\nreceipts = true\n",
            plain
                .iter()
                .map(|w| format!("\"{w}\""))
                .collect::<Vec<_>>()
                .join(", ")
        );
        assert!(!set_fabric_command(&same, &plain).expect("ok").1);
        let open = "[fabric]\ncommand = [\"aterm\",\n";
        assert!(set_fabric_command(open, &plain).is_err());
    }

    #[test]
    fn every_warning_shape_has_a_fix() {
        let roles = [
            Role { fleet: None },
            Role {
                fleet: Some("lab".to_string()),
            },
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
            "instance 1: the broker's retention gave up this node's unread mail before its bridge \
             read it (1 range(s) since the instance started): @10..@42 (3 records)",
            "@s-a (instance 1): the broker's retention gave up records of its own before it \
             handled them: its mail lane @50..@60 (`inbox` lane_given_up=)",
            "@s-a (instance 1) has 1 unhandled task/ask older than 10 min",
            "2 held ask(s) past their deadline cannot be judged: the bus no longer keeps (or this \
             cap does not read) every answer, report and ack above them on their asker's lane, so \
             status cannot say whether they had one",
            "@s-a (instance 1) has 3 queued post(s) and no working bridge",
            "instance 1 did not answer (x)",
            "cannot list this machine's aterm instances: x",
            "the bus advertises @s-g live on n-a",
            "h-andrew has a fleet halt standing (reason=stop)",
            "no rendezvous file at /x",
            "instance 1's bridge is another build than the instance: bridge a, instance b",
            "instance 1 could not parse 3 of its bridge's lines (lane_faults=3)",
            "node n-a holds 3072 of the 4096 distinct subjects one node may hold at once: a \
             departed session's presence row and the lanes this node wrote to it go back five \
             minutes after it leaves (its `ev` face once the session-log mirror landed its tail), \
             but each topic a session broadcast on keeps one subject for good, so what stays is \
             held by live sessions, every topic face a session of this node ever broadcast on, \
             mail still owed and the node's own faces; past the bound a new session gets no \
             presence row and its posts are refused",
            "the presence roster has more than 4096 rows and the read stopped there (rows=4096+ \
             truncated): NODES, SESSIONS and every warning drawn from them leave out what lies \
             past it",
            "the broker at /x says its log is POISONED (a flush failed): it refuses every write \
             until the log is reopened, so no record lands; a broker of this build exits within \
             seconds for its supervisor to reopen it",
            "the broker at /x says a flush of its log failed 2 time(s) since it opened: the disk \
             under /x.log is failing writes, and the log was not poisoned when this asked (a \
             failed flush that put acked records at risk poisons it, and the next report says so)",
        ] {
            for role in &roles {
                let fix = fix_for(w, role);
                assert_ne!(fix, "see `aterm help fabric`", "no fix for: {w}");
                if role.fleet.is_some() {
                    assert_eq!(
                        fix.matches("`aterm fabric on").count(),
                        fix.matches("`aterm fabric on --fleet lab`").count(),
                        "{w}: {fix}"
                    );
                }
            }
        }
        assert_eq!(
            fix_for("something new", &Role::default()),
            "see `aterm help fabric`"
        );
    }

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
        let role = Role::default();
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
            let bare = from(None);
            assert!(
                bare.starts_with("nothing listens at the broker's socket")
                    && bare.contains("starts it")
                    && !bare.contains("--repair"),
                "{bare}"
            );
        }
        assert_eq!(
            crate::cli::log_refusal_remedy(K::AlreadyExists, "/r/bus.log"),
            None
        );
    }

    #[test]
    fn a_poisoned_log_is_answered_by_the_brokers_restart() {
        let role = Role::default();
        let quoted = |last: &str| {
            fix_for(
                &format!(
                    "the broker at /r/bus.sock does not answer (connect: x): no record lands and \
                     none is delivered; its supervisor's broker.err, written 1s ago: {last}"
                ),
                &role,
            )
        };
        let reason = "a flush failed (EIO) with records no full flush covers";
        for restart in [None, Some("aterm link broker /r/bus.sock /r/bus.log")] {
            let exited = quoted(&crate::broker_health::poisoned_line(
                "/r/bus.log",
                reason,
                restart,
            ));
            assert!(
                exited.starts_with("the broker exited over a poisoned log")
                    && !exited.contains("--repair")
                    && !exited.contains("fix that first"),
                "{exited}"
            );
            if restart.is_some() {
                assert!(
                    exited.contains("the command its line names")
                        && !exited.contains("`aterm fabric on` starts"),
                    "{exited}"
                );
            } else {
                assert!(
                    exited.contains("`aterm fabric on` starts it again")
                        && !exited.contains("the command its line names"),
                    "{exited}"
                );
            }
        }
        assert!(
            quoted("aterm link broker: open /r/bus.log: address in use").contains("fix that first")
        );
        let down = |reason: &str| {
            fix_for(
                &format!(
                    "instance 1's bridge is attached but its broker link is down (fabric=stalled \
                     reason={reason}, last ack 3s ago)"
                ),
                &role,
            )
        };
        let poisoned = down("poisoned");
        assert!(
            poisoned.starts_with("the broker's log is poisoned")
                && !poisoned.contains("node.cap")
                && !poisoned.contains("`aterm fabric on`"),
            "{poisoned}"
        );
        assert!(
            down("attach").contains("node.cap"),
            "the refused cap keeps its fix"
        );
    }

    #[test]
    fn a_fix_picks_the_remedy_the_warning_names() {
        fn fix_for(warning: &str) -> String {
            super::fix_for(warning, &Role::default())
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
        assert_eq!(
            fix_for("@s-a (instance 0) is HELD (reason=x origin=local)"),
            "`aterm ctl --pid <pid> hold s-a off`"
        );
        let fleet = held("reason=stop origin=fleet");
        assert!(
            fleet.contains("aterm fabric halt") && !fleet.contains("hold s-a off"),
            "{fleet}"
        );
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
        let person = held("reason=taken-over origin=person");
        assert_eq!(person, PERSON_HOLD_LIFT);
        let beside = held("reason=taken-over origin=person,local");
        assert!(
            beside.starts_with(PERSON_HOLD_LIFT)
                && beside.contains("`aterm ctl --pid 1 hold s-a off`"),
            "{beside}"
        );
        let remote = held("reason=- origin=remote");
        assert!(
            remote.starts_with(REMOTE_HOLD_LIFT)
                && remote.ends_with("so does `aterm ctl --pid 1 hold s-a off`"),
            "{remote}"
        );
        let beside = held("reason=- origin=remote,fleet");
        assert!(
            beside.starts_with(REMOTE_HOLD_LIFT) && beside.contains("aterm fabric halt"),
            "{beside}"
        );
        let all = held("reason=taken-over origin=person,fleet,local");
        assert!(
            all.starts_with(PERSON_HOLD_LIFT)
                && all.contains("aterm fabric halt")
                && all.contains("hold s-a off"),
            "{all}"
        );

        let down = |reason: &str| {
            fix_for(&format!(
                "instance 1's bridge is attached but its broker link is down (fabric=stalled \
                 reason={reason}, last ack 3s ago)"
            ))
        };
        assert!(
            down("expired").contains("has lapsed") && down("expired") != down("attach"),
            "{}",
            down("expired")
        );
        assert!(
            fix_for(&format!("the broker at /x {}: x", fabric::CAP_LAPSED))
                .starts_with(&renew_remedy("`aterm fabric on`")),
            "{}",
            fix_for(&format!("the broker at /x {}: x", fabric::CAP_LAPSED))
        );
        let skew = "the broker at /x speaks astream protocol 3 and this aterm speaks 2";
        let here = protocol_remedy("`aterm fabric on`");
        assert!(down("protocol").starts_with(&here), "{}", down("protocol"));
        assert_eq!(fix_for(skew), here);
        for reason in ["busy", "record", "protocol"] {
            assert!(
                !down(reason).contains("node.cap aside") && down(reason) != down("other"),
                "{reason}: {}",
                down(reason)
            );
        }
        assert!(
            down("cut-say") != down("closed") && down("cut-inbox") == down("cut-say"),
            "{}",
            down("cut-say")
        );
        let unread = fix_for("the broker at /x answers but the bus could not be read: head query");
        assert!(
            unread.contains("move <root>/node.cap aside"),
            "a ring for this fleet that lacks a grant passes `on` as already: {unread}"
        );
    }

    #[test]
    fn rendezvous_defaults_fill_only_absent_flags() {
        let r = Rendezvous {
            fleet: "f".to_string(),
            broker: "/b".to_string(),
            node: "n-a".to_string(),
            cap_file: "/c".to_string(),
            state: "/s".to_string(),
            command: words("x"),
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
        assert_eq!(root_of("/r/bus.sock"), Some(PathBuf::from("/r")));
        assert_eq!(root_of("bus.sock"), None, "a relative socket");
    }

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

    /// The proof against a scripted instance: its `sessions` listing names `s-1`, then the proof's
    /// own connection gets `replies` in order. Answers the proof's result and that connection's
    /// requests.
    fn prove_against(name: &str, replies: &[&str]) -> (Result<u64, String>, Vec<String>) {
        use std::io::{BufRead, BufReader, Write};
        let dir = std::env::temp_dir().join(format!("atlk-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        let sock = dir.join("a.sock");
        std::fs::write(dir.join("a.sock.token"), "deadbeef\n").expect("token");
        let listener = std::os::unix::net::UnixListener::bind(&sock).expect("bind");
        let replies: Vec<String> = replies.iter().map(|r| (*r).to_string()).collect();
        let server = std::thread::spawn(move || {
            let (listing, _) = listener.accept().expect("accept the listing");
            let mut lines = BufReader::new(listing.try_clone().expect("clone"));
            for _ in 0..2 {
                lines
                    .read_line(&mut String::new())
                    .expect("AUTH, then sessions");
            }
            (&listing)
                .write_all(b"OK 1\n0 s-1 - alive zsh\n")
                .expect("list");
            let (conn, _) = listener.accept().expect("accept the proof");
            let mut lines = BufReader::new(conn.try_clone().expect("clone"));
            lines.read_line(&mut String::new()).expect("AUTH");
            let mut seen = Vec::new();
            for reply in replies {
                let mut line = String::new();
                if lines.read_line(&mut line).expect("a request") == 0 {
                    break;
                }
                seen.push(line.trim_end().to_string());
                (&conn).write_all(reply.as_bytes()).expect("reply");
            }
            seen
        });
        let inst = Instance {
            pid: std::process::id(),
            sock: sock.to_str().expect("utf8").to_string(),
            supervised: true,
            state: "connected".to_string(),
            reason: String::new(),
            command: None,
        };
        let proved = prove(&[inst], &mut Out::kept());
        let seen = server.join().expect("server");
        let _ = std::fs::remove_dir_all(&dir);
        (proved, seen)
    }

    #[test]
    fn the_proof_waits_past_another_senders_note_for_its_own() {
        let (proved, seen) = prove_against(
            "proof-past",
            &[
                "OK 0 hold=0\n",
                "OK 1 off=6\n",
                "OK inbox 1\n",
                "OK 1 hold=0\nmsg 1 off=5 t=1 from=s-9@n-b kind=note trust=agent\n",
                "OK inbox 2\n",
                "OK 1 hold=0\nmsg 2 off=6 t=2 from=s-1@n-a kind=note trust=agent\n",
            ],
        );
        assert!(proved.is_ok(), "{proved:?}: {seen:#?}");
        assert!(
            seen[4].starts_with("@s-1 await inbox since=1 kinds=note "),
            "the second wait starts past the other sender's note: {seen:#?}"
        );
    }

    #[test]
    fn a_proof_wait_that_times_out_says_the_note_did_not_come_back() {
        let (proved, seen) = prove_against(
            "proof-timeout",
            &["OK 0 hold=0\n", "OK 1 off=6\n", "OK timeout\n"],
        );
        let why = proved.expect_err("nothing came back");
        assert!(
            why.starts_with("landed at @6 but was not delivered back within 60s: ")
                && why.contains("OK timeout"),
            "{why}: {seen:#?}"
        );
    }

    #[test]
    fn an_explicit_broker_gets_no_rendezvous_cap_or_state() {
        for broker in ["/r/bus.sock", "127.0.0.1:7000"] {
            let r = Rendezvous {
                fleet: "f".to_string(),
                broker: broker.to_string(),
                node: "n-a".to_string(),
                cap_file: "/r/node.cap".to_string(),
                state: "/r/link-state".to_string(),
                command: words("x"),
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

    #[test]
    fn a_cap_minted_under_the_roots_secret_is_recognised() {
        let dir = std::env::temp_dir().join(format!("atfo-minted-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        let node = "n-0123456789abcdef";
        let ring = node_grants("lab", node);
        let cap_under = |key: &[u8], name: &str, grants: &[String]| {
            let lines: String = grants
                .iter()
                .map(|g| {
                    let c = astream_cap::mint(key, g).expect("mint");
                    astream_cap::capfile::format_line(&c.filter, &c.tag) + "\n"
                })
                .collect();
            let path = dir.join(name);
            std::fs::write(&path, lines).expect("cap");
            path
        };
        let own = cap_under(&[4u8; 32], "own.cap", &ring);
        assert_eq!(cap_is_for(&own, "lab", node, &[4u8; 32]), Ok(8));
        assert_eq!(
            cap_is_for(&own, "lab", "n-other", &[4u8; 32]),
            Err(CapMiss {
                why: "minted for a different node id, not n-other".to_string(),
                keep: true
            }),
            "another node's ring, the last record of its id once the state dir names another"
        );
        let foreign = cap_under(&[5u8; 32], "foreign.cap", &ring);
        assert!(cap_is_for(&foreign, "lab", node, &[4u8; 32]).is_err());
        assert_eq!(
            cap_is_for(&foreign, "lab", "n-other", &[4u8; 32]),
            Err(CapMiss::from(
                "not minted under the current mint secret".to_string()
            )),
            "another secret's ring is judged by its secret first"
        );
        assert!(cap_is_for(&dir.join("absent.cap"), "lab", node, &[4u8; 32]).is_err());
        assert_eq!(
            cap_is_for(&own, "work", node, &[4u8; 32]),
            Err(CapMiss {
                why: "minted for fleet lab, not work".to_string(),
                keep: false
            }),
            "another fleet's ring, which `on --fleet lab` mints again grant for grant"
        );
        let by_hand = cap_under(&[4u8; 32], "hand.cap", &ring[..7]);
        assert_eq!(cap_is_for(&by_hand, "lab", node, &[4u8; 32]), Ok(7));
        assert!(
            cap_is_for(&by_hand, "work", node, &[4u8; 32]).is_err_and(|m| m.keep),
            "a ring no `on` mints is kept aside"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_node_id_is_n_dash_and_a_principal() {
        assert!(is_node_id("n-0123456789abcdef"));
        assert!(is_node_id("n-m7"));
        for bad in [
            "new", "n-", "s-0123", "h-andrew", "n-UPPER", "n-a b", "n-a/b", "",
        ] {
            assert!(!is_node_id(bad), "{bad}");
        }
    }

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
                script = aterm_codec::xml::escape(script),
            )
        };
        let install = |text: &str| std::fs::write(p.plist(), text).expect("plist");

        let legacy = plist(&format!(
            "rm -f '{r}/bus.sock'; exec '/u/aterm' link broker '{r}/bus.sock' '{r}/bus.log'"
        ));
        install(&legacy);
        assert_eq!(Role::of(&p), Ok(Role { fleet: None }));
        assert!(!p.plist_is_current(&legacy), "`on` still rewrites it");

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
