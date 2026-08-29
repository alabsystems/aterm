// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Build plan (release spec §6 `buildplan.rs`): per-arch cargo builds of
//! aterm-gui + atpkg + aterm-ctl + aterm-cli with `SOURCE_DATE_EPOCH=n` as the SOLE
//! build-number conduit (zero build.rs changes — spec decision 11), `lipo` to
//! universal (single-arch pass-through under `--arm64-only`), then dSYM
//! (success judged by DWARF file existence — inherited dsymutil exit-code
//! caveat — plus UUID match), `strip -x` on shipped copies, and the dSYM zip.
//!
//! ONE compiler lane (owner decision, 2026-07): the repo's rust-toolchain.toml
//! pins the Trust toolchain and .cargo/config.toml carries the single
//! documented verification opt-out, so the native slice here is a PLAIN
//! `cargo build` — no `RUSTC=…`, no `RUSTC_BOOTSTRAP`, no RUSTFLAGS
//! surgery, no `--no-trust` escape hatch. Dev builds, tests, and the shipped
//! native slice are byte-for-byte the same lane. The build hard-fails unless
//! the produced binary self-reports the `+t` flavor (see [`run`]'s provenance
//! gate): a release that silently fell back to upstream must be impossible.
//!
//! The ONE exception: the x86_64-apple-darwin compat slice of the universal
//! binary rides upstream stable via `RUSTUP_TOOLCHAIN=stable`. The reason is
//! NOT that Trust lacks an x86_64 std — it has one, and six ALab programs ship
//! x86_64 artifacts built with it. What a CROSS-HOST Trust sysroot lacks is
//! rustc_private, so an out-of-tree rustc-driver tool cannot link against it;
//! that is a narrower gap than "no std", and it is why the rustc coherence
//! group is still aarch64-only while the plain programs are not. The compat
//! slice rides stable because this lane wants no Trust-specific state on it at
//! all. That pin lives HERE and nowhere else, and the lane scrubs
//! inherited RUSTC/RUSTFLAGS state so stale shell exports cannot steer it.
//!
//! Preserved semantics:
//!   * `SOURCE_DATE_EPOCH` inherited by every cargo child so the binary's
//!     ATERM_BUILD_NUMBER == the plist CFBundleVersion == the manifest
//!     build_number, from ONE in-process u64 (spec §2 "propagation");
//!   * dsymutil success judged by the DWARF file's existence, NOT its exit
//!     code (see [`extract_dsym`]);
//!   * a failed cargo build is a hard error — a release artifact with a
//!     silently missing arch slice or missing atpkg/aterm-ctl/aterm-cli must
//!     be impossible.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// THE shipped binary: `(cargo package, cargo bin name, basename it ships
/// under)` — all three are `aterm`. One binary is the whole surface: the
/// window (a no-TTY/Finder launch), the transparent session (a TTY launch),
/// and every verb (`aterm ctl/pkg/fleet/drive/help`) in-process. The bundle
/// adds argv0 compat SYMLINKS (aterm-ctl, atpkg, aterm-fleet, aterm-drive,
/// aterm-gui, aterm-cli) pointing at it — see `bundle::assemble` — so
/// pre-one-binary scripts, installs, and Help examples keep resolving.
const PACKAGES: [(&str, &str, &str); 1] = [("aterm", "aterm", "aterm")];

const ARM64: &str = "aarch64-apple-darwin";
const X86_64: &str = "x86_64-apple-darwin";
const LIPO_ARM64: &str = "arm64";
const LIPO_X86_64: &str = "x86_64";

/// Everything [`run`] needs, resolved by the caller (cli/gates own flag
/// parsing and the ledger claim; this module only builds).
pub struct BuildPlan {
    /// Workspace root — every child process runs with this as its cwd.
    pub repo_root: PathBuf,
    /// `dist/` — receives the dSYM + dSYM zip (the .app lands there later,
    /// via `bundle::assemble`).
    pub out_dir: PathBuf,
    /// The claimed ledger number `n`, exported as `SOURCE_DATE_EPOCH` to every
    /// cargo child (→ ATERM_BUILD_NUMBER + ATERM_BUILD_TIME via the untouched
    /// build.rs — spec decision 11).
    pub build_number: u64,
    /// Release version ("0.2.0") — names the dSYM zip `aterm-0.2.0-dSYM.zip`.
    pub short_version: String,
    /// `--arm64-only`: skip the x86_64 slice (single-arch pass-through
    /// instead of lipo). The universal build is the default (spec decision 18).
    pub arm64_only: bool,
    /// Exact lowercase SHA-256 fingerprint that every independently-built and
    /// final shipped architecture slice must embed for its compiled updater
    /// key. `None` exists only for unsigned development/rehearsal fixtures.
    pub expected_update_pin_sha256: Option<String>,
}

/// What [`run`] produced: the stripped, ship-ready universal binaries plus the
/// dSYM artifacts. Paths are the copies `bundle::assemble` should place in
/// `Contents/MacOS` verbatim (already `strip -x`ed; symbols live in the dSYM,
/// matched by the Mach-O UUID, which strip preserves).
pub struct BuildOutput {
    /// Stripped universal `aterm` (the shipped GUI binary).
    pub aterm: PathBuf,
    /// `lipo -archs` of the shipped `aterm` (e.g. "x86_64 arm64") — for the
    /// cut transcript and a universal/single-arch sanity print.
    pub archs: String,
    /// The built binary's `--diagnose` `compiler:` line — provenance for the
    /// cut transcript (`rustc <release> (<slug>) · trust · release ·
    /// trust_verify …`). Always Trust-flavor: [`run`] hard-fails otherwise.
    pub compiler_line: String,
    /// `dist/aterm.dSYM` — present iff dsymutil produced a non-empty DWARF
    /// whose UUIDs match the binary (see the exit-code caveat on [`extract_dsym`]).
    pub dsym: Option<PathBuf>,
    /// `dist/aterm-<ver>-dSYM.zip` — the archive attached to the release.
    pub dsym_zip: Option<PathBuf>,
}

/// Run the whole build phase: per-arch cargo builds → lipo → dsymutil →
/// strip → dSYM zip. Returns the ship-ready binaries.
pub fn run(plan: &BuildPlan) -> Result<BuildOutput, String> {
    // dist/ receives the dSYM below; create it up front (and keep the build
    // OUTPUT out of Spotlight — see bundle.rs for the full WHY; touching the
    // marker here too keeps a `--resume` that re-enters mid-pipeline covered).
    std::fs::create_dir_all(&plan.out_dir)
        .map_err(|e| format!("create {}: {e}", plan.out_dir.display()))?;
    let _ = std::fs::write(plan.out_dir.join(".metadata_never_index"), "");

    // --- per-arch builds --------------------------------------------------
    // arm64 first (the native slice), then x86_64 unless --arm64-only.
    //
    // The native slice is a PLAIN cargo build: rust-toolchain.toml pins the
    // Trust toolchain and .cargo/config.toml carries the one verification
    // opt-out, so THE dev/test lane is the ship lane. No --target on purpose
    // (host proc-macros and build scripts ride the same config rustflags).
    // Provenance is hard-gated below: the built binary must self-report +t.
    let mut slices: Vec<Vec<PathBuf>> = vec![Vec::new(); PACKAGES.len()];
    let t = Instant::now();
    println!(
        "==> [{ARM64}] Trust toolchain (+t): native build (SOURCE_DATE_EPOCH={})",
        plan.build_number
    );
    let mut built: Vec<&str> = Vec::new();
    for (i, (pkg, bin, _)) in PACKAGES.iter().enumerate() {
        // One crate can ship several bins (aterm-agent → fleet + drive);
        // `cargo build -p` produces them all, so build each package once.
        if !built.contains(pkg) {
            build_one(plan, pkg, None)?;
            built.push(pkg);
        }
        // Native build (no --target) → artifacts under target/release.
        slices[i].push(plan.repo_root.join("target/release").join(bin));
    }
    println!("    arm64 done in {}", fmt_elapsed(t));

    if !plan.arm64_only {
        // x86_64 compat slice: upstream stable via rustup's target std — THE
        // one exception to the single Trust lane; see the module docs for why
        // (it is NOT that Trust lacks an x86_64 std; it has one). NOT auto-added here: spec
        // decision 18 — print the remediation and require an explicit
        // --arm64-only to ship single-arch.
        let t = Instant::now();
        println!("==> [{X86_64}] upstream stable (+r): --target compat slice");
        require_rustup_target(&plan.repo_root, X86_64)?;
        let mut built: Vec<&str> = Vec::new();
        for (i, (pkg, bin, _)) in PACKAGES.iter().enumerate() {
            if !built.contains(pkg) {
                build_one(plan, pkg, Some(X86_64))?;
                built.push(pkg);
            }
            slices[i].push(target_bin(&plan.repo_root, X86_64, bin));
        }
        println!("    x86_64 done in {}", fmt_elapsed(t));
    }

    if let Some(expected) = &plan.expected_update_pin_sha256 {
        verify_built_slice_update_pins(&plan.repo_root, &slices[0], expected)?;
        println!(
            "    updater pin: every architecture slice embeds {}…",
            &expected[..12]
        );
    }

    // --- lipo to universal (single-arch pass-through) ----------------------
    let universal = plan.repo_root.join("target/universal");
    std::fs::create_dir_all(&universal)
        .map_err(|e| format!("create {}: {e}", universal.display()))?;
    let mut fat: Vec<PathBuf> = Vec::new();
    for (i, (_, _, ship_name)) in PACKAGES.iter().enumerate() {
        let out = universal.join(ship_name);
        lipo_or_copy(&slices[i], &out)?;
        fat.push(out);
    }
    let archs = lipo_archs(&fat[0])?;
    if plan.arm64_only {
        println!("    single-arch binary ({archs}) — NOT universal (--arm64-only)");
    } else {
        println!("    universal binary: {archs}");
    }

    // --- dSYM from the UN-stripped binary ----------------------------------
    let (dsym, dsym_zip) =
        extract_dsym(&fat[0], &plan.out_dir, &plan.short_version, &plan.repo_root)?;

    // --- strip the SHIPPED copies ------------------------------------------
    // Symbols live in the archived .dSYM (matched by the Mach-O UUID, which
    // strip preserves); the bundle binaries stay small while crash reports
    // remain symbolicatable. Stripping COPIES (target/universal/ship/) keeps
    // the unstripped originals for a later re-run of dsymutil under --resume.
    let ship_dir = universal.join("ship");
    std::fs::create_dir_all(&ship_dir)
        .map_err(|e| format!("create {}: {e}", ship_dir.display()))?;
    let mut shipped: Vec<PathBuf> = Vec::new();
    for (src, (_, _, ship_name)) in fat.iter().zip(PACKAGES.iter()) {
        let dst = ship_dir.join(ship_name);
        std::fs::copy(src, &dst).map_err(|e| format!("copy {}: {e}", src.display()))?;
        make_executable(&dst)?;
        // `strip -x`: local symbols only — matches build-app.sh; a failure here
        // is tolerated (`|| true` in the script) because an unstripped binary
        // is merely bigger, never wrong.
        let _ = Command::new("strip")
            .arg("-x")
            .arg(&dst)
            .current_dir(&plan.repo_root)
            .output();
        shipped.push(dst);
    }

    // Bind the ACTUAL post-strip bytes, not merely cargo's pre-lipo inputs.
    // A tolerated strip failure still reaches this mandatory proof; a successful
    // strip/lipo that drops or changes either architecture's dedicated section
    // fails closed. Fat slices are extracted as data and never executed.
    if let Some(expected) = &plan.expected_update_pin_sha256 {
        verify_shipped_update_pin_slices(&shipped[0], &archs, plan.arm64_only, expected)?;
        println!("    updater pin: every final shipped architecture structurally verified");
    }

    // --- compiler provenance HARD GATE ---------------------------------------
    // Ask the BUILT binary which compiler produced it: build.rs bakes the
    // `$RUSTC -vV` probe into `build_info::compiler_summary()`, and
    // `--diagnose` prints it as the `compiler:` line (`rustc <release>
    // (<slug>) · trust|rust · <profile> · trust_verify on|off`) — ground
    // truth for the shipped bytes. Single-lane invariant: the native slice
    // MUST be a Trust build (`· trust ·`). Anything else means the toolchain
    // file was bypassed (broken rustup link, stale env) — that is a broken
    // toolchain, not a fallback, so the cut refuses to continue.
    let diagnose = match Command::new(&shipped[0])
        .arg("--diagnose")
        .current_dir(&plan.repo_root)
        .output()
    {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).into_owned(),
        _ => {
            return Err(
                "compiler provenance probe failed: the built aterm binary did not answer \
                 --diagnose on this host"
                    .to_string(),
            );
        }
    };
    if let Some(expected) = &plan.expected_update_pin_sha256 {
        validate_slice_update_pin_reports(expected, &[("final universal binary", &diagnose)])?;
        println!("    updater pin: final universal runtime cross-check passed");
    }
    validate_app_version_reports(
        &plan.short_version,
        &[("final universal binary", &diagnose)],
    )?;
    let version_output = Command::new(&shipped[0])
        .arg("--version")
        .current_dir(&plan.repo_root)
        .output()
        .map_err(|error| format!("execute final universal binary --version: {error}"))?;
    if !version_output.status.success() {
        return Err(format!(
            "app-version probe failed: final universal binary --version exited {}",
            version_output.status
        ));
    }
    validate_cli_app_version(&plan.short_version, &version_output.stdout)?;
    println!(
        "    app version: {} (diagnostics + CLI identity gates passed)",
        plan.short_version
    );
    let compiler_line = diagnose
        .lines()
        .find_map(|l| l.strip_prefix("compiler:"))
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    if !compiler_line.contains("\u{00b7} trust \u{00b7}") {
        return Err(format!(
            "compiler provenance gate: the native slice reports {compiler_line:?} — not a \
             Trust-flavor build. The repo compiles with Trust always; the native lane must \
             have been driven by something other than the stage2 targo/trustc (stale env, \
             wrong TRUST_STAGE2_BIN?). Fix the toolchain resolution and recut"
        ));
    }
    println!("    compiler: {compiler_line}  (Trust provenance gate passed)");

    Ok(BuildOutput {
        aterm: shipped[0].clone(),
        archs,
        compiler_line,
        dsym,
        dsym_zip,
    })
}

/// One `cargo build --release -p <pkg>` invocation. `target` = None for the
/// native Trust slice (the toolchain file supplies the compiler);
/// `target` = Some(triple) for the upstream-stable compat slice.
///
/// Output is streamed (not captured): release builds run for minutes and the
/// operator needs cargo's own progress; on failure cargo has already printed
/// the errors, so the returned Err only names the step.
fn build_one(plan: &BuildPlan, pkg: &str, target: Option<&str>) -> Result<(), String> {
    // The build driver, per lane. Native slice: `targo` from the Trust stage2
    // tool dir — never a PATH `cargo`, which since the stock-name purge is a
    // rustup shim the repo's toolchain pin can no longer satisfy. Compat
    // slice: upstream stable's `cargo` via the rustup shim — the ONE
    // deliberately stock lane (see the module docs; Trust DOES have an
    // x86_64-apple-darwin std, so that is not the reason).
    let driver: (PathBuf, &'static str) = if target.is_some() {
        (PathBuf::from("cargo"), "cargo")
    } else {
        let stage2 = crate::gates::trust_stage2_bin().map_err(|e| e.to_string())?;
        (stage2.join("targo"), "targo")
    };
    let (driver_path, driver_name) = driver;
    let mut cmd = Command::new(&driver_path);
    cmd.current_dir(&plan.repo_root);
    // targo refuses a bare verb: every artifact is EXPLICITLY verified
    // (`targo trust build`) or explicitly not (`--unverified`). The workspace
    // rides the unverified lane until the Trust-Std campaign greens — the
    // same statement .cargo/config.toml's off-switch makes, now visible in
    // the invocation. The compat slice's cargo has no such flag.
    if target.is_none() {
        cmd.arg("--unverified");
    }
    cmd.args(["build", "--release", "--locked", "-p", pkg]);
    if let Some(triple) = target {
        cmd.args(["--target", triple]);
    }

    // Toolchain PATH, per lane. Native: the Trust stage2 bin dir first, so
    // targo resolves its co-located trustc/trustdoc (the physical dir —
    // protected Trust drivers refuse symlinked toolchain paths). Compat:
    // ~/.cargo/bin first so `cargo` resolves to the rustup shim and the
    // RUSTUP_TOOLCHAIN=stable below picks the toolchain that carries the
    // other Apple arch's std (port of build-app.sh's PATH preference; no-op
    // when rustup isn't installed).
    let old_path = std::env::var("PATH").unwrap_or_default();
    if target.is_some() {
        if let Some(home) = std::env::var_os("HOME") {
            let shim = Path::new(&home).join(".cargo/bin");
            if shim.join("rustup").is_file() {
                cmd.env("PATH", format!("{}:{}", shim.display(), old_path));
            }
        }
    } else if let Some(bin_dir) = driver_path.parent() {
        cmd.env("PATH", format!("{}:{}", bin_dir.display(), old_path));
    }

    // THE build-number conduit (spec §2 propagation): build.rs reads
    // SOURCE_DATE_EPOCH for ATERM_BUILD_NUMBER **and** ATERM_BUILD_TIME, and a
    // valid epoch WINS over its live-git fallback — so the binary, the plist
    // stamp, and the manifest all carry the one claimed u64.
    cmd.env("SOURCE_DATE_EPOCH", plan.build_number.to_string());

    // Real .dSYM: line-table debug info AND no stripping (the release
    // profile's strip=true would erase the debug map dsymutil follows).
    // Scoped to THIS build via cargo profile env overrides — the global
    // profile is unchanged. Unlike build-app.sh's `:-` defaults these are set
    // UNCONDITIONALLY: an ambient CARGO_PROFILE_RELEASE_STRIP=true would
    // silently kill the dSYM, and a release cutter must not be steerable by
    // stale shell state.
    cmd.env("CARGO_PROFILE_RELEASE_DEBUG", "1");
    cmd.env("CARGO_PROFILE_RELEASE_STRIP", "false");

    // NO trust anchors are injected into the child build. They used to arrive here
    // from ~/.aterm/release.conf so `option_env!` could bake them in, which made what
    // a binary trusts a property of the shell that compiled it. They are committed
    // constants now (`aterm_update_core::pins`) and the child compiles them in
    // directly; exporting them would recreate a second, disagreeing source.

    // Lane env. Native slice: NOTHING beyond the driver itself — the resolved
    // targo supplies trustc and .cargo/config.toml (which targo reads, same
    // discovery as cargo) the verification opt-out; adding env here would
    // create a second, undocumented lane. Compat slice: RUSTUP_TOOLCHAIN=stable
    // overrides the toolchain file (a deliberate stock lane, not a Trust
    // limitation — see the module docs), and inherited
    // RUSTC/RUSTC_BOOTSTRAP/RUSTFLAGS are scrubbed on BOTH lanes — a release
    // cutter must not be steerable by stale shell state (same rule as the
    // CARGO_PROFILE pins above).
    cmd.env_remove("RUSTC")
        .env_remove("RUSTC_BOOTSTRAP")
        .env_remove("RUSTFLAGS")
        .env_remove("RUSTUP_TOOLCHAIN")
        .env_remove("ATERM_APP_RELEASE_VERSION");
    // The value comes from the validated release context, never the ambient
    // environment or release.conf. Both architecture builds receive the same
    // exact app identity while Cargo.toml remains on its source-version line.
    cmd.env("ATERM_APP_RELEASE_VERSION", &plan.short_version);
    let lane = if target.is_some() {
        cmd.env("RUSTUP_TOOLCHAIN", "stable");
        "upstream stable (+r) compat"
    } else {
        "Trust (+t)"
    };

    println!(
        "==> {driver_name} build --release {}-p {pkg}  [{lane}]",
        target.map(|t| format!("--target {t} ")).unwrap_or_default()
    );
    let status = cmd
        .stdin(std::process::Stdio::null())
        .status()
        .map_err(|e| format!("spawn {driver_name} for {pkg}: {e}"))?;
    if !status.success() {
        // Hard error (release cutter): a warn-and-skip would let a release
        // ship with a missing slice or missing atpkg/aterm-ctl/aterm-cli.
        return Err(format!("{driver_name} build -p {pkg} failed ({status})"));
    }
    Ok(())
}

/// Refuse (with the exact remediation) when the stable toolchain's target std
/// is absent. Probes STABLE explicitly: the compat slice builds with
/// `RUSTUP_TOOLCHAIN=stable`, and a bare `rustup target list` here would
/// resolve the repo's `trust` toolchain (rust-toolchain.toml), which never
/// carries rustup-managed targets. Also refuses when rustup itself is missing.
fn require_rustup_target(repo_root: &Path, triple: &str) -> Result<(), String> {
    let out = Command::new("rustup")
        .env("RUSTUP_TOOLCHAIN", "stable")
        .args(["target", "list", "--installed"])
        .current_dir(repo_root)
        .output()
        .map_err(|e| format!("rustup not runnable ({e}) — install rustup or pass --arm64-only"))?;
    if !out.status.success() {
        return Err(format!(
            "rustup target list failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    if !String::from_utf8_lossy(&out.stdout)
        .lines()
        .any(|l| l.trim() == triple)
    {
        return Err(format!(
            "rust std for {triple} is not installed on stable — run \
             `rustup +stable target add {triple}` \
             (or pass --arm64-only to ship a single-arch build)"
        ));
    }
    Ok(())
}

const MACH_HEADER_64_LEN: u64 = 32;
const MACH_MAGIC_64: u32 = 0xfeed_facf;
const MH_EXECUTE: u32 = 2;
const LC_SEGMENT: u32 = 0x1;
const LC_SEGMENT_64: u32 = 0x19;
const SEGMENT_COMMAND_64_LEN: u64 = 72;
const SECTION_64_LEN: u64 = 80;
const CPU_TYPE_X86_64: u32 = 0x0100_0007;
const CPU_TYPE_ARM64: u32 = 0x0100_000c;
const UPDATE_PIN_SEGMENT: &[u8] = b"__DATA";
const UPDATE_PIN_SECTION: &[u8] = b"__aterm_upin";
const UPDATE_PIN_RECORD_LEN: u64 = 64;

fn canonical_sha256(value: &str) -> bool {
    value.len() == UPDATE_PIN_RECORD_LEN as usize
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().expect("fixed-width u32 field"))
}

fn le_u64(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes.try_into().expect("fixed-width u64 field"))
}

fn macho_name_eq(field: &[u8], expected: &[u8]) -> bool {
    field.len() == 16
        && field.get(..expected.len()) == Some(expected)
        && field[expected.len()..].iter().all(|byte| *byte == 0)
}

fn read_exact_macho<R: Read>(
    reader: &mut R,
    bytes: &mut [u8],
    description: &str,
) -> Result<(), String> {
    reader
        .read_exact(bytes)
        .map_err(|error| format!("read Mach-O {description}: {error}"))
}

/// Parse one THIN 64-bit Mach-O executable and return the sole dedicated
/// updater-authority record.  This intentionally understands the small Mach-O
/// surface we emit instead of searching arbitrary executable bytes: a matching
/// fingerprint elsewhere (a diagnostic literal, debug data, or an attacker-added
/// decoy) is never authority.
fn parse_thin_macho_update_pin<R: Read + Seek>(
    reader: &mut R,
    file_len: u64,
) -> Result<String, String> {
    if file_len < MACH_HEADER_64_LEN {
        return Err("thin Mach-O is shorter than its 64-bit header".into());
    }

    reader
        .seek(SeekFrom::Start(0))
        .map_err(|error| format!("seek Mach-O header: {error}"))?;
    let mut header = [0_u8; MACH_HEADER_64_LEN as usize];
    read_exact_macho(reader, &mut header, "64-bit header")?;
    if le_u32(&header[0..4]) != MACH_MAGIC_64 {
        return Err("updater-pin proof requires a thin little-endian 64-bit Mach-O".into());
    }
    let cpu_type = le_u32(&header[4..8]);
    if !matches!(cpu_type, CPU_TYPE_ARM64 | CPU_TYPE_X86_64) {
        return Err(format!(
            "updater-pin proof found unsupported Mach-O CPU type {cpu_type:#x}"
        ));
    }
    if le_u32(&header[12..16]) != MH_EXECUTE {
        return Err("updater-pin proof requires a Mach-O executable".into());
    }

    let command_count = u64::from(le_u32(&header[16..20]));
    let command_bytes = u64::from(le_u32(&header[20..24]));
    let commands_end = MACH_HEADER_64_LEN
        .checked_add(command_bytes)
        .ok_or_else(|| "Mach-O load-command range overflowed".to_string())?;
    if commands_end > file_len {
        return Err("Mach-O load-command table extends beyond the file".into());
    }
    if command_count > command_bytes / 8 {
        return Err("Mach-O load-command count cannot fit in sizeofcmds".into());
    }

    let mut command_offset = MACH_HEADER_64_LEN;
    let mut record_offset = None;
    for command_index in 0..command_count {
        if command_offset
            .checked_add(8)
            .is_none_or(|end| end > commands_end)
        {
            return Err(format!(
                "Mach-O load command {command_index} has no complete header"
            ));
        }
        reader
            .seek(SeekFrom::Start(command_offset))
            .map_err(|error| format!("seek Mach-O load command {command_index}: {error}"))?;
        let mut load_header = [0_u8; 8];
        read_exact_macho(reader, &mut load_header, "load-command header")?;
        let command = le_u32(&load_header[0..4]);
        let command_size = u64::from(le_u32(&load_header[4..8]));
        if command_size < 8 {
            return Err(format!(
                "Mach-O load command {command_index} has invalid size {command_size}"
            ));
        }
        let command_end = command_offset
            .checked_add(command_size)
            .ok_or_else(|| format!("Mach-O load command {command_index} range overflowed"))?;
        if command_end > commands_end {
            return Err(format!(
                "Mach-O load command {command_index} extends beyond sizeofcmds"
            ));
        }

        if command == LC_SEGMENT {
            return Err("64-bit Mach-O contains a 32-bit LC_SEGMENT command".into());
        }
        if command == LC_SEGMENT_64 {
            if command_size < SEGMENT_COMMAND_64_LEN {
                return Err(format!(
                    "Mach-O LC_SEGMENT_64 command {command_index} is too short"
                ));
            }
            let mut segment = [0_u8; (SEGMENT_COMMAND_64_LEN - 8) as usize];
            read_exact_macho(reader, &mut segment, "LC_SEGMENT_64 body")?;
            let segment_file_offset = le_u64(&segment[32..40]);
            let segment_file_size = le_u64(&segment[40..48]);
            let segment_file_end = segment_file_offset
                .checked_add(segment_file_size)
                .ok_or_else(|| "Mach-O segment file range overflowed".to_string())?;
            if segment_file_end > file_len {
                return Err("Mach-O segment extends beyond the file".into());
            }
            let section_count = u64::from(le_u32(&segment[56..60]));
            let required_size = SEGMENT_COMMAND_64_LEN
                .checked_add(
                    section_count
                        .checked_mul(SECTION_64_LEN)
                        .ok_or_else(|| "Mach-O section table size overflowed".to_string())?,
                )
                .ok_or_else(|| "Mach-O segment-command size overflowed".to_string())?;
            if required_size > command_size {
                return Err(format!(
                    "Mach-O segment command {command_index} cannot contain its {section_count} sections"
                ));
            }

            for section_index in 0..section_count {
                let mut section = [0_u8; SECTION_64_LEN as usize];
                read_exact_macho(reader, &mut section, "section_64 record")?;
                if !macho_name_eq(&section[0..16], UPDATE_PIN_SECTION) {
                    continue;
                }
                if record_offset.is_some() {
                    return Err("Mach-O contains duplicate __aterm_upin sections".into());
                }
                if !macho_name_eq(&segment[0..16], UPDATE_PIN_SEGMENT)
                    || !macho_name_eq(&section[16..32], UPDATE_PIN_SEGMENT)
                {
                    return Err(format!(
                        "Mach-O __aterm_upin section {section_index} is not in __DATA"
                    ));
                }
                let section_size = le_u64(&section[40..48]);
                if section_size != UPDATE_PIN_RECORD_LEN {
                    return Err(format!(
                        "Mach-O __aterm_upin section has length {section_size}, expected {UPDATE_PIN_RECORD_LEN}"
                    ));
                }
                if le_u32(&section[64..68]) & 0xff != 0 {
                    return Err(
                        "Mach-O __aterm_upin section is not file-backed S_REGULAR data".into(),
                    );
                }
                let section_offset = u64::from(le_u32(&section[48..52]));
                let section_end = section_offset
                    .checked_add(section_size)
                    .ok_or_else(|| "Mach-O updater-pin section range overflowed".to_string())?;
                if section_offset < commands_end || section_end > file_len {
                    return Err("Mach-O updater-pin section points outside file-backed data".into());
                }
                if section_offset < segment_file_offset || section_end > segment_file_end {
                    return Err("Mach-O updater-pin section lies outside its segment".into());
                }
                record_offset = Some(section_offset);
            }
        }

        command_offset = command_end;
    }
    if command_offset != commands_end {
        return Err("Mach-O sizeofcmds contains unclaimed trailing bytes".into());
    }

    let record_offset =
        record_offset.ok_or_else(|| "Mach-O is missing __DATA,__aterm_upin".to_string())?;
    reader
        .seek(SeekFrom::Start(record_offset))
        .map_err(|error| format!("seek Mach-O updater-pin record: {error}"))?;
    let mut record = [0_u8; UPDATE_PIN_RECORD_LEN as usize];
    read_exact_macho(reader, &mut record, "updater-pin record")?;
    let observed = std::str::from_utf8(&record)
        .map_err(|_| "Mach-O updater-pin record is not UTF-8".to_string())?;
    if !canonical_sha256(observed) {
        return Err("Mach-O updater-pin record is not canonical lowercase SHA-256".into());
    }
    Ok(observed.to_string())
}

fn read_thin_macho_update_pin(path: &Path) -> Result<String, String> {
    let mut file = File::open(path)
        .map_err(|error| format!("open architecture slice {}: {error}", path.display()))?;
    let file_len = file
        .metadata()
        .map_err(|error| format!("stat architecture slice {}: {error}", path.display()))?
        .len();
    parse_thin_macho_update_pin(&mut file, file_len)
        .map_err(|error| format!("architecture slice {}: {error}", path.display()))
}

fn validate_embedded_update_pin(expected: &str, observed: &str, label: &str) -> Result<(), String> {
    if !canonical_sha256(expected) {
        return Err("expected updater-pin fingerprint is not canonical lowercase SHA-256".into());
    }
    if !canonical_sha256(observed) || observed != expected {
        return Err(format!(
            "architecture slice {label} embedded updater pin {observed:?} differs from the permanent authority"
        ));
    }
    Ok(())
}

fn expected_lipo_architectures(arm64_only: bool) -> &'static [&'static str] {
    if arm64_only {
        &[LIPO_ARM64]
    } else {
        &[LIPO_ARM64, LIPO_X86_64]
    }
}

fn validate_lipo_architectures(archs: &str, arm64_only: bool) -> Result<Vec<&str>, String> {
    let observed: Vec<&str> = archs.split_whitespace().collect();
    let expected = expected_lipo_architectures(arm64_only);
    if observed.len() != expected.len()
        || expected
            .iter()
            .any(|required| observed.iter().filter(|arch| *arch == required).count() != 1)
        || observed.iter().any(|arch| !expected.contains(arch))
    {
        return Err(format!(
            "shipped Mach-O architectures {observed:?} differ from required {expected:?}"
        ));
    }
    Ok(observed)
}

fn validate_final_slice_records(
    expected_pin: &str,
    required_architectures: &[&str],
    records: &[(&str, &str)],
) -> Result<(), String> {
    if records.len() != required_architectures.len() {
        return Err(format!(
            "final shipped updater-pin proof supplied {} records for {} architectures",
            records.len(),
            required_architectures.len()
        ));
    }
    for required in required_architectures {
        let matching: Vec<&str> = records
            .iter()
            .filter_map(|(architecture, record)| (*architecture == *required).then_some(*record))
            .collect();
        let [record] = matching.as_slice() else {
            return Err(format!(
                "final shipped architecture {required} has {} updater-pin records; expected exactly one",
                matching.len()
            ));
        };
        validate_embedded_update_pin(expected_pin, record, required)?;
    }
    if records
        .iter()
        .any(|(architecture, _)| !required_architectures.contains(architecture))
    {
        return Err("final shipped updater-pin proof contains an unexpected architecture".into());
    }
    Ok(())
}

static TEMP_SLICE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct PrivateSliceDir(PathBuf);

impl PrivateSliceDir {
    fn create() -> Result<Self, String> {
        for _ in 0..128 {
            let sequence = TEMP_SLICE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "aterm-update-pin-proof-{}-{sequence}",
                std::process::id()
            ));
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt as _;
                builder.mode(0o700);
            }
            match builder.create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(format!(
                        "create private updater-pin proof directory: {error}"
                    ));
                }
            }
        }
        Err("could not allocate a unique updater-pin proof directory".into())
    }
}

impl Drop for PrivateSliceDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn verify_shipped_update_pin_slices(
    shipped: &Path,
    archs: &str,
    arm64_only: bool,
    expected_pin: &str,
) -> Result<(), String> {
    let architectures = validate_lipo_architectures(archs, arm64_only)?;
    let mut owned_records = Vec::with_capacity(architectures.len());

    if architectures.len() == 1 {
        owned_records.push((
            architectures[0].to_string(),
            read_thin_macho_update_pin(shipped)?,
        ));
    } else {
        let temp = PrivateSliceDir::create()?;
        for architecture in &architectures {
            // `architecture` came through the exact allowlist above, so it is
            // both a safe filename component and a valid lipo selector.
            let thin = temp.0.join(architecture);
            let mut command = Command::new("lipo");
            command
                .arg(shipped)
                .args(["-thin", architecture, "-output"])
                .arg(&thin);
            run_quiet(
                command,
                &format!("lipo -thin {architecture} for updater-pin proof"),
            )?;
            owned_records.push((
                (*architecture).to_string(),
                read_thin_macho_update_pin(&thin)?,
            ));
        }
    }

    let records: Vec<(&str, &str)> = owned_records
        .iter()
        .map(|(architecture, record)| (architecture.as_str(), record.as_str()))
        .collect();
    validate_final_slice_records(
        expected_pin,
        expected_lipo_architectures(arm64_only),
        &records,
    )
}

/// Require every diagnostics report to carry exactly one app-version field
/// equal to the ledger-derived release identity.
pub fn validate_app_version_reports(
    expected: &str,
    reports: &[(&str, &str)],
) -> Result<(), String> {
    for (label, report) in reports {
        let fields: Vec<&str> = report
            .lines()
            .filter_map(|line| line.strip_prefix("version:"))
            .map(str::trim)
            .collect();
        let [field] = fields.as_slice() else {
            return Err(format!(
                "{label} reported {} diagnostics version fields; expected exactly one",
                fields.len()
            ));
        };
        let observed = field.split_once(" (").map(|(version, _)| version);
        if observed != Some(expected) {
            return Err(format!(
                "{label} diagnostics app version {observed:?} differs from claimed {expected:?}"
            ));
        }
    }
    Ok(())
}

/// Require the one-binary CLI identity to be exactly `aterm <claimed>`.
pub fn validate_cli_app_version(expected: &str, stdout: &[u8]) -> Result<(), String> {
    validate_named_cli_app_version("aterm", expected, stdout)
}

/// Require an argv0 alias identity to be exactly `<name> <claimed>` on LINE ONE.
///
/// `aterm --version` says which copy runs after its identity line (S12 of
/// `docs/DESIGN-which-copy-runs-2026-08-27.md`): `running: <path>` and, per other
/// `aterm.app` in the usual places, `another copy: …`. Those lines are path-dependent
/// by design — a staged universal binary names its own path — so the gate pins the
/// identity line byte for byte and admits ONLY the S12 lines after it: anything else
/// (a stale cached library slice's chatter, alias-routing drift) still fails.
pub fn validate_named_cli_app_version(
    name: &str,
    expected: &str,
    stdout: &[u8],
) -> Result<(), String> {
    let observed =
        std::str::from_utf8(stdout).map_err(|_| format!("{name} --version output is not UTF-8"))?;
    let wanted = format!("{name} {expected}\n");
    let Some(rest) = observed.strip_prefix(wanted.as_str()) else {
        return Err(format!(
            "{name} --version output {observed:?} does not open with {wanted:?}"
        ));
    };
    if !rest.is_empty() && !rest.ends_with('\n') {
        return Err(format!(
            "{name} --version output {observed:?} does not end with a newline"
        ));
    }
    for line in rest.split_terminator('\n') {
        if !WHICH_COPY_LINE_PREFIXES
            .iter()
            .any(|prefix| line.starts_with(prefix))
        {
            return Err(format!(
                "{name} --version output {observed:?} carries {line:?} after the identity \
                 line {wanted:?} — only the which-copy lines ({}) may follow it",
                WHICH_COPY_LINE_PREFIXES.join(", ")
            ));
        }
    }
    Ok(())
}

/// The only lines `aterm --version` may print after its identity line — the S12
/// "which copy runs" report, spelled by `aterm_update::which_copy::WhichCopy::lines`.
const WHICH_COPY_LINE_PREFIXES: &[&str] = &["running: ", "another copy: "];

/// Pure report validator used by the native-slice and final-universal runtime
/// cross-checks. Each report must contain exactly one stable diagnostics field
/// and independently equal the authority.
pub fn validate_slice_update_pin_reports(
    expected: &str,
    reports: &[(&str, &str)],
) -> Result<(), String> {
    if !canonical_sha256(expected) {
        return Err("expected updater-pin fingerprint is not canonical lowercase SHA-256".into());
    }
    if reports.is_empty() {
        return Err("no architecture slice diagnostics were supplied".into());
    }
    for (label, report) in reports {
        let observed: Vec<&str> = report
            .lines()
            .filter_map(|line| line.strip_prefix("update-pin-sha256: "))
            .collect();
        let [observed] = observed.as_slice() else {
            return Err(format!(
                "architecture slice {label} reported {} update-pin-sha256 fields; expected exactly one",
                observed.len()
            ));
        };
        if !canonical_sha256(observed) || *observed != expected {
            return Err(format!(
                "architecture slice {label} updater pin {observed:?} differs from the permanent authority"
            ));
        }
    }
    Ok(())
}

fn verify_built_slice_update_pins(
    repo_root: &Path,
    slices: &[PathBuf],
    expected: &str,
) -> Result<(), String> {
    if slices.is_empty() {
        return Err("no architecture slices were supplied for updater-pin proof".into());
    }

    // Structural proof for EVERY thin slice.  In particular, the x86_64 path
    // is read as data and is never executed, so this gate works without Rosetta.
    for slice in slices {
        let observed = read_thin_macho_update_pin(slice)?;
        validate_embedded_update_pin(expected, &observed, &slice.display().to_string())?;
    }

    // Independent executable conformance for the native slice.  The final
    // stripped universal is checked again by `run`; neither probe executes the
    // x86_64 compatibility slice.
    let native = &slices[0];
    let output = Command::new(native)
        .arg("--diagnose")
        .current_dir(repo_root)
        .output()
        .map_err(|error| {
            format!(
                "execute native architecture slice {} for updater-pin proof: {error}",
                native.display()
            )
        })?;
    if !output.status.success() {
        return Err(format!(
            "native architecture slice {} --diagnose exited {}",
            native.display(),
            output.status
        ));
    }
    let report = String::from_utf8(output.stdout).map_err(|_| {
        format!(
            "native architecture slice {} diagnostics are not UTF-8",
            native.display()
        )
    })?;
    validate_slice_update_pin_reports(expected, &[("native architecture slice", &report)])
}

/// Combine slices into one fat binary, or pass a single slice through
/// (build-app.sh's lipo step, incl. the single-arch copy branch).
fn lipo_or_copy(slices: &[PathBuf], out: &Path) -> Result<(), String> {
    match slices {
        [] => Err("no architecture built".into()), // unreachable: builds hard-fail
        [one] => {
            std::fs::copy(one, out).map_err(|e| format!("copy {}: {e}", one.display()))?;
            make_executable(out)
        }
        many => {
            let mut cmd = Command::new("lipo");
            cmd.arg("-create").args(many).arg("-output").arg(out);
            run_quiet(cmd, "lipo -create")?;
            make_executable(out)
        }
    }
}

/// `lipo -archs <bin>` → e.g. "x86_64 arm64".
fn lipo_archs(bin: &Path) -> Result<String, String> {
    let out = Command::new("lipo")
        .arg("-archs")
        .arg(bin)
        .output()
        .map_err(|e| format!("spawn lipo -archs: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "lipo -archs failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Extract `dist/aterm.dSYM` from the UN-stripped universal binary, then zip
/// it as `dist/aterm-<ver>-dSYM.zip` for the release assets.
///
/// EXIT-CODE CAVEAT (inherited from build-app.sh, preserved on purpose):
/// dsymutil exits non-zero on harmless "unable to open object file" warnings
/// (cargo's deleted intermediate .o's), so success is judged by the DWARF
/// file's existence and non-emptiness, NOT by the exit code. A missing/empty
/// dSYM is a WARNING, not an abort — the release still ships, crash reports
/// just won't symbolicate (same tolerance as the script).
fn extract_dsym(
    bin: &Path,
    out_dir: &Path,
    short_version: &str,
    repo_root: &Path,
) -> Result<(Option<PathBuf>, Option<PathBuf>), String> {
    let dsym = out_dir.join("aterm.dSYM");
    let _ = std::fs::remove_dir_all(&dsym);
    // Exit code + output deliberately ignored (see caveat above).
    let _ = Command::new("dsymutil")
        .arg(bin)
        .arg("-o")
        .arg(&dsym)
        .current_dir(repo_root)
        .output();

    let dwarf = dsym.join("Contents/Resources/DWARF/aterm");
    let ok = std::fs::metadata(&dwarf)
        .map(|m| m.len() > 0)
        .unwrap_or(false);
    if !ok {
        println!("    WARNING: .dSYM empty/failed (crash reports won't symbolicate)");
        return Ok((None, None));
    }

    // UUID match (spec §6): the dSYM only symbolicates if its per-arch UUIDs
    // cover the binary's (strip preserves the UUID, so checking the unstripped
    // original vouches for the shipped copy too). Best-effort: a missing
    // dwarfdump skips the check rather than failing the cut.
    match (dwarf_uuids(bin), dwarf_uuids(&dwarf)) {
        (Some(bin_uuids), Some(dsym_uuids)) => {
            if !bin_uuids.iter().all(|u| dsym_uuids.contains(u)) {
                println!(
                    "    WARNING: dSYM UUIDs {dsym_uuids:?} don't cover binary UUIDs {bin_uuids:?} — discarding dSYM"
                );
                return Ok((None, None));
            }
            println!("    dSYM -> {} (UUID match)", dsym.display());
        }
        _ => println!(
            "    dSYM -> {} (dwarfdump unavailable; UUID check skipped)",
            dsym.display()
        ),
    }

    // ditto -c -k --keepParent: the standard .dSYM archive form (preserves the
    // bundle structure so Xcode/symbolicators accept it after unzip).
    let zip = out_dir.join(format!("aterm-{short_version}-dSYM.zip"));
    let _ = std::fs::remove_file(&zip);
    let mut cmd = Command::new("/usr/bin/ditto");
    cmd.arg("-c")
        .arg("-k")
        .arg("--keepParent")
        .arg(&dsym)
        .arg(&zip);
    run_quiet(cmd, "ditto dSYM zip")?;
    println!("    dSYM zip -> {}", zip.display());
    Ok((Some(dsym), Some(zip)))
}

/// `dwarfdump --uuid <path>` → the UUID token of every "UUID: <hex> (<arch>)"
/// line, or None when dwarfdump is unavailable / produced nothing.
fn dwarf_uuids(path: &Path) -> Option<Vec<String>> {
    let out = Command::new("dwarfdump")
        .arg("--uuid")
        .arg(path)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let uuids: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            l.trim()
                .strip_prefix("UUID: ")?
                .split_whitespace()
                .next()
                .map(String::from)
        })
        .collect();
    (!uuids.is_empty()).then_some(uuids)
}

/// chmod +x equivalent for a produced binary copy.
#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)
        .map_err(|e| format!("stat {}: {e}", path.display()))?
        .permissions();
    perms.set_mode(perms.mode() | 0o755);
    std::fs::set_permissions(path, perms).map_err(|e| format!("chmod {}: {e}", path.display()))
}

/// Windows: executability comes from the file extension; no mode bits to set.
#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// Run a short command with captured output; on failure surface its stderr in
/// the error (the "typed Command shell-outs with captured stderr" rule, §6).
fn run_quiet(mut cmd: Command, what: &str) -> Result<(), String> {
    let out = cmd.output().map_err(|e| format!("spawn {what}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{what} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// Where a `--target` build put a cargo binary (by its [`PACKAGES`] bin name;
/// the SHIP rename happens at the lipo/copy step).
fn target_bin(repo_root: &Path, triple: &str, bin: &str) -> PathBuf {
    repo_root
        .join("target")
        .join(triple)
        .join("release")
        .join(bin)
}

/// "4m12s" / "38s" — per-step timing for the cut transcript (spec §6).
fn fmt_elapsed(start: Instant) -> String {
    let s = start.elapsed().as_secs();
    if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{
        LC_SEGMENT_64, MACH_HEADER_64_LEN, MACH_MAGIC_64, MH_EXECUTE, SECTION_64_LEN,
        SEGMENT_COMMAND_64_LEN, parse_thin_macho_update_pin, validate_app_version_reports,
        validate_cli_app_version, validate_embedded_update_pin, validate_final_slice_records,
        validate_lipo_architectures, validate_named_cli_app_version,
        validate_slice_update_pin_reports,
    };

    const EXPECTED: &str = "529d8b60583fdc58b13afdba7050de6b21c0740b86dd87e5af769a2afb6c30f4";
    const WRONG: &str = "b8d47d9179feb56b1cbbe61c000b81f18d1ac152507d8abd320e2a2297890f1f";

    #[cfg(target_vendor = "apple")]
    #[used]
    #[unsafe(link_section = "__DATA,__aterm_upin")]
    static NATIVE_MACHO_TEST_RECORD: [u8; 64] =
        *b"529d8b60583fdc58b13afdba7050de6b21c0740b86dd87e5af769a2afb6c30f4";

    fn report(pin: &str) -> String {
        format!("aterm diagnostics\nupdate-pin-sha256: {pin}\nrenderer: gpu\n")
    }

    fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn write_name(bytes: &mut [u8], offset: usize, value: &[u8]) {
        assert!(value.len() <= 16);
        bytes[offset..offset + value.len()].copy_from_slice(value);
    }

    /// Minimal structurally valid arm64 thin Mach-O.  `declared_size` is kept
    /// separate from the data length so negative tests can exercise exact-size
    /// and bounds checks independently.
    fn thin_macho(sections: &[(&[u8], u64, &[u8])]) -> Vec<u8> {
        let section_count = u32::try_from(sections.len()).unwrap();
        let command_size = SEGMENT_COMMAND_64_LEN + SECTION_64_LEN * u64::from(section_count);
        let commands_end = MACH_HEADER_64_LEN + command_size;
        let data_size: u64 = sections
            .iter()
            .map(|(_, _, data)| u64::try_from(data.len()).unwrap())
            .sum();
        let file_len = commands_end + data_size;
        let mut bytes = vec![0_u8; usize::try_from(file_len).unwrap()];

        write_u32(&mut bytes, 0, MACH_MAGIC_64);
        write_u32(&mut bytes, 4, super::CPU_TYPE_ARM64);
        write_u32(&mut bytes, 12, MH_EXECUTE);
        write_u32(&mut bytes, 16, 1);
        write_u32(&mut bytes, 20, u32::try_from(command_size).unwrap());

        let segment = MACH_HEADER_64_LEN as usize;
        write_u32(&mut bytes, segment, LC_SEGMENT_64);
        write_u32(
            &mut bytes,
            segment + 4,
            u32::try_from(command_size).unwrap(),
        );
        write_name(&mut bytes, segment + 8, b"__DATA");
        write_u64(&mut bytes, segment + 40, commands_end);
        write_u64(&mut bytes, segment + 48, data_size);
        write_u32(&mut bytes, segment + 64, section_count);

        let mut data_offset = commands_end;
        for (index, (name, declared_size, data)) in sections.iter().enumerate() {
            let section =
                segment + SEGMENT_COMMAND_64_LEN as usize + index * SECTION_64_LEN as usize;
            write_name(&mut bytes, section, name);
            write_name(&mut bytes, section + 16, b"__DATA");
            write_u64(&mut bytes, section + 40, *declared_size);
            write_u32(
                &mut bytes,
                section + 48,
                u32::try_from(data_offset).unwrap(),
            );
            let end = data_offset + u64::try_from(data.len()).unwrap();
            bytes[usize::try_from(data_offset).unwrap()..usize::try_from(end).unwrap()]
                .copy_from_slice(data);
            data_offset = end;
        }
        bytes
    }

    fn parse_fixture(bytes: &[u8]) -> Result<String, String> {
        parse_thin_macho_update_pin(&mut Cursor::new(bytes), u64::try_from(bytes.len()).unwrap())
    }

    #[test]
    fn native_and_final_runtime_reports_must_match_exact_authority_pin() {
        let arm = report(EXPECTED);
        let x86 = report(EXPECTED);
        assert!(
            validate_slice_update_pin_reports(EXPECTED, &[("arm64", &arm), ("x86_64", &x86)])
                .is_ok()
        );

        let empty = report("empty");
        assert!(validate_slice_update_pin_reports(EXPECTED, &[("arm64", &empty)]).is_err());
        let wrong = report(WRONG);
        assert!(validate_slice_update_pin_reports(EXPECTED, &[("arm64", &wrong)]).is_err());
        assert!(
            validate_slice_update_pin_reports(EXPECTED, &[("arm64", &arm), ("x86_64", &wrong)])
                .is_err()
        );
        assert!(validate_slice_update_pin_reports(EXPECTED, &[("arm64", "")]).is_err());
        let duplicate = format!("{}update-pin-sha256: {EXPECTED}\n", report(EXPECTED));
        assert!(validate_slice_update_pin_reports(EXPECTED, &[("arm64", &duplicate)]).is_err());
    }

    #[test]
    fn app_version_reports_and_cli_identities_must_match_claim_exactly() {
        let report = "aterm diagnostics\nversion:   0.2.0 (abc123, built now)\nrenderer: gpu\n";
        assert!(validate_app_version_reports("0.2.0", &[("universal", report)]).is_ok());
        assert!(validate_app_version_reports("0.3.0", &[("universal", report)]).is_err());
        // The dev-build spelling of the same workspace version is NOT the
        // release version: only DEV == 0 ships.
        assert!(validate_app_version_reports("0.2.1", &[("universal", report)]).is_err());
        assert!(validate_app_version_reports("0.2.0", &[("universal", "")]).is_err());
        let duplicate = format!("{report}version:   0.2.0 (abc123, built now)\n");
        assert!(validate_app_version_reports("0.2.0", &[("universal", &duplicate)]).is_err());

        assert!(validate_cli_app_version("0.2.0", b"aterm 0.2.0\n").is_ok());
        assert!(validate_cli_app_version("0.2.0", b"aterm 0.2.1\n").is_err());
        assert!(validate_named_cli_app_version("aterm-gui", "0.2.0", b"aterm-gui 0.2.0\n").is_ok());
        assert!(validate_named_cli_app_version("aterm-ctl", "0.2.0", b"aterm-ctl 0.2.0\n").is_ok());
        assert!(
            validate_named_cli_app_version("aterm-ctl", "0.2.0", b"aterm-gui 0.2.0\n").is_err()
        );
        // The S12 which-copy lines may follow the identity line — and only those.
        assert!(
            validate_cli_app_version(
                "0.2.0",
                b"aterm 0.2.0\nrunning: /Applications/aterm.app\nanother copy: \
                  /Users//ana/Applications/aterm.app (0.1.0) \xe2\x80\x94 not the one running; \
                  the updater updates only this one\n"
            )
            .is_ok()
        );
        assert!(validate_cli_app_version("0.2.0", b"aterm 0.2.0\nrunning: /x/aterm\n").is_ok());
        assert!(
            validate_cli_app_version("0.2.1", b"aterm 0.2.0\nrunning: /x/aterm\n").is_err(),
            "the identity line is still exact"
        );
        assert!(
            validate_cli_app_version("0.2.0", b"aterm 0.2.0\nwarning: stale slice\n").is_err(),
            "anything but the which-copy lines still fails"
        );
        assert!(
            validate_cli_app_version("0.2.0", b"aterm 0.2.0\n\nrunning: /x/aterm\n").is_err(),
            "a blank line is not a which-copy line"
        );
        assert!(
            validate_cli_app_version("0.2.0", b"aterm 0.2.0\nrunning: /x/aterm").is_err(),
            "the report is newline-terminated"
        );
        assert!(
            validate_cli_app_version("0.2.0", b"running: /x/aterm\naterm 0.2.0\n").is_err(),
            "the identity line comes first"
        );
    }

    #[test]
    fn thin_macho_parser_accepts_one_exact_dedicated_record() {
        let bytes = thin_macho(&[(b"__aterm_upin", 64, EXPECTED.as_bytes())]);
        let observed = parse_fixture(&bytes).expect("valid dedicated section");
        assert_eq!(observed, EXPECTED);
        validate_embedded_update_pin(EXPECTED, &observed, "arm64").unwrap();
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn parser_reads_the_real_native_test_macho_section() {
        let executable = std::env::current_exe().expect("current test executable");
        let observed = super::read_thin_macho_update_pin(&executable)
            .expect("parse dedicated section from the real native Mach-O test binary");
        assert_eq!(observed, EXPECTED);
    }

    #[test]
    fn thin_macho_parser_rejects_missing_duplicate_and_wrong_length_records() {
        let missing = thin_macho(&[]);
        assert!(parse_fixture(&missing).unwrap_err().contains("missing"));

        let duplicate = thin_macho(&[
            (b"__aterm_upin", 64, EXPECTED.as_bytes()),
            (b"__aterm_upin", 64, EXPECTED.as_bytes()),
        ]);
        assert!(parse_fixture(&duplicate).unwrap_err().contains("duplicate"));

        for declared_size in [63, 65] {
            let wrong_length = thin_macho(&[(b"__aterm_upin", declared_size, EXPECTED.as_bytes())]);
            assert!(parse_fixture(&wrong_length).unwrap_err().contains("length"));
        }
    }

    #[test]
    fn thin_macho_parser_rejects_wrong_segment_bounds_and_noncanonical_bytes() {
        let mut wrong_segment = thin_macho(&[(b"__aterm_upin", 64, EXPECTED.as_bytes())]);
        let segment = MACH_HEADER_64_LEN as usize;
        wrong_segment[segment + 8..segment + 24].fill(0);
        write_name(&mut wrong_segment, segment + 8, b"__WRONG");
        assert!(
            parse_fixture(&wrong_segment)
                .unwrap_err()
                .contains("__DATA")
        );

        let mut outside = thin_macho(&[(b"__aterm_upin", 64, EXPECTED.as_bytes())]);
        let section = segment + SEGMENT_COMMAND_64_LEN as usize;
        write_u32(&mut outside, section + 48, u32::MAX);
        assert!(parse_fixture(&outside).unwrap_err().contains("outside"));

        let uppercase = EXPECTED.to_ascii_uppercase();
        let noncanonical = thin_macho(&[(b"__aterm_upin", 64, uppercase.as_bytes())]);
        assert!(
            parse_fixture(&noncanonical)
                .unwrap_err()
                .contains("canonical")
        );

        let mut zero_fill = thin_macho(&[(b"__aterm_upin", 64, EXPECTED.as_bytes())]);
        write_u32(&mut zero_fill, section + 64, 1);
        assert!(parse_fixture(&zero_fill).unwrap_err().contains("S_REGULAR"));
    }

    #[test]
    fn raw_fingerprint_substrings_are_never_authority() {
        let mut missing_with_decoy = thin_macho(&[]);
        missing_with_decoy.extend_from_slice(EXPECTED.as_bytes());
        assert!(parse_fixture(&missing_with_decoy).is_err());

        let mut wrong_record_with_decoy = thin_macho(&[(b"__aterm_upin", 64, WRONG.as_bytes())]);
        wrong_record_with_decoy.extend_from_slice(EXPECTED.as_bytes());
        let observed = parse_fixture(&wrong_record_with_decoy).unwrap();
        assert_eq!(observed, WRONG);
        assert!(validate_embedded_update_pin(EXPECTED, &observed, "x86_64").is_err());
    }

    #[test]
    fn malformed_thin_macho_metadata_fails_closed() {
        let valid = thin_macho(&[(b"__aterm_upin", 64, EXPECTED.as_bytes())]);

        let mut fat_magic = valid.clone();
        write_u32(&mut fat_magic, 0, 0xcafe_babe);
        assert!(parse_fixture(&fat_magic).is_err());

        let mut truncated_commands = valid.clone();
        write_u32(&mut truncated_commands, 20, u32::MAX);
        assert!(parse_fixture(&truncated_commands).is_err());

        let mut impossible_count = valid;
        write_u32(&mut impossible_count, 16, u32::MAX);
        assert!(parse_fixture(&impossible_count).is_err());

        let mut wrong_segment_command = thin_macho(&[(b"__aterm_upin", 64, EXPECTED.as_bytes())]);
        write_u32(
            &mut wrong_segment_command,
            MACH_HEADER_64_LEN as usize,
            super::LC_SEGMENT,
        );
        assert!(
            parse_fixture(&wrong_segment_command)
                .unwrap_err()
                .contains("32-bit")
        );
    }

    #[test]
    fn final_shipped_slice_proof_requires_exact_architecture_coverage() {
        let required = ["arm64", "x86_64"];
        assert!(
            validate_final_slice_records(
                EXPECTED,
                &required,
                &[("arm64", EXPECTED), ("x86_64", EXPECTED)],
            )
            .is_ok()
        );
        assert!(validate_final_slice_records(EXPECTED, &required, &[("arm64", EXPECTED)]).is_err());
        assert!(
            validate_final_slice_records(
                EXPECTED,
                &required,
                &[("arm64", EXPECTED), ("arm64", EXPECTED)],
            )
            .is_err()
        );
        assert!(
            validate_final_slice_records(
                EXPECTED,
                &required,
                &[("arm64", EXPECTED), ("x86_64", WRONG)],
            )
            .is_err()
        );
        assert!(
            validate_final_slice_records(
                EXPECTED,
                &required,
                &[("arm64", EXPECTED), ("ppc64", EXPECTED)],
            )
            .is_err()
        );

        assert!(validate_lipo_architectures("x86_64 arm64", false).is_ok());
        assert!(validate_lipo_architectures("arm64", true).is_ok());
        for invalid in ["arm64", "x86_64", "arm64 arm64", "arm64 x86_64 ppc64"] {
            assert!(validate_lipo_architectures(invalid, false).is_err());
        }
    }
}
