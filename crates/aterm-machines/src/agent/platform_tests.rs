// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::port::{Fake, Ran};

/// `ioreg -rd1 -c IOPlatformExpertDevice` in the macOS 27.0 VM (VirtualMac2,1).
pub(crate) const IOREG_VM: &str = r##"+-o VMA2MACOSAP  <class IOPlatformExpertDevice, id 0x10000012f, registered, matched, active, busy 0 (9302 ms), retain 28>
    {
      "#address-cells" = <02000000>
      "AAPL,phandle" = <01000000>
      "serial-number" = <5a4e59564e54524a4e3500000000000000000000000000000000000000000000>
      "IOBusyInterest" = "IOCommand is not serializable"
      "target-type" = <"VMA2MACOS">
      "platform-name" = <766d6170706c6532000000000000000000000000000000000000000000000000>
      "name" = <"device-tree">
      "secure-root-prefix" = <"md">
      "region-info" = <4c4c2f4100000000000000000000000000000000000000000000000000000000>
      "manufacturer" = <"Apple Inc.">
      "target-sub-type" = <"VMA2MACOSAP">
      "compatible" = <"VMA2MACOSAP","VirtualMac2,1","AppleVirtualPlatformARM">
      "IOPlatformSerialNumber" = "ZNYVNTRJN5"
      "time-stamp" = <"Sat Aug 8 12:52:53 PDT 2026">
      "clock-frequency" = <00366e01>
      "model" = <"VirtualMac2,1">
      "device-tree-tag" = <"EmbeddedDeviceTrees-12661.1.5">
      "mlb-serial-number" = <0000000000000000000000000000000000000000000000000000000000000000>
      "model-number" = <564d303030310000000000000000000000000000000000000000000000000000>
      "IOConsoleSecurityInterest" = "IOCommand is not serializable"
      "IONWInterrupts" = "IONWInterrupts"
      "device_type" = <"bootrom">
      "#size-cells" = <02000000>
      "IOPlatformUUID" = "C3396674-FCD3-52E5-A1C1-BD5B9E8F5C43"
    }
"##;

#[test]
fn the_uuid_and_serial_come_from_the_platform_record() {
    assert_eq!(
        ioreg_value(IOREG_VM, "IOPlatformUUID").as_deref(),
        Some("C3396674-FCD3-52E5-A1C1-BD5B9E8F5C43")
    );
    assert_eq!(
        ioreg_value(IOREG_VM, "IOPlatformSerialNumber").as_deref(),
        Some("ZNYVNTRJN5")
    );
    assert_eq!(ioreg_value(IOREG_VM, "model"), None);
    let fake = Fake::root();
    fake.on(
        "/usr/sbin/ioreg -rd1 -c IOPlatformExpertDevice",
        Ran::ok(IOREG_VM),
    );
    assert_eq!(
        platform_uuid(&fake).unwrap(),
        "C3396674-FCD3-52E5-A1C1-BD5B9E8F5C43"
    );
}

#[test]
fn an_unanswered_uuid_is_an_error_never_an_empty_identity() {
    let fake = Fake::root();
    fake.on(
        "/usr/sbin/ioreg -rd1 -c IOPlatformExpertDevice",
        Ran::ok("+-o Root\n"),
    );
    assert!(platform_uuid(&fake).is_err());
    let fake = Fake::root();
    assert!(platform_uuid(&fake).is_err());
}
