// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test-only: the round-16 engine invariants (design ruling 267), found by a
//! second day on one windowed instance — a row aterm quits under is recorded
//! as cut off by the quit with where its work was, a script's first load is
//! its declaration, and a finished record offers no dead intent on the wire.

use crate::Outcome;
use crate::center::MessageCenter;
use crate::glass::Links;
use crate::log::{LogLine, MessageLog, Retired};
use crate::model::{Hold, Intent, Load, Loads, Message, Meter, Severity, WallStamp, tags};
use crate::text::char_width;
use crate::wire::{Applied, NoticeRequest, ReadQuery, WireGate, apply, message_rows};
use crate::{Duration, Instant, LOAD_AFTER, PROGRESS_GRACE};

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn stamp() -> WallStamp {
    WallStamp { unix_ms: 1_000 }
}

fn notice(c: &mut MessageCenter, g: &mut WireGate, line: &str, now: Instant) -> Applied {
    let req = NoticeRequest::parse(line).unwrap_or_else(|e| panic!("{line:?}: {e}"));
    apply(c, g, req, stamp(), now)
}

fn sweep(c: &mut MessageCenter, now: Instant) {
    c.settle(now, true);
    c.commit_rows(now, 3);
}

/// ATERM QUITTING IS NOT THE REPORTER GOING SILENT (ruling 267). Day two:
/// a script's `Rendering the video` at 28 % and a held `Disk nearly full`
/// warning were open when aterm quit; after the relaunch the log read the
/// render as `stopped reporting` with no percent, and the warning as the
/// same. The graceful exit now retires every open row `Quit`: the words
/// stand, the determinate row says where it was, and the lines survive the
/// codec so the next launch reads exactly that.
#[test]
fn a_row_open_when_aterm_quits_is_recorded_as_cut_off_with_its_percent() {
    let t0 = Instant::now();
    let mut c = MessageCenter::new(MessageLog::empty(), t0);
    let mut g = WireGate::default();
    let render = notice(
        &mut c,
        &mut g,
        "progress video done=17/60 unit=steps Rendering the video",
        t0,
    );
    let Applied { reply, .. } = render;
    assert!(reply.starts_with("OK"), "{reply}");
    let warn = c
        .post(
            Message::new(tags::SYSTEM, Severity::Warn, "Disk nearly full")
                .line("2 GB free on the startup disk"),
            stamp(),
            t0,
        )
        .id;
    let busy = c
        .post(
            Message::new(tags::TOOLCHAIN, Severity::Info, "Checking ALab tools")
                .hold(Hold::Live {
                    stale_after: Duration::from_secs(120),
                })
                .meter(Meter::busy("")),
            stamp(),
            t0,
        )
        .id;
    let quit_at = t0 + PROGRESS_GRACE + ms(500);
    sweep(&mut c, quit_at);
    assert_eq!(c.quit(quit_at), 3, "every open row is closed");
    assert_eq!(c.live_rows().count(), 0);
    let log = c.log();
    let render = log
        .records()
        .find(|r| r.title == "Rendering the video")
        .expect("the render's record keeps its title");
    assert_eq!(render.retired(), Some(&Retired::Quit));
    assert_eq!(
        render.detail.first().map(String::as_str),
        Some("28% when aterm quit")
    );
    assert_eq!(render.severity, Severity::Info, "the mark is the row's own");
    let warn = log.get(warn).expect("the warning's record");
    assert_eq!(warn.retired(), Some(&Retired::Quit));
    assert_eq!(warn.title, "Disk nearly full");
    assert_eq!(
        warn.detail,
        vec!["2 GB free on the startup disk".to_string()]
    );
    assert_eq!(warn.severity, Severity::Warn, "it stays in Problems");
    let busy = log.get(busy).expect("the check's record");
    assert_eq!(busy.retired(), Some(&Retired::Quit));
    assert!(busy.detail.is_empty(), "a busy row has no percent to name");
    // What the next launch reads: the Retired lines round-trip, and replay
    // lands them over the Posted lines.
    let lines: Vec<LogLine> = c
        .drain_shelved_for_persist()
        .into_iter()
        .map(|(line, _)| LogLine::decode(&line.encode()).expect("decodes"))
        .collect();
    let mut next = MessageLog::empty();
    for line in lines {
        next.replay(line);
    }
    let replayed = next
        .records()
        .find(|r| r.title == "Rendering the video")
        .expect("replayed");
    assert_eq!(replayed.retired(), Some(&Retired::Quit));
    assert_eq!(
        replayed.detail.first().map(String::as_str),
        Some("28% when aterm quit")
    );
    assert!(replayed.retired_unix_ms.is_some());
}

/// A SCRIPT'S FIRST LOAD IS ITS DECLARATION (ruling 267). Day two: a wire
/// row posted with `load=disk` reserved the widest words of all four loads
/// (`network busy`), so at 60 columns the row dropped its `Details ›` and
/// its words with cells to spare, and the words stayed off until 80. It
/// reserves its own load's words now, and a different load later widens
/// the slot once (ruling 221).
#[test]
fn a_scripts_first_load_is_its_declaration_and_reserves_only_its_words() {
    let t0 = Instant::now();
    let mut c = MessageCenter::new(MessageLog::empty(), t0);
    let mut g = WireGate::default();
    // Day two's band: the ALab tools check above, the script's row under it.
    let alab = c
        .post(
            Message::new(tags::TOOLCHAIN, Severity::Info, "Updating ALab tools")
                .hold(Hold::Live {
                    stale_after: Duration::from_secs(120),
                })
                .meter(Meter {
                    fill_permille: Some(120),
                    ..Meter::default()
                }),
            stamp(),
            t0,
        )
        .id;
    let line = |k: u64| {
        format!("progress img done={k}/60 unit=steps load=disk Building the release image")
    };
    let mut now = t0;
    for k in 0..=12u64 {
        now = t0 + ms(k * 750);
        notice(&mut c, &mut g, &line(k), now);
        sweep(&mut c, now);
    }
    now += LOAD_AFTER;
    notice(&mut c, &mut g, &line(13), now);
    sweep(&mut c, now);
    let live = c.live_rows().find(|l| l.id != alab).expect("the row");
    assert_eq!(live.msg.loads, Loads::NONE.with(Load::Disk));
    assert_eq!(live.load_slot, Loads::NONE.with(Load::Disk));
    // At 60 columns the cells the widest words held empty come back: the
    // row keeps its `Details ›` (before, it lost it AND the words), and the
    // ETA still outranks the load words (ruling 229).
    let p = c.presentation(60, &char_width, None, Links::Painted);
    assert_eq!(p.rows.len(), 2, "{p:?}");
    assert_eq!(p.rows[0].kind, crate::glass::RowKind::Message(alab));
    let row = &p.rows[1];
    assert!(row.eta.is_some(), "{row:?}");
    assert_eq!(row.capsules.len(), 1, "{row:?}");
    assert_eq!(row.capsules[0].full_label, "Details \u{203a}");
    // At 70 the disk words show — where the widest reservation kept them
    // off until 80.
    let p = c.presentation(70, &char_width, None, Links::Painted);
    let row = &p.rows[1];
    assert_eq!(
        row.load.map(|(_, words)| words),
        Some("disk busy"),
        "{row:?}"
    );
    assert_eq!(row.load_slot.map(|(_, w)| w), Some(9), "{row:?}");
    // A load outside the declaration widens the slot once.
    notice(
        &mut c,
        &mut g,
        "progress img done=14/60 unit=steps load=network Building the release image",
        now + ms(750),
    );
    let live = c.live_rows().find(|l| l.id != alab).expect("the row");
    assert_eq!(
        live.load_slot,
        Loads::NONE.with(Load::Disk).with(Load::Network),
        "a load outside the declaration joins it (ruling 221)"
    );
}

/// THE WIRE OFFERS NO DEAD INTENT (ruling 267, the page's ruling 265): day
/// two's `messages` listed `actions="Stop paste"` on the finished `Paste
/// stopped` records while the page offered nothing. A live row still lists
/// it; its record does not.
#[test]
fn a_finished_records_row_lists_no_intent_that_ended_with_it() {
    let t0 = Instant::now();
    let mut c = MessageCenter::new(MessageLog::empty(), t0);
    let paste = c
        .post(
            Message::new(tags::SYSTEM, Severity::Info, "Pasting 4.2 MB")
                .hold(Hold::Live {
                    stale_after: Duration::from_secs(120),
                })
                .meter(Meter::busy(""))
                .action(Intent::StopPaste { session: 3 }),
            stamp(),
            t0,
        )
        .id;
    let enc = |s: &str| s.replace(' ', "%20");
    let q = ReadQuery::default();
    let rows = message_rows(&c, &q, 2_000, &enc);
    assert!(rows[0].contains(" actions=Stop%20paste "), "{}", rows[0]);
    assert!(c.resolve(paste, Outcome::Warn, t0 + ms(400)));
    let rows = message_rows(&c, &q, 2_000, &enc);
    assert!(rows[0].contains(" actions=- "), "{}", rows[0]);
}
