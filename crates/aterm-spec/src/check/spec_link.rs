// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a `trust-ir spec-link` report may be credited as.
//!
//! aterm's lowered modules are explicitly design-only, so a violation-free
//! analysis exits 1 with a non-certifying status and one design-only reason
//! per machine. That honest outcome, and a certifying exit 0, are the only two
//! a check may consume: everything else, a structural or obligation diagnostic
//! included, is a failure, and a report whose shape is not exactly TrustIr's
//! grammar fails closed rather than being half-read.

use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Disposition {
    Certifying,
    ExplicitDesignOnly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CoverageMode {
    Linked,
    DesignOnly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CoverageRow<'a> {
    machine: &'a str,
    covered: usize,
    total: usize,
    mode: CoverageMode,
}

fn is_numbered_spec_link_violation(line: &str) -> bool {
    let Some(line) = line.trim_start().strip_prefix('[') else {
        return false;
    };
    let Some((index, violation)) = line.split_once("] [") else {
        return false;
    };
    if parse_canonical_usize(index).is_none() {
        return false;
    }
    let Some((tag, detail)) = violation.split_once("] ") else {
        return false;
    };
    if detail.is_empty() {
        return false;
    }
    tag.strip_prefix("Ob.")
        .and_then(|tag| tag.split_once(' '))
        .is_some_and(|(obligation, name)| {
            parse_canonical_usize(obligation).is_some() && !name.is_empty()
        })
        || ["L1 ", "L2 ", "S0 ", "S1 ", "S2 "]
            .iter()
            .any(|prefix| tag.strip_prefix(prefix).is_some_and(|tag| !tag.is_empty()))
}

/// Fail closed on every structural/Ob/L1/L2/S0-S2 diagnostic TrustIr v27 emits.
/// Parse diagnostics only in their protocol positions: arbitrary model or manifest
/// paths may truthfully contain marker-like text such as `spec-link failed:` or
/// `[Ob.` and must not become false failures.
pub(crate) fn report_has_spec_link_failure(report: &str) -> bool {
    report.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with("error:")
            || line.starts_with("spec-link failed:")
            || is_numbered_spec_link_violation(line)
    })
}

/// Parse only TrustIr's primary machine-count header. Any other `spec-link:` line
/// is protocol drift, which [`classify_spec_link`] fails closed on.
fn parse_spec_link_header(line: &str) -> Option<(usize, &str)> {
    let header = line.strip_prefix("spec-link: ")?;
    let (count, path) = header.split_once(" machine(s) in ")?;
    let count = parse_canonical_usize(count)?;
    (!path.is_empty()).then_some((count, path))
}

fn parse_canonical_usize(raw: &str) -> Option<usize> {
    let value = raw.parse::<usize>().ok()?;
    (raw == value.to_string()).then_some(value)
}

/// Parse one complete TrustIr v27 coverage row. This deliberately validates the
/// whole grammar rather than treating a machine-name prefix and enforcement-mode
/// suffix as evidence: origin, coverage ratio, anchored/waived accounting, and
/// trailing text must all be exactly what TrustIr emits.
fn parse_spec_link_coverage_row(line: &str) -> Option<CoverageRow<'_>> {
    let row = line.strip_prefix("machine \"")?;
    let (machine, row) = row.split_once("\" [")?;
    if machine.is_empty()
        || !machine
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        return None;
    }

    let (origin, row) = row.rsplit_once("] coverage ")?;
    let valid_origin = origin == "embedded"
        || origin
            .strip_prefix("external(")
            .and_then(|path| path.strip_suffix(')'))
            .is_some_and(|path| !path.trim().is_empty());
    if !valid_origin {
        return None;
    }

    let (fraction, row) = row.split_once(" = ")?;
    let (covered, total) = fraction.split_once('/')?;
    let covered = parse_canonical_usize(covered)?;
    let total = parse_canonical_usize(total)?;
    let (percentage, row) = row.split_once("% (")?;

    let (accounting, mode) = if let Some(accounting) = row.strip_suffix(") [linked]") {
        (accounting, CoverageMode::Linked)
    } else {
        let accounting = row.strip_suffix(") [design-only]")?;
        (accounting, CoverageMode::DesignOnly)
    };
    let (anchored, waived) = accounting.split_once(" anchored, ")?;
    let waived = waived.strip_suffix(" waived")?;
    let anchored = parse_canonical_usize(anchored)?;
    let waived = parse_canonical_usize(waived)?;

    if anchored.checked_add(waived)? != covered || covered > total {
        return None;
    }
    let ratio = if total == 0 {
        100.0
    } else {
        covered as f64 / total as f64 * 100.0
    };
    if percentage != format!("{ratio:.1}") {
        return None;
    }

    Some(CoverageRow {
        machine,
        covered,
        total,
        mode,
    })
}

fn parse_spec_link_coverage(report: &str) -> Option<Vec<CoverageRow<'_>>> {
    report
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("machine"))
        .map(parse_spec_link_coverage_row)
        .collect()
}

/// Distinguish a `trust-ir` too old for this repo's lowering from a real finding:
/// `--version` does not change across rebuilds, so the tell is exit 2 with a parse
/// error on a `spec_module` keyword. Returned as a suffix for the failure messages.
pub(crate) fn stale_trust_ir_hint(exit_code: Option<i32>, report: &str) -> String {
    if exit_code == Some(2) && report.contains("unexpected token in spec_module") {
        "\n\n  LIKELY A STALE `trust-ir` — NOT A SPEC FAILURE.\n  \
         Exit 2 is the tool's I/O/parse code, and the report rejects a token this\n  \
         repo's lowering emits, so the installed binary predates that grammar.\n  \
         Update it:\n    \
         aterm pkg install trust-ir\n  \
         `--version` is unchanged across builds, so confirm via the binary's \
         mtime/size (the VERIFY ESCALATION TIER line names both)."
            .to_string()
    } else {
        String::new()
    }
}

/// Classify the two consumable outcomes: an exit-0 `certifying` report, or TrustIr
/// v27's exit-1 violation-free report whose modules and reasons are all `design-only`.
/// Anything else is rejected.
pub(crate) fn classify_spec_link(exit_code: Option<i32>, report: &str) -> Option<Disposition> {
    if report_has_spec_link_failure(report) {
        return None;
    }

    let statuses: Vec<_> = report
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("spec-link-status:"))
        .collect();
    let [status] = statuses.as_slice() else {
        return None;
    };
    // Every `spec-link:` line must be the one header: a new or malformed record
    // must be understood before the gate can consume it; never silently ignore
    // protocol drift.
    let headers: Vec<_> = report
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("spec-link:"))
        .map(parse_spec_link_header)
        .collect::<Option<_>>()?;
    let [(header_count, _header_path)] = headers.as_slice() else {
        return None;
    };
    let coverage = parse_spec_link_coverage(report)?;
    let reasons: Vec<_> = report
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("spec-link-noncert:"))
        .collect();
    let coverage_machines: Vec<_> = coverage.iter().map(|row| row.machine).collect();
    let unique_coverage: BTreeSet<_> = coverage_machines.iter().copied().collect();
    if coverage.is_empty()
        || *header_count != coverage.len()
        || unique_coverage.len() != coverage.len()
    {
        return None;
    }

    if exit_code == Some(0)
        && *status == "spec-link-status: certifying"
        && coverage
            .iter()
            .all(|row| row.mode == CoverageMode::Linked && row.covered == row.total)
        && reasons.is_empty()
    {
        return Some(Disposition::Certifying);
    }

    let design_only_reasons_match = reasons.len() == coverage_machines.len()
        && coverage_machines.iter().all(|machine| {
            let description = format!("machine {machine:?} is explicitly design-only");
            let expected = format!("spec-link-noncert: code=design-only detail={description:?}");
            reasons
                .iter()
                .filter(|reason| **reason == expected.as_str())
                .count()
                == 1
        });
    if exit_code == Some(1)
        && *status == "spec-link-status: non-certifying"
        && coverage
            .iter()
            .all(|row| row.mode == CoverageMode::DesignOnly)
        && design_only_reasons_match
    {
        Some(Disposition::ExplicitDesignOnly)
    } else {
        None
    }
}

/// Trust's coverage line must name the exact action count and report 100% for
/// every machine aterm's inventory actively lowered. Enforcement mode must agree
/// with the classified overall disposition.
pub(crate) fn report_has_exact_full_coverage(
    report: &str,
    machine: &str,
    expected_actions: usize,
    disposition: Disposition,
) -> bool {
    let Some(coverage) = parse_spec_link_coverage(report) else {
        return false;
    };
    let mut matches = coverage.iter().filter(|row| row.machine == machine);
    let Some(row) = matches.next() else {
        return false;
    };
    if matches.next().is_some() {
        return false;
    }
    let expected_mode = match disposition {
        Disposition::Certifying => CoverageMode::Linked,
        Disposition::ExplicitDesignOnly => CoverageMode::DesignOnly,
    };
    expected_actions > 0
        && row.covered == expected_actions
        && row.total == expected_actions
        && row.mode == expected_mode
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xref::SpecModule;

    #[test]
    fn spec_link_report_classification_accepts_only_honest_zero_violation_states() {
        let certifying = concat!(
            "spec-link: 1 machine(s) in good.trust_ir\n",
            "  machine \"Ring\" [embedded] coverage 1/1 = 100.0% (1 anchored, 0 waived) [linked]\n",
            "spec-link-status: certifying\n",
        );
        assert_eq!(
            classify_spec_link(Some(0), certifying),
            Some(Disposition::Certifying)
        );
        assert!(report_has_exact_full_coverage(
            certifying,
            "Ring",
            1,
            Disposition::Certifying
        ));
        let marker_text_in_header_path =
            certifying.replace("good.trust_ir", "/tmp/spec-link failed:/[Ob./good.trust_ir");
        assert_eq!(
            classify_spec_link(Some(0), &marker_text_in_header_path),
            Some(Disposition::Certifying),
            "failure-marker text inside a primary-header path is not a diagnostic"
        );

        let design_only = concat!(
            "spec-link: 1 machine(s) in exploratory.trust_ir\n",
            "  machine \"Ring\" [embedded] coverage 1/1 = 100.0% (1 anchored, 0 waived) [design-only]\n",
            "spec-link-status: non-certifying\n",
            "spec-link-noncert: code=design-only detail=\"machine \\\"Ring\\\" is explicitly design-only\"\n",
        );
        assert_eq!(
            classify_spec_link(Some(1), design_only),
            Some(Disposition::ExplicitDesignOnly)
        );
        assert!(report_has_exact_full_coverage(
            design_only,
            "Ring",
            1,
            Disposition::ExplicitDesignOnly
        ));
        let external = certifying.replace(
            "[embedded]",
            "[external(/tmp/spec-link failed:/[Ob./spec ] coverage models/Ring.tla)]",
        );
        assert_eq!(
            classify_spec_link(Some(0), &external),
            Some(Disposition::Certifying),
            "the external(nonempty path) origin variant is part of TrustIr's grammar"
        );
        assert!(report_has_exact_full_coverage(
            &external,
            "Ring",
            1,
            Disposition::Certifying
        ));

        let zero_action = certifying.replace(
            "coverage 1/1 = 100.0% (1 anchored, 0 waived)",
            "coverage 0/0 = 100.0% (0 anchored, 0 waived)",
        );
        assert_eq!(
            classify_spec_link(Some(0), &zero_action),
            Some(Disposition::Certifying),
            "TrustIr defines an empty action set as vacuously 100.0% covered"
        );

        let partial_design_only = design_only.replace(
            "coverage 1/1 = 100.0% (1 anchored, 0 waived)",
            "coverage 1/3 = 33.3% (1 anchored, 0 waived)",
        );
        assert_eq!(
            classify_spec_link(Some(1), &partial_design_only),
            Some(Disposition::ExplicitDesignOnly),
            "a DesignOnly row may truthfully report partial coverage"
        );
        assert!(!report_has_exact_full_coverage(
            &partial_design_only,
            "Ring",
            1,
            Disposition::ExplicitDesignOnly
        ));

        for poisoned in [
            format!("{design_only}spec-link failed: bad.trust_ir (1 violation)\n"),
            format!("{design_only}error: structurally invalid TrustIr\n"),
            format!("{design_only}  [0] [Ob.1 action-exists] undeclared action\n"),
            format!("{design_only}  [0] [L1 proof-resolves] missing harness\n"),
            format!("{design_only}  [0] [L2 projection-present] missing projection\n"),
            format!("{design_only}  [0] [S0 fallback-identity] duplicate anchor\n"),
            format!(
                "{design_only}spec-link-noncert: code=external-projection-unresolved detail=bad\n"
            ),
            design_only.replace(
                "spec-link-noncert: code=design-only detail=\"machine \\\"Ring\\\" is explicitly design-only\"\n",
                "",
            ),
            format!(
                "{design_only}spec-link-noncert: code=design-only detail=\"machine \\\"Ring\\\" is explicitly design-only\"\n"
            ),
            format!("{design_only}spec-link: harness manifest malformed\n"),
            format!(
                "{design_only}spec-link: harness manifest /tmp/other.json (1 harness), checked 1 proof binding\n"
            ),
            design_only.replace("[embedded]", "[external()]"),
            design_only.replace("[embedded]", "[external( )]"),
            design_only.replace("[embedded]", "[garbage]"),
            design_only.replace(
                "coverage 1/1 = 100.0% (1 anchored, 0 waived)",
                "coverage 1/1 = 100.0% (0 anchored, 0 waived)",
            ),
            design_only.replace(
                "coverage 1/1 = 100.0% (1 anchored, 0 waived)",
                "coverage 2/1 = 200.0% (2 anchored, 0 waived)",
            ),
            design_only.replace("1/1 = 100.0%", "1/1 = 99.9%"),
            design_only.replace("1/1 = 100.0%", "1/1 = 100%"),
            design_only.replace("coverage 1/1", "coverage 01/1"),
            design_only.replace("[design-only]\n", "[design-only] trailing\n"),
            design_only.replace("machine \\\"Ring\\\"", "machine \\\"Other\\\""),
        ] {
            assert_eq!(
                classify_spec_link(Some(1), &poisoned),
                None,
                "must fail closed:\n{poisoned}"
            );
        }
        let certifying_with_reason = format!(
            "{certifying}spec-link-noncert: code=design-only detail=\"machine \\\"Ring\\\" is explicitly design-only\"\n"
        );
        assert_eq!(classify_spec_link(Some(0), &certifying_with_reason), None);
        assert_eq!(
            classify_spec_link(
                Some(0),
                &format!("{certifying}spec-link-status: non-certifying\n")
            ),
            None,
            "contradictory status lines must fail closed"
        );
        assert_eq!(
            classify_spec_link(Some(0), "spec-link-status: certifying\n"),
            None,
            "certifying output needs nonempty linked coverage"
        );
        assert_eq!(
            classify_spec_link(Some(0), &certifying.replace("[linked]", "[design-only]")),
            None,
            "certifying output cannot carry design-only coverage"
        );
        assert_eq!(classify_spec_link(Some(0), design_only), None);
        assert_eq!(classify_spec_link(Some(1), certifying), None);
        assert_eq!(classify_spec_link(None, certifying), None, "signal/crash");
        assert_eq!(classify_spec_link(Some(2), design_only), None, "exit 2");

        let duplicate_certifying_status = format!("{certifying}spec-link-status: certifying\n");
        assert_eq!(
            classify_spec_link(Some(0), &duplicate_certifying_status),
            None,
            "duplicate same status must fail closed"
        );
        let missing_header = certifying.replace("spec-link: 1 machine(s) in good.trust_ir\n", "");
        assert_eq!(classify_spec_link(Some(0), &missing_header), None);
        let incorrect_header_count =
            certifying.replace("spec-link: 1 machine(s)", "spec-link: 2 machine(s)");
        assert_eq!(classify_spec_link(Some(0), &incorrect_header_count), None);
        let noncanonical_header_count =
            certifying.replace("spec-link: 1 machine(s)", "spec-link: 01 machine(s)");
        assert_eq!(
            classify_spec_link(Some(0), &noncanonical_header_count),
            None
        );
        let duplicate_header =
            format!("spec-link: 1 machine(s) in duplicate.trust_ir\n{certifying}");
        assert_eq!(classify_spec_link(Some(0), &duplicate_header), None);
        let malformed_header = format!("spec-link: not-a-header\n{certifying}");
        assert_eq!(classify_spec_link(Some(0), &malformed_header), None);
        let garbage_coverage = certifying.replace(
            "machine \"Ring\" [embedded] coverage 1/1 = 100.0% (1 anchored, 0 waived) [linked]",
            "machine \"Ring\" [garbage coverage 1/1 = 100.0%] [linked]",
        );
        assert_eq!(
            classify_spec_link(Some(0), &garbage_coverage),
            None,
            "a name prefix and linked suffix cannot disguise a malformed coverage row"
        );
        assert!(!report_has_exact_full_coverage(
            &garbage_coverage,
            "Ring",
            1,
            Disposition::Certifying
        ));
        let partial_linked = certifying.replace(
            "coverage 1/1 = 100.0% (1 anchored, 0 waived)",
            "coverage 1/3 = 33.3% (1 anchored, 0 waived)",
        );
        assert_eq!(
            classify_spec_link(Some(0), &partial_linked),
            None,
            "TrustIr cannot certify a linked machine with partial coverage"
        );
        let duplicate_coverage = certifying
            .replace("spec-link: 1 machine(s)", "spec-link: 2 machine(s)")
            .replace(
                "spec-link-status: certifying\n",
                concat!(
                    "  machine \"Ring\" [embedded] coverage 1/1 = 100.0% ",
                    "(1 anchored, 0 waived) [linked]\n",
                    "spec-link-status: certifying\n",
                ),
            );
        assert_eq!(
            classify_spec_link(Some(0), &duplicate_coverage),
            None,
            "duplicate machine coverage must fail closed"
        );

        // The live assembled-module shape: every unique machine row must classify as an
        // honest DesignOnly report.
        // The live registry's shape (one row per machine name: this test
        // binary also links the interpreter's own fixtures, some of which
        // share a name).
        let mut seen = BTreeSet::new();
        let live_modules: Vec<SpecModule> = crate::xref::model_registry()
            .into_iter()
            .filter(|m| seen.insert(m.name))
            .map(SpecModule::Embedded)
            .collect();
        let mut live_report = format!(
            "spec-link: {} machine(s) in /tmp/aterm_spec_xref.trust_ir\n",
            live_modules.len()
        );
        for module in &live_modules {
            let name = module.name();
            let actions = module.action_names().len();
            let origin = match module {
                SpecModule::Embedded(_) => "embedded".to_string(),
                SpecModule::External(spec) => format!("external({})", spec.file_path),
            };
            let percentage = if actions == 0 { "100.0" } else { "0.0" };
            live_report.push_str(&format!(
                "  machine {name:?} [{origin}] coverage 0/{actions} = {percentage}% \
                 (0 anchored, 0 waived) [design-only]\n"
            ));
        }
        live_report.push_str("spec-link-status: non-certifying\n");
        for module in &live_modules {
            let description = format!("machine {:?} is explicitly design-only", module.name());
            live_report.push_str(&format!(
                "spec-link-noncert: code=design-only detail={description:?}\n"
            ));
        }
        assert_eq!(
            classify_spec_link(Some(1), &live_report),
            Some(Disposition::ExplicitDesignOnly),
            "the live DesignOnly report shape must classify honestly"
        );
    }
}
