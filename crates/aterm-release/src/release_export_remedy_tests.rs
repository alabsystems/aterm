// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
