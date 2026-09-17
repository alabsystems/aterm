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
//! binary — run in place when it is untagged, from a clean byte copy when it is tagged
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
//! # The policy when the lane cannot run
//!
//! A tracked process whose lane fails — no launchd, a job that never starts, a helper
//! that does not answer, a file that came back tagged anyway — writes the files itself,
//! says so once on stderr, and (for a staged bundle) RECORDS the choice beside the
//! bundle (`<build>.tracked-install`) for `aterm pkg doctor` and `aterm pkg repair` to
//! name ([`TrackedPolicy::Allow`], the default since 2026-09-14); tagged shims need no
//! record, the doctor's `bin/` scan sees them directly. That is not the v0.83.0 shape:
//! v0.83.0 was a tagged toolchain NOBODY HAD MEASURED, cut into a release. Now the tag is
//! measured at every seam that matters — the record here, the doctor's scan, and the
//! release cutter's pre-claim gate (`gates::provenance_gate`), which refuses a tagged
//! `trustc`/`targo` BEFORE a build number is burned. Between 2026-09-12 and 2026-09-14
//! the default was to REFUSE the install instead, and on the owner's own machine that
//! refused every stage — claude and codex never installed, `trust` and `clean` "aborted:
//! stage", "⚠ ALab toolchain install failed" on screen — because the lane's one refusal
//! (a tagged bundle executable) fired on every self-updated app. The lane now handles
//! that shape ([`crate::stage_helper::HelperPlan::CopyBundleThenExec`]), and a lane that
//! still cannot run must not leave a user with no toolchain to protect a release cut
//! that guards itself. `[packages] tracked_install = "refuse"` in aterm.toml
//! ([`crate::config::PackagesConfig::tracked_install`]) restores the refusal for a MACHINE
//! whose operator would rather have no toolchain than a tagged one, and
//! `ATPKG_REFUSE_TRACKED_INSTALL=1` ([`REFUSE_TRACKED_ENV`]) for one run: the env var, set
//! non-empty, wins over the key ([`tracked_policy_of`]). The key exists because the env
//! var alone could not reach the passes that matter (2026-09-15): the window's own
//! update/seed passes are spawned from launchd's environment, where no shell export
//! arrives, so a release cutter's machine could refuse a pass typed by hand and never the
//! unattended ones that lay most of its toolchain. `ATPKG_ALLOW_TRACKED_INSTALL`
//! ([`ALLOW_TRACKED_ENV`]), the old escape hatch, names the default and is accepted so
//! nothing that set it breaks; it relaxes nothing, not even for one run — a machine that
//! spelled `refuse` records again by editing the key.
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

/// The pre-2026-09-14 escape hatch, now the name of the DEFAULT: a provenance-tracked
/// installer whose untracked lane cannot run writes tagged files itself without being
/// asked — the staged bundle recorded beside it as `<build>.tracked-install`, the shims
/// visible to `aterm pkg doctor`'s `bin/` scan. Accepted (and a no-op) so a script that
/// set it keeps working; the knobs that change the behaviour are [`REFUSE_TRACKED_ENV`]
/// and `[packages] tracked_install`. It does NOT relax a machine whose config spells
/// `"refuse"` — not even for one run (2026-09-15): the policy has two inputs
/// ([`tracked_policy_of`]) and this is neither of them.
pub const ALLOW_TRACKED_ENV: &str = "ATPKG_ALLOW_TRACKED_INSTALL";

/// Set (non-empty) to REFUSE instead: a provenance-tracked installer whose untracked
/// lane cannot run fails the install, naming why the lane failed — the 2026-09-12 to
/// 2026-09-14 default, kept for an operator who would rather have no toolchain than one
/// that cannot cut a release (the cutter's own machine). Off by default: `aterm pkg
/// doctor` and the cutter's pre-claim gate name a tagged toolchain either way, and a
/// refused install left the owner's machine with no `claude`, no `codex` and a stale
/// `trust` (2026-09-14). Wins over [`ALLOW_TRACKED_ENV`] when both are set — fail-closed.
///
/// This is the ONE-RUN spelling: an operator typing it in a shell means it for that run,
/// so it wins over `[packages] tracked_install` in aterm.toml
/// ([`crate::config::PackagesConfig::tracked_install`]), which is the spelling for a
/// MACHINE — the one that reaches the window's own passes, spawned from launchd's
/// environment where no shell export arrives (2026-09-15). Empty is unset: `""` defers
/// to the key. The precedence table is [`tracked_policy_of`]'s and is unit-tested there.
pub const REFUSE_TRACKED_ENV: &str = "ATPKG_REFUSE_TRACKED_INSTALL";

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
    /// Refuse, naming why the lane failed and the way out. `ATPKG_REFUSE_TRACKED_INSTALL=1`
    /// for one run; `[packages] tracked_install = "refuse"` for a machine.
    Refuse,
    /// Write the files in-process (tagged), say so once, and — for a staged bundle —
    /// record it beside the build. The default; `[packages] tracked_install = "record"`
    /// names it.
    Allow,
}

/// The policy this process runs under — the two inputs of [`tracked_policy_of`], read:
/// [`REFUSE_TRACKED_ENV`] from the environment and `[packages] tracked_install` from the
/// once-loaded config ([`crate::config::cached`]). [`ALLOW_TRACKED_ENV`] in any state
/// changes nothing.
#[must_use]
pub fn tracked_policy() -> TrackedPolicy {
    tracked_policy_of(
        std::env::var_os(REFUSE_TRACKED_ENV).as_deref(),
        crate::config::cached().tracked_install(),
    )
}

/// The policy the two inputs select — pure, so the precedence table is a unit test:
/// `env`, the value of [`REFUSE_TRACKED_ENV`], WINS when it is set non-empty (an operator
/// typing it in a shell means it for that run); an unset or EMPTY env var is not set,
/// and `config` — what `[packages] tracked_install` spelled, `None` when it spelled
/// nothing admissible — decides; neither ⇒ `Allow`, the default since 2026-09-14.
/// The env var can only ever say `Refuse`, so a config `Refuse` has no one-run override:
/// that is deliberate (fail-closed on the machine that asked for it), and
/// [`ALLOW_TRACKED_ENV`] is not a third input.
#[must_use]
pub fn tracked_policy_of(env: Option<&OsStr>, config: Option<TrackedPolicy>) -> TrackedPolicy {
    if env.is_some_and(|v| !v.is_empty()) {
        return TrackedPolicy::Refuse;
    }
    config.unwrap_or(TrackedPolicy::Allow)
}

/// The refusal both lanes print under [`TrackedPolicy::Refuse`]: what could not be done
/// (`what`, e.g. "stage the bundle" or "lay 12 executable(s)"), why the lane failed,
/// what the tag breaks, and the way out. It no longer recommends "run this from an
/// untracked process — Terminal.app, or `launchctl submit … <path to atpkg>`": the tag
/// follows the EXECUTABLE, so on the one machine shape that reaches here in practice — a
/// self-updated app, whose bundle carries the tag — a Terminal.app shell or a launchd job
/// running that atpkg is tracked all the same (measured 2026-09-14), and the advice was
/// a loop. It names BOTH spellings of the refusal (2026-09-15) rather than which one
/// fired: the policy arrives here already decided, and an operator who set neither has
/// nothing to unset, so the pair is the whole search space.
#[must_use]
pub fn tracked_refusal(what: &str, why: &str) -> String {
    format!(
        "this process is provenance-tracked (a probe file it wrote came back carrying \
         com.apple.provenance) and the untracked launchd lane could not {what} ({why}) — \
         refusing rather than write files that would all carry the tag, because the \
         refuse policy is in force ({REFUSE_TRACKED_ENV} set in this environment, or \
         `[packages] tracked_install = \"refuse\"` in aterm.toml): {}. fix: unset \
         {REFUSE_TRACKED_ENV} and set tracked_install = \"record\" (or drop the key) and \
         the files are written in-process and recorded beside the build as \
         <build>.tracked-install (`aterm pkg doctor` names it and every tagged shim), or \
         clear what stopped launchd from running the helper — named above — and retry",
        crate::provenance::WHAT_IT_BREAKS
    )
}

/// The one stderr line printed when [`TrackedPolicy::Allow`] takes the in-process path.
fn allowed_note(what: &str, why: &str) {
    eprintln!(
        "atpkg: note — this process is provenance-tracked and the untracked lane could not \
         {what} ({why}); the files are written in-process and WILL carry \
         com.apple.provenance — `aterm pkg doctor` names what that breaks; \
         {REFUSE_TRACKED_ENV}=1 refuses instead for one run, `[packages] tracked_install \
         = \"refuse\"` in aterm.toml for this machine"
    );
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
/// when it is tracked; and, when that lane fails, under [`tracked_policy`]: write
/// in-process and say so by default, refuse under `ATPKG_REFUSE_TRACKED_INSTALL=1` or
/// `[packages] tracked_install = "refuse"`. Empty `files` is a no-op that measures
/// nothing.
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
        // A fact about the binary, not a failure of the lane (module doc) — but a
        // tracked process under the REFUSE policy is told, not quietly served: an
        // operator who asked for no tagged files gets none from a binary that has no
        // way to lay clean ones either (audit 2026-09-14).
        Lane::Unavailable(why) => {
            return match policy {
                TrackedPolicy::Refuse => Err(io::Error::other(tracked_refusal(
                    &format!("lay {} executable(s)", files.len()),
                    &format!("this binary has no untracked lane: {why}"),
                ))),
                TrackedPolicy::Allow => write_all_in_process(files),
            };
        }
    };
    let what = format!("lay {} executable(s)", files.len());
    // The lanes' scratch is `<prefix>/staging/.lanes/` (2026-09-15), never the shared
    // per-user `$TMPDIR`: the dead-job sweep `Job::prepare` runs there walks only
    // directories atpkg created, and takes only the stems this crate submits
    // (`stage_helper::JOB_STEMS`) even so. No store (no `HOME`): the temp dir, as before.
    let scratch = crate::stage_helper::lanes_scratch().unwrap_or_else(std::env::temp_dir);
    match lay_untracked(helper, files, &scratch) {
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

    /// The precedence table (2026-09-15), every row: the env var set non-empty WINS
    /// (an operator typing it means it for that run), an unset or EMPTY env var defers
    /// to `[packages] tracked_install`, and neither is ALLOW — the default since
    /// 2026-09-14, when a refused install left the owner's machine without a toolchain.
    #[test]
    fn the_policy_precedence_is_env_then_config_then_allow() {
        use TrackedPolicy::{Allow, Refuse};
        let unset: Option<&OsStr> = None;
        let empty = Some(OsStr::new(""));
        let set = Some(OsStr::new("1"));
        // (unset, none) → Allow: the default.
        assert_eq!(tracked_policy_of(unset, None), Allow);
        // (unset, record) → Allow: the key naming the default.
        assert_eq!(tracked_policy_of(unset, Some(Allow)), Allow);
        // (unset, refuse) → Refuse: the machine's durable spelling, reached from
        // launchd's environment where no shell export is.
        assert_eq!(tracked_policy_of(unset, Some(Refuse)), Refuse);
        // ("1", record) → Refuse: the shell wins for this run.
        assert_eq!(tracked_policy_of(set, Some(Allow)), Refuse);
        // ("", refuse) → Refuse: an empty env var is unset, so the key decides.
        assert_eq!(tracked_policy_of(empty, Some(Refuse)), Refuse);
        // (set, none) → Refuse: the pre-2026-09-15 behaviour, unchanged.
        assert_eq!(tracked_policy_of(set, None), Refuse);
        // The two rows the table above implies and a regression would silently flip.
        assert_eq!(tracked_policy_of(empty, None), Allow, "empty env is unset");
        assert_eq!(tracked_policy_of(set, Some(Refuse)), Refuse);
        // Any non-empty value is "set" — the env var is a presence switch, not a bool.
        assert_eq!(tracked_policy_of(Some(OsStr::new("0")), None), Refuse);
        assert_eq!(REFUSE_TRACKED_ENV, "ATPKG_REFUSE_TRACKED_INSTALL");
        assert_eq!(ALLOW_TRACKED_ENV, "ATPKG_ALLOW_TRACKED_INSTALL");
    }

    /// The config side feeds the table through `PackagesConfig::tracked_install`, so
    /// the two modules agree on which word is which policy — pinned here, where the
    /// policy lives, so a rename on either side fails a test in both.
    #[test]
    fn the_config_key_spells_the_two_policies() {
        let of = |toml: &str| {
            tracked_policy_of(None, crate::config::parse_packages(toml).tracked_install())
        };
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

    /// The decision table, with the lane's outcome forced: an untracked process writes
    /// in-process whatever the lane; a tracked one with no lane for its binary writes
    /// in-process; a tracked one whose lane FAILS refuses under `Refuse` — nothing laid,
    /// the refusal naming the cause and the way out — and writes under `Allow`, the
    /// default.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_tracked_process_whose_lane_fails_refuses_under_refuse_and_writes_under_allow() {
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
        // Tracked, no lane for this binary: in-process under the default, REFUSED under
        // `Refuse` — an operator who asked for no tagged files gets none from a binary
        // that cannot lay clean ones (2026-09-15).
        lay_executables_with(&wanted, true, &none, TrackedPolicy::Allow).unwrap();
        for f in &wanted {
            std::fs::remove_file(&f.path).unwrap();
        }
        let err = lay_executables_with(&wanted, true, &none, TrackedPolicy::Refuse)
            .expect_err("tracked, no lane, refuse policy: refused");
        assert!(err.to_string().contains("no untracked lane"), "{err}");
        for f in &wanted {
            assert!(!f.path.exists(), "refused means not laid");
        }
        // Tracked, lane fails, `Refuse` opted into: REFUSED, nothing laid.
        let err = lay_executables_with(&wanted, true, &broken, TrackedPolicy::Refuse)
            .expect_err("a tracked process with a broken lane must refuse under Refuse");
        let msg = err.to_string();
        assert!(msg.contains("provenance-tracked"), "{msg}");
        assert!(msg.contains("exited without a result"), "the cause: {msg}");
        assert!(msg.contains("lay 3 executable(s)"), "{msg}");
        assert!(
            msg.contains(REFUSE_TRACKED_ENV),
            "the knob that refused: {msg}"
        );
        assert!(msg.contains("proof_snapshot.py"), "what it breaks: {msg}");
        assert!(
            !msg.contains("Terminal.app") && !msg.contains("launchctl submit"),
            "the looping remedy is gone: {msg}"
        );
        for f in &wanted {
            assert!(
                !f.path.exists(),
                "refused means not laid: {}",
                f.path.display()
            );
        }
        // Tracked, lane fails, the default: written in-process.
        assert_eq!(tracked_policy_of(None, None), TrackedPolicy::Allow);
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

    /// The refusal text names every piece an operator needs — and not the remedy that
    /// looped on a self-updated app (a Terminal.app shell or a launchd job running a
    /// tagged bundle binary is tracked all the same).
    #[test]
    fn the_refusal_names_the_cause_the_breakage_and_the_way_out() {
        let msg = tracked_refusal("stage the bundle", "launchctl submit failed (exit 1)");
        assert!(
            msg.starts_with("this process is provenance-tracked"),
            "{msg}"
        );
        assert!(msg.contains("could not stage the bundle (launchctl submit failed (exit 1))"));
        assert!(msg.contains("xattr -d"), "{msg}");
        assert!(msg.contains(REFUSE_TRACKED_ENV), "{msg}");
        assert!(
            msg.contains("tracked_install = \"refuse\"")
                && msg.contains("tracked_install = \"record\""),
            "both spellings of the refusal, and the way back: {msg}"
        );
        assert!(msg.contains("<build>.tracked-install"), "{msg}");
        assert!(!msg.contains("Terminal.app"), "{msg}");
    }
}
