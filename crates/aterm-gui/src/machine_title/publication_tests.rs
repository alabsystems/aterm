// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::{Cell, RefCell};
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;

use aterm_agent::machine_name::{NameTo, file, publish, set_name};
use aterm_net::fleet_member::name_record::{ByKind, FLOOR_MS};
use aterm_secrets::client::LabLabels;
use aterm_types::machine_name::{Nickname, Number};

use super::{change_number, naming};

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
    calls: RefCell<Vec<Call>>,
}

impl Names {
    fn new(fail: bool) -> Self {
        Self {
            values: RefCell::new(std::array::from_fn(|_| Some("m8".to_owned()))),
            fail: Cell::new(fail),
            calls: RefCell::new(Vec::new()),
        }
    }
}

impl publish::SystemNames for Names {
    fn read(&self) -> [Option<String>; 3] {
        self.calls.borrow_mut().push(Call::Read);
        self.values.borrow().clone()
    }

    fn apply(&self, number: Number) -> Result<(), String> {
        self.calls.borrow_mut().push(Call::Apply(number));
        if self.fail.get() {
            return Err("inert OS publication refusal".to_owned());
        }
        *self.values.borrow_mut() = std::array::from_fn(|_| Some(number.to_string()));
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
    let nick_only = crate::machine_rename_sheet::Field::open(Some("Puffin"), &held.word);
    assert!(
        matches!(
            crate::machine_rename_sheet::request(&snapshot, &held, &number, &nick_only).unwrap(),
            crate::machine_rename_sheet::RenameTo::Nick(_)
        ),
        "a nickname edit never borrows synchronization authority"
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
