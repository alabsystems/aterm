// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

fn assert_send<T: Send>() {}

/// Runtime check that a type does not implement `Sync`.
///
/// Inherent methods take priority over trait methods in method resolution.
/// When `T: Sync`, the inherent `is_sync_check()` resolves → returns `true`.
/// When `T: !Sync`, only the trait fallback is available → returns `false`.
/// The trait must be in scope at the call site for the fallback to resolve.
mod sync_check {
    pub struct Probe<T>(pub std::marker::PhantomData<T>);

    impl<T: Sync> Probe<T> {
        pub fn is_sync_check(&self) -> bool {
            true
        }
    }

    pub trait NotSyncFallback {
        fn is_sync_check(&self) -> bool;
    }
    impl<T> NotSyncFallback for Probe<T> {
        fn is_sync_check(&self) -> bool {
            false
        }
    }
}

#[test]
fn scrollback_is_send_but_not_sync() {
    use sync_check::NotSyncFallback;

    // WarmTier's Cell/RefCell fields (failure counter, byte count, block cache)
    // are Send but not Sync: Scrollback moves between threads, and sharing it
    // takes external synchronization (Mutex).
    assert_send::<Scrollback>();

    // Verify !Sync via method resolution. If Scrollback were Sync, the
    // inherent method would resolve and return true, failing the assertion.
    let probe = sync_check::Probe::<Scrollback>(std::marker::PhantomData);
    assert!(
        !probe.is_sync_check(),
        "Scrollback must NOT be Sync — interior mutability (Cell, RefCell) \
         requires external synchronization for cross-thread sharing"
    );

    // Sanity: verify the probe returns true for a known Sync type.
    let sync_probe = sync_check::Probe::<i32>(std::marker::PhantomData);
    assert!(
        sync_probe.is_sync_check(),
        "i32 is Sync — probe should return true to validate the mechanism"
    );
}
