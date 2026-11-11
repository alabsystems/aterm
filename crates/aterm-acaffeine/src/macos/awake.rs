// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `NSProcessInfo` activity wrapper that disables idle system and display sleep.

use aterm_objc::send::{send_id, send_id_usize_id, send_v_id};
use aterm_objc::{Id, Obj, autoreleasepool, class, sel};

/// What the power manager names the activity: the mug's, and a bridge's across an update
/// restart ([`mod@super::bridge`]).
pub(super) const AWAKE_REASON: &str = crate::held::AWAKE_REASON;
const IDLE_DISPLAY_SLEEP_DISABLED: usize = 1 << 40;
const USER_INITIATED: usize = 0x00FF_FFFF;
const OPTIONS: usize = IDLE_DISPLAY_SLEEP_DISABLED | USER_INITIATED;

/// A held keep-awake activity; dropping it ends the activity.
pub(super) struct Awake {
    /// The activity token `-beginActivityWithOptions:reason:` answered, retained.
    token: Obj,
}

impl Awake {
    /// Begin keeping the Mac awake, `reason` being what `pmset -g assertions` names it by.
    pub(super) fn begin(reason: &str) -> Option<Self> {
        let reason = aterm_objc::ns_string(reason)?;
        autoreleasepool(|_| {
            // SAFETY: `+processInfo` is `@16@0:8`, a shared instance;
            // `-beginActivityWithOptions:reason:` is `@32@0:8Q16@24` and answers the
            // activity token at +0, retained here (the activity lasts while it lives and
            // until `-endActivity:`). `reason` is a live NSString the call copies.
            unsafe {
                let info = process_info();
                if info.is_null() {
                    return None;
                }
                let token = send_id_usize_id(
                    info,
                    sel!(beginActivityWithOptions:reason:),
                    OPTIONS,
                    reason.id(),
                );
                Obj::retain(token).map(|token| Self { token })
            }
        })
    }
}

impl Drop for Awake {
    fn drop(&mut self) {
        autoreleasepool(|_| {
            // SAFETY: `-endActivity:` is `v24@0:8@16`, handed the token its own
            // `-beginActivityWithOptions:reason:` answered; the token is released after.
            unsafe {
                let info = process_info();
                if !info.is_null() {
                    send_v_id(info, sel!(endActivity:), self.token.id());
                }
            }
        });
    }
}

/// `[NSProcessInfo processInfo]`, +0.
///
/// # Safety
/// Inside an autorelease pool.
unsafe fn process_info() -> Id {
    // SAFETY: `+processInfo` is `@16@0:8` on a class Foundation always registers.
    unsafe { send_id(class(c"NSProcessInfo").as_id(), sel!(processInfo)) }
}

#[cfg(test)]
#[path = "awake_tests.rs"]
pub(super) mod tests;
