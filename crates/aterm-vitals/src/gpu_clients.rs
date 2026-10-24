// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! GPU time per process: the host's reads of the GPU's user clients (cumulative GPU time per
//! client, macOS's `AppUsage`), turned into each process's share of the device over the window
//! between two reads.

use std::collections::{HashMap, HashSet};

use crate::explain::WINDOW_MAX_NS;

/// One GPU user client as the host read it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GpuClientRow {
    /// The client's registry entry id, unique for the boot.
    pub entry: u64,
    /// The process that opened it.
    pub pid: u32,
    /// That process's short name as the client names it (`p_comm`, 16 bytes at most).
    pub name: String,
    /// GPU time the client's queues ran, summed, ns; cumulative while the client lives.
    pub gpu_ns: u64,
}

/// Every GPU user client, read together.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GpuClientsSample {
    /// When it was read (ns on [`crate::Sample::at_ns`]'s clock).
    pub at_ns: u64,
    /// The clients.
    pub rows: Vec<GpuClientRow>,
    /// Every enumerated accelerator child's entry id, including children whose counter
    /// properties were absent or unreadable.
    pub entries: Vec<u64>,
    /// Whether every child's entry id was read. A client missing from an incomplete read may
    /// still have been there, so the next read counts none of its time rather than all of it.
    pub complete: bool,
}

/// The GPU's client time by owner over the last client window, each a share of the device
/// (`1.0` is the whole GPU busy for the window).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GpuSplit {
    /// Every client's, each process clamped at `1.0`, summed.
    pub clients: f32,
    /// aterm's and its tabs'.
    pub aterm: f32,
    /// The user's processes outside aterm.
    pub others: f32,
    /// Other users' processes, those whose owner was not read, and client time no swept process
    /// claims (a process gone since, or a reused pid).
    pub services: f32,
}

/// The last client read, the next window's start.
#[derive(Clone, Debug)]
struct Baseline {
    /// When it was read.
    at_ns: u64,
    /// Each client entry's time.
    ns: HashMap<u64, u64>,
    /// Every enumerated child, whether or not its counter was readable.
    entries: HashSet<u64>,
    /// Whether it read every child identity ([`GpuClientsSample::complete`]).
    complete: bool,
}

/// Each process's GPU share, from the last two client reads; held between reads and dropped
/// [`WINDOW_MAX_NS`] after the reads that made it.
#[derive(Clone, Debug, Default)]
pub(crate) struct GpuShares {
    /// The last read.
    last: Option<Baseline>,
    /// Each pid's name and share of the device over the last window.
    rates: HashMap<u32, (String, f32)>,
    /// When the read that made [`Self::rates`] was taken; `None` while none is held.
    at: Option<u64>,
}

impl GpuShares {
    /// Take one sample's client read, if it carries one, and drop held shares once stale at
    /// `now_ns`.
    ///
    /// Over the window between this read and the last: an entry in both contributes its delta, an
    /// entry absent from a complete last enumeration all its time (it was created inside the
    /// window), an entry present without a counter or missed by an incomplete read nothing
    /// (it may be old), and an entry that is gone nothing. The first read, or one more than
    /// [`WINDOW_MAX_NS`] after the last, is a baseline and gives no shares.
    pub(crate) fn absorb(&mut self, read: Option<&GpuClientsSample>, now_ns: u64) {
        if let Some(read) = read {
            let now: HashMap<u64, u64> = read.rows.iter().map(|r| (r.entry, r.gpu_ns)).collect();
            if let Some(then) = self.last.take()
                && let Some(window) = read
                    .at_ns
                    .checked_sub(then.at_ns)
                    .filter(|w| *w > 0 && *w <= WINDOW_MAX_NS)
            {
                let mut ran: HashMap<u32, (String, u64)> = HashMap::new();
                for r in &read.rows {
                    let d = match then.ns.get(&r.entry) {
                        Some(before) => r.gpu_ns.saturating_sub(*before),
                        None if then.complete && !then.entries.contains(&r.entry) => r.gpu_ns,
                        None => 0,
                    };
                    let e = ran.entry(r.pid).or_insert_with(|| (r.name.clone(), 0));
                    e.1 = e.1.saturating_add(d);
                }
                self.rates = ran
                    .into_iter()
                    .map(|(pid, (name, ns))| {
                        (pid, (name, (ns as f64 / window as f64).min(1.0) as f32))
                    })
                    .collect();
                self.at = Some(read.at_ns);
            }
            self.last = Some(Baseline {
                at_ns: read.at_ns,
                ns: now,
                entries: read.entries.iter().copied().collect(),
                complete: read.complete,
            });
        }
        if self
            .at
            .is_some_and(|t| now_ns.saturating_sub(t) > WINDOW_MAX_NS)
        {
            self.rates.clear();
            self.at = None;
        }
    }

    /// Whether shares are held.
    pub(crate) fn held(&self) -> bool {
        self.at.is_some()
    }

    /// The process `pid` named `name`'s share of the device: `None` while no shares are held,
    /// `0.0` for a process with no client, or whose name is not its clients' (the pid was reused).
    pub(crate) fn share(&self, pid: u32, name: &str) -> Option<f32> {
        self.held().then(|| {
            self.rates
                .get(&pid)
                .filter(|(n, _)| same_comm(n, name))
                .map_or(0.0, |(_, share)| *share)
        })
    }

    /// Every client's share, summed, while held.
    pub(crate) fn clients(&self) -> Option<f32> {
        self.held()
            .then(|| self.rates.values().map(|(_, s)| *s).sum())
    }
}

/// Whether two short names are one process's: equal in their first 16 bytes (`MAXCOMLEN`, where
/// macOS cuts `p_comm`).
fn same_comm(a: &str, b: &str) -> bool {
    fn cut(s: &str) -> &[u8] {
        &s.as_bytes()[..s.len().min(16)]
    }
    cut(a) == cut(b)
}

#[cfg(test)]
mod tests {
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
        g.absorb(Some(&read(10, &[(1, 7, "mtl", 5 * S)])), 10 * S);
        assert!(!g.held());
        assert_eq!(g.share(7, "mtl"), None);
        assert_eq!(g.clients(), None);
    }

    #[test]
    fn a_delta_over_the_window_is_its_share() {
        let mut g = GpuShares::default();
        g.absorb(Some(&read(10, &[(1, 7, "mtl", 5 * S)])), 10 * S);
        g.absorb(Some(&read(14, &[(1, 7, "mtl", 6 * S)])), 14 * S);
        assert!(close(g.share(7, "mtl"), 0.25), "{g:?}");
        assert!(close(g.clients(), 0.25));
    }

    #[test]
    fn a_client_born_inside_the_window_counts_all_its_time() {
        let mut g = GpuShares::default();
        g.absorb(Some(&read(10, &[(1, 7, "mtl", 5 * S)])), 10 * S);
        g.absorb(
            Some(&read(14, &[(1, 7, "mtl", 5 * S), (2, 8, "new", S)])),
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
        g.absorb(Some(&first), 10 * S);
        let next = read(
            14,
            &[
                (1, 7, "mtl", 6 * S),
                (2, 88, "WindowServer", 900 * S),
                (3, 9, "new", 2 * S),
            ],
        );
        g.absorb(Some(&next), 14 * S);
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
        g.absorb(Some(&cut), 10 * S);
        g.absorb(
            Some(&read(
                14,
                &[(1, 7, "mtl", 6 * S), (2, 88, "WindowServer", 900 * S)],
            )),
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
            10 * S,
        );
        g.absorb(Some(&read(14, &[(1, 7, "mtl", 7 * S)])), 14 * S);
        assert!(close(g.share(7, "mtl"), 0.5));
        assert!(close(g.share(8, "gone"), 0.0));
        assert!(close(g.clients(), 0.5));
        // A counter that went back reads 0, never a wrap.
        g.absorb(Some(&read(18, &[(1, 7, "mtl", S)])), 18 * S);
        assert!(close(g.share(7, "mtl"), 0.0));
    }

    #[test]
    fn two_clients_of_one_process_sum_and_clamp_at_the_whole_gpu() {
        let mut g = GpuShares::default();
        let zero = [(1, 7, "mtl", 0), (2, 7, "mtl", 0), (3, 9, "x", 0)];
        g.absorb(Some(&read(10, &zero)), 10 * S);
        let half = [(1, 7, "mtl", S), (2, 7, "mtl", S), (3, 9, "x", 3 * S)];
        g.absorb(Some(&read(14, &half)), 14 * S);
        assert!(close(g.share(7, "mtl"), 0.5), "{g:?}");
        assert!(close(g.share(9, "x"), 0.75));
        let over = [
            (1, 7, "mtl", 4 * S),
            (2, 7, "mtl", 4 * S),
            (3, 9, "x", 3 * S),
        ];
        g.absorb(Some(&read(18, &over)), 18 * S);
        assert!(close(g.share(7, "mtl"), 1.0), "{g:?}");
        assert!(close(g.clients(), 1.0));
    }

    #[test]
    fn a_name_that_is_not_the_clients_takes_no_gpu_time() {
        let mut g = GpuShares::default();
        g.absorb(Some(&read(10, &[(1, 7, "mtl", 0)])), 10 * S);
        g.absorb(Some(&read(14, &[(1, 7, "mtl", 2 * S)])), 14 * S);
        assert!(close(g.share(7, "zsh"), 0.0));
        assert!(close(g.share(7, "mtl"), 0.5));
        assert!(close(g.share(99, "mtl"), 0.0));
        let long = "com.apple.WebKit.GPU";
        g.absorb(Some(&read(18, &[(1, 7, &long[..16], 4 * S)])), 18 * S);
        assert!(close(g.share(7, long), 0.5), "a cut p_comm is its name");
    }

    #[test]
    fn held_shares_expire_and_a_stale_baseline_gives_none() {
        let mut g = GpuShares::default();
        g.absorb(Some(&read(10, &[(1, 7, "mtl", 0)])), 10 * S);
        g.absorb(Some(&read(14, &[(1, 7, "mtl", 2 * S)])), 14 * S);
        let max_s = WINDOW_MAX_NS / S;
        g.absorb(None, (14 + max_s) * S);
        assert!(g.held(), "held through the window");
        g.absorb(None, (15 + max_s) * S);
        assert!(!g.held());
        assert_eq!(g.share(7, "mtl"), None);
        g.absorb(
            Some(&read(16 + 2 * max_s, &[(1, 7, "mtl", 9 * S)])),
            (16 + 2 * max_s) * S,
        );
        assert!(
            !g.held(),
            "a read past the window after the last is a baseline"
        );
    }
}
