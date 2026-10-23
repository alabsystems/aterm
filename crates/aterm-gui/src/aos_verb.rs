// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Control-thread parser, formatter, and main-thread dispatch for the `aos` verb.

use aterm_aos::contract::Verdict;
use aterm_aos::evidence::EvidenceGrade;
use aterm_aos::publish::Head;
use aterm_effects::aos_momentum::{MomentumAosStatus, MomentumCode};

use crate::aos_host::{Repaired, Resumed, TraceSummary};

/// Whether this build hosts ([`cmd`]): a debug or test build only.
const AOS_ENABLED: bool = cfg!(any(test, debug_assertions));

/// The usage line every junk form answers with.
const USAGE: &str =
    "usage: aos [status|attach [code=<name>]|pause|repair [code=<name>]|resume|detach|trace]";

/// The refusal every form gets where this build does not host.
pub(crate) const NEEDS_DEBUG: &str = "aos needs a debug build";

/// What the verb asks of the front window's momentum glow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Asked {
    /// Read it (`status`, or bare).
    Status,
    /// Start hosting it under this code.
    Attach(MomentumCode),
    /// Pause it at this step boundary.
    Pause,
    /// Publish this code over it (pausing first if it runs).
    Repair(MomentumCode),
    /// Reject the pending update: the old code resumes.
    Resume,
    /// Stop hosting it.
    Detach,
    /// Summarize the exported trace.
    Trace,
}

/// The verb's arguments, refused where this build does not host.
pub(crate) fn parse(rest: &str, enabled: bool) -> Result<Asked, String> {
    if !enabled {
        return Err(NEEDS_DEBUG.to_string());
    }
    let words: Vec<&str> = rest.split_whitespace().collect();
    Ok(match words.as_slice() {
        [] | ["status"] => Asked::Status,
        ["attach"] => Asked::Attach(MomentumCode::Shipped),
        ["attach", code] => Asked::Attach(code_of(code)?),
        ["pause"] => Asked::Pause,
        ["repair"] => Asked::Repair(MomentumCode::Shipped),
        ["repair", code] => Asked::Repair(code_of(code)?),
        ["resume"] => Asked::Resume,
        ["detach"] => Asked::Detach,
        ["trace"] => Asked::Trace,
        _ => return Err(USAGE.to_string()),
    })
}

/// A `code=<name>` word: a code this build carries, by its short or artifact name.
fn code_of(word: &str) -> Result<MomentumCode, String> {
    let Some(name) = word.strip_prefix("code=") else {
        return Err(USAGE.to_string());
    };
    MomentumCode::from_name(name).ok_or_else(|| {
        let names: Vec<&str> = MomentumCode::ALL.iter().map(|c| c.name()).collect();
        format!(
            "{USAGE} (no code `{name}`; this build carries {})",
            names.join(", ")
        )
    })
}

/// `pass`, or `fail:<law>@<cursor>` (the detail stays out of a `k=v` line).
fn verdict_word(v: &Verdict) -> String {
    match v {
        Verdict::Pass => "pass".to_string(),
        Verdict::Fail { cursor, law, .. } => format!("fail:{law}@{cursor}"),
    }
}

/// The grade's word.
const fn grade_word(g: EvidenceGrade) -> &'static str {
    match g {
        EvidenceGrade::Tested => "tested",
    }
}

/// A signed epoch offset, or `none`.
fn offset_word(v: Option<i128>) -> String {
    v.map_or_else(|| "none".to_string(), |t| t.to_string())
}

/// A text value as one `k=v` token: spaces become `_`, nothing at all is `none`.
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
         range={from}..{to} log_epoch={} delivered={}",
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
        r.commit.delivered.len()
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

/// The verb's control-thread half: the ask on the main thread (`Wake::Aos`).
pub(crate) fn cmd(
    proxy: &aterm_window::event_loop::EventLoopProxy<crate::Wake>,
    rest: &str,
) -> String {
    let asked = match parse(rest, AOS_ENABLED) {
        Ok(asked) => asked,
        Err(refusal) => return format!("ERR {refusal}\n"),
    };
    match crate::control::control_media::call_main(proxy, |reply| crate::Wake::Aos { asked, reply })
    {
        Ok(Ok(line)) => format!("OK {line}\n"),
        Ok(Err(e)) => format!("ERR {e}\n"),
        Err(e) => format!("ERR {e}\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every form parses to its ask, `code=` names a code this build carries by either
    /// name, and junk is the usage line; a release build refuses every form, the read
    /// included.
    #[test]
    fn the_aos_verb_parses_its_forms_and_a_release_build_refuses_all_of_them() {
        let demo = MomentumCode::DemoStrandedSettle;
        for (rest, want) in [
            ("", Asked::Status),
            ("status", Asked::Status),
            ("  status ", Asked::Status),
            ("attach", Asked::Attach(MomentumCode::Shipped)),
            ("attach code=shipped", Asked::Attach(MomentumCode::Shipped)),
            ("attach code=demo-stranded-settle", Asked::Attach(demo)),
            (
                "attach code=momentum-glow.demo-stranded-settle",
                Asked::Attach(demo),
            ),
            ("pause", Asked::Pause),
            ("repair", Asked::Repair(MomentumCode::Shipped)),
            ("repair code=shipped", Asked::Repair(MomentumCode::Shipped)),
            ("repair code=demo-stranded-settle", Asked::Repair(demo)),
            ("resume", Asked::Resume),
            ("detach", Asked::Detach),
            ("trace", Asked::Trace),
        ] {
            assert_eq!(parse(rest, true), Ok(want), "{rest:?}");
            assert_eq!(
                parse(rest, false),
                Err(NEEDS_DEBUG.to_string()),
                "a release build refuses {rest:?}"
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
        ] {
            let refusal = parse(junk, true).expect_err(junk);
            assert!(refusal.starts_with(USAGE), "{junk:?}: {refusal}");
        }
        assert_eq!(
            parse("repair code=nope", true),
            Err(format!(
                "{USAGE} (no code `nope`; this build carries shipped, demo-stranded-settle)"
            ))
        );
    }

    /// The unattached status line is the window and the glow's three predicates only;
    /// a text value never splits a `k=v` token.
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

    /// The trace line ends with the seal and the per-kind counts, after the break's reason
    /// when there is one.
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
    }
}
