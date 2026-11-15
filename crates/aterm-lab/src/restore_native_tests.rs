// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn fixture_quit_confirms_only_the_owned_quit_and_requires_its_acknowledgement() {
    let mut commands = Vec::new();
    request_fixture_quit(|command| {
        commands.push(command.to_string());
        Ok(if command == "invoke Quit" {
            "OK confirm pending kind=quit window=0; `confirm yes|no` answers it".into()
        } else {
            "OK answered=yes kind=quit".into()
        })
    })
    .unwrap();
    assert_eq!(commands, ["invoke Quit", "confirm yes"]);

    commands.clear();
    request_fixture_quit(|command| {
        commands.push(command.to_string());
        Ok(if command == "invoke Quit" {
            "OK confirm pending window=0 kind=quit; Help wording may change.".into()
        } else {
            "OK answered=yes kind=quit".into()
        })
    })
    .unwrap();
    assert_eq!(commands, ["invoke Quit", "confirm yes"]);

    commands.clear();
    request_fixture_quit(|command| {
        commands.push(command.to_string());
        Ok("OK invoked Quit".into())
    })
    .unwrap();
    assert_eq!(commands, ["invoke Quit"]);

    for reply in [
        Err("disconnected".into()),
        Ok("ERR quit refused".into()),
        Ok("OK confirm pending kind=close window=0; `confirm yes|no` answers it".into()),
        Ok("OK confirm pending kind=quit window=1; `confirm yes|no` answers it".into()),
        Ok("OK invoked CloseTab".into()),
    ] {
        commands.clear();
        assert!(
            request_fixture_quit(|command| {
                commands.push(command.to_string());
                reply.clone()
            })
            .is_err()
        );
        assert_eq!(
            commands,
            ["invoke Quit"],
            "unexpected reply must never confirm another action"
        );
    }
    for answer in [
        Err("disconnected".into()),
        Ok("ERR nothing pending".into()),
        Ok("OK answered=no kind=quit".into()),
        Ok("OK answered=yes kind=close".into()),
    ] {
        assert!(
            request_fixture_quit(|command| {
                if command == "invoke Quit" {
                    Ok("OK confirm pending kind=quit window=0; `confirm yes|no` answers it".into())
                } else {
                    answer.clone()
                }
            })
            .is_err()
        );
    }
}

#[test]
fn fixture_quit_requires_positive_process_absence() {
    use aterm_uds::process::ProcessPresence;
    assert!(fixture_gui_absent(ProcessPresence::Absent, None).unwrap());
    assert!(!fixture_gui_absent(ProcessPresence::Present, Some(true)).unwrap());
    assert!(!fixture_gui_absent(ProcessPresence::Exited, None).unwrap());
    assert!(fixture_gui_absent(ProcessPresence::Present, None).is_err());
    assert!(fixture_gui_absent(ProcessPresence::Present, Some(false)).is_err());
    assert!(fixture_gui_absent(ProcessPresence::Unknown, None).is_err());
}

const MIXED: &str = "OK 6
window id=0 focused=true tabs=2 active=7 rows=48 cols=110
 tab id=7 index=1 kind=terminal title=\"shell\" state=idle active=true parked=false
  leaf path=/ view=9 rect_px=0,0,880,720 focused=true kind=terminal session=0
 tab id=8 index=2 kind=mixed title=\"file\" state=idle active=false parked=false
  split path=/ axis=horizontal ratio=0.5000 rect_px=0,0,880,720
   leaf path=/0 view=10 rect_px=0,0,439,720 focused=false kind=app app=editor title=\"restore-00.txt\"
   leaf path=/1 view=11 rect_px=440,0,440,720 focused=true kind=terminal session=1
";

#[test]
fn topology_oracle_ignores_reminted_ids_but_keeps_leaf_placement_and_focus() {
    let before = shape(MIXED).unwrap();
    let reminted = MIXED
        .replace("view=9", "view=90")
        .replace("view=10", "view=100")
        .replace("view=11", "view=110")
        .replace("session=0", "session=40")
        .replace("session=1", "session=41");
    assert!(same_shape(&before, &shape(&reminted).unwrap()).is_ok());
    let moved = reminted
        .replace("path=/0 view=100", "path=/1 view=100")
        .replace("path=/1 view=110", "path=/0 view=110");
    assert!(same_shape(&before, &shape(&moved).unwrap()).is_err());
    let changed_focus = MIXED
        .replace("focused=false kind=app", "focused=true kind=app")
        .replace(
            "focused=true kind=terminal session=1",
            "focused=false kind=terminal session=1",
        );
    assert!(same_shape(&before, &shape(&changed_focus).unwrap()).is_err());
}

#[test]
fn matching_session_counts_do_not_hide_a_missing_native_leaf_or_duplicate_identity() {
    assert!(shape(&MIXED.replace("kind=app app=editor", "kind=missing app=editor")).is_err());
    assert!(shape(&MIXED.replace("view=11", "view=9")).is_err());
    assert!(shape(&MIXED.replace("session=1", "session=0")).is_err());
    assert!(shape(&MIXED.replace("tabs=2", "tabs=3")).is_err());
    for index in ["0", "3", "18446744073709551615", "18446744073709551616"] {
        assert!(shape(&MIXED.replace("index=1", &format!("index={index}"))).is_err());
    }
    assert!(shape(&MIXED.replace("tabs=2", "tabs=0")).is_err());
    assert!(
        shape("window id=0 focused=true tabs=1 active=7 rows=48 cols=110\nleaf path=/ view=9 rect_px=0,0,880,720 focused=true kind=terminal session=0\n").is_err(),
        "a leaf cannot index an absent tab",
    );
}

#[test]
fn aggregate_leaf_counts_do_not_prove_a_mixed_tab_fixture() {
    let mixed = shape(MIXED).unwrap();
    judge_seed(&mixed, Case::Mixed, 1, 2).unwrap();
    let separated = "window id=0 focused=true tabs=3 active=7 rows=48 cols=110
tab id=7 index=1 kind=terminal title=\"shell\" state=idle active=true parked=false
leaf path=/ view=9 rect_px=0,0,880,720 focused=true kind=terminal session=0
tab id=8 index=2 kind=app title=\"file\" state=idle active=false parked=false
leaf path=/ view=10 rect_px=0,0,880,720 focused=true kind=app app=editor title=\"restore-00.txt\"
tab id=9 index=3 kind=terminal title=\"shell\" state=idle active=false parked=false
leaf path=/ view=11 rect_px=0,0,880,720 focused=true kind=terminal session=1
";
    let separated = shape(separated).unwrap();
    assert_eq!(
        (mixed.native, mixed.terminal.len()),
        (separated.native, separated.terminal.len())
    );
    assert!(judge_seed(&separated, Case::Mixed, 1, 2).is_err());
}

#[test]
fn native_fixture_requires_app_tab_kind_and_editor_leaf() {
    let native = "window id=0 focused=true tabs=2 active=7 rows=48 cols=110
tab id=7 index=1 kind=terminal title=\"shell\" state=idle active=true parked=false
leaf path=/ view=9 rect_px=0,0,880,720 focused=true kind=terminal session=0
tab id=8 index=2 kind=app title=\"file\" state=idle active=false parked=false
leaf path=/ view=10 rect_px=0,0,880,720 focused=true kind=app app=editor title=\"restore-00.txt\"
";
    judge_seed(&shape(native).unwrap(), Case::Native, 1, 2).unwrap();
    let wrong_tab = native.replace("index=2 kind=app", "index=2 kind=editor");
    assert!(judge_seed(&shape(&wrong_tab).unwrap(), Case::Native, 1, 2).is_err());
    assert!(shape(&native.replace("app=editor", "app=markdown")).is_err());
    assert!(judge_seed(&shape(native).unwrap(), Case::Native, 2, 2).is_err());
}

const EDITOR_FIRST: &str = "window id=0 focused=true tabs=2 active=7 rows=48 cols=110
tab id=8 index=1 kind=app title=\"file\" state=idle active=false parked=false
leaf path=/ view=10 rect_px=0,0,880,720 focused=true kind=app app=editor title=\"restore-00.txt\"
tab id=7 index=2 kind=terminal title=\"shell\" state=idle active=true parked=false
leaf path=/ view=9 rect_px=0,0,880,720 focused=true kind=terminal session=0
window id=5 focused=false tabs=1 active=12 rows=48 cols=110
tab id=12 index=1 kind=app title=\"extra\" state=idle active=true parked=false
leaf path=/ view=15 rect_px=0,0,880,720 focused=true kind=app app=editor title=\"restore-extra-window.txt\"
";

#[test]
fn editor_first_topology_requires_the_exact_native_window_and_later_bootstrap() {
    let original = shape(EDITOR_FIRST).unwrap();
    judge_seed(&original, Case::EditorFirstNativeWindow, 1, 2).unwrap();
    assert_eq!(original.window_tabs, [2, 1]);
    assert_eq!(original.terminal, BTreeMap::from([("1:2:/".into(), 0)]));
    assert_eq!(
        original.native_views,
        BTreeMap::from([("1:1:/".into(), 10), ("2:1:/".into(), 15)])
    );
    let reminted = EDITOR_FIRST
        .replace("id=5", "id=50")
        .replace("view=15", "view=150");
    same_shape(&original, &shape(&reminted).unwrap()).unwrap();
    let folded = EDITOR_FIRST
        .replace("tabs=2", "tabs=3")
        .replace(
            "window id=5 focused=false tabs=1 active=12 rows=48 cols=110\n",
            "",
        )
        .replace("id=12 index=1", "id=12 index=3")
        .replace(
            "state=idle active=true parked=false\nleaf path=/ view=15",
            "state=idle active=false parked=false\nleaf path=/ view=15",
        );
    let folded = shape(&folded).unwrap();
    assert_eq!(folded.native, original.native);
    assert!(judge_seed(&folded, Case::EditorFirstNativeWindow, 1, 2).is_err());
    assert!(same_shape(&original, &folded).is_err());
    for invalid in [
        EDITOR_FIRST.replace("id=5", "id=0"),
        EDITOR_FIRST.replace("id=12", "id=8"),
        EDITOR_FIRST.replace("view=15", "view=10"),
        EDITOR_FIRST.replace("id=12 index=1", "id=12 index=3"),
        EDITOR_FIRST.replace("tabs=1 active=12", "tabs=2 active=12"),
        EDITOR_FIRST.replace("focused=true kind=app", "focused=false kind=app"),
    ] {
        assert!(
            shape(&invalid).is_err(),
            "accepted malformed multi-window placement: {invalid}"
        );
    }
    assert!(judge_seed(&original, Case::EditorFirst, 1, 2).is_err());
    assert!(judge_seed(&shape(MIXED).unwrap(), Case::EditorFirst, 1, 2).is_err());
}

#[test]
fn native_uri_placement_uses_view_identity_not_equal_titles_or_leaf_counts() {
    let topology = shape(EDITOR_FIRST).unwrap();
    let documents = "OK 2\ndoc=1 uri=file:///first.txt seq=0 dirty=0 disk=same views=10 ticket=- cursor=- sel=-\n\
doc=2 uri=file:///second.txt seq=0 dirty=0 disk=same views=15 ticket=- cursor=- sel=-\n";
    let expected = BTreeMap::from([
        ("1:1:/".into(), "file:///first.txt".into()),
        ("2:1:/".into(), "file:///second.txt".into()),
    ]);
    assert_eq!(document_positions(&topology, documents).unwrap(), expected);
    let swapped = documents
        .replace("views=10", "views=999")
        .replace("views=15", "views=10")
        .replace("views=999", "views=15");
    assert_ne!(document_positions(&topology, &swapped).unwrap(), expected);
    for invalid in [
        documents.replace("views=15", "views=10"),
        documents.replace("views=15", "views=16"),
        documents.replace("doc=2", "doc=1"),
        documents.replace("file:///second.txt", "file:///first.txt"),
        documents.replace("dirty=0", "dirty=1"),
        documents.replace("views=15", "views=-"),
        documents.replace("views=15", "views=15 views=16"),
        documents.replace("OK 2", "OK 1"),
    ] {
        assert!(
            document_positions(&topology, &invalid).is_err(),
            "accepted bad document placement: {invalid}"
        );
    }
}

#[test]
fn a_new_terminal_cannot_substitute_for_the_live_bootstrap() {
    let original = Session {
        local: 0,
        sid: "original".into(),
        window: "0".into(),
        title: "shell".into(),
        active: true,
        program: "cat".into(),
        focused: true,
    };
    same_bootstrap(&original, &original).unwrap();
    for changed in [
        Session {
            local: 1,
            ..original.clone()
        },
        Session {
            sid: "replacement".into(),
            ..original.clone()
        },
        Session {
            window: "1".into(),
            ..original.clone()
        },
    ] {
        assert!(same_bootstrap(&original, &changed).is_err());
    }
}

fn complete() -> Observation {
    Observation {
        calls: 5,
        max_ns: 50,
        first: 110,
        last: 500,
        max_first: 400,
        max_last: 450,
        finished: true,
        key_first: 100,
        key_last: 600,
        posted: 100,
        enabled: true,
        refused: false,
        gate_released: true,
        key_active: false,
        key_conflict: false,
        burst: 7,
    }
}

#[test]
fn input_posting_after_restore_cannot_be_accepted_as_overlap() {
    assert!(overlap(&complete()).is_ok());
    let mut delayed = complete();
    delayed.key_first = 501;
    delayed.key_last = 700;
    assert!(overlap(&delayed).is_err());
    let mut ended = complete();
    ended.key_last = 499;
    assert!(overlap(&ended).is_err());
}

#[test]
fn callback_evidence_must_cover_completion_and_the_measured_maximum() {
    let mut incomplete = complete();
    incomplete.finished = false;
    assert!(overlap(&incomplete).is_err());
    let mut excludes_tail = complete();
    excludes_tail.max_last = 550;
    excludes_tail.max_ns = 150;
    assert!(overlap(&excludes_tail).is_err());
    let mut wrong_clock = complete();
    wrong_clock.max_ns = 49;
    assert!(overlap(&wrong_clock).is_err());
}

#[test]
fn whole_burst_endpoints_do_not_invent_an_overlapping_key_cohort() {
    let posts = [(100, 105), (200, 205), (300, 305)];
    assert_eq!(cohort(&posts, 150, 250), (1, 0));
    assert_eq!(cohort(&posts, 201, 299), (0, 1));
    assert_eq!(cohort(&posts, 100, 305), (3, 0));
}
