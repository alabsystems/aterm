// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn pace(marks: &str, now: u64) -> Pace {
    Pace {
        build: "1.0.0+gabc".into(),
        marks: Some(marks.into()),
        inputs: None,
        materialization: None,
        network: None,
        now,
    }
}

fn blocked(detail: &str) -> Vec<Row> {
    vec![Row {
        name: "Secrets vault".into(),
        status: "blocked".into(),
        detail: detail.into(),
        elapsed_ms: None,
    }]
}

#[test]
fn the_pause_doubles_to_four_minutes() {
    let minutes: Vec<u64> = (1..=6).map(|tries| pause(tries).as_secs() / 60).collect();
    assert_eq!(minutes, [1, 2, 4, 4, 4, 4]);
    assert_eq!(pause(u32::MAX), LONGEST);
    assert!(LONGEST < Duration::from_secs(5 * 60));
}

#[test]
fn a_pause_ends_with_its_time_or_any_change() {
    let root = aterm_tempfile::tempdir().unwrap();
    let root = root.path();
    let mut rows = blocked("the vault does not answer");
    record(root, 3, &pace("m", 1_000), false, &mut rows).unwrap();
    assert_eq!(
        rows[0].detail,
        "the vault does not answer; aterm tries again by itself a minute after this try, and at once when this Mac's network or vault changes or on Set up now or Join in System State"
    );
    let shown = paused(root, 3, &pace("m", 1_059)).expect("paused");
    assert_eq!(shown[0].detail, rows[0].detail);
    assert_eq!(shown[0].status, "blocked");
    assert!(paused(root, 3, &pace("m", 1_060)).is_none(), "due");
    assert!(paused(root, 3, &pace("n", 1_001)).is_none(), "marks");
    assert!(paused(root, 4, &pace("m", 1_001)).is_none(), "revision");
    let mut other = pace("m", 1_001);
    other.build = "1.0.0+gdef".into();
    assert!(paused(root, 3, &other).is_none(), "build");
    assert!(paused(root, 3, &pace("m", 999)).is_none(), "clock set back");

    let mut rows = blocked("the vault does not answer");
    record(root, 3, &pace("m", 1_060), false, &mut rows).unwrap();
    assert!(rows[0].detail.contains("2 minutes after this try"));
    assert!(paused(root, 3, &pace("m", 1_060 + 119)).is_some());
    let mut rows = blocked("the vault does not answer");
    record(root, 3, &pace("n", 1_200), false, &mut rows).unwrap();
    assert!(rows[0].detail.contains("a minute after this try"));

    std::fs::write(root.join(RECORD), b"not json").unwrap();
    assert!(paused(root, 3, &pace("n", 1_201)).is_none(), "unreadable");

    let mut rows = blocked("the vault does not answer");
    record(root, 3, &pace("n", 1_300), false, &mut rows).unwrap();
    assert!(paused(root, 3, &pace("n", 1_301)).is_some());
    let mut done = vec![Row {
        name: "Secrets vault".into(),
        status: "ok".into(),
        detail: "in vault alab (seq 7)".into(),
        elapsed_ms: None,
    }];
    record(root, 3, &pace("n", 1_302), true, &mut done).unwrap();
    assert_eq!(done[0].detail, "in vault alab (seq 7)");
    assert!(!root.join(RECORD).exists());
    assert!(paused(root, 3, &pace("n", 1_303)).is_none());
}

#[test]
fn parked_retry_deadlines_belong_to_the_unfinished_attempt_and_hold_failed_changes() {
    let root = aterm_tempfile::tempdir().unwrap();
    let root = root.path();
    assert_eq!(retry_due(root, 3, 2_000), None);
    record(root, 3, &pace("m", 1_000), false, &mut blocked("offline")).unwrap();
    assert_eq!(retry_due(root, 3, 1_059), None);
    assert_eq!(retry_due(root, 3, 1_060), Some(1_060));
    assert_eq!(retry_due(root, 3, 1_300), Some(1_060));
    assert_eq!(retry_due(root, 4, 1_300), None, "another run's deadline");
    record(root, 3, &pace("m", 1_300), false, &mut blocked("offline")).unwrap();
    assert_eq!(retry_due(root, 3, 1_419), None);
    assert_eq!(
        retry_due(root, 3, 1_420),
        Some(1_420),
        "a new try advances it"
    );
    let mut waiting = blocked("an administrator must approve");
    waiting[0].status = super::super::WAITING.into();
    record(root, 3, &pace("m", 1_500), false, &mut waiting).unwrap();
    assert_eq!(retry_due(root, 3, 9_000), None);
    assert_eq!(
        waiting[0].detail,
        format!("an administrator must approve; {WAITS}"),
        "a row that waits names no timer"
    );
    let mut unreached = blocked(super::super::scope::NOT_REACHED);
    unreached[0].status = super::super::UNREACHED.into();
    record(root, 3, &pace("u", 1_600), false, &mut unreached).unwrap();
    assert!(unreached[0].detail.contains("a minute after this try"));
    assert_eq!(retry_due(root, 3, 1_659), None);
    assert_eq!(
        retry_due(root, 3, 1_660),
        Some(1_660),
        "a vault that did not answer is asked again"
    );
    let failed = || {
        let mut rows = blocked("the change failed");
        rows[0].status = "error".into();
        rows
    };
    record(root, 3, &pace("m", 2_000), false, &mut failed()).unwrap();
    record(root, 3, &pace("m", 3_000), false, &mut failed()).unwrap();
    assert_eq!(
        retry_due(root, 3, 9_000),
        Some(3_000 + 120),
        "failed changes come due again by themselves"
    );
    let mut rows = blocked("offline on another network");
    record(root, 3, &pace("n", 4_000), false, &mut rows).unwrap();
    assert_eq!(
        retry_due(root, 3, 4_060),
        Some(4_060),
        "external change re-arms it"
    );
}

/// A TRY THAT HEARD NO VAULT THERE NOW COMES DUE BY ITSELF: a server that holds no vault just
/// now (one mid-restart answers so) or a network without one changes nothing on this Mac when
/// the vault comes back, so a try whose rows only wait for that is asked again on the hour-long
/// pause of a vault that did not answer, its row saying when; a try that waits on someone's
/// act alone keeps no timer, its own change waking the run. NEGATIVE CONTROL: give every
/// waiting row no timer, as before, and the restarting server's vault is never asked again.
#[test]
fn a_try_that_heard_no_vault_there_now_comes_due_by_itself_on_the_slow_pace() {
    use super::super::{ABSENT, WAITING};
    use aterm_machines::init_secrets::{NO_VAULT_NOW, NOT_HERE};
    // Another vault answering there (`NOT_HERE`) is as absent as a server holding none:
    // before, its row read as a person's wait and was never asked again.
    for (status, detail, asked) in [
        (ABSENT, NO_VAULT_NOW, true),
        (ABSENT, NOT_HERE, true),
        (ABSENT, super::super::scope::NO_VAULT, true),
        (WAITING, "an administrator must approve", false),
        (WAITING, NO_VAULT_NOW, false),
    ] {
        let root = aterm_tempfile::tempdir().unwrap();
        let root = root.path();
        let mut rows = blocked(detail);
        rows[0].status = status.into();
        record(root, 3, &pace("m", 1_000), false, &mut rows).unwrap();
        if !asked {
            assert_eq!(retry_window(root, 3), None, "{detail}");
            assert_eq!(retry_due(root, 3, 99_000), None, "{detail}");
            assert!(
                rows[0].detail.ends_with(WAITS),
                "{detail}: {}",
                rows[0].detail
            );
            continue;
        }
        assert_eq!(retry_window(root, 3), Some((1_000, 1_060)), "{detail}");
        assert_eq!(retry_ahead(root, 3, 1_059), Some(1_060), "{detail}");
        assert_eq!(retry_due(root, 3, 1_060), Some(1_060), "{detail}");
        assert!(
            rows[0].detail.contains("a minute after this try"),
            "{detail}: {}",
            rows[0].detail
        );
        // Each later try that heard the same waits twice as long, up to an hour.
        let mut at = 1_000;
        let mut pauses = Vec::new();
        for _ in 0..8 {
            let (from, due) = retry_window(root, 3).unwrap();
            pauses.push((due - from) / 60);
            at = due;
            let mut again = blocked(detail);
            again[0].status = status.into();
            record(root, 3, &pace("m", at), false, &mut again).unwrap();
        }
        assert_eq!(pauses, [1, 2, 4, 8, 16, 32, 60, 60], "{detail}");
        assert_eq!(retry_window(root, 3), Some((at, at + 3_600)), "{detail}");
    }
}

/// A TRY THAT FAILED TWICE COMES DUE BY ITSELF: two tries in a row that left a change not
/// made are asked again less often, never more than an hour apart, with no fact to change and
/// no press; the deadline is the record's, so a relaunch or a reboot reads the same one.
#[test]
fn a_try_that_failed_twice_comes_due_by_itself_and_survives_a_relaunch() {
    let root = aterm_tempfile::tempdir().unwrap();
    let root = root.path();
    let failed = || {
        let mut rows = blocked("the change failed");
        rows[0].status = "error".into();
        rows
    };
    record(root, 3, &pace("m", 1_000), false, &mut failed()).unwrap();
    record(root, 3, &pace("m", 1_060), false, &mut failed()).unwrap();
    assert!(
        paused(root, 3, &pace("m", 1_061)).is_some(),
        "calm right after"
    );
    let longest = LONGEST_UNREACHED.as_secs();
    let due = retry_ahead(root, 3, 1_061).expect("a try that failed twice has its next ahead");
    assert!(due <= 1_060 + longest, "never more than an hour on");
    assert_eq!(retry_due(root, 3, due), Some(due));
    assert!(
        paused(root, 3, &pace("m", due)).is_none(),
        "a pass at its deadline tries, reading only the record"
    );
    record(root, 3, &pace("m", due), false, &mut failed()).unwrap();
    let next = retry_ahead(root, 3, due).expect("tried, and paused again");
    assert!(
        next - due >= due - 1_060 && next - due <= longest,
        "longer each time, never past the hour"
    );
}

/// Check again starts the run's try afresh: its record goes, so the next pass tries now
/// with no failure counted; another run's record stays as it is.
#[test]
fn check_again_starts_the_runs_try_afresh() {
    let root = aterm_tempfile::tempdir().unwrap();
    let root = root.path();
    let mut failed = blocked("the change failed");
    failed[0].status = "error".into();
    record(root, 3, &pace("m", 1_000), false, &mut failed.clone()).unwrap();
    record(root, 3, &pace("m", 1_060), false, &mut failed.clone()).unwrap();
    assert!(paused(root, 3, &pace("m", 1_100)).is_some(), "the control");
    afresh(root, 4).unwrap();
    assert!(
        paused(root, 3, &pace("m", 1_100)).is_some(),
        "another run's press"
    );
    afresh(root, 3).unwrap();
    assert!(paused(root, 3, &pace("m", 1_100)).is_none());
    assert!(Attempt::load(root).is_none());
    record(root, 3, &pace("m", 1_100), false, &mut failed).unwrap();
    assert_eq!(Attempt::load(root).unwrap().failures, 1, "counted afresh");
    afresh(root, 3).unwrap();
    afresh(root, 3).unwrap();
}

/// This Mac on the network `routes` names, at `now`, with no vault to read.
struct OnRoute(String, u64);

impl Reads for OnRoute {
    fn build(&self) -> String {
        "1.0.0+gabc".into()
    }

    fn now(&self) -> u64 {
        self.1
    }

    fn routes(&self) -> Option<String> {
        Some(self.0.clone())
    }

    fn vault(&self) -> Option<Arc<dyn SecretsOps>> {
        None
    }
}

/// AN UNREACHED VAULT IS ASKED LESS OFTEN, AND CALMLY: a vault that did not answer is asked
/// again 1, 2, 4, 8, 16 and 32 minutes after each try, then once an hour, never counted as a
/// failure; and only a real move of this Mac's network asks at once. An AirDrop, `llw` or
/// VPN address, or a new temporary IPv6 address, coming mid-pause moves nothing.
/// NEGATIVE CONTROL: keyed to every interface address, as the routes were, that churn asks.
#[test]
fn an_unreached_vault_is_asked_up_to_hourly_and_at_once_only_on_a_real_network_move() {
    use aterm_secrets::network::{InterfaceAddress, default_route_of};
    use std::net::{IpAddr, Ipv4Addr};
    let root = aterm_tempfile::tempdir().unwrap();
    let root = root.path();
    let source = Ipv4Addr::new(192, 168, 1, 20);
    let address = |name: &str, address: IpAddr| InterfaceAddress {
        name: name.into(),
        address,
        netmask: None,
        flags: 1,
    };
    let en0 = address("en0", IpAddr::V4(source));
    let home: &[u8] = &[0x00, 0x11, 0x22, 0x33, 0x44, 0x55];
    let on = |interfaces: &[InterfaceAddress], gateway: &[u8], now| {
        super::pace(&OnRoute(
            default_route_of(source, interfaces, Some(gateway)),
            now,
        ))
    };
    let unreached = || {
        vec![Row {
            name: "Secrets vault".into(),
            status: super::super::UNREACHED.into(),
            detail: super::super::scope::NOT_REACHED.into(),
            elapsed_ms: None,
        }]
    };
    let mut at = 1_000;
    let mut minutes = Vec::new();
    let mut rows = unreached();
    for _ in 0..8 {
        rows = unreached();
        record(
            root,
            3,
            &on(std::slice::from_ref(&en0), home, at),
            false,
            &mut rows,
        )
        .unwrap();
        let due = retry_ahead(root, 3, at).expect("a retry is ahead");
        minutes.push((due - at) / 60);
        assert_eq!(retry_due(root, 3, due), Some(due));
        at = due;
    }
    assert_eq!(minutes, [1, 2, 4, 8, 16, 32, 60, 60]);
    assert!(
        rows[0].detail.contains("an hour after this try"),
        "{rows:?}"
    );
    assert_eq!(page(root, 3, &rows).rows[0].detail, "", "it stays calm");
    let last = at - LONGEST_UNREACHED.as_secs();
    let churned = [
        en0.clone(),
        address("awdl0", "fe80::1".parse().unwrap()),
        address("llw0", "fe80::2".parse().unwrap()),
        address("utun4", IpAddr::V4(Ipv4Addr::new(100, 64, 0, 7))),
        address("en0", "2001:db8::abcd:1".parse().unwrap()),
    ];
    assert!(
        paused(root, 3, &on(&churned, home, last + 60)).is_some(),
        "address churn asked the vault again"
    );
    let elsewhere: &[u8] = &[0x00, 0x11, 0x22, 0x33, 0x44, 0x66];
    assert!(
        paused(
            root,
            3,
            &on(std::slice::from_ref(&en0), elsewhere, last + 60)
        )
        .is_none(),
        "a move to another network asks at once"
    );
    let every = |interfaces: &[InterfaceAddress]| format!("{:?}", Some(interfaces));
    let control = aterm_tempfile::tempdir().unwrap();
    let control = control.path();
    let then = super::pace(&OnRoute(every(std::slice::from_ref(&en0)), 1_000));
    record(control, 3, &then, false, &mut unreached()).unwrap();
    assert!(
        paused(control, 3, &super::pace(&OnRoute(every(&churned), 1_010))).is_none(),
        "CONTROL: every interface's address moves with the churn"
    );
    let mut offline = Vec::new();
    for tries in 1..=4 {
        offline = blocked("the vault does not answer");
        record(root, 4, &pace("m", 10_000 * tries), false, &mut offline).unwrap();
    }
    assert!(
        offline[0].detail.contains("4 minutes after this try"),
        "CONTROL: any other unfinished row keeps the four-minute cap: {offline:?}"
    );
}

/// A route read whose address moved to another subnet is another network, even while its new
/// gateway's hardware address is not resolved yet (`en0 10.0.4.9 -`): the route source tells
/// only address and link changes, not ARP's, so this read may be the only one a paused try
/// gets before its pause (up to an hour) runs out. The same address with the hardware unread
/// stays paused ([`a_route_read_with_a_part_unread_is_no_move_for_a_paused_try`]).
#[test]
fn a_route_on_another_address_ends_the_pause_before_arp_resolves() {
    use aterm_secrets::network::{InterfaceAddress, default_route_of};
    use std::net::{IpAddr, Ipv4Addr};
    let root = aterm_tempfile::tempdir().unwrap();
    let root = root.path();
    let iface = |source: Ipv4Addr| {
        [InterfaceAddress {
            name: "en0".into(),
            address: IpAddr::V4(source),
            netmask: None,
            flags: 1,
        }]
    };
    let home_ip = Ipv4Addr::new(192, 168, 1, 20);
    let home_hw: &[u8] = &[0x00, 0x11, 0x22, 0x33, 0x44, 0x55];
    let on = |source: Ipv4Addr, hardware: Option<&[u8]>, now| {
        super::pace(&OnRoute(
            default_route_of(source, &iface(source), hardware),
            now,
        ))
    };
    let mut rows = vec![Row {
        name: "Secrets vault".into(),
        status: super::super::UNREACHED.into(),
        detail: super::super::scope::NOT_REACHED.into(),
        elapsed_ms: None,
    }];
    record(
        root,
        3,
        &on(home_ip, Some(home_hw), 1_000),
        false,
        &mut rows,
    )
    .unwrap();
    assert!(paused(root, 3, &on(home_ip, Some(home_hw), 1_010)).is_some());
    assert!(
        paused(root, 3, &on(home_ip, None, 1_015)).is_some(),
        "CONTROL: the same address, its gateway unread: still paused"
    );
    let away = Ipv4Addr::new(10, 0, 4, 9);
    assert!(
        paused(root, 3, &on(away, None, 1_020)).is_none(),
        "this Mac's address moved to another subnet: another network, whatever its new \
         gateway's hardware reads yet"
    );
}

/// A wake reads this Mac's route before the gateway's hardware address resolves
/// (`en0 192.168.1.20 -`): a part not read is no move, so the paused try stays paused on the
/// same network, and only a route read whole on another gateway asks at once.
/// NEGATIVE CONTROL: that other gateway's route.
#[test]
fn a_route_read_with_a_part_unread_is_no_move_for_a_paused_try() {
    use aterm_secrets::network::{InterfaceAddress, default_route_of};
    use std::net::{IpAddr, Ipv4Addr};
    let root = aterm_tempfile::tempdir().unwrap();
    let root = root.path();
    let source = Ipv4Addr::new(192, 168, 1, 20);
    let en0 = [InterfaceAddress {
        name: "en0".into(),
        address: IpAddr::V4(source),
        netmask: None,
        flags: 1,
    }];
    let home: &[u8] = &[0x00, 0x11, 0x22, 0x33, 0x44, 0x55];
    let on = |hardware: Option<&[u8]>, now| {
        super::pace(&OnRoute(default_route_of(source, &en0, hardware), now))
    };
    let mut rows = vec![Row {
        name: "Secrets vault".into(),
        status: super::super::UNREACHED.into(),
        detail: super::super::scope::NOT_REACHED.into(),
        elapsed_ms: None,
    }];
    record(root, 3, &on(Some(home), 1_000), false, &mut rows).unwrap();
    let waking = on(None, 1_010);
    assert_eq!(
        waking
            .inputs
            .as_ref()
            .and_then(|inputs| inputs.routes.as_ref()),
        None,
        "a route with a part unread is not known"
    );
    assert!(
        paused(root, 3, &waking).is_some(),
        "the gateway's hardware not read yet: the try stays paused"
    );
    assert!(
        paused(root, 3, &on(Some(home), 1_020)).is_some(),
        "read whole on the same network: still paused"
    );
    let elsewhere: &[u8] = &[0x00, 0x11, 0x22, 0x33, 0x44, 0x66];
    assert!(
        paused(root, 3, &on(Some(elsewhere), 1_030)).is_none(),
        "CONTROL: another gateway asks at once"
    );
}

#[test]
fn builds_taking_turns_share_one_pause() {
    let root = aterm_tempfile::tempdir().unwrap();
    let root = root.path();
    let mut old = pace("m", 1_000);
    old.build = "1.0.0+gold".into();
    let mut new = pace("m", 1_010);
    new.build = "1.0.0+gnew".into();
    record(root, 3, &old, false, &mut blocked("offline")).unwrap();
    assert!(
        paused(root, 3, &new).is_none(),
        "a build that has not tried"
    );
    record(root, 3, &new, false, &mut blocked("offline")).unwrap();
    old.now = 1_020;
    assert!(
        paused(root, 3, &old).is_some(),
        "the old build is in the row"
    );
    new.now = 1_030;
    assert!(paused(root, 3, &new).is_some());
    let mut third = pace("m", 1_040);
    third.build = "1.0.0+gthird".into();
    assert!(paused(root, 3, &third).is_none(), "the control");
}

/// A change that keeps failing is tried again by itself, less often, and never held: the
/// pause after two failures in a row grows toward an hour, and each build and each run keeps
/// its own count.
#[test]
fn a_change_that_keeps_failing_is_tried_less_often_and_never_held() {
    let root = aterm_tempfile::tempdir().unwrap();
    let root = root.path();
    let failed = || {
        vec![Row {
            name: "Secrets vault".into(),
            status: "error".into(),
            detail: "joining the vault: which nothing pins".into(),
            elapsed_ms: None,
        }]
    };
    let mut rows = failed();
    record(root, 3, &pace("m", 1_000), false, &mut rows).unwrap();
    assert!(
        rows[0].detail.contains("a minute after this try"),
        "{rows:?}"
    );
    assert!(
        paused(root, 3, &pace("m", 1_060)).is_none(),
        "the second try"
    );
    let mut rows = failed();
    record(root, 3, &pace("m", 1_060), false, &mut rows).unwrap();
    assert!(
        rows[0].detail.contains("2 minutes after this try"),
        "{rows:?}"
    );
    assert_eq!(page(root, 3, &failed()).rows[0].detail, PAGE_SLOWER);
    assert_eq!(
        page(root, 4, &failed()).rows[0].detail,
        PAGE_RETRIES,
        "an older revision's failing attempt never marks a new revision's first error"
    );
    let shown = paused(root, 3, &pace("m", 1_179)).expect("calm until its pause ends");
    assert_eq!(shown[0].detail, rows[0].detail);
    for later in [1_180, 1_060 + 3_600, 1_060 + 86_400] {
        assert!(
            paused(root, 3, &pace("m", later)).is_none(),
            "tried again by itself at {later}"
        );
    }
    assert!(paused(root, 3, &pace("n", 1_100)).is_none(), "marks");
    assert!(paused(root, 4, &pace("m", 1_100)).is_none(), "run");
    let mut other = pace("m", 1_100);
    other.build = "1.0.0+gdef".into();
    assert!(paused(root, 3, &other).is_none(), "build");

    let mut rows = failed();
    record(root, 3, &other, false, &mut rows).unwrap();
    assert!(
        rows[0].detail.contains("a minute after this try"),
        "a new build's first try: {rows:?}"
    );
    assert_eq!(page(root, 3, &failed()).rows[0].detail, PAGE_SLOWER);
    assert!(
        paused(root, 3, &pace("m", 1_159)).is_some(),
        "calm for both builds"
    );
    assert!(
        paused(root, 3, &pace("m", 1_160)).is_none(),
        "and due for both"
    );
    let mut third = pace("m", 1_200);
    third.build = "1.0.0+gthird".into();
    let mut rows = blocked("the vault does not answer");
    record(root, 3, &third, false, &mut rows).unwrap();
    assert!(
        rows[0].detail.contains("a minute after this try"),
        "{rows:?}"
    );
    assert!(paused(root, 3, &pace("m", 1_260)).is_none(), "the control");
}

#[test]
fn an_unknown_materialization_read_never_erases_a_known_hold() {
    let root = aterm_tempfile::tempdir().unwrap();
    let root = root.path();
    let failed = || {
        vec![Row {
            name: "Secrets vault".into(),
            status: "error".into(),
            detail: "injected change failure".into(),
            elapsed_ms: None,
        }]
    };
    let mut unknown = pace("unchanged source and interface", 1_000);
    record(root, 3, &unknown, false, &mut failed()).unwrap();
    record(root, 3, &unknown, false, &mut failed()).unwrap();
    assert!(paused(root, 3, &unknown).is_some());

    let mut known = unknown.clone();
    known.materialization = MaterializationMark::from_cache_key(&"01".repeat(32));
    assert!(known.materialization.is_some());
    assert!(
        paused(root, 3, &known).is_none(),
        "the first conclusive local reading permits one new try"
    );
    record(root, 3, &known, false, &mut failed()).unwrap();
    known.now += FIRST_PAUSE.as_secs();
    unknown.now = known.now;
    record(root, 3, &known, false, &mut failed()).unwrap();
    assert!(paused(root, 3, &known).is_some());
    assert!(paused(root, 3, &unknown).is_some(), "busy is no new fact");
    // Even a recorded outcome whose post-stage read was contended keeps
    // the earlier identity; an unknown/known cycle never re-arms it.
    record(root, 3, &unknown, false, &mut failed()).unwrap();
    assert_eq!(
        Attempt::load(root).unwrap().materialization,
        known.materialization
    );
    assert!(paused(root, 3, &known).is_some());
    known.materialization = MaterializationMark::from_cache_key(&"02".repeat(32));
    assert!(
        paused(root, 3, &known).is_none(),
        "a real change releases it"
    );
}
