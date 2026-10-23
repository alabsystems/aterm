// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//! What a bare `cargo`/`rustc` is about to run, named by what it is.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use aterm_types::rust_lane;

use crate::reroute::PASSTHROUGH_ENV;

/// The resolution cache, a file under the manager prefix.
pub const CACHE_FILE: &str = "reroute-identity.cache";
/// The first field of every cache line; a line with another (an older schema's) is ignored.
const CACHE_SCHEMA: &str = "v3";
/// A binary's identity ([`CacheKey`]): one entry per canonical path.
const KIND_IDENTITY: &str = "id";
/// rustup's answer ([`which_key`]): one entry per set of inputs.
const KIND_WHICH: &str = "which";
/// Identities kept, newest first: a machine runs a handful of toolchains.
const MAX_IDENTITIES: usize = 32;
/// rustup answers kept, newest first: one per directory and toolchain asked about.
const MAX_WHICH: usize = 64;
/// A cache larger than this is not ours to parse.
const MAX_CACHE_BYTES: usize = 256 * 1024;
/// `src/stage0` is a few KB of `key=value` lines.
const MAX_STAGE0_BYTES: usize = 256 * 1024;
/// rustup's `settings.toml`: a few lines, plus one per directory override.
const MAX_SETTINGS_BYTES: usize = 256 * 1024;

/// What a Rust tool's `-vV` says it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vv {
    /// The first word of the first line: `rustc`, `cargo`.
    pub tool: String,
    /// The `release:` line.
    pub release: String,
    /// The `commit-hash:` line, when it names one.
    pub commit: Option<String>,
    /// The `host:` line.
    pub host: Option<String>,
    /// The `trust:` line — Trust's own version.
    pub trust: Option<String>,
    /// The first other mark of Trust it printed: `trust-repo-commit-hash: e9ca49080f0f`, `binary: trustc`, `(trustc 0.1.0)`.
    pub mark: Option<String>,
}

/// What the file about to run is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Identity {
    /// It answered `-vV` as a Rust tool does ([`parse_vv`]).
    Known(Vv),
    /// It did not: nothing is claimed of it — least of all that it is Trust.
    Unknown {
        /// Why, in a clause: `/x/rustc -vV exited 3`.
        why: String,
    },
}

impl Identity {
    /// Trust by its own `-vV`: a `trust:` line.
    #[must_use]
    pub const fn is_trust(&self) -> bool {
        matches!(self, Self::Known(vv) if vv.trust.is_some())
    }

    /// The `host:` it reported.
    fn host(&self) -> Option<&str> {
        match self {
            Self::Known(vv) => vv.host.as_deref(),
            Self::Unknown { .. } => None,
        }
    }

    /// `Trust 7d5533b2` / `STOCK rustc 1.97.1 (8bab26f4f)` / `cargo 1.99.0-dev (e9ca49080),
    /// which prints `trust-repo-commit-hash: e9ca49080f0f` but no `trust:` version` /
    /// `an unidentified rustc (why)`.
    fn name(&self, tool: &str) -> String {
        match self {
            Self::Known(vv) if vv.trust.is_some() => match &vv.commit {
                Some(commit) => format!("Trust {}", short(commit, 8)),
                None => format!("Trust {}", vv.trust.as_deref().unwrap_or_default()),
            },
            Self::Known(vv) => {
                let version = match &vv.commit {
                    Some(commit) => format!("{} {} ({})", vv.tool, vv.release, short(commit, 9)),
                    None => format!("{} {}", vv.tool, vv.release),
                };
                match &vv.mark {
                    None => format!("STOCK {version}"),
                    Some(mark) => {
                        format!("{version}, which prints `{mark}` but no `trust:` version")
                    }
                }
            }
            Self::Unknown { why } => format!("an unidentified {tool} ({why})"),
        }
    }
}

fn short(s: &str, n: usize) -> &str {
    s.char_indices().nth(n).map_or(s, |(at, _)| &s[..at])
}

/// Why rustup picked the toolchain it did — said only where it is measured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Picked {
    /// `+<toolchain>`, the caller's own.
    CommandLine,
    /// `$RUSTUP_TOOLCHAIN`, which rustup ranks above every file.
    Environment,
    /// A toolchain file up the tree, naming the toolchain rustup picked.
    File(PathBuf),
    /// rustup's `default_toolchain`, with nothing nearer naming another.
    Default,
    /// rustup picked it for a reason this did not measure (a `rustup override`, say).
    Unsaid,
}

/// The toolchain a rustup proxy resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rustup {
    /// Its name under `<rustup home>/toolchains/`: `trust`, `1.97.1-aarch64-apple-darwin`.
    pub toolchain: String,
    /// Why.
    pub picked: Picked,
}

/// What a bare `cargo`/`rustc` runs here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The file that runs, canonical — or, when rustup named none, the proxy on PATH.
    pub real: PathBuf,
    /// The toolchain rustup picked, when the copy on PATH is a rustup proxy that named one.
    pub rustup: Option<Rustup>,
    /// What that file is.
    pub identity: Identity,
}

impl Resolved {
    /// Trust by its own `-vV`.
    #[must_use]
    pub const fn is_trust(&self) -> bool {
        self.identity.is_trust()
    }

    /// What runs, for the first line of the signpost — `tool` is the name typed:
    #[must_use]
    pub fn verdict(&self, tool: &str) -> String {
        let mut out = self.identity.name(tool);
        if let Some(rustup) = &self.rustup {
            out.push_str(", via rustup ");
            out.push_str(&rustup.toolchain);
            match &rustup.picked {
                Picked::CommandLine => out.push_str(" (named on the command line)"),
                Picked::Environment => out.push_str(" (set by $RUSTUP_TOOLCHAIN)"),
                Picked::File(file) => {
                    out.push_str(&format!(" (pinned by {})", file.display()));
                }
                Picked::Default => out.push_str(" (the rustup default)"),
                Picked::Unsaid => {}
            }
        }
        match &self.identity {
            Identity::Known(vv) if vv.trust.is_some() => {}
            Identity::Known(vv) if vv.mark.is_none() => out.push_str("; not Trust"),
            Identity::Known(_) | Identity::Unknown { .. } => {
                out.push_str("; not known to be Trust");
            }
        }
        out
    }
}

/// One question for [`resolve`]: what does `name`, whose first copy on PATH is `target`, run here?
#[derive(Debug, Clone, Copy)]
pub struct Ask<'a> {
    /// `cargo` or `rustc`.
    pub name: &'a str,
    /// The first upstream copy on PATH (`crate::reroute::upstream_on_path`).
    pub target: &'a Path,
    /// The caller's `+<toolchain>`.
    pub toolchain: Option<&'a str>,
    /// The directory the command runs in — every probe runs there too.
    pub cwd: Option<&'a Path>,
    /// `$RUSTUP_TOOLCHAIN`.
    pub env_toolchain: Option<&'a str>,
    /// rustup's home (`crate::seam::rustup_home_with`), for its `default_toolchain`.
    pub rustup_home: Option<&'a Path>,
    /// The resolution cache ([`CACHE_FILE`]) — rustup's answers and binaries' identities; `None` caches nothing.
    pub cache: Option<&'a Path>,
}

/// [`Resolved`] for `ask`: through rustup when the copy on PATH is a proxy, then by the resolved file's `-vV`.
#[must_use]
pub fn resolve(ask: &Ask<'_>) -> Resolved {
    let canonical = std::fs::canonicalize(ask.target).unwrap_or_else(|_| ask.target.to_path_buf());
    let Some(rustup) = rustup_proxy(ask.target, &canonical) else {
        return Resolved {
            identity: identity_of(ask, ask.target, &canonical, ask.toolchain),
            real: canonical,
            rustup: None,
        };
    };
    match cached_rustup_which(&rustup, ask) {
        Err(why) => Resolved {
            real: ask.target.to_path_buf(),
            rustup: None,
            identity: Identity::Unknown { why },
        },
        Ok(which) => {
            let real = std::fs::canonicalize(&which).unwrap_or_else(|_| which.clone());
            let identity = identity_of(ask, &which, &real, None);
            let toolchain = toolchain_name(&which).unwrap_or_else(|| which.display().to_string());
            let picked = picked(ask, &toolchain, identity.host());
            Resolved {
                real,
                rustup: Some(Rustup { toolchain, picked }),
                identity,
            }
        }
    }
}

/// The `rustup` to ask when the copy on PATH at `target` (canonical: `canonical`) is one of
/// rustup's proxies, `None` otherwise.
fn rustup_proxy(target: &Path, canonical: &Path) -> Option<PathBuf> {
    let named_rustup = |path: &Path| {
        path.file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| matches!(name, "rustup" | "rustup-init"))
    };
    if named_rustup(canonical) {
        return Some(canonical.to_path_buf());
    }
    let ours = std::fs::metadata(canonical).ok()?;
    let mut siblings: Vec<PathBuf> = [target.parent(), canonical.parent()]
        .into_iter()
        .flatten()
        .map(|dir| dir.join("rustup"))
        .collect();
    siblings.dedup();
    siblings.into_iter().find(|sibling| {
        let Ok(theirs) = std::fs::metadata(sibling) else {
            return false;
        };
        theirs.is_file()
            && ours.len() == theirs.len()
            && (same_inode(&ours, &theirs)
                || std::fs::read(canonical)
                    .ok()
                    .is_some_and(|bytes| std::fs::read(sibling).ok() == Some(bytes)))
    })
}

#[cfg(unix)]
fn same_inode(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    a.dev() == b.dev() && a.ino() == b.ino()
}

#[cfg(not(unix))]
fn same_inode(_: &std::fs::Metadata, _: &std::fs::Metadata) -> bool {
    false
}

/// [`rustup_which`], from the cache when the same inputs asked it before ([`which_key`])
/// and its answer is still a file; a fresh answer is cached, a failure never is.
fn cached_rustup_which(rustup: &Path, ask: &Ask<'_>) -> Result<PathBuf, String> {
    let key = ask.cache.and_then(|_| which_key(ask, rustup));
    if let (Some(file), Some(key)) = (ask.cache, &key)
        && let Some([answer]) = cache_get(file, KIND_WHICH, key).as_deref()
        && Path::new(answer).is_file()
    {
        return Ok(PathBuf::from(answer));
    }
    let answer = rustup_which(rustup, ask)?;
    if let (Some(file), Some(key), Some(text)) = (ask.cache, &key, answer.to_str()) {
        cache_put(file, KIND_WHICH, key, &[text], MAX_WHICH);
    }
    Ok(answer)
}

/// Everything `rustup which` reads to answer `ask`, as one cache key — `None` (never
/// cached) without a rustup home or a directory, or where a path could not be carried.
fn which_key(ask: &Ask<'_>, rustup: &Path) -> Option<String> {
    let home = ask.rustup_home?;
    let cwd = ask.cwd?;
    let mut parts = vec![
        ask.name.to_string(),
        ask.toolchain.unwrap_or_default().to_string(),
        ask.env_toolchain.unwrap_or_default().to_string(),
        home.to_str()?.to_string(),
        cwd.to_str()?.to_string(),
        stamp_of_path(&home.join("settings.toml")),
        stamp_of_path(&home.join("toolchains")),
        stamp_of_path(rustup),
    ];
    for dir in cwd.ancestors() {
        let files: Vec<String> = ["rust-toolchain", "rust-toolchain.toml"]
            .iter()
            .map(|name| dir.join(name))
            .filter(|file| file.exists())
            .map(|file| format!("{}={}", file.display(), stamp_of_path(&file)))
            .collect();
        if !files.is_empty() {
            parts.extend(files);
            break;
        }
    }
    let key = parts.join("\u{1f}");
    (!key.contains(['\t', '\n'])).then_some(key)
}

/// `rustup which [--toolchain <tc>] <name>`, as the proxy would resolve it here: the
/// absolute file rustup would exec, or why there is none (rustup's own first line).
fn rustup_which(rustup: &Path, ask: &Ask<'_>) -> Result<PathBuf, String> {
    let mut cmd = aterm_uds::spawnfd::command(rustup);
    #[cfg(unix)]
    cmd.arg0("rustup");
    cmd.arg("which");
    if let Some(toolchain) = ask.toolchain {
        cmd.args(["--toolchain", toolchain]);
    }
    cmd.arg(ask.name);
    let which = || match ask.toolchain {
        Some(tc) => format!("`rustup which --toolchain {tc} {}`", ask.name),
        None => format!("`rustup which {}`", ask.name),
    };
    let out = run_probe(&mut cmd, ask.cwd).ok_or_else(|| no_answer(&which()))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(
            match stderr.lines().map(str::trim).find(|l| !l.is_empty()) {
                Some(line) => format!("{}: {line}", which()),
                None => format!("{} failed ({})", which(), out.status),
            },
        );
    }
    let answer = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let path = PathBuf::from(&answer);
    if path.is_absolute() && path.is_file() {
        Ok(path)
    } else {
        Err(format!("{} answered {answer:?}, not a file", which()))
    }
}

/// `trust` for `<rustup home>/toolchains/trust/bin/rustc`; `None` for a path of any other
/// shape (a toolchain file's `path =`).
fn toolchain_name(which: &Path) -> Option<String> {
    let dir = which.parent()?.parent()?;
    (dir.parent()?.file_name()? == "toolchains")
        .then(|| dir.file_name()?.to_str().map(str::to_string))
        .flatten()
}

/// Why rustup picked `toolchain` — in rustup's order, and only what is measured: the
/// caller's `+<toolchain>`, `$RUSTUP_TOOLCHAIN`, a toolchain file up the tree that names
/// it, rustup's default that names it; else [`Picked::Unsaid`] (a directory override
/// outranking the file, say, which this does not read).
fn picked(ask: &Ask<'_>, toolchain: &str, host: Option<&str>) -> Picked {
    if ask.toolchain.is_some() {
        return Picked::CommandLine;
    }
    if ask.env_toolchain.is_some_and(|tc| !tc.is_empty()) {
        return Picked::Environment;
    }
    if let Some((file, channel)) = ask.cwd.and_then(rust_lane::find_pin)
        && names(&channel, toolchain, host)
    {
        return Picked::File(file);
    }
    if ask
        .rustup_home
        .and_then(default_toolchain)
        .is_some_and(|default| names(&default, toolchain, host))
    {
        return Picked::Default;
    }
    Picked::Unsaid
}

/// Whether a channel as written (`1.97.1`, `trust`) names the installed `toolchain`: the
/// same name, or the channel completed with the host triple the compiler reported, as
/// rustup completes it.
fn names(channel: &str, toolchain: &str, host: Option<&str>) -> bool {
    toolchain == channel || host.is_some_and(|host| toolchain == format!("{channel}-{host}"))
}

/// rustup's `default_toolchain` (a top-level key of `<rustup home>/settings.toml`).
fn default_toolchain(rustup_home: &Path) -> Option<String> {
    let text = crate::metadata_io::read_bounded_regular_utf8_follow_final_links(
        &rustup_home.join("settings.toml"),
        MAX_SETTINGS_BYTES,
    )
    .ok()?;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            return None;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() == "default_toolchain" {
            let value = value.trim().trim_matches('"');
            return (!value.is_empty()).then(|| value.to_string());
        }
    }
    None
}

/// The first answer of a Rust tool's `-vV`: `None` when it is not one (no `release:` line).
#[must_use]
pub fn parse_vv(text: &str) -> Option<Vv> {
    let tool = text.lines().next()?.split_whitespace().next()?.to_string();
    let field = |key: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(key))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    Some(Vv {
        tool,
        release: field("release:")?,
        commit: field("commit-hash:").filter(|commit| commit != "unknown"),
        host: field("host:"),
        trust: field("trust:"),
        mark: trust_mark(text),
    })
}

/// The first mark of Trust in a `-vV`, other than a `trust:` line with a version: a
/// `(trustc …)`/`(targo …)` on the first line (the branded entries stamp it), `binary:
/// trustc`, or any key beginning `trust` (`trust-repo-commit-hash:`, which a shipped Trust
/// cargo prints with no `trust:` line, or an empty `trust:`).
fn trust_mark(text: &str) -> Option<String> {
    let first = text.lines().next().unwrap_or_default();
    for brand in ["(trustc", "(targo"] {
        if let Some(at) = first.find(brand) {
            let rest = &first[at..];
            return Some(rest[..rest.find(')').map_or(rest.len(), |end| end + 1)].to_string());
        }
    }
    text.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        let (key, value) = (key.trim(), value.trim());
        let trust_version = key == "trust" && !value.is_empty();
        let marks = (key.to_ascii_lowercase().starts_with("trust") && !trust_version)
            || (key == "binary" && value == "trustc");
        let value = short(value, 12);
        marks.then(|| match value {
            "" => format!("{key}:"),
            _ => format!("{key}: {value}"),
        })
    })
}

/// What the file `exec` (canonical: `real`) is when run as `ask.name` with `plus` (a
/// `+<toolchain>` it is handed): from the cache when it is a toolchain's own file
/// ([`CacheKey::of`]) handed no `+<toolchain>`, else its `-vV`, asked as it will run and
/// cached only under that key.
fn identity_of(ask: &Ask<'_>, exec: &Path, real: &Path, plus: Option<&str>) -> Identity {
    let key = match (ask.cache, plus) {
        (Some(_), None) => CacheKey::of(real, ask.name),
        _ => None,
    };
    if let (Some(file), Some(key)) = (ask.cache, &key)
        && let Some(vv) = key.lookup(file)
    {
        return Identity::Known(vv);
    }
    let asked = if key.is_some() { real } else { exec };
    let identity = probe_vv(asked, plus, ask.cwd);
    if let (Some(file), Some(key), Identity::Known(vv)) = (ask.cache, &key, &identity) {
        key.store(file, vv);
    }
    identity
}

/// `exec [+<toolchain>] -vV`, bounded.
fn probe_vv(exec: &Path, plus: Option<&str>, cwd: Option<&Path>) -> Identity {
    let mut cmd = aterm_uds::spawnfd::command(exec);
    if let Some(toolchain) = plus {
        cmd.arg(format!("+{toolchain}"));
    }
    cmd.arg("-vV");
    let asked = || match plus {
        Some(toolchain) => format!("{} +{toolchain} -vV", exec.display()),
        None => format!("{} -vV", exec.display()),
    };
    let Some(out) = run_probe(&mut cmd, cwd) else {
        return Identity::Unknown {
            why: no_answer(&asked()),
        };
    };
    if !out.status.success() {
        return Identity::Unknown {
            why: match out.status.code() {
                Some(code) => format!("{} exited {code}", asked()),
                None => format!("{} was killed", asked()),
            },
        };
    }
    match parse_vv(&String::from_utf8_lossy(&out.stdout)) {
        Some(vv) => Identity::Known(vv),
        None => Identity::Unknown {
            why: format!("{} printed no `release:` line", asked()),
        },
    }
}

/// Why a probe that `asked` is not known: no answer inside the bound.
fn no_answer(asked: &str) -> String {
    format!(
        "{asked} did not start, or gave no answer within {} s; a first run on a busy machine \
         can take longer, so run it again",
        crate::prereq::PROBE_TIMEOUT.as_secs()
    )
}

/// `cmd` as every probe runs: in `cwd`, the escape marker set, rustup's auto-install off,
/// stdout and stderr kept, within [`crate::prereq::PROBE_TIMEOUT`].
fn run_probe(
    cmd: &mut aterm_uds::spawnfd::Command,
    cwd: Option<&Path>,
) -> Option<std::process::Output> {
    cmd.env(PASSTHROUGH_ENV, "1")
        .env("RUSTUP_AUTO_INSTALL", "0");
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    crate::doctor::output_bounded_capturing(cmd, crate::prereq::PROBE_TIMEOUT)
}

/// What must be unchanged for a cached identity to still be the file's: the canonical path,
/// and the [`stamp_of`] of the file and of every `librustc_driver*` in `<its dir>/../lib`
/// (a compiler's version is compiled into its driver, which can be rebuilt under an
/// unchanged launcher).
#[derive(Debug, Clone, PartialEq, Eq)]
struct CacheKey {
    path: String,
    stamp: String,
}

impl CacheKey {
    /// The key of the canonical file `real`, run as `name`, only when it is a toolchain's own
    /// file: a Mach-O or ELF binary named `name` (so the `argv[0]` it is asked by is its own),
    /// in a `bin/` whose `../lib` holds at least one `librustc_driver*`.
    fn of(real: &Path, name: &str) -> Option<Self> {
        if real.file_name()? != name {
            return None;
        }
        let bin = real.parent()?;
        if bin.file_name()? != "bin" {
            return None;
        }
        let head = crate::metadata_io::read_head(real, 8).ok()?;
        if !(crate::macho::is_macho_header(&head) || head.starts_with(b"\x7fELF")) {
            return None;
        }
        let path = real.to_str()?.to_string();
        let mut stamp = stamp_of(&std::fs::metadata(real).ok()?)?;
        let mut drivers: Vec<(String, String)> = std::fs::read_dir(bin.parent()?.join("lib"))
            .ok()?
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_str()?.to_string();
                if !name.starts_with("librustc_driver") {
                    return None;
                }
                Some((name, stamp_of(&std::fs::metadata(entry.path()).ok()?)?))
            })
            .collect();
        if drivers.is_empty() {
            return None;
        }
        drivers.sort();
        for (name, driver) in drivers {
            stamp.push(';');
            stamp.push_str(&name);
            stamp.push('=');
            stamp.push_str(&driver);
        }
        let key = Self { path, stamp };
        (!key.path.contains(['\t', '\n']) && !key.stamp.contains(['\t', '\n'])).then_some(key)
    }

    /// The identity cached for this path, when it was cached under this stamp.
    fn lookup(&self, file: &Path) -> Option<Vv> {
        let optional = |s: &str| (!s.is_empty()).then(|| s.to_string());
        match cache_get(file, KIND_IDENTITY, &self.path)?.as_slice() {
            [stamp, tool, release, commit, host, trust, mark]
                if *stamp == self.stamp && !release.is_empty() =>
            {
                Some(Vv {
                    tool: tool.clone(),
                    release: release.clone(),
                    commit: optional(commit),
                    host: optional(host),
                    trust: optional(trust),
                    mark: optional(mark),
                })
            }
            _ => None,
        }
    }

    /// Cache `vv` for this path under this stamp, replacing what the path had.
    fn store(&self, file: &Path, vv: &Vv) {
        cache_put(
            file,
            KIND_IDENTITY,
            &self.path,
            &[
                &self.stamp,
                &vv.tool,
                &vv.release,
                vv.commit.as_deref().unwrap_or_default(),
                vv.host.as_deref().unwrap_or_default(),
                vv.trust.as_deref().unwrap_or_default(),
                vv.mark.as_deref().unwrap_or_default(),
            ],
            MAX_IDENTITIES,
        );
    }
}

/// `dev:ino:len:mtime` of a regular file.
fn stamp_of(meta: &std::fs::Metadata) -> Option<String> {
    meta.is_file().then(|| stamp_any(meta)).flatten()
}

/// [`stamp_any`] of whatever is at `path` (a file, a directory), `-` for nothing there.
fn stamp_of_path(path: &Path) -> String {
    std::fs::metadata(path)
        .ok()
        .and_then(|meta| stamp_any(&meta))
        .unwrap_or_else(|| String::from("-"))
}

/// `dev:ino:len:mtime_s.mtime_ns` — a file's identity and its last change.
#[cfg(unix)]
fn stamp_any(meta: &std::fs::Metadata) -> Option<String> {
    use std::os::unix::fs::MetadataExt as _;
    Some(format!(
        "{}:{}:{}:{}.{}",
        meta.dev(),
        meta.ino(),
        meta.len(),
        meta.mtime(),
        meta.mtime_nsec()
    ))
}

/// No inode to key on: nothing is cached.
#[cfg(not(unix))]
fn stamp_any(_: &std::fs::Metadata) -> Option<String> {
    None
}

/// The values cached under `kind` and `key` — a line `<schema> \t kind \t key \t values…`.
fn cache_get(file: &Path, kind: &str, key: &str) -> Option<Vec<String>> {
    let text = crate::metadata_io::read_bounded_regular_utf8(file, MAX_CACHE_BYTES).ok()?;
    text.lines().find_map(|line| {
        let mut fields = line.split('\t');
        (fields.next() == Some(CACHE_SCHEMA)
            && fields.next() == Some(kind)
            && fields.next() == Some(key))
        .then(|| fields.map(str::to_string).collect())
    })
}

/// Put `kind`/`key`'s line first with `values`, drop the one it replaces, keep `keep` lines
/// of that kind — by rename, so a reader sees the old file or the new one.
fn cache_put(file: &Path, kind: &str, key: &str, values: &[&str], keep: usize) {
    let mut fields = vec![CACHE_SCHEMA, kind, key];
    fields.extend_from_slice(values);
    if fields.iter().any(|field| field.contains(['\t', '\n'])) {
        return;
    }
    let old =
        crate::metadata_io::read_bounded_regular_utf8(file, MAX_CACHE_BYTES).unwrap_or_default();
    let mut lines = vec![fields.join("\t")];
    let mut of_kind = 1;
    for line in old.lines() {
        let mut parts = line.split('\t');
        if parts.next() != Some(CACHE_SCHEMA) {
            continue;
        }
        if parts.next() == Some(kind) {
            if parts.next() == Some(key) || of_kind >= keep {
                continue;
            }
            of_kind += 1;
        }
        lines.push(line.to_string());
    }
    let mut body = lines.join("\n");
    body.push('\n');
    let Some(dir) = file.parent() else {
        return;
    };
    let tmp = dir.join(format!(".{CACHE_FILE}.{}.tmp", std::process::id()));
    if std::fs::write(&tmp, body).is_err() || std::fs::rename(&tmp, file).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// The root of the Trust source checkout `cwd` is in: the nearest directory up the tree holding `x.py` whose `src/stage0` says `compiler_dist_channel=trust`.
#[must_use]
pub fn trust_checkout(cwd: &Path) -> Option<PathBuf> {
    for dir in cwd.ancestors() {
        if !dir.join("x.py").is_file() {
            continue;
        }
        let Ok(stage0) = crate::metadata_io::read_bounded_regular_utf8_follow_final_links(
            &dir.join("src").join("stage0"),
            MAX_STAGE0_BYTES,
        ) else {
            continue;
        };
        return stage0_is_trust(&stage0).then(|| dir.to_path_buf());
    }
    None
}

/// Whether a `src/stage0` names Trust's channel.
fn stage0_is_trust(stage0: &str) -> bool {
    stage0.lines().any(|line| {
        line.split_once('=').is_some_and(|(key, value)| {
            key.trim() == "compiler_dist_channel" && value.trim() == "trust"
        })
    })
}

#[cfg(test)]
#[path = "upstream_identity_tests.rs"]
mod tests;
