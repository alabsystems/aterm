// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! win-resources: the Win32 side-by-side manifest and icon `crates/aterm/build.rs` embeds stay whole.

use super::{Finding, Rule, Tree};

pub(super) const WIN_RESOURCES: Rule = Rule {
    id: "win-resources",
    why: "crates/aterm/build.rs links crates/aterm/assets/aterm.manifest and \
          crates/aterm-gui/assets/aterm.ico into the Windows executable: the manifest must bind \
          Common-Controls v6, declare longPathAware=true, activeCodePage=UTF-8, the Windows \
          10/11 supportedOS ladder and level=asInvoker, and leave DPI awareness to winit; the \
          icon must exist as a non-empty .ico",
    scope: &[],
    check: win_resources,
};

const BUILD_RS: &str = "crates/aterm/build.rs";
const CARGO_TOML: &str = "crates/aterm/Cargo.toml";
const MANIFEST: &str = "crates/aterm/assets/aterm.manifest";
const ICON: &str = "crates/aterm-gui/assets/aterm.ico";

fn win_resources(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    if tree.corpus.get(BUILD_RS).is_none() && !tree.root.join(CARGO_TOML).is_file() {
        return;
    }
    match std::fs::read_to_string(tree.root.join(MANIFEST)) {
        Ok(raw) => check_manifest(&raw, out),
        Err(e) => out.push(finding(
            MANIFEST,
            0,
            format!("cannot read embedded Win32 manifest ({e})"),
        )),
    }
    match std::fs::read(tree.root.join(ICON)) {
        Ok(bytes) => check_icon(&bytes, out),
        Err(e) => out.push(finding(
            ICON,
            0,
            format!("cannot read embedded Win32 icon ({e})"),
        )),
    }
}

fn finding(path: &str, line: usize, detail: impl Into<String>) -> Finding {
    Finding {
        rule: WIN_RESOURCES.id,
        path: path.to_string(),
        line,
        detail: detail.into(),
    }
}

fn strip_xml_comments(raw: &str) -> Result<String, usize> {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    let mut consumed = 0;
    while let Some(open) = rest.find("<!--") {
        out.push_str(&rest[..open]);
        match rest[open..].find("-->") {
            Some(close) => {
                let step = open + close + 3;
                consumed += step;
                rest = &rest[step..];
            }
            None => {
                let line = raw[..consumed + open].matches('\n').count() + 1;
                return Err(line);
            }
        }
    }
    out.push_str(rest);
    Ok(out)
}

fn check_manifest(raw: &str, out: &mut Vec<Finding>) {
    let markup = match strip_xml_comments(raw) {
        Ok(markup) => markup,
        Err(line) => {
            out.push(finding(MANIFEST, line, "unterminated XML comment"));
            return;
        }
    };
    const REQUIRED: &[(&str, &str)] = &[
        (
            "Microsoft.Windows.Common-Controls",
            "manifest must declare a dependency on Common-Controls",
        ),
        (
            r#"version="6.0.0.0""#,
            "Common-Controls must be pinned at v6",
        ),
        (
            r#"publicKeyToken="6595b64144ccf1df""#,
            "Common-Controls v6 identity requires Microsoft's fixed public key token",
        ),
        ("<longPathAware", "manifest must declare longPathAware"),
        (
            ">true</longPathAware>",
            "longPathAware must be true, not merely present",
        ),
        ("<activeCodePage", "manifest must declare activeCodePage"),
        (
            ">UTF-8</activeCodePage>",
            "activeCodePage must be UTF-8, the encoding aterm uses end to end",
        ),
        (
            "{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}",
            "manifest must declare the Windows 10/11 supportedOS GUID",
        ),
        (
            r#"level="asInvoker""#,
            "manifest must request asInvoker execution level",
        ),
    ];
    for &(needle, why) in REQUIRED {
        if !markup.contains(needle) {
            out.push(finding(MANIFEST, 0, why));
        }
    }
    let supported = markup.matches("<supportedOS").count();
    if supported < 2 {
        out.push(finding(
            MANIFEST,
            0,
            format!("expected the conventional supportedOS ladder, found {supported} entries"),
        ));
    }
    if markup.contains("dpiAware") {
        out.push(finding(
            MANIFEST,
            0,
            "manifest must not declare dpiAware/dpiAwareness: winit owns process DPI awareness \
             via SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2)",
        ));
    }
}

fn check_icon(bytes: &[u8], out: &mut Vec<Finding>) {
    if bytes.len() <= 6 {
        out.push(finding(ICON, 0, "icon file is truncated"));
        return;
    }
    if bytes[0..4] != [0, 0, 1, 0] {
        out.push(finding(ICON, 0, "not an .ico (bad ICONDIR header)"));
    }
    if u16::from_le_bytes([bytes[4], bytes[5]]) == 0 {
        out.push(finding(ICON, 0, "icon carries no frames"));
    }
}

#[cfg(test)]
mod tests {
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
}
