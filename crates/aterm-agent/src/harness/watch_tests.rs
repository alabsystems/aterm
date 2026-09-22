// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the watch loop (design §5.3, §5.4, §5.6, §5.8, §5.8.10).
//!
//! Every clock value here is a literal and every wake is injected: nothing in
//! this file reads the wall clock, the environment, a socket or a file, and
//! the fake wire refuses to advance time unless an `await` was armed — so a
//! loop that tried to sample on a cadence would hang these tests rather than
//! pass them slowly.
//!
//! The negative controls are the point of several of these: a stall that has
//! an EXPLANATION is not a stall; a nudge above `liveness.level` is not taken;
//! a screen that did not move does not re-verdict; and no act in the whole
//! vocabulary types `y` or presses Enter bare.

use super::*;
use crate::supervise::run::CtlReply;
use crate::supervise::screen::Screen;

const NOW_S: i64 = 1_789_660_000;
const NOW_MS: u64 = 1_000_000;
const GEN: u64 = 25;

// -- fixtures ------------------------------------------------------------------

fn zone_none(_: &str) -> Option<i64> {
    None
}

/// A REAL launch nonce: 32 lowercase hex characters, the `nonce=<hex32>` a
/// `local`/`sessions` roster row publishes and the only `<epoch>` the
/// server's `pty_idem::parse_key` will parse. The tests used to say `e1`,
/// which checked the harness's own invented spelling against itself and
/// passed while every minted `turn id=` was dead on the wire.
const NONCE: &str = "8186e0fa4920bfd5377f1edf9e763708";

fn cfg() -> WatchConfig {
    WatchConfig {
        nonce: NONCE.to_string(),
        producer: 7,
        zone_offset: zone_none,
        ..WatchConfig::default()
    }
}

fn watcher() -> Watcher {
    let mut w = Watcher::new("s-1", cfg(), LimitsConfig::default());
    w.wake(&Wake::Generation(GEN), NOW_MS, NOW_S);
    w
}

fn host() -> HostGuards {
    HostGuards {
        hold: false,
        busy: false,
        custody_user: false,
        generation: GEN,
        engaged: true,
    }
}

fn sf(error: &str) -> Evidence {
    Evidence::StopFailure {
        error: error.into(),
        details: None,
    }
}

fn pms() -> Evidence {
    Evidence::PostModelSwitch {
        from: "claude-fable-5-1".into(),
        to: "claude-sonnet-4-5".into(),
        source: None,
    }
}

fn window(pct: f64, resets_at: i64) -> Evidence {
    window_of(limits::WindowKind::FiveHour, pct, resets_at)
}

fn window_of(which: limits::WindowKind, pct: f64, resets_at: i64) -> Evidence {
    Evidence::Window {
        which,
        used_pct: Some(pct),
        resets_at: Some(resets_at),
        source: crate::harness::source::Source::StatusLine,
        age_s: Some(8),
    }
}

fn status_line(phase: &str, revision: u64, since_ms: u64, hold: bool) -> String {
    reasoned_status_line(phase, revision, since_ms, hold, "fg_job,content_activity")
}

/// The shape of `status` on a session whose CLASSIFICATION moved but whose
/// output did not: `reasons=` carries `fg_job` and NOT `content_activity`.
/// That distinction is the spine's, not this test's — `Observer` reads
/// `content_activity` as "output moved", so a stalled session must not carry
/// it or every re-read would refute its own stall.
fn still_line(phase: &str, revision: u64, since_ms: u64) -> String {
    reasoned_status_line(phase, revision, since_ms, false, "fg_job")
}

fn reasoned_status_line(
    phase: &str,
    revision: u64,
    since_ms: u64,
    hold: bool,
    reasons: &str,
) -> String {
    // `seq=` is on the SHIPPED status line (`session_status.rs`) and is the
    // terminal's `content_seq`. It is the only source the loop has for the
    // `await seq <n>` it parks on — the subscribed stream is `events`, which
    // carries no `DELTA` — so a fixture without it would let the arm read
    // `seq 0` here and pass while production spun. Derived from `revision`
    // so a screen that moved carries a sequence that moved.
    format!(
        "OK schema=1 sid=s-1 observed=true phase={phase} since_ms={since_ms} outcome=none \
         exit_code=- signal=- detail=- confidence=strong reasons={reasons} \
         attribution=adopted conflict=false revision={revision} seq={} enabled=true hold={}",
        revision.saturating_mul(10).saturating_add(3),
        u8::from(hold)
    )
}

fn status(phase: &str, revision: u64, since_ms: u64) -> observe::StatusSample {
    observe::StatusSample::parse_line(&status_line(phase, revision, since_ms, false))
        .expect("fixture status")
}

fn still(phase: &str, revision: u64, since_ms: u64) -> observe::StatusSample {
    observe::StatusSample::parse_line(&still_line(phase, revision, since_ms))
        .expect("fixture status")
}

fn sample(status: observe::StatusSample) -> observe::Sample {
    observe::Sample {
        status,
        grid: None,
        offscreen: None,
        search: None,
    }
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

fn act_of(plan: &Plan) -> Option<&Planned> {
    match plan {
        Plan::Act(p) => Some(p),
        _ => None,
    }
}

fn lines(p: &Planned) -> Vec<String> {
    p.acts.iter().filter_map(Act::line).collect()
}

// -- the fake wire --------------------------------------------------------------

/// A [`Wire`] whose clock only moves when an `await` was armed and answered.
///
/// This is the test's own statement of the no-timer rule: a loop that wanted
/// to sample every N milliseconds would have to ask for an arm it does not
/// have, and [`FakeWire::park`] panics when the script says a deadline fired
/// with nothing armed.
struct FakeWire {
    now_ms: u64,
    now_s: i64,
    /// The scripted wakes, oldest first. Each one may carry a clock jump.
    script: Vec<(u64, Wake)>,
    /// Every arm the loop asked for, in order.
    armed: Vec<Arm>,
    /// Every line sent, in order.
    sent: Vec<String>,
    /// Every ledger row written, in order: `(ring, row)`.
    rows: Vec<(String, String)>,
    /// What `send` answers; an `Err` for a line whose prefix is in here.
    refuse: Vec<(String, String)>,
    /// What `send` answers for a line whose prefix is in here: an `Ok` body
    /// other than the bare `OK`. The two that matter are the server's own
    /// success replies that mean NOTHING HAPPENED — `OK skipped` (the guard
    /// declined) and `OK 0 dup=1` (the key was a replay) — and neither
    /// appeared anywhere in this module's tests until 2026-09-22.
    reply: Vec<(String, String)>,
    guards: HostGuards,
    status: String,
    grid: Option<String>,
    next_id: u64,
}

impl FakeWire {
    fn new(script: Vec<(u64, Wake)>) -> FakeWire {
        FakeWire {
            now_ms: NOW_MS,
            now_s: NOW_S,
            script,
            armed: Vec::new(),
            sent: Vec::new(),
            rows: Vec::new(),
            refuse: Vec::new(),
            reply: Vec::new(),
            guards: host(),
            status: status_line("running", 1, 100, false),
            grid: None,
            next_id: 1,
        }
    }
}

impl observe::Introspect for FakeWire {
    fn status(&mut self) -> CtlReply {
        CtlReply {
            code: 0,
            stdout: self.status.clone(),
            stderr: String::new(),
        }
    }

    fn text_json_tail(&mut self, _rows: usize) -> CtlReply {
        match &self.grid {
            Some(json) => CtlReply {
                code: 0,
                stdout: json.clone(),
                stderr: String::new(),
            },
            None => CtlReply {
                code: 1,
                stdout: String::new(),
                stderr: "ERR unsupported".to_string(),
            },
        }
    }
}

impl Wire for FakeWire {
    fn park(&mut self, arm: Option<&Arm>) -> Wake {
        if let Some(a) = arm {
            self.armed.push(a.clone());
        }
        let Some((jump, wake)) = self.script.first().cloned() else {
            return Wake::Closed;
        };
        self.script.remove(0);
        if jump > 0 {
            assert!(
                arm.is_some(),
                "the clock moved with nothing armed — a loop that waits must ask for the wait"
            );
            self.now_ms = self.now_ms.saturating_add(jump);
            self.now_s = self.now_s.saturating_add((jump / 1000) as i64);
        }
        wake
    }

    fn send(&mut self, line: &str) -> Result<String, String> {
        for (prefix, err) in &self.refuse {
            if line.starts_with(prefix.as_str()) {
                return Err(err.clone());
            }
        }
        self.sent.push(line.to_string());
        for (prefix, body) in &self.reply {
            if line.starts_with(prefix.as_str()) {
                return Ok(body.clone());
            }
        }
        Ok("OK".to_string())
    }

    fn journal(&mut self, ring: &str, row: &str) -> u64 {
        self.rows.push((ring.to_string(), row.to_string()));
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn guards(&mut self) -> HostGuards {
        self.guards
    }

    fn now_ms(&mut self) -> u64 {
        self.now_ms
    }

    fn now_unix(&mut self) -> i64 {
        self.now_s
    }
}

// -- the wire format ------------------------------------------------------------

#[test]
fn the_three_frame_shapes_parse_and_nothing_else_does() {
    assert_eq!(
        parse_frame("DELTA s-1 seq=4210 screen 12"),
        Some(Frame::Delta { seq: Some(4210) })
    );
    assert_eq!(
        parse_frame("EVENT s-1 turn 7 submitted=1 status=settled"),
        Some(Frame::Event {
            kind: "turn".to_string()
        })
    );
    assert_eq!(
        parse_frame("GAP s-1 bytes-dropped=8192"),
        Some(Frame::Gap {
            bytes_dropped: 8192,
            resync: None,
            events_resync: None,
        })
    );
    assert_eq!(
        parse_frame("GAP s-1 resync=99"),
        Some(Frame::Gap {
            bytes_dropped: 0,
            resync: Some(99),
            events_resync: None,
        })
    );
    assert_eq!(
        parse_frame("GAP * events-resync=12"),
        Some(Frame::Gap {
            bytes_dropped: 0,
            resync: None,
            events_resync: Some(12),
        })
    );
    // NEGATIVE CONTROLS: a body row, a byte burst, a reply and a blank line
    // are not frames, and none of them is an error either.
    for junk in [
        "",
        "BYTES s-1 40",
        "OK 3 first=2379 last=2381",
        "DELTA",
        "  ❯ keep going",
    ] {
        assert_eq!(parse_frame(junk), None, "{junk:?}");
    }
}

// -- the act grammar ------------------------------------------------------------

#[test]
fn a_typed_act_is_the_fence_and_never_a_bare_enter() {
    let acts = fenced(
        TurnId {
            nonce: NONCE.to_string(),
            producer: 7,
            seq: 3,
        },
        "/model opus",
    );
    assert_eq!(acts.len(), 2, "a turn and its guarded press, always");
    let turn = acts[0].line().expect("the turn has a line");
    // `id=` must LEAD: a trailing `id=` is typed as TEXT.
    assert!(turn.starts_with(&format!("turn id={NONCE}:7:3 ")), "{turn}");
    // …and the `<epoch>` half is checked against the SERVER's reader, not
    // against a literal this file also wrote.
    let epoch = turn
        .trim_start_matches("turn id=")
        .split(':')
        .next()
        .expect("an epoch");
    assert!(
        aterm_session::LaunchNonce::from_hex(epoch).is_some(),
        "the server parses `<epoch>` with LaunchNonce::from_hex; {epoch:?} does not: {turn}"
    );
    // `submit_window=` is NOT optional on a `settle=gone:` turn.
    assert!(
        turn.contains(&format!("settle=gone:{BUSY_FOOTER}")),
        "{turn}"
    );
    assert!(
        turn.contains(&format!("submit_window={SUBMIT_WINDOW_MS}")),
        "{turn}"
    );
    assert!(turn.contains(&format!("yield={YIELD_FLOOR}")), "{turn}");
    assert!(turn.contains("submit=none"), "{turn}");
    assert!(turn.ends_with("/model opus"), "{turn}");
    let press = acts[1].line().expect("the press has a line");
    assert!(press.starts_with("key if="), "{press}");
    assert!(press.ends_with(" enter"), "{press}");
    assert_ne!(press, "key enter", "never a bare Enter");
    // The guard is ONE wire token: a space in it would make the regex a
    // second operand and the key a word.
    let guard = press
        .trim_start_matches("key if=")
        .trim_end_matches(" enter");
    assert!(!guard.is_empty() && !guard.contains(' '), "{guard:?}");
}

#[test]
fn a_quoted_row_can_never_inject_a_second_line_into_a_row_framed_protocol() {
    let act = Act::Description("a\nb\r\tc".to_string());
    let line = act.line().expect("a line");
    // Runs of folded controls collapse to ONE space: the shared reader does
    // that, and a local sweep that kept `a b  c` was one of four copies.
    assert_eq!(line, "meta set description a b c");
    assert_eq!(line.lines().count(), 1);
    // The two separators `char::is_control()` does NOT cover.
    let sep = Act::Description("a\u{2028}b\u{2029}c".to_string())
        .line()
        .expect("a line");
    assert_eq!(sep, "meta set description a b c");
}

/// `Act::Attention` used to carry no text and render a bare `meta set
/// attention`, which the server answers `ERR usage` (`MetaWriteError::Empty`):
/// every warn and escalate badge would have failed. It now always carries the
/// harness's stem, folds to one line and fits the server's 256-byte cap WITH
/// the stem, whatever it quotes.
#[test]
fn an_attention_act_is_never_the_bare_usage_error_and_always_carries_the_stem() {
    let long = "x".repeat(1000);
    let wide = "é".repeat(300);
    for why in [
        "",
        "   ",
        "hung-tool on s-1 · no output for 600s",
        "a\nsecond line\u{2028}third",
        long.as_str(),
        wide.as_str(),
    ] {
        let line = Act::Attention(why.to_string()).line().expect("a line");
        assert_ne!(line.trim(), "meta set attention", "{why:?}");
        assert_eq!(line.lines().count(), 1, "{line:?}");
        let text = line.strip_prefix("meta set attention ").expect("the verb");
        assert!(text.starts_with(mark::STEM), "{text:?}");
        assert!(text.len() <= ATTENTION_CAP, "{} bytes", text.len());
        assert!(!text.trim().is_empty());
    }
    assert_eq!(
        Act::Attention("stuck".to_string()).line().as_deref(),
        Some(format!("meta set attention {}: stuck", mark::STEM).as_str())
    );
    assert_eq!(
        Act::ClearAttention.line().as_deref(),
        Some("meta unset attention")
    );
    assert_eq!(Act::ClearAttention.level(), 1);
}

/// The attention is SHARED (§4.3 L1): the harness sets over nothing or over
/// its own badge, clears only its own, and never touches another writer's —
/// the link hook's `claude needs approval: …` while a box is open, or
/// supervise's `limited: …`. Each gate sends ONE line, the bare `meta` read.
#[test]
fn the_harness_sets_and_clears_only_its_own_attention() {
    let meta = |attention: &str| {
        format!(
            "OK title=- user_title=- description=- icon=- role=- attention={attention} cwd=- \
             state=alive"
        )
    };
    let set = Act::Attention("hung-tool".to_string());
    let clear = Act::ClearAttention;
    type Gate = Result<(), &'static str>;
    let cases: [(&str, Gate, Gate); 5] = [
        // Nothing standing: a set goes, a clear has nothing to clear.
        ("-", Ok(()), Err("attention-none")),
        // The harness's own badge: both go.
        ("aterm%20harness:%20hung-tool%20on%20s-1", Ok(()), Ok(())),
        // The link hook's badge, for a box still open: neither goes.
        (
            "claude%20needs%20approval:%20Bash%20rm%20-rf%20build",
            Err("attention-kept"),
            Err("attention-kept"),
        ),
        // supervise's: neither goes.
        (
            "limited:%20weekly%20reset=Fri",
            Err("attention-kept"),
            Err("attention-kept"),
        ),
        // A person's own words: neither goes.
        (
            "look%20at%20me",
            Err("attention-kept"),
            Err("attention-kept"),
        ),
    ];
    for (standing, want_set, want_clear) in cases {
        for (act, want) in [(&set, want_set), (&clear, want_clear)] {
            let mut wire = FakeWire::new(Vec::new());
            wire.reply = vec![("meta".to_string(), meta(standing))];
            assert_eq!(attention_gate(&mut wire, act), want, "{standing} {act:?}");
            assert_eq!(wire.sent, ["meta"], "one read, nothing written");
        }
    }
    // A refused read proves nothing is ours: neither goes.
    for act in [&set, &clear] {
        let mut wire = FakeWire::new(Vec::new());
        wire.refuse = vec![("meta".to_string(), "ERR halted".to_string())];
        assert_eq!(attention_gate(&mut wire, act), Err("attention-unread"));
    }
    assert!(attention_is_ours(Some(&attention_text("x"))));
    assert!(!attention_is_ours(None));
}

#[test]
fn the_whole_actuator_vocabulary_never_answers_an_approval_and_never_sends_l5() {
    let w = watcher();
    let st = limits::State::new(
        &limits::Classification {
            class: Class::Session5hLimit,
            unpaired: false,
            reasons: Vec::new(),
            resets_at: Some(NOW_S + 1800),
            storm: false,
            no_response: false,
        },
        NOW_S,
        GEN,
        limits::Carry::default(),
    );
    for action in Action::ALL {
        let acts = w.acts_for(action, action.level(), &st, NOW_S);
        let mut typed = Vec::new();
        for (i, act) in acts.iter().enumerate() {
            if let Act::Type { text, .. } = act {
                typed.push(text.clone());
                // Every typed act is FENCED: the very next act is the guarded
                // press, never an unfenced submit.
                assert!(
                    matches!(acts.get(i + 1), Some(Act::Submit { guard }) if !guard.is_empty()),
                    "{action}: a turn with no guarded press after it"
                );
            }
            // L5 is never in any harness's vocabulary in ABI 1.
            assert!(act.level() <= 4, "{action}: level {}", act.level());
            let line = act.line().unwrap_or_default();
            // A bare `meta set attention` is `ERR usage` on the server.
            assert_ne!(line.trim(), "meta set attention", "{action}");
            if let Act::Attention(_) = act {
                assert!(
                    line.starts_with(&format!("meta set attention {}: ", mark::STEM)),
                    "{action}: {line}"
                );
            }
            assert!(!line.starts_with("signal "), "{action}: {line}");
            assert!(!line.starts_with("close"), "{action}: {line}");
        }
        for t in typed {
            // Answering a permission prompt is the VENDOR's channel. The
            // harness never types the answer, in any shape.
            assert!(
                !matches!(t.trim(), "y" | "Y" | "yes" | "1" | "2" | "3"),
                "{action} typed {t:?} — that is an approval answer"
            );
        }
    }
}

/// REWRITTEN 2026-09-22 with the narrowed two-source rule (§5.8.2). It was
/// `an_auth_hook_alone_is_display_only_and_an_auth_banner_corroborates_it`
/// and it asserted that ONE source could only `escalate`; the door needed a
/// pair. `relogin` is not one of the four spending actions, so the door now
/// opens on rank-1 evidence — including on the banner alone, which is all a
/// `--bare` session has.
#[test]
fn a_single_auth_source_opens_the_vendors_door_and_a_pair_does_too() {
    // The CLASSIFIER's own output, end to end — `classify` -> `step` ->
    // `acts_for`, never a hand-built `State`.
    let mut alone = watcher();
    alone.wake(
        &Wake::Hook(HookEvent::Evidence(sf("authentication_failed"))),
        NOW_MS,
        NOW_S,
    );
    assert_eq!(alone.class(), Some(Class::Auth));
    assert!(
        alone.limits_state().expect("state").unpaired,
        "a hook alone is one source"
    );
    let p = alone.plan(&host(), NOW_MS, NOW_S);
    assert_eq!(
        act_of(&p).expect("something is due").limit_action,
        Some(Action::Relogin)
    );

    // The zero-hook half: nothing but the banner the vendor drew.
    let mut grid_only = watcher();
    grid_only.wake(
        &Wake::Hook(HookEvent::Evidence(Evidence::Banner {
            text: "API Error: 401 Invalid API key - Please run /login".to_string(),
            resets_at: None,
        })),
        NOW_MS,
        NOW_S,
    );
    assert_eq!(grid_only.class(), Some(Class::Auth));
    assert!(grid_only.limits_state().expect("state").unpaired);
    let p = grid_only.plan(&host(), NOW_MS, NOW_S);
    assert_eq!(
        act_of(&p).expect("the auth row acts").limit_action,
        Some(Action::Relogin),
        "the grid is rank 1: a session with no hooks still reaches the door"
    );

    // The same hook, corroborated by the vendor's own auth banner on the
    // grid: a pair, and the door opens.
    let mut paired = watcher();
    paired.wake(
        &Wake::Hook(HookEvent::Evidence(sf("authentication_failed"))),
        NOW_MS,
        NOW_S,
    );
    paired.wake(
        &Wake::Hook(HookEvent::Evidence(Evidence::Banner {
            text: "API Error: 401 Invalid API key - Please run /login".to_string(),
            resets_at: None,
        })),
        NOW_MS,
        NOW_S,
    );
    assert_eq!(paired.class(), Some(Class::Auth));
    assert!(!paired.limits_state().expect("state").unpaired);
    let p = paired.plan(&host(), NOW_MS, NOW_S);
    assert_eq!(
        act_of(&p).expect("the auth row acts").limit_action,
        Some(Action::Relogin)
    );
}

/// A watcher whose `auth` state carries a pair, which is what step 3 of
/// §5.8.10 needs. Built by hand so the tests below vary ONE thing at a time;
/// the test above proves the real classifier reaches the same state.
fn auth_watcher() -> Watcher {
    let mut w = watcher();
    w.limits = Some(limits::State::new(
        &limits::Classification {
            class: Class::Auth,
            unpaired: false,
            reasons: vec!["independent pair: StopFailure + Notification".to_string()],
            resets_at: None,
            storm: false,
            no_response: false,
        },
        NOW_S,
        GEN,
        limits::Carry::default(),
    ));
    w
}

#[test]
fn the_relogin_path_opens_the_vendors_door_and_touches_no_credential() {
    let mut w = auth_watcher();
    let plan = w.plan(&host(), NOW_MS, NOW_S);
    let planned = act_of(&plan).expect("the auth row acts");
    assert_eq!(planned.limit_action, Some(Action::Relogin));
    let sent = lines(planned);
    // Six characters, typed as a human would. The VENDOR then opens
    // Anthropic's own sign-in URL; nothing here reads, holds, forwards or
    // renews a token.
    assert!(sent.iter().any(|l| l.ends_with("/login")), "{sent:?}");
    for l in &sent {
        for forbidden in [
            "setup-token",
            "authorization code",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "ANTHROPIC_API_KEY",
            ".credentials",
            "keychain",
        ] {
            assert!(!l.contains(forbidden), "{l} carries {forbidden}");
        }
    }
    // And the WAIT that follows is bounded and typed nothing: it watches the
    // banner leave.
    w.commit(planned, 1, NOW_MS, NOW_S);
    w.verdict(limits::Verdict::Executed, NOW_MS + 1, NOW_S + 1);
    let arm = w.next_arm(NOW_MS + 2, NOW_S + 1).expect("a bounded wait");
    assert_eq!(arm.timer, Timer::ReloginWait);
    assert_eq!(arm.cond[0], "gone");
    // `relogin_wait_s` is 900 s; the server clamps an `await` at 600 000 ms,
    // so the wait arms AT the clamp and is re-armed on the wake. One wake per
    // ten minutes is a bounded re-arm, not a poll.
    assert_eq!(arm.timeout_ms, AWAIT_CLAMP_MS);
}

// -- the storm, end to end -------------------------------------------------------

#[test]
fn a_fake_overload_storm_walks_the_transient_row_end_to_end() {
    let mut w = watcher();
    // Three `overloaded` failures plus a `PostModelSwitch`: a storm, and two
    // independent channels, so it is a pair.
    for e in [sf("overloaded"), sf("overloaded"), sf("overloaded"), pms()] {
        w.wake(&Wake::Hook(HookEvent::Evidence(e)), NOW_MS, NOW_S);
    }
    assert_eq!(w.class(), Some(Class::TransientCapacity));
    let st = w.limits_state().expect("a live class");
    assert!(st.storm && !st.unpaired);

    // t = 0: let the vendor retry. Display only — the harness never fights a
    // retry the vendor is already doing.
    let p = w.plan(&host(), NOW_MS, NOW_S);
    let step0 = act_of(&p).expect("step 0").clone();
    assert_eq!(step0.limit_action, Some(Action::LetVendorRetry));
    assert_eq!(step0.level, 0);
    assert!(!step0.lease);
    assert!(step0.acts.iter().all(|a| a.level() <= 1), "display only");
    w.commit(&step0, 1, NOW_MS, NOW_S);

    // Before T1 nothing is acted on, and the moment is a DEADLINE, not a
    // countdown.
    let wait = w.plan(&host(), NOW_MS + 1000, NOW_S + 1);
    assert!(matches!(
        wait,
        Plan::Wait {
            timer: Timer::Schedule,
            ..
        }
    ));
    let arm = w.next_arm(NOW_MS + 1000, NOW_S + 1).expect("an arm");
    assert_eq!(arm.timer, Timer::Schedule);
    assert!(arm.timeout_ms <= AWAIT_CLAMP_MS && arm.timeout_ms > 0);

    // At T1 (300 s) with a storm: switch-model, typed, fenced, under a lease.
    let at_t1_ms = NOW_MS + 300_000;
    let at_t1_s = NOW_S + 300;
    let p = w.plan(&host(), at_t1_ms, at_t1_s);
    let switch = act_of(&p).expect("the switch").clone();
    assert_eq!(switch.limit_action, Some(Action::SwitchModel));
    assert_eq!(switch.level, 3);
    assert!(switch.lease, "an L3 act holds the turn lease");
    let sent = lines(&switch);
    assert!(
        sent[0].starts_with(&format!("turn id={NONCE}:7:1 ")),
        "{sent:?}"
    );
    assert!(sent[0].ends_with("/model opus"), "{sent:?}");
    assert!(sent[1].starts_with("key if="), "{sent:?}");
    w.commit(&switch, 2, at_t1_ms, at_t1_s);

    // While it is out, a second class arriving is QUEUED, never acted on.
    assert!(matches!(
        w.plan(&host(), at_t1_ms + 1, at_t1_s),
        Plan::Refused {
            why: Refuse::InFlight,
            ..
        }
    ));
    w.verdict(limits::Verdict::Executed, at_t1_ms + 30_000, at_t1_s + 30);
    assert_eq!(
        w.limits_state().expect("state").last_switch_at,
        Some(at_t1_s + 30)
    );

    // T2 (900 s): the wait arms, then a retry, then escalate — and the
    // generation is terminal after it.
    let p = w.plan(&host(), NOW_MS + 900_000, NOW_S + 900);
    let waiting = act_of(&p).expect("the wait").clone();
    assert_eq!(waiting.limit_action, Some(Action::Wait));
    w.commit(&waiting, 3, NOW_MS + 900_000, NOW_S + 900);
    let p = w.plan(&host(), NOW_MS + 1_500_000, NOW_S + 1500);
    let retry = act_of(&p).expect("the retry").clone();
    assert_eq!(retry.limit_action, Some(Action::Retry));
    assert!(
        lines(&retry)[0].ends_with("continue"),
        "the default retry text is the fixed word, never screen-derived"
    );
    w.commit(&retry, 4, NOW_MS + 1_500_000, NOW_S + 1500);
    w.verdict(limits::Verdict::Timeout, NOW_MS + 1_600_000, NOW_S + 1600);
    let p = w.plan(&host(), NOW_MS + 1_600_000, NOW_S + 1600);
    let esc = act_of(&p).expect("the escalation").clone();
    assert_eq!(esc.limit_action, Some(Action::Escalate));
    assert!(
        esc.acts.iter().any(|a| matches!(a, Act::Ask { .. })),
        "an escalation asks a human"
    );
    w.commit(&esc, 5, NOW_MS + 1_600_000, NOW_S + 1600);
    assert!(matches!(
        w.plan(&host(), NOW_MS + 1_601_000, NOW_S + 1601),
        Plan::Refused {
            why: Refuse::Terminal,
            ..
        }
    ));
}

// -- the exhausted window, end to end --------------------------------------------

#[test]
fn a_fake_exhausted_five_hour_window_waits_then_retries_and_never_switches_account() {
    let mut w = watcher();
    w.wake(
        &Wake::Hook(HookEvent::Evidence(sf("rate_limit"))),
        NOW_MS,
        NOW_S,
    );
    w.wake(
        &Wake::Hook(HookEvent::Evidence(window(100.0, NOW_S + 1800))),
        NOW_MS,
        NOW_S,
    );
    assert_eq!(w.class(), Some(Class::Session5hLimit));

    // `switch-account` is step 0 of this class's row, and it is SKIPPED:
    // `[accounts] enabled = false` and the shipped level is 3, so the step is
    // journaled as refused rather than silently dropped.
    let p = w.plan(&host(), NOW_MS, NOW_S);
    let first = act_of(&p).expect("something is due").clone();
    assert_ne!(
        first.limit_action,
        Some(Action::SwitchAccount),
        "account rotation needs [accounts] enabled AND level >= 4"
    );
    assert_eq!(first.limit_action, Some(Action::SwitchModel));
    w.commit(&first, 1, NOW_MS, NOW_S);
    w.verdict(limits::Verdict::Executed, NOW_MS + 1000, NOW_S + 1);

    // The wait is keyed on `resets_at`, and the retry lands at
    // `resets_at + jitter`, never before.
    let p = w.plan(&host(), NOW_MS + 2000, NOW_S + 2);
    let waiting = act_of(&p).expect("the wait").clone();
    assert_eq!(waiting.limit_action, Some(Action::Wait));
    w.commit(&waiting, 2, NOW_MS + 2000, NOW_S + 2);
    assert!(matches!(
        w.plan(&host(), NOW_MS + 3000, NOW_S + 3),
        Plan::Wait { .. }
    ));
    let due_s = NOW_S + 1800 + limits::RESET_JITTER_S;
    let p = w.plan(&host(), NOW_MS + 1_900_000, due_s);
    let retry = act_of(&p).expect("the retry").clone();
    assert_eq!(retry.limit_action, Some(Action::Retry));
}

#[test]
fn a_switch_the_harness_made_schedules_its_own_switch_back_and_a_vendor_switch_is_adopted() {
    let mut w = watcher();
    for e in [sf("overloaded"), sf("overloaded"), sf("overloaded"), pms()] {
        w.wake(&Wake::Hook(HookEvent::Evidence(e)), NOW_MS, NOW_S);
    }
    w.wake(
        &Wake::Hook(HookEvent::Evidence(Evidence::Banner {
            text: "Repeated 529 Overloaded errors".to_string(),
            resets_at: None,
        })),
        NOW_MS,
        NOW_S,
    );
    let p = w.plan(&host(), NOW_MS, NOW_S);
    let step0 = act_of(&p).expect("step 0").clone();
    w.commit(&step0, 1, NOW_MS, NOW_S);
    let p = w.plan(&host(), NOW_MS + 300_000, NOW_S + 300);
    let switch = act_of(&p).expect("the switch").clone();
    assert_eq!(switch.limit_action, Some(Action::SwitchModel));
    w.commit(&switch, 2, NOW_MS + 300_000, NOW_S + 300);
    w.verdict(limits::Verdict::Executed, NOW_MS + 330_000, NOW_S + 330);
    // No `resets_at` on a capacity storm, so there is nothing to schedule —
    // and the loop says so rather than inventing a moment.
    assert!(!w.switch_back_due(NOW_S + 10_000));

    // Trigger (c): a `PostModelSwitch` back to the primary that the harness
    // did not make. The state is ADOPTED, no budget is consumed, the dwell
    // starts.
    let before = w
        .limits_state()
        .expect("state")
        .budget
        .left(LimitsConfig::default().budget, NOW_S + 400);
    w.adopt_switch("fable", NOW_S + 400);
    assert!(!w.switch_back_due(NOW_S + 10_000));
    assert_eq!(
        w.limits_state()
            .expect("state")
            .budget
            .left(LimitsConfig::default().budget, NOW_S + 400),
        before,
        "adopting a switch spends nothing"
    );
}

// -- liveness --------------------------------------------------------------------

/// Put the watcher in the shape a hung tool leaves: a tool out, the bounded
/// `await seq` answered `OK timeout`, and aterm's own `since_ms` agreeing.
fn hung_tool(w: &mut Watcher, at_ms: u64) {
    w.wake(
        &Wake::Hook(HookEvent::PreToolUse {
            tool_use_id: "toolu_01".to_string(),
        }),
        NOW_MS,
        NOW_S,
    );
    let arm = w.next_arm(NOW_MS, NOW_S).expect("an arm");
    assert_eq!(arm.timer, Timer::ToolStall);
    assert_eq!(arm.timeout_ms, TOOL_STALL_MS);
    assert_eq!(arm.line(), format!("await seq 0 timeout={TOOL_STALL_MS}"));
    w.wake(&Wake::Deadline(Timer::ToolStall), at_ms, NOW_S + 600);
    w.sample(
        &sample(still("quiet", 2, TOOL_STALL_MS + 1)),
        at_ms,
        NOW_S + 600,
    );
}

#[test]
fn a_hung_tool_warns_and_does_not_nudge_at_the_default_level() {
    let mut w = watcher();
    assert_eq!(w.config().level, Level::Stop, "the shipped default");
    let t0 = NOW_MS + TOOL_STALL_MS;
    hung_tool(&mut w, t0);
    let v = w.liveness(t0);
    assert_eq!(v.class, Liveness::ToolStalled);
    assert_eq!(v.confidence, Confidence::Strong);
    assert!(v.reasons.contains(&"tool_use_id"));

    // Rung 1: warn. L1 only — an icon, a description, a notice. No typing.
    let p = w.plan(&host(), t0, NOW_S + 600);
    let warn = act_of(&p).expect("rung 1").clone();
    assert_eq!(warn.rung, Some(Rung::Warn));
    assert_eq!(warn.level, 1);
    assert!(
        warn.acts.iter().all(|a| !matches!(a, Act::Type { .. })),
        "rung 1 types nothing"
    );
    assert!(lines(&warn).iter().any(|l| l.contains("toolu_01")));
    w.commit(&warn, 1, t0, NOW_S + 600);

    // Inside `warn_s` nothing more is due, and the next moment is a DEADLINE.
    assert!(matches!(
        w.plan(&host(), t0 + 1000, NOW_S + 601),
        Plan::Wait {
            timer: Timer::Warn,
            ..
        }
    ));

    // At `warn_s` the badge lands — still rung 1, still L1.
    let p = w.plan(&host(), t0 + WARN_MS, NOW_S + 720);
    let badge = act_of(&p).expect("the badge").clone();
    assert_eq!(badge.rung, Some(Rung::Warn));
    assert!(
        badge
            .acts
            .iter()
            .any(|a| matches!(a, Act::Attention(why) if !why.is_empty())),
        "{:?}",
        badge.acts
    );
    w.commit(&badge, 2, t0 + WARN_MS, NOW_S + 720);
    // And it is taken ONCE: a badge that stayed due would re-take rung 1 on
    // every wake for ever.
    assert!(matches!(
        w.plan(&host(), t0 + WARN_MS + 1, NOW_S + 721),
        Plan::Idle
    ));

    // Past `nudge_after_s`, at the DEFAULT level, the ladder keeps going —
    // but it escalates rather than typing. Rung 3 is opt-in and is not taken.
    let late = t0 + WARN_MS + NUDGE_AFTER_MS;
    w.wake(&Wake::Deadline(Timer::ToolStall), late, NOW_S + 1700);
    w.sample(
        &sample(still("idle", 3, TOOL_STALL_MS + 1)),
        late,
        NOW_S + 1700,
    );
    let p = w.plan(&host(), late, NOW_S + 1700);
    let next = act_of(&p).expect("the ladder keeps going").clone();
    assert_eq!(next.rung, Some(Rung::Escalate));
    assert!(
        next.acts.iter().all(|a| !matches!(a, Act::Type { .. })),
        "nothing is typed at liveness.level = stop"
    );
}

#[test]
fn the_nudge_is_reached_only_when_the_owner_raised_the_level() {
    let mut w = Watcher::new(
        "s-1",
        WatchConfig {
            level: Level::Turn,
            ..cfg()
        },
        LimitsConfig::default(),
    );
    w.wake(&Wake::Generation(GEN), NOW_MS, NOW_S);
    let t0 = NOW_MS + TOOL_STALL_MS;
    hung_tool(&mut w, t0);
    let p = w.plan(&host(), t0, NOW_S + 600);
    let warn = act_of(&p).expect("rung 1").clone();
    w.commit(&warn, 1, t0, NOW_S + 600);
    let p = w.plan(&host(), t0 + WARN_MS, NOW_S + 720);
    let badge = act_of(&p).expect("the badge").clone();
    w.commit(&badge, 2, t0 + WARN_MS, NOW_S + 720);
    let late = t0 + WARN_MS + NUDGE_AFTER_MS;
    w.wake(&Wake::Deadline(Timer::ToolStall), late, NOW_S + 1700);
    w.sample(
        &sample(still("idle", 3, TOOL_STALL_MS + 1)),
        late,
        NOW_S + 1700,
    );
    let p = w.plan(&host(), late, NOW_S + 1700);
    let nudge = act_of(&p).expect("rung 3").clone();
    assert_eq!(nudge.rung, Some(Rung::Nudge));
    let sent = lines(&nudge);
    assert!(
        sent[0].contains("status check: are you blocked?"),
        "{sent:?}"
    );
    assert!(sent[1].starts_with("key if="), "the fence, always");
}

#[test]
fn every_explanation_for_a_quiet_screen_stops_the_ladder_before_it_starts() {
    // (i) The link `stop` hook is holding the turn on `await inbox`.
    let mut w = watcher();
    w.wake(
        &Wake::Hook(HookEvent::SessionStart { link_hooks: true }),
        NOW_MS,
        NOW_S,
    );
    w.wake(&Wake::Hook(HookEvent::Stop), NOW_MS, NOW_S);
    w.wake(&Wake::Deadline(Timer::ApiStall), NOW_MS + 1, NOW_S);
    w.sample(
        &sample(still("quiet", 2, API_STALL_MS + 1)),
        NOW_MS + 1,
        NOW_S,
    );
    assert_eq!(w.liveness(NOW_MS + 1).class, Liveness::StopHookWaiting);
    assert!(matches!(w.plan(&host(), NOW_MS + 1, NOW_S), Plan::Idle));

    // (ii) A GAP. The window was UNOBSERVED, not quiet.
    let mut w = watcher();
    w.wake(&Wake::Deadline(Timer::ApiStall), NOW_MS, NOW_S);
    w.sample(&sample(still("quiet", 2, API_STALL_MS + 1)), NOW_MS, NOW_S);
    assert_eq!(w.liveness(NOW_MS).class, Liveness::ApiStalled);
    w.wake(
        &Wake::Frame(Frame::Gap {
            bytes_dropped: 4096,
            resync: None,
            events_resync: None,
        }),
        NOW_MS + 1,
        NOW_S,
    );
    assert!(w.resync_due(), "a GAP owes a re-read");
    let v = w.liveness(NOW_MS + 1);
    assert_eq!(v.class, Liveness::Unobserved);
    assert!(v.reasons.contains(&"degraded:gap"));
    assert!(matches!(w.plan(&host(), NOW_MS + 1, NOW_S), Plan::Idle));

    // (iii) A human at the keyboard, read from `custody` — never from
    // `await momentum`, which latches when typing has STOPPED.
    let mut w = watcher();
    w.wake(&Wake::Custody { user: true }, NOW_MS, NOW_S);
    w.wake(&Wake::Deadline(Timer::ApiStall), NOW_MS + 1, NOW_S);
    w.sample(
        &sample(still("quiet", 2, API_STALL_MS + 1)),
        NOW_MS + 1,
        NOW_S,
    );
    let v = w.liveness(NOW_MS + 1);
    assert_eq!(v.class, Liveness::WaitingHuman);
    assert!(v.reasons.contains(&"custody"));
}

#[test]
fn a_screen_that_did_not_move_does_not_re_verdict_and_since_ms_must_agree() {
    let mut w = watcher();
    // Signal A fired but aterm's own `since_ms` disagrees: NOT a stall. This
    // is the two-signal rule's negative control.
    w.wake(&Wake::Deadline(Timer::ApiStall), NOW_MS, NOW_S);
    w.sample(&sample(status("running", 2, 5_000)), NOW_MS, NOW_S);
    let v = w.liveness(NOW_MS);
    assert_eq!(v.class, Liveness::Working);
    assert!(v.reasons.contains(&"since_ms-disagrees"));

    // Both signals: a stall, and rung 1 is taken with the revision recorded.
    w.sample(&sample(still("quiet", 3, API_STALL_MS + 1)), NOW_MS, NOW_S);
    w.wake(&Wake::Deadline(Timer::ApiStall), NOW_MS, NOW_S);
    assert_eq!(w.liveness(NOW_MS).class, Liveness::ApiStalled);
    let p = w.plan(&host(), NOW_MS, NOW_S);
    let warn = act_of(&p).expect("rung 1").clone();
    w.commit(&warn, 1, NOW_MS, NOW_S);

    // A second timeout on the SAME classification reads `working`: the
    // revision has not moved, so there is no new evidence to walk on.
    w.wake(&Wake::Deadline(Timer::ApiStall), NOW_MS + 1, NOW_S);
    let v = w.liveness(NOW_MS + 1);
    assert_eq!(v.class, Liveness::Working);
    assert!(v.reasons.contains(&"revision-unmoved"));
}

#[test]
fn stopped_short_is_unavailable_with_zero_hooks_and_says_so() {
    assert!(!stopped_short_available(false));
    assert!(stopped_short_available(true));
    // And the rest of the ladder is reachable anyway: this is the whole
    // inversion. A watcher that has never seen a hook still warns.
    let mut w = watcher();
    assert!(!w.hooks());
    w.wake(&Wake::Deadline(Timer::ApiStall), NOW_MS, NOW_S);
    w.sample(&sample(still("quiet", 2, API_STALL_MS + 1)), NOW_MS, NOW_S);
    let v = w.liveness(NOW_MS);
    assert_eq!(v.class, Liveness::ApiStalled);
    assert_eq!(
        v.confidence,
        Confidence::Heuristic,
        "no hook, so no exact evidence — lower confidence, not silence"
    );
    assert!(act_of(&w.plan(&host(), NOW_MS, NOW_S)).is_some());
}

// -- the refusals ----------------------------------------------------------------

fn switching_watcher() -> (Watcher, u64, i64) {
    let mut w = watcher();
    for e in [sf("overloaded"), sf("overloaded"), sf("overloaded"), pms()] {
        w.wake(&Wake::Hook(HookEvent::Evidence(e)), NOW_MS, NOW_S);
    }
    let p = w.plan(&host(), NOW_MS, NOW_S);
    let step0 = act_of(&p).expect("step 0").clone();
    w.commit(&step0, 1, NOW_MS, NOW_S);
    (w, NOW_MS + 300_000, NOW_S + 300)
}

#[test]
fn a_standing_hold_refuses_every_act_and_the_row_says_so() {
    let (w, at_ms, at_s) = switching_watcher();
    let mut g = host();
    g.hold = true;
    let p = w.plan(&g, at_ms, at_s);
    assert!(matches!(
        p,
        Plan::Refused {
            why: Refuse::Hold,
            ..
        }
    ));
    assert_eq!(Refuse::Hold.as_str(), "refused:hold");
}

#[test]
fn a_live_turn_refuses_an_l3_act_and_the_loop_re_awaits_the_boundary() {
    let (w, at_ms, at_s) = switching_watcher();
    let mut g = host();
    g.busy = true;
    assert!(matches!(
        w.plan(&g, at_ms, at_s),
        Plan::Refused {
            why: Refuse::Busy,
            ..
        }
    ));
    // The L0 display half is NOT refused by `busy`: it reaches the PTY
    // through nothing.
    let (w0, _, _) = {
        let mut w0 = watcher();
        for e in [sf("overloaded"), pms()] {
            w0.wake(&Wake::Hook(HookEvent::Evidence(e)), NOW_MS, NOW_S);
        }
        (w0, 0u64, 0i64)
    };
    let p = w0.plan(&g, NOW_MS, NOW_S);
    assert_eq!(
        act_of(&p).expect("the display half").limit_action,
        Some(Action::LetVendorRetry)
    );
}

#[test]
fn a_human_reading_right_now_defers_an_l3_act() {
    let (w, at_ms, at_s) = switching_watcher();
    let mut g = host();
    g.custody_user = true;
    assert!(matches!(
        w.plan(&g, at_ms, at_s),
        Plan::Refused {
            why: Refuse::Custody,
            ..
        }
    ));
    assert_eq!(Refuse::Custody.as_str(), "refused:custody");
}

#[test]
fn a_dry_switch_budget_degrades_to_the_display_half_rather_than_acting() {
    let mut w = watcher();
    for e in [sf("overloaded"), sf("overloaded"), sf("overloaded"), pms()] {
        w.wake(&Wake::Hook(HookEvent::Evidence(e)), NOW_MS, NOW_S);
    }
    // Spend the shared switch budget dry through the engine's own ledger.
    let budget = LimitsConfig::default().budget;
    if let Some(st) = w.limits.as_mut() {
        st.step = 1;
        for _ in 0..budget.count {
            st.budget.spend(NOW_S);
        }
        st.last_switch_at = None;
    }
    let p = w.plan(&host(), NOW_MS + 300_000, NOW_S + 300);
    let degraded = act_of(&p).expect("a degraded act").clone();
    assert_eq!(degraded.limit_action, Some(Action::SwitchModel));
    assert_eq!(degraded.level, 1, "L3 degraded to the L1 half");
    assert!(
        degraded.reason.contains("degraded:budget"),
        "{}",
        degraded.reason
    );
    assert!(
        degraded.acts.iter().all(|a| !matches!(a, Act::Type { .. })),
        "a dry budget types nothing"
    );
}

#[test]
fn the_escape_rung_is_refused_by_its_own_dry_bucket() {
    let mut w = Watcher::new(
        "s-1",
        WatchConfig {
            level: Level::Escape,
            ..cfg()
        },
        LimitsConfig::default(),
    );
    w.wake(&Wake::Generation(GEN), NOW_MS, NOW_S);
    let t0 = NOW_MS + TOOL_STALL_MS;
    hung_tool(&mut w, t0);
    for _ in 0..w.config().escape_budget.count {
        w.live.escapes.spend(0);
    }
    // Walk to the escape rung.
    w.live.rung = Some(Rung::Nudge);
    w.live.warned_at = Some(t0);
    w.live.badged = true;
    let late = t0 + TOOL_STALL_MS * 2;
    w.wake(&Wake::Deadline(Timer::ToolStall), late, NOW_S + 2000);
    w.sample(
        &sample(still("idle", 4, TOOL_STALL_MS + 1)),
        late,
        NOW_S + 2000,
    );
    assert!(matches!(
        w.plan(&host(), late, NOW_S + 2000),
        Plan::Refused {
            why: Refuse::Budget,
            ..
        }
    ));
}

#[test]
fn a_stale_generation_is_dropped_rather_than_applied_late() {
    let (mut w, at_ms, at_s) = switching_watcher();
    let mut g = host();
    g.generation = GEN + 1;
    assert!(matches!(
        w.plan(&g, at_ms, at_s),
        Plan::Refused {
            why: Refuse::Generation,
            ..
        }
    ));
    // And when the loop ADOPTS the new generation, everything pending goes
    // with the old one: the class, the evidence, the act in flight.
    let p = w.plan(&host(), at_ms, at_s);
    let switch = act_of(&p).expect("the switch").clone();
    w.commit(&switch, 2, at_ms, at_s);
    assert!(w.open_act().is_some());
    w.wake(&Wake::Generation(GEN + 1), at_ms + 1, at_s);
    assert_eq!(w.generation(), GEN + 1);
    assert_eq!(w.class(), None);
    assert!(w.open_act().is_none());
    // A verdict that arrives AFTER the generation moved changes nothing.
    w.verdict(limits::Verdict::Executed, at_ms + 2, at_s);
    assert_eq!(w.class(), None);
}

#[test]
fn a_bypassed_harness_and_a_disabled_watcher_both_refuse_and_name_themselves() {
    let (w, at_ms, at_s) = switching_watcher();
    let mut g = host();
    g.engaged = false;
    assert!(matches!(
        w.plan(&g, at_ms, at_s),
        Plan::Refused {
            why: Refuse::Bypassed,
            ..
        }
    ));
    let off = Watcher::new(
        "s-1",
        WatchConfig {
            enabled: false,
            ..cfg()
        },
        LimitsConfig::default(),
    );
    assert!(matches!(
        off.plan(&host(), at_ms, at_s),
        Plan::Refused {
            why: Refuse::Disabled,
            ..
        }
    ));
}

// -- ingress: no race between the two channels -----------------------------------

#[test]
fn a_pushed_frame_and_a_hook_event_converge_whichever_lands_first() {
    let evidence = Evidence::Banner {
        text: "You've reached your weekly usage limit".to_string(),
        resets_at: Some(NOW_S + 3600),
    };
    let frame = Wake::Frame(Frame::Delta { seq: Some(41) });
    let hook = Wake::Hook(HookEvent::Evidence(evidence.clone()));

    let weekly = Wake::Hook(HookEvent::Evidence(window_of(
        limits::WindowKind::SevenDay,
        100.0,
        NOW_S + 3600,
    )));
    let failure = Wake::Hook(HookEvent::Evidence(sf("rate_limit")));

    let mut a = watcher();
    a.wake(&frame, NOW_MS, NOW_S);
    a.wake(&hook, NOW_MS + 1, NOW_S);
    a.wake(&failure, NOW_MS + 2, NOW_S);
    a.wake(&weekly, NOW_MS + 3, NOW_S);

    let mut b = watcher();
    b.wake(&hook, NOW_MS, NOW_S);
    b.wake(&weekly, NOW_MS + 1, NOW_S);
    b.wake(&failure, NOW_MS + 2, NOW_S);
    b.wake(&frame, NOW_MS + 3, NOW_S);

    assert_eq!(a.class(), b.class());
    assert_eq!(a.class(), Some(Class::Weekly7dLimit));
    let pa = a.plan(&host(), NOW_MS + 10, NOW_S);
    let pb = b.plan(&host(), NOW_MS + 10, NOW_S);
    assert_eq!(
        act_of(&pa).map(|p| p.limit_action),
        act_of(&pb).map(|p| p.limit_action),
        "the two orders decide the same thing"
    );
    // The frame moved the quiet clock in BOTH, and neither is a stall: a
    // class in force hands the session to the recovery table.
    assert_eq!(a.liveness(NOW_MS + 10).class, Liveness::QuotaWait);
    assert_eq!(b.liveness(NOW_MS + 10).class, Liveness::QuotaWait);
}

// -- the pump --------------------------------------------------------------------

#[test]
fn one_pump_parks_reads_journals_acts_and_writes_the_verdict_in_that_order() {
    let mut w = watcher();
    for e in [sf("overloaded"), sf("overloaded"), sf("overloaded"), pms()] {
        w.wake(&Wake::Hook(HookEvent::Evidence(e)), NOW_MS, NOW_S);
    }
    let p = w.plan(&host(), NOW_MS, NOW_S);
    let step0 = act_of(&p).expect("step 0").clone();
    w.commit(&step0, 1, NOW_MS, NOW_S);

    // One wake, 300 s later: the switch is due.
    let mut wire = FakeWire::new(vec![(300_000, Wake::Deadline(Timer::Schedule))]);
    wire.status = status_line("idle", 9, 300_000, false);
    wire.grid = Some(
        aterm_json::to_string(&aterm_json::Value::Object(
            [
                (
                    "rows".to_string(),
                    aterm_json::Value::Array(vec![aterm_json::Value::from("❯ ".to_string())]),
                ),
                ("seq".to_string(), aterm_json::Value::from(41u64)),
                ("first".to_string(), aterm_json::Value::from(0u64)),
            ]
            .into_iter()
            .collect(),
        ))
        .expect("grid json"),
    );
    let pass = w.pump(&mut wire);
    let planned = act_of(&pass.plan).expect("the switch").clone();
    assert_eq!(planned.limit_action, Some(Action::SwitchModel));
    // The arm the loop asked for is an `await`, not a sleep.
    assert_eq!(wire.armed.len(), 1);
    assert!(wire.armed[0].line().starts_with("await "));
    // The journal row came FIRST, before any line was sent.
    assert_eq!(wire.rows[0].0, RING_RECOVERY);
    assert!(wire.rows[0].1.contains("\"action\":\"switch-model\""));
    assert!(wire.rows[0].1.contains("\"cap\":\"limits\""));
    assert!(wire.rows[0].1.contains(&format!("\"gen\":{GEN}")));
    // Then the lease, the fenced pair, the release.
    assert!(
        wire.sent[0].starts_with("lease acquire ttl="),
        "{:?}",
        wire.sent
    );
    assert!(
        wire.sent[1].starts_with(&format!("turn id={NONCE}:7:1 ")),
        "{:?}",
        wire.sent
    );
    assert!(wire.sent[2].starts_with("key if="), "{:?}", wire.sent);
    // The release NAMES the holder: `lease_release` refuses a bare release
    // of a live cooperative lease, so the bare form left the lease standing
    // for its whole TTL and refused every other driver `ERR busy`.
    assert_eq!(wire.sent[3], format!("lease release holder={LEASE_HOLDER}"));
    // Then the verdict row.
    assert_eq!(pass.verdict.as_deref(), Some("executed"));
    assert!(wire.rows[1].1.contains("\"verdict\":\"executed\""));
    assert!(wire.rows[1].1.contains("\"ref\":1"));
}

#[test]
fn a_refused_send_is_journaled_as_a_refusal_and_never_as_an_execution() {
    let mut w = watcher();
    for e in [sf("overloaded"), sf("overloaded"), sf("overloaded"), pms()] {
        w.wake(&Wake::Hook(HookEvent::Evidence(e)), NOW_MS, NOW_S);
    }
    let p = w.plan(&host(), NOW_MS, NOW_S);
    let step0 = act_of(&p).expect("step 0").clone();
    w.commit(&step0, 1, NOW_MS, NOW_S);
    let mut wire = FakeWire::new(vec![(300_000, Wake::Deadline(Timer::Schedule))]);
    wire.status = status_line("idle", 9, 300_000, false);
    wire.refuse = vec![(
        "turn ".to_string(),
        "ERR halted reason=fleet origin=local".to_string(),
    )];
    let pass = w.pump(&mut wire);
    // The CAUSE the server stated. `ERR halted` is a standing hold, and a
    // ledger that spells it `refused:busy` hides which of three different
    // refusals stopped the act.
    assert_eq!(pass.verdict.as_deref(), Some("refused:hold"));
    assert!(
        !wire.sent.iter().any(|l| l.starts_with("key ")),
        "a refused turn never gets its press"
    );
}

#[test]
fn an_idle_session_costs_the_loop_a_status_and_nothing_else() {
    let mut w = watcher();
    let mut wire = FakeWire::new(vec![
        (0, Wake::Frame(Frame::Delta { seq: Some(1) })),
        (0, Wake::Frame(Frame::Delta { seq: Some(1) })),
    ]);
    // `revision` never moves, so the grid is read ONCE (the first read is
    // always due) and never again.
    wire.grid = Some(r#"{"rows":["❯ "],"seq":1,"first":0}"#.to_string());
    let first = w.pump(&mut wire);
    let second = w.pump(&mut wire);
    assert!(matches!(first.plan, Plan::Idle | Plan::Wait { .. }));
    assert!(matches!(second.plan, Plan::Idle | Plan::Wait { .. }));
    assert!(wire.sent.is_empty(), "an idle session is not acted on");
    assert!(
        wire.rows.is_empty(),
        "and it writes no ledger row either: {:?}",
        wire.rows
    );
}

// -- the no-timer rule -----------------------------------------------------------

#[test]
fn no_code_path_in_the_loop_sleeps_where_an_await_would_do() {
    // The source itself is the evidence. aterm exists so nothing samples on a
    // clock, and the ONE place a duration is computed is `next_arm`, which
    // turns it into `await … timeout=<ms>`.
    let src = include_str!("watch.rs");
    let code: String = src
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    for banned in [
        "thread::sleep",
        "sleep(",
        "Instant::now",
        "SystemTime::now",
        "std::time::",
        "spin_loop",
        "park_timeout",
        "recv_timeout",
        "Duration::from",
    ] {
        assert!(
            !code.contains(banned),
            "watch.rs reaches for {banned:?} — every deadline here is an `await … timeout=`"
        );
    }
    // Every deadline really does become one `await` line, clamped at the
    // server's own clamp.
    let w = watcher();
    let arm = w.next_arm(NOW_MS, NOW_S).expect("an arm");
    assert!(arm.line().starts_with("await "), "{}", arm.line());
    assert!(arm.timeout_ms <= AWAIT_CLAMP_MS);
    // And a deadline beyond the clamp arms AT the clamp rather than being
    // counted down to.
    let far = Watcher::new(
        "s-1",
        WatchConfig {
            api_stall_ms: 10 * AWAIT_CLAMP_MS,
            ..cfg()
        },
        LimitsConfig::default(),
    );
    assert_eq!(
        far.next_arm(NOW_MS, NOW_S).expect("an arm").timeout_ms,
        AWAIT_CLAMP_MS
    );
}

#[test]
fn a_closed_session_asks_for_no_further_wait() {
    let mut w = watcher();
    w.wake(
        &Wake::Frame(Frame::Event {
            kind: "exited".to_string(),
        }),
        NOW_MS,
        NOW_S,
    );
    assert!(w.closed());
    assert_eq!(w.next_arm(NOW_MS, NOW_S), None);
}

// -- the ledger rows -------------------------------------------------------------

#[test]
fn a_journal_row_is_one_line_of_json_whatever_text_it_quotes() {
    let w = watcher();
    let planned = Planned {
        acts: vec![Act::Attention("a badge".to_string())],
        cap: "liveness",
        action: "warn".to_string(),
        level: 1,
        reason: "a \"quoted\"\nreason".to_string(),
        lease: false,
        step: None,
        limit_action: None,
        rung: Some(Rung::Warn),
        wait_until: None,
    };
    let row = w.journal_row(&planned, NOW_S, &[9101, 9127]);
    assert_eq!(row.lines().count(), 1, "{row}");
    assert!(row.contains("[9101,9127]"), "{row}");
    // The ledger ESCAPES what it quotes rather than erasing it: the newline
    // survives as `\n` and the row is still one line.
    assert!(row.contains(r#""reason":"a \"quoted\"\nreason""#), "{row}");
    let parsed = aterm_json::from_str::<aterm_json::Value>(&row).expect("valid JSON");
    assert_eq!(
        parsed.get("cap").and_then(aterm_json::Value::as_str),
        Some("liveness")
    );
    let v = verdict_row(2, 1, NOW_S, "refused:hold", "switch-model");
    assert_eq!(v.lines().count(), 1);
    assert!(aterm_json::from_str::<aterm_json::Value>(&v).is_ok());
}

#[test]
fn every_refusal_spelling_is_the_ledger_vocabulary() {
    for (r, s) in [
        (Refuse::Hold, "refused:hold"),
        (Refuse::Busy, "refused:busy"),
        (Refuse::Custody, "refused:custody"),
        (Refuse::Generation, "refused:generation"),
        (Refuse::Disabled, "refused:disabled"),
        (Refuse::Level, "refused:level"),
        (Refuse::InFlight, "refused:in-flight"),
        (Refuse::Bypassed, "refused:bypassed"),
        (Refuse::Budget, "refused:budget"),
        (Refuse::Terminal, "refused:terminal"),
    ] {
        assert_eq!(r.as_str(), s);
        assert!(s.starts_with("refused:"));
    }
    // The engine's own refusals map onto the same vocabulary rather than a
    // second one.
    assert_eq!(Refuse::from(limits::Refusal::Hold), Refuse::Hold);
    assert_eq!(Refuse::from(limits::Refusal::Busy), Refuse::Busy);
    assert_eq!(
        Refuse::from(limits::Refusal::StaleGeneration),
        Refuse::Generation
    );
}

#[test]
fn the_level_and_timer_vocabularies_round_trip() {
    for l in [Level::Warn, Level::Stop, Level::Turn, Level::Escape] {
        assert_eq!(Level::parse(l.as_str()), Some(l));
    }
    assert_eq!(Level::parse("nudge"), None);
    assert_eq!(Level::parse(""), None);
    // The ladder's order is the ladder's order, and `Ord` is what enforces
    // `rung.needs() > cfg.level`.
    assert!(Level::Warn < Level::Stop && Level::Stop < Level::Turn);
    assert!(Rung::Warn < Rung::StopBlock && Rung::StopBlock < Rung::Nudge);
    assert_eq!(Rung::Warn.needs(), Level::Warn);
    assert_eq!(Rung::Escalate.needs(), Level::Warn);
    assert_eq!(Rung::StopBlock.needs(), Level::Stop);
    assert_eq!(Timer::ToolStall.as_str(), "tool-stall");
    assert_eq!(Liveness::StoppedShort.as_str(), "stopped-short");
    assert!(!Liveness::StopHookWaiting.is_stall());
    assert!(!Liveness::Unobserved.is_stall());
    assert!(Liveness::ToolStalled.is_stall());
}

#[test]
fn a_stop_block_is_answered_in_band_and_names_the_harness() {
    let mut w = Watcher::new("s-1", cfg(), LimitsConfig::default());
    w.wake(&Wake::Generation(GEN), NOW_MS, NOW_S);
    let t0 = NOW_MS + TOOL_STALL_MS;
    hung_tool(&mut w, t0);
    let p = w.plan(&host(), t0, NOW_S + 600);
    let warn = act_of(&p).expect("rung 1").clone();
    w.commit(&warn, 1, t0, NOW_S + 600);
    let p = w.plan(&host(), t0 + WARN_MS, NOW_S + 720);
    let badge = act_of(&p).expect("the badge").clone();
    w.commit(&badge, 2, t0 + WARN_MS, NOW_S + 720);
    // A `Stop` arrives: rung 2 is due, and it is the ONLY rung that needs a
    // hook.
    w.wake(&Wake::Hook(HookEvent::Stop), t0 + WARN_MS + 1, NOW_S + 721);
    w.wake(
        &Wake::Deadline(Timer::ToolStall),
        t0 + WARN_MS + 1,
        NOW_S + 721,
    );
    w.sample(
        &sample(still("idle", 5, TOOL_STALL_MS + 1)),
        t0 + WARN_MS + 1,
        NOW_S + 721,
    );
    let p = w.plan(&host(), t0 + WARN_MS + 1, NOW_S + 721);
    let block = act_of(&p).expect("rung 2").clone();
    assert_eq!(block.rung, Some(Rung::StopBlock));
    let Some(Act::StopBlock { reason }) = block.acts.first() else {
        panic!("rung 2 is a stop block");
    };
    assert!(reason.starts_with(mark::STEM), "{reason}");
    assert!(
        block.acts.iter().all(|a| a.line().is_none()),
        "a stop block is printed by the bridge, never sent over the socket"
    );
}

#[test]
fn a_screen_with_a_composer_and_no_banner_leaves_the_classifier_alone() {
    // The spine path, with no hook anywhere: a grid that says nothing about a
    // limit produces no class, and the loop simply parks.
    let mut w = watcher();
    let rows = crate::supervise::prompt::fixtures::composer("");
    let mut s = sample(status("idle", 2, 1000));
    s.grid = Some(screen(rows, 41));
    w.sample(&s, NOW_MS, NOW_S);
    assert_eq!(w.class(), None);
    assert!(matches!(w.plan(&host(), NOW_MS, NOW_S), Plan::Idle));
}

#[test]
fn a_painted_usage_panel_reaches_the_classifier_from_the_grid_alone() {
    // ZERO hooks, and the spine still supplies the window figure §5.8.1
    // ranks first. The rows are the `/usage` panel Claude Code 2.1.278
    // painted in a real session, captured 2026-09-22.
    let mut w = watcher();
    let rows: Vec<String> = "\
   Current session
   ███▌                                               7% used
   Resets 1:20pm

   Current week (all models)
   ███████████████████████████████                    100% used
   Resets Sep 23 at 12pm
"
    .lines()
    .map(str::to_owned)
    .collect();
    let mut s = sample(status("idle", 2, 1000));
    s.grid = Some(screen(rows.clone(), 41));
    w.sample(&s, NOW_MS, NOW_S);
    // The painted 100 % names the weekly class on its own; it is ONE source,
    // so the verdict says so rather than pretending to a pair.
    assert_eq!(w.class(), Some(Class::Weekly7dLimit));
    assert!(
        w.limits_state().expect("state").unpaired,
        "a percentage corroborates; alone it is never a pair"
    );

    // The same panel read again is the SAME frame: no new evidence, and the
    // evidence vector does not grow a copy per revision.
    let held = w.limits_state().expect("state").hits.len();
    let mut again = sample(status("idle", 3, 1200));
    again.grid = Some(screen(rows, 42));
    w.sample(&again, NOW_MS + 10, NOW_S + 1);
    assert_eq!(
        w.limits_state().expect("state").hits.len(),
        held,
        "a redrawn panel is not a second limit event"
    );

    // NEGATIVE CONTROL: a screen with no panel leaves the classifier alone,
    // so the reader is not finding windows in anything it is shown.
    let mut plain = watcher();
    let mut blank = sample(status("idle", 2, 1000));
    blank.grid = Some(screen(crate::supervise::prompt::fixtures::composer(""), 41));
    plain.sample(&blank, NOW_MS, NOW_S);
    assert_eq!(plain.class(), None);
}

#[test]
fn the_live_spine_read_scrapes_the_painted_panel_the_same_as_an_injected_sample() {
    // THE REGRESSION THIS TEST EXISTS FOR: `read_spine` — the path the loop
    // actually runs — folded a SUBSET of what `sample` folds, and the subset
    // left out the painted `/usage` figures. Every test above drove `sample`,
    // which only tests call, so the rank-1 evidence reached the classifier in
    // the suite and never in a session. One folder now serves both, and this
    // drives the TRANSPORT side of it.
    let rows: Vec<String> = "\
   Current session
   ███▌                                               7% used
   Resets 1:20pm

   Current week (all models)
   ███████████████████████████████                    100% used
   Resets Sep 23 at 12pm
"
    .lines()
    .map(str::to_owned)
    .collect();
    let mut wire = FakeWire::new(Vec::new());
    wire.status = status_line("idle", 2, 1000, false);
    wire.grid = Some(grid_json(&rows, 41));

    let mut w = watcher();
    w.read_spine(&mut wire, NOW_MS, NOW_S);
    assert_eq!(
        w.class(),
        Some(Class::Weekly7dLimit),
        "the loop's own read must see what the panel paints"
    );

    // NEGATIVE CONTROL: the same path over a screen with no panel classifies
    // nothing, so the assertion above is about the panel and not the path.
    let mut plain_wire = FakeWire::new(Vec::new());
    plain_wire.status = status_line("idle", 2, 1000, false);
    plain_wire.grid = Some(grid_json(
        &crate::supervise::prompt::fixtures::composer(""),
        41,
    ));
    let mut plain = watcher();
    plain.read_spine(&mut plain_wire, NOW_MS, NOW_S);
    assert_eq!(plain.class(), None);
}

// -- the approval fence, the stated refusal, the unwind, the bound ---------------

/// A watcher carrying a paired five-hour limit: the state that reaches L3.
fn limited_watcher() -> Watcher {
    let mut w = watcher();
    for e in [sf("rate_limit"), window(100.0, NOW_S + 1800)] {
        w.wake(&Wake::Hook(HookEvent::Evidence(e)), NOW_MS, NOW_S);
    }
    assert_eq!(w.class(), Some(Class::Session5hLimit));
    assert!(
        !w.limits_state().expect("state").unpaired,
        "the fixture must be a PAIR or nothing below is about L3"
    );
    w
}

/// Put Claude Code's real approval box on the grid the watcher reads.
fn show_approval(w: &mut Watcher) {
    let mut s = sample(status("running", 2, 1000));
    s.grid = Some(screen(
        crate::supervise::prompt::fixtures::bash_one_row(),
        41,
    ));
    w.sample(&s, NOW_MS, NOW_S);
}

#[test]
fn an_approval_box_refuses_every_typed_act_on_the_limit_path() {
    // THE FINDING. The `prompt` short-circuit lives in `classify_liveness`,
    // which the LIMIT path never reaches — and `classify_liveness` answers on
    // the class BEFORE it looks at the box, so once a class was in force the
    // approval box was invisible to every decision `plan_limits` made.
    let mut w = limited_watcher();
    // Without the box, the class acts: this is the control that keeps the
    // assertion below from passing vacuously.
    assert!(
        act_of(&w.plan(&host(), NOW_MS, NOW_S)).is_some_and(|p| p.level >= 3),
        "the fixture reaches L3 with no box on the screen"
    );

    show_approval(&mut w);
    match w.plan(&host(), NOW_MS, NOW_S) {
        Plan::Refused { why, what } => {
            assert_eq!(why, Refuse::WaitingHuman);
            assert_eq!(why.as_str(), "refused:waiting-human");
            assert!(!what.is_empty());
        }
        other => panic!("an approval box is human-only, got {other:?}"),
    }

    // And the refusal is JOURNALLED, not silent: a pass that refuses writes
    // its verdict row like any other.
    let mut wire = FakeWire::new(vec![(0, Wake::Frame(Frame::Delta { seq: Some(9) }))]);
    wire.status = status_line("running", 2, 1000, false);
    wire.grid = Some(grid_json(
        &crate::supervise::prompt::fixtures::bash_one_row(),
        41,
    ));
    let pass = w.pump(&mut wire);
    assert_eq!(pass.verdict.as_deref(), Some("refused:waiting-human"));
    assert!(
        !wire
            .sent
            .iter()
            .any(|l| l.starts_with("turn ") || l.starts_with("key ")),
        "nothing is typed at an approval box: {:?}",
        wire.sent
    );
}

/// The rows of a screen as the `text --json` payload the loop parses.
fn grid_json(rows: &[String], seq: u64) -> String {
    let quoted = rows
        .iter()
        .map(|r| format!("\"{}\"", r.replace('\\', "\\\\").replace('"', "\\\"")))
        .collect::<Vec<_>>()
        .join(",");
    format!("{{\"rows\":[{quoted}],\"seq\":{seq},\"first\":0}}")
}

#[test]
fn the_submit_guard_arms_on_a_composer_row_and_never_on_an_option_row() {
    // The SHIPPED matcher — `aterm_observe::row_matcher` is what the server
    // compiles a `key if=<re>` guard with, so this tests the real decision
    // and not a second reading of the pattern.
    let guard = aterm_observe::row_matcher(crate::claude_composer_ready_pattern())
        .expect("the guard compiles");
    let box_rows = crate::supervise::prompt::fixtures::bash_one_row();
    let option = box_rows
        .iter()
        .find(|r| r.contains("1. Yes"))
        .expect("the measured box has a highlighted option");
    assert!(option.contains('❯'), "{option:?}");

    // The OLD guard armed on it. That is the defect, stated as a test so it
    // cannot come back by someone "simplifying" the pattern.
    let old = aterm_observe::row_matcher(crate::claude_prompt_ready_pattern())
        .expect("the old guard compiles");
    assert!(
        old.matches(option),
        "the plain-caret pattern matches the approval option row"
    );
    assert!(
        !guard.matches(option),
        "the composer guard must not arm on {option:?}"
    );
    for row in &box_rows {
        assert!(
            !guard.matches(row),
            "no row of the approval box may arm the submit: {row:?}"
        );
    }

    // And it DOES arm on the rows this loop actually types.
    for typed in ["❯ /model opus", "│ ❯ continue", "  ❯ /login"] {
        assert!(guard.matches(typed), "{typed:?} is a composer row");
    }
    // Numbered drafts are refused too. The tie breaks toward not pressing:
    // `OK skipped` writes nothing, and the next pass measures again.
    assert!(!guard.matches("❯ 1. Keep the harness"));
}

#[test]
fn every_typed_act_in_the_vocabulary_is_guarded_on_the_composer() {
    // A new `Act::Submit` built with the plain caret pattern would pass every
    // other test in this file; this is the one that catches it.
    let mut w = auth_watcher();
    w.cfg.accounts_enabled = true;
    w.cfg.account_dir = Some("/tmp/does-not-matter".to_string());
    let mut seen = 0;
    for action in [
        Action::Retry,
        Action::SwitchModel,
        Action::LowerPriority,
        Action::LimitReset,
        Action::Relogin,
        Action::SwitchAccount,
    ] {
        let st = w.limits_state().expect("state").clone();
        for act in w.acts_for(action, 4, &st, NOW_S) {
            if let Act::Submit { guard } | Act::Escape { guard } = &act {
                seen += 1;
                assert_eq!(
                    guard,
                    crate::claude_composer_ready_pattern(),
                    "{action:?} submits on the wrong guard"
                );
            }
        }
    }
    assert!(seen >= 4, "the sweep saw {seen} guarded presses");
}

#[test]
fn the_stated_refusal_is_the_word_the_server_used() {
    assert_eq!(
        refusal_of("ERR halted reason=fleet origin=local"),
        Some(Refuse::Hold)
    );
    assert_eq!(refusal_of("ERR busy turn=7"), Some(Refuse::Busy));
    assert_eq!(refusal_of("ERR busy lease=op"), Some(Refuse::Busy));
    assert_eq!(
        refusal_of("ERR rate (self-feed floor)"),
        Some(Refuse::Budget)
    );
    assert_eq!(Refuse::Hold.as_str(), "refused:hold");
    assert_eq!(Refuse::Budget.as_str(), "refused:budget");
    // The NEGATIVE controls — the whole reason this is a word and not a
    // substring scan. Every one of these was a refusal under `contains`.
    for not_a_refusal in [
        "ERR badregex",
        "ERR unsupported",
        "ERR notfound sid=separate-run",
        "the socket closed while writing corporate.txt",
        "ERR timeout id=7",
        "",
    ] {
        assert_eq!(
            refusal_of(not_a_refusal),
            None,
            "{not_a_refusal:?} is not a stated refusal"
        );
    }
}

#[test]
fn a_fence_whose_enter_is_refused_clears_the_composer_it_typed_into() {
    // `turn … submit=none` types WITHOUT submitting and does not clear the
    // composer, so a half-landed fence leaves this loop's text on screen and
    // the next table row's Enter would submit `/model opuscontinue`.
    let mut w = limited_watcher();
    assert!(
        act_of(&w.plan(&host(), NOW_MS, NOW_S)).is_some_and(|p| p.level >= 3),
        "this test is about a typed act"
    );
    let mut wire = FakeWire::new(vec![(0, Wake::Frame(Frame::Delta { seq: Some(9) }))]);
    wire.status = status_line("idle", 9, 300_000, false);
    // The TYPE lands; the guarded Enter is refused. The refusal names the
    // Enter LINE exactly, so the compensating clear — same verb, same guard,
    // different key — is left to go through.
    wire.refuse = vec![(
        format!("key if={} enter", crate::claude_composer_ready_pattern()),
        "ERR busy turn=4".to_string(),
    )];
    let pass = w.pump(&mut wire);
    assert_eq!(pass.verdict.as_deref(), Some("refused:busy"));
    let cleared: Vec<&String> = wire.sent.iter().filter(|l| l.contains("ctrl+u")).collect();
    assert_eq!(cleared.len(), 1, "one clear, once: {:?}", wire.sent);
    assert!(
        cleared[0].starts_with(&format!(
            "key if={}",
            crate::claude_composer_ready_pattern()
        )),
        "the clear is guarded on the composer: {:?}",
        cleared[0]
    );
    assert!(
        wire.rows
            .iter()
            .any(|(_, row)| row.contains("composer-cleared")),
        "the unwind is journalled: {:?}",
        wire.rows
    );
}

#[test]
fn a_clear_that_is_itself_refused_says_the_composer_is_dirty() {
    // A hold that landed between the two sends refuses the clear as well.
    // The loop cannot fix that; what it owes is a ledger row that says the
    // composer still holds its text, rather than silence.
    let mut w = limited_watcher();
    let mut wire = FakeWire::new(vec![(0, Wake::Frame(Frame::Delta { seq: Some(9) }))]);
    wire.status = status_line("idle", 9, 300_000, false);
    wire.refuse = vec![("key ".to_string(), "ERR halted reason=fleet".to_string())];
    let pass = w.pump(&mut wire);
    assert_eq!(pass.verdict.as_deref(), Some("refused:hold"));
    assert!(
        wire.rows
            .iter()
            .any(|(_, row)| row.contains("composer-dirty")),
        "a clear that did not land is said out loud: {:?}",
        wire.rows
    );
}

#[test]
fn a_fence_that_never_typed_clears_nothing() {
    // The negative control for the test above: when the TYPE itself is
    // refused there is no text on the screen, so no clear is owed.
    let mut w = limited_watcher();
    let mut wire = FakeWire::new(vec![(0, Wake::Frame(Frame::Delta { seq: Some(9) }))]);
    wire.status = status_line("idle", 9, 300_000, false);
    wire.refuse = vec![("turn ".to_string(), "ERR halted reason=fleet".to_string())];
    let pass = w.pump(&mut wire);
    assert_eq!(pass.verdict.as_deref(), Some("refused:hold"));
    assert!(
        !wire.sent.iter().any(|l| l.contains("ctrl+u")),
        "nothing was typed, so nothing is cleared: {:?}",
        wire.sent
    );
}

#[test]
fn an_account_rotation_sends_no_line_and_is_refused_by_its_own_name() {
    // MEASURED 2026-09-22 against a live instance and against the parser:
    // `spawn` admits `cwd|identity|window|raise|split|connected|place|of` and
    // NOTHING else, so the `spawn env=CLAUDE_CONFIG_DIR=… cmd=claude` this
    // act used to build was `ERR usage:` every time — read back as
    // `refused:unresolved`, the same word the roster uses for "no candidate
    // account", while the switch budget was still spent. There is no
    // admitted line, so there is no line.
    let mut w = watcher();
    w.limits = Some(limits::State::new(
        &limits::Classification {
            class: Class::Session5hLimit,
            unpaired: false,
            reasons: vec!["independent pair: StopFailure + Window".to_string()],
            resets_at: None,
            storm: false,
            no_response: false,
        },
        NOW_S,
        GEN,
        limits::Carry::default(),
    ));
    let st = w.limits_state().expect("state").clone();
    // A RESOLVED directory, which is the case that used to "work".
    w.cfg.account_dir = Some("/Users//x/.claude-alt".to_string());
    w.cfg.account_label = "alt".to_string();
    w.cfg.accounts_enabled = true;
    w.limits_cfg.level = 4;
    let acts = w.acts_for(Action::SwitchAccount, 4, &st, NOW_S);
    let relaunch = acts
        .iter()
        .find(|a| matches!(a, Act::Relaunch { .. }))
        .expect("the rotation is still shaped as a relaunch");
    assert_eq!(relaunch.line(), None, "there is no admitted line to send");
    assert!(
        !acts
            .iter()
            .any(|a| matches!(a.line(), Some(l) if l.contains("CLAUDE_CONFIG_DIR"))),
        "no act may carry an environment the grammar cannot take"
    );
    // The label is still named, so a ledger row can be read back against
    // accounts.toml even though nothing was sent.
    assert!(
        acts.iter()
            .any(|a| matches!(a, Act::Description(d) if d.contains("alt"))),
        "the label is named in the journal"
    );

    // And the PLANNER refuses before any of that, by its own word.
    let host = HostGuards {
        hold: false,
        busy: false,
        custody_user: false,
        generation: w.generation(),
        engaged: true,
    };
    match w.plan_limits(&host, 0, NOW_S) {
        Plan::Refused { why, what } => {
            assert_eq!(why.as_str(), "refused:unsupported");
            assert_eq!(what, "switch-account");
        }
        // NEGATIVE CONTROL for the word itself: `unresolved` is what a
        // roster with no candidate answers, and this roster has one.
        other => panic!("an account rotation planned {other:?}"),
    }
}

/// EVERY act's control line must be a verb the SERVER admits, with keys that
/// verb's own catalog entry names.
///
/// This is the test that was missing. The one that existed asserted the
/// emitted line `contains("CLAUDE_CONFIG_DIR=…")`, i.e. it pinned the
/// INVENTED spelling: nothing anywhere asked whether `spawn env=… cmd=…` was
/// a line the protocol had. The catalog is generated from the same table the
/// server answers `help` from (`aterm_types::control_verbs`), so this cannot
/// drift from what the build actually speaks.
#[test]
fn every_act_line_is_a_verb_the_catalog_admits_with_keys_it_names() {
    use aterm_types::control_verbs::VERBS;

    let id = TurnId {
        nonce: "8186e0fa4920bfd5377f1edf9e763708".to_string(),
        producer: 1,
        seq: 1,
    };
    let acts = vec![
        Act::Icon("⏸".to_string()),
        Act::Description("d".to_string()),
        Act::Notice("n".to_string()),
        Act::Attention("a".to_string()),
        Act::StopBlock {
            reason: "r".to_string(),
        },
        Act::Type {
            id: id.clone(),
            text: "/model opus".to_string(),
        },
        Act::Submit {
            guard: crate::claude_composer_ready_pattern().to_string(),
        },
        Act::Escape {
            guard: ESCAPE_GUARD.to_string(),
        },
        Act::Relaunch {
            label: "alt".to_string(),
            config_dir: "/Users//x/.claude-alt".to_string(),
            resume: None,
            model: None,
        },
        Act::Ask {
            to: "owner".to_string(),
            text: "t".to_string(),
        },
    ];
    let mut lines = 0;
    for act in &acts {
        let Some(line) = act.line() else { continue };
        lines += 1;
        let verb = line.split_whitespace().next().expect("a verb");
        let spec = VERBS
            .iter()
            .find(|v| v.name == verb)
            .unwrap_or_else(|| panic!("`{verb}` is not a verb the server has: {line}"));
        let help = spec.help_line();
        // Leading `k=v` tokens only: the parsers consume those and then take
        // the rest verbatim, so a key the entry does not name is the failure
        // mode `spawn env=` was.
        for tok in line.split_whitespace().skip(1) {
            let Some((k, _)) = tok.split_once('=') else {
                break;
            };
            assert!(
                help.contains(&format!("{k}=")),
                "`{verb}` has no `{k}=` key: {line}\n{help}"
            );
        }
    }
    assert!(lines >= 7, "the sweep must actually have read lines");
    // NEGATIVE CONTROL: the check can fail. The retired spelling is exactly
    // what this test would have caught.
    let dead = "spawn env=CLAUDE_CONFIG_DIR=/x cmd=claude";
    let spec = VERBS.iter().find(|v| v.name == "spawn").expect("spawn");
    assert!(
        !spec.help_line().contains("env="),
        "the catalog must not name an `env=` key: {dead}"
    );
}

#[test]
fn account_rotation_is_off_until_a_config_says_otherwise() {
    // It was hard-coded `false` at the one call site, so the action was dead
    // in production while its table row and its tests read as live.
    let w = watcher();
    assert!(!w.config().accounts_enabled, "OFF is the shipped default");
    let mut on = watcher();
    on.cfg.accounts_enabled = true;
    on.cfg.account_dir = Some("/Users//x/.claude-alt".to_string());
    let guards = on.limit_guards(&host());
    assert!(guards.accounts_enabled, "the field reaches the engine");
    assert!(!watcher().limit_guards(&host()).accounts_enabled);
}

#[test]
fn the_evidence_of_one_generation_is_bounded_and_banners_are_deduplicated() {
    let mut w = watcher();
    // The same banner, redrawn: one piece of evidence, not two hundred.
    for _ in 0..200 {
        w.observed(
            Evidence::Banner {
                text: "Approaching usage limit".to_string(),
                resets_at: None,
            },
            NOW_S,
        );
    }
    assert_eq!(w.evidence.len(), 1, "a redraw is not a new fact");
    // A banner whose FIGURE ticks is a different banner, and unbounded
    // without the cap: this is the shape that grew all session.
    for pct in 0..500 {
        w.observed(
            Evidence::Banner {
                text: format!("Approaching usage limit · {pct}% left"),
                resets_at: None,
            },
            NOW_S,
        );
    }
    assert!(
        w.evidence.len() <= MAX_EVIDENCE,
        "bounded at {MAX_EVIDENCE}, held {}",
        w.evidence.len()
    );
    // A hook value is never the thing evicted to make room for a banner.
    let mut h = watcher();
    h.wake(
        &Wake::Hook(HookEvent::Evidence(sf("rate_limit"))),
        NOW_MS,
        NOW_S,
    );
    for pct in 0..500 {
        h.observed(
            Evidence::Banner {
                text: format!("banner {pct}"),
                resets_at: None,
            },
            NOW_S,
        );
    }
    assert!(
        h.evidence
            .iter()
            .any(|e| matches!(e, Evidence::StopFailure { .. })),
        "the hook value survives a storm of banners"
    );
    assert!(h.evidence.len() <= MAX_EVIDENCE);
}

// -- the nine findings of 2026-09-22 ---------------------------------------------

/// F-1. Every `turn id=` the harness minted was rejected by the shipped
/// server before a byte was typed, because `<epoch>` must be a 32-char
/// launch nonce and the only two spellings this tree ever produced were the
/// word `harness` (the shipped default) and the session id.
#[test]
fn a_turn_id_whose_epoch_is_not_a_launch_nonce_refuses_before_anything_is_typed() {
    // The three spellings, measured against the SERVER's own reader.
    assert!(!valid_turn_nonce("harness"), "the shipped default");
    assert!(!valid_turn_nonce("s-1e918c4662a1b7b8bd43"), "a session id");
    assert!(!valid_turn_nonce(""), "an unfilled field");
    assert!(
        !valid_turn_nonce(&NONCE[..31]),
        "31 characters is not a nonce"
    );
    assert!(!valid_turn_nonce(&format!("{NONCE}0")), "33 is not either");
    assert!(
        !valid_turn_nonce("zz86e0fa4920bfd5377f1edf9e763708"),
        "not hex"
    );
    assert!(valid_turn_nonce(NONCE), "a real roster nonce");

    // The default is the honest absence, not a word that cannot parse.
    assert_eq!(WatchConfig::default().nonce, "");

    // And the plan REFUSES rather than sending a line that dies at parse.
    let bad = WatchConfig {
        nonce: "s-1e918c4662a1b7b8bd43".to_string(),
        ..cfg()
    };
    let mut w = Watcher::new("s-1", bad, LimitsConfig::default());
    w.wake(&Wake::Generation(GEN), NOW_MS, NOW_S);
    for e in [sf("overloaded"), sf("overloaded"), sf("overloaded"), pms()] {
        w.wake(&Wake::Hook(HookEvent::Evidence(e)), NOW_MS, NOW_S);
    }
    let p0 = w.plan(&host(), NOW_MS, NOW_S);
    let step0 = act_of(&p0).expect("step 0").clone();
    // The L1 display-only step types nothing, so it is NOT fenced away.
    assert_eq!(step0.limit_action, Some(Action::LetVendorRetry));
    w.commit(&step0, 1, NOW_MS, NOW_S);
    let p = w.plan(&host(), NOW_MS + 300_000, NOW_S + 300);
    assert!(
        matches!(
            p,
            Plan::Refused {
                why: Refuse::Unresolved,
                ..
            }
        ),
        "{p:?}"
    );

    // NEGATIVE CONTROL: the same watcher with a real nonce acts.
    let mut ok = watcher();
    for e in [sf("overloaded"), sf("overloaded"), sf("overloaded"), pms()] {
        ok.wake(&Wake::Hook(HookEvent::Evidence(e)), NOW_MS, NOW_S);
    }
    let s0 = act_of(&ok.plan(&host(), NOW_MS, NOW_S))
        .expect("step 0")
        .clone();
    ok.commit(&s0, 1, NOW_MS, NOW_S);
    let p = ok.plan(&host(), NOW_MS + 300_000, NOW_S + 300);
    assert_eq!(
        act_of(&p).expect("the switch").limit_action,
        Some(Action::SwitchModel)
    );
}

/// The watcher the F-2/F-3/F-4 probes drive: a storm, committed through step
/// 0, parked at the `switch-model` row.
fn at_switch_model() -> (Watcher, Vec<(u64, Wake)>) {
    let mut w = watcher();
    for e in [sf("overloaded"), sf("overloaded"), sf("overloaded"), pms()] {
        w.wake(&Wake::Hook(HookEvent::Evidence(e)), NOW_MS, NOW_S);
    }
    let s0 = act_of(&w.plan(&host(), NOW_MS, NOW_S))
        .expect("step 0")
        .clone();
    w.commit(&s0, 1, NOW_MS, NOW_S);
    (w, vec![(300_000, Wake::Deadline(Timer::Schedule))])
}

/// F-2. `OK skipped` is the GUARD saying no. It is a success reply and a
/// refused act, and recording it as `executed` left the composer holding
/// `/model opus` with no compensating clear — the concatenation §4.3's
/// atomicity comment names.
#[test]
fn a_guard_that_answered_ok_skipped_is_a_refusal_and_never_an_executed_act() {
    let (mut w, script) = at_switch_model();
    let mut wire = FakeWire::new(script);
    wire.reply
        .push(("key if=".to_string(), "OK skipped".to_string()));
    let pass = w.pump(&mut wire);
    assert_eq!(
        pass.verdict.as_deref(),
        Some("refused:skipped"),
        "sent={:?}",
        wire.sent
    );
    // The turn went out; the Enter did not land.
    assert!(
        wire.sent.iter().any(|l| l.starts_with("turn id=")),
        "{:?}",
        wire.sent
    );
    // And because the Enter did not land, the composer is UNWOUND.
    assert!(
        wire.sent.iter().any(|l| l.contains("ctrl+u")),
        "a half-landed fence is always cleared: {:?}",
        wire.sent
    );
    assert!(
        wire.rows
            .iter()
            .any(|(_, r)| r.contains("\"verdict\":\"refused:skipped\"")),
        "{:?}",
        wire.rows
    );

    // The SECOND half: a clear that itself answered `OK skipped` cleared
    // nothing, and must not be journalled `composer-cleared`.
    let (mut w2, script2) = at_switch_model();
    let mut wire2 = FakeWire::new(script2);
    wire2
        .refuse
        .push(("key if=".to_string(), "ERR halted".to_string()));
    wire2
        .reply
        .push(("key if=".to_string(), "OK skipped".to_string()));
    let _ = w2.pump(&mut wire2);
    assert!(
        wire2.rows.iter().any(|(_, r)| r.contains("composer-dirty")),
        "{:?}",
        wire2.rows
    );
    assert!(
        !wire2
            .rows
            .iter()
            .any(|(_, r)| r.contains("composer-cleared")),
        "{:?}",
        wire2.rows
    );

    // NEGATIVE CONTROL: a plain `OK` on both sends is still `executed`.
    let (mut w3, script3) = at_switch_model();
    let mut wire3 = FakeWire::new(script3);
    let pass3 = w3.pump(&mut wire3);
    assert_eq!(pass3.verdict.as_deref(), Some("executed"));
}

/// F-3. `OK … dup=1` means the key was a REPLAY: nothing was typed this
/// pass, so the fenced Enter must not follow — it would land on whatever
/// composer row is on screen, including a person's own unsent draft.
#[test]
fn a_duplicate_turn_typed_nothing_so_the_fenced_enter_never_follows() {
    let (mut w, script) = at_switch_model();
    let mut wire = FakeWire::new(script);
    wire.reply
        .push(("turn id=".to_string(), "OK 0 dup=1".to_string()));
    let pass = w.pump(&mut wire);
    assert_eq!(pass.verdict.as_deref(), Some("refused:duplicate"));
    assert!(
        !wire.sent.iter().any(|l| l.starts_with("key if=")),
        "no press of any kind follows a replayed turn: {:?}",
        wire.sent
    );

    // NEGATIVE CONTROL: the same reply on a line that typed nothing is not a
    // duplicate verdict — only an `Act::Type` can be a replayed typing.
    assert!(reply_duplicate("OK 0 dup=1"));
    assert!(reply_duplicate("OK dup=1"));
    assert!(!reply_duplicate("OK 0 dup=0"));
    assert!(!reply_duplicate("OK 3 rows: dup=1x"));
    assert!(reply_skipped("OK skipped"));
    assert!(reply_skipped("OK skipped (no match)"));
    assert!(!reply_skipped("OK skippedy"));
    assert!(!reply_skipped("OK"));
}

/// F-4. The lease is RELEASED by name — a bare `lease release` is refused by
/// the shipped `lease_release`, so the lease survived its whole TTL and held
/// every other cooperative driver out — and a REFUSED acquire stops the pass
/// before the first act.
#[test]
fn the_lease_is_released_by_holder_and_a_refused_acquire_acts_on_nothing() {
    let (mut w, script) = at_switch_model();
    let mut wire = FakeWire::new(script);
    let _ = w.pump(&mut wire);
    assert!(
        wire.sent
            .iter()
            .any(|l| l == &format!("lease release holder={LEASE_HOLDER}")),
        "{:?}",
        wire.sent
    );
    assert!(
        !wire.sent.iter().any(|l| l == "lease release"),
        "a bare release answers `ERR lease held by …`: {:?}",
        wire.sent
    );

    // A refused ACQUIRE: nothing is typed, and the row blames the lease.
    let (mut w2, script2) = at_switch_model();
    let mut wire2 = FakeWire::new(script2);
    wire2.refuse.push((
        "lease acquire".to_string(),
        "ERR lease held by other:driver".to_string(),
    ));
    let pass = w2.pump(&mut wire2);
    assert_eq!(pass.verdict.as_deref(), Some("refused:busy"));
    assert!(
        !wire2.sent.iter().any(|l| l.starts_with("turn id=")),
        "a refused lease types nothing: {:?}",
        wire2.sent
    );
}

/// F-5. Four stated refusals were spelled `timeout`, whose documented
/// meaning is "the act went out and did not come back"; and any free-form
/// line's first word was read as a refusal word.
#[test]
fn a_stated_refusal_is_spelled_by_its_own_word_and_free_text_states_none() {
    assert_eq!(
        refusal_of("ERR usage: id=<epoch>:<producer>:<seq>"),
        Some(Refuse::Unresolved)
    );
    assert_eq!(refusal_of("ERR epoch mismatch"), Some(Refuse::Generation));
    assert_eq!(
        refusal_of("ERR lease held by harness:claude-harness"),
        Some(Refuse::Busy)
    );
    assert_eq!(
        refusal_of("ERR write failed partial accepted=3"),
        Some(Refuse::Transport)
    );
    assert_eq!(refusal_of("ERR halted reason=x"), Some(Refuse::Hold));
    assert_eq!(refusal_of("ERR busy turn=7"), Some(Refuse::Busy));
    assert_eq!(
        refusal_of("ERR rate (self-feed floor)"),
        Some(Refuse::Budget)
    );
    // NEGATIVE CONTROLS: no `ERR` word, no stated refusal.
    assert_eq!(refusal_of("rate limited by the vendor"), None);
    assert_eq!(refusal_of("busy doing something else"), None);
    assert_eq!(refusal_of(""), None);
}

/// Cruft F8/F9. The two line-framing promises this module makes are
/// absolute, and `char::is_control()` is Cc-only: `U+2028`/`U+2029` walked
/// straight through the old hand-rolled sweeps. And the ledger's escaper
/// EMITS control characters rather than erasing them.
#[test]
fn the_line_fold_catches_the_separators_and_the_ledger_row_keeps_what_it_quotes() {
    let folded = Act::Notice("rm policy\u{2028}forged".to_string())
        .line()
        .expect("a line");
    assert!(
        !folded.contains('\u{2028}') && !folded.contains('\u{2029}'),
        "{folded:?}"
    );
    assert!(!folded.contains('\n'), "{folded:?}");
    // The ledger is byte-faithful: a tab is ESCAPED, never destroyed.
    let row = verdict_row(1, 0, 0, "ok", "stalled at\tstep 2");
    assert!(row.contains("stalled at\\tstep 2"), "{row}");
    assert!(!row.contains('\t'), "{row}");
    // …and it is still exactly one line.
    assert_eq!(row.lines().count(), 1, "{row}");
}

// -- the ingress predicate, the arm floor and the fences -------------------------

/// THE INGRESS PREDICATE HAS A SOURCE.
///
/// `next_arm` parks on `await seq <last_seq>`, and `last_seq` had exactly one
/// writer: `Wake::Frame(Frame::Delta)`. The subscribed stream is `events`
/// (`wire::STREAMS`), which carries `EVENT`/`GAP` and no `DELTA` — `DELTA`
/// belongs to `screen|cells|bytes` — so against a real wire `last_seq` was
/// permanently `None` and every arm was `await seq 0`. MEASURED against a
/// live instance: `await seq 0 timeout=2000` answers `OK seq 441367` in
/// 10 ms. A predicate that is already true is not a park: no deadline could
/// ever fire, `stall_timer_fired` was never set, the whole liveness ladder
/// was unreachable in production, and `pump` became a hot loop.
///
/// `status` carries `seq=` and the loop reads `status` every pass.
#[test]
fn the_arm_parks_on_the_sequence_the_spine_read_and_never_on_zero() {
    let mut w = watcher();
    let seq = w.next_arm(NOW_MS, NOW_S).expect("an arm").cond[1].clone();
    // BEFORE any spine read there is nothing to name, and the loop says zero
    // — which is why the fold below is the whole fix.
    assert_eq!(seq, "0", "no sample has been folded yet");

    let mut wire = FakeWire::new(vec![(
        0,
        Wake::Frame(Frame::Event {
            kind: "turn".into(),
        }),
    )]);
    wire.status = status_line("running", 9, 1_000, false);
    let pass = w.pump(&mut wire);
    assert!(matches!(pass.wake, Wake::Frame(_)));
    let arm = w.next_arm(w.live.last_change_ms, NOW_S).expect("an arm");
    assert_eq!(arm.cond[0], "seq");
    assert_eq!(
        arm.cond[1], "93",
        "the arm names the sequence `status` reported"
    );
    // NEGATIVE CONTROL — the exact shape that silently disabled the ladder:
    // an arm whose condition is `seq 0` while the session's own sequence is
    // not zero is a predicate already true, i.e. no wait at all.
    assert_ne!(
        arm.cond[1], "0",
        "`await seq 0` against a live session returns at once; it is a spin, not a park"
    );
    assert!(arm.line().starts_with("await seq 93 timeout="), "{arm:?}");
}

/// A DEADLINE IN THE PAST IS NOT A DEADLINE.
///
/// `next_arm` clamped every computed duration into `1..=AWAIT_CLAMP_MS`, so a
/// moment that had already passed armed `timeout=1`. `warned_at` is cleared
/// only by `end_episode`, which a HOOKLESS session never reaches, so from
/// `warned_at + warn_ms` onward the loop ran ~1000 passes a second — each a
/// `status`, a `who`, a `custody` and a fresh socket — for ever. The existing
/// no-timer test checked only that the arm was an `await` and at most the
/// clamp; nothing pinned a LOWER bound, which is why this survived.
#[test]
fn a_warn_deadline_that_has_passed_arms_no_one_millisecond_spin() {
    let mut w = watcher();
    w.live.warned_at = Some(NOW_MS);
    w.live.rung = Some(Rung::Warn);

    // POSITIVE CONTROL: one millisecond BEFORE the moment, the warn deadline
    // is real and is what the loop parks on.
    let just_before = NOW_MS + w.cfg.warn_ms - 1;
    let arm = w.next_arm(just_before, NOW_S).expect("an arm");
    assert_eq!(arm.timer, Timer::Warn);
    assert_eq!(
        arm.timeout_ms, ARM_FLOOR_MS,
        "floored, never one millisecond"
    );

    // The moment itself, and long past it: the warn arm is not offered, so
    // the loop falls through to the stall window rather than spinning.
    for now in [NOW_MS + w.cfg.warn_ms, NOW_MS + w.cfg.warn_ms * 100] {
        let arm = w.next_arm(now, NOW_S).expect("an arm");
        assert_ne!(arm.timer, Timer::Warn, "a passed moment is not a deadline");
        assert!(
            arm.timeout_ms >= ARM_FLOOR_MS,
            "no arm may be shorter than the floor: {arm:?}"
        );
    }
    // And a badge already taken owes nothing at all.
    w.live.badged = true;
    let arm = w.next_arm(just_before, NOW_S).expect("an arm");
    assert_ne!(arm.timer, Timer::Warn);

    // NO SWITCH-BACK ARM EVER. §5.8.5's switch-back is recorded and never
    // acted on, and arming a deadline for an act that cannot happen was the
    // second copy of the same spin: `switch_back_at` is cleared only by
    // `adopt_switch`, which has no production caller.
    let mut w = watcher();
    w.switched_model = true;
    w.switch_back_at = Some(NOW_S - 10_000);
    let arm = w.next_arm(NOW_MS, NOW_S).expect("an arm");
    assert_ne!(arm.timer, Timer::SwitchBack, "{arm:?}");
    assert!(arm.timeout_ms >= ARM_FLOOR_MS, "{arm:?}");
}

/// EVERY planner owes the approval fence, not two of the three.
///
/// It lived in `plan_limits` and `switch` only; the automatic ladder
/// inherited it by accident, through `classify_liveness`'s `waiting-human`
/// short-circuit feeding `plan_ladder`'s `is_stall()` test. `Watcher::nudge`
/// computed a verdict and threw it away, so a hand-asked `--level turn`
/// reached an L3 `turn` with an approval box on the grid — while its own
/// docstring promised a manual rung could reach nothing an automatic one
/// could.
#[test]
fn a_hand_asked_rung_refuses_at_an_approval_box_and_at_every_explained_verdict() {
    let mut w = watcher();
    w.cfg.level = Level::Escape;
    // A REAL approval box, from the shipped fixtures, through the shipped
    // reader — not a hand-typed row.
    let rows = crate::supervise::prompt::fixtures::bash_one_row();
    let mut wire = FakeWire::new(vec![(
        0,
        Wake::Frame(Frame::Event {
            kind: "turn".into(),
        }),
    )]);
    wire.status = status_line("running", 11, 1_000, false);
    wire.grid = Some(grid_json(&rows, 113));
    w.pump(&mut wire);
    assert!(w.prompt, "the spine read the box");

    for rung in [Rung::Warn, Rung::Nudge, Rung::Escape] {
        match w.nudge(rung, &host(), NOW_MS) {
            Plan::Refused { why, .. } => assert_eq!(
                why.as_str(),
                "refused:waiting-human",
                "{rung:?} must refuse at an approval box"
            ),
            other => panic!("{rung:?} planned {other:?} with an approval box up"),
        }
    }
    // The AUTOMATIC planner refuses the same rung the same way, which is the
    // equality the docstring claims.
    assert!(matches!(w.plan(&host(), NOW_MS, NOW_S), Plan::Idle));

    // A GAP is the other explained verdict: the window was UNOBSERVED, not
    // quiet, and a rung must not be taken on it either.
    let mut w = watcher();
    w.cfg.level = Level::Escape;
    w.wake(
        &Wake::Frame(Frame::Gap {
            bytes_dropped: 64,
            resync: None,
            events_resync: None,
        }),
        NOW_MS,
        NOW_S,
    );
    match w.nudge(Rung::Nudge, &host(), NOW_MS) {
        Plan::Refused { what, .. } => assert!(what.contains("unobserved"), "{what}"),
        other => panic!("a rung was planned over a GAP: {other:?}"),
    }

    // NEGATIVE CONTROL: `working` is the ABSENCE of a symptom, not a reason,
    // so a hand-asked rung still plans — the operator may assert a symptom
    // the loop has not seen. The fence above is about the states that name
    // something else owning the session.
    let w = watcher();
    assert!(matches!(w.nudge(Rung::Warn, &host(), NOW_MS), Plan::Act(_)));
}

/// A typed act's TEXT is typed, never parsed as options.
///
/// The `turn` parser consumes leading `k=v` tokens and breaks at the first
/// token that is not a known option, and it accepts `--` as an explicit
/// sentinel (the shipped caller in `aterm-gui/src/control.rs` uses it). Our
/// line did not, so a text beginning with `timeout=`, `idle=`, `cadence=`,
/// `yield=`, `presses=`, `trim=` or `typed=` would have been eaten as an
/// option — silently not typed, and changing the turn's own timing on the
/// way.
#[test]
fn a_typed_act_carries_the_option_sentinel_so_its_text_is_never_an_option() {
    let id = TurnId {
        nonce: NONCE.to_string(),
        producer: 7,
        seq: 1,
    };
    for text in ["/model opus", "timeout=1 is not an option", "yield=9"] {
        let line = Act::Type {
            id: id.clone(),
            text: text.to_string(),
        }
        .line()
        .expect("a typed act has a line");
        let (opts, body) = line.split_once(" -- ").expect("the sentinel is present");
        assert_eq!(body, text, "the text is verbatim after the sentinel");
        // NEGATIVE CONTROL: every token before the sentinel really is one of
        // the turn parser's own options, so the sentinel is the only thing
        // separating them from the message.
        for tok in opts.split_whitespace().skip(1) {
            let (k, _) = tok.split_once('=').expect("a leading option");
            assert!(
                [
                    "id",
                    "presses",
                    "settle",
                    "submit_window",
                    "yield",
                    "submit"
                ]
                .contains(&k),
                "{k} is not a turn option: {line}"
            );
        }
    }
}
