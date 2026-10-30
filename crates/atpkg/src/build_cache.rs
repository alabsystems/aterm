// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Rust build intermediates out of Spotlight (macOS).

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

#[path = "build_cache_stale.rs"]
pub mod stale;

#[path = "build_cache_tooling.rs"]
pub mod tooling;

#[path = "build_cache_retired.rs"]
pub mod retired;

#[path = "build_cache_registered.rs"]
pub mod registered;

#[path = "build_cache_remove.rs"]
pub mod remove;

#[path = "build_cache_fence.rs"]
mod fence;

pub use stale::RECLAIM_STALE;

/// When a workspace is gone: the rule the window's disk watch uses too.
pub use aterm_toolchain::checkout;

pub use aterm_toolchain::build_cache::*;

/// What [`reclaim`] renames a build directory to before removing it, so a build that
/// starts meanwhile gets a fresh folder instead of one being deleted under it: the first of the
/// stage names it takes from ([`stage_name`]), and the one fixed name an older aterm staged
/// under, which a pass still reads as a stage and finishes.
const DOOMED: &str = ".cargo-build.removing";

/// THE STAGE NAMES FROM `base`, lowest first: `base` itself for `n` 0, then `base-<n>`.
fn stage_name(base: &OsStr, n: u32) -> OsString {
    let mut name = base.to_os_string();
    if n > 0 {
        name.push(format!("-{n}"));
    }
    name
}

/// Whether `name` is one of the stage names from `base` ([`stage_name`]): `base` itself, or
/// `base-<n>` with `n` a decimal from 1, written without a leading zero.
fn is_stage(name: &OsStr, base: &OsStr) -> bool {
    match name
        .as_encoded_bytes()
        .strip_prefix(base.as_encoded_bytes())
    {
        Some([]) => true,
        Some([b'-', first, rest @ ..]) => {
            (b'1'..=b'9').contains(first) && rest.iter().all(u8::is_ascii_digit)
        }
        _ => false,
    }
}

/// TAKE A STAGE NAME OF ITS OWN in `holder`: `make` tried at each stage name from `base`, lowest
/// first, until one is made; a name something stands at (`AlreadyExists`: `make` never replaces
/// an entry) moves on to the next, so a stage an earlier pass could not finish never keeps
/// another from being staged beside it. Any other refusal is returned.
fn take_stage_name(
    holder: &Path,
    base: &OsStr,
    make: &mut dyn FnMut(&Path) -> std::io::Result<()>,
) -> std::io::Result<PathBuf> {
    for n in 0..u32::MAX {
        let path = holder.join(stage_name(base, n));
        match make(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(std::io::ErrorKind::AlreadyExists.into())
}

/// STAGE `from` ASIDE in `holder` under the lowest stage name from `base` nothing stands at
/// ([`take_stage_name`]), renamed never over another entry ([`rename_noreplace`]): the stage.
fn stage_aside(from: &Path, holder: &Path, base: &OsStr) -> std::io::Result<PathBuf> {
    take_stage_name(holder, base, &mut |to| rename_noreplace(from, to))
}

/// A NEW EMPTY STAGE in `holder`, made where nothing stands under the lowest stage name from
/// `base` that is free ([`take_stage_name`]; a folder is never made over another entry).
fn new_stage(holder: &Path, base: &OsStr) -> std::io::Result<PathBuf> {
    take_stage_name(holder, base, &mut |to| std::fs::create_dir(to))
}

/// THE STAGE NAME A PASS WOULD TAKE in `holder` now, read and never acted on: the lowest stage
/// name from `base` nothing stands at (not even a link), while this user may add a name to
/// `holder`; `None` when it may not, or a name cannot be looked at. A prune's stage is made only
/// while it is `Some`; a leaf or tooling entry is staged only while [`stage_for`] is.
fn free_stage(holder: &Path, base: &OsStr) -> Option<PathBuf> {
    if !crate::platform::dir_writable_by_caller(holder) {
        return None;
    }
    for n in 0..u32::MAX {
        let path = holder.join(stage_name(base, n));
        match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(path),
            Err(_) => return None,
            Ok(_) => {}
        }
    }
    None
}

/// THE STAGE `entry` WOULD BE RENAMED TO in `holder`, read and never acted on: the free stage
/// name ([`free_stage`]), while `entry` itself can be renamed: it is no link, it is on
/// `holder`'s volume (a volume mounted at it never moves), and this user may take its name out of
/// `holder` (`platform::may_take_name`: on macOS its own flags and ACL and its folder's, as the
/// rename itself is authorized). A leaf or tooling entry is planned, and its intent asked, only
/// while it is `Some`, so no plan lists, and no pass records, a removal no rename can start.
fn stage_for(entry: &Path, holder: &Path, base: &OsStr) -> Option<PathBuf> {
    let stage = free_stage(holder, base)?;
    let meta = std::fs::symlink_metadata(entry).ok()?;
    let movable = !meta.file_type().is_symlink()
        && same_volume(&meta, holder)
        && crate::platform::may_take_name(entry);
    movable.then_some(stage)
}

/// Whether the entry read as `meta` is on the volume of the folder `holder`.
#[cfg(unix)]
fn same_volume(meta: &std::fs::Metadata, holder: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata(holder).is_ok_and(|h| h.dev() == meta.dev())
}

/// Off Unix, every entry of a folder is taken to be on its volume.
#[cfg(not(unix))]
fn same_volume(_: &std::fs::Metadata, _: &Path) -> bool {
    true
}

/// The folders in `holder` named as stages from `base` ([`is_stage`]), sorted; never through a
/// link.
fn stages_in(holder: &Path, base: &OsStr) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(holder)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| is_stage(&e.file_name(), base) && e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    found.sort();
    found
}

/// Rename `from` to `to` only where nothing stands at `to` (`renameat` with `RENAME_EXCL` on
/// macOS, `RENAME_NOREPLACE` on Linux): `AlreadyExists` otherwise, and nothing is replaced.
#[cfg(unix)]
fn rename_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    aterm_dirfd::renameat_with(
        aterm_dirfd::CWD,
        from,
        aterm_dirfd::CWD,
        to,
        aterm_dirfd::RenameFlags::NOREPLACE,
    )
    .map_err(std::io::Error::from)
}

/// Off Unix, a rename to a name nothing stood at when looked at (a folder is never renamed over
/// another there).
#[cfg(not(unix))]
fn rename_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    if std::fs::symlink_metadata(to).is_ok() {
        return Err(std::io::ErrorKind::AlreadyExists.into());
    }
    std::fs::rename(from, to)
}

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

/// A build directory under the root the cache moved from ([`moved_from`]) goes whole once unwritten this long with no build holding its locks ([`reclaim_moved`]), and the root once empty. No build of this version writes there, so nothing is moved into the new root: cargo keeps absolute paths in a build directory (build scripts' output, dep-info), so a moved one would rebuild anyway, and new builds make the new root before a pass runs. Only a shim laid before the move, or a checkout's verify built from older code, still builds there; it holds cargo's locks while it does, and an hour quiet means it has stopped, at worst costing that checkout one fresh build in whichever root its code names.
pub const MOVED_AFTER: Duration = Duration::from_secs(60 * 60);

/// The shim's half, rendered ahead of a `targo`/`targo-tippy` shim's `exec` on macOS:
/// shell builtins only.
const SH_BLOCK: &str = r#"# aterm: build intermediates out of Spotlight
__atb_t=
__atb_c=${HOME-}/Library/Caches/aterm/.ubd/cargo-build.noindex
__atb= __atb_k= __atb_e= __atb_T= __atb_tf= __atb_tn= __atb_D= __atb_al= __atb_ab= __atb_q= __atb_v=
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
# aterm: a test run's scratch out of fseventsd's stream, in $TMPDIR's .ubd folder once one is this user's
case $__atb_v in
  test | t | bench)
    case ${TMPDIR-} in
      */.ubd/* | */.ubd) ;;
      /?*)
        __atb_q=${TMPDIR%/}/.ubd
        if [ -d "$__atb_q" ] && [ ! -h "$__atb_q" ] && [ -O "$__atb_q" ]; then TMPDIR=$__atb_q/; export TMPDIR; fi
        ;;
    esac
    ;;
esac
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
            root: None,
            cwd: cwd.as_deref(),
            env: &|name| std::env::var_os(name),
            read: &read_config,
            writable: &writable,
        },
    )
}

/// [`with_build_dir`] on this process's machine for a nested `targo` it starts at `cwd`, a
/// checkout it names: what the shim on `$PATH` hands a build typed there, in the cache root that
/// shim names ([`shim_root`]; Trust's `$CARGO` is the store's `targo` itself, past the shim);
/// `None` off macOS.
#[must_use]
pub fn with_build_dir_from(args: &[String], tool: Tool, cwd: &Path) -> Option<Vec<String>> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let home = aterm_types::dirs::home_dir();
    let root = home
        .as_deref()
        .map(|home| shim_root(home, &std::env::var_os("PATH").unwrap_or_default()));
    let m = Machine {
        home: home.as_deref(),
        root: root.as_deref(),
        cwd: None,
        env: &|name| std::env::var_os(name),
        read: &read_config,
        writable: &writable,
    };
    with_build_dir_at(args, tool, cwd, &m)
}

/// [`with_build_dir`] for a command started at `cwd`, a folder the caller names, on `m` (whose
/// own `cwd` it replaces). The inherited `$PWD` says where the caller was typed, not where this
/// command starts, so it is not read; a folder named through a link is left alone, as one reached
/// through a link is.
#[must_use]
pub fn with_build_dir_at(
    args: &[String],
    tool: Tool,
    cwd: &Path,
    m: &Machine<'_>,
) -> Option<Vec<String>> {
    if std::fs::canonicalize(cwd).ok()? != cwd {
        return None;
    }
    let env = |name: &str| if name == "PWD" { None } else { (m.env)(name) };
    with_build_dir(
        args,
        tool,
        &Machine {
            cwd: Some(cwd),
            env: &env,
            ..*m
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
    /// It is under the root the cache moved from ([`moved_from`]), which no build of this version writes, and nothing wrote it for `idle`, at least `after` ([`MOVED_AFTER`]): it goes whatever its workspace's state.
    Moved { idle: Duration, after: Duration },
    /// What an earlier pass renamed aside and did not finish removing.
    Unfinished,
    /// A different compiler target is waiting; this settled disposable generation must
    /// retire whole before another target can be created in the same package cache.
    PackageSuperseded,
    /// An explicitly disposable package generation exceeded its settled allocation budget,
    /// or its volume fell below the package disk floor. Its whole producer coordinator is
    /// held, its attempt is retired, and ordinary Cargo/process gates still apply.
    PackageBudget {
        allocated: u64,
        budget: u64,
        low_disk: bool,
    },
    /// NOT A REMOVAL WHOLE: what a removal ([`remove::tree`]) left of the stage it is returned
    /// beside, a build directory renamed aside or a stage of its parts — `stayed` entries of it
    /// would not go even once made removable (another user's, on another volume, or still
    /// refusing), and `freed` bytes of it went. It stays staged: the next pass records it
    /// ([`Unused::Unfinished`]) and tries what is left; [`remove::stayed_words`] names it.
    Stayed { stayed: usize, freed: u64 },
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

/// Removes the build directories under the cache `root` that nobody builds into or uses any
/// more, and returns those removed whole: one unwritten for [`RECLAIM_GONE`] whose workspace is
/// seen gone ([`checkout::gone`], `home` naming the folders it never looks into), and any
/// unwritten for [`RECLAIM_IDLE`]. A live process or unreadable ownership keeps its cache.
pub fn reclaim(root: &Path, home: Option<&Path>, now: SystemTime) -> Vec<PathBuf> {
    reclaim_watched(root, home, now, RECLAIM_GONE, None, &mut |_, _| true)
        .into_iter()
        .filter(|(_, why)| !matches!(why, Unused::Stayed { .. }))
        .map(|(path, _)| path)
        .collect()
}

/// [`reclaim`] as the window's disk watch runs it, each directory returned with why it went ([`Unused`]), and each stage a removal could not finish with what stayed of it ([`Unused::Stayed`]): a gone workspace's build directory goes once unwritten for `gone_after` ([`RECLAIM_GONE`] while the volume is only low, [`RECLAIM_GONE_UNDER_PRESSURE`] below the floor); and with `placing` (the home volume's device and a complete mount table), a workspace nested in a removed checkout is gone too ([`checkout::gone_placed`]).
pub fn reclaim_watched(
    root: &Path,
    home: Option<&Path>,
    now: SystemTime,
    gone_after: Duration,
    placing: Option<&checkout::Placing>,
    record: &mut dyn FnMut(&Path, Unused) -> bool,
) -> Vec<(PathBuf, Unused)> {
    reclaim_watched_on(
        root,
        home,
        now,
        gone_after,
        placing,
        record,
        &fence::Fence::default(),
    )
}

/// The sweep with an injected ownership probe for headless fixtures.
#[cfg(any(test, debug_assertions))]
pub fn reclaim_watched_with_processes(
    root: &Path,
    home: Option<&Path>,
    now: SystemTime,
    gone_after: Duration,
    placing: Option<&checkout::Placing>,
    record: &mut dyn FnMut(&Path, Unused) -> bool,
    processes: fence::Processes<'_>,
) -> Vec<(PathBuf, Unused)> {
    reclaim_watched_on(
        root,
        home,
        now,
        gone_after,
        placing,
        record,
        &fence::Fence::new(processes),
    )
}

fn reclaim_watched_on(
    root: &Path,
    home: Option<&Path>,
    now: SystemTime,
    gone_after: Duration,
    placing: Option<&checkout::Placing>,
    record: &mut dyn FnMut(&Path, Unused) -> bool,
    fence: &fence::Fence<'_>,
) -> Vec<(PathBuf, Unused)> {
    let mut removed = Vec::new();
    if std::fs::symlink_metadata(root).is_ok_and(|m| m.is_dir()) {
        let mut sweep = Sweep {
            root,
            home,
            now,
            gone_after,
            placing,
            moved: false,
            compiler_store: compiler_store(home),
            record,
            fence,
            removed: &mut removed,
        };
        sweep.walk(root, 0);
    }
    removed
}

/// THE ROOT THE CACHE MOVED FROM, emptied ([`MOVED_AFTER`]): each build directory under `root` that nothing wrote for [`MOVED_AFTER`] goes whole, its locks held as [`reclaim_watched`] holds them ([`Unused::Moved`]), whatever its workspace's state; nothing is pruned in part, and a build directory a build holds or wrote within the hour stays for a later pass. The root itself goes once nothing is left in it and it is as quiet, or this pass emptied it. `record` is asked before each removal, and a stage a removal could not finish is returned with what stayed of it ([`Unused::Stayed`]).
pub fn reclaim_moved(
    root: &Path,
    now: SystemTime,
    record: &mut dyn FnMut(&Path, Unused) -> bool,
) -> Vec<(PathBuf, Unused)> {
    let fence = fence::Fence::default();
    let mut removed = Vec::new();
    if std::fs::symlink_metadata(root).is_ok_and(|m| m.is_dir()) {
        let quiet = last_written(root).is_some_and(|t| age(now, t) >= MOVED_AFTER);
        let mut sweep = Sweep {
            root,
            home: None,
            now,
            gone_after: MOVED_AFTER,
            placing: None,
            moved: true,
            compiler_store: None,
            record,
            fence: &fence,
            removed: &mut removed,
        };
        let any = sweep.walk(root, 0);
        if quiet || any {
            let _ = std::fs::remove_dir(root);
        }
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
    /// `root` is the one the cache moved from ([`reclaim_moved`]): each build directory goes whole once [`MOVED_AFTER`] quiet, or stays whole.
    moved: bool,
    /// Independently admitted actual store; unknown keeps compiler-classified units.
    compiler_store: Option<PathBuf>,
    /// Asked before each removal, its locks held: `false` keeps it.
    record: &'a mut dyn FnMut(&Path, Unused) -> bool,
    fence: &'a fence::Fence<'a>,
    removed: &'a mut Vec<(PathBuf, Unused)>,
}

/// How deep [`reclaim`] walks the mirror of the filesystem.
const MAX_DEPTH: usize = 64;

/// One mirror folder `dir` under `root`: judge its `.cargo-build`, finish a removal an
/// earlier pass left, walk its other children, then remove `dir` itself if it ended empty
/// and is not brand new.
impl Sweep<'_> {
    /// Whether the stage at `path` may be cleared: a folder stands there and its intent is on
    /// record (staged bytes need one, even after a crash); its intent refused, or no folder,
    /// keeps it as it is.
    fn stage_recorded(&mut self, path: &Path, original: &Path) -> bool {
        std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir())
            && self.fence.clear(original, path)
            && (self.record)(path, Unused::Unfinished)
    }

    /// FINISH A STAGE an earlier pass left, once a pass, its intent on record
    /// ([`Self::stage_recorded`]): removed whole, it is returned as [`Unused::Unfinished`], so
    /// its outcome is on record beside its intent and its bytes are counted ([`Self::take`]);
    /// one a removal could not finish stays for the next pass to record and finish, costing
    /// only itself: every other stage of its folder is staged under a name of its own
    /// ([`stage_aside`], [`new_stage`]).
    fn finish_stage(&mut self, stage: &Path, original: &Path) -> bool {
        self.take(stage, stage.to_path_buf(), Unused::Unfinished, original)
    }

    /// REMOVE THE STAGE `stage` ([`remove::tree`], every removal a pass makes): removed whole,
    /// `went` is returned as gone for `why`, and `true`; one some entry of which stayed is
    /// returned as [`Unused::Stayed`] (what stayed, and the bytes that went) and stays staged for
    /// the next pass, and `false`.
    fn take(&mut self, stage: &Path, went: PathBuf, why: Unused, original: &Path) -> bool {
        if !self.fence.clear(original, stage) {
            return false;
        }
        let left = remove::tree(stage);
        if left.whole() {
            self.removed.push((went, why));
        } else {
            self.removed.push((
                stage.to_path_buf(),
                Unused::Stayed {
                    stayed: left.stayed,
                    freed: left.freed,
                },
            ));
        }
        left.whole()
    }

    /// WHAT NO BUILD READS AGAIN in the build directory `leaf` that stays ([`stale`]):
    /// surveyed without locks, then every cargo lock it holds taken (one a build holds keeps
    /// it all this pass), and the decision made again from a fresh survey under those locks;
    /// moved under the locks into a new stage of its own in `dir` ([`new_stage`] from
    /// [`DOOMED`], beside any stage an earlier pass left, which the walk finishes as a stage of
    /// its own), recorded as that stage ([`Unused::Stale`]; refused, every entry moves back),
    /// and removed with the locks let go.
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
        if !self.fence.clear(leaf, leaf) {
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
        if !self.fence.clear(leaf, leaf) {
            return false;
        }
        let Ok(stage) = new_stage(dir, OsStr::new(DOOMED)) else {
            return false;
        };
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
            // A new process may have started from a staged image while the intent was
            // refused. Keep that stage for a later recorded pass rather than move its image.
            if !self.fence.clear(leaf, &stage) {
                drop(held);
                return false;
            }
            for (from, to) in moved.iter().rev() {
                let _ = std::fs::rename(to, from);
            }
            let _ = std::fs::remove_dir(&stage);
            drop(held);
            return false;
        }
        drop(held);
        self.take(&stage, stage.clone(), why, leaf);
        true
    }

    /// ONE MIRROR FOLDER `dir`, its children as one listing read them: its [`LEAF`] judged
    /// (taken whole, under a stage of its own, or pruned), each stage an earlier pass left
    /// ([`is_stage`]) finished once, every other folder walked; the stages this pass makes are not
    /// in that listing, so no stage is visited twice in a pass.
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
                let judged = if self.moved {
                    unused(&child, &|seen| moved(seen, now))
                } else {
                    workspace_of(root, dir).and_then(|ws| {
                        unused(&child, &|seen| {
                            whole(&ws, seen, home, now, self.gone_after, self.placing)
                        })
                    })
                };
                let Some((held, why)) = judged else {
                    if !self.moved {
                        any |= self.prune(dir, &child);
                    }
                    continue;
                };
                if stage_for(&child, dir, OsStr::new(DOOMED)).is_none()
                    || !self.fence.clear(&child, &child)
                    || !(self.record)(&child, why)
                {
                    drop(held);
                    continue;
                }
                if !self.fence.clear(&child, &child) {
                    drop(held);
                    continue;
                }
                let staged = stage_aside(&child, dir, OsStr::new(DOOMED));
                drop(held);
                if let Ok(stage) = staged {
                    any = true;
                    self.take(&stage, child.clone(), why, &child);
                }
            } else if is_stage(name, OsStr::new(DOOMED)) {
                let original = dir.join(LEAF);
                if self.stage_recorded(&child, &original) {
                    any |= self.finish_stage(&child, &original);
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

/// `leaf`'s build locks, held, and why it goes ([`Unused`]), when nobody builds into the
/// build directory `leaf` any more: `judge` says why it goes from what [`look`] sees (idle
/// long enough), no build holds a lock, and nothing was written since it was first looked at.
fn unused(
    leaf: &Path,
    judge: &dyn Fn(&Seen) -> Option<Unused>,
) -> Option<(Vec<crate::lock::Flock>, Unused)> {
    let seen = look(leaf)?;
    let why = judge(&seen)?;
    let held = hold(&seen.locks)?;
    let again = look(leaf)?;
    (again.newest == seen.newest && again.locks == seen.locks).then_some((held, why))
}

/// Why a build directory under the root the cache moved from, as [`look`] `seen` it, goes
/// whole: nothing wrote it for [`MOVED_AFTER`].
fn moved(seen: &Seen, now: SystemTime) -> Option<Unused> {
    let idle = age(now, seen.newest);
    (idle >= MOVED_AFTER).then_some(Unused::Moved {
        idle,
        after: MOVED_AFTER,
    })
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

/// How deep [`remove_unbuilt`] looks for cargo's locks: a cells lane builds
/// `<cell>/<triple>/<profile>` under its target directory.
const UNBUILT_DEPTH: usize = 5;

/// A cargo target or build directory `dir` that a tool of this repository no longer builds in,
/// removed whole once no build holds any of its cargo locks ([`CARGO_LOCKS`], up to
/// [`UNBUILT_DEPTH`] folders down, never through a link): every lock taken without waiting and
/// looked for again, then `dir` renamed aside, never over another entry, to the lowest free
/// stage name from `<dir>.removing` (`<dir>.removing`, then `<dir>.removing-<n>`:
/// [`stage_aside`]) and removed under them. Builds resolving the original path after its rename
/// get a fresh directory. Each folder an earlier call left aside (every stage name from
/// `<dir>.removing`, the one fixed name an older aterm used included) is removed first, each on
/// its own: one that keeps an entry costs only itself, and `dir` is judged and removed all the
/// same. Each removal is [`remove::tree`]'s, which no one entry stops. A caller whose `dir` is
/// gone calls it all the same to finish what earlier calls left aside. `Ok(true)`: `dir` went
/// whole, and nothing aside stayed. `Ok(false)`: nothing aside stayed, and `dir` is no directory
/// or a build holds a lock (or one is no regular file, or one came meanwhile), or a process uses
/// it or cannot be ruled out, and it stays.
///
/// # Errors
/// A directory scan, an entry's metadata or the rename failed; or some entry of an aside stayed,
/// the earlier calls' or the one `dir` was renamed to, whether or not `dir` itself went: the
/// error names each such aside ([`remove::stayed_words`], joined by `; `), the rest of it went,
/// and the next call tries it again.
pub fn remove_unbuilt(dir: &Path) -> std::io::Result<bool> {
    remove_unbuilt_on(dir, &fence::Fence::default())
}

/// Remove an old target with an ownership probe supplied by a headless fixture.
///
/// # Errors
/// The same directory, metadata, rename and incomplete-removal errors as [`remove_unbuilt`].
#[cfg(any(test, debug_assertions))]
pub fn remove_unbuilt_with_processes(
    dir: &Path,
    processes: fence::Processes<'_>,
) -> std::io::Result<bool> {
    remove_unbuilt_on(dir, &fence::Fence::new(processes))
}

fn remove_unbuilt_on(dir: &Path, fence: &fence::Fence<'_>) -> std::io::Result<bool> {
    let Some(name) = dir.file_name() else {
        return Ok(false);
    };
    let holder = dir
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut base = name.to_os_string();
    base.push(".removing");
    let mut stayed = Vec::new();
    let mut held_by_process = false;
    let mut gone = |aside: &Path| {
        if !fence.clear(dir, aside) {
            held_by_process = true;
            return false;
        }
        let left = remove::tree(aside);
        if !left.whole() {
            stayed.push(remove::stayed_words(aside, left.stayed));
        }
        left.whole()
    };
    for aside in stages_in(holder, &base) {
        gone(&aside);
    }
    let went = 'went: {
        if !std::fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir()) {
            break 'went false;
        }
        if !fence.clear(dir, dir) {
            break 'went false;
        }
        let locks = locks_under(dir)?;
        let Some(held) = hold(&locks) else {
            break 'went false;
        };
        if locks_under(dir)? != locks || !fence.clear(dir, dir) {
            break 'went false;
        }
        let aside = stage_aside(dir, holder, &base)?;
        let whole = gone(&aside);
        drop(held);
        whole
    };
    if stayed.is_empty() {
        Ok(went && !held_by_process)
    } else {
        Err(std::io::Error::other(stayed.join("; ")))
    }
}

/// Cargo's locks under `dir` to [`UNBUILT_DEPTH`] folders down, sorted; no link is followed.
fn locks_under(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut locks = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), 0)];
    while let Some((at, depth)) = stack.pop() {
        for entry in std::fs::read_dir(&at)? {
            let entry = entry?;
            if CARGO_LOCKS.iter().any(|lock| entry.file_name() == *lock) {
                // Keep every lock pathname for `hold` to judge, including directories and
                // links: a nonregular lock must retain the tree rather than disappear here.
                locks.push(entry.path());
            } else if entry.file_type()?.is_dir() && depth < UNBUILT_DEPTH {
                stack.push((entry.path(), depth + 1));
            }
        }
    }
    locks.sort();
    Ok(locks)
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

/// [`reclaim_watched`] over this user's cache, now, then [`reclaim_moved`] over the root it moved from and [`tooling::reclaim`] over its tooling cache, `record` asked before each removal; nothing off macOS or without a home.
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
        removed.extend(reclaim_moved(&moved_from(&home), now, record));
        removed.extend(tooling::reclaim(&tooling::root(&home), now, record));
        removed.extend(registered::reclaim(&home, now, gone_after, placing, record));
        removed
    })
}

/// [`reclaim`] over this user's cache, now, each path returned with why it went; nothing off macOS or without a home.
#[must_use]
pub fn reclaim_home() -> Vec<(PathBuf, Unused)> {
    reclaim_home_watched(RECLAIM_GONE, None, &mut |_, _| true)
}

/// An advisory cleanup candidate from [`plan`]: a whole build directory, its stale parts,
/// or a stage an earlier pass left. Cleanup rechecks eligibility and active use before
/// removing anything; this observation does not promise that its footprint will be freed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    /// The build directory, or the stage.
    pub path: PathBuf,
    /// The observed reason for considering it.
    pub why: Unused,
    /// Candidate paths whose footprint is reported: `path` itself, or its stale parts.
    pub paths: Vec<PathBuf>,
    /// Cargo locks could not all be taken when checked. False does not rule out active
    /// process use or unknown ownership; stages and tooling do not probe Cargo locks here.
    pub held: bool,
}

/// Advisory cleanup candidates in `root` at `now`, including observed Cargo lock failures.
/// Process ownership is not probed here. [`reclaim_watched`] rechecks eligibility and active
/// use before removal. Nothing is moved or removed and no stage is finished by this read.
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
        let pass = (
            root,
            home,
            now,
            gone_after,
            placing,
            store.as_deref(),
            false,
        );
        plan_walk(pass, root, 0, &mut planned);
    }
    planned
}

/// Advisory candidates in the root the cache moved from, `root`, at `now`. As in [`plan`],
/// process ownership is checked by cleanup, not this read-only observation.
#[must_use]
pub fn plan_moved(root: &Path, now: SystemTime) -> Vec<Planned> {
    let mut planned = Vec::new();
    if std::fs::symlink_metadata(root).is_ok_and(|m| m.is_dir()) {
        let pass = (root, None, now, MOVED_AFTER, None, None, true);
        plan_walk(pass, root, 0, &mut planned);
    }
    planned
}

/// Advisory candidates for this user's current, moved, tooling and registered caches, with
/// the day's age window ([`reclaim_home`]'s). Active use is checked by cleanup. Nothing is
/// listed off macOS or without a home.
#[must_use]
pub fn plan_home() -> Vec<Planned> {
    if !cfg!(target_os = "macos") {
        return Vec::new();
    }
    aterm_types::dirs::home_dir().map_or_else(Vec::new, |home| {
        let now = SystemTime::now();
        let mut planned = plan(&cache_root(&home), Some(&home), now, RECLAIM_GONE, None);
        planned.extend(plan_moved(&moved_from(&home), now));
        planned.extend(tooling::plan(&tooling::root(&home), now));
        planned.extend(registered::plan(&home, now));
        planned
    })
}

/// What one [`plan`] judges by: its `root`, `home`, `now`, `gone_after`, `placing`, the
/// compiler store, and whether `root` is the one the cache moved from ([`plan_moved`]).
type PlanPass<'a> = (
    &'a Path,
    Option<&'a Path>,
    SystemTime,
    Duration,
    Option<&'a checkout::Placing>,
    Option<&'a Path>,
    bool,
);

/// [`plan`]'s walk of the mirror folder `dir`, as [`Sweep::walk`] walks it: each stage an
/// earlier pass left listed once, its leaf nominated whole only while it can be staged
/// ([`stage_for`]), and its stale parts only while a stage can be made beside it
/// ([`free_stage`]).
fn plan_walk(pass: PlanPass<'_>, dir: &Path, depth: usize, planned: &mut Vec<Planned>) {
    let (root, home, now, gone_after, placing, store, was_moved) = pass;
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
            if free_stage(dir, OsStr::new(DOOMED)).is_none() {
                continue;
            }
            let Some(seen) = look(&child) else {
                continue;
            };
            let held = hold(&seen.locks).is_none();
            let goes = if was_moved {
                moved(&seen, now)
            } else {
                workspace_of(root, dir)
                    .and_then(|ws| whole(&ws, &seen, home, now, gone_after, placing))
            };
            if let Some(why) = goes {
                if stage_for(&child, dir, OsStr::new(DOOMED)).is_some() {
                    planned.push(Planned {
                        path: child.clone(),
                        why,
                        paths: vec![child],
                        held,
                    });
                }
                continue;
            }
            if was_moved {
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
        } else if is_stage(name, OsStr::new(DOOMED)) {
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

/// The bytes paths hold on disk: one definition with the capacity ledger's footprints.
pub use aterm_capacity::footprint::allocated;

/// The workspaces whose build directories the cache under `root` holds; a stage ([`is_stage`])
/// is never looked into.
#[must_use]
pub fn workspaces(root: &Path) -> Vec<PathBuf> {
    fn walk(root: &Path, dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            if !e.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            if e.file_name() == LEAF {
                out.extend(workspace_of(root, dir));
            } else if depth < MAX_DEPTH && !is_stage(&e.file_name(), OsStr::new(DOOMED)) {
                walk(root, &e.path(), depth + 1, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, 0, &mut out);
    out
}

#[cfg(test)]
#[path = "build_cache_tests.rs"]
mod tests;
