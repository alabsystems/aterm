// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The grid spine's tests. Two of the fixtures are MEASURED captures of the
//! live, ADOPTED Claude Code session that hosted this work (2026-09-21, this
//! Mac): [`REAL_STATUS`] and [`REAL_GRID`]. That session reads `detail=-`
//! with `blocks` → `OK 0`, which is the case the first design would have
//! missed silently, so it is the case these tests lead with. The rest are the
//! shipped `aterm_phase` fixtures — real screens saved from real sessions —
//! and screens built from the same composer geometry.

use super::*;
use crate::supervise::prompt::fixtures::{bash_one_row, composer, rows as rows_of};

/// MEASURED 2026-09-21, `aterm ctl @self status` from inside the live Claude
/// Code session: `detail=-`, `attribution=adopted`, `confidence=strong`.
const REAL_STATUS: &str = "OK schema=1 sid=0 subject=%E2%97%90%20Aterm%20wrapper%20harness%20design subject_source=osc observed=true phase=running since_ms=130288 outcome=none exit_code=- signal=- detail=- confidence=strong reasons=fg_job,content_activity attribution=adopted fs_consent=unknown conflict=false revision=172 enabled=true hold=0 fabric=absent fabric_rtt_ms=- fabric_link_age_ms=- identity=- hand=- level=quiet story=0";

/// MEASURED 2026-09-21, `aterm ctl @self text --json tail=14` on the same
/// session: a worker mid-turn (`✻ Waiting for 1 dynamic workflow to finish`)
/// with the composer frame under it.
const REAL_GRID: &str = r#"{"rows":["  with zero hooks installed. Each stage now also runs its own format, lint and test checks before committing, plus the","  help-surfaces check when it touches a doc comment, because that is what caught me out.","","  I will gate and push again once stages land.","","✻ Waiting for 1 dynamic workflow to finish","                                                                                                            ◎ /goal active (3h)","───────────────────────────────────────────────────────────────────────────────────────────────────────────────────── workspace ─","❯ keep going","─────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────────","  ⏵⏵ bypass permissions on · 1 monitor · ← for agents · ↓ to manage","","  ◯ harness-all-stages  Build stages 1-7 of the aterm harness end to end              0/1 agents done · 4m 0s · ↓ 131.1k tokens","  ⧉  aterm Wrapper Brief · canvas"],"cursor":{"row":51,"col":2,"visible":true,"style":"blinking_block"},"dims":{"rows":57,"cols":129},"seq":233817,"first":43}"#;

/// A real captured screen whose transcript holds TWO `You've reached your
/// Fable limit` notices — both history, the worker idle at its composer
/// after a `/model` switch. The containment control: a limit notice above
/// the live zone is not a live banner.
const IDLE_AFTER_LIMIT: &str =
    crate::supervise::prompt::fixtures::IDLE_AFTER_LIMIT_AND_MODEL_SWITCH;

fn ok(stdout: &str) -> CtlReply {
    CtlReply {
        code: 0,
        stdout: stdout.to_string(),
        stderr: String::new(),
    }
}

fn refused(stderr: &str) -> CtlReply {
    CtlReply {
        code: 1,
        stdout: String::new(),
        stderr: stderr.to_string(),
    }
}

/// A `status` line with the fields these tests vary and the measured shape
/// of the rest — `detail=-`, the adopted case, unless a test says otherwise.
fn status_line(phase: &str, revision: u64, detail: &str, since_ms: u64) -> String {
    format!(
        "OK schema=1 sid=s-1 observed=true phase={phase} since_ms={since_ms} outcome=none \
         exit_code=- signal=- detail={detail} confidence=strong \
         reasons=fg_job,content_activity attribution=adopted conflict=false \
         revision={revision} enabled=true hold=0"
    )
}

fn st(phase: &str, revision: u64) -> StatusSample {
    StatusSample::parse_line(&status_line(phase, revision, "-", 100)).expect("fixture status")
}

fn screen(rows: Vec<String>, seq: u64) -> Screen {
    Screen {
        rows,
        cursor_row: 0,
        cursor_col: 0,
        seq,
        first: 0,
    }
}

fn saved(text: &str) -> Vec<String> {
    text.lines().map(str::to_string).collect()
}

fn sample(status: StatusSample, grid: Option<Screen>) -> Sample {
    Sample {
        status,
        grid,
        offscreen: None,
        search: None,
    }
}

fn kinds(events: &[Event]) -> Vec<&'static str> {
    events.iter().map(|e| e.kind.name()).collect()
}

fn find<'a>(events: &'a [Event], name: &str) -> Option<&'a Event> {
    events.iter().find(|e| e.kind.name() == name)
}

/// An idle Claude Code screen: one thing said, then the composer.
fn idle_screen(footer: &str) -> Vec<String> {
    let mut r = rows_of(&["⏺ All done."]);
    r.extend(composer(footer));
    r
}

// ---------------------------------------------------------------------------
// The status reply
// ---------------------------------------------------------------------------

#[test]
fn the_measured_adopted_status_line_parses_with_detail_absent() {
    let s = StatusSample::parse(&ok(REAL_STATUS)).expect("the measured line parses");
    assert_eq!(s.phase, SessionPhase::Running);
    assert_eq!(s.confidence, Confidence::Strong);
    assert_eq!(s.revision, Some(172));
    assert_eq!(s.since_ms, Some(130_288));
    assert_eq!(s.attribution, Attribution::Adopted);
    assert_eq!(
        s.detail, None,
        "THE case: an adopted session owns no shell-integration block, so `detail=` is `-`"
    );
    assert!(s.has_reason("fg_job") && s.has_reason("content_activity"));
    assert!(s.observed && s.enabled && !s.hold);
}

#[test]
fn an_unknown_schema_is_refused_rather_than_parsed_best_effort() {
    // THE NEGATIVE CONTROL the catalog asks for: "reject an unknown schema
    // MAJOR rather than best-effort parsing".
    let wrong = REAL_STATUS.replace("schema=1", "schema=2");
    assert_eq!(
        StatusSample::parse(&ok(&wrong)),
        Err(ObserveError::Schema(2))
    );
    assert!(StatusSample::parse_line("OK sid=s-1 phase=idle").is_err());
}

#[test]
fn a_refused_verb_is_an_error_and_never_an_empty_sample() {
    assert_eq!(
        StatusSample::parse(&refused("ERR denied\n")),
        Err(ObserveError::Refused("ERR denied".to_string()))
    );
    assert!(matches!(
        StatusSample::parse(&ok("nothing like a reply")),
        Err(ObserveError::Malformed(_))
    ));
}

#[test]
fn a_header_framed_on_stderr_is_read_like_one_on_stdout() {
    let reply = CtlReply {
        code: 0,
        stdout: String::new(),
        stderr: format!("aterm-ctl: {REAL_STATUS}"),
    };
    let s = StatusSample::parse(&reply).expect("the stderr-framed header");
    assert_eq!(s.revision, Some(172));
}

#[test]
fn an_unknown_phase_confidence_or_outcome_token_reads_unknown_never_an_error() {
    let line =
        status_line("teleporting", 4, "-", 1).replace("confidence=strong", "confidence=psychic");
    let s = StatusSample::parse_line(&line).expect("unknown tokens are not errors");
    assert_eq!(s.phase, SessionPhase::Unknown);
    assert_eq!(s.confidence, Confidence::Unknown);
    assert_eq!(Outcome::parse("melted"), Outcome::None);
    assert_eq!(Attribution::parse("borrowed"), Attribution::Unknown);
}

#[test]
fn detail_is_pct_decoded_and_a_dash_is_absence() {
    let s =
        StatusSample::parse_line(&status_line("running", 1, "targo%20test", 1)).expect("detail");
    assert_eq!(s.detail.as_deref(), Some("targo test"));
    let s = StatusSample::parse_line(&status_line("running", 1, "-", 1)).expect("dash");
    assert_eq!(s.detail, None);
}

#[test]
fn reasons_are_bounded_in_count_and_in_length() {
    let many: Vec<String> = (0..40).map(|i| format!("r{i}")).collect();
    let line = status_line("running", 1, "-", 1).replace(
        "reasons=fg_job,content_activity",
        &format!("reasons={}", many.join(",")),
    );
    let s = StatusSample::parse_line(&line).expect("many reasons");
    assert_eq!(s.reasons.len(), REASONS_CAP);
    assert!(s.reasons.iter().all(|r| r.len() <= 32));
}

#[test]
fn confidence_orders_and_the_weaker_of_two_is_the_safe_answer() {
    assert!(Confidence::Unknown < Confidence::Heuristic);
    assert!(Confidence::Heuristic < Confidence::Strong);
    assert!(Confidence::Strong < Confidence::Exact);
    assert_eq!(
        Confidence::Exact.weaker(Confidence::Heuristic),
        Confidence::Heuristic
    );
    assert_eq!(
        Confidence::Unknown.weaker(Confidence::Exact),
        Confidence::Unknown
    );
}

#[test]
fn a_search_reply_is_read_from_either_frame_and_a_refusal_is_an_error() {
    let cli = CtlReply {
        code: 0,
        stdout: "39 96 5\n".to_string(),
        stderr: "aterm-ctl: OK 1 (search)\n".to_string(),
    };
    assert_eq!(
        SearchHits::parse(&cli, "limit").expect("cli frame"),
        SearchHits {
            pattern: "limit".to_string(),
            hits: 1
        }
    );
    assert_eq!(
        SearchHits::parse(&ok("OK 0\n"), "limit")
            .expect("raw frame")
            .hits,
        0
    );
    assert!(SearchHits::parse(&refused("ERR denied"), "limit").is_err());
}

// ---------------------------------------------------------------------------
// The revision gate
// ---------------------------------------------------------------------------

#[test]
fn needs_grid_is_the_presence_gate_an_idle_session_costs_nothing() {
    let mut o = Observer::new();
    assert!(o.needs_grid(Some(7)), "never read: the grid is due");
    assert!(o.needs_grid(None));
    o.on_sample(
        &sample(
            st("running", 7),
            Some(screen(idle_screen("? for shortcuts"), 1)),
        ),
        0,
    );
    // `on_sample` alone does not mark the grid read — `read` does, because it
    // is what performed the read. The gate is exercised through `read` below.
    assert!(o.needs_grid(Some(7)));
}

#[test]
fn read_reads_status_every_pass_and_the_grid_only_when_revision_moved() {
    struct Fixture {
        status: String,
        status_calls: usize,
        grid_calls: usize,
        tail: usize,
    }
    impl Introspect for Fixture {
        fn status(&mut self) -> CtlReply {
            self.status_calls += 1;
            ok(&self.status)
        }
        fn text_json_tail(&mut self, rows: usize) -> CtlReply {
            self.grid_calls += 1;
            self.tail = rows;
            ok(REAL_GRID)
        }
    }
    let mut fx = Fixture {
        status: REAL_STATUS.to_string(),
        status_calls: 0,
        grid_calls: 0,
        tail: 0,
    };
    let mut o = Observer::new();
    o.read(&mut fx, 10).expect("first pass");
    assert_eq!((fx.status_calls, fx.grid_calls), (1, 1));
    assert_eq!(fx.tail, TAIL_ROWS, "the shipped forty rows");
    o.read(&mut fx, 20).expect("second pass, revision unmoved");
    assert_eq!(
        (fx.status_calls, fx.grid_calls),
        (2, 1),
        "an idle session costs one 378-byte status read and no grid read"
    );
    fx.status = REAL_STATUS.replace("revision=172", "revision=173");
    o.read(&mut fx, 30).expect("third pass, revision moved");
    assert_eq!((fx.status_calls, fx.grid_calls), (3, 2));
}

#[test]
fn an_unreadable_grid_degrades_the_pass_and_never_fails_it() {
    struct Fixture;
    impl Introspect for Fixture {
        fn status(&mut self) -> CtlReply {
            ok(REAL_STATUS)
        }
        fn text_json_tail(&mut self, _rows: usize) -> CtlReply {
            refused("ERR busy")
        }
    }
    let mut o = Observer::new();
    let events = o
        .read(&mut Fixture, 1)
        .expect("status still answered")
        .events;
    assert!(kinds(&events).contains(&"started"));
    assert!(
        o.needs_grid(Some(172)),
        "the grid stays due, so the next pushed frame retries it"
    );
}

#[test]
fn a_refused_status_fails_the_pass_and_emits_nothing() {
    struct Fixture;
    impl Introspect for Fixture {
        fn status(&mut self) -> CtlReply {
            refused("ERR halted")
        }
        fn text_json_tail(&mut self, _rows: usize) -> CtlReply {
            ok(REAL_GRID)
        }
    }
    let mut o = Observer::new();
    assert_eq!(
        o.read(&mut Fixture, 1),
        Err(ObserveError::Refused("ERR halted".to_string()))
    );
    assert!(o.program().is_none());
}

#[test]
fn the_default_transport_answers_unsupported_for_the_on_demand_verbs() {
    struct Minimal;
    impl Introspect for Minimal {
        fn status(&mut self) -> CtlReply {
            ok(REAL_STATUS)
        }
        fn text_json_tail(&mut self, _rows: usize) -> CtlReply {
            ok(REAL_GRID)
        }
    }
    let mut m = Minimal;
    assert!(!m.offscreen(0, 10).ok());
    assert!(!m.search("limit").ok());
    let mut o = Observer::new();
    assert!(
        !o.read(&mut m, 1)
            .expect("a two-verb host still works")
            .events
            .is_empty()
    );
}

// ---------------------------------------------------------------------------
// Started — and the adopted case
// ---------------------------------------------------------------------------

#[test]
fn the_first_sample_names_the_program_from_status_detail() {
    let mut o = Observer::new();
    let s = StatusSample::parse_line(&status_line("running", 1, "claude", 10)).expect("detail");
    let events = o.on_sample(&sample(s, None), 5);
    let started = find(&events, "started").expect("started");
    assert_eq!(
        started.kind,
        EventKind::Started {
            program: Some("claude".to_string())
        }
    );
    assert!(started.has_reason(Reason::StatusDetail) && started.has_reason(Reason::FirstSample));
    assert!(started.has_reason(Reason::FgJob));
    assert_eq!(started.confidence, Confidence::Strong);
    assert!(started.sources.has(Source::Status));
    assert_eq!(o.program(), Some("claude"));
}

#[test]
fn the_adopted_session_names_its_program_from_the_grid_when_detail_is_absent() {
    // THE CASE THAT KILLED THE FIRST DESIGN: `detail=-`, `blocks` → OK 0,
    // zero hooks. Both fixtures are the live session's own measured bytes.
    let status = StatusSample::parse(&ok(REAL_STATUS)).expect("measured status");
    assert_eq!(status.detail, None);
    let grid = parse_text_json(REAL_GRID).expect("measured grid");
    let mut o = Observer::new();
    let events = o.on_sample(&sample(status, Some(grid)), 1);
    let started = find(&events, "started").expect("started");
    assert_eq!(
        started.kind,
        EventKind::Started {
            program: Some("claude".to_string())
        }
    );
    assert!(started.has_reason(Reason::GridComposer));
    assert_eq!(
        started.confidence,
        Confidence::Heuristic,
        "the composer frame is evidence, not proof"
    );
    assert!(started.sources.has(Source::Grid));
    assert!(
        kinds(&events).contains(&"turn-began"),
        "and the turn is still classified, with no hook anywhere: {:?}",
        kinds(&events)
    );
}

#[test]
fn a_program_the_first_sample_could_not_name_is_named_late_and_only_once() {
    let mut o = Observer::new();
    let first = o.on_sample(&sample(st("running", 1), None), 1);
    assert_eq!(
        find(&first, "started").expect("started").kind,
        EventKind::Started { program: None },
        "nothing named it, so nothing is guessed"
    );
    assert!(
        find(&first, "started")
            .unwrap()
            .has_reason(Reason::DetailAbsent)
    );
    let grid = parse_text_json(REAL_GRID).expect("measured grid");
    let second = o.on_sample(&sample(st("running", 2), Some(grid.clone())), 2);
    let late = find(&second, "started").expect("named late");
    assert!(late.has_reason(Reason::ProgramLate));
    assert_eq!(o.program(), Some("claude"));
    let third = o.on_sample(&sample(st("running", 3), Some(grid)), 3);
    assert!(find(&third, "started").is_none(), "never a third time");
}

#[test]
fn a_screen_with_no_composer_and_no_detail_names_nothing() {
    let mut o = Observer::new();
    let grid = screen(rows_of(&["$ ls", "Cargo.toml  src"]), 4);
    let events = o.on_sample(&sample(st("running", 1), Some(grid)), 1);
    let started = find(&events, "started").expect("started");
    assert_eq!(started.kind, EventKind::Started { program: None });
    assert!(started.has_reason(Reason::NoProgramEvidence));
    assert_eq!(started.confidence, Confidence::Unknown);
}

// ---------------------------------------------------------------------------
// Turns
// ---------------------------------------------------------------------------

#[test]
fn a_turn_begins_and_ends_from_the_grid() {
    let mut o = Observer::new();
    let busy = parse_text_json(REAL_GRID).expect("busy grid");
    let began = o.on_sample(&sample(st("running", 1), Some(busy)), 1);
    let ev = find(&began, "turn-began").expect("turn-began");
    assert!(ev.has_reason(Reason::GridBusy));
    assert!(ev.sources.has(Source::Grid));
    assert!(o.turn_in_flight());
    let idle = screen(idle_screen("? for shortcuts"), 300_000);
    let ended = o.on_sample(&sample(st("idle", 2), Some(idle)), 2);
    let ev = find(&ended, "turn-ended").expect("turn-ended");
    assert!(ev.has_reason(Reason::GridIdle));
    assert!(!o.turn_in_flight());
}

#[test]
fn a_grid_opened_turn_is_never_closed_by_status_alone() {
    // The anti-flap rule. Without it a pass that read no grid (because
    // `revision` did not move) would close a live turn and re-open it.
    let mut o = Observer::new();
    let busy = parse_text_json(REAL_GRID).expect("busy grid");
    o.on_sample(&sample(st("running", 1), Some(busy)), 1);
    assert!(o.turn_in_flight());
    let quiet = o.on_sample(&sample(st("quiet", 2), None), 2);
    assert!(
        find(&quiet, "turn-ended").is_none(),
        "a quiet status with no grid read is not a turn boundary: {:?}",
        kinds(&quiet)
    );
    assert!(o.turn_in_flight());
}

#[test]
fn a_status_opened_turn_is_closed_by_the_grid() {
    let mut o = Observer::new();
    let began = o.on_sample(&sample(st("running", 1), None), 1);
    let ev = find(&began, "turn-began").expect("turn-began");
    assert!(ev.has_reason(Reason::StatusRunning));
    assert_eq!(
        ev.confidence,
        Confidence::Heuristic,
        "status alone cannot see a composer, so it never claims better"
    );
    let idle = screen(idle_screen("? for shortcuts"), 7);
    let ended = o.on_sample(&sample(st("running", 2), Some(idle)), 2);
    let ev = find(&ended, "turn-ended").expect("the grid closes it");
    assert!(ev.has_reason(Reason::GridIdle));
    assert!(!o.turn_in_flight());
}

#[test]
fn a_status_only_turn_opens_and_closes_from_status() {
    let mut o = Observer::new();
    o.on_sample(&sample(st("running", 1), None), 1);
    assert!(o.turn_in_flight());
    let ended = o.on_sample(&sample(st("idle", 2), None), 2);
    let ev = find(&ended, "turn-ended").expect("turn-ended");
    assert!(ev.has_reason(Reason::StatusIdle));
    assert!(!ev.has_reason(Reason::GridStale), "no grid was ever read");
}

#[test]
fn a_status_transition_taken_after_a_grid_read_says_the_grid_is_stale() {
    struct Fixture(String);
    impl Introspect for Fixture {
        fn status(&mut self) -> CtlReply {
            ok(&self.0)
        }
        fn text_json_tail(&mut self, _rows: usize) -> CtlReply {
            let idle = idle_screen("? for shortcuts").join("\",\"");
            ok(&format!(r#"{{"rows":["{idle}"],"seq":5,"first":0}}"#))
        }
    }
    let mut fx = Fixture(status_line("idle", 1, "-", 10));
    let mut o = Observer::new();
    o.read(&mut fx, 1).expect("first pass reads the grid");
    assert!(!o.turn_in_flight());
    // The revision does not move, so no grid is read — and `status` opens the
    // turn, saying out loud that its grid reading is older.
    fx.0 = status_line("running", 1, "-", 10);
    let events = o.read(&mut fx, 2).expect("second pass").events;
    let ev = find(&events, "turn-began").expect("turn-began");
    assert!(ev.has_reason(Reason::GridStale));
}

#[test]
fn an_approval_box_keeps_the_turn_open_because_a_blocked_turn_has_not_ended() {
    let mut o = Observer::new();
    let busy = parse_text_json(REAL_GRID).expect("busy grid");
    o.on_sample(&sample(st("running", 1), Some(busy)), 1);
    let prompt = screen(bash_one_row(), 2);
    let events = o.on_sample(&sample(st("running", 2), Some(prompt)), 2);
    assert!(
        find(&events, "turn-ended").is_none(),
        "waiting on a human is not a finished turn: {:?}",
        kinds(&events)
    );
    assert!(o.turn_in_flight());
    let banner = find(&events, "banner").expect("the approval box is a banner");
    match &banner.kind {
        EventKind::Banner { kind, text, .. } => {
            assert_eq!(*kind, BannerKind::Permission);
            assert!(text.starts_with("bash: "), "{text}");
        }
        other => panic!("{other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Output moved, quiet, exited
// ---------------------------------------------------------------------------

#[test]
fn the_grids_content_counter_is_exact_evidence_that_output_moved() {
    let mut o = Observer::new();
    let first = parse_text_json(REAL_GRID).expect("grid");
    o.on_sample(&sample(st("running", 1), Some(first.clone())), 1);
    let mut later = first;
    later.seq += 1;
    let events = o.on_sample(&sample(st("running", 2), Some(later)), 2);
    let moved = find(&events, "output-moved").expect("output-moved");
    assert_eq!(moved.confidence, Confidence::Exact);
    assert!(moved.has_reason(Reason::ContentSeq));
    assert!(moved.sources.has(Source::Grid));
}

#[test]
fn status_alone_reports_output_moved_from_revision_plus_content_activity() {
    let mut o = Observer::new();
    o.on_sample(&sample(st("quiet", 1), None), 1);
    let events = o.on_sample(&sample(st("running", 2), None), 2);
    let moved = find(&events, "output-moved").expect("output-moved");
    assert!(moved.has_reason(Reason::RevisionMoved) && moved.has_reason(Reason::ContentActivity));
    assert_eq!(moved.confidence, Confidence::Strong);
    assert!(moved.sources.has(Source::Status));
}

#[test]
fn a_still_per_grid_counter_does_not_veto_the_status_reading() {
    // `seq` is PER-GRID: an alt-screen round trip does not move the main
    // grid's counter. A grid that held still therefore costs the reading its
    // exactness and does not silence it.
    let mut o = Observer::new();
    let grid = parse_text_json(REAL_GRID).expect("grid");
    o.on_sample(&sample(st("running", 1), Some(grid.clone())), 1);
    let events = o.on_sample(&sample(st("running", 2), Some(grid)), 2);
    let moved = find(&events, "output-moved").expect("still reported");
    assert_eq!(moved.confidence, Confidence::Heuristic);
    assert!(moved.has_reason(Reason::RevisionMoved));
    assert!(
        !moved.has_reason(Reason::ContentSeq),
        "the counter did not move"
    );
}

#[test]
fn a_screen_with_no_live_zone_at_all_yields_no_banner() {
    // No composer frame and nothing the geometry calls "said": there is no
    // live zone to place a row in, so the scan does not run. Without this
    // floor the whole grid would be scanned and a quoted literal would fire.
    let mut o = Observer::new();
    let rows = rows_of(&["                 You've reached your weekly limit."]);
    let events = o.on_sample(&sample(st("running", 1), Some(screen(rows, 1))), 1);
    assert!(
        find(&events, "banner").is_none(),
        "an unplaceable row is not a banner: {:?}",
        kinds(&events)
    );
}

#[test]
fn the_first_sample_never_claims_output_moved() {
    let mut o = Observer::new();
    let events = o.on_sample(&sample(st("running", 1), None), 1);
    assert!(
        find(&events, "output-moved").is_none(),
        "there was no earlier state to move from: {:?}",
        kinds(&events)
    );
}

#[test]
fn a_move_carries_the_stillness_it_ended() {
    let mut o = Observer::new();
    o.on_sample(&sample(st("idle", 1), None), 1);
    let quiet = StatusSample::parse_line(&status_line("quiet", 2, "-", 42_000)).expect("quiet");
    o.on_sample(&sample(quiet, None), 2);
    let events = o.on_sample(&sample(st("running", 3), None), 3);
    assert_eq!(
        find(&events, "output-moved").expect("moved").kind,
        EventKind::OutputMoved {
            still_ms: Some(42_000)
        }
    );
}

#[test]
fn quiet_fires_on_the_transition_and_not_again() {
    let mut o = Observer::new();
    o.on_sample(&sample(st("running", 1), None), 1);
    let quiet = StatusSample::parse_line(&status_line("quiet", 2, "-", 9_000)).expect("quiet");
    let first = o.on_sample(&sample(quiet.clone(), None), 2);
    assert_eq!(
        find(&first, "quiet").expect("quiet").kind,
        EventKind::Quiet {
            since_ms: Some(9_000)
        }
    );
    let again = o.on_sample(&sample(quiet, None), 3);
    assert!(find(&again, "quiet").is_none(), "{:?}", kinds(&again));
}

#[test]
fn an_exit_closes_an_open_turn_first_and_fires_exactly_once() {
    let mut o = Observer::new();
    o.on_sample(&sample(st("running", 1), None), 1);
    assert!(o.turn_in_flight());
    let gone = StatusSample::parse_line(
        &status_line("exited", 2, "-", 5)
            .replace("outcome=none exit_code=-", "outcome=failure exit_code=2")
            // A session whose program is gone reports no content activity.
            .replace("reasons=fg_job,content_activity", "reasons=exit"),
    )
    .expect("exited");
    let events = o.on_sample(&sample(gone.clone(), None), 2);
    assert_eq!(kinds(&events), vec!["turn-ended", "exited"]);
    assert!(
        find(&events, "turn-ended")
            .unwrap()
            .has_reason(Reason::StatusExited),
        "a status-opened turn is closed by the same status that says the program is gone"
    );
    assert_eq!(
        find(&events, "exited").unwrap().kind,
        EventKind::Exited {
            outcome: Outcome::Failure,
            exit_code: Some(2)
        }
    );
    assert!(o.on_sample(&sample(gone, None), 3).is_empty());
}

#[test]
fn an_exit_closes_a_grid_opened_turn_that_status_alone_may_not_close() {
    // The one turn `status` is forbidden to close is closed here, because a
    // program that is gone cannot still be mid-turn.
    let mut o = Observer::new();
    let busy = parse_text_json(REAL_GRID).expect("busy grid");
    o.on_sample(&sample(st("running", 1), Some(busy)), 1);
    assert!(o.turn_in_flight());
    let gone = StatusSample::parse_line(
        &status_line("exited", 2, "-", 5)
            .replace("reasons=fg_job,content_activity", "reasons=exit"),
    )
    .expect("exited");
    let events = o.on_sample(&sample(gone, None), 2);
    assert_eq!(kinds(&events), vec!["turn-ended", "exited"]);
    assert!(
        find(&events, "turn-ended")
            .unwrap()
            .has_reason(Reason::SessionExited),
        "and it says why"
    );
    assert!(!o.turn_in_flight());
}

// ---------------------------------------------------------------------------
// Banners
// ---------------------------------------------------------------------------

#[test]
fn a_live_limit_banner_is_read_through_the_shipped_reader_and_carries_its_reset() {
    let mut rows = rows_of(&["⏺ Working on it."]);
    rows.extend(composer(
        "You've reached your Fable limit. Resets at 7:30pm (America/Los_Angeles)",
    ));
    let mut o = Observer::new();
    let events = o.on_sample(&sample(st("idle", 1), Some(screen(rows, 1))), 1);
    let banner = find(&events, "banner").expect("banner");
    match &banner.kind {
        EventKind::Banner { kind, text, reset } => {
            assert_eq!(*kind, BannerKind::Limit);
            assert!(text.contains("Fable limit"), "{text}");
            assert_eq!(reset.as_deref(), Some("7:30pm (America/Los_Angeles)"));
        }
        other => panic!("{other:?}"),
    }
    assert!(banner.has_reason(Reason::LiveZone));
    assert_eq!(banner.confidence, Confidence::Strong);
}

#[test]
fn an_auth_notice_in_the_live_zone_reads_auth() {
    let mut rows = rows_of(&["⏺ Working on it."]);
    rows.extend(composer(
        "API Error: 401 Invalid API key · Please run /login",
    ));
    let mut o = Observer::new();
    let events = o.on_sample(&sample(st("idle", 1), Some(screen(rows, 1))), 1);
    let banner = find(&events, "banner").expect("banner");
    match &banner.kind {
        EventKind::Banner { kind, .. } => assert_eq!(*kind, BannerKind::Auth),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_transcript_row_that_merely_quotes_a_literal_is_not_a_banner() {
    // The containment control. This design document itself quotes `Please run
    // /login` and `weekly limit`; a screen showing those words ABOVE the live
    // zone is the worker talking, not the vendor announcing.
    let mut rows =
        rows_of(&["⏺ The classifier's literals include `Please run /login` and `weekly limit`."]);
    rows.extend(composer("? for shortcuts"));
    let mut o = Observer::new();
    let events = o.on_sample(&sample(st("idle", 1), Some(screen(rows, 1))), 1);
    assert!(
        find(&events, "banner").is_none(),
        "a quoted literal is not a banner: {:?}",
        kinds(&events)
    );
}

#[test]
fn a_limit_notice_left_in_the_transcript_is_history_and_not_a_live_banner() {
    // A REAL captured screen holding two `You've reached your Fable limit`
    // notices, both above the live zone, the worker idle after a /model
    // switch. The shipped reader already refuses them; so does this one.
    let rows = saved(IDLE_AFTER_LIMIT);
    assert!(
        rows.iter().any(|r| r.contains("reached your Fable limit")),
        "the fixture must still carry the notices"
    );
    let mut o = Observer::new();
    let events = o.on_sample(&sample(st("idle", 1), Some(screen(rows, 1))), 1);
    assert!(
        find(&events, "banner").is_none(),
        "history is not a banner: {:?}",
        kinds(&events)
    );
}

#[test]
fn a_banner_is_reported_once_while_it_stays_up_and_again_after_it_goes() {
    let mut rows = rows_of(&["⏺ Working on it."]);
    rows.extend(composer(
        "You've reached your Fable limit. Resets at 7:30pm (America/Los_Angeles)",
    ));
    let mut o = Observer::new();
    let first = o.on_sample(&sample(st("idle", 1), Some(screen(rows.clone(), 1))), 1);
    assert!(find(&first, "banner").is_some());
    let second = o.on_sample(&sample(st("idle", 2), Some(screen(rows.clone(), 2))), 2);
    assert!(find(&second, "banner").is_none(), "still up, not new");
    let cleared = screen(idle_screen("? for shortcuts"), 3);
    o.on_sample(&sample(st("idle", 3), Some(cleared)), 3);
    let back = o.on_sample(&sample(st("idle", 4), Some(screen(rows, 4))), 4);
    assert!(
        find(&back, "banner").is_some(),
        "it came back, so it is news"
    );
}

#[test]
fn banner_text_is_bounded_quoted_data() {
    let long = format!("You've reached your Fable limit. {}", "x".repeat(2_000));
    let mut rows = rows_of(&["⏺ Working on it."]);
    rows.extend(composer(&long));
    let mut o = Observer::new();
    let events = o.on_sample(&sample(st("idle", 1), Some(screen(rows, 1))), 1);
    match &find(&events, "banner").expect("banner").kind {
        EventKind::Banner { text, .. } => assert!(text.len() <= BANNER_CAP, "{}", text.len()),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_banner_on_a_row_that_scrolled_off_is_heuristic_and_names_its_source() {
    let mut s = sample(
        st("idle", 1),
        Some(screen(idle_screen("? for shortcuts"), 1)),
    );
    s.offscreen = Some(Offscreen {
        archived: vec!["You've reached your weekly limit.".to_string()],
        ..Offscreen::default()
    });
    let mut o = Observer::new();
    let events = o.on_sample(&s, 1);
    let banner = find(&events, "banner").expect("banner");
    assert!(banner.has_reason(Reason::OffscreenRow));
    assert!(banner.sources.has(Source::Offscreen));
    assert_eq!(
        banner.confidence,
        Confidence::Heuristic,
        "an archived row has no live zone to be checked against"
    );
}

#[test]
fn a_search_hit_corroborates_a_banner_and_never_creates_one() {
    let mut rows = rows_of(&["⏺ Working on it."]);
    rows.extend(composer("You've reached your Fable limit."));
    let mut with = sample(st("idle", 1), Some(screen(rows, 1)));
    with.search = Some(SearchHits {
        pattern: "limit".to_string(),
        hits: 3,
    });
    let mut o = Observer::new();
    let banner = find(&o.on_sample(&with, 1), "banner")
        .cloned()
        .expect("banner");
    assert!(banner.has_reason(Reason::HistoryHit));
    assert!(banner.sources.has(Source::Search));

    let mut alone = sample(
        st("idle", 1),
        Some(screen(idle_screen("? for shortcuts"), 1)),
    );
    alone.search = Some(SearchHits {
        pattern: "limit".to_string(),
        hits: 9,
    });
    let mut fresh = Observer::new();
    assert!(
        find(&fresh.on_sample(&alone, 1), "banner").is_none(),
        "history alone is not a live banner"
    );
}

#[test]
fn a_gap_in_the_evidence_window_caps_confidence_and_says_so() {
    let mut s = sample(st("running", 1), Some(parse_text_json(REAL_GRID).unwrap()));
    s.offscreen = Some(Offscreen {
        lost: 12,
        breaks: 1,
        ..Offscreen::default()
    });
    let mut o = Observer::new();
    let events = o.on_sample(&s, 1);
    assert!(!events.is_empty());
    for e in &events {
        assert!(e.has_reason(Reason::DegradedGap), "{:?}", e.kind.name());
        assert!(e.confidence <= Confidence::Strong);
    }
}

// ---------------------------------------------------------------------------
// The vocabulary
// ---------------------------------------------------------------------------

#[test]
fn provenance_is_a_set_and_names_its_members_in_order() {
    let p = SourceSet::just(Source::Grid).with(Source::Search);
    assert!(p.has(Source::Grid) && p.has(Source::Search));
    assert!(!p.has(Source::Status) && !p.has(Source::Offscreen));
    assert_eq!(p.names(), vec!["grid", "search"]);
    assert_eq!(SourceSet::EMPTY.names(), Vec::<&str>::new());
    assert!(
        !SourceSet::EMPTY.has_all(SourceSet::EMPTY),
        "the empty set has nothing"
    );
}

#[test]
fn every_token_the_ledger_prints_is_a_word_this_module_chose() {
    for (phase, name) in [
        (SessionPhase::Unknown, "unknown"),
        (SessionPhase::Starting, "starting"),
        (SessionPhase::Idle, "idle"),
        (SessionPhase::Running, "running"),
        (SessionPhase::Quiet, "quiet"),
        (SessionPhase::Exited, "exited"),
    ] {
        assert_eq!(phase.as_str(), name);
        assert_eq!(SessionPhase::parse(name), phase);
    }
    for kind in [
        BannerKind::Limit,
        BannerKind::Auth,
        BannerKind::Permission,
        BannerKind::Error,
    ] {
        assert!(!kind.as_str().is_empty());
    }
    for reason in [
        Reason::FirstSample,
        Reason::GridComposer,
        Reason::DegradedGap,
        Reason::SessionExited,
    ] {
        assert!(
            reason
                .as_str()
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '_' || c == ':')
        );
    }
    assert_eq!(Reason::DegradedGap.as_str(), "degraded:gap");
}

#[test]
fn an_observe_error_says_what_it_refused() {
    assert_eq!(
        ObserveError::Schema(9).to_string(),
        "status schema 9 is not 1"
    );
    assert!(
        ObserveError::Refused("ERR denied".into())
            .to_string()
            .contains("denied")
    );
    assert!(
        ObserveError::Malformed("no rows".into())
            .to_string()
            .contains("no rows")
    );
}

/// TOOL OUTPUT IS NOT A BANNER, and the words in the table are PHRASES.
///
/// The free row scan over the live zone matched bare substrings — `weekly`,
/// `billing`, `on hold`, `verification`, `authentication`, `high load`,
/// `overloaded`, `usage credits` — against every non-empty row below the
/// last thing said. Tool output lands there, so `cat`ting a changelog line
/// reading "weekly rollup" minted `Evidence::Banner -> Class::Weekly7dLimit`.
/// A class SHORT-CIRCUITS `classify_liveness`, so that stray word turned the
/// stall ladder off for the class's lifetime — a capability that silently
/// stops working, driven by in-session, accident-writable text.
///
/// The containment test that existed proved only that a quote ABOVE the live
/// zone is not a banner. This is the half below it.
#[test]
fn a_tool_result_block_is_never_read_as_a_banner() {
    let mut rows = rows_of(&["⏺ Bash(cat CHANGELOG.md)"]);
    rows.push("  ⎿  ## 2026-09-01 — the weekly rollup, billing notes and".to_string());
    rows.push("       authentication rewrite; the account is on hold no longer".to_string());
    rows.push("       … +12 lines".to_string());
    rows.extend(composer("? for shortcuts"));
    let mut o = Observer::new();
    let events = o.on_sample(&sample(st("idle", 1), Some(screen(rows, 1))), 1);
    assert!(
        find(&events, "banner").is_none(),
        "a command's own output is not something the vendor said: {:?}",
        kinds(&events)
    );

    // POSITIVE CONTROL: the same words in a row the VENDOR drew, below the
    // live zone and outside any tool-result block, are still a banner. The
    // fence narrows the scan; it does not switch it off.
    let mut rows = rows_of(&["⏺ Done."]);
    rows.extend(composer("You've reached your weekly usage limit"));
    let mut o = Observer::new();
    let events = o.on_sample(&sample(st("idle", 2), Some(screen(rows, 2))), 1);
    assert!(find(&events, "banner").is_some(), "{:?}", kinds(&events));
}

/// NEEDLES ARE PHRASES. A bare `weekly`, `billing`, `on hold`,
/// `verification` or `authentication` is an ordinary English word, and one of
/// them on the screen used to mint a limit class.
#[test]
fn one_ordinary_word_mints_no_limit_class() {
    use super::super::limits::{Class, banner_class};
    for benign in [
        "our weekly planning meeting is on Tuesday",
        "see billing for the invoice layout",
        "the PR is on hold pending review",
        "add verification to the signature path",
        "authentication is spelled with one n",
        "Weekly rollup: 14 commits",
    ] {
        assert_eq!(banner_class(benign), None, "{benign}");
    }
    // POSITIVE CONTROLS: every phrase design §5.8.2 records still places.
    for (text, class) in [
        (
            "You've reached your weekly usage limit",
            Class::Weekly7dLimit,
        ),
        ("Your account is on hold", Class::SpendBilling),
        ("Authentication failed. Please run /login", Class::Auth),
        ("Verification required to continue", Class::SpendBilling),
    ] {
        assert_eq!(banner_class(text), Some(class), "{text}");
    }
}

/// THE REVISION GATE BREAKS TO THE SAFE ANSWER HERE, unlike in `presence.rs`.
///
/// `needs_grid` answered FALSE for a `status` whose `revision=` was missing or
/// unparseable once one grid read had happened, so the grid would never be
/// read again and `watch::Watcher::prompt` — the approval-box fence on every
/// L3 act — would freeze at its last value. `presence.rs` gates a cosmetic
/// roster row with the same shape; here it gates an actuation fence.
#[test]
fn a_status_with_no_revision_re_reads_the_grid_rather_than_wedging_it_closed() {
    struct Fixture;
    impl Introspect for Fixture {
        fn status(&mut self) -> CtlReply {
            ok(REAL_STATUS)
        }
        fn text_json_tail(&mut self, _rows: usize) -> CtlReply {
            ok(REAL_GRID)
        }
    }
    let mut o = Observer::new();
    // Never read: due, on both readings.
    assert!(o.needs_grid(Some(172)));
    assert!(o.needs_grid(None));
    o.read(&mut Fixture, 1).expect("the pass reads");
    // POSITIVE CONTROL: an unmoved revision still costs nothing, which is the
    // whole point of the gate.
    assert!(!o.needs_grid(Some(172)), "an idle session re-reads nothing");
    assert!(o.needs_grid(Some(173)), "a moved revision is due");
    // THE DIVERGENCE: "I could not read the revision" is not "it did not
    // move".
    assert!(
        o.needs_grid(None),
        "a status that cannot say must not wedge the fence shut"
    );
}
