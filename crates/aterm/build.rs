// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Win32 resources for the shipped executable, and the build facts on every target.
//
// The resource step emits `cargo:rustc-link-arg-bins`, which reaches only this package's bins,
// so the resources of the shipped `aterm` (install.ps1 hardlinks every CLI name onto it) are
// embedded here, not in aterm-gui. One generated .rc carries:
//   1 ICON          the application icon (read back by winit's `Icon::from_resource(1, None)`
//                   in aterm-gui/src/app_window.rs)
//   1 RT_MANIFEST   assets/aterm.manifest: Common-Controls v6, longPathAware,
//                   activeCodePage=UTF-8, supportedOS
//   VS_VERSION_INFO the version block Explorer, Task Manager and ARP read
//
// A missing resource compiler warns but never breaks the build: every outcome but `Linked`
// prints a banner naming the visible consequence, and `ATERM_REQUIRE_WIN_RESOURCES=1` makes it
// an error. Off Windows the resource half emits nothing, no link arg included.
//
// On Apple silicon the bins link with chained fixups (`chained_fixups`), a launch-time cost
// before `main`.
//
// `mod build_stamp` and `mod compiler` emit the `rustc-env` build facts `src/main.rs`'s
// `BUILD_STAMP` compiles in, stamped in the binary crate so a commit recompiles only this crate.
#![allow(
    clippy::disallowed_types,
    reason = "never linked into the shipped binary: a build script"
)]

/// Ordinal of both the icon and the manifest; resources are keyed by (type, id), so they do not
/// collide. Neither is free: the loader consults manifest id 1 only
/// (`CREATEPROCESS_MANIFEST_RESOURCE_ID`), and aterm-gui looks up icon ordinal 1.
const RESOURCE_ID: u16 = 1;

/// `RT_MANIFEST` as its raw type number: the macro needs `#include <winuser.h>`, and a box with
/// `rc.exe` but no SDK headers on its include path would then fail to compile the resource.
const RT_MANIFEST: u16 = 24;

/// `VS_VERSION_INFO` as its raw id. Without `#include <winver.h>` rc.exe takes the bare macro
/// as a resource name string: the build succeeds, but `GetFileVersionInfo` looks up only
/// `MAKEINTRESOURCE(1)`, finds nothing, and every property reads back empty.
const VS_VERSION_INFO_ID: u16 = 1;

fn main() {
    build_stamp::emit();
    compiler::emit();
    chained_fixups();

    // A build script runs on the host, so `cfg!(windows)` would answer for the host, not the
    // target.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    // The main thread's stack: MSVC's linker reserves 1 MiB unless told otherwise, macOS
    // gives 8 MiB, and the window code is written against that. A debug build overflowed
    // the 1 MiB (0xC00000FD) when its last window closed and on `invoke NewWindow`
    // (measured 2026-09-27). Reserved address space, committed only as the stack grows.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/STACK:{}", 8 * 1024 * 1024);
    }

    let manifest_dir = std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo always sets CARGO_MANIFEST_DIR"),
    );
    let out_dir = std::path::PathBuf::from(
        std::env::var_os("OUT_DIR").expect("cargo always sets OUT_DIR for a build script"),
    );

    // The .ico is aterm-gui's (the same artwork `aterm-gui.exe` embeds, one copy); this crate
    // depends on aterm-gui by path, so the directory is present. Reached via the parent, not a
    // literal `../`, so the generated .rc carries a clean absolute path.
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

    // The link arg reaches every bin of this package, so the console `aterm` and the windowed
    // `aterm-windowed` (shipped as `aterm-gui.exe`) both carry the resources. One .rc serves
    // both (see the note beside `OriginalFilename`).
    let outcome = aterm_winres::compile(&rc_path);
    if outcome.is_linked() {
        return;
    }
    report(&outcome.to_string());
}

/// Apple-silicon macOS bins link with chained fixups (`ld -fixup_chains`), which the linker
/// emits only from a macOS 12 deployment target on its own; Rust's for `aarch64-apple-darwin`
/// is 11.0, so without this the shipped executable carries the older rebase/bind opcodes and
/// dyld rebases every pointer in `__DATA_CONST` (about 46,800 of them) before `main`. With
/// chains the kernel fixes each page up as it is first touched (page-in linking, macOS 13 on)
/// and dyld binds the rest from the chain's import table.
///
/// Every supported macOS loads it (checked 2026-10-19 against Apple's published sources).
/// For an 11.0 target the linker picks `DYLD_CHAINED_PTR_64` (its own default at 12.0 is
/// `DYLD_CHAINED_PTR_64_OFFSET`; it forces chains without a word even at 10.15, so its silence
/// proves nothing). The dyld of every macOS 11 release (832.7.1 for 11.0.1 and 11.1, 832.7.3,
/// 851.27, 852, 852.2 for 11.5) accepts that format in `validChainedFixupsInfo` and applies it
/// in `fixupAllChainedFixups`, from both of its launch paths, and reads `LC_DYLD_EXPORTS_TRIE`;
/// so did 750.6, macOS 10.15's. The kernel leaves both load commands to dyld. The shipped
/// binary's x86_64 slice carries no chains (below).
///
/// Chains bind every import at launch, where the old opcodes left most lazy until first
/// called: an import newer than the deployment target stops the app at launch on an older
/// macOS instead of at its first call. None is newer today: of the 529 on 2026-10-19, those
/// the SDK declares are from macOS 10.15 or earlier or carry no availability, and the 22 no
/// public header declares (the ARC runtime's, the stack guard's) are older still. A newer
/// API is looked up at run time (`dlsym`), as Rust's std does.
///
/// Measured 2026-10-18 on a profiling build relinked from the same object both ways (headless
/// launches through `posix_spawn`, launcher spun 200 ms first, interleaved, n=30 per arm and
/// kind): birth to Rust `main` 5.67 -> 5.17 ms cold and 5.58 -> 5.05 ms warm; the main
/// thread's CPU before `main` 4.39 -> 3.92 ms; page faults before `main` 682 -> 583.
///
/// Not on `x86_64-apple-darwin`, whose deployment target (10.12) predates chained fixups.
fn chained_fixups() {
    let target = |key: &str| std::env::var(key).unwrap_or_default();
    if target("CARGO_CFG_TARGET_OS") == "macos" && target("CARGO_CFG_TARGET_ARCH") == "aarch64" {
        println!("cargo:rustc-link-arg-bins=-Wl,-fixup_chains");
    }
}

/// Print the "this exe will look unfinished" banner and honour `ATERM_REQUIRE_WIN_RESOURCES`.
///
/// Many lines on purpose: a single `cargo:warning` line disappears into the dependency noise.
/// It names the visible consequence, not the mechanism.
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

    // Opt-in: a fresh box gets a working build and the banner; a release cut sets the switch.
    let required = std::env::var("ATERM_REQUIRE_WIN_RESOURCES")
        .map(|v| !matches!(v.as_str(), "" | "0" | "false" | "no"))
        .unwrap_or(false);
    if required {
        panic!("ATERM_REQUIRE_WIN_RESOURCES is set and the Windows resources did not link: {why}");
    }
}

/// Build the resource script text. Generated, not checked in: `VS_VERSION_INFO` carries the
/// version four times, all from `CARGO_PKG_VERSION_*`, so it cannot drift from the crate's.
fn resource_script(icon: &std::path::Path, app_manifest: &std::path::Path) -> String {
    let major = env_num("CARGO_PKG_VERSION_MAJOR");
    let minor = env_num("CARGO_PKG_VERSION_MINOR");
    let patch = env_num("CARGO_PKG_VERSION_PATCH");
    let display = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());

    // The quad's fourth field is the build number, left 0: aterm's (`ATERM_BUILD_NUMBER`) is a
    // Unix epoch, which overflows the u16 field. The exact build is in the About panel and
    // `aterm ctl version`.
    let quad = format!("{major},{minor},{patch},0");

    // VS_FF_* flags, computed: a debug build and a pre-release version (`0.45.0-rc1`) say so.
    // FILEFLAGSMASK is the full 0x3f so it covers every bit that may be set.
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

/// A path as it may appear inside an `.rc` string literal. Backslash is the .rc escape
/// character, so separators become `/`: every resource compiler in play (`rc.exe`, `llvm-rc`,
/// `windres`) hands the path to a Win32 file open, which accepts it.
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

/// Run `cmd args…`, its trimmed stdout when it succeeded with some, else `None`.
fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(cmd).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

// The build facts, stamped into this crate as compile-time env vars. `src/main.rs` compiles them
// into one `BUILD_STAMP` and installs it first thing (`aterm_types::version::install`); every
// surface reads them from there (`aterm-gui/src/build_info.rs`: the About panel, `aterm ctl
// version`, the menu signature, the handoff identity; `app_version()` elsewhere).
// `mod build_stamp` emits the per-commit half:
//
//   ATERM_GIT_COMMIT      short commit (+ "-dirty" when the working tree had uncommitted
//                         changes); "unknown" without git.
//   ATERM_GIT_COMMIT_FULL all forty hex characters, or "unknown".
//   ATERM_GIT_DIRTY       "true" | "false".
//   ATERM_DEV_COMMITS     commits since the newest release tag; "0" without git or tag.
//   ATERM_BUILD_NUMBER    monotonic build number: SOURCE_DATE_EPOCH (the ledger claim
//                         `targo --unverified ship cut` exports) when set, else HEAD's
//                         committer epoch; "0" only without git.
//   ATERM_BUILD_TIME      UTC build timestamp (RFC3339), or "unknown".
//
// and `mod compiler` the rest. Here, not in a library's build script, because a `rustc-env`
// reaches every unit of its package: stamped in aterm-gui, an empty commit recompiled all of
// aterm-gui (185 s incremental). Every probe is best-effort: a missing `git`/`date` degrades to
// "unknown", so a source tarball without a .git still builds.
mod build_stamp {
    use super::run;
    use std::path::PathBuf;

    /// A build fact supplied by the caller instead of probed from git. Empty is
    /// treated as absent, so `FOO=` cannot half-pin a build.
    fn pinned(key: &str) -> Option<String> {
        println!("cargo:rerun-if-env-changed={key}");
        std::env::var(key).ok().filter(|v| !v.trim().is_empty())
    }

    /// Emit the stamp and the `rerun-if` directives that keep it current.
    pub fn emit() {
        // A caller that has resolved these facts (the release build plan: its snapshot has no
        // `.git`) pins them, and the script then asks git nothing and watches nothing.
        let pinned_commit = pinned("ATERM_BUILD_GIT_COMMIT");
        let pinned_full_commit = pinned("ATERM_BUILD_GIT_COMMIT_FULL");
        let pinned_dev_commits = pinned("ATERM_BUILD_DEV_COMMITS");
        let git_is_pinned =
            pinned_commit.is_some() && pinned_full_commit.is_some() && pinned_dev_commits.is_some();

        // Git commit (short, 12 hex) + a "-dirty" suffix when the tree isn't clean.
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

        // Commits since the newest release tag: a dev build is identified by this count and its
        // hash, never by advancing the version. "0" without git or the tag.
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

        // Monotonic build number in epoch seconds: the updater's "apply only if greater"
        // ordering, independent of the display version. A dev build takes HEAD's committer epoch:
        // later for every later release, stable across rebuilds of one commit, and readable in a
        // shallow clone. It fits one sub-2^32 CFBundleVersion component.
        //
        // A valid `SOURCE_DATE_EPOCH` wins: `targo --unverified ship cut` claims the number from
        // `RELEASES.ledger` and exports it once, so this binary's `ATERM_BUILD_NUMBER`, the
        // bundle's sealed CFBundleVersion and the manifest's `build_number` agree even if HEAD
        // moves between steps, as the updater's sealed-CFBundleVersion == manifest.build_number
        // anti-replay bind requires.
        let is_epoch = |s: &String| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        let build_number = std::env::var("SOURCE_DATE_EPOCH")
            .ok()
            .filter(is_epoch)
            .or_else(|| run("git", &["show", "-s", "--format=%ct", "HEAD"]).filter(is_epoch))
            .unwrap_or_else(|| "0".into());
        println!("cargo:rustc-env=ATERM_BUILD_NUMBER={build_number}");

        // Build timestamp (UTC, RFC3339). Honour SOURCE_DATE_EPOCH for reproducible
        // builds when set; otherwise stamp the current wall clock.
        let build_time = match std::env::var("SOURCE_DATE_EPOCH") {
            // BSD date spells "format this epoch" `-r <epoch>`; GNU date spells it
            // `-d @<epoch>` (its -r means "a file's mtime"). Try BSD first — on GNU
            // the bare number is a missing file, a clean failure — then GNU.
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

        // Re-stamp when HEAD moves. The paths are asked of git: in a linked worktree `.git` is a
        // file, so a literal `.git/…` path never exists and cargo would rerun every build. The
        // index is not watched (`git status` refreshes it constantly); the dirty flag is computed
        // whenever the script runs. A pinned build watches no git path.
        if !git_is_pinned {
            for path in git_watch_paths() {
                println!("cargo:rerun-if-changed={}", path.display());
            }
        }
        println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    }

    /// A path git prints, made absolute. `--path-format=absolute` arrived after
    /// `--git-common-dir`/`--git-path`, so an older git's relative answer is joined
    /// to this script's working directory (the package root, where git ran).
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
    /// worktree (whose `.git` is a file) and the main checkout both work. Only existing paths:
    /// cargo reruns a build script on every build while a watched path is missing.
    ///
    /// The paths hang off the common git dir, so a `fetch`, `gc` or `pack-refs` in any worktree
    /// invalidates this crate's compile in all of them. Pinning the three `ATERM_BUILD_*` facts
    /// skips both the probe and this watch.
    fn git_watch_paths() -> Vec<PathBuf> {
        let mut paths = Vec::new();
        let Some(git_dir) = run("git", &["rev-parse", "--absolute-git-dir"]).map(PathBuf::from)
        else {
            return paths;
        };
        let common_dir = git_abs_path(&["--git-common-dir"]).unwrap_or_else(|| git_dir.clone());

        // HEAD is worktree-private: a checkout, or a commit on a detached HEAD.
        paths.push(git_dir.join("HEAD"));

        // The loose ref HEAD names moves on every commit to the branch. If it is
        // packed right now, the loose file does not exist yet, so watch the nearest
        // existing directory below `refs/` — the commit that writes it is observed.
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

        // A currently packed ref changes here; a reftable-backend repo keeps every
        // ref below `reftable/` (and HEAD is a stub there), so watch it when present.
        paths.push(common_dir.join("packed-refs"));
        paths.push(common_dir.join("reftable"));

        paths.retain(|p| p.exists());
        paths.sort();
        paths.dedup();
        paths
    }
}

// Compiler provenance (a machine can carry both upstream Rust and Trust's trustc, and machines
// carry different Trust builds), parsed from `$RUSTC -vV` by src/compiler_probe.rs, which is
// include!d below so the test suite tests the same parser:
//
//   ATERM_COMPILER_VERSION_LINE  full first line of `$RUSTC -vV`
//   ATERM_COMPILER_COMMIT        the compiler's full git commit hash
//   ATERM_COMPILER_HOST          the compiler's host triple
//   ATERM_COMPILER_TRUST_VERSION Trust's own version (the `trust:` -vV line), "" if none
//   ATERM_COMPILER_FLAVOR        'r' (upstream Rust) | 't' (Trust: trustc)
//   ATERM_BUILD_PROFILE          cargo PROFILE ("debug"/"release")
//   ATERM_TRUST_VERIFY           "on" iff the Trust verification pipeline was in
//                                this compile, else "off"
//
// Best-effort: a missing compiler answer degrades to "unknown".
mod compiler {
    use super::run;

    include!("src/compiler_probe.rs");

    /// Emit the compiler, profile and verification facts.
    pub fn emit() {
        // Ask the compiler the build driver is about to use (cargo and targo both set RUSTC;
        // bare "rustc" is the no-driver fallback), not whatever is first on PATH.
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
        // Whether the Trust verification pipeline ran in this compile (`trust_verify_state`).
        println!(
            "cargo:rustc-env=ATERM_TRUST_VERIFY={}",
            trust_verify_state()
        );
        println!("cargo:rerun-if-env-changed=CARGO_ENCODED_RUSTFLAGS");
        println!("cargo:rerun-if-env-changed=RUSTC");
        println!("cargo:rerun-if-env-changed=RUSTUP_TOOLCHAIN");
    }

    /// `"on"` iff this compile really runs the Trust verification pipeline.
    ///
    /// `CARGO_CFG_TRUST_VERIFY` alone answers wrong on a Trust toolchain: cargo derives every
    /// `CARGO_CFG_*` from a `rustc --print cfg` probe, and targo strips the `-Ztrust-verify`
    /// family from that probe, so the cfg reports the compiler's default (on) while every unit of
    /// the `targo --unverified` lane compiles with `-Ztrust-verify=off`. That switch reaches this
    /// script in `CARGO_ENCODED_RUSTFLAGS`, so the rustflags win and the probe is the fallback
    /// for a build with no explicit flag (stock Rust, where the cfg is unset).
    ///
    /// Recognises `-Ztrust-verify=off` and the retired `-Zno-trust-verify=yes`, joined or split
    /// (`-Z` `trust-verify=off`).
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
            // CARGO_ENCODED_RUSTFLAGS is 0x1f-separated (cargo's documented encoding).
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

        // No explicit flag: the cfg probe is authoritative (and on stock Rust, unset).
        if std::env::var_os("CARGO_CFG_TRUST_VERIFY").is_some() {
            "on"
        } else {
            "off"
        }
    }
}
