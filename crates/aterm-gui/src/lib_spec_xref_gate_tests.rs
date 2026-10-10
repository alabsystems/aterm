// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::collections::BTreeSet;
use std::path::PathBuf;

use aterm_uds::spawnfd::{Child, Command, Stdio, command};

use aterm_spec::tla_check::TlaSpec;
use aterm_spec::xref::{self, SpecModule};

// Verification gate for `ty` and `trust-ir` (`aterm_spec::verify`). The in-process
// closure is authoritative; `ty` also checks the bounded models when installed; TrustIr
// analyzes the emitted cross-reference, reported as design-only and non-certifying.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SpecLinkDisposition {
    Certifying,
    ExplicitDesignOnly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SpecLinkCoverageMode {
    Linked,
    DesignOnly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SpecLinkCoverageRow<'a> {
    machine: &'a str,
    covered: usize,
    total: usize,
    mode: SpecLinkCoverageMode,
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
fn report_has_spec_link_failure(report: &str) -> bool {
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
fn parse_spec_link_coverage_row(line: &str) -> Option<SpecLinkCoverageRow<'_>> {
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
        (accounting, SpecLinkCoverageMode::Linked)
    } else {
        let accounting = row.strip_suffix(") [design-only]")?;
        (accounting, SpecLinkCoverageMode::DesignOnly)
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

    Some(SpecLinkCoverageRow {
        machine,
        covered,
        total,
        mode,
    })
}

fn parse_spec_link_coverage(report: &str) -> Option<Vec<SpecLinkCoverageRow<'_>>> {
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
fn stale_trust_ir_hint(exit_code: Option<i32>, report: &str) -> String {
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
fn classify_spec_link(exit_code: Option<i32>, report: &str) -> Option<SpecLinkDisposition> {
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
            .all(|row| row.mode == SpecLinkCoverageMode::Linked && row.covered == row.total)
        && reasons.is_empty()
    {
        return Some(SpecLinkDisposition::Certifying);
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
            .all(|row| row.mode == SpecLinkCoverageMode::DesignOnly)
        && design_only_reasons_match
    {
        Some(SpecLinkDisposition::ExplicitDesignOnly)
    } else {
        None
    }
}

/// Trust's coverage line must name the exact action count and report 100% for
/// every machine aterm's inventory actively lowered. Enforcement mode must agree
/// with the classified overall disposition.
fn report_has_exact_full_coverage(
    report: &str,
    machine: &str,
    expected_actions: usize,
    disposition: SpecLinkDisposition,
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
        SpecLinkDisposition::Certifying => SpecLinkCoverageMode::Linked,
        SpecLinkDisposition::ExplicitDesignOnly => SpecLinkCoverageMode::DesignOnly,
    };
    expected_actions > 0
        && row.covered == expected_actions
        && row.total == expected_actions
        && row.mode == expected_mode
}

/// Run `trust-ir spec-link <module>` and return (exit code, stdout+stderr). A
/// violation-free design-only report exits 1 in TrustIr v27, so callers go through
/// [`classify_spec_link`].
fn run_spec_link(trust_ir: &std::path::Path, module: &std::path::Path) -> (Option<i32>, String) {
    let mut cmd = command(trust_ir);
    // aterm emits the documented v27 TEXT shape (`lower_to_ir`); trust-ir 0.2.0
    // auto-detects the `.trust_ir` extension as BINARY, so pin the format.
    cmd.arg("spec-link").arg("--format").arg("text").arg(module);
    let out = spawn_with_settled_env(
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .and_then(Child::wait_with_output)
    .unwrap_or_else(|e| panic!("failed to run {trust_ir:?} spec-link: {e}"));
    let mut report = String::from_utf8_lossy(&out.stdout).into_owned();
    let err = String::from_utf8_lossy(&out.stderr);
    if !err.trim().is_empty() {
        report.push_str(&err);
    }
    (out.status.code(), report)
}

#[test]
fn spec_link_report_classification_accepts_only_honest_zero_violation_states() {
    let certifying = concat!(
        "spec-link: 1 machine(s) in good.trust_ir\n",
        "  machine \"Ring\" [embedded] coverage 1/1 = 100.0% (1 anchored, 0 waived) [linked]\n",
        "spec-link-status: certifying\n",
    );
    assert_eq!(
        classify_spec_link(Some(0), certifying),
        Some(SpecLinkDisposition::Certifying)
    );
    assert!(report_has_exact_full_coverage(
        certifying,
        "Ring",
        1,
        SpecLinkDisposition::Certifying
    ));
    let marker_text_in_header_path =
        certifying.replace("good.trust_ir", "/tmp/spec-link failed:/[Ob./good.trust_ir");
    assert_eq!(
        classify_spec_link(Some(0), &marker_text_in_header_path),
        Some(SpecLinkDisposition::Certifying),
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
        Some(SpecLinkDisposition::ExplicitDesignOnly)
    );
    assert!(report_has_exact_full_coverage(
        design_only,
        "Ring",
        1,
        SpecLinkDisposition::ExplicitDesignOnly
    ));
    let external = certifying.replace(
        "[embedded]",
        "[external(/tmp/spec-link failed:/[Ob./spec ] coverage models/Ring.tla)]",
    );
    assert_eq!(
        classify_spec_link(Some(0), &external),
        Some(SpecLinkDisposition::Certifying),
        "the external(nonempty path) origin variant is part of TrustIr's grammar"
    );
    assert!(report_has_exact_full_coverage(
        &external,
        "Ring",
        1,
        SpecLinkDisposition::Certifying
    ));

    let zero_action = certifying.replace(
        "coverage 1/1 = 100.0% (1 anchored, 0 waived)",
        "coverage 0/0 = 100.0% (0 anchored, 0 waived)",
    );
    assert_eq!(
        classify_spec_link(Some(0), &zero_action),
        Some(SpecLinkDisposition::Certifying),
        "TrustIr defines an empty action set as vacuously 100.0% covered"
    );

    let partial_design_only = design_only.replace(
        "coverage 1/1 = 100.0% (1 anchored, 0 waived)",
        "coverage 1/3 = 33.3% (1 anchored, 0 waived)",
    );
    assert_eq!(
        classify_spec_link(Some(1), &partial_design_only),
        Some(SpecLinkDisposition::ExplicitDesignOnly),
        "a DesignOnly row may truthfully report partial coverage"
    );
    assert!(!report_has_exact_full_coverage(
        &partial_design_only,
        "Ring",
        1,
        SpecLinkDisposition::ExplicitDesignOnly
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
    let duplicate_header = format!("spec-link: 1 machine(s) in duplicate.trust_ir\n{certifying}");
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
        SpecLinkDisposition::Certifying
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
    let live_modules = registered_modules();
    // This test checks the report SHAPE against the live set, which is built
    // from the registry itself and so cannot stop being representative. What a
    // bare count would have hidden is the same machine twice — refused here, as
    // `duplicate_coverage` two blocks up refuses it in the report.
    let mut names: Vec<&str> = live_modules.iter().map(SpecModule::name).collect();
    let total = names.len();
    names.sort();
    names.dedup();
    assert_eq!(
        names.len(),
        total,
        "the module registry contains a DUPLICATE machine name"
    );
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
        Some(SpecLinkDisposition::ExplicitDesignOnly),
        "the live DesignOnly report shape must classify honestly"
    );
}

/// Spawn `cmd` while no handoff test has the process environment pointed at
/// its scratch. Those tests point HOME and XDG_RUNTIME_DIR at a scratch dir
/// while their fixture lives (`seamless::tests::ScratchEnv`), under
/// `seamless::tests::ENV_LOCK`, and a cargo started inside that window takes
/// `<scratch>/.cargo` for its home: it fetched the registry there (measured
/// 2026-09-30). A child copies the environment when it is spawned, so the
/// lock is held for the spawn alone, never the build.
///
/// Every child this gate starts itself goes through here: `trust-ir spec-link`. A `ty` goes
/// through `aterm_spec::verify::ty_output`, which gives it a HOME of its
/// own; which `ty` or `trust-ir` runs is the account's store's answer, never
/// the moved HOME's (`aterm_spec::verify`'s discovery).
fn spawn_with_settled_env(cmd: &mut Command) -> std::io::Result<Child> {
    let _env = crate::seamless::tests::ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    cmd.spawn()
}

/// The registered `SpecModule` set: every embedded `Model` plus every active external
/// `.tla` in aterm-spec-models' `specs/`.
fn registered_modules() -> Vec<SpecModule> {
    let mut modules: Vec<SpecModule> = xref::model_registry()
        .into_iter()
        .map(SpecModule::Embedded)
        .collect();
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../aterm-spec-models/specs");
    let mut external = 0usize;
    for entry in std::fs::read_dir(&dir).expect("read aterm-spec-models specs/") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            continue; // aterm-spec-models' own test forbids a subdirectory
        }
        if path.extension().and_then(|e| e.to_str()) != Some("tla") {
            continue;
        }
        let spec = TlaSpec::parse_file(&path)
            .unwrap_or_else(|e| panic!("failed to parse external spec {path:?}: {e}"));
        modules.push(SpecModule::External(spec));
        external += 1;
    }
    assert!(
        external > 0,
        "no external ISOLATION .tla parsed from {dir:?} — Phase-1 quarantine wrong?"
    );
    modules
}

/// The six ISOLATION machines Phase 2 activates, with the action count the
/// closure gate must find bound-or-waived (the `Next` disjuncts of each spec).
/// `fork_exec` is anchored-but-not-Tier-1 (its child branch can't be driven
/// in-process); the rest carry Tier-1 conformance in their own crates' tests.
#[cfg(unix)]
const ISOLATION: &[(&str, usize)] = &[
    ("sandbox", 1),      // Apply
    ("path_confine", 1), // Confine
    ("fork_exec", 6),    // Fork, Setrlimit, Chdir, CloseMaster, UnsafeEnvOp, Exec
    ("write_all", 2),    // Progress, Interrupted
    ("alt_screen", 4),   // WriteMain, Enter, Scribble, Leave
    ("gpu_encode", 2),   // Append, Encode
];
/// Windows twin of [`ISOLATION`]: `fork_exec` and `write_all` model POSIX code that does
/// not compile here, so those machines stay report-only; the four whose subjects ship
/// on Windows stay fully gated.
#[cfg(windows)]
const ISOLATION: &[(&str, usize)] = &[
    ("sandbox", 1),      // Apply
    ("path_confine", 1), // Confine
    ("alt_screen", 4),   // WriteMain, Enter, Scribble, Leave
    ("gpu_encode", 2),   // Append, Encode
];

/// The eleven `SelectionCustody` actions, shared by the coverage and execution-evidence
/// assertions so both check the same set.
const SELECTION_CUSTODY_ACTIONS: &[&str] = &[
    "SelectLow",
    "SelectOldest",
    "SelectHigh",
    "UserClear",
    "TypingPress",
    "InertPress",
    "UniformScroll",
    "RegionDamageLow",
    "RegionDamageHigh",
    "Evict",
    "WholesaleInvalidate",
];

/// The sixteen `PressCustody` actions — see [`SELECTION_CUSTODY_ACTIONS`].
const PRESS_CUSTODY_ACTIONS: &[&str] = &[
    "UserScroll",
    "UserScrollTowardLive",
    "SnapToLive",
    "UserSelect",
    "UserClear",
    "TypingPress",
    "RepeatPress",
    "InertPress",
    "ReleaseEvent",
    "OutputAtLive",
    "OutputWhileReading",
    "OutputDamagesTheSelectedRows",
    "OutputDamagesTheSelectedRowsInPlace",
    "OutputTookTheSelectionUnattributed",
    "OutputTookTheSelectionUnattributedInPlace",
    "OutputInvalidatesTheCoordinateSpace",
];

/// Assert one machine's per-step execution evidence and return the derived summary of
/// what it does and does not discriminate.
///
/// String coverage only proves each action name is claimed by some anchor, not that the
/// anchor sits on a function that runs. So the conformance opens a one-step window per
/// step (`aterm_spec::xref::StepEvidence::step`) and this requires each action's
/// anchored function to be entered by every window driving that action
/// (`StepAudit::blind_steps`), and, conversely, no anchored function to be entered by a
/// window driving an action it does not claim (`StepAudit::stray`), which discriminates
/// inside a `Terminal::process` batch.
///
/// Residual, printed in the summary: a function whose entry pattern exactly matches its
/// claimed actions is indistinguishable from the right seam (`interchangeable` is a
/// lower bound), and labels on one function can be permuted unobservably. The derived
/// figures are not a quality score.
///
/// Applied to `SelectionCustody` and `PressCustody` only; the other conformances here
/// (and anchor-bearing machines without one) have string coverage only.
fn assert_step_evidence(
    machine: &str,
    expected_actions: &[&str],
    ev: &aterm_spec::xref::StepEvidence,
) -> String {
    let audit = ev.audit();
    assert!(
        audit.uninstrumented.is_empty(),
        "{machine}: every anchor must carry an entry probe — an anchor the macro \
         could not instrument (a `const fn`, or a non-fn item) would be a way back \
         out of this check; unprobed: {:?}",
        audit.uninstrumented
    );
    assert!(
        audit.ambiguous.is_empty(),
        "{machine}: two anchors share an entry id, so one fn's entry would discharge \
         the other's obligation: {:?}",
        audit.ambiguous
    );
    // Check the action set, not an anchor count: an action may legitimately have more
    // than one anchor site (e.g. `Evict` on both `set_scrollback_line_limit` and
    // `Terminal::post_process`).
    let want: BTreeSet<&str> = expected_actions.iter().copied().collect();
    let got: BTreeSet<&str> = audit.actions.iter().copied().collect();
    assert_eq!(
        got,
        want,
        "{machine}: the ACTIONS carried by anchors linked into this gate's binary \
         must be exactly the {} this gate checks. Missing {:?}; unexpected {:?}. \
         (Anchor SITES may be many-per-action and are not counted here: {} sites \
         across {} functions.)",
        expected_actions.len(),
        want.difference(&got).collect::<Vec<_>>(),
        got.difference(&want).collect::<Vec<_>>(),
        audit.anchors,
        audit.by_function.len(),
    );
    assert!(
        audit.unanchored_steps.is_empty(),
        "{machine}: the conformance tagged step(s) with action name(s) no anchor of \
         this machine carries, so those steps checked nothing: {:?}",
        audit.unanchored_steps
    );
    assert!(
        audit.undriven.is_empty(),
        "{machine}: anchored action(s) that the Tier-1 conformance never DROVE, so \
         no step window could witness them: {:?}. Drive them, or the anchor is a \
         claim nothing tests.",
        audit.undriven
    );
    assert!(
        audit.unwitnessed.is_empty(),
        "{machine}: anchored action(s) whose annotated function was NOT ENTERED by \
         the step that drives them: {:?}. The conformance exercises the action, so \
         the function the anchor sits on is not the function that performs it — move \
         the anchor to the real seam (or drive that seam). Note that entry ELSEWHERE \
         in the run does not count: the window is one step wide, which is what makes \
         an anchor moved onto a function OUTSIDE this action's own window fail here. \
         Witnessed {}/{} step windows.",
        audit.unwitnessed,
        audit.witnessed_steps,
        audit.steps,
    );
    // The and, not the OR. `unwitnessed` above passes an action on any one of its
    // windows, so `Evict` — driven by three steps — was checked on one and unchecked
    // on two. This requires every window to have entered its own action's function,
    // which is what stops a step from quietly dropping out of the evidence by being
    // driven outside `ev.step` while a sibling step still carries the action.
    assert!(
        audit.blind_steps.is_empty(),
        "{machine}: step window(s) that did NOT enter their own action's annotated \
         function: {:?}. An action driven by several steps is `witnessed` on one of \
         them, so these would otherwise check nothing at all — either the seam that \
         step calls is not the anchored one, or the step was driven outside its \
         window. Witnessed {}/{} step windows.",
        audit.blind_steps,
        audit.witnessed_steps,
        audit.steps,
    );
    // The contrapositive: a function that is the seam for `{B, C}` must not be entered by
    // a window driving `D`, which catches an anchor parked on a function the batch merely
    // passes through. It can also fire for a function entered on every batch, whose entry
    // is then no evidence for its actions; the message lists the resolutions.
    assert!(
        audit.stray.is_empty(),
        "{machine}: anchored function(s) entered by the window of an action they are \
         NOT anchored for: {:?} (fn -> the foreign actions whose windows entered it). \
         Entry in such a function is not evidence for the actions it claims, because \
         it also runs when those other actions fire. Either the anchor is on the \
         wrong function (move it to the seam that is specific to its action), or the \
         function really is the shared seam for those actions too (anchor it for them \
         as well), or the step's window is wider than the seam it means to name \
         (narrow it).",
        audit.stray
    );
    assert!(
        audit.is_complete(),
        "{machine} step-evidence audit incomplete: {audit:?}"
    );
    audit.summary()
}

/// The stack each Tier-1 conformance body runs on ([`run_conformance_body`]). An
/// unoptimized test build moves a `WindowState` by value through every frame of
/// `BTreeMap::insert`; on 2026-09-30 input_release_pairing's `two_window_app` ->
/// `App::insert_logical_window` chain needed 2,135,360 bytes of frames, past
/// libtest's 2 MiB thread stack, and the SIGBUS at the guard page killed the whole
/// `aterm-gui --lib` binary with its tests unreported (`aterm: fatal signal 10`, as an
/// earlier test had armed crash_signal's handler). 32 MiB is reserved, not touched.
/// The bodies' stack is not what this gate judges: on 2026-10-02 the whole gate
/// still fit on one thread of 2,035,712 bytes and not of 2,023,424, about 61 KB
/// under libtest's 2 MiB, so a body's growth goes unseen here until it outgrows
/// the reserve. The by-value moves are the size; this stack only gives them room.
const CONFORMANCE_BODY_STACK: usize = 32 << 20;

/// Run one Tier-1 conformance body on a thread of its own with
/// [`CONFORMANCE_BODY_STACK`], recording a panic as `machine: message` in `failed`, so
/// one red body does not hide a later one and the gate names every failure. The thread
/// is joined before this returns, so a body's thread-local evidence windows
/// (`aterm_spec::xref`) open and close on that one thread.
fn run_conformance_body<T: Send>(
    machine: &str,
    failed: &mut Vec<String>,
    body: impl FnOnce() -> T + Send,
) -> Option<T> {
    let joined = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name(format!("spec_xref_closure/{machine}"))
            .stack_size(CONFORMANCE_BODY_STACK)
            .spawn_scoped(scope, body)
            .map(|thread| thread.join())
    });
    match joined {
        Ok(Ok(value)) => Some(value),
        Ok(Err(payload)) => {
            let message = payload
                .downcast_ref::<&str>()
                .map(|text| (*text).to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "non-string panic payload".to_string());
            failed.push(format!("{machine}: {message}"));
            None
        }
        Err(spawn) => {
            failed.push(format!("{machine}: its thread did not start: {spawn}"));
            None
        }
    }
}

/// [`run_conformance_body`] gives a body more stack than a test thread has
/// (at least 4 MiB spent here; libtest's threads have 2), and still names a
/// body that panics instead of propagating it.
#[test]
fn a_conformance_body_runs_on_its_own_stack_and_a_panic_is_named() {
    /// Spend about `kib` KiB of stack, every frame live until the deepest
    /// returns.
    fn spend(kib: usize) -> u64 {
        let frame = std::hint::black_box([1u8; 64 << 10]);
        let below = if kib > 64 { spend(kib - 64) } else { 0 };
        below + u64::from(frame[kib % frame.len()])
    }
    let mut failed = Vec::new();
    assert_eq!(
        run_conformance_body("deep", &mut failed, || spend(4 << 10)),
        Some(64)
    );
    assert_eq!(
        run_conformance_body("red", &mut failed, || -> u8 { panic!("boom") }),
        None
    );
    assert_eq!(failed, ["red: boom"]);
}

#[test]
fn spec_xref_closure() {
    // LINK the keeper's anchors: aterm-keeper is a test-only dependency here
    // (the window does not use it before P3), and `inventory` sees only
    // object code the binary links. Naming the anchored handlers links them.
    std::hint::black_box((
        aterm_keeper::KeeperCore::new as fn(bool) -> aterm_keeper::KeeperCore,
        aterm_keeper::RelaunchBrake::new as fn() -> aterm_keeper::RelaunchBrake,
        aterm_keeper::core::classify_death
            as fn(aterm_keeper::core::DeathEvidence) -> aterm_keeper::core::Verdict,
    ));
    // Tiered: proofs #1–#3 (anchors, closure, coverage, Tier-1 conformances) need
    // no toolchain; conformances escalate to `ty` where installed. Proof #4
    // (`spec_xref_strict_vacuity`) and analysis #5 (TrustIr spec-link) need the toolchain
    // and print a notice otherwise.

    // ---- Proof #1: inventory ACTUALLY collected the anchors, cross-crate. ----
    let refs: Vec<_> = xref::refinements().collect();
    let waivers: Vec<_> = xref::waivers().collect();

    // Per-machine collected counts (the cross-crate collection evidence).
    let machines: BTreeSet<&str> = refs
        .iter()
        .map(|r| r.machine)
        .chain(
            waivers
                .iter()
                .filter(|w| !w.machine.is_empty())
                .map(|w| w.machine),
        )
        .collect();
    crate::logging::stderr_line!(
        "spec_xref_closure (relocated to aterm-gui): collected {} refinement anchor(s) + \
         {} waiver(s) across {} machine(s):",
        refs.len(),
        waivers.len(),
        machines.len()
    );
    for m in &machines {
        let r = refs.iter().filter(|x| x.machine == *m).count();
        let w = waivers.iter().filter(|x| x.machine == *m).count();
        crate::logging::stderr_line!("    {m:<16} refinements={r:<3} waivers={w}");
    }

    // Phase 0 not regressed: the terminal_modes anchors (from aterm-core, now
    // visible via its `spec-anchors` feature) must still be collected here.
    let tm = refs
        .iter()
        .filter(|r| r.machine == "terminal_modes")
        .count();
    assert!(
        tm >= 26,
        "expected >= 26 `terminal_modes` refinement anchors collected cross-crate from \
         aterm-core (Phase 0 must not regress), found {tm}"
    );

    // Every ISOLATION machine's anchors collected here (cross-crate).
    for (machine, _) in ISOLATION {
        let r = refs.iter().filter(|x| x.machine == *machine).count();
        let w = waivers.iter().filter(|x| x.machine == *machine).count();
        assert!(
            r + w > 0,
            "ISOLATION machine `{machine}` collected ZERO anchors — the cross-crate \
             `inventory` collection (spec-anchors feature) did not link. The gate would be \
             vacuously green for it."
        );
    }

    // ---- Build the registered SpecModule set (embedded + external ISOLATION) ----
    let modules = registered_modules();
    crate::logging::stderr_line!(
        "spec_xref_closure: {} registered SpecModule(s) ({} embedded + external ISOLATION)",
        modules.len(),
        xref::model_registry().len()
    );

    // ---- Run the obligations (1, 3, 4) ----
    let report = xref::check_closure(&modules);

    crate::logging::stderr_line!("spec_xref_closure: per-machine coverage ledger:");
    for c in &report.coverage {
        crate::logging::stderr_line!(
            "  {:<16} ratio={:.3} bound={:<3} waived={:<3} actions={:<3} {}{}",
            c.machine,
            c.ratio(),
            c.bound.len(),
            c.waived.len(),
            c.total_actions,
            if c.active {
                "[ACTIVE]"
            } else {
                "[report-only]"
            },
            if c.active && !c.uncovered.is_empty() {
                format!(" UNCOVERED={:?}", c.uncovered)
            } else {
                String::new()
            },
        );
    }

    // ---- Proof #2: GREEN (no obligation violations) ----
    assert!(
        report.is_ok(),
        "spec_xref_closure FAILED — source↔spec obligations violated:\n{}",
        report
            .violations
            .iter()
            .map(|v| format!("  [obligation {}] {}", v.obligation, v.message))
            .collect::<Vec<_>>()
            .join("\n")
    );

    // StreamingSearch's shipping methods are fully bound in ty: keep the
    // machine itself non-vacuous.
    let streaming = report
        .coverage
        .iter()
        .find(|c| aterm_spec::xref::machine_matches("streaming_search", &c.machine))
        .expect("StreamingSearch must be present in the coverage ledger");
    assert!(streaming.active, "StreamingSearch must be ACTIVE");
    assert_eq!(
        streaming.total_actions, 9,
        "StreamingSearch action inventory drifted"
    );
    assert_eq!(
        streaming.bound.len(),
        9,
        "all StreamingSearch actions must carry refinement anchors"
    );
    assert_eq!(
        streaming.ratio(),
        1.0,
        "StreamingSearch must remain fully bound"
    );
    assert!(
        streaming.uncovered.is_empty(),
        "StreamingSearch must have no uncovered action"
    );

    // ---- Proof #3: every ISOLATION machine is active and fully covered (==1.0) ----
    for (machine, want_actions) in ISOLATION {
        let c = report
            .coverage
            .iter()
            .find(|c| aterm_spec::xref::machine_matches(machine, &c.machine))
            .unwrap_or_else(|| panic!("ISOLATION machine `{machine}` not in the coverage ledger"));
        assert!(
            c.active,
            "ISOLATION machine `{machine}` must be ACTIVE (>= 1 refinement)"
        );
        assert_eq!(
            c.ratio(),
            1.0,
            "ISOLATION machine `{machine}` must be fully bound-or-waived (ratio == 1.0); \
             uncovered = {:?}",
            c.uncovered
        );
        assert_eq!(
            c.total_actions, *want_actions,
            "ISOLATION machine `{machine}` expected {want_actions} actions (its Next \
             disjuncts), found {}",
            c.total_actions
        );
    }

    // Each Tier-1 body runs under its own catch (`run_conformance_body`) and the
    // failures are asserted together once the last body has run.
    let mut failed_bodies: Vec<String> = Vec::new();
    // Proof #3a: `window_routing` is actively bound (the `#[refines]` on
    // `insert_logical_window` / `close_window_logical`); assert that, then run its real
    // Tier-1 conformance here so the gate fails if close→exit routing diverges.
    {
        let wr = report
            .coverage
            .iter()
            .find(|c| aterm_spec::xref::machine_matches("window_routing", &c.machine))
            .expect("window_routing must be in the coverage ledger");
        assert!(
            wr.active,
            "window_routing must be ACTIVELY-BOUND (>= 1 refinement on the real App seams) — \
             finding 3 requires it be gated, not report-only"
        );
        assert_eq!(
            wr.ratio(),
            1.0,
            "window_routing must be fully bound (CreateWindow + CloseWindow + \
             ConnectedCreateWindow + RaiseSession all anchored); uncovered = {:?}",
            wr.uncovered
        );
        assert_eq!(
            wr.total_actions, 4,
            "window_routing has 4 actions (CreateWindow, CloseWindow, \
             ConnectedCreateWindow, RaiseSession — the session-connections acts, \
             design §9), found {}",
            wr.total_actions
        );
    }
    // Run it (the gate now proves window_routing Tier-1, not merely claims it).
    if run_conformance_body(
        "window_routing",
        &mut failed_bodies,
        super::window_routing_conformance::run_conformance,
    )
    .is_some()
    {
        crate::logging::stderr_line!(
            "spec_xref_closure: window_routing is actively-bound AND its Tier-1 conformance \
             (real App create/close→exit routing) was RUN by the gate (finding 3)."
        );
    }

    // `pane_tree` is actively bound via `PaneTree::split_focused` / `close_pane`; run the
    // real split/close conformance here.
    {
        let pt = report
            .coverage
            .iter()
            .find(|c| aterm_spec::xref::machine_matches("pane_tree", &c.machine))
            .expect("pane_tree must be in the coverage ledger");
        assert!(
            pt.active,
            "pane_tree must be ACTIVELY-BOUND (>= 1 refinement on the real PaneTree seams)"
        );
        assert_eq!(
            pt.ratio(),
            1.0,
            "pane_tree must be fully bound (Split + Close both anchored); uncovered = {:?}",
            pt.uncovered
        );
        assert_eq!(
            pt.total_actions, 2,
            "pane_tree has 2 actions (Split, Close), found {}",
            pt.total_actions
        );
    }
    if run_conformance_body(
        "pane_tree",
        &mut failed_bodies,
        super::pane_tree_conformance::run_conformance,
    )
    .is_some()
    {
        crate::logging::stderr_line!(
            "spec_xref_closure: pane_tree is actively-bound AND its Tier-1 conformance \
             (real split/close re-point) was RUN by the gate."
        );
    }

    // `session_pool` is actively bound via `SessionPool::attach` / `detach`; run the real
    // refcount conformance here.
    {
        let sp = report
            .coverage
            .iter()
            .find(|c| aterm_spec::xref::machine_matches("session_pool", &c.machine))
            .expect("session_pool must be in the coverage ledger");
        assert!(
            sp.active,
            "session_pool must be ACTIVELY-BOUND (>= 1 refinement on the real pool seams)"
        );
        assert_eq!(
            sp.ratio(),
            1.0,
            "session_pool must be fully bound (Acquire + Release both anchored); uncovered = {:?}",
            sp.uncovered
        );
        assert_eq!(
            sp.total_actions, 2,
            "session_pool has 2 actions (Acquire, Release), found {}",
            sp.total_actions
        );
    }
    if run_conformance_body(
        "session_pool",
        &mut failed_bodies,
        super::session_pool_conformance::run_conformance,
    )
    .is_some()
    {
        crate::logging::stderr_line!(
            "spec_xref_closure: session_pool is actively-bound AND its Tier-1 conformance \
             (real attach/detach refcount accounting) was RUN by the gate."
        );
    }

    // The `#[refines]` on the real `open_tab_in` / `cycle_tab` / `close_tab_at`
    // seams make `tab_strip` an active, coverage-gated machine; assert that, THEN
    // drive the real NewTab/SelectTab/Close conformance — incl. the non-front close
    // whose stale-strip desync `StripMirrorsTruth` forbids — from inside the gate.
    {
        let ts = report
            .coverage
            .iter()
            .find(|c| aterm_spec::xref::machine_matches("tab_strip", &c.machine))
            .expect("tab_strip must be in the coverage ledger");
        assert!(
            ts.active,
            "tab_strip must be ACTIVELY-BOUND (>= 1 refinement on the real tab seams)"
        );
        assert_eq!(
            ts.ratio(),
            1.0,
            "tab_strip must be fully bound (NewTab + SelectTab + Close all anchored); uncovered = {:?}",
            ts.uncovered
        );
        assert_eq!(
            ts.total_actions, 3,
            "tab_strip has 3 actions (NewTab, SelectTab, Close), found {}",
            ts.total_actions
        );
    }
    if run_conformance_body(
        "tab_strip",
        &mut failed_bodies,
        super::tab_strip_conformance::run_conformance,
    )
    .is_some()
    {
        crate::logging::stderr_line!(
            "spec_xref_closure: tab_strip is actively-bound AND its Tier-1 conformance \
             (native strip parity, incl. a non-front-window close) was RUN by the gate."
        );
    }

    // The asymmetric layout machine spans three real presentation layers:
    // Renderer/WindowCpu, GpuRenderer/WindowGpu, and this GUI RepaintKey.
    // Cross-crate anchors make the whole five-action machine active; run the
    // GUI decision conformance here so coverage cannot be a decorative ledger.
    {
        let layout = report
            .coverage
            .iter()
            .find(|c| aterm_spec::xref::machine_matches("AsymmetricPadLayout", &c.machine))
            .expect("AsymmetricPadLayout must be in the coverage ledger");
        assert!(
            layout.active,
            "AsymmetricPadLayout must be ACTIVELY-BOUND by CPU/GPU/GUI layout seams"
        );
        assert_eq!(
            layout.ratio(),
            1.0,
            "AsymmetricPadLayout must be fully bound-or-waived; uncovered = {:?}",
            layout.uncovered
        );
        assert_eq!(
            layout.total_actions, 5,
            "AsymmetricPadLayout action surface changed; update anchors/conformance"
        );
        for action in [
            "ApplyInitialTop",
            "PrimeLayoutCache",
            "ApplyChangedTop",
            "RenderWithLayoutCache",
        ] {
            assert!(
                layout.bound.contains(action),
                "shipping layout/cache action `{action}` must carry a #[refines] anchor"
            );
        }
        assert!(
            layout.waived.contains("PickLayout"),
            "only the model's nondeterministic external input selection may be waived"
        );
    }
    if run_conformance_body(
        "AsymmetricPadLayout",
        &mut failed_bodies,
        super::early_out_tests::run_asymmetric_pad_layout_conformance,
    )
    .is_some()
    {
        crate::logging::stderr_line!(
            "spec_xref_closure: AsymmetricPadLayout is fully bound across CPU/GPU/GUI and the \
             real RepaintKey origin-change conformance was RUN by the gate."
        );
    }

    // Cross-window key ownership is active only when the twelve owner/routing seams are
    // linked: require all 21 actions bound or waived, then run the two-window
    // conformance.
    {
        let pairing = report
            .coverage
            .iter()
            .find(|c| aterm_spec::xref::machine_matches("input_release_pairing", &c.machine))
            .expect("input_release_pairing must be in the coverage ledger");
        assert!(
            pairing.active,
            "input_release_pairing must be ACTIVELY-BOUND by the physical owner-map seams"
        );
        assert_eq!(
            pairing.ratio(),
            1.0,
            "input_release_pairing must be fully bound-or-waived; uncovered = {:?}",
            pairing.uncovered
        );
        assert_eq!(
            pairing.total_actions, 21,
            "input_release_pairing action surface changed; update anchors/conformance"
        );
        for action in [
            "ConsumePhysicalPress",
            "ForwardPress",
            "ForwardLiteralPress",
            "CaptureLocalRepeatPress",
            "SwallowUntrackedRepeat",
            "ForwardRepeatOfForwardedPress",
            "ForwardRepeatOfLiteralPress",
            "ForwardLocalRepeat",
            "ReleaseConsumedPress",
            "ReleaseForwardedPress",
            "ReleaseLiteralPress",
            "ReleaseLocalRepeatPress",
        ] {
            assert!(
                pairing.bound.contains(action),
                "real physical-owner action `{action}` must carry a #[refines] anchor"
            );
        }
        assert!(
            pairing.waived.contains("TransferFocusWhileHeld"),
            "focus transfer is an explicit OS-environment waiver, never an uncovered action"
        );
    }
    if run_conformance_body(
        "input_release_pairing",
        &mut failed_bodies,
        super::app_input::input_release_pairing_conformance::run_conformance,
    )
    .is_some()
    {
        crate::logging::stderr_line!(
            "spec_xref_closure: input_release_pairing is actively-bound AND its Tier-1 \
             two-window owner-map conformance was RUN by the gate."
        );
    }

    // Selection custody: anchors on the gesture/press seams (`App::finish_selection`,
    // `app_input::apply_press_custody`) and engine seams (`Terminal::post_process`,
    // `set_scrollback_line_limit`, `clear_scrollback`) make it active. Assert all eleven
    // actions bound and none waived, then run the Tier-1 body.
    {
        let custody = report
            .coverage
            .iter()
            .find(|c| aterm_spec::xref::machine_matches("SelectionCustody", &c.machine))
            .expect("SelectionCustody must be in the coverage ledger");
        assert!(
            custody.active,
            "SelectionCustody must be ACTIVELY-BOUND (>= 1 refinement on the real \
             selection seams), not report-only"
        );
        assert_eq!(
            custody.total_actions, 11,
            "SelectionCustody action inventory drifted; update the anchors AND the \
             Tier-1 conformance, found {}",
            custody.total_actions
        );
        assert_eq!(
            custody.ratio(),
            1.0,
            "SelectionCustody must be fully bound-or-waived; uncovered = {:?}",
            custody.uncovered
        );
        for action in SELECTION_CUSTODY_ACTIONS {
            assert!(
                custody.bound.contains(*action),
                "SelectionCustody action `{action}` must carry a #[refines] anchor on a \
                 REAL seam (it is bound, not waived: every one of the eleven has a site)"
            );
        }
        assert!(
            custody.waived.is_empty(),
            "SelectionCustody carries no waivers — every action has an observable \
             shipping site; found {:?}",
            custody.waived
        );
    }
    // Execution evidence: `run_conformance` drives each step in its own window and
    // returns the ledger; `assert_step_evidence` checks each action's function was
    // entered by every window driving it and by none driving an action it does not
    // claim.
    let sel_ev = run_conformance_body(
        "SelectionCustody",
        &mut failed_bodies,
        super::selection_custody_conformance::run_conformance,
    );

    // Press custody: anchors sit on the four recorders that write `Terminal::note_custody`
    // (`app_input::note_press_custody`, `app_mouse::note_selection_custody`,
    // `Terminal::note_scroll_custody` inside the primitives that raise `display_offset`,
    // and `Terminal::note_output_custody`). They must differ from `SelectionCustody`'s
    // seams because the `#[refines]` const name omits the machine, so same-named
    // actions cannot share a function. Recording which transition fired is what
    // distinguishes actions that are otherwise identical in every observable, and it
    // backs the `custody` control verb.
    {
        let press = report
            .coverage
            .iter()
            .find(|c| aterm_spec::xref::machine_matches("PressCustody", &c.machine))
            .expect("PressCustody must be in the coverage ledger");
        assert!(
            press.active,
            "PressCustody must be ACTIVELY-BOUND (>= 1 refinement on the real custody \
             recorders), not report-only"
        );
        assert_eq!(
            press.total_actions, 16,
            "PressCustody action inventory drifted; update the anchors AND the Tier-1 \
             conformance, found {}",
            press.total_actions
        );
        assert_eq!(
            press.ratio(),
            1.0,
            "PressCustody must be fully bound-or-waived; uncovered = {:?}",
            press.uncovered
        );
        for action in PRESS_CUSTODY_ACTIONS {
            assert!(
                press.bound.contains(*action),
                "PressCustody action `{action}` must carry a #[refines] anchor on the \
                 REAL recorder (it is bound, not waived: the custody record gives every \
                 one of the sixteen an observable site)"
            );
        }
        assert!(
            press.waived.is_empty(),
            "PressCustody carries no waivers — `OutputAtLive` is anchored and DRIVEN \
             rather than waived, because the record makes its transition nameable at \
             all: a batch that destroyed the highlight it was not entitled to \
             destroy comes back under a DIFFERENT variant, so the step fails on the \
             recorded name; found {:?}",
            press.waived
        );
    }
    // Execution evidence as for SelectionCustody: pins that the recorders are the
    // functions each transition's step actually reaches.
    let press_ev = run_conformance_body(
        "PressCustody",
        &mut failed_bodies,
        super::press_custody_conformance::run_conformance,
    );
    // Every body has run under its own catch; name all failures.
    assert!(
        failed_bodies.is_empty(),
        "spec_xref_closure: {} Tier-1 conformance bod(ies) failed (a later entry can \
         be a consequence of an earlier one; `--ignored --exact <wrapper>` runs one \
         alone):\n{}",
        failed_bodies.len(),
        failed_bodies.join("\n")
    );
    let (Some(sel_ev), Some(press_ev)) = (sel_ev, press_ev) else {
        unreachable!("a body that returned no ledger is listed in failed_bodies")
    };
    // SelectionCustody's execution evidence (its ledger was taken above).
    let sel_summary = assert_step_evidence("SelectionCustody", SELECTION_CUSTODY_ACTIONS, &sel_ev);
    crate::logging::stderr_line!(
        "spec_xref_closure: SelectionCustody is actively-bound (11/11 actions, zero \
         waivers) AND its Tier-1 conformance (real drag/press gestures + real VT \
         damage/scroll/eviction/ED-3/RIS batches) was RUN by the gate — with each \
         action proved to have entered AT LEAST ONE of its annotated functions in \
         EVERY STEP WINDOW THAT DRIVES IT, not merely somewhere in the run. (\"At \
         least one\": the design permits several anchor sites per action, and the \
         check is an OR across them, so a second site is never required to run — the \
         count of sites that never did is in the derived numbers.) THE EXACT CLAIM, \
         in two halves, and no more than the two. FIRST, entry: an anchor moved onto \
         a function this action's own windows do NOT enter fails by name — a stub \
         with no callers in the tree, or a function only some other step calls. That \
         half alone would be weak, because a window is only as narrow as the seam its \
         step calls and a `Terminal::process` batch enters several anchored functions \
         at once. SECOND, and this is what narrows it inside one window: an anchored \
         function must NOT be entered by a window driving an action it is not \
         anchored for. `SelectionCustody::UniformScroll` parked on \
         `Terminal::note_output_custody` — a function that same batch really does \
         enter — fails on that second half, because it also runs in the damage and \
         invalidation windows. WHAT SURVIVES BOTH: a move onto a function whose entry \
         pattern is EXACTLY the action set it claims (including any unanchored \
         function inside the window, which runs no probe and so cannot even be \
         counted below), and any permutation of the actions that share one function. \
         {sel_summary} \
         Still unproved, and WIDER than the sentence above: this audit's whole \
         notion of a \"function\" is the bare `fn` IDENTIFIER the macro captured, so \
         two distinct functions that happen to share a name are ONE node here and a \
         move between them is invisible — the entry-pattern check cannot separate \
         what the identifier already merged. Beyond that the `project=` and \
         `rust_method` paths remain unresolved strings no layer maps to a symbol \
         (obligation 2 proper — Phase 3 `trust-ir`), and the trust-ir manifest this \
         gate emits is unchanged by any of it. The conformance remains the half that \
         carries real weight: it drives shipping code and is falsified by a \
         regression in it."
    );
    let press_summary = assert_step_evidence("PressCustody", PRESS_CUSTODY_ACTIONS, &press_ev);
    crate::logging::stderr_line!(
        "spec_xref_closure: PressCustody is actively-bound (16/16 actions, zero \
         waivers) AND its Tier-1 conformance (real scroll/wheel/gesture/press seams — \
         all four press classes delivered as real key events — plus real \
         `Terminal::process` batches, every step named by the ENGINE's own custody \
         record rather than by the harness) was RUN by the gate, each step inside its \
         own execution-evidence window. As with SelectionCustody the coverage half is \
         no longer only a claim about anchor STRINGS — each action's annotated \
         function (or, where an action has several anchor sites, at least one of \
         them) is proved to have been entered by EVERY window that drives it AND to \
         be entered by no window driving an action it is not anchored for AMONG THIS \
         MACHINE'S OWN ANCHORS — the scope matters, because a function carrying \
         anchors for a DIFFERENT machine is invisible to this audit, and the \
         shared-function line below prints the counterexample from inside this same \
         ledger — the same two halves as above, and with the same residual: a function whose entry \
         pattern is exactly the action set it claims is indistinguishable from the \
         right seam, and so is any permutation of the actions sharing one function. \
         Per-branch attribution and \
         `project=` symbol resolution remain unproved: {press_summary} The \
         conformance is still the half that drives shipping code and goes red on a \
         regression in it. Its per-step verdict is a REFINEMENT check (the action's \
         guard and its exact update image) PLUS an evaluation of every model \
         invariant over the observed pre- and post-states; whole-reachable-space \
         invariant checking is Tier-0's, in `aterm-spec/tests/derived_ring_ty.rs`."
    );

    // How far execution evidence reaches: per-step windows must be added inside each
    // conformance module, so others get string coverage only. The audited count and
    // names are derived from the ledgers (`StepEvidence::machine()`); the string-only
    // list below is hand-maintained and its count taken from the array.
    {
        /// Tier-1 conformances run here without per-step windows. Hand-maintained;
        /// forgetting one under-reports.
        const STRING_ONLY_CONFORMANCES: &[&str] = &[
            "window_routing",
            "pane_tree",
            "session_pool",
            "tab_strip",
            "AsymmetricPadLayout",
            "input_release_pairing",
        ];
        let audited_ledgers: [&aterm_spec::xref::StepEvidence; 2] = [&sel_ev, &press_ev];
        let mut machines: BTreeSet<&'static str> = BTreeSet::new();
        let mut all_anchors = 0usize;
        for r in xref::refinements() {
            machines.insert(r.machine);
            all_anchors += 1;
        }
        let audited_names: Vec<&'static str> =
            audited_ledgers.iter().map(|e| e.machine()).collect();
        let audited_machines = audited_names.len();
        let audited: usize = audited_ledgers.iter().map(|e| e.audit().anchors).sum();
        let widest: usize = audited_ledgers
            .iter()
            .map(|e| e.audit().wide_windows().2.len())
            .max()
            .unwrap_or(0);
        crate::logging::stderr_line!(
            "spec_xref_closure: EXECUTION-EVIDENCE REACH — {audited_machines} of {} \
             anchor-bearing machines linked into this binary are audited for per-step \
             function entry ({audited_names:?}; {audited} of {all_anchors} anchors). \
             The other {} carry the STRING closure only: for them a green ratio still \
             says an action name is claimed by some anchor and says nothing about the \
             function it sits on. {} of them run a Tier-1 conformance in this very \
             test ({STRING_ONLY_CONFORMANCES:?} — a HAND-MAINTAINED list, unlike the \
             counts around it) and are the cheapest to convert; the rest have no \
             in-process conformance here at all. And for the {audited_machines} that \
             ARE audited, the evidence is function ENTRY inside a window as wide as \
             the seam its step calls — up to {widest} anchored functions in one \
             window here — never a proof that the anchored function is the one that \
             performs the action.",
            machines.len(),
            machines.len().saturating_sub(audited_machines),
            STRING_ONLY_CONFORMANCES.len(),
        );
    }

    // ---- Proof #4 (non-vacuity / Tier-0 under the armed ty) is its own test,
    // `spec_xref_strict_vacuity` below: it reads nothing the proofs here build,
    // and apart from them it runs beside them rather than after them. ----

    // Analysis #5: TrustIr independently analyzes the obligations in design-only mode.
    // Lower the modules and deduplicated anchors/waivers to v27 `.trust_ir`, run
    // `trust-ir spec-link`, and require no structural, Ob.1/Ob.4, L2 or S0-S2
    // violation, every active machine at
    // 100% (Ob.3 still enforced by the in-Rust closure), and either certifying exit 0 or
    // v27's violation-free design-only result. Design-only is an integrity check, not
    // certification, and the notice says so.
    if let Some(trust_ir) = aterm_spec::verify::trust_ir_escalation(
        "spec_xref_closure (Phase-3 TrustIr design-only analysis, Analysis #5)",
    ) {
        let fallback_deduplicated =
            aterm_spec::ir::fallback_anchor_deduplication_count(&modules, &refs);
        let module_txt = aterm_spec::ir::lower_to_ir("aterm_spec_xref", &modules, &refs, &waivers);
        let lowered = aterm_spec::ir::lowered_machine_names(&modules, &refs);
        crate::logging::stderr_line!(
            "spec_xref_closure: assembled .trust_ir — {} bytes, {} SpecModule block(s), {} \
             actively-lowered machine(s): {:?}; {} colliding source \
             anchor record(s) omitted from TrustIr's unresolved S0 fallback view",
            module_txt.len(),
            modules.len(),
            lowered.len(),
            lowered,
            fallback_deduplicated,
        );

        let ir_dir = std::env::temp_dir().join(format!("aterm_spec_ir_{}", std::process::id()));
        std::fs::create_dir_all(&ir_dir).expect("mk ir tmp dir");
        let ir_path = ir_dir.join("aterm_spec_xref.trust_ir");
        std::fs::write(&ir_path, &module_txt).expect("write .trust_ir");

        let (exit_code, report) = run_spec_link(&trust_ir, &ir_path);
        crate::logging::stderr_line!(
            "--- trust-ir spec-link report (REAL assembled module, L2 armed) ---\n{report}"
        );
        let disposition = classify_spec_link(exit_code, &report).unwrap_or_else(|| {
            panic!(
                "trust-ir spec-link produced neither a certifying result nor an explicit \
                 design-only zero-violation analysis — structural/Ob/L1/L2/S0-S2 failures \
                 remain hard errors. Report:\n{report}\n--- module ({} bytes) ---\n{module_txt}{}",
                module_txt.len(),
                stale_trust_ir_hint(exit_code, &report),
            )
        });
        if disposition == SpecLinkDisposition::Certifying {
            assert_eq!(
                fallback_deduplicated, 0,
                "a certifying TrustIr result cannot stand in for source anchors omitted by \
                unresolved fallback-identity deduplication"
            );
        }
        // Every actively-lowered machine must appear under its canonical name with
        // its exact declared action count fully covered. This is stricter than merely
        // finding a machine-name substring in a design-only report.
        for canon in &lowered {
            let expected_actions = modules
                .iter()
                .find(|module| module.name() == canon.as_str())
                .unwrap_or_else(|| panic!("lowered machine {canon:?} has no module"))
                .coverage_actions()
                .len();
            assert!(
                report_has_exact_full_coverage(&report, canon, expected_actions, disposition),
                "trust-ir report must give actively-lowered machine {canon:?} exact \
                 {expected_actions}/{expected_actions} coverage in {disposition:?} mode; \
                 report:\n{report}"
            );
        }
        match disposition {
            SpecLinkDisposition::Certifying => crate::logging::stderr_line!(
                "trust-ir spec-link: CERTIFYING — typed Trust linkage was present and every \
                 checked obligation passed."
            ),
            SpecLinkDisposition::ExplicitDesignOnly => crate::logging::stderr_line!(
                "NOTICE — trust-ir spec-link is VIOLATION-FREE BUT NON-CERTIFYING: every \
                 aterm SpecModule is explicitly design-only (`target none`) because aterm \
                 does not yet own Trust FuncIds/typed projection targets. This external run \
                 analyzed the emitted fallback-deduplicated structure and coverage \
                 reporting; it did not inspect {fallback_deduplicated} colliding source \
                 anchor record(s) omitted from that view. The in-Rust full-inventory closure \
                 and Tier-1 conformance remain the authority."
            ),
        }
        let _ = std::fs::remove_dir_all(&ir_dir);
    }

    crate::logging::stderr_line!(
        "spec_xref_closure: LOCAL GREEN — obligations 1/3/4 hold for terminal_modes + {} \
         ISOLATION machines; Tier-1 conformances discharged (interpreter default, ty \
         escalation). TrustIr analysis ran when installed; an explicit design-only result \
         is non-certifying and is never counted as external proof (Phase 3).",
        ISOLATION.len()
    );
}

#[test]
fn spec_xref_strict_vacuity() {
    // Proof #4: every embedded model is `ty check --strict-vacuity`ed (vacuous exit 3 on
    // an empty Init, a never-fired anchored action, or a vacuously true invariant) and
    // must pass.
    //
    // The one tolerated class is a `Buggy`-guarded negative-control action, dead at
    // `Buggy=0` by design. Scalar models run with `--allow-vacuous=dead-action`, and
    // `verify::audit_dead_negative_controls` machine-verifies every dead action: the
    // interpreter recomputes the dead set (exact agreement both ways), requires
    // `Buggy=0`, proves the all-live `Buggy=1` baseline safe, and proves each dead
    // action fires and is caught when added back alone. Anything else stays red.
    //
    // PARALLEL (2026-10-01). Both halves of each model's obligation are
    // unchanged; what changed is how many models are in flight. While
    // `ty_output` held the per-user `ty` lock, `ty` checked one model at a
    // time, so the most this test could do was pipeline: the interpreter's
    // audits ran on two threads beside one sequential `ty` queue (46 s run
    // in turn, 20 s of it `ty`, measured 2026-09-29; 9.3 s pipelined, 8.3 s
    // of it `ty`, measured 2026-10-01). Phase 0 of
    // `docs/design/TY-TRACE-CASES-AND-FAIL-CLOSED-2026-10-01.md` deleted the
    // lock, so each model's `ty` run and the audit of its transcript now go
    // back to back on one of up to `available_parallelism` threads, each
    // owning a contiguous chunk of the registry: 5.0 s on 14 cores, held up
    // by the one chunk holding `HarnessUpgradeLadder` (3.6 s alone, 2.6 s of
    // it the audit), against 1.3 s for the mean chunk (measured 2026-10-01).
    // A SHARED QUEUE (2026-10-05): on the 6-core gate host a contiguous
    // chunk queued every model behind its heaviest one — 109 s for the
    // test against 223 s of work, `SupervisorAccountRotation` alone 64 s
    // of it (18 s `ty`, 46 s audit); the threads now take the next model in
    // registry order, so a heavy model holds up nothing behind it (75 s,
    // measured the same day under the same load). No model is taken after
    // a failure, so the panic this test re-raises is still the first
    // failure in registry order, the one the sequential loop stopped at;
    // each other failure is printed beside it. (It was a
    // block of `spec_xref_closure` until 2026-09-29; nothing there reads it,
    // and on its own it runs beside that test rather than after it.)
    let Some(ty) = aterm_spec::verify::ty_escalation("spec_xref_closure strict-vacuity (Proof #4)")
    else {
        return;
    };
    /// `ty`'s dead-action report line, in both the strict form
    /// (`Warning: vacuous: N dead action(s) (never fired): A, B`) and the
    /// relaxed form (`note: vacuity warning downgraded by
    /// --allow-vacuous=dead-action: N dead action(s) (never fired): A, B`).
    fn parse_dead_actions(out: &str) -> Vec<String> {
        const MARKER: &str = "dead action(s) (never fired): ";
        out.lines()
            .find_map(|l| l.find(MARKER).map(|i| &l[i + MARKER.len()..]))
            .map(|names| {
                names
                    .split(',')
                    .map(|n| n.trim().to_string())
                    .filter(|n| !n.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    }
    /// The interpreter's half of one scalar model's obligation, on the
    /// transcript of its `ty` run: the dead actions it proves to be caught
    /// negative controls (0 for none), or a panic naming the model.
    fn audit_transcript(m: &aterm_spec::derive::Model, text: &str) -> usize {
        // First prove `ty` walked the same space the interpreter does: a dead set
        // measured on a reduced space is not evidence.
        aterm_spec::verify::assert_ty_saw_whole_space(m, text, "spec_xref_closure");
        // ALWAYS audited (not only when ty reports dead actions): the
        // interpreter recomputes the dead set itself, so a silently
        // under-reported/unparsed ty warning cannot fail open.
        let reported = parse_dead_actions(text);
        let reported_refs: Vec<&str> = reported.iter().map(String::as_str).collect();
        match aterm_spec::verify::audit_dead_negative_controls(m, &reported_refs) {
            Ok(0) => 0,
            Ok(n) => {
                crate::logging::stderr_line!(
                    "spec_xref_closure: `{}` — {n} dead action(s) at the committed \
                    config machine-verified as independent prove-and-catch negative \
                     controls (safe all-live baseline; each isolated mutant fires and \
                     is caught): {:?}",
                    m.name,
                    reported
                );
                n
            }
            Err(why) => panic!(
                "ty check --strict-vacuity: derived model `{}` has dead action(s) \
                 that are NOT machine-verified negative controls — {why}\n{text}",
                m.name
            ),
        }
    }
    /// One model's whole obligation, in `dir`: its `ty check
    /// --strict-vacuity` run, then (scalar models) the interpreter's audit of
    /// that run's transcript. Returns the dead actions proven to be caught
    /// negative controls (0 for none), or panics naming the model.
    fn check_model(
        ty: &std::path::Path,
        dir: &std::path::Path,
        m: &aterm_spec::derive::Model,
    ) -> usize {
        let tla = dir.join(format!("{}.tla", m.name));
        let cfg = dir.join(format!("{}.cfg", m.name));
        std::fs::write(&tla, m.to_tla()).expect("write tla");
        std::fs::write(&cfg, m.to_cfg()).expect("write cfg");
        // std's Command: `aterm_spec::verify::ty_output` (aterm-spec) takes one.
        let mut cmd = std::process::Command::new(ty);
        cmd.arg("check")
            .arg(&tla)
            .arg("--config")
            .arg(&cfg)
            .arg("--strict-vacuity")
            // V2 evidence is an oracle obligation, not a performance
            // lane: the current trust-cg fused path compiles every
            // action but does not feed its counters into the vacuity
            // report. Force explicit sequential coverage so allow mode
            // records the exact dead set for the fail-closed tier
            // comparison. (`--backend interpreter` and `--workers 1`
            // come from the shared arming below, which is also where
            // they belong; `ty` refuses a flag given twice.)
            .arg("--coverage");
        // THE WHOLE SPACE, from the ONE place that defines what that
        // means. This gate used to hand-roll its flag list and omitted
        // `--no-auto-por`, so it ran the entire registry with
        // partial-order reduction ON — 128 reachable states of
        // `RainbowJumpBurstLifecycle` collapsed to 1, four of its six
        // actions never fired, and `--strict-vacuity` called them dead.
        // A dead set measured on a reduced space is not evidence about
        // the model, and `aterm-spec`'s driver had already learned that
        // (see `arm_whole_space_check`). Two drivers, one arming.
        aterm_spec::verify::arm_whole_space_check(&mut cmd);
        // Scalar models earn the dead-action relaxation via the interpreter
        // audit below; function-valued models CANNOT be audited (the
        // interpreter can't evaluate them), so they stay fully strict.
        let auditable = m.fn_vars.is_empty();
        if auditable {
            cmd.arg("--allow-vacuous=dead-action");
        }
        let out = aterm_spec::verify::ty_output(&mut cmd)
            .unwrap_or_else(|e| panic!("failed to run {ty:?}: {e}"));
        // The transcript names its own checker: every panic below quotes
        // this text, and "which `ty` produced this?" was the single most
        // expensive question in diagnosing the 2026-08-06 red gate.
        let text = format!(
            "{}{}\n{}",
            aterm_spec::verify::ty_evidence_header(ty),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            out.status.success(),
            "ty check --strict-vacuity FAILED (VACUOUS or property violation) for derived \
             model `{}`{}\n{text}",
            m.name,
            if auditable {
                " (a non-dead-action vacuity class, or a violation — \
                 --allow-vacuous=dead-action was in effect)"
            } else {
                " (function-valued model: fully strict, no relaxation available)"
            },
        );
        if auditable {
            audit_transcript(m, &text)
        } else {
            0
        }
    }
    let dir = std::env::temp_dir().join(format!("aterm_spec_xref_gui_{}", std::process::id()));
    let registry = aterm_spec::xref::model_registry();
    let threads = std::thread::available_parallelism().map_or(1, usize::from);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let failed = std::sync::atomic::AtomicBool::new(false);
    let slots: Vec<std::sync::Mutex<Option<std::thread::Result<usize>>>> = registry
        .iter()
        .map(|_| std::sync::Mutex::new(None))
        .collect();
    std::thread::scope(|scope| {
        for k in 0..threads.min(registry.len()) {
            // A directory per worker: no two threads ever write one path,
            // whatever names the registry holds.
            let dir = dir.join(k.to_string());
            let (ty, models, next, failed, slots) = (&ty, &registry, &next, &failed, &slots);
            scope.spawn(move || {
                std::fs::create_dir_all(&dir).expect("mk tmp dir");
                // A shared queue in registry order, not a contiguous chunk
                // per thread: one heavy model no longer holds up the models
                // queued behind it on its thread.
                while !failed.load(std::sync::atomic::Ordering::SeqCst) {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let Some(m) = models.get(i) else { break };
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        check_model(ty, &dir, m)
                    }));
                    if r.is_err() {
                        failed.store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                    *slots[i]
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(r);
                }
            });
        }
    });
    // Models are taken in registry order and no new one is taken after a
    // failure, so every model before the first failing one ran: the panic
    // re-raised here is the first failure in registry order, the one a
    // sequential loop stops at (each other failure printed beside it).
    let mut dead_per_model = Vec::with_capacity(registry.len());
    for slot in slots {
        match slot
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            Some(Ok(n)) => dead_per_model.push(n),
            Some(Err(panic)) => std::panic::resume_unwind(panic),
            None => unreachable!("a model was skipped with no failure before it"),
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    crate::logging::stderr_line!(
        "spec_xref_closure: all {} embedded models pass `ty check --strict-vacuity` (armed \
         Trust) — {} dead mutant action(s) across {} model(s) machine-verified as \
         independent prove-and-catch negative controls (interpreter-recomputed dead sets, \
         safe baseline + isolated fire/catch proven); every other model is strictly \
         non-vacuous.",
        registry.len(),
        dead_per_model.iter().sum::<usize>(),
        dead_per_model.iter().filter(|&&n| n > 0).count(),
    );
}

/// `trust-ir spec-link` rejects a violating module even in design-only mode: a module
/// with a bogus anchor (an undeclared action) exits 1 with `[Ob.1]`, and the same
/// module without it is violation-free (certifying or design-only, never presented as
/// certification). A controlled negative/positive pair.
#[test]
fn trust_ir_has_teeth() {
    use aterm_spec::xref::{RefinementAnchor, SpecModule};

    // Tiered: external-tool negative control, run only with the Trust toolchain.
    let Some(trust_ir) =
        aterm_spec::verify::trust_ir_escalation("trust_ir_has_teeth (Phase-3 negative control)")
    else {
        return;
    };

    // A single embedded model (the ring) is enough to demonstrate the obligation.
    let modules = vec![SpecModule::Embedded(aterm_spec::derive::ring_model())];

    // A VALID anchor (Push exists in Ring), so the good module is fully covered.
    let good_anchor = RefinementAnchor {
        machine: "ring",
        action: "Push",
        rust_method: "aterm_buffer::Ring::push",
        location: "crates/aterm-buffer/src/ring.rs:1:1",
        project: "aterm_buffer::Ring::project",
        // Hand-built fixture: no macro ran, so no injected entry probe. `trust-ir`
        // lowering is about the STRING closure; execution evidence is the
        // conformance-owning gate's per-step `StepEvidence` window.
        entry_id: "",
    };

    // The BOGUS anchor: `MeltDown` is not an action of the Ring model. trust-ir's
    // Ob.1 (action-exists) must reject it.
    let bogus_anchor = RefinementAnchor {
        machine: "ring",
        action: "MeltDown",
        rust_method: "aterm_buffer::Ring::melt_down",
        location: "crates/aterm-buffer/src/ring.rs:9:9",
        project: "",
        entry_id: "",
    };

    let dir = std::env::temp_dir().join(format!("aterm_spec_teeth_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mk teeth tmp dir");

    // ---- Negative: the BAD module must be REJECTED (exit 1) with [Ob.1] ----
    let bad_txt = aterm_spec::ir::lower_to_ir(
        "aterm_teeth_bad",
        &modules,
        &[&good_anchor, &bogus_anchor],
        &[],
    );
    let bad_path = dir.join("bad.trust_ir");
    std::fs::write(&bad_path, &bad_txt).expect("write bad module");
    // Sanity: the lowering actually emitted the bogus anchor against canonical "Ring".
    assert!(
        bad_txt.contains("anchor machine \"Ring\" action \"MeltDown\""),
        "bogus anchor must be present in the lowered module:\n{bad_txt}"
    );

    let (bad_exit_code, bad_report) = run_spec_link(&trust_ir, &bad_path);
    crate::logging::stderr_line!("--- trust-ir spec-link report (BOGUS module) ---\n{bad_report}");
    assert_eq!(
        bad_exit_code,
        Some(1),
        "the explicit DesignOnly negative control must fail with exact exit code 1; \
         signals/crashes and other nonzero exits do not demonstrate the intended rejection. \
         Report:\n{bad_report}{}",
        stale_trust_ir_hint(bad_exit_code, &bad_report),
    );
    assert!(
        classify_spec_link(bad_exit_code, &bad_report).is_none()
            && report_has_spec_link_failure(&bad_report),
        "trust-ir spec-link must REJECT a module with an undeclared anchor action; an \
         explicit design-only status must not hide the violation. Report:\n{bad_report}"
    );
    assert!(
        bad_report.contains("[Ob.1") && bad_report.contains("MeltDown"),
        "trust-ir must report the [Ob.1 action-exists] violation naming `MeltDown`; \
         report:\n{bad_report}"
    );

    // ---- Positive: drop the bogus anchor -> the same module is violation-free ----
    // Cover the remaining action (Ring has only Push) — already covered by the
    // good anchor, so no waiver needed; the good module is fully bound.
    let good_txt = aterm_spec::ir::lower_to_ir("aterm_teeth_good", &modules, &[&good_anchor], &[]);
    let good_path = dir.join("good.trust_ir");
    std::fs::write(&good_path, &good_txt).expect("write good module");
    let (good_exit_code, good_report) = run_spec_link(&trust_ir, &good_path);
    crate::logging::stderr_line!(
        "--- trust-ir spec-link report (CONTROL good module) ---\n{good_report}"
    );
    let good_disposition = classify_spec_link(good_exit_code, &good_report).unwrap_or_else(|| {
        panic!(
            "the same module WITHOUT the bogus anchor must be certifying OR explicit \
             design-only and violation-free, so rejection remains attributable solely to \
            the undeclared action. Report:\n{good_report}"
        )
    });
    assert!(
        report_has_exact_full_coverage(&good_report, "Ring", 1, good_disposition),
        "the controlled good Ring must report exact 1/1 coverage; report:\n{good_report}"
    );

    let _ = std::fs::remove_dir_all(&dir);
    crate::logging::stderr_line!(
        "trust_ir_has_teeth: TRUST rejected the bogus anchor with [Ob.1] and accepted the \
         controlled good module as {good_disposition:?}. ExplicitDesignOnly means \
         violation-free analysis, not external certification."
    );
}
