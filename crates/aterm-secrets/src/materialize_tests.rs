// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::entry_class::SITE_DEPLOYER_KEY;
use crate::text::Audience;

fn scratch() -> aterm_tempfile::TempDir {
    aterm_tempfile::tempdir().expect("tempdir")
}

fn entry(name: &str, value: &[u8]) -> Entry {
    Entry {
        name: name.to_string(),
        value: SecretBytes::from(value),
        audience: Audience::Everyone,
        if_absent: false,
        anchor: None,
    }
}

/// A missing plain entry is a gap only where a pass writes it: with `~/.secrets` missing or
/// this user's directory. A symlinked `~/.secrets` (to a directory the link makes look
/// writable) is no gap, as the pass beside it writes nothing there; a file in its place
/// neither. Control: the same names in a private `~/.secrets` are gaps, and a pass fills them.
#[test]
fn a_plain_entry_is_a_gap_only_where_a_pass_writes_it() {
    let root = scratch();
    let entries = [entry("token", b"value")];
    let gap = |home: &Path| is_gap(home, "token");

    let missing = root.path().join("missing");
    std::fs::create_dir(&missing).unwrap();
    assert!(gap(&missing), "a pass makes ~/.secrets");

    let linked = root.path().join("linked");
    let elsewhere = root.path().join("elsewhere");
    std::fs::create_dir_all(&linked).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, linked.join(".secrets")).unwrap();
    assert!(!gap(&linked));
    let (_, report) = apply(&linked, &entries, &Manifest::default(), 1).unwrap();
    assert!(report.written.is_empty(), "{report:?}");
    assert!(!elsewhere.join("token").exists());

    let filed = root.path().join("filed");
    std::fs::create_dir(&filed).unwrap();
    std::fs::write(filed.join(".secrets"), b"not a directory").unwrap();
    assert!(!gap(&filed));

    let private = root.path().join("private");
    std::fs::create_dir_all(private.join(".secrets")).unwrap();
    assert!(gap(&private));
    let (_, report) = apply(&private, &entries, &Manifest::default(), 1).unwrap();
    assert_eq!(report.written, ["token"]);
    assert!(!gap(&private), "the pass filled it");
}

#[test]
fn commons_control_is_not_materialized_or_resumed_and_a_changed_copy_stays() {
    let root = scratch();
    let name = crate::commons::ENTRY;
    let entries = [
        entry("ordinary", b"ordinary value"),
        entry(name, b"inert control key"),
        entry("aterm-commons-control-v2", b"inert future control key"),
    ];
    let path = root.path().join(".secrets").join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    plant(&path, b"an existing local file", 0o600);
    let held = Manifest {
        files: BTreeMap::from([(name.into(), digest_hex(b"what an older build wrote"))]),
    };
    let resumed = Pending::from([(name.into(), digest_hex(b"inert control key"))]);
    let (manifest, report) = apply_resuming(
        root.path(),
        &entries,
        &held,
        &resumed,
        1,
        &mut |held, pending| {
            assert_eq!(
                held.files.get(name),
                Some(&digest_hex(b"what an older build wrote")),
                "saved ahead with the record, for a pass cut short"
            );
            assert!(!pending.contains_key(name));
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(read(&path), b"an existing local file");
    assert_eq!(report.written, ["ordinary"]);
    assert_eq!(
        report.left,
        [name],
        "a copy changed since it was written stays"
    );
    assert!(report.removed.is_empty() && report.failed.is_empty() && report.moved_aside.is_empty());
    assert!(!manifest.files.contains_key(name));
    for spelling in [
        name.to_owned(),
        name.to_uppercase(),
        format!("~/.secrets/{name}"),
        format!("~/.SECRETS/{}", name.to_uppercase()),
        "aterm-commons-control-v2".into(),
    ] {
        assert!(target_path(root.path(), &spelling).is_err(), "{spelling}");
    }
    let empty = scratch();
    apply(empty.path(), &entries[1..], &Manifest::default(), 2).unwrap();
    assert!(
        !empty.path().join(".secrets").join(name).exists(),
        "a control record never creates a plaintext key file"
    );
}

/// Reserved names a build that did not reserve them could write as files, by the spelling
/// its manifest held (a `~/.secrets/` spelling was never a valid entry name).
const RESERVED_COPIES: [&str; 5] = [
    "aterm-fleet-code",
    "ATERM-FLEET-CODE",
    crate::commons::ENTRY,
    "aterm-claude-token-ac-0123456789abcdef",
    "aterm-claude-token-ac-0123456789abcdef-meta",
];

#[test]
fn a_reserved_file_an_older_build_wrote_is_removed_while_it_holds_what_it_wrote() {
    for name in RESERVED_COPIES {
        let root = scratch();
        let path = root.path().join(".secrets").join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let held = Manifest {
            files: BTreeMap::from([
                (name.into(), digest_hex(b"inert reserved value")),
                ("ordinary".into(), digest_hex(b"ordinary value")),
            ]),
        };
        plant(
            &root.path().join(".secrets/ordinary"),
            b"ordinary value",
            0o600,
        );
        // Whether the vault still holds the entry (in place now) or dropped it, the file an
        // older build wrote goes, and the manifest forgets the name.
        for entries in [
            vec![
                entry("ordinary", b"ordinary value"),
                entry(name, b"inert reserved value"),
            ],
            vec![entry("ordinary", b"ordinary value")],
        ] {
            plant(&path, b"inert reserved value", 0o600);
            let (manifest, report) = apply(root.path(), &entries, &held, 1).unwrap();
            assert!(!path.exists(), "{name}: {report:?}");
            assert_eq!(report.removed, [name], "{name}");
            assert!(
                report.failed.is_empty() && report.left.is_empty(),
                "{name}: {report:?}"
            );
            assert_eq!(manifest.files.keys().collect::<Vec<_>>(), ["ordinary"]);
            assert_eq!(
                read(&root.path().join(".secrets/ordinary")),
                b"ordinary value"
            );
            let (again, report) = apply(root.path(), &entries, &manifest, 2).unwrap();
            assert_eq!(again, manifest, "{name}");
            assert!(
                report.removed.is_empty() && report.left.is_empty(),
                "{name}: {report:?}"
            );
        }
    }
}

#[test]
fn a_reserved_file_the_person_changed_or_linked_stays_and_is_forgotten() {
    for name in RESERVED_COPIES {
        let root = scratch();
        let path = root.path().join(".secrets").join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let held = Manifest {
            files: BTreeMap::from([(name.into(), digest_hex(b"inert reserved value"))]),
        };
        plant(&path, b"the person's own value", 0o600);
        let (manifest, report) = apply(root.path(), &[], &held, 1).unwrap();
        assert_eq!(read(&path), b"the person's own value", "{name}");
        assert_eq!(report.left, [name], "{name}");
        assert!(
            report.removed.is_empty() && report.failed.is_empty(),
            "{name}"
        );
        assert!(manifest.files.is_empty(), "{name}: {manifest:?}");
        // A link in the file's place is never followed, whatever it reaches.
        std::fs::remove_file(&path).unwrap();
        let reached = root.path().join("reached");
        plant(&reached, b"inert reserved value", 0o600);
        std::os::unix::fs::symlink(&reached, &path).unwrap();
        let (manifest, report) = apply(root.path(), &[], &held, 2).unwrap();
        assert!(
            std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(read(&reached), b"inert reserved value", "{name}");
        assert_eq!(report.left, [name], "{name}");
        assert!(manifest.files.is_empty(), "{name}: {manifest:?}");
    }
}

fn mode(path: &Path) -> u32 {
    std::fs::symlink_metadata(path).expect("meta").mode() & 0o7777
}

fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path).expect("read")
}

fn plant(path: &Path, bytes: &[u8], mode: u32) {
    std::fs::write(path, bytes).expect("write");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

#[test]
fn target_paths() {
    let home = Path::new("/h");
    assert_eq!(
        target_path(home, "gh_token").expect("ok"),
        Path::new("/h/.secrets/gh_token")
    );
    assert_eq!(
        target_path(home, "~/.aterm/release-credentials.toml").expect("ok"),
        Path::new("/h/.aterm/release-credentials.toml")
    );
    assert_eq!(
        target_path(home, "~/.config/gh/hosts.yml").expect("ok"),
        Path::new("/h/.config/gh/hosts.yml")
    );
    assert!(
        target_path(home, "~/x").is_err(),
        "a home path aterm does not name"
    );
    for bad in [
        "",
        "../x",
        "a/b",
        "~/../x",
        "~/a/../../x",
        "/etc/passwd",
        "~/.secrets/x",
        ".x",
        "~/",
    ] {
        assert!(target_path(home, bad).is_err(), "{bad}");
    }
    assert_eq!(aside_name("~/.aterm/x.toml", 9), "~%2F.aterm%2Fx.toml.9");
}

#[test]
fn a_vault_entry_naming_a_cli_login_is_never_written_nor_taken_back() {
    const LOGIN: &str = "~/.codex/auth.json";
    let home = scratch();
    let login = home.path().join(".codex").join("auth.json");
    std::fs::create_dir(home.path().join(".codex")).expect("dir");
    plant(&login, b"this machine's own login", 0o600);
    let (_, report) = apply(
        home.path(),
        &[entry(LOGIN, b"another machine's login"), entry("t", b"v")],
        &Manifest::default(),
        1,
    )
    .expect("applied");
    assert_eq!(read(&login), b"this machine's own login");
    assert_eq!(report.written, ["t"]);
    assert!(report.moved_aside.is_empty(), "{:?}", report.moved_aside);
    assert!(
        report.failed.is_empty(),
        "the rest applied: {:?}",
        report.failed
    );
    assert_eq!(
        report
            .refused
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        [LOGIN]
    );
    // An older build wrote it, and the vault no longer names it: the login standing there
    // is the CLI's own now, never the vault's to remove.
    plant(&login, b"written by an older build", 0o600);
    let held = Manifest {
        files: [(LOGIN.to_string(), digest_hex(b"written by an older build"))].into(),
    };
    let (manifest, report) = apply(home.path(), &[], &held, 2).expect("dropped");
    assert!(report.removed.is_empty(), "{:?}", report.removed);
    assert_eq!(read(&login), b"written by an older build");
    assert!(manifest.files.is_empty(), "{manifest:?}");
    // Still in the vault, it leaves the manifest too, so no later pass counts it as a file
    // the vault wrote.
    let (manifest, report) = apply(
        home.path(),
        &[entry(LOGIN, b"another machine's login")],
        &held,
        3,
    )
    .expect("applied");
    assert!(manifest.files.is_empty(), "{manifest:?}");
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!(read(&login), b"written by an older build");
}

#[test]
fn digests_are_lowercase_hex_sha256() {
    assert_eq!(
        digest_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn a_first_sync_writes_0600_files_and_narrows_the_directory() {
    let home = scratch();
    let secrets = home.path().join(".secrets");
    std::fs::create_dir(&secrets).expect("dir");
    std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o755)).expect("widen");
    let entries = [entry("a", b"one"), entry("b", b""), entry("c", &[0, 1, 2])];
    let (manifest, report) =
        apply(home.path(), &entries, &Manifest::default(), 1).expect("applied");
    assert_eq!(mode(&secrets), 0o700);
    assert_eq!(report.written, ["a", "b", "c"]);
    assert_eq!(
        report,
        Report {
            written: report.written.clone(),
            ..Report::default()
        }
    );
    for e in &entries {
        let path = secrets.join(&e.name);
        assert_eq!(read(&path), e.value.as_slice());
        assert_eq!(mode(&path), 0o600);
        assert_eq!(manifest.files[&e.name], digest_hex(e.value.as_slice()));
    }
    let (again, report) = apply(home.path(), &entries, &manifest, 2).expect("again");
    assert_eq!(again, manifest);
    assert_eq!(report.unchanged, ["a", "b", "c"]);
    assert!(report.written.is_empty() && report.moved_aside.is_empty());
}

#[test]
fn a_pre_existing_different_file_is_moved_aside_with_its_content() {
    let home = scratch();
    let secrets = home.path().join(".secrets");
    std::fs::create_dir(&secrets).expect("dir");
    plant(&secrets.join("gh_token"), b"the old hand-made token", 0o644);
    let (manifest, report) = apply(
        home.path(),
        &[entry("gh_token", b"vault token")],
        &Manifest::default(),
        1700,
    )
    .expect("applied");
    let aside = secrets.join(".replaced").join("gh_token.1700");
    assert_eq!(
        report.moved_aside,
        [("gh_token".to_string(), aside.clone())]
    );
    assert_eq!(report.written, ["gh_token"]);
    assert_eq!(read(&aside), b"the old hand-made token");
    assert_eq!(mode(&secrets.join(".replaced")), 0o700);
    assert_eq!(read(&secrets.join("gh_token")), b"vault token");
    assert_eq!(mode(&secrets.join("gh_token")), 0o600);
    assert_eq!(manifest.files["gh_token"], digest_hex(b"vault token"));
}

#[test]
fn an_equal_file_is_adopted_and_narrowed() {
    let home = scratch();
    let secrets = home.path().join(".secrets");
    std::fs::create_dir(&secrets).expect("dir");
    plant(&secrets.join("t"), b"same", 0o644);
    let (manifest, report) =
        apply(home.path(), &[entry("t", b"same")], &Manifest::default(), 1).expect("applied");
    assert_eq!(report.unchanged, ["t"]);
    assert!(report.written.is_empty() && report.moved_aside.is_empty());
    assert!(
        !manifest.files.contains_key("t"),
        "an adopted file stays the machine's own"
    );
    assert_eq!(mode(&secrets.join("t")), 0o600);
    assert!(!secrets.join(".replaced").exists());
    let (manifest, report) =
        apply(home.path(), &[entry("t", b"same")], &manifest, 2).expect("again");
    assert_eq!(report.unchanged, ["t"]);
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert!(manifest.files.is_empty());
}

#[test]
fn narrowing_never_follows_a_link_swapped_in_after_the_look() {
    let home = scratch();
    let elsewhere = home.path().join("elsewhere");
    plant(&elsewhere, b"same", 0o644);
    let link = home.path().join("t");
    std::os::unix::fs::symlink(&elsewhere, &link).expect("link");
    assert!(narrow(&link).is_err(), "a link is refused");
    assert_eq!(mode(&elsewhere), 0o644, "what it names keeps its mode");
    narrow(&elsewhere).expect("narrowed");
    assert_eq!(mode(&elsewhere), 0o600);
}

#[test]
fn dropping_an_adopted_entry_leaves_the_machines_own_file() {
    let home = scratch();
    let secrets = home.path().join(".secrets");
    std::fs::create_dir(&secrets).expect("dir");
    plant(&secrets.join("token"), b"hand-made", 0o600);
    let entries = [entry("token", b"hand-made"), entry("other", b"o")];
    let (held, _) = apply(home.path(), &entries, &Manifest::default(), 1).expect("first");
    let (manifest, report) = apply(home.path(), &entries[1..], &held, 2).expect("dropped");
    assert!(report.removed.is_empty(), "{:?}", report.removed);
    assert_eq!(read(&secrets.join("token")), b"hand-made");
    assert!(!manifest.files.contains_key("token"));
    let (_, report) = apply(home.path(), &[], &manifest, 3).expect("dropped");
    assert_eq!(report.removed, ["other"]);
    assert!(!secrets.join("other").exists());
    assert_eq!(read(&secrets.join("token")), b"hand-made");
}

/// Release detach (docs/DESIGN-alab-one-mesh-2026-11-05.md §4.9, §10): the pass that drops
/// a kept entry is handed a manifest without it (`aterm secrets keep`), and the file the
/// vault wrote survives with its digest unchanged; the file the vault wrote and that was not
/// kept is taken back (the control).
#[test]
fn a_kept_file_survives_its_entrys_removal_and_an_unkept_one_does_not() {
    let home = scratch();
    let secrets = home.path().join(".secrets");
    let kept = ["~/.aterm/release-ring/machine.key", "signing"];
    let entries = [
        entry(kept[0], b"ring key"),
        entry(kept[1], b"developer id"),
        entry("token", b"dev token"),
    ];
    let (mut held, report) =
        apply(home.path(), &entries, &Manifest::default(), 1).expect("written");
    assert_eq!(report.written.len(), 3, "{report:?}");
    let ring = home.path().join(".aterm/release-ring/machine.key");
    let before = [
        digest_hex(&read(&ring)),
        digest_hex(&read(&secrets.join("signing"))),
    ];
    for name in kept {
        held.files.remove(name).expect("the vault wrote it");
    }
    let (manifest, report) = apply(home.path(), &[], &held, 2).expect("deleted");
    assert_eq!(report.removed, ["token"], "{report:?}");
    assert!(
        !secrets.join("token").exists(),
        "the unkept file is taken back"
    );
    assert!(manifest.files.is_empty(), "{manifest:?}");
    let after = [
        digest_hex(&read(&ring)),
        digest_hex(&read(&secrets.join("signing"))),
    ];
    assert_eq!(after, before, "each kept file is unchanged");
}

#[test]
fn a_new_value_moves_an_adopted_file_aside_first() {
    let home = scratch();
    let secrets = home.path().join(".secrets");
    std::fs::create_dir(&secrets).expect("dir");
    plant(&secrets.join("token"), b"hand-made", 0o600);
    let (held, _) = apply(
        home.path(),
        &[entry("token", b"hand-made")],
        &Manifest::default(),
        1,
    )
    .expect("adopted");
    let (manifest, report) =
        apply(home.path(), &[entry("token", b"rotated")], &held, 2).expect("changed");
    let [(name, aside)] = report.moved_aside.as_slice() else {
        panic!("moved aside: {:?}", report.moved_aside);
    };
    assert_eq!(name, "token");
    assert_eq!(read(aside), b"hand-made");
    assert_eq!(read(&secrets.join("token")), b"rotated");
    assert_eq!(manifest.files["token"], digest_hex(b"rotated"));
}

#[test]
fn an_adoptable_file_is_never_announced_as_a_write() {
    let home = scratch();
    let secrets = home.path().join(".secrets");
    std::fs::create_dir(&secrets).expect("dir");
    plant(&secrets.join("token"), b"hand-made", 0o600);
    let mut told = Vec::new();
    let (_, report) = apply_resuming(
        home.path(),
        &[entry("token", b"hand-made"), entry("new", b"n")],
        &Manifest::default(),
        &Pending::new(),
        1,
        &mut |_, pending| {
            told.extend(pending.keys().cloned());
            Ok(())
        },
    )
    .expect("applied");
    assert_eq!(
        told,
        ["new"],
        "a cut pass must not count the adopted file the vault's"
    );
    assert_eq!(report.unchanged, ["token"]);
    assert_eq!(report.written, ["new"]);
}

#[test]
fn a_change_overwrites_what_the_vault_wrote_but_saves_a_local_edit() {
    let home = scratch();
    let secrets = home.path().join(".secrets");
    let (held, _) = apply(
        home.path(),
        &[entry("a", b"v1"), entry("b", b"v1")],
        &Manifest::default(),
        1,
    )
    .expect("first");
    plant(&secrets.join("b"), b"edited by hand", 0o600);
    let (manifest, report) = apply(
        home.path(),
        &[entry("a", b"v2"), entry("b", b"v2")],
        &held,
        2,
    )
    .expect("second");
    assert_eq!(report.written, ["a", "b"]);
    assert_eq!(
        report.moved_aside,
        [("b".to_string(), secrets.join(".replaced").join("b.2"))]
    );
    assert_eq!(
        read(&secrets.join(".replaced").join("b.2")),
        b"edited by hand"
    );
    assert_eq!(read(&secrets.join("a")), b"v2");
    assert_eq!(read(&secrets.join("b")), b"v2");
    assert_eq!(manifest.files["a"], digest_hex(b"v2"));
}

#[test]
fn a_taken_aside_name_gets_a_numbered_suffix() {
    let home = scratch();
    let secrets = home.path().join(".secrets");
    std::fs::create_dir(&secrets).expect("dir");
    plant(&secrets.join("t"), b"first", 0o600);
    apply(home.path(), &[entry("t", b"v")], &Manifest::default(), 5).expect("first");
    plant(&secrets.join("t"), b"second", 0o600);
    let (_, report) =
        apply(home.path(), &[entry("t", b"w")], &Manifest::default(), 5).expect("second");
    let replaced = secrets.join(".replaced");
    assert_eq!(
        report.moved_aside,
        [("t".to_string(), replaced.join("t.5.1"))]
    );
    assert_eq!(read(&replaced.join("t.5")), b"first");
    assert_eq!(read(&replaced.join("t.5.1")), b"second");
}

#[test]
fn a_dropped_entry_is_removed_only_while_unchanged() {
    let home = scratch();
    let secrets = home.path().join(".secrets");
    let entries = [
        entry("keep", b"k"),
        entry("drop", b"d"),
        entry("edited", b"e"),
        entry("gone", b"g"),
    ];
    let (held, _) = apply(home.path(), &entries, &Manifest::default(), 1).expect("first");
    plant(&secrets.join("edited"), b"local edit", 0o600);
    std::fs::remove_file(secrets.join("gone")).expect("rm");
    let (manifest, report) = apply(home.path(), &entries[..1], &held, 2).expect("second");
    assert_eq!(report.removed, ["drop"]);
    assert_eq!(report.left, ["edited"]);
    assert_eq!(report.unchanged, ["keep"]);
    assert!(!secrets.join("drop").exists());
    assert_eq!(read(&secrets.join("edited")), b"local edit");
    assert_eq!(manifest.files.keys().collect::<Vec<_>>(), ["keep"]);
    let (_, report) = apply(home.path(), &entries[..1], &manifest, 3).expect("third");
    assert!(report.left.is_empty() && report.removed.is_empty());
    assert_eq!(
        read(&secrets.join("edited")),
        b"local edit",
        "a file the vault let go of stays"
    );
}

#[test]
fn a_dropped_file_whose_unlink_is_refused_keeps_its_bytes_for_the_next_pass() {
    let home = scratch();
    let name = "~/.config/gh/hosts.yml";
    let (held, _) =
        apply(home.path(), &[entry(name, b"v")], &Manifest::default(), 1).expect("first");
    let locked = home.path().join(".config/gh");
    let target = locked.join("hosts.yml");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).expect("lock");
    let refused = apply(home.path(), &[], &held, 2);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).expect("unlock");
    let (manifest, report) = refused.expect("applied");
    if fsx::euid() == 0 {
        assert_eq!(report.removed, [name]);
        return;
    }
    assert_eq!(failed_names(&report), [name]);
    assert_eq!(read(&target), b"v", "not zeroed while its name stays");
    assert_eq!(manifest, held, "still what the vault wrote");
    let (manifest, report) = apply(home.path(), &[], &manifest, 3).expect("next");
    assert_eq!(report.removed, [name]);
    assert!(report.left.is_empty() && report.failed.is_empty());
    assert!(manifest.files.is_empty());
    assert!(!target.exists());
}

#[test]
fn a_file_the_vault_never_wrote_is_never_removed() {
    let home = scratch();
    let secrets = home.path().join(".secrets");
    std::fs::create_dir(&secrets).expect("dir");
    plant(&secrets.join("mine"), b"hand-made", 0o600);
    let (_, report) = apply(home.path(), &[], &Manifest::default(), 1).expect("applied");
    assert_eq!(report, Report::default());
    assert_eq!(read(&secrets.join("mine")), b"hand-made");
}

#[test]
fn a_home_path_target_gets_private_parents() {
    let home = scratch();
    let name = "~/.aterm/release-ring/roster/aterm-machines.toml";
    let (manifest, report) = apply(
        home.path(),
        &[entry(name, b"k = 1\n")],
        &Manifest::default(),
        1,
    )
    .expect("applied");
    assert_eq!(report.written, [name]);
    let target = home
        .path()
        .join(".aterm/release-ring/roster/aterm-machines.toml");
    assert_eq!(read(&target), b"k = 1\n");
    assert_eq!(mode(&target), 0o600);
    assert_eq!(mode(&home.path().join(".aterm")), 0o700);
    assert_eq!(mode(&home.path().join(".aterm/release-ring")), 0o700);
    assert_eq!(mode(&home.path().join(".aterm/release-ring/roster")), 0o700);
    assert_eq!(manifest.files[name], digest_hex(b"k = 1\n"));

    let other = scratch();
    let existing = other.path().join(".aterm");
    std::fs::create_dir(&existing).expect("dir");
    std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    plant(&existing.join("machine.toml"), b"old", 0o644);
    let (_, report) = apply(
        other.path(),
        &[entry("~/.aterm/machine.toml", b"new")],
        &Manifest::default(),
        3,
    )
    .expect("applied");
    assert_eq!(mode(&existing), 0o755, "an existing parent keeps its mode");
    let aside = other
        .path()
        .join(".secrets/.replaced/~%2F.aterm%2Fmachine.toml.3");
    assert_eq!(
        report.moved_aside,
        [("~/.aterm/machine.toml".to_string(), aside.clone())]
    );
    assert_eq!(read(&aside), b"old");
    assert_eq!(read(&existing.join("machine.toml")), b"new");
}

#[test]
fn a_symlink_at_the_target_is_replaced_never_followed() {
    let home = scratch();
    let outside = scratch();
    let secrets = home.path().join(".secrets");
    std::fs::create_dir(&secrets).expect("dir");
    let elsewhere = outside.path().join("elsewhere");
    plant(&elsewhere, b"not the vault's", 0o644);
    std::os::unix::fs::symlink(&elsewhere, secrets.join("t")).expect("link");
    let (_, report) = apply(
        home.path(),
        &[entry("t", b"vault value")],
        &Manifest::default(),
        4,
    )
    .expect("applied");
    assert_eq!(read(&elsewhere), b"not the vault's");
    assert_eq!(mode(&elsewhere), 0o644);
    let target = secrets.join("t");
    assert_eq!(fsx::kind(&target).expect("kind"), Kind::File);
    assert_eq!(read(&target), b"vault value");
    assert_eq!(mode(&target), 0o600);
    let aside = secrets.join(".replaced/t.4");
    assert_eq!(report.moved_aside, [("t".to_string(), aside.clone())]);
    assert_eq!(fsx::kind(&aside).expect("kind"), Kind::Symlink);
    assert_eq!(std::fs::read_link(&aside).expect("link"), elsewhere);

    let held = Manifest {
        files: BTreeMap::from([("t".to_string(), digest_hex(b"vault value"))]),
    };
    std::fs::remove_file(&target).expect("rm");
    std::os::unix::fs::symlink(&elsewhere, &target).expect("link");
    let (manifest, report) = apply(home.path(), &[], &held, 5).expect("applied");
    assert_eq!(report.left, ["t"]);
    assert!(manifest.files.is_empty());
    assert_eq!(fsx::kind(&target).expect("kind"), Kind::Symlink);
    assert_eq!(read(&elsewhere), b"not the vault's");
}

#[test]
fn a_directory_in_the_way_fails_that_entry_only() {
    let home = scratch();
    let secrets = home.path().join(".secrets");
    std::fs::create_dir_all(secrets.join("dir")).expect("dir");
    std::fs::write(home.path().join(".config"), b"x").expect("file");
    let held = Manifest {
        files: BTreeMap::from([("dir".to_string(), digest_hex(b"old"))]),
    };
    let entries = [
        entry("dir", b"v"),
        entry("ok", b"v"),
        entry("~/.config/gh/hosts.yml", b"v"),
    ];
    let (manifest, report) = apply(home.path(), &entries, &held, 1).expect("applied");
    assert_eq!(report.written, ["ok"]);
    let failed: Vec<&str> = report
        .failed
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    assert_eq!(failed, ["dir", "~/.config/gh/hosts.yml"]);
    assert!(
        report.failed[0].1.contains("directory"),
        "{}",
        report.failed[0].1
    );
    assert!(
        report.failed[1].1.contains("not a directory"),
        "{}",
        report.failed[1].1
    );
    assert!(secrets.join("dir").is_dir());
    assert_eq!(
        manifest.files["dir"],
        digest_hex(b"old"),
        "a failed entry keeps what the vault wrote"
    );
    assert!(!manifest.files.contains_key("~/.config/gh/hosts.yml"));
}

#[test]
fn a_dropped_name_whose_directory_is_now_a_file_is_forgotten() {
    let home = scratch();
    let name = "~/.config/gh/hosts.yml";
    let (held, _) =
        apply(home.path(), &[entry(name, b"v")], &Manifest::default(), 1).expect("first");
    let foo = home.path().join(".config/gh");
    std::fs::remove_dir_all(&foo).expect("rm");
    plant(&foo, b"a file now", 0o600);
    let (manifest, report) = apply(home.path(), &[], &held, 2).expect("dropped");
    assert_eq!(report, Report::default(), "nothing can be at that path");
    assert!(manifest.files.is_empty());
    assert_eq!(read(&foo), b"a file now");
}

fn folds_case(dir: &Path) -> bool {
    let probe = dir.join("CaseProbe");
    std::fs::write(&probe, b"").expect("probe");
    let folds = fsx::file_id(&dir.join("caseprobe")) == fsx::file_id(&probe);
    std::fs::remove_file(&probe).expect("rm probe");
    folds
}

fn failed_names(report: &Report) -> Vec<&str> {
    report
        .failed
        .iter()
        .map(|(name, _)| name.as_str())
        .collect()
}

/// Two names that fold alike are one file where the volume folds case, as macOS's does: the
/// classification compares names folded, so neither is written on any volume, and both are
/// reported with the other's name (DESIGN-claude-setup-token §3.1). One the vault wrote
/// before is left as it is, out of the manifest. NEGATIVE CONTROL ([`entry_class::UNFOLDED`],
/// names compared byte for byte): on a volume that folds case the first spelling is written
/// and the second fails on its file.
#[test]
fn a_folded_pair_writes_neither_name_and_reports_both() {
    let home = scratch();
    let secrets = home.path().join(".secrets");
    std::fs::create_dir(&secrets).expect("dir");
    plant(&secrets.join("tok"), b"0", 0o600);
    let held = Manifest {
        files: BTreeMap::from([("tok".to_string(), digest_hex(b"0"))]),
    };
    let entries = [entry("Tok", b"1"), entry("bar", b"b"), entry("tok", b"2")];
    let (manifest, report) = apply(home.path(), &entries, &held, 1).expect("applied");
    assert_eq!(report.written, ["bar"]);
    assert!(
        report.failed.is_empty() && report.removed.is_empty(),
        "{report:?}"
    );
    let refused: Vec<(&str, &str)> = report
        .refused
        .iter()
        .map(|(name, why)| (name.as_str(), why.as_str()))
        .collect();
    assert_eq!(refused.len(), 2, "{refused:?}");
    assert_eq!(refused[0].0, "Tok");
    assert!(refused[0].1.contains("`Tok` and `tok`"), "{}", refused[0].1);
    assert_eq!(refused[1].0, "tok");
    assert!(refused[1].1.contains("`tok` and `Tok`"), "{}", refused[1].1);
    assert_eq!(
        read(&secrets.join("tok")),
        b"0",
        "neither written nor taken back"
    );
    assert_eq!(manifest.files.keys().collect::<Vec<_>>(), ["bar"]);

    let unfolded = scratch();
    let secrets = unfolded.path().join(".secrets");
    std::fs::create_dir(&secrets).expect("dir");
    entry_class::UNFOLDED.with(|on| on.set(true));
    let applied = apply(unfolded.path(), &entries, &Manifest::default(), 1);
    entry_class::UNFOLDED.with(|on| on.set(false));
    let (_, report) = applied.expect("applied");
    assert!(report.refused.is_empty(), "CONTROL: {report:?}");
    if folds_case(&secrets) {
        assert_eq!(report.written, ["Tok", "bar"], "CONTROL");
        assert_eq!(failed_names(&report), ["tok"], "CONTROL");
        assert_eq!(read(&secrets.join("tok")), b"1", "CONTROL: one file");
    } else {
        assert_eq!(report.written, ["Tok", "bar", "tok"], "CONTROL");
    }
}

/// A case variant of release material (`GH_ACCESS_TOKEN_ALABSYSTEMS`) is set apart: never
/// written, reported refused, and the real entry is written to its file. NEGATIVE CONTROL
/// ([`entry_class::UNFOLDED`]): without the fold the variant is a name of its own, sorted
/// first as a merged view sorts names, and on a volume that folds case it overwrites the
/// real entry's file, which then fails as landing on the same file.
#[test]
fn a_case_variant_of_release_material_is_refused_and_the_real_entry_written() {
    let variant = "GH_ACCESS_TOKEN_ALABSYSTEMS";
    let entries = [
        entry(variant, b"the variant"),
        entry(entry_class::RELEASE_GH_TOKEN, b"the release token"),
    ];
    let home = scratch();
    let (manifest, report) =
        apply(home.path(), &entries, &Manifest::default(), 1).expect("applied");
    let file = home
        .path()
        .join(".secrets")
        .join(entry_class::RELEASE_GH_TOKEN);
    assert_eq!(read(&file), b"the release token");
    assert_eq!(report.written, [entry_class::RELEASE_GH_TOKEN]);
    assert!(report.failed.is_empty(), "{report:?}");
    assert_eq!(report.refused.len(), 1, "{report:?}");
    assert_eq!(report.refused[0].0, variant);
    assert!(
        report.refused[0].1.contains("release material"),
        "{}",
        report.refused[0].1
    );
    assert_eq!(
        manifest.files.keys().collect::<Vec<_>>(),
        [entry_class::RELEASE_GH_TOKEN]
    );

    let unfolded = scratch();
    entry_class::UNFOLDED.with(|on| on.set(true));
    let applied = apply(unfolded.path(), &entries, &Manifest::default(), 1);
    entry_class::UNFOLDED.with(|on| on.set(false));
    let (_, report) = applied.expect("applied");
    let secrets = unfolded.path().join(".secrets");
    if folds_case(&secrets) {
        assert_eq!(
            read(&secrets.join(entry_class::RELEASE_GH_TOKEN)),
            b"the variant",
            "CONTROL: the variant overwrites the file"
        );
        assert_eq!(
            failed_names(&report),
            [entry_class::RELEASE_GH_TOKEN],
            "CONTROL"
        );
    } else {
        assert_eq!(report.written.len(), 2, "CONTROL: {report:?}");
    }
}

/// Home paths aterm does not name are set apart: never written, a file of the Mac's own there
/// never moved aside or taken back, even one a build before the classification wrote and
/// named in its manifest, and each is reported refused. NEGATIVE CONTROL: the same value
/// under a shared name is written.
#[test]
fn a_home_path_aterm_does_not_name_is_never_written_nor_taken_back() {
    const SET_APART: [&str; 4] = [
        "~/.zshrc",
        "~/Library/LaunchAgents/x.plist",
        "~/.ssh/config",
        "~/.config/git/hooks/x",
    ];
    let home = scratch();
    let mut held = Manifest::default();
    for (i, name) in SET_APART.iter().enumerate() {
        let path = written_path(home.path(), name).expect("valid");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        if i % 2 == 0 {
            plant(&path, b"the vault's old copy", 0o600);
            held.files
                .insert((*name).to_owned(), digest_hex(b"the vault's old copy"));
        }
    }
    let mut entries: Vec<Entry> = SET_APART
        .iter()
        .map(|name| entry(name, b"runs on every Mac"))
        .collect();
    entries.push(entry("shared", b"runs on every Mac"));
    for (entries, pass) in [(&entries[..], 1), (&[][..], 2)] {
        let (manifest, report) = apply(home.path(), entries, &held, pass).expect("applied");
        assert!(
            report.removed.is_empty() && report.failed.is_empty() && report.moved_aside.is_empty(),
            "{report:?}"
        );
        for (i, name) in SET_APART.iter().enumerate() {
            let path = written_path(home.path(), name).expect("valid");
            if i % 2 == 0 {
                assert_eq!(read(&path), b"the vault's old copy", "{name}");
            } else {
                assert!(!path.exists(), "{name}");
            }
            assert!(!manifest.files.contains_key(*name), "{name}");
        }
        if pass == 1 {
            let refused: Vec<&str> = report
                .refused
                .iter()
                .map(|(name, _)| name.as_str())
                .collect();
            assert_eq!(refused, SET_APART);
            assert!(
                report
                    .refused
                    .iter()
                    .all(|(_, why)| why.contains("no vault writes")),
                "{:?}",
                report.refused
            );
            assert_eq!(report.written, ["shared"], "CONTROL");
            assert_eq!(
                read(&home.path().join(".secrets/shared")),
                b"runs on every Mac",
                "CONTROL"
            );
        }
    }
}

fn apple(home: &Path) -> PathBuf {
    let dir = home.join(".aterm/apple");
    std::fs::create_dir_all(&dir).expect("dir");
    dir
}
#[test]
fn a_second_name_for_a_placed_file_is_refused_whatever_its_spelling() {
    let home = scratch();
    let first = apple(home.path()).join("devid-first.key");
    plant(&first, b"v", 0o600);
    std::fs::hard_link(&first, apple(home.path()).join("devid-second.key")).expect("link");
    let (manifest, report) = apply(
        home.path(),
        &[
            entry("~/.aterm/apple/devid-first.key", b"v"),
            entry("~/.aterm/apple/devid-second.key", b"w"),
        ],
        &Manifest::default(),
        1,
    )
    .expect("applied");
    assert_eq!(report.unchanged, ["~/.aterm/apple/devid-first.key"]);
    assert_eq!(failed_names(&report), ["~/.aterm/apple/devid-second.key"]);
    assert!(
        report.failed[0]
            .1
            .contains("`~/.aterm/apple/devid-first.key`"),
        "{:?}",
        report.failed
    );
    assert!(report.moved_aside.is_empty());
    assert_eq!(read(&first), b"v", "never written through the second name");
    assert!(manifest.files.is_empty(), "{:?}", manifest.files);
}

#[test]
fn a_dropped_name_for_another_entrys_file_is_never_wiped() {
    let home = scratch();
    let old = apple(home.path()).join("devid-old.key");
    plant(&old, b"v", 0o600);
    let new = apple(home.path()).join("devid-new.key");
    std::fs::hard_link(&old, &new).expect("link");
    let held = Manifest {
        files: BTreeMap::from([("~/.aterm/apple/devid-old.key".to_string(), digest_hex(b"v"))]),
    };
    let (manifest, report) = apply(
        home.path(),
        &[entry("~/.aterm/apple/devid-new.key", b"v")],
        &held,
        2,
    )
    .expect("applied");
    assert_eq!(report.unchanged, ["~/.aterm/apple/devid-new.key"]);
    assert!(report.removed.is_empty() && report.left.is_empty());
    assert_eq!(read(&new), b"v");
    assert_eq!(read(&old), b"v");
    assert_eq!(
        manifest.files.keys().collect::<Vec<_>>(),
        ["~/.aterm/apple/devid-new.key"]
    );

    let kept = apple(home.path()).join("kept");
    std::fs::hard_link(&new, &kept).expect("link");
    let (_, report) = apply(home.path(), &[], &manifest, 3).expect("dropped");
    assert_eq!(report.removed, ["~/.aterm/apple/devid-new.key"]);
    assert!(!new.exists());
    assert_eq!(read(&kept), [0u8]);
}

#[test]
fn an_entry_respelled_by_case_keeps_what_the_vault_wrote() {
    let home = scratch();
    let secrets = home.path().join(".secrets");
    let (held, _) = apply(
        home.path(),
        &[entry("Token", b"v1")],
        &Manifest::default(),
        1,
    )
    .expect("first");
    let (manifest, report) =
        apply(home.path(), &[entry("token", b"v2")], &held, 2).expect("respelled");
    assert_eq!(report.written, ["token"]);
    assert!(report.failed.is_empty());
    assert_eq!(manifest.files.keys().collect::<Vec<_>>(), ["token"]);
    assert_eq!(read(&secrets.join("token")), b"v2");
    if folds_case(&secrets) {
        assert!(report.moved_aside.is_empty(), "{:?}", report.moved_aside);
        assert!(report.removed.is_empty());
    } else {
        assert_eq!(report.removed, ["Token"]);
        assert!(!secrets.join("Token").exists());
    }
}

fn gap(name: &str, value: &[u8]) -> Entry {
    Entry {
        if_absent: true,
        ..entry(name, value)
    }
}

#[test]
fn an_if_absent_entry_fills_a_gap_but_never_touches_a_machines_own_file() {
    let home = scratch();
    let secrets = home.path().join(".secrets");
    std::fs::create_dir(&secrets).expect("dir");
    let aterm = home.path().join(".aterm");
    std::fs::create_dir(&aterm).expect("dir");
    plant(&aterm.join("machine.key"), b"this machine's own", 0o644);
    plant(&secrets.join("same"), b"v", 0o644);
    let elsewhere = home.path().join("elsewhere");
    plant(&elsewhere, b"linked", 0o600);
    std::os::unix::fs::symlink(&elsewhere, secrets.join("link")).expect("link");
    std::fs::create_dir(secrets.join("dir")).expect("dir");
    let entries = [
        gap("~/.aterm/machine.key", b"the vault's"),
        gap("~/.aterm/apple/devid-missing.key", b"filled"),
        gap("same", b"v"),
        gap("link", b"v"),
        gap("dir", b"v"),
    ];
    let (manifest, report) =
        apply(home.path(), &entries, &Manifest::default(), 3).expect("applied");
    assert_eq!(report.written, ["~/.aterm/apple/devid-missing.key"]);
    assert_eq!(report.left, ["~/.aterm/machine.key", "same", "link", "dir"]);
    assert!(report.moved_aside.is_empty() && report.failed.is_empty());
    assert_eq!(read(&aterm.join("machine.key")), b"this machine's own");
    assert_eq!(mode(&aterm.join("machine.key")), 0o644, "not even narrowed");
    assert_eq!(
        mode(&secrets.join("same")),
        0o644,
        "an equal file is not adopted"
    );
    assert_eq!(
        fsx::kind(&secrets.join("link")).expect("kind"),
        Kind::Symlink
    );
    assert_eq!(read(&elsewhere), b"linked");
    assert_eq!(read(&aterm.join("apple/devid-missing.key")), b"filled");
    assert_eq!(mode(&aterm.join("apple/devid-missing.key")), 0o600);
    assert_eq!(
        manifest.files.keys().collect::<Vec<_>>(),
        ["~/.aterm/apple/devid-missing.key"],
        "a file left alone is not the vault's"
    );
    assert!(!secrets.join(REPLACED_DIR).exists());

    let (again, report) = apply(home.path(), &entries, &manifest, 4).expect("again");
    assert_eq!(again, manifest);
    assert_eq!(report.unchanged, ["~/.aterm/apple/devid-missing.key"]);
    assert_eq!(report.left.len(), 4);
    let (last, report) = apply(home.path(), &[], &again, 5).expect("dropped");
    assert_eq!(report.removed, ["~/.aterm/apple/devid-missing.key"]);
    assert!(report.left.is_empty() && report.failed.is_empty());
    assert!(last.files.is_empty());
    assert_eq!(read(&aterm.join("machine.key")), b"this machine's own");
    assert_eq!(read(&secrets.join("same")), b"v");
}

#[test]
fn an_if_absent_file_the_vault_wrote_is_the_machines_own_once_changed() {
    let home = scratch();
    let secrets = home.path().join(".secrets");
    let config = home.path().join(".config/git/config");
    let version = |v: &[u8]| {
        [
            gap("a", v),
            gap("~/.config/git/config", v),
            gap("c", v),
            gap("d", v),
        ]
    };
    let (held, report) =
        apply(home.path(), &version(b"v1"), &Manifest::default(), 1).expect("first");
    assert_eq!(report.written, ["a", "~/.config/git/config", "c", "d"]);
    plant(&config, b"edited since", 0o644);
    std::fs::remove_file(secrets.join("c")).expect("rm");
    let elsewhere = home.path().join("elsewhere");
    plant(&elsewhere, b"linked", 0o600);
    std::fs::remove_file(secrets.join("d")).expect("rm");
    std::os::unix::fs::symlink(&elsewhere, secrets.join("d")).expect("link");
    let (manifest, report) = apply(home.path(), &version(b"v2"), &held, 2).expect("second");
    assert_eq!(report.written, ["a", "c"]);
    assert_eq!(report.left, ["~/.config/git/config", "d"]);
    assert!(report.moved_aside.is_empty() && report.failed.is_empty());
    assert!(!secrets.join(REPLACED_DIR).exists(), "nothing moved aside");
    assert_eq!(read(&secrets.join("a")), b"v2");
    assert_eq!(read(&secrets.join("c")), b"v2");
    assert_eq!(read(&config), b"edited since");
    assert_eq!(mode(&config), 0o644, "not even narrowed");
    assert_eq!(fsx::kind(&secrets.join("d")).expect("kind"), Kind::Symlink);
    assert_eq!(read(&elsewhere), b"linked");
    assert_eq!(
        manifest.files.keys().collect::<Vec<_>>(),
        ["a", "c"],
        "a changed file is the machine's own now"
    );

    let (manifest, report) = apply(home.path(), &version(b"v3"), &manifest, 3).expect("third");
    assert_eq!(report.written, ["a", "c"]);
    assert_eq!(report.left, ["~/.config/git/config", "d"]);
    assert_eq!(read(&config), b"edited since");
    let (rest, report) = apply(home.path(), &[], &manifest, 4).expect("unset");
    assert_eq!(report.removed, ["a", "c"]);
    assert!(report.left.is_empty() && report.failed.is_empty());
    assert!(rest.files.is_empty());
    assert_eq!(read(&config), b"edited since");
    assert_eq!(fsx::kind(&secrets.join("d")).expect("kind"), Kind::Symlink);
}

#[test]
fn an_if_absent_entry_cannot_claim_a_file_another_entry_placed() {
    let home = scratch();
    let first = apple(home.path()).join("devid-first.key");
    plant(&first, b"v", 0o600);
    std::fs::hard_link(&first, apple(home.path()).join("devid-second.key")).expect("link");
    let (_, report) = apply(
        home.path(),
        &[
            entry("~/.aterm/apple/devid-first.key", b"v"),
            gap("~/.aterm/apple/devid-second.key", b"w"),
        ],
        &Manifest::default(),
        1,
    )
    .expect("applied");
    assert_eq!(failed_names(&report), ["~/.aterm/apple/devid-second.key"]);
    assert!(report.left.is_empty());
}

fn anchored(name: &str, value: &[u8], anchor: &str) -> Entry {
    Entry {
        anchor: Some(anchor.to_string()),
        ..gap(name, value)
    }
}

const KEY: &str = "~/.aterm/machine.key";
const TOML: &str = "~/.aterm/machine.toml";
const PROFILE: &str = "~/.aterm/release-credentials.toml";

fn identity(value: &[u8]) -> [Entry; 3] {
    [
        anchored(KEY, value, TOML),
        anchored(TOML, value, PROFILE),
        anchored(PROFILE, value, KEY),
    ]
}

#[test]
fn a_machines_own_machine_toml_keeps_the_vaults_key_and_profile_away() {
    let home = scratch();
    let at = |name: &str| target_path(home.path(), name).expect("valid");
    std::fs::create_dir(home.path().join(".aterm")).expect("dir");
    plant(&at(TOML), b"its own id", 0o600);
    let (manifest, report) =
        apply(home.path(), &identity(b"shared"), &Manifest::default(), 1).expect("applied");
    assert_eq!(report.left, [KEY, TOML, PROFILE]);
    assert!(report.written.is_empty() && report.failed.is_empty());
    assert!(manifest.files.is_empty());
    assert!(!at(KEY).exists() && !at(PROFILE).exists());
    assert_eq!(read(&at(TOML)), b"its own id");
}

#[test]
fn a_pass_cut_short_after_its_writes_keeps_the_anchored_set() {
    let home = scratch();
    let at = |name: &str| target_path(home.path(), name).expect("valid");
    let (held, _) = apply(home.path(), &identity(b"v1"), &Manifest::default(), 1).expect("first");
    plant(&at(PROFILE), b"v2", 0o600);
    let (manifest, report) = apply(home.path(), &identity(b"v2"), &held, 2).expect("again");
    assert!(report.removed.is_empty(), "{:?}", report.removed);
    assert!(report.left.is_empty() && report.failed.is_empty());
    for name in [KEY, TOML, PROFILE] {
        assert_eq!(read(&at(name)), b"v2", "{name}");
        assert_eq!(manifest.files[name], digest_hex(b"v2"), "{name}");
    }
}

#[test]
fn a_temp_a_pass_cut_short_left_holding_its_value_is_wiped() {
    let home = scratch();
    let secrets = home.path().join(".secrets");
    let (held, _) =
        apply(home.path(), &[entry("t", b"v1")], &Manifest::default(), 1).expect("first");
    let cut = secrets.join(".t.tmp.4242.0badcafe");
    plant(&cut, b"v2", 0o600);
    plant(&secrets.join("f"), b"filled", 0o600);
    let linked = secrets.join(".f.tmp.4242.12345678");
    std::fs::hard_link(secrets.join("f"), &linked).expect("link");
    let other = secrets.join(".t.tmp.4343.deadbeef");
    plant(&other, b"another writer's", 0o600);
    let resumed = Pending::from([
        ("t".to_string(), digest_hex(b"v2")),
        ("f".to_string(), digest_hex(b"filled")),
    ]);
    let entries = [entry("t", b"v2"), gap("f", b"filled")];
    let (_, report) = apply_resuming(
        home.path(),
        &entries,
        &held,
        &resumed,
        2,
        &mut |_, _| Ok(()),
    )
    .expect("resumed");
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert!(!cut.exists(), "the cut pass's copy of the value is gone");
    assert!(!linked.exists());
    assert_eq!(
        read(&secrets.join("f")),
        b"filled",
        "never zeroed through it"
    );
    assert_eq!(read(&other), b"another writer's");
    assert_eq!(read(&secrets.join("t")), b"v2");
}

#[test]
fn a_cut_temp_that_names_another_file_loses_only_its_name() {
    let home = scratch();
    let secrets = home.path().join(".secrets");
    std::fs::create_dir(&secrets).expect("dir");
    std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o700)).expect("0700");
    plant(&secrets.join("t.tmp.1"), b"shared value", 0o600);
    let theirs = secrets.join(".t.tmp.1.tmp.4242.12345678");
    std::fs::hard_link(secrets.join("t.tmp.1"), &theirs).expect("link");
    plant(&secrets.join("g-kept"), b"shared value", 0o600);
    let moved = secrets.join(".g.tmp.4242.87654321");
    std::fs::hard_link(secrets.join("g-kept"), &moved).expect("link");
    let resumed = Pending::from(
        ["t", "t.tmp.1", "g"].map(|name| (name.to_string(), digest_hex(b"shared value"))),
    );
    let entries = [
        entry("t", b"shared value"),
        gap("t.tmp.1", b"shared value"),
        gap("g", b"shared value"),
    ];
    let (manifest, report) = apply_resuming(
        home.path(),
        &entries,
        &Manifest::default(),
        &resumed,
        2,
        &mut |_, _| Ok(()),
    )
    .expect("resumed");
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert!(!theirs.exists() && !moved.exists());
    assert_eq!(read(&secrets.join("t.tmp.1")), b"shared value");
    assert_eq!(read(&secrets.join("g-kept")), b"shared value");
    assert!(report.left.is_empty(), "{:?}", report.left);
    assert_eq!(manifest.files.len(), 3);
}

#[test]
fn a_gap_taken_while_the_pass_writes_fails_so_the_next_pass_takes_the_set_back() {
    let home = scratch();
    let at = |name: &str| target_path(home.path(), name).expect("valid");
    let (manifest, report) = apply_resuming(
        home.path(),
        &identity(b"shared"),
        &Manifest::default(),
        &Pending::new(),
        1,
        &mut |_, pending| {
            assert_eq!(pending.len(), 3);
            std::fs::create_dir(home.path().join(".aterm")).expect("dir");
            plant(&at(TOML), b"its own id", 0o600);
            Ok(())
        },
    )
    .expect("applied");
    assert_eq!(failed_names(&report), [TOML]);
    assert_eq!(read(&at(TOML)), b"its own id");
    assert!(!manifest.files.contains_key(TOML));

    let (manifest, report) = apply(home.path(), &identity(b"shared"), &manifest, 2).expect("next");
    assert_eq!(report.removed, [KEY, PROFILE]);
    assert_eq!(report.left, [TOML]);
    assert!(report.failed.is_empty() && report.written.is_empty());
    assert!(manifest.files.is_empty());
    assert!(!at(KEY).exists() && !at(PROFILE).exists());
    assert_eq!(read(&at(TOML)), b"its own id");
}

#[test]
fn a_file_the_vault_wrote_changed_while_the_pass_writes_fails_so_the_next_pass_takes_the_set_back()
{
    let home = scratch();
    let at = |name: &str| target_path(home.path(), name).expect("valid");
    let (held, _) = apply(home.path(), &identity(b"v1"), &Manifest::default(), 1).expect("first");
    let (manifest, report) = apply_resuming(
        home.path(),
        &identity(b"v2"),
        &held,
        &Pending::new(),
        2,
        &mut |_, pending| {
            assert_eq!(pending.len(), 3);
            plant(&at(TOML), b"its own id", 0o600);
            Ok(())
        },
    )
    .expect("applied");
    assert_eq!(failed_names(&report), [TOML]);
    assert_eq!(read(&at(TOML)), b"its own id");

    let (manifest, report) = apply(home.path(), &identity(b"v2"), &manifest, 3).expect("next");
    assert_eq!(report.removed, [KEY, PROFILE]);
    assert_eq!(report.left, [TOML]);
    assert!(report.failed.is_empty() && report.written.is_empty());
    assert!(manifest.files.is_empty());
    assert!(!at(KEY).exists() && !at(PROFILE).exists());
    assert_eq!(read(&at(TOML)), b"its own id");
}

#[test]
fn an_anchored_set_fills_a_machine_that_has_none_of_it_whole() {
    let home = scratch();
    let at = |name: &str| target_path(home.path(), name).expect("valid");
    let (manifest, report) =
        apply(home.path(), &identity(b"shared"), &Manifest::default(), 1).expect("applied");
    assert_eq!(report.written, [KEY, TOML, PROFILE]);
    assert!(report.left.is_empty() && report.failed.is_empty());
    for name in [KEY, TOML, PROFILE] {
        assert_eq!(read(&at(name)), b"shared", "{name}");
        assert_eq!(manifest.files[name], digest_hex(b"shared"), "{name}");
    }

    std::fs::remove_file(at(PROFILE)).expect("rm");
    let (manifest, report) = apply(home.path(), &identity(b"next"), &manifest, 2).expect("next");
    assert_eq!(report.written, [KEY, TOML, PROFILE]);
    assert_eq!(read(&at(PROFILE)), b"next");
    assert_eq!(manifest.files.len(), 3);

    plant(&at(KEY), b"its own key", 0o600);
    std::fs::remove_file(at(PROFILE)).expect("rm");
    let (manifest, report) = apply(home.path(), &identity(b"next"), &manifest, 3).expect("own");
    assert_eq!(report.removed, [TOML]);
    assert_eq!(report.left, [KEY, PROFILE]);
    assert!(report.written.is_empty() && report.unchanged.is_empty());
    assert!(report.moved_aside.is_empty() && report.failed.is_empty());
    assert_eq!(read(&at(KEY)), b"its own key");
    assert!(!at(PROFILE).exists());
    assert!(!at(TOML).exists());
    assert!(manifest.files.is_empty());
}

#[test]
fn a_profile_the_vault_filled_beside_a_machines_own_key_is_taken_back() {
    let home = scratch();
    let at = |name: &str| target_path(home.path(), name).expect("valid");
    std::fs::create_dir(home.path().join(".aterm")).expect("dir");
    plant(&at(KEY), b"its own key", 0o600);
    plant(&at(TOML), b"its own id", 0o600);
    let (held, report) = apply(
        home.path(),
        &[gap(KEY, b"v1"), gap(TOML, b"v1"), gap(PROFILE, b"v1")],
        &Manifest::default(),
        1,
    )
    .expect("unanchored");
    assert_eq!(report.written, [PROFILE], "what the defect did");
    assert_eq!(held.files.keys().collect::<Vec<_>>(), [PROFILE]);

    let (manifest, report) = apply(home.path(), &identity(b"v2"), &held, 2).expect("anchored");
    assert_eq!(report.removed, [PROFILE]);
    assert_eq!(report.left, [KEY, TOML]);
    assert!(report.written.is_empty() && report.unchanged.is_empty());
    assert!(report.moved_aside.is_empty() && report.failed.is_empty());
    assert!(!at(PROFILE).exists());
    assert_eq!(read(&at(KEY)), b"its own key");
    assert_eq!(read(&at(TOML)), b"its own id");
    assert!(manifest.files.is_empty());
    assert!(!home.path().join(".secrets").join(REPLACED_DIR).exists());

    let (manifest, report) = apply(home.path(), &identity(b"v3"), &manifest, 3).expect("again");
    assert_eq!(report.left, [KEY, TOML, PROFILE]);
    assert!(report.written.is_empty() && report.removed.is_empty());
    assert!(manifest.files.is_empty());
    assert!(!at(PROFILE).exists());

    let other = scratch();
    let at = |name: &str| target_path(other.path(), name).expect("valid");
    std::fs::create_dir(other.path().join(".aterm")).expect("dir");
    plant(&at(KEY), b"its own key", 0o600);
    let (held, _) = apply(
        other.path(),
        &[gap(KEY, b"v1"), gap(PROFILE, b"v1")],
        &Manifest::default(),
        1,
    )
    .expect("unanchored");
    plant(&at(PROFILE), b"edited since", 0o600);
    let (manifest, report) = apply(other.path(), &identity(b"v2"), &held, 2).expect("anchored");
    assert!(report.removed.is_empty(), "{:?}", report.removed);
    assert_eq!(read(&at(PROFILE)), b"edited since");
    assert!(manifest.files.is_empty());
}

#[test]
fn a_key_and_machine_toml_the_vault_filled_beside_a_machines_own_profile_are_taken_back() {
    let home = scratch();
    let at = |name: &str| target_path(home.path(), name).expect("valid");
    std::fs::create_dir(home.path().join(".aterm")).expect("dir");
    plant(&at(PROFILE), b"its own profile", 0o600);
    let (held, report) = apply(
        home.path(),
        &[gap(KEY, b"v1"), gap(TOML, b"v1"), gap(PROFILE, b"v1")],
        &Manifest::default(),
        1,
    )
    .expect("unanchored");
    assert_eq!(report.written, [KEY, TOML], "what the defect did");
    let (manifest, report) = apply(home.path(), &identity(b"v1"), &held, 2).expect("anchored");
    assert_eq!(report.removed, [KEY, TOML]);
    assert_eq!(report.left, [PROFILE]);
    assert!(report.unchanged.is_empty() && report.failed.is_empty());
    assert!(!at(KEY).exists() && !at(TOML).exists());
    assert_eq!(read(&at(PROFILE)), b"its own profile");
    assert!(manifest.files.is_empty());
}

#[test]
fn a_file_the_vault_wrote_beside_an_anchor_it_cannot_look_at_stays_the_vaults() {
    let home = scratch();
    let outside = scratch();
    let anchor = "~/.config/gh/hosts.yml";
    let (held, report) = apply(
        home.path(),
        &[gap("profile", b"v1")],
        &Manifest::default(),
        1,
    )
    .expect("first");
    assert_eq!(report.written, ["profile"]);
    std::os::unix::fs::symlink(outside.path(), home.path().join(".config")).expect("link");
    let entries = [gap(anchor, b"v2"), anchored("profile", b"v2", anchor)];
    let (manifest, report) = apply(home.path(), &entries, &held, 2).expect("unseen");
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!(report.left, [anchor, "profile"]);
    assert!(report.written.is_empty() && report.removed.is_empty());
    assert_eq!(read(&home.path().join(".secrets/profile")), b"v1");
    assert_eq!(manifest, held, "still what the vault wrote");
}

#[test]
fn a_machines_own_key_keeps_the_vaults_profile_and_machine_toml_away() {
    let home = scratch();
    let aterm = home.path().join(".aterm");
    std::fs::create_dir(&aterm).expect("dir");
    plant(&aterm.join("machine.key"), b"its own key", 0o600);
    let (manifest, report) =
        apply(home.path(), &identity(b"shared"), &Manifest::default(), 1).expect("applied");
    assert_eq!(report.left, [KEY, TOML, PROFILE]);
    assert!(report.written.is_empty() && report.failed.is_empty());
    assert!(manifest.files.is_empty());
    assert_eq!(read(&aterm.join("machine.key")), b"its own key");
    assert!(!aterm.join("machine.toml").exists());
    assert!(!aterm.join("release-credentials.toml").exists());

    plant(&aterm.join("machine.toml"), b"its own id", 0o600);
    let (manifest, report) = apply(home.path(), &identity(b"shared"), &manifest, 2).expect("again");
    assert_eq!(report.left, [KEY, TOML, PROFILE]);
    assert!(manifest.files.is_empty());
    assert!(!aterm.join("release-credentials.toml").exists());
}

#[test]
fn a_machines_own_profile_keeps_the_vaults_key_away() {
    let home = scratch();
    let aterm = home.path().join(".aterm");
    std::fs::create_dir(&aterm).expect("dir");
    plant(
        &aterm.join("release-credentials.toml"),
        b"its own profile",
        0o600,
    );
    let (manifest, report) =
        apply(home.path(), &identity(b"shared"), &Manifest::default(), 1).expect("applied");
    assert_eq!(report.left, [KEY, TOML, PROFILE]);
    assert!(report.written.is_empty() && report.failed.is_empty());
    assert!(manifest.files.is_empty());
    assert!(!aterm.join("machine.key").exists());
    assert!(!aterm.join("machine.toml").exists());
    assert_eq!(
        read(&aterm.join("release-credentials.toml")),
        b"its own profile"
    );
}

#[test]
fn an_anchor_the_machine_cannot_judge_fills_nothing() {
    let home = scratch();
    let outside = scratch();
    std::os::unix::fs::symlink(outside.path(), home.path().join(".config")).expect("link");
    let entries = [
        anchored("a", b"v", "missing"),
        entry("plain", b"v"),
        anchored("b", b"v", "plain"),
        gap("~/.config/gh/hosts.yml", b"v"),
        anchored("c", b"v", "~/.config/gh/hosts.yml"),
    ];
    let (manifest, report) =
        apply(home.path(), &entries, &Manifest::default(), 1).expect("applied");
    assert_eq!(report.written, ["plain"]);
    assert_eq!(failed_names(&report), ["a", "b"]);
    assert!(
        report.failed[0].1.contains("anchor `missing`"),
        "{:?}",
        report.failed
    );
    assert!(
        report.failed[1].1.contains("anchor `plain`"),
        "{:?}",
        report.failed
    );
    assert_eq!(report.left, ["~/.config/gh/hosts.yml", "c"]);
    let secrets = home.path().join(".secrets");
    for name in ["a", "b", "c"] {
        assert!(!secrets.join(name).exists(), "{name}");
    }
    assert!(!outside.path().join("gh/hosts.yml").exists());
    assert_eq!(manifest.files.keys().collect::<Vec<_>>(), ["plain"]);
}

#[test]
fn a_symlinked_directory_on_the_way_is_refused_never_followed() {
    let home = scratch();
    let outside = scratch();
    std::os::unix::fs::symlink(outside.path(), home.path().join(".aterm")).expect("link");
    let real = home.path().join("real");
    std::fs::create_dir_all(real.join("deep")).expect("dir");
    std::fs::create_dir(home.path().join("company-life")).expect("dir");
    std::os::unix::fs::symlink(real.join("deep"), home.path().join("company-life/config"))
        .expect("link");
    let entries = [
        entry("~/.aterm/machine.toml", b"v"),
        entry(SITE_DEPLOYER_KEY, b"v"),
        entry("~/.config/gh/hosts.yml", b"v"),
    ];
    let (manifest, report) =
        apply(home.path(), &entries, &Manifest::default(), 1).expect("applied");
    assert_eq!(report.written, ["~/.config/gh/hosts.yml"]);
    assert_eq!(
        failed_names(&report),
        ["~/.aterm/machine.toml", SITE_DEPLOYER_KEY]
    );
    let aterm = home.path().join(".aterm").display().to_string();
    assert!(report.failed[0].1.contains(&aterm), "{:?}", report.failed);
    assert!(
        report.failed[0].1.contains("symlink"),
        "{:?}",
        report.failed
    );
    let b = home
        .path()
        .join("company-life/config")
        .display()
        .to_string();
    assert!(report.failed[1].1.contains(&b), "{:?}", report.failed);
    assert!(!outside.path().join("machine.toml").exists());
    assert!(
        !real.join("deep/local").exists(),
        "no directory made through the link"
    );
    assert_eq!(
        manifest.files.keys().collect::<Vec<_>>(),
        ["~/.config/gh/hosts.yml"]
    );

    plant(
        &outside.path().join("release-credentials.toml"),
        b"o",
        0o600,
    );
    let held = Manifest {
        files: BTreeMap::from([(
            "~/.aterm/release-credentials.toml".to_string(),
            digest_hex(b"o"),
        )]),
    };
    let (manifest, report) = apply(home.path(), &[], &held, 2).expect("dropped");
    assert_eq!(report.left, ["~/.aterm/release-credentials.toml"]);
    assert!(report.removed.is_empty() && report.failed.is_empty());
    assert_eq!(manifest, held, "still what the vault wrote");
    assert_eq!(read(&outside.path().join("release-credentials.toml")), b"o");
    let (again, report) = apply(home.path(), &[], &manifest, 3).expect("again");
    assert_eq!(report.left, ["~/.aterm/release-credentials.toml"]);
    assert_eq!(again, held, "kept however many syncs meet the link");

    std::fs::remove_file(home.path().join(".aterm")).expect("unlink");
    std::fs::create_dir(home.path().join(".aterm")).expect("dir");
    plant(
        &home.path().join(".aterm/release-credentials.toml"),
        b"o",
        0o600,
    );
    let (last, report) = apply(home.path(), &[], &again, 4).expect("reached");
    assert_eq!(report.removed, ["~/.aterm/release-credentials.toml"]);
    assert!(report.left.is_empty());
    assert!(last.files.is_empty());
    assert!(!home.path().join(".aterm/release-credentials.toml").exists());
}

#[test]
fn aterms_own_directory_by_another_spelling_is_refused() {
    let home = scratch();
    // aterm's own directory is the site deployer key's, by a link in its own place.
    let own = home.path().join("company-life/config/local");
    std::fs::create_dir_all(&own).expect("dir");
    std::fs::create_dir_all(home.path().join(".config/aterm")).expect("dir");
    std::os::unix::fs::symlink(&own, home.path().join(".config/aterm/secrets")).expect("link");
    plant(&own.join("identity"), b"the machine key", 0o600);
    let name = SITE_DEPLOYER_KEY;
    let (manifest, report) = apply(
        home.path(),
        &[entry(name, b"v"), entry("~/.config/gh/hosts.yml", b"v")],
        &Manifest::default(),
        1,
    )
    .expect("applied");
    assert_eq!(failed_names(&report), [name]);
    assert!(
        report.failed[0].1.contains("aterm's own"),
        "{:?}",
        report.failed
    );
    assert_eq!(report.written, ["~/.config/gh/hosts.yml"]);
    assert!(report.moved_aside.is_empty());
    assert_eq!(read(&own.join("identity")), b"the machine key");
    assert_eq!(
        manifest.files.keys().collect::<Vec<_>>(),
        ["~/.config/gh/hosts.yml"]
    );

    let held = Manifest {
        files: BTreeMap::from([(name.to_string(), digest_hex(b"the machine key"))]),
    };
    let (manifest, report) = apply(home.path(), &[], &held, 2).expect("dropped");
    assert_eq!(report.left, [name]);
    assert_eq!(manifest, held);
    assert_eq!(read(&own.join("identity")), b"the machine key");
}

#[test]
fn a_symlinked_secrets_dir_fails_its_entries_and_spares_the_rest() {
    let home = scratch();
    let real = home.path().join("real");
    std::fs::create_dir(&real).expect("dir");
    let secrets = home.path().join(".secrets");
    std::os::unix::fs::symlink(&real, &secrets).expect("link");
    plant(&real.join("dropped"), b"d", 0o600);
    std::fs::create_dir_all(home.path().join(".config/git")).expect("dir");
    plant(
        &home.path().join(".config/git/credentials"),
        b"hand-made",
        0o600,
    );
    let held = Manifest {
        files: BTreeMap::from([
            ("a".to_string(), digest_hex(b"old")),
            ("dropped".to_string(), digest_hex(b"d")),
        ]),
    };
    let entries = [
        entry("a", b"v"),
        entry("~/.config/gh/hosts.yml", b"v"),
        entry("~/.config/git/credentials", b"v"),
    ];
    let (manifest, report) = apply(home.path(), &entries, &held, 1).expect("applied");
    assert_eq!(report.written, ["~/.config/gh/hosts.yml"]);
    assert_eq!(failed_names(&report), ["a", "~/.config/git/credentials"]);
    for (_, why) in &report.failed {
        assert!(
            why.contains(&secrets.display().to_string()) && why.contains("symlink"),
            "{why}"
        );
    }
    assert_eq!(report.left, ["dropped"]);
    assert!(report.moved_aside.is_empty() && report.removed.is_empty());
    assert!(!real.join("a").exists());
    assert!(!real.join(REPLACED_DIR).exists());
    assert_eq!(read(&real.join("dropped")), b"d");
    assert_eq!(
        read(&home.path().join(".config/git/credentials")),
        b"hand-made",
        "not overwritten when it cannot be moved aside"
    );
    assert_eq!(
        manifest.files.get("a"),
        Some(&digest_hex(b"old")),
        "a failed entry keeps what the vault wrote"
    );
    assert_eq!(
        manifest.files.get("dropped"),
        Some(&digest_hex(b"d")),
        "a dropped name it could not reach stays the vault's"
    );
    assert_eq!(fsx::kind(&secrets).expect("kind"), Kind::Symlink);

    std::fs::remove_file(&secrets).expect("unlink");
    std::fs::rename(&real, &secrets).expect("move back");
    let (last, report) = apply(home.path(), &entries, &manifest, 2).expect("reached");
    assert_eq!(report.removed, ["dropped"]);
    assert!(report.failed.is_empty() && report.left.is_empty());
    assert!(!secrets.join("dropped").exists());
    assert_eq!(read(&secrets.join("a")), b"v");
    assert!(!last.files.contains_key("dropped"));
}

/// An entry behind a symlinked directory is never written through it. A plain entry, and
/// any entry in a symlinked `~/.secrets`, is a failure; an ` if-absent` home file there is
/// the machine's own (a dotfiles setup's `~/.config`), left, and kept in the manifest
/// when the vault wrote it.
#[test]
fn an_entry_behind_a_symlinked_directory_is_refused_before_any_look() {
    let home = scratch();
    let real = home.path().join("real");
    std::fs::create_dir(&real).expect("dir");
    std::os::unix::fs::symlink(&real, home.path().join(".secrets")).expect("link");
    plant(&real.join("gh_token"), b"theirs", 0o600);
    let deep = home.path().join("deep");
    std::fs::create_dir(&deep).expect("dir");
    plant(&deep.join("config"), b"theirs", 0o600);
    std::fs::create_dir(home.path().join(".config")).expect("dir");
    std::os::unix::fs::symlink(&deep, home.path().join(".config/git")).expect("link");
    let target = home.path().join(".aterm/release-ring/roster");
    std::fs::create_dir_all(&target).expect("dir");
    plant(&target.join("aterm-machines.toml"), b"v", 0o600);
    std::os::unix::fs::symlink(&target, home.path().join(".aterm/roster")).expect("link");
    plant(&deep.join("credentials"), b"held", 0o600);
    let entries = [
        gap("gh_token", b"vault"),
        gap("~/.config/git/config", b"vault"),
        gap("~/.config/git/credentials", b"vault"),
        entry("~/.aterm/roster/aterm-machines.toml", b"v"),
        entry("~/.aterm/release-ring/roster/aterm-machines.toml", b"v"),
    ];
    let held = Manifest {
        files: BTreeMap::from([("~/.config/git/credentials".to_string(), digest_hex(b"held"))]),
    };
    let (manifest, report) = apply(home.path(), &entries, &held, 1).expect("applied");
    assert_eq!(
        failed_names(&report),
        ["gh_token", "~/.aterm/roster/aterm-machines.toml"]
    );
    for ((_, why), link) in report.failed.iter().zip([".secrets", ".aterm/roster"]) {
        let link = home.path().join(link).display().to_string();
        assert!(why.contains(&link) && why.contains("symlink"), "{why}");
    }
    assert_eq!(
        report.left,
        ["~/.config/git/config", "~/.config/git/credentials"]
    );
    assert_eq!(
        report.unchanged,
        ["~/.aterm/release-ring/roster/aterm-machines.toml"]
    );
    assert_eq!(
        manifest.files,
        BTreeMap::from([("~/.config/git/credentials".to_string(), digest_hex(b"held"))]),
        "the vault's file behind the link stays the vault's"
    );
    assert_eq!(read(&real.join("gh_token")), b"theirs");
    assert_eq!(read(&deep.join("config")), b"theirs");
    assert_eq!(read(&deep.join("credentials")), b"held");
}

#[test]
fn a_dropped_name_for_a_file_a_refused_entry_names_is_kept() {
    let home = scratch();
    let real = home.path().join(".aterm/release-ring/roster");
    std::fs::create_dir_all(&real).expect("dir");
    plant(&real.join("aterm-machines.toml"), b"v", 0o600);
    std::os::unix::fs::symlink(&real, home.path().join(".aterm/roster")).expect("link");
    let held = Manifest {
        files: BTreeMap::from([(
            "~/.aterm/release-ring/roster/aterm-machines.toml".to_string(),
            digest_hex(b"v"),
        )]),
    };
    let (manifest, report) = apply(
        home.path(),
        &[entry("~/.aterm/roster/aterm-machines.toml", b"w")],
        &held,
        2,
    )
    .expect("applied");
    assert_eq!(
        failed_names(&report),
        ["~/.aterm/roster/aterm-machines.toml"]
    );
    assert_eq!(
        report.left,
        ["~/.aterm/release-ring/roster/aterm-machines.toml"]
    );
    assert!(report.removed.is_empty());
    assert_eq!(read(&real.join("aterm-machines.toml")), b"v");
    assert_eq!(manifest, held);
}

#[test]
fn an_invalid_name_fails_that_entry_only() {
    let home = scratch();
    let (_, report) = apply(
        home.path(),
        &[entry("../escape", b"v"), entry("ok", b"v")],
        &Manifest::default(),
        1,
    )
    .expect("applied");
    assert_eq!(failed_names(&report), ["../escape"]);
    assert_eq!(report.written, ["ok"]);
    assert!(!home.path().join("escape").exists());
}

#[test]
fn moved_aside_names_stay_within_name_max_and_apart() {
    assert_eq!(aside_name("gh_token", 1700), "gh_token.1700");
    let longest_file_name = "a".repeat(128);
    assert_eq!(
        aside_name(&longest_file_name, u64::MAX),
        format!("{longest_file_name}.{}", u64::MAX)
    );
    let deep = |last: &str| format!("~/{}/{last}", vec!["c".repeat(100); 6].join("/"));
    let (one, two) = (deep("one.toml"), deep("two.toml"));
    for now in [0, 1_792_530_000, u64::MAX] {
        let a = aside_name(&one, now);
        let b = aside_name(&two, now);
        assert!(a.len() + ".999".len() <= NAME_MAX, "{}", a.len());
        assert_ne!(a, b, "a shared start still gives two names");
        assert_eq!(a, aside_name(&one, now), "deterministic");
        assert!(a.starts_with("~%2Fcccc"), "{a}");
        assert!(a.ends_with(&format!(".{now}")), "{a}");
    }
}

#[test]
fn reports_carry_names_never_values() {
    let sentinel = "Zq9!sentinel+value";
    let home = scratch();
    let secrets = home.path().join(".secrets");
    std::fs::create_dir_all(secrets.join("d")).expect("dir");
    plant(&secrets.join("old"), sentinel.as_bytes(), 0o600);
    let entries = [
        entry("old", b"new"),
        entry("d", sentinel.as_bytes()),
        entry("n", sentinel.as_bytes()),
    ];
    let (manifest, report) =
        apply(home.path(), &entries, &Manifest::default(), 1).expect("applied");
    let shown = format!("{report:?} {manifest:?}");
    assert!(!shown.contains("sentinel"), "{shown}");
    assert_eq!(report.failed.len(), 1);
}
