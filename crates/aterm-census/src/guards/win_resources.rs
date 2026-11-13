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
    match crate::tree::read_to_string(&tree.root.join(MANIFEST)) {
        Ok(raw) => check_manifest(&raw, out),
        Err(e) => out.push(finding(
            MANIFEST,
            0,
            format!("cannot read embedded Win32 manifest ({e})"),
        )),
    }
    match crate::tree::read_bytes(&tree.root.join(ICON)) {
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
#[path = "win_resources_tests.rs"]
mod tests;
