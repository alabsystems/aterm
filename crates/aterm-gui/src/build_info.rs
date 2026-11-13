// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Build provenance for the Settings `/about` route and `aterm ctl version`.

use aterm_stamp::Stamp;
use std::sync::OnceLock;

pub(crate) const AUTHOR_ATTRIBUTION: &str = "By Andrew Yates";
pub(crate) const COMPANY: &str = "ALab";
/// Application tagline shown in About and Copy Build Information.
pub(crate) const TAGLINE: &str = "Fast, hardened, introspectable.";
pub(crate) const AUTHOR_COMPANY_BYLINE: &str = "By Andrew Yates · ALab";
/// Short author name for narrow About bylines.
pub(crate) const AUTHOR_SHORT: &str = "A. Yates";
/// Project website domain.
pub(crate) const SITE: &str = aterm_types::identity::SITE;

/// The stamp this crate's test binary reads: a clean, stamped Trust build, so the suite is
/// deterministic. Its version is a fixed release (the one current when the product version left
/// the library graph), so no bump is an input of this test binary.
#[cfg(test)]
const TEST_STAMP: Stamp = Stamp {
    app_version: "0.156.0",
    git_commit: "7e57b1d0c0de",
    git_commit_full: "7e57b1d0c0de7e57b1d0c0de7e57b1d0c0de7e57",
    git_dirty: "false",
    build_time: "2065-01-24T05:20:00Z",
    build_number: "3000000000",
    dev_commits: "1",
    compiler_version_line: "rustc 1.99.0-dev (321aaeda7 2026-09-17) (trustc 0.1.0)",
    compiler_commit: "321aaeda75478038420d97830f14724c33c8dc39",
    compiler_host: "aarch64-apple-darwin",
    compiler_trust_version: "0.1.0",
    compiler_flavor: "t",
    build_profile: "debug",
    trust_verify: "off",
    index_hint: "",
    release_build: false,
};

/// The running binary's build facts: the stamp `crates/aterm` installs first thing in `main`
/// (`aterm_stamp`), read by every accessor here.
fn stamp() -> &'static Stamp {
    #[cfg(test)]
    let unstamped = &TEST_STAMP;
    #[cfg(not(test))]
    let unstamped = &Stamp::UNSTAMPED;
    aterm_stamp::installed().unwrap_or(unstamped)
}

/// Workspace package version.
#[must_use]
pub(crate) fn version() -> &'static str {
    stamp().app_version
}

/// Short git commit hash, with `-dirty` when built from a tree modified outside the measured
/// ledger `docs/measured` (`crates/aterm/stamp_paths.rs`).
#[must_use]
pub(crate) fn git_commit() -> &'static str {
    stamp().git_commit
}

/// Full 40-hex git commit hash, or `"unknown"`.
#[must_use]
pub(crate) fn git_commit_full() -> &'static str {
    stamp().git_commit_full
}

/// `"true"` when the build tree was dirty.
#[must_use]
pub(crate) fn git_dirty() -> &'static str {
    stamp().git_dirty
}

pub(crate) const BINARY_TARGET: &str = env!("ATERM_BINARY_TARGET");

/// UTC build timestamp (RFC3339), or `"unknown"`.
#[must_use]
pub(crate) fn build_time() -> &'static str {
    stamp().build_time
}

/// Monotonic build number.
#[must_use]
pub(crate) fn build_number() -> &'static str {
    stamp().build_number
}

/// Commits since the newest release tag at build time.
#[must_use]
pub(crate) fn dev_commits() -> &'static str {
    stamp().dev_commits
}

/// Returns whether a release lane produced this binary.
#[must_use]
pub(crate) fn is_release_build() -> bool {
    stamp().release_build
}

/// First line of the producing compiler's `-vV` output.
#[must_use]
pub(crate) fn compiler_version_line() -> &'static str {
    stamp().compiler_version_line
}

/// Producing compiler's full commit hash, or `"unknown"`.
#[must_use]
pub(crate) fn compiler_commit() -> &'static str {
    stamp().compiler_commit
}

/// Producing compiler's host triple.
#[must_use]
pub(crate) fn compiler_host() -> &'static str {
    stamp().compiler_host
}

/// Trust toolchain version from `-vV`, or `""` for upstream Rust.
#[must_use]
pub(crate) fn compiler_trust_version() -> &'static str {
    stamp().compiler_trust_version
}

/// Compiler flavor (`"r"` upstream Rust, `"t"` Trust).
#[must_use]
pub(crate) fn compiler_flavor() -> &'static str {
    stamp().compiler_flavor
}

/// Cargo build profile (`"debug"` or `"release"`).
#[must_use]
pub(crate) fn build_profile() -> &'static str {
    stamp().build_profile
}

/// `"on"` if compiled with the Trust verification pipeline, else `"off"`.
#[must_use]
pub(crate) fn trust_verify() -> &'static str {
    stamp().trust_verify
}

/// Lowercase SHA-256 fingerprint of the release master key, or all zeros when unset.
#[cfg(any(target_os = "macos", test))]
pub(crate) const EMBEDDED_UPDATE_PIN_SHA256: &str = env!("ATERM_UPDATE_PIN_SHA256");

#[cfg(target_os = "macos")]
const fn update_pin_record_bytes(value: &str) -> [u8; 64] {
    let bytes = value.as_bytes();
    assert!(bytes.len() == 64);
    let mut record = [0_u8; 64];
    let mut index = 0;
    while index < record.len() {
        let byte = bytes[index];
        assert!(byte.is_ascii_digit() || (byte >= b'a' && byte <= b'f'));
        record[index] = byte;
        index += 1;
    }
    record
}

/// Linker-retained, fixed-format release-authority record. Its own Mach-O section lets the
/// release cutter find it structurally, never by a byte-substring match.
#[cfg(target_vendor = "apple")]
#[used]
#[unsafe(link_section = "__DATA,__aterm_upin")]
static ATERM_UPDATE_PIN_RECORD: [u8; 64] = update_pin_record_bytes(EMBEDDED_UPDATE_PIN_SHA256);

/// Short (8-hex) commit slug of the producing compiler.
#[must_use]
pub(crate) fn compiler_commit_short() -> &'static str {
    let commit = compiler_commit();
    if commit.len() >= 8 && commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        &commit[..8]
    } else {
        "unknown"
    }
}

/// Display version (`[workspace.package] version`).
#[must_use]
pub(crate) fn version_display() -> &'static str {
    version()
}

/// The compiler's bare release, e.g. `1.96.0` / `1.96.0-dev` — the second token of
/// the `-vV` first line (`rustc <release> (<hash> <date>)`), `"unknown"` if absent.
fn compiler_release() -> &'static str {
    compiler_version_line()
        .split_whitespace()
        .nth(1)
        .unwrap_or("unknown")
}

/// The toolchain's own name and version, as About leads with it: `Trust 0.1.0` (`Trust
/// (version unreported)` when the compiler predates the `trust:` line), or `Rust 1.96.0`.
fn toolchain_display() -> String {
    if compiler_flavor() == "t" {
        let trust = compiler_trust_version();
        if trust.is_empty() {
            "Trust (version unreported)".to_string()
        } else {
            format!("Trust {trust}")
        }
    } else {
        format!("Rust {}", compiler_release())
    }
}

/// Single-line compiler provenance summary for About and `--diagnose`.
#[must_use]
pub(crate) fn compiler_summary() -> String {
    let slug = compiler_commit_short();
    let profile = build_profile();
    let verify = trust_verify();
    if compiler_flavor() == "t" {
        format!(
            "{} \u{00b7} trustc {slug} \u{00b7} compatible with Rust {} \u{00b7} {profile} \u{00b7} trust_verify {verify}",
            toolchain_display(),
            compiler_release(),
        )
    } else {
        format!(
            "{} \u{00b7} rustc {slug} \u{00b7} {profile} \u{00b7} trust_verify {verify}",
            toolchain_display(),
        )
    }
}

/// 16-hex content signature of the running binary, or `"unknown"`.
#[must_use]
pub(crate) fn binary_signature() -> &'static str {
    static SIG: OnceLock<String> = OnceLock::new();
    SIG.get_or_init(|| {
        #[cfg(target_os = "macos")]
        if let Some(cdhash) = running_cdhash() {
            return aterm_codec::hex::encode_lower(&cdhash[..8]);
        }
        std::env::current_exe()
            .and_then(std::fs::read)
            .map(|bytes| format!("{:016x}", aterm_hash::fx_hash_bytes(&bytes)))
            .unwrap_or_else(|_| "unknown".to_string())
    })
    .as_str()
}

/// Returns whether [`binary_signature`] hashes the executable file on disk.
#[must_use]
pub(crate) fn binary_signature_reads_the_file() -> bool {
    #[cfg(target_os = "macos")]
    return running_cdhash().is_none();
    #[cfg(not(target_os = "macos"))]
    true
}

/// The running image's code directory hash, the kernel's own copy from the exec
/// (`csops(getpid(), CS_OPS_CDHASH)`), so no file is read; `None` when it has none.
#[cfg(target_os = "macos")]
fn running_cdhash() -> Option<[u8; 20]> {
    const CS_OPS_CDHASH: u32 = 5;
    unsafe extern "C" {
        /// `int csops(pid_t, unsigned int ops, void *useraddr, size_t usersize)`; no SDK
        /// header declares it.
        fn csops(pid: libc::pid_t, ops: u32, useraddr: *mut libc::c_void, usersize: usize) -> i32;
    }
    let mut cdhash = [0u8; 20];
    // SAFETY: `cdhash` is 20 writable bytes, the size passed; `getpid` has no preconditions.
    let rc = unsafe {
        csops(
            libc::getpid(),
            CS_OPS_CDHASH,
            cdhash.as_mut_ptr().cast(),
            cdhash.len(),
        )
    };
    (rc == 0).then_some(cdhash)
}

/// Structured `(key, value)` provenance rows for `/about` and Copy Build Information.
#[must_use]
pub(crate) fn about_fields() -> Vec<(&'static str, String)> {
    let mut fields = vec![
        ("tagline", TAGLINE.to_string()),
        ("author", AUTHOR_ATTRIBUTION.to_string()),
        ("company", COMPANY.to_string()),
        ("site", SITE.to_string()),
        ("version", version_display().to_string()),
        ("build", build_number().to_string()),
        ("commit", git_commit().to_string()),
        ("built", build_time().to_string()),
        ("arch", std::env::consts::ARCH.to_string()),
        ("compiler", compiler_summary()),
        ("signature", binary_signature().to_string()),
    ];
    if let Some(copy) = aterm_update::which_copy::observe() {
        fields.extend(which_copy_rows(&copy));
    }
    fields
}

/// Returns the `running` and `another copy` rows from a which-copy report.
#[must_use]
pub(crate) fn which_copy_rows(
    copy: &aterm_update::which_copy::WhichCopy,
) -> Vec<(&'static str, String)> {
    let mut rows = vec![("running", copy.running_detail())];
    if !copy.others.is_empty() {
        let others = copy
            .others
            .iter()
            .map(|other| copy.other_detail(other))
            .collect::<Vec<_>>()
            .join(" \u{00b7} ");
        rows.push(("another copy", others));
    }
    rows
}

/// Formats the `aterm ctl version` single-line `key=value` reply.
#[must_use]
pub(crate) fn control_line() -> String {
    let flavor = compiler_flavor();
    let trust = if flavor != "t" {
        "none"
    } else if compiler_trust_version().is_empty() {
        "unreported"
    } else {
        compiler_trust_version()
    };
    let host = compiler_host();
    let profile = build_profile();
    let verify = trust_verify();
    static UPDATE_PIN_FINGERPRINT: OnceLock<String> = OnceLock::new();
    let update_pin_sha256 =
        UPDATE_PIN_FINGERPRINT.get_or_init(aterm_update::compiled_update_pin_sha256);
    #[cfg(target_os = "macos")]
    let objc_contained = aterm_objc::contained_count();
    #[cfg(not(target_os = "macos"))]
    let objc_contained = 0u64;
    format!(
        "OK version={} build={} commit={} built={} \
         arch={} trustc={} trustc_commit={} trustc_host={host} flavor={flavor} \
         trust={trust} rust_compat={} \
         profile={profile} trust_verify={verify} update_pin_sha256={update_pin_sha256} \
         objc_contained={objc_contained} signature={}\n",
        version_display(),
        build_number(),
        git_commit(),
        build_time(),
        std::env::consts::ARCH,
        compiler_release(),
        compiler_commit_short(),
        compiler_release(),
        binary_signature()
    )
}

#[cfg(test)]
#[path = "build_info_tests.rs"]
mod tests;
