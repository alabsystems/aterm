// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Static guards on the Win32 resources `build.rs` links into THE shipped exe.
//!
//! These run on EVERY platform on purpose. The inputs are two committed files
//! (`assets/aterm.manifest` and aterm-gui's `.ico`), a macOS-only contributor can
//! break them just as easily as a Windows one, and the failure they guard against
//! is invisible until someone launches the built binary on Windows and looks at
//! the title bar — which is exactly how the missing icon shipped for months in the
//! first place.
//!
//! What they cannot check: that a resource compiler was actually present and the
//! `.rsrc` section really landed. That is a property of the built PE, not of the
//! source tree; `build.rs` covers it with a loud banner plus the opt-in
//! `ATERM_REQUIRE_WIN_RESOURCES=1` hard failure.

use std::path::PathBuf;

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn manifest_text() -> String {
    let path = crate_dir().join("assets/aterm.manifest");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// The manifest with every `<!-- … -->` removed.
///
/// Needed because the file's own comments discuss the very elements some of these
/// assertions require to be ABSENT (the `<dpiAware>` explanation, most of all).
/// Matching on raw text would let a comment satisfy — or falsely fail — a check
/// about real markup.
fn manifest_markup() -> String {
    let raw = manifest_text();
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw.as_str();
    while let Some(open) = rest.find("<!--") {
        out.push_str(&rest[..open]);
        match rest[open..].find("-->") {
            Some(close) => rest = &rest[open + close + 3..],
            // Unterminated comment: the file is not well-formed XML, and the SxS
            // parser would reject the whole manifest at load. Say so here rather
            // than silently swallowing the tail.
            None => panic!("aterm.manifest has an unterminated XML comment"),
        }
    }
    out.push_str(rest);
    out
}

/// Common-Controls v6 is the entry with consequences beyond looks: without it the
/// process binds comctl32 v5, every `MessageBoxW` draws classic grey 3D buttons,
/// and `TaskDialogIndirect` — the documented follow-up for aterm's close/quit
/// confirmation — does not exist in the loaded DLL at all.
#[test]
fn manifest_binds_common_controls_v6() {
    let m = manifest_markup();
    assert!(
        m.contains("Microsoft.Windows.Common-Controls"),
        "manifest must declare a dependency on Common-Controls"
    );
    assert!(m.contains(r#"version="6.0.0.0""#), "Common-Controls must be pinned at v6");
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
    assert!(m.contains("<longPathAware"), "manifest must declare longPathAware");
    assert!(
        m.contains(">true</longPathAware>"),
        "longPathAware must be true, not merely present"
    );
    assert!(m.contains("<activeCodePage"), "manifest must declare activeCodePage");
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
    assert!(count >= 2, "expected the conventional supportedOS ladder, found {count} entries");
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

/// THE TRAP THIS FILE EXISTS FOR.
///
/// winit calls `SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2)` itself at
/// event-loop bring-up. A manifest DPI declaration is applied by the loader, before
/// any Rust runs, so it wins outright — winit's call then fails with
/// `ERROR_ACCESS_DENIED` and winit ignores the result *silently*. Adding
/// `<dpiAware>`/`<dpiAwareness>` here therefore hands ownership of the DPI mode to
/// a file that nobody would think to check when per-monitor DPI regresses. It is
/// the single most tempting line to add to a Win32 manifest and the single most
/// damaging one for this codebase.
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

/// THE SECOND TRAP, and the expensive one — this shipped once, in a build that
/// looked completely fixed.
///
/// `VS_VERSION_INFO` and `RT_MANIFEST` are MACROS from the Windows SDK headers.
/// `build.rs` deliberately includes no SDK header (so a box with `rc.exe` but no
/// configured include path still builds), which means a bare macro token is taken
/// by the resource compiler as a resource *name string* instead of the number it
/// stands for. The build then succeeds, `.rsrc` appears, and dumping the resource
/// tree shows the version entry under a NAME rather than under ordinal 1 — while
/// `GetFileVersionInfoSizeW` returns 0 and Explorer, Task Manager and
/// `FileVersionInfo` all read back empty, exactly as if there were no version
/// block at all. Measured, not theorised.
///
/// So the resource script must spell both as numbers. This is a source-text guard
/// because the property it protects only exists in a linked PE — it cannot be
/// asserted from a unit test, and by the time anyone notices on glass the symptom
/// is identical to the bug this whole change set fixes.
#[test]
fn resource_script_uses_numeric_ids_not_sdk_macros() {
    let src = std::fs::read_to_string(crate_dir().join("build.rs")).expect("build.rs is readable");
    assert!(
        src.contains("{VS_VERSION_INFO_ID} VERSIONINFO"),
        "the VERSIONINFO statement must be keyed by the numeric id constant; the bare \
         VS_VERSION_INFO macro becomes a resource NAME without <winver.h> and \
         GetFileVersionInfo then finds nothing"
    );
    assert!(
        src.contains("{RESOURCE_ID} {RT_MANIFEST}"),
        "the manifest statement must use the numeric RT_MANIFEST type (24); the bare \
         RT_MANIFEST macro becomes a resource TYPE NAME without <winuser.h>"
    );
}

/// `build.rs` reaches across into aterm-gui for the icon rather than keeping a
/// second copy. That cross-crate path is exactly the kind of thing a directory
/// move breaks quietly — the build would only warn, and the exe would ship blank.
#[test]
fn icon_the_build_script_embeds_exists_and_is_an_ico() {
    let icon = crate_dir().join("../aterm-gui/assets/aterm.ico");
    let bytes = std::fs::read(&icon)
        .unwrap_or_else(|e| panic!("build.rs embeds {}, which cannot be read: {e}", icon.display()));
    // ICONDIR: reserved=0, type=1 (icon), count>=1 — all little-endian u16.
    assert!(bytes.len() > 6, "icon file is truncated");
    assert_eq!(&bytes[0..4], &[0, 0, 1, 0], "not an .ico (bad ICONDIR header)");
    let frames = u16::from_le_bytes([bytes[4], bytes[5]]);
    assert!(frames > 0, "icon carries no frames");
}
