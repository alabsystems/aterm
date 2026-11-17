// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A title a program prints is a claim at the in-band rank: read through the real engine and
//! the drift flush, coalesced, scoped to the open agent, never standing over a title the
//! session's own program or a person stated, paced by its own bucket and drained at the log's
//! settle tick.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_slog::{Actor, Kind, Node, Rank, Tier};

use super::super::tests::{Hand, at, hand_clock_from, opened, with_claude};
use super::InbandRead;
use crate::session_log::observed::{PROGRAM_BURST, PROGRAM_REFILL_MS, TitleStands};
use crate::session_log::{BlockRead, DescRefused, DescWrite};
use crate::session_timeline::{MetaEdit, MetaField, SessionTimeline, write_session_meta};
use crate::{App, WindowId};

type TermArc = Arc<Mutex<aterm_core::terminal::Terminal>>;

/// A new headless app's front tab: its session, terminal and timeline.
fn front_tab() -> (App, u64, TermArc, Arc<Mutex<SessionTimeline>>) {
    let app = App::headless_for_test();
    let front = app.front_terminal(WindowId(0)).expect("a front terminal");
    let (session, term) = (front.session, Arc::clone(&front.term));
    let timeline = Arc::clone(&app.pool.get(session).expect("pooled").ctx.timeline);
    (app, session, term, timeline)
}

/// The terminal takes `bytes` and the drift flush reads it at `at`.
fn printed(app: &mut App, session: u64, term: &TermArc, bytes: &[u8], at: Instant) {
    crate::term_lock(term).process(bytes);
    assert!(app.flush_title_drift(session, at), "uncontended");
}

/// The `desc` records of `timeline`'s log, encoded at `tier`.
fn desc_lines(timeline: &Mutex<SessionTimeline>, tier: Tier) -> Vec<String> {
    let g = timeline.lock().unwrap();
    g.slog()
        .log()
        .window()
        .filter(|r| r.k == Kind::Desc)
        .map(|r| aterm_slog::encode(r, tier).expect("fits"))
        .collect()
}

/// The title a reader is shown: its text, its scope and its author's rank.
fn shown_title(timeline: &Mutex<SessionTimeline>) -> Option<(String, Node, Rank)> {
    let g = timeline.lock().unwrap();
    let now = g.slog().now().wall_ms;
    aterm_slog::shown(g.slog().log().state(), "title", now)
        .map(|s| (s.claim.value.clone(), s.scope, s.rank))
}

/// Every `title` claim, as `described` lists it: its text, scope, rank and whether it stands.
fn title_claims(timeline: &Mutex<SessionTimeline>) -> Vec<(String, Node, Rank, bool)> {
    let g = timeline.lock().unwrap();
    let now = g.slog().now().wall_ms;
    g.slog()
        .log()
        .state()
        .desc
        .listing(now)
        .iter()
        .filter(|r| r.slot == "title")
        .map(|r| (r.claim.value.clone(), r.scope, r.rank, r.standing))
        .collect()
}

/// `ESC ] 7501 ; <body> ST`.
fn report(body: &str) -> Vec<u8> {
    format!("\x1b]7501;{body}\x1b\\").into_bytes()
}

/// A window title of the program's own words, as the drift flush reads it with no 7501 title.
fn own(shown: &str) -> InbandRead<'_> {
    InbandRead {
        shown,
        untitled: false,
        stands: TitleStands::Own,
        program: None,
        program_seq: 0,
    }
}

/// The in-band author reads `read` at `ms` on `hand`'s clock.
fn observed(timeline: &Mutex<SessionTimeline>, hand: &Hand, read: &InbandRead<'_>, ms: u64) {
    hand.set(ms);
    timeline
        .lock()
        .unwrap()
        .slog_mut()
        .observe_inband_title(read, at(ms));
}

/// The in-band author's tick at `ms` on `hand`'s clock.
fn ticked(timeline: &Mutex<SessionTimeline>, hand: &Hand, ms: u64) {
    hand.set(ms);
    timeline.lock().unwrap().slog_mut().inband_tick(at(ms));
}

/// The in-band author's records, at the host tier.
fn inband_lines(timeline: &Mutex<SessionTimeline>) -> Vec<String> {
    desc_lines(timeline, Tier::Host)
        .into_iter()
        .filter(|l| l.contains(" desc field=title src=engine "))
        .collect()
}

/// When the session's log wants its settle tick.
fn log_wake(timeline: &Mutex<SessionTimeline>) -> Option<u64> {
    timeline.lock().unwrap().slog_wake()
}

/// "QZP8 Refactor parser", base64.
const REFACTOR_B64: &str = "UVpQOCBSZWZhY3RvciBwYXJzZXI=";

/// AN IN-BAND TITLE IS A CLAIM AT THE IN-BAND RANK (streams design §6, in-band): an OSC 2 title
/// a program prints, read through the real engine and the drift flush, is the session's `title`
/// claim by nobody at `src=engine`, ranked `inband`, on `-` while no agent runs; its ring line at
/// the host tier is `mark - desc field=title src=engine conf=claim` with no text and no `by=`, its
/// text kept at the content tier without the status glyph. A spinner's next phase writes nothing.
/// OSC 0, and a 7501 report's `title` from a program aterm does not know, are claims the same
/// way, and a spinner frame in front of the window title leaves the 7501 title standing. With an
/// agent run open the claim is that agent's (`r<n>`), and the claim on `-` is withdrawn first.
///
/// CONTROLS: a command line set as the title (before it runs, and while it runs under a title cut
/// with `...`, which only the command-line guard reads as the line), the shell's own title at its
/// prompt and a child record's `title` write no claim text: the shell's title withdraws the
/// program's claim, and no word of the command line or the child's title is in any record.
#[test]
fn an_in_band_title_is_a_claim_at_the_in_band_rank() {
    let t0 = Instant::now();
    let tick = |s: u64| t0 + Duration::from_secs(s);
    let (mut app, session, term, timeline) = front_tab();
    printed(
        &mut app,
        session,
        &term,
        "\x1b]2;\u{2733} QZP8 Fix the parser\x07".as_bytes(),
        t0,
    );
    assert_eq!(
        shown_title(&timeline),
        Some((
            "QZP8 Fix the parser".to_string(),
            Node::SESSION,
            Rank::Inband
        ))
    );
    let host = desc_lines(&timeline, Tier::Host);
    assert_eq!(host.len(), 1, "{host:?}");
    assert!(
        host[0].contains(" mark - desc field=title src=engine conf=claim t=")
            && !host[0].contains("QZP8")
            && !host[0].contains(" by="),
        "the host tier says only that the program titled itself: {host:?}"
    );
    let content = desc_lines(&timeline, Tier::Content);
    assert!(
        content[0].contains(" title=QZP8%20Fix%20the%20parser "),
        "{content:?}"
    );
    for (i, frame) in ["\u{280b}", "\u{2819}", "\u{2733}"].iter().enumerate() {
        let bytes = format!("\x1b]2;{frame} QZP8 Fix the parser\x07");
        printed(
            &mut app,
            session,
            &term,
            bytes.as_bytes(),
            tick(1 + i as u64),
        );
    }
    assert_eq!(
        desc_lines(&timeline, Tier::Host).len(),
        1,
        "a spinner's phase or a status glyph writes nothing"
    );

    printed(
        &mut app,
        session,
        &term,
        b"\x1b]0;QZP8 Review the fold\x07",
        tick(4),
    );
    assert_eq!(
        shown_title(&timeline),
        Some((
            "QZP8 Review the fold".to_string(),
            Node::SESSION,
            Rank::Inband
        )),
        "OSC 0"
    );
    // "QZP8 Ship the card", base64.
    let titled = report("state=working:app=kimi-code:title=UVpQOCBTaGlwIHRoZSBjYXJk");
    printed(&mut app, session, &term, &titled, tick(5));
    assert_eq!(
        shown_title(&timeline),
        Some((
            "QZP8 Ship the card".to_string(),
            Node::SESSION,
            Rank::Inband
        )),
        "a 7501 title from a program aterm does not know"
    );
    printed(
        &mut app,
        session,
        &term,
        "\x1b]2;\u{280b} QZP8 Review the fold\x07".as_bytes(),
        tick(5) + Duration::from_millis(500),
    );
    assert_eq!(
        shown_title(&timeline).map(|(text, ..)| text).as_deref(),
        Some("QZP8 Ship the card"),
        "a spinner frame of the window title leaves the program's 7501 title standing"
    );
    assert_eq!(desc_lines(&timeline, Tier::Host).len(), 3);
    // A child record's title, "QZP8CHILD": no claim.
    let child = report("state=working:app=kimi-code:id=sub_1:title=UVpQOENISUxE");
    printed(&mut app, session, &term, &child, tick(6));
    assert_eq!(
        desc_lines(&timeline, Tier::Host).len(),
        3,
        "CONTROL: a child"
    );

    with_claude(&timeline);
    let run = timeline
        .lock()
        .unwrap()
        .slog()
        .log()
        .window()
        .find(|r| r.k == Kind::Agent)
        .map(|r| r.node)
        .expect("the agent's run opened");
    printed(
        &mut app,
        session,
        &term,
        "\x1b]2;\u{2733} QZP8 Agent topic\x07".as_bytes(),
        tick(7),
    );
    assert_eq!(
        shown_title(&timeline),
        Some(("QZP8 Agent topic".to_string(), run, Rank::Inband)),
        "the open agent's topic"
    );
    let claims = title_claims(&timeline);
    assert_eq!(
        claims,
        [("QZP8 Agent topic".to_string(), run, Rank::Inband, true)],
        "the claim on - was withdrawn"
    );
    let host = desc_lines(&timeline, Tier::Host);
    assert!(
        host[host.len() - 2].contains(" mark - desc field=title src=engine conf=claim ")
            && host[host.len() - 1].contains(&format!(
                " mark {run} desc field=title src=engine conf=claim "
            )),
        "the withdrawal on - first, then the agent's claim: {host:?}"
    );

    // CONTROLS: the shell's own title at its prompt, then a command line set as the title.
    let before = desc_lines(&timeline, Tier::Host).len();
    printed(
        &mut app,
        session,
        &term,
        b"\x1b]133;A\x1b\\\x1b]133;B\x1b\\\x1b]0;~/QZP8DIR\x07",
        tick(8),
    );
    assert_eq!(
        title_claims(&timeline),
        [],
        "the shell's title withdraws it"
    );
    assert_eq!(desc_lines(&timeline, Tier::Host).len(), before + 1);
    printed(
        &mut app,
        session,
        &term,
        b"\x1b]633;E;printf\\x20QZP8WORD\x1b\\\x1b]0;printf QZP8WORD\x07",
        tick(9),
    );
    assert_eq!(
        desc_lines(&timeline, Tier::Host).len(),
        before + 1,
        "a command line claims nothing"
    );
    // CONTROL where only the command-line guard decides: the line runs (133;C) under a title
    // cut with `...`, which `title_stands` reads as the line and `shell_titled` does not.
    printed(
        &mut app,
        session,
        &term,
        b"\x1b]133;C\x1b\\\x1b]0;printf QZP8W...\x07",
        tick(10),
    );
    {
        let t = crate::term_lock(&term);
        assert!(
            !crate::app_take_over::untitled_by_its_program(&t),
            "the control must not reach the line through `untitled`"
        );
        assert!(matches!(
            crate::session_status::title_stands(&t),
            TitleStands::CommandLine
        ));
    }
    assert_eq!(
        desc_lines(&timeline, Tier::Host).len(),
        before + 1,
        "a running command line, cut with `...`, claims nothing"
    );
    for l in desc_lines(&timeline, Tier::Content) {
        assert!(
            !l.contains("QZP8W") && !l.contains("QZP8DIR") && !l.contains("CHILD"),
            "no command line, shell title or child's title in a claim: {l}"
        );
    }
}

/// A PRINTED TITLE SEQUENCE DOES NOT DISPLACE THE SESSION'S OWN CLAIM: the session's own
/// program (a process inside it) claims the title, then `cat` prints a file holding
/// `ESC ] 2 ; … BEL`, which the engine takes and the drift flush reads. The program's title still
/// stands and stays pinned; the printed one is stored and listed at the in-band rank under it.
/// The same holds under a person's rename, whose pinned title the printed one leaves where it is.
///
/// CONTROL: in a session where nobody claimed the title, the same bytes make the in-band claim
/// stand, and still pin nothing.
#[test]
fn a_printed_title_sequence_does_not_displace_the_sessions_own_claim() {
    const OWN: &str = "s-0123456789abcdef0123";
    let t0 = Instant::now();
    let catted = |file: &str| {
        format!(
            "\x1b]133;A\x1b\\\x1b]133;B\x1b\\\x1b]633;E;cat {file}\x1b\\\x1b]0;cat {file}\x07\
             \x1b]133;C\x1b\\notes, line 1\r\n\x1b]2;QZP8 printed by {file}\x07notes, line 2\r\n"
        )
    };
    let (mut app, session, term, timeline) = front_tab();
    let ctx = app.pool.get(session).expect("pooled").ctx.clone();
    timeline.lock().unwrap().slog_mut().set_desc_own(OWN);
    let program = crate::session_log::sid_actor(OWN).expect("a minted sid");
    write_session_meta(&ctx, MetaField::Title, MetaEdit::Set("Mine"), program)
        .expect("the program's title");
    let pinned = || ctx.meta.lock().unwrap().user_title.clone();
    assert_eq!(pinned().as_deref(), Some("Mine"));
    printed(&mut app, session, &term, catted("a.txt").as_bytes(), t0);
    assert_eq!(
        shown_title(&timeline),
        Some(("Mine".to_string(), Node::SESSION, Rank::Program)),
        "the program's title stands"
    );
    assert_eq!(
        title_claims(&timeline),
        [
            ("Mine".to_string(), Node::SESSION, Rank::Program, true),
            (
                "QZP8 printed by a.txt".to_string(),
                Node::SESSION,
                Rank::Inband,
                false
            ),
        ],
        "the printed title is stored and listed under it"
    );
    assert_eq!(pinned().as_deref(), Some("Mine"));

    write_session_meta(
        &ctx,
        MetaField::Title,
        MetaEdit::Set("Person's"),
        Actor::Human,
    )
    .expect("a person's rename");
    printed(
        &mut app,
        session,
        &term,
        catted("b.txt").as_bytes(),
        t0 + Duration::from_secs(1),
    );
    assert_eq!(
        shown_title(&timeline),
        Some(("Person's".to_string(), Node::SESSION, Rank::Person)),
        "the person's title stands"
    );
    assert!(
        title_claims(&timeline).contains(&(
            "QZP8 printed by b.txt".to_string(),
            Node::SESSION,
            Rank::Inband,
            false
        )),
        "{:?}",
        title_claims(&timeline)
    );
    assert_eq!(pinned().as_deref(), Some("Person's"), "user_title unmoved");

    let (mut app, session, term, timeline) = front_tab();
    let ctx = app.pool.get(session).expect("pooled").ctx.clone();
    printed(&mut app, session, &term, catted("a.txt").as_bytes(), t0);
    assert_eq!(
        shown_title(&timeline),
        Some((
            "QZP8 printed by a.txt".to_string(),
            Node::SESSION,
            Rank::Inband
        )),
        "CONTROL: with no other claim the printed title stands"
    );
    assert_eq!(
        ctx.meta.lock().unwrap().user_title,
        None,
        "and pins nothing"
    );
}

/// A SPINNER FRAME LEAVES A PROGRAM'S 7501 TITLE STANDING: a 7501 root report's `title` and an
/// OSC 0 title read in one flush give the 7501 text, the program's own word; later flushes that
/// change only the window title's spinner phase or status glyph are no news, so they write no
/// record and the 7501 text still stands.
///
/// CONTROL: a window title whose words move takes the claim.
#[test]
fn a_spinner_frame_leaves_a_programs_7501_title_standing() {
    let t0 = Instant::now();
    let tick = |ms: u64| t0 + Duration::from_millis(ms);
    let (mut app, session, term, timeline) = front_tab();
    let mut bytes = report(&format!(
        "state=working:app=grok-build:title={REFACTOR_B64}"
    ));
    bytes.extend_from_slice("\x1b]0;\u{280b} grok\x07".as_bytes());
    printed(&mut app, session, &term, &bytes, t0);
    let refactor = Some((
        "QZP8 Refactor parser".to_string(),
        Node::SESSION,
        Rank::Inband,
    ));
    assert_eq!(
        shown_title(&timeline),
        refactor,
        "both moved: the 7501 title"
    );
    let before = desc_lines(&timeline, Tier::Host).len();
    for (i, frame) in ["\u{2819}", "\u{2839}", "\u{2733}"].iter().enumerate() {
        let bytes = format!("\x1b]0;{frame} grok\x07");
        printed(
            &mut app,
            session,
            &term,
            bytes.as_bytes(),
            tick(100 * (1 + i as u64)),
        );
        assert_eq!(shown_title(&timeline), refactor, "after {frame}");
    }
    assert_eq!(
        desc_lines(&timeline, Tier::Host).len(),
        before,
        "a spinner phase or a status glyph is no news"
    );

    printed(
        &mut app,
        session,
        &term,
        b"\x1b]0;grok editing foo\x07",
        tick(400),
    );
    assert_eq!(
        shown_title(&timeline),
        Some(("grok editing foo".to_string(), Node::SESSION, Rank::Inband)),
        "CONTROL: the window's words moved"
    );
    assert_eq!(desc_lines(&timeline, Tier::Host).len(), before + 1);
}

/// A PROMPT'S TITLE AFTER A COMMAND LINE WITHDRAWS WHAT THE PROGRAM LEFT: a program runs under
/// its command line as the window title, which claims nothing, titles itself by a 7501 report,
/// and ends without clearing it. The shell's title at the next prompt claims nothing either, but
/// it is other words than the command line's, so the window moved, and its move withdraws the
/// program's claim.
#[test]
fn a_prompts_title_after_a_command_line_withdraws_what_the_program_left() {
    let t0 = Instant::now();
    let tick = |s: u64| t0 + Duration::from_secs(s);
    let (mut app, session, term, timeline) = front_tab();
    printed(
        &mut app,
        session,
        &term,
        b"\x1b]133;A\x1b\\\x1b]133;B\x1b\\\x1b]633;E;grok\x1b\\\x1b]0;grok\x07\x1b]133;C\x1b\\",
        t0,
    );
    assert_eq!(title_claims(&timeline), [], "a command line claims nothing");
    let titled = report(&format!(
        "state=working:app=grok-build:title={REFACTOR_B64}"
    ));
    printed(&mut app, session, &term, &titled, tick(1));
    assert_eq!(
        title_claims(&timeline),
        [(
            "QZP8 Refactor parser".to_string(),
            Node::SESSION,
            Rank::Inband,
            true
        )]
    );
    printed(
        &mut app,
        session,
        &term,
        b"\x1b]133;D;0\x1b\\\x1b]133;A\x1b\\\x1b]133;B\x1b\\\x1b]0;~/QZP8DIR\x07",
        tick(2),
    );
    assert!(
        crate::app_take_over::untitled_by_its_program(&crate::term_lock(&term)),
        "the prompt's title is the shell's"
    );
    assert_eq!(
        title_claims(&timeline),
        [],
        "the prompt's title withdraws what the program left"
    );
}

/// A DECISION THE BUCKET HELD BACK IS WRITTEN AT THE LOG'S SETTLE TICK, WITH TAB STATUS OFF (so no
/// status sweep runs): a flood of distinct titles through the drift flush empties the in-band
/// bucket, and the flood's last title is held back; the flush arms the log's wake for the
/// bucket's next token, and the loop's settle tick (`tick_session_logs`) writes the title then and
/// arms nothing more. The same holds for a withdrawal: the shell's title at its prompt, read with
/// the bucket empty, is held back while the claim stands, and the tick withdraws it.
///
/// CONTROL: before the token is back, the log's tick writes nothing and the claim stands.
#[test]
fn a_held_in_band_decision_is_written_at_the_settle_tick_with_tab_status_off() {
    let (mut app, session, term, timeline) = front_tab();
    app.config.tab_status = Some(false);
    let base = crate::turn_ledger::now_ms();
    let hand = hand_clock_from(&timeline, base);
    let t0 = Instant::now();
    let burst = usize::try_from(PROGRAM_BURST).expect("small");
    for i in 1..=burst + 1 {
        let bytes = format!("\x1b]2;QZP8 Building {i}\x07");
        printed(&mut app, session, &term, bytes.as_bytes(), t0);
    }
    let building = |i: usize| Some((format!("QZP8 Building {i}"), Node::SESSION, Rank::Inband));
    assert_eq!(inband_lines(&timeline).len(), burst, "the burst is spent");
    assert_eq!(shown_title(&timeline), building(burst));
    let token = base + PROGRAM_REFILL_MS;
    assert_eq!(
        app.session_status.slog_wakes.next(),
        Some(token),
        "the flush armed the tick for the next token"
    );

    hand.set(PROGRAM_REFILL_MS - 1);
    let _ = timeline.lock().unwrap().slog_tick();
    assert_eq!(
        inband_lines(&timeline).len(),
        burst,
        "CONTROL: no token yet"
    );
    assert_eq!(shown_title(&timeline), building(burst));

    hand.set(PROGRAM_REFILL_MS);
    crate::turn_ledger::advance_for_test(PROGRAM_REFILL_MS);
    app.tick_session_logs();
    assert_eq!(inband_lines(&timeline).len(), burst + 1);
    assert_eq!(
        shown_title(&timeline),
        building(burst + 1),
        "the held title"
    );
    assert!(
        app.session_status.slog_wakes.is_empty(),
        "nothing held, nothing armed"
    );

    printed(
        &mut app,
        session,
        &term,
        b"\x1b]133;A\x1b\\\x1b]133;B\x1b\\\x1b]0;~/QZP8DIR\x07",
        t0 + Duration::from_secs(1),
    );
    assert_eq!(
        shown_title(&timeline),
        building(burst + 1),
        "the withdrawal is held back"
    );
    assert_eq!(
        app.session_status.slog_wakes.next(),
        Some(token + PROGRAM_REFILL_MS)
    );
    hand.set(2 * PROGRAM_REFILL_MS);
    crate::turn_ledger::advance_for_test(PROGRAM_REFILL_MS);
    app.tick_session_logs();
    assert_eq!(title_claims(&timeline), [], "the tick withdrew it");
    assert_eq!(inband_lines(&timeline).len(), burst + 2);
    assert!(app.session_status.slog_wakes.is_empty());
}

/// AN IN-BAND FLOOD WAITS FOR ITS OWN BUCKET AND SPENDS NO MARK TOKEN: 22 distinct titles at one
/// instant write [`PROGRAM_BURST`] claims; the newest decision replaces the one still waiting, so
/// the 21st title is in no record, and the log's wake asks for the tick at the next token. The
/// mark limiter's overflow bucket, which a `Nobody` author shares with every author of no named
/// slot, is untouched: at the same instant it still takes a whole burst of claims from such an
/// author, then refuses the next. The tick a second later writes the 22nd title, and then nothing
/// more is owed.
///
/// CONTROL: a tick a millisecond short of the token writes nothing, and a second tick at the
/// token writes nothing either.
#[test]
fn an_in_band_flood_waits_for_its_own_bucket_and_spends_no_mark_token() {
    let (tl, hand) = opened();
    let burst = usize::try_from(PROGRAM_BURST).expect("small");
    for i in 1..=22 {
        observed(&tl, &hand, &own(&format!("QZP8 t{i}")), 10);
    }
    assert_eq!(inband_lines(&tl).len(), burst);
    assert_eq!(
        shown_title(&tl),
        Some(("QZP8 t20".to_string(), Node::SESSION, Rank::Inband))
    );
    for l in desc_lines(&tl, Tier::Content) {
        assert!(!l.contains("QZP8%20t21"), "replaced while it waited: {l}");
    }
    assert_eq!(log_wake(&tl), Some(10 + PROGRAM_REFILL_MS));

    {
        let mut g = tl.lock().unwrap();
        for i in 0..aterm_slog::MARK_BURST {
            let text = format!("probe {i}");
            g.slog_mut()
                .claim_desc(
                    &DescWrite::of_session("stage", Some(&text)),
                    Actor::Vendor,
                    at(10),
                )
                .expect("the overflow bucket is full");
        }
        assert!(matches!(
            g.slog_mut().claim_desc(
                &DescWrite::of_session("stage", Some("one more")),
                Actor::Vendor,
                at(10),
            ),
            Err(DescRefused::Busy(_))
        ));
    }

    ticked(&tl, &hand, 10 + PROGRAM_REFILL_MS - 1);
    assert_eq!(inband_lines(&tl).len(), burst, "CONTROL: no token yet");
    ticked(&tl, &hand, 10 + PROGRAM_REFILL_MS);
    assert_eq!(inband_lines(&tl).len(), burst + 1);
    assert_eq!(
        shown_title(&tl),
        Some(("QZP8 t22".to_string(), Node::SESSION, Rank::Inband))
    );
    assert_eq!(log_wake(&tl), None, "nothing held, nothing armed");
    ticked(&tl, &hand, 10 + PROGRAM_REFILL_MS);
    assert_eq!(inband_lines(&tl).len(), burst + 1, "CONTROL: nothing more");
}

/// A CLAIM MADE BEFORE THE AGENT OPENED MOVES TO IT AT THE TICK: a title claimed on `-` while no
/// agent ran stays there when Claude Code's run opens, and the log's wake asks for the tick; the
/// tick withdraws the claim on `-`, then makes it on the agent's run, where it is the topic.
///
/// CONTROL: a second tick writes nothing, and nothing more is owed.
#[test]
fn a_claim_made_before_the_agent_opened_moves_to_it_at_the_tick() {
    let (tl, hand) = opened();
    observed(&tl, &hand, &own("QZP8 early topic"), 10);
    let early = "QZP8 early topic".to_string();
    assert_eq!(
        shown_title(&tl),
        Some((early.clone(), Node::SESSION, Rank::Inband))
    );
    assert_eq!(log_wake(&tl), None);
    with_claude(&tl);
    let run = tl
        .lock()
        .unwrap()
        .slog()
        .log()
        .window()
        .find(|r| r.k == Kind::Agent)
        .map(|r| r.node)
        .expect("the agent's run opened");
    assert_eq!(
        title_claims(&tl),
        [(early.clone(), Node::SESSION, Rank::Inband, true)],
        "nothing moved yet"
    );
    assert!(log_wake(&tl).is_some(), "the move is owed");
    let before = inband_lines(&tl).len();

    ticked(&tl, &hand, 20);
    assert_eq!(
        title_claims(&tl),
        [(early.clone(), run, Rank::Inband, true)]
    );
    let lines = inband_lines(&tl);
    assert_eq!(lines.len(), before + 2, "{lines:?}");
    assert!(
        lines[before].contains(" mark - desc ")
            && lines[before + 1].contains(&format!(" mark {run} desc ")),
        "the withdrawal on - first, then the agent's claim: {lines:?}"
    );
    ticked(&tl, &hand, 30);
    assert_eq!(inband_lines(&tl).len(), before + 2, "CONTROL: nothing more");
    assert_eq!(log_wake(&tl), None);
}

/// AN AGENT'S CLAIM LAPSES AT ITS END AND IS NOT MADE AGAIN UNTIL A TITLE MOVES: a title claimed
/// on Claude Code's open run lapses when the run ends; neither the tick nor a read of the same
/// title makes it again on `-`, and nothing is owed.
///
/// CONTROL: a title whose words move is claimed on `-`.
#[test]
fn an_agents_claim_lapses_at_its_end_and_is_not_made_again_until_a_title_moves() {
    let (tl, hand) = opened();
    with_claude(&tl);
    let run = tl
        .lock()
        .unwrap()
        .slog()
        .log()
        .window()
        .find(|r| r.k == Kind::Agent)
        .map(|r| r.node)
        .expect("the agent's run opened");
    let topic = own("QZP8 agent topic");
    observed(&tl, &hand, &topic, 10);
    assert_eq!(
        shown_title(&tl),
        Some(("QZP8 agent topic".to_string(), run, Rank::Inband))
    );
    {
        let mut g = tl.lock().unwrap();
        hand.set(20);
        assert!(g.note_foreground_group(100));
        assert!(g.set_program(100, Some("zsh".into())));
        let pgid = g.agent().program_pgid;
        let _ = g.sweep_slog(&BlockRead::default(), false, 100, pgid);
        assert!(!g.slog().log().is_open(run), "the agent's run ended");
    }
    let before = desc_lines(&tl, Tier::Host).len();
    ticked(&tl, &hand, 30);
    observed(&tl, &hand, &topic, 30);
    assert_eq!(shown_title(&tl), None, "the claim lapsed with its agent");
    assert_eq!(
        desc_lines(&tl, Tier::Host).len(),
        before,
        "and is not made again on -"
    );
    assert_eq!(log_wake(&tl), None);

    observed(&tl, &hand, &own("QZP8 after"), 40);
    assert_eq!(
        shown_title(&tl),
        Some(("QZP8 after".to_string(), Node::SESSION, Rank::Inband)),
        "CONTROL: a title that moves"
    );
}

/// AN IN-BAND CLAIM THE FOLD DROPPED IS NOT SENT AGAIN UNTIL IT MOVES: eight writers aterm ran,
/// told apart by `as=`, fill the session's `title` slot with claims that all rank above the
/// in-band one, so the fold drops the in-band claim as soon as it is written. Spinner frames of
/// the same title, and a 7501 report naming the same text, write nothing more: one in-band
/// record, and the slot's count of claims dropped is one.
///
/// CONTROL: a title whose text moves is sent again, once, and dropped again.
#[test]
fn an_in_band_claim_the_fold_dropped_is_not_sent_again_until_it_moves() {
    let (tl, hand) = opened();
    {
        let mut g = tl.lock().unwrap();
        for i in 0..8 {
            let (name, text) = (format!("w{i}"), format!("Writer {i}"));
            let write = DescWrite {
                as_name: Some(&name),
                ..DescWrite::of_session("title", Some(&text))
            };
            g.slog_mut()
                .claim_desc(&write, Actor::Aterm, at(1))
                .expect("a writer's title");
        }
    }
    let dropped = |tl: &Mutex<SessionTimeline>| {
        let g = tl.lock().unwrap();
        let now = g.slog().now().wall_ms;
        g.slog()
            .log()
            .state()
            .desc
            .listing(now)
            .iter()
            .find(|r| r.scope == Node::SESSION && r.slot == "title")
            .map(|r| r.dropped)
    };
    observed(&tl, &hand, &own("QZP8 Fix the parser"), 10);
    assert_eq!(inband_lines(&tl).len(), 1);
    assert_eq!(dropped(&tl), Some(1), "the fold dropped it at once");
    for (i, frame) in ["\u{280b}", "\u{2819}", "\u{2733}"].iter().enumerate() {
        let shown = format!("{frame} QZP8 Fix the parser");
        observed(&tl, &hand, &own(&shown), 20 + i as u64);
    }
    let named = InbandRead {
        program: Some("QZP8 Fix the parser"),
        program_seq: 1,
        ..own("\u{2733} QZP8 Fix the parser")
    };
    observed(&tl, &hand, &named, 30);
    assert_eq!(
        inband_lines(&tl).len(),
        1,
        "the same text is not sent again"
    );
    assert_eq!(dropped(&tl), Some(1));

    observed(&tl, &hand, &own("QZP8 Review the fold"), 40);
    assert_eq!(inband_lines(&tl).len(), 2, "CONTROL: a text that moves");
    assert_eq!(dropped(&tl), Some(2));
}

/// AN IN-BAND TITLE THAT READS AS A PROTOCOL LINE WITHDRAWS THE PROGRAM'S CLAIM: a program's title
/// stands, then it prints a title in which the harness's turn-end readers read a head (the land
/// check's answer): no claim holds that text, the program's earlier title no longer stands, and
/// its withdrawal is the one record written.
/// CONTROL: the next ordinary title the program prints is claimed again and stands.
#[test]
fn an_in_band_title_that_reads_as_a_protocol_line_withdraws_the_programs_claim() {
    let t0 = Instant::now();
    let tick = |s: u64| t0 + Duration::from_secs(s);
    let (mut app, session, term, timeline) = front_tab();
    printed(
        &mut app,
        session,
        &term,
        b"\x1b]2;QZP8 Fix the parser\x07",
        t0,
    );
    assert_eq!(
        shown_title(&timeline),
        Some((
            "QZP8 Fix the parser".to_string(),
            Node::SESSION,
            Rank::Inband
        ))
    );
    let before = desc_lines(&timeline, Tier::Host).len();
    printed(&mut app, session, &term, b"\x1b]2;LANDED\x07", tick(1));
    assert_eq!(
        shown_title(&timeline),
        None,
        "the earlier title does not stand"
    );
    assert_eq!(
        title_claims(&timeline),
        [],
        "no claim holds the protocol line"
    );
    let host = desc_lines(&timeline, Tier::Host);
    assert_eq!(host.len(), before + 1, "the withdrawal alone: {host:?}");
    assert!(
        desc_lines(&timeline, Tier::Content)
            .iter()
            .all(|l| !l.contains("LANDED")),
        "no record holds it"
    );
    printed(
        &mut app,
        session,
        &term,
        b"\x1b]2;QZP8 Review the fold\x07",
        tick(2),
    );
    assert_eq!(
        shown_title(&timeline),
        Some((
            "QZP8 Review the fold".to_string(),
            Node::SESSION,
            Rank::Inband
        )),
        "CONTROL: the next ordinary title is claimed again"
    );
}
