// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_containment::consent::{
    DebuggerMember, DebuggerMode, DebuggerProbe, DebuggerProbeLabel, DevToolList, DevToolListLabel,
    DevToolProbe, DevToolProbeLabel, DrClass, FdaProbe, ProbeGate, ProbeOutcome, SelfHardening,
};
use aterm_types::MutexExt as _;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Mutex, OnceLock};

use super::*;
use crate::consent_state::{AxProbe, ChangeKind, ConsentProbes};
const FIXTURE_DEBUGGER: DebuggerRow = DebuggerRow {
    probe: DebuggerProbe {
        mode: DebuggerMode::Disabled,
        member: DebuggerMember::Admin,
        label: DebuggerProbeLabel::StatusOk,
    },
    self_hardened: SelfHardening::Hardened,
    probe_age_ms: Some(1204),
};

const FIXTURE_DEVTOOLS: DevToolsRow = DevToolsRow {
    probe: DevToolProbe {
        state: aterm_containment::DevToolState::Unlisted,
        label: DevToolProbeLabel::Observed,
        requests: Some(233),
        not_allowed: Some(233),
        answered_at_ms: Some(1_791_381_501_820),
    },
    list: DevToolList {
        listing: aterm_containment::DevToolListing::Absent,
        label: DevToolListLabel::Read,
        modified_at_s: None,
        rows: Some(1),
    },
    subject: Some("com.aterm.aterm"),
    probe_age_ms: Some(812),
    answer_age_ms: Some(4100),
};

fn snapshot(sessions: usize, fda: FdaState, evidence: SpikeEvidence) -> PrivacySnapshot {
    snapshot_with_warmup(sessions, fda, evidence, &[])
}

fn fixture_identity() -> &'static SigningIdentity {
    static IDENTITY: OnceLock<SigningIdentity> = OnceLock::new();
    IDENTITY.get_or_init(|| SigningIdentity {
        bundle_id: Some("com.aterm.aterm".to_string()),
        reset_subject: None,
        display_name: Some("aterm".to_string()),
        signing: "developer-id",
        team: Some("A66A9P66Z7".to_string()),
        dr: DrClass::Identity,
        dev_build: Some(false),
    })
}

fn snapshot_with_warmup(
    sessions: usize,
    fda: FdaState,
    evidence: SpikeEvidence,
    warmup: &[(Folder, crate::consent_warmup::WarmupRow)],
) -> PrivacySnapshot {
    let split = covers_split(fda, evidence);
    let folders: Vec<(&'static str, &'static str)> = folder_names()
        .into_iter()
        .map(|name| (name, folder_state(name, warmup, &split.covers)))
        .collect();
    let folder_source = folder_source(&folders);
    PrivacySnapshot {
        folders,
        folder_source,
        claimants: None,
        census_pending: false,
        trash_tool: true,
        platform: "macos",
        os: Some("26.6.2"),
        identity: fixture_identity(),
        install: "installed",
        running: Some("/Applications/aterm.app/Contents/MacOS/aterm".to_string()),
        anchor: aterm_containment::ImageAnchor::Live,
        fda,
        probe: ProbeLabel::OpenEperm,
        probe_age_ms: Some(1840),
        evidence,
        split,
        attribution_root: Attribution::Live,
        sessions: (0..sessions)
            .map(|i| SessionRow {
                sid: format!("s-{i}"),
                attribution: if i == 1 {
                    Attribution::Adopted
                } else {
                    Attribution::Live
                },
                responsible_pid: Some(4711),
                responsible: Responsible::SelfProcess,
                fs_consent: FsConsent::Unknown,
                cwd: Some("/Users//a b/src".to_string()),
            })
            .collect(),
        containment_mode: "user".to_string(),
        seatbelt: "none",
        protected: vec!["~/one".to_string(), "~/two".to_string()],
        warmup: "on-request",
        warmup_last_ms: None,
        debugger: FIXTURE_DEBUGGER,
        devtools: FIXTURE_DEVTOOLS,
        accessibility: AxProbe::Read { trusted: false },
        program_check: Some(crate::vitals_jobs::HeldCount {
            held: 0,
            oldest_ms: 0,
            stranded: Some(0),
        }),
        observer_fda: "ok",
        observer_responsible: "ok",
        observer_log: "off",
        access_card: AccessCardReport {
            state: "quiet",
            record: crate::consent_card::AccessRecord::default(),
            needs: 0,
        },
    }
}

#[test]
fn a_headless_app_wires_the_inert_consent_probe_arm() {
    let app = crate::App::headless_for_test();
    let (lines, pending) = app.privacy_report_for_test(PrivacyForm::Lines, Scope::Owner);
    let row = |prefix: &str| {
        lines
            .iter()
            .find(|l| l.starts_with(prefix))
            .unwrap_or_else(|| panic!("no {prefix} row: {lines:?}"))
            .clone()
    };
    let fda = row("full_disk_access=");
    assert!(
        fda.starts_with("full_disk_access=unknown probe=refused_disabled "),
        "{fda}"
    );
    assert!(row("observer ").contains(" responsible=off "));
    for prefix in ["debugger ", "devtools "] {
        let line = row(prefix);
        assert!(line.contains(" probe=refused_disabled "), "{line}");
    }
    assert_eq!(
        row("accessibility "),
        "accessibility grant=unknown probe=refused_disabled"
    );
    assert!(!pending);
}

#[test]
fn an_edge_sees_only_the_sessions_it_may_read() {
    use aterm_session::{EdgeToken, Op, SessionId};
    let mut app = crate::App::headless_for_test();
    let sibling = app.pool.insert(crate::stub_session(1));
    let grant = |app: &crate::App, op: Op| {
        let ctx = &app.pool.get(sibling).expect("the sibling").ctx;
        let mut table = ctx.edges.lock().unwrap_or_else(|p| p.into_inner());
        table.grant(SessionId::new("s-src"), ctx.self_id.clone(), op, ctx.nonce)
    };
    let read = Scope::Edge(grant(&app, Op::ReadScreen));
    let write = Scope::Edge(grant(&app, Op::WriteInput));
    let sibling_sid = app
        .pool
        .get(sibling)
        .expect("sid")
        .ctx
        .self_id
        .as_str()
        .to_string();
    let rows = |scope: Scope| -> Vec<String> {
        let lines = app.privacy_report_for_test(PrivacyForm::Lines, scope).0;
        assert!(
            lines.iter().any(|l| l.starts_with("full_disk_access=")),
            "the instance-wide posture answers every scope: {lines:?}"
        );
        lines
            .into_iter()
            .filter(|l| l.starts_with("session "))
            .collect()
    };
    assert_eq!(rows(Scope::Owner).len(), 2, "Owner sees every session");
    let seen = rows(read);
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert!(
        seen[0].starts_with(&format!("session sid={sibling_sid} ")),
        "{seen:?}"
    );
    assert!(rows(write).is_empty(), "a write edge reads no session");
    assert!(rows(Scope::Edge(EdgeToken::generate())).is_empty());
    let json = app
        .privacy_report_for_test(PrivacyForm::Json, read)
        .0
        .concat();
    assert_eq!(json.matches("\"sid\":").count(), 1, "{json}");
    assert!(json.contains(&sibling_sid), "{json}");
}

#[test]
fn off_macos_the_report_claims_no_consent_layer_and_keeps_its_shape() {
    let mac = snapshot(1, FdaState::Unknown, SpikeEvidence::UNMEASURED);
    let mut linux = snapshot(1, FdaState::Unknown, SpikeEvidence::UNMEASURED);
    linux.platform = "linux";
    let (mac_lines, linux_lines) = (mac.lines(), linux.lines());
    assert_eq!(mac_lines.len(), linux_lines.len(), "the same rows");
    assert!(
        mac_lines.contains(&"prompt_possible=yes".to_string()),
        "control"
    );
    assert!(
        linux_lines.contains(&"prompt_possible=no".to_string()),
        "{linux_lines:?}"
    );
    assert!(
        linux_lines
            .contains(&"remediate fda=- files=- debugger=- devtools=- accessibility=-".to_string()),
        "{linux_lines:?}"
    );
    let containment = |lines: &[String]| {
        lines
            .iter()
            .find(|l| l.starts_with("containment "))
            .cloned()
            .unwrap()
    };
    assert!(
        containment(&linux_lines).ends_with(" protected=-"),
        "{linux_lines:?}"
    );
    assert!(
        !containment(&mac_lines).ends_with(" protected=-"),
        "control"
    );
    assert_eq!(
        linux_lines.last().unwrap(),
        &format!("note {}", pct_encode(NOTE_NO_OS_CONSENT))
    );
    let json = cmd_privacy_json(&linux);
    assert!(json.contains("\"prompt_possible\":false"), "{json}");
    assert!(json.contains("\"remediate\":{\"fda\":null"), "{json}");
    assert!(json.contains("\"protected\":[]"), "{json}");
}

#[test]
fn every_live_session_gets_one_row_with_its_cwd_pct_encoded() {
    for count in [0, 1, 40] {
        let lines = snapshot(count, FdaState::Unknown, SpikeEvidence::UNMEASURED).lines();
        let rows: Vec<&String> = lines.iter().filter(|l| l.starts_with("session ")).collect();
        assert_eq!(rows.len(), count);
        assert!(
            lines
                .iter()
                .any(|l| l.contains(&format!(" sessions_total={count} "))),
            "{lines:?}"
        );
        if let Some(row) = rows.first() {
            assert!(row.ends_with(" cwd=/Users//a%20b/src"), "{row}");
        }
    }
}

/// The devtools line ends with how many of this user's programs macOS's program check holds
/// now, the longest held and how many are stranded: `0` with no oldest when none is held, no
/// stranded count when none was judged, `-` for all three when nothing was read. The JSON's
/// `devtools` object carries the same three values, `null` for `-`.
#[test]
fn the_devtools_line_counts_the_live_wait() {
    use crate::vitals_jobs::HeldCount;
    let mut snap = snapshot(0, FdaState::Granted, SpikeEvidence::MEASURED);
    let count = |held, oldest_ms, stranded| {
        Some(HeldCount {
            held,
            oldest_ms,
            stranded,
        })
    };
    for (held, end, json) in [
        (
            count(2, 35_940_000, Some(1)),
            " program_check=2 program_check_oldest_ms=35940000 program_check_stranded=1",
            (Some(2), Some(35_940_000), Some(1)),
        ),
        (
            count(2, 35_940_000, None),
            " program_check=2 program_check_oldest_ms=35940000 program_check_stranded=-",
            (Some(2), Some(35_940_000), None),
        ),
        (
            count(0, 0, Some(0)),
            " program_check=0 program_check_oldest_ms=- program_check_stranded=0",
            (Some(0), None, Some(0)),
        ),
        (
            None,
            " program_check=- program_check_oldest_ms=- program_check_stranded=-",
            (None, None, None),
        ),
    ] {
        snap.program_check = held;
        let line = snap
            .lines()
            .into_iter()
            .find(|l| l.starts_with("devtools "))
            .expect("a devtools row");
        assert!(line.ends_with(end), "{held:?}: {line}");
        let body = cmd_privacy_json(&snap);
        let parsed: aterm_json::Value =
            aterm_json::from_str(body.lines().nth(1).expect("a body line")).expect("JSON");
        let devtools = &parsed["devtools"];
        assert_eq!(
            (
                devtools["program_check"].as_u64(),
                devtools["program_check_oldest_ms"].as_u64(),
                devtools["program_check_stranded"].as_u64()
            ),
            json,
            "{held:?}: {devtools:?}"
        );
        for key in [
            "program_check",
            "program_check_oldest_ms",
            "program_check_stranded",
        ] {
            assert!(devtools.get(key).is_some(), "{key} is always present");
        }
    }
}

#[test]
fn the_line_body_is_the_documented_shape() {
    let keys = |line: &String| -> String {
        line.split(' ')
            .enumerate()
            .filter_map(|(i, field)| match field.split_once('=') {
                Some((key, _)) => Some(key),
                None => (i == 0).then_some(field),
            })
            .collect::<Vec<_>>()
            .join(" ")
    };
    let mut snap = snapshot(2, FdaState::Denied, SpikeEvidence::UNMEASURED);
    let mut got: Vec<String> = snap.lines().iter().map(keys).collect();
    let folder = std::iter::once("folder")
        .chain(folder_names())
        .chain(["source"])
        .collect::<Vec<_>>()
        .join(" ");
    let session = "session sid attribution responsible_pid responsible fs_consent cwd";
    let mut want: Vec<String> = [
        "schema",
        "platform os",
        "bundle_id display_name signing team dr grant_stable dev_build",
        "install running anchor",
        "full_disk_access probe probe_age_ms fda_scope",
        "covers",
        "uncovered",
        "unmeasured",
        &folder,
        "claimants conflicting census",
        "prompt_possible",
        "attribution_root sessions_total sessions_adopted",
        session,
        session,
        "containment mode seatbelt protected",
        "warmup warmup_last_ms",
        "debugger mode member self_hardened right probe probe_age_ms pending pending_right \
         pending_age_ms last watch_ms",
        "devtools state subject requests not_allowed answer_age_ms window_s probe \
         probe_age_ms grant list list_probe program_check program_check_oldest_ms \
         program_check_stranded",
        "accessibility grant probe",
        "observer fda responsible log taskport",
        "access_card state asked_at opened_at verified_at renew_at needs",
        "remediate fda files debugger devtools accessibility",
        "note",
    ]
    .map(String::from)
    .to_vec();
    got.sort_unstable();
    want.sort_unstable();
    assert_eq!(got, want);

    let summary = |snap: &PrivacySnapshot| {
        snap.lines()
            .into_iter()
            .find(|l| l.starts_with("claimants="))
            .expect("a census row")
    };
    assert_eq!(summary(&snap), "claimants=- conflicting=- census=off");
    snap.census_pending = true;
    assert_eq!(summary(&snap), "claimants=- conflicting=- census=pending");
    snap.claimants = Some((
        census_answer(vec![claimant(
            "/Applications/aterm.app",
            DrClass::Identity,
            "developer-id",
            true,
        )]),
        Duration::ZERO,
    ));
    assert_eq!(
        summary(&snap),
        "claimants=1 conflicting=0 census=complete sole=yes age_ms=0"
    );
    let lines = snap.lines();
    assert!(
        !lines.iter().any(|l| l.starts_with("claimant ")),
        "{lines:?}"
    );
    assert_eq!(
        lines.iter().filter(|l| l.starts_with("note ")).count(),
        1,
        "only the closing note: {lines:?}"
    );
}

fn census_answer(found: Vec<aterm_containment::consent::Claimant>) -> Claimants {
    Claimants {
        found,
        enumeration: aterm_containment::consent::Enumeration::Complete,
    }
}

fn claimant(
    path: &str,
    dr: DrClass,
    signing: &'static str,
    running: bool,
) -> aterm_containment::consent::Claimant {
    aterm_containment::consent::Claimant {
        path: std::path::PathBuf::from(path),
        dr,
        dr_text: format!("designated => {} {path}", dr.as_str()),
        signing,
        team: None,
        running,
        links: Vec::new(),
    }
}

#[test]
fn the_census_rows_offer_the_trash_only_for_what_the_move_takes() {
    let mut snap = snapshot(1, FdaState::Granted, SpikeEvidence::UNMEASURED);
    let mut linked = claimant("/Users//a/dev/aterm.app", DrClass::Cdhash, "adhoc", false);
    linked.links = vec![PathBuf::from("/Applications/aterm-dev.app")];
    let census = census_answer(vec![
        claimant(
            "/Applications/aterm.app",
            DrClass::Identity,
            "developer-id",
            true,
        ),
        claimant(
            "/Applications/aterm.app.rollback",
            DrClass::Cdhash,
            "adhoc",
            false,
        ),
        linked,
        claimant(
            "/Users//a/pinned/aterm.app",
            DrClass::Unknown,
            "adhoc",
            false,
        ),
    ]);
    let moved: Vec<PathBuf> = census.retirable().iter().map(|c| c.path.clone()).collect();
    assert_eq!(moved, [PathBuf::from("/Applications/aterm.app.rollback")]);
    snap.claimants = Some((census, Duration::from_millis(1200)));
    let offered = |snap: &PrivacySnapshot| -> Vec<PathBuf> {
        let lines = snap.lines();
        assert!(
            !lines
                .iter()
                .any(|l| l.starts_with("claimant path=/Applications/aterm.app ")),
            "the running copy is the reference: {lines:?}"
        );
        let text: Vec<PathBuf> = lines
            .iter()
            .filter_map(|line| line.strip_prefix("claimant path="))
            .filter_map(|row| {
                let (path, fields) = row.split_once(' ').expect("fields");
                let trash = fields
                    .split(' ')
                    .find_map(|field| field.strip_prefix("trash="))
                    .expect("every row says whether the Trash takes it");
                (trash == "offered").then(|| PathBuf::from(path))
            })
            .collect();
        let json: aterm_json::Value =
            aterm_json::from_str(cmd_privacy_json(snap).lines().nth(1).expect("JSON body"))
                .expect("the JSON parses");
        let json: Vec<PathBuf> = json["census"]["conflicts"]
            .as_array()
            .expect("an answered census lists its conflicts")
            .iter()
            .filter_map(|row| {
                let trash = row["trash"]
                    .as_str()
                    .expect("every conflict says whether the Trash takes it");
                (trash == "offered").then(|| PathBuf::from(row["path"].as_str().expect("path")))
            })
            .collect();
        assert_eq!(text, json, "both forms read one rule");
        text
    };
    assert_eq!(offered(&snap), moved, "only what the move takes is offered");
    snap.trash_tool = false;
    assert_eq!(offered(&snap), Vec::<PathBuf>::new(), "no tool, no offer");
}

#[test]
fn the_json_census_parses_in_both_states_and_is_capped() {
    let keys = [
        "state",
        "claimants",
        "conflicting",
        "sole",
        "age_ms",
        "conflicts",
        "truncated",
    ];
    let snap = snapshot(1, FdaState::Granted, SpikeEvidence::UNMEASURED);
    let json: aterm_json::Value =
        aterm_json::from_str(cmd_privacy_json(&snap).lines().nth(1).expect("JSON body"))
            .expect("pending json parses");
    for key in keys {
        assert!(
            json["census"].get(key).is_some(),
            "{key} in the unanswered form"
        );
    }
    assert_eq!(json["census"]["state"].as_str(), Some("off"));

    let mut snap = snapshot(1, FdaState::Granted, SpikeEvidence::UNMEASURED);
    let mut found = vec![claimant(
        "/A/aterm.app",
        DrClass::Identity,
        "developer-id",
        true,
    )];
    for n in 0..(aterm_containment::consent::MAX_CLAIMANT_ROWS + 3) {
        found.push(claimant(
            &format!("/A/copy-{n}.app"),
            DrClass::Cdhash,
            "adhoc",
            false,
        ));
    }
    snap.claimants = Some((census_answer(found), Duration::from_millis(7)));
    let json: aterm_json::Value =
        aterm_json::from_str(cmd_privacy_json(&snap).lines().nth(1).expect("JSON body"))
            .expect("answered json parses");
    for key in keys {
        assert!(
            json["census"].get(key).is_some(),
            "{key} in the answered form"
        );
    }
    assert_eq!(
        json["census"]["conflicts"].as_array().map(Vec::len),
        Some(aterm_containment::consent::MAX_CLAIMANT_ROWS)
    );
    assert_eq!(json["census"]["truncated"].as_u64(), Some(3));
    assert_eq!(json["census"]["conflicting"].as_u64(), Some(11));
}

#[test]
fn no_unmeasured_folder_state_is_ever_inferred_from_the_grant() {
    const STILL_UNMEASURED: &[&str] = &[
        "documents",
        "desktop",
        "downloads",
        "network-volumes",
        "removable-volumes",
    ];
    for fda in [FdaState::Granted, FdaState::Denied, FdaState::Unknown] {
        let lines = snapshot(1, fda, SpikeEvidence::UNMEASURED).lines();
        let folder = lines
            .iter()
            .find(|l| l.starts_with("folder "))
            .expect("a folder row");
        for name in STILL_UNMEASURED {
            assert!(
                folder.contains(&format!("{name}=unknown")),
                "{name} has no measurement of its own: {folder}"
            );
        }
        let expected = if fda == FdaState::Granted {
            "app-data=covered-by-fda"
        } else {
            "app-data=unknown"
        };
        assert!(folder.contains(expected), "{fda:?}: {folder}");
        let expected_source = if fda == FdaState::Granted {
            " source=fda"
        } else {
            " source=none"
        };
        assert!(folder.ends_with(expected_source), "{fda:?}: {folder}");
    }
}

#[test]
fn an_observed_warmup_row_outranks_the_coverage_rule_and_reaches_the_wire() {
    use crate::consent_warmup::WarmupRow;
    let warmed = [
        (Folder::Documents, WarmupRow::Denied),
        (Folder::Desktop, WarmupRow::Allowed),
        (Folder::AppData, WarmupRow::Denied),
    ];
    let lines =
        snapshot_with_warmup(1, FdaState::Granted, SpikeEvidence::UNMEASURED, &warmed).lines();
    let folder = lines
        .iter()
        .find(|l| l.starts_with("folder "))
        .expect("a folder row");
    assert!(folder.contains("documents=denied"), "{folder}");
    assert!(folder.contains("desktop=allowed"), "{folder}");
    assert!(folder.contains("app-data=denied"), "{folder}");
    assert!(folder.contains("downloads=unknown"), "{folder}");
    assert!(folder.ends_with(" source=warmup"), "{folder}");

    let asking = [(Folder::Documents, WarmupRow::Asking)];
    let lines =
        snapshot_with_warmup(1, FdaState::Denied, SpikeEvidence::UNMEASURED, &asking).lines();
    let folder = lines
        .iter()
        .find(|l| l.starts_with("folder "))
        .expect("a folder row");
    assert!(folder.contains("documents=asking"), "{folder}");
    assert!(folder.ends_with(" source=warmup"), "{folder}");
}

fn fake_debugger(runs: &std::sync::atomic::AtomicUsize) -> DebuggerProbe {
    runs.fetch_add(1, Ordering::AcqRel);
    DebuggerProbe {
        mode: DebuggerMode::Disabled,
        member: DebuggerMember::Admin,
        label: DebuggerProbeLabel::StatusOk,
    }
}

fn fake_devtools(runs: &std::sync::atomic::AtomicUsize, at_ms: i64) -> DevToolProbe {
    runs.fetch_add(1, Ordering::AcqRel);
    DevToolProbe {
        state: aterm_containment::DevToolState::Unlisted,
        label: DevToolProbeLabel::Observed,
        requests: Some(3),
        not_allowed: Some(3),
        answered_at_ms: Some(at_ms),
    }
}

fn fake_subject() -> Option<&'static str> {
    Some("com.example.subject")
}

#[test]
fn the_privacy_facts_hop_makes_no_os_call() {
    use std::sync::atomic::AtomicUsize;
    static FDA: AtomicUsize = AtomicUsize::new(0);
    static RESPONSIBLE: AtomicUsize = AtomicUsize::new(0);
    static CENSUS: AtomicUsize = AtomicUsize::new(0);
    static DEBUGGER: AtomicUsize = AtomicUsize::new(0);
    static HARDENING: AtomicUsize = AtomicUsize::new(0);
    static DEVTOOLS: AtomicUsize = AtomicUsize::new(0);
    static LIST: AtomicUsize = AtomicUsize::new(0);
    static ACCESSIBILITY: AtomicUsize = AtomicUsize::new(0);
    fn fda(_gate: ProbeGate) -> FdaProbe {
        FDA.fetch_add(1, Ordering::AcqRel);
        consent::classify_probe(ProbeOutcome::Ok)
    }
    fn responsible(pids: &[i32]) -> Vec<Result<i32, ResponsibleError>> {
        RESPONSIBLE.fetch_add(1, Ordering::AcqRel);
        pids.iter().map(|&pid| Ok(pid)).collect()
    }
    fn census(_bundle_id: &str) -> Claimants {
        CENSUS.fetch_add(1, Ordering::AcqRel);
        Claimants::default()
    }
    fn debugger(_gate: ProbeGate) -> DebuggerProbe {
        fake_debugger(&DEBUGGER)
    }
    fn hardening() -> SelfHardening {
        HARDENING.fetch_add(1, Ordering::AcqRel);
        SelfHardening::Hardened
    }
    fn devtools(_gate: ProbeGate, _subject: &str) -> DevToolProbe {
        fake_devtools(&DEVTOOLS, 0)
    }
    fn list(_gate: ProbeGate, _subject: &str) -> DevToolList {
        LIST.fetch_add(1, Ordering::AcqRel);
        DevToolList::refused(DevToolListLabel::RefusedDisabled)
    }
    fn accessibility(_gate: ProbeGate) -> AxProbe {
        ACCESSIBILITY.fetch_add(1, Ordering::AcqRel);
        AxProbe::Read { trusted: true }
    }
    let counts = || {
        [
            &FDA,
            &RESPONSIBLE,
            &CENSUS,
            &DEBUGGER,
            &HARDENING,
            &DEVTOOLS,
            &LIST,
        ]
        .map(|runs| runs.load(Ordering::Acquire))
    };
    let mut app = crate::App::headless_for_test();
    *app.consent.probes_for_test() = ConsentProbes {
        fda,
        responsible,
        census,
        debugger,
        hardening,
        devtools,
        devtools_list: list,
        accessibility,
        subject: fake_subject,
        warm_subject: fake_subject,
        live: true,
    };
    assert!(
        app.consent.policy().gate.permits() && app.consent_policy().gate.permits(),
        "the gate is open, so only the hop can keep the arms quiet"
    );

    let facts = app.privacy_facts(Scope::Owner);
    assert_eq!(facts.sessions.len(), 1, "the hop read the pool");
    assert_eq!(counts(), [0; 7], "the hop made an OS call");
    assert_eq!(
        ACCESSIBILITY.load(Ordering::Acquire),
        0,
        "the hop read the Accessibility grant"
    );

    let identity = SigningIdentity {
        bundle_id: Some("com.example.subject".to_string()),
        ..SigningIdentity::unknown().clone()
    };
    let consent = &facts.consent;
    let seen = consent.publication();
    let slots = consent.report_slots(&facts.policy, &identity);
    assert_eq!(
        slots.pending_kinds(),
        [
            ChangeKind::Fda,
            ChangeKind::Census,
            ChangeKind::Debugger,
            ChangeKind::Devtools
        ],
        "every slot dispatched its worker"
    );
    assert_eq!(facts.responsible_answers().len(), 1);
    for kind in slots.pending_kinds() {
        assert!(
            consent.wait_for_change(seen, &[kind], Duration::from_secs(60)),
            "the {kind:?} worker publishes"
        );
    }
    assert!(counts().iter().all(|&runs| runs >= 1), "{:?}", counts());
}

#[test]
fn the_report_asks_the_spi_before_it_reads_a_slot() {
    use std::sync::atomic::AtomicUsize;
    static SLOT_READS: AtomicUsize = AtomicUsize::new(0);
    static READS_AT_THE_SPI: AtomicUsize = AtomicUsize::new(usize::MAX);
    fn responsible(pids: &[i32]) -> Vec<Result<i32, ResponsibleError>> {
        READS_AT_THE_SPI.store(SLOT_READS.load(Ordering::Acquire), Ordering::Release);
        pids.iter().map(|&pid| Ok(pid)).collect()
    }
    fn subject() -> Option<&'static str> {
        SLOT_READS.fetch_add(1, Ordering::AcqRel);
        None
    }
    let mut app = crate::App::headless_for_test();
    app.consent.probes_for_test().live = true;
    app.consent.probes_for_test().responsible = responsible;
    app.consent.probes_for_test().subject = subject;
    let facts = app.privacy_facts(Scope::Owner);
    assert_eq!(facts.sessions.len(), 1, "a session to ask about");
    let snapshot = facts.report(SigningIdentity::unknown());
    assert_eq!(
        READS_AT_THE_SPI.load(Ordering::Acquire),
        0,
        "the SPI was asked after a slot read"
    );
    assert!(
        SLOT_READS.load(Ordering::Acquire) >= 1,
        "the report read the slots"
    );
    assert!(
        snapshot.sessions[0].responsible_pid.is_some(),
        "the SPI's answer is joined"
    );
}

#[test]
fn the_report_asks_the_spi_once_for_every_session() {
    static ASKED: Mutex<Vec<Vec<i32>>> = Mutex::new(Vec::new());
    fn responsible(pids: &[i32]) -> Vec<Result<i32, ResponsibleError>> {
        ASKED.lock_or_recover().push(pids.to_vec());
        pids.iter().map(|&pid| Ok(pid + 1000)).collect()
    }
    let mut app = crate::App::headless_for_test();
    app.consent.probes_for_test().responsible = responsible;
    let with_sessions = |mut facts: PrivacyAppFacts| {
        let first = facts.sessions[0].clone();
        for (sid, pid) in [("s-a", 7001), ("s-b", 7002)] {
            facts.sessions.push(SessionFacts {
                sid: sid.to_string(),
                pid,
                ..first.clone()
            });
        }
        facts
    };

    let facts = with_sessions(app.privacy_facts(Scope::Owner));
    assert!(facts.policy.report_attribution);
    let pids: Vec<i32> = facts.sessions.iter().map(|s| s.pid).collect();
    let snapshot = facts.report(SigningIdentity::unknown());
    assert_eq!(
        *ASKED.lock_or_recover(),
        std::slice::from_ref(&pids),
        "one call, every pid, in session order"
    );
    let joined: Vec<Option<i32>> = snapshot
        .sessions
        .iter()
        .map(|s| s.responsible_pid)
        .collect();
    let want: Vec<Option<i32>> = pids.iter().map(|&pid| Some(pid + 1000)).collect();
    assert_eq!(joined, want, "each answer is joined to its own session");
    assert_eq!(snapshot.observer_responsible, "ok");

    let mut facts = with_sessions(app.privacy_facts(Scope::Owner));
    facts.policy.report_attribution = false;
    let snapshot = facts.report(SigningIdentity::unknown());
    assert_eq!(ASKED.lock_or_recover().len(), 1, "the SPI was asked again");
    assert_eq!(snapshot.observer_responsible, "off");
    assert!(
        snapshot
            .sessions
            .iter()
            .all(|s| s.responsible_pid.is_none())
    );
}

#[test]
fn an_unvalidatable_identity_is_reported_and_does_not_hide_behind_a_granted_probe() {
    use aterm_containment::ImageAnchor;
    for (anchor, notes) in [
        (ImageAnchor::Live, 1),
        (ImageAnchor::NotBundled, 1),
        (ImageAnchor::Unknown, 1),
        (ImageAnchor::Displaced, 2),
        (ImageAnchor::Deleted, 2),
    ] {
        let mut snap = snapshot(1, FdaState::Granted, SpikeEvidence::UNMEASURED);
        snap.anchor = anchor;
        let lines = snap.lines();
        assert!(
            lines
                .iter()
                .any(|l| l.ends_with(&format!(" anchor={}", anchor.as_str()))),
            "{anchor:?}: {lines:?}"
        );
        assert_eq!(
            lines.iter().filter(|l| l.starts_with("note ")).count(),
            notes,
            "{anchor:?}: {lines:?}"
        );
    }
}

#[test]
fn the_remediate_row_names_only_places() {
    let lines = snapshot(1, FdaState::Denied, SpikeEvidence::UNMEASURED).lines();
    let remediate = lines
        .iter()
        .find_map(|l| l.strip_prefix("remediate "))
        .expect("a remediate row");
    for field in remediate.split(' ') {
        let (_, place) = field.split_once('=').expect("k=v");
        assert!(
            place.starts_with("settings:") || place.starts_with("aterm-settings:"),
            "{field}"
        );
    }
    assert_eq!(
        lines.last().expect("a note row").split(' ').count(),
        2,
        "one field after `note`"
    );
}

#[test]
fn unavailable_is_distinct_from_off_and_from_a_negative_answer() {
    for (label, want) in [
        (ProbeLabel::RefusedDisabled, "off"),
        (ProbeLabel::RefusedOutOfBundle, "unavailable"),
        (ProbeLabel::RefusedNoHome, "unavailable"),
        (ProbeLabel::UnsupportedPlatform, "unavailable"),
        (ProbeLabel::OpenOk, "ok"),
        (ProbeLabel::OpenEperm, "ok"),
        (ProbeLabel::OpenErrno(13), "ok"),
    ] {
        assert_eq!(observer_fda_value(label), want, "{label:?}");
    }
    for (answers, want) in [
        (&[Err(ResponsibleError::Unsupported)][..], "off"),
        (
            &[Err(ResponsibleError::SymbolUnavailable)][..],
            "unavailable",
        ),
        (&[Err(ResponsibleError::NotOurs), Ok(1)][..], "ok"),
        (&[][..], "off"),
    ] {
        assert_eq!(observer_responsible_value(answers), want, "{answers:?}");
    }
}

#[test]
fn the_access_card_row_reports_where_the_question_stands() {
    use crate::consent_card::{AccessRecord, Marker, RENEW_AFTER, report_state};
    let asked = AccessRecord {
        asked_at: Some(1_000),
        ..AccessRecord::default()
    };
    let week = 1_000 + RENEW_AFTER.as_secs();
    let mut snap = snapshot(0, FdaState::Denied, SpikeEvidence::UNMEASURED);
    snap.access_card = AccessCardReport {
        state: "asked",
        record: asked,
        needs: 3,
    };
    let row = snap
        .lines()
        .into_iter()
        .find(|l| l.starts_with("access_card "))
        .expect("an access_card row");
    let renew = format!("renew_at={week}");
    for field in [renew.as_str(), "verified_at=-", "needs=3"] {
        assert!(row.split(' ').any(|f| f == field), "{field}: {row}");
    }
    for (marker, record, fda, now, want) in [
        (None, AccessRecord::default(), FdaState::Denied, 5, "quiet"),
        (
            Some(Marker::NotNow),
            asked,
            FdaState::Granted,
            5,
            "declined",
        ),
        (None, asked, FdaState::Granted, 5, "granted"),
        (None, asked, FdaState::Denied, 1_500, "asked"),
        (None, asked, FdaState::Denied, week, "quiet"),
    ] {
        assert_eq!(report_state(marker, &record, fda, now), want, "{want}");
    }
}

#[test]
fn the_json_form_carries_the_documented_sub_objects() {
    let parse = |snap: &PrivacySnapshot| -> aterm_json::Value {
        let body = cmd_privacy_json(snap);
        assert!(body.starts_with("OK 1\n"), "{body}");
        assert_eq!(body.lines().count(), 2, "a JSON reply is one body line");
        aterm_json::from_str(body.lines().nth(1).expect("a body line")).expect("it parses")
    };
    let observed = snapshot(2, FdaState::Denied, SpikeEvidence::UNMEASURED);
    let json = parse(&observed);
    for key in [
        "folders",
        "debugger",
        "devtools",
        "accessibility",
        "observers",
        "remediate",
    ] {
        assert!(json[key].is_object(), "{key}: {json:?}");
    }
    assert_eq!(json["sessions"].as_array().map(Vec::len), Some(2));
    assert_eq!(json["sessions"][0]["cwd"].as_str(), Some("/Users//a b/src"));
    assert_eq!(json["debugger"]["probe_age_ms"].as_u64(), Some(1204));
    assert_eq!(json["devtools"]["answer_age_ms"].as_u64(), Some(4100));

    let mut unobserved = observed.clone();
    unobserved.debugger = DebuggerRow::awaiting(SelfHardening::Unknown);
    unobserved.devtools = DevToolsRow::refused(DevToolProbeLabel::Pending, None);
    let pending = parse(&unobserved);
    for row in ["debugger", "devtools"] {
        let keys = |v: &aterm_json::Value| -> Vec<String> {
            v[row]
                .as_object()
                .expect("an object")
                .keys()
                .cloned()
                .collect()
        };
        assert_eq!(keys(&pending), keys(&json), "{row}: no key dropped");
        assert!(pending[row]["probe_age_ms"].is_null(), "{row}");
    }
    assert!(pending["devtools"]["subject"].is_null());
}

/// The `accessibility` row and the JSON's `accessibility` object say what one read of aterm's
/// own Accessibility grant answered, in the `devtools` row's `grant=` words, and `remediate`
/// names the Accessibility pane wherever there is one: granted, not granted, not read (a
/// headless instance, `[privacy]` off, no executable, or one outside a `.app`), and off macOS,
/// where the pane is `-` like the rest.
#[test]
fn the_accessibility_row_says_what_the_read_answered_and_names_its_pane() {
    const PANE: Option<&str> = Some("settings:Privacy_Accessibility");
    for (platform, probe, grant, label, pane) in [
        (
            "macos",
            AxProbe::Read { trusted: true },
            "granted",
            "read",
            PANE,
        ),
        (
            "macos",
            AxProbe::Read { trusted: false },
            "not_granted",
            "read",
            PANE,
        ),
        (
            "macos",
            AxProbe::RefusedDisabled,
            "unknown",
            "refused_disabled",
            PANE,
        ),
        (
            "macos",
            AxProbe::RefusedNoExe,
            "unknown",
            "refused_no_exe",
            PANE,
        ),
        (
            "macos",
            AxProbe::RefusedOutOfBundle,
            "unknown",
            "refused_out_of_bundle",
            PANE,
        ),
        (
            "linux",
            AxProbe::UnsupportedPlatform,
            "unknown",
            "unsupported_platform",
            None,
        ),
    ] {
        let mut snap = snapshot(1, FdaState::Denied, SpikeEvidence::UNMEASURED);
        snap.platform = platform;
        snap.accessibility = probe;
        let lines = snap.lines();
        let rows: Vec<&String> = lines
            .iter()
            .filter(|l| l.starts_with("accessibility "))
            .collect();
        assert_eq!(
            rows,
            [&format!("accessibility grant={grant} probe={label}")],
            "{probe:?}"
        );
        let remediate = lines
            .iter()
            .find_map(|l| l.strip_prefix("remediate "))
            .expect("a remediate row");
        assert!(
            remediate.ends_with(&format!(" accessibility={}", pane.unwrap_or("-"))),
            "{probe:?}: {remediate}"
        );
        let json: aterm_json::Value =
            aterm_json::from_str(cmd_privacy_json(&snap).lines().nth(1).expect("JSON body"))
                .expect("the JSON parses");
        assert_eq!(json["accessibility"]["grant"].as_str(), Some(grant));
        assert_eq!(json["accessibility"]["probe"].as_str(), Some(label));
        assert_eq!(json["remediate"]["accessibility"].as_str(), pane);
        assert!(
            json["remediate"].get("accessibility").is_some(),
            "{probe:?}: the key stays when the pane is null"
        );
    }
}

/// The report's `accessibility` row is the consent arm's answer: read once per report, after
/// the main-thread hop and never in it, and not read at all while `[privacy] check` is off.
/// The negative control: the same App with the arm's answer flipped reports the other grant,
/// so a row that ignored the arm fails here.
#[test]
fn the_accessibility_row_is_the_injected_arm_s_answer() {
    use std::cell::Cell;
    thread_local! {
        static TRUSTED: Cell<bool> = const { Cell::new(false) };
        static READS: Cell<usize> = const { Cell::new(0) };
    }
    fn accessibility(_gate: ProbeGate) -> AxProbe {
        READS.with(|reads| reads.set(reads.get() + 1));
        AxProbe::Read {
            trusted: TRUSTED.with(Cell::get),
        }
    }
    let mut app = crate::App::headless_for_test();
    app.consent.probes_for_test().accessibility = accessibility;
    let report = |app: &crate::App| -> (String, aterm_json::Value) {
        let before = READS.with(Cell::get);
        let facts = app.privacy_facts(Scope::Owner);
        assert_eq!(READS.with(Cell::get), before, "the hop read the grant");
        let snapshot = facts.report(SigningIdentity::unknown());
        let read = READS.with(Cell::get) - before;
        let row = snapshot
            .lines()
            .into_iter()
            .find(|l| l.starts_with("accessibility "))
            .expect("an accessibility row");
        let json: aterm_json::Value = aterm_json::from_str(
            cmd_privacy_json(&snapshot)
                .lines()
                .nth(1)
                .expect("JSON body"),
        )
        .expect("the JSON parses");
        assert!(read <= 1, "one read per report, not {read}");
        (format!("{row} reads={read}"), json["accessibility"].clone())
    };

    for (trusted, grant) in [(true, "granted"), (false, "not_granted")] {
        TRUSTED.with(|cell| cell.set(trusted));
        let (row, json) = report(&app);
        assert_eq!(
            row,
            format!("accessibility grant={grant} probe=read reads=1"),
            "the arm said {trusted}"
        );
        assert_eq!(json["grant"].as_str(), Some(grant), "{json:?}");
        assert_eq!(json["probe"].as_str(), Some("read"), "{json:?}");
    }

    TRUSTED.with(|cell| cell.set(true));
    app.set_privacy_for_test(crate::app_config::PrivacyConfig {
        check: Some(false),
        ..Default::default()
    });
    let (row, json) = report(&app);
    assert_eq!(
        row, "accessibility grant=unknown probe=refused_disabled reads=0",
        "`[privacy] check = false` reads nothing"
    );
    assert_eq!(json["grant"].as_str(), Some("unknown"), "{json:?}");
}
