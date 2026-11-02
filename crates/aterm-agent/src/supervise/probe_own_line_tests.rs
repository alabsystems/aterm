// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ACCOUNTS PROBE'S `/usage` LEFT TYPED IS THE LOOP'S OWN LINE (slice pl1): the probe types
//! `/usage` with a fenced write and, where its echo or its Enter misses, leaves it typed. That
//! line is the one this loop left ([`turn_end_loop::OwnLine`]), as an act's is since slice at1, so
//! a draft equal to it is no person's ([`Session::person_at`]): drawn late, it holds nothing for a
//! person's grace, and the probe's own Enter sends it when it draws. A probe the ledger seeds
//! after a restart claims no draft, a person's keys still hold, and the probe's end ends the
//! claim.

use super::super::super::turn_end_loop::{ECHO_HANG, ECHO_HUNG, OwnBy, OwnLine, Written};
use super::*;
use crate::harness::usage::USAGE_COMMAND;
use crate::supervise::policy::turn_end::{PERSON_TYPING, RULE_USAGE_PROBE};

/// How long the program holds the probe's write unread before it draws it: past the echo's hang.
const DRAWN_AFTER: Duration = Duration::from_secs(75);

/// [`probe_run`] over `max_s` of the run's clock.
fn probe_run_over(
    tag: &str,
    m: &mut Mock,
    host: &Arc<ProbeHost>,
    ledger: &Path,
    policy: SupervisorConfig,
    max_s: u64,
) -> (Vec<String>, Vec<String>) {
    let (dir, path) = journal_file(tag);
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(host) as Arc<dyn IdleHost>),
        journal: Some(path.clone()),
        policy,
        ..hosted(max_s)
    };
    let (lines, _) = watch_lines_with(m, &opts, |s| {
        s.set_clock(PROBE_NOW, PDT);
        s.set_approval_ledger(Some(ledger.to_path_buf()));
        probe_waits(s);
    });
    let (records, _) = journal_records(&path);
    let _ = std::fs::remove_dir_all(&dir);
    (lines, records.into_iter().map(|r| r.line).collect())
}

/// The journal's `WAITING` lines that hold for a person typing.
fn person_waits(journal: &[String]) -> Vec<&String> {
    journal
        .iter()
        .filter(|l| l.starts_with("WAITING ") && l.contains(PERSON_TYPING))
        .collect()
}

/// The run's clock at each Enter the loop pressed.
fn enters_at(m: &Mock) -> Vec<Instant> {
    m.key_times
        .iter()
        .filter(|(r, _)| r.ends_with(" enter"))
        .map(|(_, at)| *at)
        .collect()
}

/// The probe's `/usage` held unread by its program and drawn `drawn` after the write, its screen
/// repainted at `repaint` meanwhile with the composer still empty (none: the screen stands), run
/// under the shipped grace (120 s) until the probe's Escape or the run's 600 s: the run's lines,
/// its journal, the probe's ledger rows, and the Enter's lag behind the draw (none: no Enter).
fn late_draw(
    tag: &str,
    repaint: Option<Duration>,
    drawn: Duration,
) -> (
    Mock,
    Vec<String>,
    Vec<String>,
    Vec<String>,
    Option<Duration>,
) {
    let (dir, ledger) = ledger_at(tag);
    let host = ProbeHost::probing(true);
    let mut m = probe_world(&[panel_fx::USAGE_SETTLED], &host);
    m.unmoved_after_send = Some(drawn);
    m.unread_after_write = Some(drawn);
    m.unread_first_send_only = true;
    world(&mut m).repaint_late_at = repaint;
    // The text it draws is typed, never a grey suggestion.
    m.cell_attrs_on.insert(1, "none");
    m.stall_sleep = Some(Duration::from_secs(5));
    m.exit_when = Some(|m| probe_writes(m).contains(&"esc"));
    let (lines, journal) = probe_run_over(
        &format!("te-{tag}"),
        &mut m,
        &host,
        &ledger,
        probe_policy(120),
        600,
    );
    let rows = probe_rows(&ledger);
    let _ = std::fs::remove_dir_all(&dir);
    let sent = m.first_send_at.expect("the probe's write");
    let at = world(&mut m).drawn_late_at.expect("drawn late");
    assert!(
        at.saturating_duration_since(sent) >= ECHO_HANG,
        "{tag}: drawn past the echo's hang: {lines:#?}"
    );
    let lag = enters_at(&m)
        .first()
        .map(|enter| enter.saturating_duration_since(at));
    (m, lines, journal, rows, lag)
}

/// What a probe's `/usage` drawn late must come to ([`late_draw`]): given up on at the echo's
/// hang (one `SKIPPED … typed, not submitted` record), sent within seconds of its draw by the
/// probe's own Enter (`TYPED … left typed`), no person's grace waited on it and no act's booking,
/// its panel closed unsampled and the ledger's owed row closed.
fn sent_as_its_own(
    tag: &str,
    run: (
        Mock,
        Vec<String>,
        Vec<String>,
        Vec<String>,
        Option<Duration>,
    ),
) {
    let (m, lines, journal, rows, lag) = run;
    let left: Vec<&String> = journal
        .iter()
        .filter(|l| l.contains(&format!("rule={RULE_USAGE_PROBE} typed, not submitted")))
        .collect();
    assert!(
        left.len() == 1 && left[0].contains(ECHO_HUNG),
        "{tag}: given up on at the echo's hang: {journal:#?}"
    );
    assert_eq!(
        person_waits(&journal),
        Vec::<&String>::new(),
        "{tag}: no person's grace on the probe's own line"
    );
    let lag = lag.unwrap_or_else(|| panic!("{tag}: its Enter: {lines:#?}\n{journal:#?}"));
    assert!(
        lag < Duration::from_secs(10),
        "{tag}: the Enter {lag:?} after the draw: {journal:#?}"
    );
    let typed = format!(" rule={RULE_USAGE_PROBE} {USAGE_COMMAND} left typed");
    assert_eq!(
        journal
            .iter()
            .filter(|l| l.starts_with("TYPED ") && l.contains(&typed))
            .count(),
        1,
        "{tag}: the probe's own Enter: {journal:#?}"
    );
    assert!(
        !journal
            .iter()
            .any(|l| l.starts_with("CONTINUED ") && l.contains(USAGE_COMMAND)),
        "{tag}: `/usage` is no continuation: {journal:#?}"
    );
    assert_eq!(
        probe_writes(&m),
        [USAGE_COMMAND, "esc"],
        "{tag}: {lines:#?}"
    );
    assert_eq!(
        rows,
        ["typed /usage the accounts probe", "waiting /usage closed"],
        "{tag}"
    );
}

/// A CLAUDE CODE THAT HOLDS THE PROBE'S `/usage` UNREAD PAST THE ECHO'S HANG GETS IT SENT WHEN IT
/// DRAWS, BY THE PROBE'S OWN ENTER, WITH NO PERSON'S GRACE WAITED ON IT ([`sent_as_its_own`]): the
/// program draws it 75 s after the write on a screen that stood still since. On main before this
/// slice the probe ended on the still-empty composer before the draw, nothing looked at the point
/// again when `/usage` drew, and it stood unsent, a person's draft to every read: no Enter in the
/// run's 600 s. Negative controls by mutation: the probe's left line not kept as the loop's own
/// ([`Session::left_own`]): no Enter; the probe ended on the composer before the draw
/// ([`Session::composer_settles`]): `WAITING … a person is typing`, then the `/usage` sent 120 s
/// after the draw as a continuation (`CONTINUED … /usage`) and its panel left open.
#[test]
fn the_probes_usage_drawn_late_is_sent_when_it_draws_with_no_persons_grace() {
    sent_as_its_own(
        "probe-drawn-late",
        late_draw("probe-drawn-late", None, DRAWN_AFTER),
    );
}

/// THE PROBE'S LINE OUTLIVES A REPAINT BEFORE ITS DRAW ([`Session::probe_line_unseen`]): the
/// program's screen moves once while it holds the write, the composer still empty (its footer
/// redrawn), past the echo's hang (65 s, drawn at 75 s) or later (75 s, drawn at 90 s). The probe
/// lasts while the line it left has not been seen whole and no person has keyed since, however
/// its screen moved, so the draw is still the probe's own line ([`sent_as_its_own`]). Negative
/// control by mutation: the probe ended at the first screen move with its composer empty (its
/// life tied to the unmoved screen alone): `WAITING … a person is typing`, then `/usage` sent 120 s
/// after its draw as `CONTINUED rule=continue@v1 /usage`, its panel left open.
#[test]
fn the_probes_usage_drawn_late_after_its_screen_moved_is_still_its_own() {
    for (tag, repaint, drawn) in [
        ("probe-repainted-late", 65, 75),
        ("probe-repainted-later", 75, 90),
    ] {
        let secs = Duration::from_secs;
        sent_as_its_own(tag, late_draw(tag, Some(secs(repaint)), secs(drawn)));
    }
}

/// WHOSE DRAFT A `/usage` IS ([`Session::person_at`]): one no line of the loop's claims is a
/// person's, a probe the ledger seeded after a restart (`UsageProbe::Owed` with `own: false`)
/// included, since this process wrote nothing; the line this process's probe left is no person's;
/// a person's key still holds over it; and the probe's end ends its claim, where an act's line
/// outlives it. Negative controls by mutation: a `/usage` draft claimed while any probe is owed
/// (the seed's assertion fails); the probe's end keeping its claim (its assertion fails).
#[test]
fn a_usage_draft_is_no_persons_only_while_the_probe_that_left_it_lasts() {
    let mut m = Mock::new(vec![with_draft(&ended(PROBE_SAID), USAGE_COMMAND)]);
    m.gen_fence = true;
    m.human_seq = Some(0);
    m.cursor_col = Some(2 + USAGE_COMMAND.len());
    let mut s = virtual_session(&mut m, None);
    let mut heard = Heard::default();
    s.screen().expect("the read");
    let seen = s
        .person_at()
        .expect("a draft no line of the loop's claims reads as a person's");
    s.probe = UsageProbe::Owed {
        own: false,
        since: s.fx.mono(),
    };
    assert_eq!(s.person_at(), Some(seen), "a seeded probe claims no draft");
    s.own_line = Some(OwnLine::of(USAGE_COMMAND, OwnBy::Probe));
    assert_eq!(s.person_at(), None, "the probe's own line is no person's");
    s.fx.sleep(Duration::from_secs(3));
    let key = s.fx.mono();
    s.person = Some(key);
    assert_eq!(s.person_at(), Some(key), "a person's key holds over it");
    s.person = None;
    s.probe_closed(0, &mut heard);
    assert_eq!(s.person_at(), Some(seen), "the probe's end ends its claim");
    s.own_line = Some(OwnLine::of(
        USAGE_COMMAND,
        OwnBy::Act(TurnEndAction::Nothing),
    ));
    s.probe = UsageProbe::Owed {
        own: true,
        since: s.fx.mono(),
    };
    s.probe_closed(0, &mut heard);
    assert_eq!(
        s.person_at(),
        None,
        "an act's line outlives the probe's end"
    );
}

/// A LINE ANY WRITE LEAVES IS THE LOOP'S OWN ([`Session::type_text_written`], which Cruise's carry
/// at a break writes through as a point's act does): given up on unechoed at the echo's hang and
/// drawn after, it reads as no person typing ([`Session::person_at`]); a person's key read after
/// it still does. Negative control by mutation: the write not claiming the line it left (the draw
/// reads as a person's draft at its sighting).
#[test]
fn a_line_a_write_leaves_is_no_persons_draft_when_it_draws() {
    let said = "Fixed the parser; the suite is green.";
    let mut m = Mock::new(vec![
        busy_screen(),
        ended(said),
        typed_in(said, "keep going"),
    ]);
    m.gen_fence = true;
    m.sends_gen = true;
    m.human_seq = Some(0);
    m.help = FENCED_HELP.to_string();
    m.screen_cols.insert(2, 12);
    m.unmoved_after_send = Some(Duration::from_secs(70));
    let mut s = virtual_session(&mut m, None);
    let point = s.await_turn(Duration::from_secs(60)).expect("the turn end");
    let mut heard = Heard::default();
    let written = s
        .type_text_written(&point, "keep going", RULE_CONTINUE, &mut heard)
        .expect("the write");
    assert_eq!(written, Written::Left, "{:#?}", heard.noted);
    s.fx.sleep(Duration::from_secs(15));
    let drawn = s.screen().expect("the draw");
    assert!(
        drawn.rows.iter().any(|r| r == "❯ keep going"),
        "drawn: {:#?}",
        drawn.rows
    );
    assert_eq!(s.person_at(), None, "the loop's own line, drawn late");
    s.ctl.human_ms = Some(0);
    s.foreground_program().expect("status");
    assert!(s.person_at().is_some(), "a person's key holds");
}

/// A PROBE THE LEDGER SEEDS AFTER A RESTART PRESSES NO ENTER OF ITS OWN ON A `/usage` IT FINDS
/// STANDING ([`Session::probe_left_stands`]): the first loop leaves its `/usage` typed and ends;
/// the next loop's probe, seeded owed from the ledger (`UsageProbe::Owed` with `own: false`), finds
/// it standing, a line no write of this process left: it holds for a person's grace as any
/// person's draft does, and past the grace no `TYPED … usage-probe@v1 /usage left typed` is the
/// seeded probe's. Negative control by mutation: the probe's Enter on any `/usage` standing while
/// a probe is owed (its own line unasked): the seeded probe's `TYPED … left typed` past the grace.
#[test]
fn a_probe_the_ledger_seeds_presses_no_enter_on_a_usage_it_did_not_write() {
    let (dir, ledger) = ledger_at("probe-seeded-past-grace");
    let host = ProbeHost::probing(true);
    let mut m = probe_world(
        &[panel_fx::USAGE_REFRESHING, panel_fx::USAGE_SETTLED],
        &host,
    );
    m.tick_at_key = 1;
    m.exit_when = Some(|m| m.keys >= 1);
    let _ = probe_run_over(
        "te-probe-seeded-past-grace",
        &mut m,
        &host,
        &ledger,
        probe_policy(120),
        60,
    );
    m.exit_when = None;
    assert_eq!(
        probe_writes(&m),
        [USAGE_COMMAND],
        "the first loop's, left typed"
    );
    let standing = m.screens.last().cloned().expect("a screen");
    m.screens.push(with_draft(&busy_screen(), USAGE_COMMAND));
    m.screens.push(standing);
    m.served = m.screens.len() - 2;
    m.stalls = 0;
    m.exit_when = Some(|m| probe_writes(m).contains(&"esc"));
    let next = ProbeHost::probing(true);
    let (lines, journal) = probe_run_over(
        "te-probe-seeded-past-grace-next",
        &mut m,
        &next,
        &ledger,
        probe_policy(120),
        600,
    );
    let rows = probe_rows(&ledger);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        !person_waits(&journal).is_empty(),
        "no line of this process's: a person's grace first: {journal:#?}"
    );
    let typed = format!(" rule={RULE_USAGE_PROBE} {USAGE_COMMAND} left typed");
    assert!(
        !journal
            .iter()
            .any(|l| l.starts_with("TYPED ") && l.contains(&typed)),
        "the seeded probe's own Enter on a `/usage` it did not write: {journal:#?}"
    );
    // Past the grace the policy's draft step sends it as any person's draft (the act's text, as
    // `probe-left-not-ours` has it), and the seeded probe closes the panel that opens.
    assert!(
        journal.iter().any(|l| l.starts_with("CONTINUED ")
            && l.ends_with(&format!(" rule={RULE_CONTINUE} {USAGE_COMMAND}"))),
        "{journal:#?}"
    );
    assert_eq!(
        m.writes(),
        ["text:/usage", "enter", "enter", "esc"],
        "{lines:#?}"
    );
    assert_eq!(
        rows,
        ["typed /usage the accounts probe", "waiting /usage closed"]
    );
}

/// A LOOK WHOSE READ SHOWS NO COMPOSER ENDS NO PROBE WHOSE LINE THE NEXT READ SHOWS STANDING
/// ([`Session::usage_panel_look`]): a torn read (the composer not drawn) and a fresh grid with no
/// panel and the probe's `/usage` standing whole is that line still, not the panel gone: the probe
/// and its claim last, so the next read of it is no person's draft. Negative control by mutation:
/// the probe ended on the torn read with no look at the fresh grid's composer: the probe idle and
/// the `/usage` a person's draft.
#[test]
fn a_look_with_no_composer_ends_no_probe_whose_line_the_next_read_shows() {
    let standing = with_draft(&ended(PROBE_SAID), USAGE_COMMAND);
    let caret = standing
        .iter()
        .rposition(|r| r.starts_with('❯'))
        .expect("the composer");
    let torn = standing[..caret - 1].to_vec();
    let mut m = Mock::new(vec![torn, standing]);
    m.gen_fence = true;
    m.human_seq = Some(0);
    m.cursor_col = Some(2 + USAGE_COMMAND.len());
    let mut s = virtual_session(&mut m, None);
    let mut heard = Heard::default();
    let screen = s.screen().expect("the torn read");
    assert!(
        s.reader(&screen.rows).composer(&screen.rows).is_none(),
        "no composer: {:#?}",
        screen.rows
    );
    let turn = s.point_of(screen);
    s.probe_left_seen(&turn);
    let mut state = Looking {
        approved: Vec::new(),
        gone_first: true,
        handed: None,
        handed_seq: None,
        handed_submit: None,
        moved: Saw::default(),
        survey: Survey::Closed,
        read_back: None,
        face_owed: false,
    };
    let deadline = s.fx.mono() + Duration::from_secs(60);
    let opts = hosted(60);
    s.usage_panel_look(&turn, &mut state, deadline, false, &opts, &mut heard)
        .expect("the look");
    assert_ne!(
        s.probe,
        UsageProbe::Idle,
        "the probe lasts: {:#?}",
        heard.noted
    );
    s.screen().expect("the next read");
    assert_eq!(
        s.person_at(),
        None,
        "the probe's own line, no person's draft"
    );
}
