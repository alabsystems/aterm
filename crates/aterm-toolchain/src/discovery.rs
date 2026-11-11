// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Trust toolchain discovery (std-only) shared by `aterm-verify`, `atpkg`, and `aterm-release`.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub use crate::rustup::rustup_home;

/// The shared `aterm.toml` admission budget for Settings, packages and store discovery.
pub const MAX_ATERM_CONFIG_BYTES: usize = 512 * 1024;

/// The seed-first Trust stage2 recreator and seal command shared across remedies.
#[macro_export]
macro_rules! trust_recreate_bootstrap_cmd {
    () => {
        "python3 scripts/recreate_bootstrap.py --stage 2 --require-seed --fresh-seed --no-register \
         (stage0 = the pinned Trust seed; it refuses rather than wrap a stock rustc), then \
         seal it with scripts/promote-toolchain.sh"
    };
}

/// Whether `path` is a runnable file: a regular file with an execute bit.
#[cfg(unix)]
#[must_use]
pub fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

pub(crate) fn is_pending_stub(path: &Path) -> bool {
    use std::io::Read as _;
    const HEAD: u64 = 256;
    const MARKER: &str = "atpkg pending-program stub";
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let Ok(file) = options.open(path) else {
        return false;
    };
    let Ok(meta) = file.metadata() else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    let cap = usize::try_from(meta.len().min(HEAD).saturating_add(1)).unwrap_or(0);
    let mut head = Vec::with_capacity(cap);
    if file.take(HEAD).read_to_end(&mut head).is_err() {
        return false;
    }
    String::from_utf8_lossy(&head).contains(MARKER)
}

/// Whether `path` exists and is a regular file (Windows has no execute permission bit).
#[cfg(not(unix))]
#[must_use]
pub fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.is_file())
        .unwrap_or(false)
}

/// Tool filename with the platform executable suffix (`<name>` on Unix, `<name>.exe` on Windows).
#[must_use]
pub fn tool_file(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

pub(crate) fn strip_unquoted_comment(line: &str) -> &str {
    let mut in_double = false;
    let mut in_single = false;
    let mut escaped = false;
    for (idx, ch) in line.char_indices() {
        if in_double {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_double = false;
            }
        } else if in_single {
            if ch == '\'' {
                in_single = false;
            }
        } else {
            match ch {
                '"' => in_double = true,
                '\'' => in_single = true,
                '#' => return &line[..idx],
                _ => {}
            }
        }
    }
    line
}

pub(crate) fn table_header(line: &str) -> Option<&str> {
    let head = strip_unquoted_comment(line).trim_end();
    if head.starts_with("[[") {
        return Some("");
    }
    head.strip_prefix('[')?.strip_suffix(']').map(str::trim)
}

fn strip_toml_key<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(key)?;
    if !rest.starts_with([' ', '\t', '=']) {
        return None;
    }
    rest.trim_start().strip_prefix('=').map(str::trim_start)
}

fn take_hex_scalar(chars: &mut std::str::Chars<'_>, digits: usize) -> Option<char> {
    let mut code = 0u32;
    for _ in 0..digits {
        code = code
            .checked_mul(16)?
            .checked_add(chars.next()?.to_digit(16)?)?;
    }
    char::from_u32(code)
}

pub(crate) fn parse_toml_string(rhs: &str) -> Option<String> {
    let (value, rest) = take_toml_string(rhs)?;
    let tail = rest.trim_start();
    (tail.is_empty() || tail.starts_with('#')).then_some(value)
}

/// One single-line TOML string at the head of `rhs`, and the text after its closing quote.
fn take_toml_string(rhs: &str) -> Option<(String, &str)> {
    if !rhs.starts_with("'''")
        && let Some(body) = rhs.strip_prefix('\'')
    {
        let (value, rest) = body.split_once('\'')?;
        if value
            .chars()
            .any(|ch| ch == '\x7F' || (ch.is_control() && ch != '\t'))
        {
            return None;
        }
        return Some((value.to_string(), rest));
    }
    if rhs.starts_with("\"\"\"") {
        return None;
    }
    let body = rhs.strip_prefix('"')?;
    let mut chars = body.chars();
    let mut out = String::new();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => return Some((out, chars.as_str())),
            '\\' => match chars.next()? {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                'b' => out.push('\u{0008}'),
                'f' => out.push('\u{000C}'),
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                'u' => out.push(take_hex_scalar(&mut chars, 4)?),
                'U' => out.push(take_hex_scalar(&mut chars, 8)?),
                _ => return None,
            },
            ch if ch == '\x7F' || (ch.is_control() && ch != '\t') => return None,
            ch => out.push(ch),
        }
    }
    None
}

/// A TOML array of single-line strings at the head of `rhs` (it may run over several lines, with
/// comments between elements), or `None` when it is anything else.
fn parse_toml_string_array(rhs: &str) -> Option<Vec<String>> {
    let mut rest = rhs.strip_prefix('[')?;
    let mut out = Vec::new();
    loop {
        rest = rest.trim_start();
        if let Some(comment) = rest.strip_prefix('#') {
            rest = comment.split_once('\n').map_or("", |(_, next)| next);
            continue;
        }
        if rest.starts_with(']') {
            return Some(out);
        }
        let (value, after) = take_toml_string(rest)?;
        out.push(value);
        rest = after.trim_start();
        if let Some(next) = rest.strip_prefix(',') {
            rest = next;
        } else if !rest.starts_with(']') && !rest.starts_with('#') {
            return None;
        }
    }
}

/// Extract the `channel = "..."` value from `rust-toolchain.toml` text (under `[toolchain]` or before any table header).
#[must_use]
pub fn pinned_channel_in(toml: &str) -> Option<String> {
    let mut in_toolchain = true;
    for l in toml.lines().map(str::trim) {
        if l.starts_with('#') {
            continue;
        }
        if l.starts_with('[') {
            in_toolchain = table_header(l) == Some("toolchain");
            continue;
        }
        if !in_toolchain {
            continue;
        }
        let Some(rhs) = strip_toml_key(l, "channel") else {
            continue;
        };
        return parse_toml_string(rhs);
    }
    None
}

pub(crate) fn read_bounded_text(path: &Path, max_bytes: usize) -> Option<String> {
    use std::io::Read as _;
    let sentinel = u64::try_from(max_bytes).ok()?.checked_add(1)?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = options.open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > u64::try_from(max_bytes).ok()? {
        return None;
    }
    let cap = usize::try_from(meta.len())
        .unwrap_or(0)
        .min(max_bytes)
        .saturating_add(1);
    let mut buf = Vec::with_capacity(cap);
    file.take(sentinel).read_to_end(&mut buf).ok()?;
    if buf.len() > max_bytes {
        return None;
    }
    String::from_utf8(buf).ok()
}

/// [`pinned_channel_in`] over `<root>/rust-toolchain.toml`.
#[must_use]
pub fn pinned_channel(root: &Path) -> Option<String> {
    read_bounded_text(&root.join("rust-toolchain.toml"), 64 << 10)
        .as_deref()
        .and_then(pinned_channel_in)
}

fn is_upstream_channel(channel: &str) -> bool {
    matches!(channel, "" | "stable" | "beta" | "nightly")
        || channel.starts_with("nightly-")
        || channel.starts_with("1.")
}

fn is_pinned_toolchain(dir: &Path, pinned: Option<&str>, probe: &mut Probe<'_>) -> bool {
    let channel = match pinned {
        None => return true,
        Some(channel) if is_upstream_channel(channel) => return true,
        Some(channel) => channel,
    };
    pinned_sysroot(dir, channel, probe).is_some()
}

fn pinned_sysroot(dir: &Path, channel: &str, probe: &mut Probe<'_>) -> Option<PathBuf> {
    let driver = [format!("{channel}c"), channel.to_string()]
        .into_iter()
        .map(|n| dir.join(tool_file(&n)))
        .find(|p| is_executable_file(p) && !is_pending_stub(p))?;
    let sysroot = probe.stdout(&driver, &["--print", "sysroot"], probe_bound())?;
    let sysroot = sysroot.trim();
    (!sysroot.is_empty() && Path::new(sysroot).join("lib/rustlib").is_dir())
        .then(|| PathBuf::from(sysroot))
}

/// Why the locked build in `dir` is not the compiler `found` names: its `trustc -vV` reports no
/// `commit-hash`, or another than the lock's `compiler_commit`. `None` when they agree. The tree
/// root already names the build's bytes; this holds the lock's own two facts to each other, so a
/// lock whose commit was never that build's compiles nothing.
fn locked_commit_refusal(
    dir: &Path,
    found: &crate::trust_lock::Found,
    locked: &crate::trust_lock::TrustLock,
    probe: &mut Probe<'_>,
) -> Option<String> {
    let trustc = dir.join(tool_file("trustc"));
    let reported = probe
        .stdout(&trustc, &["-vV"], probe_bound())
        .and_then(|text| crate::trust_lock::commit_hash_line(&text));
    if reported.as_deref() == Some(locked.compiler_commit.as_str()) {
        return None;
    }
    Some(format!(
        "{} names Trust build {} with compiler commit {}, and that build's trustc reports {}:          the lock does not describe its build. Fix: rewrite it with `atpkg-publish adopt trust={}`",
        found.path.display(),
        locked.build,
        locked.compiler_commit,
        reported.map_or_else(|| "no commit".to_string(), |c| format!("commit {c}")),
        locked.build
    ))
}

/// Resolve the `atpkg` install prefix from `[packages].prefix` in `aterm.toml`.
#[must_use]
pub fn atpkg_prefix(home: &Path, xdg_config_home: Option<&Path>) -> PathBuf {
    if let Some(xdg) = xdg_config_home.filter(|d| !d.as_os_str().is_empty()) {
        if let Some(text) = read_bounded_text(&xdg.join("aterm/aterm.toml"), MAX_ATERM_CONFIG_BYTES)
            && let Some(configured) = configured_prefix_in(&text, home)
        {
            return configured;
        }
        return default_atpkg_prefix(home);
    }
    #[cfg(windows)]
    if let Some(text) = read_bounded_text(
        &home
            .join("AppData")
            .join("Roaming")
            .join("aterm")
            .join("aterm.toml"),
        MAX_ATERM_CONFIG_BYTES,
    ) {
        return configured_prefix_in(&text, home).unwrap_or_else(|| default_atpkg_prefix(home));
    }
    if let Some(text) = read_bounded_text(
        &home.join(".config/aterm/aterm.toml"),
        MAX_ATERM_CONFIG_BYTES,
    ) && let Some(configured) = configured_prefix_in(&text, home)
    {
        return configured;
    }
    default_atpkg_prefix(home)
}

/// Platform default `atpkg` install prefix under `home`.
#[must_use]
pub fn default_atpkg_prefix(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/aterm/pkg")
    } else if cfg!(windows) {
        home.join("AppData").join("Local").join("aterm").join("pkg")
    } else {
        home.join(".local/share/aterm/pkg")
    }
}

/// Parse `[packages].prefix` from `aterm.toml` text, expanding `~` against `home`.
#[must_use]
pub fn configured_prefix_in(toml: &str, home: &Path) -> Option<PathBuf> {
    let mut in_packages = false;
    for line in toml.lines().map(str::trim) {
        if line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            in_packages = table_header(line) == Some("packages");
            continue;
        }
        if !in_packages {
            continue;
        }
        let Some(rhs) = strip_toml_key(line, "prefix") else {
            continue;
        };
        let raw = parse_toml_string(rhs)?;
        let value = raw.trim();
        if value.is_empty() {
            return None;
        }
        if value == "~" {
            return Some(home.to_path_buf());
        }
        if let Some(tail) = value.strip_prefix("~/") {
            return Some(home.join(tail));
        }
        return Path::new(value).is_absolute().then(|| PathBuf::from(value));
    }
    None
}

/// `<prefix>/store/trust/current/bin`: the build a tree with no `trust.lock` compiles with.
#[must_use]
pub fn store_stage2_bin(prefix: &Path) -> PathBuf {
    prefix
        .join(crate::layout::STORE_DIR)
        .join(STORE_CHANNEL)
        .join("current/bin")
}

const STORE_CHANNEL: &str = "trust";

/// The `bin` holding the build atpkg's shim for `tool` runs in a tree: the directory of `tool` in
/// the checkout `<prefix>/links/trust` dev-links (`aterm pkg which targo` says `dev-linked`),
/// when the marker links `tool` (one of its `bins`) from an absolute checkout outside the prefix
/// that has a `lib`; else the build the tree's `trust.lock` names (`lock`,
/// [`crate::trust_lock::build_dir`], whether or not the store holds it); else
/// [`store_stage2_bin`]. A dev link moves the shims of the bins it names and pauses the store's
/// updates, leaving `current` on a build those shims no longer run, so discovery reads this
/// rather than `current` alone.
#[must_use]
pub fn answering_bin(
    prefix: &Path,
    tool: &str,
    lock: Option<&crate::trust_lock::TrustLock>,
) -> PathBuf {
    linked_tool_bin(prefix, tool).unwrap_or_else(|| match lock {
        Some(lock) => crate::trust_lock::build_dir(prefix, lock.build).join("bin"),
        None => store_stage2_bin(prefix),
    })
}

/// The `bin` of `tool` in the checkout `<prefix>/links/trust` dev-links, if it links `tool`.
#[must_use]
pub fn linked_tool_bin(prefix: &Path, tool: &str) -> Option<PathBuf> {
    let marker = read_bounded_text(&prefix.join("links").join(STORE_CHANNEL), 64 << 10)?;
    let mut checkout = None;
    let mut bins = None;
    let mut lines = marker.lines();
    while let Some(line) = lines.next() {
        let line = line.trim();
        if let Some(rhs) = strip_toml_key(line, "checkout") {
            checkout = parse_toml_string(rhs).filter(|value| !value.is_empty());
        } else if let Some(rhs) = strip_toml_key(line, "bins") {
            let mut text = rhs.to_string();
            for more in lines.clone() {
                text.push('\n');
                text.push_str(more);
            }
            bins = parse_toml_string_array(&text);
        }
    }
    let checkout = PathBuf::from(checkout?);
    let file = tool_file(tool);
    let rel = bins?.into_iter().map(PathBuf::from).find(|rel| {
        rel.file_name() == Some(OsStr::new(&file))
            && rel
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_)))
    })?;
    let inside_prefix = std::fs::canonicalize(&checkout).is_ok_and(|c| {
        c.starts_with(std::fs::canonicalize(prefix).unwrap_or_else(|_| prefix.into()))
    });
    let dir = checkout.join(rel).parent()?.to_path_buf();
    (checkout.is_absolute() && !inside_prefix && dir.is_dir() && checkout.join("lib").is_dir())
        .then_some(dir)
}

/// A runnable compiler must answer its identity probes; scheduling delay is not absence.
const DATE_PROBE_BOUND: Duration = Duration::from_secs(60);

/// The hang detector shared by every compiler identity probe.
pub(crate) fn probe_bound() -> Duration {
    DATE_PROBE_BOUND
}

/// A rustup `trust` toolchain ranked below the `atpkg` store because its `commit-date` is older
/// than the store build's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Demoted {
    /// The rustup entry's physical `bin` directory.
    pub dir: PathBuf,
    /// Its compiler's `commit-date`.
    pub its: String,
    /// The store build's `commit-date`.
    pub store: String,
    /// True when the rustup toolchain entry is a symlink (`aterm pkg repair` can re-point it).
    pub linked: bool,
}

const REMEDY_LINK: &str = "`aterm pkg repair` re-points it at the store";

macro_rules! detach_fix {
    () => {
        "remove that entry yourself (e.g. `rustup toolchain uninstall trust`), then \
         `aterm pkg repair`"
    };
}

/// Remediation message when a non-symlink toolchain directory blocks `atpkg` attach.
pub const DETACH_FIX: &str = detach_fix!();

/// Remediation message when a demoted rustup entry is a real directory.
pub const REMEDY_DIRECTORY: &str = concat!(
    "`aterm pkg repair` will not replace a directory it did not lay: ",
    detach_fix!()
);

impl Demoted {
    /// Remediation command for this demoted entry based on whether it is a symlink.
    #[must_use]
    pub const fn remedy(&self) -> &'static str {
        if self.linked {
            REMEDY_LINK
        } else {
            REMEDY_DIRECTORY
        }
    }

    /// Human-readable diagnostic explaining why this rustup entry was demoted.
    #[must_use]
    pub fn sentence(&self) -> String {
        format!(
            "rustup `trust` ({}) is a toolchain from {}, older than the atpkg store's {}: \
             ranked below the store, not used — {}",
            self.dir.display(),
            self.its,
            self.store,
            self.remedy()
        )
    }
}

fn commit_date(bin: &Path, probe: &mut Probe<'_>) -> Option<String> {
    ["trustc", "rustc"]
        .iter()
        .map(|n| bin.join(tool_file(n)))
        .filter(|p| is_executable_file(p) && !is_pending_stub(p))
        .find_map(|driver| {
            let text = probe.stdout(&driver, &["-vV"], probe_bound())?;
            let date = text
                .lines()
                .find_map(|l| l.strip_prefix("commit-date: "))?
                .trim()
                .to_string();
            let shaped = date.len() == 10
                && date.bytes().enumerate().all(|(i, b)| {
                    if i == 4 || i == 7 {
                        b == b'-'
                    } else {
                        b.is_ascii_digit()
                    }
                });
            shaped.then_some(date)
        })
}

struct Probe<'a> {
    before: &'a mut dyn FnMut(&Path) -> Result<(), String>,
    error: Option<String>,
}

impl Probe<'_> {
    fn stdout(&mut self, program: &Path, args: &[&str], bound: Duration) -> Option<String> {
        if self.error.is_some() {
            return None;
        }
        let executable = std::fs::canonicalize(program).ok()?;
        let directory = executable.parent()?;
        if let Err(error) = (self.before)(directory) {
            self.error = Some(error);
            return None;
        }
        bounded_stdout(&executable, args, bound)
    }
}

pub(crate) fn bounded_stdout(program: &Path, args: &[&str], bound: Duration) -> Option<String> {
    bounded_stdout_at(program, args, bound, std::time::Instant::now())
}

fn bounded_stdout_at(
    program: &Path,
    args: &[&str],
    bound: Duration,
    started: std::time::Instant,
) -> Option<String> {
    use std::io::Read as _;
    use std::process::Stdio;
    const MAX_STDOUT: u64 = 64 << 10;
    if started.elapsed() >= bound {
        return None;
    }
    #[expect(
        clippy::disallowed_types,
        reason = "a std-only probe: this crate takes no dependency, so it cannot reach aterm-uds"
    )]
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let pipe = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let read = pipe
            .take(MAX_STDOUT.saturating_add(1))
            .read_to_end(&mut buf);
        let _ = tx.send(
            read.ok()
                .filter(|_| (buf.len() as u64) <= MAX_STDOUT)
                .and_then(|_| String::from_utf8(buf).ok()),
        );
    });
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < bound => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let rem = bound
        .saturating_sub(started.elapsed())
        .max(Duration::from_millis(10));
    let out = rx.recv_timeout(rem).ok().flatten()?;
    let _ = reader.join();
    status.success().then_some(out)
}

fn older_than_store(
    rustup: &Path,
    store_prefix: &Path,
    probe: &mut Probe<'_>,
) -> Option<(String, String)> {
    if std::fs::symlink_metadata(store_prefix.join("links").join(STORE_CHANNEL)).is_ok() {
        return None;
    }
    let store = store_stage2_bin(store_prefix);
    std::fs::metadata(&store).ok()?;
    let its = commit_date(rustup, probe)?;
    let theirs = commit_date(&store, probe)?;
    (its < theirs).then_some((its, theirs))
}

fn view_presents(entry: &Path, store_prefix: &Path) -> Option<PathBuf> {
    let resolved = std::fs::canonicalize(entry).ok()?;
    let views = std::fs::canonicalize(store_prefix.join(crate::layout::VIEWS_DIR)).ok()?;
    if !resolved.starts_with(&views) {
        return None;
    }
    Some(answering_bin(store_prefix, "targo", None))
}

/// Resolved Trust toolchain paths and discovery diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toolchain {
    /// The resolved (physical) stage2 `bin` directory.
    pub stage2_dir: PathBuf,
    /// Path to the `targo` binary in `stage2_dir`.
    pub targo: PathBuf,
    /// Path to the `trustdoc` binary in `stage2_dir`.
    pub trustdoc: PathBuf,
    /// `targo-tippy` binary path, if found in the toolchain or borrowed from the managed build.
    pub tippy: Option<PathBuf>,
    /// `trustfmt` binary path, if found in the toolchain or borrowed from the managed build.
    pub trustfmt: Option<PathBuf>,
    /// Formatter/lint tools (`trustfmt`, `targo-tippy`) borrowed from the managed Trust build when
    /// absent from a non-override toolchain directory.
    pub borrowed: Vec<(&'static str, PathBuf)>,
    /// First candidate directory containing `targo` that failed the pinned-toolchain check.
    pub refused: Option<PathBuf>,
    /// The build atpkg's `targo` shim runs ([`answering_bin`]: a dev-linked checkout's `bin`, else
    /// `<prefix>/store/trust/current/bin`); `None` when `$TRUST_STAGE2_BIN` is set.
    pub store_bin: Option<PathBuf>,
    /// Rustup `trust` entry demoted below the store when another candidate won.
    pub demoted: Option<Demoted>,
    /// The `trust.lock` of the tree this toolchain was resolved for ([`crate::trust_lock::find`]),
    /// if it has one: a locked tree resolves the build the lock names, by number and tree root,
    /// and never the store's `current`, the rustup view or `PATH`.
    pub lock: Option<crate::trust_lock::Found>,
    /// Why the tree's lock resolves no toolchain here: it does not read, no store holds its build,
    /// or the store holds that build number with another tree. `None` when it resolved, or when
    /// there is no lock.
    pub lock_refusal: Option<String>,
    /// What took the lock's place, said: `TRUST_STAGE2_BIN` or a dev-linked checkout.
    pub lock_override: Option<String>,
}

impl Toolchain {
    /// Discover the toolchain using environment `XDG_CONFIG_HOME` and `RUSTUP_HOME`, for a tree
    /// whose `trust.lock` is `lock` ([`crate::trust_lock::find`]; `None`: the tree has none).
    #[must_use]
    pub fn discover(
        stage2_bin: Option<&Path>,
        home: &Path,
        path_env: &OsStr,
        pinned: Option<&str>,
        lock: Option<&crate::trust_lock::Found>,
    ) -> Self {
        Self::discover_guarded(stage2_bin, home, path_env, pinned, lock, &mut |_| Ok(()))
            .expect("unguarded discovery cannot refuse a probe")
    }

    /// Discover with a guard before every compiler probe, using the ordinary environment resolution.
    /// The guard receives the canonical directory containing the executable to be probed.
    /// # Errors
    /// The first guard refusal; no compiler probe executes after a refusal.
    pub fn discover_guarded(
        stage2_bin: Option<&Path>,
        home: &Path,
        path_env: &OsStr,
        pinned: Option<&str>,
        lock: Option<&crate::trust_lock::Found>,
        before_probe: &mut dyn FnMut(&Path) -> Result<(), String>,
    ) -> Result<Self, String> {
        let xdg = {
            let explicit = std::env::var_os("XDG_CONFIG_HOME").filter(|d| !d.is_empty());
            #[cfg(windows)]
            let explicit =
                explicit.or_else(|| std::env::var_os("APPDATA").filter(|d| !d.is_empty()));
            explicit.map(PathBuf::from)
        };
        let prefix = atpkg_prefix(home, xdg.as_deref());
        let rustup = rustup_home(std::env::var_os("RUSTUP_HOME").as_deref(), home);
        Self::discover_with_store_guarded(
            stage2_bin,
            &rustup,
            Some(&prefix),
            path_env,
            pinned,
            lock,
            before_probe,
        )
    }

    /// Discover the toolchain across `stage2_bin`, `rustup_home`, `store_prefix`, and `path_env`,
    /// for a tree whose `trust.lock` is `lock` (`None`: the tree has none).
    ///
    /// The order: an explicit `stage2_bin` (`TRUST_STAGE2_BIN`); a dev-linked checkout
    /// ([`linked_tool_bin`]); for a locked tree, the build its lock names, whose recorded tree
    /// root must be the lock's ([`crate::trust_lock::locked_build`]), and nothing else; for a tree
    /// with no lock, the rustup view, the store's `current` and `PATH`, as before locks.
    #[must_use]
    pub fn discover_with_store(
        stage2_bin: Option<&Path>,
        rustup_home: &Path,
        store_prefix: Option<&Path>,
        path_env: &OsStr,
        pinned: Option<&str>,
        lock: Option<&crate::trust_lock::Found>,
    ) -> Self {
        Self::discover_with_store_guarded(
            stage2_bin,
            rustup_home,
            store_prefix,
            path_env,
            pinned,
            lock,
            &mut |_| Ok(()),
        )
        .expect("unguarded discovery cannot refuse a probe")
    }

    /// Discover with a guard before compiler execution, including version comparisons and borrowed tools.
    /// The guard retains any protection it acquires for the canonical executable directory.
    /// # Errors
    /// The first guard refusal; it is never converted into a successful fallback selection.
    pub fn discover_with_store_guarded(
        stage2_bin: Option<&Path>,
        rustup_home: &Path,
        store_prefix: Option<&Path>,
        path_env: &OsStr,
        pinned: Option<&str>,
        lock: Option<&crate::trust_lock::Found>,
        before_probe: &mut dyn FnMut(&Path) -> Result<(), String>,
    ) -> Result<Self, String> {
        let mut probe = Probe {
            before: before_probe,
            error: None,
        };
        let physical = |dir: PathBuf| std::fs::canonicalize(&dir).unwrap_or(dir);
        let targo = tool_file("targo");
        let mut refused = None;
        let mut demoted = None;
        let mut lock_refusal = None;
        let mut lock_override = None;
        // The lock whose `compiler_commit` the resolved build's compiler must report.
        let mut commit_of: Option<&crate::trust_lock::TrustLock> = None;
        let (tool_dir, store_bin) = if let Some(explicit) = stage2_bin {
            let dir = physical(explicit.to_path_buf());
            let driver = dir.join(&targo);
            if is_executable_file(&driver)
                && !is_pending_stub(&driver)
                && !is_pinned_toolchain(&dir, pinned, &mut probe)
            {
                refused = Some(dir.clone());
            }
            if let Some(found) = lock {
                lock_override = Some(format!(
                    "TRUST_STAGE2_BIN={} overrides {}",
                    dir.display(),
                    found.path.display()
                ));
            }
            (dir, None)
        } else if let Some(found) = lock {
            let linked = store_prefix.and_then(|prefix| linked_tool_bin(prefix, "targo"));
            let candidate = match (&found.lock, store_prefix, linked) {
                (lock, _, Some(linked)) => {
                    lock_override = Some(format!(
                        "the dev-linked checkout ({}) overrides {} ({})",
                        linked.display(),
                        found.path.display(),
                        lock.as_ref().map_or_else(
                            |why| format!("which does not read: {why}"),
                            |locked| format!("build {}", locked.build)
                        )
                    ));
                    Some(linked)
                }
                (Err(why), _, None) => {
                    lock_refusal = Some(format!(
                        "{why}: a tree whose trust.lock does not read compiles with no \
                         toolchain. Fix: restore it from git (`git checkout -- {}`), or rewrite \
                         it with `atpkg-publish adopt`",
                        found.path.display()
                    ));
                    None
                }
                (Ok(_), None, None) => {
                    lock_refusal = Some(format!(
                        "{} names a Trust build of the atpkg store, and no store is configured \
                         here",
                        found.path.display()
                    ));
                    None
                }
                (Ok(locked), Some(prefix), None) => {
                    let standing = crate::trust_lock::locked_build(prefix, locked);
                    lock_refusal = standing.refusal(found, locked);
                    match standing {
                        crate::trust_lock::LockedBuild::Present { bin } => {
                            commit_of = Some(locked);
                            Some(bin)
                        }
                        _ => None,
                    }
                }
            };
            let reported = match (&found.lock, store_prefix) {
                (Ok(locked), Some(prefix)) => answering_bin(prefix, "targo", Some(locked)),
                _ => store_prefix.map_or_else(
                    || {
                        rustup_home
                            .join("toolchains")
                            .join(pinned.unwrap_or(STORE_CHANNEL))
                            .join("bin")
                    },
                    store_stage2_bin,
                ),
            };
            let store_bin = Some(reported.clone());
            match candidate {
                Some(dir) => {
                    let dir = physical(dir);
                    let driver = dir.join(&targo);
                    if is_executable_file(&driver)
                        && !is_pending_stub(&driver)
                        && !is_pinned_toolchain(&dir, pinned, &mut probe)
                    {
                        refused = Some(dir.clone());
                    }
                    if let Some(locked) = commit_of.filter(|_| refused.is_none()) {
                        lock_refusal = locked_commit_refusal(&dir, found, locked, &mut probe);
                    }
                    (dir, store_bin)
                }
                None => (reported, store_bin),
            }
        } else {
            let entry = rustup_home
                .join("toolchains")
                .join(pinned.unwrap_or("trust"));
            let rustup = store_prefix
                .and_then(|prefix| view_presents(&entry, prefix))
                .unwrap_or_else(|| entry.join("bin"));
            let store_bin = store_prefix.map(|prefix| answering_bin(prefix, "targo", None));
            let rustup_targo = rustup.join(&targo);
            let stale = store_prefix
                .filter(|_| pinned.unwrap_or(STORE_CHANNEL) == STORE_CHANNEL)
                .filter(|_| is_executable_file(&rustup_targo) && !is_pending_stub(&rustup_targo))
                .and_then(|prefix| older_than_store(&rustup, prefix, &mut probe));
            let head: Vec<PathBuf> = match (&stale, &store_bin) {
                (Some(_), Some(store)) => vec![store.clone(), rustup.clone()],
                _ => std::iter::once(rustup.clone())
                    .chain(store_bin.clone())
                    .collect(),
            };
            let candidates = head.into_iter().chain(std::env::split_paths(path_env));
            let mut chosen = None;
            for dir in candidates {
                let driver = dir.join(&targo);
                if !is_executable_file(&driver) || is_pending_stub(&driver) {
                    continue;
                }
                let dir = physical(dir);
                if is_pinned_toolchain(&dir, pinned, &mut probe) {
                    chosen = Some(dir);
                    refused = None;
                    break;
                }
                refused.get_or_insert(dir);
            }
            let rustup_dir = physical(rustup.clone());
            let linked =
                std::fs::symlink_metadata(&entry).is_ok_and(|m| m.file_type().is_symlink());
            demoted = stale
                .filter(|_| chosen.as_ref().is_some_and(|c| *c != rustup_dir))
                .map(|(its, store)| Demoted {
                    dir: rustup_dir,
                    its,
                    store,
                    linked,
                });
            let reported = chosen.unwrap_or_else(|| store_bin.clone().unwrap_or(rustup));
            (reported, store_bin)
        };
        // An explicit override is never fallen back from, and a locked tree takes every tool from
        // the one build it names: a formatter or lint of another build would judge it.
        let borrows = stage2_bin.is_none() && lock.is_none();
        let mut borrowed = Vec::new();
        let mut tool = |name: &'static str| {
            let own = tool_dir.join(tool_file(name));
            if is_executable_file(&own) && !is_pending_stub(&own) {
                return Some(own);
            }
            if !borrows {
                return None;
            }
            let found = managed_tool_guarded(name, store_prefix, path_env, pinned, &mut probe)?;
            borrowed.push((name, found.clone()));
            Some(found)
        };
        let trustfmt = tool(TRUSTFMT);
        let tippy = tool(TIPPY);
        let found = Self {
            targo: tool_dir.join(targo),
            trustdoc: tool_dir.join(tool_file("trustdoc")),
            tippy,
            trustfmt,
            borrowed,
            stage2_dir: tool_dir,
            refused,
            store_bin,
            demoted,
            lock: lock.cloned(),
            lock_refusal,
            lock_override,
        };
        match probe.error {
            Some(error) => Err(error),
            None => Ok(found),
        }
    }

    /// Path from which `name` was borrowed, or `None` if native to `stage2_dir` or absent.
    #[must_use]
    pub fn borrowed(&self, name: &str) -> Option<&Path> {
        self.borrowed
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, p)| p.as_path())
    }

    /// Summary note naming tools borrowed from the managed Trust build, if any.
    #[must_use]
    pub fn borrowed_note(&self) -> Option<String> {
        if self.borrowed.is_empty() {
            return None;
        }
        let each: Vec<String> = self
            .borrowed
            .iter()
            .map(|(name, file)| format!("{name} from {}", file.display()))
            .collect();
        Some(format!(
            "{} — the managed Trust build's; {} has none",
            each.join(", "),
            self.stage2_dir.display()
        ))
    }

    /// True when an executable `targo` matching the pinned channel was found.
    #[must_use]
    pub fn have_targo(&self) -> bool {
        self.refused.is_none()
            && self.lock_refusal.is_none()
            && is_executable_file(&self.targo)
            && !is_pending_stub(&self.targo)
    }

    /// True when `trustdoc` is an executable file in `stage2_dir`.
    #[must_use]
    pub fn have_trustdoc(&self) -> bool {
        is_executable_file(&self.trustdoc) && !is_pending_stub(&self.trustdoc)
    }

    /// Prepend `stage2_dir` to `inherited` `PATH` when [`Self::have_targo`] is true.
    #[must_use]
    pub fn path_with_stage2_first(&self, inherited: &OsStr) -> OsString {
        if !self.have_targo() {
            return inherited.to_os_string();
        }
        let mut p = OsString::from(self.stage2_dir.as_os_str());
        if !inherited.is_empty() {
            p.push(if cfg!(windows) { ";" } else { ":" });
            p.push(inherited);
        }
        p
    }

    /// Remediation instructions for installing or building the pinned Trust toolchain.
    pub const INSTALL_REMEDY: &'static str = concat!(
        "`aterm pkg install trust` (then `aterm pkg doctor` \
         to confirm the store); from source instead, in a Trust checkout: ",
        crate::trust_recreate_bootstrap_cmd!()
    );

    /// Remediation instructions when an explicit `TRUST_STAGE2_BIN` is invalid or incomplete.
    pub const OVERRIDE_REMEDY: &'static str =
        "unset TRUST_STAGE2_BIN, or point it at a sealed toolchain's bin";

    const fn remedy(&self) -> &'static str {
        if self.store_bin.is_none() {
            Self::OVERRIDE_REMEDY
        } else {
            Self::INSTALL_REMEDY
        }
    }

    /// The diagnostic for no usable toolchain: none found, or one found and refused.
    #[must_use]
    pub fn missing_targo_label(&self) -> String {
        if let Some(why) = &self.lock_refusal {
            return why.clone();
        }
        let explicit = self.store_bin.is_none();
        match (&self.refused, &self.store_bin) {
            (Some(dir), _) if explicit => format!(
                "TRUST_STAGE2_BIN={} is NOT the toolchain rust-toolchain.toml pins (no trustc \
                 there answers with a sysroot). Fix: {}",
                dir.display(),
                Self::OVERRIDE_REMEDY,
            ),
            (Some(dir), _) => format!(
                "targo at {} is NOT the toolchain rust-toolchain.toml pins (no trustc beside it \
                 answers with a sysroot). Fix: {}",
                dir.display(),
                Self::INSTALL_REMEDY,
            ),
            (None, Some(store)) => format!(
                "targo not found — looked in the rustup `trust` toolchain, the atpkg store at \
                 {}, and PATH. Fix: {}; or point TRUST_STAGE2_BIN at a stage2 bin",
                store.display(),
                Self::INSTALL_REMEDY,
            ),
            (None, None) => format!(
                "TRUST_STAGE2_BIN={} holds no targo. Fix: {}",
                self.stage2_dir.display(),
                Self::OVERRIDE_REMEDY,
            ),
        }
    }

    /// Diagnostic message when `trustdoc` is unavailable.
    #[must_use]
    pub fn missing_trustdoc_label(&self) -> String {
        format!(
            "no doc driver: {} is not an executable file and no trustdoc is on PATH, so no doctest ran. Fix: {}",
            self.trustdoc.display(),
            self.remedy(),
        )
    }

    /// Skip reason when `targo-tippy` is unavailable.
    #[must_use]
    pub fn missing_tippy_label(&self) -> String {
        format!("tippy lint ({})", self.missing_tool(TIPPY))
    }

    /// Skip reason when `trustfmt` is unavailable.
    #[must_use]
    pub fn missing_trustfmt_label(&self) -> String {
        format!("{TRUSTFMT} ({})", self.missing_tool(TRUSTFMT))
    }

    fn missing_tool(&self, name: &str) -> String {
        if let Some(found) = self.lock.as_ref().filter(|_| self.lock_override.is_none()) {
            return format!(
                "no {name} in {}, the build {} names; a locked tree takes no tool from another \
                 build",
                self.stage2_dir.display(),
                found.path.display()
            );
        }
        if self.store_bin.is_none() {
            format!(
                "no {name} in TRUST_STAGE2_BIN={}, which is never fallen back from — fix: {}",
                self.stage2_dir.display(),
                Self::OVERRIDE_REMEDY,
            )
        } else {
            format!(
                "no {name} in {}, the atpkg store or PATH — fix: {}",
                self.stage2_dir.display(),
                Self::INSTALL_REMEDY,
            )
        }
    }
}

/// The Trust formatter's file name.
pub const TRUSTFMT: &str = "trustfmt";

/// The Trust lint driver's file name.
pub const TIPPY: &str = "targo-tippy";

#[cfg(all(test, unix))]
fn managed_tool(
    name: &str,
    store_prefix: Option<&Path>,
    path_env: &OsStr,
    pinned: Option<&str>,
) -> Option<PathBuf> {
    managed_tool_guarded(
        name,
        store_prefix,
        path_env,
        pinned,
        &mut Probe {
            before: &mut |_| Ok(()),
            error: None,
        },
    )
}

fn managed_tool_guarded(
    name: &str,
    store_prefix: Option<&Path>,
    path_env: &OsStr,
    pinned: Option<&str>,
    probe: &mut Probe<'_>,
) -> Option<PathBuf> {
    store_prefix
        .map(|prefix| answering_bin(prefix, name, None))
        .into_iter()
        .chain(std::env::split_paths(path_env))
        .find_map(|dir| {
            let tool = dir.join(tool_file(name));
            if !is_executable_file(&tool) || is_pending_stub(&tool) {
                return None;
            }
            let tool = std::fs::canonicalize(&tool).unwrap_or(tool);
            let Some(channel) = pinned.filter(|channel| !is_upstream_channel(channel)) else {
                return Some(tool);
            };
            let sysroot = pinned_sysroot(tool.parent()?, channel, probe)?;
            let physical = std::fs::canonicalize(sysroot.join("bin").join(tool_file(name))).ok()?;
            (is_executable_file(&physical) && !is_pending_stub(&physical)).then_some(physical)
        })
}

#[cfg(test)]
#[allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]
#[path = "discovery_tests.rs"]
mod tests;
