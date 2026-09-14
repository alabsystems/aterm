// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TEMPORARY DIAGNOSTIC (2026-09-13, WASM host analyst) — the alab.systems
//! /terminal page, headless: boot.js's key seam + alab-sh's line discipline +
//! aterm-wasm's render loop, driven against the REAL `EffectsPipeline`, so the
//! Rainbow Kitty v2 bed (`RenderInput::glow_under`) can be read per column
//! after every frame. Reproduces what the page does on keydown, Backspace,
//! Enter, arrows and Ctrl-A, and prints where the ribbon lands.
//!
//! The page's seam, verbatim from /assets/term/boot.js `userInput`:
//!   - a printable: `sh.write(bytes)` (echo buffered), THEN
//!     `term.note_typed_char(ch)`, THEN `flushShell()` → `process_str(echo)`.
//!   - any other key (Backspace/Enter/arrows/^A/^K/^U): `term.note_keystroke()`
//!     (TEXT-BLIND), `sh.write(bytes)`, `process_str(echo)`. No erase seam.
//!   - a rAF frame: `advance_effects(dt)`, `render()` → `effects.apply`.
//!
//! alab-sh's echoes, verbatim from /assets/term/alab-sh.js:
//!   - insert:    ch + (tail ? tail + CSI n D : "")
//!   - backspace: "\b" + CSI K + (tail ? tail + CSI n D : "")
//!   - moveTo:    CSI d C | CSI d D
//!   - ^K:        buf.length = cur; CSI K        ^U: moveTo(0); CSI K
//!   - Enter:     "\r\n" + output + prompt ("visitor@alab:~ ❯ ", 17 cols)
//!   - ArrowUp:   redrawLine = "\r" + CSI K + prompt + buf (+ CSI n D)
//!
//!   targo --unverified run -p aterm-effects --example wasm_host_echo -- <scenario>
//!   scenarios: type bs spaces left enter_up bs_mid bs_type enter_only blur all

use aterm_core::render::RenderInput;
use aterm_core::terminal::{Rgb, Terminal};
use aterm_effects::pipeline::EffectsPipeline;

const ROWS: u16 = 24;
const COLS: u16 = 80;
/// JetBrains Mono at 14 CSS px × dpr 2 (boot.js FONT_PX * devicePixelRatio).
static CELL: std::sync::OnceLock<(usize, usize)> = std::sync::OnceLock::new();
fn cell() -> (usize, usize) {
    *CELL.get_or_init(|| {
        let g = |k: &str, d: usize| {
            std::env::var(k)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(d)
        };
        (g("RK_CW", 17), g("RK_CH", 37))
    })
}
#[allow(non_snake_case)]
fn CW() -> usize {
    cell().0
}
#[allow(non_snake_case)]
fn CH() -> usize {
    cell().1
}
const FRAME_MS: f64 = 1000.0 / 60.0;
const KEY_MS: f64 = 120.0;
const PROMPT: &str = "visitor@alab:~ \u{276f} ";
const PROMPT_W: usize = 17;
const CSI: &str = "\x1b[";

/// alab-sh's single-row line editor (the parts that move the caret).
struct Sh {
    buf: Vec<char>,
    cur: usize,
    out: String,
    history: Vec<String>,
}

impl Sh {
    fn new() -> Self {
        Self {
            buf: Vec::new(),
            cur: 0,
            out: String::new(),
            history: Vec::new(),
        }
    }
    fn out(&mut self, s: &str) {
        self.out.push_str(s);
    }
    fn prompt(&mut self) {
        self.out(&format!("\x1b]133;A\x07{PROMPT}\x1b]133;B\x07"));
        self.buf.clear();
        self.cur = 0;
    }
    fn tail(&self) -> String {
        self.buf[self.cur..].iter().collect()
    }
    fn insert(&mut self, ch: char) {
        if PROMPT_W + self.buf.len() >= usize::from(COLS) - 1 {
            return;
        }
        self.buf.insert(self.cur, ch);
        self.cur += 1;
        let tail = self.tail();
        let mut s = String::new();
        s.push(ch);
        if !tail.is_empty() {
            s.push_str(&format!("{tail}{CSI}{}D", tail.chars().count()));
        }
        self.out(&s);
    }
    fn move_to(&mut self, ix: usize) {
        if ix == self.cur {
            return;
        }
        let d = ix as i64 - self.cur as i64;
        let s = if d > 0 {
            format!("{CSI}{d}C")
        } else {
            format!("{CSI}{}D", -d)
        };
        self.out(&s);
        self.cur = ix;
    }
    fn backspace(&mut self) {
        if self.cur == 0 {
            return;
        }
        self.cur -= 1;
        self.buf.remove(self.cur);
        let tail = self.tail();
        let mut s = format!("\x08{CSI}K");
        if !tail.is_empty() {
            s.push_str(&format!("{tail}{CSI}{}D", tail.chars().count()));
        }
        self.out(&s);
    }
    fn kill_to_end(&mut self) {
        self.buf.truncate(self.cur);
        self.out(&format!("{CSI}K"));
    }
    fn kill_line(&mut self) {
        self.move_to(0);
        self.buf.clear();
        self.out(&format!("{CSI}K"));
    }
    fn enter(&mut self) {
        let line: String = self.buf.iter().collect();
        self.out("\r\n");
        if !line.trim().is_empty() {
            self.history.push(line.clone());
            self.out(&format!("alab-sh: {line}: command not found\r\n"));
        }
        self.prompt();
    }
    fn redraw_line(&mut self) {
        let text: String = self.buf.iter().collect();
        self.out(&format!("\r{CSI}K{PROMPT}{text}"));
        if self.cur < self.buf.len() {
            self.out(&format!("{CSI}{}D", self.buf.len() - self.cur));
        }
    }
    fn history_up(&mut self) {
        if let Some(last) = self.history.last().cloned() {
            self.buf = last.chars().collect();
            self.cur = self.buf.len();
            self.redraw_line();
        }
    }
    /// The engine's VT bytes for a key, decoded the way alab-sh's parser does.
    fn write(&mut self, bytes: &str) {
        let mut it = bytes.chars().peekable();
        while let Some(c) = it.next() {
            match c {
                '\x1b' => {
                    // CSI <final> only (arrows/Home/End) — enough for this probe.
                    let _ = it.next(); // '['
                    let mut params = String::new();
                    let mut fin = ' ';
                    for c2 in it.by_ref() {
                        if ('@'..='~').contains(&c2) {
                            fin = c2;
                            break;
                        }
                        params.push(c2);
                    }
                    match fin {
                        'A' => self.history_up(),
                        'C' => self.move_to((self.cur + 1).min(self.buf.len())),
                        'D' => self.move_to(self.cur.saturating_sub(1)),
                        'H' => self.move_to(0),
                        'F' => self.move_to(self.buf.len()),
                        _ => {}
                    }
                }
                '\r' | '\n' => self.enter(),
                '\x7f' | '\x08' => self.backspace(),
                '\x01' => self.move_to(0),
                '\x05' => self.move_to(self.buf.len()),
                '\x0b' => self.kill_to_end(),
                '\x15' => self.kill_line(),
                c if (c as u32) < 0x20 => {}
                c => self.insert(c),
            }
        }
    }
}

/// boot.js + aterm-wasm's `AtermTerminal`, headless.
struct Page {
    term: Terminal,
    input: RenderInput,
    p: EffectsPipeline,
    sh: Sh,
    t_ms: f64,
    verbose: bool,
}

impl Page {
    fn boot(verbose: bool) -> Self {
        let mut term = Terminal::new(ROWS, COLS);
        let rgb = |c: u32| Rgb {
            r: ((c >> 16) & 0xff) as u8,
            g: ((c >> 8) & 0xff) as u8,
            b: (c & 0xff) as u8,
        };
        // boot.js THEME
        term.set_default_foreground(rgb(0xe6e4dd));
        term.set_default_background(rgb(0x0c0d0c));
        term.set_default_cursor_color(Some(rgb(0x5bb8c4)));
        term.set_default_selection_background(Some(rgb(0x1e4247)));
        let input = term.cell_frame(usize::from(ROWS), usize::from(COLS));
        let mut p = EffectsPipeline::new();
        // boot.js: effects.glow(true, "rainbow kitty"); effects.pet(true); effects.sparkle(true)
        p.set_cursor_glow(
            true,
            "rainbow kitty",
            None,
            None,
            450,
            36,
            0.85,
            1.2,
            true,
            0x5bb8c4,
        );
        p.set_cursor_pet(true, 0x5eed);
        p.set_focused(true);
        p.set_effects_visibility("focused");
        let mut page = Self {
            term,
            input,
            p,
            sh: Sh::new(),
            t_ms: 0.0,
            verbose,
        };
        page.sh.out(&format!("{CSI}?2004h"));
        page.sh.prompt();
        page.flush();
        page.frame("boot");
        page.idle(6);
        page
    }

    fn flush(&mut self) {
        if !self.sh.out.is_empty() {
            let s = std::mem::take(&mut self.sh.out);
            self.term.process(s.as_bytes());
        }
    }

    /// boot.js `userInput(bytes, text)`.
    fn user_input(&mut self, bytes: &str, text: Option<&str>, label: &str) {
        if text.is_none() {
            self.p.note_keystroke();
        }
        self.sh.write(bytes);
        if let Some(t) = text {
            for ch in t.chars() {
                let ok = self.p.note_committed_char(&mut self.term, &self.input, ch);
                debug_assert!(ok);
            }
        }
        self.flush();
        // The keydown handler's flushShell() → requestFrame(): the next rAF.
        let frames = (KEY_MS / FRAME_MS).round() as usize;
        for i in 0..frames {
            let tag = if i == 0 { label } else { "" };
            self.frame(tag);
        }
    }

    fn key_text(&mut self, ch: char) {
        let s = ch.to_string();
        self.user_input(&s, Some(&s), &format!("key {ch:?}"));
    }
    fn key(&mut self, name: &str) {
        let bytes = match name {
            "Backspace" => "\x7f",
            "Enter" => "\r",
            "ArrowLeft" => "\x1b[D",
            "ArrowRight" => "\x1b[C",
            "ArrowUp" => "\x1b[A",
            "Home" => "\x1b[H",
            "End" => "\x1b[F",
            "C-a" => "\x01",
            "C-e" => "\x05",
            "C-k" => "\x0b",
            "C-u" => "\x15",
            other => panic!("unknown key {other}"),
        };
        self.user_input(bytes, None, &format!("key {name}"));
    }

    fn idle(&mut self, frames: usize) {
        for _ in 0..frames {
            self.frame("");
        }
    }

    /// One rAF: advance, refill the snapshot, apply — then read the bed.
    fn frame(&mut self, label: &str) {
        self.t_ms += FRAME_MS;
        self.p.advance(FRAME_MS);
        self.term
            .cell_frame_into(&mut self.input, usize::from(ROWS), usize::from(COLS));
        let _fp = self.p.apply(&mut self.term, &mut self.input, CW(), CH());
        if !label.is_empty() || self.verbose {
            self.dump(label);
        }
    }

    /// Per-column bed report: the caret row first, then every other row
    /// that carries bed quads.
    fn dump(&self, label: &str) {
        let crow = self.input.cursor_row as u16;
        self.dump_row(label, crow, true);
        let mut rows: Vec<u16> = self.input.glow_under.iter().map(|q| q.row).collect();
        rows.sort_unstable();
        rows.dedup();
        for r in rows {
            if r != crow {
                self.dump_row("  (other row)", r, false);
            }
        }
    }

    fn dump_row(&self, label: &str, row: u16, is_caret_row: bool) {
        let col = self.input.cursor_col;
        let mut covered = vec![0u32; usize::from(COLS) * CW()];
        let mut colour_at = vec![None::<u32>; usize::from(COLS)];
        let mut spine_y = None::<u16>;
        // The bed's densest row is the spine's neighbourhood — pick the y with
        // most quads on this grid row and sample colours there.
        let mut per_y = std::collections::BTreeMap::<u16, usize>::new();
        for q in self.input.glow_under.iter().filter(|q| q.row == row) {
            *per_y.entry(q.y).or_default() += usize::from(q.w);
            for x in q.x..q.x.saturating_add(q.w) {
                if let Some(c) = covered.get_mut(usize::from(x)) {
                    *c += 1;
                }
            }
        }
        if let Some((y, _)) = per_y.iter().max_by_key(|(_, n)| **n) {
            spine_y = Some(*y);
            for q in self
                .input
                .glow_under
                .iter()
                .filter(|q| q.row == row && q.y == *y)
            {
                let c = usize::from(q.x) / CW() + usize::from(q.w / 2) / CW();
                if c < usered(&colour_at) {
                    colour_at[c].get_or_insert(q.color);
                }
            }
        }
        let mut line = String::new();
        let mut first = None;
        let mut last = None;
        for c in 0..usize::from(COLS) {
            let px = &covered[c * CW()..(c + 1) * CW()];
            let n = px.iter().filter(|v| **v > 0).count();
            let ch = if n == 0 {
                '.'
            } else if n == CW() {
                '#'
            } else {
                'p'
            };
            if n > 0 {
                first.get_or_insert(c);
                last = Some(c);
            }
            line.push(ch);
        }
        // px slits: uncovered px between the first and last covered px.
        let mut slits = Vec::new();
        if let (Some(f), Some(l)) = (first, last) {
            let x0 = f * CW();
            let x1 = (l + 1) * CW();
            let mut x = x0;
            while x < x1 {
                if covered[x] == 0 {
                    let s = x;
                    while x < x1 && covered[x] == 0 {
                        x += 1;
                    }
                    slits.push(format!(
                        "x[{s},{x}) col {}..{} ({} px)",
                        s / CW(),
                        (x - 1) / CW(),
                        x - s
                    ));
                } else {
                    x += 1;
                }
            }
        }
        let text: String = self.input.cells[usize::from(row)]
            .iter()
            .map(|c| c.ch)
            .collect::<String>()
            .trim_end()
            .to_string();
        let span = match (first, last) {
            (Some(f), Some(l)) => format!("cols {f}..={l}"),
            _ => "none".to_string(),
        };
        println!(
            "t={:6.0}ms {:<14} row={row} caret=({},{col}) bed={span} quads={} text={text:?}",
            self.t_ms,
            label,
            self.input.cursor_row,
            self.input.glow_under.len()
        );
        println!(
            "            {}",
            &line[..(last.unwrap_or(40) + 8).min(usize::from(COLS))]
        );
        if is_caret_row {
            println!("            {}^caret", " ".repeat(col));
        }
        if !slits.is_empty() {
            println!(
                "            SLITS(px uncovered at spine band): {}",
                slits.join(" | ")
            );
        }
        // BAND-HEIGHT NOTCHES and SPINE-LUMA DIPS per px column: a "dark slit"
        // that is not an uncovered column shows up as a px column whose
        // covered height is below both neighbours', or whose spine colour is
        // much darker than both neighbours'.
        if let (Some(f), Some(l)) = (first, last) {
            let x0 = f * CW();
            let x1 = (l + 1) * CW();
            let luma = |c: u32| -> f32 {
                let r = ((c >> 16) & 0xff) as f32;
                let g = ((c >> 8) & 0xff) as f32;
                let b = (c & 0xff) as f32;
                0.2126 * r + 0.7152 * g + 0.0722 * b
            };
            let mut spine_l = vec![None::<f32>; usize::from(COLS) * CW()];
            if let Some(y) = spine_y {
                for q in self
                    .input
                    .glow_under
                    .iter()
                    .filter(|q| q.row == row && q.y == y)
                {
                    for x in q.x..q.x.saturating_add(q.w) {
                        if let Some(slot) = spine_l.get_mut(usize::from(x)) {
                            *slot = Some(luma(q.color));
                        }
                    }
                }
            }
            let mut notches = Vec::new();
            let mut dips = Vec::new();
            for x in (x0 + 1)..(x1 - 1) {
                let (hl, h, hr) = (covered[x - 1], covered[x], covered[x + 1]);
                if h + 1 < hl.min(hr) {
                    notches.push(format!("x{x}(col{}) h={h} vs {hl}/{hr}", x / CW()));
                }
                if let (Some(a), Some(b), Some(c)) = (spine_l[x - 1], spine_l[x], spine_l[x + 1])
                    && b < 0.6 * a.min(c)
                {
                    dips.push(format!("x{x}(col{}) L={b:.0} vs {a:.0}/{c:.0}", x / CW()));
                }
            }
            if !notches.is_empty() {
                println!("            HEIGHT NOTCHES: {}", notches.join(" | "));
            }
            if !dips.is_empty() {
                println!("            SPINE LUMA DIPS: {}", dips.join(" | "));
            }
            // per-column band height at the column centre + edges (cols f..=l)
            let prof: Vec<String> = (f..=l)
                .map(|c| {
                    let xl = c * CW();
                    let xc = c * CW() + CW() / 2;
                    let xr = (c + 1) * CW() - 1;
                    format!("{c}:{}/{}/{}", covered[xl], covered[xc], covered[xr])
                })
                .collect();
            println!(
                "            band height (left/centre/right px of each col): {}",
                prof.join(" ")
            );
        }
        if let Some(y) = spine_y {
            let hues: Vec<String> = colour_at
                .iter()
                .enumerate()
                .filter_map(|(c, h)| h.map(|h| format!("{c}:{h:06x}")))
                .collect();
            println!("            spine y={y} hue/col: {}", hues.join(" "));
        }
    }
}

fn usered(v: &[Option<u32>]) -> usize {
    v.len()
}

fn scenario(name: &str, verbose: bool) {
    println!("=== scenario {name} (cw {} ch {}) ===", CW(), CH());
    let mut pg = Page::boot(verbose);
    match name {
        "type" => {
            for ch in "asdf".chars() {
                pg.key_text(ch);
            }
            pg.idle(60);
            pg.frame("idle +1s");
        }
        "bs" => {
            for ch in "asdfghjklqw".chars() {
                pg.key_text(ch);
            }
            for _ in 0..7 {
                pg.key("Backspace");
            }
            pg.key("C-a");
            pg.idle(30);
            pg.frame("idle +0.5s");
        }
        "spaces" => {
            for ch in "asdf       ".chars() {
                pg.key_text(ch);
            }
            pg.key("C-a");
            pg.idle(30);
            pg.frame("idle +0.5s");
        }
        "left" => {
            for ch in "asdf".chars() {
                pg.key_text(ch);
            }
            for _ in 0..4 {
                pg.key("ArrowLeft");
            }
            pg.idle(30);
            pg.frame("idle +0.5s");
        }
        "enter_up" => {
            for ch in "asdf".chars() {
                pg.key_text(ch);
            }
            pg.key("Enter");
            pg.key("ArrowUp");
            pg.key("C-a");
            pg.idle(30);
            pg.frame("idle +0.5s");
        }
        "bs_mid" => {
            for ch in "asdf".chars() {
                pg.key_text(ch);
            }
            pg.key("Backspace");
            pg.key("Backspace");
            for ch in "xy".chars() {
                pg.key_text(ch);
            }
            pg.idle(30);
            pg.frame("idle +0.5s");
        }
        "bs_type" => {
            for ch in "asdfghjklqw".chars() {
                pg.key_text(ch);
            }
            for _ in 0..7 {
                pg.key("Backspace");
            }
            for ch in "xyz".chars() {
                pg.key_text(ch);
            }
            pg.idle(30);
            pg.frame("idle +0.5s");
        }
        "enter_only" => {
            for ch in "asdf".chars() {
                pg.key_text(ch);
            }
            pg.key("Enter");
            for i in 1..=6 {
                pg.idle(15);
                pg.frame(&format!("idle +{}ms", i * 250));
            }
        }
        "blur" => {
            for ch in "asdf".chars() {
                pg.key_text(ch);
            }
            // window.blur → pushVisibility(): set_effects_focused(false) +
            // set_effects_visibility("visible_unfocused"), one frame, then focus back.
            pg.p.set_focused(false);
            pg.p.set_effects_visibility("visible_unfocused");
            pg.frame("blur");
            pg.p.set_focused(true);
            pg.p.set_effects_visibility("focused");
            pg.frame("refocus");
            pg.idle(6);
            pg.frame("refocus +100ms");
        }
        other => panic!("unknown scenario {other}"),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let verbose = args.iter().any(|a| a == "-v");
    let names: Vec<&str> = args
        .iter()
        .filter(|a| *a != "-v")
        .map(String::as_str)
        .collect();
    let all = [
        "type",
        "bs",
        "spaces",
        "left",
        "enter_up",
        "bs_mid",
        "bs_type",
        "enter_only",
        "blur",
    ];
    let run: Vec<&str> = if names.is_empty() || names == ["all"] {
        all.to_vec()
    } else {
        names
    };
    for n in run {
        scenario(n, verbose);
    }
}
