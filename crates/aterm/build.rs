// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// build.rs — Win32 resources for THE shipped executable.
//
// WHY HERE: the resource step emits `cargo:rustc-link-arg-bins`, and a build
// script's link args reach only the bin targets of ITS OWN package. The binary
// that ships is this crate's `[[bin]] name = "aterm"` (install.ps1 hardlinks every
// CLI name onto it), so its resources must be embedded here, not in aterm-gui —
// without them Windows shows the generic exe glyph and a bare filename.
//
// Three resources land here, all through one generated .rc:
//   1 ICON         the application icon (read back at runtime by winit's
//                  `Icon::from_resource(1, None)` in aterm-gui/src/app_window.rs)
//   1 RT_MANIFEST  the side-by-side manifest (assets/aterm.manifest) — Common-
//                  Controls v6, longPathAware, activeCodePage=UTF-8, supportedOS
//   VS_VERSION_INFO the version block Explorer / Task Manager / ARP read
//
// FAILURE POSTURE: warn loudly, never break the build. A machine with no resource
// compiler must still build aterm, but a silent miss is how a missing icon ships,
// so every outcome that is not `Linked` prints a banner naming the user-visible
// consequence (`aterm_winres::Outcome` has no silent variant), and
// `ATERM_REQUIRE_WIN_RESOURCES=1` makes it a hard error for release cutters.
//
// THE BUILD FACTS are this script's other job, on every target: `mod build_stamp`
// below emits the commit, build number and build time, and `mod compiler` the
// compiler provenance, profile and verification state, that `src/main.rs`'s
// `BUILD_STAMP` compiles in and installs at startup (`aterm_types::version`).
// Stamped here, in the binary crate, so a commit or a compiler change recompiles
// this crate rather than the libraries. It emits only `rustc-env` values, never
// a link arg.
//
// Off Windows the resource half returns at once: no .rc, no resource compiler,
// and NO link arg — a macOS or Linux binary is byte-identical to one built
// without this file.

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
    build_stamp::emit();
    compiler::emit();

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

    // The link arg the step emits targets every bin of this package: the console
    // image `aterm` AND the windowed image `aterm-windowed` (shipped as
    // `aterm-gui.exe`), which is what we want — both files are launched by
    // Explorer or shown in Task Manager, so both carry the icon, the manifest
    // and the version block. One .rc for both: the only per-file string in it
    // is `OriginalFilename`, and it names `aterm.exe` for the windowed image
    // too (see the note beside that VALUE).
    let outcome = aterm_winres::compile(&rc_path);
    if outcome.is_linked() {
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
        "    llvm-rc — or point ATERM_RC at one — then rebuild.".to_string(),
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
    // This crate is the one reader of `CARGO_PKG_VERSION*`: these are the same
    // workspace `version` `src/main.rs`'s `BUILD_STAMP` compiles in — one
    // ground truth, read at the only time a build script can read it.
    let major = env_num("CARGO_PKG_VERSION_MAJOR");
    let minor = env_num("CARGO_PKG_VERSION_MINOR");
    let patch = env_num("CARGO_PKG_VERSION_PATCH");
    let display = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());

    // The fourth field of a Windows version quad is the build number, and aterm
    // has a real one (`ATERM_BUILD_NUMBER`, from SOURCE_DATE_EPOCH or HEAD's
    // committer epoch — see `mod build_stamp`). It is NOT used here: it
    // is a Unix epoch, which overflows the u16 each quad field is, and
    // squeezing it into range would buy a number nothing reads. Zero is the honest filler; the
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

/// Run `cmd args…`, its trimmed stdout when it succeeded with some, else `None`.
fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(cmd).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

// THE STAMP — the binary's build facts, stamped into THIS crate as compile-time
// env vars. `src/main.rs` compiles them into ONE `BUILD_STAMP` and installs it
// first thing (`aterm_types::version::install`), and every surface reads them
// from there (`aterm-gui/src/build_info.rs`: the About panel, `aterm ctl
// version`, the menu signature, the handoff identity; `app_version()` everywhere
// else). `mod build_stamp` emits the per-commit half:
//
//   ATERM_GIT_COMMIT      short commit the binary was built from (+ "-dirty" when the
//                         working tree had uncommitted changes); "unknown" w/o git.
//   ATERM_GIT_COMMIT_FULL all forty hex characters, or "unknown".
//   ATERM_GIT_DIRTY       "true" | "false".
//   ATERM_DEV_COMMITS     commits since the newest release tag; "0" w/o git or tag.
//   ATERM_BUILD_NUMBER    monotonic build number (ordering is independent of the
//                         display/source version): SOURCE_DATE_EPOCH — the ledger
//                         claim `targo --unverified ship cut` exports — wins when set;
//                         else HEAD's committer Unix epoch (dev builds); "0" only w/o git.
//   ATERM_BUILD_TIME      UTC build timestamp (RFC3339), or "unknown".
//
// and `mod compiler` the rest (see there).
//
// WHY HERE AND NOT IN A LIBRARY'S build script: every value above moves with every
// commit, and a build script's `rustc-env` reaches every unit of its package.
// Stamped in aterm-gui, an empty commit recompiled all of aterm-gui (measured 185 s
// incremental); here it recompiles only this crate's own units.
//
// All probes are best-effort: a missing `git`/`date` degrades to "unknown" rather
// than failing the build (so a source tarball without a .git still compiles).
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
        // THE PIN. A caller that already knows these facts — the gate,
        // which resolves them ONCE for the snapshot it verifies — supplies them, and
        // this script then asks git nothing and WATCHES NOTHING. That is the whole
        // point: see `git_watch_paths` for the defect the watch caused.
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

        // THE DEV COUNTER: commits since the newest release tag, so a development
        // build is identified by this count and its hash, never by advancing the
        // version. "0" when git or the tag is unavailable (a source-tarball build
        // still marks DEV via the release-env discriminator).
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

        // Monotonic build number, epoch-scale (seconds). The updater's "apply only if
        // greater" ordering lives in this metadata, independent of the app/source
        // display version. A dev build derives it from HEAD's committer Unix epoch:
        //   * strictly monotonic across releases — a later release is a later commit;
        //   * STABLE across rebuilds of the same commit — the committer date is frozen in the
        //     commit object, so two builds of one release agree.
        //   * topology-INDEPENDENT — a per-commit field, so it survives a shallow clone and
        //     rebase/squash/amend just re-stamp it forward; no hand-bumped counter to drift.
        // Fits a single sub-2^32 CFBundleVersion component (~1.78e9 today, valid past 2100).
        //
        // COORDINATION: `SOURCE_DATE_EPOCH`, when a valid epoch, WINS over the live `git`
        // read — it is the release PIN. `targo --unverified ship cut` (crates/aterm-release) claims the
        // build number from the append-only `RELEASES.ledger` (`n = max(tail+1, unix_now)`,
        // the same epoch scale as the dev fallback) and exports it ONCE, so this binary's
        // `ATERM_BUILD_NUMBER`, the bundle's sealed CFBundleVersion, and the release
        // manifest's `build_number` all carry the SAME n even if HEAD moves between build
        // steps — the tool's post-build self-check asserts the triple agreement. Without
        // the pin they would each re-read HEAD at a different instant and could DISAGREE,
        // which fails the updater's sealed-CFBundleVersion == manifest.build_number
        // anti-replay bind. Unset (a dev build) ⇒ HEAD's committer epoch; "0" only w/o git.
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

        // Re-stamp when HEAD moves (new commit / checkout); the release build number
        // comes from SOURCE_DATE_EPOCH. The git paths are asked of git.
        //
        // In a linked worktree `.git` is a FILE, so literal `.git/…` paths never
        // exist and cargo reruns the script on every build. The index is not watched
        // (`git status` refreshes it constantly); the dirty flag is computed whenever
        // the script runs. A PINNED BUILD WATCHES NO GIT PATH AT ALL: see
        // `git_watch_paths`.
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

    /// The git files whose change means the commit stamp may be stale, resolved
    /// through git so a linked worktree (whose `.git` is a file) and the main
    /// checkout both work. Only EXISTING paths are returned: cargo reruns a build
    /// script on every build while a watched path is missing. Modelled on
    /// `git_watch_paths` in crates/aterm-release/build.rs, minus the index.
    ///
    /// WHAT THIS WATCH COSTS WHEN IT IS NOT PINNED: the paths hang off the COMMON
    /// git dir, shared by every linked worktree, so a `fetch`, `gc` or `pack-refs` in
    /// ANY of them invalidates this crate's compile in ALL of them, mid-gate included.
    /// The stamp really can change with those files, so the watch is right but
    /// unpinnable from here. A caller that has resolved the facts passes
    /// `ATERM_BUILD_GIT_COMMIT`, `ATERM_BUILD_GIT_COMMIT_FULL` and
    /// `ATERM_BUILD_DEV_COMMITS`, and then neither the probe nor this watch runs.
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

// COMPILER provenance (which compiler produced this binary — matters because a
// machine can carry both upstream Rust and the Trust toolchain's trustc, and
// different machines carry DIFFERENT trust builds), parsed from `$RUSTC -vV` by
// src/compiler_probe.rs (include!d below so the same parser is unit-tested under
// the test suite; RUSTC is the env name cargo AND targo hand build scripts):
//
//   ATERM_COMPILER_VERSION_LINE  full first line of `$RUSTC -vV`
//   ATERM_COMPILER_COMMIT        the compiler's full git commit hash
//   ATERM_COMPILER_HOST          the compiler's host triple
//   ATERM_COMPILER_TRUST_VERSION Trust's OWN version (the `trust:` -vV line), "" if none
//   ATERM_COMPILER_FLAVOR        'r' (upstream Rust) | 't' (Trust: trustc)
//   ATERM_BUILD_PROFILE          cargo PROFILE ("debug"/"release")
//   ATERM_TRUST_VERIFY           "on" iff the Trust verification pipeline was in
//                                this compile, else "off"
//
// All probes are best-effort: a missing compiler answer degrades to "unknown"
// rather than failing the build.
mod compiler {
    use super::run;

    // Shared with the test suite: `src/main.rs` mounts the same file as a
    // #[cfg(test)] module, so the -vV parser and the flavor classifier are tested
    // against real fixtures while this script uses the identical code.
    include!("src/compiler_probe.rs");

    /// Emit the compiler, profile and verification facts.
    pub fn emit() {
        // Interrogate the ACTUAL compiler the build driver is about to use (cargo
        // and targo both set RUSTC for build scripts — targo's custom_build lane
        // keeps the env name for drop-in compatibility; bare "rustc" is the
        // no-driver fallback) rather than trusting whatever is first on PATH —
        // per-binary provenance is the whole point when upstream Rust and the
        // Trust toolchain (trustc) coexist on one machine.
        let compiler_path = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
        let vv = run(&compiler_path, &["-vV"]).unwrap_or_default();
        let compiler = parse_rustc_vv(&vv);
        // Flavor ('r' = upstream Rust, 't' = Trust fork), in priority order:
        //   1. the -vV self-identification ('binary: trustc' / '(trustc)' version
        //      line — the 2026-07 toolchains stamp both; covers the lane where a
        //      bare PATH-resolved `rustc` sets no env hint at all);
        //   2. RUSTC path containing '/trust/' (the fork lives in $HOME/trust/build/...);
        //   3. RUSTUP_TOOLCHAIN == 'trust' (a linked `rustup run trust` lane);
        //   4. default 'r'.
        // NOT inferred from '-dev' in the release — any local rustc build reports
        // -dev. There is NO env override: an ATERM_COMPILER_FLAVOR that outranked
        // the compiler's own self-report let a shell claim trustc provenance for a
        // binary upstream rustc built, in the one field About and `ctl version`
        // ship AS evidence. The three signals above cover every lane it was added
        // for.
        let toolchain = std::env::var("RUSTUP_TOOLCHAIN").ok();
        let flavor = detect_flavor(&vv, &compiler_path, toolchain.as_deref());
        println!(
            "cargo:rustc-env=ATERM_COMPILER_VERSION_LINE={}",
            compiler.version_line
        );
        println!("cargo:rustc-env=ATERM_COMPILER_COMMIT={}", compiler.commit);
        println!("cargo:rustc-env=ATERM_COMPILER_HOST={}", compiler.host);
        // Trust's OWN version (the `trust:` -vV line, e.g. 0.1.0) — "" when the
        // compiler reports none. Distinct from the rustc-shaped release token in
        // the version line, which is the Rust release Trust is compatible with.
        println!(
            "cargo:rustc-env=ATERM_COMPILER_TRUST_VERSION={}",
            compiler.trust_version
        );
        println!("cargo:rustc-env=ATERM_COMPILER_FLAVOR={flavor}");
        println!(
            "cargo:rustc-env=ATERM_BUILD_PROFILE={}",
            std::env::var("PROFILE").unwrap_or_else(|_| "unknown".into())
        );
        // Whether the Trust verification pipeline was really in this compile, for
        // About and `aterm ctl version`. See `trust_verify_state` — the `--print
        // cfg` probe alone answers this WRONG on a Trust toolchain.
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
    /// `CARGO_CFG_TRUST_VERIFY` alone is NOT the answer, and trusting it shipped a
    /// false provenance line for months. Cargo derives every `CARGO_CFG_*` from a
    /// `rustc --print cfg` probe, and targo deliberately strips the `-Ztrust-verify`
    /// family from that probe — so on a Trust toolchain the cfg reports the
    /// compiler's DEFAULT (verification on) while every real unit is compiled with
    /// `-Ztrust-verify=off` by the `targo --unverified` lane (until 2026-10-07 by
    /// `.cargo/config.toml`, which carries none now). Printing that as "on" claims
    /// a hardening the binary does not carry, which is exactly what the honesty
    /// ratchet forbids. The lane's switch reaches this script in
    /// `CARGO_ENCODED_RUSTFLAGS` (measured 2026-10-07 on seal 321aaeda7, for
    /// `targo --unverified` and for a compat `cargo` given the cfg-scoped
    /// `--config` switch alike).
    ///
    /// So the rustflags cargo will actually pass to rustc win over the probe, and
    /// the probe is only the fallback for the case it does answer correctly: no
    /// explicit flag at all (a stock-Rust build, where the cfg is simply unset).
    ///
    /// Both spellings of the off-switch are recognised — `-Ztrust-verify=off` is
    /// current, `-Zno-trust-verify=yes` is the retired one — and both the joined
    /// (`-Ztrust-verify=off`) and split (`-Z` `trust-verify=off`) argument forms,
    /// since a rustflags list may carry either.
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
