// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The account application's root maintenance runs in the existing secrets
//! approver, using its current opened authority, never a catalog-writer role.

use aterm_db::accounts::recovery::Queue;
use std::sync::{Mutex, OnceLock, TryLockError};

static QUEUE: OnceLock<Mutex<Queue>> = OnceLock::new();
static NATIVE_FIRST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(super) fn install() {
    aterm_secrets::client::on_account_upkeep(|paths, admin, deadline| {
        let queue = QUEUE.get_or_init(|| Mutex::new(Queue::default()));
        let mut queue = match queue.try_lock() {
            Ok(queue) => queue,
            Err(TryLockError::Poisoned(error)) => error.into_inner(),
            Err(TryLockError::WouldBlock) => return Ok(0),
        };
        let (recovery, native) =
            if NATIVE_FIRST.fetch_xor(true, std::sync::atomic::Ordering::Relaxed) {
                let native = super::native::root_upkeep(paths, admin, deadline);
                (queue.step(paths, admin, deadline), native)
            } else {
                let recovery = queue.step(paths, admin, deadline);
                (recovery, super::native::root_upkeep(paths, admin, deadline))
            };
        match (recovery, native) {
            (Ok(recovery), Ok(native)) => Ok(recovery + native),
            (Err(error), _) => Err(aterm_secrets::Error::new(error.to_string())),
            (_, Err(error)) => Err(aterm_secrets::Error::new(error)),
        }
    });
}
