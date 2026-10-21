// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The conditional `screen` read's promise, held over the host's changes as well as the
//! program's: the etag moves whenever the frame would, or a poller is told `unchanged` about a
//! stale picture. Output moves [`Terminal::screen_mutations`]; a config reload, a selection, a
//! resize or a reset change the frame with no output and must reach the etag another way. A
//! GUI scroll changes nothing the frame shows (the frame is the live screen), so it moves
//! neither the frame nor the etag. So "frame moved ⇒ etag moved" is checked over a corpus whose
//! every arm moves the frame, and over a seeded random walk in which two visited states may
//! share an etag only if they share a frame.
//!
//! The negative control, `content_seq` alone, runs through the same corpus and walk and must be
//! caught.
//!
//! A child of `control_query`, so it reads the private etag.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use aterm_core::config::TerminalConfig;
use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::Terminal;

use super::{
    cmd_screen_styled_json, gather_styled_frame, line_size_name, screen_etag, screen_gen,
    screen_hash, screen_text, serialize_styled_frame,
};
use crate::frame_key::FrameKey;
use crate::frame_memo::{FrameSlot, Styled};

/// The frame a full `screen` read serves, without its trailing etag key.
fn frame(t: &Terminal) -> String {
    serialize_styled_frame(&gather_styled_frame(t))
}

/// The `"etag"` a full reply ends with, as the 16 digits a poller sends back.
fn served_etag(reply: &str) -> String {
    let at = reply
        .rfind("\"etag\":\"")
        .expect("a full frame carries its etag")
        + 8;
    reply[at..at + 16].to_string()
}

/// Kitty graphics: transmit-and-display a 2x1-pixel RGBA image as image `id` over 2x1 cells,
/// `C=1` (the cursor does not move), `q=2` (no reply).
fn kitty_place(id: u32, fill: u8) -> Vec<u8> {
    let b64 = aterm_codec::base64::encode(&[fill; 8]).expect("encode");
    format!("\x1b_Ga=T,f=32,s=2,v=1,c=2,r=1,C=1,q=2,i={id};{b64}\x1b\\").into_bytes()
}

/// Kitty graphics: delete image `id` and its placements, freeing the data.
fn kitty_delete(id: u32) -> Vec<u8> {
    format!("\x1b_Ga=d,d=I,q=2,i={id}\x1b\\").into_bytes()
}

/// Kitty graphics: store image `id` (10x20 RGBA, one fill byte) without
/// placing it (`a=t`).
fn kitty_store(id: u32, fill: u8) -> Vec<u8> {
    let raw = vec![fill; 10 * 20 * 4];
    let b64 = aterm_codec::base64::encode(&raw).expect("encode");
    format!("\x1b_Ga=t,f=32,s=10,v=20,q=2,i={id};{b64}\x1b\\").into_bytes()
}

/// A Kitty Unicode placeholder cell naming image `id` (by foreground colour).
fn kitty_placeholder(id: u8) -> Vec<u8> {
    let mut ph = format!("\x1b[38;5;{id}m").into_bytes();
    let mut buf = [0u8; 4];
    for c in ['\u{10EEEE}', '\u{0305}', '\u{0305}'] {
        ph.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    }
    ph.extend_from_slice(b"\x1b[0m");
    ph
}

/// iTerm2 OSC 1337: an inline 3x1-cell image. The engine carries no codec, so
/// the payload need not decode; the frame ships its bytes either way.
fn osc1337_image(payload: &[u8]) -> Vec<u8> {
    let b64 = aterm_codec::base64::encode(payload).expect("encode");
    format!("\x1b]1337;File=inline=1;width=3;height=1:{b64}\x07").into_bytes()
}

/// A terminal plus the host config it was last handed: the reload arms change one field of it
/// and apply it the way the GUI's config reload does.
struct Rig {
    t: Terminal,
    config: TerminalConfig,
}

/// A bold palette-3 cell and a dim cell: the host style policy (`bold_is_bright`,
/// `faint_opacity`) re-resolves exactly these two without writing the grid.
const BOLD_AND_DIM: &[u8] = b"\x1b[1;33mB\x1b[0m \x1b[2mD\x1b[0m";

fn rig() -> Rig {
    let mut t = Terminal::new(6, 20);
    // Kitty placeholders resolve against real cell metrics.
    t.set_cell_pixel_size(10, 20);
    // OSC 4 SET is fail-closed behind this; set through the config, so a reload arm that
    // re-applies it does not switch it off again.
    let config = TerminalConfig {
        allow_palette_reconfigure: true,
        ..TerminalConfig::default()
    };
    t.apply_config(&config);
    // History, so the viewport has somewhere to scroll.
    for i in 0..10 {
        t.process(format!("history {i}\r\n").as_bytes());
    }
    t.process(BOLD_AND_DIM);
    Rig { t, config }
}

/// `inner` written at 1-based `(1, col)` with the cursor (and SGR) saved and
/// restored around it, so the write moves nothing but the cell it lands on.
fn at_top_left(r: &mut Rig, col: u16, inner: &str) {
    r.t.process(format!("\x1b7\x1b[1;{col}H{inner}\x1b8").as_bytes());
}

/// The glyph the cell at `(0, col - 1)` holds (row 0 is painted by the rig).
fn glyph_at(r: &Rig, col: u16) -> String {
    r.t.cell_grapheme(0, usize::from(col - 1))
        .expect("row 0 is painted")
}

type Step = (&'static str, fn(&mut Rig));

/// Every arm must move the frame (checked per arm); the order builds the state later arms
/// need. The host-only arms (config reload, selection, resize, host reset) feed no output, so
/// only what the etag reads of the terminal's state can see them.
fn corpus() -> Vec<Step> {
    let steps: &[Step] = &[
        // Host style policy, applied as a config reload does: resolved at render time, with no
        // grid write, seq move or output. First, so the reload changes nothing else on screen.
        ("config reload: bold_is_bright", |r| {
            r.config.bold_is_bright = !r.config.bold_is_bright;
            r.t.apply_config(&r.config);
        }),
        ("config reload: faint_opacity", |r| {
            r.config.faint_opacity = 0.9;
            r.t.apply_config(&r.config);
        }),
        // Host palette from a config reload: re-colours the bold cell (palette 3 now that bold
        // is not bright) with no output. While no program colour differs from the config's,
        // only the palette reaches the etag; later palette changes are OSC 4, which moves
        // `screen_mutations` too. Entry 11 keeps its default, so making bold bright again
        // later still re-colours that cell.
        ("config reload: custom_palette", |r| {
            let mut palette = aterm_types::ColorPalette::new();
            palette.set(3, aterm_types::Rgb::new(0x12, 0x9a, 0x6c));
            r.config.custom_palette = Some(palette);
            r.t.apply_config(&r.config);
        }),
        ("write text", |r| r.t.process(b"hello")),
        // The glyph a cell already holds, written over it: of everything the
        // frame shows, only `seq` moves.
        ("rewrite a cell with the glyph it holds", |r| {
            let glyph = glyph_at(r, 1);
            at_top_left(r, 1, &glyph);
        }),
        ("rewrite a cell with another glyph", |r| {
            let other = if glyph_at(r, 1) == "Q" { "R" } else { "Q" };
            at_top_left(r, 1, other);
        }),
        ("underline a cell over the glyph it holds", |r| {
            let glyph = glyph_at(r, 2);
            at_top_left(r, 2, &format!("\x1b[4m{glyph}"));
        }),
        ("link a cell over the glyph it holds (OSC 8)", |r| {
            let glyph = glyph_at(r, 3);
            at_top_left(
                r,
                3,
                &format!("\x1b]8;;https://example.org\x1b\\{glyph}\x1b]8;;\x1b\\"),
            );
        }),
        ("relink that cell to another target (OSC 8)", |r| {
            let glyph = glyph_at(r, 3);
            at_top_left(
                r,
                3,
                &format!("\x1b]8;;https://example.net\x1b\\{glyph}\x1b]8;;\x1b\\"),
            );
        }),
        ("move cursor", |r| r.t.process(b"\x1b[3;5H")),
        ("hide cursor (DECTCEM)", |r| r.t.process(b"\x1b[?25l")),
        ("cursor style (DECSCUSR)", |r| r.t.process(b"\x1b[3 q")),
        ("cursor colour (OSC 12)", |r| {
            r.t.process(b"\x1b]12;#ff00ff\x1b\\");
        }),
        ("paint palette-indexed text (SGR 31)", |r| {
            r.t.process(b"\x1b[2;1H\x1b[31mIDX\x1b[m");
        }),
        ("palette entry (OSC 4)", |r| {
            r.t.process(b"\x1b]4;1;#00ff00\x1b\\");
        }),
        ("default fg (OSC 10)", |r| {
            r.t.process(b"\x1b]10;#123456\x1b\\")
        }),
        ("default bg (OSC 11)", |r| {
            r.t.process(b"\x1b]11;#654321\x1b\\")
        }),
        ("selection bg (OSC 17)", |r| {
            r.t.process(b"\x1b]17;#0000ff\x1b\\");
        }),
        ("selection fg (OSC 19)", |r| {
            r.t.process(b"\x1b]19;#00ffff\x1b\\");
        }),
        ("reverse video on (DECSCNM)", |r| r.t.process(b"\x1b[?5h")),
        ("reverse video off", |r| r.t.process(b"\x1b[?5l")),
        ("styled write (SGR 1;4:3;58)", |r| {
            r.t.process(b"\x1b[1;4:3;58:2::255:0:0mZZ\x1b[m");
        }),
        ("hyperlink write (OSC 8)", |r| {
            r.t.process(b"\x1b]8;id=x;https://example.com\x1b\\LINK\x1b]8;;\x1b\\");
        }),
        ("wide + combining write", |r| {
            r.t.process("界e\u{301}".as_bytes())
        }),
        // Cursor saved and restored, so the line size is all that moves.
        ("DEC double width (ESC # 6)", |r| {
            r.t.process(b"\x1b7\x1b[2;1H\x1b#6\x1b8");
        }),
        ("DEC double height top (ESC # 3)", |r| {
            r.t.process(b"\x1b7\x1b[3;1H\x1b#3\x1b8");
        }),
        ("select a range", |r| {
            let sel = r.t.text_selection_mut();
            sel.start_selection(0, 1, SelectionSide::Left, SelectionType::Simple);
            sel.update_selection(0, 5, SelectionSide::Right);
            sel.complete_selection();
        }),
        ("selection becomes a block", |r| {
            let sel = r.t.text_selection_mut();
            sel.start_selection(0, 1, SelectionSide::Left, SelectionType::Block);
            sel.update_selection(0, 5, SelectionSide::Right);
            sel.complete_selection();
        }),
        ("clear the selection", |r| r.t.text_selection_mut().clear()),
        // The style policy again, over a recoloured screen: this reload also re-asserts the
        // config's colours, so it is not clean like the first two.
        ("config reload: faint_opacity back", |r| {
            r.config.faint_opacity = 0.5;
            r.t.apply_config(&r.config);
        }),
        // The cursor goes where the image will land as its own arm, so the placement below
        // moves the image and nothing else.
        ("cursor to where the image lands", |r| {
            r.t.process(b"\x1b[5;10H")
        }),
        ("kitty image placed, cursor unmoved (C=1)", |r| {
            r.t.process(&kitty_place(7, 0x00));
        }),
        // Same id, same size, same cell, different pixels.
        ("kitty image replaced by a same-size one", |r| {
            r.t.process(&kitty_delete(7));
            r.t.process(&kitty_place(7, 0xff));
        }),
        ("kitty image deleted (a=d)", |r| {
            r.t.process(&kitty_delete(7))
        }),
        ("kitty image shown by a placeholder cell", |r| {
            r.t.process(b"\x1b[6;15H");
            r.t.process(&kitty_store(5, 0));
            r.t.process(&kitty_placeholder(5));
        }),
        // No cell written: the placeholder shows whatever image 5 is now.
        ("kitty image re-transmitted under its placeholder", |r| {
            r.t.process(&kitty_store(5, 0xff));
        }),
        ("inline image (OSC 1337)", |r| {
            r.t.process(b"\x1b[4;1H");
            r.t.process(&osc1337_image(b"not-a-real-png"));
        }),
        ("scroll a line", |r| r.t.process(b"\x1b[6;1H\r\n")),
        ("enter alt screen (1049)", |r| r.t.process(b"\x1b[?1049h")),
        ("write on alt screen", |r| r.t.process(b"alt-content")),
        ("leave alt screen", |r| r.t.process(b"\x1b[?1049l")),
        ("resize", |r| r.t.resize(8, 24)),
        ("host reset (no output)", |r| r.t.reset()),
        ("bold and dim text after the reset", |r| {
            r.t.process(BOLD_AND_DIM)
        }),
        ("config reload: bold_is_bright back", |r| {
            r.config.bold_is_bright = !r.config.bold_is_bright;
            r.t.apply_config(&r.config);
        }),
        ("full reset (RIS)", |r| r.t.process(b"\x1bc")),
    ];
    steps.to_vec()
}

/// An etag under test: the shipped one, or a negative control.
type Etag = fn(&Terminal) -> u64;

/// What one pass of the corpus found, per etag under test.
struct Pass {
    /// Arms that did not move the frame (must stay empty: a dark arm is an
    /// untested mutation class wearing a green test).
    dark: Vec<&'static str>,
    /// Per etag, the arms that moved the frame but not that etag.
    missed: Vec<Vec<&'static str>>,
}

/// Run the corpus once, reading every etag in `etags` before and after each arm.
fn run_corpus(etags: &[Etag]) -> Pass {
    let mut rig = rig();
    let mut pass = Pass {
        dark: Vec::new(),
        missed: vec![Vec::new(); etags.len()],
    };
    let read = |t: &Terminal| -> (String, Vec<u64>) {
        (frame(t), etags.iter().map(|etag| etag(t)).collect())
    };
    for (name, step) in corpus() {
        let (f0, e0) = read(&rig.t);
        step(&mut rig);
        let (f1, e1) = read(&rig.t);
        // A pure read: two reads of one state, one etag — or the conditional
        // could never fire at all.
        assert_eq!(e1, read(&rig.t).1, "{name}: two reads, two etags");
        if f0 == f1 {
            pass.dark.push(name);
            continue;
        }
        for (i, (before, after)) in e0.iter().zip(&e1).enumerate() {
            if before == after {
                pass.missed[i].push(name);
            }
        }
    }
    pass
}

/// A config reload of `bold_is_bright` or `faint_opacity` re-colours bold and dim cells while
/// moving none of the counters the etag reads (no output, seq, generation or viewport; asserted
/// as premises). Only `render_sig` carries it, so a poller holding the old etag gets the
/// re-coloured frame.
#[test]
fn a_host_style_policy_reload_is_not_unchanged() {
    type Reload = (&'static str, fn(&mut TerminalConfig));
    let reloads: [Reload; 2] = [
        ("bold_is_bright", |c| c.bold_is_bright = !c.bold_is_bright),
        ("faint_opacity", |c| c.faint_opacity = 0.9),
    ];
    for (name, change) in reloads {
        let mut config = TerminalConfig::default();
        let mut t = Terminal::new(4, 20);
        t.apply_config(&config);
        t.process(BOLD_AND_DIM);
        let counters = |t: &Terminal| {
            (
                t.content_seq(),
                screen_gen(t).to_string(),
                t.screen_mutations(),
                t.grid().display_offset(),
            )
        };
        let (frame_0, counters_0) = (frame(&t), counters(&t));
        let sig_0 = crate::frame_key::render_sig(&t);
        let term = Arc::new(Mutex::new(t));
        let token = served_etag(&cmd_screen_styled_json(
            &term,
            &crate::frame_memo::FrameSlot::default(),
            "",
        ));
        change(&mut config);
        term.lock().expect("mutex").apply_config(&config);
        {
            let t = term.lock().expect("mutex");
            assert_eq!(
                counters(&t),
                counters_0,
                "{name}: the premise: the reload moves no counter and no viewport"
            );
            assert_ne!(
                frame(&t),
                frame_0,
                "{name}: the premise: the reload re-resolves a cell's colour"
            );
            assert_ne!(
                crate::frame_key::render_sig(&t),
                sig_0,
                "{name}: render_sig carries the style policy (so `subscribe … cells` \
                 re-emits on the reload too)"
            );
        }
        let reply = cmd_screen_styled_json(
            &term,
            &crate::frame_memo::FrameSlot::default(),
            &format!("since={token}"),
        );
        assert!(
            !reply.contains("\"unchanged\"") && reply.contains("\"glyph\""),
            "{name}: a style-policy reload was served as `unchanged`: {reply}"
        );
        assert_ne!(served_etag(&reply), token, "{name}: the etag moved");
    }
}

/// VT fragments the random walk feeds between host operations.
const FRAGMENTS: &[&[u8]] = &[
    b"x",
    b"hello",
    b"\r\n",
    b"\x1b[H",
    b"\x1b[3;7H",
    b"\x1b[1;31m",
    b"\x1b[1;33mB",
    b"\x1b[0m",
    b"\x1b[2mD",
    b"\x1b[7m",
    b"\x1b[4:3m",
    b"\x1b[58;2;1;2;3m",
    b"\x1b]8;;http://a\x1b\\",
    b"\x1b]8;;\x1b\\",
    b"\x1b[?25l",
    b"\x1b[?25h",
    b"\x1b[5 q",
    b"\x1b]12;#123456\x1b\\",
    b"\x1b]112\x1b\\",
    b"\x1b]4;1;#00ff00\x1b\\",
    b"\x1b]104\x1b\\",
    b"\x1b[?5h",
    b"\x1b[?5l",
    b"\x1b#6",
    b"\x1b#5",
    b"\x1b[?1049h",
    b"\x1b[?1049l",
    b"\x1b[2J",
    b"\x1b[K",
    b"\x1b[L",
    b"\x1b[M",
    "界".as_bytes(),
    "e\u{301}".as_bytes(),
];

/// What one random walk saw under one etag.
struct Walk {
    /// Steps that moved the frame.
    moved: usize,
    /// Steps whose etag had been handed out before (each compared frames).
    revisits: usize,
    /// Distinct etags handed out.
    distinct: usize,
    /// The first step at which two different frames shared an etag, if any.
    collision: Option<String>,
}

/// A seeded random walk over VT fragments and host operations under `etag`, stopping at the
/// first etag two different frames share: a token from any earlier state must not match a
/// different frame now. Host operations feed no output, so the walk revisits etags (a reload
/// toggled back, a scroll undone), the comparisons that matter.
fn walk(etag: Etag) -> Walk {
    let mut seen: HashMap<u64, String> = HashMap::new();
    let (mut moved, mut revisits) = (0usize, 0usize);
    let mut collision = None;
    let mut prev = frame(&rig().t);
    walk_steps(|step, what, rig| {
        let now = frame(&rig.t);
        let e = etag(&rig.t);
        if now != prev {
            moved += 1;
        }
        if let Some(earlier) = seen.get(&e) {
            revisits += 1;
            if earlier != &now {
                collision = Some(format!(
                    "step {step} ({what}): a different frame under etag {e:016x}, \
                     handed out earlier for another frame"
                ));
                return false;
            }
        } else {
            seen.insert(e, now.clone());
        }
        prev = now;
        true
    });
    Walk {
        moved,
        revisits,
        distinct: seen.len(),
        collision,
    }
}

/// The seeded walk's 800 steps over a fresh [`rig`], each handed to `visit`
/// with its index and name once taken; `visit` answers whether to go on.
fn walk_steps(mut visit: impl FnMut(usize, &'static str, &Rig) -> bool) {
    let mut rig = rig();
    // xorshift64*: deterministic, no dependency, no clock.
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = move |n: usize| -> usize {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        (state.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 33) as usize % n
    };
    for step in 0..800 {
        let what = match next(17) {
            0 => {
                rig.t
                    .process(&kitty_place(3, u8::try_from(next(256)).unwrap_or(0)));
                "kitty place"
            }
            1 => {
                rig.t.process(&kitty_delete(3));
                "kitty delete"
            }
            // Image 3 stored and named by a placeholder cell at the cursor:
            // text whose picture a viewport-mapped decode would lose.
            2 => {
                rig.t
                    .process(&kitty_store(3, u8::try_from(next(256)).unwrap_or(0)));
                rig.t.process(&kitty_placeholder(3));
                "kitty placeholder"
            }
            3 => {
                rig.t
                    .process(&osc1337_image(&[u8::try_from(next(256)).unwrap_or(0); 5]));
                "OSC 1337 image"
            }
            4 => {
                let rows = 4 + u16::try_from(next(4)).unwrap_or(0);
                let cols = 12 + u16::try_from(next(8)).unwrap_or(0);
                rig.t.resize(rows, cols);
                "resize"
            }
            5 | 6 => {
                rig.t.scroll_display(if next(2) == 0 { 2 } else { -2 });
                "viewport scroll"
            }
            7..=9 => {
                rig.config.bold_is_bright = next(2) == 0;
                rig.config.faint_opacity = if next(2) == 0 { 0.5 } else { 0.8 };
                rig.t.apply_config(&rig.config);
                "config reload (style policy)"
            }
            10 | 11 => {
                let sel = rig.t.text_selection_mut();
                if next(3) == 0 {
                    sel.clear();
                } else {
                    let row = i32::try_from(next(4)).unwrap_or(0);
                    let kind = if next(2) == 0 {
                        SelectionType::Simple
                    } else {
                        SelectionType::Block
                    };
                    sel.start_selection(row, 0, SelectionSide::Left, kind);
                    let end = u16::try_from(next(10)).unwrap_or(0);
                    sel.update_selection(row + 1, end, SelectionSide::Right);
                    sel.complete_selection();
                }
                "selection"
            }
            12 => match next(8) {
                0 => {
                    rig.t.reset();
                    "host reset"
                }
                1 => {
                    rig.t.clear_scrollback();
                    "clear scrollback"
                }
                _ => {
                    rig.t.process(BOLD_AND_DIM);
                    "bold and dim text"
                }
            },
            _ => {
                rig.t.process(FRAGMENTS[next(FRAGMENTS.len())]);
                "VT fragment"
            }
        };
        if !visit(step, what, &rig) {
            return;
        }
    }
}

/// The shipped etag over the walk: no two visited frames share an etag, and
/// the walk had reach — it moved the screen, and it came back to etags it had
/// already handed out (a repeat etag is the case that matters). The negative
/// control, `content_seq` alone, must be caught handing one etag to two
/// frames, or the walk has no teeth.
#[test]
fn no_two_visited_states_share_an_etag_unless_they_share_a_frame() {
    assert!(
        walk(Terminal::content_seq).collision.is_some(),
        "negative control: the walk never showed `content_seq` alone two frames under one etag"
    );
    let w = walk(screen_etag);
    assert_eq!(
        w.collision, None,
        "a poller holding the earlier etag would be told `unchanged`"
    );
    assert!(
        w.moved > 250,
        "only {} of 800 steps moved the frame ({} distinct etags)",
        w.moved,
        w.distinct
    );
    assert!(
        w.revisits > 50,
        "only {} steps revisited an etag, so equal etags were barely compared",
        w.revisits
    );
}

/// THE PUBLISHED FRAME IS THE FRESH ONE. A session's slot, read over the
/// seeded walk the way every reader reads it (a look, the keys compared under
/// the hold, a miss built and published), serves at every step exactly what a
/// fresh read of the same state builds: the text ([`screen_text`]), its hash
/// ([`screen_hash`]), and the styled frame. A `cells` hold keeps the frame, as
/// a `subscribe … cells` watch does. The walk must also HIT, on both halves,
/// or nothing published was ever served. NEGATIVE CONTROL: a frame memo keyed
/// on `content_seq` alone is caught serving a stale frame on the same walk (the
/// text half's control is `frame_memo`'s alt re-entry test: over this walk the
/// text never changes at an unchanged seq).
#[test]
fn the_published_frame_equals_a_fresh_read_over_the_walk() {
    let slot = std::sync::Arc::new(FrameSlot::default());
    let _cells = FrameSlot::hold_cells(&slot);
    let (mut text_hits, mut frame_hits) = (0usize, 0usize);
    let mut last_text = None;
    // The negative control: (seq, frame) as of its last build.
    let mut by_seq: Option<(u64, String)> = None;
    let mut stale = None;
    walk_steps(|step, what, rig| {
        let t = &rig.t;
        let mut look = slot.look_frame();
        let text = look.text(t);
        let key = FrameKey::read(t);
        let styled = match look.styled(t, &key) {
            Styled::Hit(frame) => {
                frame_hits += 1;
                frame
            }
            Styled::Miss(snap) => look.publish_styled(key, &snap),
        };
        drop(look);
        if last_text
            .as_ref()
            .is_some_and(|last| std::sync::Arc::ptr_eq(last, &text))
        {
            text_hits += 1;
        }
        let (fresh_text, fresh_frame) = (screen_text(t), frame(t));
        assert_eq!(text.whole(), fresh_text, "step {step} ({what}): the text");
        assert_eq!(text.fnv(), screen_hash(t), "step {step} ({what}): the hash");
        assert_eq!(
            styled.body(),
            fresh_frame,
            "step {step} ({what}): the frame"
        );
        last_text = Some(text);

        let seq = t.content_seq();
        match &by_seq {
            Some((at, served)) if *at == seq => {
                if *served != fresh_frame {
                    stale.get_or_insert(format!("step {step} ({what})"));
                }
            }
            _ => by_seq = Some((seq, fresh_frame)),
        }
        true
    });
    assert!(
        text_hits > 50,
        "only {text_hits} steps were served a published text"
    );
    assert!(
        frame_hits > 20,
        "only {frame_hits} steps were served a published frame"
    );
    assert!(
        stale.is_some(),
        "a frame memo keyed on content_seq alone must be caught serving a stale frame"
    );
}

/// The shipped etag over the corpus misses no arm, and the negative control
/// (`content_seq` alone) is caught, so the corpus has teeth.
#[test]
fn every_incomplete_etag_is_caught_missing_real_frame_changes() {
    let pass = run_corpus(&[screen_etag, Terminal::content_seq]);
    assert!(pass.dark.is_empty(), "dark arms: {:?}", pass.dark);
    assert!(
        pass.missed[0].is_empty(),
        "the shipped etag missed {:?}",
        pass.missed[0]
    );
    assert!(
        !pass.missed[1].is_empty(),
        "no arm showed `content_seq` alone a moved frame under an unmoved etag, \
         so the harness cannot tell it from the shipped one"
    );
}

/// What a VIEWPORT-relative read of the frame's rows would see: each row's DEC
/// line size off the display-mapped grid, and each image placement off the
/// viewport's image reader.
fn viewport_view(t: &Terminal) -> (Vec<&'static str>, Vec<(usize, usize)>) {
    let rows = t.rows();
    let line_sizes = (0..rows)
        .map(|r| {
            line_size_name(
                t.grid()
                    .row(r)
                    .map_or(aterm_core::grid::LineSize::SingleWidth, |row| {
                        row.line_size()
                    }),
            )
        })
        .collect();
    let images = t
        .images_frame(usize::from(rows))
        .iter()
        .enumerate()
        .flat_map(|(r, row)| row.iter().map(move |&(c, _)| (r, c)))
        .collect();
    (line_sizes, images)
}

/// A VIEWPORT SCROLL IS NOT A FRAME CHANGE. Every field of the frame is read
/// off the live screen, so a person scrolling back through history moves
/// neither the frame nor the etag, and a poller holding the etag is answered
/// `unchanged`. The screen carries everything a viewport-relative read would
/// move — an image on screen, a DEC double-width row, an image scrolled into
/// history, a Kitty Unicode placeholder on the last row (whose viewport row,
/// scrolled back, is history text, so a placeholder decoded through the
/// viewport drops its image) — and the premises assert the viewport readers
/// do see it move, so the frame standing still is the live read at work, not
/// an empty screen.
///
/// NEGATIVE CONTROL: that premise, a viewport-relative read of the same
/// screen ([`viewport_view`]), moves on every scroll; an etag folding the
/// display offset cost every `screen since=` poller a full frame each time
/// the person touched the wheel.
#[test]
fn a_viewport_scroll_moves_neither_the_frame_nor_the_etag() {
    type Scroll = (&'static str, fn(&mut Terminal));
    let scrolls: [Scroll; 2] = [
        ("scroll back 2", |t| t.scroll_display(2)),
        ("scroll to the top", Terminal::scroll_to_top),
    ];
    let kitty_b64 = format!(
        "\"b64\":\"{}\"",
        aterm_codec::base64::encode(&[0u8; 10 * 20 * 4]).expect("encode")
    );
    for (name, scroll) in scrolls {
        let mut t = Terminal::new(6, 20);
        // Kitty placeholders resolve against real cell metrics.
        t.set_cell_pixel_size(10, 20);
        t.process(&osc1337_image(b"scrolls-into-history"));
        for i in 0..10 {
            t.process(format!("history {i}\r\n").as_bytes());
        }
        t.process(b"\x1b[3;1H");
        t.process(&osc1337_image(b"on-screen"));
        t.process(b"\x1b[5;1HDOUBLE\x1b#6");
        t.process(b"\x1b[6;15H");
        t.process(&kitty_store(5, 0));
        t.process(&kitty_placeholder(5));
        let (frame_0, etag_0, view_0) = (frame(&t), screen_etag(&t), viewport_view(&t));
        assert!(
            frame_0.contains("\"double_width\"") && frame_0.contains("\"b64\":"),
            "{name}: the premise: a double-width row and an image are on screen"
        );
        assert!(
            frame_0.contains(&kitty_b64),
            "{name}: the premise: the placeholder shows its Kitty image"
        );
        let term = Arc::new(Mutex::new(t));
        let token = served_etag(&cmd_screen_styled_json(
            &term,
            &crate::frame_memo::FrameSlot::default(),
            "",
        ));
        scroll(&mut term.lock().expect("mutex"));
        {
            let t = term.lock().expect("mutex");
            assert_ne!(
                t.grid().display_offset(),
                0,
                "{name}: the premise: the viewport moved"
            );
            assert_ne!(
                viewport_view(&t),
                view_0,
                "{name}: the premise: a viewport-relative read sees the rows move"
            );
            assert_eq!(frame(&t), frame_0, "{name}: the frame is the live screen");
            assert_eq!(screen_etag(&t), etag_0, "{name}: the etag stands");
        }
        let reply = cmd_screen_styled_json(
            &term,
            &crate::frame_memo::FrameSlot::default(),
            &format!("since={token}"),
        );
        assert!(
            reply.contains("\"unchanged\""),
            "{name}: a viewport scroll was served a full frame: {reply}"
        );
    }
}
