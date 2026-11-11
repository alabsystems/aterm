// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::{Cell, RefCell};
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;

use aterm_agent::machine_name::{NameTo, file, publish, set_name};
use aterm_net::fleet_member::name_record::{ByKind, FLOOR_MS};
use aterm_secrets::client::LabLabels;
use aterm_types::machine_name::{Nickname, Number};

use super::{change_nickname, change_number, naming};

const NOW: u64 = FLOOR_MS + 300 * 86_400_000;

struct Scratch {
    _root: aterm_tempfile::TempDir,
    state: PathBuf,
    config: PathBuf,
}

fn fresh() -> Scratch {
    let root = aterm_tempfile::tempdir().unwrap();
    // Keep the state inside its own scratch parent: fleet publication must not find the
    // person's vault beside a conventional account state path.
    let state = root.path().join("state");
    std::fs::create_dir(&state).unwrap();
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = root.path().join("aterm.toml");
    Scratch {
        _root: root,
        state,
        config,
    }
}

fn request(number: u32) -> NameTo {
    NameTo {
        num: Number::new(number).unwrap(),
        nick: Some(Nickname::parse("Heron").unwrap()),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Call {
    Read,
    Apply(Number),
}

struct Names {
    values: RefCell<[Option<String>; 3]>,
    fail: Cell<bool>,
    authorized: Cell<bool>,
    calls: RefCell<Vec<Call>>,
}

impl Names {
    fn new(fail: bool) -> Self {
        Self {
            values: RefCell::new(std::array::from_fn(|_| Some("m8".to_owned()))),
            fail: Cell::new(fail),
            authorized: Cell::new(true),
            calls: RefCell::new(Vec::new()),
        }
    }
}

impl publish::SystemNames for Names {
    fn read(&self) -> [Option<String>; 3] {
        self.calls.borrow_mut().push(Call::Read);
        self.values.borrow().clone()
    }

    fn authorization(&self, _: &std::path::Path, _: &std::path::Path) -> Result<bool, String> {
        Ok(self.authorized.get())
    }

    fn apply(
        &self,
        _: &std::path::Path,
        _: &std::path::Path,
        names: &aterm_types::machine_name::GeneratedNames,
    ) -> Result<(), String> {
        self.calls.borrow_mut().push(Call::Apply(names.number()));
        if self.fail.get() {
            return Err("inert OS publication refusal".to_owned());
        }
        *self.values.borrow_mut() = [
            Some(names.computer_name().to_owned()),
            Some(names.host_name().to_owned()),
            Some(names.host_name().to_owned()),
        ];
        Ok(())
    }
}

#[test]
fn naming_policy_refuses_before_the_canonical_set_or_any_system_operation() {
    for policy in ["[identity]\nname = false\n", "[identity\n"] {
        let scratch = fresh();
        let before = set_name(
            &scratch.state,
            &request(8),
            || Ok(LabLabels::default()),
            ByKind::Person,
            || Ok(None),
            NOW,
            false,
        )
        .unwrap()
        .name;
        std::fs::write(&scratch.config, policy).unwrap();
        let names = Names::new(false);
        assert!(
            change_number(
                &scratch.state,
                &scratch.config,
                &request(86),
                &names,
                || panic!("a rejected policy must not enter the canonical setter"),
                || panic!("a rejected policy must not read the roster"),
                NOW + 1,
            )
            .is_err()
        );
        assert!(
            change_nickname(
                &scratch.state,
                &scratch.config,
                &aterm_agent::machine_name::NickTo::Word(Nickname::parse("Dog").unwrap()),
                &names,
                || panic!("a rejected policy must not read the nickname roster"),
                NOW + 1,
            )
            .is_err()
        );
        assert_eq!(file::read(&scratch.state).unwrap(), Some(before));
        assert_eq!(std::fs::read(&scratch.config).unwrap(), policy.as_bytes());
        assert!(names.calls.borrow().is_empty());
    }
}

#[test]
fn a_partial_name_publication_retries_without_rechoosing_and_rejects_a_stale_retry() {
    use std::os::unix::fs::MetadataExt as _;

    let scratch = fresh();
    let prior_config: &[u8] = b"bell_sound = false\n";
    std::fs::write(&scratch.config, prior_config).unwrap();
    let names = Names::new(true);
    let requested = request(86);
    let edited = change_number(
        &scratch.state,
        &scratch.config,
        &requested,
        &names,
        || Ok(LabLabels::default()),
        || Ok(None),
        NOW,
    )
    .unwrap();
    assert!(edited.changed);
    assert_eq!(edited.name.num, Some(requested.num));
    assert!(matches!(
        edited.publication,
        Some(publish::Pending::SystemNames(_))
    ));
    assert_eq!(
        file::read(&scratch.state).unwrap(),
        Some(edited.name.clone())
    );
    assert_eq!(
        aterm_net::fleet_member::read_primary_name(&scratch.state).unwrap(),
        Some("m86".to_owned())
    );
    assert!(matches!(
        publish::publication(&scratch.state, &scratch.config, &edited.name, Some(&names.values.borrow())).unwrap(),
        publish::Publication::Owed(owed) if owed.len() == 1 && matches!(&owed[0], publish::Owed::SystemNames(_))
    ));
    let backup = scratch.config.with_file_name("aterm.toml.bak");
    assert_eq!(std::fs::read(&backup).unwrap(), prior_config);
    let fleet_name = scratch.state.join(aterm_net::fleet_member::NAME_FILE);
    // Keep each original inode alive, so a replacement cannot pass through inode reuse.
    let held_files = [&scratch.config, &backup, &fleet_name].map(|path| {
        (
            path,
            std::fs::File::open(path).unwrap(),
            std::fs::read(path).unwrap(),
        )
    });
    let unchanged_files = || {
        for (path, held, bytes) in &held_files {
            let before = held.metadata().unwrap();
            let after = std::fs::metadata(path).unwrap();
            assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
            assert_eq!(&std::fs::read(path).unwrap(), bytes);
        }
    };

    // The actual panel-open producer sees a saved number and the unfinished OS step. Its
    // untouched fields request publication, rather than choosing the number again.
    let preview = aterm_agent::machine_name::rename_preview(
        &scratch.state,
        || Ok(LabLabels::default()),
        || Ok(None),
        NOW + 1,
    )
    .unwrap();
    let held = crate::machine_rename_sheet::Held::of_read(&preview);
    let status =
        naming::NamingStatus::read(&scratch.state, &scratch.config, preview.name(), &names);
    let snapshot = crate::machine_rename_sheet::Snapshot::named(preview, status);
    let number = crate::machine_rename_sheet::Field::open(None, held.number_text());
    let nickname = crate::machine_rename_sheet::Field::open(None, &held.word);
    assert_eq!(
        crate::machine_rename_sheet::request(&snapshot, &held, &number, &nickname).unwrap(),
        crate::machine_rename_sheet::RenameTo::Synchronize(requested.num)
    );

    // Missing/old permission is a separate explicit action carrying no captured name.
    names.authorized.set(false);
    let auth_preview = aterm_agent::machine_name::rename_preview(
        &scratch.state,
        || Ok(LabLabels::default()),
        || Ok(None),
        NOW + 1,
    )
    .unwrap();
    let auth_status =
        naming::NamingStatus::read(&scratch.state, &scratch.config, auth_preview.name(), &names);
    assert!(auth_status.authorize());
    let auth_snapshot = crate::machine_rename_sheet::Snapshot::named(auth_preview, auth_status);
    assert_eq!(
        crate::machine_rename_sheet::request(&auth_snapshot, &held, &number, &nickname).unwrap(),
        crate::machine_rename_sheet::RenameTo::Authorize,
    );
    names.authorized.set(true);

    // A failed publication observation keeps the explicit retry available. The
    // execution still reads the current policy before reaching any system port.
    let good_config = std::fs::read(&scratch.config).unwrap();
    for (policy, expected) in [
        (
            "[identity\n",
            crate::machine_rename_sheet::RenameTo::Synchronize(requested.num),
        ),
        (
            "[identity]\nname = false\n",
            crate::machine_rename_sheet::RenameTo::Keep,
        ),
    ] {
        std::fs::write(&scratch.config, policy).unwrap();
        let preview = aterm_agent::machine_name::rename_preview(
            &scratch.state,
            || Ok(LabLabels::default()),
            || Ok(None),
            NOW + 1,
        )
        .unwrap();
        let status =
            naming::NamingStatus::read(&scratch.state, &scratch.config, preview.name(), &names);
        let unchecked = crate::machine_rename_sheet::Snapshot::named(preview, status);
        assert_eq!(
            crate::machine_rename_sheet::request(&unchecked, &held, &number, &nickname).unwrap(),
            expected,
        );
        let calls = names.calls.borrow().len();
        assert!(matches!(
            naming::synchronize(&scratch.state, &scratch.config, requested.num, &names),
            Err(publish::Pending::Policy(_)),
        ));
        assert_eq!(names.calls.borrow().len(), calls);
        assert_eq!(
            file::read(&scratch.state).unwrap(),
            Some(edited.name.clone())
        );
        assert_eq!(std::fs::read(&scratch.config).unwrap(), policy.as_bytes());
    }
    std::fs::write(&scratch.config, good_config).unwrap();

    let nick_only = crate::machine_rename_sheet::Field::open(Some("Puffin"), &held.word);
    assert!(
        matches!(
            crate::machine_rename_sheet::request(&snapshot, &held, &number, &nick_only).unwrap(),
            crate::machine_rename_sheet::RenameTo::Nick(_)
        ),
        "a nickname edit is saved before its generated names are synchronized"
    );

    let applies = || {
        names
            .calls
            .borrow()
            .iter()
            .filter(|call| matches!(call, Call::Apply(number) if *number == requested.num))
            .count()
    };
    let applied = applies();
    assert!(matches!(
        naming::synchronize(&scratch.state, &scratch.config, requested.num, &names),
        Err(publish::Pending::SystemNames(_))
    ));
    assert_eq!(applies(), applied + 1);
    unchanged_files();

    names.fail.set(false);
    naming::synchronize(&scratch.state, &scratch.config, requested.num, &names).unwrap();
    assert_eq!(applies(), applied + 2);
    unchanged_files();
    assert_eq!(
        file::read(&scratch.state).unwrap(),
        Some(edited.name.clone()),
        "retry must not choose the number again"
    );
    assert_eq!(
        publish::publication(
            &scratch.state,
            &scratch.config,
            &edited.name,
            Some(&names.values.borrow())
        )
        .unwrap(),
        publish::Publication::Current,
    );

    // A Bonjour alias is visible drift, not a repair that repeatedly resets macOS.
    let matching = names.values.borrow().clone();
    names.values.borrow_mut()[1] = Some("m86-heron-2".to_owned());
    let applied = applies();
    assert!(matches!(
        naming::synchronize(&scratch.state, &scratch.config, requested.num, &names),
        Err(publish::Pending::SystemNames(_))
    ));
    assert_eq!(applies(), applied);
    let preview = aterm_agent::machine_name::rename_preview(
        &scratch.state,
        || Ok(LabLabels::default()),
        || Ok(None),
        NOW + 1,
    )
    .unwrap();
    let held_alias = crate::machine_rename_sheet::Held::of_read(&preview);
    let status =
        naming::NamingStatus::read(&scratch.state, &scratch.config, preview.name(), &names);
    assert!(!status.retry());
    assert!(
        !status.authorize(),
        "a collision alias never asks for naming permission"
    );
    let text = status.text("m86");
    assert!(text.contains("expected m86-heron.local"));
    assert!(text.contains("actual m86-heron-2.local (collision alias)"));
    let snapshot = crate::machine_rename_sheet::Snapshot::named(preview, status);
    let number = crate::machine_rename_sheet::Field::open(None, held_alias.number_text());
    let nickname = crate::machine_rename_sheet::Field::open(None, &held_alias.word);
    assert_eq!(
        crate::machine_rename_sheet::request(&snapshot, &held_alias, &number, &nickname).unwrap(),
        crate::machine_rename_sheet::RenameTo::Keep,
    );
    // Policy debt alongside an accepted alias needs ordinary synchronization, not a
    // permission prompt for an OS setter that must not run.
    let alias_config = std::fs::read(&scratch.config).unwrap();
    std::fs::write(&scratch.config, "[identity]\nname = \"m1\"\n").unwrap();
    names.authorized.set(false);
    let alias_status =
        naming::NamingStatus::read(&scratch.state, &scratch.config, &edited.name, &names);
    assert!(alias_status.retry());
    assert!(!alias_status.authorize());
    std::fs::write(&scratch.config, alias_config).unwrap();
    names.authorized.set(true);
    *names.values.borrow_mut() = matching;

    // Matching bytes do not replace the writer's exact private-file permission contract.
    std::fs::set_permissions(&fleet_name, std::fs::Permissions::from_mode(0o400)).unwrap();
    naming::synchronize(&scratch.state, &scratch.config, requested.num, &names).unwrap();
    assert_eq!(
        std::fs::metadata(&fleet_name).unwrap().mode() & 0o7777,
        0o600
    );

    let newer = change_number(
        &scratch.state,
        &scratch.config,
        &request(87),
        &names,
        || Ok(LabLabels::default()),
        || Ok(None),
        NOW + 1,
    )
    .unwrap();
    assert!(newer.publication.is_none());
    let config = std::fs::read(&scratch.config).unwrap();
    let fleet = aterm_net::fleet_member::read_primary_name(&scratch.state).unwrap();
    let values = names.values.borrow().clone();
    let calls = names.calls.borrow().len();
    assert!(matches!(
        naming::synchronize(&scratch.state, &scratch.config, requested.num, &names),
        Err(publish::Pending::Superseded(_)),
    ));
    assert_eq!(file::read(&scratch.state).unwrap(), Some(newer.name));
    assert_eq!(std::fs::read(&scratch.config).unwrap(), config);
    assert_eq!(
        aterm_net::fleet_member::read_primary_name(&scratch.state).unwrap(),
        fleet
    );
    assert_eq!(*names.values.borrow(), values);
    assert_eq!(
        names.calls.borrow().len(),
        calls,
        "a stale retry must not reach the system port"
    );

    let nick = change_nickname(
        &scratch.state,
        &scratch.config,
        &aterm_agent::machine_name::NickTo::Word(Nickname::parse("dog").unwrap()),
        &names,
        || Ok(None),
        NOW + 2,
    )
    .unwrap();
    assert!(nick.publication.is_none());
    assert_eq!(nick.name.num, Some(Number::new(87).unwrap()));
    assert_eq!(nick.name.nick.as_str(), "dog");
    assert_eq!(
        *names.values.borrow(),
        [
            Some("m87 Dog".to_owned()),
            Some("m87-dog".to_owned()),
            Some("m87-dog".to_owned()),
        ]
    );
    assert_eq!(
        aterm_net::fleet_member::read_primary_name(&scratch.state)
            .unwrap()
            .as_deref(),
        Some("m87")
    );

    // The person changed the saved choice while permission was pending. Completion reads
    // the new number and nickname; it cannot replay the panel's prior m87 Dog target.
    let newer = NameTo {
        num: Number::new(88).unwrap(),
        nick: Some(Nickname::parse("Otter").unwrap()),
    };
    aterm_agent::machine_name::set_name(
        &scratch.state,
        &newer,
        || Ok(LabLabels::default()),
        aterm_net::fleet_member::name_record::ByKind::Person,
        || Ok(None),
        NOW + 3,
        false,
    )
    .unwrap();
    let resumed_applies = || {
        names
            .calls
            .borrow()
            .iter()
            .filter(|call| matches!(call, Call::Apply(_)))
            .count()
    };
    let applied_before = resumed_applies();
    names.authorized.set(false);
    assert!(super::synchronize_current(&scratch.state, &scratch.config, &names).is_err());
    assert_eq!(
        resumed_applies(),
        applied_before,
        "lost permission cannot run the resumed publication"
    );
    names.authorized.set(true);
    let current = super::synchronize_current(&scratch.state, &scratch.config, &names).unwrap();
    assert!(current.publication.is_none());
    assert_eq!(current.name.num, Some(newer.num));
    assert_eq!(current.name.nick.as_str(), "Otter");
    assert_eq!(resumed_applies(), applied_before + 1);
    assert_eq!(
        names
            .calls
            .borrow()
            .iter()
            .rev()
            .find_map(|call| match call {
                Call::Apply(number) => Some(*number),
                Call::Read => None,
            }),
        Some(newer.num),
        "authorization completion applies the current number"
    );
    assert_eq!(
        *names.values.borrow(),
        [
            Some("m88 Otter".to_owned()),
            Some("m88-otter".to_owned()),
            Some("m88-otter".to_owned()),
        ]
    );
}

#[test]
fn saving_a_fresh_seeded_number_makes_a_real_choice_without_promoting_an_unread_view() {
    use crate::machine_rename_sheet::{Field, Held, RenameTo, Snapshot, request as panel_request};
    use aterm_types::machine_name::Class;
    let scratch = fresh();
    let (seeded, _) = aterm_agent::machine_name::ensure(
        &scratch.state,
        || Ok(Some("m86".to_owned())),
        |_| false,
        NOW,
    )
    .unwrap();
    assert_eq!(seeded.class, Class::Seeded);
    let preview = aterm_agent::machine_name::rename_preview(
        &scratch.state,
        || Ok(LabLabels::default()),
        || Ok(None),
        NOW + 1,
    )
    .unwrap();
    let held = Held::of_read(&preview);
    let number = Field::open(None, held.number_text());
    let nickname = Field::open(None, &held.word);
    assert_eq!(
        panel_request(&Snapshot::Reading, &held, &number, &nickname).unwrap(),
        RenameTo::Keep
    );
    let names = Names::new(true);
    let status =
        naming::NamingStatus::read(&scratch.state, &scratch.config, preview.name(), &names);
    let snapshot = Snapshot::named(preview, status);
    let RenameTo::Name(selected) = panel_request(&snapshot, &held, &number, &nickname).unwrap()
    else {
        panic!("Save explicitly chooses the fresh displayed seeded number");
    };
    assert_eq!(selected.num, seeded.num.unwrap());
    assert_eq!(selected.nick, None);
    let edited = change_number(
        &scratch.state,
        &scratch.config,
        &selected,
        &names,
        || Ok(LabLabels::default()),
        || Ok(None),
        NOW + 2,
    )
    .unwrap();
    assert_eq!(edited.name.class, Class::Chosen);
    assert!(matches!(
        edited.publication,
        Some(publish::Pending::SystemNames(_))
    ));
    let preview = aterm_agent::machine_name::rename_preview(
        &scratch.state,
        || Ok(LabLabels::default()),
        || Ok(None),
        NOW + 3,
    )
    .unwrap();
    let held = Held::of_read(&preview);
    let status =
        naming::NamingStatus::read(&scratch.state, &scratch.config, preview.name(), &names);
    let snapshot = Snapshot::named(preview, status);
    assert_eq!(
        panel_request(
            &snapshot,
            &held,
            &Field::open(None, held.number_text()),
            &Field::open(None, &held.word)
        )
        .unwrap(),
        RenameTo::Synchronize(selected.num)
    );
}
