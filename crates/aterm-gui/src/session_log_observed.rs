// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE REMAINING §4.1 MARKS (session log Phase 3B, plan §3.2): what the
//! engine, the status classifier, the screen reader, the resize ledger and
//! the launch card say, each one typed call into [`SessionLog`] from a
//! timeline hold its site already takes (no payload is formatted to be parsed
//! back, design:775). [`Observed`] holds what each last wrote, so a value
//! written twice writes one mark, and crosses a seamless update as additive
//! tokens on the carry's `state` host line (fingerprints only, ruling S5), so
//! a successor does not restate them.
//!
//! What each writes (the node `-` is the session, `<run>` the run in front):
//! - `mark - integration state=on|off|degraded`, when the shell
//!   integration's posture moves;
//! - `mark - notify n=<delta>`, at most once a sweep, the notifications the
//!   program asked for since the last (counted with notifications off too,
//!   ruling S19; never their text), the first read a baseline;
//! - `mark - bell n=<delta>`, at most once a second;
//! - `mark <run> progress state=none|normal|error|indeterminate|paused
//!   pct=<n>`, on a state change or each ten points;
//! - `mark <run> status phase=<w> out=ok|fail|signal code=<n>`, each
//!   published status transition, at the classifier's confidence;
//! - `mark <run> ctx pct=<n>` when Claude Code's context left moves a
//!   tenth, and `mark <run> goal state=on|off` when its goal does;
//! - `amend r<n> agent ver= model= effort= src=screen conf=strong`, the
//!   launch card's names (a transcript's exact amend outranks it);
//! - `mark - render state=desync-risk|healed …`, each transition of the
//!   resize ledger's verdict, with every field of its row (phase 6, 6F-7);
//! - `mark - cwd cwd=<path>` when the OSC 7 directory moves (`cwd=` absent
//!   for a cleared one), and `mark - title title=<informative title>`
//!   (content tier: never journaled, never carried) when the title moves
//!   past a spinner frame, from the drift flush;
//! - `mark - modes n=<reverted>`, the modes a handback or a manual reset
//!   put back, since phase 6 (6F-7) on every row with its `source=` or
//!   `from_pg= to_pg= prog=`, `reverted=` and `bytes=`;
//! - `mark - in-doubt verb=<w> producer= key_seq=`, a keyed write whose
//!   outcome is unknown (never its bytes; its reply's first two words at
//!   the content tier, 6F-7);
//! - `mark - mail dir=in|out …`, `mail-seen`, `mail-landed` and `hold`, from
//!   the fabric's one typed hook (`FabricFact`), never a body; since phase
//!   6 (6F-6) each with every field of its row: a post's `re= dl=` and, host
//!   tier, `via= idem=`; every `inbox seen` with `moved=0|1` (the fold folds
//!   only moves); a post landed nowhere with its `why=`; a hold's `origin=
//!   hold_origin=` and its reason at the content tier;
//! - `mark - meta field=<f> value=<v>` by its writer, from the one
//!   `meta-change` writer (`SessionTimeline::record_meta_change`), one for
//!   every row (phase 6, 6F-8, ruling U13) — for `attention`, whose text
//!   can hold a box's command or a question's words (the supervisor's
//!   escalation), `fp=<16 hex>` in place of the value, the text beside it
//!   at the content tier only (`attn_text=`: RAM, never journaled, carried
//!   or published); the Autopilot pair's timer `land_at=`; and `carried=1`
//!   where the value is what this log last said of the field (a successor
//!   restoring a carried value: no news) — and the `need`/`clear
//!   word=escalated src=meta` it derives `by=aterm`;
//! - `mark - story word=<told verb>` by its teller, never its text;
//! - `mark - usage h5=<pct> d7=<pct>` `by=vendor src=aterm`, each usage tap
//!   reading the harness worker takes while `[harness] usage_tap` is on;
//! - `mark - task rev= state= attention= [why=] [code=] src=aterm
//!   conf=exact`, each move of a worker's task record (phase 5, 5C-3), in
//!   the hold that records its `task-change` row;
//! - `mark <run> program state=<idle|working|blocked|done|error|none>
//!   [ask=<permission|question|auth>] [coalesced=<n>] src=engine
//!   conf=claim`, each move of what the program in front says about itself
//!   (OSC 7501) that the status sweep's gate shows, in the hold that records
//!   its `program-state` row (phase 6, 6F-4; rulings U17, U44): a per-run
//!   bucket of [`PROGRAM_BURST`], then one each [`PROGRAM_REFILL_MS`],
//!   bounds a flapping program, and a move that finds it empty is
//!   COALESCED, never dropped: it replaces the run's one pending state, and
//!   the newest is written once a token frees (at the next move or status
//!   sweep, [`SessionLog::program_tick`]) with `coalesced=` the states it
//!   replaced. WHERE IT LANDS: on the run in front when the gate shows the
//!   move (`-` with none open); a run's pending state is written on that run
//!   before it closes (every run close is `SessionLog::close_node`), before
//!   the session closes and before an update's carry, so the move the gate
//!   shows once the program left (its report cleared, `state=none`) is the
//!   next front's, and no program mark ever falls to the deepest open node.
//!   The log keeps [`aterm_slog::program_share`] of them a run (64, T6); the
//!   fold never reads them and `brief` prints none;
//! - `mark - shown [need=<w> sev=<stuck|decide>] [held=<w>] [coalesced=<n>]
//!   by=aterm src=aterm conf=strong`, each move of what the person is shown
//!   of the session's need (phase 6, 6F-9; rulings U26, U44): KEYED ON THE
//!   PRINTED TUPLE ([`ShownTuple`]: `status needs=`, its severity, and
//!   `hold_by=`; each absent for none), never on the published verdict's
//!   other facts (`waiting`, `recheck_unix`), from the one verdict writer
//!   (`SessionTimeline::set_need`); a per-session bucket like the program
//!   marks' coalesces a flapping verdict to its newest tuple, written once a
//!   token frees (the next move or status sweep, [`SessionLog::shown_tick`]),
//!   before the session closes and before an update's carry. Local (no bus
//!   class), journaled; `brief` counts them (`shown <n>`, a count line shed
//!   first), and the carry holds the last tuple so a successor does not
//!   restate it;
//! - THE RING-ONLY FACTS (phase 6, 6F-5), each in the hold that records its
//!   ring row: `mark - state state=<w>` each registry move
//!   ([`SessionLog::observe_state`]; the close is the session's close
//!   record); `mark - policy gen=<n>` on every session the harness policy
//!   cell moves on (ruling U16); and from the fabric's hook `mark - advisory
//!   origin=<w> gen=<n>` (its reason at the content tier), `mark -
//!   mail-fetch off=<n>` (ruling U15) and `mark - topic change=add|drop
//!   fp=<16 hex>` (the name and `since=` at the content tier, ruling U11).
//!
//! None of it is input, and none of it names a PTY path (slog-inert).

use aterm_slog::{
    Amend, BoundedText, BoundedToken, Conf, Fields, Key, Kind, Mark, Node, Src, Stamp, Val,
};

use super::{SessionLog, name_fp, tok, with};

/// A bell is said at most this often (monotonic ms): a flood of BELs is one
/// count a second.
pub(crate) const BELL_EVERY_MS: u64 = 1_000;

/// A program's moves written at once before its run's bucket is empty
/// (6F-4, U17): the mark limiter's burst.
pub(crate) const PROGRAM_BURST: u64 = 20;
/// Past the burst, a program's run earns one program mark this often
/// (monotonic ms).
pub(crate) const PROGRAM_REFILL_MS: u64 = 1_000;
/// A full bucket, in thousandths of a token.
const PROGRAM_FULL_MILLI: u64 = PROGRAM_BURST * 1_000;

/// A THROTTLED MARK CLASS'S BUCKET (6F-4, 6F-9; ruling U44): [`PROGRAM_BURST`]
/// tokens, refilled one each [`PROGRAM_REFILL_MS`], held in thousandths as
/// of `at` (monotonic ms). A program's run keeps one ([`ProgramMarks`]), and
/// a session's shown marks one ([`ShownMarks`]); each coalesces what finds
/// it empty to its newest value, never dropping that.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MarkBucket {
    milli: u64,
    at: u64,
}

impl Default for MarkBucket {
    fn default() -> Self {
        MarkBucket::full(0)
    }
}

impl MarkBucket {
    /// A full bucket as of `at` (monotonic ms).
    const fn full(at: u64) -> Self {
        MarkBucket {
            milli: PROGRAM_FULL_MILLI,
            at,
        }
    }

    /// Refill to `now` (monotonic ms; a clock read behind the last is the
    /// last), then spend a whole token, or spend one anyway (`force`: a
    /// close or a carry, which never loses the newest value). Whether it
    /// was spent.
    fn spend(&mut self, now: u64, force: bool) -> bool {
        let now = now.max(self.at);
        let earned = (now - self.at).saturating_mul(1_000) / PROGRAM_REFILL_MS;
        self.milli = self.milli.saturating_add(earned).min(PROGRAM_FULL_MILLI);
        self.at = now;
        if self.milli < 1_000 && !force {
            return false;
        }
        self.milli = self.milli.saturating_sub(1_000);
        true
    }
}

/// What the program in front says about itself, as its mark words it: the
/// `state` word (`none` once the shown report went away) and the `ask` word
/// of a blocked one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProgramWords {
    pub(crate) state: &'static str,
    pub(crate) ask: Option<&'static str>,
}

impl ProgramWords {
    /// The words of the shown report `shown` (`None`: none shown).
    pub(crate) fn of(shown: Option<aterm_core::terminal::program_state::ProgramState>) -> Self {
        shown.map_or(
            ProgramWords {
                state: "none",
                ask: None,
            },
            |s| ProgramWords {
                state: s.state.as_str(),
                ask: s.kind.map(|k| k.as_str()),
            },
        )
    }
}

/// THE PROGRAM MARKS' BUCKET (6F-4; rulings U17, U44): the run in front's
/// tokens, and the newest move not yet written. A move that finds no token
/// replaces the pending one, counted; the pending state is written as soon
/// as a token frees, so the newest is never lost.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProgramMarks {
    /// The run the bucket is the share of (`None`: no move yet).
    run: Option<Node>,
    /// Its tokens.
    bucket: MarkBucket,
    /// The newest move not yet written, and how many states it replaced.
    pending: Option<(ProgramWords, u64)>,
}

/// WHAT THE PERSON IS SHOWN of a session's need (phase 6, 6F-9; ruling
/// U26): the printed tuple, as its `shown` mark words it — the need's word
/// (`status needs=`), how it is shown (its severity) and whose hold stands
/// (`hold_by=`), each `None` for none.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ShownTuple {
    pub(crate) need: Option<&'static str>,
    pub(crate) sev: Option<&'static str>,
    pub(crate) held: Option<&'static str>,
}

impl ShownTuple {
    /// The tuple verdict `v` prints; its `waiting` and `recheck_unix` are
    /// not shown in it.
    pub(crate) fn of(v: &crate::needs::Verdict) -> Self {
        ShownTuple {
            need: v.need.as_ref().map(|n| n.word.word()),
            sev: v.need.as_ref().map(|n| n.word.severity().word()),
            held: v.held.map(aterm_types::needs::HoldWord::word),
        }
    }

    /// The carry's token (`<need>.<sev>.<held>`, `-` for none).
    #[cfg(any(unix, test))]
    fn token(self) -> String {
        format!(
            "{}.{}.{}",
            self.need.unwrap_or("-"),
            self.sev.unwrap_or("-"),
            self.held.unwrap_or("-")
        )
    }

    /// The tuple a carry's token names, each word one its list holds (`-`:
    /// none); `None` for a token that does not read.
    fn read(token: &str) -> Option<Self> {
        let mut it = token.split('.');
        let word = |raw: Option<&str>, words: &[&'static str]| match raw? {
            "-" => Some(None),
            w => carried_word(words, w).map(Some),
        };
        let need = word(it.next(), aterm_slog::vocab::NEED_WORDS)?;
        let sev = word(it.next(), aterm_slog::vocab::SEV_WORDS)?;
        let held = word(it.next(), aterm_slog::vocab::HELD_WORDS)?;
        it.next()
            .is_none()
            .then_some(ShownTuple { need, sev, held })
    }
}

/// THE SHOWN MARKS' BUCKET (6F-9; rulings U26, U44): the tuple last taken
/// (written, or pending: the dedupe), the session's tokens, and the newest
/// tuple not yet written with how many it replaced. A move that finds no
/// token replaces the pending one, counted, and the pending tuple is
/// written as soon as a token frees, so the newest is never lost.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ShownMarks {
    last: ShownTuple,
    bucket: MarkBucket,
    pending: Option<(ShownTuple, u64)>,
}

/// What the engine says, read under the status sweep's terminal guard
/// ([`Self::of`]) and written once that guard is released.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EngineRead {
    /// The shell integration's posture.
    pub(crate) integration: aterm_core::terminal::ShellIntegrationPosture,
    /// Notifications asked for ([`aterm_core::terminal::Terminal::notify_total`]).
    pub(crate) notify_total: u64,
    /// Bells that fired ([`aterm_core::terminal::Terminal::bell_total`]).
    pub(crate) bell_total: u64,
    /// The OSC 9;4 progress.
    pub(crate) progress: Option<aterm_types::osc::TaskbarProgress>,
}

impl EngineRead {
    /// Read off `t`: four field loads.
    pub(crate) fn of(t: &aterm_core::terminal::Terminal) -> EngineRead {
        EngineRead {
            integration: t.shell_integration_posture(),
            notify_total: t.notify_total(),
            bell_total: t.bell_total(),
            progress: t.taskbar_progress(),
        }
    }
}

/// One published status transition, typed ([`crate::session_status::Status`]'s
/// words): the phase, the outcome with its code, the classifier's confidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StatusFacts {
    pub(crate) phase: &'static str,
    /// `ok`, `fail` or `signal`, and the exit code or signal number.
    pub(crate) out: Option<(&'static str, Option<i64>)>,
    pub(crate) conf: Conf,
}

/// What put a session's modes back ([`SessionLog::observe_modes`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModesFrom<'a> {
    /// A manual reset, by its source word (`manual_reset::ResetSource`'s
    /// `ctl|menu`).
    Reset(&'static str),
    /// A foreground handback: the group that lost the terminal, the one that
    /// holds it now, and the program the timeline names for the first.
    Handback {
        from: i32,
        to: i32,
        program: Option<&'a str>,
    },
}

/// What each kind of mark last wrote ([`SessionLog`]'s `observed`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Observed {
    integration: Option<&'static str>,
    /// The context left, in tenths.
    ctx: Option<u8>,
    goal: Option<bool>,
    /// The notify and bell totals last counted from (this process's engine:
    /// neither crosses an update, so the first read is a baseline).
    notify: Option<u64>,
    bell: Option<u64>,
    /// When a bell was last said (monotonic ms).
    bell_at: Option<u64>,
    /// The progress state and its tenth.
    progress: Option<(&'static str, Option<u8>)>,
    /// The cwd and the informative title last said, as fingerprints
    /// ([`name_fp`]; 0: cleared): the carry holds no text (ruling S5).
    cwd: Option<u64>,
    title: Option<u64>,
    /// Whether the effective attention owner was the supervisor's when a
    /// meta write last said (the `escalated` need's standing).
    escalated: Option<bool>,
    /// Each meta field's value as the log last said it, a fingerprint
    /// ([`name_fp`]; 0: unset), at most [`META_FIELDS_MAX`] fields.
    meta: Vec<(String, u64)>,
    /// The program marks' bucket (this process's: a successor starts a full
    /// one, and the carry writes any pending state first).
    program: ProgramMarks,
    /// The shown marks' tuple and bucket (6F-9): the tuple crosses an
    /// update (`obs_shown=`), the bucket is this process's.
    shown: ShownMarks,
}

/// The most meta fields [`Observed`] remembers: every user field and the
/// supervisor's claim (`SUPERVISOR_FIELD`).
const META_FIELDS_MAX: usize = crate::session_timeline::MetaField::ALL.len() + 1;

/// A meta field whose value is free text that can hold what must never
/// leave RAM (a box's command, a question's words, the screen's last line:
/// the supervisor's escalation): its mark says a fingerprint below the
/// content tier, and the text only at it (`attn_text=`; phase 6, 6F-8,
/// ruling U13: "never stored" became "never leaves RAM", the ring's own
/// exposure).
const FINGERPRINTED_META: &[&str] = &["attention"];

/// A title's fingerprint, its leading busy-spinner frame left out
/// ([`crate::toolbar::busy_spinner_title_parts`]): a move of the frame alone
/// (`⠋ Build` → `⠙ Build`) is no move. 0 for no title.
pub(crate) fn title_fp(title: &str) -> u64 {
    if title.is_empty() {
        return 0;
    }
    match crate::toolbar::busy_spinner_title_parts(title) {
        Some((_, rest)) => name_fp(Some(&format!("\u{1}{rest}"))),
        None => name_fp(Some(title)),
    }
}

/// The `state` word of a shell integration posture.
fn integration_word(p: aterm_core::terminal::ShellIntegrationPosture) -> &'static str {
    use aterm_core::terminal::ShellIntegrationPosture as P;
    match p {
        P::Off => "off",
        P::On => "on",
        P::Degraded => "degraded",
    }
}

/// The `state` word and percent of an OSC 9;4 progress (`none` for none).
fn progress_of(p: Option<aterm_types::osc::TaskbarProgress>) -> (&'static str, Option<u8>) {
    use aterm_types::osc::TaskbarProgress as P;
    match p {
        None | Some(P::Hidden) => ("none", None),
        Some(P::Normal(n)) => ("normal", Some(n)),
        Some(P::Error(n)) => ("error", Some(n)),
        Some(P::Indeterminate) => ("indeterminate", None),
        Some(P::Paused(n)) => ("paused", Some(n)),
        // A state a later engine names that this build does not know.
        Some(_) => ("unknown", None),
    }
}

/// A word of `words` a carried token names, as the `&'static` word.
fn carried_word(words: &[&'static str], raw: &str) -> Option<&'static str> {
    words.iter().copied().find(|w| *w == raw)
}

const INTEGRATION_WORDS: &[&str] = &["off", "on", "degraded"];
const PROGRESS_WORDS: &[&str] = &[
    "none",
    "normal",
    "error",
    "indeterminate",
    "paused",
    "unknown",
];

impl Observed {
    /// The carry's tokens for the `state` host line (additive: a reader
    /// without them reads the line as it did): what a successor must not
    /// restate. Its one caller, the carry's `host_lines`, is Unix's and the
    /// tests' (the seamless update's handoff), so it carries that gate.
    #[cfg(any(unix, test))]
    pub(crate) fn carry_tokens(&self) -> String {
        let mut s = String::new();
        if let Some(w) = self.integration {
            s.push_str(&format!(" obs_int={w}"));
        }
        if let Some(d) = self.ctx {
            s.push_str(&format!(" obs_ctx={d}"));
        }
        if let Some(g) = self.goal {
            s.push_str(&format!(" obs_goal={}", u8::from(g)));
        }
        if let Some((w, d)) = self.progress {
            match d {
                Some(d) => s.push_str(&format!(" obs_prog={w}.{d}")),
                None => s.push_str(&format!(" obs_prog={w}")),
            }
        }
        if let Some(fp) = self.cwd {
            s.push_str(&format!(" cwd_fp={fp:x}"));
        }
        if let Some(fp) = self.title {
            s.push_str(&format!(" title_fp={fp:x}"));
        }
        if let Some(e) = self.escalated {
            s.push_str(&format!(" obs_esc={}", u8::from(e)));
        }
        if !self.meta.is_empty() {
            let fields: Vec<String> = self
                .meta
                .iter()
                .map(|(f, fp)| format!("{f}.{fp:x}"))
                .collect();
            s.push_str(&format!(" meta_fp={}", fields.join(",")));
        }
        if self.shown.last != ShownTuple::default() {
            s.push_str(&format!(" obs_shown={}", self.shown.last.token()));
        }
        s
    }

    /// Read back the tokens of [`Self::carry_tokens`] off a `state` line;
    /// what is absent or unreadable stays unknown.
    pub(crate) fn read_carry(&mut self, kv: impl Fn(&str) -> Option<String>) {
        self.integration = kv("obs_int").and_then(|w| carried_word(INTEGRATION_WORDS, &w));
        self.ctx = kv("obs_ctx")
            .and_then(|d| d.parse().ok())
            .filter(|d| *d <= 10);
        let flag = |k: &str| {
            kv(k).and_then(|g| match g.as_str() {
                "0" => Some(false),
                "1" => Some(true),
                _ => None,
            })
        };
        self.goal = flag("obs_goal");
        self.escalated = flag("obs_esc");
        self.progress = kv("obs_prog").and_then(|v| {
            let (w, d) = match v.split_once('.') {
                Some((w, d)) => (w.to_string(), Some(d.parse::<u8>().ok()?)),
                None => (v, None),
            };
            Some((carried_word(PROGRESS_WORDS, &w)?, d))
        });
        let fp = |k: &str| kv(k).and_then(|h| u64::from_str_radix(&h, 16).ok());
        self.cwd = fp("cwd_fp");
        self.title = fp("title_fp");
        self.meta = kv("meta_fp").map_or_else(Vec::new, |v| {
            v.split(',')
                .filter_map(|pair| {
                    let (f, h) = pair.split_once('.')?;
                    BoundedToken::new(Key::Field, f)?;
                    Some((f.to_string(), u64::from_str_radix(h, 16).ok()?))
                })
                .take(META_FIELDS_MAX)
                .collect()
        });
        self.shown.last = kv("obs_shown")
            .and_then(|t| ShownTuple::read(&t))
            .unwrap_or_default();
    }

    /// Note that the log says meta field `field` holds the value of
    /// fingerprint `fp`: whether that moved it from what the log last said.
    fn meta_moved(&mut self, field: &str, fp: u64) -> bool {
        match self.meta.iter_mut().find(|(f, _)| f == field) {
            Some((_, held)) if *held == fp => false,
            Some((_, held)) => {
                *held = fp;
                true
            }
            None => {
                if self.meta.len() < META_FIELDS_MAX {
                    self.meta.push((field.to_string(), fp));
                }
                true
            }
        }
    }
}

impl SessionLog {
    /// The run in front: the agent's, else the program's, else the
    /// command's; the session when none is open.
    fn front_run(&self) -> Node {
        let open = |n: Node| self.log.is_open(n).then_some(n);
        self.agent
            .as_ref()
            .and_then(|a| open(a.node))
            .or_else(|| self.fg.as_ref().and_then(|f| open(f.node)))
            .or_else(|| self.cmd.as_ref().and_then(|c| open(c.node)))
            .unwrap_or(Node::SESSION)
    }

    /// The open agent run of Claude Code, the one reader whose screen facts
    /// the log keeps (plan §3.2; Codex is Phase 5).
    fn claude_run(&self) -> Option<Node> {
        self.agent
            .as_ref()
            .filter(|a| {
                self.log.is_open(a.node)
                    && match a.who {
                        aterm_phase::Program::Claude => true,
                        aterm_phase::Program::Codex
                        | aterm_phase::Program::Agy
                        | aterm_phase::Program::Muse
                        | aterm_phase::Program::Gemini
                        | aterm_phase::Program::Generic => false,
                    }
            })
            .map(|a| a.node)
    }

    fn put(&mut self, kind: Kind, src: Src, conf: Conf, node: Node, f: Fields, at: Stamp) {
        let _ = self
            .log
            .mark(Mark::new(kind, src, conf).at(node).fields(f), at);
    }

    /// THE ENGINE'S MARKS, from one read under the sweep's guard: the
    /// integration posture on a move, the notifications and bells counted
    /// since the last (bells at most once a second), the progress on a
    /// state change or each ten points.
    pub(crate) fn observe_engine(&mut self, e: &EngineRead, at: Stamp) {
        let integration = integration_word(e.integration);
        if self.observed.integration != Some(integration) {
            self.observed.integration = Some(integration);
            let f = with(Fields::new(), Key::State, Some(Val::Word(integration)));
            self.put(
                Kind::Integration,
                Src::Engine,
                Conf::Exact,
                Node::SESSION,
                f,
                at,
            );
        }
        // The first read in a process is the baseline: what came before it
        // was another engine's, or said by the process that counted it.
        match self.observed.notify {
            Some(base) if e.notify_total > base => {
                let f = with(Fields::new(), Key::N, Some(Val::U(e.notify_total - base)));
                self.put(Kind::Notify, Src::Engine, Conf::Exact, Node::SESSION, f, at);
                self.observed.notify = Some(e.notify_total);
            }
            Some(base) if e.notify_total >= base => {}
            _ => self.observed.notify = Some(e.notify_total),
        }
        match self.observed.bell {
            Some(base) if e.bell_total > base => {
                let due = self
                    .observed
                    .bell_at
                    .is_none_or(|t| at.mono_ms.saturating_sub(t) >= BELL_EVERY_MS);
                if due {
                    let f = with(Fields::new(), Key::N, Some(Val::U(e.bell_total - base)));
                    self.put(Kind::Bell, Src::Engine, Conf::Exact, Node::SESSION, f, at);
                    self.observed.bell = Some(e.bell_total);
                    self.observed.bell_at = Some(at.mono_ms);
                }
            }
            Some(base) if e.bell_total >= base => {}
            _ => self.observed.bell = Some(e.bell_total),
        }
        let (word, pct) = progress_of(e.progress);
        let now = (word, pct.map(|p| p / 10));
        match self.observed.progress {
            Some(held) if held == now => {}
            // A session that never showed progress says nothing.
            None if word == "none" => self.observed.progress = Some(now),
            _ => {
                self.observed.progress = Some(now);
                let f = with(Fields::new(), Key::State, Some(Val::Word(word)));
                let f = with(f, Key::Pct, pct.map(|p| Val::U(u64::from(p))));
                let run = self.front_run();
                self.put(Kind::Progress, Src::Engine, Conf::Exact, run, f, at);
            }
        }
    }

    /// `mark <run> program state= [ask=] [coalesced=]`: one move of the
    /// program's own report as the status sweep's gate showed it (`None`:
    /// the shown report went away), through the run's bucket (the module
    /// doc). A move in front of another run first writes the last run's
    /// pending state (a run's change is no flap), and the new run's bucket
    /// starts full.
    pub(crate) fn observe_program(
        &mut self,
        shown: Option<aterm_core::terminal::program_state::ProgramState>,
        at: Stamp,
    ) {
        let words = ProgramWords::of(shown);
        let run = self.front_run();
        if self.observed.program.run != Some(run) {
            self.write_program(at, true);
            self.observed.program = ProgramMarks {
                run: Some(run),
                bucket: MarkBucket::full(at.mono_ms),
                pending: None,
            };
        }
        let p = &mut self.observed.program;
        p.pending = Some(match p.pending {
            Some((_, replaced)) => (words, replaced.saturating_add(1)),
            None => (words, 0),
        });
        self.write_program(at, false);
    }

    /// THE PROGRAM BUCKET'S TICK, each status sweep: a pending state is
    /// written once its run's bucket has a token again.
    pub(crate) fn program_tick(&mut self, at: Stamp) {
        self.write_program(at, false);
    }

    /// Write the pending program state now, whatever the bucket holds: the
    /// session closes, or its log is carried across an update.
    pub(crate) fn flush_program(&mut self, at: Stamp) {
        self.write_program(at, true);
    }

    /// The run `closing` is about to close: when the bucket's run is that
    /// run or lies under it, its pending state is written on it now,
    /// whatever the bucket holds ([`SessionLog::close_node`], every run
    /// close's one path).
    pub(super) fn settle_program(&mut self, closing: Node, at: Stamp) {
        let Some(run) = self.observed.program.run else {
            return;
        };
        if run == closing || self.descends(run, closing) {
            self.write_program(at, true);
        }
    }

    /// Refill the bucket to `at`, and write the pending state with a token
    /// (or without one, `force`), on the bucket's run while it is open and
    /// on the session (`-`) once it is not: never on whatever node is
    /// deepest open ([`aterm_slog::Log::mark`]'s rule for a mark that names
    /// none).
    fn write_program(&mut self, at: Stamp, force: bool) {
        let open = self.observed.program.run.filter(|n| self.log.is_open(*n));
        let p = &mut self.observed.program;
        let Some((words, replaced)) = p.pending else {
            return;
        };
        if !p.bucket.spend(at.mono_ms, force) {
            return;
        }
        p.pending = None;
        let node = open.unwrap_or(Node::SESSION);
        let f = with(Fields::new(), Key::State, Some(Val::Word(words.state)));
        let f = with(f, Key::Ask, words.ask.map(Val::Word));
        let f = with(
            f,
            Key::Coalesced,
            (replaced > 0).then_some(Val::U(replaced)),
        );
        self.put(Kind::Program, Src::Engine, Conf::Claim, node, f, at);
    }

    /// `mark - shown [need= sev=] [held=] [coalesced=]`: what the person is
    /// shown of the session's need moved to `tuple` (the module doc), from
    /// the one verdict writer
    /// ([`crate::session_timeline::SessionTimeline::set_need`]), through
    /// the session's bucket. A publication that moves only what the tuple
    /// does not print writes nothing.
    pub(crate) fn observe_shown(&mut self, tuple: ShownTuple, at: Stamp) {
        let s = &mut self.observed.shown;
        if s.last == tuple {
            return;
        }
        s.last = tuple;
        s.pending = Some(match s.pending {
            Some((_, replaced)) => (tuple, replaced.saturating_add(1)),
            None => (tuple, 0),
        });
        self.write_shown(at, false);
    }

    /// THE SHOWN BUCKET'S TICK, each status sweep: a pending tuple is
    /// written once the session's bucket has a token again.
    pub(crate) fn shown_tick(&mut self, at: Stamp) {
        self.write_shown(at, false);
    }

    /// Write the pending shown tuple now, whatever the bucket holds: the
    /// session closes, or its log is carried across an update.
    pub(crate) fn flush_shown(&mut self, at: Stamp) {
        self.write_shown(at, true);
    }

    /// Refill the bucket to `at`, and write the pending tuple with a token
    /// (or without one, `force`), on the session, by aterm.
    fn write_shown(&mut self, at: Stamp, force: bool) {
        let s = &mut self.observed.shown;
        let Some((tuple, replaced)) = s.pending else {
            return;
        };
        if !s.bucket.spend(at.mono_ms, force) {
            return;
        }
        s.pending = None;
        let f = with(Fields::new(), Key::Need, tuple.need.map(Val::Word));
        let f = with(f, Key::Sev, tuple.sev.map(Val::Word));
        let f = with(f, Key::Held, tuple.held.map(Val::Word));
        let f = with(
            f,
            Key::Coalesced,
            (replaced > 0).then_some(Val::U(replaced)),
        );
        let _ = self.log.mark(
            Mark::new(Kind::Shown, Src::Aterm, Conf::Strong)
                .at(Node::SESSION)
                .by(aterm_slog::Actor::Aterm)
                .fields(f),
            at,
        );
    }

    /// `mark <run> status …`: one published status transition (the status
    /// sweep dedups transitions; every call writes).
    pub(crate) fn observe_status(&mut self, s: StatusFacts, at: Stamp) {
        let mut f = with(Fields::new(), Key::Phase, Some(Val::Word(s.phase)));
        if let Some((out, code)) = s.out {
            f = with(f, Key::Out, Some(Val::Word(out)));
            f = with(f, Key::Code, code.map(Val::I));
        }
        let run = self.front_run();
        self.put(Kind::Status, Src::Status, s.conf, run, f, at);
    }

    /// The screen saw a person stop the turn (Esc): its idle close reads
    /// `out=interrupted` (called before the verdict is published).
    pub(crate) fn agent_interrupted(&mut self) {
        if let Some(a) = self.agent.as_mut() {
            a.tracker.note_interrupted();
        }
    }

    /// Claude Code's context left (`pct`, when its screen shows it) and its
    /// goal (`None`: not read), after the verdict is published: `mark <run>
    /// ctx pct=` when the tenth moves, `mark <run> goal state=` when the
    /// goal does (a first `off` is no news).
    pub(crate) fn observe_screen_facts(
        &mut self,
        ctx_pct: Option<u8>,
        goal: Option<bool>,
        at: Stamp,
    ) {
        let Some(run) = self.claude_run() else {
            return;
        };
        if let Some(pct) = ctx_pct {
            let tenth = pct.min(100) / 10;
            if self.observed.ctx != Some(tenth) {
                self.observed.ctx = Some(tenth);
                let f = with(Fields::new(), Key::Pct, Some(Val::U(u64::from(pct))));
                self.put(Kind::Ctx, Src::Screen, Conf::Strong, run, f, at);
            }
        }
        if let Some(goal) = goal
            && self.observed.goal != Some(goal)
        {
            let first = self.observed.goal.is_none();
            self.observed.goal = Some(goal);
            if goal || !first {
                let word = if goal { "on" } else { "off" };
                let f = with(Fields::new(), Key::State, Some(Val::Word(word)));
                self.put(Kind::Goal, Src::Screen, Conf::Strong, run, f, at);
            }
        }
    }

    /// THE LAUNCH CARD's names, kept anew for the process in front
    /// (`SessionTimeline::note_claude_card`): an amend of the agent run with
    /// what it does not already hold at least as surely.
    pub(crate) fn observe_card(&mut self, card: &aterm_phase::LaunchCard, at: Stamp) {
        let Some(run) = self.claude_run() else {
            return;
        };
        let held = self
            .log
            .state()
            .run()
            .filter(|o| o.node == run)
            .map(|o| o.f.clone())
            .unwrap_or_default();
        let mut f = Fields::new();
        for (k, v) in [
            (Key::Ver, Some(card.version.as_str())),
            (Key::Model, Some(card.model.as_str())),
            (Key::Effort, card.effort.as_deref()),
        ] {
            if let Some(t) = v.and_then(|v| BoundedToken::new(k, v))
                && held.str(k) != Some(t.as_str())
            {
                let _ = f.set(k, Val::Tok(t));
            }
        }
        if f.iter().next().is_some() {
            let _ = self.log.amend(
                run,
                Amend::new(Kind::Agent, Src::Screen, Conf::Strong, f),
                at,
            );
        }
    }

    /// `mark - render state=desync-risk|healed …`: the resize ledger's
    /// verdict moved (`SessionTimeline::record_render`), one mark for each
    /// transition its row records, with every field of the row (phase 6,
    /// 6F-7): a run's `run= displaced= net= at=` or `run= after_ms=`, an
    /// epoch's `resize_epoch= displaced= pushed= cut= drew= class= at=` or
    /// `resize_epoch= after_ms= healed_by=` (none for an open epoch's `-`).
    pub(crate) fn observe_render(&mut self, t: &crate::resize_ledger::Transition, at: Stamp) {
        use crate::resize_ledger::Transition as T;
        let u = |f: Fields, k: Key, n: u64| with(f, k, Some(Val::U(n)));
        let i = |f: Fields, k: Key, n: i32| with(f, k, Some(Val::I(i64::from(n))));
        let f = with(Fields::new(), Key::State, Some(Val::Word(t.state_word())));
        let f = match *t {
            T::Risk {
                run,
                displaced,
                net_zero,
                at_ms,
            } => {
                let f = i(u(f, Key::Run, run), Key::Displaced, displaced);
                let net = if net_zero { "zero" } else { "changed" };
                u(with(f, Key::Net, Some(Val::Word(net))), Key::At, at_ms)
            }
            T::Healed { run, after_ms } => u(u(f, Key::Run, run), Key::AfterMs, after_ms),
            T::EpochRisk {
                epoch,
                shift,
                pushed,
                cut,
                drew,
                class,
                at_ms,
            } => {
                let f = i(u(f, Key::ResizeEpoch, epoch), Key::Displaced, shift);
                let f = u(i(f, Key::Pushed, pushed), Key::Cut, u64::from(cut));
                let f = u(f, Key::Drew, u64::from(drew));
                let f = f.clone().word(Key::Class, class.wire()).unwrap_or(f);
                u(f, Key::At, at_ms)
            }
            T::EpochHealed {
                epoch,
                after_ms,
                by,
            } => {
                let f = u(u(f, Key::ResizeEpoch, epoch), Key::AfterMs, after_ms);
                f.clone().word(Key::HealedBy, by.wire()).unwrap_or(f)
            }
        };
        self.put(Kind::Render, Src::Aterm, Conf::Exact, Node::SESSION, f, at);
    }

    /// `mark - cwd cwd=<path>`: the OSC 7 directory the drift flush read
    /// (`SessionTimeline::record_cwd_change`), when it moved; a cleared one
    /// is the mark with no `cwd=`, and the first read of a session with no
    /// directory is no news (as a title's is). Returns the directory the
    /// mark holds (empty: none) when it was written: the text the
    /// timeline's `cwd-change` row prints, so the row is its record's
    /// (phase 6, 6F-8, ruling U13), on the record's trigger too — a
    /// successor that carried the fingerprint restates neither.
    pub(crate) fn observe_cwd(&mut self, cwd: Option<&str>, at: Stamp) -> Option<String> {
        let cwd = cwd.filter(|c| !c.is_empty());
        let fp = name_fp(cwd);
        if self.observed.cwd == Some(fp) {
            return None;
        }
        let first = self.observed.cwd.is_none();
        self.observed.cwd = Some(fp);
        if first && cwd.is_none() {
            return None;
        }
        let mut f = Fields::new();
        let text = cwd.and_then(|c| BoundedText::new(Key::Cwd, c));
        let held = text
            .as_ref()
            .map_or_else(String::new, |t| t.as_str().to_string());
        if let Some(t) = text {
            let _ = f.set(Key::Cwd, Val::Text(t));
        }
        self.put(Kind::Cwd, Src::Engine, Conf::Exact, Node::SESSION, f, at);
        Some(held)
    }

    /// `mark - title title=<informative title>`: the drift flush's title
    /// ([`crate::tab_label::informative`]) moved, a spinner frame's move
    /// aside ([`title_fp`]). The title is content tier: `log` prints it
    /// only at `tier=content`, and neither the journal nor the carry holds
    /// it. Returns the title the mark holds (empty: cleared) when it was
    /// written: the `title-change` row's text, which since phase 6 (6F-8,
    /// ruling U13) follows this mark, its cadence and its words.
    pub(crate) fn observe_title(&mut self, title: &str, at: Stamp) -> Option<String> {
        let fp = title_fp(title);
        if self.observed.title == Some(fp) {
            return None;
        }
        // The first read of a session with no title is no news.
        let first = self.observed.title.is_none();
        self.observed.title = Some(fp);
        if first && fp == 0 {
            return None;
        }
        let mut f = Fields::new();
        let text = BoundedText::new(Key::Title, title);
        let held = text
            .as_ref()
            .map_or_else(String::new, |t| t.as_str().to_string());
        if let Some(t) = text {
            let _ = f.set(Key::Title, Val::Text(t));
        }
        self.put(Kind::Title, Src::Engine, Conf::Exact, Node::SESSION, f, at);
        Some(held)
    }

    /// `mark - modes n=<k> …`: a foreground handback or a manual reset put
    /// `reverted` modes back (`by=human` for Edit ▸ Reset Terminal), one
    /// mark for every `modes-restored` row, nothing reverted too, with every
    /// field of the row (phase 6, 6F-7): a reset's `source=ctl|menu`, or a
    /// handback's foreground groups `from_pg= to_pg=` (host tier) and the
    /// program that lost the terminal (`prog=`, when the timeline names
    /// one); then the engine's mode names (`reverted=alt,kitty`, none for
    /// none) and the bytes it wrote (`bytes=`).
    pub(crate) fn observe_modes(
        &mut self,
        from: ModesFrom<'_>,
        reverted: &[&str],
        bytes: usize,
        by: aterm_slog::Actor,
        at: Stamp,
    ) {
        let count = |n: usize| Some(Val::U(u64::try_from(n).unwrap_or(u64::MAX)));
        let f = with(Fields::new(), Key::N, count(reverted.len()));
        let f = match from {
            ModesFrom::Reset(source) => f.clone().word(Key::Source, source).unwrap_or(f),
            ModesFrom::Handback { from, to, program } => {
                let f = with(f, Key::FromPg, Some(Val::I(i64::from(from))));
                let f = with(f, Key::ToPg, Some(Val::I(i64::from(to))));
                with(f, Key::Prog, program.and_then(|p| tok(Key::Prog, p)))
            }
        };
        let modes = reverted.join(",");
        let f = with(f, Key::Reverted, tok(Key::Reverted, &modes));
        let f = with(f, Key::Bytes, count(bytes));
        let _ = self.log.mark(
            Mark::new(Kind::Modes, Src::Engine, Conf::Exact)
                .at(Node::SESSION)
                .by(by)
                .fields(f),
            at,
        );
    }

    /// THE FABRIC'S MARKS ([`crate::fabric::FabricFact`], through the one
    /// hook `fabric_bind` implements, in the timeline row's hold): `mail
    /// dir=in|out`, `mail-seen`, `mail-landed` and `hold`, and since phase 6
    /// (6F-5) `topic change=add|drop fp=`, `advisory origin= gen=` and
    /// `mail-fetch off=`; since 6F-6 each with every field of its row (a
    /// post's `re= dl=` and, host tier, `via= idem=`; every `inbox seen`
    /// with `moved=`; a hold's `origin= hold_origin=`). Ids, words and
    /// offsets — a body is never in a fact, so it is in no record; a topic's
    /// name and `since=` and a hold's or an advisory's reason are content
    /// tier, RAM only (ruling U11).
    pub(crate) fn observe_fabric(&mut self, fact: crate::fabric::FabricFact<'_>, at: Stamp) {
        use crate::fabric::FabricFact as F;
        // A person's key the timeline's hold kept from the log stands
        // before the mail (ruling T2).
        self.reconcile_person(at);
        // A word outside the key's list is `unknown`, and a key with no
        // `unknown` is left out: the fields already set stay.
        let word = |f: Fields, k: Key, w: &str| {
            let kept = f.clone();
            f.word(k, w)
                .or_else(|_| kept.clone().word(k, "unknown"))
                .unwrap_or(kept)
        };
        let text = |mut f: Fields, k: Key, t: &str| {
            if let Some(t) = BoundedText::new(k, t) {
                let _ = f.set(k, Val::Text(t));
            }
            f
        };
        let some_text = |f: Fields, k: Key, t: Option<&str>| match t {
            Some(t) => text(f, k, t),
            None => f,
        };
        let (kind, by, f) = match fact {
            F::MailIn {
                kind,
                from,
                off,
                inbox,
            } => {
                let f = word(Fields::new(), Key::Dir, "in");
                let f = word(f, Key::MailKind, kind);
                let f = text(f, Key::From, from);
                let f = with(f, Key::Off, Some(Val::U(off)));
                let f = with(f, Key::Inbox, Some(Val::U(inbox)));
                (Kind::Mail, aterm_slog::Actor::Bridge, f)
            }
            // The post's options (6F-6): `re=`, `dl=`, and host tier, never
            // on the bus, its relay hops and the caller's key (`idem=`, the
            // row's `key=`). A broadcast's topic is a fingerprint wherever
            // its record leaves RAM (U11): `to=say:<16 hex>`, the name at
            // the content tier.
            F::MailOut {
                kind,
                to,
                post,
                re,
                dl,
                via,
                idem,
            } => {
                let f = word(Fields::new(), Key::Dir, "out");
                let f = word(f, Key::MailKind, kind);
                let f = match to.strip_prefix("say:") {
                    Some(topic) => {
                        let fp = format!("say:{:016x}", name_fp(Some(topic)));
                        text(text(f, Key::To, &fp), Key::TopicName, topic)
                    }
                    None => text(f, Key::To, to),
                };
                let f = with(f, Key::Inbox, Some(Val::U(post)));
                let f = with(
                    f,
                    Key::Re,
                    re.and_then(|r| aterm_slog::ShortId::new(&r.to_string()))
                        .map(Val::Id),
                );
                let f = with(f, Key::Dl, dl.map(Val::U));
                let f = some_text(f, Key::Via, via);
                let f = some_text(f, Key::Idem, idem);
                (Kind::Mail, aterm_slog::Actor::Nobody, f)
            }
            // Every `inbox seen` (6F-6), `moved=0` when it moved nothing:
            // the fold folds only moves. A decided verdict names the row's
            // kind and sender, as the row does.
            F::MailSeen {
                inbox,
                off,
                floor,
                seen,
                verdict,
                kind,
                from,
                moved,
            } => {
                let f = with(Fields::new(), Key::Inbox, Some(Val::U(inbox)));
                let f = with(f, Key::Off, Some(Val::U(off)));
                let f = with(f, Key::Floor, floor.map(Val::U));
                let f = with(f, Key::Seen, Some(Val::U(seen)));
                let f = with(f, Key::Moved, Some(Val::U(u64::from(moved))));
                let f = match verdict {
                    Some(v) => text(
                        word(word(f, Key::Word, v), Key::MailKind, kind),
                        Key::From,
                        from,
                    ),
                    None => f,
                };
                (Kind::MailSeen, aterm_slog::Actor::Nobody, f)
            }
            // A landing the bridge reported (`off=`, or none: `off=-`), or a
            // post aterm retired, landed nowhere, and why (6F-6).
            F::MailLanded { post, off, why } => {
                let f = with(Fields::new(), Key::Inbox, Some(Val::U(post)));
                let f = with(f, Key::Off, off.map(Val::U));
                let (f, by) = match why {
                    Some(w) => (word(f, Key::Why, w), aterm_slog::Actor::Aterm),
                    None => (f, aterm_slog::Actor::Bridge),
                };
                (Kind::MailLanded, by, f)
            }
            // The hold's row, whole (6F-6): its origin, the origins standing,
            // whether it was carried in, and its reason at the content tier
            // (a person's text: never journaled, carried or published).
            F::Hold {
                held,
                bridge,
                origin,
                hold_origin,
                reason,
                carried,
            } => {
                let f = word(Fields::new(), Key::State, if held { "on" } else { "off" });
                let mut f = word(f, Key::Origin, origin);
                if let Some(t) = (hold_origin != "-")
                    .then(|| BoundedToken::new(Key::HoldOrigin, hold_origin))
                    .flatten()
                {
                    let _ = f.set(Key::HoldOrigin, Val::Tok(t));
                }
                let f = some_text(f, Key::Reason, reason);
                let f = with(f, Key::Carried, carried.then_some(Val::U(1)));
                let by = if bridge {
                    aterm_slog::Actor::Bridge
                } else {
                    aterm_slog::Actor::Nobody
                };
                (Kind::Hold, by, f)
            }
            // A topic is a fingerprint wherever its record leaves RAM
            // (ruling U11): the name and its `since=` are content tier.
            F::Topic { add, name, since } => {
                let f = word(Fields::new(), Key::Change, if add { "add" } else { "drop" });
                let f = with(
                    f,
                    Key::Fp,
                    aterm_slog::ShortId::new(&format!("{:016x}", name_fp(Some(name)))).map(Val::Id),
                );
                let f = text(f, Key::TopicName, name);
                let f = match since {
                    Some(s) => text(f, Key::Since, s),
                    None => f,
                };
                (Kind::Topic, aterm_slog::Actor::Nobody, f)
            }
            // What a lost bridge left: aterm's advisory, the reason content
            // tier as a hold's is.
            F::Advisory {
                origin,
                reason,
                generation,
            } => {
                let f = word(Fields::new(), Key::Origin, origin);
                let f = with(
                    f,
                    Key::Gen,
                    aterm_slog::ShortId::new(&generation.to_string()).map(Val::Id),
                );
                let f = text(f, Key::Reason, reason);
                (Kind::Advisory, aterm_slog::Actor::Aterm, f)
            }
            F::Fetch { off } => {
                let f = with(Fields::new(), Key::Off, Some(Val::U(off)));
                (Kind::MailFetch, aterm_slog::Actor::Nobody, f)
            }
        };
        // A `mail dir=in` mark sets the fold's mail latch as it folds: the
        // person's next input is marked (rulings T39, U19). A post's relay
        // hops are the one field whose grammar's bound the record's others
        // can leave no room for (`via=` admits 560 bytes beside a 70-byte
        // `to=`, a 64-byte `idem=` and three numbers): the mark is cut there,
        // never refused (the review of 6F-6). The row's 256-byte payload
        // shows less of the hops than the record keeps.
        let elastic = (kind == Kind::Mail).then_some(Key::Via);
        let mark = |f: Fields| {
            Mark::new(kind, Src::Fabric, Conf::Exact)
                .at(Node::SESSION)
                .by(by)
                .fields(f)
        };
        let _ = self.mark_within_record(mark, f, elastic, at);
    }

    /// `mark(f)` appended; when its record would pass
    /// [`aterm_slog::MAX_RECORD_BYTES`], the free-text key `elastic` is cut
    /// by the excess ([`aterm_slog::field::clamp_encoded`], never inside a
    /// character) and the mark appended once more, so a record its writer
    /// owes is never refused for one long field. Returns the seq.
    fn mark_within_record(
        &mut self,
        mark: impl Fn(Fields) -> Mark,
        f: Fields,
        elastic: Option<Key>,
        at: Stamp,
    ) -> Option<u64> {
        let over = match self.log.mark(mark(f.clone()), at) {
            Ok(seq) => return Some(seq),
            Err(aterm_slog::Refused::Encode(aterm_slog::EncodeError::TooLarge(n))) => {
                n.saturating_sub(aterm_slog::MAX_RECORD_BYTES)
            }
            Err(_) => return None,
        };
        let key = elastic?;
        let text = f.get_str(key)?;
        let room = aterm_slog::field::encoded_len(text).checked_sub(over)?;
        let kept = aterm_slog::field::clamp_encoded(text, room).to_string();
        let mut f = f;
        match BoundedText::new(key, &kept) {
            Some(t) => {
                let _ = f.set(key, Val::Text(t));
            }
            None => f.remove(key),
        }
        self.log.mark(mark(f), at).ok()
    }

    /// `mark - meta field=<f> value=<v>` by its writer (`value=` absent for
    /// a key unset; host tier; `fp=` its fingerprint in place of the text
    /// for a [`FINGERPRINTED_META`] field, the text beside it at the content
    /// tier, `attn_text=`, RAM only), from the one `meta-change` writer
    /// ([`crate::session_timeline::SessionTimeline::record_meta_change`]):
    /// ONE MARK FOR EVERY ROW (phase 6, 6F-8, ruling U13), with every field
    /// the row prints (the Autopilot pair's timer, `land_at=`), so the
    /// `meta-change` row is its record's. A value this log last said of the
    /// field (a carried log's: a successor restoring what it carried) is
    /// no news (ruling S5): its mark says `carried=1`, as a carried
    /// session's `state` and `hold` marks do. And the `escalated` need that
    /// write moves (addendum A16):
    /// `escalated` is whether the effective attention owner is now the
    /// supervisor's, and on a move it is `need` (raised) or `clear`
    /// (lifted) `word=escalated src=meta conf=strong`, always `by=aterm`,
    /// whoever wrote — so a second writer raises no second need, and
    /// aterm's own seed, supersede or restore can lift what any writer's
    /// write raised (a need is its raiser's, `fold::clear_lifts`).
    pub(crate) fn observe_meta(
        &mut self,
        change: crate::session_timeline::MetaChange<'_>,
        escalated: bool,
        at: Stamp,
    ) {
        let land_at = change.land_at.map(|ms| ms.div_euclid(1000));
        // The timer is the pair's: a move of it alone is a move.
        let fp = match land_at {
            None => name_fp(change.value),
            Some(s) => name_fp(Some(&format!("{}\u{0}{s}", change.value.unwrap_or("")))),
        };
        let moved = self.observed.meta_moved(change.field, fp);
        let mut f = Fields::new();
        if let Some(t) = BoundedToken::new(Key::Field, change.field) {
            let _ = f.set(Key::Field, Val::Tok(t));
        }
        if FINGERPRINTED_META.contains(&change.field) {
            if let Some(v) = change.value {
                let shown = name_fp(Some(v));
                if let Some(id) = aterm_slog::ShortId::new(&format!("{shown:016x}")) {
                    let _ = f.set(Key::Fp, Val::Id(id));
                }
                if let Some(t) = BoundedText::new(Key::AttnText, v) {
                    let _ = f.set(Key::AttnText, Val::Text(t));
                }
            }
        } else if let Some(t) = change.value.and_then(|v| BoundedText::new(Key::Value, v)) {
            let _ = f.set(Key::Value, Val::Text(t));
        }
        let f = with(f, Key::LandAt, land_at.map(Val::I));
        let f = with(f, Key::Carried, (!moved).then_some(Val::U(1)));
        let _ = self.log.mark(
            Mark::new(Kind::Meta, Src::Meta, Conf::Exact)
                .at(Node::SESSION)
                .by(change.by)
                .fields(f),
            at,
        );
        // The first write to a session that is not escalated is no news.
        let was = self.observed.escalated.replace(escalated).unwrap_or(false);
        if was == escalated {
            return;
        }
        let kind = if escalated { Kind::Need } else { Kind::Clear };
        let f = Fields::new()
            .word(Key::Word, "escalated")
            .unwrap_or_default();
        let _ = self.log.mark(
            Mark::new(kind, Src::Meta, Conf::Strong)
                .at(Node::SESSION)
                .by(aterm_slog::Actor::Aterm)
                .fields(f),
            at,
        );
    }

    /// A WORKER'S TASK MOVED (plan 5C-3; ruling T21): `mark - task rev=<n>
    /// state=<working|done|exited|stalled> attention=<0|1> [why=<w>]
    /// [code=<exit>] src=aterm conf=exact`, the fact of the timeline's
    /// `task-change` row ([`crate::task_record::TaskRecord::event_payload`]),
    /// written in the hold that records the row. Closed words and counters
    /// only: never the result's path, size or text, and never who started it.
    /// Local: never bus class, so no fleet reader learns it.
    pub(crate) fn observe_task(&mut self, task: &crate::task_record::TaskRecord, at: Stamp) {
        let f = Fields::new();
        let f = with(f, Key::Rev, Some(Val::U(task.rev)));
        let f = with(
            f,
            Key::State,
            aterm_slog::vocab::lookup(aterm_slog::vocab::STATE_WORDS, task.state.as_str())
                .map(Val::Word),
        );
        let f = with(f, Key::Attention, Some(Val::U(u64::from(task.attention))));
        let f = with(
            f,
            Key::Why,
            task.why
                .and_then(|w| aterm_slog::vocab::lookup(aterm_slog::vocab::WHY_WORDS, w))
                .map(Val::Word),
        );
        let f = with(f, Key::Code, task.exit.map(|c| Val::I(i64::from(c))));
        self.put(Kind::Task, Src::Aterm, Conf::Exact, Node::SESSION, f, at);
    }

    /// `mark - story word=<verb>`: what `aterm ctl story` told the window
    /// ([`aterm_messages::presence::StoryVerb::word`]) by its teller (the
    /// wire's `log_actor`), never its text.
    pub(crate) fn observe_story(&mut self, verb: &str, by: aterm_slog::Actor, at: Stamp) {
        let f = Fields::new()
            .word(Key::Word, verb)
            .or_else(|_| Fields::new().word(Key::Word, "unknown"))
            .unwrap_or_default();
        let _ = self.log.mark(
            Mark::new(Kind::Story, Src::Ctl, Conf::Exact)
                .at(Node::SESSION)
                .by(by)
                .fields(f),
            at,
        );
    }

    /// `mark - usage h5=<pct> d7=<pct>`: a usage tap reading the harness
    /// worker took (`harness_host::log_usage`), the account's five-hour and
    /// seven-day windows as the vendor reported them — `by=vendor
    /// src=aterm conf=strong` (the owner's default 8: aterm read what the
    /// vendor said). A window the reading lacks has no key.
    pub(crate) fn observe_usage(&mut self, h5: Option<u32>, d7: Option<u32>, at: Stamp) {
        let f = with(Fields::new(), Key::H5, h5.map(|p| Val::U(u64::from(p))));
        let f = with(f, Key::D7, d7.map(|p| Val::U(u64::from(p))));
        let _ = self.log.mark(
            Mark::new(Kind::Usage, Src::Aterm, Conf::Strong)
                .at(Node::SESSION)
                .by(aterm_slog::Actor::Vendor)
                .fields(f),
            at,
        );
    }

    /// `mark - act word=relaunch rule=<row>`: a relaunch or upgrade row of
    /// the window's harness host that moved the session
    /// (`harness_host::log_act`, plan 5F-2, rulings T25 and T28) — `by=harness
    /// src=aterm conf=strong`: aterm saw its own harness act. `rule=` is the
    /// row's name ([`aterm_agent::harness::acts::relaunch_act`]); the row's
    /// pid, conversation and versions stay in the row. The turn the pass
    /// typed keeps its transport's `by=ctl`; this mark says the pass was the
    /// harness's. Counted in `brief`'s one `acts N` line, as every act is.
    ///
    /// Once the act is written, and only when the row's step brought a new
    /// agent process up (`started`,
    /// [`aterm_agent::harness::upgrade_drive::started`]: never a turn typed
    /// into the running agent, a stop or a wait), the failure the restart
    /// caused is lifted with `mark - seen by=harness` when it is the
    /// session's only one, an agent ran under its run, and the relaunched
    /// run is open ([`SessionLog::harness_saw_failure`], Phase 6 step 6C-1).
    pub(crate) fn observe_relaunch_act(&mut self, rule: &str, started: bool, at: Stamp) {
        let Ok(f) = Fields::new()
            .word(Key::Word, aterm_agent::harness::acts::RELAUNCH_WORD)
            .and_then(|f| f.id(Key::Rule, rule))
        else {
            return;
        };
        let acted = self
            .log
            .mark(
                Mark::new(Kind::Act, Src::Aterm, Conf::Strong)
                    .at(Node::SESSION)
                    .by(aterm_slog::Actor::Harness)
                    .fields(f),
                at,
            )
            .is_ok();
        if acted && started {
            self.harness_saw_failure(at);
        }
    }

    /// `mark - state state=<spawning|alive|exited>`: the registry moved the
    /// session's lifecycle state (`SessionStore::set_state`, `mark_alive`),
    /// the fact of the timeline's `state-change` row, in its hold (phase 6,
    /// 6F-5). The registration's state is the `spawned` row's, which the
    /// session open carries (6F-7; a carried session's, already open, is
    /// this mark with `carried=1`), and the close's `state=closed` row is
    /// the session close's: a mark after the close is refused.
    pub(crate) fn observe_state(&mut self, state: &str, at: Stamp) {
        let f = Fields::new()
            .word(Key::State, state)
            .or_else(|_| Fields::new().word(Key::State, "unknown"))
            .unwrap_or_default();
        self.put(Kind::State, Src::Aterm, Conf::Exact, Node::SESSION, f, at);
    }

    /// `mark - policy gen=<n>`: the harness policy cell moved to generation
    /// `generation` (`App::announce_harness_policy`), the fact of the
    /// `harness-policy gen=<n>` row it writes on every session, in its hold
    /// (phase 6, 6F-5; ruling U16). The node log's `config key=` stays.
    pub(crate) fn observe_policy(&mut self, generation: u64, at: Stamp) {
        let f = with(
            Fields::new(),
            Key::Gen,
            aterm_slog::ShortId::new(&generation.to_string()).map(Val::Id),
        );
        self.put(Kind::Policy, Src::Aterm, Conf::Exact, Node::SESSION, f, at);
    }

    /// `mark - in-doubt verb=<w> producer=<n> key_seq=<n> reply=<w-w>`: a
    /// keyed write whose outcome nobody knows (`pty_idem::record_in_doubt`),
    /// named by its verb (`unknown` for one the vocabulary does not hold)
    /// and its key's producer and sequence, never by its bytes; the first
    /// two words of its reply (`reply`, the `in-doubt` row's) at the content
    /// tier, RAM only (phase 6, 6F-7).
    pub(crate) fn observe_in_doubt(
        &mut self,
        verb: &str,
        producer: u64,
        key_seq: u64,
        reply: &str,
        at: Stamp,
    ) {
        let f = Fields::new()
            .word(Key::Verb, verb)
            .or_else(|_| Fields::new().word(Key::Verb, "unknown"))
            .unwrap_or_default();
        let f = with(f, Key::Producer, Some(Val::U(producer)));
        let f = with(f, Key::KeySeq, Some(Val::U(key_seq)));
        let f = with(
            f,
            Key::Reply,
            BoundedText::new(Key::Reply, reply).map(Val::Text),
        );
        self.put(Kind::InDoubt, Src::Aterm, Conf::Exact, Node::SESSION, f, at);
    }
}

#[cfg(test)]
#[path = "session_log_observed_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "session_log_overlap_tests.rs"]
pub(crate) mod overlap_tests;

#[cfg(test)]
#[path = "session_log_program_tests.rs"]
mod program_tests;

#[cfg(test)]
#[path = "session_log_shown_tests.rs"]
mod shown_tests;

#[cfg(test)]
#[path = "session_log_ring_facts_tests.rs"]
mod ring_facts_tests;
