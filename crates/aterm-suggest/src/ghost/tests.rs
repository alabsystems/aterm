// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the live-line state machine.
//!
//! The load-bearing one is [`narrowing_always_matches_a_full_rescan`]: the
//! incremental fast path is an optimization, and an optimization that can
//! disagree with the thing it replaces is a bug generator. It is checked
//! against a brute-force recompute over a deterministic pseudo-random script.

use super::*;
use crate::{SuggestConfig, SuggestMode};

const HOUR: u64 = 60 * 60 * 1000;
/// The pinned line clock every test ranks against.
const T: u64 = 100 * HOUR;

fn engine(cmds: &[(&str, Option<&str>, u64)]) -> Engine {
    let mut e = Engine::new(SuggestConfig {
        mode: SuggestMode::History,
        ..SuggestConfig::default()
    });
    for &(c, cwd, at) in cmds {
        e.record(c, cwd, Some(0), at);
    }
    e
}

fn live(buffer: &str) -> Context<'_> {
    Context {
        buffer,
        cwd: None,
        at_prompt: true,
        alt_screen: false,
        echoing: true,
    }
}

/// Type `buffer` into a fresh line and return what shows.
fn ghost_for(g: &mut Ghost, buffer: &str) -> Option<String> {
    g.update(&live(buffer));
    g.visible().map(str::to_owned)
}

#[test]
fn nothing_shows_before_a_line_begins() {
    let mut g = Ghost::new(engine(&[("cargo build", None, 0)]));
    // No `begin_line` — i.e. no OSC 133;B, so no shell integration.
    assert_eq!(ghost_for(&mut g, "car"), None);
}

#[test]
fn a_line_begins_and_the_ghost_appears() {
    let mut g = Ghost::new(engine(&[("cargo build --release", None, 0)]));
    g.begin_line(T);
    assert_eq!(ghost_for(&mut g, "cargo b"), Some("uild --release".into()));
}

#[test]
fn typing_the_suggestion_consumes_it() {
    let mut g = Ghost::new(engine(&[("git status", None, 0)]));
    g.begin_line(T);
    assert_eq!(ghost_for(&mut g, "git st"), Some("atus".into()));
    assert_eq!(ghost_for(&mut g, "git stat"), Some("us".into()));
    assert_eq!(
        ghost_for(&mut g, "git status"),
        None,
        "nothing left to offer"
    );
}

#[test]
fn diverging_kills_the_ghost() {
    let mut g = Ghost::new(engine(&[("git status", None, 0)]));
    g.begin_line(T);
    assert_eq!(ghost_for(&mut g, "git st"), Some("atus".into()));
    assert_eq!(ghost_for(&mut g, "git sto"), None, "no command matches");
}

#[test]
fn a_backwards_edit_rescans_and_can_revive_the_ghost() {
    let mut g = Ghost::new(engine(&[("git status", None, 0)]));
    g.begin_line(T);
    assert_eq!(ghost_for(&mut g, "git sto"), None);
    // Backspace.
    assert_eq!(
        ghost_for(&mut g, "git st"),
        Some("atus".into()),
        "the slow path must re-find what narrowing had dropped"
    );
}

#[test]
fn end_line_clears_and_silences() {
    let mut g = Ghost::new(engine(&[("cargo build", None, 0)]));
    g.begin_line(T);
    assert!(ghost_for(&mut g, "car").is_some());
    g.end_line();
    assert_eq!(ghost_for(&mut g, "car"), None);
}

#[test]
fn dismiss_lasts_until_the_next_line() {
    let mut g = Ghost::new(engine(&[("cargo build", None, 0)]));
    g.begin_line(T);
    assert!(ghost_for(&mut g, "car").is_some());
    g.dismiss();
    assert_eq!(ghost_for(&mut g, "carg"), None, "Escape must stick");
    assert_eq!(ghost_for(&mut g, "cargo"), None);
    g.begin_line(T + 1000);
    assert!(ghost_for(&mut g, "car").is_some(), "a new line re-arms it");
}

#[test]
fn a_context_turning_unsafe_clears_the_glass_immediately() {
    let mut g = Ghost::new(engine(&[("cargo build", None, 0)]));
    g.begin_line(T);
    assert!(ghost_for(&mut g, "car").is_some());
    let unsafe_ctx = Context {
        alt_screen: true,
        ..live("car")
    };
    g.update(&unsafe_ctx);
    assert_eq!(g.visible(), None, "entering vim must erase the ghost");
}

// ───────────────────────────── accept ─────────────────────────────

#[test]
fn accept_all_returns_the_bytes_and_empties_the_ghost() {
    let mut g = Ghost::new(engine(&[("cargo build --release", None, 0)]));
    g.begin_line(T);
    ghost_for(&mut g, "cargo b");
    assert_eq!(g.accept_all(), Some("uild --release".into()));
    assert_eq!(g.visible(), None);
}

#[test]
fn accept_all_is_none_when_nothing_shows() {
    let mut g = Ghost::new(engine(&[("cargo build", None, 0)]));
    g.begin_line(T);
    assert_eq!(
        g.accept_all(),
        None,
        "the key must keep its normal meaning when there is no ghost"
    );
}

#[test]
fn accept_word_walks_one_token_at_a_time() {
    let mut g = Ghost::new(engine(&[("git commit --amend --no-edit", None, 0)]));
    g.begin_line(T);
    assert_eq!(
        ghost_for(&mut g, "git co"),
        Some("mmit --amend --no-edit".into())
    );
    assert_eq!(g.accept_word(), Some("mmit".into()));
    assert_eq!(g.visible(), Some(" --amend --no-edit"));
    assert_eq!(g.accept_word(), Some(" --amend".into()));
    assert_eq!(g.visible(), Some(" --no-edit"));
    assert_eq!(g.accept_word(), Some(" --no-edit".into()));
    assert_eq!(g.visible(), None);
    assert_eq!(g.accept_word(), None);
}

#[test]
fn accept_word_takes_a_path_whole() {
    let mut g = Ghost::new(engine(&[("vim crates/aterm-suggest/src/lib.rs", None, 0)]));
    g.begin_line(T);
    ghost_for(&mut g, "vim c");
    assert_eq!(
        g.accept_word(),
        Some("rates/aterm-suggest/src/lib.rs".into()),
        "splitting a path into segments is the tedium partial accept avoids"
    );
}

#[test]
fn accepting_then_typing_continues_from_the_accepted_text() {
    let mut g = Ghost::new(engine(&[("cargo test --workspace", None, 0)]));
    g.begin_line(T);
    ghost_for(&mut g, "cargo t");
    assert_eq!(g.accept_word(), Some("est".into()));
    // The host wrote "est" to the PTY; the echoed buffer is now "cargo test".
    assert_eq!(
        ghost_for(&mut g, "cargo test"),
        Some(" --workspace".into()),
        "the ghost must survive its own accept"
    );
}

// ───────────── the corpus can change under a standing ghost ─────────────

#[test]
fn clearing_the_corpus_erases_a_standing_ghost() {
    let mut g = Ghost::new(engine(&[("psql -h prod.internal -U admin", None, 0)]));
    g.begin_line(T);
    assert!(ghost_for(&mut g, "psq").is_some());
    // The user-facing "forget my history" control. If narrowing kept going, the
    // very command they just erased would stay on glass.
    g.engine_mut().clear();
    assert_eq!(
        ghost_for(&mut g, "psql"),
        None,
        "clear() must invalidate the ghost, not just the corpus"
    );
}

#[test]
fn a_command_recorded_mid_line_can_change_the_ranking() {
    let mut g = Ghost::new(engine(&[("git stash list", None, 0)]));
    g.begin_line(T);
    assert_eq!(ghost_for(&mut g, "git st"), Some("ash list".into()));
    // Another pane completes `git status`, which is newer and so ranks higher.
    g.engine_mut().record("git status", None, Some(0), T + 1);
    assert_eq!(
        ghost_for(&mut g, "git sta"),
        Some("tus".into()),
        "a corpus change must force a rescan rather than narrow the stale winner"
    );
}

// ───────────────────────── accept re-offers ─────────────────────────

#[test]
fn accepting_leaves_a_longer_sibling_still_on_offer() {
    let mut g = Ghost::new(engine(&[
        ("git status --short", None, 0),
        ("git status", None, HOUR),
    ]));
    g.begin_line(T);
    assert_eq!(ghost_for(&mut g, "git st"), Some("atus".into()));
    assert_eq!(g.accept_all(), Some("atus".into()));
    // The shell echoes it; the buffer is now exactly `git status`.
    assert_eq!(
        ghost_for(&mut g, "git status"),
        Some(" --short".into()),
        "accept must not wedge the line ghost-less by pre-matching for_buffer"
    );
}

// ───────────── narrowing must not outlive the engine's gates ─────────────
//
// The fast path skips `Engine::suggest` entirely, so every refusal that lives
// INSIDE `suggest` has to gate the fast path too. These two were divergences:
// narrowing kept a ghost that a full rescan would have dropped.

#[test]
fn typing_a_space_drops_the_ghost_exactly_as_a_rescan_would() {
    let mut g = Ghost::new(engine(&[("cargo build --release", None, 0)]));
    g.begin_line(T);
    assert_eq!(ghost_for(&mut g, "cargo build"), Some(" --release".into()));
    // `Engine::suggest` refuses a buffer ending in whitespace (a ghost that
    // flickers as you type a space). The incremental path must agree.
    assert_eq!(
        ghost_for(&mut g, "cargo build "),
        None,
        "narrowing must apply the trailing-whitespace refusal too"
    );
}

#[test]
fn narrowing_to_an_all_blank_remainder_drops_the_ghost() {
    // `echo hi 世界` truncates at the wide glyph, so the completion the engine
    // hands out shrinks to a lone space as the user types — and a lone space is
    // no suggestion at all (no pixels, but it would still latch a repaint).
    let mut g = Ghost::new(engine(&[("echo hi 世界", None, 0)]));
    g.begin_line(T);
    assert_eq!(ghost_for(&mut g, "echo h"), Some("i ".into()));
    assert_eq!(
        ghost_for(&mut g, "echo hi"),
        None,
        "the remaining ' ' is blank — a rescan returns None, so narrowing must too"
    );
}

// ───────────────────── the equivalence property ─────────────────────

/// A tiny deterministic LCG — no `rand` dependency, and a fixed seed so a
/// failure is reproducible from the test name alone.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        self.0 >> 33
    }
}

#[test]
fn narrowing_always_matches_a_full_rescan() {
    const CORPUS: &[&str] = &[
        "git status",
        "git stash pop",
        "git stash list",
        "git commit --amend",
        "git checkout main",
        "cargo build",
        "cargo build --release",
        "cargo test --workspace",
        "cargo clippy --all-targets",
        "ls -la",
        "cd crates/aterm-suggest",
        "make install",
    ];
    let corpus: Vec<(&str, Option<&str>, u64)> = CORPUS
        .iter()
        .enumerate()
        .map(|(i, c)| (*c, None, i as u64 * 1000))
        .collect();

    let mut g = Ghost::new(engine(&corpus));
    let reference = engine(&corpus);

    let mut rng = Lcg(0xA7E4_2026);
    let alphabet: Vec<char> = "gictaru sl-".chars().collect();

    // Drive mostly along REAL command prefixes, so a ghost is actually standing
    // most of the time — that is the only regime where the fast path runs at
    // all. An earlier version of this test typed random junk from a small
    // alphabet; ghosts were rare, the fast path was barely exercised, and it
    // missed two divergences (trailing whitespace, all-blank remainder) that a
    // single hand-written case exposes immediately.
    for line in 0..80u64 {
        let now = T + line * 1000;
        g.begin_line(now);
        let target = CORPUS[(rng.next() as usize) % CORPUS.len()];
        let mut buffer = String::new();
        for _ in 0..28 {
            match rng.next() % 10 {
                // 60%: extend along the target command (keeps a ghost alive).
                0..=5 => {
                    if let Some(c) = target.chars().nth(buffer.chars().count()) {
                        buffer.push(c);
                    } else {
                        buffer.pop();
                    }
                }
                // 15%: backspace (forces the slow path to re-find a dropped ghost).
                6 | 7 => {
                    buffer.pop();
                }
                // 10%: a space — the trailing-whitespace refusal, WITH a live ghost.
                8 => buffer.push(' '),
                // 15%: a divergent character.
                _ => buffer.push(alphabet[(rng.next() as usize) % alphabet.len()]),
            }
            let ctx = live(&buffer);
            g.update(&ctx);
            let incremental = g.visible().map(str::to_owned);
            let full = reference
                .suggest(&ctx, now)
                .map(|s| String::from(s.completion));
            assert_eq!(
                incremental, full,
                "narrowing disagreed with a full rescan at buffer {buffer:?}"
            );
        }
        g.end_line();
    }
}

/// The same equivalence, over a corpus whose completions get TRUNCATED (wide
/// glyphs) and whose entries are destructive — the two filters that run after
/// scoring, which is where the subset argument is least obviously sound.
#[test]
fn narrowing_matches_a_rescan_across_the_post_scoring_filters() {
    const TRICKY: &[&str] = &[
        "echo hi 世界",
        "echo hint",
        "rm -rf build",
        "rsync -a src dst",
        "git push --force origin main",
        "git push origin main",
        "cd 世界",
        "cd crates",
    ];
    let corpus: Vec<(&str, Option<&str>, u64)> = TRICKY
        .iter()
        .enumerate()
        .map(|(i, c)| (*c, None, i as u64 * 1000))
        .collect();
    let mut g = Ghost::new(engine(&corpus));
    let reference = engine(&corpus);

    let mut rng = Lcg(0x5EED_1234);
    for line in 0..80u64 {
        let now = T + line * 1000;
        g.begin_line(now);
        let target = TRICKY[(rng.next() as usize) % TRICKY.len()];
        let mut buffer = String::new();
        for _ in 0..24 {
            match rng.next() % 8 {
                0..=5 => {
                    if let Some(c) = target.chars().nth(buffer.chars().count()) {
                        buffer.push(c);
                    } else {
                        buffer.pop();
                    }
                }
                6 => {
                    buffer.pop();
                }
                _ => buffer.push(' '),
            }
            let ctx = live(&buffer);
            g.update(&ctx);
            assert_eq!(
                g.visible().map(str::to_owned),
                reference
                    .suggest(&ctx, now)
                    .map(|s| String::from(s.completion)),
                "narrowing disagreed with a rescan at buffer {buffer:?}"
            );
        }
        g.end_line();
    }
}
