// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the menu-bar fleet glance.

use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

fn peer(label: &str, header: &[(&str, &str)], rows: &[&[(&str, &str)]]) -> String {
    let object = |pairs: &[(&str, &str)]| {
        let members: Vec<String> = pairs
            .iter()
            .map(|(k, v)| format!("\"{k}\":\"{v}\""))
            .collect();
        format!("{{{}}}", members.join(","))
    };
    let rows: Vec<String> = rows.iter().map(|r| object(r)).collect();
    format!(
        "{{\"label\":\"{label}\",\"fleet\":true,\"instance\":{},\"sessions\":[{}]}}",
        object(header),
        rows.join(",")
    )
}

fn unanswered(label: &str, why: &str) -> String {
    format!("{{\"label\":\"{label}\",\"fleet\":true,\"error\":\"{why}\"}}")
}

fn header(node: &str) -> Vec<(&'static str, String)> {
    vec![
        ("pid", "77".to_string()),
        ("host", "mac.local".to_string()),
        ("sessions", "1".to_string()),
        ("level", "facts".to_string()),
        ("cut", "0".to_string()),
        ("node", node.to_string()),
    ]
}

fn pairs<'a>(v: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    v.iter().map(|(k, s)| (*k, s.as_str())).collect()
}

const QUIET: &[(&str, &str)] = &[
    ("sid", "s-q"),
    ("state", "alive"),
    ("program", "zsh"),
    ("program_kind", "shell"),
    ("agent", "-"),
    ("needs", "-"),
];

#[test]
fn a_fleet_read_parses_to_its_peers() {
    let mut hosts = vec![
        "{\"label\":\"pid 1\",\"instance\":{\"pid\":\"1\"},\"sessions\":[]}".to_string(),
        "{\"label\":\"pid 2\",\"instance\":{\"pid\":\"2\"},\"sessions\":[]}".to_string(),
        peer("self", &pairs(&header("own-node")), &[QUIET]),
    ];
    let stalled: &[(&str, &str)] = &[
        ("sid", "s-w"),
        ("state", "alive"),
        ("program", "claude"),
        ("program_kind", "agent"),
        ("agent", "busy"),
        ("task", "stalled"),
        ("task_from", "s-1e91aaaa"),
        ("task_ms", "120000"),
        ("needs", "stalled"),
    ];
    let manager: &[(&str, &str)] = &[
        ("sid", "s-1e91aaaa"),
        ("state", "alive"),
        ("program", "claude"),
        ("program_kind", "agent"),
        ("agent", "idle"),
        ("role", "lab-sweep"),
        ("needs", "-"),
    ];
    hosts.push(peer("mac-17", &pairs(&header("n17")), &[stalled, manager]));
    let older_row: &[(&str, &str)] = &[
        ("sid", "s-o"),
        ("state", "alive"),
        ("program", "claude"),
        ("program_kind", "agent"),
        ("agent", "prompt"),
        ("attention", "1"),
    ];
    hosts.push(peer("mac-old", &pairs(&header("nold")), &[older_row]));
    hosts.push(peer(
        "mac-old2",
        &pairs(&header("nold2")),
        &[older_row, QUIET],
    ));
    let mut cut = header("ncut");
    cut[2].1 = "5".to_string();
    hosts.push(peer("mac-cut", &pairs(&cut), &[QUIET, QUIET]));
    for n in 0..24 {
        hosts.push(peer(
            &format!("mac-{n:02}"),
            &pairs(&header(&format!("q{n}"))),
            &[QUIET],
        ));
    }
    for n in 0..3 {
        hosts.push(unanswered(&format!("mac-t{n}"), "no connection in 3 s"));
    }
    let json = format!("[{}]", hosts.join(","));
    let snap = parse(&json, Some("own-node"), 1_000).expect("a survey");
    assert_eq!(snap.macs.len() + snap.unanswered.len(), 31);
    assert_eq!(snap.unanswered.len(), 3);
    assert!(
        !snap
            .macs
            .iter()
            .any(|m| m.dial == "self" || m.dial.starts_with("pid "))
    );
    let m17 = snap
        .macs
        .iter()
        .find(|m| m.dial == "mac-17")
        .expect("mac-17");
    assert_eq!(m17.needs.len(), 1);
    assert_eq!(m17.needs[0].word, NeedWord::Stalled);
    assert_eq!(m17.needs[0].age_ms, Some(120_000));
    assert_eq!(
        m17.needs[0].worker_of.as_deref(),
        Some(crate::presence::short_sid("s-1e91aaaa").as_str())
    );
    assert_eq!(
        m17.activity,
        Some(AgentTally {
            agents: 2,
            busy: 1,
            idle: 1,
            ..AgentTally::default()
        })
    );
    assert_eq!(m17.managers.len(), 1);
    assert_eq!(m17.managers[0].workers, 1);
    assert_eq!(m17.host.as_deref(), Some("mac.local"));
    let old = snap
        .macs
        .iter()
        .find(|m| m.dial == "mac-old")
        .expect("older");
    assert!(old.older && old.needs.is_empty(), "never counted");
    assert_eq!(snap.macs.iter().filter(|m| m.older).count(), 2);
    assert!(
        snap.macs
            .iter()
            .filter(|m| m.older)
            .all(|m| m.needs.is_empty())
    );
    let cut = snap.macs.iter().find(|m| m.dial == "mac-cut").expect("cut");
    assert_eq!(cut.left_out, 3);
    // Only the Macs whose activity counts: mac-17's two; the older Macs' agents are not read.
    assert!(
        snap.macs
            .iter()
            .filter(|m| m.older)
            .all(|m| m.activity.is_none())
    );
    assert_eq!(snap.agents(), Some(2));
    let with_self = parse(&json, None, 1_000).expect("a survey");
    assert_eq!(with_self.macs.len(), snap.macs.len() + 1);
    assert!(parse("not json", None, 0).is_err());
}

/// A PEER'S INSTANCES READ ARE ITS NODE'S WORD: a node service that read every window says so
/// (`instances_read=`), and only instances past those go unread; a window that answered alone read
/// itself.
#[test]
fn a_peers_instances_read_are_its_nodes_word() {
    let with = |dial: &str, extra: &[(&'static str, &str)]| {
        let mut header = header(&format!("n-{dial}"));
        header.extend(extra.iter().map(|(k, v)| (*k, (*v).to_string())));
        peer(dial, &pairs(&header), &[QUIET])
    };
    let json = format!(
        "[{},{},{}]",
        with("mac-all", &[("instances", "2"), ("instances_read", "2")]),
        with("mac-some", &[("instances", "3"), ("instances_read", "2")]),
        with("mac-one", &[("instances", "2")]),
    );
    let snap = parse(&json, None, 1_000).expect("a survey");
    let read = |dial: &str| {
        let mac = snap.macs.iter().find(|m| m.dial == dial).expect(dial);
        (mac.instances_read, mac.instances_unread)
    };
    assert_eq!(read("mac-all"), (2, 0));
    assert_eq!(read("mac-some"), (2, 1));
    assert_eq!(read("mac-one"), (1, 1));
}

fn unproven(entry: &str) -> String {
    entry.replacen("\"fleet\":true", "\"fleet\":true,\"unproven\":true", 1)
}

#[test]
fn an_unproven_peer_is_listed_on_its_word_alone() {
    let stalled: &[(&str, &str)] = &[
        ("sid", "s-w"),
        ("state", "alive"),
        ("program", "claude"),
        ("program_kind", "agent"),
        ("agent", "busy"),
        ("task", "stalled"),
        ("task_ms", "1000"),
        ("needs", "stalled"),
    ];
    let mut claims = header("n9");
    claims[1].1 = "elsewhere.example".to_string();
    claims.push(("drive", "1".to_string()));
    let me = peer("self.local", &pairs(&header("own-node")), &[QUIET]);
    let studio = peer("studio.local", &pairs(&claims), &[stalled]);
    type Row = (bool, usize, Option<&'static str>, &'static str, bool, bool);
    let table: [Row; 2] = [
        (false, 1, Some("n9"), "elsewhere.example", true, true),
        (true, 2, None, "studio.local", false, false),
    ];
    for (marked, listed, node, host, drive, counts) in table {
        let entries = [me.clone(), studio.clone()];
        let entries = entries.map(|e| if marked { unproven(&e) } else { e });
        let snap =
            parse(&format!("[{}]", entries.join(",")), Some("own-node"), 0).expect("a survey");
        assert_eq!(snap.macs.len(), listed, "unproven={marked}: {snap:?}");
        let m = snap
            .macs
            .iter()
            .find(|m| m.dial == "studio.local")
            .expect("studio");
        assert_eq!(m.unproven, marked);
        assert_eq!(m.node.as_deref(), node);
        assert_eq!(m.ident(), node.unwrap_or("studio.local"));
        assert_eq!(m.host.as_deref(), Some(host));
        assert_eq!((m.drive, m.counts()), (drive, counts), "unproven={marked}");
        assert_eq!(m.needs.len(), 1, "its needs are read either way");
        if marked {
            let me = snap.macs.iter().find(|m| m.dial == "self.local");
            assert!(
                me.is_some_and(|m| m.unproven && m.node.is_none()),
                "a claim of this Mac's node is listed, not dropped: {me:?}"
            );
        }
    }
}

#[test]
fn a_fleet_need_names_its_tab_as_a_local_one_does() {
    let need_of = |level: &str, needs: &str, extra: &[(&str, &str)]| {
        let mut row: Vec<(&str, &str)> = vec![
            ("sid", "s-a"),
            ("state", "alive"),
            ("program", "claude"),
            ("program_kind", "agent"),
            ("agent", "idle"),
            ("agent_ms", "10800000"),
            ("task_ms", "5000"),
            ("needs", needs),
        ];
        row.extend_from_slice(extra);
        let mut head = header("n1");
        head[3].1 = level.to_string();
        let json = format!("[{}]", peer("mac-1", &pairs(&head), &[row.as_slice()]));
        let snap = parse(&json, None, 1_000).expect("a survey");
        let need = snap.macs[0].needs[0].clone();
        (need.who, need.age_ms)
    };
    let spun = need_of("content", "ask", &[("title", "\u{25d0} fix the build")]);
    assert_eq!(spun, ("fix the build".to_string(), None));
    assert_eq!(
        need_of("content", "ask", &[("title", "\u{25d1} fix the build")]),
        spun,
        "the next spinner frame is the same row"
    );
    let idle = need_of("content", "ask", &[("title", "\u{2733} fix the build")]);
    assert_eq!(idle.0, "fix the build");
    let roles = [("role", "review"), ("user_title", "mine"), ("title", "x")];
    assert_eq!(need_of("content", "ask", &roles).0, "review");
    assert_eq!(need_of("content", "ask", &roles[1..]).0, "mine");
    assert_eq!(need_of("content", "ask", &[]).0, "claude", "the program");
    assert_eq!(
        need_of("facts", "ask", &[("title", "\u{25d0} secret")]).0,
        "claude",
        "facts: no title"
    );
    assert_eq!(need_of("facts", "stalled", &[]).1, Some(5000));
}

/// A peer's harness report (`needs_by=harness`) carries on, at facts and content alike, so this
/// Mac's Needs you lists it after the stopped work (§14.7); CONTROL: a direct ask (`-`) and a
/// program's own report (`program`) have stopped their agents.
#[test]
fn a_peers_harness_report_carries_on_and_a_direct_ask_does_not() {
    let carries_on = |level: &str, by: &str| {
        let fields: Vec<(&str, &str)> = vec![
            ("sid", "s-a"),
            ("state", "alive"),
            ("program", "claude"),
            ("program_kind", "agent"),
            ("agent", "busy"),
            ("agent_ms", "1000"),
            ("needs", "ask"),
            ("needs_by", by),
        ];
        let mut head = header("n1");
        head[3].1 = level.to_string();
        let json = format!("[{}]", peer("mac-1", &pairs(&head), &[fields.as_slice()]));
        parse(&json, None, 1_000).expect("a survey").macs[0].needs[0].carries_on
    };
    for level in ["facts", "content"] {
        assert!(carries_on(level, "harness"), "{level}");
        assert!(!carries_on(level, "-"), "{level}: CONTROL, a direct ask");
        assert!(
            !carries_on(level, "program"),
            "{level}: CONTROL, the program's own report"
        );
    }
}

#[test]
fn a_content_peer_says_what_its_need_is_and_a_freeze_dates_only_an_agent() {
    let need_of = |level: &str, needs: &str, row: &[(&str, &str)]| {
        let mut fields: Vec<(&str, &str)> = vec![
            ("sid", "s-a"),
            ("state", "alive"),
            ("program", "claude"),
            ("agent_ms", "10800000"),
            ("needs", needs),
        ];
        fields.extend_from_slice(row);
        let mut head = header("n1");
        head[3].1 = level.to_string();
        let json = format!("[{}]", peer("mac-1", &pairs(&head), &[fields.as_slice()]));
        parse(&json, None, 1_000).expect("a survey").macs[0].needs[0].clone()
    };
    let agent = [("program_kind", "agent")];
    let ask = [
        ("program_kind", "agent"),
        ("agent", "idle"),
        ("attention", "which branch: main or release?"),
    ];
    assert_eq!(
        need_of("content", "ask", &ask).what.as_deref(),
        Some("which branch: main or release?")
    );
    assert_eq!(need_of("facts", "ask", &[("attention", "1")]).what, None);
    let harness = [
        ("program_kind", "agent"),
        ("agent", "idle"),
        (
            "attention",
            "claude needs you: grant Screen Recording to com.aterm.aterm in System Settings \
             \u{25b8} \u{2026} (+1 more) \u{2014} carrying on",
        ),
    ];
    let what = need_of("content", "ask", &harness).what.expect("words");
    assert!(
        what.starts_with("grant Screen Recording") && what.ends_with("\u{2026} (+1 more)"),
        "{what}"
    );
    assert!(what.len() <= crate::needs::ROW_WHAT_MAX, "{what}");
    let boxed = [
        ("program_kind", "agent"),
        ("agent", "prompt"),
        ("agent_detail", "bash:not-read-only"),
    ];
    let band = crate::presence::prompt_band_word(Some("bash:not-read-only"));
    assert_eq!(need_of("content", "approval", &boxed).what, Some(band));
    assert_eq!(need_of("facts", "approval", &boxed).what, None);
    // A need the row says is the program's own report says so at every level, in aterm's fixed
    // words; the banner (`escalation`) reads them too.
    let claimed = [
        ("program_kind", "agent"),
        ("agent", "prompt"),
        ("needs_by", "program"),
    ];
    for level in ["facts", "content"] {
        let need = need_of(level, "approval", &claimed);
        assert_eq!(
            need.claim,
            Some(crate::needs::CODEX_APPROVAL_WORDS),
            "{level}"
        );
        assert_eq!(
            need.what.as_deref(),
            Some(crate::needs::CODEX_APPROVAL_WORDS),
            "{level}"
        );
    }
    let signing = [
        ("program_kind", "agent"),
        ("agent", "wall:auth"),
        ("needs_by", "program"),
    ];
    assert_eq!(
        need_of("facts", "approval", &signing).claim,
        Some(crate::needs::CODEX_SIGN_IN_WORDS)
    );
    assert_eq!(need_of("facts", "approval", &boxed).claim, None, "CONTROL");
    let walled = [("program_kind", "agent"), ("agent", "wall:spend")];
    assert_eq!(
        need_of("content", "wall", &walled).what,
        Some(crate::needs::wall_what("spend", None))
    );
    assert_eq!(need_of("content", "frozen", &agent).what, None, "a stall's");
    let plain = [("program_kind", "program"), ("agent", "-")];
    assert_eq!(need_of("content", "frozen", &plain).age_ms, None);
    assert_eq!(need_of("content", "stopped", &plain).age_ms, None);
    let frozen_agent = [("program_kind", "agent"), ("agent", "wall:unresponsive")];
    assert_eq!(
        need_of("facts", "frozen", &frozen_agent).age_ms,
        Some(10_800_000)
    );
}

/// WHAT THE BOOK SAYS OF THIS MAC (`read_book`): no fleet key is no fleet; the
/// code file identifies the Mac that minted it, and peers that joined
/// it no longer establish that local fact (F17); a member whose book holds
/// only its issuer, and no copy of that issuer's book (an older issuer, or no
/// read yet), knows only it, and names it.
#[cfg(unix)]
#[test]
fn the_book_says_whether_this_mac_knows_only_its_issuer() {
    use aterm_net::fleet_member::{Peer, Via, update_peers};
    let dir = std::env::temp_dir().join(format!("aterm-read-book-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    assert_eq!(read_book(&dir), BookFacts::default(), "no key: no fleet");
    std::fs::write(dir.join(aterm_net::fleet::KEY_FILE), b"key").unwrap();
    let peer = |host: &str, via: Via| Peer {
        node: (via != Via::Manual).then(|| format!("{:032x}", host.len())),
        key: None,
        key_from: None,
        host: host.to_string(),
        addr: None,
        port: 7100,
        version: None,
        seen_ms: None,
        via,
        fleet: None,
        signed: None,
    };
    let book = |peers: Vec<Peer>| {
        update_peers(&dir, |book| {
            *book = peers;
            Ok(())
        })
        .expect("the book written");
        read_book(&dir)
    };
    let member = book(vec![peer("m7", Via::Issuer)]);
    assert_eq!(
        (
            member.in_fleet,
            member.minted,
            member.issuer_label.as_deref()
        ),
        (true, false, Some("m7"))
    );
    assert_eq!(member.unbound_issuer.as_deref(), Some("m7"));
    let key = aterm_net::fleet::NodeIdentity::from_seed(&format!("{:032x}", 2), &[7; 32])
        .expect("a node key")
        .key();
    let bound = book(vec![Peer {
        key: Some(key),
        ..peer("m7", Via::Issuer)
    }]);
    assert_eq!(
        (bound.issuer_label.as_deref(), bound.unbound_issuer),
        (Some("m7"), None),
        "its issuer's key is bound"
    );
    let joined = book(vec![peer("mac-1", Via::Joined)]);
    assert!(
        !joined.minted && joined.issuer_label.is_none(),
        "peers that joined it say no more that this Mac founded the fleet"
    );
    assert_eq!(
        joined.unbound_issuer, None,
        "a peer that joined is no issuer"
    );
    let code = dir.join(aterm_net::fleet_member::CODE_FILE);
    std::fs::write(&code, b"code\n").unwrap();
    assert!(
        read_book(&dir).minted,
        "the code file identifies where the code was minted"
    );
    std::fs::remove_file(&code).unwrap();
    let more = book(vec![peer("m7", Via::Issuer), peer("mac-9", Via::Manual)]);
    assert_eq!(
        (more.minted, more.issuer_label),
        (false, None),
        "a peer named by hand is read too"
    );
    let m7 = peer("m7", Via::Issuer).node.expect("an issuer's node");
    let keep = |issuer: &str| {
        aterm_net::fleet_member::hold_book(&dir).and_then(|book| {
            book.keep_issuer_book(Some((issuer, &[])))
                .map_err(|e| e.to_string())
        })
    };
    keep(&m7).expect("the copy written");
    let shared = book(vec![peer("m7", Via::Issuer)]);
    assert_eq!(shared.issuer_label, None, "its issuer shares its book");
    keep(&"ff".repeat(16)).expect("another issuer's copy written");
    let stale = book(vec![peer("m7", Via::Issuer)]);
    assert_eq!(
        stale.issuer_label.as_deref(),
        Some("m7"),
        "another issuer's copy is no book of this one's"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_peer_known_by_name_is_dialled_by_it() {
    let exited: &[(&str, &str)] = &[("sid", "s-x"), ("state", "exited"), ("needs", "-")];
    let at = |addr: &str| {
        let mut head = header("AB12CD34EF56AB12CD34EF56AB12CD34");
        head[1].1 = "mac-17.local".to_string();
        head[2].1 = "3".to_string();
        format!(
            "[{},{},{},{}]",
            peer(
                &format!("mac-17.local at {addr}"),
                &pairs(&head),
                &[QUIET, QUIET, exited]
            ),
            peer("lab-mini:7101 at 10.0.0.5", &pairs(&header("n2")), &[QUIET]),
            peer("10.0.0.9", &pairs(&header("n3")), &[QUIET]),
            unanswered("mac-22.local", "no connection in 3 s"),
        )
    };
    let snap = parse(&at("192.168.1.17"), None, 0).expect("a survey");
    let m17 = &snap.macs[0];
    assert_eq!(
        (m17.dial.as_str(), m17.name.as_str()),
        ("mac-17.local", "mac-17")
    );
    assert_eq!(m17.ident(), "ab12cd34ef56ab12cd34ef56ab12cd34");
    assert_eq!(
        fleet_read_args(&m17.dial, Some("s-w")),
        Some(
            ["ctl", "survey", "--host", "mac-17.local", "--sid", "s-w"]
                .map(str::to_string)
                .to_vec()
        )
    );
    assert_eq!(m17.left_out, 0, "the exited row was sent");
    let lab = &snap.macs[1];
    assert_eq!(
        (lab.dial.as_str(), lab.name.as_str(), lab.host.as_deref()),
        ("lab-mini:7101", "lab-mini", Some("mac.local"))
    );
    assert_eq!(snap.macs[2].name, "10.0.0.9", "an address is shown whole");
    assert_eq!(
        (
            snap.unanswered[0].dial.as_str(),
            snap.unanswered[0].name.as_str()
        ),
        ("mac-22.local", "mac-22")
    );
    let moved = parse(&at("192.168.1.40"), None, 0).expect("a survey");
    assert_eq!(
        herald_key(moved.macs[0].ident(), "s-w"),
        herald_key(m17.ident(), "s-w")
    );
    assert_eq!(fleet_read_args("mac-17.local at 192.168.1.17", None), None);
    assert_eq!(fleet_read_args("-x", None), None);
    assert_eq!(fleet_read_args("mac-17", Some("not a sid")), None);
    assert_ne!(
        herald_key("mac-17.local at 192.168.1.40", "s-w"),
        herald_key("mac-17.local at 192.168.1.17", "s-w")
    );
}

#[test]
fn the_fleet_word_tells_no_fleet_from_not_read_yet() {
    let mut slot = FleetSlot::default();
    assert_eq!(slot.word(), "off");
    slot.book.in_fleet = true;
    assert_eq!(slot.word(), "unread");
    slot.reading = true;
    assert_eq!(slot.word(), "reading");
    slot.reading = false;
    slot.snapshot = Some(Arc::new(FleetSnapshot {
        read_at_ms: 42,
        ..FleetSnapshot::default()
    }));
    assert_eq!(slot.word(), "as_of:42");
}

#[test]
fn should_poll_reads_on_the_minute_or_at_the_menu() {
    let member = BookFacts {
        in_fleet: true,
        minted: false,
        issuer_label: Some("m7".to_string()),
        unbound_issuer: None,
    };
    let issuer = BookFacts {
        in_fleet: true,
        minted: true,
        issuer_label: None,
        unbound_issuer: None,
    };
    let none = BookFacts::default();
    let at = |s: u64| s * 1000;
    type Case<'a> = (&'a BookFacts, bool, Option<u64>, u64, bool, bool);
    let cases: [Case<'_>; 15] = [
        (&none, true, None, at(100), true, false),
        (&member, false, None, at(100), false, false),
        (&member, false, None, at(100), true, true),
        (&member, false, Some(at(90)), at(100), true, false),
        (&member, false, Some(at(85)), at(100), true, true),
        (&member, false, Some(at(0)), at(100), false, false),
        (&member, true, Some(at(0)), at(59), false, false),
        (&member, true, Some(at(0)), at(60), false, true),
        (&issuer, false, None, at(1), false, false),
        (&issuer, false, Some(at(10)), at(50), false, false),
        (&issuer, false, Some(at(10)), at(70), false, false),
        (&issuer, false, Some(at(95)), at(100), true, false),
        (&issuer, false, Some(at(0)), at(100), true, true),
        (&issuer, true, None, at(1), false, true),
        (&issuer, true, Some(at(10)), at(70), false, true),
    ];
    for (n, (book, watch, last, now, menu, want)) in cases.into_iter().enumerate() {
        assert_eq!(
            should_poll(book, watch, last, now, menu, POLL_EVERY),
            want,
            "case {n}"
        );
    }
}

/// A read where every peer the book names was dark.
struct Nobody {
    runs: Sender<()>,
}

impl FleetRunner for Nobody {
    fn read(
        &mut self,
        own_node: Option<&str>,
        now_ms: &dyn Fn() -> u64,
    ) -> Result<FleetSnapshot, String> {
        let _ = self.runs.send(());
        parse(
            r#"[{"fleet":true,"label":"m7.local at 192.168.1.7:7100","error":"did not answer in time"}]"#,
            own_node,
            now_ms(),
        )
    }
}

/// THE ISSUER'S GLANCE BACKS OFF WHILE IT REACHES NOBODY (docs/DESIGN-fluid-swarm-2026-10-28.md
/// F4): after a read that reached no peer the next waits twice as long, up to 30 minutes; a move
/// of this Mac's network reads at once and starts the wait again from the minute; a read that
/// reached a peer starts it again too.
/// NEGATIVE CONTROL: a constant 60 s wait (no `Quiet`) reads again at the minute, failing the
/// first "no read" look; a `Kick::Moved` that ends no backoff fails the move's read.
#[test]
fn a_glance_that_reached_nobody_backs_off_and_a_move_resets_it() {
    use std::sync::atomic::AtomicU64;
    let hang = aterm_spec::wait::MUST_HAPPEN;
    let start = 1_000_000;
    let clock = Arc::new(AtomicU64::new(start));
    let (runs_tx, runs) = std::sync::mpsc::channel();
    let (looks_tx, looks) = std::sync::mpsc::channel();
    let (posts_tx, _posts) = std::sync::mpsc::channel();
    let now = Arc::clone(&clock);
    let glance = spawn(
        Box::new(Nobody { runs: runs_tx }),
        ReaderSeams {
            tick: Duration::from_millis(20),
            now_ms: Box::new(move || {
                let _ = looks_tx.send(());
                now.load(Ordering::SeqCst)
            }),
            ..seams(true, posts_tx)
        },
    )
    .expect("the thread starts");
    // Until the read just made is over and dated `at_ms`: the reader dates a read by the clock
    // after it, so the clock moves only once it has.
    let read_over = |at_ms: u64| {
        while glance.copy().tried_ms != Some(at_ms) {
            looks.recv_timeout(hang).expect("the read ends");
        }
    };
    runs.recv_timeout(hang).expect("the issuer reads at once");
    read_over(start);
    // Whether the reader, looking three times at `at` seconds past the start (its decision is the
    // first or second look, and a read it makes runs before the third), read.
    let read_at = |at: u64| {
        let now = start + at * 1000;
        clock.store(now, Ordering::SeqCst);
        while looks.try_recv().is_ok() {}
        for _ in 0..3 {
            looks.recv_timeout(hang).expect("the reader looks again");
        }
        let read = runs.try_recv().is_ok();
        if read {
            read_over(now);
        }
        read
    };
    assert!(!read_at(60), "reached nobody: not again at the minute");
    assert!(read_at(120), "but at two");
    assert!(!read_at(120 + 239), "then four");
    assert!(read_at(120 + 240));
    let mut at = 120 + 240;
    let mut wait = 480;
    while wait < QUIET_MAX.as_secs() {
        assert!(!read_at(at + wait - 1), "wait {wait}");
        assert!(read_at(at + wait), "wait {wait}");
        at += wait;
        wait *= 2;
    }
    assert!(!read_at(at + QUIET_MAX.as_secs() - 1), "never past 30 min");
    assert!(read_at(at + QUIET_MAX.as_secs()));
    // The move comes a second after that read, so its own read is dated apart from it.
    at += QUIET_MAX.as_secs() + 1;
    clock.store(start + at * 1000, Ordering::SeqCst);
    glance.kick(Kick::Moved);
    runs.recv_timeout(hang).expect("a move reads at once");
    read_over(start + at * 1000);
    assert!(!read_at(at + 119), "the move started the wait again");
    assert!(read_at(at + 120));
}

/// AWAY, A GLANCE THAT REACHED NOBODY WAITS THE LONGEST AT ONCE: off the ALab LAN, after a read
/// that reached no peer the next waits the full 30 minutes (no doubling that dials the members
/// behind the ALab LAN five more times); the move back reads at once.
/// NEGATIVE CONTROL: `Quiet::after` ignoring `away` doubles the wait, reads again at two minutes,
/// and "away: the longest wait at once" fails.
#[test]
fn away_a_glance_that_reached_nobody_waits_the_longest_at_once() {
    use std::sync::atomic::AtomicU64;
    let hang = aterm_spec::wait::MUST_HAPPEN;
    let start = 1_000_000;
    let clock = Arc::new(AtomicU64::new(start));
    let (runs_tx, runs) = std::sync::mpsc::channel();
    let (looks_tx, looks) = std::sync::mpsc::channel();
    let (posts_tx, _posts) = std::sync::mpsc::channel();
    let now = Arc::clone(&clock);
    let glance = spawn(
        Box::new(Nobody { runs: runs_tx }),
        ReaderSeams {
            tick: Duration::from_millis(20),
            now_ms: Box::new(move || {
                let _ = looks_tx.send(());
                now.load(Ordering::SeqCst)
            }),
            away: Box::new(|| true),
            ..seams(true, posts_tx)
        },
    )
    .expect("the thread starts");
    let read_over = |at_ms: u64| {
        while glance.copy().tried_ms != Some(at_ms) {
            looks.recv_timeout(hang).expect("the read ends");
        }
    };
    runs.recv_timeout(hang).expect("the issuer reads at once");
    read_over(start);
    let read_at = |at: u64| {
        let now = start + at * 1000;
        clock.store(now, Ordering::SeqCst);
        while looks.try_recv().is_ok() {}
        for _ in 0..3 {
            looks.recv_timeout(hang).expect("the reader looks again");
        }
        let read = runs.try_recv().is_ok();
        if read {
            read_over(now);
        }
        read
    };
    assert!(!read_at(120), "away: the longest wait at once");
    assert!(!read_at(QUIET_MAX.as_secs() - 1));
    assert!(read_at(QUIET_MAX.as_secs()));
    let at = QUIET_MAX.as_secs() + 1;
    clock.store(start + at * 1000, Ordering::SeqCst);
    glance.kick(Kick::Moved);
    runs.recv_timeout(hang)
        .expect("the move back reads at once");
}

/// THE GLANCE WAITS FOR THE LAN BROKER: a read dials the fleet's Macs through the broker, so
/// while the broker does not answer, the glance reads nothing by itself (its tick, a menu
/// opening); once it answers, it reads. A person's Read the Fleet Now reads all the same.
#[test]
fn the_glance_reads_nothing_by_itself_while_the_lan_broker_does_not_answer() {
    let hang = aterm_spec::wait::MUST_HAPPEN;
    let broker_answers = Arc::new(AtomicBool::new(false));
    let (runs_tx, runs) = std::sync::mpsc::channel();
    let (looks_tx, looks) = std::sync::mpsc::channel();
    let (posts_tx, _posts) = std::sync::mpsc::channel();
    let asked = Arc::clone(&broker_answers);
    let glance = spawn(
        Box::new(Nobody { runs: runs_tx }),
        ReaderSeams {
            tick: Duration::from_millis(20),
            lan: Box::new(move || {
                let _ = looks_tx.send(());
                asked.load(Ordering::SeqCst)
            }),
            ..seams(true, posts_tx)
        },
    )
    .expect("the thread starts");
    let looks_three_times = || {
        while looks.try_recv().is_ok() {}
        for _ in 0..3 {
            looks
                .recv_timeout(hang)
                .expect("the reader looks for the broker");
        }
    };
    looks_three_times();
    assert!(
        runs.try_recv().is_err(),
        "it read by itself before the broker answered"
    );
    glance.kick(Kick::MenuOpening);
    looks_three_times();
    assert!(
        runs.try_recv().is_err(),
        "a menu opening read before the broker answered"
    );
    broker_answers.store(true, Ordering::SeqCst);
    runs.recv_timeout(hang)
        .expect("it reads once the broker answers");

    let (runs_tx, runs) = std::sync::mpsc::channel();
    let (posts_tx, _posts) = std::sync::mpsc::channel();
    let held = spawn(
        Box::new(Nobody { runs: runs_tx }),
        ReaderSeams {
            lan: Box::new(|| false),
            ..seams(true, posts_tx)
        },
    )
    .expect("the thread starts");
    held.kick(Kick::Now);
    runs.recv_timeout(hang)
        .expect("a person's Read the Fleet Now reads while the broker does not answer");
}

struct Gated {
    runs: Arc<AtomicUsize>,
    gate: Receiver<()>,
    started: Sender<()>,
}

impl FleetRunner for Gated {
    fn read(
        &mut self,
        own_node: Option<&str>,
        now_ms: &dyn Fn() -> u64,
    ) -> Result<FleetSnapshot, String> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        let _ = self.started.send(());
        let _ = self.gate.recv();
        parse("[]", own_node, now_ms())
    }
}

fn seams(minted: bool, posts: Sender<()>) -> ReaderSeams {
    ReaderSeams {
        book: Box::new(move || BookFacts {
            in_fleet: true,
            minted,
            issuer_label: None,
            unbound_issuer: None,
        }),
        watch: Arc::new(AtomicBool::new(minted)),
        own_node: None,
        now_ms: Box::new(|| aterm_time::unix_millis().unwrap_or(0)),
        post: Box::new(move || posts.send(()).is_ok()),
        tick: Duration::from_secs(3600),
        moved: None,
        lan: Box::new(|| true),
        away: Box::new(|| false),
    }
}

#[test]
fn the_reader_is_single_flight() {
    let hang = aterm_spec::wait::MUST_HAPPEN;
    let runs = Arc::new(AtomicUsize::new(0));
    let (open, gate) = std::sync::mpsc::channel();
    let (started_tx, started) = std::sync::mpsc::channel();
    let (posts_tx, posts) = std::sync::mpsc::channel();
    let glance = spawn(
        Box::new(Gated {
            runs: Arc::clone(&runs),
            gate,
            started: started_tx,
        }),
        seams(true, posts_tx),
    )
    .expect("the thread starts");
    started
        .recv_timeout(hang)
        .expect("the issuer reads at once");
    assert!(glance.copy().reading);
    for _ in 0..3 {
        glance.kick(Kick::MenuOpening);
        glance.kick(Kick::Now);
    }
    open.send(()).unwrap();
    posts.recv_timeout(hang).expect("began");
    posts.recv_timeout(hang).expect("ended");
    assert!(!glance.copy().reading);
    assert_eq!(glance.copy().generation, 1);
    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "the kicks were answered by it"
    );
    glance.kick(Kick::Now);
    started.recv_timeout(hang).expect("Read Now reads");
    open.send(()).unwrap();
    posts.recv_timeout(hang).expect("began");
    posts.recv_timeout(hang).expect("ended");
    assert_eq!(runs.load(Ordering::SeqCst), 2);
    assert_eq!(glance.copy().generation, 2);
    assert!(
        glance.copy().snapshot_at(1).is_some(),
        "the read before is kept"
    );
    assert!(glance.copy().snapshot_at(0).is_none());
}

/// A KICK THAT COMES ONCE A READ IS OVER IS ANSWERED: the kicks that come while a read runs are
/// answered by it (but a move of the network,
/// [`a_move_while_a_read_runs_reads_again_once_it_is_over`]), and one sent once its result is in
/// the slot (a person's Read the Fleet Now, a move of the network) asks for a read of its own.
/// NEGATIVE CONTROL: dropping the kicks queued once the slot is written loses that one.
#[test]
fn a_kick_once_a_read_is_in_the_slot_reads_again() {
    let hang = aterm_spec::wait::MUST_HAPPEN;
    let runs = Arc::new(AtomicUsize::new(0));
    let (open, gate) = std::sync::mpsc::channel();
    let (started_tx, started) = std::sync::mpsc::channel();
    let (posts_tx, _posts) = std::sync::mpsc::channel();
    let (looked_tx, looked) = std::sync::mpsc::channel();
    let (go_on_tx, go_on) = std::sync::mpsc::channel::<()>();
    let looks = AtomicUsize::new(0);
    let glance = spawn(
        Box::new(Gated {
            runs: Arc::clone(&runs),
            gate,
            started: started_tx,
        }),
        ReaderSeams {
            // The book's look once the first read is in the slot holds the reader there until
            // the test has kicked.
            book: Box::new(move || {
                if looks.fetch_add(1, Ordering::SeqCst) == 1 {
                    let _ = looked_tx.send(());
                    let _ = go_on.recv_timeout(hang);
                }
                BookFacts {
                    in_fleet: true,
                    minted: true,
                    issuer_label: None,
                    unbound_issuer: None,
                }
            }),
            ..seams(true, posts_tx)
        },
    )
    .expect("the thread starts");
    started
        .recv_timeout(hang)
        .expect("the issuer reads at once");
    open.send(()).unwrap();
    looked.recv_timeout(hang).expect("the read is over");
    assert!(
        glance.copy().tried_ms.is_some(),
        "its result is in the slot"
    );
    glance.kick(Kick::Now);
    go_on_tx.send(()).unwrap();
    started.recv_timeout(hang).expect("the kick reads again");
    open.send(()).unwrap();
    assert_eq!(runs.load(Ordering::SeqCst), 2);
}

/// A MOVE WHILE A READ RUNS IS NOT ANSWERED BY IT: that read went out on the old network, so once
/// it is over the reader reads again, as a move does at any other time, though the clock says the
/// last read was just now.
/// NEGATIVE CONTROL: dropping a move with the kicks a read answered reads nothing more.
#[test]
fn a_move_while_a_read_runs_reads_again_once_it_is_over() {
    let hang = aterm_spec::wait::MUST_HAPPEN;
    let runs = Arc::new(AtomicUsize::new(0));
    let (open, gate) = std::sync::mpsc::channel();
    let (started_tx, started) = std::sync::mpsc::channel();
    let (posts_tx, _posts) = std::sync::mpsc::channel();
    let glance = spawn(
        Box::new(Gated {
            runs: Arc::clone(&runs),
            gate,
            started: started_tx,
        }),
        ReaderSeams {
            now_ms: Box::new(|| 1_000_000),
            ..seams(true, posts_tx)
        },
    )
    .expect("the thread starts");
    started
        .recv_timeout(hang)
        .expect("the issuer reads at once");
    glance.kick(Kick::Moved);
    glance.kick(Kick::Now);
    open.send(()).unwrap();
    started
        .recv_timeout(hang)
        .expect("the move reads again on the new network");
    open.send(()).unwrap();
    assert_eq!(runs.load(Ordering::SeqCst), 2);
}

struct Empty;

impl FleetRunner for Empty {
    fn read(
        &mut self,
        own_node: Option<&str>,
        now_ms: &dyn Fn() -> u64,
    ) -> Result<FleetSnapshot, String> {
        parse("[]", own_node, now_ms())
    }
}

#[test]
fn the_app_is_told_when_the_last_read_stops_counting() {
    use std::sync::atomic::AtomicU64;
    let hang = aterm_spec::wait::MUST_HAPPEN;
    let clock = Arc::new(AtomicU64::new(1_000_000));
    let (posts_tx, posts) = std::sync::mpsc::channel();
    let (looks_tx, looks) = std::sync::mpsc::channel();
    let now = Arc::clone(&clock);
    let glance = spawn(
        Box::new(Empty),
        ReaderSeams {
            tick: Duration::from_millis(20),
            now_ms: Box::new(move || {
                let _ = looks_tx.send(());
                now.load(Ordering::SeqCst)
            }),
            ..seams(false, posts_tx)
        },
    )
    .expect("the thread starts");
    posts.recv_timeout(hang).expect("the first look is told");
    assert!(
        glance.copy().snapshot.is_none(),
        "a member reads when asked"
    );
    glance.kick(Kick::Now);
    posts.recv_timeout(hang).expect("began");
    posts.recv_timeout(hang).expect("ended");
    let read_at = glance.copy().snapshot.expect("read").read_at_ms;
    while looks.try_recv().is_ok() {}
    for _ in 0..3 {
        looks.recv_timeout(hang).expect("the reader looks again");
    }
    assert!(posts.try_recv().is_err(), "a fresh read owes no post");
    clock.store(read_at - 1_000, Ordering::SeqCst);
    posts
        .recv_timeout(hang)
        .expect("the clock rollback is told");
    let rewound = glance.copy();
    assert!(!rewound.snapshot.expect("kept").fresh(read_at - 1_000));
    assert_eq!(rewound.generation, 1, "the rollback makes no new read");
    assert!(!rewound.reading, "disabled polling stays disabled");
    clock.store(read_at, Ordering::SeqCst);
    posts
        .recv_timeout(hang)
        .expect("the clock catch-up is told");
    let caught_up = glance.copy();
    assert!(caught_up.snapshot.expect("kept").fresh(read_at));
    assert_eq!(caught_up.generation, 1, "catch-up makes no new read");
    assert!(!caught_up.reading, "polling remains disabled");
    clock.store(read_at + millis(COUNT_FRESH) + 1_000, Ordering::SeqCst);
    posts.recv_timeout(hang).expect("the lapse is told");
    let slot = glance.copy();
    assert!(
        !slot
            .snapshot
            .expect("kept")
            .fresh(clock.load(Ordering::SeqCst))
    );
    assert!(!slot.reading, "the lapse is no read");
}

#[test]
fn a_lapse_that_passes_while_the_read_is_told_is_told() {
    use std::sync::atomic::AtomicU64;
    let hang = aterm_spec::wait::MUST_HAPPEN;
    let start = 1_000_000;
    let lapsed = start + millis(COUNT_FRESH) + 1_000;
    let clock = Arc::new(AtomicU64::new(start));
    let (posts_tx, posts) = std::sync::mpsc::channel();
    let (now, moved) = (Arc::clone(&clock), Arc::clone(&clock));
    let told = AtomicUsize::new(0);
    let glance = spawn(
        Box::new(Empty),
        ReaderSeams {
            tick: Duration::from_secs(3600),
            now_ms: Box::new(move || now.load(Ordering::SeqCst)),
            post: Box::new(move || {
                if told.fetch_add(1, Ordering::SeqCst) == 2 {
                    moved.store(lapsed, Ordering::SeqCst);
                }
                posts_tx.send(()).is_ok()
            }),
            ..seams(false, std::sync::mpsc::channel().0)
        },
    )
    .expect("the thread starts");
    posts.recv_timeout(hang).expect("the first look is told");
    glance.kick(Kick::Now);
    posts.recv_timeout(hang).expect("began");
    posts.recv_timeout(hang).expect("ended");
    let read = glance.copy().snapshot.expect("read");
    assert!(read.fresh(start), "CONTROL: fresh when its end was told");
    posts.recv_timeout(hang).expect("the lapse is told");
    let slot = glance.copy();
    assert!(!slot.snapshot.expect("kept").fresh(lapsed));
    assert_eq!(slot.generation, 1, "CONTROL: the lapse is no read");
    assert!(!slot.reading);
}

/// A CHILD THAT NEVER ANSWERS IS KILLED AT THE CAP and the read fails: the
/// slot keeps no snapshot, says why, and the App is told.
#[cfg(unix)]
#[test]
fn a_child_that_never_answers_is_killed_and_posts_its_failure() {
    let hang = aterm_spec::wait::MUST_HAPPEN;
    let (posts_tx, posts) = std::sync::mpsc::channel();
    let glance = spawn(
        Box::new(ChildRunner {
            program: PathBuf::from("/bin/sleep"),
            args: vec!["600".to_string()],
            cap: Duration::from_millis(200),
        }),
        seams(true, posts_tx),
    )
    .expect("the thread starts");
    posts.recv_timeout(hang).expect("began");
    posts.recv_timeout(hang).expect("ended");
    let slot = glance.copy();
    assert!(slot.snapshot.is_none());
    assert!(slot.error.is_some(), "{slot:?}");
    assert_eq!(slot.word(), "failed");
    let (posts_tx, posts) = std::sync::mpsc::channel();
    let glance = spawn(
        Box::new(ChildRunner {
            program: PathBuf::from("/bin/echo"),
            args: vec!["[]".to_string()],
            cap: Duration::from_secs(60),
        }),
        seams(true, posts_tx),
    )
    .expect("the thread starts");
    posts.recv_timeout(hang).expect("began");
    posts.recv_timeout(hang).expect("ended");
    assert!(glance.copy().snapshot.is_some());
    assert!(glance.copy().word().starts_with("as_of:"));
}

fn inert_seams() -> ReaderSeams {
    ReaderSeams {
        book: Box::new(BookFacts::default),
        watch: Arc::new(AtomicBool::new(false)),
        own_node: None,
        now_ms: Box::new(|| aterm_time::unix_millis().unwrap_or(0)),
        post: Box::new(|| true),
        tick: Duration::from_secs(3600),
        moved: None,
        lan: Box::new(|| true),
        away: Box::new(|| false),
    }
}

#[test]
fn a_headless_app_starts_no_fleet_glance() {
    let mut app = crate::App::headless_for_test();
    app.start_fleet_glance_with(Box::new(Empty), inert_seams());
    app.start_fleet_glance();
    assert!(app.fleet_glance.is_none());
    assert!(app.status_item_chrome_line().ends_with(" fleet=off"));
    let mut windowed = crate::App::headless_for_test();
    windowed.headless = false;
    windowed.start_fleet_glance_with(Box::new(Empty), inert_seams());
    assert_eq!(windowed.fleet_glance.is_some(), cfg!(target_os = "macos"));
    if let Some(first) = windowed.fleet_glance.as_ref().map(|f| Arc::clone(&f.slot)) {
        windowed.start_fleet_glance_with(Box::new(Empty), inert_seams());
        let again = windowed.fleet_glance.as_ref().map(|f| Arc::clone(&f.slot));
        assert!(
            again.is_some_and(|slot| Arc::ptr_eq(&slot, &first)),
            "never twice"
        );
    }
}

fn glance_over(slot: FleetSlot) -> FleetGlance {
    let (kick, _) = std::sync::mpsc::channel();
    FleetGlance {
        slot: Arc::new(Mutex::new(slot)),
        kick,
        watch: Arc::new(AtomicBool::new(false)),
    }
}

fn read_slot(read_at_ms: u64, dark: bool) -> FleetSlot {
    let need = FleetNeed {
        sid: "s-w".to_string(),
        word: NeedWord::Stalled,
        who: "claude".to_string(),
        worker_of: None,
        age_ms: Some(1_000),
        what: None,
        claim: None,
        carries_on: false,
        ids: Vec::new(),
    };
    FleetSlot {
        generation: 1,
        snapshot: Some(Arc::new(FleetSnapshot {
            read_at_ms,
            macs: (!dark)
                .then(|| FleetMac {
                    name: "mac-17".to_string(),
                    dial: "mac-17".to_string(),
                    needs: vec![need],
                    read_at_ms,
                    ..FleetMac::default()
                })
                .into_iter()
                .collect(),
            unanswered: Vec::new(),
            seats: None,
        })),
        book: BookFacts {
            in_fleet: true,
            minted: true,
            issuer_label: None,
            unbound_issuer: None,
        },
        ..FleetSlot::default()
    }
}

/// A fleet read heralds each need of a Mac that counts once per transition, and a read that
/// reached no Mac, or one stale or from the future, heralds nothing.
#[test]
fn a_fleet_read_heralds_its_needs() {
    let take_posts =
        || crate::app_tabs::HERALD_POSTS.with(|p| std::mem::take(&mut *p.borrow_mut()));
    let settle = |app: &mut crate::App| {
        if let Some(due) = app.messages_deadline() {
            let _ = app.settle_messages(due + Duration::from_secs(1));
        }
    };
    let now = aterm_time::unix_millis().unwrap_or(0);
    let mut app = crate::App::headless_for_test();
    app.headless = false;
    app.config.desktop_alerts = Some(true);
    crate::app_tabs::HERALD_REFUSAL.set(None);
    let _ = take_posts();

    app.fleet_glance = Some(glance_over(read_slot(now, true)));
    app.on_fleet_read();
    assert!(
        take_posts().is_empty(),
        "a read that reached no Mac is no need"
    );

    app.fleet_glance = Some(glance_over(read_slot(now, false)));
    app.on_fleet_read();
    settle(&mut app);
    let posts = take_posts();
    assert_eq!(posts.len(), 1, "one notice for the fleet need");
    assert!(is_herald_key(posts[0].session));
    assert_eq!(
        posts[0].title,
        crate::status_item::headline(NeedWord::Stalled)
    );
    app.on_fleet_read();
    assert!(take_posts().is_empty(), "the same need re-read");
    assert_eq!(app.status_glance().need_count(), 1);

    let mut stale = crate::App::headless_for_test();
    stale.headless = false;
    stale.config.desktop_alerts = Some(true);
    let old = now.saturating_sub(millis(COUNT_FRESH) + 60_000);
    stale.fleet_glance = Some(glance_over(read_slot(old, false)));
    stale.on_fleet_read();
    assert!(take_posts().is_empty(), "a stale read heralds nothing");
    let glance = stale.status_glance();
    assert_eq!((glance.needs.len(), glance.need_count()), (1, 0));

    let future = now + 86_400_000;
    stale.fleet_glance = Some(glance_over(read_slot(future, false)));
    stale.on_fleet_read();
    assert!(take_posts().is_empty(), "a future read heralds nothing");
    let glance = stale.status_glance();
    assert_eq!((glance.needs.len(), glance.need_count()), (1, 0));
}

/// A SEAMLESS SUCCESSOR TELLS NO STANDING FLEET NEED AGAIN: the outgoing
/// herald's told fleet needs, as its park stamps them
/// (`App::fleet_told_rows`) and the successor reads them back
/// (`parse_told`), are taken as told at the successor's first fresh read
/// where it finds the same word on the same Mac's session — whenever that
/// read comes, as one process reading on would — and still count; a need
/// that arises after posts.
#[cfg(unix)]
#[test]
fn a_successors_first_fleet_read_tells_no_standing_need_again() {
    let take_posts =
        || crate::app_tabs::HERALD_POSTS.with(|p| std::mem::take(&mut *p.borrow_mut()));
    let now = aterm_time::unix_millis().unwrap_or(0);
    let key = herald_key("mac-17", "s-w");
    let first_read = |adopt: Option<std::collections::HashMap<u64, NeedWord>>,
                      refusal: Option<&'static str>| {
        let mut app = crate::App::headless_for_test();
        app.headless = false;
        app.config.desktop_alerts = Some(true);
        crate::app_tabs::HERALD_REFUSAL.set(refusal);
        app.fleet_adopt = adopt;
        app.fleet_glance = Some(glance_over(read_slot(now, false)));
        let _ = take_posts();
        app.on_fleet_read();
        crate::app_tabs::HERALD_REFUSAL.set(None);
        let posted = take_posts().len();
        (app, posted)
    };
    let (parent, told) = first_read(None, None);
    assert_eq!(told, 1, "no update: told");
    let rows = parent.fleet_told_rows();
    assert_eq!(rows, vec![told_row(key, NeedWord::Stalled)]);
    let carried = parse_told(&rows);
    assert_eq!(carried.get(&key), Some(&NeedWord::Stalled));

    let (mut app, posted) = first_read(Some(carried.clone()), None);
    assert_eq!(posted, 0, "told before the update");
    assert_eq!(app.status_glance().need_count(), 1, "still counted");
    assert!(app.fleet_adopt.is_none(), "spent by the first read");
    let mut slot = read_slot(now, false);
    let mut snapshot = (*slot.snapshot.take().expect("a read")).clone();
    let arose = FleetNeed {
        sid: "s-x".to_string(),
        ..snapshot.macs[0].needs[0].clone()
    };
    snapshot.macs[0].needs.push(arose);
    slot.snapshot = Some(Arc::new(snapshot));
    slot.generation = 2;
    app.fleet_glance = Some(glance_over(slot));
    app.on_fleet_read();
    assert_eq!(take_posts().len(), 1, "the need that arose since");

    let (_, unnamed) = first_read(Some(std::collections::HashMap::new()), None);
    assert_eq!(unnamed, 1, "arose during the update: told");
    let other = std::collections::HashMap::from([(key, NeedWord::Ask)]);
    assert_eq!(first_read(Some(other), None).1, 1, "another word: told");
    let (refused, _) = first_read(None, Some("the notifier's queue is full"));
    assert!(refused.fleet_told_rows().is_empty(), "owed is not told");
}

#[test]
fn told_fleet_rows_read_back_and_drop_what_does_not_read() {
    let key = herald_key("mac-17", "s-w");
    let rows = vec![
        told_row(key, NeedWord::Ask),
        format!("{:016x}=ask", 7_u64),
        format!("{:016x}=someday", herald_key("mac-1", "s-1")),
        "no row".to_string(),
    ];
    let told = parse_told(&rows);
    assert_eq!(told.len(), 1);
    assert_eq!(told.get(&key), Some(&NeedWord::Ask));
}

#[test]
fn host_words_and_herald_keys() {
    for ok in ["mac-17", "mac-17.local", "10.0.0.5", "a_b"] {
        assert_eq!(host_word(ok).as_deref(), Some(ok));
    }
    assert_eq!(short_name("[::1]:7100"), "::1");
    assert_eq!(short_name("[::ffff:127.0.0.1]:7100"), "::ffff:127.0.0.1");
    for bad in ["", "-x", "a b", "a;b", "a/b", "x@y", "\u{e9}"] {
        assert_eq!(host_word(bad), None, "{bad:?}");
    }
    let key = herald_key("mac-17", "s-1");
    assert!(is_herald_key(key));
    assert_ne!(key, herald_key("mac-17", "s-2"));
    assert!(!is_herald_key(0) && !is_herald_key(u64::from(u32::MAX)));
}

/// A FLEET ACT GOES THROUGH THE PLATFORM SEAMS, whose test arms open and
/// copy nothing of the system's: Screen Sharing names `vnc://<host>`, Copy
/// "ssh" puts the line on the (test) pasteboard.
#[cfg(target_os = "macos")]
#[test]
fn a_fleet_act_goes_through_the_seams_and_no_host_gets_nothing() {
    use crate::status_item::{Pick, StatusAction};
    let mut app = crate::App::headless_for_test();
    let _ = crate::platform::take_opened_urls();
    let p = Pick {
        generation: 0,
        index: 0,
    };
    app.fleet_act(
        StatusAction::PeerScreen(p),
        "mac-17.local",
        "mac-17",
        Some("mac-17.local"),
        Some("s-1"),
    );
    assert_eq!(
        crate::platform::take_opened_urls(),
        vec!["vnc://mac-17.local"]
    );
    app.fleet_act(
        StatusAction::MacSsh(p),
        "mac-17.local",
        "mac-17",
        Some("mac-17.local"),
        None,
    );
    assert_eq!(
        crate::control::pbpaste().as_deref(),
        Some("ssh mac-17.local")
    );
    app.fleet_act(
        StatusAction::MacScreen(p),
        "x",
        "x",
        Some("a;open -a Terminal"),
        None,
    );
    app.fleet_act(StatusAction::PeerSsh(p), "x", "x", Some("a b"), None);
    assert!(crate::platform::take_opened_urls().is_empty());
    assert_eq!(
        crate::control::pbpaste().as_deref(),
        Some("ssh mac-17.local")
    );
}

fn mac17(read_at_ms: u64, need: bool) -> FleetMac {
    FleetMac {
        name: "mac-17".to_string(),
        dial: "mac-17.local".to_string(),
        node: Some("n17".to_string()),
        needs: need
            .then(|| FleetNeed {
                sid: "s-w".to_string(),
                word: NeedWord::Stalled,
                who: "claude".to_string(),
                worker_of: None,
                age_ms: Some(1_000),
                what: None,
                claim: None,
                carries_on: false,
                ids: Vec::new(),
            })
            .into_iter()
            .collect(),
        read_at_ms,
        ..FleetMac::default()
    }
}

fn silent(dial: &str) -> Unanswered {
    Unanswered {
        name: short_name(dial),
        dial: dial.to_string(),
        why: "no connection in 3 s".to_string(),
        ..Unanswered::default()
    }
}

fn slot_of(read_at_ms: u64, macs: Vec<FleetMac>, unanswered: Vec<Unanswered>) -> FleetSlot {
    FleetSlot {
        generation: 1,
        snapshot: Some(Arc::new(FleetSnapshot {
            read_at_ms,
            macs,
            unanswered,
            seats: None,
        })),
        book: BookFacts {
            in_fleet: true,
            minted: true,
            issuer_label: None,
            unbound_issuer: None,
        },
        ..FleetSlot::default()
    }
}

#[test]
fn a_read_carries_a_silent_mac_while_its_rows_count() {
    let t0 = 1_000_000;
    let fresh = millis(COUNT_FRESH);
    let mut busy = mac17(0, false);
    busy.dial = "mac-busy".to_string();
    let next = |at: u64| FleetSnapshot {
        read_at_ms: at,
        macs: vec![FleetMac {
            read_at_ms: at,
            ..busy.clone()
        }],
        unanswered: vec![silent("mac-17.local")],
        seats: None,
    };
    let first = FleetSnapshot {
        read_at_ms: t0,
        macs: vec![mac17(t0, true)],
        ..FleetSnapshot::default()
    };
    let carried = carry(Some(&first), next(t0 + 60_000), t0 + 60_000);
    assert_eq!(carried.macs.len(), 2);
    let m17 = &carried.macs[1];
    assert!(m17.carried && m17.read_at_ms == t0 && m17.needs.len() == 1);
    assert_eq!((carried.answered(), carried.unanswered.len()), (1, 1));
    assert_eq!(
        carried.next_freshness_change(t0 + 60_000),
        Some(t0 + fresh + 1)
    );
    let again = carry(Some(&carried), next(t0 + 120_000), t0 + 120_000);
    assert_eq!(again.macs.len(), 2, "carried through a second miss");
    assert_eq!(again.macs[1].read_at_ms, t0);
    let lapsed = carry(Some(&again), next(t0 + fresh + 1), t0 + fresh + 1);
    assert_eq!(lapsed.macs.len(), 1, "past its own read's freshness");
    assert_eq!(carry(None, next(t0), t0).macs.len(), 1);
}

#[test]
fn future_fleet_reads_are_uncounted_and_repolled_after_a_clock_rollback() {
    let now = 1_000_000;
    let at = now + 60_000;
    let previous = FleetSnapshot {
        read_at_ms: at,
        macs: vec![mac17(at, true)],
        ..FleetSnapshot::default()
    };
    assert!(!previous.fresh(now), "a future read is not fresh");
    assert!(!previous.macs[0].fresh(now), "neither are its peer rows");
    assert_eq!(previous.next_freshness_change(now), Some(at));
    assert_eq!(
        previous.next_freshness_change(at),
        Some(at + millis(COUNT_FRESH) + 1)
    );
    let next = FleetSnapshot {
        read_at_ms: now,
        unanswered: vec![silent("mac-17.local")],
        ..FleetSnapshot::default()
    };
    assert!(carry(Some(&previous), next, now).macs.is_empty());

    let slot = slot_of(at, previous.macs.clone(), Vec::new());
    assert!(should_poll(
        &slot.book,
        true,
        Some(at),
        now,
        false,
        POLL_EVERY
    ));
    assert!(should_poll(
        &slot.book,
        false,
        Some(at),
        now,
        true,
        POLL_EVERY
    ));
    assert!(!should_poll(
        &slot.book,
        false,
        Some(at),
        now,
        false,
        POLL_EVERY
    ));

    let fresh = millis(COUNT_FRESH);
    assert!(previous.fresh(at));
    assert!(previous.fresh(at + fresh));
    assert!(!previous.fresh(at + fresh + 1));
}

fn take_posts() -> Vec<crate::status_item::HeraldNotice> {
    crate::app_tabs::HERALD_POSTS.with(|p| std::mem::take(&mut *p.borrow_mut()))
}

fn windowed_app() -> crate::App {
    let mut app = crate::App::headless_for_test();
    app.headless = false;
    app.config.desktop_alerts = Some(true);
    crate::app_tabs::HERALD_REFUSAL.set(None);
    let _ = take_posts();
    app
}

#[test]
fn a_standing_fleet_need_posts_once_across_a_stale_read_and_a_silent_mac() {
    let now = aterm_time::unix_millis().unwrap_or(0);
    let old = now.saturating_sub(millis(COUNT_FRESH) + 60_000);
    let mut app = windowed_app();
    let key = herald_key("n17", "s-w");
    let read = |app: &mut crate::App, slot| {
        app.fleet_glance = Some(glance_over(slot));
        app.on_fleet_read();
        take_posts().len()
    };
    assert_eq!(
        read(&mut app, slot_of(now, vec![mac17(now, true)], vec![])),
        1
    );
    assert_eq!(
        read(&mut app, slot_of(old, vec![mac17(old, true)], vec![])),
        0
    );
    assert_eq!(
        app.status_glance().need_count(),
        0,
        "a stale read counts nothing"
    );
    assert_eq!(
        read(&mut app, slot_of(now, vec![], vec![silent("mac-17.local")])),
        0
    );
    assert!(
        app.fleet_heralded.contains_key(&key),
        "a silent Mac's need is kept"
    );
    assert_eq!(
        read(&mut app, slot_of(now, vec![mac17(now, true)], vec![])),
        0
    );
    assert_eq!(app.status_glance().need_count(), 1);
    assert_eq!(
        read(&mut app, slot_of(now, vec![mac17(now, false)], vec![])),
        0
    );
    assert!(
        !app.fleet_heralded.contains_key(&key),
        "answered without it"
    );
    assert_eq!(
        read(&mut app, slot_of(now, vec![mac17(now, true)], vec![])),
        1
    );
}

#[test]
fn an_unproven_macs_need_is_never_told() {
    let now = aterm_time::unix_millis().unwrap_or(0);
    for (marked, posts) in [(true, 0), (false, 1)] {
        let mut app = windowed_app();
        let studio = FleetMac {
            unproven: marked,
            ..mac17(now, true)
        };
        app.fleet_glance = Some(glance_over(slot_of(now, vec![studio], vec![])));
        app.on_fleet_read();
        assert_eq!(take_posts().len(), posts, "unproven={marked}");
        assert_eq!(app.fleet_heralded.len(), posts);
        assert_eq!(app.status_glance().need_count(), posts);
    }
}

#[test]
fn a_fleet_need_that_cannot_be_told_owes_nothing() {
    let now = aterm_time::unix_millis().unwrap_or(0);
    let old = now.saturating_sub(millis(COUNT_FRESH) + 60_000);
    let later = std::time::Instant::now() + Duration::from_secs(120);
    let key = herald_key("n17", "s-w");
    let owed = |app: &crate::App| app.presence.herald.due(later).0.contains(&key);
    let mut app = windowed_app();
    let read = |app: &mut crate::App, slot| {
        app.fleet_glance = Some(glance_over(slot));
        app.on_fleet_read();
    };
    crate::app_tabs::HERALD_REFUSAL.set(Some("the notifier's queue is full"));
    read(&mut app, slot_of(now, vec![mac17(now, true)], vec![]));
    assert!(owed(&app), "the control: a refused notice is owed");
    read(&mut app, slot_of(now, vec![mac17(now, true)], vec![]));
    assert!(owed(&app), "the control: still owed while it stands");
    read(&mut app, slot_of(old, vec![mac17(old, true)], vec![]));
    assert!(!owed(&app), "a stale read owes nothing");
    let mut asks = mac17(now, true);
    asks.needs[0].word = NeedWord::Ask;
    crate::app_tabs::HERALD_REFUSAL.set(Some("the notifier's queue is full"));
    read(&mut app, slot_of(now, vec![asks], vec![]));
    assert!(owed(&app));
    read(&mut app, slot_of(now, vec![], vec![silent("mac-17.local")]));
    assert!(!owed(&app), "a silent Mac's need owes nothing");
    assert!(app.fleet_heralded.contains_key(&key), "and is kept");
    let _ = take_posts();
}

#[test]
fn a_hold_this_mac_placed_is_read_back_whatever_its_need() {
    let mine = "0123456789abcdef0123456789abcdef";
    let theirs = "fedcba9876543210fedcba9876543210";
    let held = |sid: &'static str, node: &'static str| {
        vec![
            ("sid", sid),
            ("state", "alive"),
            ("program", "claude"),
            ("program_kind", "agent"),
            ("agent", "idle"),
            ("role", "review"),
            ("needs", "-"),
            ("hold_by", "remote"),
            ("held_from", node),
        ]
    };
    let text = format!(
        "[{}]",
        peer(
            "lab-mini.local",
            &pairs(&header("n4")),
            &[&held("s-a", mine), &held("s-b", theirs), QUIET]
        )
    );
    let snap = parse(&text, Some(mine), 0).expect("a survey");
    assert_eq!(
        snap.macs[0].held_here,
        [FleetHeld {
            sid: "s-a".to_string(),
            who: "review".to_string(),
        }]
    );
    assert!(snap.macs[0].needs.is_empty(), "a hold is no need");
    let blind = parse(&text, None, 0).expect("a survey");
    assert!(blind.macs[0].held_here.is_empty(), "{:?}", blind.macs[0]);
}

#[test]
fn a_peer_that_says_drive_is_held_and_handed_back_over_the_fleet() {
    let mut head = header("n4");
    head.push(("drive", "1".to_string()));
    let text = format!(
        "[{},{}]",
        peer("lab-mini.local", &pairs(&head), &[QUIET]),
        peer("mac-17.local", &pairs(&header("n5")), &[QUIET])
    );
    let snap = parse(&text, None, 0).expect("a survey");
    assert_eq!(
        snap.macs.iter().map(|m| m.drive).collect::<Vec<_>>(),
        [true, false]
    );
    assert_eq!(
        fleet_hold_args(&snap.macs[0].dial, "s-q", true),
        Some(
            ["ctl", "--host", "lab-mini.local", "hold", "s-q", "on"]
                .map(str::to_string)
                .to_vec()
        )
    );
    assert_eq!(
        fleet_hold_args("lab-mini.local", "s-q", false).map(|args| args[5].clone()),
        Some("off".to_string())
    );
    for (dial, sid) in [
        ("-x", "s-q"),
        ("lab mini", "s-q"),
        ("", "s-q"),
        ("lab-mini", "q"),
        ("lab-mini", "s-q x"),
    ] {
        assert_eq!(fleet_hold_args(dial, sid, true), None, "{dial:?} {sid:?}");
    }
}

/// The glance's `lan` seam is `network_moved::lan_answers`, which on macOS asks the root LAN
/// broker for its hello, a round trip on its socket. So the glance asks it only for a read it
/// then makes; a tick that reads nothing (its quiet pause) does not.
/// NEGATIVE CONTROL: ask `(seams.lan)()` before `should_poll` in `reader_loop`: every tick asks.
#[test]
fn the_glance_asks_the_lan_door_only_for_a_read_it_makes() {
    let hang = aterm_spec::wait::MUST_HAPPEN;
    let (runs_tx, runs) = std::sync::mpsc::channel();
    let looks = Arc::new(AtomicUsize::new(0));
    let (posts_tx, _posts) = std::sync::mpsc::channel();
    let (ticks_tx, ticks) = std::sync::mpsc::channel();
    let counted = Arc::clone(&looks);
    let _glance = spawn(
        Box::new(Nobody { runs: runs_tx }),
        ReaderSeams {
            tick: Duration::from_millis(20),
            lan: Box::new(move || {
                counted.fetch_add(1, Ordering::SeqCst);
                true
            }),
            book: Box::new(move || {
                let _ = ticks_tx.send(());
                BookFacts {
                    in_fleet: true,
                    minted: true,
                    issuer_label: None,
                    unbound_issuer: None,
                }
            }),
            ..seams(true, posts_tx)
        },
    )
    .expect("the thread starts");
    runs.recv_timeout(hang).expect("the first read");
    while ticks.try_recv().is_ok() {}
    // 25 ticks within the read's quiet pause: none reads.
    for _ in 0..25 {
        ticks.recv_timeout(hang).expect("the reader ticks again");
    }
    let made = 1 + runs.try_iter().count();
    let asked = looks.load(Ordering::SeqCst);
    assert!(
        asked <= made,
        "the glance went through the LAN door {asked} times for {made} reads"
    );
}

/// Every text a menu's rows show, submenus' rows after their label.
#[cfg(unix)]
fn menu_texts(rows: &[crate::status_item::StatusRow]) -> Vec<String> {
    use crate::status_item::StatusRow;
    let mut texts = Vec::new();
    for row in rows {
        match row {
            StatusRow::Info(text) => texts.push(text.clone()),
            StatusRow::Action { label, .. } | StatusRow::FleetSubmenu { label, .. } => {
                texts.push(label.clone());
            }
            StatusRow::Submenu { label, rows } => {
                texts.push(label.clone());
                texts.extend(menu_texts(rows));
            }
            _ => {}
        }
    }
    texts
}

/// THE GLANCE READS THIS MAC'S COPY AND STARTS NOTHING (one-mesh §4.12, §4.14, §11 B9): with the
/// mesh on, the glance asks the node service's view socket for each seat's standing, presence
/// row and now/needs row. An up seat's row names a stalled worker of a manager and a session this
/// Mac holds, and the Fleet menu lists them under the seat's build; a silent seat (whose stale
/// row asks something) is listed as not answering, its need never shown; and no child starts
/// while the glance reads. The survey's read, counted the same way, starts one: the count is no
/// count of nothing.
#[cfg(unix)]
#[test]
fn the_glance_spawns_no_child_process_and_shows_a_fixtures_now_needs_rows() {
    use aterm_mesh::presence::{Behind, Build, PresenceRow, Seat, Standing};
    use aterm_mesh::view::View;
    use aterm_metrics::row::{HeldEntry, ManagerEntry, NeedEntry, NeedsRow, Sid, Word};
    use aterm_metrics::subject::Node;
    const NOW: u64 = 1_800_000_000_000;
    let state = aterm_tempfile::Builder::new()
        .prefix("ag")
        .tempdir_in("/tmp")
        .expect("a scratch state dir");
    let mesh = state.path().join(aterm_mesh::logs::MESH_DIR);
    std::fs::create_dir_all(&mesh).expect("the mesh folder");
    let principal = |n: u8| format!("n-{}", aterm_codec::hex::encode_lower(&[n; 8]));
    let seat = |n: u8, standing: Standing| Seat {
        principal: principal(n),
        key: [n; 32],
        me: n == 1,
        standing,
        behind: Behind::InStep,
        row: Some((
            3,
            PresenceRow {
                at_ms: NOW - 1_000,
                build: Build::new("0.159.0+g4ef9cd6144").expect("a build"),
                addresses: vec!["192.0.2.7:7100".parse().expect("an address")],
                membership: None,
                tables: None,
                topics: Vec::new(),
            },
        )),
    };
    let sid = |text: &str| Sid::parse(text).expect("a sid");
    let (worker, manager, held) = (
        sid("s-0123456789abcdef0123"),
        sid("s-aaaaaaaaaaaaaaaaaaaa"),
        sid("s-bbbbbbbbbbbbbbbbbbbb"),
    );
    let up_needs = NeedsRow {
        at: NOW / 1000 - 120,
        needs: vec![NeedEntry {
            sid: worker,
            word: Word::new("stalled").expect("a word"),
            harness: false,
            worker_of: Some(manager),
            since: Some(NOW / 1000 - 120),
        }],
        held: vec![HeldEntry {
            sid: held,
            hold: Word::new("drive").expect("a word"),
            by: Some(Node::parse(&principal(1)).expect("this Mac's node")),
            since: None,
        }],
        managers: vec![ManagerEntry {
            sid: manager,
            workers: 1,
        }],
    };
    let silent_needs = NeedsRow {
        at: NOW / 1000 - 4 * 3_600,
        needs: vec![NeedEntry {
            sid: sid("s-cccccccccccccccccccc"),
            word: Word::new("ask").expect("a word"),
            harness: false,
            worker_of: None,
            since: None,
        }],
        held: Vec::new(),
        managers: Vec::new(),
    };
    let view = View {
        me: principal(1),
        at_ms: NOW,
        standings: true,
        seats: vec![
            seat(1, Standing::Up),
            seat(2, Standing::Up),
            seat(
                3,
                Standing::Silent {
                    last_seen_ms: Some(NOW - 3 * 3_600_000),
                },
            ),
        ],
        needs: Some(
            [(principal(2), up_needs), (principal(3), silent_needs)]
                .into_iter()
                .collect(),
        ),
        activity: None,
    };
    let served =
        aterm_mesh::view::serve_fixture(&mesh, view.clone()).expect("the node's view served");

    // No vault: the copy reads no membership log, so every seat is the view's, unlabelled.
    let mut copy = runner(true, Some(state.path().to_path_buf()), None);
    assert!(!copy.dials(), "a read of this Mac's copy dials no Mac");
    let (read, spawned) = aterm_uds::spawnfd::spawns_during(|| copy.read(None, &|| NOW));
    drop(served);
    assert_eq!(spawned, 0, "the glance's read started a child");
    let snapshot = read.expect("the copy reads");
    let up = principal(2);
    assert_eq!(snapshot.macs.len(), 1, "{snapshot:?}");
    let mac = &snapshot.macs[0];
    assert_eq!(
        (mac.name.as_str(), mac.node.as_deref(), mac.host.as_deref()),
        (up.as_str(), Some(up.as_str()), Some("192.0.2.7"))
    );
    assert_eq!(mac.build.as_deref(), Some("0.159.0+g4ef9cd6144"));
    assert_eq!(
        mac.needs,
        vec![FleetNeed {
            sid: worker.to_string(),
            word: NeedWord::Stalled,
            who: crate::presence::short_sid(&worker.to_string()),
            worker_of: Some(crate::presence::short_sid(&manager.to_string())),
            age_ms: Some(120_000),
            what: None,
            claim: None,
            carries_on: false,
            ids: Vec::new(),
        }]
    );
    assert_eq!(
        mac.held_here
            .iter()
            .map(|h| h.sid.as_str())
            .collect::<Vec<_>>(),
        vec![held.to_string().as_str()]
    );
    assert_eq!(
        mac.managers
            .iter()
            .map(|m| (m.sid.as_str(), m.workers))
            .collect::<Vec<_>>(),
        vec![(manager.to_string().as_str(), 1)]
    );
    assert_eq!(
        snapshot
            .unanswered
            .iter()
            .map(|u| (u.name.as_str(), u.why.as_str()))
            .collect::<Vec<_>>(),
        vec![(principal(3).as_str(), "silent for 3 h")]
    );
    assert_eq!(
        snapshot.seats.as_ref().map(|s| (s.members, s.up)),
        Some((3, Some(2)))
    );
    assert_eq!(snapshot.agents(), None, "a copy's rows count no agents");

    // A view whose node has not read the standings, or could not read the needs rows, is a
    // read that failed: the last good one stands, and no up seat reads as needing nothing.
    for (unready, why) in [
        (
            View {
                standings: false,
                ..view.clone()
            },
            "standings",
        ),
        (
            View {
                needs: None,
                ..view.clone()
            },
            "needs rows",
        ),
    ] {
        let served = aterm_mesh::view::serve_fixture(&mesh, unready).expect("served");
        let read = copy.read(None, &|| NOW);
        drop(served);
        assert!(
            read.as_ref().is_err_and(|e| e.contains(why)),
            "{why}: {read:?}"
        );
    }

    let slot = FleetSlot {
        generation: 1,
        snapshot: Some(Arc::new(snapshot)),
        book: BookFacts {
            in_fleet: true,
            ..BookFacts::default()
        },
        ..FleetSlot::default()
    };
    let texts = menu_texts(&crate::status_item::compose_fleet_menu(
        &slot,
        i64::try_from(NOW / 1000).expect("seconds"),
    ));
    // `status`'s members= reads the copy's seats while the read stands, never once it failed or
    // went stale.
    let glance = glance_over(slot.clone());
    assert_eq!(
        glance.seats(NOW).map(|s| (s.members, s.up)),
        Some((3, Some(2)))
    );
    assert_eq!(
        glance.seats(NOW + millis(COUNT_FRESH) + 1),
        None,
        "a stale read"
    );
    let failed = glance_over(FleetSlot {
        error: Some("the node could not read the needs rows".to_owned()),
        ..slot.clone()
    });
    assert_eq!(failed.seats(NOW), None, "a failed last read");
    for want in [
        "at 0.159.0+g4ef9cd6144",
        "agents not read",
        "1 not answering",
    ] {
        assert!(texts.iter().any(|t| t == want), "{want}: {texts:?}");
    }
    assert!(
        texts
            .iter()
            .any(|t| t.contains(&crate::presence::short_sid(&worker.to_string()))),
        "the stalled worker is listed: {texts:?}"
    );
    assert!(
        texts.iter().any(|t| t.contains("silent for 3 h")),
        "{texts:?}"
    );
    assert!(
        !texts.iter().any(|t| t.contains("cccc")),
        "a silent seat's need is never shown: {texts:?}"
    );

    let mut survey = ChildRunner {
        program: PathBuf::from("/bin/echo"),
        args: vec!["[]".to_string()],
        cap: Duration::from_secs(60),
    };
    assert!(survey.dials());
    let (surveyed, spawned) = aterm_uds::spawnfd::spawns_during(|| survey.read(None, &|| NOW));
    assert!(surveyed.is_ok(), "{surveyed:?}");
    assert_eq!(spawned, 1, "the survey's read starts its child");
}

/// AN AGENT'S OWN WORD IS KEPT (Fleet Field §6, B2 #7): a Mac's agents tally by their rows'
/// `agent=` words, so an agent that asks or sits at a wall is never counted at work, a prompt its
/// harness answers (`needs=-`) is at work as `aterm ctl survey` classes it, and a shell is a
/// session and no agent.
#[test]
fn tally_keeps_agent_words() {
    fn agent<'a>(sid: &'a str, word: &'a str) -> Vec<(&'a str, &'a str)> {
        vec![
            ("sid", sid),
            ("state", "alive"),
            ("program", "claude"),
            ("program_kind", "agent"),
            ("agent", word),
            ("needs", "-"),
        ]
    }
    let words = [
        "busy",
        "prompt",
        "question",
        "idle",
        "wall:usage-weekly",
        "wall:auth",
        "unknown",
        "zzz",
        "prompt",
    ];
    let sids = [
        "s-a", "s-b", "s-c", "s-d", "s-e", "s-f", "s-g", "s-h", "s-i",
    ];
    let mut rows: Vec<Vec<(&str, &str)>> =
        sids.iter().zip(words).map(|(s, w)| agent(s, w)).collect();
    rows[0].extend([
        ("task", "exited"),
        ("task_conclusion", "failed"),
        ("ctx_left_pct", "42"),
        ("usage_7d_pct", "97"),
    ]);
    // The first prompt and the question name their need for a person; the last prompt's row
    // read its needs and found none (`needs=-`).
    rows[1][5].1 = "approval";
    rows[2][5].1 = "question";
    rows[3].push(("task", "done"));
    rows.push(QUIET.to_vec());
    let read = |rows: &[Vec<(&str, &str)>]| {
        let refs: Vec<&[(&str, &str)]> = rows.iter().map(Vec::as_slice).collect();
        let json = format!("[{}]", peer("m7", &pairs(&header("n7")), &refs));
        parse(&json, None, 1_000).expect("a survey")
    };
    let snap = read(&rows);
    let mac = &snap.macs[0];
    let tally = mac.activity.expect("its agents were read");
    assert_eq!(
        tally,
        AgentTally {
            agents: 9,
            busy: 2,
            idle: 1,
            waiting: 2,
            walled: 2,
            unknown: 2,
        }
    );
    assert_eq!(mac.agent_sessions[8].agent, AgentWord::Busy);
    // Negative control: the same prompt once its row names a need waits on a person.
    let mut asked = rows.clone();
    asked[8][5].1 = "approval";
    let asked = read(&asked).macs[0].activity.expect("read");
    assert_eq!((asked.busy, asked.waiting), (1, 3));
    assert_eq!(snap.agents(), Some(9));
    // The shell row is a session and no agent.
    assert_eq!((mac.sessions, mac.agent_sessions.len()), (Some(10), 9));
    assert!(mac.agent_sessions.iter().all(|s| s.sid != "s-q"));
    let first = &mac.agent_sessions[0];
    assert_eq!(
        (
            first.agent.clone(),
            first.task,
            first.ctx_left_pct,
            first.usage_5h_pct,
            first.usage_7d_pct
        ),
        (
            AgentWord::Busy,
            Some(TaskWord::Failed),
            Some(42),
            None,
            Some(97)
        )
    );
    assert_eq!(mac.agent_sessions[3].task, Some(TaskWord::Done));
    assert_eq!(
        mac.agent_sessions[4].agent,
        AgentWord::Walled("usage-weekly".to_owned())
    );
    assert_eq!(mac.agent_sessions[7].agent, AgentWord::Unknown);
    assert_eq!(mac.sessions_more, 0);

    // Past the per-Mac cap the rest are counted, never kept; the tally holds every one.
    let many: Vec<String> = (0..SESSIONS_MAX + 3).map(|n| format!("s-{n}")).collect();
    let rows: Vec<Vec<(&str, &str)>> = many.iter().map(|sid| agent(sid, "busy")).collect();
    let refs: Vec<&[(&str, &str)]> = rows.iter().map(Vec::as_slice).collect();
    let json = format!("[{}]", peer("m8", &pairs(&header("n8")), &refs));
    let snap = parse(&json, None, 1_000).expect("a survey");
    let mac = &snap.macs[0];
    assert_eq!(
        (mac.agent_sessions.len(), mac.sessions_more),
        (SESSIONS_MAX, 3)
    );
    assert_eq!(
        mac.activity.map(|t| usize::from(t.busy)),
        Some(SESSIONS_MAX + 3)
    );

    // An older aterm's agents are not read: absent, never zero.
    let older: &[(&str, &str)] = &[
        ("sid", "s-o"),
        ("state", "alive"),
        ("program", "claude"),
        ("program_kind", "agent"),
        ("agent", "busy"),
    ];
    let json = format!("[{}]", peer("m9", &pairs(&header("n9")), &[older]));
    let snap = parse(&json, None, 1_000).expect("a survey");
    assert_eq!(snap.macs[0].activity, None);
    assert_eq!(snap.agents(), None, "no Mac's agents were read");
}

/// THE GLANCE READS THE PRODUCER'S REASON TOKEN (Fleet Field §10, B2 #11, the GUI half): every
/// token aterm-net types round-trips through [`Reach::parse`]; a token this build does not know,
/// or none, is [`Reach::Unread`] with the client's words kept; and two Macs that did not answer
/// in time at different addresses share one reason, whatever their words.
#[cfg(unix)]
#[test]
fn reasons_typed_by_producer() {
    for token in aterm_net::fleet_member::REASONS {
        assert_eq!(Reach::parse(token).token(), Some(*token), "{token}");
    }
    let row = |label: &str, why: &str, reason: Option<&str>| {
        reason.map_or_else(
            || unanswered(label, why),
            |reason| {
                format!(
                    "{{\"label\":\"{label}\",\"fleet\":true,\"error\":\"{why}\",\
                     \"reason\":\"{reason}\"}}"
                )
            },
        )
    };
    let json = format!(
        "[{}]",
        [
            row(
                "m1",
                "192.0.2.1:7100 did not answer in 3 s",
                Some("no-answer")
            ),
            row(
                "m2",
                "192.0.2.2:7100 did not answer in 3 s",
                Some("no-answer")
            ),
            row("m3", "a reason from a later build", Some("zzz-later")),
            row("m4", "an older peer's words", None),
            row(
                "m5",
                "192.0.2.5:7100 refused the connection",
                Some("nothing-listens")
            ),
        ]
        .join(",")
    );
    let snap = parse(&json, None, 1_000).expect("a survey");
    let reasons: Vec<(&str, Reach, &str)> = snap
        .unanswered
        .iter()
        .map(|u| (u.name.as_str(), u.reason, u.why.as_str()))
        .collect();
    assert_eq!(
        reasons,
        vec![
            (
                "m1",
                Reach::NoAnswer,
                "192.0.2.1:7100 did not answer in 3 s"
            ),
            (
                "m2",
                Reach::NoAnswer,
                "192.0.2.2:7100 did not answer in 3 s"
            ),
            ("m3", Reach::Unread, "a reason from a later build"),
            ("m4", Reach::Unread, "an older peer's words"),
            (
                "m5",
                Reach::NothingListens,
                "192.0.2.5:7100 refused the connection"
            ),
        ]
    );
    // The two that did not answer group as one: their reason is one, their words are not.
    assert_ne!(snap.unanswered[0].why, snap.unanswered[1].why);
    let groups: std::collections::BTreeSet<Option<&str>> =
        snap.unanswered.iter().map(|u| u.reason.token()).collect();
    assert_eq!(groups.len(), 3, "{groups:?}");
    // The glance's own reasons name no producer token.
    for own in [
        Reach::Silent { since_ms: 1 },
        Reach::NeverUp,
        Reach::PastCap,
        Reach::NoHeader,
        Reach::Unread,
    ] {
        assert_eq!(own.token(), None, "{own:?}");
    }
    let headless = "[{\"label\":\"m6\",\"fleet\":true}]";
    let snap = parse(headless, None, 1_000).expect("a survey");
    assert_eq!(snap.unanswered[0].reason, Reach::NoHeader);
}

/// THIS MAC'S COPY JOINS `aterm fleet`'S ROWS (Fleet Field §11, B2 #13): each seat of the view
/// takes its row's log label, build (the book's where it has no presence row), update and tables,
/// and the snapshot's seats are the composition's whole; a seat whose node no row names keeps
/// its book host's name and no label, and a seat of the log the view does not hold is counted as
/// never up.
#[cfg(unix)]
#[test]
fn snapshot_of_joins_seat_rows() {
    use aterm_agent::fleet_view::{Fleet, SeatRow};
    use aterm_mesh::presence::{Behind, Build, PresenceRow, Seat, Standing};
    use aterm_mesh::view::View;
    use aterm_netplace::words::{InStep, Seats};
    const NOW: u64 = 1_800_000_000_000;
    let principal = |n: u8| format!("n-{}", aterm_codec::hex::encode_lower(&[n; 8]));
    let presence = |build: &str| PresenceRow {
        at_ms: NOW - 1_000,
        build: Build::new(build).expect("a build"),
        addresses: vec!["192.0.2.7:7100".parse().expect("an address")],
        membership: None,
        tables: None,
        topics: Vec::new(),
    };
    let lag = Behind::Behind {
        at_least: Duration::from_secs(240),
    };
    let seat = |n: u8, standing: Standing, row: Option<PresenceRow>| Seat {
        principal: principal(n),
        key: [n; 32],
        me: n == 1,
        standing,
        behind: Behind::InStep,
        row: row.map(|row| (3, row)),
    };
    let view = View {
        me: principal(1),
        at_ms: NOW,
        standings: true,
        seats: vec![
            seat(1, Standing::Up, Some(presence("0.160.0"))),
            seat(2, Standing::Up, Some(presence("0.156.0"))),
            seat(
                3,
                Standing::Silent {
                    last_seen_ms: Some(NOW - 3_600_000),
                },
                None,
            ),
            seat(4, Standing::Up, Some(presence("0.160.0"))),
        ],
        needs: Some(std::collections::BTreeMap::new().into_iter().collect()),
        activity: None,
    };
    let seat_row = |label: &str, n: Option<u8>, build: Option<&str>| SeatRow {
        label: label.to_owned(),
        name: format!("{label}.local"),
        node: n.map(principal),
        me: n == Some(1),
        presence: None,
        build: build.map(str::to_owned),
        needs_update: false,
        behind: None,
    };
    let seats = Seats {
        members: 5,
        up: Some(3),
        needs_update: vec![("m4".to_owned(), "0.147.0".to_owned())],
        in_step: Some(InStep {
            with: 1,
            of: 2,
            slowest_ms: Some(240_000),
            apart: 0,
        }),
        log: Some((5000, 65536)),
    };
    let fleet = Fleet {
        seats: seats.clone(),
        rows: vec![
            seat_row("m1", Some(1), Some("0.160.0")),
            SeatRow {
                needs_update: true,
                behind: Some(lag),
                ..seat_row("m22", Some(2), Some("0.156.0"))
            },
            // No presence row: its build is its signed book entry's, as the composition gave it.
            SeatRow {
                needs_update: true,
                ..seat_row("m4", Some(3), Some("0.147.0"))
            },
            seat_row("m9", None, Some("0.150.0")),
        ],
        unserved: None,
    };
    let book_peer = |n: u8, host: &str| aterm_net::fleet_member::Peer {
        node: None,
        key: Some(aterm_net::fleet::NodeKey::from_bytes([n; 32])),
        key_from: None,
        host: host.to_owned(),
        addr: None,
        port: 7100,
        version: None,
        seen_ms: None,
        via: aterm_net::fleet_member::Via::Joined,
        fleet: None,
        signed: None,
    };
    let book = [
        book_peer(2, "mac-22.local"),
        book_peer(3, "mac-4.local"),
        book_peer(4, "studio.local"),
    ];
    let snap = snapshot_of(&view, &fleet, &book, NOW);
    assert_eq!(
        snap.seats.as_ref(),
        Some(&seats),
        "the composition's seats, whole"
    );

    let m22 = snap
        .macs
        .iter()
        .find(|m| m.node.as_deref() == Some(principal(2).as_str()))
        .expect("m22 is up");
    assert_eq!(
        (
            m22.name.as_str(),
            m22.label.as_deref(),
            m22.build.as_deref(),
            m22.needs_update,
            m22.tables
        ),
        ("mac-22", Some("m22"), Some("0.156.0"), true, Some(lag))
    );
    assert_eq!(m22.activity, None, "a view of no activity rows reads none");

    // Negative control: a seat whose node no row names keeps its book host's name, no label.
    let studio = snap
        .macs
        .iter()
        .find(|m| m.node.as_deref() == Some(principal(4).as_str()))
        .expect("studio is up");
    assert_eq!(
        (
            studio.name.as_str(),
            studio.label.as_deref(),
            studio.build.as_deref(),
            studio.needs_update,
            studio.tables
        ),
        ("studio", None, Some("0.160.0"), false, None)
    );
    assert_eq!(snap.macs.len(), 2, "this Mac is no seat: {:?}", snap.macs);

    let silent: Vec<_> = snap
        .unanswered
        .iter()
        .map(|u| {
            (
                u.name.as_str(),
                u.label.as_deref(),
                u.reason,
                u.build.as_deref(),
                u.silent_since_ms,
                u.why.as_str(),
                u.needs_update,
            )
        })
        .collect();
    assert_eq!(
        silent,
        vec![
            (
                "mac-4",
                Some("m4"),
                Reach::Silent {
                    since_ms: NOW - 3_600_000
                },
                Some("0.147.0"),
                Some(NOW - 3_600_000),
                "silent for 1 h",
                true,
            ),
            (
                "m9",
                Some("m9"),
                Reach::NeverUp,
                Some("0.150.0"),
                None,
                "silent, never seen up here",
                false,
            ),
        ]
    );
}

/// THE GLANCE'S GENERATION IS READ WITHOUT A COPY (Fleet Field §12): the band's service turn
/// sees a new read, a failure and the fleet's book move by three words under the slot's lock.
#[test]
fn the_generation_moves_with_the_slot() {
    let slot = slot_of(1_000, vec![mac17(1_000, false)], Vec::new());
    let glance = glance_over(slot.clone());
    assert_eq!(glance.generation(), (1, false, true));
    {
        let mut held = glance.slot.lock().expect("the slot");
        held.generation = 2;
        held.error = Some("the read failed".to_owned());
    }
    assert_eq!(glance.generation(), (2, true, true));
    let outside = glance_over(FleetSlot {
        book: BookFacts::default(),
        ..slot
    });
    assert_eq!(outside.generation(), (1, false, false));
}

/// A survey that refused the whole fleet's read writes one row for it, which names no Mac: the
/// read failed, and no seat is silent. Negative control: a Mac's own `not-dialled` row is a
/// silent seat.
#[test]
fn a_refused_fleet_read_is_no_seat() {
    let row = |reason: &str| {
        format!(
            "[{{\"label\":\"the fleet\",\"fleet\":true,\"error\":\"the book cannot be read\",\
             \"reason\":\"{reason}\"}}]"
        )
    };
    let refused = parse(&row(aterm_ctl::FLEET_UNREAD), None, 1_000);
    assert_eq!(
        refused,
        Err("fleet not read: the book cannot be read".to_owned())
    );
    let one = parse(&row("not-dialled"), None, 1_000).expect("a read");
    assert_eq!(one.unanswered.len(), 1);
    assert_eq!(one.unanswered[0].reason, Reach::NotDialled);
}

/// The survey rows of `rows` (each its `key=value` fields) as a window's survey answer, and as the
/// fleet read's JSON of one peer: the two forms one count reads.
#[cfg(unix)]
fn survey_forms(rows: &[Vec<(&str, &str)>]) -> (String, String) {
    let mut wire = format!(
        "OK {}\ninstance pid=77 host=mac.local sessions={} level=facts cut=0 node=n7\n",
        rows.len() + 1,
        rows.len()
    );
    for row in rows {
        let fields: Vec<String> = row
            .iter()
            .map(|(k, v)| format!("{k}={}", aterm_codec::pct::encode(v)))
            .collect();
        wire.push_str(&format!("session {}\n", fields.join(" ")));
    }
    let refs: Vec<&[(&str, &str)]> = rows.iter().map(Vec::as_slice).collect();
    let json = format!("[{}]", peer("m7", &pairs(&header("n7")), &refs));
    (wire, json)
}

/// One agent session's survey row.
#[cfg(unix)]
fn agent_row<'a>(sid: &'a str, word: &'a str, needs: Option<&'a str>) -> Vec<(&'a str, &'a str)> {
    let mut row = vec![
        ("sid", sid),
        ("state", "running"),
        ("program", "claude"),
        ("program_kind", "agent"),
        ("agent", word),
    ];
    row.extend(needs.map(|needs| ("needs", needs)));
    row
}

/// THE NODE COUNTS ITS WINDOWS' AGENTS AS A PEER'S SURVEY IS COUNTED (Fleet Field §11, §14): from
/// a window's survey answer, a `prompt` whose row read no need for a person (`needs=-`) is busy,
/// one naming a need (`needs=ask`) waits, as does a `question` whose `needs` names one; a wall is
/// walled with its kind; an exited agent and a shell are not counted; and the sessions are listed
/// by their minted sids and words. The fleet read's JSON of the same rows counts the same tally.
/// An answer that is no survey, and one a row of which sends no `needs` (an older window), count
/// nothing, as an older peer's rows do; so does a partial answer, which the row cannot mark as a
/// lower bound: a window left unread (`instances_read=1` of `instances=2`) or rows the reply cap
/// cut (`sessions=` past the rows sent). Every window read, the merged answer counts.
/// NEGATIVE CONTROL: flipping one prompt's `needs=-` to `ask` moves it from busy to waiting in
/// both counts alike.
#[cfg(unix)]
#[test]
fn the_nodes_count_of_its_windows_is_the_surveys_count() {
    use aterm_metrics::row::{AgentEntry, Sid, Word};
    let sids = [
        "s-00000000000000000001",
        "s-00000000000000000002",
        "s-00000000000000000003",
        "s-00000000000000000004",
        "s-00000000000000000005",
        "s-00000000000000000006",
    ];
    let mut rows = vec![
        agent_row(sids[0], "prompt", Some("-")),
        agent_row(sids[1], "prompt", Some("ask")),
        agent_row(sids[2], "question", Some("question")),
        agent_row(sids[3], "wall:usage-weekly", Some("wall")),
        agent_row(sids[4], "idle", Some("-")),
        agent_row("s-short", "busy", Some("-")),
        QUIET.to_vec(),
    ];
    let mut exited = agent_row(sids[5], "busy", Some("-"));
    exited[1].1 = "exited";
    rows.push(exited);
    let count = |rows: &[Vec<(&str, &str)>]| {
        let (wire, json) = survey_forms(rows);
        let row = activity_of_survey(&wire, 1_793_160_000).expect("a survey counts");
        let read = parse(&json, None, 1_000).expect("a fleet read");
        (row, read.macs[0].activity.expect("its agents were read"))
    };
    let (row, tally) = count(&rows);
    assert_eq!(row.at, 1_793_160_000);
    assert_eq!(
        (row.busy, row.idle, row.waiting, row.walled, row.unknown),
        (2, 1, 2, 1, 0)
    );
    assert_eq!(AgentTally::of_row(&row), tally, "the survey's own count");
    let entry = |n: usize, word: &str, detail: Option<&str>| AgentEntry {
        sid: Sid::parse(sids[n]).expect("a sid"),
        word: Word::new(word).expect("a word"),
        detail: detail.and_then(Word::new),
    };
    assert_eq!(
        row.sessions,
        vec![
            entry(0, "busy", None),
            entry(1, "waiting", None),
            entry(2, "waiting", None),
            entry(3, "walled", Some("usage-weekly")),
            entry(4, "idle", None),
        ],
        "a sid that is no minted one is counted, not listed"
    );
    let mut asked = rows.clone();
    asked[0][5].1 = "ask";
    let (row, tally) = count(&asked);
    assert_eq!((row.busy, row.waiting), (1, 3));
    assert_eq!(AgentTally::of_row(&row), tally);

    assert_eq!(activity_of_survey("ERR busy: ask again\n", 1), None);
    let mut older = rows.clone();
    older[4].truncate(5);
    let (wire, json) = survey_forms(&older);
    assert_eq!(activity_of_survey(&wire, 1), None, "an older window's rows");
    assert_eq!(
        parse(&json, None, 1_000).expect("read").macs[0].activity,
        None
    );
    let (wire, _) = survey_forms(&[]);
    assert_eq!(
        activity_of_survey(&wire, 5).map(|row| row.agents()),
        Some(0),
        "a window of no agents"
    );
    let (wire, _) = survey_forms(&rows);
    let marked = |from: &str, to: &str| wire.replacen(from, to, 1);
    assert_eq!(
        activity_of_survey(
            &marked(" node=n7", " node=n7 instances=2 instances_read=1"),
            1
        ),
        None,
        "a window left unread"
    );
    assert_eq!(
        activity_of_survey(&marked(" node=n7", " node=n7 instances=2"), 1),
        None,
        "a window that answered alone of two"
    );
    let claimed = format!(" sessions={} ", rows.len() + 1);
    assert_eq!(
        activity_of_survey(&marked(&format!(" sessions={} ", rows.len()), &claimed), 1),
        None,
        "rows the reply cap cut"
    );
    assert_eq!(
        activity_of_survey(
            &marked(" node=n7", " node=n7 instances=2 instances_read=2"),
            1
        )
        .map(|row| row.agents()),
        Some(6),
        "every window read"
    );
}

/// ON THIS MAC'S COPY, A SEAT'S ACTIVITY IS ITS FRESH `now/activity` ROW (Fleet Field §11, §14):
/// an up seat whose row trails its presence row by no more than `ACTIVITY_FRESH_MS` reads its
/// tally and its sessions by sid and word (none of their program, task or context), the agents
/// past those listed counted; the field draws its mark (busy, or a wall over busy). A seat with no
/// row, and one whose row trails its presence row a second past the bound, read no activity, and
/// draw no activity mark; each says why (unpublished, stale by how much), as a view whose node
/// read no activity rows does. The menu's row of a read seat counts its agents and no sessions,
/// which no copy reads.
/// NEGATIVE CONTROL: the survey path, which reads its sessions, still counts them on that row.
#[cfg(unix)]
#[test]
fn a_seats_fresh_activity_row_is_its_activity_on_the_copy() {
    use aterm_mesh::presence::{Behind, Build, PresenceRow, Seat, Standing};
    use aterm_mesh::view::View;
    use aterm_metrics::row::{ACTIVITY_FRESH_MS, ActivityRow, AgentEntry, Sid, Word};
    const NOW: u64 = 1_800_000_000_000;
    let principal = |n: u8| format!("n-{}", aterm_codec::hex::encode_lower(&[n; 8]));
    let seat = |n: u8| Seat {
        principal: principal(n),
        key: [n; 32],
        me: n == 1,
        standing: Standing::Up,
        behind: Behind::InStep,
        row: Some((
            3,
            PresenceRow {
                at_ms: NOW - 1_000,
                build: Build::new("0.160.0").expect("a build"),
                addresses: Vec::new(),
                membership: None,
                tables: None,
                topics: Vec::new(),
            },
        )),
    };
    let sid = Sid::parse("s-0123456789abcdef0123").expect("a sid");
    let fresh = ActivityRow {
        at: (NOW - 1_000 - ACTIVITY_FRESH_MS) / 1000,
        busy: 3,
        walled: 1,
        idle: 1,
        sessions: vec![
            AgentEntry {
                sid,
                word: Word::new("busy").expect("a word"),
                detail: None,
            },
            AgentEntry {
                sid,
                word: Word::new("walled").expect("a word"),
                detail: Word::new("auth"),
            },
        ],
        ..ActivityRow::default()
    };
    let stale = ActivityRow {
        at: fresh.at - 1,
        ..fresh.clone()
    };
    let view = View {
        me: principal(1),
        at_ms: NOW,
        standings: true,
        seats: vec![seat(1), seat(2), seat(3), seat(4), seat(5)],
        needs: Some(std::collections::BTreeMap::new()),
        activity: Some(
            [
                (principal(2), fresh.clone()),
                (principal(4), stale),
                (
                    principal(5),
                    ActivityRow {
                        at: fresh.at,
                        busy: 2,
                        ..ActivityRow::default()
                    },
                ),
            ]
            .into_iter()
            .collect(),
        ),
    };
    let fleet = aterm_agent::fleet_view::Fleet {
        seats: aterm_netplace::words::Seats::default(),
        rows: Vec::new(),
        unserved: None,
    };
    let mut snapshot = snapshot_of(&view, &fleet, &[], NOW);
    let mac = |snapshot: &FleetSnapshot, n: u8| {
        snapshot
            .macs
            .iter()
            .find(|m| m.node.as_deref() == Some(principal(n).as_str()))
            .cloned()
            .expect("an up seat")
    };
    let read = mac(&snapshot, 2);
    assert_eq!(
        read.activity,
        Some(AgentTally {
            agents: 5,
            busy: 3,
            idle: 1,
            waiting: 0,
            walled: 1,
            unknown: 0,
        })
    );
    assert_eq!(
        read.agent_sessions
            .iter()
            .map(|s| (s.sid.as_str(), s.agent.clone(), s.program.is_none(), s.task))
            .collect::<Vec<_>>(),
        vec![
            (sid.to_string().as_str(), AgentWord::Busy, true, None),
            (
                sid.to_string().as_str(),
                AgentWord::Walled("auth".to_owned()),
                true,
                None
            ),
        ]
    );
    assert_eq!(read.sessions_more, 3, "the agents past those listed");
    assert_eq!(snapshot.agents(), Some(7));
    assert_eq!(mac(&snapshot, 3).activity, None, "no activity row");
    assert_eq!(mac(&snapshot, 4).activity, None, "a row past the bound");
    assert_eq!(
        [2, 3, 4].map(|n| mac(&snapshot, n).activity_absent),
        [
            None,
            Some(ActivityAbsent::Unpublished),
            Some(ActivityAbsent::Stale {
                behind_ms: Some(ACTIVITY_FRESH_MS + 1_000)
            }),
        ]
    );
    assert_eq!(read.sessions, None, "the copy reads no session count");
    let unread = View {
        activity: None,
        ..view.clone()
    };
    assert_eq!(
        mac(&snapshot_of(&unread, &fleet, &[], NOW), 2).activity_absent,
        Some(ActivityAbsent::Unread)
    );

    // The field draws the read seat's mark, and none for the seats not read.
    snapshot.read_at_ms = NOW;
    let slot = FleetSlot {
        generation: 1,
        snapshot: Some(Arc::new(snapshot.clone())),
        book: BookFacts {
            in_fleet: true,
            ..BookFacts::default()
        },
        ..FleetSlot::default()
    };
    let observed = aterm_types::network_state::NetworkObservation {
        visibility: aterm_types::network_state::Visibility::Active,
        history: aterm_types::network_state::HistoryState::Ready,
        ..aterm_types::network_state::NetworkObservation::default()
    };
    let field = crate::network_deck::field::project(Some(&slot), &observed, NOW);
    let mark = |n: u8| {
        let i = snapshot
            .macs
            .iter()
            .position(|m| m.node.as_deref() == Some(principal(n).as_str()))
            .expect("listed");
        field
            .seats
            .iter()
            .find(|seat| seat.src == crate::network_deck::field::SeatSrc::Mac(i))
            .expect("a seat")
            .activity
    };
    assert_eq!(
        mark(2),
        Some(crate::network_deck::field::Activity::Walled),
        "a wall outranks the busy agents"
    );
    assert_eq!(mark(5), Some(crate::network_deck::field::Activity::Busy(2)));
    assert_eq!(mark(3), None);
    assert_eq!(mark(4), None);

    let texts = menu_texts(&crate::status_item::compose_fleet_menu(
        &slot,
        i64::try_from(NOW / 1000).expect("seconds"),
    ));
    assert!(
        texts
            .iter()
            .any(|t| t == "5 agents: 3 busy, 0 waiting, 1 walled"),
        "{texts:?}"
    );
    assert!(
        !texts.iter().any(|t| t.contains("0 sessions")),
        "no session count the copy did not read: {texts:?}"
    );
    let (_, json) = survey_forms(&[
        agent_row("s-00000000000000000001", "prompt", Some("-")),
        QUIET.to_vec(),
    ]);
    let surveyed = FleetSlot {
        snapshot: Some(Arc::new(parse(&json, None, NOW).expect("a fleet read"))),
        ..slot
    };
    let texts = menu_texts(&crate::status_item::compose_fleet_menu(
        &surveyed,
        i64::try_from(NOW / 1000).expect("seconds"),
    ));
    assert!(
        texts
            .iter()
            .any(|t| t == "2 sessions \u{b7} 1 agent: 1 busy, 0 waiting, 0 walled"),
        "{texts:?}"
    );
}
