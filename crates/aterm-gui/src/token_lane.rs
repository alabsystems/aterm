// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The vault token view's own lane (`docs/DESIGN-claude-setup-token-2026-11-05.md` §5.9, §5.10):
//! the one reader of this Mac's token view
//! ([`aterm_agent::harness::accounts_token_view::view_here`]), on its own thread ([`THREAD`],
//! [`Role::Background`](crate::qos::Role::Background)), off the Claude Code footer's resolver,
//! which only asks for it ([`want`]), as the Lab page does. Each read posts what it found
//! ([`crate::token_shown::post`]) for the Account card, the Lab page, the harness page's state
//! line and `ctl status`, which map it without reading a file.
//!
//! It reads for one of three causes, and on no clock:
//! - **demand:** the first [`want`] (the first Claude Code footer read of a login, a Lab page
//!   collection);
//! - **a file it reads moved:** before each read it names the files the read opens
//!   ([`accounts_token_view::read_set_here`](aterm_agent::harness::accounts_token_view::read_set_here)),
//!   places a watch on each ([`FileWatch`]: the file and its folder, so a replace by rename is
//!   heard, and a missing file's nearest folder), and only then stamps them
//!   ([`ReadSet::stamp`](aterm_agent::harness::accounts_token_view::ReadSet::stamp)) and reads,
//!   so a write after the stamp is heard and moves it. A watch that tells is a cause only where
//!   the stamp moved: the first look of a new watch, a write the read already saw, and the
//!   vault's `state.toml` rewritten for when it last heard this Mac read nothing;
//! - **its deadline:** the first second at which what it said changes by the clock alone
//!   ([`accounts_token_view::view_changes_after`](aterm_agent::harness::accounts_token_view::view_changes_after):
//!   a token's `soon` and end, a login's turns, the state line's days, a `signed here` row's
//!   next post).
//!
//! A read a moved file causes starts no sooner than [`FLOOR_S`] after the last read began, and the
//! causes inside that floor are one read at its end: at most one read a minute while its files
//! keep moving (no more often than the footer's minute gate read it before), and none while
//! nothing moves. A deadline reads at its second. A read the lane's own writes cause (an offer
//! recorded, a post's try) finds them and writes nothing, so it causes no other.
//!
//! The deadline is a wall second, and the thread waits toward it on the monotonic clock, which
//! stops while the Mac sleeps. So the lane hears the wall clock's moves (its own
//! [`aterm_uds::wall_watch::WallWatch`], polled on [`WALL_THREAD`]): a step of the wall clock or a
//! wake is [`Msg::Wall`], which ends the wait, and the next is measured again from the wall, so a
//! deadline slept past reads at the wake. Where no watch is had, its descriptors cannot be polled,
//! or it hears no sleep ([`WallWatch::hears_sleep`]: a macOS watch whose power half was refused
//! hears steps alone), each wait is at most [`WALL_LOOK_S`], so a sleep or a step is noticed within
//! one.
//!
//! Where no watcher can be had (the kernel refused one), the lane is on its clocks arm: each
//! demand reads where [`FLOOR_S`] has passed since the last read began, as the footer's minute
//! gate read it. Each later read places its watches again.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use aterm_uds::file_watch::FileWatch;
use aterm_uds::wall_watch::WallWatch;

/// The lane's thread.
pub(crate) const THREAD: &str = "aterm-token-view";

/// The thread of the lane's file watch.
const WATCH_THREAD: &str = "aterm-token-watch";

/// The thread that polls the lane's wall clock watch ([`listen_wall`]).
#[cfg(unix)]
const WALL_THREAD: &str = "aterm-token-wall";

/// The least time from the start of one read to a read a moved file causes, and on the clocks
/// arm to a read a demand causes, in seconds.
pub(crate) const FLOOR_S: i64 = 60;

/// The longest wait toward a deadline where the wall clock's moves are not heard ([`wait_toward`]:
/// no watch, none polled, or one that hears no sleep), in seconds: a sleep or a step of the wall
/// clock is noticed within one.
pub(crate) const WALL_LOOK_S: u64 = 60;

/// What the token view's reader does for the lane: name the files a read opens, stamp them, and
/// read.
pub(crate) trait View {
    /// The files a read opens, as named before it.
    type Set;
    /// A [`Self::Set`]'s files as they stand.
    type Stamp: PartialEq;
    /// The files a read at wall second `now` opens.
    fn set(&mut self, now: i64) -> Self::Set;
    /// The paths of `set` to watch.
    fn paths(&self, set: &Self::Set) -> Vec<PathBuf>;
    /// `set`'s files as they stand now.
    fn stamp(&mut self, set: &Self::Set) -> Self::Stamp;
    /// Read the view at wall second `now` and post it: the first second after `now` at which
    /// what it says changes by the clock alone, where one is ahead.
    fn read(&mut self, now: i64) -> Option<i64>;
}

/// Where the lane's watches go.
pub(crate) trait Watch {
    /// Watch exactly `paths` (each already watched stays as it is): `false` where no watcher can
    /// be had, which puts the lane on its clocks arm.
    fn place(&mut self, paths: &[PathBuf]) -> bool;
}

/// How the lane hears its files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Arm {
    /// Its files are watched: a demand after the first read reads nothing.
    Watched,
    /// No watcher could be had: each demand reads where the floor has passed.
    Clocks,
}

/// What reaches the lane's thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Msg {
    /// A surface asked for the view ([`want`]).
    Want,
    /// A watch of the read set told that its file may have moved.
    Told,
    /// The wall clock was set, or the machine woke ([`listen_wall`]): the wait toward the
    /// deadline, kept on the monotonic clock, which stops in a sleep, ends and is measured again
    /// from the wall. It owes no read of its own: a deadline the wall passed meanwhile reads.
    #[cfg_attr(
        not(unix),
        expect(
            dead_code,
            reason = "sent only by the unix `listen_wall`; Windows has no wall watch"
        )
    )]
    Wall,
}

/// The lane's state between turns ([`Self::turn`]), on the wall clock in seconds.
pub(crate) struct Lane<V: View, W: Watch> {
    view: V,
    watch: W,
    /// The last read's files and their stamps from before it.
    held: Option<(V::Set, V::Stamp)>,
    /// When the last read began.
    began: Option<i64>,
    /// A cause owes a read.
    owed: bool,
    /// The last read's deadline.
    due: Option<i64>,
    arm: Arm,
}

impl<V: View, W: Watch> Lane<V, W> {
    /// A lane over `view` whose watches go to `watch`, before its first read.
    pub(crate) fn new(view: V, watch: W) -> Self {
        Self {
            view,
            watch,
            held: None,
            began: None,
            owed: false,
            due: None,
            arm: Arm::Watched,
        }
    }

    /// Hear `msg`: a demand owes a read before the first and on the clocks arm; a tell owes one
    /// only where the held files' stamp moved; a move of the wall owes none (the turn after it
    /// reads where the deadline has come by the wall).
    pub(crate) fn hear(&mut self, msg: Msg) {
        match msg {
            Msg::Want => {
                if self.began.is_none() || self.arm == Arm::Clocks {
                    self.owed = true;
                }
            }
            Msg::Told => {
                if let Some((set, stamp)) = &self.held
                    && self.view.stamp(set) != *stamp
                {
                    self.owed = true;
                }
            }
            Msg::Wall => {}
        }
    }

    /// The wall second of the lane's next read, where one is owed or due: an owed read's floor
    /// end (at once before the first read), or the last read's deadline, whichever is first.
    pub(crate) fn next(&self) -> Option<i64> {
        let owed = self
            .owed
            .then(|| self.began.map_or(i64::MIN, |at| at.saturating_add(FLOOR_S)));
        owed.into_iter().chain(self.due).min()
    }

    /// Read at wall second `now` where a read is owed and its floor has passed (or the clock
    /// stepped back past the last read), or the deadline has come: whether it read.
    pub(crate) fn turn(&mut self, now: i64) -> bool {
        let floored = self.owed
            && self
                .began
                .is_none_or(|at| now >= at.saturating_add(FLOOR_S) || now < at);
        let due = self.due.is_some_and(|due| due <= now);
        if !(floored || due) {
            return false;
        }
        let set = self.view.set(now);
        self.arm = if self.watch.place(&self.view.paths(&set)) {
            Arm::Watched
        } else {
            Arm::Clocks
        };
        let stamp = self.view.stamp(&set);
        self.due = self.view.read(now);
        self.held = Some((set, stamp));
        self.began = Some(now);
        self.owed = false;
        true
    }

    /// How the lane hears its files now.
    pub(crate) fn arm(&self) -> Arm {
        self.arm
    }
}

/// Ask for the token view: the lane starts on the first ask and reads once; later asks read
/// nothing while its files are watched, and on the clocks arm read where [`FLOOR_S`] has passed.
/// Idempotent and cheap: a footer read of a Claude Code login asks on every pass.
pub(crate) fn want() {
    #[cfg(test)]
    crate::work_counts::token_wanted();
    if WATCHED.load(Ordering::Acquire) {
        return;
    }
    if let Some(lane) = LANE.get_or_init(start) {
        let _ = lane.send(Msg::Want);
    }
}

/// The lane's door, once it started; `None` where its thread could not start.
static LANE: OnceLock<Option<Sender<Msg>>> = OnceLock::new();

/// Whether the lane's files are watched, so an ask after its first read is nothing to send.
static WATCHED: AtomicBool = AtomicBool::new(false);

/// Start the lane's thread over this Mac's view.
fn start() -> Option<Sender<Msg>> {
    let (door, heard) = channel();
    let files = Files::new(door.clone());
    match start_lane(
        (Lane::new(Here, files), wall_now),
        (door.clone(), heard),
        WallWatch::start().map(Arc::new),
    ) {
        Ok(_) => Some(door),
        Err(why) => {
            aterm_log::warn!("accounts: the token view's lane did not start: {why}");
            None
        }
    }
}

/// Start `lane`'s thread, reading the wall clock with `clock`, hearing `heard` (whose sender is
/// `door`), and `wall`'s moves where one is had ([`listen_wall`]); where none is, it cannot be
/// polled, or it hears no sleep ([`WallWatch::hears_sleep`]), the lane's waits are held to
/// [`WALL_LOOK_S`] ([`wait_toward`]). A polled watch that hears no sleep still ends a wait at
/// each step it hears. Whether the waits run to their deadlines: `wall` polled, and hearing
/// each sleep.
fn start_lane<V, W, C>(
    (lane, clock): (Lane<V, W>, C),
    (door, heard): (Sender<Msg>, Receiver<Msg>),
    wall: Option<Arc<WallWatch>>,
) -> std::io::Result<bool>
where
    V: View + Send + 'static,
    W: Watch + Send + 'static,
    V::Set: Send,
    V::Stamp: Send,
    C: Fn() -> i64 + Send + 'static,
{
    let heard_wall = wall.is_some_and(|wall| {
        let hears_sleep = wall.hears_sleep();
        match listen_wall(wall, door) {
            Ok(()) if hears_sleep => true,
            Ok(()) => {
                aterm_log::info!(
                    "accounts: the token view's lane hears no sleep of this Mac (its wall clock \
                     watch hears steps alone): its waits are held to a minute"
                );
                false
            }
            Err(why) => {
                aterm_log::info!(
                    "accounts: the token view's lane hears no wall clock move ({why}): its waits \
                     are held to a minute"
                );
                false
            }
        }
    });
    crate::qos::spawn_lane_thread(THREAD, crate::qos::Role::Background, move || {
        run(&heard, lane, &clock, heard_wall);
    })
    .map(|_| heard_wall)
}

/// Tell the lane at `door` each step of the wall clock and each wake `wall` announces
/// ([`Msg::Wall`]), on [`WALL_THREAD`], which polls `wall`'s descriptors
/// ([`atpkg::dirwatch::Watcher::add_readable`]) with no timeout and ends once the lane is gone.
///
/// # Errors
/// No watcher could be had, or one of `wall`'s descriptors was refused, or the thread did not
/// start: nothing is told.
#[cfg(unix)]
fn listen_wall(wall: Arc<WallWatch>, door: Sender<Msg>) -> std::io::Result<()> {
    let watcher = atpkg::dirwatch::Watcher::new()?;
    for fd in wall.fds() {
        watcher.add_readable(fd)?;
    }
    crate::qos::spawn_lane_thread(WALL_THREAD, crate::qos::Role::Background, move || {
        // `wall` is held as long as `watcher` polls its descriptors.
        let wall = wall;
        while watcher.wait(None).is_ok() {
            let moved = wall.take();
            if (moved.stepped || moved.woke) && door.send(Msg::Wall).is_err() {
                return;
            }
        }
    })
    .map(|_| ())
}

/// No descriptor is polled here: the lane's waits are held to [`WALL_LOOK_S`].
#[cfg(not(unix))]
fn listen_wall(_wall: Arc<WallWatch>, _door: Sender<Msg>) -> std::io::Result<()> {
    Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
}

/// How long the lane waits from wall second `now` toward wall second `at`: the whole span where
/// the wall clock's moves are heard (`heard_wall`: a polled watch that hears each sleep, whose
/// [`Msg::Wall`] ends the wait), else at most [`WALL_LOOK_S`], after which the loop looks at the
/// wall again.
pub(crate) fn wait_toward(at: i64, now: i64, heard_wall: bool) -> Duration {
    let left = u64::try_from(at.saturating_sub(now)).unwrap_or(0);
    Duration::from_secs(if heard_wall {
        left
    } else {
        left.min(WALL_LOOK_S)
    })
}

/// The lane's loop: sleep until its next read or a message ([`wait_toward`], the wall clock read
/// with `clock`), hear every message waiting, take the turn.
fn run<V: View, W: Watch>(
    heard: &Receiver<Msg>,
    mut lane: Lane<V, W>,
    clock: &impl Fn() -> i64,
    heard_wall: bool,
) {
    loop {
        let now = clock();
        let msg = match lane.next() {
            Some(at) => heard.recv_timeout(wait_toward(at, now, heard_wall)),
            None => heard.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match msg {
            Ok(msg) => lane.hear(msg),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        for msg in heard.try_iter() {
            lane.hear(msg);
        }
        if lane.turn(clock()) {
            WATCHED.store(lane.arm() == Arm::Watched, Ordering::Release);
        }
    }
}

/// The wall clock, in unix seconds.
fn wall_now() -> i64 {
    aterm_time::unix_secs()
        .and_then(|at| i64::try_from(at).ok())
        .unwrap_or(0)
}

/// This Mac's token view, as the lane reads it. A unit test's reads book the read
/// (`work_counts::token_view_read`) on the thread that made it and read no file: the lane a test
/// starts names none and posts nothing.
pub(crate) struct Here;

#[cfg(unix)]
impl View for Here {
    type Set = aterm_agent::harness::accounts_token_view::ReadSet;
    type Stamp = aterm_agent::harness::accounts_token_view::SetStamp;

    fn set(&mut self, now: i64) -> Self::Set {
        #[cfg(test)]
        {
            let _ = now;
            Self::Set::default()
        }
        #[cfg(not(test))]
        {
            crate::harness_host::harness_state()
                .map(|harness| {
                    aterm_agent::harness::accounts_token_view::read_set_here(
                        &harness,
                        crate::app_config::config_path().as_deref(),
                        aterm_types::dirs::home_dir().as_deref(),
                        now,
                    )
                })
                .unwrap_or_default()
        }
    }

    fn paths(&self, set: &Self::Set) -> Vec<PathBuf> {
        set.paths()
    }

    fn stamp(&mut self, set: &Self::Set) -> Self::Stamp {
        set.stamp()
    }

    fn read(&mut self, now: i64) -> Option<i64> {
        #[cfg(test)]
        {
            let _ = now;
            crate::work_counts::token_view_read();
            None
        }
        #[cfg(not(test))]
        {
            use aterm_agent::harness::accounts_token_view::{
                ask_lab_here, steps_here, view_changes_after, view_here,
            };
            // With no harness state there is nothing to read from: the facts stay as they were
            // (not read yet, before a first read), never "no vault".
            let harness = crate::harness_host::harness_state()?;
            let config = crate::app_config::config_path();
            let home = aterm_types::dirs::home_dir();
            let view = view_here(&harness, config.as_deref(), home.as_deref(), now);
            let due = view
                .as_deref()
                .and_then(|accounts| view_changes_after(accounts, now));
            if let (Some(accounts), Some(home)) = (&view, home.as_deref())
                && let Err(why) = ask_lab_here(accounts, home, now)
            {
                aterm_log::warn!("accounts: the lab could not be asked for a token: {why}");
            }
            let steps = view
                .as_deref()
                .map(|accounts| steps_here(&harness, home.as_deref(), accounts))
                .unwrap_or_default();
            crate::token_shown::post(crate::token_shown::facts_of_view(
                &harness,
                (view, &steps),
                now,
            ));
            due
        }
    }
}

/// A platform with no vault tokens has no vault to read, and no file to watch.
#[cfg(not(unix))]
impl View for Here {
    type Set = ();
    type Stamp = ();

    fn set(&mut self, _now: i64) {}

    fn paths(&self, (): &()) -> Vec<PathBuf> {
        Vec::new()
    }

    fn stamp(&mut self, (): &()) {}

    fn read(&mut self, _now: i64) -> Option<i64> {
        #[cfg(test)]
        crate::work_counts::token_view_read();
        #[cfg(not(test))]
        crate::token_shown::post(crate::token_shown::Tokens::no_vault());
        None
    }
}

/// The lane's watches: one [`FileWatch`] over the read set's paths, each told as [`Msg::Told`].
struct Files {
    door: Sender<Msg>,
    watch: Option<FileWatch>,
    /// Each watched path's key.
    keys: BTreeMap<PathBuf, u64>,
    next: u64,
    /// The watcher's refusal was logged.
    said: bool,
}

impl Files {
    fn new(door: Sender<Msg>) -> Self {
        Self {
            door,
            watch: None,
            keys: BTreeMap::new(),
            next: 0,
            said: false,
        }
    }
}

impl Watch for Files {
    fn place(&mut self, paths: &[PathBuf]) -> bool {
        if self.watch.is_none() {
            if paths.is_empty() {
                return true;
            }
            let door = self.door.clone();
            match FileWatch::platform(
                WATCH_THREAD,
                Box::new(move |_key| {
                    let _ = door.send(Msg::Told);
                }),
            ) {
                Ok(watch) => self.watch = Some(watch),
                Err(why) => {
                    if !self.said {
                        aterm_log::info!(
                            "accounts: no file watch for the token view ({why}): it is read on \
                             demand at most once a minute"
                        );
                        self.said = true;
                    }
                    return false;
                }
            }
        }
        let Some(watch) = self.watch.as_ref() else {
            return false;
        };
        self.keys.retain(|path, key| {
            let keep = paths.contains(path);
            if !keep {
                watch.unwatch(*key);
            }
            keep
        });
        for path in paths {
            if !self.keys.contains_key(path) {
                self.next += 1;
                watch.watch(self.next, path.clone());
                self.keys.insert(path.clone(), self.next);
            }
        }
        true
    }
}

#[cfg(test)]
#[path = "token_lane_tests.rs"]
mod tests;
