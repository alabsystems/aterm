// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright 2026 Andrew Yates

use std::num::NonZeroU64;
use std::sync::Arc;
use std::time::Instant;

use crate::{Monitor, Pool, SlotState};

/// Connect cached live monitoring to the shared pool's admission veto.
///
/// This does not mint CPU capacity or turn observations into reservations.
/// Every admitted worker must still acquire its own actual pool lease.
pub struct HostPolicy {
    pool: Pool,
    monitor: Option<Arc<Monitor>>,
    per_worker: NonZeroU64,
    reserve: u64,
    fallback_memory: Arc<dyn Fn() -> Option<u64> + Send + Sync>,
    native_cpu: Arc<dyn Fn() -> u64 + Send + Sync>,
}

impl HostPolicy {
    /// Use a monitor when available. Before any successful observation, a
    /// kernel CPU/memory reading may permit a bounded global worker budget on
    /// machines without aterm. A previously working monitor that becomes
    /// stale/unavailable pauses new work until fresh observations recover.
    pub fn new(
        pool: Pool,
        monitor: Option<Arc<Monitor>>,
        per_worker: NonZeroU64,
        reserve: u64,
        fallback_memory: Arc<dyn Fn() -> Option<u64> + Send + Sync>,
    ) -> Self {
        Self { pool, monitor, per_worker, reserve, fallback_memory, native_cpu: Arc::new(|| 0) }
    }

    /// Supply validated process CPU measurements. An absent/unknown owner
    /// contributes zero, never an invented busy core from a held lease count.
    pub fn with_native_cpu(mut self, source: Arc<dyn Fn() -> u64 + Send + Sync>) -> Self {
        self.native_cpu = source;
        self
    }

    /// The next acquisition's current observed limit; errors pause admission.
    pub fn limit(&self) -> u32 {
        let Ok(states) = self.pool.snapshot() else { return 0 };
        let held =
            states.iter().filter(|state| matches!(state, SlotState::Held { .. })).count() as u32;
        self.limit_for(held)
    }

    /// Use the count from `Pool::try_acquire_observed` inside its transaction.
    pub fn limit_for(&self, held: u32) -> u32 {
        self.limit_at(Instant::now(), held)
    }

    fn limit_at(&self, now: Instant, _held: u32) -> u32 {
        if let Some(monitor) = &self.monitor {
            if let Some((observation, claimable)) = monitor.latest() {
                return observation
                    .admission_limit(
                        now,
                        (self.native_cpu)(),
                        claimable,
                        self.per_worker,
                        self.reserve,
                    )
                    .min(self.pool.slots().get());
            }
            if monitor.ever_observed() {
                return 0;
            }
        }
        (self.fallback_memory)().map_or(0, |available| {
            (available.saturating_sub(self.reserve) / self.per_worker.get())
                .min(u64::from(self.pool.slots().get())) as u32
        })
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use super::*;

    #[test]
    fn absent_monitor_uses_measured_kernel_budget_without_inventing_monitor_freshness() {
        let _fixture = crate::tests::kernel_fixture();
        let dir = tempfile::tempdir().unwrap();
        let pool = Pool::open_in(dir.path(), NonZeroU32::new(8).unwrap()).unwrap();
        let policy = HostPolicy::new(
            pool.clone(),
            None,
            NonZeroU64::new(1000).unwrap(),
            200,
            Arc::new(|| Some(5000)),
        );
        assert_eq!(policy.limit(), 4);
        let unknown =
            HostPolicy::new(pool, None, NonZeroU64::new(1000).unwrap(), 200, Arc::new(|| None));
        assert_eq!(unknown.limit(), 0);
    }
}
