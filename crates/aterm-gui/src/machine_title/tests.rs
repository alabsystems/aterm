// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::cell::{Cell, RefCell};
use std::io;
use std::os::unix::fs::PermissionsExt as _;
use std::sync::mpsc;
use std::time::Duration;

use aterm_agent::machine_name::file;
use aterm_types::machine_name::{Nickname, Number};

use super::*;

fn view(name: &str, nick: &str, fleet: bool) -> MachineView {
    MachineView {
        rev: 0,
        name: name.to_owned(),
        nick: nick.to_owned(),
        word: nick.to_owned(),
        fleet: if fleet { InFleet::In } else { InFleet::Out },
    }
}

/// The vault of a scratch home under `dir`: none, so not in a fleet, until a test writes one.
fn vault(dir: &std::path::Path) -> Paths {
    Paths::locate_for_home(&dir.join("home"))
}

fn state() -> aterm_tempfile::TempDir {
    let root = aterm_tempfile::tempdir().expect("a tempdir");
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
        .expect("a private dir");
    root
}

#[test]
fn the_title_is_the_name_and_the_nickname_clipped_to_sixteen_scalars() {
    assert_eq!(
        machine_menu_bar_title(&view("m8", "Heron", true)),
        "m8 Heron"
    );
    assert_eq!(
        machine_menu_bar_title(&view("m0", "Otter", false)),
        "m0 Otter"
    );
    let sixteen = "Abcdefghijklmnop";
    assert_eq!(
        machine_menu_bar_title(&view("m8", sixteen, true)),
        format!("m8 {sixteen}"),
        "sixteen stand whole"
    );
    assert_eq!(
        machine_menu_bar_title(&view("m8", "Abcdefghijklmnopq", true)),
        "m8 Abcdefghijklmno\u{2026}",
        "fifteen and an ellipsis"
    );
    assert_eq!(
        machine_menu_bar_title(&view("m8", "Été-très-long-surnom", true))
            .chars()
            .count(),
        3 + NICK_SHOWN,
        "counted in scalars, not bytes"
    );
    assert_eq!(machine_menu_bar_title(&view("m8", "", true)), "m8");
}

#[test]
fn the_first_row_says_whether_this_machine_is_in_a_fleet() {
    assert_eq!(
        machine_menu_info_row(&view("m8", "Heron", true)),
        "m8 \u{00B7} Heron \u{00B7} in a fleet"
    );
    assert_eq!(
        machine_menu_info_row(&view("m0", "Otter", false)),
        "m0 \u{00B7} Otter \u{00B7} not in a fleet"
    );
    let joining = MachineView {
        fleet: InFleet::Joining,
        ..view("m0", "Otter", false)
    };
    assert_eq!(
        machine_menu_info_row(&joining),
        "m0 \u{00B7} Otter \u{00B7} joining a fleet"
    );
    let unread = MachineView {
        fleet: InFleet::Unread("source.toml: bad".to_owned()),
        ..view("m0", "Otter", false)
    };
    assert_eq!(
        machine_menu_info_row(&unread),
        "m0 \u{00B7} Otter \u{00B7} fleet not read"
    );
}

/// A nickname no draw can make (the nouns are one word each), so a rename to it always changes
/// the view.
const RENAMED: &str = "Blue Heron";

fn rename(dir: &std::path::Path) {
    file::replace(dir, |name| {
        name.nick = Nickname::parse(RENAMED).map_err(|e| io::Error::other(e.to_string()))?;
        Ok(())
    })
    .expect("renames");
}

fn join(dir: &std::path::Path) {
    let key = format!("{}\n", "ab".repeat(32));
    aterm_uds::private_file::write_private(dir, aterm_net::fleet::KEY_FILE, key.as_bytes())
        .expect("joins");
}

#[test]
fn the_reader_posts_the_name_and_each_change_it_hears() {
    let root = state();
    let dir = root.path();
    let posted = RefCell::new(Vec::new());
    let waits = RefCell::new(Vec::new());
    let post = |view: MachineView| {
        posted.borrow_mut().push(view);
        true
    };
    let wait = |timeout: Option<Duration>| {
        waits.borrow_mut().push(timeout);
        match waits.borrow().len() {
            // The watch's first look tells with nothing changed: the read posts nothing.
            1 => Heard::Change,
            2 => {
                rename(dir);
                Heard::Change
            }
            3 => {
                join(dir);
                Heard::Change
            }
            _ => Heard::Ended,
        }
    };
    read_loop(dir, Some(&vault(dir)), || Ok(None), post, wait);
    let posted = posted.into_inner();
    let first = posted.first().expect("a first view").clone();
    assert_eq!((first.name.as_str(), &first.fleet), ("m0", &InFleet::Out));
    assert_eq!(
        posted,
        [first, view("m0", RENAMED, false), view("m0", RENAMED, true)],
        "one post per change, none for a read that found none"
    );
    assert_eq!(
        waits.into_inner(),
        [None; 4],
        "a read that succeeded waits for a change alone, and the reader ends with its watch"
    );
}

#[test]
fn a_read_that_fails_makes_nothing_and_is_tried_again_after_a_minute() {
    let root = state();
    let asked = Cell::new(0);
    let roster = || {
        asked.set(asked.get() + 1);
        if asked.get() == 1 {
            Err(io::Error::other("the roster is busy"))
        } else {
            Ok(Some("m8".to_owned()))
        }
    };
    let waits = RefCell::new(Vec::new());
    let mut posted = Vec::new();
    read_loop(
        root.path(),
        Some(&vault(root.path())),
        roster,
        |view| {
            posted.push(view);
            false
        },
        |timeout| {
            assert!(
                file::read(root.path()).expect("reads").is_none(),
                "nothing made of a failed read"
            );
            waits.borrow_mut().push(timeout);
            Heard::Quiet
        },
    );
    assert_eq!(waits.into_inner(), [Some(RETRY_AFTER)]);
    assert_eq!(posted.len(), 1);
    assert_eq!(posted[0].name, "m8", "the next read's roster");
}

/// The watch hears `machine-name.toml` replaced by rename, a vault first made after the watch
/// started (its folder missing, so the watch listens on the nearest folder above it), each of
/// the vault's standing files written, and a fleet key: each change is a new view.
#[test]
fn the_watch_hears_a_rename_by_replace_a_vault_made_later_and_a_fleet_key_written() {
    let root = state();
    let dir = root.path().to_path_buf();
    let secrets = vault(&dir);
    let (watch, changes) = watch(&dir, Some(&secrets)).expect("a watch");
    let (tell, views) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        read_loop(
            &dir,
            Some(&secrets),
            || Ok(None),
            |view| tell.send(view).is_ok(),
            |timeout| heard(&changes, timeout),
        );
    });
    // Each wait is a one-minute hang detector for a change that must be heard.
    let hang = Duration::from_secs(60);
    let first = views.recv_timeout(hang).expect("a first view");
    rename(root.path());
    assert_eq!(
        views.recv_timeout(hang).expect("the rename heard"),
        view(&first.name, RENAMED, false)
    );
    // Files the vault cannot read, each written before the one its status reads ahead of it, so
    // each read fails at the new file: each standing file is heard, as a new reason the standing
    // is unread, and the row says so.
    let vault_dir = vault(root.path()).dir;
    std::fs::create_dir_all(&vault_dir).expect("a vault folder");
    let mut was = String::new();
    for file in ["state.toml", "identity", "source.toml"] {
        let at = vault_dir.join(file);
        // Published whole at its private mode by one rename, so the reader can find it in one
        // state only. Written at the umask's mode and made private after, it has two (refused as
        // readable by other users, then unparsable), and a read of each posts a view of its own.
        aterm_uds::atomic_file::write(&at, b"not = [a vault file", 0o600).expect("a vault file");
        let unread = views.recv_timeout(hang).expect("the vault's file heard");
        let InFleet::Unread(why) = &unread.fleet else {
            panic!("{file}: {:?}", unread.fleet);
        };
        assert_ne!(why, &was, "{file}");
        was.clone_from(why);
    }
    join(root.path());
    assert_eq!(
        views.recv_timeout(hang).expect("the fleet key heard"),
        view(&first.name, RENAMED, true)
    );
    drop(watch);
    reader.join().expect("the reader ends with its watch");
}

#[test]
fn no_watch_reads_once_and_retries_only_a_failed_read() {
    assert_eq!(unwatched(None), Heard::Ended);
    assert_eq!(unwatched(Some(Duration::ZERO)), Heard::Quiet);
}

#[test]
fn a_title_read_before_an_update_commits_is_kept() {
    let mut app = crate::App::headless_for_test();
    app.incoming_handoff_pending = true;
    assert!(
        !app.drops_wake_before_commit(&Wake::MachineName(view("m8", "Heron", true))),
        "the reader posts a view once, so a candidate that dropped it would show no title"
    );
}

#[test]
fn the_nickname_alert_reads_change_cancel_and_the_random_noun_from_its_response() {
    use crate::appkit::consts::{
        NS_ALERT_FIRST_BUTTON_RETURN, NS_ALERT_SECOND_BUTTON_RETURN, NS_ALERT_THIRD_BUTTON_RETURN,
    };
    assert_eq!(NICK_BUTTONS, ["Change", "Cancel", "New Random Noun"]);
    assert_eq!(
        nick_answer(NS_ALERT_FIRST_BUTTON_RETURN, || "Kestrel".to_owned()),
        NickAnswer::Typed("Kestrel".to_owned())
    );
    let unread = || -> String { panic!("only Change reads the field") };
    assert_eq!(
        nick_answer(NS_ALERT_SECOND_BUTTON_RETURN, unread),
        NickAnswer::Cancel
    );
    assert_eq!(
        nick_answer(NS_ALERT_THIRD_BUTTON_RETURN, unread),
        NickAnswer::Random
    );
    // `NSModalResponseAbort`: a modal session ended by something other than a button.
    assert_eq!(nick_answer(-1001, unread), NickAnswer::Cancel);
}

#[test]
fn a_typed_nickname_is_trimmed_and_a_word_that_is_none_is_refused_by_name() {
    let typed = |text: &str| NickAnswer::Typed(text.to_owned());
    assert!(matches!(
        nick_to(&typed("  Kestrel \n"), "heron"),
        Ok(Some(NickTo::Word(nick))) if nick.as_str() == "Kestrel"
    ));
    assert!(matches!(
        nick_to(&NickAnswer::Random, "heron"),
        Ok(Some(NickTo::Random))
    ));
    assert!(matches!(nick_to(&NickAnswer::Cancel, "heron"), Ok(None)));
    assert_eq!(
        nick_to(&typed("   "), "heron").err().as_deref(),
        Some("Type a nickname first.")
    );
    let refused = nick_to(&typed("m12"), "heron").expect_err("a number shape is no nickname");
    assert!(
        refused.starts_with("\u{201c}m12\u{201d} is no nickname: ") && refused.ends_with('.'),
        "{refused}"
    );
}

#[test]
fn an_untouched_field_changes_nothing_but_another_case_is_a_change() {
    let typed = |text: &str| NickAnswer::Typed(text.to_owned());
    assert!(
        matches!(nick_to(&typed("heron"), "heron"), Ok(None)),
        "the held word typed back is no change, so a drawn noun stays drawn"
    );
    assert!(matches!(nick_to(&typed(" heron "), "heron"), Ok(None)));
    assert!(matches!(
        nick_to(&typed("Heron"), "heron"),
        Ok(Some(NickTo::Word(nick))) if nick.as_str() == "Heron"
    ));
}

#[test]
fn the_view_starts_the_field_at_the_stored_word_not_the_title_s_form() {
    let root = state();
    let dir = root.path();
    read(dir, Some(&vault(dir)), || Ok(None)).expect("a first read makes the name");
    file::replace(dir, |name| {
        name.nick = Nickname::parse("kestrel").map_err(|e| io::Error::other(e.to_string()))?;
        Ok(())
    })
    .expect("renames");
    let shown = read(dir, Some(&vault(dir)), || Ok(None)).expect("a read");
    assert_eq!(
        (shown.nick.as_str(), shown.word.as_str()),
        ("Kestrel", "kestrel")
    );
}

#[test]
fn the_alert_names_the_number_and_puts_the_last_refusal_first() {
    let body = nickname_alert_body(&view("m8", "Heron", true), None);
    assert!(body.contains("beside its number, m8."), "{body}");
    assert!(body.contains("random noun"), "{body}");
    let again = nickname_alert_body(&view("m8", "Heron", true), Some("Type a nickname first."));
    assert_eq!(again, format!("Type a nickname first.\n\n{body}"));
}

#[test]
fn a_nickname_change_is_refused_to_a_gesture_the_person_did_not_make_and_before_a_read() {
    let mut app = crate::App::headless_for_test();
    app.machine_view = Some(view("m8", "Heron", true));
    for mark in 0..3 {
        app.agents.controller_gesture = mark == 0;
        app.agents.assistive_gesture = mark == 1;
        app.agents.relayed_gesture = mark == 2;
        app.change_nickname();
        assert_eq!(
            app.pending_action_refusal.take().as_deref(),
            Some(NICK_PERSON_ONLY),
            "mark {mark}"
        );
    }
    app.agents.relayed_gesture = false;
    app.machine_view = None;
    app.change_nickname();
    assert_eq!(
        app.pending_action_refusal.take().as_deref(),
        Some(NICK_UNREAD)
    );
}

#[test]
fn a_failed_set_is_kept_for_the_next_press_unless_a_later_press_superseded_it() {
    let mut app = crate::App::headless_for_test();
    let unset = |generation| NameUnset {
        generation,
        asked: Asked {
            kind: EditKind::Nickname,
            number: None,
            nick: Some("Kestrel".to_owned()),
        },
        why: "The nickname was not changed: the lock is busy.".to_owned(),
    };
    app.machine_sets = 2;
    app.machine_name_not_set(unset(1));
    assert_eq!(
        app.machine_unset, None,
        "press 2 started after press 1's set"
    );
    app.machine_name_not_set(unset(2));
    assert_eq!(app.machine_unset, Some(unset(2)));
}

/// Rename sets a nickname alone as Change Nickname… does and another number as `aterm name set`
/// does; fields that ask for nothing set nothing.
#[test]
fn rename_writes_what_its_fields_ask() {
    use crate::machine_rename_sheet::RenameTo;
    assert!(rename_change(RenameTo::Keep).is_none());
    let word = Nickname::parse("Puffin").expect("a nickname");
    assert!(matches!(
        rename_change(RenameTo::Nick(word.clone())),
        Some(NameChange::Nickname(NickTo::Word(set))) if set == word
    ));
    let to = NameTo {
        num: Number::new(86).expect("a number"),
        nick: Some(word),
    };
    assert!(matches!(
        rename_change(RenameTo::Name(to.clone())),
        Some(NameChange::Name(set)) if set == to
    ));
}

/// A set that fails keeps what its press typed, field by field, for the next press of the same
/// menu row, which takes it; a press of the other row leaves it for its own.
#[test]
fn a_failed_set_refills_the_next_press_of_its_own_row() {
    let asked = Asked {
        kind: EditKind::Name,
        number: Some("m86".to_owned()),
        nick: None,
    };
    let failed = unset_of(3, &asked, "the vault is busy");
    assert_eq!(failed.why, "The name was not changed: the vault is busy.");
    assert_eq!(failed.asked, asked);
    let mut kept = Some(failed.clone());
    assert_eq!(unset_for(&mut kept, EditKind::Nickname), None);
    assert_eq!(kept.as_ref(), Some(&failed), "left for Rename");
    assert_eq!(unset_for(&mut kept, EditKind::Name), Some(failed));
    assert_eq!(kept, None, "taken");
}

#[test]
fn a_machine_rename_refuses_a_gesture_the_person_did_not_make() {
    let mut app = crate::App::headless_for_test();
    app.machine_view = Some(view("m184", "Linnet", true));
    app.agents.controller_gesture = true;
    app.rename_machine();
    assert_eq!(
        app.pending_action_refusal.take().as_deref(),
        Some(NAME_PERSON_ONLY)
    );
    assert_eq!(app.machine_sets, 0);
}

#[test]
fn the_writer_applies_number_and_nickname_requests_in_submission_order() {
    use aterm_agent::machine_name::{set_name, set_nick};
    use aterm_net::fleet_member::name_record::{ByKind, FLOOR_MS};
    use aterm_secrets::client::LabLabels;
    let root = state();
    let now = FLOOR_MS + 300 * 86_400_000;
    let number = |n| {
        NameChange::Name(NameTo {
            num: Number::new(n).expect("a number"),
            nick: None,
        })
    };
    let nickname =
        |word| NameChange::Nickname(NickTo::Word(Nickname::parse(word).expect("a nickname")));
    let (send, jobs) = mpsc::channel();
    for (generation, to) in [
        number(86),
        nickname("MissBunny"),
        number(87),
        nickname("Heron"),
    ]
    .into_iter()
    .enumerate()
    {
        send.send(NameJob {
            to,
            generation: u64::try_from(generation).expect("a generation"),
            asked: Asked {
                kind: EditKind::Name,
                number: None,
                nick: None,
            },
        })
        .expect("queues");
    }
    drop(send);
    let mut applied = Vec::new();
    write_changes(jobs, |job| {
        let stamp = now + job.generation;
        let name = match job.to {
            NameChange::Name(to) => {
                set_name(
                    root.path(),
                    &to,
                    || Ok(LabLabels::default()),
                    ByKind::Person,
                    || Ok(None),
                    stamp,
                    false,
                )
                .expect("sets a number")
                .name
            }
            NameChange::Nickname(to) => {
                set_nick(root.path(), &to, ByKind::Person, || Ok(None), stamp, false)
                    .expect("sets a nickname")
                    .name
            }
        };
        applied.push((job.generation, name.title(), name.rev));
    });
    assert_eq!(
        applied
            .iter()
            .map(|(generation, _, _)| *generation)
            .collect::<Vec<_>>(),
        [0, 1, 2, 3]
    );
    assert_eq!(
        applied.iter().map(|(_, _, rev)| *rev).collect::<Vec<_>>(),
        [1, 2, 3, 4],
        "number and nickname edits advance the same local revision"
    );
    assert_eq!(applied[1].1, "m86 MissBunny");
    assert_eq!(applied[2].1, "m87 MissBunny");
    assert_eq!(
        file::read(root.path())
            .expect("reads")
            .expect("a name")
            .title(),
        "m87 Heron"
    );
}

#[test]
fn reading_and_posting_a_snapshot_share_the_same_serial_order() {
    let root = state();
    let posted = post_read(
        root.path(),
        None,
        || {
            assert!(
                matches!(
                    READ_POST.try_lock(),
                    Err(std::sync::TryLockError::WouldBlock)
                ),
                "a second reader cannot overtake this read"
            );
            Ok(None)
        },
        |_| {
            assert!(
                matches!(
                    READ_POST.try_lock(),
                    Err(std::sync::TryLockError::WouldBlock)
                ),
                "the snapshot posts before another read can publish"
            );
            true
        },
    )
    .expect("a first read");
    assert!(posted);
}

#[test]
fn an_authoritative_name_file_reset_replaces_the_windows_higher_revision() {
    use aterm_agent::machine_name::file::MachineName;
    use aterm_types::machine_name::{Class, Number};

    let root = state();
    let mut original =
        MachineName::unnumbered(Nickname::parse("MissBunny").expect("a nickname"), 1);
    original.num = Some(Number::new(86).expect("a number"));
    original.class = Class::Chosen;
    original.rev = 9;
    file::read_or_create(root.path(), || Ok(original)).expect("the existing name");
    let mut app = crate::App::headless_for_test();
    post_read(
        root.path(),
        None,
        || Ok(None),
        |view| {
            app.machine_name_read(view);
            true
        },
    )
    .expect("the first serialized read");
    assert_eq!(app.machine_view.as_ref().expect("the first view").rev, 9);
    std::fs::remove_file(root.path().join(file::FILE)).expect("the local file was reset");
    post_read(
        root.path(),
        None,
        || Ok(None),
        |view| {
            app.machine_name_read(view);
            true
        },
    )
    .expect("the authoritative read after a reset");
    let restored = app.machine_view.as_ref().expect("the replacement view");
    assert_eq!((restored.name.as_str(), restored.rev), ("m0", 0));
    let stored = file::read(root.path())
        .expect("reads")
        .expect("the replacement file");
    assert_eq!(restored.word, stored.nick.as_str());
}

/// A read name reaches the status item: the glance the item is built from carries the title, and
/// the read owes the item a rebuild though no session row moved (machine names §5.5).
#[test]
fn a_read_name_leads_the_status_items_tooltip_and_this_mac_row() {
    let mut app = crate::App::headless_for_test();
    let _ = app.take_status_rebuild();
    let before = app.status_glance();
    assert_eq!(before.machine, None, "CONTROL: nothing read yet");
    assert_eq!(before.tooltip(), "aterm");
    app.machine_name_read(view("m8", "Heron", true));
    assert!(
        app.take_status_rebuild(),
        "the read owes the item a rebuild"
    );
    let after = app.status_glance();
    assert_eq!(after.machine.as_deref(), Some("m8 Heron"));
    assert_eq!(after.tooltip(), "aterm \u{b7} m8 Heron");
    assert!(
        crate::status_item::compose_status_menu(&after, 0)
            .iter()
            .any(|row| matches!(
                row,
                crate::status_item::StatusRow::Info(line) if line.starts_with("m8 Heron \u{2014} ")
            )),
        "This Mac's row is led by the title"
    );
    assert_ne!(before.fingerprint(), after.fingerprint());
}
