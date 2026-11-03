// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::swarm::partition::split::tests as ledgers;

#[test]
fn an_in_process_join_post_obeys_the_cut_file_and_its_heal() {
    use std::io::{self, BufReader, Write as _};
    use std::net::TcpListener;
    use std::os::fd::AsFd as _;

    let dir = aterm_tempfile::tempdir().expect("private cut file");
    let cut_file = dir.path().join("cut");
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("loopback");
    listener.set_nonblocking(true).expect("nonblocking");
    let address = listener.local_addr().expect("listener address");
    let url = format!("http://{address}/v1/lab");
    let recipient = aterm_secrets::age::Identity::from_secret_bytes([1; 32])
        .recipient()
        .to_bech32();
    let line = format!("{recipient} joining abcd-ef23-ghij-k234");
    let post = || aterm_secrets::transport::post_join(&url, &line).map_err(|e| e.to_string());

    std::fs::write(&cut_file, format!("{address}\n")).expect("cut");
    in_partition(&cut_file, post).expect_err("the other island is unreachable");
    assert_eq!(
        listener
            .accept()
            .expect_err("no connection crossed the cut")
            .kind(),
        io::ErrorKind::WouldBlock
    );

    std::fs::write(&cut_file, "").expect("heal");
    std::thread::scope(|scope| {
        let server = scope.spawn(|| {
            aterm_uds::ready::until(
                listener.as_fd(),
                aterm_uds::ready::Interest::Read,
                Instant::now() + HANG,
            )
            .expect("a healed post must arrive");
            let (mut stream, _) = aterm_uds::accept(&listener).expect("healed connection");
            stream
                .set_read_timeout(Some(HANG))
                .expect("read hang detector");
            stream
                .set_write_timeout(Some(HANG))
                .expect("write hang detector");
            let request = crate::stub::read_request(&mut BufReader::new(&mut stream))
                .expect("join request")
                .expect("one post");
            assert_eq!(request.method, "POST");
            assert_eq!(request.target, "/v1/lab/join");
            assert_eq!(request.body, format!("{line}\n").as_bytes());
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .expect("accepted");
        });
        in_partition(&cut_file, post).expect("the same post reaches the healed island");
        server.join().expect("server");
    });
}

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

/// The fleet Macs going into the cut: a4 joined at a2, b4 at b2.
const FLEET: [&str; 6] = ["a1", "a2", "a4", "b1", "b2", "b4"];

/// The fleet after the heal: those, and b3, which joined at b1 during the cut.
const AFTER: [&str; 7] = ["a1", "a2", "a4", "b1", "b2", "b4", "b3"];

fn survey(phase: Phase, from: &str, answered: &[&str]) -> SurveyRead {
    SurveyRead {
        phase,
        from: from.to_owned(),
        answered: s(answered),
        silent: Vec::new(),
    }
}

fn drive(phase: Phase, from: &str, to: &str, ok: bool) -> DriveTry {
    DriveTry {
        phase,
        from: from.to_owned(),
        to: to.to_owned(),
        ok,
        words: if ok {
            String::new()
        } else {
            "the driver is not in this machine's fleet book".to_owned()
        },
        copy_age_s: Some(1),
    }
}

/// Every member to every other of `macs`, each admitted.
fn every_pair(macs: &[&str]) -> Vec<DriveTry> {
    macs.iter()
        .flat_map(|from| {
            macs.iter()
                .filter(move |to| *to != from)
                .map(move |to| drive(Phase::End, from, to, true))
        })
        .collect()
}

/// Every one of `macs` reached every other.
fn every_reach(macs: &[&str]) -> Vec<Reach> {
    macs.iter()
        .map(|mac| Reach {
            mac: (*mac).to_owned(),
            reached: s(&macs
                .iter()
                .copied()
                .filter(|m| m != mac)
                .collect::<Vec<_>>()),
            dark: Vec::new(),
        })
        .collect()
}

/// A fleet in which every part held: each Mac heard every other before the cut, each island
/// alone during it, and after the heal each member heard, drove and reached every other.
fn whole_fleet() -> FleetEvidence {
    let mut surveys: Vec<SurveyRead> = FLEET
        .iter()
        .map(|m| survey(Phase::Before, m, &FLEET))
        .collect();
    surveys.push(survey(Phase::EarlyCut, "a1", &["a1", "a2", "a4"]));
    surveys.push(survey(Phase::EarlyCut, "b1", &["b1", "b2", "b4"]));
    surveys.extend(AFTER.iter().map(|m| survey(Phase::Healed, m, &AFTER)));
    let mut drives = vec![
        DriveTry {
            words: "no route to host".to_owned(),
            ..drive(Phase::EarlyCut, "a1", "b1", false)
        },
        DriveTry {
            copy_age_s: Some(63),
            ..drive(Phase::LateCut, "b1", "b2", true)
        },
        drive(Phase::LateCut, "b1", "b4", true),
        drive(Phase::Healed, "b1", "b2", true),
    ];
    drives.extend(every_pair(&AFTER));
    FleetEvidence {
        before_members: s(&FLEET),
        away: Vec::new(),
        surveys,
        drives,
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
        surveys_ms: Some(13_000),
        whole_ms: Some(2_000),
        first_read_ms: Some(1_500),
        reach_before: every_reach(&FLEET),
        reach: every_reach(&AFTER),
        reach_ms: Some(12_000),
    }
}

/// The survey `from` read in `phase`.
fn survey_mut<'a>(ev: &'a mut Evidence, phase: Phase, from: &str) -> &'a mut SurveyRead {
    ev.fleet
        .surveys
        .iter_mut()
        .find(|s| s.phase == phase && s.from == from)
        .unwrap()
}

/// The drive `from` -> `to` in `phase`.
fn drive_mut<'a>(ev: &'a mut Evidence, phase: Phase, from: &str, to: &str) -> &'a mut DriveTry {
    ev.fleet
        .drives
        .iter_mut()
        .find(|d| d.phase == phase && d.from == from && d.to == to)
        .unwrap()
}

fn check(ev: &Evidence, name: &str) -> Check {
    judge(ev).into_iter().find(|c| c.name == name).unwrap()
}

const SPREAD_BEFORE: &str = "fleet: every Mac heard every member before the cut";
const WHOLE_AGAIN: &str = "fleet: whole again after the heal";
const DROVE_EVERY: &str = "fleet: every Mac drove every other after the heal";
const REACHED_EVERY: &str = REACHED_AFTER;

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
            cut_ledger: Some(ledgers::held()),
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
        fleet: whole_fleet(),
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

/// A membership record one island made in the cut, held on the other island's server and a Mac
/// there before the heal, fails [`RECORDS_HELD`] alone: the heads and values the other cut
/// checks read did not cross.
///
/// NEGATIVE CONTROL (restored with cp from a backup plus touch, confirmed with cmp): drop the
/// `split::judge(RECORDS_HELD, ..)` row from `vault_checks`: fails with left `[]`, right
/// `[RECORDS_HELD]` (proven 2026-11-02).
#[test]
fn a_record_that_crossed_the_cut_fails_records_held() {
    let mut ev = whole();
    ev.vault.cut_ledger = Some(ledgers::crossed());
    assert_eq!(failed(&ev), vec![RECORDS_HELD]);
}

/// A run is recorded under `docs/measured/` only where every split it held held: a record that
/// crossed the cut or F21's split voids it, while a run whose removals plane held no F21 split
/// (it did not get that far) stands on the cut alone.
///
/// An F21 split whose records sync went unexercised saw nothing cross, so its run is recorded
/// (its row red).
///
/// NEGATIVE CONTROL (restored with cp from a backup plus touch, confirmed with cmp): in
/// `unmeasured`, leave F21's split out (`f21` always `None`): the run whose F21 split leaked is
/// recordable, and the test fails at `!recordable(&with_split(Some(ledgers::crossed())))`
/// (proven 2026-11-03). Make `split::Shortfall::voids` answer `true` for every shortfall: the
/// unexercised split's run is not recorded, and the test fails at
/// `recordable(&with_split(Some(ledgers::unexercised())))` (proven 2026-11-03).
#[test]
fn a_run_whose_cut_or_f21_split_leaked_is_never_recorded() {
    let recordable = |ev: &Evidence| unmeasured(ev, &judge(ev)).is_empty();
    assert!(recordable(&whole()));

    let mut cut_leaked = whole();
    cut_leaked.vault.cut_ledger = Some(ledgers::crossed());
    assert!(!recordable(&cut_leaked));

    let with_split = |ledger: Option<split::Ledger>| {
        let mut ev = whole();
        ev.removals = Some(removals::Evidence {
            kept: Some(removals::Kept {
                ledger,
                ..removals::Kept::default()
            }),
            ..removals::Evidence::default()
        });
        ev
    };
    assert!(recordable(&with_split(Some(ledgers::held()))));
    assert!(!recordable(&with_split(Some(ledgers::crossed()))));
    assert!(recordable(&with_split(None)));
    assert!(recordable(&with_split(Some(ledgers::unexercised()))));
}

/// The not-recorded line says why each split fell short: one whose server could not be read at
/// its end is not shown to hold, never told as one something crossed, and one that leaked says
/// so.
///
/// NEGATIVE CONTROL (restored with cp from a backup plus touch, confirmed with cmp): in
/// `unmeasured`, word every shortfall as `split::Shortfall::Crossed`: the unread cut reads as
/// crossed, and the test fails at its first `assert_eq!` (proven 2026-11-03).
#[test]
fn a_split_not_read_whole_is_not_recorded_as_one_that_leaked() {
    let mut unread = whole();
    if let Some(ledger) = unread.vault.cut_ledger.as_mut() {
        ledger
            .end
            .unread
            .push("island B's server's records: timed out".to_owned());
    }
    assert_eq!(
        unmeasured(&unread, &judge(&unread)),
        vec!["the cut could not be read whole, so it is not shown that nothing crossed"]
    );
    let mut crossed = whole();
    crossed.vault.cut_ledger = Some(ledgers::crossed());
    assert_eq!(
        unmeasured(&crossed, &judge(&crossed)),
        vec!["a record or head crossed the cut"]
    );
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
    survey_mut(&mut ev, Phase::EarlyCut, "b1")
        .answered
        .push("a2".to_owned());
    assert_eq!(failed(&ev), vec![FLEET_CUT_HELD]);
    let mut ev = whole();
    drive_mut(&mut ev, Phase::EarlyCut, "a1", "b1").ok = true;
    assert_eq!(failed(&ev), vec![FLEET_CUT_HELD]);
}

/// The cut is judged from both islands: surveys from island A alone are no proof it held.
/// NEGATIVE CONTROL: with the older `during.len() < 2` rule, two surveys from island A pass:
/// "assertion `left == right` failed left: [] right: [\"fleet: the cut held\"]".
#[test]
fn a_cut_surveyed_from_one_island_alone_is_not_shown_to_hold() {
    let mut ev = whole();
    ev.fleet
        .surveys
        .retain(|s| s.phase != Phase::EarlyCut || s.from.starts_with('a'));
    ev.fleet
        .surveys
        .push(survey(Phase::EarlyCut, "a2", &["a1", "a2", "a4"]));
    assert_eq!(failed(&ev), vec![FLEET_CUT_HELD]);
    assert!(
        check(&ev, FLEET_CUT_HELD)
            .detail
            .contains("not surveyed from both islands"),
        "{}",
        check(&ev, FLEET_CUT_HELD).detail
    );
}

#[test]
fn a_drive_refused_once_the_copy_aged_out_fails_the_island_that_lost_its_issuer() {
    let mut ev = whole();
    let late = drive_mut(&mut ev, Phase::LateCut, "b1", "b4");
    late.ok = false;
    late.words = "the driver is not in this machine's fleet book".to_owned();
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
    let b1 = survey_mut(&mut ev, Phase::Healed, "b1");
    b1.answered.retain(|m| m != "a2");
    b1.silent
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

/// Every member surveys after the heal, not only a1 and b1: a member that never surveyed is no
/// proof it hears the fleet.
/// NEGATIVE CONTROL: judge only the surveys that ran (drop the `no survey from` arm): "assertion
/// `left == right` failed left: [] right: [\"fleet: whole again after the heal\"]".
#[test]
fn a_member_that_never_surveyed_after_the_heal_is_not_shown_whole() {
    let mut ev = whole();
    ev.fleet
        .surveys
        .retain(|s| !(s.phase == Phase::Healed && s.from == "a4"));
    assert_eq!(failed(&ev), vec![WHOLE_AGAIN]);
    let detail = check(&ev, WHOLE_AGAIN).detail;
    assert!(
        detail.contains("no survey from a4 after the heal"),
        "{detail}"
    );
}

/// What a build whose members take in only the nodes their books hold looks like once the
/// heal settles: a4 (joined at a2) and b3 (joined at b1) hear only the Macs they registered
/// with, a drive between them or from a member that never heard them is refused, and a4's
/// heartbeat reaches only its own two.
/// NEGATIVE CONTROL: count every drive that ran as admitted (`Some(d) if d.ok || true`):
/// "assertion `left == right` failed left: [\"fleet: whole again after the heal\", \"fleet:
/// every Mac's heartbeat reached every member after the heal\"] right: [\"fleet: whole again
/// after the heal\", \"fleet: every Mac drove every other after the heal\", \"fleet: every
/// Mac's heartbeat reached every member after the heal\"]".
#[test]
fn macs_that_joined_at_members_and_never_spread_fail_the_survey_the_drives_and_the_reach() {
    let mut ev = whole();
    survey_mut(&mut ev, Phase::Healed, "a4").answered = s(&["a1", "a2", "a4"]);
    survey_mut(&mut ev, Phase::Healed, "b3").answered = s(&["a1", "b1", "b3"]);
    for (from, to) in [("a4", "b3"), ("b3", "a2"), ("b2", "a4")] {
        let d = drive_mut(&mut ev, Phase::End, from, to);
        d.ok = false;
        d.words = "the driver is not in this machine's fleet book".to_owned();
    }
    ev.fleet
        .reach
        .iter_mut()
        .find(|r| r.mac == "a4")
        .unwrap()
        .reached = s(&["a1", "a2"]);
    assert_eq!(failed(&ev), vec![WHOLE_AGAIN, DROVE_EVERY, REACHED_EVERY]);
    let whole_again = check(&ev, WHOLE_AGAIN).detail;
    assert!(
        whole_again.contains("a4 did not hear b1, b2, b4, b3")
            && whole_again.contains("b3 did not hear a2, a4, b2, b4"),
        "{whole_again}"
    );
    let drove = check(&ev, DROVE_EVERY).detail;
    for pair in ["a4 -> b3 refused", "b3 -> a2 refused", "b2 -> a4 refused"] {
        assert!(drove.contains(pair), "{pair}: {drove}");
    }
}

/// A pair of members never driven at the end is no proof the two bind each other.
/// NEGATIVE CONTROL: pass over a pair with no drive (drop the `never tried` arm): "assertion
/// `left == right` failed left: [] right: [\"fleet: every Mac drove every other after the
/// heal\"]".
#[test]
fn a_pair_never_driven_fails_every_mac_drove_every_other() {
    let mut ev = whole();
    ev.fleet
        .drives
        .retain(|d| !(d.phase == Phase::End && d.from == "b3" && d.to == "a2"));
    assert_eq!(failed(&ev), vec![DROVE_EVERY]);
    let detail = check(&ev, DROVE_EVERY).detail;
    assert!(detail.contains("b3 -> a2 never tried"), "{detail}");
}

/// A heartbeat that asks the same three members every round never reaches the rest: with
/// seven members, each must hear every other's answer after the heal, and a member whose
/// heartbeats were not read reached nobody the lane can show.
/// NEGATIVE CONTROL: count a member as reaching the fleet once three answered it (a round's
/// `HEARTBEAT_MEMBERS`): "assertion `left == right` failed left: [] right: [\"fleet: every
/// Mac's heartbeat reached every member after the heal\"]".
#[test]
fn a_heartbeat_that_asks_the_same_three_members_never_reaches_the_rest() {
    let mut ev = whole();
    ev.fleet
        .reach
        .iter_mut()
        .find(|r| r.mac == "a2")
        .unwrap()
        .reached = s(&["a1", "a4", "b1"]);
    assert_eq!(failed(&ev), vec![REACHED_EVERY]);
    let detail = check(&ev, REACHED_EVERY).detail;
    assert!(
        detail.contains("a2's heartbeat reached a1, a4, b1 and never b2, b4, b3"),
        "{detail}"
    );
    let mut ev = whole();
    ev.fleet.reach.retain(|r| r.mac != "b3");
    assert_eq!(failed(&ev), vec![REACHED_EVERY]);
    let detail = check(&ev, REACHED_EVERY).detail;
    assert!(detail.contains("b3's heartbeats were not read"), "{detail}");
}

/// Before the cut every fleet Mac hears every other, a4 and b4 (joined at a2 and b2) too, and
/// each one surveyed.
/// NEGATIVE CONTROL: judge only a1's and b1's surveys before the cut, as the lane once read
/// them: "assertion `left == right` failed left: [] right: [\"fleet: every Mac heard every
/// member before the cut\"]".
#[test]
fn a_mac_unheard_before_the_cut_fails_the_spread_before_it() {
    let mut ev = whole();
    survey_mut(&mut ev, Phase::Before, "a4").answered = s(&["a1", "a2", "a4"]);
    assert_eq!(failed(&ev), vec![SPREAD_BEFORE]);
    let detail = check(&ev, SPREAD_BEFORE).detail;
    assert!(detail.contains("a4 did not hear b1, b2, b4"), "{detail}");
    let mut ev = whole();
    ev.fleet
        .surveys
        .retain(|s| !(s.phase == Phase::Before && s.from == "b4"));
    assert_eq!(failed(&ev), vec![SPREAD_BEFORE]);
    let detail = check(&ev, SPREAD_BEFORE).detail;
    assert!(
        detail.contains("no survey from b4 before the cut"),
        "{detail}"
    );
}

/// With the minter stopped before the heal (`--minter-away`), its silence is no failure, its
/// book's heartbeats are not read, and every other member must still hear, drive and reach
/// every other, b3 too: membership spreads with no minter.
/// NEGATIVE CONTROL: leave the away Mac among the members (`members_after` ignores `away`):
/// "assertion `left == right` failed left: [\"fleet: whole again after the heal\", \"fleet:
/// every Mac drove every other after the heal\", \"fleet: every Mac's heartbeat reached every
/// member after the heal\"] right: []".
#[test]
fn with_the_minter_away_the_rest_must_still_spread() {
    let mut ev = whole();
    ev.fleet.away = s(&["a1"]);
    ev.fleet.heartbeats.clear();
    let rest: Vec<&str> = AFTER.iter().copied().filter(|m| *m != "a1").collect();
    ev.fleet
        .surveys
        .retain(|s| !(s.phase == Phase::Healed && s.from == "a1"));
    for s in ev
        .fleet
        .surveys
        .iter_mut()
        .filter(|s| s.phase == Phase::Healed)
    {
        s.answered.retain(|m| m != "a1");
    }
    ev.fleet.drives.retain(|d| d.phase != Phase::End);
    ev.fleet.drives.extend(every_pair(&rest));
    ev.fleet.reach = every_reach(&rest);
    assert_eq!(failed(&ev), Vec::<&str>::new());
    assert!(
        check(&ev, WHOLE_AGAIN)
            .detail
            .contains("not read, a1 stopped before the heal"),
        "{}",
        check(&ev, WHOLE_AGAIN).detail
    );
    survey_mut(&mut ev, Phase::Healed, "a2")
        .answered
        .retain(|m| m != "b3");
    assert_eq!(failed(&ev), vec![WHOLE_AGAIN]);
    assert!(
        check(&ev, WHOLE_AGAIN)
            .detail
            .contains("a2 did not hear b3"),
        "{}",
        check(&ev, WHOLE_AGAIN).detail
    );
}

/// `--minter-away` and `--label` read, a label is one plain part of a file name, and each arm
/// names its record apart.
/// NEGATIVE CONTROL: take any `--label` word (`label_word` always true): "assertion `left ==
/// right` failed: --label \"Bad\" was taken left: None right: Some(2)".
#[test]
fn the_minter_away_and_label_flags_read_and_name_the_record() {
    let opts = parse(&s(&["--minter-away", "--label", "nc-issuer-only-merge"])).unwrap();
    assert!(opts.minter_away);
    assert_eq!(opts.label.as_deref(), Some("nc-issuer-only-merge"));
    assert_eq!(
        record_arm(&opts),
        (
            "-minter-away-nc-issuer-only-merge".to_owned(),
            " --minter-away --label nc-issuer-only-merge".to_owned()
        )
    );
    let plain = parse(&[]).unwrap();
    assert!(!plain.minter_away && plain.label.is_none());
    assert_eq!(record_arm(&plain), (String::new(), String::new()));
    let both = parse(&s(&["--one-url", "--writers", "a"])).unwrap();
    assert_eq!(record_arm(&both).0, "-one-url-writers-a");
    let long = "a".repeat(41);
    for bad in ["Bad", "-x", "x-", "a/b", "a b", "..", long.as_str()] {
        assert_eq!(
            parse(&s(&["--label", bad])).map_err(|e| e.code).err(),
            Some(2),
            "--label {bad:?} was taken"
        );
    }
    assert_eq!(parse(&s(&["--label"])).unwrap_err().code, 2);
}

/// Before the cut each of the six fleet Macs has five others and a round asks three: a heartbeat
/// that asks the same three members every round (F17's second review fix undone) leaves two out
/// for good, and a Mac whose heartbeats were not read reached nobody the lane can show.
/// NEGATIVE CONTROL: judge no heartbeat reach before the cut (drop its `reach_check` row):
/// "assertion `left == right` failed left: [] right: [\"fleet: every Mac's heartbeat reached
/// every member before the cut\"]".
#[test]
fn a_heartbeat_that_asks_the_same_three_members_before_the_cut_fails_the_reach_before_it() {
    let mut ev = whole();
    ev.fleet
        .reach_before
        .iter_mut()
        .find(|r| r.mac == "a4")
        .unwrap()
        .reached = s(&["a1", "a2", "b1"]);
    assert_eq!(failed(&ev), vec![REACHED_BEFORE]);
    let detail = check(&ev, REACHED_BEFORE).detail;
    assert!(
        detail.contains("a4's heartbeat reached a1, a2, b1 and never b2, b4"),
        "{detail}"
    );
    assert!(!detail.contains("missed"), "{detail}");
    let mut ev = whole();
    ev.fleet.reach_before.retain(|r| r.mac != "b4");
    assert_eq!(failed(&ev), vec![REACHED_BEFORE]);
    let detail = check(&ev, REACHED_BEFORE).detail;
    assert!(detail.contains("b4's heartbeats were not read"), "{detail}");
}

/// A member whose last dial by a Mac's heartbeat missed, still dark when the row is read, is
/// told from one the rounds never asked: the row names the Macs whose last dial missed.
/// NEGATIVE CONTROL: leave the dark members out of the row (`dark` read as none in
/// `reach_check`): the assertion failed with the row "b1's heartbeat reached b2, b4, b3 and never
/// a1, a2, a4", which no longer says whose last dial missed.
#[test]
fn a_member_passed_over_after_a_missed_dial_is_told_from_one_never_asked() {
    let mut ev = whole();
    let b1 = ev.fleet.reach.iter_mut().find(|r| r.mac == "b1").unwrap();
    b1.reached = s(&["b2", "b4", "b3"]);
    b1.dark = s(&["a1", "a2"]);
    assert_eq!(failed(&ev), vec![REACHED_EVERY]);
    let detail = check(&ev, REACHED_EVERY).detail;
    assert!(
        detail.contains(
            "b1's heartbeat reached b2, b4, b3 and never a1, a2, a4 (its last dial of a1, a2 \
             missed)"
        ),
        "{detail}"
    );
}

/// Whether a member answered a Mac's heartbeat is read from that Mac's own stamps, keyed by the
/// member's node: the member's side says nothing of it. Here a1's stamps hold b1's answer (a1's
/// heartbeat reached b1, as a1's merge of another member's book once moved b1's `seen_ms` in
/// a1's book), and b1's own hold only a missed dial of a1, so b1's heartbeat has not reached a1.
/// NEGATIVE CONTROL: read the member's stamps for the Mac's node (swap `mac_state` and
/// `member_state` in `stamps_for`): "assertion `left == right` failed left:
/// (Some(1793565847733), None) right: (None, Some(1793565847723))".
#[test]
fn a_heartbeat_answer_is_read_from_the_heartbeating_macs_own_stamps() {
    use aterm_net::fleet_member::{ANSWERED_FILE, ensure_private_dir, node_identity};
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let [a1, b1, c] = ["a1", "b1", "c"].map(|m| dir.path().join(m));
    for d in [&a1, &b1, &c] {
        ensure_private_dir(d).expect("a private state directory");
    }
    let a1_node = node_identity(&a1).expect("a1's node").node().to_owned();
    let b1_node = node_identity(&b1).expect("b1's node").node().to_owned();
    let now = u64::try_from(crate::util::now_ms()).expect("a clock after 1970");
    let stamps = |state: &Path, lines: &[String]| {
        aterm_uds::private_file::write_private(
            state,
            ANSWERED_FILE,
            format!("{}\n", lines.join("\n")).as_bytes(),
        )
        .expect("stamps");
    };
    stamps(&a1, &[format!("answered node={b1_node} ms={now}")]);
    stamps(&b1, &[format!("missed node={a1_node} ms={}", now - 10)]);
    assert_eq!(stamps_for(&b1, &a1), (None, Some(now - 10)));
    assert_eq!(stamps_for(&a1, &b1), (Some(now), None));
    stamps(
        &b1,
        &[
            format!("missed node={a1_node} ms={}", now - 10),
            format!("answered node={a1_node} ms={}", now - 5),
        ],
    );
    assert_eq!(stamps_for(&b1, &a1), (Some(now - 5), Some(now - 10)));
    assert_eq!(stamps_for(&b1, &c), (None, None), "c holds no node");
}
