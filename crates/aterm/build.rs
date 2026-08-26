// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// build.rs — Win32 resources for THE shipped executable.
//
// WHY THIS FILE EXISTS AT ALL, and why the obvious "it's already done" is wrong:
// `crates/aterm-gui/build.rs` has embedded `assets/aterm.rc` (`1 ICON`) since the
// Windows port started, and `docs/NATIVE_WINDOWS_DESIGN.md` recorded it as done.
// But `embed_resource::compile` emits `cargo:rustc-link-arg-bins`, and a build
// script's link args reach only the bin targets of ITS OWN package — i.e.
// `aterm-gui.exe`, which nobody ships. The binary that ships is this crate's
// `[[bin]] name = "aterm"` (install.ps1 hardlinks all seven CLI names onto that
// one file), and this crate had no build script, so the shipped exe carried no
// resource section whatsoever:
//
//     resource_rva=0  size=0   sections: .text .rdata .data .pdata .reloc
//     FileDescription=[]  ProductName=[]  FileVersion=[]
//     WM_GETICON(SMALL)=0  WM_GETICON(BIG)=0  GCLP_HICON=0  GCLP_HICONSM=0
//
// — the generic Windows exe glyph in the caption, in Alt-Tab and in Task View,
// and a bare filename in Task Manager and Explorer. Start Menu and Desktop looked
// right only because install.ps1 copies a loose .ico next to the exe and points
// the shortcut's IconLocation at it; the exe itself had nothing.
//
// Three resources land here, all through one generated .rc:
//   1 ICON         the application icon (read back at runtime by winit's
//                  `Icon::from_resource(1, None)` in aterm-gui/src/app_window.rs)
//   1 RT_MANIFEST  the side-by-side manifest (assets/aterm.manifest) — Common-
//                  Controls v6, longPathAware, activeCodePage=UTF-8, supportedOS
//   VS_VERSION_INFO the version block Explorer / Task Manager / ARP read
//
// FAILURE POSTURE: warn loudly, never break the build. A machine with a Rust
// toolchain but no resource compiler must still be able to build aterm — that is
// why `embed-resource` is used with `manifest_optional()` semantics. But the
// SILENT half of that posture is exactly how the missing icon shipped for
// months, so every non-Ok outcome (including `NotAttempted`, which
// `manifest_optional()` maps to `Ok(())` and says nothing about) prints a
// multi-line banner naming the user-visible consequence, and
// `ATERM_REQUIRE_WIN_RESOURCES=1` turns the whole thing into a hard error for
// release cutters and CI.
//
// Off Windows this file returns on its first statement: no .rc is generated, no
// resource compiler is invoked, and NO link arg is emitted — so a macOS or Linux
// build produces a byte-identical binary to one built without this file. (The
// `embed-resource` build-dependency itself is still compiled, as any host build
// dependency is; it is the same version aterm-gui already pins, so it is already
// in the tree and adds nothing. Its `compile()` would also return `NotWindows`
// on its own — the early return above just means we never ask.)

/// Ordinal shared by the icon and the manifest.
///
/// They do not collide: resources are keyed by (type, id), so `1 ICON` and
/// `1 RT_MANIFEST` are two distinct entries. Neither number is a free choice —
/// `CREATEPROCESS_MANIFEST_RESOURCE_ID` is fixed at 1 (the only id the loader
/// consults for an .exe), and the runtime lookup in aterm-gui asks for icon
/// ordinal 1 by name. Changing this breaks a live consumer at both ends.
const RESOURCE_ID: u16 = 1;

/// `RT_MANIFEST`, spelled as the raw type number.
///
/// The prettier `1 RT_MANIFEST "…"` form needs `#include <winuser.h>`, which
/// drags the whole Windows SDK header set onto the resource compiler's include
/// path. That turns a box with `rc.exe` but no SDK headers configured from
/// "builds fine" into "fails to compile a resource" — the precise toolchain-less
/// case the warn-don't-fail posture above exists to survive. The number is
/// stable ABI; the macro is a convenience.
const RT_MANIFEST: u16 = 24;

/// `VS_VERSION_INFO`, spelled as the raw resource id — and this one is a TRAP
/// with a measurement behind it.
///
/// `VS_VERSION_INFO VERSIONINFO` is the form every example on the internet uses,
/// and it is WRONG without `#include <winver.h>`: `VS_VERSION_INFO` is a macro
/// that expands to `1`, and with no include rc.exe cannot know that, so it takes
/// the bare token as a resource *name string*. The build succeeds, `.rsrc`
/// appears, and dumping the resource tree shows `TYPE 16 / NAME named=True` — a
/// version resource keyed by the string "VS_VERSION_INFO". `GetFileVersionInfo`
/// only ever looks up `MAKEINTRESOURCE(1)`, so it finds nothing:
/// `GetFileVersionInfoSizeW` returns 0 and every property reads back empty,
/// which is indistinguishable from having no version block at all. That is
/// exactly the symptom this whole file exists to fix, so it would have shipped
/// looking fixed. Measured on a real build before the number replaced the macro.
///
/// Same reasoning as `RT_MANIFEST` above: the number is stable ABI, the macro
/// needs an SDK header the toolchain-less case may not have.
const VS_VERSION_INFO_ID: u16 = 1;

fn main() {
    // Build scripts run on the HOST, so `cfg!(windows)` here answers "what am I
    // running on", not "what am I building for" — wrong for either direction of
    // cross build. Cargo hands the answer we actually need in the environment.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest_dir = std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo always sets CARGO_MANIFEST_DIR"),
    );
    let out_dir = std::path::PathBuf::from(
        std::env::var_os("OUT_DIR").expect("cargo always sets OUT_DIR for a build script"),
    );

    // The .ico deliberately stays where it already lives, in aterm-gui's assets:
    // it is the SAME artwork `aterm-gui.exe` embeds, and a second copy under this
    // crate would be a second file to keep in sync and a second file to forget.
    // `crates/aterm` depends on `aterm-gui` by path, so the sibling directory is
    // present in any tree that can build this crate at all.
    //
    // Reached via the parent rather than a literal `../` so the path written into
    // the generated .rc is a clean absolute one — it is the first thing anyone
    // debugging a resource-compiler failure will read.
    let crates_dir = manifest_dir.parent().unwrap_or(&manifest_dir).to_path_buf();
    let icon = crates_dir.join("aterm-gui/assets/aterm.ico");
    let app_manifest = manifest_dir.join("assets/aterm.manifest");

    println!("cargo:rerun-if-changed={}", icon.display());
    println!("cargo:rerun-if-changed={}", app_manifest.display());
    println!("cargo:rerun-if-env-changed=ATERM_REQUIRE_WIN_RESOURCES");

    let rc_path = out_dir.join("aterm.rc");
    let rc_text = resource_script(&icon, &app_manifest);
    if let Err(e) = std::fs::write(&rc_path, rc_text) {
        report(&format!("could not write {}: {e}", rc_path.display()));
        return;
    }

    // `compile()` (not `compile_for*`) targets every bin of this package, which
    // is exactly one: `[[bin]] name = "aterm"`.
    let outcome = embed_resource::compile(&rc_path, embed_resource::NONE);
    if outcome == embed_resource::CompilationResult::Ok {
        return;
    }
    report(&outcome.to_string());
}

/// Emit the "this exe is going to look unfinished" banner, and honour the
/// opt-in hard-failure switch.
///
/// Deliberately many lines: cargo prints `cargo:warning` inline with the build
/// output where a single line disappears into a scrollback of dependency noise,
/// and *disappearing into the noise* is the specific failure mode being fixed.
/// The banner names the consequence rather than the mechanism, because the
/// person who needs to act on it is looking at a window, not at a linker.
fn report(why: &str) {
    for line in [
        "-----------------------------------------------------------------".to_string(),
        "aterm: the Windows executable is being built WITHOUT its resources".to_string(),
        format!("  cause: {why}"),
        "  visible result: the generic Windows exe icon in the title bar, in".to_string(),
        "    Alt-Tab and in Task View; a bare filename (no description, no".to_string(),
        "    version) in Task Manager, Explorer and Apps & Features; classic".to_string(),
        "    grey 3D dialog buttons instead of Common-Controls v6.".to_string(),
        "  fix: install a resource compiler — the Windows SDK's rc.exe (it is".to_string(),
        "    on PATH inside a VS Developer prompt / vcvars64) or LLVM's".to_string(),
        "    llvm-rc — then rebuild.".to_string(),
        "  set ATERM_REQUIRE_WIN_RESOURCES=1 to make this a hard build error".to_string(),
        "    instead of a warning (release cuts and CI should).".to_string(),
        "-----------------------------------------------------------------".to_string(),
    ] {
        println!("cargo:warning={line}");
    }

    // Opt-in, not opt-out. A contributor on a fresh box gets a working build and
    // a loud banner; a release cutter gets a build that refuses to produce an
    // exe the shell would render as an anonymous rectangle.
    let required = std::env::var("ATERM_REQUIRE_WIN_RESOURCES")
        .map(|v| !matches!(v.as_str(), "" | "0" | "false" | "no"))
        .unwrap_or(false);
    if required {
        panic!("ATERM_REQUIRE_WIN_RESOURCES is set and the Windows resources did not link: {why}");
    }
}

/// Build the resource script text.
///
/// Generated rather than checked in for one reason: `VS_VERSION_INFO` carries the
/// version FOUR times (two numeric quads, two strings) and a checked-in .rc would
/// be four more places to forget on a version bump. Everything here comes from
/// `CARGO_PKG_VERSION_*`, which cargo derives from the workspace `version` this
/// crate inherits — so the strings Explorer and Task Manager show cannot drift
/// from the crate version, ever.
fn resource_script(icon: &std::path::Path, app_manifest: &std::path::Path) -> String {
    // AUTHORIZED direct read of cargo's version env (allowlisted by
    // aterm-types' `shipped_crates_cannot_bypass_the_shared_app_version`):
    // a build script cannot link aterm_types, and these vars are the same
    // workspace `version` that APP_VERSION itself is derived from — one
    // ground truth, read at the only time a build script can read it.
    let major = env_num("CARGO_PKG_VERSION_MAJOR");
    let minor = env_num("CARGO_PKG_VERSION_MINOR");
    let patch = env_num("CARGO_PKG_VERSION_PATCH");
    let display = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());

    // The fourth field of a Windows version quad is the build number, and aterm
    // has a real one (`ATERM_BUILD_NUMBER`, from SOURCE_DATE_EPOCH or HEAD's
    // committer epoch — see crates/aterm-gui/build.rs). It is NOT used here: it
    // is a Unix epoch, which overflows the u16 each quad field is, and
    // reproducing the git probe in a second build script to squeeze it into
    // range would buy a number nothing reads. Zero is the honest filler; the
    // exact build is already in the About panel and `aterm ctl version`.
    let quad = format!("{major},{minor},{patch},0");

    // VS_FF_* flags. Both are statements of fact about THIS binary, so both are
    // computed rather than hardcoded: a debug build says so, and a pre-release
    // version (`0.45.0-rc1`) says so. FILEFLAGSMASK must cover every bit we may
    // set, so it is the full 0x3f rather than the two bits used today.
    const VS_FF_DEBUG: u32 = 0x1;
    const VS_FF_PRERELEASE: u32 = 0x2;
    let mut flags = 0u32;
    if std::env::var("PROFILE").as_deref() == Ok("debug") {
        flags |= VS_FF_DEBUG;
    }
    if !std::env::var("CARGO_PKG_VERSION_PRE")
        .unwrap_or_default()
        .is_empty()
    {
        flags |= VS_FF_PRERELEASE;
    }

    format!(
        r#"// GENERATED by crates/aterm/build.rs -- do not edit, do not check in.
// Regenerated on every build; see that file for why each entry is here.
// Kept strictly ASCII: an .rc without a UTF-16 BOM is decoded by rc.exe in the
// host ANSI codepage, so a stray non-ASCII byte anywhere -- even in a comment --
// decodes differently on a machine with a different locale.

{RESOURCE_ID} ICON "{icon}"
{RESOURCE_ID} {RT_MANIFEST} "{app_manifest}"

// The id is the literal 1, NOT the VS_VERSION_INFO macro -- see the const's doc
// comment in build.rs. Without <winver.h> the macro becomes a resource NAME and
// GetFileVersionInfo silently finds nothing.
{VS_VERSION_INFO_ID} VERSIONINFO
FILEVERSION     {quad}
PRODUCTVERSION  {quad}
FILEFLAGSMASK   0x3fL
FILEFLAGS       0x{flags:x}L
FILEOS          0x40004L        // VOS_NT_WINDOWS32
FILETYPE        0x1L            // VFT_APP
FILESUBTYPE     0x0L
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        // 0409 = en-US, 04b0 = 1200 = Unicode. Must agree with the
        // Translation pair below or the shell finds no strings at all.
        BLOCK "040904b0"
        BEGIN
            VALUE "CompanyName",      "Andrew Yates"
            VALUE "FileDescription",  "aterm - the transparent terminal"
            VALUE "FileVersion",      "{display}"
            VALUE "InternalName",     "aterm"
            VALUE "LegalCopyright",   "Copyright (C) 2026 Andrew Yates. Licensed under Apache-2.0."
            // Every installed CLI alias (aterm-ctl.exe, atpkg.exe, ...) is a
            // HARDLINK onto this same file, so they all report aterm.exe here.
            // That is the truth, not a bug: there is exactly one executable.
            VALUE "OriginalFilename", "aterm.exe"
            VALUE "ProductName",      "aterm"
            VALUE "ProductVersion",   "{display}"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x409, 1200
    END
END
"#,
        icon = rc_path_literal(icon),
        app_manifest = rc_path_literal(app_manifest),
    )
}

/// A path as it may appear inside an `.rc` string literal.
///
/// Backslash is the ESCAPE character in an .rc string, so a raw Windows path
/// (`C:\Users\…`) is read as the invalid escapes `\U`, `\…`. Forward slashes
/// avoid the question entirely: every resource compiler in play (`rc.exe`,
/// `llvm-rc`, `windres`) hands the path to the platform's file open, and Win32
/// accepts `/` as a separator. The alternative — doubling every backslash — is
/// what MSVC's own generator emits, but it is one more transformation to get
/// wrong and it reads worse in the generated file when debugging.
fn rc_path_literal(p: &std::path::Path) -> String {
    p.display().to_string().replace('\\', "/")
}

/// A `CARGO_PKG_VERSION_*` component, clamped into the u16 a version-quad field
/// actually is. Cargo guarantees these parse; the fallback keeps a malformed
/// environment from failing a build over a cosmetic resource.
fn env_num(key: &str) -> u16 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .map(|v| v.min(u32::from(u16::MAX)) as u16)
        .unwrap_or(0)
}
