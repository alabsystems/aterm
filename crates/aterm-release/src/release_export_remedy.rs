// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What an export refusal asks of whoever reads it: the source fixed, never the export, and the
//! dev set's check that finds the same reds offline before a push.

/// What an export refusal asks: the source fixed, never the export; the dev set's check that
/// finds the engine's content-guard reds offline before a push ([`aterm_verify::export_scan`]);
/// and, where gitleaks found a secret's shape, [`GITLEAKS_FIX`].
pub(super) fn refusal_fix(findings: &[&(String, Vec<String>)]) -> String {
    let mut fix = format!(
        "fix the SOURCE, never the export — patterns: the engine's \
         baseline/forbidden-content.txt + publish/forbidden-extra.txt; `targo --unverified verify \
         {}` finds the forbidden-content, private-reference and gitleaks reds before a push",
        aterm_verify::checks::Check::ExportScan.name()
    );
    if findings
        .iter()
        .any(|(name, _)| super::gitleaks_report(name).is_some())
    {
        fix.push_str(GITLEAKS_FIX);
    }
    fix
}

/// How a line gitleaks reads as a secret is fixed: in the source, because the engine refuses a
/// `.gitleaks.toml` allowlist.
const GITLEAKS_FIX: &str = "; a gitleaks finding is fixed in the source with a plain word or \
                            the value bound apart from its keyword, never a .gitleaks.toml \
                            allowlist, which the engine refuses";

#[cfg(test)]
#[path = "release_export_remedy_tests.rs"]
mod tests;
