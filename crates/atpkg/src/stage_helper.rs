// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The UNTRACKED staging lane: extraction handed to a launchd job when the installer
//! measures itself as provenance-tracked ([`crate::provenance`]).
//!
//! # The problem it solves
//!
//! A provenance-tracked atpkg — one started from a tagged binary, or descended from a
//! tracked process (a shell inside a tracked aterm.app, an agent whose `claude` carries
//! the tag) — tags every file it extracts, and a Trust bundle whose `trustc` carries
//! `com.apple.provenance` cannot cut a release: the tag follows the executable into every
//! object file and the proof snapshot, which the cutter refuses after the claim (v0.83.0,
//! 2026-09-12, bundle `trust/8590`). Nothing a tracked process spawns escapes — `fork`,
//! `posix_spawn`, `setsid`, `osascript`, `exec` into a clean image were all measured
//! tagged. The one thing that does escape is a job LAUNCHD spawns from an UNTAGGED
//! executable: launchd is the job's parent, not us.
//!
//! # The mechanism (measured: `scratchpad/provenance/probe2.sh` m6/m8/m17/m18 on
//! 2026-09-12, re-measured on 2026-09-13 — the commit that introduced [`plan_helper`])
//!
//! 1. The tracked parent writes a small SPEC (archive, destination, the fields
//!    [`crate::install::stage_payload_spec`] reads) into the per-program staging scratch.
//! 2. It MEASURES this very binary (`xattr`, [`crate::provenance::xattr_names`]) and plans
//!    the job from that ([`plan_helper`]):
//!    * an UNTAGGED binary — the installed app's own executable, a build made from an
//!      untracked shell — is run IN PLACE, by its real path: launchd is the job's parent
//!      and the tag follows the executable, so the job is untracked and writes clean
//!      files (a launchd job exec'ing a clean script writes clean files; one exec'ing a
//!      tagged script writes tagged files — both measured 2026-09-13);
//!    * a TAGGED binary that stands alone — a `target/debug/aterm` built from a tracked
//!      shell — is first BYTE-COPIED by the job itself (`cat`, by the untracked job: the
//!      copy is clean and runs clean, measured; `cp`/`ditto` would carry the tag across)
//!      and the copy is run;
//!    * a TAGGED binary that is a bundle's own executable (`….app/Contents/MacOS/…`)
//!      is run from a byte copy of its WHOLE bundle, made by the job (directories,
//!      symlinks re-linked, regular files `cat`'d, executables re-marked): run in place
//!      it would be tracked, and a LONE copy of the executable will not run — its code
//!      signature covers the bundle's `Info.plist`, so the kernel kills that copy at
//!      exec (measured 2026-09-13 on the Developer ID `aterm.app`: `codesign -v` on the
//!      lone copy says "invalid Info.plist (plist or signature have been modified)", the
//!      exec dies with SIGKILL, and the old lane — which copied EVERY binary — reported
//!      that as "exited without a result" with an empty stderr). A faithful copy of the
//!      bundle keeps its seal — `codesign --verify --deep --strict` passes, the copy
//!      runs, and what it lays is clean (measured 2026-09-14 on the shipped, notarized
//!      v0.85.0 bundle, tagged by its own in-place self-update). Between 2026-09-13 and
//!      this arm the shape was REFUSED instead, and since a self-updated or
//!      browser-downloaded app is tagged, that refusal was every user's every install.
//! 3. It submits a one-shot launchd job — `/bin/sh -c '<wrapper>'` — that runs the helper
//!    on the hidden verb with the spec file as its one argument, RECORDS the helper's exit
//!    status in `<job>/status` (a signal as `128 + n`, `/bin/sh`'s convention), removes its
//!    own label, and exits 0. Always 0: `launchctl submit` keeps a job alive on failure and
//!    re-spawns it, which is how a failed helper became a label launchd re-ran for a day.
//!    The copy, when there is one, is named `atpkg` so the `aterm` multi-tool's argv0
//!    alias routes it to this CLI.
//! 4. The helper runs the ordinary extractor — the same vetting, caps, mode sanitizing and
//!    fused `tree_root` fold as the in-process lane, because it IS the in-process lane in
//!    another process — into the `incoming` scratch the parent created, and writes the
//!    folded root to a result file (temp + rename).
//! 5. The parent waits for the result, and carries on exactly as before: the tree_root
//!    re-verify, the swap (a tracked parent renaming a clean DIRECTORY tags the directory
//!    and leaves the files inside clean — measured), the marker. A helper that did NOT
//!    answer is reported by its real fate — the status the wrapper recorded (exit code,
//!    or the signal that killed it: SIGKILL means the binary could not run) and its
//!    stderr — never as a bare "exited without a result" when that fate is known.
//!
//! # What it is not
//!
//! Not a trust boundary: the helper is our own bytes (in place, or a copy in our own
//! `0700` staging dir under the store lock), and the root it reports is re-verified
//! against the SIGNED root by the parent as always (`ATPKG_STAGE_DISK_REVERIFY` re-arms
//! the on-disk walk). Not silent once it is needed: a tracked installer whose lane
//! cannot run — no launchd, a job that never starts, a helper that does not answer —
//! stages in-process, says so, and RECORDS it beside the build
//! (`<build>.tracked-install`), which `aterm pkg doctor` reports as the cause and
//! `aterm pkg repair` names as needing a re-seed; `ATPKG_REFUSE_TRACKED_INSTALL=1`
//! REFUSES the install instead ([`crate::install::StageError::TrackedInstaller`]) for an
//! operator who would rather have no toolchain than a tagged one
//! ([`crate::install::stage_for_store_with`] has the decision, [`crate::lay`] the policy
//! and why the default flipped on 2026-09-14). And it is only half of the story:
//! `bin/<tool>` is a `#!/bin/sh`
//! script, and a tagged shim tracks the tool it execs (law m21) — so the shims, the
//! `agents/` twins, the pending and reroute stubs and the tombstones go through the same
//! job under a second hidden verb ([`crate::lay`]), sharing the [`Job`] below.
//!
//! # Leaks, and why there are none now
//!
//! A parent killed between `submit` and its `Drop` (a test under a timeout, a `kill -9`)
//! used to leave the label registered: two such labels from one dead test pid sat in
//! launchd for a day, re-spawned and re-`cat`ing `/usr/bin/true` (2026-09-12/13). Now
//! the JOB removes its own label as its last act, so a dead parent leaves nothing behind
//! once the job has run; and [`Job::prepare`] sweeps any `systems.alab.atpkg.*` label
//! whose owning pid is gone (a job wedged when its parent died), so the next install
//! tidies after the last one.
//!
//! macOS only; on every other platform [`stage_untracked`] is unavailable by construction.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use crate::install::StageSpec;

/// The hidden verb the launchd job execs — machinery, not vocabulary: unlisted in help
/// and `VERBS`, dispatched before the store lock (its parent HOLDS that lock).
pub const HIDDEN_VERB: &str = "__stage-payload";

/// First line of a spec file — a version stamp, so a stale copy of this binary never
/// misreads a newer spec.
const SPEC_HEADER: &str = "atpkg-stage-spec v1";

/// How long the parent tolerates "no result, no status AND no running job" before calling
/// the lane dead — covers launchd's start latency (the wrapper's `status` file, written
/// the moment the helper exits, is what normally ends the wait on a failure).
const EXIT_GRACE: Duration = Duration::from_secs(3);

/// The absolute ceiling on one staged extraction (the shipped `trust` member is 3.4 GB;
/// a slow disk is minutes, never hours). Past this the lane is declared wedged — a lane
/// failure, which the caller's policy answers (an in-process stage, recorded, by default).
const CEILING: Duration = Duration::from_secs(6 * 60 * 60);

// ---------------------------------------------------------------------------------------
// The spec: what the helper needs, byte-safe, dependency-free.
// ---------------------------------------------------------------------------------------

/// Render the spec the helper reads. Values are lowercase hex of their raw bytes (paths
/// as OS bytes), so a path with a space, a quote or a non-UTF-8 byte round-trips.
#[must_use]
pub fn encode_spec(spec: &StageSpec, archive: &Path, dest: &Path) -> String {
    let mut out = String::from(SPEC_HEADER);
    out.push('\n');
    push_kv(
        &mut out,
        "archive",
        crate::call1(crate::platform::os_str_bytes, archive.as_os_str()),
    );
    push_kv(
        &mut out,
        "dest",
        crate::call1(crate::platform::os_str_bytes, dest.as_os_str()),
    );
    push_kv(&mut out, "payload", spec.payload.as_bytes());
    push_kv(&mut out, "entry", spec.entry.as_bytes());
    out.push_str("strip_components=");
    out.push_str(&crate::dec_u64(u64::from(spec.strip_components)));
    out.push('\n');
    out.push_str("size_cap=");
    out.push_str(&crate::dec_u64(spec.size_cap));
    out.push('\n');
    for (name, target) in &spec.links {
        out.push_str("link=");
        out.push_str(&crate::tree::hex(name.as_bytes()));
        out.push('=');
        out.push_str(&crate::tree::hex(target.as_bytes()));
        out.push('\n');
    }
    out
}

fn push_kv(out: &mut String, key: &str, raw: &[u8]) {
    out.push_str(key);
    out.push('=');
    out.push_str(&crate::tree::hex(raw));
    out.push('\n');
}

/// Parse a spec rendered by [`encode_spec`]. Every line is required to be well-formed; a
/// missing `archive`/`dest` or a bad hex digit refuses the whole spec — the helper must
/// never extract into a path it half-understood.
pub fn decode_spec(text: &str) -> Result<(StageSpec, PathBuf, PathBuf), String> {
    let mut lines = text.lines();
    if lines.next() != Some(SPEC_HEADER) {
        return Err(String::from("spec header missing or of another version"));
    }
    let mut archive: Option<PathBuf> = None;
    let mut dest: Option<PathBuf> = None;
    let mut spec = StageSpec {
        payload: String::new(),
        entry: String::new(),
        strip_components: 0,
        links: BTreeMap::new(),
        size_cap: 0,
    };
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!("malformed spec line: {line:?}"));
        };
        match key {
            "archive" => archive = Some(path_of(unhex(value)?)),
            "dest" => dest = Some(path_of(unhex(value)?)),
            "payload" => spec.payload = utf8(unhex(value)?, "payload")?,
            "entry" => spec.entry = utf8(unhex(value)?, "entry")?,
            "strip_components" => {
                spec.strip_components = value
                    .parse()
                    .map_err(|_| format!("strip_components is not a number: {value:?}"))?;
            }
            "size_cap" => {
                spec.size_cap = value
                    .parse()
                    .map_err(|_| format!("size_cap is not a number: {value:?}"))?;
            }
            "link" => {
                let Some((name, target)) = value.split_once('=') else {
                    return Err(format!("malformed link line: {line:?}"));
                };
                spec.links.insert(
                    utf8(unhex(name)?, "link name")?,
                    utf8(unhex(target)?, "link target")?,
                );
            }
            other => return Err(format!("unknown spec key: {other:?}")),
        }
    }
    let archive = archive.ok_or_else(|| String::from("spec names no archive"))?;
    let dest = dest.ok_or_else(|| String::from("spec names no dest"))?;
    if !archive.is_absolute() || !dest.is_absolute() {
        return Err(String::from("spec paths must be absolute"));
    }
    Ok((spec, archive, dest))
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

fn utf8(raw: Vec<u8>, what: &str) -> Result<String, String> {
    String::from_utf8(raw).map_err(|_| format!("{what} is not UTF-8"))
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
// The helper side: what the launchd job's clean copy runs.
// ---------------------------------------------------------------------------------------

/// The hidden verb's body: `__stage-payload <spec-file>`. Reads the spec, stages through
/// the ONE producer ([`crate::install::stage_payload_spec`]), and answers in
/// `<spec-dir>/result` — `ok\n<tree_root>\n` or `err\n<message>\n`, written temp+rename so
/// the parent never reads a half-written answer. Touches nothing else: no config, no store
/// lock, no status.
pub fn run_helper(args: &[String]) -> ExitCode {
    let Some(spec_path) = args.first().map(PathBuf::from) else {
        eprintln!("atpkg {HIDDEN_VERB}: usage: {HIDDEN_VERB} <spec-file>");
        return ExitCode::from(2);
    };
    let result_path = spec_path.with_file_name("result");
    let outcome = (|| -> Result<String, String> {
        let text = std::fs::read_to_string(&spec_path)
            .map_err(|e| format!("read spec {}: {e}", spec_path.display()))?;
        let (spec, archive, dest) = decode_spec(&text)?;
        crate::install::stage_payload_spec(&spec, &archive, &dest).map_err(|e| e.to_string())
    })();
    let body = match &outcome {
        Ok(root) => format!("ok\n{root}\n"),
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
// The parent side: submit, wait, read back.
// ---------------------------------------------------------------------------------------

/// Whether `exe` is a binary that serves [`HIDDEN_VERB`] under the argv0 name `atpkg`:
/// the standalone `atpkg` (dev builds) or the `aterm` multi-tool, whose argv0 alias
/// routes `atpkg` to this CLI. Anything else — a test harness, a foreign binary — would
/// be run, and answer nothing, costing a round trip for no result. This is a guard on
/// OUR OWN spelling, not a measurement of the subject; the result file is the only proof
/// the lane accepts.
#[must_use]
pub fn exe_serves_hidden_verb(exe: &Path) -> bool {
    matches!(
        exe.file_name().and_then(|n| n.to_str()),
        Some("atpkg" | "aterm")
    )
}

/// What the untracked lane laid: the folded `tree_root`, and whether the first regular
/// file it laid nevertheless came back carrying the tag — MEASURED, not assumed. A tagged
/// witness means the lane ran but did not achieve its purpose (never observed: launchd is
/// the job's parent and the helper it runs is untagged — in place or as a clean copy —
/// so every measurement came back clean);
/// the tree is still the verified tree, and the caller decides under its policy
/// ([`crate::install::stage_for_store_with`]) whether to keep it and record the fact or
/// to refuse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedUntracked {
    /// The folded `tree_root` of what the job laid.
    pub root: String,
    /// Whether the first regular file under `dest` carries `com.apple.provenance`.
    pub witness_tagged: bool,
}

/// Stage `archive` at `dest` through an untracked launchd job running `helper_exe` — in
/// place when it is untagged, as a clean byte copy when it is tagged and free-standing
/// ([`plan_helper`]; see the module docs) — returning the folded `tree_root` and the
/// measured outcome ([`StagedUntracked`]).
///
/// `scratch` holds the job's spec, logs and any copy — the per-program staging dir, a
/// `0700` scratch the store lock already guards. `dest` must exist and be empty, as for
/// the in-process lane; on ANY error it is left as the caller made it (emptied), so the
/// caller can extract into it in-process if its policy allows.
///
/// `Err(reason)` is "the lane could not do it" — never a verdict on the archive: a helper
/// that ran and refused the payload reports that refusal, and a caller that is allowed
/// to re-run the in-process lane obtains the canonical [`crate::install::StageError`]
/// there.
#[cfg(target_os = "macos")]
pub fn stage_untracked(
    helper_exe: &Path,
    spec: &StageSpec,
    archive: &Path,
    dest: &Path,
    scratch: &Path,
) -> Result<StagedUntracked, String> {
    if !exe_serves_hidden_verb(helper_exe) {
        return Err(format!(
            "{} is not an atpkg/aterm binary, so it would not serve {HIDDEN_VERB}",
            helper_exe.display()
        ));
    }
    // `Job` removes its label and scratch on drop, so every early `?` below leaves
    // neither a registered launchd job nor a spec file behind.
    let mut job = Job::prepare(scratch, "stage-helper")?;
    let spec_text = encode_spec(spec, archive, dest);
    std::fs::write(&job.spec, spec_text).map_err(|e| format!("write spec: {e}"))?;
    job.submit(helper_exe, HIDDEN_VERB)?;
    let waited = job.wait_for_result();
    let root = match waited {
        Ok(()) => job.read_result()?,
        Err(why) => {
            clear_dir(dest);
            return Err(why);
        }
    };
    let root = match root {
        Ok(root) => root,
        Err(refusal) => {
            clear_dir(dest);
            return Err(format!(
                "the untracked helper refused the payload: {refusal}"
            ));
        }
    };
    // MEASURE THE OUTCOME, do not assume it: the first regular file the job laid down
    // is read back for the tag. The caller decides what a tagged witness means under its
    // policy; this lane only reports it.
    let witness_tagged = first_regular_file(dest)
        .is_some_and(|witness| crate::provenance::carries_provenance(&witness));
    Ok(StagedUntracked {
        root,
        witness_tagged,
    })
}

/// See the macOS body — there is no launchd and no tag anywhere else.
#[cfg(not(target_os = "macos"))]
pub fn stage_untracked(
    _helper_exe: &Path,
    _spec: &StageSpec,
    _archive: &Path,
    _dest: &Path,
    _scratch: &Path,
) -> Result<StagedUntracked, String> {
    Err(String::from(
        "the untracked staging lane exists on macOS only",
    ))
}

/// Empty `dir` in place (remove its contents, keep the directory) — the state the
/// in-process lane's `require_empty_destination` accepts.
fn clear_dir(dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            let is_dir = std::fs::symlink_metadata(&p).is_ok_and(|m| m.is_dir());
            let _ = if is_dir {
                std::fs::remove_dir_all(&p)
            } else {
                std::fs::remove_file(&p)
            };
        }
    }
}

/// The first regular file under `dir`, depth-first in name order.
pub(crate) fn first_regular_file(dir: &Path) -> Option<PathBuf> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .collect();
    entries.sort();
    for p in entries {
        let meta = std::fs::symlink_metadata(&p).ok()?;
        if meta.is_file() {
            return Some(p);
        }
        if meta.is_dir()
            && let Some(found) = first_regular_file(&p)
        {
            return Some(found);
        }
    }
    None
}

/// How the launchd job runs the helper — decided by [`plan_helper`] from a MEASUREMENT
/// of the binary (its tag) and its shape (a bundle's own executable or free-standing),
/// never from where it was built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelperPlan {
    /// The binary is untagged: the job runs it in place, by its real path. launchd is
    /// the parent and the tag follows the executable, so the job is untracked.
    ExecOriginal,
    /// The binary is tagged and free-standing: the job `cat`s it to a clean copy first
    /// (a byte copy made by an untracked process is clean — measured) and runs the copy.
    CopyThenExec,
    /// The binary is tagged and is a bundle's own executable (`<bundle>.app/Contents/
    /// MacOS/<exe>`): the job byte-copies the WHOLE bundle — every directory, every
    /// symlink re-linked, every regular file `cat`'d, executables re-marked `0755` —
    /// into its scratch and runs the copy's executable of the same name. A lone copy
    /// of the executable dies at exec (its Developer ID signature seals the bundle's
    /// `Info.plist`, which the lone copy lacks — SIGKILL, measured 2026-09-13); a
    /// faithful copy of the bundle keeps its seal: `codesign --verify --deep --strict`
    /// passes on it, it runs, and — launchd being its parent and the copy untagged —
    /// the files it lays are CLEAN while the same verb run from the tagged bundle in
    /// place lays tagged ones (measured 2026-09-14 on the shipped, notarized v0.85.0
    /// `aterm.app`, which carried the tag because the in-place self-updater — a tracked
    /// process — wrote it). Until this arm existed such a bundle was REFUSED, which on
    /// a machine whose app had ever self-updated or been downloaded by a browser was
    /// every install: "ALab toolchain install failed" on 2026-09-14.
    CopyBundleThenExec {
        /// The bundle root (`<bundle>.app`), the tree the job replicates.
        bundle: PathBuf,
    },
}

/// The bundle `exe` is the executable of — `<bundle>.app` for
/// `<bundle>.app/Contents/MacOS/<exe>` with a `Contents/Info.plist` beside it — or
/// `None` for a free-standing binary. A bundle-resident executable is never copied: its
/// code signature covers the bundle's `Info.plist`, so a copy outside the bundle fails
/// its own signature check and the kernel kills it at exec (measured 2026-09-13).
#[must_use]
pub fn bundle_of(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    if macos.file_name()? != std::ffi::OsStr::new("MacOS") {
        return None;
    }
    let contents = macos.parent()?;
    if contents.file_name()? != std::ffi::OsStr::new("Contents") {
        return None;
    }
    if !contents.join("Info.plist").is_file() {
        return None;
    }
    contents.parent().map(Path::to_path_buf)
}

/// Decide how the job runs `exe` from the two facts about it: `tagged` (measured —
/// [`crate::provenance::carries_provenance`]) and the bundle it belongs to, if any
/// ([`bundle_of`]). An untagged binary runs in place whatever its shape; a tagged
/// free-standing one is copied clean by the job; a tagged bundle executable is run from
/// a clean copy of its WHOLE bundle ([`HelperPlan::CopyBundleThenExec`]) — in place it
/// would be tracked, and a lone copy of the executable cannot run. Every shape has a
/// plan; `exe` is unused now that no shape is refused, and stays in the signature so
/// the table reads as the three facts it is decided from.
#[must_use]
pub fn plan_helper(_exe: &Path, tagged: bool, bundle: Option<&Path>) -> HelperPlan {
    match (tagged, bundle) {
        (false, _) => HelperPlan::ExecOriginal,
        (true, None) => HelperPlan::CopyThenExec,
        (true, Some(bundle)) => HelperPlan::CopyBundleThenExec {
            bundle: bundle.to_path_buf(),
        },
    }
}

/// [`plan_helper`] for a real binary: the tag AND the quarantine attribute measured with
/// `xattr` (on the binary, and for a bundle executable on the bundle), the bundle read off
/// the path. A binary that cannot be inspected is an error, not "untagged" — a plan built
/// on a failure to look would run a possibly tainted helper and call its files clean.
pub fn plan_for_exe(exe: &Path) -> Result<HelperPlan, String> {
    let names = crate::provenance::xattr_names(exe).map_err(|e| {
        format!(
            "cannot inspect {} for com.apple.provenance: {e}",
            exe.display()
        )
    })?;
    let tagged = names
        .iter()
        .any(|n| n == crate::provenance::PROVENANCE_XATTR);
    // QUARANTINE COUNTS THE SAME. A browser download carries `com.apple.quarantine` and
    // no provenance tag at all, so it reads as untagged — and yet a launchd job that
    // exec's it IN PLACE is tracked and writes tagged files (measured 2026-09-14 on m16
    // with the Safari-installed 0.84.0: the result file its hidden verb wrote came back
    // tagged). Planning that binary `ExecOriginal` is the one way this lane can still
    // hand out a tagged toolchain while believing it laid a clean one, so a quarantined
    // helper takes the copy lane exactly as a tagged one does.
    let quarantined = crate::provenance::quarantined_carrier(exe).is_some();
    Ok(plan_helper(
        exe,
        tagged || quarantined,
        bundle_of(exe).as_deref(),
    ))
}

/// The wrapper `/bin/sh -c` runs, positional: `$1` helper, `$2` spec, `$3` verb,
/// `$4` status file, `$5` this job's label, `$6` the copy path (empty to run in place),
/// `$7` the bundle root to replicate (empty for a free-standing helper).
///
/// With `$7` set, `$6` is a DIRECTORY and the wrapper replicates the bundle into it
/// before anything runs — the directories first, then every symlink re-linked to the
/// target `readlink` reports (relative stays relative: `atpkg -> aterm`), then every
/// regular file byte-copied by `cat` with the executable bit restored — and runs the
/// copy's `Contents/MacOS/<name of $1>`. `find … -exec sh -c '…' "$6" {} +` rather than a
/// `while read` loop so a name with a space or a newline cannot split; `$0` inside is
/// the copy root. The helper's own path is absolute, as are the spec and the status
/// file, so the `cd` into the bundle (in a subshell) changes nothing that follows. A
/// replica that fails at any step runs nothing and is recorded as
/// [`STATUS_COPY_FAILED`], exactly like a lone copy that could not be made.
///
/// It runs the helper, records the helper's exit status — `128 + n` for a signal, the
/// shell's convention — in `$4` (temp + rename), removes its own label and exits 0. The
/// unconditional 0 is deliberate: `launchctl submit` keeps a job alive on failure, and a
/// helper that could not run became a label launchd re-spawned every ten seconds; the
/// status file, not launchd's opinion of the wrapper, is the record of what happened.
/// The self-removal is what makes a parent killed before its `Drop` leak nothing.
#[cfg(target_os = "macos")]
const WRAPPER: &str = r#"exe="$1"
if [ -n "$7" ]; then
  if mkdir -p "$6" && ( cd "$7" && find . -type d -exec sh -c 'for d; do mkdir -p "$0/$d" || exit 1; done' "$6" {} + && find . -type l -exec sh -c 'for l; do ln -s "$(readlink "$l")" "$0/$l" || exit 1; done' "$6" {} + && find . -type f -exec sh -c 'for f; do cat "$f" > "$0/$f" || exit 1; if [ -x "$f" ]; then chmod 755 "$0/$f" || exit 1; fi; done' "$6" {} + ); then exe="$6/Contents/MacOS/$(basename "$1")"; else exe=""; fi
elif [ -n "$6" ]; then
  if cat "$1" > "$6" && chmod 755 "$6"; then exe="$6"; else exe=""; fi
fi
if [ -n "$exe" ]; then "$exe" "$3" "$2"; s=$?; else echo "atpkg-untracked: could not copy $1 to $6" >&2; s=125; fi
printf '%s\n' "$s" > "$4.tmp" && mv -f "$4.tmp" "$4"
/bin/launchctl remove "$5"
exit 0
"#;

/// The wrapper's status when the helper could not be copied (before any exec).
#[cfg(target_os = "macos")]
const STATUS_COPY_FAILED: i32 = 125;

/// One submitted job: its scratch dir, label and files. Shared by the two hidden verbs —
/// the staging lane here and the executable-laying lane ([`crate::lay`]) — so there is
/// ONE launchd choreography (plan, wrapper, result and status files, grace, ceiling,
/// self-removal, sweep, cleanup).
#[cfg(target_os = "macos")]
pub(crate) struct Job {
    dir: PathBuf,
    label: String,
    pub(crate) spec: PathBuf,
    result: PathBuf,
    status: PathBuf,
    copy: PathBuf,
    out_log: PathBuf,
    err_log: PathBuf,
    /// The helper the job was submitted with, for the fate report.
    helper: PathBuf,
}

/// The label prefix every job of this crate carries; the sweep looks for it.
#[cfg(target_os = "macos")]
const LABEL_PREFIX: &str = "systems.alab.atpkg.";

#[cfg(target_os = "macos")]
impl Job {
    /// Create the job's `0700` scratch under `scratch`, named `<stem>-<pid>-<seq>-<nonce>`
    /// (the label is `systems.alab.atpkg.` + that name). First sweeps the labels earlier
    /// parents left behind ([`Self::sweep_orphans`]).
    pub(crate) fn prepare(scratch: &Path, stem: &str) -> Result<Self, String> {
        Self::sweep_orphans();
        Self::sweep_dead_job_dirs(scratch);
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let stem = format!("{stem}-{}-{seq}-{nonce:x}", std::process::id());
        let dir = scratch.join(&stem);
        // `create_dir`, not `create_dir_all`: a directory already there under this
        // name would be adopted with whatever stale `result` it holds, and the parent
        // would read that as the job's answer. The name carries pid, sequence and a
        // nanosecond nonce, so this fails only when something is wrong.
        std::fs::create_dir(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        crate::platform::set_mode(&dir, 0o700)
            .map_err(|e| format!("chmod {}: {e}", dir.display()))?;
        Ok(Self {
            label: format!("{LABEL_PREFIX}{stem}"),
            spec: dir.join("spec"),
            result: dir.join("result"),
            status: dir.join("status"),
            // Named `atpkg` so the `aterm` multi-tool routes the copy here by argv0.
            copy: dir.join("atpkg"),
            out_log: dir.join("out.log"),
            err_log: dir.join("err.log"),
            helper: PathBuf::new(),
            dir,
        })
    }

    /// Remove every `systems.alab.atpkg.<stem>-<pid>-<seq>-<nonce>` label whose `<pid>`
    /// no longer exists: a parent that died before its `Drop` (a test killed under a
    /// timeout) left its job registered, and until 2026-09-13 launchd kept such labels —
    /// and re-spawned them — indefinitely. A label whose pid is alive is left alone
    /// (another install in flight, or a pid reused by something else: not ours to judge).
    /// Best effort: a `launchctl` that cannot list is simply no sweep.
    fn sweep_orphans() {
        let Ok(out) = std::process::Command::new("/bin/launchctl")
            .arg("list")
            .output()
        else {
            return;
        };
        let me = std::process::id();
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let Some(label) = line.split('\t').nth(2) else {
                continue;
            };
            let Some(pid) = owner_pid_of_label(label) else {
                continue;
            };
            if pid == me || pid_exists(pid) {
                continue;
            }
            let _ = std::process::Command::new("/bin/launchctl")
                .args(["remove", label])
                .output();
        }
    }

    /// Remove every job directory under `scratch` — `<stem>-<pid>-<seq>-<nonce>`, the
    /// same shape as the label — whose `<pid>` no longer exists: a parent killed
    /// between `prepare` and its `Drop` (a `kill -9`, a test under a timeout) left its
    /// scratch behind, and with a whole-bundle replica inside that is ~51 MB a leak; the
    /// store's `gc` sweeps regular files, never these directories (audit 2026-09-14).
    /// Only directories, only our naming, only a dead pid — an archive or a `.part`
    /// beside them never parses as a label, and a live sibling's job is left alone.
    fn sweep_dead_job_dirs(scratch: &Path) {
        let Ok(entries) = std::fs::read_dir(scratch) else {
            return;
        };
        let me = std::process::id();
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some(pid) = owner_pid_of_label(&format!("{LABEL_PREFIX}{name}")) else {
                continue;
            };
            if pid == me || pid_exists(pid) {
                continue;
            }
            let _ = std::fs::remove_dir_all(&path);
        }
    }

    /// `launchctl submit` the one-shot job: the wrapper runs `helper_exe` on `verb` with
    /// the spec file as its one argument — in place, from a clean copy of the binary, or
    /// from a clean copy of its whole bundle, as [`plan_for_exe`] decides; launchd — not
    /// this process — is its parent. Only a binary that cannot be INSPECTED for the tag
    /// is refused here, before anything is submitted.
    pub(crate) fn submit(&mut self, helper_exe: &Path, verb: &str) -> Result<(), String> {
        let plan = plan_for_exe(helper_exe)?;
        self.helper = helper_exe.to_path_buf();
        let (copy, bundle): (PathBuf, PathBuf) = match plan {
            HelperPlan::ExecOriginal => (PathBuf::new(), PathBuf::new()),
            HelperPlan::CopyThenExec => (self.copy.clone(), PathBuf::new()),
            // The replica keeps the bundle's own name (`aterm.app`) inside the job's
            // scratch: the name is not part of the seal, but a `.app` suffix is what
            // every tool that looks at it expects.
            HelperPlan::CopyBundleThenExec { bundle } => {
                let name = bundle.file_name().map_or_else(
                    || std::ffi::OsString::from("bundle.app"),
                    |n| n.to_os_string(),
                );
                (self.dir.join(name), bundle)
            }
        };
        let out = std::process::Command::new("/bin/launchctl")
            .arg("submit")
            .args(["-l", &self.label])
            .arg("-o")
            .arg(&self.out_log)
            .arg("-e")
            .arg(&self.err_log)
            .arg("--")
            .arg("/bin/sh")
            .arg("-c")
            .arg(WRAPPER)
            .arg("atpkg-untracked")
            .arg(helper_exe)
            .arg(&self.spec)
            .arg(verb)
            .arg(&self.status)
            .arg(&self.label)
            .arg(copy)
            .arg(bundle)
            .output()
            .map_err(|e| format!("spawn /bin/launchctl: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "launchctl submit failed ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(())
    }

    /// What launchd says about the label right now.
    fn liveness(&self) -> Liveness {
        let Ok(o) = std::process::Command::new("/bin/launchctl")
            .args(["list", &self.label])
            .output()
        else {
            return Liveness::Unknown;
        };
        if !o.status.success() {
            return Liveness::Unknown;
        }
        let text = String::from_utf8_lossy(&o.stdout);
        if text.contains("\"PID\"") {
            Liveness::Running
        } else {
            Liveness::Registered {
                last_exit: last_exit_status(&text),
            }
        }
    }

    /// The status the wrapper recorded, once the helper has exited.
    fn read_status(&self) -> Option<i32> {
        std::fs::read_to_string(&self.status)
            .ok()?
            .lines()
            .next()?
            .trim()
            .parse()
            .ok()
    }

    /// Block until the result file exists. `Err` names the helper's real fate when it
    /// exited without one (the status the wrapper recorded: exit code, or the signal that
    /// killed it), the job's disappearance when launchd has neither a job nor a status
    /// after [`EXIT_GRACE`], or the [`CEILING`] passing with it still running.
    pub(crate) fn wait_for_result(&self) -> Result<(), String> {
        let started = Instant::now();
        let mut last_liveness = Instant::now();
        let mut gone_since: Option<Instant> = None;
        loop {
            if self.result.exists() {
                return Ok(());
            }
            // The helper writes its answer (temp + rename) BEFORE it exits and the
            // wrapper writes the status AFTER: a status with no result is a helper that
            // never answered.
            if let Some(status) = self.read_status() {
                if self.result.exists() {
                    return Ok(());
                }
                return Err(self.fate(status));
            }
            if started.elapsed() > CEILING {
                return Err(format!(
                    "the untracked helper job {} is still running after {} s",
                    self.label,
                    CEILING.as_secs()
                ));
            }
            if last_liveness.elapsed() >= Duration::from_millis(500) {
                last_liveness = Instant::now();
                match self.liveness() {
                    Liveness::Running => gone_since = None,
                    not_running => {
                        let since = *gone_since.get_or_insert_with(Instant::now);
                        if since.elapsed() >= EXIT_GRACE {
                            // One more look: the status may have landed after the poll.
                            if self.result.exists() {
                                return Ok(());
                            }
                            if let Some(status) = self.read_status() {
                                return Err(self.fate(status));
                            }
                            return Err(self.vanished(not_running));
                        }
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The helper exited (status recorded by the wrapper) without writing a result: say
    /// what happened to it ([`fate_body`]), with its stderr.
    fn fate(&self, status: i32) -> String {
        format!(
            "the untracked helper job {} {}{}",
            self.label,
            fate_body(&self.helper, status),
            self.log_tail()
        )
    }

    /// launchd has no running job and the wrapper recorded no status: the job never
    /// started, or the wrapper itself was killed. Say which, with launchd's last exit
    /// status for the wrapper when the label is still registered.
    fn vanished(&self, liveness: Liveness) -> String {
        let seen = match liveness {
            Liveness::Registered {
                last_exit: Some(wait),
            } => format!(
                "; launchd still lists the label and its last exit status for the wrapper \
                 was {}",
                describe_wait_status(wait)
            ),
            Liveness::Registered { last_exit: None } => {
                "; launchd still lists the label with no run recorded — the job never \
                 started inside the grace"
                    .to_string()
            }
            Liveness::Unknown | Liveness::Running => {
                "; launchd no longer lists the label".to_string()
            }
        };
        format!(
            "the untracked helper job {} is gone without a result or an exit status{seen}{}",
            self.label,
            self.log_tail()
        )
    }

    /// The job's stderr, for a refusal message — bounded, one line.
    fn log_tail(&self) -> String {
        let text = std::fs::read_to_string(&self.err_log).unwrap_or_default();
        let tail: String = text
            .lines()
            .last()
            .unwrap_or("")
            .chars()
            .take(200)
            .collect();
        if tail.is_empty() {
            String::new()
        } else {
            format!(" (stderr: {tail})")
        }
    }

    /// `Ok(Ok(body))`, `Ok(Err(refusal))` for an answered job, `Err` for an unreadable
    /// answer. The body is the second line of the result file — the staging verb's
    /// `tree_root`, the laying verb's file count.
    pub(crate) fn read_result(&self) -> Result<Result<String, String>, String> {
        let text = std::fs::read_to_string(&self.result)
            .map_err(|e| format!("read {}: {e}", self.result.display()))?;
        let mut lines = text.lines();
        let verdict = lines.next().unwrap_or("");
        let body = lines.next().unwrap_or("").to_string();
        match verdict {
            "ok" if !body.is_empty() => Ok(Ok(body)),
            "err" => Ok(Err(body)),
            _ => Err(format!("malformed helper result: {text:?}")),
        }
    }
}

/// What a recorded status says happened to the helper. SIGKILL is named for what it
/// means here — the kernel would not run the binary — because that is the shape a copied
/// bundle executable dies in (exit 137 from the wrapper, empty stderr; 2026-09-13).
#[cfg(target_os = "macos")]
fn fate_body(helper: &Path, status: i32) -> String {
    match status {
        STATUS_COPY_FAILED => format!(
            "never ran: the wrapper could not byte-copy {} into the job's scratch",
            helper.display()
        ),
        126 => format!(
            "exited without a result: {} could not be executed (exit status 126)",
            helper.display()
        ),
        127 => format!(
            "exited without a result: {} was not found (exit status 127)",
            helper.display()
        ),
        129..=192 => {
            let signal = status - 128;
            // SIGKILL is not self-describing: the wrapper records only `128+n`, so the
            // CAUSE is not measured here. The one cause this lane has actually produced
            // is the code-signature kill of a byte copy of a bundle-signed executable,
            // and it is named as the first thing to check — not as the finding. Memory
            // pressure during a multi-GB extract, `launchctl kill -9` and an operator
            // all read identically at this seam.
            let meaning = if signal == 9 {
                "the kernel killed the helper before it answered; this lane's known cause \
                 is a code-signature kill of a byte copy of a bundle-signed executable \
                 (it fails its own Info.plist check), but jetsam under memory pressure and \
                 an explicit kill land here too — the wrapper records the signal, not the \
                 reason"
            } else {
                "the helper crashed"
            };
            format!(
                "was killed by signal {signal} ({}) before it answered — {meaning}; helper {}",
                signal_name(signal),
                helper.display()
            )
        }
        code => format!("exited without a result (exit status {code})"),
    }
}

/// What `launchctl list <label>` says about a job.
#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Liveness {
    /// A live pid.
    Running,
    /// Registered, no pid: not started yet, or exited (with launchd's raw wait status
    /// for the wrapper when one has been recorded).
    Registered { last_exit: Option<i32> },
    /// launchd does not list the label (the job removed itself, or was never submitted).
    Unknown,
}

/// `"LastExitStatus" = N;` from `launchctl list <label>`'s plist text — the raw wait
/// status (`exit 3` reads 768, a SIGKILL reads 9; measured 2026-09-13).
#[cfg(target_os = "macos")]
fn last_exit_status(list_text: &str) -> Option<i32> {
    let idx = list_text.find("\"LastExitStatus\"")?;
    let rest = &list_text[idx..];
    let eq = rest.find('=')?;
    let value: String = rest[eq + 1..]
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    value.parse().ok()
}

/// A raw wait status, spoken: `exit status 3`, or `signal 9 (SIGKILL)`.
#[cfg(target_os = "macos")]
fn describe_wait_status(wait: i32) -> String {
    let signal = wait & 0x7f;
    if signal == 0 {
        format!("exit status {}", (wait >> 8) & 0xff)
    } else {
        format!("signal {signal} ({})", signal_name(signal))
    }
}

/// The names `/bin/sh` and launchd's numbers stand for, for the ones a helper dies of.
#[cfg(target_os = "macos")]
fn signal_name(signal: i32) -> &'static str {
    match signal {
        1 => "SIGHUP",
        2 => "SIGINT",
        3 => "SIGQUIT",
        4 => "SIGILL",
        5 => "SIGTRAP",
        6 => "SIGABRT",
        7 => "SIGEMT",
        8 => "SIGFPE",
        9 => "SIGKILL",
        10 => "SIGBUS",
        11 => "SIGSEGV",
        12 => "SIGSYS",
        13 => "SIGPIPE",
        14 => "SIGALRM",
        15 => "SIGTERM",
        _ => "signal",
    }
}

/// The `<pid>` in a label of ours — `systems.alab.atpkg.<stem>-<pid>-<seq>-<nonce>`,
/// where `<stem>` may itself carry dashes (`stage-helper`, `lay-helper`), so the fields
/// are read from the right. `None` for any other label, including the integration tests'
/// `systems.alab.atpkg.test.<name>.<pid>`, which clean up after themselves.
#[cfg(target_os = "macos")]
fn owner_pid_of_label(label: &str) -> Option<u32> {
    let rest = label.strip_prefix(LABEL_PREFIX)?;
    if rest.starts_with("test.") {
        return None;
    }
    let mut fields = rest.rsplitn(4, '-');
    let _nonce = fields.next()?;
    let _seq = fields.next()?;
    let pid = fields.next()?;
    let _stem = fields.next()?;
    pid.parse().ok()
}

/// Whether a process with `pid` exists (`kill(pid, 0)`: `ESRCH` is the only "no").
#[cfg(target_os = "macos")]
fn pid_exists(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return true;
    };
    // SAFETY: `kill` with signal 0 delivers nothing; it only asks the kernel whether the
    // pid is addressable.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

#[cfg(target_os = "macos")]
impl Drop for Job {
    /// Forget the label and the scratch; a one-shot job that already exited (and, since
    /// 2026-09-13, removed its own label) is a no-op here, a wedged one is stopped. On
    /// drop, so no exit path of the lane — a spec that would not write, a submit that
    /// failed, a result that would not parse — leaves a registered job or a scratch dir
    /// behind.
    fn drop(&mut self) {
        let _ = std::process::Command::new("/bin/launchctl")
            .args(["remove", &self.label])
            .output();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> StageSpec {
        let mut links = BTreeMap::new();
        links.insert(
            "emacs".to_string(),
            "Emacs.app/Contents/MacOS/Emacs".to_string(),
        );
        StageSpec {
            payload: "tar-zst".to_string(),
            entry: "gh".to_string(),
            strip_components: 1,
            links,
            size_cap: 12_345_678,
        }
    }

    /// Every field round-trips, including a path with a space and a quote — the store
    /// lives under `~/Library/Application Support`.
    #[test]
    fn the_spec_round_trips_byte_for_byte() {
        let archive =
            Path::new("/Users//x/Library/Application Support/aterm/pkg/staging/gh/gh's.tar.zst");
        let dest =
            Path::new("/Users//x/Library/Application Support/aterm/pkg/store/gh/9.incoming-1");
        let text = encode_spec(&spec(), archive, dest);
        let (back, a, d) = decode_spec(&text).unwrap();
        assert_eq!(back, spec());
        assert_eq!(a, archive);
        assert_eq!(d, dest);
        assert!(text.starts_with(SPEC_HEADER), "{text}");
    }

    /// A non-UTF-8 path survives: the encoding is bytes, not strings.
    #[cfg(unix)]
    #[test]
    fn a_non_utf8_path_round_trips() {
        use std::os::unix::ffi::OsStrExt as _;
        let raw = std::ffi::OsStr::from_bytes(b"/tmp/\xff\xfe/archive.tar.zst");
        let text = encode_spec(&spec(), Path::new(raw), Path::new("/tmp/dest"));
        let (_, a, _) = decode_spec(&text).unwrap();
        assert_eq!(a.as_os_str().as_bytes(), raw.as_bytes());
    }

    /// The refusals: a foreign header, a half-understood line, a relative path, a bad
    /// digit — none of them yields a spec.
    #[test]
    fn a_spec_the_helper_half_understands_is_refused_whole() {
        let good = encode_spec(&spec(), Path::new("/a/b.tar.zst"), Path::new("/a/dest"));
        assert!(decode_spec("atpkg-stage-spec v0\n").is_err());
        assert!(decode_spec(&good.replace("dest=", "dest")).is_err());
        assert!(
            decode_spec(&good.replace("archive=2f", "archive=61")).is_err(),
            "relative"
        );
        assert!(decode_spec(&good.replace("size_cap=", "size_cap=x")).is_err());
        let bad_hex = format!("{good}payload=zz\n");
        assert!(decode_spec(&bad_hex).is_err());
        let unknown = format!("{good}colour=00\n");
        assert!(decode_spec(&unknown).is_err());
        let no_dest: String = good
            .lines()
            .filter(|l| !l.starts_with("dest="))
            .map(|l| format!("{l}\n"))
            .collect();
        assert_eq!(decode_spec(&no_dest).unwrap_err(), "spec names no dest");
    }

    /// Only the two production spellings are run as helpers; a test harness is not.
    #[test]
    fn only_atpkg_and_aterm_binaries_are_taken_as_helpers() {
        assert!(exe_serves_hidden_verb(Path::new("/x/target/debug/atpkg")));
        assert!(exe_serves_hidden_verb(Path::new(
            "/Applications/aterm.app/Contents/MacOS/aterm"
        )));
        assert!(!exe_serves_hidden_verb(Path::new(
            "/x/target/debug/deps/atpkg-5bcf5aa9619bc54c"
        )));
        assert!(!exe_serves_hidden_verb(Path::new("/usr/bin/true")));
    }

    /// The helper verb answers a bad spec in the result file, not by dying silently: the
    /// parent reads `err` at once and applies its policy, instead of waiting out the grace.
    #[test]
    fn the_helper_reports_a_bad_spec_through_the_result_file() {
        let d = std::env::temp_dir().join(format!("atpkg-stage-helper-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let spec_path = d.join("spec");
        std::fs::write(&spec_path, "not a spec\n").unwrap();
        let code = run_helper(&[spec_path.display().to_string()]);
        assert_eq!(code, ExitCode::from(1));
        let result = std::fs::read_to_string(d.join("result")).unwrap();
        assert!(result.starts_with("err\n"), "{result}");
        assert!(result.contains("header"), "{result}");
        assert!(
            !d.join("result.tmp").exists(),
            "temp answer must be renamed away"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The plan has an arm for EVERY shape — nothing is refused: untagged runs in place,
    /// bundle or not; a tagged free-standing binary is copied; a tagged bundle executable
    /// is run from a copy of its WHOLE bundle, never a lone copy (killed at exec) and never
    /// in place (tracked). The refusal this replaced (2026-09-13) was every install on a
    /// self-updated app, whose bundle carries the tag.
    #[test]
    fn a_tagged_bundle_executable_is_run_from_a_copy_of_its_whole_bundle() {
        let d =
            std::env::temp_dir().join(format!("atpkg-stage-helper-bundle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let contents = d.join("aterm.app").join("Contents");
        std::fs::create_dir_all(contents.join("MacOS")).unwrap();
        let exe = contents.join("MacOS").join("aterm");
        std::fs::write(&exe, b"").unwrap();
        // No Info.plist yet: not a bundle, whatever the path looks like.
        assert_eq!(bundle_of(&exe), None);
        std::fs::write(contents.join("Info.plist"), b"<plist/>").unwrap();
        let bundle = bundle_of(&exe).expect("Contents/MacOS/<exe> beside Info.plist");
        assert_eq!(bundle, d.join("aterm.app"));
        assert_eq!(
            plan_helper(&exe, false, Some(&bundle)),
            HelperPlan::ExecOriginal
        );
        assert_eq!(
            plan_helper(&exe, true, Some(&bundle)),
            HelperPlan::CopyBundleThenExec {
                bundle: bundle.clone()
            }
        );
        // Free-standing.
        let free = Path::new("/x/target/debug/aterm");
        assert_eq!(bundle_of(free), None);
        assert_eq!(plan_helper(free, false, None), HelperPlan::ExecOriginal);
        assert_eq!(plan_helper(free, true, None), HelperPlan::CopyThenExec);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Quarantine is taint. A clean copy of a base-OS binary plans in place; the same
    /// copy stamped `com.apple.quarantine` — the attribute a browser sets, and one a
    /// test CAN mint, unlike the provenance tag — plans a copy. This is the
    /// Safari-download shape that read as untagged, ran in place and laid tagged files
    /// (2026-09-14).
    #[cfg(target_os = "macos")]
    #[test]
    fn a_quarantined_helper_is_planned_as_tainted() {
        let d = std::env::temp_dir().join(format!(
            "atpkg-stage-helper-quarantine-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let exe = d.join("atpkg");
        std::fs::copy("/usr/bin/true", &exe).unwrap();
        let tagged = crate::provenance::carries_provenance(&exe);
        assert_eq!(
            plan_for_exe(&exe).unwrap(),
            if tagged {
                HelperPlan::CopyThenExec
            } else {
                HelperPlan::ExecOriginal
            },
            "before the stamp the plan follows the tag alone (this process tracked: {tagged})"
        );
        crate::provenance::set_xattr_for_test(
            &exe,
            crate::provenance::QUARANTINE_XATTR,
            b"0083;00000000;Safari;",
        )
        .unwrap();
        assert_eq!(
            plan_for_exe(&exe).unwrap(),
            HelperPlan::CopyThenExec,
            "quarantined ⇒ copied, never run in place"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The wrapper, run by `/bin/sh` directly (no launchd): in place it makes no copy and
    /// records the helper's exit status; a helper killed by SIGKILL is recorded as 137
    /// (`128 + 9`, the shell's convention); with a copy path it `cat`s the helper there,
    /// mode 0755, and runs the copy; with a bundle root it replicates the WHOLE bundle —
    /// directories, symlinks (relative stays relative), regular files with the executable
    /// bit restored and a name with a space intact — and runs the replica's executable of
    /// the helper's name, from INSIDE the replica; and it exits 0 every time.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_wrapper_records_the_helpers_fate_and_copies_only_when_told_to() {
        use std::os::unix::fs::PermissionsExt as _;
        let d =
            std::env::temp_dir().join(format!("atpkg-stage-helper-wrapper-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let run = |helper: &Path, copy: &Path, bundle: &Path, tag: &str| -> (i32, String) {
            let status = d.join(format!("status-{tag}"));
            let out = std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(WRAPPER)
                .arg("atpkg-untracked")
                .arg(helper)
                .arg(d.join("spec"))
                .arg(HIDDEN_VERB)
                .arg(&status)
                .arg("systems.alab.atpkg.test.no-such-label")
                .arg(copy)
                .arg(bundle)
                .output()
                .unwrap();
            let recorded = std::fs::read_to_string(&status).unwrap_or_default();
            (out.status.code().unwrap_or(-1), recorded)
        };
        let none = Path::new("");
        // In place: /usr/bin/true exits 0; no copy appears.
        let copy = d.join("atpkg");
        let (wrapper_exit, recorded) = run(Path::new("/usr/bin/true"), none, none, "inplace");
        assert_eq!(wrapper_exit, 0);
        assert_eq!(recorded.trim(), "0");
        assert!(!copy.exists(), "no copy was asked for");
        // Copied: the copy exists at 0755 and ran.
        let (wrapper_exit, recorded) = run(Path::new("/usr/bin/true"), &copy, none, "copied");
        assert_eq!(wrapper_exit, 0);
        assert_eq!(recorded.trim(), "0");
        assert!(copy.exists());
        assert_eq!(
            std::fs::metadata(&copy).unwrap().permissions().mode() & 0o755,
            0o755
        );
        // Killed: a helper that SIGKILLs itself is recorded as 137, and the wrapper still exits 0.
        let killer = d.join("killer.sh");
        std::fs::write(&killer, "#!/bin/sh\nkill -9 $$\n").unwrap();
        std::fs::set_permissions(&killer, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (wrapper_exit, recorded) = run(&killer, none, none, "killed");
        assert_eq!(wrapper_exit, 0);
        assert_eq!(recorded.trim(), "137");
        // A copy that cannot be made (destination dir missing) is 125 and runs nothing.
        let (wrapper_exit, recorded) = run(
            &killer,
            &d.join("no-such-dir").join("atpkg"),
            none,
            "nocopy",
        );
        assert_eq!(wrapper_exit, 0);
        assert_eq!(recorded.trim(), STATUS_COPY_FAILED.to_string());
        // A bundle: replicated whole, and the replica's own executable is what runs.
        // The tool records its `$0` (the path it ran as) beside the spec.
        let src = d.join("src.app");
        let macos = src.join("Contents").join("MacOS");
        let resources = src.join("Contents").join("Resources");
        std::fs::create_dir_all(&macos).unwrap();
        std::fs::create_dir_all(&resources).unwrap();
        std::fs::write(src.join("Contents").join("Info.plist"), b"<plist/>").unwrap();
        let tool = macos.join("tool");
        std::fs::write(
            &tool,
            "#!/bin/sh\nprintf '%s\\n' \"$0\" > \"$(dirname \"$2\")/ran-from\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink("tool", macos.join("alias")).unwrap();
        std::fs::write(resources.join("with space.txt"), b"data\n").unwrap();
        std::fs::set_permissions(
            resources.join("with space.txt"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        let replica = d.join("copy.app");
        let (wrapper_exit, recorded) = run(&tool, &replica, &src, "bundle");
        assert_eq!(wrapper_exit, 0);
        assert_eq!(recorded.trim(), "0", "the replica's tool ran and exited 0");
        assert!(replica.join("Contents").join("Info.plist").is_file());
        let replica_tool = replica.join("Contents").join("MacOS").join("tool");
        assert_eq!(
            std::fs::metadata(&replica_tool)
                .unwrap()
                .permissions()
                .mode()
                & 0o755,
            0o755,
            "the executable bit is restored"
        );
        assert_eq!(
            std::fs::read_link(replica.join("Contents").join("MacOS").join("alias")).unwrap(),
            PathBuf::from("tool"),
            "a relative symlink stays relative"
        );
        let data = replica
            .join("Contents")
            .join("Resources")
            .join("with space.txt");
        assert_eq!(std::fs::read(&data).unwrap(), b"data\n");
        assert_eq!(
            std::fs::metadata(&data).unwrap().permissions().mode() & 0o111,
            0,
            "a data file is not made executable"
        );
        assert_eq!(
            std::fs::read_to_string(d.join("ran-from")).unwrap().trim(),
            replica_tool.display().to_string(),
            "the replica's executable ran, not the original"
        );
        // A bundle that cannot be replicated (its root is gone) is 125 and runs nothing.
        let (wrapper_exit, recorded) = run(
            &tool,
            &d.join("copy2.app"),
            &d.join("no-such.app"),
            "nobundle",
        );
        assert_eq!(wrapper_exit, 0);
        assert_eq!(recorded.trim(), STATUS_COPY_FAILED.to_string());
        assert!(!d.join("copy2.app").join("Contents").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The fate report names the real thing: a signal by number and name, an exit status
    /// by number, the copy failure as "never ran" — and the bare "exited without a result"
    /// only when that is all there is. For SIGKILL it names this lane's KNOWN cause (the
    /// code-signature kill of a copied bundle binary) while saying the signal is all the
    /// wrapper recorded: jetsam and an explicit kill reach the same seam, so the report
    /// must not hand back a cause it never measured.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_fate_report_names_the_signal_or_the_exit_status() {
        let helper = Path::new("/x/atpkg");
        let killed = fate_body(helper, 137);
        assert!(killed.contains("signal 9 (SIGKILL)"), "{killed}");
        assert!(killed.contains("code-signature kill"), "{killed}");
        // It must NOT hand back that cause as the finding: the other causes are named
        // and the source of the reading is stated.
        assert!(killed.contains("jetsam"), "{killed}");
        assert!(
            killed.contains("records the signal, not the reason"),
            "{killed}"
        );
        assert!(!killed.contains("exited without a result"), "{killed}");
        let crashed = fate_body(helper, 139);
        assert!(crashed.contains("signal 11 (SIGSEGV)"), "{crashed}");
        assert!(crashed.contains("crashed"), "{crashed}");
        assert_eq!(
            fate_body(helper, 0),
            "exited without a result (exit status 0)"
        );
        assert_eq!(
            fate_body(helper, 3),
            "exited without a result (exit status 3)"
        );
        assert!(fate_body(helper, 126).contains("could not be executed (exit status 126)"));
        assert!(fate_body(helper, 127).contains("was not found (exit status 127)"));
        assert!(fate_body(helper, STATUS_COPY_FAILED).starts_with("never ran"));
    }

    /// launchd's `LastExitStatus` is a raw wait status (measured: `exit 3` → 768, SIGKILL
    /// → 9); it is parsed off the plist text and spoken as an exit status or a signal.
    #[cfg(target_os = "macos")]
    #[test]
    fn launchds_last_exit_status_is_a_wait_status_and_is_spoken_as_one() {
        let text = "{\n\t\"LimitLoadToSessionType\" = \"Aqua\";\n\t\"Label\" = \"x\";\n\t\"LastExitStatus\" = 768;\n};";
        assert_eq!(last_exit_status(text), Some(768));
        assert_eq!(describe_wait_status(768), "exit status 3");
        assert_eq!(describe_wait_status(9), "signal 9 (SIGKILL)");
        assert_eq!(describe_wait_status(0), "exit status 0");
        assert_eq!(last_exit_status("{\n\t\"PID\" = 12;\n};"), None);
    }

    /// The sweep reads the owning pid off our labels only: the two shapes the lanes mint
    /// (a dashed stem), never the integration tests' `test.` labels or a foreign label.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_sweep_reads_the_owning_pid_off_our_labels_only() {
        assert_eq!(
            owner_pid_of_label("systems.alab.atpkg.stage-helper-4281-0-18d4bf618a493520"),
            Some(4281)
        );
        assert_eq!(
            owner_pid_of_label("systems.alab.atpkg.lay-helper-4281-1-18d4bf61975232a8"),
            Some(4281)
        );
        assert_eq!(
            owner_pid_of_label("systems.alab.atpkg.test.clean.123"),
            None
        );
        assert_eq!(owner_pid_of_label("com.apple.Finder"), None);
        assert_eq!(owner_pid_of_label("systems.alab.atpkg.odd"), None);
        assert!(pid_exists(std::process::id()), "this process exists");
        assert!(pid_exists(1), "launchd exists (EPERM is not ESRCH)");
    }
}
