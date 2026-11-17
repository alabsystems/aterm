// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HOST'S FACTS, BELLED (the footer's cause design, step C8): the facts a footer read takes
//! of this window and this machine rather than of its session's files — the window's switches, a
//! tab's published snapshot, the lab's journal, the wall clock and the local time zone. Each rings
//! a [`Bell`] where it is stored or announced, and the footer resolver re-reads every session it
//! watches at the bell (`claude_footer`'s `Command::HostMoved`), so no clock polls them and a
//! read that took one has a cause.
//!
//! In a test build, `MEMBERS` names every host accessor a footer read takes and the bell that
//! answers for it; a test counts, on its own thread, each accessor's reads (`noted`) and holds
//! every read a derive took to a member (law L10 HostFactsBelled,
//! `claude_footer_cause_tests::every_host_fact_a_derive_reads_is_belled`).

/// What moved on the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Bell {
    /// A window switch a read takes moved (`harness_host::usage_tap_on`, `accounts_mode`,
    /// `usage_credits`): a config reload stored another value.
    Config,
    /// A tab's published relaunch snapshot moved (`harness_host::publish_snapshot_from`), which
    /// the restart offer and the tap's room are judged by.
    Snapshot,
    /// The lab's journal this process holds came to say something else
    /// (`aterm_agent::harness::alab_seen::hold`).
    Journal,
    /// The wall clock was set, or the machine woke (`aterm_uds::wall_watch`): a deadline a read
    /// posted on the monotonic clock stands at another wall instant.
    Wall,
    /// The local time zone was set: an offset a read took is another.
    Zone,
}

/// Every host accessor a footer read takes, by the name its read notes ([`noted`]), with the
/// bell that rings when what it answers moves; `None` for one fixed for the process's life (the
/// harness state's folder, of the environment it started in).
#[cfg(test)]
pub(crate) const MEMBERS: [(&str, Option<Bell>); 9] = [
    ("usage_tap_on", Some(Bell::Config)),
    ("accounts_mode", Some(Bell::Config)),
    ("usage_credits", Some(Bell::Config)),
    ("published_snapshot", Some(Bell::Snapshot)),
    ("offered_for", Some(Bell::Snapshot)),
    ("tap_room_for", Some(Bell::Snapshot)),
    ("harness_state", None),
    ("journal", Some(Bell::Journal)),
    ("local_offset", Some(Bell::Zone)),
];

/// Ring `bell`: the footer resolver, where one runs, re-reads every session it watches; with
/// none running nothing is watched and nothing is told. A test's ring is logged on its thread
/// instead (`take_rung`).
pub(crate) fn ring(bell: Bell) {
    #[cfg(test)]
    RUNG.with(|rung| rung.borrow_mut().push(bell));
    #[cfg(not(test))]
    crate::claude_footer::host_moved(bell);
}

/// Have the lab's journal ring [`Bell::Journal`] when what this process holds of it comes to say
/// something else (`aterm_agent::harness::alab_seen::hold`); once per process.
pub(crate) fn listen() {
    let _ = aterm_agent::harness::alab_seen::listen(journal_moved);
}

/// [`listen`]'s listener.
fn journal_moved() {
    ring(Bell::Journal);
}

#[cfg(test)]
thread_local! {
    /// Each host accessor's reads on this thread, by its name ([`noted`]).
    static NOTED: std::cell::RefCell<std::collections::BTreeMap<&'static str, usize>> =
        const { std::cell::RefCell::new(std::collections::BTreeMap::new()) };
    /// The bells rung on this thread ([`ring`]).
    static RUNG: std::cell::RefCell<Vec<Bell>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Count one read of host accessor `accessor` on this thread.
#[cfg(test)]
pub(crate) fn noted(accessor: &'static str) {
    NOTED.with(|noted| *noted.borrow_mut().entry(accessor).or_default() += 1);
}

/// The host accessors read on this thread since the last take, with their counts.
#[cfg(test)]
pub(crate) fn take_noted() -> std::collections::BTreeMap<&'static str, usize> {
    NOTED.with(|noted| std::mem::take(&mut *noted.borrow_mut()))
}

/// Of `noted`, the accessors no member names: reads no bell answers for.
#[cfg(test)]
pub(crate) fn unbelled(
    noted: &std::collections::BTreeMap<&'static str, usize>,
) -> Vec<&'static str> {
    noted
        .keys()
        .copied()
        .filter(|name| MEMBERS.iter().all(|(member, _)| member != name))
        .collect()
}

/// The bells rung on this thread since the last take, in order.
#[cfg(test)]
pub(crate) fn take_rung() -> Vec<Bell> {
    RUNG.with(|rung| std::mem::take(&mut *rung.borrow_mut()))
}
