// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::envelope;

/// A standing browse that found no broker waits for one with no timer of its own, however long
/// it has waited, and the first browse that then ends waits [`RESTART_FIRST`]; a browse that ran
/// and ended backs off doubling, up to [`RESTART_LONGEST`], and from the start again once one ran
/// that long. NEGATIVE CONTROL: the broker's absence on a pause of its own (the 2 s cadence
/// before) fails the first assertion; on the doubled backoff, it waits 5, 10, 20, 40 and 60 s.
#[test]
fn a_standing_browse_backs_off_and_waits_for_its_broker_with_no_timer() {
    let mut pause = RESTART_FIRST;
    for _ in 0..8 {
        let (wait, next) = restart_pause(Started::NoBroker, pause);
        assert_eq!(wait, None);
        pause = next;
    }
    let quick = Started::Ran(Duration::from_secs(1));
    let mut waits = Vec::new();
    for _ in 0..6 {
        let (wait, next) = restart_pause(quick, pause);
        waits.push(wait.map(|wait| wait.as_secs()));
        pause = next;
    }
    assert_eq!(waits, [5, 10, 20, 40, 60, 60].map(Some));
    let (wait, _) = restart_pause(Started::Ran(RESTART_LONGEST), pause);
    assert_eq!(wait, Some(RESTART_FIRST));
}

/// A browse's add of `name` of `service` on `interface`, as the broker streams it.
fn added(service: &str, interface: u32, name: &str) -> Event {
    Event::Added(Instance {
        name: name.to_owned(),
        service: format!("{service}."),
        domain: "local.".to_owned(),
        interface,
    })
}

/// A browse's removal of `name` of `service` from `interface`.
fn removed(service: &str, interface: u32, name: &str) -> Event {
    let Event::Added(instance) = added(service, interface, name) else {
        unreachable!("added makes an add")
    };
    Event::Removed(instance)
}

/// A service resolve's answer: `host` on `port` with `txt`'s strings.
fn resolved(host: &str, port: u16, txt: &[&str]) -> Event {
    Event::Resolved {
        host: host.to_owned(),
        port,
        txt: txt
            .iter()
            .map(|string| string.as_bytes().to_vec())
            .collect(),
        interface: 4,
    }
}

/// A member's instance name from a browse of `_alab._tcp` and its fleet port from its
/// resolve: active `_alab._tcp` instances only, the host lowercase without its dot, and a bad
/// port or host left out.
#[test]
fn a_resolved_member_is_its_host_and_fleet_port() {
    let browse = [
        added(MEMBER_SERVICE, 14, "alab-gone"),
        added(MEMBER_SERVICE, 14, "alab-1a2b3c4d"),
        added(MEMBER_SERVICE, 15, "alab-9f8e7d6c"),
        added("_alab-vault._tcp", 14, "alab-secrets"),
        removed(MEMBER_SERVICE, 14, "alab-gone"),
    ];
    assert_eq!(
        heard_instances(browse, MEMBER_SERVICE),
        ["alab-1a2b3c4d", "alab-9f8e7d6c"]
    );
    assert_eq!(
        member_target(&resolved("M22.local.", 7100, &["v=1"])).as_deref(),
        Some("m22.local:7100")
    );
    assert_eq!(member_target(&resolved("m7.local.", 0, &[])), None);
    assert_eq!(member_target(&resolved("bad host.local.", 7100, &[])), None);
    assert_eq!(member_target(&added(MEMBER_SERVICE, 4, "m7")), None);
}

/// `ATERM_LAB_MEMBER_SERVICE` unset browses `_alab._tcp`; set, it names a lab service or the
/// browse fails, and it never falls back to the real members' service.
///
/// NEGATIVE CONTROL (2026-10-29): `member_service` with its refusal arm returning
/// `Ok(MEMBER_SERVICE.to_owned())` (a value it cannot take falls back to the real members)
/// fails here at the first refused value: `"_alab._tcp" fell back to Ok("_alab._tcp")`.
#[test]
fn a_lab_member_service_is_taken_and_any_other_value_finds_no_member() {
    use std::ffi::OsStr;
    assert_eq!(super::member_service(None).unwrap(), "_alab._tcp");
    for lab in [
        "_alab-lab-k3x9q2._tcp",
        "_alab-lab-a._tcp",
        "_alab-lab-000000._tcp",
    ] {
        assert!(super::lab_service(lab), "{lab}");
        assert_eq!(super::member_service(Some(OsStr::new(lab))).unwrap(), lab);
    }
    for refused in [
        "_alab._tcp",
        "_alab-vault._tcp",
        "",
        "_alab-lab-._tcp",
        "_alab-lab-k3x9q2x._tcp",
        "_alab-lab-K3X9._tcp",
        "_alab-lab-k3x9._udp",
        "_alab-lab-k3-9._tcp",
        "_alab-lab-k3x9._tcp.",
        " _alab-lab-k3x9._tcp",
        "_alab-nux-k3x9._tcp",
    ] {
        assert!(!super::lab_service(refused), "{refused:?}");
        let said = super::member_service(Some(OsStr::new(refused)));
        assert!(said.is_err(), "{refused:?} fell back to {said:?}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        assert!(super::member_service(Some(OsStr::from_bytes(b"_alab-lab-\xff._tcp"))).is_err());
    }
}

/// `ATERM_LAB_VAULT_SERVICE` unset, a vault browse browses `_alab-vault._tcp`; set, it names
/// a lab service or no vault is browsed at all, never the real vault's type.
///
/// NEGATIVE CONTROL: `vault_service` with its refusal arm returning
/// `Ok(SERVICE_TYPE.to_owned())` fails here at the first refused value.
#[test]
fn a_lab_vault_service_is_taken_and_any_other_value_browses_no_vault() {
    use std::ffi::OsStr;
    let vault = crate::server::SERVICE_TYPE;
    let lab = "_alab-lab-k3x9q2._tcp";
    assert_eq!(super::vault_service(None).unwrap(), vault);
    assert_eq!(super::vault_service(Some(OsStr::new(lab))).unwrap(), lab);
    for refused in [vault, "_alab._tcp", "", "_alab-lab-K3X9._tcp"] {
        let said = super::vault_service(Some(OsStr::new(refused)));
        assert!(said.is_err(), "{refused:?} fell back to {said:?}");
    }
}

/// `ATERM_LAB_VAULT_SERVICE` set, good value or bad, the look's candidates hold no
/// compiled-in ALab address: a lab's look names only what its lab browse heard. Unset, the
/// defaults come first, as ever.
///
/// NEGATIVE CONTROL: `look_defaults` answering `ALAB_DEFAULT_URLS` for a set variable fails
/// at the first value.
#[test]
fn a_lab_look_asks_no_compiled_in_address() {
    use std::ffi::OsStr;
    let heard = DiscoveredVault {
        url: "http://alab-secrets.local:7485/v1/alab".to_owned(),
        txt: None,
    };
    for set in ["_alab-lab-k3x9q2._tcp", crate::server::SERVICE_TYPE, ""] {
        let urls = candidate_urls(
            super::look_defaults(Some(OsStr::new(set))),
            std::slice::from_ref(&heard),
            crate::ALAB_VAULT,
        );
        assert!(
            urls.iter().all(|url| !crate::is_alab_default_url(url)),
            "{set:?}: {urls:?}"
        );
        assert_eq!(urls, std::slice::from_ref(&heard.url), "{set:?}");
    }
    let urls = candidate_urls(
        super::look_defaults(None),
        std::slice::from_ref(&heard),
        crate::ALAB_VAULT,
    );
    assert_eq!(
        urls.first().map(String::as_str),
        Some(crate::ALAB_DEFAULT_URL)
    );
    assert!(urls.contains(&heard.url));
}

/// The look logs each vault instance it hears once per process, on a line naming the
/// instance and the service, and holds at most `MOST_LOGGED` of them.
///
/// NEGATIVE CONTROL: `first_heard` returning `true` for one already logged fails at the
/// second hearing.
#[test]
fn each_vault_instance_heard_is_logged_once() {
    let lab = "_alab-lab-k3x9q2._tcp";
    let line = super::vault_heard_line(lab, "alab-secrets");
    assert!(
        line.contains(lab) && line.contains("alab-secrets"),
        "{line}"
    );
    let mut logged = Vec::new();
    assert!(super::first_heard(&mut logged, lab, "alab-secrets"));
    assert!(!super::first_heard(&mut logged, lab, "ALAB-SECRETS"));
    assert!(super::first_heard(
        &mut logged,
        crate::server::SERVICE_TYPE,
        "alab-secrets"
    ));
    for n in logged.len()..super::MOST_LOGGED {
        assert!(super::first_heard(&mut logged, lab, &format!("m{n}")));
    }
    assert!(!super::first_heard(&mut logged, lab, "one-more"));
}

/// Only the client look's watch logs what it hears and takes the lab knob; a vault server's
/// [`Watch::start`] browses `_alab-vault._tcp`, the type its own advert is registered under.
///
/// NEGATIVE CONTROL: `Watch::start_for` building its watch with `vault_type().ok()` or
/// `look: true` fails here.
#[test]
fn a_servers_vault_watch_is_not_the_look() {
    let bell = Arc::new(Bell::default());
    let server = Watch::start(&bell, &[]);
    assert!(!server.watched.look);
    assert_eq!(
        server.watched.browsed.as_deref(),
        Some(crate::server::SERVICE_TYPE)
    );
    assert!(Watch::start_look(&bell, &[]).watched.look);
}

/// The vault's watch, where `ATERM_LAB_VAULT_SERVICE` names a lab service, hears that
/// service's instances alone; the real vault's type is another type's event to it.
///
/// NEGATIVE CONTROL: `hear_event` matching `Watched::service` in place of `browsed` hears the
/// real vault and not the lab's.
#[test]
fn the_vault_watch_of_a_lab_service_hears_that_service_alone() {
    let lab = "_alab-lab-k3x9q2._tcp";
    let bell = Arc::new(Bell::default());
    let watch = Watch::idle(&bell, &[]);
    let watched = Watched::new(
        crate::server::SERVICE_TYPE,
        Some(lab.to_owned()),
        &bell,
        &[],
    );
    let lab_watch = Watch {
        watched: Arc::new(watched),
        closing: None,
    };
    for w in [&watch, &lab_watch] {
        hears(w, &added(crate::server::SERVICE_TYPE, 4, "alab-secrets"));
        hears(w, &added(lab, 4, "alab-secrets (2)"));
    }
    assert_eq!(watch.heard(), ["alab-secrets"]);
    assert_eq!(lab_watch.heard(), ["alab-secrets (2)"]);
}

/// A browse of a lab service reads that service's instances alone: a real member heard on the
/// same browse is no lab member, and a lab member is no real one.
///
/// NEGATIVE CONTROL: `heard_instances` reading `MEMBER_SERVICE` whatever service it is given
/// fails here: `left: ["alab-1a2b3c4d"]`, `right: ["alab-5e6f7a8b"]`.
#[test]
fn a_lab_browse_reads_its_own_service_alone() {
    let browse = [
        added(MEMBER_SERVICE, 14, "alab-1a2b3c4d"),
        added("_alab-lab-k3x9q2._tcp", 6, "alab-5e6f7a8b"),
        added("_alab-lab-zzzzzz._tcp", 6, "alab-00000000"),
    ];
    assert_eq!(
        heard_instances(browse.clone(), "_alab-lab-k3x9q2._tcp"),
        ["alab-5e6f7a8b"]
    );
    assert_eq!(heard_instances(browse, MEMBER_SERVICE), ["alab-1a2b3c4d"]);
}

/// A joiner's browse keeps every member of a lab of 40 Macs, so each one is asked; a flood of
/// instances is asked at most [`MAX_MEMBERS`] at a time.
/// NEGATIVE CONTROL: the cut at `MAX_CANDIDATES` (16) this browse had leaves 24 of them
/// never asked.
#[test]
fn a_joiners_browse_keeps_every_member_of_a_lab_of_forty() {
    let mut browse: Vec<Event> = (0..40)
        .map(|n| added(MEMBER_SERVICE, 14, &format!("alab-{n:08x}")))
        .collect();
    let heard = heard_instances(browse.clone(), MEMBER_SERVICE);
    assert_eq!(heard.len(), 40, "{heard:?}");
    assert_eq!(heard[39], "alab-00000027");
    assert_eq!(
        rotated_names(heard, &AtomicUsize::new(0), MAX_MEMBERS).len(),
        40
    );
    browse.extend(
        (40..MAX_MEMBERS + 10).map(|n| added(MEMBER_SERVICE, 14, &format!("alab-{n:08x}"))),
    );
    assert_eq!(
        rotated_names(
            heard_instances(browse, MEMBER_SERVICE),
            &AtomicUsize::new(0),
            MAX_MEMBERS
        )
        .len(),
        MAX_MEMBERS,
        "a flood of instances stays bounded"
    );
}

#[test]
fn repeated_member_browses_do_not_exclude_names_beyond_the_first_batch() {
    let names: Vec<_> = (0..MAX_MEMBERS * 2 + 1)
        .map(|n| format!("alab-{n:08x}"))
        .collect();
    let expected: std::collections::BTreeSet<_> = names.iter().cloned().collect();
    for reordered in [false, true] {
        let from = AtomicUsize::new(0);
        let mut selected = std::collections::BTreeSet::new();
        for pass in 0..names.len().div_ceil(MAX_MEMBERS) {
            let mut arrivals = names.clone();
            if reordered {
                arrivals.rotate_right((pass * MAX_MEMBERS) % names.len());
            }
            let browse = arrivals.iter().map(|name| added(MEMBER_SERVICE, 14, name));
            let heard = heard_instances(browse, MEMBER_SERVICE);
            assert_eq!(heard, arrivals, "the fold keeps the browse's order");
            let batch = rotated_names(heard, &from, MAX_MEMBERS);
            assert!(batch.len() <= MAX_MEMBERS);
            assert!(batch.iter().all(|name| expected.contains(name)));
            selected.extend(batch);
        }
        assert_eq!(selected, expected, "reordered={reordered}");
    }
}

#[test]
fn lab_member_batches_rotate_without_selecting_live_members() {
    let service = "_alab-lab-merge1._tcp";
    let browse: Vec<Event> = (0..MAX_MEMBERS * 2)
        .flat_map(|n| {
            [service, MEMBER_SERVICE].map(move |kind| added(kind, 14, &format!("alab-{n:08x}")))
        })
        .collect();
    let from = AtomicUsize::new(0);
    let batch =
        |service: &str| rotated_names(heard_instances(browse.clone(), service), &from, MAX_MEMBERS);
    let first = batch(service);
    let second = batch(service);
    assert_eq!(first.len(), MAX_MEMBERS);
    assert_eq!(second.len(), MAX_MEMBERS);
    assert!(first.iter().all(|name| !second.contains(name)));
    assert!(batch("_alab-lab-other._tcp").is_empty());
}

/// `alab-secrets` and its Bonjour renames are holders; any other name, including one from
/// the LAN whose multibyte character sits across the cut after `alab-secrets`' length, is
/// not, and reading it never panics (a panic stopped vault-tend on every server that heard it).
/// NEGATIVE CONTROL: `split_at` in place of `split_at_checked` panics on the third name.
#[test]
fn only_alab_secrets_and_its_renames_are_holders_and_no_name_panics() {
    let alab = crate::alab_advertised();
    assert_eq!(alab, "alab-secrets", "the advertised holder name moved");
    assert!(is_alab_holder_instance("alab-secrets"));
    assert!(is_alab_holder_instance("ALAB-Secrets (2)"));
    assert!(is_alab_holder_instance(" alab-secrets (17) "));
    // `alab-secret` is 11 bytes, then a 2-byte `é` covers bytes 11 and 12.
    for name in [
        "alab-secreté (22)",
        "alab-secret€€ (3)",
        "ålab-secrets (2)",
        "alab-secrets (x)",
        "alab-secrets ()",
        "alab-secrets(2)",
        "alab-secrets (2",
        "m22",
        "",
    ] {
        assert!(!is_alab_holder_instance(name), "{name:?} read as a holder");
    }
}

/// A standing browse rings its bell for each vault server it had not heard, or heard on
/// another interface: once per Add. One heard already, one Removed and not back, and this
/// process's own instance ring nothing. It keeps what is heard now.
/// NEGATIVE CONTROL: a watch that keeps rows and rings nothing leaves every nap to its timer.
#[test]
fn a_standing_browse_rings_for_each_server_it_had_not_heard() {
    let bell = Arc::new(Bell::default());
    let watch = Watch::idle(&bell, &["m22"]);
    let rung = || bell.nap(Duration::ZERO);
    assert_eq!(rung(), Wake::Timer, "nothing heard");
    watch.hear(true, 4, "m3");
    assert_eq!(rung(), Wake::Browse, "a server not heard before");
    watch.hear(true, 4, "m3");
    assert_eq!(rung(), Wake::Timer, "heard already");
    watch.hear(true, 9, "m3");
    assert_eq!(rung(), Wake::Browse, "heard on another interface");
    watch.hear(true, 4, "m22");
    assert_eq!(rung(), Wake::Timer, "this process's own");
    watch.hear(true, 9, "M22");
    assert_eq!(rung(), Wake::Timer, "this process's own in uppercase");
    watch.hear(false, 9, "M22");
    watch.hear(false, 4, "m3");
    watch.hear(false, 9, "m3");
    assert_eq!(rung(), Wake::Timer, "a removal");
    assert_eq!(watch.heard(), ["m22"]);
    watch.hear(true, 4, "m3");
    assert_eq!(rung(), Wake::Browse, "back after it left");
    assert_eq!(watch.heard(), ["m22", "m3"]);
    assert!(
        watch.found(Duration::from_secs(1)).is_empty(),
        "a test resolves nothing"
    );
}

/// The node job's service.
const ALAB_TCP: &str = "_alab._tcp";

/// Have `watch` hear `event` now, as the standing browse hands it the broker's.
fn hears(watch: &Watch, event: &Event) {
    watch.watched.hear_event(event, Instant::now());
}

/// A browse its process runs itself feeds the watch as the standing browse does: each start
/// forgets what the last one heard and when it last held a member, and opens a settle window.
/// After it a member heard beside one held, this process's own instance and a nameless event
/// ring nothing, and a member heard after a settle window that heard none rings once.
/// NEGATIVE CONTROL: a start that keeps what the last browse heard still holds a member that
/// went while no browse ran, and one that keeps when the last browse held a member takes the
/// member as heard beside those: either way its return rings nothing.
#[test]
fn a_browse_its_process_runs_feeds_the_watch_as_the_standing_browse_does() {
    let bell = Arc::new(Bell::default());
    let rung = || bell.nap(Duration::ZERO);
    let watch = Watch::idle_for(ALAB_TCP, &bell, &["alab-0000a0a0"]);
    watch.began_browse();
    watch.hear(true, 4, "alab-a1b2c3d4");
    assert_eq!(rung(), Wake::Timer, "heard in the settle window");
    watch.child_began(SETTLE);
    watch.hear(true, 4, "alab-c3d4e5f6");
    assert_eq!(rung(), Wake::Timer, "beside a member it holds");
    watch.hear(true, 4, "alab-0000a0a0");
    watch.hear(true, 4, "");
    assert_eq!(rung(), Wake::Timer, "its own, and no name");
    assert_eq!(
        watch.heard(),
        ["alab-a1b2c3d4", "alab-c3d4e5f6", "alab-0000a0a0"]
    );
    // The connection ended and began again.
    watch.began_browse();
    assert!(watch.heard().is_empty(), "{:?}", watch.heard());
    watch.child_began(SETTLE);
    watch.hear(true, 4, "alab-c3d4e5f6");
    assert_eq!(rung(), Wake::Browse, "heard again on the new connection");
}

/// An `_alab._tcp` watch holds what a browse hears in its settle window as its baseline,
/// ringing nothing for it, nor for a member heard beside it. With nothing held but its own, a
/// member rings once; one it holds, one heard beside it, another service's event and this
/// process's own instance ring nothing, and a removed member is held no more. It holds at
/// most [`MOST_HELD`] members. The vault watch has no settle window and hears no
/// `_alab._tcp` event. Members coming and going are
/// `an_alab_tcp_watch_rings_only_for_a_member_heard_after_every_member_was_silent`'s.
/// NEGATIVE CONTROLS: a watch with no baseline rings in the settle window; one that keeps
/// another service's event rings for it; one that keeps a removed member still holds it; one
/// with no cap holds a 257th; one that gives the vault watch the settle window misses its
/// server.
#[test]
fn an_alab_tcp_watch_holds_a_baseline_and_rings_for_a_member_where_it_held_none() {
    let bell = Arc::new(Bell::default());
    let rung = || bell.nap(Duration::ZERO);
    let settling = Watch::idle_for(ALAB_TCP, &bell, &[]);
    settling.child_began(Duration::ZERO);
    hears(&settling, &added(ALAB_TCP, 4, "alab-a1b2c3d4"));
    assert_eq!(rung(), Wake::Timer, "heard in the settle window");
    assert_eq!(
        settling.heard(),
        ["alab-a1b2c3d4"],
        "a baseline event is held"
    );
    settling.child_began(SETTLE);
    hears(&settling, &added(ALAB_TCP, 4, "alab-c3d4e5f6"));
    assert_eq!(rung(), Wake::Timer, "beside a baseline member");

    let watch = Watch::idle_for(ALAB_TCP, &bell, &["alab-0000a0a0"]);
    watch.child_began(SETTLE);
    hears(&watch, &added(crate::server::SERVICE_TYPE, 4, "m22"));
    assert_eq!(rung(), Wake::Timer, "another service");
    hears(&watch, &added(ALAB_TCP, 4, "alab-0000a0a0"));
    assert_eq!(rung(), Wake::Timer, "this process's own");
    hears(&watch, &added(ALAB_TCP, 5, "ALAB-0000A0A0"));
    assert_eq!(rung(), Wake::Timer, "this process's own (case-insensitive)");
    hears(&watch, &added(ALAB_TCP, 4, "alab-c3d4e5f6"));
    assert_eq!(rung(), Wake::Browse, "a member, where it held none");
    hears(&watch, &added(ALAB_TCP, 4, "alab-c3d4e5f6"));
    assert_eq!(rung(), Wake::Timer, "held since");
    hears(&watch, &added(ALAB_TCP, 4, "alab-a1b2c3d4"));
    assert_eq!(rung(), Wake::Timer, "beside one it holds");
    assert_eq!(
        watch.peer_names(),
        ["alab-a1b2c3d4", "alab-c3d4e5f6"],
        "own instances excluded from peer_names regardless of case"
    );
    hears(&watch, &removed(ALAB_TCP, 5, "ALAB-0000A0A0"));
    hears(&watch, &removed(ALAB_TCP, 4, "alab-c3d4e5f6"));
    assert_eq!(rung(), Wake::Timer, "a removal");
    assert_eq!(watch.heard(), ["alab-0000a0a0", "alab-a1b2c3d4"]);

    let crowd = Watch::idle_for(ALAB_TCP, &bell, &[]);
    for n in 0..MOST_HELD {
        hears(&crowd, &added(ALAB_TCP, 4, &format!("alab-{n:08x}")));
    }
    assert_eq!(rung(), Wake::Browse);
    hears(&crowd, &added(ALAB_TCP, 4, "alab-ffffffff"));
    assert_eq!(rung(), Wake::Timer, "one more than it holds");
    assert_eq!(crowd.heard().len(), MOST_HELD);
    hears(&crowd, &removed(ALAB_TCP, 4, "alab-00000000"));
    hears(&crowd, &added(ALAB_TCP, 4, "alab-ffffffff"));
    assert_eq!(rung(), Wake::Timer, "beside the members it holds");
    assert_eq!(crowd.heard().len(), MOST_HELD, "room again");
    assert_eq!(
        crowd.heard().last().map(String::as_str),
        Some("alab-ffffffff")
    );

    let vault = Watch::idle(&bell, &[]);
    vault.child_began(Duration::ZERO);
    hears(&vault, &added(crate::server::SERVICE_TYPE, 4, "m3"));
    assert_eq!(rung(), Wake::Browse, "the vault watch has no settle window");
    hears(&vault, &added(ALAB_TCP, 4, "alab-a1b2c3d4"));
    assert_eq!(
        rung(),
        Wake::Timer,
        "the vault watch hears no _alab._tcp event"
    );
    assert_eq!(vault.heard(), ["m3"]);
}

/// A peer's comings and goings on `_alab._tcp` are no move of this Mac's network (each
/// process names its advert afresh, so a peer's window or node job starting again is a name
/// never heard): while the watch holds a member, one never heard, one back after leaving for
/// longer than [`SILENCE`], and one heard on another interface ring nothing, over an hour
/// and a half of such churn. A member heard once every member but this process's own was
/// silent for longer than [`SILENCE`] (this Mac back on a LAN) rings once, and the next one
/// heard with it nothing.
/// NEGATIVE CONTROLS: a member heard after a silence no longer than [`SILENCE`] rings
/// nothing, and nor does this process's own instance, nor an event in the settle window.
/// MUTANTS: the rule before this one (each member not held rings unless back within the
/// silence) fails "never heard"; taking the watch as alone whenever it holds no member, with
/// no silence, or noting when it held one only at adds fails "back within the silence";
/// noting it at every event after which it holds one, not at the last member's removal,
/// fails "within the silence of the last member's removal"; taking it as alone once it heard
/// no event for the silence, though it holds a member, fails "back after it left"; counting
/// this process's own instance as a member fails "after every member's silence".
#[test]
fn an_alab_tcp_watch_rings_only_for_a_member_heard_after_every_member_was_silent() {
    let secs = Duration::from_secs;
    let silence = SILENCE;
    let bell = Arc::new(Bell::default());
    let rung = || bell.nap(Duration::ZERO);
    let watch = Watch::idle_for(ALAB_TCP, &bell, &["alab-0000a0a0"]);
    let start = Instant::now();
    watch.child_began(Duration::ZERO);
    watch.hear_at(true, 4, "alab-00000000", start);
    assert_eq!(rung(), Wake::Timer, "heard in the settle window");
    watch.child_began(SETTLE);
    watch.hear_at(true, 4, "alab-0000a0a0", start);
    let mut at = start;
    for i in 1..40_u32 {
        let name = format!("alab-{i:08x}");
        at += secs(31);
        watch.hear_at(true, 4, &name, at);
        assert_eq!(rung(), Wake::Timer, "never heard: {name}");
        watch.hear_at(false, 4, &name, at + secs(1));
        at += silence + secs(2);
        watch.hear_at(true, 4, &name, at);
        assert_eq!(rung(), Wake::Timer, "back after it left: {name}");
        watch.hear_at(true, 5, &name, at);
        assert_eq!(rung(), Wake::Timer, "on another interface: {name}");
    }
    assert!(at - start > secs(90 * 60), "{:?}", at - start);
    assert_eq!(watch.heard().len(), 41, "{:?}", watch.heard());

    let left = at + secs(1);
    for name in watch.heard() {
        for interface in [4, 5] {
            watch.hear_at(false, interface, &name, left);
        }
    }
    assert!(watch.heard().is_empty(), "{:?}", watch.heard());
    watch.hear_at(true, 4, "alab-00000000", left + silence);
    assert_eq!(rung(), Wake::Timer, "back within the silence");
    let left = left + silence + secs(1);
    watch.hear_at(false, 4, "alab-00000000", left);
    watch.hear_at(true, 4, "alab-0badcafe", left + silence);
    assert_eq!(
        rung(),
        Wake::Timer,
        "within the silence of the last member's removal"
    );
    let left = left + silence;
    watch.hear_at(false, 4, "alab-0badcafe", left);
    let back = left + silence + secs(1);
    watch.hear_at(true, 4, "alab-0000a0a0", back);
    assert_eq!(rung(), Wake::Timer, "its own, after every member's silence");
    watch.hear_at(true, 4, "alab-feedface", back);
    assert_eq!(rung(), Wake::Browse, "after every member's silence");
    watch.hear_at(true, 4, "alab-00000000", back);
    assert_eq!(rung(), Wake::Timer, "one heard with it");
    assert_eq!(
        watch.heard(),
        ["alab-0000a0a0", "alab-feedface", "alab-00000000"]
    );
}

/// `adverts` resolves each member the watch holds but this process's own, in first-heard
/// order, leaving out one that does not answer; a watch with no resolver resolves nothing in
/// a test build.
/// NEGATIVE CONTROL: an `adverts` that resolves this process's own instance fails the list.
#[test]
fn adverts_resolve_each_member_held_but_this_process_own() {
    let bell = Arc::new(Bell::default());
    let watch = Watch::idle_resolving(ALAB_TCP, &bell, &["alab-0000a0a0"], |name| {
        (name != "alab-deadbeef").then(|| Advert {
            instance: name.to_owned(),
            host: format!("{name}.local"),
            port: 7100,
            txt: Vec::new(),
        })
    });
    for name in [
        "alab-c3d4e5f6",
        "alab-0000a0a0",
        "alab-deadbeef",
        "alab-a1b2c3d4",
    ] {
        hears(&watch, &added(ALAB_TCP, 4, name));
    }
    let adverts = watch.adverts(Duration::from_secs(1));
    assert_eq!(
        adverts
            .iter()
            .map(|advert| advert.instance.as_str())
            .collect::<Vec<_>>(),
        ["alab-c3d4e5f6", "alab-a1b2c3d4"]
    );
    let plain = Watch::idle_for(ALAB_TCP, &bell, &[]);
    hears(&plain, &added(ALAB_TCP, 4, "alab-c3d4e5f6"));
    assert!(plain.adverts(Duration::from_secs(1)).is_empty());
}

/// `advert_of` reads an `_alab._tcp` resolve's answer: the host lowercase without its
/// trailing dot, the fleet port it registered and its TXT pairs (a string with no `=`, an
/// empty key or no UTF-8 left out), under the instance name the browse heard. It refuses an
/// answer with no usable target, and any other event.
/// NEGATIVE CONTROL: an `advert_of` that keeps a string with no `=` reads `"bare"` as a pair.
#[test]
fn advert_of_reads_an_alab_tcp_resolve_and_refuses_one_with_no_target() {
    let mut answer = resolved(
        "M3.local.",
        7100,
        &[
            "v=1",
            "role=steward",
            "bare",
            "=x",
            "fleet=7100",
            "window=1",
        ],
    );
    if let Event::Resolved { txt, .. } = &mut answer {
        txt.push(vec![b'k', b'=', 0xff]);
    }
    assert_eq!(
        advert_of("alab-1a2b3c4d", &answer),
        Some(Advert {
            instance: "alab-1a2b3c4d".to_owned(),
            host: "m3.local".to_owned(),
            port: 7100,
            txt: [
                ("v", "1"),
                ("role", "steward"),
                ("fleet", "7100"),
                ("window", "1")
            ]
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect(),
        })
    );
    let spaced = advert_of("m3 (2)", &resolved("m3-2.local.", 7101, &["role=member"]))
        .expect("the spaced name's advert");
    assert_eq!(
        (spaced.instance.as_str(), spaced.host.as_str(), spaced.port),
        ("m3 (2)", "m3-2.local", 7101)
    );
    assert_eq!(advert_of("x", &resolved("m3.local.", 0, &[])), None);
    assert_eq!(advert_of("x", &resolved("-m3.local.", 7100, &[])), None);
    assert_eq!(advert_of("x", &added(ALAB_TCP, 4, "x")), None);
}

#[test]
fn browse_and_resolve_read_active_instances_and_order_txt_hints_behind_defaults() {
    let vault = crate::server::SERVICE_TYPE;
    let browse = [
        added(vault, 15, "gone-node"),
        added(vault, 4, "alab-secrets"),
        added(vault, 9, "m3"),
        added(vault, 4, "m7"),
        removed(vault, 15, "gone-node"),
    ];
    assert_eq!(
        heard_instances(browse, vault),
        vec!["alab-secrets", "m3", "m7"]
    );

    let signer = crate::b64::decode_padded(crate::ALAB_SIGNER).expect("signer");
    let signer = <[u8; 32]>::try_from(signer.as_slice()).expect("32 bytes");
    let fp = envelope::fingerprint(&signer);
    let head = "ab".repeat(32);
    let signed = [
        "path=/v1".to_owned(),
        "vault=alab".to_owned(),
        format!("signer={fp}"),
        "seq=19".to_owned(),
        format!("head={head}"),
    ];
    let signed: Vec<&str> = signed.iter().map(String::as_str).collect();
    let resolved_m3 =
        resolved_vault(&resolved("m3.local.", 7484, &signed), "alab").expect("m3's answer");
    assert_eq!(resolved_m3.url, "http://m3.local:7484/v1/alab");
    assert_eq!(resolved_m3.txt.as_ref().map(|t| t.seq), Some(19));
    let resolved_m7 =
        resolved_vault(&resolved("M7.local.", 7484, &["path=/v1"]), "alab").expect("m7's answer");
    assert_eq!(resolved_m7.url, "http://m7.local:7484/v1/alab");
    assert_eq!(resolved_m7.txt, None);
    let team: Vec<&str> = signed
        .iter()
        .map(|string| {
            if *string == "vault=alab" {
                "vault=team"
            } else {
                string
            }
        })
        .collect();
    assert_eq!(
        resolved_vault(&resolved("other.local.", 7484, &team), "alab"),
        None,
        "another vault"
    );
    assert_eq!(
        resolved_vault(&resolved("other.local.", 7484, &["path=/v2"]), "alab"),
        None,
        "another path"
    );
    let resolved = [resolved_m7, resolved_m3];
    let extra = DiscoveredVault {
        url: "http://m9.local:7484/v1/alab".into(),
        txt: resolved[1].txt.clone().map(|mut t| {
            t.seq = 42;
            t
        }),
    };
    let duplicate_default = DiscoveredVault {
        url: crate::ALAB_DEFAULT_URL
            .to_uppercase()
            .replace("V1/ALAB", "v1/alab"),
        txt: resolved[1].txt.clone(),
    };
    let urls = candidate_urls(
        crate::ALAB_DEFAULT_URLS,
        &[
            resolved[0].clone(),
            resolved[1].clone(),
            extra,
            duplicate_default,
        ],
        "alab",
    );
    assert_eq!(
        urls,
        vec![
            crate::ALAB_DEFAULT_URL.to_owned(),
            "http://m9.local:7484/v1/alab".to_owned(),
            "http://m3.local:7484/v1/alab".to_owned(),
            "http://m7.local:7484/v1/alab".to_owned(),
        ]
    );
}

/// About 35 Macs serve, so a server hears about 34 others and a member about 35. Each lookup
/// selects every stable heard name within three passes, including a laptop that rejoined
/// after its name expired and was heard last. Selection does not establish a response.
/// NEGATIVE CONTROL: selecting only the first `MAX_CANDIDATES` heard on every pass would
/// leave 19 of 35 never selected.
#[test]
fn every_heard_server_is_selected_within_three_bounded_passes() {
    let bell = Arc::new(Bell::default());
    let watch = Watch::idle(&bell, &["m00"]);
    for n in 0..36 {
        watch.hear(true, 4, &format!("m{n:02}"));
    }
    let (mut peers, mut found) = (
        std::collections::BTreeSet::new(),
        std::collections::BTreeSet::new(),
    );
    for _ in 0..3 {
        let named = watch.peer_names();
        assert!(named.len() <= MAX_CANDIDATES, "{named:?}");
        assert!(!named.iter().any(|name| name == "m00"), "its own instance");
        peers.extend(named);
        let named = watch.found_names();
        assert!(named.len() <= MAX_CANDIDATES, "{named:?}");
        found.extend(named);
    }
    assert_eq!(peers.len(), 35, "{peers:?}");
    assert_eq!(found.len(), 36, "{found:?}");

    let alab_watch = Watch::idle(&bell, &["ALAB-SECRETS"]);
    alab_watch.hear(true, 4, "alab-secrets");
    assert_eq!(
        alab_watch.peer_names(),
        vec!["alab-secrets"],
        "alab-secrets remains in peer_names regardless of own-note case"
    );
}

#[test]
fn every_holder_is_queried_within_bounded_passes_independently_of_other_lookups() {
    for count in [0usize, 1, MAX_CANDIDATES, MAX_CANDIDATES + 1, 35] {
        let bell = Arc::new(Bell::default());
        let watch = Watch::idle(&bell, &["alab-secrets"]);
        let mut expected = std::collections::BTreeSet::new();
        for index in 0..count {
            let name = if index == 0 {
                "alab-secrets".to_owned()
            } else {
                format!("alab-secrets ({})", index + 1)
            };
            watch.hear(true, 1, &name);
            expected.insert(name);
            watch.hear(true, 1, &format!("ordinary-vault-{index}"));
        }
        let mut queried = std::collections::BTreeSet::new();
        for _ in 0..count.div_ceil(MAX_CANDIDATES).max(1) {
            // These consumers have independent progress, even on the same watch.
            let _ = watch.found_names();
            let _ = watch.found_names();
            let _ = watch.peer_names();
            let names = watch.holder_names();
            assert!(names.len() <= MAX_CANDIDATES);
            assert!(names.iter().all(|name| is_alab_holder_instance(name)));
            queried.extend(names);
        }
        assert_eq!(queried, expected, "holder count {count}");
    }

    let bell = Arc::new(Bell::default());
    let members = Watch::idle_for(MEMBER_SERVICE, &bell, &[]);
    members.hear(true, 1, "alab-secrets");
    assert!(members.holder_names().is_empty());
}

#[test]
fn raw_browse_order_does_not_cancel_candidate_rotation() {
    let defaults = candidate_urls(crate::ALAB_DEFAULT_URLS, &[], crate::ALAB_VAULT);
    let limit = MAX_CANDIDATES - defaults.len();
    assert!(limit > 0);
    let names: Vec<_> = (0..35).map(|n| format!("m{n:02}")).collect();
    let expected: std::collections::BTreeSet<_> = names
        .iter()
        .map(|name| format!("http://{name}.local:7484/v1/alab"))
        .collect();
    let from = AtomicUsize::new(0);
    let mut queried = std::collections::BTreeSet::new();
    for pass in 0..names.len().div_ceil(limit) {
        // A restarted browse can report the same live servers in an order
        // that cancels a cursor's progress through first-heard names.
        let mut arrivals = names.clone();
        arrivals.rotate_right((pass * limit) % names.len());
        let browse = arrivals
            .iter()
            .map(|name| added(crate::server::SERVICE_TYPE, 4, name));
        let heard = heard_instances(browse, crate::server::SERVICE_TYPE);
        assert_eq!(heard, arrivals, "the fold preserves event order");
        let discovered: Vec<_> = alab_candidate_names(heard, &from)
            .into_iter()
            .map(|name| DiscoveredVault {
                url: format!("http://{name}.local:7484/v1/alab"),
                txt: None,
            })
            .collect();
        let urls = candidate_urls(crate::ALAB_DEFAULT_URLS, &discovered, crate::ALAB_VAULT);
        assert!(urls.len() <= MAX_CANDIDATES);
        assert!(urls.starts_with(&defaults));
        queried.extend(urls.into_iter().filter(|url| expected.contains(url)));
    }
    assert_eq!(queried, expected);
}

#[test]
fn restarted_browse_order_does_not_cancel_watch_rotation() {
    type Select = fn(&Watch) -> Vec<String>;
    let names: Vec<_> = (0..35)
        .map(|n| {
            if n == 0 {
                "alab-secrets".to_owned()
            } else {
                format!("alab-secrets ({})", n + 1)
            }
        })
        .collect();
    let expected: std::collections::BTreeSet<_> = names.iter().cloned().collect();
    let found_limit =
        MAX_CANDIDATES - candidate_urls(crate::ALAB_DEFAULT_URLS, &[], crate::ALAB_VAULT).len();
    let mut coverage = Vec::new();
    for (kind, select, limit) in [
        ("found", Watch::found_names as Select, found_limit),
        ("peers", Watch::peer_names as Select, MAX_CANDIDATES),
        ("holders", Watch::holder_names as Select, MAX_CANDIDATES),
    ] {
        let bell = Arc::new(Bell::default());
        let watch = Watch::idle(&bell, &[]);
        let mut queried = std::collections::BTreeSet::new();
        for pass in 0..names.len().div_ceil(limit) {
            let mut arrivals = names.clone();
            arrivals.rotate_right((pass * limit) % names.len());
            watch.began_browse();
            for name in arrivals {
                watch.hear(true, 4, &name);
            }
            let selected = select(&watch);
            assert!(selected.len() <= limit);
            assert!(selected.iter().all(|name| expected.contains(name)));
            queried.extend(selected);
        }
        coverage.push((kind, queried.len()));
    }
    assert_eq!(coverage, [("found", 35), ("peers", 35), ("holders", 35)]);
}

#[test]
fn default_urls_do_not_starve_discovered_servers_at_the_final_candidate_limit() {
    let defaults = candidate_urls(crate::ALAB_DEFAULT_URLS, &[], crate::ALAB_VAULT);
    assert!(!defaults.is_empty() && defaults.len() < MAX_CANDIDATES);
    for use_watch in [true, false] {
        for count in [16usize, 32, 35] {
            let bell = Arc::new(Bell::default());
            let watch = Watch::idle(&bell, &[]);
            let browse_from = AtomicUsize::new(0);
            let mut expected = std::collections::BTreeSet::new();
            for index in 0..count {
                let name = format!("m{index:02}");
                watch.hear(true, 1, &name);
                expected.insert(format!("http://{name}.local:7484/v1/alab"));
            }
            let mut queried = std::collections::BTreeSet::new();
            for _ in 0..3 {
                let names = if use_watch {
                    watch.found_names()
                } else {
                    alab_candidate_names(watch.heard(), &browse_from)
                };
                let discovered: Vec<_> = names
                    .into_iter()
                    .map(|name| DiscoveredVault {
                        url: format!("http://{name}.local:7484/v1/alab"),
                        txt: None,
                    })
                    .collect();
                let urls = candidate_urls(crate::ALAB_DEFAULT_URLS, &discovered, crate::ALAB_VAULT);
                assert!(urls.len() <= MAX_CANDIDATES);
                assert!(urls.starts_with(&defaults));
                queried.extend(urls.into_iter().filter(|url| expected.contains(url)));
            }
            assert_eq!(queried, expected, "servers={count}, watch={use_watch}");
        }
    }
}

/// The standing browse's own thread ([`browse_standing`]), run against a stand-in LAN broker in
/// a scratch folder (`aterm_uds::lan::seam_here`): no test reaches the person's broker or the LAN.
/// Each test drops its watch in one of the thread's waits and sees the thread end within the
/// hang detector.
#[cfg(target_os = "macos")]
mod standing {
    use super::*;
    use aterm_spec::wait::MUST_HAPPEN;
    use aterm_uds::lan::wire::{self, Frame, Reader, Request};
    use std::path::{Path, PathBuf};
    use std::sync::mpsc;

    /// What the stand-in does with one connection it accepts.
    #[derive(Clone, Debug)]
    enum Answer {
        /// Refuses this process before its hello, and closes.
        Refuse,
        /// Says it could not judge this process, and closes.
        Unchecked,
        /// Says its hello, tells a request it is then sent, streams `events` and holds the
        /// request open until the client ends it (`hold`), or closes.
        Hello { events: Vec<Event>, hold: bool },
        /// As `Hello` that holds its request, but streams `events` only once its test sends on
        /// `go` (within the hang detector).
        Later {
            events: Vec<Event>,
            go: Arc<Mutex<mpsc::Receiver<()>>>,
        },
    }

    /// What the stand-in tells its test.
    #[derive(Debug, PartialEq, Eq)]
    enum Told {
        /// A request came.
        Asked(Request),
        /// A client ended a request held open.
        Ended,
    }

    /// A stand-in broker and what it tells.
    struct StandIn {
        told: mpsc::Receiver<Told>,
        accepted: Arc<AtomicUsize>,
    }

    impl StandIn {
        /// The connections it accepted so far.
        fn accepted(&self) -> usize {
            self.accepted.load(Ordering::SeqCst)
        }

        /// The browse request it was sent next.
        fn browsed(&self) -> bool {
            matches!(
                self.told.recv_timeout(MUST_HAPPEN),
                Ok(Told::Asked(Request::Browse { .. }))
            )
        }
    }

    /// A stand-in broker at `socket`, answering the connections it accepts in turn as `answers`
    /// say, on a thread of its own. Its socket is bound beside the socket's folder and moved into
    /// it whole, so a browse that waits on that folder hears one change, of a socket that
    /// already listens.
    fn serve(socket: &Path, answers: Vec<Answer>) -> StandIn {
        let bound = socket.parent().unwrap().with_file_name("bound.sock");
        let listener = aterm_uds::CtlListener::bind(&bound).unwrap();
        std::fs::rename(&bound, socket).unwrap();
        let (tell, told) = mpsc::channel();
        let accepted = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&accepted);
        std::thread::spawn(move || {
            for answer in answers {
                let Ok((stream, _)) = aterm_uds::accept(
                    &listener,
                    aterm_uds::AcceptWait::Until(Instant::now() + MUST_HAPPEN),
                    aterm_uds::AcceptedMode::Blocking,
                ) else {
                    return;
                };
                counted.fetch_add(1, Ordering::SeqCst);
                answered(&stream, answer, &tell);
            }
        });
        StandIn { told, accepted }
    }

    /// Answer one connection as `answer` says.
    fn answered(stream: &aterm_uds::CtlStream, answer: Answer, tell: &mpsc::Sender<Told>) {
        let (events, hold, go) = match answer {
            Answer::Refuse => {
                let words = "a stand-in that refuses".to_owned();
                let _ = wire::send(stream, &Frame::Refused { words });
                return;
            }
            Answer::Unchecked => {
                let words = "a stand-in that could not check".to_owned();
                let _ = wire::send(stream, &Frame::Unchecked { words });
                return;
            }
            Answer::Hello { events, hold } => (events, hold, None),
            Answer::Later { events, go } => (events, true, Some(go)),
        };
        let hello = Frame::Hello {
            protocol: aterm_uds::lan::PROTOCOL,
            build: "stand-in".to_owned(),
        };
        let mut reader = Reader::new(0);
        let deadline = Some(Instant::now() + MUST_HAPPEN);
        if wire::send(stream, &hello).is_err()
            || !matches!(
                reader.frame(stream, deadline),
                Ok(Some(Frame::Hello { .. }))
            )
        {
            return;
        }
        // A hello alone (an arrival's) is closed by its client.
        let Ok(Some(Frame::Request(request))) = reader.frame(stream, deadline) else {
            return;
        };
        let _ = tell.send(Told::Asked(request));
        if let Some(go) = go {
            let go = go.lock().unwrap_or_else(PoisonError::into_inner);
            if go.recv_timeout(MUST_HAPPEN).is_err() {
                return;
            }
        }
        for event in events {
            if wire::send(stream, &Frame::Event(event)).is_err() {
                return;
            }
        }
        if hold && matches!(reader.frame(stream, None), Ok(None)) {
            let _ = tell.send(Told::Ended);
        }
    }

    /// A scratch folder for a broker's socket (under this test binary's short root, so the path
    /// fits `sun_path`), the socket's path in it, and the LAN seam that points a thread's client
    /// at that socket.
    fn scratch() -> (aterm_tempfile::TempDir, PathBuf, aterm_uds::lan::Seam) {
        let short = aterm_uds::test_binary::short_root().expect("a test binary's short root");
        let root = aterm_tempfile::Builder::new().tempdir_in(short).unwrap();
        let folder = root.path().join("broker");
        std::fs::create_dir(&folder).unwrap();
        let socket = folder.join("b.sock");
        let seam = aterm_uds::lan::Seam {
            socket: Some(socket.clone()),
            ..aterm_uds::lan::Seam::default()
        };
        (root, socket, seam)
    }

    /// A socket at `socket` that nothing listens on, as a broker that went leaves its own: bound
    /// beside the folder, its listener dropped, and moved in whole.
    fn left_behind(socket: &Path) {
        let bound = socket.parent().unwrap().with_file_name("bound.sock");
        drop(aterm_uds::CtlListener::bind(&bound).unwrap());
        std::fs::rename(&bound, socket).unwrap();
    }

    /// A restart backoff longer than the hang detector: a test whose browse waited it out would
    /// fail that detector.
    const LONG: Duration = Duration::from_secs(600);

    /// A vault watch whose standing browse runs against the broker `seam` names, its first
    /// restart backoff `restart_first`, and what says its thread ended.
    fn browsing_after(
        bell: &Arc<Bell>,
        seam: aterm_uds::lan::Seam,
        restart_first: Duration,
    ) -> (Watch, mpsc::Receiver<()>) {
        let (ended, end) = mpsc::channel();
        let watch = Watch::browsing(
            crate::server::SERVICE_TYPE,
            bell,
            &[],
            restart_first,
            move |browse| {
                aterm_uds::lan::seam_here(seam, browse);
                let _ = ended.send(());
            },
        );
        (watch, end)
    }

    /// [`browsing_after`] with the production backoff.
    fn browsing(bell: &Arc<Bell>, seam: aterm_uds::lan::Seam) -> (Watch, mpsc::Receiver<()>) {
        browsing_after(bell, seam, RESTART_FIRST)
    }

    /// The waits `watch`'s standing browse made, once `enough` says they are (asked at each new
    /// one), within the hang detector.
    fn waits_once(watch: &Watch, enough: impl Fn(&[Waited]) -> bool) -> Vec<Waited> {
        let deadline = Instant::now() + MUST_HAPPEN;
        let mut waits = watch
            .watched
            .waits
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        while !enough(&waits) {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "the browse's waits stayed {:?}", *waits);
            waits = watch
                .watched
                .waited
                .wait_timeout(waits, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        waits.clone()
    }

    /// Drop `watch` and see its thread end within the hang detector.
    fn ends(watch: Watch, end: &mpsc::Receiver<()>) {
        drop(watch);
        end.recv_timeout(MUST_HAPPEN)
            .expect("the browse's thread ends with its watch");
    }

    /// A hello alone, closed by its client: an arrival's.
    const ARRIVAL: Answer = Answer::Hello {
        events: Vec::new(),
        hold: false,
    };

    /// A running standing browse waits for each frame with no timeout handed to poll(2), beside
    /// its watch's pipe, and hands each event to the watch, which rings its bell; dropping the
    /// watch while it waits ends the thread at once and, with it, the broker request. The
    /// broker's arrival is asked its hello before the browse, and the broker sends its event
    /// only once the browse waits for it, so the browse waits once before it and once after.
    ///
    /// NEGATIVE CONTROLS: a frame wait handed a deadline (`Some(Duration::from_secs(1))` for
    /// `None` in `Standing::hear`, the 1 s poll of before) is noted `Waited(Frames, Some(1 s))`
    /// and fails the first assertion on the waits; one without the pipe beside it
    /// (`events.next()` for the wait and `next_beside`) notes no wait, and a watch that keeps its
    /// pipe's write end once dropped (`mem::forget` in `Drop`) never ends the thread: the hang
    /// detector fails each.
    #[test]
    fn a_standing_browse_waits_for_frames_with_no_deadline() {
        let (_root, socket, seam) = scratch();
        let (go, gone) = mpsc::channel();
        let browse = Answer::Later {
            events: vec![added(crate::server::SERVICE_TYPE, 4, "m3")],
            go: Arc::new(Mutex::new(gone)),
        };
        let broker = serve(&socket, vec![ARRIVAL, browse]);
        let bell = Arc::new(Bell::default());
        let (watch, end) = browsing(&bell, seam);
        assert!(broker.browsed());
        assert_eq!(
            waits_once(&watch, |waits| !waits.is_empty()),
            [Waited(Why::Frames, None)],
            "a wait for the first frame, with no timeout"
        );
        go.send(()).unwrap();
        assert_eq!(bell.nap(MUST_HAPPEN), Wake::Browse, "a server heard");
        assert_eq!(watch.heard(), ["m3"]);
        assert_eq!(
            waits_once(&watch, |waits| waits.len() == 2),
            [Waited(Why::Frames, None); 2],
            "and one for the next, with no timeout"
        );
        ends(watch, &end);
        assert_eq!(
            broker.told.recv_timeout(MUST_HAPPEN),
            Ok(Told::Ended),
            "the request ended with the watch"
        );
    }

    /// A standing browse that found no broker waits, with no timeout handed to poll(2), for one
    /// to arrive at its socket, and browses as soon as one answers its checked hello.
    ///
    /// NEGATIVE CONTROLS: a broker wait handed a timeout of its own where the folder is watched
    /// (`Some(retry.min(BROKER_PAUSE))` for `(retry != Duration::MAX).then_some(retry)` in
    /// `Standing::wait_for_broker`, a 2 s timer again) is noted `Waited(Broker, Some(2 s))` and
    /// fails the first assertion; so is the broker's absence waited on `BROKER_PAUSE` alone
    /// (`Ok(false) => None`).
    #[test]
    fn a_broker_that_arrives_is_browsed_without_a_pause() {
        let (_root, socket, seam) = scratch();
        let bell = Arc::new(Bell::default());
        let (watch, end) = browsing(&bell, seam);
        assert_eq!(
            waits_once(&watch, |waits| !waits.is_empty()),
            [Waited(Why::Broker, None)],
            "no broker: a wait for one, with no timeout"
        );
        let browse = Answer::Hello {
            events: Vec::new(),
            hold: true,
        };
        let broker = serve(&socket, vec![ARRIVAL, browse]);
        assert!(broker.browsed());
        assert_eq!(
            waits_once(&watch, |waits| waits.len() == 2),
            [Waited(Why::Broker, None), Waited(Why::Frames, None)]
        );
        assert_eq!(broker.accepted(), 2, "its hello, then the browse");
        ends(watch, &end);
    }

    /// A broker there that refuses this process is asked its hello once, and the browse once,
    /// and nothing more until its socket is made again: the browse waits with no timeout, and
    /// dropping the watch ends that wait at once.
    ///
    /// NEGATIVE CONTROL: a wait that browses again whenever the broker's arrival says nothing new
    /// (`Ok(false) => return false` in `Standing::wait_for_broker`) asks the refusing broker
    /// without end and never waits, and the hang detector fails the first assertion.
    #[test]
    fn a_refusing_broker_is_asked_nothing_more_until_its_socket_changes() {
        let (_root, socket, seam) = scratch();
        let broker = serve(&socket, vec![Answer::Refuse; 64]);
        let bell = Arc::new(Bell::default());
        let (watch, end) = browsing(&bell, seam);
        assert_eq!(
            waits_once(&watch, |waits| !waits.is_empty()),
            [Waited(Why::Broker, None)]
        );
        assert_eq!(broker.accepted(), 2, "the arrival's hello, then the browse");
        ends(watch, &end);
        assert_eq!(broker.accepted(), 2, "nothing more");
    }

    /// A broker whose hello did not judge this process (its code check did not finish) is asked
    /// again on the arrivals' own backoff (the first a second off), and browsed once it answers:
    /// its hello did not judge, its browse refused, its next hello answered. Each wait for the
    /// broker before then has the rest of that second as its timeout; where the browse's own
    /// exchange outlasted the second (a loaded Mac), the hello is asked again before any wait.
    ///
    /// NEGATIVE CONTROL: a wait with no timeout whatever that backoff says (`retry =
    /// Duration::MAX`) never asks again, and the hang detector fails it.
    #[test]
    fn a_broker_that_did_not_judge_this_process_is_asked_again_on_its_backoff() {
        let (_root, socket, seam) = scratch();
        let answers = vec![
            Answer::Unchecked,
            Answer::Refuse,
            ARRIVAL,
            Answer::Hello {
                events: Vec::new(),
                hold: true,
            },
        ];
        let broker = serve(&socket, answers);
        let bell = Arc::new(Bell::default());
        let (watch, end) = browsing(&bell, seam);
        assert!(broker.browsed());
        let waits = waits_once(&watch, |waits| {
            waits.last() == Some(&Waited(Why::Frames, None))
        });
        let (browsed, before) = waits.split_last().expect("a wait");
        assert_eq!(*browsed, Waited(Why::Frames, None));
        assert!(
            before.iter().all(|waited| matches!(
                waited,
                Waited(Why::Broker, Some(retry)) if *retry <= Duration::from_secs(1)
            )),
            "{waits:?}"
        );
        assert_eq!(broker.accepted(), 4);
        ends(watch, &end);
    }

    /// A browse its broker could not answer (it did not judge this process) waits out the
    /// restart backoff, beside the broker's arrivals: a broker whose socket is made again
    /// meanwhile is browsed at once, though the backoff is longer than the hang detector.
    ///
    /// NEGATIVE CONTROL: a backoff waited beside the pipe alone (`others` left empty and no
    /// `arrivals.poll()` in `Standing::restart_after`) browses the new broker only once the 600 s
    /// pass, and the hang detector fails it.
    #[test]
    fn a_broker_made_again_during_a_restart_backoff_is_browsed_at_once() {
        let (_root, socket, seam) = scratch();
        serve(&socket, vec![ARRIVAL, Answer::Unchecked]);
        let bell = Arc::new(Bell::default());
        let (watch, end) = browsing_after(&bell, seam, LONG);
        assert_eq!(
            waits_once(&watch, |waits| !waits.is_empty()),
            [Waited(Why::Restart, Some(LONG))],
            "the browse it could not answer: the backoff"
        );
        let browse = Answer::Hello {
            events: Vec::new(),
            hold: true,
        };
        let again = serve(&socket, vec![ARRIVAL, browse]);
        assert!(again.browsed(), "the broker made again, browsed");
        assert_eq!(
            waits_once(&watch, |waits| waits.len() == 2),
            [Waited(Why::Restart, Some(LONG)), Waited(Why::Frames, None)]
        );
        ends(watch, &end);
    }

    /// A socket nothing listens on (a broker that went leaves its socket behind) waits for the
    /// broker's next generation, not out the restart backoff, and that generation is browsed as
    /// soon as it answers: its hello is asked again on the arrivals' own backoff (1 s doubling
    /// to 30 s) while the socket stays, and at once when the socket is made again.
    ///
    /// NEGATIVE CONTROL: a refusing socket taken for a broker that could not answer (step 3's
    /// `broker_settled`: `NotInstalled | NotRunning` with the socket there not waiting for a
    /// broker) waits out the 600 s backoff, noted `Waited(Restart, ..)`, and fails the first
    /// assertion.
    #[test]
    fn a_socket_nothing_listens_on_waits_for_the_next_generation() {
        let (_root, socket, seam) = scratch();
        left_behind(&socket);
        let bell = Arc::new(Bell::default());
        let (watch, end) = browsing_after(&bell, seam, LONG);
        let waits = waits_once(&watch, |waits| !waits.is_empty());
        assert!(
            matches!(waits[0], Waited(Why::Broker, Some(retry)) if retry <= Duration::from_secs(30)),
            "{waits:?}"
        );
        let browse = Answer::Hello {
            events: Vec::new(),
            hold: true,
        };
        let broker = serve(&socket, vec![ARRIVAL, browse]);
        assert!(broker.browsed(), "the broker made again, browsed");
        ends(watch, &end);
    }

    /// A direct path whose allowance is read from the file `record` names (allowed once it says
    /// `allow`), and whose server answers a browse as the stand-in broker does, telling `told`.
    struct AllowedWhenWritten {
        record: std::sync::OnceLock<PathBuf>,
        told: Mutex<Option<mpsc::Sender<Told>>>,
    }

    impl aterm_uds::lan::direct::Direct for AllowedWhenWritten {
        fn allowed(&self) -> Result<String, String> {
            let record = self.record.get().ok_or("no record")?;
            match std::fs::read_to_string(record) {
                Ok(said) if said.trim() == "allow" => Ok("the record's Allow".to_owned()),
                _ => Err("no Allow in the record".to_owned()),
            }
        }

        fn record(&self) -> Option<PathBuf> {
            self.record.get().cloned()
        }

        fn serve(&self, stream: aterm_uds::CtlStream) {
            let tell = self
                .told
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            if let Some(tell) = tell {
                let browse = Answer::Hello {
                    events: Vec::new(),
                    hold: true,
                };
                answered(&stream, browse, &tell);
            }
        }
    }

    /// With no broker, an Allow macOS records for this process while its browse waits is heard
    /// through the record's file, and the browse asked again goes through the direct path it
    /// opened. The Allow is written once the wait was asked again after the watch's first look,
    /// in a folder apart from the socket's, so only the record's own events can bring the browse.
    ///
    /// NEGATIVE CONTROL: a broker wait that does not watch the record
    /// (`lan::direct::record().filter(|_| false)?` in `Allowance::watch`) waits on for a broker,
    /// and the hang detector fails the first assertion (only one wait is ever noted).
    #[test]
    fn an_allowance_given_while_no_broker_answers_is_browsed_through_the_direct_path() {
        static DIRECT: AllowedWhenWritten = AllowedWhenWritten {
            record: std::sync::OnceLock::new(),
            told: Mutex::new(None),
        };
        let (root, _socket, seam) = scratch();
        let prefs = root.path().join("prefs");
        std::fs::create_dir(&prefs).unwrap();
        let record = prefs.join("networkextension.plist");
        std::fs::write(&record, "ask\n").unwrap();
        DIRECT.record.set(record.clone()).unwrap();
        let (tell, told) = mpsc::channel();
        *DIRECT.told.lock().unwrap_or_else(PoisonError::into_inner) = Some(tell);
        let bell = Arc::new(Bell::default());
        let (ended, end) = mpsc::channel();
        let watch = Watch::browsing(
            crate::server::SERVICE_TYPE,
            &bell,
            &[],
            RESTART_FIRST,
            move |browse| {
                aterm_uds::lan::direct::seam_here(&DIRECT, || {
                    aterm_uds::lan::seam_here(seam, browse);
                });
                let _ = ended.send(());
            },
        );
        let waits = waits_once(&watch, |waits| waits.len() >= 2);
        assert!(
            waits
                .iter()
                .all(|waited| *waited == Waited(Why::Broker, None)),
            "no broker and no Allow: a wait with no timeout, the browse asked again at the \
             watch's first look, and a wait again: {waits:?}"
        );
        // As macOS writes its record: a new file renamed over the old.
        let written = prefs.join("written.plist");
        std::fs::write(&written, "allow\n").unwrap();
        std::fs::rename(&written, &record).unwrap();
        assert!(
            matches!(
                told.recv_timeout(MUST_HAPPEN),
                Ok(Told::Asked(Request::Browse { .. }))
            ),
            "the Allow heard, the browse goes through the direct path"
        );
        ends(watch, &end);
    }

    /// A browse its broker ended starts again after the backoff, waited out beside the watch's
    /// pipe, so dropping the watch ends that wait at once, though the backoff is longer than the
    /// hang detector.
    ///
    /// NEGATIVE CONTROL: a backoff slept out (`{ std::thread::sleep(within); false }` for the wait
    /// in `Standing::restart_after`) ends the thread only once the 600 s pass, and the hang
    /// detector fails it.
    #[test]
    fn a_dropped_watch_ends_its_thread_during_a_restart_pause() {
        let (_root, socket, seam) = scratch();
        let broker = serve(&socket, vec![ARRIVAL, ARRIVAL]);
        let bell = Arc::new(Bell::default());
        let (watch, end) = browsing_after(&bell, seam, LONG);
        assert!(broker.browsed());
        assert_eq!(
            waits_once(&watch, |waits| waits.len() == 2),
            [Waited(Why::Frames, None), Waited(Why::Restart, Some(LONG))]
        );
        ends(watch, &end);
    }
}
