// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn head(seq: u64, digest: &str, parents: usize) -> Head {
    Head {
        seq,
        digest: digest.repeat(64 / digest.len()),
        parents,
    }
}

fn s(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_owned()).collect()
}

/// What a run in which every part held would show: each island wrote and admitted during the
/// cut, the heal merged both sides into one head everywhere, and the fleet held on both sides.
fn whole() -> Evidence {
    let merged = head(9, "c", 2);
    let macs = ["a1", "a2", "a3", "b1", "b2", "b3"];
    Evidence {
        publish: Publish::Policy,
        writers: Writers::Both,
        vault: VaultEvidence {
            before: Some(head(5, "0", 1)),
            members_before: s(&["a1", "a2", "b1", "b2"]),
            sets: vec![
                SetTry {
                    name: "f8-a-1".to_owned(),
                    island: Island::A,
                    by: "a1".to_owned(),
                    ok: true,
                    words: String::new(),
                },
                SetTry {
                    name: "f8-b-1".to_owned(),
                    island: Island::B,
                    by: "b1".to_owned(),
                    ok: true,
                    words: String::new(),
                },
            ],
            joins: vec![
                JoinTry {
                    mac: "a3".to_owned(),
                    island: Island::A,
                    posted: true,
                    in_during_cut_ms: Some(1_200),
                },
                JoinTry {
                    mac: "b3".to_owned(),
                    island: Island::B,
                    posted: true,
                    in_during_cut_ms: Some(1_400),
                },
            ],
            end_of_cut: vec![
                IslandView {
                    island: Island::A,
                    server: Some(head(7, "a", 1)),
                    macs: vec![
                        ("a1".to_owned(), Some(head(7, "a", 1))),
                        ("a2".to_owned(), Some(head(7, "a", 1))),
                    ],
                    crossed: Vec::new(),
                },
                IslandView {
                    island: Island::B,
                    server: Some(head(7, "b", 1)),
                    macs: vec![
                        ("b1".to_owned(), Some(head(7, "b", 1))),
                        ("b2".to_owned(), Some(head(7, "b", 1))),
                    ],
                    crossed: Vec::new(),
                },
            ],
            healed: Some((2_500, merged.clone())),
            servers_after: vec![
                (Island::A, Some(merged.clone())),
                (Island::B, Some(merged.clone())),
            ],
            macs_after: macs
                .iter()
                .map(|m| MacAfter {
                    mac: (*m).to_owned(),
                    island: if m.starts_with('a') {
                        Island::A
                    } else {
                        Island::B
                    },
                    membership: "in".to_owned(),
                    head: Some(merged.clone()),
                    at_ms: Some(2_000),
                })
                .collect(),
            entries_after: vec![
                EntryAfter {
                    name: "f8-a-1".to_owned(),
                    island: Island::A,
                    on: s(&macs),
                    missing: Vec::new(),
                },
                EntryAfter {
                    name: "f8-b-1".to_owned(),
                    island: Island::B,
                    on: s(&macs),
                    missing: Vec::new(),
                },
            ],
            runners: Vec::new(),
        },
        fleet: FleetEvidence {
            surveys: vec![
                SurveyRead {
                    phase: Phase::Before,
                    from: "a1".to_owned(),
                    answered: s(&["a1", "a2", "b1", "b2"]),
                    silent: Vec::new(),
                },
                SurveyRead {
                    phase: Phase::EarlyCut,
                    from: "a1".to_owned(),
                    answered: s(&["a1", "a2"]),
                    silent: Vec::new(),
                },
                SurveyRead {
                    phase: Phase::EarlyCut,
                    from: "b1".to_owned(),
                    answered: s(&["b1", "b2"]),
                    silent: Vec::new(),
                },
                SurveyRead {
                    phase: Phase::Healed,
                    from: "b1".to_owned(),
                    answered: s(&["a1", "a2", "b1", "b2", "b3"]),
                    silent: Vec::new(),
                },
            ],
            drives: vec![
                DriveTry {
                    phase: Phase::EarlyCut,
                    from: "a1".to_owned(),
                    to: "b1".to_owned(),
                    ok: false,
                    words: "no route to host".to_owned(),
                    copy_age_s: Some(4),
                },
                DriveTry {
                    phase: Phase::LateCut,
                    from: "b1".to_owned(),
                    to: "b2".to_owned(),
                    ok: true,
                    words: String::new(),
                    copy_age_s: Some(63),
                },
                DriveTry {
                    phase: Phase::Healed,
                    from: "b1".to_owned(),
                    to: "b2".to_owned(),
                    ok: true,
                    words: String::new(),
                    copy_age_s: Some(1),
                },
            ],
            joins: vec![FleetJoin {
                phase: Phase::EarlyCut,
                ok: true,
                words: String::new(),
            }],
            heartbeats: vec![Heartbeat {
                mac: "b1".to_owned(),
                island: Island::B,
                advanced_in_cut: false,
                back_ms: Some(4_800),
            }],
            late_skipped: None,
            whole_ms: Some(2_000),
            first_read_ms: Some(1_500),
        },
        commons: None,
        accounts: None,
        removals: None,
        load: Vec::new(),
        cut_ms: 80_000,
    }
}

fn failed(ev: &Evidence) -> Vec<&'static str> {
    judge(ev)
        .into_iter()
        .filter(|c| !c.pass)
        .map(|c| c.name)
        .collect()
}

#[test]
fn a_run_where_every_part_held_passes_every_check() {
    let checks = judge(&whole());
    assert!(checks.len() >= 9, "{checks:#?}");
    assert_eq!(failed(&whole()), Vec::<&str>::new(), "{checks:#?}");
}

#[test]
fn the_one_url_publish_never_converges_and_fails_one_head() {
    // F8's negative control: island B's server never takes island A's head.
    let mut ev = whole();
    ev.publish = Publish::OneUrl;
    ev.vault.healed = None;
    ev.vault.servers_after[1].1 = Some(head(7, "b", 1));
    for m in ev
        .vault
        .macs_after
        .iter_mut()
        .filter(|m| m.island == Island::B)
    {
        m.head = Some(head(7, "b", 1));
    }
    let checks = judge(&ev);
    let one = checks.iter().find(|c| c.name == ONE_HEAD).unwrap();
    assert!(!one.pass);
    assert!(one.detail.contains("server B"), "{}", one.detail);
    assert!(one.detail.contains("b3"), "{}", one.detail);
}

#[test]
fn an_entry_a_merge_dropped_is_a_loss() {
    let mut ev = whole();
    ev.vault.entries_after[1].on = s(&["b1"]);
    ev.vault.entries_after[1].missing = s(&["a1", "a2", "a3", "b2", "b3"]);
    assert_eq!(
        failed(&ev),
        vec!["vault: nothing set or admitted during the cut was lost"]
    );
}

#[test]
fn a_set_never_looked_for_after_the_heal_is_a_loss_not_a_pass() {
    let mut ev = whole();
    ev.vault.entries_after.pop();
    assert_eq!(
        failed(&ev),
        vec!["vault: nothing set or admitted during the cut was lost"]
    );
}

#[test]
fn a_mac_admitted_in_the_cut_and_out_after_the_heal_is_a_loss() {
    let mut ev = whole();
    let b3 = ev
        .vault
        .macs_after
        .iter_mut()
        .find(|m| m.mac == "b3")
        .unwrap();
    b3.membership = "pending".to_owned();
    let got = failed(&ev);
    assert!(
        got.contains(&"vault: nothing set or admitted during the cut was lost"),
        "{got:?}"
    );
    assert!(
        got.contains(&"vault: every Mac that asked during the cut is in after the heal"),
        "{got:?}"
    );
}

#[test]
fn an_island_that_could_not_write_or_admit_did_not_keep_working() {
    let mut ev = whole();
    ev.writers = Writers::A;
    ev.vault.sets[1].ok = false;
    ev.vault.sets[1].words = "b1 is no admin".to_owned();
    ev.vault.joins[1].in_during_cut_ms = None;
    let checks = judge(&ev);
    let kept = checks
        .iter()
        .find(|c| c.name == "vault: each island kept writing and admitting")
        .unwrap();
    assert!(!kept.pass);
    assert!(
        kept.detail.contains("b1 could not set f8-b-1"),
        "{}",
        kept.detail
    );
    assert!(kept.detail.contains("b3 asked"), "{}", kept.detail);
}

#[test]
fn a_member_holding_the_other_islands_head_is_a_leaked_cut() {
    let mut ev = whole();
    ev.vault.end_of_cut[1].macs[1].1 = Some(head(7, "a", 1));
    assert!(failed(&ev).contains(&CUT_HELD), "{:?}", failed(&ev));
    let mut ev = whole();
    ev.vault.end_of_cut[1].server = Some(head(7, "a", 1));
    for m in &mut ev.vault.end_of_cut[1].macs {
        m.1 = Some(head(7, "a", 1));
    }
    assert!(failed(&ev).contains(&CUT_HELD), "{:?}", failed(&ev));
}

#[test]
fn a_survey_or_drive_across_the_cut_is_a_leaked_cut() {
    let mut ev = whole();
    ev.fleet.surveys[2].answered.push("a2".to_owned());
    assert_eq!(failed(&ev), vec![FLEET_CUT_HELD]);
    let mut ev = whole();
    ev.fleet.drives[0].ok = true;
    assert_eq!(failed(&ev), vec![FLEET_CUT_HELD]);
}

#[test]
fn a_drive_refused_once_the_copy_aged_out_fails_the_island_that_lost_its_issuer() {
    let mut ev = whole();
    ev.fleet.drives[1].ok = false;
    ev.fleet.drives[1].words = "the driver is not in this machine's fleet book".to_owned();
    assert_eq!(
        failed(&ev),
        vec!["fleet: each island drove its own members for the whole cut"]
    );
    let mut ev = whole();
    ev.fleet.drives.retain(|d| d.phase != Phase::LateCut);
    ev.fleet.late_skipped = Some("--hold 30s is under the window".to_owned());
    assert_eq!(
        failed(&ev),
        vec!["fleet: each island drove its own members for the whole cut"]
    );
}

#[test]
fn a_fleet_that_stays_split_after_the_heal_is_not_whole() {
    let mut ev = whole();
    ev.fleet.heartbeats[0].back_ms = None;
    assert_eq!(failed(&ev), vec!["fleet: whole again after the heal"]);
    let mut ev = whole();
    ev.fleet.surveys[3].answered.retain(|m| m != "a2");
    ev.fleet.surveys[3]
        .silent
        .push("a2: 127.0.0.1:41000 did not answer: timed out".to_owned());
    assert_eq!(failed(&ev), vec!["fleet: whole again after the heal"]);
    let whole_again = judge(&ev)
        .into_iter()
        .find(|c| c.name == "fleet: whole again after the heal")
        .unwrap();
    assert!(
        whole_again
            .detail
            .contains("b1 did not hear a2 (a2: 127.0.0.1:41000 did not answer: timed out)"),
        "{}",
        whole_again.detail
    );
    let mut ev = whole();
    ev.fleet.joins[0].ok = false;
    let got = failed(&ev);
    assert!(
        got.contains(&"fleet: whole again after the heal"),
        "{got:?}"
    );
    assert!(
        got.contains(&"fleet: b3 joined on island B during the cut"),
        "{got:?}"
    );
}

#[test]
fn parse_reads_each_arm_and_refuses_what_it_does_not_know() {
    let args = |words: &[&str]| s(words);
    let opts = parse(&args(&[
        "--one-url",
        "--writers",
        "a",
        "--hold",
        "90",
        "--record",
    ]))
    .unwrap();
    assert_eq!(opts.publish, Publish::OneUrl);
    assert_eq!(opts.writers, Writers::A);
    assert_eq!(opts.hold, Duration::from_secs(90));
    assert!(opts.record && !opts.keep);
    let plain = parse(&[]).unwrap();
    assert_eq!(
        (plain.publish, plain.writers, plain.hold),
        (Publish::Policy, Writers::Both, DEFAULT_HOLD)
    );
    for bad in [
        &["--hold", "5"][..],
        &["--hold", "x"],
        &["--writers", "b"],
        &["--aterm"],
        &["--frobnicate"],
    ] {
        assert_eq!(parse(&args(bad)).unwrap_err().code, 2, "{bad:?}");
    }
}

#[test]
fn one_head_that_splits_again_before_the_runs_end_fails_one_head() {
    let mut ev = whole();
    ev.vault.servers_after[0].1 = Some(head(10, "d", 1));
    let checks = judge(&ev);
    let one = checks.iter().find(|c| c.name == ONE_HEAD).unwrap();
    assert!(!one.pass);
    assert!(
        one.detail.contains("split again: server A"),
        "{}",
        one.detail
    );
}

#[test]
fn a_member_before_the_cut_that_the_heal_left_out_fails_one_head_and_is_lost() {
    // b2 reads the merged head but cannot open it: the heal dropped its locker.
    let mut ev = whole();
    let b2 = ev
        .vault
        .macs_after
        .iter_mut()
        .find(|m| m.mac == "b2")
        .unwrap();
    b2.membership = "revoked".to_owned();
    let got = failed(&ev);
    assert!(got.contains(&ONE_HEAD), "{got:?}");
    assert!(
        got.contains(&"vault: nothing set or admitted during the cut was lost"),
        "{got:?}"
    );
    let lost = judge(&ev)
        .into_iter()
        .find(|c| c.name == "vault: nothing set or admitted during the cut was lost")
        .unwrap();
    assert!(
        lost.detail
            .contains("b2 was a member before the cut and is revoked after the heal"),
        "{}",
        lost.detail
    );
}

#[test]
fn a_value_that_crossed_the_cut_is_a_leak_though_the_servers_differ() {
    let mut ev = whole();
    ev.vault.end_of_cut[1]
        .crossed
        .push("b2 held f8-a-1, which island A set during the cut".to_owned());
    assert!(failed(&ev).contains(&CUT_HELD), "{:?}", failed(&ev));
}

#[test]
fn b3_joined_and_unheard_after_the_heal_is_not_whole() {
    let mut ev = whole();
    let healed = ev
        .fleet
        .surveys
        .iter_mut()
        .find(|s| s.phase == Phase::Healed)
        .unwrap();
    healed.answered.retain(|m| m != "b3");
    healed.silent.push("b3: not in the survey".to_owned());
    let whole_again = judge(&ev)
        .into_iter()
        .find(|c| c.name == "fleet: whole again after the heal")
        .unwrap();
    assert!(!whole_again.pass);
    assert!(
        whole_again.detail.contains("did not hear b3"),
        "{}",
        whole_again.detail
    );
}
