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
//!   set less `human` and `harness-act`) into the ring's `(kind, payload)`,
//!   and over a scripted session the two are equal, row for record, in
//!   order (ids and clocks aside): the WHOLE payload, so an exact pair has
//!   no field the comparison does not see. `harness-act` pairs through its
//!   word map ([`aterm_agent::harness::acts::self_claim_act`]): the ring's
//!   row `act=<act> holder=<h>` is compared, row for record, as `word=<w>
//!   rule=<act, @ .>`, and a row whose act has no word pairs with no record,
//!   counted; the holder (the claim's lease holder; the mark's `by=` is the
//!   connection's) is no field of the mark, so the kind is a named
//!   difference, `Lossy` for `holder`, not an exact pair. Every other kind
//!   of plan §0.1 is a row of [`KNOWN_DIFFERENCES`]: no record, a record
//!   without the named fields, or a record on another trigger, each driven
//!   through the row's own writer. `human` is no longer an exact pair:
//!   since 5D-4 (T39) the log holds every ring row and, after delivered
//!   mail, the person's next input that reached the program too
//!   ([`SUPERSET`], log ⊇ ring (post-mail marks)); a burst's mark no such
//!   input came with says `reached=0` and is still the row's record.
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
//! NEGATIVE CONTROLS: a projection with `fp=` dropped fails; an exit map
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
/// `human` left for [`SUPERSET`] at 5D-4; `harness-act`, compared through
/// its word map, is a named difference for the `holder` its mark does not
/// carry, [`KNOWN_DIFFERENCES`]).
const EXACT: [&str; 4] = [
    "agent-change",
    crate::session_status::STATUS_CHANGE,
    "inbox",
    "task-change",
];

/// The ring kinds whose every row the log holds, in order, with records of
/// its own beside them: log ⊇ ring (post-mail marks). `human` since 5D-4
/// (T39): the ring keeps the 30 s burst rule, and the log marks the person's
/// first input that reached the program after a `mail dir=in` mark as well
/// (a burst's own mark keeps the row whatever its first gesture reached,
/// `reached=0` when nothing did).
const SUPERSET: [&str; 1] = ["human"];

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

/// One record as the ring row it pairs with, if it is an exact pair's.
fn project(r: &Rec) -> Option<(String, String)> {
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
        Kind::Human => ("human", String::new()),
        // A self-claim's act, through the word map ([`act_row`]).
        Kind::Act => (
            crate::session_timeline::HARNESS_ACT,
            format!("word={} rule={}", text(f, Key::Word), text(f, Key::Rule)),
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
        _ => return None,
    };
    Some((row.0.to_string(), row.1))
}

/// The log's records of the exact pairs as the ring's `(kind, payload)`,
/// oldest first.
pub(crate) fn overlap_rows(log: &Log) -> Vec<(String, String)> {
    overlap_rows_without(log, None)
}

/// [`overlap_rows`] with `drop` taken out of every record first (the
/// negative control's lossy projection).
fn overlap_rows_without(log: &Log, drop: Option<Key>) -> Vec<(String, String)> {
    log.window()
        .filter_map(|r| {
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
        .map(|e| (e.kind.to_string(), e.payload.clone()))
        .collect()
}

/// The ring's `harness-act` rows through the word map ([`act_row`]),
/// oldest first: the rows whose act has a word, as their marks say them.
fn act_rows(tl: &SessionTimeline) -> Vec<(String, String)> {
    tl.since(None)
        .filter(|e| e.kind == crate::session_timeline::HARNESS_ACT)
        .filter_map(|e| act_row(&e.payload).map(|p| (e.kind.to_string(), p)))
        .collect()
}

/// A `harness-act` row's payload (`act=<act> holder=<h>`) through the word
/// map, as its record says it: `word=<w> rule=<r>`, or `None` for an act
/// the log has no word for (its row stands alone).
fn act_row(payload: &str) -> Option<String> {
    let act = value(payload, "act")?;
    let (word, rule) = aterm_agent::harness::acts::self_claim_act(act)?;
    Some(format!("word={word} rule={rule}"))
}

/// THE EXACT PAIRS AGREE (T19): over one scripted session, through each
/// row's production writer — the status sweep's published transitions (a
/// failing command, then a prompt), the agent publication (each verdict,
/// one with no screen stamp), a person's burst (no exact pair since 5D-4:
/// [`SUPERSET`]), two bridge deliveries (and a redelivery, which
/// writes neither) and a worker's task moved through `task mark` and the
/// supervisor's attention (and a repeated mark, which writes neither) — the
/// log's records of the exact pairs, projected, are the ring's rows of those
/// kinds, in order. NEGATIVE CONTROL: the same projection with `fp=` dropped
/// differs.
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
    let subs = crate::subscribe::new_registry();
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
            &subs,
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
    for kind in EXACT {
        assert!(
            ring.iter().any(|(k, _)| k == kind),
            "the script wrote no {kind} row: {ring:?}"
        );
    }
    let log = only(overlap_rows(g.slog().log()), &EXACT);
    assert_eq!(log, ring, "the log's exact pairs against the ring's rows");
    // `harness-act` through its word map, row for record (its `holder` the
    // named difference).
    const ACT: [&str; 1] = [crate::session_timeline::HARNESS_ACT];
    let acts = act_rows(&g);
    assert_eq!(acts.len(), 2, "two worded acts: {acts:?}");
    assert_eq!(
        only(overlap_rows(g.slog().log()), &ACT),
        acts,
        "the log's act marks against the ring's acts"
    );
    // The act with no word is a row and no record, counted.
    let unworded = g
        .since(None)
        .filter(|e| e.kind == crate::session_timeline::HARNESS_ACT)
        .filter(|e| act_row(&e.payload).is_none())
        .count();
    assert_eq!(unworded, 1, "finished@v1 writes no act mark");
    // NEGATIVE CONTROLS: a lossy projection is caught, for the agent's
    // `fp=`, the task's `rev=` and the act's `rule=` alike.
    for key in [Key::Fp, Key::Rev] {
        assert_ne!(
            only(overlap_rows_without(g.slog().log(), Some(key)), &EXACT),
            ring,
            "a projection with {key:?} dropped agrees"
        );
    }
    assert_ne!(
        only(overlap_rows_without(g.slog().log(), Some(Key::Rule)), &ACT),
        acts,
        "an act projection with rule= dropped agrees"
    );
}

/// LOG ⊇ RING (post-mail marks), for `human` (ruling T39): a person's
/// burst, a delivered mail, then the same person typing on inside that
/// 30 s burst, twice: the ring keeps one `human` row (the burst's), the log
/// holds it and one more `human` mark, the first input after the mail; the
/// input after that writes neither. Every ring row is a log record, in
/// order. NEGATIVE CONTROL (by scratch revert): without the latch, the
/// second mark is missing.
#[test]
fn the_log_holds_every_human_row_and_the_first_input_after_mail() {
    let (h, store) = rig(61);
    let _hand = super::super::tests::hand_clock(&h.ctx.timeline);
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
    let g = ctx.timeline.lock().unwrap();
    let ring = ring_rows(&g, &SUPERSET);
    let marks: Vec<(String, String)> = g
        .slog()
        .log()
        .window()
        .filter(|r| r.op == Op::Mark && matches!(r.k, Kind::Human | Kind::Mail))
        .map(|r| (r.k.word().to_string(), String::new()))
        .collect();
    assert_eq!(ring.len(), 1, "the ring keeps the burst rule: {ring:?}");
    assert_eq!(
        marks,
        [
            ("human".to_string(), String::new()),
            ("mail".to_string(), String::new()),
            ("human".to_string(), String::new()),
        ],
        "the person's first input after the mail is a human mark"
    );
    let log = only(overlap_rows(g.slog().log()), &SUPERSET);
    let mut rest = log.iter();
    assert!(
        ring.iter().all(|row| rest.any(|rec| rec == row)),
        "a ring row the log does not hold: {ring:?} against {log:?}"
    );
    assert!(log.len() > ring.len(), "{log:?}");
}

/// How a timeline kind and the log differ (T19's named differences).
#[derive(Clone, Copy, Debug)]
enum Diff {
    /// The row's writer writes no record in its hold.
    NoRecord,
    /// The writer writes a record (`<op> <kind>`), and each named field of
    /// the row is not the record's (absent, or another value).
    Lossy(&'static str, &'static [&'static str]),
    /// The record (`<op> <kind>`) is written on another trigger or dedupe
    /// rule: the driven call writes the row and no such record.
    Trigger(&'static str),
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
    /// The row's own writer, driven; `None` for one no unit test reaches
    /// without the live link's process-wide state.
    drive: Option<fn() -> Seen>,
}

/// Every §0.1 kind that is no exact pair, and how it differs.
const KNOWN_DIFFERENCES: &[Known] = &[
    Known {
        row: "spawned",
        diff: Diff::Lossy("open spawn", &["state"]),
        why: "the session open names no state word (§0.1)",
        until: None,
        drive: Some(drive_spawned),
    },
    Known {
        row: "closing",
        diff: Diff::Lossy("close spawn", &["by"]),
        why: "3A R1: a sid no one minted is no session's, so the close says by=-",
        until: None,
        drive: Some(drive_closing),
    },
    Known {
        row: "state-change",
        diff: Diff::NoRecord,
        why: "T21: state stays ring-only",
        until: None,
        drive: Some(drive_state),
    },
    Known {
        row: "title-change",
        diff: Diff::Trigger("mark title"),
        why: "two triggers: the row is set_title's raw front-tab title, the mark the drift \
              flush's informative title on every session",
        until: None,
        drive: Some(drive_title),
    },
    Known {
        row: "cwd-change",
        diff: Diff::Trigger("mark cwd"),
        why: "two dedupe rules: the ring's last cwd starts empty in a successor, the mark's \
              fingerprint is carried",
        until: None,
        drive: Some(drive_cwd),
    },
    Known {
        row: "meta-change",
        diff: Diff::Lossy("mark meta", &["value"]),
        why: "an attention text is never stored: the mark says its fp= (DESIGN:885)",
        until: None,
        drive: Some(drive_meta),
    },
    Known {
        row: "render",
        diff: Diff::Lossy("mark render", &["run", "displaced", "at"]),
        why: "the mark is the state word alone, deduped by state",
        until: None,
        drive: Some(drive_render),
    },
    Known {
        row: "modes-restored",
        diff: Diff::Lossy("mark modes", &["reverted", "bytes"]),
        why: "the mark counts the modes (n=)",
        until: None,
        drive: Some(drive_modes),
    },
    Known {
        row: "in-doubt",
        diff: Diff::Lossy("mark in-doubt", &["producer", "seq", "reply"]),
        why: "the mark names the verb alone",
        until: None,
        drive: Some(drive_in_doubt),
    },
    Known {
        row: "handoff",
        diff: Diff::Lossy("mark handoff", &["carried", "from_build"]),
        why: "the ring is never carried (carried=0); the log's mark counts what it carried \
              and names no build",
        until: None,
        drive: Some(drive_handoff),
    },
    Known {
        row: crate::session_timeline::HARNESS_ACT,
        diff: Diff::Lossy("mark act", &["holder"]),
        why: "T19 pairs it through its word map (self_claim_act): word= and rule= are compared \
              row for record (the_ring_and_the_log_agree_on_the_exact_pairs); the mark names no \
              holder, its by= being the connection's",
        until: None,
        drive: Some(drive_act),
    },
    Known {
        row: "program-state",
        diff: Diff::NoRecord,
        why: "T21: program stays ring-only",
        until: None,
        drive: Some(drive_program),
    },
    Known {
        row: crate::harness_policy_verb::HARNESS_POLICY_EVENT,
        diff: Diff::NoRecord,
        why: "T21: policy stays ring-only",
        until: None,
        drive: Some(drive_policy),
    },
    Known {
        row: "inbox-seen",
        diff: Diff::Trigger("mark mail-seen"),
        why: "the mark is written only when the watermark or a verdict moved",
        until: None,
        drive: Some(drive_inbox_seen),
    },
    Known {
        row: "post",
        diff: Diff::Lossy("mark mail", &["dl", "key"]),
        why: "mail dir=out keeps no post options (§0.1)",
        until: None,
        drive: Some(drive_post),
    },
    Known {
        row: "post-landed",
        diff: Diff::Trigger("mark mail-landed"),
        why: "a dead post (off=-) landed nowhere: no mark",
        until: None,
        drive: Some(drive_landed),
    },
    Known {
        row: "hold",
        diff: Diff::Lossy("mark hold", &["reason"]),
        why: "the mark is the hold's state alone: a reason is the owner's text",
        until: None,
        drive: Some(drive_hold),
    },
    Known {
        row: "fetch",
        diff: Diff::NoRecord,
        why: "dropped by design (§4.1, DESIGN:382); its writer parks only under a live link",
        until: None,
        drive: None,
    },
    Known {
        row: "topic",
        diff: Diff::NoRecord,
        why: "T21: topic stays ring-only",
        until: None,
        drive: Some(drive_topic),
    },
    Known {
        row: "hold-advisory",
        diff: Diff::NoRecord,
        why: "T21: advisory stays ring-only; its writer runs only on the live link's loss",
        until: None,
        drive: None,
    },
];

/// What one driven call wrote: the ring's rows, and the log's records at
/// the content tier.
#[derive(Clone, Debug, Default)]
struct Seen {
    rows: Vec<(String, String)>,
    recs: Vec<String>,
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
            .map(|e| (e.kind.to_string(), e.payload.clone()))
            .collect(),
        recs: g
            .slog()
            .log()
            .window()
            .filter(|r| r.seq > seq)
            .map(|r| aterm_slog::encode(r, Tier::Content).expect("fits"))
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
    let Some(row) = rows.first() else {
        return Err(format!(
            "{}: the driver wrote no row: {:?}",
            k.row, seen.rows
        ));
    };
    let of =
        |sel: &str| -> Vec<&String> { seen.recs.iter().filter(|l| op_kind(l) == sel).collect() };
    match k.diff {
        Diff::NoRecord if seen.recs.is_empty() => Ok(()),
        Diff::NoRecord => Err(format!(
            "{}: closed? its writer now writes {:?}",
            k.row, seen.recs
        )),
        Diff::Lossy(sel, fields) => {
            let recs = of(sel);
            if recs.is_empty() {
                return Err(format!("{}: no {sel} record: {:?}", k.row, seen.recs));
            }
            for f in fields {
                let Some(want) = value(row, f) else {
                    return Err(format!("{}: the row has no {f}=: {row}", k.row));
                };
                if let Some(rec) = recs.iter().find(|l| value(l, f) == Some(want)) {
                    return Err(format!("{}: closed? {rec} carries {f}={want}", k.row));
                }
            }
            Ok(())
        }
        Diff::Trigger(sel) if of(sel).is_empty() => Ok(()),
        Diff::Trigger(sel) => Err(format!(
            "{}: closed? the call wrote {:?} beside the row",
            k.row,
            of(sel)
        )),
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

fn drive_state() -> Seen {
    let (h, store) = rig(43);
    during(&h.ctx.timeline, || {
        store.write().unwrap().set_state(43, SessionState::Exited);
    })
}

fn drive_title() -> Seen {
    let (h, store) = rig(44);
    during(&h.ctx.timeline, || {
        store.write().unwrap().set_title(44, "vim build");
    })
}

fn drive_cwd() -> Seen {
    let (h, store) = rig(45);
    h.ctx
        .timeline
        .lock()
        .unwrap()
        .record_cwd_change(Some("/tmp/a b"));
    let (next, _next_store, _hand) = hand_over(&h, &store.read().unwrap(), 1_000, true);
    during(&next.ctx.timeline, || {
        next.ctx
            .timeline
            .lock()
            .unwrap()
            .record_cwd_change(Some("/tmp/a b"));
    })
}

fn drive_meta() -> Seen {
    let (h, store) = rig(46);
    during(&h.ctx.timeline, || {
        let reply = crate::control::meta_line_for_test(
            &store,
            &h.term,
            h.local_id,
            &h.ctx,
            "set attention look here",
        );
        assert_eq!(reply, "OK\n");
    })
}

fn drive_render() -> Seen {
    let (h, _store) = rig(48);
    during(&h.ctx.timeline, || {
        let _ = h.ctx.timeline.lock().unwrap().record_render(&[
            crate::resize_ledger::Transition::Risk {
                run: 1,
                displaced: 2,
                net_zero: true,
                at_ms: 3,
            },
        ]);
    })
}

fn drive_modes() -> Seen {
    let (h, _store) = rig(49);
    during(&h.ctx.timeline, || {
        crate::manual_reset::record(
            49,
            &h.ctx.timeline,
            crate::manual_reset::ResetSource::Menu,
            &crate::manual_reset::ResetReport {
                reverted: vec!["mouse", "paste"],
                bytes: 12,
            },
        );
    })
}

fn drive_in_doubt() -> Seen {
    let (h, _store) = rig(50);
    let key = crate::pty_idem::parse_key(&format!("{}:3:9", h.ctx.nonce.to_hex()), h.ctx.nonce)
        .expect("a key");
    during(&h.ctx.timeline, || {
        let _ = crate::pty_idem::record_in_doubt(&h.ctx, "send", key, "ERR write failed\n");
    })
}

fn drive_handoff() -> Seen {
    let (h, store) = rig(51);
    h.ctx
        .timeline
        .lock()
        .unwrap()
        .record_cwd_change(Some("/tmp"));
    let (next, _next_store, _hand) = hand_over(&h, &store.read().unwrap(), 1_000, true);
    seen_since(&next.ctx.timeline, None, 0)
}

fn drive_act() -> Seen {
    let (h, _store) = rig(59);
    during(&h.ctx.timeline, || {
        let _ = h.ctx.timeline.lock().unwrap().note_harness_act(
            "approve-all@v1",
            "codex-alab@1",
            Actor::Ctl,
        );
    })
}

fn drive_program() -> Seen {
    let (h, _store) = rig(53);
    during(&h.ctx.timeline, || {
        let _ = h.ctx.timeline.lock().unwrap().note_program_state(None);
    })
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

fn drive_inbox_seen() -> Seen {
    let (h, _store) = delivered(54);
    let seen = crate::fabric::cmd_inbox_seen_waking(&h.ctx, "1 handled", &|| {});
    assert!(seen.starts_with("OK seen=1"), "{seen}");
    during(&h.ctx.timeline, || {
        let _ = crate::fabric::cmd_inbox_seen_waking(&h.ctx, "1 handled", &|| {});
    })
}

/// A post from `h` (post id 1), queued whether or not a bridge is up.
fn post(h: &SessionHandle) {
    let reply = crate::fabric::cmd_post_waking(
        &h.ctx,
        "to=@s-peer kind=ask dl=60000 key=k1 --wait=0 SENTINEL-POST",
        None,
        &|| {},
    );
    assert!(
        reply.starts_with("OK 1") || reply.contains(" id=1 "),
        "{reply}"
    );
}

fn drive_post() -> Seen {
    let (h, _store) = rig(55);
    during(&h.ctx.timeline, || post(&h))
}

fn drive_landed() -> Seen {
    let (h, store) = rig(56);
    post(&h);
    let line = format!("{} 1 off=-", h.ctx.self_id.as_str());
    during(&h.ctx.timeline, || {
        let reply = crate::fabric::cmd_outbox_sent_waking(&store, &line, &|_| {});
        assert!(reply.starts_with("OK"), "{reply}");
    })
}

fn drive_hold() -> Seen {
    let (h, store) = rig(57);
    let line = format!("{} on reason=demo", h.ctx.self_id.as_str());
    during(&h.ctx.timeline, || {
        let reply = crate::fabric::cmd_hold_waking(
            &store,
            &line,
            crate::fabric::HoldIssuer::Owner,
            &|_| {},
        );
        assert!(reply.starts_with("OK hold="), "{reply}");
    })
}

fn drive_topic() -> Seen {
    let (h, _store) = rig(58);
    during(&h.ctx.timeline, || {
        assert_eq!(
            h.ctx.fabric.topic_add("news", "head", &h.ctx.timeline),
            Ok(true)
        );
    })
}

/// THE TABLE IS WHOLE: every kind of §0.1 is an exact pair, a superset
/// kind or a named difference, once; the kinds are the ones 5B-5's script writes (one row
/// of each, the goldens' script), and every fabric kind the code lists
/// ([`crate::fabric::FABRIC_EVENT_KINDS`]) is among them. A kind added to
/// the ring without a row here fails.
#[test]
fn every_timeline_kind_is_an_exact_pair_or_a_named_difference() {
    let mut named: Vec<&str> = EXACT
        .iter()
        .chain(&SUPERSET)
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
/// step that closes one (a record where there was none, a field the record
/// now carries, a record on the row's own trigger) turns this red, and its
/// row moves to [`EXACT`] in the same commit. The two writers no unit test
/// reaches are named. NEGATIVE CONTROL: each kind of difference, closed on
/// what its driver wrote, fails.
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
    assert_eq!(undriven, ["fetch", "hold-advisory"]);
    // NEGATIVE CONTROLS: each difference closed.
    let known = |row: &str| {
        KNOWN_DIFFERENCES
            .iter()
            .find(|k| k.row == row)
            .expect("named")
    };
    let mut program = seen["program-state"].clone();
    program
        .recs
        .push("9 mark - unknown state=working src=engine conf=claim t=1".into());
    assert!(
        holds(known("program-state"), &program).is_err(),
        "a program record"
    );
    let mut hold = seen["hold"].clone();
    for l in &mut hold.recs {
        if op_kind(l) == "mark hold" {
            *l = l.replace(" state=on ", " state=on reason=demo ");
        }
    }
    assert!(
        holds(known("hold"), &hold).is_err(),
        "a hold with its reason"
    );
    let mut title = seen["title-change"].clone();
    title
        .recs
        .push("9 mark - title title=vim%20build src=engine conf=exact t=1".into());
    assert!(
        holds(known("title-change"), &title).is_err(),
        "a title mark"
    );
    let mut act = seen[crate::session_timeline::HARNESS_ACT].clone();
    for l in &mut act.recs {
        if op_kind(l) == "mark act" {
            *l = l.replace(" word=", " holder=codex-alab@1 word=");
        }
    }
    assert!(
        holds(known(crate::session_timeline::HARNESS_ACT), &act).is_err(),
        "an act mark with its holder"
    );
    let none = Seen::default();
    assert!(holds(known("state-change"), &none).is_err(), "no row");
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
                g.slog_mut().open_session(None, now);
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
