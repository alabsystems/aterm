// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Laying EXECUTABLES — the `bin/` shims, their `agents/` twins and `alab-` aliases, the
//! pending stubs, the reroute stubs, the tombstones — through the UNTRACKED launchd lane
//! when this process is provenance-tracked, and the ONE policy both lanes (this one and
//! the staging lane, [`crate::stage_helper`]) apply when the lane cannot run.
//!
//! # Why the shims need it (law m21)
//!
//! A `bin/<tool>` shim is a `#!/bin/sh` script the kernel execs, and `com.apple.provenance`
//! follows the EXECUTABLE: a tagged shim makes the tool it `exec`s a tracked process even
//! when the tool's own binary is clean, and even when the shell that typed the command is
//! not (measured 2026-09-12, `crate::provenance`). So a clean bundle behind tagged shims
//! is a clean bundle nobody runs: every file a user builds through `targo`, `trustc`,
//! `ay` — every object file, every proof — comes out tagged, and a release cut built
//! through them dies after the claim exactly as v0.83.0 did. The staging lane made the
//! bundle clean; this lane makes the way IN clean. Nothing a tracked process writes
//! itself escapes (`fork`, `posix_spawn`, `exec` into a clean image were all measured
//! tagged), so the shims are handed, like the bundle, to a job launchd spawns from this
//! binary — run in place when neither it nor an `.app` above it is tagged, from a clean
//! byte copy (of the binary, or of its whole bundle) when either is
//! ([`crate::stage_helper::plan_helper`]) — the one mechanism that measured clean.
//!
//! # The mechanism
//!
//! The tracked parent renders every file it wants laid — path and body — into a SPEC,
//! submits the shared one-shot job ([`crate::stage_helper::Job`]: run this binary — or a
//! clean `cat` copy of it — on [`HIDDEN_VERB`]), and the helper writes each file the way
//! the parent would have: a dotted sibling temp, mode `0755`, fsync, `rename(2)` — all
//! of it in the untracked process, because a tracked parent tags a clean file by merely
//! renaming or `chmod`ing it. The parent then MEASURES the first file back. Batched on
//! purpose: one job per laying pass (all of a build's shims and aliases at once, all the
//! reroute stubs at once), never one per file — a round trip is a fraction of a second,
//! a `repair` lays dozens.
//!
//! # When the lane cannot run
//!
//! A tracked process whose lane fails — no launchd, a job that never starts, a helper
//! that does not answer — writes the files itself, and a lane that answered but whose
//! files came back tagged keeps them as they are (they are complete). Either way the tag
//! is then cleared in place ([`crate::provenance::heal`]: one launchd job of platform
//! binaries, which no state of aterm's own bundle can make tracked), silently. Only a
//! heal that fails is said — one plain line for the log ([`crate::provenance`]'s
//! `log_line`), and `aterm pkg doctor` counts what is still tagged. The release cutter's
//! pre-claim gate (`gates::provenance_gate`) refuses a tagged `trustc`/`targo` BEFORE a
//! build number is burned whatever happened here.
//!
//! `[packages] tracked_install = "refuse"` in aterm.toml
//! ([`crate::config::PackagesConfig::tracked_install`]) — the ONE spelling
//! ([`tracked_policy_of`]) — refuses instead when the tag could not be cleared, for a
//! machine whose operator would rather have no toolchain than a tagged one (the rustup
//! view and the exec roots, [`crate::seam`] and [`crate::compat`], refuse as soon as their
//! lane cannot run). The key exists because an environment variable could not reach the
//! window's own passes, which are spawned from launchd's environment.
//! `ATPKG_REFUSE_TRACKED_INSTALL` (the one-run refusal) and `ATPKG_ALLOW_TRACKED_INSTALL`
//! (a no-op name for the default) are gone (2026-09-23, R2: no env alternatives).
//!
//! A binary that is not `atpkg`/`aterm` — a test harness, some other embedding of this
//! crate — has NO lane by construction ([`Lane::Unavailable`]: it would not serve the
//! verb) and writes in-process as it always did; that is a fact about the
//! binary, not a failure of the lane, and the product's one binary is always `aterm`.
//!
//! Sourced shell hooks (`~/.aterm/shell.d`) are deliberately NOT routed here: a sourced
//! file changes no process image, so its tag tracks nothing.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// The hidden verb the launchd job execs — machinery, not vocabulary: unlisted in help
/// and `VERBS`, dispatched before the store lock (its parent HOLDS that lock).
pub const HIDDEN_VERB: &str = "__lay-files";

/// First line of a spec file — a version stamp, so a stale copy of this binary never
/// misreads a newer spec.
const SPEC_HEADER: &str = "atpkg-lay-spec v1";

/// One executable to lay: where, and what bytes. Always mode `0755`, always temp+rename.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Executable {
    /// The destination path (absolute in production; the helper refuses a relative one).
    pub path: PathBuf,
    /// The file's bytes.
    pub body: Vec<u8>,
}

impl Executable {
    /// An executable at `path` with `body`.
    pub fn new(path: impl Into<PathBuf>, body: impl Into<Vec<u8>>) -> Self {
        Self {
            path: path.into(),
            body: body.into(),
        }
    }
}

// ---------------------------------------------------------------------------------------
// Policy.
// ---------------------------------------------------------------------------------------

/// What a tracked process does with files whose tag could not be cleared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackedPolicy {
    /// Refuse them: `[packages] tracked_install = "refuse"` — the cutter's own machine,
    /// which would rather have no toolchain than one that cannot cut a release.
    Refuse,
    /// Keep them, log one line, and — for a staged bundle — record it beside the build.
    /// The default; `[packages] tracked_install = "record"` names it.
    Allow,
}

/// The policy this process runs under: `[packages] tracked_install` from the
/// once-loaded config ([`crate::config::cached`]), through [`tracked_policy_of`].
#[must_use]
pub fn tracked_policy() -> TrackedPolicy {
    tracked_policy_of(crate::config::cached().tracked_install())
}

/// The policy `config` selects — what `[packages] tracked_install` spelled, `None` when
/// it spelled nothing admissible — and `Allow`, the default since 2026-09-14, when it
/// selects none. Pure, so the table is a unit test.
#[must_use]
pub fn tracked_policy_of(config: Option<TrackedPolicy>) -> TrackedPolicy {
    config.unwrap_or(TrackedPolicy::Allow)
}

/// The refusal under [`TrackedPolicy::Refuse`], when the tag could not be cleared: what
/// was not done (`what`, e.g. "installing this build" or "laying 12 executable(s)"), why,
/// and the switch that refused — the machine's key, its one spelling. Plain, and short:
/// the operator who set the switch is the one reading it.
#[must_use]
pub fn tracked_refusal(what: &str, why: &str) -> String {
    format!(
        "not done: {what} would leave files with a macOS tag (com.apple.provenance) that \
         release builds refuse, and it could not be cleared ({why}); remove \
         `tracked_install = \"refuse\"` from aterm.toml to allow it"
    )
}

// ---------------------------------------------------------------------------------------
// Which binary would serve the lane.
// ---------------------------------------------------------------------------------------

/// Whether this binary has an untracked lane at all: it must serve the hidden verbs,
/// which only the two production spellings do
/// ([`crate::stage_helper::exe_serves_hidden_verb`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lane {
    /// The helper the job runs: this binary, canonicalized (the real path is what the
    /// job execs, and the symlink a `~/.local/bin/aterm` is can carry a tag its target
    /// does not).
    Helper(PathBuf),
    /// No lane for this binary — a test harness, a foreign embedding. Says why.
    Unavailable(String),
}

/// The lane THIS binary offers.
#[must_use]
pub fn lane_for_this_binary() -> Lane {
    match std::env::current_exe().and_then(std::fs::canonicalize) {
        Ok(exe) if crate::stage_helper::exe_serves_hidden_verb(&exe) => Lane::Helper(exe),
        Ok(exe) => Lane::Unavailable(format!(
            "{} is not an atpkg/aterm binary, so it would not serve {HIDDEN_VERB}",
            exe.display()
        )),
        Err(e) => Lane::Unavailable(format!("cannot resolve this binary's own path: {e}")),
    }
}

// ---------------------------------------------------------------------------------------
// The spec: byte-safe, dependency-free.
// ---------------------------------------------------------------------------------------

/// Render the spec the helper reads: one `file=<hex path>=<hex body>` line per
/// executable, in order (the helper writes them in order — a caller that wants a marker
/// file down before the files it marks puts it first).
#[must_use]
pub fn encode_spec(files: &[Executable]) -> String {
    let mut out = String::from(SPEC_HEADER);
    out.push('\n');
    for f in files {
        out.push_str("file=");
        out.push_str(&crate::tree::hex(crate::call1(
            crate::platform::os_str_bytes,
            f.path.as_os_str(),
        )));
        out.push('=');
        out.push_str(&crate::tree::hex(&f.body));
        out.push('\n');
    }
    out
}

/// Parse a spec rendered by [`encode_spec`]. A foreign header, a malformed line, a bad
/// hex digit or a relative path refuses the whole spec — the helper must never lay a
/// file at a path it half-understood.
pub fn decode_spec(text: &str) -> Result<Vec<Executable>, String> {
    let mut lines = text.lines();
    if lines.next() != Some(SPEC_HEADER) {
        return Err(String::from("spec header missing or of another version"));
    }
    let mut files = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some(rest) = line.strip_prefix("file=") else {
            return Err(format!("malformed spec line: {line:?}"));
        };
        let Some((path, body)) = rest.split_once('=') else {
            return Err(format!("malformed file line: {line:?}"));
        };
        let path = path_of(unhex(path)?);
        if !path.is_absolute() {
            return Err(format!("spec path is not absolute: {}", path.display()));
        }
        files.push(Executable {
            path,
            body: unhex(body)?,
        });
    }
    Ok(files)
}

fn unhex(s: &str) -> Result<Vec<u8>, String> {
    let bytes = s.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return Err(format!("odd-length hex value: {s:?}"));
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks(2) {
        let hi = hex_nibble(pair[0]).ok_or_else(|| format!("bad hex digit in {s:?}"))?;
        let lo = hex_nibble(pair[1]).ok_or_else(|| format!("bad hex digit in {s:?}"))?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}

#[cfg(unix)]
fn path_of(raw: Vec<u8>) -> PathBuf {
    use std::os::unix::ffi::OsStringExt as _;
    PathBuf::from(std::ffi::OsString::from_vec(raw))
}

#[cfg(not(unix))]
fn path_of(raw: Vec<u8>) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(&raw).into_owned())
}

// ---------------------------------------------------------------------------------------
// Writing — the one discipline, in whichever process runs it.
// ---------------------------------------------------------------------------------------

/// Write ONE executable in THIS process: a dotted sibling temp (`.<name>.lay-<pid>`,
/// never swept by the reroute dir's walk), the bytes, mode `0755` set explicitly (so a
/// restrictive umask cannot lay a shim only its owner can run in a system prefix),
/// fsync, then `rename(2)` over the destination — a shim on the user's PATH is never
/// briefly absent or half-written. This is the discipline the platform shim writer, the
/// stub writer and the tombstone writer each restated; they now share it.
pub fn write_in_process(file: &Executable) -> io::Result<()> {
    let name = match crate::call1(Path::file_name, file.path.as_path()) {
        Some(name) => crate::call1(OsStr::to_str, name),
        None => None,
    }
    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "executable has no file name"))?;
    let mut tmp_name = String::from(".");
    tmp_name.push_str(name);
    tmp_name.push_str(".lay-");
    tmp_name.push_str(&crate::dec_u64(u64::from(std::process::id())));
    let tmp = file.path.with_file_name(tmp_name);
    let _ = std::fs::remove_file(&tmp);
    let written = (|| -> io::Result<()> {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o755);
        }
        let mut f = opts.open(&tmp)?;
        use std::io::Write as _;
        f.write_all(&file.body)?;
        f.sync_all()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
        }
        Ok(())
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    // Windows `rename` does not replace an existing file (the same brief window the
    // Windows shim writer documents; a shim there is per-user and serialized).
    #[cfg(windows)]
    let _ = std::fs::remove_file(&file.path);
    if let Err(e) = std::fs::rename(&tmp, &file.path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// The hidden verb's body: `__lay-files <spec-file>`. Reads the spec, writes every file
/// through [`write_in_process`] — in THIS (untracked) process, which is the whole point —
/// and answers in `<spec-dir>/result`: `ok\n<count>\n`, or `err\n<message>\n` naming the
/// first path that would not write (the files before it stay: each is a complete,
/// correct file). Temp + rename, so the parent never reads a half-written answer.
/// Touches nothing else: no config, no store lock, no status.
pub fn run_helper(args: &[std::ffi::OsString]) -> ExitCode {
    crate::stage_helper::arm_parent_watchdog();
    let Some(spec_path) = args.first().map(PathBuf::from) else {
        eprintln!("atpkg {HIDDEN_VERB}: usage: {HIDDEN_VERB} <spec-file>");
        return ExitCode::from(2);
    };
    let result_path = spec_path.with_file_name("result");
    let outcome = (|| -> Result<usize, String> {
        let text = std::fs::read_to_string(&spec_path)
            .map_err(|e| format!("read spec {}: {e}", spec_path.display()))?;
        let files = decode_spec(&text)?;
        for f in &files {
            write_in_process(f).map_err(|e| format!("write {}: {e}", f.path.display()))?;
        }
        Ok(files.len())
    })();
    let body = match &outcome {
        Ok(n) => format!("ok\n{n}\n"),
        Err(why) => format!("err\n{why}\n"),
    };
    let tmp = result_path.with_file_name("result.tmp");
    if std::fs::write(&tmp, body).is_err() || std::fs::rename(&tmp, &result_path).is_err() {
        eprintln!(
            "atpkg {HIDDEN_VERB}: cannot write {}",
            result_path.display()
        );
        return ExitCode::from(1);
    }
    if outcome.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

// ---------------------------------------------------------------------------------------
// The parent side.
// ---------------------------------------------------------------------------------------

/// Lay `files` through an untracked launchd job running `helper_exe` (in place, or from
/// a clean byte copy when the binary is tagged — [`crate::stage_helper::plan_helper`]),
/// then MEASURE: the first file is read back for the tag, and a tagged one is a lane
/// that ran without achieving its purpose — `Err`, like a lane that did not run.
/// `scratch` holds the job's spec, logs and any copy. `Err(reason)` is "the lane
/// could not do it"; the files it did write (all of them, on the measured-tagged path)
/// are complete files, which the caller keeps and clears ([`lay_executables_with`]).
#[cfg(target_os = "macos")]
pub fn lay_untracked(
    helper_exe: &Path,
    files: &[Executable],
    scratch: &Path,
) -> Result<(), String> {
    if files.is_empty() {
        return Ok(());
    }
    if !crate::stage_helper::exe_serves_hidden_verb(helper_exe) {
        return Err(format!(
            "{} is not an atpkg/aterm binary, so it would not serve {HIDDEN_VERB}",
            helper_exe.display()
        ));
    }
    let mut job = crate::stage_helper::Job::prepare(scratch, "lay-helper")?;
    std::fs::write(&job.spec, encode_spec(files)).map_err(|e| format!("write spec: {e}"))?;
    job.submit(helper_exe, HIDDEN_VERB)?;
    job.wait_for_result()?;
    match job.read_result()? {
        Ok(_count) => {}
        Err(refusal) => return Err(format!("the untracked helper refused: {refusal}")),
    }
    let witness = &files[0].path;
    match crate::provenance::xattr_names(witness) {
        Ok(names)
            if names
                .iter()
                .any(|n| n == crate::provenance::PROVENANCE_XATTR) =>
        {
            Err(format!(
                "the untracked lane laid the files, but {} still carries com.apple.provenance",
                witness.display()
            ))
        }
        Ok(_) => Ok(()),
        Err(e) => Err(format!(
            "the untracked lane answered, but {} cannot be inspected: {e}",
            witness.display()
        )),
    }
}

/// See the macOS body — there is no launchd and no tag anywhere else.
#[cfg(not(target_os = "macos"))]
pub fn lay_untracked(
    _helper_exe: &Path,
    _files: &[Executable],
    _scratch: &Path,
) -> Result<(), String> {
    Err(String::from("the untracked lane exists on macOS only"))
}

/// Lay `files` (mode `0755`, temp+rename each): in this process when it is not
/// provenance-tracked — measured, [`crate::provenance::process_is_tracked`] — or when
/// this binary has no lane ([`Lane::Unavailable`]); through the untracked launchd job
/// when it is tracked; and, when that lane fails, in this process with the tag cleared
/// afterwards ([`crate::provenance::heal`]) — refused under `[packages] tracked_install =
/// "refuse"` only if the tag stays. Empty `files` is a no-op that measures nothing.
pub fn lay_executables(files: &[Executable]) -> io::Result<()> {
    if files.is_empty() {
        return Ok(());
    }
    let tracked =
        cfg!(target_os = "macos") && crate::provenance::process_is_tracked(&std::env::temp_dir());
    lay_executables_with(
        files,
        tracked,
        &lane_for_this_binary(),
        tracked_policy(),
        crate::provenance::heal,
    )
}

/// [`lay_executables`] with its inputs explicit — the tracking measurement, the lane, the
/// policy and the heal — so every branch is exercisable from a test that is not itself in
/// a position to be tracked.
pub fn lay_executables_with(
    files: &[Executable],
    tracked: bool,
    lane: &Lane,
    policy: TrackedPolicy,
    heal: crate::provenance::Healer,
) -> io::Result<()> {
    if files.is_empty() {
        return Ok(());
    }
    if !tracked {
        return write_all_in_process(files);
    }
    let what = format!("laying {} executable(s)", files.len());
    let helper = match lane {
        Lane::Helper(exe) => exe,
        // A fact about the binary, not a failure of the lane (module doc) — but a
        // tracked process under the REFUSE policy is told, not quietly served: an
        // operator who asked for no tagged files gets none from a binary that has no
        // way to lay clean ones either (audit 2026-09-14).
        Lane::Unavailable(why) => {
            return match policy {
                TrackedPolicy::Refuse => Err(io::Error::other(tracked_refusal(
                    &what,
                    &format!("this binary cannot lay them clean: {why}"),
                ))),
                TrackedPolicy::Allow => write_all_in_process(files),
            };
        }
    };
    // The lanes' scratch is `<prefix>/staging/.lanes/` (2026-09-15), never the shared
    // per-user `$TMPDIR`: the dead-job sweep `Job::prepare` runs there walks only
    // directories atpkg created, and takes only the stems this crate submits
    // (`stage_helper::JOB_STEMS`) even so. No store (no `HOME`): the temp dir, as before.
    let scratch = crate::stage_helper::lanes_scratch().unwrap_or_else(std::env::temp_dir);
    let Err(lane_why) = lay_untracked(helper, files, &scratch) else {
        return Ok(());
    };
    // THE LANE DID NOT LAY THEM CLEAN. Whatever it did lay is complete — it writes each
    // file temp+rename — so only what is missing or different is written here, never a
    // second copy of what it laid tagged (the old in-process rewrite came back tagged
    // too), and then everything is cleared in place.
    //
    // Under `Refuse` a file that STANDS is not replaced on the hope of a heal: a refusal
    // cannot put it back, so a heal that then failed would leave the shim gone. The heal
    // proves itself first, on a probe this process tags: one that cannot clear it is
    // refused with nothing touched, as every refusal was before the heal existed.
    if policy == TrackedPolicy::Refuse
        && files
            .iter()
            .any(|f| f.path.symlink_metadata().is_ok() && !laid_as_asked(f))
        && let Err(heal_why) = heal_works_in(&scratch, heal)
    {
        return Err(io::Error::other(tracked_refusal(
            &what,
            &format!("{lane_why}; {heal_why}"),
        )));
    }
    let created: Vec<PathBuf> = files
        .iter()
        .filter(|f| f.path.symlink_metadata().is_err())
        .map(|f| f.path.clone())
        .collect();
    write_differing_in_process(files)?;
    let paths: Vec<PathBuf> = files.iter().map(|f| f.path.clone()).collect();
    let healed = heal(&paths, &scratch);
    if healed.is_clean() {
        return Ok(());
    }
    let why = format!(
        "{lane_why}; {}",
        healed.why().unwrap_or("the tag could not be cleared")
    );
    match policy {
        TrackedPolicy::Refuse => {
            // Refused means not laid by this step: what it created goes again.
            for path in &created {
                let _ = std::fs::remove_file(path);
            }
            Err(io::Error::other(tracked_refusal(&what, &why)))
        }
        TrackedPolicy::Allow => {
            crate::provenance::log_line(&format!(
                "could not clear a macOS tag from {} it laid ({why}); `aterm pkg repair` \
                 tries again",
                crate::provenance::count_of(healed.left().len(), "file")
            ));
            Ok(())
        }
    }
}

fn write_all_in_process(files: &[Executable]) -> io::Result<()> {
    for f in files {
        write_in_process(f)?;
    }
    Ok(())
}

/// Write, in this process, each of `files` not already on disk as asked — the exact bytes
/// at mode `0755`.
fn write_differing_in_process(files: &[Executable]) -> io::Result<()> {
    for f in files {
        if !laid_as_asked(f) {
            write_in_process(f)?;
        }
    }
    Ok(())
}

/// Whether `heal` clears the tag here: a probe file this process writes into `scratch` —
/// tagged, as everything a tracked process writes is — handed to it, then removed. `Err`
/// says why not.
fn heal_works_in(scratch: &Path, heal: crate::provenance::Healer) -> Result<(), String> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let probe = scratch.join(format!(".heal-probe-{}-{seq}", std::process::id()));
    std::fs::write(&probe, b"probe")
        .map_err(|e| format!("the heal could not be tried ({}): {e}", probe.display()))?;
    let healed = heal(std::slice::from_ref(&probe), scratch);
    let _ = std::fs::remove_file(&probe);
    if healed.is_clean() {
        Ok(())
    } else {
        Err(healed
            .why()
            .unwrap_or("the tag could not be cleared")
            .to_string())
    }
}

/// Whether `file` is on disk as a regular file with its bytes, at mode `0755`.
fn laid_as_asked(file: &Executable) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(&file.path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if meta.permissions().mode() & 0o777 != 0o755 {
            return false;
        }
    }
    std::fs::read(&file.path).is_ok_and(|bytes| bytes == file.body)
}

/// [`lay_executables`] for one file — the shape the per-file writers keep.
pub fn lay_executable(path: &Path, body: &[u8]) -> io::Result<()> {
    lay_executables(&[Executable::new(path, body)])
}

/// Whether a lay from THIS process could produce files FREE of `com.apple.provenance` —
/// the question a caller must ask before re-laying a file whose only fault is the tag.
///
/// An untracked process writes clean itself; a tracked one can only write clean through
/// the untracked launchd lane, which needs a binary that serves [`HIDDEN_VERB`]
/// ([`Lane::Helper`]). A tracked process with [`Lane::Unavailable`] — a test harness, a
/// foreign embedding — has no way to clear the tag AT ALL: every byte it lays comes back
/// tagged (measured; nothing a tracked process writes escapes, module doc), so a rewrite
/// it performs to clear the tag is guaranteed churn. Optimistic about a `Helper` lane
/// that then fails: that pass writes in-process under [`TrackedPolicy::Allow`], says so,
/// and the next pass tries the lane again — which is the design's intent, a tag being
/// worth retrying for.
#[must_use]
pub fn lay_clears_provenance() -> bool {
    let tracked =
        cfg!(target_os = "macos") && crate::provenance::process_is_tracked(&std::env::temp_dir());
    lay_clears_provenance_with(tracked, &lane_for_this_binary())
}

/// Whether a file of ours at `path` that is CURRENT in content but carries the tag is worth
/// re-laying now: a lay from this process can come back clean ([`lay_clears_provenance`]),
/// and this very file — path and inode — was not already re-laid once and came back tagged
/// ([`note_relayed`]). One attempt per file instance: a lane whose file returns tagged
/// re-laid identical bytes through a launchd job every pass, for ever (the reroute marker,
/// 2026-09-19..22), while a file that is laid anew — a self-update's, a fallback's — gets
/// its one try. The memo is read only for a tagged file.
#[must_use]
pub fn relay_worth_trying(layout: &crate::store::Layout, path: &Path) -> bool {
    lay_clears_provenance() && !relay_tried(&read_relay_memo(layout), path)
}

/// After laying `paths`: remember each that came back TAGGED, by path and inode, so no pass
/// re-lays those bytes again ([`relay_worth_trying`]); forget each that came back clean or
/// is gone. The memo (`<prefix>/tag-relay.tried`, `<inode>\t<path>` per line) is written
/// only when it changes. Best-effort.
pub fn note_relayed(layout: &crate::store::Layout, paths: &[PathBuf]) {
    note_relayed_with(layout, paths, crate::provenance::carries_provenance);
}

/// [`note_relayed`] with the tag probe explicit — no test can put the kernel's tag on a file.
fn note_relayed_with(
    layout: &crate::store::Layout,
    paths: &[PathBuf],
    tagged: impl Fn(&Path) -> bool,
) {
    let had = read_relay_memo(layout);
    let mut memo: Vec<(String, String)> = had
        .iter()
        .filter(|(_, p)| !paths.iter().any(|q| q.to_string_lossy() == p.as_str()))
        .cloned()
        .collect();
    for path in paths {
        if let Some(id) = file_instance(path)
            && tagged(path)
        {
            memo.push((id, path.to_string_lossy().into_owned()));
        }
    }
    memo.sort();
    let mut sorted_had = had;
    sorted_had.sort();
    if memo == sorted_had {
        return;
    }
    let target = relay_memo_path(layout);
    if memo.is_empty() {
        let _ = std::fs::remove_file(&target);
        return;
    }
    let mut body = String::new();
    for (id, path) in &memo {
        body.push_str(id);
        body.push('\t');
        body.push_str(path);
        body.push('\n');
    }
    let tmp = target.with_file_name(format!("tag-relay.tried.tmp-{}", std::process::id()));
    if crate::call2(std::fs::write, &tmp, body).is_err() || std::fs::rename(&tmp, &target).is_err()
    {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// `<prefix>/tag-relay.tried` ([`note_relayed`]).
fn relay_memo_path(layout: &crate::store::Layout) -> PathBuf {
    layout.prefix.join("tag-relay.tried")
}

/// The memo's `(instance, path)` rows; empty when absent or unreadable.
fn read_relay_memo(layout: &crate::store::Layout) -> Vec<(String, String)> {
    crate::metadata_io::read_bounded_regular_utf8(&relay_memo_path(layout), 256 * 1024)
        .map(|text| {
            text.lines()
                .filter_map(|l| l.split_once('\t'))
                .map(|(id, p)| (id.to_string(), p.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// Whether `memo` holds the file now at `path` — the same path at the same inode.
fn relay_tried(memo: &[(String, String)], path: &Path) -> bool {
    let Some(id) = file_instance(path) else {
        return false;
    };
    let path = path.to_string_lossy();
    memo.iter().any(|(i, p)| *i == id && *p == path)
}

/// The identity of the file instance at `path`: its inode (a re-lay renames a new one over
/// it), or its modification time where there are no inodes.
fn file_instance(path: &Path) -> Option<String> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        Some(meta.ino().to_string())
    }
    #[cfg(not(unix))]
    {
        let modified = meta.modified().ok()?;
        let since = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
        Some(since.as_nanos().to_string())
    }
}

/// [`lay_clears_provenance`] with its two inputs explicit — the tracking measurement and
/// the lane — so both branches are exercisable from a test process that is in no
/// position to choose whether it is tracked ([`lay_executables_with`]'s shape).
#[must_use]
pub fn lay_clears_provenance_with(tracked: bool, lane: &Lane) -> bool {
    !tracked || matches!(lane, Lane::Helper(_))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(label: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("atpkg-lay-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn files(d: &Path) -> Vec<Executable> {
        vec![
            Executable::new(d.join("targo"), "#!/bin/sh\nexec '/s/bin/targo' \"$@\"\n"),
            Executable::new(
                d.join("alab-targo"),
                "#!/bin/sh\nexec '/s/bin/targo' \"$@\"\n",
            ),
            Executable::new(d.join("with space"), b"\x00\xff binary body\n".to_vec()),
        ]
    }

    /// ONE TAG-DRIVEN RE-LAY PER FILE: a re-lay that came back tagged is remembered by path
    /// and inode, and that very file is never re-laid for its tag again — the loop the
    /// reroute marker ran every pass (2026-09-19..22). A file laid anew (a new inode) gets
    /// its one try; one that came back clean is forgotten; an unchanged memo is not written.
    #[cfg(unix)]
    #[test]
    fn a_relay_that_came_back_tagged_is_not_tried_again_for_the_same_file() {
        let d = tmp("relay-memo");
        let layout = crate::store::Layout { prefix: d.clone() };
        let twin = d.join("claude");
        std::fs::write(&twin, "#!/bin/sh\n").unwrap();
        assert!(
            !relay_tried(&read_relay_memo(&layout), &twin),
            "never tried"
        );
        note_relayed_with(&layout, std::slice::from_ref(&twin), |_| true);
        assert!(
            relay_tried(&read_relay_memo(&layout), &twin),
            "came back tagged"
        );
        let memo = relay_memo_path(&layout);
        let written = std::fs::metadata(&memo).unwrap().modified().unwrap();
        note_relayed_with(&layout, std::slice::from_ref(&twin), |_| true);
        assert_eq!(
            std::fs::metadata(&memo).unwrap().modified().unwrap(),
            written,
            "unchanged: not rewritten"
        );
        // Laid anew — a rename puts a new inode at the path — it gets its one try.
        let next = d.join(".claude.new");
        std::fs::write(&next, "#!/bin/sh\n# v2\n").unwrap();
        std::fs::rename(&next, &twin).unwrap();
        assert!(!relay_tried(&read_relay_memo(&layout), &twin), "a new file");
        // A re-lay that came back clean is forgotten, and an empty memo goes.
        note_relayed_with(&layout, std::slice::from_ref(&twin), |_| false);
        assert!(!memo.exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Who can lay a file free of the tag: anyone untracked, and a tracked process only
    /// through a `Helper` lane. A tracked process with no lane can never clear it, so
    /// nothing should rewrite a file on its behalf.
    #[test]
    fn only_an_untracked_process_or_a_real_lane_can_lay_a_file_clean() {
        let helper = Lane::Helper(PathBuf::from("/Applications/aterm.app/…/aterm"));
        let none = Lane::Unavailable(String::from("a test harness"));
        assert!(lay_clears_provenance_with(false, &helper));
        assert!(lay_clears_provenance_with(false, &none));
        assert!(lay_clears_provenance_with(true, &helper));
        assert!(
            !lay_clears_provenance_with(true, &none),
            "tracked and laneless: every byte it lays comes back tagged"
        );
    }

    /// Every file round-trips — a path with a space, a body with a NUL and a non-UTF-8
    /// byte — in the order given.
    #[test]
    fn the_spec_round_trips_byte_for_byte_in_order() {
        let d = tmp("spec");
        let text = encode_spec(&files(&d));
        assert!(text.starts_with(SPEC_HEADER), "{text}");
        assert_eq!(decode_spec(&text).unwrap(), files(&d));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The refusals: a foreign header, a line that is not `file=`, a relative path, an
    /// odd hex digit — none of them yields a list.
    #[test]
    fn a_spec_the_helper_half_understands_is_refused_whole() {
        let d = tmp("refuse");
        let good = encode_spec(&files(&d));
        assert!(decode_spec("atpkg-lay-spec v0\n").is_err());
        assert!(decode_spec(&good.replace("file=", "shim=")).is_err());
        assert!(
            decode_spec(&good.replacen("file=2f", "file=61", 1)).is_err(),
            "relative"
        );
        let bad_hex = format!("{good}file=zz=00\n");
        assert!(decode_spec(&bad_hex).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The table (2026-09-23), every row: the key decides, and none is ALLOW — the
    /// default since 2026-09-14, when a refused install left the owner's machine without
    /// a toolchain. No environment variable is an input any more.
    #[test]
    fn the_policy_is_the_key_then_allow() {
        use TrackedPolicy::{Allow, Refuse};
        assert_eq!(tracked_policy_of(None), Allow);
        assert_eq!(tracked_policy_of(Some(Allow)), Allow);
        assert_eq!(tracked_policy_of(Some(Refuse)), Refuse);
    }

    /// The config side feeds the table through `PackagesConfig::tracked_install`, so
    /// the two modules agree on which word is which policy — pinned here, where the
    /// policy lives, so a rename on either side fails a test in both.
    #[test]
    fn the_config_key_spells_the_two_policies() {
        let of =
            |toml: &str| tracked_policy_of(crate::config::parse_packages(toml).tracked_install());
        assert_eq!(of(""), TrackedPolicy::Allow);
        assert_eq!(
            of("[packages]\ntracked_install = \"record\"\n"),
            TrackedPolicy::Allow
        );
        assert_eq!(
            of("[packages]\ntracked_install = \"refuse\"\n"),
            TrackedPolicy::Refuse
        );
        assert_eq!(
            of("[packages]\ntracked_install = \"refuze\"\n"),
            TrackedPolicy::Allow,
            "a word the owner did not write does not refuse"
        );
    }

    /// The in-process writer: mode 0755, the exact bytes, an existing file replaced, no
    /// temp left behind.
    #[cfg(unix)]
    #[test]
    fn the_in_process_writer_lays_0755_atomically() {
        use std::os::unix::fs::PermissionsExt as _;
        let d = tmp("write");
        let dest = d.join("tool");
        std::fs::write(&dest, "old\n").unwrap();
        let f = Executable::new(&dest, "#!/bin/sh\necho new\n");
        write_in_process(&f).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), f.body);
        assert_eq!(
            std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777,
            0o755
        );
        let leftovers: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with('.'))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The helper verb lays every file and answers the count through the result file;
    /// a bad spec is answered with `err`, not a silent exit.
    #[test]
    fn the_helper_lays_the_files_and_answers_through_the_result_file() {
        let d = tmp("helper");
        let job = d.join("job");
        std::fs::create_dir_all(&job).unwrap();
        let dest = d.join("dest");
        std::fs::create_dir_all(&dest).unwrap();
        let wanted = files(&dest);
        let spec = job.join("spec");
        std::fs::write(&spec, encode_spec(&wanted)).unwrap();
        let code = run_helper(&[spec.clone().into_os_string()]);
        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(
            std::fs::read_to_string(job.join("result")).unwrap(),
            "ok\n3\n"
        );
        for f in &wanted {
            assert_eq!(
                std::fs::read(&f.path).unwrap(),
                f.body,
                "{}",
                f.path.display()
            );
        }
        std::fs::write(&spec, "not a spec\n").unwrap();
        let code = run_helper(&[spec.clone().into_os_string()]);
        assert_eq!(code, ExitCode::from(1));
        let result = std::fs::read_to_string(job.join("result")).unwrap();
        assert!(result.starts_with("err\n"), "{result}");
        assert!(!job.join("result.tmp").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A heal that clears what it was handed.
    #[cfg(target_os = "macos")]
    fn heal_clears(_roots: &[PathBuf], _scratch: &Path) -> crate::provenance::HealOutcome {
        crate::provenance::HealOutcome::Healed { cleared: 3 }
    }

    /// A heal that could not clear what it was handed.
    #[cfg(target_os = "macos")]
    fn heal_fails(roots: &[PathBuf], _scratch: &Path) -> crate::provenance::HealOutcome {
        crate::provenance::HealOutcome::Left {
            carriers: roots.to_vec(),
            why: String::from("xattr exited 1"),
        }
    }

    /// A lay that needs no clearing never asks.
    #[cfg(target_os = "macos")]
    fn heal_not_asked(_roots: &[PathBuf], _scratch: &Path) -> crate::provenance::HealOutcome {
        panic!("a lay with nothing to clear must not run the heal")
    }

    /// The decision table, with the lane's outcome and the heal's forced: an untracked
    /// process writes in-process whatever the lane; a tracked one with no lane for its
    /// binary writes in-process, or is refused under `Refuse`; a tracked one whose lane
    /// FAILS writes the files and clears the tag — laid, under either policy, when the tag
    /// clears; when it does not, refused under `Refuse` (nothing it created is left, and a
    /// file that stood is never replaced) and kept under `Allow`, the default.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_tracked_process_whose_lane_fails_heals_and_refuses_only_what_stays_tagged() {
        let d = tmp("policy");
        let dest = d.join("dest");
        std::fs::create_dir_all(&dest).unwrap();
        let wanted = files(&dest);
        // A helper that is spelled right but cannot answer: `/usr/bin/true` under the name
        // `atpkg` exits 0 without a result — a real lane failure, detected inside the
        // exit grace.
        let fake_dir = d.join("fake");
        std::fs::create_dir_all(&fake_dir).unwrap();
        let fake = fake_dir.join("atpkg");
        std::fs::copy("/usr/bin/true", &fake).unwrap();
        let broken = Lane::Helper(fake);
        let none = Lane::Unavailable(String::from("a test harness"));
        let remove_all = || {
            for f in &wanted {
                let _ = std::fs::remove_file(&f.path);
            }
        };

        // Untracked: in-process, no lane consulted, nothing to clear.
        lay_executables_with(
            &wanted,
            false,
            &broken,
            TrackedPolicy::Refuse,
            heal_not_asked,
        )
        .unwrap();
        remove_all();
        // Tracked, no lane for this binary: in-process under the default, REFUSED under
        // `Refuse` — an operator who asked for no tagged files gets none from a binary
        // that cannot lay clean ones (2026-09-15).
        lay_executables_with(&wanted, true, &none, TrackedPolicy::Allow, heal_not_asked).unwrap();
        remove_all();
        let err = lay_executables_with(&wanted, true, &none, TrackedPolicy::Refuse, heal_not_asked)
            .expect_err("tracked, no lane, refuse policy: refused");
        assert!(err.to_string().contains("cannot lay them clean"), "{err}");
        for f in &wanted {
            assert!(!f.path.exists(), "refused means not laid");
        }
        // Tracked, lane fails, the tag clears: laid, under either policy.
        for policy in [TrackedPolicy::Refuse, TrackedPolicy::Allow] {
            lay_executables_with(&wanted, true, &broken, policy, heal_clears).unwrap();
            for f in &wanted {
                assert_eq!(std::fs::read(&f.path).unwrap(), f.body);
            }
            remove_all();
        }
        // Tracked, lane fails, the tag stays, `Refuse` opted into: REFUSED, nothing left.
        let err = lay_executables_with(&wanted, true, &broken, TrackedPolicy::Refuse, heal_fails)
            .expect_err("a tag that stays is refused under Refuse");
        let msg = err.to_string();
        assert!(msg.starts_with("not done: laying 3 executable(s)"), "{msg}");
        assert!(msg.contains("exited without a result"), "the cause: {msg}");
        assert!(msg.contains("xattr exited 1"), "why it stayed: {msg}");
        assert!(
            msg.contains("tracked_install = \"refuse\"") && !msg.contains("ATPKG_"),
            "the one key that refused, and no environment knob: {msg}"
        );
        for f in &wanted {
            assert!(
                !f.path.exists(),
                "refused means not laid: {}",
                f.path.display()
            );
        }
        // …and a file that STANDS is not replaced on the hope of a heal: refused under
        // `Refuse` by a heal that cannot clear a probe, it is exactly as it was — and a heal
        // that can clear it lets the new bytes in.
        let ino = |p: &Path| {
            use std::os::unix::fs::MetadataExt as _;
            std::fs::metadata(p).unwrap().ino()
        };
        std::fs::write(&wanted[0].path, b"standing").unwrap();
        let standing = ino(&wanted[0].path);
        let err = lay_executables_with(&wanted, true, &broken, TrackedPolicy::Refuse, heal_fails)
            .expect_err("a heal that cannot clear is refused before anything is replaced");
        assert!(err.to_string().contains("xattr exited 1"), "{err}");
        assert_eq!(std::fs::read(&wanted[0].path).unwrap(), b"standing");
        assert_eq!(
            ino(&wanted[0].path),
            standing,
            "the standing file was never replaced"
        );
        assert!(!wanted[1].path.exists() && !wanted[2].path.exists());
        lay_executables_with(&wanted, true, &broken, TrackedPolicy::Refuse, heal_clears).unwrap();
        for f in &wanted {
            assert_eq!(std::fs::read(&f.path).unwrap(), f.body);
        }
        remove_all();
        // Tracked, lane fails, the tag stays, the default: kept.
        assert_eq!(tracked_policy_of(None), TrackedPolicy::Allow);
        lay_executables_with(&wanted, true, &broken, TrackedPolicy::Allow, heal_fails).unwrap();
        for f in &wanted {
            assert_eq!(std::fs::read(&f.path).unwrap(), f.body);
        }
        // What is already laid as asked is never rewritten: only a file that differs is.
        let before: Vec<u64> = wanted.iter().map(|f| ino(&f.path)).collect();
        std::fs::write(&wanted[2].path, b"stale").unwrap();
        let stale = ino(&wanted[2].path);
        lay_executables_with(&wanted, true, &broken, TrackedPolicy::Allow, heal_clears).unwrap();
        assert_eq!(ino(&wanted[0].path), before[0], "laid as asked: left alone");
        assert_eq!(ino(&wanted[1].path), before[1], "laid as asked: left alone");
        assert_ne!(
            ino(&wanted[2].path),
            stale,
            "a file that differs is written"
        );
        assert_eq!(std::fs::read(&wanted[2].path).unwrap(), wanted[2].body);
        // The lane's job scratch and label are gone either way.
        let leftovers: Vec<_> = std::fs::read_dir(std::env::temp_dir())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(&format!("lay-helper-{}-", std::process::id())))
            .collect();
        assert!(
            leftovers.is_empty(),
            "job scratch left behind: {leftovers:?}"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The refusal is short and plain: what was not done, why, and the switch that
    /// refused — the machine's key, its one spelling; no environment knob.
    #[test]
    fn the_refusal_names_what_was_not_done_why_and_the_switch() {
        let msg = tracked_refusal("installing this build", "xattr exited 1");
        assert!(msg.starts_with("not done: installing this build"), "{msg}");
        assert!(msg.contains("(xattr exited 1)"), "{msg}");
        assert!(msg.contains("com.apple.provenance"), "{msg}");
        assert!(msg.contains("tracked_install = \"refuse\""), "{msg}");
        assert!(!msg.contains("ATPKG_"), "no environment knob: {msg}");
        assert!(msg.chars().count() < 300, "{msg}");
        assert!(!msg.contains("untracked") && !msg.contains("lane"), "{msg}");
    }
}
