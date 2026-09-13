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
//! # The mechanism (every step measured, `scratchpad/provenance/probe2.sh` m6/m8/m17/m18)
//!
//! 1. The tracked parent writes a small SPEC (archive, destination, the fields
//!    [`crate::install::stage_payload_spec`] reads) into the per-program staging scratch.
//! 2. It submits a one-shot launchd job — `/bin/sh -c 'cat "$1" > "$2" && chmod 755 "$2"
//!    && exec "$2" __stage-payload "$3"'` — that BYTE-COPIES this very binary (`cat`, by
//!    the untracked job: the copy is clean; `cp`/`ditto` would carry the tag across),
//!    and execs the copy on the hidden verb. The copy is named `atpkg` so the `aterm`
//!    multi-tool's argv0 alias routes it to this CLI.
//! 3. The copy runs the ordinary extractor — the same vetting, caps, mode sanitizing and
//!    fused `tree_root` fold as the in-process lane, because it IS the in-process lane in
//!    another process — into the `incoming` scratch the parent created, and writes the
//!    folded root to a result file (temp + rename).
//! 4. The parent waits for the result, removes the job, and carries on exactly as before:
//!    the tree_root re-verify, the swap (a tracked parent renaming a clean DIRECTORY tags
//!    the directory and leaves the files inside clean — measured), the marker.
//!
//! # What it is not
//!
//! Not a trust boundary: the copy is our own bytes in our own `0700` staging dir under
//! the store lock, and the root it reports is re-verified against the SIGNED root by the
//! parent as always (`ATPKG_STAGE_DISK_REVERIFY` re-arms the on-disk walk). Not
//! optional once it is needed: a tracked installer whose lane cannot run — no launchd, a
//! job that never starts, a helper that does not answer — REFUSES the install
//! ([`crate::install::StageError::TrackedInstaller`]) rather than lay a bundle that would
//! carry the tag on every executable; the one escape hatch,
//! `ATPKG_ALLOW_TRACKED_INSTALL=1`, accepts the in-process stage and RECORDS it beside the
//! build (`<build>.tracked-install`), which `aterm pkg doctor` reports as the cause and
//! `aterm pkg repair` names as needing a re-seed ([`crate::install::stage_for_store_with`]
//! has the decision). And it is only half of the story: `bin/<tool>` is a `#!/bin/sh`
//! script, and a tagged shim tracks the tool it execs (law m21) — so the shims, the
//! `agents/` twins, the pending and reroute stubs and the tombstones go through the same
//! job under a second hidden verb ([`crate::lay`]), sharing the [`Job`] below.
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

/// How long the parent tolerates "no result AND no running job" before calling the lane
/// dead — covers launchd's start latency and the 50 MB `cat` of the helper copy.
const EXIT_GRACE: Duration = Duration::from_secs(3);

/// The absolute ceiling on one staged extraction (the shipped `trust` member is 3.4 GB;
/// a slow disk is minutes, never hours). Past this the lane is declared wedged — a lane
/// failure, which the caller's policy answers (refuse by default).
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
/// be copied, exec'd, and answer nothing, costing the exit grace for no result. This is a
/// guard on OUR OWN spelling, not a measurement of the subject; the result file is the
/// only proof the lane accepts.
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
/// the job's parent and the copy is clean bytes, so every measurement came back clean);
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

/// Stage `archive` at `dest` through an untracked launchd job running a clean byte copy
/// of `helper_exe` (see the module docs), returning the folded `tree_root` and the
/// measured outcome ([`StagedUntracked`]).
///
/// `scratch` holds the job's spec, logs and the copy — the per-program staging dir, a
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
            "{} is not an atpkg/aterm binary, so a copy of it would not serve {HIDDEN_VERB}",
            helper_exe.display()
        ));
    }
    // `Job` removes its label and scratch on drop, so every early `?` below leaves
    // neither a registered launchd job nor a spec file behind.
    let job = Job::prepare(scratch, "stage-helper")?;
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

/// One submitted job: its scratch dir, label and files. Shared by the two hidden verbs —
/// the staging lane here and the executable-laying lane ([`crate::lay`]) — so there is
/// ONE launchd choreography (byte copy, exec, result file, grace, ceiling, cleanup).
#[cfg(target_os = "macos")]
pub(crate) struct Job {
    dir: PathBuf,
    label: String,
    pub(crate) spec: PathBuf,
    result: PathBuf,
    copy: PathBuf,
    out_log: PathBuf,
    err_log: PathBuf,
}

#[cfg(target_os = "macos")]
impl Job {
    /// Create the job's `0700` scratch under `scratch`, named `<stem>-<pid>-<seq>-<nonce>`
    /// (the label is `systems.alab.atpkg.` + that name).
    pub(crate) fn prepare(scratch: &Path, stem: &str) -> Result<Self, String> {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let stem = format!("{stem}-{}-{seq}-{nonce:x}", std::process::id());
        let dir = scratch.join(&stem);
        std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        crate::platform::set_mode(&dir, 0o700)
            .map_err(|e| format!("chmod {}: {e}", dir.display()))?;
        Ok(Self {
            label: format!("systems.alab.atpkg.{stem}"),
            spec: dir.join("spec"),
            result: dir.join("result"),
            // Named `atpkg` so the `aterm` multi-tool routes the copy here by argv0.
            copy: dir.join("atpkg"),
            out_log: dir.join("out.log"),
            err_log: dir.join("err.log"),
            dir,
        })
    }

    /// `launchctl submit` the one-shot job: byte-copy `helper_exe` (by the untracked
    /// job, so the copy is clean), exec the copy on `verb` with the spec file as its one
    /// argument. launchd — not this process — is its parent.
    pub(crate) fn submit(&self, helper_exe: &Path, verb: &str) -> Result<(), String> {
        let script = "cat \"$1\" > \"$2\" && chmod 755 \"$2\" && exec \"$2\" \"$4\" \"$3\"";
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
            .arg(script)
            .arg("atpkg-untracked")
            .arg(helper_exe)
            .arg(&self.copy)
            .arg(&self.spec)
            .arg(verb)
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

    /// Whether launchd still reports a live pid for the label.
    fn running(&self) -> bool {
        std::process::Command::new("/bin/launchctl")
            .args(["list", &self.label])
            .output()
            .is_ok_and(|o| {
                o.status.success() && String::from_utf8_lossy(&o.stdout).contains("\"PID\"")
            })
    }

    /// Block until the result file exists. `Err` when the job is gone without one (after
    /// [`EXIT_GRACE`]), or [`CEILING`] passes with it still running.
    pub(crate) fn wait_for_result(&self) -> Result<(), String> {
        let started = Instant::now();
        let mut last_liveness = Instant::now();
        let mut gone_since: Option<Instant> = None;
        loop {
            if self.result.exists() {
                return Ok(());
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
                if self.running() {
                    gone_since = None;
                } else {
                    let since = *gone_since.get_or_insert_with(Instant::now);
                    if since.elapsed() >= EXIT_GRACE {
                        return Err(format!(
                            "the untracked helper job {} exited without a result{}",
                            self.label,
                            self.log_tail()
                        ));
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
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

#[cfg(target_os = "macos")]
impl Drop for Job {
    /// Forget the label and the scratch; a one-shot job that already exited is simply
    /// unregistered, a wedged one is stopped. On drop, so no exit path of the lane —
    /// a spec that would not write, a submit that failed, a result that would not parse
    /// — leaves a registered job or a scratch dir behind.
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

    /// Only the two production spellings are copied and exec'd; a test harness is not.
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
}
