// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Control-thread parser, formatter, and main-thread dispatch for the `aos` verb.

use aterm_aos::contract::Verdict;
use aterm_aos::evidence::EvidenceGrade;
use aterm_aos::publish::Head;
use aterm_aos_pane::trace::{PANE_TRACE_MAX_BYTES, PaneCheck, PaneTrace, PaneTraceFault};
#[cfg(debug_assertions)]
use aterm_effects::aos_momentum::bundle::CandidateBundle;
#[cfg(debug_assertions)]
use aterm_effects::aos_momentum::m4::Admitted;
use aterm_effects::aos_momentum::{
    DEMO_CODE_NAME, MomentumAosStatus, MomentumCode, MomentumTrace, TraceCheck, TraceFault,
};

#[cfg(debug_assertions)]
use crate::aos_host::PlanPublished;
use crate::aos_host::{Exported, Repaired, Resumed, TraceSummary};

/// Whether this build answers the debug-only forms ([`cmd`]): `publish`, `bundle` and the demonstration code.
const DEBUG_FORMS: bool = cfg!(debug_assertions);

/// The demonstration code's names, which a release build refuses by name.
const DEMO_NAMES: [&str; 2] = ["demo-stranded-settle", DEMO_CODE_NAME];

/// The usage line every junk form answers with.
const USAGE: &str = "usage: aos [status|ls|attach [code=<name>]|pause|repair [code=<name>]|\
                     publish plan=<24 hex>|bundle|resume|detach|trace [hex]|export [kept=<n>]|\
                     check <file>]";

/// The largest momentum trace file `aos check` decodes, refused past it: at least twice the largest a window exports.
const CHECK_MAX_BYTES: usize = aterm_effects::aos_momentum::MOMENTUM_TRACE_MAX_BYTES;

/// The largest file `aos check` reads, refused past it before it is decoded: a pane trace's bound (`rewind check export`), the larger.
const READ_MAX_BYTES: usize = if PANE_TRACE_MAX_BYTES > CHECK_MAX_BYTES {
    PANE_TRACE_MAX_BYTES
} else {
    CHECK_MAX_BYTES
};

/// The directory under the state dir that `aos export` writes and a bare `aos check` name reads.
const EXPORT_DIR: &str = "aos";

/// The bytes of a schedule plan, as `publish plan=` spells them in hex.
const PLAN_BYTES: usize = 12;

#[cfg(debug_assertions)]
const _: () = assert!(PLAN_BYTES == aterm_effects::aos_momentum::plan::PLAN_LEN);

/// What `publish` answers when its ask reaches the main thread without an evaluated plan.
pub(crate) const UNEVALUATED: &str = "aos publish: no admitted plan came with the ask";

/// The refusal a debug-only form gets in a release build.
pub(crate) fn needs_debug(form: &str) -> String {
    format!("aos {form} needs a debug build")
}

/// What the verb asks of the front window's momentum glow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Asked {
    /// Read it (`status`, or bare).
    Status,
    /// List every window's hosted task and its timeline.
    Ls,
    /// Start hosting it under this code.
    Attach(MomentumCode),
    /// Pause it at this step boundary.
    Pause,
    /// Publish this code over it (pausing first if it runs).
    Repair(MomentumCode),
    /// Evaluate this schedule plan and, if admitted, publish it over the glow.
    Publish([u8; PLAN_BYTES]),
    /// Export the bundle the live plan was published with.
    Bundle,
    /// Reject the pending update: the old code resumes.
    Resume,
    /// Stop hosting it.
    Detach,
    /// Summarize the exported trace.
    Trace,
    /// Summarize the exported trace and export it, as one trace file in hex.
    TraceHex,
}

/// The verb's arguments, where a build without `debug_forms` refuses `publish`, `bundle` and the demonstration code by name.
pub(crate) fn parse(rest: &str, debug_forms: bool) -> Result<Asked, String> {
    let words: Vec<&str> = rest.split_whitespace().collect();
    Ok(match words.as_slice() {
        [] | ["status"] => Asked::Status,
        ["ls"] => Asked::Ls,
        ["attach"] => Asked::Attach(MomentumCode::Shipped),
        ["attach", code] => Asked::Attach(code_of(code, debug_forms)?),
        ["pause"] => Asked::Pause,
        ["repair"] => Asked::Repair(MomentumCode::Shipped),
        ["repair", code] => Asked::Repair(code_of(code, debug_forms)?),
        [form @ ("publish" | "bundle"), ..] if !debug_forms => return Err(needs_debug(form)),
        ["publish", plan] => Asked::Publish(plan_of(plan)?),
        ["bundle"] => Asked::Bundle,
        ["resume"] => Asked::Resume,
        ["detach"] => Asked::Detach,
        ["trace"] => Asked::Trace,
        ["trace", "hex"] => Asked::TraceHex,
        _ => return Err(USAGE.to_string()),
    })
}

/// One form of the verb by where it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Form {
    /// An ask the main thread answers (`Wake::Aos`).
    Main(Asked),
    /// Write the front window's live trace, or its kept fault `n`, to a private file (`Wake::AosExport`, then this thread).
    Export(Option<u64>),
    /// Replay this trace file under this build, on this thread.
    Check(String),
}

/// [`parse`], with `export [kept=<n>]` and `check <file>`, whose file is the rest of the line.
pub(crate) fn parse_form(rest: &str, debug_forms: bool) -> Result<Form, String> {
    let trimmed = rest.trim();
    if let Some(file) = trimmed.strip_prefix("check")
        && (file.is_empty() || file.starts_with(char::is_whitespace))
    {
        let file = file.trim();
        if file.is_empty() {
            return Err(USAGE.to_string());
        }
        return Ok(Form::Check(file.to_owned()));
    }
    let words: Vec<&str> = trimmed.split_whitespace().collect();
    match words.as_slice() {
        ["export"] => Ok(Form::Export(None)),
        ["export", kept] => kept
            .strip_prefix("kept=")
            .filter(|n| n.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|n| n.parse::<u64>().ok())
            .map(|n| Form::Export(Some(n)))
            .ok_or_else(|| USAGE.to_string()),
        _ => parse(rest, debug_forms).map(Form::Main),
    }
}

fn code_of(word: &str, debug_forms: bool) -> Result<MomentumCode, String> {
    let Some(name) = word.strip_prefix("code=") else {
        return Err(USAGE.to_string());
    };
    if !debug_forms && DEMO_NAMES.contains(&name) {
        return Err(needs_debug(word));
    }
    MomentumCode::from_name(name).ok_or_else(|| {
        let names: Vec<&str> = MomentumCode::ALL.iter().map(|c| c.name()).collect();
        format!(
            "{USAGE} (no code `{name}`; this build carries {})",
            names.join(", ")
        )
    })
}

fn plan_of(word: &str) -> Result<[u8; PLAN_BYTES], String> {
    let Some(hex) = word.strip_prefix("plan=") else {
        return Err(USAGE.to_string());
    };
    let refused = || {
        format!(
            "{USAGE} (plan= takes {} hex digits, a schedule plan's {PLAN_BYTES} bytes; got `{hex}`)",
            PLAN_BYTES * 2
        )
    };
    let digits = hex.as_bytes();
    if digits.len() != PLAN_BYTES * 2 {
        return Err(refused());
    }
    let mut plan = [0u8; PLAN_BYTES];
    let (pairs, _) = digits.as_chunks::<2>();
    for (byte, &[hi, lo]) in plan.iter_mut().zip(pairs) {
        let nibble = |c: u8| char::from(c).to_digit(16);
        let (Some(hi), Some(lo)) = (nibble(hi), nibble(lo)) else {
            return Err(refused());
        };
        *byte = u8::try_from((hi << 4) | lo).map_err(|_| refused())?;
    }
    Ok(plan)
}

/// The lowercase hex of `bytes`.
#[cfg(any(test, debug_assertions))]
fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Evaluates `plan` under the pinned policy in `build` on the control thread.
///
/// # Errors
/// The pinned policy's refusal, or the candidate's first refusal, as a line.
#[cfg(debug_assertions)]
pub(crate) fn evaluate(build: &str, plan: &[u8]) -> Result<Admitted, String> {
    let evaluator =
        aterm_effects::aos_momentum::m4::Evaluator::v1(build, std::time::Instant::now())
            .map_err(|e| format!("aos publish: the pinned policy is refused: {e}"))?;
    let mut admitted = evaluator
        .admit_one(plan)
        .map_err(|r| format!("aos publish: the plan is not admitted: {r}"))?;
    admitted.declare_toolchain(crate::build_info::compiler_version_line());
    Ok(admitted)
}

/// Main-thread payload: an ask, or an admitted plan to publish.
#[derive(Debug)]
pub(crate) enum Ask {
    /// An ask the main thread answers itself.
    Asked(Asked),
    /// An admitted plan to publish over the front window's glow.
    #[cfg(debug_assertions)]
    Publish(Box<Admitted>),
}

impl From<Asked> for Ask {
    fn from(asked: Asked) -> Self {
        Ask::Asked(asked)
    }
}

fn verdict_word(v: &Verdict) -> String {
    match v {
        Verdict::Pass => "pass".to_string(),
        Verdict::Fail { cursor, law, .. } => format!("fail:{law}@{cursor}"),
    }
}

const fn grade_word(g: EvidenceGrade) -> &'static str {
    match g {
        EvidenceGrade::Tested => "tested",
    }
}

fn offset_word(v: Option<i128>) -> String {
    v.map_or_else(|| "none".to_string(), |t| t.to_string())
}

fn token(v: Option<&str>) -> String {
    v.map_or_else(|| "none".to_string(), |s| s.replace(' ', "_"))
}

/// Formats the `status` answer as a single `k=v` line with signed-nanosecond epoch offsets; an
/// attached line says whether the person's grant to keep keys aterm cannot see echoed is live
/// (`unseen=on`).
pub(crate) fn status_line(window: u64, s: &MomentumAosStatus, unseen: bool) -> String {
    let mut line = format!(
        "window={window} attached={} code={} hot={} active={} settling={}",
        s.attached, s.code, s.hot, s.is_active, s.settling
    );
    if !s.attached {
        return line;
    }
    line.push_str(if unseen { " unseen=on" } else { " unseen=off" });
    let st = &s.stats;
    line.push_str(&format!(
        " build={} code_digest={} rev={} branch={} paused={} queued={}/{} log={} log_epoch={} \
         cursor={} broken={} checkpoint={} state={} chain={} latched={:.4} fp={:#x}/{:#x} \
         clock={} cold_at={} query_at={} deadline={}",
        token(s.build.as_deref()),
        token(s.code_digest.as_deref()),
        s.revision,
        s.branch,
        s.paused,
        s.queued,
        s.queue_cap,
        s.log_len,
        s.log_epoch,
        s.input_cursor,
        s.broken.is_some(),
        token(s.checkpoint.as_deref()),
        token(s.state_digest.as_deref()),
        token(s.observation_chain.as_deref()),
        s.latched.unwrap_or(0.0),
        s.fp_last.unwrap_or(0),
        s.fp_prev.unwrap_or(0),
        offset_word(s.clock),
        offset_word(s.cold_at),
        offset_word(s.last_query_at),
        offset_word(s.next_deadline),
    ));
    if let Some(fr) = s.last_frame {
        line.push_str(&format!(
            " frame_fill={} frame_hot={} frame_fp={:#x} quads={}+{}",
            fr.fill
                .map_or_else(|| "none".to_string(), |c| format!("{c:06x}")),
            fr.hot,
            fr.fp,
            fr.out,
            fr.under
        ));
    }
    if let (Some(moved), Some(firings)) = (s.guard_moved, s.guard_firings) {
        line.push_str(&format!(" guard=moved:{moved},firings:{firings}"));
    }
    line.push_str(&format!(
        " replays={} publications={} rejections={} delivered={} refused={} skipped={} \
         queued_max={} reanchors={} breaks={} restores={} clamped={}",
        st.replays,
        st.publications,
        st.rejections,
        st.delivered,
        st.refused,
        st.skipped,
        st.queued_max,
        st.reanchors,
        st.breaks,
        st.restores,
        s.clamped
    ));
    if let Some(why) = s.broken {
        line.push_str(&format!(" broken_why={}", token(Some(why))));
    }
    line
}

/// The `pause` answer: the expected live head it captured.
pub(crate) fn pause_line(window: u64, head: &Head) -> String {
    format!(
        "window={window} paused rev={} branch={} cursor={} state={} identity={}",
        head.revision.0,
        head.branch.0,
        head.input_cursor,
        head.state_digest.short(),
        head.identity.short()
    )
}

/// The `repair` answer: the published revision and the witness it was published on.
pub(crate) fn repair_line(r: &Repaired) -> String {
    let w = &r.commit.witness;
    let (from, to) = w.range();
    format!(
        "window={} repaired paused_first={} rev={} code={} live={} live_verdict={} \
         candidate_verdict={} same_code_reproduced={} grade={} trace={} ancestor={} \
         range={from}..{to} log_epoch={} delivered={} main_us={} off_main={}",
        r.window.0,
        r.paused_first,
        r.commit.revision.0,
        w.candidate_code().name,
        w.live_code().name,
        verdict_word(w.live_verdict()),
        verdict_word(w.candidate_verdict()),
        w.same_code_reproduced(),
        grade_word(w.grade()),
        w.trace_digest().short(),
        w.ancestor().short(),
        w.log_epoch(),
        r.commit.delivered.len(),
        r.main.as_micros(),
        r.off_main
    )
}

/// A row verdict as [`verdict_word`] writes a witness's.
#[cfg(debug_assertions)]
fn row_word(v: &aterm_effects::aos_momentum::bundle::RowVerdict) -> String {
    use aterm_effects::aos_momentum::bundle::RowVerdict;
    match v {
        RowVerdict::NotRun => "not-run".to_string(),
        RowVerdict::Pass => "pass".to_string(),
        RowVerdict::Fail { law, cursor } => format!("fail:{law}@{cursor}"),
    }
}

/// Formats the `publish` answer from the published plan, selection gains, witness, and bundle.
#[cfg(debug_assertions)]
pub(crate) fn publish_line(r: &PlanPublished) -> String {
    let parts = r.published.bundle().parts();
    let sel = &parts.selection;
    let mut line = format!(
        "window={} published paused_first={} rev={} plan={} plan_digest={} useful={} \
         gain_ppm={} s0_gain_ppm={}",
        r.window.0,
        r.paused_first,
        r.published.revision().0,
        hex_of(&parts.plan),
        aterm_effects::aos_momentum::search::candidate_id(&parts.plan).short(),
        sel.useful,
        sel.gain_ppm,
        sel.s0_gain_ppm
    );
    if let Some(p) = r.published.publication() {
        line.push_str(&format!(
            " code={} live={} live_verdict={} candidate_verdict={} trace={} ancestor={} \
             range={}..{} log_epoch={}",
            p.candidate_code.name,
            p.live_code.name,
            row_word(&p.live_verdict),
            row_word(&p.candidate_verdict),
            p.trace.short(),
            p.ancestor.short(),
            p.from,
            p.to,
            p.log_epoch
        ));
    }
    line.push_str(&format!(
        " grade={} bundle={} delivered={}",
        parts.grade.to_lowercase(),
        r.published.bundle().digest().short(),
        r.published.delivered().len()
    ));
    line
}

/// Formats the `bundle` answer with full digest, revision, byte length, and lowercase hex.
#[cfg(debug_assertions)]
pub(crate) fn bundle_line(window: u64, revision: u64, bundle: &CandidateBundle) -> String {
    let bytes = bundle.encode();
    format!(
        "window={window} bundle={} rev={revision} bytes={} hex={}",
        bundle.digest().hex(),
        bytes.len(),
        hex_of(&bytes)
    )
}

/// The `resume` answer: what the old code was handed from the queue.
pub(crate) fn resume_line(r: &Resumed) -> String {
    let refused = r.delivered.iter().filter(|d| d.is_err()).count();
    format!(
        "window={} resumed delivered={} refused={refused}",
        r.window.0,
        r.delivered.len()
    )
}

/// Formats the `trace` answer from the decoded trace summary and per-kind input counts.
pub(crate) fn trace_line(t: &TraceSummary) -> String {
    let mut line = format!(
        "window={} log={} log_epoch={} first={} end={} trace={} checkpoint={} identity={} \
         broken={} bytes={}+{}",
        t.window.0,
        t.log_len,
        t.log_epoch,
        t.first,
        t.end,
        t.trace.map_or_else(|| "none".to_string(), |d| d.hex()),
        t.checkpoint.hex(),
        t.identity.short(),
        t.broken.is_some(),
        t.checkpoint_bytes,
        t.log_bytes
    );
    if let Some(why) = t.broken {
        line.push_str(&format!(" broken_why={}", token(Some(why))));
    }
    let c = &t.counts;
    line.push_str(&format!(
        " sealed=true keys={} resets={} ticks={} queries={} unseen={}",
        c.keys, c.resets, c.ticks, c.queries, t.unseen
    ));
    line
}

/// Formats the `trace hex` answer from [`trace_line`] and the exported trace file in hex.
pub(crate) fn trace_file_line(t: &TraceSummary, file: &[u8]) -> String {
    let mut line = trace_line(t);
    line.push_str(&format!(
        " file_bytes={} hex={}",
        file.len(),
        aterm_codec::hex::encode_lower(file)
    ));
    line
}

/// The `export` answer's words before the file's path, which ends the line since the state dir may hold a space.
pub(crate) fn export_line(e: &Exported, file: &str) -> String {
    format!(
        "window={} exported kept={} log_epoch={} identity={} bytes={} unseen={} mode=0600 \
         file={file}",
        e.window.0,
        e.kept.map_or_else(|| "live".to_string(), |k| k.to_string()),
        e.log_epoch,
        e.identity.short(),
        e.bytes.len(),
        e.unseen
    )
}

/// Writes `bytes` as a new private (0600) file `name` in `root`'s export directory and returns its path.
/// # Errors
/// The directory is a link or not a directory, is not ours or not private, or the file exists or did not write.
pub(crate) fn write_export(
    root: &std::path::Path,
    name: &str,
    bytes: &[u8],
) -> std::io::Result<std::path::PathBuf> {
    let dir = private_child(root, EXPORT_DIR, "aos export")?;
    dir.write_new_private(std::ffi::OsStr::new(name), bytes)?;
    Ok(dir.path().join(name))
}

/// `root`'s directory `leaf` opened with no link followed (made 0700 when absent), its owner checked and its mode set to 0700 through the opened directory, never through a path; `verb` names the writer in a refusal.
/// # Errors
/// The leaf is a link or not a directory, belongs to another user, or cannot be made or opened.
pub(crate) fn private_child(
    root: &std::path::Path,
    leaf: &str,
    verb: &str,
) -> std::io::Result<crate::pinned_dir::PinnedDir> {
    use std::io::{Error, ErrorKind};
    aterm_types::fs_restricted::create_dir_restricted(root)?;
    let state = crate::pinned_dir::PinnedDir::open_resolved(root)?;
    let name = std::ffi::OsStr::new(leaf);
    let dir = match state.child(name) {
        Ok(dir) => dir,
        Err(e) if e.kind() == ErrorKind::NotFound => state.create_child(name)?,
        Err(e) => {
            let path = root.join(leaf);
            let link = std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink());
            return Err(if link {
                Error::new(
                    ErrorKind::InvalidInput,
                    format!(
                        "{}: is a symbolic link; {verb} writes only into a real directory",
                        path.display()
                    ),
                )
            } else {
                Error::new(e.kind(), format!("{}: {e}", path.display()))
            });
        }
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = dir.leaf_metadata()?;
        if meta.uid() != aterm_uds::peer::our_uid() {
            return Err(Error::new(
                ErrorKind::PermissionDenied,
                format!("{}: belongs to another user", dir.path().display()),
            ));
        }
        if meta.mode() & 0o7777 != 0o700 {
            dir.restrict_to_owner()?;
        }
    }
    Ok(dir)
}

/// A fresh export file name: the window, the log epoch, the wall clock and a per-process count.
fn export_name(e: &Exported) -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let what = e
        .kept
        .map_or_else(|| "live".to_string(), |k| format!("kept{k}"));
    format!(
        "momentum-w{}-{what}-e{}-{ms}-{}-{n}.trace",
        e.window.0,
        e.log_epoch,
        std::process::id()
    )
}

/// The path `aos check` reads: a bare name in `root`'s export directory, or an absolute path.
/// # Errors
/// A relative path with a separator, or a bare name with no state dir.
pub(crate) fn check_path(
    root: Option<&std::path::Path>,
    file: &str,
) -> Result<std::path::PathBuf, String> {
    let path = std::path::Path::new(file);
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    if path.components().count() != 1 || file.contains(std::path::MAIN_SEPARATOR) {
        return Err(format!(
            "aos check: `{file}` is neither a bare export name nor an absolute path"
        ));
    }
    root.map(|r| r.join(EXPORT_DIR).join(file))
        .ok_or_else(|| "aos check: no state directory for a bare export name".to_string())
}

/// `aos check <file>`: reads the trace file (a pane trace at most [`PANE_TRACE_MAX_BYTES`], a momentum trace at most [`CHECK_MAX_BYTES`]), refuses it unless this build runs its identity, and replays it with no live effect.
/// # Errors
/// The read's failure, or the trace's refusal as `refused=<word>: <why>`.
pub(crate) fn check_file(path: &std::path::Path, build: &str) -> Result<String, String> {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(format!("aos check: `{}` names no file", path.display()));
    };
    let dir = crate::pinned_dir::PinnedDir::open_resolved(parent)
        .map_err(|e| format!("aos check: {}: {e}", parent.display()))?;
    let (bytes, _) = dir
        .read_private_at_retained(name, READ_MAX_BYTES)
        .map_err(|e| format!("aos check: {}: {e}", path.display()))?;
    let file = name.to_string_lossy();
    if PaneTrace::is_pane_trace(&bytes) {
        let refused = |f: PaneTraceFault| format!("aos check refused={}: {f}", f.word());
        let trace = PaneTrace::decode(&bytes).map_err(refused)?;
        let c = trace
            .check(aterm_core::terminal::ClockReading::now(), build)
            .map_err(refused)?;
        return Ok(pane_check_line(&file, bytes.len(), &c));
    }
    if bytes.len() > CHECK_MAX_BYTES {
        return Err(format!(
            "aos check: {}: artifact source exceeded its size bound",
            path.display()
        ));
    }
    let refused = |f: TraceFault| format!("aos check refused={}: {f}", f.word());
    let trace = MomentumTrace::decode(&bytes).map_err(refused)?;
    let c = trace
        .check(std::time::Instant::now(), build)
        .map_err(refused)?;
    Ok(check_line(&file, bytes.len(), &c))
}

/// Formats the `check` answer for a pane trace ([`aterm_aos_pane`]) from its replay and the contract's verdict on it.
pub(crate) fn pane_check_line(file: &str, bytes: usize, c: &PaneCheck) -> String {
    format!(
        "checked task={} code={} {} replies={} start={} state={} chain={} verdict={} \
         bytes={bytes} file={file}",
        aterm_aos_pane::TASK_NAME,
        c.code.name(),
        c.counts,
        c.replies_matched,
        c.start.short(),
        c.state_digest.short(),
        c.observation_chain.short(),
        verdict_word(&c.verdict)
    )
}

/// Formats the `check` answer from the replay and the contract's verdict on it; `unseen=` is the
/// replayed head's count of keys logged unseen, the checkpoint's included.
pub(crate) fn check_line(file: &str, bytes: usize, c: &TraceCheck) -> String {
    let r = &c.replay;
    let n = &r.counts;
    format!(
        "checked code={} log_epoch={} first={} keys={} resets={} ticks={} queries={} unseen={} \
         checkpoint={} state={} chain={} verdict={} bytes={bytes} file={file}",
        r.code.name(),
        r.log_epoch,
        r.first_cursor,
        n.keys,
        n.resets,
        n.ticks,
        n.queries,
        r.unseen,
        r.checkpoint.short(),
        r.state_digest.short(),
        r.observation_chain.short(),
        verdict_word(&c.verdict)
    )
}

/// The verb's control-thread half: the ask on the main thread (`Wake::Aos`), an export's bytes from it (`Wake::AosExport`) written here, or a check run here.
pub(crate) fn cmd(
    proxy: &aterm_window::event_loop::EventLoopProxy<crate::Wake>,
    rest: &str,
) -> String {
    let asked = match parse_form(rest, DEBUG_FORMS) {
        Ok(Form::Main(asked)) => asked,
        Ok(Form::Export(kept)) => return export(proxy, kept),
        Ok(Form::Check(file)) => {
            let answer = check_path(aterm_types::dirs::state_dir().as_deref(), &file)
                .and_then(|path| check_file(&path, &aterm_stamp::build_id()));
            return match answer {
                Ok(line) => format!("OK {line}\n"),
                Err(e) => format!("ERR {e}\n"),
            };
        }
        Err(refusal) => return format!("ERR {refusal}\n"),
    };
    #[cfg(debug_assertions)]
    let ask = match asked {
        Asked::Publish(plan) => match evaluate(&aterm_stamp::build_id(), &plan) {
            Ok(admitted) => Ask::Publish(Box::new(admitted)),
            Err(refusal) => return format!("ERR {refusal}\n"),
        },
        other => Ask::Asked(other),
    };
    #[cfg(not(debug_assertions))]
    let ask = Ask::Asked(asked);
    match crate::control::control_media::call_main(proxy, |reply| crate::Wake::Aos { ask, reply }) {
        Ok(answer) => main_reply(asked, answer),
        Err(e) => format!("ERR {e}\n"),
    }
}

/// The wire reply to the main thread's answer to `asked`: `ls`'s rows as its Lines framing reads them (`OK <n>`, then the n rows), any other form's answer as one status line.
pub(crate) fn main_reply(asked: Asked, answer: Result<String, String>) -> String {
    match answer {
        Ok(rows) if asked == Asked::Ls => format!("OK {}\n{rows}\n", rows.lines().count()),
        Ok(line) => format!("OK {line}\n"),
        Err(e) => format!("ERR {e}\n"),
    }
}

/// `aos export [kept=<n>]`: the trace from the main thread, written here under the state dir.
fn export(
    proxy: &aterm_window::event_loop::EventLoopProxy<crate::Wake>,
    kept: Option<u64>,
) -> String {
    let exported = match crate::control::control_media::call_main(proxy, |reply| {
        crate::Wake::AosExport { kept, reply }
    }) {
        Ok(Ok(e)) => e,
        Ok(Err(e)) => return format!("ERR {e}\n"),
        Err(e) => return format!("ERR {e}\n"),
    };
    let Some(root) = aterm_types::dirs::state_dir() else {
        return "ERR aos export: no state directory\n".to_string();
    };
    let name = export_name(&exported);
    match write_export(&root, &name, &exported.bytes) {
        Ok(path) => format!(
            "OK {} path={}\n",
            export_line(&exported, &name),
            path.display()
        ),
        Err(e) => format!("ERR aos export: {e}\n"),
    }
}

/// Whether this target publishes nothing: the pinned acceptance policy names the golden trace's
/// target (aarch64 macOS), and another target's profile is another policy, which [`evaluate`]
/// refuses by name (`policy_tests::the_policy_is_pinned`). Off it, a test that publishes asserts
/// that refusal and goes no further.
#[cfg(all(test, debug_assertions))]
pub(crate) fn refused_off_the_pinned_target() -> bool {
    use aterm_effects::aos_momentum::{corpus::GOLDEN_TRACE_TARGET, target_name};
    if target_name() == GOLDEN_TRACE_TARGET {
        return false;
    }
    let refusal = evaluate("aos-verb-test-build", &[0u8; PLAN_BYTES])
        .expect_err("off the pinned target nothing is admitted");
    assert!(
        refusal.starts_with(
            "aos publish: the pinned policy is refused: acceptance policy refused: its digest"
        ),
        "{refusal}"
    );
    true
}

#[cfg(test)]
#[path = "aos_verb_tests.rs"]
mod tests;
