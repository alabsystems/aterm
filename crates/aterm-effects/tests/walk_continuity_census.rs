// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Walk continuity census.

use crate::common::Ground;
use crate::walk_census::*;
use aterm_effects::cursor_glow::InsertWidth;

const ON: &str = " and on it goes";

fn sh() -> Shell {
    Shell::new(60, 83, Echo::After(2))
}

fn sh20() -> Shell {
    Shell::new(60, 83, Echo::After(20))
}

fn paced() -> Shell {
    Shell::with(60, 83, Echo::After(2), "$ ", true)
}

fn typed(mut s: Shell, text: &str) -> Shell {
    s.mark("type");
    s.type_str(text);
    s.mark("idle");
    s.idle(400);
    s
}

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

fn rested(s: Shell, text: &str, ms: u64, then: &str) -> Shell {
    edited(s, text, "rest", |s| s.idle(ms), then)
}

fn bs(s: &mut Shell, n: usize) {
    for _ in 0..n {
        s.backspace();
    }
}

fn lagged(mut s: Shell) -> Shell {
    s.mark("type");
    s.type_lagged(&prose(90), 40, 60);
    s.mark("idle");
    s.idle(400);
    s
}

fn coalesced_fold(echo: u64) -> Shell {
    let mut s = Shell::new(60, 83, Echo::After(echo)).folding(Fold::Kernel);
    s.mark("type");
    let line = prose(60);
    let (a, b) = line.split_at(57);
    s.type_str(a);
    s.mark("burst");
    s.type_lagged(b, 3, 40);
    s.idle(40);
    s.mark("type2");
    s.type_str(" and on");
    s.mark("idle");
    s.idle(400);
    s
}

/// Scenarios the census runs; `zle-` and `kernel-` name their [`Fold`], while others run behind every fold.
#[expect(clippy::type_complexity, reason = "a test's case table")]
const SCENARIOS: &[(&str, fn() -> Shell)] = &[
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
    ("return-newline", || {
        edited(sh(), &prose(20), "ret", Shell::ret, &prose(30))
    }),
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
    ("one-key-rest-echo20", || {
        rested(sh20(), "t", 2_500, " and on")
    }),
    ("fold-glyph-rest-echo20", || {
        rested(sh20(), &prose(59), 2_500, " and on")
    }),
    ("kernel-coalesced-fold-continue-echo2", || coalesced_fold(2)),
    ("kernel-coalesced-fold-continue-echo20", || {
        coalesced_fold(20)
    }),
];

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
    // Each scenario and capture on its own dark ground, then on each light one.
    let mut grounds = vec![None];
    grounds.extend(crate::trail_host::light_grounds().into_iter().map(Some));
    let jobs: Vec<(String, &Run, Option<Ground>)> = jobs
        .iter()
        .flat_map(|(label, run)| {
            grounds.iter().map(move |&g| match g {
                None => (label.clone(), run, g),
                Some(on) => (format!("{label} on {}", on.name), run, g),
            })
        })
        .collect();
    let read = crate::common::par_map(&jobs, |&(_, run, ground)| {
        let take = || match run {
            Run::Scenario(build, None) => build().snaps,
            Run::Scenario(build, Some(fold)) => with_default_fold(*fold, build).snaps,
            Run::Capture(cap) => replay(cap),
        };
        let snaps = match ground {
            None => take(),
            Some(g) => crate::common::on_ground(g, take),
        };
        let lit_frames = snaps
            .iter()
            .filter(|f| f.pos.iter().filter(|p| p.lit).count() >= 4)
            .count();
        (lit_frames, standing(&snaps), recolours(&snaps))
    });
    let mut dirty = Vec::new();
    for ((label, _, _), (lit_frames, standing, recoloured)) in jobs.iter().zip(read) {
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
