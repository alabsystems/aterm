// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_uds::{
    process::{Birth, Identity},
    spawnfd,
};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, VecDeque},
};

#[derive(Default)]
struct Fake {
    members: BTreeMap<u32, GroupMembers>,
    lineage: RefCell<BTreeMap<i32, VecDeque<Option<Lineage>>>>,
    bytes: BTreeMap<u32, Option<u64>>,
    presence: BTreeMap<i32, ProcessPresence>,
    group_presence: Option<ProcessPresence>,
    reads: Cell<usize>,
    memory_error: Option<io::ErrorKind>,
}

fn lineage(pid: i32, group: u32) -> Lineage {
    Lineage {
        identity: Identity {
            pid,
            birth: Birth(17),
            idversion: None,
        },
        uniqueid: 17,
        parent: 2,
        pgid: group,
        launcher_version: None,
        tty: None,
    }
}

impl Fake {
    fn member(&mut self, pid: i32, group: u32, bytes: u64) {
        self.members
            .entry(group)
            .or_insert_with(|| GroupMembers {
                pids: Vec::new(),
                complete: true,
            })
            .pids
            .push(pid);
        self.lineage.get_mut().insert(
            pid,
            VecDeque::from([Some(lineage(pid, group)), Some(lineage(pid, group))]),
        );
        self.bytes.insert(pid as u32, Some(bytes));
    }
}

impl Probe for Fake {
    fn members(&self, group: u32) -> Option<GroupMembers> {
        self.members.get(&group).cloned()
    }
    fn group_presence(&self, _: u32) -> ProcessPresence {
        self.group_presence.unwrap_or(ProcessPresence::Unknown)
    }
    fn lineage(&self, pid: i32) -> Option<Lineage> {
        self.lineage.borrow_mut().get_mut(&pid)?.pop_front()?
    }
    fn memory(&self, pid: u32) -> io::Result<u64> {
        self.reads.set(self.reads.get() + 1);
        self.bytes.get(&pid).copied().flatten().ok_or_else(|| {
            io::Error::new(
                self.memory_error.unwrap_or(io::ErrorKind::Other),
                "injected missing memory",
            )
        })
    }
    fn presence(&self, pid: i32) -> ProcessPresence {
        self.presence
            .get(&pid)
            .copied()
            .unwrap_or(ProcessPresence::Unknown)
    }
}

#[test]
fn owned_memory_deduplicates_all_groups_and_checks_sum_overflow() {
    let mut fake = Fake::default();
    fake.member(11, 10, 7);
    fake.member(21, 20, 9);
    fake.members.get_mut(&20).unwrap().pids.push(11);
    assert_eq!(sample_with(&[10, 20, 10], &fake).unwrap(), 16);
    assert_eq!(fake.reads.get(), 2);
    let mut fake = Fake::default();
    fake.member(11, 10, u64::MAX);
    fake.member(12, 10, 1);
    assert!(sample_with(&[10], &fake).is_err());
}

#[test]
fn owned_memory_refuses_unknown_incomplete_and_empty_live_census() {
    for groups in [vec![0], vec![1], vec![-10]] {
        assert!(sample_with(&groups, &Fake::default()).is_err());
    }
    assert_eq!(sample_with(&[], &Fake::default()).unwrap(), 0);
    assert!(sample_with(&[10], &Fake::default()).is_err());
    let mut fake = Fake::default();
    fake.member(11, 10, 7);
    assert!(sample_with(&[10, 20], &fake).is_err());
    assert_eq!(
        fake.reads.get(),
        0,
        "no partial census becomes a memory total"
    );
    for complete in [false, true] {
        let mut fake = Fake::default();
        fake.members.insert(
            10,
            GroupMembers {
                pids: Vec::new(),
                complete,
            },
        );
        fake.group_presence = Some(ProcessPresence::Absent);
        assert_eq!(sample_with(&[10], &fake).is_ok(), complete);
        fake.group_presence = Some(ProcessPresence::Present);
        assert!(sample_with(&[10], &fake).is_err());
    }
}

#[test]
fn owned_memory_omits_only_positive_exit_and_never_missing_memory() {
    for presence in [
        ProcessPresence::Unknown,
        ProcessPresence::Present,
        ProcessPresence::Exited,
        ProcessPresence::Absent,
    ] {
        for missing_read in 0..3 {
            let mut fake = Fake::default();
            fake.member(11, 10, 7);
            fake.presence.insert(11, presence);
            match missing_read {
                0 => fake.lineage.get_mut().clear(),
                1 => {
                    fake.bytes.insert(11, None);
                }
                _ => {
                    fake.lineage.get_mut().get_mut(&11).unwrap()[1] = None;
                }
            }
            let result = sample_with(&[10], &fake);
            if matches!(presence, ProcessPresence::Exited | ProcessPresence::Absent) {
                assert_eq!(result.unwrap(), 0);
            } else {
                assert!(result.is_err());
            }
        }
    }
}

#[test]
fn owned_memory_refuses_identity_reuse_exec_or_group_change() {
    for changed in 0..4 {
        let mut fake = Fake::default();
        fake.member(11, 10, 7);
        let mut after = lineage(11, 10);
        match changed {
            0 => after.identity.birth = Birth(18),
            1 => after.identity.idversion = Some(2),
            2 => after.uniqueid = 18,
            _ => after.pgid = 20,
        }
        fake.lineage.get_mut().get_mut(&11).unwrap()[1] = Some(after);
        assert!(sample_with(&[10], &fake).is_err());
    }
    let mut fake = Fake::default();
    fake.member(11, 10, 7);
    fake.lineage.get_mut().get_mut(&11).unwrap()[0] = Some(lineage(11, 20));
    assert!(sample_with(&[10], &fake).is_err());
}

#[test]
fn owned_memory_never_masks_counter_corruption_or_overflow_as_exit() {
    for presence in [ProcessPresence::Exited, ProcessPresence::Absent] {
        let mut fake = Fake::default();
        fake.member(11, 10, 7);
        fake.bytes.insert(11, None);
        fake.memory_error = Some(io::ErrorKind::InvalidData);
        fake.presence.insert(11, presence);
        assert_eq!(
            sample_with(&[10], &fake).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}

struct Child(spawnfd::Child, Option<std::process::ChildStdin>);
impl Drop for Child {
    fn drop(&mut self) {
        self.1.take();
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn owned_memory_samples_one_actual_small_native_group() {
    let attempt = spawnfd::BirthAttempt::new().unwrap();
    let mut born = spawnfd::command("/bin/cat")
        .stdin(spawnfd::Stdio::piped())
        .stdout(spawnfd::Stdio::piped())
        .spawn_cohort(attempt)
        .unwrap();
    let input = aterm_uds::deadline::cat_echoes(born.child_mut());
    let mut child = Child(born.into_child(), Some(input));
    let group = i32::try_from(child.0.id()).unwrap();
    assert_eq!(process::read_lineage(group).unwrap().pgid, group as u32);
    assert!(sample(&[group, group]).unwrap() > 0);
    child.1.take();
    assert!(child.0.wait().unwrap().success());
}

#[test]
#[cfg(target_vendor = "apple")]
fn anchor_census_cannot_trust_a_later_dead_parent_or_empty_error() {
    let leader = 71;
    let parent = 72;
    let unseen_child = 73;
    let mut probe = Fake {
        group_presence: Some(ProcessPresence::Unknown),
        ..Fake::default()
    };
    // Snapshot P then P forks C and exits before a later status read. The
    // stale snapshot plus P's exit must never authorize reaping the anchor.
    probe.members.insert(
        leader,
        GroupMembers {
            pids: vec![leader as i32, parent],
            complete: true,
        },
    );
    probe.presence.insert(parent, ProcessPresence::Exited);
    probe
        .presence
        .insert(unseen_child, ProcessPresence::Present);
    assert!(!other_members_exited_with(leader, &probe));
    probe.members.get_mut(&leader).unwrap().pids.clear();
    assert!(!other_members_exited_with(leader, &probe));
    let members = probe.members.get_mut(&leader).unwrap();
    members.pids.push(leader as i32);
    members.complete = false;
    assert!(!other_members_exited_with(leader, &probe));
    probe.members.get_mut(&leader).unwrap().complete = true;
    assert!(other_members_exited_with(leader, &probe));
}
