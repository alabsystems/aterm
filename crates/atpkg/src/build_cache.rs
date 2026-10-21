// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Rust build intermediates out of Spotlight (macOS).
//!
//! A command that builds gets a build directory Spotlight skips: `--config
//! build.build-dir=<cache><workspace>/.cargo-build` when nothing else chose a target or
//! build directory (binaries stay in `target/`, the rest goes under
//! `~/Library/Caches/aterm/cargo-build.noindex`), else `<target>/build.noindex` under the
//! target directory it builds into; only a caller's own build directory keeps cargo's
//! layout. The rule itself is `aterm_toolchain::build_cache` ([`decide`], re-exported
//! here with the rest of it); the `targo`/`targo-tippy` shims decide in `sh`
//! ([`shim_block`]), `cargo` in an aterm tab through [`with_build_dir_here`], and
//! `aterm-verify` for the children it starts; the tests hold the `sh` and Rust halves to
//! one rule. Only `targo trust`, whose cargo reads no `--config`, gets its folder through
//! the environment (`CARGO_BUILD_BUILD_DIR`); every other command is handed a `--config`,
//! which a nested cargo started by the build never inherits. [`reclaim`] removes what
//! nobody builds into any more.

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
/// unwritten this long ([`reclaim_watched`]'s `gone_after` there) instead of
/// [`RECLAIM_GONE`]: nobody
/// builds into a removed workspace again, and a day of waiting is a day of a full disk. Ten
/// minutes, the window the harness's disk watch keeps for a just-finished build.
pub const RECLAIM_GONE_UNDER_PRESSURE: Duration = Duration::from_secs(10 * 60);

/// Any build directory goes once unwritten this long, whatever its workspace's state: it
/// is a cache, and a month on the compiler has usually moved and cargo would rebuild anyway.
pub const RECLAIM_IDLE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// The shim's half, rendered ahead of a `targo`/`targo-tippy` shim's `exec` on macOS:
/// shell builtins only. It puts `--config …` in front of `"$@"`, or for `targo trust`
/// (whose cargo reads no `--config`) exports `CARGO_BUILD_BUILD_DIR` to that exec.
const SH_BLOCK: &str = r#"# aterm: build intermediates out of Spotlight
__atb_t=
__atb_c=${HOME-}/Library/Caches/aterm/cargo-build.noindex
__atb= __atb_k= __atb_e= __atb_T= __atb_tf= __atb_tn= __atb_D= __atb_al= __atb_ab=
case ${HOME-} in /?*) case ${PWD-} in "$HOME"/*) __atb=1 ;; esac ;; esac
case ${HOME-} in *[\'[:cntrl:]]*) __atb= ;; esac
case ${HOME-}${PWD-} in *[{}]*) __atb= ;; esac
case ${PWD-} in "$__atb_c" | "$__atb_c"/*) __atb= ;; esac
[ -n "${CARGO_BUILD_BUILD_DIR+1}" ] && __atb=
__atb_scan() {
  for __atb_f in "$1/config.toml" "$1/config"; do
    [ -f "$__atb_f" ] || continue
    [ -r "$__atb_f" ] || { __atb=; return; }
    __atb_s=
    while IFS= read -r __atb_l || [ -n "$__atb_l" ]; do
      __atb_x=${__atb_l#"${__atb_l%%[![:blank:]]*}"}
      case $__atb_x in [#]*) continue ;; esac
      case $__atb_l in *build-dir*) __atb=; return ;; esac
      case $__atb_l in *target-dir*) __atb_tn=1 ;; esac
      case $__atb_x in
        "["*) case $__atb_x in "[alias]"*) __atb_s=1 ;; *) __atb_s= ;; esac; continue ;;
      esac
      [ -n "$__atb_al" ] || continue
      case $__atb_x in
        alias."$__atb_v"[[:blank:]=]*) ;;
        "$__atb_v"[[:blank:]=]*) [ -n "$__atb_s" ] || continue ;;
        *) continue ;;
      esac
      case $__atb_x in *=*) ;; *) continue ;; esac
      __atb_x=${__atb_x#*=}
      while :; do
        case $__atb_x in [[:blank:]]* | '['* | '"'* | "'"*) __atb_x=${__atb_x#?} ;; *) break ;; esac
      done
      case ${__atb_x%%[![:alnum:]_-]*} in
        build | b | check | c | test | t | run | r | bench | doc | d | rustc | rustdoc | clippy | tippy | fix | clean) __atb_ab=1 ;;
      esac
    done < "$__atb_f"
  done
}
__atb_ok() {
  case $1 in
    c) case $2 in *target-dir* | *build-dir*) __atb= ;; *=*) ;; *) __atb= ;; esac ;;
    m) case $2 in *..* | *[{}]* | "$__atb_c"/*) __atb= ;; "$HOME"/* | [!/]*) ;; *) __atb= ;; esac ;;
    t) __atb_T=$2 __atb_tf=1 ;;
  esac
}
if [ -n "$__atb" ]; then
  __atb_v=$__atb_t __atb_u= __atb_n=
  for __atb_a in "$@"; do
    case $__atb_n in
      c | m | t) __atb_ok "$__atb_n" "$__atb_a"; __atb_n=; continue ;;
      s) __atb_n=; continue ;;
    esac
    case $__atb_a in
      --) break ;;
      --unverified) __atb_u=1 ;;
      --config) __atb_n=c ;;
      --config=*) __atb_ok c "${__atb_a#--config=}" ;;
      --manifest-path) __atb_n=m ;;
      --manifest-path=*) __atb_ok m "${__atb_a#--manifest-path=}" ;;
      --target-dir) __atb_n=t ;;
      --target-dir=*) __atb_ok t "${__atb_a#--target-dir=}" ;;
      -C | -C?*) __atb= ;;
      -Z | --color | --explain) [ -n "$__atb_v" ] || __atb_n=s ;;
      -*) ;;
      *) [ -n "$__atb_v" ] || __atb_v=$__atb_a ;;
    esac
  done
  [ -z "$__atb_n" ] || __atb=
  case $__atb_v in
    clean | tippy | clippy) __atb_k=1 ;;
    trust) __atb_e=1 ;;
    build | b | check | c | test | t | run | r | bench | doc | d | rustc | rustdoc | fix) [ -n "$__atb_u" ] && __atb_k=1 || __atb= ;;
    "") __atb= ;;
    *) [ -n "$__atb_u" ] && __atb_k=1 __atb_al=1 || __atb= ;;
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
[ -z "$__atb_al" ] || [ -n "$__atb_ab" ] || __atb=
if [ -n "$__atb" ] && [ -z "$__atb_tf" ]; then
  if [ -n "${CARGO_TARGET_DIR+1}" ]; then __atb_T=$CARGO_TARGET_DIR __atb_tf=1
  elif [ -n "${CARGO_BUILD_TARGET_DIR+1}" ]; then __atb_T=$CARGO_BUILD_TARGET_DIR __atb_tf=1
  fi
fi
if [ -n "$__atb" ] && [ -n "$__atb_tf" ]; then
  case $__atb_T in "") __atb= ;; /*) ;; *) __atb_T=$PWD/$__atb_T ;; esac
  case $__atb_T/ in *.noindex/*) __atb= ;; esac
  case $__atb_T in *[\'{}[:cntrl:]]*) __atb= ;; esac
  __atb_D=$__atb_T/build.noindex
elif [ -n "$__atb" ]; then
  [ -z "$__atb_tn" ] || __atb=
  if [ -n "$__atb_k" ]; then
    if [ -d "$__atb_c" ]; then [ -w "$__atb_c" ] || __atb_k=
    elif [ -d "${__atb_c%/*}" ]; then [ -w "${__atb_c%/*}" ] || __atb_k=
    else [ -w "$HOME/Library/Caches" ] || __atb_k=
    fi
  fi
  case $PWD/ in *.noindex/*) [ -n "$__atb_k" ] || __atb= ;; esac
  [ -n "$__atb_k" ] || __atb_D='{workspace-root}/target/build.noindex'
fi
if [ -z "$__atb" ]; then __atb_D=
elif [ -z "$__atb_D" ]; then set -- --config "build.build-dir='$__atb_c{workspace-root}/.cargo-build'" "$@"
elif [ -z "$__atb_e" ]; then set -- --config "build.build-dir='$__atb_D'" "$@"
else
  CARGO_BUILD_BUILD_DIR=$__atb_D
  export CARGO_BUILD_BUILD_DIR
fi
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

/// [`with_build_dir`] on this process's machine; `None` off macOS. It hands `--config`
/// only: `targo trust`'s folder goes through the environment, which the shim sets.
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

/// WHY A BUILD DIRECTORY GOES ([`reclaim`], [`reclaim_watched`]): what its intent row, its
/// removal row and the watch's account of it say — each rule [`reclaim`] takes one by, with
/// the figures it was taken on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unused {
    /// Its workspace is seen gone ([`checkout::gone_placed`]) and nothing wrote it for
    /// `idle`, at least the pass's window `after` ([`RECLAIM_GONE`], or
    /// [`RECLAIM_GONE_UNDER_PRESSURE`] below the disk watch's floor).
    Gone { idle: Duration, after: Duration },
    /// Nothing wrote it for `idle`, [`RECLAIM_IDLE`] or more: it goes whatever its
    /// workspace's state, which was not looked at — it may still be there.
    Idle { idle: Duration },
    /// What an earlier pass renamed aside and did not finish removing.
    Unfinished,
}

/// Removes the build directories under the cache `root` that nobody builds into any more,
/// and returns them: one unwritten for [`RECLAIM_GONE`] whose workspace is seen gone
/// ([`checkout::gone`], `home` naming the folders it never looks into), and any unwritten
/// for [`RECLAIM_IDLE`]. One whose build lock a build holds is kept; each other is renamed
/// aside while its locks are held, then removed. A stage an earlier pass renamed aside and
/// did not finish removing is finished, and returned too. Never follows a symlink and never
/// leaves `root`. Mirror folders left empty go too.
pub fn reclaim(root: &Path, home: Option<&Path>, now: SystemTime) -> Vec<PathBuf> {
    reclaim_watched(root, home, now, RECLAIM_GONE, None, &mut |_, _| true)
        .into_iter()
        .map(|(path, _)| path)
        .collect()
}

/// [`reclaim`] as the window's disk watch runs it, each directory returned with why it went
/// ([`Unused`]): a gone workspace's build directory goes once unwritten for `gone_after`
/// ([`RECLAIM_GONE`] while the volume is only low, [`RECLAIM_GONE_UNDER_PRESSURE`] below the
/// floor); and with `placing` (the home volume's device and a complete mount table), a
/// workspace nested in a removed checkout is gone too ([`checkout::gone_placed`]).
///
/// WRITE-AHEAD: `record` is asked for each build directory about to go, and why, with its
/// locks held and before anything of it is moved — and for a stage an earlier pass left
/// ([`Unused::Unfinished`]) before it is removed; one it answers `false` for (its intent
/// could not be put on record — a full disk) is kept. The caller may size it there. What is
/// returned was removed whole: a stage finished is returned as [`Unused::Unfinished`], so
/// each intent asked has its outcome; a directory whose removal was cut short is returned by
/// the pass that finishes its stage.
pub fn reclaim_watched(
    root: &Path,
    home: Option<&Path>,
    now: SystemTime,
    gone_after: Duration,
    placing: Option<&checkout::Placing>,
    record: &mut dyn FnMut(&Path, Unused) -> bool,
) -> Vec<(PathBuf, Unused)> {
    let mut removed = Vec::new();
    if std::fs::symlink_metadata(root).is_ok_and(|m| m.is_dir()) {
        let mut sweep = Sweep {
            root,
            home,
            now,
            gone_after,
            placing,
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
    /// What places a workspace nested in a removed checkout ([`checkout::gone_placed`]).
    placing: Option<&'a checkout::Placing>,
    /// Asked before each removal, its locks held: `false` keeps it.
    record: &'a mut dyn FnMut(&Path, Unused) -> bool,
    removed: &'a mut Vec<(PathBuf, Unused)>,
}

/// How deep [`reclaim`] walks the mirror of the filesystem.
const MAX_DEPTH: usize = 64;

/// One mirror folder `dir` under `root`: judge its `.cargo-build`, finish a removal an
/// earlier pass left, walk its other children, then remove `dir` itself if it ended empty
/// and is not brand new. Returns whether anything under `dir` was removed.
impl Sweep<'_> {
    /// Whether the stage at `path` may be cleared: `Some(false)` where there is none (a
    /// missing stage needs no intent), `Some(true)` where one stands and its intent is on
    /// record (existing staged bytes need one, even after a crash), `None` to keep it — its
    /// intent refused, or unknown metadata, or no directory.
    fn stage_recorded(&mut self, path: &Path) -> Option<bool> {
        match std::fs::symlink_metadata(path) {
            Ok(meta) => (meta.is_dir() && (self.record)(path, Unused::Unfinished)).then_some(true),
            Err(e) => (e.kind() == std::io::ErrorKind::NotFound).then_some(false),
        }
    }

    /// FINISH A STAGE an earlier pass left, its intent on record ([`Self::stage_recorded`]):
    /// removed whole, it is returned as [`Unused::Unfinished`], so its outcome is on record
    /// beside its intent and its bytes are counted; one a removal could not finish stays,
    /// returned as nothing, for the next pass to record and finish.
    fn finish_stage(&mut self, stage: &Path) -> bool {
        let gone = std::fs::remove_dir_all(stage).is_ok();
        if gone {
            self.removed.push((stage.to_path_buf(), Unused::Unfinished));
        }
        gone
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
                let Some((held, why)) = workspace_of(root, dir)
                    .and_then(|ws| unused(&ws, &child, home, now, self.gone_after, self.placing))
                else {
                    continue;
                };
                if !(self.record)(&child, why) {
                    drop(held);
                    continue;
                }
                let doomed = dir.join(DOOMED);
                let Some(staged) = self.stage_recorded(&doomed) else {
                    drop(held);
                    continue;
                };
                if staged {
                    any |= self.finish_stage(&doomed);
                }
                let moved = std::fs::rename(&child, &doomed).is_ok();
                // A build that started meanwhile waited on these, and builds into a new folder.
                drop(held);
                // Returned only once removed whole: a removal cut short leaves its rest a
                // stage, which the next pass records and finishes as [`Unused::Unfinished`]
                // — counted there, and never twice.
                if moved {
                    any = true;
                    if std::fs::remove_dir_all(&doomed).is_ok() {
                        self.removed.push((child, why));
                    }
                }
            } else if name == DOOMED {
                if self.stage_recorded(&child) == Some(true) {
                    any |= self.finish_stage(&child);
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
/// path — the inverse of [`aterm_toolchain::build_cache::build_dir`]'s join on macOS, the
/// one platform anything writes the cache on ([`with_build_dir_here`], the shim and
/// `aterm-verify` add a build directory there alone; [`reclaim_home`] runs there alone).
/// On Windows nothing writes it (`<root>C:\…` could not even be named: a `:` inside a
/// component), and a folder found under it anyway reads as `\<rel>`, no absolute path,
/// which `checkout::gone_placed` refuses: nothing under it is called gone.
fn workspace_of(root: &Path, dir: &Path) -> Option<PathBuf> {
    let rel = dir.strip_prefix(root).ok()?;
    Some(Path::new("/").join(rel))
}

/// `leaf`'s build locks, held, and why it goes ([`Unused`]), when nobody builds into
/// `leaf`, `workspace`'s build directory, any more: idle long enough, no build holding a
/// lock, and nothing written since it was first looked at. The workspace is looked at only
/// when the age alone does not decide.
fn unused(
    workspace: &Path,
    leaf: &Path,
    home: Option<&Path>,
    now: SystemTime,
    gone_after: Duration,
    placing: Option<&checkout::Placing>,
) -> Option<(Vec<crate::lock::Flock>, Unused)> {
    let seen = look(leaf)?;
    let idle = age(now, seen.newest);
    if idle < gone_after {
        return None;
    }
    let why = if idle >= RECLAIM_IDLE {
        Unused::Idle { idle }
    } else if checkout::gone_placed(workspace, home, placing).is_ok() {
        Unused::Gone {
            idle,
            after: gone_after,
        }
    } else {
        return None;
    };
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
    (again.newest == seen.newest && again.locks == seen.locks).then_some((held, why))
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

/// [`reclaim_watched`] over this user's cache, now, `record` asked before each removal;
/// nothing off macOS or without a home.
pub fn reclaim_home_watched(
    gone_after: Duration,
    placing: Option<&checkout::Placing>,
    record: &mut dyn FnMut(&Path, Unused) -> bool,
) -> Vec<(PathBuf, Unused)> {
    if !cfg!(target_os = "macos") {
        return Vec::new();
    }
    aterm_types::dirs::home_dir().map_or_else(Vec::new, |home| {
        reclaim_watched(
            &cache_root(&home),
            Some(&home),
            SystemTime::now(),
            gone_after,
            placing,
            record,
        )
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
