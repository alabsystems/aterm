// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE TIMELINE AS A VIEW OF THE SESSION LOG:
//! the rows a session's `timeline` prints, read off its log's own
//! records instead of a ring of their own.
//!
//! - **The grammar** ([`rows_of`]) is the ring's: each record a row prints
//!   becomes that row's `(kind, payload)`, the whole payload, and a record
//!   no row prints becomes none. It is the session log's row rule too
//!   ([`row_span`], installed by `SessionLog` on every log it holds,
//!   `aterm_slog::Log::keep_rows`): a record the grammar prints is flagged
//!   as it is appended and takes the next dense row ids, so no writer can
//!   write a row the view does not hold.
//! - **The view** ([`timeline_rows`]) is this epoch's flagged records,
//!   oldest first, as the ring's events: id, the record's process-clock
//!   reading (6F-1), kind and payload (cut by the ring's own 256-byte rule).
//!   A carried record is no row (the ring was never carried either).
//! - **The holes** ([`timeline_rows_since`]): a row id the log issued and
//!   no longer holds (an eviction past the row floor, the program share) is
//!   counted, so a reader past it is told a `GAP`, as the ring's digest
//!   tells its drop-oldest loss; so is an update's jump past a
//!   predecessor's id, as the digest tells it. A reader's id above every id
//!   the log passed (a crashed window's, U13) reads from the first row, as
//!   the ring's `since` answers it.
//!
//! Each kind of plan section 0.1 the view prints equals the ring's rows by
//! its rule (`session_log_overlap_tests`): row for record, or, for
//! `program-state`, the log's retention rule (U39). The ids equal the
//! ring's while every ring row is a record's: a ring-only row (a dev build's
//! holder and card rows, `job-change`, `spare adopted`) or a program move
//! the log coalesced moves the ring's count past the view's.

use aterm_slog::{Fields, Key, Kind, Node, Op, Rec, Val};

// THE READERS' HALF (`TimelineView`, `timeline_rows`, `timeline_rows_since`)
// is test-only until 6G-3 moves the ring's readers onto it: an
// `expect(dead_code)` on a reader that calls another is unfulfilled under
// the cross cells' compiler, which counts the caller's expectation as a use
// (`verify cells`, the review of 6G-2).
#[cfg(test)]
use crate::session_timeline::{TimelineEvent, clamp_payload};
#[cfg(test)]
use aterm_slog::Log;

/// The session log's row rule (`aterm_slog::RowRule`): how many rows
/// [`rows_of`] prints of `r`.
pub(crate) fn row_span(r: &Rec) -> u8 {
    u8::try_from(rows_of(r).len()).unwrap_or(u8::MAX)
}

/// What a reader past a row id is shown: the rows after it, oldest first,
/// and how many row ids between it and the log's head the log no longer
/// holds.
#[cfg(test)]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TimelineView {
    /// The row ids past the reader's the log issued and no longer holds.
    pub(crate) lost: u64,
    /// The rows past the reader's id, oldest first.
    pub(crate) rows: Vec<TimelineEvent>,
}

/// THE VIEW: the log's rows of this epoch, oldest first, as the ring's
/// events.
#[cfg(test)]
pub(crate) fn timeline_rows(log: &Log) -> Vec<TimelineEvent> {
    timeline_rows_since(log, None).rows
}

/// THE VIEW past row id `after` (`None`: from the first), and the holes it
/// crosses on its way to the log's head ([`aterm_slog::Log::rows_lost_between`]).
/// A reader's id is read as the ring reads it, whichever process issued it:
///
/// - **Above every id this log passed** ([`aterm_slog::Log::row_head`]): no
///   id of this count but another process's, the window a keeper's crash
///   recovery restarted at 1 from (U13), so the reader reads from the first
///   row, as `SessionTimeline::since` answers it, with this log's own holes.
/// - **Below the first id this log issued** ([`aterm_slog::Log::row_first`],
///   once it issued one): the predecessor's, across an update; the ids
///   between it and this log's first are told lost too, as the ring's
///   digest tells the update's `HANDOFF_ID_RESERVE` jump
///   (`GAP <sid> events-dropped=`), so a watcher re-reads what it follows.
#[cfg(test)]
pub(crate) fn timeline_rows_since(log: &Log, after: Option<u64>) -> TimelineView {
    let head = log.row_head();
    let after = after.filter(|&a| a <= head);
    let from = after.unwrap_or(0);
    let mut rows = Vec::new();
    for row in log.rows() {
        for (id, (kind, payload)) in (row.id..).zip(rows_of(row.rec)) {
            if id > from {
                rows.push(TimelineEvent {
                    id,
                    t_ms: row.mono,
                    kind,
                    payload: clamp_payload(payload),
                });
            }
        }
    }
    let jumped = match after {
        Some(a) if log.rows_issued() > 0 => log.row_first().saturating_sub(1).saturating_sub(a),
        _ => 0,
    };
    TimelineView {
        lost: jumped.saturating_add(log.rows_lost_between(from, head)),
        rows,
    }
}

/// A field of a record, as text (`-` when it is not set).
fn text(f: &Fields, key: Key) -> String {
    match f.get(key) {
        Some(Val::U(n)) => n.to_string(),
        Some(Val::I(n)) => n.to_string(),
        Some(_) => f.get_str(key).unwrap_or("-").to_string(),
        None => "-".to_string(),
    }
}

/// A text field as it was written (`-` when it is not set).
fn raw(f: &Fields, key: Key) -> &str {
    f.get_str(key).unwrap_or("-")
}

/// THE GRAMMAR: one record as the timeline rows it stands for, oldest
/// first, each `(kind, payload)` whole (a reader cuts it by the ring's own
/// rule, [`crate::session_timeline::clamp_payload`]): none, one, or the
/// session close's two (`closing` and the last `state-change`, in that
/// order, written in one hold as the ring's are; the first only for a close
/// that names its reason, as the ring's `closing` row is).
pub(crate) fn rows_of(r: &Rec) -> Vec<(&'static str, String)> {
    let row = |k: &'static str, p: String| vec![(k, p)];
    if r.node == Node::SESSION {
        match r.op {
            Op::Close => {
                let mut rows = Vec::new();
                match r.f.get_word(Key::Out) {
                    Some("unknown") | None => {}
                    Some(out) => rows.extend(row(
                        "closing",
                        format!(
                            "reason={out} by={}",
                            crate::control::pct_encode(raw(&r.f, Key::Closer))
                        ),
                    )),
                }
                rows.extend(row("state-change", "state=closed".to_string()));
                return rows;
            }
            // The registration: the open (a carried session's is its
            // `state carried=1` mark, below).
            Op::Open => return row("spawned", format!("state={}", text(&r.f, Key::State))),
            // An uncarried handoff's loss, or a keeper's crash adoption's.
            Op::Gap if matches!(r.f.get_word(Key::Why), Some("handoff" | "crash")) => {
                return row("handoff", handoff_row(&r.f));
            }
            _ => {}
        }
    }
    project_mark(r).into_iter().collect()
}

/// A `handoff` row as its record says it: the ring is never carried, so
/// `carried=0` whatever the log carried, and `build=` its `from_build=`.
fn handoff_row(f: &Fields) -> String {
    format!("carried=0 from_build={}", text(f, Key::Build))
}

/// A mark as the timeline row it stands for, if it is one's.
fn project_mark(r: &Rec) -> Option<(&'static str, String)> {
    if r.op != Op::Mark {
        return None;
    }
    let f = &r.f;
    let row = match r.k {
        Kind::Agent => (
            "agent-change",
            format!(
                "{} rev={} gen={} fp={}",
                f.get_word(Key::Word).unwrap_or("unknown"),
                text(f, Key::Rev),
                text(f, Key::Gen),
                text(f, Key::Fp)
            ),
        ),
        Kind::Status => {
            let code = text(f, Key::Code);
            let (outcome, exit_code, signal) = match f.get_word(Key::Out) {
                None => ("none", "-".to_string(), "-".to_string()),
                Some("ok") => ("success", "-".to_string(), "-".to_string()),
                Some("fail") => ("failure", code, "-".to_string()),
                Some("signal") => ("signal", "-".to_string(), code),
                Some(other) => (other, code, "-".to_string()),
            };
            (
                crate::session_status::STATUS_CHANGE,
                format!(
                    "phase={} outcome={outcome} exit_code={exit_code} signal={signal} confidence={}",
                    text(f, Key::Phase),
                    r.conf.word()
                ),
            )
        }
        // The burst marks: the mail latch's own mark, no
        // burst's, says `why=mail` and is no row.
        Kind::Human if f.get_word(Key::Why) == Some("mail") => return None,
        Kind::Human => ("human", String::new()),
        // Phase 6: the four rows whose rule differed, as
        // the view prints them. The title and the directory as their marks
        // hold them (a cleared directory `-`).
        Kind::Title => (
            crate::session_timeline::TITLE_CHANGE,
            format!(
                "title={}",
                crate::control::pct_encode(f.get_str(Key::Title).unwrap_or(""))
            ),
        ),
        Kind::Cwd => (
            crate::session_timeline::CWD_CHANGE,
            format!(
                "cwd={}",
                f.get_str(Key::Cwd)
                    .map_or_else(|| "-".to_string(), crate::control::pct_encode)
            ),
        ),
        // A meta field's value, or the attention text beside its `fp=` (the
        // content tier), `-` for none; the Autopilot pair's timer. A
        // successor's restore (`carried=1`) is a row as any write is.
        Kind::Meta => (
            crate::session_timeline::META_CHANGE,
            format!(
                "field={} value={}{}",
                text(f, Key::Field),
                f.get_str(Key::Value)
                    .or_else(|| f.get_str(Key::AttnText))
                    .map_or_else(|| "-".to_string(), crate::control::pct_encode),
                match f.get(Key::LandAt) {
                    Some(Val::I(s)) => format!(" land_at={s}"),
                    _ => String::new(),
                }
            ),
        ),
        // A self-claim's act, whole (`harness_act=`, the review of 6F-7),
        // and its holder; a relaunch's act (`src=aterm`) is no row's.
        Kind::Act if r.src == aterm_slog::Src::Mark => (
            crate::session_timeline::HARNESS_ACT,
            format!(
                "act={} holder={}",
                raw(f, Key::HarnessAct),
                raw(f, Key::Holder)
            ),
        ),
        // `exit=` is the log's `code=`, as a command's is (5C-1).
        Kind::Task => (
            "task-change",
            format!(
                "rev={} state={} attention={}{}{}",
                text(f, Key::Rev),
                text(f, Key::State),
                text(f, Key::Attention),
                f.get_word(Key::Why)
                    .map(|w| format!(" why={w}"))
                    .unwrap_or_default(),
                match f.get(Key::Code) {
                    Some(Val::I(c)) => format!(" exit={c}"),
                    _ => String::new(),
                }
            ),
        ),
        Kind::Mail if f.get_word(Key::Dir) == Some("in") => (
            "inbox",
            format!(
                "{} from={} kind={} off={}",
                text(f, Key::Inbox),
                text(f, Key::From),
                text(f, Key::MailKind),
                text(f, Key::Off)
            ),
        ),
        // Phase 6 (6F-5): the ring-only facts, as marks; a carried
        // session's registration is its `spawned` row (6F-7).
        Kind::State if f.get_u(Key::Carried) == Some(1) => {
            ("spawned", format!("state={}", text(f, Key::State)))
        }
        Kind::State => ("state-change", format!("state={}", text(f, Key::State))),
        Kind::Policy => (
            crate::harness_policy_verb::HARNESS_POLICY_EVENT,
            format!("gen={}", text(f, Key::Gen)),
        ),
        Kind::Advisory => (
            "hold-advisory",
            format!(
                "1 reason={} origin={}",
                raw(f, Key::Reason),
                text(f, Key::Origin)
            ),
        ),
        Kind::MailFetch => ("fetch", format!("off={}", text(f, Key::Off))),
        // Phase 6 (6F-6): the fabric rows, whole. A broadcast's `to=` is
        // its topic's fingerprint on the record, the name beside it.
        Kind::Mail if f.get_word(Key::Dir) == Some("out") => {
            let to = match (raw(f, Key::To).strip_prefix("say:"), f.get(Key::TopicName)) {
                (Some(_), Some(_)) => format!("say:{}", raw(f, Key::TopicName)),
                _ => raw(f, Key::To).to_string(),
            };
            let opt = |k: Key, name: &str| match f.get(k) {
                Some(_) => format!(" {name}={}", text(f, k)),
                None => String::new(),
            };
            (
                "post",
                format!(
                    "{} to={} kind={}{}{}{}{}",
                    text(f, Key::Inbox),
                    aterm_control::wire::pct_encode(&to),
                    text(f, Key::MailKind),
                    opt(Key::Re, "re"),
                    opt(Key::Dl, "dl"),
                    opt(Key::Via, "via"),
                    opt(Key::Idem, "key"),
                ),
            )
        }
        Kind::MailLanded => (
            "post-landed",
            format!(
                "{} off={}{}",
                text(f, Key::Inbox),
                text(f, Key::Off),
                f.get_word(Key::Why)
                    .map(|w| format!(" reason={w}"))
                    .unwrap_or_default()
            ),
        ),
        Kind::MailSeen => (
            "inbox-seen",
            format!(
                "{} off={} floor={} seen={}{}",
                text(f, Key::Inbox),
                text(f, Key::Off),
                text(f, Key::Floor),
                text(f, Key::Seen),
                match f.get_word(Key::Word) {
                    Some(v) => format!(
                        " verdict={v} kind={} from={}",
                        text(f, Key::MailKind),
                        raw(f, Key::From)
                    ),
                    None => String::new(),
                }
            ),
        ),
        Kind::Hold => (
            "hold",
            format!(
                "{} reason={} origin={} hold_origin={}{}",
                u8::from(f.get_word(Key::State) == Some("on")),
                raw(f, Key::Reason),
                text(f, Key::Origin),
                text(f, Key::HoldOrigin),
                if f.get_u(Key::Carried) == Some(1) {
                    " carried=1"
                } else {
                    ""
                }
            ),
        ),
        Kind::Topic => (
            "topic",
            match f.get_word(Key::Change) {
                Some("add") => format!(
                    "add {} since={}",
                    raw(f, Key::TopicName),
                    raw(f, Key::Since)
                ),
                _ => format!("drop {}", raw(f, Key::TopicName)),
            },
        ),
        // Phase 6 (6F-7): the session rows, whole.
        Kind::Handoff => ("handoff", handoff_row(f)),
        Kind::Render => ("render", render_row(f)),
        Kind::Modes => {
            let reverted = |none: &'static str| f.get_str(Key::Reverted).unwrap_or(none);
            (
                "modes-restored",
                match f.get_word(Key::Source) {
                    Some(source) => format!(
                        "reason=manual source={source} reverted={} bytes={}",
                        reverted("-"),
                        text(f, Key::Bytes)
                    ),
                    None => format!(
                        "from={} to={} program={} reverted={} bytes={}",
                        text(f, Key::FromPg),
                        text(f, Key::ToPg),
                        f.get_str(Key::Prog)
                            .map_or_else(|| "-".to_string(), crate::control::pct_encode),
                        reverted(""),
                        text(f, Key::Bytes)
                    ),
                },
            )
        }
        Kind::InDoubt => (
            "in-doubt",
            format!(
                "producer={} seq={} reply={}",
                text(f, Key::Producer),
                text(f, Key::KeySeq),
                crate::control::pct_encode(f.get_str(Key::Reply).unwrap_or(""))
            ),
        ),
        // The program's own report: kept under the log's
        // own retention rule (64 a run, a flap past the run's bucket
        // coalesced into the next mark), so its rows are the ring's by that
        // rule, not row for record. A report that went away (`none`) is the
        // ring's `-`.
        Kind::Program => (
            crate::session_timeline::PROGRAM_STATE,
            format!(
                "state={} kind={}",
                match f.get_word(Key::State) {
                    Some("none") | None => "-",
                    Some(w) => w,
                },
                f.get_word(Key::Ask).unwrap_or("-")
            ),
        ),
        _ => return None,
    };
    Some(row)
}

/// A `render` mark as the resize ledger's row: a run's verdict or an
/// epoch's (`resize_epoch=` set), into `desync-risk` or `healed`.
fn render_row(f: &Fields) -> String {
    let signed = |k: Key| match f.get(k) {
        Some(Val::I(n)) => format!("{n:+}"),
        _ => "-".to_string(),
    };
    let state = text(f, Key::State);
    match (f.get(Key::ResizeEpoch).is_some(), state.as_str()) {
        (false, "desync-risk") => format!(
            "desync-risk run={} displaced={} net={} at={}",
            text(f, Key::Run),
            signed(Key::Displaced),
            text(f, Key::Net),
            text(f, Key::At)
        ),
        (false, _) => format!(
            "{state} run={} after_ms={}",
            text(f, Key::Run),
            text(f, Key::AfterMs)
        ),
        (true, "desync-risk") => format!(
            "desync-risk epoch={} displaced={} pushed={} cut={} drew={} class={} at={}",
            text(f, Key::ResizeEpoch),
            signed(Key::Displaced),
            text(f, Key::Pushed),
            text(f, Key::Cut),
            text(f, Key::Drew),
            text(f, Key::Class),
            text(f, Key::At)
        ),
        (true, _) => format!(
            "{state} epoch={} after_ms={} by={}",
            text(f, Key::ResizeEpoch),
            text(f, Key::AfterMs),
            text(f, Key::HealedBy)
        ),
    }
}
