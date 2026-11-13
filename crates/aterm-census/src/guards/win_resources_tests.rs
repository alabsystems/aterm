// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::super::tests::{run, spots};
use super::*;

const VALID_MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<!-- DELIBERATELY ABSENT: no <dpiAware> and no <dpiAwareness> -->
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency><dependentAssembly>
    <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls"
        version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
  </dependentAssembly></dependency>
  <application xmlns="urn:schemas-microsoft-com:asm.v3"><windowsSettings>
    <longPathAware xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">true</longPathAware>
    <activeCodePage xmlns="http://schemas.microsoft.com/SMI/2019/WindowsSettings">UTF-8</activeCodePage>
  </windowsSettings></application>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1"><application>
    <supportedOS Id="{1f676c76-80e1-4239-95bb-83d0f6d0da78}"/>
    <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
  </application></compatibility>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3"><security><requestedPrivileges>
    <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
  </requestedPrivileges></security></trustInfo>
</assembly>
"#;

#[test]
fn win_resources_accepts_sound_manifest_and_ico_and_flags_each_defect() {
    let mut clean = Vec::new();
    check_manifest(VALID_MANIFEST, &mut clean);
    check_icon(&[0, 0, 1, 0, 1, 0, 16], &mut clean);
    assert!(clean.is_empty(), "{clean:?}");

    let check_m = |raw: &str| {
        let mut out = Vec::new();
        check_manifest(raw, &mut out);
        out
    };
    assert_eq!(spots(&check_m("a\n<!-- open")), [format!("{MANIFEST}:2")]);
    assert!(!check_m(&VALID_MANIFEST.replace("6.0.0.0", "5.0.0.0")).is_empty());
    assert!(!check_m(&VALID_MANIFEST.replace(">true<", ">false<")).is_empty());
    assert!(!check_m(&VALID_MANIFEST.replace(">UTF-8<", ">ACP<")).is_empty());
    assert!(
        !check_m(&VALID_MANIFEST.replace(
            "<supportedOS Id=\"{1f676c76-80e1-4239-95bb-83d0f6d0da78}\"/>",
            "",
        ))
        .is_empty()
    );
    assert!(!check_m(&VALID_MANIFEST.replace("asInvoker", "requireAdministrator")).is_empty());
    assert!(
        !check_m(&VALID_MANIFEST.replace(
            "</windowsSettings>",
            "<dpiAwareness>PerMonitorV2</dpiAwareness></windowsSettings>",
        ))
        .is_empty()
    );

    let check_i = |bytes: &[u8]| {
        let mut out = Vec::new();
        check_icon(bytes, &mut out);
        out.len()
    };
    assert_eq!(check_i(&[0, 0, 1, 0, 1, 0]), 1);
    assert_eq!(check_i(&[0, 0, 2, 0, 1, 0, 0]), 1);
    assert_eq!(check_i(&[0, 0, 1, 0, 0, 0, 0]), 1);

    assert!(run(&WIN_RESOURCES, &[("crates/x/src/lib.rs", "")]).is_empty());
    assert_eq!(
        spots(&run(&WIN_RESOURCES, &[(BUILD_RS, "fn main() {}\n")])),
        [format!("{ICON}:0"), format!("{MANIFEST}:0")],
    );
}
