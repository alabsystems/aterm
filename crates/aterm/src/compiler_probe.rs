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
#[path = "compiler_probe_tests.rs"]
mod tests;
