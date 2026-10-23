// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Rust build intermediates out of Spotlight (macOS).

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

#[path = "build_cache_stale.rs"]
pub mod stale;

#[path = "build_cache_tooling.rs"]
pub mod tooling;

pub use stale::RECLAIM_STALE;

/// When a workspace is gone: the rule the window's disk watch uses too.
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

/// Below the disk watch's floor, a build directory whose workspace is seen gone goes once unwritten this long ([`reclaim_watched`]'s `gone_after` there) instead of [`RECLAIM_GONE`]: nobody builds into a removed workspace again, and a day of waiting is a day of a full disk.
pub const RECLAIM_GONE_UNDER_PRESSURE: Duration = Duration::from_secs(10 * 60);

/// Any build directory goes once unwritten this long, whatever its workspace's state: it is a cache, and a month on the compiler has usually moved and cargo would rebuild anyway.
pub const RECLAIM_IDLE: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// The shim's half, rendered ahead of a `targo`/`targo-tippy` shim's `exec` on macOS:
/// shell builtins only.
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

/// The block a shim forwarding to the store file `name` carries: `targo`'s, or `targo-tippy`'s, which names no subcommand (it is the lint).
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

/// Whether a cargo that reports `release` (its `-vV` line) reads `build.build-dir` and its `{workspace-root}` template: stable 1.91 and every later release, and any pre-release from 1.92 on (a 1.91 nightly may predate the stabilisation).
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

/// WHY A BUILD DIRECTORY GOES ([`reclaim`], [`reclaim_watched`]): what its intent row, its removal row and the watch's account of it say — each rule [`reclaim`] takes one by, with the figures it was taken on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unused {
    /// Its workspace is seen gone ([`checkout::gone_placed`]) and nothing wrote it for `idle`, at least the pass's window `after` ([`RECLAIM_GONE`], or [`RECLAIM_GONE_UNDER_PRESSURE`] below the disk watch's floor).
    Gone { idle: Duration, after: Duration },
    /// Nothing wrote it for `idle`, [`RECLAIM_IDLE`] or more: it goes whatever its workspace's state, which was not looked at — it may still be there.
    Idle { idle: Duration },
    /// What an earlier pass renamed aside and did not finish removing.
    Unfinished,
    /// A completed pinned tool of the tooling cache nothing ran for `idle`, at least `after` ([`RECLAIM_IDLE`]).
    Tooling { idle: Duration, after: Duration },
    /// A tooling scratch carrying both its give-up exec stamp and a regular run-proof file ([`aterm_uds::deadline::RAN`]); elapsed time and a reboot never replace that proof.
    GivenUp,
    /// PART of a build directory that stays ([`stale`]): `incremental` crate folders and `units` an old compiler built, each unwritten for `after` ([`RECLAIM_STALE`]) or more.
    Stale {
        incremental: usize,
        units: usize,
        after: Duration,
    },
}

/// Removes the build directories under the cache `root` that nobody builds into any more, and returns them: one unwritten for [`RECLAIM_GONE`] whose workspace is seen gone ([`checkout::gone`], `home` naming the folders it never looks into), and any unwritten for [`RECLAIM_IDLE`].
pub fn reclaim(root: &Path, home: Option<&Path>, now: SystemTime) -> Vec<PathBuf> {
    reclaim_watched(root, home, now, RECLAIM_GONE, None, &mut |_, _| true)
        .into_iter()
        .map(|(path, _)| path)
        .collect()
}

/// [`reclaim`] as the window's disk watch runs it, each directory returned with why it went ([`Unused`]): a gone workspace's build directory goes once unwritten for `gone_after` ([`RECLAIM_GONE`] while the volume is only low, [`RECLAIM_GONE_UNDER_PRESSURE`] below the floor); and with `placing` (the home volume's device and a complete mount table), a workspace nested in a removed checkout is gone too ([`checkout::gone_placed`]).
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
            compiler_store: compiler_store(home),
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
    /// Independently admitted actual store; unknown keeps compiler-classified units.
    compiler_store: Option<PathBuf>,
    /// Asked before each removal, its locks held: `false` keeps it.
    record: &'a mut dyn FnMut(&Path, Unused) -> bool,
    removed: &'a mut Vec<(PathBuf, Unused)>,
}

/// How deep [`reclaim`] walks the mirror of the filesystem.
const MAX_DEPTH: usize = 64;

/// One mirror folder `dir` under `root`: judge its `.cargo-build`, finish a removal an
/// earlier pass left, walk its other children, then remove `dir` itself if it ended empty
/// and is not brand new.
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

    /// WHAT NO BUILD READS AGAIN in the build directory `leaf` that stays ([`stale`]):
    /// surveyed without locks, then every cargo lock it holds taken (one a build holds keeps
    /// it all this pass), and the decision made again from a fresh survey under those locks;
    /// moved into the stage `dir`/[`DOOMED`] under the locks, recorded as that stage
    /// ([`Unused::Stale`]; refused, every entry moves back), and removed with the locks let
    /// go.
    fn prune(&mut self, dir: &Path, leaf: &Path) -> bool {
        let Some(seen) = look(leaf) else {
            return false;
        };
        let profiles = profiles_of(&seen.locks);
        let doomed = stale::decide(
            &stale::survey(
                &stale::Disk,
                leaf,
                &profiles,
                self.compiler_store.as_deref(),
            ),
            self.now,
        );
        if doomed.is_empty() {
            return false;
        }
        let Some(held) = hold(&seen.locks) else {
            return false;
        };
        if look(leaf).is_none_or(|again| again.locks != seen.locks) {
            return false;
        }
        let doomed = stale::decide(
            &stale::survey(
                &stale::Disk,
                leaf,
                &profiles,
                self.compiler_store.as_deref(),
            ),
            self.now,
        );
        if doomed.is_empty() {
            return false;
        }
        let stage = dir.join(DOOMED);
        let mut any = false;
        match self.stage_recorded(&stage) {
            None => return false,
            Some(true) => any |= self.finish_stage(&stage),
            Some(false) => {}
        }
        if std::fs::create_dir(&stage).is_err() {
            return any;
        }
        let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
        let (mut incremental, mut units) = (0, 0);
        for one in &doomed {
            let before = moved.len();
            for path in &one.paths {
                let to = stage.join(moved.len().to_string());
                if std::fs::rename(path, &to).is_ok() {
                    moved.push((path.clone(), to));
                }
            }
            if moved.len() > before {
                match one.part {
                    stale::Part::Incremental => incremental += 1,
                    stale::Part::Unit => units += 1,
                }
            }
        }
        let why = Unused::Stale {
            incremental,
            units,
            after: RECLAIM_STALE,
        };
        if moved.is_empty() || !(self.record)(&stage, why) {
            for (from, to) in moved.iter().rev() {
                let _ = std::fs::rename(to, from);
            }
            let _ = std::fs::remove_dir(&stage);
            drop(held);
            return any;
        }
        drop(held);
        if std::fs::remove_dir_all(&stage).is_ok() {
            self.removed.push((stage, why));
        }
        true
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
                    any |= self.prune(dir, &child);
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
                drop(held);
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
fn workspace_of(root: &Path, dir: &Path) -> Option<PathBuf> {
    let rel = dir.strip_prefix(root).ok()?;
    Some(Path::new("/").join(rel))
}

/// `leaf`'s build locks, held, and why it goes ([`Unused`]), when nobody builds into
/// `leaf`, `workspace`'s build directory, any more: idle long enough, no build holding a
/// lock, and nothing written since it was first looked at.
fn unused(
    workspace: &Path,
    leaf: &Path,
    home: Option<&Path>,
    now: SystemTime,
    gone_after: Duration,
    placing: Option<&checkout::Placing>,
) -> Option<(Vec<crate::lock::Flock>, Unused)> {
    let seen = look(leaf)?;
    let why = whole(workspace, &seen, home, now, gone_after, placing)?;
    let held = hold(&seen.locks)?;
    let again = look(leaf)?;
    (again.newest == seen.newest && again.locks == seen.locks).then_some((held, why))
}

/// Why the build directory of `workspace`, as [`look`] `seen` it, goes whole: idle at least
/// `gone_after` with its workspace seen gone, or [`RECLAIM_IDLE`] whatever its state.
fn whole(
    workspace: &Path,
    seen: &Seen,
    home: Option<&Path>,
    now: SystemTime,
    gone_after: Duration,
    placing: Option<&checkout::Placing>,
) -> Option<Unused> {
    let idle = age(now, seen.newest);
    if idle < gone_after {
        None
    } else if idle >= RECLAIM_IDLE {
        Some(Unused::Idle { idle })
    } else if checkout::gone_placed(workspace, home, placing).is_ok() {
        Some(Unused::Gone {
            idle,
            after: gone_after,
        })
    } else {
        None
    }
}

/// Every cargo lock in `locks` taken without waiting: `None` when a build holds one, or one
/// is no regular file (a link is never followed to a lock elsewhere).
fn hold(locks: &[PathBuf]) -> Option<Vec<crate::lock::Flock>> {
    locks
        .iter()
        .map(|lock| {
            if !std::fs::symlink_metadata(lock).is_ok_and(|m| m.is_file()) {
                return None;
            }
            crate::lock::Flock::try_lock(std::fs::File::open(lock).ok()?).ok()
        })
        .collect()
}

/// The profiles of a build directory: the folders holding its cargo `locks`.
fn profiles_of(locks: &[PathBuf]) -> Vec<PathBuf> {
    let mut profiles: Vec<PathBuf> = locks
        .iter()
        .filter_map(|lock| lock.parent().map(Path::to_path_buf))
        .collect();
    profiles.sort();
    profiles.dedup();
    profiles
}

/// What [`look`] finds in a build directory.
struct Seen {
    /// The newest modification time of the directory and its folders.
    newest: SystemTime,
    /// The cargo locks in its profiles ([`CARGO_LOCKS`]), sorted.
    locks: Vec<PathBuf>,
}

/// The newest modification time of `leaf` and every folder down to [`WRITE_DEPTH`]
/// levels below it, and the cargo locks on the way; never follows a link.
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

/// Admit the actual configured store independently of cache-supplied sysroots.
#[cfg(unix)]
fn compiler_store(home: Option<&Path>) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt as _;
    use std::path::Component;

    let home = home.filter(|home| {
        home.is_absolute() && !home.components().any(|c| c == Component::ParentDir)
    })?;
    let cfg = match crate::config::config_path() {
        Some(path) => crate::config::load_live(&path)?,
        None => crate::config::PackagesConfig::default(),
    };
    if cfg.unreadable {
        return None;
    }
    let default = crate::store::default_prefix(home);
    let prefix = cfg
        .prefix_path(Some(home))
        .unwrap_or_else(|| default.clone());
    if !prefix.is_absolute()
        || prefix.components().any(|c| c == Component::ParentDir)
        || (prefix != default && checkout::never_looked_at(&prefix, Some(home)).is_some())
    {
        return None;
    }
    let home_meta = std::fs::symlink_metadata(home).ok()?;
    if !home_meta.is_dir() || crate::platform::is_reparse(&home_meta) {
        return None;
    }
    let store = prefix.join("store");
    let own = prefix.starts_with(home) && prefix != home;
    let base = if own { home } else { Path::new("/") };
    let mut path = base.to_path_buf();
    let mut way = vec![path.clone()];
    for component in store.strip_prefix(base).ok()?.components() {
        let Component::Normal(name) = component else {
            return None;
        };
        path.push(name);
        way.push(path.clone());
    }
    for path in way {
        let meta = std::fs::symlink_metadata(&path).ok()?;
        let owner_ok = if own {
            crate::platform::dir_meta_is_private(&meta)
        } else {
            crate::platform::dir_meta_is_system(&meta)
        };
        if !meta.is_dir() || crate::platform::is_reparse(&meta) || !owner_ok {
            return None;
        }
        if path.starts_with(&prefix) && meta.dev() != home_meta.dev() {
            return None;
        }
    }
    Some(store)
}

#[cfg(not(unix))]
fn compiler_store(_: Option<&Path>) -> Option<PathBuf> {
    None
}

/// [`reclaim_watched`] over this user's cache, now, then [`tooling::reclaim`] over its tooling cache, `record` asked before each removal; nothing off macOS or without a home.
pub fn reclaim_home_watched(
    gone_after: Duration,
    placing: Option<&checkout::Placing>,
    record: &mut dyn FnMut(&Path, Unused) -> bool,
) -> Vec<(PathBuf, Unused)> {
    if !cfg!(target_os = "macos") {
        return Vec::new();
    }
    aterm_types::dirs::home_dir().map_or_else(Vec::new, |home| {
        let now = SystemTime::now();
        let mut removed = reclaim_watched(
            &cache_root(&home),
            Some(&home),
            now,
            gone_after,
            placing,
            record,
        );
        removed.extend(tooling::reclaim(&tooling::root(&home), now, record));
        removed
    })
}

/// [`reclaim`] over this user's cache, now, each path returned with why it went; nothing off macOS or without a home.
#[must_use]
pub fn reclaim_home() -> Vec<(PathBuf, Unused)> {
    reclaim_home_watched(RECLAIM_GONE, None, &mut |_, _| true)
}

/// WHAT A PASS WOULD TAKE, read and never acted on ([`plan`]): one build directory that would go whole, the parts that would go from one that stays, or a stage an earlier pass left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    /// The build directory, or the stage.
    pub path: PathBuf,
    /// Why, as the pass would record it.
    pub why: Unused,
    /// What would be removed: `path` itself, or the parts of it.
    pub paths: Vec<PathBuf>,
    /// Its cargo locks could not all be taken just now (a build holds one): a pass now keeps it whole.
    pub held: bool,
}

/// [`reclaim_watched`]'s decisions over the cache `root` at `now`, read and never acted on: nothing is moved or removed and no stage is finished.
#[must_use]
pub fn plan(
    root: &Path,
    home: Option<&Path>,
    now: SystemTime,
    gone_after: Duration,
    placing: Option<&checkout::Placing>,
) -> Vec<Planned> {
    let mut planned = Vec::new();
    if std::fs::symlink_metadata(root).is_ok_and(|m| m.is_dir()) {
        let store = compiler_store(home);
        let pass = (root, home, now, gone_after, placing, store.as_deref());
        plan_walk(pass, root, 0, &mut planned);
    }
    planned
}

/// [`plan`] for this user's cache now, with the day's window ([`reclaim_home`]'s), then [`tooling::plan`] for its tooling cache; nothing off macOS or without a home.
#[must_use]
pub fn plan_home() -> Vec<Planned> {
    if !cfg!(target_os = "macos") {
        return Vec::new();
    }
    aterm_types::dirs::home_dir().map_or_else(Vec::new, |home| {
        let now = SystemTime::now();
        let mut planned = plan(&cache_root(&home), Some(&home), now, RECLAIM_GONE, None);
        planned.extend(tooling::plan(&tooling::root(&home), now));
        planned
    })
}

/// What one [`plan`] judges by: its `root`, `home`, `now`, `gone_after` and `placing`.
type PlanPass<'a> = (
    &'a Path,
    Option<&'a Path>,
    SystemTime,
    Duration,
    Option<&'a checkout::Placing>,
    Option<&'a Path>,
);

/// [`plan`]'s walk of the mirror folder `dir`, as [`Sweep::walk`] walks it.
fn plan_walk(pass: PlanPass<'_>, dir: &Path, depth: usize, planned: &mut Vec<Planned>) {
    let (root, home, now, gone_after, placing, store) = pass;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut children: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    children.sort();
    for child in children {
        let name = child.file_name().unwrap_or_default();
        if name == LEAF {
            let Some(seen) = look(&child) else {
                continue;
            };
            let held = hold(&seen.locks).is_none();
            let goes = workspace_of(root, dir)
                .and_then(|ws| whole(&ws, &seen, home, now, gone_after, placing));
            if let Some(why) = goes {
                planned.push(Planned {
                    path: child.clone(),
                    why,
                    paths: vec![child],
                    held,
                });
                continue;
            }
            let survey = stale::survey(&stale::Disk, &child, &profiles_of(&seen.locks), store);
            let doomed = stale::decide(&survey, now);
            if doomed.is_empty() {
                continue;
            }
            let count = |part| doomed.iter().filter(|d| d.part == part).count();
            planned.push(Planned {
                path: child,
                why: Unused::Stale {
                    incremental: count(stale::Part::Incremental),
                    units: count(stale::Part::Unit),
                    after: RECLAIM_STALE,
                },
                paths: doomed.into_iter().flat_map(|d| d.paths).collect(),
                held,
            });
        } else if name == DOOMED {
            planned.push(Planned {
                path: child.clone(),
                why: Unused::Unfinished,
                paths: vec![child],
                held: false,
            });
        } else if depth < MAX_DEPTH {
            plan_walk(pass, &child, depth + 1, planned);
        }
    }
}

/// The bytes `paths` hold on disk: each file's allocated blocks once, however many names it has among them, never following a link.
#[must_use]
pub fn allocated(paths: &[PathBuf]) -> u64 {
    let mut seen: std::collections::HashSet<(u64, u64)> = std::collections::HashSet::new();
    let mut total = 0u64;
    let mut stack: Vec<PathBuf> = paths.to_vec();
    while let Some(path) = stack.pop() {
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            stack.extend(
                std::fs::read_dir(&path)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|e| e.path()),
            );
            continue;
        }
        #[cfg(unix)]
        let bytes = {
            use std::os::unix::fs::MetadataExt as _;
            if meta.nlink() > 1 && !seen.insert((meta.dev(), meta.ino())) {
                continue;
            }
            meta.blocks().saturating_mul(512)
        };
        #[cfg(not(unix))]
        let bytes = {
            let _ = &mut seen;
            meta.len()
        };
        total = total.saturating_add(bytes);
    }
    total
}

#[cfg(test)]
#[path = "build_cache_tests.rs"]
mod tests;
