// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Build provenance — version, git commit, build timestamp, and a content signature
//! of the running binary. Surfaced in two places: the cross-platform About overlay
//! ([`about_fields`]) and over the control socket
//! (`aterm-ctl version`, see [`crate::control`]).
//!
//! `VERSION` comes from the shared application identity; `GIT_COMMIT` and
//! `BUILD_TIME` are stamped at compile time by `build.rs`.
//! [`binary_signature`] is computed at runtime from the actual executable, so it
//! reflects the EXACT shipped bytes (the `.app`'s signed binary hashes differently
//! from the bare `target/release` binary — which is correct: it is what's running).

use std::hash::Hasher;
use std::sync::OnceLock;

pub(crate) const AUTHOR_ATTRIBUTION: &str = "By Andrew Yates";
pub(crate) const COMPANY: &str = "ALab";
pub(crate) const AUTHOR_COMPANY_BYLINE: &str = "By Andrew Yates · ALab";
/// The project site About prints in its byline (a live link) and its `Project`
/// row — `alab.systems`, from the one identity every surface shares.
pub(crate) const SITE: &str = aterm_types::identity::SITE;

/// Application identity shared across the one binary. Release builds use the
/// private app-channel claim; ordinary builds use Cargo's source version.
pub const VERSION: &str = aterm_types::version::APP_VERSION;

/// Short git commit the binary was built from — with a `-dirty` suffix when the
/// working tree had uncommitted changes. `"unknown"` when git was unavailable at
/// build time (e.g. a source tarball). Stamped by `build.rs`.
pub const GIT_COMMIT: &str = env!("ATERM_GIT_COMMIT");

/// UTC build timestamp (RFC3339), or `"unknown"`. Stamped by `build.rs`.
pub const BUILD_TIME: &str = env!("ATERM_BUILD_TIME");

/// Monotonic build number, stamped by `build.rs`. The updater's ordering lives
/// here in metadata, independent of the app/source display version above. For a
/// RELEASE this is the number `cargo ship cut` claims in the append-only
/// `RELEASES.ledger` (`max(last + 1, unix_now)` — strictly increasing by
/// construction, epoch-scale) and pins via `SOURCE_DATE_EPOCH`; a dev build
/// falls back to HEAD's committer Unix epoch — the same seconds scale, so dev
/// and release builds stay mutually ordered. Used as the macOS
/// `CFBundleVersion`.
pub const BUILD_NUMBER: &str = env!("ATERM_BUILD_NUMBER");

/// Commits since the newest release tag at build time ("0" when git or the
/// tag was unavailable) — the menu bar's DEV COUNTER: the third slot of a dev
/// build's displayed version (owner, 2026-08-16; a release's third slot is
/// always literal 0, so a nonzero counter can never be mistaken for one).
pub const DEV_COMMITS: &str = env!("ATERM_DEV_COMMITS");

/// Whether the release cutter produced this binary. The cutter supplies
/// `ATERM_APP_RELEASE_VERSION` on both architecture builds and it is
/// deliberately absent from every ordinary build, so its presence at compile
/// time IS the release/dev discriminator — the same fact `APP_VERSION`'s
/// selection already keys on, exposed as a bool for display surfaces (the
/// menu bar's DEV signature).
pub const IS_RELEASE_BUILD: bool = option_env!("ATERM_APP_RELEASE_VERSION").is_some();

/// Full first line of the producing compiler's `-vV`, e.g.
/// `rustc 1.96.0 (ac68faa20 2026-05-25) (Homebrew)` or
/// `rustc 1.99.0-dev (2b118046a 2026-07-29) (trustc)` — trustc leads with the
/// canonical `rustc` token by ecosystem contract (version-sniffing build
/// scripts assert it) and self-identifies in the parenthetical. Stamped by
/// `build.rs`.
pub const COMPILER_VERSION_LINE: &str = env!("ATERM_COMPILER_VERSION_LINE");

/// The producing compiler's FULL git commit hash (`commit-hash:` from `-vV`), or
/// `"unknown"`. This is what tells two coexisting 1.96.0 toolchains apart —
/// per-binary compiler provenance. Stamped by `build.rs`.
pub const COMPILER_COMMIT: &str = env!("ATERM_COMPILER_COMMIT");

/// The producing compiler's host triple (`host:` from `-vV`). Stamped by `build.rs`.
pub const COMPILER_HOST: &str = env!("ATERM_COMPILER_HOST");

/// Trust's OWN version — the `trust:` line of the producing compiler's `-vV`
/// (e.g. `0.1.0`). This is NOT the `1.99.0-dev` in [`COMPILER_VERSION_LINE`]:
/// that token is the RUST release Trust is compatible with, and it stays
/// rustc-shaped on purpose — cargo, targo and every version-sniffing build
/// script parse `rustc <release>` off the first line, and rustup refuses a
/// toolchain without a `bin/rustc` — so the compatibility protocol cannot be
/// where Trust says its own name. It says it here. `""` when the compiler
/// reported no such line (upstream rustc, or a Trust build predating the
/// marker). Stamped by `build.rs`.
pub const COMPILER_TRUST_VERSION: &str = env!("ATERM_COMPILER_TRUST_VERSION");

/// Compiler flavor: `"r"` = upstream Rust, `"t"` = Trust (trustc). Detection order
/// (see `build.rs` / `compiler_probe.rs`): the `-vV` self-identification,
/// `/trust/` in the RUSTC path, `RUSTUP_TOOLCHAIN=trust`, else `"r"`. Evidence
/// only — no build-environment override can flip it (the `build.rs` name below
/// is a `cargo:rustc-env` conduit the build script writes, not one it reads).
pub const COMPILER_FLAVOR: &str = env!("ATERM_COMPILER_FLAVOR");

/// Cargo profile the binary was compiled under (`"debug"`/`"release"`).
pub const BUILD_PROFILE: &str = env!("ATERM_BUILD_PROFILE");

/// `"on"` iff `--cfg trust_verify` was active in this compile, else `"off"`.
pub const TRUST_VERIFY: &str = env!("ATERM_TRUST_VERIFY");

/// Exact lowercase SHA-256 fingerprint of the raw compiled Ed25519 updater key,
/// or the all-zero no-pin sentinel in an ordinary development build.  `build.rs`
/// derives it from the same COMMITTED constant consumed by `aterm-update`
/// (`aterm_update_core::pins::update_channel_signing_pubkey`), never from a build
/// environment variable; the release cutter independently cross-checks this record
/// against both runtime diagnostics and the permanent channel authority.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const EMBEDDED_UPDATE_PIN_SHA256: &str = env!("ATERM_UPDATE_PIN_SHA256");

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
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
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
static ATERM_UPDATE_PIN_RECORD: [u8; 64] = update_pin_record_bytes(EMBEDDED_UPDATE_PIN_SHA256);

/// Short (8-hex) slug of the producing compiler's commit — the version-suffix slug.
/// `"unknown"` when the toolchain didn't report a hash (e.g. some distro builds).
#[must_use]
pub fn compiler_commit_short() -> &'static str {
    if COMPILER_COMMIT.len() >= 8 && COMPILER_COMMIT.bytes().all(|b| b.is_ascii_hexdigit()) {
        &COMPILER_COMMIT[..8]
    } else {
        "unknown"
    }
}

/// The DISPLAY version. There is ONE lineage, Cargo's `MAJOR.MINOR.0`:
/// releases carry it with DEV reset to 0 (e.g. `0.2.0`), ordinary
/// development builds carry it verbatim (e.g. `0.2.1`).
/// The build number, commit, compiler/toolchain, and build
/// time are their OWN provenance rows (see [`about_fields`] / [`control_line`]).
///
/// Display-ONLY by contract: the running build's version is not an input to the
/// updater at all — `aterm_update::check_now` does not take one. Selection is by
/// the release's `vMAJOR.MINOR.PATCH` tag and the apply gate is the monotonic
/// [`BUILD_NUMBER`], so this string can never affect an update comparison.
#[must_use]
pub fn version_display() -> &'static str {
    VERSION
}

/// The compiler's bare release, e.g. `1.96.0` / `1.96.0-dev` — the second token of
/// the `-vV` first line (`rustc <release> (<hash> <date>)`), `"unknown"` if absent.
fn compiler_release() -> &'static str {
    COMPILER_VERSION_LINE
        .split_whitespace()
        .nth(1)
        .unwrap_or("unknown")
}

/// The toolchain's OWN name and version, as About leads with it: `Trust 0.1.0`
/// for a Trust build (`Trust (version unreported)` when the compiler predates
/// the `trust:` marker), `Rust 1.96.0` for upstream. The one place the display
/// decides what to call the compiler; the flavor classifier already weighed the
/// compiler's own self-identification, so this is a rendering, not a guess.
fn toolchain_display() -> String {
    if COMPILER_FLAVOR == "t" {
        if COMPILER_TRUST_VERSION.is_empty() {
            "Trust (version unreported)".to_string()
        } else {
            format!("Trust {COMPILER_TRUST_VERSION}")
        }
    } else {
        format!("Rust {}", compiler_release())
    }
}

/// One human line of compiler provenance for About and `--diagnose`, e.g.
/// `Trust 0.1.0 · trustc 3a3e781f · compatible with Rust 1.99.0-dev · release ·
/// trust_verify on` for a Trust build, or `Rust 1.96.0 · rustc ac68faa2 · release
/// · trust_verify off` for upstream.
///
/// It LEADS with the toolchain's own name and version, and states the Rust
/// release as a separate, labelled fact. Until 2026-09-14 the row read
/// `trustc 1.99.0-dev (3a3e781f) · trust · …`: the rustc-shaped release token
/// Trust prints for cargo's sake, presented as if it were Trust's version — the
/// owner read it as "built by Rust 1.99" and ruled that the surface must be
/// compatible AND explicit: keep the `rustc` protocol, but SAY Trust, say
/// Trust's own version, and say what it is compatible with, as three facts.
/// The release cutter's provenance gate keys on the leading `Trust`.
#[must_use]
pub fn compiler_summary() -> String {
    let slug = compiler_commit_short();
    if COMPILER_FLAVOR == "t" {
        format!(
            "{} \u{00b7} trustc {slug} \u{00b7} compatible with Rust {} \u{00b7} {BUILD_PROFILE} \u{00b7} trust_verify {TRUST_VERIFY}",
            toolchain_display(),
            compiler_release(),
        )
    } else {
        format!(
            "{} \u{00b7} rustc {slug} \u{00b7} {BUILD_PROFILE} \u{00b7} trust_verify {TRUST_VERIFY}",
            toolchain_display(),
        )
    }
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
    let mut fields = vec![
        (
            "tagline",
            "a fast, hardened, AI-introspectable terminal".to_string(),
        ),
        ("author", AUTHOR_ATTRIBUTION.to_string()),
        ("company", COMPANY.to_string()),
        // The byline's live link and the native route's `Project` row / "Open
        // Project Site" button — where aterm comes from (owner, 2026-09-14).
        ("site", SITE.to_string()),
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
    ];
    // WHICH COPY runs (S12): a few stats and at most one small plist read per other
    // copy — observed fresh on every open, so a copy installed since launch shows.
    if let Some(copy) = aterm_update::which_copy::observe() {
        fields.extend(which_copy_rows(&copy));
    }
    fields
}

/// The S12 About rows (`docs/DESIGN-which-copy-runs-2026-08-27.md`): `running` — the
/// path of the running bundle (the executable off macOS) — and, when another
/// `aterm.app` sits in one of the usual places, `another copy` carrying the one
/// sentence `aterm --version` prints: `<path> (<version>) — not the one running; the
/// updater updates only this one`. Both values are spelled by
/// `aterm_update::which_copy`, so the two surfaces cannot drift. Several other copies
/// share the one row, ` · `-separated — the metadata card wraps at exactly that.
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

/// The control-socket (`aterm-ctl version`) response line: a stable, greppable
/// `key=value` form so scripts can parse the running build's provenance. Existing
/// keys keep their meaning (additive only — with ONE deliberate exception:
/// v0.10.0 renamed the compiler keys `rustc=`/`rustc_commit=`/`rustc_host=` to
/// `trustc=`/`trustc_commit=`/`trustc_host=` by owner ruling, because the
/// toolchain that builds aterm is trustc and the surface must say so; on the
/// upstream-stable x86_64 compat slice the same keys carry that slice's stock
/// compiler data — `flavor=r` marks it). `version=` carries the display suffix
/// (`+r.<slug>`/`+t.<slug>`) — nothing machine-side compares it (the updater
/// orders by `build=`, which stays the bare monotonic counter).
///
/// 2026-09-14 ADDED two keys, so the line says what `trustc=1.99.0-dev` never
/// could on its own: `trust=` is Trust's OWN version (the `trust:` line of
/// `-vV` — `none` on an upstream build, `unreported` on a Trust build predating
/// the marker), and `rust_compat=` is the Rust release the compiler is
/// compatible with — the SAME value `trustc=` has always carried, now under a
/// name that says what it is. `trustc=` keeps carrying that rustc-shaped
/// release token, unchanged, for every script that already parses it.
#[must_use]
pub fn control_line() -> String {
    let trust = if COMPILER_FLAVOR != "t" {
        "none"
    } else if COMPILER_TRUST_VERSION.is_empty() {
        "unreported"
    } else {
        COMPILER_TRUST_VERSION
    };
    let update_pin_sha256 = aterm_update::compiled_update_pin_sha256();
    // `master_pin_sha256=` (2026-09-14): the paper master this build trusts — the
    // anchor that actually authorizes a release once the roster tier is armed.
    let master_pin_sha256 = aterm_update::compiled_master_pin_sha256();
    // `objc_contained=` — how many NSExceptions `aterm_objc::exception` has
    // caught inside declared Objective-C methods in THIS process (each one is
    // also an ERROR line in aterm.log). Additive, per the rule above; `0` off
    // macOS, where no such method exists.
    #[cfg(target_os = "macos")]
    let objc_contained = aterm_objc::contained_count();
    #[cfg(not(target_os = "macos"))]
    let objc_contained = 0u64;
    format!(
        "OK version={} build={BUILD_NUMBER} commit={GIT_COMMIT} built={BUILD_TIME} \
         arch={} trustc={} trustc_commit={} trustc_host={COMPILER_HOST} flavor={COMPILER_FLAVOR} \
         trust={trust} rust_compat={} \
         profile={BUILD_PROFILE} trust_verify={TRUST_VERIFY} update_pin_sha256={update_pin_sha256} \
         master_pin_sha256={master_pin_sha256} objc_contained={objc_contained} signature={}\n",
        version_display(),
        std::env::consts::ARCH,
        compiler_release(),
        compiler_commit_short(),
        compiler_release(),
        binary_signature()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The display version is the shared application identity — provenance
    /// (build number, commit, compiler) lives in the other rows.
    #[test]
    fn version_display_is_shared_application_identity() {
        let v = version_display();
        assert!(!v.contains('+'), "no compiler suffix in the version: {v}");
        assert_eq!(v, aterm_types::version::APP_VERSION);
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
        // The raw -vV first line leads with `rustc` today by ecosystem contract;
        // accept a future trustc-led banner too so the toolchain can drop the
        // compat token without breaking this crate.
        assert!(
            !COMPILER_VERSION_LINE.is_empty()
                && (COMPILER_VERSION_LINE.starts_with("rustc ")
                    || COMPILER_VERSION_LINE.starts_with("trustc ")),
            "unexpected -vV first line: {COMPILER_VERSION_LINE}"
        );
        assert_ne!(
            compiler_release(),
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

    /// The S12 rows, pinned: `running` carries the running copy's path; `another copy`
    /// appears only when there is one and carries THE sentence, verbatim — several
    /// copies share the row at ` · ` (the card's wrap point).
    #[test]
    fn which_copy_rows_pin_the_s12_sentences() {
        use aterm_update::which_copy::{OtherCopy, Running, WhichCopy};
        let alone = WhichCopy {
            running: std::path::PathBuf::from("/Applications/aterm.app"),
            kind: Running::InstalledApp,
            others: Vec::new(),
        };
        assert_eq!(
            which_copy_rows(&alone),
            vec![("running", "/Applications/aterm.app".to_string())]
        );
        let other = |path: &str, version: Option<&str>| OtherCopy {
            path: std::path::PathBuf::from(path),
            version: version.map(str::to_string),
        };
        let two = WhichCopy {
            others: vec![
                other("/Users//ana/Applications/aterm.app", Some("0.60.0")),
                other("/opt/homebrew/Caskroom/aterm/0.59.0/aterm.app", None),
            ],
            ..alone.clone()
        };
        assert_eq!(
            which_copy_rows(&two),
            vec![
                ("running", "/Applications/aterm.app".to_string()),
                (
                    "another copy",
                    "/Users//ana/Applications/aterm.app (0.60.0) \u{2014} not the one running; \
                     the updater updates only this one \u{00b7} \
                     /opt/homebrew/Caskroom/aterm/0.59.0/aterm.app (version unknown) \u{2014} \
                     not the one running; the updater updates only this one"
                        .to_string()
                ),
            ]
        );
        // A bare binary (every test run) names its path and promises nothing.
        let binary = WhichCopy {
            running: std::path::PathBuf::from("/Users//ana/aterm/target/release/aterm"),
            kind: Running::Binary,
            others: vec![other("/Applications/aterm.app", Some("0.60.0"))],
        };
        assert_eq!(
            which_copy_rows(&binary),
            vec![
                (
                    "running",
                    "/Users//ana/aterm/target/release/aterm".to_string()
                ),
                (
                    "another copy",
                    "/Applications/aterm.app (0.60.0) \u{2014} not the one running".to_string()
                ),
            ]
        );
        // And the live About carries the `running` row for THIS process.
        let fields = about_fields();
        let running = fields
            .iter()
            .filter(|(k, _)| *k == "running")
            .map(|(_, v)| v.as_str())
            .collect::<Vec<_>>();
        assert_eq!(running.len(), 1, "exactly one running row: {fields:?}");
        assert!(
            std::path::Path::new(running[0]).is_absolute(),
            "names a path: {}",
            running[0]
        );
    }

    /// About gains exactly one `compiler` row, in the documented shape: it LEADS
    /// with the toolchain's own name (`Trust <own version>` / `Rust <release>`),
    /// names the compiler binary and its slug, and — for Trust — states the Rust
    /// release it is compatible with as a separate, labelled fact, so the
    /// rustc-shaped `1.99.0-dev` token can never again read as Trust's version.
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
        assert_eq!(row, &compiler_summary());
        assert!(
            row.contains(compiler_commit_short()),
            "carries the slug: {row}"
        );
        assert!(
            row.contains(&format!(
                " \u{00b7} {BUILD_PROFILE} \u{00b7} trust_verify {TRUST_VERIFY}"
            )),
            "{row}"
        );
        if COMPILER_FLAVOR == "t" {
            assert!(row.starts_with("Trust"), "leads with Trust: {row}");
            assert!(
                row.contains(&format!(
                    " \u{00b7} trustc {} \u{00b7} ",
                    compiler_commit_short()
                )),
                "names the trustc binary: {row}"
            );
            assert!(
                row.contains(&format!(
                    " \u{00b7} compatible with Rust {} \u{00b7} ",
                    compiler_release()
                )),
                "the Rust release is a labelled compatibility fact: {row}"
            );
            if !COMPILER_TRUST_VERSION.is_empty() {
                assert!(
                    row.starts_with(&format!("Trust {COMPILER_TRUST_VERSION} \u{00b7} ")),
                    "Trust's own version, from the trust: line: {row}"
                );
                assert_ne!(
                    COMPILER_TRUST_VERSION,
                    compiler_release(),
                    "Trust's own version is not the rust-compat release"
                );
            } else {
                assert!(
                    row.starts_with("Trust (version unreported) \u{00b7} "),
                    "{row}"
                );
            }
            assert!(!row.starts_with("Rust "), "{row}");
        } else {
            assert!(
                row.starts_with(&format!("Rust {} \u{00b7} rustc ", compiler_release())),
                "upstream leads with Rust and its release: {row}"
            );
            assert!(
                !row.contains("Trust"),
                "an upstream build claims no Trust: {row}"
            );
        }
        // And the version row is the suffixed display form.
        let version = fields
            .iter()
            .find(|(k, _)| *k == "version")
            .map(|(_, v)| v.as_str());
        assert_eq!(version, Some(version_display()));
    }

    /// About's header rows say where aterm comes from: author, company and the
    /// site (`alab.systems`) — the site row is what the byline links and the
    /// native route's "Open Project Site" opens.
    #[test]
    fn about_fields_carry_the_origin() {
        let fields = about_fields();
        let row = |k: &str| {
            fields
                .iter()
                .find(|(key, _)| *key == k)
                .map(|(_, v)| v.as_str())
                .unwrap_or_else(|| panic!("no {k} row: {fields:?}"))
        };
        assert_eq!(row("site"), aterm_types::identity::SITE);
        assert_eq!(row("company"), aterm_types::identity::COMPANY);
        assert!(
            row("author").ends_with(aterm_types::identity::AUTHOR),
            "{}",
            row("author")
        );
        assert_eq!(
            fields.iter().filter(|(k, _)| *k == "site").count(),
            1,
            "exactly one site row"
        );
    }

    /// The ctl line stays greppable key=value and gains the compiler keys —
    /// including `trust=` (Trust's own version) and `rust_compat=` (the Rust
    /// release), which together say what `trustc=` alone never could.
    #[test]
    fn control_line_carries_compiler_provenance() {
        let line = control_line();
        assert!(line.starts_with("OK version="), "framing preserved");
        let value = |key: &str| {
            line.split(' ')
                .find_map(|kv| kv.strip_prefix(key))
                .map(str::trim_end)
                .unwrap_or_else(|| panic!("no {key} in {line}"))
        };
        // `rust_compat=` is the same token `trustc=` has always carried.
        assert_eq!(value("rust_compat="), value("trustc="));
        assert_eq!(value("rust_compat="), compiler_release());
        match COMPILER_FLAVOR {
            "t" if COMPILER_TRUST_VERSION.is_empty() => assert_eq!(value("trust="), "unreported"),
            "t" => assert_eq!(value("trust="), COMPILER_TRUST_VERSION),
            _ => assert_eq!(value("trust="), "none"),
        }
        for key in [
            "build=",
            "commit=",
            "built=",
            "trustc=",
            "trustc_commit=",
            "trustc_host=",
            "flavor=",
            "trust=",
            "rust_compat=",
            "profile=",
            "trust_verify=",
            "update_pin_sha256=",
            "master_pin_sha256=",
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
