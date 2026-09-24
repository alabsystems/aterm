// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// build.rs — stamp build provenance into the binary as compile-time env vars,
// surfaced at runtime by `src/build_info.rs` (the About panel + `aterm-ctl version`).
//
//   ATERM_GIT_COMMIT   short commit the binary was built from (+ "-dirty" when the
//                      working tree had uncommitted changes); "unknown" w/o git.
//   ATERM_BUILD_NUMBER monotonic build number (ordering is independent of the
//                      display/source version): SOURCE_DATE_EPOCH — the ledger
//                      claim `cargo ship cut` exports — wins when set; else HEAD's
//                      committer Unix epoch (dev builds); "0" only w/o git.
//   ATERM_BUILD_TIME   UTC build timestamp (RFC3339), or "unknown".
//
// Plus COMPILER provenance (which compiler produced this binary — matters because
// this box carries both upstream Rust and the Trust toolchain's trustc, and
// different machines carry DIFFERENT trust builds), parsed from `$RUSTC -vV` by
// src/compiler_probe.rs (include!d below so the same parser is unit-tested under
// the test suite; RUSTC is the env name cargo AND targo hand build scripts):
//
//   ATERM_COMPILER_VERSION_LINE  full first line of `$RUSTC -vV`
//   ATERM_COMPILER_COMMIT        the compiler's full git commit hash
//   ATERM_COMPILER_HOST          the compiler's host triple
//   ATERM_COMPILER_TRUST_VERSION Trust's OWN version (the `trust:` -vV line), "" if none
//   ATERM_COMPILER_FLAVOR 'r' (upstream Rust) | 't' (Trust: trustc)
//   ATERM_BUILD_PROFILE  cargo PROFILE ("debug"/"release")
//   ATERM_TRUST_VERIFY   "on" iff --cfg trust_verify was active, else "off"
//
// All probes are best-effort: a missing `git`/`date` degrades to "unknown" rather
// than failing the build (so a source tarball without a .git still compiles).

use aterm_digest::Sha256;
use std::path::PathBuf;
use std::process::Command;

/// Fixed 64-byte lowercase record used only by unpinned development builds.
/// A release always has a non-empty committed pin
/// (`aterm_update_core::pins::update_channel_signing_pubkey`), so the release
/// cutter compares the Mach-O record against the permanent authority and
/// rejects this sentinel.
const UNPINNED_UPDATE_PIN_SENTINEL: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

// Shared with the test suite: `main.rs` mounts the same file as a #[cfg(test)]
// module, so the -vV parser and the flavor classifier are tested against real
// fixtures while build.rs uses the identical code.
include!("src/compiler_probe.rs");

/// A build fact supplied by the caller instead of probed from git. Empty is
/// treated as absent, so `FOO=` cannot half-pin a build.
fn pinned(key: &str) -> Option<String> {
    println!("cargo:rerun-if-env-changed={key}");
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn main() {
    // ONE NAME FOR "the accessibility tree is compiled in". It is a FEATURE on
    // macOS/Windows (where the platform has its own surface, or the weight is
    // genuinely optional) and a PLATFORM FACT on Linux, where AccessKit is the
    // only path to AT-SPI and therefore to a screen reader. Folding both here
    // keeps 70-odd cfg sites from having to spell the policy — and keeps the
    // policy in one place when it changes again.
    println!("cargo::rustc-check-cfg=cfg(a11y_tree)");
    let a11y_feature = std::env::var_os("CARGO_FEATURE_A11Y_ACCESSKIT").is_some();
    let linux = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux");
    if a11y_feature || linux {
        println!("cargo::rustc-cfg=a11y_tree");
    }

    // Derive the updater-key fingerprint from the SAME compile-time input that
    // `aterm-update::PINNED_UPDATE_PUBKEY` consumes.  `build_info.rs` places this
    // exact fixed-width value in `__DATA,__aterm_upin`, allowing the release
    // cutter to prove the x86_64 slice's authority without executing it under
    // Rosetta.  Invalid non-empty inputs fail the build instead of embedding an
    // ambiguous record; an ordinary unpinned dev build gets the explicit zero
    // sentinel and remains updater-inert.
    // The anchor is the COMMITTED constant, not build-environment state: read it
    // from the one file that owns it so the embedded record proves the same key the
    // runtime verifies under. Reading an env var here would reintroduce exactly the
    // drift this record exists to detect — a binary whose embedded pin disagrees
    // with the anchor it actually trusts.
    let update_pin_sha256 = match aterm_update_core::pins::update_channel_signing_pubkey() {
        encoded if !encoded.is_empty() => {
            let raw = aterm_codec::base64::decode_strict(encoded.as_bytes())
                .expect("UPDATE_CHANNEL_PUBKEYS[0] must be standard base64");
            assert_eq!(
                raw.len(),
                32,
                "UPDATE_CHANNEL_PUBKEYS[0] must decode to an Ed25519 32-byte public key"
            );
            Sha256::digest(raw)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        }
        _ => UNPINNED_UPDATE_PIN_SENTINEL.to_string(),
    };
    println!("cargo:rustc-env=ATERM_UPDATE_PIN_SHA256={update_pin_sha256}");

    // THE PIN (2026-09-21). A caller that already knows these facts — the gate,
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
    let full_commit = pinned_full_commit
        .unwrap_or_else(|| run("git", &["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into()));
    let dirty = commit.ends_with("-dirty");
    println!("cargo:rustc-env=ATERM_GIT_COMMIT_FULL={full_commit}");
    println!("cargo:rustc-env=ATERM_GIT_DIRTY={dirty}");
    println!(
        "cargo:rustc-env=ATERM_BINARY_TARGET={}",
        std::env::var("TARGET").expect("Cargo target")
    );

    // THE DEV COUNTER (owner, 2026-08-16: dev builds are identified by "the
    // 3rd developer number and the hash", never by advancing the version):
    // commits since the newest release tag, baked so the menu bar can show
    // `v0.21.<N>+g<sha> · DEV` for a development build while a release shows
    // its clean `v0.21.0`. Releases keep the third slot at literal 0, so a
    // nonzero third component is an unambiguous dev signature. "0" when git
    // or the tag is unavailable (a source-tarball build still marks DEV via
    // the release-env discriminator; only the counter degrades).
    let dev_commits = pinned_dev_commits.clone().unwrap_or_else(|| {
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
    // read — it is the release PIN. `cargo ship cut` (crates/aterm-release) claims the
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
        Ok(epoch) if !epoch.is_empty() => run("date", &["-u", "-r", &epoch, "+%Y-%m-%dT%H:%M:%SZ"])
            .or_else(|| {
                run(
                    "date",
                    &["-u", "-d", &format!("@{epoch}"), "+%Y-%m-%dT%H:%M:%SZ"],
                )
            }),
        _ => run("date", &["-u", "+%Y-%m-%dT%H:%M:%SZ"]),
    }
    .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=ATERM_BUILD_TIME={build_time}");

    // Compiler provenance: interrogate the ACTUAL compiler the build driver is
    // about to use (cargo and targo both set RUSTC for build scripts — targo's
    // custom_build lane keeps the env name for drop-in compatibility; bare
    // "rustc" is the no-driver fallback) rather than trusting whatever is first
    // on PATH — per-binary provenance is the whole point when upstream Rust and
    // the Trust toolchain (trustc) coexist on one machine.
    let compiler_path = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let vv = run(&compiler_path, &["-vV"]).unwrap_or_default();
    let compiler = parse_rustc_vv(&vv);
    // Flavor ('r' = upstream Rust, 't' = Trust fork), in priority order:
    //   1. the -vV self-identification ('binary: trustc' / '(trustc)' version line
    //      — the 2026-07 toolchains stamp both; covers the lane where a bare
    //      PATH-resolved `rustc` sets no env hint at all);
    //   2. RUSTC path containing '/trust/' (the fork lives in $HOME/trust/build/...);
    //   3. RUSTUP_TOOLCHAIN == 'trust' (a linked `rustup run trust` lane);
    //   4. default 'r'.
    // NOT inferred from '-dev' in the release — any local rustc build reports -dev.
    // There is NO env override: an ATERM_COMPILER_FLAVOR that outranked the
    // compiler's own self-report let a shell claim trustc provenance for a
    // binary upstream rustc built, in the one field About and `ctl version` ship
    // AS evidence. The three signals above cover every lane it was added for.
    let toolchain = std::env::var("RUSTUP_TOOLCHAIN").ok();
    let flavor = detect_flavor(&vv, &compiler_path, toolchain.as_deref());
    println!(
        "cargo:rustc-env=ATERM_COMPILER_VERSION_LINE={}",
        compiler.version_line
    );
    println!("cargo:rustc-env=ATERM_COMPILER_COMMIT={}", compiler.commit);
    println!("cargo:rustc-env=ATERM_COMPILER_HOST={}", compiler.host);
    // Trust's OWN version (the `trust:` -vV line, e.g. 0.1.0) — "" when the
    // compiler reports none. Distinct from the rustc-shaped release token in the
    // version line, which is the Rust release Trust is compatible with.
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
    // About and `aterm ctl version`. See `trust_verify_state` — the `--print cfg`
    // probe alone answers this WRONG on a Trust toolchain.
    println!(
        "cargo:rustc-env=ATERM_TRUST_VERIFY={}",
        trust_verify_state()
    );
    println!("cargo:rerun-if-env-changed=CARGO_ENCODED_RUSTFLAGS");
    println!("cargo:rerun-if-env-changed=RUSTC");
    println!("cargo:rerun-if-env-changed=RUSTUP_TOOLCHAIN");

    // Re-stamp when HEAD moves (new commit / checkout) or the workspace source
    // version changes. The release build number comes from SOURCE_DATE_EPOCH;
    // ordinary builds fall back to HEAD's committer epoch. Cargo.toml is two
    // levels up from this manifest; the git paths are asked of git.
    //
    // 2026-09-13: these were the literal `../../.git/HEAD` and `../../.git/index`.
    // In a linked worktree `.git` is a FILE, so neither path ever existed and cargo
    // — which treats a missing watched path as changed — reran this script and
    // relinked aterm-gui on EVERY build (measured: a no-change rebuild compiled
    // aterm-gui again). In the main checkout the index watch did the same
    // whenever `git status` refreshed the index. The index is not watched at all
    // now: the stamp needs HEAD, and the dirty flag is still computed whenever
    // the script runs (it was already stale for unstaged edits).
    // A PINNED BUILD WATCHES NO GIT PATH AT ALL, and that is the fix rather than
    // a side effect: the watch below names a path in the COMMON git dir, which
    // for any linked worktree is the main checkout's `.git`. See
    // `git_watch_paths`.
    if git_is_pinned {
        println!("cargo:rerun-if-env-changed=ATERM_BUILD_GIT_COMMIT");
        println!("cargo:rerun-if-env-changed=ATERM_BUILD_GIT_COMMIT_FULL");
        println!("cargo:rerun-if-env-changed=ATERM_BUILD_DEV_COMMITS");
    } else {
        for path in git_watch_paths() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
    println!("cargo:rerun-if-changed=../../Cargo.toml");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");

    // Windows: compile + link the application icon into the DEV `aterm-gui.exe`
    // so the taskbar button, Alt-Tab, titlebar and Explorer show the aterm icon
    // instead of the generic exe glyph (the shipped `aterm.exe` gets its
    // resources from crates/aterm/build.rs — link args reach only the emitting
    // package's own bins). Gated on the TARGET os (build scripts run on the
    // host, so `cfg!` would reflect the wrong platform); a missing resource
    // compiler is a warning rather than a failure, so a toolchain-less build
    // still works. No-op on every non-Windows target.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=assets/aterm.ico");
        let outcome = aterm_winres::compile(std::path::Path::new("assets/aterm.rc"));
        if !outcome.is_linked() {
            println!("cargo:warning=aterm-gui: window icon not embedded: {outcome}");
        }
    }
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
/// WHAT THIS WATCH COSTS WHEN IT IS NOT PINNED (measured 2026-09-20). The paths
/// below hang off the COMMON git dir, and for a linked worktree that is the MAIN
/// checkout's `.git` — this machine has 29 worktrees sharing one. So a `fetch`,
/// `gc` or `pack-refs` in ANY of them rewrites `packed-refs` and invalidates
/// this crate's compile in ALL of them, including inside a running gate: the
/// file's mtime landed between two builds of one gate run. The stamp really can
/// change with those files (`git describe --tags` reads them), so the watch is
/// not wrong — it is unpinnable from inside a build script. `ATERM_BUILD_GIT_COMMIT`
/// with `ATERM_BUILD_GIT_COMMIT_FULL` and `ATERM_BUILD_DEV_COMMITS` are the answer: a caller that has already
/// resolved the facts passes them, and then neither the probe nor this watch runs.
fn git_watch_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let Some(git_dir) = run("git", &["rev-parse", "--absolute-git-dir"]).map(PathBuf::from) else {
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

/// `"on"` iff this compile really runs the Trust verification pipeline.
///
/// `CARGO_CFG_TRUST_VERIFY` alone is NOT the answer, and trusting it shipped a
/// false provenance line for months. Cargo derives every `CARGO_CFG_*` from a
/// `rustc --print cfg` probe, and targo deliberately strips the `-Ztrust-verify`
/// family from that probe — so on a Trust toolchain the cfg reports the
/// compiler's DEFAULT (verification on) while every real unit is compiled with
/// `-Ztrust-verify=off` from `.cargo/config.toml`. Printing that as "on" claims
/// a hardening the binary does not carry, which is exactly what the honesty
/// ratchet forbids.
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
