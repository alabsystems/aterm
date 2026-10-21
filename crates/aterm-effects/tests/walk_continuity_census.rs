// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The walk's continuity census: the typed band's colour is smooth and continuous. A cell's
//! colour is `Cohort::t_at(col) = walk_zero() + walk_t(walk_d(col))`; along one input,
//! neighbouring lit cells step by exactly one walk step (`1/16` over the walk's first sixteen
//! cells, `WALK_LAY_RATE` after), and a wrap carries both colour and distance. The designed
//! restarts (Return, a line kill, a cross-row keyed Nav: colour kept, pace restarted) never put
//! two lit cells of one line side by side. So per presented frame ([`walk_census::Snap::judge`]):
//!
//! * every pair of text-adjacent lit cells steps forward by one walk step, never back
//!   (`Back`), never the same stop twice (`Repeat`), never more than one step or any other
//!   step (`Jump`);
//! * a `1/16` step never follows a `1/36` one to its left (`Restart`), and a line's first
//!   sixteen cells step `1/16` (`NoFastLeg`);
//! * no text cell between two lit ones is dark (`Hole`);
//!
//! and across frames ([`walk_census::recolours`]) no text cell is lit again in another colour
//! while the text up to it stayed the same.
//!
//! A flag standing [`STANDING_FRAMES`] frames or more is a defect; fewer, a
//! transient. The host is the GUI's frame seam (`tests/claude_wrap_band.rs`'s
//! lock A) on a real `Terminal`: a line editor on the primary screen
//! ([`walk_census::Shell`], keys stamped as `app_input.rs` stamps them) on a
//! flat 16 ms train or the GUI's pacing, plus captured Claude Code composer
//! takes replayed byte for byte. The shell folds as bash readline does (`<glyph>
//! \r` on the key that fills the margin); scenarios that do not name a fold are
//! read behind every [`Fold`] (readline, zsh zle, and the kernel's cooked-mode
//! echo used by `cat` and `read`).

use crate::walk_census::*;
use aterm_effects::cursor_glow::InsertWidth;

/// What a scenario types after a rest.
const ON: &str = " and on it goes";

/// A readline shell at 60 columns, keys 83 ms apart, each echoed 2 ms later.
fn sh() -> Shell {
    Shell::new(60, 83, Echo::After(2))
}

/// The same shell echoing each key 20 ms (a frame) after it.
fn sh20() -> Shell {
    Shell::new(60, 83, Echo::After(20))
}

/// The same shell framed at the GUI's real pacing.
fn paced() -> Shell {
    Shell::with(60, 83, Echo::After(2), "$ ", true)
}

/// `text` typed, then the rest out.
fn typed(mut s: Shell, text: &str) -> Shell {
    s.mark("type");
    s.type_str(text);
    s.mark("idle");
    s.idle(400);
    s
}

/// `text` typed, `edit` (labelled `label`), `then` typed, then the rest out.
fn edited(
    mut s: Shell,
    text: &str,
    label: &str,
    edit: impl FnOnce(&mut Shell),
    then: &str,
) -> Shell {
    s.mark("type");
    s.type_str(text);
    s.mark(label);
    edit(&mut s);
    s.mark("type2");
    s.type_str(then);
    s.mark("idle");
    s.idle(400);
    s
}

/// `text` typed, a rest of `ms`, then `then` typed.
fn rested(s: Shell, text: &str, ms: u64, then: &str) -> Shell {
    edited(s, text, "rest", |s| s.idle(ms), then)
}

/// `n` Backspaces.
fn bs(s: &mut Shell, n: usize) {
    for _ in 0..n {
        s.backspace();
    }
}

/// A lagging shell: echoes 60 ms behind 40 ms keys, across the fold.
fn lagged(mut s: Shell) -> Shell {
    s.mark("type");
    s.type_lagged(&prose(90), 40, 60);
    s.mark("idle");
    s.idle(400);
    s
}

/// Every scenario the census runs. A name starting `zle-` or `kernel-` names
/// its fold ([`Fold`]) and is read once; every other is read behind each fold.
#[expect(clippy::type_complexity, reason = "a test's case table")]
const SCENARIOS: &[(&str, fn() -> Shell)] = &[
    // Plain typing across two wraps, at 12 cps behind three echo lags, and
    // at 25 cps.
    ("wrap2-83ms-echo2", || typed(sh(), &prose(140))),
    ("wrap2-83ms-echo0", || {
        typed(Shell::new(60, 83, Echo::After(0)), &prose(140))
    }),
    ("wrap2-83ms-echo20", || typed(sh20(), &prose(140))),
    ("wrap2-40ms-echo2", || {
        typed(Shell::new(60, 40, Echo::After(2)), &prose(140))
    }),
    ("burst-25ms-across-wrap", || {
        typed(Shell::new(60, 25, Echo::After(2)), &prose(90))
    }),
    // Backspace then retype: mid-row, back up through a fold, after a rest
    // inside the grace, and held (33 ms repeat) back through the fold.
    ("bs5-retype", || {
        edited(sh(), &prose(30), "bs", |s| bs(s, 5), "abcdefgh")
    }),
    ("bs-across-wrap-retype", || {
        edited(
            sh(),
            &prose(70),
            "bs",
            |s| bs(s, 16),
            "walking the fold again",
        )
    }),
    ("rest-bs-retype", || {
        edited(
            sh(),
            &prose(30),
            "rest-bs",
            |s| {
                s.idle(600);
                bs(s, 5);
            },
            "abcdefgh",
        )
    }),
    ("bs-held-across-wrap", || {
        edited(
            sh(),
            &prose(70),
            "bs",
            |s| {
                s.key_ms = 33;
                bs(s, 20);
                s.key_ms = 83;
            },
            "walking the fold again",
        )
    }),
    // ^W and ^U then retype (^U is a new line: the pace restarts by design).
    ("ctrlw-retype", || {
        edited(
            sh(),
            "the rainbow walks along every",
            "^W",
            Shell::word_kill,
            "kitty types",
        )
    }),
    ("ctrlw-across-wrap-retype", || {
        edited(
            sh(),
            &format!("{} crossing", prose(55)),
            "^W",
            Shell::word_kill,
            "walking on",
        )
    }),
    ("ctrlu-retype", || {
        edited(sh(), &prose(40), "^U", Shell::line_kill, &prose(30))
    }),
    ("ctrlu-wrapped-retype", || {
        edited(sh(), &prose(80), "^U", Shell::line_kill, &prose(30))
    }),
    // Rests: past the swoosh (the rebirth, `last_walk`), inside it
    // (retracting), twice, and past the chain window (a cold start by design).
    ("rest-rebirth", || rested(sh(), &prose(30), 2_500, ON)),
    ("rest-rebirth-echo20", || {
        rested(sh20(), &prose(30), 2_500, ON)
    }),
    ("rest-mid-swoosh", || rested(sh(), &prose(30), 1_300, ON)),
    ("rest-rebirth-short", || {
        rested(sh(), "hello", 2_500, " world")
    }),
    ("rest-rebirth-wrapped", || {
        rested(sh(), &prose(80), 2_500, ON)
    }),
    ("rest-rebirth-twice", || {
        edited(
            sh(),
            "the rainbow",
            "rest",
            |s| {
                s.idle(2_500);
                s.type_str(" walks");
                s.idle(2_500);
            },
            " along",
        )
    }),
    ("rest-past-chain", || rested(sh(), &prose(30), 5_600, ON)),
    // Navigation: Home, a rest, End; Home then an insert pushing the whole
    // line; Left x3 then End.
    ("home-end-type", || {
        edited(
            sh(),
            &prose(30),
            "home",
            |s| {
                s.home();
                s.idle(300);
                s.end();
            },
            " and on",
        )
    }),
    ("home-type", || {
        edited(sh(), &prose(30), "home", Shell::home, "XYZ ")
    }),
    ("left-end-type", || {
        edited(
            sh(),
            &prose(30),
            "left",
            |s| {
                for _ in 0..3 {
                    s.left();
                }
                s.end();
            },
            " on",
        )
    }),
    // A paste burst, a Tab completing the word, a Tab whose completion
    // REWRITES the word (a case-folding match: the shell reprints from the
    // word's start), a Space held across the fold (auto-repeat at 33 ms).
    ("paste-burst", || {
        edited(
            sh(),
            "echo the ",
            "paste",
            |s| s.paste("rainbow walks"),
            " along every line",
        )
    }),
    ("tab-complete", || {
        edited(
            sh(),
            "git sta",
            "tab",
            |s| s.tab("tus "),
            "--short --branch",
        )
    }),
    ("tab-rewrite", || {
        edited(
            sh(),
            "ls ~/doc",
            "tab",
            |s| {
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
            },
            " -la",
        )
    }),
    ("space-held-across-wrap", || {
        edited(
            sh(),
            &prose(54),
            "hold",
            |s| {
                for _ in 0..8 {
                    s.key_at(' ', 33);
                }
            },
            "after the held space",
        )
    }),
    // Return: the census follows the new line (a designed restart).
    ("return-newline", || {
        edited(sh(), &prose(20), "ret", Shell::ret, &prose(30))
    }),
    // The GUI's real pacing, the same takes.
    ("paced-wrap2-83ms", || typed(paced(), &prose(140))),
    ("paced-bs-across-wrap-retype", || {
        edited(
            paced(),
            &prose(70),
            "bs",
            |s| bs(s, 16),
            "walking the fold again",
        )
    }),
    ("paced-ctrlw-retype", || {
        edited(
            paced(),
            "the rainbow walks along every",
            "^W",
            Shell::word_kill,
            "kitty types",
        )
    }),
    ("paced-rest-rebirth", || {
        rested(paced(), &prose(30), 2_500, ON)
    }),
    ("lag60-40ms-across-wrap", || lagged(sh())),
    ("paced-lag60-40ms-across-wrap", || lagged(paced())),
    // A long prompt (the line folds inside its fast leg), a 20-column pane
    // (four folds).
    ("long-prompt-early-fold", || {
        let prompt = format!("{}$ ", "~".repeat(48));
        typed(
            Shell::with(60, 83, Echo::After(2), &prompt, false),
            &prose(40),
        )
    }),
    ("narrow-20cols", || {
        typed(Shell::new(20, 83, Echo::After(2)), &prose(75))
    }),
    // The same typing across two folds, and the same wrapped line revived
    // after a rest, behind zsh's zle and behind the kernel's cooked-mode echo
    // (`cat`, `read`); the last behind a kernel echo that lands a frame after
    // its key (§43's order: the revive's re-lay laid before the key's cell).
    ("zle-wrap2-83ms-echo2", || {
        typed(sh().folding(Fold::Zle), &prose(140))
    }),
    ("kernel-wrap2-83ms-echo2", || {
        typed(sh().folding(Fold::Kernel), &prose(140))
    }),
    ("zle-rest-rebirth-wrapped", || {
        rested(sh().folding(Fold::Zle), &prose(80), 2_500, ON)
    }),
    ("kernel-rest-rebirth-wrapped", || {
        rested(sh().folding(Fold::Kernel), &prose(80), 2_500, ON)
    }),
    ("kernel-rest-rebirth-wrapped-echo20", || {
        rested(sh20().folding(Fold::Kernel), &prose(80), 2_500, ON)
    }),
    // A run no key saw standing: the hand rests right after a run's first key
    // (one key, or behind the kernel echo the fold's own glyph), so no later
    // key frame read the run's walk while it stood, and behind a frame-late
    // echo the revive's re-lay is laid before the key's cell. At 60 columns
    // behind `$ `, text 58 is the continuation row's column 0.
    ("one-key-rest-echo20", || {
        rested(sh20(), "t", 2_500, " and on")
    }),
    ("fold-glyph-rest-echo20", || {
        rested(sh20(), &prose(59), 2_500, " and on")
    }),
];

/// A captured Claude Code take at its recording's grid (`resize 40 120`,
/// 7×14 px cells, the composer's text on rows 33..=37 behind a two-cell
/// indent), with a rest inserted before the `k`-th printable key.
const fn claude(name: &'static str, src: &'static str, pause: Option<(usize, u64)>) -> Capture {
    Capture {
        name,
        src,
        rows: 40,
        cols: 120,
        cw: 7,
        ch: 14,
        text_rows: (33, 37),
        indent: 2,
        pause,
    }
}

const WRAP_END: &str = include_str!("fixtures/claude-wrap-2026-09-23-end.ptylog");

/// The captured Claude Code takes the census replays: the wrap take with the
/// hand resting 2.5 s before `however` (row 1) and before ` in the other`
/// (row 2, after the box grew), and as recorded; the composer's end take; an
/// arrow-key insert.
const CAPTURES: [Capture; 5] = [
    claude("claude-wrap-end-rest-row1", WRAP_END, Some((62, 2_500))),
    claude("claude-wrap-end-rest-row2", WRAP_END, Some((125, 2_500))),
    claude("claude-wrap-end", WRAP_END, None),
    claude(
        "claude-composer-end",
        include_str!("fixtures/claude-composer-2026-09-21-end.ptylog"),
        None,
    ),
    claude(
        "claude-arrow-insert",
        include_str!("fixtures/claude-composer-2026-09-24-left.ptylog"),
        None,
    ),
];

/// The census: every scenario and capture lays a lit band and reads clean (no
/// flagged run standing [`STANDING_FRAMES`] frames and no recolour). A
/// scenario that does not name its fold is read behind every fold
/// ([`Fold::ALL`], verdicts labelled `<name>@<fold>`).
#[test]
fn census() {
    enum Run<'a> {
        Scenario(fn() -> Shell, Option<Fold>),
        Capture(&'a Capture),
    }
    let mut jobs: Vec<(String, Run)> = Vec::new();
    for &(name, build) in SCENARIOS {
        if name.starts_with("zle-") || name.starts_with("kernel-") {
            jobs.push((name.to_string(), Run::Scenario(build, None)));
        } else {
            for fold in Fold::ALL {
                jobs.push((
                    format!("{name}@{}", fold.name()),
                    Run::Scenario(build, Some(fold)),
                ));
            }
        }
    }
    for cap in &CAPTURES {
        jobs.push((cap.name.to_string(), Run::Capture(cap)));
    }
    // Each replay is its own shell or capture on its own `Terminal`: they run
    // on the binary's threads, each read where it ran, and are judged in the
    // order above.
    let read = crate::common::par_map(&jobs, |(_, run)| {
        let snaps = match run {
            Run::Scenario(build, None) => build().snaps,
            Run::Scenario(build, Some(fold)) => with_default_fold(*fold, build).snaps,
            Run::Capture(cap) => replay(cap),
        };
        let lit_frames = snaps
            .iter()
            .filter(|f| f.pos.iter().filter(|p| p.lit).count() >= 4)
            .count();
        (lit_frames, standing(&snaps), recolours(&snaps))
    });
    let mut dirty = Vec::new();
    for ((label, _), (lit_frames, standing, recoloured)) in jobs.iter().zip(read) {
        assert!(lit_frames > 10, "{label}: the census read a lit band");
        if !standing.is_empty() || !recoloured.is_empty() {
            dirty.push(format!(
                "{label}: {} standing runs (first {:?}), {} recolours (first {:?})",
                standing.len(),
                standing.first(),
                recoloured.len(),
                recoloured.first()
            ));
        }
    }
    assert!(
        dirty.is_empty(),
        "the census read a discontinuity:\n{}",
        dirty.join("\n")
    );
}
