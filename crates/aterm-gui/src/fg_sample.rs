// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The foreground sampler of a PTY gather that no process watch serves (Linux, and a macOS
//! session whose kqueue watch could not open): while a block runs, the gather reads on a timer
//! which group holds the terminal and that group's leader's image, so a command that prints
//! nothing is still named ([`FgSampler`]). Each new image is heard in the session's ran log
//! placed where the shell's output stood while its group held the foreground
//! ([`crate::command_ran::RanFeed::sampled`]), so it counts for its own block alone and may stand
//! in for a word that is no program file, as an exec the watch hears does.
//!
//! Linux has no process watch here. Its proc connector reports each `exec` without root from
//! Linux 6.6 on, to a listener in the initial user and pid namespaces (before 6.6 its subscription
//! needs `CAP_NET_ADMIN`); but it reports every process on the machine, so one socket per session
//! would wake each session's gather for every fork, exec and exit of a build, and Linux cannot
//! count the bytes a master holds unread (`TIOCOUTQ` reads nothing there, and `FIONREAD` misses
//! what the pty has not flushed), so an exec it reports could be placed in its block only where the
//! master is quiet, which is where this sampler places what it sees
//! (`docs/DESIGN-session-log-2026-10-15.md` §10, H3's landing).

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::command_ran::RanFeed;

/// How soon after the gather hands on a batch the sampler looks at the parse stage: the batch may
/// hold a block's C, and a command's job takes the foreground about a millisecond after its shell
/// writes C. It is also each new block's first step, and the first wait before a look again while
/// the parse stage stays behind the gather.
pub(crate) const LOOK: Duration = Duration::from_millis(2);

/// The longest step between two samples while one block runs: the step starts at [`LOOK`] for
/// each block and doubles after each sample, so a block that runs for a minute is sampled about
/// 70 times.
pub(crate) const STEP_MAX: Duration = Duration::from_secs(1);

/// How many images a sampler remembers having heard.
pub(crate) const SEEN_MAX: usize = 16;

/// What one sample reads of a group's leader: its start and the executable the kernel mapped for
/// it, canonical (never a name the process gives itself). Two sightings are one image when all
/// three agree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Sight {
    /// The group, whose leader was read.
    pub(crate) pgid: i32,
    /// The leader's start, in microseconds on the platform's start clock ([`start_clock_us`]).
    pub(crate) start_us: u64,
    /// The executable the kernel mapped.
    pub(crate) exe: Arc<Path>,
}

/// Reads the leader of group `pgid` now (`None`: gone, a zombie, another user's, or not exec'd
/// since its fork).
pub(crate) type ReadSight = fn(i32) -> Option<Sight>;

/// `SessionFactory::fg_sight`: how a gather with no process watch reads what holds the terminal.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SightSeam {
    /// Reads a group's leader ([`read_sight`]).
    pub(crate) read: ReadSight,
    /// Now on the clock of [`Sight::start_us`], in microseconds ([`start_clock_us`]; `None`:
    /// unreadable, so no leader is ever held older than a block).
    pub(crate) now_us: fn() -> Option<u64>,
    /// How far a [`Sight::start_us`] can read before the leader's real start, in microseconds
    /// ([`start_tick_us`]; `None`: unknown, so no leader is ever held older than a block).
    pub(crate) tick_us: fn() -> Option<u64>,
}

/// The shipping seam: [`read_sight`], [`start_clock_us`] and [`start_tick_us`].
pub(crate) const SHIPPING: SightSeam = SightSeam {
    read: read_sight,
    now_us: start_clock_us,
    tick_us: start_tick_us,
};

/// Whether the shipping factory gives a gather with no process watch its sampler: always, but
/// `ATERM_DEV_NO_FGSAMPLE=1` in a dev-seams build turns it off ([`aterm_types::dev_seam!`]),
/// the control the sampler's cost is measured against (`docs/DESIGN-session-log-2026-10-15.md`
/// §10, unit L1).
pub(crate) fn sampler_wanted() -> bool {
    !aterm_types::dev_seam!("ATERM_DEV_NO_FGSAMPLE").is_some_and(|v| v == "1")
}

/// The shipping reader: once the leader has exec'd since its fork, its executable (Linux
/// `/proc/<pgid>/exe`, macOS `proc_pidpath`) and its start (Linux `/proc/<pgid>/stat`, macOS
/// its birth record), bracketed by the same start record and no file of the image's own. A leader that has not exec'd
/// (`aterm_uds::process::read_process_execd`: a shell's child between its fork and its exec,
/// or one whose command was not found, which never execs) still maps the shell's image, which
/// is no program the command ran, so it reads nothing; the flag is checked before the path,
/// between equal birth records. An exec sets it for good for that birth. A reused PID cannot
/// lend its predecessor's flag to a new fork.
pub(crate) fn read_sight(pgid: i32) -> Option<Sight> {
    read_sight_using(
        pgid,
        crate::holder_image::start_us_of,
        aterm_uds::process::read_process_execd,
        aterm_sysprobe::process_path,
    )
}

fn read_sight_using(
    pgid: i32,
    mut start: impl FnMut(i32) -> Option<u64>,
    execd: impl FnOnce(i32) -> Option<bool>,
    path: impl FnOnce(u32) -> Option<std::path::PathBuf>,
) -> Option<Sight> {
    let pid = u32::try_from(pgid).ok().filter(|p| *p > 1)?;
    let start_us = execd_start_using(pgid, &mut start, execd)?;
    let exe = path(pid)?;
    if start(pgid) != Some(start_us) {
        return None;
    }
    Some(Sight {
        pgid,
        start_us,
        exe: Arc::from(crate::holder_image::kept_exe(&exe)),
    })
}

/// The same birth on both sides of an affirmative exec flag, for pairing with a later holder
/// image's own birth. Unknown or changing process identity is no affirmative exec evidence.
pub(crate) fn execd_start(pgid: i32) -> Option<u64> {
    execd_start_using(
        pgid,
        crate::holder_image::start_us_of,
        aterm_uds::process::read_process_execd,
    )
}

fn execd_start_using(
    pgid: i32,
    mut start: impl FnMut(i32) -> Option<u64>,
    execd: impl FnOnce(i32) -> Option<bool>,
) -> Option<u64> {
    let before = start(pgid)?;
    (execd(pgid) == Some(true) && start(pgid) == Some(before)).then_some(before)
}

/// Now on the clock a leader's start is read on (`crate::holder_image::start_us_of`), in
/// microseconds: Linux's boot clock (`CLOCK_BOOTTIME`, which `/proc/<pid>/stat`'s start time
/// counts from), macOS's wall clock (a process's birth record); `None` elsewhere.
pub(crate) fn start_clock_us() -> Option<u64> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        aterm_uds::process::boot_clock_now().map(|c| c.0 / 1_000)
    }
    #[cfg(target_vendor = "apple")]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|d| u64::try_from(d.as_micros()).ok())
    }
    #[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
    {
        None
    }
}

/// How far a leader's start as read (`crate::holder_image::start_us_of`) can precede its real
/// start, in microseconds: one tick of Linux's `/proc/<pid>/stat` start time (10 ms at 100 a
/// second, `aterm_uds::process::proc_start_tick_ns`), a microsecond of macOS's birth record;
/// `None` elsewhere.
pub(crate) fn start_tick_us() -> Option<u64> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        aterm_uds::process::proc_start_tick_ns().map(|ns| ns.div_ceil(1_000))
    }
    #[cfg(target_vendor = "apple")]
    {
        Some(1)
    }
    #[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
    {
        None
    }
}

/// An image a sampler heard, by a sample or from the proc connector ([`FgSampler::connected`]).
#[derive(Debug)]
struct Seen {
    /// The process that mapped it (a sample's: the group's leader).
    pid: i32,
    /// Its start, on the start clock ([`Sight::start_us`]).
    start_us: u64,
    /// The executable.
    exe: Arc<Path>,
    /// The block executing where the parse stage stopped when it was heard (`None`: the parse
    /// stage had bytes still to parse, so the block it is placed in was not known yet).
    block: Option<u64>,
    /// Whether it was placed in the output ([`RanFeed::sampled`]); else by its stamp, which a
    /// later sample that places it may still hear once.
    exact: bool,
}

/// One gather's foreground sampler. It is due [`LOOK`] after each batch the gather hands on and
/// then, while the parse stage finds a block executing (or has bytes still to parse), after a
/// step that starts at [`LOOK`] for each new block and doubles to [`STEP_MAX`]; while the parse
/// stage has bytes still to parse it looks again on a wait that starts at [`LOOK`] and doubles,
/// since nothing wakes the gather when it catches up; once it has parsed every byte and no block
/// runs, nothing is due until the next batch, but for one settle look after a quiet stand-down
/// where a leader's start reads coarser than a microsecond (Linux). The gather's poll waits at
/// most until it is due ([`FgSampler::wait_ms`]) and ticks it when the wait ends with nothing to
/// read ([`FgSampler::tick`]).
///
/// It hears each image once ([`FgSampler::fresh`]): one exec counts for one block, so an image it
/// heard is not heard again in a later block (a stopped job resumed by `%1` exec'd nothing then),
/// nor one whose leader surely started before the last quiet prompt (a background job brought to
/// the front), and a re-exec is heard only in the block that heard the leader's earlier image.
#[derive(Debug)]
pub(crate) struct FgSampler {
    seam: SightSeam,
    /// When the next sample is due (`None`: none is, as no block runs).
    next: Option<Instant>,
    /// When the sampler looks at the parse stage again after a batch was handed on, or while the
    /// parse stage stays behind.
    look: Option<Instant>,
    /// The wait before the next look while the parse stage stays behind.
    look_step: Duration,
    /// The step after the next sample.
    step: Duration,
    /// The block a tick last found executing where the parse stage stopped.
    block: Option<u64>,
    /// The group whose leader the last sample read.
    read_pgid: Option<i32>,
    /// The images heard, oldest first, at most [`SEEN_MAX`].
    seen: VecDeque<Seen>,
    /// The start clock read just before the last stand-down found the master quiet: every block
    /// begun since began after it.
    quiet_us: Option<u64>,
    /// Whether a quiet stand-down since the last batch armed its settle look.
    settled: bool,
    /// Where the proc connector last said it lost messages, until the blocks the loss may fall
    /// in are over: meanwhile the connector's `exec`s are placed by their stamps, and the
    /// sampler alone stands in ([`FgSampler::connected`]).
    lost: Option<Lost>,
}

/// Where a loss the proc connector reported falls ([`FgSampler::connector_lost`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lost {
    /// Every byte the shell wrote by then was read and parsed, this block executing there
    /// (`None`: none was): the loss falls in it, and a block that runs after it is clean.
    In(Option<u64>),
    /// Bytes still waited, on the master or for the parse stage: a block may have begun in them
    /// that is not known yet, so every block runs lost until a quiet stand-down
    /// ([`FgSampler::stand_down`]), after which every block begun before it is over.
    UntilQuiet,
}

impl FgSampler {
    /// A sampler reading each leader through `seam`, nothing due.
    pub(crate) const fn new(seam: SightSeam) -> Self {
        Self {
            seam,
            next: None,
            look: None,
            look_step: LOOK,
            step: LOOK,
            block: None,
            read_pgid: None,
            seen: VecDeque::new(),
            quiet_us: None,
            settled: false,
            lost: None,
        }
    }

    /// The gather handed on a batch at `now`: look at the parse stage within [`LOOK`], and
    /// sample then too when no sample is due.
    pub(crate) fn handed(&mut self, now: Instant) {
        let soon = now + LOOK;
        self.look = Some(self.look.map_or(soon, |l| l.min(soon)));
        self.look_step = LOOK;
        self.settled = false;
        if self.next.is_none() {
            self.next = Some(soon);
            self.step = LOOK;
        }
    }

    /// How long the gather's poll may wait at `now`, in whole milliseconds rounded up (`-1`:
    /// nothing is due).
    pub(crate) fn wait_ms(&self, now: Instant) -> i32 {
        self.due().map_or(-1, |due| {
            let left = due
                .saturating_duration_since(now)
                .as_micros()
                .div_ceil(1_000);
            i32::try_from(left).unwrap_or(i32::MAX)
        })
    }

    fn due(&self) -> Option<Instant> {
        self.next.into_iter().chain(self.look).min()
    }

    /// The gather's poll waited until `now` with nothing to read, `fg` reading the terminal's
    /// foreground group and `unread` whether the master holds a byte unread. When due: with no
    /// block executing where the parse stage stopped and every byte parsed, it stands down
    /// (nothing is due until the next batch but a settle look), noting the start clock when the
    /// master is quiet; while bytes wait to be parsed it looks again later; it samples when a new block runs or
    /// its step is due, and at a look while bytes wait to be parsed when a group it has not just
    /// read holds the foreground (a block may have begun there), scheduling the next sample one
    /// step on after a stepped one. Returns whether a sample heard an image.
    pub(crate) fn tick(
        &mut self,
        now: Instant,
        feed: &RanFeed,
        fg: impl FnOnce() -> i32,
        unread: impl FnOnce() -> bool,
    ) -> bool {
        if self.due().is_none_or(|due| due > now) {
            return false;
        }
        self.look = None;
        let (running, behind) = feed.ran().parse_point();
        if running.is_none() && !behind {
            self.stand_down(now, unread);
            return false;
        }
        if behind {
            self.look = Some(now + self.look_step);
            self.look_step = (self.look_step * 2).min(STEP_MAX);
        }
        let new_block = running.is_some() && running != self.block;
        if new_block {
            self.block = running;
            self.step = LOOK;
        }
        let stepped = new_block || self.next.is_none_or(|next| next <= now);
        if !stepped && !behind {
            return false;
        }
        let heard = self.sample(feed, fg, unread, running.filter(|_| !behind), !stepped);
        if stepped {
            self.next = Some(now + self.step);
            self.step = (self.step * 2).min(STEP_MAX);
        }
        heard
    }

    /// No block runs and every byte is parsed at `now`: nothing is due until the next batch. The
    /// start clock is read first and kept when the master is then quiet: every byte the shell
    /// wrote before that was handed on and parsed with no block begun, so the next block's C, and
    /// its job, come after the reading, and every block a loss the proc connector reported may
    /// fall in is over ([`FgSampler::connector_lost`]). The first such stand-down after a batch looks once more
    /// a tick of the start record and a [`LOOK`] later when that tick is coarser than a
    /// microsecond: a job the last command left in the background forked before its D, a few
    /// milliseconds before this reading, and Linux may read its start up to a tick early, so only
    /// a reading a tick later surely follows it ([`FgSampler::fresh`]).
    fn stand_down(&mut self, now: Instant, unread: impl FnOnce() -> bool) {
        self.next = None;
        self.look = None;
        self.look_step = LOOK;
        self.step = LOOK;
        self.block = None;
        let at = (self.seam.now_us)();
        if unread() {
            return;
        }
        self.lost = None;
        self.quiet_us = at.or(self.quiet_us);
        if at.is_some() && !self.settled {
            self.settled = true;
            if let Some(tick) = (self.seam.tick_us)().filter(|t| *t > 1) {
                self.look = Some(now + Duration::from_micros(tick) + LOOK);
            }
        }
    }

    /// Read the foreground group; unless it is the shell's, or (`only_new`) the group whose
    /// leader the last sample read, or the master then holds a byte the shell wrote that the
    /// gather has not read (whose place before or after the read is not known: the next batch
    /// looks again), read its leader and hear its image if it is fresh for `block`
    /// ([`RanFeed::sampled`], [`FgSampler::fresh`]). The order is what places it: the foreground
    /// is read first, so a quiet master after it means every byte the shell wrote before the read
    /// was handed on.
    fn sample(
        &mut self,
        feed: &RanFeed,
        fg: impl FnOnce() -> i32,
        unread: impl FnOnce() -> bool,
        block: Option<u64>,
        only_new: bool,
    ) -> bool {
        let fg = fg();
        if fg <= 1 || fg == feed.shell() || (only_new && self.read_pgid == Some(fg)) || unread() {
            return false;
        }
        self.read_pgid = Some(fg);
        let Some(sight) = (self.seam.read)(fg) else {
            return false;
        };
        if !self.fresh(&sight, block) {
            return false;
        }
        feed.sampled(sight.pgid, &sight.exe);
        self.remember(Seen {
            pid: sight.pgid,
            start_us: sight.start_us,
            exe: sight.exe,
            block,
            exact: true,
        });
        true
    }

    fn remember(&mut self, seen: Seen) {
        if self.seen.len() >= SEEN_MAX {
            self.seen.pop_front();
        }
        self.seen.push_back(seen);
    }

    /// The proc connector said it lost messages (`ENOBUFS`, or the session's inbox was full), read
    /// with every byte the gather read handed on, `unread` saying whether the master holds one
    /// it has not: the lost `exec` may be any block's that began before now, so the connector's
    /// `exec`s only confirm ([`FgSampler::connected`]) until those blocks are over. With every
    /// byte parsed and none unread, that is the block executing where the parse stage stopped,
    /// over once another runs; with bytes still to parse or read, whose blocks are not known yet,
    /// it lasts until a quiet stand-down. A loss already lasting that long keeps doing so.
    pub(crate) fn connector_lost(&mut self, feed: &RanFeed, unread: impl FnOnce() -> bool) {
        let (running, behind) = feed.ran().parse_point();
        self.lost = Some(
            if behind || self.lost == Some(Lost::UntilQuiet) || unread() {
                Lost::UntilQuiet
            } else {
                Lost::In(running)
            },
        );
    }

    /// The proc connector routed `exec` (read by its listener's thread just after it was heard):
    /// hear its image in the session's ran log, unless the shell's own group mapped it, this
    /// sampler heard that image of that process already, or its group held the terminal's
    /// foreground neither when the listener read it (`tpgid`, just after the `exec`) nor now as
    /// `fg` reads it: a background job's or a hook's `exec` outside the foreground is no
    /// command's, so it confirms nothing, as macOS's watch hears it
    /// (`crate::exec_edges::ExecEdges`). It stands in, placed where the shell's output stood
    /// ([`RanFeed::connected`]), only where a sample would place it: its group holds the
    /// foreground now, the master then shows no byte unread, it exec'd after the last quiet
    /// prompt's reading (an older one is no later block's), and no loss the connector reported
    /// may fall in its block ([`FgSampler::connector_lost`]). Elsewhere (its process already
    /// gone and the terminal handed back, output still to read) it is placed by its stamp, and
    /// confirms a word whose file it is but never stands in; a later sample that places its
    /// image may still hear it once. Only a group leader's is remembered with what samples heard
    /// ([`SEEN_MAX`]): a sample reads leaders alone, so a build's thousand compiler `exec`s never
    /// push its leader out. Returns whether it was heard.
    pub(crate) fn connected(
        &mut self,
        exec: &aterm_uds::proc_connector::Exec,
        feed: &RanFeed,
        fg: impl FnOnce() -> i32,
        unread: impl FnOnce() -> bool,
    ) -> bool {
        let shell = feed.shell();
        if exec.pid <= 1 || exec.pgid <= 1 || exec.pid == shell || exec.pgid == shell {
            return false;
        }
        let exe: Arc<Path> = Arc::from(crate::holder_image::kept_exe(&exec.image.path));
        let start_us = exec.start_ns / 1_000;
        if self
            .seen
            .iter()
            .any(|s| s.pid == exec.pid && s.start_us == start_us && s.exe == exe)
        {
            return false;
        }
        let front = fg();
        if exec.tpgid != exec.pgid && front != exec.pgid {
            return false;
        }
        let (running, behind) = feed.ran().parse_point();
        if let Some(Lost::In(at)) = self.lost
            && running.is_some()
            && running != at
        {
            self.lost = None;
        }
        let exact = self.lost.is_none()
            && self.quiet_us.is_none_or(|quiet| exec.at_ns / 1_000 > quiet)
            && front == exec.pgid
            && !unread();
        feed.connected(exec, &exe, exact);
        if exec.pid == exec.pgid {
            self.remember(Seen {
                pid: exec.pid,
                start_us,
                exe,
                block: running.filter(|_| !behind),
                exact,
            });
        }
        true
    }

    /// Whether `sight`, read where the parse stage found `block` executing (`None`: behind), is an
    /// exec this sampler has not heard and may place: not when its leader surely started before
    /// the last quiet prompt's reading, its start as read plus the start record's tick
    /// ([`SightSeam::tick_us`]) at most that reading (it is older than every block begun since: a
    /// job resumed or brought to the front), nor when it heard this image (one exec counts for
    /// one block), and for a leader it heard with another image (a re-exec) only in a known
    /// block every earlier image of that leader was heard in or not placed in yet.
    fn fresh(&self, sight: &Sight, block: Option<u64>) -> bool {
        if let (Some(quiet), Some(tick)) = (self.quiet_us, (self.seam.tick_us)())
            && sight.start_us.saturating_add(tick) <= quiet
        {
            return false;
        }
        self.seen
            .iter()
            .filter(|s| s.exact && s.pid == sight.pgid && s.start_us == sight.start_us)
            .all(|s| {
                s.exe != sight.exe && block.is_some() && s.block.is_none_or(|b| Some(b) == block)
            })
    }
}

#[cfg(test)]
#[path = "fg_sample_tests.rs"]
mod tests;
