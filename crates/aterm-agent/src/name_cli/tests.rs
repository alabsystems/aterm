// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::Cell;
use std::ffi::OsString;
use std::io;
use std::os::unix::fs::PermissionsExt as _;
use std::process::ExitCode;

use aterm_net::fleet::KEY_FILE;
use aterm_net::fleet_member::{PEERS_FILE, node_identity};
use aterm_types::machine_name::{IMPLIED_SINCE, nouns};
use aterm_uds::private_file::write_private;

use super::unix::report;
use super::{Mode, main_entry, parse};
use crate::machine_name::file;
use crate::machine_name::tests::{NOW, entry, record};

fn state(mode: u32) -> aterm_tempfile::TempDir {
    let root = crate::machine_name::tests::state();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(mode))
        .expect("sets the mode");
    root
}

fn args(words: &[&str]) -> Vec<OsString> {
    words.iter().map(OsString::from).collect()
}

fn no_roster() -> io::Result<Option<String>> {
    Ok(None)
}

#[test]
fn name_takes_json_or_help_and_refuses_every_other_word() {
    use std::os::unix::ffi::OsStringExt as _;
    assert_eq!(parse(&args(&[])), Ok(Mode::Show { json: false }));
    assert_eq!(parse(&args(&["--json"])), Ok(Mode::Show { json: true }));
    assert_eq!(parse(&args(&["--json", "-h"])), Ok(Mode::Help));
    assert_eq!(parse(&args(&["--help", "set"])), Ok(Mode::Help));
    for refused in [
        &["--json", "--json"][..],
        &["set", "m8"],
        &["--jsonx"],
        &["json"],
    ] {
        assert!(parse(&args(refused)).is_err(), "{refused:?}");
        assert_eq!(main_entry(args(refused)), ExitCode::from(2), "{refused:?}");
    }
    assert!(
        parse(&[OsString::from_vec(vec![0xff])]).is_err(),
        "not UTF-8"
    );
    assert_eq!(main_entry(args(&["--help"])), ExitCode::from(0));
}

#[test]
fn a_first_read_makes_the_name_and_a_later_one_reads_it_back_with_no_roster_read() {
    let root = state(0o700);
    let asked = Cell::new(0);
    let roster = || {
        asked.set(asked.get() + 1);
        Ok(None)
    };
    let (line, narrowed) = report(root.path(), false, roster, NOW).expect("reports");
    assert_eq!(
        (narrowed, asked.get()),
        (None, 1),
        "the roster read for the first name"
    );
    let (title, rest) = line.split_once("  ").expect("two spaces end the title");
    let nick = title.strip_prefix("m0 ").expect("m0 while unnumbered");
    assert!(
        nouns::NOUNS.contains(&nick.to_ascii_lowercase().as_str()),
        "{line}"
    );
    assert_eq!(rest, "class=auto fleet=out\n", "no fleet key, no node");

    let again = report(
        root.path(),
        false,
        || -> io::Result<Option<String>> { unreachable!("no roster read for a name already made") },
        NOW,
    )
    .expect("reports");
    assert_eq!(again, (line, None));
}

#[test]
fn a_roster_that_cannot_be_read_makes_no_name() {
    let root = state(0o700);
    let busy = || -> io::Result<Option<String>> { Err(io::Error::other("the roster is busy")) };
    let refused = report(root.path(), false, busy, NOW).expect_err("an unread roster");
    assert!(refused.contains("busy"), "{refused}");
    assert_eq!(
        file::read(root.path()).expect("reads"),
        None,
        "nothing kept"
    );
}

#[test]
fn the_report_names_the_fleet_the_node_and_a_migration_not_finished() {
    let root = state(0o700);
    let me = node_identity(root.path()).expect("a node");
    let key = format!("{}\n", "ab".repeat(32));
    write_private(root.path(), KEY_FILE, key.as_bytes()).expect("writes a fleet key");

    let (json, _) = report(root.path(), true, || Ok(Some("m8".to_owned())), NOW).expect("reports");
    let name = file::read(root.path())
        .expect("reads")
        .expect("made by the report");
    let (nick, shown) = (name.nick.as_str(), name.nick.display());
    let node = me.node();
    assert_eq!(
        json,
        format!(
            "{{\"class\":\"seeded\",\"fleet\":true,\"made\":\"evidence\",\"migration\":\"pending\",\
             \"name\":\"m8\",\"nick\":\"{nick}\",\"nick_class\":\"auto\",\"node\":\"{node}\",\
             \"number\":8,\"rev\":0,\"since\":{IMPLIED_SINCE},\"title\":\"m8 {shown}\"}}\n"
        ),
        "the roster's NAME at the implied rank, pending migration"
    );
    let (line, _) = report(root.path(), false, no_roster, NOW).expect("reports");
    assert_eq!(
        line,
        format!("m8 {shown}  class=seeded fleet=in migration=pending node={node}\n")
    );
    let (json, _) = report(root.path(), true, no_roster, NOW).expect("reports");
    assert!(json.contains("\"made\":\"kept\""), "{json}");

    let fresh = state(0o700);
    let (json, _) = report(fresh.path(), true, no_roster, NOW).expect("reports");
    for field in [
        "\"made\":\"drawn\"",
        "\"migration\":null",
        "\"name\":\"m0\"",
        "\"node\":null",
        "\"number\":null",
        "\"fleet\":false",
    ] {
        assert!(json.contains(field), "{field} in {json}");
    }
}

#[test]
fn a_first_read_brings_back_this_nodes_own_record() {
    let root = state(0o700);
    let me = node_identity(root.path()).expect("a node");
    write_private(
        root.path(),
        PEERS_FILE,
        entry(&me, &record(&me, Some(9), 3, Some("Heron"))).as_bytes(),
    )
    .expect("writes the book");
    let unread = || -> io::Result<Option<String>> { unreachable!("Floor names the claim") };
    let (json, _) = report(root.path(), true, unread, NOW).expect("reports");
    for field in [
        "\"made\":\"floor\"",
        "\"title\":\"m9 Heron\"",
        "\"class\":\"chosen\"",
        "\"rev\":3",
    ] {
        assert!(json.contains(field), "{field} in {json}");
    }
    let (line, _) = report(root.path(), false, no_roster, NOW).expect("reports");
    assert_eq!(
        line,
        format!("m9 Heron  class=chosen fleet=out node={}\n", me.node())
    );
}

#[test]
fn a_state_directory_open_to_others_is_narrowed_before_the_name_is_made() {
    let root = state(0o755);
    let (_, narrowed) = report(root.path(), false, no_roster, NOW).expect("reports");
    assert_eq!(narrowed, Some(0o755));
    let mode = std::fs::metadata(root.path())
        .expect("stats")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o700);
    assert!(
        file::read(root.path()).expect("reads").is_some(),
        "then made"
    );
}

fn unread() -> io::Result<Option<String>> {
    panic!("a name already made reads no roster")
}

fn nickname(word: &str) -> aterm_types::machine_name::Nickname {
    aterm_types::machine_name::Nickname::parse(word).expect("a nickname")
}

#[test]
fn nick_takes_one_word_or_random_with_dry_run_and_json() {
    use super::Nick;
    assert_eq!(
        parse(&args(&["nick", "Blue Heron"])),
        Ok(Mode::Nick {
            to: Nick::Word(nickname("Blue Heron")),
            dry_run: false,
            json: false
        })
    );
    assert_eq!(
        parse(&args(&["nick", "--dry-run", "--random", "--json"])),
        Ok(Mode::Nick {
            to: Nick::Random,
            dry_run: true,
            json: true
        })
    );
    assert_eq!(parse(&args(&["nick", "Heron", "--help"])), Ok(Mode::Help));
    assert_eq!(parse(&args(&["nick", "m8", "--help"])), Ok(Mode::Help));
    for refused in [
        &["nick"][..],
        &["nick", "m8"],
        &["nick", "Heron", "Otter"],
        &["nick", "Heron", "--random"],
        &["nick", "--clear"],
        &["nick", "Heron", "--json", "--json"],
        &["--json", "nick", "Heron"],
    ] {
        assert!(parse(&args(refused)).is_err(), "{refused:?}");
        assert_eq!(main_entry(args(refused)), ExitCode::from(2), "{refused:?}");
    }
}

#[test]
fn the_caller_is_an_agent_whose_session_status_names_an_agent_program() {
    use aterm_net::fleet_member::name_record::ByKind;
    for (status, kind) in [
        (
            Some("OK schema=1 sid=3 program=claude agent=busy\n"),
            ByKind::Agent,
        ),
        (Some("OK schema=1 sid=3 program=codex\n"), ByKind::Agent),
        (
            Some("OK schema=1 sid=3 program=zsh agent=-\n"),
            ByKind::Person,
        ),
        (
            Some("OK schema=1 sid=3 program=node agent=idle\n"),
            ByKind::Agent,
        ),
        (Some("OK schema=1 sid=3 program=-\n"), ByKind::Person),
        (Some("ERR no such session\n"), ByKind::Person),
        (None, ByKind::Person),
    ] {
        assert_eq!(super::caller(status), kind, "{status:?}");
    }
}

#[test]
fn nick_sets_a_chosen_nickname_raising_rev_and_keeping_the_claim() {
    use super::unix::{Note, renick};
    use crate::machine_name::NickTo;
    use aterm_net::fleet_member::name_record::{ByKind, NickClass, node_word};
    let root = state(0o700);
    let heron = NickTo::Word(nickname("Blue Heron"));
    let (line, notes) = renick(
        root.path(),
        &heron,
        ByKind::Person,
        false,
        false,
        no_roster,
        NOW,
    )
    .expect("sets it");
    assert!(
        line.starts_with("m0 Blue Heron  class=auto fleet=out node="),
        "{line}"
    );
    assert!(notes.is_empty(), "{notes:?}");
    let set = file::read(root.path()).expect("reads").expect("a name");
    assert_eq!(
        (set.nick.as_str(), set.nick_class, set.rev),
        ("Blue Heron", NickClass::Chosen, 0),
        "a file no record seeded stays below every record"
    );
    let by = set.by.expect("who set it");
    let me = aterm_net::fleet_member::read_node_identity(root.path())
        .expect("reads")
        .expect("a node minted for `by`");
    assert_eq!(
        (node_word(by.node), by.kind),
        (me.node().to_owned(), ByKind::Person)
    );

    let (_, notes) = renick(
        root.path(),
        &heron,
        ByKind::Agent,
        false,
        false,
        unread,
        NOW,
    )
    .expect("sets it again");
    assert_eq!(notes, [Note::Already("Blue Heron".to_owned())]);
    assert_eq!(
        file::read(root.path()).expect("reads"),
        Some(set.clone()),
        "no change, no rev"
    );

    let (json, notes) = renick(
        root.path(),
        &NickTo::Random,
        ByKind::Agent,
        true,
        false,
        unread,
        NOW,
    )
    .expect("draws");
    assert!(notes.is_empty(), "{notes:?}");
    assert!(json.contains("\"chosen\""), "{json}");
    let drawn = file::read(root.path()).expect("reads").expect("a name");
    assert!(nouns::NOUNS.contains(&drawn.nick.as_str()), "{drawn:?}");
    assert_eq!(
        (drawn.nick_class, drawn.rev, drawn.by.map(|by| by.kind)),
        (NickClass::Chosen, 0, Some(ByKind::Agent))
    );
    assert_eq!(
        (drawn.num, drawn.class, drawn.since, drawn.act),
        (set.num, set.class, set.since, set.act),
        "the claim as it was"
    );

    let before = std::fs::read(root.path().join(file::FILE)).expect("reads");
    let (line, notes) = renick(
        root.path(),
        &heron,
        ByKind::Person,
        false,
        true,
        unread,
        NOW,
    )
    .expect("a dry run");
    assert!(line.starts_with("m0 Blue Heron  "), "{line}");
    assert_eq!(notes, [Note::DryRun]);
    assert_eq!(
        std::fs::read(root.path().join(file::FILE)).expect("reads"),
        before,
        "nothing written"
    );
}

#[test]
fn a_nickname_another_machine_in_view_carries_is_set_and_said() {
    use super::unix::{Note, renick};
    use crate::machine_name::NickTo;
    use aterm_net::fleet::NodeIdentity;
    use aterm_net::fleet_member::name_record::ByKind;
    let root = state(0o700);
    node_identity(root.path()).expect("a node");
    let other = NodeIdentity::from_seed(&"b2".repeat(16), &[0x22; 32]).expect("an identity");
    write_private(
        root.path(),
        PEERS_FILE,
        entry(&other, &record(&other, Some(11), 4, Some("Puffin"))).as_bytes(),
    )
    .expect("writes the book");
    let (line, notes) = renick(
        root.path(),
        &NickTo::Word(nickname("Puffin")),
        ByKind::Person,
        false,
        false,
        no_roster,
        NOW,
    )
    .expect("sets it");
    assert!(line.starts_with("m0 Puffin  "), "{line}");
    assert_eq!(notes, [Note::Shared("Puffin".to_owned())]);
}
