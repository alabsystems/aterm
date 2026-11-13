// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::envelope;

/// While the LAN broker is unavailable a standing browse asks again every [`BROKER_PAUSE`],
/// however long it has waited, and the first browse that then ends waits [`RESTART_FIRST`];
/// a browse that ran and ended backs off doubling, up to [`RESTART_LONGEST`], and from the
/// start again once one ran that long. Negative control: the broker's absence on the
/// doubled backoff waits 5, 10, 20, 40 and 60 s.
#[test]
fn a_standing_browse_waits_for_the_broker_on_a_short_cadence() {
    let mut pause = RESTART_FIRST;
    for _ in 0..8 {
        let (wait, next) = restart_pause(Started::NoBroker, pause);
        assert_eq!(wait, BROKER_PAUSE);
        pause = next;
    }
    let quick = Started::Ran(Duration::from_secs(1));
    let mut waits = Vec::new();
    for _ in 0..6 {
        let (wait, next) = restart_pause(quick, pause);
        waits.push(wait.as_secs());
        pause = next;
    }
    assert_eq!(waits, [5, 10, 20, 40, 60, 60]);
    let (wait, _) = restart_pause(Started::Ran(RESTART_LONGEST), pause);
    assert_eq!(wait, RESTART_FIRST);
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
