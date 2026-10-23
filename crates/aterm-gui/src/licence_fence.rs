// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Control-socket cursor-move licence fence carried across threads ([`LicenceFence`], [`owed`]).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// One session's licence boundaries, counted ([`crate::SessionCtx`]).
#[derive(Debug, Default)]
pub(crate) struct LicenceFence {
    fences: AtomicU64,
    hand_offs: AtomicU64,
}

/// A reading of a [`LicenceFence`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LicenceEpoch {
    fences: u64,
    hand_offs: u64,
}

impl LicenceFence {
    /// Records one boundary.
    pub(crate) fn fence(&self, keep_hand: bool) {
        if !keep_hand {
            self.hand_offs.fetch_add(1, Ordering::Release);
        }
        self.fences.fetch_add(1, Ordering::Release);
    }

    /// The counters now.
    pub(crate) fn read(&self) -> LicenceEpoch {
        let fences = self.fences.load(Ordering::Acquire);
        let hand_offs = self.hand_offs.load(Ordering::Acquire);
        LicenceEpoch { fences, hand_offs }
    }
}

/// What one window last applied.
#[derive(Debug)]
pub(crate) struct LicenceSeen {
    session: u64,
    fence: Arc<LicenceFence>,
    epoch: LicenceEpoch,
}

/// The fence a window owes, and its record brought up to date.
pub(crate) fn owed(
    seen: &mut Option<LicenceSeen>,
    front: Option<u64>,
    fence_of: impl FnOnce(u64) -> Option<Arc<LicenceFence>>,
) -> Option<bool> {
    let owed = seen.as_mut().and_then(|seen| {
        let now = seen.fence.read();
        let owed = (now != seen.epoch).then_some(now.hand_offs == seen.epoch.hand_offs);
        seen.epoch = now;
        owed
    });
    if let Some(front) = front
        && seen.as_ref().is_none_or(|seen| seen.session != front)
    {
        *seen = fence_of(front).map(|fence| LicenceSeen {
            session: front,
            epoch: fence.read(),
            fence,
        });
    }
    owed
}

#[cfg(test)]
mod tests {
    use super::{LicenceFence, owed};
    use std::sync::Arc;

    #[test]
    fn a_window_owes_each_fence_of_the_session_it_presented_once() {
        let a = Arc::new(LicenceFence::default());
        let b = Arc::new(LicenceFence::default());
        let fence_of = |session| match session {
            1 => Some(a.clone()),
            2 => Some(b.clone()),
            _ => None,
        };
        let mut seen = None;
        a.fence(false);
        assert_eq!(owed(&mut seen, Some(1), fence_of), None);
        assert_eq!(owed(&mut seen, Some(1), fence_of), None);
        a.fence(true);
        assert_eq!(owed(&mut seen, Some(1), fence_of), Some(true));
        assert_eq!(
            owed(&mut seen, Some(1), fence_of),
            None,
            "a fence is owed once"
        );
        a.fence(true);
        a.fence(false);
        a.fence(true);
        assert_eq!(
            owed(&mut seen, Some(1), fence_of),
            Some(false),
            "one hand-off among the fences takes the hand off"
        );
        a.fence(true);
        assert_eq!(owed(&mut seen, None, fence_of), Some(true));
        b.fence(false);
        assert_eq!(owed(&mut seen, Some(2), fence_of), None);
        b.fence(true);
        assert_eq!(owed(&mut seen, Some(2), fence_of), Some(true));
        a.fence(false);
        assert_eq!(owed(&mut seen, Some(2), fence_of), None);
        let mut turned = None;
        assert_eq!(owed(&mut turned, Some(1), fence_of), None);
        a.fence(false);
        assert_eq!(owed(&mut turned, Some(2), fence_of), Some(false));
        assert_eq!(owed(&mut turned, Some(9), fence_of), None);
        assert!(turned.is_none());
    }
}
