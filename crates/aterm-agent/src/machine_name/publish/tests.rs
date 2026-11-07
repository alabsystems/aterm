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
            nick: None,
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

    fn apply(&self, number: Number) -> Result<(), String> {
        self.log.borrow_mut().push(format!("apply {number}"));
        (self.on_apply)();
        if let Some(after) = &self.after {
            *self.names.borrow_mut() = after.clone();
        }
        self.answer.clone()
    }
}

fn nothing() {}

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
    fake.after = Some(names([Some("m59"); 3]));
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
            Some(&names([Some("m59"); 3]))
        ),
        Ok(Publication::Current)
    );
}

#[test]
fn names_macos_already_reads_ask_for_nothing() {
    let scratch = fresh();
    chosen(&scratch.state, 59);
    let policy = Policy::read(&scratch.config, num(59)).expect("a policy");
    let fake = FakeNames::new(names([Some("m59"); 3]), &nothing);
    assert_eq!(
        publish(&scratch.state, &scratch.config, &policy, Some(&fake)),
        Ok(())
    );
    assert_eq!(*fake.log.borrow(), ["read"], "no apply");
    assert_eq!(
        read_primary_name(&scratch.state).expect("reads"),
        Some("m59".to_owned())
    );
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

    // macOS's names: the password sheet cancelled.
    let scratch = fresh();
    chosen(&scratch.state, 59);
    let policy = Policy::read(&scratch.config, num(59)).expect("a policy");
    let mut fake = FakeNames::new(names([Some("m184"); 3]), &nothing);
    fake.answer = Err("the password sheet was cancelled, or its window closed".to_owned());
    assert_eq!(
        publish(&scratch.state, &scratch.config, &policy, Some(&fake)),
        Err(Pending::SystemNames(
            "the password sheet was cancelled, or its window closed".to_owned()
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
    fake.after = Some(names([Some("m60"); 3]));
    assert_eq!(
        publish(&scratch.state, &scratch.config, &policy, Some(&fake)),
        Err(Pending::Superseded(SUPERSEDED.to_owned()))
    );
    assert_eq!(fake.applied(), ["apply m60"]);

    // A set of m62 while the password sheet is cancelled: the sheet's words, superseded.
    let policy = Policy::read(&scratch.config, num(61)).expect("a policy");
    let during = || {
        chosen(&scratch.state, 62);
    };
    let mut fake = FakeNames::new(names([Some("m184"); 3]), &during);
    fake.answer = Err("the password sheet was cancelled, or its window closed".to_owned());
    assert_eq!(
        publish(&scratch.state, &scratch.config, &policy, Some(&fake)),
        Err(Pending::Superseded(
            "the password sheet was cancelled, or its window closed".to_owned()
        ))
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
            &policy,
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
            &policy,
            Some(&fake),
            fleet_then_set(&scratch, 60, 61)
        ),
        Err(Pending::Superseded(SUPERSEDED.to_owned()))
    );
    assert!(fake.log.borrow().is_empty(), "macOS not asked");
}

#[test]
fn a_final_system_verification_rejects_a_newer_canonical_name() {
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
    fake.after = Some(names([Some("m59"); 3]));
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
    fake.after = Some(names([Some("m59"); 3]));
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
    let fake = FakeNames::new(names([Some("m184"), Some("m59"), Some("m59")]), &nothing);
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
    let now = names([Some("m59"); 3]);
    assert_eq!(
        publication_of(Some(&published), Some("m59"), &name, Some(&now)),
        Ok(Publication::Current)
    );
    assert_eq!(
        publication_of(
            Some("[ai]\nmachine = \"m59\"\n[fleet]\nmachine = \"m158\"\n"),
            Some("m158"),
            &name,
            Some(&names([Some("m59"), Some("m59"), None]))
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
fn the_root_half_is_the_fixed_four_words() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt as _;
    let exe = Path::new("/x/aterm");
    let words = apply_system_words(exe, num(59)).expect("UTF-8");
    assert_eq!(words, ["/x/aterm", "name", "apply-system", "m59"]);
    assert_eq!(is_apply_system(&words, exe), Some(num(59)));
    let owned = |words: &[&str]| words.iter().map(|&w| w.to_owned()).collect::<Vec<_>>();
    for other in [
        owned(&["/y/aterm", "name", "apply-system", "m59"]),
        owned(&["/x/aterm", "name", "apply-system", "m59", "m60"]),
        owned(&["/x/aterm", "name", "apply-system"]),
        owned(&["/x/aterm", "name", "--set", "m59"]),
        owned(&["/x/aterm", "name", "apply-system", "59"]),
        owned(&["/x/aterm", "name", "apply-system", "M59"]),
        owned(&["/x/aterm", "name", "apply-system", "m0"]),
        owned(&["/usr/sbin/scutil", "--set", "HostName", "m59"]),
    ] {
        assert_eq!(is_apply_system(&other, exe), None, "{other:?}");
    }
    let unnamed = Path::new(OsStr::from_bytes(b"/x/\xffaterm"));
    assert_eq!(
        apply_system_words(unnamed, num(59)),
        Err("this program's path is not UTF-8".to_owned())
    );
    assert_eq!(is_apply_system(&words, unnamed), None);
}

#[test]
fn all_three_names_must_read_the_number() {
    assert!(all_read(&names([Some("m59"); 3]), num(59)));
    assert!(!all_read(&names([Some("m59"), Some("m59"), None]), num(59)));
    assert!(!all_read(
        &names([Some("m59"), Some("M59"), Some("m59")]),
        num(59)
    ));
    assert!(!all_read(&names([Some("m59"); 3]), num(5)));
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
            "macOS still calls this Mac by its old name (x)",
        ),
        (
            Pending::NameUnread("x".to_owned()),
            "its name could not be read back (x)",
        ),
    ] {
        assert_eq!(pending.why(), "x");
        assert_eq!(pending.phrase(), phrase);
    }
}
