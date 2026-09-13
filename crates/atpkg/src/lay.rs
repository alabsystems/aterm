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
//! tagged), so the shims are handed, like the bundle, to a job launchd spawns from a
//! clean byte copy of this binary — the one mechanism that measured clean.
//!
//! # The mechanism
//!
//! The tracked parent renders every file it wants laid — path and body — into a SPEC,
//! submits the shared one-shot job ([`crate::stage_helper::Job`]: `cat` this binary to a
//! clean copy, exec the copy on [`HIDDEN_VERB`]), and the copy writes each file the way
//! the parent would have: a dotted sibling temp, mode `0755`, fsync, `rename(2)` — all
//! of it in the untracked process, because a tracked parent tags a clean file by merely
//! renaming or `chmod`ing it. The parent then MEASURES the first file back. Batched on
//! purpose: one job per laying pass (all of a build's shims and aliases at once, all the
//! reroute stubs at once), never one per file — a round trip is a fraction of a second,
//! a `repair` lays dozens.
//!
//! # The policy when the lane cannot run
//!
//! A tracked process whose lane fails — no launchd, a job that never starts, a helper
//! that does not answer, a file that came back tagged anyway — does NOT quietly write
//! the files itself: that is the v0.83.0 shape reproduced by a bug, with nothing on
//! disk to say so. By default it REFUSES ([`TrackedPolicy::Refuse`]), naming why the
//! lane failed and the two ways out; `ATPKG_ALLOW_TRACKED_INSTALL=1`
//! ([`ALLOW_TRACKED_ENV`]) is the escape hatch that accepts tagged files — for an
//! operator who would rather have a toolchain that cannot cut a release than none —
//! and the staging lane RECORDS that choice beside the bundle (`<build>.tracked-install`)
//! for `aterm pkg doctor` and `aterm pkg repair` to name; tagged shims need no record,
//! the doctor's `bin/` scan sees them directly.
//!
//! A binary that is not `atpkg`/`aterm` — a test harness, some other embedding of this
//! crate — has NO lane by construction ([`Lane::Unavailable`]: a copy of it would not
//! serve the verb) and writes in-process as it always did; that is a fact about the
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

/// The escape hatch: set (non-empty) to let a provenance-tracked installer whose
/// untracked lane cannot run write tagged files itself — the staged bundle recorded
/// beside it as `<build>.tracked-install`, the shims visible to `aterm pkg doctor`'s
/// `bin/` scan. Off by default: the default is to refuse.
pub const ALLOW_TRACKED_ENV: &str = "ATPKG_ALLOW_TRACKED_INSTALL";

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

/// What a tracked process does when its untracked lane cannot run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackedPolicy {
    /// Refuse, naming why the lane failed and the escape hatch. The default.
    Refuse,
    /// Write the files in-process (tagged), say so once, and — for a staged bundle —
    /// record it beside the build. `ATPKG_ALLOW_TRACKED_INSTALL=1`.
    Allow,
}

/// The policy this process runs under: [`ALLOW_TRACKED_ENV`] non-empty is `Allow`.
#[must_use]
pub fn tracked_policy() -> TrackedPolicy {
    tracked_policy_of(std::env::var_os(ALLOW_TRACKED_ENV).as_deref())
}

/// The policy a value of [`ALLOW_TRACKED_ENV`] selects: unset or empty is `Refuse`.
#[must_use]
pub fn tracked_policy_of(value: Option<&OsStr>) -> TrackedPolicy {
    if value.is_some_and(|v| !v.is_empty()) {
        TrackedPolicy::Allow
    } else {
        TrackedPolicy::Refuse
    }
}

/// The refusal both lanes print under [`TrackedPolicy::Refuse`]: what could not be done
/// (`what`, e.g. "stage the bundle" or "lay 12 executable(s)"), why the lane failed,
/// what the tag breaks, and the two ways out.
#[must_use]
pub fn tracked_refusal(what: &str, why: &str) -> String {
    format!(
        "this process is provenance-tracked (a probe file it wrote came back carrying \
         com.apple.provenance) and the untracked launchd lane could not {what} ({why}) — \
         refusing rather than write files that would all carry the tag: {}. fix: run this \
         from an untracked process — Terminal.app, or `launchctl submit -l aterm-pkg -- \
         <path to atpkg> <verb…>` — or set {ALLOW_TRACKED_ENV}=1 to accept tagged files (a \
         bundle staged that way is recorded beside it as <build>.tracked-install, and \
         `aterm pkg doctor` names it and every tagged shim)",
        crate::provenance::WHAT_IT_BREAKS
    )
}

/// The one stderr line printed when [`TrackedPolicy::Allow`] takes the in-process path.
fn allowed_note(what: &str, why: &str) {
    eprintln!(
        "atpkg: note — this process is provenance-tracked and the untracked lane could not \
         {what} ({why}); {ALLOW_TRACKED_ENV} is set, so the files are written in-process and \
         WILL carry com.apple.provenance — `aterm pkg doctor` names what that breaks"
    );
}

// ---------------------------------------------------------------------------------------
// Which binary would serve the lane.
// ---------------------------------------------------------------------------------------

/// Whether this binary has an untracked lane at all: a copy of it must serve the hidden
/// verbs, which only the two production spellings do
/// ([`crate::stage_helper::exe_serves_hidden_verb`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lane {
    /// The helper to copy: this binary, canonicalized.
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
            "{} is not an atpkg/aterm binary, so a copy of it would not serve {HIDDEN_VERB}",
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
pub fn run_helper(args: &[String]) -> ExitCode {
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

/// Lay `files` through an untracked launchd job running a clean byte copy of
/// `helper_exe`, then MEASURE: the first file is read back for the tag, and a tagged one
/// is a lane that ran without achieving its purpose — `Err`, like a lane that did not
/// run. `scratch` holds the job's spec, logs and the copy. `Err(reason)` is "the lane
/// could not do it"; the files it did write (all of them, on the measured-tagged path)
/// are complete files, and the caller's policy decides what happens next.
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
            "{} is not an atpkg/aterm binary, so a copy of it would not serve {HIDDEN_VERB}",
            helper_exe.display()
        ));
    }
    let job = crate::stage_helper::Job::prepare(scratch, "lay-helper")?;
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
/// when it is tracked; and, when that lane fails, under [`tracked_policy`]: refuse by
/// default, write in-process under `ATPKG_ALLOW_TRACKED_INSTALL=1`. Empty `files` is a
/// no-op that measures nothing.
pub fn lay_executables(files: &[Executable]) -> io::Result<()> {
    if files.is_empty() {
        return Ok(());
    }
    let tracked =
        cfg!(target_os = "macos") && crate::provenance::process_is_tracked(&std::env::temp_dir());
    lay_executables_with(files, tracked, &lane_for_this_binary(), tracked_policy())
}

/// [`lay_executables`] with its three inputs explicit — the tracking measurement, the
/// lane and the policy — so every branch is exercisable from a test that is not itself
/// in a position to be tracked.
pub fn lay_executables_with(
    files: &[Executable],
    tracked: bool,
    lane: &Lane,
    policy: TrackedPolicy,
) -> io::Result<()> {
    if files.is_empty() {
        return Ok(());
    }
    if !tracked {
        return write_all_in_process(files);
    }
    let helper = match lane {
        Lane::Helper(exe) => exe,
        // A fact about the binary, not a failure of the lane (module doc).
        Lane::Unavailable(_) => return write_all_in_process(files),
    };
    let what = format!("lay {} executable(s)", files.len());
    match lay_untracked(helper, files, &std::env::temp_dir()) {
        Ok(()) => Ok(()),
        Err(why) => match policy {
            TrackedPolicy::Refuse => Err(io::Error::other(tracked_refusal(&what, &why))),
            TrackedPolicy::Allow => {
                allowed_note(&what, &why);
                write_all_in_process(files)
            }
        },
    }
}

fn write_all_in_process(files: &[Executable]) -> io::Result<()> {
    for f in files {
        write_in_process(f)?;
    }
    Ok(())
}

/// [`lay_executables`] for one file — the shape the per-file writers keep.
pub fn lay_executable(path: &Path, body: &[u8]) -> io::Result<()> {
    lay_executables(&[Executable::new(path, body)])
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

    /// The policy knob: unset and empty refuse, anything else allows.
    #[test]
    fn the_policy_is_refuse_unless_the_escape_hatch_is_set() {
        assert_eq!(tracked_policy_of(None), TrackedPolicy::Refuse);
        assert_eq!(
            tracked_policy_of(Some(OsStr::new(""))),
            TrackedPolicy::Refuse
        );
        assert_eq!(
            tracked_policy_of(Some(OsStr::new("1"))),
            TrackedPolicy::Allow
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
        let code = run_helper(&[spec.display().to_string()]);
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
        let code = run_helper(&[spec.display().to_string()]);
        assert_eq!(code, ExitCode::from(1));
        let result = std::fs::read_to_string(job.join("result")).unwrap();
        assert!(result.starts_with("err\n"), "{result}");
        assert!(!job.join("result.tmp").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The decision table, with the lane's outcome forced: an untracked process writes
    /// in-process whatever the lane; a tracked one with no lane for its binary writes
    /// in-process; a tracked one whose lane FAILS refuses by default — nothing laid, the
    /// refusal naming the cause and the escape hatch — and writes under `Allow`.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_tracked_process_whose_lane_fails_refuses_by_default_and_writes_under_allow() {
        let d = tmp("policy");
        let dest = d.join("dest");
        std::fs::create_dir_all(&dest).unwrap();
        let wanted = files(&dest);
        // A helper that is spelled right but cannot answer: `/usr/bin/true` under the
        // name `atpkg` exits 0 without a result — a real lane failure, detected inside
        // the exit grace.
        let fake_dir = d.join("fake");
        std::fs::create_dir_all(&fake_dir).unwrap();
        let fake = fake_dir.join("atpkg");
        std::fs::copy("/usr/bin/true", &fake).unwrap();
        let broken = Lane::Helper(fake);
        let none = Lane::Unavailable(String::from("a test harness"));

        // Untracked: in-process, no lane consulted.
        lay_executables_with(&wanted, false, &broken, TrackedPolicy::Refuse).unwrap();
        for f in &wanted {
            std::fs::remove_file(&f.path).unwrap();
        }
        // Tracked, no lane for this binary: in-process.
        lay_executables_with(&wanted, true, &none, TrackedPolicy::Refuse).unwrap();
        for f in &wanted {
            std::fs::remove_file(&f.path).unwrap();
        }
        // Tracked, lane fails, default policy: REFUSED, nothing laid.
        let err = lay_executables_with(&wanted, true, &broken, TrackedPolicy::Refuse)
            .expect_err("a tracked process with a broken lane must refuse");
        let msg = err.to_string();
        assert!(msg.contains("provenance-tracked"), "{msg}");
        assert!(msg.contains("exited without a result"), "the cause: {msg}");
        assert!(msg.contains("lay 3 executable(s)"), "{msg}");
        assert!(msg.contains(ALLOW_TRACKED_ENV), "the escape hatch: {msg}");
        assert!(msg.contains("launchctl submit"), "the other way out: {msg}");
        assert!(msg.contains("proof_snapshot.py"), "what it breaks: {msg}");
        for f in &wanted {
            assert!(
                !f.path.exists(),
                "refused means not laid: {}",
                f.path.display()
            );
        }
        // Tracked, lane fails, escape hatch: written in-process.
        lay_executables_with(&wanted, true, &broken, TrackedPolicy::Allow).unwrap();
        for f in &wanted {
            assert_eq!(std::fs::read(&f.path).unwrap(), f.body);
        }
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

    /// The refusal text names every piece an operator needs.
    #[test]
    fn the_refusal_names_the_cause_the_breakage_and_both_ways_out() {
        let msg = tracked_refusal("stage the bundle", "launchctl submit failed (exit 1)");
        assert!(
            msg.starts_with("this process is provenance-tracked"),
            "{msg}"
        );
        assert!(msg.contains("could not stage the bundle (launchctl submit failed (exit 1))"));
        assert!(msg.contains("xattr -d"), "{msg}");
        assert!(msg.contains("Terminal.app"), "{msg}");
        assert!(msg.contains("<build>.tracked-install"), "{msg}");
    }
}
