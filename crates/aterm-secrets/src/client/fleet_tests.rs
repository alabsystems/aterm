// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use SwarmDenialSnapshot::{Available, Unavailable};
use SwarmDenialUnavailable as Why;

fn denial_fixture() -> (aterm_tempfile::TempDir, Paths) {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("member"));
    let source = root.path().join("roster.vault");
    create(&paths, source.to_str().unwrap(), None, Some("m60")).unwrap();
    let admin = admin_open(&paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &Audience::Everyone,
        SWARM_ROSTER_ENTRY,
        b"deny_nodes = [\"m99\"]\n",
    )
    .unwrap();
    admin_publish(&paths, &admin, &doc).unwrap();
    (root, paths)
}

#[test]
fn bounded_denial_waits_only_for_busy_and_keeps_the_signed_denials() {
    let (_root, paths) = denial_fixture();
    let mut writer = Some(StateLock::take(&paths).unwrap());
    assert_eq!(swarm_denied_nodes(&paths).unwrap(), None);
    let mut waits = 0;
    assert_eq!(
        swarm_denial_snapshot(&paths).unwrap(),
        Unavailable(Why::WriterBusy)
    );
    let read = swarm_denial_snapshot_until_with(
        &paths,
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        |_| {
            waits += 1;
            drop(writer.take());
        },
    )
    .unwrap();
    assert_eq!(waits, 1);
    assert_eq!(read, Available(vec!["m99".to_owned()]));
    // The returned owned list holds no reader lease across its caller's network work.
    assert!(StateLock::try_take(&paths).unwrap().is_some());
}

/// A writer mid-apply holds the state lock: the bounded roster read waits it out and reads the
/// roster the copy holds; past its deadline it says the roster is busy, never that there is
/// none. NEGATIVE CONTROL: the unbounded read takes the same moment as no roster.
#[test]
fn bounded_roster_waits_out_a_writer_and_is_busy_past_its_deadline() {
    let (root, paths) = denial_fixture();
    let settled =
        swarm_roster_until(&paths, Instant::now() + aterm_spec::wait::MUST_HAPPEN).unwrap();
    assert!(settled.is_some(), "the fixture publishes a roster");
    let mut writer = Some(StateLock::take(&paths).unwrap());
    assert_eq!(test_unwaited_roster(&paths).unwrap(), None);
    let busy = swarm_roster_until(&paths, Instant::now()).unwrap_err();
    assert_eq!(busy.kind(), crate::ErrorKind::VaultBusy);
    let mut waits = 0;
    let read = swarm_roster_until_with(
        &paths,
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        |_| {
            waits += 1;
            drop(writer.take());
        },
    )
    .unwrap();
    assert_eq!(waits, 1);
    assert_eq!(read, settled);
    // The returned roster holds no reader lease across its caller's network work.
    assert!(StateLock::try_take(&paths).unwrap().is_some());
    // A folder no copy was ever applied to has no roster, and nothing to wait for.
    let none = Paths::for_home(&root.path().join("never"));
    assert_eq!(
        swarm_roster_until_with(
            &none,
            Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            |_| { panic!("nothing holds an absent vault") }
        )
        .unwrap(),
        None
    );
}

#[test]
fn bounded_denial_rereads_a_source_changed_by_the_writer() {
    let (root, paths) = denial_fixture();
    let other = Paths::for_home(&root.path().join("other"));
    let other_source = root.path().join("other.vault");
    create(&other, other_source.to_str().unwrap(), None, Some("m61")).unwrap();
    let next = config(&other).unwrap().unwrap();
    let mut writer = Some(StateLock::take(&paths).unwrap());
    let mut waits = 0;
    let read = swarm_denial_snapshot_until_with(
        &paths,
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        |_| {
            waits += 1;
            save_config(&paths, &next).unwrap();
            drop(writer.take());
        },
    )
    .unwrap();
    assert_eq!(waits, 1);
    assert_eq!(
        read,
        Unavailable(Why::ConfigurationChanged),
        "an old applied copy authorizes no new source"
    );
}

#[test]
fn bounded_denial_pins_an_independently_readable_source() {
    let (root, paths) = denial_fixture();
    let mut next = config(&paths).unwrap().unwrap();
    next.source = Source::Path(root.path().join("relocated.vault"));
    let mut next_state = load_state(&paths).unwrap();
    next_state.source_identity = Some(source_identity(&next));
    let mut writer = Some(StateLock::take(&paths).unwrap());
    let mut waits = 0;
    let read = swarm_denial_snapshot_until_with(
        &paths,
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        |_| {
            waits += 1;
            save_config(&paths, &next).unwrap();
            save_state(&paths, &next_state).unwrap();
            drop(writer.take());
            assert_eq!(
                swarm_denied_nodes(&paths).unwrap(),
                Some(vec!["m99".to_owned()]),
                "the replacement is an independently readable snapshot"
            );
        },
    )
    .unwrap();
    assert_eq!(waits, 1);
    assert_eq!(
        read,
        Unavailable(Why::ConfigurationChanged),
        "this wait belongs to the original configured source"
    );
}

#[test]
fn bounded_denial_pins_a_removed_or_unsigned_source() {
    let outcomes: Vec<_> = [false, true]
        .into_iter()
        .map(|unsigned| {
            let (_root, paths) = denial_fixture();
            let mut next = config(&paths).unwrap().unwrap();
            next.signer = None;
            let mut writer = Some(StateLock::take(&paths).unwrap());
            let mut waits = 0;
            let read = swarm_denial_snapshot_until_with(
                &paths,
                Instant::now() + aterm_spec::wait::MUST_HAPPEN,
                |_| {
                    waits += 1;
                    if unsigned {
                        save_config(&paths, &next).unwrap();
                    } else {
                        std::fs::remove_file(paths.source_file()).unwrap();
                    }
                    drop(writer.take());
                    assert_eq!(swarm_denied_nodes(&paths).unwrap(), Some(Vec::new()));
                },
            )
            .unwrap();
            assert_eq!(waits, 1);
            (unsigned, read)
        })
        .collect();
    assert_eq!(
        outcomes,
        [
            (false, Unavailable(Why::ConfigurationChanged)),
            (true, Unavailable(Why::ConfigurationChanged))
        ]
    );
}

#[test]
fn bounded_denial_does_not_wait_on_expiry_revocation_absence_or_io_error() {
    let (_root, paths) = denial_fixture();
    let original = load_state(&paths).unwrap();
    let writer = StateLock::take(&paths).unwrap();
    assert_eq!(
        swarm_denial_snapshot_until_with(&paths, Instant::now(), |_| panic!("expired")).unwrap(),
        Unavailable(Why::WriterBusy)
    );
    drop(writer);

    let future = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    let mut revoked = original.clone();
    revoked.membership = Stored::Revoked;
    save_state(&paths, &revoked).unwrap();
    assert_eq!(
        swarm_denial_snapshot_until_with(&paths, future, |_| panic!("revoked")).unwrap(),
        Unavailable(Why::Revoked)
    );
    save_state(&paths, &original).unwrap();
    let mut partial = original.clone();
    partial.failed = true;
    save_state(&paths, &partial).unwrap();
    assert_eq!(
        swarm_denial_snapshot_until_with(&paths, future, |_| panic!("partial apply")).unwrap(),
        Unavailable(Why::EntryUnavailable)
    );
    save_state(&paths, &original).unwrap();
    std::fs::remove_file(paths.lock_file()).unwrap();
    assert_eq!(
        swarm_denial_snapshot_until_with(&paths, future, |_| panic!("missing lock")).unwrap(),
        Unavailable(Why::MissingStateLock)
    );
    drop(StateLock::take(&paths).unwrap());
    std::fs::set_permissions(
        paths.source_file(),
        std::os::unix::fs::PermissionsExt::from_mode(0o644),
    )
    .unwrap();
    assert!(swarm_denial_snapshot_until_with(&paths, future, |_| panic!("read error")).is_err());
}

#[test]
fn a_writer_released_after_the_entry_read_cannot_erase_roster_denials() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("member"));
    let source = root.path().join("roster.vault");
    create(&paths, source.to_str().unwrap(), None, Some("m60")).unwrap();
    let admin = admin_open(&paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(
        &Audience::Everyone,
        SWARM_ROSTER_ENTRY,
        b"deny_nodes = [\"m99\"]\n",
    )
    .unwrap();
    admin_publish(&paths, &admin, &doc).unwrap();
    sync(&paths, None).unwrap();
    let expected = Some(vec!["m99".to_owned()]);
    assert_eq!(swarm_denied_nodes(&paths).unwrap(), expected);
    let observed = swarm_denied_nodes_then(&paths, || {
        assert!(StateLock::try_take(&paths).unwrap().is_none());
    });
    assert_eq!(observed.unwrap(), expected);

    let writer = StateLock::take(&paths).unwrap();
    let raced = swarm_denied_nodes_then(&paths, move || drop(writer));
    assert_eq!(raced.unwrap(), None);
    assert_eq!(swarm_denied_nodes(&paths).unwrap(), expected);
}

#[test]
fn an_absent_roster_is_known_only_from_an_observed_joined_snapshot() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("member"));
    assert_eq!(swarm_denied_nodes(&paths).unwrap(), Some(Vec::new()));
    let source = root.path().join("roster.vault");
    create(&paths, source.to_str().unwrap(), None, Some("m60")).unwrap();
    assert_eq!(swarm_denied_nodes(&paths).unwrap(), Some(Vec::new()));
    assert_eq!(
        swarm_denial_snapshot(&paths).unwrap(),
        Available(Vec::new())
    );

    let joined = load_state(&paths).unwrap();
    std::fs::remove_file(paths.lock_file()).unwrap();
    assert_eq!(swarm_denied_nodes(&paths).unwrap(), None);
    assert_eq!(
        swarm_denial_snapshot(&paths).unwrap(),
        Unavailable(Why::MissingStateLock)
    );
    let configured = config(&paths).unwrap().unwrap();
    let mut unsigned = configured.clone();
    unsigned.signer = None;
    save_config(&paths, &unsigned).unwrap();
    assert_eq!(swarm_denied_nodes(&paths).unwrap(), Some(Vec::new()));
    assert_eq!(
        swarm_denial_snapshot(&paths).unwrap(),
        Available(Vec::new())
    );
    save_config(&paths, &configured).unwrap();
    for membership in [Stored::None, Stored::Pending, Stored::Plain] {
        let mut unjoined = joined.clone();
        unjoined.membership = membership;
        save_state(&paths, &unjoined).unwrap();
        assert_eq!(swarm_denied_nodes(&paths).unwrap(), Some(Vec::new()));
        assert_eq!(
            swarm_denial_snapshot(&paths).unwrap(),
            Available(Vec::new())
        );
    }
    save_state(&paths, &joined).unwrap();
    drop(StateLock::take(&paths).unwrap());
    assert_eq!(swarm_denied_nodes(&paths).unwrap(), Some(Vec::new()));
}

#[test]
fn a_conditional_roster_is_unavailable_rather_than_absent() {
    for anchor in [None, Some("anchor")] {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(&root.path().join("member"));
        let source = root.path().join("roster.vault");
        create(&paths, source.to_str().unwrap(), None, Some("m60")).unwrap();
        let admin = admin_open(&paths).unwrap();
        let mut doc = admin.doc.clone();
        if anchor.is_some() {
            doc.set_entry_with(&Audience::Everyone, "anchor", b"inert", true, None)
                .unwrap();
        }
        doc.set_entry_with(
            &Audience::Everyone,
            SWARM_ROSTER_ENTRY,
            b"deny_nodes = [\"m99\"]\n",
            true,
            anchor,
        )
        .unwrap();
        admin_publish(&paths, &admin, &doc).unwrap();
        sync(&paths, None).unwrap();
        assert_eq!(swarm_denied_nodes(&paths).unwrap(), None);
        assert_eq!(
            swarm_denial_snapshot(&paths).unwrap(),
            Unavailable(Why::EntryUnavailable)
        );
    }
}

#[test]
fn parse_denied_nodes_normalizes_and_rejects_bad_types() {
    assert_eq!(parse_denied_nodes_toml("").unwrap(), Vec::<String>::new());
    assert_eq!(
        parse_denied_nodes_toml(
            "deny_labels = [\"m9\"]\ndeny_nodes = [\" DEADBEEF \", \"0102\", \"deadbeef\", \"\"]\n"
        )
        .unwrap(),
        vec!["0102".to_string(), "deadbeef".to_string()]
    );
    assert!(parse_denied_nodes_toml("deny_nodes = 7\n").is_err());
    assert!(parse_denied_nodes_toml("deny_nodes = [7]\n").is_err());
}

#[test]
fn parse_denied_nodes_keeps_values_out_of_errors() {
    let private = "private-roster-value-must-not-escape";
    let text = format!("deny_nodes = [\"{private}\" !]\n");
    let error = parse_denied_nodes_toml(&text).unwrap_err().to_string();
    assert!(
        !error.contains(private),
        "a parse diagnostic exposed a vault value"
    );
    assert!(
        error.contains("line 1"),
        "the source position was lost: {error}"
    );
}

/// A state lock an older build left at `0644`, this user's own, is narrowed to `0600` through
/// its descriptor by the deny list's read, which then answers, directly and through the
/// listener's memo, as the roster's memo does. The same lock seen as another user's, or one
/// reached through a symlink, is left exactly as it is and refused as before.
///
/// The roster's read narrows it as well, the lock narrowed is the inode the checks judged
/// (a file renamed to its name in between is left), and one in a directory other users may
/// write is left.
///
/// NEGATIVE CONTROLS (2026-11-01): with `swarm_denied_nodes`, `swarm_roster` and the memo
/// not calling `StateLock::narrow_owned`, the first read fails on the wide lock (`read past a
/// wide lock: ... mode 644 lets other users read it; run chmod 600 on it`); with only
/// `swarm_roster` not calling it, the lock stays wide (`the roster's read narrows the lock`);
/// with `narrow_owned` setting the mode by the lock's name (`std::fs::set_permissions`) in
/// place of its descriptor, the file renamed to that name is narrowed (`the checked lock is
/// narrowed`); with its directory mode check dropped, the lock in a group-writable directory
/// is narrowed (`a lock in a group-writable directory is left`); with
/// `narrow_owned` opening the lock `O_NOFOLLOW` but with no owner check (in place of
/// `open_existing`) and its directory owner check dropped, another user's lock is narrowed
/// (`another user's lock is left`); with it opening the lock by its name
/// (`std::fs::File::open`, which follows a symlink) and checking only the opened file's
/// owner, kind and links, the symlink's target is narrowed (`assertion failed:
/// !StateLock::narrow_owned(&admin_paths)...`). NEGATIVE CONTROL (2026-11-05): with
/// `observe_narrowed` taking no lock as wide, the roster's read fails on it (`assertion failed:
/// swarm_roster_until(&admin_paths, ...`).
#[test]
fn an_own_wide_lock_is_narrowed_by_the_deny_list_read_and_a_foreign_or_linked_one_is_not() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let vault_path = root.path().join("swarm.vault");
    let admin_paths = Paths::for_home(&root.path().join("admin"));
    create(&admin_paths, vault_path.to_str().unwrap(), None, Some("m1")).expect("create");
    let opened = admin_open(&admin_paths).expect("admin open");
    let mut doc = opened.doc.clone();
    doc.set_entry(
        &Audience::Everyone,
        SWARM_ROSTER_ENTRY,
        b"deny_nodes = [\"ff00\"]\n",
    )
    .expect("set roster");
    admin_publish(&admin_paths, &opened, &doc).expect("publish");
    sync(&admin_paths, None).expect("sync");
    let denied = Some(vec!["ff00".to_owned()]);
    let lock = admin_paths.lock_file();
    let mode = |path: &std::path::Path| {
        std::fs::symlink_metadata(path)
            .expect("lock metadata")
            .mode()
            & 0o7777
    };
    let widen = |path: &std::path::Path| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).expect("widen");
    };
    assert_eq!(mode(&lock), 0o600, "a sync leaves the lock private");

    widen(&lock);
    let inode = std::fs::metadata(&lock).expect("lock").ino();
    assert_eq!(
        swarm_denied_nodes(&admin_paths).expect("read past a wide lock"),
        denied
    );
    assert_eq!(mode(&lock), 0o600);
    assert_eq!(
        std::fs::metadata(&lock).expect("lock").ino(),
        inode,
        "narrowed in place"
    );

    widen(&lock);
    assert_eq!(
        super::super::swarm_denied_nodes_now(&admin_paths).expect("memo read"),
        denied
    );
    assert_eq!(mode(&lock), 0o600);
    widen(&lock);
    assert!(
        swarm_roster_until(&admin_paths, Instant::now() + aterm_spec::wait::MUST_HAPPEN).is_ok()
    );
    assert_eq!(mode(&lock), 0o600);
    // The roster's own read narrows it too, past its memo.
    widen(&lock);
    assert!(
        swarm_roster_until(&admin_paths, Instant::now() + aterm_spec::wait::MUST_HAPPEN).is_ok()
    );
    assert_eq!(mode(&lock), 0o600, "the roster's read narrows the lock");

    // Narrowed through the descriptor: a file renamed to the lock's name after the checks
    // keeps its mode, and the lock that was checked is the one narrowed.
    widen(&lock);
    let checked = admin_paths.dir.join("checked-lock");
    let narrowed = StateLock::narrow_owned_then(&admin_paths, || {
        std::fs::rename(&lock, &checked).expect("move the checked lock");
        std::fs::write(&lock, b"").expect("another file at the lock's name");
        widen(&lock);
    });
    assert!(narrowed.expect("narrowed"));
    assert_eq!(mode(&checked), 0o600, "the checked lock is narrowed");
    assert_eq!(mode(&lock), 0o644, "the file now at its name is left");
    std::fs::rename(&checked, &lock).expect("put the lock back");

    // In a directory other users may write, someone else could have renamed one of this
    // user's files to the lock's name: it is left, and refused.
    widen(&lock);
    std::fs::set_permissions(&admin_paths.dir, std::fs::Permissions::from_mode(0o770))
        .expect("open the directory to the group");
    let narrowed = StateLock::narrow_owned(&admin_paths);
    std::fs::set_permissions(&admin_paths.dir, std::fs::Permissions::from_mode(0o700))
        .expect("close the directory");
    assert!(
        !narrowed.expect("nothing to fail"),
        "a lock in a group-writable directory is left"
    );
    assert_eq!(mode(&lock), 0o644);
    assert_eq!(
        swarm_denied_nodes(&admin_paths).expect("narrowed in a private directory"),
        denied
    );

    // Another user's lock is not this user's to narrow.
    widen(&lock);
    let (narrowed, read) = fsx::as_another_user(|| {
        (
            StateLock::narrow_owned(&admin_paths),
            swarm_denied_nodes(&admin_paths),
        )
    });
    assert!(
        !narrowed.expect("nothing to fail"),
        "another user's lock is left"
    );
    assert!(read.is_err(), "another user's lock is refused: {read:?}");
    assert_eq!(mode(&lock), 0o644);

    // A lock that is a symlink, here to this user's own wide file, is never followed.
    let target = admin_paths.dir.join("linked-lock");
    std::fs::rename(&lock, &target).expect("move the lock aside");
    std::os::unix::fs::symlink(&target, &lock).expect("link the lock");
    assert!(!StateLock::narrow_owned(&admin_paths).expect("nothing to fail"));
    assert!(
        swarm_denied_nodes(&admin_paths).is_err(),
        "a linked lock is refused"
    );
    assert_eq!(mode(&target), 0o644, "the link's target is left as it is");
}

#[test]
fn swarm_denied_nodes_preserves_cache_signal_while_locked() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let fresh = Paths::for_home(&root.path().join("fresh"));
    assert_eq!(swarm_denied_nodes(&fresh).unwrap(), Some(Vec::new()));
    let mut initial_writer = Some(StateLock::take(&fresh).expect("hold initial state lock"));
    assert_eq!(
        swarm_denied_nodes(&fresh).unwrap(),
        None,
        "an initial apply holding state before source.toml lands is busy, not empty"
    );
    let fresh_vault = root.path().join("fresh.vault");
    let mut waits = 0;
    let after_initial = swarm_denial_snapshot_until_with(
        &fresh,
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        |_| {
            waits += 1;
            drop(initial_writer.take());
            create(&fresh, fresh_vault.to_str().unwrap(), None, Some("m0")).unwrap();
            let admin = admin_open(&fresh).unwrap();
            let mut doc = admin.doc.clone();
            doc.set_entry(
                &Audience::Everyone,
                SWARM_ROSTER_ENTRY,
                b"deny_nodes = [\"aa00\"]\n",
            )
            .unwrap();
            admin_publish(&fresh, &admin, &doc).unwrap();
        },
    )
    .unwrap();
    assert_eq!(waits, 1);
    assert_eq!(after_initial, Available(vec!["aa00".to_string()]));

    let vault_path = root.path().join("swarm.vault");
    let admin_paths = Paths::for_home(&root.path().join("admin"));
    create(&admin_paths, vault_path.to_str().unwrap(), None, Some("m1")).expect("create");
    assert_eq!(swarm_denied_nodes(&admin_paths).unwrap(), Some(Vec::new()));

    let opened = admin_open(&admin_paths).expect("admin open");
    let mut doc = opened.doc.clone();
    let m1_recipient = machine_identity(&admin_paths)
        .expect("admin identity")
        .recipient()
        .to_bech32();
    let m9_recipient = Identity::from_secret_bytes([0x99; 32])
        .recipient()
        .to_bech32();
    let m10_recipient = Identity::from_secret_bytes([0x98; 32])
        .recipient()
        .to_bech32();
    let roster_toml = format!(
        "deny_labels = [\"m9\"]\ndeny_nodes = [\"aa11bb22cc33dd44\", \"ff00\"]\n\n[[member]]\nrecipient = \"{m1_recipient}\"\nvault_name = \"m1\"\nlabel = \"m1\"\nadmitted_ms = 2\nnode_id = \"9988776655443322\"\n\n[[member]]\nrecipient = \"{m9_recipient}\"\nvault_name = \"m9\"\nlabel = \"m9\"\nadmitted_ms = 1\nnode_id = \"1122334455667788\"\n\n[[member]]\nrecipient = \"{m10_recipient}\"\nvault_name = \"m9\"\nlabel = \"m9\"\nadmitted_ms = 1\nnode_id = \"9988776655443322\"\n"
    );
    doc.set_entry(
        &Audience::Everyone,
        SWARM_ROSTER_ENTRY,
        roster_toml.as_bytes(),
    )
    .expect("set roster");
    admin_publish(&admin_paths, &opened, &doc).expect("publish");
    sync(&admin_paths, None).expect("sync");
    assert_eq!(
        swarm_denied_nodes(&admin_paths).unwrap(),
        Some(vec![
            "1122334455667788".to_string(),
            "aa11bb22cc33dd44".to_string(),
            "ff00".to_string()
        ])
    );

    let _lock = StateLock::take(&admin_paths).expect("hold state lock");
    assert_eq!(
        swarm_denied_nodes(&admin_paths).unwrap(),
        None,
        "mid-apply lock returns None so callers retain their cached deny-list"
    );
}
