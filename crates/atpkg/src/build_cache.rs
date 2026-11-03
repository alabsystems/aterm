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

#[path = "build_cache_inventory.rs"]
mod inventory;

/// THE TESTS' CALL INTO A PASS between an entry's intent on record and its rename aside (the
/// leaf's in [`Sweep::walk`], a tooling entry's in [`tooling::reclaim`]), with the entry's path:
/// where a test sets what refuses that rename after the check. Unix test builds alone: only this
/// crate's Unix tests call it.
#[cfg(all(test, unix))]
pub(crate) mod seams {
    use std::cell::RefCell;
    use std::path::Path;

    type Hook = Box<dyn FnMut(&Path)>;

    thread_local! {
        static STAGING: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }

    /// Call `then` with the entry's path each time a pass on this thread is about to rename an
    /// entry aside, its intent on record.
    pub(crate) fn on_staging(then: impl FnMut(&Path) + 'static) {
        STAGING.with(|h| *h.borrow_mut() = Some(Box::new(then)));
    }

    /// Forget the call of this thread.
    pub(crate) fn clear() {
        STAGING.with(|h| *h.borrow_mut() = None);
    }

    pub(super) fn staging(entry: &Path) {
        let call = STAGING.with(|h| h.borrow_mut().take());
        if let Some(mut call) = call {
            call(entry);
            STAGING.with(|h| {
                let mut slot = h.borrow_mut();
                if slot.is_none() {
                    *slot = Some(call);
                }
            });
        }
    }
}

#[cfg(not(all(test, unix)))]
mod seams {
    pub(super) const fn staging(_: &std::path::Path) {}
}

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

/// STAGE `from` ASIDE WHILE ITS CUSTODY ALLOWS: renamed aside in `holder` ([`stage_aside`]),
/// then custody read again by path at the stage and at every folder above it
/// ([`registered::retention::allows_tree`]). A rename acts by name, so a keep published at `from`
/// (or a marker there that cannot be read) after the custody read that let it be staged, and
/// before the rename acted, moved with it: it is put back at `from`, never over another entry,
/// and `None` returned, so it stands where it stood. Where something took `from` meanwhile, the
/// stage is returned all the same, and the removal that takes it reads its custody before
/// anything of it goes ([`remove::tree_while`]).
fn stage_unkept(from: &Path, holder: &Path, base: &OsStr) -> std::io::Result<Option<PathBuf>> {
    let stage = stage_aside(from, holder, base)?;
    staged::at(&stage);
    if !registered::retention::allows_tree(&stage) && rename_noreplace(&stage, from).is_ok() {
        return Ok(None);
    }
    Ok(Some(stage))
}

/// THE TESTS' HOOK just after entries were renamed into a stage ([`stage_unkept`], a prune's
/// parts), before custody is read again: a test publishes a keep at that instant, on this
/// thread. Unix test builds alone: only this crate's Unix tests set it.
#[cfg(all(test, unix))]
pub(crate) mod staged {
    use std::cell::RefCell;
    use std::path::Path;

    type Hook = Box<dyn FnMut(&Path)>;

    thread_local! {
        static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }

    /// Call `then` with each stage just filled on this thread, until [`clear`].
    pub(crate) fn on(then: impl FnMut(&Path) + 'static) {
        HOOK.with(|hook| *hook.borrow_mut() = Some(Box::new(then)));
    }

    /// No hook on this thread.
    pub(crate) fn clear() {
        HOOK.with(|hook| *hook.borrow_mut() = None);
    }

    pub(super) fn at(stage: &Path) {
        HOOK.with(|hook| {
            if let Some(then) = hook.borrow_mut().as_mut() {
                then(stage);
            }
        });
    }
}

#[cfg(not(all(test, unix)))]
mod staged {
    pub(super) const fn at(_: &std::path::Path) {}
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
/// rename itself is authorized; on Linux its folder's write bit, its own and its folder's
/// immutable and append-only attributes, and a sticky folder's owners). A leaf or tooling entry
/// is planned, and its intent asked, only while it is `Some`, so no plan lists, and no pass
/// records, a removal no rename can start. What changes after this reading (a flag, an ACL entry
/// or a folder mode set since, a volume mounted there, or an attribute no reading here sees) can
/// still refuse the rename once the intent is on record: that intent closes with
/// [`Unused::Refused`] ([`Intents`]), the entry left as it was.
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
__atb= __atb_k= __atb_e= __atb_T= __atb_tf= __atb_tn= __atb_D= __atb_al= __atb_ab= __atb_q= __atb_v= __atb_r=
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
else
  [ -n "$__atb_D" ] || __atb_D=$__atb_c'{workspace-root}/.cargo-build'
  [ -n "$__atb_e" ] || set -- --config "build.build-dir='$__atb_D'" "$@"
  # Only an external frontend loses those words, and the trust lane reads only the variable.
  # Anything else keeps them, aliases included, and the variable would reach its build scripts,
  # where a nested cargo on this workspace would wait forever on the lock this build holds. The
  # verb alone is matched, never joined to a flag, so no alias's name can fall into an arm.
  case $__atb_v in
    tippy | clippy | trust)
      CARGO_BUILD_BUILD_DIR=$__atb_D
      export CARGO_BUILD_BUILD_DIR
      ;;
  esac
fi
# aterm: descriptors past a stock shell's soft limit: a test binary's threads pass
# 256 together, so a run would fail `Too many open files` on the caller's limit
# alone. Raise only the soft limit, toward the floor, and only for a verb that
# builds or runs code (the scan above classified it, aliases included): reading
# a limit is a fork on this shell, so fmt/metadata/help invocations skip it. The
# kernel refuses a raise past the hard limit, and only then is the hard limit
# read. Digit strings are normalized and length-checked before any arithmetic:
# a shell's own rendering of an unlimited value need not fit its signed integers.
# The tippy shim needs no scan to know it builds: its name is baked in above.
case $__atb_t in tippy) __atb_r=1 ;; esac
case $__atb_v in
  build | b | check | c | test | t | run | r | bench | doc | d | rustc | rustdoc | fix | clippy | tippy | trust) __atb_r=1 ;;
  *) [ -n "$__atb_ab" ] && __atb_r=1 ;;
esac
if [ -n "$__atb_r" ]; then
  __atb_z=$(ulimit -Sn 2>/dev/null) || __atb_z=
  case $__atb_z in
    '' | *[!0-9]*) ;;
    *)
      while [ "${#__atb_z}" -gt 1 ]; do case $__atb_z in 0*) __atb_z=${__atb_z#0} ;; *) break ;; esac; done
      if [ "${#__atb_z}" -le 15 ] && [ "$__atb_z" -lt @ATB_NOFILE_FLOOR@ ] && ! ulimit -Sn @ATB_NOFILE_FLOOR@ 2>/dev/null; then
        __atb_y=$(ulimit -Hn 2>/dev/null) || __atb_y=
        case $__atb_y in
          '' | *[!0-9]*) ;;
          *)
            while [ "${#__atb_y}" -gt 1 ]; do case $__atb_y in 0*) __atb_y=${__atb_y#0} ;; *) break ;; esac; done
            if [ "${#__atb_y}" -le 15 ] && [ "$__atb_z" -lt "$__atb_y" ]; then ulimit -Sn "$__atb_y" 2>/dev/null || :; fi
            ;;
        esac
      fi
      ;;
  esac
fi
"#;

/// The block a shim forwarding to the store file `name` carries: `targo`'s, or `targo-tippy`'s, which names no subcommand (it is the lint).
#[must_use]
pub fn shim_block(name: &str) -> Option<String> {
    // The one descriptor floor, spelled into the block from its owning const
    // (`aterm_uds::spawnfd::SOFT_NOFILE_FLOOR`) so the script and the Rust
    // raises can never drift apart.
    let floor = aterm_uds::spawnfd::SOFT_NOFILE_FLOOR.to_string();
    match name {
        "targo" => Some(SH_BLOCK.replace("@ATB_NOFILE_FLOOR@", &floor)),
        "targo-tippy" => Some(
            SH_BLOCK
                .replacen("\n__atb_t=\n", "\n__atb_t=tippy\n", 1)
                .replace("@ATB_NOFILE_FLOOR@", &floor),
        ),
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

/// WHY A BUILD DIRECTORY GOES ([`reclaim`], [`reclaim_watched`]): what its intent row, its removal row and the watch's account of it say — each rule [`reclaim`] takes one by, with the figures it was taken on; and, returned after an intent, what became of it when it did not go whole ([`Unused::Stayed`], [`Unused::Revoked`], [`Unused::Refused`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unused {
    /// An explicit request names this genuine registered ordinary cache. Its current
    /// identity, complete Cargo locks and process-use gates still hold before retirement.
    Requested,
    /// Its workspace is seen gone ([`checkout::gone_placed`]) and nothing wrote it for `idle`, at least the pass's window `after` ([`RECLAIM_GONE`], or [`RECLAIM_GONE_UNDER_PRESSURE`] below the disk watch's floor).
    Gone { idle: Duration, after: Duration },
    /// Nothing wrote it for `idle`, [`RECLAIM_IDLE`] or more: it goes whatever its workspace's state, which was not looked at — it may still be there.
    Idle { idle: Duration },
    /// It is under the root the cache moved from ([`moved_from`]), which no build of this version writes, and nothing wrote it for `idle`, at least `after` ([`MOVED_AFTER`]): it goes whatever its workspace's state.
    Moved { idle: Duration, after: Duration },
    /// What an earlier pass renamed aside and did not finish removing. A dry plan forecasts what
    /// of it will stay, and why ([`Planned::stays`]).
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
    /// would not go even once made removable (another user's, on another volume, one whose
    /// attributes cannot be read, a file named outside it too, or still refusing:
    /// [`remove::Why`]), and `freed` bytes of it went. It stays staged: the next pass records it
    /// ([`Unused::Unfinished`]) and tries what is left; [`remove::stayed_words`] names what
    /// stayed, each with its reason. Only a removal returns it: a dry plan's forecast of what
    /// will stay is [`Planned::stays`].
    Stayed { stayed: usize, freed: u64 },
    /// Permission was revoked during removal. The retained remainder was not counted;
    /// only the exact allocated bytes already freed are known.
    Revoked { freed: u64 },
    /// NOT A REMOVAL AT ALL: an entry whose intent was on record and nothing of which went, `by`
    /// what refused it after the check that let its intent be asked ([`Refusal`]): its rename
    /// aside failed, or the pass stopped before its move, or, moved into a stage already, before
    /// its removal. It is the outcome that closes that intent, 0 bytes freed (every intent a pass
    /// records closes, [`Intents`]), returned beside the path that holds it now: the entry, which
    /// stays where it was and the next pass judges again, or its stage ([`Refusal::Staged`]),
    /// which stays staged and the next pass finishes. [`reclaim`] never returns it, and nothing
    /// counts it as gone ([`Unused::nothing_went`]).
    Refused { by: Refusal },
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

impl Unused {
    /// Whether it is returned for an entry nothing of which went ([`Unused::Refused`]): an
    /// outcome that closes its intent and that no count of what went or was freed takes in.
    #[must_use]
    pub const fn nothing_went(self) -> bool {
        matches!(self, Self::Refused { .. })
    }
}

/// WHAT REFUSED AN ENTRY whose intent was on record, before anything of it went
/// ([`Unused::Refused`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Its rename aside failed, with this error kind: a flag, an ACL entry or a folder mode set
    /// after the check (`PermissionDenied`), a volume mounted there (`CrossesDevices`,
    /// `ResourceBusy`), or an attribute the check does not read. It stays where it was.
    Rename(std::io::ErrorKind),
    /// The pass stopped before its move: a check made again once its intent was on record
    /// refused it (a process came to use it, or its locks or its registered receipt changed), or
    /// a step its move needs failed (a registered cache's stage or receipt not written). It
    /// stays where it was.
    Stopped,
    /// The pass stopped once it was in a stage (moved aside by this pass, or a stage an earlier
    /// pass left), before its removal: a check made again refused it, as for
    /// [`Refusal::Stopped`]. It stays staged, returned as its stage's path, and the next pass
    /// finishes that stage.
    Staged,
}

impl Refusal {
    /// The one word a ledger row or a printer says it by: the rename's error kind as `std::io`
    /// names it (`PermissionDenied`, `CrossesDevices`, …), `Stopped` or `Staged`; never a path
    /// or an error's message.
    #[must_use]
    pub fn word(self) -> String {
        match self {
            Self::Rename(kind) => format!("{kind:?}"),
            Self::Stopped => "Stopped".to_owned(),
            Self::Staged => "Staged".to_owned(),
        }
    }

    /// What it says of the entry and of what the next pass does with it, in the words
    /// `aterm pkg machine cache`, the disk ledger and a run's relief line say it by.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::Rename(_) => {
                "its rename aside was refused after its check (a flag, an ACL entry or a folder \
                 mode set since, or a volume mounted there); it stays where it was, and the next \
                 pass judges it again"
            }
            Self::Stopped => {
                "the pass stopped before its move (a process came to use it, or what it was \
                 judged by changed, once its intent was on record); it stays where it was, and \
                 the next pass judges it again"
            }
            Self::Staged => {
                "the pass stopped once it was moved aside, before its removal (a process came to \
                 use it, or what it was judged by changed, once its intent was on record); it \
                 stays in its stage, and the next pass finishes the stage"
            }
        }
    }
}

/// A PASS'S INTENTS AND THEIR OUTCOMES: `record` asked before each removal ([`Intents::ask`]),
/// and what is returned for each intent it put on record: what went, what stayed of it, or,
/// when nothing of it went, [`Unused::Refused`]. An intent on record always closes with an
/// outcome: a pass asks for and finishes one entry at a time (every outcome follows its own
/// intent, before the next is asked), so an intent still open when the next is asked, or when
/// the pass ends, is closed with nothing gone ([`Intents::stop`]); a step that knows why
/// closes it so ([`Intents::refused`]). Where the entry is matters to that outcome: once it is
/// in a stage ([`Intents::ask_staged`], [`Intents::moved`]), a stop returns the stage's path
/// ([`Refusal::Staged`]), never the entry's, which no longer holds it.
struct Intents<'r> {
    record: &'r mut dyn FnMut(&Path, Unused) -> bool,
    /// The intent on record whose outcome is not in yet: the path it was asked for, and the
    /// stage that holds it once it is staged.
    open: Option<(PathBuf, Option<PathBuf>)>,
    out: Vec<(PathBuf, Unused)>,
}

impl<'r> Intents<'r> {
    fn new(record: &'r mut dyn FnMut(&Path, Unused) -> bool) -> Self {
        Self {
            record,
            open: None,
            out: Vec::new(),
        }
    }

    /// Ask `record` for `path`'s removal for `why`, any intent still open closed first: `true`
    /// puts it on record, open until its outcome is in.
    fn ask(&mut self, path: &Path, why: Unused) -> bool {
        self.stop();
        let on = (self.record)(path, why);
        if on {
            self.open = Some((path.to_path_buf(), None));
        }
        on
    }

    /// [`Intents::ask`] for a stage, an entry already moved aside (by an earlier pass, or by
    /// this one before its intent): on record, a stop closes it as [`Refusal::Staged`].
    fn ask_staged(&mut self, stage: &Path, why: Unused) -> bool {
        let on = self.ask(stage, why);
        self.moved(stage);
        on
    }

    /// The entry of the intent on record is now in `stage`: a stop from here closes it as
    /// staged, at `stage`.
    fn moved(&mut self, stage: &Path) {
        if let Some((_, staged)) = self.open.as_mut() {
            *staged = Some(stage.to_path_buf());
        }
    }

    /// The outcome of the intent on record: `path` (the entry, or the stage of it that stayed)
    /// returned for `why`.
    fn outcome(&mut self, path: PathBuf, why: Unused) {
        self.open = None;
        self.out.push((path, why));
    }

    /// Close the intent on record, if one is open, with nothing of it gone, refused `by`, at
    /// the path that holds it now (its stage once staged).
    fn refused(&mut self, by: Refusal) {
        if let Some((path, staged)) = self.open.take() {
            self.out
                .push((staged.unwrap_or(path), Unused::Refused { by }));
        }
    }

    /// The pass stopped before the intent on record went: closed with nothing of it gone, as
    /// [`Refusal::Staged`] at its stage once it is staged, as [`Refusal::Stopped`] where it was
    /// before.
    fn stop(&mut self) {
        let staged = self
            .open
            .as_ref()
            .is_some_and(|(_, staged)| staged.is_some());
        self.refused(if staged {
            Refusal::Staged
        } else {
            Refusal::Stopped
        });
    }

    /// Every outcome, in the order their intents were recorded, the last intent closed.
    fn finish(mut self) -> Vec<(PathBuf, Unused)> {
        self.stop();
        self.out
    }
}

/// Removes the build directories under the cache `root` that nobody builds into or uses any
/// more, and returns those removed whole: one unwritten for [`RECLAIM_GONE`] whose workspace is
/// seen gone ([`checkout::gone`], `home` naming the folders it never looks into), and any
/// unwritten for [`RECLAIM_IDLE`]. A live process or unreadable ownership keeps its cache.
pub fn reclaim(root: &Path, home: Option<&Path>, now: SystemTime) -> Vec<PathBuf> {
    reclaim_watched(root, home, now, RECLAIM_GONE, None, &mut |_, _| true)
        .into_iter()
        .filter(|(_, why)| {
            !why.nothing_went() && !matches!(why, Unused::Stayed { .. } | Unused::Revoked { .. })
        })
        .map(|(path, _)| path)
        .collect()
}

/// [`reclaim`] as the window's disk watch runs it, `record` asked before each removal (`false` keeps it), each directory returned with why it went ([`Unused`]), each stage a removal could not finish with what stayed of it ([`Unused::Stayed`]), and each entry whose intent `record` put on record and nothing of which went (its rename aside refused after the check, or the pass stopped before its move, or, moved aside, before its removal, returned as its stage: [`Refusal`]) with [`Unused::Refused`], so every intent on record closes with an outcome ([`Intents`]): a gone workspace's build directory goes once unwritten for `gone_after` ([`RECLAIM_GONE`] while the volume is only low, [`RECLAIM_GONE_UNDER_PRESSURE`] below the floor); and with `placing` (the home volume's device and a complete mount table), a workspace nested in a removed checkout is gone too ([`checkout::gone_placed`]).
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
    let mut intents = Intents::new(record);
    if std::fs::symlink_metadata(root).is_ok_and(|m| m.is_dir()) {
        let mut sweep = Sweep {
            root,
            home,
            now,
            gone_after,
            placing,
            moved: false,
            compiler_store: compiler_store(home),
            intents: &mut intents,
            fence,
            permitted: &|| true,
        };
        sweep.walk(root, 0);
    }
    intents.finish()
}

/// THE ROOT THE CACHE MOVED FROM, emptied ([`MOVED_AFTER`]): each build directory under `root` that nothing wrote for [`MOVED_AFTER`] goes whole, its locks held as [`reclaim_watched`] holds them ([`Unused::Moved`]), whatever its workspace's state; nothing is pruned in part, and a build directory a build holds or wrote within the hour stays for a later pass. The root itself goes once nothing is left in it and it is as quiet, or this pass emptied it. `record` is asked before each removal, a stage a removal could not finish is returned with what stayed of it ([`Unused::Stayed`]), and an entry whose intent is on record and nothing of which went with [`Unused::Refused`].
pub fn reclaim_moved(
    root: &Path,
    now: SystemTime,
    record: &mut dyn FnMut(&Path, Unused) -> bool,
) -> Vec<(PathBuf, Unused)> {
    let fence = fence::Fence::default();
    let mut intents = Intents::new(record);
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
            intents: &mut intents,
            fence: &fence,
            permitted: &|| true,
        };
        let any = sweep.walk(root, 0);
        if quiet || any {
            let _ = std::fs::remove_dir(root);
        }
    }
    intents.finish()
}

/// One [`reclaim`] pass: where, when, and what went.
struct Sweep<'a, 'r> {
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
    /// Its `record`, asked before each removal, its locks held (`false` keeps it), and the
    /// outcome that closes each intent it put on record.
    intents: &'a mut Intents<'r>,
    fence: &'a fence::Fence<'a>,
    /// Additional authority checked at every deletion boundary for registered roots.
    permitted: &'a dyn Fn() -> bool,
}

/// How deep [`reclaim`] walks the mirror of the filesystem.
const MAX_DEPTH: usize = 64;

/// One mirror folder `dir` under `root`: judge its `.cargo-build`, finish a removal an
/// earlier pass left, walk its other children, then remove `dir` itself if it ended empty
/// and is not brand new.
impl Sweep<'_, '_> {
    /// Whether the stage at `path` may be cleared: a folder stands there and its intent is on
    /// record (staged bytes need one, even after a crash); its intent refused, or no folder,
    /// keeps it as it is.
    fn stage_recorded(&mut self, path: &Path, original: &Path) -> bool {
        std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir())
            && self.fence.clear(original, path)
            && self.intents.ask_staged(path, Unused::Unfinished)
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

    /// REMOVE THE STAGE `stage` ([`remove::tree`], every removal a pass makes), its intent on
    /// record: removed whole, `went` is returned as gone for `why`, and `true`; one some entry
    /// of which stayed is returned as [`Unused::Stayed`] (what stayed, and the bytes that went)
    /// and stays staged for the next pass, and `false`. Custody is read by path above the stage,
    /// and in the stage by the walk itself, through the folder it holds
    /// ([`registered::retention::allows_walk`]). A search denial is lifted only after a complete
    /// fresh name scan proves that no custody marker is hidden there; hidden or unreadable
    /// custody leaves the folder unchanged. A stage whose walk is stopped once begun (by a keep
    /// the walk reads in the stage, by one a read by path finds at `original` or above the stage
    /// at a boundary of the walk, or by the pass's authority withdrawn there) is returned as
    /// [`Unused::Revoked`] with the bytes that went. One refused before its walk (by a keep a
    /// read by path finds at `original` or above the stage, or by the pass's authority or fence,
    /// a process come to use it since its intent) closes its intent with nothing of it gone, at
    /// `stage` ([`Refusal::Staged`]), and `false`: it stays staged as it stood, for the next pass
    /// to finish.
    fn take(&mut self, stage: &Path, went: PathBuf, why: Unused, original: &Path) -> bool {
        if !(self.permitted)()
            || !registered::retention::allows_tree(original)
            || !registered::retention::allows_walk(stage)
            || !self.fence.clear(original, stage)
        {
            self.intents.stop();
            return false;
        }
        let permitted = || {
            (self.permitted)()
                && registered::retention::allows_tree(original)
                && registered::retention::allows_walk(stage)
        };
        let left = match remove::tree_while(stage, &permitted) {
            Ok(left) => left,
            Err(revoked) => {
                self.intents.outcome(
                    stage.to_path_buf(),
                    Unused::Revoked {
                        freed: revoked.freed,
                    },
                );
                return false;
            }
        };
        if left.whole() {
            self.intents.outcome(went, why);
        } else {
            self.intents.outcome(
                stage.to_path_buf(),
                Unused::Stayed {
                    stayed: left.stayed,
                    freed: left.freed,
                },
            );
        }
        left.whole()
    }

    /// WHAT NO BUILD READS AGAIN in the build directory `leaf` that stays ([`stale`]):
    /// surveyed without locks, then every cargo lock it holds taken (one a build holds keeps
    /// it all this pass), and the decision made again from a fresh survey under those locks;
    /// moved under the locks into a new stage of its own in `dir` ([`new_stage`] from
    /// [`DOOMED`], beside any stage an earlier pass left, which the walk finishes as a stage of
    /// its own), while custody read by path at `leaf` and above allows it, read again once they
    /// moved, recorded as that stage ([`Unused::Stale`]; refused, or kept by custody read again,
    /// every entry moves back, never over another entry, and one whose path was taken meanwhile
    /// stays staged for a later pass, which finishes it only while custody at `leaf` allows),
    /// and removed with the locks let go.
    fn prune(&mut self, dir: &Path, leaf: &Path) -> bool {
        if !(self.permitted)() || !registered::retention::allows_tree(leaf) {
            return false;
        }
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
            if !(self.permitted)() || !registered::retention::allows_tree(leaf) {
                break;
            }
            let before = moved.len();
            for path in &one.paths {
                let to = stage.join(moved.len().to_string());
                if rename_noreplace(path, &to).is_ok() {
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
        // A rename acts by name: a keep published at the leaf as its parts moved would lose
        // them, so custody is read there again once they moved, and one it keeps moves back.
        staged::at(&stage);
        if moved.is_empty()
            || !registered::retention::allows_tree(leaf)
            || !self.intents.ask_staged(&stage, why)
        {
            // A new process may have started from a staged image while the intent was
            // refused. Keep that stage for a later recorded pass rather than move its image.
            if !self.fence.clear(leaf, &stage) {
                drop(held);
                return false;
            }
            // Never over another entry: a part whose path something took meanwhile stays in
            // the stage, which a later pass finishes only while custody at `leaf` allows.
            for (from, to) in moved.iter().rev() {
                let _ = rename_noreplace(to, from);
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
    /// in that listing, so no stage is visited twice in a pass. A leaf whose rename aside is
    /// refused once its intent is on record (a flag, an ACL entry or a folder mode set since the
    /// check, a volume mounted there) is returned as [`Unused::Refused`] with the rename's error
    /// kind, whole where it was; one put back where it stood because a keep was published at it
    /// as it moved ([`stage_unkept`]), as [`Refusal::Stopped`]; one moved aside that a process
    /// came to use before its removal, as its stage ([`Refusal::Staged`]).
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
                if !(self.permitted)()
                    || !registered::retention::allows_tree(&child)
                    || stage_for(&child, dir, OsStr::new(DOOMED)).is_none()
                    || !self.fence.clear(&child, &child)
                    || !self.intents.ask(&child, why)
                {
                    drop(held);
                    continue;
                }
                if !(self.permitted)()
                    || !registered::retention::allows_tree(&child)
                    || !self.fence.clear(&child, &child)
                {
                    drop(held);
                    self.intents.stop();
                    continue;
                }
                seams::staging(&child);
                let staged = stage_unkept(&child, dir, OsStr::new(DOOMED));
                drop(held);
                match staged {
                    Ok(Some(stage)) => {
                        any = true;
                        self.intents.moved(&stage);
                        self.take(&stage, child.clone(), why, &child);
                    }
                    // Put back where it stood: a keep was published at it as it moved.
                    Ok(None) => self.intents.stop(),
                    Err(e) => self.intents.refused(Refusal::Rename(e.kind())),
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
        .map(|lock| crate::lock::Flock::try_lock(crate::metadata_io::open_regular(lock).ok()?).ok())
        .collect()
}

/// A cargo target or build directory `dir` that a tool of this repository no longer builds in,
/// removed whole once no build holds any of its cargo locks ([`CARGO_LOCKS`], found by a
/// complete bounded inventory, never through a link): every lock taken without waiting and
/// looked for again, then `dir` renamed aside, never over another entry, to the lowest free
/// stage name from `<dir>.removing` (`<dir>.removing`, then `<dir>.removing-<n>`:
/// [`stage_aside`]) and removed under them. Builds resolving the original path after its rename
/// get a fresh directory. Each folder an earlier call left aside (every stage name from
/// `<dir>.removing`, the one fixed name an older aterm used included) is removed first, each on
/// its own: one that keeps an entry costs only itself, and `dir` is judged and removed all the
/// same. Each removal is [`remove::tree_while`]'s, which no one entry stops. Custody is read
/// first, as a sweep reads a stage's: `dir` is renamed only while a custody read by its path
/// allows it at `dir` and at every folder above it, and that read is made again at the aside
/// just after the rename ([`stage_unkept`]), so a keep there (or a marker that cannot be read),
/// even one published as the rename acted, stays where it stands; an aside is removed only while
/// every folder above it allows, its own custody and what it holds read by the walk, and one a
/// keep stops stays. A caller whose `dir` is gone calls it all the same to finish what earlier
/// calls left aside. `Ok(true)`: `dir` went whole, and nothing aside stayed. `Ok(false)`: no
/// entry of an aside stayed, and either `dir` did not go (it is no directory, a build holds a
/// lock (or one is no regular file, or one came meanwhile), a process uses it or cannot be ruled
/// out, or custody keeps it, and it stays), or an aside, this call's or an earlier one's, is kept
/// untouched because a process uses it or cannot be ruled out.
///
/// # Errors
/// A directory scan, an entry's metadata or the rename failed; or some entry of an aside stayed,
/// the earlier calls' or the one `dir` was renamed to, whether or not `dir` itself went: the
/// error names each such aside ([`remove::stayed_words`], joined by `; `; one a keep above it
/// stopped as its removal's permission revoked by that folder's custody marker:
/// [`revoked_words`]), the rest of it went (all a walk took before a keep stopped it, where one
/// did), and the next call tries it again.
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
        match remove::tree_while(aside, &|| registered::retention::allows_walk(aside)) {
            Ok(left) if left.whole() => true,
            Ok(left) => {
                stayed.push(remove::stayed_words(aside, left.stayed));
                false
            }
            // A walk a keep stopped leaves the aside standing, what it did not walk with it.
            Err(_) => {
                stayed.push(revoked_words(aside));
                false
            }
        }
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
        if locks_under(dir)? != locks
            || !fence.clear(dir, dir)
            || !registered::retention::allows_tree(dir)
        {
            break 'went false;
        }
        let Some(aside) = stage_unkept(dir, holder, &base)? else {
            break 'went false;
        };
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

/// What [`remove_unbuilt`]'s readers are told of `aside`, whose removal custody stopped. Where a
/// folder above it keeps it (its custody read by its path: [`registered::retention::kept_by`]),
/// the walk's custody check above `aside` stopped it, so `aside` is said as one opaque root
/// whose removal's permission that folder's custody marker revoked
/// ([`remove::Why::PermissionRevoked`]), never an entry of it as refusing its removal; else what
/// stands of it, as a removal's readers name it ([`remove::stayed_words`]).
fn revoked_words(aside: &Path) -> String {
    match aside.parent().and_then(registered::retention::kept_by) {
        Some(at) => format!(
            "{} stayed: {}; the custody marker of {}, a folder above it, keeps it",
            aside.display(),
            remove::Why::PermissionRevoked.words(aside),
            at.display()
        ),
        None => remove::stayed_words(aside, 1),
    }
}

/// All Cargo locks under `dir`, sorted; an incomplete inventory refuses rather than omitting
/// deeper locks. No directory link is followed.
fn locks_under(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    inventory::scan(dir).map(|seen| seen.locks)
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

/// The newest directory modification time and all Cargo locks in one complete bounded
/// inventory. An unreadable, changing or oversized tree is unknown, never an idle witness.
fn look(leaf: &Path) -> Option<Seen> {
    inventory::scan(leaf).ok()
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
    /// For a stage an earlier pass left ([`Unused::Unfinished`]), what of it the removal will
    /// leave and why, read and never acted on ([`remove::forecast`], the first [`remove::NAMED`]
    /// named); `None` for anything else.
    pub stays: Option<remove::Forecast>,
}

impl Planned {
    /// A stage an earlier pass left, removed in place by the next pass ([`Unused::Unfinished`]),
    /// with the forecast of what of it will stay ([`Planned::stays`], [`remove::forecast`]).
    #[must_use]
    pub fn stage(stage: PathBuf) -> Self {
        let stays = remove::forecast(&stage, remove::NAMED);
        Self::staged(stage, stays)
    }

    /// A stage an earlier pass left, with `stays`, the forecast of what of it will stay by the
    /// rules of the removal that takes it (a registered pending stage's, for one).
    #[must_use]
    pub fn staged(stage: PathBuf, stays: remove::Forecast) -> Self {
        Self {
            path: stage.clone(),
            why: Unused::Unfinished,
            paths: vec![stage],
            held: false,
            stays: Some(stays),
        }
    }
}

/// Advisory cleanup candidates in `root` at `now`, including observed Cargo lock failures.
/// Process ownership is not probed here. [`reclaim_watched`] rechecks eligibility and active
/// use before removal. Nothing is moved or removed and no stage is finished by this read. Each
/// stage an earlier pass left is listed with the forecast of what of it the removal will leave
/// and why ([`Planned::stays`], [`remove::forecast`]), read without changing it.
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
/// earlier pass left listed once with the forecast of what of it will stay ([`Planned::stage`]),
/// its leaf nominated whole only while it can be staged ([`stage_for`]), and its stale parts only
/// while a stage can be made beside it ([`free_stage`]).
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
                        stays: None,
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
                stays: None,
            });
        } else if is_stage(name, OsStr::new(DOOMED)) {
            planned.push(Planned::stage(child));
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
