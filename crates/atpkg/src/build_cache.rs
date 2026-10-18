// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Rust build intermediates out of Spotlight (macOS).
//!
//! A command that builds gets `--config build.build-dir=<cache><workspace>/.cargo-build`
//! when nothing else chose a target or build directory: binaries stay in `target/`, the
//! rest goes under `~/Library/Caches/aterm/cargo-build.noindex`, which Spotlight skips.
//! The rule itself is `aterm_toolchain::build_cache` ([`with_build_dir`], re-exported
//! here with the rest of it); the `targo`/`targo-tippy` shims decide in `sh`
//! ([`shim_block`]), `cargo` in an aterm tab through [`with_build_dir_here`], and
//! `aterm-verify` for the children it starts; the tests hold the `sh` and Rust halves to
//! one rule. Nothing is exported, so a nested cargo started by the build never inherits
//! it. [`reclaim`] removes what nobody builds into any more.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// When a workspace is gone: the rule the window's disk watch uses too. It never looks
/// into a privacy-guarded folder or a mounted volume.
pub use aterm_toolchain::checkout;

pub use aterm_toolchain::build_cache::*;

/// What [`reclaim`] renames a build directory to before removing it, so a build that
/// starts meanwhile gets a fresh folder instead of one being deleted under it.
const DOOMED: &str = ".cargo-build.removing";

/// The locks cargo holds in a profile for a whole build; [`reclaim`] skips a build
/// directory while a build holds one.
const CARGO_LOCKS: &[&str] = &[".cargo-build-lock", ".cargo-lock", ".cargo-artifact-lock"];

/// How deep under a build directory [`reclaim`] reads folders' modification times: a
/// build writes `<profile>/deps`, or `<triple>/<profile>/deps` when it names a target.
const WRITE_DEPTH: usize = 3;

/// A build directory whose workspace is seen gone goes once unwritten this long.
pub const RECLAIM_GONE: Duration = Duration::from_secs(24 * 60 * 60);

/// Below the disk watch's floor, a build directory whose workspace is seen gone goes once
/// unwritten this long ([`reclaim_under_pressure`]) instead of [`RECLAIM_GONE`]: nobody
/// builds into a removed workspace again, and a day of waiting is a day of a full disk. Ten
/// minutes, the window the harness's disk watch keeps for a just-finished build.
pub const RECLAIM_GONE_UNDER_PRESSURE: Duration = Duration::from_secs(10 * 60);

/// Any build directory goes once unwritten this long, whatever its workspace's state: it
/// is a cache, and a month on the compiler has usually moved and cargo would rebuild anyway.
pub const RECLAIM_IDLE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

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
    reclaim_after(root, home, now, RECLAIM_GONE, &mut |_| true)
}

/// [`reclaim`] when the volume is short of space (the window's disk watch, below its
/// floor): a gone workspace's build directory goes once unwritten for
/// [`RECLAIM_GONE_UNDER_PRESSURE`]; every other rule is [`reclaim`]'s.
///
/// WRITE-AHEAD: `record` is asked for each build directory about to go, with its locks
/// held and before anything of it is moved; one it answers `false` for (its intent could
/// not be put on record — a full disk) is kept. The caller may size it there.
pub fn reclaim_under_pressure(
    root: &Path,
    home: Option<&Path>,
    now: SystemTime,
    record: &mut dyn FnMut(&Path) -> bool,
) -> Vec<PathBuf> {
    reclaim_after(root, home, now, RECLAIM_GONE_UNDER_PRESSURE, record)
}

/// [`reclaim`], a gone workspace's build directory going once unwritten for `gone_after`
/// and once `record` answers for it.
fn reclaim_after(
    root: &Path,
    home: Option<&Path>,
    now: SystemTime,
    gone_after: Duration,
    record: &mut dyn FnMut(&Path) -> bool,
) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    if std::fs::symlink_metadata(root).is_ok_and(|m| m.is_dir()) {
        let mut sweep = Sweep {
            root,
            home,
            now,
            gone_after,
            record,
            removed: &mut removed,
        };
        sweep.walk(root, 0);
    }
    removed
}

/// One [`reclaim`] pass: where, when, and what went.
struct Sweep<'a> {
    root: &'a Path,
    home: Option<&'a Path>,
    now: SystemTime,
    /// How long a gone workspace's build directory must be unwritten.
    gone_after: Duration,
    /// Asked before each removal, its locks held: `false` keeps it.
    record: &'a mut dyn FnMut(&Path) -> bool,
    removed: &'a mut Vec<PathBuf>,
}

/// How deep [`reclaim`] walks the mirror of the filesystem.
const MAX_DEPTH: usize = 64;

/// One mirror folder `dir` under `root`: judge its `.cargo-build`, finish a removal an
/// earlier pass left, walk its other children, then remove `dir` itself if it ended empty
/// and is not brand new. Returns whether anything under `dir` was removed.
impl Sweep<'_> {
    /// A missing stage needs no intent; existing staged bytes do, even after a crash.
    /// Unknown metadata and non-directory entries stay untouched.
    fn stage_recorded(&mut self, path: &Path) -> bool {
        match std::fs::symlink_metadata(path) {
            Ok(meta) => meta.is_dir() && (self.record)(path),
            Err(e) => e.kind() == std::io::ErrorKind::NotFound,
        }
    }

    fn walk(&mut self, dir: &Path, depth: usize) -> bool {
        let (root, home, now) = (self.root, self.home, self.now);
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
                let Some(held) = workspace_of(root, dir)
                    .and_then(|ws| unused(&ws, &child, home, now, self.gone_after))
                else {
                    continue;
                };
                if !(self.record)(&child) {
                    drop(held);
                    continue;
                }
                let doomed = dir.join(DOOMED);
                if !self.stage_recorded(&doomed) {
                    drop(held);
                    continue;
                }
                let _ = std::fs::remove_dir_all(&doomed);
                let moved = std::fs::rename(&child, &doomed).is_ok();
                // A build that started meanwhile waited on these, and builds into a new folder.
                drop(held);
                if moved {
                    let _ = std::fs::remove_dir_all(&doomed);
                    self.removed.push(child);
                    any = true;
                }
            } else if name == DOOMED {
                if self.stage_recorded(&child) {
                    any |= std::fs::remove_dir_all(&child).is_ok();
                }
            } else if depth < MAX_DEPTH {
                any |= self.walk(&child, depth + 1);
            }
        }
        if dir != root && (quiet || any) {
            let _ = std::fs::remove_dir(dir);
        }
        any
    }
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
    gone_after: Duration,
) -> Option<Vec<crate::lock::Flock>> {
    let seen = look(leaf)?;
    let idle = age(now, seen.newest);
    if idle < gone_after || (idle < RECLAIM_IDLE && checkout::gone(workspace, home).is_err()) {
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

/// [`reclaim_under_pressure`] over this user's cache, now, `record` asked before each
/// removal; nothing off macOS or without a home.
pub fn reclaim_home_under_pressure(record: &mut dyn FnMut(&Path) -> bool) -> Vec<PathBuf> {
    if !cfg!(target_os = "macos") {
        return Vec::new();
    }
    aterm_types::dirs::home_dir().map_or_else(Vec::new, |home| {
        reclaim_under_pressure(&cache_root(&home), Some(&home), SystemTime::now(), record)
    })
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
