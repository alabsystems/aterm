// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::port::{Fake, Ran};
use std::collections::BTreeMap;

/// A browse heard `name` of `service` come on `interface`.
fn added(name: &str, service: &str, interface: u32) -> Event {
    Event::Added(heard(name, service, interface))
}

/// A browse heard `name` of `service` go on `interface`.
fn removed(name: &str, service: &str, interface: u32) -> Event {
    Event::Removed(heard(name, service, interface))
}

fn heard(name: &str, service: &str, interface: u32) -> aterm_uds::lan::Instance {
    aterm_uds::lan::Instance {
        name: name.to_owned(),
        service: format!("{service}."),
        domain: "local.".to_owned(),
        interface,
    }
}

/// A resolve heard `host:port` on interface 5.
fn resolved(host: &str, port: u16) -> Event {
    Event::Resolved {
        host: host.to_owned(),
        port,
        txt: Vec::new(),
        interface: 5,
    }
}

const AIR: &str = "m14 Air M5 32GB 4TB prime";
const VM: &str = "Managed\u{2019}s Virtual Machine";
const FIXTURE: &str = "Caf\u{e9} Lab\u{2019}s Mac 7 (fixture)";

/// A browse of `_ssh._tcp` in the VM (captured with `dns-sd -B` on 16 Oct 2026): this Mac
/// on interface 5 (en0), the VM itself on 1 (lo0) and 5.
fn browse_ssh() -> Vec<Event> {
    vec![
        added(AIR, SSH_SERVICE, 5),
        added(VM, SSH_SERVICE, 1),
        added(VM, SSH_SERVICE, 5),
    ]
}

/// The same browse while a service was registered and then withdrawn: its comings and
/// goings on both interfaces.
fn browse_ssh_add_rmv() -> Vec<Event> {
    let mut heard = browse_ssh();
    heard.extend([
        added(FIXTURE, SSH_SERVICE, 1),
        added(FIXTURE, SSH_SERVICE, 5),
        removed(FIXTURE, SSH_SERVICE, 1),
        removed(FIXTURE, SSH_SERVICE, 5),
    ]);
    heard
}

/// A browse of `_rfb._tcp` in the VM: only the VM advertises Screen Sharing.
fn browse_rfb() -> Vec<Event> {
    vec![added(VM, RFB_SERVICE, 1), added(VM, RFB_SERVICE, 5)]
}

/// A later session in the VM (macOS 27.0, then named lab-02), while a host with Remote
/// Login alone was advertised (`spark-1`), as a Linux box's Avahi does.
fn browse_ssh_lab() -> Vec<Event> {
    vec![
        added("lab-02", SSH_SERVICE, 1),
        added("lab-02", SSH_SERVICE, 5),
        added("spark-1", SSH_SERVICE, 1),
        added("spark-1", SSH_SERVICE, 5),
        added(AIR, SSH_SERVICE, 5),
    ]
}

fn browse_sftp_lab() -> Vec<Event> {
    vec![
        added("lab-02", SFTP_SERVICE, 1),
        added("lab-02", SFTP_SERVICE, 5),
        added(AIR, SFTP_SERVICE, 5),
    ]
}

fn browse_rfb_lab() -> Vec<Event> {
    vec![
        added("lab-02", RFB_SERVICE, 1),
        added("lab-02", RFB_SERVICE, 5),
    ]
}

fn browsing(service: &str) -> Bonjour {
    Bonjour::Browse(service.to_owned())
}

fn resolving(name: &str, service: &str) -> Bonjour {
    Bonjour::Resolve {
        name: name.to_owned(),
        service: service.to_owned(),
    }
}

#[test]
fn an_instance_merges_the_interfaces_it_was_heard_on() {
    assert_eq!(
        instances(&browse_ssh(), SSH_SERVICE),
        [
            Instance {
                name: AIR.into(),
                interfaces: vec![5],
            },
            Instance {
                name: VM.into(),
                interfaces: vec![1, 5],
            },
        ]
    );
}

#[test]
fn an_instance_withdrawn_on_every_interface_is_gone() {
    let found = instances(&browse_ssh_add_rmv(), SSH_SERVICE);
    let names: Vec<&str> = found.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, [AIR, VM]);
    let mut half = browse_ssh_add_rmv();
    half.pop();
    let found = instances(&half, SSH_SERVICE);
    let fixture = found.iter().find(|i| i.name == FIXTURE).unwrap();
    assert_eq!(fixture.interfaces, [5]);
}

#[test]
fn an_event_of_another_service_or_kind_is_not_an_instance() {
    assert!(instances(&browse_rfb(), SSH_SERVICE).is_empty());
    assert_eq!(instances(&browse_rfb(), RFB_SERVICE).len(), 1);
    assert!(instances(&[resolved("lab-02.local.", 22)], SSH_SERVICE).is_empty());
    assert!(instances(&[], SSH_SERVICE).is_empty());
}

#[test]
fn a_resolve_names_each_target_once_without_its_trailing_dot() {
    assert_eq!(
        targets(&[resolved("m14-Air-M5-32GB-4TB-prime.local.", 22)]),
        [("m14-Air-M5-32GB-4TB-prime.local".to_string(), 22)]
    );
    assert_eq!(
        targets(&[
            resolved("Manageds-Virtual-Machine.local.", 22),
            resolved("Manageds-Virtual-Machine.local.", 22),
        ]),
        [("Manageds-Virtual-Machine.local".to_string(), 22)]
    );
    assert!(targets(&[resolved("-x.local.", 22), resolved("a.local.", 0)]).is_empty());
    assert!(targets(&[]).is_empty());
}

/// The fake answering the VM's session (the SFTP browse is a later session's, when the VM
/// was named lab-02: only this Mac's row there matches a row here).
fn vm(browse_ssh: Vec<Event>) -> Fake {
    let fake = Fake::root();
    fake.on_bonjour(&browsing(SSH_SERVICE), Ok(browse_ssh));
    fake.on_bonjour(&browsing(RFB_SERVICE), Ok(browse_rfb()));
    fake.on_bonjour(&browsing(SFTP_SERVICE), Ok(browse_sftp_lab()));
    fake.on_bonjour(
        &resolving(AIR, SSH_SERVICE),
        Ok(vec![resolved("m14-Air-M5-32GB-4TB-prime.local.", 22)]),
    );
    fake.on_bonjour(
        &resolving(VM, SSH_SERVICE),
        Ok(vec![
            resolved("Manageds-Virtual-Machine.local.", 22),
            resolved("Manageds-Virtual-Machine.local.", 22),
        ]),
    );
    fake.on_bonjour(
        &resolving(VM, RFB_SERVICE),
        Ok(vec![resolved("Manageds-Virtual-Machine.local.", 5900)]),
    );
    fake
}

#[test]
fn a_browse_finds_one_row_per_mac_and_marks_screen_sharing() {
    let fake = vm(browse_ssh());
    let got = browse(&fake, Duration::from_secs(3)).unwrap();
    assert_eq!(
        got.found,
        [
            Found {
                name: AIR.into(),
                address: "m14-Air-M5-32GB-4TB-prime.local".into(),
                port: 22,
                ssh: true,
                screen_sharing: false,
                sftp: true,
                source: Source::Bonjour,
            },
            Found {
                name: VM.into(),
                address: "Manageds-Virtual-Machine.local".into(),
                port: 22,
                ssh: true,
                screen_sharing: true,
                sftp: false,
                source: Source::Bonjour,
            },
        ]
    );
    assert!(got.unresolved.is_empty());
    let calls = fake.calls();
    assert_eq!(calls.len(), 5, "{calls:?}");
    assert!(
        calls.iter().all(|c| c.starts_with("bonjour ")),
        "every Bonjour request goes through the port: {calls:?}"
    );
    assert!(!calls.contains(&resolving(VM, RFB_SERVICE).key()));
}

/// Fake system fixture for the lab-02 session.
pub(crate) fn lab_session() -> Fake {
    lab_session_with(Ok(browse_sftp_lab()))
}

/// [`lab_session`] with the SFTP browse answering `sftp`.
pub(crate) fn lab_session_with(sftp: Result<Vec<Event>, String>) -> Fake {
    let fake = Fake::root();
    fake.on_bonjour(&browsing(SSH_SERVICE), Ok(browse_ssh_lab()));
    fake.on_bonjour(&browsing(SFTP_SERVICE), sftp);
    fake.on_bonjour(&browsing(RFB_SERVICE), Ok(browse_rfb_lab()));
    fake.on_bonjour(
        &resolving("lab-02", SSH_SERVICE),
        Ok(vec![
            resolved("lab-02.local.", 22),
            resolved("lab-02.local.", 22),
        ]),
    );
    fake.on_bonjour(
        &resolving("spark-1", SSH_SERVICE),
        Ok(vec![resolved("spark-1.local.", 22)]),
    );
    fake.on_bonjour(
        &resolving(AIR, SSH_SERVICE),
        Ok(vec![resolved("m14-Air-M5-32GB-4TB-prime.local.", 22)]),
    );
    fake
}

#[test]
fn a_mac_shows_macos_sftp_beside_remote_login_and_an_ssh_only_box_does_not() {
    let got = browse(&lab_session(), Duration::from_secs(3)).unwrap();
    let flags: Vec<(&str, bool, bool, bool)> = got
        .found
        .iter()
        .map(|f| (f.address.as_str(), f.ssh, f.sftp, f.screen_sharing))
        .collect();
    assert_eq!(
        flags,
        [
            ("lab-02.local", true, true, true),
            ("m14-Air-M5-32GB-4TB-prime.local", true, true, false),
            ("spark-1.local", true, false, false),
        ]
    );
}

#[test]
fn a_failed_sftp_browse_is_noted_and_the_scan_goes_on() {
    let fake = lab_session_with(Err("Bonjour error -65537".to_owned()));
    let got = browse(&fake, Duration::from_secs(3)).unwrap();
    assert_eq!(got.found.len(), 3);
    assert!(got.found.iter().all(|f| !f.sftp));
    assert!(got.sftp_failed.unwrap().contains("-65537"));
}

#[test]
fn a_mac_with_only_screen_sharing_is_a_row_without_ssh() {
    let got = browse(
        &vm(vec![added(AIR, SSH_SERVICE, 5)]),
        Duration::from_secs(3),
    )
    .unwrap();
    let vm_row = got
        .found
        .iter()
        .find(|f| f.name.starts_with("Managed"))
        .unwrap();
    assert!(!vm_row.ssh);
    assert!(vm_row.screen_sharing);
    assert_eq!(vm_row.port, 5900);
}

#[test]
fn an_instance_no_resolve_locates_is_listed_not_dropped() {
    let lost = Fake::root();
    lost.on_bonjour(&browsing(SSH_SERVICE), Ok(browse_ssh_add_rmv()));
    lost.on_bonjour(
        &resolving(AIR, SSH_SERVICE),
        Ok(vec![resolved("m14-Air-M5-32GB-4TB-prime.local.", 22)]),
    );
    let got = browse(&lost, Duration::from_secs(3)).unwrap();
    assert_eq!(got.found.len(), 1);
    assert_eq!(got.unresolved, [VM]);
}

/// A browse that cannot run (no LAN broker, or Bonjour's own error) fails the scan with its
/// words; one whose instance names look like error codes is still a browse.
#[test]
fn a_browse_that_cannot_run_fails_the_scan() {
    let fake = Fake::root();
    fake.on_bonjour(
        &browsing(SSH_SERVICE),
        Err("Bonjour error -65537".to_owned()),
    );
    let err = browse(&fake, Duration::from_secs(1)).unwrap_err();
    assert!(err.contains("-65537"), "{err}");
    let absent = Fake::root();
    let why = aterm_uds::lan::Unavailable::NotInstalled.to_string();
    absent.on_bonjour(&browsing(SSH_SERVICE), Err(why.clone()));
    let err = browse(&absent, Duration::from_secs(1)).unwrap_err();
    assert!(err.ends_with(&why), "{err}");
    let named = Fake::root();
    let mut heard = browse_ssh();
    heard.push(added("Lab -65570", SSH_SERVICE, 5));
    named.on_bonjour(&browsing(SSH_SERVICE), Ok(heard));
    assert!(browse(&named, Duration::from_secs(1)).is_ok());
}

/// Every accepted CIDR's endpoints are representable, including the
/// ranges ending at IPv4's maximum.
#[test]
fn a_subnet_ending_at_ipv4_max_never_overflows_its_endpoint() {
    let ordinary = hosts_of("10.0.0.0/24").expect("ordinary /24");
    assert_eq!(ordinary.len(), 254);
    assert_eq!(ordinary.first(), Some(&Ipv4Addr::new(10, 0, 0, 1)));
    assert_eq!(ordinary.last(), Some(&Ipv4Addr::new(10, 0, 0, 254)));
    assert_eq!(
        hosts_of("10.0.0.7/32").expect("single host"),
        [Ipv4Addr::new(10, 0, 0, 7)]
    );

    let observed: Vec<_> = [
        (
            "255.255.255.0/24",
            254,
            Ipv4Addr::new(255, 255, 255, 1),
            Ipv4Addr::new(255, 255, 255, 254),
        ),
        (
            "255.255.255.255/32",
            1,
            Ipv4Addr::BROADCAST,
            Ipv4Addr::BROADCAST,
        ),
    ]
    .into_iter()
    .map(|(cidr, count, first, last)| {
        let correct = std::panic::catch_unwind(|| hosts_of(cidr)).is_ok_and(|got| {
            got.is_ok_and(|hosts| {
                hosts.len() == count && hosts.first() == Some(&first) && hosts.last() == Some(&last)
            })
        });
        (cidr, correct)
    })
    .collect();
    assert!(
        observed.iter().all(|(_, correct)| *correct),
        "valid CIDR endpoints must survive checked arithmetic: {observed:?}"
    );
}

#[test]
fn a_subnet_is_its_host_addresses_and_no_wider_than_a_slash_22() {
    let hosts = hosts_of("192.168.64.0/24").unwrap();
    assert_eq!(hosts.len(), 254);
    assert_eq!(hosts[0], Ipv4Addr::new(192, 168, 64, 1));
    assert_eq!(hosts[253], Ipv4Addr::new(192, 168, 64, 254));
    assert_eq!(hosts_of("10.0.4.77/22").unwrap().len(), 1022);
    assert_eq!(
        hosts_of("10.0.0.5/32").unwrap(),
        [Ipv4Addr::new(10, 0, 0, 5)]
    );
    assert_eq!(hosts_of("10.0.0.4/31").unwrap().len(), 2);
    assert!(hosts_of("10.0.0.0/21").unwrap_err().contains("/22"));
    assert!(hosts_of("10.0.0.0").is_err());
    assert!(hosts_of("10.0.0.0/33").is_err());
    assert!(hosts_of("lab/24").is_err());
}

/// Answers by address; everything else is silent.
struct Scripted(BTreeMap<Ipv4Addr, Answer>, Answer);

impl Knock for Scripted {
    fn knock(&self, addr: SocketAddr, _timeout: Duration) -> Answer {
        match addr {
            SocketAddr::V4(v4) if v4.port() == 22 => self
                .0
                .get(v4.ip())
                .cloned()
                .unwrap_or_else(|| self.1.clone()),
            _ => panic!("knocked {addr}, not port 22"),
        }
    }
}

#[test]
fn a_sweep_lists_the_addresses_whose_port_22_accepted_in_order() {
    let knock = Scripted(
        BTreeMap::from([
            (Ipv4Addr::new(192, 168, 64, 9), Answer::Open),
            (Ipv4Addr::new(192, 168, 64, 2), Answer::Open),
            (Ipv4Addr::new(192, 168, 64, 3), Answer::Closed),
        ]),
        Answer::NoAnswer,
    );
    let found = subnet(&knock, "192.168.64.0/24", Duration::from_millis(1)).unwrap();
    let addrs: Vec<&str> = found.iter().map(|f| f.address.as_str()).collect();
    assert_eq!(addrs, ["192.168.64.2", "192.168.64.9"]);
    assert!(
        found
            .iter()
            .all(|f| f.ssh && f.port == 22 && f.source == Source::Subnet)
    );
}

/// Without a LAN broker to connect through, a sweep is not run, naming the broker, rather
/// than reading as a subnet with no Macs. NEGATIVE CONTROL: a subnet nothing answers on is
/// swept, and empty.
#[test]
fn a_sweep_with_no_lan_broker_names_it() {
    let why = aterm_uds::lan::Unavailable::NotInstalled.to_string();
    let knock = Scripted(BTreeMap::new(), Answer::NoBroker(why.clone()));
    let err = subnet(&knock, "192.168.64.0/28", Duration::from_millis(1)).unwrap_err();
    assert_eq!(err, format!("192.168.64.0/28 was not swept: {why}"));
    let knock = Scripted(
        BTreeMap::from([(Ipv4Addr::new(192, 168, 64, 3), Answer::Closed)]),
        Answer::NoAnswer,
    );
    assert!(
        subnet(&knock, "192.168.64.0/28", Duration::from_millis(1))
            .unwrap()
            .is_empty()
    );
}

/// The VM's host key blob, as `ssh-keyscan -t ed25519` read it there.
const VM_KEY: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIHv2Xf9kgkNkpV3WltnLoVx25AQOEx+mch1DHT/oZdSa";

const VM_UUID: &str = "0A1B2C3D-0000-4000-8000-000000000002";

fn vm_row() -> Found {
    Found {
        name: "Managed\u{2019}s Virtual Machine".into(),
        address: "Manageds-Virtual-Machine.local".into(),
        port: 22,
        ssh: true,
        screen_sharing: true,
        sftp: true,
        source: Source::Bonjour,
    }
}

fn lab() -> (Paths, Inventory) {
    let paths = Paths {
        root: std::path::PathBuf::from("/ctl/machines"),
    };
    let inv = crate::controller::inventory::parse(&format!(
        "schema = 1\n[[host]]\nname = \"lab-vm\"\nuuid = \"{VM_UUID}\"\naddress = \"192.168.64.2\"\nuser = \"admin\"\n"
    ))
    .unwrap();
    (paths, inv)
}

/// `fake` answers the scan of the VM ([`super::super::lan_proxy::scanned`]): `key`
/// recorded, or nothing, and `ssh` ending as `ran`.
pub(crate) fn vm_scanned(fake: &Fake, key: Option<&str>, ran: Ran) {
    super::super::lan_proxy::scanned(
        fake,
        std::path::Path::new("/ctl/machines/scan"),
        "Manageds-Virtual-Machine.local",
        22,
        OFFERED_WITHIN,
        key,
        ran,
    );
}

/// How `ssh` ends a scan whose key exchange recorded the key: no method authenticates.
fn denied() -> Ran {
    Ran::exit(
        255,
        "hostkey@host: Permission denied (publickey,password).\n",
    )
}

#[test]
fn a_mac_whose_key_is_pinned_under_an_inventory_uuid_is_that_host() {
    let (paths, inv) = lab();
    let fake = Fake::root();
    vm_scanned(&fake, Some(VM_KEY), denied());
    fake.file(
        "/ctl/machines/known_hosts",
        format!(
            "{} ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIHv2Xf9kgkNkpV3WltnLoVx25AQOEx+mch1DHT/oZdSa\n",
            VM_UUID.to_ascii_lowercase()
        )
        .as_bytes(),
    );
    assert_eq!(
        pinned_host(&fake, &paths, &inv, &vm_row()),
        Read::Value("lab-vm".into())
    );
}

#[test]
fn a_mac_whose_key_matches_no_pin_is_new_even_at_an_enrolled_address() {
    let (paths, inv) = lab();
    let fake = Fake::root();
    vm_scanned(&fake, Some(VM_KEY), denied());
    fake.file(
        "/ctl/machines/known_hosts",
        format!("{VM_UUID} ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOtherKeyOtherKeyOtherKeyOtherKeyOtherKey\n")
            .as_bytes(),
    );
    assert_eq!(pinned_host(&fake, &paths, &inv, &vm_row()), Read::Absent);
    let fresh = Fake::root();
    vm_scanned(&fresh, Some(VM_KEY), denied());
    assert_eq!(pinned_host(&fresh, &paths, &inv, &vm_row()), Read::Absent);
}

#[test]
fn a_mac_that_shows_no_key_is_unknown_never_new() {
    let (paths, inv) = lab();
    let fake = Fake::root();
    vm_scanned(&fake, None, Ran::exit(255, ""));
    assert!(matches!(
        pinned_host(&fake, &paths, &inv, &vm_row()),
        Read::Unusable(_)
    ));
    let mut screen_only = vm_row();
    screen_only.ssh = false;
    assert!(matches!(
        pinned_host(&fake, &paths, &inv, &screen_only),
        Read::Unusable(_)
    ));
}

#[test]
fn a_resolve_that_cannot_run_locates_nothing() {
    let fake = Fake::root();
    assert!(resolve(&fake, "x", SSH_SERVICE, Duration::from_millis(1)).is_empty());
}

/// A browse instance whose name looks like a target never supplies a socket address.
/// Only a resolved event supplies the target, independently of the instance's free text.
#[test]
fn an_instance_name_that_spells_the_marker_does_not_choose_the_target() {
    let spoofed = " 0:00:00.001  Lab can be reached at decoy.local.:2222 (interface 1)._ssh._tcp.local. can be reached at real.local.:22 (interface 14)\n";
    assert_eq!(
        targets(&[added(spoofed, SSH_SERVICE, 14), resolved("real.local", 22)]),
        vec![("real.local".into(), 22)]
    );
}
