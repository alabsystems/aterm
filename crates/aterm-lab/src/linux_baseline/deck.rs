// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Layout checks for the stats Deck across Linux display families.

use std::ops::Range;
use std::path::Path;
use std::time::Instant;

use super::{Caption, Frame, Recording, Session, slot_class};
use crate::linux::{self, Up};
use crate::linux_guest::{self as guest, Arch, Display, Screen, Seat};
use crate::linux_proof::{self as proof, Gui};

/// The Deck's height in points: aterm-gui's `stats_deck::DECK_PT`.
pub const DECK_PT: f64 = 62.0;

/// The column a press selects the first of two tabs at: well inside its
/// card on an 80-column strip, clear of its close mark.
const FIRST_TAB_COL: u16 = 5;

/// What one check found.
struct Check {
    name: String,
    ok: bool,
    detail: String,
}

impl Check {
    fn new(name: String, r: Result<String, String>) -> Check {
        match r {
            Ok(detail) => Check {
                name,
                ok: true,
                detail,
            },
            Err(detail) => Check {
                name,
                ok: false,
                detail,
            },
        }
    }
}

/// A row's name.
#[must_use]
pub(crate) fn row_name(arch: Arch, screen: Screen) -> String {
    format!(
        "{} ({}) stats Deck {} ({})",
        arch.name(),
        arch.runs(),
        screen.name(),
        screen.display.protocol()
    )
}

/// Runs one display family's stats Deck layout row.
pub(crate) fn row(up: &Up, arch: Arch, screen: Screen, bin: &str, results: &Path) -> proof::Row {
    let name = row_name(arch, screen);
    println!("\n=== {name} ===");
    let started = Instant::now();
    let r = drive(up, arch, screen, bin, results);
    up.stop_units();
    let (verdict, detail) = match r {
        Err(why) => (proof::Verdict::NotRun, why),
        Ok(checks) => {
            let failed: Vec<&Check> = checks.iter().filter(|c| !c.ok).collect();
            let verdict = if failed.is_empty() {
                proof::Verdict::Pass
            } else {
                proof::Verdict::Fail
            };
            let lines: Vec<String> = failed
                .iter()
                .copied()
                .chain(checks.iter().filter(|c| c.ok))
                .map(|c| {
                    format!(
                        "{} {}: {}",
                        if c.ok { "ok" } else { "FAILED" },
                        c.name,
                        c.detail
                    )
                })
                .collect();
            (verdict, lines.join("\n"))
        }
    };
    println!(
        "  {} {}",
        verdict.label(),
        detail.lines().next().unwrap_or("")
    );
    proof::Row {
        name,
        verdict,
        secs: started.elapsed().as_secs_f64(),
        detail,
    }
}

/// Start the screen, then aterm-gui with the Deck on twice, on the GPU (the
/// renderer a Linux window draws with, whose `image` is its own render) and
/// on the CPU (`--cpu`, whose remainder bands are aterm's own): each check's
/// finding, or why the row could not run.
fn drive(
    up: &Up,
    arch: Arch,
    screen: Screen,
    bin: &str,
    results: &Path,
) -> Result<Vec<Check>, String> {
    if screen.display == Display::Weston {
        linux::start_weston_capturing(up)?;
    } else {
        linux::start_screen(up, screen)?;
    }
    let mut checks = Vec::new();
    for cpu in [false, true] {
        let presenter = if cpu { "cpu" } else { "gpu" };
        let gui = Gui {
            up,
            bin: bin.to_string(),
            dir: format!(
                "/tmp/{}{}-{}-deck-{presenter}",
                guest::UNIT_PREFIX,
                arch.name(),
                screen.name()
            ),
            name: format!("{}-{}-deck-{presenter}", arch.name(), screen.name()),
            cpu,
            deck: true,
        };
        gui.prepare().map_err(|e| format!("scratch world: {e}"))?;
        let s = Session {
            gui,
            arch,
            screen,
            bus: format!("{bin}/lane-bus"),
            wl: String::new(),
            results: results.to_path_buf(),
            rec: Recording::default(),
            roles: Vec::new(),
            end_terminal: None,
            marks: Vec::new(),
        };
        let invocation = s.gui.start(Some(screen.display), &[])?;
        let started = (|| -> Result<String, String> {
            if !s.sh(&guest::wait_socket(&s.gui.sock(), 60)).ok() {
                return Err("its control socket never listened (60 s)".into());
            }
            let r = s.ctl(&["ready", "60000"])?;
            if !r.starts_with("OK ready") {
                return Err(format!("`ready` answered {r}"));
            }
            s.settled_dims()
        })();
        let on = match started {
            Ok(d) => d,
            Err(why) => {
                let journal = s.gui.journal(&invocation);
                let kept = s.gui.keep_crash_reports(results);
                s.gui.stop();
                return Err(format!(
                    "aterm-gui on the {presenter}: {why}; its journal: {}{kept}",
                    crate::util::one_line(&journal, 400)
                ));
            }
        };
        checks.extend(session_checks(&s, presenter, &on));
        s.gui.stop();
    }
    Ok(checks)
}

/// A `dims` reply's numbers.
struct Dims {
    scale: f64,
    cell_w: usize,
    cell_h: usize,
    pad: usize,
    pad_top: usize,
    head: usize,
    tab_rows: usize,
    deck_y: usize,
    deck_h: usize,
    surface: (usize, usize),
}

impl Dims {
    fn read(d: &str) -> Result<Dims, String> {
        let num = |k: &str| {
            proof::field(d, k)
                .and_then(|v| v.parse::<usize>().ok())
                .ok_or_else(|| format!("`dims` has no {k}: {d}"))
        };
        Ok(Dims {
            scale: proof::field(d, "scale")
                .and_then(|v| v.parse::<f64>().ok())
                .ok_or_else(|| format!("`dims` has no scale: {d}"))?,
            cell_w: num("cell_w")?,
            cell_h: num("cell_h")?,
            pad: num("pad")?,
            pad_top: num("pad_top")?,
            head: num("head")?,
            tab_rows: num("tab_rows")?,
            deck_y: num("deck_y")?,
            deck_h: num("deck_h")?,
            surface: (num("surface_w")?, num("surface_h")?),
        })
    }

    /// The Deck's 62 pt at this scale, in device px.
    fn deck_want(&self) -> usize {
        (DECK_PT * self.scale).round() as usize
    }

    /// The Deck's rows in the frame.
    fn deck_rows(&self) -> Range<usize> {
        self.deck_y..self.deck_y + self.deck_h
    }

    /// The strip band's foot: the top pad, the head and the strip's rows.
    fn band_foot(&self) -> usize {
        self.pad_top + self.head + self.tab_rows * self.cell_h
    }
}

/// One session's checks: the layout on both presenters, and the bands where
/// the CPU presenter makes them.
fn session_checks(s: &Session<'_>, presenter: &'static str, on: &str) -> Vec<Check> {
    let named = |check: &str| format!("{presenter} {check}");
    let mut out = Vec::new();
    let d = match Dims::read(on) {
        Ok(d) => d,
        Err(e) => {
            out.push(Check::new(named("head"), Err(e)));
            return out;
        }
    };
    out.push(Check::new(named("head"), head(&d)));
    if s.screen.display.seat() == Some(Seat::Xtest)
        && let Err(e) = s.seat_unit_of("", "-", &["move:0,0"]).and_then(|seat| {
            let done = seat.done(s);
            seat.stop(s);
            done
        })
    {
        out.push(Check::new(
            named("pointer"),
            Err(format!("not parked: {e}")),
        ));
    }
    let caption = s.caption();
    let frame_on = s.frame(&format!("deck-{presenter}-on"));
    out.push(Check::new(
        named("buttons"),
        match (&caption, &frame_on) {
            (Ok(c), Ok(f)) => buttons(c, f, &d),
            (Err(e), _) | (_, Err(e)) => Err(e.clone()),
        },
    ));
    out.push(Check::new(
        named("off"),
        off(s, presenter, &d, frame_on.as_ref().ok()),
    ));
    out.push(Check::new(named("click"), click(s)));
    if presenter == "cpu" {
        out.push(Check::new(
            named("bands"),
            match &caption {
                Ok(c) => bands(s, c),
                Err(e) => Err(e.clone()),
            },
        ));
    }
    out
}

/// The Deck's height and place in the head.
fn head(d: &Dims) -> Result<String, String> {
    let want = d.deck_want();
    if d.deck_h != want || d.deck_y != 0 || d.head < d.deck_h {
        return Err(format!(
            "deck_y={} deck_h={} in head={}, where the Deck's {DECK_PT} pt at scale {:.2} \
             is {want} px at the head's top",
            d.deck_y, d.deck_h, d.head, d.scale
        ));
    }
    Ok(format!(
        "deck_y=0 deck_h={want} ({DECK_PT} pt at scale {:.2}) in head={}",
        d.scale, d.head
    ))
}

/// Each header button's slot drawn in the band under the Deck.
fn buttons(caption: &Caption, f: &Frame, d: &Dims) -> Result<String, String> {
    if !caption.client {
        return Ok("the window system frames the window: no buttons of aterm's".into());
    }
    let x = |c: u16| d.pad + usize::from(c) * d.cell_w;
    let top = d.deck_rows().end;
    let read: Vec<String> = caption
        .buttons
        .iter()
        .map(|(n, s, e)| {
            let (class, differ) = slot_class(f, x(*s), x(*e), top, d.band_foot());
            format!("{n} {class} ({differ} px)")
        })
        .collect();
    let all = read.join(", ");
    if caption.buttons.is_empty() || read.iter().any(|r| r.contains(" bare ")) {
        Err(format!(
            "under the Deck, rows {top}..{}: {all}",
            d.band_foot()
        ))
    } else {
        Ok(format!(
            "under the Deck, rows {top}..{}: {all}",
            d.band_foot()
        ))
    }
}

/// The Deck turned off: the head less it, and the band under it the band
/// without it, pixel for pixel; then the Deck turned back on.
fn off(
    s: &Session<'_>,
    presenter: &str,
    on: &Dims,
    frame_on: Option<&Frame>,
) -> Result<String, String> {
    let frame_on = frame_on.ok_or("no frame with the Deck on")?;
    let r = s.ctl(&["invoke", "StatsBarsOff"])?;
    if !r.starts_with("OK") {
        return Err(format!("`invoke StatsBarsOff` answered {r}"));
    }
    s.dims_until(&|r| proof::field(r, "deck_h") == Some("0"))?;
    let off = Dims::read(&s.settled_dims()?)?;
    let frame_off = s.frame(&format!("deck-{presenter}-off"));
    let back = s.ctl(&["invoke", "StatsBarsDeck"]);
    let back = back.and_then(|r| {
        if r.starts_with("OK") {
            let want = on.deck_h.to_string();
            s.dims_until(&|r| proof::field(r, "deck_h") == Some(want.as_str()))
                .and_then(|_| s.settled_dims())
        } else {
            Err(format!("`invoke StatsBarsDeck` answered {r}"))
        }
    });
    let frame_off = frame_off?;
    if on.head.checked_sub(on.deck_h) != Some(off.head) {
        return Err(format!(
            "head={} with the Deck ({} px), head={} without it",
            on.head, on.deck_h, off.head
        ));
    }
    let band = off.band_foot();
    let held = band_held(frame_on, on.deck_rows().end, &frame_off, band)?;
    back.map_err(|e| format!("{held}; the Deck would not come back: {e}"))?;
    Ok(format!(
        "head {} less the Deck is {} without it; {held}",
        on.head, off.head
    ))
}

/// Whether frame `on`'s `rows` band rows from `top` are frame `off`'s from
/// its top, by RGB: what it says of them.
fn band_held(on: &Frame, top: usize, off: &Frame, rows: usize) -> Result<String, String> {
    if on.w != off.w || on.h < top + rows || off.h < rows {
        return Err(format!(
            "the frames are {}x{} with the Deck and {}x{} without it, for a {rows}-row band \
             from row {top}",
            on.w, on.h, off.w, off.h
        ));
    }
    let (mut differ, mut first) = (0usize, None);
    for y in 0..rows {
        for x in 0..on.w {
            let a = ((top + y) * on.w + x) * 4;
            let b = (y * off.w + x) * 4;
            if on.rgba[a..a + 3] != off.rgba[b..b + 3] {
                differ += 1;
                first.get_or_insert((x, y));
            }
        }
    }
    match first {
        None => Ok(format!(
            "the {}x{rows} band under the Deck (rows {top}..{}) is the band without it, \
             pixel for pixel",
            on.w,
            top + rows
        )),
        Some((x, y)) => Err(format!(
            "{differ} of {} band pixels under the Deck differ from the band without it; \
             first at ({x},{y}) of the band (row {} with the Deck)",
            on.w * rows,
            top + y
        )),
    }
}

/// A press on the strip under the Deck selects the first of two tabs.
fn click(s: &Session<'_>) -> Result<String, String> {
    let made = s.ctl(&["tab", "new"])?;
    let active = |r: &str| r.split_whitespace().nth(1).map(str::to_string);
    let second = active(&made).ok_or_else(|| format!("`tab new` answered {made}"))?;
    let r = (|| -> Result<String, String> {
        s.click_strip(FIRST_TAB_COL)?;
        let next = s.ctl(&["tab", "next"])?;
        if active(&next).as_deref() == Some(second.as_str()) {
            Ok(format!(
                "a press at strip column {FIRST_TAB_COL} selected the first of two tabs"
            ))
        } else {
            Err(format!(
                "a press at strip column {FIRST_TAB_COL} left the second tab selected \
                 (`tab next` answered {next})"
            ))
        }
    })();
    let _ = s.ctl(&["tab", "close"]);
    r
}

/// A window not whole cells, its frame held to the server's pixels but for
/// the Deck's rows, and its remainder bands each the frame's nearest pixel.
fn bands(s: &Session<'_>, caption: &Caption) -> Result<String, String> {
    let display = s.screen.display;
    let (how, d) = if caption.client {
        s.click_button(caption, "maximize")?;
        let c = s.caption_until(&|c| c.maximized)?;
        ("maximized through its button", (c, s.settled_dims()?))
    } else {
        let x = s
            .x_display()
            .ok_or("the window system frames the window, and it is not X11")?;
        let win = s
            .x_window(&x)
            .ok_or("xdotool found no window for aterm-gui's pid")?;
        let before = s.ctl(&["dims"])?;
        let out = s.x(
            &x,
            &["xdotool", "windowsize", "--sync", &win, "1000", "700"],
        );
        if !out.ok() {
            return Err(format!("xdotool windowsize: {}", out.tail(2)));
        }
        s.dims_until(&|r| r != before)?;
        (
            "sized 1000x700 by xdotool",
            (caption.clone(), s.settled_dims()?),
        )
    };
    let (c, dims) = d;
    let at = Dims::read(&dims)?;
    let banded = ["band_left", "band_right", "band_top", "band_bottom"]
        .iter()
        .any(|k| proof::field(&dims, k).is_some_and(|v| v != "0"));
    let held = s
        .gui
        .held_to_server_but(display, &s.results, "-bands", at.deck_rows());
    if caption.client {
        let restored = s
            .click_button(&c, "restore")
            .and_then(|()| s.caption_until(&|c| !c.maximized));
        restored.map_err(|e| format!("restore: {e}"))?;
    }
    let held = held?;
    if !banded {
        return Err(format!(
            "{how}, the window is whole cells: no band to hold ({}); {held}",
            super::dims_summary(&dims)
        ));
    }
    Ok(format!(
        "{how} ({}x{} surface): {held}; the Deck's rows {}..{} left out",
        at.surface.0,
        at.surface.1,
        at.deck_y,
        at.deck_y + at.deck_h
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: usize, h: usize, paint: impl Fn(usize, usize) -> [u8; 4]) -> Frame {
        let mut rgba = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                rgba.extend_from_slice(&paint(x, y));
            }
        }
        Frame { rgba, w, h }
    }

    #[test]
    fn the_band_under_the_deck_is_held_to_the_band_without_it() {
        let band = |x: usize, y: usize| {
            if (1..3).contains(&x) && (1..3).contains(&y) {
                [200, 200, 200, 255]
            } else {
                [40, 40, 40, 255]
            }
        };
        let off = frame(5, 8, band);
        let deck = |x: usize, y: usize| {
            if y < 3 {
                [90, 0, 0, 255]
            } else {
                band(x, y - 3)
            }
        };
        let on = frame(5, 11, deck);
        let held = band_held(&on, 3, &off, 4).expect("the band is under the Deck");
        assert!(
            held.contains("5x4 band under the Deck (rows 3..7)"),
            "{held}"
        );
        let over = frame(5, 11, |x, y| {
            if (1..4).contains(&y) {
                [90, 0, 0, 255]
            } else {
                band(x, y.saturating_sub(3))
            }
        });
        let why = band_held(&over, 3, &off, 4).unwrap_err();
        assert!(why.starts_with("5 of 20 band pixels"), "{why}");
        assert!(band_held(&on, 2, &off, 4).is_err());
        assert!(band_held(&on, 8, &off, 4).is_err(), "past the frame");
    }

    #[test]
    fn the_decks_height_is_its_points_at_the_scale_at_the_heads_top() {
        let dims = |scale: &str, head: u32, deck_y: u32, deck_h: u32| {
            Dims::read(&format!(
                "OK 24x80 scale={scale} cell_w=9 cell_h=20 pad=12 pad_top=2 head={head} \
                 tab_rows=1 deck_y={deck_y} deck_h={deck_h} frame_w=744 surface_w=744 \
                 surface_h=590"
            ))
            .expect("dims")
        };
        assert!(head(&dims("1.00", 76, 0, 62)).is_ok());
        assert!(head(&dims("1.50", 114, 0, 93)).is_ok());
        assert!(head(&dims("2.00", 152, 0, 124)).is_ok());
        assert!(head(&dims("1.00", 76, 14, 62)).is_err());
        assert!(head(&dims("2.00", 152, 0, 62)).is_err());
        assert!(head(&dims("1.00", 14, 0, 0)).is_err());
        let d = dims("1.00", 76, 0, 62);
        assert_eq!(d.band_foot(), 98);
        assert_eq!(d.deck_rows(), 0..62);
        assert!(Dims::read("OK 24x80 scale=1.00").is_err());
    }
}
