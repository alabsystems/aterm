// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Regression repros across the engine: each test states the behaviour it
//! pins.

use crate::carry::{CarriedMessage, Carry};
use crate::center::{EchoKind, MessageCenter, Outcome};
use crate::glass::{Links, Presentation, RowKind, RowSpec, layout_row, layout_row_with};
use crate::log::{LogLine, LogRecord, LogState, MessageLog, Retired, Shelf};
use crate::model::{
    Amount, Glyph, Hold, Intent, Load, Loads, Message, MessageId, Meter, Origin, Restatement,
    Severity, Tag, Unit, UpgradeWord, WallStamp, tags,
};
use crate::page::{Host, MessageView, MessagesState, description};
use crate::presence::View;
use crate::progress::{Eta, ProgressTrack};
use crate::text::{char_width, shape_detail, truncate};
use crate::waits::{WaitEnd, lines_words, paste_row, rewrap_row, rewrap_stats};
use crate::wire::{NoticeRequest, ReadQuery, message_rows, parse_read_args};
use crate::words::{day_heading, local_day};
use crate::{Duration, Instant, LOG_CAP, MAX_ACTIONS, PROGRESS_GRACE, SHRINK_QUIET, STALE_TAILED};

fn t0() -> Instant {
    Instant::now()
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

fn stamp(unix_ms: u64) -> WallStamp {
    WallStamp { unix_ms }
}

fn mid(raw: u64) -> MessageId {
    MessageId::from_raw(raw).expect("a nonzero id")
}

fn fresh(now: Instant) -> MessageCenter {
    MessageCenter::new(MessageLog::empty(), now)
}

fn info(title: &str) -> Message {
    Message::new(tags::SYSTEM, Severity::Info, title)
}

/// A live indeterminate row (the comet).
fn busy(title: &str) -> Message {
    Message::new(tags::PACKAGES, Severity::Info, title)
        .in_flight()
        .hold(Hold::Live {
            stale_after: STALE_TAILED,
        })
}

fn glass_ids(c: &MessageCenter) -> Vec<MessageId> {
    c.on_glass().map(|l| l.id).collect()
}

/// A file's Posted line for `id`.
fn posted(id: u64, unix_ms: u64, tag: Tag, title: &str) -> LogLine {
    LogLine::Posted(LogRecord::from_posted(
        mid(id),
        stamp(unix_ms),
        &Message::new(tag, Severity::Info, title),
    ))
}

/// A file's Retired line for `id`.
fn retired(id: u64, how: Retired, unix_ms: u64, title: &str) -> LogLine {
    LogLine::Retired {
        id: mid(id),
        how,
        unix_ms,
        title: title.into(),
        detail: Vec::new(),
        repeats: 1,
        mark: None,
    }
}

fn drained(c: &mut MessageCenter) -> Vec<LogLine> {
    c.drain_shelved_for_persist()
        .into_iter()
        .map(|(l, _)| l)
        .collect()
}

// ---- center ---------------------------------------------------------------

/// A row that leaves the glass without retiring is demoted, and its patience
/// re-bases so it re-anchors on its return. A host heartbeat restating the
/// unchanged hold (`messages_host::restatement_of`) must not move that
/// patience back to the post.
#[test]
fn a_hold_restate_keeps_a_demoted_rows_patience() {
    let now = t0();
    let mut c = fresh(now);
    let a = c.post(info("A"), stamp(1), now).id;
    c.commit_rows(now, 1);
    assert_eq!(glass_ids(&c), vec![a]);
    let w = c
        .post(
            Message::new(tags::SYSTEM, Severity::Warn, "W"),
            stamp(2),
            now + secs(20),
        )
        .id;
    c.commit_rows(now + secs(20), 1);
    assert_eq!(glass_ids(&c), vec![w], "the warn took the one slot");
    assert_eq!(
        c.live(a).unwrap().patience_at,
        now + secs(80),
        "the demote re-based A's patience from t0+20s"
    );
    assert!(c.restate(
        a,
        Restatement {
            hold: Some(Hold::Default),
            ..Restatement::default()
        },
        now + secs(25),
    ));
    let patience = c.live(a).unwrap().patience_at;
    let settled = c.settle(now + secs(60), true);
    assert!(
        settled.retired.iter().all(|(id, _)| *id != a),
        "A (shown 20 s, demoted at t0+20s) retired {:?} at t0+60s: the same-hold restate \
         moved its patience to t0+{:?}, before the demote's t0+80s",
        settled.retired,
        patience.saturating_duration_since(now)
    );
    c.settle(now + secs(65), true);
    c.commit_rows(now + secs(65), 1);
    assert_eq!(glass_ids(&c), vec![a], "A returns when W folds");
}

/// The quiet shrink never cuts a Complete echo that outlasts `SHRINK_QUIET`
/// and hides the row that survives it, nor restarts that row's hold.
#[test]
fn the_quiet_shrink_never_hides_the_row_beside_a_complete_echo() {
    let start = t0();
    let mut c = fresh(start);
    let a = c.post(busy("Checking tools"), stamp(1), start).id;
    let b = c.post(info("Font applied"), stamp(2), start).id;
    c.commit_rows(start, 3);
    assert_eq!(glass_ids(&c), vec![a, b], "the busy row ranks above");
    let fold_b = c.live(b).unwrap().fold_at;
    let t1 = start + secs(5);
    assert!(c.resolve(a, Outcome::Ok, t1));
    c.commit_rows(t1, 3);
    let until = c.echoes()[0].until;
    assert!(
        until > t1 + SHRINK_QUIET,
        "premise: the Complete echo runs {:?}, past the quiet {SHRINK_QUIET:?}",
        until - t1
    );
    let mut t = t1;
    while t <= t1 + ms(2000) {
        c.settle(t, true);
        c.commit_rows(t, 3);
        assert_eq!(
            glass_ids(&c),
            vec![b],
            "at resolve+{:?} the surviving row left the glass (echoes {:?})",
            t - t1,
            c.echoes().iter().map(|e| e.slot).collect::<Vec<_>>()
        );
        assert_eq!(
            c.live(b).unwrap().fold_at,
            fold_b,
            "at resolve+{:?} the surviving row's hold restarted",
            t - t1
        );
        t += ms(50);
    }
}

/// `revision()` bumps on every observable change: a reveal that moves rows
/// on and off the glass changes every `on_glass` the page projects, so it
/// moves the revision.
#[test]
fn a_reveal_that_moves_the_glass_bumps_the_revision() {
    let start = t0();
    let mut c = fresh(start);
    let x = c.post(info("X"), stamp(1), start).id;
    let y = c.post(info("Y"), stamp(2), start).id;
    let z = c.post(info("Z"), stamp(3), start).id;
    c.commit_rows(start, 3);
    assert_eq!(glass_ids(&c), vec![x, y, z]);
    let t = start + secs(1);
    let r = c
        .post(
            busy("Fetching sources").reveal_after(PROGRESS_GRACE),
            stamp(4),
            t,
        )
        .id;
    c.commit_rows(t, 3);
    assert_eq!(c.glass_position(r), None, "inside its grace");
    let rev0 = c.revision();
    let glass0 = glass_ids(&c);
    let at = t + PROGRESS_GRACE;
    c.settle(at, true);
    c.commit_rows(at, 3);
    assert!(c.glass_position(r).is_some(), "revealed onto the glass");
    assert_ne!(glass_ids(&c), glass0, "the glass moved");
    assert_ne!(
        c.revision(),
        rev0,
        "the glass went {glass0:?} -> {:?} and the revision did not move",
        glass_ids(&c)
    );
}

/// The host's contract (`messages_host.rs` `App::seed_carried_messages`,
/// `PARKED_PARENT_ID_RESERVE`): the parked parent keeps minting in
/// `[next_id, next_id + reserve)` until Commit and this process mints above
/// it. A squatter the seed moves off a carried number lands above the band,
/// never on the parent's next id. The host reserves before the seed; this
/// test replays that order, and the host's
/// `a_record_the_seed_moves_off_a_carried_id_lands_above_the_parked_band`
/// pins the order itself.
#[test]
fn a_squatter_moved_by_the_seed_lands_above_the_parents_band() {
    const RESERVE: u64 = 1000;
    let now = t0();
    let mut log = MessageLog::empty();
    log.replay_all([
        posted(5, 1, tags::RENDER, "Other writer"),
        retired(5, Retired::Folded, 2, "Other writer"),
    ]);
    let mut c = MessageCenter::new(log, now);
    let carry = Carry {
        next_id: 6,
        live: vec![CarriedMessage {
            id: 5,
            unix_ms: 3,
            tag: "system".into(),
            severity: "warn".into(),
            glyph: '\u{26a0}',
            title: "Parent row".into(),
            detail: Vec::new(),
            actions: Vec::new(),
            hold: Hold::Default.encode(),
            key: None,
            fill_permille: None,
            stats: String::new(),
            busy: false,
            on_glass: true,
            excerpt: true,
            finished: None,
            retrospective: false,
            wire: false,
            repeats: 1,
        }],
    };
    // The host's order: the parked parent's reserve, then the seed.
    c.raise_next_id(carry.next_id + RESERVE);
    c.seed_carried(&carry, stamp(4), now);
    assert_eq!(
        c.live(mid(5)).map(|l| l.msg.title.as_str()),
        Some("Parent row")
    );
    let squatter = c
        .log()
        .records()
        .find(|r| r.title == "Other writer")
        .expect("the other writer's record stays");
    assert!(
        squatter.id.raw() >= carry.next_id + RESERVE,
        "the squatter moved to {}, inside the parked parent's band [{}, {})",
        squatter.id,
        carry.next_id,
        carry.next_id + RESERVE
    );
}

// ---- log --------------------------------------------------------------------

/// `ReadQuery::select` returns records ascending by id, with `since=` the
/// oldest ones above it, so a reader paging with the last id it saw never
/// skips one: after a replay collision mints a fresh id above the file's
/// top, paging one at a time from `since=2` still delivers 3, 4 and 5.
#[test]
fn paging_after_a_replay_collision_skips_no_record() {
    let mut log = MessageLog::empty();
    log.replay_all([
        posted(1, 1, tags::SYSTEM, "One"),
        posted(2, 2, tags::SYSTEM, "Two"),
        posted(2, 3, tags::RENDER, "Another writer's two"),
        posted(3, 4, tags::SYSTEM, "Three"),
        posted(4, 5, tags::SYSTEM, "Four"),
    ]);
    let ring: Vec<u64> = log.records().map(|r| r.id.raw()).collect();
    let all: Vec<u64> = parse_read_args("since=2")
        .unwrap()
        .select(&log)
        .iter()
        .map(|r| r.id.raw())
        .collect();
    let mut pages = Vec::new();
    let mut since = 2;
    while pages.len() < 10 {
        let q = parse_read_args(&format!("1 since={since}")).unwrap();
        let Some(rec) = q.select(&log).first().copied() else {
            break;
        };
        pages.push(rec.id.raw());
        since = rec.id.raw();
    }
    assert_eq!(
        pages,
        vec![3, 4, 5],
        "paging n=1 from since=2 (ring order {ring:?}, since=2 selects {all:?})"
    );
}

/// `merge_reloaded`: a record the ring holds that the boot read as `Stale`
/// takes the ending the file has, one post one record; a collision-remapped
/// record whose title changed when it retired does not come back as a second
/// entry.
#[test]
fn a_remapped_record_retired_after_the_boot_merges_into_one() {
    let boot_lines = || {
        vec![
            posted(3, 100, tags::SYSTEM, "Other"),
            retired(3, Retired::Folded, 150, "Other"),
            posted(3, 200, tags::TOOLCHAIN, "Updating ALab tools"),
        ]
    };
    let mut ring = MessageLog::empty();
    let mut boot = boot_lines();
    boot.push(posted(4, 300, tags::SYSTEM, "Four"));
    ring.replay_all(boot);
    // After the boot the parent wrote 6 and retired its 3; sorted by id.
    let mut disk_lines = boot_lines();
    disk_lines.push(retired(
        3,
        Retired::Resolved(Outcome::Ok),
        500,
        "Updated ALab tools",
    ));
    disk_lines.push(posted(4, 300, tags::SYSTEM, "Four"));
    disk_lines.push(posted(6, 400, tags::SYSTEM, "Six"));
    let mut disk = MessageLog::empty();
    disk.replay_all(disk_lines);
    ring.merge_reloaded(&disk);
    let parents: Vec<(u64, String, LogState)> = ring
        .records()
        .filter(|r| r.stamp.unix_ms == 200)
        .map(|r| (r.id.raw(), r.title.clone(), r.state.clone()))
        .collect();
    assert_eq!(
        parents.len(),
        1,
        "the parent's one post is {} records: {parents:?}",
        parents.len()
    );
    assert_eq!(
        parents[0].2,
        LogState::Retired(Retired::Resolved(Outcome::Ok)),
        "{parents:?}"
    );
}

/// A line cut short by a crash mid-append fails to decode instead of reading
/// as a shorter record: a torn Posted line with the next append glued on
/// never decodes as a record mixing the two.
#[test]
fn a_torn_line_glued_to_the_next_never_decodes() {
    let a = LogLine::Posted(LogRecord::from_posted(
        mid(7),
        stamp(1000),
        &Message::new(tags::SYSTEM, Severity::Warn, "Build failed"),
    ));
    let b = LogLine::Posted(LogRecord::from_posted(
        mid(20),
        stamp(2000),
        &Message::new(tags::SYSTEM, Severity::Info, "Hello")
            .line("x")
            .key("wire.k"),
    ));
    let ea = a.encode();
    let cut = ea.find("title=Build fai").unwrap() + "title=Build fai".len();
    let glued = format!("{}{}", &ea[..cut], b.encode());
    let decoded = LogLine::decode(&glued);
    assert!(
        decoded.is_err(),
        "a torn line glued to the next decoded as {decoded:?}"
    );
}

/// `replay_all` raises past every id in the file first, so a collision's
/// fresh id is above the file's own and never renumbers a later record; one
/// ceiling id in the file (a corrupt or hand-edited line) must not switch
/// that off.
#[test]
fn a_ceiling_id_never_brings_back_the_renumbering_cascade() {
    let mut log = MessageLog::empty();
    log.replay_all([
        posted(1, 1, tags::SYSTEM, "One"),
        posted(2, 2, tags::SYSTEM, "Two"),
        posted(2, 3, tags::RENDER, "Another writer's two"),
        posted(3, 4, tags::SYSTEM, "Three"),
        retired(3, Retired::Folded, 5, "Three"),
        posted(u64::MAX, 6, tags::SYSTEM, "Corrupt"),
    ]);
    let id_of = |title: &str| log.records().find(|r| r.title == title).map(|r| r.id.raw());
    assert_eq!(
        id_of("Three"),
        Some(3),
        "the file's own 3 was renumbered (the collision took {:?})",
        id_of("Another writer's two")
    );
}

/// `LogLine::decode` brings every word back under the model's caps, so a
/// hand-edited or hostile line cannot put in the ring what `post` would never
/// admit: at most `MAX_ACTIONS` intents.
#[test]
fn a_decoded_record_offers_at_most_max_actions() {
    let mut rec = LogRecord::from_posted(mid(4), stamp(1), &info("x"));
    rec.actions = vec![
        Intent::OpenSettings {
            route: "/packages".into(),
        },
        Intent::OpenPath { path: "/a".into() },
        Intent::ApplyUpdate { build: 7 },
        Intent::OpenSystemPane { pane: "x".into() },
        Intent::NewWindow,
    ];
    let line = LogLine::Posted(rec).encode();
    let Ok(LogLine::Posted(back)) = LogLine::decode(&line) else {
        panic!("the line decodes: {line}")
    };
    assert!(
        back.actions.len() <= MAX_ACTIONS,
        "a decoded record offers {} intents: {:?}",
        back.actions.len(),
        back.actions
    );
}

// ---- carry ------------------------------------------------------------------

/// A carried wire row stays the wire's (`Origin::wire_owned`): a keyless
/// wire row (`notice post … sev=warn` with no `key=`) carried across the
/// handoff files its Retired line on the wire's shelf, beside its Posted
/// line.
#[test]
fn a_keyless_wire_row_stays_the_wires_after_the_carry() {
    let now = t0();
    let mut parent = fresh(now);
    let id = parent
        .post(
            Message::new(
                Tag::try_new("build").unwrap(),
                Severity::Warn,
                "Build failed",
            )
            .origin(Origin::Wire),
            stamp(1),
            now,
        )
        .id;
    parent.commit_rows(now, 3);
    let shelved = parent.drain_shelved_for_persist();
    assert!(
        shelved
            .iter()
            .any(|(l, s)| matches!(l, LogLine::Posted(_)) && *s == Shelf::Wire),
        "premise: the Posted line is the wire's: {shelved:?}"
    );
    let carry = parent.carried();
    let mut c = fresh(now);
    c.seed_carried(&carry, stamp(2), now);
    let _ = c.drain_shelved_for_persist();
    assert!(c.dismiss(id, now + secs(1)));
    let shelves: Vec<Shelf> = c
        .drain_shelved_for_persist()
        .into_iter()
        .filter(|(l, _)| matches!(l, LogLine::Retired { .. }))
        .map(|(_, s)| s)
        .collect();
    assert_eq!(
        shelves,
        vec![Shelf::Wire],
        "the carried wire row's Retired line left the wire's file (origin now {:?})",
        c.log().get(id).map(|r| r.origin)
    );
}

/// A row's duplicate count (the Retired line's `rep=N`, the page's `×N`)
/// crosses the handoff: `carried()` drops echoes, the estimator, load and
/// the motion epoch, not the count.
#[test]
fn a_rows_repeats_cross_the_handoff() {
    let now = t0();
    let mut parent = fresh(now);
    let mut id = None;
    for i in 0..5 {
        id = Some(parent.post(info("Same words"), stamp(i), now + ms(i)).id);
    }
    let id = id.unwrap();
    parent.commit_rows(now + ms(5), 3);
    assert_eq!(parent.live(id).unwrap().repeats, 5);
    let carry = parent.carried();
    let mut c = fresh(now);
    c.seed_carried(&carry, stamp(9), now);
    assert_eq!(
        c.live(id).map(|l| l.repeats),
        Some(5),
        "the carried row folded 5 posts and reads"
    );
}

// ---- wire -------------------------------------------------------------------

/// `messages` reads a live row's current severity, as the glass and the page
/// do: a live row restated to Warn prints `sev=warn`, and the `sev=` floor
/// selects it.
#[test]
fn messages_reads_a_live_rows_restated_severity() {
    let now = t0();
    let mut c = fresh(now);
    let id = c
        .post(
            info("Checking the index").hold(Hold::STANDING),
            stamp(1),
            now,
        )
        .id;
    c.commit_rows(now, 3);
    assert!(c.restate(
        id,
        Restatement {
            severity: Some(Severity::Warn),
            ..Restatement::default()
        },
        now + secs(1),
    ));
    assert_eq!(c.live(id).unwrap().msg.severity, Severity::Warn);
    let rows = message_rows(&c, &ReadQuery::default(), 2_000, &|s| s.to_string());
    assert!(
        rows.len() == 1 && rows[0].contains(" sev=warn "),
        "{rows:?}"
    );
    let warn = parse_read_args("sev=warn").unwrap();
    assert_eq!(
        warn.select(c.log()).len(),
        1,
        "`messages sev=warn` leaves out the live warning"
    );
}

/// The title form runs on the stripped title, so an invisible character
/// cannot hide a trailing period, with or without a space before it.
#[test]
fn an_invisible_tail_after_a_space_hides_no_period() {
    for line in [
        "post script sev=warn Build failed. \u{2060}",
        "post script sev=warn Build failed. \u{7}",
        "post script sev=warn Build failed. \u{200b}",
        "progress k Building aterm. \u{2060}",
        "done k warn Build failed. \u{2060}",
    ] {
        assert_eq!(
            NoticeRequest::parse(line),
            Err("notice: title ends with a period"),
            "{line:?}"
        );
    }
}

/// Whitespace on the wire is a space before the strip (as
/// `a_tab_on_the_wire_is_a_space` pins), or the strip glues words and ` -- `
/// never splits the detail off; U+2028/U+2029 included.
#[test]
fn a_line_separator_on_the_wire_is_a_space() {
    let parsed = NoticeRequest::parse("post script sev=warn Deploy\u{2028}failed -- a\u{2029}b");
    let Ok(NoticeRequest::Post(p)) = parsed else {
        panic!("{parsed:?}")
    };
    assert_eq!(p.title, "Deploy failed");
    assert_eq!(p.detail, vec!["a b"]);
}

/// The grammar `<title>[ -- <detail>]`: a separator with an empty tail is no
/// detail, never part of the title.
#[test]
fn a_separator_with_an_empty_tail_leaves_the_title() {
    let parsed = NoticeRequest::parse("post script sev=warn Deploy failed -- ");
    let Ok(NoticeRequest::Post(p)) = parsed else {
        panic!("{parsed:?}")
    };
    assert_eq!(p.title, "Deploy failed");
    assert!(p.detail.is_empty(), "{:?}", p.detail);
    let parsed = NoticeRequest::parse("progress k Building aterm -- ");
    let Ok(NoticeRequest::Progress(p)) = parsed else {
        panic!("{parsed:?}")
    };
    assert_eq!(p.title, "Building aterm");
    assert!(p.stats.is_empty(), "{:?}", p.stats);
}

/// The wire's numbers are plain digits and its grammar edges are refusals:
/// `parse_read_args` refuses a signed number.
#[test]
fn messages_refuses_a_signed_number() {
    assert_eq!(parse_read_args("+5"), Err(crate::wire::READ_USAGE));
    assert_eq!(parse_read_args("since=+3"), Err(crate::wire::READ_USAGE));
}

// ---- render -----------------------------------------------------------------

fn band(msgs: &[Message], cols: usize) -> Presentation {
    let now = Instant::now();
    let mut c = MessageCenter::new(MessageLog::empty(), now);
    for msg in msgs {
        c.post(msg.clone(), stamp(1_000), now);
    }
    c.commit_rows(now, 3);
    c.presentation(cols, &char_width, None, Links::Painted)
}

/// What is gone at one width is gone at every narrower one, and the cells a
/// shorter capsule frees never re-buy an excerpt a wider row gave up
/// (`fit_fixed`): a label forced short because a sibling row went short does
/// not hand its saved cells to the excerpt.
#[test]
fn a_forced_short_label_never_brings_the_excerpt_back() {
    let a = Message::new(
        tags::CONFIG,
        Severity::Warn,
        "Couldn't load the theme file you picked",
    )
    .action(Intent::OpenConfigEditor { line: None });
    let b = Message::new(tags::CONFIG, Severity::Warn, "Unknown setting")
        .line("unknown key \"fnt\"")
        .action(Intent::OpenConfigEditor { line: None });
    let b_id = mid(2);
    let mut gone_at: Option<usize> = None;
    let mut seen_whole = false;
    for cols in (40..=200).rev() {
        let p = band(&[a.clone(), b.clone()], cols);
        let row = p
            .rows
            .iter()
            .find(|r| r.kind == RowKind::Message(b_id))
            .expect("B is on the band");
        match (&row.detail, gone_at) {
            (Some(_), None) => seen_whole = true,
            (None, None) => gone_at = Some(cols),
            (Some((_, d)), Some(gone)) => panic!(
                "B's excerpt {d:?} came back at {cols} columns after it was gone at {gone} \
                 (labels {:?})",
                p.rows
                    .iter()
                    .map(|r| r
                        .capsules
                        .iter()
                        .map(|c| c.text.clone())
                        .collect::<Vec<_>>())
                    .collect::<Vec<_>>()
            ),
            (None, Some(_)) => {}
        }
    }
    assert!(seen_whole && gone_at.is_some(), "the sweep saw both");
}

/// A row forced to the short time words pays the long width's room and the
/// cells it saves buy nothing (`allocate_extras`): a busy row's forced short
/// elapsed slot leaves its excerpt, stats and load slot exactly as its own
/// layout has them.
#[test]
fn a_forced_short_elapsed_slot_spends_nothing() {
    let spec = RowSpec {
        kind: RowKind::Message(MessageId::FIRST),
        severity: Severity::Info,
        live: true,
        glyph: '\u{21e3}',
        title: "Installing Homebrew",
        detail0: None,
        meter: Some((None, "~3 GB")),
        busy: true,
        animated: false,
        eta: false,
        load: None,
        load_slot: Loads::NONE,
        capsules: Vec::new(),
    };
    let extras = |l: &crate::glass::RowLayout| {
        (
            l.detail.as_ref().map(|(_, d)| d.clone()),
            l.stats.as_ref().map(|(_, s)| s.clone()),
            l.load_slot,
        )
    };
    let mut long_widths = 0;
    for cols in 20..=160 {
        let own = layout_row(&spec, cols, &char_width);
        if own.elapsed.is_none() || own.elapsed_short {
            continue;
        }
        long_widths += 1;
        let forced = layout_row_with(&spec, cols, &char_width, true);
        assert!(forced.elapsed.is_some() && forced.elapsed_short, "{cols}");
        assert_eq!(
            extras(&forced),
            extras(&own),
            "{cols}: the forced short elapsed slot's saved cells bought an extra"
        );
    }
    assert!(
        long_widths > 10,
        "the sweep saw the long form: {long_widths}"
    );
}

/// `keep_command`: when a `Run `…`` sentence's command cannot survive the
/// cut whole, as much of the command as fits is kept, never its first token
/// alone.
#[test]
fn a_run_sentence_keeps_as_much_of_its_command_as_fits() {
    let detail = "Run `x --flag-long-option-value-here` please";
    let shaped = shape_detail(detail, 20);
    assert_eq!(
        shaped,
        truncate(detail, 20),
        "the command was cut to its first token"
    );
    assert_eq!(shaped, "Run `x --flag-long-\u{2026}");
}

// ---- presence ---------------------------------------------------------------

/// `View::ripple_deadline` names the next instant the pixels change: at that
/// instant the step has moved, and while the ripple runs it is never before
/// `now`.
#[test]
fn the_ripple_deadline_names_an_instant_the_step_moves() {
    let start = Instant::now();
    let v = View {
        ripple_at: Some(start),
        ..View::default()
    };
    for us in [33_400u64, 66_800, 133_400, 166_800, 233_400, 266_800] {
        let now = start + Duration::from_micros(us);
        let d = v.ripple_deadline(now).expect("the ripple runs");
        assert!(
            d > now,
            "at {us} µs the deadline is {:?} µs, not after now",
            d.saturating_duration_since(start).as_micros()
        );
        assert_ne!(
            v.ripple_step(d),
            v.ripple_step(now),
            "at {us} µs the deadline's step has not moved"
        );
    }
}

// ---- marks and reloads across the handoff --------------------------------

/// A row restated to another mark retires under the mark it shows, written on
/// its Retired line when it differs from the Posted line's. The ring's record
/// reads the shown mark while the row lives, so the line is weighed against
/// `logged_mark`, never the record: a replay of the file reads the shown mark.
#[test]
fn a_restated_mark_still_rides_the_retired_line() {
    let now = t0();
    let mut c = fresh(now);
    let id = c
        .post(
            Message::new(tags::SYSTEM, Severity::Warn, "Upgrade waits").hold(Hold::STANDING),
            stamp(1),
            now,
        )
        .id;
    c.commit_rows(now, 3);
    assert!(c.restate(
        id,
        Restatement {
            severity: Some(Severity::Info),
            glyph: Some(Severity::Info.default_glyph()),
            ..Restatement::default()
        },
        now + secs(1),
    ));
    assert!(c.dismiss(id, now + secs(2)));
    let lines: Vec<LogLine> = c
        .drain_shelved_for_persist()
        .into_iter()
        .map(|(l, _)| l)
        .collect();
    let mark = lines.iter().find_map(|l| match l {
        LogLine::Retired { mark, .. } => Some(*mark),
        _ => None,
    });
    assert_eq!(
        mark,
        Some(Some((Severity::Info, Severity::Info.default_glyph()))),
        "{lines:?}"
    );
    let mut file = MessageLog::empty();
    file.replay_all(lines);
    assert_eq!(file.get(id).map(|r| r.severity), Some(Severity::Info));
}

/// The same across a seed: the parent restated its row Warn → Info, the
/// successor's boot load holds the parent's Posted line (Warn), and the row
/// the seed re-opens takes that line's mark as the one it was logged under,
/// so its Retired line says Info.
#[test]
fn a_seeded_rows_restated_mark_rides_its_retired_line() {
    let now = t0();
    let mut parent = fresh(now);
    let id = parent
        .post(
            Message::new(tags::SYSTEM, Severity::Warn, "Upgrade waits").hold(Hold::STANDING),
            stamp(1),
            now,
        )
        .id;
    parent.commit_rows(now, 3);
    let file: Vec<LogLine> = parent
        .drain_shelved_for_persist()
        .into_iter()
        .map(|(l, _)| l)
        .collect();
    assert!(parent.restate(
        id,
        Restatement {
            severity: Some(Severity::Info),
            glyph: Some(Severity::Info.default_glyph()),
            ..Restatement::default()
        },
        now + secs(1),
    ));
    let carry = parent.carried();
    let mut boot = MessageLog::empty();
    boot.replay_all(file.clone());
    let mut c = MessageCenter::new(boot, now);
    c.seed_carried(&carry, stamp(2), now);
    let _ = c.drain_shelved_for_persist();
    assert!(c.dismiss(id, now + secs(2)));
    let lines: Vec<LogLine> = c
        .drain_shelved_for_persist()
        .into_iter()
        .map(|(l, _)| l)
        .collect();
    let mut all = file;
    all.extend(lines);
    let mut replayed = MessageLog::empty();
    replayed.replay_all(all);
    assert_eq!(
        replayed.get(id).map(|r| r.severity),
        Some(Severity::Info),
        "the file reads the Posted line's Warn"
    );
}

/// `commit_rows` moves the revision when its own rebalance moves the glass
/// though the count stands: a restate raises a queued row's severity (a
/// restate never re-ranks), and the following commit puts it on the one-row
/// glass.
#[test]
fn a_commit_that_moves_the_glass_bumps_the_revision() {
    let now = t0();
    let mut c = fresh(now);
    let x = c.post(info("X").hold(Hold::STANDING), stamp(1), now).id;
    let y = c.post(info("Y").hold(Hold::STANDING), stamp(2), now).id;
    c.commit_rows(now, 1);
    let before = glass_ids(&c);
    assert!(c.restate(
        y,
        Restatement {
            severity: Some(Severity::Error),
            ..Restatement::default()
        },
        now + secs(1),
    ));
    assert_eq!(glass_ids(&c), before, "premise: a restate never re-ranks");
    let rev = c.revision();
    assert_eq!(c.commit_rows(now + secs(1), 1), None, "the count stands");
    assert_eq!(
        glass_ids(&c),
        vec![y],
        "premise: the Error took the glass from {x}"
    );
    assert_ne!(
        c.revision(),
        rev,
        "the glass moved and the revision did not"
    );
}

/// A record the seed moved off a carried number (another writer's, under the
/// parent's row's id) keeps its file id, so Commit's reload merges its ending
/// into it once its words have changed: one post, one record. Both kinds of
/// squatter: the file's own record of that number, and one a boot collision
/// had loaded under it.
#[test]
fn a_squatter_the_seed_moved_merges_its_ending_on_reload() {
    let other = || posted(4, 100, tags::TOOLCHAIN, "Updating ALab tools");
    let ended = || retired(4, Retired::Resolved(Outcome::Ok), 500, "Updated ALab tools");
    let first = || posted(4, 10, tags::SYSTEM, "First");
    // (the boot's file, the parent's row's id, the file at Commit)
    let scenes: [(Vec<LogLine>, u64, Vec<LogLine>); 2] = [
        (
            vec![other()],
            4,
            vec![other(), posted(4, 300, tags::SYSTEM, "Parent row"), ended()],
        ),
        (
            vec![first(), other()],
            5,
            vec![
                first(),
                other(),
                posted(5, 300, tags::SYSTEM, "Parent row"),
                posted(6, 350, tags::SYSTEM, "Parent's next"),
                ended(),
            ],
        ),
    ];
    for (scene, (boot_lines, row, disk_lines)) in scenes.into_iter().enumerate() {
        let now = t0();
        let mut boot = MessageLog::empty();
        boot.replay_all(boot_lines);
        let mut c = MessageCenter::new(boot, now);
        let carry = Carry {
            next_id: row + 1,
            live: vec![CarriedMessage {
                id: row,
                unix_ms: 300,
                tag: "system".into(),
                severity: "info".into(),
                glyph: '\u{2139}',
                title: "Parent row".into(),
                detail: Vec::new(),
                actions: Vec::new(),
                hold: Hold::Default.encode(),
                key: None,
                fill_permille: None,
                stats: String::new(),
                busy: false,
                on_glass: true,
                excerpt: true,
                finished: None,
                retrospective: false,
                wire: false,
                repeats: 1,
            }],
        };
        c.seed_carried(&carry, stamp(400), now);
        let mut disk = MessageLog::empty();
        disk.replay_all(disk_lines);
        c.merge_reloaded_log(&disk);
        let others: Vec<(u64, String, LogState)> = c
            .log()
            .records()
            .filter(|r| r.stamp.unix_ms == 100)
            .map(|r| (r.id.raw(), r.title.clone(), r.state.clone()))
            .collect();
        assert_eq!(
            others.len(),
            1,
            "scene {scene}: the other writer's one post: {others:?}"
        );
        assert_eq!(
            others[0].2,
            LogState::Retired(Retired::Resolved(Outcome::Ok)),
            "scene {scene}: {others:?}"
        );
        assert_eq!(
            c.live(mid(row)).map(|l| l.msg.title.as_str()),
            Some("Parent row"),
            "scene {scene}"
        );
    }
}

/// The late park: the parent posted its row (Warn) after the successor's boot
/// load, restated it to Info and parked. The seed has no Posted line to weigh
/// the mark against and takes the carried Info; Commit's reload reads the
/// file's Posted line (Warn), and the row's Retired line still carries Info,
/// so the file reads what the row showed.
#[test]
fn a_late_parked_rows_restated_mark_rides_its_retired_line() {
    let now = t0();
    let mut parent = fresh(now);
    let id = parent
        .post(
            Message::new(tags::SYSTEM, Severity::Warn, "Upgrade waits").hold(Hold::STANDING),
            stamp(1),
            now,
        )
        .id;
    parent.commit_rows(now, 3);
    let file: Vec<LogLine> = parent
        .drain_shelved_for_persist()
        .into_iter()
        .map(|(l, _)| l)
        .collect();
    assert!(parent.restate(
        id,
        Restatement {
            severity: Some(Severity::Info),
            glyph: Some(Severity::Info.default_glyph()),
            ..Restatement::default()
        },
        now + secs(1),
    ));
    let carry = parent.carried();
    // The boot loaded the file before the parent's post.
    let mut c = MessageCenter::new(MessageLog::empty(), now);
    c.seed_carried(&carry, stamp(2), now);
    let _ = c.drain_shelved_for_persist();
    let mut disk = MessageLog::empty();
    disk.replay_all(file.clone());
    c.merge_reloaded_log(&disk);
    assert!(c.dismiss(id, now + secs(2)));
    let lines: Vec<LogLine> = c
        .drain_shelved_for_persist()
        .into_iter()
        .map(|(l, _)| l)
        .collect();
    let mut all = file;
    all.extend(lines);
    let mut replayed = MessageLog::empty();
    replayed.replay_all(all);
    assert_eq!(
        replayed.get(id).map(|r| r.severity),
        Some(Severity::Info),
        "the file reads the Posted line's Warn"
    );
}

// ---- progress -------------------------------------------------------------

/// A count's patience is max(STALL_AFTER, COUNT_STALL_GAPS × its own mean gap
/// between advances) and restarts when a held phase ends; the plateau is no
/// part of the count's pace. A count that moved once a second before a
/// two-minute held phase, and then really stops, says `stalled` 10 s after
/// the phase ends.
#[test]
fn a_held_plateau_never_inflates_the_counts_pace() {
    let base = t0();
    let at = |n: u64| base + ms(n);
    let steps = |done: u64, unit: Unit| Amount {
        series: 7,
        done,
        total: 1000,
        unit,
    };
    let mut t = ProgressTrack::default();
    // Four advances, one a second.
    for k in 0..=4u64 {
        t.observe(at(k * 1000), steps(k * 10, Unit::Steps));
    }
    // A two-minute held phase: the fill holds, re-fed every 2 s.
    for k in 0..=60u64 {
        let now = at(4_000 + k * 2_000);
        t.observe(now, steps(40, Unit::HeldSteps));
        assert_ne!(t.eta(now), Eta::Stalled, "held at {k}");
    }
    // The next download starts at the same share, and hangs.
    let resumed = at(126_000);
    t.observe(resumed, steps(40, Unit::Steps));
    assert_eq!(
        t.stall_onset(resumed),
        Some(at(136_000)),
        "the patience after the phase is max(10 s, 4 × the 1 s pace)"
    );
    assert_eq!(t.eta(at(136_000)), Eta::Stalled);
}

// ---- words: the finished-verb table ----------------------------------------

/// A live row withdrawn by a Fault is logged `… stopped` (by a Vanish
/// `… ended`), with its live title as the first detail line.
#[test]
fn a_failed_rewrap_is_logged_stopped() {
    let now = t0();
    let mut c = fresh(now);
    let id = c
        .post(
            rewrap_row(1, 1_200_000, 3_400_000, Duration::ZERO),
            stamp(1),
            now,
        )
        .id;
    c.commit_rows(now, 3);
    let live_title = c.live(id).unwrap().msg.title.clone();
    assert_eq!(live_title, "Rewrapping 3.4M lines");
    assert!(c.withdraw_with(id, WaitEnd::Failed.echo(), now + secs(5)));
    let rec = c.log().get(id).unwrap();
    assert_eq!(
        (rec.title.as_str(), rec.detail.first().map(String::as_str)),
        ("Rewrapping stopped", Some("Rewrapping 3.4M lines")),
        "{rec:?}"
    );
}

/// A paste row withdrawn because its session went away (a Vanish) is logged
/// `Pasting ended`, not in its in-flight words.
#[test]
fn a_paste_whose_session_went_away_is_logged_ended() {
    let now = t0();
    let mut c = fresh(now);
    let id = c
        .post(
            paste_row(1, 9, 1_100_000, 4_200_000, Duration::ZERO),
            stamp(1),
            now,
        )
        .id;
    c.commit_rows(now, 3);
    assert_eq!(c.live(id).unwrap().msg.title, "Pasting 4.2 MB");
    assert_eq!(WaitEnd::Gone.echo(), EchoKind::Vanish);
    assert!(c.withdraw_with(id, WaitEnd::Gone.echo(), now + secs(5)));
    let rec = c.log().get(id).unwrap();
    assert_eq!(
        (rec.title.as_str(), rec.detail.first().map(String::as_str)),
        ("Pasting ended", Some("Pasting 4.2 MB")),
        "{rec:?}"
    );
}

// ---- words: the spoken description ----------------------------------------

/// A page entry at `at_unix_ms`.
fn view_at(at_unix_ms: u64) -> MessageView {
    MessageView {
        id: 1,
        at_unix_ms,
        tag: "toolchain".into(),
        severity: "info",
        glyph: '\u{2139}',
        title: "ALab tools are current".into(),
        detail: Vec::new(),
        state: "recorded",
        on_glass: None,
        repeats: 1,
        retired_unix_ms: None,
        superseded_by: None,
        answer: None,
        actions: Vec::new(),
        sentences: 1,
    }
}

/// The page is sectioned by the reader's local day, and what a screen reader
/// hears for a row agrees with its header: at UTC−7 on Tuesday 08:00, an
/// entry of Sunday 20:00 (36 h ago) sits under `Sunday` and one of Friday
/// 20:00 local (Saturday 03:00 UTC) under `Friday`.
#[test]
fn the_spoken_when_agrees_with_the_local_day_header() {
    let off: i64 = -25_200;
    let now: u64 = 1_790_694_000_000;
    let a: u64 = 1_790_564_400_000;
    let b: u64 = 1_790_391_600_000;
    let today = local_day(now, off);
    assert_eq!(day_heading(today, today, false), "Today");
    assert_eq!(day_heading(today, local_day(a, off), false), "Sunday");
    assert_eq!(day_heading(today, local_day(b, off), false), "Friday");
    let said_a = description(&view_at(a), now);
    let said_b = description(&view_at(b), now);
    assert!(
        !said_a.contains("yesterday"),
        "an entry under `Sunday` (yesterday was Monday) is spoken {said_a:?}"
    );
    assert!(
        !said_b.contains("2026-09-26"),
        "an entry under `Friday` (the 25th, local) is spoken {said_b:?}"
    );
}

// ---- waits: line counts -----------------------------------------------------

/// `lines_words`: a figure that would round to ten of the tenths unit reads
/// as the whole unit, so 9,950–9,999 lines read `10K`, never `10.0K`.
#[test]
fn a_count_that_rounds_to_ten_thousand_reads_10k() {
    assert_eq!(lines_words(9_949), "9.9K");
    assert_eq!(lines_words(9_950), "10K");
    assert_eq!(lines_words(9_999), "10K");
    assert_eq!(rewrap_stats(9_960, 10_000), "10K of 10K lines");
}

// ---- center: the retired record's mark -------------------------------------

/// The ring's and the replayed file's mark for `id`.
fn marks(c: &mut MessageCenter, id: MessageId) -> ((Severity, Glyph), Option<(Severity, Glyph)>) {
    let ring = c.log().get(id).map(|r| (r.severity, r.glyph)).unwrap();
    let mut file = MessageLog::empty();
    file.replay_all(drained(c));
    (ring, file.get(id).map(|r| (r.severity, r.glyph)))
}

/// A row retires under the mark it shows, and the ring and the file agree: a
/// stall entry restated Warn, then kept as an Info record (`keep_as_record`),
/// reads Info in the ring (the page, `messages sev=`) as in the file.
#[test]
fn a_record_kept_under_its_first_mark_reads_it_in_the_ring() {
    let now = t0();
    let mut c = fresh(now);
    let info_mark = (Severity::Info, Severity::Info.default_glyph());
    let a = c
        .post(
            Message::new(tags::UPDATE, Severity::Info, "Upgrade waiting")
                .key("upgrade.s-1")
                .hold(Hold::STANDING),
            stamp(1),
            now,
        )
        .id;
    c.commit_rows(now, 3);
    assert!(c.restate(
        a,
        Restatement {
            severity: Some(Severity::Warn),
            glyph: Some(Severity::Warn.default_glyph()),
            ..Restatement::default()
        },
        now + ms(1),
    ));
    assert!(c.keep_as_record(
        a,
        &Message::new(tags::UPDATE, Severity::Info, "Upgrade asks on its own").key("upgrade.s-1"),
        now + ms(2),
    ));
    let (ring, file) = marks(&mut c, a);
    assert_eq!(file, Some(info_mark), "the file");
    assert_eq!(ring, info_mark, "the ring reads the mark the file does");
}

/// A restate to Info that also folds the row (`hold: LogOnly`) retires it
/// under Info; the ring does not keep the Warn an earlier restate showed.
#[test]
fn a_row_restated_back_to_info_and_folded_reads_info_in_the_ring() {
    let now = t0();
    let mut c = fresh(now);
    let info_mark = (Severity::Info, Severity::Info.default_glyph());
    let a = c
        .post(
            Message::new(tags::UPDATE, Severity::Info, "Upgrade waiting")
                .key("upgrade.s-1")
                .hold(Hold::STANDING),
            stamp(1),
            now,
        )
        .id;
    c.commit_rows(now, 3);
    assert!(c.restate(
        a,
        Restatement {
            severity: Some(Severity::Warn),
            glyph: Some(Severity::Warn.default_glyph()),
            ..Restatement::default()
        },
        now + ms(1),
    ));
    assert!(c.restate(
        a,
        Restatement {
            severity: Some(Severity::Info),
            glyph: Some(Severity::Info.default_glyph()),
            hold: Some(Hold::LogOnly),
            ..Restatement::default()
        },
        now + ms(2),
    ));
    assert!(c.live(a).is_none(), "the LogOnly restate retired it");
    let (ring, file) = marks(&mut c, a);
    assert_eq!(file, Some(info_mark), "the file");
    assert_eq!(ring, info_mark, "the ring reads the mark the file does");
}

// ---- log: the reload merge --------------------------------------------------

/// The reload merge gives a Stale record the ending the file has for that
/// record: of two records with one stamp, tag and title (the first folded,
/// the second still open when aterm stopped), the successor's own post
/// evicts the first from its full ring before Commit, and the merge must not
/// hand the first's ending to the second.
#[test]
fn the_reload_merge_never_hands_one_records_ending_to_its_twin() {
    let lines = || {
        let mut v = vec![
            posted(1, 1000, tags::CONFIG, "Unknown setting"),
            retired(1, Retired::Folded, 9000, "Unknown setting"),
            posted(2, 1000, tags::CONFIG, "Unknown setting"),
        ];
        for id in 3..=512u64 {
            v.push(posted(
                id,
                10_000 + id,
                tags::SYSTEM,
                &format!("Record {id}"),
            ));
            v.push(retired(
                id,
                Retired::Folded,
                20_000 + id,
                &format!("Record {id}"),
            ));
        }
        v
    };
    let mut boot = MessageLog::empty();
    boot.replay_all(lines());
    assert_eq!(boot.records().count(), LOG_CAP, "the boot ring is full");
    assert_eq!(boot.get(mid(2)).unwrap().retired(), Some(&Retired::Stale));
    let now = t0();
    let mut c = MessageCenter::new(boot, now);
    // The successor records something before Commit.
    c.post(
        Message::new(tags::SYSTEM, Severity::Info, "Before commit").hold(Hold::LogOnly),
        stamp(30_000),
        now,
    );
    assert!(
        c.log().get(mid(1)).is_none(),
        "the successor evicted record 1"
    );
    let mut disk = MessageLog::empty();
    disk.replay_all(lines());
    c.merge_reloaded_log(&disk);
    let two = c.log().get(mid(2)).unwrap();
    assert_eq!(
        two.retired(),
        Some(&Retired::Stale),
        "record 2 was still open when aterm stopped: {two:?}"
    );
}

/// A determinate row's count and its fill agree: the band floors a fill to
/// its percent and `count_span` rounds each count's permille, so a fill
/// measured finer than its count is held one permille under the percent the
/// next count paints: `3 of 10 programs` at 45 % paints `39%`, not `4 of
/// 10`'s `40%`; `9 of 10` never fills the bar; `1 of 3` stops at `65%`
/// because `2 of 3` paints `66%` (667). Every count of every total up to
/// twelve, wherever the two counts' percents differ.
#[test]
fn a_measured_fill_never_paints_the_next_counts_percent() {
    let fill = |fill: u16, stats: &str, amount: bool| {
        Meter {
            fill_permille: Some(fill),
            stats: stats.into(),
            amount: amount.then_some(Amount {
                series: 1,
                done: u64::from(fill),
                total: 1000,
                unit: Unit::Steps,
            }),
            ..Meter::default()
        }
        .normalized()
        .fill_permille
        .unwrap()
    };
    let mut reached = Vec::new();
    for total in 1u64..=12 {
        for done in 0..total {
            let this = fill(0, &format!("{done} of {total} programs"), false);
            let next = fill(0, &format!("{} of {total} programs", done + 1), false);
            let top = fill(1000, &format!("{done} of {total} programs"), true);
            if next / 10 > this / 10 && top / 10 >= next / 10 {
                reached.push(format!(
                    "`{done} of {total}` at {}% — `{} of {total}`'s",
                    top / 10,
                    done + 1
                ));
            }
        }
    }
    assert!(reached.is_empty(), "{reached:#?}");
}

/// The band and the `messages` line share one load rule (`Live::said_load`):
/// the work's own primary load is the title's, so a download row whose
/// `network busy` the band does not paint prints no `load=network`.
#[test]
fn messages_prints_load_only_where_the_glass_says_it() {
    let now = t0();
    let mut c = fresh(now);
    let msg = Message::new(tags::PACKAGES, Severity::Info, "Installing ALab tools")
        .meter(Meter {
            fill_permille: Some(300),
            stats: "3 of 10 programs".into(),
            load: Some(Load::Network),
            ..Meter::default()
        })
        .hold(Hold::Live {
            stale_after: STALE_TAILED,
        })
        .loads([Load::Network, Load::Disk, Load::Cpu])
        .primary_load(Load::Network);
    c.post(msg, stamp(1), now);
    c.commit_rows(now, 3);
    let glass = c.presentation(120, &char_width, None, Links::Painted);
    assert_eq!(
        glass.rows[0].load, None,
        "premise: the band paints no load words for the work's own load"
    );
    let enc = |s: &str| s.to_string();
    let rows = message_rows(&c, &ReadQuery::default(), 2, &enc);
    assert_eq!(rows.len(), 1, "one record: {rows:?}");
    let line = &rows[0];
    assert!(
        !line.contains(" load="),
        "the wire says a load the glass does not: {line}"
    );
}

/// A page host with nothing staged, no files and no upgrades.
struct Plain;

impl Host for Plain {
    fn now_unix_ms(&self) -> u64 {
        60_000
    }
    fn log_folder(&self) -> Option<String> {
        None
    }
    fn saved(&self) -> bool {
        true
    }
    fn utc_offset_s(&self) -> i64 {
        0
    }
    fn staged_build(&self) -> Option<u64> {
        None
    }
    fn is_regular_file(&self, _path: &str) -> bool {
        false
    }
    fn upgrade_takes(&self, _tab: &str, _to: &str, _word: UpgradeWord) -> bool {
        false
    }
    fn upgrade_stands(&self, _tab: &str) -> bool {
        false
    }
    fn performs_navigation(&self) -> bool {
        true
    }
    fn sentence_lines(&self, _key: Option<&str>, _detail: &[String]) -> usize {
        1
    }
}

/// `still open when aterm stopped` is for a record replayed open from a
/// process that was killed or crashed. A seamless handoff ends the parent
/// gracefully: the rows its carry did not take (unrevealed rows) are closed by
/// the parent at Commit
/// (`MessageCenter::close_uncarried`), never read in the successor as cut
/// off by a crash.
#[test]
fn rows_the_carry_drops_are_not_read_as_a_crash_in_the_successor() {
    let now = t0();
    let mut parent = fresh(now);
    let pass = parent
        .post(
            Message::new(tags::PACKAGES, Severity::Info, "Checking ALab tools")
                .in_flight()
                .key("toolchain.pass")
                .hold(Hold::Live {
                    stale_after: STALE_TAILED,
                })
                .reveal_after(PROGRESS_GRACE),
            stamp(1),
            now,
        )
        .id;
    let check = parent
        .post(
            Message::new(tags::UPDATE, Severity::Info, "Checking for updates")
                .in_flight()
                .key("update.check")
                .hold(Hold::Live {
                    stale_after: STALE_TAILED,
                })
                .reveal_after(PROGRESS_GRACE),
            stamp(2),
            now,
        )
        .id;
    parent.commit_rows(now, 3);
    let carry = parent.carried();
    assert!(
        carry
            .live
            .iter()
            .all(|m| m.id != pass.raw() && m.id != check.raw()),
        "premise: neither row is carried: {:?}",
        carry.live.iter().map(|m| m.id).collect::<Vec<_>>()
    );
    // The parent's Commit: what the carry did not take ends with it, right
    // before its last write.
    let carried: Vec<u64> = carry.live.iter().map(|m| m.id).collect();
    assert_eq!(parent.close_uncarried(&carried, now), 2);
    // The parent's file, as it flushed it before handing off.
    let mut disk = MessageLog::empty();
    disk.replay_all(drained(&mut parent));
    let mut c = MessageCenter::new(disk.clone(), now);
    c.seed_carried(&carry, stamp(3), now);
    c.merge_reloaded_log(&disk);
    let page = MessagesState::of(&c, &Plain);
    let crashed: Vec<String> = [pass, check]
        .into_iter()
        .filter_map(|id| {
            let view = page.entry(id.raw()).expect("the record is in the ring");
            (view.state_words() == "still open when aterm stopped"
                || c.log().get(id).and_then(|r| r.retired()) == Some(&Retired::Stale))
            .then(|| format!("{} ({}): state={}", id.raw(), view.title, view.state))
        })
        .collect();
    assert!(
        crashed.is_empty(),
        "records read as a crash's (\"still open when aterm stopped\") after a graceful handoff: {crashed:?}"
    );
}
