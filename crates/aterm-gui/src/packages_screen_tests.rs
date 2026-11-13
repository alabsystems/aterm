// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn refresh(report: PackagesStatusReport) -> PackagesWorkerCompletion {
    PackagesWorkerCompletion::refresh(report)
}

fn succeeded(report: PackagesStatusReport, operation: PackagesBusy) -> PackagesWorkerCompletion {
    PackagesWorkerCompletion::command(report, PackagesCommandOutcome::Succeeded { operation })
}

fn status(outcome: &str) -> atpkg::Status {
    let mut programs = std::collections::BTreeMap::new();
    programs.insert(
        "ay".to_string(),
        atpkg::ProgramStatus {
            installed_build: Some(1971),
            state: "active".to_string(),
            tree_root: String::new(),
        },
    );
    atpkg::Status {
        schema: 1,
        updated_at: "2026-07-21T00:00:00Z".to_string(),
        enabled: true,
        index_source: "alabsystems/aterm".to_string(),
        outcome: outcome.to_string(),
        seams: Vec::new(),
        last_success_at: String::new(),
        last_index_build: 0,
        index_build_changed_at: String::new(),
        last_pass: String::new(),
        last_pass_at: String::new(),
        last_pass_attempted_index_build: 0,
        last_pass_attempted_at: String::new(),
        pass_seq: 0,
        stale_index_only_pass_seq: 0,
        programs,
        refused_shims: Default::default(),
        extra: Default::default(),
    }
}

#[test]
fn service_serializes_workers_and_rejects_stale_completions() {
    let mut service = PackagesService::new();
    let base = service.revision();
    let seq = service
        .begin(Some(PackagesBusy::Check))
        .expect("idle → begin");
    assert!(service.revision() > base, "busy flip fans out");
    assert_eq!(service.busy(), Some(PackagesBusy::Check));
    assert!(
        service.begin(Some(PackagesBusy::Install)).is_none(),
        "second verb joins, never overlaps"
    );

    let report =
        PackagesStatusReport::from_parts(true, true, "fp".into(), Some(&status("ok")), &[]);
    assert!(
        !service.finish(seq + 1, succeeded(report.clone(), PackagesBusy::Check)),
        "a superseded worker cannot import facts"
    );
    assert!(!service.finish(seq, refresh(report.clone())), "kind");
    assert!(
        !service.finish(seq, succeeded(report.clone(), PackagesBusy::Install)),
        "operation"
    );
    assert_eq!(
        service.busy(),
        Some(PackagesBusy::Check),
        "a stale or mismatched finish is inert"
    );
    let before = service.revision();
    assert!(service.finish(seq, succeeded(report, PackagesBusy::Check)));
    assert_eq!(service.busy(), None);
    assert!(service.revision() > before);
    let projection = service.state(true, true, true).projection();
    assert!(projection.recorded);
    assert!(projection.actions_enabled);
}

#[test]
fn abort_releases_the_reservation_without_fabricating_a_report() {
    let mut service = PackagesService::new();
    let seq = service.begin(None).expect("idle → begin");
    let before = service.revision();
    assert!(service.abort(seq), "current abort releases");
    assert!(service.revision() > before, "the un-busy flip fans out");
    assert_eq!(service.busy(), None);
    assert!(
        !service.state(true, true, true).projection().observed,
        "never-observed stays honestly unobserved after an abort"
    );

    let seq = service.begin(Some(PackagesBusy::Check)).unwrap();
    let report =
        PackagesStatusReport::from_parts(true, true, "fp".into(), Some(&status("ok")), &[]);
    assert!(service.finish(seq, succeeded(report, PackagesBusy::Check)));
    let seq = service.begin(Some(PackagesBusy::Install)).unwrap();
    assert!(!service.abort(seq + 1), "stale abort is inert");
    assert_eq!(service.busy(), Some(PackagesBusy::Install));
    assert!(service.abort(seq));
    let projection = service.state(true, true, true).projection();
    assert!(projection.recorded, "the prior report's facts survive");
    assert_eq!(projection.programs.len(), 1);
    assert!(projection.actions_enabled);
}

#[test]
fn projection_headlines_track_the_honest_manager_posture() {
    let unobserved = PackagesState::unobserved().projection();
    assert!(
        !unobserved.observed && !unobserved.actions_enabled,
        "nothing is claimed before observation"
    );

    let mut service = PackagesService::new();
    let seq = service.begin(None).unwrap();
    assert!(service.finish(
        seq,
        refresh(PackagesStatusReport::from_parts(
            false,
            false,
            String::new(),
            None,
            &[],
        )),
    ));
    let unavailable = service.state(true, true, true).projection();
    assert_eq!(
        unavailable.detail.as_deref(),
        Some(super::PACKAGE_TOOL_MISSING_DETAIL)
    );
    assert!(!unavailable.actions_enabled);

    let seq = service.begin(None).unwrap();
    assert!(service.finish(
        seq,
        refresh(PackagesStatusReport::from_parts(
            true,
            true,
            "fp".into(),
            Some(&status("up to date")),
            &[]
        )),
    ));
    let live = service.state(true, true, true).projection();
    assert!(live.actions_enabled);

    let _ = service.begin(Some(PackagesBusy::Install)).unwrap();
    let busy = service.state(true, true, true).projection();
    assert_eq!(busy.busy, Some(PackagesBusy::Install));
    assert!(!busy.actions_enabled);
}

fn report_with(rows: &[(&str, String)]) -> PackagesStatusReport {
    let mut status = status("up to date (index build 44)");
    status.programs = rows
        .iter()
        .map(|(name, state)| {
            (
                (*name).to_string(),
                atpkg::ProgramStatus {
                    installed_build: Some(
                        atpkg::state::managed_pin(state).map_or(1, |(build, _)| build),
                    ),
                    state: state.clone(),
                    tree_root: String::new(),
                },
            )
        })
        .collect();
    PackagesStatusReport::from_parts(true, true, "fp".into(), Some(&status), &[])
}

fn observe(service: &mut PackagesService, report: PackagesStatusReport) {
    let sequence = service.begin(None).expect("idle");
    assert!(service.finish(sequence, refresh(report)));
}

fn projection(service: &PackagesService) -> PackagesProjection {
    service.state(true, true, true).projection()
}

/// A LAN SHARING FLIP NEVER TAKES THE PAGE'S HERO: the switch runs through the packages verb
/// path, but its result is kept apart from the last verb's, so a failed check's headline and
/// detail stand through a flip, its run and a refresh after it. Only the switch's row says a
/// flip failed, until the next flip.
#[test]
fn a_sharing_flip_leaves_the_hero_to_the_last_verb() {
    let at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_800_000_000);
    let hero = |service: &PackagesService| {
        let p = service.state(true, true, true).projection_at(at);
        (p.headline, p.detail, p.sharing_failed)
    };
    let report = report_with(&[("ay", "active".to_string())]);
    let mut service = PackagesService::new();
    observe(&mut service, report.clone());
    let check = service.begin(Some(PackagesBusy::Check)).expect("idle");
    assert!(service.finish(
        check,
        PackagesWorkerCompletion::command(
            report.clone(),
            PackagesCommandOutcome::Failed {
                operation: PackagesBusy::Check,
                message: "atpkg update exited with status 7".to_string(),
            },
        ),
    ));
    let (headline, detail, failed) = hero(&service);
    assert_eq!(headline, "Package check failed");
    assert_eq!(failed, None);
    let stands = (headline, detail, None);

    let off = PackagesBusy::Sharing { on: false };
    let flip = service.begin(Some(off)).expect("idle");
    assert_eq!(
        projection(&service).busy,
        Some(off),
        "the switch shows it running"
    );
    assert_eq!(hero(&service), stands, "while the flip runs");
    assert!(service.finish(
        flip,
        PackagesWorkerCompletion::command(
            report.clone(),
            PackagesCommandOutcome::Failed {
                operation: off,
                message: "could not write units-off".to_string(),
            },
        ),
    ));
    let said = Some("LAN sharing not changed: could not write units-off".to_string());
    let after = (stands.0.clone(), stands.1.clone(), said);
    assert_eq!(
        hero(&service),
        after,
        "a failed flip says so on its row alone"
    );
    observe(&mut service, report.clone());
    assert_eq!(hero(&service), after, "a refresh keeps both");

    let on = PackagesBusy::Sharing { on: true };
    let flip = service.begin(Some(on)).expect("idle");
    assert_eq!(
        hero(&service),
        stands,
        "the next flip clears the last one's failure"
    );
    assert!(service.finish(flip, succeeded(report, on)));
    assert_eq!(hero(&service), stands, "and its success takes no headline");
}

#[test]
fn attention_is_the_doctors_faults_and_an_unreached_vendor() {
    let item = |name: &str, state: &str| attention_item(name, state);
    let unreached = atpkg::state::vendor_kept(
        "2.1.280",
        "Anthropic's release channel not reached since 2026-09-20",
    );
    for (name, fault) in [
        ("codex", "error: codex 0.157.0 refused: x"),
        ("codex", "error: stage: disk full"),
        ("trust", "aborted: stage"),
        ("claude", "tombstoned: claude 2.1.279 was yanked"),
        (
            "*toolset*",
            "unavailable: every published build was refused",
        ),
        ("*index*", "error: index unreachable"),
        ("claude", unreached.as_str()),
    ] {
        assert!(item(name, fault).is_some(), "{name}: {fault}");
    }
    let p = std::path::Path::new;
    for benign in [
        atpkg::state::managed(6808, 41),
        atpkg::state::vendor_managed("2.1.280", "Anthropic"),
        atpkg::state::vendor_kept("2.1.280", "held by local pin"),
        atpkg::state::vendor_source("2.1.280", "Anthropic"),
        atpkg::state::shadowed(1971, p("/Users//dev/.local/bin/ay")),
        atpkg::state::system(p("/opt/homebrew/bin/gh"), None),
        atpkg::state::AGENT_INSTALLING.to_string(),
        "extra — not installed (opt in: aterm pkg install ty)".to_string(),
        "needs admin — run: aterm pkg install clt".to_string(),
        "unavailable on x86_64-apple-darwin: no build is published for this target".to_string(),
        "blocked by clt: needs admin — run: aterm pkg install clt".to_string(),
        "held: pinned build 9 is not published for x86_64-apple-darwin; staying on build 8"
            .to_string(),
        "active".to_string(),
        "linked".to_string(),
    ] {
        assert_eq!(item("x", &benign), None, "{benign}");
    }
    assert_eq!(item("--help", "error: not a program"), None, "a stray row");
    for unserved in [
        atpkg::state::TOOLSET_UNSERVED,
        atpkg::state::TOOLSET_UNSERVED_BLOCKED,
    ] {
        assert_eq!(item("*toolset*", unserved), None, "{unserved}");
    }
}

#[test]
fn a_declined_store_wears_no_badge() {
    let mut declined = report_with(&[
        (
            "ay",
            "error: ay stage failed: no space left on device".to_string(),
        ),
        ("--help", "error: stray".to_string()),
    ]);
    declined.declined = true;
    let mut service = PackagesService::new();
    observe(&mut service, declined.clone());
    let p = projection(&service);
    assert!(
        p.attention.is_empty(),
        "declined: no badge: {:?}",
        p.attention
    );
    assert!(service.note_pass_trouble(
        PassTroubleKind::Failed,
        "atpkg: prefix is not writable".into(),
        at(100),
    ));
    assert_eq!(
        projection(&service).attention.len(),
        1,
        "the pass's own trouble is not a declined row"
    );
}

fn at(secs: u64) -> std::time::SystemTime {
    std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs)
}

fn completed_at(stamp: &str) -> PackagesStatusReport {
    let mut report = report_with(&[("ay", atpkg::state::managed(1971, 44))]);
    report.last_success_at = stamp.to_string();
    report
}

#[test]
fn a_missing_c_toolchain_is_the_badge_and_names_its_act() {
    let mut service = PackagesService::new();
    let healthy = report_with(&[("ay", atpkg::state::managed(1971, 44))]);
    observe(&mut service, healthy.clone());
    assert!(
        projection(&service).attention.is_empty(),
        "control: no badge"
    );
    let line = atpkg::prereq::CcVerdict::NoDriver.line();
    let mut blocked = healthy;
    blocked.cc_prereq = Some(atpkg::prereq::CcVerdict::NoDriver);
    observe(&mut service, blocked);
    let shown = projection(&service);
    let item = cc_prereq_item(&atpkg::prereq::CcVerdict::NoDriver);
    assert_eq!(shown.attention, vec![item.clone()], "one badge item");
    assert!(
        shown.detail.as_deref().is_some_and(|d| d.contains(&line)),
        "{:?}",
        shown.detail
    );
    assert_eq!(shown.programs.len(), 1, "no synthetic program row");

    let mut troubled = report_with(&[("ay", "error: verify failed".to_string())]);
    troubled.cc_prereq = Some(atpkg::prereq::CcVerdict::NoDriver);
    observe(&mut service, troubled);
    let shown = projection(&service);
    assert_eq!(shown.attention.len(), 2, "{:?}", shown.attention);
    assert_eq!(shown.attention[1], item, "{:?}", shown.attention);
}

#[test]
fn the_c_toolchain_is_probed_where_doctor_probes_and_said_only_when_it_blocks() {
    let installed = report_with(&[("ay", atpkg::state::managed(1971, 44))]);
    let note = cc_prereq_note(&installed, || atpkg::prereq::CcVerdict::NoDriver)
        .expect("an installed toolset with no C compiler is named");
    assert_eq!(note, atpkg::prereq::CcVerdict::NoDriver);
    let cc = std::path::PathBuf::from("/usr/bin/cc");
    let wedged = atpkg::prereq::CcVerdict::Unanswered {
        driver: cc.clone(),
        bound: atpkg::prereq::PROBE_TIMEOUT,
    };
    assert_eq!(cc_prereq_note(&installed, || wedged), None, "an unknown");
    let ready = atpkg::prereq::CcVerdict::Ready { driver: cc };
    assert_eq!(cc_prereq_note(&installed, || ready), None, "a compiler");
    let nothing = report_with(&[]);
    assert_eq!(
        cc_prereq_note(&nothing, || panic!("probed with nothing installed")),
        None
    );
    let mut declined = installed;
    declined.declined = true;
    assert_eq!(
        cc_prereq_note(&declined, || panic!("probed a declined toolset")),
        None
    );
}

#[test]
fn a_failed_pass_is_the_badge_until_a_clean_pass_clears_it() {
    let mut service = PackagesService::new();
    observe(
        &mut service,
        report_with(&[("ay", atpkg::state::managed(1971, 44))]),
    );
    assert!(projection(&service).attention.is_empty());
    let before = service.revision();
    assert!(service.note_pass_trouble(
        PassTroubleKind::Failed,
        "atpkg: prefix is not writable".into(),
        at(100),
    ));
    assert!(service.revision() > before, "the badge fans out");
    let settled = service.revision();
    assert!(!service.note_pass_trouble(
        PassTroubleKind::Failed,
        "atpkg: prefix is not writable".into(),
        at(200),
    ));
    assert_eq!(service.revision(), settled, "same words: nothing new");
    assert_eq!(projection(&service).attention.len(), 1, "the badge is up");
    assert!(service.note_pass_clean());
    assert!(!service.note_pass_clean(), "idempotent");
    let p = projection(&service);
    assert!(p.attention.is_empty(), "cleared: the badge is off");
    assert!(
        service.note_pass_trouble(
            PassTroubleKind::StoodDown,
            "an earlier toolchain pass was still running after 30 min — automatic updates \
         are off; run: aterm pkg seed"
                .into(),
            at(100),
        )
    );
    assert_eq!(
        projection(&service).attention.len(),
        1,
        "a stand-down badges too"
    );
}

#[test]
fn a_pass_completed_since_the_trouble_clears_it() {
    let noted = at(1_790_000_000);
    let troubled = || {
        let mut service = PackagesService::new();
        observe(&mut service, completed_at(""));
        assert!(service.note_pass_trouble(
            PassTroubleKind::Failed,
            "ay stage failed: no space left on device".into(),
            noted,
        ));
        service
    };
    let badged = |service: &PackagesService| !projection(service).attention.is_empty();
    for operation in [PackagesBusy::Check, PackagesBusy::Install] {
        let mut service = troubled();
        let sequence = service.begin(Some(operation)).unwrap();
        assert!(service.finish(sequence, succeeded(completed_at(""), operation)));
        assert!(!badged(&service), "{operation:?} succeeded: cleared");
        assert_eq!(
            projection(&service).headline,
            operation.completed_headline(),
            "the verb's own words, and nothing over them"
        );
    }
    let mut service = troubled();
    let sequence = service.begin(Some(PackagesBusy::Check)).unwrap();
    assert!(service.finish(
        sequence,
        PackagesWorkerCompletion::command(
            completed_at(""),
            PackagesCommandOutcome::Failed {
                operation: PackagesBusy::Check,
                message: "exit 1".into(),
            },
        ),
    ));
    assert!(badged(&service), "a failed Check proves nothing");
    let mut service = troubled();
    let sequence = service.begin(Some(PackagesBusy::MachineApply)).unwrap();
    assert!(service.finish(
        sequence,
        succeeded(completed_at(""), PackagesBusy::MachineApply)
    ));
    assert!(badged(&service), "a machine apply is not a pass");
    for (stamp, clears) in [
        ("2026-09-21T14:13:21Z", true),
        ("2026-09-21T14:13:20Z", false),
        ("2026-09-20T00:00:00Z", false),
        ("not a time", false),
    ] {
        let mut service = troubled();
        observe(&mut service, completed_at(stamp));
        assert_eq!(!badged(&service), clears, "{stamp}");
    }
}

#[test]
fn rows_parse_every_canonical_state() {
    let p = std::path::Path::new;
    let claude = atpkg::vendor_direct::Version::parse("2.1.280")
        .unwrap()
        .build_id();
    let fixture: Vec<(&str, String, Option<u64>)> = vec![
        ("trust", atpkg::state::managed(6808, 41), Some(6808)),
        (
            "ay",
            atpkg::state::shadowed(1971, p("/Users//dev/.local/bin/ay")),
            Some(1971),
        ),
        (
            "gh",
            atpkg::state::system(p("/opt/homebrew/bin/gh"), Some("2026-08-27")),
            None,
        ),
        ("codex", atpkg::state::agent_installing(), None),
        (
            "claude",
            atpkg::state::vendor_managed("2.1.280", "Anthropic"),
            Some(claude),
        ),
        (
            "vendorx",
            "extra — not installed (opt in: aterm pkg install vendorx)".to_string(),
            None,
        ),
        (
            "brew",
            "blocked by clt: needs admin — run: aterm pkg install clt".to_string(),
            None,
        ),
        (
            "emacs",
            atpkg::state::unavailable("aarch64-pc-windows-msvc", "no Windows-on-ARM build"),
            None,
        ),
        ("ny", "error: x".to_string(), None),
    ];
    let mut status = status("up to date");
    status.programs = fixture
        .iter()
        .map(|(name, state, build)| {
            (
                (*name).to_string(),
                atpkg::ProgramStatus {
                    installed_build: *build,
                    state: state.clone(),
                    tree_root: String::new(),
                },
            )
        })
        .collect();
    let report = PackagesStatusReport::from_parts(true, true, "fp".into(), Some(&status), &[]);
    let row = |name: &str| {
        report
            .programs
            .iter()
            .find(|row| row.name == name)
            .unwrap_or_else(|| panic!("{name} row"))
    };
    for (name, state, _) in &fixture {
        assert_eq!(
            &row(name).state,
            state,
            "{name} keeps the canonical spelling"
        );
    }
    assert_eq!(
        row("trust").kind,
        ProgramStateKind::Managed {
            build: 6808,
            index: 41
        }
    );
    assert_eq!(
        row("ay").kind,
        ProgramStateKind::Shadowed {
            build: 1971,
            path: "/Users//dev/.local/bin/ay".into()
        }
    );
    assert_eq!(
        row("gh").kind,
        ProgramStateKind::System {
            path: "/opt/homebrew/bin/gh".into(),
            retired: Some("2026-08-27".into())
        }
    );
    assert_eq!(row("codex").kind, ProgramStateKind::AgentInstalling);
    assert_eq!(
        row("claude").kind,
        ProgramStateKind::VendorLatest {
            version: "2.1.280".into(),
            vendor: "Anthropic".into()
        }
    );
    assert_eq!(
        ProgramStateKind::parse(&atpkg::state::vendor_kept("2.1.279", "held by local pin")),
        ProgramStateKind::VendorKept {
            version: "2.1.279".into(),
            why: "held by local pin".into()
        }
    );
    assert_eq!(
        ProgramStateKind::parse(&atpkg::state::shadowed(
            claude,
            p("/Users//dev/.local/bin/claude")
        )),
        ProgramStateKind::Shadowed {
            build: claude,
            path: "/Users//dev/.local/bin/claude".into()
        }
    );
    assert_eq!(row("emacs").kind, ProgramStateKind::Unavailable);
    for retired in ["vendorx", "brew", "ny"] {
        assert_eq!(row(retired).kind, ProgramStateKind::Other, "{retired}");
    }
    for name in ["trust", "ay", "gh", "emacs", "ny", "vendorx"] {
        assert!(row(name).facts.is_none(), "{name} carries no vendor facts");
    }

    assert!(row("codex").facts.is_some(), "codex facts");
    assert!(row("claude").facts.is_some(), "claude facts");
    let odd = VendorFacts::parse("Some Tool without the grammar");
    assert_eq!(odd.vendor, "Some Tool without the grammar");
    assert_eq!(odd.license, None);
    assert_eq!(odd.size, None);
}

#[test]
fn report_merges_dev_links_into_the_program_rows() {
    let links = vec![
        (
            "ay".to_string(),
            Some(std::path::PathBuf::from("/Users//x/ay")),
        ),
        ("orc".to_string(), None),
    ];
    let report =
        PackagesStatusReport::from_parts(true, true, "fp".into(), Some(&status("ok")), &links);
    assert_eq!(report.programs.len(), 2);
    let ay = report.programs.iter().find(|row| row.name == "ay").unwrap();
    assert_eq!(
        ay.installed_build, None,
        "the current link shadows the historical store build"
    );
    assert_eq!(ay.kind, ProgramStateKind::DevLinked);
    assert!(
        ay.annotation
            .as_deref()
            .is_some_and(|a| a.contains("/Users//x/ay"))
    );
    let orc = report
        .programs
        .iter()
        .find(|row| row.name == "orc")
        .unwrap();
    assert_eq!(orc.kind, ProgramStateKind::DevLinkUnavailable);
    assert_eq!(orc.installed_build, None);
    assert!(report.programs.windows(2).all(|w| w[0].name <= w[1].name));
}

#[test]
fn a_current_dev_link_overlays_stale_installing_and_keeps_pass_history() {
    const OWN: &str = "0.1.0-dev.13917+codex.0.165.0.g1aef4a3bf";
    const CLIENT: &str = "0.166.0-dev.13917+g1aef4a3bf";
    let temp = aterm_tempfile::tempdir().unwrap();
    let layout = atpkg::Layout {
        prefix: temp.path().join("pkg"),
    };
    let checkout = temp.path().join("codex");
    let rel =
        std::path::PathBuf::from("bin").join(format!("codex{}", std::env::consts::EXE_SUFFIX));
    let exe = checkout.join(&rel);
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(&exe, b"package bytes, never executed").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::fs::write(
        checkout.join("codex-package.json"),
        format!(
            r#"{{"layoutVersion":1,"entrypoint":"{}","version":"{OWN}"}}"#,
            rel.to_string_lossy().replace('\\', "/")
        ),
    )
    .unwrap();
    std::fs::write(
        checkout.join("alab-build.json"),
        format!(r#"{{"version":"{OWN}","client_version":"{CLIENT}"}}"#),
    )
    .unwrap();
    atpkg::link(&layout, "codex", &checkout, &[rel]).unwrap();
    let mut prior = status("previous pass completed");
    prior.last_success_at = "2026-10-28T07:03:52Z".into();
    prior.programs.insert(
        "codex".into(),
        atpkg::ProgramStatus {
            state: atpkg::state::agent_installing(),
            ..Default::default()
        },
    );
    atpkg::status::write(&layout, &prior).unwrap();
    let project = |target| {
        let mut report = PackagesStatusReport::from_parts(
            true,
            true,
            "fixture".into(),
            Some(&prior),
            &[("codex".into(), target)],
        );
        report.attach_store_facts(&layout, None);
        report
    };
    let report = project(atpkg::linkmode::live_linked_checkout(&layout, "codex").unwrap());
    let row = report
        .programs
        .iter()
        .find(|row| row.name == "codex")
        .unwrap();
    assert_eq!(row.kind, ProgramStateKind::DevLinked);
    assert_eq!(
        row.update.version,
        Some(format!("{OWN} (client {CLIENT}; dev-linked)"))
    );
    assert_eq!(
        row.update.latest_known, None,
        "a signed index pin does not own the link"
    );
    assert_eq!(report.outcome, prior.outcome);
    assert_eq!(report.last_success_at, prior.last_success_at);
    assert_eq!(atpkg::status::read(&layout).unwrap(), prior);

    std::fs::write(checkout.join("alab-build.json"), "not JSON").unwrap();
    let unknown = project(atpkg::linkmode::live_linked_checkout(&layout, "codex").unwrap());
    let row = unknown
        .programs
        .iter()
        .find(|row| row.name == "codex")
        .unwrap();
    assert_eq!(row.kind, ProgramStateKind::DevLinked);
    assert_eq!(row.update.version, None, "invalid metadata stays unknown");
    std::fs::remove_file(&exe).unwrap();
    assert!(atpkg::linkmode::live_linked_checkout(&layout, "codex").is_err());
    let broken = project(None);
    let row = broken
        .programs
        .iter()
        .find(|row| row.name == "codex")
        .unwrap();
    assert_eq!(row.kind, ProgramStateKind::DevLinkUnavailable);
    assert_eq!(row.update.version, None);

    let harness_checkout = temp.path().join("grok-alab");
    let harness_rel =
        std::path::PathBuf::from("bin").join(format!("grok-alab{}", std::env::consts::EXE_SUFFIX));
    let harness_exe = harness_checkout.join(&harness_rel);
    std::fs::create_dir_all(harness_exe.parent().unwrap()).unwrap();
    std::fs::write(&harness_exe, b"harness bytes, never executed").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&harness_exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::fs::write(
        harness_checkout.join(atpkg::harness::LOCAL_BUILD_JSON),
        r#"{"schema":2,"origin":"local-build","program":"grok-alab","command":"grok-alab","version":"0.3.0+g1aef4a3bf"}"#,
    )
    .unwrap();
    atpkg::link(&layout, "grok-alab", &harness_checkout, &[harness_rel]).unwrap();
    let mut harness_report = PackagesStatusReport::from_parts(
        true,
        true,
        "fixture".into(),
        Some(&prior),
        &[(
            "grok-alab".into(),
            atpkg::linkmode::live_linked_checkout(&layout, "grok-alab").unwrap(),
        )],
    );
    harness_report.attach_store_facts(&layout, None);
    let harness_row = harness_report
        .programs
        .iter()
        .find(|row| row.name == "grok-alab")
        .unwrap();
    assert_eq!(harness_row.kind, ProgramStateKind::DevLinked);
    assert_eq!(
        harness_row.update.version.as_deref(),
        Some("0.3.0+g1aef4a3bf (dev-linked)")
    );
}

/// Shadowed is read.
#[cfg(unix)]
#[test]
fn the_collector_reads_shadowing_against_the_path_it_is_handed() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = std::env::temp_dir().join(format!("aterm-packages-shadow-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let layout = atpkg::Layout {
        prefix: root.join("pkg"),
    };
    let build = layout.build_dir("ay", 18);
    std::fs::create_dir_all(build.join("bin")).unwrap();
    std::fs::write(build.join("bin/ay"), b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(build.join("bin/ay"), std::fs::Permissions::from_mode(0o755)).unwrap();
    atpkg::activate::install_shims(
        &layout,
        &build,
        &["ay".to_string()],
        atpkg::activate::Aliases::Off,
    )
    .unwrap();
    atpkg::store::mark_build_ready(&build).unwrap();
    let mut programs = std::collections::BTreeMap::new();
    programs.insert(
        "ay".to_string(),
        atpkg::ProgramStatus {
            installed_build: Some(18),
            state: atpkg::state::managed(18, 41),
            tree_root: String::new(),
        },
    );
    atpkg::status::write(
        &layout,
        &atpkg::Status {
            schema: 1,
            programs,
            ..Default::default()
        },
    )
    .unwrap();
    let foreign = root.join("foreign");
    std::fs::create_dir_all(&foreign).unwrap();
    std::fs::write(foreign.join("ay"), b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(foreign.join("ay"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let row = |path: &std::ffi::OsStr| {
        collect_packages_status_from_layout(true, Some(&layout), Some(path))
            .programs
            .into_iter()
            .find(|r| r.name == "ay")
            .unwrap()
    };
    let ahead = std::env::join_paths([foreign.clone(), layout.bin_dir()]).unwrap();
    assert!(
        matches!(
            row(&ahead).kind,
            ProgramStateKind::Shadowed { build: 18, .. }
        ),
        "{:?}",
        row(&ahead).state
    );
    assert_eq!(row(&ahead).update.fix, None);
    atpkg::activate::install_shims(
        &layout,
        &build,
        &["ay".to_string()],
        atpkg::activate::Aliases::Alab,
    )
    .unwrap();
    let shadowed = row(&ahead);
    assert_eq!(
        shadowed.update.fix.as_deref(),
        Some(atpkg::state::alias_hint("alab-ay").as_str())
    );
    std::fs::write(foreign.join("alab-ay"), b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        foreign.join("alab-ay"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert_eq!(row(&ahead).update.fix, None);
    std::fs::remove_file(foreign.join("alab-ay")).unwrap();
    let behind = std::env::join_paths([layout.bin_dir(), foreign]).unwrap();
    assert!(matches!(
        row(&behind).kind,
        ProgramStateKind::Managed {
            build: 18,
            index: 41
        }
    ));
    assert_eq!(
        atpkg::status::read(&layout).unwrap().programs["ay"].state,
        atpkg::state::managed(18, 41),
        "the record itself is never touched by a read"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn real_collector_turns_status_fifo_into_a_completed_error_report() {
    use std::os::unix::ffi::OsStrExt as _;

    let prefix = std::env::temp_dir().join(format!("aterm-packages-fifo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&prefix);
    std::fs::create_dir_all(&prefix).unwrap();
    let layout = atpkg::Layout {
        prefix: prefix.clone(),
    };
    let status = layout.status();
    let status_c = std::ffi::CString::new(status.as_os_str().as_bytes()).unwrap();
    // SAFETY: `status_c` is a live NUL-terminated path in our private fixture.
    assert_eq!(unsafe { libc::mkfifo(status_c.as_ptr(), 0o600) }, 0);

    let report = collect_packages_status_from_layout(true, Some(&layout), None);
    assert!(
        report
            .collection_error
            .as_deref()
            .is_some_and(|error| error.contains("regular non-link")),
        "the production collector surfaces the checked read's FIFO refusal: {:?}",
        report.collection_error
    );
    let mut service = PackagesService::new();
    let sequence = service.begin(None).unwrap();
    assert!(service.finish(sequence, refresh(report)));
    let projection = service.state(true, true, true).projection();
    assert!(projection.observed && !projection.refreshing);
    let _ = std::fs::remove_dir_all(prefix);
}

fn machine_state(
    uc: atpkg::machine::UcPosture,
    home: atpkg::machine::HomePosture,
) -> atpkg::machine::MachineState {
    atpkg::machine::MachineState {
        universal_control: uc,
        policy: atpkg::config::UniversalControlPolicy::Off,
        home,
        config_unreadable: false,
        rosetta: atpkg::machine::RosettaPosture::Present,
        rosetta_wanted: true,
    }
}

fn live_service() -> PackagesService {
    let mut service = PackagesService::new();
    observe(
        &mut service,
        PackagesStatusReport::from_parts(true, true, "fp".into(), Some(&status("ok")), &[]),
    );
    service
}

#[test]
fn a_passes_record_answers_its_change_without_a_read() {
    use atpkg::machine::{HomePosture, UcPosture};
    let now = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    let mut service = PackagesService::new();
    service.note_machine_change("universal-control disabled".into(), now);
    service.expect_machine_record();
    let waiting = service.state(true, true, true).projection();
    assert!(waiting.machine.refreshing, "the status reads as refreshing");
    assert!(!waiting.machine.apply_enabled);
    assert!(service.machine().awaiting_record && !service.machine().refreshing);
    service.note_machine_record(machine_state(UcPosture::Disabled, HomePosture::Account));
    let m = service.machine();
    assert!(m.observed && !m.awaiting_record && !m.refreshing);
    assert_eq!(
        m.state.as_ref().map(|s| s.universal_control),
        Some(UcPosture::Disabled)
    );
    assert!(!service.take_record_expectation(), "nothing left to answer");

    assert!(service.request_machine_read(), "a read is spawned");
    assert!(
        !service.request_machine_read(),
        "…and a second queues a rerun"
    );
    service.note_machine_record(machine_state(UcPosture::Disabled, HomePosture::Account));
    assert!(service.machine().superseded);
    assert!(
        !service.machine().rerun,
        "the record satisfies the queued rerun"
    );
    let rerun =
        service.replace_machine_state(Ok(machine_state(UcPosture::Default, HomePosture::Account)));
    assert!(!rerun);
    let m = service.machine();
    assert!(!m.refreshing && !m.superseded);
    assert_eq!(
        m.state.as_ref().map(|s| s.universal_control),
        Some(UcPosture::Disabled),
        "the older read must not overwrite the pass's record"
    );

    service.expect_machine_record();
    assert!(service.take_record_expectation());
    assert!(!service.take_record_expectation());
    assert!(!service.machine().awaiting_record);

    assert!(service.request_machine_read(), "a read is running…");
    service.expect_machine_record();
    assert!(
        !service.replace_machine_state(Ok(machine_state(UcPosture::Default, HomePosture::Account)))
    );
    assert!(
        service.machine().awaiting_record,
        "a pre-change read measured the machine the change then changed"
    );
    assert!(
        service.request_machine_read(),
        "…and one started afterwards"
    );
    assert!(
        !service
            .replace_machine_state(Ok(machine_state(UcPosture::Disabled, HomePosture::Account)))
    );
    assert!(!service.machine().awaiting_record, "answers it");
    let seq = service.begin(Some(PackagesBusy::MachineApply)).unwrap();
    service.expect_machine_record();
    let done = succeeded(
        PackagesStatusReport::unobserved(),
        PackagesBusy::MachineApply,
    )
    .with_machine_state(Some(machine_state(
        UcPosture::Disabled,
        HomePosture::Account,
    )));
    assert!(service.finish(seq, done));
    let m = service.machine();
    assert!(!m.awaiting_record);
    assert_eq!(
        m.state.as_ref().map(|s| s.universal_control),
        Some(UcPosture::Disabled)
    );
}

#[test]
fn projection_carries_the_machine_posture() {
    use atpkg::machine::{HomePosture, UcPosture};
    let unobserved = PackagesService::new().state(true, true, true).projection();
    assert!(!unobserved.machine.observed);
    assert!(!unobserved.machine.apply_enabled);
    assert!(!unobserved.machine.nothing_to_apply);
    assert_eq!(unobserved.machine.last_change, None);
    assert_eq!(unobserved.machine.supported, cfg!(target_os = "macos"));

    let mut service = live_service();
    service.set_machine_refreshing(true);
    let before = service.revision();
    let _ =
        service.replace_machine_state(Ok(machine_state(UcPosture::Default, HomePosture::Account)));
    assert!(service.revision() > before, "a read fans out");
    assert!(!service.machine().refreshing, "a read ends the refresh");
    let live = service.state(true, true, true).projection();
    assert!(live.machine.observed);
    assert_eq!(live.machine.apply_enabled, cfg!(target_os = "macos"));

    let now = std::time::SystemTime::now();
    for (uc, home, saved) in [
        (
            UcPosture::Default,
            HomePosture::Account,
            Some(
                "Saved: aterm turns Universal Control off at the next ALab tools check. \
                 Apply Now does it now.",
            ),
        ),
        (UcPosture::Disabled, HomePosture::Account, None),
        (
            UcPosture::Default,
            HomePosture::Mismatch,
            Some("Saved: aterm turns Universal Control off at the next ALab tools check."),
        ),
    ] {
        let _ = service.replace_machine_state(Ok(atpkg::machine::MachineState {
            policy: atpkg::config::UniversalControlPolicy::Leave,
            ..machine_state(uc, home)
        }));
        let off = service
            .state(true, true, true)
            .with_machine_config(atpkg::config::UniversalControlPolicy::Off)
            .projection_at(now);
        assert_eq!(off.machine.saved.as_deref(), saved, "{uc:?} {home:?}");
        assert_eq!(
            saved.is_some_and(|saved| saved.contains("Apply Now")),
            off.machine.next == MACHINE_APPLY_WOULD,
            "Apply Now named exactly where it is drawn ({uc:?} {home:?})"
        );
        let kept_off = service
            .state(false, true, true)
            .with_machine_config(atpkg::config::UniversalControlPolicy::Off)
            .projection_at(now);
        let when = crate::prefs::ApplicationTiming::DailyLaunchApply.clause();
        assert_eq!(
            kept_off.machine.saved.as_deref(),
            saved
                .map(|saved| saved.replace("at the next ALab tools check", when))
                .as_deref(),
            "{uc:?} {home:?} with Keep off"
        );
    }
    let _ =
        service.replace_machine_state(Ok(machine_state(UcPosture::Default, HomePosture::Account)));

    let _ = service.replace_machine_state(Err("atpkg machine printed no state".to_string()));
    let errored = service.state(true, true, true).projection();
    assert!(errored.machine.observed);
    assert!(!errored.machine.apply_enabled);
    assert!(!errored.machine.nothing_to_apply);
    assert!(errored.machine.next.is_empty(), "{}", errored.machine.next);
    assert!(
        service.machine().state.is_some(),
        "a failed read never erases the last measurement"
    );
}

#[test]
fn machine_apply_ignores_the_manager_gate() {
    use atpkg::machine::{HomePosture, UcPosture};
    let manager_off =
        PackagesStatusReport::from_parts(true, false, "fp".into(), Some(&status("ok")), &[]);
    let mut torn =
        PackagesStatusReport::from_parts(true, true, "fp".into(), Some(&status("ok")), &[]);
    torn.collection_error = Some("status.toml: torn record".to_string());
    for (report, why) in [(manager_off, "manager off"), (torn, "collection error")] {
        let mut service = PackagesService::new();
        let seq = service.begin(None).unwrap();
        assert!(service.finish(seq, refresh(report)));
        let _ = service
            .replace_machine_state(Ok(machine_state(UcPosture::Default, HomePosture::Account)));
        let p = service.state(true, true, true).projection();
        assert!(!p.actions_enabled, "{why}: the package verbs are gated");
        assert_eq!(
            p.machine.apply_enabled,
            cfg!(target_os = "macos"),
            "{why}: the machine verb is not"
        );
        assert!(!p.machine.nothing_to_apply, "{why}");
    }
}

#[test]
fn flipped_machine_switch_drives_the_apply_verdict_not_the_stale_record() {
    use atpkg::config::UniversalControlPolicy;
    use atpkg::machine::{HomePosture, UcPosture};
    let mut service = live_service();
    let _ =
        service.replace_machine_state(Ok(machine_state(UcPosture::Default, HomePosture::Account)));
    let b = service
        .state(true, true, true)
        .with_machine_config(UniversalControlPolicy::Leave)
        .projection();
    assert!(!b.machine.apply_enabled);
    assert!(b.machine.nothing_to_apply);
    assert!(
        b.machine.saved.is_some(),
        "the saved switch differs from the record"
    );
    let _ = service.replace_machine_state(Ok(atpkg::machine::MachineState {
        policy: UniversalControlPolicy::Leave,
        ..machine_state(UcPosture::Default, HomePosture::Account)
    }));
    let a = service
        .state(true, true, true)
        .with_machine_config(UniversalControlPolicy::Off)
        .projection();
    assert_eq!(a.machine.apply_enabled, cfg!(target_os = "macos"));
    assert!(!a.machine.nothing_to_apply);
    assert!(
        a.machine.saved.is_some(),
        "the saved switch differs from the record"
    );
}

#[test]
fn a_missing_rosetta_is_said_and_applied_on_its_own() {
    use atpkg::machine::{HomePosture, MachineState, RosettaPosture, UcPosture};
    let mut service = live_service();
    let missing = MachineState {
        rosetta: RosettaPosture::Missing,
        ..machine_state(UcPosture::Disabled, HomePosture::Account)
    };
    let _ = service.replace_machine_state(Ok(missing.clone()));
    let p = service.state(true, true, true).projection().machine;
    assert!(
        p.rosetta
            .as_deref()
            .is_some_and(|line| line.starts_with("Rosetta 2 isn\u{2019}t installed, so")),
        "{:?}",
        p.rosetta
    );
    assert_eq!(p.next, MACHINE_APPLY_WOULD_ROSETTA);
    assert_eq!(p.apply_enabled, cfg!(target_os = "macos"));
    assert!(!p.nothing_to_apply);
    assert!(
        p.saved.is_none(),
        "the Universal Control switch did not move"
    );
    let _ = service.replace_machine_state(Ok(MachineState {
        universal_control: UcPosture::Default,
        ..missing.clone()
    }));
    let both = service.state(true, true, true).projection().machine;
    assert_eq!(both.next, MACHINE_APPLY_WOULD_BOTH);
    let _ = service.replace_machine_state(Ok(missing));
    let off = service
        .state(true, true, true)
        .with_machine_rosetta(false)
        .projection()
        .machine;
    assert!(!off.apply_enabled);
    assert!(off.nothing_to_apply);
    assert!(
        off.rosetta
            .as_deref()
            .is_some_and(|line| line.contains("leaves it so")),
        "{:?}",
        off.rosetta
    );
    let _ =
        service.replace_machine_state(Ok(machine_state(UcPosture::Disabled, HomePosture::Account)));
    let done = service.state(true, true, true).projection().machine;
    assert!(done.rosetta.is_none());
    assert!(done.nothing_to_apply);
    assert_eq!(done.next, MACHINE_NOTHING_TO_APPLY);
}

#[test]
fn a_read_in_flight_when_the_apply_finishes_is_rerun_not_joined() {
    use atpkg::machine::{HomePosture, UcPosture};
    let report =
        || PackagesStatusReport::from_parts(true, true, "fp".into(), Some(&status("ok")), &[]);
    let mut service = live_service();
    assert!(service.request_machine_read(), "idle ⇒ the caller spawns");
    assert!(service.machine().refreshing);
    assert!(!service.machine().rerun);

    let seq = service.begin(Some(PackagesBusy::MachineApply)).unwrap();
    assert!(
        service.finish(
            seq,
            succeeded(report(), PackagesBusy::MachineApply)
                .with_machine_verdict(Some("applied — universal-control disabled".to_string())),
        )
    );
    assert!(
        !service.request_machine_read(),
        "a read in flight is queued, not joined"
    );
    assert!(service.machine().refreshing);
    assert!(service.machine().rerun);

    assert!(
        service.replace_machine_state(Ok(machine_state(UcPosture::Default, HomePosture::Account)))
    );
    assert!(!service.machine().rerun, "handed back exactly once");
    let stale = service.state(true, true, true).projection();
    assert!(stale.machine.observed);
    assert!(stale.machine.refreshing);
    assert!(!stale.machine.apply_enabled);

    assert!(
        !service
            .replace_machine_state(Ok(machine_state(UcPosture::Disabled, HomePosture::Account)))
    );
    let fresh = service.state(true, true, true).projection();
    assert!(!fresh.machine.refreshing);
    assert!(fresh.machine.nothing_to_apply);
    assert!(!fresh.machine.apply_enabled);
}

#[test]
fn a_reported_change_lands_on_the_status_and_asks_for_a_read() {
    use atpkg::machine::{HomePosture, UcPosture};
    let mut service = live_service();
    let _ =
        service.replace_machine_state(Ok(machine_state(UcPosture::Default, HomePosture::Account)));
    let before = service.state(true, true, true).projection().machine;
    assert_eq!(before.last_change, None);

    let at = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000);
    service.note_machine_change("universal-control disabled".to_string(), at);
    let after = service
        .state(true, true, true)
        .projection_at(at + std::time::Duration::from_secs(24))
        .machine;
    assert_ne!(after.last_change, before.last_change);
    assert!(
        after
            .last_change
            .as_deref()
            .is_some_and(|change| change
                .starts_with("Last change: ALab tools turned off Universal Control · ")),
        "{:?}",
        after.last_change
    );
    assert!(
        service.request_machine_read(),
        "the arm's other half: a re-read starts"
    );
}

#[test]
fn an_unreadable_config_is_said_in_the_status_and_disables_apply() {
    use atpkg::config::UniversalControlPolicy::Off;
    use atpkg::machine::{HomePosture, MachineState, UcPosture};
    let mut service = live_service();
    let _ = service.replace_machine_state(Ok(MachineState {
        config_unreadable: true,
        ..machine_state(UcPosture::Default, HomePosture::Account)
    }));
    let projection = service
        .state(true, true, true)
        .with_machine_config(Off)
        .projection()
        .machine;
    let reason = projection.reason.as_deref().expect("a refusal is said");
    assert!(
        reason.contains("aterm.toml"),
        "names the file to fix: {reason}"
    );
    assert!(
        projection.next.is_empty(),
        "nothing is next when nothing may be applied: {projection:?}"
    );
    assert!(
        !projection.apply_enabled,
        "Apply must be off: {projection:?}"
    );
    assert!(
        !projection.nothing_to_apply,
        "a refusal is not the same as being already applied: {projection:?}"
    );
}

#[test]
fn machine_projection_covers_every_posture() {
    use atpkg::config::UniversalControlPolicy::{Leave, Off};
    use atpkg::machine::{HomePosture, MachineState, UcPosture};
    let rows = [
        (
            "default",
            machine_state(UcPosture::Default, HomePosture::Account),
            Off,
            false,
            false,
            true,
        ),
        (
            "disabled: nothing left",
            machine_state(UcPosture::Disabled, HomePosture::Account),
            Off,
            false,
            true,
            false,
        ),
        (
            "home mismatch",
            machine_state(UcPosture::Default, HomePosture::Mismatch),
            Off,
            true,
            false,
            false,
        ),
        (
            "partial",
            machine_state(UcPosture::Partial, HomePosture::Account),
            Off,
            false,
            false,
            true,
        ),
        (
            "unknown",
            machine_state(UcPosture::Unknown, HomePosture::Account),
            Off,
            false,
            false,
            true,
        ),
        (
            "default, left alone",
            MachineState {
                policy: Leave,
                ..machine_state(UcPosture::Default, HomePosture::Account)
            },
            Leave,
            false,
            true,
            false,
        ),
        (
            "home mismatch, already as chosen",
            machine_state(UcPosture::Disabled, HomePosture::Mismatch),
            Off,
            false,
            true,
            false,
        ),
        (
            "home unresolved",
            machine_state(UcPosture::Default, HomePosture::Unresolved),
            Off,
            true,
            false,
            false,
        ),
    ];
    for (name, state, saved, reason, nothing_to_apply, apply_on_macos) in rows {
        let mut service = live_service();
        let _ = service.replace_machine_state(Ok(state));
        let p = service
            .state(true, true, true)
            .with_machine_config(saved)
            .projection();
        assert_eq!(p.machine.reason.is_some(), reason, "{name}");
        assert_eq!(p.machine.nothing_to_apply, nothing_to_apply, "{name}");
        assert_eq!(
            p.machine.apply_enabled,
            cfg!(target_os = "macos") && apply_on_macos,
            "{name}"
        );
        assert!(p.machine.saved.is_none(), "{name}: saved == record");
    }
}

fn incident_reason() -> String {
    INCIDENT_2026_09_14_REASON.to_string()
}

#[test]
fn reason_lines_keeps_a_short_state_unchanged() {
    for state in [
        "managed 1971 — pinned by index 41",
        "",
        "error: stale link. fix: `aterm pkg repair`",
    ] {
        assert_eq!(
            reason_lines(state, 64),
            vec![state.to_string()],
            "{state:?}"
        );
    }
    let edge = "x".repeat(64);
    assert_eq!(reason_lines(&edge, 64), vec![edge.clone()]);
    assert_eq!(reason_lines(&format!("{edge} y"), 64).len(), 2);
}

#[test]
fn reason_lines_puts_the_fix_first_and_whole() {
    let incident = incident_reason();
    assert!(
        incident.chars().count() > 600,
        "{}",
        incident.chars().count()
    );
    let fix_at = incident
        .find(" fix: ")
        .expect("atpkg spells the remedy ` fix: `");
    let fix_clause = &incident[fix_at + " fix: ".len()..];
    for width in [64usize, 104, 144] {
        let lines = reason_lines(&incident, width);
        assert!(
            lines[0].starts_with("fix: unset ATPKG_REFUSE_TRACKED_INSTALL"),
            "{width}: {:?}",
            lines[0]
        );
        for line in &lines {
            assert!(fits(line, width), "{width}: {line:?}");
        }
        let fix_lines: Vec<&str> = lines
            .iter()
            .map(String::as_str)
            .take_while(|line| !line.starts_with("error: stage:") && !line.starts_with('\u{2026}'))
            .collect();
        assert_eq!(
            fix_lines.join(" "),
            format!("fix: {fix_clause}"),
            "{width}: nothing of the fix is cut"
        );
        let last = lines.last().unwrap();
        assert!(last.ends_with(" more)"), "{width}: {last:?}");
        let expected_len = if fix_lines.len() >= MAX_REASON_LINES {
            fix_lines.len() + 1
        } else {
            MAX_REASON_LINES
        };
        assert_eq!(lines.len(), expected_len, "{width}: {lines:#?}");
        if fix_lines.len() < MAX_REASON_LINES {
            assert!(
                lines[fix_lines.len()]
                    .starts_with("error: stage: this process is provenance-tracked"),
                "{width}: the diagnosis head follows the fix: {:?}",
                lines[fix_lines.len()]
            );
        }
    }
}

#[test]
fn reason_lines_wraps_and_bounds_a_reason_without_a_fix() {
    let within = "error: stage: the signed index names build 1971 for ay but the store holds \
                  no such tree and the download step was refused by the policy";
    let wrapped = reason_lines(within, 48);
    assert!(
        wrapped.len() > 1 && wrapped.len() <= MAX_REASON_LINES,
        "{wrapped:#?}"
    );
    assert_eq!(
        wrapped.join(" "),
        within.split_whitespace().collect::<Vec<_>>().join(" ")
    );
    for line in &wrapped {
        assert!(fits(line, 48), "{line:?}");
    }

    let long: String = (0..60)
        .map(|n| format!("word{n:02}"))
        .collect::<Vec<_>>()
        .join(" ");
    let all = wrap_words(&long, 40);
    assert!(all.len() > MAX_REASON_LINES);
    let bounded = reason_lines(&long, 40);
    assert_eq!(bounded.len(), MAX_REASON_LINES);
    assert_eq!(bounded[..MAX_REASON_LINES - 1], all[..MAX_REASON_LINES - 1]);
    let hidden = all.len() - MAX_REASON_LINES;
    let last = &bounded[MAX_REASON_LINES - 1];
    assert!(
        last.ends_with(&format!(" \u{2026}({hidden} more)")),
        "{last:?}"
    );
    assert!(fits(last, 40), "{last:?}");
    assert!(all[MAX_REASON_LINES - 1].starts_with(last.split(" \u{2026}").next().unwrap()));

    let path = format!(
        "/Users//example/Library/Application-Support/aterm/pkg/{}",
        "a".repeat(70)
    );
    let pieces = wrap_words(&path, 40);
    assert!(pieces.len() > 1 && pieces.iter().all(|line| fits(line, 40)));
    assert_eq!(pieces.concat(), path);
}

#[test]
fn a_cjk_reason_wraps_between_its_characters() {
    let reason = format!(
        "{}。{}、atpkg を確認してください。",
        "パッケージの状態を読み込めませんでし", "フォルダーの権限がありません"
    );
    for width in 12..=48 {
        let lines = wrap_words(&reason, width);
        assert!(lines.len() > 1, "{width}: {lines:?}");
        for line in &lines {
            assert!(fits(line, width), "{width}: {line:?}");
            let wide = line
                .chars()
                .filter(|c| aterm_grapheme::char_width(*c) > 1)
                .count();
            assert!(
                2 * wide <= width,
                "{width}: {line:?} holds {wide} wide chars"
            );
        }
        for line in &lines[1..] {
            let first = line.chars().next().expect("a line has a character");
            assert!(
                !['。', '、', 'ッ', 'ー'].contains(&first),
                "{width}: {lines:?}"
            );
        }
        assert_eq!(
            lines.concat().replace(' ', ""),
            reason.replace(' ', ""),
            "{width}: {lines:?}"
        );
    }
    assert_eq!(
        wrap_words("設定 atpkg", 40),
        ["設定 atpkg"],
        "a space between a CJK word and a Latin one stays"
    );
}

#[test]
fn reason_lines_marker_is_the_word_fix() {
    let prefix = format!(
        "error: stage: the prefix: /Users//example/Library/Application Support/aterm/pkg is {}",
        "not writable and the untracked lane could not be reached for the install pass"
    );
    let lines = reason_lines(&prefix, 48);
    assert!(
        lines[0].starts_with("error: stage: the prefix:"),
        "{lines:#?}"
    );
    assert!(
        lines.iter().all(|line| !line.starts_with("fix:")),
        "{lines:#?}"
    );

    let bundle = format!(
        "error: the bundle at /Applications/aterm.app is missing {} (the bundle's fix: {})",
        "Contents/MacOS/aterm",
        "reinstall aterm from its disk image, then `aterm pkg repair` re-lays the shims"
    );
    let lines = reason_lines(&bundle, 64);
    assert!(
        lines[0].starts_with("fix: reinstall aterm from its disk image"),
        "{lines:#?}"
    );

    let glued = format!(
        "error: stage: /tmp/fix:abc is not a store path {}",
        "x".repeat(60)
    );
    let lines = reason_lines(&glued, 48);
    assert!(
        lines[0].starts_with("error: stage: /tmp/fix:abc"),
        "{lines:#?}"
    );
}

const NOW: i64 = 1_790_000_000;
const PDT: i64 = -7 * 3600;

#[test]
fn when_words_are_relative_then_the_local_calendar() {
    let w = |ago: i64, offset: i64| when_words(NOW - ago, NOW, LocalClock::fixed(offset));
    assert_eq!(w(4 * 60, PDT), "4 min ago");
    assert_eq!(w(86_400, PDT), "yesterday 07:13");
    assert_eq!(w(400 * 86_400, PDT), "Aug 17 2025");
    assert_eq!(w(25 * 3600, 0), "yesterday 13:13");
    assert_eq!(w(25 * 3600, 12 * 3600), "yesterday 01:13");
    assert_eq!(w(25 * 3600, -14 * 3600), "Sat 23:13");
    let utc = LocalClock::fixed(0);
    assert_eq!(when_words(NOW + 7200, NOW, utc), "today 16:13");
    assert_eq!(when_words(NOW + 86_400, NOW, utc), "Sep 22 14:13");
    for words in [
        w(86_400, PDT),
        w(400 * 86_400, 0),
        when_words(NOW + 60, NOW, utc),
    ] {
        assert!(!words.contains('T') && !words.contains('Z'), "{words}");
    }
}

#[test]
fn the_local_clock_reads_each_instant_on_its_own_offset() {
    const PST: i64 = -8 * 3600;
    const SWITCH: i64 = 1_793_523_600;
    let now = SWITCH + 36 * 3600;
    let zone = |at: i64| Some(if at < SWITCH { PDT } else { PST });
    let clock = LocalClock::probe(now, zone);
    let (since, before) = clock.changed.expect("the switch is inside the week");
    assert!((SWITCH..SWITCH + 60).contains(&since), "{since}");
    assert_eq!(before, PDT);
    let saturday = 1_793_480_700;
    assert_eq!(when_words(saturday, now, clock), "Sat 14:05");
    assert_eq!(
        when_words(saturday, now, LocalClock::fixed(PST)),
        "Sat 13:05",
        "one offset for every instant is the hour off this replaces"
    );
    assert_eq!(
        when_words(saturday + 20 * 3600, now, clock),
        "yesterday 09:05"
    );
    assert_eq!(
        LocalClock::probe(now + 30 * 86_400, zone),
        LocalClock::fixed(PST)
    );
    assert_eq!(LocalClock::probe(now, |_| None), LocalClock::UNKNOWN);
    assert_eq!(
        when_words(saturday, now, LocalClock::UNKNOWN),
        "Sat 21:05 UTC"
    );
    assert_eq!(
        when_words(now - 120, now, LocalClock::UNKNOWN),
        "2 min ago",
        "a relative time needs no zone"
    );
    if let (Some(then), Some(at_now)) = (
        crate::presence::local_offset_at(now - CLOCK_WINDOW_S),
        crate::presence::local_offset_at(now),
    ) {
        let live = LocalClock::read(now);
        assert_eq!(live.offset_s, Some(at_now));
        assert_eq!(live.changed.is_some(), then != at_now, "{live:?}");
    }
}

fn vendor_id(version: &str) -> u64 {
    atpkg::vendor_direct::Version::parse(version)
        .unwrap()
        .build_id()
}

fn report_of(rows: &[(&str, Option<u64>, String)]) -> PackagesStatusReport {
    let mut status = status("up to date (index build 44)");
    status.last_index_build = 44;
    status.programs = rows
        .iter()
        .map(|(name, build, state)| {
            (
                (*name).to_string(),
                atpkg::ProgramStatus {
                    installed_build: *build,
                    state: state.clone(),
                    tree_root: String::new(),
                },
            )
        })
        .collect();
    PackagesStatusReport::from_parts(true, true, "fp".into(), Some(&status), &[])
}

#[test]
fn the_headline_says_up_to_date_only_when_every_current_row_is() {
    let headline = |ay_latest: &str| {
        let mut report = report_of(&[
            ("ay", Some(1971), atpkg::state::managed(1971, 44)),
            (
                "codex",
                Some(vendor_id("0.156.0")),
                atpkg::state::vendor_kept("0.156.0", "held by local pin"),
            ),
        ]);
        for r in &mut report.programs {
            r.update.latest_known = Some(if r.name == "ay" {
                ay_latest.to_string()
            } else {
                "0.157.0".to_string()
            });
        }
        let mut service = PackagesService::new();
        observe(&mut service, report);
        projection(&service).headline
    };
    assert!(headline("build 1980").contains("available"));
    assert!(
        headline("build 1971").contains("up to date"),
        "a pinned row knowing a newer build is not the page's news"
    );
}

#[test]
fn a_qualified_up_to_date_outcome_is_shown_not_folded() {
    assert!(outcome_is_plainly_current("up to date"));
    assert!(outcome_is_plainly_current("up to date (index build 45)"));
    assert!(outcome_is_plainly_current("up to date (ay build 8256)"));
    let qualified = "up to date (ay build 1971) \u{2014} a clause the headline does not carry";
    assert!(!outcome_is_plainly_current(qualified));
    assert!(!outcome_is_plainly_current("installed ay build 1972"));
    let mut service = PackagesService::new();
    observe(
        &mut service,
        PackagesStatusReport::from_parts(true, true, "fp".into(), Some(&status(qualified)), &[]),
    );
    let detail = projection(&service).detail.unwrap_or_default();
    assert!(
        detail.contains("a clause the headline does not carry"),
        "{detail}"
    );
}

/// What the store says.
#[cfg(unix)]
#[test]
fn the_store_supplies_when_a_build_went_live_and_the_latest_known() {
    let root = std::env::temp_dir().join(format!("aterm-packages-facts-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let layout = atpkg::Layout {
        prefix: root.join("pkg"),
    };
    let version = atpkg::vendor_direct::Version::parse("2.1.280").unwrap();
    let build = layout.build_dir("claude", version.build_id());
    std::fs::create_dir_all(build.join("bin")).unwrap();
    let spec = atpkg::vendor_direct::spec("claude").unwrap();
    let record = atpkg::vendor_direct::VendorRecord {
        schema: 1,
        program: "claude".into(),
        version,
        vendor: spec.vendor.to_string(),
        source_url: format!("{}2.1.280/darwin-arm64/claude", spec.url_prefixes[0]),
        sha256: "a".repeat(64),
        sha512: String::new(),
        size: 1,
        tree_root: "0".repeat(64),
        apple_team: cfg!(target_os = "macos").then(|| spec.apple_team.to_string()),
        anchor: spec.anchor,
        build_date: None,
        verified_at: NOW - 7200,
        measured_sha256: None,
        agreed_with: None,
        tree_from: None,
    };
    std::fs::write(
        atpkg::vendor_direct::record_path(&build).unwrap(),
        aterm_toml::to_string(&record).unwrap(),
    )
    .unwrap();
    atpkg::store::mark_build_ready(&build).unwrap();
    atpkg::vendor_direct::ProgramStamp::record_check(
        &layout,
        "claude",
        NOW - 240,
        atpkg::vendor_direct::Version::parse("2.1.281"),
        None,
    )
    .unwrap();
    let ay = layout.build_dir("ay", 1971);
    std::fs::create_dir_all(&ay).unwrap();
    std::os::unix::fs::symlink(&ay, layout.program_current("ay")).unwrap();

    let mut report = report_of(&[
        (
            "claude",
            Some(version.build_id()),
            atpkg::state::vendor_managed("2.1.280", "Anthropic"),
        ),
        ("ay", Some(1971), atpkg::state::managed(1971, 44)),
    ]);
    let pins: std::collections::BTreeMap<String, u64> =
        [("ay".to_string(), 1980)].into_iter().collect();
    report.attach_store_facts(&layout, Some(&pins));
    let fact = |name: &str| {
        report
            .programs
            .iter()
            .find(|r| r.name == name)
            .unwrap()
            .update
            .clone()
    };
    let claude = fact("claude");
    assert_eq!(claude.version.as_deref(), Some("2.1.280"));
    assert_eq!(claude.source, "Anthropic latest");
    assert_eq!(claude.updated_at, Some(NOW - 7200), "its verification");
    assert_eq!(
        claude.latest_known.as_deref(),
        Some("2.1.281"),
        "the stamp's head"
    );
    assert_eq!(claude.checked_at, Some(NOW - 240));
    let ay = fact("ay");
    assert_eq!(ay.version.as_deref(), Some("build 1971"));
    assert_eq!(ay.source, "ALab index 44");
    assert_eq!(
        ay.latest_known.as_deref(),
        Some("build 1980"),
        "the index pin"
    );
    let flipped = ay.updated_at.expect("the current link's time");
    let wall = aterm_time::unix_secs().unwrap() as i64;
    assert!(
        (wall - 120..=wall + 5).contains(&flipped),
        "{flipped} vs {wall}"
    );
    let mut unpinned = report_of(&[("ay", Some(1971), atpkg::state::managed(1971, 44))]);
    unpinned.attach_store_facts(&layout, None);
    assert_eq!(
        unpinned.programs[0].update.latest_known.as_deref(),
        Some("build 1971")
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// The worker reads the upstream an ALab codex is based on off the record of the store build
/// its shim runs.
#[cfg(unix)]
#[test]
fn the_worker_reads_the_upstream_an_alab_build_records() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = aterm_tempfile::tempdir().expect("scratch folder");
    let layout = atpkg::Layout {
        prefix: root.path().join("pkg"),
    };
    const BUILD: u64 = 2_026_101_601;
    let dir = layout.build_dir("codex", BUILD);
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::write(dir.join("bin/codex"), b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(
        dir.join("bin/codex"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    std::fs::write(
        dir.join(atpkg::alab::PACKAGE_JSON),
        r#"{"layoutVersion":1,"version":"0.164.0-dev.12+gabc","entrypoint":"bin/codex"}"#,
    )
    .unwrap();
    std::fs::write(dir.join(atpkg::alab::ALAB_BUILD_JSON), br#"{"source":"0"}"#).unwrap();
    atpkg::store::mark_build_ready(&dir).unwrap();
    atpkg::activate::activate_build(&layout, &dir).unwrap();
    atpkg::activate::install_shims(
        &layout,
        &dir,
        &["codex".to_string()],
        atpkg::activate::Aliases::Off,
    )
    .unwrap();
    let mut programs = std::collections::BTreeMap::new();
    for (name, build) in [("codex", BUILD), ("ay", 1971)] {
        programs.insert(
            name.to_string(),
            atpkg::ProgramStatus {
                installed_build: Some(build),
                state: atpkg::state::managed(build, 75),
                tree_root: String::new(),
            },
        );
    }
    atpkg::status::write(
        &layout,
        &atpkg::Status {
            schema: 1,
            programs,
            ..Default::default()
        },
    )
    .unwrap();
    let upstream = |name: &str| {
        collect_packages_status_from_layout(true, Some(&layout), None)
            .programs
            .into_iter()
            .find(|r| r.name == name)
            .unwrap()
            .upstream
    };
    assert_eq!(upstream("codex"), None, "a record from before the field");
    let sha = "0123456789abcdef0123456789abcdef01234567";
    let record = dir.join(atpkg::alab::ALAB_BUILD_JSON);
    std::fs::write(
        &record,
        format!(
            r#"{{"source":"0","upstream":{{"release":"rust-v0.163.0","commit":"{sha}","commit_date":"2026-10-15T18:02:11Z"}}}}"#
        ),
    )
    .unwrap();
    assert_eq!(
        upstream("codex").as_deref(),
        Some("based on upstream rust-v0.163.0+g0123456789ab")
    );
    assert_eq!(upstream("ay"), None, "ALab does not build it");
    for bad in [
        r#"{"source":"0","upstream":"rust-v0.163.0"}"#,
        r#"{"source":"0","upstream":{"release":"rust-v0.163.0","commit":"0123"}}"#,
        "{\"upstream\":{",
    ] {
        std::fs::write(&record, bad).unwrap();
        assert_eq!(upstream("codex"), None, "{bad}");
    }

    // A stale vendor head stamp on disk must not override the ALab index pin on an active
    // ALab store build of codex.
    atpkg::vendor_direct::ProgramStamp::record_check(
        &layout,
        "codex",
        NOW - 120,
        atpkg::vendor_direct::Version::parse("0.164.0"),
        None,
    )
    .unwrap();
    let mut report = report_of(&[("codex", Some(BUILD), atpkg::state::managed(BUILD, 75))]);
    let pins: std::collections::BTreeMap<String, u64> =
        [("codex".to_string(), BUILD + 1)].into_iter().collect();
    report.attach_store_facts(&layout, Some(&pins));
    let codex = &report.programs[0].update;
    assert_eq!(
        codex.latest_known.as_deref(),
        Some(atpkg::vendor_direct::build_words(BUILD + 1).as_str()),
        "ALab build uses the index pin, never a stale vendor ProgramStamp"
    );
    assert_eq!(codex.checked_at, None);
}

fn entry(at: i64, kind: &str, pid: u32, fields: &[(&str, &str)]) -> atpkg::packages_log::Entry {
    atpkg::packages_log::Entry {
        at: Some(at),
        kind: kind.to_string(),
        pid,
        fields: fields
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect(),
    }
}

#[test]
fn the_activity_shows_as_many_passes_beside_each_discovery_line() {
    let dir = aterm_tempfile::tempdir().expect("scratch folder");
    let mut log = String::new();
    for pass in 0..40u32 {
        let at = |s: u32| {
            format!(
                "2026-09-21T{:02}:{:02}:{:02}Z",
                pass / 6,
                (pass % 6) * 10,
                s
            )
        };
        let pid = 1000 + pass;
        log.push_str(&format!(
            "{}\tpass-start\tpid={pid}\tlane=window\tverb=update\n",
            at(0)
        ));
        log.push_str(&format!(
            "{}\tindex-resolved\tpid={pid}\theads=3\trounds=1\tgets=0\tfound=44\n",
            at(1)
        ));
        log.push_str(&format!(
            "{}\tpass-end\tpid={pid}\tlane=window\tverb=update\texit=0\tsecs=1\n",
            at(2)
        ));
    }
    std::fs::write(dir.path().join(atpkg::packages_log::LOG_NAME), log).unwrap();
    let entries = atpkg::packages_log::read_tail_of(
        dir.path(),
        atpkg::packages_log::ACTIVITY_EVENTS,
        shown_in_activity,
    );
    let passes = activity_sentences(&entries)
        .iter()
        .filter(|(entry, ..)| entry.kind == atpkg::packages_log::kind::PASS_END)
        .count();
    assert_eq!(passes, 25, "{entries:?}");
}

#[test]
fn the_activity_reads_the_log_newest_first() {
    use atpkg::packages_log::kind;
    let entries = vec![
        entry(
            NOW - 30,
            kind::PASS_START,
            9,
            &[("lane", "session"), ("verb", "update")],
        ),
        entry(
            NOW - 60,
            kind::PASS_END,
            7,
            &[
                ("lane", "window"),
                ("verb", "update"),
                ("exit", "0"),
                ("secs", "7"),
                ("outcome", "up to date (index build 44)"),
            ],
        ),
        entry(
            NOW - 70,
            kind::PROGRAM,
            7,
            &[
                ("program", "claude"),
                ("from", "2.1.278"),
                ("to", "2.1.280"),
                ("source", "Anthropic latest"),
                ("result", "updated"),
            ],
        ),
        entry(
            NOW - 80,
            kind::PROGRAM,
            7,
            &[
                ("program", "codex"),
                ("from", "0.156.0"),
                ("source", "OpenAI latest"),
                ("result", "refused"),
                ("reason", "error: codex 0.157.0 refused: digest mismatch"),
            ],
        ),
        entry(
            NOW - 90,
            kind::PROGRAM,
            7,
            &[
                ("program", "ay"),
                ("from", "build 1971"),
                ("source", "ALab index 44"),
                ("result", "held"),
                ("reason", "held: build 1980 is not published for this Mac"),
            ],
        ),
        entry(
            NOW - 100,
            kind::PASS_START,
            7,
            &[("lane", "window"), ("verb", "update")],
        ),
        entry(
            NOW - 3 * 3600,
            kind::PASS_END,
            5,
            &[
                ("lane", "head watch"),
                ("verb", "update claude"),
                ("exit", "69"),
            ],
        ),
        entry(
            NOW - 86_400,
            kind::PASS_END,
            4,
            &[
                ("lane", "typed"),
                ("verb", "seed"),
                ("exit", "75"),
                ("outcome", "did not run: another pass holds the store"),
            ],
        ),
        entry(
            NOW - 2 * 86_400,
            kind::PASS_END,
            3,
            &[
                ("lane", "agent update"),
                ("verb", "update codex"),
                ("exit", "1"),
                ("secs", "12"),
                ("outcome", "codex: stage failed"),
            ],
        ),
    ];
    let lines = activity_lines(&entries, NOW, LocalClock::fixed(PDT));
    let got: Vec<(&str, bool)> = lines.iter().map(|l| (l.when.as_str(), l.trouble)).collect();
    assert_eq!(
        got,
        vec![
            ("just now", false),
            ("1 min ago", false),
            ("1 min ago", false),
            ("1 min ago", true),
            ("1 min ago", false),
            ("3 h ago", false),
            ("yesterday 07:13", false),
            ("Sat 07:13", true),
        ]
    );
    assert!(lines[2].text.contains("2.1.280"), "{}", lines[2].text);
    assert!(
        lines[3].text.contains("digest mismatch"),
        "{}",
        lines[3].text
    );
    assert!(activity_lines(&[], NOW, LocalClock::fixed(0)).is_empty());
    assert!(
        program_activity(
            "ty",
            Some("build 5"),
            None,
            "ALab index 44",
            "failed",
            "error: stage: disk full"
        )
        .1
    );
}
