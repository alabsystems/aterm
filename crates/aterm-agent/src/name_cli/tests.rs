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

/// The vault of a scratch home under `root`, which holds none: not in a fleet.
fn vault(root: &std::path::Path) -> aterm_secrets::client::Paths {
    aterm_secrets::client::Paths::locate_for_home(&root.join("home"))
}

#[test]
fn a_registry_origin_is_named_in_the_json_report() {
    use crate::machine_name::{InFleet, Origin};
    let mut name = file::MachineName::unnumbered(
        aterm_types::machine_name::Nickname::parse("MissBunny").expect("a nickname"),
        NOW,
    );
    name.num = aterm_types::machine_name::Number::new(86);
    name.class = aterm_types::machine_name::Class::Seeded;
    name.since = IMPLIED_SINCE;
    let shown = super::unix::Shown {
        name: &name,
        origin: Origin::Registry,
        fleet: InFleet::Out,
        node: None,
    };
    let json: aterm_json::Value = aterm_json::from_str(&shown.json()).expect("JSON");
    assert_eq!(json["made"].as_str(), Some("registry"));
    assert_eq!(json["title"].as_str(), Some("m86 Miss Bunny"));
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
    assert_eq!(parse(&args(&["sync"])), Ok(Mode::Sync { json: false }));
    assert_eq!(
        parse(&args(&["sync", "--json"])),
        Ok(Mode::Sync { json: true })
    );
    assert_eq!(parse(&args(&["sync", "--help"])), Ok(Mode::Help));
    for refused in [
        &["--json", "--json"][..],
        &["set", "-1"],
        &["--jsonx"],
        &["json"],
        &["sync", "85"],
        &["sync", "--nick", "dog"],
        &["sync", "--dry-run"],
        &["sync", "--json", "--json"],
    ] {
        assert!(parse(&args(refused)).is_err(), "{refused:?}");
        assert_eq!(
            main_entry(args(refused), || panic!("no sync for invalid arguments")),
            ExitCode::from(2),
            "{refused:?}"
        );
    }
    assert!(
        parse(&[OsString::from_vec(vec![0xff])]).is_err(),
        "not UTF-8"
    );
    assert_eq!(
        main_entry(args(&["--help"]), || panic!("help changes no names")),
        ExitCode::from(0)
    );
    let calls = Cell::new(0);
    assert_eq!(
        main_entry(args(&["sync", "--json"]), || {
            calls.set(calls.get() + 1);
            Ok(Some("macOS names match m85 Dog / m85-dog".into()))
        }),
        ExitCode::from(0)
    );
    assert_eq!(
        calls.get(),
        1,
        "sync dispatches the saved-intent adapter once"
    );
    for result in [
        Ok(None),
        Err("existing authorization refused".into()),
        Err("macOS retains a numeric collision alias".into()),
    ] {
        let (text, code) = super::sync_report(result, true);
        assert_eq!(code, 1);
        let report: aterm_json::Value = aterm_json::from_str(&text).unwrap();
        assert_eq!(report["status"].as_str(), Some("pending"));
        assert!(
            report["message"]
                .as_str()
                .is_some_and(|why| !why.is_empty())
        );
    }
    for result in [Ok(None), Err("existing authorization refused".into())] {
        assert_eq!(main_entry(args(&["sync"]), || result), ExitCode::from(1));
    }
}

#[test]
fn set_takes_a_number_and_optional_exact_nickname_with_preview_flags() {
    use super::Name;
    use aterm_types::machine_name::Number;
    assert_eq!(
        parse(&args(&[
            "set",
            "86",
            "--nick",
            "MissBunny",
            "--dry-run",
            "--json"
        ])),
        Ok(Mode::Set {
            to: Name {
                num: Number::new(86).expect("a number"),
                nick: Some(nickname("MissBunny"))
            },
            dry_run: true,
            json: true,
        })
    );
    assert_eq!(
        parse(&args(&["set", "m86"])),
        Ok(Mode::Set {
            to: Name {
                num: Number::new(86).expect("a number"),
                nick: None
            },
            dry_run: false,
            json: false,
        })
    );
    assert_eq!(parse(&args(&["set", "m0", "--help"])), Ok(Mode::Help));
    for refused in [
        &["set"][..],
        &["set", "-1"],
        &["set", "m086"],
        &["set", "m1000000"],
        &["set", "m86", "m7"],
        &["set", "m86", "--nick"],
        &["set", "m86", "--nick", "m7"],
        &["set", "m86", "--nick", "Otter", "--nick", "Heron"],
        &["set", "m86", "--dry-run", "--dry-run"],
        &["set", "m86", "--json", "--json"],
        &["set", "m86", "--take"],
    ] {
        assert!(parse(&args(refused)).is_err(), "{refused:?}");
        assert_eq!(
            main_entry(args(refused), || panic!("set parsing calls no sync")),
            ExitCode::from(2),
            "{refused:?}"
        );
    }
}

#[test]
fn set_reports_the_exact_name_and_previews_without_changing_it() {
    use super::unix::{NameAsk, Note, rename};
    use crate::machine_name::NameTo;
    use aterm_net::fleet_member::name_record::ByKind;
    use aterm_types::machine_name::Number;
    let root = state(0o700);
    report(
        root.path(),
        Some(&vault(root.path())),
        false,
        || Ok(Some("m7".to_owned())),
        NOW,
    )
    .expect("an initial name");
    let before = file::read(root.path()).expect("reads");
    let to = NameTo {
        num: Number::new(86).expect("a number"),
        nick: Some(nickname("MissBunny")),
    };
    let (preview, notes) = rename(
        root.path(),
        Some(&vault(root.path())),
        &NameAsk {
            to: &to,
            kind: ByKind::Person,
            dry_run: true,
        },
        false,
        no_roster,
        NOW,
    )
    .expect("previews");
    assert!(
        preview.starts_with("m86 Miss Bunny  class=chosen fleet=out"),
        "{preview}"
    );
    assert_eq!(notes, [Note::NameDryRun]);
    assert_eq!(file::read(root.path()).expect("reads"), before);
    let (json, notes) = rename(
        root.path(),
        Some(&vault(root.path())),
        &NameAsk {
            to: &to,
            kind: ByKind::Person,
            dry_run: false,
        },
        true,
        no_roster,
        NOW,
    )
    .expect("renames");
    let json: aterm_json::Value = aterm_json::from_str(&json).expect("JSON");
    assert_eq!(json["name"].as_str(), Some("m86"));
    assert_eq!(json["nick"].as_str(), Some("Miss Bunny"));
    assert!(notes.is_empty());
    assert_eq!(
        file::read(root.path())
            .expect("reads")
            .expect("a name")
            .title(),
        "m86 Miss Bunny"
    );
}

#[test]
fn a_first_read_makes_the_name_and_a_later_one_reads_it_back_with_no_roster_read() {
    let root = state(0o700);
    let asked = Cell::new(0);
    let roster = || {
        asked.set(asked.get() + 1);
        Ok(None)
    };
    let (line, narrowed) =
        report(root.path(), Some(&vault(root.path())), false, roster, NOW).expect("reports");
    assert_eq!(
        (narrowed, asked.get()),
        (None, 1),
        "the roster read for the first name"
    );
    let (title, rest) = line.split_once("  ").expect("two spaces end the title");
    let nick = title
        .strip_prefix("unnumbered ")
        .expect("unnumbered while unnumbered");
    assert!(
        nouns::NOUNS.contains(&nick.to_ascii_lowercase().as_str()),
        "{line}"
    );
    assert_eq!(rest, "class=auto fleet=out\n", "no fleet key, no node");

    let again = report(
        root.path(),
        Some(&vault(root.path())),
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
    let refused = report(root.path(), Some(&vault(root.path())), false, busy, NOW)
        .expect_err("an unread roster");
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
    crate::fleet_vault::take_fixture(root.path(), Some(&vault(root.path()))).unwrap();

    let (json, _) = report(
        root.path(),
        Some(&vault(root.path())),
        true,
        || Ok(Some("m8".to_owned())),
        NOW,
    )
    .expect("reports");
    let name = file::read(root.path())
        .expect("reads")
        .expect("made by the report");
    let shown = name.nick.display();
    let node = me.node();
    assert_eq!(
        json,
        format!(
            "{{\"class\":\"seeded\",\"fleet\":\"in\",\"made\":\"evidence\",\"migration\":\"pending\",\
             \"name\":\"m8\",\"nick\":\"{shown}\",\"nick_class\":\"auto\",\"node\":\"{node}\",\
             \"number\":8,\"rev\":0,\"since\":{IMPLIED_SINCE},\"title\":\"m8 {shown}\"}}\n"
        ),
        "the roster's NAME at the implied rank, pending migration"
    );
    let (line, _) = report(
        root.path(),
        Some(&vault(root.path())),
        false,
        no_roster,
        NOW,
    )
    .expect("reports");
    assert_eq!(
        line,
        format!("m8 {shown}  class=seeded fleet=in migration=pending node={node}\n")
    );
    let (json, _) =
        report(root.path(), Some(&vault(root.path())), true, no_roster, NOW).expect("reports");
    assert!(json.contains("\"made\":\"kept\""), "{json}");

    let fresh = state(0o700);
    let (json, _) = report(
        fresh.path(),
        Some(&vault(fresh.path())),
        true,
        no_roster,
        NOW,
    )
    .expect("reports");
    for field in [
        "\"made\":\"drawn\"",
        "\"migration\":null",
        "\"name\":\"unnumbered\"",
        "\"node\":null",
        "\"number\":null",
        "\"fleet\":\"out\"",
    ] {
        assert!(json.contains(field), "{field} in {json}");
    }
}

#[test]
fn a_report_keeps_a_known_name_when_identity_is_damaged_but_refuses_uncertain_names() {
    use super::unix::{NickAsk, renick};
    use crate::machine_name::NickTo;
    use aterm_net::fleet_member::name_record::ByKind;
    use aterm_net::fleet_member::{NODE_FILE, NODE_KEY_FILE};
    use aterm_types::machine_name::{Class, Number};

    for damaged in [NODE_FILE, NODE_KEY_FILE] {
        let root = state(0o700);
        node_identity(root.path()).expect("a node");
        let mut known = file::MachineName::unnumbered(nickname("MissBunny"), NOW);
        known.num = Number::new(86);
        known.class = Class::Chosen;
        known.rev = 1;
        file::read_or_create(root.path(), || Ok(known.clone())).expect("a valid local name");
        write_private(root.path(), damaged, b"damaged-identity\n").expect("damages identity");
        let unread = || -> io::Result<Option<String>> { unreachable!("the name is already known") };
        let (json, _) = report(root.path(), None, true, unread, NOW).expect("reports the name");
        let json: aterm_json::Value = aterm_json::from_str(&json).expect("JSON");
        assert_eq!(json["title"].as_str(), Some("m86 Miss Bunny"));
        assert_eq!(json["number"].as_u64(), Some(86));
        assert_eq!(json["rev"].as_u64(), Some(1));
        assert!(json["node"].is_null(), "an unread identity is not a node");
        let (line, _) = report(root.path(), None, false, unread, NOW).expect("reports text");
        assert_eq!(line, "m86 Miss Bunny  class=chosen fleet=out\n");
        assert!(
            renick(
                root.path(),
                None,
                &NickAsk {
                    to: &NickTo::Word(nickname("Heron")),
                    kind: ByKind::Agent,
                    dry_run: false,
                },
                false,
                unread,
                NOW,
            )
            .is_err(),
            "an edit still requires its actor identity"
        );
        assert_eq!(
            file::read(root.path()).expect("reads the local file"),
            Some(known)
        );
        assert_eq!(
            std::fs::read(root.path().join(damaged)).expect("reads the damaged identity"),
            b"damaged-identity\n",
            "a report never replaces identity"
        );

        std::fs::remove_file(root.path().join(file::FILE)).expect("removes the name");
        assert!(report(root.path(), None, true, no_roster, NOW).is_err());
        assert_eq!(file::read(root.path()).expect("reads absent name"), None);
        write_private(root.path(), file::FILE, b"no valid name\n").expect("a corrupt name");
        assert!(report(root.path(), None, true, no_roster, NOW).is_err());
        assert_eq!(
            std::fs::read(root.path().join(file::FILE)).expect("reads the corrupt name"),
            b"no valid name\n",
            "a report never replaces a damaged name"
        );
    }
}

#[test]
fn a_state_directory_open_to_others_is_narrowed_before_the_name_is_made() {
    let root = state(0o755);
    let (_, narrowed) = report(
        root.path(),
        Some(&vault(root.path())),
        false,
        no_roster,
        NOW,
    )
    .expect("reports");
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
        assert_eq!(
            main_entry(args(refused), || panic!("nick parsing calls no sync")),
            ExitCode::from(2),
            "{refused:?}"
        );
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
    use super::unix::{NickAsk, Note, renick};
    use crate::machine_name::NickTo;
    use crate::machine_name::registry::{Written, with_door};
    use aterm_net::fleet_member::name_record::{ByKind, NickClass};
    let root = state(0o700);
    let heron = NickTo::Word(nickname("Blue Heron"));
    let ((line, notes), asked) = with_door(Written::Unbound, || {
        renick(
            root.path(),
            Some(&vault(root.path())),
            &NickAsk {
                to: &heron,
                kind: ByKind::Person,
                dry_run: false,
            },
            false,
            no_roster,
            NOW,
        )
        .expect("sets it")
    });
    assert!(
        line.starts_with("unnumbered Blue Heron  class=auto fleet=out node="),
        "{line}"
    );
    assert!(notes.is_empty(), "{notes:?}");
    let set = file::read(root.path()).expect("reads").expect("a name");
    assert_eq!(
        (set.nick.as_str(), set.nick_class, set.rev),
        ("Blue Heron", NickClass::Chosen, 1),
        "the first nickname edit advances the local revision"
    );
    assert_eq!(
        asked,
        [["nickname=Blue Heron", "by=person"]],
        "the row's write says a person asked"
    );

    let (_, notes) = renick(
        root.path(),
        Some(&vault(root.path())),
        &NickAsk {
            to: &NickTo::Word(nickname("blue-heron")),
            kind: ByKind::Agent,
            dry_run: false,
        },
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

    let ((json, notes), asked) = with_door(Written::Unbound, || {
        renick(
            root.path(),
            Some(&vault(root.path())),
            &NickAsk {
                to: &NickTo::Random,
                kind: ByKind::Agent,
                dry_run: false,
            },
            true,
            unread,
            NOW,
        )
        .expect("draws")
    });
    assert!(notes.is_empty(), "{notes:?}");
    assert!(json.contains("\"chosen\""), "{json}");
    let drawn = file::read(root.path()).expect("reads").expect("a name");
    assert!(nouns::NOUNS.contains(&drawn.nick.as_str()), "{drawn:?}");
    assert_eq!((drawn.nick_class, drawn.rev), (NickClass::Chosen, 2));
    assert_eq!(
        asked,
        [[
            format!("nickname={}", drawn.display_nick()),
            "by=program".to_owned()
        ]],
        "the row's write says a program asked"
    );
    assert_eq!(
        (drawn.num, drawn.class, drawn.since, drawn.act),
        (set.num, set.class, set.since, set.act),
        "the claim as it was"
    );

    let before = std::fs::read(root.path().join(file::FILE)).expect("reads");
    let (line, notes) = renick(
        root.path(),
        Some(&vault(root.path())),
        &NickAsk {
            to: &heron,
            kind: ByKind::Person,
            dry_run: true,
        },
        false,
        unread,
        NOW,
    )
    .expect("a dry run");
    assert!(line.starts_with("unnumbered Blue Heron  "), "{line}");
    assert_eq!(notes, [Note::DryRun]);
    assert_eq!(
        std::fs::read(root.path().join(file::FILE)).expect("reads"),
        before,
        "nothing written"
    );
}

#[test]
fn a_name_another_machine_in_view_carries_is_refused() {
    use super::unix::{NickAsk, renick};
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
    let refused = renick(
        root.path(),
        Some(&vault(root.path())),
        &NickAsk {
            to: &NickTo::Word(nickname(" pUfFiN ")),
            kind: ByKind::Person,
            dry_run: false,
        },
        false,
        no_roster,
        NOW,
    )
    .expect_err("another machine holds this name");
    assert_eq!(
        crate::machine_name::Refused::of(&refused).map(|refused| &refused.cause),
        Some(&crate::machine_name::Cause::Nickname),
        "{refused}"
    );
    let before = crate::machine_name::file::read(root.path())
        .unwrap()
        .unwrap();
    let refused = crate::machine_name::set_name(
        root.path(),
        &crate::machine_name::NameTo {
            num: aterm_types::machine_name::Number::new(0).unwrap(),
            nick: Some(nickname("PUFFIN")),
        },
        || Ok(aterm_secrets::client::LabLabels::default()),
        ByKind::Person,
        no_roster,
        NOW,
        false,
    )
    .expect_err("number and name must change atomically");
    assert_eq!(
        crate::machine_name::Refused::of(&refused).map(|refused| &refused.cause),
        Some(&crate::machine_name::Cause::Nickname),
        "{refused}"
    );
    assert_eq!(
        crate::machine_name::file::read(root.path()).unwrap(),
        Some(before)
    );
}

#[test]
fn undo_and_history_take_their_flags_and_refuse_every_other_word() {
    assert_eq!(
        parse(&args(&["undo"])),
        Ok(Mode::Undo {
            dry_run: false,
            json: false
        })
    );
    assert_eq!(
        parse(&args(&["undo", "--json", "--dry-run"])),
        Ok(Mode::Undo {
            dry_run: true,
            json: true
        })
    );
    assert_eq!(parse(&args(&["undo", "--help"])), Ok(Mode::Help));
    assert_eq!(
        parse(&args(&["history"])),
        Ok(Mode::History { json: false })
    );
    assert_eq!(
        parse(&args(&["history", "--json"])),
        Ok(Mode::History { json: true })
    );
    assert_eq!(parse(&args(&["history", "-h"])), Ok(Mode::Help));
    for refused in [
        &["undo", "m7"][..],
        &["undo", "--dry-run", "--dry-run"],
        &["undo", "--json", "--json"],
        &["undo", "--random"],
        &["history", "--dry-run"],
        &["history", "m7"],
        &["history", "--json", "--json"],
        &["--json", "undo"],
        &["--json", "history"],
    ] {
        assert!(parse(&args(refused)).is_err(), "{refused:?}");
        assert_eq!(
            main_entry(args(refused), || panic!(
                "undo and history parsing call no sync"
            )),
            ExitCode::from(2),
            "{refused:?}"
        );
    }
}

/// Every path under `root` with its bytes (`None` for a directory), so a test sees any write.
fn tree(root: &std::path::Path) -> std::collections::BTreeMap<std::path::PathBuf, Option<Vec<u8>>> {
    let mut found = std::collections::BTreeMap::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).expect("lists") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                dirs.push(path.clone());
                found.insert(path, None);
            } else {
                let bytes = std::fs::read(&path).expect("reads");
                found.insert(path, Some(bytes));
            }
        }
    }
    found
}

/// One owner write to this Mac's row: the nickname `to` over `from`, as History lists it.
fn nick_write(op: u8, from: &str, to: &str) -> crate::machine_name::history::NameWrite {
    use crate::machine_name::history::{NameCell, NameWrite};
    NameWrite {
        op: [op; 32],
        at_ms: u64::from(op) * 1_000,
        mac: "m8".to_owned(),
        kind: aterm_db::att::EntryKind::Explicit,
        reverts: None,
        carried: false,
        cells: vec![NameCell {
            cell: aterm_db::file::Renamed::Nickname,
            value: Some(to.to_owned()),
            stands: true,
            replaced: vec![Some(from.to_owned())],
            unseen: Vec::new(),
        }],
    }
}

/// This Mac's row, holding `writes`, newest first: only the newest write's values stand.
fn row_history(
    mut writes: Vec<crate::machine_name::history::NameWrite>,
) -> crate::machine_name::history::History {
    for older in writes.iter_mut().skip(1) {
        for cell in &mut older.cells {
            cell.stands = false;
        }
    }
    crate::machine_name::history::History {
        row: Some("mc-0123456789abcdef".to_owned()),
        pending: Vec::new(),
        writes,
    }
}

/// `aterm name undo --dry-run` reports the name the undo would give and changes nothing under
/// the state directory, no file added or rewritten; the control, the same undo for real, writes
/// that name and queues the nickname it gives back to the row, as a person's. An undo whose
/// History, read again under the naming locks, gives another undo (a later rename) is refused as
/// unseen, writing nothing, so the command takes back only the rename it read, and on scratch
/// state, where no registry row is this Mac's, every real undo is. Undoing the undo redoes the
/// rename. The renames come from `aterm name nick` on an unnumbered Mac, which publishes nothing.
/// NEGATIVE CONTROL: with `edit_name`'s `history::fence` removed, the undo over History holding
/// the later `Third` writes `Test`, and the tree differs.
#[test]
fn name_undo_dry_run_changes_nothing() {
    use super::unix::{NickAsk, Note, UndoAsk, renick, undo};
    use crate::machine_name::NickTo;
    use crate::machine_name::history::{undo_of, with_history};
    use crate::machine_name::registry::{Written, with_door};
    use aterm_net::fleet_member::name_record::ByKind;
    let root = state(0o700);
    for word in ["Test", "Other"] {
        renick(
            root.path(),
            Some(&vault(root.path())),
            &NickAsk {
                to: &NickTo::Word(nickname(word)),
                kind: ByKind::Person,
                dry_run: false,
            },
            false,
            no_roster,
            NOW,
        )
        .expect("a nickname");
    }
    let read = row_history(vec![nick_write(2, "Test", "Other")]);
    let rename = undo_of(&read).expect("an undo");
    let before = tree(root.path());
    let ask = |dry_run| UndoAsk {
        kind: ByKind::Person,
        dry_run,
        seen: &rename,
    };
    let third = row_history(vec![
        nick_write(3, "Other", "Third"),
        nick_write(2, "Test", "Other"),
    ]);
    for (row, case) in [
        (Some(third), "a later rename"),
        (None, "no registry row is a scratch state's"),
    ] {
        let unshown = || {
            undo(
                root.path(),
                Some(&vault(root.path())),
                &ask(false),
                false,
                unread,
                NOW + 1,
            )
        };
        let refused = match row {
            Some(row) => with_history(Ok(row), unshown),
            None => unshown(),
        }
        .expect_err(case);
        assert_eq!(
            crate::machine_name::Refused::of(&refused).map(|refused| &refused.cause),
            Some(&crate::machine_name::Cause::Unseen),
            "{case}: {refused}"
        );
        assert_eq!(
            tree(root.path()),
            before,
            "{case}: an unseen undo writes nothing"
        );
    }
    let (line, notes) = with_history(Ok(read.clone()), || {
        undo(
            root.path(),
            Some(&vault(root.path())),
            &ask(true),
            false,
            unread,
            NOW + 1,
        )
    })
    .expect("a dry run");
    assert!(line.starts_with("unnumbered Test  class=auto"), "{line}");
    assert_eq!(notes, [Note::NameDryRun]);
    assert_eq!(tree(root.path()), before, "nothing written");
    let (undone, asked) = with_door(Written::Unbound, || {
        with_history(Ok(read.clone()), || {
            undo(
                root.path(),
                Some(&vault(root.path())),
                &ask(false),
                true,
                unread,
                NOW + 1,
            )
        })
    });
    let (json, notes) = undone.expect("the control: the undo itself");
    assert!(notes.is_empty(), "{notes:?}");
    assert_ne!(tree(root.path()), before, "the control: the undo writes");
    assert_eq!(asked, [["nickname=Test", "by=person"]]);
    let json: aterm_json::Value = aterm_json::from_str(&json).expect("JSON");
    assert_eq!(json["title"].as_str(), Some("unnumbered Test"));
    assert_eq!(
        file::read(root.path())
            .expect("reads")
            .expect("a name")
            .nick
            .as_str(),
        "Test"
    );

    let undone = row_history(vec![
        nick_write(3, "Other", "Test"),
        nick_write(2, "Test", "Other"),
    ]);
    let redo = undo_of(&undone).expect("the undo is a rename");
    let (line, _) = with_history(Ok(undone), || {
        undo(
            root.path(),
            Some(&vault(root.path())),
            &UndoAsk {
                kind: ByKind::Person,
                dry_run: true,
                seen: &redo,
            },
            false,
            unread,
            NOW + 2,
        )
    })
    .expect("an undo of the undo");
    assert!(line.starts_with("unnumbered Other  "), "{line}");
}

/// `aterm name history` prints History's lines byte for byte ([`History::lines`], every write)
/// and `--json` its object ([`History::json`]), and makes no name: a state with no
/// `machine-name.toml` holds none after it. On scratch state, with no row this Mac's, it says
/// [`NO_ROW`] in one line and an object with no row.
/// NEGATIVE CONTROL: with `unix::history` reading the name first (`ensure_in_view`, as `aterm
/// name` does), the scratch state holds a `machine-name.toml` after it.
///
/// [`History::lines`]: crate::machine_name::history::History::lines
/// [`History::json`]: crate::machine_name::history::History::json
/// [`NO_ROW`]: crate::machine_name::history::NO_ROW
#[test]
fn history_prints_the_rows_writes_and_makes_no_name() {
    use super::unix::history;
    use crate::machine_name::history::{NO_ROW, with_history};
    let root = state(0o700);
    let read_by = || std::time::Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let before = tree(root.path());
    let read = row_history(vec![
        nick_write(3, "Other", "Test"),
        nick_write(2, "Test", "Other"),
    ]);
    let text =
        with_history(Ok(read.clone()), || history(root.path(), false, read_by())).expect("History");
    assert_eq!(
        text,
        read.lines(None)
            .into_iter()
            .map(|line| line + "\n")
            .collect::<String>(),
        "History's lines, byte for byte"
    );
    assert_eq!(text.lines().count(), 2, "{text}");
    assert!(
        text.contains("  m8  explicit  nickname Other -> Test"),
        "{text}"
    );
    let json =
        with_history(Ok(read.clone()), || history(root.path(), true, read_by())).expect("History");
    assert_eq!(
        aterm_json::from_str::<aterm_json::Value>(&json).expect("JSON"),
        read.json()
    );
    assert_eq!(
        with_history(Err("busy".to_owned()), || history(
            root.path(),
            false,
            read_by()
        )),
        Err("busy".to_owned())
    );
    assert_eq!(
        history(root.path(), false, read_by()),
        Ok(format!("{NO_ROW}\n"))
    );
    let json: aterm_json::Value =
        aterm_json::from_str(&history(root.path(), true, read_by()).expect("History"))
            .expect("JSON");
    assert!(json["row"].is_null(), "{json}");
    assert_eq!(json["writes"], aterm_json::json!([]));
    assert_eq!(tree(root.path()), before, "History makes no name");
}

/// `aterm name set`'s refusal of a number older builds hold names the next free numbers above
/// it ([`super::unix::next_free_note`]), in the words every surface shares, and each number it
/// names the set takes. Controls: the first number named renames for real, and an error that
/// carries no refusal names none.
#[test]
fn a_refused_set_names_the_free_numbers_the_set_takes() {
    use super::unix::{NameAsk, next_free_note, rename};
    use crate::machine_name::{NameTo, next_free_words};
    use aterm_net::fleet::NodeIdentity;
    use aterm_net::fleet_member::Peer;
    use aterm_net::fleet_member::name_record::ByKind;
    use aterm_types::machine_name::Number;
    let num = |n| Number::new(n).expect("a number");
    let root = state(0o700);
    let home = vault(root.path());
    let set = |n: u32, dry_run: bool| {
        rename(
            root.path(),
            Some(&home),
            &NameAsk {
                to: &NameTo {
                    num: num(n),
                    nick: None,
                },
                kind: ByKind::Person,
                dry_run,
            },
            false,
            no_roster,
            NOW,
        )
    };
    set(5, false).expect("this Mac takes m5");
    let older = |seed: u8, n: u32| {
        let other = NodeIdentity::from_seed(&format!("{seed:02x}").repeat(16), &[seed; 32])
            .expect("an identity");
        let name = num(n).to_string();
        Peer::own(&other, (&format!("{name}.local"), 7485, "0.160.0"), NOW)
            .with_primary_name(&other, Some(&name))
            .line()
            + "\n"
    };
    write_private(
        root.path(),
        PEERS_FILE,
        (older(0xb2, 7) + &older(0xc3, 8)).as_bytes(),
    )
    .expect("older builds hold m7 and m8");
    let held = set(7, false).expect_err("an older build holds m7");
    let named = file::read(root.path()).expect("reads");
    let note = next_free_note(root.path(), Some(&home), num(7), &held, no_roster, NOW);
    assert_eq!(note, next_free_words(&[num(9), num(10), num(11)]), "{held}");
    assert_eq!(
        file::read(root.path()).expect("reads"),
        named,
        "no name changed"
    );
    for free in [9, 10, 11] {
        set(free, true).expect("the set takes each number named");
    }
    set(9, false).expect("the control: m9 renames");
    assert_eq!(
        file::read(root.path())
            .expect("reads")
            .and_then(|name| name.num),
        Some(num(9))
    );
    assert_eq!(
        next_free_note(
            root.path(),
            Some(&home),
            num(7),
            &io::Error::other("busy"),
            no_roster,
            NOW
        ),
        None,
        "the control: no refusal, no numbers"
    );
}
