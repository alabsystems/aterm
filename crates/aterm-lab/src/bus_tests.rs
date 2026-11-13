// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::unix::fs::PermissionsExt as _;

/// The fake mint: `<grant> <tag>` for each grant, the tag a counter, failing on the grant
/// numbered `fail_at` (from 1).
fn fake_mint(fail_at: Option<usize>) -> impl FnMut(&[String]) -> Result<Vec<u8>, String> {
    let mut n = 0;
    move |argv: &[String]| {
        n += 1;
        assert_eq!(argv[1..3], ["link", "mint"], "{argv:?}");
        assert_eq!(argv[4], "--secret-file", "{argv:?}");
        if fail_at == Some(n) {
            return Err("refused QZP6WORD1".to_string());
        }
        Ok(format!("{} {n:04x}\n", argv[3]).into_bytes())
    }
}

fn mode(p: &Path) -> u32 {
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

fn grants_of(cap: &Path) -> Vec<String> {
    std::fs::read_to_string(cap)
        .unwrap()
        .lines()
        .map(|l| l.rsplit_once(' ').unwrap().0.to_string())
        .collect()
}

/// A LAB BUS LAYS THE NODE RING AND AN OBSERVER RING: the keys (program link, 32-byte secret,
/// `node.cap` with the node's grants in ceiling order, `observer.cap` with an `a-` service ring)
/// in one dir and the bridge's state in another, dirs 0700 and secrets 0600; the table's
/// command is plain words naming the live socket, the node cap, the live state and the node;
/// the rendezvous file renders and parses back with that command. A failed mint makes no cap
/// and names its grant; no observer asked, no observer cap.
#[test]
fn a_lab_bus_lays_the_node_ring_and_an_observer_ring() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let (keys, live) = (root.join("run/fabric"), root.join("sock/fabric"));
    std::fs::create_dir_all(root.join("run")).unwrap();
    std::fs::create_dir_all(root.join("sock")).unwrap();
    let aterm = Path::new("/b/aterm");
    assert!(aterm_link::person::is_person_id("a-lab-observer"));
    let laid = lay(
        aterm,
        &keys,
        &live,
        Some("a-lab-observer"),
        &mut fake_mint(None),
    )
    .unwrap();
    assert_eq!(
        grants_of(&laid.cap),
        astream_cap::GrantCeiling::aterm_node_ring(FLEET, &laid.node)
            .unwrap()
            .rules()
    );
    let observer = laid.observer.clone().expect("an observer cap");
    assert_eq!(observer, keys.join("observer.cap"));
    assert_eq!(
        grants_of(&observer),
        astream_cap::GrantCeiling::aterm_person_ring(FLEET, "a-lab-observer")
            .unwrap()
            .rules()
    );
    assert!(
        laid.node.len() == 18
            && laid.node.starts_with("n-")
            && laid.node[2..].bytes().all(|b| b.is_ascii_hexdigit()),
        "{}",
        laid.node
    );
    assert_eq!(
        std::fs::read_to_string(laid.state.join("node")).unwrap(),
        format!("{}\n", laid.node)
    );
    assert_eq!(std::fs::read_link(&laid.program).unwrap(), aterm);
    assert_eq!(std::fs::read(&laid.secret).unwrap().len(), 32);
    for d in [&keys, &live, &laid.state] {
        assert_eq!(mode(d), 0o700, "{}", d.display());
    }
    for f in [&laid.secret, &laid.cap, &observer, &laid.state.join("node")] {
        assert_eq!(mode(f), 0o600, "{}", f.display());
    }
    let command = laid.command();
    for word in &command {
        assert_eq!(aterm_link::enable::argv_safe(word), Ok(()), "{word}");
    }
    let line = command.join(" ");
    for want in [
        format!("--broker {}", live.join("b.sock").display()),
        format!("--cap-file {}", keys.join("node.cap").display()),
        format!("--state {}", live.join("state").display()),
        format!("--accept-from {}", laid.node),
    ] {
        assert!(line.contains(&want), "{want}: {line}");
    }
    let doc: aterm_toml::Value = aterm_toml::from_str(&laid.table).unwrap();
    assert_eq!(
        doc.get("fabric")
            .and_then(|t| t.get("command"))
            .and_then(aterm_toml::Value::as_str),
        Some(line.as_str())
    );
    assert_eq!(
        laid.broker_argv(),
        [
            keys.join("aterm").display().to_string(),
            "link".into(),
            "broker".into(),
            live.join("b.sock").display().to_string(),
            live.join("b.log").display().to_string(),
            "--secret-file".into(),
            keys.join("mint.secret").display().to_string(),
        ]
    );
    let back = aterm_link::enable::Rendezvous::parse(&laid.rendezvous().render()).expect("parses");
    assert_eq!(back, laid.rendezvous());
    assert_eq!(back.command, command);
    assert_eq!(
        (back.fleet.as_str(), back.node.as_str()),
        (FLEET, laid.node.as_str())
    );

    let failed = lay(
        aterm,
        &root.join("run2/fabric"),
        &root.join("sock2/fabric"),
        Some("a-lab-observer"),
        &mut fake_mint(Some(3)),
    )
    .expect_err("the third grant is refused");
    let third = &astream_cap::GrantCeiling::aterm_node_ring(FLEET, "n-0")
        .unwrap()
        .rules()[2];
    assert!(
        failed.contains(&format!("aterm link mint {third}: ")) && failed.contains("QZP6WORD1"),
        "{failed}"
    );
    assert!(!root.join("run2/fabric/node.cap").exists());
    assert!(!root.join("run2/fabric/observer.cap").exists());
    // The observer's first grant is the mint after the node ring's last.
    let node_grants = astream_cap::GrantCeiling::aterm_node_ring(FLEET, "n-0")
        .unwrap()
        .rules()
        .len();
    let failed_observer = lay(
        aterm,
        &root.join("run3/fabric"),
        &root.join("sock3/fabric"),
        Some("a-lab-observer"),
        &mut fake_mint(Some(node_grants + 1)),
    )
    .expect_err("the observer's first grant is refused");
    assert!(
        failed_observer.contains("a-lab-observer"),
        "{failed_observer}"
    );
    assert!(!root.join("run3/fabric/observer.cap").exists());

    let plain = lay(
        aterm,
        &root.join("run4/fabric"),
        &root.join("sock4/fabric"),
        None,
        &mut fake_mint(None),
    )
    .unwrap();
    assert_eq!(plain.observer, None);
    assert!(!root.join("run4/fabric/observer.cap").exists());
    assert!(
        lay(
            aterm,
            &root.join("run5/fabric"),
            &root.join("sock5/fabric"),
            Some("lab-observer"),
            &mut fake_mint(None),
        )
        .is_err()
    );
}

/// The listen wait returns once the socket stands, and a broker that exits first ends it
/// with what the broker said.
#[test]
fn a_broker_that_exits_before_it_listens_ends_the_wait_with_what_it_said() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let sock = tmp.path().join("b.sock");
    let e = await_listening(&sock, LISTEN_HANG, || true, || " no QZP6WORD2\n".into())
        .expect_err("gone");
    assert_eq!(e, "the lab's broker never listened: no QZP6WORD2");
    std::fs::write(&sock, "").unwrap();
    assert_eq!(
        await_listening(&sock, LISTEN_HANG, || true, String::new),
        Ok(())
    );
}

/// What a current `aterm link broker` prints after each usage error: its usage, which names
/// `--lifeline-fd` (its head and its row).
const CURRENT_USAGE: &str = "usage: aterm link broker <socket> [log] [--secret-file <path>] \
    [--durability <tier>]\n                         [--lifeline-fd <n>]\n  \
    --lifeline-fd <n>    descriptor <n> is the read end of a pipe or FIFO its launcher holds";

/// A broker older than the lifeline (exit 2, its parser refusing `--lifeline-fd` as an
/// unknown flag, its usage naming no such flag) starts no bus as a refusal of its own, which
/// a proof ends NOT RUN, naming the verb. A current broker's refusals are failures: a usage
/// error or a lifeline it cannot adopt, each followed by its usage naming the flag, a parser
/// that lost the flag its usage still names, and the old refusal's words with any exit but 2;
/// so is a refusal that names the flag once but refuses no unknown flag. One that listens
/// starts the bus. Each is a stand-in aterm whose `link mint` answers every grant; the old one
/// refuses only an argv that holds the flag, so it shows the lifeline armed.
#[test]
fn a_broker_that_refuses_its_lifeline_is_no_failure() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let start = |name: &str, broker: &str| {
        let aterm = tmp.path().join(name);
        std::fs::write(
            &aterm,
            format!(
                "#!/bin/sh\nusage() (printf '%s\\n' '{usage}' >&2)\n\
                 old='aterm link broker: unknown flag `--lifeline-fd`'\n\
                 case \"$2\" in\nmint) echo \"$3 00\" ;;\nbroker) {broker} ;;\nesac\n",
                usage = CURRENT_USAGE,
            ),
        )
        .unwrap();
        std::fs::set_permissions(&aterm, std::fs::Permissions::from_mode(0o755)).unwrap();
        Fabric::start(&aterm, &tmp.path().join(format!("{name}.bus"))).err()
    };
    let listen = ": > \"$3\"; read x";
    let old = start(
        "old",
        &format!(
            "for a; do if [ \"$a\" = --lifeline-fd ]; then echo \"$old\" >&2; \
             echo 'usage: aterm link broker <socket> [log] [--secret-file <path>]' >&2; \
             exit 2; fi; done; {listen}"
        ),
    );
    assert_eq!(old, Some(Unstarted::NoLifeline));
    for (name, broker, words) in [
        (
            "usage",
            "echo 'aterm link broker: --durability needs a value' >&2; usage; exit 2",
            "--durability needs a value",
        ),
        (
            "adopt",
            "echo 'aterm link broker: --lifeline-fd 0 is not an open descriptor' >&2; usage; \
             exit 2",
            "--lifeline-fd 0 is not an open descriptor",
        ),
        (
            "once",
            "echo 'aterm link broker: --lifeline-fd 0 is not an open descriptor' >&2; exit 2",
            "--lifeline-fd 0 is not an open descriptor",
        ),
        (
            "lost",
            "echo \"$old\" >&2; usage; exit 2",
            "unknown flag `--lifeline-fd`",
        ),
        (
            "exit1",
            "echo \"$old\" >&2; exit 1",
            "unknown flag `--lifeline-fd`",
        ),
    ] {
        let got = start(name, broker);
        assert!(
            matches!(&got, Some(Unstarted::Failed(why)) if why.contains(words)),
            "{name}: {got:?}"
        );
    }
    assert_eq!(start("current", listen), None, "it listens");
    let aterm = Path::new("/old/aterm");
    let not_run = Unstarted::NoLifeline.fail("proof", aterm);
    assert_eq!(not_run.code, 3, "{:?}", not_run.msg);
    assert_eq!(
        not_run.msg.as_deref(),
        Some(
            "NOT RUN: proof: /old/aterm refuses --lifeline-fd: it predates the lifeline \
             every lab launch is armed with"
        )
    );
    assert_eq!(Unstarted::Failed("no".into()).fail("proof", aterm).code, 1);
}
