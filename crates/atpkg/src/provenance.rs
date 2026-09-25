// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `com.apple.provenance` — the extended attribute macOS stamps on every file a
//! provenance-TRACKED process writes, and the predicate every surface in aterm uses to
//! see it.
//!
//! # What was measured (2026-09-12, macOS 26 / Darwin 25.6, this repo's
//! `scratchpad/provenance/probe.sh` + `probe2.sh`)
//!
//! * A process is tracked when its EXECUTABLE carries the tag, when the bundle the kernel
//!   charges it to carries it — the outermost `.app` above the executable, or failing
//!   one the `X.<ext>` wrapper around `Contents/MacOS/` (measured 2026-09-23) — or when
//!   its PARENT is tracked. Everything a tracked process creates or touches is tagged:
//!   `open(O_CREAT)`, but also `rename`, `link`, `chmod`, `utimes`, `setxattr` and an
//!   append — a tracked parent that merely renames a clean file tags it. Reads, `stat` and `fsync` do not.
//! * The tag is inherited across `fork`, `posix_spawn`, `setsid`, `osascript`,
//!   `launchctl asuser`, and it SURVIVES `exec` into an untagged image: a tagged
//!   `#!/bin/sh` shim that `exec`s a clean tool still produces tagged output. The shims
//!   atpkg lays in `bin/` are exactly that shape.
//! * A job launchd spawns (`launchctl submit`) is NOT a child of the submitter: with an
//!   untagged executable outside any stamped bundle it is untracked even when a tracked
//!   process submitted it; with a tagged executable it is tracked.
//! * `xattr -d com.apple.provenance` and `xattr -c` run by a TRACKED process exit 0 and
//!   remove nothing. The same `/usr/bin/xattr -d` run by a job launchd spawns from
//!   `/bin/sh` — platform binaries, which never carry the tag — removes it in place
//!   (2026-09-23: files, directories, a symlink itself, a Developer ID Mach-O whose
//!   signature still verifies afterwards; a 324 MB tree in milliseconds; 2026-09-24: the
//!   whole store on m7, 0 tagged files left). That is [`heal`], the ONE cure.
//! * (2026-09-15) A process that LOADS a tagged dynamic library becomes tracked: a
//!   launchd-spawned, untracked `python3` that `dlopen`ed a tagged copy of the trust
//!   bundle's `libstd-*.dylib` wrote a tagged file afterwards; the same process
//!   without the `dlopen` wrote a clean one. `trustc` loads `librustc_driver` and
//!   `libstd` from the bundle's `lib/`, so a bundle whose `bin/` is clean and whose
//!   `lib/` is tagged runs tracked all the same — [`tagged_files_under`] scans `lib/`,
//!   and the doctor and the cutter's gate read it.
//!
//! # Why aterm cares
//!
//! The release cutter builds with the Trust toolchain atpkg installs. If that
//! `trustc`/`targo` carries the tag, every object file, the proof snapshot and the
//! release artifacts inherit it, and `tools/proof_snapshot.py` refuses "published proof
//! snapshot has extended metadata" AFTER the ledger claim — a burned build number
//! (v0.83.0, 2026-09-12: bundle `trust/8590`, seeded from a shell that was itself
//! tracked). The 02:27 seed of `trust/8589` by the app's own update lane, from an
//! untracked app, came out clean, and the cut succeeded once that bundle was first on
//! PATH. So every store-changing door and `aterm pkg repair` end with [`heal_store`], the
//! cutter clears its own toolchain with [`heal`] and refuses before the claim whatever
//! stays tagged (`aterm-release::gates::provenance_gate`), and `aterm pkg doctor` counts
//! what is still tagged. Installs write in-process like any other program: until
//! 2026-09-24 they were routed through untracked launchd lanes so their files came out
//! clean, and that machinery was deleted once the heal cleared the real store in place.
//!
//! Outside a release cut the tag changes nothing a user runs: every tagged tool runs
//! normally. Its one everyday trace is archive metadata — macOS `tar` and `ditto -c -k`
//! carry it into `._` entries — and that comes from whatever process wrote the files,
//! not from the store.

use std::io;
use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
mod job;

/// The attribute name, exactly as `xattr -l` prints it.
pub const PROVENANCE_XATTR: &str = "com.apple.provenance";

/// What the tag does to a release cut, for the release cutter's refusal
/// (`aterm-release::gates`) — the one surface left that explains it at length.
pub const WHAT_IT_BREAKS: &str = "the tag follows the executable and the parent process: every \
    file a tagged trustc/targo (or a cutter descended from a tagged process) writes inherits \
    it, and tools/proof_snapshot.py refuses a tagged proof snapshot AFTER the ledger claim — \
    a burned build number (v0.83.0, 2026-09-12); `xattr -d` run by a tracked process exits 0 \
    and removes nothing, while the same command run by a launchd job clears it";

/// The cure, for the release cutter's refusal: the heal, from the store's door or the
/// cutter's own — never a re-install, which writes the same tagged files again.
pub const REMEDY: &str = "`aterm pkg repair` clears the tag from every installed file through \
    a launchd job, and the cutter clears its own toolchain and binary the same way before this \
    gate; a file still tagged after that is one the job could not clear — its reason is above, \
    and `aterm pkg doctor` counts what stays tagged";

/// The extended-attribute NAMES on `path` (symlinks followed), in the order the kernel
/// lists them.
///
/// `Ok(empty)` on a filesystem that has no extended attributes (`ENOTSUP`), and on every
/// non-macOS platform — the tag is a macOS phenomenon and there is nothing to read. A
/// path the kernel refuses to inspect (`ENOENT`, `EACCES`) is an error, not "clean": a
/// predicate that answers `false` on a failure to look is the bound-returned-as-a-fact
/// defect this repo has paid for before.
#[cfg(target_os = "macos")]
pub fn xattr_names(path: &Path) -> io::Result<Vec<String>> {
    list_names(path, 0)
}

/// [`xattr_names`] of `path` ITSELF: a symlink's own attributes, never its target's. What
/// the store scan reads ([`scan_roots`]), which must never wander out of the roots it was
/// handed.
#[cfg(target_os = "macos")]
fn xattr_names_nofollow(path: &Path) -> io::Result<Vec<String>> {
    list_names(path, libc::XATTR_NOFOLLOW)
}

/// See the macOS body.
#[cfg(not(target_os = "macos"))]
fn xattr_names_nofollow(_path: &Path) -> io::Result<Vec<String>> {
    Ok(Vec::new())
}

/// `listxattr(2)` of `path` with `options` (`0` follows a symlink, `XATTR_NOFOLLOW` does
/// not), as names.
#[cfg(target_os = "macos")]
fn list_names(path: &Path, options: libc::c_int) -> io::Result<Vec<String>> {
    use std::os::unix::ffi::OsStrExt as _;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    // The list can grow between the size query and the read (another writer adding an
    // attribute); `ERANGE` then says so and the loop asks again. Bounded so a pathological
    // writer cannot hold this in a spin.
    for _ in 0..4 {
        // SAFETY: `c_path` is a NUL-terminated C string that outlives both calls; a null
        // buffer with size 0 is the documented size query; `options` is one of the two
        // documented flag values.
        let needed = unsafe { libc::listxattr(c_path.as_ptr(), std::ptr::null_mut(), 0, options) };
        if needed < 0 {
            return unsupported_as_empty(io::Error::last_os_error());
        }
        if needed == 0 {
            return Ok(Vec::new());
        }
        let mut buf = vec![0u8; needed as usize];
        // SAFETY: `buf` is a live, writable allocation of exactly `buf.len()` bytes, which
        // is the size passed; `c_path` as above.
        let got = unsafe {
            libc::listxattr(
                c_path.as_ptr(),
                buf.as_mut_ptr().cast::<libc::c_char>(),
                buf.len(),
                options,
            )
        };
        if got < 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::ERANGE) {
                continue;
            }
            return unsupported_as_empty(err);
        }
        buf.truncate(got as usize);
        return Ok(buf
            .split(|b| *b == 0)
            .filter(|name| !name.is_empty())
            .map(|name| String::from_utf8_lossy(name).into_owned())
            .collect());
    }
    Err(io::Error::other(
        "listxattr kept answering ERANGE — the attribute list would not hold still",
    ))
}

/// See the macOS body: nothing to read here.
#[cfg(not(target_os = "macos"))]
pub fn xattr_names(_path: &Path) -> io::Result<Vec<String>> {
    Ok(Vec::new())
}

/// `ENOTSUP` (a volume without extended attributes) is an honest empty answer; anything
/// else is a failure to look and stays an error.
#[cfg(target_os = "macos")]
fn unsupported_as_empty(err: io::Error) -> io::Result<Vec<String>> {
    if matches!(
        err.raw_os_error(),
        Some(libc::ENOTSUP) | Some(libc::EOPNOTSUPP)
    ) {
        Ok(Vec::new())
    } else {
        Err(err)
    }
}

/// Whether `path` carries the extended attribute `attr`. A path that cannot be inspected
/// reads as NOT carrying it — callers that must distinguish "clean" from "could not look"
/// use [`xattr_names`] directly (the cutter's gate does).
#[must_use]
pub fn carries(path: &Path, attr: &str) -> bool {
    xattr_names(path).is_ok_and(|names| names.iter().any(|n| n == attr))
}

/// The attribute macOS stamps on a browser download. A QUARANTINED executable is
/// provenance-tracked in every invocation (measured 2026-09-14 on m16), which the window's
/// one provenance log line reads (`aterm-gui`'s `provenance_repair`).
pub const QUARANTINE_XATTR: &str = "com.apple.quarantine";

/// The regular files a scan found carrying an attribute, and how many regular files it
/// looked at ("3 of 31").
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Scan {
    /// Carriers, sorted by name.
    pub carriers: Vec<PathBuf>,
    /// Regular files inspected.
    pub total: usize,
}

/// Every regular file under `dir`, recursively, that carries `attr` — the shape a
/// bundle's `lib/` scan reports (2026-09-15: a tagged dylib tracks the process that
/// loads it, so `lib/` counts as `bin/` does). Symlinks are not followed; an unreadable
/// directory contributes nothing.
#[must_use]
pub fn tagged_files_under(dir: &Path, attr: &str) -> Scan {
    fn walk(dir: &Path, attr: &str, out: &mut Vec<PathBuf>, total: &mut usize) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for p in paths {
            let Ok(meta) = std::fs::symlink_metadata(&p) else {
                continue;
            };
            if meta.is_dir() {
                walk(&p, attr, out, total);
            } else if meta.is_file() {
                *total += 1;
                if carries(&p, attr) {
                    out.push(p);
                }
            }
        }
    }
    let mut carriers = Vec::new();
    let mut total = 0usize;
    walk(dir, attr, &mut carriers, &mut total);
    Scan { carriers, total }
}

/// Every directory under the prefix that holds files a process RUNS — what [`heal_store`]
/// clears and `aterm pkg doctor` scans, defined once: each active build's whole tree (not
/// only `bin/` and `lib/`: `codex-path/`, `libexec/` run too), the trust bundle's private
/// sysroots, every rustup view (`rustc`/`cargo` through rustup run out of it), the exec
/// roots under `compat/`, and the `bin/`, `agents/` and reroute shims. Only real
/// directories, never a symlink: the heal and the scan stay inside the prefix. Never an
/// app bundle — aterm's updater owns the app, and macOS refuses changes inside a launched
/// one.
#[must_use]
pub fn store_roots(layout: &crate::store::Layout) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = crate::ops::active_builds(layout)
        .into_iter()
        .map(|(program, build)| layout.build_dir(&program, build))
        .collect();
    roots.push(crate::seam::owned_root(layout).join("rustc-private-sysroots"));
    if let Ok(views) = std::fs::read_dir(crate::seam::views_root(layout)) {
        let mut views: Vec<PathBuf> = views.flatten().map(|e| e.path()).collect();
        views.sort();
        roots.extend(views);
    }
    roots.push(crate::compat::compat_dir(layout));
    roots.push(layout.bin_dir());
    roots.push(layout.agents_dir());
    roots.push(crate::reroute::dir(layout));
    roots.retain(|root| std::fs::symlink_metadata(root).is_ok_and(|m| m.is_dir()));
    roots
}

/// Every regular file under `roots` — each a directory, walked recursively, or a regular
/// file itself — that carries `attr`, read without following a symlink anywhere (the walk
/// takes each entry's own type, and the attribute is listed with `XATTR_NOFOLLOW`), so a
/// link out of the prefix is never looked through. Read-only. An unreadable directory
/// contributes nothing.
#[must_use]
pub fn scan_roots(roots: &[PathBuf], attr: &str) -> Scan {
    fn look(path: PathBuf, attr: &str, carriers: &mut Vec<PathBuf>, total: &mut usize) {
        *total += 1;
        if xattr_names_nofollow(&path).is_ok_and(|names| names.iter().any(|n| n == attr)) {
            carriers.push(path);
        }
    }
    fn walk(dir: &Path, attr: &str, carriers: &mut Vec<PathBuf>, total: &mut usize) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                walk(&entry.path(), attr, carriers, total);
            } else if kind.is_file() {
                look(entry.path(), attr, carriers, total);
            }
        }
    }
    let mut carriers = Vec::new();
    let mut total = 0usize;
    for root in roots {
        match std::fs::symlink_metadata(root) {
            Ok(meta) if meta.is_dir() => walk(root, attr, &mut carriers, &mut total),
            Ok(meta) if meta.is_file() => look(root.clone(), attr, &mut carriers, &mut total),
            _ => {}
        }
    }
    carriers.sort();
    Scan { carriers, total }
}

/// What [`heal`] did. Only the re-scan after the job decides it — never the job's exit
/// status alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealOutcome {
    /// Nothing under the roots carried the tag; no job ran.
    Clean,
    /// Nothing carries the tag now; `cleared` files did before the job.
    Healed { cleared: usize },
    /// The job ran, or ran out of time, and these files still carry the tag.
    Left { carriers: Vec<PathBuf>, why: String },
    /// No job could be started; these files carry the tag.
    Unavailable { carriers: Vec<PathBuf>, why: String },
}

impl HealOutcome {
    /// Whether nothing under the roots carries the tag now.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        matches!(self, Self::Clean | Self::Healed { .. })
    }

    /// The files that still carry the tag (empty when [`Self::is_clean`]).
    #[must_use]
    pub fn left(&self) -> &[PathBuf] {
        match self {
            Self::Clean | Self::Healed { .. } => &[],
            Self::Left { carriers, .. } | Self::Unavailable { carriers, .. } => carriers,
        }
    }

    /// Why files were left tagged — for a log line or a record, never for glass.
    #[must_use]
    pub fn why(&self) -> Option<&str> {
        match self {
            Self::Clean | Self::Healed { .. } => None,
            Self::Left { why, .. } | Self::Unavailable { why, .. } => Some(why),
        }
    }
}

/// The shape of [`heal`], so a caller whose outcome is under test can be handed a stub.
pub type Healer = fn(&[PathBuf], &Path) -> HealOutcome;

/// How long [`heal`] waits for its job. The whole store (about 2,500 files) clears in well
/// under a second; this bounds a wedged launchd, never a slow disk.
pub const HEAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// The heal job's body, for `/bin/sh -c`: `$1` the status file, `$2` its own label, `$3`
/// the attribute, the roots after. `-r` walks each root, `-s` acts on a symlink itself and
/// never on its target, and `-d` removes that ONE attribute — never `-c`, which would strip
/// every other one (a code signature kept in an attribute included). It records xattr's
/// status, removes its own label and exits 0 (`provenance/job.rs`): a job launchd keeps
/// is one it re-runs.
#[cfg(target_os = "macos")]
const HEAL_SCRIPT: &str = r#"s="$1"; l="$2"; a="$3"; shift 3
/usr/bin/xattr -r -s -d "$a" "$@"
r=$?
printf '%s\n' "$r" > "$s.tmp" && /bin/mv -f "$s.tmp" "$s"
/bin/launchctl remove "$l"
exit 0
"#;

/// Clear `com.apple.provenance` from everything under `roots`, in place, and say what is
/// left ([`HealOutcome`]). The caller holds the store lock.
///
/// A tracked process cannot clear the tag itself — its `xattr -d` exits 0 and removes
/// nothing — so the clearing is ONE launchd job made only of platform binaries (`/bin/sh`
/// and `/usr/bin/xattr` cannot carry the tag, so the job is untracked whatever state
/// aterm's own bundle is in). Nothing is submitted when a scan finds no carrier. The job
/// is waited for at most [`HEAL_TIMEOUT`] and its label is removed on every path; a scan
/// afterwards is the verdict. `scratch` holds the job's status (removed after).
/// Off macOS there is no tag, and this answers [`HealOutcome::Clean`] without looking.
#[must_use]
pub fn heal(roots: &[PathBuf], scratch: &Path) -> HealOutcome {
    heal_with(roots, scratch, PROVENANCE_XATTR)
}

/// [`heal`] for the attribute `attr` — a parameter only so a test can use one it is able to
/// set (`com.apple.provenance` cannot be minted by hand; the release cutter's gate test
/// uses it too); production clears that one.
#[must_use]
pub fn heal_with(roots: &[PathBuf], scratch: &Path, attr: &str) -> HealOutcome {
    if !cfg!(target_os = "macos") {
        return HealOutcome::Clean;
    }
    let before = scan_roots(roots, attr);
    if before.carriers.is_empty() {
        return HealOutcome::Clean;
    }
    let ran = run_heal_job(roots, scratch, attr);
    let after = scan_roots(roots, attr);
    if after.carriers.is_empty() {
        return HealOutcome::Healed {
            cleared: before.carriers.len(),
        };
    }
    match ran {
        Err(why) => HealOutcome::Unavailable {
            carriers: after.carriers,
            why,
        },
        Ok(status) => HealOutcome::Left {
            why: match status {
                Some(0) => format!(
                    "{} file(s) still carry it after clearing",
                    after.carriers.len()
                ),
                Some(code) => format!("xattr exited {code}"),
                None => format!(
                    "the clearing job did not finish within {} s",
                    HEAL_TIMEOUT.as_secs()
                ),
            },
            carriers: after.carriers,
        },
    }
}

/// Submit the heal job and wait for it: `Ok(Some(status))` when xattr finished,
/// `Ok(None)` when the job ran out of time or vanished, `Err` when it could not be started.
/// The job is dropped — its label removed and its scratch deleted — before this returns.
#[cfg(target_os = "macos")]
fn run_heal_job(roots: &[PathBuf], scratch: &Path, attr: &str) -> Result<Option<i32>, String> {
    let mut job = job::Job::prepare(scratch, "heal")?;
    let mut args: Vec<&std::ffi::OsStr> = vec![std::ffi::OsStr::new(attr)];
    args.extend(roots.iter().map(|r| r.as_os_str()));
    job.submit("atpkg-heal", HEAL_SCRIPT, &args)?;
    Ok(job.wait_for_status(HEAL_TIMEOUT))
}

/// See the macOS body.
#[cfg(not(target_os = "macos"))]
fn run_heal_job(_roots: &[PathBuf], _scratch: &Path, _attr: &str) -> Result<Option<i32>, String> {
    Err(String::from("there is no launchd here"))
}

/// [`heal`] over the whole store ([`store_roots`]), then the retired lanes' leftovers
/// swept ([`sweep_lane_leftovers`]). The caller holds the store lock. What every
/// store-changing door runs at its end, and what `aterm pkg repair` runs.
///
/// Inside atpkg's own unit tests the heal is `test_bind`'s scan-only stand-in unless the
/// test opted into the real one: a tracked test process (an agent's shell) tags every file
/// it writes, so every test that ran a pass, an install, a seed or a repair submitted one
/// real launchd job here — a unit test of that door, not of launchd.
#[must_use]
pub fn heal_store(layout: &crate::store::Layout) -> HealOutcome {
    heal_store_with(layout, store_healer())
}

/// The heal [`heal_store`] runs: [`heal`].
#[cfg(not(test))]
fn store_healer() -> Healer {
    heal
}

/// The heal [`heal_store`] runs under test: this thread's choice ([`test_bind`]).
#[cfg(test)]
fn store_healer() -> Healer {
    test_bind::store_healer()
}

/// The store heal under atpkg's own unit tests, per thread — tests run in parallel, each on
/// its own thread, and a pass runs on the thread that called it (the shape of
/// [`crate::packages_log::test_bind`]).
///
/// By default [`heal_store`] runs [`test_bind`]'s `scan_only`: the scan [`heal`] starts
/// with, answered as the heal that cleared every carrier it found, and NO launchd job — so
/// the door carries on exactly as after a heal that worked, while the files keep whatever
/// tag they had. A test that proves the real heal THROUGH A DOOR opts in for its
/// scope with [`test_bind::real`]. The heal's own tests call [`heal`], [`heal_with`] or
/// [`heal_store_with`] directly and never pass through this binding.
#[cfg(test)]
pub(crate) mod test_bind {
    use super::{HealOutcome, Healer, Scan};
    use std::cell::{Cell, RefCell};
    use std::path::{Path, PathBuf};

    thread_local! {
        static REAL: Cell<bool> = const { Cell::new(false) };
        static FAKED: RefCell<Vec<(Vec<PathBuf>, Scan)>> = const { RefCell::new(Vec::new()) };
    }

    /// Run the REAL heal ([`super::heal`]: one launchd job when anything carries the tag)
    /// in every [`super::heal_store`] on this thread until the guard drops.
    pub(crate) fn real() -> Guard {
        REAL.with(|r| r.set(true));
        Guard
    }

    pub(super) fn store_healer() -> Healer {
        if REAL.with(Cell::get) {
            super::heal
        } else {
            scan_only
        }
    }

    /// What [`super::heal`] would have cleared — the same scan, over the same attribute —
    /// with nothing submitted: `Clean` when nothing carries the tag, `Healed` with the
    /// count otherwise. Kept for the test to read ([`faked`]).
    fn scan_only(roots: &[PathBuf], _scratch: &Path) -> HealOutcome {
        let scan = super::scan_roots(roots, super::PROVENANCE_XATTR);
        let outcome = if scan.carriers.is_empty() {
            HealOutcome::Clean
        } else {
            HealOutcome::Healed {
                cleared: scan.carriers.len(),
            }
        };
        FAKED.with(|f| f.borrow_mut().push((roots.to_vec(), scan)));
        outcome
    }

    /// Every store heal the stand-in answered on this thread since the last call, oldest
    /// first: the roots it was handed and what it found carrying the tag. Draining, so a
    /// test that calls it before its door reads only that door's heals.
    pub(crate) fn faked() -> Vec<(Vec<PathBuf>, Scan)> {
        FAKED.with(|f| std::mem::take(&mut *f.borrow_mut()))
    }

    pub(crate) struct Guard;

    impl Drop for Guard {
        fn drop(&mut self) {
            REAL.with(|r| r.set(false));
        }
    }
}

/// [`heal_store`] with the heal explicit ([`Healer`]).
#[must_use]
pub fn heal_store_with(layout: &crate::store::Layout, heal: Healer) -> HealOutcome {
    let outcome = heal(&store_roots(layout), &heal_scratch(layout));
    sweep_lane_leftovers(layout);
    outcome
}

/// Where a heal of `layout`'s files keeps its job: `<prefix>/staging/.lanes/` (the name the
/// retired lanes gave it), or the temp dir when it cannot be made.
fn heal_scratch(layout: &crate::store::Layout) -> PathBuf {
    let scratch = layout.prefix.join("staging").join(".lanes");
    if layout.ensure_dir(&scratch).is_ok() {
        scratch
    } else {
        std::env::temp_dir()
    }
}

/// Remove what the untracked lanes (deleted 2026-09-24) left in the store, which nothing
/// reads any more: every `store/<program>/<build>.tracked-install` record and the
/// `<prefix>/tag-relay.tried` memo. Tagged files are measured, never recorded.
fn sweep_lane_leftovers(layout: &crate::store::Layout) {
    let _ = std::fs::remove_file(layout.prefix.join("tag-relay.tried"));
    let Ok(programs) = std::fs::read_dir(layout.prefix.join("store")) else {
        return;
    };
    for program in programs.flatten() {
        let Ok(entries) = std::fs::read_dir(program.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let is_record = entry
                .file_name()
                .to_str()
                .and_then(|n| n.strip_suffix(".tracked-install"))
                .is_some_and(|build| !build.is_empty());
            if is_record && entry.file_type().is_ok_and(|t| t.is_file()) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// Say `msg` where a record is kept, and never on a terminal: a pass the window spawns
/// (its stderr goes to aterm.log) and a host (whose notices are log records) hear it; a
/// verb typed at a terminal does not. For what the system did or could not do on its own
/// — `aterm pkg doctor` is where a person asks.
pub(crate) fn log_line(msg: &str) {
    use std::io::IsTerminal as _;
    if crate::notice::hosted() || !std::io::stderr().is_terminal() {
        crate::notice::say(msg);
    }
}

/// `1 file`, `2 files` — `n` of `noun`, for the heal's one line.
#[must_use]
pub fn count_of(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// Whether THIS process is provenance-tracked — MEASURED, by writing a probe file into
/// `scratch` and reading the attribute back, never inferred from the binary's own
/// attributes (a clean binary under a tracked parent is tracked): `Some(tagged)` when the
/// probe could be written and read, `None` when it could not. The probe is removed.
pub fn measure_tracked(scratch: &Path) -> Option<bool> {
    if !cfg!(target_os = "macos") {
        return Some(false);
    }
    let probe = scratch.join(format!(".provenance-probe-{}", std::process::id()));
    std::fs::write(&probe, b"probe\n").ok()?;
    let verdict = xattr_names(&probe)
        .ok()
        .map(|names| names.iter().any(|n| n == PROVENANCE_XATTR));
    let _ = std::fs::remove_file(&probe);
    verdict
}

/// Set an extended attribute — TEST SCAFFOLDING for the synthetic-attribute tests here
/// and in `doctor.rs`/`install.rs`: `com.apple.provenance` itself cannot be minted by
/// hand (`xattr -w` is refused), so the predicate is exercised with `user.*` names
/// through the very `listxattr` path production reads.
#[cfg(all(test, target_os = "macos"))]
pub(crate) fn set_xattr_for_test(path: &Path, name: &str, value: &[u8]) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt as _;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    let c_name = std::ffi::CString::new(name)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "name contains NUL"))?;
    // SAFETY: both C strings and the value slice outlive the call; the length passed is
    // the slice's own.
    let rc = unsafe {
        libc::setxattr(
            c_path.as_ptr(),
            c_name.as_ptr(),
            value.as_ptr().cast::<libc::c_void>(),
            value.len(),
            0,
            0,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    fn tmp(label: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("atpkg-provenance-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// The name this process cannot mint is read through the same syscall as a name it
    /// can: a synthetic `user.*` attribute is listed, and only where it was set.
    #[test]
    fn a_synthetic_attribute_is_listed_where_set_and_nowhere_else() {
        let d = tmp("listed");
        let tagged = d.join("tagged");
        let clean = d.join("clean");
        std::fs::write(&tagged, b"x").unwrap();
        std::fs::write(&clean, b"x").unwrap();
        set_xattr_for_test(&tagged, "user.aterm.probe", b"1").unwrap();
        assert!(carries(&tagged, "user.aterm.probe"));
        assert!(!carries(&clean, "user.aterm.probe"));
        // A name that is NOT set is not reported, whatever else the file carries.
        assert!(!carries(&tagged, "user.aterm.other"));
        // The listing names the attribute exactly as `xattr -l` would.
        assert!(
            xattr_names(&tagged)
                .unwrap()
                .iter()
                .any(|n| n == "user.aterm.probe")
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// "Could not look" is an error, never "clean".
    #[test]
    fn a_missing_path_is_an_error_not_a_clean_answer() {
        let d = tmp("missing");
        let err = xattr_names(&d.join("nope")).expect_err("ENOENT must surface");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(!carries(&d.join("nope"), PROVENANCE_XATTR));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The recursive scan counts regular files only, names the carriers sorted, walks into
    /// subdirectories and never through a symlink.
    #[test]
    fn the_lib_scan_counts_regular_files_and_names_the_carriers() {
        let d = tmp("scan");
        let lib = d.join("lib");
        std::fs::create_dir_all(lib.join("rustlib")).unwrap();
        for name in ["libstd.dylib", "rustlib/librustc_driver.dylib", "README"] {
            std::fs::write(lib.join(name), b"x").unwrap();
        }
        std::os::unix::fs::symlink("libstd.dylib", lib.join("libstd-alias.dylib")).unwrap();
        for tagged in ["libstd.dylib", "rustlib/librustc_driver.dylib"] {
            set_xattr_for_test(&lib.join(tagged), "user.aterm.probe", b"1").unwrap();
        }
        let scan = tagged_files_under(&lib, "user.aterm.probe");
        assert_eq!(scan.total, 3, "three regular files, one link");
        assert_eq!(
            scan.carriers,
            vec![
                lib.join("libstd.dylib"),
                lib.join("rustlib/librustc_driver.dylib")
            ]
        );
        assert!(
            tagged_files_under(&lib, "user.aterm.other")
                .carriers
                .is_empty()
        );
        assert_eq!(
            tagged_files_under(&d.join("absent"), "user.aterm.probe"),
            Scan::default()
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The roots are every directory of things that RUN — each active build's whole
    /// tree, the private sysroots, every rustup view, the exec roots, the three shim
    /// directories — and only real directories: an absent one is left out, and so is a
    /// symlink standing where one would be, so neither the scan nor the heal can leave
    /// the prefix. A build that is installed but not active is not a root.
    #[test]
    fn the_store_roots_are_every_real_directory_of_things_that_run() {
        let d = tmp("roots");
        let layout = crate::store::Layout {
            prefix: d.join("pkg"),
        };
        for (program, build) in [("trust", 8590u64), ("ay", 8256)] {
            let dir = layout.build_dir(program, build);
            std::fs::create_dir_all(dir.join("bin")).unwrap();
            std::fs::write(dir.join("bin").join(program), b"#!/bin/true\n").unwrap();
            crate::install_shims(
                &layout,
                &dir,
                &[program.to_string()],
                crate::activate::Aliases::Off,
            )
            .unwrap();
        }
        // Installed, not active: no shim names it.
        std::fs::create_dir_all(layout.build_dir("trust", 8589).join("bin")).unwrap();
        let sysroots = crate::seam::owned_root(&layout).join("rustc-private-sysroots");
        std::fs::create_dir_all(&sysroots).unwrap();
        let view = crate::seam::view_dir(&layout, "trust");
        std::fs::create_dir_all(view.join("bin")).unwrap();
        std::os::unix::fs::symlink(&d, crate::seam::views_root(&layout).join("elsewhere")).unwrap();
        std::fs::create_dir_all(crate::compat::compat_dir(&layout)).unwrap();
        std::fs::create_dir_all(layout.agents_dir()).unwrap();
        let active: Vec<PathBuf> = crate::ops::active_builds(&layout)
            .into_iter()
            .map(|(p, b)| layout.build_dir(&p, b))
            .collect();
        assert_eq!(
            active.len(),
            2,
            "the fixture's shims make two builds active"
        );
        let mut want = active;
        want.extend([
            sysroots,
            view,
            crate::compat::compat_dir(&layout),
            layout.bin_dir(),
            layout.agents_dir(),
        ]);
        assert_eq!(
            store_roots(&layout),
            want,
            "no reroute dir (absent), no symlinked view, no inactive build"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The scan walks every root to the bottom, counts regular files only, names the
    /// carriers sorted, takes a regular file as a root of its own, and never looks
    /// through a symlink — not at a tagged file it points to outside the roots, not into
    /// a directory it points to.
    #[test]
    fn the_scan_walks_every_root_and_never_follows_a_link() {
        let d = tmp("scan-roots");
        let root = d.join("root");
        let outside = d.join("outside");
        std::fs::create_dir_all(root.join("a").join("b")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        for f in ["top", "a/mid", "a/b/deep"] {
            std::fs::write(root.join(f), b"x").unwrap();
        }
        std::fs::write(outside.join("far"), b"x").unwrap();
        std::os::unix::fs::symlink(outside.join("far"), root.join("to-far")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("to-outside")).unwrap();
        let lone = d.join("lone");
        std::fs::write(&lone, b"x").unwrap();
        for tagged in [
            root.join("a/b/deep"),
            root.join("top"),
            outside.join("far"),
            lone.clone(),
        ] {
            set_xattr_for_test(&tagged, "user.aterm.probe", b"1").unwrap();
        }
        let scan = scan_roots(
            &[root.clone(), lone.clone(), d.join("absent")],
            "user.aterm.probe",
        );
        assert_eq!(scan.total, 4, "three files under the root, one lone file");
        assert_eq!(
            scan.carriers,
            vec![lone.clone(), root.join("a/b/deep"), root.join("top")],
            "sorted, and nothing reached through a link"
        );
        assert!(scan_roots(&[root], "user.aterm.absent").carriers.is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// THE HEAL, for real: one launchd job clears the attribute from every file, the
    /// directories and a symlink ITSELF under the roots — and nothing else. Another
    /// attribute on the same file stays, a file OUTSIDE the roots that a link points to
    /// keeps its tag (the job never follows a link), and the job's scratch is removed. A
    /// clean tree runs no job at all.
    #[test]
    fn the_heal_clears_exactly_the_one_attribute_inside_the_roots() {
        let d = tmp("heal");
        let root = d.join("root");
        let outside = d.join("outside");
        let scratch = d.join("scratch");
        for dir in [root.join("bin"), outside.clone(), scratch.clone()] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let tool = root.join("bin").join("tool");
        let data = root.join("data.json");
        std::fs::write(&tool, b"#!/bin/true\n").unwrap();
        std::fs::write(&data, b"{}").unwrap();
        std::fs::write(outside.join("far"), b"x").unwrap();
        let link = root.join("bin").join("far");
        std::os::unix::fs::symlink(outside.join("far"), &link).unwrap();
        for tagged in [&tool, &data, &outside.join("far")] {
            set_xattr_for_test(tagged, "user.aterm.probe", b"1").unwrap();
        }
        set_xattr_for_test(&tool, "user.aterm.keep", b"k").unwrap();
        set_xattr_for_test(&root.join("bin"), "user.aterm.probe", b"1").unwrap();

        let outcome = heal_with(std::slice::from_ref(&root), &scratch, "user.aterm.probe");
        assert_eq!(
            outcome,
            HealOutcome::Healed { cleared: 2 },
            "two files carried it"
        );
        assert!(outcome.is_clean() && outcome.left().is_empty());
        assert!(!carries(&tool, "user.aterm.probe"));
        assert!(!carries(&data, "user.aterm.probe"));
        assert!(
            !carries(&root.join("bin"), "user.aterm.probe"),
            "a directory too"
        );
        assert!(
            carries(&tool, "user.aterm.keep"),
            "no other attribute is touched"
        );
        assert!(
            carries(&outside.join("far"), "user.aterm.probe"),
            "the file a link points to, outside the roots, keeps its tag"
        );
        // The job's scratch goes after launchd has forgotten its label (`Job`'s drop; the
        // label's own removal is pinned by `job`'s test, which knows it — other tests'
        // heals of this process run beside this one).
        let jobs: Vec<String> = std::fs::read_dir(&scratch)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(jobs.is_empty(), "the job's scratch is removed: {jobs:?}");
        // Clean now: no job is submitted, and the answer says so.
        assert_eq!(
            heal_with(std::slice::from_ref(&root), &scratch, "user.aterm.probe"),
            HealOutcome::Clean
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// THE REAL TAG. A file this process writes carries `com.apple.provenance` exactly
    /// when the process is tracked (an agent's shell, a shell inside a tracked aterm.app);
    /// its own `xattr -d` removes nothing, and [`heal`] clears it. Under an untracked test
    /// process there is nothing to clear and the heal says `Clean` — vacuous, and said so.
    #[test]
    fn the_heal_clears_the_real_tag_a_tracked_process_leaves() {
        let d = tmp("heal-real");
        let scratch = d.join("scratch");
        let tree = d.join("tree");
        std::fs::create_dir_all(&scratch).unwrap();
        std::fs::create_dir_all(tree.join("sub")).unwrap();
        std::fs::write(tree.join("sub").join("file"), b"written by this process").unwrap();
        let tracked = carries(&tree.join("sub").join("file"), PROVENANCE_XATTR);
        let own = std::process::Command::new("/usr/bin/xattr")
            .args(["-d", PROVENANCE_XATTR])
            .arg(tree.join("sub").join("file"))
            .status()
            .unwrap();
        if tracked {
            assert!(own.success(), "a tracked xattr -d reports success…");
            assert!(
                carries(&tree.join("sub").join("file"), PROVENANCE_XATTR),
                "…and removes nothing"
            );
        }
        let outcome = heal(std::slice::from_ref(&tree), &scratch);
        eprintln!("this test process tracked={tracked}; heal: {outcome:?}");
        assert_eq!(
            outcome,
            if tracked {
                HealOutcome::Healed { cleared: 1 }
            } else {
                HealOutcome::Clean
            }
        );
        assert!(!carries(&tree.join("sub").join("file"), PROVENANCE_XATTR));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The measurement leaves no probe behind and agrees with a direct read of a file
    /// this process writes — whichever way this test happens to be run (tracked or not),
    /// the two must say the same thing.
    #[test]
    fn the_tracking_measurement_matches_a_direct_write_and_cleans_up() {
        let d = tmp("measure");
        let measured = measure_tracked(&d).expect("a writable scratch dir must be measurable");
        let witness = d.join("witness");
        std::fs::write(&witness, b"w").unwrap();
        assert_eq!(measured, carries(&witness, PROVENANCE_XATTR));
        let leftovers: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .filter(|n| n.to_string_lossy().starts_with(".provenance-probe-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "the probe must be removed: {leftovers:?}"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// An unwritable scratch cannot be measured — `None`, never a guess.
    #[test]
    fn an_unwritable_scratch_is_unmeasurable() {
        let d = tmp("unwritable");
        assert_eq!(measure_tracked(&d.join("absent").join("deeper")), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The store heal sweeps what the retired lanes left: every `<build>.tracked-install`
    /// record and the tag-relay memo go, whatever the heal found; a build, its `.ready` and
    /// a file that merely ENDS in the suffix with no build before it stay.
    #[test]
    fn the_store_heal_sweeps_the_retired_lanes_leftovers() {
        let d = tmp("leftovers");
        let layout = crate::store::Layout {
            prefix: d.join("pkg"),
        };
        let build = layout.build_dir("trust", 9192);
        std::fs::create_dir_all(build.join("bin")).unwrap();
        let store = build.parent().unwrap().to_path_buf();
        for keep in ["9192.ready", ".tracked-install"] {
            std::fs::write(store.join(keep), b"x").unwrap();
        }
        for stale in ["9192.tracked-install", "8590.tracked-install"] {
            std::fs::write(store.join(stale), b"tracked-install v1\nwhy=x\n").unwrap();
        }
        std::fs::write(layout.prefix.join("tag-relay.tried"), b"1\t/x\n").unwrap();
        let healed = heal_store_with(&layout, |_, _| HealOutcome::Clean);
        assert_eq!(healed, HealOutcome::Clean);
        assert!(!store.join("9192.tracked-install").exists());
        assert!(!store.join("8590.tracked-install").exists());
        assert!(!layout.prefix.join("tag-relay.tried").exists());
        assert!(store.join("9192.ready").exists() && build.join("bin").is_dir());
        assert!(
            store.join(".tracked-install").exists(),
            "no build named: not a record"
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}

#[cfg(all(test, not(target_os = "macos")))]
mod off_macos_tests {
    use super::*;

    /// Off macOS there is no tag: the heal answers `Clean` without looking or submitting.
    #[test]
    fn the_heal_is_a_no_op() {
        assert_eq!(
            heal(&[PathBuf::from("/nonexistent")], &std::env::temp_dir()),
            HealOutcome::Clean
        );
    }
}
