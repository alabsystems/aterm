// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What Commons repair did in this process, counted: its rounds by direction, its serving
//! loops' ticks and why each loop ended, what its transports read and started, and what each
//! round this side dialled did to its endpoint's schedule (which turn chose the dial, which table
//! turn the round took, why it asked, and whether its endpoint then rested), process-wide and per
//! seat ([`seats`]). Each count only rises, from the process's start, whichever runtime or
//! transport did the work; a reader takes two readings and subtracts. They decide nothing: no
//! schedule reads them.

use super::{RepairRound, discovery::Verdict};
use crate::replica::{Ask, TableTurn};
use astream_wire::sync::Actor;
use std::{
    collections::BTreeMap,
    sync::{
        Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
};

/// A closed family of counted words ([`Event`]): each of its values in their order, and the word
/// a reader sees for each.
pub(crate) trait Family: Copy + 'static {
    /// Every value, in the order its counts are kept and read.
    const ALL: &'static [Self];
    /// The word a reader sees.
    fn word(self) -> &'static str;
}

/// What a round this side dialled did to its endpoint's schedule (`super::discovery::Hints::
/// finish`): the reason its next dial comes when it does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Next {
    /// It rests until its audit: the round moved nothing, owes nothing, its scan has cycled, its
    /// audit is complete and not yet due, and this Mac owes its seat no round before then.
    Rest,
    /// The round moved records: the endpoint follows up.
    Transferred,
    /// Its hints name a follow-up still to take.
    OwesFollowUp,
    /// Its hints name a led table still to take.
    OwesLead,
    /// Its hints name a table whose witness differed, not yet visited.
    OwesDiffering,
    /// Its scan has not yet taken its turn at each table both list.
    Sweeping,
    /// Its audit is due and not yet complete, or none has completed yet.
    Auditing,
    /// It would rest, but a push hurried it.
    Pushed,
    /// It would rest until its audit, but what its rest stood on moved while the round ran,
    /// which brought its next dial sooner.
    Unsettled,
    /// It would rest until its audit, or until the sooner dial an unsettling brought, but this
    /// Mac owes its seat a round due before then (`super::discovery`'s `Owed`), which its walk
    /// dials ([`Dial::Owed`]): the walk does not read the endpoint's own wait.
    OwesRound,
    /// The round deferred its table.
    Deferred,
    /// The round deferred its table for this side's quota.
    DeferredQuota,
    /// The round's membership or table exchange broke off after its control exchange.
    Unfinished,
    /// The round ended unfinished after its peer authenticated or took this side's hello
    /// (contention, a peer that closed mid-round), or failed to start here.
    Busy,
    /// The round never reached the peer's answer.
    Failed,
    /// The round asked the peer nothing: this side stopped it, its LAN was held, or its worker
    /// panicked. A transport's stop at a generation's end makes these.
    Unasked,
}

impl Next {
    const ALL: [Self; 16] = [
        Self::Rest,
        Self::Transferred,
        Self::OwesFollowUp,
        Self::OwesLead,
        Self::OwesDiffering,
        Self::Sweeping,
        Self::Auditing,
        Self::Pushed,
        Self::Unsettled,
        Self::OwesRound,
        Self::Deferred,
        Self::DeferredQuota,
        Self::Unfinished,
        Self::Busy,
        Self::Failed,
        Self::Unasked,
    ];

    const fn word(self) -> &'static str {
        match self {
            Self::Rest => "rest",
            Self::Transferred => "transferred",
            Self::OwesFollowUp => "owes-follow-up",
            Self::OwesLead => "owes-lead",
            Self::OwesDiffering => "owes-differing",
            Self::Sweeping => "sweeping",
            Self::Auditing => "auditing",
            Self::Pushed => "pushed",
            Self::Unsettled => "unsettled",
            Self::OwesRound => "owes-round",
            Self::Deferred => "deferred",
            Self::DeferredQuota => "deferred-quota",
            Self::Unfinished => "unfinished",
            Self::Busy => "busy",
            Self::Failed => "failed",
            Self::Unasked => "unasked",
        }
    }

    /// What `owed` names a round owes ([`crate::control::Repair::owed`]).
    pub(crate) const fn owes(owed: crate::replica::Owed) -> Self {
        match owed {
            crate::replica::Owed::FollowUp => Self::OwesFollowUp,
            crate::replica::Owed::Lead => Self::OwesLead,
            crate::replica::Owed::Differing => Self::OwesDiffering,
        }
    }
}

/// What chose a dial (`super::discovery::Hints::next_allowed`), and what it was besides.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Dial {
    /// The follow-up queue chose it.
    FollowUp,
    /// It led ahead of ordinary discovery (a candidate this lab tagged, remembered or kin).
    Lead,
    /// Its audit was due.
    Audit,
    /// The ordinary scan came to it.
    Scan,
    /// A walk chose it, to offer what moved here.
    Walk,
    /// A walk chose it for a round this Mac owed its seat (`super::discovery`'s `Owed`), with
    /// nothing that moved here to offer it.
    Owed,
    /// It was the probe of its endpoint's silence.
    Probe,
    /// Its endpoint was not verified: a seat first met, or met again at a new port, a new
    /// generation or after its endpoint expired. The dial restarts the seat's cycle.
    MetAgain,
    /// Its endpoint was last reached so long ago by the wall clock that the dial restarts the
    /// seat's cycle.
    Away,
    /// Its round was the first to trade lists since a restart of the seat's cycle that threw away
    /// no completed one.
    SweepRestarted,
    /// Its round was the first to trade lists since a restart of the seat's cycle that threw away
    /// a completed one.
    SweepDiscarded,
}

impl Dial {
    const ALL: [Self; 11] = [
        Self::FollowUp,
        Self::Lead,
        Self::Audit,
        Self::Scan,
        Self::Walk,
        Self::Owed,
        Self::Probe,
        Self::MetAgain,
        Self::Away,
        Self::SweepRestarted,
        Self::SweepDiscarded,
    ];

    const fn word(self) -> &'static str {
        match self {
            Self::FollowUp => "follow-up",
            Self::Lead => "lead",
            Self::Audit => "audit",
            Self::Scan => "scan",
            Self::Walk => "walk",
            Self::Owed => "owed",
            Self::Probe => "probe",
            Self::MetAgain => "met-again",
            Self::Away => "away",
            Self::SweepRestarted => "sweep-restarted",
            Self::SweepDiscarded => "sweep-discarded",
        }
    }
}

/// How a serving loop (`super::serve_paced`) opened and why it ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Loop {
    /// A repairing loop opened: a new generation's scheduler.
    OpenedServing,
    /// A presence loop opened: it dials nothing.
    OpenedPresence,
    /// It ended for a generation refresh.
    Refresh,
    /// It ended because the vault's observation moved, or Commons control came into view.
    Vault,
    /// It ended because an enrollment's witness came into view.
    Witnessed,
    /// A tick refused to go on (a recheck failed, a window claimed Commons, the configuration
    /// turned it off).
    Refused,
    /// Its stop was set.
    Stopped,
}

impl Loop {
    const ALL: [Self; 7] = [
        Self::OpenedServing,
        Self::OpenedPresence,
        Self::Refresh,
        Self::Vault,
        Self::Witnessed,
        Self::Refused,
        Self::Stopped,
    ];

    const fn word(self) -> &'static str {
        match self {
            Self::OpenedServing => "opened-serving",
            Self::OpenedPresence => "opened-presence",
            Self::Refresh => "refresh",
            Self::Vault => "vault",
            Self::Witnessed => "witnessed",
            Self::Refused => "refused",
            Self::Stopped => "stopped",
        }
    }
}

impl Family for Next {
    const ALL: &'static [Self] = &Self::ALL;
    fn word(self) -> &'static str {
        Self::word(self)
    }
}

impl Family for Dial {
    const ALL: &'static [Self] = &Self::ALL;
    fn word(self) -> &'static str {
        Self::word(self)
    }
}

impl Family for Loop {
    const ALL: &'static [Self] = &Self::ALL;
    fn word(self) -> &'static str {
        Self::word(self)
    }
}

impl Family for TableTurn {
    const ALL: &'static [Self] = &[
        Self::Pushed,
        Self::Batch,
        Self::Retry,
        Self::Led,
        Self::Hinted,
        Self::Visit,
        Self::Regular,
    ];
    fn word(self) -> &'static str {
        match self {
            Self::Pushed => "pushed",
            Self::Batch => "batch",
            Self::Retry => "retry",
            Self::Led => "led",
            Self::Hinted => "hinted",
            Self::Visit => "visit",
            Self::Regular => "regular",
        }
    }
}

impl Family for Ask {
    const ALL: &'static [Self] = &[Self::First, Self::Gap, Self::Audit, Self::Asked, Self::Peer];
    fn word(self) -> &'static str {
        match self {
            Self::First => "first",
            Self::Gap => "gap",
            Self::Audit => "audit",
            Self::Asked => "asked",
            Self::Peer => "peer",
        }
    }
}

/// One counted event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Event {
    /// A round this side dialled ended, whatever it came to (`Transport::finish`).
    RoundOut,
    /// A round a peer dialled in ended, whatever it came to.
    RoundIn,
    /// One of those rounds completed with its peer authenticated and moved nothing.
    RoundQuiet,
    /// A serving loop ran a tick (`super::serve_paced`).
    OwnerTick,
    /// A transport read a discovery datagram that its multicast socket's checks passed.
    Beacon,
    /// A transport's look found this Mac's Commons state moved (`super::moved`), which its
    /// dials then offer: one count a look, however much moved.
    Announce,
    /// An exchange started its `db-exchange-deadline` thread (`crate::network::Deadline`).
    DeadlineSpawn,
    /// A transport wrote its remembered endpoints' file (`commons-peers.state`), whether or not
    /// the write held: once the seats it keeps change, or their times are an hour stale.
    RememberedWrite,
    /// What a round this side dialled did to its endpoint's schedule ([`own_round`]).
    Next(Next),
    /// What chose a dial, and what it was besides ([`own_round`]).
    Dial(Dial),
    /// Which table turn a round this side dialled took ([`own_round`]).
    Turn(TableTurn),
    /// Why a round this side dialled asked which tables differ ([`own_round`]).
    Ask(Ask),
    /// A serving loop opened or ended ([`Loop`]).
    Loop(Loop),
    /// A transport dropped a verified endpoint not reached for its expiry ([`expired`]).
    Expired,
    /// A round was refused because a round with its seat was already in flight here
    /// ([`seat_busy`]).
    SeatBusy,
    /// A writer pass of this Mac's Commons storage could not finish some ring's retirement (an
    /// old period's storage it could not delete, a state it could not write) and went on with
    /// what that retirement left written (`crate::catalog::Directory::retired`): one count a
    /// ring a pass.
    Unretired,
}

const NEXT: usize = 8;
const DIAL: usize = NEXT + Next::ALL.len();
const TURN: usize = DIAL + Dial::ALL.len();
const ASK: usize = TURN + <TableTurn as Family>::ALL.len();
const LOOP: usize = ASK + <Ask as Family>::ALL.len();
const EXPIRED: usize = LOOP + Loop::ALL.len();
const SEAT_BUSY: usize = EXPIRED + 1;
const UNRETIRED: usize = SEAT_BUSY + 1;
const EVENTS: usize = UNRETIRED + 1;

impl Event {
    /// Where `self` is counted in [`COUNTS`]: each family's words follow one another.
    const fn slot(self) -> usize {
        match self {
            Self::RoundOut => 0,
            Self::RoundIn => 1,
            Self::RoundQuiet => 2,
            Self::OwnerTick => 3,
            Self::Beacon => 4,
            Self::Announce => 5,
            Self::DeadlineSpawn => 6,
            Self::RememberedWrite => 7,
            Self::Next(next) => NEXT + next as usize,
            Self::Dial(dial) => DIAL + dial as usize,
            Self::Turn(turn) => TURN + turn as usize,
            Self::Ask(ask) => ASK + ask as usize,
            Self::Loop(end) => LOOP + end as usize,
            Self::Expired => EXPIRED,
            Self::SeatBusy => SEAT_BUSY,
            Self::Unretired => UNRETIRED,
        }
    }
}

static COUNTS: [AtomicU64; EVENTS] = [const { AtomicU64::new(0) }; EVENTS];

#[cfg(test)]
thread_local! {
    /// The calling thread's own counts ([`cadence_here`]).
    static HERE: [std::cell::Cell<u64>; EVENTS] = const { [const { std::cell::Cell::new(0) }; EVENTS] };
}

/// Count `event` once.
pub(crate) fn count(event: Event) {
    COUNTS[event.slot()].fetch_add(1, Ordering::Relaxed);
    #[cfg(test)]
    HERE.with(|here| {
        let cell = &here[event.slot()];
        cell.set(cell.get() + 1);
    });
}

/// Commons repair's counts in this process since it started ([`cadence`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cadence {
    /// Rounds this side dialled that ended, whatever each came to.
    pub rounds_out: u64,
    /// Rounds peers dialled in that ended, whatever each came to.
    pub rounds_in: u64,
    /// Rounds of either direction that completed with their peer authenticated and moved
    /// nothing.
    pub rounds_quiet: u64,
    /// Ticks its serving loops ran.
    pub owner_ticks: u64,
    /// Discovery datagrams its transports read.
    pub beacons: u64,
    /// Looks of its transports that found this Mac's Commons state moved, whoever wrote it: one
    /// a look, however much moved.
    pub announces: u64,
    /// Fleet book entries' signatures this process checked, by any caller
    /// (`aterm_net::fleet_member::signature_checks`).
    pub signature_checks: u64,
    /// `db-exchange-deadline` threads its exchanges started.
    pub deadline_spawns: u64,
    /// Writes of its transports' remembered endpoints' files.
    pub remembered_writes: u64,
    /// Verified endpoints its transports dropped, not reached for their expiry.
    pub expired: u64,
    /// Rounds refused because a round with the same seat was already in flight here, either
    /// side's.
    pub seat_busy: u64,
    /// Writer passes that could not finish some ring's retirement and went on with what it left
    /// written, one count a ring a pass: an old period this Mac cannot delete, say, stays one of
    /// its ring's held periods.
    pub unretired: u64,
    /// The most table rounds this side dialled to one seat in a row with no regular turn among
    /// them, of the seats [`seats`] keeps.
    pub run_max: u64,
    /// Every event's count, by [`Event::slot`].
    counts: [u64; EVENTS],
}

impl Cadence {
    fn of(read: impl Fn(Event) -> u64, signature_checks: u64, run_max: u64) -> Self {
        let mut counts = [0; EVENTS];
        for event in every_event() {
            counts[event.slot()] = read(event);
        }
        Self {
            rounds_out: read(Event::RoundOut),
            rounds_in: read(Event::RoundIn),
            rounds_quiet: read(Event::RoundQuiet),
            owner_ticks: read(Event::OwnerTick),
            beacons: read(Event::Beacon),
            announces: read(Event::Announce),
            signature_checks,
            deadline_spawns: read(Event::DeadlineSpawn),
            remembered_writes: read(Event::RememberedWrite),
            expired: read(Event::Expired),
            seat_busy: read(Event::SeatBusy),
            unretired: read(Event::Unretired),
            run_max,
            counts,
        }
    }

    /// How many times `event` was counted.
    pub(crate) const fn get(&self, event: Event) -> u64 {
        self.counts[event.slot()]
    }

    /// The counts of each family a round's end or a loop's tells, each as the words counted at
    /// least once with their counts, in the family's order: `commons_next` ([`Next`]),
    /// `commons_dials` ([`Dial`]), `commons_turns` ([`TableTurn`]), `commons_asks` ([`Ask`]) and
    /// `commons_loops` ([`Loop`]).
    #[must_use]
    pub fn ledgers(&self) -> [(&'static str, Vec<(&'static str, u64)>); 5] {
        [
            ("commons_next", self.ledger(Event::Next)),
            ("commons_dials", self.ledger(Event::Dial)),
            ("commons_turns", self.ledger(Event::Turn)),
            ("commons_asks", self.ledger(Event::Ask)),
            ("commons_loops", self.ledger(Event::Loop)),
        ]
    }

    fn ledger<F: Family>(&self, event: impl Fn(F) -> Event) -> Vec<(&'static str, u64)> {
        F::ALL
            .iter()
            .map(|&word| (word.word(), self.get(event(word))))
            .filter(|(_, count)| *count != 0)
            .collect()
    }
}

/// Every event, each once: one for each slot of [`COUNTS`].
fn every_event() -> impl Iterator<Item = Event> {
    [
        Event::RoundOut,
        Event::RoundIn,
        Event::RoundQuiet,
        Event::OwnerTick,
        Event::Beacon,
        Event::Announce,
        Event::DeadlineSpawn,
        Event::RememberedWrite,
        Event::Expired,
        Event::SeatBusy,
        Event::Unretired,
    ]
    .into_iter()
    .chain(Next::ALL.into_iter().map(Event::Next))
    .chain(Dial::ALL.into_iter().map(Event::Dial))
    .chain(<TableTurn as Family>::ALL.iter().copied().map(Event::Turn))
    .chain(<Ask as Family>::ALL.iter().copied().map(Event::Ask))
    .chain(Loop::ALL.into_iter().map(Event::Loop))
}

/// The counts now.
#[must_use]
pub fn cadence() -> Cadence {
    Cadence::of(
        |event| COUNTS[event.slot()].load(Ordering::Relaxed),
        aterm_net::fleet_member::signature_checks(),
        run_max(&SEATS.lock().unwrap_or_else(PoisonError::into_inner)),
    )
}

/// [`cadence`] of what the calling thread itself did, so a test reads its own counts beside the
/// tests that run with it: a round counts on the thread that takes its end, a tick on the thread
/// that runs the loop, and a deadline thread on the thread that starts it.
#[cfg(test)]
pub(crate) fn cadence_here() -> Cadence {
    Cadence::of(
        |event| HERE.with(|here| here[event.slot()].get()),
        aterm_net::fleet_member::signature_checks_here(),
        SEATS_HERE.with(|seats| run_max(&seats.borrow())),
    )
}

/// The most seats [`seats`] keeps: one for each endpoint a transport holds
/// (`super::discovery`'s bound). Past it, the seat written least recently goes.
pub(crate) const MAX_SEATS: usize = super::discovery::MAX_PEERS;

/// What this process counted of one seat ([`seats`]).
#[derive(Clone, Debug, Default)]
struct Seat {
    /// Its machine, once a round's contact with it resolved through membership ([`name`]).
    machine: Option<[u8; 16]>,
    /// The ledger's write count at its last write: the seat written least recently goes first.
    written: u64,
    /// Rounds this side dialled to it that ended.
    out: u64,
    /// What each did to its endpoint's schedule.
    next: [u64; Next::ALL.len()],
    /// What chose each, and what each was besides.
    dials: [u64; Dial::ALL.len()],
    /// Rounds it dialled in that completed.
    inbound: u64,
    /// Its verified endpoints a transport dropped for their expiry.
    expired: u64,
    /// Rounds with it refused because one was already in flight: those this side dialled, and
    /// those it dialled in.
    busy: [u64; 2],
    /// Table rounds this side dialled to it since the last that took the scan's regular turn.
    run: u64,
    /// The most [`Self::run`] has been.
    run_max: u64,
}

/// The seats [`seats`] keeps, at most [`MAX_SEATS`], and how many writes it has taken.
#[derive(Debug)]
struct Ledger {
    seats: BTreeMap<Actor, Seat>,
    writes: u64,
}

impl Ledger {
    const fn new() -> Self {
        Self {
            seats: BTreeMap::new(),
            writes: 0,
        }
    }

    /// Write `actor`'s seat by `write`, making room for it if it is new.
    fn write(&mut self, actor: Actor, write: impl FnOnce(&mut Seat)) {
        if !self.seats.contains_key(&actor)
            && self.seats.len() >= MAX_SEATS
            && let Some(oldest) = self
                .seats
                .iter()
                .min_by_key(|(_, seat)| seat.written)
                .map(|(actor, _)| *actor)
        {
            self.seats.remove(&oldest);
        }
        self.writes += 1;
        let seat = self.seats.entry(actor).or_default();
        seat.written = self.writes;
        write(seat);
    }
}

static SEATS: Mutex<Ledger> = Mutex::new(Ledger::new());

#[cfg(test)]
thread_local! {
    /// The calling thread's own seats ([`seats_here`]).
    static SEATS_HERE: std::cell::RefCell<Ledger> = const { std::cell::RefCell::new(Ledger::new()) };
}

/// Write `actor`'s seat by `write`, in the process's ledger and, in tests, the calling thread's.
fn write_seat(actor: Actor, write: impl Fn(&mut Seat)) {
    SEATS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .write(actor, &write);
    #[cfg(test)]
    SEATS_HERE.with(|seats| seats.borrow_mut().write(actor, &write));
}

fn run_max(ledger: &Ledger) -> u64 {
    ledger
        .seats
        .values()
        .map(|seat| seat.run_max)
        .max()
        .unwrap_or(0)
}

/// Count what a round this side dialled came to: what it did to its endpoint's schedule
/// (`verdict`) and what its report says it did (`round`; none for a round whose worker panicked
/// or never started): the table turn its completed exchange took, why it asked, and what a
/// restart of its seat's cycle threw away, which its first list trade since reports whether the
/// exchange then completed, broke off or was stopped. Process-wide, and against its seat where
/// the round names one.
pub(super) fn own_round(verdict: &Verdict, round: Option<&RepairRound>) {
    let application = round.and_then(|round| round.application);
    let sweep = round
        .and_then(|round| round.application.or(round.application_interrupted))
        .and_then(|report| report.sweep);
    let mut dials = Vec::new();
    if let Some(marks) = verdict.marks {
        dials.push(marks.chose);
        if marks.probe {
            dials.push(Dial::Probe);
        }
        if marks.met_again {
            dials.push(Dial::MetAgain);
        }
        if marks.away {
            dials.push(Dial::Away);
        }
    }
    match sweep {
        Some(crate::replica::Sweep::Restarted) => dials.push(Dial::SweepRestarted),
        Some(crate::replica::Sweep::Discarded) => dials.push(Dial::SweepDiscarded),
        None => {}
    }
    count(Event::Next(verdict.next));
    for &dial in &dials {
        count(Event::Dial(dial));
    }
    let turn = application.and_then(|report| report.turn);
    if let Some(turn) = turn {
        count(Event::Turn(turn));
    }
    if let Some(ask) = application.and_then(|report| report.asked) {
        count(Event::Ask(ask));
    }
    let Some(actor) = verdict.seat else {
        return;
    };
    write_seat(actor, |seat| {
        seat.out += 1;
        seat.next[verdict.next as usize] += 1;
        for &dial in &dials {
            seat.dials[dial as usize] += 1;
        }
        match turn {
            Some(TableTurn::Regular) => seat.run = 0,
            Some(_) => {
                seat.run += 1;
                seat.run_max = seat.run_max.max(seat.run);
            }
            None => {}
        }
    });
}

/// Count a round `actor` dialled in that completed.
pub(super) fn inbound(actor: Actor) {
    write_seat(actor, |seat| seat.inbound += 1);
}

/// Count a verified endpoint a transport dropped for its expiry, against the seat its last round
/// authenticated, if any.
pub(super) fn expired(actor: Option<Actor>) {
    count(Event::Expired);
    if let Some(actor) = actor {
        write_seat(actor, |seat| seat.expired += 1);
    }
}

/// Count a round with `actor` refused because one with it was already in flight here: one this
/// side `dialled`, or one it dialled in.
pub(crate) fn seat_busy(actor: Actor, dialled: bool) {
    count(Event::SeatBusy);
    write_seat(actor, |seat| seat.busy[usize::from(!dialled)] += 1);
}

/// Label `actor`'s seat, if this process counted anything of it, by its `machine`: what a round's
/// contact with it resolved through membership. A label is no write.
pub(super) fn name(actor: Actor, machine: [u8; 16]) {
    let mut ledger = SEATS.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(seat) = ledger.seats.get_mut(&actor) {
        seat.machine = Some(machine);
    }
    drop(ledger);
    #[cfg(test)]
    SEATS_HERE.with(|seats| {
        if let Some(seat) = seats.borrow_mut().seats.get_mut(&actor) {
            seat.machine = Some(machine);
        }
    });
}

/// One seat's counts ([`seats`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeatCounts {
    /// Its machine's first 6 bytes in lower hex once a contact with it resolved, else `actor-`
    /// and its actor's first 6 bytes; a machine's seats beyond one add `-actor-` and theirs.
    pub label: String,
    /// Every count, each seat's in the same order: `out` (rounds this side dialled to it), each
    /// of what those did to its schedule (`rest` … `unasked`), each of what chose them and what
    /// they were besides (`follow-up` … `sweep-discarded`), then `inbound`, `expired`,
    /// `busy-dialled`, `busy-served`, `run` and `run_max`.
    pub cells: Vec<(&'static str, u64)>,
}

impl SeatCounts {
    fn of(actor: &Actor, seat: &Seat) -> Self {
        let label = seat.machine.map_or_else(
            || format!("actor-{}", hex(&actor[..6])),
            |machine| hex(&machine[..6]),
        );
        let mut cells = vec![("out", seat.out)];
        cells.extend(
            Next::ALL
                .iter()
                .map(|next| (next.word(), seat.next[*next as usize])),
        );
        cells.extend(
            Dial::ALL
                .iter()
                .map(|dial| (dial.word(), seat.dials[*dial as usize])),
        );
        cells.extend([
            ("inbound", seat.inbound),
            ("expired", seat.expired),
            ("busy-dialled", seat.busy[0]),
            ("busy-served", seat.busy[1]),
            ("run", seat.run),
            ("run_max", seat.run_max),
        ]);
        Self { label, cells }
    }
}

/// Each seat `ledger` keeps, in its actors' order. Two seats of one machine (a Mac enrolled
/// again) each add their actor's bytes to the label, so no two seats share one.
fn seats_of(ledger: &Ledger) -> Vec<SeatCounts> {
    let mut seats: Vec<SeatCounts> = ledger
        .seats
        .iter()
        .map(|(actor, seat)| SeatCounts::of(actor, seat))
        .collect();
    let labels: Vec<String> = seats.iter().map(|seat| seat.label.clone()).collect();
    for ((actor, _), seat) in ledger.seats.iter().zip(&mut seats) {
        if labels.iter().filter(|label| **label == seat.label).count() > 1 {
            seat.label = format!("{}-actor-{}", seat.label, hex(&actor[..6]));
        }
    }
    seats
}

/// `bytes` in lower hex.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// What this process counted of each seat it keeps, at most [`MAX_SEATS`], in its actors' order.
#[must_use]
pub fn seats() -> Vec<SeatCounts> {
    seats_of(&SEATS.lock().unwrap_or_else(PoisonError::into_inner))
}

/// [`seats`] as the calling thread wrote them (tests).
#[cfg(test)]
pub(crate) fn seats_here() -> Vec<SeatCounts> {
    SEATS_HERE.with(|seats| seats_of(&seats.borrow()))
}

/// `actor`'s seat as the calling thread wrote it, by its cells' words (tests).
#[cfg(test)]
pub(crate) fn seat_here(actor: Actor) -> Option<BTreeMap<&'static str, u64>> {
    SEATS_HERE.with(|seats| {
        seats
            .borrow()
            .seats
            .get(&actor)
            .map(|seat| SeatCounts::of(&actor, seat).cells.into_iter().collect())
    })
}

#[cfg(test)]
#[path = "census_tests.rs"]
mod tests;
