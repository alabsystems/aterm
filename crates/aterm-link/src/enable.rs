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

const PROOF_DEADLINE: Duration = Duration::from_secs(5);

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
            xml_escape(&self.label())
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
             # ls` and `aterm fabric` read it when their flags are omitted;\n\
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

/// Prepend absent `--fleet` and `--broker` from `r`.
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
    create_temp_beside, write as write_atomic, ConfigFile, LOCK_OPEN_FLAGS,
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

/// The step lines, printed as they happen.
pub(crate) struct Out {
    pub(crate) dry_run: bool,
    pub(crate) changed: usize,
    pub(crate) failed: bool,
}

impl Out {
    pub(crate) const fn new(dry_run: bool) -> Self {
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
    out.done("root", &would.join("; "), &did.join("; "));
    if !out.dry_run {
        for d in [p.root.as_path(), state] {
            if let Err(e) = ensure_private_dir(d) {
                failed(out, d, &e);
                return false;
            }
        }
    }
    true
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

fn record_node(state: &Path, node: &str, out: &mut Out) -> bool {
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
pub(crate) fn probe(
    p: &Paths,
    node: &str,
    procs: impl FnOnce() -> Vec<fabric::Proc>,
) -> fabric::BrokerView {
    match fabric::bridge_config(&p.bridge_command(node)) {
        Ok(cfg) => fabric::probe_broker(&cfg, &procs()).0,
        Err(e) => fabric::BrokerView {
            endpoint: p.sock(),
            error: Some(e),
            ..fabric::BrokerView::default()
        },
    }
}

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
    /// The domain holds no such job.
    Absent,
    /// There is no `gui/<uid>` domain.
    NoGuiLogin,
    /// Loaded; `Some(pid)` while it runs.
    Loaded(Option<u32>),
}

const LAUNCHCTL_NO_DOMAIN: i32 = 112;

/// Query `launchctl print gui/<uid>/<label>`.
pub(crate) fn launchd_job(label: &str) -> LaunchdJob {
    let Ok(out) = aterm_uds::spawnfd::command(launchctl_program())
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
            let view = probe(p, node, fabric::process_table);
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
            let text = p.plist_text();
            let same = std::fs::read_to_string(&plist).is_ok_and(|old| p.plist_is_current(&old));
            let running = match launchd_job(&label) {
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

/// `aterm fabric on`.
#[must_use]
pub fn on(opts: &OnOpts) -> ExitCode {
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
    let kept_from = match (opts.durability, &durability_job) {
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
        return ExitCode::from(2);
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
        return ExitCode::from(2);
    }
    match binary_has_link(&p) {
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
            return ExitCode::from(2);
        }
    }

    if !root_step(&p, &state, &mut out) || !program_step(&p, &mut out) {
        return ExitCode::from(2);
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
                if let Err(e) = random_bytes(SECRET_LEN)
                    .and_then(|b| aterm_uds::private_file::write_private(&p.root, MINT_SECRET, &b))
                {
                    out.fail("secret", &format!("{}: {e}", secret.display()));
                    return ExitCode::from(2);
                }
            }
        }
    }

    let cap = p.cap();
    let cap_ok = if cap.exists() {
        read_secret(&secret_shown)
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

    let broker_ok = broker_step(&p, &node, service, opts.durability.is_some(), &mut out);
    if !broker_ok && !opts.dry_run {
        return ExitCode::FAILURE;
    }
    let argv = p.bridge_command(&node);
    finish(&p, &node, &argv, &mut out, opts.dry_run)
}

fn finish(p: &Paths, node: &str, argv: &[String], out: &mut Out, dry_run: bool) -> ExitCode {
    let node = node.to_string();
    let state = p.state();
    let sock = p.sock();
    let cap = p.cap();
    if !config_step(p.config.as_deref(), argv, out) {
        return ExitCode::FAILURE;
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
                "the broker is up and ended one subscription (the face after `cut-`), as astream \
                 does to a reader that leaves its socket unread for the 30 s stream write bound: \
                 a bridge stopped or starved that long, or a link that lost one connection — \
                 nothing on the broker needs a restart"
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
             re-mints the cap for the node id in the state dir and probes again; check `--fleet`"
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
            remedies.push(REMOTE_HOLD_LIFT.to_string());
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
    } else if s("lost") && s("evicted") {
        "read mail sooner (`aterm ctl @<sid> await inbox`) and `inbox seen` what is handled; the \
         evicted records are still on the bus"
            .to_string()
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
    } else if s("distinct subjects one node may ever mint") {
        format!(
            "move <root>/link-state/node aside and run {again}: it provisions a new node id, \
             re-mints the cap for it and writes the new [fabric] command; then quit and reopen \
             each aterm instance (`on` names every one still armed with the old command), whose \
             bridge starts on the new id with a fresh budget (a killed bridge would come back on \
             the command its aterm armed at launch); mail still queued for the old id is \
             abandoned with it"
        )
    } else if s("presence roster has more than") && s("truncated") {
        "nothing on the bus is wrong: one roster read takes at most that many rows and every \
         session ever hosted keeps one; a session past the cut is still listed by `aterm ctl \
         ls`"
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
     hands it back; no command here does";

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

    fn words(line: &str) -> Vec<String> {
        aterm_types::fabric_command::split_line(line)
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
        let ps = fabric::Proc {
            pid: 7,
            ppid: 1,
            cmd: argv.join(" "),
        };
        assert_eq!(fabric::broker_pid(&[ps], &sock), Some(7));
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
        assert_eq!(g.len(), 8);
        assert_eq!(g.iter().filter(|x| x.contains("p=n-a:")).count(), 4);
        assert_eq!(g.iter().filter(|x| x.starts_with("ro:")).count(), 4);
        assert_eq!(g.iter().filter(|x| x.contains("n-a")).count(), 6);
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
            "@s-a (instance 1) has 1 unhandled task/ask older than 10 min",
            "@s-a (instance 1) has 3 queued post(s) and no working bridge",
            "instance 1 did not answer (x)",
            "cannot list this machine's aterm instances: x",
            "the bus advertises @s-g live on n-a",
            "h-andrew has a fleet halt standing (reason=stop)",
            "no rendezvous file at /x",
            "instance 1's bridge is another build than the instance: bridge a, instance b",
            "instance 1 could not parse 3 of its bridge's lines (lane_faults=3)",
            "node n-a holds 3072 of the 4096 distinct subjects one node may ever mint, and none but \
             a closed session's own `ev` face (while the session-log mirror runs) is ever given \
             back: past the bound a new session gets no presence row and its posts are refused; \
             the remedy is a new node id",
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
        assert_eq!(remote, REMOTE_HOLD_LIFT);
        assert!(!remote.contains("hold s-a off"), "{remote}");
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
    }

    #[test]
    fn the_installed_job_reads_back_as_the_broker_argv() {
        let p = Paths {
            home: PathBuf::from("/Users//u"),
            login_home: Some(PathBuf::from("/Users//u")),
            root: PathBuf::from("/tmp/r"),
            aterm: "/usr/local/bin/aterm".to_string(),
            fleet: "local".to_string(),
            config: None,
            rendezvous: None,
            durability: None,
        };
        let plist = p.plist_text();
        assert!(p.plist_is_current(&plist), "{plist}");
        assert_eq!(
            plist_program_arguments(&collapse_between_tags(&plist)),
            Some(p.broker_argv())
        );
        let unit = p.unit_text();
        assert!(!unit.contains("rm -f"), "{unit}");
        assert_eq!(unit_argv(&unit), Some(p.broker_argv()));
    }

    #[test]
    fn the_rendezvous_file_round_trips_and_a_command_names_its_root() {
        let r = Rendezvous {
            fleet: "f".to_string(),
            broker: "/r/bus.sock".to_string(),
            node: "n-a".to_string(),
            cap_file: "/r/node.cap".to_string(),
            state: "/r/link-state".to_string(),
            command: words("x"),
        };
        let text = r.render();
        assert_eq!(Rendezvous::parse(&text), Ok(r.clone()));
        assert!(Rendezvous::parse(&text.replace("node = \"n-a\"", "node = 1")).is_err());
        assert_eq!(root_of("/r/bus.sock"), Some(PathBuf::from("/r")));
        assert_eq!(root_of("bus.sock"), None, "a relative socket");
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
        let cap_under = |key: &[u8], name: &str| {
            let lines: String = node_grants("lab", "n-0123456789abcdef")
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
        let node = "n-0123456789abcdef";
        let own = cap_under(&[4u8; 32], "own.cap");
        assert_eq!(cap_is_for(&own, node, &[4u8; 32]), Ok(8));
        assert!(
            cap_is_for(&own, "n-other", &[4u8; 32]).is_err(),
            "another node"
        );
        assert!(cap_is_for(&cap_under(&[5u8; 32], "foreign.cap"), node, &[4u8; 32]).is_err());
        assert!(cap_is_for(&dir.join("absent.cap"), node, &[4u8; 32]).is_err());
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
                script = xml_escape(script),
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
