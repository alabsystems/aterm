// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::RefCell;

/// Where a hook runs: in [`super::Observation::check`], for each predecessor, or at the gate of
/// a held read ([`super::Preserving::of`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Point {
    /// The predecessor's custody is revalidated; this Mac's standing in its vault is not yet
    /// judged.
    BeforeDenial,
    /// The predecessor's vault does not deny this Mac; the check's access is not yet judged
    /// current.
    BeforeCurrent,
    /// The gate of a held read is asked and has read nothing yet. Asked by a read
    /// (`catalog::Directory::observe_displaced`), the ordinary observation of the table has
    /// refused and no recovery replica has answered, neither of them busy, and the held open
    /// has not begun.
    HeldGate,
}

/// A hook and the point it waits for.
type Hook = (Point, Box<dyn FnMut()>);

thread_local! {
    static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
}

/// Run `hook` once, when this thread next reaches `point`.
pub(crate) fn at(point: Point, hook: impl FnMut() + 'static) {
    HOOK.with(|slot| *slot.borrow_mut() = Some((point, Box::new(hook))));
}

pub(super) fn reached(point: Point) {
    let hook = HOOK.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.as_ref().is_some_and(|(at, _)| *at == point) {
            slot.take()
        } else {
            None
        }
    });
    if let Some((_, mut hook)) = hook {
        hook();
    }
}

thread_local! {
    /// The signer of the lab vault this test thread reads as the default ALab's
    /// ([`alab_is`]): a scratch vault is signed by a test key, never ALab's.
    static ALAB: std::cell::Cell<Option<[u8; 32]>> = const { std::cell::Cell::new(None) };
}

/// Read the vault `signer` pins as the default ALab's on this thread, so its succession enrolls
/// a successor by itself as ALab's does.
pub(crate) fn alab_is(signer: [u8; 32]) {
    ALAB.set(Some(signer));
}

pub(super) fn stands_in_for_alab(vault: &aterm_secrets::client::Config) -> bool {
    ALAB.get()
        .is_some_and(|signer| vault.signer == Some(signer))
}
