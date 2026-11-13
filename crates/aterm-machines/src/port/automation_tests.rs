// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

type DataSize = unsafe extern "C" fn(*const AeDesc) -> isize;
type Data = unsafe extern "C" fn(*const AeDesc, *mut c_void, isize) -> i16;

/// The descriptor `AECreateDesc` fills reads back the bundle id it was given, through
/// `AEGetDescDataSize` and `AEGetDescData`: the packed layout is the framework's. Nothing
/// here reads a privacy answer.
#[test]
fn the_system_events_address_reads_back_through_the_apple_event_manager() {
    let services = open(CORE_SERVICES).unwrap();
    assert!(symbol(services, c"AEDeterminePermissionToAutomateTarget").is_some());
    let target = Target::new(services).unwrap();
    // SAFETY: CoreServices' `Size AEGetDescDataSize(const AEDesc *)` and `OSErr
    // AEGetDescData(const AEDesc *, void *, Size)`.
    let (size, data) = unsafe {
        (
            std::mem::transmute::<*mut c_void, DataSize>(
                symbol(services, c"AEGetDescDataSize").unwrap(),
            ),
            std::mem::transmute::<*mut c_void, Data>(symbol(services, c"AEGetDescData").unwrap()),
        )
    };
    // SAFETY: a live descriptor, read only.
    let len = unsafe { size(&raw const target.desc) };
    assert_eq!(usize::try_from(len).unwrap(), SYSTEM_EVENTS.len());
    let mut back = vec![0_u8; SYSTEM_EVENTS.len()];
    // SAFETY: a live descriptor and a buffer of `len` writable bytes.
    let copied = unsafe { data(&raw const target.desc, back.as_mut_ptr().cast(), len) };
    assert_eq!(copied, 0);
    assert_eq!(back, SYSTEM_EVENTS);
    let kind = target.desc.descriptor_type;
    assert_eq!(kind, TYPE_APPLICATION_BUNDLE_ID);
}

#[test]
fn each_status_reads_as_its_answer() {
    assert_eq!(consent_of(0), AutomationConsent::Allowed);
    assert_eq!(consent_of(-1743), AutomationConsent::Refused);
    assert_eq!(consent_of(-1744), AutomationConsent::Unanswered);
    assert_eq!(consent_of(-600), AutomationConsent::Unread);
}
