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
mod tests {
    use super::super::{EngineRun, Outcome, export_outcome};
    use super::GITLEAKS_FIX;
    use std::path::Path;

    /// AN EXPORT REFUSAL NAMES THE CHECK THAT FINDS IT BEFORE A PUSH: a content guard's refusal
    /// asks for the fix in the source and names the dev set's `verify export-scan`, and a
    /// gitleaks refusal also says how such a line is fixed ([`GITLEAKS_FIX`]); a refusal of the
    /// forbidden-content guard alone does not.
    #[test]
    fn an_export_refusal_names_the_check_that_finds_it_before_a_push() {
        let (log, reports) = (Path::new("/c/dry-run.log"), Path::new("/c/reports"));
        let remedy =
            |text: &str| match export_outcome(&EngineRun::read(1, text), "r", None, log, reports) {
                Outcome::Refuse(r) => r.remedy,
                other => panic!("{text:?}: {other:?}"),
            };
        let exported = "== exported 1 of 1 tracked files from r\n";
        let stopped = "FAIL: guards failed — nothing was pushed\n";
        let check = format!(
            "`targo --unverified verify {}`",
            aterm_verify::checks::Check::ExportScan.name()
        );
        let leaks = remedy(&format!(
            "{exported}== guard ok: no forbidden content\n== GUARD FAIL: gitleaks findings (see \
             /o/gitleaks-adjudicated.json)\n  a/b.rs:7 generic-api-key\n{stopped}"
        ));
        let content = remedy(&format!(
            "{exported}== GUARD FAIL: forbidden content (1 hits; first 5):\n  x.rs:1\n{stopped}"
        ));
        for fix in [&leaks, &content] {
            assert!(fix.starts_with("fix the SOURCE, never the export"), "{fix}");
            assert!(fix.contains(&check), "{fix}");
        }
        assert!(leaks.ends_with(GITLEAKS_FIX), "{leaks}");
        assert!(!content.contains(GITLEAKS_FIX), "{content}");
    }
}
