// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A LOOP THAT FOLLOWS ANOTHER BEHAVES AS THE FIRST LOOP, AT EVERY HANDOFF
//! (the owner's rule for the successor's seed: a seamless update, a new
//! window, a policy reload or a fault restart starts a loop on the
//! session's ledger). For each scenario — a usage window, the weekly
//! window, a spend limit, a model bucket with and without the fallback's
//! relaunch, the done check, the take window — the FIRST loop runs a
//! scripted timeline on the fixed clock. At every instant a handoff could
//! fall (just after each event and act, just before each act, inside long
//! gaps) a SUCCESSOR is started on the first loop's ledger as it stood then
//! (its rows restamped on the test's clock, so every `ts` the seed reads is
//! the instant it was written), seeded as `drive_loop` seeds
//! ([`Session::seed_from_ledger`], [`Session::seed_wall_memory`]), and fed
//! the rest of the timeline — and at every handoff a CHAIN too: that
//! successor run to the next handoff, where a third loop attaches on its
//! books ([`run_chain`]). The successor's acts are matched to the first
//! loop's (the same text within 40 s); the first that differs is the
//! cell's, and it must be no sooner than the first loop's act — not by a
//! second — the same text, never one where the first loop types nothing, and
//! never a wait with nothing due where the first loop acts; later is
//! fail-closed only while bounded by a fresh appearance's first rung (30
//! minutes from attaching, a person's standing draft's grace on top:
//! [`LATER_BOUND_S`]). The world answers an act once it is typed: an answer
//! to an act a successor typed later (inside the 40 s) comes as much later.
//! A successor the host started as Off lifted finds no park on its books:
//! the host stopped the loop before it under Off, and the row is its own to
//! write. And at every point, a loop STOPPED IN THE POINT'S WINDOW — its line
//! and the task's end its fold took journaled, as `look` journals them
//! ([`Session::say_point`]), its act still to come, where the host's step
//! takes the point and the host stops the loop — is followed a second on,
//! as Off lifts where Off stood and lifted next, and in a chain from it and
//! into it ([`window_cells`]): before 2026-10-20 the end's note was owed to
//! the act, and the matrix, deciding and acting on a point in one step with
//! no point line journaled, had no handoff between them.
//! Where the take window is modelled, a successor attached between an act
//! and the first loop's first read after it first looks there (the read
//! latency is the world's). Time on the fixed clock moves the policy's
//! instants as a real clock would ([`TurnEndState::slept`] at every step,
//! the watch's marks moved with it so no sleep is read), and the monotonic
//! clock stands between steps ([`Session::set_mono_standing`]): no real time
//! is read, so a step a loaded machine is slow over moves no instant. The
//! take window, where a scenario models it, is production's 30 s
//! ([`TAKE_S`]) on the worker's clock, which the test's does not move: a
//! read the model places past it is decided with no window
//! ([`Loop::take_clock`]), so the matrix sleeps for none and decides the
//! same every run (the scenarios run on the machine's threads, each cell's
//! loops its own). NEGATIVE CONTROL:
//! successors that take nothing up from the ledger type sooner, and the
//! matrix says so. An API error's retry ladder (a 529) is held to the rule
//! too ([`api_retry_scenarios`]): its rung and appearance are every row's
//! mark while it stands ([`crate::supervise::approvals::RetryMark`],
//! [`TurnEndState::seed_retry`]).
//!
//! What the seed takes up, by the ledger's rows (the verifier of 341e1fa81,
//! RS7-1..7): an awaited act's take window from the first loop's first read
//! after it (its wall's row) or, nothing read yet, this loop's
//! ([`TurnEndState::seed_awaited`]); a try met again from where it was judged
//! met (that window's end), however old, its rung due in the gap owed
//! ([`TurnEndState::seed_met_attempt`]); a try nobody read met at once where
//! this loop's first read dates its answer — the done row, the transcript's
//! notice ([`TurnEndState::date_unread_try`]; its record for a loop after it,
//! [`crate::supervise::approvals::RULE_TRY_ANSWERED`]) — and, nothing dating
//! it, aged out over before this loop's first wall, the wall standing ([`TurnEndState::take_try_over`],
//! [`crate::supervise::approvals::RULE_TRY_OVER`]); Off standing
//! ([`crate::supervise::approvals::RULE_OFF_STOOD`]), parked as the loop that
//! parked holds its books — a record, no read of a try; a task's end
//! ([`crate::supervise::approvals::RULE_TASK_DONE`]), the act before it
//! answered; the Land episode ([`crate::supervise::approvals::land_episode`]);
//! and the short streak ([`crate::supervise::approvals::short_streak`]): an
//! act's mark, its yield still to come, or the record of the last point a
//! loop judged ([`crate::supervise::approvals::RULE_POINT`], round 8: every
//! act's answer and every move of the streak; round 9: every move of the
//! point's time), that point's streak and time, and the answer of the check
//! or a land act judged there — unless a turn was read running after it
//! ([`crate::supervise::approvals::RULE_TURN_RAN`], round 9): the next idle
//! point is that turn's end, a later turn's. A person's exchange left to them
//! is in the ledger whenever it is left, the task's end said before it or not
//! (round 9). The gap cells ([`gap_cells`]: [`run_follower`] on books cut at
//! a point the loop before never read, whose end it reads as a turn that ran:
//! [`TurnEndState::take_gap`]) are their own tests, every scenario's among
//! them, and so is the cut a loop's start makes of a ledger past its bound
//! ([`crate::supervise::approvals::bound`]).

use super::*;
use crate::supervise::approvals::Ledger;
use std::fmt::Write as _;

const SID: &str = "@s-1";

/// The tab a cold restore reopened the follower's from ([`Books::restored`]).
const WAS: &str = "s-a0";

#[derive(Clone, Debug)]
enum Ev {
    /// The screen shows a point: a turn's end, a wall.
    Point(Vec<String>),
    /// The Mac SLEPT the given seconds up to this beat, and the screen shows
    /// a point, read first after the wake: the wall clock moves the length
    /// of the sleep with nothing run and the watch's marks left where they
    /// stood ([`Loop::advance_to`]), and the machine counts the sleep and
    /// records it as its last, so the loop's first read measures it
    /// ([`Session::watch_sleep`]) — no other step of the matrix's clock is
    /// a sleep the loop reads, and a loop that attaches after it finds the
    /// count and the record as they stand ([`First::asleep_at`]).
    Woke(i64, Vec<String>),
    /// A live turn read busy.
    Busy(Vec<String>),
    /// The host's transcript of the agent changes (nothing drawn).
    Tail(Option<String>),
    /// Nothing happens: a handoff instant (one second after it) where the
    /// rest of the timeline has none.
    Mark,
    /// A person's keystroke in the window (`status human_ms=` from it on);
    /// nothing drawn.
    Key,
    /// The session's Autopilot stood at Off and lifted
    /// ([`TurnEndState::park_off`]); nothing drawn. A loop attached one
    /// second after it is the one the host started as Off lifted
    /// ([`Session::seed_from_ledger`]'s `lifted_off`).
    OffLifted,
}

impl Ev {
    /// The seconds the Mac slept up to the event ([`Ev::Woke`]): none for
    /// any other.
    fn slept(&self) -> i64 {
        match self {
            Ev::Woke(secs, _) => *secs,
            _ => 0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum At {
    /// Seconds from the scenario's start.
    Start(i64),
    /// Seconds after the first loop's `k`-th act (0-based).
    Act(usize, i64),
}

struct Beat {
    at: At,
    label: String,
    ev: Ev,
}

fn beat(at: At, label: &str, ev: Ev) -> Beat {
    Beat {
        at,
        label: label.to_string(),
        ev,
    }
}

struct Scenario {
    name: String,
    t0: i64,
    policy: SupervisorConfig,
    /// The host's restart answers, and its transcript at the start; `None`:
    /// no host.
    host: Option<(Vec<&'static str>, Option<String>)>,
    beats: Vec<Beat>,
    /// Seconds past the last beat simulated.
    tail_s: i64,
    /// The take window is modelled ([`TAKE_S`]); else every point is
    /// judged at once.
    take: bool,
    /// Every screen read carries the person's last keystroke ([`Ev::Key`]),
    /// as production's reads carry `human_ms`: a point is judged by the stamp
    /// its own read carries, before any `status` is read. Else only the
    /// window's `status` says it.
    person_on_screen: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct Act {
    at: i64,
    rule: String,
    text: String,
}

/// One loop over the scripted timeline.
struct Loop<'m> {
    s: Session<'m, Mock>,
    opts: SuperviseOpts,
    host: Option<Arc<AccountHost>>,
    ledger: PathBuf,
    read: usize,
    /// The ledger mirrored: its rows, read again only as it grows ([`RowCount`]).
    count: RowCount,
    /// Every ledger row, stamped with the test's clock when it was written.
    stamped: Vec<(i64, String)>,
    /// The lines said and noted since the last look ([`Journaled`]), each
    /// with the count of ledger rows written before it.
    lines: Vec<Said>,
    /// Every line the loop said or noted, journaled as production journals
    /// it beside the ledger ([`crate::supervise::approvals::journal_beside`]),
    /// stamped where it fell among the rows.
    journal: Vec<(i64, String)>,
    acts: Vec<Act>,
    seq: u64,
    /// The wait standing (unix seconds).
    due: Option<i64>,
    busy_since: Option<i64>,
    /// The last point decided (what a wait's end decides again).
    seen: Option<Turn>,
    /// The screen standing.
    rows: Vec<String>,
    log: Vec<String>,
    /// The take window is modelled ([`Scenario::take`]).
    take: bool,
    /// The first point after the last act no busy read followed: when, on
    /// the test's clock.
    unseen: Option<i64>,
    /// An act awaited, no busy read since.
    awaiting: bool,
    /// A person's last keystroke ([`Ev::Key`]), on the test's clock.
    key_at: Option<i64>,
    /// Every screen read carries it ([`Scenario::person_on_screen`]).
    person_on_screen: bool,
    /// Every point's window ([`Window`]), in order.
    windows: Vec<Window>,
}

/// THE WINDOW OF A POINT ([`Loop::point`]): its own line journaled, and the
/// task's end its fold took right after it, its act still to come — where
/// production's host may take the point with a step of its own and the
/// loop be stopped before the act (`look`: the live upgrade's restart, a
/// relaunch's carry-on, then Off, a SUPERVISION setting saved, aterm's own
/// update). Its instant, and how many of the loop's rows ([`Loop::stamped`])
/// and lines ([`Loop::journal`]) stand in it: the books a loop stopped there
/// leaves ([`window_cells`]).
#[derive(Clone, Copy, Debug)]
struct Window {
    at: i64,
    rows: usize,
    lines: usize,
}

/// A line said or noted ([`Journaled`]): the count of ledger rows written
/// before it, and what its journal record carries beside the line — the
/// mark of the worker's last words at a point
/// ([`crate::supervise::journal::JournalRecord::mark`]), and the time an
/// end it restates was first noted
/// ([`crate::supervise::journal::JournalRecord::ended`]).
struct Said {
    rows: usize,
    line: String,
    mark: Option<u64>,
    ended: Option<i64>,
}

/// The production take window ([`TurnEndTiming::take_within`]), on the
/// test's clock.
const TAKE_S: i64 = 30;

/// The take window a scenario's loops run with: production's where it is
/// modelled (the worker's clock, which the test's does not move, never runs
/// it out: [`Loop::take_clock`] ends it), else none.
fn take_within(take: bool) -> Duration {
    if take {
        Duration::from_secs(TAKE_S.unsigned_abs())
    } else {
        Duration::ZERO
    }
}

/// THE LOOP'S REVIEW, AS PRODUCTION'S JOURNALS: every line said or noted
/// goes to the session's journal (`Lines`, `StopAtReview`: `Sink::record`),
/// which the window's host keeps beside the ledger and a loop that follows
/// reads the landing from (`landed_before`). With no journal, the matrix
/// never met that seed (the merge review's MG9-S2). Each line is kept with
/// the count of ledger rows written before it, so [`Loop::collect`] stamps it
/// where it fell among them, as production's one clock orders them.
struct Journaled<'j> {
    ledger: &'j Path,
    /// The rows [`Loop::collect`] has stamped: the count a line takes where
    /// the file cannot be read.
    read: usize,
    /// The ledger mirrored: its rows, read again only as it grows ([`RowCount`]).
    count: &'j mut RowCount,
    lines: &'j mut Vec<Said>,
}

/// THE LEDGER A LOOP APPENDS TO, mirrored: its bytes as the file holds them
/// now, and the rows (`\n`) in them. A look costs one `stat` where the file
/// stands (its length and modification time unmoved), and a read of the rows
/// appended since where it grew — through the descriptor the last read held,
/// where the platform names the file's identity and the path still names that
/// file (a loop writes its books in place only by appending; its cut writes a
/// new file and renames it in, read whole). Anything else is read whole, as a
/// look at every line the loop says once read it.
#[derive(Default)]
struct RowCount {
    /// The file read, held open, and its identity.
    held: Option<(std::fs::File, u64)>,
    /// The length and modification time the mirror stands at.
    at: Option<(u64, Option<std::time::SystemTime>)>,
    /// The file's bytes, and the rows in them.
    body: Vec<u8>,
    rows: usize,
}

/// The file's identity where the platform names one (its inode).
fn file_identity(meta: &std::fs::Metadata) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        Some(meta.ino())
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        None
    }
}

impl RowCount {
    /// The rows at `path` now; `unread` where it cannot be read.
    fn rows(&mut self, path: &Path, unread: usize) -> usize {
        self.counted(path).unwrap_or(unread)
    }

    /// The rows at `path` now, `None` where it cannot be read.
    fn counted(&mut self, path: &Path) -> Option<usize> {
        self.sync(path).then_some(self.rows)
    }

    /// The text at `path` now (`str::lines` of it as `std::fs::read_to_string`
    /// reads it: empty where it cannot be read or is no UTF-8).
    fn text(&mut self, path: &Path) -> &str {
        if self.sync(path) {
            std::str::from_utf8(&self.body).unwrap_or_default()
        } else {
            ""
        }
    }

    /// The mirror brought to the file at `path`: `false` where it cannot be
    /// read (the mirror then holds nothing).
    fn sync(&mut self, path: &Path) -> bool {
        use std::io::Read as _;
        let Ok(meta) = std::fs::metadata(path) else {
            *self = Self::default();
            return false;
        };
        let id = file_identity(&meta);
        let at = (meta.len(), meta.modified().ok());
        let held = id.is_some() && self.held.as_ref().map(|(_, h)| *h) == id;
        if self.at == Some(at) && (held || id.is_none()) {
            return true;
        }
        if held
            && at.0 > self.body.len() as u64
            && let Some((file, _)) = self.held.as_mut()
        {
            let from = self.body.len();
            if file.read_to_end(&mut self.body).is_ok() {
                self.rows += self.body[from..].iter().filter(|&&c| c == b'\n').count();
                self.at = Some((self.body.len() as u64, at.1));
                return true;
            }
        }
        let mut body = Vec::new();
        let Ok(mut file) = std::fs::File::open(path) else {
            *self = Self::default();
            return false;
        };
        if file.read_to_end(&mut body).is_err() {
            *self = Self::default();
            return false;
        }
        self.rows = body.iter().filter(|&&c| c == b'\n').count();
        self.at = Some((body.len() as u64, at.1));
        self.body = body;
        self.held = id.map(|id| (file, id));
        true
    }
}

impl Journaled<'_> {
    fn record(&mut self, line: &str, mark: Option<u64>, ended: Option<i64>) {
        let rows = self.count.rows(self.ledger, self.read);
        self.lines.push(Said {
            rows,
            line: line.to_string(),
            mark,
            ended,
        });
    }
}

impl Review for Journaled<'_> {
    fn approved(&mut self, _seq: u64, _command: &str) -> Result<(), String> {
        Ok(())
    }
    fn review(&mut self, turn: &Turn) -> Result<bool, String> {
        self.review_marked(turn, None)
    }
    /// The point's own line, with its mark, as `Lines` journals it: what a
    /// loop that follows reads an end's point by ([`ended_before`]).
    fn review_marked(&mut self, turn: &Turn, mark: Option<u64>) -> Result<bool, String> {
        self.record(&event_line(turn, &[]), mark, None);
        Ok(true)
    }
    fn say(&mut self, line: &str) -> Result<(), String> {
        self.record(line, None, None);
        Ok(())
    }
    fn note(&mut self, line: &str) {
        self.record(line, None, None);
    }
    fn note_ended(&mut self, line: &str, ended: Option<i64>) {
        self.record(line, None, ended);
    }
    fn unattended(&self) -> bool {
        false
    }
}

/// The rows of `stamped` (ledger rows or journal lines) written before `ms`.
fn before_ms(stamped: &[(i64, String)], ms: i64) -> Vec<(i64, String)> {
    stamped.iter().filter(|(t, _)| *t < ms).cloned().collect()
}

fn restamp(line: &str, ms: i64) -> String {
    let Some(rest) = line.strip_prefix("{\"ts\":") else {
        return line.to_string();
    };
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    format!("{{\"ts\":{ms}{}", &rest[digits..])
}

impl Loop<'_> {
    fn now(&self) -> i64 {
        self.s.now_unix()
    }

    fn advance(&mut self, to: i64) {
        let from_ms = self.s.now_unix_ms();
        if to * 1000 <= from_ms {
            return;
        }
        self.s.set_clock(to, PDT);
        let by = u64::try_from(to * 1000 - from_ms).unwrap_or(0);
        self.s.turn_end.slept(Duration::from_millis(by));
        // The person's last keystroke the last `status` read, and the
        // draft's last change, are as far back.
        let back = |p: &mut Instant| {
            if let Some(earlier) = p.checked_sub(Duration::from_millis(by)) {
                *p = earlier;
            }
        };
        if let Some(p) = self.s.person.as_mut() {
            back(p);
        }
        if let Some((_, p)) = self.s.draft_seen.as_mut() {
            back(p);
        }
        self.s.clock_marks = Some((self.s.fx.mono(), self.s.now_unix_ms()));
    }

    /// The clock to `to` for an event that comes then ([`Self::advance`]),
    /// the last `asleep` seconds of it a sleep of the Mac ([`Ev::Woke`]):
    /// the wall clock alone moves over those, no instant of the policy's
    /// and no mark of the watch's with it. A busy turn's own measure is the
    /// loop's monotonic clock, which a sleep stops: its start moves with it,
    /// and so does a person's keystroke, whose age the host reads on its
    /// awake clock.
    fn advance_to(&mut self, to: i64, asleep: i64) {
        self.advance(to - asleep);
        if asleep > 0 && to > self.now() {
            let asleep = to - self.now();
            self.s.set_clock(to, PDT);
            // The machine counts it, and records it as its last sleep.
            self.s.count_slept(asleep);
            if let Some(b) = &mut self.busy_since {
                *b += asleep;
            }
            // A person's keystroke's age is the host's awake clock's
            // (`status human_ms=`), which the sleep stopped too.
            if let Some(k) = &mut self.key_at {
                *k += asleep;
            }
        }
    }

    /// The rows written since the last look, stamped now; typed rows are
    /// acts.
    fn collect(&mut self) -> Vec<Act> {
        // No row since the last look: nothing to read ([`RowCount`]).
        let unmoved = self.count.counted(&self.ledger) == Some(self.read);
        let (total, fresh): (usize, Vec<String>) = if unmoved {
            (self.read, Vec::new())
        } else {
            let text = self.count.text(&self.ledger);
            (
                text.lines().count(),
                text.lines().skip(self.read).map(str::to_owned).collect(),
            )
        };
        let now = self.now();
        let mut new = Vec::new();
        for (i, line) in (self.read..).zip(&fresh) {
            let ms = now * 1000 + i64::try_from(i - self.read).unwrap_or(0);
            self.stamped.push((ms, restamp(line, ms)));
            let Ok(v) = aterm_json::from_str::<aterm_json::Value>(line) else {
                continue;
            };
            let field = |k: &str| {
                v.get(k)
                    .and_then(aterm_json::Value::as_str)
                    .unwrap_or("")
                    .to_string()
            };
            let rule = field("rule_id");
            self.log.push(format!(
                "    +{} row {} {} {:?} {:?}",
                now,
                field("decision"),
                rule,
                field("command"),
                field("reason")
            ));
            if field("decision") == "typed" {
                new.push(Act {
                    at: now,
                    rule,
                    text: field("command"),
                });
            }
        }
        // The lines said meanwhile, stamped as the next row after the ones
        // before them would be.
        for said in std::mem::take(&mut self.lines) {
            let ms = now * 1000 + i64::try_from(said.rows.saturating_sub(self.read)).unwrap_or(0);
            let mut rec =
                crate::supervise::journal::JournalRecord::of_line(ms, Some(SID), &said.line, None);
            rec.mark = said.mark;
            rec.ended = said.ended;
            self.journal.push((ms, rec.to_json()));
        }
        self.read = total;
        self.acts.extend(new.clone());
        new
    }

    /// The take window, as the worker's clock would run it: a point decided
    /// [`TAKE_S`] (the test's clock) after the first unjudged one is past it
    /// — the window is zero for that read — and one inside is not.
    fn take_clock(&mut self) {
        let mut window = take_within(self.take);
        if self.take && self.awaiting {
            match self.unseen {
                Some(t1) if self.now() - t1 >= TAKE_S => window = Duration::ZERO,
                Some(_) => {}
                None => self.unseen = Some(self.now()),
            }
        }
        self.s.turn_end.timing.take_within = window;
    }

    /// The person as the window's status says it now ([`Ev::Key`]).
    fn status_person(&mut self) {
        let now = self.now();
        self.s.ctl.human_ms = self
            .key_at
            .map(|k| u64::try_from((now - k) * 1000).unwrap_or(0));
    }

    /// A point drawn now.
    fn point(&mut self, rows: Vec<String>) -> Vec<Act> {
        self.take_clock();
        self.status_person();
        self.seq += 1;
        self.rows.clone_from(&rows);
        self.s.ctl.screens = vec![rows.clone()];
        self.s.ctl.served = 0;
        // The point's read, as production's loop reads every point it
        // decides: it clocks a draft from its first sight (`Session::screen`),
        // whose grace a person's draft is held for.
        let _ = self.s.screen();
        self.s.ctl.served = 0;
        let worked = self
            .busy_since
            .take()
            .map(|b| Duration::from_secs(u64::try_from(self.now() - b).unwrap_or(0)));
        let mut p = point_at(&self.s, rows, self.seq);
        // The read's own stamp ([`Scenario::person_on_screen`]).
        if self.person_on_screen
            && let Some(k) = self.key_at
        {
            p.screen.human = crate::supervise::screen::HumanInput::Ago(
                u64::try_from((self.now() - k) * 1000).unwrap_or(0),
            );
        }
        let a = self
            .s
            .turn_end_at_point(&p, worked, &self.opts)
            .unwrap_or(TurnEndAction::Nothing);
        self.log.push(format!(
            "  {} point seq={} worked={:?} -> {}",
            self.now(),
            self.seq,
            worked.map(|d| d.as_secs()),
            brief(&self.s, &a)
        ));
        // `look`'s order: the task's end the fold took is recorded and
        // published, then the point's own line and the end's note
        // ([`Session::say_point`])…
        let mut review = Journaled {
            ledger: &self.ledger,
            read: self.read,
            count: &mut self.count,
            lines: &mut self.lines,
        };
        let _ = self.s.task_edge_at_fold(&p.screen, &self.opts, &mut review);
        let _ = self.s.say_point(&p, &mut review);
        // …THE WINDOW, where the host's step may take the point and the loop
        // be stopped ([`Window`])…
        let written = self.count.rows(&self.ledger, self.read);
        self.windows.push(Window {
            at: self.now(),
            rows: self.stamped.len() + written.saturating_sub(self.read),
            lines: self.journal.len() + self.lines.len(),
        });
        // …then the point's act.
        let mut review = Journaled {
            ledger: &self.ledger,
            read: self.read,
            count: &mut self.count,
            lines: &mut self.lines,
        };
        let _ = self.s.turn_end_execute(&p, a, &self.opts, &[], &mut review);
        self.seen = Some(p);
        self.after_act()
    }

    /// The standing wait ran out: the point is decided again.
    fn wake(&mut self) -> Vec<Act> {
        let Some(seen) = self.seen.clone() else {
            self.due = None;
            return Vec::new();
        };
        self.s.ctl.screens = vec![self.rows.clone()];
        self.s.ctl.served = 0;
        self.due = None;
        self.take_clock();
        self.status_person();
        let mut review = Journaled {
            ledger: &self.ledger,
            read: self.read,
            count: &mut self.count,
            lines: &mut self.lines,
        };
        let _ = self.s.turn_end_now(&seen, &self.opts, &[], &mut review);
        let new = self.after_act();
        self.log.push(format!(
            "  {} wake -> due {:?}",
            self.now(),
            self.due.map(|u| u - self.now())
        ));
        new
    }

    fn after_act(&mut self) -> Vec<Act> {
        let new = self.collect();
        if !new.is_empty() {
            self.awaiting = true;
            self.unseen = None;
        }
        // The wait the decision (or a restart's tries) left standing; the
        // take window's on the production clock.
        let unseen = self.unseen;
        self.due = self.s.turn_end_due.as_ref().map(|d| {
            if self.take
                && d.why.starts_with("the worker to take")
                && let Some(t1) = unseen
            {
                return (t1 + TAKE_S).max(self.now() + 1);
            }
            let u = self.s.unix_of(d.at);
            u.max(self.now() + 1)
        });
        new
    }

    /// A live turn read busy now.
    fn busy(&mut self, rows: Vec<String>) {
        self.seq += 1;
        self.rows.clone_from(&rows);
        let reader = self.s.reader(&rows);
        self.s.end_memory_on_live_turn(reader, &rows, self.seq);
        self.s.took(self.seq);
        self.awaiting = false;
        self.unseen = None;
        self.busy_since.get_or_insert(self.now());
        self.due = None;
        self.s.turn_end_due = None;
        let _ = self.collect();
        self.log
            .push(format!("  {} busy seq={}", self.now(), self.seq));
    }

    fn tail(&mut self, t: Option<String>) {
        if let Some(h) = &self.host {
            *h.tail.lock().unwrap() = t;
        }
    }

    /// Run the standing waits that fall before `t`; the first act, if
    /// `stop`, ends the run.
    fn waits_before(&mut self, t: i64, stop: bool) -> Option<Act> {
        let mut n = 0;
        while let Some(u) = self.due {
            if u >= t || n > 400 {
                break;
            }
            n += 1;
            self.advance(u);
            let new = self.wake();
            if stop && let Some(a) = new.into_iter().next() {
                return Some(a);
            }
        }
        None
    }

    fn event(&mut self, ev: &Ev) -> Vec<Act> {
        match ev.clone() {
            Ev::Point(rows) | Ev::Woke(_, rows) => self.point(rows),
            Ev::Busy(rows) => {
                self.busy(rows);
                Vec::new()
            }
            Ev::Tail(t) => {
                self.tail(t);
                Vec::new()
            }
            Ev::Mark => Vec::new(),
            Ev::Key => {
                self.key_at = Some(self.now());
                Vec::new()
            }
            Ev::OffLifted => {
                // `park_while_off`'s fold: nothing read under Off is folded
                // in, and the park ends what it ends, its ledger row with it
                // — its note journaled first, in that build's words, as
                // every build since Autopilot notes it.
                let note = crate::supervise::run::lifecycle::parked_off_note(self.seq);
                Journaled {
                    ledger: &self.ledger,
                    read: self.read,
                    count: &mut self.count,
                    lines: &mut self.lines,
                }
                .note(&note);
                self.s.off_stood(self.seq);
                self.collect()
            }
        }
    }
}

fn brief(s: &Session<'_, Mock>, a: &TurnEndAction) -> String {
    match a {
        TurnEndAction::WaitUntil { until, why, .. } => {
            format!(
                "wait +{}s ({})",
                s.unix_of(*until) - s.now_unix(),
                clip(why)
            )
        }
        TurnEndAction::Type { text, rule_id } => format!("type {rule_id} {:?}", clip(text)),
        TurnEndAction::Restart { rule_id, why, .. } => format!("restart {rule_id} {why:?}"),
        other => format!("{other:?}").chars().take(80).collect(),
    }
}

fn opts_for(sc: &Scenario, host: Option<&Arc<AccountHost>>) -> SuperviseOpts {
    SuperviseOpts {
        idle_host: host.map(|h| Arc::clone(h) as Arc<dyn IdleHost>),
        policy: sc.policy.clone(),
        ..auto(30)
    }
}

fn host_for(
    sc: &Scenario,
    answers: &[&'static str],
    tail: Option<String>,
) -> Option<Arc<AccountHost>> {
    sc.host.as_ref().map(|_| {
        let h = AccountHost::scripted(&[None], answers);
        *h.tail.lock().unwrap() = tail;
        h
    })
}

/// The first loop's run: its concrete timeline (absolute times), its acts,
/// and its ledger rows stamped.
struct First {
    /// (time, label, event, the act it answers: `None` for the world's).
    events: Vec<(i64, String, Ev, Option<usize>)>,
    acts: Vec<Act>,
    stamped: Vec<(i64, String)>,
    /// Its journal's lines, stamped ([`Loop::journal`]).
    journal: Vec<(i64, String)>,
    /// Every point's window ([`Window`]), on its books.
    windows: Vec<Window>,
    log: Vec<String>,
    /// When the run ends (unix seconds): its last event, and the
    /// scenario's tail past it.
    end: i64,
    /// The machine a loop that follows it runs on ([`Machine`]).
    machine: Machine,
}

/// THE MACHINE A LOOP THAT FOLLOWS RUNS ON ([`run_follower`]): the first
/// loop's, its count of its sleep on the same boot ([`First::asleep_at`])
/// and its record of its last sleep ([`First::last_sleep_at`]) — unless a
/// test says it keeps no record, or booted again since.
#[derive(Clone, Copy, Default)]
struct Machine {
    /// It keeps no record of its last sleep (Linux keeps none).
    no_last: bool,
    /// It booted again since the first loop's acts: its count of its sleep
    /// is another boot's.
    rebooted: bool,
}

fn run_first(sc: &Scenario, dir: &Path) -> First {
    let ledger = dir.join("first.jsonl");
    let _ = std::fs::remove_file(&ledger);
    let host = host_for(
        sc,
        sc.host.as_ref().map_or(&[][..], |(a, _)| a.as_slice()),
        sc.host.as_ref().and_then(|(_, t)| t.clone()),
    );
    let opts = opts_for(sc, host.as_ref());
    let mut m = Mock::new(vec![Vec::new()]);
    let mut l = Loop {
        s: session(&mut m, Some(SID.to_string())),
        opts,
        host,
        ledger: ledger.clone(),
        read: 0,
        count: RowCount::default(),
        stamped: Vec::new(),
        lines: Vec::new(),
        journal: Vec::new(),
        acts: Vec::new(),
        seq: 100,
        due: None,
        busy_since: None,
        seen: None,
        rows: Vec::new(),
        log: Vec::new(),
        take: sc.take,
        unseen: None,
        awaiting: false,
        key_at: None,
        person_on_screen: sc.person_on_screen,
        windows: Vec::new(),
    };
    l.s.set_zone_lookup(test_zones);
    l.s.ledger = Ledger::open(Some(&ledger), Some(SID), &mut Vec::new());
    l.s.set_turn_end_timing(TurnEndTiming {
        take_within: take_within(sc.take),
        ..TurnEndTiming::default()
    });
    let first_at = sc
        .beats
        .iter()
        .filter_map(|b| match b.at {
            At::Start(dt) => Some(sc.t0 + dt),
            At::Act(..) => None,
        })
        .min()
        .unwrap_or(sc.t0);
    l.s.set_clock(first_at.min(sc.t0), PDT);
    l.s.set_mono_standing();
    // The machine counts its sleep ([`Ev::Woke`]), from none.
    l.s.count_sleeps(Duration::ZERO);
    l.s.turn_end_seeded = true;
    let mut events = Vec::new();
    let mut stamps = std::collections::BTreeMap::new();
    for b in &sc.beats {
        let t = match b.at {
            At::Start(dt) => sc.t0 + dt,
            At::Act(k, dt) => {
                // The act this beat answers: the loop's waits run until it
                // is typed.
                let mut guard = 0;
                while l.acts.len() <= k && guard < 400 {
                    guard += 1;
                    let Some(u) = l.due else { break };
                    l.advance(u);
                    l.wake();
                }
                let Some(a) = l.acts.get(k) else {
                    l.log
                        .push(format!("  !! no act {k} for beat {:?}", b.label));
                    break;
                };
                a.at + dt
            }
        };
        // Nothing runs while the Mac sleeps.
        let asleep = b.ev.slept();
        l.waits_before(t - asleep, false);
        if t - asleep < l.now() {
            l.log.push(format!(
                "  !! beat {:?} at +{} comes after +{}",
                b.label,
                t - sc.t0,
                l.now() - sc.t0
            ));
        }
        let t = t.max(l.now() + asleep);
        l.advance_to(t, asleep);
        l.log.push(format!("  {} == {}", t - sc.t0, b.label));
        let ev = match &b.ev {
            Ev::Point(rows) => Ev::Point(resolve(rows, t, &mut stamps)),
            Ev::Woke(secs, rows) => Ev::Woke(*secs, resolve(rows, t, &mut stamps)),
            Ev::Busy(rows) => Ev::Busy(resolve(rows, t, &mut stamps)),
            // A transcript row written when its beat comes ([`TAIL_AT`]).
            Ev::Tail(Some(tail)) => Ev::Tail(Some(
                tail.replace(TAIL_AT, &crate::harness::usage::rfc3339_utc(t)),
            )),
            other => other.clone(),
        };
        l.event(&ev);
        let answers = match b.at {
            At::Start(_) => None,
            At::Act(k, _) => Some(k),
        };
        events.push((t, b.label.clone(), ev, answers));
    }
    let end = events.iter().map(|e| e.0).max().unwrap_or(sc.t0) + sc.tail_s;
    l.waits_before(end, false);
    let log = std::mem::take(&mut l.log);
    let out = First {
        events,
        acts: l.acts.clone(),
        stamped: l.stamped.clone(),
        journal: l.journal.clone(),
        windows: l.windows.clone(),
        log,
        end,
        machine: Machine::default(),
    };
    drop(l);
    out
}

/// A cell's follower: its first act that is not the first loop's next, the
/// first loop's act it was due to match, the wait it left standing (unix
/// seconds), and its log.
type Cell = (Option<Act>, usize, Option<i64>, Vec<String>);

/// What a loop that follows did ([`run_follower`]).
struct Followed {
    /// Its first act that is not the first loop's next.
    diverged: Option<Act>,
    /// The first loop's act it was due to match next.
    at_k: usize,
    /// The last act it (or a loop before it) typed.
    last: Option<i64>,
    /// The wait it left standing (unix seconds).
    due: Option<i64>,
    log: Vec<String>,
    /// The books it attached on and every row it wrote, stamped: the ledger
    /// a loop that follows IT attaches on ([`run_chain`]).
    stamped: Vec<(i64, String)>,
    /// The journal it attached beside and every line it said, stamped: the
    /// journal beside that ledger.
    journal: Vec<(i64, String)>,
    /// Every point's window it met ([`Window`]), on those books.
    windows: Vec<Window>,
}

/// The books a loop that follows attaches on: the ledger's rows and the
/// journal's lines beside it, stamped — the ones before its attach standing.
#[derive(Clone, Copy)]
struct Books<'b> {
    rows: &'b [(i64, String)],
    journal: &'b [(i64, String)],
    /// A COLD RESTORE reopened the follower's tab from tab [`WAS`]
    /// ([`Restore`]).
    restored: Option<Restore>,
}

/// A COLD RESTORE that reopened the follower's tab from tab [`WAS`] at
/// `at_ms`: the books' rows and lines before it are that tab's, signed with
/// its session, and the rest the follower's tab's own, beside them.
#[derive(Clone, Copy)]
struct Restore {
    /// When the restore reopened the tab.
    at_ms: i64,
    /// Whether the restore noted `WAS` beside the tab's ledger
    /// (`upgrade_drive::note_earlier_tab`); not: the control.
    noted: bool,
}

impl First {
    /// THE MACHINE'S COUNT OF ITS SLEEP AT `h` (unix seconds): every sleep
    /// of the timeline before it ([`Ev::Woke`]), and as much of one that has
    /// begun and not ended (a loop attached at `h` counts the rest itself).
    fn asleep_at(&self, h: i64) -> Duration {
        let secs = self.slept_between(i64::MIN, h);
        Duration::from_secs(u64::try_from(secs).unwrap_or(0))
    }

    /// How many seconds of the timeline's sleeps ([`Ev::Woke`]) fall between
    /// `a` and `b` (unix seconds).
    fn slept_between(&self, a: i64, b: i64) -> i64 {
        self.events
            .iter()
            .map(|(t, _, ev, _)| {
                let (from, to) = ((t - ev.slept()).max(a), (*t).min(b));
                (to - from).max(0)
            })
            .sum()
    }

    /// THE MACHINE'S RECORD OF ITS LAST SLEEP AT `h` (unix ms, asleep and
    /// woke): the last sleep of the timeline that ended by then.
    fn last_sleep_at(&self, h: i64) -> Option<(i64, i64)> {
        self.events
            .iter()
            .filter(|(t, _, ev, _)| ev.slept() > 0 && *t <= h)
            .max_by_key(|(t, ..)| *t)
            .map(|(t, _, ev, _)| ((t - ev.slept()) * 1000, t * 1000))
    }

    /// The first loop's books ([`Books`]).
    fn books(&self) -> Books<'_> {
        Books {
            rows: &self.stamped,
            journal: &self.journal,
            restored: None,
        }
    }

    /// The instants a loop stopped in the window at `at` ([`Window`]) is
    /// followed at: a second on, and — Off standing and lifting next — a
    /// second after it lifts (the host stops a worker its session reads Off
    /// for, and starts one as Off lifts).
    fn window_attach(&self, at: i64) -> Vec<i64> {
        let mut hs = vec![at + 1];
        if let Some((t_off, ..)) = self
            .events
            .iter()
            .find(|(t, ..)| *t > at)
            .filter(|(_, _, ev, _)| matches!(ev, Ev::OffLifted))
        {
            hs.push(t_off + 1);
        }
        hs
    }
}

impl Followed {
    /// The books a loop that follows this one attaches on ([`Books`]).
    fn books(&self) -> Books<'_> {
        Books {
            rows: &self.stamped,
            journal: &self.journal,
            restored: None,
        }
    }
}

/// A successor attached at `h` on the first loop's books as they stood
/// then, fed the rest of the timeline: its first act, and its last wait.
fn run_successor(sc: &Scenario, first: &First, h: i64, dir: &Path, seed: bool) -> Cell {
    let before: Vec<&Act> = first.acts.iter().filter(|a| a.at < h).collect();
    let last = before.last().map(|a| a.at);
    let f = run_follower(
        sc,
        first,
        first.books(),
        (h, before.len(), last),
        None,
        dir,
        seed,
    );
    (f.diverged, f.at_k, f.due, f.log)
}

/// A loop attached at `h` on `on` (rows and journal lines stamped, the ones
/// before `h` standing), the first loop's acts before `k0` typed by the loops before
/// it (the last of them at `last`), fed the rest of the timeline — up to
/// `until` (exclusive), where a loop that follows it takes over, or to the
/// run's end.
fn run_follower(
    sc: &Scenario,
    first: &First,
    on: Books<'_>,
    (h, k0, last): (i64, usize, Option<i64>),
    until: Option<i64>,
    dir: &Path,
    seed: bool,
) -> Followed {
    // ONE SET OF FILES PER `dir`, its loops run one after another: each
    // file a loop reads is written whole before it attaches (the books, the
    // journal beside them, a restore's note, which names the same tab every
    // time), and the loop writes nothing here but its ledger's rows. Made and
    // removed per cell, the files were most of a restored cell's time.
    let books = dir.join("books.jsonl");
    // Whether the host started this loop as the session's Off lifted: the
    // loop before it was stopped under Off, so the park's row is this
    // loop's to write ([`Session::seed_from_ledger`]), not on its books.
    let lifted_at = first
        .events
        .iter()
        .find(|(t, _, ev, _)| matches!(ev, Ev::OffLifted) && *t < h && h - *t <= 1)
        .map(|e| e.0);
    let lifted_off = lifted_at.is_some();
    let standing_rows: Vec<(i64, String)> = on
        .rows
        .iter()
        .filter(|(ms, _)| *ms < h * 1000)
        .filter(|(ms, row)| {
            !(lifted_at.is_some_and(|t| *ms >= t * 1000)
                && row.contains(&format!(
                    "\"rule_id\":\"{}\"",
                    crate::supervise::approvals::RULE_OFF_STOOD
                )))
        })
        .cloned()
        .collect();
    // A COLD RESTORE ([`Books::restored`]): the books standing before it are
    // those of the tab it was reopened from, signed with that tab's session,
    // under a state root's `drive` beside this tab's own ledger.
    let state = dir.join("state");
    let (written, books) = match on.restored {
        None => (books.clone(), books),
        Some(restore) => {
            use crate::supervise::approvals::ledger_under;
            let tab = SID.trim_start_matches('@');
            std::fs::create_dir_all(state.join("drive")).expect("the drive");
            if restore.noted {
                crate::harness::upgrade_drive::note_earlier_tab_under(&state, tab, WAS);
            }
            (
                ledger_under(&state, Some(WAS)),
                ledger_under(&state, Some(SID)),
            )
        }
    };
    let theirs = |ms: i64| on.restored.is_some_and(|r| ms < r.at_ms);
    let signed = |l: &str| {
        l.replace(
            &format!("\"sid\":\"{}\"", SID.trim_start_matches('@')),
            &format!("\"sid\":\"{WAS}\""),
        )
    };
    let body = |lines: &[(i64, String)], mine: bool| -> String {
        lines
            .iter()
            .filter(|(ms, _)| {
                if mine {
                    !theirs(*ms)
                } else {
                    on.restored.is_none() || theirs(*ms)
                }
            })
            .map(|(ms, l)| {
                if theirs(*ms) {
                    format!("{}\n", signed(l))
                } else {
                    format!("{l}\n")
                }
            })
            .collect()
    };
    std::fs::write(&written, body(&standing_rows, false)).expect("the books");
    // The journal beside them, as the window's host keeps it.
    let journal = crate::supervise::approvals::journal_beside(&written);
    let standing_lines = before_ms(on.journal, h * 1000);
    std::fs::write(&journal, body(&standing_lines, false)).expect("the journal");
    // The restored tab's own, beside them.
    let own = on.restored.is_some().then(|| {
        let journal = crate::supervise::approvals::journal_beside(&books);
        std::fs::write(&books, body(&standing_rows, true)).expect("the tab's books");
        std::fs::write(&journal, body(&standing_lines, true)).expect("the tab's journal");
        standing_rows.iter().filter(|(ms, _)| !theirs(*ms)).count()
    });
    // The transcript as it stands at `h` (the world's answer to an act its
    // books do not hold is not written yet).
    let mut tail = sc.host.as_ref().and_then(|(_, t)| t.clone());
    for (t, _, ev, answers) in &first.events {
        if *t < h
            && answers.is_none_or(|k| k < k0)
            && let Ev::Tail(x) = ev
        {
            tail.clone_from(x);
        }
    }
    // The window's person as its status says it at `h`: the keystroke's age
    // on the host's awake clock, which a sleep since it does not move.
    let key_at = first
        .events
        .iter()
        .filter(|(t, _, ev, _)| *t < h && matches!(ev, Ev::Key))
        .map(|(t, ..)| *t)
        .max()
        .map(|k| k + first.slept_between(k, h));
    let host = host_for(sc, &["relaunched"], tail);
    let opts = opts_for(sc, host.as_ref());
    let mut m = Mock::new(vec![Vec::new()]);
    let mut l = Loop {
        s: session(&mut m, Some(SID.to_string())),
        opts,
        host,
        ledger: books.clone(),
        read: 0,
        count: RowCount::default(),
        stamped: Vec::new(),
        lines: Vec::new(),
        journal: Vec::new(),
        acts: Vec::new(),
        seq: 500,
        due: None,
        busy_since: None,
        seen: None,
        rows: Vec::new(),
        log: Vec::new(),
        take: sc.take,
        unseen: None,
        awaiting: false,
        key_at,
        person_on_screen: sc.person_on_screen,
        windows: Vec::new(),
    };
    l.s.set_zone_lookup(test_zones);
    l.s.set_turn_end_timing(TurnEndTiming {
        take_within: take_within(sc.take),
        ..TurnEndTiming::default()
    });
    l.s.set_clock(h, PDT);
    l.s.set_mono_standing();
    // The machine's count of its sleep and its record of its last sleep, as
    // they stand at `h` ([`First::asleep_at`]), on the machine the test says
    // ([`Machine`]).
    l.s.count_sleeps(first.asleep_at(h));
    if first.machine.rebooted {
        l.s.set_boot(crate::supervise::fx::OTHER_TEST_BOOT);
    }
    l.s.set_last_sleep(first.last_sleep_at(h).filter(|_| !first.machine.no_last));
    // `drive_loop`'s seed. Its take window on the test's clock: an act
    // younger than [`TAKE_S`] is awaited (production's 30 s), whose window
    // then runs on the worker's clock as the first loop's does.
    // (Not seeded: the negative control, a loop with no books.)
    l.s.turn_end.timing.take_within = Duration::from_secs(TAKE_S.unsigned_abs());
    // The ledger is open as the seed is read, as `drive_loop` opens it: the
    // rows so far are the predecessor's, and any the seed writes this loop's.
    l.s.ledger = Ledger::open(Some(&books), Some(SID), &mut Vec::new());
    l.read = own.unwrap_or(standing_rows.len());
    let open = seed
        .then(|| l.s.seed_from_ledger(&books, h * 1000, lifted_off))
        .flatten();
    let _ = l.collect();
    l.s.turn_end.timing.take_within = take_within(sc.take);
    l.s.seed_wall_memory(open);
    l.awaiting = sc.take && l.s.turn_end.untaken_act().is_some();
    // The window the seed counts from the first loop's read, on the test's
    // clock ([`TurnEndState::seed_awaited`]).
    if l.awaiting {
        l.unseen = l.s.turn_end.untaken_act_seen().map(|t| {
            let ago = l.s.fx.mono().saturating_duration_since(t).as_millis();
            h - i64::try_from((ago + 500) / 1000).unwrap_or(0)
        });
    }
    if lifted_off {
        l.log
            .push(format!("  {} == attached as Off lifted", h - sc.t0));
    }
    // Its first look: the screen standing at `h` — never the world's answer
    // to an act its books do not hold (books cut short of the first loop's:
    // [`a_tries_answer_no_loop_read_is_dated_by_the_screen_or_the_transcript`]).
    let standing = first.events.iter().rev().find(|(t, _, ev, answers)| {
        *t < h
            && answers.is_none_or(|k| k < k0)
            && matches!(ev, Ev::Point(_) | Ev::Woke(..) | Ev::Busy(_))
    });
    // On the fixed clock (no take window modelled) a point is judged at once:
    // a first look, inside an act's own window, at the screen the act was
    // typed at would judge what no loop judges there (production's window is
    // 30 s, and the act is awaited: [`TurnEndState::seed_awaited`]). Those
    // scenarios' successors first look at the next event. (The act the
    // loops before this one typed: a chain's successor may have typed none
    // where the first loop did.)
    //
    // Where the take window is modelled, a successor attached after an act
    // and before the first loop's first read after it (a point or a busy
    // read, however late it comes) first looks there too: a loop reads at
    // its poll, and a handoff takes longer, so the read latency is the
    // world's (a read 70 s on is the Mac asleep, a refusal's pause, an
    // outage ridden out: no loop reads sooner). One that read sooner would
    // open the act's window sooner, as the first loop itself would reading
    // then, and judge a turn whose busy read comes in the gap as no taking of
    // the act.
    let first_read = |a: i64| {
        first
            .events
            .iter()
            .filter(|(t, _, ev, _)| {
                *t > a && matches!(ev, Ev::Point(_) | Ev::Woke(..) | Ev::Busy(_))
            })
            .map(|e| e.0)
            .min()
    };
    let standing = standing.filter(|(t, ..)| match last {
        Some(a) if *t <= a && sc.take => first_read(a).is_none(),
        Some(a) if *t <= a => h - a >= TAKE_S,
        _ => true,
    });
    // The successor's acts that match the first loop's from `k0` on (the
    // same text, no sooner and within 40 s later; any relaunch for a
    // relaunch) carry it on: the
    // world's answers to them are its events too. The first that does not
    // match is the cell's.
    let same = |want: Option<&Act>, got: &Act| {
        want.is_some_and(|w| {
            (w.rule == "model-fallback@v1" && got.rule == w.rule)
                || (w.text == got.text && (0..=40).contains(&(got.at - w.at)))
        })
    };
    let mut matched = 0usize;
    let mut diverged: Option<Act> = None;
    let mut chain: Vec<Act> = Vec::new();
    let mut take = |acts: Vec<Act>, matched: &mut usize, chain: &mut Vec<Act>| -> bool {
        for a in acts {
            if same(first.acts.get(k0 + *matched), &a) {
                *matched += 1;
                chain.push(a);
            } else {
                diverged = Some(a);
                return false;
            }
        }
        true
    };
    let mut going = true;
    if let Some((_, label, ev, _)) = standing {
        l.log.push(format!("  {} == attach on {label}", h - sc.t0));
        let acts = match ev {
            Ev::Point(rows) | Ev::Woke(_, rows) => l.point(rows.clone()),
            Ev::Busy(rows) => {
                l.busy(rows.clone());
                Vec::new()
            }
            _ => Vec::new(),
        };
        going = take(acts, &mut matched, &mut chain);
    }
    let end = until.unwrap_or(first.end);
    // The rest of the timeline: what comes after `h`, and the world's answer
    // to every act its books do not hold yet, however early the first loop
    // met it (placed after this loop's own act, below).
    let mut evs: Vec<&(i64, String, Ev, Option<usize>)> = first
        .events
        .iter()
        .filter(|(t, _, _, answers)| (*t >= h || answers.is_some_and(|k| k >= k0)) && *t < end)
        .collect();
    evs.sort_by_key(|e| e.0);
    // The world answers an act once it is typed: an answer to one this loop
    // typed later than the first loop (inside the 40 s it matches by) comes
    // as much later, one to an act it typed sooner on the first loop's
    // timeline (the screen an act was typed at stands until the first loop's
    // own answer comes), and one to an act it has not typed, not yet.
    let at_of = |e: &(i64, String, Ev, Option<usize>), chain: &[Act]| match e.3 {
        Some(k) if k >= k0 => chain
            .get(k - k0)
            .map(|mine| e.0 + (mine.at - first.acts[k].at).max(0)),
        _ => Some(e.0),
    };
    while going {
        let next = evs
            .iter()
            .enumerate()
            .filter_map(|(i, e)| at_of(e, &chain).map(|t| (i, t)))
            .min_by_key(|&(i, t)| (t, i))
            .filter(|&(_, t)| t < end);
        let typed = matched;
        // Nothing runs while the Mac sleeps ([`Ev::Woke`]).
        let awake = next.map_or(end, |(i, t)| t - evs[i].2.slept());
        while going && let Some(a) = l.waits_before(awake, true) {
            going = take(vec![a], &mut matched, &mut chain);
        }
        // An act typed meanwhile: its answers may come first.
        if !going || matched != typed {
            continue;
        }
        let Some((i, t)) = next else {
            break;
        };
        let (_, label, ev, _) = evs.remove(i);
        l.advance_to(t, ev.slept());
        l.log.push(format!("  {} == {label}", t - sc.t0));
        let acts = l.event(ev);
        going = take(acts, &mut matched, &mut chain);
    }
    while going && let Some(a) = l.waits_before(end, true) {
        going = take(vec![a], &mut matched, &mut chain);
    }
    let due = l.due;
    let mut log = std::mem::take(&mut l.log);
    log.push(format!(
        "  matched the first loop's acts {}..{}: {:?}",
        k0,
        k0 + matched,
        chain
            .iter()
            .map(|a| format!("+{} {}", a.at - sc.t0, a.rule))
            .collect::<Vec<_>>()
    ));
    let windows = l
        .windows
        .iter()
        .map(|w| Window {
            rows: standing_rows.len() + w.rows,
            lines: standing_lines.len() + w.lines,
            ..*w
        })
        .collect();
    let mut stamped = standing_rows;
    stamped.extend(l.stamped.iter().cloned());
    let mut lines = standing_lines;
    lines.extend(l.journal.iter().cloned());
    drop(l);
    Followed {
        diverged,
        at_k: k0 + matched,
        // The world answers the first loop's acts on its timeline: the
        // screen an act was typed at stands until the first loop's own
        // answer comes (a relaunch matched however much sooner).
        last: chain.last().map(|a| a.at).or(last).max(
            (k0 + matched)
                .checked_sub(1)
                .and_then(|k| first.acts.get(k))
                .map(|a| a.at),
        ),
        due,
        log,
        stamped,
        journal: lines,
        windows,
    }
}

/// A CHAIN OF THREE LOOPS: a successor attached at `h1` on the first loop's
/// books, run up to `h2`, where a third loop attaches on ITS books (the
/// first loop's rows and the successor's own) and runs to the end. `None`
/// where the successor itself parted from the first loop before `h2` (that
/// cell is the pair's).
fn run_chain(sc: &Scenario, first: &First, h1: i64, h2: i64, dir: &Path) -> Option<Cell> {
    let before: Vec<&Act> = first.acts.iter().filter(|a| a.at < h1).collect();
    let last = before.last().map(|a| a.at);
    let second = run_follower(
        sc,
        first,
        first.books(),
        (h1, before.len(), last),
        Some(h2),
        dir,
        true,
    );
    if second.diverged.is_some() {
        return None;
    }
    let third = run_follower(
        sc,
        first,
        second.books(),
        (h2, second.at_k, second.last),
        None,
        dir,
        true,
    );
    let mut log = vec![format!("  the successor at +{}:", h1 - sc.t0)];
    log.extend(second.log);
    log.push(format!("  the third loop at +{}:", h2 - sc.t0));
    log.extend(third.log);
    Some((third.diverged, third.at_k, third.due, log))
}

/// The handoff instants of a run: just after each event and act, just
/// before each act, and the middle of every long gap — but none while the
/// Mac sleeps ([`Ev::Woke`]): no loop stops or starts then.
fn handoffs(first: &First) -> Vec<i64> {
    let mut moments: Vec<i64> = first.events.iter().map(|e| e.0).collect();
    moments.sort_unstable();
    moments.extend(first.acts.iter().map(|a| a.at));
    moments.sort_unstable();
    moments.dedup();
    let mut hs = Vec::new();
    for (i, &m) in moments.iter().enumerate() {
        hs.push(m + 1);
        if first.acts.iter().any(|a| a.at == m) {
            hs.push(m - 1);
        }
        if let Some(&next) = moments.get(i + 1)
            && next - m > 240
        {
            hs.push(m + (next - m) / 2);
            hs.push(next - 2);
            if next - m > 1200 {
                hs.push(m + (next - m) / 4);
                hs.push(m + 3 * (next - m) / 4);
            }
        }
    }
    hs.sort_unstable();
    hs.dedup();
    let start = first.events.iter().map(|e| e.0).min().unwrap_or(0);
    hs.retain(|h| *h > start);
    hs.retain(|h| {
        !first
            .events
            .iter()
            .any(|(t, _, ev, _)| t - ev.slept() < *h && *h < *t)
    });
    hs
}

/// The cells of a scenario's matrix that break the rule, and how many it ran.
#[derive(Default)]
struct Tally {
    cells: usize,
    broken: Vec<String>,
}

/// How long past the first loop's act a successor's may come: a fresh
/// appearance's first rung from attaching (30 min), a person's draft standing
/// in the composer, whose grace each loop counts from its own first sight
/// of it (`human_grace_s`, 2 min: a loop held by it past the try's next
/// rung leaves the rung owed to the loop after it, which types it once its
/// own grace is over: [`TurnEndState::seed_met_attempt`]), and the take
/// window's slack.
const LATER_BOUND_S: i64 = 30 * MIN + 2 * MIN + 40;

/// The rule a follower's first differing act breaks, if any (module docs):
/// `mine` the first loop's act it was due to match, `theirs` its own, `due`
/// the wait it left standing.
fn verdict(mine: Option<&Act>, theirs: Option<&Act>, due: Option<i64>) -> Option<&'static str> {
    match (mine, theirs) {
        (Some(a), Some(b)) if b.at < a.at => Some("SOONER"),
        (Some(a), Some(b)) if a.text != b.text => Some("OTHER-TEXT"),
        (None, Some(_)) => Some("TYPES-WHERE-FIRST-DOES-NOT"),
        (Some(_), None) if due.is_none() => Some("UNBOUNDED"),
        (Some(a), Some(b)) if b.at > a.at + LATER_BOUND_S => Some("LATER-UNBOUNDED"),
        (Some(a), None) if due.is_some_and(|d| d > a.at + LATER_BOUND_S) => Some("LATER-UNBOUNDED"),
        _ => None,
    }
}

/// One scenario's matrix: the first loop's run, and a successor at every
/// handoff (`seed`: taken up from the ledger, else started blank, the
/// negative control) — and, seeded, a CHAIN at every handoff: the
/// successor followed in its turn by a third loop at the next handoff
/// ([`run_chain`]) — and the cells of every point's window
/// ([`window_cells`]). The log of every cell, and the cells that break the
/// rule (module docs) with their successor's log.
fn run_matrix(sc: &Scenario, seed: bool) -> (String, Tally) {
    run_matrix_owed(sc, seed, false)
}

/// [`run_matrix`]; `owed`: the window's books without the task's end said
/// in it ([`window_cells`], the negative control).
fn run_matrix_owed(sc: &Scenario, seed: bool, owed: bool) -> (String, Tally) {
    let mut out = String::new();
    let mut tally = Tally::default();
    // The scenario's name as a file name every platform takes (Windows refuses `:`, `?`, …).
    let tag: String = sc
        .name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let dir = crate::supervise::test_scratch_path("seed-matrix", &tag);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let first = run_first(sc, &dir);
    let _ = writeln!(out, "\n=== {} (t0={})", sc.name, sc.t0);
    for line in &first.log {
        let _ = writeln!(out, "{line}");
    }
    let _ = writeln!(
        out,
        "  first loop acts: {:?}",
        first
            .acts
            .iter()
            .map(|a| format!("+{} {} {:?}", a.at - sc.t0, a.rule, a.text))
            .collect::<Vec<_>>()
    );
    let rel = |a: Option<&Act>| {
        a.map_or("none".to_string(), |a| {
            format!("+{} {} {:?}", a.at - sc.t0, a.rule, a.text)
        })
    };
    let cell =
        |what: String, (theirs, at_k, due, log): Cell, out: &mut String, tally: &mut Tally| {
            tally.cells += 1;
            let mine = first.acts.get(at_k);
            let line = format!(
                "{} | {what} (act {at_k}): first {} ; successor {}{}",
                sc.name,
                rel(mine),
                rel(theirs.as_ref()),
                due.map_or(String::new(), |d| format!(
                    " (still waiting until +{})",
                    d - sc.t0
                ))
            );
            let v = verdict(mine, theirs.as_ref(), due);
            let _ = writeln!(out, "  [{}] {line}", v.unwrap_or("ok"));
            if let Some(v) = v {
                let mut broken = format!("{v}: {line}");
                for l in &log {
                    let _ = write!(broken, "\n      {l}");
                }
                tally.broken.push(broken);
            }
        };
    let hs = handoffs(&first);
    for (i, &h) in hs.iter().enumerate() {
        cell(
            format!("handoff +{}", h - sc.t0),
            run_successor(sc, &first, h, &dir, seed),
            &mut out,
            &mut tally,
        );
        if let Some(&h2) = hs.get(i + 1).filter(|_| seed)
            && let Some(c) = run_chain(sc, &first, h, h2, &dir)
        {
            cell(
                format!("chain +{} then +{}", h - sc.t0, h2 - sc.t0),
                c,
                &mut out,
                &mut tally,
            );
        }
    }
    for (what, c) in window_cells(sc, &first, &hs, &dir, seed, owed) {
        cell(what, c, &mut out, &mut tally);
    }
    let _ = std::fs::remove_dir_all(&dir);
    (out, tally)
}

/// The acts typed on `rows` (ledger rows, stamped): how many, and when the
/// last was (unix seconds) — what a loop attached on them takes the first
/// loop's acts to be up to ([`run_follower`]'s `k0` and `last`).
fn typed_on(rows: &[(i64, String)]) -> (usize, Option<i64>) {
    let typed: Vec<i64> = rows
        .iter()
        .filter(|(_, row)| {
            aterm_json::from_str::<aterm_json::Value>(row).is_ok_and(|v| {
                v.get("decision").and_then(aterm_json::Value::as_str) == Some("typed")
            })
        })
        .map(|(ms, _)| ms.div_euclid(1000))
        .collect();
    (typed.len(), typed.last().copied())
}

/// Books a loop left, owned ([`Books`]): its ledger's rows and its
/// journal's lines, stamped.
type Stood = (Vec<(i64, String)>, Vec<(i64, String)>);

/// THE BOOKS A LOOP STOPPED IN A POINT'S WINDOW LEAVES ([`Window`]): the
/// rows and lines of `on` that stood there. `owed`: without the task's end
/// said in it (`DONE …`, `LEFT …` in the window's instant) — the books
/// before the end's note went right after its point's line, when it was
/// owed to the point's act, the negative control.
fn in_window(on: Books<'_>, w: Window, owed: bool) -> Stood {
    let rows = on.rows[..w.rows].to_vec();
    let lines = on.journal[..w.lines]
        .iter()
        .filter(|(ms, line)| {
            !(owed
                && *ms >= w.at * 1000
                && (line.contains("\"line\":\"DONE ") || line.contains("\"line\":\"LEFT ")))
        })
        .cloned()
        .collect();
    (rows, lines)
}

/// THE WINDOW CELLS OF A SCENARIO (the claim of 2026-10-20 against
/// `look()`: a point's line journaled, the task's end its fold took said
/// at the point's act, which the host's step took): at every point of the
/// first loop's timeline, the loop STOPPED IN THE POINT'S WINDOW
/// ([`Window`]: its line and the task's end said, its act not yet typed —
/// the host's step took the point, and the host stopped the loop) and a
/// successor attached a second on, on the books as it left them; where Off
/// stood and lifted next, one attached as it lifts (the host stops a worker
/// its session reads Off for, and starts one as Off lifts:
/// [`Session::seed_from_ledger`]'s `lifted_off`); a CHAIN FROM IT, that
/// successor run to the next handoff (`hs`) and a third loop attached
/// there on its books; and a CHAIN INTO IT, a successor attached at the
/// handoff before the point, stopped in its own window there, and a third
/// loop attached a second on (and as Off lifts) on the books it left.
/// Each a cell of the rule (module docs), named with its log. `owed`: the
/// books without the task's end said in the window ([`in_window`]), the
/// negative control.
fn window_cells(
    sc: &Scenario,
    first: &First,
    hs: &[i64],
    dir: &Path,
    seed: bool,
    owed: bool,
) -> Vec<(String, Cell)> {
    let mut cells = Vec::new();
    let attach_at = |at: i64| first.window_attach(at);
    let follow =
        |what: &str, on: Stood, at: i64, log: &[String], cells: &mut Vec<(String, Cell)>| {
            let books = Books {
                rows: &on.0,
                journal: &on.1,
                restored: None,
            };
            let (k0, last) = typed_on(books.rows);
            for h in attach_at(at) {
                let f = run_follower(sc, first, books, (h, k0, last), None, dir, seed);
                let mut lines = log.to_vec();
                lines.push(format!("  attached at +{}:", h - sc.t0));
                lines.extend(f.log);
                cells.push((
                    format!(
                        "{what} stopped in the window at +{}, attached at +{}",
                        at - sc.t0,
                        h - sc.t0
                    ),
                    (f.diverged, f.at_k, f.due, lines),
                ));
            }
        };
    for w in &first.windows {
        follow(
            "the first loop",
            in_window(first.books(), *w, owed),
            w.at,
            &[],
            &mut cells,
        );
        if !seed {
            continue;
        }
        // A CHAIN FROM IT: the successor a second on, run to the next
        // handoff, where a third loop attaches on its books.
        let books = in_window(first.books(), *w, owed);
        let on = Books {
            rows: &books.0,
            journal: &books.1,
            restored: None,
        };
        let (k0, last) = typed_on(on.rows);
        if let Some(&h2) = hs.iter().find(|&&h| h > w.at + 1) {
            let second = run_follower(sc, first, on, (w.at + 1, k0, last), Some(h2), dir, true);
            if second.diverged.is_none() {
                let third = run_follower(
                    sc,
                    first,
                    second.books(),
                    (h2, second.at_k, second.last),
                    None,
                    dir,
                    true,
                );
                let mut log = vec![format!("  the successor at +{}:", w.at + 1 - sc.t0)];
                log.extend(second.log);
                log.push(format!("  the third loop at +{}:", h2 - sc.t0));
                log.extend(third.log);
                cells.push((
                    format!(
                        "stopped in the window at +{}, chain +{} then +{}",
                        w.at - sc.t0,
                        w.at + 1 - sc.t0,
                        h2 - sc.t0
                    ),
                    (third.diverged, third.at_k, third.due, log),
                ));
            }
        }
        // A CHAIN INTO IT: a successor attached at the handoff before the
        // point, stopped in its own window there.
        if let Some(&h1) = hs.iter().rev().find(|&&h| h <= w.at) {
            let before: Vec<&Act> = first.acts.iter().filter(|a| a.at < h1).collect();
            let second = run_follower(
                sc,
                first,
                first.books(),
                (h1, before.len(), before.last().map(|a| a.at)),
                Some(w.at + 1),
                dir,
                true,
            );
            if second.diverged.is_none()
                && let Some(sw) = second.windows.iter().rev().find(|x| x.at == w.at)
            {
                let mut log = vec![format!("  the successor at +{}:", h1 - sc.t0)];
                log.extend(second.log.iter().cloned());
                follow(
                    &format!("a loop attached at +{}", h1 - sc.t0),
                    in_window(second.books(), *sw, owed),
                    w.at,
                    &log,
                    &mut cells,
                );
            }
        }
    }
    cells
}

// ---------------------------------------------------------------- screens

/// `unix` on the test's clock as Claude Code's done row says it (`3:11 PM`).
fn done_clock(unix: i64) -> String {
    let local = unix + PDT;
    let h = (local / 3600).rem_euclid(24);
    let m = (local / 60).rem_euclid(60);
    let (h12, ap) = match h {
        0 => (12, "AM"),
        1..=11 => (h, "AM"),
        12 => (12, "PM"),
        _ => (h - 12, "PM"),
    };
    format!("{h12}:{m:02} {ap}")
}

/// A reset's clock as a notice names it (`5:15pm`, `5pm`).
fn reset_clock(unix: i64) -> String {
    let local = unix + PDT;
    let h = (local / 3600).rem_euclid(24);
    let m = (local / 60).rem_euclid(60);
    let (h12, ap) = match h {
        0 => (12, "am"),
        1..=11 => (h, "am"),
        12 => (12, "pm"),
        _ => (h - 12, "pm"),
    };
    if m == 0 {
        format!("{h12}{ap}")
    } else {
        format!("{h12}:{m:02}{ap}")
    }
}

/// Claude Code's screen: its launch card, the conversation `history`, the
/// composer, `footer` rows and the mode line.
fn cc(history: &[String], footer: &[String], mode_extra: &str) -> Vec<String> {
    let mut r = vec![
        " ▐▛███▛█   Claude Code v2.1.300".to_string(),
        "▝▜██████▀  Sonnet 5.5 · Claude API".to_string(),
        " ▝▝   ▝▝   /private/tmp/at.5NoF/proj".to_string(),
        String::new(),
        String::new(),
    ];
    r.extend(history.iter().cloned());
    let bar = "─".repeat(120);
    r.push(bar.clone());
    r.push("❯".to_string());
    r.push(bar);
    r.extend(footer.iter().cloned());
    r.push(format!(
        "  ⏵⏵ auto mode on (shift+tab to cycle) · ← for agents{mode_extra}"
    ));
    r
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| (*x).to_string()).collect()
}

/// A turn: `prompt` (none: the worker's own continuation), its reply rows,
/// its done row (`worked` minutes, done at `at`).
fn turn(prompt: Option<&str>, reply: &[&str], worked_s: i64, at: i64) -> Vec<String> {
    let mut r = Vec::new();
    if let Some(p) = prompt {
        r.push(format!("❯ {p}"));
        r.push(String::new());
    }
    for line in reply {
        r.push((*line).to_string());
    }
    r.push(String::new());
    let (m, sec) = (worked_s / 60, worked_s % 60);
    let took = if m > 0 {
        format!("{m}m {sec}s")
    } else {
        format!("{sec}s")
    };
    // A turn whose end is not known yet (`at` 0) is stamped when its screen
    // is first drawn ([`resolve`]).
    let done = if at == 0 {
        format!(
            "@T{}@",
            TURN_IDS.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        )
    } else {
        done_clock(at)
    };
    r.push(format!("✻ Worked for {took} · done {done}"));
    r.push(String::new());
    r
}

static TURN_IDS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);

/// The screen's turns ended at `t` where first drawn: each `@T<n>@` stamp is
/// the done clock of the first screen that drew it.
fn resolve(
    rows: &[String],
    t: i64,
    seen: &mut std::collections::BTreeMap<String, String>,
) -> Vec<String> {
    rows.iter()
        .map(|r| {
            let mut out = r.clone();
            while let Some(a) = out.find("@T") {
                let Some(len) = out[a + 2..].find('@') else {
                    break;
                };
                let key = out[a..a + 2 + len + 1].to_string();
                let clock = seen
                    .entry(key.clone())
                    .or_insert_with(|| done_clock(t))
                    .clone();
                out = out.replacen(&key, &clock, 1);
            }
            out
        })
        .collect()
}

fn live(history: &[String], prompt: &str) -> Vec<String> {
    let mut h = history.to_vec();
    h.push(format!("❯ {prompt}"));
    h.push(String::new());
    h.push("⏺ Working.".to_string());
    h.push(String::new());
    h.push("✻ Synthesizing… (18s)".to_string());
    h.push(String::new());
    let mut r = cc(&h, &[], "");
    let n = r.len();
    r[n - 1] = "  esc to interrupt".to_string();
    r
}

// ---------------------------------------------------------------- scenarios

const MIN: i64 = 60;
const HOUR: i64 = 3600;
/// 2026-09-17 15:00 PDT.
const T0: i64 = TEST_NOW + 6 * HOUR + 5 * MIN;

/// The matrix's policy. Under `[harness] usage_credits = true`: the spend
/// limit's entitlement ladder, whose tries the scenarios hand off, is a
/// ladder under the key on alone (under the key off its owner is told,
/// `turn_end`'s `a_wall_only_usage_credits_lift_is_not_continued_under_the_key_off`).
fn default_policy() -> SupervisorConfig {
    SupervisorConfig {
        model_fallback: None,
        usage_credits: true,
        ..owners_policy()
    }
}

/// The five-hour window's wall as a turn's end draws it, the vendor's
/// countdown armed (`armed`) or not.
fn session_wall(
    history: &[String],
    reset: i64,
    at: i64,
    worked_s: i64,
    armed: bool,
) -> (Vec<String>, Vec<String>) {
    let clock = reset_clock(reset);
    let mut h = history.to_vec();
    let mut reply = vec![
        "⏺ Migrating the second table.".to_string(),
        format!("  ⎿  You've hit your session limit · resets {clock} (America/Los_Angeles)"),
    ];
    if armed {
        reply.push(String::new());
        reply.push(format!(
            "⏺ Usage limit reached · continuing automatically at {clock} · esc to cancel"
        ));
    }
    let refs: Vec<&str> = reply.iter().map(String::as_str).collect();
    h.extend(turn(None, &refs, worked_s, at));
    let footer = if armed {
        vec![
            format!("  ⚠ Usage limit reached · limit resets {clock}"),
            format!("    Continuing automatically at {clock} · esc to cancel"),
        ]
    } else {
        Vec::new()
    };
    (h, footer)
}

/// The session's work turns as the screen keeps them.
fn work0() -> Vec<String> {
    turn(
        Some("carry on with the task"),
        &["⏺ Migrated the first table."],
        9 * MIN,
        T0,
    )
}

fn with(history: &[String], more: &[String]) -> Vec<String> {
    let mut h = history.to_vec();
    h.extend(more.iter().cloned());
    h
}

/// The five-hour wall's history after Esc on its countdown.
fn cancelled(h: &[String]) -> Vec<String> {
    with(
        h,
        &s(&[
            "⏺ Automatic continue cancelled · /rate-limit-options to re-arm",
            "",
        ]),
    )
}

const CANCELLED_NOTE: &str =
    "                             Automatic continue cancelled · /rate-limit-options to re-arm";

/// The spend limit, as a turn's end draws it.
fn spend_turn(prompt: Option<&str>, worked_s: i64, at: i64) -> Vec<String> {
    turn(
        prompt,
        &[
            "⏺ Running the suite.",
            "  ⎿  You've hit your monthly spend limit.",
        ],
        worked_s,
        at,
    )
}

/// After the try: a busy read of its turn two seconds on.
fn taken(k: usize, h: &[String], prompt: &str) -> Beat {
    beat(
        At::Act(k, 2),
        "the act's turn runs",
        Ev::Busy(live(h, prompt)),
    )
}

// The lab's Sonnet bucket (2.1.300): its reset, Oct 17 4:20pm PDT.
const BUCKET_RESET: i64 = 1_792_279_200;

fn bucket_armed() -> Vec<String> {
    aterm_phase::prompt::fixtures::screen(
        aterm_phase::prompt::fixtures::WALL_SONNET_BUCKET_AUTO_CONTINUE_ARMED_2_1_300,
    )
}

/// The bucket's screen with its countdown rows gone and `add` above the
/// composer.
fn bucket_with(add: &[&str], model: &str) -> Vec<String> {
    let mut rows: Vec<String> = bucket_armed()
        .into_iter()
        .filter(|r| {
            !["Usage limit reached", "Continuing automatically"]
                .iter()
                .any(|g| r.contains(g))
        })
        .map(|r| {
            r.replace(
                "Sonnet 5.5 · Claude API",
                &format!("{model} 5.5 · Claude API"),
            )
        })
        .collect();
    let top = rows
        .iter()
        .position(|r| r.starts_with('─'))
        .expect("the composer's frame");
    rows.splice(top..top, add.iter().map(|x| (*x).to_string()));
    rows
}

/// The relaunched process's redraw (as the existing tests construct it).
fn bucket_redrawn() -> Vec<String> {
    bucket_armed()
        .iter()
        .filter(|r| {
            ![
                "Usage limit reached",
                "Continuing automatically",
                "/rate-limit-options",
                "Claude Code will continue automatically",
                "to cancel the wait",
            ]
            .iter()
            .any(|g| r.contains(g))
        })
        .map(|r| r.replace("Sonnet 5.5 · Claude API", "Opus 5.5 · Claude API"))
        .collect()
}

fn above_composer(base: &[String], add: &[&str]) -> Vec<String> {
    let mut rows = base.to_vec();
    let top = rows
        .iter()
        .position(|r| r.starts_with('─'))
        .expect("the composer's frame");
    rows.splice(top..top, add.iter().map(|x| (*x).to_string()));
    rows
}

const BUCKET_NOTICE: &str =
    "You've hit your Sonnet limit · resets Oct 17 at 4:20pm (America/Los_Angeles)";

/// The transcript ending on `prompt` answered by the bucket's notice,
/// written at `at` (the A2 test's).
fn bucket_tail(prompt: &str, at: i64) -> String {
    let stamp = crate::harness::usage::rfc3339_utc(at);
    include_str!("fixtures/claude-2.1.294-transcript-session-limit.jsonl")
        .lines()
        .skip(1)
        .filter(|l| !l.contains("\"subtype\":\"informational\""))
        .map(|l| {
            let mut row = l
                .replace("seventh try", prompt)
                .replace(
                    "You've hit your session limit · resets 11:33am (America/Los_Angeles)",
                    BUCKET_NOTICE,
                )
                .replace("\"five_hour\"", "\"seven_day_sonnet\"");
            for ms in ["921", "951", "952", "953"] {
                row = row.replace(
                    &format!("\"2026-10-08T16:34:58.{ms}Z\""),
                    &format!("\"{stamp}\""),
                );
            }
            row
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

fn scenarios() -> Vec<Scenario> {
    let mut out = Vec::new();
    let reset = T0 + 12 * MIN + 2 * HOUR + 15 * MIN; // 17:27
    let w0 = work0();
    let wall_at = T0 + 12 * MIN;
    let (h_wall, f_armed) = session_wall(&w0, reset, wall_at, 12 * MIN, true);
    let walled = cc(&h_wall, &f_armed, "");
    let start = |label: &str| {
        beat(
            At::Start(0),
            label,
            Ev::Busy(live(&w0, "carry on with the task")),
        )
    };
    // ---- A: five-hour, armed; the vendor goes on by itself.
    for (tag, reply, busy_s) in [
        ("progress", "⏺ Stage 3 of 5 is in; stage 4 is next.", 0),
        (
            "done",
            "⏺ All done: the change is committed and pushed, and every suite passes.",
            0,
        ),
        ("worked", "⏺ Stage 3 of 5 is in; stage 4 is next.", 9 * MIN),
    ] {
        let fired_at = reset + 65;
        let mut h2 = h_wall.clone();
        h2.push("⏺ Usage limit reset · continuing automatically".to_string());
        h2.push(String::new());
        let end_at = fired_at + busy_s;
        h2.extend(turn(None, &[reply], busy_s, end_at));
        let fired = cc(&h2, &[], "");
        let mut beats = vec![
            start("the worker's turn"),
            beat(
                At::Start(12 * MIN),
                "the five-hour wall, armed",
                Ev::Point(walled.clone()),
            ),
        ];
        if busy_s > 0 {
            beats.push(beat(
                At::Start(fired_at - T0),
                "the vendor's continuation runs",
                Ev::Busy(live(&h_wall, "carry on with the task")),
            ));
        }
        beats.push(beat(
            At::Start(end_at - T0),
            "the vendor's continuation ends",
            Ev::Point(fired),
        ));
        beats.push(taken(0, &h2, "keep going"));
        out.push(Scenario {
            name: format!("A five-hour armed, vendor goes on ({tag})"),
            t0: T0,
            policy: default_policy(),
            host: None,
            beats,
            tail_s: 3 * HOUR,
            take: false,
            person_on_screen: false,
        });
    }
    // ---- B: five-hour, Esc on the countdown; the loop's try at the reset.
    let h_c = cancelled(&h_wall);
    let esc = cc(&h_c, &[], CANCELLED_NOTE);
    let notice_again = format!(
        "  ⎿  You've hit your session limit · resets {} (America/Los_Angeles)",
        reset_clock(reset)
    );
    for tag in [
        "served",
        "met again",
        "a person's turn met the wall",
        "a /clear",
        "done report, the done check",
    ] {
        let mut beats = vec![
            start("the worker's turn"),
            beat(
                At::Start(12 * MIN),
                "the five-hour wall, armed",
                Ev::Point(walled.clone()),
            ),
            beat(
                At::Start(22 * MIN),
                "Esc on the countdown",
                Ev::Point(esc.clone()),
            ),
        ];
        let mut h_after = h_c.clone();
        match tag {
            "a person's turn met the wall" => {
                beats.push(beat(
                    At::Start(40 * MIN),
                    "a person's prompt runs",
                    Ev::Busy(live(&h_c, "please look at the schema")),
                ));
                h_after = with(
                    &h_c,
                    &turn(
                        Some("please look at the schema"),
                        &[&notice_again],
                        20,
                        T0 + 40 * MIN + 20,
                    ),
                );
                beats.push(beat(
                    At::Start(40 * MIN + 20),
                    "the person's prompt met the wall",
                    Ev::Point(cc(&h_after, &[], "")),
                ));
            }
            "a /clear" => {
                beats.push(beat(
                    At::Start(30 * MIN),
                    "a person's /clear",
                    Ev::Point(cleared_at_an_armed_limit()),
                ));
            }
            _ => {}
        }
        if tag == "a /clear" {
            // The new task's work meets the window again; its countdown armed.
            let fresh = turn(
                Some("start on the parser"),
                &["⏺ Reading the parser."],
                5 * MIN,
                T0 + 55 * MIN,
            );
            let (h_new, f_new) = session_wall(&fresh, reset, T0 + 55 * MIN, 5 * MIN, true);
            beats.push(beat(
                At::Start(50 * MIN),
                "a person's new task runs",
                Ev::Busy(live(&[], "start on the parser")),
            ));
            beats.push(beat(
                At::Start(55 * MIN),
                "the new task meets the window, armed",
                Ev::Point(cc(&h_new, &f_new, "")),
            ));
            let mut h2 = h_new.clone();
            h2.push("⏺ Usage limit reset · continuing automatically".to_string());
            h2.push(String::new());
            h2.extend(turn(None, &["⏺ Reading the lexer next."], 0, reset + 65));
            beats.push(beat(
                At::Start(reset + 65 - T0),
                "the vendor goes on",
                Ev::Point(cc(&h2, &[], "")),
            ));
            beats.push(taken(0, &h2, "keep going"));
        } else {
            beats.push(taken(0, &h_after, "keep going"));
            match tag {
                "served" => {
                    let h3 = with(
                        &h_after,
                        &turn(
                            Some("keep going"),
                            &["⏺ Stage 3 of 5 is in; stage 4 is next."],
                            15 * MIN,
                            0,
                        ),
                    );
                    beats.push(beat(
                        At::Act(0, 15 * MIN),
                        "the try's work ends",
                        Ev::Point(cc(&h3, &[], "")),
                    ));
                    beats.push(taken(1, &h3, "keep going"));
                }
                "met again" => {
                    let h3 = with(&h_after, &turn(Some("keep going"), &[&notice_again], 0, 0));
                    // (no busy read: replace the take's busy)
                    beats.pop();
                    beats.push(beat(
                        At::Act(0, 3),
                        "the try met the wall again",
                        Ev::Point(cc(&h3, &[], "")),
                    ));
                    beats.push(taken(1, &h3, "keep going"));
                    let h4 = with(
                        &h3,
                        &turn(Some("keep going"), &["⏺ Stage 3 of 5 is in."], 12 * MIN, 0),
                    );
                    beats.push(beat(
                        At::Act(1, 12 * MIN),
                        "the second try's work ends",
                        Ev::Point(cc(&h4, &[], "")),
                    ));
                    beats.push(taken(2, &h4, "keep going"));
                }
                "done report, the done check" => {
                    let h3 = with(
                        &h_after,
                        &turn(
                            Some("keep going"),
                            &[
                                "⏺ All done: the change is committed and pushed, and every suite passes.",
                            ],
                            10 * MIN,
                            0,
                        ),
                    );
                    beats.push(beat(
                        At::Act(0, 10 * MIN),
                        "the try's work ends in a done report",
                        Ev::Point(cc(&h3, &[], "")),
                    ));
                    beats.push(taken(1, &h3, "Before you stop:"));
                    let h4 = with(
                        &h3,
                        &turn(
                            Some(crate::supervise::policy::turn_end::DONE_CHECK),
                            &["⏺ All done: verified again, every suite passes."],
                            70,
                            0,
                        ),
                    );
                    beats.push(beat(
                        At::Act(1, 72),
                        "the done check's answer: done again",
                        Ev::Point(cc(&h4, &[], "")),
                    ));
                }
                _ => {}
            }
        }
        out.push(Scenario {
            name: format!("B five-hour Esc'd, the loop's try ({tag})"),
            t0: T0,
            policy: default_policy(),
            host: None,
            beats,
            tail_s: 3 * HOUR,
            take: false,
            person_on_screen: false,
        });
    }
    // ---- C: the weekly window, no countdown.
    let weekly_notice =
        "  ⎿  You've hit your weekly limit · resets Sep 19 at 11am (America/Los_Angeles)";
    for tag in ["served", "met again"] {
        let h_w = with(
            &w0,
            &turn(
                None,
                &["⏺ Migrating the second table.", weekly_notice],
                12 * MIN,
                wall_at,
            ),
        );
        let mut beats = vec![
            start("the worker's turn"),
            beat(
                At::Start(12 * MIN),
                "the weekly wall",
                Ev::Point(cc(&h_w, &[], "")),
            ),
        ];
        if tag == "met again" {
            let h3 = with(&h_w, &turn(Some("keep going"), &[weekly_notice], 0, 0));
            beats.push(beat(
                At::Act(0, 3),
                "the try met the wall again",
                Ev::Point(cc(&h3, &[], "")),
            ));
            beats.push(taken(1, &h3, "keep going"));
        } else {
            beats.push(taken(0, &h_w, "keep going"));
            let h3 = with(
                &h_w,
                &turn(Some("keep going"), &["⏺ Stage 3 of 5 is in."], 20 * MIN, 0),
            );
            beats.push(beat(
                At::Act(0, 20 * MIN),
                "the try's work ends",
                Ev::Point(cc(&h3, &[], "")),
            ));
            beats.push(taken(1, &h3, "keep going"));
        }
        out.push(Scenario {
            name: format!("C weekly, no countdown ({tag})"),
            t0: T0,
            policy: default_policy(),
            host: None,
            beats,
            tail_s: 3 * HOUR,
            take: false,
            person_on_screen: false,
        });
    }
    // ---- D-H: the spend limit (the entitlement ladder).
    let sp0 = with(&w0, &spend_turn(None, 3 * MIN, T0 + 3 * MIN));
    let spend_wall = cc(&sp0, &[], "");
    let met = |h: &[String]| with(h, &spend_turn(Some("keep going"), 0, 0));
    for tag in [
        "met again, the ladder",
        "the try's own work meets it",
        "the try's own short turn meets it",
        "met again, a person's turn read busy ends on it",
        "met again, a /clear, a new task meets it",
        "the try's turn ends on a 529, its retry's work meets it",
        "met again, a person's turn not read busy ends on it",
    ] {
        let mut beats = vec![
            start("the worker's turn"),
            beat(
                At::Start(3 * MIN),
                "the spend limit",
                Ev::Point(spend_wall.clone()),
            ),
        ];
        let h1 = met(&sp0);
        match tag {
            "met again, the ladder" => {
                beats.push(beat(
                    At::Act(0, 30),
                    "the try met again",
                    Ev::Point(cc(&h1, &[], "")),
                ));
                let h2 = met(&h1);
                beats.push(beat(
                    At::Act(1, 30),
                    "the second try met again",
                    Ev::Point(cc(&h2, &[], "")),
                ));
                beats.push(taken(2, &h2, "keep going"));
            }
            "the try's own work meets it" => {
                beats.push(taken(0, &sp0, "keep going"));
                let h2 = with(&sp0, &spend_turn(Some("keep going"), 70 * MIN, 0));
                beats.push(beat(
                    At::Act(0, 70 * MIN),
                    "the try's work meets the limit",
                    Ev::Point(cc(&h2, &[], "")),
                ));
                beats.push(taken(1, &h2, "keep going"));
            }
            "the try's own short turn meets it" => {
                // Under `progress` (no new episode: the rung counts from its
                // end), and long enough for a handoff inside it past the try's
                // take window.
                beats.push(taken(0, &sp0, "keep going"));
                let h2 = with(&sp0, &spend_turn(Some("keep going"), 4 * MIN + 30, 0));
                beats.push(beat(
                    At::Act(0, 4 * MIN + 32),
                    "the try's short turn meets the limit",
                    Ev::Point(cc(&h2, &[], "")),
                ));
                beats.push(taken(1, &h2, "keep going"));
            }
            "met again, a person's turn read busy ends on it" => {
                beats.push(beat(
                    At::Act(0, 30),
                    "the try met again",
                    Ev::Point(cc(&h1, &[], "")),
                ));
                beats.push(beat(
                    At::Act(0, 20 * MIN),
                    "a person's turn runs",
                    Ev::Busy(live(&h1, "please look at the schema")),
                ));
                let h2 = with(
                    &h1,
                    &spend_turn(Some("please look at the schema"), 20 * MIN, 0),
                );
                beats.push(beat(
                    At::Act(0, 40 * MIN),
                    "the person's turn ends on the limit",
                    Ev::Point(cc(&h2, &[], "")),
                ));
                beats.push(taken(1, &h2, "keep going"));
            }
            "met again, a person's turn not read busy ends on it" => {
                beats.push(beat(
                    At::Act(0, 30),
                    "the try met again",
                    Ev::Point(cc(&h1, &[], "")),
                ));
                let h2 = with(&h1, &spend_turn(Some("please look at the schema"), 0, 0));
                beats.push(beat(
                    At::Act(0, 20 * MIN),
                    "a person's prompt met the limit",
                    Ev::Point(cc(&h2, &[], "")),
                ));
                beats.push(taken(1, &h2, "keep going"));
            }
            "met again, a /clear, a new task meets it" => {
                beats.push(beat(
                    At::Act(0, 30),
                    "the try met again",
                    Ev::Point(cc(&h1, &[], "")),
                ));
                beats.push(beat(
                    At::Act(0, 5 * MIN),
                    "a person's /clear",
                    Ev::Point(cleared_at_an_armed_limit()),
                ));
                beats.push(beat(
                    At::Act(0, 100 * MIN),
                    "a new task runs",
                    Ev::Busy(live(&[], "start on the parser")),
                ));
                let h2 = spend_turn(Some("start on the parser"), 5 * MIN, 0);
                beats.push(beat(
                    At::Act(0, 105 * MIN),
                    "the new task meets the limit",
                    Ev::Point(cc(&h2, &[], "")),
                ));
                beats.push(taken(1, &h2, "keep going"));
            }
            "the try's turn ends on a 529, its retry's work meets it" => {
                beats.push(taken(0, &sp0, "keep going"));
                let h2 = with(
                    &sp0,
                    &turn(
                        Some("keep going"),
                        &[
                            "⏺ Running the suite.",
                            "  ⎿  API Error: 529 Overloaded. This is a server-side issue, usually temporary — try again in a moment. If it persists, check https://status.claude.com.",
                        ],
                        20 * MIN,
                        0,
                    ),
                );
                beats.push(beat(
                    At::Act(0, 20 * MIN),
                    "the try's turn ends on a 529",
                    Ev::Point(cc(&h2, &[], "")),
                ));
                beats.push(taken(1, &h2, "try again"));
                let h3 = with(&h2, &spend_turn(Some("try again"), 60 * MIN, 0));
                beats.push(beat(
                    At::Act(1, 60 * MIN),
                    "the retry's work meets the spend limit",
                    Ev::Point(cc(&h3, &[], "")),
                ));
                beats.push(taken(2, &h3, "keep going"));
            }
            _ => unreachable!(),
        }
        out.push(Scenario {
            name: format!("D spend ({tag})"),
            t0: T0,
            policy: default_policy(),
            host: None,
            beats,
            tail_s: 8 * HOUR,
            take: false,
            person_on_screen: false,
        });
    }
    // ---- I: the Sonnet bucket, no fallback.
    let bt0 = BUCKET_RESET - 2 * 86_400;
    let busy0 = live(&s(&["❯ start the task", ""]), "carry on with the task");
    for tag in [
        "the vendor goes on",
        "Esc'd, the loop's try",
        "Esc'd, the try met again",
    ] {
        let mut beats = vec![
            beat(
                At::Start(-5 * MIN),
                "the worker's turn",
                Ev::Busy(busy0.clone()),
            ),
            beat(At::Start(0), "the bucket, armed", Ev::Point(bucket_armed())),
        ];
        match tag {
            "the vendor goes on" => {
                let fired = bucket_with(
                    &[
                        "⏺ Usage limit reset · continuing automatically",
                        "",
                        "⏺ Stage 3 of 5 is in; stage 4 is next.",
                        "",
                        "✻ Worked for 0s · done 4:21 PM",
                        "",
                    ],
                    "Sonnet",
                );
                beats.push(beat(
                    At::Start(BUCKET_RESET + 65 - bt0),
                    "the vendor goes on",
                    Ev::Point(fired.clone()),
                ));
                beats.push(taken(0, &[], "keep going"));
            }
            _ => {
                let esc = bucket_with(
                    &[
                        "⏺ Automatic continue cancelled · /rate-limit-options to re-arm",
                        "",
                    ],
                    "Sonnet",
                );
                beats.push(beat(
                    At::Start(10 * MIN),
                    "Esc on the countdown",
                    Ev::Point(esc.clone()),
                ));
                if tag.ends_with("met again") {
                    let again = above_composer(
                        &esc,
                        &[
                            "❯ keep going",
                            "  ⎿  You've hit your Sonnet limit · resets Oct 24 at 4:20pm (America/Los_Angeles)",
                            "",
                            "✻ Churned for 0s · done 4:23 PM",
                            "",
                        ],
                    );
                    beats.push(beat(
                        At::Act(0, 3),
                        "the try met a new bucket",
                        Ev::Point(again),
                    ));
                    beats.push(taken(1, &[], "keep going"));
                } else {
                    beats.push(taken(0, &[], "keep going"));
                }
            }
        }
        out.push(Scenario {
            name: format!("I bucket, no fallback ({tag})"),
            t0: bt0,
            policy: default_policy(),
            host: None,
            beats,
            tail_s: 3 * HOUR,
            take: false,
            person_on_screen: false,
        });
    }
    // ---- J/K: the bucket with the fallback's relaunch.
    let redrawn = bucket_redrawn();
    let before = bucket_tail("carry on with the task", bt0 - 600);
    for tag in [
        "relaunched, the continuation works",
        "relaunched, the continuation met by the bucket",
        "relaunched, a person's prompt met by the bucket",
        "wait:resume thrice, a person's prompt met by the bucket before the adoption",
        "wait:resume thrice, the redraw",
    ] {
        let answers: Vec<&'static str> = if tag.starts_with("wait:resume") {
            vec!["wait:resume", "wait:resume", "wait:resume", "relaunched"]
        } else {
            vec!["relaunched"]
        };
        // The bucket met at once (no work read): the redraw is a short turn.
        let mut beats = vec![beat(
            At::Start(0),
            "the bucket, armed",
            Ev::Point(bucket_armed()),
        )];
        if !tag.contains("person's prompt") {
            beats.insert(
                0,
                beat(
                    At::Start(-5 * MIN),
                    "the worker's turn",
                    Ev::Busy(busy0.clone()),
                ),
            );
        }
        let met_prompt = |prompt: &str| {
            above_composer(
                &redrawn,
                &[
                    &format!("❯ {prompt}"),
                    &format!("  ⎿  {BUCKET_NOTICE}"),
                    "",
                    "✻ Churned for 0s · done 8:49 AM",
                    "",
                ],
            )
        };
        match tag {
            "relaunched, the continuation works" => {
                beats.push(beat(
                    At::Act(0, 30),
                    "the relaunch's redraw",
                    Ev::Point(redrawn.clone()),
                ));
                beats.push(taken(1, &[], "keep going"));
                let worked = above_composer(
                    &redrawn,
                    &[
                        "❯ keep going",
                        "",
                        "⏺ Stage 3 of 5 is in.",
                        "",
                        "✻ Worked for 15m 0s · done 8:59 AM",
                        "",
                    ],
                );
                beats.push(beat(
                    At::Act(1, 15 * MIN),
                    "the continuation's work ends",
                    Ev::Point(worked),
                ));
                beats.push(taken(2, &[], "keep going"));
            }
            "relaunched, the continuation met by the bucket" => {
                beats.push(beat(
                    At::Act(0, 30),
                    "the relaunch's redraw",
                    Ev::Point(redrawn.clone()),
                ));
                beats.push(beat(
                    At::Act(1, 3),
                    "the continuation met by the bucket",
                    Ev::Point(met_prompt("keep going")),
                ));
                beats.push(taken(2, &[], "keep going"));
            }
            "relaunched, a person's prompt met by the bucket" => {
                beats.push(beat(
                    At::Act(0, 30),
                    "the relaunch's redraw",
                    Ev::Point(redrawn.clone()),
                ));
                beats.push(beat(
                    At::Act(0, 85),
                    "the transcript: the person's prompt met",
                    Ev::Tail(Some(bucket_tail(
                        "please run the integration tests",
                        bt0 + 85,
                    ))),
                ));
                beats.push(beat(
                    At::Act(0, 90),
                    "a person's prompt met by the bucket",
                    Ev::Point(met_prompt("please run the integration tests")),
                ));
                beats.push(taken(1, &[], "keep going"));
            }
            "wait:resume thrice, a person's prompt met by the bucket before the adoption" => {
                beats.push(beat(
                    At::Start(55),
                    "the transcript: the person's prompt met",
                    Ev::Tail(Some(bucket_tail(
                        "please run the integration tests",
                        bt0 + 55,
                    ))),
                ));
                beats.push(beat(
                    At::Start(60),
                    "a person's prompt met by the bucket",
                    Ev::Point(met_prompt("please run the integration tests")),
                ));
                beats.push(beat(
                    At::Act(0, 20),
                    "the relaunch's redraw",
                    Ev::Point(met_prompt("please run the integration tests")),
                ));
                beats.push(taken(1, &[], "keep going"));
            }
            "wait:resume thrice, the redraw" => {
                beats.push(beat(
                    At::Act(0, 20),
                    "the relaunch's redraw",
                    Ev::Point(redrawn.clone()),
                ));
                beats.push(taken(1, &[], "keep going"));
            }
            _ => unreachable!(),
        }
        out.push(Scenario {
            name: format!("J bucket, the fallback ({tag})"),
            t0: bt0,
            policy: owners_policy(),
            host: Some((answers, Some(before.clone()))),
            beats,
            tail_s: 3 * HOUR,
            take: false,
            person_on_screen: false,
        });
    }
    out.extend(more_scenarios());
    out.extend(grown_scenarios());
    out.extend(round8_scenarios());
    out.extend(round9_scenarios());
    out.extend(api_retry_scenarios());
    out.extend(person_scenarios());
    out
}

/// The take window on the worker's clock, the done check a wall cut, and
/// limits of mixed kinds.
fn more_scenarios() -> Vec<Scenario> {
    let mut out = Vec::new();
    let w0 = work0();
    let start = || {
        beat(
            At::Start(0),
            "the worker's turn",
            Ev::Busy(live(&w0, "carry on with the task")),
        )
    };
    let sp0 = with(&w0, &spend_turn(None, 3 * MIN, T0 + 3 * MIN));
    let spend_wall = cc(&sp0, &[], "");
    let window = true;
    // ---- TW: the spend try, its take window thirty seconds.
    for tag in [
        "the notice read inside the window, the try taken, its turn ends on it",
        "the notice read inside the window, a local command, a person's 3h turn ends on it",
        "the notice read inside the window, a local command, a person's 1h turn ends on it",
        "met inside the window, judged, a person's turn read busy ends on it",
        "met inside the window, judged, the ladder",
    ] {
        let mut beats = vec![
            start(),
            beat(
                At::Start(3 * MIN),
                "the spend limit",
                Ev::Point(spend_wall.clone()),
            ),
        ];
        let local = with(
            &sp0,
            &s(&["❯ /model", "  ⎿  Kept model as Sonnet 5.5 (default)", ""]),
        );
        let person = |h: &[String], at: i64, work: i64| -> Vec<Beat> {
            let end = with(h, &spend_turn(Some("please look at the schema"), work, 0));
            vec![
                beat(
                    At::Act(0, at),
                    "a person's turn runs",
                    Ev::Busy(live(h, "please look at the schema")),
                ),
                beat(
                    At::Act(0, at + work),
                    "the person's turn ends on the limit",
                    Ev::Point(cc(&end, &[], "")),
                ),
                taken(1, &end, "keep going"),
            ]
        };
        match tag {
            "the notice read inside the window, the try taken, its turn ends on it" => {
                beats.push(beat(
                    At::Act(0, 5),
                    "the try's screen read again",
                    Ev::Point(spend_wall.clone()),
                ));
                beats.push(beat(
                    At::Act(0, 20),
                    "the try's turn runs",
                    Ev::Busy(live(&sp0, "keep going")),
                ));
                let end = with(&sp0, &spend_turn(Some("keep going"), 19 * MIN, 0));
                beats.push(beat(
                    At::Act(0, 20 * MIN),
                    "the try's turn ends on the limit",
                    Ev::Point(cc(&end, &[], "")),
                ));
                beats.push(taken(1, &end, "keep going"));
            }
            "met inside the window, judged, a person's turn read busy ends on it" => {
                let h1 = with(&sp0, &spend_turn(Some("keep going"), 0, 0));
                beats.push(beat(
                    At::Act(0, 3),
                    "the try met again",
                    Ev::Point(cc(&h1, &[], "")),
                ));
                beats.extend(person(&h1, 20 * MIN, 20 * MIN));
            }
            "met inside the window, judged, the ladder" => {
                let h1 = with(&sp0, &spend_turn(Some("keep going"), 0, 0));
                beats.push(beat(
                    At::Act(0, 3),
                    "the try met again",
                    Ev::Point(cc(&h1, &[], "")),
                ));
                let h2 = with(&h1, &spend_turn(Some("keep going"), 0, 0));
                beats.push(beat(
                    At::Act(1, 3),
                    "the second try met again",
                    Ev::Point(cc(&h2, &[], "")),
                ));
                beats.push(taken(2, &h2, "keep going"));
            }
            _ => {
                let work = if tag.contains("3h") { 3 * HOUR } else { HOUR };
                beats.push(beat(
                    At::Act(0, 2),
                    "the try's screen read again",
                    Ev::Point(spend_wall.clone()),
                ));
                beats.push(beat(
                    At::Act(0, 4),
                    "a person's local command under it",
                    Ev::Point(cc(&local, &[], "")),
                ));
                beats.extend(person(&local, 10, work));
            }
        }
        out.push(Scenario {
            name: format!("TW spend, take window ({tag})"),
            t0: T0,
            policy: default_policy(),
            host: None,
            beats,
            tail_s: 8 * HOUR,
            take: window,
            person_on_screen: false,
        });
    }
    // ---- TW-long: the try taken, its own turn longer than the try's rung.
    for work in [3 * HOUR, 90 * MIN] {
        let mut beats = vec![
            start(),
            beat(
                At::Start(3 * MIN),
                "the spend limit",
                Ev::Point(spend_wall.clone()),
            ),
            beat(
                At::Act(0, 5),
                "the try's screen read again",
                Ev::Point(spend_wall.clone()),
            ),
            beat(
                At::Act(0, 20),
                "the try's turn runs",
                Ev::Busy(live(&sp0, "keep going")),
            ),
        ];
        let end = with(&sp0, &spend_turn(Some("keep going"), work, 0));
        beats.push(beat(
            At::Act(0, 20 + work),
            "the try's turn ends on the limit",
            Ev::Point(cc(&end, &[], "")),
        ));
        beats.push(taken(1, &end, "keep going"));
        out.push(Scenario {
            name: format!(
                "TW spend, take window (the try taken, its own {}m turn ends on it)",
                work / 60
            ),
            t0: T0,
            policy: default_policy(),
            host: None,
            beats,
            tail_s: 8 * HOUR,
            take: window,
            person_on_screen: false,
        });
    }
    // ---- TW-draft: the try's Enter did not take (its line left in the composer).
    {
        let mut drafted = spend_wall.clone();
        let caret = drafted
            .iter()
            .rposition(|r| r == "❯")
            .expect("the composer");
        drafted[caret] = "❯ keep going".to_string();
        let beats = vec![
            start(),
            beat(
                At::Start(3 * MIN),
                "the spend limit",
                Ev::Point(spend_wall.clone()),
            ),
            beat(
                At::Act(0, 2),
                "the try's line left in the composer",
                Ev::Point(drafted.clone()),
            ),
            taken(1, &sp0, "keep going"),
        ];
        out.push(Scenario {
            name: "TW spend, take window (the try's Enter did not take)".to_string(),
            t0: T0,
            policy: default_policy(),
            host: None,
            beats,
            tail_s: 8 * HOUR,
            take: window,
            person_on_screen: false,
        });
    }
    // ---- O: a model bucket no reset ends (the entitlement ladder).
    let ob0 = with(
        &w0,
        &turn(
            None,
            &["⏺ Running the suite.", "  ⎿  You've hit your Opus limit"],
            3 * MIN,
            T0 + 3 * MIN,
        ),
    );
    let ob_wall = cc(&ob0, &[], "");
    let ob_met = |h: &[String], p: &str, work: i64| {
        with(
            h,
            &turn(Some(p), &["  ⎿  You've hit your Opus limit"], work, 0),
        )
    };
    for tag in [
        "met again, the ladder",
        "met again, a person's 3h turn read busy ends on it",
        "take window: the notice read inside, a local command, a person's 3h turn ends on it",
        "take window: the try taken, its own 3h turn ends on it",
    ] {
        let mut beats = vec![
            start(),
            beat(
                At::Start(3 * MIN),
                "the Opus bucket, no reset",
                Ev::Point(ob_wall.clone()),
            ),
        ];
        let mut take = false;
        match tag {
            "met again, the ladder" => {
                let h1 = ob_met(&ob0, "keep going", 0);
                beats.push(beat(
                    At::Act(0, 30),
                    "the try met again",
                    Ev::Point(cc(&h1, &[], "")),
                ));
                let h2 = ob_met(&h1, "keep going", 0);
                beats.push(beat(
                    At::Act(1, 30),
                    "the second try met again",
                    Ev::Point(cc(&h2, &[], "")),
                ));
                beats.push(taken(2, &h2, "keep going"));
            }
            "met again, a person's 3h turn read busy ends on it" => {
                let h1 = ob_met(&ob0, "keep going", 0);
                beats.push(beat(
                    At::Act(0, 30),
                    "the try met again",
                    Ev::Point(cc(&h1, &[], "")),
                ));
                beats.push(beat(
                    At::Act(0, 20 * MIN),
                    "a person's turn runs",
                    Ev::Busy(live(&h1, "please look at the schema")),
                ));
                let h2 = ob_met(&h1, "please look at the schema", 3 * HOUR);
                beats.push(beat(
                    At::Act(0, 20 * MIN + 3 * HOUR),
                    "the person's turn ends on the bucket",
                    Ev::Point(cc(&h2, &[], "")),
                ));
                beats.push(taken(1, &h2, "keep going"));
            }
            "take window: the notice read inside, a local command, a person's 3h turn ends on it" =>
            {
                take = window;
                let local = with(
                    &ob0,
                    &s(&["❯ /model", "  ⎿  Kept model as Opus 5.5 (default)", ""]),
                );
                beats.push(beat(
                    At::Act(0, 2),
                    "the try's screen read again",
                    Ev::Point(ob_wall.clone()),
                ));
                beats.push(beat(
                    At::Act(0, 4),
                    "a person's local command under it",
                    Ev::Point(cc(&local, &[], "")),
                ));
                beats.push(beat(
                    At::Act(0, 10),
                    "a person's turn runs",
                    Ev::Busy(live(&local, "please look at the schema")),
                ));
                let h2 = ob_met(&local, "please look at the schema", 3 * HOUR);
                beats.push(beat(
                    At::Act(0, 10 + 3 * HOUR),
                    "the person's turn ends on the bucket",
                    Ev::Point(cc(&h2, &[], "")),
                ));
                beats.push(taken(1, &h2, "keep going"));
            }
            "take window: the try taken, its own 3h turn ends on it" => {
                take = window;
                beats.push(beat(
                    At::Act(0, 5),
                    "the try's screen read again",
                    Ev::Point(ob_wall.clone()),
                ));
                beats.push(beat(
                    At::Act(0, 20),
                    "the try's turn runs",
                    Ev::Busy(live(&ob0, "keep going")),
                ));
                let h2 = ob_met(&ob0, "keep going", 3 * HOUR);
                beats.push(beat(
                    At::Act(0, 20 + 3 * HOUR),
                    "the try's turn ends on the bucket",
                    Ev::Point(cc(&h2, &[], "")),
                ));
                beats.push(taken(1, &h2, "keep going"));
            }
            _ => unreachable!(),
        }
        out.push(Scenario {
            name: format!("O Opus bucket, no reset ({tag})"),
            t0: T0,
            policy: default_policy(),
            host: None,
            beats,
            tail_s: 8 * HOUR,
            take,
            person_on_screen: false,
        });
    }
    // ---- K: the done check a wall cut, waited out, its retry.
    let reset = T0 + 12 * MIN + 2 * HOUR + 15 * MIN;
    let wall_at = T0 + 12 * MIN;
    let (h_wall, f_armed) = session_wall(&w0, reset, wall_at, 12 * MIN, true);
    let h_c = cancelled(&h_wall);
    let weekly_notice =
        "  ⎿  You've hit your weekly limit · resets Sep 19 at 11am (America/Los_Angeles)";
    {
        let mut beats = vec![
            start(),
            beat(
                At::Start(12 * MIN),
                "the five-hour wall, armed",
                Ev::Point(cc(&h_wall, &f_armed, "")),
            ),
            beat(
                At::Start(22 * MIN),
                "Esc on the countdown",
                Ev::Point(cc(&h_c, &[], CANCELLED_NOTE)),
            ),
            taken(0, &h_c, "keep going"),
        ];
        let h3 = with(
            &h_c,
            &turn(
                Some("keep going"),
                &["⏺ All done: the change is committed and pushed, and every suite passes."],
                10 * MIN,
                0,
            ),
        );
        beats.push(beat(
            At::Act(0, 10 * MIN),
            "the try's work ends in a done report",
            Ev::Point(cc(&h3, &[], "")),
        ));
        let check = crate::supervise::policy::turn_end::DONE_CHECK;
        let h4 = with(&h3, &turn(Some(check), &[weekly_notice], 0, 0));
        beats.push(beat(
            At::Act(1, 3),
            "the done check met by the weekly limit",
            Ev::Point(cc(&h4, &[], "")),
        ));
        beats.push(taken(2, &h4, "keep going"));
        let h5 = with(
            &h4,
            &turn(
                Some("keep going"),
                &["⏺ All done: verified again, every suite passes."],
                70,
                0,
            ),
        );
        beats.push(beat(
            At::Act(2, 72),
            "the retry's answer: done again",
            Ev::Point(cc(&h5, &[], "")),
        ));
        out.push(Scenario {
            name: "K the done check cut by the weekly limit, its retry's done reply".to_string(),
            t0: T0,
            policy: default_policy(),
            host: None,
            beats,
            tail_s: 3 * HOUR,
            take: false,
            person_on_screen: false,
        });
    }
    // ---- M: limits of mixed kinds.
    for tag in [
        "the spend try met by the five-hour window",
        "the five-hour try met by the weekly window",
        "the vendor's continuation met by the weekly window",
        "the five-hour try met by the spend limit",
    ] {
        let mut beats = vec![start()];
        let five_notice = format!(
            "  ⎿  You've hit your session limit · resets {} (America/Los_Angeles)",
            reset_clock(T0 + 5 * HOUR)
        );
        match tag {
            "the spend try met by the five-hour window" => {
                beats.push(beat(
                    At::Start(3 * MIN),
                    "the spend limit",
                    Ev::Point(spend_wall.clone()),
                ));
                let h1 = with(&sp0, &turn(Some("keep going"), &[&five_notice], 0, 0));
                beats.push(beat(
                    At::Act(0, 3),
                    "the try met by the five-hour window",
                    Ev::Point(cc(&h1, &[], "")),
                ));
                beats.push(taken(1, &h1, "keep going"));
            }
            "the five-hour try met by the weekly window"
            | "the five-hour try met by the spend limit" => {
                beats.push(beat(
                    At::Start(12 * MIN),
                    "the five-hour wall, armed",
                    Ev::Point(cc(&h_wall, &f_armed, "")),
                ));
                beats.push(beat(
                    At::Start(22 * MIN),
                    "Esc on the countdown",
                    Ev::Point(cc(&h_c, &[], CANCELLED_NOTE)),
                ));
                let met = if tag.ends_with("spend limit") {
                    "  ⎿  You've hit your monthly spend limit."
                } else {
                    weekly_notice
                };
                let h1 = with(&h_c, &turn(Some("keep going"), &[met], 0, 0));
                beats.push(beat(
                    At::Act(0, 3),
                    "the try met by another limit",
                    Ev::Point(cc(&h1, &[], "")),
                ));
                beats.push(taken(1, &h1, "keep going"));
            }
            "the vendor's continuation met by the weekly window" => {
                beats.push(beat(
                    At::Start(12 * MIN),
                    "the five-hour wall, armed",
                    Ev::Point(cc(&h_wall, &f_armed, "")),
                ));
                let mut h2 = h_wall.clone();
                h2.push("⏺ Usage limit reset · continuing automatically".to_string());
                h2.push(String::new());
                h2.extend(turn(None, &[weekly_notice], 0, reset + 65));
                beats.push(beat(
                    At::Start(reset + 65 - T0),
                    "the vendor's continuation met by the weekly window",
                    Ev::Point(cc(&h2, &[], "")),
                ));
                beats.push(taken(0, &h2, "keep going"));
            }
            _ => unreachable!(),
        }
        out.push(Scenario {
            name: format!("M mixed ({tag})"),
            t0: T0,
            policy: default_policy(),
            host: None,
            beats,
            tail_s: 3 * HOUR,
            take: false,
            person_on_screen: false,
        });
    }
    out
}

/// The five-hour window's notice again (the try met it: its reset placed
/// early).
fn five_again(reset: i64) -> String {
    format!(
        "  ⎿  You've hit your session limit · resets {} (America/Los_Angeles)",
        reset_clock(reset)
    )
}

/// A person's `/compact` running (its spinner, `esc to interrupt`).
fn compacting(history: &[String]) -> Vec<String> {
    let mut h = history.to_vec();
    h.extend(s(&[
        "❯ /compact",
        "",
        "✳ Compacting conversation… (0s)",
        "",
    ]));
    let mut r = cc(&h, &[], "");
    let n = r.len();
    r[n - 1] = "  ⏵⏵ auto mode on (shift+tab to cycle) · esc to interrupt".to_string();
    r
}

/// The conversation after a person's `/compact`: redrawn from the
/// compaction's banner, the kept turn, and the command's answer.
fn compacted(kept: &str) -> Vec<String> {
    cc(
        &s(&[
            "✻ Conversation compacted (ctrl+o for history)",
            "",
            "",
            kept,
            "",
            "✻ Worked for 9m 0s · done 3:09 PM",
            "",
            "❯ /compact",
            "  ⎿  Compacted (ctrl+o to see full summary)",
            "",
        ]),
        &[],
        "",
    )
}

/// The weekly window's wall at `at`, its reset at `reset` named with its
/// date (`resets Sep 17 at 5:27pm`), the vendor's countdown armed or not.
fn weekly_wall(history: &[String], reset: i64, at: i64, armed: bool) -> (Vec<String>, Vec<String>) {
    let clock = format!("Sep 17 at {}", reset_clock(reset));
    let mut h = history.to_vec();
    let mut reply = vec![
        "⏺ Migrating the second table.".to_string(),
        format!("  ⎿  You've hit your weekly limit · resets {clock} (America/Los_Angeles)"),
    ];
    if armed {
        reply.push(String::new());
        reply.push(format!(
            "⏺ Usage limit reached · continuing automatically at {clock} · esc to cancel"
        ));
    }
    let refs: Vec<&str> = reply.iter().map(String::as_str).collect();
    h.extend(turn(None, &refs, 12 * MIN, at));
    let footer = if armed {
        vec![
            format!("  ⚠ Usage limit reached · limit resets {clock}"),
            format!("    Continuing automatically at {clock} · esc to cancel"),
        ]
    } else {
        Vec::new()
    };
    (h, footer)
}

fn land_policy() -> SupervisorConfig {
    SupervisorConfig {
        autopilot: crate::supervise::config::Autopilot::Land,
        ..default_policy()
    }
}

/// THE SHAPES GROWN IN ROUND 7 (the seed's real inputs the matrix lacked):
/// a handoff in the read latency past a try's take window; acts in a
/// limit's place other than the done check; a cancelled notice under the
/// screen a try was typed at; a loop started as Off lifts; a done check
/// already answered, then a person's turn; the weekly window reached; a
/// usage window's try met by a model bucket; tries in a row; a person's
/// typing grace; a `/compact` between; the vendor's `continuing
/// automatically` words standing with no countdown.
#[allow(clippy::too_many_lines)]
fn grown_scenarios() -> Vec<Scenario> {
    let mut out = Vec::new();
    let w0 = work0();
    let start = || {
        beat(
            At::Start(0),
            "the worker's turn",
            Ev::Busy(live(&w0, "carry on with the task")),
        )
    };
    let reset = T0 + 12 * MIN + 2 * HOUR + 15 * MIN;
    let wall_at = T0 + 12 * MIN;
    let (h_wall, f_armed) = session_wall(&w0, reset, wall_at, 12 * MIN, true);
    let walled = cc(&h_wall, &f_armed, "");
    let h_c = cancelled(&h_wall);
    let esc = cc(&h_c, &[], CANCELLED_NOTE);
    let again = five_again(reset);
    let sp0 = with(&w0, &spend_turn(None, 3 * MIN, T0 + 3 * MIN));
    let spend_wall = cc(&sp0, &[], "");
    let done = "⏺ All done: the change is committed and pushed, and every suite passes.";
    let force = "⏺ Shall I force-push the rewritten history over main?";
    let progress = "⏺ Stage 3 of 5 is in; stage 4 is next.";
    let check = "If the task is finished and verified, end your reply with ATERM-TASK-DONE on a line by itself and stop; otherwise continue.";
    let b_head = || {
        vec![
            start(),
            beat(
                At::Start(12 * MIN),
                "the five-hour wall, armed",
                Ev::Point(walled.clone()),
            ),
            beat(
                At::Start(22 * MIN),
                "Esc on the countdown",
                Ev::Point(esc.clone()),
            ),
        ]
    };
    let sc = |name: String, policy: SupervisorConfig, beats: Vec<Beat>, tail_s: i64, take: bool| {
        Scenario {
            name,
            t0: T0,
            policy,
            host: None,
            beats,
            tail_s,
            take,
            person_on_screen: false,
        }
    };
    // ---- R1: a handoff in the read latency past the try's take window.
    for (tag, first_read, busy_at) in [
        ("the first read after the try 35 s on", 35, 40),
        ("the first read 2 s on, the busy read 32 s on", 2, 32),
        ("the first read after the try 70 s on", 70, 75),
    ] {
        let mut beats = vec![
            start(),
            beat(
                At::Start(3 * MIN),
                "the spend limit",
                Ev::Point(spend_wall.clone()),
            ),
        ];
        let read = beat(
            At::Act(0, first_read),
            "the try's screen read",
            Ev::Point(spend_wall.clone()),
        );
        let mark = beat(At::Act(0, 30), "(a handoff instant)", Ev::Mark);
        if first_read < 30 {
            beats.extend([read, mark]);
        } else if first_read < 2 * TAKE_S {
            beats.extend([mark, read]);
        } else {
            // A handoff past twice the window, the try unread still: the
            // seed once read such a try as met at the screen it was typed at
            // (ATERM-KI-1016-1, follow-2).
            let late = beat(At::Act(0, first_read - 5), "(a handoff instant)", Ev::Mark);
            beats.extend([mark, late, read]);
        }
        beats.push(beat(
            At::Act(0, busy_at),
            "the try's turn runs",
            Ev::Busy(live(&sp0, "keep going")),
        ));
        let end = with(&sp0, &spend_turn(Some("keep going"), 3 * HOUR, 0));
        beats.push(beat(
            At::Act(0, busy_at + 3 * HOUR),
            "the try's 3h turn ends on the limit",
            Ev::Point(cc(&end, &[], "")),
        ));
        beats.push(taken(1, &end, "keep going"));
        out.push(sc(
            format!("R1 spend, take window, the read latency ({tag})"),
            default_policy(),
            beats,
            8 * HOUR,
            true,
        ));
    }
    // ---- R2: acts in a limit's place other than the done check. The
    // vendor's continuation at the reset, the worker's reply under it, the
    // limit's rows scrolled off.
    for (tag, reply, policy) in [
        ("the reversible answer", force, default_policy()),
        ("the done check (control)", done, default_policy()),
        ("Land: the land instruction", done, land_policy()),
        ("Land: the land answer", force, land_policy()),
    ] {
        for (take, late) in [(false, false), (true, false), (true, true)] {
            let fired = cc(
                &turn(
                    None,
                    &["⏺ Usage limit reset · continuing automatically", "", reply],
                    0,
                    reset + 65,
                ),
                &[],
                "",
            );
            let mut beats = vec![
                start(),
                beat(
                    At::Start(12 * MIN),
                    "the five-hour wall, armed",
                    Ev::Point(walled.clone()),
                ),
                beat(
                    At::Start(reset + 65 - T0),
                    "the vendor's continuation, its limit scrolled off",
                    Ev::Point(fired.clone()),
                ),
            ];
            let (read_at, busy_at) = if late { (70, 75) } else { (5, 25) };
            if take {
                if late {
                    beats.push(beat(At::Act(0, 30), "(a handoff instant)", Ev::Mark));
                    beats.push(beat(At::Act(0, 65), "(a handoff instant)", Ev::Mark));
                }
                beats.push(beat(
                    At::Act(0, read_at),
                    "the act's screen read again",
                    Ev::Point(fired.clone()),
                ));
                if !late {
                    beats.push(beat(At::Act(0, 20), "(a handoff instant)", Ev::Mark));
                }
                beats.push(beat(
                    At::Act(0, busy_at),
                    "the act's turn runs",
                    Ev::Busy(live(&fired[5..fired.len() - 4], "(the act)")),
                ));
            } else {
                beats.push(taken(0, &fired[5..fired.len() - 4], "(the act)"));
            }
            let answered = with(
                &turn(
                    None,
                    &["⏺ Usage limit reset · continuing automatically", "", reply],
                    0,
                    reset + 65,
                ),
                &turn(Some("(the act)"), &[progress], 10 * MIN, 0),
            );
            beats.push(beat(
                At::Act(0, 10 * MIN + busy_at),
                "the act's answer: progress",
                Ev::Point(cc(&answered, &[], "")),
            ));
            beats.push(taken(1, &answered, "keep going"));
            out.push(sc(
                format!(
                    "R2 in the limit's place: {tag}{}{}",
                    if take { ", take window" } else { "" },
                    if late {
                        " (the first read 70 s on)"
                    } else {
                        ""
                    }
                ),
                policy.clone(),
                beats,
                3 * HOUR,
                take,
            ));
        }
    }
    // ---- R2b: a land nudge in the limit's place: the instruction typed at
    // an ordinary point, its turn meets the five-hour wall, the vendor goes
    // on and the reply is a done report.
    {
        let p0 = cc(
            &with(&w0, &turn(None, &[progress], 0, T0 + 2 * MIN)),
            &[],
            "",
        );
        let mut beats = vec![
            start(),
            beat(At::Start(2 * MIN), "the worker's progress", Ev::Point(p0)),
        ];
        let land_turn = with(
            &w0,
            &turn(Some("(land)"), &["⏺ Landing the change."], 10 * MIN, 0),
        );
        beats.push(taken(0, &w0, "(land)"));
        let (h_w2, f_w2) = session_wall(&land_turn, reset, 0, 10 * MIN, true);
        beats.push(beat(
            At::Act(0, 10 * MIN),
            "the land turn meets the five-hour wall, armed",
            Ev::Point(cc(&h_w2, &f_w2, "")),
        ));
        let fired = cc(
            &turn(
                None,
                &["⏺ Usage limit reset · continuing automatically", "", done],
                0,
                reset + 65,
            ),
            &[],
            "",
        );
        beats.push(beat(
            At::Start(reset + 65 - T0),
            "the vendor's continuation, its limit scrolled off: done",
            Ev::Point(fired.clone()),
        ));
        beats.push(taken(1, &fired[5..fired.len() - 4], "(nudge)"));
        out.push(sc(
            "R2 in the limit's place: Land: a land nudge".to_string(),
            land_policy(),
            beats,
            3 * HOUR,
            false,
        ));
    }
    // ---- R2c: Land, its landing spent (the instruction, then its nudges,
    // each answered with progress): nothing more is typed for it.
    {
        let p0 = cc(
            &with(&w0, &turn(None, &[progress], 0, T0 + 2 * MIN)),
            &[],
            "",
        );
        let mut beats = vec![
            start(),
            beat(At::Start(2 * MIN), "the worker's progress", Ev::Point(p0)),
        ];
        let mut h = w0.clone();
        for k in 0..3 {
            beats.push(taken(k, &h, "(land)"));
            h = with(
                &h,
                &turn(Some("(land)"), &["⏺ Stage 4 of 5 is in."], 10 * MIN, 0),
            );
            beats.push(beat(
                At::Act(k, 10 * MIN),
                "the land act's turn ends: progress",
                Ev::Point(cc(&h, &[], "")),
            ));
        }
        out.push(sc(
            "R2 Land, the landing spent".to_string(),
            land_policy(),
            beats,
            3 * HOUR,
            false,
        ));
    }
    // ---- R3: the cancelled notice under the screen the try was typed at
    // (the take window modelled).
    for tag in [
        "read again inside the window",
        "no read before the busy one",
    ] {
        let mut beats = b_head();
        if tag.starts_with("read again") {
            beats.push(beat(
                At::Act(0, 3),
                "the try's screen read again (the cancel under it)",
                Ev::Point(esc.clone()),
            ));
            beats.push(beat(At::Act(0, 6), "(a handoff instant)", Ev::Mark));
        }
        beats.push(beat(
            At::Act(0, 10),
            "the try's turn runs",
            Ev::Busy(live(&h_c, "keep going")),
        ));
        let h3 = with(&h_c, &turn(Some("keep going"), &[progress], 15 * MIN, 0));
        beats.push(beat(
            At::Act(0, 10 + 15 * MIN),
            "the try's work ends",
            Ev::Point(cc(&h3, &[], "")),
        ));
        beats.push(taken(1, &h3, "keep going"));
        out.push(sc(
            format!("R3 five-hour Esc'd, take window, the cancel under the try ({tag})"),
            default_policy(),
            beats,
            3 * HOUR,
            true,
        ));
    }
    // ---- R4: Off stood and lifted (a loop the host starts as it lifts).
    {
        // (a) Inside the try's take window.
        let mut beats = b_head();
        beats.push(beat(
            At::Act(0, 3),
            "the try's screen read again",
            Ev::Point(esc.clone()),
        ));
        beats.push(beat(At::Act(0, 5), "Off stood and lifted", Ev::OffLifted));
        beats.push(beat(
            At::Act(0, 12),
            "the try's turn runs",
            Ev::Busy(live(&h_c, "keep going")),
        ));
        let h3 = with(&h_c, &turn(Some("keep going"), &[progress], 15 * MIN, 0));
        beats.push(beat(
            At::Act(0, 12 + 15 * MIN),
            "the try's work ends",
            Ev::Point(cc(&h3, &[], "")),
        ));
        beats.push(taken(1, &h3, "keep going"));
        out.push(sc(
            "R4 Off lifts inside the try's take window (five-hour Esc'd)".to_string(),
            default_policy(),
            beats,
            3 * HOUR,
            true,
        ));
    }
    {
        // (b) A done check the weekly limit cut, Off lifting before its retry.
        let weekly_notice =
            "  ⎿  You've hit your weekly limit · resets Sep 19 at 11am (America/Los_Angeles)";
        let mut beats = b_head();
        beats.push(taken(0, &h_c, "keep going"));
        let h3 = with(&h_c, &turn(Some("keep going"), &[done], 10 * MIN, 0));
        beats.push(beat(
            At::Act(0, 10 * MIN),
            "the try's work ends in a done report",
            Ev::Point(cc(&h3, &[], "")),
        ));
        let h4 = with(&h3, &turn(Some(check), &[weekly_notice], 0, 0));
        beats.push(beat(
            At::Act(1, 3),
            "the done check met by the weekly limit",
            Ev::Point(cc(&h4, &[], "")),
        ));
        beats.push(beat(
            At::Act(1, 10 * MIN),
            "Off stood and lifted",
            Ev::OffLifted,
        ));
        beats.push(taken(2, &h4, "keep going"));
        let h5 = with(
            &h4,
            &turn(
                Some("keep going"),
                &["⏺ All done: verified again, every suite passes."],
                70,
                0,
            ),
        );
        beats.push(beat(
            At::Act(2, 72),
            "the retry's answer: done again",
            Ev::Point(cc(&h5, &[], "")),
        ));
        beats.push(taken(3, &h5, "(the check)"));
        out.push(sc(
            "R4 Off lifts at a done check the weekly limit cut".to_string(),
            default_policy(),
            beats,
            3 * HOUR,
            false,
        ));
    }
    {
        // (c) The spend limit's ladder, Off lifting between its tries.
        let h1 = with(&sp0, &spend_turn(Some("keep going"), 0, 0));
        let beats = vec![
            start(),
            beat(
                At::Start(3 * MIN),
                "the spend limit",
                Ev::Point(spend_wall.clone()),
            ),
            beat(
                At::Act(0, 30),
                "the try met again",
                Ev::Point(cc(&h1, &[], "")),
            ),
            beat(At::Act(0, 10 * MIN), "Off stood and lifted", Ev::OffLifted),
            taken(1, &h1, "keep going"),
        ];
        out.push(sc(
            "R4 Off lifts between the spend limit's tries".to_string(),
            default_policy(),
            beats,
            8 * HOUR,
            false,
        ));
    }
    {
        // (d) Land: the instruction typed, Off lifting before its answer.
        let p0 = cc(
            &with(&w0, &turn(None, &[progress], 0, T0 + 2 * MIN)),
            &[],
            "",
        );
        let land_turn = with(
            &w0,
            &turn(Some("(land)"), &["⏺ Stage 4 of 5 is in."], 10 * MIN, 0),
        );
        let beats = vec![
            start(),
            beat(At::Start(2 * MIN), "the worker's progress", Ev::Point(p0)),
            taken(0, &w0, "(land)"),
            beat(At::Act(0, 5 * MIN), "Off stood and lifted", Ev::OffLifted),
            beat(
                At::Act(0, 10 * MIN),
                "the land turn ends: progress",
                Ev::Point(cc(&land_turn, &[], "")),
            ),
            taken(1, &land_turn, "(next)"),
        ];
        out.push(sc(
            "R4 Off lifts during the land instruction's turn (Land)".to_string(),
            land_policy(),
            beats,
            3 * HOUR,
            false,
        ));
    }
    // (e) A task's end, then Off stood and lifted: the landing — the clean
    // defense in words that answers Land's instruction (FOREVER LAND; the
    // retired token lands nothing, and Cruise never ends, so main's token
    // and done-check cells have no end to hand over). A loop stopped in the
    // end's window ([`window_cells`]) leaves books a loop started as Off
    // lifts reads the end from.
    {
        let defense = [
            "⏺ Merged the parser into main and pushed it; every suite passes.",
            "",
            "  Nothing left: all of it is merged into the remote main branch.",
        ];
        let p0 = cc(
            &with(&w0, &turn(None, &[progress], 0, T0 + 2 * MIN)),
            &[],
            "",
        );
        // The land act's own `❯` row over its answer, as Claude Code draws
        // the words typed: a successor lands a reply no read of its own saw
        // run only where its act's taking is that plain (the land act's
        // seed, `TurnEndState::seed_land_act`); under a `❯ (land)` stand-in
        // the books' landing was owed Land's act again.
        let land = crate::supervise::policy::turn_end::LAND;
        let landed = with(&w0, &turn(Some(land), &defense, 10 * MIN, 0));
        let beats = vec![
            start(),
            beat(At::Start(2 * MIN), "the worker's progress", Ev::Point(p0)),
            taken(0, &w0, land),
            beat(
                At::Act(0, 10 * MIN),
                "the land turn ends: a clean defense (landed)",
                Ev::Point(cc(&landed, &[], "")),
            ),
            beat(
                At::Act(0, 10 * MIN + 30),
                "Off stood and lifted",
                Ev::OffLifted,
            ),
            beat(At::Act(0, 10 * MIN + 90), "(a handoff instant)", Ev::Mark),
        ];
        out.push(sc(
            "R4 Off lifts after the landing (Land, in words)".to_string(),
            land_policy(),
            beats,
            3 * HOUR,
            false,
        ));
    }
    // ---- R6: the weekly window reached within the run.
    let (hw, fw) = weekly_wall(&w0, reset, wall_at, true);
    let wk_walled = cc(&hw, &fw, "");
    let hw_c = cancelled(&hw);
    let wk_esc = cc(&hw_c, &[], CANCELLED_NOTE);
    let wk_again = format!(
        "  ⎿  You've hit your weekly limit · resets Sep 17 at {} (America/Los_Angeles)",
        reset_clock(reset)
    );
    for tag in [
        "the vendor goes on",
        "Esc'd, the loop's try served",
        "Esc'd, the try met again",
        "Esc'd, the try met again twice",
    ] {
        let mut beats = vec![
            start(),
            beat(
                At::Start(12 * MIN),
                "the weekly wall, armed",
                Ev::Point(wk_walled.clone()),
            ),
        ];
        if tag == "the vendor goes on" {
            let mut h2 = hw.clone();
            h2.push("⏺ Usage limit reset · continuing automatically".to_string());
            h2.push(String::new());
            h2.extend(turn(None, &[progress], 0, reset + 65));
            beats.push(beat(
                At::Start(reset + 65 - T0),
                "the vendor goes on",
                Ev::Point(cc(&h2, &[], "")),
            ));
            beats.push(taken(0, &h2, "keep going"));
        } else {
            beats.push(beat(
                At::Start(22 * MIN),
                "Esc on the countdown",
                Ev::Point(wk_esc.clone()),
            ));
            let mut h_at = hw_c.clone();
            let tries = match tag {
                "Esc'd, the try met again" => 1,
                "Esc'd, the try met again twice" => 2,
                _ => 0,
            };
            for k in 0..tries {
                h_at = with(&h_at, &turn(Some("keep going"), &[&wk_again], 0, 0));
                beats.push(beat(
                    At::Act(k, 3),
                    "the try met the weekly window again",
                    Ev::Point(cc(&h_at, &[], "")),
                ));
            }
            beats.push(taken(tries, &h_at, "keep going"));
            let h3 = with(&h_at, &turn(Some("keep going"), &[progress], 15 * MIN, 0));
            beats.push(beat(
                At::Act(tries, 15 * MIN),
                "the try's work ends",
                Ev::Point(cc(&h3, &[], "")),
            ));
            beats.push(taken(tries + 1, &h3, "keep going"));
        }
        out.push(sc(
            format!("R6 weekly, its reset reached ({tag})"),
            default_policy(),
            beats,
            3 * HOUR,
            false,
        ));
    }
    // ---- R7: the five-hour window's try met by a model bucket.
    for (tag, notice, take) in [
        (
            "the Opus bucket, no reset",
            "  ⎿  You've hit your Opus limit",
            false,
        ),
        (
            "the Sonnet bucket, its reset named",
            "  ⎿  You've hit your Sonnet limit · resets Sep 19 at 4:20pm (America/Los_Angeles)",
            false,
        ),
        (
            "the Opus bucket, no reset, take window",
            "  ⎿  You've hit your Opus limit",
            true,
        ),
    ] {
        let mut beats = b_head();
        let h1 = with(&h_c, &turn(Some("keep going"), &[notice], 0, 0));
        beats.push(beat(
            At::Act(0, 3),
            "the try met by a model bucket",
            Ev::Point(cc(&h1, &[], "")),
        ));
        beats.push(taken(1, &h1, "keep going"));
        let h2 = with(&h1, &turn(Some("keep going"), &[progress], 15 * MIN, 0));
        beats.push(beat(
            At::Act(1, 15 * MIN),
            "the bucket's try's work ends",
            Ev::Point(cc(&h2, &[], "")),
        ));
        beats.push(taken(2, &h2, "keep going"));
        out.push(sc(
            format!("R7 five-hour try met by a bucket ({tag})"),
            default_policy(),
            beats,
            8 * HOUR,
            take,
        ));
    }
    // ---- R7b: the five-hour window's try TAKEN, its own 20-minute turn
    // ending on a bucket that names no reset: the bucket's appearance is that
    // turn's end, judged at once (work read), take window or not — not the
    // window's end after it (ATERM-KI-1016-1, "a loop that follows another").
    for take in [false, true] {
        let mut beats = b_head();
        beats.push(taken(0, &h_c, "keep going"));
        let h1 = with(
            &h_c,
            &turn(
                Some("keep going"),
                &[progress, "  ⎿  You've hit your Opus limit"],
                20 * MIN,
                0,
            ),
        );
        beats.push(beat(
            At::Act(0, 20 * MIN),
            "the try's own turn meets a model bucket",
            Ev::Point(cc(&h1, &[], "")),
        ));
        beats.push(taken(1, &h1, "keep going"));
        let h2 = with(&h1, &turn(Some("keep going"), &[progress], 15 * MIN, 0));
        beats.push(beat(
            At::Act(1, 15 * MIN),
            "the bucket's try's work ends",
            Ev::Point(cc(&h2, &[], "")),
        ));
        beats.push(taken(2, &h2, "keep going"));
        out.push(sc(
            format!(
                "R7b five-hour try's own turn meets the Opus bucket, no reset{}",
                if take { ", take window" } else { "" }
            ),
            default_policy(),
            beats,
            8 * HOUR,
            take,
        ));
    }
    // ---- R8: tries in a row before the handoff (the five-hour window).
    for take in [false, true] {
        let mut beats = b_head();
        let mut h_at = h_c.clone();
        for k in 0..2 {
            h_at = with(&h_at, &turn(Some("keep going"), &[&again], 0, 0));
            beats.push(beat(
                At::Act(k, 3),
                "the try met the five-hour window again",
                Ev::Point(cc(&h_at, &[], "")),
            ));
        }
        beats.push(taken(2, &h_at, "keep going"));
        let h3 = with(&h_at, &turn(Some("keep going"), &[progress], 12 * MIN, 0));
        beats.push(beat(
            At::Act(2, 12 * MIN),
            "the third try's work ends",
            Ev::Point(cc(&h3, &[], "")),
        ));
        beats.push(taken(3, &h3, "keep going"));
        out.push(sc(
            format!(
                "R8 five-hour Esc'd, met again twice{}",
                if take { ", take window" } else { "" }
            ),
            default_policy(),
            beats,
            3 * HOUR,
            take,
        ));
    }
    // ---- R9: a person's typing grace.
    {
        // (a) The Esc and a keystroke a minute before the try.
        let mut beats = b_head();
        beats.insert(3, beat(At::Start(22 * MIN), "the Esc's keystroke", Ev::Key));
        beats.push(beat(
            At::Start(reset + 100 - T0),
            "a person's keystroke",
            Ev::Key,
        ));
        beats.push(taken(0, &h_c, "keep going"));
        let h3 = with(&h_c, &turn(Some("keep going"), &[progress], 15 * MIN, 0));
        beats.push(beat(
            At::Act(0, 15 * MIN),
            "the try's work ends",
            Ev::Point(cc(&h3, &[], "")),
        ));
        beats.push(taken(1, &h3, "keep going"));
        out.push(sc(
            "R9 five-hour Esc'd, a person's keystroke before the try".to_string(),
            default_policy(),
            beats,
            3 * HOUR,
            false,
        ));
    }
    for take in [false, true] {
        // (b) The spend limit: a keystroke 20 s before the first rung, and
        // one inside the try's take window.
        let mut beats = vec![
            start(),
            beat(
                At::Start(3 * MIN),
                "the spend limit",
                Ev::Point(spend_wall.clone()),
            ),
            beat(At::Start(33 * MIN - 20), "a person's keystroke", Ev::Key),
        ];
        beats.push(beat(At::Act(0, 3), "a person's keystroke", Ev::Key));
        if take {
            beats.push(beat(
                At::Act(0, 5),
                "the try's screen read again",
                Ev::Point(spend_wall.clone()),
            ));
        }
        let h1 = with(&sp0, &spend_turn(Some("keep going"), 0, 0));
        beats.push(beat(
            At::Act(0, 8),
            "the try met again",
            Ev::Point(cc(&h1, &[], "")),
        ));
        beats.push(taken(1, &h1, "keep going"));
        out.push(sc(
            format!(
                "R9 spend, a person's keystrokes at the first rung and the try{}",
                if take { ", take window" } else { "" }
            ),
            default_policy(),
            beats,
            8 * HOUR,
            take,
        ));
    }
    // ---- R11: a person's `/compact` between.
    for (tag, busy) in [("read busy", true), ("not read busy", false)] {
        let mut beats = b_head();
        if busy {
            beats.push(beat(
                At::Start(30 * MIN),
                "a person's /compact runs",
                Ev::Busy(compacting(&h_c)),
            ));
        }
        let after = compacted("⏺ Migrated the first table.");
        beats.push(beat(
            At::Start(31 * MIN),
            "the /compact's answer",
            Ev::Point(after.clone()),
        ));
        beats.push(taken(0, &after[5..after.len() - 4], "keep going"));
        let h3 = with(
            &after[5..after.len() - 4],
            &turn(Some("keep going"), &[progress], 15 * MIN, 0),
        );
        beats.push(beat(
            At::Act(0, 15 * MIN),
            "the try's work ends",
            Ev::Point(cc(&h3, &[], "")),
        ));
        beats.push(taken(1, &h3, "keep going"));
        out.push(sc(
            format!("R11 five-hour Esc'd, a person's /compact {tag}"),
            default_policy(),
            beats,
            3 * HOUR,
            false,
        ));
    }
    {
        // The spend limit met again, a /compact, the ladder.
        let h1 = with(&sp0, &spend_turn(Some("keep going"), 0, 0));
        let after = compacted("⏺ Running the suite.");
        let kept = &after[5..after.len() - 4];
        let h2 = with(kept, &spend_turn(Some("keep going"), 0, 0));
        let beats = vec![
            start(),
            beat(
                At::Start(3 * MIN),
                "the spend limit",
                Ev::Point(spend_wall.clone()),
            ),
            beat(
                At::Act(0, 30),
                "the try met again",
                Ev::Point(cc(&h1, &[], "")),
            ),
            beat(
                At::Act(0, 10 * MIN),
                "a person's /compact runs",
                Ev::Busy(compacting(&h1)),
            ),
            beat(
                At::Act(0, 11 * MIN),
                "the /compact's answer",
                Ev::Point(after.clone()),
            ),
            beat(
                At::Act(1, 30),
                "the second try met again",
                Ev::Point(cc(&h2, &[], "")),
            ),
            taken(2, &h2, "keep going"),
        ];
        out.push(sc(
            "R11 spend met again, a person's /compact, the ladder".to_string(),
            default_policy(),
            beats,
            12 * HOUR,
            false,
        ));
    }
    // ---- R12: the vendor's `continuing automatically` words.
    {
        // (a) The dialog's answer `Claude Code will continue automatically
        // at …`, the countdown armed; the vendor goes on.
        let clock = reset_clock(reset);
        let dialog = with(
            &h_wall,
            &[
                "❯ /rate-limit-options".to_string(),
                format!(
                    "  ⎿  Claude Code will continue automatically at {clock}. Keep this session open; it may still pause for permission prompts. Press esc"
                ),
                "     to cancel the wait.".to_string(),
                String::new(),
            ],
        );
        let mut h2 = dialog.clone();
        h2.push("⏺ Usage limit reset · continuing automatically".to_string());
        h2.push(String::new());
        h2.extend(turn(None, &[progress], 0, reset + 65));
        let beats = vec![
            start(),
            beat(
                At::Start(12 * MIN),
                "the five-hour wall, armed",
                Ev::Point(walled.clone()),
            ),
            beat(
                At::Start(14 * MIN),
                "the dialog's 'Wait here': continue automatically",
                Ev::Point(cc(&dialog, &f_armed, "")),
            ),
            beat(
                At::Start(reset + 65 - T0),
                "the vendor goes on",
                Ev::Point(cc(&h2, &[], "")),
            ),
            taken(0, &h2, "keep going"),
        ];
        out.push(sc(
            "R12 the dialog's 'will continue automatically', the vendor goes on".to_string(),
            default_policy(),
            beats,
            3 * HOUR,
            false,
        ));
    }
    for (tag, seen_armed) in [("after the armed wall", true), ("first seen so", false)] {
        // (b) The countdown's record `⏺ Usage limit reached · continuing
        // automatically at …` standing with no countdown (a `/resume` back
        // into the conversation dropped it): nobody goes on.
        let mut beats = vec![start()];
        if seen_armed {
            beats.push(beat(
                At::Start(12 * MIN),
                "the five-hour wall, armed",
                Ev::Point(walled.clone()),
            ));
        }
        beats.push(beat(
            At::Start(20 * MIN),
            "a /resume back: the countdown's record, no countdown",
            Ev::Point(cc(&h_wall, &[], "")),
        ));
        beats.push(taken(0, &h_wall, "keep going"));
        let h3 = with(&h_wall, &turn(Some("keep going"), &[progress], 15 * MIN, 0));
        beats.push(beat(
            At::Act(0, 15 * MIN),
            "the try's work ends",
            Ev::Point(cc(&h3, &[], "")),
        ));
        beats.push(taken(1, &h3, "keep going"));
        out.push(sc(
            format!("R12 the countdown's record standing, no countdown ({tag})"),
            default_policy(),
            beats,
            3 * HOUR,
            false,
        ));
    }
    // ---- R13: short turns in a row (the streak's back-off is read from
    // the screen again, not seeded), and Off lifting in its back-off before
    // a person's own short exchange (the carry the park drops, which the
    // acts a successor seeds give back).
    for tag in [
        "short turns in a row",
        "Off lifts in the back-off, a person's exchange",
    ] {
        let p0 = cc(&with(&w0, &turn(None, &[progress], 0, T0)), &[], "");
        let mut beats = vec![beat(At::Start(0), "the worker's short turn", Ev::Point(p0))];
        let mut h = with(&w0, &turn(None, &[progress], 0, T0));
        if tag == "short turns in a row" {
            for k in 0..3 {
                h = with(
                    &h,
                    &turn(Some("keep going"), &["⏺ Stage 3 of 5: still on it."], 20, 0),
                );
                beats.push(beat(
                    At::Act(k, 20),
                    "the act's short turn ends",
                    Ev::Point(cc(&h, &[], "")),
                ));
            }
            beats.push(taken(3, &h, "keep going"));
        } else {
            h = with(
                &h,
                &turn(Some("keep going"), &["⏺ Stage 3 of 5: still on it."], 20, 0),
            );
            beats.push(beat(
                At::Act(0, 20),
                "the act's short turn ends",
                Ev::Point(cc(&h, &[], "")),
            ));
            beats.push(beat(At::Act(0, 60), "Off stood and lifted", Ev::OffLifted));
            beats.push(beat(At::Act(0, 70), "a person's keystroke", Ev::Key));
            beats.push(beat(
                At::Act(0, 75),
                "a person's message runs",
                Ev::Busy(live(&h, "what is left to do?")),
            ));
            let h2 = with(
                &h,
                &turn(
                    Some("what is left to do?"),
                    &["⏺ Two stages are left: the parser and the docs."],
                    3 * MIN,
                    0,
                ),
            );
            beats.push(beat(
                At::Act(0, 75 + 3 * MIN),
                "the person's exchange ends",
                Ev::Point(cc(&h2, &[], "")),
            ));
            beats.push(taken(1, &h2, "(next)"));
        }
        out.push(sc(
            format!("R13 {tag}"),
            default_policy(),
            beats,
            3 * HOUR,
            false,
        ));
    }
    out
}

/// THE SHAPES GROWN IN ROUND 8 (the verifier of 97daeeace, RS8): Off
/// standing between a try and its late first read (a record of the loop's
/// is no read); the short streak moved at points no act follows — a
/// person's short turn, a wake's, a done check's answer judged not done —
/// and held past the window, to the ladder's top; the task ended, then a
/// later turn's done report; and an act's yield seen run by a successor
/// attached in its turn (a land act's done reply, a spend limit's try's
/// real work).
#[allow(clippy::too_many_lines)]
fn round8_scenarios() -> Vec<Scenario> {
    let mut out = Vec::new();
    let w0 = work0();
    let done = "⏺ All done: the change is committed and pushed, and every suite passes.";
    let progress = "⏺ Stage 3 of 5 is in; stage 4 is next.";
    let still = "⏺ Stage 3 of 5: still on it.";
    let check = "If the task is finished and verified, end your reply with ATERM-TASK-DONE on a line by itself and stop; otherwise continue.";
    let start = || {
        beat(
            At::Start(0),
            "the worker's turn",
            Ev::Busy(live(&w0, "carry on with the task")),
        )
    };
    let sc = |name: String, policy: SupervisorConfig, beats: Vec<Beat>, tail_s: i64, take: bool| {
        Scenario {
            name,
            t0: T0,
            policy,
            host: None,
            beats,
            tail_s,
            take,
            person_on_screen: false,
        }
    };
    // The worker's short turn, then `n` short yields to `keep going`.
    let short_yields = |n: usize, beats: &mut Vec<Beat>| {
        let p0 = cc(&with(&w0, &turn(None, &[progress], 0, T0)), &[], "");
        beats.push(beat(At::Start(0), "the worker's short turn", Ev::Point(p0)));
        let mut h = with(&w0, &turn(None, &[progress], 0, T0));
        for k in 0..n {
            h = with(&h, &turn(Some("keep going"), &[still], 20, 0));
            beats.push(beat(
                At::Act(k, 20),
                "the act's short turn ends",
                Ev::Point(cc(&h, &[], "")),
            ));
        }
        h
    };
    // ---- RS8-R1: Off stood between the spend limit's try and its first
    // read 20 s on (the record of it no read of the try's).
    let sp0 = with(&w0, &spend_turn(None, 3 * MIN, T0 + 3 * MIN));
    let spend_wall = cc(&sp0, &[], "");
    for off_at in [2, 10] {
        let end = with(&sp0, &spend_turn(Some("keep going"), 3 * HOUR, 0));
        let beats = vec![
            start(),
            beat(
                At::Start(3 * MIN),
                "the spend limit",
                Ev::Point(spend_wall.clone()),
            ),
            beat(At::Act(0, off_at), "Off stood and lifted", Ev::OffLifted),
            beat(
                At::Act(0, 20),
                "the try's screen read",
                Ev::Point(spend_wall.clone()),
            ),
            beat(At::Act(0, 34), "(a handoff instant)", Ev::Mark),
            beat(
                At::Act(0, 40),
                "the try's turn runs",
                Ev::Busy(live(&sp0, "keep going")),
            ),
            beat(
                At::Act(0, 40 + 3 * HOUR),
                "the try's 3h turn ends on the limit",
                Ev::Point(cc(&end, &[], "")),
            ),
            taken(1, &end, "keep going"),
        ];
        out.push(sc(
            format!("RS8-R1 spend, take window, Off stood {off_at} s after the try"),
            default_policy(),
            beats,
            8 * HOUR,
            true,
        ));
    }
    // ---- RS8-R3: short turns in a row, then a person's short turn after
    // the last yield was judged (no act between).
    for acts in [2, 3] {
        let mut beats = Vec::new();
        let h = short_yields(acts, &mut beats);
        let k = acts - 1;
        beats.push(beat(At::Act(k, 30), "a person's keystroke", Ev::Key));
        beats.push(beat(
            At::Act(k, 35),
            "a person's message runs",
            Ev::Busy(live(&h, "what is left to do?")),
        ));
        let h2 = with(
            &h,
            &turn(
                Some("what is left to do?"),
                &["⏺ Two stages are left: the parser and the docs."],
                20,
                0,
            ),
        );
        beats.push(beat(
            At::Act(k, 55),
            "the person's short turn ends",
            Ev::Point(cc(&h2, &[], "")),
        ));
        beats.push(taken(acts, &h2, "keep going"));
        out.push(sc(
            format!("RS8-R3 {acts} short turns, then a person's short turn"),
            default_policy(),
            beats,
            3 * HOUR,
            false,
        ));
    }
    // ---- RS8-R4: the done check answered NOT done (a short reply), then a
    // person's turn ending in a done report.
    {
        let p0 = with(&w0, &turn(None, &[done], 10 * MIN, T0 + 10 * MIN));
        let mut beats = vec![
            start(),
            beat(
                At::Start(10 * MIN),
                "the worker's done report",
                Ev::Point(cc(&p0, &[], "")),
            ),
            taken(0, &p0, "If the task is finished"),
        ];
        let h4 = with(
            &p0,
            &turn(
                Some(check),
                &["⏺ Not yet: one suite still fails; I am on it."],
                20,
                0,
            ),
        );
        beats.push(beat(
            At::Act(0, 22),
            "the done check's answer: not done",
            Ev::Point(cc(&h4, &[], "")),
        ));
        beats.push(beat(At::Act(0, 40), "a person's keystroke", Ev::Key));
        beats.push(beat(
            At::Act(0, 45),
            "a person's turn runs",
            Ev::Busy(live(&h4, "please also update the changelog")),
        ));
        let h5 = with(
            &h4,
            &turn(
                Some("please also update the changelog"),
                &["⏺ All done: the changelog is updated, committed and pushed."],
                3 * MIN,
                0,
            ),
        );
        beats.push(beat(
            At::Act(0, 45 + 3 * MIN),
            "the person's turn ends in a done report",
            Ev::Point(cc(&h5, &[], "")),
        ));
        beats.push(taken(1, &h5, "(the check)"));
        out.push(sc(
            "RS8-R4 the done check answered not done, then a person's done report".to_string(),
            default_policy(),
            beats,
            3 * HOUR,
            false,
        ));
    }
    // ---- RS8-F2: Land, landed by the land act's 10 min done reply, then a
    // new task's short turn (the land act's yield seen run).
    for (tag, reply) in [("a done report", done), ("progress", progress)] {
        let p0 = cc(
            &with(&w0, &turn(None, &[progress], 0, T0 + 2 * MIN)),
            &[],
            "",
        );
        let mut beats = vec![
            start(),
            beat(At::Start(2 * MIN), "the worker's progress", Ev::Point(p0)),
            taken(0, &w0, "(land)"),
        ];
        let h1 = with(&w0, &turn(Some("(land)"), &[done], 10 * MIN, 0));
        beats.push(beat(
            At::Act(0, 10 * MIN),
            "the land turn ends: done (landed)",
            Ev::Point(cc(&h1, &[], "")),
        ));
        beats.push(beat(
            At::Act(0, 20 * MIN),
            "a new task's turn runs",
            Ev::Busy(live(&h1, "now write the docs")),
        ));
        let h2 = with(&h1, &turn(Some("now write the docs"), &[reply], 30, 0));
        beats.push(beat(
            At::Act(0, 20 * MIN + 30),
            "the new task's short turn ends",
            Ev::Point(cc(&h2, &[], "")),
        ));
        beats.push(taken(1, &h2, "(next)"));
        let h3 = with(&h2, &turn(Some("(next)"), &[progress], 10 * MIN, 0));
        beats.push(beat(
            At::Act(1, 10 * MIN),
            "the next act's turn ends: progress",
            Ev::Point(cc(&h3, &[], "")),
        ));
        out.push(sc(
            format!("RS8-F2 Land, landed, then a new task's short turn ({tag})"),
            land_policy(),
            beats,
            3 * HOUR,
            false,
        ));
    }
    // ---- RS8-F2: a wake's short turn between two acts.
    {
        let mut beats = Vec::new();
        let mut h = short_yields(1, &mut beats);
        beats.push(beat(
            At::Act(0, 80),
            "a turn nobody typed runs (the worker's own wake)",
            Ev::Busy(live(&h, "(background task finished)")),
        ));
        h = with(
            &h,
            &turn(
                Some("(background task finished)"),
                &["⏺ The build is still going."],
                20,
                0,
            ),
        );
        beats.push(beat(
            At::Act(0, 100),
            "the wake's short turn ends",
            Ev::Point(cc(&h, &[], "")),
        ));
        for k in 1..3 {
            h = with(&h, &turn(Some("keep going"), &[still], 20, 0));
            beats.push(beat(
                At::Act(k, 20),
                "the act's short turn ends",
                Ev::Point(cc(&h, &[], "")),
            ));
        }
        beats.push(taken(3, &h, "keep going"));
        out.push(sc(
            "RS8-F2 short turns, a wake's short turn between two acts".to_string(),
            default_policy(),
            beats,
            4 * HOUR,
            false,
        ));
    }
    // ---- RS8-F2/F3: short turns in a row to the ladder's top (60 min).
    {
        let mut beats = Vec::new();
        let h = short_yields(7, &mut beats);
        beats.push(taken(7, &h, "keep going"));
        out.push(sc(
            "RS8-F3 short turns in a row to the ladder's top".to_string(),
            default_policy(),
            beats,
            6 * HOUR,
            false,
        ));
    }
    // ---- RS8-F2: short turns, the spend limit at a short yield, the try's
    // real work (no continuation: its yield keeps the streak).
    {
        let mut beats = Vec::new();
        let mut h = short_yields(1, &mut beats);
        h = with(&h, &spend_turn(Some("keep going"), 20, 0));
        beats.push(beat(
            At::Act(1, 20),
            "the act's short turn ends on the spend limit",
            Ev::Point(cc(&h, &[], "")),
        ));
        beats.push(taken(2, &h, "keep going"));
        h = with(&h, &turn(Some("keep going"), &[progress], 10 * MIN, 0));
        beats.push(beat(
            At::Act(2, 10 * MIN),
            "the try's 10 min of work ends: progress",
            Ev::Point(cc(&h, &[], "")),
        ));
        beats.push(taken(3, &h, "keep going"));
        h = with(&h, &turn(Some("keep going"), &[progress], 10 * MIN, 0));
        beats.push(beat(
            At::Act(3, 10 * MIN),
            "the next act's work ends: progress",
            Ev::Point(cc(&h, &[], "")),
        ));
        out.push(sc(
            "RS8-F2 short turns, the spend limit, the try's real work".to_string(),
            default_policy(),
            beats,
            3 * HOUR,
            false,
        ));
    }
    out
}

/// Round 9's shapes (the verifier of 75b03ac6d): the land act answered not
/// done (RS9-F4). (Its RS9-F2 and RS9-F3 began at a task the worker's own
/// token ended, which FOREVER CRUISE retired.)
fn round9_scenarios() -> Vec<Scenario> {
    let mut out = Vec::new();
    let w0 = work0();
    let progress = "⏺ Stage 3 of 5 is in; stage 4 is next.";
    let start = || {
        beat(
            At::Start(0),
            "the worker's turn",
            Ev::Busy(live(&w0, "carry on with the task")),
        )
    };
    let sc =
        |name: &str, policy: SupervisorConfig, beats: Vec<Beat>, person_on_screen: bool| Scenario {
            name: name.to_string(),
            t0: T0,
            policy,
            host: None,
            beats,
            tail_s: 3 * HOUR,
            take: false,
            person_on_screen,
        };
    // ---- RS9-F4: RS8-R4 under Land: the land instruction answered NOT
    // done (a short reply), then a turn ending in a done report — a
    // person's, and nobody's: no landing in hand, so Land nudges.
    for (tag, with_key) in [("a person's hand", true), ("nobody's hand", false)] {
        let p0 = cc(
            &with(&w0, &turn(None, &[progress], 0, T0 + 2 * MIN)),
            &[],
            "",
        );
        let mut beats = vec![
            start(),
            beat(At::Start(2 * MIN), "the worker's progress", Ev::Point(p0)),
            taken(0, &w0, "(land)"),
        ];
        let h1 = with(
            &w0,
            &turn(
                Some("(land)"),
                &["⏺ Not yet: one suite still fails; I am on it."],
                20,
                0,
            ),
        );
        beats.push(beat(
            At::Act(0, 22),
            "the land act's answer: not done",
            Ev::Point(cc(&h1, &[], "")),
        ));
        if with_key {
            beats.push(beat(At::Act(0, 40), "a person's keystroke", Ev::Key));
        }
        beats.push(beat(
            At::Act(0, 41),
            "a turn runs",
            Ev::Busy(live(&h1, "please also update the changelog")),
        ));
        let h2 = with(
            &h1,
            &turn(
                Some("please also update the changelog"),
                &["⏺ All done: the changelog is updated, committed and pushed."],
                3 * MIN,
                0,
            ),
        );
        beats.push(beat(
            At::Act(0, 41 + 3 * MIN),
            "the turn ends in a done report",
            Ev::Point(cc(&h2, &[], "")),
        ));
        beats.push(taken(1, &h2, "(next)"));
        let h3 = with(&h2, &turn(Some("(next)"), &[progress], 10 * MIN, 0));
        beats.push(beat(
            At::Act(1, 10 * MIN),
            "the next act's turn ends: progress",
            Ev::Point(cc(&h3, &[], "")),
        ));
        out.push(sc(
            &format!("RS9-F4 Land, the land act answered not done, then a done report ({tag})"),
            land_policy(),
            beats,
            false,
        ));
    }
    out
}

/// AN API ERROR'S RETRY LADDER (a 529, overloaded: [`TurnEndTiming::retry_backoff`],
/// 1, 5, 15, 30, then 60 minutes from each appearance; ATERM-KI-1016-1's "a
/// loop that follows another"). Taken up from nothing, a loop that follows
/// retried on the ladder's first rung from its own read, whatever rung the
/// loop before was on: up to an hour sooner. The ladder met again at every
/// rung; a retry's own long work meeting it again (a new episode, the
/// ladder from the start); a retry's short turn meeting it again (the same
/// episode); and the ladder in a take window.
fn api_retry_scenarios() -> Vec<Scenario> {
    let mut out = Vec::new();
    let w0 = work0();
    let overloaded = "  ⎿  API Error: 529 Overloaded. This is a server-side issue, usually \
                      temporary — try again in a moment. If it persists, check \
                      https://status.claude.com.";
    // A 500's ladder is the 529's; an older build's journal names both
    // `api-error`'s and the network's alike, and only the act's words tell
    // them apart (`retry_held`).
    let server = "  ⎿  API Error: 500 Internal server error. This is a server-side issue, \
                  usually temporary — try again in a moment.";
    let api_turn_of = |error: &'static str| {
        move |prompt: Option<&str>, worked_s: i64, at: i64| {
            turn(prompt, &["⏺ Running the suite.", error], worked_s, at)
        }
    };
    let api_turn = api_turn_of(overloaded);
    let report = "⏺ Stage 3 of 5 is in; stage 4 is next.";
    let start = || {
        beat(
            At::Start(0),
            "the worker's turn",
            Ev::Busy(live(&w0, "carry on with the task")),
        )
    };
    let h0 = with(&w0, &api_turn(None, 3 * MIN, T0 + 3 * MIN));
    for take in [false, true] {
        let window = if take { ", take window" } else { "" };
        // The ladder met again at every rung, then a retry taken.
        for (code, error) in [("529", overloaded), ("500", server)] {
            let api_turn = api_turn_of(error);
            let h0 = with(&w0, &api_turn(None, 3 * MIN, T0 + 3 * MIN));
            let mut beats = vec![
                start(),
                beat(
                    At::Start(3 * MIN),
                    "the API error",
                    Ev::Point(cc(&h0, &[], "")),
                ),
            ];
            let mut h = h0.clone();
            for k in 0..6 {
                h = with(&h, &api_turn(Some("try again"), 0, 0));
                beats.push(beat(
                    At::Act(k, 3),
                    "the retry met the API error again",
                    Ev::Point(cc(&h, &[], "")),
                ));
            }
            beats.push(taken(6, &h, "try again"));
            let end = with(&h, &turn(Some("try again"), &[report], 10 * MIN, 0));
            beats.push(beat(
                At::Act(6, 10 * MIN),
                "the retry's work ends",
                Ev::Point(cc(&end, &[], "")),
            ));
            beats.push(taken(7, &end, "keep going"));
            out.push(Scenario {
                name: format!("API {code} met again at every rung, then taken{window}"),
                t0: T0,
                policy: default_policy(),
                host: None,
                beats,
                tail_s: 2 * HOUR,
                take,
                person_on_screen: false,
            });
        }
        // Met again twice, then a retry's own work meets it: 20 minutes (a
        // new episode, its ladder from the start) or 3 (the same episode).
        for (work, tag) in [(20 * MIN, "long"), (3 * MIN, "short")] {
            let mut beats = vec![
                start(),
                beat(At::Start(3 * MIN), "the 529", Ev::Point(cc(&h0, &[], ""))),
            ];
            let mut h = h0.clone();
            for k in 0..2 {
                h = with(&h, &api_turn(Some("try again"), 0, 0));
                beats.push(beat(
                    At::Act(k, 3),
                    "the retry met the 529 again",
                    Ev::Point(cc(&h, &[], "")),
                ));
            }
            beats.push(taken(2, &h, "try again"));
            let met = with(&h, &api_turn(Some("try again"), work, 0));
            beats.push(beat(
                At::Act(2, work + 2),
                "the retry's own work meets the 529",
                Ev::Point(cc(&met, &[], "")),
            ));
            beats.push(taken(3, &met, "try again"));
            let end = with(&met, &turn(Some("try again"), &[report], 10 * MIN, 0));
            beats.push(beat(
                At::Act(3, 10 * MIN),
                "the retry's work ends",
                Ev::Point(cc(&end, &[], "")),
            ));
            beats.push(taken(4, &end, "keep going"));
            out.push(Scenario {
                name: format!("API 529 met again twice, a retry's {tag} work meets it{window}"),
                t0: T0,
                policy: default_policy(),
                host: None,
                beats,
                tail_s: 2 * HOUR,
                take,
                person_on_screen: false,
            });
        }
        // A 529 cuts Land's act and the wall's retry is answered with a clean defense: the
        // retry delivers the act again and lands the task, unless a person's turn came between.
        let land = crate::supervise::policy::turn_end::LAND;
        let retry = crate::supervise::policy::turn_end::wall_retry_text(
            crate::supervise::policy::turn_end::RULE_API_RETRY,
            aterm_phase::WallKind::Overloaded,
            aterm_phase::Program::Claude,
        );
        let defense = [
            "⏺ Merged the parser into main and pushed it; every suite passes.",
            "",
            "  Nothing left: all of it is merged into the remote main branch.",
        ];
        let p0 = cc(&with(&w0, &turn(None, &[report], 0, T0 + 2 * MIN)), &[], "");
        let cut = with(&w0, &api_turn(Some(land), 5 * MIN, 0));
        for person in [false, true] {
            let mut beats = vec![
                start(),
                beat(
                    At::Start(2 * MIN),
                    "the worker's progress",
                    Ev::Point(p0.clone()),
                ),
                taken(0, &w0, land),
                beat(
                    At::Act(0, 5 * MIN),
                    "the act's turn meets a 529",
                    Ev::Point(cc(&cut, &[], "")),
                ),
            ];
            let mut h = cut.clone();
            if person {
                let said = "please also look at the docs";
                h = with(&cut, &api_turn(Some(said), 4 * MIN, 0));
                beats.extend([
                    beat(At::Act(0, 5 * MIN + 20), "a person's keystroke", Ev::Key),
                    beat(
                        At::Act(0, 5 * MIN + 25),
                        "a person's turn runs",
                        Ev::Busy(live(&cut, said)),
                    ),
                    beat(
                        At::Act(0, 9 * MIN + 30),
                        "the person's turn meets a 529",
                        Ev::Point(cc(&h, &[], "")),
                    ),
                ]);
            }
            let answered = with(&h, &turn(Some(&retry), &defense, 3 * MIN, 0));
            beats.extend([
                taken(1, &h, &retry),
                beat(
                    At::Act(1, 3 * MIN),
                    "the retry's turn ends: a clean defense",
                    Ev::Point(cc(&answered, &[], "")),
                ),
            ]);
            // The first loop lands the task, or, a person's turn between, nudges the landing.
            beats.push(if person {
                taken(2, &answered, crate::supervise::policy::turn_end::LAND_NUDGE)
            } else {
                beat(At::Act(1, 3 * MIN + 90), "(a handoff instant)", Ev::Mark)
            });
            let between = if person {
                ", a person's turn meets one too"
            } else {
                ""
            };
            out.push(Scenario {
                name: format!("API 529 cuts Land's act{between}, its retry defended{window}"),
                t0: T0,
                policy: land_policy(),
                host: None,
                beats,
                tail_s: 3 * HOUR,
                take,
                person_on_screen: false,
            });
        }
        // Cruise's done check meets a full context in its own turn, or a 529 cuts it and the
        // wall's retry meets one: the `/compact`'s continuation types the check again. No row
        // names that wall, so a successor reads it off the check's point and the `/compact`
        // after it. A person's turn between (met by a 529 too) leaves the plain continuation.
        // The report names an irreversible act, so the check carries the reversible clause,
        // as a successor's continuation types it for a check it did not type
        // (`compact_owed_text`).
        let check = crate::supervise::policy::turn_end::DONE_CHECK;
        let claim = "⏺ All done: committed and pushed. Should I delete the old release branch?";
        let h1 = with(&w0, &turn(None, &[claim], 0, T0 + 2 * MIN));
        let after = compacted("⏺ Running the suite.");
        let kept = &after[5..after.len() - 4];
        let full_context = [
            "⏺ Running the suite.",
            "  ⎿  Context limit reached · /compact or /clear to continue",
        ];
        for (api, person) in [(false, false), (true, false), (true, true)] {
            let mut beats = vec![
                start(),
                beat(
                    At::Start(2 * MIN),
                    "the worker's done report",
                    Ev::Point(cc(&h1, &[], "")),
                ),
                taken(0, &h1, check),
            ];
            // The act whose turn meets the full context: the check, or the wall's retry.
            let (k, act, mut h) = if api {
                (
                    1,
                    retry.as_str(),
                    with(&h1, &api_turn(Some(check), 5 * MIN, 0)),
                )
            } else {
                (0, check, h1.clone())
            };
            if api {
                beats.push(beat(
                    At::Act(0, 5 * MIN),
                    "the check's turn meets a 529",
                    Ev::Point(cc(&h, &[], "")),
                ));
            }
            if person {
                let said = "please also look at the docs";
                let cut = h.clone();
                h = with(&cut, &api_turn(Some(said), 4 * MIN, 0));
                beats.extend([
                    beat(At::Act(0, 5 * MIN + 20), "a person's keystroke", Ev::Key),
                    beat(
                        At::Act(0, 5 * MIN + 25),
                        "a person's turn runs",
                        Ev::Busy(live(&cut, said)),
                    ),
                    beat(
                        At::Act(0, 9 * MIN + 30),
                        "the person's turn meets a 529",
                        Ev::Point(cc(&h, &[], "")),
                    ),
                ]);
            }
            if api {
                beats.push(taken(1, &h, act));
            }
            let full = with(&h, &turn(Some(act), &full_context, 4 * MIN, 0));
            let again = if person { "keep going" } else { check };
            let answered = with(
                kept,
                &turn(
                    Some(again),
                    &["⏺ All done: verified again, every suite passes."],
                    3 * MIN,
                    0,
                ),
            );
            beats.extend([
                beat(
                    At::Act(k, 4 * MIN),
                    "the act's turn meets a full context",
                    Ev::Point(cc(&full, &[], "")),
                ),
                beat(
                    At::Act(k + 1, 2),
                    "the /compact runs",
                    Ev::Busy(compacting(&full)),
                ),
                beat(
                    At::Act(k + 1, 40),
                    "the /compact's answer",
                    Ev::Point(after.clone()),
                ),
                taken(k + 2, kept, again),
                beat(
                    At::Act(k + 2, 3 * MIN),
                    "the continuation's turn ends: done again",
                    Ev::Point(cc(&answered, &[], "")),
                ),
                taken(k + 3, &answered, check),
            ]);
            let name = match (api, person) {
                (false, _) => "the done check's turn meets a full context",
                (true, false) => "API 529 cuts the done check, its retry meets a full context",
                (true, true) => {
                    "API 529 cuts the done check, a person's turn meets one too, its retry \
                     meets a full context"
                }
            };
            out.push(Scenario {
                name: format!("{name}{window}"),
                t0: T0,
                policy: default_policy(),
                host: None,
                beats,
                tail_s: 3 * HOUR,
                take,
                person_on_screen: false,
            });
        }
        // Land's instruction meets a full context in its own turn, or a 529 cuts it and the
        // wall's retry meets one, or the continuation's own turn meets one again: the
        // `/compact`'s continuation types the instruction again, and its clean defense lands
        // the task. No row names that wall: a successor reads it off the act's point and the
        // `/compact` after it, and the act by its row's rule (`compact_owed_text`).
        let answer = crate::supervise::policy::turn_end::LAND_ANSWER;
        for (api, twice, asked) in [
            (false, false, false),
            (true, false, false),
            (false, true, false),
            (false, false, true),
        ] {
            let mut beats = vec![
                start(),
                beat(
                    At::Start(2 * MIN),
                    "the worker's progress",
                    Ev::Point(p0.clone()),
                ),
                taken(0, &w0, land),
            ];
            let (mut k, act, h) = if api {
                let cut = with(&w0, &api_turn(Some(land), 5 * MIN, 0));
                beats.extend([
                    beat(
                        At::Act(0, 5 * MIN),
                        "the act's turn meets a 529",
                        Ev::Point(cc(&cut, &[], "")),
                    ),
                    taken(1, &cut, &retry),
                ]);
                (1, retry.as_str(), cut)
            } else if asked {
                let q = "⏺ Stage 3 is in. Should I merge the parser branch into main now?";
                let h = with(&w0, &turn(Some(land), &[q], 4 * MIN, 0));
                beats.extend([
                    beat(
                        At::Act(0, 4 * MIN),
                        "the instruction's turn ends: a question",
                        Ev::Point(cc(&h, &[], "")),
                    ),
                    taken(1, &h, answer),
                ]);
                (1, answer, h)
            } else {
                (0, land, w0.clone())
            };
            let land = if asked { answer } else { land };
            let full = with(&h, &turn(Some(act), &full_context, 4 * MIN, 0));
            beats.extend([
                beat(
                    At::Act(k, 4 * MIN),
                    "the act's turn meets a full context",
                    Ev::Point(cc(&full, &[], "")),
                ),
                beat(
                    At::Act(k + 1, 2),
                    "the /compact runs",
                    Ev::Busy(compacting(&full)),
                ),
                beat(
                    At::Act(k + 1, 40),
                    "the /compact's answer",
                    Ev::Point(after.clone()),
                ),
                taken(k + 2, kept, land),
            ]);
            let mut kept = kept.to_vec();
            if twice {
                let full = with(&kept, &turn(Some(land), &full_context, 3 * MIN, 0));
                let after = compacted("⏺ Running the suite once more.");
                kept = after[5..after.len() - 4].to_vec();
                beats.extend([
                    beat(
                        At::Act(k + 2, 3 * MIN),
                        "the continuation's turn meets a full context",
                        Ev::Point(cc(&full, &[], "")),
                    ),
                    beat(
                        At::Act(k + 3, 2),
                        "the /compact runs",
                        Ev::Busy(compacting(&full)),
                    ),
                    beat(
                        At::Act(k + 3, 40),
                        "the /compact's answer",
                        Ev::Point(after),
                    ),
                    taken(k + 4, &kept, land),
                ]);
                k += 2;
            }
            let answered = with(&kept, &turn(Some(land), &defense, 3 * MIN, 0));
            beats.extend([
                beat(
                    At::Act(k + 2, 3 * MIN),
                    "the continuation's turn ends: a clean defense",
                    Ev::Point(cc(&answered, &[], "")),
                ),
                beat(
                    At::Act(k + 2, 3 * MIN + 90),
                    "(a handoff instant)",
                    Ev::Mark,
                ),
            ]);
            let name = match (api, twice, asked) {
                (true, ..) => "API 529 cuts Land's instruction, its retry meets a full context",
                (_, true, _) => {
                    "Land's instruction meets a full context, its continuation meets one again"
                }
                (_, _, true) => "Land's answer meets a full context",
                _ => "Land's instruction meets a full context",
            };
            out.push(Scenario {
                name: format!("{name}, defended{window}"),
                t0: T0,
                policy: land_policy(),
                host: None,
                beats,
                tail_s: 3 * HOUR,
                take,
                person_on_screen: false,
            });
        }
    }
    out
}

/// The scenarios of ATERM-KI-1016-1's person lane, after its review
/// (KR-P6-ESC): the first loop's `keep going`, its answer stopped by an Esc
/// that is no act of the harness's — a person's key, and the Esc with no
/// keystroke the window stamps — then, inside the Esc's hold, a person's own
/// short message, answered in a minute. The first loop's act answered under
/// that Esc ends its carry (`TurnEndState::observe`'s act arm), so the
/// person's turn after it is their own exchange, left to them. Every read
/// carries the person's stamp.
fn person_scenarios() -> Vec<Scenario> {
    let mut out = Vec::new();
    let w0 = work0();
    let progress = "⏺ Stage 3 of 5 is in; stage 4 is next.";
    for (tag, key) in [("the Esc a key", true), ("the Esc no key", false)] {
        let p0 = cc(
            &with(&w0, &turn(None, &[progress], 0, T0 + 2 * MIN)),
            &[],
            "",
        );
        let mut beats = vec![
            beat(
                At::Start(0),
                "the worker's turn",
                Ev::Busy(live(&w0, "carry on with the task")),
            ),
            beat(At::Start(2 * MIN), "the worker's progress", Ev::Point(p0)),
            taken(0, &w0, "keep going"),
        ];
        if key {
            beats.push(beat(At::Act(0, 60), "a person's Esc", Ev::Key));
        }
        let h1 = with(
            &w0,
            &s(&[
                "❯ keep going",
                "",
                "⏺ Working on stage 4.",
                "  ⎿  Interrupted · What should Claude do instead?",
                "",
            ]),
        );
        beats.push(beat(
            At::Act(0, 60),
            "the act's turn stopped with Esc",
            Ev::Point(cc(&h1, &[], "")),
        ));
        beats.push(beat(At::Act(0, 89), "a person's keystroke", Ev::Key));
        beats.push(beat(
            At::Act(0, 90),
            "a person's message runs",
            Ev::Busy(live(&h1, "what is left?")),
        ));
        let h2 = with(
            &h1,
            &turn(
                Some("what is left?"),
                &["⏺ The parser and the docs."],
                60,
                0,
            ),
        );
        beats.push(beat(
            At::Act(0, 150),
            "the person's exchange ends",
            Ev::Point(cc(&h2, &[], "")),
        ));
        out.push(Scenario {
            name: format!("KI1016-P6 the act's answer Esc'd, then a person's exchange ({tag})"),
            t0: T0,
            policy: default_policy(),
            host: None,
            beats,
            tail_s: 3 * HOUR,
            take: false,
            person_on_screen: true,
        });
    }
    out
}

/// How many scenarios of a matrix run side by side. The test binary already runs a test per
/// core, so a matrix spread over every core only contends with them: the matrix's 23 tests
/// alone took 7.3 s and 68 CPU-s on 18 workers (35 of them the kernel's), 4.2 s and 38 CPU-s
/// on 4, 6.2 s on 2.
const MATRIX_WORKERS: usize = 4;

/// Each scenario's matrix, in order: run on [`MATRIX_WORKERS`] threads (each
/// cell's loops are its own, on the fixed clock).
fn run_all(scenarios: &[Scenario], seed: bool) -> Vec<(String, Tally)> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let next = AtomicUsize::new(0);
    let mut out: Vec<(usize, (String, Tally))> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..MATRIX_WORKERS.min(scenarios.len()))
            .map(|_| {
                scope.spawn(|| {
                    let mut done = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(sc) = scenarios.get(i) else {
                            return done;
                        };
                        done.push((i, run_matrix(sc, seed)));
                    }
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("a scenario's matrix"))
            .collect()
    });
    out.sort_by_key(|(i, _)| *i);
    out.into_iter().map(|(_, r)| r).collect()
}

#[test]
fn a_loop_that_follows_types_as_the_first_loop_at_every_handoff() {
    let scenarios = scenarios();
    let runs = run_all(&scenarios, true);
    let cells: usize = runs.iter().map(|(_, t)| t.cells).sum();
    let broken: Vec<&String> = runs.iter().flat_map(|(_, t)| &t.broken).collect();
    assert!(cells > 500, "the matrix ran {cells} cells");
    assert!(
        broken.is_empty(),
        "{} of {cells} cells break the rule:\n{}",
        broken.len(),
        broken
            .iter()
            .map(|b| b.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
    // NEGATIVE CONTROL: successors that take nothing up from the ledger, at
    // a spend limit met again and at a five-hour window's try met again,
    // type sooner than the first loop, and the matrix says so.
    let controls: Vec<Scenario> = scenarios
        .into_iter()
        .filter(|sc| {
            sc.name == "D spend (met again, the ladder)"
                || sc.name == "B five-hour Esc'd, the loop's try (met again)"
        })
        .collect();
    assert_eq!(controls.len(), 2, "the control's scenarios");
    for (out, tally) in run_all(&controls, false) {
        assert!(
            tally.broken.iter().any(|b| b.starts_with("SOONER")),
            "CONTROL: a successor with no seed broke nothing:\n{out}"
        );
    }
    // NEGATIVE CONTROL: the books a loop stopped in a point's window leaves
    // without the task's end said there — its note owed to the point's act,
    // as `look` owed it until 2026-10-20 ([`window_cells`]' `owed`): the
    // landing said in words, read by a loop started as Off lifts, gets the
    // land instruction again.
    let landed = self::scenarios()
        .into_iter()
        .find(|sc| sc.name == "R4 Off lifts after the landing (Land, in words)")
        .expect("the landing's scenario");
    let (out, tally) = run_matrix_owed(&landed, true, true);
    assert!(
        tally.broken.iter().any(|b| {
            b.starts_with("TYPES-WHERE-FIRST-DOES-NOT") && b.contains(" stopped in the window at ")
        }),
        "CONTROL: the landing's note owed past its window broke nothing:\n{out}"
    );
}

/// A LAND EPISODE A READING IN ANOTHER MODE ENDED IS OVER FOR A LOOP THAT
/// FOLLOWS (the record of it, [`crate::supervise::approvals::RULE_LAND_OVER`];
/// the episode's seed, RS7-2). The land instruction is typed under Land; the
/// next point, its short yield, is read under Cruise and types nothing (the
/// short streak's back-off): the episode is over, a Land after it is a new
/// one, and the ledger holds no episode for a loop that follows
/// ([`crate::supervise::approvals::land_episode`]). With no row, a loop that
/// follows under Land again would nudge, or type nothing for a landing
/// spent, where the loop that read Cruise types the instruction anew — the
/// matrix's scenarios hold one mode each. NEGATIVE CONTROL: the same point
/// read under Land keeps the episode.
#[test]
fn a_land_episode_a_reading_in_another_mode_ended_is_over_for_a_loop_that_follows() {
    use crate::supervise::config::Autopilot;
    let dir = crate::supervise::test_scratch_path("seed-matrix", "land-over");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let w0 = work0();
    let progress = cc(
        &with(
            &w0,
            &turn(None, &["⏺ Stage 3 of 5 is in."], 0, T0 + 2 * MIN),
        ),
        &[],
        "",
    );
    let yielded = cc(
        &with(
            &w0,
            &turn(
                Some("(land)"),
                &["⏺ Stage 4 of 5 is in."],
                20,
                T0 + 2 * MIN + 20,
            ),
        ),
        &[],
        "",
    );
    fn decide(
        s: &mut Session<'_, Mock>,
        rows: &[String],
        seq: u64,
        worked: u64,
        opts: &SuperviseOpts,
    ) -> Option<&'static str> {
        s.ctl.screens = vec![rows.to_vec()];
        s.ctl.served = 0;
        let p = point_at(s, rows.to_vec(), seq);
        let a = s
            .turn_end_at_point(&p, Some(Duration::from_secs(worked)), opts)
            .expect("a decision");
        let rule = a.rule_id();
        s.turn_end_execute(&p, a, opts, &[], &mut Alone)
            .expect("decided");
        rule
    }
    let under = |autopilot: Autopilot| SuperviseOpts {
        policy: SupervisorConfig {
            autopilot,
            ..land_policy()
        },
        ..auto(30)
    };
    for (mode, over) in [(Autopilot::Cruise, true), (Autopilot::Land, false)] {
        let ledger = dir.join(format!("{}.jsonl", mode.word()));
        let mut m = Mock::new(vec![Vec::new()]);
        let mut s = session(&mut m, Some(SID.to_string()));
        s.set_zone_lookup(test_zones);
        s.ledger = Ledger::open(Some(&ledger), Some(SID), &mut Vec::new());
        s.turn_end_seeded = true;
        s.set_turn_end_timing(TurnEndTiming {
            take_within: Duration::ZERO,
            ..TurnEndTiming::default()
        });
        s.set_clock(T0 + 2 * MIN, PDT);
        let rule = decide(&mut s, &progress, 101, 120, &under(Autopilot::Land));
        assert_eq!(
            rule,
            Some(crate::supervise::policy::turn_end::RULE_LAND),
            "the instruction"
        );
        s.set_clock(T0 + 2 * MIN + 20, PDT);
        let rule = decide(&mut s, &yielded, 102, 20, &under(mode));
        assert_eq!(rule, None, "{mode:?}: the short yield is backed off");
        drop(s);
        let episode = crate::supervise::approvals::land_episode(&ledger, Some(SID));
        assert_eq!(episode.is_none(), over, "{mode:?}: {episode:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE HOLD'S `since` IS THE INSTRUCTION'S ROW, FOR THIS LOOP AND THE NEXT
/// (the Autopilot design's §12.3.3, slice 3a): the loop that types Land's
/// instruction holds it from the stamp its ledger row carries
/// (`ledger_row` → `TurnEndState::ledgered`), and a loop that follows on
/// the same books (a restart, a seamless update) seeds that very stamp
/// (`approvals::LandSeed::hold` → `TurnEndState::seed_land_hold`), its
/// episode's legs unread until its first yield. The nudge its yield is owed,
/// typed and ledgered later, counts one and leaves `since` at the
/// instruction's row, in this loop and the next. NEGATIVE CONTROLS: the
/// hold before the instruction is none, a loop over books with no episode
/// holds none, and the nudge's own row is later than the instruction's.
#[test]
fn a_land_holds_since_its_instruction_row_for_this_loop_and_the_next() {
    use crate::supervise::config::Autopilot;
    use crate::supervise::policy::turn_end::{RULE_LAND, RULE_LAND_NUDGE};
    let dir = crate::supervise::test_scratch_path("seed-matrix", "land-since");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let w0 = work0();
    let progress = cc(
        &with(
            &w0,
            &turn(None, &["⏺ Stage 3 of 5 is in."], 0, T0 + 2 * MIN),
        ),
        &[],
        "",
    );
    let opts = SuperviseOpts {
        policy: SupervisorConfig {
            autopilot: Autopilot::Land,
            ..land_policy()
        },
        ..auto(30)
    };
    let ledger = dir.join("land.jsonl");
    let mut m = Mock::new(vec![Vec::new()]);
    let mut s = session(&mut m, Some(SID.to_string()));
    s.set_zone_lookup(test_zones);
    s.ledger = Ledger::open(Some(&ledger), Some(SID), &mut Vec::new());
    s.turn_end_seeded = true;
    s.set_turn_end_timing(TurnEndTiming {
        take_within: Duration::ZERO,
        ..TurnEndTiming::default()
    });
    s.set_clock(T0 + 2 * MIN, PDT);
    // Virtual clocks, moved by sleeps alone: each row's stamp is its own.
    {
        use crate::supervise::fx::{ClockSource, Fx, VirtualClock};
        s.set_fx(Fx::over(ClockSource::Virtual(std::sync::Arc::new(
            VirtualClock::new(std::time::Instant::now(), (T0 + 2 * MIN) * 1000),
        ))));
    }
    assert_eq!(s.turn_end.land_held(), None, "CONTROL: no instruction yet");
    s.ctl.screens = vec![progress.clone()];
    s.ctl.served = 0;
    let p = point_at(&s, progress.clone(), 101);
    let a = s
        .turn_end_at_point(&p, Some(Duration::from_secs(120)), &opts)
        .expect("a decision");
    assert_eq!(a.rule_id(), Some(RULE_LAND), "{a:?}");
    s.turn_end_execute(&p, a, &opts, &[], &mut Alone)
        .expect("decided");
    let row_of = |rule: &str| {
        std::fs::read_to_string(&ledger)
            .expect("the ledger")
            .lines()
            .filter_map(|l| aterm_json::from_str::<aterm_json::Value>(l).ok())
            .filter(|v| v.get("rule_id").and_then(aterm_json::Value::as_str) == Some(rule))
            .filter_map(|v| v.get("ts").and_then(aterm_json::Value::as_i64))
            .next_back()
    };
    let row_ts = row_of(RULE_LAND).expect("the instruction's row");
    // This loop: the row's own stamp, its yield not read yet.
    let live = s.turn_end.land_held().expect("the episode");
    assert_eq!(live.since_ms, Some(row_ts), "{live:?}");
    assert!(!live.read && live.legs == 0 && live.nudges == 0, "{live:?}");
    let instructed = dir.join("instructed.jsonl");
    std::fs::copy(&ledger, &instructed).expect("the books so far");
    // Its yield, real work that is no landing, and the nudge it is owed.
    let yielded = cc(
        &with(
            &w0,
            &turn(
                Some("(land)"),
                &["⏺ Stage 4 of 5 is in."],
                3 * MIN,
                T0 + 6 * MIN,
            ),
        ),
        &[],
        "",
    );
    s.fx.sleep(Duration::from_secs(4 * 60));
    s.ctl.screens = vec![yielded.clone()];
    s.ctl.served = 0;
    let p = point_at(&s, yielded.clone(), 102);
    let a = s
        .turn_end_at_point(&p, Some(Duration::from_secs(180)), &opts)
        .expect("a decision");
    assert_eq!(a.rule_id(), Some(RULE_LAND_NUDGE), "{a:?}");
    s.turn_end_execute(&p, a, &opts, &[], &mut Alone)
        .expect("decided");
    let nudge_ts = row_of(RULE_LAND_NUDGE).expect("the nudge's row");
    assert_eq!(
        (row_ts, nudge_ts),
        ((T0 + 2 * MIN) * 1000, (T0 + 6 * MIN) * 1000),
        "CONTROL: each row's own stamp"
    );
    let nudged = s.turn_end.land_held().expect("the episode");
    assert_eq!(
        (nudged.since_ms, nudged.nudges),
        (Some(row_ts), 1),
        "{nudged:?}"
    );
    drop(s);
    // The loop that follows, seeded from those books: before the nudge…
    let mut m = Mock::new(vec![Vec::new()]);
    let mut next = session(&mut m, Some(SID.to_string()));
    next.seed_from_ledger(&instructed, row_ts + 60_000, false);
    let seeded = next.turn_end.land_held().expect("the seeded episode");
    assert_eq!(seeded, live, "the same hold, from the same row");
    drop(next);
    // …and after it.
    let mut m = Mock::new(vec![Vec::new()]);
    let mut next = session(&mut m, Some(SID.to_string()));
    next.seed_from_ledger(&ledger, nudge_ts + 60_000, false);
    let seeded = next.turn_end.land_held().expect("the seeded episode");
    assert_eq!(
        (seeded.since_ms, seeded.nudges, seeded.read),
        (Some(row_ts), 1, false),
        "{seeded:?}"
    );
    drop(next);
    // NEGATIVE CONTROL: books with no episode seed none.
    let mut m = Mock::new(vec![Vec::new()]);
    let mut bare = session(&mut m, Some(SID.to_string()));
    bare.seed_from_ledger(&dir.join("none.jsonl"), row_ts + 60_000, false);
    assert_eq!(bare.turn_end.land_held(), None);
    drop(bare);
    let _ = std::fs::remove_dir_all(&dir);
}

/// OFF STANDS AT EVERY PARK THE JOURNAL SAYS, IN THE BOOKS A LOOP THAT
/// FOLLOWS READS (ATERM-KI-1016-1, done-6): a build through 0.105.0 notes
/// `PARKED seq=<n> autopilot=off` as it parks and records no
/// `autopilot-off@v1`; read where that note stands, the Land episode the
/// park ended is over (`approvals::land_episode_in`), and a newer build's
/// own record beside its note reads the same. NEGATIVE CONTROLS, the
/// episode still in hand: another session's park, a park before the
/// episode's instruction, an `UNPARKED` note, no journal.
#[test]
fn off_stands_at_every_park_the_journal_says() {
    use crate::supervise::approvals::{self, Outcome, Row};
    use crate::supervise::journal::JournalRecord;
    use crate::supervise::policy::turn_end::RULE_LAND;
    use crate::supervise::run::{lifecycle::parked_off_note, with_parks_journaled};
    let row = |rule: &str, outcome: Outcome, reason: &str, at: i64| {
        Row {
            rule_id: rule,
            outcome,
            command: "",
            reason,
            box_seq: 1,
        }
        .to_json(at, Some(SID))
    };
    let land = row(RULE_LAND, Outcome::Typed, "the turn-end policy", 1_000);
    let note = |at: i64, sid: &str, line: &str| {
        JournalRecord::of_line(at, Some(sid), line, None).to_json() + "\n"
    };
    let park = |at: i64, sid: &str| note(at, sid, &parked_off_note(7));
    let in_hand = |body: &str, journal: Option<&str>| {
        approvals::land_episode_in(&with_parks_journaled(body, journal, Some(SID)), Some(SID))
            .is_some()
    };
    let books = format!("{land}\n");
    assert!(!in_hand(&books, Some(&park(2_000, SID))), "the park");
    let newer = format!(
        "{land}\n{}\n",
        row(
            approvals::RULE_OFF_STOOD,
            Outcome::Waiting,
            approvals::OFF_STOOD,
            2_001
        )
    );
    assert!(
        !in_hand(&newer, Some(&park(2_000, SID))),
        "a newer build's record too"
    );
    // The park among later rows: placed before the first row at or after it.
    let later = format!(
        "{land}\n{}\n",
        row(RULE_LAND, Outcome::Typed, "the turn-end policy", 3_000)
    );
    assert!(
        in_hand(&later, Some(&park(2_000, SID))),
        "a Land typed after the park"
    );
    // NEGATIVE CONTROLS.
    assert!(
        in_hand(&books, Some(&park(2_000, "s-2"))),
        "another session's park"
    );
    assert!(
        in_hand(&books, Some(&park(500, SID))),
        "a park before the episode"
    );
    let unparked = note(2_000, SID, "UNPARKED seq=7 autopilot=land after 900 ms");
    assert!(in_hand(&books, Some(&unparked)), "an UNPARKED note");
    assert!(in_hand(&books, None), "no journal");
}

/// A LOOP WHOSE SEED FOUND THE LEDGER'S TRY AGED OUT WRITES THE TRY OVER,
/// NOT THE WALL'S END, before its first wall row (RS8-R2,
/// [`crate::supervise::approvals::RULE_TRY_OVER`]). The ledger: a done check
/// typed in a spend limit's place three hours ago, past its two-hour rung.
/// This loop's first point is the spend limit: the try is over for a loop
/// that follows it ([`crate::supervise::approvals::usage_attempt`]), and no
/// row ends a wall — what a wall stands for (a check it cut, a move refused
/// at it) stands, as for this loop. NEGATIVE CONTROL: the try an hour old is
/// inside its rung, and the wall row alone is written. Written as the wall's
/// end ([`crate::supervise::approvals::LIMIT_ENDED`]), a third loop read a
/// check the wall cut as answered by nothing and asked it again after the
/// retry's done reply, and asked a move again at the wall it was refused at.
#[test]
fn a_try_a_seed_found_aged_out_is_over_and_ends_no_wall() {
    use crate::supervise::approvals::{self, Outcome, Row};
    use crate::supervise::policy::turn_end::{LimitTry, RULE_DONE_CHECK};
    let dir = crate::supervise::test_scratch_path("seed-matrix", "try-over");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let limit = LimitTry {
        bucket: false,
        in_place: true,
        attempts: 1,
        entitlement: true,
    };
    for (ago_s, over) in [(3 * HOUR, true), (HOUR, false)] {
        let ledger = dir.join(format!("books-{ago_s}.jsonl"));
        let now_ms = crate::supervise::journal::unix_ms();
        let at = now_ms - ago_s * 1000;
        let wall = approvals::wall_wait_reason(
            None,
            None,
            aterm_phase::WallKind::Spend,
            "You've hit your monthly spend limit.",
        );
        let check = approvals::act_reason(Some(limit));
        let body: String = [
            (
                at - 1000,
                approvals::RULE_WALL_WAIT,
                Outcome::Waiting,
                "",
                &wall,
            ),
            (
                at,
                RULE_DONE_CHECK,
                Outcome::Typed,
                "If the task is finished",
                &check,
            ),
        ]
        .iter()
        .map(|(ts, rule_id, outcome, command, reason)| {
            let row = Row {
                rule_id,
                outcome: *outcome,
                command,
                reason,
                box_seq: 1,
            };
            format!("{}\n", row.to_json(*ts, Some(SID)))
        })
        .collect();
        std::fs::write(&ledger, &body).expect("the books");
        let mut m = Mock::new(vec![Vec::new()]);
        let mut s = session(&mut m, Some(SID.to_string()));
        s.set_zone_lookup(test_zones);
        s.ledger = Ledger::open(Some(&ledger), Some(SID), &mut Vec::new());
        s.turn_end_seeded = true;
        s.set_clock(T0, PDT);
        let open = s.seed_from_ledger(&ledger, now_ms, false);
        s.seed_wall_memory(open);
        let sp = with(
            &work0(),
            &spend_turn(Some("If the task is finished"), 20, T0),
        );
        let rows = cc(&sp, &[], "");
        s.ctl.screens = vec![rows.clone()];
        s.ctl.served = 0;
        let opts = auto(30);
        let p = point_at(&s, rows, 101);
        let a = s.turn_end_at_point(&p, None, &opts).expect("a decision");
        s.turn_end_execute(&p, a, &opts, &[], &mut Alone)
            .expect("decided");
        drop(s);
        let after = std::fs::read_to_string(&ledger).expect("the books");
        let rules: Vec<String> = after
            .lines()
            .skip(2)
            .filter_map(|l| aterm_json::from_str::<aterm_json::Value>(l).ok())
            .filter_map(|v| {
                v.get("rule_id")
                    .and_then(aterm_json::Value::as_str)
                    .map(str::to_string)
            })
            .collect();
        let wall_at = rules
            .iter()
            .position(|r| r == approvals::RULE_WALL_WAIT)
            .unwrap_or_else(|| panic!("{ago_s}: the wall: {rules:?}"));
        let before: Vec<&str> = rules[..wall_at].iter().map(String::as_str).collect();
        let want: &[&str] = if over {
            &[approvals::RULE_TRY_OVER]
        } else {
            &[]
        };
        assert_eq!(before, want, "{ago_s}: {rules:?}");
        assert!(
            !rules.iter().any(|r| r == approvals::RULE_WALL_WAIT_END),
            "{ago_s}: no end of the wall: {rules:?}"
        );
        assert_eq!(
            approvals::usage_attempt(&ledger, Some(SID)).is_none(),
            over,
            "{ago_s}: {after}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE TRY'S FIRST READ CAME LATE, AND A SUCCESSOR ATTACHED INSIDE THE
/// WINDOW IT OPENED AWAITS THE TRY (RS8-F4): the spend limit's try, its
/// first read 70 s on (the Mac asleep, an outage ridden out), its turn's
/// busy read 90 s on, its three-hour turn ending on the limit again. The
/// first loop's window counts from its late read, so the busy read takes
/// the try, and it types 30 minutes from the wall that turn ends on; so does
/// a successor attached after that read, the window the ledger shows (the
/// wall's row) counted from there however late it came. Capped at twice the
/// window, the successor judged the try met at the screen it was typed at,
/// kept its two-hour rung through the try's own turn, and typed into the
/// standing limit 30 minutes sooner. So does a successor attached before
/// that read, 66 s after the try, which no read has seen yet: its own first
/// read is the try's first, however late after the try it comes, as the
/// first loop's was (ATERM-KI-1016-1, follow-2: capped at twice the window,
/// it judged the try met at the screen it was typed at and typed 30 minutes
/// sooner). NEGATIVE CONTROL: a successor that takes nothing up types
/// sooner.
#[test]
fn a_try_s_late_first_read_opens_the_window_a_successor_awaits() {
    let w0 = work0();
    let sp0 = with(&w0, &spend_turn(None, 3 * MIN, T0 + 3 * MIN));
    let spend_wall = cc(&sp0, &[], "");
    let end = with(&sp0, &spend_turn(Some("keep going"), 3 * HOUR, 0));
    let sc = Scenario {
        name: "RS8-F4 spend, take window, the first read 70 s on".to_string(),
        t0: T0,
        policy: default_policy(),
        host: None,
        beats: vec![
            beat(
                At::Start(0),
                "the worker's turn",
                Ev::Busy(live(&w0, "carry on with the task")),
            ),
            beat(
                At::Start(3 * MIN),
                "the spend limit",
                Ev::Point(spend_wall.clone()),
            ),
            beat(
                At::Act(0, 70),
                "the try's screen read",
                Ev::Point(spend_wall),
            ),
            beat(At::Act(0, 75), "(a handoff instant)", Ev::Mark),
            beat(
                At::Act(0, 90),
                "the try's turn runs",
                Ev::Busy(live(&sp0, "keep going")),
            ),
            beat(
                At::Act(0, 90 + 3 * HOUR),
                "the try's 3h turn ends on the limit",
                Ev::Point(cc(&end, &[], "")),
            ),
            taken(1, &end, "keep going"),
        ],
        tail_s: 8 * HOUR,
        take: true,
        person_on_screen: false,
    };
    let dir = crate::supervise::test_scratch_path("seed-matrix", "late-first-read");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let first = run_first(&sc, &dir);
    let tried = first.acts.first().expect("the try").at;
    assert_eq!(first.acts.len(), 2, "the try and the next: {:?}", first.log);
    for after_read in [66, 71, 76] {
        let h = tried + after_read;
        for (seed, want) in [(true, None), (false, Some("SOONER"))] {
            let (theirs, at_k, due, log) = run_successor(&sc, &first, h, &dir, seed);
            assert_eq!(
                verdict(first.acts.get(at_k), theirs.as_ref(), due),
                want,
                "+{after_read} seeded={seed}:\n{}",
                log.join("\n")
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A TRY'S TURN NO LOOP WATCHED IS READ AS THE LOOP THAT WATCHED IT READS IT
/// (ATERM-KI-1016-1, "a long unwatched gap"): the loop that typed a spend
/// limit's try stopped before any read after it, the try's own turn worked
/// 40 minutes and ended on the limit again, and a loop attached a second
/// after that end — its books holding the try and nothing since — reads the
/// done row Claude Code drew there as the try's turn ([`act_turn_work`]):
/// the ladder starts over at that turn's end, the next try 30 minutes on,
/// when the first loop's. Read as a point no read saw run, the try's rung
/// stood from the follower's first look: two hours on. So too a try's turn
/// that worked three hours (its done row's span past the first rung's
/// two). NEGATIVE CONTROL (in the test): the same follower on a screen
/// whose done row is the turn the try was typed after (Claude Code held the
/// line, no turn ran) keeps the try's rung.
#[test]
fn a_tries_turn_no_loop_watched_starts_the_ladder_over_as_the_first_loop_does() {
    let w0 = work0();
    let sp0 = with(&w0, &spend_turn(None, 3 * MIN, T0 + 3 * MIN));
    let spend_wall = cc(&sp0, &[], "");
    let dir = crate::supervise::test_scratch_path("seed-matrix", "unwatched-try");
    for (ran, held) in [(40 * MIN, false), (3 * HOUR, false), (40 * MIN, true)] {
        let end = if held {
            sp0.clone()
        } else {
            with(&sp0, &spend_turn(Some("keep going"), ran, 0))
        };
        let sc = Scenario {
            name: format!("unwatched spend try, its turn {ran} s, held={held}"),
            t0: T0,
            policy: default_policy(),
            host: None,
            beats: vec![
                beat(
                    At::Start(0),
                    "the worker's turn",
                    Ev::Busy(live(&w0, "carry on with the task")),
                ),
                beat(
                    At::Start(3 * MIN),
                    "the spend limit",
                    Ev::Point(spend_wall.clone()),
                ),
                beat(
                    At::Act(0, 2),
                    "the try's turn runs",
                    Ev::Busy(live(&sp0, "keep going")),
                ),
                beat(
                    At::Act(0, 2 + ran),
                    "the try's turn ends on the limit",
                    Ev::Point(cc(&end, &[], "")),
                ),
                taken(1, &end, "keep going"),
            ],
            tail_s: 8 * HOUR,
            take: true,
            person_on_screen: false,
        };
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let first = run_first(&sc, &dir);
        let tried = first.acts.first().expect("the try").at;
        assert_eq!(first.acts.len(), 2, "the try and the next: {:?}", first.log);
        // The books as the loop that typed the try left them: the try, and
        // nothing read after it.
        let rows = before_ms(&first.stamped, (tried + 1) * 1000);
        let journal = before_ms(&first.journal, (tried + 1) * 1000);
        let books = Books {
            rows: &rows,
            journal: &journal,
            restored: None,
        };
        let h = tried + 2 + ran + 1;
        let f = run_follower(&sc, &first, books, (h, 1, Some(tried)), None, &dir, true);
        let why = format!(
            "{}:
  first: {:?}
{}",
            sc.name,
            first.acts,
            f.log.join("\n")
        );
        if held {
            // The try's rung stands: the next try comes two hours after the
            // follower's first look, never the first loop's 30 minutes.
            assert_eq!(
                verdict(first.acts.get(f.at_k), f.diverged.as_ref(), f.due),
                Some("LATER-UNBOUNDED"),
                "CONTROL {why}"
            );
        } else {
            assert_eq!(
                verdict(first.acts.get(f.at_k), f.diverged.as_ref(), f.due),
                None,
                "{why}"
            );
            assert_eq!(f.at_k, 2, "the first loop's next try matched: {why}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// AN ACT'S TIME IS THE WALL CLOCK'S ACROSS A SLEEP (limits-5's review): the
/// act a spend limit's try is, typed and then no read for 40 minutes that
/// the monotonic clock did not count (the matrix's stands between steps,
/// while each step moves the policy's instants with the wall clock:
/// [`Loop::advance`]), read at last at the end of the try's turn, its
/// done row `Worked for 40m 0s` reaching back to the try: the try's own
/// turn, which starts the ladder over at its end — the next try 30 minutes
/// on. Set against the act's time on the take window's clock, which the
/// sleep does not move, the try read as typed when the loop woke, the turn
/// as one that ran before it, and the try's two-hour rung stood. NEGATIVE
/// CONTROL: the line held (no turn ran: the done row is the turn the try was
/// typed after) keeps the rung.
#[test]
fn a_turn_an_act_began_is_dated_from_the_act_across_a_sleep() {
    let w0 = work0();
    let sp0 = with(&w0, &spend_turn(None, 3 * MIN, T0 + 3 * MIN));
    let dir = crate::supervise::test_scratch_path("seed-matrix", "act-across-sleep");
    for held in [false, true] {
        let end = if held {
            sp0.clone()
        } else {
            with(&sp0, &spend_turn(Some("keep going"), 40 * MIN, 0))
        };
        let sc = Scenario {
            name: format!("a spend try's turn read only at its end (held {held})"),
            t0: T0,
            policy: default_policy(),
            host: None,
            beats: vec![
                beat(
                    At::Start(0),
                    "the worker's turn",
                    Ev::Busy(live(&w0, "carry on with the task")),
                ),
                beat(
                    At::Start(3 * MIN),
                    "the spend limit",
                    Ev::Point(cc(&sp0, &[], "")),
                ),
                beat(
                    At::Act(0, 2 + 40 * MIN),
                    "the try's turn ends on the limit, no read having seen it run",
                    Ev::Point(cc(&end, &[], "")),
                ),
            ],
            tail_s: 3 * HOUR,
            take: true,
            person_on_screen: false,
        };
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let first = run_first(&sc, &dir);
        let why = format!("{}\n{}", sc.name, first.log.join("\n"));
        let [tried, next, ..] = first.acts.iter().map(|a| a.at).collect::<Vec<_>>()[..] else {
            panic!("the try and the next: {why}");
        };
        // The loop's first read after the try: the turn's end.
        let read = tried + 2 + 40 * MIN;
        if held {
            assert!(next >= read + 2 * HOUR, "CONTROL {why}");
        } else {
            assert!(
                (read + 30 * MIN..read + 31 * MIN).contains(&next),
                "the ladder over at the turn's end: {why}"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A SLEEP OF THE MAC INSIDE A TRY'S TURN IS NO WORK OF THE TURN'S (limits-5
/// live, `docs/measured/real-claude-lab-2026-10-15.md`): Claude Code times a
/// turn on the wall clock, which runs on through a sleep, so the spend
/// limit's try it answered at once, the Mac asleep 30 minutes from the
/// Enter, draws `✻ Worked for 30m 0s` at the wake. The loop measured the
/// sleep ([`Ev::Woke`], [`Session::watch_sleep`]), so the turn's work is
/// its span less the sleep ([`act_turn_work`]'s `asleep`,
/// [`TurnEndReading::of_slept`]): under the five minutes that end a limit's
/// episode ([`TurnEndTiming::progress`]), the try was met again and keeps
/// its two-hour rung; ten minutes of work around the same sleep start the
/// ladder over at the turn's end — a long turn of work is not counted short.
/// So too where a read saw the try's turn run before the sleep (the loop's
/// own measure, which a sleep stops, three seconds). Read whole, `30m 0s`
/// and `32m 30s` started the ladder over, the next try due 90 minutes
/// sooner. CONTROLS, the Mac awake: 30 minutes of work start the ladder
/// over, two and a half keep the rung.
#[test]
fn a_sleep_inside_a_tries_turn_is_no_work_of_the_turns() {
    let dir = crate::supervise::test_scratch_path("seed-matrix", "sleep-in-try");
    for (what, ran, span, asleep, over) in SLEEP_IN_TRY {
        let sc = sleep_in_try(what, ran, span, asleep);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let first = run_first(&sc, &dir);
        let why = format!("{}\n{}", sc.name, first.log.join("\n"));
        let [tried, next, ..] = first.acts.iter().map(|a| a.at).collect::<Vec<_>>()[..] else {
            panic!("the try and the next: {why}");
        };
        // The loop's first read after the try's turn: its end.
        let read = tried + 2 + span;
        if over {
            assert!(
                (read + 30 * MIN..read + 31 * MIN).contains(&next),
                "the ladder over at the turn's end: {why}"
            );
        } else {
            assert!(next >= read + 2 * HOUR, "the try's rung stands: {why}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A SLEEP INSIDE A PERSON'S TURN IS NO WORK OF IT EITHER (the measured
/// doc's "a person's turn across one", where no act is awaited): the act's
/// answer stopped with an Esc, a person's message read running, the Mac
/// asleep 30 minutes, its turn ends at the wake, its done row counting the
/// night. The turn's span is read less the sleep the loop measured inside
/// it ([`Session::turn_asleep`]): answered in three seconds, their own
/// exchange, left to them — nothing typed; fifteen minutes of work before
/// the same sleep, a job, continued at the turn's end. Read whole, the three
/// seconds were half an hour of work, a job, and the loop typed `keep going`
/// into the person's exchange. CONTROLS, the Mac awake: three seconds are
/// left to the person, fifteen minutes continued.
#[test]
fn a_sleep_inside_a_persons_turn_is_no_work_of_it() {
    let dir = crate::supervise::test_scratch_path("seed-matrix", "sleep-in-person");
    for (what, worked, asleep, left) in SLEEP_IN_PERSON {
        let sc = sleep_in_person(what, worked, asleep, left);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let first = run_first(&sc, &dir);
        let why = format!("{}\n{}", sc.name, first.log.join("\n"));
        let [carried, ref after @ ..] = first.acts.iter().map(|a| a.at).collect::<Vec<_>>()[..]
        else {
            panic!("the act: {why}");
        };
        // The person's turn's end, read first at the wake.
        let end = carried + 90 + worked + asleep;
        if left {
            assert!(after.is_empty(), "left to the person: {why}");
        } else {
            assert!(
                after
                    .first()
                    .is_some_and(|t| (end..end + 2 * MIN).contains(t)),
                "continued at the turn's end: {why}"
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// [`a_sleep_inside_a_persons_turn_is_no_work_of_it`]'s cases: what, the
/// person's turn's work, the Mac asleep at its end, left to the person.
const SLEEP_IN_PERSON: [(&str, i64, i64, bool); 4] = [
    ("a person's 3 s exchange, the Mac asleep", 3, 30 * MIN, true),
    (
        "a person's 15 min job, the Mac asleep",
        15 * MIN,
        30 * MIN,
        false,
    ),
    (
        "CONTROL: a person's 3 s exchange, the Mac awake",
        3,
        0,
        true,
    ),
    (
        "CONTROL: a person's 15 min job, the Mac awake",
        15 * MIN,
        0,
        false,
    ),
];

/// The act's answer stopped with a person's Esc, then their message, read
/// running, whose turn works `worked` seconds and ends at the wake from a
/// sleep of `asleep` ([`SLEEP_IN_PERSON`]; the shape of
/// [`person_scenarios`]' KI1016-P6) — and, not `left` to the person, the
/// act that continues it taken.
fn sleep_in_person(what: &str, worked: i64, asleep: i64, left: bool) -> Scenario {
    let w0 = work0();
    let progress = "⏺ Stage 3 of 5 is in; stage 4 is next.";
    let p0 = cc(
        &with(&w0, &turn(None, &[progress], 0, T0 + 2 * MIN)),
        &[],
        "",
    );
    let h1 = with(
        &w0,
        &s(&[
            "❯ keep going",
            "",
            "⏺ Working on stage 4.",
            "  ⎿  Interrupted · What should Claude do instead?",
            "",
        ]),
    );
    let ask = "what is left?";
    let span = worked + asleep;
    let end = with(
        &h1,
        &turn(Some(ask), &["⏺ The parser and the docs."], span, 0),
    );
    let read_at = if asleep > 0 {
        Ev::Woke(asleep, cc(&end, &[], ""))
    } else {
        Ev::Point(cc(&end, &[], ""))
    };
    let mut beats = vec![
        beat(
            At::Start(0),
            "the worker's turn",
            Ev::Busy(live(&w0, "carry on with the task")),
        ),
        beat(At::Start(2 * MIN), "the worker's progress", Ev::Point(p0)),
        taken(0, &w0, "keep going"),
        beat(At::Act(0, 60), "a person's Esc", Ev::Key),
        beat(
            At::Act(0, 60),
            "the act's turn stopped with Esc",
            Ev::Point(cc(&h1, &[], "")),
        ),
        beat(At::Act(0, 89), "a person's keystroke", Ev::Key),
        beat(
            At::Act(0, 90),
            "a person's message runs",
            Ev::Busy(live(&h1, ask)),
        ),
        beat(
            At::Act(0, 90 + span),
            "the person's turn ends, read first at the wake",
            read_at,
        ),
    ];
    if !left {
        beats.push(taken(1, &end, "keep going"));
    }
    Scenario {
        name: format!("{what}: after an Esc"),
        t0: T0,
        policy: default_policy(),
        host: None,
        beats,
        tail_s: 3 * HOUR,
        take: false,
        person_on_screen: true,
    }
}

/// A SLEEP THAT ENDED BEFORE A PERSON'S PROMPT IS NO PART OF THEIR TURN:
/// the act's answer stopped with a person's Esc, their question answered at
/// once (their own exchange, left to them), the Mac then asleep two hours,
/// idle; the person wakes it and presses Enter five seconds after the wake,
/// and the done row of their message names the minute it ended in. That
/// minute reaches back up to 59 s before the Enter, so the span a loop reads
/// the turn in holds the end of the sleep — but a turn begins and ends
/// awake, so a sleep the system's record places exactly lies wholly inside
/// a turn or wholly outside it
/// ([`crate::supervise::policy::sleeps::Sleep::most_inside`]), and this one
/// is outside. So a loop that follows types as the first loop at every
/// handoff: the person's own 5-minute exchange, ending 50 s into its
/// minute, is left to them, and a 620 s job, ending 30 s into its minute,
/// is continued at its end. Read with the sleep's last 45 s inside the
/// exchange, its keystroke fell outside its 255 s turn (no person's hand),
/// and a loop that followed typed `keep going` into it; with 25 s inside
/// the job, it read as a 595 s exchange, under [`EXCHANGE_WORK`], and a
/// loop that followed left it to the person. CONTROLS, the Mac awake: the
/// same.
#[test]
fn a_sleep_before_a_persons_prompt_is_no_part_of_their_turn() {
    let dir = crate::supervise::test_scratch_path("seed-matrix", "sleep-before-person");
    let mut cases = Vec::new();
    for (work, second, left) in [(5 * MIN, 50, true), (620, 30, false)] {
        for asleep in [2 * HOUR, 0] {
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("dir");
            // The act's own time is the first loop's: the turn's end is
            // moved into its minute once that is known.
            let probe = run_first(&sleep_before_person(work, asleep, 0, left), &dir);
            let a0 = probe.acts.first().expect("the act").at;
            let adj = (second - (a0 + SLEPT_BEFORE_ENTER + work)).rem_euclid(60);
            let sc = sleep_before_person(work, asleep, adj, left);
            let first = run_first(&sc, &dir);
            let why = format!("{}\n{}", sc.name, first.log.join("\n"));
            let end = a0 + SLEPT_BEFORE_ENTER + adj + work;
            assert_eq!(end.rem_euclid(60), second, "the end's second: {why}");
            let after: Vec<i64> = first.acts.iter().skip(1).map(|a| a.at).collect();
            if left {
                assert!(after.is_empty(), "left to the person: {why}");
            } else {
                assert!(
                    after
                        .first()
                        .is_some_and(|t| (end..end + 2 * MIN).contains(t)),
                    "continued at the turn's end: {why}"
                );
            }
            cases.push(sc);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    let runs = run_all(&cases, true);
    let cells: usize = runs.iter().map(|(_, t)| t.cells).sum();
    let broken: Vec<&String> = runs.iter().flat_map(|(_, t)| &t.broken).collect();
    assert!(cells > 40, "the matrix ran {cells} cells");
    assert!(
        broken.is_empty(),
        "{} of {cells} cells break the rule:\n{}",
        broken.len(),
        broken
            .iter()
            .map(|b| b.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
    let (cells, broken) = gap_matrix(&cases, false);
    assert!(
        broken.is_empty(),
        "the gap cells: {} of {cells} break the rule:\n{}",
        broken.len(),
        broken.join("\n")
    );
}

/// [`sleep_before_person`]'s Enter, after the act: the act's answer, a
/// person's exchange, two hours and five minutes on.
const SLEPT_BEFORE_ENTER: i64 = 2 * HOUR + 5 * MIN + 5;

/// The act's answer stopped with a person's Esc and their question
/// answered in three seconds; `asleep` seconds of sleep (none: the Mac
/// awake) ending five seconds before their next message's Enter
/// ([`SLEPT_BEFORE_ENTER`] after the act, `adj` seconds later), which works
/// `work` seconds — and, not `left` to the person, the act that continues
/// it taken.
fn sleep_before_person(work: i64, asleep: i64, adj: i64, left: bool) -> Scenario {
    let w0 = work0();
    let progress = "⏺ Stage 3 of 5 is in; stage 4 is next.";
    let p0 = cc(
        &with(&w0, &turn(None, &[progress], 0, T0 + 2 * MIN)),
        &[],
        "",
    );
    let h1 = with(
        &w0,
        &s(&[
            "❯ keep going",
            "",
            "⏺ Working on stage 4.",
            "  ⎿  Interrupted · What should Claude do instead?",
            "",
        ]),
    );
    let ask1 = "what is left?";
    let end1 = with(
        &h1,
        &turn(Some(ask1), &["⏺ The parser and the docs."], 3, 0),
    );
    let ask = "now do the parser and the docs";
    let end = with(
        &end1,
        &turn(Some(ask), &["⏺ The parser and the docs are in."], work, 0),
    );
    let enter = SLEPT_BEFORE_ENTER + adj;
    let mut beats = vec![
        beat(
            At::Start(0),
            "the worker's turn",
            Ev::Busy(live(&w0, "carry on with the task")),
        ),
        beat(At::Start(2 * MIN), "the worker's progress", Ev::Point(p0)),
        taken(0, &w0, "keep going"),
        beat(At::Act(0, 60), "a person's Esc", Ev::Key),
        beat(
            At::Act(0, 60),
            "the act's turn stopped with Esc",
            Ev::Point(cc(&h1, &[], "")),
        ),
        beat(At::Act(0, 89), "a person's keystroke", Ev::Key),
        beat(
            At::Act(0, 90),
            "a person's question runs",
            Ev::Busy(live(&h1, ask1)),
        ),
        beat(
            At::Act(0, 93),
            "their own exchange ends: left to them",
            Ev::Point(cc(&end1, &[], "")),
        ),
    ];
    if asleep > 0 {
        beats.push(beat(
            At::Act(0, enter - 5),
            "the Mac woke, idle",
            Ev::Woke(asleep, cc(&end1, &[], "")),
        ));
    }
    beats.extend([
        beat(At::Act(0, enter - 1), "a person's keystroke", Ev::Key),
        beat(
            At::Act(0, enter),
            "a person's message runs",
            Ev::Busy(live(&end1, ask)),
        ),
        beat(
            At::Act(0, enter + work),
            "the person's turn ends",
            Ev::Point(cc(&end, &[], "")),
        ),
    ]);
    if !left {
        beats.push(taken(1, &end, "keep going"));
    }
    let mac = if asleep > 0 {
        "asleep before it"
    } else {
        "CONTROL: awake"
    };
    Scenario {
        name: format!("a person's {work} s turn, the Mac {mac} (adj {adj})"),
        t0: T0,
        policy: default_policy(),
        host: None,
        beats,
        tail_s: 3 * HOUR,
        take: false,
        person_on_screen: true,
    }
}

/// SLEEPS AFTER A PERSON'S EXCHANGE LEAVE IT THEIRS, FOR A LOOP THAT
/// FOLLOWS: the act's answer stopped with a person's Esc; their message,
/// read running, is answered in 5m 28s (their own exchange, left to them),
/// two seconds before its minute's end; five seconds later the Mac sleeps
/// an hour, wakes five seconds with no loop reading, and sleeps eight hours
/// (the system's record of its last sleep). The loop that read the
/// person's turn running stopped a second after that read, and a loop
/// attaches a second after the last wake. The ledger counts the hour, but
/// cannot place it: it lies somewhere between the act and the eight hours,
/// so as much of it as the turn's span holds may lie inside it — and none
/// must. A person's keystroke is read against the turn's work at its most
/// (the least sleep inside: `TurnEndReading::worked_most`), so the exchange
/// is still theirs, and the loop that follows types nothing, as the first
/// loop. Read against its work at its least, the 328 s turn was none, the
/// keystroke outside it, and the loop that follows typed `keep going` into
/// it after the short streak's back-off. CONTROL: the eight hours alone,
/// placed exactly after the exchange.
#[test]
fn sleeps_after_a_persons_exchange_leave_it_theirs_for_a_loop_that_follows() {
    let dir = crate::supervise::test_scratch_path("seed-matrix", "exchange-then-sleeps");
    let mut out = Vec::new();
    for (s1, s2) in [(HOUR, 8 * HOUR), (0, 8 * HOUR)] {
        let sc = exchange_then_sleeps(s1, s2);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let mut first = run_first(&sc, &dir);
        let a0 = first.acts.first().expect("the act").at;
        let why = format!("{}\n{}", sc.name, first.log.join("\n"));
        assert_eq!(first.acts.len(), 1, "left to the person: {why}");
        assert_eq!(
            (a0 + 90 + EXCHANGE_THEN_SLEEPS).rem_euclid(60),
            58,
            "the end's second: {why}"
        );
        // The books of the loop that read the person's turn running, cut a
        // second after that read.
        let rows = before_ms(&first.stamped, (a0 + 91) * 1000);
        let journal = before_ms(&first.journal, (a0 + 91) * 1000);
        let last_wake = first.events.iter().map(|e| e.0).max().expect("events");
        first.machine = Machine::default();
        let books = Books {
            rows: &rows,
            journal: &journal,
            restored: None,
        };
        let f = run_follower(
            &sc,
            &first,
            books,
            (last_wake + 1, 1, Some(a0)),
            None,
            &dir,
            true,
        );
        let v = verdict(first.acts.get(f.at_k), f.diverged.as_ref(), f.due);
        out.push((sc.name.clone(), v, f.log.join("\n")));
    }
    let _ = std::fs::remove_dir_all(&dir);
    for (name, v, log) in &out {
        assert_eq!(*v, None, "{name}: the loop that follows:\n{log}");
    }
}

/// [`exchange_then_sleeps`]' person's turn: 5m 28s.
const EXCHANGE_THEN_SLEEPS: i64 = 5 * MIN + 28;

/// [`sleeps_after_a_persons_exchange_leave_it_theirs_for_a_loop_that_follows`]'s
/// shape: a first sleep of `s1` seconds (none: one sleep only), five seconds
/// awake, and the last of `s2`, after the person's exchange.
fn exchange_then_sleeps(s1: i64, s2: i64) -> Scenario {
    let w0 = work0();
    let progress = "⏺ Stage 3 of 5 is in; stage 4 is next.";
    let p0 = cc(
        &with(&w0, &turn(None, &[progress], 0, T0 + 2 * MIN)),
        &[],
        "",
    );
    let h1 = with(
        &w0,
        &s(&[
            "❯ keep going",
            "",
            "⏺ Working on stage 4.",
            "  ⎿  Interrupted · What should Claude do instead?",
            "",
        ]),
    );
    let ask = "what is left?";
    let work = EXCHANGE_THEN_SLEEPS;
    let end1 = with(
        &h1,
        &turn(Some(ask), &["⏺ The parser and the docs."], work, 0),
    );
    let mut beats = vec![
        beat(
            At::Start(0),
            "the worker's turn",
            Ev::Busy(live(&w0, "carry on with the task")),
        ),
        beat(At::Start(2 * MIN), "the worker's progress", Ev::Point(p0)),
        taken(0, &w0, "keep going"),
        beat(At::Act(0, 60), "a person's Esc", Ev::Key),
        beat(
            At::Act(0, 60),
            "the act's turn stopped with Esc",
            Ev::Point(cc(&h1, &[], "")),
        ),
        beat(At::Act(0, 89), "a person's keystroke", Ev::Key),
        beat(
            At::Act(0, 90),
            "a person's message runs",
            Ev::Busy(live(&h1, ask)),
        ),
        beat(
            At::Act(0, 90 + work),
            "their own exchange ends: left to them",
            Ev::Point(cc(&end1, &[], "")),
        ),
    ];
    let mut t = 90 + work + 5;
    if s1 > 0 {
        t += s1;
        beats.push(beat(
            At::Act(0, t),
            "the Mac woke from its first sleep",
            Ev::Woke(s1, cc(&end1, &[], "")),
        ));
        t += 5;
    }
    t += s2;
    beats.push(beat(
        At::Act(0, t),
        "the Mac woke from its last sleep",
        Ev::Woke(s2, cc(&end1, &[], "")),
    ));
    let name = if s1 > 0 {
        format!("a person's exchange, then sleeps of {s1} s and {s2} s")
    } else {
        format!("CONTROL: a person's exchange, then a sleep of {s2} s")
    };
    Scenario {
        name,
        t0: T0,
        policy: default_policy(),
        host: None,
        beats,
        tail_s: 3 * HOUR,
        take: false,
        person_on_screen: true,
    }
}

/// A LOOP ATTACHED AFTER A SLEEP IT NEVER MEASURED reads the try's turn
/// less it as the loop that measured it does (the measured doc's "a
/// follower attached after a sleep it never measured"): the loop that typed
/// a spend limit's try stopped before any read after it, the Mac slept 30
/// minutes inside the try's turn, and a loop attached a second after the
/// wake reads the done row Claude Code drew there. The try's row on the
/// books records the machine's count of its sleep and the boot it counts in
/// (`approvals::SLEPT_MS`), and the loop's own count, read at its first look,
/// says how long the machine slept since the try
/// ([`crate::supervise::policy::sleeps::SleepBook::take_up`]); the system's
/// record of its last sleep places it exactly, with no count on the books.
/// So the try answered at once, or after two and a half minutes of work,
/// keeps its two-hour rung, as the first loop's does, and ten minutes of
/// work still start the ladder over at the turn's end. WHERE NOTHING SAYS —
/// an older build's books (no count) or another boot's count, on a machine
/// that keeps no record of its last sleep — the done row reads whole, as
/// before the count, the ladder over 90 minutes sooner than the first
/// loop's rung (data that does not exist), and the journal says so once
/// (`SLEEP …`); so too where only the system's record counts it. NEGATIVE
/// CONTROL: the same books on the same boot, no record, keep the rung.
#[test]
fn a_loop_attached_after_a_sleep_it_never_measured_reads_the_tries_turn_less_it() {
    let dir = crate::supervise::test_scratch_path("seed-matrix", "unmeasured-sleep");
    let [at_once, short, long, ..] = SLEEP_IN_TRY;
    for (what, ran, span, asleep, over) in [at_once, short, long] {
        let sc = sleep_in_try(what, ran, span, asleep);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let mut first = run_first(&sc, &dir);
        let tried = first.acts.first().expect("the try").at;
        assert_eq!(first.acts.len(), 2, "the try and the next: {:?}", first.log);
        // The books as the loop that typed the try left them: the try, and
        // nothing read after it.
        let rows = before_ms(&first.stamped, (tried + 1) * 1000);
        let older: Vec<(i64, String)> = rows.iter().map(|(t, r)| (*t, uncounted(r))).collect();
        let journal = before_ms(&first.journal, (tried + 1) * 1000);
        assert_ne!(rows, older, "the try's row records the count");
        let h = tried + 2 + span + 1;
        let same_boot = Machine::default();
        let no_last = Machine {
            no_last: true,
            ..Machine::default()
        };
        let rebooted = Machine {
            no_last: true,
            rebooted: true,
        };
        // (what says it, the books, the machine, read whole, `SLEEP` said)
        for (says, books, machine, whole, said) in [
            ("CONTROL: the books' count", &rows, no_last, false, false),
            (
                "the system's record and the books' count",
                &rows,
                same_boot,
                false,
                false,
            ),
            (
                "the system's record, an older build's books",
                &older,
                same_boot,
                false,
                true,
            ),
            (
                "nothing: an older build's books",
                &older,
                no_last,
                true,
                true,
            ),
            ("nothing: another boot's count", &rows, rebooted, true, true),
        ] {
            first.machine = machine;
            let books = Books {
                rows: books,
                journal: &journal,
                restored: None,
            };
            let f = run_follower(&sc, &first, books, (h, 1, Some(tried)), None, &dir, true);
            let why = format!(
                "{} | {says}:\n  first: {:?}\n{}",
                sc.name,
                first.acts,
                f.log.join("\n")
            );
            let v = verdict(first.acts.get(f.at_k), f.diverged.as_ref(), f.due);
            // Read whole, a turn of under five minutes' work starts the
            // ladder over where the first loop's rung stood; one that worked
            // ten minutes started it over for both.
            if whole && !over {
                assert_eq!(v, Some("SOONER"), "{why}");
            } else {
                assert_eq!(v, None, "{why}");
                assert_eq!(f.at_k, 2, "the first loop's next try matched: {why}");
            }
            let sleep_said = f
                .journal
                .iter()
                .filter(|(ms, l)| *ms >= h * 1000 && l.contains(r#""line":"SLEEP "#))
                .count();
            assert_eq!(sleep_said, usize::from(said), "{why}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE SEED MATRIX ACROSS A SLEEP: at every handoff of the scenarios a
/// sleep of the Mac falls in — inside a spend limit's try's turn
/// ([`SLEEP_IN_TRY`]), inside a person's turn ([`SLEEP_IN_PERSON`]), and
/// their controls awake — a loop that follows types as the first loop: one
/// attached before the sleep measures it as the first loop does, one
/// attached after it takes it up from the books' count and the system's
/// record ([`a_loop_attached_after_a_sleep_it_never_measured_reads_the_tries_turn_less_it`]),
/// and a chain of three so; so too one attached a second after the wake
/// where the loop before stopped before it, a turn read running across the
/// sleep on its books ([`gap_cells`]). So too on an older build's books,
/// whose acts count no sleep, by the system's record of its last sleep. NEGATIVE
/// CONTROL: those books on a machine that keeps no such record — nothing
/// says how long it slept before the follower's first look, and the try's
/// turn of seconds read whole starts the ladder over sooner than the first
/// loop.
#[test]
fn a_loop_that_follows_across_a_sleep_types_as_the_first_loop_at_every_handoff() {
    let slept: Vec<Scenario> = SLEEP_IN_TRY
        .iter()
        .map(|&(what, ran, span, asleep, _)| sleep_in_try(what, ran, span, asleep))
        .chain(
            SLEEP_IN_PERSON
                .iter()
                .map(|&(what, worked, asleep, left)| sleep_in_person(what, worked, asleep, left)),
        )
        .collect();
    let runs = run_all(&slept, true);
    let cells: usize = runs.iter().map(|(_, t)| t.cells).sum();
    let broken: Vec<&String> = runs.iter().flat_map(|(_, t)| &t.broken).collect();
    assert!(cells > 100, "the matrix ran {cells} cells");
    assert!(
        broken.is_empty(),
        "{} of {cells} cells break the rule:\n{}",
        broken.len(),
        broken
            .iter()
            .map(|b| b.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
    // THE GAP CELLS ([`gap_cells`]): a loop that stopped before a point —
    // the wake's first read among them, a turn it read running before the
    // sleep on its books — and one attached a second after it, which never
    // measured the sleep.
    let (cells, broken) = gap_matrix(&slept, false);
    assert!(
        broken.is_empty(),
        "the gap cells: {} of {cells} break the rule:\n{}",
        broken.len(),
        broken.join("\n")
    );
    let older = |machine: Machine| {
        std::thread::scope(|scope| {
            let hs: Vec<_> = slept
                .iter()
                .map(|sc| scope.spawn(move || uncounted_cells(sc, machine)))
                .collect();
            hs.into_iter()
                .map(|h| h.join().expect("a scenario's cells"))
                .fold((0, Vec::new()), |(c, mut b), (c1, b1)| {
                    b.extend(b1);
                    (c + c1, b)
                })
        })
    };
    let (cells, broken) = older(Machine::default());
    assert!(
        broken.is_empty(),
        "an older build's books: {} of {cells} cells break the rule:\n{}",
        broken.len(),
        broken.join("\n")
    );
    let (_, broken) = older(Machine {
        no_last: true,
        rebooted: false,
    });
    let at_once = SLEEP_IN_TRY[0].0;
    assert!(
        broken
            .iter()
            .any(|b| b.starts_with(&format!("SOONER: {at_once}: a spend try's turn | "))),
        "CONTROL: an older build's books, no record of the last sleep, broke nothing:\n{}",
        broken.join("\n")
    );
}

/// [`a_sleep_inside_a_tries_turn_is_no_work_of_the_turns`]'s cases: what,
/// the try's turn read running from (seconds after it), the done row's span,
/// the Mac asleep at its end, the ladder over.
const SLEEP_IN_TRY: [(&str, Option<i64>, i64, i64, bool); 6] = [
    (
        "answered at once, the Mac asleep",
        None,
        30 * MIN,
        30 * MIN,
        false,
    ),
    (
        "2.5 min of work, the Mac asleep",
        None,
        32 * MIN + 30,
        30 * MIN,
        false,
    ),
    (
        "10 min of work, the Mac asleep",
        None,
        40 * MIN,
        30 * MIN,
        true,
    ),
    (
        "read running, the Mac asleep",
        Some(1),
        30 * MIN + 3,
        30 * MIN,
        false,
    ),
    (
        "CONTROL: 30 min of work, the Mac awake",
        None,
        30 * MIN,
        0,
        true,
    ),
    (
        "CONTROL: 2.5 min of work, the Mac awake",
        None,
        2 * MIN + 30,
        0,
        false,
    ),
];

/// A spend limit's try whose turn's done row spans `span` seconds, the last
/// `asleep` of them a sleep of the Mac read first at the wake, and read
/// running `ran` seconds after the try ([`SLEEP_IN_TRY`]).
fn sleep_in_try(what: &str, ran: Option<i64>, span: i64, asleep: i64) -> Scenario {
    let w0 = work0();
    let sp0 = with(&w0, &spend_turn(None, 3 * MIN, T0 + 3 * MIN));
    let end = with(&sp0, &spend_turn(Some("keep going"), span, 0));
    let read_at = if asleep > 0 {
        Ev::Woke(asleep, cc(&end, &[], ""))
    } else {
        Ev::Point(cc(&end, &[], ""))
    };
    let mut beats = vec![
        beat(
            At::Start(0),
            "the worker's turn",
            Ev::Busy(live(&w0, "carry on with the task")),
        ),
        beat(
            At::Start(3 * MIN),
            "the spend limit",
            Ev::Point(cc(&sp0, &[], "")),
        ),
    ];
    if let Some(dt) = ran {
        beats.push(beat(
            At::Act(0, dt),
            "the try's turn read running",
            Ev::Busy(live(&sp0, "keep going")),
        ));
    }
    beats.push(beat(
        At::Act(0, 2 + span),
        "the try's turn ends on the limit, read first at the wake",
        read_at,
    ));
    beats.push(taken(1, &end, "keep going"));
    Scenario {
        name: format!("{what}: a spend try's turn"),
        t0: T0,
        policy: default_policy(),
        host: None,
        beats,
        tail_s: 3 * HOUR,
        take: true,
        person_on_screen: false,
    }
}

/// The stamp of a transcript row written when its beat comes: the beat's
/// time on the test's clock ([`run_first`]).
const TAIL_AT: &str = "@AT@";

/// A try a spend limit answers AT ONCE, as Claude Code draws it (the shape
/// measured on 2.1.302's Sonnet bucket): the prompt, the notice under it, and
/// — `done`, Claude Code's `showTurnDuration` on — the turn's done row, its
/// clock the try's minute.
fn spend_answer(prompt: &str, done: bool) -> Vec<String> {
    let mut r = vec![
        format!("❯ {prompt}"),
        "  ⎿  You've hit your monthly spend limit.".to_string(),
        String::new(),
    ];
    if done {
        r.push(format!(
            "✻ Worked for 0s · done @T{}@",
            TURN_IDS.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        r.push(String::new());
    }
    r
}

/// The transcript ending on `prompt` answered by the spend limit's notice,
/// both written when the beat comes ([`TAIL_AT`]).
fn spend_tail(prompt: &str) -> String {
    let rows = [
        format!(
            r#"{{"parentUuid":"p0","isSidechain":false,"type":"user","message":{{"role":"user","content":"{prompt}"}},"uuid":"u1","timestamp":"{TAIL_AT}"}}"#
        ),
        format!(
            r#"{{"parentUuid":"u1","isSidechain":false,"type":"assistant","uuid":"n1","timestamp":"{TAIL_AT}","message":{{"model":"<synthetic>","role":"assistant","content":[{{"type":"text","text":"You've hit your monthly spend limit."}}]}},"error":"rate_limit","isApiErrorMessage":true}}"#
        ),
        format!(
            r#"{{"parentUuid":"n1","isSidechain":false,"type":"system","subtype":"turn_duration","durationMs":40,"timestamp":"{TAIL_AT}","uuid":"d1"}}"#
        ),
    ];
    rows.join("\n") + "\n"
}

/// A spend limit the first loop tries four times, each try answered by the
/// limit at once — drawn with its done row (`done`), or written to the
/// transcript (`transcript`) — or, `held`, the second try's line held by
/// Claude Code: no turn, nothing drawn or written.
fn answered_tries(done: bool, transcript: bool, held: bool) -> Scenario {
    let w0 = work0();
    let mut h = with(&w0, &spend_turn(None, 3 * MIN, T0 + 3 * MIN));
    let mut beats = vec![
        beat(
            At::Start(0),
            "the worker's turn",
            Ev::Busy(live(&w0, "carry on with the task")),
        ),
        beat(
            At::Start(3 * MIN),
            "the spend limit",
            Ev::Point(cc(&h, &[], "")),
        ),
    ];
    for k in 0..4 {
        let held = held && k == 1;
        if !held {
            h = with(&h, &spend_answer("keep going", done));
            if transcript {
                beats.push(beat(
                    At::Act(k, 1),
                    "the transcript: the try met",
                    Ev::Tail(Some(spend_tail("keep going"))),
                ));
            }
        }
        beats.push(beat(
            At::Act(k, 1),
            if held {
                "the try's line held"
            } else {
                "the limit answers the try"
            },
            Ev::Point(cc(&h, &[], "")),
        ));
    }
    Scenario {
        name: format!(
            "spend tries answered at once (done row {done}, transcript {transcript}, held {held})"
        ),
        t0: T0,
        policy: default_policy(),
        host: transcript.then(|| (Vec::new(), None)),
        beats,
        tail_s: 7 * HOUR,
        take: true,
        person_on_screen: false,
    }
}

/// A TRY'S ANSWER NO LOOP READ IS DATED BY THE SCREEN OR THE TRANSCRIPT
/// (ATERM-KI-1016-1, "a loop that follows another"; limits-1): the loop that
/// typed a spend limit's second try stopped before any read after it, and a
/// loop attached later — its books holding that try and nothing since —
/// reads the limit's answer to it where the screen's done row (`Worked for
/// 0s`, the try's minute) or the transcript's notice row (written at the
/// try), or both, date it: the try met at once, as the loop that typed it
/// read it, its first read dated at the latest the signs allow — no sooner
/// than the answer drew, no later than the notice's second and the slack or
/// the done row's minute's end, whichever is earlier — so the follower types
/// the first loop's tries at their times — 6 h 1 min
/// after the try (its rung due in the gap) at once, an hour after it at the
/// rung — and so does a third loop attached on its books. Read as a new
/// appearance (the rule before), the first follower typed 30 minutes after
/// its attach and then every rung of a fresh ladder: a try more than the
/// first loop by 12 h; and an hour after the try it counted the rung from
/// its own read, an hour late. NEGATIVE CONTROL (in the test): the second
/// try's line held (no turn, nothing drawn or written) dates nothing, and
/// the follower keeps the rule before: a new appearance's first rung from
/// its attach, or the rung from its own read.
#[test]
fn a_tries_answer_no_loop_read_is_dated_by_the_screen_or_the_transcript() {
    // Each sign's case on a thread of its own, in a directory of its own:
    // the six share nothing.
    std::thread::scope(|scope| {
        for (done, transcript) in [(true, false), (false, true), (true, true)] {
            for held in [false, true] {
                scope.spawn(move || a_tries_answer_dated_by(done, transcript, held));
            }
        }
    });
}

/// [`a_tries_answer_no_loop_read_is_dated_by_the_screen_or_the_transcript`]
/// where the screen's done row (`done`) or the transcript's notice row
/// (`transcript`) dates the try, its line `held` (the control).
fn a_tries_answer_dated_by(done: bool, transcript: bool, held: bool) {
    let dir = crate::supervise::test_scratch_path("seed-matrix", "dated-try");
    // The first read a follower dated the try at (its record for the loop
    // after it).
    let dated = |f: &Followed| -> Option<i64> {
        f.stamped.iter().find_map(|(_, row)| {
            let tag = "its first read_ms=";
            let at = row.find(tag)? + tag.len();
            let ms: String = row[at..].chars().take_while(char::is_ascii_digit).collect();
            ms.parse().ok()
        })
    };
    let sc = answered_tries(done, transcript, held);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let first = run_first(&sc, &dir);
    let acts: Vec<i64> = first.acts.iter().map(|a| a.at).collect();
    assert!(
        acts.len() >= 4 && acts[2] - acts[1] >= 6 * HOUR && acts[3] - acts[2] >= 6 * HOUR,
        "{}: the first loop's tries {acts:?}\n{}",
        sc.name,
        first.log.join("\n")
    );
    let tried = acts[1];
    // The books as the loop that typed the second try left them: the
    // try, and nothing read after it.
    let cut = first
        .stamped
        .iter()
        .find(|(ms, row)| ms / 1000 == tried && row.contains(",\"decision\":\"typed\","))
        .map(|(ms, _)| ms + 1)
        .expect("the try's row");
    let rows = before_ms(&first.stamped, cut);
    let journal = before_ms(&first.journal, cut);
    let books = Books {
        rows: &rows,
        journal: &journal,
        restored: None,
    };
    for (ago, owed) in [(6 * HOUR + MIN, true), (HOUR, false)] {
        let h = tried + ago;
        let f = run_follower(&sc, &first, books, (h, 2, Some(tried)), None, &dir, true);
        let why = format!(
            "{} | attached +{ago} s after the try:\n  first: {:?}\n{}",
            sc.name,
            first.acts,
            f.log.join("\n")
        );
        let v = verdict(first.acts.get(f.at_k), f.diverged.as_ref(), f.due);
        match (held, owed) {
            (false, _) => {
                assert_eq!((v, f.diverged.as_ref()), (None, None), "{why}");
                assert_eq!(f.at_k, first.acts.len(), "{why}");
                // The answer drew a second after the try.
                let drew = tried + 1;
                let by_notice = transcript.then_some(drew + 10);
                let by_done = done.then_some(drew - drew.rem_euclid(MIN) + MIN - 1);
                let latest = by_notice.into_iter().chain(by_done).min();
                assert!(
                    dated(&f).is_some_and(
                        |ms| ms >= drew * 1000 && latest.is_some_and(|l| ms <= l * 1000)
                    ),
                    "dated {:?}, the answer drew at {drew}, its signs {latest:?}: {why}",
                    dated(&f)
                );
            }
            // CONTROL: a new appearance, its first rung from attaching.
            (true, true) => assert!(
                f.diverged.as_ref().is_some_and(|a| a.at >= h + 30 * MIN),
                "CONTROL {why}"
            ),
            // CONTROL: the rung from the follower's own read.
            (true, false) => assert_eq!(v, Some("LATER-UNBOUNDED"), "CONTROL {why}"),
        }
    }
    // The whole matrix of handoffs over the same timeline.
    let (out, tally) = run_matrix(&sc, true);
    assert!(
        tally.broken.is_empty(),
        "{}\n{out}",
        tally.broken.join("\n")
    );
    if held {
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }
    // A THIRD LOOP on the follower's books, an hour after it attached
    // — and five seconds after, inside the take window its first
    // read would have opened.
    for h2 in [tried + 2 * HOUR, tried + HOUR + 5] {
        let second = run_follower(
            &sc,
            &first,
            books,
            (tried + HOUR, 2, Some(tried)),
            Some(h2),
            &dir,
            true,
        );
        assert!(second.diverged.is_none(), "{:?}", second.log);
        let third = run_follower(
            &sc,
            &first,
            second.books(),
            (h2, second.at_k, second.last),
            None,
            &dir,
            true,
        );
        let why = format!(
            "{}: the third loop at +{} s\n{}",
            sc.name,
            h2 - tried,
            third.log.join("\n")
        );
        assert_eq!(
            (
                verdict(
                    first.acts.get(third.at_k),
                    third.diverged.as_ref(),
                    third.due
                ),
                third.diverged.as_ref()
            ),
            (None, None),
            "{why}"
        );
        assert_eq!(third.at_k, first.acts.len(), "{why}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A LEDGER NO LOOP RECORDED POINTS IN SEEDS NO YIELD (RS8-F2): an older
/// build wrote no point records ([`crate::supervise::approvals::RULE_POINT`]),
/// so its last act may have been answered already, and a turn a successor
/// sees run is judged as someone else's, its short turn lengthening the
/// streak, as before. A ledger this build wrote records every act's answer:
/// its last act's yield is still to come, and moves the streak as the act's
/// own arm does (a wall's retry: none). NEGATIVE CONTROL: the same acts with
/// a point record before them.
#[test]
fn a_ledger_no_loop_recorded_points_in_seeds_no_yield() {
    use crate::supervise::approvals::{self, Outcome, Row};
    use crate::supervise::policy::turn_end::{RULE_API_RETRY, RULE_CONTINUE};
    let dir = crate::supervise::test_scratch_path("seed-matrix", "unrecorded");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    for (recorded, want) in [(false, 4), (true, 3)] {
        let ledger = dir.join(format!("books-{recorded}.jsonl"));
        let now_ms = crate::supervise::journal::unix_ms();
        let marked = approvals::act_reason_after(approvals::Carried::streak(3), None);
        let point = approvals::point_reason(approvals::Carried::streak(3), false);
        let mut rows = vec![(
            now_ms - 20 * MIN * 1000,
            RULE_CONTINUE,
            Outcome::Typed,
            "keep going",
            marked.as_str(),
        )];
        if recorded {
            rows.push((
                now_ms - 15 * MIN * 1000,
                approvals::RULE_POINT,
                Outcome::Waiting,
                "",
                point.as_str(),
            ));
        }
        rows.push((
            now_ms - 10 * MIN * 1000,
            RULE_API_RETRY,
            Outcome::Typed,
            "keep going",
            marked.as_str(),
        ));
        let body: String = rows
            .iter()
            .map(|(ts, rule_id, outcome, command, reason)| {
                let row = Row {
                    rule_id,
                    outcome: *outcome,
                    command,
                    reason,
                    box_seq: 1,
                };
                format!("{}\n", row.to_json(*ts, Some(SID)))
            })
            .collect();
        std::fs::write(&ledger, &body).expect("the books");
        let mut m = Mock::new(vec![Vec::new()]);
        let mut s = session(&mut m, Some(SID.to_string()));
        s.set_zone_lookup(test_zones);
        s.ledger = Ledger::open(Some(&ledger), Some(SID), &mut Vec::new());
        s.turn_end_seeded = true;
        s.set_turn_end_timing(TurnEndTiming {
            take_within: Duration::ZERO,
            ..TurnEndTiming::default()
        });
        s.set_clock(T0, PDT);
        let open = s.seed_from_ledger(&ledger, now_ms, false);
        s.seed_wall_memory(open);
        assert_eq!(s.turn_end.short_streak(), 3, "{recorded}: the act's mark");
        // A turn this loop sees run, and its short end.
        s.turn_end.took();
        let h = with(
            &work0(),
            &turn(
                Some("keep going"),
                &["⏺ Stage 3 of 5: still on it."],
                20,
                T0,
            ),
        );
        let rows = cc(&h, &[], "");
        s.ctl.screens = vec![rows.clone()];
        s.ctl.served = 0;
        let opts = auto(30);
        let p = point_at(&s, rows, 101);
        let _ = s
            .turn_end_at_point(&p, Some(Duration::from_secs(20)), &opts)
            .expect("a decision");
        assert_eq!(s.turn_end.short_streak(), want, "recorded={recorded}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A PERSON'S EXCHANGE THE LOOP BEFORE READ RUNNING IS DATED FROM THAT READ
/// ([`TurnEndState::seed_ran_turn`]): the loop before judged a point 20
/// minutes ago, read the person's turn running 2 minutes ago and stopped. A
/// loop that attaches at the turn's box ([`TurnEndState::box_up`]: no gap
/// left to take) and whose next read is the turn's end, the person's key
/// 10 s old, leaves it to them, as the loop before would have: two minutes
/// of work. Dated from the point, it was 20 minutes of work, a job, and
/// continued. The key 2 min 10 s old, sent after the point but just before
/// the turn was read running (inside
/// [`crate::supervise::policy::turn_end::PERSON_TURN_SLACK`]), leaves it to
/// them too. NEGATIVE CONTROL: a turn read running 15 minutes ago is a job.
#[test]
fn a_persons_exchange_read_running_by_the_loop_before_is_dated_from_that_read() {
    use crate::supervise::approvals::{self, Outcome, Row};
    let dir = crate::supervise::test_scratch_path("seed-matrix", "ran-dated");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let now_ms = T0 * 1000;
    let point = approvals::point_reason(approvals::Carried::streak(0), false);
    for (ran_min, key_ms, left) in [(2, 10_000, true), (2, 130_000, true), (15, 10_000, false)] {
        let ledger = dir.join(format!("books-{ran_min}-{key_ms}.jsonl"));
        let rows = [
            (20, approvals::RULE_POINT, point.as_str()),
            (ran_min, approvals::RULE_TURN_RAN, approvals::TURN_RAN),
        ];
        let body: String = rows
            .iter()
            .map(|(ago_min, rule_id, reason)| {
                let row = Row {
                    rule_id,
                    outcome: Outcome::Waiting,
                    command: "",
                    reason,
                    box_seq: 1,
                };
                format!(
                    "{}\n",
                    row.to_json(now_ms - ago_min * MIN * 1000, Some(SID))
                )
            })
            .collect();
        std::fs::write(&ledger, &body).expect("the books");
        let mut m = Mock::new(vec![Vec::new()]);
        let mut s = session(&mut m, Some(SID.to_string()));
        s.set_zone_lookup(test_zones);
        s.ledger = Ledger::open(Some(&ledger), Some(SID), &mut Vec::new());
        s.turn_end_seeded = true;
        s.set_clock(T0, PDT);
        s.set_mono_standing();
        let open = s.seed_from_ledger(&ledger, now_ms, false);
        s.seed_wall_memory(open);
        s.box_up(101);
        let reply = cc(
            &with(
                &work0(),
                &turn(
                    Some("what do you think?"),
                    &["⏺ Keep the token: it survives a restart."],
                    ran_min * MIN,
                    T0,
                ),
            ),
            &[],
            "",
        );
        s.ctl.screens = vec![reply.clone()];
        s.ctl.served = 0;
        let mut p = point_at(&s, reply, 102);
        p.screen.human = crate::supervise::screen::HumanInput::Ago(key_ms);
        let _ = s
            .turn_end_at_point(&p, None, &auto(30))
            .expect("a decision");
        assert_eq!(
            s.turn_end.left_to_person(),
            left,
            "read running {ran_min} min ago, the key {key_ms} ms ago"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE GAP CELLS OF ONE SCENARIO (round 9, [`run_follower`] on books cut
/// at a point the loop before never read): at every point of the first
/// loop's timeline, a loop that stopped just before it (the first loop, and
/// a loop that followed it from the point before: a chain) and a successor
/// attached a second after it, on the screen as it stands. `unrecorded`:
/// the books without the records of a turn read running
/// ([`crate::supervise::approvals::RULE_TURN_RAN`]), the negative control.
/// The cells counted, and those that break the rule (module docs) with
/// their successor's log.
fn gap_cells(sc: &Scenario, unrecorded: bool) -> (usize, Vec<String>) {
    let dir = crate::supervise::test_scratch_path("seed-matrix", "gap");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let first = run_first(sc, &dir);
    let start = first.events.first().map_or(sc.t0, |e| e.0);
    let mut cells = 0;
    let mut broken = Vec::new();
    for (t, label, ev, _) in &first.events {
        if !matches!(ev, Ev::Point(_) | Ev::Woke(..)) || *t <= start {
            continue;
        }
        let books: Vec<(i64, String)> = first
            .stamped
            .iter()
            .filter(|(ms, row)| {
                *ms < t * 1000
                    && !(unrecorded && row.contains(crate::supervise::approvals::RULE_TURN_RAN))
            })
            .cloned()
            .collect();
        // The journal as the loop before left it: stopped before the
        // point, it said nothing there.
        let journal = before_ms(&first.journal, t * 1000);
        let before: Vec<&Act> = first.acts.iter().filter(|a| a.at < *t).collect();
        let last = before.last().map(|a| a.at);
        let mut follow = |on: Books<'_>, k0: usize, last: Option<i64>, what: &str| {
            let f = run_follower(sc, &first, on, (t + 1, k0, last), None, &dir, true);
            cells += 1;
            if let Some(v) = verdict(first.acts.get(f.at_k), f.diverged.as_ref(), f.due) {
                broken.push(format!(
                    "{v}: {} | {what} stopped before +{} ({label}), attached a second on\n      {}",
                    sc.name,
                    t - sc.t0,
                    f.log.join("\n      ")
                ));
            }
        };
        follow(
            Books {
                rows: &books,
                journal: &journal,
                restored: None,
            },
            before.len(),
            last,
            "the first loop",
        );
        // The chain: a loop attached a second after the point before
        // this one, stopped at this one.
        if let Some(&(t1, ..)) = first.events.iter().rev().find(|(t1, _, ev, _)| {
            t1 < t && *t1 > start && matches!(ev, Ev::Point(_) | Ev::Woke(..))
        }) {
            let before1: Vec<&Act> = first.acts.iter().filter(|a| a.at < t1 + 1).collect();
            let second = run_follower(
                sc,
                &first,
                first.books(),
                (t1 + 1, before1.len(), before1.last().map(|a| a.at)),
                Some(*t),
                &dir,
                true,
            );
            if second.diverged.is_none() {
                let books: Vec<(i64, String)> = second
                    .stamped
                    .iter()
                    .filter(|(_, row)| {
                        !(unrecorded && row.contains(crate::supervise::approvals::RULE_TURN_RAN))
                    })
                    .cloned()
                    .collect();
                let journal = before_ms(&second.journal, t * 1000);
                follow(
                    Books {
                        rows: &books,
                        journal: &journal,
                        restored: None,
                    },
                    second.at_k,
                    second.last,
                    "a loop that followed",
                );
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    (cells, broken)
}

/// [`gap_cells`] over `scenarios`, each on a thread of its own: the cells
/// counted and those that break the rule.
fn gap_matrix(scenarios: &[Scenario], unrecorded: bool) -> (usize, Vec<String>) {
    std::thread::scope(|scope| {
        let hs: Vec<_> = scenarios
            .iter()
            .map(|sc| scope.spawn(move || gap_cells(sc, unrecorded)))
            .collect();
        hs.into_iter()
            .map(|h| h.join().expect("a scenario's gaps"))
            .fold((0, Vec::new()), |(c, mut b), (c1, b1)| {
                b.extend(b1);
                (c + c1, b)
            })
    })
}

/// THE END OF A TURN THE LOOP BEFORE READ RUNNING IS JUDGED AS THE FIRST
/// LOOP JUDGES IT, AT EVERY GAP OF EVERY SCENARIO (ATERM-KI-1016-1's "a loop
/// that follows another": 17 of the matrix's 590 gap cells, in six shapes;
/// and the verifier of 75b03ac6d, RS9-F1). A GAP HANDOFF: the loop before
/// read a turn running and stopped before the turn ended (an update, a
/// restart: its books end at a point it never read), and a successor
/// attaches a second after that point, on the screen as it stands — and so
/// does a loop that took the streak up from a record stopped before (a
/// chain: attached a second after the point before, stopped at this one).
/// A gap cell's successor reads the point the loop before never read as the
/// end of a turn that ran, its work Claude Code's own done row
/// ([`Session::turn_end_at_point`], [`TurnEndState::take_gap`]): the
/// ledger says the turn ran — a turn read running since the point recorded
/// last ([`crate::supervise::approvals::RULE_TURN_RAN`], a loop's first
/// busy read among them), or the limit's try taken
/// ([`crate::supervise::approvals::awaited_try`]). Read as the point
/// recorded before it, the turn moved no streak and the back-off counted
/// from the older point: the successor typed up to a rung sooner than the
/// first loop, into a standing five-hour limit and a standing spend limit
/// too, and read that turn's done report as the answer of the check judged
/// before it. Read as a point no read saw run, a FINISHED that names open
/// work ended the task unchallenged (since the owner's rule of 2026-10-19 a
/// loop's first point judges its words whoever read it, so that shape no
/// longer turns on the record), a spend limit's try met by its own turn
/// counted its next rung from the try (272 s sooner) or kept the try's rung
/// over its own 90 and 19 minutes of work (20 s sooner, 70 minutes later),
/// and a person's `/compact` left the limit's track standing (78 minutes
/// later). NEGATIVE CONTROL: the books with no record of a turn read running
/// break the rule in each of those shapes the record still decides.
#[test]
fn the_end_of_a_turn_the_loop_before_read_running_is_judged_as_the_first_loop_judges_it() {
    let all = scenarios();
    let (cells, broken) = gap_matrix(&all, false);
    assert!(cells > 500, "{cells} gap cells");
    assert!(
        broken.is_empty(),
        "{} of {cells} gap cells break the rule:\n{}",
        broken.len(),
        broken.join("\n")
    );
    // The control's shapes, each with the rule its follower breaks there
    // (empty: any).
    let shapes = [
        ("RS8-R3 3 short turns, then a person's short turn", "SOONER"),
        (
            "R11 five-hour Esc'd, a person's /compact read busy",
            "SOONER",
        ),
        (
            "R11 spend met again, a person's /compact, the ladder",
            "SOONER",
        ),
    ];
    let controls: Vec<Scenario> = all
        .into_iter()
        .filter(|sc| shapes.iter().any(|(name, _)| sc.name == *name))
        .collect();
    assert_eq!(controls.len(), shapes.len(), "the control's scenarios");
    let (_, broken) = gap_matrix(&controls, true);
    for (name, rule) in shapes {
        assert!(
            broken
                .iter()
                .any(|b| b.starts_with(rule) && b.contains(&format!(": {name} | "))),
            "CONTROL: {name}: books with no record of a turn read running broke no rule{}",
            if rule.is_empty() {
                String::new()
            } else {
                format!(" ({rule})")
            }
        );
    }
}

/// A LIMIT NO RESET ENDS COUNTS ITS FIRST RUNG FROM THE WALL FOR A LOOP THAT
/// FOLLOWS (ATERM-KI-1016-1's "a loop that follows another"): at a spend
/// limit and at a bucket that names no reset, a loop attached between the
/// wall and the first try tries when the first loop does, 30 minutes after
/// the wall the loop before waited on (the open `wall-wait@v1` row, the
/// point that judged it: [`TurnEndState::seed_unnamed_wall`]), not 30
/// minutes after its own attach — which the matrix's rule allows as later
/// (a fresh appearance's first rung), and so does not catch. So too where
/// the wall is a five-hour try's answer: met at once, inside its take
/// window (the appearance the point at the window's end), or met by the
/// try's own turn (that turn's end). NEGATIVE CONTROL: a successor that
/// takes nothing up does not try when the first loop does.
#[test]
fn a_limit_no_reset_ends_counts_its_first_rung_from_the_wall_for_a_loop_that_follows() {
    // (scenario, the first loop's act that is the first try at the wall
    // that names no reset, when that wall first shows)
    let picked: Vec<(Scenario, usize, i64)> = scenarios()
        .into_iter()
        .filter_map(|sc| match sc.name.as_str() {
            "D spend (met again, the ladder)"
            | "O Opus bucket, no reset (met again, the ladder)" => Some((sc, 0, 3 * MIN)),
            "R7 five-hour try met by a bucket (the Opus bucket, no reset, take window)"
            | "R7b five-hour try's own turn meets the Opus bucket, no reset"
            | "R7b five-hour try's own turn meets the Opus bucket, no reset, take window" => {
                Some((sc, 1, 0))
            }
            _ => None,
        })
        .collect();
    assert_eq!(picked.len(), 5, "the scenarios");
    for (sc, k, shown) in &picked {
        let dir = crate::supervise::test_scratch_path(
            "seed-matrix",
            &format!("unnamed-{}", sc.name.replace(' ', "-")),
        );
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let first = run_first(sc, &dir);
        let try0 = first.acts.get(*k).expect("the first try").at;
        // The wall's first read: the scenario's, or the act before's answer.
        let from = if *k == 0 {
            sc.t0 + shown
        } else {
            first
                .events
                .iter()
                .map(|e| e.0)
                .filter(|&t| t < try0)
                .max()
                .expect("the wall")
        };
        let before: Vec<i64> = handoffs(&first)
            .into_iter()
            .filter(|&h| h < try0 && h > from + 1)
            .collect();
        assert!(before.len() >= 3, "{}: handoffs {before:?}", sc.name);
        let mut blank_off = 0;
        for &h in &before {
            for seed in [true, false] {
                // Run to a minute past the first loop's try: its own act by
                // then (`last`), or the wait it left standing.
                let acts: Vec<&Act> = first.acts.iter().filter(|a| a.at < h).collect();
                let f = run_follower(
                    sc,
                    &first,
                    first.books(),
                    (h, acts.len(), acts.last().map(|a| a.at)),
                    Some(try0 + MIN),
                    &dir,
                    seed,
                );
                let at = f.last.or(f.due);
                let log = f.log;
                if seed {
                    assert!(
                        at.is_some_and(|at| at <= try0 + 2),
                        "{} handoff +{}: first try +{}, the successor's {:?}\n{}",
                        sc.name,
                        h - sc.t0,
                        try0 - sc.t0,
                        at.map(|a| a - sc.t0),
                        log.join("\n")
                    );
                } else if !at.is_some_and(|at| (try0 - 2..=try0 + 2).contains(&at)) {
                    blank_off += 1;
                }
            }
        }
        assert!(
            blank_off > 0,
            "CONTROL: {}: a successor with no seed tried when the first loop did",
            sc.name
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A LOOP THAT FOLLOWS LEAVES A PERSON'S OWN EXCHANGE TO THEM AS THE FIRST
/// LOOP DOES, EVERY READ STAMPED WITH THE PERSON (ATERM-KI-1016-1), as
/// production's reads carry `human_ms`. At every handoff of every scenario
/// a person's keystroke reaches ([`Ev::Key`]; in one with none, or one whose
/// reads carry it already, a stamped read is the read the base matrix makes:
/// [`a_loop_that_follows_types_as_the_first_loop_at_every_handoff`]'s cells):
/// a successor that takes up acts typed before the task's end, a person's
/// exchange or a park under Off read them as carrying the task, and typed
/// `keep going` into a person's short turn after it where the first loop
/// left it to them (7 of 4051 cells: Off lifted in a back-off, then a
/// person's exchange; a person's new short turn after the task's end;
/// [`crate::supervise::approvals::carry_ended_in`]). And at every gap cell
/// of the scenarios that hold a person's exchange: a follower whose first
/// point ends a person's turn the loop before read running read it as a
/// first point no read saw run, never left to a person, and typed
/// `keep going` after the back-off ([`TurnEndState::seed_ran_turn`]).
/// So too where the act's answer was stopped by an Esc that is no act of the
/// harness's, then a person's short turn ran ([`person_scenarios`]): the
/// first loop's carry ends at that answer, but a successor attached after it
/// read the point's record as no end of the carry, and one attached before
/// it judged the answer as the seeded act's yield and kept the carry, both
/// typing `keep going` into the person's exchange (10 of 15 cells, the
/// review's KR-P6-ESC; [`crate::supervise::approvals::carry_ended_in`],
/// [`TurnEndState::seed_yield`]).
/// NEGATIVE CONTROL: the same gap cells over books with no record of the
/// turn read running type sooner.
#[test]
fn a_loop_that_follows_leaves_a_persons_exchange_as_the_first_loop_does() {
    let stamped = |sc: Scenario| Scenario {
        person_on_screen: true,
        ..sc
    };
    let keyed: Vec<Scenario> = scenarios()
        .into_iter()
        .filter(|sc| !sc.person_on_screen && sc.beats.iter().any(|b| matches!(b.ev, Ev::Key)))
        .map(stamped)
        .collect();
    assert!(
        keyed.len() >= 8,
        "{} scenarios a keystroke reaches",
        keyed.len()
    );
    let runs = run_all(&keyed, true);
    let cells: usize = runs.iter().map(|(_, t)| t.cells).sum();
    let broken: Vec<&String> = runs.iter().flat_map(|(_, t)| &t.broken).collect();
    assert!(cells > 200, "the matrix ran {cells} cells");
    assert!(
        broken.is_empty(),
        "{} of {cells} cells break the rule:\n{}",
        broken.len(),
        broken
            .iter()
            .map(|b| b.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
    let persons: Vec<Scenario> = scenarios()
        .into_iter()
        .map(stamped)
        .filter(|sc| {
            [
                "R13 Off lifts in the back-off, a person's exchange",
                "KI1016-P6",
            ]
            .iter()
            .any(|p| sc.name.starts_with(p))
        })
        .collect();
    assert_eq!(persons.len(), 3, "the person's scenarios");
    let (cells, broken) = gap_matrix(&persons, false);
    assert!(cells > 10, "{cells} gap cells");
    assert!(
        broken.is_empty(),
        "{} of {cells} gap cells break the rule:\n{}",
        broken.len(),
        broken.join("\n")
    );
    // NEGATIVE CONTROL.
    let (_, broken) = gap_matrix(&persons, true);
    assert!(
        !broken.is_empty(),
        "CONTROL: books with no record of the turn read running broke nothing"
    );
}

/// AN ACT'S ANSWER NO LOOP READ RUN, STOPPED BY AN ESC THAT IS NO ACT OF THE
/// HARNESS'S, ENDS THE CARRY FOR A LOOP THAT FOLLOWS (the review's
/// KR-P6-ESC, [`person_scenarios`]). The loop that typed `keep going`
/// stopped before any read saw its answer run (its books end at the act),
/// and a follower attaches a second after the answer's Esc'd point: its
/// first point is the act's yield, which the books say is still to come
/// ([`TurnEndState::seed_yield`]), stopped by someone's Esc. The first loop
/// ends its carry there, so the person's short turn after it is theirs;
/// the follower must type nothing into it either — nor one on books that
/// hold the act's answer read running (the gap cell). Mutation-checked:
/// with the first point's arm keeping the carry the first case types
/// `keep going` into the person's exchange.
#[test]
fn an_acts_answer_no_loop_read_run_stopped_by_an_esc_ends_the_carry_for_a_loop_that_follows() {
    let dir = crate::supervise::test_scratch_path("seed-matrix", "esc-unseen-yield");
    for sc in person_scenarios() {
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let first = run_first(&sc, &dir);
        assert!(first.acts.len() <= 1, "{}: {:?}", sc.name, first.acts);
        let act = first.acts.first().expect("the first loop's act").at;
        let esc = first
            .events
            .iter()
            .find(|(_, label, ..)| label == "the act's turn stopped with Esc")
            .map(|e| e.0)
            .expect("the Esc'd point");
        let follow = |cut: i64, h: i64| {
            let rows = before_ms(&first.stamped, cut * 1000);
            let journal = before_ms(&first.journal, cut * 1000);
            let on = Books {
                rows: &rows,
                journal: &journal,
                restored: None,
            };
            let f = run_follower(&sc, &first, on, (h, 1, Some(act)), None, &dir, true);
            (
                verdict(first.acts.get(f.at_k), f.diverged.as_ref(), f.due),
                f.log.join("\n  "),
            )
        };
        // The books end at the act: no read saw its answer run.
        let (v, log) = follow(act + 1, esc + 1);
        assert_eq!(
            v, None,
            "{}: attached after the Esc'd point\n  {log}",
            sc.name
        );
        let (v, log) = follow(esc, esc + 1);
        assert_eq!(v, None, "{}: its answer read running\n  {log}", sc.name);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE BOOKS A BUILD THROUGH 0.106.0 WROTE of the first loop's run (`rows`,
/// its ledger as this build wrote it): no record of a point, a turn read
/// running, Off standing, a task's end, a Land episode's end, a try aged
/// out, a person's exchange left to them, a fallback's turn asked or its
/// relaunch waiting; no mark on an act (the short streak, the job's
/// challenge, the wall's retries, the limit's try), no `made=` on a model
/// switch, no `kind=` on a wall's row; and one end of a wall but a
/// conversation with no task (that build's
/// `the wall left the screen or the worker worked`). The journal is the
/// same: every build writes its lines in these words.
fn books_of_0_106(rows: &[(i64, String)]) -> Vec<(i64, String)> {
    use crate::supervise::approvals::{
        FALLBACK_ASKED, LIMIT_ENDED, RULE_LAND_OVER, RULE_OFF_STOOD, RULE_POINT, RULE_TASK_DONE,
        RULE_TRY_OVER, RULE_TURN_RAN, RULE_WALL_WAIT, RULE_WALL_WAIT_END, WALL_TAKEN,
    };
    use crate::supervise::policy::turn_end::{RULE_LEFT, RULE_MODEL_FALLBACK};
    const WALL_LEFT: &str = "the wall left the screen or the worker worked";
    const MARKS: [&str; 5] = [
        " (short turns in a row: ",
        " (the open-items challenge was typed this job)",
        " (the wall's retries: ",
        " (the limit's try: ",
        " (in the limit's place: ",
    ];
    let records = [
        RULE_POINT,
        RULE_TURN_RAN,
        RULE_OFF_STOOD,
        RULE_TASK_DONE,
        RULE_LAND_OVER,
        RULE_TRY_OVER,
        RULE_LEFT,
    ];
    let with_reason = |row: &str, reason: &str| -> String {
        let a = row.find(",\"reason\":").expect("a reason");
        let b = row.find(",\"box_seq\":").expect("a box seq");
        format!(
            "{},\"reason\":{}{}",
            &row[..a],
            crate::supervise::journal::json_str(reason),
            &row[b..]
        )
    };
    rows.iter()
        .filter_map(|(ms, row)| {
            let v: aterm_json::Value = aterm_json::from_str(row).ok()?;
            let text = |k: &str| v.get(k).and_then(aterm_json::Value::as_str).unwrap_or("");
            let (rule, reason) = (text("rule_id"), text("reason"));
            let row = match (text("decision"), rule) {
                (_, r) if records.contains(&r) => return None,
                ("waiting", RULE_MODEL_FALLBACK)
                    if reason == FALLBACK_ASKED
                        || reason.starts_with("the fallback's relaunch was asked") =>
                {
                    return None;
                }
                ("typed", _) if reason.starts_with("the turn-end policy") => {
                    let mut r = reason.to_string();
                    // A mark's words hold no `)` of their own.
                    for mark in MARKS {
                        while let Some(a) = r.find(mark) {
                            let from = a + mark.len() - 1;
                            let Some(b) = r[from..].find(')') else { break };
                            r.replace_range(a..=from + b, "");
                        }
                    }
                    if let Some(a) = r.find(" made=") {
                        let b = r[a..].find(')').map_or(r.len(), |b| a + b);
                        r.replace_range(a..b, "");
                    }
                    with_reason(row, &r)
                }
                (_, RULE_WALL_WAIT) => match (reason.find("; kind="), reason.find("; message=")) {
                    (Some(a), Some(b)) if a < b => {
                        with_reason(row, &format!("{}{}", &reason[..a], &reason[b..]))
                    }
                    _ => row.clone(),
                },
                (_, RULE_WALL_WAIT_END) if reason == LIMIT_ENDED || reason == WALL_TAKEN => {
                    with_reason(row, WALL_LEFT)
                }
                _ => row.clone(),
            };
            // Nor did that build count the machine's sleep on an act's row.
            Some((*ms, uncounted(&row)))
        })
        .collect()
}

/// `row` without the machine's count of its sleep an act's row ends on
/// (`approvals::SLEPT_MS`, `approvals::BOOT`): a row an older build wrote.
fn uncounted(row: &str) -> String {
    let field = format!(",\"{}\":", crate::supervise::approvals::SLEPT_MS);
    match row.find(&field) {
        Some(a) => format!("{}}}", &row[..a]),
        None => row.to_string(),
    }
}

/// The cells of `sc` a loop breaks that follows, at every handoff, one of a
/// build before the machine's count of its sleep: the first loop's books,
/// no act's row counting it ([`uncounted`]), and its journal, on `machine`.
fn uncounted_cells(sc: &Scenario, machine: Machine) -> (usize, Vec<String>) {
    let dir = crate::supervise::test_scratch_path("seed-matrix", "uncounted");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let mut first = run_first(sc, &dir);
    first.machine = machine;
    let rows: Vec<(i64, String)> = first
        .stamped
        .iter()
        .map(|(t, row)| (*t, uncounted(row)))
        .collect();
    let mut cells = 0;
    let mut broken = Vec::new();
    for h in handoffs(&first) {
        let before: Vec<&Act> = first.acts.iter().filter(|a| a.at < h).collect();
        let on = Books {
            rows: &rows,
            journal: &first.journal,
            restored: None,
        };
        let f = run_follower(
            sc,
            &first,
            on,
            (h, before.len(), before.last().map(|a| a.at)),
            None,
            &dir,
            true,
        );
        cells += 1;
        if let Some(v) = verdict(first.acts.get(f.at_k), f.diverged.as_ref(), f.due) {
            broken.push(format!(
                "{v}: {} | attached at +{}\n      {}",
                sc.name,
                h - sc.t0,
                f.log.join("\n      ")
            ));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    (cells, broken)
}

/// The cells of `sc` a loop breaks that follows one of a build through
/// 0.106.0 at every handoff: that build's books of the first loop's run
/// ([`books_of_0_106`]), and its journal (`journal`) or none.
fn older_cells(sc: &Scenario, i: usize, journal: bool, restored: bool) -> (usize, Vec<String>) {
    let dir = crate::supervise::test_scratch_path(
        "seed-matrix",
        &format!("older-{i}-{journal}-{restored}"),
    );
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let first = run_first(sc, &dir);
    let rows = books_of_0_106(&first.stamped);
    // That build's journal, its points' lines in it (`EVENT …`) as every
    // build writes them, each before what the point decided: read as every
    // point line ending what was held, a wall's wait lost the short streak
    // across it and a point inside an act's take window the act it still
    // awaited (`journaled_hold`, the review of 2026-10-20).
    let said = if journal {
        first.journal.clone()
    } else {
        Vec::new()
    };
    let mut cells = 0;
    let mut broken = Vec::new();
    for h in handoffs(&first) {
        let before: Vec<&Act> = first.acts.iter().filter(|a| a.at < h).collect();
        let on = Books {
            rows: &rows,
            journal: &said,
            restored: restored.then_some(Restore {
                at_ms: h * 1000,
                noted: true,
            }),
        };
        let f = run_follower(
            sc,
            &first,
            on,
            (h, before.len(), before.last().map(|a| a.at)),
            None,
            &dir,
            true,
        );
        cells += 1;
        if let Some(v) = verdict(first.acts.get(f.at_k), f.diverged.as_ref(), f.due) {
            broken.push(format!(
                "{v}: {} | attached at +{}\n      {}",
                sc.name,
                h - sc.t0,
                f.log.join("\n      ")
            ));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    (cells, broken)
}

/// A LOOP THAT FOLLOWS ONE OF A BUILD THROUGH 0.106.0 — the update's first
/// loop — TYPES AS THE FIRST LOOP AT EVERY HANDOFF (ATERM-KI-1016-1,
/// follow-5). That build's books hold no record of a point and no mark on
/// an act ([`books_of_0_106`]): read alone, the follower backed off its
/// first point from its own attach on the short ladder's first rung (up to
/// the ladder's top sooner), retried an overload on its first rung (an
/// hour sooner), read an unmarked spend limit's try as a usage window's
/// first (2 hours sooner) and the screen a try was typed at as its answer,
/// took a person's prompt that met a bucket as the fallback's redraw and
/// typed into the standing bucket, and read past Off's park (done-6): a Land episode the park ended nudged
/// where the first loop typed the instruction anew, and a done check a wall
/// cut before the park ended by the retry's done reply where the first loop
/// asked the check again. What that build's loop journaled says each: the
/// waits it held and the streak they name, its retries' rungs, an act's
/// take window, a task's end, the relaunch's first ask, the challenge, the
/// park (`PARKED seq=<n> autopilot=off`, `with_parks_journaled`). And so in
/// a tab a cold restore reopened after the update, from the earlier tab's
/// books and journal (follow-7). The journal holds that build's point lines
/// too, each before what the point decided: read as each ending what was
/// held, the follower typed sooner across a wall's wait (the short streak
/// lost) and at a point inside an act's take window (an overload's retry
/// rung lost) — the review of 2026-10-20, 18 cells.
/// NEGATIVE CONTROL: the same books with no
/// journal beside them type sooner at the short ladder, an overload's
/// retries, a spend limit's try and the bucket a person's prompt met, and
/// read past the park a Land episode's instruction stood under.
#[test]
fn a_loop_after_an_older_builds_loop_types_as_that_loop_at_every_handoff() {
    let names = [
        "RS8-F3 short turns in a row to the ladder's top",
        "API 529 met again at every rung, then taken, take window",
        "API 500 met again at every rung, then taken",
        "O Opus bucket, no reset (met again, the ladder)",
        "D spend (met again, the ladder)",
        "R1 spend, take window, the read latency (the first read after the try 70 s on)",
        "RS8-F2 short turns, the spend limit, the try's real work",
        "J bucket, the fallback (wait:resume thrice, a person's prompt met by the bucket \
         before the adoption)",
        "RS9-F4 Land, the land act answered not done, then a done report (nobody's hand)",
        "RS8-F2 Land, landed, then a new task's short turn (progress)",
        "R4 Off lifts at a done check the weekly limit cut",
        "R4 Off lifts during the land instruction's turn (Land)",
    ];
    let all = scenarios();
    let picked: Vec<&Scenario> = names
        .iter()
        .map(|n| all.iter().find(|sc| sc.name == *n).expect(n))
        .collect();
    let run = |picked: &[&Scenario], journal: bool, restored: bool| {
        std::thread::scope(|scope| {
            let hs: Vec<_> = picked
                .iter()
                .enumerate()
                .map(|(i, sc)| scope.spawn(move || older_cells(sc, i, journal, restored)))
                .collect();
            hs.into_iter()
                .map(|h| h.join().expect("a scenario's cells"))
                .fold((0, Vec::new()), |(c, mut b), (c1, b1)| {
                    b.extend(b1);
                    (c + c1, b)
                })
        })
    };
    let (cells, broken) = run(&picked, true, false);
    assert!(cells > 200, "{cells} cells");
    assert!(
        broken.is_empty(),
        "{} of {cells} cells break the rule:\n{}",
        broken.len(),
        broken.join("\n")
    );
    // A cold restore after the update (aterm ended under that build, and
    // this one reopened its tabs under new session ids): the earlier tab's
    // books and journal, taken up from beside the new ledger (follow-7).
    let (cells, broken) = run(&picked, true, true);
    assert!(
        broken.is_empty(),
        "restored: {} of {cells} cells break the rule:\n{}",
        broken.len(),
        broken.join("\n")
    );
    // The control's shapes, each with the rule its follower breaks there.
    let controls = [
        (names[0], "SOONER"),
        (names[1], "SOONER"),
        (names[5], "SOONER"),
        (names[7], "SOONER"),
        (names[11], "OTHER-TEXT"),
    ];
    let (_, broken) = run(
        &picked
            .iter()
            .copied()
            .filter(|sc| controls.iter().any(|(name, _)| sc.name == *name))
            .collect::<Vec<_>>(),
        false,
        false,
    );
    for (name, rule) in controls {
        assert!(
            broken
                .iter()
                .any(|b| b.starts_with(&format!("{rule}: {name} | "))),
            "CONTROL: {name}: the books with no journal beside them broke no rule ({rule})"
        );
    }
}

/// [`a_loop_in_a_tab_a_cold_restore_reopened_types_as_the_first_loop_at_every_handoff`]'s
/// cells for scenario `sc` (the `i`th): a follower attached at each handoff
/// in a tab a cold restore reopened from tab [`WAS`], on that tab's books
/// ([`Books::restored`]), the restore's note beside its own ledger or
/// (`noted` false, the control) none; and the tab's loop STOPPED IN EACH
/// POINT'S WINDOW ([`Window`]: aterm quit as the host's step took the point)
/// and the restore reopening it a second on — and as Off lifts, where Off
/// stood and lifts next ([`First::window_attach`]) — on the books it left
/// ([`in_window`]; `owed`: without the task's end said in the window, the
/// control).
fn restored_cells(sc: &Scenario, i: usize, noted: bool, owed: bool) -> (usize, Vec<String>) {
    let dir =
        crate::supervise::test_scratch_path("seed-matrix", &format!("restored-{i}-{noted}-{owed}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let first = run_first(sc, &dir);
    let mut cells = 0;
    let mut broken = Vec::new();
    let mut cell = |what: String, f: &Followed| {
        cells += 1;
        if let Some(v) = verdict(first.acts.get(f.at_k), f.diverged.as_ref(), f.due) {
            broken.push(format!(
                "{v}: {} | {what}\n      {}",
                sc.name,
                f.log.join("\n      ")
            ));
        }
    };
    let hs = handoffs(&first);
    for (i, &h) in hs.iter().enumerate() {
        let before: Vec<&Act> = first.acts.iter().filter(|a| a.at < h).collect();
        let restored = Some(Restore {
            at_ms: h * 1000,
            noted,
        });
        let on = Books {
            restored,
            ..first.books()
        };
        let last = before.last().map(|a| a.at);
        let f = run_follower(sc, &first, on, (h, before.len(), last), None, &dir, true);
        cell(format!("attached at +{}", h - sc.t0), &f);
        // A loop that follows the restored tab's first (a seamless update, a
        // new window): the earlier tab's books, then the tab's own.
        if let Some(&h2) = hs.get(i + 1) {
            let second = run_follower(
                sc,
                &first,
                on,
                (h, before.len(), last),
                Some(h2),
                &dir,
                true,
            );
            if second.diverged.is_none() {
                let on = Books {
                    restored,
                    ..second.books()
                };
                let mut third = run_follower(
                    sc,
                    &first,
                    on,
                    (h2, second.at_k, second.last),
                    None,
                    &dir,
                    true,
                );
                let mut log = second.log;
                log.append(&mut third.log);
                third.log = log;
                cell(
                    format!("attached at +{} then +{}", h - sc.t0, h2 - sc.t0),
                    &third,
                );
            }
        }
    }
    for w in &first.windows {
        let (rows, journal) = in_window(first.books(), *w, owed);
        let (k0, last) = typed_on(&rows);
        for h in first.window_attach(w.at) {
            let on = Books {
                rows: &rows,
                journal: &journal,
                restored: Some(Restore {
                    at_ms: h * 1000,
                    noted,
                }),
            };
            let f = run_follower(sc, &first, on, (h, k0, last), None, &dir, true);
            cell(
                format!(
                    "stopped in the window at +{}, restored at +{}",
                    w.at - sc.t0,
                    h - sc.t0
                ),
                &f,
            );
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    (cells, broken)
}

/// A LOOP IN A TAB A COLD RESTORE REOPENED (ATERM-KI-1016-1, follow-7): the
/// restore reopens each tab under a new session id, so the loop's ledger is
/// the new tab's and holds nothing; the books of the conversation's loop are
/// the tab's it was reopened from, which the restore notes beside the new
/// ledger (`upgrade_drive::note_earlier_tab`). Read from its own ledger
/// alone, the follower backed off its first point from its attach, started a
/// limit's ladder over, and typed Land's instruction anew where the loop
/// before had nudged or spent the landing. Every scenario of the matrix, a
/// follower at each handoff on the earlier tab's books: typed as the first
/// loop. And at every point's window, the tab's loop stopped there (aterm
/// quit as the host's step took the point) and the restore reopening it: as
/// the first loop too. NEGATIVE CONTROL: the same books with no note beside
/// the new ledger break the rule in each of those three shapes: the short
/// ladder and a spend limit's ladder typed sooner, Land's instruction typed
/// anew; and the window's books without the task's end said in it — its
/// note owed to the point's act, as `look` owed it until 2026-10-20 — get
/// a landing said in words the land instruction again as Off lifts.
#[test]
fn a_loop_in_a_tab_a_cold_restore_reopened_types_as_the_first_loop_at_every_handoff() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let run = |scenarios: &[Scenario], noted: bool, owed: bool| {
        let next = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            let hs: Vec<_> = (0..MATRIX_WORKERS.min(scenarios.len()))
                .map(|_| {
                    scope.spawn(|| {
                        let mut done = (0, Vec::new());
                        loop {
                            let i = next.fetch_add(1, Ordering::Relaxed);
                            let Some(sc) = scenarios.get(i) else {
                                return done;
                            };
                            let (c, b) = restored_cells(sc, i, noted, owed);
                            done.0 += c;
                            done.1.extend(b);
                        }
                    })
                })
                .collect();
            hs.into_iter()
                .map(|h| h.join().expect("a scenario's cells"))
                .fold((0, Vec::new()), |(c, mut b), (c1, b1)| {
                    b.extend(b1);
                    (c + c1, b)
                })
        })
    };
    let all = scenarios();
    let (cells, broken) = run(&all, true, false);
    assert!(cells > 500, "{cells} cells");
    assert!(
        broken.is_empty(),
        "{} of {cells} cells break the rule:\n{}",
        broken.len(),
        broken.join("\n")
    );
    // The control's shapes, each with the rule its follower breaks there.
    let shapes = [
        ("RS8-F3 short turns in a row to the ladder's top", "SOONER"),
        ("D spend (met again, the ladder)", "SOONER"),
        ("R2 Land, the landing spent", ""),
    ];
    let controls: Vec<Scenario> = all
        .into_iter()
        .filter(|sc| shapes.iter().any(|(name, _)| sc.name == *name))
        .collect();
    assert_eq!(controls.len(), shapes.len(), "the control's scenarios");
    let (_, broken) = run(&controls, false, false);
    for (name, rule) in shapes {
        assert!(
            broken
                .iter()
                .any(|b| b.starts_with(rule) && b.contains(&format!(": {name} | "))),
            "CONTROL: {name}: the earlier tab's books with no note beside the new ledger broke no \
             rule{}",
            if rule.is_empty() {
                String::new()
            } else {
                format!(" ({rule})")
            }
        );
    }
    let landed: Vec<Scenario> = self::scenarios()
        .into_iter()
        .filter(|sc| sc.name == "R4 Off lifts after the landing (Land, in words)")
        .collect();
    assert_eq!(landed.len(), 1, "the landing's scenario");
    let (_, broken) = run(&landed, true, true);
    assert!(
        broken.iter().any(|b| {
            b.starts_with("TYPES-WHERE-FIRST-DOES-NOT") && b.contains(" stopped in the window at ")
        }),
        "CONTROL: the landing's note owed past its window broke nothing in a restored tab"
    );
}
