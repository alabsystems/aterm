// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The UNIFIED metrics service: the SINGLE place that samples every system + per-tab
//! figure, once per HUD tick, into one snapshot. The cell HUD, the GPU widget tray,
//! and the read-only `widgets` control verb all READ this one snapshot — so "the
//! widget reads from the introspection API and never collects its own metrics" is a
//! STRUCTURAL guarantee, not a convention.
//!
//! Honesty is type-enforced: every fallible figure is an [`Avail`], and figures that
//! cannot be obtained on this platform (per-tab GPU) have NO field at all — the
//! renderer literally cannot fabricate them. Raw OS probes live in [`crate::sysmetrics`]
//! (the one unsafe-FFI seam); this module only diffs counters and composes the snapshot.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant};

/// A poll-driven figure older than this is reported as `n/a` (the probe stalled).
/// Applied to the background GPU slow-probe sample so a parked worker decays to `n/a`.
pub(crate) const STALE: Duration = Duration::from_secs(5);

/// Availability of a measured figure. `BestEffort` marks an approximate/unverified
/// reading (GPU utilization from an undocumented registry key) so the UI can flag it;
/// the `widgets` verb renders it as `approx:<v>`.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Avail<T> {
    Ok(T),
    BestEffort(T),
    Unavailable,
}

impl<T: Copy> Avail<T> {
    /// The value if present (Ok or BestEffort), else `None`.
    pub(crate) fn value(self) -> Option<T> {
        match self {
            Avail::Ok(v) | Avail::BestEffort(v) => Some(v),
            Avail::Unavailable => None,
        }
    }
    /// True when the value is present but only approximate. Exercised by the
    /// metrics-honesty conformance test (`gpu_system` must be BestEffort); the
    /// `widgets` verb distinguishes the states by matching `Avail` directly.
    #[allow(dead_code)]
    pub(crate) fn is_approx(self) -> bool {
        matches!(self, Avail::BestEffort(_))
    }
    /// Build from an `Option`, treating `Some` as exact.
    fn from_opt(o: Option<T>) -> Self {
        o.map_or(Avail::Unavailable, Avail::Ok)
    }
}

/// Internet/link reachability summary for the network widget, produced by
/// [`crate::sysmetrics::net_health`] (SCNetworkReachability of the default route + a
/// baud-threshold "slow" heuristic on macOS; an honest link heuristic elsewhere).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum NetHealth {
    Offline,
    Online,
    Slow,
    Unknown,
}

/// One coherent sample of everything the system-performance widget needs. All system
/// figures are whole-machine; `tab_*` are THIS tab's shell job tree. Per-tab GPU is
/// deliberately absent (unobtainable on macOS) so it can never be faked.
#[derive(Clone)]
pub(crate) struct MetricsSnapshot {
    /// When this snapshot was sampled — the [`MetricsSnapshot::staled`] freshness gate
    /// reverts poll-driven fields to `n/a` once this is older than [`STALE`].
    pub at: Instant,
    // --- system capacity + usage ---
    pub cpu_cores: u32,
    pub cpu_system: Avail<f64>, // 0..1 of all cores
    pub mem_total: u64,
    pub mem_system: Avail<f64>, // 0..1
    pub gpu_system: Avail<f64>, // 0..1 (best-effort)
    pub gpu_vram_budget: Avail<u64>,
    pub gpu_vram_used: Avail<u64>,
    // --- this tab ---
    pub tab_cpu: Avail<f64>, // 0..1 of all cores
    pub tab_rss: Avail<u64>, // physical footprint (name kept for the aterm-ctl JSON schema; see MEM-ACCT-1)
    // (no tab GPU field — unobtainable on macOS, by design)
    // --- aterm-gui's OWN process (where the GPU/render memory + any heap leak live) ---
    // Sampled from OUR pid, not the shell subtree — `tab_rss` never covers aterm-gui, so
    // a socket reader otherwise sees ZERO of the process that actually holds the atlas /
    // vibrancy backdrops. macOS footprint (`ri_phys_footprint`), i.e. the ledger figure.
    pub mem_self: Avail<u64>,
    // --- disk + network ---
    pub disk_free: Avail<u64>,
    pub disk_total: Avail<u64>,
    pub net_rx_bps: Avail<f64>,
    pub net_tx_bps: Avail<f64>,
    pub net_link_baud: Avail<u64>,
    pub net_health: NetHealth,
}

impl MetricsSnapshot {
    /// Freshness gate (Stage B): once this snapshot is older than [`STALE`], every
    /// poll-driven figure reverts to `n/a` so a stalled/parked sampler never renders its
    /// last value as live. Static capacities (core count, RAM/disk totals, nominal link
    /// baud) are kept — they do not go stale. Pure; applied at the read seam
    /// ([`global_snapshot`]).
    pub(crate) fn staled(mut self, now: Instant) -> Self {
        if now.saturating_duration_since(self.at) > STALE {
            self.cpu_system = Avail::Unavailable;
            self.mem_system = Avail::Unavailable;
            self.gpu_system = Avail::Unavailable;
            self.gpu_vram_used = Avail::Unavailable;
            self.gpu_vram_budget = Avail::Unavailable;
            self.tab_cpu = Avail::Unavailable;
            self.tab_rss = Avail::Unavailable;
            self.net_rx_bps = Avail::Unavailable;
            self.net_tx_bps = Avail::Unavailable;
            self.net_health = NetHealth::Unknown;
        }
        self
    }

    fn empty(now: Instant) -> Self {
        Self {
            at: now,
            cpu_cores: crate::sysmetrics::ncpu(),
            cpu_system: Avail::Unavailable,
            mem_total: crate::sysmetrics::mem_total().unwrap_or(0),
            mem_system: Avail::Unavailable,
            gpu_system: Avail::Unavailable,
            gpu_vram_budget: Avail::Unavailable,
            gpu_vram_used: Avail::Unavailable,
            tab_cpu: Avail::Unavailable,
            tab_rss: Avail::Unavailable,
            mem_self: Avail::Unavailable,
            disk_free: Avail::Unavailable,
            disk_total: Avail::Unavailable,
            net_rx_bps: Avail::Unavailable,
            net_tx_bps: Avail::Unavailable,
            net_link_baud: Avail::Unavailable,
            net_health: NetHealth::Unknown,
        }
    }
}

/// Per-interface byte-counter delta robust to a 32-bit wrap vs a counter reset (a
/// reset, a large backwards jump, contributes 0 rather than a multi-GB phantom).
fn iface_delta(new: u32, prev: u32) -> u64 {
    let d = new.wrapping_sub(prev);
    if d > u32::MAX / 2 { 0 } else { u64::from(d) }
}

/// Map a cached background slow-probe sample to the whole-machine GPU triple
/// `(util, vram_used, vram_budget)`, honoring the [`STALE`] TTL on the WORKER timestamp:
/// a sample older than the TTL (a parked/stalled worker) decays every figure to
/// `Unavailable` rather than rendering a carried-over value. GPU utilization comes from
/// an UNDOCUMENTED registry key, so it is `BestEffort` (approx); VRAM used/budget are
/// exact system figures (`Ok`). A field the probe didn't carry stays `Unavailable`.
fn gpu_avail(
    sample: Option<&crate::sysmetrics::SlowSample>,
    now: Instant,
) -> (Avail<f64>, Avail<u64>, Avail<u64>) {
    match sample.filter(|s| now.saturating_duration_since(s.at) <= STALE) {
        Some(s) => (
            s.gpu.map_or(Avail::Unavailable, Avail::BestEffort),
            s.vram_used.map_or(Avail::Unavailable, Avail::Ok),
            s.vram_budget.map_or(Avail::Unavailable, Avail::Ok),
        ),
        None => (Avail::Unavailable, Avail::Unavailable, Avail::Unavailable),
    }
}

/// The process-global handle the control thread reads (`widgets` verb) — published by
/// the service at construction so the read path never needs `App`.
static GLOBAL: OnceLock<Arc<RwLock<MetricsSnapshot>>> = OnceLock::new();

/// A clone of the latest snapshot for the read-only control verb, or `None` before
/// the service has been constructed. Freshness-gated ([`MetricsSnapshot::staled`]) at
/// the read seam: a stalled/parked sampler decays to honest `n/a`, never a stale value.
pub(crate) fn global_snapshot() -> Option<MetricsSnapshot> {
    let now = Instant::now();
    // The param is named `snap` deliberately: GLOBAL holds a clone of the
    // service's `snap` Arc (same RwLock instance), so the lock-order census
    // (OB-7) unifies this read with the `snap` identity instead of splitting
    // one lock across two names.
    GLOBAL.get().map(|snap| {
        snap.read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .staled(now)
    })
}

/// The single sampler. Owned by `App`; sampled once per HUD tick before the panels so
/// every reader sees one coherent snapshot.
pub(crate) struct MetricsService {
    snap: Arc<RwLock<MetricsSnapshot>>,
    prev_cpu: Option<(u64, u64)>,
    prev_net: HashMap<String, (u32, u32)>,
    prev_net_at: Option<Instant>,
    /// (pid, cumulative cpu_ns, sampled_at) for the per-tab CPU delta.
    prev_tab: Option<(i32, u64, Instant)>,
}

impl MetricsService {
    pub(crate) fn new(now: Instant) -> Self {
        let snap = Arc::new(RwLock::new(MetricsSnapshot::empty(now)));
        // Publish the handle for the control thread; first writer wins (single App).
        let _ = GLOBAL.set(snap.clone());
        Self {
            snap,
            prev_cpu: None,
            prev_net: HashMap::new(),
            prev_net_at: None,
            prev_tab: None,
        }
    }

    /// A clone of the latest snapshot (for the in-process tray + cell HUD readers,
    /// wired in Stage B/C; the control verb uses [`global_snapshot`]).
    #[allow(dead_code)]
    pub(crate) fn snapshot(&self) -> MetricsSnapshot {
        self.snap
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Sample everything once. `tab_pid` is the frontmost tab's PTY-child PID (its job
    /// tree is measured); `cwd` is that tab's working dir for the disk figure (the
    /// system root if unknown).
    pub(crate) fn sample(&mut self, tab_pid: Option<i32>, cwd: Option<&str>, now: Instant) {
        let cores = crate::sysmetrics::ncpu();

        // CPU%: Δbusy/Δtotal between two tick samples (first tick has no delta).
        // `sysmetrics::cpu_ticks()` returns aggregate `[user, system, idle, nice]`;
        // fold to `(busy, total)` (busy = user+system+nice; total adds idle) so the
        // existing delta math and `prev_cpu` stay `(u64, u64)`.
        let cur_cpu = crate::sysmetrics::cpu_ticks()
            .map(|[user, system, idle, nice]| (user + system + nice, user + system + idle + nice));
        let cpu_system = match (self.prev_cpu, cur_cpu) {
            (Some((pb, pt)), Some((b, t))) if t > pt => {
                Avail::Ok((((b - pb) as f64) / ((t - pt) as f64)).clamp(0.0, 1.0))
            }
            _ => Avail::Unavailable,
        };
        if cur_cpu.is_some() {
            self.prev_cpu = cur_cpu;
        }

        // Memory.
        let mem_total = crate::sysmetrics::mem_total().unwrap_or(0);
        let mem_system = Avail::from_opt(crate::sysmetrics::mem_used_frac());
        // aterm-gui's OWN footprint (our pid) — the process the shell-subtree `tab_rss`
        // never covers, yet where the GPU atlas / vibrancy backdrops / any heap leak live.
        let mem_self = Avail::from_opt(
            crate::sysmetrics::proc_usage(crate::sysmetrics::self_pid()).map(|s| s.footprint),
        );

        // Per-tab CPU%/footprint over the shell job tree (cumulative cpu_ns diffed over wall).
        let (mut tab_cpu, mut tab_rss) = (Avail::Unavailable, Avail::Unavailable);
        if let Some(pid) = tab_pid
            && let Some((cpu_ns, rss)) = crate::sysmetrics::proc_tree_cpu_rss(pid)
        {
            tab_rss = Avail::Ok(rss);
            if let Some((ppid, pcpu, pat)) = self.prev_tab
                && ppid == pid
            {
                let dt = now
                    .checked_duration_since(pat)
                    .map_or(0.0, |d| d.as_secs_f64());
                if dt > 0.0 && cpu_ns >= pcpu && cores > 0 {
                    let busy_s = (cpu_ns - pcpu) as f64 / 1.0e9;
                    tab_cpu = Avail::Ok((busy_s / (dt * f64::from(cores))).clamp(0.0, 1.0));
                }
            }
            self.prev_tab = Some((pid, cpu_ns, now));
        } else {
            self.prev_tab = None;
        }

        // Disk for the tab's cwd (fall back to the system root).
        let (disk_free, disk_total) = match crate::sysmetrics::disk_for(cwd.unwrap_or("/")) {
            Some((f, t)) => (Avail::Ok(f), Avail::Ok(t)),
            None => (Avail::Unavailable, Avail::Unavailable),
        };

        // Network throughput per interface, summed; nominal link speed; reachability.
        let (mut rx_bps, mut tx_bps) = (Avail::Unavailable, Avail::Unavailable);
        let ifaces = crate::sysmetrics::net_ifaces();
        if let Some(list) = &ifaces {
            let cur: HashMap<String, (u32, u32)> =
                list.iter().map(|(n, r, t)| (n.clone(), (*r, *t))).collect();
            if let Some(pt) = self.prev_net_at {
                let dt = now
                    .checked_duration_since(pt)
                    .map_or(0.0, |d| d.as_secs_f64());
                if dt > 0.0 {
                    let (mut drx, mut dtx) = (0u64, 0u64);
                    for (name, &(r, t)) in &cur {
                        if let Some(&(pr, ptx)) = self.prev_net.get(name) {
                            drx += iface_delta(r, pr);
                            dtx += iface_delta(t, ptx);
                        }
                    }
                    rx_bps = Avail::Ok(drx as f64 / dt);
                    tx_bps = Avail::Ok(dtx as f64 / dt);
                }
            }
            self.prev_net = cur;
            self.prev_net_at = Some(now);
        }
        // Whole-machine GPU utilization + VRAM (used, budget) from the BACKGROUND
        // slow-probe worker (IOKit registry walks + Metal device creation are multi-
        // millisecond and must NEVER run on this event-loop thread). Read the already-
        // cached sample lock-briefly and apply the same TTL staleness filter the HUD
        // uses — a parked/stalled worker decays to honest `n/a`, never a carried-over
        // figure. Util is from an undocumented key (BestEffort); VRAM is exact.
        // Per-tab GPU stays absent: macOS has no public per-process GPU counter.
        let slow = crate::sysmetrics::slow_probes_latest();
        let (gpu_system, gpu_vram_used, gpu_vram_budget) = gpu_avail(slow.as_ref(), now);

        let net_link_baud = Avail::from_opt(crate::sysmetrics::net_primary_baud());
        // Reachability: SCNetworkReachability of the default route (a fast synchronous
        // LOCAL probe) folded with link presence + a baud "slow" threshold — Online /
        // Slow / Offline / Unknown, never a state we can't prove.
        let net_health = crate::sysmetrics::net_health(&ifaces, net_link_baud.value());

        let snap = MetricsSnapshot {
            at: now,
            cpu_cores: cores,
            cpu_system,
            mem_total,
            mem_system,
            gpu_system,
            gpu_vram_budget,
            gpu_vram_used,
            tab_cpu,
            tab_rss,
            mem_self,
            disk_free,
            disk_total,
            net_rx_bps: rx_bps,
            net_tx_bps: tx_bps,
            net_link_baud,
            net_health,
        };
        *self
            .snap
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = snap;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Build a slow-probe sample carrying the given GPU figures, stamped `at`.
    fn slow_sample(
        gpu: Option<f64>,
        vram_used: Option<u64>,
        vram_budget: Option<u64>,
        at: Instant,
    ) -> crate::sysmetrics::SlowSample {
        crate::sysmetrics::SlowSample {
            gpu,
            disk: None,
            procs: None,
            vram_used,
            vram_budget,
            at,
        }
    }

    #[test]
    fn iface_delta_handles_wrap_and_reset() {
        assert_eq!(iface_delta(1000, 400), 600);
        assert_eq!(iface_delta(100, u32::MAX - 99), 200); // wrap
        assert_eq!(iface_delta(0, 1000), 0); // reset
    }

    #[test]
    fn gpu_avail_surfaces_bestfffort_and_vram_then_decays_stale() {
        let now = Instant::now();
        // A fresh sample surfaces util as BestEffort (undocumented key) + VRAM exact.
        let fresh = slow_sample(Some(0.42), Some(1_000), Some(4_000), now);
        let (g, used, budget) = gpu_avail(Some(&fresh), now);
        assert_eq!(g.value(), Some(0.42));
        assert!(
            g.is_approx(),
            "GPU util is from an undocumented key -> BestEffort"
        );
        assert_eq!(used.value(), Some(1_000));
        assert_eq!(budget.value(), Some(4_000));
        assert!(
            !matches!(used, Avail::BestEffort(_)),
            "VRAM used is exact (Ok)"
        );
        // A sample older than the TTL decays every figure (parked/stalled worker).
        let old = slow_sample(
            Some(0.42),
            Some(1_000),
            Some(4_000),
            now - STALE - Duration::from_secs(1),
        );
        let (g, used, budget) = gpu_avail(Some(&old), now);
        assert!(g.value().is_none() && used.value().is_none() && budget.value().is_none());
        // A fresh sample carrying no figures (locked-down runner) stays Unavailable.
        let empty = slow_sample(None, None, None, now);
        let (g, used, budget) = gpu_avail(Some(&empty), now);
        assert!(g.value().is_none() && used.value().is_none() && budget.value().is_none());
        // No sample at all (worker never armed) is Unavailable.
        let (g, ..) = gpu_avail(None, now);
        assert!(g.value().is_none());
    }

    #[test]
    fn stale_snapshot_reverts_poll_fields_to_na() {
        let base = Instant::now();
        let mut snap = MetricsSnapshot::empty(base);
        snap.cpu_system = Avail::Ok(0.5);
        snap.gpu_system = Avail::BestEffort(0.3);
        snap.gpu_vram_used = Avail::Ok(1_000);
        snap.net_health = NetHealth::Online;
        let (cores, mem_total) = (snap.cpu_cores, snap.mem_total);
        // Within the TTL: every value is kept.
        let fresh = snap.clone().staled(base + STALE);
        assert_eq!(fresh.cpu_system.value(), Some(0.5));
        assert!(fresh.gpu_system.is_approx());
        assert_eq!(fresh.net_health, NetHealth::Online);
        // Past the TTL: poll-driven fields revert to n/a; static capacities survive.
        let stale = snap.staled(base + STALE + Duration::from_secs(1));
        assert!(stale.cpu_system.value().is_none());
        assert!(stale.gpu_system.value().is_none());
        assert!(stale.gpu_vram_used.value().is_none());
        assert_eq!(stale.net_health, NetHealth::Unknown);
        assert_eq!(
            stale.cpu_cores, cores,
            "core count is a static capacity, kept"
        );
        assert_eq!(
            stale.mem_total, mem_total,
            "RAM total is a static capacity, kept"
        );
    }

    /// Tier-1 conformance: the SHIPPING [`MetricsSnapshot::staled`] freshness gate tracks
    /// the `Freshness` ty model (`aterm_spec::derive::freshness_model`), which the real
    /// Trust `ty` proves at `Buggy=0` and catches at `Buggy=1` in aterm-spec's
    /// `derived_ring_ty`. A figure presented as live is never older than [`STALE`]; the
    /// negative control (a no-revert gate keeping a stale value) violates the invariant.
    #[test]
    fn staleness_gate_matches_freshness_model() {
        use aterm_spec::derive::freshness_model;
        let m = freshness_model();
        let stale_secs = STALE.as_secs();
        let base = Instant::now();
        let mut fresh = MetricsSnapshot::empty(base);
        fresh.cpu_system = Avail::Ok(0.5);

        // Drive the model from Init (age=0, live=1) one Tick per elapsed second, in
        // lockstep with the real gate over the same wall-clock age.
        let mut st = m.init_state();
        for age in 0..=stale_secs + 1 {
            let live_real = i64::from(
                fresh
                    .clone()
                    .staled(base + Duration::from_secs(age))
                    .cpu_system
                    .value()
                    .is_some(),
            );
            assert_eq!(st["age"], age as i64, "model age tracks wall-clock age");
            assert_eq!(
                st["live"], live_real,
                "gate liveness matches the model at age {age}"
            );
            assert!(
                m.check_invariant("FreshWhenLive", &st),
                "FreshWhenLive holds at age {age}"
            );
            if age > stale_secs {
                assert_eq!(live_real, 0, "a figure older than STALE reverts to n/a");
            } else {
                assert_eq!(live_real, 1, "a within-TTL figure stays live");
            }
            m.fire("Tick", &mut st);
        }

        // NON-VACUOUS negative control: a no-revert gate keeps the figure live past
        // STALE — exactly the model's Buggy mutant — and that state violates the model.
        let buggy = BTreeMap::from([("age", stale_secs as i64 + 1), ("live", 1)]);
        assert!(
            !m.check_invariant("FreshWhenLive", &buggy),
            "a stale-but-live figure must be caught by the model"
        );
    }

    #[test]
    fn sample_publishes_a_global_snapshot() {
        let t0 = Instant::now();
        let mut svc = MetricsService::new(t0);
        svc.sample(None, Some("/"), t0);
        // second tick yields a CPU delta on a real OS
        svc.sample(None, Some("/"), t0 + Duration::from_millis(300));
        let g = global_snapshot().expect("global published");
        assert!(g.cpu_cores >= 1, "core count is known");
        // tab metrics are Unavailable with no pid (honesty, not fabrication)
        assert!(g.tab_cpu.value().is_none());
    }
}
