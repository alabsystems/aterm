// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Codex's usage wall and the reset it names: the Codex lane's stand-in
//! instance and kernel, the tab's Codex at its usage wall (the `■` row
//! codex-rs prints, naming `try again at <when>` in the printing Codex's zone,
//! or `try again later`). The record keeps the named reset
//! (`St::limit_until`) as the Claude lane does, so the owner's view tells a
//! Codex limit at its real reset; the quiet flow reads a wall a grace past its
//! reset as an idle point. The reset is read in the Codex's own zone, and the
//! same words keep their first placement (owner, 2026-09-29).

use super::*;
use crate::harness::upgrade_drive::{self as drive, LIMIT_GRACE_S, Watcher};
use crate::supervise::limit;

/// `t` down to its minute: a reset Codex prints names no seconds.
fn minute(t: u64) -> u64 {
    t - t % 60
}

/// `at` (a whole minute) as Codex prints a reset on another day, in the zone
/// `offset_s` east of UTC: `Sep 25th, 2026 3:05 PM`.
fn codex_when(at: u64, offset_s: i64) -> String {
    let local = i64::try_from(at).expect("at") + offset_s;
    let (y, m, d) = aterm_types::rfc3339::civil_from_days(local.div_euclid(86_400));
    let secs = local.rem_euclid(86_400);
    let (h, min) = (secs / 3_600, secs % 3_600 / 60);
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let month = MONTHS[usize::try_from(m - 1).expect("month")];
    let suffix = match (d % 100, d % 10) {
        (11..=13, _) => "th",
        (_, 1) => "st",
        (_, 2) => "nd",
        (_, 3) => "rd",
        _ => "th",
    };
    let (h12, half) = match h {
        0 => (12, "AM"),
        1..=11 => (h, "AM"),
        12 => (12, "PM"),
        _ => (h - 12, "PM"),
    };
    format!("{month} {d}{suffix}, {y} {h12}:{min:02} {half}")
}

/// `at` as a Codex started with no `TZ` prints it, in the machine's own
/// zone (`try again at …`): the stand-in TUI's, unless a test names one.
fn at_local(at: u64) -> String {
    let unix = i64::try_from(at).expect("at");
    let offset = limit::machine_offset_at(unix).expect("the machine's zone");
    format!("at {}", codex_when(at, offset))
}

/// `at` as a Codex started with `TZ=<zone>` prints it.
fn at_zone(at: u64, zone: &str) -> String {
    let unix = i64::try_from(at).expect("at");
    let offset = limit::offset_at(Some(zone), unix).expect("a zone the machine knows");
    format!("at {}", codex_when(at, offset))
}

/// A Codex screen whose last turn ended on `row`.
fn turn_rows(row: &str) -> Vec<String> {
    [
        "› say hello",
        "",
        row,
        "",
        "› Ask Codex to do anything",
        "",
        "  GPT-6-Astra default · /w",
    ]
    .iter()
    .map(|r| (*r).to_string())
    .collect()
}

/// The tab's record, `f` applied, saved.
fn edit(rig: &Rig, f: impl FnOnce(&mut St)) {
    let mut st = rig.state();
    f(&mut st);
    save(&rig.opts, &key(TAB), &st);
}

/// A daemon living in `rig`'s Codex home, pid `pid`.
fn daemon_at(rig: &Rig, pid: u32) {
    let dir = rig.tui.home.join("app-server-daemon");
    std::fs::create_dir_all(&dir).expect("the daemon's dir");
    std::fs::write(dir.join("daemon.pid"), format!(r#"{{"pid":{pid}}}"#)).expect("daemon.pid");
}

/// The wall row: `when` is `at <time>` or `later`.
fn wall_row(when: &str) -> String {
    format!(
        "■ You've hit your usage limit. Visit https://chatgpt.com/codex/settings/usage to \
         purchase more credits or try again {when}."
    )
}

/// Codex 0.157's screen at its usage wall: [`codex_idle`]'s, its turn ended
/// on the wall's row (`when`: [`wall_row`]).
fn codex_wall(when: &str, seq: u64) -> String {
    codex_walled_by(&wall_row(when), seq)
}

/// [`codex_wall`]'s screen, its turn ended on the wall row `wall`.
fn codex_walled_by(wall: &str, seq: u64) -> String {
    screen_json(
        &[
            "╭─────────────────────────────────────────────╮",
            "│ >_ OpenAI Codex (v0.157.0)                  │",
            "╰─────────────────────────────────────────────╯",
            "› say hello",
            wall,
            "",
            "› Ask Codex to do anything",
            "",
            "  GPT-6-Astra default · /w",
            "  ← for agents · ? for shortcuts",
        ],
        (6, 2),
        seq,
    )
}

/// A rig whose tab's embedded Codex stands at its usage wall (`when`), in
/// the ask flow (`ask`) or the quiet one, its pending record settled.
fn walled(name: &str, when: String, ask: bool) -> Rig {
    let mut rig = Rig::new(name, T2, true, move |w| w.before = codex_wall(&when, 10));
    rig.opts.ask = ask;
    rig.settle(10);
    rig
}

/// Its row in the owner's view.
fn row_of(rig: &Rig) -> drive::Row {
    drive::rows(&rig.opts)
        .into_iter()
        .find(|r| r.tab == TAB)
        .expect("its row")
}

/// Every request of `rig`'s that could type or press: none may be sent at a
/// wall.
fn hands(rig: &Rig) -> Vec<String> {
    rig.asked()
        .into_iter()
        .filter(|l| {
            let verb = l.split_whitespace().find(|w| !w.starts_with('@'));
            !verb.is_some_and(|v| ["sessions", "text", "cell", "status", "help"].contains(&v))
        })
        .collect()
}

/// The wall's reset by the supervisor's grammar (`limit::parse_reset`,
/// `limit::reset_in_zone`) at a fixed clock and zone. A bare clock time is its
/// print's day's (codex-rs names the date of any other day's reset): at 23:00
/// a `12:30 AM` is today's, passed. A zone whose offset cannot be read places
/// nothing. Negative controls: `try again later`, the context window's wall
/// and an interrupted turn name no reset, and a wall that is no limit (an
/// overload, a server error) names none whatever time it says, while a
/// retry's `429` does.
#[test]
fn a_codex_walls_reset_is_read_by_the_supervisors_grammar() {
    // 2026-09-27 14:00 UTC.
    let midnight = 20_723 * 86_400;
    let now = midnight + 14 * 3_600;
    let utc = |_: i64| Some(0);
    let read = |row: &str, now: u64, offset_at: &dyn Fn(i64) -> Option<i64>| {
        wall_reset_at(&wall_reset_text(&turn_rows(row))?, now, offset_at)
    };
    let later = now + 26 * 3_600 + 5 * 60;
    assert_eq!(
        read(
            &wall_row(&format!("at {}", codex_when(later, 0))),
            now,
            &utc
        ),
        Some(later)
    );
    let bare = "■ You've hit your usage limit. Try again at 3:05 PM.";
    assert_eq!(read(bare, now, &utc), Some(now + 3_600 + 5 * 60));
    assert_eq!(
        read(bare, now, &|_| Some(2 * 3_600)),
        Some(now + 3_600 + 5 * 60 - 2 * 3_600),
        "3:05 PM two hours east of UTC"
    );
    let late = midnight + 23 * 3_600;
    let small = "■ You've hit your usage limit. Try again at 12:30 AM.";
    assert_eq!(read(small, late, &utc), Some(midnight + 30 * 60));
    let spec = limit::parse_reset("12:30 AM").expect("a clock time");
    let late_s = i64::try_from(late).expect("late");
    assert_eq!(
        limit::reset_in_zone(&spec, late_s, limit::SESSION_AHEAD_MAX, &utc),
        Some(i64::try_from(midnight + 86_400 + 30 * 60).expect("tomorrow")),
        "the session rule reads it as tomorrow's, 1½ h ahead"
    );
    assert_eq!(read(bare, now, &|_| None), None, "no zone, no reset");
    for none in [
        wall_row("later"),
        "■ Codex ran out of room in the model's context window. Start a new thread.".to_string(),
        "■ Conversation interrupted - tell the model what to do differently.".to_string(),
    ] {
        assert_eq!(wall_reset_text(&turn_rows(&none)), None, "{none}");
    }
    for (row, kind) in [
        (
            "■ We're currently experiencing high demand, which may cause temporary errors. \
             Try again at 3:05 PM.",
            "an overload",
        ),
        (
            "■ exceeded retry limit, last status: 500 Internal Server Error. Try again at 3:05 PM.",
            "a server error",
        ),
    ] {
        let rows = turn_rows(row);
        let wall = aterm_phase::codex::wall(&rows).expect(kind);
        assert_eq!(wall.reset.as_deref(), Some("3:05 PM"), "{kind}: its time");
        assert!(!wall.kind.reads_limited(), "{kind}");
        assert_eq!(wall_reset_text(&rows), None, "{kind}: no limit");
    }
    let throttled =
        "■ exceeded retry limit, last status: 429 Too Many Requests. Try again at 3:05 PM.";
    assert_eq!(read(throttled, now, &utc), Some(now + 3_600 + 5 * 60));
}

/// A wall's same words are the same reset, kept as first placed: a bare
/// `11:30 PM` placed later that day stays there when a look past midnight
/// finds the same words, so the wall is spent. Other words are placed afresh,
/// unplaceable words keep nothing, and a look with no wall clears the record.
/// Negative control: placing the same words again reads a live limit at every
/// look.
#[test]
fn a_codex_walls_bare_reset_is_kept_as_first_placed_across_midnight() {
    // 2026-09-27 00:00 UTC.
    let day = 20_723 * 86_400;
    let utc = |_: i64| Some(0);
    let text = "11:30 PM";
    let reset = day + 23 * 3_600 + 30 * 60;
    let mut st = St::default();
    keep_wall_reset(&mut st, Some(text), |t| {
        wall_reset_at(t, day + 21 * 3_600 + 5 * 60, &utc)
    });
    assert_eq!((st.limit_text.as_str(), st.limit_until), (text, reset));
    let look = reset + LIMIT_GRACE_S + 60;
    keep_wall_reset(&mut st, Some(text), |_: &str| -> Option<u64> {
        unreachable!("the same words are not placed again")
    });
    assert_eq!(st.limit_until, reset, "kept");
    assert!(look >= st.limit_until + LIMIT_GRACE_S, "spent at the look");
    assert_eq!(
        wall_reset_at(text, look, &utc),
        Some(reset + 86_400),
        "placed again, the next day's: live"
    );
    let other = "Sep 28th 9:00 AM";
    keep_wall_reset(&mut st, Some(other), |t| wall_reset_at(t, look, &utc));
    assert_eq!(
        (st.limit_text.as_str(), st.limit_until),
        (other, day + 86_400 + 9 * 3_600)
    );
    keep_wall_reset(&mut st, Some(text), |_| None);
    assert_eq!((st.limit_text.as_str(), st.limit_until), ("", 0));
    keep_wall_reset(&mut st, Some(other), |t| wall_reset_at(t, look, &utc));
    keep_wall_reset(&mut st, None, |_: &str| -> Option<u64> {
        unreachable!("no wall, nothing placed")
    });
    assert_eq!((st.limit_text.as_str(), st.limit_until), ("", 0));
}

/// The record keeps the reset the wall names, and the view tells it there: a
/// Codex at a live wall waits `limited`, typing nothing, and the owner's view
/// ends the limit at that reset (`Row::limit_ends_at`), telling it
/// `LIMIT_GRACE_S` past it. Negative controls: a wall naming no reset records
/// `0` and is told by the week bound (`LONGEST_LIMIT_S`); a look that finds
/// the wall gone clears what was kept.
#[test]
fn a_codex_wall_records_the_reset_it_names_and_is_told_there_not_a_week_on() {
    let reset = minute(now_s() + 2 * 3_600);
    let rig = walled("wall-named", at_local(reset), true);
    let r = rig.visit(None);
    assert_eq!(r.step, "wait:limited", "{r:?}\n{:#?}", rig.asked());
    assert!(hands(&rig).is_empty(), "{:#?}", hands(&rig));
    let st = rig.state();
    assert_eq!((st.wait.as_str(), st.limit_until), ("limited", reset));
    let row = row_of(&rig);
    assert_eq!(row.limit_ends_at(), Some(reset));
    let told = reset + LIMIT_GRACE_S;
    assert!(!row.limit_passed(told - 1), "a record until reset + grace");
    assert!(row.limit_passed(told));
    assert_eq!(row.stall(told).as_deref(), Some("overdue"));
    let words = row.stall_words(told).expect("told at reset + grace");
    assert!(words.contains("its usage limit's reset passed"), "{words}");

    // Negative control: a wall naming no reset — `0`, and the week bound.
    let unnamed = walled("wall-unnamed", "later".to_string(), true);
    assert_eq!(unnamed.visit(None).step, "wait:limited");
    let st = unnamed.state();
    assert_eq!((st.wait.as_str(), st.limit_until), ("limited", 0));
    let row = row_of(&unnamed);
    assert_eq!(
        row.limit_ends_at(),
        Some(st.wait_since + drive::upgrade_status::LONGEST_LIMIT_S)
    );
    assert!(
        !row.limit_passed(told),
        "no reset read: a record at reset + grace"
    );
    assert_eq!(row.stall(told), None);

    // Negative control: the wall gone, a look clears the reset kept.
    let gone = Rig::new("wall-gone", T2, true, |_| {});
    gone.settle(10);
    let mut st = gone.state();
    st.limit_until = reset;
    save(&gone.opts, &key(TAB), &st);
    assert_eq!(gone.visit(None).step, "announced:1");
    assert_eq!(gone.state().limit_until, 0);
}

/// A Codex spend or credit wall is the owner's hold, never a limit that
/// passed: it waits `limited`, typing nothing, and its record says the wall is
/// money (`St::limit_spend`), naming no reset. The view never bounds it by the
/// week, and once overdue tells it as the owner's to lift (credits, the cap).
/// Negative control: the usage wall naming no reset is a record until the
/// week bound, and told as passed past it.
#[test]
fn a_codex_spend_wall_is_the_owners_hold_never_a_limit_that_passed() {
    use drive::upgrade_status::{LONGEST_LIMIT_S, SPEND_WORDS};
    let spend = {
        let mut rig = Rig::new("wall-spend", T2, true, |w| {
            w.before = codex_walled_by(
                "■ You hit your spend cap set in your workspace. Increase your spend cap to \
                 continue.",
                10,
            );
        });
        rig.opts.ask = true;
        rig.settle(10);
        rig
    };
    let r = spend.visit(None);
    assert_eq!(r.step, "wait:limited", "{r:?}\n{:#?}", spend.asked());
    assert!(hands(&spend).is_empty(), "{:#?}", hands(&spend));
    let st = spend.state();
    assert_eq!(
        (st.wait.as_str(), st.limit_until, st.limit_spend),
        ("limited", 0, true)
    );
    let row = row_of(&spend);
    assert!(row.spend_held());
    assert_eq!(row.limit_ends_at(), None, "no usage window");
    let week_on = st.wait_since + LONGEST_LIMIT_S + LIMIT_GRACE_S + 86_400;
    assert!(!row.limit_passed(week_on));
    assert!(
        !row.line(week_on).contains("limit-passed"),
        "{}",
        row.line(week_on)
    );
    // Told once overdue, as the owner's to lift.
    let overdue = row.behind_since + drive::STALLED_AFTER_S;
    assert!(!row.asks_on_its_own(overdue), "a row, not a record");
    assert_eq!(row.stall(overdue).as_deref(), Some("overdue"));
    let words = row.stall_words(overdue).expect("told");
    assert!(words.contains(SPEND_WORDS), "{words}");
    assert!(
        row.line(overdue).contains(" cause=person told=-"),
        "{}",
        row.line(overdue)
    );
    // Negative control: the usage wall naming no reset.
    let usage = walled("wall-usage-unnamed", "later".to_string(), true);
    assert_eq!(usage.visit(None).step, "wait:limited");
    let st = usage.state();
    assert!(!st.limit_spend, "a usage window");
    let row = row_of(&usage);
    assert!(!row.spend_held());
    assert!(row.asks_on_its_own(row.behind_since + drive::STALLED_AFTER_S));
    assert!(row.limit_passed(st.wait_since + LONGEST_LIMIT_S + LIMIT_GRACE_S));
}

/// The quiet upgrade moves a Codex whose wall is a grace past its reset: under
/// `upgrade_ask = false` the wall left `LIMIT_GRACE_S` past its named reset is
/// no limit, and the idle conversation's `/exit` goes there, nothing asked
/// (owner, 2026-09-29).
/// Negative controls, each waiting `limited` with nothing typed: a minute
/// short of the grace; a wall naming no reset; a live wall; and the ask flow.
#[test]
fn a_codex_wall_a_grace_past_its_named_reset_is_moved_as_any_idle_point() {
    let now = now_s();
    let spent = minute(now - LIMIT_GRACE_S - 120);
    let rig = walled("wall-spent", at_local(spent), false);
    let r = rig.visit(None);
    assert_eq!(r.step, "done:resumed", "{r:?}\n{:#?}", rig.asked());
    let typed = rig.typed();
    assert_eq!(
        typed.len(),
        2,
        "the `/exit` and the resume alone: {typed:#?}"
    );
    assert!(typed[0].ends_with(" /exit"), "{typed:#?}");
    assert!(typed[1].contains(&format!("'resume' '{T2}'")), "{typed:#?}");
    assert!(rig.ledger_steps().contains(&"exit-typed".to_string()));

    let short = minute(now - LIMIT_GRACE_S + 180);
    let ahead = minute(now + 2 * 3_600);
    for (name, when, ask, kept) in [
        ("wall-short", at_local(short), false, short),
        ("wall-none", "later".to_string(), false, 0),
        ("wall-live", at_local(ahead), false, ahead),
        ("wall-asked", at_local(spent), true, spent),
    ] {
        let rig = walled(name, when, ask);
        let r = rig.visit(None);
        assert_eq!(r.step, "wait:limited", "{name}: {r:?}");
        assert!(rig.typed().is_empty(), "{name}: {:#?}", rig.typed());
        assert_eq!(rig.state().limit_until, kept, "{name}");
    }
}

/// The watch records a Codex wall's reset off any point: a Codex at its wall
/// offers no point, so the watch reads the reset off the screen (one `text`
/// read) and records it with no hands. The latest read's word is kept exactly:
/// a wall naming no reset records `0`. Negative control: the dry read alone
/// writes nothing.
#[test]
fn the_watch_records_a_codex_walls_named_reset_with_no_hands() {
    let reset = minute(now_s() + 2 * 3_600);
    let rig = walled("watch-wall", at_local(reset), true);
    let read = |o: &Opts| {
        assert!(o.dry_run, "the watch reads through a dry run");
        visit(o, &rig.tui, &rig.target, None, &rig.kernel)
    };
    let before = rig.state();
    assert_eq!(
        read(&Opts {
            dry_run: true,
            only_sid: Some(TAB.to_string()),
            ..rig.opts.clone()
        })
        .step,
        "wait:limited"
    );
    assert_eq!(rig.state(), before, "the dry read alone writes nothing");
    let due = drive::watch_at(&before).expect("a record owed a watch");
    let w = drive::watch_in(&rig.opts, TAB, &Watcher::default(), due, &read, &rig.kernel);
    assert_eq!(
        (w.step.as_str(), w.limit_until),
        ("wait:limited", Some(reset)),
        "{w:?}"
    );
    assert!(w.wrote.contains(&"looked".to_string()), "{w:?}");
    let st = rig.state();
    assert_eq!((st.wait.as_str(), st.limit_until), ("limited", reset));
    assert_eq!(row_of(&rig).limit_ends_at(), Some(reset));
    assert!(hands(&rig).is_empty(), "{:#?}", hands(&rig));

    // A wall naming no reset: `0` over the reset the record had.
    let unnamed = walled("watch-unnamed", "later".to_string(), true);
    let mut st = unnamed.state();
    st.limit_until = reset;
    save(&unnamed.opts, &key(TAB), &st);
    let st = unnamed.state();
    let read = |o: &Opts| visit(o, &unnamed.tui, &unnamed.target, None, &unnamed.kernel);
    let due = drive::watch_at(&st).expect("owed");
    let w = drive::watch_in(
        &unnamed.opts,
        TAB,
        &Watcher::default(),
        due,
        &read,
        &unnamed.kernel,
    );
    assert_eq!(
        (w.step.as_str(), w.limit_until),
        ("wait:limited", None),
        "{w:?}"
    );
    assert_eq!(unnamed.state().limit_until, 0);
    assert!(hands(&unnamed).is_empty(), "{:#?}", hands(&unnamed));
}

/// The reset is read in the zone of the Codex that printed it: codex-rs formats
/// `try again at <time>` in its own `TZ` (else the machine's) and names no
/// zone. A Codex started with a distant `TZ` (also spelled `:<zone>`) prints a
/// reset an hour ahead there; the quiet flow waits `limited` and keeps that
/// real reset. Negative controls, each waiting `limited` with `0` kept: an
/// unreadable environment, an unknown `TZ`, and a daemon with another zone.
/// Its positive twin: a daemon that agrees, and the spent wall is moved.
#[test]
fn a_codex_walls_reset_is_read_in_the_zone_the_codex_printed_it_in() {
    let now = now_s();
    let here = limit::offset_at(None, i64::try_from(now).expect("now")).expect("this zone");
    // At least three hours west of this process's zone, else a day east.
    let zone = if here >= -9 * 3_600 {
        "Etc/GMT+12"
    } else {
        "Pacific/Kiritimati"
    };
    let reset = minute(now + 3_600);
    for (name, tz) in [
        ("zone-named", zone.to_string()),
        ("zone-colon", format!(":{zone}")),
    ] {
        let mut rig = walled(name, at_zone(reset, zone), false);
        rig.kernel.tz = vec![(TUI, Some(tz))];
        let r = rig.visit(None);
        assert_eq!(r.step, "wait:limited", "{name}: {r:?}\n{:#?}", rig.asked());
        assert!(rig.typed().is_empty(), "{name}: {:#?}", rig.typed());
        assert_eq!(rig.state().limit_until, reset, "{name}");
    }

    let spent = minute(now - LIMIT_GRACE_S - 120);
    let unknown = vec![(TUI, Some("Nowhere/Atlantis".to_string()))];
    let differs = vec![(TUI, None), (SHELL, Some(zone.to_string()))];
    for (name, tz, daemon) in [
        ("zone-unreadable", Vec::new(), false),
        ("zone-unknown", unknown, false),
        ("zone-daemon-differs", differs, true),
    ] {
        let mut rig = walled(name, at_local(spent), false);
        rig.kernel.tz = tz;
        if daemon {
            daemon_at(&rig, SHELL);
        }
        let r = rig.visit(None);
        assert_eq!(r.step, "wait:limited", "{name}: {r:?}");
        assert!(rig.typed().is_empty(), "{name}: {:#?}", rig.typed());
        assert_eq!(rig.state().limit_until, 0, "{name}");
    }
    let mut rig = walled("zone-daemon-agrees", at_local(spent), false);
    rig.kernel.tz = vec![(TUI, None), (SHELL, None)];
    daemon_at(&rig, SHELL);
    let r = rig.visit(None);
    assert_eq!(r.step, "done:resumed", "{r:?}\n{:#?}", rig.asked());
}

/// The visit and the watch keep a wall's first placement: the record keeps
/// the wall's words (`St::limit_text`) and the reset first placed for them, so
/// the same wall read again keeps it and the quiet move goes by the kept
/// reset. Negative control: another wall's words kept, this wall's own are
/// placed from now, live: it waits `limited`, typing nothing.
#[test]
fn the_visit_and_the_watch_keep_a_walls_first_placement() {
    let now = now_s();
    let ahead = minute(now + 2 * 3_600);
    let when = at_local(ahead);
    let words = wall_reset_text(&turn_rows(&wall_row(&when))).expect("the wall's words");
    let spent = minute(now - LIMIT_GRACE_S - 120);
    let kept = |rig: &Rig, text: &str| {
        edit(rig, |st| {
            text.clone_into(&mut st.limit_text);
            st.limit_until = spent;
        });
    };

    let rig = walled("keep-visit", when.clone(), false);
    kept(&rig, &words);
    let r = rig.visit(None);
    assert_eq!(r.step, "done:resumed", "{r:?}\n{:#?}", rig.asked());

    let other = walled("keep-visit-other", when.clone(), false);
    kept(&other, "Jan 1st 9:00 AM");
    assert_eq!(other.visit(None).step, "wait:limited");
    assert!(other.typed().is_empty(), "{:#?}", other.typed());
    let st = other.state();
    assert_eq!(
        (st.limit_text.as_str(), st.limit_until),
        (words.as_str(), ahead)
    );

    let watched = walled("keep-watch", when, true);
    kept(&watched, &words);
    let read = |o: &Opts| visit(o, &watched.tui, &watched.target, None, &watched.kernel);
    let due = drive::watch_at(&watched.state()).expect("owed");
    let w = drive::watch_in(
        &watched.opts,
        TAB,
        &Watcher::default(),
        due,
        &read,
        &watched.kernel,
    );
    assert_eq!(
        (w.step.as_str(), w.limit_until),
        ("wait:limited", Some(spent)),
        "{w:?}"
    );
    assert!(w.wrote.contains(&"looked".to_string()), "{w:?}");
    let st = watched.state();
    assert_eq!(
        (st.limit_text.as_str(), st.limit_until),
        (words.as_str(), spent)
    );
    assert!(hands(&watched).is_empty(), "{:#?}", hands(&watched));
}
