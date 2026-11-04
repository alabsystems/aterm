// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The tree meter's booking on fixed members, and on macOS its read of a live stand-in that
//! touches 256 MiB beside one that touches nothing.

use super::*;

/// An accounting snapshot may omit a process it could not measure. The actual owned child
/// is still executing, so that omission cannot prove the observed work drained.
#[cfg(target_os = "macos")]
#[test]
fn an_omitted_live_identity_cannot_make_the_observed_tree_drained() {
    use aterm_uds::spawnfd::{self, Stdio};

    let mut child = spawnfd::command("/bin/cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let input = aterm_uds::deadline::cat_echoes(&mut child);
    let pid = child.id();
    let meter = TreeMeter::start(pid);
    meter.read();
    let before = meter.left_observed();
    let unmetered = meter.survivors_using_read(|_| Some(Vec::new()));
    let unknown = meter.watch().left_using(|_| Some(Vec::new()), |_| Err(()));
    let omitted = meter.watch().left_using(|_| Some(Vec::new()), known_alive);
    drop(input);
    let status = child.wait().unwrap();
    assert!(status.success());
    assert_eq!(
        unmetered,
        Survivors::Unread,
        "a live survivor's missing footprint is not zero use"
    );
    assert_eq!(
        unknown,
        Left::Unread,
        "unknown liveness cannot close a known identity"
    );
    assert!(
        matches!(before, Left::Read(ref identities) if identities.iter().any(|&(seen, _)| seen == pid))
    );
    assert!(
        matches!(omitted, Left::Read(ref identities) if identities.iter().any(|&(seen, _)| seen == pid)),
        "a missing accounting row is not drain: {omitted:?}"
    );
    assert_eq!(
        meter.watch().left_using(|_| Some(Vec::new()), known_alive),
        Left::Read(Vec::new())
    );
}

const MIB: u64 = 1 << 20;

fn member(pid: u32, footprint_mib: u64, peak_mib: u64) -> Member {
    Member {
        pid,
        start: 7,
        footprint: footprint_mib * MIB,
        peak: peak_mib * MIB,
    }
}

/// A read sums the members in the tree and keeps the most of those sums; a member's lifetime
/// peak floors it; each member is asked about once. NEGATIVE CONTROL: a member outside the tree
/// (the front itself, a pipeline's other stage) adds nothing, however large.
#[test]
fn a_read_sums_the_trees_members_and_their_peaks_floor_it() {
    let mut tally = Tally::default();
    let mut asked = Vec::new();
    let mut in_tree = |pid: u32| {
        asked.push(pid);
        Some(pid != 9)
    };
    tally.book(
        &[
            member(1, 100, 100),
            member(2, 300, 300),
            member(9, 4000, 4000),
        ],
        &mut in_tree,
    );
    assert_eq!(tally.peak(), 400 * MIB);
    tally.book(
        &[
            member(1, 50, 100),
            member(2, 10, 900),
            member(9, 4000, 4000),
        ],
        &mut in_tree,
    );
    assert_eq!(tally.peak(), 900 * MIB, "member 2's lifetime peak");
    assert_eq!(asked, [1, 2, 9], "each member asked once");
    let mut none = Tally::default();
    none.book(&[member(9, 4000, 4000)], &mut |_| Some(false));
    assert_eq!(none.peak(), 0);
}

/// A member whose ancestry one read cannot decide is not booked outside for good: it
/// adds nothing that read, a deciding read still takes it in, and until one does it is
/// what the tree leaves under a known parent. NEGATIVE CONTROL: a member decided
/// outside is never left, under that same parent or any other.
#[test]
fn an_undecided_member_is_not_booked_outside_for_good() {
    let mut tally = Tally::default();
    tally.book(&[member(1, 10, 10), member(9, 10, 10)], &mut |pid| {
        Some(pid == 1)
    });
    tally.book(&[member(20, 300, 300)], &mut |_| None);
    assert_eq!(
        tally.peak(),
        10 * MIB,
        "the undecided member adds nothing yet"
    );
    let now = [alive(1, 7, 1), alive(20, 7, 1), alive(9, 7, 1)];
    let mut left = tally.left(&now);
    left.sort_unstable();
    assert_eq!(
        left,
        [1, 20],
        "still found under its known parent, unlike 9"
    );
    tally.book(&[member(20, 300, 300)], &mut |_| Some(true));
    assert_eq!(tally.peak(), 300 * MIB, "a deciding read takes it in");
    let mut outside = Tally::default();
    outside.book(
        &[member(1, 10, 10), member(9, 10, 10), member(20, 300, 300)],
        &mut |pid| Some(pid == 1),
    );
    assert_eq!(outside.left(&now), [1], "decided outside stays outside");
}

/// A cleanup query is itself a positive observation: discovering a child there
/// must retain its identity even if its parent ends before the next memory sample.
#[test]
fn a_child_discovered_by_cleanup_stays_known_after_its_parent_ends() {
    let mut tally = Tally::default();
    tally.book(&[member(10, 10, 10)], &mut |_| Some(true));
    assert_eq!(
        tally.left_identities(&[alive(10, 7, 1), alive(20, 7, 10)]),
        [(10, 7), (20, 7)]
    );
    assert_eq!(
        tally.left_identities(&[alive(20, 7, 1)]),
        [(20, 7)],
        "the child's observed identity survives its parent's end"
    );
    assert!(
        tally.left_identities(&[alive(20, 8, 1)]).is_empty(),
        "a later birth at that pid inherits no membership"
    );
}

/// Ancestry is decided against the original root and the members already booked
/// inside, on an injected parent walk (pid reuse itself cannot be forced): a chain to
/// a live original root or to a known member is inside; a chain to the root's pid
/// under a later process authorizes nothing. A completed chain to the system parent
/// decides outside only while the original root lives; unread metadata and bounded
/// walks never decide outside, even then. No outside member grants ancestry.
#[test]
fn ancestry_is_fenced_to_the_original_root_and_known_members() {
    // 20 -> 5 (known) -> 2 (root); 21 -> 2; 22 -> 9 (outside) -> 1 (system);
    // 23 -> 1; 24 unread; 25 cycles (the bound must leave it undecided).
    let parents: HashMap<u32, u32> = [(20, 5), (5, 2), (21, 2), (22, 9), (9, 1), (23, 1), (25, 25)]
        .into_iter()
        .collect();
    let anchors: HashSet<u32> = [5].into_iter().collect();
    let verdict = |pid: u32, state: RootState| {
        ancestry_verdict(pid, 2, state, &anchors, &mut |at| parents.get(&at).copied())
    };
    assert_eq!(verdict(21, RootState::Live), Some(true), "to the live root");
    assert_eq!(
        verdict(21, RootState::Ended),
        None,
        "a later process with the root's pid grants no ancestry"
    );
    assert_eq!(verdict(21, RootState::Unknown), None, "the root unread");
    assert_eq!(
        verdict(20, RootState::Ended),
        Some(true),
        "to a known member, the root gone"
    );
    assert_eq!(
        verdict(22, RootState::Live),
        Some(false),
        "out, the root living"
    );
    assert_eq!(verdict(22, RootState::Ended), None, "out, the root gone");
    assert_eq!(verdict(23, RootState::Live), Some(false));
    assert_eq!(verdict(23, RootState::Ended), None);
    assert_eq!(verdict(23, RootState::Unknown), None);
    assert_eq!(
        verdict(2, RootState::Ended),
        None,
        "the reused root pid itself"
    );
    for state in [RootState::Live, RootState::Ended, RootState::Unknown] {
        assert_eq!(verdict(24, state), None, "missing parent metadata");
        assert_eq!(
            verdict(25, state),
            None,
            "a bounded walk is not outside proof"
        );
    }
}

#[cfg(target_os = "macos")]
const STANDIN: &str = "ATERM_CAPACITY_METER_STANDIN";

/// What the stand-in says once its memory is touched.
#[cfg(target_os = "macos")]
const TOUCHED: &str = "ATERM_CAPACITY_METER_TOUCHED";

/// A stand-in, killed when it is dropped still running, so none outlives its test.
#[cfg(target_os = "macos")]
struct Standin(aterm_uds::spawnfd::Child);

#[cfg(target_os = "macos")]
impl Drop for Standin {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

/// Start [`standin_touches_memory`] touching `mib` MiB, once it says so.
#[cfg(target_os = "macos")]
fn touching(mib: u64) -> Standin {
    use std::io::BufRead as _;
    let mut child = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"))
        .args([
            "--exact",
            "tree_meter::tests::standin_touches_memory",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(STANDIN, mib.to_string())
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("the stand-in");
    let out = child.stdout.take().expect("its stdout");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut lines = std::io::BufReader::new(out).lines();
        // libtest writes `test <name> ... ` before the test's own output, on its line.
        let ready = lines.any(|l| l.is_ok_and(|l| l.ends_with(TOUCHED)));
        let _ = tx.send(ready);
        // Drained to its end, so libtest's last lines find a reader.
        lines.for_each(drop);
    });
    assert!(
        rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("hang detector: the stand-in never touched its memory"),
        "the stand-in ended before it touched its memory"
    );
    Standin(child)
}

/// The stand-in's own end: touch the MiB `STANDIN` names, say so, and hold them until stdin
/// ends. Run alone, it does nothing.
#[cfg(target_os = "macos")]
#[test]
fn standin_touches_memory() {
    use std::io::{Read as _, Write as _};
    let Some(mib) = std::env::var(STANDIN)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
    else {
        return;
    };
    let held = vec![1u8; mib << 20];
    println!("{TOUCHED}");
    let _ = std::io::stdout().flush();
    let mut rest = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut rest);
    assert_eq!(held.len(), mib << 20);
}

/// On macOS a meter on a stand-in that touched 256 MiB reads at least that, and keeps it once
/// the stand-in has ended. NEGATIVE CONTROL: a meter on its sibling, which touched nothing and
/// shares its process group, reads far less: the group's other members are not its tree's.
#[cfg(target_os = "macos")]
#[test]
fn a_meter_reads_its_trees_memory_and_not_its_siblings() {
    let mut heavy = touching(256);
    let light = touching(0);
    let on_heavy = TreeMeter::start(heavy.0.id());
    let on_light = TreeMeter::start(light.0.id());
    on_heavy.read();
    on_light.read();
    assert!(
        on_heavy.peak() >= 256 * MIB,
        "read {} MiB",
        on_heavy.peak() / MIB
    );
    assert!(
        on_light.peak() < 64 * MIB,
        "read {} MiB",
        on_light.peak() / MIB
    );
    drop(heavy.0.stdin.take());
    assert!(heavy.0.wait().expect("its end").success());
    assert!(on_heavy.stop(0) >= 256 * MIB, "kept once it ended");
    assert_eq!(on_light.stop(5 << 30), 5 << 30, "the floor holds");
}

fn alive(pid: u32, start: u64, ppid: u32) -> Alive {
    Alive { pid, start, ppid }
}

/// What a tree leaves is each live member a read booked inside it, the same process by its
/// start, and the children since of one left, however deep. NEGATIVE CONTROL: a member booked
/// outside it (the front, a pipeline's other stage), a child of one, and a pid taken anew by
/// another process are never left.
#[test]
fn what_a_tree_leaves_is_its_members_and_their_children_since() {
    let mut tally = Tally::default();
    tally.book(
        &[member(1, 10, 10), member(2, 10, 10), member(9, 10, 10)],
        &mut |pid| Some(pid != 9),
    );
    let now = [
        alive(1, 7, 1),
        alive(2, 8, 50),
        alive(9, 7, 1),
        alive(20, 7, 1),
        alive(21, 7, 20),
        alive(30, 7, 9),
    ];
    let mut left = tally.left(&now);
    left.sort_unstable();
    assert_eq!(left, [1, 20, 21]);
    assert!(
        Tally::default().left(&now).is_empty(),
        "nothing read, nothing left"
    );
    let mut identities = tally.left_identities(&now);
    identities.sort_unstable();
    assert_eq!(
        identities,
        [(1, 7), (20, 7), (21, 7)],
        "each left process by its pid and its start"
    );
}

/// A failed read of a known group and a group no read ever found are each their own answer:
/// neither is a reading of none left, and only a succeeded read of none is [`Left::Read`]
/// empty. NEGATIVE CONTROL: the same `now` that reads none left reads nothing at all where
/// its read failed.
#[test]
fn an_unread_group_is_no_reading_of_none_left() {
    let mut tally = Tally {
        group: Some(9),
        ..Tally::default()
    };
    tally.book(&[member(1, 10, 10)], &mut |_| Some(true));
    assert_eq!(tally.left_observed(None), Left::Unread);
    assert_eq!(tally.left_observed(Some(&[])), Left::Read(Vec::new()));
    assert_eq!(
        tally.left_observed(Some(&[alive(1, 7, 1)])),
        Left::Read(vec![(1, 7)])
    );
    let mut unfound = Tally::default();
    unfound.book(&[member(1, 10, 10)], &mut |_| Some(true));
    assert_eq!(unfound.left_observed(Some(&[])), Left::Unobserved);
    assert_eq!(unfound.left_observed(None), Left::Unobserved);
}

/// On macOS a meter whose root no kernel record names observed no group: what its tree
/// leaves is unobserved, never a reading of none, and no survivor read answers either.
#[cfg(target_os = "macos")]
#[test]
fn a_meter_on_an_unfound_root_observed_nothing() {
    let meter = TreeMeter::start(u32::MAX - 1);
    assert_eq!(meter.left_observed(), Left::Unobserved);
    assert_eq!(meter.survivors_using_observed(), Survivors::Unobserved);
    assert_eq!(meter.stop(0), 0);
}

/// On macOS a tree whose root is killed leaves the process the root started, reparented away,
/// until it ends. NEGATIVE CONTROL: a sibling in the root's process group, which the root did
/// not start, is never left.
#[cfg(target_os = "macos")]
#[test]
fn a_killed_roots_tree_leaves_what_it_started_and_not_its_sibling() {
    use std::io::BufRead as _;
    let mut root = aterm_uds::spawnfd::command("/bin/sh")
        .args(["-c", "exec 3<&0; /bin/cat <&3 & echo \"LEFT $!\"; wait"])
        .process_group(0)
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("the root");
    let group = i32::try_from(root.id()).expect("a pid");
    let mut sibling = Standin(
        aterm_uds::spawnfd::command("/bin/cat")
            .process_group(group)
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .expect("the sibling"),
    );
    let out = root.stdout.take().expect("its stdout");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let said = std::io::BufReader::new(out)
            .lines()
            .find_map(|l| l.ok()?.strip_prefix("LEFT ")?.trim().parse::<u32>().ok());
        let _ = tx.send(said);
    });
    let started = rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("hang detector: the root never started its child")
        .expect("the root said its child's pid");
    let meter = TreeMeter::start(root.id());
    // A read is a sample: the root says its child's pid as soon as it forked, and a read made
    // then may not list the child yet (under load the meter missed it in every read before the
    // kill, and a member first seen once its root is gone is never the tree's). The kill waits
    // until a read has booked the child inside, as the meter's own reads would.
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "a read that books the root's child inside its tree",
        || {
            meter.read();
            meter.left().contains(&started).then_some(())
        },
    );
    let child_stdin = root.stdin.take().expect("the left child's stdin");
    root.kill().expect("the root killed");
    root.wait().expect("the root reaped");
    assert_eq!(
        meter.left(),
        [started],
        "the root's child, and not its sibling"
    );
    let Left::Read(identities) = meter.left_observed() else {
        panic!("a read of the known group: {:?}", meter.left_observed());
    };
    assert_eq!(
        identities.len(),
        1,
        "the left child's one identity: {identities:?}"
    );
    assert_eq!(identities[0].0, started, "the left child by its pid");
    drop(child_stdin);
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the left child's end",
        || meter.left().is_empty().then_some(()),
    );
    assert_eq!(
        meter.left_observed(),
        Left::Read(Vec::new()),
        "a succeeded read of none left"
    );
    drop(sibling.0.stdin.take());
    assert!(sibling.0.wait().expect("the sibling's end").success());
}

/// A ROOT'S SURVIVORS are the live members that descended from it at the last read made while it
/// lived, the same process by its start: a compiler its killed root left. NEGATIVE CONTROL: a
/// daemon an earlier read saw inside that forked away while the root lived, a member booked
/// outside, and a child born since are none (each but the outside one is what [`Tally::left`]
/// leaves); and a read made once the root is gone, which finds nothing descending, changes none.
#[test]
fn a_roots_survivors_descended_from_it_at_the_last_read_it_lived_for() {
    let mut tally = Tally::default();
    let read = [
        member(1, 10, 10),
        member(2, 10, 10),
        member(3, 10, 10),
        member(9, 10, 10),
    ];
    tally.book(&read, &mut |pid| Some(pid != 9));
    tally.mark_within(&read, &mut |pid| pid != 9, &mut || true);
    tally.mark_within(&read, &mut |pid| pid == 1 || pid == 2, &mut || true);
    tally.mark_within(&read, &mut |_| false, &mut || false);
    let now = [
        alive(2, 7, 1),
        alive(3, 7, 1),
        alive(9, 7, 1),
        alive(4, 7, 2),
    ];
    assert_eq!(tally.survivors(&now), [2], "the compiler alone");
    let using: Vec<(Alive, u64)> = now.iter().map(|a| (*a, u64::from(a.pid) * MIB)).collect();
    assert_eq!(
        tally.survivors_using(&using),
        (vec![2], 2 * MIB),
        "its footprint alone"
    );
    let mut left = tally.left(&now);
    left.sort_unstable();
    assert_eq!(left, [2, 3, 4], "what the tree leaves is wider");
    assert!(Tally::default().survivors(&now).is_empty(), "nothing read");
}

/// Set, the test binary is [`standin_touches_when_told`]: the MiB it touches once told.
#[cfg(target_os = "macos")]
const TOLD: &str = "ATERM_CAPACITY_METER_TOLD";

/// The stand-in that touches the MiB `TOLD` names only once a line comes on stdin, says so
/// ([`TOUCHED`]), and holds them until stdin ends. Run alone, it does nothing.
#[cfg(target_os = "macos")]
#[test]
fn standin_touches_when_told() {
    use std::io::{BufRead as _, Read as _, Write as _};
    let Some(mib) = std::env::var(TOLD)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
    else {
        return;
    };
    let mut stdin = std::io::stdin().lock();
    let mut line = String::new();
    let _ = stdin.read_line(&mut line);
    let held = vec![1u8; mib << 20];
    println!("{TOUCHED}");
    let _ = std::io::stdout().flush();
    let mut rest = Vec::new();
    let _ = stdin.read_to_end(&mut rest);
    assert_eq!(held.len(), mib << 20);
}

/// On macOS a meter goes on reading its tree's group once its root is gone: a killed root's
/// survivor (the compiler a killed `cargo` leaves) is named, and the 256 MiB it touches after the
/// root's end are read. NEGATIVE CONTROL (by hand: `members_of` reading the root's group anew at
/// each read, as before): the read after the root's end finds nothing, and the peak stays below
/// what the survivor touched.
#[cfg(target_os = "macos")]
#[test]
fn a_meter_reads_what_survives_its_root() {
    use std::io::{BufRead as _, Write as _};
    let exe = std::env::current_exe().expect("test binary");
    let mut root = aterm_uds::spawnfd::command("/bin/sh")
        .args([
            "-c",
            "exec 3<&0; \"$0\" --exact tree_meter::tests::standin_touches_when_told \
             --test-threads=1 --nocapture <&3 & echo \"LEFT $!\"; wait",
        ])
        .arg(&exe)
        .env(TOLD, "256")
        .process_group(0)
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("the root");
    let out = root.stdout.take().expect("its stdout");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(out).lines() {
            let Ok(line) = line else { return };
            let said = match line.strip_prefix("LEFT ") {
                Some(pid) => pid.trim().parse::<u32>().ok(),
                None => line.ends_with(TOUCHED).then_some(0),
            };
            if let Some(said) = said {
                let _ = tx.send(said);
            }
        }
    });
    let next = || {
        rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("hang detector: the root or its survivor fell silent")
    };
    let survivor = next();
    let meter = TreeMeter::start(root.id());
    meter.read();
    assert!(meter.peak() < 64 * MIB, "read {} MiB", meter.peak() / MIB);
    let mut tell = root.stdin.take().expect("the survivor's stdin");
    root.kill().expect("the root killed");
    root.wait().expect("the root reaped");
    assert_eq!(meter.survivors(), [survivor], "its survivor");
    tell.write_all(b"touch\n").expect("told");
    assert_eq!(next(), 0, "it touched its memory");
    meter.read();
    assert!(
        meter.peak() >= 256 * MIB,
        "a read after the root's end: {} MiB",
        meter.peak() / MIB
    );
    drop(tell);
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the survivor's end", || {
        meter.survivors().is_empty().then_some(())
    });
}

/// Set, the test binary is [`standin_postroot_parent`]: the file its child exits for.
#[cfg(target_os = "macos")]
const POSTROOT: &str = "ATERM_CAPACITY_METER_POSTROOT";

/// The leftover parent: once a line comes on stdin it starts
/// [`standin_postroot_child`], says its pid, and holds itself until stdin ends. Its
/// child is left running when it ends. Run alone, it does nothing.
#[cfg(target_os = "macos")]
#[test]
fn standin_postroot_parent() {
    use std::io::{BufRead as _, Read as _, Write as _};
    let Some(exit_for) = std::env::var_os(POSTROOT) else {
        return;
    };
    let mut stdin = std::io::stdin().lock();
    let mut line = String::new();
    let _ = stdin.read_line(&mut line);
    let child = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"))
        .args([
            "--exact",
            "tree_meter::tests::standin_postroot_child",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(POSTROOT, &exit_for)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("the post-root child");
    println!("SPAWNED {}", child.id());
    let _ = std::io::stdout().flush();
    let mut rest = Vec::new();
    let _ = stdin.read_to_end(&mut rest);
}

/// The post-root child: it runs until the file `POSTROOT` names appears. Run alone, it
/// does nothing.
#[cfg(target_os = "macos")]
#[test]
fn standin_postroot_child() {
    let Some(exit_for) = std::env::var_os(POSTROOT) else {
        return;
    };
    while !std::path::Path::new(&exit_for).exists() {
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Ends the post-root fixtures however the test below ends: the child for its file,
/// the parent for its stdin.
#[cfg(target_os = "macos")]
struct PostrootCleanup {
    exit_for: std::path::PathBuf,
    tell: Option<std::process::ChildStdin>,
}

#[cfg(target_os = "macos")]
impl Drop for PostrootCleanup {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.exit_for, b"end");
        drop(self.tell.take());
    }
}

/// On macOS a child a known leftover starts only after its root has ended is still
/// what the tree leaves, and stays so once that leftover itself ends: no read made
/// with the root gone may book it outside the tree for good. NEGATIVE CONTROL: the
/// sibling in the root's group is never left, before or after.
#[cfg(target_os = "macos")]
#[test]
fn a_child_born_to_a_leftover_after_its_root_ended_stays_left() {
    use std::io::{BufRead as _, Write as _};
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let exit_for = scratch.path().join("child-exits");
    let exe = std::env::current_exe().expect("test binary");
    let mut root = aterm_uds::spawnfd::command("/bin/sh")
        .args([
            "-c",
            "exec 3<&0; \"$0\" --exact tree_meter::tests::standin_postroot_parent \
             --test-threads=1 --nocapture <&3 2>&1 & echo \"LEFT $!\"; wait",
        ])
        .arg(&exe)
        .env(POSTROOT, &exit_for)
        .process_group(0)
        .stdin(aterm_uds::spawnfd::Stdio::piped())
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("the root");
    let group = i32::try_from(root.id()).expect("a pid");
    let mut sibling = Standin(
        aterm_uds::spawnfd::command("/bin/cat")
            .process_group(group)
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .spawn()
            .expect("the sibling"),
    );
    let mut cleanup = PostrootCleanup {
        exit_for: exit_for.clone(),
        tell: root.stdin.take(),
    };
    let out = root.stdout.take().expect("its stdout");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(out).lines() {
            let Ok(line) = line else { return };
            let mut said = false;
            for tag in ["LEFT ", "SPAWNED "] {
                // The shell's own line opens with its tag; libtest prints
                // `test <name> ... ` before the stand-in's, on its line.
                if let Some(pid) = line
                    .rsplit_once(tag)
                    .and_then(|(_, pid)| pid.trim().parse::<u32>().ok())
                {
                    said = true;
                    let _ = tx.send((tag, pid));
                }
            }
            if !said {
                eprintln!("postroot fixture: {line}");
            }
        }
    });
    let next = |tag: &'static str| {
        let (said, pid) = rx
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("hang detector: the root or its leftover fell silent");
        assert_eq!(said, tag, "the fixture's own order");
        pid
    };
    let leftover = next("LEFT ");
    let meter = TreeMeter::start(root.id());
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "a read that books the leftover inside its tree",
        || {
            meter.read();
            meter.left().contains(&leftover).then_some(())
        },
    );
    root.kill().expect("the root killed");
    root.wait().expect("the root reaped");
    cleanup
        .tell
        .as_mut()
        .expect("the leftover's stdin")
        .write_all(b"spawn\n")
        .expect("told");
    let child = next("SPAWNED ");
    meter.read();
    let left = meter.left();
    assert!(
        left.contains(&leftover) && left.contains(&child) && !left.contains(&sibling.0.id()),
        "the leftover and its post-root child, not its sibling: {left:?}"
    );
    drop(cleanup.tell.take());
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the leftover's end", || {
        (!meter.left().contains(&leftover)).then_some(())
    });
    assert_eq!(
        meter.left(),
        [child],
        "the post-root child alone once its parent ended"
    );
    std::fs::write(&exit_for, b"end").expect("the child's end is named");
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the child's end", || {
        meter.left().is_empty().then_some(())
    });
    assert_eq!(meter.left_observed(), Left::Read(Vec::new()));
    drop(sibling.0.stdin.take());
    assert!(sibling.0.wait().expect("the sibling's end").success());
}
