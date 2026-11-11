// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const S: u64 = 1_000_000_000;

fn read(at_s: u64, rows: &[(u64, u32, &str, u64)]) -> GpuClientsSample {
    GpuClientsSample {
        at_ns: at_s * S,
        complete: true,
        entries: rows.iter().map(|r| r.0).collect(),
        rows: rows
            .iter()
            .map(|&(entry, pid, name, gpu_ns)| GpuClientRow {
                entry,
                pid,
                name: name.into(),
                gpu_ns,
            })
            .collect(),
    }
}

fn close(a: Option<f32>, b: f32) -> bool {
    a.is_some_and(|a| (a - b).abs() < 1e-6)
}

#[test]
fn the_first_read_is_a_baseline() {
    let mut g = GpuShares::default();
    g.absorb(Some(&read(10, &[(1, 7, "mtl", 5 * S)])), None, 10 * S);
    assert!(!g.held());
    assert_eq!(g.share(7, "mtl"), None);
    assert_eq!(g.clients(), None);
}

#[test]
fn a_delta_over_the_window_is_its_share() {
    let mut g = GpuShares::default();
    g.absorb(Some(&read(10, &[(1, 7, "mtl", 5 * S)])), None, 10 * S);
    g.absorb(Some(&read(14, &[(1, 7, "mtl", 6 * S)])), None, 14 * S);
    assert!(close(g.share(7, "mtl"), 0.25), "{g:?}");
    assert!(close(g.clients(), 0.25));
}

/// κ is the device's mean busy over the client window, ū, against the clients' summed
/// shares, R, where their queues overlap (R > ū), and 1 where they do not, where ū is unread
/// and where no client ran. ū is the mean of the device readings since the last client read,
/// this one's included, and starts afresh at each client read.
#[test]
fn kappa_turns_overlapping_client_time_into_shares_of_the_devices_busy_time() {
    let near = |a: Option<f32>, b: f32| a.is_some_and(|a| (a - b).abs() < 1e-6);
    let mut g = GpuShares::default();
    assert_eq!(g.kappa(), None, "no shares held");
    g.absorb(
        Some(&read(10, &[(1, 7, "a", 0), (2, 8, "w", 0)])),
        Some(0.9),
        10 * S,
    );
    g.absorb(None, Some(0.04), 12 * S);
    // The clients ran 58 % of the window (53 % and 5 %) while the device was busy 4 % and
    // 6 %, a mean of 5 %: the 0.9 before the window's start is the baseline's.
    g.absorb(
        Some(&read(20, &[(1, 7, "a", 53 * S / 10), (2, 8, "w", S / 2)])),
        Some(0.06),
        20 * S,
    );
    assert!(near(g.clients(), 0.58), "{g:?}");
    assert!(near(g.kappa(), 0.05 / 0.58), "{g:?}");
    assert!(
        near(g.share(7, "a"), 0.53),
        "the shares stay raw client time"
    );
    // A window whose clients ran less than the device was busy keeps their time: κ is 1.
    g.absorb(
        Some(&read(30, &[(1, 7, "a", 63 * S / 10), (2, 8, "w", S / 2)])),
        Some(0.5),
        30 * S,
    );
    assert!(near(g.kappa(), 1.0), "{g:?}");
    // With no device reading in the window, ū is unread: κ is 1.
    g.absorb(
        Some(&read(40, &[(1, 7, "a", 163 * S / 10), (2, 8, "w", S / 2)])),
        None,
        40 * S,
    );
    assert!(near(g.clients(), 1.0) && near(g.kappa(), 1.0), "{g:?}");
    assert_eq!(super::kappa(Some(0.0), 0.0), 1.0, "no client ran");
    assert_eq!(super::kappa(Some(0.2), 0.4), 0.5);
    assert_eq!(super::kappa(None, 0.4), 1.0);
}

#[test]
fn a_client_born_inside_the_window_counts_all_its_time() {
    let mut g = GpuShares::default();
    g.absorb(Some(&read(10, &[(1, 7, "mtl", 5 * S)])), None, 10 * S);
    g.absorb(
        Some(&read(14, &[(1, 7, "mtl", 5 * S), (2, 8, "new", S)])),
        None,
        14 * S,
    );
    assert!(close(g.share(8, "new"), 0.25), "{g:?}");
    assert!(close(g.share(7, "mtl"), 0.0));
}

/// A child a complete read saw without a readable counter is no birth in the next window;
/// its later deltas count.
#[test]
fn a_present_child_without_a_counter_is_no_birth() {
    let mut g = GpuShares::default();
    let mut first = read(10, &[(1, 7, "mtl", 5 * S)]);
    first.entries.extend([2, 4]); // Unread client and ordinary non-GPU child.
    g.absorb(Some(&first), None, 10 * S);
    let next = read(
        14,
        &[
            (1, 7, "mtl", 6 * S),
            (2, 88, "WindowServer", 900 * S),
            (3, 9, "new", 2 * S),
        ],
    );
    g.absorb(Some(&next), None, 14 * S);
    assert!(close(g.share(88, "WindowServer"), 0.0), "{g:?}");
    assert!(close(g.share(7, "mtl"), 0.25));
    assert!(close(g.share(9, "new"), 0.5), "a genuinely new entry");
    assert!(close(g.clients(), 0.75));

    g.absorb(
        Some(&read(
            18,
            &[
                (1, 7, "mtl", 6 * S),
                (2, 88, "WindowServer", 901 * S),
                (3, 9, "new", 2 * S),
            ],
        )),
        None,
        18 * S,
    );
    assert!(close(g.share(88, "WindowServer"), 0.25));
    assert!(close(g.clients(), 0.25));
}

/// A walk the registry changed under ends early: the clients it missed are old, not born
/// inside the next window, so their lifetime's time is no share (WindowServer's would read
/// as the whole GPU), while a client both reads saw still counts its delta.
#[test]
fn a_client_an_incomplete_read_missed_is_no_birth() {
    let mut g = GpuShares::default();
    let mut cut = read(10, &[(1, 7, "mtl", 5 * S)]);
    cut.complete = false;
    g.absorb(Some(&cut), None, 10 * S);
    g.absorb(
        Some(&read(
            14,
            &[(1, 7, "mtl", 6 * S), (2, 88, "WindowServer", 900 * S)],
        )),
        None,
        14 * S,
    );
    assert!(close(g.share(88, "WindowServer"), 0.0), "{g:?}");
    assert!(close(g.share(7, "mtl"), 0.25), "{g:?}");
    assert!(close(g.clients(), 0.25));
    // That read was complete: a client new after it is born in the window.
    g.absorb(
        Some(&read(
            18,
            &[
                (1, 7, "mtl", 6 * S),
                (2, 88, "WindowServer", 901 * S),
                (3, 9, "new", 2 * S),
            ],
        )),
        None,
        18 * S,
    );
    assert!(close(g.share(88, "WindowServer"), 0.25), "{g:?}");
    assert!(close(g.share(9, "new"), 0.5), "{g:?}");
}

#[test]
fn a_gone_client_drops_out_without_going_negative() {
    let mut g = GpuShares::default();
    g.absorb(
        Some(&read(10, &[(1, 7, "mtl", 5 * S), (2, 8, "gone", 9 * S)])),
        None,
        10 * S,
    );
    g.absorb(Some(&read(14, &[(1, 7, "mtl", 7 * S)])), None, 14 * S);
    assert!(close(g.share(7, "mtl"), 0.5));
    assert!(close(g.share(8, "gone"), 0.0));
    assert!(close(g.clients(), 0.5));
    // A counter that went back reads 0, never a wrap.
    g.absorb(Some(&read(18, &[(1, 7, "mtl", S)])), None, 18 * S);
    assert!(close(g.share(7, "mtl"), 0.0));
}

#[test]
fn two_clients_of_one_process_sum_and_clamp_at_the_whole_gpu() {
    let mut g = GpuShares::default();
    let zero = [(1, 7, "mtl", 0), (2, 7, "mtl", 0), (3, 9, "x", 0)];
    g.absorb(Some(&read(10, &zero)), None, 10 * S);
    let half = [(1, 7, "mtl", S), (2, 7, "mtl", S), (3, 9, "x", 3 * S)];
    g.absorb(Some(&read(14, &half)), None, 14 * S);
    assert!(close(g.share(7, "mtl"), 0.5), "{g:?}");
    assert!(close(g.share(9, "x"), 0.75));
    let over = [
        (1, 7, "mtl", 4 * S),
        (2, 7, "mtl", 4 * S),
        (3, 9, "x", 3 * S),
    ];
    g.absorb(Some(&read(18, &over)), None, 18 * S);
    assert!(close(g.share(7, "mtl"), 1.0), "{g:?}");
    assert!(close(g.clients(), 1.0));
}

#[test]
fn a_name_that_is_not_the_clients_takes_no_gpu_time() {
    let mut g = GpuShares::default();
    g.absorb(Some(&read(10, &[(1, 7, "mtl", 0)])), None, 10 * S);
    g.absorb(Some(&read(14, &[(1, 7, "mtl", 2 * S)])), None, 14 * S);
    assert!(close(g.share(7, "zsh"), 0.0));
    assert!(close(g.share(7, "mtl"), 0.5));
    assert!(close(g.share(99, "mtl"), 0.0));
    let long = "com.apple.WebKit.GPU";
    g.absorb(Some(&read(18, &[(1, 7, &long[..16], 4 * S)])), None, 18 * S);
    assert!(close(g.share(7, long), 0.5), "a cut p_comm is its name");
    assert!(
        close(g.share(7, "com.apple.WebKi\u{2026}"), 0.5),
        "so is a name the sweep marked cut"
    );
}

#[test]
fn held_shares_expire_and_a_stale_baseline_gives_none() {
    let mut g = GpuShares::default();
    g.absorb(Some(&read(10, &[(1, 7, "mtl", 0)])), None, 10 * S);
    g.absorb(Some(&read(14, &[(1, 7, "mtl", 2 * S)])), None, 14 * S);
    let max_s = WINDOW_MAX_NS / S;
    g.absorb(None, None, (14 + max_s) * S);
    assert!(g.held(), "held through the window");
    g.absorb(None, None, (15 + max_s) * S);
    assert!(!g.held());
    assert_eq!(g.share(7, "mtl"), None);
    g.absorb(
        Some(&read(16 + 2 * max_s, &[(1, 7, "mtl", 9 * S)])),
        None,
        (16 + 2 * max_s) * S,
    );
    assert!(
        !g.held(),
        "a read past the window after the last is a baseline"
    );
}
