// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_failed_or_unmeasured_item_fails_the_run_and_a_person_step_does_not() {
    assert!(Outcome::Failed.is_failure());
    assert!(Outcome::Unknown.is_failure());
    assert!(!Outcome::Person.is_failure());
    assert!(!Outcome::Blocked.is_failure());
    assert!(!Outcome::NotApplicable.is_failure());
    assert!(!Outcome::Deferred.is_failure());
    assert!(!Outcome::Planned.is_failure());
}

#[test]
fn readiness_is_stricter_than_the_failure_policy_and_requires_evidence() {
    assert!(!Readiness::of(&[], &[]).all_green);
    let mut r = row();
    r.essential = false;
    for outcome in [
        Outcome::Ok,
        Outcome::Changed,
        Outcome::NotApplicable,
        Outcome::Planned,
        Outcome::Person,
        Outcome::Blocked,
        Outcome::Deferred,
        Outcome::Unknown,
        Outcome::Failed,
    ] {
        r.outcome = outcome;
        let ready = matches!(
            outcome,
            Outcome::Ok | Outcome::Changed | Outcome::NotApplicable
        );
        let assessment = Readiness::of(std::slice::from_ref(&r), &["base".into()]);
        assert_eq!(assessment.all_green, ready, "{outcome:?}");
        assert_eq!(assessment.scope, ["base"]);
        assert_eq!(assessment.unresolved.is_empty(), ready, "{outcome:?}");
        assert_eq!(r.fails_surface(), outcome == Outcome::Failed);
    }
}

#[test]
fn optional_readiness_round_trips_without_changing_older_reports() {
    let mut report = Report::new(ReportMode::Check, false, vec![row()]);
    let old = aterm_json::to_string(&report).unwrap();
    assert!(!old.contains("readiness"));
    assert_eq!(aterm_json::from_str::<Report>(&old).unwrap(), report);
    report.readiness = Some(Readiness::of(&report.items, &["base".into()]));
    let text = aterm_json::to_string(&report).unwrap();
    assert_eq!(aterm_json::from_str::<Report>(&text).unwrap(), report);
    assert!(text.contains(r#""all_green":false"#));
}

#[test]
fn changed_settings_await_login_or_restart_without_inventing_pending_work_for_ok_rows() {
    let mut r = row();
    for effect in [
        Effect::Now,
        Effect::Relaunch,
        Effect::Login,
        Effect::Restart,
    ] {
        r.effect = effect;
        r.outcome = Outcome::Changed;
        let readiness = Readiness::of(std::slice::from_ref(&r), &[]);
        let pending = matches!(effect, Effect::Login | Effect::Restart);
        assert_eq!(readiness.all_green, !pending, "{effect:?}");
        assert_eq!(readiness.pending_activation.len(), usize::from(pending));
        assert_eq!(readiness.satisfied, usize::from(!pending));
        r.outcome = Outcome::Ok;
        assert!(Readiness::of(std::slice::from_ref(&r), &[]).all_green);
    }
}

fn row() -> ItemReport {
    ItemReport {
        id: "base.clt".into(),
        family: "developer".into(),
        title: "the Command Line Tools".into(),
        outcome: Outcome::Blocked,
        from: Some("absent".into()),
        to: Some("installed".into()),
        why: Some("no network".into()),
        revert: None,
        effect: Effect::Now,
        detail: None,
        layer: Some(Layer::Base),
        class: Some(Class::Elevate),
        explain: Some(Explain {
            why: "no network".into(),
            remedy: Remedy::Init {
                only: "base.clt".into(),
            },
        }),
        stood_aside: Some(StoodAside::Offline("no network".into())),
        waits_on: None,
        essential: true,
        elapsed_ms: Some(12),
    }
}

/// A report written before rows and phases were timed still reads, as untimed (the
/// negative control: an old machines agent's or doctor's report), and a timed one
/// round-trips with every row's time and each phase.
#[test]
fn a_report_written_before_timing_reads_as_untimed_and_a_timed_one_round_trips() {
    let old = r#"{"schema":1,"mode":"check","complete":true,"items":[{"id":"power.sleep","family":"power","title":"sleep","outcome":"ok","effect":"now"}]}"#;
    let report: Report = aterm_json::from_str(old).unwrap();
    assert_eq!(report.items[0].elapsed_ms, None);
    assert!(report.phases.is_empty());
    let text = aterm_json::to_string(&report).unwrap();
    assert!(
        !text.contains("elapsed_ms") && !text.contains("phases"),
        "{text}"
    );

    let mut laps = Laps::start();
    laps.lap("gather");
    laps.lap("packages");
    laps.lap("gather");
    let mut timed = Report::new(ReportMode::Check, false, vec![row()]);
    timed.phases = laps.phases();
    assert_eq!(
        timed.phases.keys().collect::<Vec<_>>(),
        ["gather", "packages"]
    );
    let text = aterm_json::to_string(&timed).unwrap();
    assert!(text.contains(r#""elapsed_ms":12"#), "{text}");
    assert_eq!(aterm_json::from_str::<Report>(&text).unwrap(), timed);
    let line = laps.to_string();
    assert!(
        line.starts_with("gather ") && line.contains(", packages ") && line.contains(", total "),
        "run order, each phase once, then the total: {line}"
    );
}

#[test]
fn a_row_round_trips_through_json_with_every_new_field() {
    let mut rows = vec![row()];
    for remedy in [
        Remedy::Repair,
        Remedy::Develop,
        Remedy::Provision {
            args: "--linux".into(),
        },
        Remedy::Person {
            pane: "Privacy & Security".into(),
            how: "turn on aterm".into(),
        },
        Remedy::Network,
        Remedy::Command("aterm pkg install --default-set".into()),
        Remedy::None,
    ] {
        let mut r = row();
        r.explain = Some(Explain {
            why: "x".into(),
            remedy,
        });
        r.stood_aside = Some(StoodAside::Lock("held".into()));
        rows.push(r);
    }
    for aside in [
        StoodAside::Lock("held".into()),
        StoodAside::Offline("silent".into()),
        StoodAside::Absent("none there".into()),
        StoodAside::Waiting("an admin".into()),
        StoodAside::Permission("refused".into()),
    ] {
        let mut r = row();
        r.stood_aside = Some(aside);
        rows.push(r);
    }
    for r in rows {
        let text = aterm_json::to_string(&r).unwrap();
        assert_eq!(
            aterm_json::from_str::<ItemReport>(&text).unwrap(),
            r,
            "{text}"
        );
    }
}

#[test]
fn a_row_written_before_the_new_fields_still_reads() {
    let old = r#"{"id":"power.sleep","family":"power","title":"sleep","outcome":"changed","effect":"now"}"#;
    let r: ItemReport = aterm_json::from_str(old).unwrap();
    assert_eq!(r.outcome, Outcome::Changed);
    assert_eq!((r.layer, r.class), (None, None));
    assert!(r.explain.is_none() && r.stood_aside.is_none() && !r.essential);
    let text = aterm_json::to_string(&r).unwrap();
    assert!(
        !text.contains("layer") && !text.contains("explain") && !text.contains("essential"),
        "{text}"
    );
}

#[test]
fn only_an_essential_row_left_wrong_fails_its_surface() {
    let mut r = row();
    for (outcome, essential_fails) in [
        (Outcome::Ok, false),
        (Outcome::Changed, false),
        (Outcome::NotApplicable, false),
        (Outcome::Deferred, false),
        (Outcome::Planned, true),
        (Outcome::Person, true),
        (Outcome::Blocked, true),
        (Outcome::Unknown, true),
        (Outcome::Failed, true),
    ] {
        r.outcome = outcome;
        r.essential = true;
        assert_eq!(r.fails_surface(), essential_fails, "essential {outcome:?}");
        r.essential = false;
        assert_eq!(
            r.fails_surface(),
            outcome == Outcome::Failed,
            "advisory {outcome:?}"
        );
    }
}

#[test]
fn a_report_round_trips_in_the_one_schema() {
    let mut advisory = row();
    advisory.essential = false;
    advisory.outcome = Outcome::Planned;
    let report = Report::new(ReportMode::Check, false, vec![row(), advisory]);
    let text = aterm_json::to_string(&report).unwrap();
    assert!(
        text.starts_with(r#"{"schema":1,"mode":"check","complete":false,"items":[{"#),
        "{text}"
    );
    assert_eq!(text.matches(r#""essential":true"#).count(), 1, "{text}");
    assert_eq!(aterm_json::from_str::<Report>(&text).unwrap(), report);
    for mode in [ReportMode::Apply, ReportMode::Plan, ReportMode::Check] {
        let text = aterm_json::to_string(&Report::new(mode, true, Vec::new())).unwrap();
        assert_eq!(aterm_json::from_str::<Report>(&text).unwrap().mode, mode);
    }
}

#[test]
fn standing_aside_ends_with_the_reason_specific_exit_status() {
    assert_eq!(StoodAside::Lock(String::new()).exit_status(), 75);
    assert_eq!(StoodAside::Offline(String::new()).exit_status(), 69);
    assert_eq!(StoodAside::Absent(String::new()).exit_status(), 69);
    assert_eq!(StoodAside::Waiting(String::new()).exit_status(), 75);
    assert_eq!(StoodAside::Permission(String::new()).exit_status(), 77);
}
