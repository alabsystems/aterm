// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// Win32 resource embedding and build metadata emission.

#![allow(
    clippy::disallowed_types,
    reason = "never linked into the shipped binary: a build script"
)]

/// Ordinal of both the icon and the manifest; resources are keyed by (type, id), so they do not
/// collide.
const RESOURCE_ID: u16 = 1;

/// `RT_MANIFEST` as its raw type number: the macro needs `#include <winuser.h>`, and a box with
/// `rc.exe` but no SDK headers on its include path would then fail to compile the resource.
const RT_MANIFEST: u16 = 24;

/// `VS_VERSION_INFO` as its raw id.
const VS_VERSION_INFO_ID: u16 = 1;

fn main() {
    build_stamp::emit();
    compiler::emit();
    chained_fixups();

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/STACK:{}", 8 * 1024 * 1024);
    }

    let manifest_dir = std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo always sets CARGO_MANIFEST_DIR"),
    );
    let out_dir = std::path::PathBuf::from(
        std::env::var_os("OUT_DIR").expect("cargo always sets OUT_DIR for a build script"),
    );

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

    let outcome = aterm_winres::compile(&rc_path);
    if outcome.is_linked() {
        return;
    }
    report(&outcome.to_string());
}

/// Apple-silicon macOS bins link with chained fixups (`ld -fixup_chains`), which the linker
/// emits only from a macOS 12 deployment target on its own; Rust's for `aarch64-apple-darwin`
/// is 11.0, so without this the shipped executable carries the older rebase/bind opcodes and
/// dyld rebases every pointer in `__DATA_CONST` (about 46,800 of them) before `main`.
fn chained_fixups() {
    let target = |key: &str| std::env::var(key).unwrap_or_default();
    if target("CARGO_CFG_TARGET_OS") == "macos" && target("CARGO_CFG_TARGET_ARCH") == "aarch64" {
        println!("cargo:rustc-link-arg-bins=-Wl,-fixup_chains");
    }
}

/// Print the "this exe will look unfinished" banner and honour `ATERM_REQUIRE_WIN_RESOURCES`.
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
        "    llvm-rc — or point ATERM_RC at one — then rebuild.".to_string(),
        "  set ATERM_REQUIRE_WIN_RESOURCES=1 to make this a hard build error".to_string(),
        "    instead of a warning (release cuts and CI should).".to_string(),
        "-----------------------------------------------------------------".to_string(),
    ] {
        println!("cargo:warning={line}");
    }

    let required = std::env::var("ATERM_REQUIRE_WIN_RESOURCES")
        .map(|v| !matches!(v.as_str(), "" | "0" | "false" | "no"))
        .unwrap_or(false);
    if required {
        panic!("ATERM_REQUIRE_WIN_RESOURCES is set and the Windows resources did not link: {why}");
    }
}

/// Build the resource script text.
fn resource_script(icon: &std::path::Path, app_manifest: &std::path::Path) -> String {
    let major = env_num("CARGO_PKG_VERSION_MAJOR");
    let minor = env_num("CARGO_PKG_VERSION_MINOR");
    let patch = env_num("CARGO_PKG_VERSION_PATCH");
    let display = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());

    let quad = format!("{major},{minor},{patch},0");

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
            // HARDLINK onto the console image, so they all report aterm.exe
            // here, truthfully. The windowed image (aterm-windowed.exe, shipped
            // as aterm-gui.exe) reports it too: one .rc serves both bins, and a
            // per-bin resource would need a second rc compile + link-arg-bin
            // pair to change one cosmetic string Explorer does not show.
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
fn rc_path_literal(p: &std::path::Path) -> String {
    p.display().to_string().replace('\\', "/")
}

/// A `CARGO_PKG_VERSION_*` component, clamped into the u16 a version-quad field
/// actually is.
fn env_num(key: &str) -> u16 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .map(|v| v.min(u32::from(u16::MAX)) as u16)
        .unwrap_or(0)
}

/// Run `cmd args…`, its trimmed stdout when it succeeded with some, else `None`.
fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(cmd).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

mod build_stamp {
    use super::run;
    use std::path::PathBuf;

    /// A build fact supplied by the caller instead of probed from git.
    fn pinned(key: &str) -> Option<String> {
        println!("cargo:rerun-if-env-changed={key}");
        std::env::var(key).ok().filter(|v| !v.trim().is_empty())
    }

    /// Emits the git commit stamp and `rerun-if-changed` directives.
    pub fn emit() {
        let pinned_commit = pinned("ATERM_BUILD_GIT_COMMIT");
        let pinned_full_commit = pinned("ATERM_BUILD_GIT_COMMIT_FULL");
        let pinned_dev_commits = pinned("ATERM_BUILD_DEV_COMMITS");
        let git_is_pinned =
            pinned_commit.is_some() && pinned_full_commit.is_some() && pinned_dev_commits.is_some();

        let commit = match &pinned_commit {
            Some(commit) => commit.clone(),
            None => {
                let commit = run("git", &["rev-parse", "--short=12", "HEAD"])
                    .unwrap_or_else(|| "unknown".into());
                let dirty = run("git", &["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
                if commit != "unknown" && dirty {
                    format!("{commit}-dirty")
                } else {
                    commit
                }
            }
        };
        println!("cargo:rustc-env=ATERM_GIT_COMMIT={commit}");
        let full_commit = pinned_full_commit.unwrap_or_else(|| {
            run("git", &["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into())
        });
        let dirty = commit.ends_with("-dirty");
        println!("cargo:rustc-env=ATERM_GIT_COMMIT_FULL={full_commit}");
        println!("cargo:rustc-env=ATERM_GIT_DIRTY={dirty}");

        let dev_commits = pinned_dev_commits.unwrap_or_else(|| {
            run(
                "git",
                &["describe", "--tags", "--match", "v*.*.0", "--abbrev=0"],
            )
            .and_then(|tag| {
                run(
                    "git",
                    &["rev-list", &format!("{}..HEAD", tag.trim()), "--count"],
                )
            })
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "0".into())
        });
        println!("cargo:rustc-env=ATERM_DEV_COMMITS={dev_commits}");

        let is_epoch = |s: &String| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        let build_number = std::env::var("SOURCE_DATE_EPOCH")
            .ok()
            .filter(is_epoch)
            .or_else(|| run("git", &["show", "-s", "--format=%ct", "HEAD"]).filter(is_epoch))
            .unwrap_or_else(|| "0".into());
        println!("cargo:rustc-env=ATERM_BUILD_NUMBER={build_number}");

        let build_time = match std::env::var("SOURCE_DATE_EPOCH") {
            Ok(epoch) if !epoch.is_empty() => {
                run("date", &["-u", "-r", &epoch, "+%Y-%m-%dT%H:%M:%SZ"]).or_else(|| {
                    run(
                        "date",
                        &["-u", "-d", &format!("@{epoch}"), "+%Y-%m-%dT%H:%M:%SZ"],
                    )
                })
            }
            _ => run("date", &["-u", "+%Y-%m-%dT%H:%M:%SZ"]),
        }
        .unwrap_or_else(|| "unknown".into());
        println!("cargo:rustc-env=ATERM_BUILD_TIME={build_time}");

        if !git_is_pinned {
            for path in git_watch_paths() {
                println!("cargo:rerun-if-changed={}", path.display());
            }
        }
        println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    }

    /// A path git prints, made absolute.
    fn git_abs_path(args: &[&str]) -> Option<PathBuf> {
        let mut with_format = vec!["rev-parse", "--path-format=absolute"];
        with_format.extend_from_slice(args);
        let value = run("git", &with_format).or_else(|| {
            let mut plain = vec!["rev-parse"];
            plain.extend_from_slice(args);
            run("git", &plain)
        })?;
        let path = PathBuf::from(value);
        if path.is_absolute() {
            Some(path)
        } else {
            std::env::current_dir().ok().map(|cwd| cwd.join(path))
        }
    }

    /// The git files whose change may stale the commit stamp, resolved through git so a linked
    /// worktree (whose `.git` is a file) and the main checkout both work.
    fn git_watch_paths() -> Vec<PathBuf> {
        let mut paths = Vec::new();
        let Some(git_dir) = run("git", &["rev-parse", "--absolute-git-dir"]).map(PathBuf::from)
        else {
            return paths;
        };
        let common_dir = git_abs_path(&["--git-common-dir"]).unwrap_or_else(|| git_dir.clone());

        paths.push(git_dir.join("HEAD"));

        if let Some(reference) = run("git", &["symbolic-ref", "--quiet", "HEAD"])
            && let (Some(ref_path), Some(refs_root)) = (
                git_abs_path(&["--git-path", &reference]),
                git_abs_path(&["--git-path", "refs"]),
            )
            && let Some(watch) = ref_path
                .ancestors()
                .take_while(|p| p.starts_with(&refs_root))
                .find(|p| p.exists())
        {
            paths.push(watch.to_path_buf());
        }

        paths.push(common_dir.join("packed-refs"));
        paths.push(common_dir.join("reftable"));

        paths.retain(|p| p.exists());
        paths.sort();
        paths.dedup();
        paths
    }
}

mod compiler {
    use super::run;

    include!("src/compiler_probe.rs");

    /// Emits compiler, profile, and verification build facts.
    pub fn emit() {
        let compiler_path = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
        let vv = run(&compiler_path, &["-vV"]).unwrap_or_default();
        let compiler = parse_rustc_vv(&vv);
        let toolchain = std::env::var("RUSTUP_TOOLCHAIN").ok();
        let flavor = detect_flavor(&vv, &compiler_path, toolchain.as_deref());
        println!(
            "cargo:rustc-env=ATERM_COMPILER_VERSION_LINE={}",
            compiler.version_line
        );
        println!("cargo:rustc-env=ATERM_COMPILER_COMMIT={}", compiler.commit);
        println!("cargo:rustc-env=ATERM_COMPILER_HOST={}", compiler.host);
        println!(
            "cargo:rustc-env=ATERM_COMPILER_TRUST_VERSION={}",
            compiler.trust_version
        );
        println!("cargo:rustc-env=ATERM_COMPILER_FLAVOR={flavor}");
        println!(
            "cargo:rustc-env=ATERM_BUILD_PROFILE={}",
            std::env::var("PROFILE").unwrap_or_else(|_| "unknown".into())
        );
        println!(
            "cargo:rustc-env=ATERM_TRUST_VERIFY={}",
            trust_verify_state()
        );
        println!("cargo:rerun-if-env-changed=CARGO_ENCODED_RUSTFLAGS");
        println!("cargo:rerun-if-env-changed=RUSTC");
        println!("cargo:rerun-if-env-changed=RUSTUP_TOOLCHAIN");
    }

    /// `"on"` iff this compile really runs the Trust verification pipeline.
    fn trust_verify_state() -> &'static str {
        fn from_value(flag: &str) -> Option<&'static str> {
            if let Some(v) = flag.strip_prefix("trust-verify=") {
                return Some(if v.eq_ignore_ascii_case("off") {
                    "off"
                } else {
                    "on"
                });
            }
            if let Some(v) = flag.strip_prefix("no-trust-verify=") {
                return Some(if v.eq_ignore_ascii_case("yes") {
                    "off"
                } else {
                    "on"
                });
            }
            None
        }

        if let Some(encoded) = std::env::var_os("CARGO_ENCODED_RUSTFLAGS") {
            let encoded = encoded.to_string_lossy().into_owned();
            let flags: Vec<&str> = encoded
                .split('\u{1f}')
                .filter(|flag| !flag.is_empty())
                .collect();
            let mut expect_z_value = false;
            for flag in flags {
                if expect_z_value {
                    expect_z_value = false;
                    if let Some(state) = from_value(flag) {
                        return state;
                    }
                    continue;
                }
                if flag == "-Z" {
                    expect_z_value = true;
                    continue;
                }
                if let Some(rest) = flag.strip_prefix("-Z")
                    && let Some(state) = from_value(rest)
                {
                    return state;
                }
            }
        }

        if std::env::var_os("CARGO_CFG_TRUST_VERIFY").is_some() {
            "on"
        } else {
            "off"
        }
    }
}
