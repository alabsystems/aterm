// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::editor::{EditorBufferView, EditorLines, Selection, editor};

const PROFILES: [Profile; 4] = [
    Profile::Gui(Platform::Mac),
    Profile::Gui(Platform::Windows),
    Profile::Gui(Platform::Other),
    Profile::Terminal,
];

/// PAGE DOWN AND PAGE UP PAGE THE CARET WITH THE VIEW: a screen less two
/// lines of context each press, the caret keeping its row on screen and
/// its goal column across a short line; at the end the view stops and the
/// caret goes on to the last line, so no press is a no-op before the end
/// of the document; Page Up mirrors it back to the top.
#[test]
fn page_down_and_up_move_the_caret_and_the_view_a_screen_at_a_time() {
    let mut text = (0..300)
        .map(|line| format!("line {line:03}\n"))
        .collect::<String>();
    text = text.replacen("line 033\n", "s\n", 1);
    let (mut store, mut workspace, mut view, _) = editor(&text);
    view.reconcile_viewport(&text, 35);
    view.selections = vec![Selection::caret("line ".len())];
    let lines = EditorLines::scanning(&text);
    let at = |view: &EditorBufferView| {
        (
            lines.number_at(view.viewport_anchor),
            lines.number_at(view.primary_selection().head),
        )
    };
    workspace
        .execute(&mut store, &mut view, EditorCommand::ScrollPageDown)
        .unwrap();
    assert_eq!(at(&view), (33, 33));
    assert_eq!(view.primary_selection().head, text.find("s\n").unwrap() + 1);
    workspace
        .execute(&mut store, &mut view, EditorCommand::ScrollPageDown)
        .unwrap();
    assert_eq!(at(&view), (66, 66));
    let line_66 = text.find("line 066").unwrap();
    assert_eq!(
        view.primary_selection().head,
        line_66 + "line ".len(),
        "goal column"
    );
    let mut previous = at(&view);
    loop {
        workspace
            .execute(&mut store, &mut view, EditorCommand::ScrollPageDown)
            .unwrap();
        let now = at(&view);
        if now == previous {
            break;
        }
        assert!(
            now.1 > previous.1,
            "every press moves the caret until the end"
        );
        previous = now;
    }
    assert_eq!(
        previous.1, 300,
        "the caret reaches the last line (after the final newline)"
    );
    assert_eq!(previous.0, 301 - 35, "the view stops at the last full page");
    while at(&view).1 > 0 {
        let before = at(&view);
        workspace
            .execute(&mut store, &mut view, EditorCommand::ScrollPageUp)
            .unwrap();
        assert!(at(&view).1 < before.1, "every press moves the caret back");
    }
    assert_eq!(at(&view), (0, 0));

    if Platform::HOST == Platform::Windows {
        // Ctrl+V is paste there; Page Down and Page Up are the keys.
        return;
    }
    let (mut store, mut workspace, mut view, _) = editor(&text);
    view.reconcile_viewport(&text, 35);
    workspace
        .handle_chord(&mut store, &mut view, KeyChord::parse("C-v").unwrap())
        .unwrap();
    assert_eq!(at(&view), (33, 33));
    workspace
        .handle_chord(&mut store, &mut view, KeyChord::parse("M-v").unwrap())
        .unwrap();
    assert_eq!(at(&view), (0, 0));
}

/// Each spelling in a row of alternates is a chord of its own: `M-b` and
/// ⌥← (`M-left`) both move back a word, `M-f` and ⌥→ (`M-right`) forward,
/// and none is a prefix waiting for the other.
#[test]
fn alternate_keys_bind_single_chords() {
    let chord = |s: &str| KeyChord::parse(s).expect("test chord parses");
    for profile in PROFILES {
        let map = Keymap::emacs(profile);
        let mut word_keys = vec![
            ("M-b", EditorCommand::MoveWordBackward),
            ("M-left", EditorCommand::MoveWordBackward),
            ("M-f", EditorCommand::MoveWordForward),
            ("M-right", EditorCommand::MoveWordForward),
        ];
        if profile != Profile::Gui(Platform::Mac) {
            word_keys.push(("C-left", EditorCommand::MoveWordBackward));
            word_keys.push(("C-right", EditorCommand::MoveWordForward));
        }
        for (key, command) in word_keys {
            assert_eq!(
                map.resolve(&[chord(key)]),
                KeyResolution::Command(command),
                "{key} is one chord on {profile:?}"
            );
        }
        assert_eq!(
            map.resolve(&[chord("M-b"), chord("M-left")]),
            KeyResolution::Unbound
        );
        assert_eq!(
            map.resolve(&[chord("C-b")]),
            KeyResolution::Command(EditorCommand::MoveBackward),
            "C-b stays by-character on {profile:?}"
        );
    }

    let text = "alpha beta gamma";
    let (mut store, mut workspace, mut view, _) = editor(text);
    view.selections = vec![Selection::caret(text.len())];
    for (key, head) in [("M-left", 11), ("M-b", 6), ("M-right", 10), ("M-f", 16)] {
        workspace
            .handle_chord(&mut store, &mut view, chord(key))
            .unwrap();
        assert_eq!(view.primary_selection().head, head, "after {key}");
        assert!(!view.chord_pending(), "{key} left a prefix pending");
    }
}

/// The bindings of `map` with more than one chord that `EMACS_SEQUENCES`
/// does not declare, each as its display string.
fn undeclared_sequences(map: &Keymap) -> Vec<String> {
    fn walk(node: &KeyNode, path: &mut Vec<KeyChord>, out: &mut Vec<Vec<KeyChord>>) {
        if node.command.is_some() && path.len() > 1 {
            out.push(path.clone());
        }
        for (chord, child) in &node.children {
            path.push(chord.clone());
            walk(child, path, out);
            path.pop();
        }
    }
    let declared = EMACS_SEQUENCES
        .iter()
        .map(|(sequence, _)| {
            sequence
                .split_whitespace()
                .map(|chord| KeyChord::parse(chord).expect("declared chord parses"))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let mut bound = Vec::new();
    walk(&map.root, &mut Vec::new(), &mut bound);
    bound
        .into_iter()
        .filter(|sequence| !declared.contains(sequence))
        .map(|sequence| {
            sequence
                .iter()
                .map(KeyChord::display)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

/// Every multi-chord binding in each platform's built-in keymap is a
/// declared sequence, so no row of alternates, platform arm or later edit
/// can bind one by accident; every alternate is bound as a chord; and no
/// bound chord is also a prefix, which would hide the sequences under it.
#[test]
fn no_row_binds_an_accidental_sequence() {
    fn no_shadowed_prefix(node: &KeyNode) {
        for (chord, child) in &node.children {
            assert!(
                child.command.is_none() || child.children.is_empty(),
                "{} is both a command and a prefix",
                chord.display()
            );
            no_shadowed_prefix(child);
        }
    }
    for profile in PROFILES {
        let map = Keymap::emacs(profile);
        assert_eq!(
            undeclared_sequences(&map),
            Vec::<String>::new(),
            "{profile:?}"
        );
        let profile_rows = profile.chord_rows().iter().copied().flatten();
        for (chords, _) in EMACS_CHORDS.iter().chain(profile_rows) {
            for key in *chords {
                let chord = KeyChord::parse(key).expect("built-in chord parses");
                assert!(
                    matches!(map.resolve(&[chord]), KeyResolution::Command(_)),
                    "{key} is not bound as one chord on {profile:?}"
                );
            }
        }
        no_shadowed_prefix(&map.root);
    }
    let mut refused = Keymap::default();
    assert!(
        refused
            .bind_alternatives(&["M-b", "C-x u"], EditorCommand::MoveWordBackward)
            .is_err()
    );
    assert_eq!(
        refused.resolve(&[KeyChord::parse("M-b").unwrap()]),
        KeyResolution::Unbound
    );

    let mut old = Keymap::emacs(Profile::Terminal);
    old.bind("M-b M-left", EditorCommand::MoveWordBackward)
        .unwrap();
    assert_eq!(undeclared_sequences(&old), ["M-b M-left"]);
}

/// The real sequences still bind as sequences: their first chord is a
/// prefix and the whole sequence runs its command.
#[test]
fn sequences_still_bind_as_prefixes() {
    let keys = |sequence: &str| {
        sequence
            .split_whitespace()
            .map(|chord| KeyChord::parse(chord).expect("test chord parses"))
            .collect::<Vec<_>>()
    };
    for profile in PROFILES {
        let map = Keymap::emacs(profile);
        for (sequence, command) in [
            ("C-x C-s", EditorCommand::Save),
            ("C-x u", EditorCommand::Undo),
            ("C-x (", EditorCommand::StartMacro),
            ("M-g g", EditorCommand::GotoLine),
        ] {
            let chords = keys(sequence);
            assert!(
                matches!(map.resolve(&chords[..1]), KeyResolution::Prefix { .. }),
                "{sequence} starts with a prefix on {profile:?}"
            );
            assert_eq!(
                map.resolve(&chords),
                KeyResolution::Command(command),
                "{sequence} on {profile:?}"
            );
        }
    }
}

/// The macOS keymap is byte-identical to what shipped before the platform
/// additions: Ctrl+arrow stays unbound there (⌥ owns word motion; Ctrl+arrow
/// belongs to Mission Control).
#[test]
fn ctrl_arrows_stay_unbound_on_macos() {
    let map = Keymap::emacs(Profile::Gui(Platform::Mac));
    let chord = |s: &str| KeyChord::parse(s).expect("test chord parses");
    assert_eq!(map.resolve(&[chord("C-left")]), KeyResolution::Unbound);
    assert_eq!(map.resolve(&[chord("C-right")]), KeyResolution::Unbound);
}

/// OFF Windows the table is exactly what it always was: `C-s` is
/// isearch (emacs's own binding) and `M-s` is unbound.
#[test]
fn ctrl_s_stays_incremental_search_off_windows() {
    let chord = |s: &str| KeyChord::parse(s).expect("test chord parses");
    for profile in [
        Profile::Gui(Platform::Mac),
        Profile::Gui(Platform::Other),
        Profile::Terminal,
    ] {
        let map = Keymap::emacs(profile);
        assert_eq!(
            map.resolve(&[chord("C-s")]),
            KeyResolution::Command(EditorCommand::IncrementalSearch),
            "{profile:?}"
        );
        assert_eq!(
            map.resolve(&[chord("M-s")]),
            KeyResolution::Unbound,
            "{profile:?}"
        );
    }
}
