// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::{Cell, RefCell};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use aterm_net::fleet_member::name_record::ByKind;
use aterm_net::fleet_member::read_primary_name;
use aterm_secrets::client::LabLabels;
use aterm_types::machine_name::{Class, Nickname, Number};

use super::*;
use crate::machine_name::tests::NOW;
use crate::machine_name::{NameTo, set_name};

fn num(n: u32) -> Number {
    Number::new(n).expect("a number")
}

fn names(words: [Option<&str>; 3]) -> [Option<String>; 3] {
    words.map(|word| word.map(str::to_owned))
}

/// A state directory one level inside its own scratch folder, so no vault sits beside it
/// (`crate::fleet_vault::vault_paths_for` reads the state's parent), with its aterm.toml in that
/// folder.
struct Scratch {
    _root: aterm_tempfile::TempDir,
    state: PathBuf,
    config: PathBuf,
}

fn fresh() -> Scratch {
    let root = aterm_tempfile::tempdir().expect("a tempdir");
    let state = root.path().join("state");
    std::fs::create_dir(&state).expect("a state directory");
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))
        .expect("a private state directory");
    let config = root.path().join("aterm.toml");
    Scratch {
        _root: root,
        state,
        config,
    }
}

/// `n` chosen in `state`, as `aterm name set` chooses it.
fn chosen(state: &Path, n: u32) -> MachineName {
    set_name(
        state,
        &NameTo {
            num: num(n),
            nick: Some(Nickname::parse("Heron").expect("a nickname")),
        },
        || Ok(LabLabels::default()),
        ByKind::Person,
        || Ok(None),
        NOW,
        false,
    )
    .expect("sets the number")
    .name
}

fn config_text(scratch: &Scratch) -> Option<String> {
    std::fs::read_to_string(&scratch.config).ok()
}

/// macOS's names as a test arm: what they read, what an apply leaves them reading, the apply's
/// answer, and what was asked of them, in order (`read`, `apply m59`).
struct FakeNames<'a> {
    names: RefCell<[Option<String>; 3]>,
    after: Option<[Option<String>; 3]>,
    answer: Result<(), String>,
    log: RefCell<Vec<String>>,
    /// Run inside each read before it answers.
    on_read: &'a dyn Fn(),
    /// Run inside each apply before it answers.
    on_apply: &'a dyn Fn(),
}

impl<'a> FakeNames<'a> {
    fn new(names: [Option<String>; 3], on_apply: &'a dyn Fn()) -> Self {
        Self {
            names: RefCell::new(names),
            after: None,
            answer: Ok(()),
            log: RefCell::new(Vec::new()),
            on_read: &nothing,
            on_apply,
        }
    }

    fn applied(&self) -> Vec<String> {
        self.log
            .borrow()
            .iter()
            .filter(|step| step.starts_with("apply"))
            .cloned()
            .collect()
    }
}

impl SystemNames for FakeNames<'_> {
    fn read(&self) -> [Option<String>; 3] {
        self.log.borrow_mut().push("read".to_owned());
        (self.on_read)();
        self.names.borrow().clone()
    }

    fn apply(&self, _: &Path, _: &Path, names: &GeneratedNames) -> Result<(), String> {
        self.log
            .borrow_mut()
            .push(format!("apply {}", names.number()));
        (self.on_apply)();
        if let Some(after) = &self.after {
            *self.names.borrow_mut() = after.clone();
        }
        self.answer.clone()
    }
}

fn nothing() {}

/// Why an isolated instance is not its account's own, in `not_own_account`'s words.
const PRIVATE: &str = "it runs under a private state root, as an isolated aterm instance does";

/// The scope of the account's own shell.
fn own() -> Scope {
    Scope {
        not_own: None,
        root: false,
        account_home: None,
        home: None,
    }
}

/// The scope of a process under a private state root whose account's home, and `HOME`, is
/// `home`.
fn private_under(home: &Path) -> Scope {
    Scope {
        not_own: Some(PRIVATE),
        root: false,
        account_home: Some(home.to_path_buf()),
        home: Some(home.to_path_buf()),
    }
}

/// The fleet step for `n`, then a set of `next`: one that lands just after the fleet name is
/// written.
fn fleet_then_set(scratch: &Scratch, n: u32, next: u32) -> impl FnOnce() -> Result<(), String> {
    move || {
        let written =
            crate::fleet_member::choose_machine_name_at(&scratch.state, &scratch.config, num(n));
        chosen(&scratch.state, next);
        written.map(|_| ())
    }
}

#[test]
fn the_policy_names_every_reader_and_keeps_other_settings() {
    let before = "# keep this comment\n[ai]\nmachine = \"m158\"\n[font]\nsize = 14\n";
    let after = policy_text(before, "m59").expect("a policy");
    let parsed: aterm_toml::Table = aterm_toml::from_str(&after).expect("TOML");
    for (section, key) in [
        ("identity", "name"),
        ("ai", "machine"),
        ("fleet", "machine"),
    ] {
        assert_eq!(parsed[section][key].as_str(), Some("m59"));
    }
    assert_eq!(parsed["font"]["size"].as_integer(), Some(14));
    assert!(after.contains("# keep this comment"));
    assert_eq!(
        policy_text("identity = 3\n", "m59"),
        Err("aterm.toml identity is not a table".to_owned())
    );
    assert!(
        policy_text("[ai\n", "m59")
            .expect_err("invalid")
            .starts_with("invalid aterm.toml: ")
    );
}

#[test]
fn the_policy_is_written_only_over_its_own_snapshot_and_number() {
    let scratch = fresh();
    crate::machine_name::ensure(&scratch.state, || Ok(Some("m59".into())), |_| false, NOW)
        .expect("a seeded m59");
    assert!(holds(&scratch.state, num(59)).expect("reads"));
    assert!(!holds(&scratch.state, num(60)).expect("reads"));
    std::fs::write(&scratch.config, "[ai]\nmachine = \"m158\"\n").expect("writes");
    let stale = Policy::read(&scratch.config, num(59)).expect("a policy");
    std::fs::write(&scratch.config, "[font]\nsize = 19\n").expect("an edit after the read");
    let refused = write_policy(&scratch.state, &stale).expect_err("a concurrent edit");
    assert_eq!(
        refused.to_string(),
        "aterm.toml changed while this was editing it; run again"
    );
    assert_eq!(
        config_text(&scratch).as_deref(),
        Some("[font]\nsize = 19\n")
    );
    let other = Policy::read(&scratch.config, num(60)).expect("a policy");
    let refused = write_policy(&scratch.state, &other).expect_err("another number");
    assert_eq!(refused.to_string(), SUPERSEDED);
    assert_eq!(
        config_text(&scratch).as_deref(),
        Some("[font]\nsize = 19\n")
    );
    let fresh = Policy::read(&scratch.config, num(59)).expect("a policy");
    write_policy(&scratch.state, &fresh).expect("writes");
    assert_eq!(
        config_text(&scratch),
        Some(policy_text("[font]\nsize = 19\n", "m59").expect("a policy"))
    );
}

#[test]
fn a_superseded_name_never_signs_a_membership_label() {
    let scratch = fresh();
    chosen(&scratch.state, 59);
    let policy = Policy::read(&scratch.config, num(59)).unwrap();
    write_policy(&scratch.state, &policy).unwrap();
    let signed = Cell::new(0);
    let sign = || {
        signed.set(signed.get() + 1);
        Ok(())
    };
    publish_log_label(&scratch.state, &scratch.config, num(59), sign).unwrap();
    assert_eq!(signed.get(), 1);
    chosen(&scratch.state, 60);
    assert!(publish_log_label(&scratch.state, &scratch.config, num(59), sign).is_err());
    assert_eq!(
        signed.get(),
        1,
        "a superseded publication authored no record"
    );
}

#[test]
fn a_naming_hold_refuses_the_policy_read_that_comes_before_any_set() {
    let scratch = fresh();
    assert!(
        Policy::read(&scratch.config, num(59)).is_ok(),
        "no aterm.toml reads as an empty one"
    );
    std::fs::write(&scratch.config, "[identity]\nname = false\n").expect("writes");
    assert_eq!(
        Policy::read(&scratch.config, num(59)).err(),
        Some(HELD_OFF.to_owned())
    );
}

#[test]
fn publish_writes_the_policy_then_the_fleet_name_then_asks_macos_once() {
    let scratch = fresh();
    let name = chosen(&scratch.state, 59);
    let policy = Policy::read(&scratch.config, num(59)).expect("a policy");
    let seen = RefCell::new(None);
    let during = || {
        *seen.borrow_mut() = Some((
            config_text(&scratch),
            read_primary_name(&scratch.state).expect("reads the fleet name"),
        ));
    };
    let mut fake = FakeNames::new(names([Some("m184"), Some("m184"), None]), &during);
    fake.after = Some(names([
        Some("m59 Heron"),
        Some("m59-heron"),
        Some("m59-heron"),
    ]));
    assert_eq!(
        publish(&scratch.state, &scratch.config, &policy, Some(&fake)),
        Ok(())
    );
    assert_eq!(*fake.log.borrow(), ["read", "apply m59", "read"]);
    assert_eq!(
        seen.into_inner(),
        Some((
            Some(policy_text("", "m59").expect("a policy")),
            Some("m59".to_owned())
        )),
        "aterm.toml and the fleet name read m59 before macOS is asked"
    );
    assert_eq!(
        publication(
            &scratch.state,
            &scratch.config,
            &name,
            Some(&names([
                Some("m59 Heron"),
                Some("m59-heron"),
                Some("m59-heron")
            ]))
        ),
        Ok(Publication::Current)
    );
}

#[test]
fn names_macos_already_reads_ask_for_nothing() {
    let scratch = fresh();
    chosen(&scratch.state, 59);
    let policy = Policy::read(&scratch.config, num(59)).expect("a policy");
    let fake = FakeNames::new(
        names([Some("m59 Heron"), Some("m59-heron-2"), Some("m59-heron")]),
        &nothing,
    );
    assert!(matches!(
        publish(&scratch.state, &scratch.config, &policy, Some(&fake)),
        Err(Pending::SystemNames(_))
    ));
    assert_eq!(*fake.log.borrow(), ["read"], "no apply");
    assert_eq!(
        read_primary_name(&scratch.state).expect("reads"),
        Some("m59".to_owned())
    );
    let saved = file::read(&scratch.state).unwrap();
    let fake = FakeNames::new(
        names([Some("m59 Heron (2)"), Some("m59-heron"), Some("m59-heron")]),
        &nothing,
    );
    for _ in 0..2 {
        let policy = Policy::read(&scratch.config, num(59)).unwrap();
        assert!(matches!(
            publish(&scratch.state, &scratch.config, &policy, Some(&fake)),
            Err(Pending::SystemNames(_))
        ));
    }
    assert_eq!(*fake.log.borrow(), ["read", "read"], "no alias setter");
    assert_eq!(file::read(&scratch.state).unwrap(), saved);
    assert!(matches!(
        publication(
            &scratch.state,
            &scratch.config,
            saved.as_ref().unwrap(),
            Some(&fake.names.borrow())
        ),
        Ok(Publication::Owed(_))
    ));
}

#[test]
fn publish_stops_at_the_first_step_that_did_not_finish() {
    // The policy: aterm.toml edited after its read.
    let scratch = fresh();
    chosen(&scratch.state, 59);
    let policy = Policy::read(&scratch.config, num(59)).expect("a policy");
    std::fs::write(&scratch.config, "[font]\nsize = 19\n").expect("an edit after the read");
    let fake = FakeNames::new(names([Some("m184"); 3]), &nothing);
    assert_eq!(
        publish(&scratch.state, &scratch.config, &policy, Some(&fake)),
        Err(Pending::Policy(
            "aterm.toml changed while this was editing it; run again".to_owned()
        ))
    );
    assert_eq!(read_primary_name(&scratch.state).expect("reads"), None);
    assert!(fake.log.borrow().is_empty(), "macOS not asked");

    // The fleet name: a `[[fleet.peers]]` entry of the aterm.toml at `config` answers to m59.
    let scratch = fresh();
    chosen(&scratch.state, 59);
    std::fs::write(
        &scratch.config,
        "[[fleet.peers]]\nhost = \"m59-mini.local\"\n",
    )
    .expect("writes");
    let policy = Policy::read(&scratch.config, num(59)).expect("a policy");
    let fake = FakeNames::new(names([Some("m184"); 3]), &nothing);
    assert_eq!(
        publish(&scratch.state, &scratch.config, &policy, Some(&fake)),
        Err(Pending::FleetName(
            "m59 already names m59-mini.local; use its node alias to disambiguate".to_owned()
        ))
    );
    assert_eq!(read_primary_name(&scratch.state).expect("reads"), None);
    assert!(fake.log.borrow().is_empty(), "macOS not asked");
    assert!(
        config_text(&scratch).is_some_and(|text| text.contains("machine = \"m59\"")),
        "the policy, the step before, was written"
    );

    // macOS's names: the existing noninteractive naming authorization refuses.
    let scratch = fresh();
    chosen(&scratch.state, 59);
    let policy = Policy::read(&scratch.config, num(59)).expect("a policy");
    let mut fake = FakeNames::new(names([Some("m184"); 3]), &nothing);
    fake.answer = Err("the existing macOS naming authorization refused the request".to_owned());
    assert_eq!(
        publish(&scratch.state, &scratch.config, &policy, Some(&fake)),
        Err(Pending::SystemNames(
            "the existing macOS naming authorization refused the request".to_owned()
        ))
    );
    assert_eq!(*fake.log.borrow(), ["read", "apply m59"]);
    assert_eq!(
        config_text(&scratch),
        Some(policy_text("", "m59").expect("a policy"))
    );
    assert_eq!(
        read_primary_name(&scratch.state).expect("reads"),
        Some("m59".to_owned())
    );
}

#[test]
fn a_request_a_newer_set_replaced_is_superseded_not_pending() {
    // A set of m60 between the policy's read and the publication of m59.
    let scratch = fresh();
    chosen(&scratch.state, 59);
    let policy = Policy::read(&scratch.config, num(59)).expect("a policy");
    chosen(&scratch.state, 60);
    let fake = FakeNames::new(names([Some("m184"); 3]), &nothing);
    assert_eq!(
        publish(&scratch.state, &scratch.config, &policy, Some(&fake)),
        Err(Pending::Superseded(SUPERSEDED.to_owned()))
    );
    assert_eq!(config_text(&scratch), None, "aterm.toml untouched");
    assert_eq!(read_primary_name(&scratch.state).expect("reads"), None);
    assert!(fake.log.borrow().is_empty(), "macOS not asked");

    // A set of m61 while macOS's names are being set to m60.
    let policy = Policy::read(&scratch.config, num(60)).expect("a policy");
    let during = || {
        chosen(&scratch.state, 61);
    };
    let mut fake = FakeNames::new(names([Some("m184"); 3]), &during);
    fake.after = Some(names([
        Some("m60 Heron"),
        Some("m60-heron"),
        Some("m60-heron"),
    ]));
    assert_eq!(
        publish(&scratch.state, &scratch.config, &policy, Some(&fake)),
        Err(Pending::Superseded(SUPERSEDED.to_owned()))
    );
    assert_eq!(fake.applied(), ["apply m60"]);

    // A set of m62 while an old OS request fails: current identity wins over that error.
    let policy = Policy::read(&scratch.config, num(61)).expect("a policy");
    let during = || {
        chosen(&scratch.state, 62);
    };
    let mut fake = FakeNames::new(names([Some("m184"); 3]), &during);
    fake.answer = Err("the existing macOS naming authorization refused the request".to_owned());
    assert!(matches!(
        publish(&scratch.state, &scratch.config, &policy, Some(&fake)),
        Err(Pending::Superseded(_))
    ));
    assert_eq!(
        crate::machine_name::file::read(&scratch.state)
            .expect("reads the newer choice")
            .expect("the newer choice remains")
            .num,
        Some(num(62)),
        "a failed older OS request must not roll back the newer canonical choice"
    );
    assert_eq!(
        *fake.log.borrow(),
        ["read", "apply m61"],
        "no later OS operation"
    );

    // With no system names to set (other Unix): m62, which the name file holds, is published.
    let policy = Policy::read(&scratch.config, num(62)).expect("a policy");
    assert_eq!(
        publish(&scratch.state, &scratch.config, &policy, None),
        Ok(())
    );

    // m62's policy once m63 holds: its write, the first step, refuses it.
    chosen(&scratch.state, 63);
    assert_eq!(
        publish(&scratch.state, &scratch.config, &policy, None),
        Err(Pending::Superseded(SUPERSEDED.to_owned()))
    );

    // A set of m64 while macOS's names, which already read m63, are read: nothing is asked of
    // them, and m63 is superseded, not published.
    let policy = Policy::read(&scratch.config, num(63)).expect("a policy");
    let on_read = || {
        chosen(&scratch.state, 64);
    };
    let mut fake = FakeNames::new(names([Some("m63"); 3]), &nothing);
    fake.on_read = &on_read;
    assert_eq!(
        publish(&scratch.state, &scratch.config, &policy, Some(&fake)),
        Err(Pending::Superseded(SUPERSEDED.to_owned()))
    );
    assert_eq!(*fake.log.borrow(), ["read"], "no apply");

    // The same actual read-time rename when the OS still differs must refuse before
    // applying the obsolete number, rather than detect supersession only afterward.
    let policy = Policy::read(&scratch.config, num(64)).expect("a policy");
    let on_read = || {
        chosen(&scratch.state, 65);
    };
    let mut fake = FakeNames::new(names([Some("m184"); 3]), &nothing);
    fake.on_read = &on_read;
    assert_eq!(
        publish(&scratch.state, &scratch.config, &policy, Some(&fake)),
        Err(Pending::Superseded(SUPERSEDED.to_owned()))
    );
    assert_eq!(
        *fake.log.borrow(),
        ["read"],
        "a superseded read must never reach apply"
    );
}

#[test]
fn a_set_just_after_the_fleet_name_ends_publication_before_macos_is_asked() {
    // With no system names to set (other Unix), the check after the fleet name is the last.
    let scratch = fresh();
    chosen(&scratch.state, 59);
    let policy = Policy::read(&scratch.config, num(59)).expect("a policy");
    assert_eq!(
        publish_through(
            &scratch.state,
            &scratch.config,
            &policy,
            &own(),
            None,
            fleet_then_set(&scratch, 59, 60)
        ),
        Err(Pending::Superseded(SUPERSEDED.to_owned()))
    );
    assert_eq!(
        read_primary_name(&scratch.state).expect("reads"),
        Some("m59".to_owned()),
        "the fleet step ran"
    );

    // On macOS, its names are neither read nor set for a number a newer set replaced.
    let policy = Policy::read(&scratch.config, num(60)).expect("a policy");
    let fake = FakeNames::new(names([Some("m184"); 3]), &nothing);
    assert_eq!(
        publish_through(
            &scratch.state,
            &scratch.config,
            &policy,
            &own(),
            Some(&fake),
            fleet_then_set(&scratch, 60, 61)
        ),
        Err(Pending::Superseded(SUPERSEDED.to_owned()))
    );
    assert!(fake.log.borrow().is_empty(), "macOS not asked");
}

#[test]
fn system_reads_reject_newer_canonical_names_and_naming_policy() {
    let scratch = fresh();
    chosen(&scratch.state, 59);
    let policy = Policy::read(&scratch.config, num(59)).unwrap();
    let reads = Cell::new(0);
    let on_read = || {
        reads.set(reads.get() + 1);
        if reads.get() == 2 {
            chosen(&scratch.state, 60);
        }
    };
    let mut fake = FakeNames::new(names([Some("m184"); 3]), &nothing);
    fake.after = Some(names([
        Some("m59 Heron"),
        Some("m59-heron"),
        Some("m59-heron"),
    ]));
    fake.on_read = &on_read;
    assert_eq!(
        publish(&scratch.state, &scratch.config, &policy, Some(&fake)),
        Err(Pending::Superseded(SUPERSEDED.to_owned()))
    );
    assert_eq!(
        fake.applied(),
        ["apply m59"],
        "the original apply was current when it began"
    );
    assert_eq!(reads.get(), 2);

    for at in [1, 2] {
        for (section, key, value, allowed) in [
            ("identity", "name", Value::from(false), false),
            ("identity", "name", Value::from("m60"), false),
            ("ai", "machine", Value::from("m60"), false),
            ("fleet", "machine", Value::from("m60"), false),
            ("font", "size", Value::from(19), true),
        ] {
            let scratch = fresh();
            chosen(&scratch.state, 59);
            let policy = Policy::read(&scratch.config, num(59)).unwrap();
            let reads = Cell::new(0);
            let on_read = || {
                reads.set(reads.get() + 1);
                if reads.get() == at {
                    let mut doc = parsed(&config_text(&scratch).unwrap()).unwrap();
                    let root = doc.as_table_mut();
                    if root.get(section).is_none() {
                        root.insert(section, Item::Table(Table::new()));
                    }
                    root.get_mut(section)
                        .unwrap()
                        .as_table_like_mut()
                        .unwrap()
                        .insert(key, Item::Value(value.clone()));
                    std::fs::write(&scratch.config, doc.to_string()).unwrap();
                }
            };
            let mut fake = FakeNames::new(names([Some("m184"); 3]), &nothing);
            fake.after = Some(names([
                Some("m59 Heron"),
                Some("m59-heron"),
                Some("m59-heron"),
            ]));
            fake.on_read = &on_read;
            let result = publish(&scratch.state, &scratch.config, &policy, Some(&fake));
            if allowed {
                assert_eq!(result, Ok(()), "unrelated config edits stay allowed");
            } else {
                assert!(
                    matches!(result, Err(Pending::Policy(_))),
                    "read {at}, {section}.{key}: {result:?}"
                );
                assert_eq!(
                    fake.applied().is_empty(),
                    at == 1,
                    "a late hold prevents the next OS effect without undoing an earlier one"
                );
            }
            let current = parsed(&config_text(&scratch).unwrap()).unwrap();
            assert_eq!(
                current
                    .get(section)
                    .and_then(Item::as_table_like)
                    .and_then(|table| table.get(key))
                    .and_then(Item::as_value)
                    .map(|held| (held.as_bool(), held.as_str(), held.as_integer())),
                Some((value.as_bool(), value.as_str(), value.as_integer()))
            );
        }
    }
}

#[test]
fn publication_continues_only_while_the_nicknames_displayed_words_stay_the_same() {
    for (word, changes) in [("heron", false), ("He Ron", true)] {
        let scratch = fresh();
        chosen(&scratch.state, 59);
        let policy = Policy::read(&scratch.config, num(59)).unwrap();
        let edited = Cell::new(false);
        let on_read = || {
            if !edited.replace(true) {
                let set = crate::machine_name::set_nick(
                    &scratch.state,
                    &crate::machine_name::NickTo::Word(Nickname::parse(word).unwrap()),
                    ByKind::Person,
                    || Ok(None),
                    NOW + 1,
                    false,
                )
                .unwrap();
                assert_eq!(set.changed, changes);
            }
        };
        let mut fake = FakeNames::new(names([Some("m184"); 3]), &nothing);
        fake.after = Some(names([
            Some("m59 Heron"),
            Some("m59-heron"),
            Some("m59-heron"),
        ]));
        fake.on_read = &on_read;
        let result = publish(&scratch.state, &scratch.config, &policy, Some(&fake));
        if changes {
            assert!(matches!(result, Err(Pending::Superseded(_))));
            assert!(fake.applied().is_empty());
        } else {
            assert_eq!(result, Ok(()));
            assert_eq!(fake.applied(), ["apply m59"]);
        }
    }
}

#[test]
fn a_name_file_unread_after_the_apply_is_neither_superseded_nor_published() {
    let scratch = fresh();
    chosen(&scratch.state, 59);
    let policy = Policy::read(&scratch.config, num(59)).expect("a policy");
    let during = || {
        aterm_uds::private_file::write_private(&scratch.state, file::FILE, b"no valid name\n")
            .expect("damages the name file");
    };
    let mut fake = FakeNames::new(names([Some("m184"); 3]), &during);
    fake.after = Some(names([
        Some("m59 Heron"),
        Some("m59-heron"),
        Some("m59-heron"),
    ]));
    let pending = publish(&scratch.state, &scratch.config, &policy, Some(&fake))
        .expect_err("cannot say the request still holds");
    assert!(
        matches!(&pending, Pending::NameUnread(why) if why.starts_with(file::FILE)),
        "{pending:?}"
    );
}

#[test]
fn names_macos_did_not_keep_are_pending() {
    let scratch = fresh();
    chosen(&scratch.state, 59);
    let policy = Policy::read(&scratch.config, num(59)).expect("a policy");
    let fake = FakeNames::new(
        names([Some("m184"), Some("m59-heron"), Some("m59-heron")]),
        &nothing,
    );
    assert_eq!(
        publish(&scratch.state, &scratch.config, &policy, Some(&fake)),
        Err(Pending::SystemNames(
            "macOS did not retain all three system names".to_owned()
        ))
    );
    assert_eq!(*fake.log.borrow(), ["read", "apply m59", "read"]);
}

#[test]
fn publication_says_what_a_chosen_number_still_owes() {
    let mut name = MachineName::unnumbered(Nickname::parse("Heron").expect("a nickname"), NOW);
    name.num = Some(num(59));
    name.class = Class::Chosen;
    let old = names([Some("m184"), Some("m184"), Some("m184")]);
    let all = Owed::Policy(vec!["[identity] name", "[ai] machine", "[fleet] machine"]);
    assert_eq!(
        publication_of(None, None, &name, Some(&old)),
        Ok(Publication::Owed(vec![
            all.clone(),
            Owed::FleetName(None),
            Owed::SystemNames(vec![
                ("ComputerName", Some("m184".to_owned())),
                ("LocalHostName", Some("m184".to_owned())),
                ("HostName", Some("m184".to_owned())),
            ]),
        ]))
    );
    let published = policy_text("[font]\nsize = 14\n", "m59").expect("a policy");
    let now = names([Some("m59 Heron"), Some("m59-heron"), Some("m59-heron")]);
    assert_eq!(
        publication_of(Some(&published), Some("m59"), &name, Some(&now)),
        Ok(Publication::Current)
    );
    assert_eq!(
        publication_of(
            Some("[ai]\nmachine = \"m59\"\n[fleet]\nmachine = \"m158\"\n"),
            Some("m158"),
            &name,
            Some(&names([Some("m59 Heron"), Some("m59-heron"), None]))
        ),
        Ok(Publication::Owed(vec![
            Owed::Policy(vec!["[identity] name", "[fleet] machine"]),
            Owed::FleetName(Some("m158".to_owned())),
            Owed::SystemNames(vec![("HostName", None)]),
        ]))
    );
    assert_eq!(
        publication_of(Some(&published), Some("m59"), &name, None),
        Ok(Publication::Current),
        "macOS's names not read are not judged"
    );
    assert_eq!(
        publication_of(None, None, &name, None),
        Ok(Publication::Owed(vec![all, Owed::FleetName(None)]))
    );

    for (class, migration, number) in [
        (Class::Seeded, None, Some(num(59))),
        (Class::Auto, None, Some(num(59))),
        (Class::Chosen, Some(Migration::Waiting), Some(num(59))),
        (Class::Chosen, None, None),
    ] {
        let mut other = name.clone();
        (other.class, other.migration, other.num) = (class, migration, number);
        assert_eq!(
            publication_of(None, None, &other, Some(&old)),
            Ok(Publication::Current),
            "{class:?} {migration:?} {number:?}"
        );
    }
    let mut pending = name.clone();
    pending.migration = Some(Migration::Pending);
    assert!(matches!(
        publication_of(None, None, &pending, None),
        Ok(Publication::Owed(_))
    ));

    for class in [Class::Auto, Class::Seeded, Class::Chosen] {
        let mut held = name.clone();
        held.class = class;
        assert_eq!(
            publication_of(Some("[identity]\nname = false\n"), None, &held, Some(&old)),
            Ok(Publication::Off(HELD_OFF.to_owned()))
        );
    }
    assert!(
        publication_of(Some("[ai\n"), None, &name, None)
            .expect_err("invalid")
            .starts_with("invalid aterm.toml: ")
    );

    // The stable fleet number and the descriptive OS names are one saved choice.
    let mut dog = name.clone();
    dog.num = Some(num(85));
    dog.nick = Nickname::parse("dog").expect("a nickname");
    let policy = policy_text("", "m85").expect("a policy");
    assert_eq!(
        publication_of(
            Some(&policy),
            Some("m85"),
            &dog,
            Some(&names([Some("m85 Dog"), Some("m85-dog"), Some("m85-dog"),])),
        ),
        Ok(Publication::Current),
        "the saved number and nickname generate all three OS names"
    );
    assert!(matches!(
        publication_of(
            Some(&policy),
            Some("m85"),
            &dog,
            Some(&names([
                Some("m85 Dog"),
                Some("m85-dog-2"),
                Some("m85-dog"),
            ])),
        ),
        Ok(Publication::Owed(_))
    ));
}

#[test]
fn what_is_owed_reads_as_a_phrase() {
    let m86 = num(86);
    assert_eq!(
        Owed::Policy(vec!["[identity] name", "[ai] machine"]).phrase(m86),
        "aterm.toml's [identity] name, [ai] machine not yet m86"
    );
    assert_eq!(
        Owed::FleetName(Some("m184".to_owned())).phrase(m86),
        "the fleet name is m184"
    );
    assert_eq!(Owed::FleetName(None).phrase(m86), "the fleet name is unset");
    assert_eq!(
        Owed::SystemNames(vec![
            ("ComputerName", Some("m184".to_owned())),
            ("HostName", None)
        ])
        .phrase(m86),
        "macOS's ComputerName is m184 and its HostName is unset"
    );
    assert_eq!(
        Owed::SystemNames(vec![
            ("ComputerName", Some("m184".to_owned())),
            ("LocalHostName", Some("m184".to_owned())),
            ("HostName", None)
        ])
        .phrase(m86),
        "macOS's ComputerName is m184, its LocalHostName is m184 and its HostName is unset"
    );
    assert_eq!(
        Owed::SystemNames(vec![("HostName", None)]).phrase(m86),
        "macOS's HostName is unset"
    );
}

#[test]
fn all_three_names_must_read_the_number() {
    assert!(all_read(
        &names([Some("m59"); 3]),
        &GeneratedNames::number_only(num(59))
    ));
    assert!(!all_read(
        &names([Some("m59"), Some("m59"), None]),
        &GeneratedNames::number_only(num(59))
    ));
    assert!(!all_read(
        &names([Some("m59"), Some("M59"), Some("m59")]),
        &GeneratedNames::number_only(num(59))
    ));
    assert!(!all_read(
        &names([Some("m59"); 3]),
        &GeneratedNames::number_only(num(5))
    ));
}

#[test]
fn pending_keeps_the_cli_s_words_and_names_its_step() {
    let superseded = Pending::Superseded(SUPERSEDED.to_owned());
    assert_eq!(
        superseded.why(),
        "a newer machine name replaced this request"
    );
    assert_eq!(superseded.phrase(), "a newer machine name replaced it");
    for (pending, phrase) in [
        (
            Pending::Policy("x".to_owned()),
            "aterm.toml does not name it yet (x)",
        ),
        (
            Pending::FleetName("x".to_owned()),
            "its fleet name is not published yet (x)",
        ),
        (
            Pending::SystemNames("x".to_owned()),
            "macOS names are not fully aligned (x)",
        ),
        (
            Pending::NameUnread("x".to_owned()),
            "its name could not be read back (x)",
        ),
        (
            Pending::NotOwnAccount("x".to_owned()),
            "this process publishes it nowhere (x)",
        ),
    ] {
        assert_eq!(pending.why(), "x");
        assert_eq!(pending.phrase(), phrase);
    }
}

#[test]
fn a_private_root_never_writes_config_under_the_account_home() {
    let scratch = fresh();
    chosen(&scratch.state, 59);
    let home = aterm_tempfile::tempdir().expect("an account home");
    let config = home.path().join(".config").join("aterm").join("aterm.toml");
    std::fs::create_dir_all(config.parent().expect("a folder")).expect("its folder");
    let kept = "# the account's own settings\n";
    std::fs::write(&config, kept).expect("the account's aterm.toml");
    let policy = Policy::read(&config, num(59)).expect("a policy");
    let fleet = Cell::new(0);
    let step = || {
        fleet.set(fleet.get() + 1);
        Ok(())
    };
    let refused = publish_through(
        &scratch.state,
        &config,
        &policy,
        &private_under(home.path()),
        None,
        step,
    );
    assert!(
        matches!(&refused, Err(Pending::NotOwnAccount(why)) if why.contains(PRIVATE)),
        "{refused:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&config).expect("reads"),
        kept,
        "the account's aterm.toml is left as it was"
    );
    assert_eq!(fleet.get(), 0, "no later step runs");

    // Control: the same process writes its own scratch aterm.toml outside the account's home.
    let policy = Policy::read(&scratch.config, num(59)).expect("a policy");
    assert_eq!(
        publish_through(
            &scratch.state,
            &scratch.config,
            &policy,
            &private_under(home.path()),
            None,
            step,
        ),
        Ok(())
    );
    assert_eq!(
        config_text(&scratch),
        Some(policy_text("", "m59").expect("a policy"))
    );
    assert_eq!(fleet.get(), 1);

    // Control: the account's own shell writes the aterm.toml under its home, as before.
    let policy = Policy::read(&config, num(59)).expect("a policy");
    let own_home = Scope {
        account_home: Some(home.path().to_path_buf()),
        ..own()
    };
    assert_eq!(
        publish_through(&scratch.state, &config, &policy, &own_home, None, step),
        Ok(())
    );
    assert_eq!(
        std::fs::read_to_string(&config).ok(),
        Some(policy_text(kept, "m59").expect("a policy"))
    );
}

#[test]
fn a_link_into_the_account_home_lies_under_it() {
    let home = aterm_tempfile::tempdir().expect("an account home");
    let elsewhere = aterm_tempfile::tempdir().expect("a scratch folder");
    std::fs::create_dir(home.path().join(".config")).expect("the home's config folder");
    std::os::unix::fs::symlink(home.path().join(".config"), elsewhere.path().join("config"))
        .expect("a link into the home");
    let through = elsewhere
        .path()
        .join("config")
        .join("aterm")
        .join("aterm.toml");
    assert!(
        under(&through, home.path()),
        "through a link, to a file not made yet"
    );
    assert!(under(
        &home.path().join(".config").join("aterm.toml"),
        home.path()
    ));

    // Control: neither the link's own folder nor a sibling whose name starts with the home's.
    assert!(!under(&elsewhere.path().join("aterm.toml"), home.path()));
    let sibling = PathBuf::from(format!("{}-2", home.path().display()));
    std::fs::create_dir(&sibling).expect("a sibling folder");
    let outside = under(&sibling.join("aterm.toml"), home.path());
    std::fs::remove_dir(&sibling).expect("removes the sibling");
    assert!(!outside);
}

#[test]
fn a_private_root_signs_no_vault_name_under_the_account_home() {
    // A state whose vault sits beside it (`vault_paths_for` reads the state's parent).
    let root = aterm_tempfile::tempdir().expect("a home");
    let state = root.path().join("state");
    std::fs::create_dir(&state).expect("a state directory");
    std::fs::create_dir_all(aterm_secrets::client::Paths::for_home(root.path()).dir)
        .expect("a vault beside it");
    let refused = publication_vault(&state, &private_under(root.path()));
    assert!(
        matches!(&refused, Err(Pending::NotOwnAccount(why)) if why.contains(PRIVATE)),
        "{refused:?}"
    );

    // Control: a vault beside a scratch state is returned to the same process, and to the
    // account's own shell under its home.
    let account = aterm_tempfile::tempdir().expect("another account home");
    let vault = Paths::locate_for_home(root.path()).dir;
    for scope in [
        private_under(account.path()),
        Scope {
            account_home: Some(root.path().to_path_buf()),
            ..own()
        },
    ] {
        assert_eq!(
            publication_vault(&state, &scope).map(|paths| paths.map(|paths| paths.dir)),
            Ok(Some(vault.clone()))
        );
    }
}

/// How many times [`counted_apply`] ran.
static APPLIED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn counted_apply(_: &Path, _: &Path, _: &GeneratedNames) -> Result<(), String> {
    APPLIED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    Ok(())
}

fn refusing_apply(_: &Path, _: &Path, _: &GeneratedNames) -> Result<(), String> {
    panic!("a process that is not its account's own reached the macOS naming operation")
}

fn no_authorization(_: &Path, _: &Path) -> Result<bool, String> {
    Ok(false)
}

#[test]
fn a_private_root_never_reaches_the_apply() {
    let scratch = fresh();
    let names = GeneratedNames::new(num(59), &Nickname::parse("Heron").expect("a nickname"));
    let refusing = SystemNameOperations {
        apply: refusing_apply,
        authorization: no_authorization,
    };
    let refused = apply_registered_with(
        Some(PRIVATE),
        Some(&refusing),
        &scratch.state,
        &scratch.config,
        &names,
    );
    assert!(
        refused.as_ref().is_err_and(|why| why.contains(PRIVATE)),
        "{refused:?}"
    );
    // A test binary runs under its own private state root: the live gate refuses too.
    assert!(
        apply_registered(&scratch.state, &scratch.config, &names)
            .is_err_and(|why| why.contains("private state root"))
    );

    // Control: the account's own shell reaches the operation, once.
    let counting = SystemNameOperations {
        apply: counted_apply,
        authorization: no_authorization,
    };
    assert_eq!(
        apply_registered_with(
            None,
            Some(&counting),
            &scratch.state,
            &scratch.config,
            &names
        ),
        Ok(())
    );
    assert_eq!(APPLIED.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(
        apply_registered_with(None, None, &scratch.state, &scratch.config, &names),
        Err("macOS naming is unavailable in this process".to_owned())
    );
}

#[test]
fn this_process_s_scope_is_read_live() {
    let scope = Scope::live();
    // A test binary runs under its own private state root.
    assert!(
        scope
            .not_own
            .is_some_and(|why| why.contains("private state root")),
        "{:?}",
        scope.not_own
    );
    assert_eq!(scope.account_home, aterm_uds::account::home());
    assert_eq!(scope.home, aterm_types::dirs::home_dir());
    assert_eq!(scope.root, aterm_uds::peer::our_uid() == 0);
}

#[test]
fn a_private_root_publishes_nothing_when_its_vault_is_the_account_s() {
    // An account home holding the state, with the vault beside it (`vault_paths_for` reads the
    // state's parent), and an aterm.toml outside that home.
    let home = aterm_tempfile::tempdir().expect("an account home");
    let state = home.path().join("state");
    std::fs::create_dir(&state).expect("a state directory");
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))
        .expect("a private state directory");
    std::fs::create_dir_all(aterm_secrets::client::Paths::for_home(home.path()).dir)
        .expect("a vault beside it");
    let elsewhere = aterm_tempfile::tempdir().expect("a scratch folder");
    let config = elsewhere.path().join("aterm.toml");
    let kept = "# a scratch aterm.toml\n";
    std::fs::write(&config, kept).expect("the scratch aterm.toml");
    chosen(&state, 59);
    let policy = Policy::read(&config, num(59)).expect("a policy");
    let refused = publish_in(&state, &config, &policy, &private_under(home.path()), None);
    assert!(
        matches!(&refused, Err(Pending::NotOwnAccount(why))
            if why.contains("the vault") && why.contains(PRIVATE)),
        "{refused:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&config).expect("reads"),
        kept,
        "aterm.toml is left as it was"
    );
    assert_eq!(
        read_primary_name(&state),
        Ok(None),
        "no fleet name is written"
    );
    assert_eq!(
        std::fs::read_dir(Paths::locate_for_home(home.path()).dir)
            .expect("the vault folder")
            .count(),
        0,
        "nothing is made in the vault"
    );

    // Control: the same publication, where the account's home is elsewhere, writes each step.
    let account = aterm_tempfile::tempdir().expect("another account home");
    assert_eq!(
        publish_in(
            &state,
            &config,
            &policy,
            &private_under(account.path()),
            None
        ),
        Ok(())
    );
    assert_eq!(
        std::fs::read_to_string(&config).ok(),
        Some(policy_text(kept, "m59").expect("a policy"))
    );
    assert_eq!(read_primary_name(&state), Ok(Some("m59".to_owned())));
}

/// The scope of a process run as root under `sudo`: the account database names root's own home,
/// `root_home`, while `HOME` is still the person's, `home`.
fn root_under(root_home: &Path, home: &Path) -> Scope {
    Scope {
        not_own: Some("it runs as root"),
        root: true,
        account_home: Some(root_home.to_path_buf()),
        home: Some(home.to_path_buf()),
    }
}

#[test]
fn root_publishes_nothing_wherever_it_would_write() {
    let scratch = fresh();
    chosen(&scratch.state, 59);
    let root_home = aterm_tempfile::tempdir().expect("root's home");
    let home = aterm_tempfile::tempdir().expect("the person's HOME, which sudo kept");
    let as_root = root_under(root_home.path(), home.path());
    // A scratch aterm.toml, under neither home.
    let policy = Policy::read(&scratch.config, num(59)).expect("a policy");
    let refused = publish_in(&scratch.state, &scratch.config, &policy, &as_root, None);
    assert!(
        matches!(&refused, Err(Pending::NotOwnAccount(why))
            if why.contains("run as root") && why.contains("it runs as root")),
        "{refused:?}"
    );
    assert_eq!(config_text(&scratch), None, "no aterm.toml is written");
    assert_eq!(
        read_primary_name(&scratch.state),
        Ok(None),
        "no fleet name is written"
    );
    // The person's own aterm.toml, under the HOME sudo kept and outside root's home.
    let config = home.path().join(".config").join("aterm").join("aterm.toml");
    std::fs::create_dir_all(config.parent().expect("a folder")).expect("its folder");
    let kept = "# the person's own settings\n";
    std::fs::write(&config, kept).expect("the person's aterm.toml");
    let policy = Policy::read(&config, num(59)).expect("a policy");
    assert!(matches!(
        publish_in(&scratch.state, &config, &policy, &as_root, None),
        Err(Pending::NotOwnAccount(_))
    ));
    assert_eq!(
        std::fs::read_to_string(&config).expect("reads"),
        kept,
        "the person's aterm.toml is left as it was"
    );

    // Control: the same process, not run as root, writes the scratch aterm.toml.
    let policy = Policy::read(&scratch.config, num(59)).expect("a policy");
    let not_root = Scope {
        root: false,
        ..root_under(root_home.path(), home.path())
    };
    assert_eq!(
        publish_in(&scratch.state, &scratch.config, &policy, &not_root, None),
        Ok(())
    );
    assert_eq!(
        config_text(&scratch),
        Some(policy_text("", "m59").expect("a policy"))
    );
}

#[test]
fn with_no_account_record_home_stands_for_the_account_s_home() {
    let home = aterm_tempfile::tempdir().expect("HOME");
    let elsewhere = aterm_tempfile::tempdir().expect("a scratch folder");
    let mine = home.path().join(".config").join("aterm").join("aterm.toml");
    let scratch = elsewhere.path().join("aterm.toml");
    let unrecorded = Scope {
        not_own: Some("its HOME is not this account's home"),
        root: false,
        account_home: None,
        home: Some(home.path().to_path_buf()),
    };
    let refused = unrecorded.writes("aterm.toml", &mine);
    assert!(
        matches!(&refused, Err(Pending::NotOwnAccount(why)) if why.contains("this account's own")),
        "{refused:?}"
    );
    assert_eq!(unrecorded.writes("aterm.toml", &scratch), Ok(()));
    // Where neither the account database nor `HOME` names a home, nothing is written.
    let homeless = Scope {
        home: None,
        ..unrecorded
    };
    let refused = homeless.writes("aterm.toml", &scratch);
    assert!(
        matches!(&refused, Err(Pending::NotOwnAccount(why)) if why.contains("no home is known")),
        "{refused:?}"
    );

    // Control: the account's own shell writes either, knowing no home at all.
    assert_eq!(own().writes("aterm.toml", &mine), Ok(()));
    assert_eq!(own().writes("aterm.toml", &scratch), Ok(()));
}
