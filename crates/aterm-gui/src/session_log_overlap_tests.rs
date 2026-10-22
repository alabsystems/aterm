// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHERE THE OLD STORES AND THE LOG OVERLAP, THEY AGREE (plan 5B-6; rulings
//! T17, T19, T21, T39). Phase 5 keeps three stores beside the session log
//! (T21): the timeline's RAM ring, the roster journal `exits` reads, and
//! presence's story. Each is checked here against the log where the two
//! hold the same fact, and every place they do not is a named difference
//! a test drives, so a difference that a later step closes turns this red.
//!
//! * THE TIMELINE. [`overlap_rows`] projects the log's records of the exact
//!   pairs ([`EXACT`]: `agent`, `status`, `mail dir=in` and `task`, T19's
//!   set less `human`; since phase 6 the ring-only facts, 6F-5, the fabric
//!   rows, 6F-6, the session rows, 6F-7, and since the review of 6F-7
//!   `harness-act`, every act a record with the act whole) into the ring's
//!   `(kind, payload)`, cut by the ring's own 256-byte rule
//!   ([`crate::session_timeline::clamp_payload`]), and over a scripted
//!   session, and each kind through its own writer ([`DRIVEN`]), at the
//!   longest values its producer admits too, the two are equal, row for
//!   record, in order (ids and clocks aside): the WHOLE payload, so an
//!   exact pair has no field the comparison does not see. Since 6F-8
//!   (ruling U13) the four rows whose rule differed are exact pairs too, as
//!   the view's grammar rules them: `title-change` from the content-tier
//!   title marks (their cadence), `cwd-change` from the `cwd` marks (a
//!   successor does not restate the directory it carried), `meta-change`
//!   one mark a row (the attention text beside its `fp=` at the content
//!   tier, RAM only; a successor's restore `carried=1`; the Autopilot
//!   timer's `land_at=`), and `human` the burst marks, the mail latch's own
//!   mark saying `why=` mail, which the view filters on (the fold reads no
//!   `why=`). Every other kind of plan §0.1 is a row of
//!   [`KNOWN_DIFFERENCES`]: a record the log keeps under its own retention
//!   rule (`program-state` since 6F-4, U39), driven through the row's own
//!   writer.
//! * EXITS. For every reason and actor, the roster's `Exited` row and the
//!   closed-session index's session close agree: the reason is `out=`, the
//!   exit code `code=`, the actor `by=` under 3A's R1 (a sid no one minted
//!   is `-`). A close that keeps no entry (`keep=None`, the app's last) has
//!   none, pinned.
//! * THE STORY (T17). Each point `ctl story` told has exactly one `story`
//!   mark by its teller within the log's epoch. Presence's story is per
//!   process and the log is carried, so across an update the carried marks
//!   stand before the epoch with no point beside them: pinned.
//!
//! NEGATIVE CONTROLS: a projection with `fp=` dropped fails, and one with
//! `why=` dropped shows a `human` row the ring never had; a retained
//! kind whose newest record is stale, or with more records than rows, fails;
//! an exit map
//! with `ctl-close` and `ui-close` swapped fails; a story mark written twice
//! fails; a named difference that is closed fails.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_messages::presence::StoryVerb;
use aterm_slog::{Actor, Log, Op, Rec, ShortId, Tier};

use super::super::tests::{hand_over, stamp};
use super::*;
use crate::session_store::{
    CloseAttribution, ExitActor, ExitReason, RosterChange, SessionHandle, SessionState, Store,
    new_store, test_handle,
};
use crate::session_timeline::SessionTimeline;
use crate::{App, WindowId};

// ---- the timeline -------------------------------------------------------------

/// The ring kinds the log equals row for record, the whole payload (T19's
/// exact pairs: as built at 5B-6, `task-change` since 5C-3 writes its mark;
/// `human` a superset from 5D-4 to 6F-8; `harness-act` since the review of
/// 6F-7, its act whole at `harness_act=` and an act with no word a record
/// too). Since phase 6 (6F-5) the ring-only facts
/// are marks and their rows exact pairs too: `state-change` (a `state` mark,
/// and the session close for `state=closed`), `harness-policy`,
/// `hold-advisory`, `fetch` and `topic`; since 6F-6 the lossy fabric rows:
/// `inbox-seen` (every one, `moved=0|1`), `post`, `post-landed` (`off=-`
/// too) and `hold`; since 6F-7 the lossy session rows: `spawned` (the
/// open's `state=`, or a carried session's `state carried=1` mark),
/// `closing` (the session close's `closer=`, ruling U12), `render` (every
/// transition, whole), `modes-restored` (every row, nothing reverted too),
/// `in-doubt` and `handoff` (`build=`; the ring is never carried, so its
/// `carried=0`); since 6F-8 (ruling U13) the four rows whose rule differed,
/// as the view's grammar: `title-change`, `cwd-change`, `meta-change` and
/// `human` (the burst marks; the mail latch's own, `why=mail`, is no row);
/// each checked through its own writer ([`DRIVEN`]), at its producer's
/// bounds too.
const EXACT: [&str; 24] = [
    "agent-change",
    crate::session_status::STATUS_CHANGE,
    "inbox",
    "task-change",
    "state-change",
    crate::harness_policy_verb::HARNESS_POLICY_EVENT,
    "hold-advisory",
    "fetch",
    "topic",
    "inbox-seen",
    "post",
    "post-landed",
    "hold",
    "spawned",
    "closing",
    "render",
    "modes-restored",
    "in-doubt",
    "handoff",
    crate::session_timeline::HARNESS_ACT,
    crate::session_timeline::TITLE_CHANGE,
    crate::session_timeline::CWD_CHANGE,
    crate::session_timeline::META_CHANGE,
    "human",
];

/// The exact pairs 5B-6's scripted session writes
/// ([`the_ring_and_the_log_agree_on_the_exact_pairs`]); the others are
/// [`DRIVEN`]'s, each through its own writer.
const SCRIPTED: [&str; 7] = [
    "agent-change",
    crate::session_status::STATUS_CHANGE,
    "inbox",
    "task-change",
    crate::session_timeline::HARNESS_ACT,
    crate::session_timeline::META_CHANGE,
    "human",
];

/// Every timeline kind of plan §0.1.
const SECTION_0_1: [&str; 25] = [
    "spawned",
    "closing",
    "state-change",
    "title-change",
    "cwd-change",
    "meta-change",
    "agent-change",
    "task-change",
    crate::session_status::STATUS_CHANGE,
    "human",
    "render",
    "modes-restored",
    "in-doubt",
    "handoff",
    "harness-act",
    "program-state",
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

/// One record as the ring rows it pairs with ([`project_whole`]), each
/// payload cut by the ring's own rule, 256 bytes never inside an escape
/// ([`crate::session_timeline::clamp_payload`]): a view prints the ring's
/// grammar, its bound too.
fn project(r: &Rec) -> Vec<(String, String)> {
    project_whole(r)
        .into_iter()
        .map(|(k, p)| (k, crate::session_timeline::clamp_payload(p).to_string()))
        .collect()
}

/// One record as the ring rows it pairs with: none, or the exact pairs'
/// (the session close is two rows, `closing` and the last `state-change`,
/// in that order, written in one hold as the ring's are; the first only
/// for a close that names its reason, as the ring's `closing` row is).
fn project_whole(r: &Rec) -> Vec<(String, String)> {
    let row = |k: &str, p: String| vec![(k.to_string(), p)];
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

/// A mark as the ring row it pairs with, if it is an exact pair's.
fn project_mark(r: &Rec) -> Option<(String, String)> {
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
        // The burst marks (6F-8, ruling U13): the mail latch's own mark, no
        // burst's, says `why=mail` and is no row.
        Kind::Human if f.get_word(Key::Why) == Some("mail") => return None,
        Kind::Human => ("human", String::new()),
        // Phase 6 (6F-8, ruling U13): the four rows whose rule differed, as
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
        _ => return None,
    };
    Some((row.0.to_string(), row.1))
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

/// The log's records of the exact pairs as the ring's `(kind, payload)`,
/// oldest first.
pub(crate) fn overlap_rows(log: &Log) -> Vec<(String, String)> {
    overlap_rows_without(log, None)
}

/// [`overlap_rows`] with `drop` taken out of every record first (the
/// negative control's lossy projection).
fn overlap_rows_without(log: &Log, drop: Option<Key>) -> Vec<(String, String)> {
    projected(log.window(), drop)
}

/// `recs` projected ([`project`]), `drop` taken out of every record first.
fn projected<'a>(
    recs: impl IntoIterator<Item = &'a Rec>,
    drop: Option<Key>,
) -> Vec<(String, String)> {
    recs.into_iter()
        .flat_map(|r| {
            let mut r = r.clone();
            if let Some(k) = drop {
                r.f.remove(k);
            }
            project(&r)
        })
        .collect()
}

/// `rows` of `kinds` alone.
fn only(rows: Vec<(String, String)>, kinds: &[&str]) -> Vec<(String, String)> {
    rows.into_iter()
        .filter(|(k, _)| kinds.contains(&k.as_str()))
        .collect()
}

/// The ring's rows of `kinds`, oldest first, as `(kind, payload)`: the
/// whole payload, nothing dropped.
fn ring_rows(tl: &SessionTimeline, kinds: &[&str]) -> Vec<(String, String)> {
    tl.since(None)
        .filter(|e| kinds.contains(&e.kind))
        .map(|e| (e.kind.to_string(), e.payload.to_string()))
        .collect()
}

/// Rows as the ring prints them, `(kind, payload)`, oldest first.
#[cfg(unix)]
pub(crate) type Rows = Vec<(String, String)>;

/// The log's view of the exact pairs and the ring's rows of them, oldest
/// first, for a test outside this module to hold equal (the T2 races' order,
/// the review of 6F-8): row for record, in order. Unix only, as the
/// attribution tests that call it are.
#[cfg(unix)]
pub(crate) fn exact_view_and_ring(tl: &SessionTimeline) -> (Rows, Rows) {
    (
        only(overlap_rows(tl.slog().log()), &EXACT),
        ring_rows(tl, &EXACT),
    )
}

/// THE EXACT PAIRS AGREE (T19): over one scripted session, through each
/// row's production writer — the status sweep's published transitions (a
/// failing command, then a prompt), the agent publication (each verdict,
/// one with no screen stamp), a person's burst (an exact pair again since
/// 6F-8), two bridge deliveries (and a redelivery, which
/// writes neither) and a worker's task moved through `task mark` and the
/// supervisor's attention (and a repeated mark, which writes neither; its
/// `meta-change` row's text the mark's `attn_text=`, since 6F-8) — the
/// log's records of the exact pairs, projected, are the ring's rows of those
/// kinds, in order. NEGATIVE CONTROL: the same projection with `fp=` dropped
/// differs, and with the attention's `attn_text=` dropped.
#[test]
fn the_ring_and_the_log_agree_on_the_exact_pairs() {
    let mut app = App::headless_for_test();
    let local = app.next_session_id;
    app.push_stub_tab(WindowId(0), crate::stub_session(local));
    let pooled = app.pool.get(local).expect("pooled");
    let (ctx, term) = (Arc::clone(&pooled.ctx), Arc::clone(&pooled.term));
    crate::term_lock(&term).process(
        b"\x1b]133;A\x1b\\\x1b]133;B\x1b\\false\r\n\x1b]133;C\x1b\\\
          \x1b]133;D;1\x1b\\\x1b]133;A\x1b\\\x1b]133;B\x1b\\",
    );
    let t0 = Instant::now();
    for i in 0..4 {
        app.observe_session_statuses(t0 + Duration::from_secs(i));
    }
    {
        let mut g = ctx.timeline.lock().unwrap();
        let claude = Some(aterm_phase::Program::Claude);
        g.publish_agent("busy", None, None, claude, stamp(1));
        g.publish_agent("prompt", Some("bash".into()), None, claude, stamp(2));
        g.publish_agent("question", None, None, claude, stamp(3));
        g.publish_agent("idle", None, None, claude, stamp(4));
    }
    let _ = crate::app_input::note_person(&ctx, crate::human_input::Gesture::Other);
    let _ = crate::app_input::note_person(&ctx, crate::human_input::Gesture::PlainEnter);
    let sid = ctx.self_id.as_str().to_string();
    let wake = |_: &aterm_session::SessionId| {};
    for line in [
        format!("{sid} off=7 from=h-andrew kind=task trust=human text=SENTINEL%20BODY"),
        format!("{sid} off=7 from=h-andrew kind=task trust=human text=SENTINEL%20BODY"),
        format!("{sid} off=8 from=s-peer@n-lab kind=ask trust=agent text=SENTINEL"),
    ] {
        let reply = crate::fabric::cmd_deliver_waking(&app.store, &line, &wake);
        assert!(reply.starts_with("OK "), "{reply}");
    }
    for args in [
        "working",
        "done why=marker",
        "done why=marker",
        "exited why=exit exit=2",
    ] {
        let reply = crate::task_record::cmd_task(
            &ctx,
            local,
            crate::control::Scope::Owner,
            &format!("mark {args}"),
        );
        assert!(reply.starts_with("OK task "), "{reply}");
    }
    let raised = crate::session_timeline::write_attention_owned(
        &ctx,
        crate::harness_host::ATTENTION_OWNER,
        crate::session_timeline::MetaEdit::Set("look here"),
        Actor::Harness,
    );
    assert_eq!(raised, Ok(true));
    {
        let mut g = ctx.timeline.lock().unwrap();
        for (act, by) in [
            ("approve-all@v1", Actor::Ctl),
            ("finished@v1", Actor::Ctl),
            ("usage-resume@v1", Actor::Harness),
        ] {
            let _ = g.note_harness_act(act, "codex-alab@1", by);
        }
    }
    crate::term_lock(&term).process(b"true\r\n\x1b]133;C\x1b\\\x1b]133;D;0\x1b\\");
    for i in 4..8 {
        app.observe_session_statuses(t0 + Duration::from_secs(i));
    }
    let g = ctx.timeline.lock().unwrap();
    let ring = ring_rows(&g, &EXACT);
    for kind in SCRIPTED {
        assert!(
            ring.iter().any(|(k, _)| k == kind),
            "the script wrote no {kind} row: {ring:?}"
        );
    }
    let log = only(overlap_rows(g.slog().log()), &EXACT);
    assert_eq!(log, ring, "the log's exact pairs against the ring's rows");
    // Every act is a record (the review of 6F-7): the one with no word
    // (`finished@v1`) too, `word=unknown`.
    let acts = |words: &[&str]| {
        g.slog()
            .log()
            .window()
            .filter(|r| r.k == Kind::Act && r.src == aterm_slog::Src::Mark)
            .filter(|r| words.contains(&r.f.get_word(Key::Word).unwrap_or("-")))
            .count()
    };
    assert_eq!(acts(&["approve", "continue"]), 2, "two worded acts");
    assert_eq!(acts(&["unknown"]), 1, "finished@v1 is a record too");
    // NEGATIVE CONTROLS: a lossy projection is caught, for the agent's
    // `fp=`, the task's `rev=`, the act's `harness_act=` and `holder=` and
    // the attention's text alike.
    for key in [
        Key::Fp,
        Key::Rev,
        Key::HarnessAct,
        Key::Holder,
        Key::AttnText,
    ] {
        assert_ne!(
            only(overlap_rows_without(g.slog().log(), Some(key)), &EXACT),
            ring,
            "a projection with {key:?} dropped agrees"
        );
    }
}

/// A person's burst, a delivered mail, then the same person typing on
/// inside that 30 s burst, twice (T39's latch), on session `local`.
fn drive_human_at(local: u64) -> (SessionHandle, Seen) {
    let (h, store) = rig(local);
    let hand = super::super::tests::hand_clock(&h.ctx.timeline);
    let seen = during(&h.ctx.timeline, || {
        let ctx = &h.ctx;
        let _ = crate::app_input::note_person(ctx, crate::human_input::Gesture::Other);
        let _ = crate::app_input::note_person(ctx, crate::human_input::Gesture::PlainEnter);
        let line = format!(
            "{} off=9 from=h-andrew kind=task trust=human text=SENTINEL",
            ctx.self_id.as_str()
        );
        let reply = crate::fabric::cmd_deliver(&store, &line);
        assert!(reply.starts_with("OK "), "{reply}");
        let _ = crate::app_input::note_person(ctx, crate::human_input::Gesture::Other);
        let _ = crate::app_input::note_person(ctx, crate::human_input::Gesture::PlainEnter);
    });
    drop(hand);
    (h, seen)
}

fn drive_human() -> Seen {
    drive_human_at(62).1
}

/// THE `human` ROWS ARE THE BURST MARKS (6F-8, ruling U13; T39): a
/// person's burst, a delivered mail, then the same person typing on inside
/// that 30 s burst, twice ([`drive_human_at`]): the ring keeps one `human`
/// row (the burst's); the log holds its mark and one more `human` mark, the
/// mail latch's (the first input after the mail), which says `why=mail`;
/// the input after that writes neither. The view (the log's records,
/// projected) is the ring's rows, row for record, and the fold is the same
/// with every `human` mark's `why=` taken out (it reads none: the
/// latch's mark answers the mail as it did). NEGATIVE CONTROL: the projection with `why=`
/// dropped shows a `human` row the ring never had.
#[test]
fn the_human_rows_are_the_burst_marks_and_the_latchs_own_says_why() {
    let (h, _seen) = drive_human_at(61);
    let g = h.ctx.timeline.lock().unwrap();
    let human = ["human"];
    let ring = ring_rows(&g, &human);
    assert_eq!(ring.len(), 1, "the ring keeps the burst rule: {ring:?}");
    let log = g.slog().log();
    let marks: Vec<String> = log
        .window()
        .filter(|r| r.op == Op::Mark && matches!(r.k, Kind::Human | Kind::Mail))
        .map(|r| match r.f.get_word(Key::Why) {
            Some(why) => format!("{} why={why}", r.k.word()),
            None => r.k.word().to_string(),
        })
        .collect();
    assert_eq!(
        marks,
        ["human", "mail", "human why=mail"],
        "the person's first input after the mail is the latch's own mark"
    );
    assert_eq!(
        only(overlap_rows(log), &human),
        ring,
        "the view's human rows are the ring's"
    );
    let fold = |drop_why: bool| {
        let mut fold = aterm_slog::Fold::default();
        for r in log.window() {
            let mut r = r.clone();
            if drop_why && r.k == Kind::Human {
                r.f.remove(Key::Why);
            }
            fold.apply(&r);
        }
        fold
    };
    assert_eq!(fold(false), fold(true), "the fold reads no why=");
    assert!(
        !fold(false).in_since_human(),
        "the latch's mark answered the mail"
    );
    assert_ne!(
        only(overlap_rows_without(log, Some(Key::Why)), &human),
        ring,
        "NEGATIVE CONTROL: without why= the view shows a row the ring never had"
    );
}

/// How a timeline kind and the log differ (T19's named differences; since
/// 6F-8, ruling U13, the lossy rows and the rows on another trigger are
/// exact pairs, and a retention rule is the one difference left).
#[derive(Clone, Copy, Debug)]
enum Diff {
    /// A record (`<op> <kind>`) for every row the log keeps, which it keeps
    /// under the named rule (phase 6, 6F-4; U39): the driven call writes at
    /// least one, never more records than rows, and its newest record is
    /// the newest row ([`retained_row`]); row for row under the rule's rate
    /// ([`the_program_marks_keep_the_rows_under_their_retention_rule`]).
    Retention(&'static str, &'static str),
}

/// A `program` record line as the `program-state` row it keeps: `state=<w>
/// kind=<ask|->`, `none` read as the row's `-`.
fn retained_row(line: &str) -> String {
    let state = match value(line, "state") {
        Some("none") | None => "-",
        Some(w) => w,
    };
    format!("state={state} kind={}", value(line, "ask").unwrap_or("-"))
}

/// One named difference.
struct Known {
    /// The ring's kind.
    row: &'static str,
    diff: Diff,
    /// Why (a ruling or the design's rule).
    why: &'static str,
    /// The step that closes it, if one does.
    until: Option<&'static str>,
    /// The row's own writer, driven; `None` for one no unit test reaches.
    drive: Option<fn() -> Seen>,
}

/// Every §0.1 kind that is no exact pair, and how it differs.
const KNOWN_DIFFERENCES: &[Known] = &[Known {
    row: "program-state",
    diff: Diff::Retention(
        "mark program",
        "64 a run (6F-3); a flap past the bucket coalesced to its latest state",
    ),
    why: "U39: the ring keeps 256 rows a session (PROGRAM_STATE_ROWS_MAX), the log 64 \
              program marks a run (T6's share), and a move past the run's bucket is coalesced \
              into the next mark (U17, U44)",
    until: None,
    drive: Some(drive_program),
}];

/// What one driven call wrote: the ring's rows, and the log's records at
/// the content tier (`recs`, as lines; `log`, as records).
#[derive(Clone, Debug, Default)]
struct Seen {
    rows: Vec<(String, String)>,
    recs: Vec<String>,
    log: Vec<Rec>,
}

impl Seen {
    /// `self`, then what `more` wrote.
    fn extend(&mut self, more: Seen) {
        self.rows.extend(more.rows);
        self.recs.extend(more.recs);
        self.log.extend(more.log);
    }
}

/// What `f` wrote on `tl`.
fn during(tl: &Mutex<SessionTimeline>, f: impl FnOnce()) -> Seen {
    let (id, seq) = {
        let g = tl.lock().unwrap();
        (g.high_id(), g.slog().log().last_seq())
    };
    f();
    seen_since(tl, id, seq)
}

/// What `tl` holds past ring id `id` and log seq `seq`.
fn seen_since(tl: &Mutex<SessionTimeline>, id: Option<u64>, seq: u64) -> Seen {
    let g = tl.lock().unwrap();
    Seen {
        rows: g
            .since(id)
            .map(|e| (e.kind.to_string(), e.payload.to_string()))
            .collect(),
        recs: g
            .slog()
            .log()
            .window()
            .filter(|r| r.seq > seq)
            .map(|r| aterm_slog::encode(r, Tier::Content).expect("fits"))
            .collect(),
        log: g
            .slog()
            .log()
            .window()
            .filter(|r| r.seq > seq)
            .cloned()
            .collect(),
    }
}

/// A registered session and its store.
fn rig(local: u64) -> (SessionHandle, Store) {
    let h = test_handle(local);
    let store = new_store();
    store.write().unwrap().register(h.clone());
    (h, store)
}

/// A record line's `<op> <kind>`.
fn op_kind(line: &str) -> String {
    let t: Vec<&str> = line.split(' ').collect();
    format!(
        "{} {}",
        t.get(1).copied().unwrap_or(""),
        t.get(3).copied().unwrap_or("")
    )
}

/// The value of `key=` in a space-separated line.
fn value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split(' ')
        .find_map(|t| t.strip_prefix(key).and_then(|v| v.strip_prefix('=')))
}

/// Whether the named difference `k` still holds over `seen`.
fn holds(k: &Known, seen: &Seen) -> Result<(), String> {
    let rows: Vec<&String> = seen
        .rows
        .iter()
        .filter(|(kind, _)| kind == k.row)
        .map(|(_, p)| p)
        .collect();
    if rows.is_empty() {
        return Err(format!(
            "{}: the driver wrote no row: {:?}",
            k.row, seen.rows
        ));
    }
    let of =
        |sel: &str| -> Vec<&String> { seen.recs.iter().filter(|l| op_kind(l) == sel).collect() };
    match k.diff {
        Diff::Retention(sel, rule) => {
            let recs = of(sel);
            let (Some(newest_rec), Some(newest_row)) = (recs.last(), rows.last()) else {
                return Err(format!("{}: no {sel} record: {:?}", k.row, seen.recs));
            };
            if recs.len() > rows.len() {
                return Err(format!(
                    "{}: {} records for {} rows ({rule})",
                    k.row,
                    recs.len(),
                    rows.len()
                ));
            }
            if retained_row(newest_rec) != **newest_row {
                return Err(format!(
                    "{}: the newest record {newest_rec} is not the newest row {newest_row} \
                     ({rule})",
                    k.row
                ));
            }
            Ok(())
        }
    }
}

fn drive_spawned() -> Seen {
    let h = test_handle(41);
    let store = new_store();
    during(&h.ctx.timeline, || {
        store.write().unwrap().register(h.clone())
    })
}

fn drive_closing() -> Seen {
    let (h, store) = rig(42);
    during(&h.ctx.timeline, || {
        let _ = store.write().unwrap().deregister_local_as(
            42,
            ExitReason::CtlClose,
            ExitActor::Sid("s-driver".into()),
        );
    })
}

/// The registry's every state move: back to spawning (a test handle is
/// alive), its reader's first read, the exit, then the close (whose
/// `state=closed` row is the session close's).
fn drive_state() -> Seen {
    let (h, store) = rig(43);
    during(&h.ctx.timeline, || {
        store.write().unwrap().set_state(43, SessionState::Spawning);
        assert!(store.write().unwrap().mark_alive(43));
        store.write().unwrap().set_state(43, SessionState::Exited);
        let _ =
            store
                .write()
                .unwrap()
                .deregister_local_as(43, ExitReason::Unknown, ExitActor::Unknown);
    })
}

/// What the drift flush (`App::observe_title_drift`, the production path
/// of `title-change` and `cwd-change`) wrote for `reports`, each an engine
/// report (an OSC 2 title, an OSC 7 directory) read past the debounce, on
/// the App's front session.
fn drifted(reports: &[&[u8]]) -> Seen {
    let mut app = App::headless_for_test();
    let session = app
        .front_terminal(WindowId(0))
        .expect("a front terminal")
        .session;
    let pooled = app.pool.get(session).expect("pooled");
    let (ctx, term) = (Arc::clone(&pooled.ctx), Arc::clone(&pooled.term));
    let t0 = Instant::now();
    app.observe_title_drift(session, t0);
    let step = crate::DRIFT_REFRESH_MIN_INTERVAL + Duration::from_millis(1);
    during(&ctx.timeline, || {
        let mut at = t0;
        for report in reports {
            crate::term_lock(&term).process(report);
            at += step;
            app.observe_title_drift(session, at);
        }
    })
}

/// A title, a spinner frame over it, another frame (no move: no row, no
/// mark, the title's cadence since 6F-8), another title, then none.
fn drive_title() -> Seen {
    drifted(&[
        b"\x1b]2;vim build\x07",
        "\x1b]2;\u{280b} vim build\x07".as_bytes(),
        "\x1b]2;\u{2819} vim build\x07".as_bytes(),
        b"\x1b]2;make test\x07",
        b"\x1b]2;\x07",
    ])
}

/// A TITLE AT ITS PRODUCER'S BOUNDS (6F-8): longer than the mark's
/// `title=` holds, of characters each an escape in the row, then one with
/// a hidden character: the row prints the title as the mark holds it.
fn drive_title_at_its_bounds() -> Seen {
    let (h, _store) = rig(71);
    let long = format!("vim {}end", "t\u{e9} ".repeat(80));
    during(&h.ctx.timeline, || {
        let mut g = h.ctx.timeline.lock().unwrap();
        assert!(g.record_title(&long));
        assert!(g.record_title("vim\u{200b} hidden"));
    })
}

/// Two directories, the same one again (no move), through the drift flush.
fn drive_cwd() -> Seen {
    drifted(&[
        b"\x1b]7;file://localhost/tmp/a%20b\x07",
        b"\x1b]7;file://localhost/tmp/c\x07",
        b"\x1b]7;file://localhost/tmp/c\x07",
    ])
}

/// A seamless update's successor reading the directory its predecessor
/// marked (its fingerprint carried: no row, no mark, since 6F-8), then a
/// new one, then none (`cwd=-`).
fn drive_cwd_successor() -> Seen {
    let (h, store) = rig(45);
    h.ctx
        .timeline
        .lock()
        .unwrap()
        .record_cwd_change(Some("/tmp/a b"));
    let (next, _next_store, _hand) = hand_over(&h, &store.read().unwrap(), 1_000, true);
    during(&next.ctx.timeline, || {
        let mut g = next.ctx.timeline.lock().unwrap();
        g.record_cwd_change(Some("/tmp/a b"));
        g.record_cwd_change(Some("/tmp/c"));
        g.record_cwd_change(None);
    })
}

/// A DIRECTORY AT ITS PRODUCER'S BOUNDS (6F-8): longer than the mark's
/// `cwd=` holds, of characters each an escape in the row, then one with a
/// hidden character: the row prints the directory as the mark holds it.
fn drive_cwd_at_its_bounds() -> Seen {
    let (h, _store) = rig(72);
    let long = format!("/tmp/{}end", "d \u{e9}/".repeat(80));
    during(&h.ctx.timeline, || {
        let mut g = h.ctx.timeline.lock().unwrap();
        g.record_cwd_change(Some(&long));
        g.record_cwd_change(Some("/tmp/hidden\u{200b}dir"));
    })
}

/// `lines` through the `meta` verb on `h`, each answered `OK`.
fn meta_lines(h: &SessionHandle, store: &Store, lines: &[&str]) {
    for line in lines {
        let reply = crate::control::meta_line_for_test(store, &h.term, h.local_id, &h.ctx, line);
        assert_eq!(reply, "OK\n", "{line}");
    }
}

/// The attention through the `meta` verb (set, another text, unset: its
/// row's text the mark's `attn_text=` since 6F-8), and a plain field.
fn drive_meta() -> Seen {
    let (h, store) = rig(46);
    during(&h.ctx.timeline, || {
        meta_lines(
            &h,
            &store,
            &[
                "set attention look here",
                "set attention look again",
                "unset attention",
                "set role builder",
            ],
        );
    })
}

/// A META VALUE AT ITS PRODUCER'S BOUNDS (6F-8): the longest description
/// the `meta` verb takes, of characters each an escape in the row, then
/// the longest attention text: each row shows 256 bytes, every one of them
/// the mark's.
fn drive_meta_at_its_bounds() -> Seen {
    let (h, store) = rig(73);
    let unit = "d\u{e9} ";
    let fill = |max: usize| format!("{}end", unit.repeat((max - 3) / unit.len()));
    let description = fill(crate::session_timeline::META_DESCRIPTION_MAX);
    let attention = fill(crate::session_timeline::META_ATTENTION_MAX);
    during(&h.ctx.timeline, || {
        meta_lines(
            &h,
            &store,
            &[
                &format!("set description {description}"),
                &format!("set attention {attention}"),
            ],
        );
    })
}

/// The Autopilot pair through its writer (`apply_autopilot`, the `meta`
/// verb's and the window's): a timed Cruise, its timer moved alone (a move
/// of the pair, its mark `land_at=` since 6F-8), the timer dropped, then
/// off.
fn drive_autopilot() -> Seen {
    let (h, _store) = rig(74);
    during(&h.ctx.timeline, || {
        for (word, land_at) in [
            ("cruise", Some(1_900_000_000_500)),
            ("cruise", Some(1_900_000_060_000)),
            ("cruise", None),
            ("off", None),
        ] {
            let w = crate::session_timeline::apply_autopilot(
                &h.ctx,
                Some(word.to_string()),
                land_at,
                Actor::Ctl,
                None,
            );
            assert!(w.moved, "{word} {land_at:?}");
        }
    })
}

/// A seamless update's successor restoring what its predecessor held (the
/// carried identity, `restore_carried_meta`, and the keyed attention,
/// `seed_carried_claims`): each restore a row, its mark `carried=1` (the
/// value the log last said: no news, ruling S5) since 6F-8; then a new
/// value, a move.
fn drive_meta_successor() -> Seen {
    use crate::session_timeline::{MetaEdit, MetaField, write_attention_owned};
    let (h, store) = rig(75);
    meta_lines(&h, &store, &["set description build", "set role builder"]);
    assert_eq!(
        write_attention_owned(
            &h.ctx,
            crate::harness_host::ATTENTION_OWNER,
            MetaEdit::Set("look here"),
            Actor::Harness,
        ),
        Ok(true)
    );
    let (carried, rows) = {
        let meta = h.ctx.meta.lock().unwrap();
        (meta.sanitized(), meta.handoff_attention_owners())
    };
    let (next, _next_store, _hand) = hand_over(&h, &store.read().unwrap(), 1_000, true);
    during(&next.ctx.timeline, || {
        assert!(crate::session_timeline::restore_carried_meta(
            &next.ctx, &carried
        ));
        crate::session_timeline::seed_carried_claims(
            &next.ctx.meta,
            &next.ctx.timeline,
            None,
            &rows,
            crate::metrics::now_us(),
        );
        assert!(crate::session_timeline::apply_meta_value(
            &next.ctx,
            MetaField::Role,
            Some("lead".to_string()),
            Actor::Ctl,
        ));
    })
}

/// Every kind of render transition: a run's (into the risk, signed, and
/// healed) and an epoch's (into it, healed, and healed while still open).
fn drive_render() -> Seen {
    use crate::resize_epoch::{EpochBy, ProgramClass};
    use crate::resize_ledger::Transition as T;
    let (h, _store) = rig(48);
    during(&h.ctx.timeline, || {
        let _ = h.ctx.timeline.lock().unwrap().record_render(&[
            T::Risk {
                run: 1,
                displaced: 2,
                net_zero: true,
                at_ms: 3,
            },
            T::Healed {
                run: 1,
                after_ms: 40,
            },
            T::Risk {
                run: 2,
                displaced: -3,
                net_zero: false,
                at_ms: 50,
            },
            T::EpochRisk {
                epoch: 4,
                shift: -2,
                pushed: 1,
                cut: 6,
                drew: true,
                class: ProgramClass::Learned,
                at_ms: 70,
            },
            T::EpochHealed {
                epoch: 4,
                after_ms: 80,
                by: EpochBy::Undo,
            },
            T::EpochHealed {
                epoch: 5,
                after_ms: 90,
                by: EpochBy::None,
            },
        ]);
    })
}

/// A person's reset and a verb's that reverted nothing, then a handback
/// through the reader's own recorder and one that put nothing back.
fn drive_modes() -> Seen {
    use crate::manual_reset::{ResetReport, ResetSource};
    let (h, _store) = rig(49);
    during(&h.ctx.timeline, || {
        for (source, reverted, bytes) in [
            (ResetSource::Menu, vec!["mouse", "paste"], 12),
            (ResetSource::Verb, vec![], 0),
        ] {
            crate::manual_reset::record(
                49,
                &h.ctx.timeline,
                source,
                &ResetReport { reverted, bytes },
            );
        }
        for (reverted, bytes) in [(&["alt", "kitty"][..], 42), (&[][..], 0)] {
            let _ = crate::foreground_handback::record_restored(
                &mut h.ctx.timeline.lock().unwrap(),
                crate::foreground_handback::FgEdge::group(0, 300, 301),
                reverted,
                bytes,
            );
        }
    })
}

/// Two keyed writes in doubt, one with no reply at all.
fn drive_in_doubt() -> Seen {
    let (h, _store) = rig(50);
    let key = |seq: u64| {
        crate::pty_idem::parse_key(&format!("{}:3:{seq}", h.ctx.nonce.to_hex()), h.ctx.nonce)
            .expect("a key")
    };
    during(&h.ctx.timeline, || {
        let _ = crate::pty_idem::record_in_doubt(&h.ctx, "send", key(9), "ERR write failed\n");
        let _ = crate::pty_idem::record_in_doubt(&h.ctx, "paste", key(10), "");
    })
}

/// A seamless update's successor, carried and not (each past the
/// predecessor's records: the ring is never carried), then a shell the PTY
/// keeper held through a crash: each one's handoff and registration.
fn drive_handoff() -> Seen {
    let mut all = Seen::default();
    for carry in [true, false] {
        let (h, store) = rig(51);
        h.ctx
            .timeline
            .lock()
            .unwrap()
            .record_cwd_change(Some("/tmp"));
        let head = h.ctx.timeline.lock().unwrap().slog().log().last_seq();
        let (next, _next_store, _hand) = hand_over(&h, &store.read().unwrap(), 1_000, carry);
        all.extend(seen_since(&next.ctx.timeline, None, head));
    }
    let kept = test_handle(65);
    crate::spawn::mark_handoff_gap(
        &kept.ctx.timeline,
        &kept.ctx.cast,
        &kept.ctx.temporal,
        Some(9),
        0,
        crate::session_log::LogAdopt::default(),
        true,
    );
    new_store().write().unwrap().register(kept.clone());
    all.extend(seen_since(&kept.ctx.timeline, None, 0));
    all
}

/// A program's report, as the gate shows it.
fn program_report(
    state: aterm_core::terminal::program_state::ProgramStateWord,
    kind: Option<aterm_core::terminal::program_state::ProgramBlockedKind>,
) -> aterm_core::terminal::program_state::ProgramState {
    aterm_core::terminal::program_state::ProgramState { state, kind }
}

fn drive_program() -> Seen {
    use aterm_core::terminal::program_state::{ProgramBlockedKind, ProgramStateWord};
    let (h, _store) = rig(53);
    during(&h.ctx.timeline, || {
        let mut g = h.ctx.timeline.lock().unwrap();
        let _ = g.note_program_state(Some(program_report(ProgramStateWord::Working, None)));
        let _ = g.note_program_state(Some(program_report(
            ProgramStateWord::Blocked,
            Some(ProgramBlockedKind::Permission),
        )));
        let _ = g.note_program_state(None);
    })
}

/// THE PROGRAM MARKS KEEP THE ROWS UNDER THEIR RETENTION RULE (6F-4; U39):
/// moves a second apart (under the run's bucket) are the ring's rows, row
/// for record, in order; a burst of 60 at one instant is past it, and once a
/// token frees the log's newest program mark is the ring's newest row,
/// with the states it replaced counted. NEGATIVE CONTROL: the burst's
/// records are fewer than its rows, so row for record no longer holds.
#[test]
fn the_program_marks_keep_the_rows_under_their_retention_rule() {
    use aterm_core::terminal::program_state::{ProgramBlockedKind, ProgramStateWord as W};
    let (h, _store) = rig(54);
    let hand = super::super::tests::hand_clock(&h.ctx.timeline);
    let moves = [
        (W::Working, None),
        (W::Blocked, Some(ProgramBlockedKind::Question)),
        (W::Working, None),
        (W::Done, None),
        (W::Idle, None),
        (W::Error, None),
    ];
    let rows_and_marks = |g: &SessionTimeline| {
        let rows: Vec<String> = g
            .since(None)
            .filter(|e| e.kind == crate::session_timeline::PROGRAM_STATE)
            .map(|e| e.payload.to_string())
            .collect();
        let marks: Vec<String> = g
            .slog()
            .log()
            .window()
            .filter(|r| r.k == Kind::Program)
            .map(|r| retained_row(&aterm_slog::encode(r, Tier::Content).expect("fits")))
            .collect();
        (rows, marks)
    };
    for (i, (state, kind)) in moves.into_iter().enumerate() {
        hand.set(1_000 * (i as u64 + 1));
        let mut g = h.ctx.timeline.lock().unwrap();
        let _ = g.note_program_state(Some(program_report(state, kind)));
    }
    let (rows, marks) = rows_and_marks(&h.ctx.timeline.lock().unwrap());
    assert_eq!(rows.len(), moves.len());
    assert_eq!(marks, rows, "under the bucket: row for record");
    // A burst past the bucket, then a token.
    hand.set(60_000);
    {
        let mut g = h.ctx.timeline.lock().unwrap();
        for i in 0..60 {
            let state = if i % 2 == 0 { W::Working } else { W::Blocked };
            let kind = (state == W::Blocked).then_some(ProgramBlockedKind::Auth);
            let _ = g.note_program_state(Some(program_report(state, kind)));
        }
    }
    hand.set(61_000);
    let mut g = h.ctx.timeline.lock().unwrap();
    let at = g.slog().now();
    g.slog_mut().program_tick(at);
    let (rows, marks) = rows_and_marks(&g);
    assert_eq!(
        marks.last(),
        rows.last(),
        "past it: the newest row is the newest mark"
    );
    assert_eq!(
        rows.last().map(String::as_str),
        Some("state=blocked kind=auth")
    );
    let newest = g
        .slog()
        .log()
        .window()
        .rfind(|r| r.k == Kind::Program)
        .map(|r| aterm_slog::encode(r, Tier::Content).expect("fits"))
        .expect("a program mark");
    assert!(newest.contains(" coalesced="), "{newest}");
    assert!(
        marks.len() < rows.len(),
        "NEGATIVE CONTROL: no longer row for record"
    );
}

fn drive_policy() -> Seen {
    let app = App::headless_for_test();
    let session = app
        .front_terminal(WindowId(0))
        .expect("a front terminal")
        .session;
    let ctx = Arc::clone(&app.pool.get(session).expect("pooled").ctx);
    during(&ctx.timeline, || app.announce_harness_policy())
}

/// `rig`'s session with one delivered row (inbox id 1, off=7).
fn delivered(local: u64) -> (SessionHandle, Store) {
    let (h, store) = rig(local);
    let line = format!(
        "{} off=7 from=h-andrew kind=task trust=human text=SENTINEL",
        h.ctx.self_id.as_str()
    );
    let reply = crate::fabric::cmd_deliver_waking(&store, &line, &|_| {});
    assert_eq!(reply, "OK 1\n");
    (h, store)
}

/// Every `inbox seen` (6F-6): a verdict decided, the same again (moved
/// nothing: `moved=0`), and a bare seen past it on a second row.
fn drive_inbox_seen() -> Seen {
    let (h, store) = delivered(54);
    let line = format!(
        "{} off=9 from=s-peer@n-lab kind=ask trust=agent text=SENTINEL",
        h.ctx.self_id.as_str()
    );
    assert_eq!(
        crate::fabric::cmd_deliver_waking(&store, &line, &|_| {}),
        "OK 2\n"
    );
    during(&h.ctx.timeline, || {
        for (args, reply) in [
            ("1 handled", "OK seen=1\n"),
            ("1 handled", "OK seen=1\n"),
            ("2", "OK seen=2\n"),
        ] {
            assert_eq!(crate::fabric::cmd_inbox_seen(&h.ctx, args), reply);
        }
    })
}

/// A post from `h` (post id 1), queued whether or not a bridge is up.
fn post(h: &SessionHandle) {
    let reply = crate::fabric::cmd_post(
        &h.ctx,
        "to=@s-peer kind=ask dl=60000 key=k1 --wait=0 SENTINEL-POST",
        None,
    );
    assert!(queued(&reply, 1), "{reply}");
}

/// A post with every option (6F-6), then a broadcast on a topic: its
/// `to=say:<topic>` is the topic's fingerprint on the record.
fn drive_post() -> Seen {
    let (h, _store) = rig(55);
    during(&h.ctx.timeline, || {
        post(&h);
        let reply = crate::fabric::cmd_post(
            &h.ctx,
            "to=say:builds kind=note re=7 via=n-lab --wait=0 SENTINEL-POST",
            None,
        );
        assert!(queued(&reply, 2), "{reply}");
    })
}

/// Two posts retired: one landed at an offset, one nowhere (`off=-`).
fn drive_landed() -> Seen {
    let (h, store) = rig(56);
    post(&h);
    post_again(&h);
    let sid = h.ctx.self_id.as_str().to_string();
    during(&h.ctx.timeline, || {
        for line in [format!("{sid} 1 off=90"), format!("{sid} 2 off=-")] {
            let reply = crate::fabric::cmd_outbox_sent_waking(&store, &line, &|_| {});
            assert!(reply.starts_with("OK"), "{line}: {reply}");
        }
    })
}

/// A second post from `h` (post id 2).
fn post_again(h: &SessionHandle) {
    let reply = crate::fabric::cmd_post(&h.ctx, "to=@s-peer kind=note --wait=0 SENTINEL", None);
    assert!(queued(&reply, 2), "{reply}");
}

/// Whether `post`'s reply says post `id` is queued, whatever the link: `OK
/// <id>`, or a refusal of the wait that names the queued id (`ERR fabric
/// absent id=<id>` with no bridge, `ERR timeout id=<id>` while a parallel
/// test's link section has one up).
pub(super) fn queued(reply: &str, id: u64) -> bool {
    let named = format!("id={id}");
    reply.starts_with(&format!("OK {id}")) || reply.split_whitespace().any(|t| t == named)
}

/// The owner's hold, on with its reason and lifted.
fn drive_hold() -> Seen {
    let (h, store) = rig(57);
    let sid = h.ctx.self_id.as_str().to_string();
    during(&h.ctx.timeline, || {
        for rest in ["on reason=demo", "off"] {
            let reply = crate::fabric::cmd_hold_waking(
                &store,
                &format!("{sid} {rest}"),
                crate::fabric::HoldIssuer::Owner,
                &|_| {},
            );
            assert!(reply.starts_with("OK hold="), "{reply}");
        }
    })
}

fn drive_topic() -> Seen {
    let (h, _store) = rig(58);
    during(&h.ctx.timeline, || {
        assert_eq!(
            h.ctx.fabric.topic_add("news", "head", &h.ctx.timeline),
            Ok(true)
        );
        assert!(h.ctx.fabric.topic_drop("news", &h.ctx.timeline));
    })
}

/// A bridge attached to `store` and its link up: the fabric's `connected`
/// (inside [`crate::fabric::with_link_reset`]).
pub(super) fn link_up(store: &Store) -> crate::fabric::BridgeGeneration {
    let generation = crate::fabric::next_bridge_generation();
    crate::fabric::bridge_attached(generation);
    assert!(crate::fabric::link_report(
        store,
        Some(generation),
        true,
        "",
        Some(1)
    ));
    generation
}

/// THE FETCH'S OWN WRITER (`inbox get @<off>`): a read the ring cannot
/// answer parks, writing its `fetch` row as it parks (the watchers' ring a
/// one-minute hang detector), and the bridge's `fetched= err=no-record`
/// answer ends it (inside [`crate::fabric::with_link_reset`]).
pub(super) fn fetch_answered(h: &SessionHandle, store: &Store, off: u64) {
    let watcher = crate::session_watch::Subscription::single(&h.ctx.watchers);
    let ctx = Arc::clone(&h.ctx);
    let reader =
        std::thread::spawn(move || crate::fabric::cmd_inbox_get(&*ctx, &format!("@{off}")));
    // The row is written under the inbox guard the read parks with: once
    // it is there, the bridge's answer lands in the read's own slot.
    let asked = |h: &SessionHandle| {
        h.ctx
            .timeline
            .lock()
            .unwrap()
            .since(None)
            .any(|e| e.kind == "fetch" && *e.payload == *format!("off={off}"))
    };
    while !asked(h) {
        assert!(
            watcher.wait(aterm_spec::wait::MUST_HAPPEN),
            "the fetch never parked"
        );
    }
    let answer = format!("{} fetched={off} err=no-record", h.ctx.self_id.as_str());
    let reply = crate::fabric::cmd_deliver_waking(store, &answer, &|_| {});
    assert_eq!(reply, "OK\n", "{answer}");
    let read = reader.join().expect("the reader");
    assert!(read.starts_with("ERR "), "{read}");
}

fn drive_fetch() -> Seen {
    crate::fabric::with_link_reset(|| {
        let (h, store) = rig(63);
        let _ = link_up(&store);
        during(&h.ctx.timeline, || fetch_answered(&h, &store, 7))
    })
}

/// THE ADVISORY'S OWN WRITER: a bridge that governed the session (a
/// delivery) is lost, and by default nothing is held: an advisory is put
/// (inside [`crate::fabric::with_link_reset`]).
fn drive_advisory() -> Seen {
    crate::fabric::with_link_reset(|| {
        let (h, store) = rig(64);
        let generation = link_up(&store);
        let line = format!(
            "{} off=7 from=h-andrew kind=note trust=human text=SENTINEL",
            h.ctx.self_id.as_str()
        );
        assert_eq!(
            crate::fabric::cmd_deliver_waking(&store, &line, &|_| {}),
            "OK 1\n"
        );
        during(&h.ctx.timeline, || {
            assert_eq!(crate::fabric::bridge_lost(&store, generation), 1);
        })
    })
}

/// The longest principal of `class` the grammar admits: its prefix and
/// [`aterm_types::fabric_grammar::PRINCIPAL_NAME_MAX`] bytes of `fill`.
fn longest(class: &str, fill: &str) -> String {
    format!(
        "{class}-{}",
        fill.repeat(aterm_types::fabric_grammar::PRINCIPAL_NAME_MAX)
    )
}

/// MAIL AT ITS PRODUCER'S BOUNDS (the review of 6F-6): a delivery from the
/// longest sender the grammar admits (`s-<32>@n-<32>`, 69 bytes), then its
/// `inbox seen` with a verdict, which names that sender too.
fn drive_mail_at_its_bounds() -> Seen {
    let (h, store) = rig(66);
    let from = format!("{}@{}", longest("s", "c"), longest("n", "d"));
    let line = format!(
        "{} off=7 from={from} kind=ask trust=agent text=SENTINEL",
        h.ctx.self_id.as_str()
    );
    during(&h.ctx.timeline, || {
        assert_eq!(
            crate::fabric::cmd_deliver_waking(&store, &line, &|_| {}),
            "OK 1\n"
        );
        assert_eq!(
            crate::fabric::cmd_inbox_seen(&h.ctx, "1 handled"),
            "OK seen=1\n"
        );
    })
}

/// A POST AT ITS PRODUCER'S BOUNDS (the review of 6F-6): to the longest
/// address (`@s-<32>@n-<32>`), of the longest kind, `re=` and `dl=` at their
/// widest, through sixteen hops of the longest principal (559 bytes) and
/// with a 64-byte key — a record the hops leave no room in, so they are cut
/// there, its row cut by the ring's 256 bytes — then a broadcast whose row
/// shows 233 bytes of the same hops.
fn drive_post_at_its_bounds() -> Seen {
    let via = vec![longest("n", "v"); aterm_types::fabric_grammar::VIA_MAX_HOPS].join(",");
    let (h, _store) = rig(67);
    during(&h.ctx.timeline, || {
        let reply = crate::fabric::cmd_post(
            &h.ctx,
            &format!(
                "to=@{}@{} kind=control re={max} dl={max} via={via} key={} --wait=0 SENTINEL",
                longest("s", "a"),
                longest("n", "b"),
                "k".repeat(crate::fabric::KEY_MAX),
                max = u64::MAX
            ),
            None,
        );
        assert!(queued(&reply, 1), "{reply}");
        let reply = crate::fabric::cmd_post(
            &h.ctx,
            &format!("to=say kind=note via={via} --wait=0 SENTINEL"),
            None,
        );
        assert!(queued(&reply, 2), "{reply}");
    })
}

/// A HOLD AT ITS PRODUCER'S BOUNDS (the review of 6F-6): an owner's local
/// hold whose reason is a flood of `%` (each the caller's literal percent,
/// its own escape: 42 of them), then the fleet's hold over it, by the
/// bridge (the longest record's author), whose reason is 42 escapes and two
/// bytes (128 bytes, 212 encoded: the cap), both standing
/// (`hold_origin=fleet,local`), then each lifted by its own.
fn drive_hold_at_its_bounds() -> Seen {
    use crate::fabric::HoldIssuer::{Bridge, Owner};
    let (h, store) = rig(68);
    let sid = h.ctx.self_id.as_str().to_string();
    let flood = format!("{}ab", "%".repeat(126));
    let escapes = format!("{}ab", "%E9".repeat(42));
    during(&h.ctx.timeline, || {
        for (rest, issuer) in [
            (format!("on reason={flood} origin=local"), Owner),
            (format!("on reason={escapes} origin=fleet"), Bridge),
            ("off origin=fleet".to_string(), Bridge),
            ("off origin=local".to_string(), Owner),
        ] {
            let reply =
                crate::fabric::cmd_hold_waking(&store, &format!("{sid} {rest}"), issuer, &|_| {});
            assert!(reply.starts_with("OK hold="), "{rest}: {reply}");
        }
    })
}

/// A KEYED WRITE IN DOUBT AT ITS PRODUCER'S BOUNDS (the review of 6F-7): a
/// reply whose words are longer than the log's `reply=` holds, and one with
/// a hidden character: the row prints the words as the mark holds them.
fn drive_in_doubt_at_its_bounds() -> Seen {
    let (h, _store) = rig(69);
    let key = |seq: u64| {
        crate::pty_idem::parse_key(&format!("{}:3:{seq}", h.ctx.nonce.to_hex()), h.ctx.nonce)
            .expect("a key")
    };
    let long = format!("ERR {} tail\n", "x".repeat(300));
    during(&h.ctx.timeline, || {
        let _ = crate::pty_idem::record_in_doubt(&h.ctx, "send", key(11), &long);
        let _ =
            crate::pty_idem::record_in_doubt(&h.ctx, "send", key(12), "ERR\u{200b}hidden words\n");
    })
}

/// Every mode this test can leave on: the kitty keyboard (main and alt), the
/// alternate screen, mouse reports and their encoding, focus, size and
/// colour-scheme reports, the alternate scroll off, the xterm keyboard,
/// cursor and keypad keys, backarrow, num lock, meta and alt sending escape,
/// insert, newline, reverse video, the DEC graphics in G0 shifted into GL, a
/// hidden cursor, an open hyperlink, origin mode, margins and a region, a
/// palette colour, synchronized output, and a sequence cut short.
const MODES_ON: &[u8] = b"\x1b[>1u\x1b[?1049h\x1b[>1u\x1b[?1000h\x1b[?1006h\x1b[?1004h\
    \x1b[?2048h\x1b[?2031h\x1b[?1007l\x1b[>4;2m\x1b[>4;1f\x1b[?1h\x1b=\x1b[?67h\x1b[?1035h\
    \x1b[?1036h\x1b[?1039h\x1b[4h\x1b[20h\x1b[?5h\x1b(0\x0e\x1b[?25l\x1b]8;;http://x\x1b\\\
    \x1b[?6h\x1b[?69h\x1b[2;10s\x1b[2;10r\x1b]4;1;rgb:00/00/00\x1b\\\x1b[?2026h\x1b[1";

/// A HANDBACK AT ITS PRODUCER'S BOUNDS (the review of 6F-7): a terminal a
/// program left with [`MODES_ON`], handed back through the reader's own
/// recorder with the engine's own list of what it put back, longer than the
/// row's payload shows.
fn drive_modes_at_its_bounds() -> Seen {
    let mut t = aterm_core::terminal::Terminal::new(24, 80);
    t.process(MODES_ON);
    let back = t.foreground_handback().expect("the alt screen is evidence");
    let joined = back.reverted.join(",");
    assert!(joined.len() > 200, "{} bytes: {joined}", joined.len());
    let (h, _store) = rig(70);
    during(&h.ctx.timeline, || {
        let _ = crate::foreground_handback::record_restored(
            &mut h.ctx.timeline.lock().unwrap(),
            crate::foreground_handback::FgEdge::group(0, 300, 301),
            &back.reverted,
            back.bytes.len(),
        );
    })
}

/// A kind that left [`KNOWN_DIFFERENCES`] for [`EXACT`] (phase 6, 6F-5 to
/// 6F-7), with its row's own writer, driven, and a key its projection
/// cannot do without (the negative control's).
struct Driven {
    row: &'static str,
    drive: fn() -> Seen,
    needs: Key,
}

/// Every [`EXACT`] kind 5B-6's script does not write, through its writer.
const DRIVEN: &[Driven] = &[
    Driven {
        row: "state-change",
        drive: drive_state,
        needs: Key::State,
    },
    Driven {
        row: crate::harness_policy_verb::HARNESS_POLICY_EVENT,
        drive: drive_policy,
        needs: Key::Gen,
    },
    Driven {
        row: "hold-advisory",
        drive: drive_advisory,
        needs: Key::Reason,
    },
    Driven {
        row: "fetch",
        drive: drive_fetch,
        needs: Key::Off,
    },
    Driven {
        row: "topic",
        drive: drive_topic,
        needs: Key::TopicName,
    },
    Driven {
        row: "inbox-seen",
        drive: drive_inbox_seen,
        needs: Key::Seen,
    },
    Driven {
        row: "post",
        drive: drive_post,
        needs: Key::Idem,
    },
    Driven {
        row: "post-landed",
        drive: drive_landed,
        needs: Key::Off,
    },
    Driven {
        row: "hold",
        drive: drive_hold,
        needs: Key::Reason,
    },
    Driven {
        row: "spawned",
        drive: drive_spawned,
        needs: Key::State,
    },
    Driven {
        row: "closing",
        drive: drive_closing,
        needs: Key::Closer,
    },
    Driven {
        row: "render",
        drive: drive_render,
        needs: Key::Run,
    },
    Driven {
        row: "modes-restored",
        drive: drive_modes,
        needs: Key::Reverted,
    },
    Driven {
        row: "in-doubt",
        drive: drive_in_doubt,
        needs: Key::KeySeq,
    },
    Driven {
        row: "handoff",
        drive: drive_handoff,
        needs: Key::Build,
    },
    // Phase 6 (6F-8, ruling U13): the four rows whose rule differed.
    Driven {
        row: crate::session_timeline::TITLE_CHANGE,
        drive: drive_title,
        needs: Key::Title,
    },
    Driven {
        row: crate::session_timeline::CWD_CHANGE,
        drive: drive_cwd,
        needs: Key::Cwd,
    },
    Driven {
        row: crate::session_timeline::CWD_CHANGE,
        drive: drive_cwd_successor,
        needs: Key::Cwd,
    },
    Driven {
        row: crate::session_timeline::META_CHANGE,
        drive: drive_meta,
        needs: Key::AttnText,
    },
    Driven {
        row: crate::session_timeline::META_CHANGE,
        drive: drive_autopilot,
        needs: Key::LandAt,
    },
    Driven {
        row: crate::session_timeline::META_CHANGE,
        drive: drive_meta_successor,
        needs: Key::Value,
    },
    Driven {
        row: "human",
        drive: drive_human,
        needs: Key::Why,
    },
    // AT THEIR PRODUCERS' BOUNDS (the review of 6F-6 and 6F-7): each row's
    // longest values, whole in the log, where the ring's 256 bytes show them.
    Driven {
        row: "inbox",
        drive: drive_mail_at_its_bounds,
        needs: Key::From,
    },
    Driven {
        row: "post",
        drive: drive_post_at_its_bounds,
        needs: Key::Via,
    },
    Driven {
        row: "hold",
        drive: drive_hold_at_its_bounds,
        needs: Key::Reason,
    },
    Driven {
        row: "in-doubt",
        drive: drive_in_doubt_at_its_bounds,
        needs: Key::Reply,
    },
    Driven {
        row: "modes-restored",
        drive: drive_modes_at_its_bounds,
        needs: Key::Reverted,
    },
    Driven {
        row: crate::session_timeline::TITLE_CHANGE,
        drive: drive_title_at_its_bounds,
        needs: Key::Title,
    },
    Driven {
        row: crate::session_timeline::CWD_CHANGE,
        drive: drive_cwd_at_its_bounds,
        needs: Key::Cwd,
    },
    Driven {
        row: crate::session_timeline::META_CHANGE,
        drive: drive_meta_at_its_bounds,
        needs: Key::Value,
    },
];

/// EVERY EXACT KIND AGREES THROUGH ITS OWN WRITER (phase 6, 6F-5 to 6F-8):
/// each [`DRIVEN`] kind's writer, driven as production runs it, writes rows
/// whose every exact pair is a record of the log, projected, row for record
/// and in order: the whole payload. Every [`EXACT`] kind is the script's or
/// [`DRIVEN`]'s. NEGATIVE CONTROL: each kind's projection without the key
/// its row cannot do without differs.
#[test]
fn every_exact_kind_agrees_through_its_own_writer() {
    for kind in EXACT {
        assert!(
            SCRIPTED.contains(&kind) || DRIVEN.iter().any(|d| d.row == kind),
            "{kind} is checked nowhere"
        );
    }
    // Every kind is judged, and every one that fails is named.
    let mut errs = Vec::new();
    for d in DRIVEN {
        let s = (d.drive)();
        let rows = only(s.rows.clone(), &EXACT);
        let log = only(projected(&s.log, None), &EXACT);
        if !rows.iter().any(|(k, _)| k == d.row) {
            errs.push(format!("{}: the writer wrote no row: {:?}", d.row, s.rows));
        } else if log != rows {
            errs.push(format!(
                "{}: the log's records {log:?} against the ring's rows {rows:?} ({:?})",
                d.row, s.recs
            ));
        } else if only(projected(&s.log, Some(d.needs)), &EXACT) == rows {
            errs.push(format!(
                "{}: a projection without {:?} agrees",
                d.row, d.needs
            ));
        }
    }
    assert!(errs.is_empty(), "{}", errs.join("\n"));
}

/// THE SESSION ROWS' WORDS ARE THE LOG'S (6F-7): each word a session row
/// prints for a field its mark carries as a closed word is in that key's
/// list, so the mark always carries it and the projection prints the
/// row's word — a render epoch's program class and healer (an open
/// epoch's `-` is no word, and its mark has no `healed_by=`), a reset's
/// source, and a close's reason (the session close's `out=` for each
/// `ExitReason`). NEGATIVE CONTROL: a word outside a list is refused.
#[test]
fn the_session_rows_words_are_the_logs() {
    use crate::manual_reset::ResetSource;
    use crate::resize_epoch::{EpochBy, ProgramClass};
    let words = |k: Key| match k.cap() {
        aterm_slog::Cap::Word(w) => w,
        cap => panic!("{k:?} is {cap:?}"),
    };
    assert_eq!(
        words(Key::Class),
        [
            ProgramClass::Agent,
            ProgramClass::Learned,
            ProgramClass::Unknown
        ]
        .map(ProgramClass::wire)
    );
    assert_eq!(
        words(Key::HealedBy),
        [EpochBy::App, EpochBy::Undo, EpochBy::Nudge].map(EpochBy::wire)
    );
    assert_eq!(EpochBy::None.wire(), "-");
    assert_eq!(
        words(Key::Source),
        [ResetSource::Verb, ResetSource::Menu].map(ResetSource::as_str)
    );
    for r in ExitReason::ALL {
        let out = aterm_slog::Out::Session(crate::session_log::exit_out(r));
        assert_eq!(out.word(), r.as_str(), "{r:?}");
    }
    assert!(Fields::new().word(Key::Source, "palette").is_err());
}

/// THE TABLE IS WHOLE: every kind of §0.1 is an exact pair or a named
/// difference, once (since 6F-8 no superset kind is left); the kinds are the ones 5B-5's script writes (one row
/// of each, the goldens' script), and every fabric kind the code lists
/// ([`crate::fabric::FABRIC_EVENT_KINDS`]) is among them. A kind added to
/// the ring without a row here fails.
#[test]
fn every_timeline_kind_is_an_exact_pair_or_a_named_difference() {
    let mut named: Vec<&str> = EXACT
        .iter()
        .copied()
        .chain(KNOWN_DIFFERENCES.iter().map(|k| k.row))
        .collect();
    named.sort_unstable();
    let len = named.len();
    named.dedup();
    assert_eq!(named.len(), len, "a kind named twice: {named:?}");
    let mut section: Vec<&str> = SECTION_0_1.to_vec();
    section.sort_unstable();
    assert_eq!(named, section);
    let scripted = crate::control::wire_goldens::scripted(false);
    let mut written: Vec<&str> = scripted
        .ctx
        .timeline
        .lock()
        .unwrap()
        .since(None)
        .map(|e| e.kind)
        .collect();
    written.sort_unstable();
    written.dedup();
    assert_eq!(written, section, "the goldens' script writes §0.1's kinds");
    for kind in crate::fabric::FABRIC_EVENT_KINDS {
        assert!(named.contains(kind), "{kind} is not named");
    }
}

/// EVERY NAMED DIFFERENCE STILL HOLDS, through its row's own writer: a
/// step that closes one (a field the record now carries, a record on the
/// row's own trigger) turns this red, and its row moves to [`EXACT`] in the
/// same commit. Every writer is driven (since 6F-5 the fetch and the
/// advisory too, under the live link's test reset). NEGATIVE CONTROL: the
/// retention difference, closed on what its driver wrote, fails, and a
/// driver that wrote no row (6F-8 closed the lossy and trigger kinds).
#[test]
fn every_named_difference_still_holds() {
    let mut errs = Vec::new();
    let mut seen = std::collections::BTreeMap::new();
    for k in KNOWN_DIFFERENCES {
        let Some(drive) = k.drive else { continue };
        let s = drive();
        if let Err(e) = holds(k, &s) {
            errs.push(format!("{e} ({}; until {:?})", k.why, k.until));
        }
        seen.insert(k.row, s);
    }
    assert!(errs.is_empty(), "{}", errs.join("\n"));
    let undriven: Vec<&str> = KNOWN_DIFFERENCES
        .iter()
        .filter(|k| k.drive.is_none())
        .map(|k| k.row)
        .collect();
    assert!(undriven.is_empty(), "every writer is driven: {undriven:?}");
    // NEGATIVE CONTROLS: each difference closed.
    let known = |row: &str| {
        KNOWN_DIFFERENCES
            .iter()
            .find(|k| k.row == row)
            .expect("named")
    };
    // A retained kind whose newest record is a stale state, and one with a
    // record more than its rows.
    let mut program = seen["program-state"].clone();
    if let Some(l) = program.recs.last_mut() {
        *l = l.replace(" state=none ", " state=working ");
    }
    assert!(
        holds(known("program-state"), &program).is_err(),
        "a stale newest program record"
    );
    let mut program = seen["program-state"].clone();
    let first = program.recs[0].clone();
    program.recs.insert(0, first);
    assert!(
        holds(known("program-state"), &program).is_err(),
        "a program record more than its rows"
    );
    let none = Seen::default();
    assert!(holds(known("program-state"), &none).is_err(), "no row");
}

// ---- exits --------------------------------------------------------------------

/// The `by=` an actor's close should say, under 3A's R1.
fn by_r1(actor: &ExitActor) -> String {
    match actor {
        ExitActor::Sid(sid) if aterm_session::is_minted_sid(sid) => sid.clone(),
        ExitActor::Sid(_) | ExitActor::Unknown => "-".to_string(),
        ExitActor::Human => "human".to_string(),
        ExitActor::Ctl => "ctl".to_string(),
        ExitActor::Bridge => "bridge".to_string(),
    }
}

/// The closed index's session close for `local`: `(out, code, by)`.
fn index_close(app: &App, local: u64) -> Option<(String, Option<i64>, String)> {
    let source = app
        .store
        .read()
        .unwrap()
        .closed_logs()
        .by_local(local)?
        .source();
    let log = source.into_log()?;
    let r = log
        .window()
        .rev()
        .find(|r| r.op == Op::Close && r.node == Node::SESSION)?;
    let code = match r.f.get(Key::Code) {
        Some(Val::I(n)) => Some(*n),
        _ => None,
    };
    Some((
        r.f.get_word(Key::Out)?.to_string(),
        code,
        r.by.word().to_string(),
    ))
}

/// Whether every roster `Exited` row and the closed index agree, `out_of`
/// naming each reason's `out=`; `unindexed` are the closes that keep no
/// entry (pinned: they must have none). Returns how many agreed.
fn exits_agree(
    app: &App,
    out_of: fn(ExitReason) -> &'static str,
    unindexed: &[u64],
) -> Result<usize, String> {
    let rows: Vec<crate::session_store::RosterRecord> = app
        .store
        .read()
        .unwrap()
        .roster_since(0)
        .filter(|r| r.change == RosterChange::Exited)
        .cloned()
        .collect();
    let mut agreed = 0;
    for row in &rows {
        let got = index_close(app, row.local_id);
        if unindexed.contains(&row.local_id) {
            if got.is_some() {
                return Err(format!("{}: a keep=None close is indexed", row.local_id));
            }
            continue;
        }
        let want = (
            out_of(row.reason).to_string(),
            row.exit_code.map(i64::from),
            by_r1(&row.actor),
        );
        if got.as_ref() != Some(&want) {
            return Err(format!(
                "local {} ({:?} by {:?}): the roster says {want:?}, the index {got:?}",
                row.local_id, row.reason, row.actor
            ));
        }
        agreed += 1;
    }
    Ok(agreed)
}

/// THE ROSTER AND THE CLOSED INDEX AGREE (plan 5B-6, P2): every reason ×
/// every actor (a minted sid, and one no one minted, which R1 says as
/// `-`), each through the shipping retire under its close attribution,
/// half with an exit code: the roster's `Exited` row and the index's
/// session close say the same `out=`, `code=` and `by=`. A close that keeps
/// no entry (`keep=None`) is in the roster and not the index, pinned.
/// NEGATIVE CONTROL: the map with `ctl-close` and `ui-close` swapped fails.
#[test]
fn the_roster_and_the_closed_index_agree_on_every_exit() {
    let mut app = App::headless_for_test();
    let minted = aterm_session::SessionId::generate().as_str().to_string();
    assert!(aterm_session::is_minted_sid(&minted));
    let actors = [
        ExitActor::Sid(minted),
        ExitActor::Sid("s-driver".into()),
        ExitActor::Human,
        ExitActor::Ctl,
        ExitActor::Bridge,
        ExitActor::Unknown,
    ];
    let mut n = 0u64;
    let mut close = |app: &mut App, reason, actor: ExitActor, keep| {
        let local = app.next_session_id;
        app.push_stub_tab(WindowId(0), crate::stub_session(local));
        {
            let ctx = Arc::clone(&app.pool.get(local).expect("pooled").ctx);
            let mut g = ctx.timeline.lock().unwrap();
            if g.slog().log().is_empty() {
                let now = g.slog().now();
                g.slog_mut().open_session(None, None, now);
            }
        }
        n += 1;
        if n.is_multiple_of(2) {
            app.store
                .write()
                .unwrap()
                .note_exit_code(local, Some(i32::try_from(n).unwrap_or(1)));
        }
        let _closing = CloseAttribution::enter(reason, actor);
        app.retire_session_registration_keeping(local, keep);
        local
    };
    for reason in ExitReason::ALL {
        for actor in &actors {
            let _ = close(
                &mut app,
                reason,
                actor.clone(),
                Some(crate::session_log::ClosedKeep::Skeleton),
            );
        }
    }
    let unindexed = close(&mut app, ExitReason::UiClose, ExitActor::Human, None);
    let shipped = exits_agree(&app, ExitReason::as_str, &[unindexed]);
    assert_eq!(shipped, Ok(36), "{shipped:?}");
    // NEGATIVE CONTROLS: the swapped map, and the keep=None close indexed.
    let swapped = |r: ExitReason| match r {
        ExitReason::CtlClose => "ui-close",
        ExitReason::UiClose => "ctl-close",
        other => other.as_str(),
    };
    assert!(exits_agree(&app, swapped, &[unindexed]).is_err());
    assert!(exits_agree(&app, ExitReason::as_str, &[]).is_err());
}

// ---- the story ----------------------------------------------------------------

/// The `ctl story` word a told verb has, `None` for a presence verb.
fn told_word(verb: StoryVerb) -> Option<&'static str> {
    StoryVerb::TOLD_WORDS
        .into_iter()
        .find(|w| StoryVerb::parse_told(w) == Some(verb))
}

/// The seq of the record that opened `log`'s epoch (its first record
/// naming it), 0 for a log that names none.
fn epoch_start(log: &Log) -> u64 {
    let ep = u64::from(log.ep());
    log.window()
        .find(|r| r.f.get_u(Key::Epoch) == Some(ep))
        .map_or(0, |r| r.seq)
}

/// The `story` marks of `log` from `from` on, as `(word, by)`.
fn story_marks(log: &Log, from: u64) -> Vec<(String, String)> {
    log.window()
        .filter(|r| r.seq >= from && r.op == Op::Mark && r.k == Kind::Story)
        .map(|r| {
            (
                r.f.get_word(Key::Word).unwrap_or("-").to_string(),
                r.by.word().to_string(),
            )
        })
        .collect()
}

/// Whether presence's told points are `told` (each `(seq, verb, teller)`)
/// and the log's epoch holds exactly one `story` mark per told point by
/// its teller, in order (T17: no inequality).
fn story_agrees(
    told: &[(u64, StoryVerb, Actor)],
    points: &[(u64, StoryVerb)],
    log: &Log,
) -> Result<(), String> {
    let told_points: Vec<(u64, StoryVerb)> = points
        .iter()
        .copied()
        .filter(|(_, v)| told_word(*v).is_some())
        .collect();
    let want_points: Vec<(u64, StoryVerb)> = told.iter().map(|(s, v, _)| (*s, *v)).collect();
    if told_points != want_points {
        return Err(format!(
            "presence holds {told_points:?}, told {want_points:?}"
        ));
    }
    let marks = story_marks(log, epoch_start(log));
    let want: Vec<(String, String)> = told
        .iter()
        .map(|(_, v, by)| {
            (
                told_word(*v).unwrap_or("-").to_string(),
                by.word().to_string(),
            )
        })
        .collect();
    if marks != want {
        return Err(format!("the epoch's story marks {marks:?}, told {want:?}"));
    }
    Ok(())
}

/// EACH TOLD POINT IS ONE STORY MARK BY ITS TELLER (T17): five points told
/// through `App::tell_story_by` by four tellers (the harness, an Owner
/// client, a minted session, the person) are presence's told points, in
/// order, and the epoch's `story` marks, one each, `by=` the teller. Every
/// point is `chose`, the one told word left since the owner surfaces
/// retired the seven no process sent (`StoryVerb::TOLD_WORDS`), so the
/// tellers alone tell the points apart.
/// Pinned (T17's named difference): across an update the carried log keeps
/// the predecessor's marks before the epoch while the successor's story
/// starts over, so the agreement is the epoch's. NEGATIVE CONTROLS: a
/// story mark written twice fails; read past the epoch, the carried mark
/// has no point.
#[test]
fn each_told_point_is_one_story_mark_by_its_teller() {
    let mut app = App::headless_for_test();
    let session = app
        .front_terminal(WindowId(0))
        .expect("a front terminal")
        .session;
    let ctx = Arc::clone(&app.pool.get(session).expect("pooled").ctx);
    let minted = aterm_session::SessionId::generate();
    let sid = Actor::sid(ShortId::new(minted.as_str()).expect("a short id"));
    let mut told = Vec::new();
    for (verb, by) in [
        (StoryVerb::Chose, Actor::Harness),
        (StoryVerb::Chose, Actor::Ctl),
        (StoryVerb::Chose, sid),
        (StoryVerb::Chose, Actor::Human),
        (StoryVerb::Chose, Actor::Ctl),
    ] {
        let seq = app.tell_story_by(session, verb, by).expect("told");
        told.push((seq, verb, by));
    }
    let points: Vec<(u64, StoryVerb)> = app
        .presence
        .slot(session)
        .expect("a slot")
        .story_since(0)
        .map(|p| (p.seq, p.verb))
        .collect();
    {
        let g = ctx.timeline.lock().unwrap();
        assert_eq!(story_agrees(&told, &points, g.slog().log()), Ok(()));
    }
    // NEGATIVE CONTROL: the last point's mark again.
    {
        let mut g = ctx.timeline.lock().unwrap();
        let at = g.slog().now();
        g.slog_mut().observe_story("chose", Actor::Ctl, at);
        assert!(story_agrees(&told, &points, g.slog().log()).is_err());
    }
    // THE RESET ACROSS AN UPDATE, pinned: a predecessor's told mark is
    // carried, and stands before the successor's epoch with no point.
    let (h, store) = rig(60);
    {
        let mut g = h.ctx.timeline.lock().unwrap();
        let at = g.slog().now();
        g.slog_mut().observe_story("chose", Actor::Harness, at);
    }
    let (next, _next_store, _hand) = hand_over(&h, &store.read().unwrap(), 1_000, true);
    let g = next.ctx.timeline.lock().unwrap();
    let log = g.slog().log();
    assert_eq!(log.ep(), 2);
    assert_eq!(story_agrees(&[], &[], log), Ok(()));
    assert_eq!(
        story_marks(log, 0),
        [("chose".to_string(), "harness".to_string())],
        "the carried mark, before the epoch"
    );
}
