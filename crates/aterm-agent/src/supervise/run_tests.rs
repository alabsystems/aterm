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

/// A loop on `ctl` with no box settle, no card settle and no landing time for a
/// `0`: a scripted server has no key guard and no debounce ([`BOX_SETTLE`],
/// [`SURVEY_SETTLE`] and [`SURVEY_LAND`] are the vendor's; each one's own test sets
/// it); no pause between a box's transcript reads (a scripted host's
/// transcript lags by its script, not by the clock); and the loop's own
/// waits at the test values ([`crate::harness::Timings::FAST`]).
fn session<C: Ctl>(ctl: &mut C, sid: Option<String>) -> Session<'_, C> {
    let mut s = Session::new(ctl, sid);
    s.set_timings(crate::harness::Timings::FAST);
    s.set_box_settle(Duration::ZERO);
    s.set_box_call_step(Duration::ZERO);
    s.set_survey_settle(Duration::ZERO);
    s.set_survey_land(Duration::ZERO);
    s
}

/// A scripted server: each `call` pops the next reply and records the
/// request; screens are served in order from `screens`.
///
/// Built-in answers: the content sequence advances on every read; `await
/// seq <n>` latches only while another screen is still to come and times
/// out on the last one; `key if=` is judged against the last served
/// screen with the server's own `aterm_observe::row_matcher` (`OK
/// seq=<n>` on a match, else `OK skipped seq=<n>`, `ERR badregex` for a
/// bad pattern).
///
/// `await idle` answers at once by default. With `idle_skips_busy`, it
/// passes over the busy screens next in the script when another screen
/// follows them: a busy spell the loop never sees, as a spinner never
/// holds still for the real server's `await idle 2000`.
///
/// A handoff: `by_index` answers chosen requests (by arrival order) ahead
/// of every other rule — [`closed`], the connection the old instance
/// dropped, or a server's `ERR` — and from `down`'s index on every request
/// gets `down`'s reply (the session never came back, or never serves
/// again). From `drop_awaits_from` on every `await` is [`closed`] while
/// reads are served: an outage that keeps coming back. The first lost
/// reply served starts the content seq over at `handoff_seq`, as the
/// instance that adopts the session counts from its own start; `restart`
/// starts it over at a request with none failing — a handoff no request
/// saw fail. `await seq <n>` for an `n` the content has not reached — a
/// number from before the handoff — never latches, as on the real server
/// (it passes the stalls the way the last screen does). `delay` holds a
/// request's answer back.
/// Where the digit a rating card is waiting on stands ([`Mock::card_debounce`]).
#[derive(Debug, Clone, Copy, PartialEq)]
enum Digit {
    /// The composer is empty.
    None,
    /// A `0` was written at `at`; `take`: the card will take it when its debounce has
    /// run (it was old enough), else the vendor dropped it and it only stands.
    Pending { at: Instant, take: bool },
    /// A dropped `0`: in the composer as text until a backspace.
    Standing,
    /// The card took it and left.
    Taken,
}

/// What the Mock's world was when a `key` request arrived — the state a Tier-1 bind
/// projects onto its model ([`Mock::key_probes`]).
#[derive(Debug, Clone)]
struct KeyProbe {
    request: String,
    /// The open rating card's age at the request (`None`: no card on the served screen).
    card_age: Option<Duration>,
    /// The composer held a lone `0`.
    zero: bool,
    /// The composer held other text (a draft).
    draft: bool,
    /// A box was up (the phase read `Prompt`).
    boxed: bool,
    /// Where the vendor's digit stood ([`Mock::card_debounce`]).
    digit: Digit,
}

/// A mark of the server's launch witness ([`Mock::launch`]): the command
/// start's id, the person count it recorded, the command's word and the word
/// the server judged it (`person`, `typed`, `boxed`, `other`, `pending`, or
/// `none` for no mark).
#[derive(Debug, Clone, Copy)]
struct ServerLaunch {
    id: u64,
    seq: u64,
    word: &'static str,
    why: &'static str,
}

impl ServerLaunch {
    /// The word a read whose person count is `count` gets: a `person` mark
    /// the count moved past is `since`.
    fn why_at(self, count: Option<u64>) -> &'static str {
        match self.why {
            "person" if count != Some(self.seq) => "since",
            why => why,
        }
    }

    /// `text --json`'s `"launch"` value at `count`.
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

    /// `status`'s `launch= launch_seq=` at `count`.
    fn status(self, count: Option<u64>) -> String {
        match self.why_at(count) {
            "none" => "launch=none launch_seq=-".to_string(),
            why => format!("launch={why} launch_seq={}", self.seq),
        }
    }
}

/// A scripted server: each `call` pops the next reply and records the request; screens are
/// served in order from `screens` (the last repeats).
///
/// Built in: the content seq advances on every read; `await seq <n>` latches only while
/// another screen is to come and times out on the last; a modern `key if=` is judged against
/// the last served screen by the server's own `aterm_observe::row_matcher` (`OK seq=<n>` on a
/// match, else `OK skipped seq=<n>`, `ERR badregex` for a bad pattern), and an older host
/// answers it with a bare `ERR`. `await idle` answers at once; with `idle_skips_busy` it
/// passes over the busy screens next in the script when another follows, as a spinner never
/// holds still for the real `await idle 2000`.
///
/// A handoff: `by_index` answers chosen requests (by arrival order) ahead of every other
/// rule, and from `down`'s index on every request gets `down`'s reply. From
/// `drop_awaits_from` on every `await` is [`closed`] while reads are served. The first lost
/// reply served restarts the content seq at `handoff_seq`, as the adopting instance counts
/// from its own start; `restart` restarts it with no request failing. `await seq <n>` for an
/// `n` the content has not reached never latches, as on the real server. `delay` holds a
/// request's answer back.
struct Mock {
    requests: Vec<String>,
    /// Replies to non-`text`, non-`key` requests, in order (missing = the
    /// built-in answer).
    replies: VecDeque<CtlReply>,
    /// Scripted replies to `key` requests, in order (missing = the model).
    key_replies: VecDeque<CtlReply>,
    /// Screens to serve to `text` requests, in order (the last repeats).
    screens: Vec<Vec<String>>,
    served: usize,
    seq: u64,
    /// After this many `await seq` timeouts on the last screen the session is gone (`ERR
    /// exited`, as the real await answers when the session ends): how a `watch` test ends.
    vanish_after: Option<u32>,
    /// The session is gone (`ERR exited`) at the first sitting wait after this holds of what
    /// the mock has seen: a `watch` test that ends on what happened, not on a wall-clock `max`.
    exit_when: Option<fn(&Mock) -> bool>,
    /// The session is gone (`ERR exited`, as with `exit_when`) at the first
    /// sitting wait once this many requests have been served: a world that
    /// never moves on by itself, whose waits count nothing toward
    /// `vanish_after` (the panel's), ends after a count of the loop's own
    /// steps, never on a wall-clock `max` a loaded machine fits fewer into.
    exit_at_request: Option<usize>,
    /// The session is gone (`ERR exited`) at the first `await seq`/`await
    /// gone` on the LAST screen that asks for [`PARKED`] or more: the loop's
    /// own word that nothing is due before then (its wait asks the time to
    /// its next deadline, a whole step at most), so a run that must type
    /// nothing more ends at the decision, not after a window of sleeps. A
    /// turn gate holds the last screen as before; a test whose host or
    /// server changes under a sitting screen keeps its window instead.
    end_when_parked: bool,
    /// The run's clock, when a whole-run helper made it ([`Self::run_on`]): a
    /// wait on a screen that sits ([`Self::held`]) SKIPS the whole time it
    /// asks on it ([`super::super::fx::ScriptedClock::skip`]), as a server
    /// holding a still screen answers, instead of a nap of real time — or
    /// [`Self::stall_sleep`] of it at most, for a test that counts the steps
    /// of a wait. A run that types nothing at a held screen reads its `--max`
    /// out in a few waits, not in a window of sleeps.
    waits: Option<Arc<super::super::fx::ScriptedClock>>,
    /// The real instant a run on a clock that moves by the run's steps alone
    /// ([`Self::run_on`]) panics at, a request after it: its one-minute hang
    /// detector, as a loop that spun without a wait would never read its
    /// `--max` out.
    hang_at: Option<Instant>,
    /// What every request costs on that clock ([`Self::run_on`]): a round
    /// trip, as no real server answers in no time, so a step after a request
    /// is later than it.
    round_trip: Duration,
    /// Run after every request is served, on the loop's own thread: what
    /// stops a hosted run ([`run_engine_tests.rs`]'s `Stop`) at a step of the
    /// loop and an instant of the run's clock, never at a moment of another
    /// thread's.
    after_call: Option<Box<dyn FnMut() + Send>>,
    stalls: u32,
    /// `await idle` passes over busy screens (see the struct doc).
    idle_skips_busy: bool,
    /// Replies by request index, ahead of everything (see the struct doc).
    by_index: BTreeMap<usize, CtlReply>,
    /// From this request index on, every request gets this reply.
    down: Option<(usize, CtlReply)>,
    /// From this request index on, every `await` answers [`closed`].
    drop_awaits_from: Option<usize>,
    /// Where the content seq starts over at the first lost reply served.
    handoff_seq: Option<u64>,
    /// At this request index the content seq starts over at this value,
    /// before the request is served.
    restart: Option<(usize, u64)>,
    /// How long the answer to the request at an index is held back.
    delay: BTreeMap<usize, Duration>,
    /// The `history` reply (`None`: a host without the verb).
    history: Option<CtlReply>,
    /// `offscreen` replies, in order (none left: a host without the verb).
    offscreen: VecDeque<CtlReply>,
    /// Replies by verb (`turn`, `post`, `meta`), in order, ahead of the
    /// built-in answer for that verb.
    verb_replies: BTreeMap<&'static str, VecDeque<CtlReply>>,
    /// `await seq` on this screen index times out, as on the last, until a `turn` has been
    /// sent: the screen sits (a limit notice through its reset) and moves only once the
    /// watcher types. A `text` read meanwhile shows it again, unmoved.
    turn_releases: Option<usize>,
    /// The same, released by the first `key` request instead of a `turn`:
    /// a dialog that sits until a key reaches it (`await gone` on it
    /// stalls meanwhile, as on the last screen).
    key_releases: Option<usize>,
    /// A `key` request has arrived ([`Self::key_releases`]).
    keyed: bool,
    /// How many `key` requests were served ([`Self::key_gates`]).
    keys: usize,
    /// The same, by count: the screen of script index `i` sits until
    /// more than `k` `key` requests were served — a dialog's second tab
    /// that only the loop's own next key moves on.
    key_gates: Vec<(usize, usize)>,
    /// The same, for several: the `k`-th entry's screen sits until more
    /// than `k` turns have been sent.
    turn_gates: Vec<usize>,
    /// The same, released by the loop's badge: the screen of script index `i` sits until more
    /// than `k` `meta set attention` requests were served (a box that stands until the loop
    /// has asked about it).
    attention_gates: Vec<(usize, usize)>,
    /// How many `meta set attention` requests were served
    /// ([`Self::attention_gates`]).
    attentions: usize,
    /// The `kind=` token the loop's last `meta set attention
    /// owner=supervisor` named, `None` when it named none.
    attention_kind: Option<String>,
    /// The cursor's column on the screen of this index, over
    /// `cursor_col` (a suggestion drawn at column 2, then filled).
    screen_cols: BTreeMap<usize, usize>,
    /// The cursor on the composer's caret row (the last row a `❯` or `›`
    /// opens), where a real agent parks it, rather than on row 10: for
    /// a tall screen whose row 10 is the transcript.
    cursor_on_caret: bool,
    /// The cursor's row on the screen of this index, over
    /// [`Self::cursor_row`] and `cursor_on_caret` (a measured frame's own
    /// cursor).
    screen_cursors: BTreeMap<usize, usize>,
    /// The pane's width a read's `dims.cols` sends (120 when `None`): a
    /// frame measured at another width.
    pane_cols: Option<usize>,
    /// A whole read (`text --json`, no `tail=`) right after a `tail=` read
    /// serves the screen that tail did, unmoved, as the server does when
    /// nothing is drawn between: a box the loop takes whole (an `Edit file`
    /// box on a pane taller than the tail) is one script screen, not two.
    whole_sees_the_tail: bool,
    /// The last `text` read was a `tail=` read ([`Self::whole_sees_the_tail`]).
    last_read_tail: bool,
    /// A `turn` was sent.
    turned: bool,
    /// How many `turn`s were sent.
    turns: usize,
    /// A timed-out `await seq` on a screen that sits SLEEPS this long
    /// (the requested timeout at most) before it answers, as the real
    /// server does — for a loop whose waits are bounded in
    /// milliseconds, so it does not spin through thousands of requests. On
    /// the run's clock ([`Self::waits`]) the time is skipped, not slept.
    stall_sleep: Option<Duration>,
    /// Once more than this many `turn`s were sent, every request answers
    /// `ERR exited`: the session is gone — how a probe test ends.
    dead_after_turns: Option<usize>,
    /// The terminal title `status` reports (`subject=`, from the OSC
    /// stream): `None` reports none.
    title: Option<String>,
    /// The cursor column every read reports (the model's, `None`: 2, or
    /// 3 on a stray digit); off the caret row's column 2, text on the
    /// caret row is a typed draft ([`typed_draft`]).
    cursor_col: Option<usize>,
    /// The worker's `attention` meta as the server holds it: set by an accepted `meta set
    /// attention`, cleared by `meta unset attention`, read back (pct-encoded) by a bare
    /// `meta`, so the episode's close reads the `limited: …` its escalation wrote.
    attention: Option<String>,
    /// Another writer's text (`claude needs approval: …`) that lands over ours right after the
    /// loop's `meta set attention`, while the episode is open.
    foreign_attention: Option<String>,
    /// The session's `cwd=` in the `meta` reply (`None`: `-`).
    cwd: Option<String>,
    /// `meta agent_cwd=`: the foreground program's own directory.
    pub agent_cwd: Option<String>,
    /// `meta trusted_cwd=`: the folder a worker's `spawn agent=claude`
    /// trusted for it (pct-encoded as the server writes it; `None`: `-`).
    pub trusted_cwd: Option<String>,
    /// What `help …` answers (stdout): by default the catalog line for
    /// `key` of a host that has no generation fence.
    help: String,
    /// What `help send` answers instead of [`Self::help`], when set: a
    /// host whose `send` names its generation fence (`if-gen=`).
    help_send: Option<String>,
    /// Scripted replies to `send` requests, in order (missing = the
    /// model): a partial write, a lost answer.
    send_replies: VecDeque<CtlReply>,
    /// Scripted replies to `restart` requests, in order (missing: `OK
    /// restarting …`, the act made).
    restart_replies: VecDeque<CtlReply>,
    /// `status`'s `hold=` and `fabric=`.
    hold: &'static str,
    fabric: &'static str,
    /// `hold=` after this many `status` reads is `0` (the hold lifts).
    hold_lifts_after: Option<usize>,
    status_reads: usize,
    /// At the first `key`, the screen is replaced by this one before the
    /// guard is judged (and the next read serves it): a box swapped
    /// between the loop's read and its press.
    swap_at_key: Option<Vec<String>>,
    /// How many `key` requests pass before [`Self::swap_at_key`] swaps
    /// (0: the first key): a screen that changes between the loop's
    /// read and a later key of its answer.
    swap_after_keys: usize,
    /// `key if-gen=<e.s>` is known: a press whose generation is not the
    /// screen's (`1.<content seq>`) answers `OK skipped reason=changed
    /// seq=<now>`, and `if-seq=` is `ERR usage`, as the fenced host does.
    gen_fence: bool,
    /// `text --json` carries `"gen":"1.<seq>"` (a host with the fence).
    sends_gen: bool,
    /// `text --json`'s `"human_ms"` as sent, in turn (the last repeats):
    /// `"null"` for no person ever, a number for its age; empty sends
    /// none (a host before the person stamp).
    human: Vec<&'static str>,
    /// `text --json`'s `"human_ms"` for every read of the script's screen at an index, ahead of
    /// [`Self::human`]: a person who typed into that screen's turn, however often it is read.
    human_on: BTreeMap<usize, &'static str>,
    /// Every `text` read as served: the script index of its screen and
    /// the `"human_ms"` it carried (`""` none) — what a Tier-1 bind
    /// projects a read onto.
    reads: Vec<(usize, &'static str)>,
    /// Requests not counted in the script's indexes (the start-up
    /// reconcile read).
    uncounted: usize,
    /// `await idle` never latches (a screen that animates, as Claude Code's blinking tool row)
    /// and answers `OK timeout` after at most 50 ms of its own timeout.
    idle_never: bool,
    /// This many `key` requests find the content seq moved one on (a row
    /// that ticks between the read and the press, the box unchanged).
    tick_at_key: usize,
    /// The screen redraws itself this often on the run's clock, the box
    /// unchanged (Claude Code's auto-deny countdown, `1:59`, `1:58`, …): a
    /// `key` fenced `if-gen=` that arrives this long or longer after the last
    /// `text` read finds the content seq moved one on and is skipped
    /// (`OK skipped reason=changed`) before it reaches the program, so it
    /// releases no gate ([`Self::key_gates`]).
    ticks: Option<Duration>,
    /// When the last `text` read was served, on the run's clock
    /// ([`Self::ticks`]).
    read_at: Option<Instant>,
    /// `status program=`: the session's foreground program.
    program: &'static str,
    /// The program's name as the server resolves it late (off its event loop): `await agent
    /// …` makes it this, and latches. Without one, `await agent` latches only while the
    /// program is an agent and times out otherwise.
    program_resolves_to: Option<&'static str>,
    /// `status agent=`: the server's own verdict on the screen (`-`:
    /// none, as for a shell showing a captured box).
    agent: &'static str,
    /// The supervisor-claim requests (`meta set|unset supervisor …`), kept apart from
    /// `requests` so every script's indexes run without them, and their replies, in order
    /// (none left: `OK`).
    claims: Vec<String>,
    claim_replies: VecDeque<CtlReply>,
    /// Every request in arrival order, the claim's included, each with
    /// whether it was answered `OK` — what a Tier-1 bind projects.
    order: Vec<(String, bool)>,
    /// A person's own (bare-owner) attention entry, and whether it was
    /// set after this loop's (the server shows the most recent).
    human_attention: Option<String>,
    human_on_top: bool,
    /// The worker's NEEDS YOU entry (`owner=needs`), as the server holds it:
    /// its text, and every `meta … owner=needs` request in order — no
    /// scripted reply spent on it, and no attention of the loop's or a
    /// person's moved.
    needs_attention: Option<String>,
    needs_calls: Vec<String>,
    /// Scripted replies to the `owner=needs` requests, in order (none left:
    /// `OK`): a cap's refusal, a lost request. One not `OK` leaves
    /// [`Self::needs_attention`] as it was.
    needs_replies: VecDeque<CtlReply>,
    /// Claude Code's digit rule for a rating card: a `0` pressed within this long of the
    /// first read that served the card is dropped (it stays in the composer as text).
    /// `None`: a card takes its digit at once.
    card_ignores_within: Option<Duration>,
    card_first_served: Option<Instant>,
    /// The `0`s a guarded press wrote inside that window.
    early_zeroes: usize,
    /// Claude Code's debounce for a rating card's digit (`debounceMs`, 400 ms; a test scales
    /// it): with this set the Mock reacts to the card. The script is three screens: [0] the
    /// card, [1] the card with a lone `0` in its composer, [2] the screen after it. A guarded
    /// `0` written to a card older than `card_ignores_within` is taken this long after it was
    /// written (the card leaves: [2]) unless any other key reached the composer in between
    /// (a backspace cancels the take and the card stays: [0]); a `0` written inside the mount
    /// window is dropped and stands in the composer ([1]) until a backspace. `await gone` and
    /// `await seq` wait in real time for that.
    card_debounce: Option<Duration>,
    digit: Digit,
    reactive_at: Option<usize>,
    /// The world at each `key` request, in order.
    key_probes: Vec<KeyProbe>,
    /// A card whose digit hook is not armed (or that mounted again under the digit) drops
    /// every digit written to it, whatever its age ([`Self::card_debounce`]).
    card_drops_digits: bool,
    /// Replies to `key backspace` requests only, in order, ahead of the model: a
    /// backspace the server refuses while the rest of the world runs on.
    backspace_replies: VecDeque<CtlReply>,
    /// `status human_ms=`: how long ago a person typed (`None`: the field is absent, as from
    /// an older server).
    human_ms: Option<u64>,
    /// `status hand=`: whose hand is on the session (`-`: nobody).
    hand: &'static str,
    /// `text --json`'s `"human_seq"`, the person count (`None`: a host
    /// that sends none, which takes no `if-human=` either).
    human_seq: Option<u64>,
    /// Per-screen wire count, overriding `human_seq` without changing the
    /// server's own counter: a later read can omit a formerly supplied field.
    human_seq_on: BTreeMap<usize, Option<u64>>,
    /// `text --json`'s `"submits"` on each screen (by index), the host's
    /// count of submissions written to the session (aterm-gui's
    /// `submit_count`); a screen with none sends none, as an older host.
    submits_on: BTreeMap<usize, u64>,
    /// The screens (by index) whose reads say they caught the program's DEC 2026 frame
    /// unfinished (`"frame":"open"`): a redraw the server read midway.
    open_on: std::collections::BTreeSet<usize>,
    /// `text --json`'s `"inputs"` on each screen (by index): the host's
    /// count of input writes before the rows (`None`: one in flight) and
    /// after them (aterm-gui's `input_settled`, `input_sent`); a screen with
    /// none sends none, as an older host.
    inputs_on: BTreeMap<usize, (Option<u64>, u64)>,
    /// The server's launch witness (aterm-gui's `launch_witness`), as its
    /// last signed command start left it; `None`: a host that sends none.
    /// Published as the server publishes it, `text --json`'s `"launch"`
    /// and `status launch= launch_seq=` each judged against the server's
    /// own count at the read ([`Self::human_seq`], whatever
    /// [`Self::human_seq_on`] puts on the wire), so a `person` mark reads
    /// `since` once that count moved.
    launch: Option<ServerLaunch>,
    /// This many `key` requests find a person keyed just before them (the count moved past
    /// every read so far), as the round trip between the loop's read and its key allows.
    person_at_key: usize,
    /// The same as [`Self::person_at_key`], for `send`.
    person_at_send: usize,
    /// The `status` read of this number (1-based) finds a PERSON keyed just
    /// before it — the count moved past the screen read before it — as the
    /// round trip between the loop's read and its `status` allows.
    person_at_status: Option<usize>,
    /// This many `send` requests WRITTEN are followed at once by a PERSON's
    /// key (the count moves on after the write): a person who keyed between
    /// the loop's text and its next read.
    person_after_send: usize,
    /// The [`Self::order`] indexes of the writes skipped `reason=person`
    /// (what a Tier-1 bind reads as the person's key that raced them).
    person_skipped: Vec<usize>,
    /// The attrs a `cell` read answers (`dim`: the placeholder).
    cell_attrs: &'static str,
    /// The colour a `cell` read answers (`rrggbb`).
    cell_fg: &'static str,
    /// The server does not answer a `cell` read (an older one): `ERR`.
    cell_unreadable: bool,
    /// The reply a `cell <row> <col>` read gets at that cell, over
    /// [`Self::cell_fg`] and [`Self::cell_attrs`]: a measured cell (a
    /// completion menu's entry drawn in its colour).
    cells: BTreeMap<(usize, usize), String>,
    /// The reply a `cell abs=<n> <col>` read gets at a row of
    /// [`Self::scrollback`] (`n` below its length, as the server places the
    /// grid's first row at `screen_first=`); a grid row's is
    /// [`Self::cells`]'s at `n` less the history's length.
    history_cells: BTreeMap<(usize, usize), String>,
    /// `human_ms` for the status reads to come, one each, before it
    /// answers for all the rest: a person who stops typing.
    human_ms_reads: VecDeque<Option<u64>>,
    /// `status human_ms=` from the `n`-th `text` read on (1-based; the last entry at or below
    /// the reads served), over [`Self::human_ms_reads`] and [`Self::human_ms`]: the person
    /// as the real server's `status` sees them after that read, from the stamp its `text
    /// --json` used.
    status_human_from: BTreeMap<usize, u64>,
    /// The server publishes its agent verdict (`status agent_rev=`) and pushes it: `await
    /// agent <words>` latches at the first screen to come whose phase is one of the words,
    /// passing over the rest unread.
    agent_pushes: bool,
    /// The session's `questions` meta word, after `supervisor=` in the bare `meta` reply
    /// (`None`: a build whose meta has no such field).
    questions: Option<&'static str>,
    /// The owner's hand-back: the word `questions` becomes once the loop's own badge is up
    /// (`meta set attention owner=supervisor …`); `Some(None)` unsets it.
    questions_after_badge: Option<Option<&'static str>>,
    /// The word `questions` becomes from the `n`-th bare `meta` read on (1-based, the start-up
    /// reconcile read counted): a word changed while a dialog is answered.
    questions_at: Vec<(usize, Option<&'static str>)>,
    /// The session's `autopilot` meta word, after `questions` in the bare `meta` reply (`None`:
    /// a build whose meta has no such field; `Some("-")`: unset).
    autopilot: Option<&'static str>,
    /// The word `autopilot` becomes from the `n`-th bare `meta` read on (1-based, the start-up
    /// reconcile read counted): the owner's chip clicked while the loop runs.
    autopilot_at: Vec<(usize, Option<&'static str>)>,
    /// The word `autopilot` becomes just after the script's screen at an index is read, whether
    /// or not the loop reads `meta`.
    autopilot_on: BTreeMap<usize, Option<&'static str>>,
    /// How many bare `meta` reads were served.
    metas: usize,
    /// `status`'s input fields (`input= input_bytes= input_wait_ms=
    /// fg_rss_mb=`); empty: a host that publishes none.
    input: &'static str,
    /// `input` after this many `status` reads is this (the stall lifts,
    /// or starts), in order: the last one passed is the one read.
    input_after: Vec<(usize, &'static str)>,
    /// The cursor's grid row every read reports, over the model's (row 10,
    /// or the caret row): where a real agent parks it, e.g. the focused
    /// option of a box it draws.
    cursor_row: Option<usize>,
    /// Bodies served verbatim to the next `text … tail=` reads, ahead of
    /// the model, consuming no script screen and moving no seq.
    tail_replies: VecDeque<String>,
    /// The server's pushed verdict ([`Self::agent_pushes`]) is read from
    /// only the last `n` rows of each screen, so a box drawn above them
    /// does not move it. `None`: the whole screen.
    agent_zone: Option<usize>,
    /// `status sid=`: the session's local id (`-`: none named).
    status_sid: &'static str,
    /// The `who` roster's rows, answered to every `who` (`None`: the
    /// built-in answer).
    who: Option<String>,
    /// The session each [`Ctl::call_at_host_of`] aimed its request at, in
    /// order (the request itself is served as [`Ctl::call`] serves it).
    aimed: Vec<String>,
    /// `await gone` judged as the server judges it: on the screen current during the wait
    /// (the next to be served, when the worker moved on), only over its `rows <a> <b>`; a row
    /// still matching runs the wait out (`OK timeout`, counted in [`Self::gone_held`]). Off:
    /// any screen still to come latches it.
    gone_on_next: bool,
    /// How many `await gone` waits ran out under [`Self::gone_on_next`].
    gone_held: usize,
    /// The request just served was an `await gone` on a whole row (`^…`, as `box_anchor`
    /// builds) that ran out under [`Self::gone_on_next`] ([`Self::after_gone`]).
    gone_ran_out: bool,
    /// For each of [`Self::reads`], whether it came straight after such an `await gone`: the
    /// wait's own read (`Session::wait_for_next`), which the Tier-1 bind of
    /// `run_box_key_tests.rs` keeps apart from the looks.
    after_gone: Vec<bool>,
    /// `modes`' `alt_screen=` row (`None`: a host whose `modes` names
    /// none — the bare `OK` every unscripted verb gets).
    alt_screen: Option<bool>,
    /// The main screen's history: the rows right above the screen, oldest
    /// first, the screen's own first row the absolute row after them.
    /// `search <^literal> regex` answers its hits there and on the screen
    /// last served, under the header's `screen_first=` (`search` with no
    /// pattern, the header alone), and `line <n>` each row, as the server
    /// does (`None`: a host whose `search` and `line`
    /// get the bare `OK` every unscripted verb gets).
    scrollback: Option<Vec<String>>,
    /// The screen (by index) from which [`Self::scrollback`] stands above
    /// the screen: before it is served, the history is empty (`None`: from
    /// the first) — the history of one tall turn, not of every screen.
    scrollback_from: Option<usize>,
    /// `line <n>` of a history row above this one is `ERR evicted`.
    evicted_below: u64,
    /// The ALTERNATE screen's archive of the rows that scrolled off it,
    /// oldest first, archived rows `1..=n`: `offscreen` answers its newest
    /// (`tail=`) after `since=`, the screen last served appended under
    /// `screen=1`, as the server does ([`Self::archive_read`]; `None`: the
    /// scripted [`Self::offscreen`] replies, or none).
    alt_archive: Option<Vec<String>>,
    /// A gap after the archive's newest row (`breaks=1`): a redraw with no
    /// overlap, a resize.
    archive_gap: bool,
    /// `line <n>` of this row is `ERR unreadable`.
    unreadable_row: Option<u64>,
    /// What the `search` header carries after its count (` incomplete
    /// rewrapping`: a search that may have left out newer rows).
    search_qualifier: &'static str,
    /// Claude Code's `/usage` panel as a world that reacts to the loop's writes
    /// ([`PanelWorld`]); `None`: the script alone.
    panel: Option<PanelWorld>,
    /// Claude Code's limit-reset program as a world that reacts to the
    /// loop's keys ([`ResetWorld`]); `None`: the script alone.
    reset: Option<ResetWorld>,
    /// The session's TASK RECORD as `task` answers it: its rev and the
    /// fields after it (`state=<s> from=ctl … result_bytes=<n> [why=…]
    /// [result_path=…]`); `None`: no task (`OK task -`). A `task mark`
    /// sets it as the server does — the whole new state, `changed=0` and
    /// no rev for the same one. Its requests run outside every script's
    /// indexes, as the claim's do ([`Self::task_requests`]).
    task: Option<(u64, String)>,
    /// Every `task …` request, in order.
    task_requests: Vec<String>,
    /// Once this many screens are served, the record — `done` — moves to
    /// `working why=turn` (rev + 1): the server's own reopen at a turn
    /// someone else typed into the finished worker.
    task_reopens_at: Option<usize>,
    /// How many `task mark` requests, from the first, are answered `ERR
    /// busy` (a seamless update committing) and change nothing.
    task_busy: usize,
    /// How many bare `task` reads, from the first, are turned away
    /// unserved (`ERR control server busy`): a read the loop asks again.
    task_lost: usize,
    /// THE HOST HANGS THE LOOP'S CONNECTION UP at the first request whose
    /// words (past the `@sid`) begin with this: it and every request after
    /// it fail in the transport, as the window's host cuts a loop short
    /// to stop it at once (`Ctl::interrupter`).
    hang_up_at: Option<&'static str>,
    /// The connection was hung up ([`Self::hang_up_at`]).
    hung_up: bool,
}

/// Where the `/usage` panel's world stands ([`PanelWorld`]).
#[derive(Debug, Clone, PartialEq, Eq)]
enum PanelAt {
    /// The composer, empty: the point the script rests on.
    Rest,
    /// The composer holds this text, written by a `send`, not submitted.
    Typed(String),
    /// The Settings panel stands where the composer was.
    Open,
    /// The loop's own act was submitted (or a turn started): the script
    /// runs on from here.
    Over,
}

/// What the panel's world was when a write of the probe's arrived (a `send … /usage` or a
/// `key … esc`, written or not), which a Tier-1 bind projects onto `SupervisorUsageProbe`
/// ([`usage_probe_violations`]).
#[derive(Debug, Clone)]
struct PanelProbe {
    request: String,
    /// The model's `pan` of the screen the server held: 0 no panel, 1 a
    /// panel that does not read settled, 2 one that does (by the loop's
    /// own readers, on the measured screens).
    pan: i64,
    /// A person keyed the session within the world's grace ([`PanelWorld::grace`], the
    /// truth, whatever grace the loop was configured with).
    hand: bool,
    /// The session was halted (`status hold=1`).
    hold: bool,
    /// The screen read busy: a turn running.
    turn: bool,
    /// A panel was open, or `/usage` stood typed and unsubmitted.
    owed: bool,
    /// How many probes the host had granted, and how many it had been
    /// told the end of: one more granted than told is a step still
    /// reading its panel.
    claims: usize,
    tolds: usize,
    at: Instant,
}

/// What the panel's world and the test's host ([`ProbeHost`]) share: the
/// host's counts, read by the world at each write, and the world's
/// screen, read by the host when it is handed a sample.
#[derive(Debug, Default)]
struct PanelShared {
    pan: std::sync::atomic::AtomicI64,
    hand: AtomicBool,
    claims: std::sync::atomic::AtomicUsize,
    tolds: std::sync::atomic::AtomicUsize,
}

/// Claude Code's `/usage` panel as the loop meets it (2.1.287; the frames are the measured
/// screens, `aterm_phase::prompt::fixtures::USAGE_*`), a world that reacts to what is written:
///
/// * the script's last screen is the rest point, an empty composer. A `send` the server
///   writes there puts its text in the composer; Enter on `/usage` opens the panel (each read
///   after it serves the next of `frames`, the last for ever), and Enter on anything else is
///   the loop's own act taken: the script runs on with the next of `then`;
/// * an Escape the server writes (its `if=` guard judged against the frame on screen) closes
///   the panel, the composer back empty, unless `esc_fails` says this one does not take or
///   `esc_replies` scripts a refusal;
/// * while text stands typed or the panel is open, a wait answers for the screen as it is
///   (`await seq` latches once it moved, `await gone` once no row matches, each timing out
///   after a short nap otherwise) and counts nothing toward [`Mock::vanish_after`]; a test's
///   [`Mock::exit_when`] still ends the run there.
///
/// Every write of the probe's is recorded with the world it arrived in ([`PanelProbe`]).
#[derive(Debug)]
struct PanelWorld {
    frames: Vec<Vec<String>>,
    at: PanelAt,
    /// The composer screen the world stands on (taken at the first
    /// `send`; given, for a world that starts with the panel open).
    rest: Option<Vec<String>>,
    /// Enter on `/usage` opens the panel (`false`: nothing comes, and the
    /// composer is back empty).
    opens: bool,
    /// Enter on `/usage` starts a turn instead: the script runs on with
    /// these screens.
    turn_instead: Option<Vec<Vec<String>>>,
    /// This many written Escapes leave the panel open.
    esc_fails: usize,
    /// Replies to an Escape at the open panel, in order, ahead of the
    /// world: a refusal.
    esc_replies: VecDeque<CtlReply>,
    /// The panel leaves by itself once it has been read this many times.
    leaves_after: Option<usize>,
    /// A person keys the session as this read of the open panel is
    /// served (1-based): the read carries the moved count.
    person_after: Option<usize>,
    /// The answer to this read of the open panel never comes (1-based).
    lose_read: Option<usize>,
    /// The loop's host stops it as this read of the open panel is
    /// served (1-based): the flag it sets.
    stop_at_read: Option<(usize, Arc<AtomicBool>)>,
    /// The panel repaints for ever: `await idle` never latches while it
    /// is open.
    repaints: bool,
    /// What the screen shows once the panel has closed, where it is not
    /// the composer screen as it was.
    after: Option<Vec<String>>,
    /// The panel half erased: what the first read after the closing Escape shows before the
    /// composer is drawn (measured on 2.1.287: the rule, header and upper rows gone, the
    /// lower rows and cancel hint still on the grid, no composer). The read after it shows
    /// the composer ([`Self::mends`]).
    torn: Option<Vec<String>>,
    /// The screen a half-erased panel gives way to, and whether the
    /// half-erased one has been served to a read yet.
    mends: Option<(bool, Vec<String>)>,
    /// The screens the script runs on with after each of the loop's own
    /// acts, in turn; none left, the composer stands as it was.
    then: VecDeque<Vec<Vec<String>>>,
    /// How many reads of the open panel were served.
    reads: usize,
    /// When a person last keyed, and how long their hand counts (the
    /// truth a Tier-1 bind projects, whatever the loop's own grace).
    keyed_at: Option<Instant>,
    grace: Duration,
    /// Waits are answered for the screen and counted nowhere although
    /// the composer stands empty: the panel never opened, and the loop
    /// is waiting for it — until its next ordinary (`tail=`) read.
    quiet: bool,
    /// `/usage` typed into the composer opens Claude Code's slash menu
    /// under it, in place of the footer: the menu 2.1.300 was MEASURED to
    /// draw there (`SLASH_MENU_USAGE_MEASURED`), its pointer on `/usage`.
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

/// Where the limit reset's world stands ([`ResetWorld`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResetAt {
    /// The usage-limit dialog.
    Dialog,
    /// Its title over `Checking for a reset…`.
    Checking,
    /// The confirmation.
    Confirm,
    /// Its title over `Resetting your limits…`.
    Resetting,
    /// No dialog: the composer, and the script runs on.
    Gone,
}

/// When the reset's world ends the session by itself (`ERR exited` to the
/// next wait it answers): how a run ends whose dialog never leaves, on
/// what it was run to show and never on a clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResetExit {
    /// The host has been told how the step ended.
    Told,
    /// The confirmation refused a key inside its guard.
    Refused,
    /// The loop raised its badge for the dialog.
    Asked,
    /// `status` was read this many times.
    Statuses(usize),
    /// The confirmation's yes was taken: a reset spent.
    Spent,
}

/// What the reset's world was when a `key` request arrived (written or not), which a Tier-1
/// bind projects onto `SupervisorLimitReset` (`reset_violations`).
#[derive(Debug, Clone)]
struct ResetProbe {
    request: String,
    /// The key's name (`down`, `enter`, `esc`).
    key: String,
    at: ResetAt,
    /// The model's `dlg` of the screen the server held.
    dlg: i64,
    /// The confirmation was inside the vendor's key guard.
    guard: bool,
    /// The confirmation's cursor was on its yes.
    focus: bool,
    /// A person keyed the session within the world's grace ([`ResetWorld::grace`], the
    /// truth, whatever grace the loop was configured with).
    hand: bool,
    /// The session was halted (`status hold=1`).
    hold: bool,
    /// The model's `claim`, `ph` and `own` as they stood.
    claim: i64,
    ph: i64,
    own: bool,
    /// The dialog row the cursor was on, at the dialog, and whether it
    /// was the grant row (by the world's own labels: the oracle owes
    /// nothing to the reader under test).
    row: Option<String>,
    on_grant: bool,
    /// The server wrote the key: no fence skipped it and nothing refused it.
    written: bool,
}

/// What the reset's world and the test's host ([`ResetHost`]) share: the
/// model's `claim` (the host's), its `ph` and `own` (set by the keys the
/// world took, ended by the host's last word), and the world as it
/// stood at the last request, for the host's own steps.
#[derive(Debug, Default)]
struct ResetShared {
    claim: std::sync::atomic::AtomicI64,
    ph: std::sync::atomic::AtomicI64,
    own: AtomicBool,
    dlg: std::sync::atomic::AtomicI64,
    hand: AtomicBool,
    hold: AtomicBool,
}

/// CLAUDE CODE'S LIMIT-RESET PROGRAM AS THE LOOP MEETS IT, a world that
/// reacts to the keys the server writes. EVERY SCREEN AND EVERY
/// TRANSITION OF IT IS COMPOSED FROM THE 2.1.304 BUNDLE, NEVER SEEN
/// (`docs/SPEC-claude-accounts-resets-2026-10-02.md` §0; the frames are
/// `aterm_phase::reset::fixtures`, each COMPOSED and saying so):
///
/// * the script's LAST screen is the usage-limit dialog the world opens
///   on (`labels`, the cursor on `focus`). An arrow moves the cursor;
///   Enter on the grant row redraws the dialog as its title over
///   `Checking for a reset…`; Enter on
///   any other row, and Escape, end the dialog;
/// * `Checking…` stands for `check_reads` reads, then the confirmation
///   mounts; Escape there ends
///   the dialog and prints nothing;
/// * the confirmation refuses every key inside `guard` of its mount and
///   RE-ARMS on each one it refuses (the vendor's 250 ms rule); past it,
///   Enter with the cursor on its yes redraws it as its title over
///   `Resetting your limits…` (`yes_taken`: a reset SPENT), Enter on its
///   no and Escape end it with the reset kept — unless `enter_drops` /
///   `esc_drops` say this one does not take;
/// * `Resetting…` takes no key, and after `claim_reads` reads the dialog
///   ends;
/// * the dialog gone, the screen is `after`.
///
/// While a dialog stands, and after it until the screen without it has been read and the
/// host told how the step ended, a wait answers for the screen as it is and counts nothing
/// toward [`Mock::vanish_after`]. Every key is recorded with the world it arrived in
/// ([`ResetProbe`]).
#[derive(Debug)]
struct ResetWorld {
    at: ResetAt,
    labels: Vec<String>,
    /// The dialog's row that spends a reset (2.1.304's `Use a <kind> …`
    /// row, of one kind of reset's): the world's own oracle of the row.
    grant: String,
    focus: usize,
    check_reads: usize,
    claim_reads: usize,
    /// The confirmation as it mounts (a frame of
    /// `aterm_phase::reset::fixtures`).
    confirm: Vec<String>,
    /// The confirmation is the plain one: the model's `dlg` 4 (the loop's verdict would enter
    /// it but for its focus); `false`: 5 (it must be kept).
    exact: bool,
    guard: Duration,
    mounted: Option<Instant>,
    /// The keys the confirmation refused inside its guard.
    refused: Vec<String>,
    enter_drops: usize,
    esc_drops: usize,
    /// An Escape the confirmation does not take moves its cursor to the
    /// other option: a person's arrow, landing with it.
    cursor_moves_at_esc: bool,
    /// The screen once the dialog is gone.
    after: Vec<String>,
    /// A person keys the session as this read of the confirmation is
    /// served (1-based): the read carries the moved count.
    person_at_read: Option<usize>,
    /// This many keys of the loop's find a person keyed just before them at the
    /// confirmation, as the round trip between its read and its key allows.
    person_at_confirm_key: usize,
    /// …and this many at the usage-limit dialog: a person keys after the
    /// read and the `status` the dialog was judged on, while the host
    /// is asked for the claim.
    person_at_row_key: usize,
    /// A row the dialog draws as the loop's first arrow lands, at this
    /// index, the cursor kept on its own row (the vendor's Select keeps its
    /// focus on the row's value): Claude Code 2.1.304 draws a row per kind
    /// of reset as its offers come in, while the dialog is up.
    drawn_at_arrow: Option<(usize, String)>,
    /// A person presses Enter on the confirmation's yes at the first read of it this long
    /// after it mounted: the claim begins with no key of the loop's.
    yes_by_person_after: Option<Duration>,
    /// The screen while the vendor claims (`Resetting your limits…`
    /// under the confirmation's title, as composed).
    resetting: Vec<String>,
    /// The answer to the next key the server writes at this state never
    /// comes, though the key was written and taken.
    lose_reply_to: Option<(ResetAt, &'static str)>,
    /// How many reads the state the world is in was served.
    reads: usize,
    /// The dialog gone, a read has been served since: until one is, the
    /// world still answers the waits for the screen as it is.
    gone_read: bool,
    keyed_at: Option<Instant>,
    grace: Duration,
    /// The rows whose Enter the dialog took, in order.
    chosen: Vec<String>,
    /// How many times the yes was taken: resets spent.
    yes_taken: usize,
    /// Waits answered while the world held the screen: a run that never
    /// ends its step is cut off, not waited on for ever.
    held_waits: usize,
    /// The session ends by itself once this has happened.
    exits_when: Option<ResetExit>,
    /// The host's key is off: it is never told how its claim ended, and
    /// the script runs on once the screen without the dialog was read.
    untold: bool,
    probes: Vec<ResetProbe>,
    shared: Arc<ResetShared>,
}

impl ResetWorld {
    /// The world at the usage-limit dialog `labels`, the cursor on
    /// `focus`, whose grant row opens the plain confirmation.
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

    /// The dialog as drawn: the hand-built panel frame, its rows these.
    fn dialog(&self) -> Vec<String> {
        let labels: Vec<&str> = self.labels.iter().map(String::as_str).collect();
        aterm_phase::prompt::fixtures::limit_options_panel(&labels, self.focus, true)
    }

    /// The screen the world shows where it stands.
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

    /// The confirmation is inside the vendor's guard at `now` (the run's clock, [`run_now`]).
    fn guarded(&self, now: Instant) -> bool {
        self.at == ResetAt::Confirm
            && self
                .mounted
                .is_some_and(|at| now.saturating_duration_since(at) < self.guard)
    }

    /// The dialog's cursor is on the grant row, by the world's own labels:
    /// a reader seeded with a defect cannot make the world agree with it.
    fn on_grant(&self) -> bool {
        self.labels[self.focus] == self.grant
    }

    /// The row [`Self::drawn_at_arrow`] names, drawn once, the cursor kept on
    /// the row it is on.
    fn draw_at_arrow(&mut self) {
        if let Some((at, label)) = self.drawn_at_arrow.take() {
            self.labels.insert(at, label);
            if at <= self.focus {
                self.focus += 1;
            }
        }
    }

    /// The confirmation's cursor moves to its other option.
    fn move_confirm_cursor(&mut self) {
        let rows: Vec<usize> = (0..self.confirm.len())
            .filter(|&i| {
                let row = self.confirm[i]
                    .trim_start()
                    .trim_start_matches('❯')
                    .trim_start();
                // 2.1.304's `Yes, use my <kind>`, of every kind.
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

    /// The confirmation's cursor is on its yes: its first option, whatever the server words
    /// it.
    fn on_yes(&self) -> bool {
        aterm_phase::parse_prompt_v2(&self.confirm)
            .is_some_and(|p| p.options.first().is_some_and(|o| o.focused))
    }

    /// The model's `dlg` of where the world stands: at the dialog, 1 where
    /// it draws the grant row, else 2 (the slot holds no grant).
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

/// A worker reading its input: nothing unread.
/// What a wait on the last screen asks at least, to read as the loop parked
/// ([`Mock::end_when_parked`]): half the loop's longest step (`WAIT_STEP`,
/// 20 s), and longer than every window a test ran on and every timer it sets
/// (a reset, a grace, a back-off of seconds asks less: it is due sooner).
const PARKED: Duration = Duration::from_secs(10);

const INPUT_CLEAR: &str = "input=clear input_bytes=0 input_wait_ms=0 fg_rss_mb=-";
/// A stalled-input reading: one Enter unread for 2m41s, the worker at
/// 38.8 GiB.
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
/// What aterm 0.81.0 answers `key if=… 1` (and `key nosuchkey`): the
/// option read as a key name, `aterm-ctl: ERR` and nothing more.
fn bare_err() -> CtlReply {
    CtlReply {
        code: 1,
        stdout: String::new(),
        stderr: "aterm-ctl: ERR\n".to_string(),
    }
}
/// What the client prints when an instance hands its sessions to a new one
/// under a running `watch`: the connection closed before any answer.
fn closed() -> CtlReply {
    failed(1, "server closed the connection without responding")
}
/// The run's monotonic clock now ([`Mock::waits`]): the system's, and what the
/// run's waits skipped; the system's alone with no run's clock.
fn run_now(waits: Option<&super::super::fx::ScriptedClock>) -> Instant {
    waits.map_or_else(Instant::now, super::super::fx::ScriptedClock::mono)
}

/// How long since `at` on [`run_now`]'s clock.
fn run_since(waits: Option<&super::super::fx::ScriptedClock>, at: Instant) -> Duration {
    run_now(waits).saturating_duration_since(at)
}

/// A nap of `d` on the run's clock ([`Mock::waits`]: skipped on one that runs on the run's
/// steps), else of real time.
fn run_nap(waits: Option<&super::super::fx::ScriptedClock>, d: Duration) {
    match waits {
        Some(clock) => clock.sleep(d),
        None => std::thread::sleep(d),
    }
}

/// The wait before a world's `OK idle` for a screen that held still `still`: the whole of it
/// on a run's clock that moves by the run's steps alone, as a server answers only once the
/// screen sat that long; else a `nap` of it at this server's scale.
fn idle_nap(waits: Option<&super::super::fx::ScriptedClock>, still: Duration, nap: Duration) {
    match waits {
        Some(clock) if clock.is_virtual() => clock.skip(still),
        waits => run_nap(waits, still.min(nap)),
    }
}

/// A client-side failure: `aterm-ctl: <text>` on stderr, nothing on stdout.
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
            // A real directory outside any repository: a git read is
            // judged in the session's cwd (`policy::git_config`).
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
            human_seq: None,
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
    /// A `task` request ([`Self::task`]): the record, or the record after
    /// the mark and whether it moved.
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
    /// The bare `meta` reply, `attention=` pct-encoded as the server spells
    /// it (`-` = unset): the latest entry of the loop's keyed one (owner
    /// `supervisor`) or a person's bare one — the worker's NEEDS YOU entry
    /// (owner `needs`) where it stands alone — its owner, how many stand,
    /// and each one's kind by owner (`attention_kinds=`, `<kind>:<owner>`).
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
    /// The `status` reply: the fields the loop reads.
    fn status_line(&mut self) -> String {
        self.status_reads += 1;
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
        // The panel world's person: as long ago as they keyed.
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
        let title = self.title.as_deref().map_or_else(String::new, |t| {
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
            format!(" subject={enc} subject_source=osc")
        });
        format!(
            "OK schema=1 sid={} phase=running hold={} fabric={} program={} agent={}{input} \
             seq={} hand={}{human}{title}\n",
            self.status_sid, self.hold, self.fabric, self.program, self.agent, self.seq, self.hand
        )
    }
    /// The measured reply of the `cell <row> <col>` read `tail` names
    /// ([`Self::cells`]), or of `cell abs=<n> <col>` ([`Self::history_cells`]
    /// above the grid, [`Self::cells`] on it).
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

    /// `search <^literal> regex` over [`Self::scrollback`] and the screen
    /// last served: the rows that open with the literal.
    fn scrollback_search(&self, tail: &[&str]) -> CtlReply {
        let history = self.history();
        let verb = "search";
        // No pattern: the server's answer that places the screen alone.
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

    /// `line <n>` over [`Self::scrollback`] and the screen last served.
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

    /// `offscreen [since=<i>] [tail=<n>] [screen=1]` over
    /// [`Self::alt_archive`] and the screen last served.
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

    /// [`Self::scrollback`] as it stands above the screen last served
    /// ([`Self::scrollback_from`]).
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

    /// THE HISTORY ABOVE THE SCREEN SERVED, as aterm's own hosts serve it,
    /// and empty where no test set one: `search` places the screen at row 0
    /// and `modes` says the main screen, so a read of what stands above the
    /// screen reaches the history's first row (a Land defense whose turn
    /// begins above the screen of a host with no history to read lands
    /// nothing: `Session::said_read_whole`).
    pub(super) fn history_served(&mut self) {
        self.scrollback.get_or_insert_with(Vec::new);
        self.alt_screen.get_or_insert(false);
    }
    /// The screen the loop is on sits until a `turn`, a `key` or the loop's badge releases it
    /// ([`Self::turn_releases`], [`Self::key_releases`] and the gates).
    fn held(&self) -> bool {
        self.turn_releases
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
    /// The next screen as `text --json` answers it; with `tail` (a
    /// `tail=<n>` read) a screen taller than `n` rows is served as its
    /// last `n` with the `first` row they start at, as the server does.
    fn screen_json(&mut self, tail: Option<usize>) -> String {
        let after_tail = std::mem::replace(&mut self.last_read_tail, tail.is_some());
        let i = if self.card_debounce.is_some() {
            self.reactive_index()
        } else if self.held()
            || tail.is_none() && after_tail && self.whole_sees_the_tail && self.served > 0
        {
            // The last screen is served again past the script's end.
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
        let rows: Vec<String> = self.screens[i][first..]
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
        let generation = if self.sends_gen {
            format!(",\"gen\":\"1.{}\"", self.seq)
        } else {
            String::new()
        };
        let cursor_row = if let Some(&row) = self.screen_cursors.get(&i) {
            row
        } else if let Some(row) = self.cursor_row {
            row
        } else if self.cursor_on_caret {
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
        // `dims.rows` is the pane's height, as the server sends it, whatever slice `tail=`
        // served.
        format!(
            "{{\"rows\":[{}],\"cursor\":{{\"row\":{cursor_row},\"col\":{composer_col},\"visible\":true,\"style\":\"block\"}},\"dims\":{{\"rows\":{},\"cols\":{}}},\"seq\":{}{generation}{human}{first}}}\n",
            rows.join(","),
            self.screens[i].len(),
            self.pane_cols.unwrap_or(120),
            self.seq
        )
    }
    /// The session goes at this sitting wait: a test's [`Self::exit_when`]
    /// holds, or [`Self::exit_at_request`] requests have been served.
    fn exiting(&self) -> bool {
        self.exit_when.is_some_and(|done| done(self))
            || self
                .exit_at_request
                .is_some_and(|n| self.requests.len() >= n)
    }
    /// Whether the wait `tail` (an `await` request's words after its
    /// verb) is the loop parked on the last screen ([`Self::end_when_parked`]).
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
    /// A wait on a screen that sits: `ERR exited` once `vanish_after`
    /// stalls have passed, else (after `stall_sleep`) `OK timeout`.
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
    /// The `key` requests, in order: a request whose verb (after any `@sid`) is `key`, never a
    /// badge or a text that names one.
    fn presses(&self) -> Vec<&String> {
        self.requests
            .iter()
            .filter(|r| verb_of(r) == "key")
            .collect()
    }
    /// The script screen the reactive card shows now ([`Self::card_debounce`]): a pending
    /// digit whose debounce has run is taken (or, dropped, left standing); the served
    /// screen moves the content seq only when it changes.
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
    /// The bookkeeping of a wait that ran out (`vanish_after`), for a reactive wait that
    /// already spent its time.
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
    /// A reactive wait: polls `done` in real time up to the request's `timeout`.
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
        // As a sitting screen's wait, bounded by [`Self::stall_sleep`], so a card that has
        // left does not hold the run for the loop's whole step.
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
    /// Every write to the session, in order, as the question answer
    /// makes them: a key by its name (`down`, `enter`), a `send` as
    /// `text:<its body>`.
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

/// The panel's world ([`PanelWorld`]): what it does at each request.
impl Mock {
    /// The world answers the waits: text stands typed, the panel is
    /// open, or the loop waits for a panel that never came.
    fn panel_holds(&self) -> bool {
        self.panel
            .as_ref()
            .is_some_and(|p| matches!(p.at, PanelAt::Typed(_) | PanelAt::Open) || p.quiet)
    }

    /// The model's `pan` of the screen the server holds, by the loop's
    /// own readers.
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

    /// A person's hand on the session, as the world knows it: they
    /// keyed within its grace, or `status` dates their last key inside
    /// it.
    fn panel_hand(&self) -> bool {
        let Some(p) = self.panel.as_ref() else {
            return false;
        };
        let grace = u64::try_from(p.grace.as_millis()).unwrap_or(u64::MAX);
        p.keyed_at
            .is_some_and(|at| run_since(self.waits.as_deref(), at) < p.grace)
            || self.human_ms.is_some_and(|ms| ms < grace)
    }

    /// A person keyed the session.
    fn panel_person(&mut self) {
        if let Some(p) = self.panel.as_mut() {
            p.keyed_at = Some(run_now(self.waits.as_deref()));
        }
    }

    /// A write of the probe's arrived: the world it arrived in.
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

    /// The screen the world holds becomes `rows`: the content moved.
    fn panel_show(&mut self, rows: Vec<String>) {
        let last = self.screens.len() - 1;
        if self.screens[last] != rows {
            self.screens[last] = rows;
            self.seq += 1;
        }
    }

    /// The script runs on with `screens` in place of the world's own.
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

    /// A `text` read while the panel is open: the next frame is on the
    /// screen before it is served — a person keying as it is, the panel
    /// leaving by itself. `true`: this read's answer never comes.
    fn panel_read(&mut self) -> bool {
        // A half-erased panel ([`PanelWorld::torn`]) is served to one read; the composer is
        // drawn by the next.
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

    /// A read was served: the world's screen, for the host to read when
    /// it is handed a sample ([`PanelShared`]).
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

    /// A wait while the world holds the screen ([`Self::panel_holds`]). A test's
    /// [`Self::exit_when`] ends the session here as at any sitting wait, so a world whose panel
    /// never closes still ends the run.
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
            // The screen held still that long (a nap of its window, at
            // this server's scale) — or never does, while it repaints.
            Some(&"idle") => (!(open && repaints)).then(|| {
                let still = tail
                    .get(1)
                    .and_then(|ms| ms.parse::<u64>().ok())
                    .map_or(Duration::ZERO, Duration::from_millis);
                idle_nap(self.waits.as_deref(), still, nap);
                ok("OK idle\n")
            }),
            _ => None,
        };
        latched.unwrap_or_else(|| {
            run_nap(self.waits.as_deref(), nap);
            timeout()
        })
    }

    /// A `send` the server wrote: its text stands in the composer.
    fn panel_sent(&mut self, body: &str) {
        let last = self.screens.len() - 1;
        let on_last = self.served >= self.screens.len();
        let composer = self.screens[last].clone();
        let Some(p) = self.panel.as_mut() else {
            return;
        };
        // The script ran on to a new point: the world rests there.
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

    /// A key the server wrote: Enter on what stands typed, an Escape at
    /// the open panel.
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
                    // The loop's own act, taken: the script runs on.
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
                    // The panel half erased first; the composer by the
                    // read after the next ([`Self::panel_read`]).
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

/// The limit reset's world ([`ResetWorld`]): what it does at each
/// request.
impl Mock {
    /// The world answers the waits: its dialog is the screen being
    /// served, or the dialog is gone and the host has not been told how
    /// its claim ended.
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

    /// A person's hand on the session, as the world knows it.
    fn reset_hand(&self) -> bool {
        let Some(w) = self.reset.as_ref() else {
            return false;
        };
        let grace = u64::try_from(w.grace.as_millis()).unwrap_or(u64::MAX);
        w.keyed_at
            .is_some_and(|at| run_since(self.waits.as_deref(), at) < w.grace)
            || self.human_ms.is_some_and(|ms| ms < grace)
    }

    /// A person keyed the session.
    fn reset_person(&mut self) {
        if let Some(w) = self.reset.as_mut() {
            w.keyed_at = Some(run_now(self.waits.as_deref()));
        }
    }

    /// The world as it stands, for the host's own steps
    /// ([`ResetShared`]).
    fn reset_seen(&mut self) {
        use std::sync::atomic::Ordering::SeqCst;
        let (hand, hold) = (self.reset_hand(), self.hold == "1");
        if let Some(w) = self.reset.as_ref() {
            w.shared.dlg.store(w.dlg(), SeqCst);
            w.shared.hand.store(hand, SeqCst);
            w.shared.hold.store(hold, SeqCst);
        }
    }

    /// The world's screen becomes what it shows where it stands.
    fn reset_show(&mut self) {
        let Some(w) = self.reset.as_mut() else {
            return;
        };
        let frame = w.frame();
        self.panel_show(frame);
        self.reset_seen();
    }

    /// A `text` read while the world's dialog stands: the pending lines
    /// run out, a person keys.
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
            // A person's own Enter on the yes.
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
            // This read is answered with the moved frame (`reset_show` runs before the screen
            // is served), so a move to Gone is a read of the composer; else an untold world
            // keeps its claim and holds the loop's next wait.
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

    /// A wait while the world holds the screen ([`Self::reset_holds`]).
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
                Some(ok("OK idle\n"))
            }
            _ => None,
        };
        latched.unwrap_or_else(|| {
            run_nap(self.waits.as_deref(), nap);
            timeout()
        })
    }

    /// A `key` request arrived: the world it arrived in.
    fn reset_probe(&mut self, request: &str, key: &str) {
        use std::sync::atomic::Ordering::SeqCst;
        let on_world = self.served >= self.screens.len();
        // A person keys just before a key of the loop's reaches the
        // confirmation: the count moves past every read so far.
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

    /// A key the server wrote: what the vendor's dialog does with it. `true`: the key's answer
    /// never comes ([`ResetWorld::lose_reply_to`]).
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
            // Inside its guard the confirmation refuses the key, and
            // re-arms on it.
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

/// A request's verb: its first word after any `@sid` selector.
fn verb_of(request: &str) -> &str {
    request
        .split_whitespace()
        .find(|w| !w.starts_with('@'))
        .unwrap_or("")
}

impl Mock {
    /// The built-in answers and the scripted queues (see the struct doc).
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
        if self.dead_after_turns.is_some_and(|n| self.turns > n) {
            return Ok(err("exited"));
        }
        if let Some(r) = self
            .verb_replies
            .get_mut(verb)
            .and_then(VecDeque::pop_front)
        {
            return Ok(r);
        }
        match verb {
            // The verdict line `turn` answers: submitted, settled.
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
                // The panel's world: the next frame, or a read whose
                // answer never comes. A `tail=` read is the loop back at
                // its ordinary reads: it waits for no panel any more.
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
                Ok(ok(&body))
            }
            // `modes`, as the transport renders the server's `OK <n>` and
            // its rows ([`super::super::transport::render`]): the rows.
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
            // A wait while the panel's world holds the screen
            // ([`PanelWorld`]): answered for the screen as it is.
            "await" if self.panel_holds() => Ok(self.panel_wait(tail)),
            // …and while the reset's world does ([`ResetWorld`]).
            "await" if self.reset_holds() => Ok(self.reset_wait(tail)),
            // THE SERVER'S DIRTY CHECK (`await seq <n> timeout 0`): whether
            // the content moved past `n`, this script's count, answered at
            // once as the server's `SeqAdvanced` watch answers it — no screen
            // served, no stall counted.
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
            // `await gone <re>`: latches at once when no row of the
            // screen matches, and — as `await seq` — while another
            // screen is still to come (the worker moved on); on the last
            // screen, with a row still matching, it stalls like `seq`.
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
                // `rows <a> <b>`: grid rows, inclusive, as the server
                // scans them; a span that meets no row is refused.
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
                if self.parked(tail) {
                    return Ok(err("exited"));
                }
                let stale = tail
                    .get(1)
                    .and_then(|n| n.parse::<u64>().ok())
                    .is_some_and(|n| n > self.seq);
                if self.held() {
                    // As a real wait on a screen that sits: not at once — the
                    // whole time asked, skipped on the run's clock
                    // ([`Self::waits`]), else a nap of it.
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
                Ok(self.stall(tail))
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
                Ok(ok("OK idle\n"))
            }
            "key" => {
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
                // The reset's world: every key is recorded as it
                // arrives.
                self.reset_probe(&line, tail.last().copied().unwrap_or(""));
                // The panel's world: an Escape is recorded as it
                // arrives, and one at the open panel may be refused.
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
                // A screen that ticks ([`Self::ticks`]): a fenced key a tick
                // after its read never reaches the program.
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
                // `act=<word>` (a host whose `help key` names it): what the
                // press means for the box; it fences nothing.
                if tail.first().is_some_and(|a| a.starts_with("act=")) {
                    tail = &tail[1..];
                }
                // `if-human=<n>` (a host that sends the count): a person
                // who keyed since the read skips the key.
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
                    // The take that was pending is due, or it is cancelled: any change to
                    // the composer before it runs cancels it (the vendor's hook).
                    self.reactive_index();
                    if matches!(self.digit, Digit::Pending { .. } | Digit::Standing) {
                        self.digit = Digit::None;
                    }
                }
                // Written: the panel's world takes the key, and the
                // reset's.
                self.panel_key(tail.last().copied().unwrap_or(""));
                if self.reset_key(tail.last().copied().unwrap_or("")) {
                    return Ok(closed());
                }
                Ok(ok(&format!("OK seq={seq}\n")))
            }
            // The explicit restart: its fences answer as scripted, else the act is made.
            "restart" => Ok(self
                .restart_replies
                .pop_front()
                .unwrap_or_else(|| ok("OK restarting pid 4242 cause=stall\n"))),
            "send" if self.gen_fence => {
                self.keyed = true;
                // The text after `--`, as the panel's world takes it.
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
                // `if-human=<n>` (a host that sends the count): a person
                // who keyed since the read skips the text.
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
                // Written: the panel's world echoes it in the composer.
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
            // A host without the verbs answers what an older build does.
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
            // The composer's column-2 cell: dim (the placeholder) unless
            // a test says the text there was typed.
            "cell" => Ok(ok(&format!(
                "OK %20 {} 111318 {}\n",
                self.cell_fg, self.cell_attrs
            ))),
            // The worker's NEEDS YOU: an entry of its own owner's, which
            // spends no scripted reply and moves no other entry.
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
                    // Keyed: `owner=supervisor` is the loop's entry; the
                    // bare form is a person's.
                    let keyed = tail.get(2) == Some(&"owner=supervisor");
                    // The server stores a keyed entry's `kind=` beside its
                    // text, never in it.
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
    /// [`Ctl::call`]'s answer to `args`.
    fn answer_call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
        // The supervisor claim: the loop's bookkeeping, outside every
        // script's indexes (see `claims`).
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
            // Asked, and cut short: a request the loop made, unserved.
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
        // The task record: the loop's bookkeeping too, outside the indexes —
        // and, like the claim, a request the worker was sent, which a
        // replay of the run's tape asks again (`tape::testing::assert_replays`).
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
    /// [`Ctl::call`] past the claim: the scripts' indexed requests.
    fn call_counted(&mut self, args: &[&str]) -> Result<CtlReply, String> {
        // A watch's start-up reconcile read (`meta` as the very first
        // request) is not counted: the indexes the scripts name run from
        // the loop's first wait, and only `down` answers it.
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

/// How many waits the loop begins once it has read the last scripted
/// screen before its host stops it ([`host_stops`]): it read the screen,
/// did all it does there, and is watching — it did not end on its own.
const WATCHING: usize = 3;
/// A loop that never reads the last scripted screen wakes its host after
/// this many waits: the run ends and the caller's assertions say what is
/// missing, where it would otherwise wait on its unbounded budget.
const UNREACHED: usize = 200;

/// The host's side of [`host_stops`] over a [`Mock`]: it wakes the host's
/// thread once the mock has served its last screen and the loop has
/// begun [`WATCHING`] waits since, and counts the waits the loop begins
/// with the stop already set — refusing any past the first.
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

/// [`Session::run_hosted`] over `m` (as `sid`, the approval ledger at `ledger`), stopped by
/// its host: another thread, woken by [`HostSide`] once the loop has read the last scripted
/// screen and begun [`WATCHING`] waits since. With `hand_over` the host raises the hand-over
/// flag first. Returns what `run_hosted` returned and the lines it printed.
///
/// The stop waits on that, never on a clock. Promptness is counted, not timed: every wait
/// goes through [`Session::call`], which refuses one once the stop is set, so the flag can
/// slip in before one wait, never two; a second is refused here too, so a loop past its stop
/// fails instead of waiting on. [`stop_in_every_wait`] sets the stop in every wait of a path
/// in turn.
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
/// [`host_stops`], and the run's tape as it was replayed (its real header
/// in place): what a counterfactual's test replays again under another
/// policy.
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
    // Recorded, as every whole run here ([`taped_session`]): the host's
    // thread raises the stop while the loop waits, and the tape holds the
    // loop's own read that first saw it — and replayed (below): a stop raised
    // by another thread at a moment of its own replays exactly (the replay
    // plan's model 3, on the real hosted loop).
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
    // A loop that ended without waking its host leaves it nothing to
    // wait for.
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
/// The header a hosted run over `s` with `opts` records under, the memory
/// tape's id: whether it had a window host is its presence only — a run
/// that had one is replayed with the replay's host in its place
/// (`tape::host::ReplayHost`), its every answer the tape's.
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

/// The host's stop set as the loop's `arm_at`-th wait goes out (past every
/// check the loop makes before it), and every request that reaches the
/// transport after it ([`stop_in_every_wait`]).
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

/// More waits than any script here begins before it ends on its own.
const SWEEP_CAP: usize = 80;

/// The stop in every wait: [`Session::run_hosted`] over the script `script` builds (as `sid`,
/// a temp approval ledger), once per wait the run begins, the stop set as the k-th wait goes
/// out ([`StopInWait`]), for k = 1, 2, … until a run ends on its own first (the script must
/// end with a vanishing session). In every run no other wait or input reaches the transport
/// after the stop, and the run ends `Ok`, `EXIT stopped`. One thread, the stop set by the
/// transport. Returns the waits the stop was set in, in order.
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
        // THE STOP ON THE TAPE: the transport sets it inside the k-th wait,
        // and the record of its first true read is the loop's own, after
        // that wait's round trip — never the setter's, never before it.
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
            // The run ended on its own before its k-th wait: the stop has
            // been set in every wait it begins.
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

/// A current test host carrying all stamps an option-choosing digit needs.
/// A test of a missing/unsupported stamp must remove it explicitly.
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

/// The guarded press on `bash_one_row`'s box: its command row, anchored.
const G_PRESS: &str = "key if=^\\x20{3}git\\x20log\\x20--oneline\\x20-5\\s*$ 1";

fn busy_screen() -> Vec<String> {
    let mut r = rows(&["⏺ Working.", "", "✻ Synthesizing… (18s)", ""]);
    r.extend(composer("  esc to interrupt"));
    r
}

/// A row that exhausts the matcher's step budget is undecided, and `busy_up` reads "not shown
/// gone" as up, as `await gone` does. Controls: a readable match is up, a readable clear row is
/// not.
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

/// EACH AGENT'S BUSY GUARD, COMPILED BY THE SERVER'S OWN MATCHER (`await
/// gone`, [`Session::busy_up`]), IS UP ON ITS MEASURED BUSY SCREENS AND ON
/// NOTHING ELSE: agy's spinner row, a regex aterm-phase compiles nowhere,
/// Muse Code's status row — a request it retries included (1.4.10) — and
/// Gemini CLI's spinner one column in, thinking, streaming and through a
/// 503's and a 500's retries. Muse Code's approval box keeps its status row
/// (`◆ Calling tools (…)`) above the frame, so the guard stays up while the
/// box waits; its question's head takes the status row's place, so it is
/// down there. NEGATIVE CONTROLS: each is down on its program's idle
/// composer, its draft and its end of turn (Muse Code's walls and interrupt
/// too), agy's on a box and a menu, whose footer says `esc to cancel` with no
/// turn running, and Gemini CLI's on its boxes, its menu and its dialogs.
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
/// A test's own approvals ledger (`<scratch>/s-1.jsonl`), kept by the loop it
/// is given to ([`Book::keep`]): what the loop decided, box by box, read back
/// as `<decision> <command>` rows ([`Book::decided`]), and why it handed a
/// box over ([`Book::escalated`]). Its scratch dir goes with it.
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

    /// `s` keeps its approval ledger here.
    fn keep<C: Ctl>(&self, s: &mut Session<'_, C>) {
        s.set_approval_ledger(Some(self.path.clone()));
    }

    /// Every row, in order, as `(decision, command, reason)` — but the
    /// loop's own record that a turn was read running
    /// ([`crate::supervise::approvals::RULE_TURN_RAN`]), which decides no box.
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

    /// Every row, in order, as `<decision> <command>`.
    fn decided(&self) -> Vec<String> {
        self.rows()
            .into_iter()
            .map(|(decision, command, _)| format!("{decision} {command}"))
            .collect()
    }

    /// The reasons of the rows that handed a box over, in order.
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
        // The hosted loop's: with no indicator on a screen, nothing moves.
        context_warn: 10,
        journal: None,
        ledger: None,
        // The safe approval rules alone (`rm_breaker_ask`: an rm breaker they cannot prove
        // is a person's) and every turn-end act off ([`resuming`] turns a usage limit's resume
        // on): these tests pin the approval loop, and a turn's end types nothing here. A trust
        // root covers the captured trust dialog's folder, as a person names one (the default
        // has none).
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

/// The owner's policy as the loop's scripted screens run it: every default,
/// the continuation the short `keep going` those screens draw and guard on
/// one row. The default's own words (`config::CONTINUE_TEXT`, Cruise's long
/// instruction) are the turn-end policy's tests'; these pin the loop.
fn owners_policy() -> SupervisorConfig {
    SupervisorConfig {
        continue_text: "keep going".to_string(),
        ..SupervisorConfig::default()
    }
}

/// Every power the owner's policy has, taken away (each key at its limit): the loop reads and
/// reports, and answers nothing. The autopilot is left alone: at its limit (`off`) the loop
/// parks and reports nothing (`an_autopilot_off_parks_the_loop_until_the_word_changes`).
fn powerless() -> SupervisorConfig {
    let mut p = SupervisorConfig::default();
    for key in super::super::config::KEYS {
        if *key != "autopilot" {
            p.limit(key);
        }
    }
    p
}

/// `opts` with the session survey dismissed, or not.
fn surveys(mut opts: SuperviseOpts, on: bool) -> SuperviseOpts {
    opts.policy.dismiss_surveys = on;
    opts
}

/// `opts` with a usage limit resumed after its reset, the standing rules
/// read from `rules` with the continuation.
fn resuming(opts: &SuperviseOpts, rules: Option<PathBuf>) -> SuperviseOpts {
    let mut o = opts.clone();
    o.policy.resume_limits = true;
    o.policy.rules_file = rules;
    o
}

/// After `/exit` and a fresh `claude`, the dead process's `esc to interrupt` footer stays
/// above the new launch card: the footer wait is armed over the live zone only (`rows 33
/// 49`), so the dialog latches it; only the first, unscoped wait runs out ([`UNSCOPED_CAP`]).
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

/// The second look at a box too young to press (`Step::Again { settle: false }`) is armed
/// from the box's first row down, so a dialog under a dead footer is read again at once.
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

/// The rows above a `tail=40` read were never seen, so the wait is scoped to the tail's rows
/// (`rows 33 72` on a 73-row pane) even where the live zone starts at its first row. Negative
/// control: a turn still running runs it out.
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
    // Negative control: a live footer in the span still holds it.
    let m = run(vec![busy.clone(), busy, dialog]);
    assert_eq!(
        m.requests[2],
        "await gone esc.to.interrupt rows 33 72 timeout 20000"
    );
    assert_eq!(m.gone_held, 2, "{:#?}", m.requests);
}

/// `ERR bad rows` (a resize shrank the grid under the span) ends the wait as a latch does and
/// the next read arms it again; it never ends the loop.
#[test]
fn a_span_the_grid_no_longer_has_is_read_again() {
    use aterm_phase::prompt::fixtures as f;
    let busy = f::screen(f::EXIT_RELAUNCH_STALE_FOOTER_BUSY);
    // The relaunched session idle in a pane 30 rows tall: its foot.
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

/// A READ TORN MID-REPAINT IS NO POINT (`r7-both` L6, Claude Code 2.1.307,
/// 2026-10-22). A reply growing under the window is repainted row by row,
/// its footer last; the busy footer's `await gone` latched at a batch between
/// the bottom rule (drawn 109 of 144 columns, on the grid's last row) and the
/// footer row, and the read there showed the composer frame and no busy
/// footer: idle to every reader, `- check 65 passed` of 80 judged a point,
/// and the reply's `NEEDS YOU:` above the window never read. An idle read
/// straight after that latch whose frame is not whole — its bottom rule cut
/// short, or on the last row with no footer under it — is read again after
/// the short settle, and that read decides: here the reply still streaming
/// (the footer back), then its end. NEGATIVE CONTROL: a whole frame read
/// after the latch is the point at once, with no settle.
#[test]
fn an_idle_read_torn_mid_repaint_is_read_again_after_the_settle() {
    let footer = "  ⏵⏵ auto mode on (shift+tab to cycle)";
    // The reply streaming, its rows from `first` to `last` in sight (a read
    // is the window's tail).
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
    // The frame cut where a repaint stood, with no footer row under it: the
    // live read's bottom rule part drawn, and the rule whole with the footer
    // still to come.
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
    // NEGATIVE CONTROL: no tear, the footer leaving at the end.
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

/// A READ STILL OPEN AFTER THE REREADS IS OUTPUT ARRIVING, never a point (R1 of the half-drawn
/// frame review, 2026-10-22). A program that opens a new DEC 2026 frame before each read lands
/// can leave every one of [`FRAME_REREADS`] rereads inside one, and the whole-frame read then
/// hands back the last as it stands with the server's word that its frame was open
/// ([`Session::whole_frame`]): the loop takes that for a redraw caught midway (`writing`) and
/// reads again, so the reply's end decides. The screen is whole in its geometry (no tear for
/// [`aterm_phase::ScreenReader::frame_torn`] to catch) and idle to every reader. NEGATIVE
/// CONTROL: the same screen read whole (the server's frame closed) is the point at once.
/// MUTANT: `writing` without `screen.frame_open` judges the open read idle.
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
    // NEGATIVE CONTROL: the same screen, its frame closed, is the point.
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

/// A transport that answers every request with one reply and records
/// what it was sent: the session log's `act` marks, alone.
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

/// THE ACT MARKS' LATCH: a server that refuses the verb (an
/// older build's `ERR unknown verb`) or the mark itself (`ERR usage: mark`,
/// `ERR mark:`) is sent ONE mark, and the loop sends no other after it,
/// queued then or later; `ERR busy retry_ms=` drops that mark alone, never
/// retried. Each request is exactly `mark - act word=<w> [rule=<id>]` and
/// no other verb is ever sent for one.
#[test]
fn a_refused_act_mark_latches_the_rest_off() {
    let first = "@s-1 mark - act word=approve rule=read-only.classify-v5";
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
            .queue("act word=approve rule=read-only.classify-v5".to_string());
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
    // NEGATIVE CONTROL: a server that takes them is sent each, once.
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

/// A read-only Bash prompt is approved with one guarded press (the fence asked of `help key`
/// once) and noted; the box must leave (`await seq`, then a read) before the short settle;
/// the write prompt after it is handed over.
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
            "@s-9 mark - act word=approve rule=read-only.classify-v5",
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

/// `OK skipped seq=<n>` is an answer, not an approval: nothing was
/// pressed, so nothing is approved — the press is booked skipped, and the
/// loop looks again.
#[test]
fn a_skipped_guard_is_not_an_approval() {
    let book = Book::new("skipped");
    let mut m = fenced(vec![bash_one_row(), idle_screen()]);
    // The box resolved between the read (seq 101) and the check (seq 105).
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

/// `OK skipped` at the read's own seq (no row of the parsed box matched) hands the box over.
#[test]
fn a_guard_that_matches_no_row_of_the_parsed_box_is_handed_over() {
    let mut m = fenced(vec![bash_one_row()]);
    // The read is seq 101; the guard found no row on that very screen.
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

/// The other `ERR`s a host can answer the guard with are never an
/// approval. `busy sink` is never re-sent as it was: the loop backs off,
/// reads and decides again, and presses the box still up after that
/// fresh read; `no such session` and `badregex` surface as errors with
/// nothing approved.
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

/// Under `approve = "none"` even a read-only prompt is the manager's: no key
/// is sent.
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

/// `await gone` is level-triggered and a box shows no busy footer, so the loop waits for the
/// content to move past a press before it reads again; a screen that sits is pressed once and
/// handed over.
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
            "mark - act word=approve rule=read-only.classify-v5",
        ],
        "{:?}",
        m.requests
    );
    // Approved once, and then handed over (`out` is the box's point).
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

/// An `await` timeout (exit 124) is a step: the read that follows decides.
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

/// The reply readers the guard depends on, against the measured wire.
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

/// A plain question the worker asked: the policy answers it with
/// `answer_text`, and escalates it under `answer_questions = false`. Not a
/// choice between options, whose reason differs.
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
/// `body` over an idle composer frame.
fn framed(body: &[&str]) -> Vec<String> {
    let mut r = rows(body);
    r.extend(composer("  ? for shortcuts"));
    r
}
/// A Bash box for `command` under `transcript`.
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
/// The session a whole-run helper hands its setup: over the Mock through a
/// recording door ([`taped_session`]).
type Watched<'s, 'm> = Session<'s, TapedCtl<'m, Mock>>;
/// A session over `m` whose door RECORDS — what `--tape FILE` runs —
/// into memory: the whole-run helpers below run every test with the recorder
/// on, so each test's own expectations (its exact lines, its exact
/// requests) are the recorder's transparency check, and the nesting rule's
/// assertion holds across every path they reach. The door's clock is the
/// fixed one the tests stage ([`Session::set_clock`] reaches it through the
/// door).
fn taped_session<'m, 't>(
    taped: &'t mut TapedCtl<'m, Mock>,
    tape: &MemoryTape,
) -> Session<'t, TapedCtl<'m, Mock>> {
    let mut s = session(taped, None);
    s.set_fx(tape.fx.clone());
    s
}
/// What a request costs on a run's clock that moves by its steps alone ([`Mock::round_trip`]).
const ROUND_TRIP: Duration = Duration::from_millis(1);
/// A clock not yet fixed that moves by the run's steps alone
/// ([`super::super::fx::ScriptedClock::run_virtual`]).
fn virtual_clock() -> Arc<super::super::fx::ScriptedClock> {
    let clock = Arc::new(super::super::fx::ScriptedClock::default());
    clock.run_virtual();
    clock
}
/// [`session`] over `m` on a clock of its own that moves by the run's steps alone
/// ([`virtual_clock`]), `m` on it ([`Mock::run_on`]): a back-off, a settle or a re-read's
/// step the loop sleeps through its door is skipped, at the same step however loaded the
/// machine.
fn virtual_session(m: &mut Mock, sid: Option<String>) -> Session<'_, Mock> {
    let clock = virtual_clock();
    m.run_on(&clock);
    let mut s = session(m, sid);
    s.set_fx(Fx::over(ClockSource::Scripted(clock)));
    s
}
impl Mock {
    /// Serve a run on `clock`: its waits on a sitting screen skip on it ([`Self::waits`]); on
    /// one that moves by the run's steps alone, a request costs [`ROUND_TRIP`] and a minute of
    /// real time is the run's hang detector.
    fn run_on(&mut self, clock: &Arc<super::super::fx::ScriptedClock>) {
        let steps = clock.is_virtual();
        self.waits = Some(Arc::clone(clock));
        self.hang_at = steps.then(|| Instant::now() + aterm_spec::wait::MUST_HAPPEN);
        self.round_trip = if steps { ROUND_TRIP } else { Duration::ZERO };
    }
}
/// A memory tape over a fixed clock not yet fixed that moves by the run's steps alone
/// ([`virtual_clock`]), and the transport `m` through its door.
fn memory_tape(m: &mut Mock) -> (MemoryTape, TapedCtl<'_, Mock>) {
    memory_tape_on(m, virtual_clock())
}
/// [`memory_tape`] over `clock` ([`Mock::run_on`]).
fn memory_tape_on(
    m: &mut Mock,
    clock: Arc<super::super::fx::ScriptedClock>,
) -> (MemoryTape, TapedCtl<'_, Mock>) {
    m.run_on(&clock);
    let tape = MemoryTape::over(&Fx::over(ClockSource::Scripted(clock)));
    let taped = TapedCtl::new(m, tape.fx.clone(), crate::supervise::tape::doors::CTL);
    (tape, taped)
}
/// [`watch_lines`] with the session set up first (the manager, a clock). Every watch test's
/// session keeps the same fixed clock ([`TEST_NOW`]) and zone table, so a limit notice's
/// reset lands where the test expects whatever the wall clock says. The
/// run is recorded ([`taped_session`]) and its tape must read back whole —
/// and REPLAY: from the tape alone, under the header the run's setup and
/// options make, with no transport, asking exactly what the worker was sent
/// and printing exactly what the run printed, twice
/// ([`crate::supervise::tape::testing::assert_replays`]).
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
/// A `watch` recorded over the scripted worker, as the whole-run helpers
/// record one: its tape, the header it records under, its lines and exit
/// code, and why it cannot be replayed if its setup seeded learned state.
struct RecordedWatch {
    tape: MemoryTape,
    header: crate::supervise::tape::header::Header,
    lines: Vec<String>,
    code: u8,
    unreplayable: Option<&'static str>,
    /// The requests the scripted worker was sent by this run ([`sent`]).
    sent: Vec<String>,
}
/// Record a `watch` over `m` with `opts`, the session set up as
/// [`watch_lines_with`] sets it up (the fixed clock, the zone table, then
/// `setup`), its tape rooted where its header roots it.
fn record_watch(
    m: &mut Mock,
    opts: &SuperviseOpts,
    setup: impl FnOnce(&mut Session<'_, TapedCtl<'_, Mock>>),
) -> RecordedWatch {
    record_watch_on(m, opts, virtual_clock(), setup)
}
/// [`record_watch`] on `clock`: a stand-in host the test made reads the run's own clock.
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
/// The header a `watch` over `s` with `opts` records under (as `aterm drive
/// watch --tape` captures it), the memory tape's id: what a replay rebuilds
/// the loop from.
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
/// Every request the scripted worker was sent from its `from`-th on, in
/// order, the claim's included: what a replay of the run that began there
/// must ask (a worker a test runs a second loop over holds the first's too).
fn sent(m: &Mock, from: usize) -> Vec<String> {
    m.order[from.min(m.order.len())..]
        .iter()
        .map(|(r, _)| r.clone())
        .collect()
}
/// The watch tests' clock: 2026-09-17T08:55:00-07:00.
const TEST_NOW: i64 = 1_789_660_500;
/// 2026-09-19T18:00:30Z: half a minute after the weekly reset.
const AFTER_RESET: i64 = 1_789_840_830;
const PDT: i64 = -7 * 3600;
fn test_zones(zone: &str, _unix: i64) -> Option<i64> {
    match zone {
        "America/Los_Angeles" => Some(PDT),
        "UTC" => Some(0),
        _ => None,
    }
}
/// The EVENT and APPROVED lines, without the seq (which counts reads).
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

/// The reader for a turn read with no session: the one its screen names
/// ([`aterm_phase::identify`]).
fn by_screen(turn: &Turn) -> &'static dyn ScreenReader {
    aterm_phase::identify(None, &turn.screen.rows)
}

/// The Esc line `aterm drive phase` prints under the box on `rows`, the session's program
/// `program` (`None`: no line).
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

/// UNDER A TRUST DIALOG WHOSE ESC QUITS THE PROGRAM, `aterm drive phase` SAYS SO: `exit esc`,
/// never `cancel esc`, which reads as Esc leaving the box. Claude Code's (`Esc to cancel`,
/// whose cancel handler exits), Muse Code's (`Esc quits.`), and the two that draw no Esc at all: Gemini CLI's (its bundle's
/// `FolderTrustDialog` exits on `escape`; the review of 2026-10-16 found `cancel esc` printed
/// there) and agy's (Esc quit agy 1.4.0 in the lab, 2026-10-16). agy's terms screen, which
/// names no Esc either, says `cancel esc`: its Esc goes back to the colour scheme, as its
/// `[Previous]` does. NEGATIVE CONTROLS: a box of each whose Esc refuses keeps `cancel esc`;
/// Gemini CLI's per-day quota dialog and agy's colour-scheme screen, which name no Esc and
/// where Esc changed nothing in the lab, print no Esc line rather than a guess.
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

/// A BUSY ROW THAT STAYS OVER A BOX is waited on two seconds at most: Muse
/// Code's `◆ Calling tools (…)` stays above its approval box (measured on
/// 1.4.10), so the row leaving cannot say a box was drawn, and every wait on
/// it — before any read and over the live zone after one — is capped
/// (`timeout 2000`) and the box is read on the next look. NEGATIVE CONTROL:
/// Claude Code's busy row, which leaves for its box, is waited on for the
/// whole step (`timeout 20000`) once a read scoped it.
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

/// THE LOOP WAITS IN ITS SETUP'S STEP (`SessionSetup::wait_step`), read once
/// as the setup was made, never in this process's live settle: each step is
/// the `timeout` of an `await` the tape records, and a replay in a process
/// with another settle seam must ask the recorded run's. A step of 7 s (this
/// process's is 20 s): the turn's wait for the busy row to leave asks
/// `timeout 7000` — and the run REPLAYS from its tape alone
/// ([`watch_lines_with`]), its header carrying the step, where a loop that
/// read the step live would ask the replaying process's `timeout 20000`.
/// NEGATIVE CONTROL: the same run under the shipped setup asks the 20 s step.
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

/// Note rows (a `│`-led warning, a bare ` This command requires approval`) print as `note
/// <text>` lines between `description` and `classify`; the EVENT summary carries none.
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
    // The bare note row too.
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

/// `aterm drive phase` names agy's and Muse Code's OWN BACKGROUND WORK as
/// the reason a turn's end reads busy (their readers' `background_wait`):
/// `reason its own background work: a background task running` under agy's
/// panel, `… background work running` under Muse Code's. NEGATIVE CONTROLS:
/// a turn running over agy's panel keeps its spinner's reason, and the same
/// sessions with their work over read idle, with no reason line.
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

/// The EVENT summary is the worker's last row, never the session survey
/// Claude Code parks under the done row (the saved wait_bg7 screen). A
/// workflow launch, which has no command, is named by its description.
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

/// THE EVENT SUMMARY IS THE WORKER'S WHOLE LAST LINE, its wrapped rows
/// joined, for every program whose reader reads its words: in a 50-column
/// lab (the `*-lab-reply-wrapped-50col` captures) the harness journal said
/// only the reply's last row (`EVENT question seq=344 every caller now. Which
/// do you prefer (1 or 2)?` for Claude Code, Gemini CLI and Muse Code,
/// `or 2)?` for agy). Now each reads as it does in a pane the line fits in. A
/// line longer than a summary keeps its END, `…` before it: an eight-row
/// paragraph at 120 columns ends on what its last row said. NEGATIVE CONTROLS: with no width
/// read (a host that sends none) the summary is the last row, as before; a
/// paragraph above the last one, a blank row between, is not joined to it.
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
    // An eight-row paragraph at 120 columns, wrapped at the pane's edge
    // under its `⏺` head, every row two columns in.
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
    // The manager is right there: no escalation, no probe.
    assert!(
        !m.requests
            .iter()
            .any(|r| r.starts_with("meta") || r.starts_with("turn")),
        "{:?}",
        m.requests
    );
}

/// `await idle 2000` waits through a short busy spell, so no read sees it: a footer tick is no
/// new point, the same last row after a real turn (new rows above it) is.
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

/// Points that are each new after a busy spell no read saw, with no box to
/// key them (a box's key: `run_box_key_tests.rs`): a plain shell back at its
/// `$` after another command; a retry that hit the limit wall again.
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
            // A shell has no busy footer: its first read is taken before
            // the output settled, and the settled read is its first point.
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
        // The limit case's budget extension (`EXTEND …`) has tests of
        // its own; here only the review points count.
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

/// A frameless worker (a build) whose output never held still for the idle window is busy:
/// no review point until it pauses.
#[test]
fn a_frameless_screen_still_changing_is_busy_until_it_settles() {
    let mut m = Mock::new(vec![
        rows(&["$ make", "compiling a"]),
        rows(&["$ make", "compiling a", "compiling b", "done", "$"]),
    ]);
    // `await gone` latches at once (no busy footer on a shell); the first
    // `await idle` runs out its step: the output never paused.
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

/// The budget bounds every action: at or after the deadline the result is TIMEOUT with the
/// last read's lines (no EVENT, no press), also for a pressed box that never left.
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

    // A press in the budget, then the wait for its box to leave runs out
    // with it: the approval stands, and the box that did not move is the
    // TIMEOUT, not an EVENT printed after the deadline.
    let mut m = fenced(vec![bash_one_row()]);
    // 0 the wait, 1 the read, 2 `status`, 3 the cwd `meta` a git read is
    // judged in, 4 `help key`, 5 the press, 6 the wait for the box to leave.
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
    // The start-up `meta`, then the seven (the box's `status` and cwd
    // `meta` among them); nothing read after, only the session log's mark
    // of the press the budget held (sent once its look returned).
    assert_eq!(m.requests.len(), 9, "nothing read after: {:?}", m.requests);
    assert_eq!(
        m.requests.last().map(String::as_str),
        Some("mark - act word=approve rule=read-only.classify-v5")
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

// ---- the session survey ----------------------------------------------

/// Claude Code's session survey, as it parks above the composer.
const SURVEY_ROWS: [&str; 2] = [
    "● How is Claude doing this session? (optional)",
    "  1: Bad    2: Fine   3: Good   0: Dismiss",
];

/// `r` with the survey parked right above its composer's top rule.
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

/// A survey on an idle screen is said once (`EVENT survey … dismiss with: …`), ahead of the
/// point, and never pressed.
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

/// `dismiss_surveys`: the survey that appears gets one guarded `0` (`key
/// if=^●.How.is.Claude.doing 0`, no other key) and the loop looks again
/// from a fresh read: the survey gone, the `0` is said as `DISMISSED
/// survey seq=<n>` (the press's seq), and the point under it reported. `supervise` says the line on stderr, never in its result.
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

/// AGY'S FEEDBACK SURVEY IS DISMISSED AS CLAUDE CODE'S RATING CARDS ARE (the
/// lab's capture, agy 1.4.0): drawn in its composer's place, `phase` names
/// its `0`, and under `dismiss_surveys` the loop presses that `0` and no
/// other key, guarded on the card's options row — the guard the server's own
/// matcher compiles, up on that row alone — then says `DISMISSED`. No digit
/// is typed as a turn while it is up. NEGATIVE CONTROL: with
/// `dismiss_surveys` off nothing is pressed, and the line says the guarded
/// `0` for a manager to press.
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

    // The card holds still (no composer: the loop settles on it first), then
    // the `0` takes it and the composer is back.
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

/// A guarded `0` that finds no survey row — it left between the read and
/// the check, `OK skipped seq=<n>` — pressed nothing: no DISMISSED line,
/// and the loop looks again (where the survey is gone).
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

/// A survey on a screen with a box waits for the box: the read is approved
/// first, and the survey is dismissed once the box has gone. (No `0` under a
/// box: the engine's `tier1_the_cards_0_never_goes_over_a_draft_or_a_box`.)
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

/// The survey dismissed, on the defaults of the other survey tests.
fn dismissing() -> SuperviseOpts {
    surveys(auto(30), true)
}

/// A host that only hears about the surveys the loop cannot close
/// ([`IdleHost::survey_stuck`]), as `(kind, sentence)` and `None` at the end.
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

/// A host that wants no point now and knows when it will
/// ([`IdleHost::wants_at`]).
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

/// The longest idle-point step is the time to the host's scheduled look, not [`WAIT_STEP`].
/// Negative control: a host with no look keeps the step.
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

/// A host whose one look comes due at `at` on the run's `clock`: it wants a point from then
/// until it takes one, and records that it did.
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

/// A look 300 ms away on an idle screen is taken at its instant and decides no turn end: what
/// the loop types and sends is exactly what it does with no look.
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

/// `opts` with a host that records what it is told about a survey.
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

/// No tab is ever marked for a survey: the loop writes no attention for it.
fn assert_no_attention(m: &Mock) {
    assert!(
        !m.requests
            .iter()
            .any(|r| r.starts_with("meta set attention")),
        "a survey marks no tab: {:#?}",
        m.requests
    );
}

/// The guard is judged against every row (the server's `row_matcher`), so it matches the open
/// survey's question row and no copy of it: a tool's output, the worker quoting it, an EVENT
/// line or `--help`.
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
    // And the mock judges it as the server does: the copy skips.
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

// ---- every kind of rating card, on busy reads, with the vendor's settle ----

/// The rating cards Claude Code 2.1.284 draws besides the session survey, as they park over
/// the composer (hand-built from its render code: only the session survey has a capture).
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
/// The Claude-drafted-feedback card: no survey, never pressed.
const DRAFTS_ROWS: [&str; 5] = [
    "✻ Bug report drafted: the stand-in title of a queued draft",
    "│ details preview line one",
    "│ details preview line two",
    "1 to review · 2 to send · 0 to dismiss",
    "",
];

/// `r` with `card` parked right above its composer's top rule.
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

/// A person's grace holds the `0` on busy reads too and is said once per spell; another
/// driver's hand (a lease) holds it the same way and is named as such.
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

/// A card with no known words and no `(optional)` is read through but not pressed: said as
/// `EVENT survey … kind=unknown not pressed (…)` with no command to run (its reader would have
/// to improvise a guard), and, unattended, told to the host (a record, never a tab mark), since
/// nothing else acts while a survey is open.
#[test]
fn a_withheld_card_first_read_busy_is_told_at_the_idle_look_and_ended_when_it_goes() {
    let card = [
        "● How likely are you to recommend this mode?",
        "  1: Bad    2: Fine   3: Good   0: Dismiss",
    ];
    // The busy read says the card and tells nothing (the worker is at work); the idle
    // look after it, with the card still open, tells the host, once; the card leaving
    // ends it.
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

/// A card nobody has words for and without the vendor's `(optional)` is said
/// (`EVENT survey … kind=unknown not pressed`, no command to run) and never
/// pressed; so is one that bears the mark but whose question row no guard can
/// fence. Unattended, the host is told once, and no tab is marked (the host's
/// side: `a_withheld_card_first_read_busy_is_told_at_the_idle_look_and_ended_when_it_goes`).
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
    // A card that bears the mark but whose question row no guard can fence (an ASCII
    // apostrophe in its first words; the printed command is single-quoted) is withheld the
    // same way.
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
    // A watching loop (no manager) is unattended: the host is told, once.
    assert_no_attention(&m);
    assert_eq!(
        told.0.lock().unwrap().iter().flatten().count(),
        1,
        "watch is unattended: {:#?}",
        told.0.lock().unwrap()
    );
}

/// A loop that sees no card says so once (the host may hold one a predecessor told), never
/// while a card is open.
#[test]
fn a_loop_that_sees_no_survey_tells_the_host_once() {
    let mut m = Mock::new(vec![idle_screen(), idle_screen(), idle_screen()]);
    m.vanish_after = Some(0);
    let (opts, told) = told_surveys(dismissing());
    let _ = watch_lines(&mut m, &opts);
    let told = told.0.lock().unwrap().clone();
    assert_eq!(told, [None], "once, with nothing to end: {told:?}");
}

/// The drafts card is no survey: never pressed (its `0` leads to a "turn off"
/// question whose second `0` writes a setting), never said as `EVENT survey`.
#[test]
fn the_drafts_card_is_never_pressed_or_called_a_survey() {
    let mut m = Mock::new(vec![with_card(idle_screen(), &DRAFTS_ROWS)]);
    m.vanish_after = Some(0);
    let (lines, _) = watch_lines(&mut m, &dismissing());
    assert!(m.presses().is_empty(), "{:?}", m.requests);
    assert!(!lines.iter().any(|l| l.contains("survey")), "{lines:?}");
    assert!(!survey_open(&with_card(idle_screen(), &DRAFTS_ROWS)));
}

// ---- the 0 in flight, the refused backspace, the card nobody presses, the
// workflow wait ---------------------------------------------------------------

/// A worker busy by its status row alone (a dynamic workflow it waits on, no `esc to
/// interrupt` footer): the one screen where the reader's stepping over a parked card is what
/// keeps a busy worker busy (under [`busy_screen`]'s footer a missed card changes nothing).
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

/// `r` with a lone `0` typed in its composer: the digit a card dropped, or one it has not
/// taken yet.
fn with_zero(mut r: Vec<String>) -> Vec<String> {
    let c = r.iter().position(|row| row == "❯").expect("composer");
    r[c] = "❯ 0".to_string();
    r
}

/// Every rating card over a workflow wait is dismissed on the busy read that shows it, and the
/// worker stays busy throughout.
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

/// Against a reactive card ([`Mock::card_debounce`]), a vendor timer as late as the landing
/// time allows still takes the one `0` and nothing is backspaced (test scale: landing 400 ms,
/// debounce 40 and 220 ms; shipped 2000 to 400). The idle case is
/// `tier1_the_cards_taken_0_is_never_backspaced_under_its_debounce`.
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

/// From busy reads a card is pressed once per appearance (an unarmed hook costs one `0`); the
/// retry ladder is the idle look's. Negative control: the same script read idle tries again.
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

/// A refused backspace keeps the `0` owed (a forgotten one would sit in the person's next
/// message, or be submitted): the loop parks or backs off as after any refused press and
/// removes it once the server takes the key.
#[test]
fn a_refused_backspace_keeps_the_0_owed_until_the_server_takes_it() {
    // (`busy input-unread`: the engine's
    // `tier1_the_cards_dropped_0_is_backspaced_where_the_model_allows`.)
    for (refusal, hold) in [("halted reason=owner origin=local", true), ("rate", false)] {
        let mut m = Mock::new(vec![
            with_survey(idle_screen()),
            with_zero(with_survey(idle_screen())),
            with_zero(with_survey(idle_screen())),
            with_zero(with_survey(idle_screen())),
            idle_screen(),
        ]);
        // The press is taken; the first backspace is refused; the next is taken.
        m.key_replies.push_back(ok("OK seq=101\n"));
        m.key_replies.push_back(err(refusal));
        if hold {
            m.hold = "1";
            m.hold_lifts_after = Some(1);
        }
        m.vanish_after = Some(0);
        let (lines, _) = watch_lines(&mut m, &dismissing());
        let backspaces = m.requests.iter().filter(|r| *r == "key backspace").count();
        // "busy sink" is retried inside the helper (three quick tries) before it is a
        // refusal: the refused reply is consumed by the first, the second is taken. Once
        // taken, the `0` is removed: no backspace follows it.
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

/// A `0` that did not take is retried on [`SURVEY_RETRY`] until one takes, never handed over
/// for good (no turn-end act runs while a survey is open), the host told from the second
/// failure (no tab marked). Negative control: `dismiss_surveys = false`.
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
    // Told from the second failure, and ended with the survey: no tab is marked.
    assert_no_attention(&m);
    let told = told.0.lock().unwrap().clone();
    assert!(
        matches!(&told[..], [Some((StuckSurvey::Unclosed, why)), None]
            if why.starts_with("the session survey did not close")),
        "told from the second failure, ended when it went: {told:?}"
    );
    // The `0` that did not take is accounted for at the look that finds the card still open
    // without it: a lone `0` read after the card has gone is someone else's, never backspaced.
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
    // …but not at a look under a box drawn under the card (the composer not shown): the `0`
    // stays owed there, and the card's dropped `0` is backspaced once the box has gone.
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
    // Negative control: the dismissal switched off.
    let mut m = Mock::new(vec![with_survey(idle_screen())]);
    m.vanish_after = Some(1);
    let _ = watch_lines(&mut m, &surveys(auto(30), false));
    assert!(m.presses().is_empty(), "{:#?}", m.requests);
}

/// A `0` that landed in the composer (the survey left between the check and
/// the keystroke) is backspaced, and nothing is said dismissed.
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

/// A survey that leaves during a busy spell and comes back has appeared
/// again: pressed again under `dismiss_surveys` (said again without it)
/// though no look saw it gone, only a busy read on the way to the turn.
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
    // Gone at a read no look judges (output still being written), and back while the first
    // card's `0` may still be landing: the wait on the card back keeps the first one's leaving,
    // and the first card is said dismissed once the landing time is out.
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

/// A skipped `0` with the survey still open is said with `EVENT survey` and retried after its
/// back-off.
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

/// Text on a composer row under the caret row is a typed draft, and so is the caret row's
/// text with the cursor at column 2 of another row; the one-row placeholder is not.
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
    // A cursor on no row read is judged by its column alone.
    let mut off = draft(&["❯ Try \"fix lint errors\""], 0, 2, 0);
    off.cursor_row = 99;
    assert!(!typed_draft(&aterm_phase::ClaudeReader, &off));
}

/// A composer with no placeholder (agy's, [`ScreenReader::placeholder`]): every word on its
/// caret row was typed, even under a cursor at column 2 (a draft whose cursor was moved home);
/// its empty composer holds nothing. Negative control: read as a program that draws a
/// placeholder, the same row under the same cursor is the placeholder.
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

/// GEMINI CLI'S PLACEHOLDER IS NO DRAFT, wherever the cursor is: its reader
/// takes `  Type your message or @path/to/file` out of the composer itself —
/// drawn grey, not dim, the terminal's cursor parked on its first cell in
/// column 3, so neither the column-2 rule nor a `cell` could tell it — and
/// both judges, [`typed_draft`] and the session's own (`composer_draft`),
/// read its empty composer as empty, at column 3 and at 2, and its draft
/// as typed, homed to the caret or not, asking no `cell` of either.
/// NEGATIVE CONTROL: the placeholder's words AS a composer's text, on a
/// program that draws no placeholder of the shared kind, are a draft — the
/// reader's own read is what keeps them from being typed over.
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

// ---- the context indicator: running low, and compacted ---------------

/// `r` with Claude Code's context indicator, `<left>% until
/// auto-compact`, right-aligned on the row above its composer's top rule
/// (ending two columns short of the 120-column rule, as measured).
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

/// A worker running low, then compacting, then running low again: 15%
/// (above the default threshold of 10), 9% and 4% while busy, the
/// indicator gone from the busy screen (the compaction), 12% busy, and 8%
/// at the idle point that ends the turn.
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

/// `EVENT context` is said once per descent (at 9%, not again at 4%); the indicator gone from
/// a framed screen is the compaction, said once; the next descent is said again.
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

/// `context_warn: 0` says neither line: the same descent and compaction
/// — here down to 0%, which 0 would read as low if it were a threshold
/// and not off — print only the idle point, as without the indicator.
/// The bare wait ([`Session::await_turn`], a test seam) says nothing of the
/// indicator, even on a session a loop left a threshold on.
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

/// After `EVENT context`, 30 points or more over the last reading (not the one warned at) is a
/// compaction too, 29 is not; a box over the indicator's place is no sign of one.
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
            "EVENT prompt seq=102 kind=bash classify=not-read-only:rm command=rm -rf target",
            "EVENT compacted seq=105",
            "EVENT idle seq=106 ⏺ Pushed.",
            "EXIT session gone (await seq 106 failed: aterm-ctl: ERR exited)",
        ]
    );
}

/// `r` with its composer's rules `width` columns wide and the context
/// indicator in its other spelling, `Context left until auto-compact:
/// <left>%`, parked against their right edge: the worker's pane narrowed.
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

/// A pane narrowed from 120 to 55 columns moves the indicator and is no compaction: it is
/// still read against the right edge, `context 8%`.
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

/// `supervise`'s watch lasts one run and starts armed: a run starting low warns again at its
/// first read; a compaction between runs is not seen (`watch` keeps one watch throughout).
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
    // The turn ends idle at 8%, warned at 9% on the way.
    assert_eq!(
        run(vec![
            with_context(busy_screen(), 9),
            with_context(idle_screen(), 8)
        ]),
        "EVENT context seq=101 9% until auto-compact\n"
    );
    // The next run, the indicator still low: warned again, and the
    // compaction during it said.
    assert_eq!(
        run(vec![
            with_context(busy_screen(), 8),
            busy_screen(),
            idle_screen()
        ]),
        "EVENT context seq=101 8% until auto-compact\nEVENT compacted seq=102\n"
    );
    // The next run, the worker compacted before it: nothing to say.
    assert_eq!(run(vec![busy_screen(), idle_screen()]), "");
}

/// The threshold is inclusive (10 warns, 11 not), and once warned only a framed screen with no
/// box up can show the indicator gone: a blank screen, a build's output, a covering box or a
/// 29-point jump is no compaction. After one the watch is armed again.
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

// ---- an outage: an aterm self-update's handoff ------------------------

/// A session whose reconnect pauses are milliseconds, not seconds: 5 ms
/// doubling to 20 ms, within `window`.
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
/// [`watch_quick`] with `opts`: recorded, and replayed from its tape, as
/// [`watch_lines_with`].
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
/// The approval policy with a budget of `ms` milliseconds.
fn auto_ms(ms: u64) -> SuperviseOpts {
    SuperviseOpts {
        max: Duration::from_millis(ms),
        ..auto(0)
    }
}
/// The milliseconds a `RECONNECTED after <ms> ms` line names.
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

/// A request not served (no reply, or turned away before it was read) is told from a server's
/// answer by the client's words for a closed or cut-short exchange, a dead socket, a reset or
/// broken connection, a socket timeout, and the `ERR`s that turn a connection away; never
/// another `ERR`, a usage line, a server-answered timeout, or a sandbox refusal. `ERR no such
/// session` counts only during an outage.
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
        // aterm-ctl's own words for a reply cut part-way (its counted and
        // guarded readers, and a byte-framed body).
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
        // The connect phase's own line, printed bare (not as an
        // `aterm-ctl:` error) with exit 2: the request was never sent.
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
    // A note ahead of a server's answer does not make it a lost one.
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

/// What `aterm-ctl` prints and exits with when an update's Commit cuts a
/// request it does not ask again — `await seq <n>`, `await inbox since=`,
/// a bare `await seq`, `turn`, `post` — and when no successor answered at
/// all: exit 75 ([`aterm_ctl::EXIT_REPLACED`]) and a note.
fn replaced(note: &str) -> CtlReply {
    failed(i32::from(aterm_ctl::EXIT_REPLACED), note)
}
/// `aterm-ctl`'s note for an `await seq <n>` an update cut
/// (`not_asked_again_line`), word for word.
const REPLACED_ANCHOR: &str = "aterm was replaced (pid 10 -> 11) before it \
                               answered; `seq 31` names a position in the old process — \
                               re-issue it (exit 75)";
/// `aterm-ctl`'s note for a request whose server went away with nothing
/// answering in its place within the successor wait (`gone_note`).
const REPLACED_GONE: &str = "aterm (pid 10) went away and nothing replaced it within 30.0 s; \
                             the exchange cannot be resumed (exit 75)";

/// Exit 75 is `aterm-ctl`'s "the server went away before it answered": not served, whatever
/// the note says, so it is ridden out.
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
    // Another exit code with the same words is not the client's signal.
    assert!(!failed(1, REPLACED_ANCHOR).lost());
}

/// A bare wait ([`Session::await_turn`], a test seam) on the per-request
/// client, [`crate::CtlClient`] — one `aterm-ctl` per request, its exit code
/// handed up as it is. The
/// `aterm-ctl` here ends the turn's first wait as a real one ends a wait an
/// update's Commit cut (the note, exit 75), then answers as the successor:
/// the turn is read off the successor after one `RECONNECT`.
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
    // A hang bound, not a latency one: the successor answers at once.
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

/// The measured failure: the connection drops under the wait while the
/// worker is busy (the wait for its footer to leave). One `RECONNECT` line
/// however many reads fail (two here; the third answers), then `RECONNECTED
/// after <ms> ms`, and the loop goes on — settling first, then reading —
/// to the next review point, on the seq the new instance counts.
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
            // The instance goes under the wait for the footer to leave.
            "await gone esc.to.interrupt rows 2 7 timeout 20000",
            // Three reads of the same session: two fail, one answers.
            "text --json tail=40",
            "text --json tail=40",
            "text --json tail=40",
            // The loop again: settle, read, report (the turn that ran read
            // up to the row that opened it — a host that keeps no history
            // answers nothing — the `questions` word read; the question to
            // the worker's attention, `status` read first with no manager,
            // `stall.rs`), wait past the point.
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

/// EVERY SLEEP AND CLOCK OF THE RIDE-OUT GOES THROUGH THE DOOR
/// ([`super::super::fx`]). The outage above, stretched to most of an hour:
/// an hour's reconnect window, pauses of a minute doubling to ten, and seven
/// probes that fail before one answers — 60 + 120 + 240 + 480 + 600 + 600 +
/// 600 s of pauses — run on VIRTUAL clocks that only the door's sleeps
/// advance. The loop says what it said in a few real milliseconds before:
/// one `RECONNECT`, `RECONNECTED after` exactly the pauses' sum (no other
/// virtual time can pass), then the next point on the new instance's seq.
///
/// NEGATIVE CONTROL, by construction, for each kind of bypass:
/// - A SLEEP that bypassed the door waits out the pauses for real — three
///   quarters of an hour — and the one-minute hang detector names it.
/// - A CLOCK READ that bypassed it reads the real monotonic clock, which the
///   virtual one stands a DAY behind — more than the whole reconnect window
///   and the run's two-hour budget, so a real reading lands past the virtual
///   run's deadline. A bypassing read as the outage opens, or in the probe
///   loop, finds the budget spent (`TIMEOUT`, no `RECONNECTED`); one for the
///   `RECONNECTED` figure measures the day (`after 86400…` ms). Either
///   changes the lines asserted below (each mutation was run). A virtual
///   clock at the real one's origin would not do: a bypassing loop read
///   lands at the window's start, short of its end by more than any pause,
///   and the run reads the same — measured on 8b17f1f863, whose test passed
///   with that read bypassed. The `supervise-fx` census guard catches every
///   such read statically too.
#[test]
fn fx_virtual_clock_rides_out_an_hour_long_outage() {
    use super::super::fx::{ClockSource, Fx, VirtualClock};
    const FAILED: usize = 8;
    let (pause, pause_max) = (Duration::from_secs(60), Duration::from_secs(600));
    // A Unix `Instant` is a signed timespec, so a day back is always there
    // (the tests run on the fleet's macOS and Linux hosts; the Windows cell is
    // type-checked only).
    let behind = Instant::now()
        .checked_sub(Duration::from_secs(86_400))
        .expect("the monotonic clock reaches a day back");
    let clock = Arc::new(VirtualClock::new(behind, 1_790_000_000_000));
    let ridden = Arc::clone(&clock);
    let (tx, rx) = mpsc::channel();
    // The run is the thread's own (the mock moves in), so a run that hangs is
    // left behind by the detector instead of holding the test for ever.
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

/// After an outage the point still showing is reported again (a duplicate costs the manager a
/// look, a missed point leaves the worker waiting) and waited past on the adopting instance's
/// seq, where `await seq 101` would never latch.
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
            // The same point, reported again, waited past at the new
            // instance's seq 12.
            "await seq 12 timeout 20000",
            "await idle 500 timeout 1500",
            "text --json tail=40",
            // Busy under the footer: its leaving is the next wait.
            "await gone esc.to.interrupt rows 2 7 timeout 20000",
            "text --json tail=40",
            // The turn that ran ends on rows that do not reach the row that
            // opened it: the history above is asked for its earlier words
            // (a host that keeps none answers nothing).
            "modes",
            "offscreen tail=1",
            "await seq 14 timeout 20000",
        ],
        "{:?}",
        m.requests
    );
}

/// A handoff no request saw fail: the new instance's count starts below the point's seq, and
/// the read that checks a step that ran out sees it moved (seq 5 < 101), so the loop looks
/// again rather than waiting on an `await seq` that never latches.
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
            // The start-up reconcile read: no attention stands.
            "meta",
            "await gone esc.to.interrupt timeout 3000",
            "text --json tail=40",
            // Served, and ran out: the count is another instance's now.
            "await seq 101 timeout 20000",
            // The check: seq 5, below 101.
            "text --json tail=40",
            "await idle 500 timeout 1500",
            "text --json tail=40",
            "await seq 6 timeout 20000",
        ],
        "{:?}",
        m.requests
    );
}

/// A session that never comes back ends when the reconnect window lapses, naming the last
/// failure (a closed connection, or exit 75 for an update nothing replaced).
#[test]
fn a_connection_that_never_comes_back_ends_when_the_window_lapses() {
    for (down, words) in [(closed(), CLOSED), (replaced(REPLACED_GONE), REPLACED_GONE)] {
        never_comes_back(down, words);
    }
}

fn never_comes_back(down: CtlReply, words: &str) {
    let mut m = Mock::new(vec![question_screen()]);
    // 0 the first wait, 1 the read, 2 the `questions` word, 3 the escalation's `status`, 4 the
    // question's attention, 5 the wait past it.
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
    // After the first seven requests: the probes, then the watcher's last act, its own badge
    // unset (unanswered: the session is out of reach).
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

/// An outage that keeps coming back (reads answer, waits drop) is one outage, ridden out
/// silently in one window: one `RECONNECT`, one `RECONNECTED`, the end when it lapses.
#[test]
fn an_outage_that_keeps_coming_back_is_one_window() {
    let mut m = Mock::new(vec![question_screen()]);
    // Indices 2, 3 and 4 are the session's `questions` word, the
    // escalation's `status` and the question's attention, not waits.
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

/// An outage ends once what failed is served again.
#[test]
fn a_later_outage_is_a_new_one_with_its_own_window() {
    let mut m = Mock::new(vec![question_screen()]);
    // The waits are 5, 12 and 15. Between them: the `questions` word (2, 9), `status` (3, 10),
    // the attention (4, and 11: the same question asked again after the reconnect), and a
    // stall probe of the step that ran out (14).
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

/// `ERR no such session` ends the loop at once outside an outage; inside one the adopting
/// instance may not host the session yet, so the reads go on until the window lapses.
/// `--reconnect-s 0` makes a lost connection final.
#[test]
fn no_such_session_is_final_outside_an_outage_and_not_yet_inside_one() {
    // 2 the `questions` word, 3 the escalation's `status`, 4 the question's attention: the
    // waits past the point are 5 on.
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
    // Nothing retried; the watcher's last act clears the badge it raised.
    assert_eq!(
        m.requests[7..],
        ["meta unset attention owner=supervisor"],
        "{:?}",
        m.requests
    );
}

/// A request the server turned away before doing anything (`ERR control server busy`, `ERR
/// auth`, `ERR main thread stalled …`) served nothing and is ridden out, a probe included.
#[test]
fn a_connection_turned_away_is_ridden_out() {
    // (Which `ERR`s turn a connection away:
    // `a_request_not_served_is_told_from_a_server_answer`.)
    let text = "auth";
    let mut m = Mock::new(vec![question_screen()]);
    // 2 is the session's `questions` word, 3 the escalation's
    // `status`, 4 its attention, 5 the wait.
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

/// The budget bounds an outage too: once [`SuperviseOpts::max`] (or the
/// wait's timeout) is spent while a lost connection is ridden out — or as
/// the request is lost — the loop ends as it does on any spent budget. The
/// engine tests' seams: `Session::watch` says `TIMEOUT` and exits 124;
/// `Session::supervise` returns `TIMEOUT` with the last screen read;
/// `Session::await_turn` the timed-out turn, and when no read ever
/// answered, a busy phase that says so.
#[test]
fn a_budget_spent_in_an_outage_is_the_timeout() {
    let mut m = Mock::new(vec![question_screen()]);
    // 2 is the session's `questions` word, 3 the escalation's `status`,
    // 4 the question's attention; 5 the wait past it.
    m.down = Some((5, closed()));
    // The budget (150 ms) ends the ride-out, not the reconnect window: a
    // window of two minutes against a minute's hang detector tells the two
    // apart whatever the machine's load.
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

    // The budget ran out while the request that was lost was in flight.
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
    // Nothing after the budget but the watcher's last act: the badge it
    // raised for the question, read and cleared. The wait past the badge is
    // the request lost in flight where it went out before the 100 ms did; a
    // loaded host spends the budget first and never sends it.
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

/// A guarded press whose answer never came is not sent again as it was:
/// the loop reads and classifies first. Here the box moved — a write now,
/// not the read it was about to approve — so it is handed over, with the
/// one press the log shows; nothing is APPROVED, since the press was never
/// confirmed: a press went unanswered. When the same
/// read box is still up after the reconnect, it is pressed again only
/// after the fresh read.
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
        // The box handed over is waited on by its own row leaving.
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
            // The box read and decided again (the caps asked again of
            // the instance that answers now), then pressed.
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

/// A busy read before a lost connection still counts after it (the flag is the loop's, not
/// the dropped wait's), so a point that looks like the last is reported; a relapse wipes
/// nothing.
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
    // Under the wait past the point; the probe answers.
    m.by_index.insert(2, closed());
    // A relapse under the next wait past it; the probe answers.
    m.by_index.insert(6, closed());
    // A relapse under the wait after the busy read (request 9).
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
            // Once more after the outage began: nothing was read in it.
            "EVENT idle seq=103 ⏺ Done.",
            // After the busy read, which the relapse did not wipe.
            "EVENT idle seq=107 ⏺ Done.",
            "EXIT session gone (await seq 107 failed: aterm-ctl: ERR exited)",
        ]
    );
    assert_eq!(code, 1, "{:?}", m.requests);
    // `requests` holds the start-up `meta` the indexes do not count: the
    // busy read is index 9, the footer wait after it index 10.
    assert_eq!(m.requests[10], "text --json tail=40", "{:?}", m.requests);
    assert_eq!(
        m.requests[11],
        "await gone esc.to.interrupt rows 2 7 timeout 20000"
    );
}

/// The engine tests' other seams, `Session::supervise` and
/// [`Session::await_turn`] (`await_turn_on_the_per_request_client_rides_out_exit_75`),
/// ride it out the same way; their lines go to the log they are handed, and
/// their result is untouched.
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

// ---- the journal (`--journal FILE`) -----------------------------------

/// A journal file in a temp dir of this test's own.
fn journal_file(tag: &str) -> (PathBuf, PathBuf) {
    let dir = crate::supervise::test_scratch_path("journal", tag);
    std::fs::create_dir_all(&dir).expect("tmp dir");
    let path = dir.join("journal.jsonl");
    let _ = std::fs::remove_file(&path);
    (dir, path)
}

/// The records a journal file holds, and the file's mode.
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

/// `watch --journal` over the watch loop scripted end to end (busy, a read
/// approved, busy, a question, the manager's turn, idle, the idle screen
/// sitting until the session ends): every printed line — one per decision,
/// no second EVENT for the screen that sits — is appended as one JSON object
/// (the `line` fields are the printed lines), with kind, phase, seq,
/// `complete=`/`rows=` and free-text tail read from the line. The file is
/// created 0600 and appended to, and every record carries the loop's
/// `@sid` and a non-decreasing Unix time.
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
    // The journal-only lines (the escalation, its clearing at the busy read after it, and the
    // idle point's hold) sit between the printed ones, in order.
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
        // Cleared at the busy read after the question (105), not at the
        // idle point that follows it (106), where nothing was typed.
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

/// `supervise --journal` records what it says on stderr and what `watch` would have printed:
/// the review point's EVENT, an approval, the TIMEOUT or `EXIT <reason>`.
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
    // A budget that is already spent: the TIMEOUT is the last record.
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
    // A loop that fails: the `EXIT <reason>` line `watch` would print.
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

/// An unwritable journal is said once (open and append share the one warning) and stops
/// nothing.
#[test]
fn a_journal_that_fails_is_said_once_and_stops_nothing() {
    let (dir, path) = journal_file("bad");
    // A path under a file, not a directory: the open cannot succeed.
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

// ----- A usage limit is a decision point the watcher handles alone -----

/// A weekly-limit notice under the worker's last message, at an idle
/// composer.
const WEEKLY: &str =
    "  ⎿  You've hit your weekly limit · resets Sep 19 at 11am (America/Los_Angeles)";
const WEEKLY_TEXT: &str =
    "You've hit your weekly limit · resets Sep 19 at 11am (America/Los_Angeles)";
const WEEKLY_RESET: &str = "Sep 19 at 11am (America/Los_Angeles)";
/// A box the loop does not approve is the manager's, once per box. For the
/// rm breaker box (outside a bypass session), a Read of a secret and a trust
/// prompt (`other`): the worker's `attention` is set to `claude <kind>:
/// <command, path or question> (<why no rule approved it>)`; a second look at
/// the same box raises nothing; once the box has left the badge is unset
/// (read first, ours) and journaled `CLEARED … box`. Nothing is posted.
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
            // A `/` home the Read rule reads `~` against on every host (a
            // Windows process's own is `C:\Users\…`).
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

/// The escalations (set attention) requests of a finished mock.
fn box_acts(m: &Mock) -> Vec<String> {
    m.requests
        .iter()
        .filter(|r| r.starts_with("meta set attention"))
        .cloned()
        .collect()
}

/// A headerless box's badge names the box's own first row, never a
/// transcript row above it, however tall the transcript.
#[test]
fn a_headerless_box_under_a_tall_transcript_is_named_by_its_own_first_row() {
    let transcript: Vec<String> = (0..40)
        .map(|i| format!("⏺ transcript row {i} done"))
        .collect();
    let mut screen = rows(&transcript.iter().map(String::as_str).collect::<Vec<_>>());
    screen.push(String::new());
    screen.extend(trust_box());
    // The cursor on the box's focused option, inside the loop's tail,
    // where Claude Code parks it.
    let focus = screen
        .iter()
        .position(|r| r == " ❯ 1. Yes, proceed")
        .expect("the focused option");
    let mut m = Mock::new(vec![busy_screen(), screen, busy_screen(), idle_screen()]);
    m.cursor_row = Some(focus);
    m.vanish_after = Some(4);
    let (_, _) = watch_lines(&mut m, &auto(30));
    // The attention format (`<program> <kind>: <subject> (<reason>)`): the
    // subject of a headerless box is its question row, found from the
    // box's own first row down, never a transcript row above it.
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
/// An answered box's badge goes at the first busy read after it ([`Session::look`]), not at
/// the next point. Negative control: the box read again, still up, unsets nothing.
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
    // The box's second read came between the set and the unset: a box
    // still up is never cleared.
    let reads = m.requests[set..unset]
        .iter()
        .filter(|r| r.starts_with("text"))
        .count();
    assert!(
        reads >= 2,
        "the box re-read and the busy read: {:#?}",
        m.requests
    );
    // Cleared while the worker is busy, not by the watch's release.
    assert!(
        m.requests[unset..].iter().any(|r| r.starts_with("await ")),
        "the unset was the watch's last act, not the busy read's: {:#?}",
        m.requests
    );
}

/// The same box back after the worker worked is a new review point (`EVENT
/// prompt` again) and so a new ask: the badge set again. One set per point,
/// never per look.
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

/// The badge's line is cut by terminal cells (a wide character is two), and
/// every character that breaks a line is a space.
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
    // A plain line is untouched.
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
/// The same notice after a retry: new rows, the same limit.
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
/// The model's own limit, a span from now, and the `/model` line that
/// makes it history.
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
/// The screen off the notice with no turn run: a person logged in under
/// another account in the worker's window.
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
/// The worker's answer to the continuation typed at the reset.
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
/// A `/login` half-way through, in the worker's window: Claude Code's code
/// prompt, a URL and a paste field, no composer. Constructed from the
/// dialog's shape.
fn login_code_prompt() -> Vec<String> {
    rows(&[
        " Browser didn't open? Use the url below to sign in (⌘/ctrl + click):",
        " https://claude.ai/oauth/authorize?code=true&client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e",
        "",
        " Paste code here if prompted >",
    ])
}
/// The notice with a draft typed into the composer under it.
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
/// The continuation with the rules file's text.
const RULES_CONTINUATION: &str = "keep going (standing rules: Standing rules: 1. run nothing \
                                  heavy while ~/ay-machine-busy exists 2. report counts)";
/// The guarded `turn` that types `text`.
fn continue_request(text: &str) -> String {
    format!(
        "turn submit=guarded:{} idle=600 timeout=2500 yield=0.2 {text}",
        super::turn_end_loop::composer_guard('❯', text)
    )
}
/// The turn-end policy's timings with the waits shortened: no grace past
/// a reset, the backoff after a continuation that hit the wall `backoff`.
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

// The accounts probe (`docs/SPEC-claude-accounts-probe-2026-10-01.md`):
// the host that claims one, the server it is typed into, and the Tier-1
// projection — shared by `turn_end_loop_tests.rs` (every exit of the
// step) and `run_engine_tests.rs` (the walk against the model).

/// `help key` and `help send` of a host that has every fence the
/// probe's writes carry.
const PROBE_HELP_KEY: &str =
    "key [id=<key>] [if=<re>] [if-gen=<e.s>] [if-human=<n>] <name>: send a named key\n";
const PROBE_HELP_SEND: &str =
    "send [id=<key>] [if=<re>] [if-gen=<e.s>] [if-human=<n>] [--] <text>: write text\n";

/// A host that keeps a roster and claims its probes, as a test drives it: what it advises
/// ([`IdleHost::account`]) and whether it grants the claim ([`IdleHost::account_probe`])
/// are scripted (each ask takes the next entry, the last for ever after), and everything the
/// loop asked or told it is kept, in order. It asks for no point of its own unless a test
/// says so (`wanting`), and owns no turn end.
#[derive(Debug, Default)]
struct ProbeHost {
    advice: Mutex<VecDeque<Option<AccountAdvice>>>,
    advised: Mutex<Option<AccountAdvice>>,
    claims: Mutex<VecDeque<bool>>,
    claimed: Mutex<bool>,
    /// What its relaunch answers ([`IdleHost::restart`]).
    answer: Mutex<Option<&'static str>>,
    /// It asks for a point of its own ([`IdleHost::wants`]); the step it
    /// then takes ([`IdleHost::at_idle`], kept as `at-idle`) is none.
    wanting: AtomicBool,
    /// It relaunches an agent the stall's remedy ends ([`IdleHost::relaunches_after_stall`]),
    /// so the remedy may restart it.
    relaunches: AtomicBool,
    said: Mutex<Vec<String>>,
    /// Each sample handed over, with the world's `pan` and the person's
    /// hand as they stood then ([`PanelShared`]).
    samples: Mutex<Vec<(crate::harness::accounts::Sample, i64, bool)>>,
    shared: Arc<PanelShared>,
    /// Its agent's transcript tail ([`IdleHost::transcript_tail`]; `None`:
    /// none to read), how many times the loop asked for it, and how many
    /// it had asked by each claim ([`IdleHost::account_probe`]).
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

    /// The roster says the account's read is due at every turn end, with
    /// nothing behind it, and every claim is answered `claim`.
    fn probing(claim: bool) -> Arc<Self> {
        Self::scripted(&[Some(AccountAdvice::Probe { otherwise: None })], &[claim])
    }

    /// Everything said to it that starts with `prefix`, in order.
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
        // A refusal carries the host's word for it, as a real host's
        // does (`not-due`: the roster no longer says the read is due).
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

/// A server with every fence the probe carries, its agent on the
/// alternate screen, over `screens` — the last an empty composer, the
/// point the panel's world rests on — with `frames` the measured screens
/// the open panel shows, in order ([`PanelWorld`]). The run ends once
/// the loop has taken its own act and waited twice on what follows.
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

/// The panel with a turn running under it, composed and never observed: the measured settled
/// screen (`USAGE_SETTLED`) with the done row over its rule (where the fullscreen renderer
/// keeps the transcript's last row while the panel is up) replaced by a running turn's
/// spinner row, the one shape on which the loop's busy check can see the turn.
fn panel_under_a_turn() -> Vec<String> {
    let mut rows =
        aterm_phase::prompt::fixtures::screen(aterm_phase::prompt::fixtures::USAGE_SETTLED);
    rows[1] = "✻ Synthesizing… (18s · esc to interrupt)".to_string();
    rows
}

/// The panel's world of `m`.
fn world(m: &mut Mock) -> &mut PanelWorld {
    m.panel.as_mut().expect("a panel world")
}

/// The probe's writes, as they arrived: `/usage` for its text, `esc`
/// for an Escape (written or not).
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

/// Tier-1 for `SupervisorUsageProbe` (aterm-spec `supervisor_usage_probe_model`): every
/// `send … /usage` and every `key … esc` the server received, and every sample the host was
/// handed, projected onto the model at the state the world was in when it arrived
/// ([`PanelProbe`], [`PanelShared`]); each must be enabled there. What this returns is what
/// the model forbids.
///
/// The projection. `pan`, `hand`, `hold`, `turn` are the world's: the screen the server held
/// read by the loop's own readers, a person's key within the world's grace, `status hold=`,
/// a busy screen. `ph` and `own` are the loop's, read off the host's counts: a `/usage`
/// arrives at `ph = 1` only while a claim is granted and unanswered (and at `ph = 2`, where
/// `Type` is disabled, while a panel or its text is owed); an Escape arrives at `ph = 3` once
/// the host was told what the read came to (or was never asked: a panel taken up from the
/// ledger), and at `ph = 2, own = 1` before, where the model forbids it. A sample is the
/// model's `Sample` at the `pan` the world showed as it was handed over.
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

// The limit reset (`docs/SPEC-claude-accounts-resets-2026-10-02.md`):
// the host that gives the go, the server its keys are written to, and
// the Tier-1 projection — shared by `reset_loop_tests.rs` (every exit
// of the step) and `run_engine_tests.rs` (the walk against the model).
// NOTHING OF IT HAS BEEN SEEN: the world is composed from the 2.1.304
// bundle ([`ResetWorld`]).

/// The usage-limit dialog's rows as the composed fixture draws them
/// (`aterm_phase::reset::fixtures::DIALOG_GRANT`): the grant row second.
const RESET_STOP: &str = "Stop and wait for limit to reset";
const RESET_GRANT: &str = "Use a weekly reset (2 left, until Oct 5)";
const RESET_WAIT: &str = "Wait here, then continue automatically at Oct 9 at 5am";
const RESET_CREDITS: &str = "Switch to usage credits";
const RESET_ROWS: [&str; 4] = [RESET_STOP, RESET_GRANT, RESET_WAIT, RESET_CREDITS];

/// One step of the host's own, as the model names it (`Claim`: its
/// first go for the wall; `ReadLine`: its last word on a step whose row
/// was taken; `ClaimLapses`: its last word on a claim whose row never
/// was), with the world as it stood ([`ResetShared`]).
#[derive(Debug, Clone)]
struct ResetHostStep {
    action: &'static str,
    dlg: i64,
    claim: i64,
    ph: i64,
    hand: bool,
    hold: bool,
}

/// A host that gives the go for a limit reset, as a test drives it: what it answers the
/// loop's ask ([`IdleHost::reset_take`]) and what it reads of the step's end
/// ([`IdleHost::reset_told`]) are scripted, and everything it was asked or told is kept, in
/// order. It keeps the one claim as the library does: a go while none is done, `attempted`
/// after its outcome. It asks for no point of its own and owns no turn end.
#[derive(Debug)]
struct ResetHost {
    /// The wall the go is for, and whether it stands more than a day.
    until: i64,
    long: bool,
    limits: Vec<String>,
    /// From this ask on (1-based) the wall no longer stands more than a
    /// day: the go says so.
    short_from: Mutex<Option<usize>>,
    /// It refuses, with this word.
    no: Mutex<Option<ResetNo>>,
    /// The asks (1-based) it answers that it cannot say yet
    /// ([`ResetWord::Again`]).
    again: Mutex<Vec<usize>>,
    /// From this ask on (1-based) the claim is no longer this
    /// session's.
    lapses_at: Mutex<Option<usize>>,
    asks: std::sync::atomic::AtomicUsize,
    outcome: Mutex<ResetOutcome>,
    said: Mutex<Vec<String>>,
    steps: Mutex<Vec<ResetHostStep>>,
    shared: Arc<ResetShared>,
}

impl ResetHost {
    /// A host whose go is for a weekly wall a week off, and which reads
    /// `outcome` at the step's end.
    fn reading(outcome: ResetOutcome) -> Arc<Self> {
        Self::reading_at(outcome, &["weekly"])
    }

    /// [`Self::reading`] at a wall drawn by `limits`.
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

    /// A host that refuses the reset with `why`.
    fn refusing(why: ResetNo) -> Arc<Self> {
        let host = Self::reading(ResetOutcome::Unread);
        *host.no.lock().unwrap() = Some(why);
        host
    }

    /// Everything said to it that starts with `prefix`, in order.
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
        // A claim whose row was never taken ends as the model's clock
        // ends one: the loop read no line for it.
        let taken = self.shared.ph.load(Ordering::SeqCst) > 0;
        self.step(if taken { "ReadLine" } else { "ClaimLapses" });
        self.shared.claim.store(2, Ordering::SeqCst);
        self.shared.ph.store(0, Ordering::SeqCst);
        self.shared.own.store(false, Ordering::SeqCst);
        *self.outcome.lock().unwrap()
    }
}

/// `help key` of a host with every fence the reset's keys carry.
const RESET_HELP_KEY: &str =
    "key [id=<key>] [if=<re>] [if-gen=<e.s>] [if-human=<n>] <name>: send a named key\n";

/// A server with every fence the reset's keys carry, over `screens` and
/// then the usage-limit dialog `labels` (the cursor on `focus`), which
/// the reset's world opens on ([`ResetWorld`]) under `host`.
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

/// The reset's world of `m`.
fn reset_world(m: &mut Mock) -> &mut ResetWorld {
    m.reset.as_mut().expect("a reset world")
}

/// The keys the server wrote while the reset's world stood, by name and where they landed:
/// `down@dialog`, `enter@confirm`, `esc@checking`.
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

/// The step's bounds as they ship, with the ones `cut` names shortened to a scripted server's
/// scale. The world moves by reads, so a run is fast under the real bounds and no stall of
/// the machine changes a key; a test shortens only the bound it is run to see run out, where
/// a late machine only delays what it shows.
fn reset_waits_cut(cut: impl FnOnce(&mut reset_loop::ResetWaits)) -> reset_loop::ResetWaits {
    let mut waits = reset_loop::RESET_WAITS;
    cut(&mut waits);
    waits
}

/// The window's own policy with `[harness] limit_reset` as given, under
/// `host`, a person's grace of `grace_s`.
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

/// What one watch left: the lines it printed, every line of its journal (the
/// printed ones and the journal-only ones), and its session ledger's rows.
struct ResetRun {
    lines: Vec<String>,
    journal: Vec<String>,
    ledger: Vec<String>,
}

impl ResetRun {
    /// The journal's lines about the reset's rule.
    fn of_rule(&self) -> Vec<&str> {
        self.journal
            .iter()
            .map(String::as_str)
            .filter(|l| l.contains("rule=limit-reset@v1"))
            .collect()
    }

    /// The ledger's rows under the reset's rule, as `<decision>|<command>|<reason>`.
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

    /// How many rows of the ledger are an act the loop typed.
    fn typed(&self) -> Vec<&str> {
        self.ledger
            .iter()
            .map(String::as_str)
            .filter(|row| row.contains(",\"decision\":\"typed\","))
            .collect()
    }
}

/// One watch of `m` under `opts`, with a ledger and a journal of its own
/// (`seed` writes the ledger first: what an earlier loop left).
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

/// No ledger from before, no setup beyond the watch's own.
fn fresh(_: &Path) {}

/// Tier-1 for `SupervisorLimitReset` (aterm-spec `supervisor_limit_reset_model`): every key
/// the server wrote, and each of the host's own two steps, projected onto the model at the
/// state the world was in when it arrived ([`ResetProbe`], [`ResetShared`]); each must be
/// enabled there. What this returns is what the model forbids, each named: the defect of the
/// model that is the step with its guard dropped, and the invariant that defect breaks.
///
/// The projection. `dlg`, `guard`, `focus`, `hand`, `hold` are the world's: the dialog on the
/// screen (its grant row known by the world's own labels, never by the reader under test),
/// the vendor's key guard, the confirmation's cursor, a person's key within the world's
/// grace, `status hold=`. `claim` is the host's; `ph` and `own` the loop's, read off what the
/// world took: the row's Enter (`ph = 1`, `own = 1`), an Enter on the confirmation (`2`), an
/// Escape (`3`), and the host's last word (`0`). A key at the dialog is the grant row's press
/// (`TakeRow`) while a claim is in flight with no row taken (its Enter must be on the grant
/// row), else the wait row's (`PressWait`); `enter` on the confirmation is `Enter`, `esc` is
/// `Escape`. A key the server skipped or refused wrote nothing and is no step. Any other
/// written key (an arrow at the confirmation, a key at a pending line, a second Enter on a
/// dialog row that is neither) is forbidden outright.
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
        // The defect that is this step with a guard dropped, and the
        // law it breaks.
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
                    // The grant row entered with no claim in flight.
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
    // Nothing of the reset is ever typed: a `send`, `turn` or `paste` that opens with either
    // of the vendor's commands is the model's `TypesTheCommand` (typed with no dialog up,
    // `/limit-reset` falls through to the weekly session reset, which claims at once).
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

/// `EVENT limited` escalates once an episode (the worker's `attention` set
/// to the notice, one `ESCALATED` journal line, nothing posted), and a 30 s
/// budget that would run out two
/// days before the named reset is stretched to 10 min past it, `EXTEND`
/// said once: the retry's notice (a new point, printed) escalates and
/// extends nothing again. The printed lines are unchanged but for the one
/// `EXTEND`.
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
            // The start-up reconcile read.
            &"meta".to_string(),
            // `resume_limits = false`: nothing here continues it, so it asks.
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
            // Each limited point typed nothing: a HOLD for each, since the
            // busy read between them ended the first.
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

/// The escalation records what the server said, and goes on: a refused
/// attention is a journal word, not the loop's end, and nothing is posted.
/// A notice naming no reset extends nothing.
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
    // The start-up reconcile read answers first; then the refusal.
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

/// A refusal naming a tighter cap (`attention too long (max <n> bytes)`) is answered once with
/// the text cut to it. Negative control: a cap the text already fits is not retried.
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
    // A limit's badge names its kind before the text: nothing here ends this
    // one (no reset, limits not resumed), so it asks.
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

/// `resume_limits` is the one gate: under `false` a reset already past is
/// not continued (and, past, it extends no budget). Negative control: `true`
/// continues the same point.
#[test]
fn resume_never_continues_what_the_policy_limited() {
    for (resume_limits, turns) in [(false, 0), (true, 1)] {
        let mut m = Mock::new(vec![busy_screen(), limited_screen()]);
        m.vanish_after = Some(1);
        m.turn_releases = Some(1);
        let eight_pm = TEST_NOW + 11 * 3600 + 5 * 60;
        // The false arm waits out the whole budget (nothing is typed), so
        // it is short: the reset passed long before either arm decides.
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

/// Past the reset one guarded `turn` types `keep going` with the rules file's text (the
/// program read fresh first); a wall acted on at once raises no badge and mails no one
/// (`LIMITED … handled`), and the answer after a busy spell closes the episode.
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
            // Half a minute past the reset, 10 min past it is still
            // ahead of a 30 s budget.
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

/// The screen leaving the notice before the reset (a `/login`) is the continuation's moment:
/// `keep going` goes at once and the answer closes the episode. A reset two days off extends
/// no long budget.
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
    // A limit the policy waits out raises no badge: nobody is needed.
    assert_eq!(count(&m, "meta set attention"), 0, "{:?}", m.requests);
    assert!(!lines.iter().any(|l| l.starts_with("EXTEND")), "{lines:?}");
}

/// The episode's close unsets only its own keyed entry (`owner=supervisor`): a person's bare
/// `claude needs approval: …` over it stands. Nothing is read first.
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
    // Resuming limited: the episode is escalated, and closed when the
    // worker works again.
    let (lines, _) = watch_lines_with(&mut m, &opts, |_| {});
    assert!(
        !lines.iter().any(|l| l.starts_with("CONTINUED")),
        "nothing typed: {lines:?}"
    );
    assert_eq!(count(&m, "meta set attention owner=supervisor"), 1);
    assert_eq!(count(&m, "meta set attention"), 1, "never the bare form");
    // The start-up reconcile read only.
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

/// The notice again after a continuation (the limit had not reset) is the same episode: its
/// point printed, nothing raised, and the next continuation waits the backoff (60 s, past the
/// budget: none goes), journaled `WAITING` with its deadline.
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
    // Waited out, both times: no badge, nobody told.
    assert_eq!(count(&m, "meta set attention"), 0, "{:?}", m.requests);
    assert_eq!(count(&m, "turn"), 1, "{:?}", m.requests);
    let (records, _) = journal_records(&path);
    let waiting: Vec<(Option<u64>, &str)> = records
        .iter()
        .filter(|r| r.kind == "waiting")
        .map(|r| (r.seq, r.summary.as_str()))
        .collect();
    // 18:00:30Z, the fixed clock, plus the 60 s backoff.
    assert_eq!(
        waiting,
        [(Some(104), "until=2026-09-19T18:01:30Z usage-weekly resets")],
        "{records:#?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A span (`resets in 1s`) counts from the notice's first print: read again after a
/// continuation it names the same reset, so `EXTEND` is said once and continuations back off
/// 10 then 30 ms. A model's own limit with no model to switch to is waited out like a usage
/// window. Real clock: a fixed one never moves `now + span`.
#[test]
fn a_span_is_read_once_and_the_continuations_back_off_ten_then_thirty() {
    let (dir, path) = journal_file("limit-span-once");
    let mut limited = rows(&[
        "⏺ Running the suite.",
        "",
        "✻ Worked for 12s · done 4:51 PM",
        "",
    ]);
    // Right-aligned on the mode row, where Claude Code draws its own notices.
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

/// A draft in the composer holds the continuation: a `turn` would land in it.
#[test]
fn a_draft_in_the_composer_holds_the_continuation() {
    let mut m = Mock::new(vec![busy_screen(), weekly_drafted()]);
    m.vanish_after = Some(3);
    // The cursor at the end of the draft.
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
    // Negative control: the same notice with nothing typed continues.
    let mut m = Mock::new(vec![busy_screen(), weekly_limited()]);
    m.vanish_after = Some(3);
    let _ = watch_lines_with(&mut m, &resuming(&auto(30), None), |s| {
        s.set_clock(AFTER_RESET, PDT);
        s.set_turn_end_timing(quick_turn_end(&[Duration::from_millis(10)]));
    });
    assert_eq!(count(&m, "turn"), 1, "{:?}", m.requests);
}

/// The worker answering on someone else's turn (the manager's after the reset, a human's)
/// closes the episode: its point prints, the attention is cleared, and nothing is typed (the
/// rules ride with a continuation, never as a turn of their own).
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
            // `in 3h` from the test clock, plus 10 min.
            "EXTEND until=2026-09-17T19:05:00Z reset=in 3h",
            "EVENT idle seq=104 ⏺ Done.",
            "EXIT session gone (await seq 104 failed: aterm-ctl: ERR exited)",
        ]
    );
    assert_eq!(count(&m, "turn"), 0, "{:?}", m.requests);
    assert_eq!(count(&m, "meta set attention"), 0, "waited out: no badge");
    let _ = std::fs::remove_dir_all(&rdir);
}

/// The `/login` half-way (the code prompt, no composer on the screen) reads as idle and
/// leaves the notice, and is not the continuation's moment: nothing is typed where the code
/// goes; the dialog's point prints.
#[test]
fn a_screen_without_a_composer_gets_no_continuation() {
    let mut m = Mock::new(vec![busy_screen(), weekly_limited(), login_code_prompt()]);
    m.vanish_after = Some(2);
    let (lines, _) = watch_lines_with(&mut m, &resuming(&auto(3 * 86_400), None), |_| {});
    assert_eq!(lines.len(), 3, "{lines:#?}");
    assert!(lines[0].starts_with("EVENT limited seq=102"), "{lines:#?}");
    // A screen with no composer frame is settled by the full idle wait:
    // the short settle's read, then the settled one.
    assert!(
        lines[1].starts_with("EVENT idle seq=104"),
        "{lines:#?} {:#?}",
        m.requests
    );
    assert!(lines[2].starts_with("EXIT session gone"), "{lines:#?}");
    assert_eq!(count(&m, "turn"), 0, "{:#?}", m.requests);
}

/// `Feb 1` read in September is a misreading: no budget is stretched.
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

// ---- the auto-continue notice -----------------------------------------

/// An auto-continue notice's text; the rows around it are reconstructed.
const AUTO_TEXT: &str =
    "⚠ Usage limit reached · continuing automatically at 1:50pm · esc to cancel";
const SHORTLY_TEXT: &str = "⚠ Usage limit reached · continuing shortly · esc to cancel";
const AUTO_SPINNER: &str = "✶ Deliberating… (4s · ↑ 1.2k tokens · esc to interrupt)";
fn auto_transcript() -> Vec<String> {
    rows(&["⏺ Bash(./run.sh --queue 8)", "  ⎿  queued 8 runs", ""])
}
/// The wait: the notice alone in the live zone, nothing busy under it.
fn auto_waiting(notice: &str) -> Vec<String> {
    let mut r = auto_transcript();
    r.push(notice.to_string());
    r.extend(composer("  ⏵⏵ auto mode on (shift+tab to cycle)"));
    r
}
/// A resumed turn under the notice: the notice still up, a spinner status row under it, the
/// busy footer.
fn auto_resumed() -> Vec<String> {
    let mut r = auto_transcript();
    r.extend(rows(&[AUTO_TEXT, AUTO_SPINNER, ""]));
    r.extend(composer(
        "  ⏵⏵ auto mode on (shift+tab to cycle) · esc to interrupt · 2 shells",
    ));
    r
}
/// The turn it resumed, over: the notice above the transcript is history.
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

/// An auto-continue notice reads limited (its time the reset, the budget stretched past it),
/// and the first busy read after it closes the episode; Claude Code goes on by itself, so the
/// episode opens handled: nothing typed, no badge, no mail.
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
            // 1:50pm Pacific is 20:50Z; ten minutes past it.
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

/// `continuing shortly` is a reset a minute off (`reset=shortly`, the budget to 11 min from
/// now), and nothing is typed or raised.
#[test]
fn a_continuing_shortly_notice_is_a_reset_a_minute_off() {
    let mut m = Mock::new(vec![busy_screen(), auto_waiting(SHORTLY_TEXT)]);
    m.vanish_after = Some(2);
    let (lines, _) = watch_lines_with(&mut m, &resuming(&auto(30), None), |_| {});
    assert_eq!(
        lines,
        [
            format!("EVENT limited seq=102 message={SHORTLY_TEXT} reset=shortly"),
            // TEST_NOW is 15:55:00Z: a minute, then the ten-minute grace.
            "EXTEND until=2026-09-17T16:06:00Z reset=shortly".to_string(),
            "EXIT session gone (await seq 102 failed: aterm-ctl: ERR exited)".to_string(),
        ],
        "{:#?}",
        m.requests
    );
    assert_eq!(count(&m, "turn"), 0, "the vendor goes on by itself");
    assert_eq!(count(&m, "meta set attention"), 0, "{:?}", m.requests);
}

/// Past its time, an auto-continue notice is still not continued: the continuation is the
/// vendor's own. Negative control: a notice that names a reset, at the same clock, is continued.
#[test]
fn an_auto_continue_notice_is_never_continued_even_past_its_time() {
    // 13:51:30 PDT: a minute and a half past 1:50pm.
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
    // Past its time by less than `auto_resume_grace`: still waited.
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

/// A continuation that hits the wall again (the retry's spinner, then
/// `continuing shortly`) closes the episode at the busy read and finds the
/// notice again: the same episode reopened. A self-resuming notice is
/// handled (waited out): no badge at any of them, and each episode is
/// journaled `LIMITED … handled`, never `ESCALATED`.
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

/// `meta`'s `trusted_cwd=` — the one folder an Owner's `spawn agent=claude`
/// trusted for its worker — is read pct-decoded into the loop's
/// `trusted_cwd`, which the folder-trust rule gets (`ApprovalCtx`). NEGATIVE
/// CONTROLS: `-` (none trusted), a relative value, and an older host that
/// prints no such key trust nothing, each over one that did; `cwd=` and
/// `agent_cwd=` are never read as it.
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

/// `meta`'s `worker=` — the agent a worker's spawn started — is read into
/// the loop's `worker_agent`, which the approval rules get (`ApprovalCtx`:
/// a Gemini CLI worker's folder-trust dialog is never pressed). NEGATIVE
/// CONTROLS: `-` (no spawn's worker), a word no worker's agent is
/// (`generic`, `vim`), and an older host that prints no such key name none,
/// each over one that did.
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

/// The engine's tests: the approval policy in the loop, the wake, the
/// lifecycle and the escalation (`run_engine_tests.rs`).
mod engine {
    use super::*;
    include!("run_engine_tests.rs");
    /// The policy text in the loop (`run_question_text_tests.rs`), over
    /// this module's question helpers and Tier-1 bind.
    mod text {
        use super::*;
        include!("run_question_text_tests.rs");
    }
    /// The review key of a box and the one ask it gets however it is redrawn
    /// (`run_box_key_tests.rs`), over this module's box helpers, with its Tier-1 bind to
    /// `SupervisorBoxAsk`.
    mod box_key {
        use super::*;
        include!("run_box_key_tests.rs");
    }
}

/// A box taller than the pane, read whole from the history above the screen
/// (`box_head_tests.rs`).
mod box_head_loop {
    use super::*;
    include!("box_head_tests.rs");
}

/// The turn-end policy in the loop (`turn_end_loop_tests.rs`).
mod turn_end {
    use super::*;
    include!("turn_end_loop_tests.rs");
}

/// The worker's NEEDS YOU on the person's screen (`needs_loop_tests.rs`;
/// Autopilot Cruise / Land, `escalate.rs`'s `sync_needs`).
mod needs_loop {
    use super::*;
    include!("needs_loop_tests.rs");
}

/// The session's autopilot word in the loop (`autopilot_loop_tests.rs`): Land types the land
/// instruction, Off parks the loop.
mod autopilot_loop {
    use super::*;
    include!("autopilot_loop_tests.rs");
}

/// The task's face as the loop tells it, and the landing read back at a
/// restart (`task_face_loop_tests.rs`, the Autopilot design's §9.4).
mod task_face_loop {
    use super::*;
    include!("task_face_loop_tests.rs");
}

/// Why Land still holds, as the loop tells it (`hold_loop_tests.rs`, the
/// Autopilot design's §12.3.3).
mod hold_loop {
    use super::*;
    include!("hold_loop_tests.rs");
}

/// The wall confirmed by the transcript before it is acted on (`wall_confirm_loop_tests.rs`).
mod wall_confirm_loop {
    use super::*;
    include!("wall_confirm_loop_tests.rs");
}

/// The limit reset's step in the loop (`reset_loop_tests.rs`).
mod reset {
    use super::*;
    include!("reset_loop_tests.rs");
}

/// The hold on a worker that stopped reading its input (`stall_tests.rs`).
mod stall_hold {
    use super::*;
    include!("stall_tests.rs");
}

/// A recorded loop (`tape_loop_tests.rs`): recording changes nothing the
/// loop does, a recorded approval reads back as its decision, and a run that
/// dies inside a frame leaves a tape that reads to its tear.
mod tape_loop {
    use super::*;
    include!("tape_loop_tests.rs");
}

/// A replayed loop (`replay_loop_tests.rs`): the negative controls a strict
/// replay must catch, each at its door and of its class; a recorded panic
/// reproduced; a tape stopped short, cut short or made by hand.
mod replay_loop {
    use super::*;
    include!("replay_loop_tests.rs");
}

/// `aterm drive replay --why` over a recorded loop (`why_loop_tests.rs`): a
/// recorded approval explained from the tape alone and decided again on its
/// facts, a fact or a look the tape lacks (or holds only after the point)
/// named among what the decision needed, and a turn end's trace computed in
/// a strict replay to the point.
mod why_loop {
    use super::*;
    include!("why_loop_tests.rs");
}

/// A COUNTERFACTUAL over a recorded loop (`counterfactual_loop_tests.rs`): a
/// recorded run replayed in acts mode under another policy diverges at the
/// first act it takes otherwise, both sides said; one that changes nothing
/// the run consults reproduces; a build that reads the clock more often
/// still replays in acts mode and drifts in a strict one; and the shadow
/// re-decides every recorded box under the candidate.
mod counterfactual_loop {
    use super::*;
    include!("counterfactual_loop_tests.rs");
}

/// A CORPUS TAPE made from a recorded loop (`corpus_loop_tests.rs`): the
/// recording machine's home scrubbed to the lab's token in the header and on
/// the screen, the kept text checked in acts mode by `replay --check`; and a
/// tape refused — a planted unscrubbed path, a scrub that changes what the
/// loop does, no end, a tape that does not replay as recorded.
mod corpus_loop {
    use super::*;
    include!("corpus_loop_tests.rs");
}
