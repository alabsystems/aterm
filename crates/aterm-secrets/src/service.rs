// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The launchd (macOS) and systemd user (Linux) jobs `aterm secrets` installs: the subscriber
//! and the server.

use crate::Error;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Which job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Job {
    /// `aterm secrets watch`.
    Subscriber,
    /// `aterm secrets serve …`.
    Server,
}

impl Job {
    /// The launchd label / systemd unit stem.
    pub const fn label(self) -> &'static str {
        match self {
            Job::Subscriber => "systems.alab.aterm-secrets",
            Job::Server => "systems.alab.aterm-secrets-server",
        }
    }

    /// One line naming the job: a systemd unit's `Description`.
    pub const fn description(self) -> &'static str {
        match self {
            Job::Subscriber => "aterm secrets subscriber (aterm secrets watch)",
            Job::Server => "aterm secrets server (aterm secrets serve)",
        }
    }
}

/// A job's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// Not installed.
    Absent,
    /// Installed; running with this pid, or not running.
    Installed {
        /// The pid, when running.
        pid: Option<u32>,
        /// The unit file.
        unit: PathBuf,
    },
}

/// The launchctl binary.
pub const LAUNCHCTL: &str = aterm_uds::os_path!("launchctl");

/// Where `systemctl` may be, tried in order: never a `PATH` lookup.
pub const SYSTEMCTL_PATHS: [&str; 3] = [
    "/usr/bin/systemctl",
    "/bin/systemctl",
    "/run/current-system/sw/bin/systemctl",
];

/// How long one `launchctl` or `systemctl` call may take before it is called hung.
const CHILD_HANG: Duration = Duration::from_secs(30);

/// Timeout waiting for launchd to forget a booted-out job ([`install`]).
#[cfg(any(target_os = "macos", all(test, unix)))]
const BOOTOUT_SETTLE: Duration = Duration::from_secs(30);

/// The job's launchd property list: `~/Library/LaunchAgents/<label>.plist`.
pub fn plist_path(home: &Path, job: Job) -> PathBuf {
    home.join("Library")
        .join("LaunchAgents")
        .join(format!("{}.plist", job.label()))
}

/// The job's systemd user unit: `~/.config/systemd/user/<label>.service`.
pub fn unit_path(home: &Path, job: Job) -> PathBuf {
    home.join(".config")
        .join("systemd")
        .join("user")
        .join(unit_name(job))
}

/// The job's log (launchd's stdout and stderr): `~/.config/aterm/secrets/<label>.log`.
pub fn log_path(home: &Path, job: Job) -> PathBuf {
    secrets_dir(home).join(format!("{}.log", job.label()))
}

/// The record that the person turned this login's subscriber off:
/// `~/.config/aterm/secrets/subscriber-off`. `agent off` writes it, and so does a `join` or
/// `create` with `--no-agent` where no subscriber runs; an [`install`] of the subscriber
/// removes it. aterm's setup (System State, `aterm init`) reads it as the person's choice: Sync
/// shows off, and the files stay as last synced with no subscriber installed.
pub fn subscriber_off_path(home: &Path) -> PathBuf {
    secrets_dir(home).join("subscriber-off")
}

/// Whether the person turned this login's subscriber off ([`subscriber_off_path`]). A record
/// that cannot be read is no record.
pub fn subscriber_turned_off(home: &Path) -> bool {
    crate::fsx::kind(&subscriber_off_path(home)).is_ok_and(|kind| kind != crate::fsx::Kind::Missing)
}

/// Record that the person turned this login's subscriber off ([`subscriber_off_path`]), in
/// the 0700 `~/.config/aterm/secrets`.
pub fn record_subscriber_off(home: &Path) -> crate::Result<()> {
    let _decision = SubscriberDecision::take(home)?;
    record_subscriber_off_locked(home)
}

fn record_subscriber_off_locked(home: &Path) -> crate::Result<()> {
    crate::fsx::ensure_private_dir(&secrets_dir(home))?;
    crate::fsx::write_atomic(
        &subscriber_off_path(home),
        b"the subscriber was turned off by choice; aterm secrets agent on turns it back on\n",
        0o600,
    )
}

/// Orders the subscriber's local on/off decision and service changes across
/// processes. It is separate from the vault's data lock and never spans a fetch.
struct SubscriberDecision(std::fs::File);

impl SubscriberDecision {
    fn take(home: &Path) -> crate::Result<Self> {
        use std::os::unix::fs::OpenOptionsExt as _;
        let dir = secrets_dir(home);
        crate::fsx::ensure_private_dir(&dir)?;
        let path = dir.join("subscriber.lock");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)?;
        if !file.metadata()?.is_file() {
            return Err(Error::new(format!(
                "{}: not a regular file",
                path.display()
            )));
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "the take of SubscriberDecision, whose Drop releases the local service decision lock"
        )]
        file.lock()?;
        Ok(Self(file))
    }
}

impl Drop for SubscriberDecision {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// launchd's restart window: a `KeepAlive` job that ended is started again within
/// its throttle interval (10 s by default), so a subscriber read as installed but
/// not running may only be between two runs until it has stayed down past this.
pub const RESTART_WINDOW: Duration = Duration::from_secs(11);

/// How often a job read as not running is read again within [`RESTART_WINDOW`].
pub const RESTART_POLL: Duration = Duration::from_millis(500);

/// Whether a job read as not running runs again within launchd's restart window:
/// `running` is read every [`RESTART_POLL`] until it says so, or until
/// [`RESTART_WINDOW`] has passed since `down_since`, when the job was first seen
/// down (`now`, when unset). `down_since` keeps that moment for a later read, so a
/// job already down past the window is not waited on again.
/// # Errors
/// A read of `running` failed.
pub fn back_within_restart_window<E>(
    down_since: &mut Option<Instant>,
    now: &mut dyn FnMut() -> Instant,
    running: &mut dyn FnMut() -> Result<bool, E>,
    pause: &mut dyn FnMut(Duration),
) -> Result<bool, E> {
    let until = *down_since.get_or_insert_with(&mut *now) + RESTART_WINDOW;
    loop {
        let at = now();
        if at >= until {
            return Ok(false);
        }
        pause(RESTART_POLL.min(until - at));
        if running()? {
            return Ok(true);
        }
    }
}

/// Remove the person's record that the subscriber is off: it is on again.
fn clear_subscriber_off(home: &Path) -> crate::Result<()> {
    let path = subscriber_off_path(home);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::new(format!("{}: {e}", path.display()))),
    }
}

/// `~/.config/aterm/secrets`.
fn secrets_dir(home: &Path) -> PathBuf {
    home.join(".config").join("aterm").join("secrets")
}

/// `<label>.service`.
fn unit_name(job: Job) -> String {
    format!("{}.service", job.label())
}

/// The home a log at [`log_path`] lies under: `<home>` of `<home>/.config/aterm/secrets/<file>`.
fn home_of_log(log: &Path) -> Option<&Path> {
    let dir = log.parent()?;
    if !dir.ends_with(Path::new(".config").join("aterm").join("secrets")) {
        return None;
    }
    let home = dir.parent()?.parent()?.parent()?;
    home.is_absolute().then_some(home)
}

/// The launchd property list for a job, setting `HOME` when `log` is a [`log_path`].
pub fn plist_text(label: &str, program: &Path, args: &[String], log: &Path) -> String {
    let string = |s: &str| format!("<string>{}</string>", aterm_codec::xml::escape(s));
    let mut argv = String::new();
    for arg in
        std::iter::once(program.to_string_lossy().as_ref()).chain(args.iter().map(String::as_str))
    {
        argv.push_str(&format!("\t\t{}\n", string(arg)));
    }
    let environment = home_of_log(log).map_or_else(String::new, |home| {
        format!(
            "\t<key>EnvironmentVariables</key>\n\
             \t<dict>\n\
             \t\t<key>HOME</key>\n\
             \t\t{}\n\
             \t</dict>\n",
            string(&home.to_string_lossy())
        )
    });
    let log = string(&log.to_string_lossy());
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \t<key>Label</key>\n\
         \t{label}\n\
         \t<key>ProgramArguments</key>\n\
         \t<array>\n\
         {argv}\
         \t</array>\n\
         \t<key>RunAtLoad</key>\n\
         \t<true/>\n\
         \t<key>KeepAlive</key>\n\
         \t<true/>\n\
         \t<key>ProcessType</key>\n\
         \t<string>Background</string>\n\
         \t<key>Nice</key>\n\
         \t<integer>5</integer>\n\
         \t<key>SoftResourceLimits</key>\n\
         \t<dict>\n\
         \t\t<key>NumberOfFiles</key>\n\
         \t\t<integer>4096</integer>\n\
         \t</dict>\n\
         {environment}\
         \t<key>StandardOutPath</key>\n\
         \t{log}\n\
         \t<key>StandardErrorPath</key>\n\
         \t{log}\n\
         </dict>\n\
         </plist>\n",
        label = string(label),
    )
}

/// One argv element as systemd's `ExecStart` reads it back exactly.
pub fn systemd_word(word: &str) -> String {
    let mut out = String::with_capacity(word.len() + 2);
    out.push('"');
    for c in word.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '%' => out.push_str("%%"),
            '$' => out.push_str("$$"),
            c if c.is_control() && u32::from(c) < 0x80 => {
                out.push_str(&format!("\\x{:02x}", u32::from(c)));
            }
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A unit's free-text value (`Description`): one line, `%` doubled, no trailing `\`.
fn systemd_text(text: &str) -> String {
    let line: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .replace('%', "%%");
    line.trim_end_matches('\\').to_string()
}

/// Render the systemd user unit for a job ([`systemd_word`]).
pub fn systemd_unit(description: &str, program: &Path, args: &[String]) -> String {
    let mut exec = systemd_word(&program.to_string_lossy());
    for arg in args {
        exec.push(' ');
        exec.push_str(&systemd_word(arg));
    }
    format!(
        "[Unit]\n\
         Description={description}\n\
         \n\
         [Service]\n\
         ExecStart={exec}\n\
         Restart=always\n\
         RestartSec=5\n\
         LimitNOFILE=4096\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        description = systemd_text(description),
    )
}

/// The program `unit`, a job's plist on macOS or its systemd unit elsewhere, starts: the first
/// `ProgramArguments` string or `ExecStart` word, as [`plist_text`] and [`systemd_unit`] write
/// them. `None` for a file it cannot read or a form they never write.
pub fn unit_program(unit: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(unit).ok()?;
    if cfg!(target_os = "macos") {
        plist_program(&text)
    } else {
        exec_program(&text)
    }
}

/// [`unit_program`] of a plist's text.
pub fn plist_program(text: &str) -> Option<PathBuf> {
    let (_, after) = text.split_once("<key>ProgramArguments</key>")?;
    let after = after.trim_start().strip_prefix("<array>")?;
    let (escaped, _) = after
        .trim_start()
        .strip_prefix("<string>")?
        .split_once("</string>")?;
    let mut out = String::with_capacity(escaped.len());
    let mut rest = escaped;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let (entity, c) = [
            ("&amp;", '&'),
            ("&lt;", '<'),
            ("&gt;", '>'),
            ("&quot;", '"'),
            ("&apos;", '\''),
        ]
        .into_iter()
        .find(|(entity, _)| rest.starts_with(entity))?;
        out.push(c);
        rest = &rest[entity.len()..];
    }
    out.push_str(rest);
    Some(PathBuf::from(out))
}

/// Every `<string>` of a plist's `ProgramArguments` array, its XML entities read back: the
/// program [`plist_program`] names and its arguments, as [`plist_text`] writes them. `None`
/// for a form it never writes.
pub fn plist_argv(text: &str) -> Option<Vec<String>> {
    let (_, after) = text.split_once("<key>ProgramArguments</key>")?;
    let mut rest = after.trim_start().strip_prefix("<array>")?;
    let mut argv = Vec::new();
    loop {
        rest = rest.trim_start();
        if rest.starts_with("</array>") {
            return (!argv.is_empty()).then_some(argv);
        }
        let (escaped, tail) = rest.strip_prefix("<string>")?.split_once("</string>")?;
        let mut word = String::with_capacity(escaped.len());
        let mut left = escaped;
        while let Some(at) = left.find('&') {
            word.push_str(&left[..at]);
            left = &left[at..];
            let (entity, c) = [
                ("&amp;", '&'),
                ("&lt;", '<'),
                ("&gt;", '>'),
                ("&quot;", '"'),
                ("&apos;", '\''),
            ]
            .into_iter()
            .find(|(entity, _)| left.starts_with(entity))?;
            word.push(c);
            left = &left[entity.len()..];
        }
        word.push_str(left);
        argv.push(word);
        rest = tail;
    }
}

/// Every word of a systemd unit's `ExecStart`, as [`systemd_unit`] writes them (each quoted by
/// [`systemd_word`], one space apart): the program [`exec_program`] names and its arguments.
/// `None` for a form it never writes.
pub fn exec_argv(text: &str) -> Option<Vec<String>> {
    let line = text
        .lines()
        .find_map(|line| line.strip_prefix("ExecStart="))?;
    let mut chars = line.chars();
    let mut argv = Vec::new();
    loop {
        if chars.next()? != '"' {
            return None;
        }
        let mut word = String::new();
        loop {
            match chars.next()? {
                '"' => break,
                '\\' => match chars.next()? {
                    c @ ('\\' | '"') => word.push(c),
                    _ => return None,
                },
                c @ ('%' | '$') => {
                    if chars.next()? != c {
                        return None;
                    }
                    word.push(c);
                }
                c => word.push(c),
            }
        }
        argv.push(word);
        match chars.next() {
            None => return Some(argv),
            Some(' ') => {}
            Some(_) => return None,
        }
    }
}

/// [`unit_program`] of a systemd unit's text: its `ExecStart`'s first word, which
/// [`systemd_word`] quoted (a program holds no control character, [`check_argv`]).
pub fn exec_program(text: &str) -> Option<PathBuf> {
    let line = text
        .lines()
        .find_map(|line| line.strip_prefix("ExecStart="))?;
    let mut chars = line.strip_prefix('"')?.chars();
    let mut out = String::new();
    loop {
        match chars.next()? {
            '"' => return Some(PathBuf::from(out)),
            '\\' => match chars.next()? {
                c @ ('\\' | '"') => out.push(c),
                _ => return None,
            },
            c @ ('%' | '$') => {
                if chars.next()? != c {
                    return None;
                }
                out.push(c);
            }
            c => out.push(c),
        }
    }
}

/// What a `launchctl` call does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Launchctl<'a> {
    /// `print gui/<uid>/<label>`: read the job's state.
    Print(&'a str),
    /// `bootout gui/<uid>/<label>`: stop the job and unload it.
    Bootout(&'a str),
    /// `bootstrap gui/<uid> <plist>`: load (and with `RunAtLoad`, start) the job a plist defines.
    Bootstrap(&'a Path),
    /// `kickstart gui/<uid>/<label>`: start the job unless it runs.
    Kickstart(&'a str),
    /// `kickstart -k gui/<uid>/<label>`: start the job, killing a running instance first.
    Restart(&'a str),
}

/// The argv (after `/bin/launchctl`) of one call in this user's GUI domain, `gui/<uid>`.
pub fn launchctl_argv(uid: u32, call: Launchctl<'_>) -> Vec<String> {
    let domain = format!("gui/{uid}");
    match call {
        Launchctl::Print(label) => vec!["print".into(), format!("{domain}/{label}")],
        Launchctl::Bootout(label) => vec!["bootout".into(), format!("{domain}/{label}")],
        Launchctl::Bootstrap(plist) => vec![
            "bootstrap".into(),
            domain,
            plist.to_string_lossy().into_owned(),
        ],
        Launchctl::Kickstart(label) => vec!["kickstart".into(), format!("{domain}/{label}")],
        Launchctl::Restart(label) => {
            vec!["kickstart".into(), "-k".into(), format!("{domain}/{label}")]
        }
    }
}

/// The argv (after `systemctl`) of one call to this user's manager: `--user` and `words`.
pub fn systemctl_argv(words: &[&str]) -> Vec<String> {
    std::iter::once("--user")
        .chain(words.iter().copied())
        .map(str::to_string)
        .collect()
}

/// The running pid `launchctl print`'s text names: the service's own `pid = <n>` line.
pub fn print_pid(text: &str) -> Option<u32> {
    text.lines().find_map(|line| {
        let top = line.strip_prefix('\t')?;
        top.strip_prefix("pid = ")?
            .trim_end()
            .parse()
            .ok()
            .filter(|pid| *pid > 0)
    })
}

/// The pid `systemctl --user show -p MainPID --value` printed.
pub fn main_pid(text: &str) -> Option<u32> {
    let text = text.trim();
    let text = text.strip_prefix("MainPID=").unwrap_or(text);
    text.parse().ok().filter(|pid| *pid > 0)
}

/// The first of `candidates` that exists: `systemctl`, or a clear refusal when this Linux has none.
pub fn systemctl_path(
    candidates: &[&str],
    exists: &dyn Fn(&Path) -> bool,
) -> crate::Result<PathBuf> {
    candidates
        .iter()
        .map(Path::new)
        .find(|path| exists(path))
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            Error::new(format!(
                "no systemctl at {}: this system has no systemd user manager to keep the job running; \
                 run `aterm secrets watch` (or `serve`) under your own supervisor instead",
                candidates.join(" or ")
            ))
        })
}

/// Whether `path` is in a cargo build directory.
pub fn in_build_dir(path: &Path, exists: &dyn Fn(&Path) -> bool) -> bool {
    let names: Vec<&std::ffi::OsStr> = path.iter().collect();
    let mut dir = PathBuf::new();
    for (index, name) in names.iter().enumerate().take(names.len().saturating_sub(1)) {
        let text = name.to_string_lossy();
        let after = |offset: usize| {
            names
                .get(index + offset)
                .map(|n| n.to_string_lossy().into_owned())
        };
        if text == "target" {
            if exists(&dir.join("Cargo.toml")) {
                return true;
            }
            let profile = |n: Option<String>| n.is_some_and(|n| n == "debug" || n == "release");
            if profile(after(1)) || (index + 2 < names.len() && profile(after(2))) {
                return true;
            }
        }
        if text.ends_with(".noindex")
            && (text.contains("target") || after(1).is_some_and(|n| n == "target"))
        {
            return true;
        }
        dir.push(name);
        if exists(&dir.join("CACHEDIR.TAG")) {
            return true;
        }
    }
    false
}

/// Whether `path` is a copy that goes away.
pub fn transient(path: &Path) -> bool {
    let text = path.to_string_lossy();
    let temp = [
        std::env::temp_dir(),
        PathBuf::from("/tmp"),
        PathBuf::from("/var/tmp"),
    ];
    text.starts_with("/Volumes/")
        || text.contains("/AppTranslocation/")
        || temp
            .iter()
            .flat_map(|root| [root.canonicalize().ok(), Some(root.clone())])
            .flatten()
            .any(|root| root.parent().is_some() && path.starts_with(&root))
}

/// Executable a job should run for `exe` ([`in_build_dir`], [`transient`]).
pub fn service_program(exe: &Path, exists: &dyn Fn(&Path) -> bool) -> crate::Result<PathBuf> {
    if !exe.is_absolute() {
        return Err(Error::new(format!(
            "{}: not an absolute path; pass --program <absolute path to aterm>",
            exe.display()
        )));
    }
    if in_build_dir(exe, exists) {
        return Err(Error::new(format!(
            "{} is a build in a cargo target directory, which aterm frees when nobody uses it, \
             so a job running it would stop starting; install aterm, or pass --program <the \
             aterm a lasting job should run>",
            exe.display()
        )));
    }
    if transient(exe) {
        return Err(Error::new(format!(
            "{} is on a disk image, an App Translocation copy or in a temporary directory, which \
             goes away, so a job running it would stop starting; run aterm from where it is \
             installed (move aterm.app to /Applications), or pass --program <the aterm a \
             lasting job should run>",
            exe.display()
        )));
    }
    let mut bundle = PathBuf::new();
    let mut found = None;
    let names: Vec<&std::ffi::OsStr> = exe.iter().collect();
    for (index, name) in names.iter().enumerate() {
        bundle.push(name);
        let is_app = Path::new(name).extension().is_some_and(|e| e == "app");
        let has_contents = names.get(index + 1).is_some_and(|n| *n == "Contents");
        if is_app && has_contents && index + 2 < names.len() {
            found = Some(bundle.clone());
            break;
        }
    }
    let Some(bundle) = found else {
        return Ok(exe.to_path_buf());
    };
    let macos = bundle.join("Contents").join("MacOS");
    if exe.parent() == Some(macos.as_path()) {
        return Ok(exe.to_path_buf());
    }
    let own_name = exe.file_name().map(|name| macos.join(name));
    for candidate in own_name
        .into_iter()
        .chain(std::iter::once(macos.join("aterm")))
    {
        if exists(&candidate) {
            return Ok(candidate);
        }
    }
    Err(Error::new(format!(
        "{} lies inside {} but that bundle has no executable at {}; pass --program <the aterm a \
         lasting job should run>",
        exe.display(),
        bundle.display(),
        macos.join("aterm").display()
    )))
}

/// Executable a job installed now should run ([`service_program`]).
pub fn program_for_service() -> crate::Result<PathBuf> {
    let exe = std::env::current_exe()
        .map_err(|e| Error::new(format!("cannot name this executable: {e}; pass --program")))?;
    let exe = exe
        .canonicalize()
        .map_err(|e| Error::new(format!("{}: {e}; pass --program", exe.display())))?;
    service_program(&exe, &|path| std::fs::symlink_metadata(path).is_ok())
}

/// The file that defines `job` under `home`: its launchd plist on macOS ([`plist_path`]), its
/// systemd user unit elsewhere ([`unit_path`]).
pub fn unit_file(home: &Path, job: Job) -> PathBuf {
    if cfg!(target_os = "macos") {
        plist_path(home, job)
    } else {
        unit_path(home, job)
    }
}

/// The program `job`'s unit file starts, when that is an executable file in a lasting place
/// (one [`service_program`] names as itself): what a repair keeps ([`repair_subscriber`]), so a
/// repair never moves a job a person pointed at another aterm (`agent on --program`, a held
/// snapshot) onto the aterm that repairs it; only an explicit [`install`] does. `None` where
/// the unit names no such program (none, one gone, a build, a copy that goes away).
pub fn kept_program(home: &Path, job: Job) -> Option<PathBuf> {
    kept(
        unit_program(&unit_file(home, job)),
        &|path| std::fs::symlink_metadata(path).is_ok(),
        &|path| {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::metadata(path)
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        },
    )
}

/// [`kept_program`] of the program a unit file names, `held`.
fn kept(
    held: Option<PathBuf>,
    exists: &dyn Fn(&Path) -> bool,
    runnable: &dyn Fn(&Path) -> bool,
) -> Option<PathBuf> {
    held.filter(|held| {
        runnable(held) && service_program(held, exists).is_ok_and(|named| named == *held)
    })
}

/// Refuse what a unit file cannot name faithfully.
fn check_argv(program: &Path, args: &[String]) -> crate::Result<()> {
    let Some(text) = program.to_str() else {
        return Err(Error::new(format!(
            "{}: not a UTF-8 path",
            program.display()
        )));
    };
    if !program.is_absolute() {
        return Err(Error::new(format!(
            "{text}: the program must be an absolute path"
        )));
    }
    if std::iter::once(text)
        .chain(args.iter().map(String::as_str))
        .any(|word| word.chars().any(char::is_control))
    {
        return Err(Error::new(
            "a job's program and arguments cannot hold control characters",
        ));
    }
    match std::fs::metadata(program) {
        Ok(meta) if meta.is_file() => Ok(()),
        Ok(_) => Err(Error::new(format!("{text}: not a file"))),
        Err(e) => Err(Error::new(format!("{text}: {e}"))),
    }
}

/// Write `text` to the unit file at `path` (0644, atomically), making its directory.
fn write_unit_file(path: &Path, text: &str) -> crate::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| Error::new(format!("{}: {e}", dir.display())))?;
    }
    crate::fsx::write_atomic(path, text.as_bytes(), 0o644)
}

/// Write the job's launchd plist: what [`install`] writes on macOS before it loads the job.
pub fn write_plist(
    home: &Path,
    job: Job,
    program: &Path,
    args: &[String],
) -> crate::Result<PathBuf> {
    check_argv(program, args)?;
    crate::fsx::ensure_private_dir(&secrets_dir(home))?;
    let plist = plist_path(home, job);
    let log = log_path(home, job);
    write_unit_file(&plist, &plist_text(job.label(), program, args, &log))?;
    Ok(plist)
}

/// Write the job's systemd user unit: what [`install`] does on Linux before any `systemctl` call.
pub fn write_unit(
    home: &Path,
    job: Job,
    program: &Path,
    args: &[String],
) -> crate::Result<PathBuf> {
    check_argv(program, args)?;
    let unit = unit_path(home, job);
    write_unit_file(&unit, &systemd_unit(job.description(), program, args))?;
    Ok(unit)
}

/// One child run under [`CHILD_HANG`], its stdin `/dev/null`.
fn run(program: &Path, argv: &[String]) -> crate::Result<std::process::Output> {
    let mut cmd = aterm_uds::spawnfd::command(program);
    cmd.args(argv);
    aterm_uds::deadline::output_until(&mut cmd, Instant::now() + CHILD_HANG)
        .map_err(|e| Error::new(format!("{} {}: {e}", program.display(), argv.join(" "))))
}

/// One child run that must succeed: a refusal is an error carrying what it said.
fn run_ok(program: &Path, argv: &[String]) -> crate::Result<()> {
    succeeded(program, argv, &run(program, argv)?)
}

/// `out`, what `program argv` answered, as [`run_ok`] judges it.
fn succeeded(program: &Path, argv: &[String], out: &std::process::Output) -> crate::Result<()> {
    if out.status.success() {
        return Ok(());
    }
    let said = [&out.stderr, &out.stdout]
        .iter()
        .map(|bytes| String::from_utf8_lossy(bytes).trim().to_string())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("; ");
    Err(Error::new(format!(
        "{} {} ({}){}{said}",
        program.display(),
        argv.join(" "),
        out.status,
        if said.is_empty() { "" } else { ": " }
    )))
}

#[cfg(target_os = "macos")]
fn launchctl(call: Launchctl<'_>) -> crate::Result<std::process::Output> {
    run(
        Path::new(LAUNCHCTL),
        &launchctl_argv(aterm_uds::peer::our_uid(), call),
    )
}

#[cfg(target_os = "macos")]
fn launchctl_ok(call: Launchctl<'_>) -> crate::Result<()> {
    run_ok(
        Path::new(LAUNCHCTL),
        &launchctl_argv(aterm_uds::peer::our_uid(), call),
    )
}

/// What one `launchctl` call answers, run or (in a test) made up.
#[cfg(any(target_os = "macos", all(test, unix)))]
type LaunchctlRun<'a> = dyn FnMut(Launchctl<'_>) -> crate::Result<std::process::Output> + 'a;

/// launchd's view of `label`: `None` when it knows no such job, else its pid if running.
#[cfg(target_os = "macos")]
fn launchd_job(label: &str) -> crate::Result<Option<Option<u32>>> {
    launchd_job_with(label, &mut launchctl)
}

/// [`launchd_job`] through `launchctl`.
#[cfg(any(target_os = "macos", all(test, unix)))]
fn launchd_job_with(
    label: &str,
    launchctl: &mut LaunchctlRun<'_>,
) -> crate::Result<Option<Option<u32>>> {
    let out = launchctl(Launchctl::Print(label))?;
    Ok(out
        .status
        .success()
        .then(|| print_pid(&String::from_utf8_lossy(&out.stdout))))
}

/// `launchctl`'s refusal of `call` as an error carrying what it said.
#[cfg(any(target_os = "macos", all(test, unix)))]
fn launchctl_checked(call: Launchctl<'_>, launchctl: &mut LaunchctlRun<'_>) -> crate::Result<()> {
    let out = launchctl(call)?;
    succeeded(
        Path::new(LAUNCHCTL),
        &launchctl_argv(aterm_uds::peer::our_uid(), call),
        &out,
    )
}

#[cfg(not(target_os = "macos"))]
fn systemctl() -> crate::Result<PathBuf> {
    systemctl_path(&SYSTEMCTL_PATHS, &|path| path.is_file())
}

/// Refuses an unresolved account home or a `home` that is not `account`.
pub fn own_home(home: &Path, account: Option<&Path>) -> crate::Result<()> {
    let Some(account) = account else {
        return Err(Error::new(
            "cannot resolve this login's account home for a per-login service",
        ));
    };
    #[cfg(unix)]
    let same = {
        use std::os::unix::fs::MetadataExt as _;
        match (std::fs::metadata(home), std::fs::metadata(account)) {
            (Ok(a), Ok(b)) => Some((a.dev(), a.ino()) == (b.dev(), b.ino())),
            _ => None,
        }
    };
    #[cfg(not(unix))]
    let same = None;
    let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if same.unwrap_or_else(|| canonical(home) == canonical(account)) {
        return Ok(());
    }
    Err(Error::new(format!(
        "{} is not this login's home ({}): the service is one per login, so a job for it would \
         replace the one {} runs; set it up from the login's own home",
        home.display(),
        account.display(),
        account.display()
    )))
}

/// [`own_home`] against the account database.
fn check_own_home(home: &Path) -> crate::Result<()> {
    #[cfg(unix)]
    {
        own_home(home, aterm_uds::account::home().as_deref())
    }
    #[cfg(not(unix))]
    {
        own_home(home, None)
    }
}

/// Install and start `job` under `home` ([`own_home`], [`plist_path`], [`unit_path`]).
///
/// The subscriber installed is on again: the person's record that it was turned off
/// ([`subscriber_off_path`]) goes.
///
/// A launchd job that runs as this would set it up is left running; a systemd unit is
/// restarted.
pub fn install(home: &Path, job: Job, program: &Path, args: &[String]) -> crate::Result<String> {
    install_restarting(home, job, program, args, false)
}

/// [`install`], restarting the job even when it runs as this would set it up
/// (`aterm secrets agent on --force`).
pub fn reinstall(home: &Path, job: Job, program: &Path, args: &[String]) -> crate::Result<String> {
    install_restarting(home, job, program, args, true)
}

fn install_restarting(
    home: &Path,
    job: Job,
    program: &Path,
    args: &[String],
    restart: bool,
) -> crate::Result<String> {
    check_own_home(home)?;
    if job != Job::Subscriber {
        return install_job(home, job, program, args, restart);
    }
    install_subscriber_with(home, || install_job(home, job, program, args, restart))
}

fn install_subscriber_with(
    home: &Path,
    install: impl FnOnce() -> crate::Result<String>,
) -> crate::Result<String> {
    let _decision = SubscriberDecision::take(home)?;
    let line = install()?;
    // The job runs either way: a record left behind says less than the job that runs.
    Ok(match clear_subscriber_off(home) {
        Ok(()) => line,
        Err(e) => format!("{line}; the record that it was turned off stays ({e})"),
    })
}

/// Repair a subscriber setup already found missing, preserving a person's Sync
/// off recorded since that probe. `None` means it was left off, not installed.
/// The check and install are ordered with [`turn_off`] and explicit [`install`].
///
/// The job keeps the program its unit file names when that one lasts ([`kept_program`]);
/// `program` runs only where it does not, so a caller asks [`kept_program`] first and names its
/// own program only on `None`.
pub fn repair_subscriber(
    home: &Path,
    program: &Path,
    args: &[String],
) -> crate::Result<Option<String>> {
    check_own_home(home)?;
    repair_subscriber_with(home, || {
        let program = kept_program(home, Job::Subscriber).unwrap_or_else(|| program.to_path_buf());
        install_job(home, Job::Subscriber, &program, args, false)
    })
}

/// Repair the vault server's job when the program its unit file names is gone (its `aterm.app`
/// moved or deleted, a disk image ejected), which launchd or systemd can then never start again:
/// installed again running `program()`, with the arguments it had, so it serves the same vaults
/// from the same address and store. `None` where nothing is repaired: no server job, one whose
/// program is there (left as it is, running or not, wherever it is), one a person switched off
/// (a plist's `Disabled` key), one in a shape `aterm secrets server on` never writes (no
/// `secrets serve` after its program), or a home that is not this login's own, whose jobs
/// launchd and systemd never read (as [`state`]). `program` is asked only for a repair.
pub fn repair_server(
    home: &Path,
    program: &dyn Fn() -> crate::Result<PathBuf>,
) -> crate::Result<Option<String>> {
    if check_own_home(home).is_err() {
        return Ok(None);
    }
    repair_server_with(
        home,
        &|path| std::fs::symlink_metadata(path).is_ok(),
        |args| install_job(home, Job::Server, &program()?, args, false),
    )
}

/// [`repair_server`], `exists` saying whether anything is at a path and `install` installing
/// the job with these arguments.
fn repair_server_with(
    home: &Path,
    exists: &dyn Fn(&Path) -> bool,
    install: impl FnOnce(&[String]) -> crate::Result<String>,
) -> crate::Result<Option<String>> {
    let Ok(text) = std::fs::read_to_string(unit_file(home, Job::Server)) else {
        return Ok(None);
    };
    let argv = if cfg!(target_os = "macos") {
        plist_argv(&text).filter(|_| !text.contains("<key>Disabled</key>"))
    } else {
        exec_argv(&text)
    };
    let Some((held, args)) = argv.as_deref().and_then(<[String]>::split_first) else {
        return Ok(None);
    };
    let serves = args
        .get(..2)
        .is_some_and(|words| words == ["secrets", "serve"]);
    if !serves || exists(Path::new(held)) {
        return Ok(None);
    }
    install(args).map(Some)
}

fn repair_subscriber_with(
    home: &Path,
    install: impl FnOnce() -> crate::Result<String>,
) -> crate::Result<Option<String>> {
    let _decision = SubscriberDecision::take(home)?;
    if crate::fsx::kind(&subscriber_off_path(home))? != crate::fsx::Kind::Missing {
        return Ok(None);
    }
    // This is a repair, not an explicit On: it never clears an off decision.
    // A later Off waits for this local install, then records and removes it.
    install().map(Some)
}

/// [`install`]'s job, without the person's record.
fn install_job(
    home: &Path,
    job: Job,
    program: &Path,
    args: &[String],
    restart: bool,
) -> crate::Result<String> {
    #[cfg(target_os = "macos")]
    {
        install_launchd(home, job, (program, args), restart, &mut launchctl)
    }
    #[cfg(not(target_os = "macos"))]
    {
        // systemd restarts the unit on every install, a changed one or not.
        let _ = restart;
        let systemctl = systemctl()?;
        let unit = write_unit(home, job, program, args)?;
        let name = unit_name(job);
        run_ok(&systemctl, &systemctl_argv(&["daemon-reload"]))?;
        run_ok(&systemctl, &systemctl_argv(&["enable", "--now", &name]))?;
        run_ok(&systemctl, &systemctl_argv(&["restart", &name]))?;
        Ok(format!(
            "{name}: systemd --user unit {} runs {} {} (log: journalctl --user -u {name})",
            unit.display(),
            program.display(),
            args.join(" ")
        ))
    }
}

/// [`install_job`] on macOS, each `launchctl` call through `launchctl`. A job launchd runs from
/// the plist this would write is left as it is (one `print`) unless `restart`; any other is
/// booted out if loaded, bootstrapped (`RunAtLoad` starts it) and kickstarted, killing a
/// running instance only when `restart`, so a reinstall never cuts a long-poll twice.
#[cfg(any(target_os = "macos", all(test, unix)))]
fn install_launchd(
    home: &Path,
    job: Job,
    (program, args): (&Path, &[String]),
    restart: bool,
    launchctl: &mut LaunchctlRun<'_>,
) -> crate::Result<String> {
    check_argv(program, args)?;
    let label = job.label();
    let plist = plist_path(home, job);
    let log = log_path(home, job);
    let text = plist_text(label, program, args, &log);
    let line = format!(
        "{label}: launchd user agent {} runs {} {} (log {})",
        plist.display(),
        program.display(),
        args.join(" "),
        log.display()
    );
    let loaded = launchd_job_with(label, launchctl)?;
    if !restart
        && matches!(loaded, Some(Some(_)))
        && std::fs::read(&plist).is_ok_and(|held| held == text.as_bytes())
    {
        return Ok(format!("{line}; already running"));
    }
    write_plist(home, job, program, args)?;
    if loaded.is_some() {
        let _ = launchctl(Launchctl::Bootout(label));
        let settle = Instant::now() + BOOTOUT_SETTLE;
        while launchd_job_with(label, launchctl)?.is_some() && Instant::now() < settle {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    launchctl_checked(Launchctl::Bootstrap(&plist), launchctl).map_err(|e| {
        Error::new(format!(
            "{e} (gui/{} exists only while this user is logged in at the Mac's screen)",
            aterm_uds::peer::our_uid()
        ))
    })?;
    let start = if restart {
        Launchctl::Restart(label)
    } else {
        Launchctl::Kickstart(label)
    };
    launchctl_checked(start, launchctl)?;
    Ok(line)
}

/// Stop and remove `job`, for this login's own `home` ([`own_home`]).
pub fn uninstall(home: &Path, job: Job) -> crate::Result<bool> {
    check_own_home(home)?;
    let _decision = (job == Job::Subscriber)
        .then(|| SubscriberDecision::take(home))
        .transpose()?;
    uninstall_job(home, job)
}

/// Turn a job off by choice. For the subscriber, publish that choice and remove
/// the job in one local decision, so an older automatic repair cannot undo it.
pub fn turn_off(home: &Path, job: Job) -> crate::Result<bool> {
    check_own_home(home)?;
    if job != Job::Subscriber {
        return uninstall_job(home, job);
    }
    turn_off_subscriber_with(home, || uninstall_job(home, job))
}

fn turn_off_subscriber_with(
    home: &Path,
    remove: impl FnOnce() -> crate::Result<bool>,
) -> crate::Result<bool> {
    let _decision = SubscriberDecision::take(home)?;
    record_subscriber_off_locked(home)?;
    remove()
}

/// Exercise explicit subscriber On with an inert local service action.
#[cfg(any(test, debug_assertions))]
pub fn install_subscriber_for_test(
    home: &Path,
    install: impl FnOnce() -> crate::Result<String>,
) -> crate::Result<String> {
    install_subscriber_with(home, install)
}

/// Exercise automatic subscriber repair with an inert local service action.
#[cfg(any(test, debug_assertions))]
pub fn repair_subscriber_for_test(
    home: &Path,
    install: impl FnOnce() -> crate::Result<String>,
) -> crate::Result<Option<String>> {
    repair_subscriber_with(home, install)
}

/// Exercise subscriber Off with an inert local service action.
#[cfg(any(test, debug_assertions))]
pub fn turn_off_subscriber_for_test(
    home: &Path,
    remove: impl FnOnce() -> crate::Result<bool>,
) -> crate::Result<bool> {
    turn_off_subscriber_with(home, remove)
}

fn uninstall_job(home: &Path, job: Job) -> crate::Result<bool> {
    let remove = |path: &Path| match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(Error::new(format!("{}: {e}", path.display()))),
    };
    #[cfg(target_os = "macos")]
    {
        let label = job.label();
        let loaded = launchd_job(label)?.is_some();
        if loaded {
            launchctl_ok(Launchctl::Bootout(label))?;
        }
        let removed = remove(&plist_path(home, job))?;
        Ok(loaded || removed)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let unit = unit_path(home, job);
        if crate::fsx::kind(&unit)? == crate::fsx::Kind::Missing {
            return Ok(false);
        }
        let name = unit_name(job);
        if let Ok(systemctl) = systemctl() {
            let _ = run(&systemctl, &systemctl_argv(&["disable", "--now", &name]));
            remove(&unit)?;
            run_ok(&systemctl, &systemctl_argv(&["daemon-reload"]))?;
        } else {
            remove(&unit)?;
        }
        Ok(true)
    }
}

/// `job`'s state: absent for a home that is not this login's own ([`own_home`]).
pub fn state(home: &Path, job: Job) -> crate::Result<State> {
    // The login's job is not this home's: a scratch `HOME` has none.
    if check_own_home(home).is_err() {
        return Ok(State::Absent);
    }
    #[cfg(target_os = "macos")]
    {
        let unit = plist_path(home, job);
        let on_disk = crate::fsx::kind(&unit)? != crate::fsx::Kind::Missing;
        match launchd_job(job.label())? {
            Some(pid) => Ok(State::Installed { pid, unit }),
            None if on_disk => Ok(State::Installed { pid: None, unit }),
            None => Ok(State::Absent),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let unit = unit_path(home, job);
        if crate::fsx::kind(&unit)? == crate::fsx::Kind::Missing {
            return Ok(State::Absent);
        }
        let pid = match systemctl() {
            Ok(systemctl) => {
                let out = run(
                    &systemctl,
                    &systemctl_argv(&["show", "-p", "MainPID", "--value", &unit_name(job)]),
                )?;
                if out.status.success() {
                    main_pid(&String::from_utf8_lossy(&out.stdout))
                } else {
                    None
                }
            }
            Err(_) => None,
        };
        Ok(State::Installed { pid, unit })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repair_keeps_the_lasting_program_its_job_names() {
        let held = Path::new("/Users//o/.local/share/aterm-secrets/aterm");
        let nothing = |_: &Path| false;
        let all = |_: &Path| true;
        // A held snapshot the job runs stays, whatever aterm repairs it.
        assert_eq!(
            kept(Some(held.into()), &nothing, &all).as_deref(),
            Some(held)
        );
        // CONTROL: gone or not executable, or no unit naming one, nothing is kept.
        assert_eq!(kept(Some(held.into()), &nothing, &nothing), None);
        assert_eq!(kept(None, &nothing, &all), None);
        // A build, or a copy that goes away, is no program to keep.
        for fleeting in [
            "/Users//o/aterm/target/release/aterm",
            "/Volumes/aterm/aterm.app/Contents/MacOS/aterm",
            "/tmp/aterm",
        ] {
            assert_eq!(
                kept(Some(fleeting.into()), &nothing, &all),
                None,
                "{fleeting}"
            );
        }
        // A file in a bundle that is not its executable names another program.
        let inner = Path::new("/Applications/b.app/Contents/Resources/aterm");
        let bundled = |path: &Path| path == Path::new("/Applications/b.app/Contents/MacOS/aterm");
        assert_eq!(kept(Some(inner.into()), &bundled, &all), None);
    }

    #[test]
    fn a_unit_names_back_the_program_it_starts() {
        let program = Path::new("/Applications/A&B <\"q'>.app/Contents/MacOS/aterm");
        let args = ["secrets".to_string(), "watch".to_string()];
        let plist = plist_text("l", program, &args, Path::new("/var/log/x.log"));
        assert_eq!(plist_program(&plist).as_deref(), Some(program));
        let odd = Path::new("/home/o neil/50%$x\"q\\/aterm");
        let unit = systemd_unit("d", odd, &args);
        assert_eq!(exec_program(&unit).as_deref(), Some(odd));
        // A form neither writer makes names nothing.
        for unit in [
            "ExecStart=/bin/aterm watch",
            "ExecStart=\"/a\\x41\"",
            "ExecStart=\"/a%b\"",
            "",
        ] {
            assert_eq!(exec_program(unit), None, "{unit}");
        }
        for plist in [
            "<key>ProgramArguments</key><array><string>/a&nbsp;</string>",
            "<string>/a</string>",
        ] {
            assert_eq!(plist_program(plist), None, "{plist}");
        }
    }

    #[test]
    fn a_unit_names_back_its_whole_argv() {
        let program = Path::new("/Applications/A&B <\"q'>.app/Contents/MacOS/aterm");
        let args = [
            "secrets".to_string(),
            "serve".to_string(),
            "--listen".to_string(),
            "0.0.0.0:7484".to_string(),
            "a&b <\"q'> 50%$".to_string(),
            String::new(),
        ];
        let whole: Vec<String> = std::iter::once(program.to_string_lossy().into_owned())
            .chain(args.iter().cloned())
            .collect();
        let plist = plist_text("l", program, &args, Path::new("/var/log/x.log"));
        assert_eq!(plist_argv(&plist), Some(whole.clone()));
        assert_eq!(
            exec_argv(&systemd_unit("d", program, &args)),
            Some(whole),
            "systemd's words"
        );
        for unit in [
            "ExecStart=\"/a\" watch",
            "ExecStart=\"/a\"  \"b\"",
            "ExecStart=\"/a\"\"b\"",
            "ExecStart=\"/a",
            "",
        ] {
            assert_eq!(exec_argv(unit), None, "{unit}");
        }
        for plist in [
            "<key>ProgramArguments</key><array><string>/a</string>",
            "<key>ProgramArguments</key><array></array>",
            "<key>ProgramArguments</key><array><string>/a&nbsp;</string></array>",
        ] {
            assert_eq!(plist_argv(plist), None, "{plist}");
        }
    }

    /// A server job whose program is gone is installed again with the arguments it had; one
    /// whose program is there, one switched off, one in another shape and none at all are left.
    #[test]
    fn a_server_job_whose_program_is_gone_is_installed_again_as_it_was() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let home = root.path();
        let gone = Path::new("/Volumes/aterm/aterm.app/Contents/MacOS/aterm");
        let args: Vec<String> = ["secrets", "serve", "--listen", "0.0.0.0:7484"]
            .map(String::from)
            .into();
        let write = |text: String| {
            let unit = unit_file(home, Job::Server);
            std::fs::create_dir_all(unit.parent().expect("parent")).expect("dir");
            std::fs::write(&unit, text).expect("unit");
        };
        let unit_text = |program: &Path, args: &[String]| {
            if cfg!(target_os = "macos") {
                plist_text(
                    Job::Server.label(),
                    program,
                    args,
                    &log_path(home, Job::Server),
                )
            } else {
                systemd_unit(Job::Server.description(), program, args)
            }
        };
        let repair = |exists: bool| {
            let mut installed = None;
            let line = repair_server_with(home, &|path| exists && path == gone, |args| {
                installed = Some(args.to_vec());
                Ok("installed".to_string())
            })
            .expect("read");
            (line, installed)
        };
        assert_eq!(repair(false), (None, None), "no server job");
        write(unit_text(gone, &args));
        assert_eq!(
            repair(false),
            (Some("installed".to_string()), Some(args.clone())),
            "the program gone"
        );
        assert_eq!(repair(true), (None, None), "the program there");
        let watch = ["secrets".to_string(), "watch".to_string()];
        write(unit_text(gone, &watch));
        assert_eq!(repair(false), (None, None), "not a server's arguments");
        if cfg!(target_os = "macos") {
            let off = unit_text(gone, &args).replace(
                "<key>RunAtLoad</key>",
                "<key>Disabled</key>\n\t<true/>\n\t<key>RunAtLoad</key>",
            );
            write(off);
            assert_eq!(repair(false), (None, None), "switched off");
        }
        // A home not this login's own (this scratch one) is never repaired, nor its program
        // asked: launchd and systemd never read its jobs.
        write(unit_text(gone, &args));
        assert_eq!(
            repair_server(home, &|| panic!("a program asked for")).expect("no error"),
            None
        );
    }

    fn assert_subscriber_decision_held(home: &Path) {
        let independent = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(secrets_dir(home).join("subscriber.lock"))
            .unwrap();
        #[expect(
            clippy::disallowed_methods,
            reason = "an independent scratch handle proves the lifecycle callback still holds its decision lock"
        )]
        let taken = independent.try_lock();
        assert!(matches!(taken, Err(std::fs::TryLockError::WouldBlock)));
    }

    fn words(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn a_job_is_set_up_only_for_the_logins_own_home() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let own = root.path().join("own");
        let scratch = root.path().join("scratch");
        std::fs::create_dir_all(&own).expect("dir");
        std::fs::create_dir_all(&scratch).expect("dir");
        own_home(&own, Some(&own)).expect("its own");
        own_home(&own.join("."), Some(&own)).expect("spelt another way");
        #[cfg(unix)]
        {
            let link = root.path().join("link");
            std::os::unix::fs::symlink(&own, &link).expect("link");
            own_home(&link, Some(&own)).expect("through a link");
        }
        let error = own_home(&scratch, Some(&own)).expect_err("another home");
        assert!(error.0.contains("not this login's home"), "{error}");
        for home in [&own, &scratch] {
            assert!(
                own_home(home, None).is_err(),
                "an unresolved account cannot authorize its per-login job"
            );
        }
    }

    /// A scratch HOME is never this login's, so its state is read without asking launchd.
    #[cfg(unix)]
    #[test]
    fn a_home_that_is_not_the_logins_has_no_job() {
        let scratch = aterm_tempfile::tempdir().expect("tempdir");
        for job in [Job::Subscriber, Job::Server] {
            assert!(matches!(
                state(scratch.path(), job).expect("state"),
                State::Absent
            ));
        }
    }

    /// The record that the subscriber is off is private, goes once it is on again, and is no
    /// error to remove when absent.
    #[cfg(unix)]
    #[test]
    fn the_record_that_the_subscriber_was_turned_off_is_kept_until_it_is_on_again() {
        use std::os::unix::fs::PermissionsExt as _;
        let scratch = aterm_tempfile::tempdir().expect("tempdir");
        let home = scratch.path();
        assert!(!subscriber_turned_off(home));
        record_subscriber_off(home).expect("recorded");
        assert!(subscriber_turned_off(home));
        let path = subscriber_off_path(home);
        assert_eq!(path, secrets_dir(home).join("subscriber-off"));
        let mode = |path: &Path| {
            std::fs::metadata(path)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&secrets_dir(home)), 0o700);
        record_subscriber_off(home).expect("again");
        clear_subscriber_off(home).expect("cleared");
        assert!(!subscriber_turned_off(home));
        clear_subscriber_off(home).expect("nothing to clear");
    }

    #[test]
    fn subscriber_repair_preserves_off_but_fresh_on_can_replace_it() {
        use std::cell::Cell;
        let scratch = aterm_tempfile::tempdir().unwrap();
        let home = scratch.path();
        let installs = Cell::new(0);
        let install = || {
            assert_subscriber_decision_held(home);
            installs.set(installs.get() + 1);
            Ok("scratch subscriber installed".to_owned())
        };
        assert!(repair_subscriber_with(home, install).unwrap().is_some());
        assert_eq!(
            installs.get(),
            1,
            "an unchanged enabled repair still installs"
        );
        turn_off_subscriber_with(home, || {
            assert_subscriber_decision_held(home);
            assert!(subscriber_turned_off(home), "off is saved before removal");
            Ok(true)
        })
        .unwrap();
        assert!(repair_subscriber_with(home, install).unwrap().is_none());
        assert_eq!(installs.get(), 1, "the older repair calls no installer");
        assert!(subscriber_turned_off(home));
        install_subscriber_with(home, || {
            assert_subscriber_decision_held(home);
            Err(Error::new("injected install failure"))
        })
        .unwrap_err();
        assert!(
            subscriber_turned_off(home),
            "a failed On preserves the choice"
        );
        install_subscriber_with(home, install).unwrap();
        assert_eq!(installs.get(), 2, "explicit On or Join can install");
        assert!(!subscriber_turned_off(home));
    }

    #[test]
    fn subscriber_off_after_a_repair_orders_removal_after_its_install() {
        use std::cell::Cell;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::mpsc;
        let scratch = aterm_tempfile::tempdir().unwrap();
        let home = scratch.path().to_owned();
        let running = Arc::new(AtomicBool::new(false));
        let (entered, installing) = mpsc::sync_channel(1);
        let (release, finish_install) = mpsc::sync_channel(1);
        let repair_home = home.clone();
        let repair_running = Arc::clone(&running);
        let repair = std::thread::spawn(move || {
            repair_subscriber_with(&repair_home, || {
                assert_subscriber_decision_held(&repair_home);
                repair_running.store(true, Ordering::SeqCst);
                entered.send(()).unwrap();
                finish_install
                    .recv_timeout(Duration::from_secs(60))
                    .unwrap();
                Ok("scratch subscriber installed".into())
            })
        });
        installing.recv_timeout(Duration::from_secs(60)).unwrap();
        let off_home = home.clone();
        let off_running = Arc::clone(&running);
        let (requested, requesting) = mpsc::sync_channel(1);
        let off = std::thread::spawn(move || {
            requested.send(()).unwrap();
            turn_off_subscriber_with(&off_home, || {
                assert_subscriber_decision_held(&off_home);
                assert!(subscriber_turned_off(&off_home));
                off_running.store(false, Ordering::SeqCst);
                Ok(true)
            })
        });
        requesting.recv_timeout(Duration::from_secs(60)).unwrap();
        release.send(()).unwrap();
        assert!(repair.join().unwrap().unwrap().is_some());
        assert!(off.join().unwrap().unwrap());
        assert!(subscriber_turned_off(&home));
        assert!(!running.load(Ordering::SeqCst));
        let stale_install = Cell::new(false);
        assert!(
            repair_subscriber_with(&home, || {
                stale_install.set(true);
                Ok("must not install".into())
            })
            .unwrap()
            .is_none()
        );
        assert!(!stale_install.get());
    }

    #[test]
    fn subscriber_off_write_failure_never_removes_the_job() {
        use std::cell::Cell;
        let scratch = aterm_tempfile::tempdir().unwrap();
        let home = scratch.path();
        let decision = SubscriberDecision::take(home).unwrap();
        std::fs::create_dir(subscriber_off_path(home)).unwrap();
        drop(decision);
        let removed = Cell::new(false);
        assert!(
            turn_off_subscriber_with(home, || {
                removed.set(true);
                Ok(true)
            })
            .is_err()
        );
        assert!(!removed.get());
    }

    #[test]
    fn labels_and_paths_sit_where_launchd_systemd_and_the_design_say() {
        let home = Path::new("/Users//x");
        assert_eq!(Job::Subscriber.label(), "systems.alab.aterm-secrets");
        assert_eq!(Job::Server.label(), "systems.alab.aterm-secrets-server");
        assert_eq!(
            plist_path(home, Job::Subscriber),
            Path::new("/Users//x/Library/LaunchAgents/systems.alab.aterm-secrets.plist")
        );
        assert_eq!(
            unit_path(home, Job::Server),
            Path::new("/Users//x/.config/systemd/user/systems.alab.aterm-secrets-server.service")
        );
        assert_eq!(
            log_path(home, Job::Subscriber),
            Path::new("/Users//x/.config/aterm/secrets/systems.alab.aterm-secrets.log")
        );
    }

    #[test]
    fn the_plist_names_the_job_keeps_it_alive_and_escapes_every_string() {
        let log = log_path(Path::new("/Users//a&b"), Job::Subscriber);
        let text = plist_text(
            Job::Subscriber.label(),
            Path::new("/Applications/A&B <\"q'>.app/Contents/MacOS/aterm"),
            &words(&["secrets", "watch", "<&>"]),
            &log,
        );
        assert!(text.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist"));
        assert!(text.ends_with("</dict>\n</plist>\n"));
        assert!(
            text.contains("\t<key>Label</key>\n\t<string>systems.alab.aterm-secrets</string>\n")
        );
        assert!(text.contains(
            "\t<key>ProgramArguments</key>\n\t<array>\n\
             \t\t<string>/Applications/A&amp;B &lt;&quot;q&apos;&gt;.app/Contents/MacOS/aterm</string>\n\
             \t\t<string>secrets</string>\n\
             \t\t<string>watch</string>\n\
             \t\t<string>&lt;&amp;&gt;</string>\n\
             \t</array>\n"
        ));
        assert!(!text.contains("A&B"), "an unescaped ampersand");
        assert!(text.contains("\t<key>RunAtLoad</key>\n\t<true/>\n"));
        assert!(text.contains("\t<key>KeepAlive</key>\n\t<true/>\n"));
        assert!(text.contains("\t<key>ProcessType</key>\n\t<string>Background</string>\n"));
        assert!(text.contains("\t<key>Nice</key>\n\t<integer>5</integer>\n"));
        assert!(text.contains(
            "\t<key>SoftResourceLimits</key>\n\t<dict>\n\t\t<key>NumberOfFiles</key>\n\t\t<integer>4096</integer>\n\t</dict>\n"
        ));
        assert!(text.contains(
            "\t<key>EnvironmentVariables</key>\n\t<dict>\n\t\t<key>HOME</key>\n\t\t<string>/Users//a&amp;b</string>\n\t</dict>\n"
        ));
        let log_line =
            "<string>/Users//a&amp;b/.config/aterm/secrets/systems.alab.aterm-secrets.log</string>";
        assert!(text.contains(&format!("\t<key>StandardOutPath</key>\n\t{log_line}\n")));
        assert!(text.contains(&format!("\t<key>StandardErrorPath</key>\n\t{log_line}\n")));
        assert_eq!(text.matches("<key>").count(), 12, "{text}");
    }

    #[test]
    fn a_log_elsewhere_writes_no_environment() {
        let text = plist_text("l", Path::new("/t/aterm"), &[], Path::new("/var/log/x.log"));
        assert!(!text.contains("EnvironmentVariables"), "{text}");
        assert!(text.contains("\t<array>\n\t\t<string>/t/aterm</string>\n\t</array>\n"));
        assert_eq!(
            home_of_log(Path::new("/h/.config/aterm/secrets/l.log")),
            Some(Path::new("/h"))
        );
        assert_eq!(home_of_log(Path::new(".config/aterm/secrets/l.log")), None);
        assert_eq!(home_of_log(Path::new("/h/.config/aterm/other/l.log")), None);
    }

    /// `plutil -lint` reads the plist as launchd will.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_plist_is_well_formed_by_plutil() {
        let plutil = Path::new("/usr/bin/plutil");
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let lint = |name: &str, text: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, text).expect("write");
            let mut cmd = aterm_uds::spawnfd::command(plutil);
            cmd.arg("-lint").arg(&path);
            let out = aterm_uds::deadline::output_until(
                &mut cmd,
                Instant::now() + Duration::from_secs(60),
            )
            .expect("plutil");
            (
                out.status.success(),
                String::from_utf8_lossy(&out.stdout).into_owned(),
            )
        };
        let full = plist_text(
            Job::Server.label(),
            Path::new("/A&B <\"q'>/x.app/Contents/MacOS/aterm"),
            &words(&["secrets", "serve", "--listen", "0.0.0.0:7484", "a & b"]),
            &log_path(Path::new("/Users//o'neil & co"), Job::Server),
        );
        let (ok, said) = lint("full.plist", &full);
        assert!(ok, "{said}\n{full}");
        let bare = plist_text("l", Path::new("/t/aterm"), &[], Path::new("/l.log"));
        let (ok, said) = lint("bare.plist", &bare);
        assert!(ok, "{said}\n{bare}");
        let broken = full.replace("A&amp;B", "A&B");
        let (ok, _) = lint("broken.plist", &broken);
        assert!(!ok, "the control lints");
    }

    #[test]
    fn a_systemd_word_reads_back_as_itself() {
        assert_eq!(systemd_word("/usr/bin/aterm"), "\"/usr/bin/aterm\"");
        assert_eq!(systemd_word("a b"), "\"a b\"");
        assert_eq!(systemd_word("say \"hi\""), "\"say \\\"hi\\\"\"");
        assert_eq!(systemd_word("back\\slash"), "\"back\\\\slash\"");
        assert_eq!(systemd_word("100%"), "\"100%%\"");
        assert_eq!(systemd_word("%h/x"), "\"%%h/x\"");
        assert_eq!(systemd_word("$HOME ${X}"), "\"$$HOME $${X}\"");
        assert_eq!(systemd_word("it's"), "\"it's\"");
        assert_eq!(systemd_word("a\nb\tc\rd"), "\"a\\nb\\tc\\rd\"");
        assert_eq!(systemd_word("\u{1}\u{7f}\u{85}"), "\"\\x01\\x7f\\u0085\"");
        assert_eq!(systemd_word(""), "\"\"");
        assert_eq!(systemd_word("π"), "\"π\"");
    }

    #[test]
    fn the_unit_runs_the_quoted_argv_and_always_restarts() {
        let unit = systemd_unit(
            Job::Subscriber.description(),
            Path::new("/home/o neil/.local/bin/aterm"),
            &words(&["secrets", "watch", "50%", "$x"]),
        );
        assert_eq!(
            unit,
            "[Unit]\n\
             Description=aterm secrets subscriber (aterm secrets watch)\n\
             \n\
             [Service]\n\
             ExecStart=\"/home/o neil/.local/bin/aterm\" \"secrets\" \"watch\" \"50%%\" \"$$x\"\n\
             Restart=always\n\
             RestartSec=5\n\
             LimitNOFILE=4096\n\
             \n\
             [Install]\n\
             WantedBy=default.target\n"
        );
        let odd = systemd_unit("100% sure\nof it\\", Path::new("/a"), &[]);
        assert!(odd.contains("Description=100%% sure of it\n"), "{odd}");
    }

    #[test]
    fn the_launchctl_and_systemctl_argv_are_plain() {
        assert_eq!(
            launchctl_argv(501, Launchctl::Print("systems.alab.aterm-secrets")),
            ["print", "gui/501/systems.alab.aterm-secrets"]
        );
        assert_eq!(
            launchctl_argv(501, Launchctl::Bootout("l.x")),
            ["bootout", "gui/501/l.x"]
        );
        assert_eq!(
            launchctl_argv(
                501,
                Launchctl::Bootstrap(Path::new("/h/Library/LaunchAgents/l.x.plist"))
            ),
            ["bootstrap", "gui/501", "/h/Library/LaunchAgents/l.x.plist"]
        );
        assert_eq!(
            launchctl_argv(502, Launchctl::Kickstart("l.x")),
            ["kickstart", "gui/502/l.x"]
        );
        assert_eq!(
            launchctl_argv(502, Launchctl::Restart("l.x")),
            ["kickstart", "-k", "gui/502/l.x"]
        );
        assert_eq!(
            systemctl_argv(&["daemon-reload"]),
            ["--user", "daemon-reload"]
        );
        assert_eq!(
            systemctl_argv(&["enable", "--now", "l.service"]),
            ["--user", "enable", "--now", "l.service"]
        );
        assert_eq!(
            systemctl_argv(&["show", "-p", "MainPID", "--value", "l.service"]),
            ["--user", "show", "-p", "MainPID", "--value", "l.service"]
        );
    }

    /// launchd as `install_launchd` sees it: one job, loaded or not, and every call it got.
    #[cfg(unix)]
    #[derive(Default)]
    struct FakeLaunchd {
        loaded: bool,
        calls: Vec<Vec<String>>,
    }

    #[cfg(unix)]
    impl FakeLaunchd {
        fn answer(&mut self, call: Launchctl<'_>) -> crate::Result<std::process::Output> {
            use std::os::unix::process::ExitStatusExt as _;
            self.calls.push(launchctl_argv(0, call));
            let (ok, stdout) = match call {
                Launchctl::Print(_) if self.loaded => (true, "\tpid = 42\n"),
                Launchctl::Print(_) => (false, ""),
                Launchctl::Bootout(_) => (std::mem::take(&mut self.loaded), ""),
                Launchctl::Bootstrap(_) => (!std::mem::replace(&mut self.loaded, true), ""),
                Launchctl::Kickstart(_) | Launchctl::Restart(_) => (self.loaded, ""),
            };
            Ok(std::process::Output {
                status: std::process::ExitStatus::from_raw(if ok { 0 } else { 1 << 8 }),
                stdout: stdout.as_bytes().to_vec(),
                stderr: Vec::new(),
            })
        }

        /// Install `args` (restarting when `restart`): its line, and the calls it made.
        fn install(
            &mut self,
            home: &Path,
            program: &Path,
            args: &[&str],
            restart: bool,
        ) -> (String, Vec<String>) {
            self.calls.clear();
            let line = install_launchd(
                home,
                Job::Subscriber,
                (program, &words(args)),
                restart,
                &mut |call| self.answer(call),
            )
            .expect("installs");
            let calls = self.calls.iter().map(|argv| argv.join(" ")).collect();
            (line, calls)
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_job_launchd_runs_as_installed_is_left_running_and_no_install_kills_one() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let home = root.path().join("home");
        let program = root.path().join("aterm");
        std::fs::write(&program, b"").expect("program");
        let label = Job::Subscriber.label();
        let plist = plist_path(&home, Job::Subscriber).display().to_string();
        let print = format!("print gui/0/{label}");
        let bootout = format!("bootout gui/0/{label}");
        let bootstrap = format!("bootstrap gui/0 {plist}");
        let kickstart = format!("kickstart gui/0/{label}");
        let mut launchd = FakeLaunchd::default();

        // Fresh: bootstrapped, then started without killing anything.
        let (_, calls) = launchd.install(&home, &program, &["secrets", "watch"], false);
        assert_eq!(
            calls,
            [print.as_str(), bootstrap.as_str(), kickstart.as_str()]
        );

        // `agent on` again, the job running from the same plist: one print, nothing else.
        let (line, calls) = launchd.install(&home, &program, &["secrets", "watch"], false);
        assert_eq!(calls, [print.as_str()]);
        assert!(line.ends_with("; already running"), "{line}");

        // CONTROL: a changed plist reloads the job, still never `kickstart -k`.
        let (line, calls) = launchd.install(&home, &program, &["secrets", "watch", "-v"], false);
        assert_eq!(
            calls,
            [
                print.as_str(),
                bootout.as_str(),
                print.as_str(),
                bootstrap.as_str(),
                kickstart.as_str()
            ]
        );
        assert!(!line.ends_with("; already running"), "{line}");
        let written = std::fs::read_to_string(&plist).expect("plist");
        assert!(written.contains("<string>-v</string>"), "{written}");

        // `agent on --force` restarts the job it would otherwise leave running.
        let (_, calls) = launchd.install(&home, &program, &["secrets", "watch", "-v"], true);
        let restart = format!("kickstart -k gui/0/{label}");
        assert_eq!(
            calls,
            [
                print.as_str(),
                bootout.as_str(),
                print.as_str(),
                bootstrap.as_str(),
                restart.as_str()
            ]
        );

        // A job launchd holds but does not run is started again.
        let (_, calls) = {
            let mut stopped = |call: Launchctl<'_>| match call {
                Launchctl::Print(_) if launchd.loaded => {
                    let mut out = launchd.answer(call)?;
                    out.stdout = b"\tstate = not running\n".to_vec();
                    Ok(out)
                }
                _ => launchd.answer(call),
            };
            launchd_calls(&home, &program, &mut stopped)
        };
        assert_eq!(
            calls,
            [
                print.as_str(),
                bootout.as_str(),
                print.as_str(),
                bootstrap.as_str(),
                kickstart.as_str()
            ]
        );
    }

    #[cfg(unix)]
    fn launchd_calls(
        home: &Path,
        program: &Path,
        launchctl: &mut LaunchctlRun<'_>,
    ) -> (String, Vec<String>) {
        let mut calls = Vec::new();
        let line = install_launchd(
            home,
            Job::Subscriber,
            (program, &words(&["secrets", "watch", "-v"])),
            false,
            &mut |call| {
                calls.push(launchctl_argv(0, call).join(" "));
                launchctl(call)
            },
        )
        .expect("installs");
        (line, calls)
    }

    #[test]
    fn the_pid_is_the_services_own_line() {
        let running = "gui/501/systems.alab.aterm-secrets = {\n\
                       \tactive count = 1\n\
                       \tstate = running\n\
                       \n\
                       \tenvironment = {\n\
                       \t\tHOME => /Users//x\n\
                       \t}\n\
                       \n\
                       \truns = 1\n\
                       \tpid = 1688\n\
                       }\n";
        assert_eq!(print_pid(running), Some(1688));
        assert_eq!(print_pid("gui/501/l = {\n\tstate = not running\n}\n"), None);
        assert_eq!(
            print_pid("l = {\n\tsection = {\n\t\tpid = 9\n\t}\n}\n"),
            None
        );
        assert_eq!(print_pid("\tpid = 0\n"), None);
        assert_eq!(print_pid("\tpid = 77  \n"), Some(77));
        assert_eq!(main_pid("4242\n"), Some(4242));
        assert_eq!(main_pid("MainPID=4242\n"), Some(4242));
        assert_eq!(main_pid("0\n"), None);
        assert_eq!(main_pid(""), None);
    }

    #[test]
    fn systemctl_is_found_by_absolute_path_or_refused_by_name() {
        let found = systemctl_path(&SYSTEMCTL_PATHS, &|p| p == Path::new("/bin/systemctl"));
        assert_eq!(found, Ok(PathBuf::from("/bin/systemctl")));
        let first = systemctl_path(&SYSTEMCTL_PATHS, &|_| true);
        assert_eq!(first, Ok(PathBuf::from("/usr/bin/systemctl")));
        let none = systemctl_path(&SYSTEMCTL_PATHS, &|_| false).unwrap_err();
        assert!(
            none.0.contains("no systemctl at /usr/bin/systemctl"),
            "{none}"
        );
        assert!(none.0.contains("supervisor"), "{none}");
    }

    #[test]
    fn a_build_in_a_target_directory_is_never_a_services_program() {
        let cargo = |p: &Path| p == Path::new("/Users//x/aterm/Cargo.toml");
        let none = |_: &Path| false;
        assert!(in_build_dir(
            Path::new("/Users//x/aterm/target/debug/aterm"),
            &cargo
        ));
        assert!(in_build_dir(
            Path::new("/Users//x/aterm/target/x/y/aterm"),
            &cargo
        ));
        assert!(in_build_dir(Path::new("/w/target/release/aterm"), &none));
        assert!(in_build_dir(
            Path::new("/w/target/aarch64-apple-darwin/release/aterm"),
            &none
        ));
        assert!(in_build_dir(
            Path::new("/Users//x/.cache/aterm-secrets-init.noindex/target/debug/deps/aterm"),
            &none
        ));
        assert!(in_build_dir(
            Path::new("/Users//x/aterm/target.noindex/debug/aterm"),
            &none
        ));
        assert!(in_build_dir(
            Path::new("/Users//x/aterm/target.noindex/aterm (dev).app/Contents/MacOS/aterm"),
            &none
        ));
        for path in [
            "/Applications/aterm.app/Contents/MacOS/aterm",
            "/Users//x/.local/bin/aterm",
            "/Users//x/target",
            "/opt/target/bin/aterm",
            "/Users//x/aterm-secrets-init.noindex/aterm",
            "/Users//x/aterm-secrets-init.noindex",
        ] {
            assert!(!in_build_dir(Path::new(path), &none), "{path}");
        }
    }

    #[test]
    fn a_bundled_executable_runs_from_its_bundle_and_a_build_is_refused() {
        let none = |_: &Path| false;
        let app = Path::new("/Applications/aterm.app/Contents/MacOS/aterm");
        assert_eq!(service_program(app, &none), Ok(app.to_path_buf()));
        let plain = Path::new("/Users//x/.local/bin/aterm");
        assert_eq!(service_program(plain, &none), Ok(plain.to_path_buf()));
        let main = Path::new("/Applications/aterm.app/Contents/MacOS/aterm");
        let helper = Path::new("/Applications/aterm.app/Contents/Helpers/aterm");
        assert_eq!(
            service_program(helper, &|p: &Path| p == main),
            Ok(main.to_path_buf())
        );
        let nested =
            Path::new("/Applications/aterm.app/Contents/Library/X.app/Contents/MacOS/aterm");
        assert_eq!(
            service_program(nested, &|p: &Path| p == main),
            Ok(main.to_path_buf())
        );
        let lost = service_program(helper, &none).unwrap_err();
        assert!(lost.0.contains("--program"), "{lost}");
        let build = service_program(Path::new("/w/aterm/target/debug/aterm"), &none).unwrap_err();
        assert!(build.0.contains("cargo target directory"), "{build}");
        assert!(build.0.contains("--program"), "{build}");
        assert!(service_program(Path::new("aterm"), &none).is_err());
    }

    #[test]
    fn a_copy_that_goes_away_is_never_a_services_program() {
        let none = |_: &Path| false;
        let temp = std::env::temp_dir().join("x").join("aterm");
        let resolved = std::env::temp_dir()
            .canonicalize()
            .expect("the temporary directory")
            .join("aterm");
        for path in [
            Path::new("/Volumes/aterm/aterm.app/Contents/MacOS/aterm"),
            Path::new("/private/var/folders/x/AppTranslocation/Y/d/aterm.app/Contents/MacOS/aterm"),
            Path::new("/tmp/aterm"),
            Path::new("/var/tmp/x/aterm"),
            &temp,
            &resolved,
        ] {
            let refused = service_program(path, &none).unwrap_err();
            assert!(refused.0.contains("goes away"), "{refused}");
            assert!(refused.0.contains("--program"), "{refused}");
        }
        for path in [
            "/Applications/aterm.app/Contents/MacOS/aterm",
            "/Users//x/.local/bin/aterm",
            "/Users//x/Volumes/aterm",
            "/tmpfoo/aterm",
        ] {
            assert!(!transient(Path::new(path)), "{path}");
        }
    }

    #[test]
    fn the_test_binary_itself_is_refused_as_a_build() {
        let refused = program_for_service().unwrap_err();
        assert!(refused.0.contains("cargo target directory"), "{refused}");
    }

    #[test]
    fn a_tagged_cache_directory_is_a_build_whatever_its_name() {
        let tag = |p: &Path| p == Path::new("/scratch/out/CACHEDIR.TAG");
        assert!(in_build_dir(
            Path::new("/scratch/out/debug/deps/aterm-0123"),
            &tag
        ));
        assert!(!in_build_dir(Path::new("/scratch/other/aterm"), &tag));
        let own = |p: &Path| p == Path::new("/scratch/aterm/CACHEDIR.TAG");
        assert!(!in_build_dir(Path::new("/scratch/aterm"), &own));
    }

    #[test]
    fn writing_the_plist_makes_a_0644_file_and_a_private_log_directory() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let home = root.path().join("home");
        std::fs::create_dir(&home).expect("home");
        let program = root.path().join("aterm");
        std::fs::write(&program, b"exe").expect("program");
        let args = words(&["secrets", "watch"]);
        let plist = write_plist(&home, Job::Subscriber, &program, &args).expect("written");
        assert_eq!(plist, plist_path(&home, Job::Subscriber));
        let mode = |p: &Path| std::fs::metadata(p).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode(&plist), 0o644);
        assert_eq!(mode(&home.join(".config/aterm/secrets")), 0o700);
        let text = std::fs::read_to_string(&plist).expect("read");
        assert_eq!(
            text,
            plist_text(
                Job::Subscriber.label(),
                &program,
                &args,
                &log_path(&home, Job::Subscriber)
            )
        );
        assert!(text.contains(&format!(
            "<string>{}</string>",
            aterm_codec::xml::escape(&home.to_string_lossy())
        )));
        write_plist(&home, Job::Subscriber, &program, &args).expect("rewritten");
        let names: Vec<_> = std::fs::read_dir(plist.parent().expect("dir"))
            .expect("list")
            .map(|e| e.expect("entry").file_name())
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");

        let unit = write_unit(&home, Job::Server, &program, &args).expect("unit");
        assert_eq!(mode(&unit), 0o644);
        assert_eq!(
            std::fs::read_to_string(&unit).expect("read"),
            systemd_unit(Job::Server.description(), &program, &args)
        );
    }

    #[test]
    fn a_unit_file_is_refused_for_what_it_cannot_name() {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let home = root.path();
        let program = root.path().join("aterm");
        std::fs::write(&program, b"exe").expect("program");
        let refuse = |program: &Path, args: &[String]| {
            write_plist(home, Job::Subscriber, program, args).unwrap_err()
        };
        assert!(refuse(Path::new("aterm"), &[]).0.contains("absolute"));
        assert!(
            refuse(&root.path().join("missing"), &[])
                .0
                .contains("missing")
        );
        assert!(refuse(&program, &words(&["a\nb"])).0.contains("control"));
        assert!(refuse(home, &[]).0.contains("not a file"));
        assert!(!plist_path(home, Job::Subscriber).exists());
    }

    /// A subscriber read installed but not running is read again until it runs, within
    /// launchd's restart window from when it was first seen down, and is stopped past it.
    #[test]
    fn a_subscriber_down_for_launchds_restart_gap_is_not_stopped() {
        use std::cell::Cell;
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let clock = Cell::new(t0);
        let reads = Cell::new(0);
        let read = |back_after: Option<u32>| {
            let reads = &reads;
            move || -> Result<bool, String> {
                reads.set(reads.get() + 1);
                Ok(back_after.is_some_and(|after| reads.get() >= after))
            }
        };
        let mut now = || clock.get();
        let mut pause = |d: Duration| clock.set(clock.get() + d);
        // Back on the third read again: running, and nothing more is read.
        let mut since = None;
        assert_eq!(
            back_within_restart_window(&mut since, &mut now, &mut read(Some(3)), &mut pause),
            Ok(true)
        );
        assert_eq!(reads.get(), 3);
        assert!(clock.get() - t0 < RESTART_WINDOW);
        // Never back: stopped once the window has passed since first seen down.
        reads.set(0);
        clock.set(t0);
        let mut since = None;
        assert_eq!(
            back_within_restart_window(&mut since, &mut now, &mut read(None), &mut pause),
            Ok(false)
        );
        assert_eq!(since, Some(t0), "first seen down");
        assert_eq!(
            clock.get(),
            t0 + RESTART_WINDOW,
            "waited the window, no more"
        );
        let waited = reads.get();
        assert!(waited >= 2, "read again within it: {waited}");
        // A later read still down past the window: stopped at once.
        assert_eq!(
            back_within_restart_window(&mut since, &mut now, &mut read(None), &mut pause),
            Ok(false)
        );
        assert_eq!(reads.get(), waited, "no read, no wait");
        assert_eq!(clock.get(), t0 + RESTART_WINDOW);
        // A read within the window of the first waits only what is left of it.
        clock.set(t0 + RESTART_WINDOW / 2);
        let mut since = Some(t0);
        assert_eq!(
            back_within_restart_window(&mut since, &mut now, &mut read(None), &mut pause),
            Ok(false)
        );
        assert_eq!(clock.get(), t0 + RESTART_WINDOW);
    }
}
