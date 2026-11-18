// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt as _;

use aterm_net::fleet::NodeIdentity;
use aterm_net::fleet_member::name_record::{
    By, ByKind, FLOOR_MS, NameRecord, ZERO_DIGEST, node_of,
};
use aterm_net::fleet_member::{ISSUER_BOOK_FILE, NAME_FILE, PEERS_FILE, Peer, node_identity};
use aterm_secrets::client::{LabLabels, LabelHold};
use aterm_types::machine_name::{Record, nouns};
use aterm_uds::private_file::write_private;

use super::*;

fn no_labels() -> io::Result<LabLabels> {
    Ok(LabLabels::default())
}

/// Late 2026, well past [`FLOOR_MS`].
pub(crate) const NOW: u64 = FLOOR_MS + 300 * 86_400_000;

pub(crate) fn state() -> aterm_tempfile::TempDir {
    let root = aterm_tempfile::tempdir().expect("a tempdir");
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
        .expect("a private dir");
    root
}

#[test]
fn first_name_evidence_is_read_without_holding_the_name_lock() {
    use aterm_uds::private_file::PrivateLock;
    use std::time::Duration;

    let root = state();
    let roster = || {
        let _lock = PrivateLock::take(
            root.path(),
            file::LOCK,
            Duration::ZERO,
            "the name lock was already held",
        )?;
        Ok(Some("m86".to_owned()))
    };
    let (name, origin) =
        ensure(root.path(), roster, never, NOW).expect("the roster can acquire the name lock");
    assert_eq!((name.num, origin), (Some(num(86)), Origin::Evidence));
}

#[test]
fn a_concurrent_first_reader_wins_even_when_the_other_read_fails() {
    use aterm_uds::private_file::PrivateLock;
    use std::time::Duration;

    for fails in [false, true] {
        let root = state();
        let winner = MachineName::unnumbered(Nickname::parse("Winner").expect("a nickname"), NOW);
        let roster = || {
            let _lock = PrivateLock::take(
                root.path(),
                file::LOCK,
                Duration::ZERO,
                "the candidate read held the name lock",
            )?;
            write_private(
                root.path(),
                file::FILE,
                winner.to_toml().expect("the winning file").as_bytes(),
            )?;
            if fails {
                Err(io::Error::other(
                    "this reader's roster failed after the winner arrived",
                ))
            } else {
                Ok(Some("m86".to_owned()))
            }
        };
        assert_eq!(
            ensure(root.path(), roster, never, NOW).expect("the valid winner stands"),
            (winner.clone(), Origin::Kept),
            "fails={fails}"
        );
        assert_eq!(
            file::read(root.path()).expect("reads the winner"),
            Some(winner)
        );
    }
}

fn registry_name(n: u32, nick: Option<(&str, NickClass)>) -> RegistryNameRead {
    RegistryNameRead::Bound(Ok(RegistryName {
        num: num(n),
        nick: nick.map(|(word, class)| (Nickname::parse(word).expect("a nickname"), class)),
    }))
}

#[test]
fn a_bound_registry_name_precedes_legacy_books_and_roster_without_making_a_local_claim() {
    let root = state();
    let me = node_identity(root.path()).expect("an older local identity");
    write_private(root.path(), NAME_FILE, b"m7\n").expect("older legacy evidence");
    write_private(
        root.path(),
        PEERS_FILE,
        entry(&me, &record(&me, Some(9), 8, Some("Heron"))).as_bytes(),
    )
    .expect("older signed evidence");
    let (name, origin) = ensure_from(
        root.path(),
        || registry_name(86, Some(("MissBunny", NickClass::Chosen))),
        unread,
        |_| panic!("a supplied registry nickname needs no draw"),
        NOW,
    )
    .expect("uses the bound row");
    assert_eq!(origin, Origin::Registry);
    assert_eq!(name.title(), "m86 MissBunny");
    assert_eq!(
        (name.class, name.since, name.rev),
        (Class::Seeded, IMPLIED_SINCE, 0)
    );
    assert_eq!(name.migration, None);
    assert!(name.was.is_empty());
    assert_eq!(file::read(root.path()).expect("reads"), Some(name));
}

#[test]
fn registry_nicknames_keep_their_spelling_and_automatic_or_chosen_provenance() {
    for class in [NickClass::Auto, NickClass::Chosen] {
        let root = state();
        let (name, origin) = ensure_from(
            root.path(),
            || registry_name(86, Some(("MissBunny", class))),
            unread,
            never,
            NOW,
        )
        .expect("a typed nickname");
        assert_eq!(origin, Origin::Registry);
        assert_eq!(name.nick.as_str(), "MissBunny");
        assert_eq!(name.nick_class, class);
        assert_eq!((name.num, name.class), (Some(num(86)), Class::Seeded));
    }
}

#[test]
fn a_null_registry_nickname_draws_a_local_automatic_noun_without_reading_legacy_sources() {
    use std::cell::Cell;
    let root = state();
    let draws = Cell::new(0);
    let (name, origin) = ensure_from(
        root.path(),
        || registry_name(86, None),
        unread,
        |_| {
            draws.set(draws.get() + 1);
            false
        },
        NOW,
    )
    .expect("draws the nickname only");
    assert_eq!((name.num, origin), (Some(num(86)), Origin::Registry));
    assert_eq!(name.nick_class, NickClass::Auto);
    assert!(draws.get() > 0);
    assert!(!name.nick.as_str().is_empty());
    assert!(
        read_node_identity(root.path())
            .expect("does not mint an identity")
            .is_none()
    );
}

#[test]
fn an_existing_local_name_reads_no_registry_roster_or_nickname_source() {
    let root = state();
    let winner = MachineName::unnumbered(Nickname::parse("Winner").expect("a nickname"), NOW);
    file::read_or_create(root.path(), || Ok(winner.clone())).expect("an existing local name");
    assert_eq!(
        ensure_from(
            root.path(),
            || panic!("an existing name does not ask the registry"),
            unread,
            |_| panic!("an existing name needs no draw"),
            NOW
        )
        .expect("keeps the local name"),
        (winner, Origin::Kept)
    );
}

/// The name a first read made from a bound row a person nicknamed `MissBunny`.
fn mirrored(root: &aterm_tempfile::TempDir) -> MachineName {
    let (mirror, origin) = ensure_from(
        root.path(),
        || registry_name(86, Some(("MissBunny", NickClass::Chosen))),
        unread,
        never,
        NOW,
    )
    .expect("the first name");
    assert_eq!(origin, Origin::Registry);
    assert_eq!(
        seen(&mirror),
        Some(Some("MissBunny")),
        "it saw the row it came from"
    );
    mirror
}

/// What `name` saw of its row: `None` never looked, `Some(None)` saw no chosen nickname.
fn seen(name: &MachineName) -> Option<Option<&str>> {
    match &name.row {
        file::RowSeen::Never => None,
        file::RowSeen::Saw(nick) => Some(nick.as_ref().map(Nickname::as_str)),
    }
}

/// The name kept in `root` once a read has followed this Mac's row while it holds the chosen
/// nickname `row` (`None`: none chosen).
fn follow(root: &aterm_tempfile::TempDir, row: Option<&str>) -> MachineName {
    registry::with_row(row, || follow_registry(root.path())).expect("follows the row");
    file::read(root.path())
        .expect("reads")
        .expect("a kept name")
}

/// A local nickname rename to `word` while this Mac's row holds `row`.
fn renick(root: &aterm_tempfile::TempDir, word: &str, row: Option<&str>, at: u64) -> MachineName {
    let to = NickTo::Word(Nickname::parse(word).expect("a nickname"));
    registry::with_row(row, || {
        set_nick(root.path(), &to, ByKind::Person, unread, at, false)
    })
    .expect("a rename here")
    .name
}

#[test]
fn an_owner_write_of_the_row_nickname_renames_this_mac() {
    let root = state();
    let mirror = mirrored(&root);
    let name = follow(&root, Some("Heron"));
    assert_eq!(name.title(), "m86 Heron");
    assert_eq!(
        (name.nick_class, name.rev),
        (NickClass::Chosen, mirror.rev + 1),
        "a chosen nickname: the registry keeps who wrote it"
    );
    assert_eq!(
        (name.num, name.class, name.since, name.act),
        (mirror.num, mirror.class, mirror.since, mirror.act),
        "the number and its claim stay as they were"
    );
    assert_eq!(seen(&name), Some(Some("Heron")));
    assert_eq!(
        registry::with_row(Some("Heron"), || follow_registry(root.path())).expect("reads"),
        None,
        "a row read again changes nothing"
    );
}

/// The common Mac: its row's nickname was empty or an automatic path's when its name was made,
/// so it drew a noun; a person's first write to the row from any Mac renames it.
#[test]
fn a_mac_whose_row_held_no_chosen_nickname_follows_the_first_one_written() {
    for row in [None, Some(("Kestrel", NickClass::Auto))] {
        let root = state();
        let (drawn, _) = ensure_from(root.path(), || registry_name(86, row), unread, never, NOW)
            .expect("a first name");
        assert_eq!(seen(&drawn), Some(None), "{row:?}");
        assert_eq!(follow(&root, None), drawn, "{row:?}");
        assert_eq!(follow(&root, Some("Heron")).title(), "m86 Heron", "{row:?}");
    }
}

#[test]
fn a_rename_whose_row_write_is_on_its_way_is_not_undone() {
    let root = state();
    mirrored(&root);
    let renamed = renick(&root, "Bunny", Some("MissBunny"), NOW + 1);
    assert_eq!(
        follow(&root, Some("MissBunny")),
        renamed,
        "the row still holds the nickname the rename replaced"
    );
    let written = follow(&root, Some("Bunny"));
    assert_eq!(
        (written.title(), written.rev),
        ("m86 Bunny".to_owned(), renamed.rev),
        "the rename's own row write changes no name"
    );
    assert_eq!(seen(&written), Some(Some("Bunny")));
    assert_eq!(
        follow(&root, Some("Heron")).title(),
        "m86 Heron",
        "a later write from another Mac is followed"
    );
}

/// The row holds this Mac's own earlier write when a newer rename commits: the rename starts
/// from the row as it stands, so that write never undoes it.
#[test]
fn an_earlier_own_write_does_not_undo_a_newer_rename() {
    let root = state();
    mirrored(&root);
    renick(&root, "Otter", Some("MissBunny"), NOW + 1);
    let newer = renick(&root, "Zebra", Some("Otter"), NOW + 2);
    assert_eq!(seen(&newer), Some(Some("Otter")));
    assert_eq!(follow(&root, Some("Otter")), newer);
    assert_eq!(follow(&root, Some("Zebra")).title(), "m86 Zebra");
}

/// An edit that leaves the nickname (a number alone) starts from the nickname the row was given
/// and no read followed yet, so it keeps it rather than writing the old one back over it.
#[test]
fn a_number_rename_keeps_a_row_nickname_no_read_followed_yet() {
    let root = state();
    mirrored(&root);
    let to = NameTo {
        num: num(87),
        nick: None,
    };
    let renamed = registry::with_row(Some("Heron"), || {
        set_name(
            root.path(),
            &to,
            no_labels,
            ByKind::Person,
            unread,
            NOW + 1,
            false,
        )
    })
    .expect("a number rename")
    .name;
    assert_eq!(renamed.title(), "m87 Heron");
}

/// A rename to the nickname the row was given, which no read followed yet, asks for nothing the
/// name does not hold once it follows: the file takes the row's change and the rename says so.
#[test]
fn a_rename_to_the_row_nickname_no_read_followed_changes_nothing_it_asked() {
    let root = state();
    mirrored(&root);
    let to = NickTo::Word(Nickname::parse("Heron").expect("a nickname"));
    let set = registry::with_row(Some("Heron"), || {
        set_nick(root.path(), &to, ByKind::Person, unread, NOW + 1, false)
    })
    .expect("a rename here");
    assert!(!set.changed, "the request changed nothing");
    assert_eq!(set.name.title(), "m86 Heron");
    assert_eq!(file::read(root.path()).expect("reads"), Some(set.name));
}

/// An undo starts from the name as this Mac's row leaves it: an owner's nickname written to the
/// row from another Mac, which no read here followed yet, is the row's newest rename, and its
/// undo gives back the nickname it replaced; an undo of the rename before it, read before that
/// write reached this Mac's registry, is refused as unseen, History giving another undo now.
/// NEGATIVE CONTROL: with `edit_name` starting from the file rather than `row_followed`, the
/// undo's base is `Bunny`, the nickname undo changes nothing, and `changed` is false.
#[test]
fn an_undo_read_through_the_follow_takes_back_what_it_showed() {
    let root = state();
    mirrored(&root);
    renick(&root, "Bunny", Some("MissBunny"), NOW + 1);
    let ours = owner_write(2, &[(Renamed::Nickname, Some("MissBunny"), "Bunny")]);
    let theirs = owner_write(3, &[(Renamed::Nickname, Some("Bunny"), "Heron")]);
    let mut replaced = ours.clone();
    replaced.cells[0].stands = false;
    let now = row(vec![theirs, replaced]);
    let undo = |seen: &history::Undo, at| {
        registry::with_row(Some("Heron"), || {
            history::with_history(Ok(now.clone()), || {
                undo_last(
                    root.path(),
                    seen,
                    no_labels,
                    ByKind::Person,
                    unread,
                    at,
                    false,
                )
            })
        })
    };
    let stale = undo_over(vec![ours]).expect("the rename here");
    let why = undo(&stale, NOW + 2).expect_err("a stale read");
    assert_eq!(
        Refused::of(&why).map(|refused| &refused.cause),
        Some(&Cause::Unseen)
    );
    let fresh = history::undo_of(&now).expect("the other Mac's rename");
    let undone = undo(&fresh, NOW + 4).expect("an undo");
    assert!(undone.changed, "the row's Heron taken back");
    assert_eq!(undone.name.title(), "m86 Bunny");
}

/// A row holding this Mac's own nickname, a drawn noun included, renames nothing and leaves a
/// drawn noun drawn, whatever the file saw before.
#[test]
fn a_row_holding_this_macs_own_nickname_renames_nothing() {
    let root = state();
    let (drawn, _) = ensure_from(root.path(), || registry_name(86, None), unread, never, NOW)
        .expect("a drawn noun");
    assert_eq!(drawn.nick_class, NickClass::Auto);
    for before in [
        file::RowSeen::Never,
        file::RowSeen::Saw(None),
        file::RowSeen::Saw(Some(Nickname::parse("MissBunny").expect("a nickname"))),
    ] {
        file::replace(root.path(), |name| {
            name.row = before.clone();
            Ok(())
        })
        .expect("a row seen before");
        let name = follow(&root, Some(drawn.nick.as_str()));
        assert_eq!(
            (name.title(), name.nick_class, name.rev),
            (drawn.title(), NickClass::Auto, drawn.rev),
            "{before:?}"
        );
        assert_eq!(seen(&name), Some(Some(drawn.nick.as_str())), "{before:?}");
    }
}

/// A file that never looked at the row (an earlier build's) with a chosen nickname only marks
/// the row's value seen, so a value older than that choice does not replace it, and follows the
/// next one; a drawn noun follows at once.
#[test]
fn a_name_that_never_looked_follows_a_chosen_row_only_once_it_moves() {
    for (class, first) in [
        (NickClass::Chosen, "unnumbered Bunny"),
        (NickClass::Auto, "unnumbered MissBunny"),
    ] {
        let root = state();
        let mut kept = MachineName::unnumbered(Nickname::parse("Bunny").expect("a nickname"), NOW);
        kept.nick_class = class;
        file::read_or_create(root.path(), || Ok(kept.clone()))
            .expect("a name an earlier build made");
        let looked = follow(&root, Some("MissBunny"));
        assert_eq!(
            (looked.title(), seen(&looked)),
            (first.to_owned(), Some(Some("MissBunny")))
        );
        let name = follow(&root, Some("Heron"));
        assert_eq!(
            name.title(),
            "unnumbered Heron",
            "the nickname only, never the number"
        );
    }
}

/// A row that holds no chosen nickname (empty, an automatic write's, conflicted) renames
/// nothing, and the same word written to it again later is followed.
#[test]
fn a_row_with_no_chosen_nickname_renames_nothing() {
    let root = state();
    let mirror = mirrored(&root);
    let emptied = follow(&root, None);
    assert_eq!((emptied.title(), emptied.rev), (mirror.title(), mirror.rev));
    assert_eq!(seen(&emptied), Some(None));
    renick(&root, "Otter", None, NOW + 2);
    assert_eq!(follow(&root, Some("MissBunny")).title(), "m86 MissBunny");
}

/// No row of this Mac's (no lab, a name kept elsewhere) leaves the file as it is.
#[test]
fn no_row_of_this_macs_leaves_the_file_as_it_is() {
    let root = state();
    let mirror = mirrored(&root);
    assert_eq!(follow_registry(root.path()).expect("reads"), None);
    assert_eq!(file::read(root.path()).expect("reads"), Some(mirror));
}

/// A dry run's read follows nothing into the file, though its preview starts from the row.
#[test]
fn a_dry_run_leaves_the_file_as_it_is() {
    let root = state();
    let mirror = mirrored(&root);
    let to = NameTo {
        num: num(87),
        nick: None,
    };
    let preview = registry::with_row(Some("Heron"), || {
        set_name(
            root.path(),
            &to,
            no_labels,
            ByKind::Person,
            unread,
            NOW + 1,
            true,
        )
    })
    .expect("a preview");
    assert_eq!(preview.name.title(), "m87 Heron");
    assert_eq!(file::read(root.path()).expect("reads"), Some(mirror));
}

/// What a read of this Mac's row does to the name kept in `root` while its label shows the
/// owner's number `label` (`None`: none to take; `pending`: a write of this Mac's own on its
/// way, or a held one the row does not show the rival of yet), with the vault's labels `lab`
/// reads.
fn renumber_with(
    root: &aterm_tempfile::TempDir,
    label: Option<&str>,
    pending: bool,
    lab: impl FnOnce() -> io::Result<LabLabels>,
    at: u64,
) -> io::Result<Option<MachineName>> {
    let config = root.path().join("aterm.toml");
    registry::with_label(label, pending, || {
        follow_number(root.path(), &config, lab, at)
    })
    .map(|got| got.map(|(name, _)| name))
}

/// [`renumber_with`] with no vault labels and no write of this Mac's own on its way.
fn renumber(root: &aterm_tempfile::TempDir, label: Option<&str>, at: u64) -> Option<MachineName> {
    renumber_with(root, label, false, no_labels, at).expect("a read of the row")
}

/// The name a first read made from the bound row nicknamed `MissBunny` (seeded m86), its number
/// then chosen here.
fn chosen(root: &aterm_tempfile::TempDir) -> MachineName {
    mirrored(root);
    file::replace(root.path(), |name| {
        name.class = Class::Chosen;
        Ok(())
    })
    .expect("a chosen number")
}

/// `set_name` of `to` as a person asks for it, with the row's write queued through a door that
/// answers `door`: the rename's outcome and the writes it queued, each as its cells.
fn renamed_with(
    root: &aterm_tempfile::TempDir,
    to: &str,
    door: registry::Written,
    dry_run: bool,
    at: u64,
) -> (io::Result<NameSet>, Vec<Vec<String>>) {
    let to = NameTo {
        num: Number::parse(to).expect("a number"),
        nick: None,
    };
    registry::with_door(door, || {
        registry::with_label(None, false, || {
            set_name(
                root.path(),
                &to,
                no_labels,
                ByKind::Person,
                unread,
                at,
                dry_run,
            )
        })
    })
}

/// What a person's rename through a door that queues it answers.
fn queued() -> registry::Written {
    registry::Written::Queued(registry::Queued {
        id: "wi-1".to_owned(),
        unflushed: None,
    })
}

/// The id of the write an edit queued.
fn queued_id(queued: Option<&registry::Queued>) -> Option<&str> {
    queued.map(|write| write.id.as_str())
}

/// One write to the row, as the door seam records it: its cells, then that a person asked.
fn cells(cells: &[&str]) -> Vec<Vec<String>> {
    vec![
        cells
            .iter()
            .map(|cell| (*cell).to_owned())
            .chain(["by=person".to_owned()])
            .collect(),
    ]
}

/// The registry row's number is the Mac's: an owner's number other than the chosen one renumbers
/// it as chosen (the registry keeps who wrote it), and a read again changes nothing.
#[test]
fn an_owner_write_of_the_row_label_renumbers_this_mac() {
    let root = state();
    let before = chosen(&root);
    let name = renumber(&root, Some("m87"), NOW + 1).expect("the row renumbers the Mac");
    assert_eq!(name.title(), "m87 MissBunny");
    assert_eq!(
        (name.class, name.rev),
        (Class::Chosen, before.rev + 1),
        "a chosen number: the registry keeps who wrote it"
    );
    assert_eq!(file::read(root.path()).expect("reads"), Some(name));
    assert_eq!(
        renumber(&root, Some("m87"), NOW + 2),
        None,
        "a row read again changes nothing"
    );
}

/// A follow of the row's number saves the renumbered name only under the admission of the
/// source the decision read, the name lock held through it: a source that no longer admits
/// local writes (a held, off or revoked registry) renumbers nothing and leaves the file as it
/// was, and the same row is taken once the source admits it.
///
/// NEGATIVE CONTROL, observed: with `edit_name` writing a follow's file outside the admission
/// (`LabelWrite::Follows(_) => write()`), the refused source renumbers the Mac: `a refused
/// source renumbers nothing: Ok(Some(…m87…))`.
#[test]
fn a_followed_number_is_saved_only_under_the_sources_admission() {
    use crate::harness::{
        account_sync_error::SyncError,
        alab_roster::{ReceiptGuard, SourceGuard},
    };
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    struct Admission(Arc<AtomicBool>);
    impl SourceGuard for Admission {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
        fn same_source(&self, other: &dyn SourceGuard) -> bool {
            other
                .as_any()
                .downcast_ref::<Self>()
                .is_some_and(|other| Arc::ptr_eq(&self.0, &other.0))
        }
        fn with_admitted(
            &self,
            save: Box<dyn FnOnce() -> Result<(), SyncError> + '_>,
        ) -> Result<(), SyncError> {
            if !self.0.load(Ordering::Acquire) {
                return Err("the retained source no longer admits local writes".into());
            }
            save()
        }
    }

    let root = state();
    let before = chosen(&root);
    let allowed = Arc::new(AtomicBool::new(false));
    let seam = || registry::Seam {
        label: Some(Some(num(87))),
        admission: Some(ReceiptGuard::new(Admission(allowed.clone()))),
        ..registry::Seam::default()
    };
    let config = root.path().join("aterm.toml");
    let follow = |at: u64| {
        registry::with_seam(seam(), || {
            follow_number(root.path(), &config, no_labels, at)
        })
        .map(|got| got.map(|(name, _)| name))
    };
    let refused = follow(NOW + 1);
    assert!(
        refused.is_err(),
        "a refused source renumbers nothing: {refused:?}"
    );
    assert_eq!(
        file::read(root.path()).expect("reads"),
        Some(before),
        "neither the number nor History moved"
    );
    allowed.store(true, Ordering::Release);
    let name = follow(NOW + 2)
        .expect("an admitted source")
        .expect("the row renumbers the Mac");
    assert_eq!(name.title(), "m87 MissBunny");
    assert_eq!(file::read(root.path()).expect("reads"), Some(name));
}

/// An owner's number replaces one nobody chose here (seeded from the registry, as this one is,
/// or automatic), and the number the row shows becomes chosen where it is the Mac's own: the
/// owner chose it.
#[test]
fn an_owner_number_replaces_a_seeded_one_and_makes_its_own_chosen() {
    let root = state();
    let mirror = mirrored(&root);
    assert_eq!(mirror.class, Class::Seeded);
    let name = renumber(&root, Some("m87"), NOW + 1).expect("renumbered");
    assert_eq!(
        (name.title(), name.class),
        ("m87 MissBunny".to_owned(), Class::Chosen)
    );
    let root = state();
    mirrored(&root);
    let name = renumber(&root, Some("m86"), NOW + 1).expect("the seeded number becomes chosen");
    assert_eq!(
        (name.title(), name.class),
        ("m86 MissBunny".to_owned(), Class::Chosen)
    );
    assert_eq!(
        renumber(&root, Some("m86"), NOW + 2),
        None,
        "and stays as it is"
    );
}

/// A label this Mac's own write is still changing (or a held one waits on the row to show what
/// it met), and one with no owner's number to take (empty, an automatic write's or an import's, another row's by the claim rule),
/// renumber nothing and leave the file as it is; the owner's number read later is taken.
#[test]
fn a_pending_or_untakeable_label_renumbers_nothing() {
    let root = state();
    let before = chosen(&root);
    assert_eq!(
        renumber_with(&root, Some("m87"), true, no_labels, NOW + 1).expect("reads"),
        None
    );
    assert_eq!(renumber(&root, None, NOW + 2), None);
    assert_eq!(
        file::read(root.path()).expect("reads"),
        Some(before),
        "nothing is marked or written"
    );
    assert_eq!(
        renumber(&root, Some("m87"), NOW + 3).map(|name| name.title()),
        Some("m87 MissBunny".to_owned())
    );
}

/// A person's renumber here queues the number it moves to as the row's label before the file
/// holds it, so the row never shows the number it replaced with nothing of this Mac's on its
/// way; the rename answers the write's id for its command to publish. The nickname it leaves as
/// it was is no part of the write, so a nickname the row would not take (one chosen while no row
/// was this Mac's) refuses no renumber. A nickname rename queues its nickname alone; a dry run
/// and a follow queue nothing.
///
/// NEGATIVE CONTROLS, observed: with `edit_name` queueing nothing, the rename's door is never
/// asked: `left: [] right: [["label=m88", "by=person"]]`; with it queueing the nickname the edit
/// left as it was: `left: [["label=m88", "nickname=MissBunny", "by=person"]] right:
/// [["label=m88", "by=person"]]`.
#[test]
fn a_renumber_here_queues_its_number_to_the_row() {
    let root = state();
    chosen(&root);
    let (set, asked) = renamed_with(&root, "m88", queued(), false, NOW + 1);
    let set = set.expect("a renumber here");
    assert_eq!(asked, cells(&["label=m88"]));
    assert!(set.renumbered);
    assert_eq!(queued_id(set.queued.as_ref()), Some("wi-1"));
    assert_eq!(set.name.title(), "m88 MissBunny");
    let (set, asked) = renamed_with(&root, "m89", queued(), true, NOW + 2);
    let set = set.expect("a preview");
    assert_eq!(set.name.title(), "m89 MissBunny");
    assert!(asked.is_empty(), "a dry run queues nothing");
    assert_eq!(set.queued, None);
    let to = NickTo::Word(Nickname::parse("Otter").expect("a nickname"));
    let (set, asked) = registry::with_door(queued(), || {
        registry::with_row(None, || {
            set_nick(root.path(), &to, ByKind::Person, unread, NOW + 3, false)
        })
    });
    let set = set.expect("a nickname rename");
    assert_eq!(set.name.title(), "m88 Otter");
    assert_eq!(
        asked,
        cells(&["nickname=Otter"]),
        "a nickname rename queues no number"
    );
    assert_eq!(queued_id(set.queued.as_ref()), Some("wi-1"));
    let config = root.path().join("aterm.toml");
    let (taken, asked) = registry::with_door(queued(), || {
        registry::with_label(Some("m90"), false, || {
            follow_number(root.path(), &config, no_labels, NOW + 4)
        })
    });
    let taken = taken.expect("a read of the row").expect("renumbered").0;
    assert_eq!(taken.title(), "m90 Otter");
    assert!(asked.is_empty(), "a follow's number is the row's already");
}

/// FL-194's exit: a rename of the number and the nickname together, asked in an aterm session, is
/// one write of the row, so one operation holds it and an undo of the row's newest rename can
/// take back both.
///
/// NEGATIVE CONTROL, observed: with the nickname queued as a write of its own after the number
/// (`edit_name` calling the door once per cell), the rename asks for two writes:
/// `left: [["label=m88"], ["nickname=Otter"]] right: [["label=m88", "nickname=Otter"]]`.
#[test]
fn a_rename_queues_its_number_and_nickname_as_one_row_write() {
    let root = state();
    chosen(&root);
    let to = NameTo {
        num: num(88),
        nick: Some(Nickname::parse("Otter").expect("a nickname")),
    };
    let asker = crate::machine_name::Asker {
        kind: ByKind::Person,
        session: Some("s-rename"),
    };
    let (set, asked) = registry::with_door(queued(), || {
        registry::with_label(None, false, || {
            set_name(root.path(), &to, no_labels, asker, unread, NOW, false)
        })
    });
    let set = set.expect("a rename");
    assert_eq!(set.name.title(), "m88 Otter");
    assert_eq!(asked, cells(&["label=m88", "nickname=Otter"]));
    assert_eq!(queued_id(set.queued.as_ref()), Some("wi-1"));
}

/// A dry run asks the row's rules as the real run's write would be judged, queueing nothing: a
/// rename the row would refuse is refused in its preview too, with the same cause.
///
/// NEGATIVE CONTROL, observed: with `edit_name` asking nothing of the row for a dry run, the
/// preview passes: `a refused preview: NameSet { name: MachineName { num: Some(Number(88)), … }, … }`.
#[test]
fn a_dry_run_is_refused_as_its_real_run_would_be() {
    let root = state();
    let before = chosen(&root);
    let door = registry::Written::Refused("a value the column refuses".to_owned());
    let (set, asked) = renamed_with(&root, "m88", door, true, NOW + 1);
    let why = set.expect_err("a refused preview");
    assert!(asked.is_empty(), "a dry run queues nothing: {asked:?}");
    assert_eq!(
        Refused::of(&why).map(|refused| &refused.cause),
        Some(&Cause::Registry),
        "{why}"
    );
    assert_eq!(file::read(root.path()).expect("reads"), Some(before));
    let (set, _) = renamed_with(&root, "m88", queued(), true, NOW + 2);
    assert_eq!(set.expect("a preview").name.title(), "m88 MissBunny");
}

/// A rename's write that gives way, whole, to another writer's value of the row leaves the row's
/// nickname there, and the next read follows it: the file counts the nickname it queued as what
/// the row will show, so the row showing another is a change to follow, never the rename's own.
///
/// NEGATIVE CONTROL, observed: with the queued rename leaving the file's `row` as the base saw
/// it (`Saw(MissBunny)`), the row's MissBunny reads as seen and the Mac keeps the nickname the
/// row never took: `left: "m12 Otter" right: "m12 MissBunny"`.
#[test]
fn a_rename_that_gave_way_follows_the_rows_nickname() {
    let root = state();
    chosen(&root);
    let to = NameTo {
        num: num(12),
        nick: Some(Nickname::parse("Otter").expect("a nickname")),
    };
    let (set, asked) = registry::with_door(queued(), || {
        registry::with_label(None, false, || {
            set_name(
                root.path(),
                &to,
                no_labels,
                ByKind::Person,
                unread,
                NOW + 1,
                false,
            )
        })
    });
    assert_eq!(set.expect("a rename").name.title(), "m12 Otter");
    assert_eq!(asked, cells(&["label=m12", "nickname=Otter"]));
    let name = follow(&root, Some("MissBunny"));
    assert_eq!(name.title(), "m12 MissBunny");
    assert_eq!(follow(&root, Some("MissBunny")), name);
}

/// A rename to the number the name already holds, asked for while the row shows another number
/// an owner wrote that no read followed yet, queues the number asked for: the request is the
/// newer of the two, and the row's older number does not replace it at the next read. An undo
/// that moves no number queues none.
///
/// NEGATIVE CONTROL, observed: with `LabelWrite::asked_over_row` answering `false`, the rename
/// changes nothing and queues nothing: `left: [] right: ["m86"]`.
#[test]
fn a_number_asked_for_again_is_queued_over_an_unfollowed_row_number() {
    let root = state();
    chosen(&root);
    let to = NameTo {
        num: num(86),
        nick: None,
    };
    let (set, asked) = registry::with_door(queued(), || {
        registry::with_label(Some("m9"), false, || {
            set_name(
                root.path(),
                &to,
                no_labels,
                ByKind::Person,
                unread,
                NOW + 1,
                false,
            )
        })
    });
    let set = set.expect("a rename to the number held");
    assert_eq!(asked, cells(&["label=m86"]));
    assert!(set.renumbered && !set.changed);
    assert_eq!(set.name.title(), "m86 MissBunny");
    let (_, asked) = registry::with_door(queued(), || {
        registry::with_label(Some("m86"), false, || {
            set_name(
                root.path(),
                &to,
                no_labels,
                ByKind::Person,
                unread,
                NOW + 2,
                false,
            )
        })
    });
    assert!(
        asked.is_empty(),
        "the row shows the number asked for: {asked:?}"
    );
    let nick = NickTo::Word(Nickname::parse("Otter").expect("a nickname"));
    registry::with_door(queued(), || {
        registry::with_row(None, || {
            set_nick(root.path(), &nick, ByKind::Person, unread, NOW + 3, false)
        })
    })
    .0
    .expect("a nickname rename");
    let seen = nickname_undo("MissBunny", "Otter");
    let (undone, asked) = registry::with_door(queued(), || {
        registry::with_label(Some("m9"), false, || {
            history::with_history(Ok(row_of(&seen)), || {
                undo_last(
                    root.path(),
                    &seen,
                    no_labels,
                    ByKind::Person,
                    unread,
                    NOW + 4,
                    false,
                )
            })
        })
    });
    assert_eq!(
        undone.expect("a nickname undo").name.title(),
        "m86 MissBunny"
    );
    assert_eq!(
        asked,
        cells(&["nickname=MissBunny"]),
        "an undo that moves no number queues none"
    );
}

/// A row that would not take the number refuses the rename, which leaves the file as it was.
#[test]
fn a_number_the_row_refuses_renames_nothing() {
    let root = state();
    let before = chosen(&root);
    let door = registry::Written::Refused("a value the column refuses".to_owned());
    let (set, asked) = renamed_with(&root, "m88", door, false, NOW + 1);
    let why = set.expect_err("refused");
    assert_eq!(asked, cells(&["label=m88"]));
    assert_eq!(
        Refused::of(&why).map(|refused| &refused.cause),
        Some(&Cause::Registry),
        "{why}"
    );
    assert_eq!(file::read(root.path()).expect("reads"), Some(before));
}

/// An owner who writes back the number a rename here replaced is followed: the row's number is
/// the Mac's, with nothing remembered of what it showed before.
#[test]
fn an_owner_revert_to_the_number_a_rename_here_replaced_is_followed() {
    let root = state();
    chosen(&root);
    let (set, _) = renamed_with(&root, "m88", queued(), false, NOW + 1);
    assert_eq!(set.expect("a renumber here").name.title(), "m88 MissBunny");
    assert_eq!(
        renumber(&root, Some("m88"), NOW + 2),
        None,
        "the rename's own write"
    );
    assert_eq!(
        renumber(&root, Some("m86"), NOW + 3).map(|name| name.title()),
        Some("m86 MissBunny".to_owned())
    );
}

/// A number the vault gives another Mac is refused as any rename's is, leaving the file as it
/// was, so a later read judges it again and takes it once nothing holds it.
#[test]
fn a_label_the_vault_gives_another_mac_is_refused_and_judged_again() {
    let root = state();
    let before = chosen(&root);
    let lab = LabLabels {
        held: BTreeMap::from([(num(87), LabelHold::Label)]),
        ..LabLabels::default()
    };
    let why = renumber_with(&root, Some("m87"), false, || Ok(lab), NOW + 1)
        .expect_err("another Mac's number");
    assert_eq!(
        Refused::of(&why).map(|refused| &refused.cause),
        Some(&Cause::Vault(LabelHold::Label)),
        "{why}"
    );
    assert_eq!(file::read(root.path()).expect("reads"), Some(before));
    assert_eq!(
        renumber(&root, Some("m87"), NOW + 2).map(|name| name.title()),
        Some("m87 MissBunny".to_owned())
    );
}

/// Undo takes a followed number back and queues the number it restores to the row, which then
/// shows it, so the undone number is not taken again.
#[test]
fn an_undo_of_a_followed_number_queues_the_number_it_restores() {
    let root = state();
    chosen(&root);
    renumber(&root, Some("m87"), NOW + 1).expect("renumbered");
    let seen = undo_over(vec![owner_write(
        2,
        &[(Renamed::Label, Some("m86"), "m87")],
    )])
    .expect("the owner's renumber");
    let (undone, asked) = registry::with_door(queued(), || {
        registry::with_label(Some("m87"), false, || {
            history::with_history(Ok(row_of(&seen)), || {
                undo_last(
                    root.path(),
                    &seen,
                    no_labels,
                    ByKind::Person,
                    unread,
                    NOW + 2,
                    false,
                )
            })
        })
    });
    let undone = undone.expect("an undo").name;
    assert_eq!(undone.title(), "m86 MissBunny");
    assert_eq!(asked, cells(&["label=m86"]));
    assert_eq!(
        renumber_with(&root, Some("m87"), true, no_labels, NOW + 3).expect("reads"),
        None
    );
    assert_eq!(renumber(&root, Some("m86"), NOW + 4), None);
}

#[test]
fn pending_or_invalid_bound_registry_names_refuse_first_creation_without_fallback() {
    for source in [
        RegistryNameRead::Pending,
        RegistryNameRead::Bound(Err("the label is conflicted".to_owned())),
    ] {
        let root = state();
        write_private(root.path(), NAME_FILE, b"m7\n").expect("a tempting legacy fallback");
        assert!(
            ensure_from(
                root.path(),
                || source,
                unread,
                |_| panic!("an unavailable registry name needs no draw"),
                NOW
            )
            .is_err()
        );
        assert!(
            file::read(root.path())
                .expect("reads no partial name")
                .is_none()
        );
    }
}

#[test]
fn an_unbound_registry_preserves_the_existing_fallback_order() {
    let root = state();
    write_private(root.path(), NAME_FILE, b"m7\n").expect("legacy evidence");
    let (name, origin) = ensure_from(
        root.path(),
        || RegistryNameRead::Unbound,
        no_roster,
        never,
        NOW,
    )
    .expect("uses legacy fallback");
    assert_eq!((name.num, origin), (Some(num(7)), Origin::Evidence));
    assert_eq!(name.migration, Some(Migration::Pending));
}

#[test]
fn a_concurrent_local_winner_overrides_registry_pending_or_invalid_results() {
    use aterm_uds::private_file::PrivateLock;
    use std::time::Duration;
    for source in [
        RegistryNameRead::Pending,
        RegistryNameRead::Bound(Err("the label is conflicted".to_owned())),
    ] {
        let root = state();
        let winner = MachineName::unnumbered(Nickname::parse("Winner").expect("a nickname"), NOW);
        let registry = || {
            let _lock = PrivateLock::take(
                root.path(),
                file::LOCK,
                Duration::ZERO,
                "registry preparation held the name lock",
            )
            .expect("preparation is outside the lock");
            write_private(
                root.path(),
                file::FILE,
                winner.to_toml().expect("the winner").as_bytes(),
            )
            .expect("a concurrent creator won");
            source
        };
        assert_eq!(
            ensure_from(root.path(), registry, unread, never, NOW)
                .expect("the valid winner stands"),
            (winner.clone(), Origin::Kept)
        );
        assert_eq!(file::read(root.path()).expect("reads"), Some(winner));
    }
}

#[test]
fn registry_names_remain_the_edit_base_despite_newer_old_own_book_records() {
    let root = state();
    let me = node_identity(root.path()).expect("this node's identity");
    let mut old = record(&me, Some(9), 9, Some("Heron"));
    old.claim.anchor = Some(aterm_types::machine_name::Anchor([0x42; 8]));
    old.claim.ephemeral = true;
    old.claim.sgen = 4;
    old.claim.top = 100;
    write_private(root.path(), PEERS_FILE, entry(&me, &old).as_bytes())
        .expect("a retired signed naming source");
    let (mirror, _) = ensure_from(
        root.path(),
        || registry_name(86, Some(("MissBunny", NickClass::Chosen))),
        unread,
        never,
        NOW,
    )
    .expect("the registry supplies the local mirror");
    let to = NickTo::Word(Nickname::parse("Bunny").expect("a nickname"));
    let preview = set_nick(root.path(), &to, ByKind::Person, unread, NOW + 1, true)
        .expect("previews from the local mirror");
    assert_eq!((preview.name.num, preview.name.rev), (Some(num(86)), 1));
    assert_eq!(
        file::read(root.path()).expect("reads"),
        Some(mirror.clone())
    );
    let nicked = set_nick(root.path(), &to, ByKind::Person, unread, NOW + 1, false)
        .expect("changes only the local nickname")
        .name;
    assert_eq!(nicked.title(), "m86 Bunny");
    assert_eq!(
        (nicked.class, nicked.since, nicked.act),
        (mirror.class, mirror.since, mirror.act),
        "a nickname edit does not strengthen its number"
    );
    assert_eq!(nicked.rev, 1);

    let to = NameTo {
        num: num(87),
        nick: None,
    };
    let preview = set_name(
        root.path(),
        &to,
        no_labels,
        ByKind::Person,
        unread,
        NOW + 2,
        true,
    )
    .expect("previews a local rename");
    assert_eq!(
        preview.name.num,
        Some(num(87)),
        "the preview holds the rename it would make"
    );
    assert_eq!(file::read(root.path()).expect("reads"), Some(nicked));
    let renamed = set_name(
        root.path(),
        &to,
        no_labels,
        ByKind::Person,
        unread,
        NOW + 2,
        false,
    )
    .expect("renames the local mirror")
    .name;
    assert_eq!(renamed.title(), "m87 Bunny");
    assert_eq!(renamed.rev, 2);
    assert_eq!(
        renamed.prev,
        Some(num(86)),
        "the actual previous local number"
    );
    assert_eq!(
        renamed.was.iter().map(|was| was.num).collect::<Vec<_>>(),
        [num(86)]
    );
    assert_eq!((renamed.base, renamed.tgen, renamed.sgen), (None, 0, 0));
    assert_eq!(renamed.cap, None);
}

fn num(n: u32) -> Number {
    Number::new(n).expect("a number")
}

fn never(_: &str) -> bool {
    false
}

fn no_roster() -> io::Result<Option<String>> {
    Ok(None)
}

/// A roster that gives `word`.
fn label(word: &str) -> impl FnOnce() -> io::Result<Option<String>> + '_ {
    move || Ok(Some(word.to_owned()))
}

/// A roster a test must never read.
fn unread() -> io::Result<Option<String>> {
    unreachable!("the roster is read only when nothing else names the claim")
}

/// `me`'s record naming `n`, at `rev`, nicknamed `nick` when given.
pub(crate) fn record(
    me: &NodeIdentity,
    n: Option<u32>,
    rev: u64,
    nick: Option<&str>,
) -> NameRecord {
    NameRecord {
        claim: Record {
            num: n.map(num),
            class: Class::Chosen,
            since: NOW - 1_000,
            base: None,
            tgen: 0,
            sgen: 0,
            act: NOW - 1_000,
            anchor: None,
            prev: None,
            waits: false,
            ephemeral: false,
            was: Vec::new(),
            top: 9,
            cap: None,
        },
        rev,
        eseq: 5,
        boot: None,
        nick: nick.map(|word| Nickname::parse(word).expect("a nickname")),
        nick_class: NickClass::Chosen,
        by: By {
            node: node_of(me.node()).expect("a node id"),
            kind: ByKind::Person,
        },
    }
}

/// `who`'s own signed entry carrying `record` under the zero digest.
pub(crate) fn entry(who: &NodeIdentity, record: &NameRecord) -> String {
    entry_under(who, record, ZERO_DIGEST)
}

/// `who`'s own signed entry carrying `record` signed under seed digest `digest`.
pub(crate) fn entry_under(who: &NodeIdentity, record: &NameRecord, digest: [u8; 8]) -> String {
    let mut peer = Peer::own(who, ("m9.local", 7485, "0.160.0"), NOW);
    peer.signed = peer
        .signed
        .map(|signed| signed.with_record(record.sign(who, digest)));
    peer.line() + "\n"
}

#[test]
fn a_new_machine_is_m0_and_a_drawn_noun_and_every_later_reader_keeps_it() {
    let root = state();
    let taken = |word: &str| word.eq_ignore_ascii_case("otter");
    let (name, origin) = ensure(root.path(), no_roster, taken, NOW).expect("ensures");
    assert_eq!(origin, Origin::Drawn);
    assert_eq!(name.num, None);
    assert_eq!(name.rev, 0, "the first local revision");
    assert_ne!(name.nick.as_str(), "otter", "a noun in view is not drawn");
    assert!(nouns::NOUNS.contains(&name.nick.as_str()));
    assert!(name.title().starts_with("unnumbered "), "{}", name.title());
    assert_eq!(
        ensure(root.path(), unread, never, NOW).expect("ensures"),
        (name, Origin::Kept),
        "the file stands; later evidence is reconcile's"
    );
}

#[test]
fn a_chosen_number_and_nickname_are_one_change_with_the_prior_claim_kept() {
    let root = state();
    let (before, _) = ensure(root.path(), label("m7"), never, NOW).expect("a seeded name");
    let to = NameTo {
        num: num(86),
        nick: Some(Nickname::parse("MissBunny").expect("a nickname")),
    };
    let set = set_name(
        root.path(),
        &to,
        no_labels,
        ByKind::Person,
        unread,
        NOW,
        false,
    )
    .expect("renames");
    assert!(set.changed);
    assert_eq!(set.name.title(), "m86 MissBunny");
    assert_eq!(set.name.class, Class::Chosen);
    assert_eq!(set.name.nick_class, NickClass::Chosen);
    assert_eq!(set.name.rev, before.rev + 1);
    assert!(set.name.since > before.act);
    assert_eq!(set.name.since, set.name.act);
    assert_eq!(set.name.prev, before.num);
    assert_eq!(set.name.was[0].num, num(7));
    assert_eq!(set.name.migration, None);
    assert_eq!(
        file::read(root.path()).expect("reads"),
        Some(set.name.clone())
    );

    let again = set_name(
        root.path(),
        &to,
        no_labels,
        ByKind::Agent,
        unread,
        NOW + 1,
        false,
    )
    .expect("an unchanged request");
    assert!(!again.changed);
    assert_eq!(
        again.name, set.name,
        "no new revision, actor or History entry for a no-op"
    );
}

#[test]
fn renumbering_without_a_nickname_keeps_the_word_and_preview_changes_no_name_or_key() {
    let root = state();
    let (before, _) = ensure(root.path(), label("m7"), never, NOW).expect("a seeded name");
    let to = NameTo {
        num: num(86),
        nick: None,
    };
    let preview = set_name(
        root.path(),
        &to,
        no_labels,
        ByKind::Agent,
        unread,
        NOW,
        true,
    )
    .expect("previews");
    assert!(preview.changed);
    assert_eq!(
        (preview.name.num, &preview.name.nick),
        (Some(num(86)), &before.nick)
    );
    assert_eq!(
        file::read(root.path()).expect("reads"),
        Some(before.clone())
    );
    assert!(
        read_node_identity(root.path())
            .expect("reads a key")
            .is_none()
    );
    let (set, asked) = registry::with_door(registry::Written::Unbound, || {
        set_name(
            root.path(),
            &to,
            no_labels,
            ByKind::Agent,
            unread,
            NOW,
            false,
        )
    });
    let set = set.expect("renames");
    assert_eq!((set.name.num, set.name.nick), (Some(num(86)), before.nick));
    assert_eq!(
        asked,
        [["label=m86", "by=program"]],
        "the row's write says a program asked"
    );
}

#[test]
fn another_signed_number_is_refused_without_a_partial_nickname_change() {
    let root = state();
    let me = node_identity(root.path()).expect("our identity");
    let other = NodeIdentity::from_seed(&"b2".repeat(16), &[0x22; 32]).expect("another identity");
    let (before, _) = ensure(root.path(), label("m7"), never, NOW).expect("a seeded name");
    let to = NameTo {
        num: num(86),
        nick: Some(Nickname::parse("MissBunny").expect("a nickname")),
    };
    for class in [Class::Chosen, Class::Seeded, Class::Auto] {
        let mut theirs = record(&other, Some(86), 3, Some("Otter"));
        theirs.claim.class = class;
        write_private(root.path(), PEERS_FILE, entry(&other, &theirs).as_bytes())
            .expect("writes their signed claim");
        assert!(
            set_name(
                root.path(),
                &to,
                no_labels,
                ByKind::Person,
                unread,
                NOW,
                false
            )
            .is_err()
        );
        assert_eq!(
            file::read(root.path()).expect("reads"),
            Some(before.clone())
        );
    }
    let peer = Peer::own(&other, ("m86.local", 7485, "0.160.0"), NOW)
        .with_primary_name(&other, Some("m86"));
    write_private(root.path(), PEERS_FILE, (peer.line() + "\n").as_bytes())
        .expect("writes an older signed primary");
    assert!(
        set_name(
            root.path(),
            &to,
            no_labels,
            ByKind::Person,
            unread,
            NOW,
            false
        )
        .is_err()
    );
    assert_eq!(file::read(root.path()).expect("reads"), Some(before));
    assert_eq!(
        read_node_identity(root.path())
            .expect("reads")
            .expect("our identity")
            .node(),
        me.node()
    );
}

#[test]
fn an_unread_book_never_counts_as_a_free_number() {
    let root = state();
    let (before, _) = ensure(root.path(), label("m7"), never, NOW).expect("a seeded name");
    write_private(root.path(), PEERS_FILE, b"not a peer\n").expect("a malformed book");
    let to = NameTo {
        num: num(86),
        nick: None,
    };
    assert!(
        set_name(
            root.path(),
            &to,
            no_labels,
            ByKind::Person,
            unread,
            NOW,
            false
        )
        .is_err()
    );
    assert_eq!(file::read(root.path()).expect("reads"), Some(before));
    assert!(
        set_nick(
            root.path(),
            &NickTo::Word(Nickname::parse("Heron").unwrap()),
            ByKind::Person,
            unread,
            NOW,
            false
        )
        .is_err()
    );
}

#[test]
fn a_signed_name_arriving_after_preview_refuses_both_fields_at_write() {
    let root = state();
    let (before, _) = ensure(root.path(), label("m7"), never, NOW).unwrap();
    let nick = Nickname::parse("Blue Heron").unwrap();
    let preview = rename_preview(root.path(), no_labels, unread, NOW).unwrap();
    assert!(!preview.shared(&nick));
    let other = NodeIdentity::from_seed(&"b2".repeat(16), &[0x22; 32]).unwrap();
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                aterm_net::fleet_member::write_held(
                    root.path(),
                    aterm_net::fleet_member::Began::Here,
                    |_| {
                        write_private(
                            root.path(),
                            PEERS_FILE,
                            entry(&other, &record(&other, Some(86), 3, Some("BLUE HERON")))
                                .as_bytes(),
                        )
                    },
                )
                .unwrap()
                .unwrap();
            })
            .join()
            .unwrap();
    });
    let to = NameTo {
        num: num(8),
        nick: Some(nick),
    };
    for dry_run in [true, false] {
        assert!(
            set_name(
                root.path(),
                &to,
                no_labels,
                ByKind::Person,
                unread,
                NOW,
                dry_run
            )
            .is_err()
        );
        assert_eq!(file::read(root.path()).unwrap(), Some(before.clone()));
    }
}

#[test]
fn concurrent_combined_edits_leave_one_complete_choice() {
    let root = state();
    ensure(root.path(), label("m7"), never, NOW).unwrap();
    node_identity(root.path()).unwrap();
    let choices = [
        NameTo {
            num: num(8),
            nick: Some(Nickname::parse("Heron").unwrap()),
        },
        NameTo {
            num: num(9),
            nick: Some(Nickname::parse("Otter").unwrap()),
        },
    ];
    let start = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        let threads: Vec<_> = choices
            .iter()
            .map(|to| {
                scope.spawn(|| {
                    start.wait();
                    set_name(
                        root.path(),
                        to,
                        no_labels,
                        ByKind::Person,
                        unread,
                        NOW,
                        false,
                    )
                    .unwrap();
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
    });
    let after = file::read(root.path()).unwrap().unwrap();
    assert!(
        choices
            .iter()
            .any(|to| after.num == Some(to.num) && Some(&after.nick) == to.nick.as_ref())
    );
    assert_eq!(after.rev, 2);
}

#[test]
fn name_uniqueness_uses_the_newest_verified_record_in_both_books() {
    let root = state();
    node_identity(root.path()).unwrap();
    ensure(root.path(), label("m7"), never, NOW).unwrap();
    let other = NodeIdentity::from_seed(&"b2".repeat(16), &[0x22; 32]).unwrap();
    write_private(
        root.path(),
        ISSUER_BOOK_FILE,
        entry(&other, &record(&other, Some(86), 3, Some("Blue Heron"))).as_bytes(),
    )
    .unwrap();
    write_private(
        root.path(),
        PEERS_FILE,
        entry(&other, &record(&other, Some(86), 4, Some("Otter"))).as_bytes(),
    )
    .unwrap();
    let preview = rename_preview(root.path(), no_labels, unread, NOW).unwrap();
    assert!(!preview.shared(&Nickname::parse("Blue Heron").unwrap()));
    assert!(preview.shared(&Nickname::parse("OTTER").unwrap()));
    set_nick(
        root.path(),
        &NickTo::Word(Nickname::parse("Blue Heron").unwrap()),
        ByKind::Person,
        unread,
        NOW,
        false,
    )
    .unwrap();
    assert!(
        set_nick(
            root.path(),
            &NickTo::Word(Nickname::parse("otter").unwrap()),
            ByKind::Person,
            unread,
            NOW,
            false
        )
        .is_err()
    );

    let signed = entry(&other, &record(&other, Some(86), 5, Some("Puffin")));
    let tampered = signed.replace("m9.local", "m10.local");
    assert_ne!(tampered, signed);
    write_private(root.path(), PEERS_FILE, tampered.as_bytes()).unwrap();
    let preview = rename_preview(root.path(), no_labels, unread, NOW).unwrap();
    assert!(!preview.shared(&Nickname::parse("Puffin").unwrap()));
    set_nick(
        root.path(),
        &NickTo::Word(Nickname::parse("Puffin").unwrap()),
        ByKind::Person,
        unread,
        NOW,
        false,
    )
    .unwrap();
}

#[test]
fn an_exhausted_default_name_pool_leaves_first_creation_unwritten() {
    let root = state();
    let why = ensure(root.path(), || Ok(None), |_| true, NOW).unwrap_err();
    assert_eq!(why.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(file::read(root.path()).unwrap(), None);
}

#[test]
fn an_exhausted_random_name_pool_cannot_save_a_duplicate() {
    let root = state();
    node_identity(root.path()).unwrap();
    let (before, _) = ensure(root.path(), label("m7"), never, NOW).unwrap();
    let mut books = [String::new(), String::new()];
    for (index, noun) in nouns::NOUNS.iter().enumerate() {
        let number = u32::try_from(index + 1).unwrap();
        let other = NodeIdentity::from_seed(&format!("{number:032x}"), &[0x22; 32]).unwrap();
        books[index % books.len()]
            .push_str(&entry(&other, &record(&other, Some(number), 1, Some(noun))));
    }
    for (file, book) in [PEERS_FILE, ISSUER_BOOK_FILE].into_iter().zip(books) {
        write_private(root.path(), file, book.as_bytes()).unwrap();
    }
    let why = set_nick(
        root.path(),
        &NickTo::Random,
        ByKind::Person,
        unread,
        NOW,
        false,
    )
    .unwrap_err();
    assert_eq!(why.kind(), io::ErrorKind::AlreadyExists, "{why}");
    assert_eq!(file::read(root.path()).unwrap(), Some(before));
}

#[test]
fn an_unread_fleet_key_cannot_turn_its_occupied_number_into_a_free_one() {
    use aterm_net::fleet::{KEY_FILE, read_key_file};
    let root = state();
    let (before, _) = ensure(root.path(), label("m7"), never, NOW).expect("a seeded name");
    write_private(root.path(), KEY_FILE, ("ab".repeat(32) + "\n").as_bytes()).expect("a fleet key");
    let mark = read_key_file(root.path())
        .expect("reads the fleet key")
        .mark();
    let other = NodeIdentity::from_seed(&"b2".repeat(16), &[0x22; 32]).expect("another identity");
    let theirs = record(&other, Some(86), 3, Some("Otter"));
    let mut peer = Peer::own(&other, ("m86.local", 7485, "0.160.0"), NOW);
    peer.fleet = Some(mark);
    peer.signed = peer
        .signed
        .map(|signed| signed.with_record(theirs.sign(&other, ZERO_DIGEST)));
    write_private(root.path(), PEERS_FILE, (peer.line() + "\n").as_bytes()).expect("a signed peer");
    let to = NameTo {
        num: num(86),
        nick: None,
    };
    assert!(
        set_name(
            root.path(),
            &to,
            no_labels,
            ByKind::Person,
            unread,
            NOW,
            false
        )
        .is_err()
    );
    write_private(root.path(), KEY_FILE, b"not a fleet secret\n").expect("a damaged key");
    let why = set_name(
        root.path(),
        &to,
        no_labels,
        ByKind::Person,
        unread,
        NOW,
        false,
    )
    .expect_err("a damaged key must stop the rename");
    assert!(why.to_string().contains(KEY_FILE), "{why}");
    assert_eq!(file::read(root.path()).expect("reads"), Some(before));
}

#[test]
fn a_newer_wrapper_cannot_roll_back_or_strip_a_verified_number() {
    let root = state();
    let (before, _) = ensure(root.path(), label("m7"), never, NOW).expect("a seeded name");
    let other = NodeIdentity::from_seed(&"b2".repeat(16), &[0x22; 32]).expect("another identity");
    let mut latest = record(&other, Some(86), 3, Some("Otter"));
    latest.eseq = 20;
    let mut stale = record(&other, Some(40), 2, Some("Puffin"));
    stale.eseq = 10;
    write_private(root.path(), PEERS_FILE, entry(&other, &latest).as_bytes())
        .expect("the latest record");
    let newer_wrapper = Peer::own(&other, ("m40.local", 7485, "0.160.0"), NOW + 1);
    let mut replay = newer_wrapper.clone();
    replay.signed = replay
        .signed
        .map(|signed| signed.with_record(stale.sign(&other, ZERO_DIGEST)));
    let to = NameTo {
        num: num(86),
        nick: None,
    };
    for peer in [replay, newer_wrapper] {
        write_private(
            root.path(),
            ISSUER_BOOK_FILE,
            (peer.line() + "\n").as_bytes(),
        )
        .expect("a newer wrapper with a stale or stripped record");
        let preview = rename_preview(root.path(), no_labels, unread, NOW).unwrap();
        assert!(preview.shared(&Nickname::parse("OTTER").unwrap()));
        assert!(!preview.shared(&Nickname::parse("Puffin").unwrap()));
        assert!(
            set_name(
                root.path(),
                &NameTo {
                    num: num(8),
                    nick: Some(Nickname::parse("otter").unwrap())
                },
                no_labels,
                ByKind::Person,
                unread,
                NOW,
                false,
            )
            .is_err()
        );
        assert!(
            set_name(
                root.path(),
                &to,
                no_labels,
                ByKind::Person,
                unread,
                NOW,
                false
            )
            .is_err()
        );
        assert_eq!(
            file::read(root.path()).expect("reads"),
            Some(before.clone())
        );
    }
}

#[test]
fn choosing_the_held_seeded_number_never_weakens_its_claim_against_a_rival() {
    let root = state();
    let (before, _) = ensure(root.path(), label("m7"), never, NOW).expect("a seeded name");
    let other = NodeIdentity::from_seed(&"b2".repeat(16), &[0x22; 32]).expect("another identity");
    write_private(
        root.path(),
        PEERS_FILE,
        entry(&other, &record(&other, Some(7), 3, None)).as_bytes(),
    )
    .expect("a rival's later chosen claim");
    let to = NameTo {
        num: num(7),
        nick: None,
    };
    let same = set_name(
        root.path(),
        &to,
        no_labels,
        ByKind::Person,
        unread,
        NOW,
        false,
    )
    .expect("the current seeded holder");
    assert!(same.changed);
    assert_eq!(same.name.class, Class::Chosen);
    let node = node_of(node_identity(root.path()).expect("our identity").node()).expect("a node");
    let mut view = rename_books(root.path(), NOW)
        .expect("reads the signed rival")
        .view;
    view.insert(node, Entry::Record(current_record(&same.name)));
    assert_eq!(
        settle(&view).standings.get(&node),
        Some(&aterm_types::machine_name::Standing::Holds(num(7))),
        "choosing the current seeded number must preserve its winning claim"
    );
    assert_eq!(same.name.since, before.since);
    let to = NameTo {
        num: num(7),
        nick: Some(Nickname::parse("MissBunny").expect("a nickname")),
    };
    let nick = set_name(
        root.path(),
        &to,
        no_labels,
        ByKind::Person,
        unread,
        NOW,
        false,
    )
    .expect("only the nickname changes");
    assert!(nick.changed);
    assert_eq!(nick.name.title(), "m7 MissBunny");
    assert_eq!(
        (nick.name.class, nick.name.since, nick.name.act),
        (same.name.class, same.name.since, same.name.act)
    );
    assert_eq!(
        (&nick.name.prev, &nick.name.was),
        (&same.name.prev, &same.name.was)
    );
    assert_eq!(
        (nick.name.num, nick.name.rev),
        (Some(num(7)), same.name.rev + 1),
        "a nickname chosen with the held number"
    );
}

#[test]
fn an_upgraded_machine_keeps_its_older_name_pending_migration() {
    let root = state();
    write_private(root.path(), NAME_FILE, b"m7\n").expect("writes");
    let (name, origin) = ensure(root.path(), no_roster, never, NOW).expect("ensures");
    assert_eq!(origin, Origin::Evidence);
    assert_eq!(
        (name.num, name.class, name.since, name.migration),
        (
            Some(num(7)),
            Class::Seeded,
            IMPLIED_SINCE,
            Some(Migration::Pending)
        ),
        "the legacy fleet name when no roster label exists"
    );
    assert_eq!(name.nick_class, NickClass::Auto, "no word to keep: drawn");

    let root = state();
    write_private(root.path(), NAME_FILE, b"studio\n").expect("writes");
    let (name, origin) = ensure(root.path(), label("m12"), never, NOW).expect("ensures");
    assert_eq!(origin, Origin::Evidence);
    assert_eq!(
        name.title(),
        "m12 studio",
        "the roster's NAME, the fleet name's word"
    );
    assert_eq!(name.nick_class, NickClass::Chosen);
    assert_eq!(name.migration, Some(Migration::Pending));
}

#[test]
fn the_roster_precedes_legacy_names_and_old_own_signed_records() {
    let root = state();
    let me = node_identity(root.path()).expect("a node key");
    write_private(root.path(), NAME_FILE, b"m7\n").expect("the old fleet name");
    write_private(
        root.path(),
        PEERS_FILE,
        entry(&me, &record(&me, Some(9), 3, None)).as_bytes(),
    )
    .expect("writes the book");
    let (name, origin) = ensure(root.path(), label("m12"), never, NOW).expect("ensures");
    assert_eq!((name.num, origin), (Some(num(12)), Origin::Evidence));
}

#[test]
fn a_source_that_cannot_be_read_makes_nothing_and_the_next_reader_tries_again() {
    use aterm_net::fleet_member::NODE_KEY_FILE;
    let root = state();
    let busy = || -> io::Result<Option<String>> { Err(io::Error::other("the roster is busy")) };
    let refused = ensure(root.path(), busy, never, NOW).expect_err("an unread roster");
    assert!(refused.to_string().contains("busy"), "{refused}");
    assert_eq!(
        file::read(root.path()).expect("reads"),
        None,
        "nothing kept"
    );
    let (name, origin) = ensure(root.path(), label("m8"), never, NOW).expect("ensures");
    assert_eq!(
        (name.num, origin),
        (Some(num(8)), Origin::Evidence),
        "the next reader's roster"
    );

    for unreadable in [NAME_FILE, NODE_KEY_FILE] {
        let root = state();
        node_identity(root.path()).expect("a node key");
        write_private(root.path(), NAME_FILE, b"m7\n").expect("writes");
        let path = root.path().join(unreadable);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("opens it to others");
        let refused = ensure(root.path(), no_roster, never, NOW).expect_err(unreadable);
        assert!(refused.to_string().contains(unreadable), "{refused}");
        assert_eq!(
            file::read(root.path()).expect("reads"),
            None,
            "{unreadable}: nothing kept"
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("private again");
        let (name, _) = ensure(root.path(), no_roster, never, NOW).expect(unreadable);
        assert_eq!(name.num, Some(num(7)), "{unreadable}: the next reader's");
    }

    let root = state();
    write_private(root.path(), NAME_FILE, b"not a name!\n").expect("writes");
    let (name, origin) = ensure(root.path(), no_roster, never, NOW).expect("ensures");
    assert_eq!(
        (name.num, origin),
        (None, Origin::Drawn),
        "text that is no name is no evidence, and no failure"
    );
}

#[test]
fn a_kept_name_reads_whatever_the_node_keys_state() {
    use aterm_net::fleet_member::NODE_KEY_FILE;
    let root = state();
    node_identity(root.path()).expect("a node key");
    let (made, _) = ensure_in_view(root.path(), no_roster, NOW).expect("makes");
    let path = root.path().join(NODE_KEY_FILE);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
        .expect("opens it to others");
    let (kept, origin) = ensure_in_view(root.path(), unread, NOW).expect("reads the kept name");
    assert_eq!((kept, origin), (made, Origin::Kept));
    std::fs::remove_file(root.path().join(file::FILE)).expect("removes");
    let refused = ensure_in_view(root.path(), no_roster, NOW).expect_err("makes none");
    assert!(refused.to_string().contains(NODE_KEY_FILE), "{refused}");
}

#[test]
fn a_private_state_root_reads_no_vault_roster() {
    assert!(aterm_types::dirs::runs_under_private_state_root());
    assert_eq!(vault_roster_label().expect("reads no vault"), None);
}

#[test]
fn only_a_bare_name_is_older_evidence() {
    for roster in ["m8-macbook-pro-m5-128gb-8tb", "8", "M8", "m08"] {
        let root = state();
        let (name, origin) = ensure(root.path(), label(roster), never, NOW).expect("ensures");
        assert_eq!((name.num, origin), (None, Origin::Drawn), "{roster}");
    }
    let root = state();
    let (name, origin) = ensure(root.path(), label("m8"), never, NOW).expect("ensures");
    assert_eq!(
        (name.num, origin),
        (Some(num(8)), Origin::Evidence),
        "control"
    );
}

#[test]
fn the_nicknames_taken_are_other_nodes_records_under_the_held_seed() {
    let root = state();
    let me = node_identity(root.path()).expect("a node");
    let other = NodeIdentity::from_seed(&"b2".repeat(16), &[0x22; 32]).expect("an identity");
    let mark = aterm_net::fleet::FleetMark::from_hex("0101010101010101").expect("a mark");
    let seed = aterm_net::fleet_member::fleet_seed::FleetSeed::mint(&me, mark, NOW).expect("mints");
    write_private(
        root.path(),
        aterm_net::fleet_member::fleet_seed::FILE,
        format!("{}\n", seed.words()).as_bytes(),
    )
    .expect("writes the seed");
    write_private(
        root.path(),
        ISSUER_BOOK_FILE,
        (entry(&me, &record(&me, Some(9), 3, Some("Heron")))
            + &entry_under(
                &other,
                &record(&other, Some(11), 4, Some("Puffin")),
                seed.digest(),
            ))
            .as_bytes(),
    )
    .expect("writes the book");
    assert_eq!(
        taken_in(root.path(), Some(me.node())),
        [Nickname::parse("Puffin").expect("a nickname")],
        "the other node's, under the seed held here; never this node's own"
    );
}

#[test]
fn a_nickname_in_view_is_one_in_any_case() {
    let taken = [Nickname::parse("Heron").expect("a nickname")];
    assert!(in_view(&taken, "heron") && in_view(&taken, "HERON"));
    assert!(!in_view(&taken, "otter"), "control");
    let taken = [
        Nickname::parse("Blue Heron").unwrap(),
        Nickname::parse("Élan").unwrap(),
    ];
    assert!(in_view(&taken, " BLUE\t heron "));
    assert!(in_view(&taken, "ÉLAN"));
}

#[test]
fn a_nickname_change_is_chosen_raises_rev_and_keeps_the_claim() {
    let heron = || Nickname::parse("heron").expect("a nickname");
    let held = MachineName::unnumbered(heron(), NOW);
    let chosen = renicked(&held, heron()).expect("a drawn noun chosen is a change");
    assert_eq!(
        (chosen.nick_class, chosen.rev),
        (NickClass::Chosen, 1),
        "every changed local nickname advances its revision"
    );
    assert_eq!(renicked(&chosen, heron()), None, "chosen already");
    let seeded = MachineName { rev: 4, ..chosen };
    let respelled = renicked(&seeded, Nickname::parse("Heron").expect("a nickname"))
        .expect("another spelling is another nickname");
    assert_eq!(respelled.rev, 5, "a seeded file's change raises rev");
    assert_eq!(
        (
            respelled.num,
            respelled.class,
            respelled.since,
            respelled.act
        ),
        (held.num, held.class, held.since, held.act),
        "the claim as it was"
    );
}

/// IN A FLEET IS THE VAULT'S STANDING (§5.2): a member is in; let in, asking or seatless is
/// joining; never in, taken out or let go is out; an unreadable state says why; and a fleet key
/// that reads stands for membership, until the one-mesh design's D2, wherever the vault did not
/// take this Mac out or let it go (`members_cli`'s
/// `a_vault_that_took_this_mac_out_outranks_its_fleet_key` reads a real one).
#[test]
fn in_a_fleet_is_the_vaults_standing_or_a_fleet_key() {
    use aterm_secrets::members::join::JoinPlace;
    let cases = [
        (Standing::Member("m8".to_owned()), InFleet::In),
        (Standing::Seatless, InFleet::Joining),
        (
            Standing::Allowed {
                fix: None,
                held: false,
            },
            InFleet::Joining,
        ),
        (Standing::Asking(JoinPlace::Unknown), InFleet::Joining),
        (Standing::NotIn, InFleet::Out),
        (Standing::Dropped("let go".to_owned()), InFleet::Out),
        (
            Standing::Unknown("state.toml: bad".to_owned()),
            InFleet::Unread("state.toml: bad".to_owned()),
        ),
    ];
    for (standing, want) in &cases {
        assert_eq!(&InFleet::of(false, false, standing), want, "{standing:?}");
        let keyed = if matches!(standing, Standing::Dropped(_)) {
            InFleet::Out
        } else {
            InFleet::In
        };
        assert_eq!(
            InFleet::of(true, false, standing),
            keyed,
            "a key: {standing:?}"
        );
        assert_eq!(
            InFleet::of(true, true, standing),
            InFleet::Out,
            "taken out, with a key: {standing:?}"
        );
    }
    let words: Vec<_> = [
        InFleet::In,
        InFleet::Joining,
        InFleet::Out,
        InFleet::Paused(String::new()),
        InFleet::Unread(String::new()),
    ]
    .iter()
    .map(|f| (f.word(), f.phrase()))
    .collect();
    assert_eq!(
        words,
        [
            ("in", "in a fleet"),
            ("joining", "joining a fleet"),
            ("out", "not in a fleet"),
            ("paused", "fleet paused"),
            ("unread", "fleet not read"),
        ]
    );
}

/// A retained key is active only under admitted custody; an explicit take restores it.
/// Under a private state root (a test binary) no vault is read: `$HOME`'s is the person's.
#[test]
fn a_fleet_key_reports_its_custody_without_discarding_the_key() {
    let root = state();
    let vault = aterm_secrets::client::Paths::locate_for_home(&root.path().join("home"));
    assert_eq!(InFleet::read(root.path(), Some(&vault)), InFleet::Out);
    let key = format!("{}\n", "ab".repeat(32));
    write_private(root.path(), aterm_net::fleet::KEY_FILE, key.as_bytes()).expect("a fleet key");
    assert!(matches!(
        InFleet::read(root.path(), Some(&vault)),
        InFleet::Paused(_)
    ));
    assert_eq!(InFleet::read(root.path(), None), InFleet::In);
    crate::fleet_vault::take_fixture(root.path(), Some(&vault)).unwrap();
    assert_eq!(InFleet::read(root.path(), Some(&vault)), InFleet::In);
    aterm_secrets::fsx::ensure_private_dir(&vault.dir).unwrap();
    let source = vault.dir.join("source.toml");
    for name in ["first", "second"] {
        aterm_secrets::fsx::write_atomic(
            &source,
            format!(
                "url = \"http://127.0.0.1:9/v1/{name}\"\nname = \"{name}\"\nsigner = \"{}\"\nhost = \"m22\"\n",
                aterm_secrets::b64::encode_padded(&[7; 32]),
            ).as_bytes(),
            0o600,
        ).unwrap();
        assert!(matches!(
            InFleet::read(root.path(), Some(&vault)),
            InFleet::Paused(_)
        ));
        assert_eq!(
            std::fs::read(root.path().join(aterm_net::fleet::KEY_FILE)).unwrap(),
            key.as_bytes()
        );
        crate::fleet_vault::take_fixture(root.path(), Some(&vault)).unwrap();
        assert_eq!(InFleet::read(root.path(), Some(&vault)), InFleet::In);
    }
    assert!(super::standing_vault().is_none(), "a test reads no vault");
}

/// The vault's labels on today's lab (§5.3): a number its roster or log gives another Mac is
/// refused with what holds it (`Cause::Vault`, each hold its own), changing nothing; the same
/// number with no labels read renames, which shows the labels are what refuse.
#[test]
fn a_number_the_vault_gives_another_mac_is_refused_with_its_holder() {
    let root = state();
    let (before, _) = ensure(root.path(), label("m7"), never, NOW).expect("a seeded name");
    let to = NameTo {
        num: num(20),
        nick: Some(Nickname::parse("Heron").expect("a nickname")),
    };
    for hold in [
        LabelHold::Label,
        LabelHold::Alias,
        LabelHold::Claim,
        LabelHold::VaultName,
        LabelHold::Seat,
        LabelHold::Denied,
    ] {
        let lab = LabLabels {
            held: BTreeMap::from([(num(20), hold)]),
            ..LabLabels::default()
        };
        let why = set_name(
            root.path(),
            &to,
            || Ok(lab.clone()),
            ByKind::Person,
            unread,
            NOW,
            false,
        )
        .expect_err("another Mac's number");
        assert_eq!(
            Refused::of(&why).map(|refused| &refused.cause),
            Some(&Cause::Vault(hold)),
            "{why}"
        );
        assert_eq!(
            file::read(root.path()).expect("reads"),
            Some(before.clone()),
            "neither the number nor the nickname changed"
        );
    }
    let set = set_name(
        root.path(),
        &to,
        no_labels,
        ByKind::Person,
        unread,
        NOW,
        false,
    )
    .expect("free with nothing held");
    assert_eq!(set.name.title(), "m20 Heron");
}

/// A move off the number the policy dials this Mac's vault server by is refused while the macOS
/// names follow that number: the roster label until a number is chosen, then the chosen one. A
/// rename that keeps it, or a server the policy names by another number, is not.
#[test]
fn a_move_off_the_policys_server_name_is_refused() {
    let root = state();
    let (before, _) = ensure(root.path(), label("m8"), never, NOW).expect("a seeded name");
    let serves = |label: u32, server: u32| LabLabels {
        own: BTreeSet::from([num(label)]),
        label: Some(num(label)),
        servers: BTreeSet::from([num(server)]),
        ..LabLabels::default()
    };
    let to = |n: u32| NameTo {
        num: num(n),
        nick: None,
    };
    let why = set_name(
        root.path(),
        &to(87),
        || Ok(serves(8, 8)),
        ByKind::Person,
        unread,
        NOW,
        false,
    )
    .expect_err("the server's name");
    assert_eq!(
        Refused::of(&why).map(|refused| &refused.cause),
        Some(&Cause::Server(num(8))),
        "{why}"
    );
    assert_eq!(file::read(root.path()).expect("reads"), Some(before));
    // Negative control: a server the policy names by another number lets the move through.
    set_name(
        root.path(),
        &to(87),
        || Ok(serves(8, 9)),
        ByKind::Person,
        unread,
        NOW,
        false,
    )
    .expect("this Mac serves no one by m8");
    // Chosen, the macOS names follow m87: a policy naming m8 no longer dials this Mac by it.
    set_name(
        root.path(),
        &to(88),
        || Ok(serves(8, 8)),
        ByKind::Person,
        unread,
        NOW + 1,
        false,
    )
    .expect("m87 is what the macOS names follow");
    let why = set_name(
        root.path(),
        &to(89),
        || Ok(serves(8, 88)),
        ByKind::Person,
        unread,
        NOW + 2,
        false,
    )
    .expect_err("the chosen number is the server's name");
    assert_eq!(
        Refused::of(&why).map(|refused| &refused.cause),
        Some(&Cause::Server(num(88))),
        "{why}"
    );
    set_name(
        root.path(),
        &to(88),
        || Ok(serves(8, 88)),
        ByKind::Person,
        unread,
        NOW + 3,
        false,
    )
    .expect("staying on it is no move");
}

/// A newly explicit choice of a seeded number needs current labels. Once chosen, keeping
/// the number for a nickname edit reads no labels, while a number change still needs them.
#[test]
fn keeping_the_stored_number_reads_no_labels_but_a_number_change_requires_them() {
    let unreadable = || Err(io::Error::other("the vault is busy"));
    let root = state();
    let (seeded, _) = ensure(root.path(), label("m7"), never, NOW).expect("a seeded name");
    let same = NameTo {
        num: num(7),
        nick: None,
    };
    assert!(
        set_name(
            root.path(),
            &same,
            unreadable,
            ByKind::Person,
            unread,
            NOW,
            false
        )
        .is_err(),
        "a new explicit choice requires current labels even when its number was seeded"
    );
    assert_eq!(file::read(root.path()).unwrap(), Some(seeded.clone()));
    let held_elsewhere = || {
        Ok(LabLabels {
            held: BTreeMap::from([(num(7), client::LabelHold::Seat)]),
            ..LabLabels::default()
        })
    };
    assert!(
        set_name(
            root.path(),
            &same,
            held_elsewhere,
            ByKind::Person,
            unread,
            NOW,
            false
        )
        .is_err(),
        "another Mac's fresh seat on m7 prevents turning a stale seeded number into a chosen claim"
    );
    assert_eq!(file::read(root.path()).unwrap(), Some(seeded));
    let selected = set_name(
        root.path(),
        &same,
        no_labels,
        ByKind::Person,
        unread,
        NOW,
        false,
    )
    .expect("explicitly selects the seeded number");
    assert_eq!(selected.name.class, Class::Chosen);
    let to = NameTo {
        num: num(7),
        nick: Some(Nickname::parse("Heron").expect("a nickname")),
    };
    let set = set_name(
        root.path(),
        &to,
        unreadable,
        ByKind::Person,
        unread,
        NOW,
        false,
    )
    .expect("a nickname edit reads no labels");
    assert_eq!(set.name.title(), "m7 Heron");
    let renumber = NameTo {
        num: num(70),
        nick: None,
    };
    let why = set_name(
        root.path(),
        &renumber,
        unreadable,
        ByKind::Person,
        unread,
        NOW,
        false,
    )
    .expect_err("a number change needs its labels");
    assert_eq!(
        Refused::of(&why).map(|refused| &refused.cause),
        Some(&Cause::Unread),
        "{why}"
    );
}

/// No vault read holds nothing; a home with no vault set up holds nothing either.
#[test]
fn lab_labels_with_no_vault_hold_nothing() {
    let root = state();
    assert_eq!(lab_labels(None).expect("reads"), LabLabels::default());
    let vault = aterm_secrets::client::Paths::locate_for_home(&root.path().join("home"));
    assert_eq!(
        lab_labels(Some(&vault)).expect("reads"),
        LabLabels::default()
    );
}

/// Rename This Mac…'s live line judges each number from one read as [`set_name`] does, in its
/// words: a number another signed node or the vault holds is refused, a free one passes and this
/// Mac's chosen number is `Same`; unread labels refuse each new explicit claim.
#[test]
fn a_preview_judges_each_number_in_the_words_the_set_refuses_it() {
    let root = state();
    node_identity(root.path()).expect("our identity");
    let other = NodeIdentity::from_seed(&"b2".repeat(16), &[0x22; 32]).expect("another identity");
    ensure(root.path(), label("m7"), never, NOW).expect("a seeded name");
    let before = set_name(
        root.path(),
        &NameTo {
            num: num(7),
            nick: None,
        },
        no_labels,
        ByKind::Person,
        unread,
        NOW,
        false,
    )
    .expect("a chosen number")
    .name;
    let theirs = record(&other, Some(86), 3, Some("Otter"));
    write_private(root.path(), PEERS_FILE, entry(&other, &theirs).as_bytes())
        .expect("writes their signed claim");
    let lab = LabLabels {
        held: BTreeMap::from([(num(20), LabelHold::Label)]),
        ..LabLabels::default()
    };
    let preview = rename_preview(root.path(), || Ok(lab.clone()), unread, NOW).expect("a read");
    assert_eq!(preview.name(), &before);
    let theirs = node_of(other.node()).expect("their node");
    for (n, cause) in [
        (86, Cause::Verdict(Verdict::HeldByPerson { by: theirs })),
        (20, Cause::Vault(LabelHold::Label)),
    ] {
        let to = NameTo {
            num: num(n),
            nick: None,
        };
        let refused = set_name(
            root.path(),
            &to,
            || Ok(lab.clone()),
            ByKind::Person,
            unread,
            NOW,
            true,
        )
        .expect_err("held");
        let refused = Refused::of(&refused).expect("a refusal");
        assert_eq!(refused.cause, cause, "m{n}");
        assert_eq!(preview.judge(num(n)).as_ref(), Err(refused), "m{n}");
    }
    assert_eq!(preview.judge(num(7)), Ok(Verdict::Same));
    assert_eq!(preview.judge(num(87)), Ok(Verdict::Free));
    assert!(preview.shared(&Nickname::parse("otter").expect("a nickname")));
    assert!(!preview.shared(&Nickname::parse("Kestrel").expect("a nickname")));
    let unlabelled = rename_preview(
        root.path(),
        || Err(io::Error::other("the vault's labels are unread: busy")),
        unread,
        NOW,
    )
    .expect("a read");
    assert_eq!(
        unlabelled.judge(num(87)).map_err(|refused| refused.cause),
        Err(Cause::Unread)
    );
    assert_eq!(
        unlabelled.unchecked(num(87)),
        Some("the vault's labels are unread: busy")
    );
    assert_eq!(unlabelled.judge(num(7)), Ok(Verdict::Same));
    assert_eq!(
        unlabelled.unchecked(num(7)),
        None,
        "a kept number needs no labels"
    );
    assert_eq!(preview.unchecked(num(87)), None, "the labels were read");
    assert_eq!(
        file::read(root.path()).expect("reads"),
        Some(before),
        "a read changes no name"
    );
    // Keeping a chosen number needs no labels; a move still checks the policy's
    // server name, which now follows the chosen number rather than the roster label.
    let serves = LabLabels {
        own: BTreeSet::from([num(8)]),
        label: Some(num(8)),
        servers: BTreeSet::from([num(7)]),
        ..LabLabels::default()
    };
    let served = rename_preview(root.path(), || Ok(serves.clone()), unread, NOW).expect("a read");
    let keep = NameTo {
        num: num(7),
        nick: Some(Nickname::parse("Kestrel").expect("a nickname")),
    };
    set_name(
        root.path(),
        &keep,
        || Ok(serves.clone()),
        ByKind::Person,
        unread,
        NOW,
        true,
    )
    .expect("a kept number reads no labels");
    assert_eq!(served.judge(num(7)), Ok(Verdict::Same));
    let refused = set_name(
        root.path(),
        &NameTo {
            num: num(87),
            nick: None,
        },
        || Ok(serves.clone()),
        ByKind::Person,
        unread,
        NOW,
        true,
    )
    .expect_err("a move off the server's name");
    assert_eq!(served.judge(num(87)).as_ref().err(), Refused::of(&refused));
    // A number a migration waits on holds no claim: the read says so and judges it as a move, and
    // the set then makes the claim.
    assert!(!preview.waits());
    file::replace(root.path(), |name| {
        name.migration = Some(Migration::Waiting);
        Ok(())
    })
    .expect("a waiting number");
    let waiting = rename_preview(root.path(), no_labels, unread, NOW).expect("a read");
    assert!(waiting.waits());
    assert!(waiting.judge(num(7)).is_ok());
    let unlabelled =
        rename_preview(root.path(), || Err(io::Error::other("busy")), unread, NOW).expect("a read");
    assert_eq!(
        unlabelled.judge(num(7)).map_err(|refused| refused.cause),
        Err(Cause::Unread)
    );
    let claimed = set_name(
        root.path(),
        &NameTo {
            num: num(7),
            nick: None,
        },
        no_labels,
        ByKind::Person,
        unread,
        NOW,
        false,
    )
    .expect("the shown number is claimed");
    assert_eq!(
        (claimed.name.num, claimed.name.migration),
        (Some(num(7)), None)
    );
}

/// A number that moves the name the edit starts from is judged only with the vault's labels read:
/// with none it is refused, so a stored number that moved under the set's locks after its unlocked
/// read decided it kept the number is never renamed past the vault or the server rule; the number
/// held, or a read set of labels, passes.
#[test]
fn a_move_judged_without_the_vault_s_labels_is_refused() {
    let root = state();
    node_identity(root.path()).expect("our identity");
    ensure(root.path(), label("m7"), never, NOW).expect("a seeded name");
    let held = set_name(
        root.path(),
        &NameTo {
            num: num(7),
            nick: None,
        },
        no_labels,
        ByKind::Person,
        unread,
        NOW,
        false,
    )
    .expect("a chosen number")
    .name;
    let node = node_of(node_identity(root.path()).expect("our identity").node()).expect("a node");
    let view = BTreeMap::from([(node, Entry::Record(current_record(&held)))]);
    let settled = settle(&view);
    let fleet = Fleet {
        view: &view,
        settled: &settled,
        top: 7,
        kept: &[],
        labels: &BTreeMap::new(),
    };
    let refused = judge(num(87), &held, None, &fleet, node).expect_err("unread labels");
    assert_eq!(refused.cause, Cause::Moved, "{refused}");
    assert_eq!(
        judge(num(87), &held, Some(&LabLabels::default()), &fleet, node),
        Ok(Verdict::Free)
    );
    assert_eq!(judge(num(7), &held, None, &fleet, node), Ok(Verdict::Same));
    let mut waiting = held;
    waiting.migration = Some(Migration::Waiting);
    let refused = judge(num(7), &waiting, None, &fleet, node).expect_err("a waiting number moves");
    assert_eq!(refused.cause, Cause::Moved, "{refused}");
}

fn renamed_from_m7(root: &aterm_tempfile::TempDir) -> (MachineName, MachineName) {
    let (before, _) = ensure(root.path(), label("m7"), never, NOW).expect("a seeded name");
    let to = NameTo {
        num: num(86),
        nick: Some(Nickname::parse("MissBunny").expect("a nickname")),
    };
    let set = set_name(
        root.path(),
        &to,
        no_labels,
        ByKind::Person,
        unread,
        NOW + 1,
        false,
    )
    .expect("renames")
    .name;
    (before, set)
}

/// Keep a nickname-undo control distinct from the noun the fixture happened to draw.
fn different_nickname(name: &MachineName) -> Nickname {
    let next = if name.display_nick().eq_ignore_ascii_case("heron") {
        Nickname::parse("Otter").expect("another nickname")
    } else {
        Nickname::parse("Heron").expect("a nickname")
    };
    assert!(
        !name.display_nick().eq_ignore_ascii_case(next.as_str()),
        "the fixture must change the shown nickname"
    );
    next
}

/// Labels a test must never read: an undo that keeps the number reads none.
fn unreadable() -> io::Result<LabLabels> {
    Err(io::Error::other("the vault is busy"))
}

/// An owner's write `op` to this Mac's row, as History lists it: each cell's `to`, standing,
/// over `from` (`None`: the row held none there).
fn owner_write(op: u8, cells: &[(Renamed, Option<&str>, &str)]) -> history::NameWrite {
    history::NameWrite {
        op: [op; 32],
        at_ms: NOW + u64::from(op),
        mac: "m8".to_owned(),
        kind: aterm_db::att::EntryKind::Explicit,
        reverts: None,
        carried: false,
        cells: cells
            .iter()
            .map(|(cell, from, to)| history::NameCell {
                cell: *cell,
                value: Some((*to).to_owned()),
                stands: true,
                replaced: vec![from.map(str::to_owned)],
                unseen: Vec::new(),
            })
            .collect(),
    }
}

/// This Mac's row holding `writes`, newest first.
fn row(writes: Vec<history::NameWrite>) -> history::History {
    history::History {
        row: Some("mc-0123456789abcdef".to_owned()),
        pending: Vec::new(),
        writes,
    }
}

/// What Undo makes of this Mac's row holding `writes`, newest first ([`history::undo_of`]).
fn undo_over(writes: Vec<history::NameWrite>) -> Result<history::Undo, Refused> {
    history::undo_of(&row(writes))
}

/// A History whose undo is `seen`: its rename, `seen.op`, wrote what `seen.shown` holds over
/// what `seen.cells` gives back, and an automatic write holds each other cell's value.
fn row_of(seen: &history::Undo) -> history::History {
    let mut rename = owner_write(0, &[]);
    rename.op = seen.op;
    let mut other = history::NameWrite {
        kind: aterm_db::att::EntryKind::Aterm,
        ..owner_write(1, &[])
    };
    for (cell, value) in &seen.shown {
        let written = history::NameCell {
            cell: *cell,
            value: Some(value.clone()),
            stands: true,
            replaced: Vec::new(),
            unseen: Vec::new(),
        };
        match seen.cells.iter().find(|(back, _)| back == cell) {
            Some((_, back)) => rename.cells.push(history::NameCell {
                replaced: vec![Some(back.clone())],
                ..written
            }),
            None if !value.is_empty() => other.cells.push(written),
            None => {}
        }
    }
    let history = row(vec![rename, other]);
    assert_eq!(
        history::undo_of(&history).as_ref(),
        Ok(seen),
        "a History that gives the undo"
    );
    history
}

/// The undo of the rename [`renamed_from_m7`] made, as the row's History gives it: `m7` and the
/// nickname `before` showed.
fn rename_undo(before: &MachineName) -> history::Undo {
    let shown = before.display_nick();
    undo_over(vec![owner_write(
        2,
        &[
            (Renamed::Label, Some("m7"), "m86"),
            (Renamed::Nickname, Some(&shown), "MissBunny"),
        ],
    )])
    .expect("an undo")
}

/// The undo of a nickname rename from `from` to `to`, as the row's History gives it.
fn nickname_undo(from: &str, to: &str) -> history::Undo {
    undo_over(vec![owner_write(3, &[(Renamed::Nickname, Some(from), to)])])
        .expect("a nickname undo")
}

/// [`undo_last`] of `seen` in `root` while History still gives it ([`history::with_history`]),
/// with the writes it queued to the row ([`registry::with_door`]).
fn undo_shown(
    root: &aterm_tempfile::TempDir,
    seen: &history::Undo,
    lab: impl FnOnce() -> io::Result<LabLabels>,
    kind: ByKind,
    now: u64,
    dry_run: bool,
) -> (io::Result<NameSet>, Vec<Vec<String>>) {
    registry::with_door(registry::Written::Unbound, || {
        history::with_history(Ok(row_of(seen)), || {
            undo_last(root.path(), seen, lab, kind, unread, now, dry_run)
        })
    })
}

/// Undo takes the row's last rename back: the number and nickname it replaced return, as a new
/// chosen claim and never the seeded rank it replaced; the preview judges the same undo, and a
/// dry run writes nothing. A nickname rename's undo returns the nickname alone, reads no labels
/// and leaves the claim as it is.
/// NEGATIVE CONTROL: with `undo_last` giving `change_claim` the number alone (`NameTo { nick:
/// None, .. }`), the undone title keeps `MissBunny`.
#[test]
fn undo_returns_to_the_number_and_nickname_the_last_change_replaced() {
    let root = state();
    let (before, set) = renamed_from_m7(&root);
    let seen = rename_undo(&before);
    assert_eq!(
        seen.to,
        UndoTo::Name(NameTo {
            num: num(7),
            nick: Some(Nickname::parse(&before.display_nick()).expect("a nickname")),
        })
    );
    let preview = rename_preview(root.path(), no_labels, unread, NOW + 2).expect("a read");
    assert_eq!(preview.undo(&seen), Ok(()));
    let (dry, asked) = undo_shown(&root, &seen, no_labels, ByKind::Agent, NOW + 2, true);
    let dry = dry.expect("a dry run");
    assert!(dry.changed);
    assert!(asked.is_empty(), "a dry run queues nothing: {asked:?}");
    assert_eq!(
        file::read(root.path()).expect("reads"),
        Some(set.clone()),
        "a dry run writes nothing"
    );
    let (undone, _) = undo_shown(&root, &seen, no_labels, ByKind::Agent, NOW + 2, false);
    let undone = undone.expect("undoes").name;
    assert_eq!(undone, dry.name, "the dry run said what the undo does");
    assert_eq!(undone.title(), before.title());
    assert_eq!(
        (undone.class, undone.nick_class, undone.migration),
        (Class::Chosen, NickClass::Chosen, None),
        "a new explicit claim and a chosen nickname"
    );
    assert_ne!(undone.since, before.since, "never the seeded rank");
    assert_eq!(
        undone.was.iter().map(|was| was.num).collect::<Vec<_>>(),
        [num(86)]
    );
    assert_eq!(undone.rev, set.rev + 1);
    assert_eq!(
        file::read(root.path()).expect("reads"),
        Some(undone.clone())
    );

    let next = different_nickname(&before);
    let nicked = set_nick(
        root.path(),
        &NickTo::Word(next.clone()),
        ByKind::Person,
        unread,
        NOW + 3,
        false,
    )
    .expect("a nickname change")
    .name;
    let seen = nickname_undo(&before.display_nick(), next.as_str());
    let preview = rename_preview(root.path(), unreadable, unread, NOW + 4).expect("a read");
    assert_eq!(preview.undo(&seen), Ok(()));
    let (back, _) = undo_shown(&root, &seen, unreadable, ByKind::Person, NOW + 4, false);
    let back = back.expect("a nickname undo reads no labels").name;
    assert_eq!(
        (back.display_nick(), back.nick_class),
        (before.display_nick(), NickClass::Chosen),
        "the nickname as it was shown, chosen by the undo"
    );
    assert_eq!(
        (back.num, back.class, back.since, back.act, &back.was),
        (
            nicked.num,
            nicked.class,
            nicked.since,
            nicked.act,
            &nicked.was
        ),
        "the claim as it was"
    );
}

/// An undo is one owner write of every cell it gives back, the number and nickname together, as
/// whoever asked: the KI-1114-532 case, an owner's label restored over the automatic one it
/// replaced, is written as the owner's (registration does not write over it), where an `aterm
/// file revert` would have restored registration's automatic write. A nickname that the rename
/// left as it was is no part of the write.
/// NEGATIVE CONTROL: with `edit_name` queueing each undo cell as its own `queue_rename`, the door
/// records two writes for the rename of both.
#[test]
fn an_undo_writes_its_cells_as_one_owner_write() {
    let root = state();
    let (before, _) = renamed_from_m7(&root);
    let (undone, asked) = undo_shown(
        &root,
        &rename_undo(&before),
        no_labels,
        ByKind::Person,
        NOW + 2,
        false,
    );
    undone.expect("undoes");
    assert_eq!(
        asked,
        [[
            "label=m7".to_owned(),
            format!("nickname={}", before.display_nick()),
            "by=person".to_owned()
        ]],
        "one write of both cells, a person's"
    );

    let number = state();
    ensure(number.path(), label("m7"), never, NOW).expect("registration's m7");
    set_name(
        number.path(),
        &NameTo {
            num: num(8),
            nick: None,
        },
        no_labels,
        ByKind::Person,
        unread,
        NOW + 1,
        false,
    )
    .expect("the owner's m8");
    let mut born = history::NameWrite {
        kind: aterm_db::att::EntryKind::Aterm,
        ..owner_write(1, &[(Renamed::Label, None, "m7")])
    };
    born.cells[0].stands = false;
    born.cells[0].replaced.clear();
    let seen = undo_over(vec![
        owner_write(2, &[(Renamed::Label, Some("m7"), "m8")]),
        born,
    ])
    .expect("the owner's rename");
    let (undone, asked) = undo_shown(&number, &seen, no_labels, ByKind::Agent, NOW + 2, false);
    let undone = undone.expect("undoes").name;
    assert_eq!(asked, [["label=m7", "by=program"]]);
    assert_eq!(
        (undone.num, undone.class),
        (Some(num(7)), Class::Chosen),
        "m7 comes back as an owner's"
    );
}

/// An undo of an undo redoes the rename: the undo is the row's newest owner write, so taking it
/// back gives back what the undo replaced.
/// NEGATIVE CONTROL: with `undo_of` passing over a write that wrote what the write before it
/// replaced (an undo, as the file's History did), the second undo is refused.
#[test]
fn undoing_an_undo_redoes_the_rename() {
    let root = state();
    let (before, set) = renamed_from_m7(&root);
    let (undone, _) = undo_shown(
        &root,
        &rename_undo(&before),
        no_labels,
        ByKind::Person,
        NOW + 2,
        false,
    );
    assert_eq!(undone.expect("undoes").name.title(), before.title());
    let shown = before.display_nick();
    let mut rename = owner_write(
        2,
        &[
            (Renamed::Label, Some("m7"), "m86"),
            (Renamed::Nickname, Some(&shown), "MissBunny"),
        ],
    );
    for cell in &mut rename.cells {
        cell.stands = false;
    }
    let redo = undo_over(vec![
        owner_write(
            3,
            &[
                (Renamed::Label, Some("m86"), "m7"),
                (Renamed::Nickname, Some("MissBunny"), &shown),
            ],
        ),
        rename,
    ])
    .expect("the undo is a rename");
    let (redone, asked) = undo_shown(&root, &redo, no_labels, ByKind::Person, NOW + 3, false);
    assert_eq!(redone.expect("redoes").name.title(), set.title());
    assert_eq!(asked, [["label=m86", "nickname=MissBunny", "by=person"]]);
}

/// Undo is refused once another Mac holds the old number by a person's choice, with that
/// holder's verdict, and nothing changes. Controls: an automatic holder gives `TakesAuto`, which
/// an undo refuses too, since it never displaces a holder; an older build's implied claim gives
/// `HeldByOlderBuild`, today's production case, since no shipped code signs a record; and with
/// no holder the same undo goes through.
#[test]
fn undo_is_refused_when_another_mac_now_holds_the_old_number_by_choice() {
    let root = state();
    node_identity(root.path()).expect("our identity");
    let (before, set) = renamed_from_m7(&root);
    let seen = rename_undo(&before);
    let other = NodeIdentity::from_seed(&"b2".repeat(16), &[0x22; 32]).expect("another identity");
    let theirs = node_of(other.node()).expect("their node");
    let mut auto = record(&other, Some(7), 3, Some("Otter"));
    auto.claim.class = Class::Auto;
    let implied = Peer::own(&other, ("m7.local", 7485, "0.160.0"), NOW)
        .with_primary_name(&other, Some("m7"))
        .line()
        + "\n";
    for (book, verdict) in [
        (
            entry(&other, &record(&other, Some(7), 3, Some("Otter"))),
            Verdict::HeldByPerson { by: theirs },
        ),
        (entry(&other, &auto), Verdict::TakesAuto { from: theirs }),
        (implied, Verdict::HeldByOlderBuild { by: theirs }),
    ] {
        write_private(root.path(), PEERS_FILE, book.as_bytes()).expect("their claim on m7");
        let (why, asked) = undo_shown(&root, &seen, no_labels, ByKind::Person, NOW + 2, false);
        let why = why.expect_err("m7 is held");
        assert_eq!(
            Refused::of(&why).and_then(Refused::verdict),
            Some(&verdict),
            "{why}"
        );
        assert!(asked.is_empty(), "nothing queued: {asked:?}");
        let preview = rename_preview(root.path(), no_labels, unread, NOW + 2).expect("a read");
        assert_eq!(preview.undo(&seen).as_ref().err(), Refused::of(&why));
        assert_eq!(file::read(root.path()).expect("reads"), Some(set.clone()));
    }
    write_private(root.path(), PEERS_FILE, b"").expect("no holder");
    let (back, _) = undo_shown(&root, &seen, no_labels, ByKind::Person, NOW + 2, false);
    assert_eq!(
        back.expect("the control: m7 is free").name.num,
        Some(num(7))
    );
}

/// Undo is refused once the vault gives the old number to another Mac, by any hold, with that
/// hold, the preview answering the same, and nothing changes; with no labels (the control) the
/// same undo goes through.
#[test]
fn undo_is_refused_once_the_vault_gives_the_old_number_to_another_mac() {
    let root = state();
    let (before, set) = renamed_from_m7(&root);
    let seen = rename_undo(&before);
    for hold in [
        LabelHold::Label,
        LabelHold::Alias,
        LabelHold::Claim,
        LabelHold::VaultName,
        LabelHold::Seat,
        LabelHold::Denied,
    ] {
        let lab = LabLabels {
            held: BTreeMap::from([(num(7), hold)]),
            ..LabLabels::default()
        };
        let (why, _) = undo_shown(
            &root,
            &seen,
            || Ok(lab.clone()),
            ByKind::Person,
            NOW + 2,
            false,
        );
        let why = why.expect_err("another Mac's number");
        assert_eq!(
            Refused::of(&why).map(|refused| &refused.cause),
            Some(&Cause::Vault(hold)),
            "{why}"
        );
        let preview =
            rename_preview(root.path(), || Ok(lab.clone()), unread, NOW + 2).expect("a read");
        assert_eq!(preview.undo(&seen).as_ref().err(), Refused::of(&why));
        assert_eq!(file::read(root.path()).expect("reads"), Some(set.clone()));
    }
    let (back, _) = undo_shown(&root, &seen, no_labels, ByKind::Person, NOW + 2, false);
    assert_eq!(back.expect("the control: no labels").name.num, Some(num(7)));
}

/// Undo is refused while another Mac's record carries the nickname it would bring back, whether
/// it takes the number back with it or the nickname alone, with the preview answering the same,
/// and nothing changes. Control: once that record carries another word, each undo goes through.
#[test]
fn undo_is_refused_when_another_mac_now_carries_the_old_nickname() {
    let root = state();
    node_identity(root.path()).expect("our identity");
    let (before, set) = renamed_from_m7(&root);
    let other = NodeIdentity::from_seed(&"b2".repeat(16), &[0x22; 32]).expect("another identity");
    // Each record is newer than the last, so the book takes it over the one before.
    let carries = |word: &str, rev: u64| {
        write_private(
            root.path(),
            PEERS_FILE,
            entry(&other, &record(&other, Some(11), rev, Some(word))).as_bytes(),
        )
        .expect("their record");
    };
    let refused = |seen: &history::Undo, labels: fn() -> io::Result<LabLabels>, now: u64| {
        let (why, _) = undo_shown(&root, seen, labels, ByKind::Person, now, false);
        let why = why.expect_err("their nickname");
        assert_eq!(
            Refused::of(&why).map(|refused| &refused.cause),
            Some(&Cause::Nickname),
            "{why}"
        );
        let preview = rename_preview(root.path(), labels, unread, now).expect("a read");
        assert_eq!(preview.undo(seen).as_ref().err(), Refused::of(&why));
    };
    let old = before.display_nick();
    let seen = rename_undo(&before);
    carries(&old, 3);
    refused(&seen, no_labels, NOW + 2);
    assert_eq!(file::read(root.path()).expect("reads"), Some(set));
    carries("Kestrel", 4);
    let (back, _) = undo_shown(&root, &seen, no_labels, ByKind::Person, NOW + 2, false);
    let back = back
        .expect("the control: the number and nickname come back")
        .name;
    assert_eq!(back.title(), before.title());

    let nicked = set_nick(
        root.path(),
        &NickTo::Word(Nickname::parse("Heron").expect("a nickname")),
        ByKind::Person,
        unread,
        NOW + 3,
        false,
    )
    .expect("a nickname change")
    .name;
    let seen = nickname_undo(&old, "Heron");
    carries(&old, 5);
    refused(&seen, unreadable, NOW + 4);
    assert_eq!(file::read(root.path()).expect("reads"), Some(nicked));
    carries("Kestrel", 6);
    let (back, _) = undo_shown(&root, &seen, unreadable, ByKind::Person, NOW + 4, false);
    let back = back
        .expect("the control: the nickname alone comes back")
        .name;
    assert_eq!(back.title(), before.title());
}

/// No undo leaves a Mac unnumbered: the row's rename that gave it its first number is refused,
/// over a label the row never held or one it held unset, while (the control) a nickname rename
/// while unnumbered is taken back.
#[test]
fn undo_leaves_no_mac_unnumbered() {
    let root = state();
    let (first, _) = ensure(root.path(), no_roster, never, NOW).expect("unnumbered");
    let next = different_nickname(&first);
    set_nick(
        root.path(),
        &NickTo::Word(next.clone()),
        ByKind::Person,
        unread,
        NOW + 1,
        false,
    )
    .expect("a nickname");
    let (back, _) = undo_shown(
        &root,
        &nickname_undo(&first.display_nick(), next.as_str()),
        unreadable,
        ByKind::Person,
        NOW + 2,
        false,
    );
    let back = back
        .expect("the control: an unnumbered nickname change")
        .name;
    assert_eq!(
        (back.num, back.display_nick()),
        (None, first.display_nick())
    );
    let numbered = |replaced: Vec<Option<String>>| {
        let mut write = owner_write(4, &[(Renamed::Label, None, "m86")]);
        write.cells[0].replaced = replaced;
        undo_over(vec![write])
    };
    for (replaced, case) in [(Vec::new(), "never held"), (vec![None], "held unset")] {
        let why = numbered(replaced).expect_err(case);
        assert_eq!(why.cause, Cause::Unnumbered, "{case}: {why}");
    }
}

/// The numbers offered beside a taken one are those a rename takes as `Free`: past each number a
/// signed node holds, each the vault keeps for another Mac, this Mac's own and another Mac's
/// earlier number; none where the vault's labels are unread. Control: with no other node and no
/// labels, the next numbers in order.
#[test]
fn free_after_skips_every_held_number() {
    use aterm_types::machine_name::Was;
    let root = state();
    node_identity(root.path()).expect("our identity");
    ensure(root.path(), label("m7"), never, NOW).expect("a seeded name");
    let fresh = rename_preview(root.path(), no_labels, unread, NOW).expect("a read");
    assert_eq!(
        fresh.free_after(num(86), 3),
        [num(87), num(88), num(89)],
        "the control: the next in order"
    );
    set_name(
        root.path(),
        &NameTo {
            num: num(88),
            nick: None,
        },
        no_labels,
        ByKind::Person,
        unread,
        NOW,
        false,
    )
    .expect("this Mac takes m88");
    let holder = NodeIdentity::from_seed(&"b2".repeat(16), &[0x22; 32]).expect("an identity");
    let earlier = NodeIdentity::from_seed(&"c3".repeat(16), &[0x33; 32]).expect("an identity");
    let mut moved = record(&earlier, Some(95), 4, Some("Puffin"));
    moved.claim.was = vec![Was {
        num: num(89),
        retired: false,
        lost: false,
    }];
    write_private(
        root.path(),
        PEERS_FILE,
        (entry(&holder, &record(&holder, Some(87), 3, Some("Otter"))) + &entry(&earlier, &moved))
            .as_bytes(),
    )
    .expect("two other Macs");
    let lab = LabLabels {
        held: BTreeMap::from([(num(90), LabelHold::Label)]),
        ..LabLabels::default()
    };
    let preview = rename_preview(root.path(), || Ok(lab.clone()), unread, NOW).expect("a read");
    assert_eq!(preview.free_after(num(86), 3), [num(91), num(92), num(93)]);
    assert_eq!(preview.free_after(num(86), 0), []);
    assert_eq!(
        preview.free_after(num(Number::MAX - 1), 3),
        [num(Number::MAX)],
        "none past the last number"
    );
    let unlabelled = rename_preview(root.path(), unreadable, unread, NOW).expect("a read");
    assert_eq!(unlabelled.free_after(num(86), 3), []);
}

/// Retiring an earlier number stops it raising `top` past a person's cap, as an active one does
/// whatever the cap; restoring it (the control) raises `top` again. Neither touches the NAME, the
/// nickname or History, and a number this Mac never went by is not found.
#[test]
fn a_retired_earlier_number_no_longer_raises_top() {
    use aterm_types::machine_name::{Cap, top};
    let root = state();
    ensure(root.path(), label("m90"), never, NOW).expect("a seeded name");
    set_name(
        root.path(),
        &NameTo {
            num: num(7),
            nick: None,
        },
        no_labels,
        ByKind::Person,
        unread,
        NOW,
        false,
    )
    .expect("m90 becomes an earlier number");
    let active = file::replace(root.path(), |name| {
        name.cap = Some(Cap {
            value: 10,
            since: NOW,
        });
        Ok(())
    })
    .expect("a person's cap");
    let top_of = |name: &MachineName| {
        let node = NodeId::new(&[1; 16]).expect("a node id");
        top(
            &BTreeMap::from([(node, Entry::Record(current_record(name)))]),
            &[],
            [],
            0,
        )
    };
    assert_eq!(top_of(&active), 90, "the control: an active earlier number");
    let retired = retire_was(root.path(), num(90), true).expect("retires");
    assert!(retired.changed);
    assert!(retired.name.was[0].retired);
    assert_eq!(top_of(&retired.name), 10, "only up to the cap");
    assert_eq!(retired.name.rev, active.rev + 1);
    assert_eq!(retired.name.title(), active.title());
    assert_eq!(
        file::read(root.path()).expect("reads"),
        Some(retired.name.clone())
    );
    let again = retire_was(root.path(), num(90), true).expect("retired already");
    assert!(!again.changed);
    assert_eq!(again.name, retired.name);
    let restored = retire_was(root.path(), num(90), false).expect("restores");
    assert_eq!(top_of(&restored.name), 90);
    let missing = retire_was(root.path(), num(5), true).expect_err("never m5");
    assert_eq!(missing.kind(), io::ErrorKind::NotFound);
    assert_eq!(file::read(root.path()).expect("reads"), Some(restored.name));
}

/// A preview names this Mac's node only once it has one: a keyless Mac is judged as an absent
/// node, whose stand-in is no identity to show.
#[test]
fn a_preview_names_this_macs_node_only_once_it_has_one() {
    let root = state();
    ensure(root.path(), label("m7"), never, NOW).unwrap();
    let keyless = rename_preview(root.path(), no_labels, unread, NOW).unwrap();
    assert_eq!(keyless.node(), None, "no node key yet");
    let me = node_identity(root.path()).unwrap();
    let keyed = rename_preview(root.path(), no_labels, unread, NOW).unwrap();
    assert_eq!(
        keyed.node(),
        node_of(me.node()),
        "the identity the read found"
    );
    assert!(keyed.node().is_some(), "this Mac's node");
}

/// A drawn noun no test nickname is: a nickname change to `Heron` then always changes it.
fn not_heron(word: &str) -> bool {
    word.eq_ignore_ascii_case("heron")
}

/// What an undo of the name in `root` does as a dry run over `seen`, and its undo target: the
/// title [`UndoTo::title`] names is the title the dry run gives.
fn undo_titled(
    root: &aterm_tempfile::TempDir,
    seen: &history::Undo,
) -> (MachineName, UndoTo, MachineName) {
    let held = file::read(root.path()).expect("reads").expect("a name");
    let (dry, _) = undo_shown(root, seen, no_labels, ByKind::Person, NOW + 2, true);
    let dry = dry.expect("a dry run").name;
    assert_eq!(seen.to.title(&held), dry.title(), "{:?}", seen.to);
    (held, seen.to.clone(), dry)
}

/// The title an undo names ([`UndoTo::title`]) is the title its dry run gives, for a number and
/// nickname taken back, a number alone under a drawn nickname, and a nickname alone. Control:
/// the drawn nickname's stored spelling (`heron` where the title shows `Heron`) gives another
/// title, which is the mutant `UndoTo::title` must not be.
#[test]
fn an_undo_names_the_title_its_dry_run_gives() {
    let both = state();
    let (before, _) = renamed_from_m7(&both);
    let (_, to, _) = undo_titled(&both, &rename_undo(&before));
    assert!(
        matches!(to, UndoTo::Name(NameTo { nick: Some(_), .. })),
        "{to:?}"
    );

    let number = state();
    let (seeded, _) = ensure(number.path(), label("m7"), never, NOW).expect("a seeded name");
    assert_eq!(seeded.nick_class, NickClass::Auto, "a drawn nickname");
    set_name(
        number.path(),
        &NameTo {
            num: num(86),
            nick: None,
        },
        no_labels,
        ByKind::Person,
        unread,
        NOW + 1,
        false,
    )
    .expect("renames, keeping the drawn nickname");
    let seen = undo_over(vec![owner_write(2, &[(Renamed::Label, Some("m7"), "m86")])])
        .expect("a number undo");
    let (held, to, dry) = undo_titled(&number, &seen);
    assert_eq!(
        to,
        UndoTo::Name(NameTo {
            num: num(7),
            nick: None
        })
    );
    assert_ne!(
        file::title_of(Some(num(7)), held.nick.as_str()),
        dry.title(),
        "the control: the stored spelling is not the title the undo leaves"
    );

    let nick = state();
    let (seeded, _) = ensure(nick.path(), label("m7"), not_heron, NOW).expect("a seeded name");
    set_nick(
        nick.path(),
        &NickTo::Word(Nickname::parse("Heron").expect("a nickname")),
        ByKind::Person,
        unread,
        NOW + 1,
        false,
    )
    .expect("a nickname change");
    let (_, to, _) = undo_titled(&nick, &nickname_undo(&seeded.display_nick(), "Heron"));
    assert!(matches!(to, UndoTo::Nickname(_)), "{to:?}");
}

/// An undo takes back only the rename its caller read: where History, read again under the
/// naming locks, gives another undo (a later rename of the number or of the nickname, from any
/// Mac; a later automatic write of the cell the rename did not write; no row this Mac's), the
/// undo is refused as unseen, writing nothing and queueing nothing. Controls: over History as it
/// was read, the undo takes the rename back, and a change to the name that is no rename (retiring
/// an earlier number, which raises `rev`) between that read and the undo still lets it through,
/// which a fence on `rev` would refuse.
/// The fence reads the row's whole name: a nickname undo is stopped by an automatic write of the
/// number it did not write.
/// NEGATIVE CONTROL: with `edit_name`'s `history::fence` removed, the undo over the later
/// number writes `m7`; with `fence` comparing the newest rename alone (not `Undo::shown`), the
/// nickname undo goes through over the moved number.
#[test]
fn an_undo_takes_back_only_the_change_it_read() {
    let root = state();
    let (before, set) = renamed_from_m7(&root);
    let seen = rename_undo(&before);
    let read = row_of(&seen);
    let later = |write: history::NameWrite| {
        let mut later = read.clone();
        for older in &mut later.writes {
            for cell in &mut older.cells {
                cell.stands &= write.cell(cell.cell).is_none();
            }
        }
        later.writes.insert(0, write);
        later
    };
    let number = owner_write(3, &[(Renamed::Label, Some("m86"), "m90")]);
    let nickname = owner_write(3, &[(Renamed::Nickname, Some("MissBunny"), "Kestrel")]);
    let automatic = history::NameWrite {
        kind: aterm_db::att::EntryKind::Aterm,
        ..owner_write(3, &[(Renamed::Label, Some("m86"), "m90")])
    };
    for (now, case) in [
        (later(number), "a later number"),
        (later(nickname), "a later nickname"),
        (later(automatic), "a later automatic number"),
        (history::History::default(), "no row this Mac's"),
    ] {
        let (why, asked) = registry::with_door(registry::Written::Unbound, || {
            history::with_history(Ok(now), || {
                undo_last(
                    root.path(),
                    &seen,
                    no_labels,
                    ByKind::Person,
                    unread,
                    NOW + 3,
                    false,
                )
            })
        });
        let why = why.expect_err(case);
        assert_eq!(
            Refused::of(&why).map(|refused| &refused.cause),
            Some(&Cause::Unseen),
            "{case}: {why}"
        );
        assert!(asked.is_empty(), "{case}: nothing queued: {asked:?}");
        assert_eq!(
            file::read(root.path()).expect("reads"),
            Some(set.clone()),
            "{case}: nothing undone"
        );
    }
    let nick_only = nickname_undo("MissBunny", "Bunny");
    let mut renumbered = row_of(&nick_only);
    for write in &mut renumbered.writes {
        for cell in &mut write.cells {
            cell.stands &= cell.cell != Renamed::Label;
        }
    }
    renumbered.writes.insert(
        0,
        history::NameWrite {
            kind: aterm_db::att::EntryKind::Aterm,
            ..owner_write(4, &[(Renamed::Label, Some("m86"), "m9")])
        },
    );
    let (why, _) = registry::with_door(registry::Written::Unbound, || {
        history::with_history(Ok(renumbered), || {
            undo_last(
                root.path(),
                &nick_only,
                unreadable,
                ByKind::Person,
                unread,
                NOW + 3,
                false,
            )
        })
    });
    let why = why.expect_err("the number the nickname undo did not write moved");
    assert_eq!(
        Refused::of(&why).map(|refused| &refused.cause),
        Some(&Cause::Unseen),
        "{why}"
    );

    let (dry, _) = undo_shown(&root, &seen, no_labels, ByKind::Person, NOW + 3, true);
    assert_eq!(
        dry.expect("the control: History as it was read").name.num,
        Some(num(7))
    );
    let retired = retire_was(root.path(), num(7), true)
        .expect("retires m7")
        .name;
    assert_eq!(retired.rev, set.rev + 1, "rev rises");
    let (back, _) = undo_shown(&root, &seen, no_labels, ByKind::Person, NOW + 3, false);
    assert_eq!(
        back.expect("the control: History still gives the rename read")
            .name
            .num,
        Some(num(7))
    );
}

/// Over unread vault labels, Undo Last Rename's offer leaves the number to the undo, as the live
/// line leaves Save, answering why the labels were not read where [`RenamePreview::undo`] refuses
/// it as unread; the undo itself reads the labels again and refuses it while they stay unread.
/// Control: the nickname is judged either way, so another node's record carrying the old
/// nickname refuses the offer.
#[test]
fn undo_unchecked_leaves_an_unread_number_to_the_undo() {
    let root = state();
    node_identity(root.path()).expect("our identity");
    let (before, set) = renamed_from_m7(&root);
    let seen = rename_undo(&before);
    let preview = rename_preview(root.path(), unreadable, unread, NOW + 2).expect("a read");
    let why = preview.undo_unchecked(&seen).expect("left to the undo");
    assert!(why.is_some(), "why the labels were not read");
    assert_eq!(why, preview.unchecked(num(7)));
    assert_eq!(
        preview.undo(&seen).map_err(|refused| refused.cause),
        Err(Cause::Unread)
    );
    let (refused, _) = undo_shown(&root, &seen, unreadable, ByKind::Person, NOW + 2, false);
    let refused = refused.expect_err("the undo checks the number");
    assert_eq!(
        Refused::of(&refused).map(|refused| &refused.cause),
        Some(&Cause::Unread),
        "{refused}"
    );
    assert_eq!(file::read(root.path()).expect("reads"), Some(set));

    let other = NodeIdentity::from_seed(&"b2".repeat(16), &[0x22; 32]).expect("another identity");
    write_private(
        root.path(),
        PEERS_FILE,
        entry(
            &other,
            &record(&other, Some(11), 3, Some(&before.display_nick())),
        )
        .as_bytes(),
    )
    .expect("their record carries the old nickname");
    let preview = rename_preview(root.path(), unreadable, unread, NOW + 2).expect("a read");
    assert_eq!(
        preview
            .undo_unchecked(&seen)
            .map_err(|refused| refused.cause),
        Err(Cause::Nickname),
        "the control: the nickname is judged over unread labels too"
    );
}

/// Another Mac's claim or hold on a number refuses that number alone, so the next free numbers
/// are offered beside it ([`RenamePreview::next_free`], the first [`FREE_SHOWN`] of
/// [`RenamePreview::free_after`]): the vault's label, a person's, an automatic and an older
/// build's claim. Controls: the policy's server name, unread labels and an undo's `Nothing`
/// refuse any number, and offer none, even where free numbers are there to offer.
#[test]
fn held_by_another_is_another_macs_claim_or_hold() {
    let labelled = state();
    ensure(labelled.path(), label("m7"), never, NOW).expect("a seeded name");
    let lab = LabLabels {
        held: BTreeMap::from([(num(20), LabelHold::Label)]),
        ..LabLabels::default()
    };
    let preview = rename_preview(labelled.path(), || Ok(lab.clone()), unread, NOW).expect("a read");
    let vault = preview.judge(num(20)).expect_err("the vault's label");
    assert_eq!(vault.cause, Cause::Vault(LabelHold::Label));
    assert!(vault.held_by_another());
    let free = preview.next_free(num(20), &vault);
    assert_eq!(free, preview.free_after(num(20), FREE_SHOWN));
    assert_eq!(free.len(), FREE_SHOWN);

    let root = state();
    node_identity(root.path()).expect("our identity");
    renamed_from_m7(&root);
    let other = NodeIdentity::from_seed(&"b2".repeat(16), &[0x22; 32]).expect("another identity");
    let theirs = node_of(other.node()).expect("their node");
    let mut auto = record(&other, Some(7), 3, Some("Otter"));
    auto.claim.class = Class::Auto;
    let implied = Peer::own(&other, ("m7.local", 7485, "0.160.0"), NOW)
        .with_primary_name(&other, Some("m7"))
        .line()
        + "\n";
    for (book, verdict) in [
        (
            entry(&other, &record(&other, Some(7), 3, Some("Otter"))),
            Verdict::HeldByPerson { by: theirs },
        ),
        (entry(&other, &auto), Verdict::TakesAuto { from: theirs }),
        (implied, Verdict::HeldByOlderBuild { by: theirs }),
    ] {
        write_private(root.path(), PEERS_FILE, book.as_bytes()).expect("their claim on m7");
        let preview = rename_preview(root.path(), no_labels, unread, NOW + 2).expect("a read");
        let held = preview.judge(num(7)).expect_err("m7 is held");
        assert_eq!(held.verdict(), Some(&verdict));
        assert!(held.held_by_another(), "{verdict:?}");
        let free = preview.next_free(num(7), &held);
        assert_eq!(free, preview.free_after(num(7), FREE_SHOWN), "{verdict:?}");
        assert_eq!(free.len(), FREE_SHOWN, "{verdict:?}");
    }

    let served = state();
    ensure(served.path(), label("m8"), never, NOW).expect("a seeded name");
    let serves = LabLabels {
        own: BTreeSet::from([num(8)]),
        label: Some(num(8)),
        servers: BTreeSet::from([num(8)]),
        ..LabLabels::default()
    };
    let preview =
        rename_preview(served.path(), || Ok(serves.clone()), unread, NOW).expect("a read");
    let server = preview.judge(num(87)).expect_err("the server's name");
    assert_eq!(server.cause, Cause::Server(num(8)));
    assert!(!server.held_by_another(), "the control: a server name");
    assert_eq!(preview.next_free(num(87), &server), []);

    let preview = rename_preview(served.path(), unreadable, unread, NOW).expect("a read");
    let unchecked = preview.judge(num(87)).expect_err("unread labels");
    assert_eq!(unchecked.cause, Cause::Unread);
    assert!(!unchecked.held_by_another(), "the control: unread labels");
    assert_eq!(preview.next_free(num(87), &unchecked), []);

    let preview = rename_preview(labelled.path(), no_labels, unread, NOW).expect("a read");
    let nothing = undo_over(Vec::new()).expect_err("a row with no rename");
    assert_eq!(nothing.cause, Cause::Nothing);
    assert!(
        !nothing.held_by_another(),
        "the control: no number's refusal"
    );
    assert_eq!(
        preview.free_after(num(20), FREE_SHOWN).len(),
        FREE_SHOWN,
        "free numbers are there"
    );
    assert_eq!(
        preview.next_free(num(20), &nothing),
        [],
        "and none is offered"
    );
}

/// An undo publishes under the number it takes back, which it claims as chosen, and a nickname
/// undo under the chosen number it keeps ([`undo_publishes`]). Controls: the stored chosen number
/// alone ([`chosen_number`]) is the wrong one for a number undo; a nickname undo on an automatic
/// or seeded number, or one a migration waits on, publishes nothing; nor does a name with nothing
/// to take back.
#[test]
fn an_undo_publishes_the_number_it_takes_back_or_the_chosen_one_it_keeps() {
    let root = state();
    let (before, set) = renamed_from_m7(&root);
    assert_eq!(undo_publishes(&set, &rename_undo(&before)), Some(num(7)));
    assert_eq!(
        chosen_number(&set),
        Some(num(86)),
        "the control: the stored number is not the one the undo claims"
    );
    let nicked = set_nick(
        root.path(),
        &NickTo::Word(Nickname::parse("Heron").expect("a nickname")),
        ByKind::Person,
        unread,
        NOW + 2,
        false,
    )
    .expect("a nickname change")
    .name;
    let nick_back = nickname_undo("MissBunny", "Heron");
    assert!(matches!(nick_back.to, UndoTo::Nickname(_)));
    assert_eq!(undo_publishes(&nicked, &nick_back), Some(num(86)));
    let mut waiting = nicked;
    waiting.migration = Some(Migration::Waiting);
    assert_eq!(
        undo_publishes(&waiting, &nick_back),
        None,
        "a waiting migration"
    );

    let fresh = state();
    let (first, _) = ensure(fresh.path(), label("m7"), not_heron, NOW).expect("a seeded name");
    let seeded = set_nick(
        fresh.path(),
        &NickTo::Word(Nickname::parse("Heron").expect("a nickname")),
        ByKind::Person,
        unread,
        NOW + 1,
        false,
    )
    .expect("a nickname change")
    .name;
    assert_eq!(seeded.class, Class::Seeded);
    let nick_back = nickname_undo(&first.display_nick(), "Heron");
    assert_eq!(undo_publishes(&seeded, &nick_back), None, "a seeded number");
    let mut auto = seeded;
    auto.class = Class::Auto;
    assert_eq!(
        undo_publishes(&auto, &nick_back),
        None,
        "an automatic number"
    );
}
