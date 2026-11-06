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

#[cfg(not(test))]
const DATE_PROBE_BOUND: Duration = Duration::from_secs(5);
/// A unit test's drivers are stand-ins that print and exit: their answer must come, so the
/// probe waits on the hang detector.
#[cfg(test)]
const DATE_PROBE_BOUND: Duration = aterm_spec::wait::MUST_HAPPEN;

/// The bound a test set on each driver probe, in milliseconds (0: none set).
#[cfg(any(test, debug_assertions))]
static PROBE_BOUND_FOR_TEST_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Bound each driver probe (`--print sysroot`, `-vV`) by `bound` instead of the product's 5 s,
/// for this process.
///
/// Another crate's test whose stand-in drivers print and exit sets the hang detector here: their
/// answer must come, and a loaded machine that outlasts 5 s would otherwise read a pinned
/// toolchain as missing and a borrowed tool as gone.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub fn set_probe_bound_for_test(bound: Duration) {
    let ms = u64::try_from(bound.as_millis()).unwrap_or(u64::MAX).max(1);
    PROBE_BOUND_FOR_TEST_MS.store(ms, std::sync::atomic::Ordering::Relaxed);
}

/// `DATE_PROBE_BOUND`, or the bound a test set with [`set_probe_bound_for_test`].
pub(crate) fn probe_bound() -> Duration {
    #[cfg(any(test, debug_assertions))]
    match PROBE_BOUND_FOR_TEST_MS.load(std::sync::atomic::Ordering::Relaxed) {
        0 => {}
        ms => return Duration::from_millis(ms),
    }
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
    use std::io::Read as _;
    use std::process::Stdio;
    const MAX_STDOUT: u64 = 64 << 10;
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
    let started = std::time::Instant::now();
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
mod tests {
    use super::*;
    use std::fs;

    fn mktemp_dir(tag: &str) -> std::io::Result<PathBuf> {
        Ok(aterm_tempfile::Builder::new()
            .prefix(&format!("{tag}."))
            .tempdir()?
            .keep())
    }

    fn runnable(path: &Path) -> PathBuf {
        let mut named = path.as_os_str().to_os_string();
        named.push(std::env::consts::EXE_SUFFIX);
        PathBuf::from(named)
    }

    fn make_runnable(path: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        #[cfg(not(unix))]
        {
            let _ = path;
        }
    }

    fn exec_stub(path: &Path) {
        let path = &runnable(path);
        fs::write(path, b"#!/bin/sh\nexit 0\n").expect("write");
        make_runnable(path);
    }

    fn driver_stub(path: &Path) {
        let path = &runnable(path);
        let sysroot = path.parent().expect("bin dir").parent().expect("sysroot");
        fs::create_dir_all(sysroot.join("lib/rustlib")).expect("mkdir rustlib");
        #[cfg(windows)]
        fs::copy(std::env::current_exe().expect("this test binary"), path).expect("copy");
        #[cfg(not(windows))]
        fs::write(
            path,
            format!("#!/bin/sh\necho '{}'\n", sysroot.display()).as_bytes(),
        )
        .expect("write");
        make_runnable(path);
    }

    /// CRT initializer so a copied test binary on Windows answers `--print sysroot` before libtest.
    #[cfg(windows)]
    #[used]
    #[unsafe(link_section = ".CRT$XCU")]
    static ANSWER_AS_A_DRIVER: extern "C" fn() = answer_as_a_driver;

    #[cfg(windows)]
    extern "C" fn answer_as_a_driver() {
        use std::io::Write as _;
        let args: Vec<OsString> = std::env::args_os().skip(1).collect();
        if args != [OsStr::new("--print"), OsStr::new("sysroot")] {
            return;
        }
        let Ok(me) = std::env::current_exe() else {
            std::process::exit(1);
        };
        let Some(sysroot) = me.parent().and_then(Path::parent) else {
            std::process::exit(1);
        };
        let mut out = std::io::stdout();
        let wrote = writeln!(out, "{}", sysroot.display()).and_then(|()| out.flush());
        std::process::exit(i32::from(wrote.is_err()));
    }

    #[test]
    fn the_reported_location_when_nothing_resolves_is_the_store() {
        let home = Path::new("/nonexistent-home");
        let prefix = default_atpkg_prefix(home);
        let t = Toolchain::discover_with_store(
            None,
            &rustup_home(None, home),
            Some(&prefix),
            OsStr::new(""),
            None,
            None,
        );
        assert_eq!(t.targo, store_stage2_bin(&prefix).join(tool_file("targo")));
        assert_eq!(
            t.trustdoc,
            store_stage2_bin(&prefix).join(tool_file("trustdoc"))
        );
        assert!(!t.have_targo());
        assert!(t.tippy.is_none());
        let t = Toolchain::discover_with_store(
            None,
            &rustup_home(None, home),
            None,
            OsStr::new(""),
            None,
            None,
        );
        assert_eq!(
            t.targo,
            Path::new("/nonexistent-home/.rustup/toolchains/trust/bin").join(tool_file("targo"))
        );
    }

    /// Pending `atpkg` stubs on `PATH` are skipped in favor of real toolchains.
    #[cfg(unix)]
    #[test]
    fn a_pending_stub_on_path_is_passed_over() {
        let home = mktemp_dir("atv-pending").expect("mktemp");
        let prefix = default_atpkg_prefix(&home);
        let stubs = home.join("pkg-bin");
        fs::create_dir_all(&stubs).expect("mkdir");
        for name in ["targo", "trustc"] {
            fs::write(
                stubs.join(name),
                b"#!/bin/sh\n# atpkg pending-program stub v1\nexit 127\n",
            )
            .expect("write");
            make_runnable(&stubs.join(name));
        }
        let real = pinned_bin(&home.join("real/bin"));
        let path = std::env::join_paths([&stubs, &real]).expect("join");
        for pinned in [None, Some("trust")] {
            let t = Toolchain::discover_with_store(
                None,
                &rustup_home(None, &home),
                Some(&prefix),
                &path,
                pinned,
                None,
            );
            assert_eq!(t.targo, real.join("targo"), "pinned={pinned:?}");
            assert!(t.have_targo(), "pinned={pinned:?}");
            let t = Toolchain::discover_with_store(
                None,
                &rustup_home(None, &home),
                Some(&prefix),
                stubs.as_os_str(),
                pinned,
                None,
            );
            assert_eq!(
                t.targo,
                store_stage2_bin(&prefix).join("targo"),
                "pinned={pinned:?}"
            );
            assert!(!t.have_targo(), "pinned={pinned:?}");
            let explicit = Toolchain::discover_with_store(
                Some(&stubs),
                &rustup_home(None, &home),
                Some(&prefix),
                OsStr::new(""),
                pinned,
                None,
            );
            assert!(!explicit.have_targo(), "explicit pending stub is not targo");
            assert!(
                explicit.refused.is_none(),
                "pending stub is absent, not an impostor"
            );
        }
        let _ = fs::remove_dir_all(&home);
    }

    /// Create a valid pinned toolchain `bin` directory under `root` and return its canonical path.
    #[cfg(unix)]
    fn pinned_bin(root: &Path) -> PathBuf {
        fs::create_dir_all(root).expect("mkdir");
        exec_stub(&root.join("targo"));
        driver_stub(&root.join("trustc"));
        fs::canonicalize(root).expect("canonicalize")
    }

    /// Resolution order: explicit override > rustup > store > PATH (ignoring unprobed build trees).
    #[cfg(unix)]
    #[test]
    fn one_resolution_order_and_no_build_tree() {
        let home = mktemp_dir("atv-order").expect("mktemp");
        let prefix = default_atpkg_prefix(&home);
        let store = pinned_bin(&prefix.join("store/trust/6808/bin"));
        std::os::unix::fs::symlink(
            prefix.join("store/trust/6808"),
            prefix.join("store/trust/current"),
        )
        .expect("ln current");
        let linked = pinned_bin(&home.join("sealed/bin"));
        fs::create_dir_all(home.join(".rustup/toolchains")).expect("mkdir");
        std::os::unix::fs::symlink(home.join("sealed"), home.join(".rustup/toolchains/trust"))
            .expect("ln rustup");
        let on_path = pinned_bin(&home.join("elsewhere/bin"));
        let tree = pinned_bin(&home.join("trust/build/host/stage2/bin"));
        let promoted = pinned_bin(&home.join("toolchains/trust-current/bin"));
        let path = std::env::join_paths([&tree, &promoted, &on_path]).expect("join");
        let found = |explicit: Option<&Path>, path: &OsStr| {
            Toolchain::discover_with_store(
                explicit,
                &rustup_home(None, &home),
                Some(&prefix),
                path,
                Some("trust"),
                None,
            )
        };

        let t = found(Some(&on_path), &path);
        assert_eq!(
            (t.stage2_dir.as_path(), t.store_bin.is_none()),
            (on_path.as_path(), true)
        );
        let t = found(None, &path);
        assert!(t.have_targo());
        assert_eq!(t.stage2_dir, linked);
        assert_eq!(t.store_bin, Some(store_stage2_bin(&prefix)));
        fs::remove_file(home.join(".rustup/toolchains/trust")).expect("rm rustup");
        assert_eq!(found(None, &path).stage2_dir, store);
        fs::remove_file(prefix.join("store/trust/current")).expect("rm current");
        assert_eq!(found(None, &path).stage2_dir, tree);
        let t = found(None, OsStr::new(""));
        assert!(
            !t.have_targo(),
            "a build tree under home is not a candidate: {}",
            t.stage2_dir.display()
        );
        assert_eq!(t.stage2_dir, store_stage2_bin(&prefix));
        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn rustup_home_is_the_variable_when_set_and_home_dot_rustup_otherwise() {
        let home = Path::new("/h");
        assert_eq!(rustup_home(None, home), Path::new("/h/.rustup"));
        assert_eq!(
            rustup_home(Some(OsStr::new("")), home),
            Path::new("/h/.rustup")
        );
        assert_eq!(
            rustup_home(Some(OsStr::new("/opt/rustup")), home),
            Path::new("/opt/rustup")
        );
    }

    /// Discovery resolves the pinned toolchain under a relocated `$RUSTUP_HOME`.
    #[cfg(unix)]
    #[test]
    fn a_relocated_rustup_home_is_where_the_pinned_toolchain_is_found() {
        let home = mktemp_dir("atv-rustup-home").expect("mktemp");
        let relocated = home.join("elsewhere/rustup");
        let linked = pinned_bin(&home.join("sealed/bin"));
        fs::create_dir_all(relocated.join("toolchains")).expect("mkdir");
        std::os::unix::fs::symlink(home.join("sealed"), relocated.join("toolchains/trust"))
            .expect("ln rustup");
        let found = |rustup: &Path| {
            Toolchain::discover_with_store(None, rustup, None, OsStr::new(""), Some("trust"), None)
        };
        let t = found(&rustup_home(Some(relocated.as_os_str()), &home));
        assert!(t.have_targo(), "{}", t.stage2_dir.display());
        assert_eq!(t.stage2_dir, linked);
        let t = found(&rustup_home(None, &home));
        assert!(
            !t.have_targo(),
            "the default home holds no entry: {}",
            t.stage2_dir.display()
        );
        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn a_store_targo_that_is_not_the_pin_is_refused_and_the_search_goes_on() {
        let home = mktemp_dir("atv-storepin").expect("mktemp");
        let prefix = default_atpkg_prefix(&home);
        let store = store_stage2_bin(&prefix);
        fs::create_dir_all(&store).expect("mkdir");
        exec_stub(&store.join("targo"));
        let real = home.join("elsewhere/bin");
        fs::create_dir_all(&real).expect("mkdir");
        exec_stub(&real.join("targo"));
        driver_stub(&real.join("trustc"));

        let t = Toolchain::discover_with_store(
            None,
            &rustup_home(None, &home),
            Some(&prefix),
            OsStr::new(real.to_string_lossy().as_ref()),
            Some("trust"),
            None,
        );
        assert!(t.have_targo(), "PATH still wins past a refused store");
        assert_eq!(t.stage2_dir, fs::canonicalize(&real).expect("canonicalize"));

        let t = Toolchain::discover_with_store(
            None,
            &rustup_home(None, &home),
            Some(&prefix),
            OsStr::new(""),
            Some("trust"),
            None,
        );
        assert!(!t.have_targo(), "fail-closed: the impostor is not adopted");
        assert_eq!(
            t.refused,
            Some(fs::canonicalize(&store).expect("canonicalize"))
        );
        fs::remove_dir_all(&home).ok();
    }

    /// Create a dated toolchain `bin` whose `trustc` answers both `--print sysroot` and `-vV`.
    #[cfg(unix)]
    fn dated_bin(fixture: &Path, sysroot: &Path, date: &str) -> PathBuf {
        let script = fixture.join("dated-trustc");
        if !script.is_file() {
            fs::write(
                &script,
                "#!/bin/sh\nroot=$(cd \"$(dirname \"$0\")/..\" && pwd -P)\n\
                 case \"$1\" in\n  --print) echo \"$root\" ;;\n  \
                 -vV) d=$(cat \"$root/commit-date\"); echo \"rustc 1.99.0-dev (0000000 $d)\"; \
                 echo \"commit-date: $d\" ;;\nesac\n",
            )
            .expect("write");
            make_runnable(&script);
            let _ = std::process::Command::new(&script).output();
        }
        let bin = sysroot.join("bin");
        fs::create_dir_all(sysroot.join("lib/rustlib")).expect("mkdir rustlib");
        fs::create_dir_all(&bin).expect("mkdir bin");
        let _ = fs::remove_file(bin.join("trustc"));
        fs::hard_link(&script, bin.join("trustc")).expect("link trustc");
        if !bin.join("targo").is_file() {
            exec_stub(&bin.join("targo"));
        }
        fs::write(sysroot.join("commit-date"), date).expect("date");
        fs::canonicalize(&bin).expect("canonicalize")
    }

    #[cfg(unix)]
    fn record_dated_probes(fixture: &Path, require_guard: bool) {
        let script = fixture.join("dated-trustc");
        let original = fs::read_to_string(&script).expect("read driver");
        let guard = if require_guard {
            "test -f \"$root/guarded\" || exit 1\n"
        } else {
            ""
        };
        fs::write(
            script,
            original.replace(
                "case \"$1\" in",
                &format!("{guard}echo probed >> \"$root/probes\"\ncase \"$1\" in"),
            ),
        )
        .expect("record probes");
    }

    #[cfg(unix)]
    #[test]
    fn a_probe_guard_refusal_runs_no_compiler_and_cannot_fall_back() {
        let fixture = aterm_tempfile::tempdir().unwrap();
        let root = fixture.path();
        let first = dated_bin(root, &root.join("first"), "2026-09-17");
        let second = dated_bin(root, &root.join("second"), "2026-09-18");
        record_dated_probes(root, false);
        let path = std::env::join_paths([&first, &second]).unwrap();
        let absent_rustup = root.join("no-rustup");
        // Negative control: the same executable really runs without a refusing guard.
        let unguarded =
            Toolchain::discover_with_store(None, &absent_rustup, None, &path, Some("trust"), None);
        assert_eq!(unguarded.stage2_dir, first);
        assert!(root.join("first/probes").is_file());
        for name in ["first/probes", "second/probes"] {
            let _ = fs::remove_file(root.join(name));
        }
        let mut called = Vec::new();
        let refused = Toolchain::discover_with_store_guarded(
            None,
            &absent_rustup,
            None,
            &path,
            Some("trust"),
            None,
            &mut |dir| {
                called.push(dir.to_path_buf());
                Err("lease denied".into())
            },
        );
        assert_eq!(refused.unwrap_err(), "lease denied");
        assert_eq!(called, [first]);
        assert!(!root.join("first/probes").exists());
        assert!(!root.join("second/probes").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_probe_executes_the_guarded_executable_when_its_alias_changes() {
        let fixture = aterm_tempfile::tempdir().unwrap();
        let root = fixture.path();
        let first = dated_bin(root, &root.join("first"), "2026-09-17");
        let second = dated_bin(root, &root.join("second"), "2026-09-18");
        let alias = root.join("compiler-alias");
        std::os::unix::fs::symlink(first.join("trustc"), &alias).unwrap();
        let mut before = |dir: &Path| {
            assert_eq!(dir, first);
            fs::remove_file(&alias).unwrap();
            std::os::unix::fs::symlink(second.join("trustc"), &alias).unwrap();
            Ok(())
        };
        let mut probe = Probe {
            before: &mut before,
            error: None,
        };
        let sysroot = probe
            .stdout(&alias, &["--print", "sysroot"], DATE_PROBE_BOUND)
            .unwrap();
        assert_eq!(sysroot.trim(), first.parent().unwrap().to_str().unwrap());
        assert_eq!(fs::canonicalize(&alias).unwrap(), second.join("trustc"));
    }

    #[cfg(unix)]
    #[test]
    fn a_probe_guard_covers_both_compilers_before_a_staleness_comparison() {
        let fixture = aterm_tempfile::tempdir().unwrap();
        let root = fixture.path();
        let prefix = root.join("pkg");
        let store = dated_bin(root, &prefix.join("store/trust/9192"), "2026-09-17");
        std::os::unix::fs::symlink(
            prefix.join("store/trust/9192"),
            prefix.join("store/trust/current"),
        )
        .unwrap();
        let older = dated_bin(root, &root.join("older"), "2026-08-20");
        let rustup = root.join("rustup");
        fs::create_dir_all(rustup.join("toolchains")).unwrap();
        std::os::unix::fs::symlink(root.join("older"), rustup.join("toolchains/trust")).unwrap();
        record_dated_probes(root, true);
        let mut guarded = std::collections::BTreeSet::new();
        let found = Toolchain::discover_with_store_guarded(
            None,
            &rustup,
            Some(&prefix),
            OsStr::new(""),
            Some("trust"),
            None,
            &mut |dir| {
                guarded.insert(dir.to_path_buf());
                fs::write(dir.parent().unwrap().join("guarded"), "held").unwrap();
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(found.stage2_dir, store);
        assert!(found.demoted.is_some());
        assert_eq!(
            guarded,
            [older.clone(), store.clone()].into_iter().collect()
        );
        assert!(older.parent().unwrap().join("probes").is_file());
        assert!(store.parent().unwrap().join("probes").is_file());
    }

    /// A rustup `trust` older than the store ranks below it and populates `demoted`.
    #[cfg(unix)]
    #[test]
    fn a_rustup_trust_older_than_the_store_ranks_below_it_and_is_named() {
        let home = mktemp_dir("atv-demote").expect("mktemp");
        let prefix = default_atpkg_prefix(&home);
        let store = dated_bin(&home, &prefix.join("store/trust/9192"), "2026-09-17");
        std::os::unix::fs::symlink(
            prefix.join("store/trust/9192"),
            prefix.join("store/trust/current"),
        )
        .expect("ln current");
        let stage2 = home.join("trust/build/aarch64-apple-darwin/stage2");
        let tree = dated_bin(&home, &stage2, "2026-08-20");
        fs::create_dir_all(home.join("trust/build")).expect("mkdir");
        std::os::unix::fs::symlink(
            home.join("trust/build/aarch64-apple-darwin"),
            home.join("trust/build/host"),
        )
        .expect("ln host");
        fs::create_dir_all(home.join(".rustup/toolchains")).expect("mkdir");
        std::os::unix::fs::symlink(
            home.join("trust/build/host/stage2"),
            home.join(".rustup/toolchains/trust"),
        )
        .expect("ln rustup");
        let found = || {
            Toolchain::discover_with_store(
                None,
                &home.join(".rustup"),
                Some(&prefix),
                OsStr::new(""),
                Some("trust"),
                None,
            )
        };

        let t = found();
        assert!(t.have_targo());
        assert_eq!(t.stage2_dir, store, "the older rustup entry must not win");
        let d = t.demoted.clone().expect("the demotion is named");
        assert_eq!(
            (d.dir.as_path(), d.its.as_str(), d.store.as_str()),
            (tree.as_path(), "2026-08-20", "2026-09-17")
        );
        assert!(d.linked, "the measured entry is a link");
        assert!(d.sentence().ends_with(REMEDY_LINK), "{}", d.sentence());

        let entry = home.join(".rustup/toolchains/trust");
        fs::remove_file(&entry).expect("rm the link");
        let entry_bin = dated_bin(&home, &entry, "2026-08-20");
        let t = found();
        assert_eq!(t.stage2_dir, store, "a directory entry is demoted too");
        let d = t.demoted.clone().expect("and named");
        assert_eq!((d.dir.as_path(), d.linked), (entry_bin.as_path(), false));
        let said = d.sentence();
        assert!(
            said.ends_with(REMEDY_DIRECTORY) && !said.contains(REMEDY_LINK),
            "{said}"
        );
        fs::remove_dir_all(&entry).expect("rm the directory");
        std::os::unix::fs::symlink(home.join("trust/build/host/stage2"), &entry)
            .expect("ln rustup again");

        for (date, why) in [
            ("2026-09-18", "newer"),
            ("2026-09-17", "same day"),
            ("unknown", "undated"),
        ] {
            dated_bin(&home, &stage2, date);
            let t = found();
            assert_eq!(t.stage2_dir, tree, "{why}: the rustup entry keeps its rank");
            assert_eq!(t.demoted, None, "{why}");
        }
        dated_bin(&home, &stage2, "2026-08-20");
        fs::create_dir_all(prefix.join("links")).expect("mkdir links");
        fs::write(prefix.join("links/trust"), "a dev-link marker").expect("marker");
        let t = found();
        assert_eq!(
            t.stage2_dir, tree,
            "dev-linked: nothing is stale against the store"
        );
        assert_eq!(t.demoted, None);
        fs::remove_file(prefix.join("links/trust")).expect("rm marker");

        fs::write(prefix.join("store/trust/9192/commit-date"), "2026-09-17").expect("date");
        fs::remove_dir_all(prefix.join("store/trust/9192/lib")).expect("rm sysroot");
        let on_path = pinned_bin(&home.join("elsewhere/bin"));
        let t = Toolchain::discover_with_store(
            None,
            &home.join(".rustup"),
            Some(&prefix),
            on_path.as_os_str(),
            Some("trust"),
            None,
        );
        assert_eq!(
            t.stage2_dir, tree,
            "demoted below the store, never below PATH"
        );
        assert_eq!(t.demoted, None, "it won, so nothing was demoted in effect");
        assert!(t.refused.is_none());
        fs::remove_dir_all(&home).ok();
    }

    /// A dev link answers each tool it links over the store's `current`: discovery reports the
    /// build atpkg's shim for that tool runs (m20, 2026-11-05: with no rustup `trust` entry the
    /// shims ran the linked 9178 while verify compiled and linted with `current`'s 9192), and a
    /// tool the winner lacks is borrowed from the build its own shim runs. NEGATIVE CONTROLS: a
    /// marker that links only `trustc`, names a relative checkout or one without a `lib`, or no
    /// marker at all, and the store's `current` answers.
    #[cfg(unix)]
    #[test]
    fn a_dev_linked_checkout_answers_over_the_stores_current() {
        let home = mktemp_dir("atv-linked").expect("mktemp");
        let prefix = default_atpkg_prefix(&home);
        let store = dated_bin(&home, &prefix.join("store/trust/9192"), "2026-09-18");
        exec_stub(&store.join(TIPPY));
        std::os::unix::fs::symlink(
            prefix.join("store/trust/9192"),
            prefix.join("store/trust/current"),
        )
        .expect("ln current");
        let checkout = home.join("trust-packs/4d9a20e3daf");
        let linked = dated_bin(&home, &checkout, "2026-09-27");
        exec_stub(&linked.join(TIPPY));
        fs::create_dir_all(prefix.join("links")).expect("mkdir links");
        let mark = |at: &str, bins: &str| {
            fs::write(
                prefix.join("links/trust"),
                format!("schema = 1\nprogram = \"trust\"\ncheckout = {at}\nbins = {bins}\n"),
            )
            .expect("marker");
        };
        let at = format!("\"{}\"", checkout.display());
        let every =
            "[\n    \"bin/targo\", # the driver\n    'bin/trustc',\n    \"bin/targo-tippy\",\n]";
        mark(&at, every);
        let found = || {
            Toolchain::discover_with_store(
                None,
                &home.join(".rustup"),
                Some(&prefix),
                OsStr::new(""),
                Some("trust"),
                None,
            )
        };

        let t = found();
        assert!(t.have_targo());
        assert_eq!(t.stage2_dir, linked, "the dev-linked build answers");
        assert_eq!(t.store_bin, Some(checkout.join("bin")));
        assert_eq!(t.tippy, Some(linked.join(TIPPY)), "its own tippy");
        assert!(t.borrowed.is_empty(), "{:?}", t.borrowed);

        let sealed = pinned_bin(&home.join("sealed/bin"));
        fs::create_dir_all(home.join(".rustup/toolchains")).expect("mkdir");
        let entry = home.join(".rustup/toolchains/trust");
        std::os::unix::fs::symlink(home.join("sealed"), &entry).expect("ln rustup");
        let t = found();
        assert_eq!(t.stage2_dir, sealed, "a rustup entry still ranks first");
        assert_eq!(
            t.borrowed(TIPPY),
            Some(linked.join(TIPPY).as_path()),
            "the tippy its shim runs, never the store build the link left behind"
        );
        fs::remove_file(&entry).expect("rm rustup");

        for (at, bins, why) in [
            (at.as_str(), "[\"bin/trustc\"]", "a link of trustc alone"),
            ("\"trust-packs/4d9a20e3daf\"", every, "a relative checkout"),
            (at.as_str(), "\"bin/targo\"", "bins that are no array"),
        ] {
            mark(at, bins);
            let t = found();
            assert_eq!(t.stage2_dir, store, "{why}: the store answers targo");
            assert_eq!(
                t.tippy,
                Some(store.join(TIPPY)),
                "{why}: the store's own tippy"
            );
        }

        mark(&at, every);
        fs::remove_dir_all(checkout.join("lib")).expect("rm lib");
        assert_eq!(
            found().stage2_dir,
            store,
            "a link to no toolchain: the store"
        );

        fs::remove_file(prefix.join("links/trust")).expect("rm marker");
        let t = found();
        assert_eq!(t.stage2_dir, store, "no link: the store's current");
        assert_eq!(
            t.store_bin.as_deref(),
            Some(store_stage2_bin(&prefix).as_path())
        );
        fs::remove_dir_all(&home).ok();
    }

    /// A store build `build` under `prefix` whose `trustc` answers `--print sysroot` and `-vV`
    /// (commit `commit`), with `targo`, `targo-tippy` and `trustfmt`, and its tree root recorded
    /// as `root`; its `bin`.
    #[cfg(unix)]
    fn locked_store_build(prefix: &Path, build: u64, commit: &str, root: &str) -> PathBuf {
        let dir = crate::trust_lock::build_dir(prefix, build);
        let bin = dir.join("bin");
        fs::create_dir_all(dir.join("lib/rustlib")).expect("mkdir rustlib");
        fs::create_dir_all(&bin).expect("mkdir bin");
        for tool in ["targo", TIPPY, TRUSTFMT] {
            exec_stub(&bin.join(tool));
        }
        let trustc = bin.join("trustc");
        fs::write(
            &trustc,
            format!(
                "#!/bin/sh\ncase \"$1\" in -vV) printf 'rustc 1.0\\ncommit-hash: {commit}\\n' ;; \
                 *) echo '{}' ;; esac\n",
                dir.display()
            ),
        )
        .expect("write trustc");
        make_runnable(&trustc);
        crate::trust_lock::write_tree_root_record(&dir, root).expect("record");
        fs::write(crate::trust_lock::ready_marker(prefix, build), "ok\n").expect("ready");
        fs::canonicalize(bin).expect("canonicalize")
    }

    /// A LOCKED TREE RESOLVES THE BUILD ITS LOCK NAMES AND NOTHING ELSE (A14): over a rustup
    /// entry, the store's `current` and `PATH`, all of which answer, it takes the lock's build and
    /// every tool from it; a build the store lacks, holds with another tree, or whose compiler is
    /// another commit resolves nothing (no fallback); `TRUST_STAGE2_BIN` and a dev-linked checkout
    /// take its place, said. NEGATIVE CONTROL: with no lock the rustup entry answers.
    #[cfg(unix)]
    #[test]
    fn a_locked_tree_resolves_its_build_and_nothing_else() {
        let home = mktemp_dir("atv-locked").expect("mktemp");
        let prefix = default_atpkg_prefix(&home);
        let commit = "29ebb403de7a102850d6cb10a62d5cc3baf723ed";
        let root = "4".repeat(64);
        let locked = locked_store_build(&prefix, 16387, commit, &root);
        let current = locked_store_build(&prefix, 16500, &"b".repeat(40), &"5".repeat(64));
        std::os::unix::fs::symlink(
            crate::trust_lock::build_dir(&prefix, 16500),
            prefix.join("store/trust/current"),
        )
        .expect("ln current");
        let sealed = pinned_bin(&home.join("sealed/bin"));
        fs::create_dir_all(home.join(".rustup/toolchains")).expect("mkdir");
        std::os::unix::fs::symlink(home.join("sealed"), home.join(".rustup/toolchains/trust"))
            .expect("ln rustup");
        let on_path = pinned_bin(&home.join("onpath/bin"));
        let tree = home.join("checkout");
        fs::create_dir_all(&tree).expect("mkdir tree");
        let lock = |build: u64, root: &str, commit: &str| {
            let lock = crate::trust_lock::TrustLock {
                build,
                tree_root: root.to_string(),
                compiler_commit: commit.to_string(),
                receipt: "SEED-MINT sha256:00".to_string(),
                stock_toolchain: "1.99.1".to_string(),
            };
            fs::write(
                tree.join(crate::trust_lock::LOCK_FILE),
                crate::trust_lock::render_body(&lock),
            )
            .expect("write lock");
            crate::trust_lock::find(&tree).expect("the lock")
        };
        let found = |stage2: Option<&Path>, lock: Option<&crate::trust_lock::Found>| {
            Toolchain::discover_with_store(
                stage2,
                &home.join(".rustup"),
                Some(&prefix),
                on_path.as_os_str(),
                Some("trust"),
                lock,
            )
        };

        let t = found(None, None);
        assert_eq!(t.stage2_dir, sealed, "no lock: the rustup entry answers");

        let named = lock(16387, &root, commit);
        let t = found(None, Some(&named));
        assert!(t.have_targo(), "{}", t.missing_targo_label());
        assert_eq!(
            t.stage2_dir, locked,
            "the lock's build, over rustup, current and PATH"
        );
        assert_eq!(t.tippy, Some(locked.join(TIPPY)));
        assert_eq!(t.trustfmt, Some(locked.join(TRUSTFMT)));
        assert!(t.borrowed.is_empty(), "{:?}", t.borrowed);
        assert_eq!(
            t.store_bin,
            Some(crate::trust_lock::build_dir(&prefix, 16387).join("bin"))
        );
        assert!(t.lock_override.is_none() && t.lock_refusal.is_none());

        fs::remove_file(locked.join(TRUSTFMT)).expect("rm trustfmt");
        let t = found(None, Some(&named));
        assert_eq!(
            t.trustfmt, None,
            "a locked tree borrows no formatter of another build"
        );
        assert!(t.borrowed.is_empty(), "{:?}", t.borrowed);

        for (named, why, says) in [
            (lock(16999, &root, commit), "absent", "does not hold"),
            (
                lock(16387, &"e".repeat(64), commit),
                "another tree",
                "another build",
            ),
            (
                lock(16387, &root, &"c".repeat(40)),
                "another compiler",
                "reports commit",
            ),
        ] {
            let t = found(None, Some(&named));
            assert!(!t.have_targo(), "{why}");
            let label = t.missing_targo_label();
            assert!(label.contains(says), "{why}: {label}");
            for fallback in [&sealed, &on_path, &current] {
                assert_ne!(&t.stage2_dir, fallback, "{why}: no fallback");
            }
        }

        let named = lock(16387, &root, commit);
        let t = found(Some(&on_path), Some(&named));
        assert_eq!(t.stage2_dir, on_path);
        assert!(t.have_targo());
        assert!(
            t.lock_override
                .as_deref()
                .is_some_and(|o| o.starts_with("TRUST_STAGE2_BIN=")),
            "{:?}",
            t.lock_override
        );

        // A dev-linked checkout takes the place of a lock, readable or not, as the shim's hop
        // runs it.
        let dev = pinned_bin(&home.join("devlink/bin"));
        fs::create_dir_all(prefix.join("links")).expect("mkdir links");
        fs::write(
            prefix.join("links/trust"),
            format!(
                "schema = 1\nprogram = \"trust\"\ncheckout = \"{}\"\nbins = [\"bin/targo\", \
                 \"bin/trustc\"]\n",
                home.join("devlink").display()
            ),
        )
        .expect("marker");
        let unreadable = {
            fs::write(tree.join(crate::trust_lock::LOCK_FILE), "version = 2\n").expect("lock");
            crate::trust_lock::find(&tree).expect("the lock")
        };
        for (named, why) in [(&named, "a lock"), (&unreadable, "an unreadable lock")] {
            let t = found(None, Some(named));
            assert!(t.have_targo(), "{why}: {}", t.missing_targo_label());
            assert_eq!(t.stage2_dir, dev, "{why}");
            assert!(
                t.lock_override
                    .as_deref()
                    .is_some_and(|o| o.starts_with("the dev-linked checkout")),
                "{why}: {:?}",
                t.lock_override
            );
        }
        fs::remove_dir_all(&home).ok();
    }

    /// An `atpkg` rustup view resolves through to the store build or dev-linked checkout it
    /// presents.
    #[cfg(unix)]
    #[test]
    fn atpkgs_view_is_read_through_to_the_build_it_presents() {
        let home = mktemp_dir("atv-view").expect("mktemp");
        let prefix = default_atpkg_prefix(&home);
        let store = dated_bin(&home, &prefix.join("store/trust/9200"), "2026-09-24");
        std::os::unix::fs::symlink(
            prefix.join("store/trust/9200"),
            prefix.join("store/trust/current"),
        )
        .expect("ln current");
        let view = dated_bin(&home, &prefix.join("rustup/trust"), "2026-09-17");
        fs::create_dir_all(home.join(".rustup/toolchains")).expect("mkdir");
        let entry = home.join(".rustup/toolchains/trust");
        std::os::unix::fs::symlink(prefix.join("rustup/trust"), &entry).expect("ln rustup");
        let found = || {
            Toolchain::discover_with_store(
                None,
                &home.join(".rustup"),
                Some(&prefix),
                OsStr::new(""),
                Some("trust"),
                None,
            )
        };
        let t = found();
        assert_eq!(
            t.stage2_dir, store,
            "a lagging view presents the store's build"
        );
        assert_ne!(t.stage2_dir, view, "never the view's own clones");
        assert_eq!(t.demoted, None, "read through, so never demoted");
        dated_bin(&home, &prefix.join("rustup/trust"), "2026-09-24");
        assert_eq!(
            found().stage2_dir,
            store,
            "a current view: the store's own directory"
        );
        let checkout = home.join("trust-checkout");
        let linked = dated_bin(&home, &checkout, "2026-09-20");
        fs::create_dir_all(prefix.join("links")).expect("mkdir links");
        fs::write(
            prefix.join("links/trust"),
            format!(
                "program = \"trust\"\ncheckout = '{}' # literal with comment\nbins = [\"bin/targo\"]\n",
                checkout.display()
            ),
        )
        .expect("marker");
        assert_eq!(
            found().stage2_dir,
            linked,
            "a linked view presents its checkout"
        );
        fs::remove_dir_all(checkout.join("lib")).expect("rm lib");
        assert_eq!(
            found().stage2_dir,
            store,
            "an unpresentable link: the store"
        );
        fs::write(prefix.join("links/trust"), "not a marker").expect("garbage");
        assert_eq!(found().stage2_dir, store, "an unreadable marker is no link");
        fs::remove_file(prefix.join("links/trust")).expect("rm marker");
        let tree = dated_bin(&home, &home.join("stage2"), "2026-09-17");
        fs::remove_file(&entry).expect("rm link");
        std::os::unix::fs::symlink(home.join("stage2"), &entry).expect("ln foreign");
        let d = found().demoted.expect("a foreign older link is said");
        assert_eq!(d.dir, tree);
        assert!(d.sentence().ends_with(REMEDY_LINK));
        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn the_prefix_mirror_honours_a_configured_packages_prefix() {
        let home = Path::new("/fake-home");
        let opt = |rest: &str| {
            if cfg!(windows) {
                format!("C:/opt/{rest}")
            } else {
                format!("/opt/{rest}")
            }
        };
        assert_eq!(
            configured_prefix_in("[packages]\nprefix = \"~/lab/pkg\"\n", home),
            Some(PathBuf::from("/fake-home/lab/pkg"))
        );
        assert_eq!(
            configured_prefix_in(
                "[ packages ] # inline comment\nprefix_note = \"~/decoy\"\nprefix = '~/lab/pkg' # literal\n",
                home
            ),
            Some(PathBuf::from("/fake-home/lab/pkg")),
            "table header comments/whitespace, sibling prefix_* keys, and literal strings must work"
        );
        assert_eq!(
            configured_prefix_in("[packages]\nprefix = \"~/lab/\\u0070kg\"\n", home),
            Some(PathBuf::from("/fake-home/lab/pkg")),
            "basic-string escapes must decode"
        );
        assert_eq!(
            configured_prefix_in("[packages]\nprefix = \"~/lab/pkg\" trailing\n", home),
            None,
            "trailing non-comment tokens after closing quote must fail closed"
        );
        assert_eq!(
            configured_prefix_in("[packages.links]\nprefix = \"~/lab/pkg\"\n", home),
            None,
            "subtables must not match [packages]"
        );
        assert_eq!(
            configured_prefix_in(
                &format!(
                    "[packages]\n# prefix = \"~/decoy\"\nprefix = \"{}\"\n",
                    opt("aterm/pkg")
                ),
                home
            ),
            Some(PathBuf::from(opt("aterm/pkg")))
        );
        assert_eq!(
            configured_prefix_in("[packages]\nprefix = \"relative/pkg\"\n", home),
            None
        );
        assert_eq!(
            configured_prefix_in("[packages]\nprefix = \"~user/pkg\"\n", home),
            None
        );
        assert_eq!(
            configured_prefix_in("[other]\nprefix = \"/opt/x\"\n", home),
            None
        );
        assert_eq!(configured_prefix_in("", home), None);

        let tmp = mktemp_dir("atv-prefix").expect("mktemp");
        assert_eq!(atpkg_prefix(&tmp, None), default_atpkg_prefix(&tmp));
        let xdg = tmp.join("xdg");
        fs::create_dir_all(xdg.join("aterm")).expect("mkdir");
        fs::write(
            xdg.join("aterm/aterm.toml"),
            format!("[packages]\nprefix = \"{}\"\n", opt("lab")),
        )
        .expect("write");
        assert_eq!(atpkg_prefix(&tmp, Some(&xdg)), PathBuf::from(opt("lab")));
        fs::create_dir_all(tmp.join(".config/aterm")).expect("mkdir");
        fs::write(
            tmp.join(".config/aterm/aterm.toml"),
            "[packages]\nprefix = \"~/mine\"\n",
        )
        .expect("write");
        assert_eq!(atpkg_prefix(&tmp, None), tmp.join("mine"));
        assert_eq!(
            store_stage2_bin(&default_atpkg_prefix(Path::new("/h"))),
            Path::new(if cfg!(target_os = "macos") {
                "/h/Library/Application Support/aterm/pkg/store/trust/current/bin"
            } else if cfg!(windows) {
                "/h/AppData/Local/aterm/pkg/store/trust/current/bin"
            } else {
                "/h/.local/share/aterm/pkg/store/trust/current/bin"
            })
        );
        fs::remove_dir_all(&tmp).ok();
    }

    /// Symlinked toolchain directories are canonicalized to their physical path.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_stage2_resolves_to_its_physical_path() {
        let tmp = mktemp_dir("atv-tc").expect("mktemp");
        let real = tmp.join("aarch64-apple-darwin/stage2/bin");
        fs::create_dir_all(&real).expect("mkdir");
        exec_stub(&real.join("targo"));
        std::os::unix::fs::symlink(tmp.join("aarch64-apple-darwin"), tmp.join("host")).expect("ln");

        let via_link = tmp.join("host/stage2/bin");
        let t = Toolchain::discover(
            Some(&via_link),
            Path::new("/unused"),
            OsStr::new(""),
            None,
            None,
        );
        assert!(t.have_targo());
        assert_eq!(t.stage2_dir, fs::canonicalize(&real).expect("canonicalize"));
        assert!(
            !t.stage2_dir.to_string_lossy().contains("/host/"),
            "the symlinked spelling must not survive into the tool paths"
        );
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn tippy_is_found_by_its_trust_name_only() {
        let tmp = mktemp_dir("atv-tippy").expect("mktemp");
        let real = fs::canonicalize(&tmp).expect("canonicalize");
        exec_stub(&tmp.join("targo-clippy"));
        let t = Toolchain::discover(Some(&tmp), Path::new("/unused"), OsStr::new(""), None, None);
        assert_eq!(
            t.tippy, None,
            "the retired pre-rebrand name is not a tippy: the lane is NOT RUN"
        );

        exec_stub(&tmp.join("targo-tippy"));
        let t = Toolchain::discover(Some(&tmp), Path::new("/unused"), OsStr::new(""), None, None);
        assert_eq!(
            t.tippy.as_deref(),
            Some(real.join(tool_file("targo-tippy")).as_path())
        );
        fs::remove_dir_all(&tmp).ok();
    }

    /// A non-override toolchain missing `trustfmt`/`targo-tippy` borrows them from the managed
    /// build.
    #[cfg(unix)]
    #[test]
    fn a_toolchain_lacking_the_formatter_and_lint_takes_the_managed_builds() {
        let home = mktemp_dir("atv-borrow").expect("mktemp");
        let home = fs::canonicalize(&home).expect("canonicalize");
        let prefix = default_atpkg_prefix(&home);
        let store = pinned_bin(&prefix.join("store/trust/9192/bin"));
        for tool in [TRUSTFMT, TIPPY] {
            exec_stub(&store.join(tool));
        }
        std::os::unix::fs::symlink(
            prefix.join("store/trust/9192"),
            prefix.join("store/trust/current"),
        )
        .expect("ln current");
        let stage2 = pinned_bin(&home.join("trust/build/host/stage2/bin"));
        fs::create_dir_all(home.join(".rustup/toolchains")).expect("mkdir");
        std::os::unix::fs::symlink(
            home.join("trust/build/host/stage2"),
            home.join(".rustup/toolchains/trust"),
        )
        .expect("ln rustup");
        let stray = home.join("stray/bin");
        fs::create_dir_all(&stray).expect("mkdir");
        exec_stub(&stray.join(TRUSTFMT));
        let found = |explicit: Option<&Path>, path: &OsStr| {
            Toolchain::discover_with_store(
                explicit,
                &rustup_home(None, &home),
                Some(&prefix),
                path,
                Some("trust"),
                None,
            )
        };
        let t = found(None, OsStr::new(""));
        assert_eq!(
            t.stage2_dir, stage2,
            "the toolchain is still the rustup entry"
        );
        assert!(t.have_targo());
        assert_eq!(t.trustfmt.as_deref(), Some(store.join(TRUSTFMT).as_path()));
        assert_eq!(t.tippy.as_deref(), Some(store.join(TIPPY).as_path()));
        assert_eq!(t.borrowed(TRUSTFMT), Some(store.join(TRUSTFMT).as_path()));
        let note = t.borrowed_note().expect("said");
        assert!(
            note.contains(&format!("trustfmt from {}", store.join(TRUSTFMT).display()))
                && note.contains(&format!("targo-tippy from {}", store.join(TIPPY).display()))
                && note.contains(&format!("{} has none", stage2.display())),
            "{note}"
        );
        let t = found(Some(&stage2), OsStr::new(""));
        assert_eq!(
            (t.stage2_dir.as_path(), t.store_bin.is_none()),
            (stage2.as_path(), true)
        );
        assert_eq!((t.trustfmt.as_deref(), t.tippy.as_deref()), (None, None));
        assert!(t.borrowed.is_empty());
        let skip = t.missing_tippy_label();
        assert!(
            skip.contains("TRUST_STAGE2_BIN=") && skip.contains(Toolchain::OVERRIDE_REMEDY),
            "{skip}"
        );

        let moved = home.join("store-away");
        fs::rename(prefix.join("store"), &moved).expect("move the store away");
        let t = found(None, stray.as_os_str());
        assert_eq!((t.trustfmt.as_deref(), t.tippy.as_deref()), (None, None));
        assert!(t.borrowed.is_empty() && t.borrowed_note().is_none());
        assert!(
            t.missing_trustfmt_label()
                .contains("the atpkg store or PATH")
                && t.missing_tippy_label().contains("the atpkg store or PATH"),
            "the skip names everywhere it looked: {}",
            t.missing_tippy_label()
        );
        fs::rename(&moved, prefix.join("store")).expect("move the store back");
        let current = prefix.join("store/trust/current");
        let hidden = prefix.join("store/trust/hidden-current");
        fs::rename(&current, &hidden).expect("hide current");
        let shims = pinned_bin(&prefix.join("bin"));
        fs::write(
            shims.join("trustc"),
            format!(
                "#!/bin/sh\necho '{}'\n",
                store.parent().expect("sysroot").display()
            ),
        )
        .expect("write the shim's sysroot response");
        exec_stub(&shims.join(TIPPY));
        let path = std::env::join_paths([&stray, &shims]).expect("join");
        for pin in [None, Some("stable")] {
            assert_eq!(
                managed_tool(TIPPY, None, shims.as_os_str(), pin),
                Some(shims.join(TIPPY))
            );
        }
        let t = found(None, &path);
        assert_eq!(
            t.tippy.as_deref(),
            Some(store.join(TIPPY).as_path()),
            "BORROWED_TOOL_MUST_NAME_THE_REPORTED_PHYSICAL_BUILD"
        );
        assert_eq!(t.trustfmt, None, "the stray one is still not taken");
        let parked = store.join("tippy-away");
        fs::rename(store.join(TIPPY), &parked).expect("hide the physical tool");
        assert_eq!(
            found(None, &path).tippy,
            None,
            "a shim alone is not the tool"
        );
        fs::rename(&parked, store.join(TIPPY)).expect("restore the physical tool");
        fs::rename(&hidden, &current).expect("restore current");

        for tool in [TRUSTFMT, TIPPY] {
            exec_stub(&stage2.join(tool));
        }
        let t = found(None, OsStr::new(""));
        assert_eq!(t.trustfmt.as_deref(), Some(stage2.join(TRUSTFMT).as_path()));
        assert_eq!(t.tippy.as_deref(), Some(stage2.join(TIPPY).as_path()));
        assert!(t.borrowed.is_empty() && t.borrowed_note().is_none());
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn path_is_only_rewritten_when_a_targo_is_really_there() {
        let tmp = mktemp_dir("atv-path").expect("mktemp");
        let t = Toolchain::discover(Some(&tmp), Path::new("/unused"), OsStr::new(""), None, None);
        assert_eq!(
            t.path_with_stage2_first(OsStr::new("/usr/bin")),
            OsString::from("/usr/bin")
        );

        exec_stub(&tmp.join("targo"));
        let t = Toolchain::discover(Some(&tmp), Path::new("/unused"), OsStr::new(""), None, None);
        let want =
            std::env::join_paths([t.stage2_dir.as_os_str(), OsStr::new("/usr/bin")]).expect("join");
        assert_eq!(t.path_with_stage2_first(OsStr::new("/usr/bin")), want);
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn the_pinned_channel_is_the_one_line_the_manifest_declares() {
        assert_eq!(
            pinned_channel_in("# channel = \"decoy\"\n[toolchain]\nchannel = \"trust\"\n")
                .as_deref(),
            Some("trust"),
            "a commented-out channel must not be mistaken for the pin"
        );
        assert_eq!(
            pinned_channel_in(
                "[other]\nchannel = \"decoy\"\n[toolchain] # comment\nchannel = \"trust\"\n"
            )
            .as_deref(),
            Some("trust"),
            "a channel key in another table must not be mistaken for [toolchain].channel"
        );
        assert_eq!(
            pinned_channel_in("[other]\nchannel = \"decoy\"\n").as_deref(),
            None
        );
        assert_eq!(
            pinned_channel_in("channel = \"trust\"\n").as_deref(),
            Some("trust")
        );
        assert_eq!(
            pinned_channel_in(
                "[ toolchain ] # comment\nchannels = [\"decoy\"]\nchannel = 'trust' # literal\n"
            )
            .as_deref(),
            Some("trust")
        );
        assert_eq!(
            pinned_channel_in("[toolchain]\nchannel = \"trust\" trailing\n").as_deref(),
            None
        );
        assert_eq!(
            pinned_channel_in("[toolchain]\nchannel = \"\"\"trust\"\"\"\n").as_deref(),
            None
        );
        assert_eq!(pinned_channel_in("[toolchain]\n").as_deref(), None);
        assert_eq!(
            pinned_channel(Path::new("/nonexistent-repo")).as_deref(),
            None
        );
        let tmp = mktemp_dir("atv-pin-bounded").expect("mktemp");
        fs::write(
            tmp.join("rust-toolchain.toml"),
            format!(
                "[toolchain]\nchannel = \"trust\"\n# {}\n",
                "x".repeat(65 << 10)
            ),
        )
        .expect("write");
        assert_eq!(
            pinned_channel(&tmp),
            None,
            "an oversized rust-toolchain.toml is rejected"
        );
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn a_targo_that_is_not_the_pinned_toolchain_is_refused_not_adopted() {
        let tmp = mktemp_dir("atv-pin").expect("mktemp");
        let bare = tmp.join("bare");
        fs::create_dir_all(&bare).expect("mkdir");
        exec_stub(&bare.join("targo"));

        let t = Toolchain::discover(
            Some(&bare),
            Path::new("/unused"),
            OsStr::new(""),
            Some("trust"),
            None,
        );
        assert!(!t.have_targo(), "fail-closed: not the pinned toolchain");
        assert!(t.refused.is_some());

        let half = tmp.join("half");
        fs::create_dir_all(&half).expect("mkdir");
        exec_stub(&half.join("targo"));
        exec_stub(&half.join("trustc"));
        let t = Toolchain::discover(
            Some(&half),
            Path::new("/unused"),
            OsStr::new(""),
            Some("trust"),
            None,
        );
        assert!(
            !t.have_targo(),
            "a driver that names no sysroot is not the pin (mid-rebuild stage tree)"
        );
        assert!(t.refused.is_some());

        let pinned = tmp.join("pinned");
        fs::create_dir_all(&pinned).expect("mkdir");
        exec_stub(&pinned.join("targo"));
        driver_stub(&pinned.join("trustc"));
        let t = Toolchain::discover(
            Some(&pinned),
            Path::new("/unused"),
            OsStr::new(""),
            Some("trust"),
            None,
        );
        assert!(t.have_targo());
        assert!(t.refused.is_none());

        let t = Toolchain::discover(
            Some(&bare),
            Path::new("/unused"),
            OsStr::new(""),
            Some("stable"),
            None,
        );
        assert!(t.have_targo(), "upstream channels are a passthrough");
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn the_path_fallback_adopts_only_the_pinned_toolchain() {
        let tmp = mktemp_dir("atv-pinpath").expect("mktemp");
        let impostor = tmp.join("impostor");
        let real = tmp.join("stage/bin");
        fs::create_dir_all(&impostor).expect("mkdir");
        fs::create_dir_all(&real).expect("mkdir");
        exec_stub(&impostor.join("targo"));
        exec_stub(&real.join("targo"));
        driver_stub(&real.join("trustc"));
        let path = std::env::join_paths([&impostor, &real]).expect("join");
        let home = Path::new("/nonexistent-home");
        let discover = |path: &OsStr| {
            Toolchain::discover_with_store(
                None,
                &rustup_home(None, home),
                Some(&default_atpkg_prefix(home)),
                path,
                Some("trust"),
                None,
            )
        };

        let t = discover(OsStr::new(&path));
        assert!(t.have_targo());
        assert_eq!(t.stage2_dir, fs::canonicalize(&real).expect("canonicalize"));

        let t = discover(OsStr::new(impostor.to_string_lossy().as_ref()));
        assert!(!t.have_targo());
        assert_eq!(
            t.refused,
            Some(fs::canonicalize(&impostor).expect("canonicalize"))
        );
        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn a_directory_named_targo_is_not_a_driver() {
        let tmp = mktemp_dir("atv-dir").expect("mktemp");
        fs::create_dir_all(tmp.join("targo")).expect("mkdir");
        let t = Toolchain::discover(Some(&tmp), Path::new("/unused"), OsStr::new(""), None, None);
        assert!(
            !t.have_targo(),
            "fail-closed: a directory is not the verified driver"
        );
        #[cfg(unix)]
        {
            use std::ffi::CString;
            use std::os::unix::ffi::OsStrExt as _;
            use std::os::unix::fs::PermissionsExt as _;
            let fifo_dir = tmp.join("fifo-bin");
            fs::create_dir_all(&fifo_dir).expect("mkdir fifo-bin");
            let fifo = fifo_dir.join(tool_file("targo"));
            let c_fifo = CString::new(fifo.as_os_str().as_bytes()).expect("cstr");
            // SAFETY: `c_fifo` is a live NUL-terminated path in our private temp directory; `mkfifo` retains no pointer.
            assert_eq!(unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o755) }, 0, "mkfifo");
            fs::set_permissions(&fifo, fs::Permissions::from_mode(0o755)).expect("chmod fifo");
            assert!(!is_executable_file(&fifo));
            assert!(!is_pending_stub(&fifo));
            let t = Toolchain::discover(
                Some(&fifo_dir),
                Path::new("/unused"),
                OsStr::new(""),
                None,
                None,
            );
            assert!(
                !t.have_targo(),
                "fail-closed: an executable FIFO is not a driver and must not hang discovery"
            );
        }
        fs::remove_dir_all(&tmp).ok();
    }

    #[cfg(unix)]
    #[test]
    fn bounded_stdout_drains_large_output_without_pipe_deadlock_and_rejects_oversize() {
        let sh = Path::new("/bin/sh");
        let exact = bounded_stdout(
            sh,
            &[
                "-c",
                "dd if=/dev/zero bs=1024 count=64 2>/dev/null | tr '\\0' x",
            ],
            aterm_spec::wait::MUST_HAPPEN,
        );
        assert_eq!(exact.as_ref().map(String::len), Some(64 << 10));
        let over = bounded_stdout(
            sh,
            &[
                "-c",
                "dd if=/dev/zero bs=1024 count=65 2>/dev/null | tr '\\0' x",
            ],
            aterm_spec::wait::MUST_HAPPEN,
        );
        assert_eq!(over, None);
    }
}
