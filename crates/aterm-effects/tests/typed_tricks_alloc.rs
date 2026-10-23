// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Zero-allocation regression tests for a warmed [`TrickListener`].

use aterm_effects::typed_tricks::{TrickEvent, TrickListener};
use aterm_lexicon::TrickLexicon;
use aterm_time::{Duration, Instant};

fn allocations_during(f: impl FnOnce()) -> usize {
    crate::alloc_count::allocations_during(f).1
}

const LANES: &str = r#"
[[trick]]
id    = "sit"
lang  = "ja"
words = ["おすわり"]
cjk   = true

[[vocative]]
lang  = "ja"
words = ["ねこ", "こねこ"]
cjk   = true

[[trick]]
id      = "scold"
lang    = "ja"
words   = ["わるいこ"]
address = ["わるい"]
cjk     = true

[[trick]]
id    = "sit"
lang  = "ko"
words = ["앉아"]
cjk   = true
"#;

/// What one pass of the script heard: proof the counted pass exercised every lane, so a zero is
/// not vacuous. Plain counters, so comparing them allocates nothing.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct Heard {
    fires: usize,
    confirms: usize,
    revokes: usize,
    pet_only_submits: usize,
    /// Bytes of fired words, read through the borrowed `&str`.
    word_bytes: usize,
    /// Bytes of their command surfaces, walked as a host's ignore list walks
    /// them: fewer than `word_bytes` once a glued run has fired.
    command_bytes: usize,
    /// Fires of the glued lane alone.
    glued_fires: usize,
}

impl Heard {
    fn hear(&mut self, feed: aterm_effects::typed_tricks::TrickFeed<'_>) {
        match feed.event {
            Some(TrickEvent::Fire { word, commands, .. }) => {
                self.fires += 1;
                self.word_bytes += word.len();
                self.command_bytes += commands.iter().map(str::len).sum::<usize>();
            }
            Some(TrickEvent::Confirm { .. }) => self.confirms += 1,
            Some(TrickEvent::Revoke) => self.revokes += 1,
            None => {}
        }
        self.pet_only_submits += usize::from(feed.submit_pet_only);
    }
}

fn type_script(
    listener: &mut TrickListener,
    lexicon: &TrickLexicon,
    now: &mut Instant,
    script: &str,
    heard: &mut Heard,
) {
    for c in script.chars() {
        *now += Duration::from_millis(80);
        match c {
            '⏎' => {
                let lexicon = listener.submit_needs_lexicon().then_some(lexicon);
                heard.hear(listener.note_submit(*now, lexicon));
            }
            '⌫' => heard.hear(listener.note_backspace()),
            '⌃' => heard.hear(listener.note_line_reset()),
            '⌥' => heard.hear(listener.note_word_kill()),
            '⇥' => heard.hear(listener.note_break()),
            c => {
                let c = if c == '␣' { ' ' } else { c };
                let lexicon = listener.needs_lexicon(c).then_some(lexicon);
                heard.hear(listener.note_char(*now, c, lexicon));
            }
        }
    }
}

fn one_pass(
    listener: &mut TrickListener,
    english: &TrickLexicon,
    lanes: &TrickLexicon,
    now: &mut Instant,
) -> Heard {
    let mut heard = Heard::default();
    for script in [
        "sit␣tight␣while␣I␣check⏎",
        "sit␣kitty␣⏎",
        "sit⏎",
        "good␣kitty!!␣⏎",
        "goooood␣kitty.⏎",
        "KITTY␣JUMP␣⏎",
        "sit␣down␣⏎",
        "npm␣run␣build⏎",
        "sleep␣0.5␣&&␣curl␣-s␣localhost:8080⏎",
        "play.py⏎",
        "roll␣back␣the␣last␣commit⏎",
        "siy␣⌫⌫t␣⏎",
        "sot⌫⌫⌫⌫⌫⌫sit␣⏎",
        "kitty␣sot⌥sit␣⏎",
        "ls␣-la⌃sit␣⌃",
        "kitty␣sit␣⌫⌫⌫⌫⌫⌫⌫⌫⌫⌫⌫⌫sit␣⏎",
        "si⇥t␣sit␣⏎",
        "pspspspspspspspspspspspspspspspspspsps␣⏎",
        "pspspspspspspspspspspspspspspsps␣⏎",
        "นั่นั่นั่นั่นั่นั่นั่นั่นั่นั่นั่นั่นั่นั่⌫⌫⌫⌫⌫⌫⌫⌫⌫⌫⌫⌫⌫⌫⌥sit␣⏎",
    ] {
        type_script(listener, english, now, script, &mut heard);
    }
    for commit in ["앉", "아"] {
        let lexicon = listener.ime_needs_lexicon(commit).then_some(lanes);
        heard.hear(listener.note_ime(*now, commit, lexicon));
    }
    type_script(listener, lanes, now, "␣⏎", &mut heard);
    let sentence = "ねこ、おすわり！";
    let lexicon = listener.ime_needs_lexicon(sentence).then_some(lanes);
    heard.hear(listener.note_ime(*now, sentence, lexicon));
    type_script(listener, lanes, now, "⏎", &mut heard);
    const FULL_WINDOW_MISS: &str =
        "ねこかわいいねこかわいいねこかわいいねこかわいいねこかわいいねこ";
    assert_eq!(
        FULL_WINDOW_MISS.chars().count(),
        TrickLexicon::MAX_SURFACE_CHARS
    );
    let fires_before = heard.fires;
    for run in [
        "ねこおすわり",
        "おすわりねこ",
        "わるいこねこ",
        FULL_WINDOW_MISS,
    ] {
        let lexicon = listener.ime_needs_lexicon(run).then_some(lanes);
        heard.hear(listener.note_ime(*now, run, lexicon));
        type_script(listener, lanes, now, "␣⏎", &mut heard);
    }
    heard.glued_fires = heard.fires - fires_before;
    listener.rekey(1);
    type_script(listener, english, now, "si", &mut heard);
    listener.rekey(2);
    type_script(listener, english, now, "sit␣⏎", &mut heard);
    listener.rekey(1);
    type_script(listener, english, now, "t␣⏎", &mut heard);
    heard.hear(listener.note_no_echo());
    heard.hear(listener.note_line_reset());
    heard
}

#[test]
fn a_warmed_listener_is_allocation_free_on_every_lane() {
    let english = TrickLexicon::shared_en();
    let lanes = TrickLexicon::from_source(LANES, &["all"]).expect("the lane vocabulary parses");
    assert!(lanes.conflicts().is_empty(), "{:?}", lanes.conflicts());

    let mut listener = TrickListener::new();
    let mut now = Instant::now();

    let widest = "𝐚".repeat(TrickLexicon::MAX_SURFACE_CHARS);
    let mut warmup = Heard::default();
    type_script(&mut listener, english, &mut now, &widest, &mut warmup);
    type_script(&mut listener, english, &mut now, "␣⏎", &mut warmup);
    let expected = one_pass(&mut listener, english, &lanes, &mut now);
    assert!(
        expected.fires >= 15
            && expected.glued_fires == 3
            && expected.confirms >= 2
            && expected.revokes >= 2
            && expected.pet_only_submits >= 11
            && expected.word_bytes > expected.fires
            && expected.command_bytes > expected.fires
            && expected.command_bytes < expected.word_bytes,
        "the script must exercise every lane: {expected:?}"
    );

    let mut counted = Heard::default();
    let allocations = allocations_during(|| {
        counted = one_pass(&mut listener, english, &lanes, &mut now);
    });
    assert_eq!(counted, expected, "the counted pass heard something else");
    assert_eq!(allocations, 0, "a warmed listener must not allocate");
}
