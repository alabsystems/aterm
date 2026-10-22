// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Static guards on the Win32 resources `build.rs` links into the shipped exe.
//!
//! They run on every platform: the inputs are two committed files (`assets/aterm.manifest`
//! and aterm-gui's `.ico`), a macOS-only contributor can break them as easily as a Windows
//! one, and the failure is invisible until someone launches the built binary on Windows.
//!
//! They cannot check that a resource compiler was present and the `.rsrc` section landed:
//! that is a property of the built PE, which `build.rs` covers with a loud banner and the
//! opt-in `ATERM_REQUIRE_WIN_RESOURCES=1` hard failure.

use std::path::PathBuf;

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn manifest_text() -> String {
    let path = crate_dir().join("assets/aterm.manifest");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// The manifest with every `<!-- … -->` removed: the file's own comments discuss elements
/// some assertions require to be absent (`<dpiAware>` above all), so raw text could satisfy,
/// or falsely fail, a check about real markup.
fn manifest_markup() -> String {
    let raw = manifest_text();
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw.as_str();
    while let Some(open) = rest.find("<!--") {
        out.push_str(&rest[..open]);
        match rest[open..].find("-->") {
            Some(close) => rest = &rest[open + close + 3..],
            // Unterminated comment: the SxS parser would reject the whole manifest at load.
            None => panic!("aterm.manifest has an unterminated XML comment"),
        }
    }
    out.push_str(rest);
    out
}

/// Common-Controls v6 is the entry with consequences beyond looks: without it the
/// process binds comctl32 v5, every `MessageBoxW` draws classic grey 3D buttons,
/// and `TaskDialogIndirect` (aterm's task-modal confirm) does not exist in the
/// loaded DLL at all.
#[test]
fn manifest_binds_common_controls_v6() {
    let m = manifest_markup();
    assert!(
        m.contains("Microsoft.Windows.Common-Controls"),
        "manifest must declare a dependency on Common-Controls"
    );
    assert!(
        m.contains(r#"version="6.0.0.0""#),
        "Common-Controls must be pinned at v6"
    );
    assert!(
        m.contains(r#"publicKeyToken="6595b64144ccf1df""#),
        "Common-Controls v6 identity requires Microsoft's fixed public key token"
    );
}

/// `longPathAware` and `activeCodePage` are the two process-wide settings a
/// terminal actually feels: MAX_PATH on every cwd/OSC-7/drag-drop path, and the
/// ANSI codepage under a UTF-8 application.
#[test]
fn manifest_declares_windows_settings() {
    let m = manifest_markup();
    assert!(
        m.contains("<longPathAware"),
        "manifest must declare longPathAware"
    );
    assert!(
        m.contains(">true</longPathAware>"),
        "longPathAware must be true, not merely present"
    );
    assert!(
        m.contains("<activeCodePage"),
        "manifest must declare activeCodePage"
    );
    assert!(
        m.contains(">UTF-8</activeCodePage>"),
        "activeCodePage must be UTF-8, the encoding aterm uses end to end"
    );
}

/// The Windows 10/11 GUID is the load-bearing one: without it the OS applies
/// pre-Win10 compatibility shims and, in particular, ignores `activeCodePage`.
#[test]
fn manifest_declares_supported_os() {
    let m = manifest_markup();
    assert!(
        m.contains("{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"),
        "manifest must declare the Windows 10/11 supportedOS GUID"
    );
    let count = m.matches("<supportedOS").count();
    assert!(
        count >= 2,
        "expected the conventional supportedOS ladder, found {count} entries"
    );
}

/// A terminal must never ask to elevate, and the loader's installer-detection
/// heuristic is opt-out, not opt-in.
#[test]
fn manifest_requests_as_invoker() {
    assert!(
        manifest_markup().contains(r#"level="asInvoker""#),
        "manifest must request asInvoker execution level"
    );
}

/// winit calls `SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2)` itself at event-loop
/// bring-up. A manifest DPI declaration is applied by the loader before any Rust runs, so it
/// wins: winit's call then fails with `ERROR_ACCESS_DENIED`, which winit ignores silently.
/// A `<dpiAware>`/`<dpiAwareness>` line here would hand the DPI mode to a file nobody checks
/// when per-monitor DPI regresses.
#[test]
fn manifest_leaves_dpi_awareness_to_winit() {
    let m = manifest_markup();
    assert!(
        !m.contains("dpiAware"),
        "aterm.manifest must NOT declare dpiAware/dpiAwareness: winit owns process \
         DPI awareness via SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2), and a \
         manifest declaration silently overrides it at load time"
    );
}

/// `build.rs` reaches into aterm-gui for the icon rather than keeping a second copy, and a
/// directory move breaks that quietly: the build only warns, and the exe ships blank.
#[test]
fn icon_the_build_script_embeds_exists_and_is_an_ico() {
    let icon = crate_dir().join("../aterm-gui/assets/aterm.ico");
    let bytes = std::fs::read(&icon).unwrap_or_else(|e| {
        panic!(
            "build.rs embeds {}, which cannot be read: {e}",
            icon.display()
        )
    });
    // ICONDIR: reserved=0, type=1 (icon), count>=1 — all little-endian u16.
    assert!(bytes.len() > 6, "icon file is truncated");
    assert_eq!(
        &bytes[0..4],
        &[0, 0, 1, 0],
        "not an .ico (bad ICONDIR header)"
    );
    let frames = u16::from_le_bytes([bytes[4], bytes[5]]);
    assert!(frames > 0, "icon carries no frames");
}
