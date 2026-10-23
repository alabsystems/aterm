// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Login Items' say over the login plist: aterm's switch under Background App Activity
//! (System Settings ▸ General ▸ Login Items & Extensions), through ServiceManagement's
//! `SMAppService` (macOS 13 and later; before it there is no switch, and nothing blocks).
//! Measured on macOS 27.0: the switch off, launchd loads none of aterm's login plists at
//! login and the running mug's job ends; its status for the plist reads
//! `SMAppServiceStatusRequiresApproval` until the person turns the switch on, which needs no
//! password and starts the job at once. No root, and no WindowServer: the status is a
//! question to the background task daemon.

use std::path::Path;

use aterm_objc::send::{send_id_id, send_v};
use aterm_objc::{ClassPtr, Id, Sel, autoreleasepool, class, sel};

#[link(name = "ServiceManagement", kind = "framework")]
unsafe extern "C" {}

/// `SMAppServiceStatusRequiresApproval`: the switch is off.
const REQUIRES_APPROVAL: isize = 2;

/// `+[SMAppService statusForLegacyURL:]`, `q24@0:8@16`: no helper has this shape.
type StatusFn = unsafe extern "C-unwind" fn(ClassPtr, Sel, Id) -> isize;

/// `SMAppService`, null before macOS 13.
fn service() -> ClassPtr {
    class(c"SMAppService")
}

/// Whether the switch is off for the login plist `plist`, so no login loads it however often
/// it is written. `false` on a macOS without the switch, and for a path that is not UTF-8.
pub(super) fn blocks(plist: &Path) -> bool {
    let service = service();
    if service.is_null() {
        return false;
    }
    let Some(path) = plist.to_str().and_then(aterm_objc::ns_string) else {
        return false;
    };
    autoreleasepool(|_| {
        // SAFETY: `+fileURLWithPath:` is `@24@0:8@16`, answering an autoreleased NSURL for
        // the live NSString `path`; `+statusForLegacyURL:` is `StatusFn`'s prototype on a
        // class that is there (checked above), and reads the URL it is handed.
        unsafe {
            let url = send_id_id(class(c"NSURL").as_id(), sel!(fileURLWithPath:), path.id());
            if url.is_null() {
                return false;
            }
            let status: StatusFn = aterm_objc::checked_msg();
            status(service, sel!(statusForLegacyURL:), url) == REQUIRES_APPROVAL
        }
    })
}

/// Open System Settings at Login Items & Extensions, where the switch is. Nothing on a macOS
/// without it.
pub(super) fn open_settings() {
    let service = service();
    if service.is_null() {
        return;
    }
    // SAFETY: `+openSystemSettingsLoginItems` is `v16@0:8` on a class that is there.
    unsafe { send_v(service.as_id(), sel!(openSystemSettingsLoginItems)) }
}
