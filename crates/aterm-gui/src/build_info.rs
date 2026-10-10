// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Build provenance — version, git commit, build timestamp, and a content signature
//! of the running binary. Surfaced in two places: the native Settings `/about` route
//! ([`about_fields`]) and over the control socket
//! (`aterm-ctl version`, see [`crate::control`]).
//!
//! Every build fact — the version, commit, build number, build time, compiler,
//! profile and release marker — is the binary's [`BuildStamp`], compiled into
//! `crates/aterm` and installed there first thing in `main`; this library
//! compiles none of them in and reads them at runtime (`stamp()`).
//! [`binary_signature`] is computed at runtime from the actual executable, so it
//! reflects the EXACT shipped bytes (the `.app`'s signed binary hashes differently
//! from the bare `target/release` binary — which is correct: it is what's running).

use aterm_types::version::BuildStamp;
use std::hash::Hasher;
use std::sync::OnceLock;

pub(crate) const AUTHOR_ATTRIBUTION: &str = "By Andrew Yates";
pub(crate) const COMPANY: &str = "ALab";
pub(crate) const AUTHOR_COMPANY_BYLINE: &str = "By Andrew Yates · ALab";
/// The author's SHORT form — initial plus surname, the last rung of the native
/// About byline's fit ladder. It exists because the rung above it
/// ([`aterm_types::identity::AUTHOR`]) is 164.6pt wide at the Body step under 2×
/// Dynamic Type, which does not fit the 164.0pt hero column a 240pt host gives
/// it: measured, the full name is not a floor. Initial + surname is the shortest
/// form that is still the AUTHOR, which is what the narrow large-type byline
/// keeps. [`author_short_names_the_author`] pins it to the shared identity, so a
/// name change cannot leave a stale abbreviation behind.
pub(crate) const AUTHOR_SHORT: &str = "A. Yates";
/// The project site About prints in its byline (a live link) and its `Project`
/// row — `alab.systems`, from the one identity every surface shares.
pub(crate) const SITE: &str = aterm_types::identity::SITE;

/// What this library's OWN test binary reads: a clean, stamped, CURRENT Trust
/// build.
///
/// Its tests were written against a compile-time stamp that always carried the
/// tree's real commit and its committer epoch, so they exercise the stamped
/// path (a hex commit) and play a build newer than every release fixture they
/// name (`running + 1` is a successor, `v0.100.0`'s number is in the past). A
/// fixed stamp keeps both true and makes the suite deterministic: the commit is
/// a recognisable non-commit, and the build number is 2065's epoch, far above
/// any real release and well inside the sub-2^32 range `CFBundleVersion` takes.
/// The version is the workspace's (test-only: the library itself compiles none
/// in), and the compiler is a fixed Trust `-vV`.
#[cfg(test)]
const TEST_STAMP: BuildStamp = BuildStamp {
    app_version: env!("CARGO_PKG_VERSION"),
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
    release_build: false,
};

/// The running binary's build facts: the stamp `crates/aterm` installs first
/// thing in `main` (`aterm_types::version`), which every surface here (About,
/// `aterm ctl version`, the menu signature, the handoff identity, the bundle's
/// build number) reads through the accessors below. A process whose `main`
/// installed none (the dev-only `aterm-gui` bin and its examples) reads
/// [`BuildStamp::UNSTAMPED`]: the facts a build without git always carried
/// (`unknown`, build `0`), which the handoff already refuses outside
/// `debug_assertions`. This library's own test binary reads `TEST_STAMP`.
fn stamp() -> &'static BuildStamp {
    #[cfg(test)]
    let unstamped = &TEST_STAMP;
    #[cfg(not(test))]
    let unstamped = &BuildStamp::UNSTAMPED;
    aterm_types::version::installed().unwrap_or(unstamped)
}

/// Application identity shared across the one binary: `[workspace.package]
/// version`, as written, in every build.
#[must_use]
pub(crate) fn version() -> &'static str {
    stamp().app_version
}

/// Short git commit the binary was built from, with a `-dirty` suffix when the
/// working tree had uncommitted changes. `"unknown"` when git was unavailable at
/// build time (e.g. a source tarball).
#[must_use]
pub(crate) fn git_commit() -> &'static str {
    stamp().git_commit
}

/// Full source identity for authenticated native-artifact probes. Unlike the
/// display abbreviation this binds all forty hexadecimal commit characters.
#[must_use]
pub(crate) fn git_commit_full() -> &'static str {
    stamp().git_commit_full
}

/// `"true"` when the build's tree was dirty.
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

/// Monotonic build number. The updater's ordering lives
/// here in metadata, independent of the app/source display version above. For a
/// RELEASE this is the number `targo --unverified ship cut` claims in the append-only
/// `RELEASES.ledger` (`max(last + 1, unix_now)`, strictly increasing by
/// construction, epoch-scale) and pins via `SOURCE_DATE_EPOCH`; a dev build
/// falls back to HEAD's committer Unix epoch, the same seconds scale, so dev
/// and release builds stay mutually ordered. Used as the macOS
/// `CFBundleVersion`.
#[must_use]
pub(crate) fn build_number() -> &'static str {
    stamp().build_number
}

/// Commits since the newest release tag at build time ("0" when git or the
/// tag was unavailable): the version signature's DEV COUNTER
/// ([`crate::menu::version_signature`]: the macOS menu bar's Version title and
/// the Windows tab strip's version label), the third slot of a dev build's
/// displayed version (owner, 2026-08-16; a release's third slot is always
/// literal 0, so a nonzero counter can never be mistaken for one).
#[must_use]
pub(crate) fn dev_commits() -> &'static str {
    stamp().dev_commits
}

/// Whether a release lane produced this binary. `targo --unverified ship cut` (both
/// architecture builds) and the Linux release lane set `ATERM_RELEASE_BUILD`
/// for the binary crate's compile, and no ordinary build does, so its presence
/// at compile time IS the release/dev discriminator — read by display surfaces
/// only (the DEV signature of [`crate::menu::version_signature`]). No Windows
/// lane goes through the release cutter, so every Windows build reads `· DEV`
/// today, truthfully; only a Windows release lane may ever set it. It carries no
/// version: every build reports [`version`].
#[must_use]
pub(crate) fn is_release_build() -> bool {
    stamp().release_build
}

/// Full first line of the producing compiler's `-vV`, e.g.
/// `rustc 1.96.0 (ac68faa20 2026-05-25) (Homebrew)` or
/// `rustc 1.99.0-dev (2b118046a 2026-07-29) (trustc)` — trustc leads with the
/// canonical `rustc` token by ecosystem contract (version-sniffing build
/// scripts assert it) and self-identifies in the parenthetical.
#[must_use]
pub(crate) fn compiler_version_line() -> &'static str {
    stamp().compiler_version_line
}

/// The producing compiler's FULL git commit hash (`commit-hash:` from `-vV`), or
/// `"unknown"`. This is what tells two coexisting 1.96.0 toolchains apart —
/// per-binary compiler provenance.
#[must_use]
pub(crate) fn compiler_commit() -> &'static str {
    stamp().compiler_commit
}

/// The producing compiler's host triple (`host:` from `-vV`).
#[must_use]
pub(crate) fn compiler_host() -> &'static str {
    stamp().compiler_host
}

/// Trust's OWN version — the `trust:` line of the producing compiler's `-vV`
/// (e.g. `0.1.0`). This is NOT the `1.99.0-dev` in [`compiler_version_line`]:
/// that token is the RUST release Trust is compatible with, and it stays
/// rustc-shaped on purpose — cargo, targo and every version-sniffing build
/// script parse `rustc <release>` off the first line, and rustup refuses a
/// toolchain without a `bin/rustc` — so the compatibility protocol cannot be
/// where Trust says its own name. It says it here. `""` when the compiler
/// reported no such line (upstream rustc, or a Trust build predating the
/// marker).
#[must_use]
pub(crate) fn compiler_trust_version() -> &'static str {
    stamp().compiler_trust_version
}

/// Compiler flavor: `"r"` = upstream Rust, `"t"` = Trust (trustc). Detection order
/// (see `crates/aterm/build.rs` / `compiler_probe.rs`): the `-vV`
/// self-identification, `/trust/` in the RUSTC path, `RUSTUP_TOOLCHAIN=trust`,
/// else `"r"`. Evidence only — no build-environment override can flip it.
#[must_use]
pub(crate) fn compiler_flavor() -> &'static str {
    stamp().compiler_flavor
}

/// Cargo profile the binary was compiled under (`"debug"`/`"release"`).
#[must_use]
pub(crate) fn build_profile() -> &'static str {
    stamp().build_profile
}

/// `"on"` iff the Trust verification pipeline was in the binary's compile, else
/// `"off"`.
#[must_use]
pub(crate) fn trust_verify() -> &'static str {
    stamp().trust_verify
}

/// Exact lowercase SHA-256 fingerprint of the raw paper master key — the one anchor
/// that authorizes a release — or the all-zero sentinel when none is pinned (a fork).
/// `build.rs` derives it from the same COMMITTED constant `aterm-update` reports
/// (`aterm_update_core::pins::PAPER_MASTER_PUBKEYS[0]`), never from a build environment
/// variable; the release cutter independently cross-checks this record against both
/// runtime diagnostics and the committed master.
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
pub(crate) fn compiler_commit_short() -> &'static str {
    let commit = compiler_commit();
    if commit.len() >= 8 && commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        &commit[..8]
    } else {
        "unknown"
    }
}

/// The DISPLAY version: `[workspace.package] version` (`MAJOR.MINOR.0`), the
/// same in a release and a development build. The build number, commit,
/// compiler/toolchain, and build time are their OWN provenance rows (see
/// [`about_fields`] / [`control_line`]).
///
/// Display-ONLY by contract: the running build's version is not an input to the
/// updater at all — `aterm_update::check_now` does not take one. Selection is by
/// the release's `vMAJOR.MINOR.PATCH` tag and the apply gate is the monotonic
/// [`build_number`], so this string can never affect an update comparison.
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

/// The toolchain's OWN name and version, as About leads with it: `Trust 0.1.0`
/// for a Trust build (`Trust (version unreported)` when the compiler predates
/// the `trust:` marker), `Rust 1.96.0` for upstream. The one place the display
/// decides what to call the compiler; the flavor classifier already weighed the
/// compiler's own self-identification, so this is a rendering, not a guess.
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

/// A content signature of the RUNNING binary: a 16-hex FxHash of `current_exe()`,
/// computed once and cached. Identifies the exact bytes that shipped; `"unknown"` if
/// the executable can't be read.
///
/// This is a build FINGERPRINT, not a cryptographic attestation — it uses the
/// workspace's non-cryptographic FxHash to avoid pulling in a crypto dependency. It
/// is enough to tell two builds apart and to confirm "the binary I'm running is the
/// one I shipped", which is its purpose in the native Settings `/about` route and
/// `aterm-ctl version`.
#[must_use]
pub(crate) fn binary_signature() -> &'static str {
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

/// Structured `(key, value)` provenance rows — the SINGLE source the native Settings
/// `/about` route PAINTS, `controls about` serialises (that route's semantic frame) and
/// its COPY action puts on the clipboard ([`crate::about::provenance_text`]), so its
/// pixels and its machine-readable text can never disagree. Cross-platform.
#[must_use]
pub(crate) fn about_fields() -> Vec<(&'static str, String)> {
    let mut fields = vec![
        (
            "tagline",
            "a fast, hardened, AI-introspectable terminal".to_string(),
        ),
        ("author", AUTHOR_ATTRIBUTION.to_string()),
        ("company", COMPANY.to_string()),
        // The byline's live link and the native route's `Website` row / "Open
        // Project Site" button — where aterm comes from (owner, 2026-09-14).
        ("site", SITE.to_string()),
        ("version", version_display().to_string()),
        ("build", build_number().to_string()),
        ("commit", git_commit().to_string()),
        ("built", build_time().to_string()),
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
/// sentence `aterm --version` prints: `<path> (<version>) — the updater leaves it
/// alone`. Both values are spelled by
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
    // `update_pin_sha256=`: the paper master this build trusts — the one anchor that
    // authorizes a release. A pure function of a compiled-in const, so it is
    // computed once, not on every `version` (every `aterm ctl` call's probe).
    static UPDATE_PIN_FINGERPRINT: OnceLock<String> = OnceLock::new();
    let update_pin_sha256 =
        UPDATE_PIN_FINGERPRINT.get_or_init(aterm_update::compiled_update_pin_sha256);
    // `objc_contained=` — how many NSExceptions `aterm_objc::exception` has
    // caught inside declared Objective-C methods in THIS process (each one is
    // also an ERROR line in aterm.log). Additive, per the rule above; `0` off
    // macOS, where no such method exists.
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
mod tests {
    use super::*;

    /// Every rung of the About byline's ladder is the SAME author, spelled
    /// shorter: the short form is the shared identity's initial and surname, and
    /// the long forms contain the shared identity whole. Renaming
    /// `identity::AUTHOR` without respelling these therefore fails here rather
    /// than shipping a hero that attributes aterm to two different people.
    #[test]
    fn author_short_names_the_author() {
        let author = aterm_types::identity::AUTHOR;
        let (given, surname) = author
            .split_once(' ')
            .expect("the shared author identity is given name plus surname");
        assert_eq!(
            AUTHOR_SHORT,
            format!("{}. {surname}", &given[..1]),
            "the byline's short rung is the shared author's initial and surname"
        );
        assert!(AUTHOR_ATTRIBUTION.ends_with(author));
        assert!(AUTHOR_COMPANY_BYLINE.starts_with(AUTHOR_ATTRIBUTION));
        assert!(AUTHOR_COMPANY_BYLINE.ends_with(COMPANY));
    }

    /// The display version is the shared application identity — provenance
    /// (build number, commit, compiler) lives in the other rows.
    #[test]
    fn version_display_is_shared_application_identity() {
        let v = version_display();
        assert!(!v.contains('+'), "no compiler suffix in the version: {v}");
        assert_eq!(v, stamp().app_version);
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
                    "/Users//ana/Applications/aterm.app (0.60.0) \u{2014} the updater leaves \
                     it alone \u{00b7} /opt/homebrew/Caskroom/aterm/0.59.0/aterm.app (version \
                     unknown) \u{2014} the updater leaves it alone"
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
                    "/Applications/aterm.app (0.60.0)".to_string()
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
                " \u{00b7} {} \u{00b7} trust_verify {}",
                build_profile(),
                trust_verify()
            )),
            "{row}"
        );
        if compiler_flavor() == "t" {
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
            let trust = compiler_trust_version();
            if !trust.is_empty() {
                assert!(
                    row.starts_with(&format!("Trust {trust} \u{00b7} ")),
                    "Trust's own version, from the trust: line: {row}"
                );
                assert_ne!(
                    trust,
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
        match compiler_flavor() {
            "t" if compiler_trust_version().is_empty() => {
                assert_eq!(value("trust="), "unreported");
            }
            "t" => assert_eq!(value("trust="), compiler_trust_version()),
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
