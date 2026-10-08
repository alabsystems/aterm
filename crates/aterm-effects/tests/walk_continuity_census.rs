// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The walk's continuity census: the typed band's colour is smooth and
//! continuous. A cell's colour at a column is `Cohort::t_at(col) = walk_zero() +
//! walk_t(walk_d(col))`; along one input neighbouring lit cells step by exactly
//! one walk step (`1/16` over the walk's first sixteen cells, `WALK_LAY_RATE`
//! after), and a wrap carries both colour and distance. The designed restarts
//! (a new line via Return, a line kill, a cross-row keyed Nav keep the colour
//! and restart the pace) never put two lit cells of one line side by side, so
//! per presented frame ([`walk_census::Snap::judge`]):
//!
//! * every pair of text-adjacent lit cells steps forward by one walk step,
//!   never back (`Back`), never the same stop twice (`Repeat`), never more than
//!   one step or any other step (`Jump`);
//! * a `1/16` step never follows a `1/36` one to its left (`Restart`), and a
//!   line's first sixteen cells step `1/16` (`NoFastLeg`);
//! * no text cell between two lit ones is dark (`Hole`);
//!
//! and across frames ([`walk_census::recolours`]): no text cell is lit again in
//! another colour while the text up to it stayed the same.
//!
//! A flag standing [`STANDING_FRAMES`] frames or more is a defect; fewer, a
//! transient. The host is the GUI's frame seam (`tests/claude_wrap_band.rs`'s
//! lock A) on a real `Terminal`: a line editor on the primary screen
//! ([`walk_census::Shell`], keys stamped as `app_input.rs` stamps them) on a
//! flat 16 ms train or the GUI's pacing, plus captured Claude Code composer
//! takes replayed byte for byte. The shell folds as bash readline does (`<glyph>
//! \r` on the key that fills the margin); scenarios that do not name a fold are
//! read behind every [`Fold`] (readline, zsh zle, and the kernel's cooked-mode
//! echo used by `cat` and `read`). Scenarios in [`KNOWN`] read a discontinuity,
//! each with its reason.
//!
//! `WALK_CENSUS=1 targo --unverified test -p aterm-effects --test
//! effects_suite walk_continuity_census::census -- --nocapture` prints every
//! verdict; the
//! other knobs are on [`census`].

use crate::walk_census::*;
use aterm_effects::cursor_glow::InsertWidth;
use aterm_effects::rainbow_kitty::ribbon::{WALK_FAST_CELLS, WALK_LAY_RATE};

/// Every scenario the census runs, by name.
pub fn scenario(name: &str) -> Shell {
    let e2 = Echo::After(2);
    match name {
        // Plain typing at 12 cps across two wraps of a 60-column shell.
        "wrap2-83ms-echo2" => {
            let mut s = Shell::new(60, 83, e2);
            s.mark("type");
            s.type_str(&prose(140));
            s.mark("idle");
            s.idle(400);
            s
        }
        "wrap2-83ms-echo0" => {
            let mut s = Shell::new(60, 83, Echo::After(0));
            s.mark("type");
            s.type_str(&prose(140));
            s.mark("idle");
            s.idle(400);
            s
        }
        "wrap2-83ms-echo20" => {
            let mut s = Shell::new(60, 83, Echo::After(20));
            s.mark("type");
            s.type_str(&prose(140));
            s.mark("idle");
            s.idle(400);
            s
        }
        "wrap2-40ms-echo2" => {
            let mut s = Shell::new(60, 40, e2);
            s.mark("type");
            s.type_str(&prose(140));
            s.mark("idle");
            s.idle(400);
            s
        }
        // Backspace x5 then retype, mid-row.
        "bs5-retype" => {
            let mut s = Shell::new(60, 83, e2);
            s.mark("type");
            s.type_str(&prose(30));
            s.mark("bs");
            for _ in 0..5 {
                s.backspace();
            }
            s.mark("retype");
            s.type_str("abcdefgh");
            s.mark("idle");
            s.idle(400);
            s
        }
        // Backspace back up through a fold, then retype across it again.
        "bs-across-wrap-retype" => {
            let mut s = Shell::new(60, 83, e2);
            s.mark("type");
            s.type_str(&prose(70));
            s.mark("bs");
            for _ in 0..16 {
                s.backspace();
            }
            s.mark("retype");
            s.type_str("walking the fold again");
            s.mark("idle");
            s.idle(400);
            s
        }
        // Backspace x5 after a rest inside the grace, then retype.
        "rest-bs-retype" => {
            let mut s = Shell::new(60, 83, e2);
            s.mark("type");
            s.type_str(&prose(30));
            s.mark("rest");
            s.idle(600);
            s.mark("bs");
            for _ in 0..5 {
                s.backspace();
            }
            s.mark("retype");
            s.type_str("abcdefgh");
            s.mark("idle");
            s.idle(400);
            s
        }
        // ^W then retype at once.
        "ctrlw-retype" => {
            let mut s = Shell::new(60, 83, e2);
            s.mark("type");
            s.type_str("the rainbow walks along every");
            s.mark("^W");
            s.word_kill();
            s.mark("retype");
            s.type_str("kitty types");
            s.mark("idle");
            s.idle(400);
            s
        }
        // ^W of the word that crossed the fold, then retype.
        "ctrlw-across-wrap-retype" => {
            let mut s = Shell::new(60, 83, e2);
            s.mark("type");
            s.type_str(&prose(55));
            s.type_str(" crossing");
            s.mark("^W");
            s.word_kill();
            s.mark("retype");
            s.type_str("walking on");
            s.mark("idle");
            s.idle(400);
            s
        }
        // ^U then retype: a new line (the pace restarts by design).
        "ctrlu-retype" => {
            let mut s = Shell::new(60, 83, e2);
            s.mark("type");
            s.type_str(&prose(40));
            s.mark("^U");
            s.line_kill();
            s.mark("retype");
            s.type_str(&prose(30));
            s.mark("idle");
            s.idle(400);
            s
        }
        // A rest past the swoosh, then typing on (the rebirth, last_walk).
        "rest-rebirth" => {
            let mut s = Shell::new(60, 83, e2);
            s.mark("type");
            s.type_str(&prose(30));
            s.mark("rest");
            s.idle(2_500);
            s.mark("type2");
            s.type_str(" and on it goes");
            s.mark("idle");
            s.idle(400);
            s
        }
        // The same behind a shell whose echo lands a frame after its key
        // (the key HELD, its cell from the seam's sweep after the revive's).
        "rest-rebirth-echo20" => {
            let mut s = Shell::new(60, 83, Echo::After(20));
            s.mark("type");
            s.type_str(&prose(30));
            s.mark("rest");
            s.idle(2_500);
            s.mark("type2");
            s.type_str(" and on it goes");
            s.mark("idle");
            s.idle(400);
            s
        }
        // A rest inside the swoosh (retracting), then typing on.
        "rest-mid-swoosh" => {
            let mut s = Shell::new(60, 83, e2);
            s.mark("type");
            s.type_str(&prose(30));
            s.mark("rest");
            s.idle(1_300);
            s.mark("type2");
            s.type_str(" and on it goes");
            s.mark("idle");
            s.idle(400);
            s
        }
        // Home, a rest, End, then typing on.
        "home-end-type" => {
            let mut s = Shell::new(60, 83, e2);
            s.mark("type");
            s.type_str(&prose(30));
            s.mark("home");
            s.home();
            s.idle(300);
            s.mark("end");
            s.end();
            s.mark("type2");
            s.type_str(" and on");
            s.mark("idle");
            s.idle(400);
            s
        }
        // Home and End across a fold (cross-row keyed Nav both ways).
        "home-end-wrapped-type" => {
            let mut s = Shell::new(60, 83, e2);
            s.mark("type");
            s.type_str(&prose(80));
            s.mark("home");
            s.home();
            s.idle(300);
            s.mark("end");
            s.end();
            s.mark("type2");
            s.type_str(" and on");
            s.mark("idle");
            s.idle(400);
            s
        }
        // Home, then typing at the line's start (an insert pushing the
        // whole line).
        "home-type" => {
            let mut s = Shell::new(60, 83, e2);
            s.mark("type");
            s.type_str(&prose(30));
            s.mark("home");
            s.home();
            s.mark("insert");
            s.type_str("XYZ ");
            s.mark("idle");
            s.idle(400);
            s
        }
        // A paste burst between typed words.
        "paste-burst" => {
            let mut s = Shell::new(60, 83, e2);
            s.mark("type");
            s.type_str("echo the ");
            s.mark("paste");
            s.paste("rainbow walks");
            s.mark("type2");
            s.type_str(" along every line");
            s.mark("idle");
            s.idle(400);
            s
        }
        // A Tab completing the word being typed.
        "tab-complete" => {
            let mut s = Shell::new(60, 83, e2);
            s.mark("type");
            s.type_str("git sta");
            s.mark("tab");
            s.tab("tus ");
            s.mark("type2");
            s.type_str("--short --branch");
            s.mark("idle");
            s.idle(400);
            s
        }
        // A Space held across the fold (auto-repeat at 33 ms).
        "space-held-across-wrap" => {
            let mut s = Shell::new(60, 83, e2);
            s.mark("type");
            s.type_str(&prose(54));
            s.mark("hold");
            for _ in 0..8 {
                s.key_at(' ', 33);
            }
            s.mark("type2");
            s.type_str("after the held space");
            s.mark("idle");
            s.idle(400);
            s
        }
        // Left x3, then typing on at the end after End.
        "left-end-type" => {
            let mut s = Shell::new(60, 83, e2);
            s.mark("type");
            s.type_str(&prose(30));
            s.mark("left");
            for _ in 0..3 {
                s.left();
            }
            s.mark("end");
            s.end();
            s.mark("type2");
            s.type_str(" on");
            s.mark("idle");
            s.idle(400);
            s
        }
        // A short word, a rest past the swoosh, and on.
        "rest-rebirth-short" => {
            let mut s = Shell::new(60, 83, e2);
            s.mark("type");
            s.type_str("hello");
            s.mark("rest");
            s.idle(2_500);
            s.mark("type2");
            s.type_str(" world");
            s.mark("idle");
            s.idle(400);
            s
        }
        // A wrapped line, a rest past the swoosh, and on.
        "rest-rebirth-wrapped" => {
            let mut s = Shell::new(60, 83, e2);
            s.mark("type");
            s.type_str(&prose(80));
            s.mark("rest");
            s.idle(2_500);
            s.mark("type2");
            s.type_str(" and on it goes");
            s.mark("idle");
            s.idle(400);
            s
        }
        // Two rests past the swoosh on one line.
        "rest-rebirth-twice" => {
            let mut s = Shell::new(60, 83, e2);
            s.mark("type");
            s.type_str("the rainbow");
            s.mark("rest");
            s.idle(2_500);
            s.mark("type2");
            s.type_str(" walks");
            s.mark("rest2");
            s.idle(2_500);
            s.mark("type3");
            s.type_str(" along");
            s.mark("idle");
            s.idle(400);
            s
        }
        // A rest past the chain window: a cold start by design.
        "rest-past-chain" => {
            let mut s = Shell::new(60, 83, e2);
            s.mark("type");
            s.type_str(&prose(30));
            s.mark("rest");
            s.idle(5_600);
            s.mark("type2");
            s.type_str(" and on it goes");
            s.mark("idle");
            s.idle(400);
            s
        }
        // A key-repeat-fast burst across the fold.
        "burst-25ms-across-wrap" => {
            let mut s = Shell::new(60, 25, e2);
            s.mark("type");
            s.type_str(&prose(90));
            s.mark("idle");
            s.idle(400);
            s
        }
        // A Tab whose completion REWRITES the word (a case-folding match):
        // the shell reprints from the word's start.
        "tab-rewrite" => {
            let mut s = Shell::new(60, 83, e2);
            s.mark("type");
            s.type_str("ls ~/doc");
            s.mark("tab");
            let gap = s.key_ms;
            s.press(
                gap,
                |g, t| {
                    g.note_user_gesture(t);
                    g.note_insert_armed(t, InsertWidth::Unknown);
                },
                |sh| {
                    let at = sh.text.len() - 3;
                    sh.text.truncate(at);
                    sh.text.extend("Documents/".chars());
                    sh.caret = sh.text.len();
                    sh.reprint(at)
                },
            );
            s.mark("type2");
            s.type_str(" -la");
            s.mark("idle");
            s.idle(400);
            s
        }
        // The GUI's real pacing, the same takes.
        "paced-wrap2-83ms" => {
            let mut s = Shell::with(60, 83, e2, "$ ", true);
            s.mark("type");
            s.type_str(&prose(140));
            s.mark("idle");
            s.idle(400);
            s
        }
        "paced-bs-across-wrap-retype" => {
            let mut s = Shell::with(60, 83, e2, "$ ", true);
            s.mark("type");
            s.type_str(&prose(70));
            s.mark("bs");
            for _ in 0..16 {
                s.backspace();
            }
            s.mark("retype");
            s.type_str("walking the fold again");
            s.mark("idle");
            s.idle(400);
            s
        }
        "paced-ctrlw-retype" => {
            let mut s = Shell::with(60, 83, e2, "$ ", true);
            s.mark("type");
            s.type_str("the rainbow walks along every");
            s.mark("^W");
            s.word_kill();
            s.mark("retype");
            s.type_str("kitty types");
            s.mark("idle");
            s.idle(400);
            s
        }
        "paced-rest-rebirth" => {
            let mut s = Shell::with(60, 83, e2, "$ ", true);
            s.mark("type");
            s.type_str(&prose(30));
            s.mark("rest");
            s.idle(2_500);
            s.mark("type2");
            s.type_str(" and on it goes");
            s.mark("idle");
            s.idle(400);
            s
        }
        // A lagging shell: echoes 60 ms behind 40 ms keys, across the fold.
        "lag60-40ms-across-wrap" => {
            let mut s = Shell::new(60, 83, e2);
            s.mark("type");
            s.type_lagged(&prose(90), 40, 60);
            s.mark("idle");
            s.idle(400);
            s
        }
        "paced-lag60-40ms-across-wrap" => {
            let mut s = Shell::with(60, 83, e2, "$ ", true);
            s.mark("type");
            s.type_lagged(&prose(90), 40, 60);
            s.mark("idle");
            s.idle(400);
            s
        }
        // A long prompt: the line folds inside its fast leg.
        "long-prompt-early-fold" => {
            let prompt = format!("{}$ ", "~".repeat(48));
            let mut s = Shell::with(60, 83, e2, &prompt, false);
            s.mark("type");
            s.type_str(&prose(40));
            s.mark("idle");
            s.idle(400);
            s
        }
        // A 20-column pane: four folds.
        "narrow-20cols" => {
            let mut s = Shell::new(20, 83, e2);
            s.mark("type");
            s.type_str(&prose(75));
            s.mark("idle");
            s.idle(400);
            s
        }
        // Return: the census follows the new line (a designed restart).
        "return-newline" => {
            let mut s = Shell::new(60, 83, e2);
            s.mark("type");
            s.type_str(&prose(20));
            s.mark("ret");
            s.ret();
            s.mark("type2");
            s.type_str(&prose(30));
            s.mark("idle");
            s.idle(400);
            s
        }
        // ^U of a wrapped line, then a retype.
        "ctrlu-wrapped-retype" => {
            let mut s = Shell::new(60, 83, e2);
            s.mark("type");
            s.type_str(&prose(80));
            s.mark("^U");
            s.line_kill();
            s.mark("retype");
            s.type_str(&prose(30));
            s.mark("idle");
            s.idle(400);
            s
        }
        // Backspace held (33 ms repeat) back through the fold, and retyped.
        "bs-held-across-wrap" => {
            let mut s = Shell::new(60, 83, e2);
            s.mark("type");
            s.type_str(&prose(70));
            s.mark("bs");
            s.key_ms = 33;
            for _ in 0..20 {
                s.backspace();
            }
            s.key_ms = 83;
            s.mark("retype");
            s.type_str("walking the fold again");
            s.mark("idle");
            s.idle(400);
            s
        }
        // The fold's shapes ([`Fold`]): scenarios above fold as bash readline
        // does (`<glyph> \r` on the margin key). The same typing across two
        // folds, and the same wrapped line revived after a rest, behind zsh's
        // zle and behind the kernel's cooked-mode echo (`cat`, `read`).
        "zle-wrap2-83ms-echo2" => {
            let mut s = Shell::new(60, 83, e2).folding(Fold::Zle);
            s.mark("type");
            s.type_str(&prose(140));
            s.mark("idle");
            s.idle(400);
            s
        }
        "kernel-wrap2-83ms-echo2" => {
            let mut s = Shell::new(60, 83, e2).folding(Fold::Kernel);
            s.mark("type");
            s.type_str(&prose(140));
            s.mark("idle");
            s.idle(400);
            s
        }
        "zle-rest-rebirth-wrapped" => {
            let mut s = Shell::new(60, 83, e2).folding(Fold::Zle);
            s.mark("type");
            s.type_str(&prose(80));
            s.mark("rest");
            s.idle(2_500);
            s.mark("type2");
            s.type_str(" and on it goes");
            s.mark("idle");
            s.idle(400);
            s
        }
        "kernel-rest-rebirth-wrapped" => {
            let mut s = Shell::new(60, 83, e2).folding(Fold::Kernel);
            s.mark("type");
            s.type_str(&prose(80));
            s.mark("rest");
            s.idle(2_500);
            s.mark("type2");
            s.type_str(" and on it goes");
            s.mark("idle");
            s.idle(400);
            s
        }
        // …and behind a kernel echo that lands a frame after its key (§43's
        // order: the revive's re-lay laid before the key's cell).
        "kernel-rest-rebirth-wrapped-echo20" => {
            let mut s = Shell::new(60, 83, Echo::After(20)).folding(Fold::Kernel);
            s.mark("type");
            s.type_str(&prose(80));
            s.mark("rest");
            s.idle(2_500);
            s.mark("type2");
            s.type_str(" and on it goes");
            s.mark("idle");
            s.idle(400);
            s
        }
        // A run no key saw standing: the hand rests right after a run's first
        // key (one key, or behind the kernel echo the fold's own glyph), so no
        // later key frame read the run's walk while it stood, and behind a
        // frame-late echo the revive's re-lay is laid before the key's cell.
        // At 60 columns behind `$ `, text 58 is the continuation row's column 0.
        "one-key-rest-echo20" => {
            let mut s = Shell::new(60, 83, Echo::After(20));
            s.mark("type");
            s.type_str("t");
            s.mark("rest");
            s.idle(2_500);
            s.mark("type2");
            s.type_str(" and on");
            s.mark("idle");
            s.idle(400);
            s
        }
        "fold-glyph-rest-echo20" => {
            let mut s = Shell::new(60, 83, Echo::After(20));
            s.mark("type");
            s.type_str(&prose(59));
            s.mark("rest");
            s.idle(2_500);
            s.mark("type2");
            s.type_str(" and on");
            s.mark("idle");
            s.idle(400);
            s
        }
        // A coalesced kernel-echo fold (three keys 3 ms apart across the margin,
        // each echoed 40 ms late, laying text 58 and 59 on the continuation row
        // in one frame), the hand resting right after it.
        "kernel-coalesced-fold-rest-echo2" | "kernel-coalesced-fold-rest-echo20" => {
            let echo = if name.ends_with("echo20") {
                Echo::After(20)
            } else {
                e2
            };
            let mut s = Shell::new(60, 83, echo).folding(Fold::Kernel);
            s.mark("type");
            let line = prose(60);
            let (a, b) = line.split_at(57);
            s.type_str(a);
            s.mark("burst");
            s.type_lagged(b, 3, 40);
            s.idle(40);
            s.mark("rest");
            s.idle(2_500);
            s.mark("type2");
            s.type_str(" and on");
            s.mark("idle");
            s.idle(400);
            s
        }
        other => panic!("no scenario {other}"),
    }
}

/// Whether scenario `name` names its own fold ([`Fold`]; `zle-…`, `kernel-…`):
/// read once; every other scenario is read behind each fold the harness models.
fn names_its_fold(name: &str) -> bool {
    name.starts_with("zle-") || name.starts_with("kernel-")
}

pub const SCENARIOS: [&str; 46] = [
    "paced-wrap2-83ms",
    "paced-bs-across-wrap-retype",
    "paced-ctrlw-retype",
    "paced-rest-rebirth",
    "lag60-40ms-across-wrap",
    "paced-lag60-40ms-across-wrap",
    "long-prompt-early-fold",
    "narrow-20cols",
    "return-newline",
    "ctrlu-wrapped-retype",
    "bs-held-across-wrap",
    "rest-rebirth-short",
    "rest-rebirth-wrapped",
    "rest-rebirth-twice",
    "rest-past-chain",
    "burst-25ms-across-wrap",
    "tab-rewrite",
    "wrap2-83ms-echo2",
    "wrap2-83ms-echo20",
    "wrap2-40ms-echo2",
    "bs5-retype",
    "bs-across-wrap-retype",
    "rest-bs-retype",
    "ctrlw-retype",
    "ctrlw-across-wrap-retype",
    "ctrlu-retype",
    "rest-rebirth",
    "rest-rebirth-echo20",
    "rest-mid-swoosh",
    "home-end-type",
    "home-end-wrapped-type",
    "home-type",
    "paste-burst",
    "tab-complete",
    "space-held-across-wrap",
    "left-end-type",
    "wrap2-83ms-echo0",
    "zle-wrap2-83ms-echo2",
    "kernel-wrap2-83ms-echo2",
    "zle-rest-rebirth-wrapped",
    "kernel-rest-rebirth-wrapped",
    "kernel-rest-rebirth-wrapped-echo20",
    "one-key-rest-echo20",
    "fold-glyph-rest-echo20",
    "kernel-coalesced-fold-rest-echo2",
    "kernel-coalesced-fold-rest-echo20",
];

/// The captured Claude Code takes the census replays.
pub const CAPTURES: [Capture; 5] = [
    // The wrap take with the hand resting 2.5 s before `however` (row 1)
    // and before ` in the other` (row 2, after the box grew).
    Capture {
        name: "claude-wrap-end-rest-row1",
        src: include_str!("fixtures/claude-wrap-2026-09-23-end.ptylog"),
        rows: 40,
        cols: 120,
        cw: 7,
        ch: 14,
        text_rows: (33, 37),
        indent: 2,
        pause: Some((62, 2_500)),
    },
    Capture {
        name: "claude-wrap-end-rest-row2",
        src: include_str!("fixtures/claude-wrap-2026-09-23-end.ptylog"),
        rows: 40,
        cols: 120,
        cw: 7,
        ch: 14,
        text_rows: (33, 37),
        indent: 2,
        pause: Some((125, 2_500)),
    },
    Capture {
        name: "claude-wrap-end",
        src: include_str!("fixtures/claude-wrap-2026-09-23-end.ptylog"),
        rows: 40,
        cols: 120,
        cw: 7,
        ch: 14,
        text_rows: (33, 37),
        indent: 2,
        pause: None,
    },
    Capture {
        name: "claude-composer-end",
        src: include_str!("fixtures/claude-composer-2026-09-21-end.ptylog"),
        rows: 40,
        cols: 120,
        cw: 7,
        ch: 14,
        text_rows: (33, 37),
        indent: 2,
        pause: None,
    },
    Capture {
        name: "claude-arrow-insert",
        src: include_str!("fixtures/claude-composer-2026-09-24-left.ptylog"),
        rows: 40,
        cols: 120,
        cw: 7,
        ch: 14,
        text_rows: (33, 37),
        indent: 2,
        pause: None,
    },
];

/// Scenarios the census reads a discontinuity in, each with where it is
/// reproduced and why. The census asserts only that each still reads its
/// discontinuity (removing it from this set by hand when it reads clean); every
/// other scenario must read clean.
///
/// `home-end-wrapped-type` is designed. The two coalesced kernel-echo fold
/// scenarios are open: the fold's first landing glyph (text 58, the
/// continuation row's column 0) reads a `Hole` for 12 frames (381..=392, about
/// 1.2 s into the rest, inside the swoosh), dark between lit text 57 and 59.
/// Their revive colours are clean (pinned by
/// `tests/revived_line_keeps_its_stops.rs`).
const KNOWN: [(&str, &str); 3] = [
    (
        "home-end-wrapped-type",
        "DESIGNED: Home across the fold is a hand jump that abandons the band \
         (not rejoinable), which drains while the hand types on after End; End \
         across the fold is a cross-row keyed Nav, which restarts the pace",
    ),
    (
        "kernel-coalesced-fold-rest-echo2",
        "OPEN (pre-existing on f2d7ab7df): a coalesced kernel-echo fold's first \
         landing glyph (text 58) drains dark 12 frames before its neighbours \
         inside the rest's swoosh — a Hole, frames 381..=392",
    ),
    (
        "kernel-coalesced-fold-rest-echo20",
        "OPEN (pre-existing on f2d7ab7df): the same Hole as \
         kernel-coalesced-fold-rest-echo2, frames 381..=392",
    ),
];

/// The census: every scenario and capture lays a lit band, each not in [`KNOWN`]
/// reads clean (no flagged run standing [`STANDING_FRAMES`] frames and no
/// recolour), and each in it still reads its discontinuity. A scenario that
/// does not name its fold ([`names_its_fold`]) is read behind every fold
/// ([`Fold::ALL`], verdicts labelled `<name>@<fold>`). `WALK_CENSUS=1` prints
/// every verdict; `WALK_CENSUS_ONLY=<name>` narrows the run;
/// `WALK_CENSUS_DUMP=<name>` prints the frames around each flagged run;
/// `WALK_CENSUS_FRAMES=<ms>:<ms>` (with `_ONLY`) prints every frame in the
/// window; `WALK_CENSUS_RING=<ms>:<ms>` prints the admission ring and the
/// ribbon's cohorts per frame in the window.
#[test]
fn census() {
    let print = std::env::var_os("WALK_CENSUS").is_some();
    let dump_of = std::env::var("WALK_CENSUS_DUMP").ok();
    let only = std::env::var("WALK_CENSUS_ONLY").ok();
    let window = std::env::var("WALK_CENSUS_FRAMES").ok().and_then(|w| {
        let (a, b) = w.split_once(':')?;
        Some((a.parse::<u64>().ok()?, b.parse::<u64>().ok()?))
    });
    for (name, _) in KNOWN {
        assert!(
            SCENARIOS.contains(&name) || CAPTURES.iter().any(|c| c.name == name),
            "KNOWN names {name}, which the census does not run"
        );
    }
    // Every replay the census reads, in its reading order: `(name, label,
    // what to run)`.
    enum Run<'a> {
        Scenario(Option<Fold>),
        Capture(&'a Capture),
    }
    let mut jobs: Vec<(&str, String, Run)> = Vec::new();
    for name in SCENARIOS {
        if only.as_deref().is_some_and(|o| o != name) {
            continue;
        }
        if names_its_fold(name) {
            jobs.push((name, name.to_string(), Run::Scenario(None)));
        } else {
            for fold in Fold::ALL {
                jobs.push((
                    name,
                    format!("{name}@{}", fold.name()),
                    Run::Scenario(Some(fold)),
                ));
            }
        }
    }
    for cap in &CAPTURES {
        if only.as_deref().is_some_and(|o| o != cap.name) {
            continue;
        }
        jobs.push((cap.name, cap.name.to_string(), Run::Capture(cap)));
    }
    // Each replay is its own shell or capture on its own `Terminal`: they
    // run on the binary's threads, each read where it ran — its snaps are
    // dropped there — and the verdicts are judged in the reading order
    // above (2026-09-30: the 125 replays, one after another, were this
    // binary's 20 s wall; on the pool, 3.9 s). `WALK_CENSUS_RING` prints
    // from inside a replay as it runs, unlabelled, so with it set the census
    // reads one replay at a time, as it always did.
    let read_one = |(name, label, run): &(&str, String, Run)| {
        let snaps = match run {
            Run::Scenario(None) => scenario(name).snaps,
            Run::Scenario(Some(fold)) => with_default_fold(*fold, || scenario(name)).snaps,
            Run::Capture(cap) => replay(cap),
        };
        let lit_frames = snaps
            .iter()
            .filter(|f| f.pos.iter().filter(|p| p.lit).count() >= 4)
            .count();
        let mut out = String::new();
        if print {
            out.push_str(&verdict(&snaps, label));
        }
        if let Some((a, b)) = window
            && only.as_deref() == Some(*name)
        {
            for (k, f) in snaps.iter().enumerate() {
                if f.ms >= a && f.ms <= b {
                    out.push_str(&dump(&snaps, k, k));
                }
            }
        }
        if dump_of.as_deref() == Some(*name) {
            for (_, _, a, b) in runs(&snaps) {
                out.push_str(&dump(&snaps, a.saturating_sub(2), (b + 1).min(a + 6)));
            }
        }
        (lit_frames, out, standing(&snaps), recolours(&snaps))
    };
    let read: Vec<_> = if std::env::var_os("WALK_CENSUS_RING").is_some() {
        jobs.iter().map(read_one).collect()
    } else {
        crate::common::par_map(&jobs, read_one)
    };
    let mut dirty = Vec::new();
    let mut gone_clean = Vec::new();
    for ((name, label, _), (lit_frames, out, standing, recoloured)) in jobs.iter().zip(read) {
        assert!(lit_frames > 10, "{label}: the census read a lit band");
        eprint!("{out}");
        let known = KNOWN.iter().any(|(k, _)| k == name);
        let clean = standing.is_empty() && recoloured.is_empty();
        if !known && !clean {
            dirty.push(format!(
                "{label}: {} standing runs (first {:?}), {} recolours (first {:?})",
                standing.len(),
                standing.first(),
                recoloured.len(),
                recoloured.first()
            ));
        }
        if known && clean {
            gone_clean.push(label.clone());
        }
    }
    assert!(
        dirty.is_empty(),
        "scenarios the census read clean now read a discontinuity:\n{}",
        dirty.join("\n")
    );
    assert!(
        gone_clean.is_empty(),
        "scenarios KNOWN for a discontinuity now read clean — remove each from KNOWN, \
         with its reason: {gone_clean:?}"
    );
}

/// Negative control: a synthetic snap with a backward step, a repeated stop, a
/// jump, a restarted fast leg and a hole is flagged for each, and the walk
/// itself for nothing.
#[test]
fn the_census_flags_each_discontinuity_and_not_the_walk() {
    let walk = |d: usize| {
        let d = d as f32;
        if d <= WALK_FAST_CELLS {
            d / WALK_FAST_CELLS
        } else {
            1.0 + (d - WALK_FAST_CELLS) * WALK_LAY_RATE
        }
    };
    let mk = |ts: Vec<Option<f32>>| {
        let mut s = Snap {
            ms: 0,
            label: String::new(),
            text: String::new(),
            start: 0,
            pos: ts
                .iter()
                .enumerate()
                .map(|(i, t)| Pos {
                    row: 0,
                    col: i as u16,
                    lit: t.is_some(),
                    t: *t,
                    live: true,
                })
                .collect(),
            breaks: Vec::new(),
            flags: Vec::new(),
        };
        s.judge();
        s
    };
    let clean = mk((0..40).map(|d| Some(walk(d))).collect());
    assert!(clean.flags.is_empty(), "the walk itself: {:?}", clean.flags);
    // A line whose first cells walk the lay rate did not start its walk.
    let late = mk((0..20).map(|d| Some(walk(d + 30))).collect());
    assert!(
        late.flags.iter().all(|(f, _)| *f == Flag::NoFastLeg) && late.flags.len() == 16,
        "a line with no fast leg: {:?}",
        late.flags
    );
    let mut ts: Vec<Option<f32>> = (0..40).map(|d| Some(walk(d))).collect();
    ts[5] = Some(walk(3));
    ts[10] = ts[9];
    ts[20] = Some(walk(20) + 0.2);
    ts[31] = Some(ts[30].expect("lit") + FAST);
    ts[35] = None;
    let bad = mk(ts);
    for f in [
        Flag::Back,
        Flag::Repeat,
        Flag::Jump,
        Flag::Restart,
        Flag::Hole,
    ] {
        assert!(
            bad.flags.iter().any(|(g, _)| *g == f),
            "{f:?} not flagged: {:?}",
            bad.flags
        );
    }
}

/// Negative control for [`recolours`]: a cell lit, dark and lit again in another
/// colour under the same text is read; with the text changed at the cell in
/// between (a retype) it is not; lit again in its own colour it is not.
#[test]
fn the_recolour_reading_sees_a_moved_stop_and_not_a_retype() {
    let snap = |text: &str, t: Option<f32>| Snap {
        ms: 0,
        label: String::new(),
        text: text.to_string(),
        start: 0,
        pos: vec![Pos {
            row: 0,
            col: 0,
            lit: t.is_some(),
            t,
            live: t.is_some(),
        }],
        breaks: Vec::new(),
        flags: Vec::new(),
    };
    let moved = [snap("a", Some(0.0)), snap("a", None), snap("a", Some(1.0))];
    assert_eq!(recolours(&moved).len(), 1, "a moved stop is read");
    let retyped = [snap("a", Some(0.0)), snap("", None), snap("a", Some(1.0))];
    assert!(recolours(&retyped).is_empty(), "a retype is not a recolour");
    let kept = [snap("a", Some(0.5)), snap("a", None), snap("a", Some(0.5))];
    assert!(recolours(&kept).is_empty(), "its own stop again is not");
}
