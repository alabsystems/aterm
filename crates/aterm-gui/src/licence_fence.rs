// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CONTROL SOCKET'S CURSOR-MOVE LICENCE FENCE, carried across threads
//! without a main-thread round trip.
//!
//! A driven write is a newer input boundary: a stamp a person's key armed must
//! not license the cursor move the write's output makes
//! (docs/design/EFFECTS-LICENSE-REDESIGN.md). The control thread records the
//! boundary in the session's [`LicenceFence`] before it writes: two atomic adds,
//! no wait, so a write to a background session, or one made while a dialog or a
//! stalled turn holds the main thread, is never refused for its fence. Each
//! window applies what it has not yet applied ([`owed`], through
//! `App::reconcile_licence`) before the licence is armed or judged: at the head
//! of a winit event and of the App input seam (before a press arms a stamp), at
//! the head of a controller batch (before `on_focus`), and in the cursor frame
//! (before a move is judged). Output a write causes reaches the frame only
//! through the PTY and the terminal lock, after the store, so the frame applies
//! the fence before it judges that output.
//!
//! A window is fenced for the session it presented when it last applied the
//! fence, as the main-thread hop fenced the windows presenting the session at
//! that moment. A session a window starts presenting is adopted at its counters
//! as they stand, at the window's first look after the switch. The switch
//! itself closed every licence term in the window (`App::sync_window`'s
//! `fx.front_replaced` curtains the glow and resets the trail), and every stamp
//! is armed behind a look, so until that look no stamp stands for a fence on
//! either session to close or to license. A fence the new session took before
//! the look closed the windows presenting it then, and a stamp armed in this
//! window since (the click that focused its pane) is a newer boundary than that
//! fence.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// One session's licence boundaries, counted: every fence, and the fences that
/// take the glow's hand off its row. Lives on [`crate::SessionCtx`].
#[derive(Debug, Default)]
pub(crate) struct LicenceFence {
    fences: AtomicU64,
    hand_offs: AtomicU64,
}

/// A reading of a [`LicenceFence`]: equal to an earlier reading exactly when no
/// fence has been recorded since.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LicenceEpoch {
    fences: u64,
    hand_offs: u64,
}

impl LicenceFence {
    /// Records one boundary. `keep_hand` is the control socket's fence
    /// (`control_input::control_fence`): a line-editing key or a paste closes
    /// the licence keeping the glow's hand on its row.
    pub(crate) fn fence(&self, keep_hand: bool) {
        // The hand-off before the fence, the fence's add a Release and
        // [`Self::read`]'s load of it an Acquire: a reader that sees this fence
        // sees whether it took the hand off.
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

/// What one window last applied: the session it presented then, that
/// session's fence, and the reading applied. Lives on the window's state.
#[derive(Debug)]
pub(crate) struct LicenceSeen {
    session: u64,
    fence: Arc<LicenceFence>,
    epoch: LicenceEpoch,
}

/// The fence a window owes, and its record brought up to date: `Some(keep_hand)`
/// when the session it presented at `seen` recorded a fence since, keeping the
/// hand only if none of those fences took it off. `front` is the session the
/// window presents now (`None`: native content in front, which keeps the
/// record); a different session is adopted at its counters as they stand,
/// through `fence_of`, which is asked only then. A window with no record owes
/// nothing: it has armed no stamp before its first look.
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

    /// The record a window keeps: a fence on the session it presents is owed
    /// once; the hand stays on its row only when no fence since took it off;
    /// a session it starts presenting is adopted, not fenced; and the session
    /// it presented last is still answered for after it turned away.
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
        // The first look adopts; nothing was armed before it.
        a.fence(false);
        assert_eq!(owed(&mut seen, Some(1), fence_of), None);
        // Negative control: nothing recorded since, nothing owed.
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
        // Native content in front keeps the record: a fence recorded meanwhile
        // is still owed.
        a.fence(true);
        assert_eq!(owed(&mut seen, None, fence_of), Some(true));
        // Turning to session 2 adopts its standing counters without a fence;
        // its later fences are owed, and session 1's no longer are.
        b.fence(false);
        assert_eq!(owed(&mut seen, Some(2), fence_of), None);
        b.fence(true);
        assert_eq!(owed(&mut seen, Some(2), fence_of), Some(true));
        a.fence(false);
        assert_eq!(owed(&mut seen, Some(2), fence_of), None);
        // A fence on the session a window presented at its last look is owed
        // at its next look, even when it has turned to another session since.
        let mut turned = None;
        assert_eq!(owed(&mut turned, Some(1), fence_of), None);
        a.fence(false);
        assert_eq!(owed(&mut turned, Some(2), fence_of), Some(false));
        // A session the pool no longer holds leaves no record to answer for.
        assert_eq!(owed(&mut turned, Some(9), fence_of), None);
        assert!(turned.is_none());
    }
}
