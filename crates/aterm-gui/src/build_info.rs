// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Build provenance — version, git commit, build timestamp, and a content signature
//! of the running binary. Surfaced in two places: the cross-platform About overlay
//! ([`about_fields`]) and over the control socket
//! (`aterm-ctl version`, see [`crate::control`]).
//!
//! `VERSION`/`GIT_COMMIT`/`BUILD_TIME` are stamped at compile time by `build.rs`.
//! [`binary_signature`] is computed at runtime from the actual executable, so it
//! reflects the EXACT shipped bytes (the `.app`'s signed binary hashes differently
//! from the bare `target/release` binary — which is correct: it is what's running).

use std::hash::Hasher;
use std::sync::OnceLock;

pub(crate) const AUTHOR_ATTRIBUTION: &str = "By Andrew Yates";
pub(crate) const COMPANY: &str = "ALab";
pub(crate) const AUTHOR_COMPANY_BYLINE: &str = "By Andrew Yates · ALab";

/// Semantic version, from Cargo's `[package] version`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Short git commit the binary was built from — with a `-dirty` suffix when the
/// working tree had uncommitted changes. `"unknown"` when git was unavailable at
/// build time (e.g. a source tarball). Stamped by `build.rs`.
pub const GIT_COMMIT: &str = env!("ATERM_GIT_COMMIT");

/// UTC build timestamp (RFC3339), or `"unknown"`. Stamped by `build.rs`.
pub const BUILD_TIME: &str = env!("ATERM_BUILD_TIME");

/// Monotonic build number, stamped by `build.rs`. The version (above) is a plain
/// `MAJOR.MINOR`; the monotonic ordering the updater needs lives here in metadata.
/// For a RELEASE this is the number `cargo ship cut` claims in the append-only
/// `RELEASES.ledger` (`max(last + 1, unix_now)` — strictly increasing by
/// construction, epoch-scale) and pins via `SOURCE_DATE_EPOCH`; a dev build falls
/// back to HEAD's committer Unix epoch — the same seconds scale, so dev and release
/// builds stay mutually ordered. Used as the macOS `CFBundleVersion`.
pub const BUILD_NUMBER: &str = env!("ATERM_BUILD_NUMBER");

/// Full first line of the producing compiler's `-vV`, e.g.
/// `rustc 1.96.0 (ac68faa20 2026-05-25) (Homebrew)`. Stamped by `build.rs`.
pub const RUSTC_VERSION: &str = env!("ATERM_RUSTC_VERSION");

/// The producing compiler's FULL git commit hash (`commit-hash:` from `-vV`), or
/// `"unknown"`. This is what tells two coexisting 1.96.0 toolchains apart —
/// per-binary compiler provenance. Stamped by `build.rs`.
pub const RUSTC_COMMIT: &str = env!("ATERM_RUSTC_COMMIT");

/// The producing compiler's host triple (`host:` from `-vV`). Stamped by `build.rs`.
pub const RUSTC_HOST: &str = env!("ATERM_RUSTC_HOST");

/// Compiler flavor: `"r"` = upstream Rust, `"t"` = the Trust fork. Detection order
/// (see `build.rs` / `compiler_probe.rs`): explicit `ATERM_COMPILER_FLAVOR` override,
/// `/trust/` in the RUSTC path, `RUSTUP_TOOLCHAIN=trust`, else `"r"`.
pub const COMPILER_FLAVOR: &str = env!("ATERM_COMPILER_FLAVOR");

/// Cargo profile the binary was compiled under (`"debug"`/`"release"`).
pub const BUILD_PROFILE: &str = env!("ATERM_BUILD_PROFILE");

/// `"on"` iff `--cfg trust_verify` was active in this compile, else `"off"`.
pub const TRUST_VERIFY: &str = env!("ATERM_TRUST_VERIFY");

/// Exact lowercase SHA-256 fingerprint of the raw compiled Ed25519 updater key,
/// or the all-zero no-pin sentinel in an ordinary development build.  `build.rs`
/// derives it from the same `ATERM_UPDATE_PUBKEY` input consumed by
/// `aterm-update`; the release cutter independently cross-checks this record
/// against both runtime diagnostics and the permanent channel authority.
pub const EMBEDDED_UPDATE_PIN_SHA256: &str = env!("ATERM_UPDATE_PIN_SHA256");

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

/// Linker-retained, fixed-format release-authority record.  The explicit Mach-O
/// segment/section lets the release cutter locate this value structurally; it
/// never accepts a raw byte-substring match elsewhere in the executable.
#[cfg(target_vendor = "apple")]
#[used]
#[unsafe(link_section = "__DATA,__aterm_upin")]
static ATERM_UPDATE_PIN_RECORD: [u8; 64] = update_pin_record_bytes(EMBEDDED_UPDATE_PIN_SHA256);

/// Short (8-hex) slug of the producing compiler's commit — the version-suffix slug.
/// `"unknown"` when the toolchain didn't report a hash (e.g. some distro builds).
#[must_use]
pub fn compiler_commit_short() -> &'static str {
    if RUSTC_COMMIT.len() >= 8 && RUSTC_COMMIT.bytes().all(|b| b.is_ascii_hexdigit()) {
        &RUSTC_COMMIT[..8]
    } else {
        "unknown"
    }
}

/// The DISPLAY version: a plain `MAJOR.MINOR` (Cargo carries `MAJOR.MINOR.0`; the
/// trailing `.0` is stripped). The build number, commit, compiler/toolchain, and build
/// time are their OWN provenance rows (see [`about_fields`] / [`control_line`]), never
/// crammed into the version string — a simple version, stats in metadata.
///
/// Display-ONLY by contract: the in-app updater orders builds by the monotonic
/// [`BUILD_NUMBER`] alone (its `current_version` parameter is informational — see
/// `aterm-update`), so this string can never affect an update comparison.
#[must_use]
pub fn version_display() -> &'static str {
    VERSION.strip_suffix(".0").unwrap_or(VERSION)
}

/// The compiler's bare release, e.g. `1.96.0` / `1.96.0-dev` — the second token of
/// the `-vV` first line (`rustc <release> (<hash> <date>)`), `"unknown"` if absent.
fn rustc_release() -> &'static str {
    RUSTC_VERSION.split_whitespace().nth(1).unwrap_or("unknown")
}

/// One human line of compiler provenance for the About panel, e.g.
/// `rustc 1.96.0 (ac68faa2) · rust · release · trust_verify off`.
#[must_use]
pub fn compiler_summary() -> String {
    let flavor_word = if COMPILER_FLAVOR == "t" {
        "trust"
    } else {
        "rust"
    };
    format!(
        "rustc {} ({}) \u{00b7} {flavor_word} \u{00b7} {BUILD_PROFILE} \u{00b7} trust_verify {TRUST_VERIFY}",
        rustc_release(),
        compiler_commit_short(),
    )
}

/// A content signature of the RUNNING binary: a 16-hex FxHash of `current_exe()`,
/// computed once and cached. Identifies the exact bytes that shipped; `"unknown"` if
/// the executable can't be read.
///
/// This is a build FINGERPRINT, not a cryptographic attestation — it uses the
/// workspace's non-cryptographic FxHash to avoid pulling in a crypto dependency. It
/// is enough to tell two builds apart and to confirm "the binary I'm running is the
/// one I shipped", which is its purpose in the cross-platform About overlay and
/// `aterm-ctl version`.
#[must_use]
pub fn binary_signature() -> &'static str {
    static SIG: OnceLock<String> = OnceLock::new();
    SIG.get_or_init(|| {
        std::env::current_exe()
            .and_then(std::fs::read)
            .map(|bytes| {
                let mut h = aterm_hash::FxHasher::default();
                h.write(&bytes);
                format!("{:016x}", h.finish())
            })
            .unwrap_or_else(|_| "unknown".to_string())
    })
    .as_str()
}

/// Structured `(key, value)` provenance rows — the SINGLE source the own-rendered,
/// introspectable About overlay both PAINTS and serialises for the `controls about`
/// verb, so its pixels and its machine-readable text can never disagree. Each row's
/// value is what "copy this row" puts on the clipboard. Cross-platform (the overlay
/// replaces the macOS-only native panel).
#[must_use]
pub fn about_fields() -> Vec<(&'static str, String)> {
    vec![
        (
            "tagline",
            "a fast, hardened, AI-introspectable terminal".to_string(),
        ),
        ("author", AUTHOR_ATTRIBUTION.to_string()),
        ("company", COMPANY.to_string()),
        ("version", version_display().to_string()),
        ("build", BUILD_NUMBER.to_string()),
        ("commit", GIT_COMMIT.to_string()),
        ("built", BUILD_TIME.to_string()),
        // The RUNNING slice's CPU arch (`aarch64`/`x86_64`). For a universal binary this
        // reflects which slice is executing on THIS Mac — so with a mixed-flavor build
        // (arm64 = Trust +t, x86_64 = upstream +r) `arch` + the `compiler` row together
        // tell you exactly what you're on.
        ("arch", std::env::consts::ARCH.to_string()),
        ("compiler", compiler_summary()),
        ("signature", binary_signature().to_string()),
    ]
}

/// The control-socket (`aterm-ctl version`) response line: a stable, greppable
/// `key=value` form so scripts can parse the running build's provenance. Existing
/// keys keep their meaning (additive only); `version=` carries the display suffix
/// (`+r.<slug>`/`+t.<slug>`) — nothing machine-side compares it (the updater orders
/// by `build=`, which stays the bare monotonic counter).
#[must_use]
pub fn control_line() -> String {
    let update_pin_sha256 = aterm_update::compiled_update_pin_sha256();
    format!(
        "OK version={} build={BUILD_NUMBER} commit={GIT_COMMIT} built={BUILD_TIME} \
         arch={} rustc={} rustc_commit={} rustc_host={RUSTC_HOST} flavor={COMPILER_FLAVOR} \
         profile={BUILD_PROFILE} trust_verify={TRUST_VERIFY} update_pin_sha256={update_pin_sha256} \
         signature={}\n",
        version_display(),
        std::env::consts::ARCH,
        rustc_release(),
        compiler_commit_short(),
        binary_signature()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The display version is a plain MAJOR.MINOR — provenance (build number, commit,
    /// compiler) lives in the other rows, never smuggled into the version string.
    #[test]
    fn version_display_is_bare_major_minor() {
        let v = version_display();
        assert!(!v.contains('+'), "no compiler suffix in the version: {v}");
        assert_eq!(
            v,
            VERSION.strip_suffix(".0").unwrap_or(VERSION),
            "the trailing .0 of Cargo's MAJOR.MINOR.0 is stripped"
        );
        if VERSION.ends_with(".0") {
            assert_eq!(v.split('.').count(), 2, "leaves exactly MAJOR.MINOR: {v}");
        }
    }

    /// On this box (stock cargo, no override, no trust toolchain) the flavor
    /// defaults to 'r'; the compile-time constants must agree with the classifier's
    /// contract either way (only 'r'/'t' can ever be stamped).
    #[test]
    fn stamped_flavor_is_r_or_t() {
        assert!(
            COMPILER_FLAVOR == "r" || COMPILER_FLAVOR == "t",
            "stamped flavor must be r|t: {COMPILER_FLAVOR}"
        );
        assert!(TRUST_VERIFY == "on" || TRUST_VERIFY == "off");
        assert!(!RUSTC_VERSION.is_empty() && RUSTC_VERSION.starts_with("rustc "));
        assert_ne!(
            rustc_release(),
            "unknown",
            "release parsed from the -vV line"
        );
    }

    /// The section record and runtime updater are derived independently from
    /// one compile input.  This catches build-script/runtime hash drift before
    /// the release cutter performs the same comparison on shipped bytes.
    #[test]
    fn embedded_update_pin_matches_runtime_authority() {
        assert_eq!(EMBEDDED_UPDATE_PIN_SHA256.len(), 64);
        assert!(
            EMBEDDED_UPDATE_PIN_SHA256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        );

        let runtime = aterm_update::compiled_update_pin_sha256();
        if runtime == "empty" {
            assert_eq!(EMBEDDED_UPDATE_PIN_SHA256, "0".repeat(64));
        } else {
            assert_eq!(runtime, EMBEDDED_UPDATE_PIN_SHA256);
        }

        #[cfg(target_vendor = "apple")]
        assert_eq!(
            std::str::from_utf8(&ATERM_UPDATE_PIN_RECORD),
            Ok(EMBEDDED_UPDATE_PIN_SHA256)
        );
    }

    /// About gains exactly one `compiler` row, in the documented shape.
    #[test]
    fn about_fields_has_the_compiler_row() {
        let fields = about_fields();
        let compiler: Vec<&String> = fields
            .iter()
            .filter(|(k, _)| *k == "compiler")
            .map(|(_, v)| v)
            .collect();
        assert_eq!(compiler.len(), 1, "exactly one compiler row");
        let row = compiler[0];
        assert!(
            row.starts_with("rustc "),
            "leads with the compiler name: {row}"
        );
        assert!(
            row.contains(compiler_commit_short()),
            "carries the slug: {row}"
        );
        assert!(
            row.contains(" \u{00b7} rust") || row.contains(" \u{00b7} trust"),
            "{row}"
        );
        assert!(
            row.contains("trust_verify on") || row.contains("trust_verify off"),
            "{row}"
        );
        // And the version row is the suffixed display form.
        let version = fields
            .iter()
            .find(|(k, _)| *k == "version")
            .map(|(_, v)| v.as_str());
        assert_eq!(version, Some(version_display()));
    }

    /// The ctl line stays greppable key=value and gains the compiler keys.
    #[test]
    fn control_line_carries_compiler_provenance() {
        let line = control_line();
        assert!(line.starts_with("OK version="), "framing preserved");
        for key in [
            "build=",
            "commit=",
            "built=",
            "rustc=",
            "rustc_commit=",
            "rustc_host=",
            "flavor=",
            "profile=",
            "trust_verify=",
            "update_pin_sha256=",
            "signature=",
        ] {
            assert!(line.contains(&format!(" {key}")), "has {key}: {line}");
        }
        // key=value framing: no value may smuggle a space (would break grep/awk use).
        assert!(
            line.trim_end()
                .split(' ')
                .skip(1)
                .all(|kv| kv.contains('=')),
            "every token after OK is key=value: {line}"
        );
    }
}
