// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE TIMELINE AS A VIEW OF THE SESSION LOG.

use std::sync::Arc;

use aterm_slog::{Fields, Key, Kind, Log, Node, Op, Rec, Val};

use crate::session_timeline::{TimelineEvent, clamp_payload};

/// The session log's row rule (`aterm_slog::RowRule`): how many rows [`rows_of`] prints of `r`.
pub(crate) fn row_span(r: &Rec) -> u8 {
    u8::try_from(rows_of(r).len()).unwrap_or(u8::MAX)
}

/// A warm spare adopted into a tab (`crate::spare`): `tier= age_ms= rc_ms=`.
pub(crate) const SPARE_ADOPTED: &str = "spare adopted";

/// THE ROWS THE LOG HOLDS AS THEIR ROW ALONE: the timeline kinds whose fact no other mark names, each a `row` mark.
pub(crate) const ROW_ONLY: [&str; 15] = [
    crate::sys_host::JOB_CHANGE,
    crate::occupancy_wakes::WAKE_READY,
    SPARE_ADOPTED,
    crate::claude_footer::REGISTRY_READ,
    "holder-edge",
    "holder-image",
    "holder-named",
    "holder-claim",
    "holder-adopt",
    "holder-retire",
    "card-seen",
    "card-row",
    "card-decorated",
    "card-armed",
    "card-bare",
];

/// Every timeline kind of plan section 0.1: what a test's hand-written `row` mark may name beside [`ROW_ONLY`].
#[cfg(test)]
pub(crate) const SECTION_0_1: [&str; 25] = [
    "spawned",
    "closing",
    "state-change",
    crate::session_timeline::TITLE_CHANGE,
    crate::session_timeline::CWD_CHANGE,
    crate::session_timeline::META_CHANGE,
    "agent-change",
    "task-change",
    crate::session_status::STATUS_CHANGE,
    crate::session_timeline::HUMAN,
    "render",
    "modes-restored",
    "in-doubt",
    "handoff",
    crate::session_timeline::HARNESS_ACT,
    crate::session_timeline::PROGRAM_STATE,
    crate::harness_policy_verb::HARNESS_POLICY_EVENT,
    "inbox",
    "inbox-seen",
    "post",
    "post-landed",
    "hold",
    "fetch",
    "topic",
    "hold-advisory",
];

/// A kind as a `row` mark's `row=` token: a space a `-` (the token's
/// charset holds none).
pub(crate) fn row_token(kind: &str) -> String {
    kind.replace(' ', "-")
}

/// Whether `token` spells `kind` ([`row_token`]), with no allocation.
fn spells(token: &str, kind: &str) -> bool {
    token.len() == kind.len()
        && token
            .bytes()
            .zip(kind.bytes())
            .all(|(t, k)| t == k || (k == b' ' && t == b'-'))
}

/// The kind a `row` mark's token names, if it names one.
fn row_kind(token: &str) -> Option<&'static str> {
    let known = ROW_ONLY.iter();
    #[cfg(test)]
    let known = known.chain(SECTION_0_1.iter());
    known.copied().find(|k| spells(token, k))
}

/// What a reader past a row id is shown.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TimelineView {
    /// The row ids past the reader's the log issued and no longer holds.
    pub(crate) lost: u64,
    /// The rows past the reader's id, oldest first.
    pub(crate) rows: Vec<TimelineEvent>,
}

/// THE VIEW: the log's rows of this epoch, oldest first, as the ring's events.
#[cfg(test)]
pub(crate) fn timeline_rows(log: &Log) -> Vec<TimelineEvent> {
    timeline_rows_since(log, None).rows
}

/// THE VIEW past row id `after`, and the holes it crosses on its way to the log's head.
pub(crate) fn timeline_rows_since(log: &Log, after: Option<u64>) -> TimelineView {
    let copy = ViewCopy::of(log, after, 0);
    TimelineView {
        lost: copy.lost,
        rows: copy.rows(),
    }
}

/// THE VIEW'S RECORDS past a reader's id, as a reader takes them under the
/// timeline's lock: each row record's first id, its process-clock reading
/// and the record itself (an `Arc` clone, no text formatted), newest
/// first, and the holes counted ([`TimelineView::lost`]). Formatted after
/// the lock is released ([`ViewCopy::rows`]).
#[derive(Clone, Debug, Default)]
pub(crate) struct ViewCopy {
    /// As [`TimelineView::lost`].
    pub(crate) lost: u64,
    /// The reader's id as read (0: from the first row).
    from: u64,
    /// At most this many of the newest rows (0: every one).
    keep: usize,
    /// `(first id, mono, record)`, oldest first.
    recs: Vec<(u64, u64, Arc<Rec>)>,
}

impl ViewCopy {
    /// The rows of `log` past `after` (the newest `keep` of them; 0: all),
    /// walked from the newest down to the reader's id only: a read costs
    /// the rows it is shown, never the log's whole ring.
    pub(crate) fn of(log: &Log, after: Option<u64>, keep: usize) -> ViewCopy {
        let head = log.row_head();
        let after = after.filter(|&a| a <= head);
        let from = after.unwrap_or(0);
        // The ids this log issued in `(lo, head]`, as `rows_lost_between`
        // counts them, and those it holds, counted on the same walk.
        let lo = from.max(log.row_first().saturating_sub(1));
        let (mut held, mut taken) = (0u64, 0usize);
        let mut recs = Vec::new();
        for row in log.rows().rev() {
            let last = row.id + u64::from(row.span) - 1;
            if last <= from {
                break;
            }
            #[cfg(test)]
            crate::work_counts::retained_record_touched();
            let (a, b) = (row.id.max(lo + 1), last.min(head));
            if b >= a {
                held += b - a + 1;
            }
            if keep == 0 || taken < keep {
                taken += usize::try_from(last - row.id.max(from + 1) + 1).unwrap_or(usize::MAX);
                recs.push((row.id, row.mono, Arc::clone(row.rec)));
            }
        }
        recs.reverse();
        let jumped = match after {
            Some(a) if log.rows_issued() > 0 => log.row_first().saturating_sub(1).saturating_sub(a),
            _ => 0,
        };
        ViewCopy {
            lost: jumped.saturating_add(head.saturating_sub(lo).saturating_sub(held)),
            from,
            keep,
            recs,
        }
    }

    /// The rows, oldest first, formatted by the grammar ([`rows_of`]) and
    /// cut by the timeline payload rule ([`clamp_payload`]).
    pub(crate) fn rows(&self) -> Vec<TimelineEvent> {
        let mut out = Vec::new();
        for (first, mono, rec) in &self.recs {
            for (id, (kind, payload)) in (*first..).zip(rows_of(rec)) {
                if id > self.from {
                    out.push(TimelineEvent {
                        id,
                        t_ms: *mono,
                        kind,
                        payload: clamp_payload(payload),
                    });
                }
            }
        }
        if self.keep > 0 && out.len() > self.keep {
            out.drain(..out.len() - self.keep);
        }
        out
    }
}

/// `text` without a trailing `%` escape its field's cap cut (`%` or `%h`):
/// a row's text is its writer's pct-encoded payload, held to the field's
/// encoded length ([`aterm_slog::Key::RowText`]), which knows no `%XX`, so
/// the row keeps [`clamp_payload`]'s rule that no escape is bisected.
fn whole_escapes(text: &str) -> &str {
    let b = text.as_bytes();
    match b {
        [.., b'%'] => &text[..b.len() - 1],
        [.., b'%', h] if h.is_ascii_hexdigit() => &text[..b.len() - 2],
        _ => text,
    }
}

/// The view's rows newest first, as `(kind, process-clock ms)`: the tab
/// chrome's tail (`session_chrome::timeline_tail`), which stops at the rows
/// it keeps, so the walk goes no further than that.
pub(crate) fn newest_first(log: &Log) -> impl Iterator<Item = (&'static str, u64)> + '_ {
    log.rows().rev().flat_map(|row| {
        rows_of(row.rec)
            .into_iter()
            .rev()
            .map(move |(kind, _)| (kind, row.mono))
    })
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

/// A task mark's ` fact=<word|-> fact_rev=<n>` (`task_record::TaskRecord::push_fact`): nothing
/// before the fact word's first edit, and `-` for a word that does not read back as a closed
/// [`aterm_agent::supervise::occupancy::FactWord`] (cleared, or a rule this build does not know).
fn task_fact(f: &Fields) -> String {
    let Some(rev) = f.get_u(Key::FactRev).filter(|rev| *rev > 0) else {
        return String::new();
    };
    let word = match (f.get_word(Key::FactRule), f.get_u(Key::FactN)) {
        (Some(rule), Some(n)) => {
            aterm_agent::supervise::occupancy::FactWord::parse(&format!("{rule}#{n}"))
        }
        _ => None,
    };
    let word = word.map_or_else(|| "-".to_string(), |w| w.word());
    format!(" fact={word} fact_rev={rev}")
}

/// THE GRAMMAR.
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
            Op::Open => return row("spawned", format!("state={}", text(&r.f, Key::State))),
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
        Kind::Human if f.get_word(Key::Why) == Some("mail") => return None,
        Kind::Human => (crate::session_timeline::HUMAN, String::new()),
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
        // Only a self-claim's act (`SessionLog::self_claim_act`, the one writer of
        // `harness_act=`) is a row; the harness's own `mark` act claims are none.
        Kind::Act if r.src == aterm_slog::Src::Mark && f.get(Key::HarnessAct).is_some() => (
            crate::session_timeline::HARNESS_ACT,
            format!(
                "act={} holder={}",
                raw(f, Key::HarnessAct),
                raw(f, Key::Holder)
            ),
        ),
        Kind::Task => (
            "task-change",
            format!(
                "rev={} state={} attention={}{}{}{}",
                text(f, Key::Rev),
                text(f, Key::State),
                text(f, Key::Attention),
                f.get_word(Key::Why)
                    .map(|w| format!(" why={w}"))
                    .unwrap_or_default(),
                match f.get(Key::Code) {
                    Some(Val::I(c)) => format!(" exit={c}"),
                    _ => String::new(),
                },
                task_fact(f)
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
        // A row the log holds as its row alone; a token no kind spells is no row.
        Kind::Row => (
            row_kind(f.get_str(Key::RowKind)?)?,
            whole_escapes(f.get_str(Key::RowText).unwrap_or("")).to_string(),
        ),
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
