// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

/// Parsed `rustc -vV` fields for provenance stamping.
pub(crate) struct RustcVv {
    /// Full first line of `rustc -vV`.
    pub version_line: String,
    /// Full `commit-hash:` value (or `"unknown"`).
    pub commit: String,
    /// The compiler's `host:` triple.
    pub host: String,
    /// Trust compiler version (`trust:` line or `(trustc <version>)` parenthetical), or `""`.
    pub trust_version: String,
}

fn trustc_parenthetical(first_line: &str) -> Option<String> {
    let rest = first_line.split_once("(trustc ")?.1;
    let version = rest.split(')').next()?.trim();
    (!version.is_empty()).then(|| version.to_string())
}

/// Parse `rustc -vV` output.
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

/// Classifies the compiler flavor as `"r"` (upstream Rust) or `"t"` (Trust).
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

    const UPSTREAM_VV: &str = "rustc 1.96.0 (ac68faa20 2026-05-25) (Homebrew)\n\
                               binary: rustc\n\
                               commit-hash: ac68faa20c58cbccd01ee7208bf3b6e93a7d7f96\n\
                               commit-date: 2026-05-25\n\
                               host: aarch64-apple-darwin\n\
                               release: 1.96.0\n\
                               LLVM version: 22.1.6";

    const TRUST_VV: &str = "rustc 1.96.0-dev (58b453c80 2026-06-27)\n\
                            binary: rustc\n\
                            commit-hash: 58b453c80b3a2d5a005ecb7f98f8a2491c03e598\n\
                            commit-date: 2026-06-27\n\
                            host: aarch64-apple-darwin\n\
                            release: 1.96.0-dev\n\
                            LLVM version: 22.1.2";

    const TRUST_VV_MARKED: &str = "rustc 1.96.0-dev (7e631b2a4 2026-07-04) (trustc)\n\
                                   binary: trustc\n\
                                   commit-hash: 7e631b2a4830f36d318177dd5869ce601340dd83\n\
                                   commit-date: 2026-07-04\n\
                                   host: aarch64-apple-darwin\n\
                                   release: 1.96.0-dev\n\
                                   LLVM version: 22.1.2";

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

    #[test]
    fn parse_degrades_to_unknown_not_panic() {
        let p = parse_rustc_vv("");
        assert_eq!(p.version_line, "unknown");
        assert_eq!(p.commit, "unknown");
        assert_eq!(p.host, "unknown");
        assert_eq!(p.trust_version, "");
    }

    #[test]
    fn parses_trusts_own_version_apart_from_the_rust_compat_release() {
        let p = parse_rustc_vv(TRUST_VV_RUSTC_NAME);
        assert_eq!(p.trust_version, "0.1.0");
        assert_eq!(p.version_line.split_whitespace().nth(1), Some("1.99.0-dev"));
        assert_eq!(p.host, "x86_64-unknown-linux-gnu");
        let named = "rustc 1.99.0-dev (2b118046a 2026-07-29) (trustc 0.1.0)\nbinary: trustc";
        assert_eq!(parse_rustc_vv(named).trust_version, "0.1.0");
        let both = "rustc 1.99.0-dev (2b118046a 2026-07-29) (trustc 9.9.9)\ntrust: 0.1.0";
        assert_eq!(parse_rustc_vv(both).trust_version, "0.1.0");
        assert_eq!(parse_rustc_vv(TRUST_VV).trust_version, "");
        assert_eq!(parse_rustc_vv(TRUST_VV_MARKED).trust_version, "");
        assert_eq!(
            trustc_parenthetical("rustc 1.99.0-dev (x 2026-01-01) (trustc )"),
            None
        );
    }

    #[test]
    fn detect_flavor_reads_only_real_evidence() {
        let sealed_linux = "rustc 1.99.0-dev (ec2b5c59f 2026-09-01)\nbinary: rustc\n\
            host: aarch64-unknown-linux-gnu\nrelease: 1.99.0-dev\ntrust: 0.1.0";
        let cases: [(&str, &str, &str, Option<&str>, &str); 14] = [
            (
                "upstream",
                UPSTREAM_VV,
                "/opt/homebrew/bin/rustc",
                None,
                "r",
            ),
            (
                "upstream stable toolchain",
                UPSTREAM_VV,
                "rustc",
                Some("stable"),
                "r",
            ),
            (
                "trust: line under the rustc name",
                TRUST_VV_RUSTC_NAME,
                "rustc",
                None,
                "t",
            ),
            ("sealed linux trust: line", sealed_linux, "rustc", None, "t"),
            (
                "empty trust: line",
                "rustc 1.99.0-dev\nbinary: rustc\ntrust:",
                "rustc",
                None,
                "r",
            ),
            (
                "trust: not at line start",
                "rustc 1.99.0-dev\nnote: trust: 0.1.0",
                "rustc",
                None,
                "r",
            ),
            ("both markers", TRUST_VV_MARKED, "rustc", None, "t"),
            (
                "first-line parenthetical only",
                "rustc 1.96.0-dev (7e631b2a4 2026-07-04) (trustc)\nbinary: rustc",
                "rustc",
                None,
                "t",
            ),
            (
                "binary: trustc only",
                "rustc 1.96.0-dev (7e631b2a4 2026-07-04)\nbinary: trustc",
                "rustc",
                None,
                "t",
            ),
            (
                "versioned parenthetical",
                "rustc 1.99.0-dev (2b118046a 2026-07-29) (trustc 0.1.0)\nbinary: rustc",
                "rustc",
                None,
                "t",
            ),
            (
                "(trustc) past the first line",
                "rustc 1.96.0 (ac68faa20 2026-05-25) (Homebrew)\nbinary: rustc\nnote: (trustc)",
                "rustc",
                None,
                "r",
            ),
            (
                "pre-marker trust by path",
                TRUST_VV,
                "/Users//example/trust/build/host/stage2/bin/rustc",
                None,
                "t",
            ),
            (
                "pre-marker trust by toolchain",
                TRUST_VV,
                "rustc",
                Some("trust"),
                "t",
            ),
            (
                "a -dev release alone",
                TRUST_VV,
                "/usr/local/bin/rustc",
                None,
                "r",
            ),
        ];
        for (name, vv, path, toolchain, want) in cases {
            assert_eq!(detect_flavor(vv, path, toolchain), want, "{name}");
        }
    }
}
