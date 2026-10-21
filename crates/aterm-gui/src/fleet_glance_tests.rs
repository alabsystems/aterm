// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fleet glance's tests (`docs/DESIGN-owner-surfaces-2026-10-16.md`
//! §10): the read's fixtures, the poll table, the single-flight runner and
//! its kill, and no thread on a headless App.

use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// One fleet peer's JSON entry: its header and session rows, each row a
/// list of `key=value` pairs.
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

/// A fleet peer that did not answer.
fn unanswered(label: &str, why: &str) -> String {
    format!("{{\"label\":\"{label}\",\"fleet\":true,\"error\":\"{why}\"}}")
}

/// A current peer's header.
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

/// A quiet current row.
const QUIET: &[(&str, &str)] = &[
    ("sid", "s-q"),
    ("state", "alive"),
    ("program", "zsh"),
    ("program_kind", "shell"),
    ("agent", "-"),
    ("needs", "-"),
];

/// THE FLEET READ, PARSED (§10's fixtures): 31 peers with 3 timeouts; this
/// machine's own instance and a sibling (no `fleet:true`) and this machine
/// answering through its own book (its node) dropped; a stalled worker's
/// need with its manager; 2 older peers, whose rows carry no `needs=`, kept
/// but needing nothing; a `cut=` reply's rows left out counted.
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
    let snap = parse(&json, false, Some("own-node"), 1_000).expect("a survey");
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
    assert_eq!((m17.agents, m17.working), (2, 1));
    assert_eq!(m17.managers.len(), 1);
    assert_eq!(m17.managers[0].workers, 1);
    assert_eq!(m17.host.as_deref(), Some("mac.local"));
    let old = snap
        .macs
        .iter()
        .find(|m| m.dial == "mac-old")
        .expect("older");
    assert!(old.older && old.needs.is_empty(), "never counted");
    // One row with no `needs=` marks the whole peer older.
    assert_eq!(snap.macs.iter().filter(|m| m.older).count(), 2);
    assert!(
        snap.macs
            .iter()
            .filter(|m| m.older)
            .all(|m| m.needs.is_empty())
    );
    let cut = snap.macs.iter().find(|m| m.dial == "mac-cut").expect("cut");
    assert_eq!(cut.left_out, 3);
    assert_eq!(snap.agents(), 4);
    // NEGATIVE CONTROL: with no node of its own, this machine's answer is a peer.
    let with_self = parse(&json, false, None, 1_000).expect("a survey");
    assert_eq!(with_self.macs.len(), snap.macs.len() + 1);
    assert!(parse("not json", false, None, 0).is_err());
}

/// `entry`, a fleet peer's, as the survey marks one that proved no node key
/// this machine binds (`"fleet":true,"unproven":true`).
fn unproven(entry: &str) -> String {
    entry.replacen("\"fleet\":true", "\"fleet\":true,\"unproven\":true", 1)
}

/// A PEER THAT PROVED NO NODE KEY IS ITS WORD ALONE (`"unproven":true`, the
/// survey's `node unproven`): listed, marked and its needs read, but what it
/// says of itself is not taken — its `node=` neither passes it off as this
/// Mac nor keys its notices, its `host=` sends Screen Sharing and ssh nowhere
/// but the name the book knows it by, and its `drive=1` lets this Mac drive
/// nothing — and none of its needs counts. NEGATIVE CONTROL: the same
/// answers proven are this Mac's own (dropped) and a peer whose node, host
/// and drive are its own.
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
    // (whether the answers are marked unproven, how many are listed,
    // studio's node, host and drive, and whether its needs count).
    type Row = (bool, usize, Option<&'static str>, &'static str, bool, bool);
    let table: [Row; 2] = [
        (false, 1, Some("n9"), "elsewhere.example", true, true),
        (true, 2, None, "studio.local", false, false),
    ];
    for (marked, listed, node, host, drive, counts) in table {
        let entries = [me.clone(), studio.clone()];
        let entries = entries.map(|e| if marked { unproven(&e) } else { e });
        let snap = parse(
            &format!("[{}]", entries.join(",")),
            false,
            Some("own-node"),
            0,
        )
        .expect("a survey");
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

/// A FLEET NEED NAMES ITS TAB AS A LOCAL ONE DOES (`status_item::tab_name`):
/// its role first, then, where the peer serves content, the title its person
/// gave, then its OSC title without the spinner or idle glyph an agent leads
/// it with — so a spinner frame moves neither the row nor its notice — and
/// its program where none is set. An ask's need has no age (its row carries
/// the agent publication's, not the ask's). NEGATIVE CONTROL: at facts level
/// no title is shown, and a stall keeps its task's age.
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
        let snap = parse(&json, false, None, 1_000).expect("a survey");
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

/// A CONTENT PEER SAYS WHAT ITS NEED IS: an ask's row carries the ask's
/// own words, a box its kind, a wall its kind and reset, as a local row
/// does. A frozen program dates by the agent publication only where it is
/// an identified agent (the stall sets that verdict); a plain program's
/// `agent_ms` is its session's age, so its freeze has none. NEGATIVE
/// CONTROLS: at facts level no need carries words, and a frozen agent keeps
/// its age.
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
        parse(&json, false, None, 1_000).expect("a survey").macs[0].needs[0].clone()
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
    // A harness's NEEDS YOU entry reads as a local row does (§12.2.9): the
    // need without the frame, its count whole. NEGATIVE CONTROL (run for
    // the landing): the entry folded whole keeps `claude needs you:` and
    // loses ` (+1 more)` at 72 bytes.
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

/// WHAT THE BOOK SAYS OF THIS MAC (`read_book`): no fleet key is no fleet; a
/// book with peers that joined it is the issuer's; a member whose book holds
/// only its issuer, and no copy of that issuer's book (an older issuer, or no
/// read yet), knows only it, and names it. NEGATIVE CONTROLS: a member whose
/// book also holds a peer named by hand reads that peer too, and one holding
/// its issuer's book (S11) reads the whole fleet, however few members that
/// book names — but never a copy another issuer's book left. A member whose
/// issuer's entry binds no key (it joined before node keys) names that
/// issuer as unbound; NEGATIVE CONTROLS: one that binds its issuer's key,
/// and the issuer, name none.
#[cfg(unix)]
#[test]
fn the_book_says_whether_this_mac_knows_only_its_issuer() {
    use aterm_net::fleet_member::{Peer, Via, update_peers};
    let dir = std::env::temp_dir().join(format!("aterm-read-book-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // The book's directory is private, as the state directory is.
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
            member.issuer,
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
    let issuer = book(vec![peer("mac-1", Via::Joined)]);
    assert!(issuer.issuer && issuer.issuer_label.is_none());
    assert_eq!(
        issuer.unbound_issuer, None,
        "the issuer binds none to itself"
    );
    let more = book(vec![peer("m7", Via::Issuer), peer("mac-9", Via::Manual)]);
    assert_eq!(
        (more.issuer, more.issuer_label),
        (false, None),
        "a peer named by hand is read too"
    );
    let m7 = peer("m7", Via::Issuer).node.expect("an issuer's node");
    aterm_net::fleet_member::write_issuer_book(&dir, &m7, &[]).expect("the copy written");
    let shared = book(vec![peer("m7", Via::Issuer)]);
    assert_eq!(shared.issuer_label, None, "its issuer shares its book");
    aterm_net::fleet_member::write_issuer_book(&dir, &"ff".repeat(16), &[])
        .expect("another issuer's copy written");
    let stale = book(vec![peer("m7", Via::Issuer)]);
    assert_eq!(
        stale.issuer_label.as_deref(),
        Some("m7"),
        "another issuer's copy is no book of this one's"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A PEER IS DIALLED BY THE NAME THE BOOK KNOWS IT BY: the fleet client
/// labels an answer from an address the book does not name `<name> at
/// <address>`, which `survey --host` cannot resolve, so its reads are given
/// the name, its rows show the name's first label, and its notices are keyed
/// by its node, which a new address does not move. Exited rows the header
/// counts are no rows its cap left out. NEGATIVE CONTROL: the whole label is
/// no dial, and a key by label moves with the address.
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
    let snap = parse(&at("192.168.1.17"), false, None, 0).expect("a survey");
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
    // A new address is the same Mac.
    let moved = parse(&at("192.168.1.40"), false, None, 0).expect("a survey");
    assert_eq!(
        herald_key(moved.macs[0].ident(), "s-w"),
        herald_key(m17.ident(), "s-w")
    );
    // NEGATIVE CONTROL.
    assert_eq!(fleet_read_args("mac-17.local at 192.168.1.17", None), None);
    assert_eq!(fleet_read_args("-x", None), None);
    assert_eq!(fleet_read_args("mac-17", Some("not a sid")), None);
    assert_ne!(
        herald_key("mac-17.local at 192.168.1.40", "s-w"),
        herald_key("mac-17.local at 192.168.1.17", "s-w")
    );
}

/// `chrome`'s fleet word tells a Mac in no fleet (`off`) from one in a fleet
/// not read yet (`unread`), and a read from either.
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

/// EVERY PEER REFUSED WITH NO ROUTE: the snapshot says so (the read's
/// stderr's note) and the digest's one entry is the level family's.
#[test]
fn all_no_route_is_one_line_and_one_entry() {
    let hosts: Vec<String> = (0..4)
        .map(|n| unanswered(&format!("mac-{n}"), "no route to 10.0.0.1:7100"))
        .collect();
    let snap = parse(&format!("[{}]", hosts.join(",")), true, None, 0).expect("a survey");
    assert!(snap.no_route && snap.macs.is_empty());
    assert_eq!(snap.unanswered.len(), 4);
    let entry = no_route_entry();
    // The digest's door admits it (a refused entry is recorded, not shown).
    entry.message().expect("an entry the digest shows");
    assert_eq!(
        entry.family(),
        aterm_messages::exceptions::Family::FleetUnreadable
    );
    assert!(
        entry
            .key()
            .starts_with(&aterm_messages::exceptions::Family::FleetUnreadable.prefix())
    );
}

/// WHEN TO READ (§9 S10): never in no fleet; on menu open when the last read
/// is older than 15 s or there is none; on the minute on the issuer or with
/// `[fleet] watch`; a member without watch only at the menu.
#[test]
fn should_poll_reads_on_the_minute_or_at_the_menu() {
    let member = BookFacts {
        in_fleet: true,
        issuer: false,
        issuer_label: Some("m7".to_string()),
        unbound_issuer: None,
    };
    let issuer = BookFacts {
        in_fleet: true,
        issuer: true,
        issuer_label: None,
        unbound_issuer: None,
    };
    let none = BookFacts::default();
    let at = |s: u64| s * 1000;
    // (book, watch, last read, now, menu opening, reads)
    type Case<'a> = (&'a BookFacts, bool, Option<u64>, u64, bool, bool);
    let cases: [Case<'_>; 12] = [
        (&none, true, None, at(100), true, false),
        (&member, false, None, at(100), false, false),
        (&member, false, None, at(100), true, true),
        (&member, false, Some(at(90)), at(100), true, false),
        (&member, false, Some(at(85)), at(100), true, true),
        (&member, false, Some(at(0)), at(100), false, false),
        (&member, true, Some(at(0)), at(59), false, false),
        (&member, true, Some(at(0)), at(60), false, true),
        (&issuer, false, None, at(1), false, true),
        (&issuer, false, Some(at(10)), at(50), false, false),
        (&issuer, false, Some(at(10)), at(70), false, true),
        (&issuer, false, Some(at(95)), at(100), true, false),
    ];
    for (n, (book, watch, last, now, menu, want)) in cases.into_iter().enumerate() {
        assert_eq!(should_poll(book, watch, last, now, menu), want, "case {n}");
    }
}

/// A runner that counts its reads and blocks on a gate until told to answer.
struct Gated {
    runs: Arc<AtomicUsize>,
    gate: Receiver<()>,
    started: Sender<()>,
}

impl FleetRunner for Gated {
    fn run(&mut self) -> ReadAnswer {
        self.runs.fetch_add(1, Ordering::SeqCst);
        let _ = self.started.send(());
        let _ = self.gate.recv();
        Ok(("[]".to_string(), String::new()))
    }
}

/// The seams of a reader in a fleet, its posts counted on `posts`.
fn seams(issuer: bool, posts: Sender<()>) -> ReaderSeams {
    ReaderSeams {
        book: Box::new(move || BookFacts {
            in_fleet: true,
            issuer,
            issuer_label: None,
            unbound_issuer: None,
        }),
        watch: Arc::new(AtomicBool::new(false)),
        own_node: None,
        now_ms: Box::new(|| aterm_time::unix_millis().unwrap_or(0)),
        post: Box::new(move || posts.send(()).is_ok()),
        tick: Duration::from_secs(3600),
    }
}

/// THE RUNNER IS SINGLE-FLIGHT: an issuer reads at once; kicks that arrive
/// while that read is in flight are answered by it — one read for all of
/// them — and a Read Now after it is a read of its own. The waits are hang
/// detectors.
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
    // The read's two posts: it began, it ended.
    posts.recv_timeout(hang).expect("began");
    posts.recv_timeout(hang).expect("ended");
    assert!(!glance.copy().reading);
    assert_eq!(glance.copy().generation, 1);
    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "the kicks were answered by it"
    );
    // A Read Now after it is a read of its own.
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

/// A runner that answers an empty fleet at once.
struct Empty;

impl FleetRunner for Empty {
    fn run(&mut self) -> ReadAnswer {
        Ok(("[]".to_string(), String::new()))
    }
}

/// THE APP IS TOLD WHEN THE LAST READ STOPS COUNTING, with no read: a member
/// (no issuer, no watch) reads only when asked, so nothing else would drop a
/// fleet need from the badge past [`COUNT_FRESH`]. Its first look is told
/// too, so the digest's answer is stated in no fleet. The clock is injected
/// and says when the reader looks; the waits are hang detectors.
/// NEGATIVE CONTROL: before the clock passes the freshness, a whole look
/// after the read's two posts queues none.
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
    // A look reads the clock at its top, judges, posts what is news, and
    // reads it again for its wait; so three clock reads after the read's end
    // was told hold one whole look begun after it (by its order, not by a
    // stopwatch: the tick only paces the looks).
    while looks.try_recv().is_ok() {}
    for _ in 0..3 {
        looks.recv_timeout(hang).expect("the reader looks again");
    }
    assert!(posts.try_recv().is_err(), "a fresh read owes no post");
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

/// A LAPSE THAT PASSES WHILE THE READ'S END IS TOLD IS TOLD TOO: the next
/// lapse is reckoned from the instant the App was told, not from a later
/// look at the clock, so one that passes in between (the reader descheduled
/// after its post, a Mac asleep) is still news. Here the clock passes the
/// read's freshness inside the very post that tells the read's end, the
/// latest the race can put it, and the tick is an hour, so only the lapse
/// can wake the reader. The waits are hang detectors. NEGATIVE CONTROL: the
/// read was fresh when its end was told, and the lapse is no second read.
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
            // The first look, the read's start, then its end: the clock
            // passes the read's freshness as its end is told.
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
/// slot keeps no snapshot, says why, and the App is told. The cap ends the
/// wait; the minute is a hang detector. NEGATIVE CONTROL: a child that
/// answers JSON is a snapshot.
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

/// Seams that touch nothing of this machine's: in no fleet, never posting
/// to an event loop.
fn inert_seams() -> ReaderSeams {
    ReaderSeams {
        book: Box::new(BookFacts::default),
        watch: Arc::new(AtomicBool::new(false)),
        own_node: None,
        now_ms: Box::new(|| aterm_time::unix_millis().unwrap_or(0)),
        post: Box::new(|| true),
        tick: Duration::from_secs(3600),
    }
}

/// NO THREAD ON A HEADLESS APP: the gate refuses it whatever runner and
/// seams it is handed, and `chrome` says `fleet=off`; the real start refuses
/// it there too. NEGATIVE CONTROL: the same start on a windowed App (on
/// macOS, where the reader runs) does start one, and only once.
#[test]
fn a_headless_app_starts_no_fleet_glance() {
    let mut app = crate::App::headless_for_test();
    app.start_fleet_glance_with(Box::new(Empty), inert_seams());
    app.start_fleet_glance();
    assert!(app.fleet_glance.is_none());
    assert!(app.status_item_chrome_line().ends_with(" fleet=off"));
    // NEGATIVE CONTROL.
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

/// A fleet glance handle over `slot`, with no reader thread (its kicks go
/// nowhere): the App reads it as it reads a live one.
fn glance_over(slot: FleetSlot) -> FleetGlance {
    let (kick, _) = std::sync::mpsc::channel();
    FleetGlance {
        slot: Arc::new(Mutex::new(slot)),
        kick,
        watch: Arc::new(AtomicBool::new(false)),
    }
}

/// A slot in a fleet whose last good read, at `read_at_ms`, holds one Mac
/// with one stalled worker, and refused with no route when `dark`.
fn read_slot(read_at_ms: u64, dark: bool) -> FleetSlot {
    let need = FleetNeed {
        sid: "s-w".to_string(),
        word: NeedWord::Stalled,
        who: "claude".to_string(),
        worker_of: None,
        age_ms: Some(1_000),
        what: None,
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
            no_route: dark,
        })),
        book: BookFacts {
            in_fleet: true,
            issuer: true,
            issuer_label: None,
            unbound_issuer: None,
        },
        ..FleetSlot::default()
    }
}

/// THE APP'S FLEET READ (`Wake::FleetRead`): every peer refusing with no
/// route is ONE `FleetUnreadable` digest entry, and a read that reaches a peer
/// again sweeps it (a level family); a fresh read's need posts ONE desktop
/// notice, the same need re-read posts none, and the badge counts it.
/// NEGATIVE CONTROL: the same need from a read older than `COUNT_FRESH`
/// posts nothing and is not counted.
#[test]
fn a_fleet_read_states_its_digest_entry_and_heralds_its_needs() {
    let take_posts =
        || crate::app_tabs::HERALD_POSTS.with(|p| std::mem::take(&mut *p.borrow_mut()));
    let unreadable = |app: &crate::App| {
        let prefix = aterm_messages::exceptions::Family::FleetUnreadable.prefix();
        app.messages
            .digest()
            .entries
            .iter()
            .filter(|e| e.key.starts_with(&prefix))
            .count()
    };
    let settle = |app: &mut crate::App| {
        if let Some(due) = app.messages_deadline() {
            let _ = app.settle_messages(due + Duration::from_secs(1));
        }
    };
    let now = aterm_time::unix_millis().unwrap_or(0);
    let mut app = crate::App::headless_for_test();
    // A windowed instance's fleet glance, its notices on.
    app.headless = false;
    app.config.desktop_alerts = Some(true);
    crate::app_tabs::HERALD_REFUSAL.set(None);
    let _ = take_posts();

    app.fleet_glance = Some(glance_over(read_slot(now, true)));
    app.on_fleet_read();
    assert_eq!(unreadable(&app), 1, "one entry for the whole fleet");
    app.on_fleet_read();
    assert_eq!(unreadable(&app), 1, "stated again, still one");
    assert!(take_posts().is_empty(), "no route is no need");

    app.fleet_glance = Some(glance_over(read_slot(now, false)));
    app.on_fleet_read();
    settle(&mut app);
    assert_eq!(unreadable(&app), 0, "a peer reached sweeps it");
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

    // NEGATIVE CONTROL: a stale read.
    let mut stale = crate::App::headless_for_test();
    stale.headless = false;
    stale.config.desktop_alerts = Some(true);
    let old = now.saturating_sub(millis(COUNT_FRESH) + 60_000);
    stale.fleet_glance = Some(glance_over(read_slot(old, false)));
    stale.on_fleet_read();
    assert!(take_posts().is_empty(), "a stale read heralds nothing");
    let glance = stale.status_glance();
    assert_eq!((glance.needs.len(), glance.need_count()), (1, 0));
}

/// A SEAMLESS SUCCESSOR TELLS NO STANDING FLEET NEED AGAIN: the outgoing
/// herald's told fleet needs, as its park stamps them
/// (`App::fleet_told_rows`) and the successor reads them back
/// (`parse_told`), are taken as told at the successor's first fresh read
/// where it finds the same word on the same Mac's session — whenever that
/// read comes, as one process reading on would — and still count; a need
/// that arises after posts. NEGATIVE CONTROLS: a need the carry does not
/// name (one that arose during the update), the same session carried under
/// another word, and a process that adopted nothing each post; a notice the
/// queue refused is owed, not told, and is not carried.
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

/// The park's told rows read back by herald key; a row that names no fleet
/// key or no word this build knows, or is no row, is dropped.
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

/// A host word is what a `vnc://` URL and an `ssh` line may carry; anything
/// else is no host. A fleet need's herald key is never a local id's.
#[test]
fn host_words_and_herald_keys() {
    for ok in ["mac-17", "mac-17.local", "10.0.0.5", "a_b"] {
        assert_eq!(host_word(ok).as_deref(), Some(ok));
    }
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
/// "ssh" puts the line on the (test) pasteboard, the degrade line opens the
/// Local Network pane. NEGATIVE CONTROL: a name that is no host opens and
/// copies nothing.
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
    app.dispatch_status_action(StatusAction::OpenLocalNetwork);
    assert_eq!(
        crate::platform::take_opened_urls(),
        vec![LOCAL_NETWORK_PANE]
    );
    // NEGATIVE CONTROL.
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

/// mac-17 as read at `read_at_ms`, with its stalled worker when `need`.
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
            })
            .into_iter()
            .collect(),
        read_at_ms,
        ..FleetMac::default()
    }
}

/// A peer that did not answer, by its book name.
fn silent(dial: &str) -> Unanswered {
    Unanswered {
        name: short_name(dial),
        dial: dial.to_string(),
        why: "no connection in 3 s".to_string(),
    }
}

/// An issuer's slot whose last good read, at `read_at_ms`, holds `macs` and
/// did not hear from `unanswered`.
fn slot_of(read_at_ms: u64, macs: Vec<FleetMac>, unanswered: Vec<Unanswered>) -> FleetSlot {
    FleetSlot {
        generation: 1,
        snapshot: Some(Arc::new(FleetSnapshot {
            read_at_ms,
            macs,
            unanswered,
            no_route: false,
        })),
        book: BookFacts {
            in_fleet: true,
            issuer: true,
            issuer_label: None,
            unbound_issuer: None,
        },
        ..FleetSlot::default()
    }
}

/// A READ CARRIES A SILENT MAC'S LAST ROWS WHILE THEY COUNT: a Mac that
/// missed a read is kept, carried, with its own read time — through a
/// further miss too — and is still listed as not answering; past
/// `COUNT_FRESH` of its own read it is only not answering. The reader wakes
/// first at the carried rows' lapse. NEGATIVE CONTROL: with no read before,
/// nothing is carried.
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
        no_route: false,
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
    assert_eq!(carried.next_lapse(t0 + 60_000), Some(t0 + fresh + 1));
    let again = carry(Some(&carried), next(t0 + 120_000), t0 + 120_000);
    assert_eq!(again.macs.len(), 2, "carried through a second miss");
    assert_eq!(again.macs[1].read_at_ms, t0);
    let lapsed = carry(Some(&again), next(t0 + fresh + 1), t0 + fresh + 1);
    assert_eq!(lapsed.macs.len(), 1, "past its own read's freshness");
    // NEGATIVE CONTROL.
    assert_eq!(carry(None, next(t0), t0).macs.len(), 1);
}

/// The notices the App posted since the last take.
fn take_posts() -> Vec<crate::status_item::HeraldNotice> {
    crate::app_tabs::HERALD_POSTS.with(|p| std::mem::take(&mut *p.borrow_mut()))
}

/// A windowed App, its desktop notices on, its notice tap empty.
fn windowed_app() -> crate::App {
    let mut app = crate::App::headless_for_test();
    app.headless = false;
    app.config.desktop_alerts = Some(true);
    crate::app_tabs::HERALD_REFUSAL.set(None);
    let _ = take_posts();
    app
}

/// A STANDING FLEET NEED IS ONE NOTICE (§3: one per transition): a read gone
/// stale, then a read in which its Mac did not answer (its rows too old to
/// carry), then the Mac answering again with the same need, post nothing
/// more and keep its slot, and the badge counts it again. NEGATIVE CONTROL:
/// a fresh read in which the Mac answered WITHOUT the need retires it, so
/// its return is a new notice.
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
    // NEGATIVE CONTROL.
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

/// AN UNPROVEN MAC'S NEED IS NEVER TOLD: what it says is its word alone, so
/// a fresh read of it posts no notice, keeps no slot and counts nothing.
/// NEGATIVE CONTROL: the same need, its Mac proven, posts one and counts.
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

/// A FLEET NEED THAT CANNOT BE TOLD NOW OWES NOTHING: a notice the queue
/// refused is owed (the herald's timer re-asks for it), and once the read
/// goes stale, or its Mac stops answering, the debt is dropped — no read
/// could pay it, and an owed key the App cannot note would wake the timer at
/// once forever. NEGATIVE CONTROL: while the need stands in a fresh read the
/// refused notice stays owed.
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
    // Another need there, refused, then its Mac silent.
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

/// A HOLD THIS MAC PLACED IS READ BACK WHATEVER ITS NEED: a peer's row whose
/// `held_from=` is this Mac's node is a session this Mac holds there — its
/// Hand Back's row — though it needs nothing, since a hold is never a need.
/// NEGATIVE CONTROL: another node's hold is not this Mac's, and a reader
/// that knows no node of its own reads none as its own.
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
    let snap = parse(&text, false, Some(mine), 0).expect("a survey");
    assert_eq!(
        snap.macs[0].held_here,
        [FleetHeld {
            sid: "s-a".to_string(),
            who: "review".to_string(),
        }]
    );
    assert!(snap.macs[0].needs.is_empty(), "a hold is no need");
    let blind = parse(&text, false, None, 0).expect("a survey");
    assert!(blind.macs[0].held_here.is_empty(), "{:?}", blind.macs[0]);
}

/// THE STEP-IN FROM THE MENU BAR: a peer whose header says `drive=1` lets
/// this Mac drive it, and its need's Take Over or Hand Back is `aterm ctl
/// --host <the name the book knows it by> hold <sid> on|off`. NEGATIVE
/// CONTROL: a header without it lets none, and a dial or sid that could be
/// read as more is no command.
#[test]
fn a_peer_that_says_drive_is_held_and_handed_back_over_the_fleet() {
    let mut head = header("n4");
    head.push(("drive", "1".to_string()));
    let text = format!(
        "[{},{}]",
        peer("lab-mini.local", &pairs(&head), &[QUIET]),
        peer("mac-17.local", &pairs(&header("n5")), &[QUIET])
    );
    let snap = parse(&text, false, None, 0).expect("a survey");
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
