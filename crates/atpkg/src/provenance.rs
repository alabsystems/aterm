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
//!   one the `X.<ext>` wrapper around `Contents/MacOS/` (measured 2026-09-23; the table
//!   is on [`taint_candidates`]) — or when its PARENT is tracked. Everything a
//!   tracked process creates or touches is tagged: `open(O_CREAT)`,
//!   but also `rename`, `link`, `chmod`, `utimes`, `setxattr` and an append — a tracked
//!   parent that merely renames a clean file tags it. Reads, `stat` and `fsync` do not.
//! * The tag is inherited across `fork`, `posix_spawn`, `setsid`, `osascript`,
//!   `launchctl asuser`, and it SURVIVES `exec` into an untagged image: a tagged
//!   `#!/bin/sh` shim that `exec`s a clean tool still produces tagged output. The shims
//!   atpkg lays in `bin/` are exactly that shape.
//! * A job launchd spawns (`launchctl submit`, `launchctl bootstrap gui/<uid>`) is NOT a
//!   child of the submitter: with an untagged executable it writes clean files even when
//!   submitted by a tracked process, even into a tagged directory, even through a tagged
//!   symlink. With a tagged executable it writes tagged files — the tag follows the
//!   executable. And with an untagged executable inside a STAMPED bundle it writes
//!   tagged files too (2026-09-23): the stamp on the bundle directory is enough.
//! * A byte copy of a tagged executable made by an untracked process (`cat a > b`) is
//!   clean and runs clean; `cp`/`ditto` copy the attribute along with the bytes.
//! * `xattr -d com.apple.provenance` and `xattr -c` run by a TRACKED process exit 0 and
//!   remove nothing. The same `/usr/bin/xattr -d` run by a job launchd spawns from
//!   `/bin/sh` — platform binaries, which never carry the tag — removes it in place
//!   (2026-09-23: files, directories, a symlink itself, a Developer ID Mach-O whose
//!   signature still verifies afterwards; a 324 MB tree in milliseconds). That is
//!   [`heal`], and why a tagged store is repaired in place rather than re-seeded.
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
//! PATH. Four surfaces read this module: the cutter's pre-claim gate
//! (`aterm-release::gates::provenance_gate`), `aterm pkg doctor`, the store's own
//! staging lane, which hands extraction to an untracked launchd job when the installer
//! measures itself as tracked ([`crate::stage_helper`]), and [`heal_store`], which every
//! package pass and `aterm pkg repair` run so a tag that got in anyway is cleared
//! silently.
//!
//! Outside a release cut the tag changes nothing a user runs: every tagged tool runs
//! normally. Its one everyday trace is archive metadata — macOS `tar` and `ditto -c -k`
//! carry it into `._` entries — and that comes from whatever process wrote the files,
//! not from the store.

use std::io;
use std::path::{Path, PathBuf};

/// The attribute name, exactly as `xattr -l` prints it.
pub const PROVENANCE_XATTR: &str = "com.apple.provenance";

/// What the tag does to a release cut, for the release cutter's refusal
/// (`aterm-release::gates`) — the one surface left that explains it at length.
pub const WHAT_IT_BREAKS: &str = "the tag follows the executable and the parent process: every \
    file a tagged trustc/targo (or a cutter descended from a tagged process) writes inherits \
    it, and tools/proof_snapshot.py refuses a tagged proof snapshot AFTER the ledger claim — \
    a burned build number (v0.83.0, 2026-09-12); `xattr -d` run by a tracked process exits 0 \
    and removes nothing, while the same command run by a launchd job clears it";

/// The cure, for the release cutter's refusal.
pub const REMEDY: &str = "`aterm pkg repair` clears the tag from every installed file through \
    a launchd job; if a file keeps it, re-seed the bundle — `aterm pkg uninstall <program> && \
    aterm pkg install <program>`, then `aterm pkg install --default-set` to keep the set \
    auto-completing — or point TRUST_STAGE2_BIN at an untagged bundle's bin/ (`xattr -l \
    <bin>/trustc` prints nothing on a clean one)";

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

/// Whether `path` carries `com.apple.provenance`.
#[must_use]
pub fn carries_provenance(path: &Path) -> bool {
    carries(path, PROVENANCE_XATTR)
}

/// The attribute macOS stamps on a browser download. It matters here because a
/// QUARANTINED executable is provenance-tracked in every invocation — a job launchd
/// spawns from it included, which is the one escape the untracked lane relies on
/// (measured 2026-09-14 on m16: a launchd job that exec'd the quarantined, UNTAGGED
/// `aterm.app` binary in place wrote a result file carrying `com.apple.provenance`).
/// So a quarantined helper is as unusable in place as a tagged one, and
/// [`crate::stage_helper::plan_for_exe`] must treat it the same way. An app laid down
/// by aterm's own updater carries neither attribute.
pub const QUARANTINE_XATTR: &str = "com.apple.quarantine";

/// The paths whose attributes decide whether a process exec'd from `exe` is tracked:
/// `exe` itself; every directory above it whose extension is `app` in any case; and the
/// directory `X` when `exe` is `X/Contents/MacOS/<exe>`, whatever `X` is called. Nearest
/// first, each once. Never `Contents/`, `Contents/MacOS/`, `Info.plist` or a plain
/// parent: those were measured and never charged.
///
/// # What was measured (2026-09-23, this Mac: a launchd job running a CLEAN executable in
/// place, with the named directory stamped by a tracked shell and nothing else stamped)
///
/// | where the clean executable sits | what the job wrote |
/// |---|---|
/// | `S.app/Contents/MacOS/`, `S.app` stamped | STAMPED |
/// | the same, `S.App` or `S.APP` stamped | STAMPED |
/// | `S.app/bin/`, `S.app/a/b/`, `S.app` stamped | STAMPED |
/// | `S.xpc`, `S.appex`, `S.bundle`, `S.plugin`, `S.framework`, `S.kext` or `S.foo`, each with `Contents/MacOS/`, the wrapper stamped | STAMPED |
/// | `S.foo/Contents/MacOS/` with no `Info.plist`, `S.foo` stamped | STAMPED |
/// | `S.app` stamped, no `Info.plist` | STAMPED |
/// | a stamped OUTER `.app` around a clean inner helper `.app` | STAMPED |
/// | a clean outer `.app` around a stamped inner `.app` or `.xpc` | clean |
/// | `Q/Contents/MacOS/`, `Q` stamped and bundle-shaped, no extension | clean |
/// | `S.foo/bin/`, `S.bundle/bin/`, `S.foo/Contents/Resources/`, `S.framework/Versions/A/` | clean |
/// | only `Contents/`, `Contents/MacOS/` or `Info.plist` stamped | clean |
/// | a stamped plain parent directory, or a free-standing executable | clean |
///
/// So the kernel charges the process to the OUTERMOST `.app` above its executable when
/// there is one, and otherwise to the `X.<ext>` wrapper around `Contents/MacOS/`. The
/// same mechanism was measured independently on m16 on 2026-09-20 (15ec68a85: a clean
/// `Contents/MacOS/aterm` under a tagged root wrote a tagged file from a launchd job).
///
/// This walk is a SUPERSET of that rule on purpose. It names every `.app`, not only the
/// outermost, and the wrapper even when it has no extension (`Q` above measured clean).
/// A false positive costs a replica copy; a missed carrier costs a tagged toolchain,
/// which is the failure this module exists to prevent. Shapes nobody measured — other
/// placements, other extensions without `Contents/MacOS/` — are not claimed either way.
/// `crates/atpkg/tests/untracked_stamped_bundle.rs` keeps the first rows as live
/// negative controls.
pub fn taint_candidates(exe: &Path) -> impl Iterator<Item = &Path> {
    let wrapper = contents_macos_wrapper(exe);
    std::iter::once(exe).chain(
        exe.ancestors()
            .skip(1)
            .filter(move |a| is_app_dir(a) || Some(*a) == wrapper),
    )
}

/// `X` for `X/Contents/MacOS/<exe>`, whatever `X` is called (the ASCII case of
/// `Contents` and `MacOS` ignored, as the default macOS volume does).
fn contents_macos_wrapper(exe: &Path) -> Option<&Path> {
    let macos = exe.parent()?;
    if !macos.file_name()?.eq_ignore_ascii_case("MacOS") {
        return None;
    }
    let contents = macos.parent()?;
    if !contents.file_name()?.eq_ignore_ascii_case("Contents") {
        return None;
    }
    contents.parent()
}

/// A directory named `*.app`, the extension in any ASCII case.
fn is_app_dir(p: &Path) -> bool {
    p.extension().is_some_and(|x| x.eq_ignore_ascii_case("app"))
}

/// The provenance carrier behind `exe`, if any: `exe` itself or a bundle directory above
/// it ([`taint_candidates`]). A path that cannot be INSPECTED is an `Err`, never "clean":
/// a plan built on a failure to look would run a possibly tainted helper in place and
/// call its files clean.
///
/// # Errors
/// The first candidate whose attribute list cannot be read, named.
pub fn provenance_carrier(exe: &Path) -> Result<Option<PathBuf>, String> {
    carrier_of(exe, PROVENANCE_XATTR)
}

/// [`provenance_carrier`] for the attribute `attr` — a parameter so a test can taint a
/// bundle with an attribute it is able to set.
pub(crate) fn carrier_of(exe: &Path, attr: &str) -> Result<Option<PathBuf>, String> {
    for candidate in taint_candidates(exe) {
        let names = xattr_names(candidate)
            .map_err(|e| format!("cannot inspect {} for {attr}: {e}", candidate.display()))?;
        if names.iter().any(|n| n == attr) {
            return Ok(Some(candidate.to_path_buf()));
        }
    }
    Ok(None)
}

/// The quarantined carrier behind `exe`, if any: `exe` itself or a bundle directory above
/// it ([`taint_candidates`]) — a browser stamps the bundle it downloaded and the
/// executable inside it, and either is enough to make every invocation tracked. `None`
/// when none carries the attribute, and on every non-macOS platform. Until 2026-09-23
/// this looked at the nearest lowercase `.app` only; it now shares the provenance walk,
/// which can only ever find more carriers, never fewer. Only the provenance half of that
/// walk was measured against the kernel; for quarantine it is the conservative choice,
/// not a measurement.
#[must_use]
pub fn quarantined_carrier(exe: &Path) -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    taint_candidates(exe)
        .find(|p| carries(p, QUARANTINE_XATTR))
        .map(Path::to_path_buf)
}

/// The regular files a scan found carrying an attribute, and how many regular files it
/// looked at ("3 of 31").
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Scan {
    /// Carriers, sorted by name.
    pub carriers: Vec<PathBuf>,
    /// Regular files inspected.
    pub total: usize,
}

/// Scan one directory level for regular files carrying `attr` (see [`Scan`]). An
/// unreadable `dir` is an empty scan: `total == 0` says nothing was inspected. The
/// reroute directory's marker file is not an executable — nothing execs it, so its tag
/// tracks nothing — and is never counted ([`crate::reroute::DIR_MARKER_FILE`]).
#[must_use]
pub fn tagged_files_in(dir: &Path, attr: &str) -> Scan {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Scan::default();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_name() != crate::reroute::DIR_MARKER_FILE)
        .map(|e| e.path())
        .filter(|p| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_file()))
        .collect();
    paths.sort();
    let total = paths.len();
    let carriers = paths.into_iter().filter(|p| carries(p, attr)).collect();
    Scan { carriers, total }
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
/// status, removes its own label and exits 0, like the lanes' wrapper
/// ([`crate::stage_helper`]): a job launchd keeps is one it re-runs.
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
/// afterwards is the verdict. `scratch` holds the job's status and logs (removed after).
/// Off macOS there is no tag, and this answers [`HealOutcome::Clean`] without looking.
#[must_use]
pub fn heal(roots: &[PathBuf], scratch: &Path) -> HealOutcome {
    heal_with(roots, scratch, PROVENANCE_XATTR)
}

/// [`heal`] for the attribute `attr` — a parameter only so a test can use one it is able to
/// set (`com.apple.provenance` cannot be minted by hand); production clears that one.
#[must_use]
pub(crate) fn heal_with(roots: &[PathBuf], scratch: &Path, attr: &str) -> HealOutcome {
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
    let mut job = crate::stage_helper::Job::prepare(scratch, "heal")?;
    let mut args: Vec<&std::ffi::OsStr> = vec![std::ffi::OsStr::new(attr)];
    args.extend(roots.iter().map(|r| r.as_os_str()));
    job.submit_platform("atpkg-heal", HEAL_SCRIPT, &args)?;
    Ok(job.wait_for_status(HEAL_TIMEOUT))
}

/// See the macOS body.
#[cfg(not(target_os = "macos"))]
fn run_heal_job(_roots: &[PathBuf], _scratch: &Path, _attr: &str) -> Result<Option<i32>, String> {
    Err(String::from("there is no launchd here"))
}

/// [`heal`] over the whole store ([`store_roots`]), then the records squared with it: a
/// `<build>.tracked-install` record beside a build whose files now measure clean is
/// removed, and so is one beside a build that is no longer active — nothing will ever
/// clear it otherwise. The caller holds the store lock. What every package pass runs at
/// its end, and what `aterm pkg repair` runs.
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
/// the records are squared exactly as after a heal that worked, while the files keep
/// whatever tag they had. A test that proves the real heal THROUGH A DOOR opts in for its
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
    reconcile_records(layout, outcome.left());
    outcome
}

/// Where a heal of `layout`'s files keeps its job: the lanes' scratch under that prefix
/// (`<prefix>/staging/.lanes/`), or the temp dir when it cannot be made.
fn heal_scratch(layout: &crate::store::Layout) -> PathBuf {
    let scratch = layout.prefix.join("staging").join(".lanes");
    if layout.ensure_dir(&scratch).is_ok() {
        scratch
    } else {
        std::env::temp_dir()
    }
}

/// Remove every `<build>.tracked-install` record that no longer describes a tagged build:
/// one beside a build that is not active, and one beside an active build none of whose
/// files is among `left` (what the heal could not clear). Returns the build dirs whose
/// records were removed.
pub fn reconcile_records(layout: &crate::store::Layout, left: &[PathBuf]) -> Vec<PathBuf> {
    let active: Vec<PathBuf> = crate::ops::active_builds(layout)
        .into_iter()
        .map(|(program, build)| layout.build_dir(&program, build))
        .collect();
    let mut removed = Vec::new();
    for build_dir in recorded_builds(layout) {
        let still_tagged =
            active.contains(&build_dir) && left.iter().any(|p| p.starts_with(&build_dir));
        if !still_tagged {
            crate::store::clear_tracked_install(&build_dir);
            removed.push(build_dir);
        }
    }
    removed
}

/// Every build dir with a `<build>.tracked-install` record beside it, in name order.
#[must_use]
pub fn recorded_builds(layout: &crate::store::Layout) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(programs) = std::fs::read_dir(layout.prefix.join("store")) else {
        return out;
    };
    for program in programs.flatten() {
        let Ok(entries) = std::fs::read_dir(program.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            if let Some(build) = name
                .to_str()
                .and_then(|n| n.strip_suffix(crate::store::TRACKED_INSTALL_SUFFIX))
                .filter(|b| !b.is_empty())
            {
                out.push(program.path().join(build));
            }
        }
    }
    out.sort();
    out
}

/// Clear the tag from `roots` under `layout`, which this tracked process just laid itself
/// because its lane could not run, and log one line only if some of it stays tagged — the
/// view and exec-root lanes' fallback ([`crate::seam`], [`crate::compat`]). `what` names
/// what was laid, for that line.
pub(crate) fn heal_laid(layout: &crate::store::Layout, what: &str, roots: &[PathBuf]) {
    let healed = heal(roots, &heal_scratch(layout));
    if !healed.is_clean() {
        log_line(&format!(
            "could not clear a macOS tag from {} of {what} ({}); `aterm pkg repair` tries again",
            count_of(healed.left().len(), "file"),
            healed.why().unwrap_or("no reason given")
        ));
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
/// attributes (a clean binary under a tracked parent is tracked; that is the whole
/// incident). Answered once per process: tracking is decided at exec and does not change.
///
/// `true` — tracked — when the probe cannot be written or inspected (2026-09-15; it
/// was `false`): a lane that cannot measure must route the write through the machinery
/// that measures its own outcome, never in-process where a tag would land unseen. The
/// probe is removed before returning.
///
/// An UNMEASURABLE probe is NOT cached (audit 2026-09-15). The ANSWER is a property of
/// the process, but the ability to take it is a property of the scratch dir handed in,
/// and the lanes hand in different ones: [`crate::lay::lay_executables`] measures in
/// `$TMPDIR`, [`crate::install::verify_and_stage`] in the staging dir beside the
/// archive, and the spawn seam's `reroute::lay` usually gets there first. Latching one
/// transient failure there — a `$TMPDIR` reaped between the write and the `listxattr`, a
/// momentary ENOSPC, a `$TMPDIR` this process cannot write — pinned "untracked" for the
/// life of the process, and the staging lane that followed, with a scratch dir that was
/// writable by construction, then laid every shim and staged every bundle in-process and
/// TAGGED, with nothing measuring what it laid — the v0.83.0 shape this module exists to
/// prevent (module doc). So the step at hand still proceeds as TRACKED (fail closed,
/// below), logs it once ([`log_line`]), and the next call measures again.
#[must_use]
pub fn process_is_tracked(scratch: &Path) -> bool {
    static MEASURED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if let Some(tracked) = MEASURED.get() {
        return *tracked;
    }
    let Some(tracked) = measure_tracked(scratch) else {
        // Once per process: every spawn seam with stubs to lay reaches this, and eight
        // identical lines would bury the one that matters.
        static NOTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !NOTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            // A diagnostic, for the log ([`log_line`]): a session's reroute lay reaches here.
            log_line(&format!(
                "could not check {} for the macOS tag; installing the safe way",
                scratch.display()
            ));
        }
        // FAIL CLOSED (2026-09-15): a lane that cannot measure is a lane that cannot see,
        // and the tag's whole failure mode is being invisible — a probe refused by a full
        // disk or a permission is answered by the untracked lane, which writes elsewhere
        // and MEASURES what it laid, never by an in-process write that would carry the
        // tag with nothing saying so. Not cached, so a transient failure costs one lane
        // round trip, not the process's whole life.
        return true;
    };
    // Whoever measured first wins a race; both took the same process-wide property.
    *MEASURED.get_or_init(|| tracked)
}

/// The un-cached measurement behind [`process_is_tracked`]: `Some(tagged)` when a probe
/// could be written and read, `None` when it could not.
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

    /// A quarantined BUNDLE makes its executable a quarantined carrier, and so does the
    /// executable itself — the Safari-download shape, which reads as untagged and is
    /// nevertheless tracked under every parent (2026-09-14).
    #[test]
    fn a_quarantined_bundle_or_executable_is_found_as_the_carrier() {
        let d = tmp("quarantine");
        let contents = d.join("aterm.app").join("Contents");
        std::fs::create_dir_all(contents.join("MacOS")).unwrap();
        let exe = contents.join("MacOS").join("aterm");
        std::fs::write(&exe, b"x").unwrap();
        assert_eq!(quarantined_carrier(&exe), None, "clean to begin with");
        // The attribute a browser sets is one a test CAN mint (unlike the provenance tag).
        set_xattr_for_test(&d.join("aterm.app"), QUARANTINE_XATTR, b"0083;0;Safari;").unwrap();
        assert_eq!(
            quarantined_carrier(&exe),
            Some(d.join("aterm.app")),
            "the bundle is the carrier"
        );
        let lone = d.join("atpkg");
        std::fs::write(&lone, b"x").unwrap();
        assert_eq!(quarantined_carrier(&lone), None);
        set_xattr_for_test(&lone, QUARANTINE_XATTR, b"0083;0;Safari;").unwrap();
        assert_eq!(quarantined_carrier(&lone), Some(lone.clone()));
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

    /// The directory scan counts regular files only, names the carriers sorted, and
    /// ignores symlinks and subdirectories.
    #[test]
    fn the_bin_scan_counts_regular_files_and_names_the_carriers() {
        let d = tmp("scan");
        let bin = d.join("bin");
        std::fs::create_dir_all(bin.join("subdir")).unwrap();
        for name in ["trustc", "targo", "ay"] {
            std::fs::write(bin.join(name), b"#!/bin/true\n").unwrap();
        }
        std::os::unix::fs::symlink("trustc", bin.join("rustc")).unwrap();
        set_xattr_for_test(&bin.join("trustc"), "user.aterm.probe", b"1").unwrap();
        set_xattr_for_test(&bin.join("ay"), "user.aterm.probe", b"1").unwrap();
        let scan = tagged_files_in(&bin, "user.aterm.probe");
        assert_eq!(scan.total, 3, "three regular files, one link, one dir");
        assert_eq!(scan.carriers, vec![bin.join("ay"), bin.join("trustc")]);
        // A different attribute name finds nothing.
        assert_eq!(
            tagged_files_in(&bin, "user.aterm.other").carriers,
            Vec::<PathBuf>::new()
        );
        // An unreadable dir inspects nothing — and says so through `total`.
        assert_eq!(
            tagged_files_in(&d.join("absent"), "user.aterm.probe"),
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
        // label's own removal is pinned by `stage_helper`'s platform-job test, which knows
        // it — other tests' heals of this process run beside this one).
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
        let tracked = carries_provenance(&tree.join("sub").join("file"));
        let own = std::process::Command::new("/usr/bin/xattr")
            .args(["-d", PROVENANCE_XATTR])
            .arg(tree.join("sub").join("file"))
            .status()
            .unwrap();
        if tracked {
            assert!(own.success(), "a tracked xattr -d reports success…");
            assert!(
                carries_provenance(&tree.join("sub").join("file")),
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
        assert!(!carries_provenance(&tree.join("sub").join("file")));
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
        assert_eq!(measured, carries_provenance(&witness));
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
        // The cached answer agrees with the fresh one.
        assert_eq!(process_is_tracked(&d), measured);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// An unwritable scratch cannot be measured — `None`, so the predicate answers `false`
    /// for the step at hand (it never routes on a measurement it did not take).
    #[test]
    fn an_unwritable_scratch_is_unmeasurable() {
        let d = tmp("unwritable");
        assert_eq!(measure_tracked(&d.join("absent").join("deeper")), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A probe that could not be TAKEN must not decide the process's answer: after a call
    /// with an unmeasurable scratch, a call with a measurable one still agrees with a
    /// direct measurement. The invariant is asserted in the order-independent form on
    /// purpose — the cache is process-wide and the tests in this module share it, so this
    /// must hold whether or not another test primed it first (audit 2026-09-15).
    #[test]
    fn an_unmeasurable_probe_does_not_pin_the_process_answer() {
        let d = tmp("unpinned");
        let _ = process_is_tracked(&d.join("absent").join("deeper"));
        let fresh = measure_tracked(&d).expect("a writable scratch dir must be measurable");
        assert_eq!(
            process_is_tracked(&d),
            fresh,
            "a scratch dir that could not be probed must not pin the process-wide answer"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// THE WALK, pure: the executable, every `.app` above it in any case, and the
    /// wrapper around `Contents/MacOS/` whatever it is called — nearest first, each once.
    /// The directories MEASURED not to matter (`Contents/`, `Contents/MacOS/`, a plain
    /// parent) are never looked at, and both bundles of a nested helper are, because the
    /// kernel charges the process to the OUTERMOST one (measured 2026-09-23; the table is
    /// on [`taint_candidates`]).
    #[test]
    fn the_taint_walk_names_the_executable_and_every_bundle_above_it() {
        let walk = |p: &str| -> Vec<String> {
            taint_candidates(Path::new(p))
                .map(|c| c.display().to_string())
                .collect()
        };
        assert_eq!(
            walk("/Applications/aterm.app/Contents/MacOS/aterm"),
            [
                "/Applications/aterm.app/Contents/MacOS/aterm",
                "/Applications/aterm.app"
            ],
            "the shipped app: the executable and its bundle once, not Contents/ or MacOS/"
        );
        assert_eq!(
            walk("/A/Outer.app/Contents/Helpers/Inner.app/Contents/MacOS/tool"),
            [
                "/A/Outer.app/Contents/Helpers/Inner.app/Contents/MacOS/tool",
                "/A/Outer.app/Contents/Helpers/Inner.app",
                "/A/Outer.app",
            ],
            "a nested helper: both bundles, nearest first — the OUTER one is the one \
             the kernel charges, so stopping at the nearest would miss it"
        );
        assert_eq!(
            walk("/A/aterm.App/Contents/MacOS/aterm"),
            ["/A/aterm.App/Contents/MacOS/aterm", "/A/aterm.App"],
            "a case variant of `.app` is charged (measured), so it is a candidate"
        );
        assert_eq!(
            walk("/A/Svc.xpc/Contents/MacOS/svc"),
            ["/A/Svc.xpc/Contents/MacOS/svc", "/A/Svc.xpc"],
            "a non-`.app` wrapper around Contents/MacOS/ is charged (measured)"
        );
        assert_eq!(
            walk("/A/Q/Contents/MacOS/tool"),
            ["/A/Q/Contents/MacOS/tool", "/A/Q"],
            "a wrapper with no extension measured clean, and is a candidate anyway: a \
             false positive costs a copy, a miss costs a tagged toolchain"
        );
        assert_eq!(
            walk("/A/O.app/Contents/XPCServices/S.xpc/Contents/MacOS/s"),
            [
                "/A/O.app/Contents/XPCServices/S.xpc/Contents/MacOS/s",
                "/A/O.app/Contents/XPCServices/S.xpc",
                "/A/O.app",
            ],
            "an XPC service inside an app: its wrapper and the app"
        );
        assert_eq!(
            walk("/A/C.app/bin/tool"),
            ["/A/C.app/bin/tool", "/A/C.app"],
            "an `.app` above counts wherever the executable sits inside it (measured)"
        );
        assert_eq!(
            walk("/Users//x/Library/Application Support/aterm/pkg/store/trust/9192/bin/trustc"),
            ["/Users//x/Library/Application Support/aterm/pkg/store/trust/9192/bin/trustc"],
            "a free-standing binary: itself alone"
        );
        assert_eq!(
            walk("/tmp/not-a-bundle.apps/bin/tool"),
            ["/tmp/not-a-bundle.apps/bin/tool"],
            "only an `app` extension counts, not a name that merely starts with it"
        );
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
