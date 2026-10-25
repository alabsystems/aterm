// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Control-thread parser, formatter, and main-thread dispatch for the `aos` verb.

use aterm_aos::contract::Verdict;
use aterm_aos::evidence::EvidenceGrade;
use aterm_aos::publish::Head;
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

/// The largest trace file `aos check` reads, refused past it before it is decoded: at least twice the largest a window exports.
const CHECK_MAX_BYTES: usize = aterm_effects::aos_momentum::MOMENTUM_TRACE_MAX_BYTES;

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

/// Formats the `status` answer as a single `k=v` line with signed-nanosecond epoch offsets.
pub(crate) fn status_line(window: u64, s: &MomentumAosStatus) -> String {
    let mut line = format!(
        "window={window} attached={} code={} hot={} active={} settling={}",
        s.attached, s.code, s.hot, s.is_active, s.settling
    );
    if !s.attached {
        return line;
    }
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
        " sealed=true keys={} resets={} ticks={} queries={}",
        c.keys, c.resets, c.ticks, c.queries
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
        "window={} exported kept={} log_epoch={} identity={} bytes={} mode=0600 file={file}",
        e.window.0,
        e.kept.map_or_else(|| "live".to_string(), |k| k.to_string()),
        e.log_epoch,
        e.identity.short(),
        e.bytes.len()
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
    let dir = export_dir(root)?;
    dir.write_new_private(std::ffi::OsStr::new(name), bytes)?;
    Ok(dir.path().join(name))
}

/// `root`'s export directory opened with no link followed (made 0700 when absent), its owner checked and its mode set to 0700 through the opened directory, never through a path.
/// # Errors
/// The leaf is a link or not a directory, belongs to another user, or cannot be made or opened.
fn export_dir(root: &std::path::Path) -> std::io::Result<crate::pinned_dir::PinnedDir> {
    use std::io::{Error, ErrorKind};
    aterm_types::fs_restricted::create_dir_restricted(root)?;
    let state = crate::pinned_dir::PinnedDir::open_resolved(root)?;
    let leaf = std::ffi::OsStr::new(EXPORT_DIR);
    let dir = match state.child(leaf) {
        Ok(dir) => dir,
        Err(e) if e.kind() == ErrorKind::NotFound => state.create_child(leaf)?,
        Err(e) => {
            let path = root.join(EXPORT_DIR);
            let link = std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink());
            return Err(if link {
                Error::new(
                    ErrorKind::InvalidInput,
                    format!(
                        "{}: is a symbolic link; aos export writes only into a real directory",
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

/// `aos check <file>`: reads the trace file at most [`CHECK_MAX_BYTES`], refuses it unless this build runs its identity, and replays it with no live effect.
/// # Errors
/// The read's failure, or the trace's refusal as `refused=<word>: <why>`.
pub(crate) fn check_file(path: &std::path::Path, build: &str) -> Result<String, String> {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(format!("aos check: `{}` names no file", path.display()));
    };
    let dir = crate::pinned_dir::PinnedDir::open_resolved(parent)
        .map_err(|e| format!("aos check: {}: {e}", parent.display()))?;
    let (bytes, _) = dir
        .read_private_at_retained(name, CHECK_MAX_BYTES)
        .map_err(|e| format!("aos check: {}: {e}", path.display()))?;
    let refused = |f: TraceFault| format!("aos check refused={}: {f}", f.word());
    let trace = MomentumTrace::decode(&bytes).map_err(refused)?;
    let c = trace
        .check(std::time::Instant::now(), build)
        .map_err(refused)?;
    Ok(check_line(&name.to_string_lossy(), bytes.len(), &c))
}

/// Formats the `check` answer from the replay and the contract's verdict on it.
pub(crate) fn check_line(file: &str, bytes: usize, c: &TraceCheck) -> String {
    let r = &c.replay;
    let n = &r.counts;
    format!(
        "checked code={} log_epoch={} first={} keys={} resets={} ticks={} queries={} \
         checkpoint={} state={} chain={} verdict={} bytes={bytes} file={file}",
        r.code.name(),
        r.log_epoch,
        r.first_cursor,
        n.keys,
        n.resets,
        n.ticks,
        n.queries,
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
                .and_then(|path| check_file(&path, &aterm_types::version::build_id()));
            return match answer {
                Ok(line) => format!("OK {line}\n"),
                Err(e) => format!("ERR {e}\n"),
            };
        }
        Err(refusal) => return format!("ERR {refusal}\n"),
    };
    #[cfg(debug_assertions)]
    let ask = match asked {
        Asked::Publish(plan) => match evaluate(&aterm_types::version::build_id(), &plan) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_aos_verb_parses_its_forms_and_a_release_build_refuses_only_the_debug_ones() {
        let demo = MomentumCode::DemoStrandedSettle;
        for (rest, want) in [
            ("", Asked::Status),
            ("status", Asked::Status),
            ("  status ", Asked::Status),
            ("attach", Asked::Attach(MomentumCode::Shipped)),
            ("attach code=shipped", Asked::Attach(MomentumCode::Shipped)),
            (
                "attach code=momentum-glow.shipped",
                Asked::Attach(MomentumCode::Shipped),
            ),
            ("pause", Asked::Pause),
            ("repair", Asked::Repair(MomentumCode::Shipped)),
            ("repair code=shipped", Asked::Repair(MomentumCode::Shipped)),
            ("resume", Asked::Resume),
            ("detach", Asked::Detach),
            ("trace", Asked::Trace),
            ("trace hex", Asked::TraceHex),
            (" trace  hex ", Asked::TraceHex),
        ] {
            assert_eq!(parse(rest, true), Ok(want), "{rest:?}");
            assert_eq!(
                parse(rest, false),
                Ok(want),
                "a release build answers {rest:?}"
            );
        }
        for (rest, want, form) in [
            (
                "attach code=demo-stranded-settle",
                Asked::Attach(demo),
                "code=demo-stranded-settle",
            ),
            (
                "attach code=momentum-glow.demo-stranded-settle",
                Asked::Attach(demo),
                "code=momentum-glow.demo-stranded-settle",
            ),
            (
                "repair code=demo-stranded-settle",
                Asked::Repair(demo),
                "code=demo-stranded-settle",
            ),
            ("bundle", Asked::Bundle, "bundle"),
            (" bundle  ", Asked::Bundle, "bundle"),
        ] {
            assert_eq!(parse(rest, true), Ok(want), "{rest:?}");
            assert_eq!(
                parse(rest, false),
                Err(format!("aos {form} needs a debug build")),
                "a release build refuses {rest:?} by name"
            );
        }
        for junk in [
            "bogus",
            "status now",
            "attach shipped",
            "attach code=",
            "attach code=shipped extra",
            "pause now",
            "repair code=nope",
            "resume 1",
            "trace 5",
            "trace HEX",
            "trace hex 5",
            "bundle now",
            "bundle plan=010100000000000000000000",
        ] {
            let refusal = parse(junk, true).expect_err(junk);
            assert!(refusal.starts_with(USAGE), "{junk:?}: {refusal}");
        }
        for junk in ["bogus", "attach code=", "repair code=nope", "trace HEX"] {
            let refusal = parse(junk, false).expect_err(junk);
            assert!(refusal.starts_with(USAGE), "release {junk:?}: {refusal}");
        }
        assert_eq!(
            parse("repair code=nope", true),
            Err(format!(
                "{USAGE} (no code `nope`; this build carries shipped, safe, demo-stranded-settle)"
            ))
        );
    }

    #[test]
    fn r2s_forms_parse_in_every_build_and_check_takes_the_rest_of_the_line() {
        for debug in [true, false] {
            for (rest, want) in [
                ("ls", Form::Main(Asked::Ls)),
                (" ls ", Form::Main(Asked::Ls)),
                (
                    "attach code=safe",
                    Form::Main(Asked::Attach(MomentumCode::Safe)),
                ),
                (
                    "repair code=momentum-glow.safe",
                    Form::Main(Asked::Repair(MomentumCode::Safe)),
                ),
                ("export", Form::Export(None)),
                ("export kept=3", Form::Export(Some(3))),
                ("check x.trace", Form::Check("x.trace".into())),
                (
                    "  check /a dir/with space.trace  ",
                    Form::Check("/a dir/with space.trace".into()),
                ),
                ("status", Form::Main(Asked::Status)),
            ] {
                assert_eq!(parse_form(rest, debug), Ok(want), "debug={debug} {rest:?}");
            }
            for junk in [
                "ls now",
                "export kept=",
                "export kept=-1",
                "export 3",
                "export kept=1 extra",
                "check",
                "check   ",
                "checkx",
                "lsx",
            ] {
                let refusal = parse_form(junk, debug).expect_err(junk);
                assert!(
                    refusal.starts_with(USAGE),
                    "debug={debug} {junk:?}: {refusal}"
                );
            }
        }
        assert_eq!(
            parse_form("attach code=demo-stranded-settle", false),
            Err("aos code=demo-stranded-settle needs a debug build".to_string())
        );
    }

    #[test]
    fn the_export_and_check_lines_name_what_they_wrote_and_replayed() {
        let e = Exported {
            window: crate::WindowId(4),
            kept: None,
            log_epoch: 2,
            identity: aterm_aos::Digest([0xcd; 32]),
            bytes: vec![0; 10],
        };
        assert_eq!(
            export_line(&e, "f.trace"),
            format!(
                "window=4 exported kept=live log_epoch=2 identity={} bytes=10 mode=0600 file=f.trace",
                aterm_aos::Digest([0xcd; 32]).short()
            )
        );
        let kept = Exported {
            kept: Some(1),
            ..e.clone()
        };
        assert!(export_line(&kept, "g").contains(" kept=1 "));
        let name = export_name(&e);
        assert!(
            name.starts_with("momentum-w4-live-e2-") && name.ends_with(".trace"),
            "{name}"
        );
        assert_ne!(export_name(&e), name, "each name is fresh");
        assert!(crate::pinned_dir::is_one_component(std::ffi::OsStr::new(
            &name
        )));
    }

    const WINNER_HEX: &str = "010100000000000000000000";

    #[test]
    fn publish_parses_a_plans_twelve_bytes_and_refuses_every_malformed_form() {
        let mut want = [0u8; PLAN_BYTES];
        want[0] = 1;
        want[1] = 1;
        for rest in [
            format!("publish plan={WINNER_HEX}"),
            format!("  publish   plan={}  ", WINNER_HEX.to_uppercase()),
        ] {
            assert_eq!(parse(&rest, true), Ok(Asked::Publish(want)), "{rest:?}");
            assert_eq!(
                parse(&rest, false),
                Err("aos publish needs a debug build".to_string()),
                "a release build refuses {rest:?} by name"
            );
        }
        assert_eq!(
            parse("publish plan=0a0B0c0D0e0F102030405060", true),
            Ok(Asked::Publish([
                0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f, 0x10, 0x20, 0x30, 0x40, 0x50, 0x60
            ]))
        );
        let short = &WINNER_HEX[..22];
        for junk in [
            "publish".to_string(),
            "publish plan=".to_string(),
            format!("publish {WINNER_HEX}"),
            format!("publish code={WINNER_HEX}"),
            format!("publish plan={short}"),
            format!("publish plan={WINNER_HEX}0"),
            format!("publish plan={WINNER_HEX}00"),
            format!("publish plan=0x{short}"),
            format!("publish plan={short}zz"),
            format!("publish plan={short} 00"),
            format!("publish plan={WINNER_HEX} extra"),
            "repair code=schedule-plan".to_string(),
            "repair code=momentum-glow.schedule-plan".to_string(),
            "attach code=schedule-plan".to_string(),
        ] {
            let refusal = parse(&junk, true).expect_err(&junk);
            assert!(refusal.starts_with(USAGE), "{junk:?}: {refusal}");
        }
        assert_eq!(
            parse(&format!("publish plan={short}zz"), true),
            Err(format!(
                "{USAGE} (plan= takes 24 hex digits, a schedule plan's 12 bytes; got `{short}zz`)"
            ))
        );
        assert_eq!(hex_of(&want), WINNER_HEX);
    }

    /// The evaluation refuses, naming why, bytes that are not a plan and two plans the training split refuses, and admits the grid winner as useful in the build it was given with this build's toolchain declared.
    #[cfg(debug_assertions)]
    #[test]
    fn evaluate_refuses_a_plan_the_evaluator_does_not_admit() {
        use aterm_effects::aos_momentum::plan::{SchedulePlan, Split, Wake};
        let build = "aos-verb-test-build";
        let not_a_plan = evaluate(build, &[0u8; PLAN_BYTES]).expect_err("not a plan");
        assert!(
            not_a_plan.starts_with("aos publish: the plan is not admitted: candidate refused: "),
            "{not_a_plan}"
        );
        for (wake, law) in [(Wake::ColdOnly, "NoStepSlept"), (Wake::Never, "HotHasWake")] {
            let plan = SchedulePlan::new(Split::Shipped, wake)
                .expect("in range")
                .encode();
            let refused = evaluate(build, &plan).expect_err(law);
            assert!(
                refused.starts_with("aos publish: the plan is not admitted: ")
                    && refused.contains(law),
                "{law}: {refused}"
            );
        }
        let winner = parse(&format!("publish plan={WINNER_HEX}"), true);
        let Ok(Asked::Publish(plan)) = winner else {
            panic!("the winner parses: {winner:?}");
        };
        let admitted = evaluate(build, &plan).expect("the grid winner is admitted");
        assert!(admitted.useful());
        assert_eq!(admitted.bundle().parts().build, build);
        assert_eq!(
            admitted.bundle().parts().toolchain,
            crate::build_info::compiler_version_line()
        );
    }

    #[test]
    fn an_unattached_status_line_names_the_window_and_the_shipped_code() {
        let s = MomentumAosStatus {
            code: MomentumCode::Shipped.name(),
            ..MomentumAosStatus::default()
        };
        assert_eq!(
            status_line(3, &s),
            "window=3 attached=false code=shipped hot=false active=false settling=false"
        );
        assert_eq!(token(Some("log full")), "log_full");
        assert_eq!(token(None), "none");
        assert_eq!(offset_word(Some(-5)), "-5");
        assert_eq!(verdict_word(&Verdict::Pass), "pass");
        assert_eq!(
            verdict_word(&Verdict::Fail {
                cursor: 7,
                law: "HotHasWake",
                detail: "a settling glow named no wake".to_string(),
            }),
            "fail:HotHasWake@7"
        );
    }

    #[test]
    fn a_plans_status_line_names_its_guard_and_a_prebuilt_codes_names_none() {
        let plan = MomentumAosStatus {
            attached: true,
            code: "schedule-plan",
            guard_moved: Some(true),
            guard_firings: Some(3),
            ..MomentumAosStatus::default()
        };
        let line = status_line(1, &plan);
        assert!(
            line.contains(" deadline=none guard=moved:true,firings:3 replays=0 "),
            "{line}"
        );
        let shipped = MomentumAosStatus {
            attached: true,
            code: MomentumCode::Shipped.name(),
            ..MomentumAosStatus::default()
        };
        let line = status_line(1, &shipped);
        assert!(
            line.contains(" deadline=none replays=0 ") && !line.contains(" guard="),
            "{line}"
        );
    }

    #[test]
    fn the_trace_line_ends_with_the_seal_and_the_count_of_each_kind() {
        let d = |b: u8| aterm_aos::Digest([b; 32]);
        let mut t = TraceSummary {
            window: crate::WindowId(2),
            log_len: 10,
            log_epoch: 3,
            first: 40,
            end: 50,
            trace: Some(d(0xab)),
            checkpoint: d(0x01),
            identity: d(0xcd),
            broken: None,
            checkpoint_bytes: 120,
            log_bytes: 900,
            counts: aterm_effects::aos_momentum::EventCounts {
                keys: 4,
                resets: 1,
                ticks: 3,
                queries: 2,
            },
        };
        let ab = "ab".repeat(32);
        let one = "01".repeat(32);
        assert_eq!(
            trace_line(&t),
            format!(
                "window=2 log=10 log_epoch=3 first=40 end=50 trace={ab} checkpoint={one} \
                 identity={} broken=false bytes=120+900 sealed=true keys=4 resets=1 ticks=3 queries=2",
                d(0xcd).short()
            )
        );
        t.trace = None;
        t.broken = Some("log full");
        assert_eq!(
            trace_line(&t),
            format!(
                "window=2 log=10 log_epoch=3 first=40 end=50 trace=none checkpoint={one} \
                 identity={} broken=true bytes=120+900 broken_why=log_full sealed=true keys=4 \
                 resets=1 ticks=3 queries=2",
                d(0xcd).short()
            )
        );
        assert_eq!(
            trace_file_line(&t, &[0x41, 0x4f, 0x53, 0x00, 0xff]),
            format!("{} file_bytes=5 hex=414f5300ff", trace_line(&t))
        );
        assert_eq!(
            trace_file_line(&t, &[]),
            format!("{} file_bytes=0 hex=", trace_line(&t))
        );
    }
}
