// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[cfg(unix)]
#[test]
fn failed_node_reporting_still_stops_the_owned_child() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let report = dir.path().join("started.json");
    write_new(&report, b"already published").unwrap();
    let (result, mut input) = {
        let cat = aterm_uds::deadline::stand_in(Path::new("/bin/cat"));
        let mut owned = Owned(
            aterm_uds::spawnfd::command(cat)
                .stdin(aterm_uds::spawnfd::Stdio::piped())
                .stdout(aterm_uds::spawnfd::Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let input = aterm_uds::deadline::cat_echoes(&mut owned.0);
        (write_new(&report, b"replacement"), input)
    };
    assert!(result.is_err(), "the phase publication must fail");
    aterm_uds::deadline::once_released(|| {
        if aterm_uds::hangup::fd_peer_closed(std::os::fd::AsRawFd::as_raw_fd(&input)) {
            Ok(())
        } else {
            Err(std::io::ErrorKind::WouldBlock.into())
        }
    });
    assert!(
        input.write_all(b"still running?\n").is_err(),
        "the child must be stopped even though its stdin remains held"
    );
    assert_eq!(std::fs::read(&report).unwrap(), b"already published");
}

#[test]
fn database_cli_refuses_authority_and_arbitrary_commands_before_launch() {
    for verb in [
        "accounts-authorize",
        "accounts-intent",
        "accounts-request",
        "resources-reserve",
        "table",
        "init",
        "anything",
    ] {
        let error = database(
            Path::new("missing-executable"),
            Path::new("missing-world"),
            &[verb.into()],
            false,
        )
        .unwrap_err();
        assert_eq!(error.code, 2, "{verb} must be a usage refusal before I/O");
    }
}

/// The vault CLI passes an admin's approval of a key, a join of a lab profile's own vault
/// file, a join of the URL-served lab vault the world follows and `status`, and refuses
/// before any I/O every other verb, a join of the live ALab's URL or of no file, and a write
/// of a secret. A URL join of a lab vault passes its shape check, then only in a world that
/// follows that vault: a world that follows another lab vault, or none, is refused before
/// anything runs, and the world's own passes to the executable.
///
/// NEGATIVE CONTROL: with `vault_cli`'s check against the vault the world follows removed, the
/// join of another lab vault reaches the executable:
/// `http://192.168.1.42:7192/v1/lab-ffff0000#… in …/follows: Err((1, Some("No such file or
/// directory (os error 2)")))`.
#[test]
fn vault_cli_passes_only_a_labs_own_membership_verbs() {
    let parent = crate::instance::world_parent();
    let vault = parent.path().join("fixture.vault");
    std::fs::write(&vault, b"").unwrap();
    let vault = vault.to_string_lossy().into_owned();
    let words = |w: &[&str]| w.iter().map(|w| (*w).to_owned()).collect::<Vec<String>>();
    let own =
        "http://192.168.1.42:7192/v1/lab-1a2b3c4d#wfU3N368OzA1XVYhTwWSjwGzZf1DM/qCsRwdR+r4tLo=";
    let other =
        "http://192.168.1.42:7192/v1/lab-ffff0000#wfU3N368OzA1XVYhTwWSjwGzZf1DM/qCsRwdR+r4tLo=";
    for passed in [
        words(&["status"]),
        words(&["approve", "age1qqqq", "--name", "m4"]),
        words(&["join", &vault]),
        words(&[
            "join",
            &format!("{vault}#wfU3N368OzA1XVYhTwWSjwGzZf1DM/qCsRwdR+r4tLo="),
        ]),
        words(&["join", own]),
        words(&["join", other]),
    ] {
        assert!(admitted_secrets(&passed), "{passed:?}");
    }
    for refused in [
        words(&["join"]),
        words(&["join", aterm_secrets::ALAB_DEFAULT_URL]),
        words(&["join", &format!("{}#x", aterm_secrets::ALAB_DEFAULT_URL)]),
        words(&[
            "join",
            &format!(
                "{}#{}",
                aterm_secrets::ALAB_DEFAULT_URL,
                aterm_secrets::ALAB_SIGNER
            ),
        ]),
        words(&["join", "http://192.168.1.42:7484/v1/lab-1a2b#k="]),
        words(&["join", "http://192.168.1.42:7100/v1/lab-1a2b#k="]),
        words(&["join", "http://8.8.8.8:7192/v1/lab-1a2b#k="]),
        words(&["join", "http://192.168.1.42:7192/v1/lab-1a2b"]),
        words(&["join", "http://192.168.1.42:7192/v1/lab-1a2b#k y"]),
        words(&["join", &format!("{vault}#x y")]),
        words(&["join", "fixture.vault"]),
        words(&["join", "/no/such/place/fixture.vault"]),
        words(&["approve", "--all"]),
        words(&["approve", "age1qqqq"]),
        words(&["set", "name"]),
        words(&["edit", "name"]),
        words(&["revoke", "m2"]),
    ] {
        assert!(!admitted_secrets(&refused), "{refused:?}");
        let error = vault_cli(
            Path::new("missing-executable"),
            Path::new("missing-world"),
            &refused,
            false,
        )
        .unwrap_err();
        assert_eq!(
            error.code, 2,
            "{refused:?} must be a usage refusal before I/O"
        );
    }
    // Which lab vault a URL join names is the world's to say: one that follows `own` joins
    // it and no other, and one that follows none joins no URL. The world's own reaches the
    // executable, which is missing here, so that run fails, but not as a usage refusal.
    let (follows, unpinned) = (
        parent.path().join("follows"),
        parent.path().join("unpinned"),
    );
    for root in [&follows, &unpinned] {
        prepare_recipient(root, LAB_FLEET_PORT).unwrap();
    }
    write_new(
        &World::open_at(&follows).unwrap().dir.join(LAB_VAULT),
        own.as_bytes(),
    )
    .unwrap();
    let joined = |root: &Path, vault: &str| {
        vault_cli(
            Path::new("missing-executable"),
            root,
            &words(&["join", vault]),
            false,
        )
        .map(|_| ())
        .map_err(|error| (error.code, error.msg))
    };
    for (root, vault) in [(&follows, other), (&unpinned, own)] {
        let refused = joined(root, vault);
        assert!(
            refused.as_ref().is_err_and(|(code, _)| *code == 2),
            "{vault} in {}: {refused:?}",
            root.display()
        );
    }
    let ran = joined(&follows, own);
    assert!(
        ran.as_ref().is_err_and(|(code, _)| *code != 2),
        "CONTROL: the world's own lab vault reaches the executable: {ran:?}"
    );
}

/// A lab port is any from 1024 but the live fleet's and the live vault servers', and a lab
/// URL is `http://` at a private or loopback IPv4 address on a lab port naming a `lab-` vault:
/// the live ALab vault's URL and every near miss are refused. A `prepare-…` verb's flags take
/// the lab fleet port by default and never let the served vault share it.
///
/// NEGATIVE CONTROL: with `lab_port`'s live-port exclusion removed, the live fleet's port is
/// taken: `7100 is refused: Some(7100)`.
#[test]
fn a_lab_port_and_url_are_never_the_live_fleets_or_vaults() {
    for port in ["1024", "7190", "7192", "65535"] {
        assert!(lab_port(port).is_ok(), "{port}");
    }
    for port in [
        "7100", "7484", "1023", "80", "0", "", "65536", "+7190", "7190 ", "0x1c16",
    ] {
        let taken = lab_port(port);
        assert!(
            taken.is_err(),
            "{port} is refused: {:?}",
            taken.as_ref().ok()
        );
    }
    for url in [
        "http://192.168.1.42:7192/v1/lab-1a2b3c4d",
        "http://10.0.0.5:7192/v1/lab-e1b",
        "http://127.0.0.1:50000/v1/lab-x",
    ] {
        assert!(lab_url(url), "{url}");
    }
    for url in [
        aterm_secrets::ALAB_DEFAULT_URL,
        "http://alab-secrets.local:7192/v1/lab-1a2b",
        "http://192.168.1.42:7484/v1/lab-1a2b",
        "http://192.168.1.42:7100/v1/lab-1a2b",
        "http://8.8.8.8:7192/v1/lab-1a2b",
        "https://192.168.1.42:7192/v1/lab-1a2b",
        "http://192.168.1.42:7192/v1/alab",
        "http://192.168.1.42:7192/v1/lab-",
        "http://192.168.1.42:7192/v1/lab-a/b",
        "http://192.168.1.42:7192/v1/lab-a#k=",
        "http://[fe80::1]:7192/v1/lab-a",
        "http://192.168.1.42/v1/lab-a",
    ] {
        assert!(!lab_url(url), "{url}");
    }
    let words = |w: &[&str]| w.iter().map(|w| (*w).to_owned()).collect::<Vec<String>>();
    let keys = words(&["age1a", "age1b"]);
    let (serve, port, rest) = prepare_flags(&keys).unwrap();
    assert_eq!((serve, port, rest.len()), (None, LAB_FLEET_PORT, 2));
    let given = words(&["--port", "7191", "--serve", "192.168.1.42:7192", "age1a"]);
    let (serve, port, rest) = prepare_flags(&given).unwrap();
    assert_eq!(serve, Some("192.168.1.42:7192".parse().unwrap()));
    assert_eq!((port, rest), (7191, &given[4..]));
    for flags in [
        words(&["--serve", "192.168.1.42:7190"]),
        words(&["--port", "7100"]),
        words(&["--port", "7191", "--port", "7192"]),
        words(&["--serve", "m9.local:7192"]),
        words(&["--live", "age1a"]),
    ] {
        assert!(prepare_flags(&flags).is_err(), "{flags:?}");
    }
}

/// `commons-lan members` passes a phraseless `join` naming lab members' nodes by
/// `--at <IPv4>:<port>`, and refuses before any I/O a phrase, the live fleet's or vault's port,
/// a host name, a public or IPv6 address, any other flag and any other verb.
///
/// NEGATIVE CONTROL: with `lab_port`'s live-port exclusion removed (the check `lab_member`
/// makes of each port), the live fleet's port passes: `["join", "--at",
/// "192.168.1.117:7100"]`.
#[test]
fn members_cli_passes_only_a_phraseless_join_at_lab_members() {
    let words = |w: &[&str]| w.iter().map(|w| (*w).to_owned()).collect::<Vec<String>>();
    for passed in [
        words(&["join", "--at", "192.168.1.117:7190"]),
        words(&[
            "join",
            "--at",
            "192.168.1.117:7190",
            "--at",
            "192.168.1.229:7191",
        ]),
        words(&["join", "--at", "127.0.0.1:7190"]),
    ] {
        assert!(admitted_members(&passed), "{passed:?}");
    }
    for refused in [
        words(&["join"]),
        words(&["join", "--at"]),
        words(&["join", "--at", "192.168.1.117:7100"]),
        words(&["join", "--at", "192.168.1.117:7484"]),
        words(&["join", "--at", "192.168.1.117:80"]),
        words(&["join", "--at", "192.168.1.117"]),
        words(&["join", "--at", "m113.local:7190"]),
        words(&["join", "--at", "8.8.8.8:7190"]),
        words(&["join", "--at", "[fe80::1]:7190"]),
        words(&["join", "apple", "banana", "--at", "192.168.1.117:7190"]),
        words(&["join", "--at", "192.168.1.117:7190", "apple"]),
        words(&["join", "--no-wait", "--at", "192.168.1.117:7190"]),
        words(&["join", "--member", "m2", "--at", "192.168.1.117:7190"]),
        words(&["invite"]),
        words(&["allow", "m2"]),
        words(&["remove", "m2"]),
        words(&["status"]),
    ] {
        assert!(!admitted_members(&refused), "{refused:?}");
        let error = members_cli(
            Path::new("missing-executable"),
            Path::new("missing-world"),
            &refused,
        )
        .map(|_| ())
        .err()
        .map_or(0, |error| error.code);
        assert_eq!(error, 2, "{refused:?} must be a usage refusal before I/O");
    }
}

/// A vault key no build names for ALab.
const LAB_SIGNER: &str = "wfU3N368OzA1XVYhTwWSjwGzZf1DM/qCsRwdR+r4tLo=";

/// Pin the prepared world at `root` by hand, as `pin` leaves one: its vault read at `url`
/// pinned to `signer` (written as the file the shipped join writes, so nothing is fetched)
/// and, where given, [`LAB_VAULT`] naming `follows`.
fn configure(root: &Path, (url, signer): (&str, &str), follows: Option<&str>) {
    let config = format!(
        "url = \"{url}\"\nname = \"lab-1a2b\"\nsigner = \"{signer}\"\nfollowed = []\nhost = \"m4\"\n"
    );
    write_new(
        &Paths::for_home(&root.join("home")).dir.join("source.toml"),
        config.as_bytes(),
    )
    .unwrap();
    assert!(
        client::config(&Paths::for_home(&root.join("home")))
            .unwrap()
            .is_some_and(|config| matches!(
                &config.source,
                aterm_secrets::transport::Source::Url(at) if at == url
            )),
        "PRECONDITION: the pin reads back"
    );
    if let Some(follows) = follows {
        write_new(
            &World::open_at(root).unwrap().dir.join(LAB_VAULT),
            format!("{follows}#{signer}").as_bytes(),
        )
        .unwrap();
    }
}

/// `commons-lan members` runs the shipped join only in a world pinned to its URL-served lab
/// vault: the vault it reads is at a lab URL and [`LAB_VAULT`] names that URL. A world with no
/// vault, one whose [`LAB_VAULT`] names another lab vault than the one it reads, one that
/// records none, and one that reads its vault at a URL that is no lab's are refused before
/// anything runs; the pinned world reaches the executable, which is missing here.
///
/// NEGATIVE CONTROL: with `members_cli`'s `follows` check removed (`if false && !follows`), the
/// world with no vault reaches the executable: `bare: No such file or directory (os error 2)`.
#[test]
fn members_cli_runs_only_in_a_world_pinned_to_its_lab_vault() {
    let parent = crate::instance::world_parent();
    let (lab, other) = (
        "http://127.0.0.1:7192/v1/lab-1a2b",
        "http://127.0.0.1:7192/v1/lab-ffff",
    );
    let not_lab = "http://127.0.0.1:7192/v1/alab";
    let world = |name: &str, url: Option<&str>, follows: Option<&str>| {
        let root = parent.path().join(name);
        prepare_recipient(&root, LAB_FLEET_PORT).unwrap();
        if let Some(url) = url {
            configure(&root, (url, LAB_SIGNER), follows);
        }
        root
    };
    let at = [
        "join".to_owned(),
        "--at".to_owned(),
        "127.0.0.1:7190".to_owned(),
    ];
    let ran = |root: &Path| {
        members_cli(Path::new("missing-executable"), root, &at)
            .map(|_| ())
            .err()
            .and_then(|error| error.msg)
            .unwrap_or_default()
    };
    let unpinned = "members join needs a world pinned to its URL-served lab vault";
    for (name, url, follows) in [
        ("bare", None, None),
        ("crossed", Some(lab), Some(other)),
        ("unrecorded", Some(lab), None),
        ("unlabbed", Some(not_lab), Some(not_lab)),
    ] {
        let refused = ran(&world(name, url, follows));
        assert!(refused.contains(unpinned), "{name}: {refused}");
    }
    let pinned = ran(&world("pinned", Some(lab), Some(lab)));
    assert!(
        !pinned.is_empty() && !pinned.contains(unpinned),
        "CONTROL: the pinned world reaches the executable: {pinned}"
    );
}

/// A world's node holds a fleet key only at a lab port: `fleet_key` mints one by `aterm fleet
/// init --port <the world's lab port>` where its `[fleet] listen` names one, and mints no
/// second. A world whose listen names the live fleet's port, or none (one prepared before the
/// lab fleet port), is refused before anything runs, keyed or not. A keyless world of the live
/// ALab vault gets no key and runs nothing, whatever its listen.
///
/// NEGATIVE CONTROL (each run alone): with `fleet_key`'s port read as [`LAB_FLEET_PORT`] where
/// `fleet_listen_port` refuses (`.unwrap_or(LAB_FLEET_PORT)`), the world on the live fleet's
/// port is keyed: `live-port is refused: Ok(true)`. With its live-vault return removed, the
/// keyless live world is refused for its listen: `a live world is keyless:
/// Err(Some("…/live/cfg/aterm/aterm.toml's` … `(None) names no lab fleet port, …"))`.
#[test]
fn a_lab_node_takes_a_fleet_key_only_at_a_lab_port() {
    use std::os::unix::fs::OpenOptionsExt as _;
    let echo = aterm_uds::deadline::stand_in(Path::new("/bin/echo"));
    let missing = Path::new("missing-executable");
    let parent = crate::instance::world_parent();
    let world = |name: &str, listen: Option<&str>| {
        let root = parent.path().join(name);
        prepare_recipient(&root, 7191).unwrap();
        let world = World::open_at(&root).unwrap();
        let config = world.cfg.join("aterm/aterm.toml");
        let text = std::fs::read_to_string(&config).unwrap();
        let lab = "listen = \"0.0.0.0:7191\"\n";
        assert!(text.contains(lab), "PRECONDITION: {text}");
        std::fs::write(&config, text.replace(lab, listen.unwrap_or(""))).unwrap();
        world
    };
    let key = |world: &World| {
        std::fs::create_dir_all(world.state()).unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(world.state().join(aterm_net::fleet::KEY_FILE))
            .unwrap();
    };

    let lab = world("lab", Some("listen = \"0.0.0.0:7191\"\n"));
    let log = parent.path().join("lab-init.log");
    assert_eq!(fleet_key(&echo, &lab, &log).ok(), Some(true), "minted");
    assert_eq!(
        std::fs::read_to_string(&log).unwrap(),
        "fleet init --port 7191\n",
        "the shipped init at the world's lab port"
    );
    key(&lab);
    let again = fleet_key(missing, &lab, &parent.path().join("again.log"));
    assert_eq!(again.ok(), Some(false), "a keyed world mints no second key");

    for (name, listen, keyed) in [
        ("live-port", Some("listen = \"0.0.0.0:7100\"\n"), false),
        ("old", None, false),
        ("keyed-live-port", Some("listen = \"0.0.0.0:7100\"\n"), true),
    ] {
        let refused_world = world(name, listen);
        if keyed {
            key(&refused_world);
        }
        let log = parent.path().join(format!("{name}.log"));
        let refused =
            fleet_key(&echo, &refused_world, &log).map_err(|error| error.msg.unwrap_or_default());
        assert!(
            refused
                .as_ref()
                .is_err_and(|msg| msg.contains("names no lab fleet port")),
            "{name} is refused: {refused:?}"
        );
        assert!(!log.exists(), "{name}: nothing ran");
    }

    let live = world("live", None);
    configure(
        &parent.path().join("live"),
        (aterm_secrets::ALAB_DEFAULT_URL, aterm_secrets::ALAB_SIGNER),
        None,
    );
    assert!(
        client::config(&Paths::for_home(&live.home))
            .unwrap()
            .as_ref()
            .is_some_and(client::alab_vault),
        "PRECONDITION: a world of the live vault"
    );
    let kept =
        fleet_key(missing, &live, &parent.path().join("live.log")).map_err(|error| error.msg);
    assert_eq!(kept, Ok(false), "a live world is keyless: {kept:?}");
}

#[test]
fn successive_launches_share_the_profile_without_reusing_app_logs() {
    let parent = crate::instance::world_parent();
    let root = parent.path().join("recipient");
    prepare_recipient(&root, LAB_FLEET_PORT).unwrap();
    let (profile, _) = open(&root, false).unwrap();
    let retained = root.join("aterm.log");
    std::fs::write(&retained, b"earlier profile output").unwrap();
    let (first, _) = prepare_run(&root, false).unwrap();
    let first_log = first.dir.join("aterm.log");
    write_new(&first_log, b"first app output").unwrap();
    let (second, _) = prepare_run(&root, false).unwrap();
    assert_ne!(first.dir, second.dir);
    assert_eq!(first.home, profile.home);
    assert_eq!(second.home, profile.home);
    assert_eq!(first.cfg, second.cfg);
    assert_eq!(first.run, second.run);
    write_new(&second.dir.join("aterm.log"), b"second app output").unwrap();
    drop(first);
    drop(second);
    assert_eq!(std::fs::read(&first_log).unwrap(), b"first app output");
    assert_eq!(std::fs::read(&retained).unwrap(), b"earlier profile output");
}

/// A world pinned to the live default ALab vault (as an earlier lab run left one) opens for no
/// verb without the flag that names that vault and the member it keeps, while a synthetic
/// vault's world opens as before. The live pin is written as a file: nothing is fetched.
/// Negative control: disabling `open`'s live-vault refusal opens the live world without the
/// flag and fails the refusal's `let`-`else` (run 2026-11-02, then restored).
#[test]
fn a_world_on_the_live_vault_opens_only_behind_the_flag() {
    use std::os::unix::fs::OpenOptionsExt as _;
    let parent = crate::instance::world_parent();
    let synthetic = parent.path().join("synthetic");
    let source = parent.path().join("source");
    let pending = prepare_recipient(&synthetic, LAB_FLEET_PORT).unwrap();
    let prepared = prepare_source(
        &source,
        &[pending["recipient"].as_str().unwrap().into()],
        None,
        LAB_FLEET_PORT,
        None,
    )
    .unwrap();
    join_recipient(
        &synthetic,
        &aterm_json::to_vec(&prepared["capsules"][0]).unwrap(),
        None,
    )
    .unwrap();
    assert!(
        open(&synthetic, false).is_ok(),
        "a synthetic vault's world opens"
    );

    let live = parent.path().join("live");
    prepare_recipient(&live, LAB_FLEET_PORT).unwrap();
    let pin = format!(
        "url = \"{}\"\nname = \"alab\"\nsigner = \"{}\"\nfollowed = []\nhost = \"lab-auto-test\"\n",
        aterm_secrets::ALAB_DEFAULT_URL,
        aterm_secrets::ALAB_SIGNER
    );
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(Paths::for_home(&live.join("home")).dir.join("source.toml"))
        .unwrap()
        .write_all(pin.as_bytes())
        .unwrap();
    let configured = client::config(&Paths::for_home(&live.join("home")))
        .unwrap()
        .expect("the live pin reads back");
    assert!(client::alab_vault(&configured));
    let Err(refused) = open(&live, false) else {
        panic!("a live world needs the flag");
    };
    assert_eq!(refused.code, 1);
    assert!(refused.msg.as_deref().is_some_and(|msg| msg.contains(LIVE)));
    let refused = database(
        Path::new("missing-executable"),
        &live,
        &[
            "accounts-scan".into(),
            "--collection".into(),
            "accounts".into(),
        ],
        false,
    )
    .unwrap_err();
    assert!(refused.msg.as_deref().is_some_and(|msg| msg.contains(LIVE)));
    // `home-lan` and `members` open no live world, whatever flag is given.
    let refused = home_lan(&live, "192.168.1.0/24").unwrap_err();
    assert!(refused.msg.as_deref().is_some_and(|msg| msg.contains(LIVE)));
    let at = [
        "join".to_owned(),
        "--at".to_owned(),
        "192.168.1.117:7190".to_owned(),
    ];
    let refused = members_cli(Path::new("missing-executable"), &live, &at).unwrap_err();
    assert!(refused.msg.as_deref().is_some_and(|msg| msg.contains(LIVE)));
    assert!(open(&live, true).is_ok());
}

#[test]
fn prepared_recipient_has_no_membership_records_or_application_table() {
    let parent = crate::instance::world_parent();
    let recipient = parent.path().join("recipient");
    let source = parent.path().join("source");
    let pending = prepare_recipient(&recipient, LAB_FLEET_PORT).unwrap();
    let prepared = prepare_source(
        &source,
        &[pending["recipient"].as_str().unwrap().into()],
        None,
        LAB_FLEET_PORT,
        None,
    )
    .unwrap();
    let capsule = aterm_json::to_vec(&prepared["capsules"][0]).unwrap();
    let wrong = parent.path().join("wrong-recipient");
    prepare_recipient(&wrong, LAB_FLEET_PORT).unwrap();
    assert!(join_recipient(&wrong, &capsule, None).is_err());
    assert!(!wrong.join("fixture.vault").exists());
    let joined = join_recipient(&recipient, &capsule, None).unwrap();
    let source_bootstrap = Bootstrap::required(&Paths::for_home(&source.join("home"))).unwrap();
    let recipient_bootstrap =
        Bootstrap::required(&Paths::for_home(&recipient.join("home"))).unwrap();
    assert_eq!(
        source_bootstrap.trust().network,
        recipient_bootstrap.trust().network
    );
    assert_eq!(joined["commons_admitted"], json!(false));
    assert_eq!(joined["membership_records"], json!(false));
    assert_eq!(joined["application_tables"], json!(0));
    assert!(join_recipient(&recipient, &capsule, None).is_err());
    assert_eq!(
        recipient_key(&Paths::for_home(&recipient.join("home"))).unwrap(),
        pending["recipient"].as_str().unwrap()
    );
}

#[test]
fn startup_observer_cannot_recreate_missing_custody() {
    let parent = crate::instance::world_parent();
    let recipient = parent.path().join("recipient");
    let source = parent.path().join("source");
    let pending = prepare_recipient(&recipient, LAB_FLEET_PORT).unwrap();
    prepare_source(
        &source,
        &[pending["recipient"].as_str().unwrap().into()],
        None,
        LAB_FLEET_PORT,
        None,
    )
    .unwrap();
    let paths = Paths::for_home(&source.join("home"));
    drop(Bootstrap::required(&paths).unwrap());
    assert!(observed_admission(&paths, Instant::now() + HANG));
    let lock = paths.dir.join("commons.lock");
    std::fs::remove_file(&lock).unwrap();

    assert!(!observed_admission(&paths, Instant::now() + HANG));
    assert!(!lock.exists(), "observing startup must not prepare custody");

    // The old probe is the negative control: merely asking it to read the
    // same prepared source recreates the missing custody lock.
    drop(Bootstrap::required(&paths).unwrap());
    assert!(lock.exists());
    assert!(observed_admission(&paths, Instant::now() + HANG));
}
