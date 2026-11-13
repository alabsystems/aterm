// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::durability::Durability;
use std::ffi::OsString;

fn words(args: &[&str]) -> Vec<OsString> {
    args.iter().map(|word| OsString::from(*word)).collect()
}

#[test]
fn a_broker_report_uses_the_exact_argv_or_reports_an_unknown_tier() {
    let exact = words(&[
        "/bin/aterm",
        "link",
        "broker",
        "/r/bus.sock",
        "--secret-file",
        "/r/name --durability relaxed",
    ]);
    let relaxed = words(&[
        "/bin/aterm",
        "link",
        "broker",
        "/r/bus.sock",
        "--durability",
        "relaxed",
    ]);
    let other = words(&[
        "/bin/aterm",
        "link",
        "broker",
        "/other/socket",
        "--secret-file",
        "--unix /r/bus.sock",
    ]);
    let cases = [
        (Some(relaxed), Some(Durability::Relaxed)),
        (Some(exact), Some(crate::durability::default_for(true))),
        (None, None),
        (Some(Vec::new()), None),
        (Some(other), None),
    ];
    let mut view = BrokerView {
        endpoint: "/r/bus.sock".into(),
        ..BrokerView::default()
    };
    for (argv, expected) in cases {
        let mut read = Vec::new();
        view.locate_with(
            |_| Ok(Some(71)),
            |pid| {
                assert_eq!(pid, 71);
                None
            },
            |pid, exe| {
                assert_eq!(pid, 71);
                assert_eq!(exe, "/bin/aterm", "argv[0]");
                None
            },
            |pid| {
                read.push(pid);
                argv.clone()
            },
            |_, _| unreachable!("a broker's build is not asked"),
        );
        assert_eq!(read, vec![71]);
        assert_eq!(view.pid, Some(71));
        assert_eq!(view.durability, expected);
    }
}

/// Every other read (label, argv, replaced build) is of the holder, and none is made when
/// nothing holds it or which process does is unknown. An unknown holder is kept as why, and
/// the socket then reads as held, as one a named process holds does; one nobody holds does
/// not. NEGATIVE CONTROL: read the scan's error as no holder, and the unknown holder is
/// neither said nor held.
#[test]
fn the_broker_is_the_process_holding_its_listener() {
    let argv = words(&["/opt/aterm", "link", "broker", "/r/bus.sock", "/r/bus.log"]);
    let unknown = "the listener scan cannot tell (connection refused)".to_string();
    for (held, want) in [
        (Ok(Some(71)), Some(71)),
        (Ok(None), None),
        (Err(unknown.clone()), None),
    ] {
        let mut view = BrokerView {
            endpoint: "/r/bus.sock".into(),
            ..BrokerView::default()
        };
        let mut sockets = Vec::new();
        let asked = std::cell::RefCell::new(Vec::new());
        view.locate_with(
            |sock| {
                sockets.push(sock.to_string());
                held.clone()
            },
            |pid| {
                asked.borrow_mut().push(("label", pid));
                None
            },
            |pid, _| {
                asked.borrow_mut().push(("replaced", pid));
                None
            },
            |pid| {
                asked.borrow_mut().push(("argv", pid));
                Some(argv.clone())
            },
            |pid, _| {
                asked.borrow_mut().push(("own", pid));
                false
            },
        );
        assert_eq!(
            sockets,
            ["/r/bus.sock"],
            "the holder is asked of the endpoint"
        );
        assert_eq!(view.pid, want, "held by {held:?}");
        assert_eq!(view.pid_unknown, held.as_ref().err().cloned(), "{held:?}");
        assert_eq!(view.held(), held != Ok(None), "{held:?}");
        let wanted: Vec<(&str, u32)> = want
            .map(|pid| vec![("label", pid), ("argv", pid), ("replaced", pid)])
            .unwrap_or_default();
        assert_eq!(asked.take(), wanted, "every other read is of the holder");
        assert_eq!(view.log.is_some(), want.is_some());
    }
}

/// The holder's argv names the endpoint by its bytes or its canonical spelling, as the
/// listener scan names its bound path.
#[cfg(unix)]
#[test]
fn the_holders_socket_word_names_the_endpoint_as_its_listener_does() {
    let dir = crate::test_scratch_dir("fab");
    let base = dir.path().canonicalize().expect("its own spelling");
    let (real, link) = (base.join("real"), base.join("link"));
    std::fs::create_dir(&real).expect("the real folder");
    std::os::unix::fs::symlink(&real, &link).expect("a link to it");
    let at = |dir: &Path| dir.join("bus.sock").to_string_lossy().into_owned();
    let served = |endpoint: String, word: String| {
        let mut view = BrokerView {
            endpoint,
            ..BrokerView::default()
        };
        let log = format!("{word}.log");
        let argv = words(&["/bin/aterm", "link", "broker", &word, &log]);
        view.locate_with(
            |_| Ok(Some(71)),
            |_| None,
            |_, _| None,
            |_| Some(argv.clone()),
            |_, _| false,
        );
        view.log
    };
    assert_eq!(
        served(at(&link), at(&real)),
        Some(PathBuf::from(format!("{}.log", at(&real)))),
        "the endpoint spelled through a link, the broker bound at its folder's own spelling"
    );
    assert!(served(at(&real), at(&real)).is_some(), "the same bytes");
    assert_eq!(
        served(at(&real), at(&link)),
        None,
        "a socket bound through a link is named by that spelling alone"
    );
}

/// One holder is the broker, none is none, and two leave which one serves unknown, naming
/// both. A table the scan could not read still leaves the one holder it found the broker,
/// but where it found none, a sealed or unreadable table may hold the listener: unknown.
/// NEGATIVE CONTROL: read no holder as none whatever did not read, and a scan whose one
/// possible holder was sealed names no holder.
#[test]
fn one_holder_is_the_broker_and_two_are_unknown() {
    let scan = |holders: &[u32]| aterm_uds::holders::HolderScan {
        holders: holders.to_vec(),
        scanned: 9,
        ..aterm_uds::holders::HolderScan::default()
    };
    assert_eq!(the_one_holder(&scan(&[71])), Ok(Some(71)));
    assert_eq!(the_one_holder(&scan(&[])), Ok(None));
    assert_eq!(
        the_one_holder(&scan(&[71, 72])),
        Err("2 processes hold its listener (71, 72)".to_string())
    );
    let incomplete = aterm_uds::holders::HolderScan {
        sealed: vec![80],
        unreadable: vec![81],
        ..scan(&[71])
    };
    assert_eq!(the_one_holder(&incomplete), Ok(Some(71)));
    for (sealed, unreadable) in [(vec![80], vec![]), (vec![], vec![81]), (vec![80], vec![81])] {
        let unread = aterm_uds::holders::HolderScan {
            sealed,
            unreadable,
            ..scan(&[])
        };
        assert!(the_one_holder(&unread).is_err(), "{unread:?}");
    }
}

/// Where which process holds the listener is unknown, a probe of the socket's file that finds
/// nothing bound there (no file, a socket file whose listener closed) makes it none, whatever
/// the scan could not read or tell; a stream socket there, a datagram one or a probe that
/// fails leaves it unknown. A scan that named a holder or none is not probed. On a real
/// socket, a stale file reads none and a live listener stays unknown. NEGATIVE CONTROL: leave
/// every unknown unknown, and a stale socket file beside a scan that cannot run is never
/// none.
#[test]
fn an_unknown_holder_is_none_where_nothing_is_bound_to_the_socket() {
    use aterm_uds::holders::Bound;
    let unknown = || Err::<Option<u32>, _>("the listener scan cannot tell (x)".to_string());
    let probed = std::cell::Cell::new(0);
    let bound = |found: io::Result<Bound>| {
        probed.set(probed.get() + 1);
        found
    };
    assert_eq!(
        unknown_unless_unbound(unknown(), || bound(Ok(Bound::Nothing))),
        Ok(None)
    );
    for found in [
        Ok(Bound::Stream),
        Ok(Bound::Datagram),
        Err(io::Error::from(io::ErrorKind::TimedOut)),
    ] {
        let still = unknown_unless_unbound(unknown(), || bound(found));
        assert!(
            still
                .as_ref()
                .is_err_and(|why| why.starts_with("the listener scan cannot tell (x)")),
            "{still:?}"
        );
    }
    probed.set(0);
    for held in [Ok(Some(71)), Ok(None)] {
        assert_eq!(
            unknown_unless_unbound(held.clone(), || bound(Ok(Bound::Nothing))),
            held
        );
    }
    assert_eq!(probed.get(), 0, "a scan that answered is not probed");
    #[cfg(any(target_vendor = "apple", target_os = "linux"))]
    {
        let dir = aterm_tempfile::Builder::new()
            .suffix(".noindex")
            .tempdir_in("/tmp")
            .expect("a scratch folder short enough for sun_path");
        let (stale, live) = (dir.path().join("stale.sock"), dir.path().join("live.sock"));
        drop(std::os::unix::net::UnixListener::bind(&stale).expect("a listener closed"));
        let listener = std::os::unix::net::UnixListener::bind(&live).expect("a listener");
        let real =
            |path: &Path| unknown_unless_unbound(unknown(), || aterm_uds::holders::bound_at(path));
        assert_eq!(
            real(&stale),
            Ok(None),
            "a socket file whose listener closed"
        );
        assert_eq!(real(&dir.path().join("none.sock")), Ok(None), "no file");
        assert!(real(&live).is_err(), "a live listener");
        drop(listener);
    }
}

/// With no argv read, the replaced-build check is not made.
#[test]
fn the_replaced_build_check_reads_the_whole_argv0() {
    let spaced = "/a b/aterm";
    let argv = words(&[spaced, "link", "broker", "/r/bus.sock", "/r/bus.log"]);
    for (argv, want) in [
        (Some(argv), Some(spaced)),
        (None, None),
        (Some(Vec::new()), None),
    ] {
        let mut view = BrokerView {
            endpoint: "/r/bus.sock".into(),
            ..BrokerView::default()
        };
        let mut asked = Vec::new();
        view.locate_with(
            |_| Ok(Some(72)),
            |_| None,
            |pid, exe| {
                assert_eq!(pid, 72);
                asked.push(exe.to_string());
                Some(exe.to_string())
            },
            |_| argv.clone(),
            |_, _| false,
        );
        assert_eq!(
            asked,
            want.into_iter().map(str::to_string).collect::<Vec<_>>()
        );
        assert_eq!(view.replaced.as_deref(), want);
    }
}

/// With the mesh on, the process holding `<root>/bus.sock` as `<aterm> node serve` is the node
/// service: the host, whose log is `<root>/bus.log` (so its health snapshot is read), whose
/// argv names no tier, and whose build is this one's only where `own` says it runs this
/// process's image and its own is not replaced. Another verb of `node`, another socket, or a
/// relative one is no host, and a broker's argv stays a broker's. NEGATIVE CONTROL: with the
/// mesh off the same argv reads as today, no host and no log, and `own` is never asked.
#[test]
fn the_node_service_is_read_as_the_host_of_bus_log_where_the_mesh_is_on() {
    let program = "/Applications/aterm.app/Contents/MacOS/aterm";
    let serve = words(&[program, "node", "serve"]);
    let locate = |mesh: bool, endpoint: &str, argv: &[OsString], is_own: bool, replaced: bool| {
        let mut view = BrokerView {
            endpoint: endpoint.to_string(),
            mesh,
            ..BrokerView::default()
        };
        let mut asked = Vec::new();
        view.locate_with(
            |_| Ok(Some(71)),
            |_| Some(crate::enable::NODE_JOB.to_string()),
            |_, exe| replaced.then(|| exe.to_string()),
            |_| Some(argv.to_vec()),
            |pid, exe| {
                asked.push((pid, exe.to_string()));
                is_own
            },
        );
        (view, asked)
    };
    let (node, asked) = locate(true, "/r/bus.sock", &serve, true, false);
    assert_eq!(node.host, Some(Host::NodeService));
    assert_eq!(node.log, Some(PathBuf::from("/r/bus.log")));
    assert_eq!(
        node.durability, None,
        "the node service's argv names no tier"
    );
    assert_eq!(node.program.as_deref(), Some(program));
    assert_eq!(
        asked,
        [(71, program.to_string())],
        "its build is asked of it once"
    );
    assert!(node.own_build);
    assert_eq!(node.build(), Some(aterm_stamp::build_id()));
    assert_eq!(node.keeper(), crate::enable::Keeper::Node);
    let (other, _) = locate(true, "/r/bus.sock", &serve, false, false);
    assert_eq!((other.host, other.build()), (Some(Host::NodeService), None));
    let (stale, asked) = locate(true, "/r/bus.sock", &serve, true, true);
    assert_eq!(stale.host, Some(Host::NodeService));
    assert!(
        asked.is_empty() && !stale.own_build,
        "a replaced image is no build of this one"
    );
    let mut trailing = serve.clone();
    trailing.push(OsString::from("--verbose"));
    assert_eq!(
        locate(true, "/r/bus.sock", &trailing, false, false).0.host,
        Some(Host::NodeService)
    );
    for (endpoint, argv) in [
        ("/r/bus.sock", words(&[program, "node", "status"])),
        ("/r/bus.sock", words(&[program, "node"])),
        ("/r/other.sock", serve.clone()),
        ("bus.sock", serve.clone()),
    ] {
        let (view, asked) = locate(true, endpoint, &argv, true, false);
        assert_eq!(
            (view.host, view.log.clone()),
            (None, None),
            "{endpoint} {argv:?}"
        );
        assert!(asked.is_empty() && !view.own_build);
    }
    let broker = words(&[program, "link", "broker", "/r/bus.sock", "/r/bus.log"]);
    let (linked, asked) = locate(true, "/r/bus.sock", &broker, true, false);
    assert_eq!(linked.host, Some(Host::LinkBroker));
    assert_eq!(
        linked.durability,
        Some(crate::durability::default_for(false))
    );
    assert!(asked.is_empty() && linked.build().is_none());
    assert_eq!(linked.keeper(), crate::enable::Keeper::Job);
    let (off, asked) = locate(false, "/r/bus.sock", &serve, true, false);
    assert_eq!(
        (off.host, off.log.as_deref(), off.durability),
        (None, None, None)
    );
    assert!(
        asked.is_empty() && !off.own_build,
        "NEGATIVE CONTROL: mesh off reads as today"
    );
    assert_eq!(off.keeper(), crate::enable::Keeper::Job);
}

/// The node service's tier is its current health snapshot's (the argv names none), else its
/// `Hello`'s; a snapshot another pid wrote is not its. NEGATIVE CONTROL: a broker's argv
/// tier wins over both, as it did.
#[test]
fn the_node_services_tier_is_the_one_its_snapshot_says() {
    use astream_broker::{BrokerHealth, HealthRead, HealthWriter, ServedInfo, ServedReason, Tier};
    let served = |requested| ServedInfo {
        requested,
        served: requested,
        reason: ServedReason::None,
        horizon_ms: None,
        rules: 0,
    };
    let snapshot = |pid, requested| {
        let mut health = BrokerHealth::default();
        health.pid = pid;
        health.served = served(requested);
        Some(Ok(HealthRead {
            health,
            writer: HealthWriter::Serving,
        }))
    };
    let mut view = BrokerView {
        endpoint: "/r/bus.sock".to_string(),
        mesh: true,
        host: Some(Host::NodeService),
        pid: Some(71),
        log: Some(PathBuf::from("/r/bus.log")),
        snapshot: snapshot(71, Tier::Relaxed),
        served: Some(served(Tier::Strict)),
        ..BrokerView::default()
    };
    assert_eq!(view.tier(), Some(Durability::Relaxed));
    view.snapshot = snapshot(72, Tier::Relaxed);
    assert_eq!(
        view.tier(),
        Some(Durability::Strict),
        "another pid's snapshot"
    );
    view.served = None;
    assert_eq!(view.tier(), None);
    view.snapshot = snapshot(71, Tier::Relaxed);
    view.durability = Some(Durability::Ordered);
    assert_eq!(
        view.tier(),
        Some(Durability::Ordered),
        "NEGATIVE CONTROL: an argv's tier"
    );
}

#[test]
fn swarm_enabled_false_disables_fabric_command_in_toml() {
    let off = "[swarm]\nenabled = false\n[fabric]\ncommand = [\"aterm\", \"link\", \"serve\"]\n";
    assert_eq!(command_in_toml(off), Ok(None));
    // Negative control: `[swarm] enabled = true` resolves the configured command.
    let on = "[swarm]\nenabled = true\n[fabric]\ncommand = [\"aterm\", \"link\", \"serve\"]\n";
    assert_eq!(
        command_in_toml(on),
        Ok(Some(vec![
            "aterm".to_string(),
            "link".to_string(),
            "serve".to_string(),
        ]))
    );
    assert!(command_in_toml("[swarm]\nenabled = 1\n").is_err());
}
