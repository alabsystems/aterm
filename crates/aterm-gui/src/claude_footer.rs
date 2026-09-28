// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CLAUDE CODE FOOTER, host side: aterm paints `◆ Opus 5.5 xhigh   ⌂
//! ~/aterm   ⎇ main` over Claude Code's permission-mode row (owner direction,
//! 2026-09-24). What the footer says and which row it replaces are decided in
//! `aterm_agent::harness::footer`; this module gets the facts off the event
//! loop and puts the row on the glass.
//!
//! * FACTS. One background thread ([`request_footer`]) reads them — the
//!   process's `sessions/<pid>.json`, its transcript tail, `.git/HEAD` — and
//!   publishes them on the session's timeline
//!   (`SessionTimeline::set_claude_footer`), exactly as `session_program`'s
//!   resolver publishes the program. The status sweep asks for a refresh at
//!   most every [`RECHECK`] while a Claude Code session is in front. A change
//!   wakes the loop ([`post_changed`] → `Wake::ClaudeFooter`) so the frame that
//!   shows it is not left to the next keystroke.
//! * THE ROW. [`App::splice_claude_footer`] runs with the chrome splices, per
//!   visible pane and inside that pane's own columns — a split's sibling on
//!   the same window row is untouched. It copies the vendor's cells for every
//!   piece the plan keeps (`esc to interrupt`, the pill of a mode the owner
//!   does not expect unless the lights' mode chip is drawn in its place, the
//!   right-aligned tail), so live status keeps its own colours. The Claude
//!   lights (`crate::claude_lights`) go at the row's end, and only while one
//!   is drawn does the footer give up columns for them.
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
//!   the footer row alone compared by content.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use aterm_agent::harness::footer::{self, FooterFacts, Piece};
use aterm_agent::harness::lights::Light;
use aterm_core::render::RenderInput;
use aterm_core::terminal::RenderCell;
use winit::event_loop::EventLoopProxy;

use crate::session_timeline::SessionTimeline;
use crate::{App, VisibleContentRoute, Wake, WindowId};

/// How often a Claude Code session's facts are re-read while it is in front.
/// Model and effort move per turn, the branch on a checkout: two seconds is
/// well inside a turn and costs one small file, one tail and one `HEAD`.
pub(crate) const RECHECK: Duration = Duration::from_secs(2);

/// The gap between the footer's marked values, as the owner's layout draws it.
const GAP: usize = 3;

/// The fewest columns the footer keeps beside a drawn chip before a narrow
/// pane drops the chips instead. With no chip drawn (everything as expected)
/// the footer has the whole row.
const MIN_BESIDE_LIGHTS: usize = 24;

/// One pane's painted light block, held until the write says whether its row
/// reached the scratch (only a light the glass shows gets a hit).
struct Lit {
    session: u64,
    frame_row: usize,
    col_off: usize,
    term_row: usize,
    /// The frame column the LIGHTS (not the title) start at.
    lights_col: usize,
    block: crate::claude_lights::Block,
}

/// One Claude Code pane's footer row, gathered under the window borrow and
/// painted after it (the lights need `&mut App`).
struct FooterPane {
    session: u64,
    frame_row: usize,
    /// The same row as a window grid row.
    term_row: usize,
    col_off: usize,
    pane_cols: usize,
    vendor: Vec<RenderCell>,
    plan: Vec<Piece>,
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
/// group cannot cancel a newer group's watch.
pub(crate) fn stop(session: u64, pgid: i32) {
    if pgid <= 0 {
        return;
    }
    send_stop(session, Some(pgid));
}

/// A retired session or a disabled status sweep has no live footer watch.
pub(crate) fn stop_session(session: u64) {
    send_stop(session, None);
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
    fn prune(&mut self, now: std::time::Instant) {
        self.watched
            .retain(|_, (_, asked, _)| now.saturating_duration_since(*asked) < WATCH_FOR);
    }

    fn next(&self) -> Option<std::time::Instant> {
        self.owed
            .iter()
            .map(|(at, _)| *at)
            .chain(
                self.watched
                    .values()
                    .map(|(_, _, read)| *read + IDLE_RECHECK),
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
            .find(|(_, _, read)| now >= *read + IDLE_RECHECK)
            .map(|(job, _, _)| job.clone())
    }

    fn read(&mut self, session: u64, at: std::time::Instant) {
        if let Some((_, _, read)) = self.watched.get_mut(&session) {
            *read = at;
        }
    }
}

/// The resolver thread: each request is read at once and again at each
/// follow-up, and the thread sleeps exactly until the next one is owed.
fn run(rx: &std::sync::mpsc::Receiver<Command>) {
    let mut schedule = WatchSchedule::default();
    let mut known: HashMap<u64, Identity> = HashMap::new();
    let mut tails: HashMap<u64, footer::TailCache> = HashMap::new();
    // Per session: its latest job, when it was last ASKED for, and when it
    // was last READ — the idle re-reads ([`IDLE_RECHECK`]) run off this.
    loop {
        let now = std::time::Instant::now();
        schedule.prune(now);
        known.retain(|session, _| schedule.watched.contains_key(session));
        tails.retain(|session, _| schedule.watched.contains_key(session));
        let first = match schedule.next() {
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
        // not once per ask that piled up behind it.
        let (latest, stopped) = schedule.accept(
            first
                .into_iter()
                .chain(std::iter::from_fn(|| rx.try_recv().ok())),
        );
        for session in stopped {
            known.remove(&session);
            tails.remove(&session);
        }
        for job in latest {
            resolve_and_post(&mut known, &mut tails, job.session, &job.timeline, job.pgid);
            let asked = std::time::Instant::now();
            schedule.watch(job, asked);
        }
        let now = std::time::Instant::now();
        for job in schedule.due_followups(now) {
            resolve_and_post(&mut known, &mut tails, job.session, &job.timeline, job.pgid);
            schedule.read(job.session, std::time::Instant::now());
        }
        while let Some(job) = schedule.due_idle(now) {
            resolve_and_post(&mut known, &mut tails, job.session, &job.timeline, job.pgid);
            schedule.read(job.session, std::time::Instant::now());
        }
    }
}

/// What the resolver knows about one Claude Code process for its whole
/// life: when the kernel started it (the pid-reuse guard) and the Claude Code
/// directory its environment named at exec. The directory costs one
/// `KERN_PROCARGS2` read, so it is read once per process — per session, and
/// again only when that session's foreground process is a different one (a
/// new pid, or the same pid born again: a relaunch onto a newer build).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Identity {
    pid: i32,
    started: Option<u64>,
    dir: Option<PathBuf>,
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

/// `pid`'s Claude Code directory, from its own environment at exec; aterm's
/// home when that environment cannot be read.
fn claude_dir_of_pid(pid: i32) -> Option<PathBuf> {
    match u32::try_from(pid)
        .ok()
        .and_then(atpkg::caller_shell::process_args)
    {
        Some(args) => {
            footer::claude_dir_of(args.env_var("CLAUDE_CONFIG_DIR"), args.env_var("HOME"))
        }
        None => aterm_types::dirs::home_dir().map(|home| home.join(".claude")),
    }
}

/// [`resolve_into`], then wake the loop when the facts moved — and, when the
/// moved facts carry a repository read the kernel refused with `EPERM`, post that
/// too, so the App can raise its consent attention for this session
/// (`Wake::ProtectedRead`; the App decides whether the path is protected).
fn resolve_and_post(
    known: &mut HashMap<u64, Identity>,
    tails: &mut HashMap<u64, footer::TailCache>,
    session: u64,
    timeline: &Arc<Mutex<SessionTimeline>>,
    pgid: i32,
) {
    let started = birth_seconds(pgid);
    let cached = known
        .get(&session)
        .filter(|id| id.pid == pgid && id.started.is_some() && id.started == started)
        .cloned();
    let identity = cached.unwrap_or_else(|| {
        let fresh = Identity {
            pid: pgid,
            started,
            dir: claude_dir_of_pid(pgid),
        };
        // No directory is not remembered: a launch through the atpkg twin is
        // `/bin/sh` for its first milliseconds, whose environment macOS hides,
        // and the `exec` into Claude Code keeps both the pid and the start
        // time — a cached `None` would outlive the shell and leave that
        // process with no footer for its whole life.
        if fresh.dir.is_some() {
            known.insert(session, fresh.clone());
        }
        fresh
    });
    if let Some(denied) = resolve_into(timeline, &identity, tails.entry(session).or_default()) {
        post_changed(session);
        if let Some(path) = denied
            && let Some(proxy) = PROXY.get()
        {
            let _ = proxy.send_event(Wake::ProtectedRead { session, path });
        }
    }
}

/// Read `identity`'s facts and publish them on `timeline`. `Some` when they
/// MOVED, carrying the working directory whose repository read was refused, if
/// one was. The file reads run before the (leaf) timeline lock is taken.
fn resolve_into(
    timeline: &Arc<Mutex<SessionTimeline>>,
    identity: &Identity,
    tail: &mut footer::TailCache,
) -> Option<Option<PathBuf>> {
    let facts = u32::try_from(identity.pid)
        .ok()
        .zip(identity.dir.as_deref())
        .and_then(|(pid, dir)| footer::facts_for_pid_cached(dir, pid, identity.started, tail));
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
        return None;
    }
    timeline
        .set_claude_footer(identity.pid, identity.started, facts)
        .then_some(denied)
}

/// A fingerprint of `facts` for the repaint key: `0` when there are none, so a
/// frame with no footer keys exactly as it did before this module existed.
pub(crate) fn fingerprint(facts: Option<&FooterFacts>) -> u64 {
    use std::hash::{Hash, Hasher};
    let Some(facts) = facts else {
        return 0;
    };
    let mut h = std::collections::hash_map::DefaultHasher::new();
    facts.hash(&mut h);
    h.finish() | 1
}

/// `plan` with its mode pill left out: the row's plan once the mode chip —
/// which names the mode — is painted on it, so the mode is not said twice.
fn plan_beside_mode_chip(plan: &[Piece]) -> Vec<Piece> {
    plan.iter()
        .filter(|piece| !matches!(piece, Piece::Mode(_)))
        .cloned()
        .collect()
}

/// Build the rewritten row for one pane: exactly `width` cells — the
/// footer's marked values, then the pieces the plan keeps, copied from
/// `vendor` (the original row's cells, in the columns the plan's ranges name).
///
/// THE VENDOR'S PIECES ARE NEVER CUT FOR THE FOOTER. They are live status
/// (`esc to interrupt`, a non-bypass mode, `/rc active`) and the original row
/// held them in `width` already, so when the row runs short the footer gives
/// way instead: the path is cut from the front to its last directory
/// (`footer::elide_path`), then its values are dropped whole from the back —
/// branch, then path, then model — until the row fits. Nothing is ever joined by a
/// separator that has nothing before it.
pub(crate) fn paint_row(
    vendor: &[RenderCell],
    plan: &[Piece],
    facts: &FooterFacts,
    blank: RenderCell,
    mark_fg: [u8; 3],
    width: usize,
) -> Vec<RenderCell> {
    let segments = footer::segments(facts);
    // The same values with the path cut short, tried before the branch goes.
    let short: Option<Vec<footer::Segment>> = segments
        .iter()
        .any(|s| s.mark == footer::PATH_MARK)
        .then(|| {
            segments
                .iter()
                .map(|s| match footer::elide_path(&s.text) {
                    Some(text) if s.mark == footer::PATH_MARK => {
                        footer::Segment { mark: s.mark, text }
                    }
                    _ => s.clone(),
                })
                .collect()
        });
    let mut row = Vec::new();
    'fit: for keep in (0..=segments.len()).rev() {
        let has_path = segments[..keep].iter().any(|s| s.mark == footer::PATH_MARK);
        for set in std::iter::once(&segments).chain(short.as_ref().filter(|_| has_path)) {
            row = lay_out(vendor, plan, &set[..keep], blank, mark_fg, width);
            if row.len() <= width {
                break 'fit;
            }
        }
    }
    if row.len() > width {
        row.truncate(width);
        // A wide glyph whose right half was cut is not drawn at all.
        if row
            .last()
            .is_some_and(|c| !c.wide && aterm_grapheme::char_width(c.ch) == 2)
            && let Some(last) = row.last_mut()
        {
            *last = blank;
        }
    }
    row.resize(width, blank);
    row
}

/// One candidate layout of [`paint_row`], unbounded: two cells of margin,
/// `segments` `GAP` apart, then the plan's vendor pieces. `width` is only
/// where the right-aligned tail aims.
fn lay_out(
    vendor: &[RenderCell],
    plan: &[Piece],
    segments: &[footer::Segment],
    blank: RenderCell,
    mark_fg: [u8; 3],
    width: usize,
) -> Vec<RenderCell> {
    const MARGIN: usize = 2;
    let mut out: Vec<RenderCell> = Vec::with_capacity(vendor.len().max(64));
    let pad = |out: &mut Vec<RenderCell>, n: usize| out.extend(std::iter::repeat_n(blank, n));
    let copy = |out: &mut Vec<RenderCell>, r: &std::ops::Range<usize>| {
        out.extend(vendor.iter().skip(r.start).take(r.len()).copied());
    };
    pad(&mut out, MARGIN);
    for piece in plan {
        let started = out.len() > MARGIN;
        match piece {
            Piece::Footer => {
                for (i, seg) in segments.iter().enumerate() {
                    if i > 0 {
                        pad(&mut out, GAP);
                    }
                    push_text(&mut out, blank, &seg.mark.to_string(), mark_fg);
                    pad(&mut out, 1);
                    push_text(&mut out, blank, &seg.text, blank.fg);
                }
            }
            Piece::Mode(r) => {
                if started {
                    pad(&mut out, GAP);
                }
                copy(&mut out, r);
            }
            Piece::Item(r) => {
                if started {
                    push_text(&mut out, blank, " \u{00B7} ", blank.fg);
                }
                copy(&mut out, r);
            }
            Piece::Tail(r) => {
                // Right-aligned as the vendor drew it — against the row's own
                // right edge, which the lights may have moved left of where
                // the vendor put it — else after a gap.
                let at = r.start.min(width.saturating_sub(r.len()));
                if out.len() + 2 <= at {
                    let n = at - out.len();
                    pad(&mut out, n);
                } else if started {
                    pad(&mut out, GAP);
                }
                copy(&mut out, r);
            }
        }
    }
    out
}

/// Append `text` in `fg` on `blank`'s ground, one cell per column: a wide
/// char takes its lead cell and a continuation, and a char that occupies no
/// column of its own (a combining mark, a control) is left out — a cell
/// cannot carry one without the side channels this splice never writes.
fn push_text(out: &mut Vec<RenderCell>, blank: RenderCell, text: &str, fg: [u8; 3]) {
    for ch in text.chars() {
        let cols = aterm_grapheme::char_width(ch);
        if cols == 0 || ch.is_control() {
            continue;
        }
        let mut cell = blank;
        cell.ch = ch;
        cell.fg = fg;
        out.push(cell);
        if cols == 2 {
            let mut half = blank;
            half.wide = true;
            half.fg = fg;
            out.push(half);
        }
    }
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

/// Write each `(frame_row, col_off, cells)` edit into `scratch` and return
/// the record [`undo`] needs, or `None` when nothing was written.
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
    edits: Vec<(usize, usize, Vec<RenderCell>)>,
) -> Option<FooterUndo> {
    let seq_before = scratch.snapshot_seq;
    let shifted_before = scratch.shifted_fill_seq;
    let restamp = scratch.shifted_fill_seq != 0 && scratch.host_prepend_blessing();
    let mut rows = Vec::with_capacity(edits.len());
    for (frame_row, col_off, painted) in edits {
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
            let ground = painted.last().copied().unwrap_or_default();
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
    /// The footer facts of every Claude Code session in `wid`'s visible plan,
    /// and the lights' GUI state (`crate::claude_lights`) — the window's own,
    /// and the fast latch kept on `App` (`App::claude_fast_latch`) wherever a
    /// Claude Code pane shows — folded into one repaint-key term (`0` when none
    /// shows).
    pub(crate) fn claude_footer_fp(
        &self,
        wid: WindowId,
        plan: &crate::tab_model::VisibleLeafPlan,
    ) -> u64 {
        let mut fp = self.windows.get(&wid).map_or(0, |ws| {
            ws.claude_lights.fingerprint(std::time::Instant::now())
        });
        let mut claude = false;
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
            claude |= timeline.claude_footer().is_some();
            fp = fp.rotate_left(7) ^ fingerprint(timeline.claude_footer());
        }
        // The fast latch is App-wide: taken in one window, it changes the
        // chips of every other window's Claude Code panes, which must repaint
        // for it — an idle one would otherwise keep a stale, clickable chip.
        if claude {
            fp ^= self.claude_fast_latch.generation().rotate_left(29);
        }
        fp
    }

    /// Paint the footer over each visible Claude Code pane's permission-mode
    /// row, inside that pane's columns. Runs after the tab strip is prepended
    /// (frame row = strip + pane row), so a find bar or settings panel that
    /// already claimed the row keeps it. A no-op for every pane with no facts,
    /// a pane scrolled into history, and a screen with no composer frame.
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
                let facts = entry
                    .ctx
                    .timeline
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .claude_footer()
                    .cloned();
                let Some(facts) = facts else {
                    continue;
                };
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
                // The row to rewrite, when the screen has one this pane may
                // paint: a mode row the chrome has not claimed, plain text (the
                // plan counts CHARS, a cell count only while the row holds no
                // wide cell — if one appears, the vendor drew something this
                // plan does not know) and a plan for it.
                // The reading runs behind the reader's panic fence: a screen
                // that panics it paints no row (`read_footer`).
                let row = read_footer(session, &texts, read_mode_row)
                    .filter(|&(r, _)| !self.chrome_owns_terminal_row(wid, row_off + r))
                    .and_then(|(r, plan)| {
                        let frame_row = strip + row_off + r;
                        let vendor: Vec<RenderCell> = ws.input_scratch.cells[frame_row]
                            .iter()
                            .skip(col_off)
                            .take(pane_cols)
                            .copied()
                            .collect();
                        let plan = plan.filter(|_| !vendor.iter().any(|c| c.wide))?;
                        Some((frame_row, row_off + r, vendor, plan))
                    });
                if row.is_none() {
                    note_vendor_drift(session, facts.version.as_deref(), &texts);
                }
                // Every Claude Code pane is OBSERVED for its lights, painted
                // row or not (`App::observe_claude_lights`).
                seen.push(SeenPaneText {
                    leaf_index,
                    session,
                    texts,
                });
                let Some((frame_row, term_row, vendor, plan)) = row else {
                    continue;
                };
                panes.push(FooterPane {
                    session,
                    frame_row,
                    term_row,
                    col_off,
                    pane_cols,
                    vendor,
                    plan,
                    facts,
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
        if panes.is_empty() {
            return;
        }
        let mut edits: Vec<(usize, usize, Vec<RenderCell>)> = Vec::with_capacity(panes.len());
        let mut lit: Vec<Lit> = Vec::new();
        let mut painted: Vec<(usize, usize, u64, usize)> = Vec::with_capacity(panes.len());
        for pane in panes {
            // The chips take the pane's right end when the footer keeps room
            // beside them; a pane too narrow for both keeps the footer alone.
            // Only the CHIPS must fit: the title (a hover, a selection, a
            // toggle's progress or refusal) comes along only where it fits
            // too, so pointing at a chip can never push the chips off. No
            // chip drawn — everything as expected — and there is no block.
            let room = pane.pane_cols.saturating_sub(MIN_BESIDE_LIGHTS);
            let block = self.claude_lights_block(wid, pane.session, pane.blank, room);
            // The full title where it fits; else the reason alone (the light
            // is marked beside it); else none.
            let title: &[RenderCell] = block.as_ref().map_or(&[], |b| {
                let room = pane.pane_cols - b.lights.len() - MIN_BESIDE_LIGHTS;
                if b.title.len() <= room {
                    &b.title
                } else if b.short_title.len() <= room {
                    &b.short_title
                } else {
                    &[]
                }
            });
            let width = pane.pane_cols - block.as_ref().map_or(0, |b| title.len() + b.lights.len());
            // The plan already left an expected mode's pill out
            // (`footer::plan_row`); any other mode's pill gives way to the
            // mode chip, which names it — a pane too narrow for the chip keeps
            // the pill.
            let plan: std::borrow::Cow<'_, [Piece]> =
                if block.as_ref().is_some_and(|b| b.shows(Light::Mode)) {
                    plan_beside_mode_chip(&pane.plan).into()
                } else {
                    pane.plan.as_slice().into()
                };
            let mut row = paint_row(&pane.vendor, &plan, &pane.facts, pane.blank, mark_fg, width);
            if let Some(block) = &block {
                row.extend_from_slice(title);
                row.extend_from_slice(&block.lights);
            }
            let lights_col = pane.col_off + width + title.len();
            if let Some(block) = block {
                lit.push(Lit {
                    session: pane.session,
                    frame_row: pane.frame_row,
                    col_off: pane.col_off,
                    term_row: pane.term_row,
                    lights_col,
                    block,
                });
            }
            painted.push((pane.frame_row, pane.col_off, pane.session, room));
            edits.push((pane.frame_row, pane.col_off, row));
        }
        let written: Vec<(usize, usize)> = {
            let Some(ws) = self.windows.get_mut(&wid) else {
                return;
            };
            ws.claude_footer_undo = write_edits(&mut ws.input_scratch, edits);
            ws.claude_footer_undo
                .as_ref()
                .map(|record| {
                    record
                        .rows
                        .iter()
                        .map(|r| (r.frame_row, r.col_off))
                        .collect()
                })
                .unwrap_or_default()
        };
        // Only a light the glass shows can be pointed at: a pane whose write
        // was skipped (`write_edits`) records none — keyed by the pane's row
        // AND column, since two panes side by side share a frame row.
        for (frame_row, col_off, session, room) in painted {
            if written.contains(&(frame_row, col_off)) {
                self.note_claude_footer_row(wid, session, room);
            }
        }
        for l in lit {
            if written.contains(&(l.frame_row, l.col_off)) {
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
    }

    /// `Wake::ClaudeFooter`: a session's footer facts moved — redraw every
    /// window that shows it. The repaint key's footer term is what lets the
    /// redraw through.
    pub(crate) fn on_claude_footer_changed(&mut self, session: u64) {
        // The lights ride this wake too: a toggle's next step, its deadline
        // and a refusal's end all arrive here (`crate::claude_lights`).
        self.drain_claude_lights();
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

/// A pane's footer reading: the mode row's index in `rows`, and the plan for
/// that row (`None` when the row is not one this footer knows).
type FooterRead = Option<(usize, Option<Vec<Piece>>)>;

/// The footer's reading of a pane's rows (`footer::mode_row`, then
/// `footer::plan_row` on that row).
fn read_mode_row(rows: &[String]) -> FooterRead {
    footer::mode_row(rows).map(|r| (r, footer::plan_row(&rows[r])))
}

/// VENDOR DRIFT, SAID OUT LOUD: a Claude Code pane whose composer has a
/// pill-shaped mode row under it that this build does not read
/// (`footer::opens_with_pill_glyph` but no `footer::pill_indicator`). The
/// footer and the lights stand down on such a row, as they must; this puts
/// the row in aterm's log ONCE per Claude Code build and row (at most
/// [`DRIFT_ROWS_LOGGED`] rows a run), so a vendor that renamed or added a
/// mode is found by reading the log rather than by lights that quietly went
/// grey. Behind the reader's panic fence, like every read of the pane.
fn note_vendor_drift(session: u64, version: Option<&str>, rows: &[String]) {
    static LOGGED: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let unread = crate::reader_guard::read_or_none(
        &format!("{session}"),
        "Claude Code drift check",
        rows,
        || unread_pill_row(rows),
    )
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
        "claude footer: Claude Code {build} drew a mode row this aterm does not read, so the footer and lights stand down on it: {row:?}"
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
/// (`crate::reader_guard`): a panic is warned once and read as NO mode row,
/// so the vendor's own row is left as drawn (the seam the tests inject a
/// panicking reader through).
fn read_footer(
    session: u64,
    rows: &[String],
    read: impl FnOnce(&[String]) -> FooterRead,
) -> FooterRead {
    crate::reader_guard::read_or_none(&format!("{session}"), "Claude Code footer", rows, || {
        read(rows)
    })
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(schedule.next().is_none());
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

    /// Manual cost diagnostic for a long Claude transcript with an unchanged
    /// render revision. Run with `--ignored --nocapture` after changing this
    /// path; the test above, rather than wall time, pins the behavior.
    #[test]
    #[ignore = "manual unchanged-frame cost diagnostic"]
    fn pane_text_unchanged_frame_cost() {
        use std::hint::black_box;
        use std::time::Instant;

        const REPEATS: usize = 2_000;
        let mut scratch = RenderInput::empty();
        scratch.rows = 80;
        scratch.cols = 160;
        scratch.cells = vec![row_of(&"x".repeat(160), [0, 0, 0]); 80];
        scratch.snapshot_seq = 42;
        let region = PaneTextRegion {
            strip: 0,
            row_off: 0,
            col_off: 0,
            rows: 80,
            cols: 160,
        };
        let start = Instant::now();
        for _ in 0..REPEATS {
            let rows: Vec<String> = scratch
                .cells
                .iter()
                .map(|row| row.iter().map(|c| c.ch).collect())
                .collect();
            black_box(rows);
        }
        let rebuilt = start.elapsed();
        let mut cache = PaneTextCache::default();
        black_box(cache.rows(0, 7, &scratch, region));
        let start = Instant::now();
        for _ in 0..REPEATS {
            black_box(cache.rows(0, 7, &scratch, region));
        }
        eprintln!(
            "Claude pane text: rebuilt {rebuilt:?}, cached {:?}",
            start.elapsed()
        );
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
        row.iter().map(|c| c.ch).collect()
    }

    fn facts() -> FooterFacts {
        FooterFacts {
            model: Some("Opus 5.5".into()),
            effort: Some("xhigh".into()),
            path: Some("~/aterm".into()),
            branch: Some("main".into()),
            repo_read_denied: None,
            version: None,
        }
    }

    /// The footer's reading runs behind the reader's panic fence: a reader
    /// that panics on a pane's rows paints NO row (the vendor's stays) and
    /// the frame goes on. Control: the real reader over the same rows finds
    /// the mode row and plans it, so the `None` is the panic's.
    #[test]
    fn a_reader_panic_paints_no_footer_row() {
        let rows = vec![
            "\u{2500}".repeat(60),
            "\u{276F} ".to_string(),
            "\u{2500}".repeat(60),
            "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)".to_string(),
        ];
        let (r, plan) = read_footer(9, &rows, read_mode_row).expect("the real reader finds it");
        assert_eq!(r, 3);
        assert!(plan.is_some(), "and plans it");
        let boom = |_: &[String]| -> FooterRead { panic!("stand-in reader panic") };
        assert!(read_footer(9, &rows, boom).is_none());
        assert!(read_footer(9, &rows, boom).is_none(), "every frame");
    }

    /// The owner's layout, exactly: two cells of margin, three marks, three
    /// cells between values — and the bypass pill and its hints gone.
    #[test]
    fn the_bypass_row_becomes_the_owners_footer() {
        let vendor_text = "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle) \u{00B7} \u{2190} for agents";
        let vendor = row_of(vendor_text, [255, 0, 0]);
        let plan = footer::plan_row(vendor_text).unwrap();
        let blank = cell(' ', [200, 200, 200]);
        let row = paint_row(&vendor, &plan, &facts(), blank, [0, 128, 255], 60);
        assert_eq!(row.len(), 60, "exactly the pane's width");
        assert_eq!(
            text_of(&row).trim_end(),
            "  \u{25C6} Opus 5.5 xhigh   \u{2302} ~/aterm   \u{2387} main"
        );
        assert_eq!(row[2].fg, [0, 128, 255], "the mark takes the accent");
        assert_eq!(
            row[4].fg,
            [200, 200, 200],
            "the value takes the terminal's ink"
        );
        assert!(!text_of(&row).contains("bypass"));
    }

    /// Live status keeps the VENDOR'S cells — its words and its colours.
    #[test]
    fn live_status_is_copied_with_its_own_colours() {
        let vendor_text = "  \u{23F5}\u{23F5} accept edits on (shift+tab to cycle) \u{00B7} esc to interrupt \u{00B7} \u{2190} for agents";
        let mut vendor = row_of(vendor_text, [120, 120, 120]);
        for c in vendor.iter_mut().skip(2).take(18) {
            c.fg = [255, 200, 0]; // the pill's own ink
        }
        let plan = footer::plan_row(vendor_text).unwrap();
        let blank = cell(' ', [200, 200, 200]);
        let row = paint_row(&vendor, &plan, &facts(), blank, [0, 128, 255], 120);
        let text = text_of(&row);
        assert!(
            text.contains("\u{23F5}\u{23F5} accept edits on"),
            "a mode the owner does not expect stays: {text:?}"
        );
        assert!(text.contains("\u{00B7} esc to interrupt"), "{text:?}");
        assert!(!text.contains("for agents"), "{text:?}");
        assert!(!text.contains("shift+tab"), "{text:?}");
        let pill_at = text.chars().position(|c| c == '\u{23F5}').unwrap();
        assert_eq!(
            row[pill_at].fg,
            [255, 200, 0],
            "the pill keeps the vendor's ink"
        );
    }

    #[test]
    fn a_narrow_pane_cuts_the_footer_rather_than_overflowing() {
        let vendor_text = "  \u{23F5}\u{23F5} bypass permissions on";
        let vendor = row_of(vendor_text, [255, 0, 0]);
        let plan = footer::plan_row(vendor_text).unwrap();
        let row = paint_row(
            &vendor,
            &plan,
            &facts(),
            cell(' ', [1, 1, 1]),
            [2, 2, 2],
            12,
        );
        assert_eq!(row.len(), 12);
    }

    /// A deep path is cut to its last directory before the branch gives way,
    /// and only then dropped.
    #[test]
    fn a_long_path_is_cut_short_before_the_branch_goes() {
        let vendor_text = "  \u{23F5}\u{23F5} bypass permissions on";
        let plan = footer::plan_row(vendor_text).unwrap();
        let blank = cell(' ', [1, 1, 1]);
        let deep = FooterFacts {
            path: Some("~/src/github.com/someone/aterm".into()),
            ..facts()
        };
        let paint = |width| {
            text_of(&paint_row(
                &row_of(vendor_text, [5, 5, 5]),
                &plan,
                &deep,
                blank,
                [2, 2, 2],
                width,
            ))
        };
        assert_eq!(
            paint(70).trim_end(),
            "  \u{25C6} Opus 5.5 xhigh   \u{2302} ~/src/github.com/someone/aterm   \u{2387} main"
        );
        assert_eq!(
            paint(50).trim_end(),
            "  \u{25C6} Opus 5.5 xhigh   \u{2302} \u{2026}/aterm   \u{2387} main"
        );
        assert_eq!(
            paint(33).trim_end(),
            "  \u{25C6} Opus 5.5 xhigh   \u{2302} \u{2026}/aterm"
        );
    }

    /// A recorded busy row (`context-low.txt`) at 80 columns: every piece of
    /// live status survives whole, and the footer gives way from the back.
    #[test]
    fn a_short_row_drops_footer_values_never_the_vendors_status() {
        let vendor_text = "  \u{23F8} plan mode on \u{00B7} 5 shells \u{00B7} esc to interrupt \u{00B7} \u{2190} for agents \u{00B7} \u{2193} to manage";
        let vendor = row_of(vendor_text, [120, 120, 120]);
        let plan = footer::plan_row(vendor_text).unwrap();
        let blank = cell(' ', [200, 200, 200]);
        let text = text_of(&paint_row(&vendor, &plan, &facts(), blank, [1, 2, 3], 80));
        for kept in [
            "plan mode on",
            "5 shells",
            "esc to interrupt",
            "\u{2193} to manage",
        ] {
            assert!(text.contains(kept), "{kept:?} survives: {text:?}");
        }
        assert!(text.contains("\u{25C6} Opus 5.5 xhigh"), "{text:?}");
        assert!(
            !text.contains("\u{2387}"),
            "the branch goes first: {text:?}"
        );
        assert!(!text.contains("\u{2302}"), "then the path: {text:?}");
        let plan_mode = "  \u{23F8} plan mode on (shift+tab to cycle)";
        let plan = footer::plan_row(plan_mode).unwrap();
        let text = text_of(&paint_row(
            &row_of(plan_mode, [1, 1, 1]),
            &plan,
            &facts(),
            blank,
            [1, 2, 3],
            50,
        ));
        assert_eq!(
            text.trim_end(),
            "  \u{25C6} Opus 5.5 xhigh   \u{2302} ~/aterm   \u{23F8} plan mode on"
        );
    }

    /// No facts known yet: the vendor's status opens the row, with no
    /// separator hanging in front of it.
    #[test]
    fn nothing_known_leaves_no_dangling_separator() {
        let vendor_text = "  \u{23F5}\u{23F5} bypass permissions on \u{00B7} esc to interrupt";
        let plan = footer::plan_row(vendor_text).unwrap();
        let row = paint_row(
            &row_of(vendor_text, [5, 5, 5]),
            &plan,
            &FooterFacts::default(),
            cell(' ', [1, 1, 1]),
            [2, 2, 2],
            40,
        );
        assert_eq!(text_of(&row).trim_end(), "  esc to interrupt");
    }

    /// A branch or directory name can be wide: it takes a lead cell and a
    /// continuation, and a cut through the pair leaves neither half.
    #[test]
    fn wide_names_take_two_cells() {
        let vendor_text = "  \u{23F5}\u{23F5} bypass permissions on";
        let plan = footer::plan_row(vendor_text).unwrap();
        let wide = FooterFacts {
            branch: Some("\u{6F22}\u{5B57}".into()),
            ..FooterFacts::default()
        };
        let blank = cell(' ', [1, 1, 1]);
        let row = paint_row(
            &row_of(vendor_text, [5, 5, 5]),
            &plan,
            &wide,
            blank,
            [2, 2, 2],
            20,
        );
        let lead = row.iter().position(|c| c.ch == '\u{6F22}').unwrap();
        assert!(
            !row[lead].wide && row[lead + 1].wide,
            "lead, then its right half"
        );
        assert_eq!(row[lead + 2].ch, '\u{5B57}');
        assert_eq!(row.len(), 20);
        // `  ⎇ 漢字` is 8 columns: a 7-column pane drops the value whole
        // rather than show half of its last glyph.
        let cut = paint_row(
            &row_of(vendor_text, [5, 5, 5]),
            &plan,
            &wide,
            blank,
            [2, 2, 2],
            7,
        );
        assert_eq!(cut.len(), 7);
        assert!(cut.iter().all(|c| c.ch == ' '), "{cut:?}");
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
        let mut slot = write_edits(&mut s, vec![(3, 0, painted.clone())]);
        assert!(slot.is_some());
        assert_eq!(&s.cells[3][..painted.len()], painted.as_slice());
        assert_ne!(s.snapshot_seq, s.engine_fill_seq, "the write is declared");
        assert_eq!(
            s.shifted_fill_seq, s.snapshot_seq,
            "an untouched engine fill keeps the renderer's stamp compare"
        );
        assert_eq!(s.row_rev[3], 0, "the footer row is compared by content");
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
        let mut slot = write_edits(&mut s, vec![(2, 0, painted.clone())]);
        s.snapshot_seq += 1; // another host write
        let before = (s.cells.clone(), s.snapshot_seq, s.row_rev.clone());
        assert!(!undo(&mut slot, &mut s));
        assert_eq!((s.cells.clone(), s.snapshot_seq, s.row_rev.clone()), before);
        assert!(slot.is_none(), "a stale record is dropped");

        let mut s = engine_fill(3);
        let mut slot = write_edits(&mut s, vec![(2, 0, painted)]);
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
        let mut slot = write_edits(&mut s, vec![(1, 0, left), (1, 20, right)]);
        assert_eq!(s.cells[1].len(), 34, "padded to the right pane's edge");
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
        assert!(write_edits(&mut s, vec![(1, 0, row_of("  footer", [1, 1, 1]))]).is_none());
        assert_eq!(s.cells, before);
        assert_eq!(s.snapshot_seq, 40, "nothing written, nothing declared");
    }

    /// A scratch some other host already wrote is not re-blessed: the
    /// renderer's stamp compare stays off, exactly as that write left it.
    #[test]
    fn only_an_untouched_engine_fill_keeps_its_stamps() {
        let mut s = engine_fill(2);
        s.snapshot_seq = 41; // e.g. the find bar, earlier this frame
        let mut slot = write_edits(&mut s, vec![(1, 0, row_of("  footer", [1, 1, 1]))]);
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
        let mut slot = write_edits(&mut s, vec![(3, 0, row_of("  footer", [1, 1, 1]))]);
        assert_eq!(
            s.shifted_fill_seq, s.snapshot_seq,
            "the stamp compare stays on under the strip"
        );
        assert_eq!(s.row_rev[3], 0, "the footer row is compared by content");
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

    /// An expected mode's pill (bypass, and auto just the same) is never
    /// painted: at rest the row is the footer and the live status alone.
    /// Plan mode, which the owner does not expect, keeps its pill.
    #[test]
    fn an_expected_modes_pill_is_not_painted() {
        let auto = "  \u{23F5}\u{23F5} auto mode on (shift+tab to cycle) \u{00B7} esc to interrupt";
        let vendor = row_of(auto, [9, 9, 9]);
        let plan = footer::plan_row(auto).unwrap();
        let text = text_of(&paint_row(
            &vendor,
            &plan,
            &facts(),
            cell(' ', [1, 1, 1]),
            [2, 2, 2],
            60,
        ));
        assert!(!text.contains("auto mode"), "{text:?}");
        assert!(
            text.contains("esc to interrupt"),
            "live status stays: {text:?}"
        );
        let plan_mode = "  \u{23F8} plan mode on (shift+tab to cycle)";
        let plan = footer::plan_row(plan_mode).unwrap();
        let text = text_of(&paint_row(
            &row_of(plan_mode, [9, 9, 9]),
            &plan,
            &facts(),
            cell(' ', [1, 1, 1]),
            [2, 2, 2],
            60,
        ));
        assert!(
            text.contains("\u{23F8} plan mode on"),
            "a mode the owner does not expect keeps its pill: {text:?}"
        );
    }

    /// Beside the mode chip, which names the mode, the pill gives way; live
    /// status stays.
    #[test]
    fn the_mode_chip_takes_the_pills_place() {
        let plan_mode = "  \u{23F8} plan mode on (shift+tab to cycle) \u{00B7} esc to interrupt";
        let plan = footer::plan_row(plan_mode).unwrap();
        let beside = plan_beside_mode_chip(&plan);
        assert!(
            !beside.iter().any(|p| matches!(p, Piece::Mode(_))),
            "{beside:?}"
        );
        assert_eq!(beside.len(), plan.len() - 1, "only the pill goes");
    }

    #[test]
    fn no_facts_keys_like_no_footer() {
        assert_eq!(fingerprint(None), 0);
        assert_ne!(fingerprint(Some(&facts())), 0);
        let mut other = facts();
        other.branch = Some("feature".into());
        assert_ne!(fingerprint(Some(&facts())), fingerprint(Some(&other)));
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
        let footer = &app.windows[&wid].input_scratch.cells[rows - 1];
        assert_eq!(
            footer[2].ch, '\u{25C6}',
            "the extracted live frame still paints"
        );
        assert_eq!(footer[4].fg, extracted_blank.fg);
        assert_eq!(footer[4].bg, extracted_blank.bg);

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
            app.windows[&wid].input_scratch.cells[rows - 1][2].ch,
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
        let row = &app.windows[&wid].input_scratch.cells[rows - 1];
        assert_eq!(row[2].ch, '\u{23F5}', "history frame keeps the vendor row");
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
