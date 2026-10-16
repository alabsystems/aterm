// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 of `TrickListenerPurity` (`aterm_spec::derive::trick_listener_purity_model`):
//! the real `TrickListener`, fed the way the host's press cascade feeds it,
//! beside the line an editor would hold.
//!
//! The model keeps hidden state (the open token, the snapshot, the tentative
//! fire, whether the line named the pet), so each press is checked against
//! every model state still consistent with what was observed: some action
//! must take one of them to the listener's observers (`is_pure`,
//! `is_poisoned`, `line_chars`), the editor's line and the event it reported.
//! The input class is the trace's, read off the key and the line before it.
//! Ctrl+Z goes through the listener's own `note_job_chord`, the call the host
//! makes.
//!
//! The negative controls are the bugs the laws were written against: `site`
//! and `npm run sit` answered as if the listener had fired, a glued run that
//! named its own line, and the real listener under the host's old job-chord
//! mapping (Ctrl+Z after Esc reset a line the shell kept).
//!
//! The anchors are string coverage, as for every machine outside the aterm-gui
//! gate's step-evidence audit: `close_token` and `deliver` also run inside
//! other actions' steps.

use std::collections::{BTreeMap, BTreeSet};

use aterm_effects::typed_tricks::{TrickEvent, TrickListener};
use aterm_lexicon::TrickLexicon;
use aterm_spec::derive::{Model, trick_listener_purity_model};
use aterm_spec::{interp, xref};
use aterm_time::{Duration, Instant};

type State = BTreeMap<&'static str, i64>;

const VOCAB: &str = r#"
[[trick]]
id    = "sit"
lang  = "en"
words = ["sit"]

[[trick]]
id      = "purr"
lang    = "en"
address = ["good"]

[[vocative]]
lang  = "en"
words = ["kitty"]

[[trick]]
id    = "sit"
lang  = "zh"
words = ["坐下"]
cjk   = true

[[trick]]
id      = "play"
lang    = "zh"
words   = ["玩具"]
cjk     = true
unglued = ["玩具"]

[[vocative]]
lang  = "zh"
words = ["猫咪"]
cjk   = true
"#;

const NAMES: [&str; 2] = ["kitty", "猫咪"];

/// What one press showed: the observers, and the class the trace gave it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Seen {
    ev: i64,
    said: i64,
    pure: i64,
    poisoned: i64,
    count: i64,
    /// The editor's line is not empty. Not compared on a poisoned line: what
    /// the listener did not see is the point of poison.
    real: i64,
}

fn matches(s: &State, seen: &Seen) -> bool {
    s["ev"] == seen.ev
        && s["said"] == seen.said
        && s["pure"] == seen.pure
        && s["poisoned"] == seen.poisoned
        && s["count"] == seen.count
        && (seen.poisoned == 1 || s["real"] == seen.real)
}

/// The model states one press can lead to from `frontier` that show `seen`,
/// and the actions that got there.
fn advance(model: &Model, frontier: &[State], seen: &Seen) -> (Vec<State>, BTreeSet<&'static str>) {
    let mut next: Vec<State> = Vec::new();
    let mut by = BTreeSet::new();
    for s in frontier {
        for action in &model.actions {
            for t in model.successors(action.name, s) {
                if matches(&t, seen) {
                    by.insert(action.name);
                    if !next.contains(&t) {
                        next.push(t);
                    }
                }
            }
        }
    }
    (next, by)
}

/// The projection `#[refines]` names: the listener's observers and its answer.
fn project(l: &TrickListener, ev: i64, said: i64, line: &str) -> Seen {
    Seen {
        ev,
        said,
        pure: i64::from(l.is_pure()),
        poisoned: i64::from(l.is_poisoned()),
        count: i64::from(l.line_chars() > 0),
        real: i64::from(!line.is_empty()),
    }
}

fn said(event: Option<TrickEvent<'_>>) -> i64 {
    match event {
        None => 0,
        Some(TrickEvent::Fire {
            addressed: false, ..
        }) => 1,
        Some(TrickEvent::Fire {
            addressed: true, ..
        }) => 2,
        Some(TrickEvent::Confirm { .. }) => 3,
        Some(TrickEvent::Revoke) => 4,
    }
}

/// The trailing run of word characters: the token a delimiter would close.
fn trailing_token(line: &str) -> &str {
    let start = line
        .char_indices()
        .rev()
        .find(|(_, c)| !c.is_alphanumeric())
        .map_or(0, |(i, c)| i + c.len_utf8());
    &line[start..]
}

/// A real listener, two sessions' editor lines, and the clock. Keys: `⏎`
/// Enter, `⌫` Backspace, `⌃` Ctrl+U, `ʷ` Ctrl+W, `⎋` Esc, `ᶻ` Ctrl+Z, `①` /
/// `②` a switch to that session; anything else is typed.
struct Rig {
    l: TrickListener,
    lex: TrickLexicon,
    lines: [String; 2],
    dirty: [bool; 2],
    session: usize,
    now: Instant,
}

impl Rig {
    fn new() -> Self {
        let mut l = TrickListener::new();
        l.rekey(1);
        Rig {
            l,
            lex: TrickLexicon::from_source(VOCAB, &["en", "zh"]).expect("the fixture parses"),
            lines: [String::new(), String::new()],
            dirty: [false; 2],
            session: 0,
            now: Instant::now(),
        }
    }

    /// One press: what it showed.
    fn press(&mut self, key: char) -> Seen {
        self.now += Duration::from_millis(100);
        let poisoned = self.l.is_poisoned();
        let line = &mut self.lines[self.session];
        let empty = line.is_empty();
        let lex = Some(&self.lex);
        let (ev, answer) = match key {
            '⏎' => {
                line.clear();
                (5, said(self.l.note_submit(self.now, lex).event))
            }
            '⌃' => {
                line.clear();
                (7, said(self.l.note_line_reset().event))
            }
            '⎋' => (8, said(self.l.note_break().event)),
            'ᶻ' => {
                // A reset only of a line known to be empty; any other is lost.
                let ev = if !poisoned && self.l.line_chars() == 0 {
                    7
                } else {
                    8
                };
                (ev, said(self.l.note_job_chord().event))
            }
            '①' | '②' => {
                let to = usize::from(key == '②');
                self.dirty[self.session] = self.l.line_chars() > 0 || poisoned;
                self.session = to;
                self.l.rekey(u64::from(key == '②') + 1);
                return project(&self.l, 9 + i64::from(self.dirty[to]), 0, &self.lines[to]);
            }
            '⌫' | 'ʷ' => {
                let ev = if poisoned || empty { 11 } else { 6 };
                let event = if key == '⌫' {
                    line.pop();
                    self.l.note_backspace().event
                } else {
                    let keep = line.len() - trailing_token(line).len();
                    line.truncate(keep);
                    self.l.note_word_kill().event
                };
                (ev, said(event))
            }
            c => {
                let ev = if poisoned {
                    11
                } else if c.is_alphanumeric() {
                    1
                } else if c.is_whitespace() || c == '，' {
                    if NAMES.contains(&trailing_token(line)) {
                        4
                    } else {
                        3
                    }
                } else if ".!?,".contains(c) && NAMES.contains(&trailing_token(line)) {
                    12
                } else {
                    2
                };
                line.push(c);
                (ev, said(self.l.note_char(self.now, c, lex).event))
            }
        };
        project(&self.l, ev, answer, &self.lines[self.session])
    }
}

/// Every line of the trace, from a fresh listener: each class, each answer.
const TRACE: &str = concat!(
    "sit ⏎",                    // tentative, then confirmed by Enter
    "sit⏎",                     // fired and confirmed by the same Enter
    "kitty sit ⏎",              // named, so final
    "good kitty ⏎",             // the held address word fires at the name
    "good kitty! ⏎",            // a stop closing the name names the line too
    "sit kitty. ⏎",             // and parks its confirmation for the space
    "kitty, sit ⏎",             // and the word after it is final
    "sit tight⏎",               // prose revokes
    "sit.txt ⌃",                // the glue law
    "sit: 3⌃",                  // code
    "npm run sit ⌃",            // never on a line that is not pet talk
    "site ⌃",                   // never on a prefix
    "siy ⌫⌫t ⏎",                // a typo fixed across one boundary
    "sit ⌫e ⌃",                 // un-typing a fired boundary revokes
    "npm sit ⌫ ⌃",              // an impure snapshot restores impure
    "sit sit ⌫⌫⌫⌫⌫⌫⌫⌫⌫sit ⏎",   // a second boundary crossed, then the start
    "sotʷsit ⏎",                // a measured word-kill
    "⌫⎋sotᶻ⏎",                  // Esc, text the listener never saw, the job chord
    "si②sit ①⌃②⏎①",             // sessions left dirty come back poisoned
    "猫咪坐下 ⏎",               // a glued run waits for Enter
    "猫咪坐下 kitty ⏎",         // or a name after it
    "猫咪 猫咪坐下 ⏎",          // never one before it
    "猫咪坐下，需要注意什么 ⌃", // and prose drops it
    "猫咪玩具 ⌃",               // a thing is never half of a run
);

#[test]
fn the_real_listener_refines_the_purity_model() {
    let model = trick_listener_purity_model();
    let mut rig = Rig::new();
    let mut frontier = vec![model.init_state()];
    let mut fired = BTreeSet::new();
    for (i, key) in TRACE.chars().enumerate() {
        let seen = rig.press(key);
        let (next, by) = advance(&model, &frontier, &seen);
        assert!(
            !next.is_empty(),
            "press {i} ({key:?}) showed {seen:?}, which no action admits from {frontier:?}"
        );
        frontier = next;
        fired.extend(by);
    }
    let actions: BTreeSet<&str> = model.actions.iter().map(|a| a.name).collect();
    assert_eq!(fired, actions, "the trace drives every action");

    // Every action is anchored on the real code, and nothing else is.
    let anchored: BTreeSet<&str> = xref::refinements()
        .filter(|r| r.machine == model.name)
        .map(|r| r.action)
        .collect();
    assert_eq!(anchored, actions, "the anchors name the model's actions");
}

/// Replays `keys` on the real listener, then answers the last press as
/// `forged` instead of what the listener said: from the same states, the
/// model must refuse it and its `Buggy = 1` mutant admit it.
fn caught(keys: &str, forged: impl Fn(Seen) -> Seen) {
    let model = trick_listener_purity_model();
    let mut rig = Rig::new();
    let mut frontier = vec![model.init_state()];
    let mut keys = keys.chars().peekable();
    while let Some(key) = keys.next() {
        let seen = rig.press(key);
        if keys.peek().is_none() {
            let seen = forged(seen);
            let buggy = interp::with_buggy(&model, 1);
            assert!(
                advance(&model, &frontier, &seen).0.is_empty(),
                "{seen:?} is refused"
            );
            assert!(
                !advance(&buggy, &frontier, &seen).0.is_empty(),
                "the mutant admits {seen:?}"
            );
            return;
        }
        frontier = advance(&model, &frontier, &seen).0;
        assert!(!frontier.is_empty(), "{key:?}: {seen:?}");
    }
}

#[test]
fn a_fire_on_a_prefix_on_prose_or_on_a_glued_name_is_refused() {
    let fired = |seen: Seen| Seen { said: 1, ..seen };
    // `sit` of `site`, on its completing letter.
    caught("sit", fired);
    // `npm run sit␣`: a word on a line that is not pet talk.
    caught("npm run sit ", fired);
    // `猫咪坐下␣` alone, final as if the name inside it named the line.
    caught("猫咪坐下 ", |seen| Seen { said: 2, ..seen });
}

/// The host's old mapping: Ctrl+Z on a zero count reset the line even when
/// Esc had poisoned it and the shell still held `sot`. No step of the model
/// admits the fresh, empty-counted line beside the editor's text.
#[test]
fn a_job_chord_that_resets_a_poisoned_line_is_refused() {
    let model = trick_listener_purity_model();
    let mut rig = Rig::new();
    let mut frontier = vec![model.init_state()];
    for key in "⎋sot".chars() {
        frontier = advance(&model, &frontier, &rig.press(key)).0;
    }
    assert!(rig.l.is_poisoned() && rig.l.line_chars() == 0);
    let answer = said(rig.l.note_line_reset().event);
    let seen = project(&rig.l, 7, answer, &rig.lines[0]);
    assert_eq!((seen.poisoned, seen.count, seen.real), (0, 0, 1));
    assert!(advance(&model, &frontier, &seen).0.is_empty());
}
