// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! macOS's own services' memory: which one holds far more than any service needs ([`size`]),
//! for how long ([`Judge`]), and the few aterm knows a safe restart for ([`RESTARTS`]).
//!
//! A service is a resource coalition whose live members all run as the system's own accounts
//! ([`system_uid`]), other than the kernel's own coalition ([`kernel_member`]). Its footprint is
//! the kernel's figure for the coalition, which reads without root where a root process's own
//! does not (`aterm_sysprobe::coalitions::footprint`).
//!
//! The bars come from one incident on a 128 GiB Mac (2026-10-26). fseventsd, whose launchd job
//! budgets it 30 MB and which reads 3.5 to 3.9 MB fresh, held 44.4 GiB when the kernel's first
//! memory-pressure kill named it, and 152 GB with swap full four days later. The largest
//! healthy services there were mds at 1.3 GB (5 GB at its peak) and WindowServer at 0.9 GB; a
//! WindowServer driving large displays on a 16 GiB Mac can pass 4 GiB, so neither bar is under
//! 8 GiB.
//!
//! This is early detection and a repair. Prevention is placement, elsewhere: aterm's build cache,
//! verify's side lanes, release targets and test scratch sit in `.ubd` folders, whose file events
//! fseventsd drops before it delivers or keeps them (`aterm_tempfile::QUIET`, observed on macOS
//! 27.0 and documented nowhere). Another program's churn still reaches it, and so does aterm's on
//! a macOS that stops honouring the name; this watch is for those.

use std::collections::HashMap;

use crate::explain::{CoalRow, Lead, Part, Voucher};
use crate::model::Snapshot;

/// macOS keeps the uids below this for its own accounts (root, `_windowserver`, `_spotlight`).
pub const SYSTEM_UID_BELOW: u32 = 500;

/// `nobody`'s uid (-2), a system account above [`SYSTEM_UID_BELOW`].
pub const NOBODY_UID: u32 = u32::MAX - 1;

/// A service is large from this footprint, or from [`LARGE_SHARE`] of memory where that is more.
pub const LARGE_FLOOR: u64 = 8 << 30;

/// A service is large from this fraction of the machine's memory (one sixteenth).
pub const LARGE_SHARE: u64 = 16;

/// A service is huge from this footprint, or from [`HUGE_SHARE`] of memory where that is more.
pub const HUGE_FLOOR: u64 = 16 << 30;

/// A service is huge from this fraction of the machine's memory (a quarter).
pub const HUGE_SHARE: u64 = 4;

/// A service leaves a size only once it falls this fraction (an eighth) under that size's bar
/// ([`leave_bar`]), so one that hovers at a bar keeps its size.
pub const LEAVE_SHARE: u64 = 8;

/// A service must read past the large bar this long before it is told: a burst (an update's
/// install) passes, a leak does not.
pub const HOLD_MS: u64 = 10 * 60_000;

/// Swap traffic (in plus out) from which memory is under pressure ([`Judge`]), as
/// [`crate::events::SWAP_ACTIVE_BPS`] reads swapping.
pub const SWAP_PRESSED_BPS: f64 = crate::events::SWAP_ACTIVE_BPS;

/// Whether `uid` is one of the system's own accounts.
#[must_use]
pub const fn system_uid(uid: u32) -> bool {
    uid < SYSTEM_UID_BELOW || uid == NOBODY_UID
}

/// Whether `pid` belongs to the kernel's own coalition: `kernel_task` (pid 0) and launchd
/// (pid 1), which the kernel places in it. Its footprint is mostly the kernel's, which no
/// restart frees, so it is never a service.
#[must_use]
pub const fn kernel_member(pid: u32) -> bool {
    pid <= 1
}

/// The footprint from which a service of a machine with `total` bytes of memory is large.
#[must_use]
pub fn large_bar(total: u64) -> u64 {
    LARGE_FLOOR.max(total / LARGE_SHARE)
}

/// The footprint from which a service of a machine with `total` bytes of memory is huge.
#[must_use]
pub fn huge_bar(total: u64) -> u64 {
    HUGE_FLOOR.max(total / HUGE_SHARE)
}

/// The footprint under which a service that reached `bar` leaves it ([`LEAVE_SHARE`]).
#[must_use]
pub const fn leave_bar(bar: u64) -> u64 {
    bar - bar / LEAVE_SHARE
}

/// The footprint from which the [`Judge`] follows a service on a machine with `total` bytes:
/// [`leave_bar`] of the large bar.
#[must_use]
pub fn followed_from(total: u64) -> u64 {
    leave_bar(large_bar(total))
}

/// How much a service holds, against what any service needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Size {
    /// Under the large bar.
    Usual,
    /// Past [`large_bar`].
    Large,
    /// Past [`huge_bar`].
    Huge,
}

impl Size {
    /// The wire word: `ok`, `large`, `huge`.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Usual => "ok",
            Self::Large => "large",
            Self::Huge => "huge",
        }
    }

    /// The size `footprint` bytes takes on a machine with `total` bytes for a service that had
    /// this size: it reaches a size at that size's bar and leaves it only under [`leave_bar`]
    /// of it.
    #[must_use]
    pub fn next(self, footprint: u64, total: u64) -> Self {
        let (large, huge) = (large_bar(total), huge_bar(total));
        match self {
            Self::Usual => size(footprint, total),
            Self::Huge if footprint >= leave_bar(huge) => Self::Huge,
            Self::Large | Self::Huge if footprint >= huge => Self::Huge,
            Self::Large | Self::Huge if footprint >= leave_bar(large) => Self::Large,
            Self::Large | Self::Huge => Self::Usual,
        }
    }
}

/// How much `footprint` bytes is for a service of a machine with `total` bytes of memory, read
/// once (no hysteresis: [`Size::next`] carries one across readings).
#[must_use]
pub fn size(footprint: u64, total: u64) -> Size {
    if footprint >= huge_bar(total) {
        Size::Huge
    } else if footprint >= large_bar(total) {
        Size::Large
    } else {
        Size::Usual
    }
}

/// A service aterm knows a safe restart for: launchd keeps its job alive, so a new one starts
/// once the old one has ended, and the restart was seen to free what the old one held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Restart {
    /// The kernel's short name for its process (`p_comm`), its image's basename.
    pub name: &'static str,
    /// Its image.
    pub path: &'static str,
    /// Its signing identifier, under Apple's voucher.
    pub signing_id: &'static str,
    /// The uid it runs as.
    pub uid: u32,
    /// What it is, in people's words.
    pub what: &'static str,
    /// What a restart costs, in people's words.
    pub cost: &'static str,
    /// The `aterm init --only` item that restarts it once a person types their password.
    pub init: &'static str,
}

/// fseventsd: `KeepAlive` in its launchd job. On 2026-10-26 a 152 GB one took 14.7 s from its
/// SIGTERM to its end (most of what it held was compressed or swapped out), and launchd spawned
/// the new one within milliseconds of that end; the new one held 3.7 MB, and swap fell from 59.4
/// to 3.5 GiB. The new one discards the volume's change history ("destroying old logs"), so Time
/// Machine and Spotlight scan the disk again, and for about half a minute it refuses clients
/// that register, so an app that starts watching files then misses changes.
pub const FSEVENTSD: Restart = Restart {
    name: "fseventsd",
    path: "/System/Library/Frameworks/CoreServices.framework/Versions/A/Frameworks/\
           FSEvents.framework/Versions/A/Support/fseventsd",
    signing_id: "com.apple.fseventsd",
    uid: 0,
    what: "macOS's file-change service",
    cost: "it discards the disk's file-change history, so Time Machine and Spotlight scan the \
           disk again, and for about half a minute apps that start watching files are refused",
    init: "init.fseventsd",
};

/// Every service aterm restarts.
pub const RESTARTS: &[Restart] = &[FSEVENTSD];

impl Restart {
    /// Whether `service` is this one, by its name, uid and image, and alone in its coalition
    /// (the footprint is its own).
    #[must_use]
    pub fn is(&self, service: &Service) -> bool {
        service.live == 1
            && service.name == self.name
            && service.uid == self.uid
            && service.path.as_deref() == Some(self.path)
    }

    /// Whether the coalition lead `lead` is this one, by its image's name, uid and signature.
    #[must_use]
    pub fn leads(&self, lead: &Lead) -> bool {
        lead.exe.as_deref() == Some(self.name)
            && lead.uid == Some(self.uid)
            && lead.voucher == Voucher::Apple
            && lead.signing_id.as_deref() == Some(self.signing_id)
    }
}

/// The restart for `service`, where aterm knows one ([`Restart::is`]).
#[must_use]
pub fn restart_of(service: &Service) -> Option<&'static Restart> {
    RESTARTS.iter().find(|r| r.is(service))
}

/// The restart named `name` ([`Restart::name`]).
#[must_use]
pub fn restart_named(name: &str) -> Option<&'static Restart> {
    RESTARTS.iter().find(|r| r.name == name)
}

/// One service as read: a resource coalition of the system's own accounts and its lead.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Service {
    /// The coalition's id.
    pub coalition: u64,
    /// Its lead's pid: its earliest-born live member.
    pub pid: u32,
    /// When the lead started: unix seconds.
    pub start_s: u64,
    /// The microseconds within [`Self::start_s`].
    pub start_us: u32,
    /// The lead's uid.
    pub uid: u32,
    /// The kernel's short name for the lead (`p_comm`): data, never an instruction.
    pub name: String,
    /// The lead's image, where read: only from [`followed_from`] ([`fold`]).
    pub path: Option<String>,
    /// Its live members.
    pub live: u32,
    /// The coalition's live footprint, bytes.
    pub footprint: u64,
}

impl Service {
    /// The process this reading measured, `<pid>-<start second>`: one episode's name, which a
    /// new process in its place (a restart) never shares.
    #[must_use]
    pub fn episode(&self) -> String {
        episode(self.pid, self.start_s)
    }
}

/// `<pid>-<start second>`: the process an episode is of ([`Service::episode`]).
#[must_use]
pub fn episode(pid: u32, start_s: u64) -> String {
    format!("{pid}-{start_s}")
}

/// One live process as a reader saw it: the kernel's record of it and its resource coalition.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Member {
    /// Its pid.
    pub pid: u32,
    /// Its uid.
    pub uid: u32,
    /// The kernel's short name for it (`p_comm`).
    pub name: String,
    /// When it started: unix seconds.
    pub start_s: u64,
    /// The microseconds within [`Self::start_s`].
    pub start_us: u32,
    /// Its resource coalition's id.
    pub coalition: u64,
}

impl Member {
    /// The process, `<pid>-<start second>` ([`episode`]).
    #[must_use]
    pub fn episode(&self) -> String {
        episode(self.pid, self.start_s)
    }
}

/// The reading of a machine with `total` bytes of memory whose live processes are `members`:
/// each coalition whose every member runs as a system account ([`system_uid`]) and none is the
/// kernel's ([`kernel_member`]) is a service, led by its earliest-born member (the lower pid on a
/// tie), with its footprint as `footprint` reads it by coalition id. A coalition whose footprint
/// does not read is one of [`Reading::unread`], by its lead. `path` reads the image of each lead
/// from [`followed_from`], the only ones a restart is matched to. The memory strain is left
/// unread ([`Reading::pressure_warn`], [`Reading::swapped_kib`]) for the reader to fill.
pub fn fold(
    members: &[Member],
    total: u64,
    mut footprint: impl FnMut(u64) -> Option<u64>,
    mut path: impl FnMut(u32) -> Option<String>,
) -> Reading {
    let mut groups: HashMap<u64, Vec<&Member>> = HashMap::new();
    for m in members {
        groups.entry(m.coalition).or_default().push(m);
    }
    let mut out = Vec::new();
    let mut unread = Vec::new();
    for (coalition, group) in groups {
        if !group
            .iter()
            .all(|m| !kernel_member(m.pid) && system_uid(m.uid))
        {
            continue;
        }
        let Some(lead) = group.iter().min_by_key(|m| (m.start_s, m.start_us, m.pid)) else {
            continue;
        };
        let Some(footprint) = footprint(coalition) else {
            unread.push((*lead).clone());
            continue;
        };
        out.push(Service {
            coalition,
            pid: lead.pid,
            start_s: lead.start_s,
            start_us: lead.start_us,
            uid: lead.uid,
            name: lead.name.clone(),
            path: None,
            live: u32::try_from(group.len()).unwrap_or(u32::MAX),
            footprint,
        });
    }
    out.sort_by(|a, b| {
        b.footprint
            .cmp(&a.footprint)
            .then(a.coalition.cmp(&b.coalition))
    });
    unread.sort_by_key(|m| (m.coalition, m.pid));
    let from = followed_from(total);
    for service in out.iter_mut().filter(|s| s.footprint >= from) {
        service.path = path(service.pid);
    }
    Reading {
        total,
        services: out,
        unread,
        pressure_warn: None,
        swapped_kib: None,
    }
}

/// One read of every service, and of the memory strain they are read against.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reading {
    /// The machine's physical memory, bytes.
    pub total: u64,
    /// Every service whose footprint was read, largest first.
    pub services: Vec<Service>,
    /// The lead of each service whose footprint did not read: not under its bar, unknown.
    pub unread: Vec<Member>,
    /// Whether the kernel's memory pressure level is past normal; `None` where unread.
    pub pressure_warn: Option<bool>,
    /// KiB swapped in plus out since boot; `None` where unread.
    pub swapped_kib: Option<u64>,
}

/// One service the [`Judge`] follows: read from [`followed_from`], or unread since it was.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seen {
    /// The service, as last read.
    pub service: Service,
    /// How much it holds, with [`Size::next`]'s margin: [`Size::Usual`] for one within the
    /// margin under the large bar that has not reached it.
    pub size: Size,
    /// When it reached the large bar this episode, on the judge's clock.
    pub since_ms: Option<u64>,
    /// Whether it has held past the large bar for [`HOLD_MS`] in this judge's life: a judge
    /// newer than the episode (aterm relaunched) holds anew, and its not-yet-held is no end.
    pub held: bool,
    /// Its restart, where aterm knows one: kept from an earlier reading of this process when
    /// this one did not read its image.
    pub restart: Option<&'static Restart>,
}

impl Seen {
    /// Whether this is told: held past the large bar for the hold.
    #[must_use]
    pub fn told(&self) -> bool {
        self.held && self.size != Size::Usual
    }

    /// An earlier aterm process told this episode (the window still holds its entry or its
    /// alert, `at_least` the size it told): held from the reading at `at_ms`, at the size
    /// [`Size::next`] gives from `at_least` or more, so a relaunched watch neither waits out the
    /// hold again nor reads a dip within the margin as the end. `false` for one held already
    /// (this judge's own reading stands) or under the margin of every bar.
    pub fn carry(&mut self, at_least: Size, total: u64, at_ms: u64) -> bool {
        let size = self.size.max(at_least.next(self.service.footprint, total));
        if self.held || size == Size::Usual {
            return false;
        }
        self.size = size;
        self.held = true;
        self.since_ms = self.since_ms.or(Some(at_ms));
        true
    }
}

/// What the [`Judge`] says after a reading: every service it follows, and the strain.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Told {
    /// The machine's physical memory, bytes.
    pub total: u64,
    /// Every service followed, largest first. A service absent here and from
    /// [`Self::unread`] ended its episode: it fell under [`followed_from`], or its process was
    /// replaced or ended.
    pub services: Vec<Seen>,
    /// The lead of each service whose footprint this reading did not read.
    pub unread: Vec<Member>,
    /// Whether memory is under pressure now: the kernel's level past normal, or swap moving at
    /// [`SWAP_PRESSED_BPS`] or more since the reading before.
    pub pressed: bool,
    /// When the reading was taken, on the judge's clock.
    pub at_ms: u64,
}

impl Told {
    /// The services told ([`Seen::told`]), largest first.
    pub fn told(&self) -> impl Iterator<Item = &Seen> {
        self.services.iter().filter(|s| s.told())
    }

    /// [`Seen::carry`] for the service of `episode` ([`Service::episode`]), where followed.
    pub fn carry(&mut self, episode: &str, at_least: Size) -> bool {
        let (total, at_ms) = (self.total, self.at_ms);
        self.services
            .iter_mut()
            .find(|s| s.service.episode() == episode)
            .is_some_and(|s| s.carry(at_least, total, at_ms))
    }
}

/// The readings so far, folded into the services that hold far too much.
#[derive(Clone, Debug, Default)]
pub struct Judge {
    /// Each service followed, by its lead's pid and start.
    followed: HashMap<(u32, u64, u32), Seen>,
    /// The last swap counter read and when: `(at_ms, kib)`.
    swap: Option<(u64, u64)>,
    told: Option<Told>,
}

impl Judge {
    /// Nothing read.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Folds in the reading taken at `at_ms` and returns what it tells; `None`, a reading that
    /// failed, changes nothing and returns the last. A service is followed from
    /// [`followed_from`], takes its size by [`Size::next`], and is held once past the large
    /// bar for [`HOLD_MS`] (or carried, [`Self::carry`]) until it leaves every size; a service
    /// whose footprint did not read keeps what it was; a new process in a service's place
    /// starts over.
    pub fn absorb(&mut self, at_ms: u64, reading: Option<&Reading>) -> Option<&Told> {
        let Some(reading) = reading else {
            return self.told.as_ref();
        };
        let total = reading.total;
        let from = followed_from(total);
        let mut followed = HashMap::new();
        for service in reading.services.iter().filter(|s| s.footprint >= from) {
            let key = (service.pid, service.start_s, service.start_us);
            let was = self.followed.get(&key);
            let size = was
                .map_or(Size::Usual, |w| w.size)
                .next(service.footprint, total);
            let since_ms =
                (size != Size::Usual).then(|| was.and_then(|w| w.since_ms).unwrap_or(at_ms));
            let restart = if service.live != 1 {
                None
            } else if service.path.is_some() {
                restart_of(service)
            } else {
                was.and_then(|w| w.restart)
            };
            let held = since_ms.is_some_and(|s| at_ms.saturating_sub(s) >= HOLD_MS)
                || (since_ms.is_some() && was.is_some_and(|w| w.held));
            followed.insert(
                key,
                Seen {
                    service: service.clone(),
                    size,
                    since_ms,
                    held,
                    restart,
                },
            );
        }
        for m in &reading.unread {
            let key = (m.pid, m.start_s, m.start_us);
            if let Some(was) = self.followed.get(&key) {
                followed.insert(key, was.clone());
            }
        }
        let swap_bps = match (self.swap, reading.swapped_kib) {
            (Some((then_ms, then_kib)), Some(kib)) if at_ms > then_ms => {
                kib.saturating_sub(then_kib) as f64 * 1024.0 * 1000.0 / (at_ms - then_ms) as f64
            }
            _ => 0.0,
        };
        if let Some(kib) = reading.swapped_kib {
            self.swap = Some((at_ms, kib));
        }
        let mut services: Vec<Seen> = followed.values().cloned().collect();
        services.sort_by(|a, b| {
            b.service
                .footprint
                .cmp(&a.service.footprint)
                .then(a.service.coalition.cmp(&b.service.coalition))
        });
        self.followed = followed;
        self.told = Some(Told {
            total,
            services,
            unread: reading.unread.clone(),
            pressed: reading.pressure_warn == Some(true) || swap_bps >= SWAP_PRESSED_BPS,
            at_ms,
        });
        self.told.as_ref()
    }

    /// An earlier aterm process told the service of `episode` ([`Seen::carry`]): it is held from
    /// the last reading on, at `at_least` or more. `true` when that changed what the last
    /// reading tells.
    pub fn carry(&mut self, episode: &str, at_least: Size) -> bool {
        let Some(told) = self.told.as_mut() else {
            return false;
        };
        let (total, at_ms) = (told.total, told.at_ms);
        let carried = self
            .followed
            .values_mut()
            .find(|s| s.service.episode() == episode)
            .is_some_and(|s| s.carry(at_least, total, at_ms));
        told.carry(episode, at_least) && carried
    }

    /// What the last reading told; `None` before one.
    #[must_use]
    pub fn told(&self) -> Option<&Told> {
        self.told.as_ref()
    }

    /// The services the last reading told ([`Seen::told`]), largest first.
    pub fn over(&self) -> impl Iterator<Item = &Seen> {
        self.told.iter().flat_map(Told::told)
    }

    /// The `status` word: `-` before a reading, else the largest told [`Size::word`].
    #[must_use]
    pub fn word(&self) -> &'static str {
        if self.told.is_none() {
            return "-";
        }
        self.over()
            .map(|o| o.size)
            .max()
            .unwrap_or(Size::Usual)
            .word()
    }
}

/// One service past the large bar, as a split's row or a direct read names it ([`over_in`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowOver {
    /// The coalition's id.
    pub coalition: u64,
    /// Its lead.
    pub lead: Lead,
    /// Its live footprint, bytes.
    pub footprint: u64,
    /// How much it holds.
    pub size: Size,
    /// Its restart, where aterm knows one and the lead holds the coalition alone.
    pub restart: Option<&'static Restart>,
    /// Its lead's process, `<pid>-<start second>` ([`episode`]).
    pub episode: String,
}

impl RowOver {
    /// Whether its restart is due now: aterm knows one, and it holds past the huge bar while
    /// memory is under pressure (`pressed`), as the window raises its alert. Past only the large
    /// bar, or with memory not pressed, it is a fact for later.
    #[must_use]
    pub fn restart_due(&self, pressed: bool) -> bool {
        pressed && self.size == Size::Huge && self.restart.is_some()
    }
}

fn largest_first(out: &mut [RowOver]) {
    out.sort_by(|a, b| {
        b.footprint
            .cmp(&a.footprint)
            .then(a.coalition.cmp(&b.coalition))
    });
}

/// The services among `rows` (services' part, a lead of a system account outside the kernel's
/// coalition, a footprint read) past the large bar of a machine with `total` bytes, largest
/// first; a restart only for a lead alone in its coalition.
#[must_use]
pub fn rows_over(rows: &[CoalRow], total: u64) -> Vec<RowOver> {
    let mut out: Vec<RowOver> = rows
        .iter()
        .filter(|r| r.part == Part::Services)
        .filter_map(|r| {
            let lead = r.lead.as_ref()?;
            let footprint = r.footprint?;
            let size = size(footprint, total);
            (!kernel_member(lead.pid) && lead.uid.is_some_and(system_uid) && size != Size::Usual)
                .then(|| RowOver {
                    coalition: r.id,
                    lead: lead.clone(),
                    footprint,
                    size,
                    restart: RESTARTS.iter().find(|x| r.live == 1 && x.leads(lead)),
                    episode: episode(
                        lead.pid,
                        u64::try_from(lead.birth_ms.div_euclid(1000)).unwrap_or(0),
                    ),
                })
        })
        .collect();
    largest_first(&mut out);
    out
}

/// The services of a direct read past its large bar, largest first ([`fold`] left out the
/// kernel's coalition and every one of another account's).
#[must_use]
pub fn reading_over(reading: &Reading) -> Vec<RowOver> {
    let mut out: Vec<RowOver> = reading
        .services
        .iter()
        .filter_map(|s| {
            let size = size(s.footprint, reading.total);
            (size != Size::Usual).then(|| RowOver {
                coalition: s.coalition,
                lead: Lead {
                    pid: s.pid,
                    uid: Some(s.uid),
                    exe: Some(s.name.clone()),
                    ..Lead::default()
                },
                footprint: s.footprint,
                size,
                restart: restart_of(s),
                episode: s.episode(),
            })
        })
        .collect();
    largest_first(&mut out);
    out
}

/// The services past the large bar in `snap`: its own read of them where one was taken
/// ([`Snapshot::services`], a `sys why` with the split cold), else [`rows_over`] of its split by
/// coalition against its memory total; none without either.
#[must_use]
pub fn over_in(snap: &Snapshot) -> Vec<RowOver> {
    if let Some(reading) = snap.services.as_deref() {
        return reading_over(reading);
    }
    let (Some(facts), Some(mem)) = (
        snap.why.as_deref().filter(|f| f.lane.on()),
        snap.mem.as_ref(),
    ) else {
        return Vec::new();
    };
    rows_over(&facts.split.rows, mem.sample.total)
}

#[cfg(test)]
#[path = "services_tests.rs"]
mod tests;
