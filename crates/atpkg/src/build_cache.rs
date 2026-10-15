// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Rust build intermediates out of Spotlight (macOS).
//!
//! A command that builds gets `--config build.build-dir=<cache><workspace>/.cargo-build`
//! when nothing else chose a target or build directory: binaries stay in `target/`, the
//! rest goes under `~/Library/Caches/aterm/cargo-build.noindex`, which Spotlight skips.
//! The `targo`/`targo-tippy` shims decide in `sh` ([`shim_block`]), `cargo` in an aterm
//! tab in Rust ([`with_build_dir`]); the tests hold both to one rule. Nothing is exported,
//! so a nested cargo started by the build never inherits it. [`reclaim`] removes what
//! nobody builds into any more.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// When a workspace is gone: the rule the window's disk watch and `xtask gate cells` use,
/// compiled here from its one source. It never looks into a privacy-guarded folder or a
/// mounted volume.
#[path = "../../aterm-agent/src/harness/disk_checkout.rs"]
pub mod checkout;

/// The cache root, relative to `$HOME`.
pub const CACHE_UNDER_HOME: &str = "Library/Caches/aterm/cargo-build.noindex";

/// The last component of every workspace's build directory under the cache.
pub const LEAF: &str = ".cargo-build";

/// What [`reclaim`] renames a build directory to before removing it, so a build that
/// starts meanwhile gets a fresh folder instead of one being deleted under it.
const DOOMED: &str = ".cargo-build.removing";

/// The locks cargo holds in a profile for a whole build; [`reclaim`] skips a build
/// directory while a build holds one.
const CARGO_LOCKS: &[&str] = &[".cargo-build-lock", ".cargo-lock", ".cargo-artifact-lock"];

/// How deep under a build directory [`reclaim`] reads folders' modification times: a
/// build writes `<profile>/deps`, or `<triple>/<profile>/deps` when it names a target.
const WRITE_DEPTH: usize = 3;

/// The subcommands that get a build directory: those that build, and `clean`, which
/// must see the same layout to remove it.
pub const BUILD_VERBS: &[&str] = &[
    "build", "b", "check", "c", "test", "t", "run", "r", "bench", "doc", "d", "rustc", "rustdoc",
    "clippy", "tippy", "fix", "clean",
];

/// A build directory whose workspace is seen gone goes once unwritten this long.
pub const RECLAIM_GONE: Duration = Duration::from_secs(24 * 60 * 60);

/// Any build directory goes once unwritten this long, whatever its workspace's state
/// (one aterm may not look at, one moved with its parent folder, one simply idle): it is
/// a cache, and a month on the compiler has usually moved and cargo would rebuild anyway.
pub const RECLAIM_IDLE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// `$HOME/Library/Caches/aterm/cargo-build.noindex`.
#[must_use]
pub fn cache_root(home: &Path) -> PathBuf {
    home.join(CACHE_UNDER_HOME)
}

/// The build directory Cargo resolves for the workspace at `workspace` (absolute).
#[must_use]
pub fn build_dir(root: &Path, workspace: &Path) -> PathBuf {
    let mut s = root.as_os_str().to_owned();
    s.push(workspace.as_os_str());
    PathBuf::from(s).join(LEAF)
}

/// The `--config` value both entry points pass, as a TOML literal string. `None` for a
/// root that string cannot hold (a `'` or a control character) or that Cargo would read
/// as a template (a `{` or `}`); the shim refuses the same roots.
#[must_use]
pub fn config_value(root: &Path) -> Option<String> {
    let root = root.to_str()?;
    if root
        .chars()
        .any(|c| matches!(c, '\'' | '{' | '}') || c.is_control())
    {
        return None;
    }
    Some(format!("build.build-dir='{root}{{workspace-root}}/{LEAF}'"))
}

/// The shim's half, rendered ahead of a `targo`/`targo-tippy` shim's `exec` on macOS:
/// shell builtins only, and it only ever puts `--config …` in front of `"$@"`.
const SH_BLOCK: &str = r#"# aterm: build intermediates out of Spotlight
__atb_t=
__atb_c=${HOME-}/Library/Caches/aterm/cargo-build.noindex
__atb=
case ${HOME-} in /?*) case ${PWD-} in "$HOME"/*) __atb=1 ;; esac ;; esac
case ${HOME-} in *[\'[:cntrl:]]*) __atb= ;; esac
case ${HOME-}${PWD-} in *[{}]*) __atb= ;; esac
case ${PWD-} in "$__atb_c" | "$__atb_c"/*) __atb= ;; esac
[ -n "${CARGO_TARGET_DIR+1}${CARGO_BUILD_TARGET_DIR+1}${CARGO_BUILD_BUILD_DIR+1}" ] && __atb=
__atb_scan() {
  for __atb_f in "$1/config.toml" "$1/config"; do
    [ -f "$__atb_f" ] || continue
    [ -r "$__atb_f" ] || { __atb=; return; }
    while IFS= read -r __atb_l || [ -n "$__atb_l" ]; do
      case $__atb_l in *target-dir* | *build-dir*)
        case ${__atb_l#"${__atb_l%%[![:blank:]]*}"} in [#]*) ;; *) __atb=; return ;; esac ;;
      esac
    done < "$__atb_f"
  done
}
__atb_ok() {
  case $1 in
    c) case $2 in *target-dir* | *build-dir*) __atb= ;; *=*) ;; *) __atb= ;; esac ;;
    m) case $2 in *..* | *[{}]* | "$__atb_c"/*) __atb= ;; "$HOME"/* | [!/]*) ;; *) __atb= ;; esac ;;
  esac
}
if [ -n "$__atb" ]; then
  __atb_v=$__atb_t __atb_u= __atb_n=
  for __atb_a in "$@"; do
    case $__atb_n in
      c | m) __atb_ok "$__atb_n" "$__atb_a"; __atb_n=; continue ;;
      s) __atb_n=; continue ;;
    esac
    case $__atb_a in
      --) break ;;
      --unverified) __atb_u=1 ;;
      --config) __atb_n=c ;;
      --config=*) __atb_ok c "${__atb_a#--config=}" ;;
      --manifest-path) __atb_n=m ;;
      --manifest-path=*) __atb_ok m "${__atb_a#--manifest-path=}" ;;
      --target-dir | --target-dir=* | -C | -C?*) __atb= ;;
      -Z | --color | --explain) [ -n "$__atb_v" ] || __atb_n=s ;;
      -*) ;;
      *) [ -n "$__atb_v" ] || __atb_v=$__atb_a ;;
    esac
  done
  [ -z "$__atb_n" ] || __atb=
  case $__atb_v in
    clean | tippy | clippy) ;;
    build | b | check | c | test | t | run | r | bench | doc | d | rustc | rustdoc | fix) [ -n "$__atb_u" ] || __atb= ;;
    *) __atb= ;;
  esac
fi
if [ -n "$__atb" ]; then
  __atb_d=$PWD
  while [ -n "$__atb" ]; do
    if [ -h "$__atb_d" ]; then __atb=; break; fi
    __atb_scan "$__atb_d/.cargo"
    [ -n "$__atb_d" ] || break
    __atb_d=${__atb_d%/*}
  done
fi
[ -n "$__atb" ] && __atb_scan "${CARGO_HOME:-$HOME/.cargo}"
if [ -n "$__atb" ]; then
  if [ -d "$__atb_c" ]; then [ -w "$__atb_c" ] || __atb=
  elif [ -d "${__atb_c%/*}" ]; then [ -w "${__atb_c%/*}" ] || __atb=
  else [ -w "$HOME/Library/Caches" ] || __atb=
  fi
fi
[ -n "$__atb" ] && set -- --config "build.build-dir='$__atb_c{workspace-root}/.cargo-build'" "$@"
"#;

/// The block a shim forwarding to the store file `name` carries: `targo`'s, or
/// `targo-tippy`'s, which names no subcommand (it is the lint). `None` for any other tool.
#[must_use]
pub fn shim_block(name: &str) -> Option<String> {
    match name {
        "targo" => Some(SH_BLOCK.to_owned()),
        "targo-tippy" => Some(SH_BLOCK.replacen("\n__atb_t=\n", "\n__atb_t=tippy\n", 1)),
        _ => None,
    }
}

/// Which tool reads the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    /// Trust's `targo` (or Trust's own `cargo`, which is the same program): a build needs
    /// `--unverified`; the verified lane is left alone.
    Targo,
    /// `targo-tippy`, the lint: every argument is its own, and it takes no lane.
    Tippy,
    /// An upstream `cargo`: it has no lane flag, and a leading `+<toolchain>` is rustup's
    /// and stays first.
    Cargo,
}

/// What [`with_build_dir`] reads of the machine, injected so every rule is tested
/// without touching the real one.
pub struct Machine<'a> {
    /// `$HOME`.
    pub home: Option<&'a Path>,
    /// The working directory, as the kernel names it (no symlinks).
    pub cwd: Option<&'a Path>,
    /// One environment variable.
    pub env: &'a dyn Fn(&str) -> Option<OsString>,
    /// A config file's text: `Ok(None)` when there is no such file, `Err` when one exists
    /// and cannot be read.
    pub read: &'a dyn Fn(&Path) -> Result<Option<String>, ()>,
    /// Whether this process may write the directory (`access(2)`, which a sandbox's
    /// denial answers too); `None` when it is not a directory.
    pub writable: &'a dyn Fn(&Path) -> Option<bool>,
}

/// [`with_build_dir`] on this process's machine; `None` off macOS.
#[must_use]
pub fn with_build_dir_here(args: &[String], tool: Tool) -> Option<Vec<String>> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let home = aterm_types::dirs::home_dir();
    let cwd = std::env::current_dir().ok();
    with_build_dir(
        args,
        tool,
        &Machine {
            home: home.as_deref(),
            cwd: cwd.as_deref(),
            env: &|name| std::env::var_os(name),
            read: &read_config,
            writable: &writable,
        },
    )
}

/// `args` with `--config build.build-dir=…` added, or `None` when the command keeps its
/// own layout ([`SH_BLOCK`] is the same rule in `sh`).
#[must_use]
pub fn with_build_dir(args: &[String], tool: Tool, m: &Machine<'_>) -> Option<Vec<String>> {
    let home = m
        .home
        .filter(|h| h.is_absolute() && !h.as_os_str().as_encoded_bytes().ends_with(b"/"))?;
    let cwd = m.cwd?;
    let root = cache_root(home);
    let value = config_value(&root)?;
    if !cwd.starts_with(home) || cwd == home || cwd.starts_with(&root) || has_brace(cwd) {
        return None;
    }
    // A `$PWD` that is not the real folder reached it through a link: the shim sees only
    // `$PWD`, so both halves leave it alone.
    if (m.env)("PWD").is_some_and(|p| Path::new(&p) != cwd) {
        return None;
    }
    let chosen = [
        "CARGO_TARGET_DIR",
        "CARGO_BUILD_TARGET_DIR",
        "CARGO_BUILD_BUILD_DIR",
    ];
    if chosen.iter().any(|v| (m.env)(v).is_some()) {
        return None;
    }
    let at = usize::from(tool == Tool::Cargo && args.first().is_some_and(|a| a.starts_with('+')));
    if !command_takes_it(&args[at..], tool, home, &root) {
        return None;
    }
    if cwd
        .ancestors()
        .any(|d| config_chooses(&d.join(".cargo"), m))
    {
        return None;
    }
    let cargo_home = (m.env)("CARGO_HOME")
        .filter(|v| !v.is_empty())
        .map_or_else(|| home.join(".cargo"), PathBuf::from);
    if config_chooses(&cargo_home, m) {
        return None;
    }
    let parent = root.parent()?;
    let can_write = (m.writable)(&root)
        .or_else(|| (m.writable)(parent))
        .or_else(|| (m.writable)(&home.join("Library/Caches")))
        .unwrap_or(false);
    if !can_write {
        return None;
    }
    let mut out = Vec::with_capacity(args.len() + 2);
    out.extend_from_slice(&args[..at]);
    out.push("--config".to_owned());
    out.push(value);
    out.extend_from_slice(&args[at..]);
    Some(out)
}

/// Whether a path holds a `{` or `}`, which Cargo would read as a template once
/// `{workspace-root}` is expanded.
fn has_brace(p: &Path) -> bool {
    p.as_os_str()
        .as_encoded_bytes()
        .iter()
        .any(|b| matches!(b, b'{' | b'}'))
}

/// The verb and flags half of the rule: a building verb (for `targo`, with
/// `--unverified`; `clean` and the lint take no lane), and nothing on the line before
/// `--` that names its own layout.
fn command_takes_it(args: &[String], tool: Tool, home: &Path, root: &Path) -> bool {
    let config_ok = |v: &str| v.contains('=') && !names_a_dir(v);
    let manifest_ok = |v: &str| {
        let p = Path::new(v);
        !v.is_empty()
            && !v.contains("..")
            && !has_brace(p)
            && !p.starts_with(root)
            && (!v.starts_with('/') || p.starts_with(home))
    };
    let mut verb = (tool == Tool::Tippy).then_some("tippy");
    let mut unverified = false;
    let mut it = args.iter().map(String::as_str);
    while let Some(a) = it.next() {
        let ok = match a {
            "--" => break,
            "--unverified" => {
                unverified = true;
                true
            }
            "--config" => it.next().is_some_and(config_ok),
            "--manifest-path" => it.next().is_some_and(manifest_ok),
            "-Z" | "--color" | "--explain" => verb.is_some() || it.next().is_some(),
            _ => {
                if let Some(v) = a.strip_prefix("--config=") {
                    config_ok(v)
                } else if let Some(v) = a.strip_prefix("--manifest-path=") {
                    manifest_ok(v)
                } else if a == "--target-dir"
                    || a.starts_with("--target-dir=")
                    || a.starts_with("-C")
                {
                    false
                } else {
                    if !a.starts_with('-') && verb.is_none() {
                        verb = Some(a);
                    }
                    true
                }
            }
        };
        if !ok {
            return false;
        }
    }
    match verb {
        Some("clean" | "tippy" | "clippy") => true,
        Some(v) if BUILD_VERBS.contains(&v) => tool == Tool::Cargo || unverified,
        _ => false,
    }
}

/// Whether a config value or line names a target or build directory.
fn names_a_dir(s: &str) -> bool {
    s.contains("target-dir") || s.contains("build-dir")
}

/// Whether `dir/config.toml` or `dir/config` chooses a layout: a line naming
/// `target-dir` or `build-dir` that is not a comment. A file that exists and cannot be
/// read counts as a choice.
fn config_chooses(dir: &Path, m: &Machine<'_>) -> bool {
    ["config.toml", "config"]
        .iter()
        .any(|name| match (m.read)(&dir.join(name)) {
            Err(()) => true,
            Ok(None) => false,
            Ok(Some(text)) => text.lines().any(|line| {
                names_a_dir(line) && !line.trim_start_matches([' ', '\t']).starts_with('#')
            }),
        })
}

/// [`Machine::read`] on the real filesystem: a regular file (followed, as Cargo does)
/// read whole; anything else is no file.
fn read_config(path: &Path) -> Result<Option<String>, ()> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => std::fs::read(path)
            .map(|b| Some(String::from_utf8_lossy(&b).into_owned()))
            .map_err(|_| ()),
        Ok(_) => Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(()),
    }
}

/// [`Machine::writable`] on the real filesystem.
fn writable(dir: &Path) -> Option<bool> {
    dir.is_dir()
        .then(|| crate::platform::dir_writable_by_caller(dir))
}

/// Whether a cargo that reports `release` (its `-vV` line) reads `build.build-dir` and its
/// `{workspace-root}` template: stable 1.91 and every later release, and any pre-release
/// from 1.92 on (a 1.91 nightly may predate the stabilisation).
#[must_use]
pub fn release_reads_build_dir(release: &str) -> bool {
    let (version, pre) = release
        .split_once('-')
        .map_or((release, false), |(v, _)| (v, true));
    let mut parts = version.split('.').map(str::parse::<u32>);
    match (parts.next(), parts.next()) {
        (Some(Ok(1)), Some(Ok(minor))) => minor > 91 || (minor == 91 && !pre),
        (Some(Ok(major)), Some(Ok(_))) => major > 1,
        _ => false,
    }
}

/// Removes the build directories under the cache `root` that nobody builds into any more,
/// and returns them: one unwritten for [`RECLAIM_GONE`] whose workspace is seen gone
/// ([`checkout::gone`], `home` naming the folders it never looks into), and any unwritten
/// for [`RECLAIM_IDLE`]. One whose build lock a build holds is kept; each other is renamed
/// aside while its locks are held, then removed. Never follows a symlink and never leaves
/// `root`. Mirror folders left empty go too.
pub fn reclaim(root: &Path, home: Option<&Path>, now: SystemTime) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    if std::fs::symlink_metadata(root).is_ok_and(|m| m.is_dir()) {
        walk(root, home, root, now, 0, &mut removed);
    }
    removed
}

/// How deep [`reclaim`] walks the mirror of the filesystem.
const MAX_DEPTH: usize = 64;

/// One mirror folder `dir` under `root`: judge its `.cargo-build`, finish a removal an
/// earlier pass left, walk its other children, then remove `dir` itself if it ended empty
/// and is not brand new. Returns whether anything under `dir` was removed.
fn walk(
    root: &Path,
    home: Option<&Path>,
    dir: &Path,
    now: SystemTime,
    depth: usize,
    removed: &mut Vec<PathBuf>,
) -> bool {
    let quiet = last_written(dir).is_some_and(|t| age(now, t) >= RECLAIM_GONE);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let children: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    let mut any = false;
    for child in children {
        let name = child.file_name().unwrap_or_default();
        if name == LEAF {
            let Some(held) = workspace_of(root, dir).and_then(|ws| unused(&ws, &child, home, now))
            else {
                continue;
            };
            let doomed = dir.join(DOOMED);
            let _ = std::fs::remove_dir_all(&doomed);
            let moved = std::fs::rename(&child, &doomed).is_ok();
            // A build that started meanwhile waited on these, and builds into a new folder.
            drop(held);
            if moved {
                let _ = std::fs::remove_dir_all(&doomed);
                removed.push(child);
                any = true;
            }
        } else if name == DOOMED {
            any |= std::fs::remove_dir_all(&child).is_ok();
        } else if depth < MAX_DEPTH {
            any |= walk(root, home, &child, now, depth + 1, removed);
        }
    }
    if dir != root && (quiet || any) {
        let _ = std::fs::remove_dir(dir);
    }
    any
}

/// The workspace a mirror folder stands for: `dir` with `root` taken off, as an absolute
/// path.
fn workspace_of(root: &Path, dir: &Path) -> Option<PathBuf> {
    let rel = dir.strip_prefix(root).ok()?;
    Some(Path::new("/").join(rel))
}

/// `leaf`'s build locks, held, when nobody builds into `leaf`, `workspace`'s build
/// directory, any more: idle long enough, no build holding a lock, and nothing written
/// since it was first looked at. The workspace is looked at only when the age alone does
/// not decide.
fn unused(
    workspace: &Path,
    leaf: &Path,
    home: Option<&Path>,
    now: SystemTime,
) -> Option<Vec<crate::lock::Flock>> {
    let seen = look(leaf)?;
    let idle = age(now, seen.newest);
    if idle < RECLAIM_GONE || (idle < RECLAIM_IDLE && checkout::gone(workspace, home).is_err()) {
        return None;
    }
    let held = seen
        .locks
        .iter()
        .map(|lock| {
            if !std::fs::symlink_metadata(lock).is_ok_and(|m| m.is_file()) {
                return None;
            }
            crate::lock::Flock::try_lock(std::fs::File::open(lock).ok()?).ok()
        })
        .collect::<Option<Vec<_>>>()?;
    let again = look(leaf)?;
    (again.newest == seen.newest && again.locks == seen.locks).then_some(held)
}

/// What [`look`] finds in a build directory.
struct Seen {
    /// The newest modification time of the directory and its folders.
    newest: SystemTime,
    /// The cargo locks in its profiles ([`CARGO_LOCKS`]), sorted.
    locks: Vec<PathBuf>,
}

/// The newest modification time of `leaf` and every folder down to [`WRITE_DEPTH`]
/// levels below it, and the cargo locks on the way; never follows a link. `None` when
/// `leaf` is not a real directory.
fn look(leaf: &Path) -> Option<Seen> {
    let meta = std::fs::symlink_metadata(leaf).ok()?;
    if !meta.is_dir() {
        return None;
    }
    let mut seen = Seen {
        newest: meta.modified().ok()?,
        locks: Vec::new(),
    };
    let mut stack = vec![(leaf.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                if let Ok(t) = std::fs::symlink_metadata(&path).and_then(|m| m.modified()) {
                    seen.newest = seen.newest.max(t);
                }
                if depth + 1 < WRITE_DEPTH {
                    stack.push((path, depth + 1));
                }
            } else if CARGO_LOCKS.iter().any(|l| entry.file_name() == *l) {
                seen.locks.push(path);
            }
        }
    }
    seen.locks.sort();
    Some(seen)
}

/// `dir`'s own modification time, not followed.
fn last_written(dir: &Path) -> Option<SystemTime> {
    std::fs::symlink_metadata(dir).ok()?.modified().ok()
}

/// How long ago `then` was; a time in the future is no time at all.
fn age(now: SystemTime, then: SystemTime) -> Duration {
    now.duration_since(then).unwrap_or(Duration::ZERO)
}

/// [`reclaim`] over this user's cache, now; nothing off macOS or without a home.
#[must_use]
pub fn reclaim_home() -> Vec<PathBuf> {
    if !cfg!(target_os = "macos") {
        return Vec::new();
    }
    aterm_types::dirs::home_dir().map_or_else(Vec::new, |home| {
        reclaim(&cache_root(&home), Some(&home), SystemTime::now())
    })
}

#[cfg(test)]
#[path = "build_cache_tests.rs"]
mod tests;
