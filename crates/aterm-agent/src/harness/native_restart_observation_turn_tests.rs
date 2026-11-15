// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn process(pid: u32, parent: u32) -> Process {
    Process {
        pid,
        birth: u64::from(pid) * 10,
        parent,
        group: pid,
        session: 22,
        image: Image {
            path: b"/private/vendor".to_vec(),
            device: 7,
            inode: 8,
        },
    }
}

fn binding(id: char) -> ObservationBinding {
    ObservationBinding {
        id: id.to_string().repeat(32),
        host_epoch: "2".repeat(32),
        capability: "3".repeat(64),
    }
}

/// A record of registration `id`, acknowledged by the host or not yet.
fn observed(id: char, acknowledged: bool) -> Observed {
    Observed {
        registration: Registration {
            id: id.to_string().repeat(32),
            host_epoch: "2".repeat(32),
            vendor: process(33, 22),
            shell: process(22, 1),
        },
        binding: acknowledged.then(|| binding(id)),
    }
}

/// What the 2026-11-13 landing ran into, an observer at a time: the late snapshot read had read
/// no record and was flushing its registration when the record follow observed the same tab,
/// registered, was acknowledged and saved; the first one's rename then landed its unacknowledged
/// registration over that binding, after the restart had ended the vendor, and the relaunch
/// waited on a crash observation for good. The second observer is now turned away while the
/// first has the record, so the first one's saves are the record's, in their own order, and
/// the observer after it reads the binding the host acknowledged. NEGATIVE CONTROL: another
/// tab's record is observed meanwhile, and the record is free again once its observer ends.
#[test]
fn a_record_has_one_observer_at_a_time_so_no_earlier_state_lands_over_an_acknowledged_binding() {
    let dir = aterm_tempfile::tempdir().expect("a scratch directory");
    let state = dir.path().join("state");

    let late = Turn::take(&state, "s-1").expect("the record has no observer");
    assert!(read(&state, "s-1").is_none(), "it read no record");
    assert!(
        Turn::take(&state, "s-1").is_none(),
        "a second observer of the tab is turned away while the first has not saved"
    );
    let other = Turn::take(&state, "s-2").expect("another tab's record is its own");
    save(&other, &observed('4', true)).expect("the other tab's record");
    drop(other);

    save(&late, &observed('1', false)).expect("its registration");
    assert!(
        read(&state, "s-1").expect("the record").binding.is_none(),
        "registered, not yet acknowledged"
    );
    assert!(Turn::take(&state, "s-1").is_none(), "still its turn");
    save(&late, &observed('1', true)).expect("its acknowledged binding");
    drop(late);

    let next = Turn::take(&state, "s-1").expect("the record is free once its observer ended");
    let seen = read(&state, "s-1").expect("the record");
    assert!(
        seen.binding == Some(binding('1')) && seen.registration.id == "1".repeat(32),
        "the next observer reads the acknowledged binding, and refreshes it"
    );
    drop(next);
    assert!(
        read(&state, "s-2").expect("the other record").binding == Some(binding('4')),
        "the other tab's record is as its observer left it"
    );
}
