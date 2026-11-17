// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A PERSON'S CLAIM ([`Need::person`]): a command a person typed at a tab's prompt goes ahead of
//! every waiting claim of its rank that is not one, and of nothing else.

use super::*;
use crate::reading::person_at;
use crate::{Fixed, Rank};

const HOUR: u64 = 3_600_000;

/// The machine as a command at a tab reads it: `fixed`'s room, a person or not
/// ([`Machine::person`]), under a release or not ([`Machine::release_run`]).
struct Tab<'a> {
    fixed: &'a Fixed,
    person: bool,
    release: bool,
}

impl<'a> Tab<'a> {
    /// A command a person typed at a tab's prompt.
    const fn person(fixed: &'a Fixed) -> Tab<'a> {
        Tab {
            fixed,
            person: true,
            release: false,
        }
    }

    /// An agent's call: no person.
    const fn agent(fixed: &'a Fixed) -> Tab<'a> {
        Tab {
            fixed,
            person: false,
            release: false,
        }
    }
}

impl Machine for Tab<'_> {
    fn release_run(&self) -> bool {
        self.release
    }
    fn person(&self) -> bool {
        self.person
    }
    fn read(&self, path: Option<&Path>) -> Reading {
        self.fixed.read(path)
    }
    fn procs(&self) -> Option<Procs> {
        self.fixed.procs()
    }
    fn waits(&self) -> Vec<aterm_toolchain::waits::Waiting> {
        self.fixed.waits()
    }
}

/// A scratch ledger.
fn scratch() -> (aterm_tempfile::TempDir, Ledger) {
    let dir = aterm_tempfile::Builder::new()
        .prefix("cap-person-")
        .tempdir()
        .expect("a scratch ledger");
    let ledger = Ledger::at(dir.path().join("ledger"));
    (dir, ledger)
}

/// Lay `need` as `pid` (another process than this test's, so no claim is this process's own
/// sibling) at `at_ms`, as `machine` reads the asker.
#[track_caller]
fn enter(ledger: &Ledger, machine: &dyn Machine, need: Need, pid: u32, at_ms: u64) -> Waiter {
    Waiter::enter_stand_in(ledger, machine, need, Some(pid), at_ms).expect("entered")
}

/// Poll once and expect admission.
#[track_caller]
fn admitted(w: Waiter, m: &dyn Machine) -> Claim {
    match w.poll(m) {
        Polled::Admitted(c) => c,
        Polled::Waiting(_, short) => panic!("expected admission, waits: {short}"),
        Polled::Gone(e) => panic!("expected admission, gone: {e}"),
        Polled::Unqualified(why) => panic!("expected admission, unqualified: {why}"),
    }
}

/// Poll once and expect a wait: the waiter and what it is short of.
#[track_caller]
fn waits(w: Waiter, m: &dyn Machine) -> (Waiter, Shortfall) {
    match w.poll(m) {
        Polled::Waiting(w, short) => (w, short),
        Polled::Admitted(c) => panic!("expected a wait, admitted: {:?}", c.record()),
        Polled::Gone(e) => panic!("expected a wait, gone: {e}"),
        Polled::Unqualified(why) => panic!("expected a wait, unqualified: {why}"),
    }
}

/// Whether `short` names the claim `order` among the claims it waits behind.
fn names(short: &Shortfall, order: (u64, u32, u64)) -> bool {
    short.holders.iter().any(|h| h.order == order)
}

/// A PERSON'S CLAIM GOES FIRST AMONG ITS RANK, on a 36 GiB machine with 10 GiB available and
/// 12 cores, the stamps chosen and no clock waited on.
/// - Memory: two agents' 8 GiB claims wait, laid before a person's 8 GiB claim. The person's
///   claim is admitted at its first look, where each agent claim would leave it 2 GiB; with it
///   held the later agent claim waits, behind it and behind the earlier agent claim.
/// - Cores: an agent's claim of 12 cores alone waits, laid before a person's claim of 12. The
///   person's claim is granted all 12, neither reserved for the agent claim nor shared with it.
///
/// NEGATIVE CONTROLS. Two unmarked claims keep arrival order: once the person's claim ends the
/// later agent claim still waits behind the earlier one, which is then admitted. A caller that
/// sets `person` on an inert machine enters unmarked, and its claims, memory and cores, wait
/// behind the agent claims laid before them.
#[test]
fn a_persons_claim_at_a_tab_is_admitted_before_agent_claims_that_asked_first() {
    let fixed = Fixed::new(Reading::of_gib(36, 10, 12));
    let (person, agent) = (Tab::person(&fixed), Tab::agent(&fixed));
    let me = std::process::id();
    let t0 = now_ms() - HOUR;
    let gib8 = |what: &str| Need::new(what).mem_gib(8);

    // MEMORY: the person's claim is marked as it enters, and goes ahead of the earlier agents'.
    let (_dir, ledger) = scratch();
    let first = enter(&ledger, &agent, gib8("agent build 1"), me + 1, t0);
    let second = enter(&ledger, &agent, gib8("agent build 2"), me + 2, t0 + 1);
    let typed = enter(&ledger, &person, gib8("targo build"), me + 3, t0 + 2);
    assert!(typed.record().need.person, "{:?}", typed.record());
    assert!(!first.record().need.person && !second.record().need.person);
    let first_at = first.record().order();
    let held = admitted(typed, &person);
    // The later agent claim waits behind the person's held claim and the earlier agent claim.
    let (second, short) = waits(second, &agent);
    assert!(names(&short, held.record().order()), "{short}");
    assert!(names(&short, first_at), "{short}");
    // CONTROL: two unmarked claims keep arrival order once the person's claim ends.
    drop(held);
    let (second, short) = waits(second, &agent);
    assert!(names(&short, first_at), "{short}");
    let first = admitted(first, &agent);
    drop((first, second));

    // CONTROL: a caller's mark on an inert machine is none; the claim waits behind the earlier.
    let (_dir, ledger) = scratch();
    let first = enter(&ledger, &agent, gib8("agent build 1"), me + 1, t0);
    let mut asked = gib8("claims to be a person");
    asked.person = true;
    let claimed = enter(&ledger, &agent, asked, me + 3, t0 + 2);
    assert!(!claimed.record().need.person, "{:?}", claimed.record());
    assert!(
        ledger
            .list()
            .expect("listed")
            .iter()
            .all(|r| !r.need.person),
        "no record reads a person"
    );
    let (claimed, short) = waits(claimed, &agent);
    assert!(names(&short, first.record().order()), "{short}");
    drop((first, claimed));

    // CORES: the agent's earlier claim of cores alone reserves none against the person's.
    let cores12 = |what: &str| Need::new(what).cores(12);
    let (_dir, ledger) = scratch();
    let first = enter(&ledger, &agent, cores12("agent tests"), me + 1, t0);
    let typed = enter(&ledger, &person, cores12("targo test"), me + 3, t0 + 1);
    let held = admitted(typed, &person);
    assert_eq!(
        held.cores(),
        12,
        "neither reserved for nor shared with the agent claim"
    );
    drop((first, held));

    // CONTROL: unmarked, the same claim waits behind the agent's reservation.
    let (_dir, ledger) = scratch();
    let first = enter(&ledger, &agent, cores12("agent tests"), me + 1, t0);
    let mut asked = cores12("claims to be a person");
    asked.person = true;
    let claimed = enter(&ledger, &agent, asked, me + 3, t0 + 1);
    let (_claimed, short) = waits(claimed, &agent);
    assert!(
        short
            .shorts
            .iter()
            .any(|s| matches!(s, Short::Cores { .. })),
        "{short}"
    );
    drop(first);
}

/// RELEASE, LEADS AND BACKGROUND KEEP THEIR ORDER BESIDE A PERSON'S CLAIM, on the same machine.
/// - A waiting Release claim and a waiting leading claim, each laid after a person's claim, are
///   still ahead of it: the person's claim waits behind each, and `--admit now` overrides
///   neither's memory turn.
/// - A Release claim is never marked: neither one asked at Release rank where the machine reads
///   a person, nor one a release's descendant asks there. Nor is a leading claim made there,
///   whose order among the leads is theirs alone (`sure_leads`).
/// - A Background claim is never marked, and stays behind an Ordinary claim laid after it that
///   no person asked, whoever asked for it.
#[test]
fn release_leads_and_background_order_is_unchanged_beside_a_persons_claim() {
    let fixed = Fixed::new(Reading::of_gib(36, 10, 12));
    let (person, agent) = (Tab::person(&fixed), Tab::agent(&fixed));
    let me = std::process::id();
    let t0 = now_ms() - HOUR;
    let gib8 = |what: &str| Need::new(what).mem_gib(8);

    for (kind, ahead) in [
        ("release", gib8("release slice").rank(Rank::Release)),
        ("leading", gib8("release compiler").leads()),
    ] {
        let (_dir, ledger) = scratch();
        let typed = enter(&ledger, &person, gib8("targo build"), me + 1, t0);
        assert!(typed.record().need.person);
        let ahead = enter(&ledger, &agent, ahead, me + 2, t0 + 1);
        let (typed, short) = waits(typed, &person);
        assert!(names(&short, ahead.record().order()), "{kind}: {short}");
        match typed.force(&person) {
            Ok(Forced::Turn(_, short)) => {
                assert!(names(&short, ahead.record().order()), "{kind}: {short}");
            }
            Ok(Forced::Admitted(claim, _)) => {
                panic!("{kind}: forced past its turn: {:?}", claim.record())
            }
            Err(e) => panic!("{kind}: {e}"),
        }
        drop(ahead);
    }

    // A Release claim where the machine reads a person carries no mark.
    let (_dir, ledger) = scratch();
    let asked = enter(
        &ledger,
        &person,
        gib8("release stage").rank(Rank::Release),
        me + 1,
        t0,
    );
    assert_eq!(asked.record().need.rank, Rank::Release);
    assert!(!asked.record().need.person, "{:?}", asked.record());
    let under = Tab {
        release: true,
        ..Tab::person(&fixed)
    };
    let descendant = enter(
        &ledger,
        &under,
        gib8("release's descendant"),
        me + 2,
        t0 + 1,
    );
    assert_eq!(descendant.record().need.rank, Rank::Release);
    assert!(
        !descendant.record().need.person,
        "{:?}",
        descendant.record()
    );
    drop((asked, descendant));

    // A leading claim where the machine reads a person carries no mark.
    let (_dir, ledger) = scratch();
    let leading = enter(&ledger, &person, gib8("leading build").leads(), me + 1, t0);
    assert!(leading.record().need.leads, "{:?}", leading.record());
    assert!(!leading.record().need.person, "{:?}", leading.record());
    drop(leading);

    // A person's Background claim: unmarked, and behind a later unmarked Ordinary claim.
    let (_dir, ledger) = scratch();
    let mut background = gib8("background sampling").rank(Rank::Background);
    background.person = true;
    let (laid, _kept) = ledger.lay_stand_in(&background, t0, me + 1).expect("laid");
    assert_eq!(laid.need.rank, Rank::Background);
    assert!(!laid.need.person, "{laid:?}");
    let ordinary = enter(&ledger, &agent, gib8("agent build"), me + 2, t0 + 1);
    let held = admitted(ordinary, &agent);
    assert_eq!(held.record().need.rank, Rank::Ordinary);
}

/// A PLACE KEEPS ITS MARK ([`Place::standing`]): a claim entered at a place this process
/// stands at takes the place's mark with its stamp, as work that stands in the queue without a
/// break keeps the mark it entered with until its last claim ends.
/// - Entered at a person's place where the machine no longer reads a person (the run was
///   stopped and sent to the background), it is a person's.
/// - Entered at an agent's place where the machine reads a person, it is not.
///
/// NEGATIVE CONTROL: once no record stands at the place, an entry at it is an entry at now and
/// takes the machine's reading.
#[test]
fn a_claim_entered_at_a_place_keeps_the_places_mark() {
    let fixed = Fixed::new(Reading::of_gib(36, 10, 12));
    let (person, agent) = (Tab::person(&fixed), Tab::agent(&fixed));
    let gib1 = |what: &str| Need::new(what).mem_gib(1);
    let (_dir, ledger) = scratch();

    let first = Waiter::enter(&ledger, &person, gib1("test binary 1")).expect("entered");
    let place = first.place();
    assert_eq!(place.standing().map(|(_, marked)| marked), Some(true));
    let next = Waiter::enter_at(&ledger, &agent, gib1("test binary 2"), &place).expect("entered");
    assert!(next.record().need.person, "{:?}", next.record());
    assert_eq!(next.record().start_ms, first.record().start_ms);
    drop((first, next));
    assert_eq!(place.standing(), None);
    let after = Waiter::enter_at(&ledger, &agent, gib1("test binary 3"), &place).expect("entered");
    assert!(!after.record().need.person, "{:?}", after.record());
    drop(after);

    let first = Waiter::enter(&ledger, &agent, gib1("agent binary 1")).expect("entered");
    let place = first.place();
    assert_eq!(place.standing().map(|(_, marked)| marked), Some(false));
    let next = Waiter::enter_at(&ledger, &person, gib1("agent binary 2"), &place).expect("entered");
    assert!(!next.record().need.person, "{:?}", next.record());
    drop((first, next));
}

/// THE TERMINAL RULE ([`person_at`]): a person asks where the asker's group is its controlling
/// terminal's foreground group and no agent marker is set.
///
/// NEGATIVE CONTROLS. No person for a group that is not the terminal's foreground group (a job
/// sent to the background, a landing's own group), for no terminal (a detached child, a tool
/// shell with no `/dev/tty`), where `CLAUDECODE` or `CODEX_THREAD_ID` is set (an agent's tool
/// call), nor where `ATERM_TASK_FILE` names a task file (a tab `aterm ctl spawn` started for a
/// worker). An empty variable is none, as `agent_marker` reads a marker.
#[test]
fn the_terminal_rule_reads_a_person_only_in_the_foreground_group_with_no_agent_marker() {
    let unset = |_: &str| -> Option<String> { None };
    assert!(
        person_at(Some(4_100), 4_100, &unset),
        "a command at the prompt"
    );
    assert!(!person_at(Some(4_101), 4_100, &unset), "not the foreground");
    assert!(!person_at(None, 4_100, &unset), "no terminal");
    for marker in [
        "CLAUDECODE",
        "CODEX_THREAD_ID",
        aterm_types::domain::ENV_TASK_FILE,
    ] {
        let set = |k: &str| (k == marker).then(|| "1".to_owned());
        assert!(!person_at(Some(4_100), 4_100, &set), "{marker} set");
        let empty = |k: &str| (k == marker).then(String::new);
        assert!(person_at(Some(4_100), 4_100, &empty), "{marker} empty");
    }
    // An inert machine reads no person, whatever the terminal it runs from.
    assert!(!Fixed::new(Reading::of_gib(36, 10, 12)).person());
}

/// What [`standin_leads_a_terminal_and_starts_readers`] runs on.
#[cfg(unix)]
const LEAD_STANDIN: &str = "ATERM_CAPACITY_PERSON_LEAD_STANDIN";
/// What [`standin_reads_live_person`] runs on.
#[cfg(unix)]
const READ_STANDIN: &str = "ATERM_CAPACITY_PERSON_READ_STANDIN";

/// A stand-in reader of [`the_live_terminal_rule_reads_the_foreground_group_of_dev_tty`]: say
/// what the live machine reads ([`crate::Live`]'s [`Machine::person`]). Run alone, it does
/// nothing.
#[cfg(unix)]
#[test]
fn standin_reads_live_person() {
    if std::env::var_os(READ_STANDIN).is_none() {
        return;
    }
    println!("PERSON={}", crate::Live.person());
}

/// The stand-in that leads [`the_live_terminal_rule_reads_the_foreground_group_of_dev_tty`]'s
/// terminal: in a session of its own, take its stdin (an isolated terminal) as its controlling
/// terminal, whose foreground group is then its own, and start readers
/// ([`standin_reads_live_person`]) there, each with no terminal on its stdin: in its group with
/// no marker, with `CLAUDECODE` set, naming a task file, and in a group of its own. Run alone,
/// it does nothing.
#[cfg(unix)]
#[cfg_attr(
    target_os = "linux",
    expect(
        clippy::useless_conversion,
        reason = "TIOCSCTTY is ioctl's own request type on Linux and a c_uint on macOS"
    )
)]
#[test]
fn standin_leads_a_terminal_and_starts_readers() {
    if std::env::var_os(LEAD_STANDIN).is_none() {
        return;
    }
    // SAFETY: `ioctl` with `TIOCSCTTY` and an integer argument on this process's stdin; it
    // leads its own session (the test spawned it with `setsid`), so stdin becomes its terminal.
    assert_eq!(
        unsafe { libc::ioctl(0, libc::TIOCSCTTY.into(), 0) },
        0,
        "a terminal"
    );
    let read = |case: &str, set: Option<(&str, &str)>, own_group: bool| {
        let mut reader = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"));
        reader
            .args([
                "--exact",
                "admit::person_tests::standin_reads_live_person",
                "--test-threads=1",
                "--nocapture",
            ])
            .env_remove(LEAD_STANDIN)
            .env(READ_STANDIN, "1");
        if let Some((k, v)) = set {
            reader.env(k, v);
        }
        let out = if own_group {
            // In a group of its own, which the hang detector ends with it.
            aterm_uds::deadline::output_while_process_progressing(
                &mut reader,
                aterm_spec::wait::MUST_HAPPEN,
                format!("the {case} reader"),
            )
            .expect("the reader")
        } else {
            // In this stand-in's group, which the test's hang detector ends with it.
            reader
                .stdin(aterm_uds::spawnfd::Stdio::null())
                .output()
                .expect("the reader")
        };
        let said = String::from_utf8_lossy(&out.stdout);
        let person = said
            .split("PERSON=")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .unwrap_or("unread");
        println!("READ {case}={person}");
    };
    read("foreground", None, false);
    read("agent", Some(("CLAUDECODE", "1")), false);
    read(
        "worker",
        Some((aterm_types::domain::ENV_TASK_FILE, "/nonexistent/task")),
        false,
    );
    read("background", None, true);
}

/// THE LIVE TERMINAL RULE ([`crate::Live`]'s [`Machine::person`]) on an isolated terminal, never
/// the person's: a stand-in leads a session whose controlling terminal is a new pty, and each
/// reader it starts is a process of its group with another pid and no terminal on its stdin. A
/// reader in the terminal's foreground group with no marker reads a person, so the rule reads the
/// asker's group (not its pid) against the foreground group of `/dev/tty` (not of its stdin).
///
/// NEGATIVE CONTROLS. A reader with `CLAUDECODE` set, one naming a task file, and one in a group
/// of its own (a job in the background) read none.
#[cfg(unix)]
#[test]
fn the_live_terminal_rule_reads_the_foreground_group_of_dev_tty() {
    use std::os::fd::{FromRawFd as _, OwnedFd};
    let (master, slave) = aterm_pty::open_pty_pair().expect("a terminal");
    // SAFETY: both descriptors were just opened and are owned by nothing else.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    let mut lead = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"));
    lead.args([
        "--exact",
        "admit::person_tests::standin_leads_a_terminal_and_starts_readers",
        "--test-threads=1",
        "--nocapture",
    ])
    .env(LEAD_STANDIN, "1")
    .env_remove(READ_STANDIN)
    .env_remove(aterm_types::domain::ENV_TASK_FILE)
    .setsid(true);
    for marker in aterm_types::domain::AGENT_MARKERS {
        lead.env_remove(marker);
    }
    let out = aterm_uds::deadline::output_from_while_process_progressing(
        &mut lead,
        aterm_uds::spawnfd::Stdio::from(slave),
        aterm_spec::wait::MUST_HAPPEN,
        "the terminal's stand-in",
    )
    .expect("the stand-in");
    drop(master);
    let said = String::from_utf8_lossy(&out.stdout);
    for line in [
        "READ foreground=true",
        "READ agent=false",
        "READ worker=false",
        "READ background=false",
    ] {
        assert!(said.contains(line), "{line}: {said}");
    }
}
