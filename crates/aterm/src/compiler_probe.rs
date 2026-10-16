// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// Compiler-provenance probing, shared by `build.rs` (which `include!`s it to parse `$RUSTC -vV`
// and classify the compiler) and `main.rs` (which mounts it `#[cfg(test)]` to test the same code).
// Pure std, no I/O. No `//!`: inner doc comments are illegal where include! splices this file.

/// The `rustc -vV` fields the provenance stamp needs. A missing field degrades (`"unknown"`, or
/// `""` for `trust_version`) rather than failing the build.
pub(crate) struct RustcVv {
    /// The full first line, e.g. `rustc 1.96.0 (ac68faa20 2026-05-25) (Homebrew)`.
    pub version_line: String,
    /// The full `commit-hash:` value (40 hex; some distro builds say "unknown").
    pub commit: String,
    /// The `host:` triple, e.g. `aarch64-apple-darwin`.
    pub host: String,
    /// Trust's own version: the `trust:` line, else the first line's `(trustc <version>)`
    /// parenthetical; `""` when neither is present. Distinct from the first line's release token
    /// (`1.99.0-dev`), the Rust release Trust is compatible with.
    pub trust_version: String,
}

/// The `(trustc <version>)` parenthetical of a `-vV` first line, if present:
/// `rustc 1.99.0-dev (2b118046a 2026-07-29) (trustc 0.1.0)` → `0.1.0`. Only
/// the `trustc`-named entry prints it.
fn trustc_parenthetical(first_line: &str) -> Option<String> {
    let rest = first_line.split_once("(trustc ")?.1;
    let version = rest.split(')').next()?.trim();
    (!version.is_empty()).then(|| version.to_string())
}

/// Parse `rustc -vV` output. Tolerant: missing lines yield `"unknown"`, never a panic.
pub(crate) fn parse_rustc_vv(vv: &str) -> RustcVv {
    let field = |key: &str| {
        vv.lines()
            .find_map(|l| l.strip_prefix(key))
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    RustcVv {
        version_line: vv
            .lines()
            .next()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| "unknown".into()),
        commit: field("commit-hash:").unwrap_or_else(|| "unknown".into()),
        host: field("host:").unwrap_or_else(|| "unknown".into()),
        trust_version: field("trust:")
            .or_else(|| vv.lines().next().and_then(trustc_parenthetical))
            .unwrap_or_default(),
    }
}

/// Classify the compiler flavor: `"r"` (upstream Rust) or `"t"` (Trust).
///
/// Evidence only, no override: the About panel and `aterm ctl version` ship this, so the build
/// environment must not be able to set it. Trust when any of:
///   1. the `-vV` self-identification: a non-empty `trust:` line (printed under every entry
///      name, including the `rustc` cargo and targo use), `binary: trustc`, or a `(trustc)` /
///      `(trustc <version>)` first-line parenthetical;
///   2. the `RUSTC` path contains `/trust/` (pre-marker toolchains);
///   3. `RUSTUP_TOOLCHAIN == "trust"`.
///
/// A `-dev` release string is not evidence: any locally built rustc reports it.
pub(crate) fn detect_flavor(
    vv: &str,
    rustc_path: &str,
    rustup_toolchain: Option<&str>,
) -> &'static str {
    let vv_says_trust = vv.lines().any(|l| {
        l.strip_prefix("binary:")
            .is_some_and(|b| b.trim() == "trustc")
            || l.strip_prefix("trust:")
                .is_some_and(|v| !v.trim().is_empty())
    }) || vv
        .lines()
        .next()
        .is_some_and(|first| first.contains("(trustc)") || first.contains("(trustc "));
    if vv_says_trust || rustc_path.contains("/trust/") || rustup_toolchain == Some("trust") {
        return "t";
    }
    "r"
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim `rustc -vV` from an upstream toolchain.
    const UPSTREAM_VV: &str = "rustc 1.96.0 (ac68faa20 2026-05-25) (Homebrew)\n\
                               binary: rustc\n\
                               commit-hash: ac68faa20c58cbccd01ee7208bf3b6e93a7d7f96\n\
                               commit-date: 2026-05-25\n\
                               host: aarch64-apple-darwin\n\
                               release: 1.96.0\n\
                               LLVM version: 22.1.6";

    /// Verbatim `rustc -vV` from a pre-marker Trust: only path/toolchain evidence classifies it.
    const TRUST_VV: &str = "rustc 1.96.0-dev (58b453c80 2026-06-27)\n\
                            binary: rustc\n\
                            commit-hash: 58b453c80b3a2d5a005ecb7f98f8a2491c03e598\n\
                            commit-date: 2026-06-27\n\
                            host: aarch64-apple-darwin\n\
                            release: 1.96.0-dev\n\
                            LLVM version: 22.1.2";

    /// Verbatim `trustc -vV` from a toolchain that self-identifies both ways:
    /// the `(trustc)` parenthetical and `binary: trustc`.
    const TRUST_VV_MARKED: &str = "rustc 1.96.0-dev (7e631b2a4 2026-07-04) (trustc)\n\
                                   binary: trustc\n\
                                   commit-hash: 7e631b2a4830f36d318177dd5869ce601340dd83\n\
                                   commit-date: 2026-07-04\n\
                                   host: aarch64-apple-darwin\n\
                                   release: 1.96.0-dev\n\
                                   LLVM version: 22.1.2";

    /// Verbatim `rustc -vV` from a Trust stage2 run under the `rustc` name, as cargo and targo
    /// drive it: the `trust:` line is its only self-identification (argv0 `trustc` adds the
    /// `(trustc 0.1.0)` parenthetical and `binary: trustc`).
    const TRUST_VV_RUSTC_NAME: &str = "rustc 1.99.0-dev (3a3e781fe 2026-09-14)\n\
                                       binary: rustc\n\
                                       commit-hash: 3a3e781fe082b74d3c4de0ade65b1de3cbee7255\n\
                                       commit-date: 2026-09-14\n\
                                       host: x86_64-unknown-linux-gnu\n\
                                       release: 1.99.0-dev\n\
                                       trust: 0.1.0\n\
                                       LLVM version: 22.1.2";

    #[test]
    fn parses_upstream_vv() {
        let p = parse_rustc_vv(UPSTREAM_VV);
        assert_eq!(
            p.version_line,
            "rustc 1.96.0 (ac68faa20 2026-05-25) (Homebrew)"
        );
        assert_eq!(p.commit, "ac68faa20c58cbccd01ee7208bf3b6e93a7d7f96");
        assert_eq!(p.host, "aarch64-apple-darwin");
        assert_eq!(p.trust_version, "", "upstream rustc has no Trust version");
    }

    /// Trust's version comes from the `trust:` line, else the `(trustc <v>)` parenthetical; the
    /// release token stays `1.99.0-dev` either way.
    #[test]
    fn parses_trusts_own_version_apart_from_the_rust_compat_release() {
        let p = parse_rustc_vv(TRUST_VV_RUSTC_NAME);
        assert_eq!(p.trust_version, "0.1.0");
        assert_eq!(p.version_line.split_whitespace().nth(1), Some("1.99.0-dev"));
        assert_eq!(p.host, "x86_64-unknown-linux-gnu");
        // The trustc-named entry: no `trust:` line in this fixture, so the
        // parenthetical answers.
        let named = "rustc 1.99.0-dev (2b118046a 2026-07-29) (trustc 0.1.0)\nbinary: trustc";
        assert_eq!(parse_rustc_vv(named).trust_version, "0.1.0");
        // A `trust:` line outranks the parenthetical when both are present.
        let both = "rustc 1.99.0-dev (2b118046a 2026-07-29) (trustc 9.9.9)\ntrust: 0.1.0";
        assert_eq!(parse_rustc_vv(both).trust_version, "0.1.0");
        // Pre-marker Trust (`TRUST_VV`) and the bare `(trustc)` marker report none.
        assert_eq!(parse_rustc_vv(TRUST_VV).trust_version, "");
        assert_eq!(parse_rustc_vv(TRUST_VV_MARKED).trust_version, "");
        assert_eq!(
            trustc_parenthetical("rustc 1.99.0-dev (x 2026-01-01) (trustc )"),
            None
        );
    }

    /// Under the `rustc` name cargo drives (no parenthetical, `binary: rustc`,
    /// no env hint) the `trust:` line alone classifies the compiler as Trust.
    #[test]
    fn flavor_trust_from_the_trust_line_under_the_rustc_name() {
        assert_eq!(detect_flavor(TRUST_VV_RUSTC_NAME, "rustc", None), "t");
        // An empty `trust:` value is not a claim.
        let empty = "rustc 1.99.0-dev (3a3e781fe 2026-09-14)\nbinary: rustc\ntrust:";
        assert_eq!(detect_flavor(empty, "rustc", None), "r");
    }

    #[test]
    fn parses_trust_vv() {
        let p = parse_rustc_vv(TRUST_VV);
        assert_eq!(p.version_line, "rustc 1.96.0-dev (58b453c80 2026-06-27)");
        assert_eq!(p.commit, "58b453c80b3a2d5a005ecb7f98f8a2491c03e598");
        assert_eq!(p.host, "aarch64-apple-darwin");
    }

    #[test]
    fn parse_degrades_to_unknown_not_panic() {
        let p = parse_rustc_vv("");
        assert_eq!(p.version_line, "unknown");
        assert_eq!(p.commit, "unknown");
        assert_eq!(p.host, "unknown");
        assert_eq!(
            p.trust_version, "",
            "no Trust version is the empty string, not a word"
        );
    }

    #[test]
    fn flavor_defaults_to_r() {
        // Stock cargo, no trust toolchain.
        assert_eq!(
            detect_flavor(UPSTREAM_VV, "/opt/homebrew/bin/rustc", None),
            "r"
        );
        assert_eq!(detect_flavor(UPSTREAM_VV, "rustc", Some("stable")), "r");
    }

    #[test]
    fn flavor_trust_from_rustc_path_or_toolchain() {
        assert_eq!(
            detect_flavor(
                TRUST_VV,
                "/Users//example/trust/build/host/stage2/bin/rustc",
                None
            ),
            "t"
        );
        assert_eq!(detect_flavor(TRUST_VV, "rustc", Some("trust")), "t");
    }

    /// A self-identifying trustc classifies 't' with no env evidence at all
    /// (bare `rustc` via PATH, no RUSTC, no RUSTUP_TOOLCHAIN).
    #[test]
    fn flavor_trust_from_vv_self_identification() {
        assert_eq!(detect_flavor(TRUST_VV_MARKED, "rustc", None), "t");
        // Either marker alone suffices: version-line parenthetical…
        let line_only = "rustc 1.96.0-dev (7e631b2a4 2026-07-04) (trustc)\nbinary: rustc";
        assert_eq!(detect_flavor(line_only, "rustc", None), "t");
        // …or the `binary: trustc` field.
        let field_only = "rustc 1.96.0-dev (7e631b2a4 2026-07-04)\nbinary: trustc";
        assert_eq!(detect_flavor(field_only, "rustc", None), "t");
        // …or the versioned parenthetical (`(trustc <trust-version>)`).
        let versioned = "rustc 1.99.0-dev (2b118046a 2026-07-29) (trustc 0.1.0)\nbinary: rustc";
        assert_eq!(detect_flavor(versioned, "rustc", None), "t");
        // "(trustc)" past the first line is not the marker: only the version
        // line's parenthetical is the compiler's self-name.
        let stray = "rustc 1.96.0 (ac68faa20 2026-05-25) (Homebrew)\nbinary: rustc\nnote: (trustc)";
        assert_eq!(detect_flavor(stray, "rustc", None), "r");
    }

    #[test]
    fn sealed_linux_trust_identifies_via_its_explicit_version_field() {
        let native = "rustc 1.99.0-dev (ec2b5c59f 2026-09-01)\nbinary: rustc\n\
            commit-hash: ec2b5c59f7ae81dc57dac2a3346cb4c0ef9fad0d\n\
            host: aarch64-unknown-linux-gnu\nrelease: 1.99.0-dev\ntrust: 0.1.0\nLLVM version: 22.1.2";
        assert_eq!(detect_flavor(native, "rustc", None), "t");
        assert_eq!(
            detect_flavor("rustc 1.99.0-dev\ntrust:", "rustc", None),
            "r"
        );
        assert_eq!(
            detect_flavor("rustc 1.99.0-dev\nnote: trust: 0.1.0", "rustc", None),
            "r"
        );
    }

    /// Provenance is evidence only: no environment override moves it in either
    /// direction.
    #[test]
    fn flavor_is_evidence_only_and_cannot_be_overridden() {
        // Trust evidence classifies 't' whatever the environment says.
        assert_eq!(
            detect_flavor(TRUST_VV_MARKED, "/x/trust/bin/rustc", Some("trust")),
            "t"
        );
        // An upstream compiler at an upstream path stays 'r'.
        assert_eq!(
            detect_flavor(UPSTREAM_VV, "/opt/homebrew/bin/rustc", None),
            "r"
        );
    }

    #[test]
    fn dev_release_alone_is_not_trust() {
        // "-dev" with an upstream-looking path, no trust toolchain and no -vV
        // marker stays 'r': -dev is any local build, not Trust.
        let p = parse_rustc_vv(TRUST_VV);
        assert!(p.version_line.contains("-dev"));
        assert_eq!(detect_flavor(TRUST_VV, "/usr/local/bin/rustc", None), "r");
    }
}
