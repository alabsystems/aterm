// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The tree meter's booking on fixed members, and on macOS its read of a live stand-in that
//! touches 256 MiB beside one that touches nothing.

use super::*;

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
        pid != 9
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
    none.book(&[member(9, 4000, 4000)], &mut |_| false);
    assert_eq!(none.peak(), 0);
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
        &mut |pid| pid != 9,
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
    meter.read();
    root.kill().expect("the root killed");
    root.wait().expect("the root reaped");
    assert_eq!(
        meter.left(),
        [started],
        "the root's child, and not its sibling"
    );
    drop(root.stdin.take());
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the left child's end",
        || meter.left().is_empty().then_some(()),
    );
    drop(sibling.0.stdin.take());
    assert!(sibling.0.wait().expect("the sibling's end").success());
}
