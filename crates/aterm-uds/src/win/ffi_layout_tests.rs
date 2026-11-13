// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{SystemHandleEntryEx, UnicodeString, WtsProcessInfoExW};
use std::mem::{offset_of, size_of};

#[test]
fn the_scan_structures_have_their_sdk_layouts() {
    assert_eq!(size_of::<WtsProcessInfoExW>(), 64);
    assert_eq!(offset_of!(WtsProcessInfoExW, ProcessId), 4);
    assert_eq!(offset_of!(WtsProcessInfoExW, pUserSid), 16);
    assert_eq!(offset_of!(WtsProcessInfoExW, UserTime), 48);
    assert_eq!(size_of::<SystemHandleEntryEx>(), 40);
    assert_eq!(offset_of!(SystemHandleEntryEx, UniqueProcessId), 8);
    assert_eq!(offset_of!(SystemHandleEntryEx, HandleValue), 16);
    assert_eq!(offset_of!(SystemHandleEntryEx, ObjectTypeIndex), 30);
    assert_eq!(size_of::<UnicodeString>(), 16);
    assert_eq!(offset_of!(UnicodeString, Buffer), 8);
}
