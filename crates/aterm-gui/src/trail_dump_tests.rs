// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `trail_dump`'s script and printer (`examples/support/trail_script.rs`) on the crate's own rig
//! ([`crate::trail_rig`]): the libtest pass runs no example, so its tests live here.

mod rig {
    pub(crate) use crate::trail_rig::{TrailFrame, TrailRig};
}

#[path = "../examples/support/trail_script.rs"]
mod trail_script;

use trail_script::{
    EXIT_USAGE, Event, Live, Motion, Page, Ramp, Setup, Stmt, dump, dump_main, parse, statement,
};

/// A short script that types two keys with their echo once the grid's resize hush has passed,
/// submits, moves the pointer and rests.
const TYPED: &str = "\
grid 12 40
style rainbow kitty
pet on 7
hz 60
at 0 pty \"$ \"
at 700 key a
at 702 pty 61
at 800 key b
at 802 pty 62
at 900 key enter
at 902 pty \"\\r\\nhi\\r\\n$ \"
at 960 pointer 30 30
run 1200
";

fn one(code: &str) -> Stmt {
    statement(code).unwrap_or_else(|error| panic!("{code}: {error}"))
}

fn dumped(text: &str) -> String {
    dumped_with(text, trail_script::Detail::default())
}

fn dumped_with(text: &str, detail: trail_script::Detail) -> String {
    let script = parse(text).expect("the script parses");
    let mut out = Vec::new();
    dump("typed", &script, detail, &mut out).expect("the dump runs");
    String::from_utf8(out).expect("the dump is text")
}

/// The light word `name` a `--full --frame` dump printed (`  W light <name> <hex>`).
fn light_word(text: &str, name: &str) -> Option<u64> {
    text.lines().find_map(|line| {
        let (word, hex) = line.strip_prefix("  W light ")?.split_once(' ')?;
        (word == name).then(|| u64::from_str_radix(hex, 16).expect("a word is hex"))
    })
}

#[test]
fn the_streams_line_names_every_stream_an_arm_may_declare() {
    let dump = dumped(TYPED);
    let line = dump
        .lines()
        .find(|l| l.starts_with("  streams "))
        .expect("a drawn frame prints its streams");
    let printed: Vec<&str> = line
        .split(' ')
        .skip(3)
        .map(|w| w.split_once('=').map_or(w, |(name, _)| name))
        .collect();
    let named: Vec<&str> = aterm_effects::state_words::Streams::NAMED
        .into_iter()
        .map(|(_, name)| name)
        // Phase 3 adds the stream; its bit is declared now.
        .filter(|&name| name != "cursor_ink")
        .collect();
    assert_eq!(printed, named, "in the bitset's order:\n{line}");
    let deadline = dump
        .lines()
        .find(|l| l.starts_with("  deadline "))
        .expect("a drawn frame prints its deadlines");
    let parts: Vec<&str> = deadline
        .split(' ')
        .skip(3)
        .map(|w| w.split_once('=').map_or(w, |(name, _)| name))
        .collect();
    assert_eq!(parts, ["glow", "pet", "words"], "{deadline}");
}

#[test]
fn the_dump_prints_the_same_words_twice() {
    let first = dumped(TYPED);
    let second = dumped(TYPED);
    let frames = first.lines().filter(|l| l.starts_with("F ")).count();
    assert_eq!(
        frames, 37,
        "a frame every 33.3 ms present interval:\n{first}"
    );
    assert!(
        first
            .lines()
            .any(|l| l.starts_with("  sounds ") && l != "  sounds -"),
        "the typed keys push sound cues:\n{first}"
    );
    assert_eq!(first, second, "two runs of one script print the same words");
}

/// The pet at rest beside a typed word, the pointer passing over the grid, then (when `press` is
/// given) a left click at that window pixel at 4000 ms, and a rest after it.
fn pet_script(press: Option<(f64, f64)>) -> String {
    let mut text = String::from(
        "grid 12 40\nstyle rainbow kitty\npet on 7\nhz 60\nat 0 pty \"\\e[6;1H$ \"\n\
         at 700 key c\nat 702 pty \"c\"\nat 810 key a\nat 812 pty \"a\"\n\
         at 920 key t\nat 922 pty \"t\"\nat 3000 pointer 20 20\nat 3100 pointer 200 60\n\
         at 3200 pointer off\nrun 4000\n",
    );
    if let Some((x, y)) = press {
        text.push_str(&format!("at 4000 press {x} {y}\n"));
    }
    text.push_str("run 4600\n");
    text
}

/// [`pet_script`] with its press on the cat: aimed at the centre of the pet's hit rect on the
/// last frame by 4000 ms of the script without it, which the press script draws the same.
fn pet_press_script() -> String {
    let mut rest = String::new();
    let seen = crate::trail_rig::pet_seen_for_test(|| rest = dumped(&pet_script(None)));
    let by_press = std::time::Duration::from_millis(4000);
    let (_, cat) = seen
        .iter()
        .rev()
        .find(|(t, _)| *t <= by_press)
        .expect("frames by the press");
    let cat = cat.expect("the cat is drawn at rest by the press");
    let pressed = pet_script(Some(cat));
    assert_ne!(
        dumped(&pressed),
        rest,
        "a press on the cat pets it, so its words move"
    );
    pressed
}

#[test]
fn a_press_on_the_cat_changes_the_dump() {
    // The stroke is stamped at the press's own instant, never at the wall clock, which a dump
    // runs far ahead of: an older stamp falls outside the pet's latch and the stroke is dropped.
    let _ = pet_press_script();
}

#[test]
fn a_script_slowed_to_the_wall_clock_dumps_the_same_words() {
    // The pet's press, a split's focus move and present costs over budget: the paths whose
    // clocks the review found or the live present adds.
    for script in [
        pet_press_script(),
        SPLIT.to_string(),
        shed_script("auto", 50_000_000),
    ] {
        let fast = dumped(&script);
        let slow = crate::trail_rig::pace_for_test(|| dumped(&script));
        assert_eq!(
            fast, slow,
            "a script run at the wall clock's pace prints what it prints run flat out: no host \
             read on its path reads the wall clock:\n{script}"
        );
    }
}

/// The pet's and the companion's state words on each frame of `dump` before `until_ms`.
fn pet_words_before(dump: &str, until_ms: f64) -> Vec<String> {
    let mut out = Vec::new();
    for frame in dump.split("\nF ").skip(1) {
        let t: f64 = frame
            .split_once(" t=")
            .and_then(|(_, rest)| rest.split(' ').next())
            .and_then(|t| t.parse().ok())
            .expect("a frame's time");
        if t >= until_ms {
            break;
        }
        let Some(state) = frame.lines().find(|l| l.starts_with("  state ")) else {
            continue;
        };
        let pet: Vec<&str> = state
            .split(' ')
            .filter(|w| w.starts_with("pet=") || w.starts_with("companion="))
            .collect();
        out.push(pet.join(" "));
    }
    out
}

#[test]
fn the_pet_sweep_s_pointer_moves_its_pet() {
    // The live present ticks the pet with the pointer, as the event loop's redraw does: the
    // windowless capture path ticked it blind and still, so the corpus's sweep moved nothing.
    let text =
        std::fs::read_to_string(format!("{CORPUS}/dark/pet-sweep.trail")).expect("read the script");
    let blind: String = text
        .lines()
        .filter(|l| !l.contains(" pointer "))
        .map(|l| format!("{l}\n"))
        .collect();
    assert_ne!(blind, text, "the script sweeps the pointer");
    // Its first press lands at 5887 ms.
    let swept = pet_words_before(&dumped(&text), 5887.0);
    let unswept = pet_words_before(&dumped(&blind), 5887.0);
    assert_eq!(swept.len(), unswept.len(), "the same frames");
    assert_ne!(
        swept, unswept,
        "the pointer's sweep moves the pet's words before the press"
    );
}

#[test]
fn a_held_frame_prints_main_s_state() {
    let held = dumped(
        "grid 6 40\nstyle rainbow kitty\nhz 60\nat 0 pty \"$ \"\nat 300 pty \"\\e[?2026h\"\n\
         at 600 pty \"\\e[?2026l\"\nrun 900\n",
    );
    let lines: Vec<&str> = held.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.starts_with("F ") && l.ends_with(" held"))
        .unwrap_or_else(|| panic!("a synchronized update holds a frame:\n{held}"));
    assert!(
        lines[at + 1].starts_with("  state n="),
        "a held frame prints its state words next:\n{held}"
    );
    let drawn = lines[..at]
        .iter()
        .rfind(|l| l.starts_with("  light "))
        .expect("a frame drew before the hold");
    assert_eq!(
        lines[at + 2],
        *drawn,
        "the held frame's light words are the last drawn frame's:\n{held}"
    );
    assert!(
        drawn
            .strip_prefix("  light n=6 latch=")
            .is_some_and(|hash| hash.len() == 16),
        "on main's dark ground the light words are the window's latch alone:\n{held}"
    );
}

/// The ink ledger's per-frame products (`docs/design/PRISM-GLASS.md` §15.9's preamble) print on a
/// drawn frame's `ledger` line, right after its `state` line, and a held frame, which draws
/// nothing, prints none; under `--full` its words print as `W ledger` lines and none of them as a
/// `W state` line.
#[test]
fn a_drawn_frame_prints_its_ledger_and_a_held_frame_none() {
    let script = "grid 6 40\nstyle rainbow kitty\nhz 60\nat 0 pty \"$ \"\nat 100 key a\n\
                  at 102 pty \"a\"\nat 300 pty \"\\e[?2026h\"\nat 600 pty \"\\e[?2026l\"\nrun 900\n";
    let text = dumped(script);
    let lines: Vec<&str> = text.lines().collect();
    let starts: Vec<usize> = (0..lines.len())
        .filter(|&i| lines[i].starts_with("F "))
        .collect();
    let (mut drawn, mut held) = (0, 0);
    for (k, &at) in starts.iter().enumerate() {
        let end = starts.get(k + 1).copied().unwrap_or(lines.len());
        let block = &lines[at..end];
        let ledgers: Vec<usize> = (0..block.len())
            .filter(|&i| block[i].starts_with("  ledger n="))
            .collect();
        if block[0].ends_with(" held") {
            held += 1;
            assert!(ledgers.is_empty(), "a held frame prints no ledger:\n{text}");
        } else {
            drawn += 1;
            assert!(
                matches!(ledgers[..], [i] if block[i - 1].starts_with("  state n=")),
                "a drawn frame prints one ledger line after its state:\n{}",
                block.join("\n")
            );
        }
    }
    assert!(
        drawn > 0 && held > 0,
        "fixture: frames drawn and held:\n{text}"
    );
    let frame: u64 = lines
        .iter()
        .rfind(|l| l.starts_with("F ") && !l.ends_with(" held"))
        .and_then(|l| l.split(' ').nth(1)?.parse().ok())
        .expect("the last drawn frame's number");
    let full = dumped_with(
        script,
        trail_script::Detail {
            full_frame: Some(frame),
        },
    );
    let words: Vec<&str> = full
        .lines()
        .filter_map(|l| l.strip_prefix("  W ledger "))
        .collect();
    assert!(
        !words.is_empty() && words.iter().all(|w| w.starts_with("ink.")),
        "the ledger is the ink ledger's: {words:?}"
    );
    for product in ["ink.rails", "ink.rail_ramps", "ink.dims", "ink.notes"] {
        assert!(
            !full.lines().any(|l| l
                .strip_prefix("  W state ")
                .is_some_and(|w| w.starts_with(product))),
            "{product} is no state word:\n{full}"
        );
    }
}

/// `live <level>` previews its level on a frame the latch admits, a light configured scheme over
/// a light page, and on no other: a dark scheme's frames stay `light=0` at every level.
#[test]
fn live_lights_only_what_the_latch_admits() {
    let lit = |theme: &str, live: &str| {
        let text = dumped_with(
            &format!(
                "grid 6 40\nstyle rainbow kitty\nhz 60\ntheme {theme}\n{live}at 0 pty \"$ \"\nrun 50\n"
            ),
            trail_script::Detail {
                full_frame: Some(0),
            },
        );
        let frames: Vec<&str> = text.lines().filter(|l| l.starts_with("F ")).collect();
        assert!(!frames.is_empty(), "{text}");
        let lights: std::collections::BTreeSet<String> = frames
            .iter()
            .filter_map(|l| l.split(' ').find(|w| w.starts_with("light=")))
            .map(str::to_string)
            .collect();
        let latch = (
            light_word(&text, "latch.on").expect("frame 0's latch"),
            light_word(&text, "latch.live").expect("frame 0's level"),
        );
        (lights, latch)
    };
    let set = |words: &[&str]| words.iter().map(|w| (*w).to_string()).collect();
    let paper = "#2b2d33 #f6f7f9";
    let dark = "#d0d0d0 #111318";
    assert_eq!(lit(paper, ""), (set(&["light=1"]), (1, 0)));
    assert_eq!(lit(paper, "live glass\n"), (set(&["light=3"]), (1, 1)));
    assert_eq!(lit(paper, "live vanish\n"), (set(&["light=31"]), (1, 4)));
    assert_eq!(
        lit(dark, "live vanish\n"),
        (set(&["light=0"]), (0, 4)),
        "a dark scheme is never lit"
    );
}

/// Keys typed in a split's right pane, then in its left one once the focus moves there.
const SPLIT: &str = r#"grid 8 60
style rainbow kitty
hz 60
panes 2
focus-pane 1
at 0 pty "\e[5;1H$ "
at 300 key a
at 302 pty "a"
run 700
focus-pane 0
at 700 pty "\e[5;1H$ "
at 1000 key b
at 1002 pty "b"
run 1500
"#;

#[test]
fn a_pane_focus_move_dumps_the_same_words_twice() {
    // The replaced front curtains the trail at the host clock, which the rig pins.
    assert_eq!(
        dumped(SPLIT),
        dumped(SPLIT),
        "a focus move between panes reads no clock the rig does not drive"
    );
}

/// Each frame's `px` line of `text`'s dump.
fn pixels(text: &str) -> Vec<String> {
    dumped(text)
        .lines()
        .filter(|line| line.starts_with("  px "))
        .map(str::to_string)
        .collect()
}

/// `page wallpaper` lays the rig's picture under the page (§15.1 P0.6's wallpaper page): its
/// frames' pixels leave the opaque page's, which shows none (the negative control).
#[test]
fn a_wallpaper_page_lays_its_picture() {
    let page = |page: &str| {
        pixels(&format!(
            "grid 4 20\nstyle rainbow kitty\nhz 60\npage {page}\nat 0 pty \"$ \"\nrun 40\n"
        ))
    };
    let (wallpaper, opaque) = (page("wallpaper"), page("opaque"));
    assert!(!wallpaper.is_empty() && wallpaper.len() == opaque.len());
    assert!(
        wallpaper.iter().zip(&opaque).all(|(w, o)| w != o),
        "the wallpaper shows on every frame"
    );
}

/// `pane <i> theme <fg> <bg>` dresses pane `i` in its own theme (§15.1 P0.6's mixed split): a
/// split whose first pane wears a dark theme draws other pixels than the same split with none
/// (the negative control).
#[test]
fn a_pane_theme_dresses_its_pane() {
    let split = |theme: &str| {
        pixels(&format!(
            "grid 6 40\nstyle rainbow kitty\nhz 60\npanes 2\n{theme}focus-pane 1\n\
             at 0 pty \"$ \"\nrun 40\n"
        ))
    };
    let (dressed, plain) = (split("pane 0 theme #c0caf5 #1a1b26\n"), split(""));
    assert!(!dressed.is_empty() && dressed.len() == plain.len());
    assert!(
        dressed.iter().zip(&plain).all(|(d, p)| d != p),
        "the dark pane shows on every frame"
    );
}

#[test]
fn an_unknown_directive_exits_2() {
    let dir = crate::test_scratch::short_dir("trail-dump");
    let path = dir.path().join("bad.trail");
    std::fs::write(&path, "grid 4 10\nsparkle 3\nrun 10\n").expect("write the script");
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = dump_main(&[path.to_string_lossy().into_owned()], &mut out, &mut err);
    assert_eq!(code, EXIT_USAGE);
    let err = String::from_utf8_lossy(&err);
    assert!(err.contains("line 2") && err.contains("sparkle"), "{err}");
    assert!(out.is_empty(), "nothing runs");
}

#[test]
fn grid_parses() {
    assert_eq!(
        one("grid 20 100"),
        Stmt::Setup(Setup::Grid {
            rows: 20,
            cols: 100
        })
    );
    assert!(statement("grid 0 100").is_err());
}

#[test]
fn font_parses() {
    assert_eq!(one("font 17.5"), Stmt::Setup(Setup::Font(17.5)));
}

#[test]
fn style_parses() {
    assert_eq!(
        one("style rainbow   kitty"),
        Stmt::Setup(Setup::Style("rainbow kitty".into()))
    );
}

#[test]
fn pet_parses() {
    assert_eq!(one("pet on 77"), Stmt::Setup(Setup::Pet(Some(77))));
    assert_eq!(one("pet off"), Stmt::Setup(Setup::Pet(None)));
    assert!(statement("pet on").is_err());
}

#[test]
fn words_parses() {
    assert_eq!(
        one("words candy"),
        Stmt::Setup(Setup::Words(Some("candy".into())))
    );
    assert_eq!(one("words off"), Stmt::Setup(Setup::Words(None)));
}

#[test]
fn theme_parses() {
    assert_eq!(
        one("theme #102030 ffffff"),
        Stmt::Setup(Setup::Theme {
            fg: 0x10_2030,
            bg: 0xff_ffff
        })
    );
    assert!(statement("theme #12345 #ffffff").is_err());
}

#[test]
fn panes_parses() {
    assert_eq!(one("panes 2"), Stmt::Setup(Setup::Panes(2)));
    assert!(statement("panes 0").is_err());
}

#[test]
fn pane_theme_parses() {
    assert_eq!(
        one("pane 1 theme #000000 #fafafa"),
        Stmt::Setup(Setup::PaneTheme {
            pane: 1,
            fg: 0,
            bg: 0xfa_fafa
        })
    );
}

#[test]
fn focus_pane_parses() {
    assert_eq!(one("focus-pane 1"), Stmt::Setup(Setup::FocusPane(1)));
}

#[test]
fn hz_parses() {
    assert_eq!(one("hz 120"), Stmt::Setup(Setup::Hz(120)));
    assert!(statement("hz 0").is_err());
}

#[test]
fn ramp_parses() {
    assert_eq!(one("ramp never"), Stmt::Setup(Setup::Ramp(Ramp::Never)));
    assert_eq!(one("ramp always"), Stmt::Setup(Setup::Ramp(Ramp::Always)));
}

#[test]
fn page_parses() {
    assert_eq!(one("page opaque"), Stmt::Setup(Setup::Page(Page::Opaque)));
    assert_eq!(
        one("page translucent"),
        Stmt::Setup(Setup::Page(Page::Translucent))
    );
    assert_eq!(
        one("page wallpaper"),
        Stmt::Setup(Setup::Page(Page::Wallpaper))
    );
}

#[test]
fn intensity_parses() {
    assert_eq!(one("intensity 0.5"), Stmt::Setup(Setup::Intensity(0.5)));
    assert!(statement("intensity NaN").is_err());
}

#[test]
fn live_parses() {
    for (word, live) in [
        ("off", Live::Off),
        ("glass", Live::Glass),
        ("lip", Live::Lip),
        ("marks", Live::Marks),
        ("vanish", Live::Vanish),
    ] {
        assert_eq!(one(&format!("live {word}")), Stmt::Setup(Setup::Live(live)));
    }
}

#[test]
fn at_pty_parses() {
    assert_eq!(
        one("at 10 pty 1b5b41"),
        Stmt::At {
            ms: 10,
            event: Event::Pty(vec![0x1b, b'[', b'A'])
        }
    );
    assert_eq!(
        one(r#"at 0 pty "a \"b\"\e[1m\x41\r\n""#),
        Stmt::At {
            ms: 0,
            event: Event::Pty(b"a \"b\"\x1b[1mA\r\n".to_vec())
        }
    );
    assert!(statement("at 0 pty 1b5").is_err());
}

#[test]
fn at_key_parses() {
    assert_eq!(
        one("at 5 key enter"),
        Stmt::At {
            ms: 5,
            event: Event::Key("enter".into())
        }
    );
    assert_eq!(
        one("at 5 key x mods=ctrl"),
        Stmt::At {
            ms: 5,
            event: Event::Key("x mods=ctrl".into())
        }
    );
    assert!(statement("at 5 key nosuchkey").is_err());
}

#[test]
fn at_env_parses() {
    assert_eq!(
        one("at 7 env amp 0.25 focus 0 motion reduced"),
        Stmt::At {
            ms: 7,
            event: Event::Env {
                amp: Some(0.25),
                focus: Some(false),
                motion: Some(Motion::Reduced)
            }
        }
    );
    assert_eq!(
        one("at 7 env focus 1"),
        Stmt::At {
            ms: 7,
            event: Event::Env {
                amp: None,
                focus: Some(true),
                motion: None
            }
        }
    );
    assert_eq!(
        one("at 7 env motion auto"),
        Stmt::At {
            ms: 7,
            event: Event::Env {
                amp: None,
                focus: None,
                motion: Some(Motion::Auto)
            }
        }
    );
    assert!(statement("at 7 env").is_err());
}

#[test]
fn at_cost_parses() {
    assert_eq!(
        one("at 3 cost 9000000"),
        Stmt::At {
            ms: 3,
            event: Event::Cost(9_000_000)
        }
    );
}

#[test]
fn at_pointer_parses() {
    assert_eq!(
        one("at 3 pointer 12.5 40"),
        Stmt::At {
            ms: 3,
            event: Event::Pointer(Some((12.5, 40.0)))
        }
    );
    assert_eq!(
        one("at 3 pointer off"),
        Stmt::At {
            ms: 3,
            event: Event::Pointer(None)
        }
    );
}

#[test]
fn at_press_parses() {
    assert_eq!(
        one("at 3 press 1 2"),
        Stmt::At {
            ms: 3,
            event: Event::Press(1.0, 2.0)
        }
    );
}

#[test]
fn at_select_parses() {
    assert_eq!(
        one("at 3 select 0 1 2 3"),
        Stmt::At {
            ms: 3,
            event: Event::Select(Some(((0, 1), (2, 3))))
        }
    );
    assert_eq!(
        one("at 3 select off"),
        Stmt::At {
            ms: 3,
            event: Event::Select(None)
        }
    );
}

#[test]
fn run_parses() {
    assert_eq!(one("run 500"), Stmt::Run { until_ms: 500 });
}

#[test]
fn time_never_runs_back() {
    assert!(
        parse("run 100\nat 50 key a\nrun 200\n").is_err(),
        "an event before the last run's end"
    );
    assert!(
        parse("run 100\nrun 100\n").is_err(),
        "a run that ends by the one before"
    );
    assert!(
        parse("at 300 key a\nrun 200\n").is_err(),
        "an event after the last run"
    );
    assert!(
        parse("run 100\nat 500 key a\nrun 200\nat 210 key b\nrun 300\n").is_err(),
        "an event after the last run, listed before a sooner one"
    );
    assert!(parse("at 100 key a\nrun 100\nat 100 key b\nrun 150\n").is_ok());
    assert!(
        parse("# a comment\ntheme #000000 #ffffff\nrun 10\n").is_ok(),
        "a colour's # starts no comment"
    );
}

#[test]
fn foreach_scheme_expands_over_each_polarity() {
    let script = parse("grid 4 10\nforeach-scheme light {\nrun 10\n}\nrun 20\n").expect("parses");
    let runs = script.runs();
    let light = trail_script::schemes(false);
    assert_eq!(runs.len(), light.len());
    assert!(!light.is_empty() && !trail_script::schemes(true).is_empty());
    for ((label, stmts), scheme) in runs.iter().zip(&light) {
        assert!(
            matches!(stmts.as_slice(), [Stmt::Setup(Setup::Grid { .. }), Stmt::Scheme(s), Stmt::Run { until_ms: 10 }, Stmt::Run { until_ms: 20 }] if s == scheme),
            "{label}: {stmts:?}"
        );
    }
    assert!(
        parse("foreach-scheme dark {\nrun 10\n").is_err(),
        "an unclosed block"
    );
    assert!(parse("foreach-scheme dusk {\n}\n").is_err());
}

/// A script that prints a profanity under `words <style>`.
fn words_script(style: &str) -> String {
    format!("grid 6 40\nstyle rainbow kitty\nwords {style}\nat 0 pty \"fuck\"\nrun 600\n")
}

#[test]
fn a_words_profanity_style_reaches_the_config() {
    let nova = dumped(&words_script("nova"));
    let rainbow = dumped(&words_script("rainbow"));
    assert_ne!(
        nova, rainbow,
        "`words nova` is [sparkle_words.profanity] style = \"nova\", whose burst and ink differ"
    );
    assert_eq!(
        rainbow,
        dumped(&words_script("candy")),
        "`words rainbow` names the default style, which a style no config key names keeps"
    );
}

/// A typed key, then three presents that each cost `ns` under `env motion <motion>`.
fn shed_script(motion: &str, ns: u64) -> String {
    format!(
        "grid 6 40\nstyle rainbow kitty\nhz 60\nat 0 env motion {motion}\nat 0 pty \"$ \"\n\
         at 700 key a\nat 702 pty 61\nat 750 cost {ns}\nat 783 cost {ns}\nat 816 cost {ns}\n\
         run 1300\n"
    )
}

/// `dump` with each frame's `shed` state word left out: what the frames drew and every other
/// engine's state.
fn beside_shed(dump: &str) -> String {
    dump.lines()
        .map(|l| match l.strip_prefix("  state ") {
            Some(words) => {
                let kept: Vec<&str> = words
                    .split(' ')
                    .filter(|w| !w.starts_with("shed="))
                    .collect();
                format!("  state {}", kept.join(" "))
            }
            None => l.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn present_costs_shed_only_under_auto_motion() {
    // 50 ms is over 1.5 of the 60 Hz panel's 16.7 ms frame, 1 ms under it.
    let (over, under) = (50_000_000, 1_000_000);
    assert_ne!(
        beside_shed(&dumped(&shed_script("auto", over))),
        beside_shed(&dumped(&shed_script("auto", under))),
        "under motion auto, three presents over budget shed the trail"
    );
    let (full_over, full_under) = (
        dumped(&shed_script("full", over)),
        dumped(&shed_script("full", under)),
    );
    assert_ne!(
        full_over, full_under,
        "the latch reads every present's cost, under any motion"
    );
    assert_eq!(
        beside_shed(&full_over),
        beside_shed(&full_under),
        "motion full never sheds: the costs move the latch's own words and nothing it draws"
    );
}

/// The corpus `lab trail-diff` runs when no script is named (aterm-lab's `trail_diff::CORPUS`).
const CORPUS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../aterm-effects/tests/fixtures/trail-diff"
);

/// Every `*.trail` file under `dir`.
fn corpus_scripts(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("a corpus entry").path();
        if path.is_dir() {
            corpus_scripts(&path, out);
        } else if path.extension().is_some_and(|x| x == "trail") {
            out.push(path);
        }
    }
}

/// Every corpus script parses and each of its passes reaches a `run`. It draws none of them:
/// `lab trail-diff` with no script named dumps the whole corpus.
#[test]
fn every_corpus_script_parses_and_each_pass_has_a_run() {
    let root = std::path::Path::new(CORPUS);
    let mut scripts = Vec::new();
    corpus_scripts(root, &mut scripts);
    for part in ["dark", "light", "flip"] {
        assert!(
            scripts.iter().any(|p| p.starts_with(root.join(part))),
            "the corpus has a {part}/ script"
        );
    }
    for path in &scripts {
        let text = std::fs::read_to_string(path).expect("read the script");
        let script = parse(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        for (label, stmts) in script.runs() {
            assert!(
                stmts.iter().any(|s| matches!(s, Stmt::Run { .. })),
                "{} ({label}) has no run",
                path.display()
            );
        }
    }
}
