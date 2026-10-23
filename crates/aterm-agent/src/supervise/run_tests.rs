// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::phase::{has_composer_frame, survey_open, worker_phase};
use super::super::prompt::fixtures::{
    bash_multi_row, bash_multi_row_with_note, bash_one_row, composer, read_box, rows, trust_box,
    workflow_box,
};
use super::*;
use crate::supervise::fx::ClockSource;
use crate::supervise::tape::ctl::TapedCtl;
use crate::supervise::tape::testing::MemoryTape;
use std::collections::{BTreeMap, VecDeque};
use std::sync::mpsc;

fn session<C: Ctl>(ctl: &mut C, sid: Option<String>) -> Session<'_, C> {
    let mut s = Session::new(ctl, sid);
    s.set_timings(crate::harness::Timings::FAST);
    s.set_box_settle(Duration::ZERO);
    s.set_box_call_step(Duration::ZERO);
    s.set_survey_settle(Duration::ZERO);
    s.set_survey_land(Duration::ZERO);
    s
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Digit {
    None,
    Pending { at: Instant, take: bool },
    Standing,
    Taken,
}

#[derive(Debug, Clone)]
struct KeyProbe {
    request: String,
    card_age: Option<Duration>,
    zero: bool,
    draft: bool,
    boxed: bool,
    digit: Digit,
}

fn ticked_countdown(rows: &[String], ticks: u64) -> (Vec<String>, Option<u64>) {
    let mut out = rows.to_vec();
    let Some(at) = out
        .iter()
        .rposition(|r| r.contains("deny this request in "))
    else {
        return (out, None);
    };
    let Some((head, tail)) = out[at].split_once("deny this request in ") else {
        return (out, None);
    };
    let Some((m, s)) = tail
        .split_once(',')
        .and_then(|(time, _)| time.split_once(':'))
    else {
        return (out, None);
    };
    let (Ok(m), Ok(s)) = (m.parse::<u64>(), s.parse::<u64>()) else {
        return (out, None);
    };
    let left = (m * 60 + s).saturating_sub(ticks);
    let rest = tail.split_once(',').map_or("", |(_, r)| r);
    out[at] = format!(
        "{head}deny this request in {}:{:02},{rest}",
        left / 60,
        left % 60
    );
    (out, Some(left))
}

#[derive(Debug, Clone, Copy)]
struct ServerLaunch {
    id: u64,
    seq: u64,
    word: &'static str,
    why: &'static str,
}

impl ServerLaunch {
    fn why_at(self, count: Option<u64>) -> &'static str {
        match self.why {
            "person" if count != Some(self.seq) => "since",
            why => why,
        }
    }

    fn json(self, count: Option<u64>) -> String {
        match self.why_at(count) {
            "none" => "null".to_string(),
            "person" => format!(
                "{{\"id\":{},\"seq\":{},\"word\":\"{}\",\"why\":\"person\"}}",
                self.id, self.seq, self.word
            ),
            why => format!("{{\"why\":\"{why}\"}}"),
        }
    }

    fn status(self, count: Option<u64>) -> String {
        match self.why_at(count) {
            "none" => "launch=none launch_seq=-".to_string(),
            why => format!("launch={why} launch_seq={}", self.seq),
        }
    }
}

struct Mock {
    requests: Vec<String>,
    replies: VecDeque<CtlReply>,
    key_replies: VecDeque<CtlReply>,
    screens: Vec<Vec<String>>,
    served: usize,
    seq: u64,
    vanish_after: Option<u32>,
    exit_when: Option<fn(&Mock) -> bool>,
    exit_at_request: Option<usize>,
    end_when_parked: bool,
    waits: Option<Arc<super::super::fx::ScriptedClock>>,
    hang_at: Option<Instant>,
    round_trip: Duration,
    after_call: Option<Box<dyn FnMut() + Send>>,
    stalls: u32,
    idle_skips_busy: bool,
    by_index: BTreeMap<usize, CtlReply>,
    down: Option<(usize, CtlReply)>,
    drop_awaits_from: Option<usize>,
    handoff_seq: Option<u64>,
    restart: Option<(usize, u64)>,
    delay: BTreeMap<usize, Duration>,
    history: Option<CtlReply>,
    offscreen: VecDeque<CtlReply>,
    verb_replies: BTreeMap<&'static str, VecDeque<CtlReply>>,
    turn_releases: Option<usize>,
    key_releases: Option<usize>,
    keyed: bool,
    keys: usize,
    key_gates: Vec<(usize, usize)>,
    turn_gates: Vec<usize>,
    attention_gates: Vec<(usize, usize)>,
    attentions: usize,
    attention_kind: Option<String>,
    screen_cols: BTreeMap<usize, usize>,
    cursor_on_caret: bool,
    /// The first this many `text` reads find the cursor off the caret (row 10), later ones on
    /// it; the move bumps no content seq.
    caret_after_reads: Option<usize>,
    /// A screen that sits ([`Self::held`]) is read, from this many `text` reads on, as the
    /// script's next screen at a new seq while every wait on it still sat.
    redrawn_after_reads: Option<usize>,
    screen_cursors: BTreeMap<usize, usize>,
    pane_cols: Option<usize>,
    whole_sees_the_tail: bool,
    last_read_tail: bool,
    turned: bool,
    turns: usize,
    stall_sleep: Option<Duration>,
    dead_after_turns: Option<usize>,
    title: Option<String>,
    /// The titles the next reads report, one each; the last stands after them.
    titles: VecDeque<Option<String>>,
    /// A title pinned over the program's: `status`'s `subject=`; `title` still the program's.
    pinned: Option<String>,
    /// `title` refused, as by a host without the verb.
    title_refused: bool,
    /// For the next `await idle` waits, one each: how long until its screen holds still, the
    /// wait latching then or timing out at its step, whichever comes first.
    idle_after: VecDeque<Duration>,
    cursor_col: Option<usize>,
    attention: Option<String>,
    foreign_attention: Option<String>,
    cwd: Option<String>,
    pub agent_cwd: Option<String>,
    pub trusted_cwd: Option<String>,
    help: String,
    help_send: Option<String>,
    send_replies: VecDeque<CtlReply>,
    restart_replies: VecDeque<CtlReply>,
    hold: &'static str,
    fabric: &'static str,
    hold_lifts_after: Option<usize>,
    status_reads: usize,
    swap_at_key: Option<Vec<String>>,
    swap_after_keys: usize,
    gen_fence: bool,
    sends_gen: bool,
    human: Vec<&'static str>,
    human_on: BTreeMap<usize, &'static str>,
    reads: Vec<(usize, &'static str)>,
    uncounted: usize,
    idle_never: bool,
    tick_ms: Option<u64>,
    redraws_move_gen: bool,
    countdown_ticks: bool,
    countdown_reads: BTreeMap<usize, u64>,
    countdown_left: Option<u64>,
    stall_after_read: BTreeMap<usize, Duration>,
    tick_at_key: usize,
    ticks: Option<Duration>,
    read_at: Option<Instant>,
    program: &'static str,
    program_resolves_to: Option<&'static str>,
    agent: &'static str,
    claims: Vec<String>,
    claim_replies: VecDeque<CtlReply>,
    order: Vec<(String, bool)>,
    human_attention: Option<String>,
    human_on_top: bool,
    needs_attention: Option<String>,
    needs_calls: Vec<String>,
    needs_replies: VecDeque<CtlReply>,
    card_ignores_within: Option<Duration>,
    card_first_served: Option<Instant>,
    early_zeroes: usize,
    card_debounce: Option<Duration>,
    digit: Digit,
    reactive_at: Option<usize>,
    key_probes: Vec<KeyProbe>,
    card_drops_digits: bool,
    backspace_replies: VecDeque<CtlReply>,
    human_ms: Option<u64>,
    hand: &'static str,
    hand_from: BTreeMap<usize, &'static str>,
    gen_skipped: usize,
    seq_waits_run_out: usize,
    hand_during_idle: Option<(&'static str, &'static str)>,
    human_seq: Option<u64>,
    human_seq_from: BTreeMap<usize, u64>,
    human_seq_on: BTreeMap<usize, Option<u64>>,
    submits_on: BTreeMap<usize, u64>,
    open_on: std::collections::BTreeSet<usize>,
    inputs_on: BTreeMap<usize, (Option<u64>, u64)>,
    launch: Option<ServerLaunch>,
    person_at_key: usize,
    person_at_send: usize,
    person_at_status: Option<usize>,
    person_after_send: usize,
    person_skipped: Vec<usize>,
    cell_attrs: &'static str,
    /// The attributes a `cell` read answers while the script's screen `i` is the one last
    /// served, in place of [`Self::cell_attrs`].
    cell_attrs_on: BTreeMap<usize, &'static str>,
    cell_fg: &'static str,
    cell_unreadable: bool,
    cells: BTreeMap<(usize, usize), String>,
    history_cells: BTreeMap<(usize, usize), String>,
    human_ms_reads: VecDeque<Option<u64>>,
    status_human_from: BTreeMap<usize, u64>,
    agent_pushes: bool,
    questions: Option<&'static str>,
    questions_after_badge: Option<Option<&'static str>>,
    questions_at: Vec<(usize, Option<&'static str>)>,
    autopilot: Option<&'static str>,
    autopilot_at: Vec<(usize, Option<&'static str>)>,
    autopilot_on: BTreeMap<usize, Option<&'static str>>,
    metas: usize,
    input: &'static str,
    input_after: Vec<(usize, &'static str)>,
    /// When, on the run's clock ([`run_now`]), the last `status` said the worker's input was
    /// unread (`input=pending|typeahead`).
    input_unread_at: Option<Instant>,
    /// When, on the run's clock, the first `send` arrived.
    first_send_at: Option<Instant>,
    /// Every `text` read, on the run's clock: the script's screen it served and when.
    read_times: Vec<(usize, Instant)>,
    /// Every `key` request, on the run's clock: the request and when it arrived.
    key_times: Vec<(String, Instant)>,
    /// When, on the run's clock, the supervisor's attention was first set.
    attention_at: Option<Instant>,
    /// An `await idle <ms>` of this many ms never latches: it runs its whole timeout on the
    /// run's clock and times out (a screen that never holds still that long).
    idle_never_for: Option<&'static str>,
    /// For this long after the first `send` on the run's clock, the screen does not move: every
    /// read serves the screen the write found at its seq, and an `await seq` sits until then.
    unmoved_after_send: Option<Duration>,
    /// How many reads after the first `send` move on as scripted before the screen stands
    /// ([`Self::unmoved_after_send`]): a frame drawn after the write that does not show it.
    unmoved_from_read: usize,
    /// How many `text` reads had been served when the first `send` arrived.
    reads_at_send: Option<usize>,
    /// While the screen stands unmoved ([`Self::unmoved_after_send`]) once the supervisor's
    /// attention is set, this many `await seq` waits wake at once with nothing drawn: the loop
    /// reads the screen again at the same seq.
    unmoved_wakes: u32,
    /// While the screen stands unmoved once the supervisor's attention is set, its row `.0`
    /// reads as `.1` at the same content seq and generation: a frame the loop keys apart from
    /// the point it handed over ([`point_key`]), the composer and every fence the same.
    unmoved_rekeyed: Option<(usize, &'static str)>,
    /// Every `lease` request, on the run's clock: the request and when it arrived.
    lease_times: Vec<(String, Instant)>,
    /// A settle that latches says a seq one before the read after it (`OK idle <seq>`): the
    /// screen moved between the latch and the read.
    idle_latch_behind: bool,
    /// The generation `(epoch, seq)` a read of the script's screen `i` reports, its `"seq"`
    /// and `"gen"`, in place of `1.<seq>` at the mock's seq: a grid entered since (an
    /// alternate screen, a reset), whose own seq restarted.
    screen_gens: BTreeMap<usize, (u64, u64)>,
    cursor_row: Option<usize>,
    tail_replies: VecDeque<String>,
    agent_zone: Option<usize>,
    status_sid: &'static str,
    who: Option<String>,
    aimed: Vec<String>,
    gone_on_next: bool,
    gone_held: usize,
    gone_ran_out: bool,
    after_gone: Vec<bool>,
    alt_screen: Option<bool>,
    scrollback: Option<Vec<String>>,
    scrollback_from: Option<usize>,
    evicted_below: u64,
    alt_archive: Option<Vec<String>>,
    archive_gap: bool,
    unreadable_row: Option<u64>,
    search_qualifier: &'static str,
    panel: Option<PanelWorld>,
    reset: Option<ResetWorld>,
    task: Option<(u64, String)>,
    task_requests: Vec<String>,
    task_reopens_at: Option<usize>,
    task_busy: usize,
    task_lost: usize,
    hang_up_at: Option<&'static str>,
    hung_up: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PanelAt {
    Rest,
    Typed(String),
    Open,
    Over,
}

#[derive(Debug, Clone)]
struct PanelProbe {
    request: String,
    pan: i64,
    hand: bool,
    hold: bool,
    turn: bool,
    owed: bool,
    claims: usize,
    tolds: usize,
    at: Instant,
}

#[derive(Debug, Default)]
struct PanelShared {
    pan: std::sync::atomic::AtomicI64,
    hand: AtomicBool,
    claims: std::sync::atomic::AtomicUsize,
    tolds: std::sync::atomic::AtomicUsize,
}

#[derive(Debug)]
struct PanelWorld {
    frames: Vec<Vec<String>>,
    at: PanelAt,
    rest: Option<Vec<String>>,
    opens: bool,
    turn_instead: Option<Vec<Vec<String>>>,
    esc_fails: usize,
    esc_replies: VecDeque<CtlReply>,
    leaves_after: Option<usize>,
    person_after: Option<usize>,
    lose_read: Option<usize>,
    stop_at_read: Option<(usize, Arc<AtomicBool>)>,
    repaints: bool,
    after: Option<Vec<String>>,
    torn: Option<Vec<String>>,
    mends: Option<(bool, Vec<String>)>,
    then: VecDeque<Vec<Vec<String>>>,
    reads: usize,
    keyed_at: Option<Instant>,
    grace: Duration,
    quiet: bool,
    menu: bool,
    probes: Vec<PanelProbe>,
    shared: Arc<PanelShared>,
}

impl PanelWorld {
    fn new(frames: Vec<Vec<String>>, shared: &Arc<PanelShared>) -> Self {
        Self {
            frames,
            at: PanelAt::Rest,
            rest: None,
            opens: true,
            turn_instead: None,
            esc_fails: 0,
            esc_replies: VecDeque::new(),
            leaves_after: None,
            person_after: None,
            lose_read: None,
            stop_at_read: None,
            repaints: false,
            after: None,
            torn: None,
            mends: None,
            then: VecDeque::new(),
            reads: 0,
            keyed_at: None,
            grace: Duration::from_secs(120),
            quiet: false,
            menu: false,
            probes: Vec::new(),
            shared: Arc::clone(shared),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResetAt {
    Dialog,
    Checking,
    Confirm,
    Resetting,
    Gone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResetExit {
    Told,
    Refused,
    Asked,
    Statuses(usize),
    Spent,
}

#[derive(Debug, Clone)]
struct ResetProbe {
    request: String,
    key: String,
    at: ResetAt,
    dlg: i64,
    guard: bool,
    focus: bool,
    hand: bool,
    hold: bool,
    claim: i64,
    ph: i64,
    own: bool,
    row: Option<String>,
    on_grant: bool,
    written: bool,
}

#[derive(Debug, Default)]
struct ResetShared {
    claim: std::sync::atomic::AtomicI64,
    ph: std::sync::atomic::AtomicI64,
    own: AtomicBool,
    dlg: std::sync::atomic::AtomicI64,
    hand: AtomicBool,
    hold: AtomicBool,
}

#[derive(Debug)]
struct ResetWorld {
    at: ResetAt,
    labels: Vec<String>,
    grant: String,
    focus: usize,
    check_reads: usize,
    claim_reads: usize,
    confirm: Vec<String>,
    exact: bool,
    guard: Duration,
    mounted: Option<Instant>,
    refused: Vec<String>,
    enter_drops: usize,
    esc_drops: usize,
    cursor_moves_at_esc: bool,
    after: Vec<String>,
    person_at_read: Option<usize>,
    person_at_confirm_key: usize,
    person_at_row_key: usize,
    drawn_at_arrow: Option<(usize, String)>,
    yes_by_person_after: Option<Duration>,
    resetting: Vec<String>,
    lose_reply_to: Option<(ResetAt, &'static str)>,
    reads: usize,
    gone_read: bool,
    keyed_at: Option<Instant>,
    grace: Duration,
    chosen: Vec<String>,
    yes_taken: usize,
    held_waits: usize,
    exits_when: Option<ResetExit>,
    untold: bool,
    probes: Vec<ResetProbe>,
    shared: Arc<ResetShared>,
}

impl ResetWorld {
    fn new(labels: &[&str], focus: usize, shared: &Arc<ResetShared>) -> Self {
        use aterm_phase::prompt::fixtures::screen;
        Self {
            at: ResetAt::Dialog,
            labels: labels.iter().map(|l| (*l).to_string()).collect(),
            grant: RESET_GRANT.to_string(),
            focus,
            check_reads: 1,
            claim_reads: 1,
            confirm: screen(aterm_phase::reset::fixtures::CONFIRM),
            exact: true,
            guard: Duration::ZERO,
            mounted: None,
            refused: Vec::new(),
            enter_drops: 0,
            esc_drops: 0,
            cursor_moves_at_esc: false,
            after: idle_screen(),
            person_at_read: None,
            person_at_confirm_key: 0,
            person_at_row_key: 0,
            drawn_at_arrow: None,
            yes_by_person_after: None,
            resetting: screen(aterm_phase::reset::fixtures::RESETTING),
            lose_reply_to: None,
            reads: 0,
            gone_read: false,
            keyed_at: None,
            grace: Duration::from_secs(120),
            chosen: Vec::new(),
            yes_taken: 0,
            held_waits: 0,
            exits_when: None,
            untold: false,
            probes: Vec::new(),
            shared: Arc::clone(shared),
        }
    }

    fn dialog(&self) -> Vec<String> {
        let labels: Vec<&str> = self.labels.iter().map(String::as_str).collect();
        aterm_phase::prompt::fixtures::limit_options_panel(&labels, self.focus, true)
    }

    fn frame(&self) -> Vec<String> {
        use aterm_phase::prompt::fixtures::screen;
        use aterm_phase::reset::fixtures as f;
        match self.at {
            ResetAt::Dialog => self.dialog(),
            ResetAt::Checking => screen(f::CHECKING),
            ResetAt::Confirm => self.confirm.clone(),
            ResetAt::Resetting => self.resetting.clone(),
            ResetAt::Gone => self.after.clone(),
        }
    }

    fn guarded(&self, now: Instant) -> bool {
        self.at == ResetAt::Confirm
            && self
                .mounted
                .is_some_and(|at| now.saturating_duration_since(at) < self.guard)
    }

    fn on_grant(&self) -> bool {
        self.labels[self.focus] == self.grant
    }

    fn draw_at_arrow(&mut self) {
        if let Some((at, label)) = self.drawn_at_arrow.take() {
            self.labels.insert(at, label);
            if at <= self.focus {
                self.focus += 1;
            }
        }
    }

    fn move_confirm_cursor(&mut self) {
        let rows: Vec<usize> = (0..self.confirm.len())
            .filter(|&i| {
                let row = self.confirm[i]
                    .trim_start()
                    .trim_start_matches('❯')
                    .trim_start();
                row.starts_with("Yes, use my ") || row.starts_with("No, keep it")
            })
            .collect();
        for i in rows {
            let moved = match self.confirm[i].strip_prefix("   ❯ ") {
                Some(label) => format!("     {label}"),
                None => format!("   ❯ {}", self.confirm[i].trim_start()),
            };
            self.confirm[i] = moved;
        }
    }

    fn on_yes(&self) -> bool {
        aterm_phase::parse_prompt_v2(&self.confirm)
            .is_some_and(|p| p.options.first().is_some_and(|o| o.focused))
    }

    fn dlg(&self) -> i64 {
        match self.at {
            ResetAt::Gone => 0,
            ResetAt::Dialog if self.labels.contains(&self.grant) => 1,
            ResetAt::Dialog => 2,
            ResetAt::Checking => 3,
            ResetAt::Confirm if self.exact => 4,
            ResetAt::Confirm => 5,
            ResetAt::Resetting => 6,
        }
    }
}

const PARKED: Duration = Duration::from_secs(10);

const INPUT_CLEAR: &str = "input=clear input_bytes=0 input_wait_ms=0 fg_rss_mb=-";
const INPUT_STALLED: &str = "input=stalled input_bytes=1 input_wait_ms=161000 fg_rss_mb=39731";

fn ok(stdout: &str) -> CtlReply {
    CtlReply {
        code: 0,
        stdout: stdout.to_string(),
        stderr: String::new(),
    }
}
fn timeout() -> CtlReply {
    CtlReply {
        code: 124,
        stdout: "OK timeout\n".to_string(),
        stderr: String::new(),
    }
}
fn usage(verb: &str) -> CtlReply {
    err(&format!("usage: {verb}"))
}
fn err(text: &str) -> CtlReply {
    CtlReply {
        code: 1,
        stdout: String::new(),
        stderr: format!("aterm-ctl: ERR {text}\n"),
    }
}
fn bare_err() -> CtlReply {
    CtlReply {
        code: 1,
        stdout: String::new(),
        stderr: "aterm-ctl: ERR\n".to_string(),
    }
}
fn closed() -> CtlReply {
    failed(1, "server closed the connection without responding")
}
fn run_now(waits: Option<&super::super::fx::ScriptedClock>) -> Instant {
    waits.map_or_else(Instant::now, super::super::fx::ScriptedClock::mono)
}

fn run_since(waits: Option<&super::super::fx::ScriptedClock>, at: Instant) -> Duration {
    run_now(waits).saturating_duration_since(at)
}

fn run_nap(waits: Option<&super::super::fx::ScriptedClock>, d: Duration) {
    match waits {
        Some(clock) => clock.sleep(d),
        None => std::thread::sleep(d),
    }
}

fn idle_nap(waits: Option<&super::super::fx::ScriptedClock>, still: Duration, nap: Duration) {
    match waits {
        Some(clock) if clock.is_virtual() => clock.skip(still),
        waits => run_nap(waits, still.min(nap)),
    }
}

fn failed(code: i32, text: &str) -> CtlReply {
    CtlReply {
        code,
        stdout: String::new(),
        stderr: format!("aterm-ctl: {text}\n"),
    }
}

impl Mock {
    fn new(screens: Vec<Vec<String>>) -> Self {
        Self {
            requests: Vec::new(),
            replies: VecDeque::new(),
            key_replies: VecDeque::new(),
            screens,
            served: 0,
            seq: 100,
            vanish_after: None,
            exit_when: None,
            exit_at_request: None,
            end_when_parked: false,
            waits: None,
            hang_at: None,
            round_trip: Duration::ZERO,
            after_call: None,
            title: None,
            titles: VecDeque::new(),
            pinned: None,
            title_refused: false,
            idle_after: VecDeque::new(),
            stalls: 0,
            idle_skips_busy: false,
            by_index: BTreeMap::new(),
            down: None,
            drop_awaits_from: None,
            handoff_seq: None,
            restart: None,
            delay: BTreeMap::new(),
            history: None,
            offscreen: VecDeque::new(),
            verb_replies: BTreeMap::new(),
            turn_releases: None,
            key_releases: None,
            keys: 0,
            key_gates: Vec::new(),
            keyed: false,
            turn_gates: Vec::new(),
            attention_gates: Vec::new(),
            attentions: 0,
            attention_kind: None,
            screen_cols: BTreeMap::new(),
            cursor_on_caret: false,
            caret_after_reads: None,
            redrawn_after_reads: None,
            screen_cursors: BTreeMap::new(),
            pane_cols: None,
            whole_sees_the_tail: false,
            last_read_tail: false,
            turned: false,
            turns: 0,
            stall_sleep: None,
            dead_after_turns: None,
            cursor_col: None,
            attention: None,
            foreign_attention: None,
            cwd: std::fs::canonicalize(std::env::temp_dir())
                .ok()
                .map(|d| d.to_string_lossy().into_owned()),
            agent_cwd: None,
            trusted_cwd: None,
            help: "key [id=<key>] [if=<re>] <name>: send a named key\n".to_string(),
            help_send: None,
            send_replies: VecDeque::new(),
            restart_replies: VecDeque::new(),
            hold: "0",
            fabric: "connected",
            hold_lifts_after: None,
            status_reads: 0,
            swap_at_key: None,
            swap_after_keys: 0,
            gen_fence: false,
            sends_gen: false,
            human: Vec::new(),
            human_on: BTreeMap::new(),
            reads: Vec::new(),
            uncounted: 0,
            idle_never: false,
            tick_ms: None,
            redraws_move_gen: false,
            countdown_ticks: false,
            countdown_reads: BTreeMap::new(),
            countdown_left: None,
            stall_after_read: BTreeMap::new(),
            tick_at_key: 0,
            ticks: None,
            read_at: None,
            program: "claude",
            program_resolves_to: None,
            agent: "-",
            claims: Vec::new(),
            claim_replies: VecDeque::new(),
            order: Vec::new(),
            human_attention: None,
            human_on_top: false,
            needs_attention: None,
            needs_calls: Vec::new(),
            needs_replies: VecDeque::new(),
            card_ignores_within: None,
            card_first_served: None,
            early_zeroes: 0,
            card_debounce: None,
            digit: Digit::None,
            reactive_at: None,
            key_probes: Vec::new(),
            card_drops_digits: false,
            backspace_replies: VecDeque::new(),
            human_ms: None,
            hand: "-",
            hand_from: BTreeMap::new(),
            hand_during_idle: None,
            seq_waits_run_out: 0,
            gen_skipped: 0,
            human_seq: None,
            human_seq_from: BTreeMap::new(),
            human_seq_on: BTreeMap::new(),
            submits_on: BTreeMap::new(),
            open_on: std::collections::BTreeSet::new(),
            inputs_on: BTreeMap::new(),
            launch: None,
            person_at_key: 0,
            person_at_send: 0,
            person_at_status: None,
            person_after_send: 0,
            person_skipped: Vec::new(),
            cell_attrs: "dim",
            cell_attrs_on: BTreeMap::new(),
            cell_fg: "d0d0d0",
            cell_unreadable: false,
            cells: BTreeMap::new(),
            history_cells: BTreeMap::new(),
            human_ms_reads: VecDeque::new(),
            status_human_from: BTreeMap::new(),
            agent_pushes: false,
            questions: None,
            questions_after_badge: None,
            questions_at: Vec::new(),
            autopilot: None,
            autopilot_at: Vec::new(),
            autopilot_on: BTreeMap::new(),
            metas: 0,
            input: INPUT_CLEAR,
            input_after: Vec::new(),
            input_unread_at: None,
            first_send_at: None,
            read_times: Vec::new(),
            key_times: Vec::new(),
            attention_at: None,
            idle_never_for: None,
            unmoved_after_send: None,
            unmoved_wakes: 0,
            unmoved_from_read: 0,
            reads_at_send: None,
            unmoved_rekeyed: None,
            lease_times: Vec::new(),
            idle_latch_behind: false,
            screen_gens: BTreeMap::new(),
            cursor_row: None,
            tail_replies: VecDeque::new(),
            agent_zone: None,
            status_sid: "-",
            who: None,
            aimed: Vec::new(),
            gone_on_next: false,
            gone_held: 0,
            gone_ran_out: false,
            after_gone: Vec::new(),
            alt_screen: None,
            scrollback: None,
            scrollback_from: None,
            evicted_below: 0,
            alt_archive: None,
            archive_gap: false,
            unreadable_row: None,
            search_qualifier: "",
            panel: None,
            reset: None,
            task: None,
            task_requests: Vec::new(),
            task_reopens_at: None,
            task_busy: 0,
            task_lost: 0,
            hang_up_at: None,
            hung_up: false,
        }
    }
    fn task_call(&mut self, words: &[&str]) -> CtlReply {
        if let Some(at) = self.task_reopens_at
            && self.served >= at
            && self
                .task
                .as_ref()
                .is_some_and(|(_, f)| f.starts_with("state=done "))
        {
            self.task_reopens_at = None;
            let rev = self.task.as_ref().map_or(0, |(rev, _)| rev + 1);
            self.task = Some((
                rev,
                "state=working from=ctl since_ms=0 result_bytes=0 why=turn".to_string(),
            ));
        }
        let line = |task: &Option<(u64, String)>| match task {
            Some((rev, fields)) => format!("OK task rev={rev} {fields} agent=idle attention=-"),
            None => "OK task -".to_string(),
        };
        match words {
            ["task"] if self.task_lost > 0 => {
                self.task_lost -= 1;
                err("control server busy")
            }
            ["task"] => ok(&format!("{}\n", line(&self.task))),
            ["task", "mark", ..] if self.task_busy > 0 => {
                self.task_busy -= 1;
                err("busy: a seamless update is committing")
            }
            ["task", "mark", state, facts @ ..] => {
                let bytes = facts
                    .iter()
                    .find_map(|f| f.strip_prefix("result_bytes="))
                    .unwrap_or("0");
                let others: String = facts
                    .iter()
                    .filter(|f| !f.starts_with("result_bytes="))
                    .map(|f| format!(" {f}"))
                    .collect();
                let fields =
                    format!("state={state} from=ctl since_ms=0 result_bytes={bytes}{others}");
                let changed = self.task.as_ref().is_none_or(|(_, f)| *f != fields);
                if changed {
                    let rev = self.task.as_ref().map_or(1, |(rev, _)| rev + 1);
                    self.task = Some((rev, fields));
                }
                ok(&format!(
                    "{} changed={}\n",
                    line(&self.task),
                    u8::from(changed)
                ))
            }
            _ => err("usage: task [mark <working|done|exited|stalled> …]"),
        }
    }
    fn meta_line(&self) -> String {
        let shown = match (&self.attention, &self.human_attention) {
            (Some(_), Some(h)) if self.human_on_top => Some((h, "%2D")),
            (Some(a), _) => Some((a, "supervisor")),
            (None, Some(h)) => Some((h, "%2D")),
            (None, None) => self.needs_attention.as_ref().map(|n| (n, "needs")),
        };
        let (attention, owner) = shown.map_or(("-".to_string(), "-"), |(t, o)| {
            (aterm_codec::pct::encode(t), o)
        });
        let owners = usize::from(self.attention.is_some())
            + usize::from(self.human_attention.is_some())
            + usize::from(self.needs_attention.is_some());
        let mut kinds = Vec::new();
        if self.attention.is_some() {
            let kind = self
                .attention_kind
                .as_deref()
                .and_then(|k| k.strip_prefix("kind="))
                .unwrap_or("-");
            kinds.push(format!("{kind}:supervisor"));
        }
        if self.human_attention.is_some() {
            kinds.push("-:-".to_string());
        }
        if self.needs_attention.is_some() {
            kinds.push("ask:needs".to_string());
        }
        let kinds = if kinds.is_empty() {
            "-".to_string()
        } else {
            kinds.join(",")
        };
        let cwd = self.cwd.as_deref().unwrap_or("-");
        let agent_cwd = self.agent_cwd.as_deref().unwrap_or("-");
        let trusted_cwd = self
            .trusted_cwd
            .as_deref()
            .map_or_else(|| "-".to_string(), aterm_codec::pct::encode);
        let questions = self
            .questions
            .map_or_else(String::new, |w| format!(" questions={w}"));
        let autopilot = self
            .autopilot
            .map_or_else(String::new, |w| format!(" autopilot={w}"));
        format!(
            "OK title=- user_title=- description=- icon=- role=- attention={attention} \
             cwd={cwd} state=alive attention_owner={owner} attention_owners={owners} \
             attention_kinds={kinds} supervisor=-{questions} agent_cwd={agent_cwd} \
             trusted_cwd={trusted_cwd}{autopilot}\n"
        )
    }
    fn status_line(&mut self) -> String {
        self.status_reads += 1;
        if self.pinned.is_none()
            && let Some(title) = self.titles.pop_front()
        {
            self.title = title;
        }
        if self.person_at_status == Some(self.status_reads)
            && let Some(n) = self.human_seq.as_mut()
        {
            *n += 1;
        }
        if self.hold_lifts_after.is_some_and(|n| self.status_reads > n) {
            self.hold = "0";
        }
        for &(n, input) in &self.input_after {
            if self.status_reads > n {
                self.input = input;
            }
        }
        if ["input=pending", "input=typeahead"]
            .iter()
            .any(|w| self.input.starts_with(w))
        {
            self.input_unread_at = Some(run_now(self.waits.as_deref()));
        }
        let input = if self.input.is_empty() {
            String::new()
        } else {
            format!(" {}", self.input)
        };
        let after_read = self
            .status_human_from
            .range(..=self.reads.len())
            .next_back()
            .map(|(_, ms)| *ms);
        let keyed = self
            .panel
            .as_ref()
            .and_then(|p| p.keyed_at)
            .or_else(|| self.reset.as_ref().and_then(|w| w.keyed_at))
            .map(|at| {
                u64::try_from(run_since(self.waits.as_deref(), at).as_millis()).unwrap_or(u64::MAX)
            });
        let mut human = keyed
            .or(after_read)
            .map_or_else(
                || self.human_ms_reads.pop_front().unwrap_or(self.human_ms),
                Some,
            )
            .map_or_else(String::new, |ms| format!(" human_ms={ms}"));
        if self.agent_pushes {
            human.push_str(" agent_rev=1");
        }
        if let Some(launch) = self.launch {
            human.push(' ');
            human.push_str(&launch.status(self.human_seq));
        }
        let (subject, source) = match &self.pinned {
            Some(pin) => (Some(pin.as_str()), "pin"),
            None => (self.title.as_deref(), "osc"),
        };
        let title = subject.map_or_else(String::new, |t| {
            let enc: String = t
                .bytes()
                .map(|b| {
                    if b.is_ascii_alphanumeric() {
                        char::from(b).to_string()
                    } else {
                        format!("%{b:02X}")
                    }
                })
                .collect();
            format!(" subject={enc} subject_source={source}")
        });
        let hand = self
            .hand_from
            .range(..=self.reads.len())
            .next_back()
            .map_or(self.hand, |(_, h)| *h);
        let generation = if self.redraws_move_gen {
            format!(" gen=1.{}", self.seq)
        } else {
            String::new()
        };
        format!(
            "OK schema=1 sid={} phase=running hold={} fabric={} program={} agent={}{input} \
             seq={}{generation} hand={hand}{human}{title}\n",
            self.status_sid, self.hold, self.fabric, self.program, self.agent, self.seq
        )
    }
    fn measured_cell(&self, tail: &[&str]) -> Option<String> {
        let [row, col] = tail else { return None };
        let col = col.parse().ok()?;
        let Some(abs) = row.strip_prefix("abs=") else {
            return self.cells.get(&(row.parse().ok()?, col)).cloned();
        };
        let abs: usize = abs.parse().ok()?;
        let above = self.history().len();
        if abs < above {
            self.history_cells.get(&(abs, col)).cloned()
        } else {
            self.cells.get(&(abs - above, col)).cloned()
        }
    }

    fn scrollback_search(&self, tail: &[&str]) -> CtlReply {
        let history = self.history();
        let verb = "search";
        if tail.is_empty() {
            return super::super::transport::render(
                verb,
                aterm_types::control_verbs::framing_of(verb, verb),
                &format!("OK 0 screen_first={}", history.len()),
                String::new(),
            );
        }
        let [pattern, "regex"] = tail else {
            return err("usage: search <pattern> [regex]");
        };
        let Some(lead) = pattern
            .strip_prefix('^')
            .filter(|l| !l.contains(|c: char| "\\.*+?[](){}|$^".contains(c)))
        else {
            return err("the mock searches for an anchored literal only");
        };
        let hits: String = history
            .iter()
            .chain(self.last_served())
            .enumerate()
            .filter(|(_, r)| r.starts_with(lead))
            .map(|(k, _)| format!("{k} 0 {}\n", lead.chars().count()))
            .collect();
        super::super::transport::render(
            verb,
            aterm_types::control_verbs::framing_of(verb, verb),
            &format!(
                "OK {}{} screen_first={}",
                hits.lines().count(),
                self.search_qualifier,
                history.len()
            ),
            hits,
        )
    }

    fn scrollback_line(&self, tail: &[&str]) -> CtlReply {
        let Some(row) = tail.first().and_then(|n| n.parse::<u64>().ok()) else {
            return err("usage: line <n>");
        };
        if row < self.evicted_below {
            return err("evicted");
        }
        if self.unreadable_row == Some(row) {
            return err("unreadable");
        }
        let history = self.history();
        let text = usize::try_from(row)
            .ok()
            .and_then(|k| history.iter().chain(self.last_served()).nth(k));
        let verb = "line";
        match text {
            Some(text) => super::super::transport::render(
                verb,
                aterm_types::control_verbs::framing_of(verb, verb),
                &format!("OK {text}"),
                "",
            ),
            None => err("out of range"),
        }
    }

    fn archive_read(&self, tail: &[&str]) -> CtlReply {
        let archive = self.alt_archive.as_deref().unwrap_or_default();
        let arg = |key: &str| {
            tail.iter()
                .find_map(|a| a.strip_prefix(key))
                .and_then(|v| v.parse::<usize>().ok())
        };
        let since = arg("since=").unwrap_or(0).min(archive.len());
        let after = &archive[since..];
        let take = arg("tail=").unwrap_or(2000).min(after.len());
        let rows = &after[after.len() - take..];
        let screen: &[String] = if tail.contains(&"screen=1") {
            self.last_served()
        } else {
            &[]
        };
        let mut body = String::new();
        for row in rows.iter().chain(screen) {
            body.push_str(row);
            body.push('\n');
        }
        let verb = "offscreen";
        super::super::transport::render(
            verb,
            aterm_types::control_verbs::framing_of(verb, verb),
            &format!(
                "OK {} first={} last={} lost=0 breaks={} back=0 epoch=1 origin=7 alt=1 seq={} \
                 screen_rows={}",
                rows.len() + screen.len(),
                archive.len() + 1 - take,
                archive.len(),
                u8::from(self.archive_gap),
                self.seq,
                screen.len()
            ),
            body,
        )
    }

    fn history(&self) -> &[String] {
        let from = self.scrollback_from.unwrap_or(0);
        if self.served.saturating_sub(1) < from {
            return &[];
        }
        self.scrollback.as_deref().unwrap_or_default()
    }

    fn last_served(&self) -> &[String] {
        &self.screens[self.served.saturating_sub(1).min(self.screens.len() - 1)]
    }

    pub(super) fn history_served(&mut self) {
        self.scrollback.get_or_insert_with(Vec::new);
        self.alt_screen.get_or_insert(false);
    }
    fn unmoved_for(&self) -> Option<Duration> {
        let (hold, sent) = self.unmoved_after_send.zip(self.first_send_at)?;
        if self
            .reads_at_send
            .is_some_and(|at| self.read_times.len() < at + self.unmoved_from_read)
        {
            return None;
        }
        hold.checked_sub(run_since(self.waits.as_deref(), sent))
            .filter(|left| !left.is_zero())
    }
    fn next_read_seq(&self) -> u64 {
        let redrawn = self
            .redrawn_after_reads
            .is_some_and(|n| self.reads.len() >= n);
        let last = self.screens.len().saturating_sub(1);
        let (i, seq) = if self.card_debounce.is_some() || self.held() && !redrawn {
            (self.served.saturating_sub(1).min(last), self.seq)
        } else {
            (self.served.min(last), self.seq + 1)
        };
        self.screen_gens.get(&i).map_or(seq, |&(_, s)| s)
    }
    fn idle_latched(&self) -> String {
        let seq = self.next_read_seq();
        let seq = if self.idle_latch_behind {
            seq.saturating_sub(1)
        } else {
            seq
        };
        format!("OK idle {seq}\n")
    }
    fn held(&self) -> bool {
        self.unmoved_for().is_some()
            || self
                .turn_releases
                .is_some_and(|i| self.served == i + 1 && !self.turned)
            || self
                .key_releases
                .is_some_and(|i| self.served == i + 1 && !self.keyed)
            || self
                .key_gates
                .iter()
                .any(|&(i, k)| self.served == i + 1 && self.keys <= k)
            || self
                .turn_gates
                .iter()
                .enumerate()
                .any(|(k, &g)| self.served == g + 1 && self.turns <= k)
            || self
                .attention_gates
                .iter()
                .any(|&(i, k)| self.served == i + 1 && self.attentions <= k)
    }
    fn screen_json(&mut self, tail: Option<usize>) -> String {
        let after_tail = std::mem::replace(&mut self.last_read_tail, tail.is_some());
        let redrawn = self
            .redrawn_after_reads
            .is_some_and(|n| self.reads.len() >= n);
        let i = if self.card_debounce.is_some() {
            self.reactive_index()
        } else if self.held() && !redrawn
            || tail.is_none() && after_tail && self.whole_sees_the_tail && self.served > 0
        {
            (self.served - 1).min(self.screens.len() - 1)
        } else {
            let i = self.served.min(self.screens.len() - 1);
            self.served += 1;
            self.seq += 1;
            i
        };
        if survey_open(&self.screens[i]) {
            let now = run_now(self.waits.as_deref());
            self.card_first_served.get_or_insert(now);
        } else {
            self.card_first_served = None;
        }
        let first = tail.map_or(0, |n| self.screens[i].len().saturating_sub(n));
        let shown = if self.countdown_ticks {
            let reads = self.countdown_reads.entry(i).or_insert(0);
            let (rows, left) = ticked_countdown(&self.screens[i], *reads);
            *reads += 1;
            self.countdown_left = left;
            rows
        } else {
            self.screens[i].clone()
        };
        let mut shown = shown;
        if let Some((row, text)) = self.unmoved_rekeyed
            && self.attention_at.is_some()
            && self.unmoved_for().is_some()
            && let Some(at) = shown.get_mut(row)
        {
            *at = text.to_string();
        }
        let rows: Vec<String> = shown[first..]
            .iter()
            .map(|r| format!("\"{}\"", r.replace('\\', "\\\\").replace('"', "\\\"")))
            .collect();
        let first = if first > 0 {
            format!(",\"first\":{first}")
        } else {
            String::new()
        };
        let composer_col = self
            .screen_cols
            .get(&i)
            .copied()
            .or(self.cursor_col)
            .unwrap_or_else(|| {
                if self.screens[i].iter().any(|r| r == "❯ 1" || r == "❯ 0") {
                    3
                } else {
                    2
                }
            });
        let (epoch, seq) = self.screen_gens.get(&i).copied().unwrap_or((1, self.seq));
        let generation = if self.sends_gen {
            format!(",\"gen\":\"{epoch}.{seq}\"")
        } else {
            String::new()
        };
        let cursor_row = if let Some(&row) = self.screen_cursors.get(&i) {
            row
        } else if let Some(row) = self.cursor_row {
            row
        } else if self.cursor_on_caret
            || self
                .caret_after_reads
                .is_some_and(|n| self.reads.len() >= n)
        {
            self.screens[i]
                .iter()
                .rposition(|r| r.starts_with(['❯', '›', '»']))
                .unwrap_or(10)
        } else {
            10
        };
        let stamp = match (self.human_on.get(&i), self.human.len()) {
            (Some(&on), _) => on,
            (None, 0) => "",
            (None, 1) => self.human[0],
            (None, _) => self.human.remove(0),
        };
        self.reads.push((i, stamp));
        self.read_times.push((i, run_now(self.waits.as_deref())));
        if let Some((_, &n)) = self.human_seq_from.range(..=self.reads.len()).next_back() {
            self.human_seq = Some(n);
        }
        let mut human = if stamp.is_empty() {
            String::new()
        } else {
            format!(",\"human_ms\":{stamp}")
        };
        let count = self.human_seq_on.get(&i).copied().unwrap_or(self.human_seq);
        if let Some(n) = count {
            human.push_str(&format!(",\"human_seq\":{n}"));
        }
        if let Some(launch) = self.launch {
            human.push_str(&format!(",\"launch\":{}", launch.json(self.human_seq)));
        }
        if let Some((before, after)) = self.inputs_on.get(&i) {
            let before = before.map_or_else(|| "null".to_string(), |n| n.to_string());
            human.push_str(&format!(",\"inputs\":[{before},{after}]"));
        }
        if let Some(n) = self.submits_on.get(&i) {
            human.push_str(&format!(",\"submits\":{n}"));
        }
        if self.open_on.contains(&i) {
            human.push_str(",\"frame\":\"open\"");
        }
        format!(
            "{{\"rows\":[{}],\"cursor\":{{\"row\":{cursor_row},\"col\":{composer_col},\"visible\":true,\"style\":\"block\"}},\"dims\":{{\"rows\":{},\"cols\":{}}},\"seq\":{}{generation}{human}{first}}}\n",
            rows.join(","),
            self.screens[i].len(),
            self.pane_cols.unwrap_or(120),
            seq
        )
    }
    fn exiting(&self) -> bool {
        self.exit_when.is_some_and(|done| done(self))
            || self
                .exit_at_request
                .is_some_and(|n| self.requests.len() >= n)
    }
    fn parked(&self, tail: &[&str]) -> bool {
        self.end_when_parked
            && self.served >= self.screens.len()
            && tail
                .iter()
                .position(|a| *a == "timeout")
                .and_then(|i| tail.get(i + 1))
                .and_then(|ms| ms.parse::<u64>().ok())
                .is_some_and(|ms| Duration::from_millis(ms) >= PARKED)
    }
    fn stall(&mut self, tail: &[&str]) -> CtlReply {
        if self.exiting() {
            return err("exited");
        }
        if let Some(n) = self.vanish_after {
            if self.stalls >= n {
                return err("exited");
            }
            self.stalls += 1;
        }
        if let Some(nap) = self.stall_sleep {
            let asked = tail
                .iter()
                .position(|a| *a == "timeout")
                .and_then(|i| tail.get(i + 1))
                .and_then(|ms| ms.parse::<u64>().ok())
                .map_or(nap, Duration::from_millis);
            match &self.waits {
                Some(clock) => clock.skip(nap.min(asked)),
                None => std::thread::sleep(nap.min(asked)),
            }
        }
        timeout()
    }
    fn presses(&self) -> Vec<&String> {
        self.requests
            .iter()
            .filter(|r| verb_of(r) == "key")
            .collect()
    }
    fn reactive_index(&mut self) -> usize {
        let debounce = self.card_debounce.expect("reactive");
        if let Digit::Pending { at, take } = self.digit
            && run_since(self.waits.as_deref(), at) >= debounce
        {
            self.digit = if take { Digit::Taken } else { Digit::Standing };
        }
        let i = match self.digit {
            Digit::None => 0,
            Digit::Pending { .. } | Digit::Standing => 1,
            Digit::Taken => 2,
        };
        if self.reactive_at != Some(i) {
            self.reactive_at = Some(i);
            self.seq += 1;
        }
        self.served = i + 1;
        i
    }
    fn stall_count(&mut self) -> CtlReply {
        if self.exiting() {
            return err("exited");
        }
        if let Some(n) = self.vanish_after {
            if self.stalls >= n {
                return err("exited");
            }
            self.stalls += 1;
        }
        timeout()
    }
    fn reactive_wait(
        &mut self,
        tail: &[&str],
        done: impl Fn(&mut Self) -> Option<CtlReply>,
    ) -> CtlReply {
        let asked = tail
            .iter()
            .position(|a| *a == "timeout")
            .and_then(|i| tail.get(i + 1))
            .and_then(|ms| ms.parse::<u64>().ok())
            .map_or(Duration::from_millis(2000), Duration::from_millis);
        let asked = asked.min(self.stall_sleep.unwrap_or(asked));
        let start = run_now(self.waits.as_deref());
        loop {
            self.reactive_index();
            if let Some(r) = done(self) {
                return r;
            }
            if run_since(self.waits.as_deref(), start) >= asked {
                return self.stall_count();
            }
            match &self.waits {
                Some(clock) => clock.skip(Duration::from_millis(2)),
                None => std::thread::sleep(Duration::from_millis(2)),
            }
        }
    }
    fn writes(&self) -> Vec<String> {
        self.requests
            .iter()
            .filter_map(|r| match verb_of(r) {
                "key" => r.rsplit_once(' ').map(|(_, k)| k.to_string()),
                "send" => Some(format!(
                    "text:{}",
                    r.split_once(" -- ").map_or(r.as_str(), |(_, t)| t)
                )),
                _ => None,
            })
            .collect()
    }
}

impl Mock {
    fn panel_holds(&self) -> bool {
        self.panel
            .as_ref()
            .is_some_and(|p| matches!(p.at, PanelAt::Typed(_) | PanelAt::Open) || p.quiet)
    }

    fn panel_pan(&self) -> i64 {
        use crate::harness::usage::{PanelRead, usage_panel_read};
        let rows = self.last_served();
        match aterm_phase::settings_panel(rows) {
            None => 0,
            Some(p) => match usage_panel_read(rows, p.header) {
                PanelRead::Settled(_) => 2,
                _ => 1,
            },
        }
    }

    fn panel_hand(&self) -> bool {
        let Some(p) = self.panel.as_ref() else {
            return false;
        };
        let grace = u64::try_from(p.grace.as_millis()).unwrap_or(u64::MAX);
        p.keyed_at
            .is_some_and(|at| run_since(self.waits.as_deref(), at) < p.grace)
            || self.human_ms.is_some_and(|ms| ms < grace)
    }

    fn panel_person(&mut self) {
        if let Some(p) = self.panel.as_mut() {
            p.keyed_at = Some(run_now(self.waits.as_deref()));
        }
    }

    fn panel_probe(&mut self, request: &str) {
        use std::sync::atomic::Ordering::SeqCst;
        if self.panel.is_none() {
            return;
        }
        let (pan, hand) = (self.panel_pan(), self.panel_hand());
        let turn = worker_phase(self.last_served()) == Phase::Busy;
        let hold = self.hold == "1";
        if let Some(p) = self.panel.as_mut() {
            p.probes.push(PanelProbe {
                request: request.to_string(),
                pan,
                hand,
                hold,
                turn,
                owed: matches!(p.at, PanelAt::Typed(_) | PanelAt::Open),
                claims: p.shared.claims.load(SeqCst),
                tolds: p.shared.tolds.load(SeqCst),
                at: run_now(self.waits.as_deref()),
            });
        }
    }

    fn panel_show(&mut self, rows: Vec<String>) {
        let last = self.screens.len() - 1;
        if self.screens[last] != rows {
            self.screens[last] = rows;
            self.seq += 1;
        }
    }

    fn panel_runs_on(&mut self, screens: Vec<Vec<String>>) {
        if screens.is_empty() {
            return;
        }
        let last = self.screens.len() - 1;
        self.screens.truncate(last);
        self.screens.extend(screens);
        self.served = last;
        self.seq += 1;
    }

    fn panel_read(&mut self) -> bool {
        match self.panel.as_mut().and_then(|p| p.mends.take()) {
            Some((true, rows)) => {
                self.panel_show(rows);
                return false;
            }
            Some((false, rows)) => {
                if let Some(p) = self.panel.as_mut() {
                    p.mends = Some((true, rows));
                }
                return false;
            }
            None => {}
        }
        let Some(p) = self.panel.as_mut().filter(|p| p.at == PanelAt::Open) else {
            return false;
        };
        p.reads += 1;
        if p.lose_read == Some(p.reads) {
            return true;
        }
        if let Some((at, stop)) = &p.stop_at_read
            && *at == p.reads
        {
            stop.store(true, Ordering::SeqCst);
        }
        let keys = p.person_after == Some(p.reads);
        let leaves = p.leaves_after.is_some_and(|n| p.reads > n);
        let frame = if leaves {
            p.at = PanelAt::Rest;
            p.after.clone().or_else(|| p.rest.clone())
        } else {
            p.frames.get((p.reads - 1).min(p.frames.len() - 1)).cloned()
        };
        if keys {
            p.keyed_at = Some(run_now(self.waits.as_deref()));
            if let Some(n) = self.human_seq.as_mut() {
                *n += 1;
            }
        }
        if let Some(frame) = frame {
            self.panel_show(frame);
        }
        false
    }

    fn panel_seen(&mut self) {
        use std::sync::atomic::Ordering::SeqCst;
        if self.panel.is_none() {
            return;
        }
        let (pan, hand) = (self.panel_pan(), self.panel_hand());
        if let Some(p) = self.panel.as_ref() {
            p.shared.pan.store(pan, SeqCst);
            p.shared.hand.store(hand, SeqCst);
        }
    }

    fn panel_wait(&mut self, tail: &[&str]) -> CtlReply {
        if self.exiting() {
            return err("exited");
        }
        let asked = tail
            .iter()
            .position(|a| *a == "timeout")
            .and_then(|i| tail.get(i + 1))
            .and_then(|ms| ms.parse::<u64>().ok())
            .map_or(Duration::ZERO, Duration::from_millis);
        let nap = asked.min(self.stall_sleep.unwrap_or(Duration::from_millis(2)));
        let open = self.panel.as_ref().is_some_and(|p| p.at == PanelAt::Open);
        let repaints = self.panel.as_ref().is_some_and(|p| p.repaints);
        let idle = self.idle_latched();
        let latched = match tail.first() {
            Some(&"seq") => tail
                .get(1)
                .and_then(|n| n.parse::<u64>().ok())
                .is_some_and(|n| self.seq > n)
                .then(|| ok("OK seq\n")),
            Some(&"gone") => tail
                .get(1)
                .and_then(|p| aterm_observe::row_matcher(p).ok())
                .is_none_or(|m| !self.last_served().iter().any(|r| m.matches(r)))
                .then(|| ok(&format!("OK gone {}\n", self.seq))),
            Some(&"idle") => (!(open && repaints)).then(|| {
                let still = tail
                    .get(1)
                    .and_then(|ms| ms.parse::<u64>().ok())
                    .map_or(Duration::ZERO, Duration::from_millis);
                idle_nap(self.waits.as_deref(), still, nap);
                ok(&idle)
            }),
            _ => None,
        };
        latched.unwrap_or_else(|| {
            run_nap(self.waits.as_deref(), nap);
            timeout()
        })
    }

    fn panel_sent(&mut self, body: &str) {
        let last = self.screens.len() - 1;
        let on_last = self.served >= self.screens.len();
        let composer = self.screens[last].clone();
        let Some(p) = self.panel.as_mut() else {
            return;
        };
        if p.at == PanelAt::Over && on_last {
            p.at = PanelAt::Rest;
        }
        if p.at != PanelAt::Rest {
            return;
        }
        let Some(caret) = composer.iter().rposition(|r| r == "❯") else {
            return;
        };
        p.quiet = false;
        p.at = PanelAt::Typed(body.to_string());
        p.rest = Some(composer.clone());
        let menu = p.menu && body == "/usage";
        let mut typed = composer;
        typed[caret] = format!("❯ {body}");
        if menu && let Some(bottom) = (caret + 1..typed.len()).find(|&i| typed[i].starts_with('─'))
        {
            use aterm_phase::prompt::fixtures::{SLASH_MENU_USAGE_MEASURED, screen};
            let measured = screen(SLASH_MENU_USAGE_MEASURED);
            let under = measured
                .iter()
                .rposition(|r| r.starts_with('─'))
                .expect("the composer's bottom rule");
            typed.truncate(bottom + 1);
            typed.extend(measured[under + 1..].iter().cloned());
        }
        self.cursor_col = Some(2 + body.chars().count());
        self.panel_show(typed);
    }

    fn panel_key(&mut self, key: &str) {
        enum Does {
            Nothing,
            Show(Vec<String>),
            RunOn(Vec<String>, Vec<Vec<String>>),
        }
        let Some(p) = self.panel.as_mut() else {
            return;
        };
        let does = match (key, p.at.clone()) {
            ("enter", PanelAt::Typed(body)) => {
                let rest = p.rest.clone().unwrap_or_default();
                if body != crate::harness::usage::USAGE_COMMAND {
                    p.at = PanelAt::Over;
                    Does::RunOn(rest, p.then.pop_front().unwrap_or_default())
                } else if let Some(turn) = p.turn_instead.take() {
                    p.at = PanelAt::Over;
                    Does::RunOn(rest, turn)
                } else if p.opens {
                    p.at = PanelAt::Open;
                    p.reads = 0;
                    Does::Show(p.frames[0].clone())
                } else {
                    p.at = PanelAt::Rest;
                    p.quiet = true;
                    Does::Show(rest)
                }
            }
            ("esc", PanelAt::Open) if p.esc_fails > 0 => {
                p.esc_fails -= 1;
                Does::Nothing
            }
            ("esc", PanelAt::Open) => {
                p.at = PanelAt::Rest;
                let rest = p
                    .after
                    .clone()
                    .or_else(|| p.rest.clone())
                    .unwrap_or_default();
                match p.torn.take() {
                    Some(torn) => {
                        p.mends = Some((false, rest));
                        Does::Show(torn)
                    }
                    None => Does::Show(rest),
                }
            }
            _ => Does::Nothing,
        };
        match does {
            Does::Nothing => {}
            Does::Show(rows) => {
                self.cursor_col = None;
                self.panel_show(rows);
            }
            Does::RunOn(rest, then) => {
                self.cursor_col = None;
                self.panel_show(rest);
                self.panel_runs_on(then);
            }
        }
    }
}

impl Mock {
    fn reset_holds(&self) -> bool {
        use std::sync::atomic::Ordering::SeqCst;
        self.reset.as_ref().is_some_and(|w| {
            if w.at == ResetAt::Gone {
                (w.shared.claim.load(SeqCst) == 1 && !w.untold) || !w.gone_read
            } else {
                self.served >= self.screens.len()
            }
        })
    }

    fn reset_hand(&self) -> bool {
        let Some(w) = self.reset.as_ref() else {
            return false;
        };
        let grace = u64::try_from(w.grace.as_millis()).unwrap_or(u64::MAX);
        w.keyed_at
            .is_some_and(|at| run_since(self.waits.as_deref(), at) < w.grace)
            || self.human_ms.is_some_and(|ms| ms < grace)
    }

    fn reset_person(&mut self) {
        if let Some(w) = self.reset.as_mut() {
            w.keyed_at = Some(run_now(self.waits.as_deref()));
        }
    }

    fn reset_seen(&mut self) {
        use std::sync::atomic::Ordering::SeqCst;
        let (hand, hold) = (self.reset_hand(), self.hold == "1");
        if let Some(w) = self.reset.as_ref() {
            w.shared.dlg.store(w.dlg(), SeqCst);
            w.shared.hand.store(hand, SeqCst);
            w.shared.hold.store(hold, SeqCst);
        }
    }

    fn reset_show(&mut self) {
        let Some(w) = self.reset.as_mut() else {
            return;
        };
        let frame = w.frame();
        self.panel_show(frame);
        self.reset_seen();
    }

    fn reset_read(&mut self) {
        let on_world = self.served >= self.screens.len();
        let Some(w) = self.reset.as_mut() else {
            return;
        };
        w.gone_read = w.at == ResetAt::Gone;
        if !on_world {
            return;
        }
        w.reads += 1;
        let moved = match w.at {
            ResetAt::Checking if w.reads > w.check_reads => {
                w.at = ResetAt::Confirm;
                w.mounted = Some(run_now(self.waits.as_deref()));
                true
            }
            ResetAt::Resetting if w.reads > w.claim_reads => {
                w.at = ResetAt::Gone;
                true
            }
            ResetAt::Confirm
                if w.yes_by_person_after.is_some_and(|after| {
                    w.mounted
                        .is_some_and(|at| run_since(self.waits.as_deref(), at) >= after)
                }) && w.on_yes() =>
            {
                w.yes_by_person_after = None;
                w.yes_taken += 1;
                w.at = ResetAt::Resetting;
                w.keyed_at = Some(run_now(self.waits.as_deref()));
                if let Some(n) = self.human_seq.as_mut() {
                    *n += 1;
                }
                true
            }
            _ => false,
        };
        if moved {
            w.reads = 1;
            w.gone_read = w.at == ResetAt::Gone;
        }
        let keys = w.at == ResetAt::Confirm && w.person_at_read == Some(w.reads);
        if keys {
            w.keyed_at = Some(run_now(self.waits.as_deref()));
            if let Some(n) = self.human_seq.as_mut() {
                *n += 1;
            }
        }
        if moved {
            self.reset_show();
        }
    }

    fn reset_wait(&mut self, tail: &[&str]) -> CtlReply {
        let (asked, statuses) = (self.attentions > 0, self.status_reads);
        if let Some(w) = self.reset.as_mut() {
            w.held_waits += 1;
            let over = w.exits_when.is_some_and(|exit| match exit {
                ResetExit::Told => w.shared.claim.load(Ordering::SeqCst) == 2,
                ResetExit::Refused => !w.refused.is_empty(),
                ResetExit::Asked => asked,
                ResetExit::Statuses(n) => statuses >= n,
                ResetExit::Spent => w.yes_taken > 0,
            });
            if over || w.held_waits > 20_000 {
                return err("exited");
            }
        }
        let asked = tail
            .iter()
            .position(|a| *a == "timeout")
            .and_then(|i| tail.get(i + 1))
            .and_then(|ms| ms.parse::<u64>().ok())
            .map_or(Duration::ZERO, Duration::from_millis);
        let nap = asked.min(self.stall_sleep.unwrap_or(Duration::from_millis(2)));
        let idle = self.idle_latched();
        let latched = match tail.first() {
            Some(&"seq") => tail
                .get(1)
                .and_then(|n| n.parse::<u64>().ok())
                .is_some_and(|n| self.seq > n)
                .then(|| ok("OK seq\n")),
            Some(&"gone") => tail
                .get(1)
                .and_then(|p| aterm_observe::row_matcher(p).ok())
                .is_none_or(|m| !self.last_served().iter().any(|r| m.matches(r)))
                .then(|| ok(&format!("OK gone {}\n", self.seq))),
            Some(&"idle") => {
                let still = tail
                    .get(1)
                    .and_then(|ms| ms.parse::<u64>().ok())
                    .map_or(Duration::ZERO, Duration::from_millis);
                idle_nap(self.waits.as_deref(), still, nap);
                Some(ok(&idle))
            }
            _ => None,
        };
        latched.unwrap_or_else(|| {
            run_nap(self.waits.as_deref(), nap);
            timeout()
        })
    }

    fn reset_probe(&mut self, request: &str, key: &str) {
        use std::sync::atomic::Ordering::SeqCst;
        let on_world = self.served >= self.screens.len();
        if let Some(w) = self.reset.as_mut()
            && on_world
        {
            let left = match w.at {
                ResetAt::Confirm => Some(&mut w.person_at_confirm_key),
                ResetAt::Dialog => Some(&mut w.person_at_row_key),
                _ => None,
            };
            if let Some(left) = left.filter(|left| **left > 0) {
                *left -= 1;
                w.keyed_at = Some(run_now(self.waits.as_deref()));
                if let Some(n) = self.human_seq.as_mut() {
                    *n += 1;
                }
            }
        }
        let (hand, hold) = (self.reset_hand(), self.hold == "1");
        let Some(w) = self.reset.as_mut() else {
            return;
        };
        let at = if on_world || w.at == ResetAt::Gone {
            w.at
        } else {
            ResetAt::Gone
        };
        w.probes.push(ResetProbe {
            request: request.to_string(),
            key: key.to_string(),
            at,
            dlg: if at == ResetAt::Gone { 0 } else { w.dlg() },
            guard: w.guarded(run_now(self.waits.as_deref())),
            focus: at == ResetAt::Confirm && w.on_yes(),
            hand,
            hold,
            claim: w.shared.claim.load(SeqCst),
            ph: w.shared.ph.load(SeqCst),
            own: w.shared.own.load(SeqCst),
            row: (at == ResetAt::Dialog).then(|| w.labels[w.focus].clone()),
            on_grant: at == ResetAt::Dialog && w.on_grant(),
            written: false,
        });
    }

    fn reset_key(&mut self, key: &str) -> bool {
        use std::sync::atomic::Ordering::SeqCst;
        let on_world = self.served >= self.screens.len();
        let Some(w) = self.reset.as_mut() else {
            return false;
        };
        if let Some(probe) = w.probes.last_mut() {
            probe.written = true;
        }
        if !on_world {
            return false;
        }
        let was = w.at;
        let lost = w.lose_reply_to.is_some_and(|(at, k)| at == was && k == key);
        if lost {
            w.lose_reply_to = None;
        }
        match (w.at, key) {
            (ResetAt::Dialog, "down") => {
                w.focus = (w.focus + 1) % w.labels.len();
                w.draw_at_arrow();
            }
            (ResetAt::Dialog, "up") => {
                w.focus = (w.focus + w.labels.len() - 1) % w.labels.len();
                w.draw_at_arrow();
            }
            (ResetAt::Dialog, "enter") => {
                let grant = w.on_grant();
                w.chosen.push(w.labels[w.focus].clone());
                if !grant {
                    w.at = ResetAt::Gone;
                } else {
                    w.at = ResetAt::Checking;
                    w.shared.ph.store(1, SeqCst);
                    w.shared.own.store(true, SeqCst);
                }
            }
            (ResetAt::Dialog, "esc") => w.at = ResetAt::Gone,
            (ResetAt::Checking, "esc") => {
                w.shared.ph.store(3, SeqCst);
                w.at = ResetAt::Gone;
            }
            (ResetAt::Confirm, key) if w.guarded(run_now(self.waits.as_deref())) => {
                w.refused.push(key.to_string());
                w.mounted = Some(run_now(self.waits.as_deref()));
                if matches!(key, "enter" | "esc") {
                    w.shared
                        .ph
                        .store(if key == "enter" { 2 } else { 3 }, SeqCst);
                }
            }
            (ResetAt::Confirm, "down" | "up") => w.move_confirm_cursor(),
            (ResetAt::Confirm, "enter") => {
                w.shared.ph.store(2, SeqCst);
                if w.enter_drops > 0 {
                    w.enter_drops -= 1;
                } else if w.on_yes() {
                    w.yes_taken += 1;
                    w.at = ResetAt::Resetting;
                } else {
                    w.at = ResetAt::Gone;
                }
            }
            (ResetAt::Confirm, "esc") => {
                w.shared.ph.store(3, SeqCst);
                if w.esc_drops > 0 {
                    w.esc_drops -= 1;
                    if w.cursor_moves_at_esc {
                        w.move_confirm_cursor();
                    }
                } else {
                    w.at = ResetAt::Gone;
                }
            }
            _ => {}
        }
        if w.at != was {
            w.reads = 0;
        }
        self.reset_show();
        lost
    }
}

fn verb_of(request: &str) -> &str {
    request
        .split_whitespace()
        .find(|w| !w.starts_with('@'))
        .unwrap_or("")
}

impl Mock {
    fn serve(&mut self, args: &[&str]) -> Result<CtlReply, String> {
        let line = args.join(" ");
        self.requests.push(line.clone());
        let after_gone = std::mem::take(&mut self.gone_ran_out);
        let verb_at = usize::from(args.first().is_some_and(|a| a.starts_with('@')));
        let verb = args.get(verb_at).copied().unwrap_or("");
        let tail = &args[verb_at + 1..];
        if verb == "turn" {
            self.turned = true;
            self.turns += 1;
        }
        if verb == "await"
            && tail.first() == Some(&"idle")
            && let Some((quiet, hand)) = self.hand_during_idle
            && tail.get(1) == Some(&quiet)
        {
            self.hand = hand;
            self.hand_from.clear();
        }
        if self.dead_after_turns.is_some_and(|n| self.turns > n) {
            return Ok(err("exited"));
        }
        if verb == "lease" {
            self.lease_times
                .push((line.clone(), run_now(self.waits.as_deref())));
        }
        if let Some(r) = self
            .verb_replies
            .get_mut(verb)
            .and_then(VecDeque::pop_front)
        {
            return Ok(r);
        }
        match verb {
            "turn" => Ok(ok(&format!(
                "OK id=1 submitted=1 status=settled seq={} dur_ms=600 hash=0\n",
                self.seq
            ))),
            "text" => {
                let rows = tail
                    .iter()
                    .find_map(|a| a.strip_prefix("tail="))
                    .and_then(|n| n.parse::<usize>().ok());
                if rows.is_some()
                    && let Some(body) = self.tail_replies.pop_front()
                {
                    return Ok(ok(&body));
                }
                if rows.is_some()
                    && let Some(p) = self.panel.as_mut()
                {
                    p.quiet = false;
                }
                if self.panel_read() {
                    return Ok(closed());
                }
                self.reset_read();
                let before = self.served;
                let body = self.screen_json(rows);
                self.read_at = Some(run_now(self.waits.as_deref()));
                if let Some(word) = self.autopilot_on.get(&before) {
                    self.autopilot = *word;
                }
                self.panel_seen();
                self.reset_seen();
                self.after_gone.push(after_gone);
                if let Some(stall) = self.stall_after_read.remove(&self.reads.len()) {
                    run_nap(self.waits.as_deref(), stall);
                }
                Ok(ok(&body))
            }
            "modes" if self.alt_screen.is_some() => {
                let verb = "modes";
                Ok(super::super::transport::render(
                    verb,
                    aterm_types::control_verbs::framing_of(verb, verb),
                    "OK 2",
                    format!(
                        "alt_screen={}\ncursor_visible=true\n",
                        self.alt_screen.unwrap_or(false)
                    ),
                ))
            }
            "await" if self.panel_holds() => Ok(self.panel_wait(tail)),
            "await" if self.reset_holds() => Ok(self.reset_wait(tail)),
            "await"
                if tail.first() == Some(&"seq")
                    && tail.get(2) == Some(&"timeout")
                    && tail.get(3) == Some(&"0") =>
            {
                let n = tail.get(1).and_then(|n| n.parse::<u64>().ok());
                Ok(if n.is_some_and(|n| n < self.seq) {
                    ok(&format!("OK seq {}\n", self.seq))
                } else {
                    timeout()
                })
            }
            "await"
                if tail.first() == Some(&"agent")
                    && tail.get(1).is_some_and(|w| w.contains(',')) =>
            {
                if !self.agent_pushes {
                    return Ok(usage("await <idle|seq|match|gone|block>"));
                }
                let words: Vec<&str> = tail[1].split(',').collect();
                let zone = self.agent_zone;
                let word = |rows: &[String]| match worker_phase(
                    &rows[zone.map_or(0, |n| rows.len().saturating_sub(n))..],
                ) {
                    Phase::Busy => "busy",
                    Phase::Prompt => "prompt",
                    Phase::Question => "question",
                    Phase::Limited { .. } => "wall",
                    Phase::Idle => "idle",
                };
                while self.served < self.screens.len() {
                    if words.contains(&word(&self.screens[self.served])) {
                        let w = word(&self.screens[self.served]);
                        return Ok(ok(&format!("OK agent {w} rev=2\n")));
                    }
                    self.served += 1;
                    self.seq += 1;
                }
                Ok(self.stall(tail))
            }
            "await" if tail.first() == Some(&"agent") => {
                if let Some(p) = self.program_resolves_to.take() {
                    self.program = p;
                }
                Ok(if matches!(self.program, "claude" | "codex") {
                    ok("OK agent prompt rev=1\n")
                } else {
                    timeout()
                })
            }
            "await" if tail.first() == Some(&"gone") && self.card_debounce.is_some() => {
                self.reactive_index();
                if self.parked(tail) {
                    return Ok(err("exited"));
                }
                let matcher = tail.get(1).and_then(|p| aterm_observe::row_matcher(p).ok());
                Ok(self.reactive_wait(tail, |m| {
                    let i = m.reactive_at.unwrap_or(0);
                    matcher
                        .as_ref()
                        .is_none_or(|q| !m.screens[i].iter().any(|r| q.matches(r)))
                        .then(|| ok(&format!("OK gone {}\n", m.seq)))
                }))
            }
            "await" if tail.first() == Some(&"seq") && self.card_debounce.is_some() => {
                self.reactive_index();
                if self.parked(tail) {
                    return Ok(err("exited"));
                }
                let seen = tail.get(1).and_then(|n| n.parse::<u64>().ok()).unwrap_or(0);
                Ok(self.reactive_wait(tail, |m| (m.seq > seen).then(|| ok("OK seq\n"))))
            }
            "await" if tail.first() == Some(&"gone") => {
                if let Some(r) = self.replies.pop_front() {
                    return Ok(r);
                }
                if self.parked(tail) {
                    return Ok(err("exited"));
                }
                let moved_on = self.served < self.screens.len() && !self.held();
                let screen = if self.gone_on_next && moved_on {
                    &self.screens[self.served]
                } else {
                    self.last_served()
                };
                let span = match (tail.get(2), tail.get(3), tail.get(4)) {
                    (Some(&"rows"), Some(a), Some(b)) => {
                        let (Ok(a), Ok(b)) = (a.parse::<usize>(), b.parse::<usize>()) else {
                            return Ok(usage("await gone <re> [rows <a> <b>]"));
                        };
                        if a > b || a >= screen.len() {
                            return Ok(err(
                                "bad rows: the span meets no visible row (grid rows 0..=N)",
                            ));
                        }
                        a..=b.min(screen.len() - 1)
                    }
                    _ => 0..=screen.len().saturating_sub(1),
                };
                let clear = tail
                    .get(1)
                    .and_then(|p| aterm_observe::row_matcher(p).ok())
                    .is_none_or(|m| {
                        !screen
                            .iter()
                            .enumerate()
                            .any(|(i, r)| span.contains(&i) && m.matches(r))
                    });
                if clear || (!self.gone_on_next && moved_on) {
                    return Ok(ok(&format!("OK gone {}\n", self.seq)));
                }
                if self.gone_on_next {
                    self.gone_held += 1;
                    self.gone_ran_out = tail.get(1).is_some_and(|p| p.starts_with('^'));
                }
                Ok(self.stall(tail))
            }
            "await" if tail.first() == Some(&"seq") => {
                if let Some(r) = self.replies.pop_front() {
                    return Ok(r);
                }
                if self.keys > 0 && self.seq_waits_run_out > 0 {
                    self.seq_waits_run_out -= 1;
                    return Ok(timeout());
                }
                if self.parked(tail) {
                    return Ok(err("exited"));
                }
                let stale = tail
                    .get(1)
                    .and_then(|n| n.parse::<u64>().ok())
                    .is_some_and(|n| n > self.seq);
                if self.unmoved_wakes > 0
                    && self.attention_at.is_some()
                    && self.unmoved_for().is_some()
                {
                    self.unmoved_wakes -= 1;
                    return Ok(ok("OK seq\n"));
                }
                if let Some(left) = self.unmoved_for() {
                    let asked = tail
                        .iter()
                        .position(|a| *a == "timeout")
                        .and_then(|i| tail.get(i + 1))
                        .and_then(|ms| ms.parse::<u64>().ok())
                        .map_or(Duration::ZERO, Duration::from_millis);
                    run_nap(self.waits.as_deref(), asked.min(left));
                    if left > asked {
                        return Ok(timeout());
                    }
                }
                if self.held() {
                    let asked = tail
                        .iter()
                        .position(|a| *a == "timeout")
                        .and_then(|i| tail.get(i + 1))
                        .and_then(|ms| ms.parse::<u64>().ok())
                        .unwrap_or(0);
                    match &self.waits {
                        Some(clock) => {
                            let asked = Duration::from_millis(asked.max(1));
                            clock.skip(self.stall_sleep.map_or(asked, |nap| asked.min(nap)));
                        }
                        None => std::thread::sleep(Duration::from_millis(asked.min(2))),
                    }
                    return Ok(timeout());
                }
                if self.served < self.screens.len() && !stale {
                    return Ok(ok("OK seq\n"));
                }
                if self.countdown_ticks && !stale && self.countdown_left.is_some_and(|l| l > 0) {
                    return Ok(ok("OK seq\n"));
                }
                Ok(self.stall(tail))
            }
            "await"
                if tail.first() == Some(&"idle")
                    && self.idle_never_for.is_some()
                    && tail.get(1).copied() == self.idle_never_for =>
            {
                let asked = tail
                    .iter()
                    .position(|a| *a == "timeout")
                    .and_then(|i| tail.get(i + 1))
                    .and_then(|ms| ms.parse::<u64>().ok())
                    .unwrap_or(0);
                run_nap(self.waits.as_deref(), Duration::from_millis(asked));
                Ok(timeout())
            }
            "await" if tail.first() == Some(&"idle") && !self.idle_after.is_empty() => {
                let asked = tail
                    .iter()
                    .position(|a| *a == "timeout")
                    .and_then(|i| tail.get(i + 1))
                    .and_then(|ms| ms.parse::<u64>().ok())
                    .map_or(Duration::ZERO, Duration::from_millis);
                let still = self.idle_after.pop_front().unwrap_or_default();
                run_nap(self.waits.as_deref(), still.min(asked));
                Ok(if still <= asked {
                    ok("OK idle\n")
                } else {
                    timeout()
                })
            }
            "await"
                if tail.first() == Some(&"idle")
                    && self.tick_ms.is_some_and(|tick| {
                        tail.get(1)
                            .and_then(|ms| ms.parse::<u64>().ok())
                            .is_none_or(|ms| ms >= tick)
                    }) =>
            {
                let asked = tail
                    .iter()
                    .position(|a| *a == "timeout")
                    .and_then(|i| tail.get(i + 1))
                    .and_then(|ms| ms.parse::<u64>().ok())
                    .unwrap_or(0);
                run_nap(self.waits.as_deref(), Duration::from_millis(asked.min(50)));
                if self.redraws_move_gen {
                    self.seq += 1;
                }
                Ok(timeout())
            }
            "await" if tail.first() == Some(&"idle") && self.idle_never => {
                let asked = tail
                    .iter()
                    .position(|a| *a == "timeout")
                    .and_then(|i| tail.get(i + 1))
                    .and_then(|ms| ms.parse::<u64>().ok())
                    .unwrap_or(0);
                run_nap(self.waits.as_deref(), Duration::from_millis(asked.min(50)));
                Ok(timeout())
            }
            "await" if tail.first() == Some(&"idle") && self.idle_skips_busy => {
                if let Some(r) = self.replies.pop_front() {
                    return Ok(r);
                }
                while self.served + 1 < self.screens.len()
                    && worker_phase(&self.screens[self.served]) == Phase::Busy
                {
                    self.served += 1;
                    self.seq += 1;
                }
                Ok(ok(&self.idle_latched()))
            }
            "key" => {
                self.key_times
                    .push((line.clone(), run_now(self.waits.as_deref())));
                if self.card_debounce.is_some() {
                    self.reactive_index();
                }
                let screen = self.last_served().to_vec();
                self.key_probes.push(KeyProbe {
                    request: line.clone(),
                    card_age: survey_open(&screen).then(|| {
                        self.card_first_served
                            .map_or(Duration::ZERO, |f| run_since(self.waits.as_deref(), f))
                    }),
                    zero: screen.iter().any(|r| r == "❯ 0"),
                    draft: screen.iter().any(|r| r.starts_with("❯ ") && r != "❯ 0"),
                    boxed: worker_phase(&screen) == Phase::Prompt,
                    digit: self.digit,
                });
                if tail.last() == Some(&"backspace")
                    && let Some(r) = self.backspace_replies.pop_front()
                {
                    return Ok(r);
                }
                self.reset_probe(&line, tail.last().copied().unwrap_or(""));
                if tail.last() == Some(&"esc") {
                    self.panel_probe(&line);
                    if let Some(r) = self
                        .panel
                        .as_mut()
                        .filter(|p| p.at == PanelAt::Open)
                        .and_then(|p| p.esc_replies.pop_front())
                    {
                        return Ok(r);
                    }
                }
                if let (Some(tick), Some(read)) = (self.ticks, self.read_at)
                    && tail.iter().any(|a| a.starts_with("if-gen="))
                    && run_since(self.waits.as_deref(), read) >= tick
                {
                    self.seq += 1;
                    return Ok(ok(&format!("OK skipped reason=changed seq={}\n", self.seq)));
                }
                self.keyed = true;
                self.keys += 1;
                if self.tick_at_key > 0 {
                    self.tick_at_key -= 1;
                    self.seq += 1;
                }
                if self.person_at_key > 0
                    && let Some(n) = self.human_seq.as_mut()
                {
                    self.person_at_key -= 1;
                    *n += 1;
                    self.panel_person();
                    self.reset_person();
                }
                if self.swap_at_key.is_some() && self.swap_after_keys > 0 {
                    self.swap_after_keys -= 1;
                } else if let Some(swapped) = self.swap_at_key.take() {
                    let at = self.served.saturating_sub(1).min(self.screens.len() - 1);
                    self.screens[at] = swapped.clone();
                    self.screens.insert(at + 1, swapped);
                    self.seq += 1;
                }
                if let Some(r) = self.key_replies.pop_front() {
                    return Ok(r);
                }
                let mut tail = tail;
                if tail.first().is_some_and(|a| a.starts_with("act=")) {
                    tail = &tail[1..];
                }
                if let Some(fence) = tail.first().and_then(|a| a.strip_prefix("if-human=")) {
                    let Some(now) = self.human_seq else {
                        return Ok(err("usage: key [id=<key>] [if=<re>] [if-gen=<g>] <name>"));
                    };
                    if fence != now.to_string() {
                        self.person_skipped.push(self.order.len());
                        return Ok(ok(&format!("OK skipped reason=person seq={}\n", self.seq)));
                    }
                    tail = &tail[1..];
                }
                if tail.first().is_some_and(|a| a.starts_with("if-seq=")) {
                    return Ok(if self.gen_fence {
                        err("usage: key [id=<key>] [if=<re>] [if-gen=<g>] <name>")
                    } else {
                        bare_err()
                    });
                }
                if let Some(fence) = tail.first().and_then(|a| a.strip_prefix("if-gen=")) {
                    if !self.gen_fence {
                        return Ok(bare_err());
                    }
                    if fence != format!("1.{}", self.seq) {
                        self.gen_skipped += 1;
                        return Ok(ok(&format!("OK skipped reason=changed seq={}\n", self.seq)));
                    }
                    tail = &tail[1..];
                }
                let guard = tail.first().and_then(|a| a.strip_prefix("if="));
                let seq = self.seq;
                if let Some(pattern) = guard {
                    let Ok(matcher) = aterm_observe::row_matcher(pattern) else {
                        return Ok(err("badregex"));
                    };
                    if !self.last_served().iter().any(|r| matcher.matches(r)) {
                        return Ok(ok(&format!("OK skipped seq={seq}\n")));
                    }
                    if tail.last() == Some(&"0")
                        && let (Some(within), Some(first)) =
                            (self.card_ignores_within, self.card_first_served)
                        && run_since(self.waits.as_deref(), first) < within
                    {
                        self.early_zeroes += 1;
                    }
                    if tail.last() == Some(&"0") && self.card_debounce.is_some() {
                        let take = !self.card_drops_digits
                            && self.card_ignores_within.is_none_or(|w| {
                                self.card_first_served
                                    .is_none_or(|f| run_since(self.waits.as_deref(), f) >= w)
                            });
                        self.digit = Digit::Pending {
                            at: run_now(self.waits.as_deref()),
                            take,
                        };
                    }
                }
                if tail.last() == Some(&"backspace") && self.card_debounce.is_some() {
                    self.reactive_index();
                    if matches!(self.digit, Digit::Pending { .. } | Digit::Standing) {
                        self.digit = Digit::None;
                    }
                }
                self.panel_key(tail.last().copied().unwrap_or(""));
                if self.reset_key(tail.last().copied().unwrap_or("")) {
                    return Ok(closed());
                }
                Ok(ok(&format!("OK seq={seq}\n")))
            }
            "restart" => Ok(self
                .restart_replies
                .pop_front()
                .unwrap_or_else(|| ok("OK restarting pid 4242 cause=stall\n"))),
            "send" if self.gen_fence => {
                self.keyed = true;
                if self.first_send_at.is_none() {
                    self.first_send_at = Some(run_now(self.waits.as_deref()));
                    self.reads_at_send = Some(self.read_times.len());
                }
                let body = tail
                    .iter()
                    .position(|a| *a == "--")
                    .map(|i| tail[i + 1..].join(" "))
                    .unwrap_or_default();
                if body == crate::harness::usage::USAGE_COMMAND {
                    self.panel_probe(&line);
                }
                if let Some(r) = self.send_replies.pop_front() {
                    return Ok(r);
                }
                if self.person_at_send > 0
                    && let Some(n) = self.human_seq.as_mut()
                {
                    self.person_at_send -= 1;
                    *n += 1;
                    self.panel_person();
                }
                let opts: Vec<&str> = tail
                    .iter()
                    .copied()
                    .take_while(|a| *a != "--" && a.contains('='))
                    .collect();
                let opt = |name: &str| opts.iter().find_map(|a| a.strip_prefix(name));
                if let Some(fence) = opt("if-human=") {
                    let Some(now) = self.human_seq else {
                        return Ok(err("usage: send [if-gen=<g>] [if=<re>] [--] <text>"));
                    };
                    if fence != now.to_string() {
                        self.person_skipped.push(self.order.len());
                        return Ok(ok(&format!("OK skipped reason=person seq={}\n", self.seq)));
                    }
                }
                if let Some(fence) = opt("if-gen=")
                    && fence != format!("1.{}", self.seq)
                {
                    return Ok(ok(&format!("OK skipped reason=changed seq={}\n", self.seq)));
                }
                if let Some(pattern) = opt("if=") {
                    let Ok(matcher) = aterm_observe::row_matcher(pattern) else {
                        return Ok(err("badregex"));
                    };
                    if !self.last_served().iter().any(|r| matcher.matches(r)) {
                        return Ok(ok(&format!("OK skipped seq={}\n", self.seq)));
                    }
                }
                let written = ok(&format!("OK seq={}\n", self.seq));
                self.panel_sent(&body);
                if self.person_after_send > 0
                    && let Some(n) = self.human_seq.as_mut()
                {
                    self.person_after_send -= 1;
                    *n += 1;
                    self.panel_person();
                }
                Ok(written)
            }
            "history" => Ok(self
                .history
                .clone()
                .unwrap_or_else(|| err("unknown verb (try: help)"))),
            "offscreen" if self.offscreen.is_empty() && self.alt_archive.is_some() => {
                Ok(self.archive_read(tail))
            }
            "offscreen" => Ok(self
                .offscreen
                .pop_front()
                .unwrap_or_else(|| err("unknown verb (try: help)"))),
            "meta" if tail.is_empty() => {
                self.metas += 1;
                for &(n, word) in &self.questions_at {
                    if self.metas == n {
                        self.questions = word;
                    }
                }
                for &(n, word) in &self.autopilot_at {
                    if self.metas == n {
                        self.autopilot = word;
                    }
                }
                Ok(ok(&self.meta_line()))
            }
            "status" => {
                let line = self.status_line();
                Ok(ok(&line))
            }
            "title" if tail.is_empty() && self.title_refused => {
                Ok(err("unknown verb 'title' (help lists them)"))
            }
            "title" if tail.is_empty() => {
                if let Some(title) = self.titles.pop_front() {
                    self.title = title;
                }
                Ok(ok(&format!(
                    "OK {}\n",
                    self.title.as_deref().unwrap_or_default()
                )))
            }
            "help" if tail.first() == Some(&"send") && self.help_send.is_some() => {
                Ok(ok(&self.help_send.clone().unwrap_or_default()))
            }
            "help" => Ok(ok(&self.help.clone())),
            "who" if self.who.is_some() => Ok(ok(self.who.as_deref().unwrap_or_default())),
            "search" if self.scrollback.is_some() => Ok(self.scrollback_search(tail)),
            "line" if self.scrollback.is_some() => Ok(self.scrollback_line(tail)),
            "cell" if self.cell_unreadable => Ok(err("no such verb: cell")),
            "cell" if self.measured_cell(tail).is_some() => Ok(ok(&format!(
                "{}\n",
                self.measured_cell(tail).unwrap_or_default()
            ))),
            "cell" => {
                let shown = self.served.saturating_sub(1).min(self.screens.len() - 1);
                let attrs = self
                    .cell_attrs_on
                    .get(&shown)
                    .copied()
                    .unwrap_or(self.cell_attrs);
                Ok(ok(&format!("OK %20 {} 111318 {attrs}\n", self.cell_fg)))
            }
            "meta" if tail.get(2) == Some(&"owner=needs") => {
                self.needs_calls.push(tail.join(" "));
                let r = self.needs_replies.pop_front().unwrap_or_else(|| ok("OK\n"));
                if r.ok() {
                    match tail.first() {
                        Some(&"set") => self.needs_attention = Some(tail[4..].join(" ")),
                        Some(&"unset") => self.needs_attention = None,
                        _ => {}
                    }
                }
                Ok(r)
            }
            "meta" => {
                if tail.first() == Some(&"set") && tail.get(1) == Some(&"attention") {
                    self.attentions += 1;
                }
                let r = self.replies.pop_front().unwrap_or_else(|| ok("OK\n"));
                if r.ok() && tail.get(1) == Some(&"attention") {
                    let keyed = tail.get(2) == Some(&"owner=supervisor");
                    let kinded = keyed && tail.get(3).is_some_and(|t| t.starts_with("kind="));
                    let text = tail[if kinded {
                        4
                    } else if keyed {
                        3
                    } else {
                        2
                    }..]
                        .join(" ");
                    if keyed && tail.first() == Some(&"set") {
                        self.attention_kind = kinded.then(|| tail[3].to_string());
                    }
                    match (tail.first(), keyed) {
                        (Some(&"set"), true) => {
                            self.attention_at
                                .get_or_insert_with(|| run_now(self.waits.as_deref()));
                            if let Some(word) = self.questions_after_badge.take() {
                                self.questions = word;
                            }
                            self.attention = Some(text);
                            self.human_on_top = false;
                            if let Some(f) = self.foreign_attention.take() {
                                self.human_attention = Some(f);
                                self.human_on_top = true;
                            }
                        }
                        (Some(&"unset"), true) => self.attention = None,
                        (Some(&"set"), false) => {
                            self.human_attention = Some(text);
                            self.human_on_top = true;
                        }
                        (Some(&"unset"), false) => self.human_attention = None,
                        _ => {}
                    }
                }
                Ok(r)
            }
            "await" if tail.first() == Some(&"idle") => {
                let latched = self.idle_latched();
                Ok(self.replies.pop_front().unwrap_or_else(|| ok(&latched)))
            }
            _ => Ok(self.replies.pop_front().unwrap_or_else(|| ok("OK\n"))),
        }
    }
}

impl Ctl for Mock {
    fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
        assert!(
            self.hang_at.is_none_or(|at| Instant::now() < at),
            "timed out after {:?} waiting for a run on the run's clock to end",
            aterm_spec::wait::MUST_HAPPEN
        );
        if let Some(clock) = self.waits.as_deref() {
            clock.skip(self.round_trip);
        }
        let r = self.answer_call(args);
        if let Some(after) = self.after_call.as_mut() {
            after();
        }
        r
    }

    fn call_at_host_of(&mut self, sid: &str, args: &[&str]) -> Result<CtlReply, String> {
        self.aimed.push(sid.to_string());
        self.call(args)
    }
}

impl Mock {
    fn answer_call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
        let words: Vec<&str> = args
            .iter()
            .copied()
            .filter(|a| !a.starts_with('@'))
            .collect();
        if self.hung_up
            || self
                .hang_up_at
                .is_some_and(|at| words.join(" ").starts_with(at))
        {
            self.hung_up = true;
            self.order.push((args.join(" "), false));
            return Err("the host hung the loop's connection up".to_string());
        }
        if words.len() >= 3 && words[0] == "meta" && words[2] == "supervisor" {
            self.claims.push(args.join(" "));
            let r = self.claim_replies.pop_front().unwrap_or_else(|| ok("OK\n"));
            self.order.push((args.join(" "), r.ok()));
            return Ok(r);
        }
        if words.first() == Some(&"task") {
            self.task_requests.push(words.join(" "));
            let r = self.task_call(&words);
            self.order.push((args.join(" "), r.ok()));
            return Ok(r);
        }
        let r = self.call_counted(args)?;
        self.order.push((args.join(" "), r.ok()));
        Ok(r)
    }
}

impl Mock {
    fn call_counted(&mut self, args: &[&str]) -> Result<CtlReply, String> {
        let first_meta = self.requests.is_empty()
            && args
                .iter()
                .filter(|a| !a.starts_with('@'))
                .eq(["meta"].iter());
        let at = self.requests.len() - self.uncounted;
        if first_meta {
            self.uncounted = 1;
        } else {
            if let Some(pause) = self.delay.remove(&at) {
                run_nap(self.waits.as_deref(), pause);
            }
            if let Some((i, seq)) = self.restart
                && i == at
            {
                self.seq = seq;
                self.restart = None;
            }
        }
        let verb = args
            .iter()
            .find(|a| !a.starts_with('@'))
            .copied()
            .unwrap_or("");
        let down = self
            .down
            .as_ref()
            .filter(|(from, _)| at >= *from)
            .map(|(_, r)| r.clone());
        let scripted = if first_meta {
            down
        } else {
            self.by_index.remove(&at).or(down).or_else(|| {
                (verb == "await" && self.drop_awaits_from.is_some_and(|d| at >= d)).then(closed)
            })
        };
        let reply = match scripted {
            Some(r) => {
                self.requests.push(args.join(" "));
                r
            }
            None => self.serve(args)?,
        };
        if reply.lost()
            && let Some(seq) = self.handoff_seq.take()
        {
            self.seq = seq;
        }
        Ok(reply)
    }
}

const WATCHING: usize = 3;
const UNREACHED: usize = 200;

struct HostSide<'a> {
    inner: &'a mut Mock,
    stop: Arc<AtomicBool>,
    wake: Option<mpsc::Sender<()>>,
    waits: usize,
    on_last: Option<usize>,
    after_stop: usize,
}

impl Ctl for HostSide<'_> {
    fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
        if args.iter().find(|a| !a.starts_with('@')) == Some(&"await") {
            if self.stop.load(Ordering::SeqCst) {
                self.after_stop += 1;
                if self.after_stop > 1 {
                    return Err("a second wait begun after the host's stop".to_string());
                }
            }
            self.waits += 1;
            let watching = self.on_last.is_some_and(|at| self.waits - at >= WATCHING);
            if (watching || self.waits >= UNREACHED)
                && let Some(wake) = self.wake.take()
            {
                let _ = wake.send(());
            }
        }
        let r = self.inner.call(args);
        if self.on_last.is_none() && self.inner.served >= self.inner.screens.len() {
            self.on_last = Some(self.waits);
        }
        r
    }
}

fn host_stops(
    m: &mut Mock,
    sid: Option<&str>,
    ledger: PathBuf,
    opts: &SuperviseOpts,
    hand_over: bool,
) -> (Result<(), String>, Vec<String>) {
    let (r, lines, _) = host_stops_taped(m, sid, ledger, opts, hand_over);
    (r, lines)
}
fn host_stops_taped(
    m: &mut Mock,
    sid: Option<&str>,
    ledger: PathBuf,
    opts: &SuperviseOpts,
    hand_over: bool,
) -> (Result<(), String>, Vec<String>, Vec<u8>) {
    m.stall_sleep = Some(Duration::from_millis(5));
    let from = m.order.len();
    let stop = Arc::new(AtomicBool::new(false));
    let handover = Arc::new(AtomicBool::new(false));
    let (wake, woken) = mpsc::channel::<()>();
    let (flag, over) = (Arc::clone(&stop), Arc::clone(&handover));
    let host = std::thread::spawn(move || {
        if woken.recv().is_ok() {
            over.store(hand_over, Ordering::SeqCst);
            flag.store(true, Ordering::SeqCst);
        }
    });
    let mut side = HostSide {
        inner: m,
        stop: Arc::clone(&stop),
        wake: Some(wake),
        waits: 0,
        on_last: None,
        after_stop: 0,
    };
    let mut out: Vec<u8> = Vec::new();
    let tape = MemoryTape::over(&Fx::os());
    let header;
    let r = {
        let mut taped = TapedCtl::new(
            &mut side,
            tape.fx.clone(),
            crate::supervise::tape::doors::CTL,
        );
        let mut s = Session::new(&mut taped, sid.map(str::to_string));
        s.set_fx(tape.fx.clone());
        s.set_box_settle(Duration::ZERO);
        s.set_handover(handover);
        s.set_approval_ledger(Some(ledger));
        header = hosted_header(&s, opts);
        tape.core().reroot(header.roots.clone());
        s.run_hosted(opts, stop, &mut out)
    };
    let read = tape.finish(&crate::supervise::tape::replay::hosted_end(&r));
    let raised = |door: &str| {
        read.items
            .iter()
            .filter(|i| {
                matches!(i.record(), Some(astream_effects::EffectRecord::Raised { door: d, .. }) if d == door)
            })
            .count()
    };
    let stopped = String::from_utf8_lossy(&out).ends_with("EXIT stopped\n");
    assert_eq!(
        raised(crate::supervise::tape::doors::RAISED_STOP),
        usize::from(stopped),
        "the stop's first true read is on the tape once, when the host stopped the loop"
    );
    assert_eq!(
        raised(crate::supervise::tape::doors::RAISED_HANDOVER),
        usize::from(stopped && hand_over),
        "the hand-over is read once, after the stop, and taped when it was raised"
    );
    side.wake = None;
    host.join().expect("the host's thread");
    assert!(
        side.after_stop <= 1,
        "{} waits begun after the host's stop: {:#?}",
        side.after_stop,
        side.inner.requests
    );
    let lines: Vec<String> = String::from_utf8(out)
        .expect("utf-8")
        .lines()
        .map(str::to_string)
        .collect();
    crate::supervise::tape::testing::assert_replays(
        &tape,
        &header,
        &sent(side.inner, from),
        &lines,
    );
    let bytes = tape.reframed(&header.encode());
    (r, lines, bytes)
}
fn hosted_header<C: Ctl>(
    s: &Session<'_, C>,
    opts: &SuperviseOpts,
) -> crate::supervise::tape::header::Header {
    crate::supervise::tape::header::Header::capture(
        crate::supervise::tape::testing::TAPE_ID,
        crate::supervise::tape::header::Entry::Hosted,
        None,
        s.setup(),
        opts,
    )
}

struct StopInWait<'a> {
    inner: &'a mut Mock,
    stop: Arc<AtomicBool>,
    arm_at: usize,
    waits: usize,
    armed_in: Option<String>,
    after: Vec<String>,
}

impl Ctl for StopInWait<'_> {
    fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
        let line = args.join(" ");
        if self.stop.load(Ordering::SeqCst) {
            self.after.push(line.clone());
        }
        if args.iter().find(|a| !a.starts_with('@')) == Some(&"await") {
            self.waits += 1;
            if self.waits == self.arm_at {
                self.stop.store(true, Ordering::SeqCst);
                self.armed_in = Some(line);
            }
        }
        self.inner.call(args)
    }
}

const SWEEP_CAP: usize = 80;

fn stop_in_every_wait(
    tag: &str,
    opts: &SuperviseOpts,
    sid: Option<&str>,
    script: impl Fn() -> Mock,
) -> Vec<String> {
    let ledger = std::env::temp_dir().join(format!(
        "aterm-stop-sweep-{tag}-{}.jsonl",
        std::process::id()
    ));
    let mut armed = Vec::new();
    for k in 1..=SWEEP_CAP {
        let mut m = script();
        let clock = virtual_clock();
        m.run_on(&clock);
        let stop = Arc::new(AtomicBool::new(false));
        let mut side = StopInWait {
            inner: &mut m,
            stop: Arc::clone(&stop),
            arm_at: k,
            waits: 0,
            armed_in: None,
            after: Vec::new(),
        };
        let mut out: Vec<u8> = Vec::new();
        let tape = MemoryTape::over(&Fx::over(ClockSource::Scripted(clock)));
        let r = {
            let mut taped = TapedCtl::new(
                &mut side,
                tape.fx.clone(),
                crate::supervise::tape::doors::CTL,
            );
            let mut s = Session::new(&mut taped, sid.map(str::to_string));
            s.set_fx(tape.fx.clone());
            s.set_box_settle(Duration::ZERO);
            s.set_clock(TEST_NOW, PDT);
            s.set_zone_lookup(test_zones);
            s.set_approval_ledger(Some(ledger.clone()));
            s.run_hosted(opts, stop, &mut out)
        };
        let _ = std::fs::remove_file(&ledger);
        let read = tape.finish("hosted");
        let at = |pick: &dyn Fn(&astream_effects::EffectRecord) -> bool| {
            read.items
                .iter()
                .filter_map(crate::supervise::tape::codec::Item::record)
                .enumerate()
                .filter(|(_, r)| pick(r))
                .map(|(n, _)| n)
                .collect::<Vec<usize>>()
        };
        let stops = at(&|r| {
            matches!(r, astream_effects::EffectRecord::Raised { door, .. }
                if door == crate::supervise::tape::doors::RAISED_STOP)
        });
        if side.armed_in.is_some() {
            let waits = at(&|r| match r {
                astream_effects::EffectRecord::Exchange { door, request, .. }
                    if door == crate::supervise::tape::doors::CTL =>
                {
                    crate::supervise::tape::codec::ctl_args(request).is_ok_and(|w| {
                        w.iter()
                            .find(|a| !a.starts_with('@'))
                            .is_some_and(|v| v == "await")
                    })
                }
                _ => false,
            });
            assert_eq!(stops.len(), 1, "{tag}: stop in wait {k}: one record of it");
            assert!(
                waits.get(k - 1).is_some_and(|&w| w < stops[0]),
                "{tag}: stop in wait {k}: recorded after the wait it was set in ({waits:?}, {stops:?})"
            );
        }
        let lines: Vec<String> = String::from_utf8(out)
            .expect("utf-8")
            .lines()
            .map(str::to_string)
            .collect();
        let Some(wait) = side.armed_in.take() else {
            assert!(k > 1, "{tag}: the run began no wait: {lines:#?}");
            return armed;
        };
        let late: Vec<&String> = side
            .after
            .iter()
            .filter(|r| {
                let words: Vec<&str> = r.split(' ').collect();
                words.iter().find(|w| !w.starts_with('@')) == Some(&"await")
                    || claim::is_input(&words)
            })
            .collect();
        assert!(
            late.is_empty(),
            "{tag}: the stop set in wait {k} ({wait}), then {late:#?}\n{:#?}",
            side.inner.requests
        );
        assert_eq!(r, Ok(()), "{tag}: stop in wait {k} ({wait}): {lines:#?}");
        assert_eq!(
            lines.last().map(String::as_str),
            Some("EXIT stopped"),
            "{tag}: stop in wait {k} ({wait})"
        );
        armed.push(wait);
    }
    panic!("{tag}: the script never ended on its own in {SWEEP_CAP} waits: {armed:#?}");
}

fn fenced(screens: Vec<Vec<String>>) -> Mock {
    let mut m = Mock::new(screens);
    m.gen_fence = true;
    m.sends_gen = true;
    m.human_seq = Some(7);
    m.human = vec!["null"];
    m.help = "key [id=<key>] [if=<re>] [if-gen=<e.s>] [if-human=<n>] <name>: send a named key\n"
        .to_string();
    m
}

const G_PRESS: &str = "key if=^\\x20{3}git\\x20log\\x20--oneline\\x20-5\\s*$ 1";

fn busy_screen() -> Vec<String> {
    let mut r = rows(&["⏺ Working.", "", "✻ Synthesizing… (18s)", ""]);
    r.extend(composer("  esc to interrupt"));
    r
}

#[test]
fn a_busy_row_the_guard_could_not_read_is_still_up() {
    let mut m = Mock::new(Vec::new());
    let mut s = session(&mut m, None);
    let guard = "(?:x?){1020}z";
    let unread = format!("{}z", "x".repeat(4000));
    assert!(s.busy_up(guard, &[unread]), "an unread row is not gone");
    assert!(
        s.busy_up(guard, &["xxxz".to_string()]),
        "a read match is up"
    );
    assert!(
        !s.busy_up(guard, &["Done.".to_string()]),
        "a read clear row is down"
    );
    assert!(s.busy_up("esc to interrupt", &busy_screen()));
    assert!(!s.busy_up("esc to interrupt", &idle_screen()));
}

#[test]
fn each_agents_busy_guard_is_up_on_its_busy_row_alone() {
    use aterm_phase::prompt::fixtures::screen;
    use aterm_phase::{agy::fixtures as a, gemini::fixtures as g, muse::fixtures as u};
    let mut m = Mock::new(Vec::new());
    let mut s = session(&mut m, None);
    let cases: [(Program, &[&str], &[&str]); 3] = [
        (
            Program::Agy,
            &[a::BUSY, a::BUSY_TIP],
            &[
                a::IDLE,
                a::DRAFT,
                a::END_OF_TURN,
                a::BOX_EXEC,
                a::QUESTION,
                a::SLASH_MENU,
            ],
        ),
        (
            Program::Muse,
            &[
                u::BUSY,
                u::BUSY_1_4_10,
                u::RATE_LIMIT_RETRY,
                u::SERVER_ERROR_RETRY,
                u::BOX_SHELL,
                u::BOX_WRITE,
            ],
            &[
                u::IDLE,
                u::DRAFT,
                u::END_OF_TURN,
                u::SLASH_MENU,
                u::END_OF_TURN_1_4_10,
                u::INTERRUPTED_1_4_10,
                u::KEY_REJECTED,
                u::USAGE_WALL,
                u::QUESTION,
            ],
        ),
        (
            Program::Gemini,
            &[
                g::BUSY_THINKING,
                g::BUSY_STREAMING,
                g::RETRY_503,
                g::RETRY_500,
            ],
            &[
                g::IDLE,
                g::DRAFT,
                g::END_OF_TURN,
                g::INTERRUPTED,
                g::SLASH_MENU,
                g::BOX_SHELL,
                g::BOX_WRITE,
                g::QUESTION,
                g::LIMIT_DIALOG,
                g::TRUST,
                g::ERROR_401,
            ],
        ),
    ];
    for (program, busy, idle) in cases {
        let guard = program.reader().busy_guard().expect("a busy row");
        assert!(
            aterm_observe::row_matcher(&guard).is_ok(),
            "{program:?}: {guard}"
        );
        for text in busy {
            assert!(s.busy_up(&guard, &screen(text)), "{program:?} busy: {text}");
        }
        for text in idle {
            assert!(
                !s.busy_up(&guard, &screen(text)),
                "{program:?} idle: {text}"
            );
        }
    }
}
fn idle_screen() -> Vec<String> {
    let mut r = rows(&["⏺ Done.", "", "✻ Cogitated for 4s · done 2:41 PM", ""]);
    r.extend(composer("  ? for shortcuts"));
    r
}
fn write_prompt() -> Vec<String> {
    let mut r = bash_one_row();
    r[4] = "   rm -rf target".to_string();
    r
}
struct Book {
    dir: PathBuf,
    path: PathBuf,
}

impl Book {
    fn new(tag: &str) -> Self {
        let dir = crate::supervise::test_scratch_path("book", tag);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("tmp dir");
        let path = dir.join("s-1.jsonl");
        Self { dir, path }
    }

    fn keep<C: Ctl>(&self, s: &mut Session<'_, C>) {
        s.set_approval_ledger(Some(self.path.clone()));
    }

    fn rows(&self) -> Vec<(String, String, String)> {
        std::fs::read_to_string(&self.path)
            .unwrap_or_default()
            .lines()
            .filter_map(|row| {
                let v = aterm_json::from_str::<aterm_json::Value>(row).ok()?;
                let field = |k: &str| {
                    v.get(k)
                        .and_then(aterm_json::Value::as_str)
                        .unwrap_or("?")
                        .to_string()
                };
                (field("rule_id") != crate::supervise::approvals::RULE_TURN_RAN)
                    .then(|| (field("decision"), field("command"), field("reason")))
            })
            .collect()
    }

    fn decided(&self) -> Vec<String> {
        self.rows()
            .into_iter()
            .map(|(decision, command, _)| format!("{decision} {command}"))
            .collect()
    }

    fn escalated(&self) -> Vec<String> {
        self.rows()
            .into_iter()
            .filter(|(decision, ..)| decision == "escalated")
            .map(|(.., reason)| reason)
            .collect()
    }
}

impl Drop for Book {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn auto(max_s: u64) -> SuperviseOpts {
    SuperviseOpts {
        max: Duration::from_secs(max_s),
        python_allow: vec![],
        context_warn: 10,
        journal: None,
        ledger: None,
        policy: SupervisorConfig {
            approve: Approve::Safe,
            trust_roots: vec!["/private/tmp/claude-502/scratch/work*".to_string()],
            rm_breaker_ask: true,
            dismiss_surveys: false,
            answer_questions: false,
            continue_policy: false,
            retry_api_errors: false,
            resume_limits: false,
            model_fallback: None,
            compact_on_context_wall: false,
            ..owners_policy()
        },
        yield_when_held: false,
        idle_host: None,
        tasks: None,
    }
}

fn owners_policy() -> SupervisorConfig {
    SupervisorConfig {
        continue_text: "keep going".to_string(),
        ..SupervisorConfig::default()
    }
}

fn powerless() -> SupervisorConfig {
    let mut p = SupervisorConfig::default();
    for key in super::super::config::KEYS {
        if *key != "autopilot" {
            p.limit(key);
        }
    }
    p
}

fn surveys(mut opts: SuperviseOpts, on: bool) -> SuperviseOpts {
    opts.policy.dismiss_surveys = on;
    opts
}

fn resuming(opts: &SuperviseOpts, rules: Option<PathBuf>) -> SuperviseOpts {
    let mut o = opts.clone();
    o.policy.resume_limits = true;
    o.policy.rules_file = rules;
    o
}

#[test]
fn a_dead_footer_above_the_live_zone_holds_no_wait() {
    use aterm_phase::prompt::fixtures as f;
    let busy = f::screen(f::EXIT_RELAUNCH_STALE_FOOTER_BUSY);
    let dialog = f::screen(f::EXIT_RELAUNCH_STALE_FOOTER_QUESTION2);
    let mut m = Mock::new(vec![busy, dialog]);
    m.gone_on_next = true;
    let mut s = session(&mut m, Some("@s-1".to_string()));
    let t = s.await_turn(Duration::from_secs(60)).expect("turn");
    assert_eq!(t.phase, Phase::Prompt);
    assert_eq!(m.gone_held, 1, "{:#?}", m.requests);
    assert_eq!(
        m.requests,
        [
            "@s-1 await gone esc.to.interrupt timeout 3000",
            "@s-1 text --json tail=40",
            "@s-1 await gone esc.to.interrupt rows 33 49 timeout 20000",
            "@s-1 text --json tail=40",
        ]
    );
}

#[test]
fn a_box_under_a_dead_footer_is_read_again_at_once() {
    use aterm_phase::prompt::fixtures as f;
    let dialog = f::screen(f::EXIT_RELAUNCH_STALE_FOOTER_QUESTION2);
    let mut m = Mock::new(vec![dialog.clone(), dialog]);
    m.gone_on_next = true;
    let mut s = session(&mut m, None);
    let t = s
        .await_turn(Duration::from_secs(60))
        .expect("the first look");
    assert_eq!(t.phase, Phase::Prompt);
    let mut moved = Saw::default();
    let t = s
        .await_turn_from(
            Duration::from_secs(60),
            true,
            &mut moved,
            None,
            None,
            &mut Alone,
        )
        .expect("the look again");
    assert_eq!(t.phase, Phase::Prompt);
    assert_eq!(m.gone_held, 1, "{:#?}", m.requests);
    assert_eq!(
        m.requests[2],
        "await gone esc.to.interrupt rows 33 49 timeout 20000"
    );
}

#[test]
fn a_tail_whose_live_zone_starts_at_its_first_row_is_still_scoped() {
    use aterm_phase::prompt::fixtures as f;
    let padded = |text: &str| {
        let mut rows = f::screen(text);
        rows.resize(73, String::new());
        rows
    };
    let busy = padded(f::EXIT_RELAUNCH_STALE_FOOTER_BUSY);
    let dialog = padded(f::EXIT_RELAUNCH_STALE_FOOTER_QUESTION2);
    let run = |screens: Vec<Vec<String>>| {
        let mut m = Mock::new(screens);
        m.gone_on_next = true;
        m.cursor_on_caret = true;
        let mut s = session(&mut m, None);
        let t = s.await_turn(Duration::from_secs(60)).expect("turn");
        assert_eq!(t.phase, Phase::Prompt, "{:#?}", m.requests);
        m
    };
    let m = run(vec![busy.clone(), dialog.clone()]);
    assert_eq!(
        m.requests[..3],
        [
            "await gone esc.to.interrupt timeout 3000",
            "text --json tail=40",
            "await gone esc.to.interrupt rows 33 72 timeout 20000",
        ],
        "{:#?}",
        m.requests
    );
    assert_eq!(m.gone_held, 1, "{:#?}", m.requests);
    let m = run(vec![busy.clone(), busy, dialog]);
    assert_eq!(
        m.requests[2],
        "await gone esc.to.interrupt rows 33 72 timeout 20000"
    );
    assert_eq!(m.gone_held, 2, "{:#?}", m.requests);
}

#[test]
fn a_span_the_grid_no_longer_has_is_read_again() {
    use aterm_phase::prompt::fixtures as f;
    let busy = f::screen(f::EXIT_RELAUNCH_STALE_FOOTER_BUSY);
    let small: Vec<String> = f::screen(f::EXIT_RELAUNCH_STALE_FOOTER)[20..].to_vec();
    assert_eq!(small.len(), 30);
    let mut m = Mock::new(vec![busy, small]);
    m.gone_on_next = true;
    let mut s = session(&mut m, None);
    let t = s.await_turn(Duration::from_secs(60)).expect("turn");
    assert_eq!(t.phase, Phase::Idle, "{:#?}", m.requests);
    assert_eq!(
        m.requests[2..],
        [
            "await gone esc.to.interrupt rows 33 49 timeout 20000",
            "text --json tail=40",
        ]
    );
}

#[test]
fn an_idle_read_torn_mid_repaint_is_read_again_after_the_settle() {
    let footer = "  ⏵⏵ auto mode on (shift+tab to cycle)";
    let streaming = |first: usize, last: usize| {
        let mut r = if first == 1 {
            rows(&[
                "⏺ Here is the landing report.",
                "  NEEDS YOU: rotate the deploy key",
                "",
            ])
        } else {
            Vec::new()
        };
        r.extend((first..=last).map(|i| format!("  - check {i} passed")));
        r.push(String::new());
        r.extend(composer(&format!("{footer} · esc to interrupt")));
        r
    };
    let torn = |bottom: usize| {
        let mut r: Vec<String> = (30..=65).map(|i| format!("  - check {i} passed")).collect();
        r.extend([
            String::new(),
            "─".repeat(120),
            "❯".to_string(),
            "─".repeat(bottom),
        ]);
        r
    };
    let mut end: Vec<String> = (75..=80).map(|i| format!("  - check {i} passed")).collect();
    end.extend(rows(&[
        "  Nothing left: the parser is committed and pushed to origin/main.",
        "",
        "✻ Worked for 5s · done 6:12 PM",
        "",
    ]));
    end.extend(composer(footer));
    let run = |screens: Vec<Vec<String>>| {
        let mut m = Mock::new(screens);
        m.gone_on_next = true;
        let mut s = session(&mut m, None);
        let t = s.await_turn(Duration::from_secs(60)).expect("turn");
        (t, m.requests)
    };
    for bottom in [90, 120] {
        let cut = torn(bottom);
        assert_eq!(
            worker_phase(&cut),
            Phase::Idle,
            "every reader reads it idle"
        );
        assert!(has_composer_frame(&cut));
        let (t, requests) = run(vec![streaming(1, 21), cut, streaming(36, 70), end.clone()]);
        assert_eq!(t.phase, Phase::Idle, "{bottom}: {requests:#?}");
        assert_eq!(
            t.screen.rows, end,
            "{bottom}: the reply's end decides, never the torn read: {requests:#?}"
        );
        assert_eq!(
            requests[2..6],
            [
                "await gone esc.to.interrupt rows 1 28 timeout 20000",
                "text --json tail=40",
                "await idle 500 timeout 1500",
                "text --json tail=40",
            ],
            "{bottom}: {requests:#?}"
        );
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.starts_with("await idle"))
                .count(),
            1,
            "{bottom}: the end's whole frame takes no settle: {requests:#?}"
        );
    }
    let (t, requests) = run(vec![streaming(1, 21), end.clone()]);
    assert_eq!(
        (t.phase, &t.screen.rows),
        (Phase::Idle, &end),
        "{requests:#?}"
    );
    assert_eq!(
        requests[2..],
        [
            "await gone esc.to.interrupt rows 1 28 timeout 20000",
            "text --json tail=40",
        ],
        "{requests:#?}"
    );
}

#[test]
fn a_read_still_open_after_the_rereads_is_output_arriving() {
    let footer = "  ⏵⏵ auto mode on (shift+tab to cycle)";
    let reply = |last: usize, busy: bool| {
        let mut r: Vec<String> = (1..=last)
            .map(|i| format!("  - check {i} passed"))
            .collect();
        r.push(String::new());
        r.extend(composer(&if busy {
            format!("{footer} · esc to interrupt")
        } else {
            footer.to_string()
        }));
        r
    };
    let mid = reply(12, false);
    assert_eq!(
        worker_phase(&mid),
        Phase::Idle,
        "every reader reads it idle"
    );
    assert!(
        !aterm_phase::Program::Claude.reader().frame_torn(&mid),
        "and no tear in its frame"
    );
    let mut end = reply(20, false);
    end.insert(20, "  Nothing left: the parser is committed.".to_string());
    let reads = 1 + FRAME_REREADS as usize;
    let run = |screens: Vec<Vec<String>>, open: bool| {
        let mut m = Mock::new(screens);
        m.gone_on_next = true;
        if open {
            m.open_on = (1..=reads).collect();
        }
        let mut s = session(&mut m, None);
        let t = s.await_turn(Duration::from_secs(60)).expect("turn");
        (t, m.requests)
    };
    let mut screens = vec![reply(4, true)];
    screens.extend(std::iter::repeat_n(mid.clone(), reads));
    screens.extend([reply(16, true), end.clone()]);
    let (t, requests) = run(screens, true);
    assert_eq!(
        (t.phase, &t.screen.rows),
        (Phase::Idle, &end),
        "the reply's end decides, never the open read: {requests:#?}"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.starts_with("await frame"))
            .count(),
        FRAME_REREADS as usize,
        "{requests:#?}"
    );
    let (t, requests) = run(
        vec![reply(4, true), mid.clone(), reply(16, true), end],
        false,
    );
    assert_eq!(
        (t.phase, &t.screen.rows),
        (Phase::Idle, &mid),
        "{requests:#?}"
    );
    assert!(
        !requests.iter().any(|r| r.starts_with("await frame")),
        "{requests:#?}"
    );
}

#[test]
fn an_idle_read_with_no_evidence_is_no_point_until_it_holds_still() {
    use aterm_phase::muse::fixtures as u;
    use aterm_phase::prompt::fixtures::screen;
    let cycled = screen(u::BUSY_FIRST_FRAME_CYCLED);
    let busy = screen(u::BUSY_1_4_10);
    let end = screen(u::END_OF_TURN_1_4_10);
    let reader = aterm_phase::Program::Muse.reader();
    assert_eq!(reader.phase(&cycled), Phase::Idle);
    assert!(
        !reader.phase_authoritative(&cycled, &Phase::Idle),
        "no evidence"
    );
    assert_eq!(reader.phase(&busy), Phase::Busy);
    assert!(
        reader.phase_authoritative(&end, &Phase::Idle),
        "the end is evidence"
    );
    let run_idle = |screens: Vec<Vec<String>>, idle_after: &[Duration]| {
        let mut m = Mock::new(screens);
        m.gone_on_next = true;
        m.stall_sleep = Some(Duration::from_secs(60));
        m.idle_after = idle_after.iter().copied().collect();
        let clock = virtual_clock();
        m.run_on(&clock);
        let mut s = session(&mut m, None);
        s.set_fx(Fx::over(ClockSource::Scripted(Arc::clone(&clock))));
        let started = s.fx.mono();
        let t = s.await_turn(Duration::from_secs(60)).expect("turn");
        let took = s.fx.since(started);
        (t, m.requests, took)
    };
    let run = |screens: Vec<Vec<String>>| run_idle(screens, &[]);
    let after_first_read = |requests: &[String]| {
        let first = requests
            .iter()
            .position(|r| r.starts_with("text"))
            .expect("a read");
        requests.get(first + 1).cloned().unwrap_or_default()
    };
    let (t, requests, _) = run(vec![cycled.clone(), busy.clone(), end.clone()]);
    assert_eq!(
        (t.phase, &t.screen.rows),
        (Phase::Idle, &end),
        "the turn's end decides: {requests:#?}"
    );
    assert!(
        after_first_read(&requests).starts_with("await idle 500"),
        "{requests:#?}"
    );
    let (t, requests, took) = run(vec![
        cycled.clone(),
        busy.clone(),
        busy.clone(),
        busy,
        cycled.clone(),
        end.clone(),
    ]);
    assert_eq!(
        (t.phase, &t.screen.rows),
        (Phase::Idle, &end),
        "the turn's end decides: {requests:#?}"
    );
    assert!(took > NO_EVIDENCE_HOLD * 2, "{took:?} {requests:#?}");
    let (t, requests, took) = run(vec![cycled.clone()]);
    assert_eq!(t.phase, Phase::Idle, "{requests:#?}");
    assert!(
        cycled.ends_with(&t.screen.rows),
        "the frame read (its tail): {requests:#?}"
    );
    let told = NO_EVIDENCE_HOLD..NO_EVIDENCE_HOLD + Duration::from_millis(20);
    assert!(told.contains(&took), "{took:?} {requests:#?}");
    let reads = requests.iter().filter(|r| r.starts_with("text")).count();
    assert!(reads >= 2, "read again before it is taken: {requests:#?}");
    let never = Duration::from_secs(10);
    for (idle_after, reads_then) in [
        (
            vec![Duration::from_millis(500), Duration::from_millis(500)],
            3,
        ),
        (vec![never], 2),
        (vec![Duration::from_millis(990), never], 3),
    ] {
        let (t, requests, took) = run_idle(vec![cycled.clone()], &idle_after);
        assert_eq!(t.phase, Phase::Idle, "{idle_after:?}: {requests:#?}");
        assert!(
            told.contains(&took),
            "{idle_after:?}: {took:?} {requests:#?}"
        );
        let reads = requests.iter().filter(|r| r.starts_with("text")).count();
        assert_eq!(reads, reads_then, "{idle_after:?}: {requests:#?}");
    }
    let (t, requests, _) = run(vec![end.clone()]);
    assert_eq!(
        (t.phase, &t.screen.rows),
        (Phase::Idle, &end),
        "{requests:#?}"
    );
    let reads = requests.iter().filter(|r| r.starts_with("text")).count();
    assert_eq!(reads, 1, "{requests:#?}");
}

#[test]
fn a_muse_end_under_its_running_title_is_held_until_the_title_rests() {
    use aterm_phase::muse::fixtures as u;
    use aterm_phase::prompt::fixtures::screen;
    let end = screen(u::END_OF_TURN_1_4_10);
    let run_pinned = |rows: &[String], titles: &[Option<&str>], pinned: Option<&str>| {
        let mut m = Mock::new(vec![rows.to_vec()]);
        m.titles = titles.iter().map(|t| t.map(str::to_string)).collect();
        m.pinned = pinned.map(str::to_string);
        let clock = virtual_clock();
        m.run_on(&clock);
        let mut s = session(&mut m, None);
        s.set_fx(Fx::over(ClockSource::Scripted(Arc::clone(&clock))));
        let started = s.fx.mono();
        let t = s.await_turn(Duration::from_secs(300)).expect("turn");
        let took = s.fx.since(started);
        drop(s);
        let unread = m.titles.len();
        (t, m.requests, took, unread)
    };
    let run = |rows: &[String], titles: &[Option<&str>]| {
        let (t, requests, took, _) = run_pinned(rows, titles, None);
        (t, requests, took)
    };
    let reads = |r: &[String]| r.iter().filter(|r| *r == "title" || *r == "status").count();
    let turning = [Some("⠙ proj"), Some("⠹ proj"), Some("⠸ proj"), Some("proj")];
    for pinned in [None, Some("land-d")] {
        let (t, requests, _, unread) = run_pinned(&end, &turning, pinned);
        assert_eq!(
            (t.phase, &t.screen.rows, t.timed_out),
            (Phase::Idle, &end, false),
            "{pinned:?}"
        );
        assert_eq!(unread, 0, "{pinned:?}: {requests:#?}");
        assert_eq!(reads(&requests), 4, "{pinned:?}: {requests:#?}");
        let first = requests
            .iter()
            .position(|r| r == "title" || r == "status")
            .expect("a read of the title");
        assert!(
            requests[first..]
                .iter()
                .filter(|r| r.starts_with("await idle 500"))
                .count()
                >= 3,
            "each held read waits the short settle: {pinned:?}: {requests:#?}"
        );
    }
    for titles in [[Some("proj")], [None]] {
        let (t, requests, _) = run(&end, &titles);
        assert_eq!((t.phase, &t.screen.rows), (Phase::Idle, &end), "{titles:?}");
        assert_eq!(reads(&requests), 1, "{titles:?}: {requests:#?}");
    }
    let claude = idle_screen();
    let (t, requests, _) = run(&claude, &[Some("⠙ proj")]);
    assert_eq!(t.phase, Phase::Idle);
    assert_eq!(reads(&requests), 0, "{requests:#?}");
    let (t, requests, took) = run(&end, &[Some("⠙ proj")]);
    assert_eq!(
        (t.phase, &t.screen.rows, t.timed_out),
        (Phase::Idle, &end, false)
    );
    assert!(reads(&requests) > 4, "{requests:#?}");
    assert!(
        took >= TITLE_HOLD && took < TITLE_HOLD + Duration::from_secs(5),
        "{took:?}"
    );
}

struct TitleRun {
    turn: Turn,
    requests: Vec<String>,
    took: Duration,
    unread: usize,
    noted: Vec<String>,
}

#[derive(Default)]
struct Noted(Vec<String>);

impl Review for Noted {
    fn approved(&mut self, _seq: u64, _command: &str) -> Result<(), String> {
        Ok(())
    }
    fn review(&mut self, _turn: &Turn) -> Result<bool, String> {
        Ok(false)
    }
    fn say(&mut self, line: &str) -> Result<(), String> {
        crate::supervise::ledger::assert_marked(line);
        Ok(())
    }
    fn note(&mut self, line: &str) {
        crate::supervise::ledger::assert_marked(line);
        self.0.push(line.to_string());
    }
    fn unattended(&self) -> bool {
        false
    }
}

fn title_run(screens: Vec<Vec<String>>, titles: &[&str], refused: bool) -> TitleRun {
    let mut m = Mock::new(screens);
    m.gone_on_next = true;
    m.stall_sleep = Some(Duration::from_secs(60));
    m.titles = titles.iter().map(|t| Some((*t).to_string())).collect();
    m.title_refused = refused;
    let clock = virtual_clock();
    m.run_on(&clock);
    let mut s = session(&mut m, None);
    s.set_fx(Fx::over(ClockSource::Scripted(Arc::clone(&clock))));
    let started = s.fx.mono();
    let mut noted = Noted::default();
    let turn = s
        .await_turn_from(
            Duration::from_secs(600),
            true,
            &mut Saw::default(),
            None,
            None,
            &mut noted,
        )
        .expect("turn");
    let took = s.fx.since(started);
    drop(s);
    TitleRun {
        turn,
        requests: m.requests,
        took,
        unread: m.titles.len(),
        noted: noted.0,
    }
}

fn title_reads(requests: &[String]) -> usize {
    requests.iter().filter(|r| *r == "title").count()
}

#[test]
fn a_title_hold_is_counted_from_the_review_it_holds() {
    use aterm_phase::muse::fixtures as u;
    use aterm_phase::prompt::fixtures::screen;
    let end = screen(u::END_OF_TURN_1_4_10);
    let busy = screen(u::BUSY_1_4_10);
    let turning = ["⠙ proj", "⠹ proj", "⠸ proj", "proj"];
    let mut screens = vec![end.clone()];
    screens.extend(std::iter::repeat_n(busy, 40));
    screens.push(end.clone());
    let r = title_run(screens, &turning, false);
    assert_eq!(
        (r.turn.phase, &r.turn.screen.rows, r.turn.timed_out),
        (Phase::Idle, &end, false),
        "{:#?}",
        r.requests
    );
    assert!(r.took > TITLE_HOLD, "{:?}", r.took);
    assert_eq!(
        (r.unread, title_reads(&r.requests)),
        (0, 4),
        "{:#?}",
        r.requests
    );
    assert!(r.noted.is_empty(), "{:#?}", r.noted);
    let r = title_run(vec![end.clone()], &turning, false);
    assert_eq!(
        (r.turn.phase, r.unread, title_reads(&r.requests)),
        (Phase::Idle, 0, 4)
    );
}

#[test]
fn a_title_hold_holds_a_question_and_notes_its_hang() {
    use aterm_phase::muse::fixtures as u;
    use aterm_phase::prompt::fixtures::screen;
    let asks = screen(u::REPLY_WRAPPED_50_1_4_10);
    let end = screen(u::END_OF_TURN_1_4_10);
    assert_eq!(
        aterm_phase::Program::Muse.reader().phase(&asks),
        Phase::Question
    );
    let r = title_run(vec![asks.clone()], &["⠙ proj", "⠹ proj", "proj"], false);
    assert_eq!(
        (r.turn.phase, &r.turn.screen.rows),
        (Phase::Question, &asks)
    );
    assert_eq!(
        (r.unread, title_reads(&r.requests)),
        (0, 3),
        "{:#?}",
        r.requests
    );
    let r = title_run(vec![end.clone()], &["⠙ proj"], true);
    assert_eq!((r.turn.phase, &r.turn.screen.rows), (Phase::Idle, &end));
    assert_eq!(title_reads(&r.requests), 1, "{:#?}", r.requests);
    assert!(r.took < Duration::from_secs(1), "{:?}", r.took);
    let r = title_run(vec![end.clone()], &["⠙ proj"], false);
    assert_eq!((r.turn.phase, r.turn.timed_out), (Phase::Idle, false));
    assert!(r.took >= TITLE_HOLD, "{:?}", r.took);
    assert_eq!(r.noted.len(), 1, "{:#?}", r.noted);
    assert!(
        r.noted[0].contains("title still said its run goes on 60 s into the hold"),
        "{:#?}",
        r.noted
    );
    let r = title_run(vec![end], &["⠙ proj", "proj"], false);
    assert!(r.noted.is_empty(), "{:#?}", r.noted);
}

struct MarkFake {
    sent: Vec<String>,
    reply: CtlReply,
}

impl Ctl for MarkFake {
    fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
        self.sent.push(args.join(" "));
        Ok(self.reply.clone())
    }
}

#[test]
fn a_refused_act_mark_latches_the_rest_off() {
    let first = "@s-1 mark - act word=approve rule=read-only.classify-v8";
    for (err, latched) in [
        ("ERR unknown verb 'mark' (help lists them)", true),
        ("ERR usage: mark <node|-> <kind> [word=<w>] …", true),
        ("ERR mark: word=x is not a act word (approve deny)", true),
        ("ERR busy retry_ms=50", false),
    ] {
        let mut fake = MarkFake {
            sent: Vec::new(),
            reply: CtlReply {
                code: 1,
                stdout: String::new(),
                stderr: format!("aterm-ctl: {err}\n"),
            },
        };
        let mut s = Session::new(&mut fake, Some("@s-1".to_string()));
        s.act_marks
            .queue("act word=approve rule=read-only.classify-v8".to_string());
        s.act_marks.queue("act word=escalate".to_string());
        s.send_act_marks();
        s.act_marks.queue("act word=deny rule=write.v1".to_string());
        s.send_act_marks();
        drop(s);
        let want: Vec<&str> = if latched {
            vec![first]
        } else {
            vec![
                first,
                "@s-1 mark - act word=escalate",
                "@s-1 mark - act word=deny rule=write.v1",
            ]
        };
        assert_eq!(fake.sent, want, "{err}");
    }
    let mut fake = MarkFake {
        sent: Vec::new(),
        reply: ok("OK 9\n"),
    };
    let mut s = Session::new(&mut fake, None);
    s.act_marks.queue("act word=answer".to_string());
    s.send_act_marks();
    s.send_act_marks();
    drop(s);
    assert_eq!(fake.sent, ["mark - act word=answer"]);
}

#[test]
fn supervise_auto_approves_reads_guarded_and_hands_over_writes() {
    let book = Book::new("approve");
    let mut m = fenced(vec![bash_one_row(), busy_screen(), write_prompt()]);
    let mut s = session(&mut m, Some("@s-9".to_string()));
    book.keep(&mut s);
    let (out, code) = s.supervise(&auto(30)).expect("supervise");
    assert_eq!(code, 0);
    assert!(
        out.starts_with("prompt\nkind bash\ncommand rm -rf target\n"),
        "{out}"
    );
    assert_eq!(
        m.requests,
        [
            "@s-9 await gone esc.to.interrupt timeout 3000",
            "@s-9 text --json tail=40",
            "@s-9 status",
            "@s-9 meta",
            "@s-9 help key",
            "@s-9 key if-human=7 if-gen=1.101 if=^\\x20{3}git\\x20log\\x20--oneline\\x20-5\\s*$ 1",
            "@s-9 await seq 101 timeout 20000",
            "@s-9 text --json tail=40",
            "@s-9 mark - act word=approve rule=read-only.classify-v8",
            "@s-9 await idle 500 timeout 1500",
            "@s-9 text --json tail=40",
            "@s-9 status",
            "@s-9 mark - act word=escalate",
        ],
        "one guarded press, then the box must leave before the next look; \
         after it the settle wait, not `await gone` (the footer is not up yet); \
         each box decided reads the session's program first; each look's \
         act (the press, the hand-over) is the session log's mark once the \
         look returns"
    );
    assert_eq!(
        book.decided(),
        ["approved git log --oneline -5", "escalated rm -rf target"]
    );
}

#[test]
fn a_skipped_guard_is_not_an_approval() {
    let book = Book::new("skipped");
    let mut m = fenced(vec![bash_one_row(), idle_screen()]);
    m.key_replies.push_back(ok("OK skipped seq=105\n"));
    let mut s = session(&mut m, None);
    book.keep(&mut s);
    let (out, code) = s.supervise(&auto(30)).expect("supervise");
    assert_eq!(code, 0);
    assert!(out.starts_with("idle\n--\n"), "{out}");
    assert_eq!(
        m.presses(),
        ["key if-human=7 if-gen=1.101 if=^\\x20{3}git\\x20log\\x20--oneline\\x20-5\\s*$ 1"]
    );
    assert_eq!(book.decided(), ["skipped git log --oneline -5"]);
}

#[test]
fn a_guard_that_matches_no_row_of_the_parsed_box_is_handed_over() {
    let mut m = fenced(vec![bash_one_row()]);
    m.key_replies.push_back(ok("OK skipped seq=101\n"));
    let mut s = session(&mut m, None);
    let (out, code) = s.supervise(&auto(30)).expect("supervise");
    assert_eq!(
        s.box_reason.as_ref().map(|(_, why)| why.as_str()),
        Some("aterm could not find the box's row")
    );
    drop(s);
    assert_eq!(code, 0);
    assert!(out.starts_with("prompt\nkind bash\n"), "{out}");
    assert_eq!(
        m.presses(),
        ["key if-human=7 if-gen=1.101 if=^\\x20{3}git\\x20log\\x20--oneline\\x20-5\\s*$ 1"]
    );
}

#[test]
fn guard_errors_are_retried_or_surfaced_never_approved() {
    let book = Book::new("busy");
    let mut m = fenced(vec![
        bash_one_row(),
        bash_one_row(),
        busy_screen(),
        idle_screen(),
    ]);
    m.key_replies.push_back(err("busy sink"));
    let mut s = session(&mut m, None);
    book.keep(&mut s);
    let (out, code) = s.supervise(&auto(30)).expect("supervise");
    assert_eq!(code, 0);
    assert!(out.starts_with("idle\n--\n"), "{out}");
    let guard = G_PRESS.trim_start_matches("key ");
    let first_press = format!("key if-human=7 if-gen=1.101 {guard}");
    let second_press = format!("key if-human=7 if-gen=1.102 {guard}");
    assert_eq!(
        m.presses(),
        [&first_press, &second_press],
        "{:?}",
        m.requests
    );
    let first = m
        .requests
        .iter()
        .position(|r| r == &first_press)
        .expect("press");
    assert!(
        m.requests[first + 1..]
            .iter()
            .take_while(|r| *r != &second_press)
            .any(|r| r.starts_with("text --json")),
        "the second press follows a fresh read: {:?}",
        m.requests
    );
    let decided = book.decided();
    assert_eq!(
        decided.iter().filter(|r| r.starts_with("approved")).count(),
        1,
        "{decided:?}"
    );

    for what in ["no such session", "badregex"] {
        let book = Book::new("halted");
        let mut m = fenced(vec![bash_one_row()]);
        m.key_replies.push_back(err(what));
        let mut s = session(&mut m, None);
        book.keep(&mut s);
        let e = s.supervise(&auto(30)).expect_err("surfaces");
        assert!(e.contains(&format!("ERR {what}")), "{what}: {e}");
        assert_eq!(m.presses().len(), 1, "{what}: {:?}", m.requests);
        let decided = book.decided();
        assert!(
            !decided.iter().any(|r| r.starts_with("approved")),
            "{what}: nothing approved: {decided:?}"
        );
    }
}

#[test]
fn under_approve_none_every_prompt_is_handed_over() {
    let mut m = Mock::new(vec![bash_one_row()]);
    let mut s = session(&mut m, None);
    let (out, code) = s
        .supervise(&SuperviseOpts {
            max: Duration::from_secs(5),
            policy: powerless(),
            ..SuperviseOpts::default()
        })
        .expect("supervise");
    assert_eq!(code, 0);
    assert!(out.starts_with("prompt\n"), "{out}");
    assert!(!m.requests.iter().any(|r| r.starts_with("key")));
}

#[test]
fn the_same_box_is_never_pressed_twice_on_an_unchanged_screen() {
    let book = Book::new("unchanged");
    let mut m = fenced(vec![bash_one_row()]);
    let mut s = session(&mut m, None);
    book.keep(&mut s);
    let (out, code) = s.supervise(&auto(30)).expect("supervise");
    assert_eq!(code, 0);
    assert!(out.starts_with("prompt\n"), "{out}");
    assert_eq!(
        m.requests,
        [
            "await gone esc.to.interrupt timeout 3000",
            "text --json tail=40",
            "status",
            "meta",
            "help key",
            "key if-human=7 if-gen=1.101 if=^\\x20{3}git\\x20log\\x20--oneline\\x20-5\\s*$ 1",
            "await seq 101 timeout 20000",
            "text --json tail=40",
            "mark - act word=approve rule=read-only.classify-v8",
        ],
        "{:?}",
        m.requests
    );
    assert_eq!(book.decided(), ["approved git log --oneline -5"]);
}

#[test]
fn a_read_that_keeps_coming_back_is_approved_twice_then_handed_over() {
    let book = Book::new("repeat");
    let mut m = fenced(vec![
        bash_one_row(),
        busy_screen(),
        bash_one_row(),
        busy_screen(),
        bash_one_row(),
    ]);
    let mut s = session(&mut m, None);
    book.keep(&mut s);
    let (out, code) = s.supervise(&auto(30)).expect("supervise");
    assert_eq!(code, 0);
    assert!(
        out.starts_with("prompt\nkind bash\ncommand git log --oneline -5\n"),
        "{out}"
    );
    assert_eq!(m.presses().len(), 2, "{:?}", m.requests);
    assert_eq!(
        book.decided(),
        [
            "approved git log --oneline -5",
            "approved git log --oneline -5",
            "escalated git log --oneline -5",
        ]
    );
}

#[test]
fn an_await_timeout_is_a_step_not_an_error() {
    let mut m = Mock::new(vec![idle_screen()]);
    m.replies.push_back(timeout());
    let mut s = session(&mut m, None);
    let t = s.await_turn(Duration::from_secs(30)).expect("turn");
    assert_eq!(t.phase, Phase::Idle);
    assert_eq!(
        m.requests,
        [
            "await gone esc.to.interrupt timeout 3000",
            "text --json tail=40"
        ]
    );
}

#[test]
fn reply_readers_match_the_wire() {
    assert_eq!(ok("OK seq=31\n").seq(), Some(31));
    assert_eq!(ok("OK skipped seq=7\n").seq(), Some(7));
    assert!(ok("OK skipped seq=7\n").skipped());
    assert!(!ok("OK seq=7\n").skipped());
    assert_eq!(ok("OK\n").seq(), None);
    assert!(bare_err().bare_err());
    assert!(bare_err().unknown_form());
    assert!(usage("key <name>").unknown_form());
    assert!(!err("busy sink").unknown_form());
    assert!(err("busy sink").is_err("busy sink"));
    assert!(err("halted").is_err("halted"));
    assert!(!err("halted").is_err("busy sink"));
    assert_eq!(err("no such session").err_text(), "ERR no such session");
    assert!(!ok("OK\n").bare_err());
}

fn question_screen() -> Vec<String> {
    let mut q = rows(&["⏺ Did the suite pass on your machine?", ""]);
    q.extend(composer("  ? for shortcuts"));
    q
}
fn limited_screen() -> Vec<String> {
    let mut r = rows(&[
        "⏺ Running the suite again.",
        "  ⎿  You've hit your session limit · resets 7:30pm (America/Los_Angeles)",
        "",
        "✻ Worked for 3m 2s · done 5:02 PM",
        "",
    ]);
    r.extend(composer("  ⏵⏵ auto mode on (shift+tab to cycle)"));
    r
}
fn framed(body: &[&str]) -> Vec<String> {
    let mut r = rows(body);
    r.extend(composer("  ? for shortcuts"));
    r
}
fn bash_box(transcript: &[&str], command: &str) -> Vec<String> {
    let mut r = rows(transcript);
    r.extend(rows(&[
        "",
        " Bash command",
        "",
        &format!("   {command}"),
        "   Push the branch",
        "",
        " Do you want to proceed?",
        " ❯ 1. Yes",
        "   2. Yes, and don’t ask again for: git push *",
        "   3. No",
        "",
        " Esc to cancel · Tab to amend",
    ]));
    r.extend(composer("  ? for shortcuts"));
    r
}
fn watch_lines(m: &mut Mock, opts: &SuperviseOpts) -> (Vec<String>, u8) {
    watch_lines_with(m, opts, |_| {})
}
type Watched<'s, 'm> = Session<'s, TapedCtl<'m, Mock>>;
fn taped_session<'m, 't>(
    taped: &'t mut TapedCtl<'m, Mock>,
    tape: &MemoryTape,
) -> Session<'t, TapedCtl<'m, Mock>> {
    let mut s = session(taped, None);
    s.set_fx(tape.fx.clone());
    s
}
const ROUND_TRIP: Duration = Duration::from_millis(1);
fn virtual_clock() -> Arc<super::super::fx::ScriptedClock> {
    let clock = Arc::new(super::super::fx::ScriptedClock::default());
    clock.run_virtual();
    clock
}
fn virtual_session(m: &mut Mock, sid: Option<String>) -> Session<'_, Mock> {
    let clock = virtual_clock();
    m.run_on(&clock);
    let mut s = session(m, sid);
    s.set_fx(Fx::over(ClockSource::Scripted(clock)));
    s
}
impl Mock {
    fn run_on(&mut self, clock: &Arc<super::super::fx::ScriptedClock>) {
        let steps = clock.is_virtual();
        self.waits = Some(Arc::clone(clock));
        self.hang_at = steps.then(|| Instant::now() + aterm_spec::wait::MUST_HAPPEN);
        self.round_trip = if steps { ROUND_TRIP } else { Duration::ZERO };
    }
}
fn memory_tape(m: &mut Mock) -> (MemoryTape, TapedCtl<'_, Mock>) {
    memory_tape_on(m, virtual_clock())
}
fn memory_tape_on(
    m: &mut Mock,
    clock: Arc<super::super::fx::ScriptedClock>,
) -> (MemoryTape, TapedCtl<'_, Mock>) {
    m.run_on(&clock);
    let tape = MemoryTape::over(&Fx::over(ClockSource::Scripted(clock)));
    let taped = TapedCtl::new(m, tape.fx.clone(), crate::supervise::tape::doors::CTL);
    (tape, taped)
}
fn watch_lines_with(
    m: &mut Mock,
    opts: &SuperviseOpts,
    setup: impl FnOnce(&mut Session<'_, TapedCtl<'_, Mock>>),
) -> (Vec<String>, u8) {
    let run = record_watch(m, opts, setup);
    if run.unreplayable.is_none() {
        crate::supervise::tape::testing::assert_replays(
            &run.tape,
            &run.header,
            &run.sent,
            &run.lines,
        );
    }
    (run.lines, run.code)
}
struct RecordedWatch {
    tape: MemoryTape,
    header: crate::supervise::tape::header::Header,
    lines: Vec<String>,
    code: u8,
    unreplayable: Option<&'static str>,
    sent: Vec<String>,
}
fn record_watch(
    m: &mut Mock,
    opts: &SuperviseOpts,
    setup: impl FnOnce(&mut Session<'_, TapedCtl<'_, Mock>>),
) -> RecordedWatch {
    record_watch_on(m, opts, virtual_clock(), setup)
}
fn record_watch_on(
    m: &mut Mock,
    opts: &SuperviseOpts,
    clock: Arc<super::super::fx::ScriptedClock>,
    setup: impl FnOnce(&mut Session<'_, TapedCtl<'_, Mock>>),
) -> RecordedWatch {
    let mut out: Vec<u8> = Vec::new();
    let from = m.order.len();
    let (tape, mut taped) = memory_tape_on(m, clock);
    let mut s = taped_session(&mut taped, &tape);
    s.set_clock(TEST_NOW, PDT);
    s.set_zone_lookup(test_zones);
    setup(&mut s);
    let header = run_header(&s, opts);
    let unreplayable = s.unreplayable;
    tape.core().reroot(header.roots.clone());
    let code = s.watch(opts, &mut out);
    drop(s);
    drop(taped);
    tape.finish(&crate::supervise::tape::replay::watch_end(code));
    let text = String::from_utf8(out).expect("utf-8");
    RecordedWatch {
        tape,
        header,
        lines: text.lines().map(str::to_string).collect(),
        code,
        unreplayable,
        sent: sent(m, from),
    }
}
fn run_header<C: Ctl>(
    s: &Session<'_, C>,
    opts: &SuperviseOpts,
) -> crate::supervise::tape::header::Header {
    crate::supervise::tape::header::Header::capture(
        crate::supervise::tape::testing::TAPE_ID,
        crate::supervise::tape::header::Entry::Watch,
        None,
        s.setup(),
        opts,
    )
}
fn sent(m: &Mock, from: usize) -> Vec<String> {
    m.order[from.min(m.order.len())..]
        .iter()
        .map(|(r, _)| r.clone())
        .collect()
}
const TEST_NOW: i64 = 1_789_660_500;
const AFTER_RESET: i64 = 1_789_840_830;
const PDT: i64 = -7 * 3600;
fn test_zones(zone: &str, _unix: i64) -> Option<i64> {
    match zone {
        "America/Los_Angeles" => Some(PDT),
        "UTC" => Some(0),
        _ => None,
    }
}
fn decisions(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .map(|l| {
            let mut words: Vec<&str> = l.split(' ').collect();
            words.retain(|w| !w.starts_with("seq="));
            words.join(" ")
        })
        .collect()
}

fn by_screen(turn: &Turn) -> &'static dyn ScreenReader {
    aterm_phase::identify(None, &turn.screen.rows)
}

fn esc_line_of(rows: Vec<String>, program: &'static str) -> Option<String> {
    let mut m = Mock::new(vec![rows]);
    m.program = program;
    let mut s = session(&mut m, Some("@s-1".to_string()));
    let (turn, reader) = s.phase_now().expect("a read");
    assert_eq!(turn.phase, Phase::Prompt, "a box");
    let out = render_phase_and_survey(&turn, reader, &[]);
    out.lines()
        .find(|l| l.ends_with(" esc"))
        .map(str::to_string)
}

#[test]
fn drive_phase_says_esc_quits_the_program_on_a_trust_dialog() {
    use aterm_phase::prompt::fixtures::screen;
    use aterm_phase::{agy, gemini, muse, prompt};
    for (name, rows, program) in [
        ("claude", screen(prompt::fixtures::TRUST), "claude"),
        ("muse", screen(muse::fixtures::TRUST_1_4_10), "muse"),
        ("gemini", screen(gemini::fixtures::TRUST), "node"),
        ("agy", screen(agy::fixtures::BOX_TRUST), "agy"),
        (
            "agy lab",
            screen(agy::fixtures::lab::FIRST_RUN_TRUST),
            "agy",
        ),
    ] {
        assert_eq!(
            esc_line_of(rows, program).as_deref(),
            Some("exit esc"),
            "{name}"
        );
    }
    for (name, rows, program) in [
        ("claude", screen(prompt::fixtures::BOX_BASH_TOUCH), "claude"),
        ("gemini", screen(gemini::fixtures::BOX_SHELL), "node"),
        (
            "agy terms",
            screen(agy::fixtures::lab::FIRST_RUN_TERMS),
            "agy",
        ),
    ] {
        assert_eq!(
            esc_line_of(rows, program).as_deref(),
            Some("cancel esc"),
            "the control: {name}"
        );
    }
    for (name, rows, program) in [
        (
            "gemini quota",
            screen(gemini::fixtures::LIMIT_DIALOG),
            "node",
        ),
        (
            "agy colour scheme",
            screen(agy::fixtures::lab::FIRST_RUN_SCHEME),
            "agy",
        ),
    ] {
        assert_eq!(esc_line_of(rows, program), None, "the control: {name}");
    }
}

#[test]
fn a_busy_row_that_stays_over_a_box_is_waited_on_two_seconds_at_most() {
    use aterm_phase::muse::fixtures as u;
    use aterm_phase::prompt::fixtures::screen;
    let gone_waits = |program: &'static str, screens: Vec<Vec<String>>| {
        let mut m = Mock::new(screens);
        m.program = program;
        let mut s = session(&mut m, Some("@s-1".to_string()));
        let turn = s.await_phase(Duration::from_secs(30)).expect("a turn").0;
        let waits: Vec<String> = m
            .requests
            .iter()
            .filter(|r| r.contains(" await gone "))
            .cloned()
            .collect();
        (turn.phase, waits)
    };
    let (phase, waits) = gone_waits(
        "muse",
        vec![
            screen(u::BUSY_1_4_10),
            screen(u::BUSY_1_4_10),
            screen(u::BOX_SHELL),
        ],
    );
    assert_eq!(phase, Phase::Prompt, "the box is the turn's point");
    assert!(waits.len() >= 2, "{waits:#?}");
    assert!(
        waits.iter().all(|w| w.ends_with(" timeout 2000")),
        "{waits:#?}"
    );
    let (_, waits) = gone_waits("claude", vec![busy_screen(), busy_screen(), idle_screen()]);
    assert!(
        waits.iter().any(|w| w.ends_with(" timeout 20000")),
        "the control: {waits:#?}"
    );
}

#[test]
fn the_loop_waits_in_its_setups_step_and_replays_in_it() {
    let step = Duration::from_secs(7);
    assert_ne!(step, next_wait_step(), "a step this process does not read");
    let gone_waits = |setup_step: Option<Duration>| {
        let mut m = Mock::new(vec![busy_screen(), busy_screen(), idle_screen()]);
        m.vanish_after = Some(0);
        let _ = watch_lines_with(&mut m, &auto(30), |s| {
            if let Some(step) = setup_step {
                s.setup.wait_step = step;
            }
        });
        m.requests
            .iter()
            .filter(|r| r.contains("await gone ") && r.contains(" rows "))
            .cloned()
            .collect::<Vec<_>>()
    };
    let waits = gone_waits(Some(step));
    assert!(!waits.is_empty(), "a scoped wait for the busy row");
    assert!(
        waits.iter().all(|w| w.ends_with(" timeout 7000")),
        "{waits:#?}"
    );
    let waits = gone_waits(None);
    assert!(!waits.is_empty(), "the control: a scoped wait");
    assert!(
        waits.iter().all(|w| w.ends_with(" timeout 20000")),
        "the control: {waits:#?}"
    );
}

#[test]
fn render_prompt_prints_the_notes_after_the_description_and_before_classify() {
    let turn_for = |rows: Vec<String>| Turn {
        phase: worker_phase(&rows),
        screen: Screen {
            rows,
            ..Screen::default()
        },
        timed_out: false,
    };
    let turn = turn_for(bash_multi_row_with_note());
    assert_eq!(turn.phase, Phase::Prompt);
    let out = render_phase(&turn, by_screen(&turn), &[]);
    let keys: Vec<&str> = out
        .lines()
        .map(|l| l.split(' ').next().unwrap_or(""))
        .collect();
    assert_eq!(
        keys,
        [
            "prompt",
            "kind",
            "command",
            "description",
            "note",
            "classify",
            "option",
            "option",
            "cancel"
        ],
        "{out}"
    );
    assert!(out.contains("\nnote Dangerous rm operation"), "{out}");
    let event = event_line(&turn, &[]);
    assert!(!event.contains("note"), "{event}");
    let turn = turn_for(bash_multi_row());
    let out = render_phase(&turn, by_screen(&turn), &[]);
    assert!(
        out.contains("\nnote This command requires approval\nclassify "),
        "{out}"
    );
    let turn = turn_for(bash_one_row());
    let out = render_phase(&turn, by_screen(&turn), &[]);
    assert!(!out.contains("\nnote "), "{out}");
}

#[test]
fn a_boxs_classify_word_is_the_read_only_rules_verdict_on_its_readings() {
    use crate::supervise::classify::classify_command;
    let turn_for = |rows: Vec<String>| Turn {
        phase: worker_phase(&rows),
        screen: Screen {
            rows,
            ..Screen::default()
        },
        timed_out: false,
    };
    let mut too_many = aterm_phase::prompt::fixtures::screen(
        aterm_phase::prompt::fixtures::BOX_RM_TOO_MANY_NO_RM_2_1_307,
    );
    too_many.extend(composer("  ? for shortcuts"));
    let tee: Vec<String> = bash_one_row()
        .into_iter()
        .map(|r| {
            if r == "   git log --oneline -5" {
                "   echo \\  #; tee x".to_string()
            } else {
                r
            }
        })
        .collect();
    for (rows, why) in [
        (too_many, "not read-only (newline reading): "),
        (tee, "not read as drawn ("),
    ] {
        let turn = turn_for(rows);
        assert_eq!(turn.phase, Phase::Prompt);
        let p = by_screen(&turn).prompt(&turn.screen.rows).expect("the box");
        assert!(classify_command(&p.command).read_only, "{}", p.command);
        let event = event_line(&turn, &[]);
        assert!(
            event.contains(&format!(" kind=bash classify=not-read-only:{why}")),
            "{event}"
        );
        let out = render_phase(&turn, by_screen(&turn), &[]);
        assert!(
            out.contains(&format!("\nclassify not-read-only {why}")),
            "{out}"
        );
    }
    let turn = turn_for(bash_one_row());
    let event = event_line(&turn, &[]);
    assert!(
        event.contains(" kind=bash classify=read-only command=git log"),
        "{event}"
    );
    let out = render_phase(&turn, by_screen(&turn), &[]);
    assert!(out.contains("\nclassify read-only\n"), "{out}");
}

#[test]
fn a_bash_box_not_read_whole_is_never_called_read_only() {
    let bash = Prompt {
        kind: PromptKind::Bash,
        command: "git log --oneline -5".to_string(),
        description: String::new(),
        notes: Vec::new(),
        options: vec![(1, "Yes".to_string()), (2, "No".to_string())],
    };
    let out = render_prompt(&bash, None, None);
    assert!(
        out.contains("\nclassify not-read-only the box's command was not read whole\n"),
        "{out}"
    );
    assert!(!out.contains("classify read-only"), "{out}");
    let out = render_prompt(&bash, None, Some(Ok(())));
    assert!(out.contains("\nclassify read-only\n"), "{out}");
    let other = Prompt {
        kind: PromptKind::Other,
        ..bash
    };
    assert!(!render_prompt(&other, None, None).contains("classify"));
}

#[test]
fn drive_phase_names_agy_and_muse_background_work() {
    use aterm_phase::agy::fixtures::lab as a;
    use aterm_phase::muse::fixtures as u;
    use aterm_phase::prompt::fixtures::screen;
    use aterm_phase::{AgyReader, MuseReader};
    let render = |reader: &dyn ScreenReader, text: &str| {
        let rows = screen(text);
        let turn = Turn {
            phase: reader.phase(&rows),
            screen: Screen {
                rows,
                ..Screen::default()
            },
            timed_out: false,
        };
        render_phase(&turn, reader, &[])
    };
    assert_eq!(
        render(&AgyReader, a::BG_TASK),
        "busy\nreason its own background work: a background task running\n"
    );
    assert_eq!(
        render(&MuseReader, u::END_OF_TURN_BG_RUNNING),
        "busy\nreason its own background work: background work running\n"
    );
    assert_eq!(
        render(&AgyReader, a::BG_BUSY),
        "busy\nreason status row: esc to interrupt\n"
    );
    assert_eq!(render(&AgyReader, a::BG_TASK_ENDED), "idle\n");
    assert_eq!(render(&MuseReader, u::END_OF_TURN_BG_FINISHED), "idle\n");
}

#[test]
fn an_event_summary_is_what_the_worker_said_not_the_survey() {
    let launch = workflow_box();
    let turn = Turn {
        phase: worker_phase(&launch),
        screen: Screen {
            rows: launch,
            ..Screen::default()
        },
        timed_out: false,
    };
    let event = event_line(&turn, &[]);
    assert!(
        event.contains(" kind=workflow classify=- command=Fan out the 12 benchmark"),
        "{event}"
    );

    let mut rows: Vec<String> = super::super::prompt::fixtures::WAIT_BG7
        .lines()
        .map(str::to_string)
        .collect();
    rows.remove(0);
    rows.pop();
    let turn = Turn {
        phase: worker_phase(&rows),
        screen: Screen {
            rows,
            seq: 7,
            ..Screen::default()
        },
        timed_out: false,
    };
    assert_eq!(
        event_line(&turn, &[]),
        "EVENT idle seq=7 bookkeeping, then site B, then compaction, then the post-merge \
         reschedule. Waiting for your call."
    );
}

#[test]
fn the_event_summary_joins_the_replys_wrapped_rows() {
    use aterm_phase::prompt::fixtures::screen;
    use aterm_phase::{agy, gemini, muse, prompt};
    let tail = "every caller now. Which do you prefer (1 or 2)?";
    let ask = format!("Two ways on from here: keep the old API, or move {tail}");
    let claude_line = format!("Two ways on from here: keep the old API, or move {tail}");
    for (name, text, whole, last) in [
        (
            "claude",
            prompt::fixtures::REPLY_WRAPPED_50_2_1_302,
            claude_line.as_str(),
            tail,
        ),
        (
            "gemini",
            gemini::fixtures::REPLY_WRAPPED_50,
            ask.as_str(),
            tail,
        ),
        (
            "muse",
            muse::fixtures::REPLY_WRAPPED_50_1_4_10,
            ask.as_str(),
            tail,
        ),
        (
            "agy",
            agy::fixtures::lab::REPLY_WRAPPED_50,
            ask.as_str(),
            "or 2)?",
        ),
    ] {
        let rows = screen(text);
        let reader = aterm_phase::identify(None, &rows);
        let turn = |cols: Option<u16>| Turn {
            phase: reader.phase(&rows),
            screen: Screen {
                rows: rows.clone(),
                seq: 7,
                cols,
                ..Screen::default()
            },
            timed_out: false,
        };
        let word = turn(None).phase.name();
        assert_eq!(
            event_line(&turn(Some(50)), &[]),
            format!("EVENT {word} seq=7 {whole}"),
            "{name}"
        );
        assert_eq!(
            event_line(&turn(None), &[]),
            format!("EVENT {word} seq=7 {last}"),
            "the control: {name}"
        );
    }
    let words = "Tests pin the behaviour down, reviews catch what tests miss, and \
                 together they create a safety net. "
        .repeat(8)
        + "That is what keeps the code correct, maintainable, and resilient as it evolves \
           continuously.";
    let mut rows: Vec<String> = Vec::new();
    let mut row = String::new();
    for w in words.split_whitespace() {
        if !row.is_empty() && 2 + row.chars().count() + 1 + w.chars().count() > 120 {
            rows.push(std::mem::take(&mut row));
        }
        if !row.is_empty() {
            row.push(' ');
        }
        row.push_str(w);
    }
    rows.push(row);
    assert!(rows.len() >= 6, "PRECONDITION: a paragraph of many rows");
    let last_row = rows.last().cloned().unwrap_or_default();
    let mut rows: Vec<String> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| format!("{}{r}", if i == 0 { "⏺ " } else { "  " }))
        .collect();
    rows.extend(super::super::prompt::fixtures::rows(&[
        "",
        "✻ Crunched for 0s · done 6:15 PM",
        "",
    ]));
    rows.extend(composer("  ? for shortcuts"));
    let turn = |cols: Option<u16>| Turn {
        phase: Phase::Idle,
        screen: Screen {
            rows: rows.clone(),
            seq: 7,
            cols,
            ..Screen::default()
        },
        timed_out: false,
    };
    let long = event_line(&turn(Some(120)), &[]);
    let said = long.strip_prefix("EVENT idle seq=7 ").expect("the summary");
    assert!(said.starts_with('…'), "{said}");
    assert!(said.ends_with("as it evolves continuously."), "{said}");
    assert_eq!(said.chars().count(), LINE_CHARS, "{said}");
    assert!(
        said.contains("together they create a safety net."),
        "{said}"
    );
    assert_eq!(
        event_line(&turn(None), &[]),
        format!("EVENT idle seq=7 {last_row}"),
        "the control"
    );
}

#[test]
fn supervise_hands_a_limit_notice_over_like_idle() {
    let mut m = Mock::new(vec![busy_screen(), limited_screen()]);
    let mut s = session(&mut m, None);
    let (out, code) = s.supervise(&auto(30)).expect("supervise");
    assert_eq!(code, 0);
    assert!(
        out.starts_with(
            "limited\nmessage You've hit your session limit · resets 7:30pm \
             (America/Los_Angeles)\nreset 7:30pm (America/Los_Angeles)\n--\n"
        ),
        "{out}"
    );
    assert!(m.presses().is_empty());
    assert!(
        !m.requests
            .iter()
            .any(|r| r.starts_with("meta") || r.starts_with("turn")),
        "{:?}",
        m.requests
    );
}

#[test]
fn a_point_is_new_when_its_transcript_moved_whatever_the_loop_saw() {
    let ticked = {
        let mut r = question_screen();
        let last = r.len() - 1;
        r[last] = "  ? for shortcuts                      tick 2".to_string();
        r
    };
    let mut m = Mock::new(vec![question_screen(), ticked]);
    m.idle_skips_busy = true;
    m.vanish_after = Some(1);
    let (lines, code) = watch_lines(&mut m, &auto(30));
    assert_eq!(
        lines,
        [
            "EVENT question seq=101 ⏺ Did the suite pass on your machine?",
            "EXIT session gone (await seq 102 failed: aterm-ctl: ERR exited)",
        ]
    );
    assert_eq!(code, 1);

    let first = framed(&[
        "❯ keep going",
        "",
        "⏺ Done.",
        "",
        "✻ Cogitated for 4s · done 10:40 AM",
        "",
    ]);
    let again = framed(&[
        "❯ keep going",
        "",
        "⏺ Done.",
        "",
        "✻ Cogitated for 4s · done 10:40 AM",
        "",
        "❯ and the docs",
        "",
        "⏺ Done.",
        "",
        "✻ Cogitated for 3s · done 10:40 AM",
        "",
    ]);
    let mut m = Mock::new(vec![first, busy_screen(), again]);
    m.idle_skips_busy = true;
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines(&mut m, &auto(30));
    assert_eq!(
        lines,
        [
            "EVENT idle seq=101 ⏺ Done.",
            "EVENT idle seq=103 ⏺ Done.",
            "EXIT session gone (await seq 103 failed: aterm-ctl: ERR exited)",
        ]
    );
    assert_eq!(
        m.requests.iter().filter(|r| r.starts_with("text")).count(),
        2,
        "two reads, neither of them busy: {:?}",
        m.requests
    );
}

#[test]
fn a_new_point_that_looks_like_the_last_one_is_still_reported() {
    let retried = framed(&[
        "⏺ Running the suite again.",
        "  ⎿  You've hit your session limit · resets 7:30pm (America/Los_Angeles)",
        "",
        "✻ Worked for 3m 2s · done 5:02 PM",
        "",
        "❯ continue",
        "  ⎿  You've hit your session limit · resets 7:30pm (America/Los_Angeles)",
        "",
        "✻ Churned for 0s · done 5:02 PM",
        "",
    ]);
    for (case, script, want) in [
        (
            "a shell back at its prompt",
            vec![
                rows(&["$ make", "ok", "$"]),
                rows(&["$ make", "ok", "$"]),
                rows(&["$ make", "ok", "$ ls", "a b c", "$"]),
            ],
            "EVENT idle $",
        ),
        (
            "a retry into the wall",
            vec![limited_screen(), busy_screen(), retried],
            "EVENT limited message=You've hit your session limit · resets 7:30pm \
             (America/Los_Angeles) reset=7:30pm (America/Los_Angeles)",
        ),
    ] {
        let mut m = Mock::new(script);
        m.idle_skips_busy = true;
        m.vanish_after = Some(0);
        let (lines, code) = watch_lines(
            &mut m,
            &SuperviseOpts {
                max: Duration::from_secs(30),
                policy: powerless(),
                ..SuperviseOpts::default()
            },
        );
        let got: Vec<String> = decisions(&lines)
            .into_iter()
            .filter(|l| !l.starts_with("EXTEND "))
            .collect();
        assert_eq!(&got[..2], [want, want], "{case}: {lines:?}");
        assert!(got[2].starts_with("EXIT session gone"), "{case}: {lines:?}");
        assert_eq!(got.len(), 3, "{case}: {lines:?}");
        assert_eq!(code, 1, "{case}");
        assert!(
            !m.requests.iter().any(|r| r.starts_with("key")),
            "{case}: nothing pressed: {:?}",
            m.requests
        );
    }
}

#[test]
fn a_frameless_screen_still_changing_is_busy_until_it_settles() {
    let mut m = Mock::new(vec![
        rows(&["$ make", "compiling a"]),
        rows(&["$ make", "compiling a", "compiling b", "done", "$"]),
    ]);
    m.replies.push_back(ok("OK gone 100\n"));
    m.replies.push_back(timeout());
    let mut s = session(&mut m, None);
    let (out, code) = s.supervise(&auto(30)).expect("supervise");
    assert_eq!(code, 0);
    assert_eq!(out, "idle\n--\n$ make\ncompiling a\ncompiling b\ndone\n$\n");
    assert_eq!(
        m.requests,
        [
            "await gone esc.to.interrupt timeout 3000",
            "text --json tail=40",
            "await idle 2000 timeout 3000",
            "text --json tail=40",
            "await seq 102 timeout 20000",
            "await idle 2000 timeout 3000",
            "text --json tail=40",
        ]
    );
}

#[test]
fn nothing_happens_after_the_budget() {
    let spent = SuperviseOpts {
        max: Duration::ZERO,
        ..SuperviseOpts::default()
    };
    let mut m = Mock::new(vec![busy_screen()]);
    let (lines, code) = watch_lines(&mut m, &spent);
    assert_eq!((lines, code), (vec!["TIMEOUT".to_string()], EXIT_TIMEOUT));

    let mut m = Mock::new(vec![idle_screen()]);
    let (lines, code) = watch_lines(&mut m, &spent);
    assert_eq!((lines, code), (vec!["TIMEOUT".to_string()], EXIT_TIMEOUT));

    let mut m = fenced(vec![bash_one_row()]);
    let mut s = session(&mut m, None);
    let (out, code) = s.supervise(&auto(0)).expect("supervise");
    assert_eq!(code, EXIT_TIMEOUT);
    assert!(out.starts_with("TIMEOUT\nprompt\nkind bash\n"), "{out}");
    assert!(m.presses().is_empty(), "{:?}", m.requests);

    let mut m = Mock::new(vec![idle_screen()]);
    let mut s = session(&mut m, None);
    let (out, code) = s.supervise(&spent).expect("supervise");
    assert_eq!(code, EXIT_TIMEOUT);
    assert!(out.starts_with("TIMEOUT\nidle\n--\n"), "{out}");

    let mut m = fenced(vec![bash_one_row()]);
    m.delay.insert(6, Duration::from_millis(200));
    let opts = SuperviseOpts {
        max: Duration::from_millis(100),
        ..auto(0)
    };
    let (lines, code) = watch_lines(&mut m, &opts);
    assert_eq!(
        (lines, code),
        (
            vec![
                "APPROVED seq=101 git log --oneline -5".to_string(),
                "TIMEOUT".to_string()
            ],
            EXIT_TIMEOUT
        )
    );
    assert_eq!(m.requests.len(), 9, "nothing read after: {:?}", m.requests);
    assert_eq!(
        m.requests.last().map(String::as_str),
        Some("mark - act word=approve rule=read-only.classify-v8")
    );
}

#[test]
fn watch_lines_are_cut_at_160_characters() {
    let long = "x".repeat(400);
    assert_eq!(clip(&long).chars().count(), LINE_CHARS);
    assert_eq!(clip("a\tb\nc"), "a b c");
    assert_eq!(exit_reason("ERR halted\n"), "ERR halted");
    assert_eq!(
        exit_reason("text --json tail=40 failed: aterm-ctl: ERR no such session"),
        "session gone (text --json tail=40 failed: aterm-ctl: ERR no such session)"
    );
    assert_eq!(
        exit_reason("await seq 633 failed: aterm-ctl: ERR exited"),
        "session gone (await seq 633 failed: aterm-ctl: ERR exited)"
    );
}

const SURVEY_ROWS: [&str; 2] = [
    "● How is Claude doing this session? (optional)",
    "  1: Bad    2: Fine   3: Good   0: Dismiss",
];

fn with_survey(mut r: Vec<String>) -> Vec<String> {
    let top = r
        .iter()
        .position(|row| row.starts_with('─'))
        .expect("the top rule");
    for (k, row) in SURVEY_ROWS.iter().enumerate() {
        r.insert(top + k, (*row).to_string());
    }
    r
}

#[test]
fn watch_says_a_survey_once_when_it_appears() {
    let mut m = Mock::new(vec![
        with_survey(idle_screen()),
        with_survey(busy_screen()),
        with_survey(said_idle("⏺ Pushed.")),
    ]);
    m.vanish_after = Some(1);
    let (lines, code) = watch_lines(&mut m, &auto(30));
    assert_eq!(
        lines,
        [
            "EVENT survey seq=101 dismiss with: aterm ctl key 'if=^●.How.is.Claude.doing' 0",
            "EVENT idle seq=101 ⏺ Done.",
            "EVENT idle seq=103 ⏺ Pushed.",
            "EXIT session gone (await seq 103 failed: aterm-ctl: ERR exited)",
        ]
    );
    assert_eq!(code, 1);
    assert!(m.presses().is_empty(), "{:?}", m.requests);
    let block = aterm_phase::survey_block(&with_survey(idle_screen())).expect("open");
    assert_eq!(
        survey_event_line(7, Some("@s-1"), &block),
        "EVENT survey seq=7 dismiss with: aterm ctl @s-1 key 'if=^●.How.is.Claude.doing' 0"
    );
}

#[test]
fn dismiss_surveys_presses_one_guarded_0() {
    let mut m = Mock::new(vec![with_survey(idle_screen()), idle_screen()]);
    m.vanish_after = Some(1);
    let opts = surveys(auto(30), true);
    let (lines, code) = watch_lines(&mut m, &opts);
    assert_eq!(
        lines,
        [
            "DISMISSED survey seq=101",
            "EVENT idle seq=102 ⏺ Done.",
            "EXIT session gone (await seq 102 failed: aterm-ctl: ERR exited)",
        ]
    );
    assert_eq!(code, 1);
    assert_eq!(m.presses(), ["key if=^●.How.is.Claude.doing 0"]);

    let mut m = Mock::new(vec![with_survey(idle_screen()), idle_screen()]);
    let opts = surveys(auto(30), true);
    let mut log: Vec<u8> = Vec::new();
    let (out, code) = session(&mut m, Some("@s-1".to_string()))
        .supervise_to(&opts, &mut log)
        .expect("supervise");
    assert_eq!(code, 0);
    assert!(out.starts_with("idle\n--\n⏺ Done.\n"), "{out}");
    assert!(!out.contains("DISMISSED"), "{out}");
    let log = String::from_utf8(log).expect("utf-8");
    assert_eq!(log, "DISMISSED survey seq=101\n");
    assert_eq!(m.presses(), ["@s-1 key if=^●.How.is.Claude.doing 0"]);
}

#[test]
fn agys_survey_is_dismissed_with_its_guarded_0() {
    use aterm_phase::agy::fixtures::lab;
    use aterm_phase::prompt::fixtures::screen;
    let card = screen(lab::SURVEY);
    let ended = screen(lab::END_OF_TURN);
    let guard = r"^.\[1\].Good..\[2\].Fine..\[3\].Bad..\[0\].Skip";
    let matcher = aterm_observe::row_matcher(guard).expect("the server compiles it");
    let up: Vec<&String> = card.iter().filter(|r| matcher.matches(r)).collect();
    assert_eq!(up, [" [1] Good  [2] Fine  [3] Bad  [0] Skip"]);
    assert!(!ended.iter().any(|r| matcher.matches(r)));

    let mut m = Mock::new(vec![card.clone()]);
    m.program = "agy";
    let mut s = session(&mut m, Some("@s-1".to_string()));
    let (turn, reader) = s.phase_now().expect("a read");
    assert_eq!(
        render_phase_and_survey(&turn, reader, &[]),
        "idle\nsurvey 0\n"
    );

    let mut m = Mock::new(vec![card.clone(), card.clone(), ended.clone()]);
    m.program = "agy";
    m.vanish_after = Some(1);
    let (lines, _) = watch_lines(&mut m, &surveys(auto(30), true));
    assert_eq!(
        m.presses(),
        [&format!("key if={guard} 0")],
        "{:?}",
        m.requests
    );
    assert!(
        lines.iter().any(|l| l.starts_with("DISMISSED survey seq=")),
        "{lines:?}"
    );
    assert!(
        !m.requests
            .iter()
            .any(|r| r.starts_with("turn") || r.starts_with("send")),
        "{:?}",
        m.requests
    );

    let mut m = Mock::new(vec![card.clone(), card, ended]);
    m.program = "agy";
    m.vanish_after = Some(1);
    let (lines, _) = watch_lines(&mut m, &surveys(auto(30), false));
    assert!(m.presses().is_empty(), "the control: {:?}", m.requests);
    let said = format!(" dismiss with: aterm ctl key 'if={guard}' 0");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("EVENT survey seq=") && l.ends_with(&said)),
        "{lines:?}"
    );
}

#[test]
fn a_skipped_survey_press_says_nothing() {
    let mut m = Mock::new(vec![with_survey(idle_screen()), idle_screen()]);
    m.key_replies.push_back(ok("OK skipped seq=105\n"));
    m.vanish_after = Some(0);
    let opts = surveys(auto(30), true);
    let (lines, code) = watch_lines(&mut m, &opts);
    assert_eq!(
        lines,
        [
            "EVENT idle seq=102 ⏺ Done.",
            "EXIT session gone (await seq 102 failed: aterm-ctl: ERR exited)",
        ]
    );
    assert_eq!(code, 1);
    assert_eq!(m.presses(), ["key if=^●.How.is.Claude.doing 0"]);
}

#[test]
fn a_survey_waits_while_a_box_is_up() {
    let mut m = fenced(vec![
        with_survey(bash_one_row()),
        with_survey(busy_screen()),
        with_survey(idle_screen()),
        idle_screen(),
    ]);
    m.vanish_after = Some(1);
    let opts = surveys(auto(30), true);
    let (lines, _) = watch_lines(&mut m, &opts);
    assert_eq!(
        lines,
        [
            "APPROVED seq=101 git log --oneline -5",
            "DISMISSED survey seq=103",
            "EVENT idle seq=104 ⏺ Done.",
            "EXIT session gone (await seq 104 failed: aterm-ctl: ERR exited)",
        ]
    );
    assert_eq!(
        m.presses(),
        [
            "key if-human=7 if-gen=1.101 if=^\\x20{3}git\\x20log\\x20--oneline\\x20-5\\s*$ 1",
            "key if-human=7 if=^●.How.is.Claude.doing 0"
        ],
        "{:?}",
        m.requests
    );
}

fn dismissing() -> SuperviseOpts {
    surveys(auto(30), true)
}

#[derive(Debug, Default)]
struct SurveyTold(std::sync::Mutex<Vec<Option<(StuckSurvey, String)>>>);

impl IdleHost for SurveyTold {
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn survey_stuck(&self, stuck: Option<(StuckSurvey, &str)>) {
        self.0
            .lock()
            .unwrap()
            .push(stuck.map(|(kind, text)| (kind, text.to_string())));
    }
}

#[derive(Debug)]
struct LooksAt(Option<Instant>);

impl IdleHost for LooksAt {
    fn wants(&self) -> bool {
        false
    }
    fn wants_at(&self) -> Option<Instant> {
        self.0
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
}

#[test]
fn a_look_the_host_scheduled_ends_the_idle_wait_step_at_its_instant() {
    let longest = |at: Option<Instant>| {
        let mut m = Mock::new(vec![idle_screen()]);
        m.vanish_after = Some(1);
        let opts = SuperviseOpts {
            idle_host: Some(Arc::new(LooksAt(at)) as Arc<dyn IdleHost>),
            ..auto(30)
        };
        let _ = watch_lines(&mut m, &opts);
        m.requests
            .iter()
            .filter(|r| r.starts_with("await "))
            .filter_map(|r| r.rsplit_once("timeout ")?.1.parse::<u64>().ok())
            .max()
            .unwrap_or(0)
    };
    let soon = longest(Some(Instant::now() + Duration::from_secs(3)));
    assert!(soon <= 3_000, "the step outlasted the look: {soon} ms");
    let none = longest(None);
    assert!(
        none > 3_000,
        "a host with no look keeps the step: {none} ms"
    );
}

#[derive(Debug)]
struct LookDue {
    clock: Arc<super::super::fx::ScriptedClock>,
    at: Instant,
    took: AtomicBool,
}

impl IdleHost for LookDue {
    fn wants(&self) -> bool {
        !self.took.load(Ordering::SeqCst) && self.clock.mono() >= self.at
    }
    fn wants_at(&self) -> Option<Instant> {
        (!self.took.load(Ordering::SeqCst)).then_some(self.at)
    }
    fn at_idle(&self) -> Option<HostStep> {
        self.took.store(true, Ordering::SeqCst);
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
}

#[test]
fn the_wake_at_a_host_look_steps_the_host_and_decides_no_turn_end() {
    let run = |look: bool| {
        let clock = virtual_clock();
        let host = Arc::new(LookDue {
            clock: Arc::clone(&clock),
            at: clock.mono() + Duration::from_millis(300),
            took: AtomicBool::new(false),
        });
        let mut m = Mock::new(vec![idle_screen()]);
        m.stall_sleep = Some(Duration::from_millis(50));
        m.vanish_after = Some(30);
        m.cursor_on_caret = true;
        let opts = SuperviseOpts {
            idle_host: look.then(|| Arc::clone(&host) as Arc<dyn IdleHost>),
            ..auto(30)
        };
        let run = record_watch_on(&mut m, &opts, clock, |_| {});
        crate::supervise::tape::testing::assert_replays(
            &run.tape,
            &run.header,
            &run.sent,
            &run.lines,
        );
        let lines = run.lines;
        let acts: Vec<String> = m
            .requests
            .iter()
            .filter(|r| matches!(verb_of(r), "turn" | "send" | "paste" | "key"))
            .cloned()
            .collect();
        (host.took.load(Ordering::SeqCst), acts, lines)
    };
    let (took, acts, lines) = run(true);
    assert!(took, "the look was never taken: {lines:#?}");
    let (_, alone, _) = run(false);
    assert_eq!(acts, alone, "the look decided a turn end: {lines:#?}");
}

fn told_surveys(opts: SuperviseOpts) -> (SuperviseOpts, Arc<SurveyTold>) {
    let told = Arc::new(SurveyTold::default());
    (
        SuperviseOpts {
            idle_host: Some(Arc::clone(&told) as Arc<dyn IdleHost>),
            ..opts
        },
        told,
    )
}

fn assert_no_attention(m: &Mock) {
    assert!(
        !m.requests
            .iter()
            .any(|r| r.starts_with("meta set attention")),
        "a survey marks no tab: {:#?}",
        m.requests
    );
}

#[test]
fn the_survey_guard_matches_the_open_survey_and_no_copy_of_it() {
    let open = with_survey(idle_screen());
    let survey_row = aterm_phase::survey_block(&open).expect("open").guard;
    let guard = aterm_observe::row_matcher(&survey_row).expect("the guard compiles");
    assert!(survey_open(&open));
    assert_eq!(
        open.iter().filter(|r| guard.matches(r)).collect::<Vec<_>>(),
        [SURVEY_ROWS[0]]
    );
    let event = survey_event_line(
        7,
        Some("@s-1"),
        &aterm_phase::survey_block(&open).expect("open"),
    );
    let help = format!("                       aterm ctl @sid key 'if={survey_row}' 0");
    for copy in [
        framed(&[
            "⏺ Bash(cat survey-open.txt)",
            "  ⎿  ● How is Claude doing this session? (optional)",
            "       1: Bad    2: Fine   3: Good   0: Dismiss",
            "",
        ]),
        framed(&[
            "⏺ Claude Code parks this above the composer:",
            "  ● How is Claude doing this session? (optional)",
            "    1: Bad    2: Fine   3: Good   0: Dismiss",
            "",
        ]),
        framed(&[&event, &help, &format!("  ⎿  {event}"), ""]),
    ] {
        assert!(!survey_open(&copy), "{copy:?}");
        assert!(!copy.iter().any(|r| guard.matches(r)), "{copy:?}");
    }
    let mut m = Mock::new(vec![framed(&[
        "⏺ Bash(cat survey-open.txt)",
        "  ⎿  ● How is Claude doing this session? (optional)",
        "",
    ])]);
    let r = session(&mut m, None)
        .dismiss_survey(101, &survey_row, None)
        .expect("answered");
    assert_eq!(r, Dismiss::Skipped);
    assert_eq!(m.requests, ["key if=^●.How.is.Claude.doing 0"]);
}

const LONG_CONTEXT_ROWS: [&str; 2] = [
    "● How well is Claude following the instructions you gave earlier in this conversation? (optional)",
    "  1: Bad    2: Fine   3: Good   0: Dismiss",
];
const PLUGIN_ROWS: [&str; 3] = [
    "● How helpful has the gh plugin been? (optional)",
    "  Used this session: the skill, the command",
    "  1: Bad    2: Fine   3: Good   4: Unsure 0: Dismiss",
];
const MEMORY_ROWS: [&str; 6] = [
    "● Claude recalled a memory:",
    "",
    "  The owner prefers small commits and reviews each diff before it lands.",
    "",
    "  How was Claude's recollection? (optional)",
    "  1: Bad    2: Fine   3: Good   4: Unsure 0: Dismiss",
];
const UNKNOWN_ROWS: [&str; 2] = [
    "● How likely are you to recommend this mode? (optional)",
    "  1: Bad    2: Fine   3: Good   0: Dismiss",
];
const DRAFTS_ROWS: [&str; 5] = [
    "✻ Bug report drafted: the stand-in title of a queued draft",
    "│ details preview line one",
    "│ details preview line two",
    "1 to review · 2 to send · 0 to dismiss",
    "",
];

fn with_card(mut r: Vec<String>, card: &[&str]) -> Vec<String> {
    let top = r
        .iter()
        .position(|row| row.starts_with('─'))
        .expect("the top rule");
    for (k, row) in card.iter().enumerate() {
        r.insert(top + k, (*row).to_string());
    }
    r
}

#[test]
fn a_persons_grace_holds_the_0_on_busy_reads_and_says_so_once() {
    let held = |hand: &'static str, human_ms: Option<u64>| {
        let mut m = Mock::new(vec![
            with_survey(busy_screen()),
            with_survey(busy_screen()),
            with_survey(busy_screen()),
        ]);
        m.hand = hand;
        m.human_ms = human_ms;
        m.stall_sleep = Some(Duration::from_millis(50));
        let (dir, path) = journal_file("survey-busy-held");
        let opts = SuperviseOpts {
            max: Duration::from_secs(1),
            journal: Some(path.clone()),
            ..surveys(auto(1), true)
        };
        let (_, _) = watch_lines(&mut m, &opts);
        assert!(m.presses().is_empty(), "{:?}", m.requests);
        let (records, _) = journal_records(&path);
        let _ = std::fs::remove_dir_all(&dir);
        records
            .into_iter()
            .map(|r| r.line)
            .filter(|l| l.starts_with("HELD") && l.contains("the survey's 0"))
            .collect::<Vec<_>>()
    };
    let person = held("-", Some(500));
    assert_eq!(
        person.len(),
        1,
        "said once, however many reads: {person:#?}"
    );
    assert!(person[0].contains("a person is typing"), "{person:#?}");
    let driver = held("lease:@s-7", None);
    assert_eq!(driver.len(), 1, "{driver:#?}");
    assert!(
        driver[0].contains("another driver's hand is on the session"),
        "{driver:#?}"
    );
}

#[test]
fn a_withheld_card_first_read_busy_is_told_at_the_idle_look_and_ended_when_it_goes() {
    let card = [
        "● How likely are you to recommend this mode?",
        "  1: Bad    2: Fine   3: Good   0: Dismiss",
    ];
    let mut m = Mock::new(vec![
        with_card(busy_screen(), &card),
        with_card(idle_screen(), &card),
        idle_screen(),
    ]);
    m.vanish_after = Some(1);
    let (opts, told) = told_surveys(dismissing());
    let (lines, _) = watch_lines(&mut m, &opts);
    assert!(m.presses().is_empty(), "{:?}", m.requests);
    assert_no_attention(&m);
    let told = told.0.lock().unwrap().clone();
    assert!(
        matches!(&told[..], [Some((StuckSurvey::Withheld, why)), None]
            if why.starts_with("a rating card aterm does not answer is open")),
        "told once at the idle look, ended with the card: {told:?}"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("EVENT survey seq=101")),
        "{lines:?}"
    );
}

#[test]
fn an_unknown_card_without_the_vendors_mark_is_said_and_never_pressed() {
    let card = [
        "● How likely are you to recommend this mode?",
        "  1: Bad    2: Fine   3: Good   0: Dismiss",
    ];
    let mut m = Mock::new(vec![with_card(idle_screen(), &card)]);
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines(&mut m, &dismissing());
    assert!(m.presses().is_empty(), "{:?}", m.requests);
    assert!(
        lines.iter().any(
            |l| l.starts_with("EVENT survey seq=101 kind=unknown not pressed")
                && !l.contains("dismiss with")
        ),
        "no command for a card that is not pressed: {lines:?}"
    );
    let card = [
        "● Claude's recall of a memory (optional)",
        "  1: Bad    2: Fine   3: Good   0: Dismiss",
    ];
    let mut m = Mock::new(vec![with_card(idle_screen(), &card)]);
    m.vanish_after = Some(0);
    let (opts, told) = told_surveys(dismissing());
    let (lines, _) = watch_lines(&mut m, &opts);
    assert!(m.presses().is_empty(), "{:?}", m.requests);
    assert!(
        lines.contains(
            &"EVENT survey seq=101 kind=unknown not pressed (its question row cannot be \
              guarded); read it with: aterm ctl text"
                .to_string()
        ),
        "{lines:?}"
    );
    assert_no_attention(&m);
    assert_eq!(
        told.0.lock().unwrap().iter().flatten().count(),
        1,
        "watch is unattended: {:#?}",
        told.0.lock().unwrap()
    );
}

#[test]
fn a_loop_that_sees_no_survey_tells_the_host_once() {
    let mut m = Mock::new(vec![idle_screen(), idle_screen(), idle_screen()]);
    m.vanish_after = Some(0);
    let (opts, told) = told_surveys(dismissing());
    let _ = watch_lines(&mut m, &opts);
    let told = told.0.lock().unwrap().clone();
    assert_eq!(told, [None], "once, with nothing to end: {told:?}");
}

#[test]
fn the_drafts_card_is_never_pressed_or_called_a_survey() {
    let mut m = Mock::new(vec![with_card(idle_screen(), &DRAFTS_ROWS)]);
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines(&mut m, &dismissing());
    assert!(m.presses().is_empty(), "{:?}", m.requests);
    assert!(!lines.iter().any(|l| l.contains("survey")), "{lines:?}");
    assert!(!survey_open(&with_card(idle_screen(), &DRAFTS_ROWS)));
}

fn workflow_screen() -> Vec<String> {
    let mut r = rows(&[
        "⏺ Second-pass reviewers are out and the build is running.",
        "✻ Waiting for 1 dynamic workflow to finish",
        "",
    ]);
    r.extend(composer(
        "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents",
    ));
    r
}

fn with_zero(mut r: Vec<String>) -> Vec<String> {
    let c = r.iter().position(|row| row == "❯").expect("composer");
    r[c] = "❯ 0".to_string();
    r
}

#[test]
fn every_kind_is_dismissed_over_a_workflow_wait_on_the_busy_read() {
    for (card, guard, kind) in [
        (&SURVEY_ROWS[..], "^●.How.is.Claude.doing", ""),
        (
            &LONG_CONTEXT_ROWS[..],
            "^●.How.well.is.Claude",
            " kind=long-context",
        ),
        (&PLUGIN_ROWS[..], "^●.How.helpful.has.the", " kind=plugin"),
        (
            &MEMORY_ROWS[..],
            "^●.Claude.recalled.a.memory:",
            " kind=memory",
        ),
        (&UNKNOWN_ROWS[..], "^●.How.likely.are.you", " kind=unknown"),
    ] {
        let mut m = Mock::new(vec![with_card(workflow_screen(), card), workflow_screen()]);
        m.vanish_after = Some(1);
        let (lines, _) = watch_lines(&mut m, &dismissing());
        assert_eq!(
            m.presses(),
            [&format!("key if={guard} 0")],
            "{card:?}: {:?}",
            m.requests
        );
        assert!(
            lines.contains(&format!("DISMISSED survey seq=101{kind}")),
            "{card:?}: {lines:?}"
        );
        assert!(
            !lines.iter().any(|l| l.starts_with("EVENT idle")),
            "{card:?}: {lines:?}"
        );
    }
}

#[test]
fn a_late_vendor_timer_takes_the_0_inside_the_landing_time_and_is_never_backspaced() {
    for debounce_ms in [40, 220] {
        let base = workflow_screen();
        let mut m = Mock::new(vec![
            with_survey(base.clone()),
            with_zero(with_survey(base.clone())),
            base,
        ]);
        m.card_debounce = Some(Duration::from_millis(debounce_ms));
        m.vanish_after = Some(1);
        m.stall_sleep = Some(Duration::from_millis(500));
        let (lines, _) = watch_lines_with(&mut m, &dismissing(), |s| {
            s.set_survey_land(Duration::from_millis(400));
        });
        assert_eq!(
            m.presses(),
            ["key if=^●.How.is.Claude.doing 0"],
            "debounce {debounce_ms} ms: {:#?}",
            m.requests
        );
        assert!(
            lines.iter().any(|l| l.starts_with("DISMISSED survey seq=")),
            "debounce {debounce_ms} ms: {lines:?}"
        );
        assert!(
            !lines.iter().any(|l| l.starts_with("EVENT survey")),
            "no noise: {lines:?}"
        );
    }
}

#[test]
fn a_busy_read_presses_a_card_once_and_leaves_the_ladder_to_the_idle_look() {
    let script = |base: &dyn Fn() -> Vec<String>| {
        let mut v = vec![with_survey(base()), with_zero(with_survey(base()))];
        v.extend((0..6).map(|_| with_survey(base())));
        v.push(base());
        v
    };
    let run = |base: &dyn Fn() -> Vec<String>| {
        let mut m = Mock::new(script(base));
        m.vanish_after = Some(1);
        let _ = watch_lines_with(&mut m, &dismissing(), |s| {
            s.set_survey_retry([Duration::ZERO; SURVEY_TRIES]);
        });
        m
    };
    let busy = run(&workflow_screen);
    assert_eq!(
        busy.presses(),
        ["key if=^●.How.is.Claude.doing 0", "key backspace"],
        "one 0 and its backspace, then nothing from busy reads: {:#?}",
        busy.requests
    );
    let idle = run(&idle_screen);
    assert!(
        idle.presses()
            .iter()
            .filter(|r| r.contains("if=^●"))
            .count()
            >= 2,
        "the idle look goes on down the ladder: {:#?}",
        idle.requests
    );
}

#[test]
fn a_refused_backspace_keeps_the_0_owed_until_the_server_takes_it() {
    for (refusal, hold) in [("halted reason=owner origin=local", true), ("rate", false)] {
        let mut m = Mock::new(vec![
            with_survey(idle_screen()),
            with_zero(with_survey(idle_screen())),
            with_zero(with_survey(idle_screen())),
            with_zero(with_survey(idle_screen())),
            idle_screen(),
        ]);
        m.key_replies.push_back(ok("OK seq=101\n"));
        m.key_replies.push_back(err(refusal));
        if hold {
            m.hold = "1";
            m.hold_lifts_after = Some(1);
        }
        m.vanish_after = Some(0);
        let (lines, _) = watch_lines(&mut m, &dismissing());
        let backspaces = m.requests.iter().filter(|r| *r == "key backspace").count();
        assert_eq!(
            backspaces, 2,
            "{refusal}: the removal is tried again, not forgotten, and only until it is \
             taken: {:#?}",
            m.requests
        );
        assert!(
            lines.iter().all(|l| !l.contains("DISMISSED")),
            "{refusal}: {lines:?}"
        );
    }
}

#[test]
fn a_survey_whose_0_did_not_take_is_tried_again_until_it_goes() {
    let mut m = Mock::new(vec![
        with_survey(idle_screen()),
        with_survey(idle_screen()),
        with_survey(idle_screen()),
        with_survey(idle_screen()),
        with_survey(idle_screen()),
        idle_screen(),
    ]);
    m.vanish_after = Some(1);
    let (opts, told) = told_surveys(dismissing());
    let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
        s.set_survey_retry([Duration::ZERO; SURVEY_TRIES]);
    });
    assert_eq!(
        m.presses().len(),
        3,
        "tried again after each back-off: {:#?}",
        m.requests
    );
    assert!(
        lines.iter().any(|l| l.starts_with("DISMISSED survey seq=")),
        "{lines:#?}"
    );
    assert_no_attention(&m);
    let told = told.0.lock().unwrap().clone();
    assert!(
        matches!(&told[..], [Some((StuckSurvey::Unclosed, why)), None]
            if why.starts_with("the session survey did not close")),
        "told from the second failure, ended when it went: {told:?}"
    );
    let mut m = Mock::new(vec![
        with_survey(idle_screen()),
        with_survey(idle_screen()),
        idle_screen(),
        with_zero(idle_screen()),
    ]);
    m.vanish_after = Some(1);
    let _ = watch_lines(&mut m, &dismissing());
    assert_eq!(
        m.presses(),
        ["key if=^●.How.is.Claude.doing 0"],
        "{:#?}",
        m.requests
    );
    let mut m = Mock::new(vec![
        with_survey(idle_screen()),
        with_survey(bash_one_row()),
        with_zero(with_survey(idle_screen())),
        idle_screen(),
    ]);
    m.vanish_after = Some(0);
    let _ = watch_lines(&mut m, &dismissing());
    assert_eq!(
        m.presses(),
        ["key if=^●.How.is.Claude.doing 0", "key backspace"],
        "{:#?}",
        m.requests
    );
    let mut m = Mock::new(vec![with_survey(idle_screen())]);
    m.vanish_after = Some(1);
    let _ = watch_lines(&mut m, &surveys(auto(30), false));
    assert!(m.presses().is_empty(), "{:#?}", m.requests);
}

#[test]
fn a_0_that_landed_in_the_composer_is_backspaced_not_dismissed() {
    let mut zero = idle_screen();
    let c = zero.iter().position(|r| r == "❯").expect("composer");
    zero[c] = "❯ 0".to_string();
    let mut m = Mock::new(vec![with_survey(idle_screen()), zero, idle_screen()]);
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines(&mut m, &dismissing());
    assert_eq!(
        lines,
        [
            "EVENT idle seq=103 ⏺ Done.",
            "EXIT session gone (await seq 103 failed: aterm-ctl: ERR exited)",
        ]
    );
    assert_eq!(
        m.presses(),
        ["key if=^●.How.is.Claude.doing 0", "key backspace"]
    );
}

#[test]
fn a_survey_back_after_a_busy_spell_is_a_new_appearance() {
    let mut m = Mock::new(vec![
        with_survey(idle_screen()),
        busy_screen(),
        with_survey(said_idle("⏺ Pushed.")),
        said_idle("⏺ Pushed."),
    ]);
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines(&mut m, &dismissing());
    assert_eq!(
        lines,
        [
            "DISMISSED survey seq=101",
            "DISMISSED survey seq=103",
            "EVENT idle seq=104 ⏺ Pushed.",
            "EXIT session gone (await seq 104 failed: aterm-ctl: ERR exited)",
        ]
    );
    assert_eq!(
        m.presses(),
        [
            "key if=^●.How.is.Claude.doing 0",
            "key if=^●.How.is.Claude.doing 0"
        ]
    );
    let mut m = Mock::new(vec![
        with_survey(idle_screen()),
        rows(&["$ targo --unverified build", "   Compiling aterm-agent"]),
        with_survey(idle_screen()),
    ]);
    m.vanish_after = Some(2);
    m.stall_sleep = Some(Duration::from_millis(400));
    let (lines, _) = watch_lines_with(&mut m, &dismissing(), |s| {
        s.set_survey_land(Duration::from_millis(300));
        s.set_mono_standing();
    });
    assert!(
        lines.iter().any(|l| l == "DISMISSED survey seq=101"),
        "{lines:#?}"
    );
}

#[test]
fn a_skipped_0_on_a_survey_still_open_is_handed_over() {
    let mut m = Mock::new(vec![with_survey(idle_screen())]);
    m.key_replies.push_back(ok("OK skipped seq=101\n"));
    m.vanish_after = Some(1);
    let (lines, _) = watch_lines(&mut m, &dismissing());
    assert_eq!(
        lines,
        [
            "EVENT survey seq=102 dismiss with: aterm ctl key 'if=^●.How.is.Claude.doing' 0",
            "EVENT idle seq=102 ⏺ Done.",
            "EXIT session gone (await seq 102 failed: aterm-ctl: ERR exited)",
        ]
    );
    assert_eq!(m.presses(), ["key if=^●.How.is.Claude.doing 0"]);
}

#[test]
fn a_draft_of_more_than_one_row_is_typed() {
    let draft = |composer_rows: &[&str], row: usize, col: usize, first: usize| {
        let mut r = rows(&["⏺ Done.", "", "✻ Cogitated for 4s · done 2:41 PM", ""]);
        let top = r.len();
        r.push("─".repeat(120));
        r.extend(rows(composer_rows));
        r.push("─".repeat(120));
        r.push("  ? for shortcuts".to_string());
        Screen {
            rows: r,
            cursor_row: first + top + 1 + row,
            cursor_col: col,
            first,
            ..Screen::default()
        }
    };
    for first in [0, 30] {
        assert!(typed_draft(
            &aterm_phase::ClaudeReader,
            &draft(&["❯ Fix the parser", ""], 1, 2, first)
        ));
        assert!(typed_draft(
            &aterm_phase::ClaudeReader,
            &draft(&["❯ Fix the parser", "  and the lexer"], 1, 2, first)
        ));
        assert!(typed_draft(
            &aterm_phase::ClaudeReader,
            &draft(&["❯", "  and the lexer"], 1, 15, first)
        ));
        assert!(typed_draft(
            &aterm_phase::ClaudeReader,
            &draft(&["❯ hi"], 0, 4, first)
        ));
        assert!(!typed_draft(
            &aterm_phase::ClaudeReader,
            &draft(&["❯ Try \"fix lint errors\""], 0, 2, first)
        ));
        assert!(!typed_draft(
            &aterm_phase::ClaudeReader,
            &draft(&["❯"], 0, 2, first)
        ));
    }
    let mut off = draft(&["❯ Try \"fix lint errors\""], 0, 2, 0);
    off.cursor_row = 99;
    assert!(!typed_draft(&aterm_phase::ClaudeReader, &off));
}

#[test]
fn every_word_in_a_composer_with_no_placeholder_is_typed() {
    use aterm_phase::agy::fixtures as a;
    let ended = aterm_phase::prompt::fixtures::screen(a::END_OF_TURN);
    let caret = aterm_phase::agy::composer(&ended).expect("the composer");
    let at_home = |rows: &[String]| Screen {
        rows: rows.to_vec(),
        cursor_row: caret,
        cursor_col: 2,
        ..Screen::default()
    };
    let mut drafted = ended.clone();
    drafted[caret] = "> hello there".to_string();
    let agy = &aterm_phase::AgyReader;
    assert!(typed_draft(agy, &at_home(&drafted)));
    assert!(!typed_draft(agy, &at_home(&ended)));
    let composer = agy.composer(&drafted);
    assert!(!typed_draft_in(composer.as_ref(), true, &at_home(&drafted)));
}

#[test]
fn gemini_clis_placeholder_is_no_draft_and_its_draft_is() {
    use aterm_phase::gemini::fixtures as g;
    use aterm_phase::prompt::fixtures::screen;
    let gemini = &aterm_phase::GeminiReader;
    let at = |rows: &[String], col: usize| Screen {
        rows: rows.to_vec(),
        cursor_row: gemini.composer(rows).expect("the composer").0,
        cursor_col: col,
        ..Screen::default()
    };
    let idle = screen(g::IDLE);
    let draft = screen(g::DRAFT);
    let mut m = Mock::new(Vec::new());
    let mut s = session(&mut m, None);
    for col in [2, 3] {
        assert!(!typed_draft(gemini, &at(&idle, col)), "column {col}");
        let c = gemini.composer(&idle);
        assert_eq!(
            s.composer_draft(
                c.as_ref(),
                gemini.placeholder(),
                gemini.placeholder_fg(),
                &at(&idle, col)
            ),
            Some(false),
            "column {col}"
        );
    }
    for col in [3, 3 + "a draft the lab never sends".len()] {
        assert!(typed_draft(gemini, &at(&draft, col)), "column {col}");
        let c = gemini.composer(&draft);
        assert_eq!(
            s.composer_draft(
                c.as_ref(),
                gemini.placeholder(),
                gemini.placeholder_fg(),
                &at(&draft, col)
            ),
            Some(true),
            "column {col}"
        );
    }
    drop(s);
    assert!(
        !m.requests.iter().any(|r| r.starts_with("cell ")),
        "{:#?}",
        m.requests
    );
    let caret = gemini.composer(&idle).expect("the composer").0;
    let words = (
        caret,
        vec!["Type your message or @path/to/file".to_string()],
    );
    assert!(
        typed_draft_in(Some(&words), gemini.placeholder(), &at(&idle, 3)),
        "the control"
    );
}

fn with_context(mut r: Vec<String>, left: u8) -> Vec<String> {
    let top = r
        .iter()
        .position(|row| row.starts_with('─'))
        .expect("the top rule");
    let text = format!("{left}% until auto-compact");
    let row = format!("{}{text}", " ".repeat(118 - text.chars().count()));
    r.insert(top, row);
    r
}

fn descent() -> Vec<Vec<String>> {
    vec![
        with_context(busy_screen(), 15),
        with_context(busy_screen(), 9),
        with_context(busy_screen(), 4),
        busy_screen(),
        with_context(busy_screen(), 12),
        with_context(said_idle("⏺ Pushed."), 8),
    ]
}

#[test]
fn watch_says_a_descent_once_and_the_compaction_once() {
    let mut m = Mock::new(descent());
    m.vanish_after = Some(0);
    let (lines, code) = watch_lines(&mut m, &auto(30));
    assert_eq!(
        lines,
        [
            "EVENT context seq=102 9% until auto-compact",
            "EVENT compacted seq=104",
            "EVENT context seq=106 8% until auto-compact",
            "EVENT idle seq=106 ⏺ Pushed.",
            "EXIT session gone (await seq 106 failed: aterm-ctl: ERR exited)",
        ]
    );
    assert_eq!(code, 1);
    assert!(m.presses().is_empty(), "{:?}", m.requests);
}

#[test]
fn context_warn_0_says_neither_line() {
    let mut screens = descent();
    screens.insert(3, with_context(busy_screen(), 0));
    let mut m = Mock::new(screens);
    m.vanish_after = Some(0);
    let opts = SuperviseOpts {
        context_warn: 0,
        ..auto(30)
    };
    let (lines, _) = watch_lines(&mut m, &opts);
    assert_eq!(
        lines,
        [
            "EVENT idle seq=107 ⏺ Pushed.",
            "EXIT session gone (await seq 107 failed: aterm-ctl: ERR exited)",
        ]
    );

    let mut m = Mock::new(descent());
    let mut log: Vec<u8> = Vec::new();
    let mut s = session(&mut m, None);
    s.context_warn = 10;
    let turn = s
        .await_turn_to(Duration::from_secs(30), &mut log)
        .expect("turn");
    assert_eq!(turn.phase, Phase::Idle);
    assert!(log.is_empty(), "{}", String::from_utf8_lossy(&log));
}

#[test]
fn a_jump_of_30_points_is_a_compaction_and_a_box_is_not() {
    let mut m = Mock::new(vec![
        with_context(busy_screen(), 9),
        write_prompt(),
        with_context(busy_screen(), 38),
        with_context(busy_screen(), 50),
        with_context(busy_screen(), 80),
        with_context(said_idle("⏺ Pushed."), 60),
    ]);
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines(&mut m, &auto(30));
    assert_eq!(
        lines,
        [
            "EVENT context seq=101 9% until auto-compact",
            "EVENT prompt seq=102 kind=bash classify=not-read-only:not read-only (space reading): rm \
             command=rm -rf target",
            "EVENT compacted seq=105",
            "EVENT idle seq=106 ⏺ Pushed.",
            "EXIT session gone (await seq 106 failed: aterm-ctl: ERR exited)",
        ]
    );
}

fn narrowed(r: Vec<String>, width: usize, left: u8) -> Vec<String> {
    let mut r: Vec<String> = r
        .into_iter()
        .map(|row| {
            if row.starts_with('─') {
                "─".repeat(width)
            } else {
                row
            }
        })
        .collect();
    let top = r
        .iter()
        .position(|row| row.starts_with('─'))
        .expect("the top rule");
    let text = format!("Context left until auto-compact: {left}%");
    r.insert(
        top,
        format!("{}{text}", " ".repeat(width - 2 - text.chars().count())),
    );
    r
}

#[test]
fn a_narrowed_pane_is_not_a_compaction() {
    let idle = narrowed(idle_screen(), 55, 8);
    let turn = Turn {
        phase: worker_phase(&idle),
        screen: Screen {
            rows: idle.clone(),
            ..Screen::default()
        },
        timed_out: false,
    };
    assert_eq!(
        render_phase_and_survey(&turn, by_screen(&turn), &[]),
        "idle\ncontext 8%\n"
    );
    let mut m = Mock::new(vec![
        with_context(busy_screen(), 9),
        narrowed(busy_screen(), 55, 8),
        idle,
    ]);
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines(&mut m, &auto(30));
    assert_eq!(
        lines,
        [
            "EVENT context seq=101 9% until auto-compact",
            "EVENT idle seq=103 ⏺ Done.",
            "EXIT session gone (await seq 103 failed: aterm-ctl: ERR exited)",
        ]
    );
}

#[test]
fn supervise_watches_one_run() {
    let run = |screens: Vec<Vec<String>>| {
        let mut m = Mock::new(screens);
        let mut log: Vec<u8> = Vec::new();
        let (out, code) = session(&mut m, Some("@s-1".to_string()))
            .supervise_to(&auto(30), &mut log)
            .expect("supervise");
        assert_eq!(code, 0);
        assert!(out.starts_with("idle\n"), "{out}");
        String::from_utf8(log).expect("utf-8")
    };
    assert_eq!(
        run(vec![
            with_context(busy_screen(), 9),
            with_context(idle_screen(), 8)
        ]),
        "EVENT context seq=101 9% until auto-compact\n"
    );
    assert_eq!(
        run(vec![
            with_context(busy_screen(), 8),
            busy_screen(),
            idle_screen()
        ]),
        "EVENT context seq=101 8% until auto-compact\nEVENT compacted seq=102\n"
    );
    assert_eq!(run(vec![busy_screen(), idle_screen()]), "");
}

#[test]
fn the_context_watch_is_inclusive_and_needs_the_frame() {
    let mut question = rows(&[
        "⏺ Which model?",
        "",
        " Select a model",
        " ❯ 1. Opus",
        "   2. Sonnet",
        "",
        " Esc to cancel",
    ]);
    question.extend(rows(&["", "  ? for shortcuts"]));
    let reads: Vec<(Vec<String>, &[&str])> = vec![
        (with_context(busy_screen(), 11), &[]),
        (
            with_context(busy_screen(), 10),
            &["EVENT context seq=2 10% until auto-compact"],
        ),
        (vec![String::new(); 8], &[]),
        (
            rows(&[
                "   Compiling aterm-agent v0.84.0",
                "    Finished `test` profile [unoptimized + debuginfo]",
                "$ ",
            ]),
            &[],
        ),
        (question, &[]),
        (with_context(busy_screen(), 39), &[]),
        (busy_screen(), &["EVENT compacted seq=7"]),
        (
            with_context(busy_screen(), 10),
            &["EVENT context seq=8 10% until auto-compact"],
        ),
    ];
    let mut m = Mock::new(vec![]);
    let mut s = session(&mut m, None);
    s.context_warn = 10;
    for (k, (rows, want)) in reads.into_iter().enumerate() {
        let seq = k as u64 + 1;
        let screen = Screen {
            rows,
            seq,
            ..Screen::default()
        };
        let mut said: Vec<String> = Vec::new();
        let reader = s.reader(&screen.rows);
        s.watch_context(&screen, reader, &mut |line| {
            said.push(line.to_string());
            Ok(())
        })
        .expect("said");
        assert_eq!(said, want, "read {seq}");
    }
}

fn quick(m: &mut Mock, window: Duration) -> Session<'_, Mock> {
    let mut s = session(m, None);
    s.set_reconnect(window);
    s.setup.pause = Duration::from_millis(5);
    s.setup.pause_max = Duration::from_millis(20);
    s
}
fn watch_quick(m: &mut Mock, window: Duration) -> (Vec<String>, u8) {
    watch_quick_with(m, window, &auto(30))
}
fn watch_quick_with(m: &mut Mock, window: Duration, opts: &SuperviseOpts) -> (Vec<String>, u8) {
    let mut out: Vec<u8> = Vec::new();
    let from = m.order.len();
    let (tape, mut taped) = memory_tape(m);
    let mut s = taped_session(&mut taped, &tape);
    s.set_reconnect(window);
    s.setup.pause = Duration::from_millis(5);
    s.setup.pause_max = Duration::from_millis(20);
    let header = run_header(&s, opts);
    tape.core().reroot(header.roots.clone());
    let code = s.watch(opts, &mut out);
    drop(s);
    drop(taped);
    tape.finish(&crate::supervise::tape::replay::watch_end(code));
    let text = String::from_utf8(out).expect("utf-8");
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    crate::supervise::tape::testing::assert_replays(&tape, &header, &sent(m, from), &lines);
    (lines, code)
}
fn auto_ms(ms: u64) -> SuperviseOpts {
    SuperviseOpts {
        max: Duration::from_millis(ms),
        ..auto(0)
    }
}
fn reconnected_ms(line: &str) -> u128 {
    line.strip_prefix("RECONNECTED after ")
        .and_then(|r| r.strip_suffix(" ms"))
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("not a RECONNECTED line: {line}"))
}
fn said_idle(said: &str) -> Vec<String> {
    let mut r = rows(&[said, "", "✻ Cogitated for 2s · done 2:44 PM", ""]);
    r.extend(composer("  ? for shortcuts"));
    r
}
const CLOSED: &str = "server closed the connection without responding";

#[test]
fn a_request_not_served_is_told_from_a_server_answer() {
    let refused = "connect /d/aterm-7.sock: Connection refused (os error 61) — aterm \
                   isn't running (nothing is serving this control socket); launch \
                   aterm.app (`open -a aterm`), then `aterm ctl instances` names its pid";
    let gone = "connect /d/latest: No such file or directory (os error 2) — aterm isn't \
                running (nothing is serving this control socket); launch aterm.app \
                (`open -a aterm`) and retry";
    for r in [
        closed(),
        failed(1, "server hung up before the complete response"),
        failed(1, "server hung up before the complete line-framed response"),
        failed(
            1,
            "server hung up before the complete guarded artifact response",
        ),
        failed(1, "server hung up mid-body (got 12 of 4096 bytes)"),
        failed(1, refused),
        failed(1, gone),
        failed(1, "Connection reset by peer (os error 54)"),
        failed(1, "Broken pipe (os error 32)"),
        failed(124, "Resource temporarily unavailable (os error 35)"),
        failed(
            124,
            "no reply from /d/aterm-7.sock within 900s (connected; the server did not \
             answer) — the request timed out",
        ),
        failed(
            124,
            "reply from /d/aterm-7.sock stopped part-way: nothing more within 900s \
             (connected; the server began its answer, then went quiet) — the request \
             timed out",
        ),
        CtlReply {
            code: 2,
            stdout: String::new(),
            stderr: "no answer from /d/aterm-7.sock within 10s: the instance took no \
                     request, so none was sent — its control listener is not taking \
                     connections, or the process is not running (stopped, App Nap, a \
                     saturated machine). Retry; if it persists while that aterm is \
                     running, restart it.\n"
                .to_string(),
        },
        err("control server busy; retry"),
        err("auth"),
        err("main thread stalled 12138s; retry"),
        err("input dispatch failed: main thread stalled 7s; retry"),
    ] {
        assert!(r.lost(), "{r:?}");
    }
    for r in [
        ok("OK\n"),
        timeout(),
        err("no such session"),
        err("exited"),
        err("halted"),
        err("authority revoked"),
        err("input dispatch failed: event loop gone"),
        err("main thread stalledness"),
        bare_err(),
        usage("text [--json] [trim]"),
        failed(
            1,
            "cannot resolve the control-socket directory ($XDG_RUNTIME_DIR and $HOME are \
             both unset); pass --sock",
        ),
        failed(
            1,
            "connect /d/aterm-7.sock: Operation not permitted (os error 1)",
        ),
        failed(1, "stream did not contain valid UTF-8"),
    ] {
        assert!(!r.lost(), "{r:?}");
    }
    let answered = CtlReply {
        code: 1,
        stdout: String::new(),
        stderr: "note: Connection refused once, retried\naterm-ctl: ERR exited\n".to_string(),
    };
    assert!(!answered.lost());

    let mut m = Mock::new(vec![idle_screen()]);
    let mut s = session(&mut m, None);
    assert!(
        !s.unserved(&err("no such session")),
        "outside an outage the session is gone"
    );
    s.outage = Some(Outage {
        since: Instant::now(),
        kind: "await seq".to_string(),
        told: true,
        back: false,
        pause: RECONNECT_PAUSE,
    });
    assert!(
        s.unserved(&err("no such session")),
        "in one it is not hosted yet"
    );
    assert!(!s.unserved(&err("exited")));
    assert!(!s.unserved(&err("halted")));
}

fn replaced(note: &str) -> CtlReply {
    failed(i32::from(aterm_ctl::EXIT_REPLACED), note)
}
const REPLACED_ANCHOR: &str = "aterm was replaced (pid 10 -> 11) before it \
                               answered; `seq 31` names a position in the old process — \
                               re-issue it (exit 75)";
const REPLACED_GONE: &str = "aterm (pid 10) went away and nothing replaced it within 30.0 s; \
                             the exchange cannot be resumed (exit 75)";

#[test]
fn exit_75_is_a_request_not_served() {
    for note in [
        REPLACED_ANCHOR,
        REPLACED_GONE,
        "aterm was replaced (pid 10 -> 11) before it answered; a `turn` TYPES: \
         its text may already have been typed, and submitted, in the session — read the \
         screen (`text`) before sending it again; never retry it blind (exit 75)",
    ] {
        assert!(replaced(note).lost(), "{note}");
    }
    assert!(!failed(1, REPLACED_ANCHOR).lost());
}

#[cfg(unix)]
#[test]
fn await_turn_on_the_per_request_client_rides_out_exit_75() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = crate::supervise::test_scratch_path("supervise", "ctl-exit-75");
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let rows: Vec<String> = idle_screen()
        .iter()
        .map(|r| format!("\"{}\"", r.replace('\\', "\\\\").replace('"', "\\\"")))
        .collect();
    std::fs::write(
        dir.join("screen.json"),
        format!(
            "{{\"rows\":[{}],\"cursor\":{{\"row\":10,\"col\":2}},\"seq\":7}}\n",
            rows.join(",")
        ),
    )
    .expect("write the screen");
    let ctl = dir.join("aterm-ctl");
    std::fs::write(
        &ctl,
        format!(
            "#!/bin/sh\nd='{}'\necho \"$*\" >> \"$d/asked\"\nshift\ncase \"$1\" in\n  \
             await)\n    if [ ! -e \"$d/cut\" ]; then\n      : > \"$d/cut\"\n      \
             echo 'aterm-ctl: {REPLACED_ANCHOR}' >&2\n      exit 75\n    fi\n    echo OK ;;\n  \
             text) cat \"$d/screen.json\" ;;\n  *) echo OK ;;\nesac\n",
            dir.display()
        ),
    )
    .expect("write the fake client");
    std::fs::set_permissions(&ctl, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    let mut client = crate::CtlClient::new(&ctl, None);
    let mut s = Session::new(&mut client, Some("@s-w".to_string()));
    s.set_reconnect(Duration::from_secs(60));
    s.setup.pause = Duration::from_millis(5);
    let mut log: Vec<u8> = Vec::new();
    let turn = s
        .await_turn_to(Duration::from_secs(60), &mut log)
        .expect("the update is ridden out");
    assert_eq!(turn.phase, Phase::Idle);
    let log = String::from_utf8(log).expect("utf-8");
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 2, "{log}");
    assert!(
        lines[0].starts_with("RECONNECT await ") && lines[0].contains("aterm was replaced"),
        "{log}"
    );
    assert!(lines[1].starts_with("RECONNECTED after "), "{log}");
    let asked = std::fs::read_to_string(dir.join("asked")).expect("requests");
    assert!(
        asked.lines().all(|l| l.starts_with("@s-w ")),
        "every request names the worker: {asked}"
    );
    std::fs::remove_dir_all(&dir).expect("remove the scratch dir");
}

#[test]
fn watch_rides_out_a_lost_connection_to_the_next_event() {
    let mut m = Mock::new(vec![busy_screen(), busy_screen(), question_screen()]);
    for i in 2..=4 {
        m.by_index.insert(i, closed());
    }
    m.handoff_seq = Some(20);
    m.vanish_after = Some(0);
    let (lines, code) = watch_quick(&mut m, Duration::from_secs(5));
    assert_eq!(lines.len(), 4, "{lines:?}");
    assert_eq!(
        lines[0],
        format!("RECONNECT await gone esc.to.interrupt rows 2 7 failed: aterm-ctl: {CLOSED}")
    );
    reconnected_ms(&lines[1]);
    assert_eq!(
        lines[2..],
        [
            "EVENT question seq=22 ⏺ Did the suite pass on your machine?",
            "EXIT session gone (await seq 22 failed: aterm-ctl: ERR exited)",
        ]
    );
    assert_eq!(code, 1);
    assert_eq!(
        m.requests,
        [
            "meta",
            "await gone esc.to.interrupt timeout 3000",
            "text --json tail=40",
            "await gone esc.to.interrupt rows 2 7 timeout 20000",
            "text --json tail=40",
            "text --json tail=40",
            "text --json tail=40",
            "await idle 500 timeout 1500",
            "text --json tail=40",
            "modes",
            "offscreen tail=1",
            "meta",
            "status",
            "meta set attention owner=supervisor claude question: Did the suite pass on your machine? (the worker \
             asked a question; answer_questions is off)",
            "await seq 22 timeout 20000",
        ],
        "{:?}",
        m.requests
    );
}

#[test]
fn fx_virtual_clock_rides_out_an_hour_long_outage() {
    use super::super::fx::{ClockSource, Fx, VirtualClock};
    const FAILED: usize = 8;
    let (pause, pause_max) = (Duration::from_secs(60), Duration::from_secs(600));
    let behind = Instant::now()
        .checked_sub(Duration::from_secs(86_400))
        .expect("the monotonic clock reaches a day back");
    let clock = Arc::new(VirtualClock::new(behind, 1_790_000_000_000));
    let ridden = Arc::clone(&clock);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut m = Mock::new(vec![busy_screen(), busy_screen(), question_screen()]);
        for i in 2..=FAILED {
            m.by_index.insert(i, closed());
        }
        m.handoff_seq = Some(20);
        m.vanish_after = Some(0);
        let mut s = session(&mut m, None);
        s.set_fx(Fx::over(ClockSource::Virtual(ridden)));
        s.set_reconnect(Duration::from_secs(3600));
        s.setup.pause = pause;
        s.setup.pause_max = pause_max;
        let mut out: Vec<u8> = Vec::new();
        let code = s.watch(&auto(2 * 3600), &mut out);
        let _ = tx.send((String::from_utf8(out).expect("utf-8"), code));
    });
    let (log, code) = rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN).expect(
        "the outage was not ridden out within a minute: a sleep bypassed the session's Fx and \
         waited out the virtual pauses for real",
    );
    let lines: Vec<&str> = log.lines().collect();
    let pauses: Duration = (0..FAILED - 1)
        .map(|n| pause.saturating_mul(1 << n).min(pause_max))
        .sum();
    assert_eq!(pauses, Duration::from_secs(2700));
    assert_eq!(
        lines,
        [
            format!("RECONNECT await gone esc.to.interrupt rows 2 7 failed: aterm-ctl: {CLOSED}"),
            format!("RECONNECTED after {} ms", pauses.as_millis()),
            "EVENT question seq=22 ⏺ Did the suite pass on your machine?".to_string(),
            "EXIT session gone (await seq 22 failed: aterm-ctl: ERR exited)".to_string(),
        ],
        "a `TIMEOUT` or a day's `RECONNECTED` is a clock read that bypassed the session's Fx \
         (it read the real clock, a day ahead of the virtual one): {log}"
    );
    assert_eq!(code, 1);
    assert!(
        clock.passed() >= pauses,
        "the pauses were slept on the virtual clock: {:?}",
        clock.passed()
    );
}

#[test]
fn after_an_outage_the_point_is_reported_again_and_waited_past_on_the_new_seq() {
    let pushed = said_idle("⏺ Pushed.");
    let mut m = Mock::new(vec![
        idle_screen(),
        idle_screen(),
        idle_screen(),
        busy_screen(),
        pushed,
    ]);
    m.by_index.insert(2, closed());
    m.handoff_seq = Some(10);
    m.vanish_after = Some(0);
    let (lines, code) = watch_quick(&mut m, Duration::from_secs(5));
    assert_eq!(lines.len(), 6, "{lines:?}");
    assert_eq!(lines[0], "EVENT idle seq=101 ⏺ Done.");
    assert_eq!(
        lines[1],
        format!("RECONNECT await seq 101 failed: aterm-ctl: {CLOSED}")
    );
    reconnected_ms(&lines[2]);
    assert_eq!(
        lines[3..],
        [
            "EVENT idle seq=12 ⏺ Done.",
            "EVENT idle seq=14 ⏺ Pushed.",
            "EXIT session gone (await seq 14 failed: aterm-ctl: ERR exited)",
        ]
    );
    assert_eq!(code, 1);
    assert_eq!(
        m.requests,
        [
            "meta",
            "await gone esc.to.interrupt timeout 3000",
            "text --json tail=40",
            "await seq 101 timeout 20000",
            "text --json tail=40",
            "await idle 500 timeout 1500",
            "text --json tail=40",
            "await seq 12 timeout 20000",
            "await idle 500 timeout 1500",
            "text --json tail=40",
            "await gone esc.to.interrupt rows 2 7 timeout 20000",
            "text --json tail=40",
            "modes",
            "offscreen tail=1",
            "await seq 14 timeout 20000",
        ],
        "{:?}",
        m.requests
    );
}

#[test]
fn a_count_that_started_over_with_no_request_failing_is_seen_moved() {
    let mut m = Mock::new(vec![idle_screen(), said_idle("⏺ New reply on B.")]);
    m.restart = Some((2, 4));
    m.vanish_after = Some(1);
    let (lines, code) = watch_quick(&mut m, Duration::from_secs(5));
    assert_eq!(
        lines,
        [
            "EVENT idle seq=101 ⏺ Done.",
            "EVENT idle seq=6 ⏺ New reply on B.",
            "EXIT session gone (await seq 6 failed: aterm-ctl: ERR exited)",
        ]
    );
    assert_eq!(code, 1);
    assert_eq!(
        m.requests,
        [
            "meta",
            "await gone esc.to.interrupt timeout 3000",
            "text --json tail=40",
            "await seq 101 timeout 20000",
            "text --json tail=40",
            "await idle 500 timeout 1500",
            "text --json tail=40",
            "await seq 6 timeout 20000",
        ],
        "{:?}",
        m.requests
    );
}

#[test]
fn a_connection_that_never_comes_back_ends_when_the_window_lapses() {
    for (down, words) in [(closed(), CLOSED), (replaced(REPLACED_GONE), REPLACED_GONE)] {
        never_comes_back(down, words);
    }
}

fn never_comes_back(down: CtlReply, words: &str) {
    let mut m = Mock::new(vec![question_screen()]);
    m.down = Some((5, down));
    let (lines, code) = watch_quick(&mut m, Duration::from_millis(60));
    assert_eq!(
        lines,
        [
            "EVENT question seq=101 ⏺ Did the suite pass on your machine?".to_string(),
            format!("RECONNECT await seq 101 failed: aterm-ctl: {words}"),
            format!("EXIT reconnect window lapsed: text --json tail=40 failed: aterm-ctl: {words}"),
        ]
    );
    assert_eq!(code, 1);
    let (last, probes) = m.requests[7..].split_last().expect("requests");
    assert!(!probes.is_empty(), "{:?}", m.requests);
    assert!(
        probes.iter().all(|r| r == "text --json tail=40"),
        "only reads, never the failed wait again: {:?}",
        m.requests
    );
    assert_eq!(
        last, "meta unset attention owner=supervisor",
        "{:?}",
        m.requests
    );
}

#[test]
fn an_outage_that_keeps_coming_back_is_one_window() {
    let mut m = Mock::new(vec![question_screen()]);
    m.drop_awaits_from = Some(5);
    let (lines, code) = watch_quick(&mut m, Duration::from_millis(200));
    assert_eq!(lines.len(), 4, "{lines:?}");
    assert_eq!(
        lines[0],
        "EVENT question seq=101 ⏺ Did the suite pass on your machine?"
    );
    assert_eq!(
        lines[1],
        format!("RECONNECT await seq 101 failed: aterm-ctl: {CLOSED}")
    );
    reconnected_ms(&lines[2]);
    assert_eq!(
        lines[3],
        format!("EXIT reconnect window lapsed: await idle 500 failed: aterm-ctl: {CLOSED}")
    );
    assert_eq!(code, 1);
    let relapses = m
        .requests
        .iter()
        .filter(|r| r.starts_with("await idle"))
        .count();
    assert!(
        relapses >= 2,
        "it relapsed, and said nothing more: {:?}",
        m.requests
    );
}

#[test]
fn a_later_outage_is_a_new_one_with_its_own_window() {
    let mut m = Mock::new(vec![question_screen()]);
    m.by_index.insert(5, closed());
    m.by_index.insert(12, timeout());
    m.delay.insert(12, Duration::from_millis(400));
    m.by_index.insert(15, closed());
    m.vanish_after = Some(0);
    let (lines, code) = watch_quick(&mut m, Duration::from_millis(300));
    assert_eq!(lines.len(), 8, "{lines:?}");
    assert_eq!(
        lines[0],
        "EVENT question seq=101 ⏺ Did the suite pass on your machine?"
    );
    assert_eq!(
        lines[1],
        format!("RECONNECT await seq 101 failed: aterm-ctl: {CLOSED}")
    );
    reconnected_ms(&lines[2]);
    assert_eq!(
        lines[3],
        "EVENT question seq=103 ⏺ Did the suite pass on your machine?"
    );
    assert_eq!(
        lines[4],
        format!("RECONNECT await seq 103 failed: aterm-ctl: {CLOSED}")
    );
    reconnected_ms(&lines[5]);
    assert_eq!(
        lines[6..],
        [
            "EVENT question seq=106 ⏺ Did the suite pass on your machine?",
            "EXIT session gone (await seq 106 failed: aterm-ctl: ERR exited)",
        ]
    );
    assert_eq!(code, 1);
}

#[test]
fn no_such_session_is_final_outside_an_outage_and_not_yet_inside_one() {
    let mut m = Mock::new(vec![question_screen()]);
    m.by_index.insert(5, err("no such session"));
    let (lines, code) = watch_quick(&mut m, Duration::from_secs(5));
    assert_eq!(
        lines,
        [
            "EVENT question seq=101 ⏺ Did the suite pass on your machine?",
            "EXIT session gone (await seq 101 failed: aterm-ctl: ERR no such session)",
        ]
    );
    assert_eq!(code, 1);
    assert_eq!(m.requests.len(), 7, "no retry: {:?}", m.requests);

    let mut m = Mock::new(vec![question_screen()]);
    m.by_index.insert(5, closed());
    m.by_index.insert(6, err("no such session"));
    m.by_index.insert(7, err("no such session"));
    m.vanish_after = Some(0);
    let (lines, code) = watch_quick(&mut m, Duration::from_secs(5));
    assert_eq!(lines.len(), 5, "{lines:?}");
    assert_eq!(
        lines[1],
        format!("RECONNECT await seq 101 failed: aterm-ctl: {CLOSED}")
    );
    reconnected_ms(&lines[2]);
    assert_eq!(
        lines[3..],
        [
            "EVENT question seq=103 ⏺ Did the suite pass on your machine?",
            "EXIT session gone (await seq 103 failed: aterm-ctl: ERR exited)",
        ]
    );
    assert_eq!(code, 1);

    let mut m = Mock::new(vec![question_screen()]);
    m.by_index.insert(5, closed());
    m.down = Some((6, err("no such session")));
    let (lines, code) = watch_quick(&mut m, Duration::from_millis(60));
    assert_eq!(
        lines,
        [
            "EVENT question seq=101 ⏺ Did the suite pass on your machine?".to_string(),
            format!("RECONNECT await seq 101 failed: aterm-ctl: {CLOSED}"),
            "EXIT session gone (reconnect window lapsed: text --json tail=40 failed: \
             aterm-ctl: ERR no such session)"
                .to_string(),
        ]
    );
    assert_eq!(code, 1);

    let mut m = Mock::new(vec![question_screen()]);
    m.by_index.insert(5, closed());
    let (lines, code) = watch_quick(&mut m, Duration::ZERO);
    assert_eq!(
        lines,
        [
            "EVENT question seq=101 ⏺ Did the suite pass on your machine?".to_string(),
            format!("EXIT await seq 101 failed: aterm-ctl: {CLOSED}"),
        ]
    );
    assert_eq!(code, 1);
    assert_eq!(
        m.requests[7..],
        ["meta unset attention owner=supervisor"],
        "{:?}",
        m.requests
    );
}

#[test]
fn a_connection_turned_away_is_ridden_out() {
    let text = "auth";
    let mut m = Mock::new(vec![question_screen()]);
    m.by_index.insert(4, err(text));
    m.by_index.insert(5, err(text));
    m.vanish_after = Some(0);
    let (lines, code) = watch_quick(&mut m, Duration::from_secs(5));
    assert_eq!(lines.len(), 5, "{text}: {lines:?}");
    assert_eq!(
        lines[1],
        format!("RECONNECT await seq 101 failed: aterm-ctl: ERR {text}")
    );
    reconnected_ms(&lines[2]);
    assert_eq!(
        lines[3..],
        [
            "EVENT question seq=103 ⏺ Did the suite pass on your machine?",
            "EXIT session gone (await seq 103 failed: aterm-ctl: ERR exited)",
        ],
        "{text}"
    );
    assert_eq!(code, 1);
}

#[test]
fn a_budget_spent_in_an_outage_is_the_timeout() {
    let mut m = Mock::new(vec![question_screen()]);
    m.down = Some((5, closed()));
    let started = Instant::now();
    let (lines, code) = watch_quick_with(&mut m, Duration::from_secs(120), &auto_ms(150));
    assert!(
        started.elapsed() < Duration::from_secs(60),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(
        lines,
        [
            "EVENT question seq=101 ⏺ Did the suite pass on your machine?".to_string(),
            format!("RECONNECT await seq 101 failed: aterm-ctl: {CLOSED}"),
            "TIMEOUT".to_string(),
        ]
    );
    assert_eq!(code, EXIT_TIMEOUT);

    let mut m = Mock::new(vec![question_screen()]);
    m.by_index.insert(5, closed());
    m.delay.insert(5, Duration::from_millis(200));
    let (lines, code) = watch_quick_with(&mut m, Duration::from_secs(5), &auto_ms(100));
    assert_eq!(
        lines,
        [
            "EVENT question seq=101 ⏺ Did the suite pass on your machine?",
            "TIMEOUT",
        ]
    );
    assert_eq!(code, EXIT_TIMEOUT);
    let badge = m
        .requests
        .iter()
        .position(|request| request.starts_with("meta set attention "))
        .expect("the question's badge was raised");
    let after = &m.requests[badge + 1..];
    let lost = usize::from(
        after
            .first()
            .is_some_and(|r| r.starts_with("await seq 101 ")),
    );
    assert_eq!(
        after[lost..],
        ["meta unset attention owner=supervisor"],
        "nothing after: {:?}",
        m.requests
    );

    let mut m = Mock::new(vec![busy_screen()]);
    m.down = Some((2, closed()));
    let mut log: Vec<u8> = Vec::new();
    let (out, code) = quick(&mut m, Duration::from_secs(5))
        .supervise_to(&auto_ms(150), &mut log)
        .expect("supervise");
    assert_eq!(code, EXIT_TIMEOUT);
    assert!(out.starts_with("TIMEOUT\nbusy\n"), "{out}");
    assert!(
        String::from_utf8(log)
            .expect("utf-8")
            .starts_with("RECONNECT await gone esc.to.interrupt rows 2 7 failed: "),
    );

    let mut m = Mock::new(vec![busy_screen()]);
    m.down = Some((0, closed()));
    let mut log: Vec<u8> = Vec::new();
    let turn = quick(&mut m, Duration::from_secs(5))
        .await_turn_to(Duration::from_millis(100), &mut log)
        .expect("turn");
    assert!(turn.timed_out);
    assert_eq!(turn.phase, Phase::Busy);
}

#[test]
fn a_press_in_flight_is_decided_again_from_a_fresh_read() {
    let mut m = fenced(vec![bash_one_row(), write_prompt()]);
    m.by_index.insert(5, closed());
    m.vanish_after = Some(0);
    let (lines, code) = watch_quick_with(&mut m, Duration::from_secs(5), &auto(30));
    assert_eq!(lines.len(), 4, "{lines:?}");
    assert_eq!(
        lines[0],
        format!(
            "RECONNECT key if-human=7 if-gen=1.101 if=^\\x20{{3}}git\\x20log\\x20--oneline\\x20-5\\s*$ 1 failed: \
             aterm-ctl: {CLOSED}"
        )
    );
    reconnected_ms(&lines[1]);
    assert!(
        lines[2].starts_with("EVENT prompt seq=103 kind=bash classify=not-read-only:")
            && lines[2].ends_with(" command=rm -rf target"),
        "{lines:?}"
    );
    assert_eq!(
        lines[3],
        "EXIT session gone (await gone ^\\x20{3}rm\\x20-rf\\x20target\\s*$ failed: aterm-ctl: ERR exited)"
    );
    assert_eq!(code, 1);
    assert_eq!(
        m.presses(),
        ["key if-human=7 if-gen=1.101 if=^\\x20{3}git\\x20log\\x20--oneline\\x20-5\\s*$ 1"]
    );

    let mut m = fenced(vec![
        bash_one_row(),
        bash_one_row(),
        bash_one_row(),
        busy_screen(),
        idle_screen(),
    ]);
    m.by_index.insert(5, closed());
    m.vanish_after = Some(0);
    let (lines, _) = watch_quick(&mut m, Duration::from_secs(5));
    assert_eq!(
        decisions(&lines[2..]),
        [
            "APPROVED git log --oneline -5",
            "EVENT idle ⏺ Done.",
            "EXIT session gone (await seq 105 failed: aterm-ctl: ERR exited)",
        ]
    );
    assert_eq!(
        m.requests[..14],
        [
            "meta",
            "await gone esc.to.interrupt timeout 3000",
            "text --json tail=40",
            "status",
            "meta",
            "help key",
            "key if-human=7 if-gen=1.101 if=^\\x20{3}git\\x20log\\x20--oneline\\x20-5\\s*$ 1",
            "text --json tail=40",
            "await idle 500 timeout 1500",
            "text --json tail=40",
            "status",
            "meta",
            "help key",
            "key if-human=7 if-gen=1.103 if=^\\x20{3}git\\x20log\\x20--oneline\\x20-5\\s*$ 1",
        ],
        "{:?}",
        m.requests
    );
}

#[test]
fn a_busy_read_before_a_lost_connection_still_counts() {
    let mut m = Mock::new(vec![
        idle_screen(),
        idle_screen(),
        idle_screen(),
        idle_screen(),
        busy_screen(),
        idle_screen(),
        idle_screen(),
    ]);
    m.by_index.insert(2, closed());
    m.by_index.insert(6, closed());
    m.by_index.insert(10, closed());
    m.vanish_after = Some(0);
    let (lines, code) = watch_quick(&mut m, Duration::from_secs(5));
    assert_eq!(lines.len(), 6, "{lines:?}");
    assert_eq!(lines[0], "EVENT idle seq=101 ⏺ Done.");
    assert_eq!(
        lines[1],
        format!("RECONNECT await seq 101 failed: aterm-ctl: {CLOSED}")
    );
    reconnected_ms(&lines[2]);
    assert_eq!(
        lines[3..],
        [
            "EVENT idle seq=103 ⏺ Done.",
            "EVENT idle seq=107 ⏺ Done.",
            "EXIT session gone (await seq 107 failed: aterm-ctl: ERR exited)",
        ]
    );
    assert_eq!(code, 1, "{:?}", m.requests);
    assert_eq!(m.requests[10], "text --json tail=40", "{:?}", m.requests);
    assert_eq!(
        m.requests[11],
        "await gone esc.to.interrupt rows 2 7 timeout 20000"
    );
}

#[test]
fn await_turn_and_supervise_ride_it_out_with_the_lines_on_stderr() {
    let mut m = Mock::new(vec![busy_screen(), write_prompt()]);
    m.by_index.insert(2, closed());
    let mut log: Vec<u8> = Vec::new();
    let (out, code) = quick(&mut m, Duration::from_secs(5))
        .supervise_to(&auto(30), &mut log)
        .expect("supervise");
    assert_eq!(code, 0);
    assert!(
        out.starts_with("prompt\nkind bash\ncommand rm -rf target\n"),
        "{out}"
    );
    assert!(!out.contains("RECONNECT"), "{out}");
    let log = String::from_utf8(log).expect("utf-8");
    assert!(
        log.starts_with(&format!(
            "RECONNECT await gone esc.to.interrupt rows 2 7 failed: aterm-ctl: {CLOSED}\nRECONNECTED \
             after "
        )),
        "{log}"
    );
}

fn journal_file(tag: &str) -> (PathBuf, PathBuf) {
    let dir = crate::supervise::test_scratch_path("journal", tag);
    std::fs::create_dir_all(&dir).expect("tmp dir");
    let path = dir.join("journal.jsonl");
    let _ = std::fs::remove_file(&path);
    (dir, path)
}

fn journal_records(path: &Path) -> (Vec<super::super::journal::JournalRecord>, u32) {
    let (records, bad) = super::super::journal::read_journal(path).expect("read the journal");
    assert_eq!(bad, 0, "every line is a record");
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).expect("stat").permissions().mode() & 0o777
    };
    #[cfg(not(unix))]
    let mode = 0o600;
    (records, mode)
}

#[test]
fn watch_journals_every_line_it_prints() {
    let (dir, path) = journal_file("watch");
    std::fs::write(
        &path,
        "{\"t\":1,\"kind\":\"other\",\"line\":\"an older run\"}\n",
    )
    .expect("an older run's line");
    let was = journal_records(&path).1;
    let mut m = fenced(vec![
        busy_screen(),
        bash_one_row(),
        busy_screen(),
        question_screen(),
        busy_screen(),
        idle_screen(),
    ]);
    m.vanish_after = Some(3);
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        ..auto(30)
    };
    let mut out: Vec<u8> = Vec::new();
    let mut warn: Vec<u8> = Vec::new();
    let code = session(&mut m, Some("@s-1".to_string())).watch_to(&opts, &mut out, &mut warn);
    assert_eq!(code, 1);
    assert!(
        warn.is_empty(),
        "no warning: {}",
        String::from_utf8_lossy(&warn)
    );
    let printed: Vec<String> = String::from_utf8(out)
        .expect("utf-8")
        .lines()
        .map(str::to_string)
        .collect();
    let (records, mode) = journal_records(&path);
    assert_eq!(mode, was, "a file that was already there keeps its mode");
    assert_eq!(
        records[0].line, "an older run",
        "the file is appended to, never truncated"
    );
    let (quiet, mine): (
        Vec<super::super::journal::JournalRecord>,
        Vec<super::super::journal::JournalRecord>,
    ) = records[1..]
        .iter()
        .cloned()
        .partition(|r| matches!(r.kind.as_str(), "escalated" | "cleared" | "hold"));
    assert_eq!(
        quiet
            .iter()
            .map(|r| (r.kind.as_str(), r.seq))
            .collect::<Vec<_>>(),
        [
            ("escalated", Some(104)),
            ("cleared", Some(105)),
            ("hold", Some(106))
        ]
    );
    let mine = &mine[..];
    assert_eq!(
        mine.iter().map(|r| r.line.clone()).collect::<Vec<_>>(),
        printed,
        "one record per line printed, in order"
    );
    assert_eq!(
        mine.iter().map(|r| r.kind.as_str()).collect::<Vec<_>>(),
        ["approved", "event", "event", "exit"]
    );
    assert_eq!(
        mine.iter().map(|r| r.phase.as_str()).collect::<Vec<_>>(),
        ["prompt", "question", "idle", "-"]
    );
    assert_eq!(
        mine.iter().map(|r| r.seq).collect::<Vec<_>>(),
        [Some(102), Some(104), Some(106), None]
    );
    assert_eq!(mine[0].summary, "git log --oneline -5");
    assert_eq!(mine[2].summary, "⏺ Done.");
    assert!(
        mine[3].summary.starts_with("session gone ("),
        "{:?}",
        mine[3].summary
    );
    assert!(mine.iter().all(|r| r.sid.as_deref() == Some("s-1")));
    assert!(mine.iter().all(|r| r.t > 1_700_000_000_000));
    assert!(mine.windows(2).all(|w| w[0].t <= w[1].t));
    assert!(
        mine.iter()
            .all(|r| r.complete.is_none() && r.rows.is_none())
    );
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(dir);
}

#[test]
fn supervise_journals_what_watch_would_have_printed() {
    let (dir, path) = journal_file("supervise");
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        ..auto(30)
    };
    let mut m = Mock::new(vec![
        with_context(busy_screen(), 9),
        busy_screen(),
        idle_screen(),
    ]);
    let mut log: Vec<u8> = Vec::new();
    let (out, code) = session(&mut m, Some("@s-1".to_string()))
        .supervise_to(&opts, &mut log)
        .expect("supervise");
    assert_eq!(code, 0);
    assert!(out.starts_with("idle\n"), "{out}");
    let (records, mode) = journal_records(&path);
    assert_eq!(mode, 0o600);
    assert_eq!(
        records.iter().map(|r| r.line.clone()).collect::<Vec<_>>(),
        [
            "EVENT context seq=101 9% until auto-compact",
            "EVENT compacted seq=102",
            "EVENT idle seq=103 ⏺ Done.",
        ],
        "the two lines said on stderr, then the review point"
    );
    assert_eq!(
        records.iter().map(|r| r.phase.as_str()).collect::<Vec<_>>(),
        ["context", "compacted", "idle"]
    );
    let mut m = Mock::new(vec![busy_screen()]);
    let mut log: Vec<u8> = Vec::new();
    let (_, code) = session(&mut m, None)
        .supervise_to(
            &SuperviseOpts {
                max: Duration::ZERO,
                journal: Some(path.clone()),
                ..SuperviseOpts::default()
            },
            &mut log,
        )
        .expect("supervise");
    assert_eq!(code, EXIT_TIMEOUT);
    let (records, _) = journal_records(&path);
    assert_eq!(records.last().map(|r| r.kind.as_str()), Some("timeout"));
    assert_eq!(records.last().map(|r| r.line.as_str()), Some("TIMEOUT"));
    let mut m = Mock::new(vec![idle_screen()]);
    m.down = Some((0, err("exited")));
    let mut log: Vec<u8> = Vec::new();
    let err = session(&mut m, None)
        .supervise_to(
            &SuperviseOpts {
                journal: Some(path.clone()),
                ..auto(30)
            },
            &mut log,
        )
        .expect_err("the session is gone");
    let (records, _) = journal_records(&path);
    let last = records.last().expect("a record");
    assert_eq!(last.kind, "exit");
    assert_eq!(last.line, format!("EXIT {}", exit_reason(&err)));
    assert!(
        last.line.starts_with("EXIT session gone ("),
        "{}",
        last.line
    );
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(dir);
}

#[test]
fn a_journal_that_fails_is_said_once_and_stops_nothing() {
    let (dir, path) = journal_file("bad");
    std::fs::write(&path, "").expect("a file where a directory would have to be");
    let opts = SuperviseOpts {
        journal: Some(path.join("under-a-file.jsonl")),
        ..auto(30)
    };
    let mut m = Mock::new(vec![busy_screen(), question_screen()]);
    m.vanish_after = Some(1);
    let mut out: Vec<u8> = Vec::new();
    let mut warn: Vec<u8> = Vec::new();
    let code = session(&mut m, None).watch_to(&opts, &mut out, &mut warn);
    assert_eq!(code, 1);
    let lines: Vec<String> = String::from_utf8(out)
        .expect("utf-8")
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(
        decisions(&lines)[0],
        "EVENT question ⏺ Did the suite pass on your machine?"
    );
    assert!(
        lines.last().is_some_and(|l| l.starts_with("EXIT ")),
        "{lines:?}"
    );
    let warn = String::from_utf8(warn).expect("utf-8");
    assert_eq!(
        warn.lines().count(),
        1,
        "one warning for the whole run: {warn:?}"
    );
    assert!(warn.contains("journal"), "{warn}");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(dir);
}

const WEEKLY: &str =
    "  ⎿  You've hit your weekly limit · resets Sep 19 at 11am (America/Los_Angeles)";
const WEEKLY_TEXT: &str =
    "You've hit your weekly limit · resets Sep 19 at 11am (America/Los_Angeles)";
const WEEKLY_RESET: &str = "Sep 19 at 11am (America/Los_Angeles)";
#[test]
fn a_box_the_loop_does_not_approve_is_escalated_once_and_cleared_when_it_goes() {
    for (box_rows, label) in [
        (
            bash_multi_row_with_note(),
            "claude rm-breaker: cd /work/trust-vc && S=/tmp/scratch && grep -h edition <(git sh… (",
        ),
        (
            read_box(),
            "claude read: ~/.ssh/config (a Read outside every allowed root",
        ),
        (trust_box(), "claude other: "),
    ] {
        let (dir, path) = journal_file("box-escalation");
        let mut m = Mock::new(vec![
            busy_screen(),
            box_rows.clone(),
            box_rows,
            busy_screen(),
            idle_screen(),
        ]);
        m.vanish_after = Some(4);
        let opts = SuperviseOpts {
            journal: Some(path.clone()),
            ..auto(30)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_approval_env(ApprovalEnv {
                home: Some(PathBuf::from("/Users/_owner")),
                ..ApprovalEnv::of_process()
            });
        });
        assert!(
            lines[0].starts_with("EVENT prompt "),
            "the box is the first review point: {lines:?}"
        );
        let text = m
            .requests
            .iter()
            .find_map(|r| r.strip_prefix("meta set attention owner=supervisor "))
            .expect("an attention was set")
            .to_string();
        assert!(text.starts_with(label), "{text}");
        assert!(
            text.ends_with(')') && text.len() <= ATTENTION_BYTES,
            "{text}"
        );
        let acts: Vec<&String> = m
            .requests
            .iter()
            .filter(|r| r.starts_with("meta set") || r.starts_with("meta unset"))
            .collect();
        assert_eq!(
            acts,
            [
                &format!("meta set attention owner=supervisor {text}"),
                &"meta unset attention owner=supervisor".to_string(),
            ],
            "one set, one clear: {:?}",
            m.requests
        );
        let (records, _) = journal_records(&path);
        let kinds: Vec<&str> = records.iter().map(|r| r.kind.as_str()).collect();
        let escalated = kinds.iter().filter(|k| **k == "escalated").count();
        let cleared = kinds.iter().filter(|k| **k == "cleared").count();
        assert_eq!((escalated, cleared), (1, 1), "{kinds:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

fn box_acts(m: &Mock) -> Vec<String> {
    m.requests
        .iter()
        .filter(|r| r.starts_with("meta set attention"))
        .cloned()
        .collect()
}

#[test]
fn a_headerless_box_under_a_tall_transcript_is_named_by_its_own_first_row() {
    let transcript: Vec<String> = (0..40)
        .map(|i| format!("⏺ transcript row {i} done"))
        .collect();
    let mut screen = rows(&transcript.iter().map(String::as_str).collect::<Vec<_>>());
    screen.push(String::new());
    screen.extend(trust_box());
    let focus = screen
        .iter()
        .position(|r| r == " ❯ 1. Yes, proceed")
        .expect("the focused option");
    let mut m = Mock::new(vec![busy_screen(), screen, busy_screen(), idle_screen()]);
    m.cursor_row = Some(focus);
    m.vanish_after = Some(4);
    let (_, _) = watch_lines(&mut m, &auto(30));
    let text = "claude other: Do you trust the files in this folder? \
                (an other box: no rule approves this kind)";
    assert!(!text.contains("transcript row"));
    assert_eq!(
        box_acts(&m),
        [format!("meta set attention owner=supervisor {text}")],
        "{:?}",
        m.requests
    );
}
#[test]
fn an_answered_box_loses_its_badge_at_the_first_busy_read() {
    let mut m = Mock::new(vec![busy_screen(), trust_box(), trust_box(), busy_screen()]);
    m.vanish_after = Some(3);
    let (_, _) = watch_lines(&mut m, &auto(30));
    let set = m
        .requests
        .iter()
        .position(|r| r.starts_with("meta set attention owner=supervisor "))
        .expect("the box is escalated");
    let unsets: Vec<usize> = m
        .requests
        .iter()
        .enumerate()
        .filter(|(_, r)| *r == "meta unset attention owner=supervisor")
        .map(|(i, _)| i)
        .collect();
    assert_eq!(unsets.len(), 1, "{:#?}", m.requests);
    let unset = unsets[0];
    assert!(set < unset, "{:#?}", m.requests);
    let reads = m.requests[set..unset]
        .iter()
        .filter(|r| r.starts_with("text"))
        .count();
    assert!(
        reads >= 2,
        "the box re-read and the busy read: {:#?}",
        m.requests
    );
    assert!(
        m.requests[unset..].iter().any(|r| r.starts_with("await ")),
        "the unset was the watch's last act, not the busy read's: {:#?}",
        m.requests
    );
}

#[test]
fn the_same_box_after_the_worker_worked_is_a_new_ask() {
    let b = bash_multi_row_with_note();
    let mut m = Mock::new(vec![
        busy_screen(),
        b.clone(),
        b.clone(),
        busy_screen(),
        b.clone(),
        b,
        busy_screen(),
        idle_screen(),
    ]);
    m.vanish_after = Some(4);
    let (lines, _) = watch_lines(&mut m, &auto(30));
    let prompts = lines
        .iter()
        .filter(|l| l.starts_with("EVENT prompt "))
        .count();
    let sets = m
        .requests
        .iter()
        .filter(|r| r.starts_with("meta set attention"))
        .count();
    assert_eq!((prompts, sets), (2, 2), "{lines:?}\n{:?}", m.requests);
}

#[test]
fn the_approval_line_is_sanitized_and_cut_by_cells() {
    let mut b = bash_one_row();
    let header = b.iter().position(|r| r.trim() == "Bash command").unwrap();
    b[header] = format!(" Bash command · 100% \u{7}{}\u{2028}x", "全".repeat(200));
    let line = approval_line(&b);
    assert!(
        !line.chars().any(|c| c.is_control() || c == '\u{2028}'),
        "{line:?}"
    );
    let cells: usize = line.chars().map(aterm_grapheme::char_width).sum();
    assert!(cells <= APPROVAL_LINE_CELLS, "{cells} cells: {line:?}");
    assert!(
        cells >= APPROVAL_LINE_CELLS - 1,
        "cut at the cap, not short of it: {cells}"
    );
    assert!(line.starts_with("Bash command · 100% "), "{line:?}");
    assert_eq!(approval_line(&bash_one_row()), "Bash command");
}

fn weekly_limited() -> Vec<String> {
    let mut r = rows(&[
        "⏺ Running the A/B on m7 now.",
        WEEKLY,
        "",
        "✻ Worked for 3m 2s · done 4:51 PM",
        "",
    ]);
    r.extend(composer("  ⏵⏵ auto mode on (shift+tab to cycle)"));
    r
}
fn weekly_retried() -> Vec<String> {
    let mut r = rows(&[
        "⏺ Running the A/B on m7 now.",
        WEEKLY,
        "",
        "❯ go on",
        "",
        "⏺ Retrying.",
        WEEKLY,
        "",
        "✻ Worked for 2s · done 4:58 PM",
        "",
    ]);
    r.extend(composer("  ⏵⏵ auto mode on (shift+tab to cycle)"));
    r
}
fn model_limited() -> Vec<String> {
    let mut r = rows(&[
        "⏺ Running the suite.",
        "  ⎿  You've reached your Fable limit · resets in 3h",
        "",
        "✻ Worked for 12s · done 4:51 PM",
        "",
    ]);
    r.extend(composer("  ? for shortcuts"));
    r
}
fn switched() -> Vec<String> {
    let mut r = rows(&[
        "⏺ Running the A/B on m7 now.",
        WEEKLY,
        "",
        "❯ /login",
        "  ⎿  Login successful · account or organization changed",
        "",
    ]);
    r.extend(composer("  ? for shortcuts"));
    r
}
const ANSWER: &str = "⏺ Back at it: the A/B on m7 is running again.";
fn answered(typed: &str) -> Vec<String> {
    let mut r = rows(&[
        &format!("❯ {typed}"),
        "",
        ANSWER,
        "",
        "✻ Worked for 23s · done 11:01 AM",
        "",
    ]);
    r.extend(composer("  ? for shortcuts"));
    r
}
fn login_code_prompt() -> Vec<String> {
    rows(&[
        " Browser didn't open? Use the url below to sign in (⌘/ctrl + click):",
        " https://claude.ai/oauth/authorize?code=true&client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e",
        "",
        " Paste code here if prompted >",
    ])
}
fn weekly_drafted() -> Vec<String> {
    let mut r = weekly_limited();
    let caret = r.iter().position(|row| row == "❯").expect("caret row");
    r[caret] = "❯ let me look at this first".to_string();
    r
}
fn rules_file(tag: &str) -> (PathBuf, PathBuf) {
    let dir = crate::supervise::test_scratch_path("rules", tag);
    std::fs::create_dir_all(&dir).expect("tmp dir");
    let path = dir.join("rules.md");
    std::fs::write(&path, "Standing rules:\n  1. run nothing heavy while ~/ay-machine-busy exists\n  2. report counts\n")
        .expect("write");
    (dir, path)
}
const RULES_CONTINUATION: &str = "keep going (standing rules: Standing rules: 1. run nothing \
                                  heavy while ~/ay-machine-busy exists 2. report counts)";
fn continue_request(text: &str) -> String {
    format!(
        "turn submit=guarded:{} idle=600 timeout=2500 yield=0.2 {text}",
        super::turn_end_loop::composer_guard('❯', text)
    )
}
fn quick_turn_end(backoff: &[Duration]) -> TurnEndTiming {
    TurnEndTiming {
        reset_grace: Duration::ZERO,
        reset_jitter: Duration::ZERO,
        limit_backoff: backoff.to_vec(),
        ..TurnEndTiming::default()
    }
}
fn count(m: &Mock, prefix: &str) -> usize {
    m.requests.iter().filter(|r| r.starts_with(prefix)).count()
}

const PROBE_HELP_KEY: &str =
    "key [id=<key>] [if=<re>] [if-gen=<e.s>] [if-human=<n>] <name>: send a named key\n";
const PROBE_HELP_SEND: &str =
    "send [id=<key>] [if=<re>] [if-gen=<e.s>] [if-human=<n>] [--] <text>: write text\n";

#[derive(Debug, Default)]
struct ProbeHost {
    advice: Mutex<VecDeque<Option<AccountAdvice>>>,
    advised: Mutex<Option<AccountAdvice>>,
    claims: Mutex<VecDeque<bool>>,
    claimed: Mutex<bool>,
    answer: Mutex<Option<&'static str>>,
    wanting: AtomicBool,
    relaunches: AtomicBool,
    said: Mutex<Vec<String>>,
    samples: Mutex<Vec<(crate::harness::accounts::Sample, i64, bool)>>,
    shared: Arc<PanelShared>,
    tail: Mutex<Option<String>>,
    tail_asks: std::sync::atomic::AtomicUsize,
    asks_at_claims: Mutex<Vec<usize>>,
}

impl ProbeHost {
    fn scripted(advice: &[Option<AccountAdvice>], claims: &[bool]) -> Arc<Self> {
        Arc::new(Self {
            advice: Mutex::new(advice.iter().cloned().collect()),
            claims: Mutex::new(claims.iter().copied().collect()),
            ..Self::default()
        })
    }

    fn probing(claim: bool) -> Arc<Self> {
        Self::scripted(&[Some(AccountAdvice::Probe { otherwise: None })], &[claim])
    }

    fn said(&self, prefix: &str) -> Vec<String> {
        self.said
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.starts_with(prefix))
            .cloned()
            .collect()
    }
}

impl IdleHost for ProbeHost {
    fn wants(&self) -> bool {
        self.wanting.load(Ordering::SeqCst)
    }
    fn at_idle(&self) -> Option<HostStep> {
        self.said.lock().unwrap().push("at-idle".to_string());
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn can_restart(&self) -> bool {
        true
    }
    fn relaunches_after_stall(&self) -> bool {
        self.relaunches.load(Ordering::SeqCst)
    }
    fn restart(&self, why: &super::super::policy::turn_end::Restart) -> Option<String> {
        use super::super::policy::turn_end::Restart;
        let what = match why {
            Restart::Account { to, .. } => format!("restart:{}:{to}", why.word()),
            other => format!("restart:{}", other.word()),
        };
        self.said.lock().unwrap().push(what);
        self.answer.lock().unwrap().map(str::to_string)
    }
    fn turn_ran(&self) {
        self.said.lock().unwrap().push("turn-ran".to_string());
    }
    fn inform(&self, text: &str) {
        self.said.lock().unwrap().push(format!("inform:{text}"));
    }
    fn account(&self, point: AccountPoint) -> Option<AccountAdvice> {
        let at = match point {
            AccountPoint::Wall { kind } => format!("account:wall:{}", kind.name()),
            AccountPoint::LoginEnded => "account:login-ended".to_string(),
            AccountPoint::TurnEnd => "account:turn-end".to_string(),
        };
        self.said.lock().unwrap().push(at);
        let mut last = self.advised.lock().unwrap();
        if let Some(next) = self.advice.lock().unwrap().pop_front() {
            *last = next;
        }
        last.clone()
    }
    fn transcript_tail(&self) -> Option<String> {
        self.tail_asks.fetch_add(1, Ordering::SeqCst);
        self.tail.lock().unwrap().clone()
    }
    fn account_probe(&self) -> Result<(), ProbeNo> {
        self.asks_at_claims
            .lock()
            .unwrap()
            .push(self.tail_asks.load(Ordering::SeqCst));
        let mut last = self.claimed.lock().unwrap();
        if let Some(next) = self.claims.lock().unwrap().pop_front() {
            *last = next;
        }
        let word = if *last {
            self.shared.claims.fetch_add(1, Ordering::SeqCst);
            "probe:claimed"
        } else {
            "probe:declined"
        };
        self.said.lock().unwrap().push(word.to_string());
        if *last { Ok(()) } else { Err(ProbeNo::NotDue) }
    }
    fn account_probed(&self, read: Result<&crate::harness::accounts::Sample, ProbeMiss>) {
        self.shared.tolds.fetch_add(1, Ordering::SeqCst);
        let word = match read {
            Ok(sample) => {
                self.samples.lock().unwrap().push((
                    sample.clone(),
                    self.shared.pan.load(Ordering::SeqCst),
                    self.shared.hand.load(Ordering::SeqCst),
                ));
                "probed:ok".to_string()
            }
            Err(miss) => format!("probed:{}", miss.word()),
        };
        self.said.lock().unwrap().push(word);
    }
}

fn probe_mock(screens: Vec<Vec<String>>, frames: &[&str], host: &ProbeHost) -> Mock {
    let mut m = Mock::new(screens);
    m.gen_fence = true;
    m.sends_gen = true;
    m.help = PROBE_HELP_KEY.to_string();
    m.help_send = Some(PROBE_HELP_SEND.to_string());
    m.human_seq = Some(0);
    m.alt_screen = Some(true);
    m.vanish_after = Some(2);
    m.stall_sleep = Some(Duration::from_millis(5));
    let frames = frames
        .iter()
        .map(|text| aterm_phase::prompt::fixtures::screen(text))
        .collect();
    m.panel = Some(PanelWorld::new(frames, &host.shared));
    m
}

fn panel_under_a_turn() -> Vec<String> {
    let mut rows =
        aterm_phase::prompt::fixtures::screen(aterm_phase::prompt::fixtures::USAGE_SETTLED);
    rows[1] = "✻ Synthesizing… (18s · esc to interrupt)".to_string();
    rows
}

fn world(m: &mut Mock) -> &mut PanelWorld {
    m.panel.as_mut().expect("a panel world")
}

fn probe_writes(m: &Mock) -> Vec<&'static str> {
    m.panel
        .iter()
        .flat_map(|w| &w.probes)
        .map(|p| {
            if verb_of(&p.request) == "send" {
                "/usage"
            } else {
                "esc"
            }
        })
        .collect()
}

fn usage_probe_violations(m: &Mock, host: &ProbeHost) -> Vec<String> {
    let model = aterm_models_harness::supervisor_usage_probe_model();
    let forbidden = |action: &str, vars: &[(&'static str, i64)]| {
        let mut st = model.init_state();
        for (k, v) in vars {
            st.insert(k, *v);
        }
        (!model.action_enabled(action, &st)).then(|| format!("{action} at {vars:?}"))
    };
    let mut bad = Vec::new();
    for p in m.panel.iter().flat_map(|w| &w.probes) {
        let reading = p.claims > p.tolds;
        let (action, ph, own) = if verb_of(&p.request) == "send" {
            ("Type", if p.owed { 2 } else { i64::from(reading) }, 0)
        } else {
            ("EscCloses", if reading { 2 } else { 3 }, i64::from(reading))
        };
        let vars = [
            ("pan", p.pan),
            ("ph", ph),
            ("own", own),
            ("hand", i64::from(p.hand)),
            ("hold", i64::from(p.hold)),
            ("turn", i64::from(p.turn)),
        ];
        if let Some(why) = forbidden(action, &vars) {
            bad.push(format!("{why} ({})", p.request));
        }
    }
    for (_, pan, hand) in host.samples.lock().unwrap().iter() {
        let vars = [
            ("pan", *pan),
            ("ph", 2),
            ("own", 1),
            ("hand", i64::from(*hand)),
        ];
        if let Some(why) = forbidden("Sample", &vars) {
            bad.push(format!("{why} (the sample handed to the host)"));
        }
    }
    bad
}

const RESET_STOP: &str = "Stop and wait for limit to reset";
const RESET_GRANT: &str = "Use a weekly reset (2 left, until Oct 5)";
const RESET_WAIT: &str = "Wait here, then continue automatically at Oct 9 at 5am";
const RESET_CREDITS: &str = "Switch to usage credits";
const RESET_ROWS: [&str; 4] = [RESET_STOP, RESET_GRANT, RESET_WAIT, RESET_CREDITS];

#[derive(Debug, Clone)]
struct ResetHostStep {
    action: &'static str,
    dlg: i64,
    claim: i64,
    ph: i64,
    hand: bool,
    hold: bool,
}

#[derive(Debug)]
struct ResetHost {
    until: i64,
    long: bool,
    limits: Vec<String>,
    short_from: Mutex<Option<usize>>,
    no: Mutex<Option<ResetNo>>,
    again: Mutex<Vec<usize>>,
    lapses_at: Mutex<Option<usize>>,
    asks: std::sync::atomic::AtomicUsize,
    outcome: Mutex<ResetOutcome>,
    said: Mutex<Vec<String>>,
    steps: Mutex<Vec<ResetHostStep>>,
    shared: Arc<ResetShared>,
}

impl ResetHost {
    fn reading(outcome: ResetOutcome) -> Arc<Self> {
        Self::reading_at(outcome, &["weekly"])
    }

    fn reading_at(outcome: ResetOutcome, limits: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            until: TEST_NOW + 7 * 86_400,
            long: true,
            limits: limits.iter().map(|l| (*l).to_string()).collect(),
            short_from: Mutex::new(None),
            no: Mutex::new(None),
            again: Mutex::new(Vec::new()),
            lapses_at: Mutex::new(None),
            asks: std::sync::atomic::AtomicUsize::new(0),
            outcome: Mutex::new(outcome),
            said: Mutex::new(Vec::new()),
            steps: Mutex::new(Vec::new()),
            shared: Arc::new(ResetShared::default()),
        })
    }

    fn refusing(why: ResetNo) -> Arc<Self> {
        let host = Self::reading(ResetOutcome::Unread);
        *host.no.lock().unwrap() = Some(why);
        host
    }

    fn said(&self, prefix: &str) -> Vec<String> {
        self.said
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.starts_with(prefix))
            .cloned()
            .collect()
    }

    fn step(&self, action: &'static str) {
        let s = &self.shared;
        self.steps.lock().unwrap().push(ResetHostStep {
            action,
            dlg: s.dlg.load(Ordering::SeqCst),
            claim: s.claim.load(Ordering::SeqCst),
            ph: s.ph.load(Ordering::SeqCst),
            hand: s.hand.load(Ordering::SeqCst),
            hold: s.hold.load(Ordering::SeqCst),
        });
    }
}

impl IdleHost for ResetHost {
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn inform(&self, text: &str) {
        self.said.lock().unwrap().push(format!("inform:{text}"));
    }
    fn reset_take(&self, ask: &ResetAsk) -> ResetWord {
        let n = self.asks.fetch_add(1, Ordering::SeqCst) + 1;
        self.said.lock().unwrap().push(format!(
            "take kind={} left={} day={} day_start={:?}",
            ask.kind.word(),
            ask.left,
            ask.day,
            ask.day_start
        ));
        if self.again.lock().unwrap().contains(&n) {
            return ResetWord::Again;
        }
        if let Some(why) = *self.no.lock().unwrap() {
            return ResetWord::No(why);
        }
        let lapsed = self.lapses_at.lock().unwrap().is_some_and(|at| n >= at);
        match self.shared.claim.load(Ordering::SeqCst) {
            0 if !lapsed => {
                self.step("Claim");
                self.shared.claim.store(1, Ordering::SeqCst);
            }
            1 if !lapsed => {}
            _ => return ResetWord::No(ResetNo::Attempted),
        }
        let short = self.short_from.lock().unwrap().is_some_and(|at| n >= at);
        ResetWord::Take(ask.go(self.until, self.long && !short, self.limits.clone()))
    }
    fn reset_told(&self, end: &ResetEnd) -> ResetOutcome {
        self.said.lock().unwrap().push(format!(
            "told entered={} kept={} offer={} last={}",
            end.entered,
            end.kept
                .map_or_else(|| "-".to_string(), |why| why.to_string()),
            end.offer.map_or_else(
                || "None".to_string(),
                |o| format!("{} {} {:?}", o.kind.word(), o.left, o.use_by)
            ),
            end.last
        ));
        let taken = self.shared.ph.load(Ordering::SeqCst) > 0;
        self.step(if taken { "ReadLine" } else { "ClaimLapses" });
        self.shared.claim.store(2, Ordering::SeqCst);
        self.shared.ph.store(0, Ordering::SeqCst);
        self.shared.own.store(false, Ordering::SeqCst);
        *self.outcome.lock().unwrap()
    }
}

const RESET_HELP_KEY: &str =
    "key [id=<key>] [if=<re>] [if-gen=<e.s>] [if-human=<n>] <name>: send a named key\n";

fn reset_mock(screens: Vec<Vec<String>>, labels: &[&str], focus: usize, host: &ResetHost) -> Mock {
    let world = ResetWorld::new(labels, focus, &host.shared);
    let mut screens = screens;
    screens.push(world.dialog());
    let mut m = Mock::new(screens);
    m.gen_fence = true;
    m.sends_gen = true;
    m.help = RESET_HELP_KEY.to_string();
    m.human_seq = Some(0);
    m.vanish_after = Some(2);
    m.stall_sleep = Some(Duration::from_millis(2));
    m.reset = Some(world);
    m
}

fn reset_world(m: &mut Mock) -> &mut ResetWorld {
    m.reset.as_mut().expect("a reset world")
}

fn reset_keys(m: &Mock) -> Vec<String> {
    m.reset
        .iter()
        .flat_map(|w| &w.probes)
        .filter(|p| p.written)
        .map(|p| {
            let at = match p.at {
                ResetAt::Dialog => "dialog",
                ResetAt::Checking => "checking",
                ResetAt::Confirm => "confirm",
                ResetAt::Resetting => "resetting",
                ResetAt::Gone => "gone",
            };
            format!("{}@{at}", p.key)
        })
        .collect()
}

fn reset_waits_cut(cut: impl FnOnce(&mut reset_loop::ResetWaits)) -> reset_loop::ResetWaits {
    let mut waits = reset_loop::RESET_WAITS;
    cut(&mut waits);
    waits
}

fn reset_opts(host: &Arc<ResetHost>, on: bool, grace_s: u32) -> SuperviseOpts {
    SuperviseOpts {
        idle_host: Some(Arc::clone(host) as Arc<dyn IdleHost>),
        policy: SupervisorConfig {
            limit_reset: on,
            human_grace_s: grace_s,
            ..SupervisorConfig::default()
        },
        ..auto(60)
    }
}

struct ResetRun {
    lines: Vec<String>,
    journal: Vec<String>,
    ledger: Vec<String>,
}

impl ResetRun {
    fn of_rule(&self) -> Vec<&str> {
        self.journal
            .iter()
            .map(String::as_str)
            .filter(|l| l.contains("rule=limit-reset@v1"))
            .collect()
    }

    fn rows(&self) -> Vec<String> {
        self.ledger
            .iter()
            .filter_map(|row| {
                let v = aterm_json::from_str::<aterm_json::Value>(row).ok()?;
                let field = |key: &str| v.get(key).and_then(aterm_json::Value::as_str);
                (field("rule_id") == Some("limit-reset@v1")).then(|| {
                    format!(
                        "{}|{}|{}",
                        field("decision").unwrap_or("?"),
                        field("command").unwrap_or("?"),
                        field("reason").unwrap_or("?")
                    )
                })
            })
            .collect()
    }

    fn typed(&self) -> Vec<&str> {
        self.ledger
            .iter()
            .map(String::as_str)
            .filter(|row| row.contains(",\"decision\":\"typed\","))
            .collect()
    }
}

fn reset_watch(
    tag: &str,
    m: &mut Mock,
    mut opts: SuperviseOpts,
    seed: &dyn Fn(&Path),
    setup: impl FnOnce(&mut Watched<'_, '_>),
) -> ResetRun {
    let dir = crate::supervise::test_scratch_path("reset", tag);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("tmp dir");
    let (ledger, journal) = (dir.join("s-1.jsonl"), dir.join("journal.jsonl"));
    seed(&ledger);
    opts.journal = Some(journal.clone());
    let (lines, _) = watch_lines_with(m, &opts, |s| {
        s.set_approval_ledger(Some(ledger.clone()));
        s.set_survey_retry([Duration::from_millis(20); SURVEY_TRIES]);
        setup(s);
    });
    let journal = journal_records(&journal)
        .0
        .into_iter()
        .map(|r| r.line)
        .collect();
    let ledger = std::fs::read_to_string(&ledger)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    ResetRun {
        lines,
        journal,
        ledger,
    }
}

fn fresh(_: &Path) {}

fn reset_violations(m: &Mock, host: &ResetHost) -> Vec<String> {
    let model = aterm_models_harness::supervisor_limit_reset_model();
    let buggy = aterm_spec::interp::with_buggy(&model, 1);
    let judge = |action: &str, vars: &[(&'static str, i64)], what: &str| {
        let mut st = model.init_state();
        for (k, v) in vars {
            st.insert(k, *v);
        }
        if model.action_enabled(action, &st) {
            return None;
        }
        let named: Vec<String> = buggy
            .actions
            .iter()
            .map(|a| a.name)
            .filter(|name| {
                name.starts_with(action)
                    && *name != action
                    && !name.ends_with("Dropped")
                    && buggy.action_enabled(name, &st)
            })
            .map(|name| {
                let mut after = st.clone();
                buggy.fire(name, &mut after);
                let broken: Vec<&str> = buggy
                    .invariants
                    .iter()
                    .map(|inv| inv.name)
                    .filter(|inv| !buggy.check_invariant(inv, &after))
                    .collect();
                format!("{name} breaks {}", broken.join("+"))
            })
            .collect();
        Some(format!(
            "{action} at {vars:?} [{}] ({what})",
            named.join("; ")
        ))
    };
    let mut bad = Vec::new();
    for p in m.reset.iter().flat_map(|w| &w.probes).filter(|p| p.written) {
        let grant = p.on_grant;
        let action = match (p.at, p.key.as_str()) {
            (ResetAt::Dialog, "down" | "up") if p.claim == 1 && p.ph == 0 => "TakeRow",
            (ResetAt::Dialog, "enter") if p.claim == 1 && p.ph == 0 => {
                if !grant && p.dlg == 1 {
                    bad.push(format!(
                        "the reset's Enter on another row ({:?}) ({})",
                        p.row, p.request
                    ));
                    continue;
                }
                "TakeRow"
            }
            (ResetAt::Dialog, "down" | "up" | "enter") => {
                if p.key == "enter" && grant {
                    "TakeRow"
                } else {
                    "PressWait"
                }
            }
            (ResetAt::Confirm, "enter") => "Enter",
            (ResetAt::Confirm | ResetAt::Checking, "esc") => "Escape",
            _ => {
                bad.push(format!(
                    "a key the step never writes: {} at {:?} ({})",
                    p.key, p.at, p.request
                ));
                continue;
            }
        };
        let vars = [
            ("dlg", p.dlg),
            ("guard", i64::from(p.guard)),
            ("focus", i64::from(p.focus)),
            ("claim", p.claim),
            ("ph", p.ph),
            ("own", i64::from(p.own)),
            ("hand", i64::from(p.hand)),
            ("hold", i64::from(p.hold)),
        ];
        bad.extend(judge(action, &vars, &p.request));
    }
    for request in &m.requests {
        let words: Vec<&str> = request.split(' ').collect();
        let typed = matches!(verb_of(request), "send" | "turn" | "paste")
            && words
                .iter()
                .any(|w| w.starts_with("/limit-reset") || w.starts_with("/rate-limit-options"));
        if typed {
            bad.push(format!(
                "TypesTheCommand breaks NeverTheTypedCommand ({request})"
            ));
        }
    }
    for step in host.steps.lock().unwrap().iter() {
        let vars = [
            ("dlg", step.dlg),
            ("claim", step.claim),
            ("ph", step.ph),
            ("hand", i64::from(step.hand)),
            ("hold", i64::from(step.hold)),
        ];
        bad.extend(judge(step.action, &vars, "the host's own step"));
    }
    bad
}

#[test]
fn a_limit_escalates_once_and_extends_the_budget_once() {
    let (dir, path) = journal_file("limit-escalate");
    let mut m = Mock::new(vec![
        busy_screen(),
        weekly_limited(),
        busy_screen(),
        weekly_retried(),
    ]);
    m.vanish_after = Some(3);
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        ..auto(30)
    };
    let (lines, code) = watch_lines(&mut m, &opts);
    assert_eq!(
        lines,
        [
            format!("EVENT limited seq=102 message={WEEKLY_TEXT} reset={WEEKLY_RESET}"),
            format!("EXTEND until=2026-09-19T18:10:00Z reset={WEEKLY_RESET}"),
            format!("EVENT limited seq=104 message={WEEKLY_TEXT} reset={WEEKLY_RESET}"),
            "EXIT session gone (await seq 104 failed: aterm-ctl: ERR exited)".to_string(),
        ]
    );
    assert_eq!(code, 1);
    let text = format!("limited: {WEEKLY_TEXT}");
    assert_eq!(
        m.requests
            .iter()
            .filter(|r| r.starts_with("meta"))
            .collect::<Vec<_>>(),
        [
            &"meta".to_string(),
            &format!("meta set attention owner=supervisor kind=ask {text}"),
        ]
    );
    assert_eq!(count(&m, "turn"), 0, "no probe without resuming");
    let (records, _) = journal_records(&path);
    assert_eq!(
        records
            .iter()
            .map(|r| (r.kind.as_str(), r.phase.as_str()))
            .collect::<Vec<_>>(),
        [
            ("event", "limited"),
            ("escalated", "-"),
            ("extend", "-"),
            ("hold", "point"),
            ("event", "limited"),
            ("hold", "point"),
            ("exit", "-"),
        ]
    );
    assert_eq!(
        (records[1].seq, records[1].summary.as_str()),
        (Some(102), "attention=OK")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_refused_escalation_is_journaled_and_the_loop_goes_on() {
    let (dir, path) = journal_file("limit-refused");
    let mut credits = rows(&[
        "⏺ Running the suite.",
        "  ⎿  You've reached your Fable limit. Run /usage-credits to continue or switch \
         models with /model.",
        "",
        "✻ Worked for 12s · done 4:51 PM",
        "",
    ]);
    credits.extend(composer("  ? for shortcuts"));
    let mut m = Mock::new(vec![busy_screen(), credits]);
    m.vanish_after = Some(2);
    m.verb_replies.insert(
        "meta",
        VecDeque::from([
            ok(
                "OK title=- user_title=- description=- icon=- role=- attention=- cwd=- \
                state=alive\n",
            ),
            err("attention too long (max 256 bytes)"),
        ]),
    );
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        ..auto(30)
    };
    let (lines, _) = watch_lines(&mut m, &opts);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines[0].starts_with("EVENT limited seq=102 message=You've reached your Fable limit."));
    assert!(lines[1].starts_with("EXIT session gone"), "{lines:?}");
    let (records, _) = journal_records(&path);
    assert!(
        records[1].summary.starts_with("attention=ERR ") && !records[1].summary.contains("mail="),
        "{:?}",
        records[1].summary
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_refusal_naming_a_tighter_cap_is_sent_again_cut_to_it() {
    let (dir, path) = journal_file("limit-recut");
    let mut credits = rows(&[
        "⏺ Running the suite.",
        "  ⎿  You've reached your Fable limit. Run /usage-credits to continue or switch \
         models with /model.",
        "",
        "✻ Worked for 12s · done 4:51 PM",
        "",
    ]);
    credits.extend(composer("  ? for shortcuts"));
    let mut m = Mock::new(vec![busy_screen(), credits]);
    m.vanish_after = Some(2);
    m.verb_replies.insert(
        "meta",
        VecDeque::from([
            ok(
                "OK title=- user_title=- description=- icon=- role=- attention=- cwd=- \
                state=alive\n",
            ),
            err("attention too long (max 40 bytes)"),
        ]),
    );
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        ..auto(30)
    };
    let _ = watch_lines(&mut m, &opts);
    let sets: Vec<&String> = m
        .requests
        .iter()
        .filter(|r| r.starts_with("meta set attention owner=supervisor "))
        .collect();
    assert_eq!(sets.len(), 2, "{sets:#?}");
    let second = sets[1].trim_start_matches("meta set attention owner=supervisor kind=ask ");
    assert!(second.len() <= 40, "{second}");
    assert!(
        sets[0].ends_with(second) || sets[0].contains(second),
        "{sets:#?}"
    );
    let (records, _) = journal_records(&path);
    assert!(
        records[1].summary.starts_with("attention=OK"),
        "{:?}",
        records[1].summary
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn resume_never_continues_what_the_policy_limited() {
    for (resume_limits, turns) in [(false, 0), (true, 1)] {
        let mut m = Mock::new(vec![busy_screen(), limited_screen()]);
        m.vanish_after = Some(1);
        m.turn_releases = Some(1);
        let eight_pm = TEST_NOW + 11 * 3600 + 5 * 60;
        let mut opts = auto(2);
        opts.policy.resume_limits = resume_limits;
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_clock(eight_pm, PDT);
        });
        assert_eq!(
            count(&m, "turn"),
            turns,
            "resume_limits = {resume_limits}: {lines:#?}"
        );
        assert!(!lines.iter().any(|l| l.starts_with("EXTEND")), "{lines:#?}");
    }
}

#[test]
fn at_the_reset_one_continuation_goes_with_the_rules_and_the_answer_closes_the_episode() {
    let (jdir, journal) = journal_file("limit-resume");
    let (rdir, rules) = rules_file("resume");
    let mut m = Mock::new(vec![
        busy_screen(),
        weekly_limited(),
        busy_screen(),
        answered(RULES_CONTINUATION),
    ]);
    m.vanish_after = Some(3);
    m.turn_releases = Some(1);
    let opts = SuperviseOpts {
        journal: Some(journal.clone()),
        ..auto(30)
    };
    let (lines, code) = watch_lines_with(&mut m, &resuming(&opts, Some(rules.clone())), |s| {
        s.set_clock(AFTER_RESET, PDT);
        s.set_turn_end_timing(quick_turn_end(&[Duration::from_secs(600)]));
    });
    assert_eq!(
        lines,
        [
            format!("EVENT limited seq=102 message={WEEKLY_TEXT} reset={WEEKLY_RESET}"),
            format!("EXTEND until=2026-09-19T18:10:00Z reset={WEEKLY_RESET}"),
            format!("CONTINUED seq=102 rule=usage-resume@v1 {RULES_CONTINUATION}"),
            format!("EVENT idle seq=104 {ANSWER}"),
            "EXIT session gone (await seq 104 failed: aterm-ctl: ERR exited)".to_string(),
        ]
    );
    assert_eq!(code, 1);
    let at = |needle: &str| {
        m.requests
            .iter()
            .position(|r| r == needle)
            .unwrap_or_else(|| panic!("no `{needle}`: {:#?}", m.requests))
    };
    let turn = at(&continue_request(RULES_CONTINUATION));
    assert_eq!(
        m.requests[turn - 1],
        "status",
        "the program read fresh first"
    );
    assert_eq!(
        count(&m, "meta set attention"),
        0,
        "handled at once: {:#?}",
        m.requests
    );
    assert_eq!(count(&m, "turn"), 1, "{:#?}", m.requests);
    let (records, _) = journal_records(&journal);
    assert_eq!(
        records
            .iter()
            .map(|r| (r.kind.as_str(), r.phase.as_str(), r.seq))
            .collect::<Vec<_>>(),
        [
            ("event", "limited", Some(102)),
            ("limited", "-", Some(102)),
            ("extend", "-", None),
            ("continued", "usage-resume@v1", Some(102)),
            ("cleared", "-", Some(104)),
            ("event", "idle", Some(104)),
            ("hold", "point", Some(104)),
            ("exit", "-", None),
        ]
    );
    let _ = std::fs::remove_dir_all(&jdir);
    let _ = std::fs::remove_dir_all(&rdir);
}

#[test]
fn the_screen_leaving_the_notice_is_continued_at_once() {
    let mut m = Mock::new(vec![
        busy_screen(),
        weekly_limited(),
        switched(),
        busy_screen(),
        answered("keep going"),
    ]);
    m.vanish_after = Some(2);
    m.turn_releases = Some(2);
    let (lines, _) = watch_lines_with(&mut m, &resuming(&auto(3 * 86_400), None), |_| {});
    assert_eq!(lines.len(), 5, "{lines:#?}");
    assert_eq!(
        lines[0],
        format!("EVENT limited seq=102 message={WEEKLY_TEXT} reset={WEEKLY_RESET}")
    );
    assert!(lines[1].starts_with("EVENT idle seq=103 "), "{lines:#?}");
    assert_eq!(
        lines[2],
        "CONTINUED seq=103 rule=usage-resume@v1 keep going"
    );
    assert_eq!(lines[3], format!("EVENT idle seq=105 {ANSWER}"));
    assert!(lines[4].starts_with("EXIT session gone"), "{lines:#?}");
    assert_eq!(count(&m, "turn"), 1);
    assert!(m.requests.contains(&continue_request("keep going")));
    assert_eq!(count(&m, "meta set attention"), 0, "{:?}", m.requests);
    assert!(!lines.iter().any(|l| l.starts_with("EXTEND")), "{lines:?}");
}

#[test]
fn a_standing_approval_attention_survives_the_episode_close() {
    let (dir, path) = journal_file("limit-foreign-attention");
    let approval = "claude needs approval: Bash rm -rf build";
    let mut m = Mock::new(vec![
        busy_screen(),
        weekly_limited(),
        switched(),
        busy_screen(),
        answered("keep going"),
    ]);
    m.vanish_after = Some(2);
    m.foreign_attention = Some(approval.to_string());
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        ..auto(3 * 86_400)
    };
    let (lines, _) = watch_lines_with(&mut m, &opts, |_| {});
    assert!(
        !lines.iter().any(|l| l.starts_with("CONTINUED")),
        "nothing typed: {lines:?}"
    );
    assert_eq!(count(&m, "meta set attention owner=supervisor"), 1);
    assert_eq!(count(&m, "meta set attention"), 1, "never the bare form");
    assert_eq!(
        m.requests.iter().filter(|r| *r == "meta").count(),
        1,
        "{:?}",
        m.requests
    );
    assert_eq!(
        count(&m, "meta unset attention owner=supervisor"),
        1,
        "{:?}",
        m.requests
    );
    assert_eq!(count(&m, "meta unset attention"), 1, "never the bare form");
    assert_eq!(m.human_attention.as_deref(), Some(approval));
    assert_eq!(m.attention, None);
    let (records, _) = journal_records(&path);
    let cleared = records
        .iter()
        .find(|r| r.kind == "cleared")
        .expect("a CLEARED row");
    assert!(
        cleared.summary.starts_with("attention=OK "),
        "{}",
        cleared.summary
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_notice_again_after_a_continuation_is_the_same_episode_and_backs_off() {
    let (dir, path) = journal_file("limit-again");
    let mut m = Mock::new(vec![
        busy_screen(),
        weekly_limited(),
        busy_screen(),
        weekly_retried(),
    ]);
    m.vanish_after = Some(3);
    m.turn_releases = Some(1);
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        ..auto(30)
    };
    let (lines, _) = watch_lines_with(&mut m, &resuming(&opts, None), |s| {
        s.set_clock(AFTER_RESET, PDT);
        s.set_turn_end_timing(quick_turn_end(&[Duration::from_secs(60)]));
    });
    assert_eq!(
        lines,
        [
            format!("EVENT limited seq=102 message={WEEKLY_TEXT} reset={WEEKLY_RESET}"),
            format!("EXTEND until=2026-09-19T18:10:00Z reset={WEEKLY_RESET}"),
            "CONTINUED seq=102 rule=usage-resume@v1 keep going".to_string(),
            format!("EVENT limited seq=104 message={WEEKLY_TEXT} reset={WEEKLY_RESET}"),
            "EXIT session gone (await seq 104 failed: aterm-ctl: ERR exited)".to_string(),
        ]
    );
    assert_eq!(count(&m, "meta set attention"), 0, "{:?}", m.requests);
    assert_eq!(count(&m, "turn"), 1, "{:?}", m.requests);
    let (records, _) = journal_records(&path);
    let waiting: Vec<(Option<u64>, &str)> = records
        .iter()
        .filter(|r| r.kind == "waiting")
        .map(|r| (r.seq, r.summary.as_str()))
        .collect();
    assert_eq!(
        waiting,
        [(Some(104), "until=2026-09-19T18:01:30Z usage-weekly resets")],
        "{records:#?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_span_is_read_once_and_the_continuations_back_off_ten_then_thirty() {
    let (dir, path) = journal_file("limit-span-once");
    let mut limited = rows(&[
        "⏺ Running the suite.",
        "",
        "✻ Worked for 12s · done 4:51 PM",
        "",
    ]);
    limited.extend(composer(&aterm_phase::prompt::fixtures::right_aligned(
        "  ⏵⏵ auto mode on",
        "You've reached your Fable limit · resets in 1s",
    )));
    let mut m = Mock::new(vec![
        busy_screen(),
        limited.clone(),
        busy_screen(),
        limited.clone(),
        busy_screen(),
        limited.clone(),
        busy_screen(),
        limited,
    ]);
    m.turn_gates = vec![1, 3, 5, 7];
    m.dead_after_turns = Some(3);
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        ..auto(30)
    };
    let mut out: Vec<u8> = Vec::new();
    let mut s = session(&mut m, None);
    s.set_zone_lookup(test_zones);
    s.set_turn_end_timing(quick_turn_end(&[
        Duration::from_millis(10),
        Duration::from_millis(30),
    ]));
    let _ = s.watch(&resuming(&opts, None), &mut out);
    let lines: Vec<String> = String::from_utf8(out)
        .expect("utf-8")
        .lines()
        .map(str::to_string)
        .collect();
    let (records, _) = journal_records(&path);
    let _ = std::fs::remove_dir_all(&dir);
    let limited = "EVENT limited message=You've reached your Fable limit · resets in 1s \
                   reset=in 1s";
    let got = decisions(&lines);
    assert_eq!(got[0], limited, "{lines:#?}");
    assert!(got[1].starts_with("EXTEND until="), "{lines:#?}");
    let extends = got.iter().filter(|l| l.starts_with("EXTEND")).count();
    assert_eq!(extends, 1, "the span read once: {lines:#?}");
    let sent: Vec<i64> = records
        .iter()
        .filter(|r| r.kind == "continued")
        .map(|r| r.t)
        .collect();
    assert_eq!(sent.len(), 3, "{lines:#?}");
    assert!(sent[1] - sent[0] >= 10, "{sent:?}");
    assert!(sent[2] - sent[1] >= 30, "{sent:?}");
    assert!(
        got.last()
            .is_some_and(|l| l.starts_with("EXIT session gone")),
        "{lines:#?}"
    );
}

#[test]
fn a_draft_in_the_composer_holds_the_continuation() {
    let mut m = Mock::new(vec![busy_screen(), weekly_drafted()]);
    m.vanish_after = Some(3);
    m.cursor_col = Some(29);
    let (lines, _) = watch_lines_with(&mut m, &resuming(&auto(30), None), |s| {
        s.set_clock(AFTER_RESET, PDT);
        s.set_turn_end_timing(quick_turn_end(&[Duration::from_millis(10)]));
    });
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(lines[0].starts_with("EVENT limited seq=102"), "{lines:?}");
    assert!(
        lines[1].starts_with("EXTEND until=2026-09-19T18:10:00Z"),
        "{lines:?}"
    );
    assert!(lines[2].starts_with("EXIT session gone"), "{lines:?}");
    assert_eq!(count(&m, "turn"), 0, "{:?}", m.requests);
    let mut m = Mock::new(vec![busy_screen(), weekly_limited()]);
    m.vanish_after = Some(3);
    let _ = watch_lines_with(&mut m, &resuming(&auto(30), None), |s| {
        s.set_clock(AFTER_RESET, PDT);
        s.set_turn_end_timing(quick_turn_end(&[Duration::from_millis(10)]));
    });
    assert_eq!(count(&m, "turn"), 1, "{:?}", m.requests);
}

#[test]
fn a_worked_point_closes_the_episode_and_types_nothing() {
    let (rdir, rules) = rules_file("worked");
    let mut m = Mock::new(vec![
        busy_screen(),
        model_limited(),
        busy_screen(),
        idle_screen(),
    ]);
    m.vanish_after = Some(2);
    let (lines, _) = watch_lines_with(&mut m, &resuming(&auto(30), Some(rules.clone())), |_| {});
    assert_eq!(
        lines,
        [
            "EVENT limited seq=102 message=You've reached your Fable limit · resets in 3h \
             reset=in 3h",
            "EXTEND until=2026-09-17T19:05:00Z reset=in 3h",
            "EVENT idle seq=104 ⏺ Done.",
            "EXIT session gone (await seq 104 failed: aterm-ctl: ERR exited)",
        ]
    );
    assert_eq!(count(&m, "turn"), 0, "{:?}", m.requests);
    assert_eq!(count(&m, "meta set attention"), 0, "waited out: no badge");
    let _ = std::fs::remove_dir_all(&rdir);
}

#[test]
fn a_screen_without_a_composer_gets_no_continuation() {
    let mut m = Mock::new(vec![busy_screen(), weekly_limited(), login_code_prompt()]);
    m.vanish_after = Some(2);
    let (lines, _) = watch_lines_with(&mut m, &resuming(&auto(3 * 86_400), None), |_| {});
    assert_eq!(lines.len(), 3, "{lines:#?}");
    assert!(lines[0].starts_with("EVENT limited seq=102"), "{lines:#?}");
    assert!(
        lines[1].starts_with("EVENT idle seq=104"),
        "{lines:#?} {:#?}",
        m.requests
    );
    assert!(lines[2].starts_with("EXIT session gone"), "{lines:#?}");
    assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
}

#[test]
fn a_reset_further_off_than_a_week_extends_nothing() {
    let mut misread = rows(&[
        "⏺ Running the suite.",
        "  ⎿  You've hit your weekly limit · resets Feb 1 at 11am (America/Los_Angeles)",
        "",
        "✻ Worked for 12s · done 4:51 PM",
        "",
    ]);
    misread.extend(composer("  ? for shortcuts"));
    let mut m = Mock::new(vec![busy_screen(), misread]);
    m.vanish_after = Some(1);
    let (lines, _) = watch_lines(&mut m, &auto(30));
    assert_eq!(
        lines,
        [
            "EVENT limited seq=102 message=You've hit your weekly limit · resets Feb 1 at \
             11am (America/Los_Angeles) reset=Feb 1 at 11am (America/Los_Angeles)",
            "EXIT session gone (await seq 102 failed: aterm-ctl: ERR exited)",
        ]
    );
}

const AUTO_TEXT: &str =
    "⚠ Usage limit reached · continuing automatically at 1:50pm · esc to cancel";
const SHORTLY_TEXT: &str = "⚠ Usage limit reached · continuing shortly · esc to cancel";
const AUTO_SPINNER: &str = "✶ Deliberating… (4s · ↑ 1.2k tokens · esc to interrupt)";
fn auto_transcript() -> Vec<String> {
    rows(&["⏺ Bash(./run.sh --queue 8)", "  ⎿  queued 8 runs", ""])
}
fn auto_waiting(notice: &str) -> Vec<String> {
    let mut r = auto_transcript();
    r.push(notice.to_string());
    r.extend(composer("  ⏵⏵ auto mode on (shift+tab to cycle)"));
    r
}
fn auto_resumed() -> Vec<String> {
    let mut r = auto_transcript();
    r.extend(rows(&[AUTO_TEXT, AUTO_SPINNER, ""]));
    r.extend(composer(
        "  ⏵⏵ auto mode on (shift+tab to cycle) · esc to interrupt · 2 shells",
    ));
    r
}
fn auto_done() -> Vec<String> {
    let mut r = auto_transcript();
    r.extend(rows(&[
        AUTO_TEXT,
        "⏺ All 8 runs are in.",
        "",
        "✻ Worked for 3m 2s · done 2:10 PM",
        "",
    ]));
    r.extend(composer("  ? for shortcuts"));
    r
}

#[test]
fn an_auto_continue_notice_closes_at_the_first_busy_read_and_nothing_is_typed() {
    let (rdir, rules) = rules_file("auto-continue");
    let (jdir, path) = journal_file("auto-continue");
    let mut m = Mock::new(vec![
        busy_screen(),
        auto_waiting(AUTO_TEXT),
        auto_resumed(),
        auto_done(),
    ]);
    m.vanish_after = Some(2);
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        ..auto(30)
    };
    let (lines, _) = watch_lines_with(&mut m, &resuming(&opts, Some(rules.clone())), |_| {});
    assert_eq!(
        lines,
        [
            format!("EVENT limited seq=102 message={AUTO_TEXT} reset=1:50pm"),
            "EXTEND until=2026-09-17T21:00:00Z reset=1:50pm".to_string(),
            "EVENT idle seq=104 ⏺ All 8 runs are in.".to_string(),
            "EXIT session gone (await seq 104 failed: aterm-ctl: ERR exited)".to_string(),
        ],
        "{:#?}",
        m.requests
    );
    assert_eq!(count(&m, "meta set attention"), 0, "{:?}", m.requests);
    assert_eq!(count(&m, "turn"), 0, "{:?}", m.requests);
    let (records, _) = journal_records(&path);
    let cleared: Vec<Option<u64>> = records
        .iter()
        .filter(|r| r.kind == "cleared")
        .map(|r| r.seq)
        .collect();
    assert_eq!(
        cleared,
        [Some(103)],
        "closed at the busy read, before the idle point: {records:#?}"
    );
    assert!(
        !records.iter().any(|r| r.kind == "escalated"),
        "nothing raised: {records:#?}"
    );
    let _ = std::fs::remove_dir_all(&rdir);
    let _ = std::fs::remove_dir_all(&jdir);
}

#[test]
fn a_continuing_shortly_notice_is_a_reset_a_minute_off() {
    let mut m = Mock::new(vec![busy_screen(), auto_waiting(SHORTLY_TEXT)]);
    m.vanish_after = Some(2);
    let (lines, _) = watch_lines_with(&mut m, &resuming(&auto(30), None), |_| {});
    assert_eq!(
        lines,
        [
            format!("EVENT limited seq=102 message={SHORTLY_TEXT} reset=shortly"),
            "EXTEND until=2026-09-17T16:06:00Z reset=shortly".to_string(),
            "EXIT session gone (await seq 102 failed: aterm-ctl: ERR exited)".to_string(),
        ],
        "{:#?}",
        m.requests
    );
    assert_eq!(count(&m, "turn"), 0, "the vendor goes on by itself");
    assert_eq!(count(&m, "meta set attention"), 0, "{:?}", m.requests);
}

#[test]
fn an_auto_continue_notice_is_never_continued_even_past_its_time() {
    let read_at = TEST_NOW + (4 * 60 + 55) * 60 + 90;
    let mut m = Mock::new(vec![
        busy_screen(),
        auto_waiting(AUTO_TEXT),
        auto_resumed(),
        auto_done(),
    ]);
    m.vanish_after = Some(2);
    let (lines, _) = watch_lines_with(&mut m, &resuming(&auto(30), None), |s| {
        s.set_clock(read_at, PDT);
        s.set_turn_end_timing(quick_turn_end(&[Duration::from_secs(600)]));
    });
    assert_eq!(
        lines,
        [
            format!("EVENT limited seq=102 message={AUTO_TEXT} reset=1:50pm"),
            "EXTEND until=2026-09-17T21:00:00Z reset=1:50pm".to_string(),
            "EVENT idle seq=104 ⏺ All 8 runs are in.".to_string(),
            "EXIT session gone (await seq 104 failed: aterm-ctl: ERR exited)".to_string(),
        ],
        "{:#?}",
        m.requests
    );
    assert_eq!(count(&m, "turn"), 0, "{:?}", m.requests);
    assert_eq!(count(&m, "meta set attention"), 0, "{:?}", m.requests);
    let mut m = Mock::new(vec![busy_screen(), weekly_limited()]);
    m.vanish_after = Some(2);
    let _ = watch_lines_with(&mut m, &resuming(&auto(30), None), |s| {
        s.set_clock(AFTER_RESET, PDT);
        s.set_turn_end_timing(quick_turn_end(&[Duration::from_secs(600)]));
    });
    assert_eq!(count(&m, "turn"), 1, "{:?}", m.requests);
}

#[test]
fn a_retry_that_hits_the_wall_again_is_the_same_episode() {
    let (dir, path) = journal_file("auto-retry");
    let mut m = Mock::new(vec![
        busy_screen(),
        auto_waiting(AUTO_TEXT),
        auto_resumed(),
        auto_waiting(SHORTLY_TEXT),
        auto_resumed(),
        auto_waiting(SHORTLY_TEXT),
    ]);
    m.vanish_after = Some(2);
    let opts = SuperviseOpts {
        journal: Some(path.clone()),
        ..auto(30)
    };
    let (lines, _) = watch_lines(&mut m, &resuming(&opts, None));
    assert_eq!(
        lines,
        [
            format!("EVENT limited seq=102 message={AUTO_TEXT} reset=1:50pm"),
            "EXTEND until=2026-09-17T21:00:00Z reset=1:50pm".to_string(),
            format!("EVENT limited seq=104 message={SHORTLY_TEXT} reset=shortly"),
            format!("EVENT limited seq=106 message={SHORTLY_TEXT} reset=shortly"),
            "EXIT session gone (await seq 106 failed: aterm-ctl: ERR exited)".to_string(),
        ],
        "{:#?}",
        m.requests
    );
    assert_eq!(
        count(&m, "meta set attention"),
        0,
        "nothing raised: {:#?}",
        m.requests
    );
    assert_eq!(count(&m, "turn"), 0, "no probe: {:?}", m.requests);
    let (records, _) = journal_records(&path);
    assert!(
        !records.iter().any(|r| r.kind == "escalated"),
        "{records:#?}"
    );
    assert_eq!(
        records.iter().filter(|r| r.kind == "cleared").count(),
        2,
        "each busy read closes its episode: {records:#?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_folder_a_spawn_trusted_is_read_off_meta_decoded() {
    let mut m = Mock::new(vec![idle_screen()]);
    let mut s = Session::new(&mut m, Some("@s-w".to_string()));
    let meta = |trusted: &str| {
        format!("OK title=- cwd=/w agent_cwd=/w/a state=alive trusted_cwd={trusted}\n")
    };
    s.note_cwd(&meta("/w/a%20b"));
    assert_eq!(s.trusted_cwd, Some(PathBuf::from("/w/a b")), "decoded");
    assert_eq!(s.cwd, Some(PathBuf::from("/w/a")), "agent_cwd= is the cwd");
    for none in [
        meta("-"),
        meta("w/a"),
        "OK title=- cwd=/w agent_cwd=/w/a state=alive\n".to_string(),
    ] {
        s.note_cwd(&meta("/w/a%20b"));
        s.note_cwd(&none);
        assert_eq!(s.trusted_cwd, None, "{none}");
    }
}

#[test]
fn the_agent_a_spawn_started_is_read_off_meta() {
    let mut m = Mock::new(vec![idle_screen()]);
    let mut s = Session::new(&mut m, Some("@s-w".to_string()));
    let meta = |worker: &str| {
        format!("OK title=- cwd=/w agent_cwd=/w state=alive trusted_cwd=/w worker={worker}\n")
    };
    for program in [
        aterm_phase::Program::Claude,
        aterm_phase::Program::Gemini,
        aterm_phase::Program::Agy,
        aterm_phase::Program::Muse,
    ] {
        s.note_cwd(&meta(program.name()));
        assert_eq!(s.worker_agent, Some(program));
    }
    for none in [
        meta("-"),
        meta("generic"),
        meta("vim"),
        "OK title=- cwd=/w agent_cwd=/w state=alive trusted_cwd=/w\n".to_string(),
    ] {
        s.note_cwd(&meta("gemini"));
        s.note_cwd(&none);
        assert_eq!(s.worker_agent, None, "{none}");
    }
}

mod engine {
    use super::*;
    include!("run_engine_tests.rs");
    mod text {
        use super::*;
        include!("run_question_text_tests.rs");
    }
    mod box_key {
        use super::*;
        include!("run_box_key_tests.rs");
    }
    mod box_hold {
        use super::*;
        include!("run_box_hold_tests.rs");
    }
}

mod box_head_loop {
    use super::*;
    include!("box_head_tests.rs");
}

#[path = "turn_end_loop_tests.rs"]
mod turn_end;

mod needs_loop {
    use super::*;
    include!("needs_loop_tests.rs");
}

mod autopilot_loop {
    use super::*;
    include!("autopilot_loop_tests.rs");
}

mod task_face_loop {
    use super::*;
    include!("task_face_loop_tests.rs");
}

mod hold_loop {
    use super::*;
    include!("hold_loop_tests.rs");
}

mod wall_confirm_loop {
    use super::*;
    include!("wall_confirm_loop_tests.rs");
}

mod reset {
    use super::*;
    include!("reset_loop_tests.rs");
}

mod stall_hold {
    use super::*;
    include!("stall_tests.rs");
}

mod tape_loop {
    use super::*;
    include!("tape_loop_tests.rs");
}

mod replay_loop {
    use super::*;
    include!("replay_loop_tests.rs");
}

mod why_loop {
    use super::*;
    include!("why_loop_tests.rs");
}

mod counterfactual_loop {
    use super::*;
    include!("counterfactual_loop_tests.rs");
}

mod corpus_loop {
    use super::*;
    include!("corpus_loop_tests.rs");
}
