// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CLAUDE CODE FOOTER, host side: aterm writes `◆ Opus 5.5 xhigh   ⌂
//! ~/aterm   ⎇ main` INTO the rule under Claude Code's input box,
//! right-aligned the way Claude writes its effort tag into the rule above
//! (owner directions, 2026-09-24 and 2026-09-28). What the footer says and
//! how a narrow rule gives it up are decided in
//! `aterm_agent::harness::footer`; this module gets the facts off the event
//! loop and puts them on the glass.
//!
//! * FACTS. One background thread ([`request_footer`]) reads them — the
//!   process's `sessions/<pid>.json`, its transcript, its own `--model`,
//!   `.git/HEAD`, and what its transcripts APPENDED since the last read (the
//!   session's usage, folded incrementally in its `footer::FooterCache`) —
//!   and publishes them on the session's timeline
//!   (`SessionTimeline::set_claude_footer`), exactly as `session_program`'s
//!   resolver publishes the program. What it has read of each Claude Code
//!   process — its model pin among it — is kept while that process lives,
//!   across a lapsed or stopped watch ([`ProcessTails`]), and a new aterm
//!   process starts with none. The status sweep asks for a refresh at
//!   most every [`RECHECK`] while a Claude Code session is in front. A change
//!   wakes the loop ([`post_changed`] → `Wake::ClaudeFooter`) so the frame that
//!   shows it is not left to the next keystroke. Where nothing since the
//!   process started names its model or effort, the frame compose reads
//!   Claude's launch card off the pane (`footer::launch_card`) on every
//!   frame while that holds, and keeps the newest reading for that process
//!   and session (`SessionTimeline::note_claude_card`).
//! * THE LIMIT WALL. A limit the session HIT, written into its transcript
//!   (`⧗ 5h limit · resets 3pm`), stands until its reset passes or a response
//!   is served after it; while one shows, the resolver keeps re-reading the
//!   session every [`WALL_RECHECK`] past [`WATCH_FOR`] for as long as its
//!   Claude Code process is alive, so a passed wall leaves an idle footer.
//!   Its reset is placed with a zone's offset AT an instant
//!   ([`offset_at_cached`]).
//! * THE ACCOUNT. Where `accounts.toml` lists two or more, `⎆ <label>` names
//!   the roster row the process's login is (`accounts::footer_label`),
//!   between the model and the path. The login is what the exec environment
//!   of the image Claude Code REGISTERED says, read with its command line
//!   once its registry row is seen ([`Identity`]) — never what a launcher
//!   that became Claude Code in place was started with; the roster is the
//!   resolver thread's own memo ([`RosterMemo`]), read again at most every
//!   [`ROSTER_RECHECK`] and only when a footer read answers, so a label that
//!   moved is on the glass one memo period after the next read. Nothing is
//!   shown rather than guessed: an environment that was not read or reads
//!   empty, a roster that is missing, refused or lists fewer than two, a
//!   login no row names, a home of its own, a credential in the environment.
//!   THE LIMITS: the resolver sees the environment's half only
//!   (`accounts::ProcessLogin::of_env`), so a Claude Code that a settings
//!   file or the vendor's profile store gives a credential is still
//!   labelled; and one started under another `HOME` is named by the row with
//!   no `dir` (`accounts::judged_label`).
//! * A CLOSED TAB's fold goes when the tab does ([`stop_session`], from the
//!   status observer's own retirement), not at the end of its watch; the
//!   process's tail stays while the process lives ([`ProcessTails`]).
//! * THE RULE. [`App::splice_claude_footer`] runs with the chrome splices, per
//!   visible pane and inside that pane's own columns — a split's sibling on
//!   the same window row is untouched. It finds the composer's bottom rule by
//!   STRUCTURE (`aterm_phase::phase::composer_bottom`), so the facts show in
//!   every mode, default mode (no pill) included, and nothing is painted
//!   while a dialog or a picker has replaced the composer. It covers ONLY
//!   cells that are plain rule glyphs in the rule's own colours
//!   (`footer::rule_run`); any other ink there is Claude's and stays. Claude's own
//!   footer row under the rule — its mode pill, `(shift+tab to cycle)`, `←
//!   for agents`, `esc to interrupt`, its right-hand notices — is NEVER
//!   written, colours included. The facts are drawn QUIET (owner,
//!   2026-09-30): `Script::Sub` — smaller and set low in the rule's cells,
//!   below the vivid rail of the cursor trail under the composer row above
//!   (`aterm_render::SUB_CEILING`) — and EASY TO READ (owner, 2026-10-08):
//!   bold, values in the rule's gray lifted toward the pane's ink, and SET
//!   as a line of type (`footer::set_facts`, owner 2026-10-14: the
//!   spacing): letters and words `footer::FACT_PITCH` apart, the facts an
//!   even `footer::GAP` apart, the rule running on up to them
//!   (`compose_rule`). `[claude_footer] usage = false` hides `Σ` (shown by
//!   default), applied here and in the repaint key. The Claude lights
//!   (`crate::claude_lights`) go in the same rule, left of the facts, rule
//!   glyphs between.
//! * WHAT IS NOT TOUCHED. The terminal GRID. `text`/`screen` read the engine
//!   and keep Claude Code's real row, because aterm's own supervisor reads it
//!   (`aterm_phase` busy detection keys on `esc to interrupt` there). `image`
//!   shows the glass, footer included — the capture paths run this splice too.
//! * THE REDRAW PATH STAYS ON. A host write into a window's scratch bumps its
//!   `snapshot_seq`, and every frame-reuse lane reads that bump as "a host
//!   wrote here, refill it all": the damage-scoped refill (DMG-1), the
//!   effect-only reuse that skips extraction, the strip un-splice (D-2) and
//!   the split compositor's retention (K2). A footer that stayed on screen
//!   would therefore switch all four off for as long as Claude Code is in
//!   front — which is always. So the write is UNDOABLE: the splice records
//!   exactly what it overwrote ([`FooterUndo`]), and the next frame calls
//!   [`undo`] before any of those lanes looks, restoring the cells and the
//!   token. And where the scratch was an untouched engine fill, the painted
//!   row's D-2 revision is set to the no-stamp sentinel and the splice token
//!   re-armed, so the renderer's per-row stamp compare stays on too, with
//!   the rule's row alone compared by content.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use aterm_agent::harness::accounts;
use aterm_agent::harness::footer::{self, FooterFacts, LaunchFacts};
use aterm_core::render::RenderInput;
use aterm_core::terminal::RenderCell;
use winit::event_loop::EventLoopProxy;

use crate::session_timeline::SessionTimeline;
use crate::{App, VisibleContentRoute, Wake, WindowId};

/// How often a Claude Code session's facts are re-read while it is in front.
/// Model and effort move per turn, the branch on a checkout: two seconds is
/// well inside a turn and costs one small file, one tail and one `HEAD`.
pub(crate) const RECHECK: Duration = Duration::from_secs(2);

/// One pane's painted light block, held until the write says whether its row
/// reached the scratch (only a light the glass shows gets a hit).
struct Lit {
    session: u64,
    frame_row: usize,
    /// The frame column the rule's edit starts at — its key in the write.
    col: usize,
    term_row: usize,
    /// The frame column the LIGHTS (not the title) start at.
    lights_col: usize,
    block: crate::claude_lights::Block,
}

/// One Claude Code pane's composer rule, gathered under the window borrow
/// and painted after it (the lights need `&mut App`).
struct FooterPane {
    session: u64,
    frame_row: usize,
    /// The same row as a window grid row.
    term_row: usize,
    col_off: usize,
    /// The pane columns aterm may write into (`footer::rule_run`).
    run: std::ops::Range<usize>,
    /// One of the rule's own glyph cells: what the separator between the
    /// lights and the facts is drawn with.
    rule: RenderCell,
    /// What the footer shows (the facts, filled from the kept launch card).
    facts: FooterFacts,
    blank: RenderCell,
}

/// The screen captured for a visible Claude pane, retained until the lights
/// have observed this frame and the cache has dropped panes that left it.
struct SeenPaneText {
    leaf_index: usize,
    session: u64,
    texts: Arc<Vec<String>>,
}

/// Screen text is an input to the footer and lights readers, not a separate
/// reading of the terminal. The scratch's revision already says whether its
/// cells changed; retain the converted text while that exact pane and revision
/// stay in front. In particular, [`undo`] restores the pre-footer revision on
/// an unchanged frame, so effect-only paints do not walk and allocate every
/// cell of a long Claude transcript again.
#[derive(Debug, Default)]
pub(crate) struct PaneTextCache {
    panes: Vec<Option<CachedPaneText>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PaneTextKey {
    scratch: Tokens,
    session: u64,
    region: PaneTextRegion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PaneTextRegion {
    /// Rows of window chrome before the terminal cells, as the caller's
    /// `frame_rows - ws.rows` calculates it (not `scratch.rows`, which may
    /// include a host prepend).
    strip: usize,
    row_off: usize,
    col_off: usize,
    rows: usize,
    cols: usize,
}

#[derive(Debug)]
struct CachedPaneText {
    key: PaneTextKey,
    rows: Arc<Vec<String>>,
}

impl PaneTextCache {
    /// A removed leaf must not retain a transcript (or grow this cache over a
    /// long sequence of tab layouts).
    fn trim(&mut self, leaves: usize) {
        self.panes.truncate(leaves);
    }

    /// A pane that no longer shows a Claude footer must not keep its old
    /// transcript resident just because another pane still has one.
    fn retain_active(&mut self, active: impl Fn(usize) -> bool) {
        for (index, cached) in self.panes.iter_mut().enumerate() {
            if !active(index) {
                *cached = None;
            }
        }
    }

    fn rows(
        &mut self,
        leaf_index: usize,
        session: u64,
        scratch: &RenderInput,
        region: PaneTextRegion,
    ) -> Arc<Vec<String>> {
        let key = PaneTextKey {
            scratch: Tokens::of(scratch),
            session,
            region,
        };
        if let Some(Some(cached)) = self.panes.get(leaf_index)
            && cached.key == key
        {
            return Arc::clone(&cached.rows);
        }
        // A typed echo changes the scratch revision even when nearly every row
        // keeps the same shape. Once the last frame's readers have gone, reuse
        // their row buffers instead of allocating one String per row again.
        // A still-held Arc remains an immutable snapshot; a different session
        // or pane region also gets fresh storage rather than retaining its old
        // transcript in the new pane's spare String capacities.
        let mut rows = self
            .panes
            .get_mut(leaf_index)
            .and_then(Option::take)
            .filter(|cached| cached.key.session == session && cached.key.region == region)
            .and_then(|cached| Arc::try_unwrap(cached.rows).ok())
            .unwrap_or_default();
        rows.resize_with(region.rows, String::new);
        for (r, text) in rows.iter_mut().enumerate() {
            text.clear();
            if let Some(row) = scratch.cells.get(region.strip + region.row_off + r) {
                text.extend(
                    row.iter()
                        .skip(region.col_off)
                        .take(region.cols)
                        .map(|c| c.ch),
                );
            }
        }
        let text = Arc::new(rows);
        if self.panes.len() <= leaf_index {
            self.panes.resize_with(leaf_index + 1, || None);
        }
        self.panes[leaf_index] = Some(CachedPaneText {
            key,
            rows: Arc::clone(&text),
        });
        text
    }
}

/// The loop proxy [`post_changed`] wakes through. Installed once at launch; a
/// headless test App installs nothing and a post is then a no-op.
static PROXY: OnceLock<EventLoopProxy<Wake>> = OnceLock::new();

/// Install the proxy the footer wake rides. First install wins.
pub(crate) fn install_proxy(proxy: EventLoopProxy<Wake>) {
    let _ = PROXY.set(proxy);
}

/// A session's footer facts moved: repaint the windows that show it.
pub(crate) fn post_changed(session: u64) {
    if let Some(proxy) = PROXY.get() {
        let _ = proxy.send_event(Wake::ClaudeFooter { session });
    }
}

/// The follow-up reads after every request, measured from it. Two cases need
/// them, and both were seen on a dev build (2026-09-24): Claude Code writes
/// `sessions/<pid>.json` a moment AFTER it takes the foreground, so the read
/// that fires on first sight of `claude` found nothing; and a turn's final
/// transcript row lands as the screen settles, after the last sweep that asked.
/// Bounded — a request buys these two reads and no more.
const FOLLOW_UPS: [Duration; 2] = [Duration::from_secs(2), Duration::from_secs(6)];

/// How often a Claude Code session's facts are re-read while nothing asks
/// for them — the status sweep asks only while the screen moves, so a branch
/// switched outside aterm, or the answer a turn writes after the screen has
/// settled, would stand on an idle footer — and for how long after the last
/// ask that goes on.
const IDLE_RECHECK: Duration = Duration::from_secs(15);
const WATCH_FOR: Duration = Duration::from_secs(10 * 60);

/// How often a session is re-read past [`WATCH_FOR`] while its footer shows
/// a limit wall and its Claude Code process lives: a wall whose reset passes
/// leaves the glass within this.
const WALL_RECHECK: Duration = Duration::from_secs(60);

/// How long the resolver's roster answers from its last read ([`RosterMemo`]).
const ROSTER_RECHECK: Duration = Duration::from_secs(10);

/// The re-read interval of a session last asked for at `asked`: the idle
/// clock within its watch, the wall clock past it.
fn recheck_after(now: std::time::Instant, asked: std::time::Instant) -> Duration {
    if now.saturating_duration_since(asked) < WATCH_FOR {
        IDLE_RECHECK
    } else {
        WALL_RECHECK
    }
}

/// Whether a session last asked for at `asked` is still watched: within
/// [`WATCH_FOR`] of the ask, or past it while its footer shows a limit wall
/// and its process is ALIVE (`walled_alive`, asked only then — liveness, not
/// a start time, so it holds on Linux too, and a sessions file a crash left
/// behind cannot keep the clock running for days).
fn still_watched(
    now: std::time::Instant,
    asked: std::time::Instant,
    walled_alive: impl FnOnce() -> bool,
) -> bool {
    now.saturating_duration_since(asked) < WATCH_FOR || walled_alive()
}

/// One resolution: the session, its timeline and the Claude Code process.
#[derive(Clone)]
struct Job {
    session: u64,
    timeline: Arc<Mutex<SessionTimeline>>,
    pgid: i32,
}

enum Command {
    Request(Job),
    /// `None` retires the session; a group-specific stop cannot erase a
    /// replacement Claude process that was requested before it arrived.
    Stop {
        session: u64,
        pgid: Option<i32>,
    },
}

/// The one background thread footer facts are read on, started on first use
/// and shared by both askers (the status sweep, and `session_program`'s
/// resolver the moment it names Claude Code). An answer for a group that has
/// left the foreground is dropped by `SessionTimeline::set_claude_footer`.
static RESOLVER: Mutex<Option<Sender<Command>>> = Mutex::new(None);

/// Read `pgid`'s footer facts off-thread into `timeline` now, and again at
/// each of [`FOLLOW_UPS`]. A newer request for the same session replaces the
/// follow-ups still owed for it. If the thread cannot be started the footer
/// simply does not appear.
pub(crate) fn request_footer(session: u64, timeline: &Arc<Mutex<SessionTimeline>>, pgid: i32) {
    if pgid <= 0 {
        return;
    }
    let mut slot = RESOLVER.lock().unwrap_or_else(|p| p.into_inner());
    if slot.is_none() {
        let (tx, rx) = channel::<Command>();
        let spawned = std::thread::Builder::new()
            .name("aterm-claude-footer".into())
            .spawn(move || {
                // It takes the session timeline lock the UI thread contends,
                // so the lock-holder floor applies.
                crate::qos::set_self(crate::qos::Role::Responsive);
                run(&rx);
            });
        match spawned {
            Ok(_) => *slot = Some(tx),
            Err(e) => {
                aterm_log::warn!("claude footer: could not start the resolver: {e}");
                return;
            }
        }
    }
    let job = Job {
        session,
        timeline: Arc::clone(timeline),
        pgid,
    };
    if slot
        .as_ref()
        .is_some_and(|tx| tx.send(Command::Request(job)).is_err())
    {
        // The thread is gone (it ends only when the channel closes, so this is
        // a panic in a read); start a fresh one next time.
        *slot = None;
    }
}

/// Stop refreshing the old foreground group. The sender lock serializes this
/// with requests; the resolver also matches `pgid`, so a late stop for an old
/// group cannot cancel a newer group's watch. It releases the session's usage
/// fold and wall mark with the watch: a Claude Code suspended and brought back
/// folds its transcript again from the start.
pub(crate) fn stop(session: u64, pgid: i32) {
    if pgid <= 0 {
        return;
    }
    send_stop(session, Some(pgid));
}

/// A retired session or a disabled status sweep has no live footer watch.
/// It drops everything the resolver holds for the session — its watch, the
/// follow-ups owed, its usage fold and its limit-wall mark — so a closed
/// tab's fold goes with the tab, not at the end of its watch.
pub(crate) fn stop_session(session: u64) {
    #[cfg(test)]
    STOPPED_SESSIONS.with(|s| s.borrow_mut().push(session));
    send_stop(session, None);
}

#[cfg(test)]
thread_local! {
    /// The sessions [`stop_session`] was called for on this thread (the
    /// status observer's retirement test reads it).
    pub(crate) static STOPPED_SESSIONS: std::cell::RefCell<Vec<u64>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn send_stop(session: u64, pgid: Option<i32>) {
    let mut slot = RESOLVER.lock().unwrap_or_else(|p| p.into_inner());
    if slot
        .as_ref()
        .is_some_and(|tx| tx.send(Command::Stop { session, pgid }).is_err())
    {
        *slot = None;
    }
}

#[derive(Default)]
struct WatchSchedule {
    owed: Vec<(std::time::Instant, Job)>,
    watched: HashMap<u64, (Job, std::time::Instant, std::time::Instant)>,
}

impl WatchSchedule {
    /// Drop the watches whose time is up: [`WATCH_FOR`] after the last ask,
    /// unless `walled_alive` — asked only then — says the session's footer
    /// shows a limit wall and its process lives ([`still_watched`]).
    fn prune(&mut self, now: std::time::Instant, mut walled_alive: impl FnMut(u64, &Job) -> bool) {
        self.watched.retain(|session, (job, asked, _)| {
            still_watched(now, *asked, || walled_alive(*session, job))
        });
    }

    fn next(&self, now: std::time::Instant) -> Option<std::time::Instant> {
        self.owed
            .iter()
            .map(|(at, _)| *at)
            .chain(
                self.watched
                    .values()
                    .map(|(_, asked, read)| *read + recheck_after(now, *asked)),
            )
            .min()
    }

    fn matches(job: &Job, session: u64, pgid: Option<i32>) -> bool {
        job.session == session && pgid.is_none_or(|pgid| job.pgid == pgid)
    }

    /// Collapse queued requests while respecting a stop's position in the
    /// channel. Returns the jobs to read now and the watched ids it retired.
    fn accept(&mut self, commands: impl IntoIterator<Item = Command>) -> (Vec<Job>, Vec<u64>) {
        let mut latest: HashMap<u64, Job> = HashMap::new();
        let mut stopped = Vec::new();
        for command in commands {
            match command {
                Command::Request(job) => {
                    latest.insert(job.session, job);
                }
                Command::Stop { session, pgid } => {
                    if latest
                        .get(&session)
                        .is_some_and(|job| Self::matches(job, session, pgid))
                    {
                        latest.remove(&session);
                    }
                    if self.stop(session, pgid) {
                        stopped.push(session);
                    }
                }
            }
        }
        (latest.into_values().collect(), stopped)
    }

    fn stop(&mut self, session: u64, pgid: Option<i32>) -> bool {
        if !self
            .watched
            .get(&session)
            .is_some_and(|(job, _, _)| Self::matches(job, session, pgid))
        {
            return false;
        }
        self.watched.remove(&session);
        self.owed.retain(|(_, job)| job.session != session);
        true
    }

    fn watch(&mut self, job: Job, asked: std::time::Instant) {
        self.owed
            .retain(|(_, pending)| pending.session != job.session);
        for delay in FOLLOW_UPS {
            self.owed.push((asked + delay, job.clone()));
        }
        self.watched.insert(job.session, (job, asked, asked));
    }

    fn due_followups(&mut self, now: std::time::Instant) -> Vec<Job> {
        let (due, later): (Vec<_>, Vec<_>) = self.owed.drain(..).partition(|(at, _)| *at <= now);
        self.owed = later;
        due.into_iter().map(|(_, job)| job).collect()
    }

    fn due_idle(&self, now: std::time::Instant) -> Option<Job> {
        self.watched
            .values()
            .find(|(_, asked, read)| now >= *read + recheck_after(now, *asked))
            .map(|(job, _, _)| job.clone())
    }

    fn read(&mut self, session: u64, at: std::time::Instant) {
        if let Some((_, _, read)) = self.watched.get_mut(&session) {
            *read = at;
        }
    }
}

/// How many Claude Code processes the resolver keeps transcript caches for
/// at once ([`ProcessTails`]).
const MAX_KEPT: usize = 64;

/// How often the resolver asks the kernel whether the processes it keeps
/// caches for, and no longer watches, still live ([`ProcessTails`]).
const SWEEP_EVERY: Duration = Duration::from_secs(30);

/// The transcript caches of the Claude Code processes the resolver has read
/// — and in each, what that process decided, when it was last seen in which
/// session, and whether its model is PINNED (`footer::TailCache`). They are
/// kept per PROCESS — its group and its kernel start — not per watch: a
/// watch lapses after [`WATCH_FOR`] with no ask and ends at every stop (a
/// ctrl-z, another program in front), while the process and its pin
/// outlive both, and a cache dropped then met the process again at first
/// sight, which cannot see a pin made in a session it has since left (a
/// `/resume` then named the resumed conversation's model where Claude kept
/// the pinned one). So a cache stays until its process is GONE — the kernel
/// has no process of that group born then — or, for a process whose start
/// could not be read (nothing proves a later one the same), until its
/// session's watch no longer reads that group; and never more than
/// [`MAX_KEPT`], the least recently read going first. A new aterm process
/// (an update handoff) starts with none.
#[derive(Default)]
struct ProcessTails {
    kept: HashMap<(i32, Option<u64>), KeptTail>,
    /// When the kernel was last asked ([`SWEEP_EVERY`]).
    swept: Option<std::time::Instant>,
}

/// One process's cache in [`ProcessTails`].
struct KeptTail {
    /// The session the process was last read for.
    session: u64,
    cache: footer::TailCache,
    /// When it was last read.
    used: std::time::Instant,
}

impl ProcessTails {
    /// The cache of the process (`pgid`, `started`), read now for `session`.
    fn cache(
        &mut self,
        session: u64,
        pgid: i32,
        started: Option<u64>,
        now: std::time::Instant,
    ) -> &mut footer::TailCache {
        let key = (pgid, started);
        if !self.kept.contains_key(&key)
            && self.kept.len() >= MAX_KEPT
            && let Some(oldest) = self
                .kept
                .iter()
                .min_by_key(|(_, kept)| kept.used)
                .map(|(key, _)| *key)
        {
            self.kept.remove(&oldest);
        }
        let kept = self.kept.entry(key).or_insert_with(|| KeptTail {
            session,
            cache: footer::TailCache::default(),
            used: now,
        });
        kept.session = session;
        kept.used = now;
        &mut kept.cache
    }

    /// Drop the caches whose process has left for good, as `schedule` now
    /// watches: one with no start time once its session's watch no longer
    /// reads its group; one with a start, at most every [`SWEEP_EVERY`],
    /// once `alive` (the kernel, [`birth_seconds`]) no longer finds a process
    /// of its group born then. A lapsed or stopped watch alone drops none.
    fn retain(
        &mut self,
        schedule: &WatchSchedule,
        alive: impl Fn(i32, u64) -> bool,
        now: std::time::Instant,
    ) {
        let sweep = self
            .swept
            .is_none_or(|at| now.saturating_duration_since(at) >= SWEEP_EVERY);
        if sweep {
            self.swept = Some(now);
        }
        self.kept.retain(|&(pgid, started), kept| {
            let watched = schedule
                .watched
                .get(&kept.session)
                .is_some_and(|(job, _, _)| job.pgid == pgid);
            watched || started.is_some_and(|started| !sweep || alive(pgid, started))
        });
    }
}

/// The resolver thread: each request is read at once and again at each
/// follow-up, and the thread sleeps exactly until the next one is owed.
fn run(rx: &std::sync::mpsc::Receiver<Command>) {
    let mut schedule = WatchSchedule::default();
    let mut known: HashMap<u64, Identity> = HashMap::new();
    // Per PROCESS: its transcript-tail memo, kept while the process lives.
    let mut tails = ProcessTails::default();
    // Per watched session: its usage fold, released with the watch.
    let mut folds: HashMap<u64, footer::FooterCache> = HashMap::new();
    // Sessions whose published footer shows a limit wall.
    let mut walled: HashSet<u64> = HashSet::new();
    // The accounts roster, for every session's label.
    let mut roster = RosterMemo::default();
    // Per session: its latest job, when it was last ASKED for, and when it
    // was last READ — the idle re-reads ([`IDLE_RECHECK`]) run off this.
    loop {
        let now = std::time::Instant::now();
        schedule.prune(now, |session, job| {
            walled.contains(&session)
                && u32::try_from(job.pgid).is_ok_and(crate::control_auth::pid_alive)
        });
        known.retain(|session, _| schedule.watched.contains_key(session));
        folds.retain(|session, _| schedule.watched.contains_key(session));
        walled.retain(|session| schedule.watched.contains_key(session));
        tails.retain(
            &schedule,
            |pgid, started| birth_seconds(pgid) == Some(started),
            now,
        );
        let first = match schedule.next(now) {
            Some(at) => match rx.recv_timeout(at.saturating_duration_since(now)) {
                Ok(job) => Some(job),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            },
            None => match rx.recv() {
                Ok(job) => Some(job),
                Err(_) => return,
            },
        };
        // Everything already queued, collapsed to the newest ask per session:
        // a slow read (a stalled mount under one session's cwd) is paid once,
        // not once per ask that piled up behind it. A stop in the batch wins
        // over an ask for the same group queued before it.
        let (latest, stopped) = schedule.accept(
            first
                .into_iter()
                .chain(std::iter::from_fn(|| rx.try_recv().ok())),
        );
        for session in stopped {
            forget(session, &mut known, &mut folds, &mut walled);
            // The process's tail stays while the process lives: a stopped
            // group may come back (`fg`), its pin with it ([`ProcessTails`]).
        }
        let mut reads = Reads {
            known: &mut known,
            tails: &mut tails,
            folds: &mut folds,
            walled: &mut walled,
            roster: &mut roster,
        };
        for job in latest {
            reads.resolve(&job);
            let asked = std::time::Instant::now();
            schedule.watch(job, asked);
        }
        let now = std::time::Instant::now();
        for job in schedule.due_followups(now) {
            reads.resolve(&job);
            schedule.read(job.session, std::time::Instant::now());
        }
        while let Some(job) = schedule.due_idle(now) {
            reads.resolve(&job);
            schedule.read(job.session, std::time::Instant::now());
        }
    }
}

/// The resolver's state one read touches: per session, the process known
/// for it, its usage fold and its wall mark; per process, its tail; and the
/// one roster.
struct Reads<'a> {
    known: &'a mut HashMap<u64, Identity>,
    tails: &'a mut ProcessTails,
    folds: &'a mut HashMap<u64, footer::FooterCache>,
    walled: &'a mut HashSet<u64>,
    roster: &'a mut RosterMemo,
}

impl Reads<'_> {
    /// Read `job`'s session and keep it in the walled set exactly while its
    /// published footer shows a limit wall.
    fn resolve(&mut self, job: &Job) {
        let shows_wall = resolve_and_post(
            self.known,
            self.tails,
            self.folds.entry(job.session).or_default(),
            self.roster,
            job.session,
            &job.timeline,
            job.pgid,
        );
        note_wall(self.walled, job.session, shows_wall);
    }
}

/// Keep `session` in `walled` exactly while its footer shows a limit wall.
fn note_wall(walled: &mut HashSet<u64>, session: u64, shows_wall: bool) {
    if shows_wall {
        walled.insert(session);
    } else {
        walled.remove(&session);
    }
}

/// Forget what the resolver's reads hold for `session` — its process, its
/// usage fold and its wall mark — once the schedule has stopped its watch
/// ([`stop_session`] from a closed tab, [`stop`] for a group that left). The
/// PROCESS's tail is not the session's: [`ProcessTails`] keeps it while the
/// process lives.
fn forget(
    session: u64,
    known: &mut HashMap<u64, Identity>,
    folds: &mut HashMap<u64, footer::FooterCache>,
    walled: &mut HashSet<u64>,
) {
    known.remove(&session);
    folds.remove(&session);
    walled.remove(&session);
}

/// What the resolver knows about one Claude Code process for its whole
/// life: when the kernel started it (the pid-reuse guard), the Claude Code
/// directory its environment named at exec, and what its own command line
/// and environment say ([`footer::launch_facts_of`], [`login_of_args`]). The
/// directory costs one `KERN_PROCARGS2` read, so it is read once per process
/// — per session, and again only when that session's foreground process is a
/// different one (a new pid, or the same pid born again: a relaunch onto a
/// newer build). The command line and the login are read once the registry
/// names an IMAGE (`startedAt`), and again whenever it names a new one: an
/// in-place exec keeps the pid and the kernel's start but not the argv or
/// the environment, so what was read before the row was written may be a
/// launcher's (a shim named `claude` that sets variables and execs).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Identity {
    pid: i32,
    started: Option<u64>,
    dir: Option<PathBuf>,
    launch: LaunchFacts,
    /// The image (`startedAt`) `launch`, `login` and `argv` were read for;
    /// `None` until a registry entry named one and that read answered.
    launch_for: Option<Option<u64>>,
    /// What the process's exec environment says of its login; `None` where
    /// it could not be read or reads empty. Read with `launch`.
    login: Option<accounts::ProcessLogin>,
    /// The process's argv, `argv[0]` first, from the same `KERN_PROCARGS2`
    /// read as `launch` (empty where it could not be read): the launch flags
    /// the frozen-program remedy's resume line carries
    /// ([`aterm_agent::harness::resume::of_entry`], 2026-09-26).
    argv: Vec<String>,
}

/// The kernel's start time of `pid`, unix seconds, where this platform can
/// read it for a process of ours.
fn birth_seconds(pid: i32) -> Option<u64> {
    #[cfg(unix)]
    {
        crate::seamless::read_process_birth(pid).map(|birth| birth.seconds)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        None
    }
}

/// `pid`'s argv and environment at exec, when this platform lets it be read.
fn process_args(pid: i32) -> Option<atpkg::caller_shell::ProcArgs> {
    u32::try_from(pid)
        .ok()
        .and_then(atpkg::caller_shell::process_args)
}

/// `pid`'s Claude Code directory, from its own environment at exec (`args`)
/// and, for a relative `CLAUDE_CONFIG_DIR` (empty included), the directory
/// it runs in, by the one resolver ([`footer::claude_dir_of`]); aterm's home
/// when that environment cannot be read.
pub(crate) fn claude_dir_of_args(
    args: Option<&atpkg::caller_shell::ProcArgs>,
    pid: i32,
) -> Option<PathBuf> {
    match args {
        Some(args) => footer::claude_dir_of(
            args.env_var("CLAUDE_CONFIG_DIR"),
            args.env_var("HOME"),
            || {
                u32::try_from(pid)
                    .ok()
                    .and_then(aterm_sysprobe::process_cwd)
            },
        ),
        None => aterm_types::dirs::home_dir().map(|home| home.join(".claude")),
    }
}

/// What a process's environment at exec says of its login
/// ([`accounts::ProcessLogin::of_exec_env`]). `None` where it was not read,
/// and where it reads empty.
fn login_of_args(args: Option<&atpkg::caller_shell::ProcArgs>) -> Option<accounts::ProcessLogin> {
    accounts::ProcessLogin::of_exec_env(&args?.env)
}

/// The accounts roster as the resolver last read it, for the footer's label
/// ([`accounts::footer_label`]). The resolver thread's own: no lock, and a
/// new aterm process starts with none.
#[derive(Default)]
struct RosterMemo {
    /// When `load` was last asked.
    read: Option<std::time::Instant>,
    /// What it answered, where that lists two or more.
    roster: Option<accounts::Roster>,
}

impl RosterMemo {
    /// The roster, where it lists two or more; `load` is asked at most every
    /// [`ROSTER_RECHECK`]. A load that fails is no roster until the next.
    fn at(
        &mut self,
        now: std::time::Instant,
        load: impl FnOnce() -> Option<accounts::Roster>,
    ) -> Option<&accounts::Roster> {
        if self
            .read
            .is_none_or(|at| now.saturating_duration_since(at) >= ROSTER_RECHECK)
        {
            self.read = Some(now);
            self.roster = load().filter(accounts::Roster::several);
        }
        self.roster.as_ref()
    }
}

/// The roster beside the `aterm.toml` this window loads
/// ([`accounts::default_roster_path`]): one bounded read. `None` where there
/// is no config path and where the file is refused; a missing file is the
/// empty roster. Under test it reads nothing: a test hands [`RosterMemo`]
/// its roster.
fn live_roster() -> Option<accounts::Roster> {
    if cfg!(test) {
        return None;
    }
    accounts::Roster::load(&accounts::default_roster_path()?).ok()
}

/// What a read asks outside Claude Code's own files: a process's exec read
/// and the accounts roster. The resolver asks the machine ([`LIVE`]); a test
/// answers for it.
#[derive(Clone, Copy)]
struct Asks {
    args: fn(i32) -> Option<atpkg::caller_shell::ProcArgs>,
    roster: fn() -> Option<accounts::Roster>,
}

/// [`process_args`] and [`live_roster`].
const LIVE: Asks = Asks {
    args: process_args,
    roster: live_roster,
};

/// [`resolve_into`], then wake the loop when the facts moved — and, when the
/// moved facts carry a repository read the kernel refused with `EPERM`, post that
/// too, so the App can raise its consent attention for this session
/// (`Wake::ProtectedRead`; the App decides whether the path is protected).
/// Whether the session's published footer now shows a limit wall.
fn resolve_and_post(
    known: &mut HashMap<u64, Identity>,
    tails: &mut ProcessTails,
    fold: &mut footer::FooterCache,
    roster: &mut RosterMemo,
    session: u64,
    timeline: &Arc<Mutex<SessionTimeline>>,
    pgid: i32,
) -> bool {
    let started = birth_seconds(pgid);
    let cached = known
        .get(&session)
        .is_some_and(|id| id.pid == pgid && id.started.is_some() && id.started == started)
        .then(|| known.remove(&session))
        .flatten();
    // The directory alone is taken from this read: the rest is read once
    // the registry names the image ([`resolve_into`]).
    let mut identity = cached.unwrap_or_else(|| Identity {
        pid: pgid,
        started,
        dir: claude_dir_of_args(process_args(pgid).as_ref(), pgid),
        launch: LaunchFacts::default(),
        launch_for: None,
        login: None,
        argv: Vec::new(),
    });
    let tail = tails.cache(
        session,
        identity.pid,
        identity.started,
        std::time::Instant::now(),
    );
    let (moved, shows_wall) = resolve_into(timeline, &mut identity, tail, fold, roster, LIVE);
    // No directory is not remembered: a launch through the atpkg twin is
    // `/bin/sh` for its first milliseconds, whose environment macOS hides,
    // and the `exec` into Claude Code keeps both the pid and the start time
    // — a cached `None` would outlive the shell and leave that process with
    // no footer for its whole life.
    if identity.dir.is_some() {
        known.insert(session, identity);
    }
    if let Some(denied) = moved {
        post_changed(session);
        if let Some(path) = denied
            && let Some(proxy) = PROXY.get()
        {
            let _ = proxy.send_event(Wake::ProtectedRead { session, path });
        }
    }
    shows_wall
}

/// A zone's offset from UTC AT the instant `unix` (`None`: the local zone) —
/// `aterm_agent::supervise::limit::offset_at`, the one reader, `None` for a
/// zone this machine does not know (the wall then shows nothing, never a
/// guess) — cached: each is a `date` run, and a standing wall asks about the
/// same few instants on every read. At most 64 answers, each for an hour
/// (the local zone can change under a running window). The resolver
/// thread's only.
fn offset_at_cached(zone: Option<&str>, unix: i64) -> Option<i64> {
    static ANSWERS: Mutex<Vec<OffsetAnswer>> = Mutex::new(Vec::new());
    let mut answers = ANSWERS.lock().unwrap_or_else(|p| p.into_inner());
    cached_offset(
        &mut answers,
        zone,
        unix,
        std::time::Instant::now(),
        aterm_agent::supervise::limit::offset_at,
    )
}

/// One remembered offset: the zone (`None`: local), the instant, when it
/// was read, and the offset.
type OffsetAnswer = (Option<String>, i64, std::time::Instant, i64);

/// [`offset_at_cached`] over `answers` at `now`, asking `read` on a miss.
/// Only an ANSWER is kept: a read that failed (a `date` that could not be
/// spawned under process pressure, say) is asked again next time, rather
/// than hiding a standing wall for the hour an answer is kept.
fn cached_offset(
    answers: &mut Vec<OffsetAnswer>,
    zone: Option<&str>,
    unix: i64,
    now: std::time::Instant,
    read: impl FnOnce(Option<&str>, i64) -> Option<i64>,
) -> Option<i64> {
    const FRESH: Duration = Duration::from_secs(3600);
    const ANSWERS_MAX: usize = 64;
    if let Some(hit) = answers
        .iter()
        .find(|(z, t, at, _)| {
            z.as_deref() == zone && *t == unix && now.saturating_duration_since(*at) < FRESH
        })
        .map(|(_, _, _, offset)| *offset)
    {
        return Some(hit);
    }
    let offset = read(zone, unix)?;
    answers.retain(|(z, t, _, _)| !(z.as_deref() == zone && *t == unix));
    if answers.len() >= ANSWERS_MAX {
        answers.remove(0);
    }
    answers.push((zone.map(str::to_owned), unix, now, offset));
    Some(offset)
}

/// Read `identity`'s facts and publish them on `timeline`: `Some` when they
/// MOVED, carrying the working directory whose repository read was refused, if
/// one was; and whether the footer the timeline now shows carries a limit
/// wall. The file reads run before the (leaf) timeline lock is taken. A
/// registry entry that names an image `identity` was not read for reads the
/// command line and the login into it first (`asks`). The facts are read
/// over the process's `tail`, the session's usage folded on in `fold`; a
/// read that answers is given its account from `roster`
/// ([`accounts::footer_label`]), which is asked only then, and only for a
/// process whose login was read.
fn resolve_into(
    timeline: &Arc<Mutex<SessionTimeline>>,
    identity: &mut Identity,
    tail: &mut footer::TailCache,
    fold: &mut footer::FooterCache,
    roster: &mut RosterMemo,
    asks: Asks,
) -> (Option<Option<PathBuf>>, bool) {
    // ONE footer read (`footer::read_pid`): one read of `sessions/<pid>.json`
    // feeds the model, the Σ, the wall and the frozen-program remedy's resume
    // line alike, so after `/clear` or `/resume` the footer never pairs one
    // conversation's model with another's Σ or wall, nor names another's
    // conversation to resume.
    // `read_at` is no later than that registry read: when this read saw the
    // process in the session the registry names.
    let clock = footer::ReadClock {
        read_at: std::time::SystemTime::now(),
        now: aterm_agent::supervise::limit::unix_now(),
        offset_at: &offset_at_cached,
    };
    let started = identity.started;
    let mut facts = u32::try_from(identity.pid)
        .ok()
        .zip(identity.dir.clone())
        .and_then(|(pid, dir)| {
            // The command line and the login are the image's the entry
            // names: read at its first row, and again when a row names a
            // new one (an in-place exec keeps the pid and the kernel's
            // start). A read that is refused says nothing of the image —
            // never the last one's facts — and is asked again at the next.
            let launch = |entry: &footer::SessionEntry| {
                if identity.launch_for != Some(entry.started_at) {
                    let args = (asks.args)(identity.pid);
                    identity.launch = args
                        .as_ref()
                        .map(|a| footer::launch_facts_of(&a.argv, &a.env))
                        .unwrap_or_default();
                    identity.login = login_of_args(args.as_ref());
                    if args.is_some() {
                        identity.launch_for = Some(entry.started_at);
                    }
                    identity.argv = args.map(|a| a.argv).unwrap_or_default();
                }
                (identity.launch.clone(), identity.argv.clone())
            };
            footer::read_pid(
                &dir,
                pid,
                started,
                launch,
                tail,
                fold,
                &clock,
                crate::harness_host::harness_state().as_deref(),
            )
        });
    // The account is laid in here: only this thread holds the process's
    // login and the roster (`FooterFacts::account`).
    if let (Some(facts), Some(who)) = (facts.as_mut(), identity.login.as_ref()) {
        facts.account = roster
            .at(std::time::Instant::now(), asks.roster)
            .and_then(|listed| accounts::footer_label(listed, who))
            .map(str::to_owned);
    }
    let denied = facts.as_ref().and_then(|f| f.repo_read_denied.clone());
    let mut timeline = timeline.lock().unwrap_or_else(|p| p.into_inner());
    // One unreadable read of the SAME process — Claude rewrites its sessions
    // file on every status change, and a read can land mid-write — keeps the
    // last good facts rather than blanking the footer (and the lights) until
    // the next read. The same PROCESS: its group and its kernel start time
    // (`SessionTimeline::claude_footer_identity`), so a process that reuses
    // the group never inherits another's facts; without a start time there is
    // no such proof and nothing is kept.
    if facts.is_none()
        && identity.started.is_some()
        && timeline.claude_footer_identity() == Some((identity.pid, identity.started))
    {
        return (None, shows_wall(&timeline));
    }
    let moved = timeline
        .set_claude_footer(identity.pid, identity.started, facts)
        .then_some(denied);
    (moved, shows_wall(&timeline))
}

/// Whether the footer `timeline` shows NOW carries a limit wall.
fn shows_wall(timeline: &SessionTimeline) -> bool {
    timeline
        .claude_footer()
        .and_then(|facts| facts.usage.as_ref())
        .is_some_and(|usage| usage.wall.is_some())
}

/// A fingerprint of `facts` AS SHOWN under the `[claude_footer] usage`
/// switch ([`with_usage_switch`]) for the repaint key: `0` when there are
/// none, so a frame with no footer keys exactly as it did before this module
/// existed. Taken without a copy — it runs for every Claude Code pane on
/// every frame — so with the switch off only the limit wall stands for the
/// usage, and hidden tokens that keep growing do not move the key. The
/// destructure names every field: a new one does not compile until it is
/// hashed here.
pub(crate) fn fingerprint(facts: Option<&FooterFacts>, usage: bool) -> u64 {
    use std::hash::{Hash, Hasher};
    let Some(facts) = facts else {
        return 0;
    };
    let FooterFacts {
        model,
        effort,
        path,
        branch,
        repo_read_denied,
        version,
        resume,
        usage: usage_facts,
        model_open,
        effort_open,
        effort_for,
        owner,
        upgrade,
        account,
    } = facts;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (
        model,
        effort,
        path,
        branch,
        repo_read_denied,
        version,
        resume,
    )
        .hash(&mut h);
    (model_open, effort_open, effort_for, owner, upgrade, account).hash(&mut h);
    if usage {
        usage_facts.hash(&mut h);
    } else {
        usage_facts
            .as_ref()
            .and_then(|u| u.wall.as_ref())
            .hash(&mut h);
    }
    h.finish() | 1
}

/// `facts` as the rule shows them under the `[claude_footer] usage` switch
/// (`Config::claude_footer_usage`, on by default): off, the `Σ` segment's
/// tokens and runtime are taken out and only a standing limit wall
/// is kept (`UsageFacts::wall_only`). Applied where the pane is gathered and
/// where the repaint key is taken, so hidden tokens that keep growing change
/// neither the glass nor the key.
fn with_usage_switch(facts: &mut FooterFacts, usage: bool) {
    if !usage {
        facts.usage = facts
            .usage
            .take()
            .and_then(aterm_agent::harness::session_usage::UsageFacts::wall_only);
    }
}

/// Which of the composer rule's pane columns aterm may cover: a PLAIN RULE
/// GLYPH — the cell equal, colours and every attribute, to the rule's own
/// `─` (of the row's first and last `─`, the style more of the row is drawn
/// in) — with nothing in the scratch's side channels at its frame column (a
/// cluster, a combining mark, an image, which the undo could not restore).
/// Anything else on the row is Claude's ink, and a selection or a search
/// highlight over the rule reads as ink too: the facts give way to it. Also
/// returns the rule cell.
fn rule_mask(
    scratch: &RenderInput,
    frame_row: usize,
    col_off: usize,
    pane_cols: usize,
) -> Option<(Vec<bool>, RenderCell)> {
    let row = scratch.cells.get(frame_row)?;
    let cells = row.get(col_off..row.len().min(col_off + pane_cols))?;
    let first = *cells.iter().find(|c| c.ch == '\u{2500}')?;
    let last = *cells.iter().rev().find(|c| c.ch == '\u{2500}')?;
    let count = |style: &RenderCell| cells.iter().filter(|c| *c == style).count();
    let rule = if first == last || count(&last) >= count(&first) {
        last
    } else {
        first
    };
    let mask = (0..pane_cols)
        .map(|c| {
            let col = col_off + c;
            cells.get(c) == Some(&rule)
                && !touches(&scratch.clusters, frame_row, &(col..col + 1))
                && !touches(&scratch.combining, frame_row, &(col..col + 1))
                && !touches(&scratch.images, frame_row, &(col..col + 1))
        })
        .collect();
    Some((mask, rule))
}

/// The cells aterm writes into `room` cells of the rule for one pane, and
/// where in them the lights' chips start. The lights — ` <chips>  <title> `
/// — stand at the LEFT end, then the rule's own glyphs (`rule`), then `
/// <facts> ` at the right end; without lights, the facts alone, so a rule at
/// rest is written only where the facts are. A group only when `fit` keeps
/// it. The FACTS are aterm's annotation, set apart from the terminal's text
/// and quieter than it (owner, 2026-09-30: "the typography needs to be
/// distinct from other text and less emphasized" — and lower than the
/// cursor trail that runs under the composer row above: the trail's vivid
/// rail ends above `aterm_render::SUB_CEILING`, only its faint bed lobe at
/// the caret reaching lower), yet easy to read (owner, 2026-10-08: "the
/// kerning is too wide, and it needs a bit more"): drawn `Script::Sub`,
/// smaller and set low in the rule's cells, and bold, so the small strokes
/// keep their ink; set as a line of type (`footer::set_facts`: each glyph
/// in a cell of its own, nudged to its place); values in Claude's own rule
/// gray (`rule.fg`) lifted [`VALUE_LIFT`] toward the pane's ink
/// (`blank.fg`), marks in `mark_fg` mixed halfway toward the rule's gray, on
/// the pane's ground. The light chips stay full-size: they are pointed at
/// and clicked.
/// The chips' place depends on nothing a title or the facts change: a chip
/// stays under the pointer while its title comes and goes.
fn compose_rule(
    fit: &footer::RuleFit,
    block: Option<&crate::claude_lights::Block>,
    rule: RenderCell,
    blank: RenderCell,
    mark_fg: [u8; 3],
    room: usize,
) -> (Vec<RenderCell>, Option<usize>) {
    let mut out: Vec<RenderCell> = Vec::with_capacity(room);
    let mut lights_at = None;
    if let Some(block) = block.filter(|_| fit.chips) {
        let title: &[RenderCell] = match fit.title {
            footer::TitleFit::Full => &block.title,
            footer::TitleFit::Short => &block.short_title,
            footer::TitleFit::None => &[],
        };
        out.push(blank);
        lights_at = Some(out.len());
        out.extend_from_slice(&block.lights);
        // The title's own two cells of gap (it ends with them) go between
        // the chips and its words.
        let gap = title.len().min(2);
        out.extend_from_slice(&title[title.len() - gap..]);
        out.extend_from_slice(&title[..title.len() - gap]);
        out.push(blank);
    }
    let mut facts: Vec<RenderCell> = Vec::new();
    if !fit.segments.is_empty() {
        let quiet = |fg: [u8; 3]| RenderCell {
            fg,
            bold: true,
            script: aterm_core::terminal::Script::Sub { nudge: 0 },
            ..blank
        };
        let mark = quiet(mix(mark_fg, rule.fg, 0.5));
        let value = quiet(mix(rule.fg, blank.fg, VALUE_LIFT));
        // The set line, a blank cell either side: the rule meets it there.
        let set = footer::set_facts(&fit.segments);
        facts = vec![blank; set.cells + 2];
        for glyph in set.glyphs {
            // The wall's `♻` (for "resets") is a mark inside its value.
            let ink = if glyph.mark || glyph.ch == footer::RESET_MARK {
                mark
            } else {
                value
            };
            let cell = RenderCell {
                ch: glyph.ch,
                script: aterm_core::terminal::Script::Sub { nudge: glyph.nudge },
                ..ink
            };
            facts[1 + glyph.cell] = cell;
            if glyph.cols == 2 {
                facts[2 + glyph.cell] = RenderCell {
                    ch: ' ',
                    wide: true,
                    ..cell
                };
            }
        }
    }
    if !out.is_empty() {
        let glyphs = room.saturating_sub(out.len() + facts.len());
        out.extend(std::iter::repeat_n(rule, glyphs));
    }
    out.extend(facts);
    (out, lights_at)
}

// The facts' nudges are counted in the painter's own steps.
const _: () = assert!(footer::NUDGES_PER_CELL == aterm_core::terminal::Script::NUDGES_PER_CELL);

/// How far the values' ink is lifted from Claude's rule gray toward the
/// pane's text colour: a third of the way, so the small, low facts read
/// without effort and still stand below the conversation's text.
const VALUE_LIFT: f32 = 1.0 / 3.0;

/// `a` moved `t` of the way toward `b`, per channel.
fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    std::array::from_fn(|i| {
        (f32::from(a[i]) + (f32::from(b[i]) - f32::from(a[i])) * t).round() as u8
    })
}

/// The scratch facts one footer write is valid against: every token a fill,
/// a prepend, a compose or another host write moves. [`undo`] restores only
/// while all of them still read exactly as the splice left them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Tokens {
    snapshot_seq: u64,
    engine_fill_seq: u64,
    shifted_fill_seq: u64,
    composed_fill_seq: u64,
    terminal_id: u64,
    extract_gen: u64,
    row_rev_lane: u64,
    row_shift: usize,
    rows: usize,
    cols: usize,
    cell_rows: usize,
}

impl Tokens {
    fn of(s: &RenderInput) -> Self {
        Self {
            snapshot_seq: s.snapshot_seq,
            engine_fill_seq: s.engine_fill_seq,
            shifted_fill_seq: s.shifted_fill_seq,
            composed_fill_seq: s.composed_fill_seq,
            terminal_id: s.terminal_id,
            extract_gen: s.extract_gen,
            row_rev_lane: s.row_rev_lane,
            row_shift: s.row_shift,
            rows: s.rows,
            cols: s.cols,
            cell_rows: s.cells.len(),
        }
    }
}

/// One row a footer splice wrote.
#[derive(Debug)]
struct RowUndo {
    frame_row: usize,
    col_off: usize,
    /// The row's length before the write (the engine trims trailing blanks,
    /// so the write may have padded it).
    len_before: usize,
    /// The cells the write replaced — the part of the span the row had.
    before: Vec<RenderCell>,
    /// The cells the write put there: the undo's proof they are still there.
    painted: Vec<RenderCell>,
    /// The row's D-2 revision before the write, when the write cleared it.
    rev_before: Option<u64>,
}

/// What the last footer splice overwrote in one window's scratch, with the
/// proof that nothing has touched the scratch since. Held on the window
/// (`WindowState::claude_footer_undo`) until [`undo`] consumes it.
#[derive(Debug)]
pub(crate) struct FooterUndo {
    after: Tokens,
    seq_before: u64,
    shifted_before: u64,
    rows: Vec<RowUndo>,
}

/// The pane's exact terminal extraction that supplied its cells to this frame.
/// A terminal can mutate after that extraction and before the footer splice;
/// reading it again would both contend with the PTY writer and mix generations.
/// Pure splits use canonical leaf INDEX because two views may mirror one session;
/// mixed tabs use the stable view-keyed committed cache.
fn frame_source<'a>(
    ws: &'a crate::WindowState,
    route: VisibleContentRoute,
    leaf_index: usize,
    leaf: &crate::tab_model::VisibleLeaf,
) -> Option<&'a RenderInput> {
    let source = match route {
        VisibleContentRoute::Terminal { composed: false } => &ws.input_scratch,
        VisibleContentRoute::Terminal { composed: true } if leaf.focused => {
            &ws.composed_focus_scratch
        }
        VisibleContentRoute::Terminal { composed: true } => {
            ws.unfocused_pane_scratch.get(&leaf_index)?
        }
        VisibleContentRoute::Heterogeneous => &ws.leaf_render_cache.get(&leaf.view)?.input,
        VisibleContentRoute::Native { .. } => return None,
    };
    (source.terminal_id != 0).then_some(source)
}

/// Put back what the last footer splice on `scratch` overwrote — only while
/// the scratch is still EXACTLY what that splice left: every [`Tokens`] field
/// equal, and every painted span still holding the painted cells. The cells,
/// the row lengths, the cleared revisions, `snapshot_seq` and
/// `shifted_fill_seq` all return to their values before the write, so the
/// scratch is exactly as reusable as it was before the footer was drawn — an
/// engine fill again, a blessed strip prepend, or a blessed composite.
///
/// Any doubt changes NOTHING: the footer's own `snapshot_seq` bump then
/// stands, and the next fill takes the full arm, as after any host write. The
/// record is consumed either way. Call it at the top of a frame, before the
/// first reader of the reuse tokens; the splice also calls it first, so a
/// frame that presents a scratch without refilling it never paints over its
/// own footer.
pub(crate) fn undo(slot: &mut Option<FooterUndo>, scratch: &mut RenderInput) -> bool {
    let Some(record) = slot.take() else {
        return false;
    };
    if Tokens::of(scratch) != record.after {
        return false;
    }
    let intact = record.rows.iter().all(|r| {
        scratch
            .cells
            .get(r.frame_row)
            .and_then(|row| row.get(r.col_off..r.col_off + r.painted.len()))
            .is_some_and(|span| span == r.painted.as_slice())
    });
    if !intact {
        return false;
    }
    // Newest first: two panes side by side write the same frame row, and the
    // second write's `len_before` is the first write's padded length.
    for r in record.rows.iter().rev() {
        let row = &mut scratch.cells[r.frame_row];
        row[r.col_off..r.col_off + r.before.len()].copy_from_slice(&r.before);
        row.truncate(r.len_before);
        if let (Some(rev), Some(slot)) = (r.rev_before, scratch.row_rev.get_mut(r.frame_row)) {
            *slot = rev;
        }
    }
    scratch.snapshot_seq = record.seq_before;
    scratch.shifted_fill_seq = record.shifted_before;
    true
}

/// Whether a column-keyed side channel holds an entry in `span` of `row`.
fn touches<T>(channel: &[Vec<(usize, T)>], row: usize, span: &std::ops::Range<usize>) -> bool {
    channel
        .get(row)
        .is_some_and(|entries| entries.iter().any(|(col, _)| span.contains(col)))
}

/// Write each `(frame_row, col, cells, ground)` edit into `scratch` and
/// return the record [`undo`] needs, or `None` when nothing was written. A
/// row the engine trimmed short of the edit is padded with `ground` (the
/// pane's blank), never with a painted glyph.
///
/// A span that holds a grapheme cluster, a combining mark or an image cell is
/// skipped — the vendor's row there is not the plain text the planner read,
/// and a write the undo could not restore exactly is a write not made.
///
/// Where the scratch is still exactly an engine fill — possibly shifted down
/// by a BLESSED chrome prepend (the tab strip, a band row), i.e. what
/// `RenderInput::host_prepend_blessing` answers — each written row's D-2
/// revision becomes `0` (the no-stamp sentinel, which the renderer answers by
/// comparing that row's content) and the splice token is re-armed after the
/// bump, so the renderer keeps comparing every other row by stamp. Without a
/// prepend the re-arm is sound with or without the undo: `engine_fill_seq`
/// still differs, so neither the scoped refill, the effect-only reuse nor a
/// prepend's blessing can mistake the scratch for an engine fill. With one, the
/// strip's inverse (`undo_host_row_prepend`) would accept the re-armed token,
/// so it rests on the ORDER at its one call site (`redraw_window_with_layout`):
/// [`undo`] runs immediately before it, and [`undo`] either restores the
/// footer's cells and both tokens or — the scratch having moved since — finds
/// `snapshot_seq` moved too, which the inverse refuses as well.
fn write_edits(
    scratch: &mut RenderInput,
    edits: Vec<(usize, usize, Vec<RenderCell>, RenderCell)>,
) -> Option<FooterUndo> {
    let seq_before = scratch.snapshot_seq;
    let shifted_before = scratch.shifted_fill_seq;
    let restamp = scratch.shifted_fill_seq != 0 && scratch.host_prepend_blessing();
    let mut rows = Vec::with_capacity(edits.len());
    for (frame_row, col_off, painted, ground) in edits {
        let span = col_off..col_off + painted.len();
        if touches(&scratch.clusters, frame_row, &span)
            || touches(&scratch.combining, frame_row, &span)
            || touches(&scratch.images, frame_row, &span)
        {
            continue;
        }
        let Some(cells) = scratch.cells.get_mut(frame_row) else {
            continue;
        };
        let len_before = cells.len();
        let before = cells
            .get(col_off..len_before.min(span.end))
            .map(<[RenderCell]>::to_vec)
            .unwrap_or_default();
        // The engine TRIMS trailing blanks: a short row is padded before it
        // is written into, as `splice_tab_menu` does.
        if cells.len() < span.end {
            cells.resize(span.end, ground);
        }
        cells[span].copy_from_slice(&painted);
        let rev_before = if restamp {
            scratch
                .row_rev
                .get_mut(frame_row)
                .map(|rev| std::mem::replace(rev, 0))
        } else {
            None
        };
        rows.push(RowUndo {
            frame_row,
            col_off,
            len_before,
            before,
            painted,
            rev_before,
        });
    }
    if rows.is_empty() {
        return None;
    }
    scratch.snapshot_seq = scratch.snapshot_seq.wrapping_add(1);
    if restamp {
        scratch.shifted_fill_seq = scratch.snapshot_seq;
    }
    Some(FooterUndo {
        after: Tokens::of(scratch),
        seq_before,
        shifted_before,
        rows,
    })
}

impl App {
    /// The footer facts of every Claude Code session in `wid`'s visible plan
    /// and the window's lights' GUI state (`crate::claude_lights`), folded
    /// into one repaint-key term (`0` when none shows).
    ///
    /// scope-waiver: this is a derived repaint fingerprint of existing App
    /// state. The function owns no latch or budget; copying its scalar result
    /// cannot multiply an enforcing instance.
    pub(crate) fn claude_footer_fp(
        &self,
        wid: WindowId,
        plan: &crate::tab_model::VisibleLeafPlan,
    ) -> u64 {
        let mut fp = self.windows.get(&wid).map_or(0, |ws| {
            ws.claude_lights.fingerprint(std::time::Instant::now())
        });
        let mut claude = false;
        let usage = self.config.claude_footer_usage();
        for leaf in &plan.leaves {
            let Some(session) = self
                .view_store
                .get(leaf.view)
                .copied()
                .and_then(crate::tab_model::View::terminal_session)
            else {
                continue;
            };
            let Some(entry) = self.pool.get(session) else {
                continue;
            };
            let timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
            let facts = timeline.claude_footer();
            claude |= facts.is_some();
            // What the footer shows (`claude_footer_shown`) without its
            // clone: `filled_from` returns `facts` unchanged unless a kept
            // card can fill an open model or effort, so only then is the
            // fill built and hashed; the usage switch is applied by the hash.
            let card = facts
                .filter(|f| f.model_open || f.effort_open)
                .and_then(|_| timeline.claude_card());
            let leaf = match (facts, card) {
                (Some(f), Some(_)) => fingerprint(Some(&f.filled_from(card)), usage),
                (facts, _) => fingerprint(facts, usage),
            };
            // So is the live upgrade's note ([`App::splice_claude_footer`]),
            // laid in only where there are facts to lead: both its wordings,
            // hashed beside the facts rather than cloned into them. Which one
            // shows follows the composer, whose every change is a frame.
            let note = timeline
                .upgrade_note()
                .filter(|_| facts.is_some())
                .map_or(0, |note| {
                    use std::hash::{Hash, Hasher};
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    note.hash(&mut h);
                    h.finish()
                });
            fp = fp.rotate_left(7) ^ leaf ^ note.rotate_left(17);
        }
        // The `[claude_footer] usage` switch, live: a reload that flips it
        // asks every window for a frame (the config apply), and this term
        // lets it through the key.
        if claude {
            fp ^= u64::from(usage).rotate_left(47);
        }
        fp
    }

    /// Write the footer into each visible Claude Code pane's composer rule,
    /// inside that pane's columns and only over its plain rule glyphs.
    /// Runs after the tab strip is prepended (frame row = strip + pane row),
    /// so a find bar or settings panel that already claimed the row keeps it.
    /// A no-op for every pane with no facts, a pane scrolled into history,
    /// and a screen with no composer frame (a dialog or a picker up).
    ///
    /// The write is recorded for [`undo`] (see the module header), and an
    /// outstanding record is undone FIRST: a frame that presents its scratch
    /// without refilling it must read the vendor's row, not last frame's
    /// footer.
    pub(crate) fn splice_claude_footer(
        &mut self,
        wid: WindowId,
        plan: &crate::tab_model::VisibleLeafPlan,
        route: VisibleContentRoute,
    ) {
        if let Some(ws) = self.windows.get_mut(&wid) {
            undo(&mut ws.claude_footer_undo, &mut ws.input_scratch);
            ws.claude_lights.clear_hits();
            ws.claude_footer_texts.borrow_mut().trim(plan.leaves.len());
        }
        // The footer's facts are keyed by the foreground group the status
        // sweep keeps current; with `tab_status` off that sweep does not run,
        // the group goes stale, and a new Claude Code in the tab would be
        // painted with the last one's facts. No sweep, no footer, no lights.
        if !self.config.tab_status_or_default() {
            if let Some(ws) = self.windows.get(&wid) {
                ws.claude_footer_texts.borrow_mut().trim(0);
            }
            return;
        }
        let mark_fg = crate::chrome_band::band_colors(self.theme).accent;
        let mut panes: Vec<FooterPane> = Vec::new();
        let mut seen: Vec<SeenPaneText> = Vec::new();
        // Panes whose composer is on the glass with no plain rule glyph to
        // write into (a selection over the whole rule): nothing is painted,
        // but the keyboard's chord is still the lights'.
        let mut bare: Vec<u64> = Vec::new();
        {
            let Some(ws) = self.windows.get(&wid) else {
                return;
            };
            let frame_rows = ws.input_scratch.cells.len();
            let strip = frame_rows.saturating_sub(usize::from(ws.rows));
            for (leaf_index, leaf) in plan.leaves.iter().enumerate() {
                let Some(session) = self
                    .view_store
                    .get(leaf.view)
                    .copied()
                    .and_then(crate::tab_model::View::terminal_session)
                else {
                    continue;
                };
                let Some(entry) = self.pool.get(session) else {
                    continue;
                };
                let Some(source) = frame_source(ws, route, leaf_index, leaf) else {
                    continue;
                };
                if source.display_offset != 0 {
                    continue;
                }
                // What the footer shows (`claude_footer_shown`), and — while
                // nothing since the process started named its model or
                // effort — the owner a launch card may be kept for.
                let shown = {
                    let timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
                    timeline.claude_footer().map(|facts| {
                        let open = facts.model_open || facts.effort_open;
                        (
                            facts.filled_from(timeline.claude_card()),
                            open.then(|| facts.owner.clone()).flatten(),
                            timeline.upgrade_note().cloned(),
                        )
                    })
                };
                let Some((mut shown, card_owner, upgrade_note)) = shown else {
                    continue;
                };
                let usage = self.config.claude_footer_usage();
                with_usage_switch(&mut shown, usage);
                let blank = source.implicit_blank;
                let row_off = leaf.rect.origin.y.round().max(0.0) as usize;
                let col_off = leaf.rect.origin.x.round().max(0.0) as usize;
                let pane_rows = leaf.rect.size.height.round().max(1.0) as usize;
                let pane_cols = leaf.rect.size.width.round().max(1.0) as usize;
                let region = PaneTextRegion {
                    strip,
                    row_off,
                    col_off,
                    rows: pane_rows,
                    cols: pane_cols,
                };
                let texts = ws.claude_footer_texts.borrow_mut().rows(
                    leaf_index,
                    session,
                    &ws.input_scratch,
                    region,
                );
                // The rule to write into, found by structure, behind the
                // reader's panic fence (`read_footer`) — a screen that panics
                // it paints nothing — unless the chrome claimed that row.
                let read = read_footer(session, &texts, read_rule_row);
                if read.is_some_and(|r| !r.mode_row) {
                    note_vendor_drift(session, shown.version.as_deref(), &texts);
                }
                // Nothing since this process started named its model or
                // effort: its launch card may, read off a live REPL of its
                // own build (`footer::launch_card`). It is read again on
                // every frame while they stay open, and the NEWEST reading
                // for this owner is kept (`note_claude_card`): a card read
                // in the moment between a new owner and the new process's
                // first paint — its predecessor's, still on the glass — is
                // replaced as soon as the new process draws its own. A frame
                // with no card to read (it scrolled away, a message sits
                // under it) keeps the last reading: the card still names
                // what the process started on. A card kept anew shows now.
                if let Some(owner) = card_owner
                    && let Some(card) = crate::reader_guard::read_or_none(
                        session,
                        "Claude Code launch card",
                        &texts,
                        || footer::launch_card(&texts),
                    )
                    .flatten()
                {
                    let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
                    if let Some((pgid, _)) = timeline.claude_footer_identity()
                        && timeline.note_claude_card(pgid, &owner, card)
                        && let Some(now) = timeline.claude_footer_shown()
                    {
                        shown = now;
                        with_usage_switch(&mut shown, usage);
                    }
                }
                let on_glass = read
                    .map(|r| r.rule)
                    .filter(|&r| !self.chrome_owns_terminal_row(wid, row_off + r));
                let rule = on_glass.and_then(|r| {
                    let frame_row = strip + row_off + r;
                    let (mask, rule) = rule_mask(&ws.input_scratch, frame_row, col_off, pane_cols)?;
                    let run = footer::rule_run(&mask)?;
                    Some((frame_row, row_off + r, run, rule))
                });
                if on_glass.is_some() && rule.is_none() {
                    bare.push(session);
                }
                // Every Claude Code pane is OBSERVED for its lights, painted
                // rule or not (`App::observe_claude_lights`).
                seen.push(SeenPaneText {
                    leaf_index,
                    session,
                    texts,
                });
                let Some((frame_row, term_row, run, rule)) = rule else {
                    continue;
                };
                // The live upgrade's note for the person here leads the facts
                // (`footer::FooterFacts::upgrade`), in the words the composer
                // calls for as the ENGINE has it now — by the rule the move's
                // gate reads it with (`App::claude_composer_now`: a dim
                // suggestion is no draft) — and not while a turn runs there:
                // the agent's own turn holds the move then, not the person.
                shown.upgrade = upgrade_note.and_then(|note| {
                    let (screen, empty) = self.claude_composer_now(session)?;
                    (!screen.busy)
                        .then(|| note.words(!empty).cloned())
                        .flatten()
                });
                panes.push(FooterPane {
                    session,
                    frame_row,
                    term_row,
                    col_off,
                    run,
                    rule,
                    facts: shown,
                    blank,
                });
            }
            ws.claude_footer_texts
                .borrow_mut()
                .retain_active(|index| seen.iter().any(|pane| pane.leaf_index == index));
        }
        for pane in seen {
            self.observe_claude_lights(wid, pane.session, &pane.texts);
        }
        for session in bare {
            self.note_claude_footer_row(wid, session, 0);
        }
        if panes.is_empty() {
            return;
        }
        let mut edits: Vec<(usize, usize, Vec<RenderCell>, RenderCell)> =
            Vec::with_capacity(panes.len());
        let mut lit: Vec<Lit> = Vec::new();
        for pane in panes {
            // The rule's room, and the chips' within it: beside the model,
            // which gives way last (`footer::fit_rule`). Only the CHIPS must
            // fit for the block to be drawn: the title (a hover, a
            // selection, a toggle's progress or refusal) comes along only
            // where it fits too, so pointing at a chip can never push the
            // chips off. No chip drawn — everything as expected — and there
            // is no block.
            let room = footer::rule_room(&pane.run);
            let chips_room = footer::chips_room(&pane.facts, room);
            // The rule is on the glass whether or not anything fits it: the
            // keyboard's chord is the lights' there (`on_key_claude_lights`).
            self.note_claude_footer_row(wid, pane.session, chips_room);
            let block = self.claude_lights_block(wid, pane.session, pane.blank, chips_room);
            let widths = block.as_ref().map(|b| footer::LightsWidth {
                chips: b.lights.len(),
                title: b.title.len(),
                short: b.short_title.len(),
                reason: b.reason,
            });
            let fit = footer::fit_rule(&pane.facts, room, widths);
            let (cells, lights_at) =
                compose_rule(&fit, block.as_ref(), pane.rule, pane.blank, mark_fg, room);
            if cells.is_empty() {
                continue;
            }
            // Right-aligned against the rule's last glyph, which stays.
            let col = pane.col_off + pane.run.end - 1 - cells.len();
            if let (Some(block), Some(at)) = (block, lights_at) {
                lit.push(Lit {
                    session: pane.session,
                    frame_row: pane.frame_row,
                    col,
                    term_row: pane.term_row,
                    lights_col: col + at,
                    block,
                });
            }
            edits.push((pane.frame_row, col, cells, pane.blank));
        }
        {
            let Some(ws) = self.windows.get_mut(&wid) else {
                return;
            };
            ws.claude_footer_undo = write_edits(&mut ws.input_scratch, edits);
            // Only a light the glass shows can be pointed at: a pane whose
            // write was skipped (`write_edits`) records none — keyed by the
            // edit's row AND column, since two panes side by side share a
            // frame row.
            let rows = ws
                .claude_footer_undo
                .as_ref()
                .map(|record| &record.rows[..])
                .unwrap_or_default();
            lit.retain(|l| {
                rows.iter()
                    .any(|r| (r.frame_row, r.col_off) == (l.frame_row, l.col))
            });
        }
        for l in lit {
            self.note_claude_light_hits(
                wid,
                l.session,
                l.frame_row,
                l.term_row,
                l.lights_col,
                &l.block,
            );
        }
    }

    /// `Wake::ClaudeFooter`: a session's footer facts moved — redraw every
    /// window that shows it. The repaint key's footer term is what lets the
    /// redraw through.
    pub(crate) fn on_claude_footer_changed(&mut self, session: u64) {
        // The lights ride this wake too: a toggle's next step, its deadline
        // and a refusal's end all arrive here (`crate::claude_lights`).
        self.drain_claude_lights();
        self.redraw_claude_footer(session);
    }

    /// Redraw every window that shows `session`'s footer — and nothing of
    /// the lights' wake: what a live upgrade's note that moved asks for
    /// (`App::apply_agent_upgrades`).
    pub(crate) fn redraw_claude_footer(&self, session: u64) {
        let mut windows = self.windows_with_focused_session(session);
        for (wid, _) in self.tabs_viewing_session(session) {
            if !windows.contains(&wid) {
                windows.push(wid);
            }
        }
        for wid in windows {
            if let Some(window) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
                window.request_redraw();
            }
        }
    }
}

/// A pane's footer reading: where the composer's bottom rule is, and whether
/// a mode row this build reads sits under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RuleRead {
    rule: usize,
    mode_row: bool,
}

type FooterRead = Option<RuleRead>;

/// The footer's reading of a pane's rows: the composer's bottom rule
/// (`aterm_phase::phase::composer_bottom`) and the mode row under it
/// (`footer::mode_row`).
fn read_rule_row(rows: &[String]) -> FooterRead {
    aterm_phase::phase::composer_bottom(rows).map(|rule| RuleRead {
        rule,
        mode_row: footer::mode_row_under(rows, rule).is_some(),
    })
}

/// VENDOR DRIFT, SAID OUT LOUD: a Claude Code pane whose composer has a
/// pill-shaped mode row under it that this build does not read
/// (`footer::opens_with_pill_glyph` but no `footer::pill_indicator`). The
/// lights read no mode from such a row, as they must; this puts
/// the row in aterm's log ONCE per Claude Code build and row (at most
/// [`DRIFT_ROWS_LOGGED`] rows a run), so a vendor that renamed or added a
/// mode is found by reading the log rather than by lights that quietly went
/// grey. Behind the reader's panic fence, like every read of the pane.
fn note_vendor_drift(session: u64, version: Option<&str>, rows: &[String]) {
    static LOGGED: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let unread =
        crate::reader_guard::read_or_none(session, "Claude Code drift check", rows, || {
            unread_pill_row(rows)
        })
        .flatten();
    let Some(row) = unread else {
        return;
    };
    let build = version.unwrap_or("(unknown build)");
    // Keyed by build AND the row's head, so one unreadable pill cannot take
    // the only line a build gets and hide a different one seen later.
    let head: String = row.split_whitespace().take(3).collect::<Vec<_>>().join(" ");
    let key = format!("{build}\t{head}");
    let mut logged = LOGGED.lock().unwrap_or_else(|p| p.into_inner());
    if logged.contains(&key) || logged.len() >= DRIFT_ROWS_LOGGED {
        return;
    }
    logged.push(key);
    aterm_log::warn!(
        "claude footer: Claude Code {build} drew a mode row this aterm does not read, so the lights stand down on it: {row:?}"
    );
}

/// How many distinct unreadable rows (per build) one run logs.
const DRIFT_ROWS_LOGGED: usize = 16;

/// The row under the composer that opens with a pill glyph but names no pill
/// this build knows — `None` for every screen the footer can read, and for
/// one with no composer (a box is up) or nothing pill-shaped under it.
fn unread_pill_row(rows: &[String]) -> Option<String> {
    let (_, bottom) = aterm_phase::phase::composer_rules(rows)?;
    let row = rows
        .get(bottom + 1..)?
        .iter()
        .take_while(|r| !r.trim().is_empty())
        .find(|r| footer::opens_with_pill_glyph(r))?;
    // A known pill cut short by a narrow pane is geometry, not drift.
    (footer::pill_indicator(row).is_none() && !footer::is_cut_pill(row))
        .then(|| row.trim_end().to_owned())
}

/// `read` over pane `session`'s rows inside the reader's panic fence
/// (`crate::reader_guard`): a panic is warned once and read as NO rule, so
/// the vendor's own rows are left as drawn (the seam the tests inject a
/// panicking reader through).
fn read_footer(
    session: u64,
    rows: &[String],
    read: impl FnOnce(&[String]) -> FooterRead,
) -> FooterRead {
    crate::reader_guard::read_or_none(session, "Claude Code footer", rows, || read(rows)).flatten()
}

/// A row as it READS on the glass: each cell's character, except that a
/// run of set facts (`Script::Sub` glyphs, each nudged from its cell to
/// its place on the line, `footer::set_facts`) reads in the order it is
/// drawn, with a space for each pitch of room the line leaves between
/// two glyphs — the text the line was set from, whatever cells its
/// glyphs stand in.
#[cfg(test)]
pub(crate) fn read_glass(row: &[RenderCell]) -> String {
    use aterm_core::terminal::Script;
    let glyph = |c: &RenderCell| matches!(c.script, Script::Sub { .. }) && !c.wide && c.ch != ' ';
    let in_run =
        |c: &RenderCell| matches!(c.script, Script::Sub { .. }) || (c.ch == ' ' && !c.wide);
    let step = 1.0 / f32::from(Script::NUDGES_PER_CELL);
    let mut out = String::new();
    let mut c = 0;
    while c < row.len() {
        if !glyph(&row[c]) {
            out.push(row[c].ch);
            c += 1;
            continue;
        }
        // The run: through its last glyph (and that glyph's right half).
        let mut end = c;
        let mut k = c;
        while k < row.len() && in_run(&row[k]) {
            if glyph(&row[k]) || row[k].wide {
                end = k + 1;
            }
            k += 1;
        }
        let mut last: Option<(f32, usize)> = None;
        for (k, cell) in row.iter().enumerate().take(end).skip(c) {
            let Script::Sub { nudge } = cell.script else {
                continue;
            };
            if !glyph(cell) {
                continue;
            }
            let cols = if row.get(k + 1).is_some_and(|n| n.wide) {
                2
            } else {
                1
            };
            let at = k as f32 + cols as f32 / 2.0 + f32::from(nudge) * step;
            if let Some((before, before_cols)) = last {
                let room = (at - before) / footer::FACT_PITCH - (before_cols + cols) as f32 / 2.0;
                out.extend(std::iter::repeat_n(' ', room.round().max(0.0) as usize));
            }
            out.push(cell.ch);
            last = Some((at, cols));
        }
        c = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE CHIPS NEVER TOUCH OTHER INK (main's 35d8c4ca7, 2026-09-28, a
    /// private window on 2.1.284): drawn on Claude's mode row, a chip was
    /// glued to the `◐ medium · /effort` tail Claude right-aligns there in
    /// auto mode, and main kept one cell clear
    /// wherever the pane had slack. The chips are in the composer's bottom
    /// rule now, at its left end, and the rule's own layout pads them: at
    /// EVERY room the fit draws them in, with or without a title, beside the
    /// whole facts, a limit wall and Σ, or none, the cell before the chips
    /// and the cell after them are blank — never a rule glyph, a title's
    /// word or a fact — and the chips always fit (main's other half: the gap
    /// never costs the chips their place). The painted row, on the measured
    /// screen: `claude_lights`'
    /// `the_effort_tail_stays_on_claudes_row_and_every_chip_stands_clear_in_the_rule`.
    #[test]
    fn the_chips_keep_a_blank_cell_on_each_side_at_every_width() {
        let blank = cell(' ', [200, 200, 200]);
        let rule = cell('\u{2500}', [90, 90, 90]);
        for facts in [facts(), facts_with_usage(), FooterFacts::default()] {
            for chips in [1usize, 6, 8, 14] {
                for (title, short) in [("", ""), ("Permission mode: plan  ", "plan  ")] {
                    let block = crate::claude_lights::Block::of_chips(
                        row_of(&"\u{25CB}".repeat(chips), [150, 150, 150]),
                        row_of(title, [150, 150, 150]),
                        row_of(short, [150, 150, 150]),
                        false,
                    );
                    let widths = footer::LightsWidth {
                        chips,
                        title: title.chars().count(),
                        short: short.chars().count(),
                        reason: false,
                    };
                    for room in 0usize..160 {
                        let fit = footer::fit_rule(&facts, room, Some(widths));
                        if !fit.chips {
                            assert!(
                                chips > footer::chips_room(&facts, room),
                                "{room}/{chips}: the chips fit, yet were not drawn"
                            );
                            continue;
                        }
                        let (cells, at) =
                            compose_rule(&fit, Some(&block), rule, blank, [0, 128, 255], room);
                        let at = at.expect("drawn chips have a column");
                        let row = text_of(&cells);
                        assert!(cells.len() <= room, "{room}: {row:?}");
                        assert_eq!(cells[at - 1].ch, ' ', "{room}/{chips} before: {row:?}");
                        assert_eq!(cells[at + chips].ch, ' ', "{room}/{chips} after: {row:?}");
                    }
                }
            }
        }
    }

    fn watch_model_projection(schedule: &WatchSchedule) -> i64 {
        match schedule.watched.get(&7).map(|(job, _, _)| job.pgid) {
            None => 0,
            Some(42) => 1,
            Some(43) => 2,
            other => panic!("unexpected foreground group: {other:?}"),
        }
    }

    #[test]
    fn stopped_footer_watch_has_no_followup_or_idle_reads() {
        // Tier-1 for ClaudeFooterWatch: the real scheduler's retained job and
        // idle-read guard agree with the derived model before and after stop.
        let model = aterm_spec::derive::claude_footer_watch_model();
        let mut state = model.init_state();
        let now = std::time::Instant::now();
        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        let weak = Arc::downgrade(&timeline);
        let mut schedule = WatchSchedule::default();
        schedule.watch(
            Job {
                session: 7,
                timeline,
                pgid: 42,
            },
            now,
        );
        assert!(model.fire("AskOld", &mut state));
        assert_eq!(state["watch"], watch_model_projection(&schedule));
        assert_eq!(
            model.action_enabled("IdleRead", &state),
            schedule.due_idle(now + IDLE_RECHECK).is_some()
        );
        let (immediate, stopped) = schedule.accept([Command::Stop {
            session: 7,
            pgid: None,
        }]);
        assert!(model.fire("StopSession", &mut state));
        assert!(immediate.is_empty());
        assert_eq!(stopped, [7]);
        assert_eq!(state["watch"], watch_model_projection(&schedule));
        assert!(schedule.next(now).is_none());
        assert!(schedule.due_followups(now + FOLLOW_UPS[1]).is_empty());
        assert_eq!(
            model.action_enabled("IdleRead", &state),
            schedule.due_idle(now + IDLE_RECHECK).is_some()
        );
        assert!(weak.upgrade().is_none(), "the watch released its timeline");
    }

    #[test]
    fn stale_group_stop_preserves_replacement_footer_watch() {
        let model = aterm_spec::derive::claude_footer_watch_model();
        let mut state = model.init_state();
        let now = std::time::Instant::now();
        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        let mut schedule = WatchSchedule::default();
        schedule.watch(
            Job {
                session: 7,
                timeline: Arc::clone(&timeline),
                pgid: 42,
            },
            now,
        );
        assert!(model.fire("AskOld", &mut state));
        let (mut immediate, stopped) = schedule.accept([
            Command::Request(Job {
                session: 7,
                timeline,
                pgid: 43,
            }),
            Command::Stop {
                session: 7,
                pgid: Some(42),
            },
        ]);
        assert!(model.fire("AskNew", &mut state));
        assert!(model.fire("StopOld", &mut state));
        assert_eq!(stopped, [7]);
        assert_eq!(immediate.len(), 1);
        let replacement = immediate.pop().unwrap();
        assert_eq!(replacement.pgid, 43);
        schedule.watch(replacement, now);
        assert_eq!(state["watch"], watch_model_projection(&schedule));
        let (immediate, stopped) = schedule.accept([Command::Stop {
            session: 7,
            pgid: Some(42),
        }]);
        assert!(immediate.is_empty());
        assert!(stopped.is_empty());
        let due = schedule
            .due_idle(now + IDLE_RECHECK)
            .expect("active Claude keeps its idle refresh");
        assert_eq!(due.pgid, 43);
        assert!(model.action_enabled("IdleRead", &state));
    }

    /// A WATCH LAPSE KEEPS THE PROCESS'S PIN (review of 2026-09-28, round
    /// 4). The resolver keeps a Claude process's transcript cache — what the
    /// process decided, when it was last seen in which session, and whether
    /// its model is pinned — while the process lives, however its watch
    /// goes: the ten-minute ask lapse ([`WATCH_FOR`]) and a stop (a ctrl-z)
    /// both leave it. Here the process restored Fable 5.1 at `/resume A`,
    /// which pins it; the watch lapsed (or stopped); at `/resume B` the
    /// footer still names Fable 5.1, as Claude keeps it. NEGATIVE CONTROLS:
    /// once the kernel no longer has the process its cache goes, and a first
    /// sight of B cannot know the pin — it names no model; a process whose
    /// start could not be read loses its cache with its watch; and no more
    /// than [`MAX_KEPT`] are kept, the least recently read going first.
    #[test]
    fn a_watch_lapse_keeps_the_processs_pin() {
        use std::time::{Instant, UNIX_EPOCH};
        const START: u64 = 1_790_607_848; // 2026-09-28T15:04:08Z
        const S: &str = "00000000-0000-4000-8000-000000000c01";
        const A: &str = "00000000-0000-4000-8000-000000000c02";
        const B: &str = "00000000-0000-4000-8000-000000000c03";
        let root =
            std::env::temp_dir().join(format!("aterm-gui-footer-lapse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let cwd = root.join("work");
        let claude = root.join("claude");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(claude.join("sessions")).unwrap();
        let project = claude.join("projects").join(footer::project_slug(&cwd));
        std::fs::create_dir_all(&project).unwrap();
        let register = |session: &str| {
            std::fs::write(
                claude.join("sessions/4242.json"),
                format!(
                    r#"{{"pid":4242,"sessionId":"{session}","cwd":"{}","procStart":"{}","startedAt":{},"version":"2.1.284"}}"#,
                    cwd.display(),
                    footer::lstart_utc(START),
                    START * 1000 + 684
                ),
            )
            .unwrap();
        };
        let answer = |session: &str, model: &str, at: &str| {
            std::fs::write(
                project.join(format!("{session}.jsonl")),
                format!(
                    "{{\"type\":\"assistant\",\"timestamp\":\"{at}\",\"effort\":\"high\",\"message\":{{\"model\":\"{model}\"}}}}\n"
                ),
            )
            .unwrap();
        };
        answer(S, "claude-opus-5-5", "2026-09-28T15:05:00.000Z");
        answer(A, "claude-fable-5-1", "2026-09-27T10:00:00.000Z");
        answer(B, "claude-sonnet-5", "2026-09-26T10:00:00.000Z");
        // One read of the process through the kept cache, at `START + secs`
        // on the wall clock.
        let read = |tails: &mut ProcessTails, started: Option<u64>, secs: u64, now: Instant| {
            let entry = footer::session_of_pid(&claude, 4242, started).expect("the registry");
            footer::facts_for_entry_at(
                &claude,
                4242,
                started,
                &entry,
                &LaunchFacts::default(),
                tails.cache(7, 4242, started, now),
                UNIX_EPOCH + Duration::from_secs(START + secs),
            )
            .model
        };
        let fable = Some("Fable 5.1".to_owned());
        // `/resume A` restores and pins; then the watch goes `how`, the
        // kernel says whether the process `lives`, and `/resume B`.
        let run = |started: Option<u64>, how: &str, lives: bool| {
            let t0 = Instant::now();
            let mut schedule = WatchSchedule::default();
            schedule.watch(
                Job {
                    session: 7,
                    timeline: Arc::new(Mutex::new(SessionTimeline::default())),
                    pgid: 4242,
                },
                t0,
            );
            let mut tails = ProcessTails::default();
            register(S);
            assert_eq!(
                read(&mut tails, started, 120, t0).as_deref(),
                Some("Opus 5.5")
            );
            register(A);
            assert_eq!(read(&mut tails, started, 180, t0), fable, "restored");
            let later = t0 + WATCH_FOR;
            match how {
                // A lapse with no limit wall on show.
                "lapse" => schedule.prune(later, |_, _| false),
                "stop" => {
                    let (_, stopped) = schedule.accept([Command::Stop {
                        session: 7,
                        pgid: Some(4242),
                    }]);
                    assert_eq!(stopped, [7]);
                }
                other => panic!("{other}"),
            }
            assert!(schedule.watched.is_empty(), "the watch is gone");
            tails.retain(
                &schedule,
                |pgid, born| lives && (pgid, born) == (4242, START),
                later,
            );
            register(B);
            read(&mut tails, started, 900, later)
        };
        assert_eq!(
            run(Some(START), "lapse", true),
            fable,
            "the pin outlived the lapse"
        );
        assert_eq!(run(Some(START), "stop", true), fable, "and the stop");
        // NEGATIVE CONTROLS: the process gone, and no start to prove it the
        // same — a first sight of B, which names no model.
        assert_eq!(run(Some(START), "lapse", false), None);
        assert_eq!(run(None, "lapse", true), None);
        // The bound: the least recently read goes first.
        let t0 = Instant::now();
        let mut tails = ProcessTails::default();
        tails.cache(7, 4242, Some(START), t0);
        for n in 0..MAX_KEPT {
            let n64 = u64::try_from(n).unwrap();
            tails.cache(
                8 + n64,
                5000 + i32::try_from(n).unwrap(),
                Some(1),
                t0 + Duration::from_secs(1 + n64),
            );
        }
        assert_eq!(tails.kept.len(), MAX_KEPT);
        assert!(!tails.kept.contains_key(&(4242, Some(START))));
        assert!(tails.kept.contains_key(&(5000, Some(1))));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pane_text_reuses_only_the_same_scratch_revision_and_pane() {
        let mut scratch = RenderInput::empty();
        // A host prepend can make `scratch.rows` include its chrome row;
        // `strip` remains based on the window's terminal row count.
        scratch.rows = 4;
        scratch.cols = 4;
        scratch.cells = vec![
            row_of("chrome", [0, 0, 0]),
            row_of("abcd", [0, 0, 0]),
            row_of("efgh", [0, 0, 0]),
            row_of("ijkl", [0, 0, 0]),
        ];
        scratch.snapshot_seq = 5;
        let mut cache = PaneTextCache::default();
        let full = PaneTextRegion {
            strip: 1,
            row_off: 0,
            col_off: 0,
            rows: 3,
            cols: 4,
        };
        let first = cache.rows(0, 7, &scratch, full);
        assert_eq!(&**first, &["abcd", "efgh", "ijkl"]);
        let again = cache.rows(0, 7, &scratch, full);
        assert!(Arc::ptr_eq(&first, &again));

        let other_pane = cache.rows(
            1,
            7,
            &scratch,
            PaneTextRegion {
                strip: 1,
                row_off: 1,
                col_off: 1,
                rows: 2,
                cols: 2,
            },
        );
        assert_eq!(&**other_pane, &["fg", "jk"]);
        assert!(!Arc::ptr_eq(&first, &other_pane));
        let other_session = cache.rows(0, 8, &scratch, full);
        assert_eq!(&**other_session, &**first);
        assert!(!Arc::ptr_eq(&first, &other_session));

        scratch.cells[1][0].ch = 'z';
        scratch.snapshot_seq += 1;
        let changed = cache.rows(0, 8, &scratch, full);
        assert_eq!(&changed[0], "zbcd");
        assert!(!Arc::ptr_eq(&other_session, &changed));
        cache.retain_active(|index| index == 1);
        assert!(cache.panes[0].is_none());
        assert!(cache.panes[1].is_some());
        cache.trim(1);
        assert_eq!(cache.panes.len(), 1);
        cache.trim(0);
        assert!(cache.panes.is_empty());
    }

    #[test]
    fn changed_claude_frame_reuses_free_row_buffers_but_preserves_held_snapshots() {
        let mut scratch = RenderInput::empty();
        scratch.rows = 2;
        scratch.cols = 4;
        scratch.cells = vec![row_of("abcd", [0, 0, 0]), row_of("efgh", [0, 0, 0])];
        scratch.snapshot_seq = 1;
        let region = PaneTextRegion {
            strip: 0,
            row_off: 0,
            col_off: 0,
            rows: 2,
            cols: 4,
        };
        let mut cache = PaneTextCache::default();
        let first = cache.rows(0, 7, &scratch, region);
        let vec_ptr = first.as_ref().as_ptr();
        let row_ptrs = [first[0].as_ptr(), first[1].as_ptr()];
        drop(first);

        scratch.cells[0][0].ch = 'z';
        scratch.snapshot_seq += 1;
        let reused = cache.rows(0, 7, &scratch, region);
        assert_eq!(&**reused, &["zbcd", "efgh"]);
        assert_eq!(reused.as_ref().as_ptr(), vec_ptr);
        assert_eq!([reused[0].as_ptr(), reused[1].as_ptr()], row_ptrs);

        // A reader still using that exact frame must keep its old text while
        // the next echo builds a new snapshot from fresh storage.
        let held = Arc::clone(&reused);
        scratch.cells[0][1].ch = 'y';
        scratch.snapshot_seq += 1;
        let next = cache.rows(0, 7, &scratch, region);
        assert_eq!(&**held, &["zbcd", "efgh"]);
        assert_eq!(&**next, &["zycd", "efgh"]);
        assert!(!Arc::ptr_eq(&held, &next));
    }

    fn vendor_frame(scratch: &mut RenderInput) {
        let bottom = scratch.rows - 1;
        let rule = "\u{2500}".repeat(scratch.cols);
        scratch.cells[bottom - 3] = row_of(&rule, [40, 40, 40]);
        scratch.cells[bottom - 2] = row_of("\u{276F} ", [40, 40, 40]);
        scratch.cells[bottom - 1] = row_of(&rule, [40, 40, 40]);
        scratch.cells[bottom] = row_of(
            "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle) \u{00B7} \u{2190} for agents",
            [90, 90, 90],
        );
        // This fixture paints the vendor rows directly over an engine fill.
        scratch.snapshot_seq = scratch.snapshot_seq.wrapping_add(1);
    }

    fn cell(ch: char, fg: [u8; 3]) -> RenderCell {
        RenderCell {
            ch,
            fg,
            ..RenderCell::default()
        }
    }

    fn row_of(text: &str, fg: [u8; 3]) -> Vec<RenderCell> {
        text.chars().map(|c| cell(c, fg)).collect()
    }

    fn text_of(row: &[RenderCell]) -> String {
        read_glass(row)
    }

    /// The cells a footer READING (` ◆ Opus 5.5 xhigh   ⌂ ~/aterm `: its
    /// facts [`footer::GAP`] spaces apart) takes set: its line, and a blank
    /// cell either side.
    fn cells_for(reading: &str) -> usize {
        let segments: Vec<footer::Segment> = reading
            .trim()
            .split("   ")
            .map(|fact| {
                let (mark, text) = fact.split_once(' ').expect("<mark> <text>");
                footer::Segment {
                    mark: mark.chars().next().expect("a mark"),
                    text: text.to_owned(),
                }
            })
            .collect();
        footer::segments_width(&segments) + 2
    }

    /// A cell's ink and rendition, without where its glyph is nudged to.
    fn unplaced(cell: RenderCell) -> RenderCell {
        match cell.script {
            aterm_core::terminal::Script::Sub { .. } => RenderCell {
                script: aterm_core::terminal::Script::Sub { nudge: 0 },
                ..cell
            },
            aterm_core::terminal::Script::Normal => cell,
        }
    }

    fn facts() -> FooterFacts {
        FooterFacts {
            model: Some("Opus 5.5".into()),
            effort: Some("xhigh".into()),
            path: Some("~/aterm".into()),
            branch: Some("main".into()),
            ..FooterFacts::default()
        }
    }

    /// The footer's reading runs behind the reader's panic fence: a reader
    /// that panics on a pane's rows paints NO rule (the vendor's stays) and
    /// the frame goes on. Control: the real reader over the same rows finds
    /// the rule and the mode row under it, so the `None` is the panic's.
    #[test]
    fn a_reader_panic_paints_no_footer_row() {
        let rows = vec![
            "\u{2500}".repeat(60),
            "\u{276F} ".to_string(),
            "\u{2500}".repeat(60),
            "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)".to_string(),
        ];
        assert_eq!(
            read_footer(9, &rows, read_rule_row),
            Some(RuleRead {
                rule: 2,
                mode_row: true
            }),
            "the real reader finds it"
        );
        let boom = |_: &[String]| -> FooterRead { panic!("stand-in reader panic") };
        assert!(read_footer(9, &rows, boom).is_none());
        assert!(read_footer(9, &rows, boom).is_none(), "every frame");
    }

    /// The owner's layout, exactly, written into a rule: one space either
    /// side, three spaces' room between values, the rule's last glyph kept — the
    /// way Claude writes its top-effort tag, one space either side and a `─`
    /// after it, into the rule above. The facts are set apart and quiet:
    /// small, low and bold (`Script::Sub`), values in the rule's gray lifted
    /// a third of the way toward the pane's ink, marks in the accent mixed
    /// halfway toward the rule's gray.
    #[test]
    fn the_rule_carries_the_owners_footer() {
        let blank = cell(' ', [200, 200, 200]);
        let rule = cell('\u{2500}', [60, 60, 60]);
        let fit = footer::fit_rule(&facts(), 76, None);
        let (cells, lights) = compose_rule(&fit, None, rule, blank, [0, 128, 255], 76);
        assert_eq!(lights, None);
        assert_eq!(
            text_of(&cells),
            " \u{25C6} Opus 5.5 xhigh   \u{2302} ~/aterm   \u{2387} main "
        );
        assert_eq!(
            cells[1].fg,
            [30, 94, 158],
            "the mark: the accent, toward the rule"
        );
        let o = cells.iter().position(|c| c.ch == 'O').expect("the model");
        assert_eq!(
            cells[o].fg,
            [107, 107, 107],
            "the value: the rule's gray, a third of the way to the pane's ink"
        );
        assert!(
            [1, o].iter().all(|&c| cells[c].bold
                && matches!(cells[c].script, aterm_core::terminal::Script::Sub { .. })),
            "the facts are drawn small, low and bold"
        );
        assert_eq!(cells[0].script, aterm_core::terminal::Script::Normal);
        assert_eq!(cells.len(), footer::segments_width(&fit.segments) + 2);
        // Nothing known: nothing written, no dangling pad.
        let none = footer::fit_rule(&FooterFacts::default(), 76, None);
        assert!(
            compose_rule(&none, None, rule, blank, [1, 1, 1], 76)
                .0
                .is_empty()
        );
    }

    /// THE FACTS ARE PAINTED WHERE THEY ARE SET (`footer::set_facts`, whose
    /// own tests hold the line's law): a blank cell, the line's cells, a
    /// blank cell; each glyph in the cell the line gives it, nudged to its
    /// place on the line; every other cell of the line blank, a space
    /// included. CONTROL: the line is shorter than one cell a character —
    /// the cells the old per-character layout took.
    #[test]
    fn the_facts_are_painted_where_they_are_set() {
        let blank = cell(' ', [200, 200, 200]);
        let rule = cell('\u{2500}', [60, 60, 60]);
        let fit = footer::fit_rule(&facts(), 76, None);
        let (cells, _) = compose_rule(&fit, None, rule, blank, [0, 128, 255], 76);
        let set = footer::set_facts(&fit.segments);
        assert_eq!(cells.len(), set.cells + 2);
        let mut glyph_cells = std::collections::HashSet::new();
        for glyph in &set.glyphs {
            let painted = cells[1 + glyph.cell];
            assert_eq!(painted.ch, glyph.ch);
            assert_eq!(
                painted.script,
                aterm_core::terminal::Script::Sub { nudge: glyph.nudge },
                "{glyph:?}"
            );
            glyph_cells.insert(1 + glyph.cell);
        }
        for (c, painted) in cells.iter().enumerate() {
            if !glyph_cells.contains(&c) {
                assert_eq!(*painted, blank, "cell {c} is blank");
            }
        }
        let reading = " \u{25C6} Opus 5.5 xhigh   \u{2302} ~/aterm   \u{2387} main ";
        assert_eq!(text_of(&cells), reading);
        assert!(
            cells.len() < reading.chars().count(),
            "CONTROL: {}",
            cells.len()
        );
    }

    /// THE ACCOUNT ON THE GLASS: `⎆ <label>` stands between the model and the
    /// path, drawn as every fact is — its mark in the marks' ink, its label
    /// in the values' — whole or not at all (a clipped label would name
    /// another account), beside exactly what a rule of that width shows with
    /// no account: a rule one cell short drops it, the path still whole.
    /// The same through the real splice, on the measured screen.
    /// CONTROL: the same facts with no account read the string
    /// `the_rule_carries_the_owners_footer` pins.
    #[test]
    fn the_rule_names_the_account_between_the_model_and_the_path() {
        let blank = cell(' ', [200, 200, 200]);
        let rule = cell('\u{2500}', [60, 60, 60]);
        let listed = FooterFacts {
            account: Some("work-2".into()),
            ..facts()
        };
        let paint = |facts: &FooterFacts, room: usize| {
            let fit = footer::fit_rule(facts, room, None);
            compose_rule(&fit, None, rule, blank, [0, 128, 255], room).0
        };
        let plain = " \u{25C6} Opus 5.5 xhigh   \u{2302} ~/aterm   \u{2387} main ";
        let named =
            " \u{25C6} Opus 5.5 xhigh   \u{2386} work-2   \u{2302} ~/aterm   \u{2387} main ";
        let cells = paint(&listed, 76);
        assert_eq!(text_of(&cells), named);
        let at = cells
            .iter()
            .position(|c| c.ch == footer::ACCOUNT_MARK)
            .expect("the account's mark");
        assert_eq!(
            unplaced(cells[at]),
            unplaced(RenderCell {
                ch: footer::ACCOUNT_MARK,
                ..cells[1]
            }),
            "the mark takes the marks' ink"
        );
        let w = at
            + cells[at..]
                .iter()
                .position(|c| c.ch == 'w')
                .expect("the label");
        let o = cells.iter().position(|c| c.ch == 'O').expect("the model");
        assert_eq!(
            unplaced(cells[w]),
            unplaced(RenderCell {
                ch: 'w',
                ..cells[o]
            }),
            "the label takes the values' ink"
        );
        // Whole or not at all, at every width, and nothing else moves for it.
        let whole = cells_for(named);
        for room in 0..=whole {
            let text = text_of(&paint(&listed, room));
            let unnamed = text_of(&paint(&facts(), room));
            if text.contains(footer::ACCOUNT_MARK) {
                assert_eq!(
                    text.replacen("   \u{2386} work-2", "", 1),
                    unnamed,
                    "{room}"
                );
            } else {
                assert_eq!(text, unnamed, "{room}");
            }
        }
        assert_eq!(text_of(&paint(&listed, whole)), named);
        assert_eq!(text_of(&paint(&listed, whole - 1)), plain, "the path stays");
        let beside = " \u{25C6} Opus 5.5 xhigh   \u{2386} work-2 ";
        assert_eq!(
            text_of(&paint(&listed, cells_for(beside))),
            beside,
            "beside the model, where the path no longer fits"
        );
        // NEGATIVE CONTROL: no account, and no mark for one.
        assert_eq!(text_of(&paint(&facts(), 76)), plain);

        // The real splice, at 80 columns.
        let (mut app, wid, session) = glass(80);
        let rows = claude_rows("footer-bypass-idle", 80);
        let bottom = aterm_phase::phase::composer_bottom(&rows).expect("the composer");
        feed_rows(&app, session, &rows);
        let shown = |app: &mut App, facts: FooterFacts| {
            publish(app, session, facts);
            frame(app, wid);
            text_of(&app.windows[&wid].input_scratch.cells[strip(app, wid) + bottom])
        };
        let glass = shown(&mut app, listed);
        assert!(
            glass.ends_with(&format!("\u{2500}{named}\u{2500}")),
            "{glass:?}"
        );
        let glass = shown(&mut app, facts());
        assert!(
            glass.ends_with(&format!("\u{2500}{plain}\u{2500}")),
            "{glass:?}"
        );
    }

    /// A branch or directory name can be wide: it takes a lead cell and a
    /// continuation, and the layout counts both.
    #[test]
    fn wide_names_take_two_cells() {
        let wide = FooterFacts {
            branch: Some("\u{6F22}\u{5B57}".into()),
            ..FooterFacts::default()
        };
        let blank = cell(' ', [1, 1, 1]);
        let fit = footer::fit_rule(&wide, 20, None);
        let (row, _) = compose_rule(
            &fit,
            None,
            cell('\u{2500}', [2, 2, 2]),
            blank,
            [3, 3, 3],
            20,
        );
        let lead = row.iter().position(|c| c.ch == '\u{6F22}').unwrap();
        assert!(
            !row[lead].wide && row[lead + 1].wide,
            "lead, then its right half"
        );
        assert_eq!(
            row[lead + 2..].iter().find(|c| c.ch != ' ').map(|c| c.ch),
            Some('\u{5B57}')
        );
        assert_eq!(row.len(), footer::segments_width(&fit.segments) + 2);
        // `⎇ 漢字` is six pitches, five cells of ink: a line of five cells
        // and two of pad, so a 6-cell rule drops it.
        assert_eq!(cells_for(" \u{2387} \u{6F22}\u{5B57} "), 7);
        assert!(!footer::fit_rule(&wide, 7, None).segments.is_empty());
        assert!(footer::fit_rule(&wide, 6, None).segments.is_empty());
    }

    fn facts_with_usage() -> FooterFacts {
        use aterm_agent::harness::session_usage::{ModelTokens, UsageFacts, Wall};
        FooterFacts {
            usage: UsageFacts::of(
                (
                    vec![ModelTokens {
                        label: "opus".into(),
                        input: 48_000_000,
                        output: 310_000,
                    }],
                    aterm_agent::harness::session_usage::More::default(),
                ),
                Some(Wall {
                    label: "5h",
                    resets: "3pm".into(),
                }),
                None,
            ),
            ..facts()
        }
    }

    /// Every fact's mark takes one ink, the wall's, its `♻` and `Σ`'s among
    /// them; what a narrow rule gives up, and in what order, is
    /// `footer::fit_rule`'s (its own tests).
    #[test]
    fn the_wall_and_usage_marks_take_the_marks_ink() {
        let blank = cell(' ', [200, 200, 200]);
        let rule = cell('\u{2500}', [60, 60, 60]);
        let facts = facts_with_usage();
        let room = 200;
        let fit = footer::fit_rule(&facts, room, None);
        let cells = compose_rule(&fit, None, rule, blank, [0, 128, 255], room).0;
        let ink = |mark: char| unplaced(cells[cells.iter().position(|c| c.ch == mark).unwrap()]);
        for mark in ['\u{29D7}', footer::RESET_MARK, '\u{03A3}'] {
            assert_eq!(
                ink(mark),
                RenderCell {
                    ch: mark,
                    ..ink('\u{25C6}')
                }
            );
        }
    }

    /// With the session's tokens in the rule, the TOKENS go before the path
    /// is cut: the path is the owner's, the tokens the first value a short
    /// rule gives up. Then the path is cut as it is without them.
    #[test]
    fn the_tokens_go_before_the_path_is_cut() {
        use aterm_agent::harness::session_usage::{ModelTokens, UsageFacts};
        let blank = cell(' ', [1, 1, 1]);
        let rule = cell('\u{2500}', [60, 60, 60]);
        let deep = FooterFacts {
            path: Some("~/src/github.com/someone/aterm".into()),
            usage: UsageFacts::of(
                (
                    vec![ModelTokens {
                        label: "opus".into(),
                        input: 48_000_000,
                        output: 310_000,
                    }],
                    aterm_agent::harness::session_usage::More::default(),
                ),
                None,
                None,
            )
            .map(|u| u.with_runtime(28_320_000)),
            ..facts()
        };
        let paint = |room| {
            let fit = footer::fit_rule(&deep, room, None);
            text_of(&compose_rule(&fit, None, rule, blank, [2, 2, 2], room).0)
        };
        let whole =
            " \u{25C6} Opus 5.5 xhigh   \u{2302} ~/src/github.com/someone/aterm   \u{2387} main";
        let full = format!("{whole}   \u{03A3} 7h52m \u{2191}48M \u{2193}310k ");
        assert_eq!(paint(cells_for(&full)), full);
        let runtime = format!("{whole}   \u{03A3} 7h52m ");
        assert_eq!(
            paint(cells_for(&full) - 1),
            runtime,
            "the tokens go first, the runtime stays"
        );
        assert_eq!(
            paint(cells_for(&runtime) - 1),
            format!("{whole} "),
            "then the runtime; the path stays whole"
        );
        assert_eq!(
            paint(cells_for(whole) - 1),
            " \u{25C6} Opus 5.5 xhigh   \u{2302} \u{2026}/aterm   \u{2387} main "
        );
    }

    /// The offset cache keeps an ANSWER for an hour and no failure at all:
    /// a `date` that could not run is asked again on the next read, so one
    /// failed spawn does not hide a standing wall for an hour.
    #[test]
    fn the_offset_cache_keeps_answers_and_asks_again_after_a_failure() {
        let mut answers = Vec::new();
        let t0 = std::time::Instant::now();
        let asked = std::cell::Cell::new(0);
        let mut ask = |at: std::time::Instant, answer: Option<i64>| {
            cached_offset(&mut answers, Some("UTC"), 100, at, |_, _| {
                asked.set(asked.get() + 1);
                answer
            })
        };
        assert_eq!(ask(t0, None), None, "the read failed");
        assert_eq!(ask(t0, Some(0)), Some(0), "asked again, not the failure");
        assert_eq!(asked.get(), 2);
        assert_eq!(ask(t0 + Duration::from_secs(60), Some(7)), Some(0), "kept");
        assert_eq!(
            asked.get(),
            2,
            "an answer is not read again within the hour"
        );
        assert_eq!(
            ask(t0 + Duration::from_secs(3600), Some(7)),
            Some(7),
            "an hour on, read afresh"
        );
    }

    /// The resolver's clocks: the idle re-read within the watch, the wall's
    /// slower one past it — and past it only while a wall shows AND its
    /// process lives (liveness is asked only then).
    #[test]
    fn a_walled_live_session_stays_watched_on_the_slow_clock() {
        let asked = std::time::Instant::now();
        let within = asked + WATCH_FOR - Duration::from_secs(1);
        let past = asked + WATCH_FOR + Duration::from_secs(1);
        assert_eq!(recheck_after(within, asked), IDLE_RECHECK);
        assert_eq!(recheck_after(past, asked), WALL_RECHECK);
        assert!(still_watched(within, asked, || panic!("not asked within")));
        assert!(still_watched(past, asked, || true));
        assert!(!still_watched(past, asked, || false));
        let mut walled = HashSet::new();
        note_wall(&mut walled, 7, true);
        assert!(walled.contains(&7));
        note_wall(&mut walled, 7, false);
        assert!(walled.is_empty());
        // The schedule keeps the same clocks: past its watch a session stays
        // only while walled and alive, and is re-read on the slow clock.
        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        let mut schedule = WatchSchedule::default();
        schedule.watch(
            Job {
                session: 7,
                timeline,
                pgid: 42,
            },
            asked,
        );
        let _ = schedule.due_followups(past);
        schedule.read(7, past);
        schedule.prune(past, |session, job| session == 7 && job.pgid == 42);
        assert_eq!(schedule.next(past), Some(past + WALL_RECHECK));
        assert!(schedule.due_idle(past + IDLE_RECHECK).is_none());
        assert!(schedule.due_idle(past + WALL_RECHECK).is_some());
        schedule.prune(past, |_, _| false);
        assert!(schedule.watched.is_empty(), "no wall: the watch ends");
    }

    /// A retired session's resolver state goes whole: its fold, its watch,
    /// the follow-ups owed and its wall mark — another session's stays.
    #[test]
    fn forgetting_a_session_drops_its_fold_and_watch() {
        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        let job = |session| Job {
            session,
            timeline: Arc::clone(&timeline),
            pgid: 1,
        };
        let now = std::time::Instant::now();
        let mut schedule = WatchSchedule::default();
        let mut known: HashMap<u64, Identity> = HashMap::new();
        let mut folds: HashMap<u64, footer::FooterCache> = HashMap::new();
        let mut walled: HashSet<u64> = HashSet::new();
        for session in [1, 2] {
            schedule.watch(job(session), now);
            known.insert(
                session,
                Identity {
                    pid: 1,
                    started: None,
                    dir: None,
                    launch: LaunchFacts::default(),
                    launch_for: None,
                    login: None,
                    argv: Vec::new(),
                },
            );
            folds.insert(session, footer::FooterCache::default());
            walled.insert(session);
        }
        // What the status observer's retirement sends (`stop_session`).
        let (immediate, stopped) = schedule.accept([Command::Stop {
            session: 1,
            pgid: None,
        }]);
        assert!(immediate.is_empty());
        assert_eq!(stopped, [1]);
        for session in stopped {
            forget(session, &mut known, &mut folds, &mut walled);
        }
        // Session 2 was watched at `now`, so every one of its follow-ups is
        // due by the last one; none of session 1's is left owed.
        let due = schedule.due_followups(now + FOLLOW_UPS[FOLLOW_UPS.len() - 1]);
        assert_eq!(
            due.len(),
            FOLLOW_UPS.len(),
            "the other session's follow-ups stay owed"
        );
        assert!(
            due.iter().all(|job| job.session == 2),
            "no follow-up owed for the retired session"
        );
        for held in [
            known.contains_key(&1),
            folds.contains_key(&1),
            walled.contains(&1),
            schedule.watched.contains_key(&1),
        ] {
            assert!(!held);
        }
        assert!(known.contains_key(&2) && folds.contains_key(&2) && walled.contains(&2));
        assert!(schedule.watched.contains_key(&2));
    }

    /// A group that leaves the foreground (Claude Code suspended, or another
    /// program in front) releases its session's fold with the watch; the fold
    /// is rebuilt from the transcript when Claude Code is asked for again. A
    /// late stop naming an older group releases nothing.
    #[test]
    fn a_group_leaving_the_foreground_releases_its_fold() {
        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        let now = std::time::Instant::now();
        let mut schedule = WatchSchedule::default();
        let mut known: HashMap<u64, Identity> = HashMap::new();
        let mut folds: HashMap<u64, footer::FooterCache> = HashMap::new();
        let mut walled: HashSet<u64> = HashSet::new();
        schedule.watch(
            Job {
                session: 1,
                timeline: Arc::clone(&timeline),
                pgid: 40,
            },
            now,
        );
        folds.insert(1, footer::FooterCache::default());
        walled.insert(1);
        let mut stop = |pgid| {
            let (immediate, stopped) = schedule.accept([Command::Stop {
                session: 1,
                pgid: Some(pgid),
            }]);
            assert!(immediate.is_empty());
            for session in &stopped {
                forget(*session, &mut known, &mut folds, &mut walled);
            }
            (stopped, folds.contains_key(&1), walled.contains(&1))
        };
        assert_eq!(stop(39), (vec![], true, true), "an older group's stop");
        assert_eq!(
            stop(40),
            (vec![1], false, false),
            "the watched group's stop"
        );
    }

    /// A roster of two: this machine's own login and one with a directory.
    fn two_accounts() -> accounts::Roster {
        let row = |label: &str, dir: Option<&str>| accounts::Row {
            label: label.into(),
            class: accounts::Class::Dedicated,
            dir: dir.map(str::to_owned),
        };
        accounts::Roster {
            rows: vec![row("main", None), row("work-2", Some("/logins/work-2"))],
        }
    }

    /// One process's exec read with `env` as its `KEY=value` entries.
    fn args_of(env: &[&str]) -> atpkg::caller_shell::ProcArgs {
        atpkg::caller_shell::ProcArgs {
            argv: vec!["claude".into()],
            env: env.iter().map(|kv| (*kv).to_owned()).collect(),
            ..Default::default()
        }
    }

    /// The exec read of a process under the login directory `dir`.
    fn login_under(dir: &str) -> Option<accounts::ProcessLogin> {
        let login = format!("{}={dir}", accounts::LOGIN_ENV);
        login_of_args(Some(&args_of(&["HOME=/home/someone", &login])))
    }

    /// A Claude home under a fresh temp root, with the registry row of `pid`
    /// naming the image `started_at`: `(root, home)`.
    fn claude_home(name: &str, pid: i32, started_at: u64) -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("aterm-gui-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (cwd, claude) = (root.join("work"), root.join("claude"));
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(claude.join("sessions")).unwrap();
        std::fs::write(
            claude.join(format!("sessions/{pid}.json")),
            format!(
                r#"{{"pid":{pid},"sessionId":"00000000-0000-4000-8000-000000000a01","cwd":"{}","startedAt":{},"version":"2.1.287"}}"#,
                cwd.display(),
                started_at * 1000 + 684
            ),
        )
        .unwrap();
        (root, claude)
    }

    /// The login is read only off an environment that WAS read: no read, and
    /// one that reads empty, name no login — "no variable set" would say the
    /// default one, and label a process nobody read. CONTROL: an environment
    /// with only `HOME` in it is the default login, and one that names a
    /// login names it.
    #[test]
    fn an_environment_that_was_not_read_names_no_login() {
        assert_eq!(login_of_args(None), None, "no read");
        assert_eq!(login_of_args(Some(&args_of(&[]))), None, "reads empty");
        let roster = two_accounts();
        let default = login_of_args(Some(&args_of(&["HOME=/home/someone"]))).expect("read");
        assert_eq!(accounts::footer_label(&roster, &default), Some("main"));
        let named = login_under("/logins/work-2").expect("read");
        assert_eq!(accounts::footer_label(&roster, &named), Some("work-2"));
        // The same read is the directory's, so what takes a process out of
        // the default home is seen here too.
        let own_home = login_of_args(Some(&args_of(&[
            "HOME=/home/someone",
            "CLAUDE_CONFIG_DIR=/elsewhere",
        ])))
        .expect("read");
        assert_eq!(accounts::footer_label(&roster, &own_home), None);
    }

    /// The resolver's roster is read on the first ask, answered from memory
    /// within [`ROSTER_RECHECK`] and read again after it: a footer read costs
    /// no file read of its own, and a roster edited under a running window
    /// shows within one period. CONTROLS: one row, a load that fails and an
    /// empty roster are no roster — and are not asked again within the
    /// period either.
    #[test]
    fn the_roster_is_read_at_most_every_recheck() {
        let t0 = std::time::Instant::now();
        let asked = std::cell::Cell::new(0);
        let mut memo = RosterMemo::default();
        let mut at = |now: std::time::Instant, answer: Option<accounts::Roster>| {
            memo.at(now, || {
                asked.set(asked.get() + 1);
                answer
            })
            .cloned()
        };
        let two = two_accounts();
        let mut three = two.clone();
        three.rows.push(accounts::Row {
            label: "pool-a".into(),
            class: accounts::Class::Shared,
            dir: Some("/logins/pool-a".into()),
        });
        let one = accounts::Roster {
            rows: two.rows[..1].to_vec(),
        };
        assert_eq!(at(t0, Some(two.clone())), Some(two.clone()));
        let within = t0 + ROSTER_RECHECK - Duration::from_millis(1);
        assert_eq!(at(within, Some(three.clone())), Some(two.clone()), "kept");
        assert_eq!(asked.get(), 1, "not read again within the period");
        let t1 = t0 + ROSTER_RECHECK;
        assert_eq!(at(t1, Some(three.clone())), Some(three), "read afresh");
        assert_eq!(asked.get(), 2);
        // NEGATIVE CONTROLS.
        let t2 = t1 + ROSTER_RECHECK;
        assert_eq!(at(t2, Some(one)), None, "one account: none to tell apart");
        assert_eq!(
            at(t2 + Duration::from_secs(1), Some(two.clone())),
            None,
            "no roster is an answer too, kept for the period"
        );
        assert_eq!(asked.get(), 3);
        let t3 = t2 + ROSTER_RECHECK;
        assert_eq!(at(t3, None), None, "a load that fails");
        let t4 = t3 + ROSTER_RECHECK;
        assert_eq!(
            at(t4, Some(accounts::Roster::default())),
            None,
            "none listed"
        );
        let t5 = t4 + ROSTER_RECHECK;
        assert_eq!(at(t5, Some(two.clone())), Some(two), "listed again");
        assert_eq!(asked.get(), 6);
    }

    /// The exec read of a process on this machine's own login.
    fn on_main(_: i32) -> Option<atpkg::caller_shell::ProcArgs> {
        Some(args_of(&["HOME=/home/someone"]))
    }

    /// The exec read of a process under `work-2`'s login directory, started
    /// as `claude --resume`.
    fn on_work(_: i32) -> Option<atpkg::caller_shell::ProcArgs> {
        let login = format!("{}=/logins/work-2", accounts::LOGIN_ENV);
        Some(atpkg::caller_shell::ProcArgs {
            argv: vec!["claude".into(), "--resume".into()],
            ..args_of(&["HOME=/home/someone", &login])
        })
    }

    /// An identity as [`resolve_and_post`] builds one: the directory, and
    /// nothing read of the image yet.
    fn unread(pid: i32, claude: &std::path::Path) -> Identity {
        Identity {
            pid,
            started: None,
            dir: Some(claude.to_path_buf()),
            launch: LaunchFacts::default(),
            launch_for: None,
            login: None,
            argv: Vec::new(),
        }
    }

    /// One read of `identity`: the account the timeline then shows — `None`
    /// for no footer, `Some(None)` for a footer that names no account.
    fn account_after(
        identity: &mut Identity,
        roster: &mut RosterMemo,
        asks: Asks,
    ) -> Option<Option<String>> {
        let timeline = Arc::new(Mutex::new(SessionTimeline::default()));
        timeline.lock().unwrap().note_foreground_group(identity.pid);
        resolve_into(
            &timeline,
            identity,
            &mut footer::TailCache::default(),
            &mut footer::FooterCache::default(),
            roster,
            asks,
        );
        let timeline = timeline.lock().unwrap();
        timeline.claude_footer().map(|facts| facts.account.clone())
    }

    /// THE ACCOUNT IS LAID IN BY THE READ THAT ANSWERS: the published facts
    /// carry the label of the row the process's login names. CONTROLS, each
    /// publishing the same facts with no account: a login no row names; no
    /// roster; an environment that reads empty, and an exec read that is
    /// refused, for which the roster is not asked at all — and the refused
    /// read is asked again at the next footer read, where its answer labels
    /// the process. A read with no answer (no registry row) asks neither the
    /// process nor the roster. No test reaches the machine's own roster or a
    /// real process: each hands its own answers.
    #[test]
    fn a_read_that_answers_is_given_its_account() {
        const PID: i32 = 4242;
        let (root, claude) = claude_home("footer-account", PID, 1_790_607_848);
        let listed = |args| Asks {
            args,
            roster: || Some(two_accounts()),
        };
        let unasked = |args| Asks {
            args,
            roster: || unreachable!("no login was read: the roster is not asked"),
        };
        let read = |asks: Asks| {
            let mut memo = RosterMemo::default();
            let account = account_after(&mut unread(PID, &claude), &mut memo, asks);
            (account, memo.read.is_some())
        };
        assert_eq!(read(listed(on_work)), (Some(Some("work-2".into())), true));
        assert_eq!(
            read(listed(on_main)),
            (Some(Some("main".into())), true),
            "the default login is the row with no directory"
        );
        // NEGATIVE CONTROLS: the footer is published, and names no account.
        let nobody = |_| {
            let login = format!("{}=/logins/nobody", accounts::LOGIN_ENV);
            Some(args_of(&["HOME=/home/someone", &login]))
        };
        assert_eq!(read(listed(nobody)), (Some(None), true), "no row names it");
        let none = Asks {
            args: on_main,
            roster: || None,
        };
        assert_eq!(read(none), (Some(None), true), "no roster: asked, none");
        assert_eq!(
            read(unasked(|_| Some(args_of(&[])))),
            (Some(None), false),
            "an environment that reads empty"
        );
        // An exec read that is refused: no label, and the next read asks the
        // process again.
        let mut identity = unread(PID, &claude);
        let mut memo = RosterMemo::default();
        assert_eq!(
            account_after(&mut identity, &mut memo, unasked(|_| None)),
            Some(None)
        );
        assert_eq!(
            identity.launch_for, None,
            "not read: nothing is kept for it"
        );
        assert_eq!(
            account_after(&mut identity, &mut memo, listed(on_main)),
            Some(Some("main".into())),
            "read at the next"
        );
        // A read with no answer (no registry row) asks nothing.
        std::fs::remove_file(claude.join(format!("sessions/{PID}.json"))).unwrap();
        let nothing = Asks {
            args: |_| unreachable!("no registry row: the process is not read"),
            roster: || unreachable!("no facts: the roster is not asked"),
        };
        assert_eq!(read(nothing), (None, false));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// THE LOGIN IS THE REGISTERED IMAGE'S. What the process was started with
    /// before Claude Code wrote its registry row may be a launcher's — a
    /// shim named `claude` that sets its variables and execs in place, the
    /// pid and the kernel's start kept — so the login and the command line
    /// are read at the first row, whatever the identity held, and again when
    /// a row names a new image. CONTROL: a row that names the same image
    /// reads nothing (the process is not asked), and keeps what was read.
    #[test]
    fn the_login_is_the_registered_images() {
        const PID: i32 = 4242;
        const IMAGE: u64 = 1_790_607_848;
        let (root, claude) = claude_home("footer-login", PID, IMAGE);
        let listed = |args| Asks {
            args,
            roster: || Some(two_accounts()),
        };
        let mut memo = RosterMemo::default();
        // What a read before the registry row held: the launcher's.
        let mut identity = Identity {
            login: login_of_args(on_main(PID).as_ref()),
            argv: vec!["shim".into()],
            ..unread(PID, &claude)
        };
        assert_eq!(
            account_after(&mut identity, &mut memo, listed(on_work)),
            Some(Some("work-2".into())),
            "the first row: read again"
        );
        assert_eq!(identity.launch_for, Some(Some(IMAGE)));
        assert_eq!(
            identity.argv,
            ["claude", "--resume"],
            "its command line too"
        );
        // NEGATIVE CONTROL: the same image.
        let same = listed(|_| unreachable!("the image was read: not again"));
        assert_eq!(
            account_after(&mut identity, &mut memo, same),
            Some(Some("work-2".into()))
        );
        // A new image in place.
        let _ = claude_home("footer-login", PID, IMAGE + 60);
        assert_eq!(
            account_after(&mut identity, &mut memo, listed(on_main)),
            Some(Some("main".into())),
            "a new image: read again"
        );
        assert_eq!(identity.launch_for, Some(Some(IMAGE + 60)));
        assert_eq!(identity.argv, ["claude"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A rule cell counts as the rule's only while it is the rule's own glyph
    /// in the rule's own colours with nothing in a side channel: a label in
    /// another colour, a selection's background and a cluster are ink.
    #[test]
    fn only_plain_rule_glyphs_are_coverable() {
        let mut s = RenderInput::empty();
        s.rows = 1;
        s.cols = 20;
        let mut row = row_of(&"\u{2500}".repeat(20), [60, 60, 60]);
        row[3].fg = [255, 0, 0]; // a label's ink
        row[6].bg = [0, 0, 200]; // a selection
        s.cells = vec![row];
        s.clusters = vec![vec![(9, "e\u{301}".into())]];
        s.combining = vec![Vec::new()];
        s.images = vec![Vec::new()];
        let (mask, rule) = rule_mask(&s, 0, 0, 20).unwrap();
        assert_eq!(rule.fg, [60, 60, 60]);
        // A highlight over the rule's END is not the rule's style.
        let mut lit = s.clone();
        for c in &mut lit.cells[0][17..] {
            c.bg = [0, 0, 200];
        }
        let (mask_lit, rule_lit) = rule_mask(&lit, 0, 0, 20).unwrap();
        assert_eq!(
            rule_lit.bg,
            [0, 0, 0],
            "the style most of the rule is drawn in"
        );
        assert_eq!(footer::rule_run(&mask_lit), Some(10..17));
        let ink: Vec<usize> = (0..20).filter(|&c| !mask[c]).collect();
        assert_eq!(ink, [3, 6, 9]);
        assert_eq!(footer::rule_run(&mask), Some(10..20));
        // A trimmed row: the missing cells are no rule.
        s.cells[0].truncate(12);
        let (mask, _) = rule_mask(&s, 0, 0, 20).unwrap();
        assert!(!mask[12] && mask[11]);
        // No rule glyph at all: no mask.
        s.cells[0] = row_of("  hello", [1, 1, 1]);
        assert!(rule_mask(&s, 0, 0, 20).is_none());
    }

    /// A scratch as an engine fill leaves it: every reuse token armed, a live
    /// revision lane, rows trimmed of trailing blanks.
    fn engine_fill(rows: usize) -> RenderInput {
        let mut s = RenderInput::empty();
        s.rows = rows;
        s.cols = 40;
        s.cells = (0..rows)
            .map(|r| row_of(&format!("engine row {r}"), [9, 9, 9]))
            .collect();
        s.clusters = (0..rows).map(|_| Vec::new()).collect();
        s.combining = (0..rows).map(|_| Vec::new()).collect();
        s.images = (0..rows).map(|_| Vec::new()).collect();
        s.snapshot_seq = 40;
        s.engine_fill_seq = 40;
        s.shifted_fill_seq = 40;
        s.terminal_id = 7;
        s.extract_gen = 3;
        s.row_rev_lane = 7;
        s.row_rev = (0..rows).map(|r| 100 + r as u64).collect();
        s
    }

    /// The whole point: the footer comes off exactly, and the scratch is an
    /// ENGINE FILL again — the state DMG-1's scoped refill, the effect-only
    /// reuse and D-2's stamp compare all require.
    #[test]
    fn the_undo_gives_back_the_engine_fill_exactly() {
        let mut s = engine_fill(4);
        let original = s.cells.clone();
        let painted = row_of("  \u{25C6} Opus 5.5 xhigh", [1, 1, 1]);
        let mut slot = write_edits(&mut s, vec![(3, 0, painted.clone(), cell(' ', [0, 0, 0]))]);
        assert!(slot.is_some());
        assert_eq!(&s.cells[3][..painted.len()], painted.as_slice());
        assert_ne!(s.snapshot_seq, s.engine_fill_seq, "the write is declared");
        assert_eq!(
            s.shifted_fill_seq, s.snapshot_seq,
            "an untouched engine fill keeps the renderer's stamp compare"
        );
        assert_eq!(s.row_rev[3], 0, "the written row is compared by content");
        assert_eq!(s.row_rev[2], 102, "every other row keeps its stamp");
        assert!(undo(&mut slot, &mut s));
        assert!(slot.is_none(), "consumed");
        assert_eq!(s.cells, original, "cells and trimmed lengths restored");
        assert_eq!(s.snapshot_seq, 40);
        assert_eq!(s.engine_fill_seq, 40);
        assert_eq!(s.shifted_fill_seq, 40);
        assert_eq!(s.row_rev, vec![100, 101, 102, 103]);
    }

    /// Anything else writing after the footer — or a refill that moved no
    /// token but changed the cells — and the undo changes nothing.
    #[test]
    fn a_scratch_touched_since_is_left_alone() {
        let painted = row_of("  footer", [1, 1, 1]);
        let mut s = engine_fill(3);
        let mut slot = write_edits(&mut s, vec![(2, 0, painted.clone(), cell(' ', [0, 0, 0]))]);
        s.snapshot_seq += 1; // another host write
        let before = (s.cells.clone(), s.snapshot_seq, s.row_rev.clone());
        assert!(!undo(&mut slot, &mut s));
        assert_eq!((s.cells.clone(), s.snapshot_seq, s.row_rev.clone()), before);
        assert!(slot.is_none(), "a stale record is dropped");

        let mut s = engine_fill(3);
        let mut slot = write_edits(&mut s, vec![(2, 0, painted, cell(' ', [0, 0, 0]))]);
        s.cells[2][3].ch = 'X';
        assert!(
            !undo(&mut slot, &mut s),
            "the painted span must still be ours"
        );
        assert_eq!(s.cells[2][3].ch, 'X');
    }

    /// Two panes side by side write one frame row; the undo peels them off
    /// newest first, down to the engine's trimmed length.
    #[test]
    fn two_writes_on_one_row_come_off_in_order() {
        let mut s = engine_fill(2);
        let original = s.cells.clone();
        let left = row_of("  left footer      ", [1, 1, 1]);
        let right = row_of("  right footer", [2, 2, 2]);
        let ground = cell(' ', [0, 0, 0]);
        let mut slot = write_edits(&mut s, vec![(1, 0, left, ground), (1, 20, right, ground)]);
        assert_eq!(s.cells[1].len(), 34, "padded to the right pane's edge");
        assert!(undo(&mut slot, &mut s));
        assert_eq!(s.cells, original);
    }

    /// THE RULE ROW: a span written into the middle of a full-width rule
    /// (no padding: the rule reaches the pane's edge) comes off exactly, the
    /// row's D-2 stamp with it; and a span past a row the engine TRIMMED is
    /// padded with the pane's blank — never smeared with a painted glyph —
    /// and the undo gives the trimmed length back.
    #[test]
    fn a_span_in_the_rule_row_comes_off_exactly() {
        let mut s = engine_fill(4);
        s.cells[2] = row_of(&"\u{2500}".repeat(40), [60, 60, 60]);
        let original = s.cells.clone();
        let facts = footer::fit_rule(&facts(), 36, None);
        let blank = cell(' ', [200, 200, 200]);
        let (cells, _) = compose_rule(&facts, None, s.cells[2][0], blank, [0, 128, 255], 36);
        let col = 40 - 1 - cells.len();
        let mut slot = write_edits(&mut s, vec![(2, col, cells.clone(), blank)]);
        assert_eq!(s.cells[2].len(), 40, "nothing padded");
        assert_eq!(&s.cells[2][col..39], cells.as_slice());
        assert_eq!(
            s.cells[2][39], original[2][39],
            "the rule's last glyph stays"
        );
        assert!(s.cells[2][..col].iter().all(|c| *c == original[2][0]));
        assert_eq!(s.row_rev[2], 0, "the rule row is compared by content");
        assert!(undo(&mut slot, &mut s));
        assert_eq!(s.cells, original);
        assert_eq!(s.row_rev, vec![100, 101, 102, 103]);
        // Past a trimmed row: padded with the pane's blank.
        let mut s = engine_fill(2);
        let original = s.cells.clone();
        let chip = cell('\u{23F8}', [9, 9, 9]);
        let mut slot = write_edits(&mut s, vec![(1, 30, vec![chip], blank)]);
        assert!(
            s.cells[1][12..30].iter().all(|c| *c == blank),
            "{:?}",
            &s.cells[1][12..30]
        );
        assert!(undo(&mut slot, &mut s));
        assert_eq!(s.cells, original);
    }

    /// A span holding a grapheme cluster is not the plain row the planner
    /// read, and the undo could not restore a removed cluster: no write.
    #[test]
    fn a_span_with_side_channel_content_is_not_written() {
        let mut s = engine_fill(2);
        s.clusters[1].push((4, "e\u{301}".into()));
        let before = s.cells.clone();
        assert!(
            write_edits(
                &mut s,
                vec![(1, 0, row_of("  footer", [1, 1, 1]), cell(' ', [0, 0, 0]))]
            )
            .is_none()
        );
        assert_eq!(s.cells, before);
        assert_eq!(s.snapshot_seq, 40, "nothing written, nothing declared");
    }

    /// A scratch some other host already wrote is not re-blessed: the
    /// renderer's stamp compare stays off, exactly as that write left it.
    #[test]
    fn only_an_untouched_engine_fill_keeps_its_stamps() {
        let mut s = engine_fill(2);
        s.snapshot_seq = 41; // e.g. the find bar, earlier this frame
        let mut slot = write_edits(
            &mut s,
            vec![(1, 0, row_of("  footer", [1, 1, 1]), cell(' ', [0, 0, 0]))],
        );
        assert_eq!(s.shifted_fill_seq, 40, "not re-armed");
        assert_eq!(s.row_rev[1], 101, "not cleared");
        assert!(undo(&mut slot, &mut s));
        assert_eq!(s.snapshot_seq, 41, "back to the other write's own bump");
    }

    /// Under a blessed chrome prepend (the tab strip, a band row) the footer
    /// keeps the renderer's stamp compare on too — and the order at the one
    /// production call site (the footer's undo, then the strip's inverse)
    /// still gives back the engine fill exactly.
    #[test]
    fn a_blessed_chrome_prepend_keeps_the_stamps_and_its_inverse() {
        let mut s = engine_fill(3);
        let engine_cells = s.cells.clone();
        // The prepend, as `prepend_strip_rows` makes it.
        let blessed = s.host_prepend_blessing();
        assert!(blessed);
        s.cells.insert(0, row_of("strip", [7, 7, 7]));
        s.clusters.insert(0, Vec::new());
        s.combining.insert(0, Vec::new());
        s.images.insert(0, Vec::new());
        s.rows += 1;
        s.snapshot_seq += 1;
        s.note_host_row_prepend(1, blessed);
        let mut slot = write_edits(
            &mut s,
            vec![(3, 0, row_of("  footer", [1, 1, 1]), cell(' ', [0, 0, 0]))],
        );
        assert_eq!(
            s.shifted_fill_seq, s.snapshot_seq,
            "the stamp compare stays on under the strip"
        );
        assert_eq!(s.row_rev[3], 0, "the written row is compared by content");
        assert!(undo(&mut slot, &mut s));
        let mut pool = Vec::new();
        assert!(
            s.undo_host_row_prepend(3, &mut pool, 1),
            "the strip's inverse still runs"
        );
        assert_eq!(s.cells, engine_cells);
        assert_eq!(s.snapshot_seq, s.engine_fill_seq);
    }

    /// The drift check names only a pill-shaped row this build cannot read:
    /// never a screen the footer reads, never a box, never plain text.
    #[test]
    fn only_an_unreadable_pill_row_is_drift() {
        let screen = |mode_row: &str| -> Vec<String> {
            let rule = "\u{2500}".repeat(60);
            vec![rule.clone(), "\u{276F} ".into(), rule, mode_row.into()]
        };
        let renamed = "  \u{23F5}\u{23F5} turbo mode on (shift+tab to cycle)";
        assert_eq!(unread_pill_row(&screen(renamed)).as_deref(), Some(renamed));
        for fine in [
            "  \u{23F5}\u{23F5} auto mode on (shift+tab to cycle)",
            "  \u{23F8} manual mode on \u{00B7} ? for shortcuts",
            "  ? for shortcuts",
            "  \u{23F5}\u{23F5} bypass permissi",
        ] {
            assert_eq!(unread_pill_row(&screen(fine)), None, "{fine:?}");
        }
        assert_eq!(unread_pill_row(&["$ ls".to_owned()]), None, "no composer");
    }

    // ---- ON THE GLASS: a real engine, a real frame, the real splice ----

    /// The measured 2.1.283 screen `name` (`src/fixtures` of aterm-phase,
    /// 80x24), at `cols` columns: every full-width rule rebuilt at that
    /// width, its label kept at the right end.
    fn claude_rows(name: &str, cols: usize) -> Vec<String> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../aterm-phase/src/fixtures")
            .join(format!("claude-2.1.283-{name}.txt"));
        let rows: Vec<String> = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
            .lines()
            .skip(1)
            .map(|row| {
                let rule = row.starts_with('\u{2500}') && row.trim_end().ends_with('\u{2500}');
                if !rule {
                    return row.to_owned();
                }
                let label: String = row.trim_start_matches('\u{2500}').to_owned();
                let n = cols - label.chars().count();
                format!("{}{label}", "\u{2500}".repeat(n))
            })
            .collect();
        assert!(rows.len() <= 24, "{name}: an 80x24 capture");
        rows
    }

    /// The SGR a row is drawn in, as Claude draws it: rules dim, a rule's
    /// label and the mode pill in hues of their own, the rest of the mode
    /// row dim — so a cell the footer wrongly took shows up in its colour.
    fn painted(row: &str) -> String {
        const RULE: &str = "\x1b[38;2;90;90;90m";
        const LABEL: &str = "\x1b[38;2;200;120;255m";
        const PILL: &str = "\x1b[38;2;255;110;90m";
        const DIM: &str = "\x1b[38;2;130;130;130m";
        if row.starts_with('\u{2500}') {
            let rule: String = row.chars().take_while(|c| *c == '\u{2500}').collect();
            let label = &row[rule.len()..];
            let (label, end) = label
                .strip_suffix('\u{2500}')
                .map_or((label, ""), |l| (l, "\u{2500}"));
            return format!("{RULE}{rule}{LABEL}{label}{RULE}{end}\x1b[0m");
        }
        if footer::is_mode_row(row) {
            let at = row.find(" on").map_or(row.len(), |i| i + 3);
            return format!("{PILL}{}{DIM}{}\x1b[0m", &row[..at], &row[at..]);
        }
        row.to_owned()
    }

    fn feed_rows(app: &App, session: u64, rows: &[String]) {
        let mut out = String::from("\x1b[?2004h\x1b[2J");
        for (i, row) in rows.iter().enumerate() {
            out.push_str(&format!("\x1b[{};1H{}", i + 1, painted(row)));
        }
        let caret = rows
            .iter()
            .rposition(|r| r.starts_with('\u{276F}'))
            .unwrap_or(0);
        out.push_str(&format!("\x1b[{};3H", caret + 1));
        let term = app.pool.get(session).expect("session").term.clone();
        crate::term_lock(&term).process(out.as_bytes());
    }

    fn publish(app: &App, session: u64, facts: FooterFacts) {
        let entry = app.pool.get(session).expect("session");
        let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        timeline.note_foreground_group(4242);
        assert!(timeline.set_claude_footer(4242, None, Some(facts)));
    }

    /// One frame through the capture route's splices (the terminal arm of
    /// `App::render_image`): extract, the strip, then the footer.
    fn frame(app: &mut App, wid: WindowId) {
        let prepared = app.prepare_terminal_capture_grid_with_cursor_fx_and_plan_outcome(
            wid,
            crate::app_render::ComposedCursorFxClock::Advance(std::time::Instant::now()),
        );
        let crate::app_render::CapturePreparation::Ready((grid, plan)) = prepared else {
            panic!("the headless capture must produce a frame");
        };
        app.splice_tab_strip(wid);
        app.splice_claude_footer(
            wid,
            &plan,
            VisibleContentRoute::Terminal {
                composed: grid.composed,
            },
        );
    }

    fn glass(cols: u16) -> (App, WindowId, u64) {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let session = app.focused_session_id(wid).expect("the front session");
        if cols != 80 {
            assert!(app.apply_term_resize(wid, 24, cols));
        }
        (app, wid, session)
    }

    fn strip(app: &App, wid: WindowId) -> usize {
        app.windows[&wid].input_scratch.cells.len() - usize::from(app.windows[&wid].rows)
    }

    /// THE OWNER'S LAYOUT ON THE GLASS (2026-09-28), over the measured
    /// 2.1.283 screens — bypass at rest, a busy auto turn, plan and manual
    /// mode, default mode's pill-less row — at 80, 131 and 150 columns: the
    /// rule under the input box reads `<rule> ◆ Opus 5.5 xhigh   ⌂ ~/aterm
    /// ⎇ main ─`, the marks in the accent and the values in the pane's ink;
    /// only plain rule glyphs were covered; and every cell of Claude's own
    /// footer row — pill, `(shift+tab to cycle)`, `← for agents`, `esc to
    /// interrupt`, colours included — is byte for byte what the vendor drew.
    #[test]
    fn the_facts_go_into_the_rule_and_claudes_row_stays_whole() {
        let footer_text = " \u{25C6} Opus 5.5 xhigh   \u{2302} ~/aterm   \u{2387} main \u{2500}";
        for (name, cols, default_mode) in [
            ("footer-bypass-idle", 80, false),
            ("footer-bypass-idle", 131, false),
            ("footer-bypass-idle", 150, false),
            ("footer-busy", 80, false),
            ("footer-busy", 131, false),
            ("footer-plan", 80, false),
            ("footer-manual", 150, false),
            ("footer-accept-edits", 80, false),
            ("footer-auto-draft", 131, false),
            ("footer-bypass-idle", 80, true),
        ] {
            let label = format!(
                "{name}@{cols}{}",
                if default_mode { " (default mode)" } else { "" }
            );
            let (mut app, wid, session) = glass(cols);
            let mut rows = claude_rows(name, usize::from(cols));
            let bottom = aterm_phase::phase::composer_bottom(&rows).expect("the composer");
            if default_mode {
                rows[bottom + 1] = "  ? for shortcuts".into();
            }
            feed_rows(&app, session, &rows);
            frame(&mut app, wid);
            let strip = strip(&app, wid);
            let vendor: Vec<Vec<RenderCell>> = app.windows[&wid].input_scratch.cells.clone();
            publish(&app, session, facts());
            frame(&mut app, wid);
            let glass = &app.windows[&wid].input_scratch.cells;
            let rule = &glass[strip + bottom];
            assert!(
                text_of(rule).trim_end().ends_with(footer_text),
                "{label}: {:?}",
                text_of(rule)
            );
            let at = rule.iter().position(|c| c.ch == '\u{25C6}').unwrap();
            assert_eq!(rule.len(), usize::from(cols), "{label}");
            let set = footer_text.trim_end_matches('\u{2500}');
            assert_eq!(
                at,
                usize::from(cols) - cells_for(set),
                "{label}: right-aligned, the rule's last glyph after it"
            );
            let o = rule.iter().position(|c| c.ch == 'O').unwrap();
            let glyph = *vendor[strip + bottom].last().unwrap();
            let ink = app.windows[&wid].input_scratch.implicit_blank.fg;
            assert_eq!(
                rule[o].fg,
                mix(glyph.fg, ink, VALUE_LIFT),
                "{label}: the value, in the rule's gray lifted toward the pane's ink"
            );
            assert!(
                matches!(rule[o].script, aterm_core::terminal::Script::Sub { .. }),
                "{label}: small and low"
            );
            for (c, (was, now)) in vendor[strip + bottom].iter().zip(rule).enumerate() {
                if was != now {
                    assert_eq!(*was, glyph, "{label}: col {c} was not a plain rule glyph");
                }
            }
            assert_eq!(
                rule.last(),
                Some(&glyph),
                "{label}: the rule's last glyph stays"
            );
            for r in bottom + 1..rows.len() {
                assert_eq!(
                    glass[strip + r],
                    vendor[strip + r],
                    "{label}: row {r} under the rule is Claude's, every cell"
                );
            }
            for r in 0..bottom {
                assert_eq!(glass[strip + r], vendor[strip + r], "{label}: row {r}");
            }
        }
        // The usage switch (`[claude_footer] usage`, default on): `Σ` shows
        // beside a standing limit wall — and `usage = false` takes `Σ` away,
        // the wall staying.
        let (mut app, wid, session) = glass(150);
        let rows = claude_rows("footer-bypass-idle", 150);
        let bottom = aterm_phase::phase::composer_bottom(&rows).expect("the composer");
        feed_rows(&app, session, &rows);
        publish(&app, session, facts_with_usage());
        frame(&mut app, wid);
        let rule =
            |app: &App| text_of(&app.windows[&wid].input_scratch.cells[strip(app, wid) + bottom]);
        let shown = rule(&app);
        assert!(
            shown.contains('\u{29D7}') && shown.contains('\u{03A3}'),
            "{shown:?}"
        );
        app.config.claude_footer =
            Some(crate::app_config::ClaudeFooterConfig { usage: Some(false) });
        frame(&mut app, wid);
        let hidden = rule(&app);
        assert!(
            hidden.contains('\u{29D7}') && !hidden.contains('\u{03A3}'),
            "{hidden:?}"
        );
    }

    /// Nothing is painted where no composer is drawn: a dialog or a picker
    /// that replaced it (the effort-switch box). CONTROL: the same facts on
    /// the idle screen are painted.
    #[test]
    fn no_composer_no_footer() {
        let (mut app, wid, session) = glass(80);
        let boxed: Vec<String> = [
            "\u{25CF} done",
            "",
            &"\u{2594}".repeat(80),
            "   Change effort level?",
            "",
            "   \u{276F} 1. Yes, switch to high",
            "     2. No, go back",
        ]
        .iter()
        .map(|r| (*r).to_owned())
        .collect();
        feed_rows(&app, session, &boxed);
        publish(&app, session, facts());
        frame(&mut app, wid);
        assert!(
            !app.windows[&wid]
                .input_scratch
                .cells
                .iter()
                .any(|row| row.iter().any(|c| c.ch == '\u{25C6}')),
            "no footer anywhere"
        );
        feed_rows(&app, session, &claude_rows("footer-bypass-idle", 80));
        frame(&mut app, wid);
        assert!(
            app.windows[&wid]
                .input_scratch
                .cells
                .iter()
                .any(|row| row.iter().any(|c| c.ch == '\u{25C6}'))
        );
    }

    /// A SPLIT PANE: the footer goes into the Claude pane's own rule, inside
    /// its columns, and gives way to its width (a half-width rule cuts a
    /// long path to its last directory); the sibling's cells on that row are
    /// untouched.
    #[test]
    fn a_split_panes_rule_carries_its_own_footer() {
        let (mut app, wid, session) = glass(80);
        let _other = app.split_active_stub_tab(wid);
        let (pane_rows, cols) = {
            let term = crate::term_lock(&app.pool.get(session).unwrap().term);
            (usize::from(term.rows()), usize::from(term.cols()))
        };
        assert!(cols < 60, "a half-width pane: {cols}");
        // The pane's content sits below its subtab title row: its last row is
        // the window's last row whatever the header takes from the top.
        let last = usize::from(app.windows[&wid].rows) - 1;
        assert!(
            pane_rows < last + 1,
            "a split pane gives a row to its title"
        );
        let rule = "\u{2500}".repeat(cols);
        // Bypass: at rest, no light is drawn.
        let rows: Vec<String> = (0..pane_rows - 4)
            .map(|_| String::new())
            .chain([
                rule.clone(),
                "\u{276F} ".to_owned(),
                rule,
                "  \u{23F5}\u{23F5} bypass permissions on".to_owned(),
            ])
            .collect();
        feed_rows(&app, session, &rows);
        frame(&mut app, wid);
        let strip = strip(&app, wid);
        let before = app.windows[&wid].input_scratch.cells[strip + last - 1].clone();
        let mode_before = app.windows[&wid].input_scratch.cells[strip + last].clone();
        let deep = FooterFacts {
            path: Some("~/src/github.com/someone/aterm".into()),
            ..facts()
        };
        publish(&app, session, deep);
        frame(&mut app, wid);
        let row = &app.windows[&wid].input_scratch.cells[strip + last - 1];
        let text = text_of(&row[..cols]);
        assert!(
            text.ends_with(
                " \u{25C6} Opus 5.5 xhigh   \u{2302} \u{2026}/aterm   \u{2387} main \u{2500}"
            ),
            "{text:?}"
        );
        assert_eq!(&row[cols..], &before[cols..], "the sibling's cells");
        assert_eq!(
            app.windows[&wid].input_scratch.cells[strip + last],
            mode_before,
            "Claude's row"
        );
    }

    /// The launch card fills what nothing since the start named — read off
    /// the owner's measured fresh screen, kept for the process — and a
    /// transcript's word outranks it. NEGATIVE CONTROLS: a card of another
    /// build; a new owner (an in-place exec, `/clear`) drops the kept card;
    /// the kept card outlives its scrolling away; noting it repaints.
    #[test]
    fn a_launch_card_fills_only_an_open_footer() {
        let (mut app, wid, session) = glass(80);
        let owner = footer::FactsOwner {
            floor: Some(1_790_607_848),
            session_id: "00000000-0000-4000-8000-000000000004".into(),
        };
        let open = FooterFacts {
            path: Some("~/aterm".into()),
            version: Some("2.1.283".into()),
            model_open: true,
            effort_open: true,
            owner: Some(owner.clone()),
            ..FooterFacts::default()
        };
        let rows = claude_rows("footer-bypass-idle", 80);
        let bottom = aterm_phase::phase::composer_bottom(&rows).unwrap();
        feed_rows(&app, session, &rows);
        publish(&app, session, open.clone());
        let plan = app.active_visible_leaf_plan(wid).unwrap();
        let fp_before = app.claude_footer_fp(wid, &plan);
        frame(&mut app, wid);
        let strip = strip(&app, wid);
        let rule = |app: &App| text_of(&app.windows[&wid].input_scratch.cells[strip + bottom]);
        assert!(
            rule(&app).ends_with(" \u{25C6} Opus 5.5 xhigh   \u{2302} ~/aterm \u{2500}"),
            "{:?}",
            rule(&app)
        );
        assert_ne!(
            app.claude_footer_fp(wid, &plan),
            fp_before,
            "noting it repaints"
        );
        // The card scrolls away; the kept card still speaks.
        let mut scrolled = rows.clone();
        for r in scrolled.iter_mut().take(5) {
            r.clear();
        }
        feed_rows(&app, session, &scrolled);
        frame(&mut app, wid);
        assert!(
            rule(&app).contains("\u{25C6} Opus 5.5 xhigh"),
            "{:?}",
            rule(&app)
        );
        // A new owner drops it.
        let entry = app.pool.get(session).unwrap();
        {
            let mut timeline = entry.ctx.timeline.lock().unwrap();
            assert!(timeline.set_claude_footer(
                4242,
                None,
                Some(FooterFacts {
                    owner: Some(footer::FactsOwner {
                        floor: Some(1_790_607_999),
                        ..owner.clone()
                    }),
                    ..open.clone()
                })
            ));
            assert!(timeline.claude_card().is_none());
        }
        frame(&mut app, wid);
        assert!(!rule(&app).contains('\u{25C6}'), "{:?}", rule(&app));
        // A transcript's choice outranks the card.
        feed_rows(&app, session, &rows);
        publish(
            &app,
            session,
            FooterFacts {
                model: Some("Fable 5.1".into()),
                model_open: false,
                ..open.clone()
            },
        );
        frame(&mut app, wid);
        assert!(
            rule(&app).contains("\u{25C6} Fable 5.1 xhigh"),
            "{:?}",
            rule(&app)
        );
        // A card of another build is not this process's.
        let (mut app, wid, session) = glass(80);
        feed_rows(&app, session, &rows);
        publish(
            &app,
            session,
            FooterFacts {
                version: Some("2.1.284".into()),
                ..open
            },
        );
        frame(&mut app, wid);
        let text = text_of(&app.windows[&wid].input_scratch.cells[strip + bottom]);
        assert!(!text.contains("Opus"), "{text:?}");
    }

    /// THE NEWEST CARD WINS: while the facts stay open the card is read on
    /// every frame, and a newer reading replaces the kept one — a
    /// predecessor's card of the same build, read in the moment between the
    /// new owner and the new process's first paint, gives way the moment the
    /// new process draws its own. A frame with no card to read (it scrolled
    /// away) keeps the newest reading. NEGATIVE CONTROL: the stale reading
    /// is what the footer showed first (the memo read once kept it for good).
    #[test]
    fn a_newer_launch_card_replaces_the_kept_one() {
        let (mut app, wid, session) = glass(80);
        let open = FooterFacts {
            path: Some("~/aterm".into()),
            version: Some("2.1.283".into()),
            model_open: true,
            effort_open: true,
            owner: Some(footer::FactsOwner {
                floor: Some(1_790_607_848),
                session_id: "00000000-0000-4000-8000-000000000005".into(),
            }),
            ..FooterFacts::default()
        };
        let rows = claude_rows("footer-bypass-idle", 80);
        let bottom = aterm_phase::phase::composer_bottom(&rows).unwrap();
        // The predecessor's card, still on the glass: `Haiku 4.5`, no effort.
        let stale: Vec<String> = rows
            .iter()
            .map(|r| r.replace("Opus 5.5 with xhigh effort", "Haiku 4.5"))
            .collect();
        assert_ne!(stale, rows, "the fixture's card names Opus 5.5");
        feed_rows(&app, session, &stale);
        publish(&app, session, open);
        frame(&mut app, wid);
        let strip = strip(&app, wid);
        let rule = |app: &App| text_of(&app.windows[&wid].input_scratch.cells[strip + bottom]);
        assert!(
            rule(&app).contains("\u{25C6} Haiku 4.5   \u{2302} ~/aterm"),
            "the stale reading shows first: {:?}",
            rule(&app)
        );
        // The new process draws its own card: the newer reading wins.
        feed_rows(&app, session, &rows);
        frame(&mut app, wid);
        assert!(
            rule(&app).contains("\u{25C6} Opus 5.5 xhigh   \u{2302} ~/aterm"),
            "{:?}",
            rule(&app)
        );
        // Its card scrolls away: the newest reading stands.
        let mut scrolled = rows.clone();
        for r in scrolled.iter_mut().take(5) {
            r.clear();
        }
        feed_rows(&app, session, &scrolled);
        frame(&mut app, wid);
        assert!(
            rule(&app).contains("\u{25C6} Opus 5.5 xhigh"),
            "{:?}",
            rule(&app)
        );
    }

    /// A MOVE THE PERSON HOLDS, ON THE GLASS (the owner, 2026-09-30,
    /// s-61dfc15a: the agent answered READY, the restart waited on the owner
    /// at the tab, and nothing on the screen said so): the upgrade host's row
    /// for the tab puts its note first in the rule under the composer, in the
    /// rule's mark ink, and the frame owes a repaint for it — in the words the
    /// composer calls for as the engine has it: what holds the move while the
    /// composer holds no draft (Claude Code's dim suggestion is none), the
    /// draft's while one stands, through the look that finds the person
    /// paused with it, and none once a row the draft held finds the composer
    /// empty, nor while a turn runs. A look that only restamps the person
    /// changes nothing a frame shows. The words are aterm-agent's
    /// (`Row::pause_note`, pinned there); this pins where they go. NEGATIVE
    /// CONTROLS: it goes once no look has seen the person, once the move is
    /// under way, and for any other wait — the facts staying whole each time.
    #[test]
    fn a_move_the_person_holds_leads_the_rule_until_it_goes() {
        use aterm_agent::harness::upgrade::Phase;
        use aterm_agent::harness::upgrade_drive::Row;
        let (mut app, wid, session) = glass(150);
        let rows = claude_rows("footer-bypass-idle", 150);
        let bottom = aterm_phase::phase::composer_bottom(&rows).unwrap();
        let caret = rows
            .iter()
            .rposition(|r| r.starts_with('\u{276F}'))
            .unwrap();
        // The caret row as Claude Code draws it, the cursor where the
        // fixture's is: its suggestion dim, a person's draft not.
        let compose = |app: &App, text: &str, dim: bool| {
            let sgr = if dim { "\x1b[2m" } else { "" };
            let row = caret + 1;
            let out = format!("\x1b[{row};1H\x1b[2K\u{276F} {sgr}{text}\x1b[0m\x1b[{row};3H");
            let term = app.pool.get(session).expect("session").term.clone();
            crate::term_lock(&term).process(out.as_bytes());
        };
        let suggestion = "Try \"refactor lib.rs\"";
        feed_rows(&app, session, &rows);
        compose(&app, suggestion, true);
        publish(&app, session, facts());
        let tab = {
            let store = app.store.read().unwrap_or_else(|p| p.into_inner());
            store
                .by_local(session)
                .expect("the front session")
                .sid
                .as_str()
                .to_string()
        };
        let now = crate::upgrade_host::now_s();
        let held = Row {
            session: "conv".into(),
            tab,
            from: "2.1.285".into(),
            to: "2.1.286".into(),
            source: "managed".into(),
            phase: Phase::Announced {
                at_s: now - 600,
                asks: 1,
            },
            behind_since: now - (5 * 3_600 + 38 * 60),
            wait: "attended".into(),
            wait_since: now - 20,
            wait_seen: now - 5,
            person_named: true,
            ready: true,
            ..Row::default()
        };
        let note = held.pause_note(now).expect("held by the person: noted");
        let using = note.using.as_ref().expect("an empty composer's words");
        let plan = app.active_visible_leaf_plan(wid).unwrap();
        let fp_before = app.claude_footer_fp(wid, &plan);
        app.apply_agent_upgrades(vec![held.clone()]);
        let fp_held = app.claude_footer_fp(wid, &plan);
        assert_ne!(fp_held, fp_before, "the note repaints");
        frame(&mut app, wid);
        let strip = strip(&app, wid);
        let rule = |app: &App| app.windows[&wid].input_scratch.cells[strip + bottom].clone();
        let facts_text = "\u{25C6} Opus 5.5 xhigh   \u{2302} ~/aterm   \u{2387} main \u{2500}";
        let led = |words: &str| format!(" \u{21E3} {words}   {facts_text}");
        let text = text_of(&rule(&app));
        assert!(text.ends_with(&led(&using.whole)), "{text:?}");
        let cells = rule(&app);
        let at = cells.iter().position(|c| c.ch == '\u{21E3}').unwrap();
        // The note's mark is one of the rule's marks: the same ink as the
        // model's, not the values' gray.
        let model_mark = cells.iter().position(|c| c.ch == '\u{25C6}').unwrap();
        assert_eq!(cells[at].fg, cells[model_mark].fg, "the note's mark");
        let word = at + 1 + cells[at + 1..].iter().position(|c| c.ch != ' ').unwrap();
        assert_ne!(cells[at].fg, cells[word].fg, "a mark, not a value");
        // A look that only restamps the person: no frame owed.
        app.apply_agent_upgrades(vec![Row {
            wait_seen: now,
            ..held.clone()
        }]);
        assert_eq!(app.claude_footer_fp(wid, &plan), fp_held, "no churn");
        // A person's draft in the composer: the draft's words — and the same
        // through the look that finds them paused with it.
        compose(&app, "fix the build", false);
        frame(&mut app, wid);
        let text = text_of(&rule(&app));
        assert!(text.ends_with(&led(&note.draft.whole)), "typing: {text:?}");
        let paused = Row {
            wait: "draft".into(),
            wait_seen: 0,
            person_named: false,
            ..held.clone()
        };
        app.apply_agent_upgrades(vec![paused.clone()]);
        frame(&mut app, wid);
        let text = text_of(&rule(&app));
        assert!(text.ends_with(&led(&note.draft.whole)), "paused: {text:?}");
        // The draft sent or cleared: nothing for an empty composer until the
        // next look says what holds the move.
        for (why, text, dim) in [("cleared", "", false), ("its suggestion", suggestion, true)] {
            compose(&app, text, dim);
            frame(&mut app, wid);
            let text = text_of(&rule(&app));
            assert!(!text.contains('\u{21E3}'), "{why}: {text:?}");
            assert!(text.ends_with(&format!(" {facts_text}")), "{why}: {text:?}");
        }
        // A turn running holds the move, whatever the row says of the person.
        let busy = claude_rows("footer-busy", 150);
        let busy_bottom = aterm_phase::phase::composer_bottom(&busy).unwrap();
        feed_rows(&app, session, &busy);
        app.apply_agent_upgrades(vec![held.clone()]);
        frame(&mut app, wid);
        let text = text_of(&app.windows[&wid].input_scratch.cells[strip + busy_bottom]);
        assert!(!text.contains('\u{21E3}'), "a turn runs: {text:?}");
        assert!(text.contains(facts_text), "a turn runs: {text:?}");
        feed_rows(&app, session, &rows);
        compose(&app, suggestion, true);
        // NEGATIVE CONTROLS: each takes the note down, the facts staying.
        for (why, gone) in [
            (
                "no look has seen the person lately",
                Row {
                    person_named: false,
                    ..held.clone()
                },
            ),
            (
                "the move is under way",
                Row {
                    phase: Phase::Exiting { at_s: now },
                    ..held.clone()
                },
            ),
            (
                "its own work holds it",
                Row {
                    wait: "not-idle:busy".into(),
                    ..held.clone()
                },
            ),
        ] {
            app.apply_agent_upgrades(vec![held.clone()]);
            frame(&mut app, wid);
            assert!(text_of(&rule(&app)).contains('\u{21E3}'), "{why}: held");
            app.apply_agent_upgrades(vec![gone]);
            assert_ne!(app.claude_footer_fp(wid, &plan), fp_held, "{why}: repaints");
            frame(&mut app, wid);
            let text = text_of(&rule(&app));
            assert!(!text.contains('\u{21E3}'), "{why}: {text:?}");
            assert!(text.ends_with(&format!(" {facts_text}")), "{why}: {text:?}");
        }
    }

    #[test]
    fn no_facts_keys_like_no_footer() {
        assert_eq!(fingerprint(None, true), 0);
        assert_ne!(fingerprint(Some(&facts()), false), 0);
        let mut other = facts();
        other.branch = Some("feature".into());
        assert_ne!(
            fingerprint(Some(&facts()), true),
            fingerprint(Some(&other), true)
        );
    }

    /// A label that moves, appears or goes is a repaint: facts that differ
    /// only in the account key differently, with the usage switch on or off,
    /// and the timeline takes the label alone as a move, which the window's
    /// own key lets through. CONTROL: equal accounts key equal.
    #[test]
    fn the_account_moves_the_repaint_key() {
        let on = |label: &str| FooterFacts {
            account: Some(label.into()),
            ..facts()
        };
        for usage in [true, false] {
            let key = |facts: &FooterFacts| fingerprint(Some(facts), usage);
            assert_ne!(key(&on("main")), key(&on("work-2")), "another account");
            assert_ne!(key(&on("main")), key(&facts()), "one where there was none");
            assert_eq!(key(&on("main")), key(&on("main")), "the same account");
        }
        let (app, wid, session) = glass(80);
        let plan = app.active_visible_leaf_plan(wid).unwrap();
        // `publish` holds the timeline to answering that the facts moved.
        publish(&app, session, on("main"));
        let main = app.claude_footer_fp(wid, &plan);
        publish(&app, session, on("work-2"));
        assert_ne!(app.claude_footer_fp(wid, &plan), main);
    }

    /// A PTY update between grid extraction and host chrome must not move the
    /// footer onto a newer viewport or recolour it from newer OSC defaults.
    #[test]
    fn footer_uses_the_cell_frames_viewport_and_blank_after_terminal_moves() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let plan = app.active_visible_leaf_plan(wid).unwrap();
        let front = app.front_terminal(wid).unwrap().clone();
        let (rows, cols) = {
            let ws = &app.windows[&wid];
            (usize::from(ws.rows), usize::from(ws.cols))
        };
        {
            let entry = app.pool.get(front.session).unwrap();
            let mut timeline = entry.ctx.timeline.lock().unwrap();
            timeline.note_foreground_group(42);
            assert!(timeline.set_claude_footer(42, None, Some(facts())));
        }
        {
            let mut term = crate::term_lock(&front.term);
            term.resize(rows as u16, cols as u16);
            term.set_default_foreground(aterm_core::terminal::Rgb {
                r: 10,
                g: 20,
                b: 30,
            });
            term.set_default_background(aterm_core::terminal::Rgb {
                r: 40,
                g: 50,
                b: 60,
            });
            for _ in 0..rows + 4 {
                term.process(b"history\r\n");
            }
            let ws = app.windows.get_mut(&wid).unwrap();
            term.cell_frame_into(&mut ws.input_scratch, rows, cols);
            vendor_frame(&mut ws.input_scratch);
        }
        let extracted_blank = app.windows[&wid].input_scratch.implicit_blank;
        assert_eq!(extracted_blank.fg, [10, 20, 30]);
        assert_eq!(extracted_blank.bg, [40, 50, 60]);
        assert_eq!(app.windows[&wid].input_scratch.display_offset, 0);
        {
            let mut term = crate::term_lock(&front.term);
            term.process(b"\x1b]10;rgb:aa/bb/cc\x1b\\\x1b]11;rgb:dd/ee/ff\x1b\\");
            term.scroll_display(1);
            assert!(term.grid().display_offset() > 0);
            assert_ne!(term.implicit_blank_render_cell(), extracted_blank);
        }
        app.splice_claude_footer(
            wid,
            &plan,
            VisibleContentRoute::Terminal { composed: false },
        );
        let footer = &app.windows[&wid].input_scratch.cells[rows - 2];
        let mark = footer
            .iter()
            .position(|c| c.ch == '\u{25C6}')
            .expect("the extracted live frame still paints");
        assert_eq!(footer[mark + 2].bg, extracted_blank.bg);

        // A second paint of the retained scratch first undoes the footer and
        // then reads the SAME vendor frame. It must reuse that frame's text,
        // while still painting and observing the footer again.
        let text_before = Arc::clone(
            &app.windows[&wid].claude_footer_texts.borrow().panes[0]
                .as_ref()
                .unwrap()
                .rows,
        );
        app.splice_claude_footer(
            wid,
            &plan,
            VisibleContentRoute::Terminal { composed: false },
        );
        let text_after = Arc::clone(
            &app.windows[&wid].claude_footer_texts.borrow().panes[0]
                .as_ref()
                .unwrap()
                .rows,
        );
        assert!(Arc::ptr_eq(&text_before, &text_after));
        assert_eq!(
            app.windows[&wid].input_scratch.cells[rows - 2][mark].ch,
            '\u{25C6}'
        );

        // Conversely, a retained history frame stays history even if the
        // terminal has already returned to the live tail before this splice.
        {
            let mut term = crate::term_lock(&front.term);
            let ws = app.windows.get_mut(&wid).unwrap();
            term.cell_frame_into(&mut ws.input_scratch, rows, cols);
            vendor_frame(&mut ws.input_scratch);
            assert!(ws.input_scratch.display_offset > 0);
            term.scroll_display(-1);
            assert_eq!(term.grid().display_offset(), 0);
        }
        app.splice_claude_footer(
            wid,
            &plan,
            VisibleContentRoute::Terminal { composed: false },
        );
        let row = &app.windows[&wid].input_scratch.cells[rows - 2];
        assert!(
            row.iter().all(|c| c.ch == '\u{2500}'),
            "history frame keeps the vendor's rule"
        );
    }

    #[test]
    fn footer_source_uses_leaf_index_for_splits_and_view_for_mixed_tabs() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let mut plan = app.active_visible_leaf_plan(wid).unwrap();
        let leaf = plan.leaves.remove(0);
        let mut sibling = leaf.clone();
        sibling.focused = false;
        let ws = app.windows.get_mut(&wid).unwrap();
        ws.input_scratch.terminal_id = 11;
        ws.composed_focus_scratch.terminal_id = 22;
        ws.unfocused_pane_scratch.insert(1, RenderInput::empty());
        ws.unfocused_pane_scratch.get_mut(&1).unwrap().terminal_id = 33;
        ws.leaf_render_cache
            .entry(leaf.view)
            .or_default()
            .input
            .terminal_id = 44;

        let id =
            |route: VisibleContentRoute, index: usize, leaf: &crate::tab_model::VisibleLeaf| {
                frame_source(ws, route, index, leaf).map(|s| s.terminal_id)
            };
        assert_eq!(
            id(VisibleContentRoute::Terminal { composed: false }, 0, &leaf),
            Some(11)
        );
        assert_eq!(
            id(VisibleContentRoute::Terminal { composed: true }, 0, &leaf),
            Some(22)
        );
        assert_eq!(
            id(
                VisibleContentRoute::Terminal { composed: true },
                1,
                &sibling
            ),
            Some(33)
        );
        assert_eq!(id(VisibleContentRoute::Heterogeneous, 0, &leaf), Some(44));
        assert_eq!(
            id(
                VisibleContentRoute::Terminal { composed: true },
                2,
                &sibling
            ),
            None
        );
    }
}
