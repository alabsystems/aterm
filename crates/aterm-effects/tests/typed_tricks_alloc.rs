// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exact allocation regression for the kitty-command listener.
//!
//! `TrickListener` sits on the typing path: every printed key, every
//! Backspace, every Enter in every window goes through it, feature on or off.
//! Its law is ONE inline push per letter and one fold + one hash probe per
//! closed token — and, once the vocabulary's fold scratch has grown to the
//! widest token the window can hold, NO allocation at all: not for a fire, not
//! for the word an event borrows, not for the boundary snapshot, not for a
//! held address word, a parked fire, an IME commit or a session switch.
//!
//! ONE `#[test]` IN ITS OWN BINARY, like `aterm-lexicon`'s
//! `tricks_alloc.rs`: the counter is a process-global allocator switched by a
//! global flag, so a second test running in parallel would bleed its
//! allocations into the count and make the pin flaky. Do not add tests to
//! this file.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use aterm_effects::typed_tricks::{TrickEvent, TrickListener};
use aterm_lexicon::TrickLexicon;
use aterm_time::{Duration, Instant};

struct CountingAllocator;

static COUNT_ALLOCATIONS: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: delegate the allocation unchanged to the system allocator.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() && COUNT_ALLOCATIONS.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: delegate the allocation unchanged to the system allocator.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() && COUNT_ALLOCATIONS.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: `ptr`, `layout`, and `new_size` are forwarded unchanged.
        let new_ptr = unsafe { System.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() && COUNT_ALLOCATIONS.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        new_ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` and `layout` came from this delegating allocator.
        unsafe { System.dealloc(ptr, layout) };
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn allocations_during(f: impl FnOnce()) -> usize {
    ALLOCATIONS.store(0, Ordering::Relaxed);
    COUNT_ALLOCATIONS.store(true, Ordering::Release);
    f();
    COUNT_ALLOCATIONS.store(false, Ordering::Release);
    ALLOCATIONS.load(Ordering::Relaxed)
}

/// The scripts the no-space lanes need; English rides the embedded file.
const LANES: &str = r#"
[[trick]]
id    = "sit"
lang  = "ja"
words = ["おすわり"]
cjk   = true

[[vocative]]
lang  = "ja"
words = ["ねこ"]
cjk   = true

[[trick]]
id    = "sit"
lang  = "ko"
words = ["앉아"]
cjk   = true
"#;

/// What one pass of the script heard. Plain counters: comparing them
/// allocates nothing, and they prove the counted pass really exercised every
/// lane (a script that silently stopped firing would make a zero vacuous).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct Heard {
    fires: usize,
    confirms: usize,
    revokes: usize,
    pet_only_submits: usize,
    /// Bytes of fired words, read THROUGH the borrowed `&str`.
    word_bytes: usize,
}

impl Heard {
    fn hear(&mut self, feed: aterm_effects::typed_tricks::TrickFeed<'_>) {
        match feed.event {
            Some(TrickEvent::Fire { word, .. }) => {
                self.fires += 1;
                self.word_bytes += word.len();
            }
            Some(TrickEvent::Confirm { .. }) => self.confirms += 1,
            Some(TrickEvent::Revoke) => self.revokes += 1,
            None => {}
        }
        self.pet_only_submits += usize::from(feed.submit_pet_only);
    }
}

/// A typed script: `␣` Space, `⏎` Enter, `⌫` Backspace, `⌃` Ctrl+U, `⌥`
/// Ctrl+W, `⇥` Tab; anything else a typed character. Fed the way a host
/// feeds: the vocabulary is handed over only when the listener asks.
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
        // Tentative, revoked, confirmed, final at Enter.
        "sit␣tight␣while␣I␣check⏎",
        "sit␣kitty␣⏎",
        "sit⏎",
        // Held, parked, drawn out, capitals.
        "good␣kitty!!␣⏎",
        "goooood␣kitty.⏎",
        "KITTY␣JUMP␣⏎",
        // The phrase law.
        "sit␣down␣⏎",
        // Code and prose.
        "npm␣run␣build⏎",
        "sleep␣0.5␣&&␣curl␣-s␣localhost:8080⏎",
        "play.py⏎",
        "roll␣back␣the␣last␣commit⏎",
        // Typo recovery: the snapshot, the count, the word-kill, the reset.
        "siy␣⌫⌫t␣⏎",
        "sot⌫⌫⌫⌫⌫⌫sit␣⏎",
        "kitty␣sot⌥sit␣⏎",
        "ls␣-la⌃sit␣⌃",
        "kitty␣sit␣⌫⌫⌫⌫⌫⌫⌫⌫⌫⌫⌫⌫sit␣⏎",
        // Poison.
        "si⇥t␣sit␣⏎",
        // A token past the window, and one exactly filling it.
        "pspspspspspspspspspspspspspspspspspsps␣⏎",
        "pspspspspspspspspspspspspspspsps␣⏎",
        // A marked no-space run past the window (fourteen three-scalar
        // clusters), taken back a cluster a press; then a word-kill on the
        // emptied line.
        "นั่นั่นั่นั่นั่นั่นั่นั่นั่นั่นั่นั่นั่นั่⌫⌫⌫⌫⌫⌫⌫⌫⌫⌫⌫⌫⌫⌫⌥sit␣⏎",
    ] {
        type_script(listener, english, now, script, &mut heard);
    }
    // IME commits: a syllable at a time, and a whole sentence.
    for commit in ["앉", "아"] {
        let lexicon = listener.ime_needs_lexicon(commit).then_some(lanes);
        heard.hear(listener.note_ime(*now, commit, lexicon));
    }
    type_script(listener, lanes, now, "␣⏎", &mut heard);
    let sentence = "ねこ、おすわり！";
    let lexicon = listener.ime_needs_lexicon(sentence).then_some(lanes);
    heard.hear(listener.note_ime(*now, sentence, lexicon));
    type_script(listener, lanes, now, "⏎", &mut heard);
    // Session switches: a dirty line left behind and come back to.
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

    // Warmup. The fold scratch is the listener's only heap, and it grows to
    // the widest token looked up: the window's worth of four-byte letters
    // sizes it once and for all. Then one full pass, whose tally is the
    // oracle for the counted one.
    let widest = "𝐚".repeat(TrickLexicon::MAX_SURFACE_CHARS);
    let mut warmup = Heard::default();
    type_script(&mut listener, english, &mut now, &widest, &mut warmup);
    type_script(&mut listener, english, &mut now, "␣⏎", &mut warmup);
    let expected = one_pass(&mut listener, english, &lanes, &mut now);
    assert!(
        expected.fires >= 12
            && expected.confirms >= 2
            && expected.revokes >= 2
            && expected.pet_only_submits >= 8
            && expected.word_bytes > expected.fires,
        "the script must exercise every lane: {expected:?}"
    );

    let mut counted = Heard::default();
    let allocations = allocations_during(|| {
        counted = one_pass(&mut listener, english, &lanes, &mut now);
    });
    // `assert_eq!` formats on failure only; the comparison allocates nothing,
    // and it runs after the counter is off in any case.
    assert_eq!(counted, expected, "the counted pass heard something else");
    assert_eq!(allocations, 0, "a warmed listener must not allocate");
}
