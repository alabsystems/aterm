// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **A COPY OF A LINE IS NOT WHERE THE LINE WENT — AT EVERY TIMING AND
//! HOST** (2026-09-25).
//!
//! The follow pass carries a band onto the row its text moved to
//! (`rk::witness::Witness::follow_runs`). Every law here is a screen on
//! which the text did NOT move — a line erased under an identical line, an
//! fzf list or completion popup or streamed row holding the line's glyphs at
//! its columns, vim's `yyp` — and its reading is `(followed, frames of 90 the
//! row no key wrote was lit)`, which must be `(0, 0)`. Each family sweeps a
//! grid (keys per frame, echo lag, erase key, pacing, the composed host's
//! dropped far read, pause before the erase) at the host seam of
//! `tests/trail_host/mod.rs`; a debug build runs each grid's spine
//! (`trail_host::sweep`).
//!
//! Each law names what 0.93.0 (`aa71f9319`, the shipped tree) read, as bad
//! cases out of the family's full grid, and the mechanism that holds it
//! now. Every family holds whole but for its stated LIMITS, each a rule over
//! the ids that says why (`trail_host::holds_but`) and each a case the
//! witness cannot tell from a real move (`rk::witness`'s module doc):
//!
//! * a copy that stood beside the line for less than `TWIN_MIN` (40 ms), or
//!   on one sampled frame only — under the no-pet style's idle pacing the
//!   frames come 110–130 ms apart — before the line was erased is the new
//!   row of a TORN REPAINT, which must follow
//!   (`tests/moves_are_followed.rs`); the same screens were bad on 0.93.0;
//! * a copy drawn in the SAME batch as the erase — fzf re-sorting an item
//!   that holds the query's text onto the row one above it — is, glyph for
//!   glyph, the line relocated;
//! * a strictly alternating flicker one row away never stands `TWIN_MIN`;
//! * a line MOVED beside an identical line and killed before the witness
//!   has seen the identical line stand `TWIN_MIN` beside its new row: a
//!   carried record starts over, and the copy one row off is not yet its
//!   twin
//!   (`the_limit_a_line_moved_beside_its_twin_and_killed_before_it_was_learned_lights_it`).

#[macro_use]
mod trail_host;

use aterm_effects::kitty_pet::PetBrain;
use std::time::Duration;
use trail_host::*;

// ===========================================================================
// a2 — ERASES UNDER A TWIN, AND COPIES THAT LAND BESIDE A LINE: `f1`…`f6`,
// `pre1` and the counterexamples `ce*`. One host: the scroll seam first,
// LOCK A (caret row not re-read), the tick; the pane columns noted; `far0`
// drops every far-row read. Keys: a group's presses 6 ms apart, 90 ms after
// the last event.
// ===========================================================================
mod a2 {
    use super::*;

    const ROWS: usize = 24;
    const COLS: usize = 80;

    pub struct Host {
        c: Core,
    }
    core_deref!(Host);

    impl Host {
        fn at_row(row: u16) -> Self {
            let mut c = Core::new(
                ROWS,
                COLS,
                8,
                16,
                Theme::Tokyo,
                Opts {
                    scroll_seam: true,
                    alt_rebaseline: true,
                    skip_caret: true,
                    honour_sync: false,
                    pane_columns: true,
                },
            );
            c.term.process(format!("\x1b[{};1H", row + 1).as_bytes());
            c.frame();
            Self { c }
        }

        fn idle(&mut self, ms: u64) {
            let end = self.now + Duration::from_millis(ms);
            while self.now < end {
                self.now += Duration::from_millis(16);
                self.frame();
            }
        }

        fn type_groups(&mut self, s: &str, groups: &[usize], lag: usize, suffix: &str) {
            let chars: Vec<char> = s.chars().collect();
            let mut i = 0;
            let mut gi = 0;
            while i < chars.len() {
                let g = groups.get(gi).copied().unwrap_or(1).max(1);
                gi += 1;
                let end = (i + g).min(chars.len());
                self.now += Duration::from_millis(90);
                let mut t = self.now;
                for _ in i..end {
                    self.glow.note_typed_cells(t, 1);
                    t += Duration::from_millis(6);
                }
                self.now = t;
                for _ in 0..lag {
                    self.now += Duration::from_millis(16);
                    self.frame();
                }
                let echo: String = chars[i..end].iter().collect();
                self.now += Duration::from_millis(1);
                self.term.process(format!("{echo}{suffix}").as_bytes());
                self.frame();
                i = end;
            }
        }

        fn type_str(&mut self, s: &str) {
            self.type_groups(s, &[], 0, "");
        }

        fn program(&mut self, bytes: &[u8]) {
            self.now += Duration::from_millis(16);
            self.term.process(bytes);
            self.frame();
        }

        fn kill(&mut self, bytes: &[u8]) {
            self.now += Duration::from_millis(90);
            let now = self.now;
            self.glow.note_kill(now, true);
            self.term.process(bytes);
            self.frame();
        }

        /// `(followed, retired)` since `m`, then the frames `row` is lit over
        /// the next ~1.4 s.
        fn verdict(&mut self, m: (u64, u64), row: u16) -> Outcome {
            let (f, r) = self.counts();
            let cells = self.cells(row);
            let mut n = 0;
            for _ in 0..90 {
                self.idle(16);
                n += usize::from(self.lit(row));
            }
            Outcome::ff(f - m.0, r - m.1, n).with_note(format!("cells_after={cells:?}"))
        }
    }

    // FAMILY 1: a line erased under its twin one row up, at every timing.
    fn erase_under_twin(
        cmd: &str,
        groups: &[usize],
        lag: usize,
        erase: u8,
        far_ok: bool,
    ) -> Outcome {
        let mut h = Host::at_row(4);
        h.far_ok = far_ok;
        h.program(format!("$ {cmd}\r\n$ ").as_bytes());
        h.type_groups(cmd, groups, lag, "");
        h.idle(64);
        let m = h.counts();
        let n = cmd.chars().count();
        match erase {
            0 => h.program(b"\r$ \x1b[K"),
            1 => {
                let bs = "\x08".repeat(n);
                h.kill(format!("{bs}\x1b[K").as_bytes());
            }
            _ => {
                let last = cmd.rsplit(' ').next().unwrap_or(cmd).chars().count();
                let bs = "\x08".repeat(last);
                h.kill(format!("{bs}\x1b[K").as_bytes());
            }
        }
        h.verdict(m, 4)
    }

    // FAMILY 2: the twin APPEARS after the line's first keys were armed.
    fn fzf_loading(k0: usize, groups: &[usize], far_ok: bool) -> Outcome {
        let q = "src/li";
        let mut h = Host::at_row(21);
        h.far_ok = far_ok;
        h.program(b"\x1b[22;1H> ");
        let redraw = "\x1b7\x1b[19;1H  tests/a.rs\x1b[K\x1b[20;1H  src/main.rs\x1b[K\x1b[21;1H> src/lib.rs\x1b[K\x1b8";
        h.type_groups(&q[..k0], groups, 0, "");
        h.program(redraw.as_bytes());
        h.type_groups(&q[k0..], &[], 0, redraw);
        h.idle(64);
        let m = h.counts();
        h.kill(format!("\r\x1b[2C\x1b[K{redraw}").as_bytes());
        h.verdict(m, 20)
    }

    fn fzf_reverse_loading(k0: usize, far_ok: bool) -> Outcome {
        let q = "src/li";
        let mut h = Host::at_row(10);
        h.far_ok = far_ok;
        h.program(b"\x1b[11;1H> ");
        let redraw = "\x1b7\x1b[12;1H> src/lib.rs\x1b[K\x1b[13;1H  src/main.rs\x1b[K\x1b[14;1H  tests/a.rs\x1b[K\x1b8";
        h.type_groups(&q[..k0], &[], 0, "");
        h.program(redraw.as_bytes());
        h.type_groups(&q[k0..], &[], 0, redraw);
        h.idle(64);
        let m = h.counts();
        h.kill(format!("\r\x1b[2C\x1b[K{redraw}").as_bytes());
        h.verdict(m, 11)
    }

    fn cmp_menu(k0: usize, stays_frames: usize) -> Outcome {
        let word = "println";
        let mut h = Host::at_row(5);
        h.program(b"\x1b[5;1Hfn main() {\x1b[6;1H    \x1b[7;1H}\x1b[6;5H");
        let menu = "\x1b7\x1b[7;5Hprintln!      Macro\x1b[K\x1b8";
        let restore = "\x1b7\x1b[7;1H}\x1b[K\x1b8";
        h.type_groups(&word[..k0], &[], 0, "");
        h.type_groups(&word[k0..], &[], 0, menu);
        h.idle(48);
        let m = h.counts();
        h.kill(b"\x1b[6;5H\x1b[K");
        for _ in 0..stays_frames {
            h.idle(16);
        }
        h.program(restore.as_bytes());
        h.verdict(m, 6)
    }

    fn twin_drawn_late(k0: usize, erase: u8) -> Outcome {
        let cmd = "git status";
        let mut h = Host::at_row(4);
        h.program(b"\r\n$ ");
        h.type_groups(&cmd[..k0], &[], 0, "");
        h.program(format!("\x1b7\x1b[5;1H$ {cmd}\x1b8").as_bytes());
        h.type_str(&cmd[k0..]);
        h.idle(64);
        let m = h.counts();
        if erase == 0 {
            h.program(b"\r$ \x1b[K");
        } else {
            h.kill(format!("{}\x1b[K", "\x08".repeat(cmd.len())).as_bytes());
        }
        h.verdict(m, 4)
    }

    // FAMILY 3: full witness-row budget, 8+ cohorts resident.
    fn full_budget_twin(bands: &[u16], groups: &[usize], lag: usize, far_ok: bool) -> Outcome {
        let mut h = Host::at_row(0);
        h.far_ok = far_ok;
        for &row in bands {
            h.program(format!("\x1b[{};1H", row + 1).as_bytes());
            h.type_groups("ab", &[2], 0, "");
        }
        h.program(b"\x1b[21;1H$ git status\r\n$ ");
        h.type_groups("git status", groups, lag, "");
        h.idle(48);
        let m = h.counts();
        h.program(b"\r$ \x1b[K");
        h.verdict(m, 20)
    }

    // FAMILY 4: scroll / band edges.
    fn bottom_edge_then_scroll(variant: u8) -> Outcome {
        let cmd = "git status";
        let last = (ROWS - 1) as u16;
        let mut h = Host::at_row(last);
        h.program(b"$ ");
        h.type_groups(cmd, &[2], 0, "");
        h.idle(48);
        let m = h.counts();
        match variant {
            0 => {
                h.program(format!("\r\n[1]  + done       sleep 1\r\n$ {cmd}").as_bytes());
                h.idle(32);
                h.kill(format!("{}\x1b[K", "\x08".repeat(cmd.len())).as_bytes());
            }
            _ => {
                h.program(b"\r\n");
                h.idle(32);
                h.program(
                    format!(
                        "$ {cmd}\x1b[{};1H\x1b[K\x1b[{};{}H",
                        last,
                        last + 1,
                        cmd.len() + 3
                    )
                    .as_bytes(),
                );
            }
        }
        h.verdict(m, last)
    }

    fn vim_dup_below_then_s() -> Outcome {
        let mut h = Host::at_row(3);
        h.program(b"\x1b[1;1Hfn f() {\x1b[2;1H    let a = 1;\x1b[3;1H    \x1b[4;1H}\x1b[5;1H~\x1b[6;1H~\x1b[3;5H");
        h.type_groups("let total = 0;", &[1], 0, "");
        h.idle(64);
        h.program(b"\x1b[3;18H");
        h.program(b"\x1b[4;1H\x1b[L    let total = 0;\x1b[4;5H");
        h.idle(32);
        h.program(b"\x1b[3;5H\x1b[K");
        // This case counts `followed` from the start (f0 = 0).
        h.verdict((0, 0), 3)
    }

    fn scroll_and_erase(groups: &[usize]) -> Outcome {
        let cmd = "git status";
        let last = (ROWS - 1) as u16;
        let mut h = Host::at_row(last - 1);
        h.program(format!("$ {cmd}\r\n$ ").as_bytes());
        h.type_groups(cmd, groups, 0, "");
        h.idle(48);
        let m = h.counts();
        h.program(b"\r\n\x1b[A\r$ \x1b[K");
        h.verdict(m, last - 2)
    }

    // FAMILY 5: alt screen enter / exit.
    fn alt_enter_exit(groups: &[usize]) -> Outcome {
        let cmd = "git status";
        let mut h = Host::at_row(4);
        h.program(format!("$ {cmd}\r\n$ ").as_bytes());
        h.type_groups(cmd, groups, 0, "");
        h.idle(48);
        let m = h.counts();
        h.program(b"\x1b[?1049h\x1b[H\x1b[2J");
        h.program(format!("\x1b[5;1H$ {cmd}").as_bytes());
        h.idle(32);
        h.program(b"\x1b[?1049l");
        h.program(b"\r$ \x1b[K");
        h.verdict(m, 4)
    }

    // FAMILY 6: a streamed line above quotes the composer.
    fn streaming_above(k_quote_at: usize, gap: u16) -> Outcome {
        let text = "please fix the tests";
        let mut h = Host::at_row(20);
        let top = 20 - gap;
        h.program(b"\x1b[21;1H> ");
        let stream = |i: usize| {
            format!(
                "\x1b7\x1b[{};1H\x1b[2K  thinking {}\x1b8",
                top + 1,
                ".".repeat(i % 7)
            )
        };
        let chars: Vec<char> = text.chars().collect();
        for (i, ch) in chars.iter().enumerate() {
            h.type_groups(&ch.to_string(), &[1], 0, &stream(i));
        }
        for i in 0..k_quote_at {
            h.program(stream(i).as_bytes());
        }
        h.program(format!("\x1b7\x1b[{};1H\x1b[2K  {text}\x1b8", top + 1).as_bytes());
        let m = h.counts();
        h.kill(b"\r\x1b[2C\x1b[K");
        h.verdict(m, top)
    }

    // The counterexamples: shapes not already in the grids above.
    fn ce2_fzf_info_line_ssh_batch() -> Outcome {
        let q = "src/li";
        let mut h = Host::at_row(21);
        h.program(b"\x1b[22;1H> ");
        let list = "\x1b7\x1b[18;1H  tests/a.rs\x1b[K\x1b[19;1H  src/main.rs\x1b[K\x1b[20;1H> src/lib.rs\x1b[K\x1b[21;1H  3/3\x1b[K\x1b8";
        h.type_groups(&q[..2], &[1], 0, "");
        h.program(list.as_bytes());
        h.type_groups(&q[2..], &[1], 0, list);
        h.idle(64);
        let m = h.counts();
        h.now += Duration::from_millis(90);
        let t_kill = h.now;
        h.glow.note_kill(t_kill, true);
        h.glow
            .note_typed_cells(t_kill + Duration::from_millis(60), 1);
        h.now = t_kill + Duration::from_millis(120);
        h.term.process(format!("\r\x1b[2C\x1b[Ks{list}").as_bytes());
        h.frame();
        h.verdict(m, 19)
    }

    fn pre1_vim_yyp_then_s() -> Outcome {
        let mut h = Host::at_row(3);
        h.program(b"\x1b[1;1Hfn f() {\x1b[2;1H    let a = 1;\x1b[3;1H    \x1b[4;1H}\x1b[5;1H~\x1b[6;1H~\x1b[3;5H");
        h.type_groups("let total = 0;", &[1], 0, "");
        h.idle(64);
        let m = h.counts();
        h.program(b"\x1b[4;1H\x1b[L    let total = 0;\x1b[4;5H");
        h.idle(32);
        h.program(b"\x1b[3;5H\x1b[K");
        h.verdict(m, 3)
    }

    fn ce1d_first_item_shares_only_the_directory() -> Outcome {
        let q = "src/li";
        let mut h = Host::at_row(21);
        h.program(b"\x1b[22;1H> ");
        let filtered = "\x1b7\x1b[19;1H  tests/a.rs\x1b[K\x1b[20;1H  src/main.rs\x1b[K\x1b[21;1H> src/lib.rs\x1b[K\x1b8";
        let full = "\x1b7\x1b[19;1H  tests/a.rs\x1b[K\x1b[20;1H  src/lib.rs\x1b[K\x1b[21;1H> src/main.rs\x1b[K\x1b8";
        h.type_groups(&q[..2], &[1], 0, "");
        h.program(filtered.as_bytes());
        h.type_groups(&q[2..], &[1], 0, filtered);
        h.idle(64);
        let m = h.counts();
        h.kill(format!("\r\x1b[2C\x1b[K{full}").as_bytes());
        h.verdict(m, 20)
    }

    pub fn cases(v: &mut Vec<Case>) {
        for cmd in [
            "git status",
            "cd ..",
            "cargo test -p aterm-effects",
            "ls -la",
        ] {
            let all_groups = [
                vec![1usize],
                vec![2],
                vec![3],
                vec![4],
                vec![1, 2],
                vec![2; 14],
                vec![3; 10],
                vec![40],
            ];
            for groups in sweep(&all_groups, &[vec![1], vec![2], vec![4], vec![40]]) {
                for lag in sweep(&[0usize, 1, 2, 3], &[0, 3]) {
                    for erase in [0u8, 1, 2] {
                        for far_ok in [true, false] {
                            let g = groups.clone();
                            case(
                                v,
                                format!(
                                    "a2.f1.twin_erase/{}/g{}/lag{lag}/e{erase}/far{}",
                                    slug(cmd),
                                    gstr(&groups),
                                    u8::from(far_ok)
                                ),
                                FF,
                                move || erase_under_twin(cmd, &g, lag, erase, far_ok),
                            );
                        }
                    }
                }
            }
        }
        for k0 in [1usize, 2, 3, 4] {
            for groups in [vec![1usize], vec![2], vec![4]] {
                for far_ok in [true, false] {
                    let g = groups.clone();
                    case(
                        v,
                        format!(
                            "a2.f2.fzf_loading/k{k0}/g{}/far{}",
                            gstr(&groups),
                            u8::from(far_ok)
                        ),
                        FF,
                        move || fzf_loading(k0, &g, far_ok),
                    );
                }
            }
        }
        for k0 in [1usize, 2, 3, 4] {
            for far_ok in [true, false] {
                case(
                    v,
                    format!("a2.f2b.fzf_reverse_loading/k{k0}/far{}", u8::from(far_ok)),
                    FF,
                    move || fzf_reverse_loading(k0, far_ok),
                );
            }
        }
        for k0 in [1usize, 2, 3] {
            for stays in [0usize, 1, 3] {
                case(
                    v,
                    format!("a2.f2c.cmp_menu/k{k0}/stay{stays}"),
                    FF,
                    move || cmp_menu(k0, stays),
                );
            }
        }
        for k0 in [1usize, 2, 3, 5] {
            for erase in [0u8, 1] {
                case(
                    v,
                    format!("a2.f2d.twin_drawn_late/k{k0}/e{erase}"),
                    FF,
                    move || twin_drawn_late(k0, erase),
                );
            }
        }
        let sets: [&'static [u16]; 4] = [
            &[1, 3, 5, 7, 9, 11, 13, 15, 17],
            &[2, 5, 8, 11, 14, 17],
            &[16, 17, 18, 19, 22, 23],
            &[18, 19, 22, 23, 0, 2, 4, 6, 8, 10],
        ];
        for (si, bands) in sets.into_iter().enumerate() {
            for groups in [vec![1usize], vec![2], vec![3]] {
                for lag in sweep(&[0usize, 2], &[2]) {
                    for far_ok in [true, false] {
                        let g = groups.clone();
                        case(
                            v,
                            format!(
                                "a2.f3.full_budget/set{si}/g{}/lag{lag}/far{}",
                                gstr(&groups),
                                u8::from(far_ok)
                            ),
                            FF,
                            move || full_budget_twin(bands, &g, lag, far_ok),
                        );
                    }
                }
            }
        }
        for variant in [0u8, 1] {
            case(
                v,
                format!("a2.f4.bottom_edge_then_scroll/v{variant}"),
                FF,
                move || bottom_edge_then_scroll(variant),
            );
        }
        case(
            v,
            "a2.f4b.vim_dup_below_esc_then_s",
            FF,
            vim_dup_below_then_s,
        );
        for groups in [vec![1usize], vec![2]] {
            let g = groups.clone();
            case(
                v,
                format!("a2.f4c.scroll_and_erase_one_frame/g{}", gstr(&groups)),
                FF,
                move || scroll_and_erase(&g),
            );
        }
        for groups in [vec![1usize], vec![2]] {
            let g = groups.clone();
            case(
                v,
                format!("a2.f5.alt_enter_exit/g{}", gstr(&groups)),
                FF,
                move || alt_enter_exit(&g),
            );
        }
        for gap in [1u16, 2] {
            for q in [0usize, 3] {
                case(
                    v,
                    format!("a2.f6.streaming_above_quotes/gap{gap}/q{q}"),
                    FF,
                    move || streaming_above(q, gap),
                );
            }
        }
        case(
            v,
            "a2.ce2.fzf_info_line_ctrl_u_and_key_one_ssh_batch",
            FF,
            ce2_fzf_info_line_ssh_batch,
        );
        case(v, "a2.pre1.vim_yyp_then_s", FF, pre1_vim_yyp_then_s);
        case(
            v,
            "a2.ce1d.fzf_first_item_shares_directory",
            FF,
            ce1d_first_item_shares_only_the_directory,
        );
    }
}

// ===========================================================================
// a3 — COPIES UNDER THE GUI'S PACING: `h1`, `h3`, `h4`, `h4b`, `h5` and
// `p1`…`p4`. One host: the scroll seam, LOCK A (caret
// row not re-read), the tick. HOST OPTIONS on the case: `flat` = a frame
// every 16 ms; `paced` = the GUI's real pacing (a frame at 16 ms only while
// `needs_frame_cadence`, else at `next_change_deadline`, else only when
// output lands) — style "rainbow kitty"; `pet` = paced with the default
// "rainbow kitty pet"'s own frame demand joined; `far0` periods = the
// composed (split/zoomed) host's far-row read dropped. The lit census is
// read at a flat 16 ms regardless (an instrument, not the host).
// ===========================================================================
mod a3 {
    use super::*;

    const ROWS: usize = 24;
    const COLS: usize = 80;
    const FRAME: Duration = Duration::from_millis(16);

    pub struct Host {
        c: Core,
        paced: bool,
    }
    core_deref!(Host);

    fn mode_str(mode: u8) -> &'static str {
        match mode {
            0 => "flat",
            1 => "paced",
            _ => "pet",
        }
    }

    impl Host {
        fn at_row(row: u16, paced: bool) -> Self {
            Self::at_row_mode(row, paced, false)
        }

        fn at_row_mode(row: u16, paced: bool, pet: bool) -> Self {
            let mut c = Core::new(
                ROWS,
                COLS,
                8,
                16,
                Theme::Tokyo,
                Opts {
                    scroll_seam: true,
                    alt_rebaseline: true,
                    skip_caret: true,
                    honour_sync: false,
                    pane_columns: false,
                },
            );
            c.term.process(format!("\x1b[{};1H", row + 1).as_bytes());
            c.pet = pet.then(PetBrain::default);
            c.frame();
            Self { c, paced }
        }

        /// Let `ms` pass with the frames the host would draw in that time.
        fn advance(&mut self, ms: u64) {
            let end = self.now + Duration::from_millis(ms);
            if !self.paced {
                while self.now + FRAME <= end {
                    self.now += FRAME;
                    self.frame();
                }
                self.now = end;
                return;
            }
            loop {
                let glow = if self.glow.needs_frame_cadence() {
                    Some(self.now + FRAME)
                } else {
                    self.glow.next_change_deadline(self.now, FRAME)
                };
                let pet = self.pet.as_ref().and_then(|p| {
                    if p.needs_frames() {
                        Some(self.now + FRAME)
                    } else {
                        p.next_change_deadline(self.now)
                    }
                });
                let next = match (glow, pet) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                };
                match next {
                    Some(t) if t <= end => {
                        self.now = t.max(self.now + Duration::from_millis(1));
                        self.frame();
                    }
                    _ => break,
                }
            }
            self.now = end;
        }

        fn type_with(&mut self, s: &str, suffix: &str, gap: u64) {
            for ch in s.chars() {
                self.advance(gap.saturating_sub(2));
                let now = self.now;
                self.glow.note_typed_cells(now, 1);
                self.advance(2);
                self.term.process(format!("{ch}{suffix}").as_bytes());
                self.frame();
            }
        }

        fn type_str(&mut self, s: &str) {
            self.type_with(s, "", 90);
        }

        fn program(&mut self, bytes: &[u8]) {
            self.advance(1);
            self.term.process(bytes);
            self.frame();
        }

        fn kill(&mut self, bytes: &[u8]) {
            let now = self.now;
            self.glow.note_kill(now, true);
            self.advance(2);
            self.term.process(bytes);
            self.frame();
        }

        fn kill_and_key(&mut self, bytes: &[u8], sep: u64, rtt: u64) {
            let t_kill = self.now;
            self.glow.note_kill(t_kill, true);
            self.advance(sep);
            let now = self.now;
            self.glow.note_typed_cells(now, 1);
            self.advance(rtt.saturating_sub(sep));
            self.term.process(bytes);
            self.frame();
        }

        fn verdict(&mut self, m: (u64, u64), row: u16) -> Outcome {
            let (f, r) = self.counts();
            let mut n = 0;
            for _ in 0..90 {
                self.now += FRAME;
                self.frame();
                n += usize::from(self.lit(row));
            }
            Outcome::ff(f - m.0, r - m.1, n)
        }
    }

    /// A precondition of the case; a miss is a bad verdict, named.
    fn fixture(ok: bool, what: &str, o: Outcome) -> Outcome {
        if ok {
            o
        } else {
            let mut o = o;
            o.ok = false;
            o.note = format!("FIXTURE FAILED: {what}; {}", o.note);
            o
        }
    }

    // H1: a list RE-SORTED by the erase itself (layouts 0..3).
    fn fzf_resort(layout: u8, paced: bool, remote: bool) -> Outcome {
        let q = "test";
        let reverse = layout >= 2;
        let info = layout.is_multiple_of(2);
        let prompt: u16 = if reverse { 10 } else { 21 };
        let slots: Vec<u16> = if reverse {
            let first = prompt + 2 + u16::from(info);
            (first..first + 3).collect()
        } else {
            let first = prompt - u16::from(info);
            (first - 2..=first).rev().collect()
        };
        let info_row = if reverse { prompt + 2 } else { prompt };
        let draw = |items: &[&str], count: &str| {
            let mut s = String::from("\x1b7");
            for (i, it) in items.iter().enumerate() {
                let ptr = if i == 0 { "> " } else { "  " };
                s += &format!("\x1b[{};1H{ptr}{it}\x1b[K", slots[i]);
            }
            if info {
                s += &format!("\x1b[{};1H  {count}\x1b[K", info_row);
            }
            s += "\x1b8";
            s
        };
        let filtered = draw(&["cargo test", "src/latest.rs", "git stash"], "3/9");
        let full = draw(&["tests/a.rs", "cargo test", "src/lib.rs"], "9/9");
        let mut h = Host::at_row(prompt, paced);
        h.program(format!("\x1b[{};1H> {full}\x1b[{};3H", prompt + 1, prompt + 1).as_bytes());
        h.advance(300);
        h.type_with(q, &filtered, 110);
        h.advance(250);
        let row = slots[0] - 1;
        let fx = h.live(prompt) == (2..6).collect::<Vec<u16>>();
        let m = h.counts();
        let bytes = format!("\r\x1b[2C\x1b[K{full}");
        if remote {
            h.kill_and_key(format!("\r\x1b[2C\x1b[Kt{full}").as_bytes(), 50, 90);
        } else {
            h.kill(bytes.as_bytes());
        }
        let o = h.verdict(m, row);
        fixture(fx, "the band under the query", o)
    }

    // H5: the COMPOSED host: the far-row read dropped while the list streams.
    fn composed_loading(quiet: usize, paced: bool, remote: bool) -> Outcome {
        let q = "src/li";
        let mut h = Host::at_row(21, paced);
        h.program(b"\x1b[22;1H> ");
        h.advance(200);
        let list = |n: usize| {
            format!(
                "\x1b7\x1b[18;1H  tests/a.rs\x1b[K\x1b[19;1H  src/main.rs\x1b[K\x1b[20;1H> src/lib.rs\x1b[K\x1b[21;1H  3/{n}\x1b[K\x1b8"
            )
        };
        h.type_str(&q[..2]);
        h.advance(60);
        h.far_ok = false;
        let mut n = 100;
        h.program(list(n).as_bytes());
        for ch in q[2..].chars() {
            n += 37;
            h.type_with(&ch.to_string(), &list(n), 90);
            for _ in 0..3 {
                n += 11;
                h.advance(16);
                h.term.process(list(n).as_bytes());
                h.frame();
            }
        }
        for _ in 0..10 {
            n += 13;
            h.advance(16);
            h.term.process(list(n).as_bytes());
            h.frame();
        }
        h.far_ok = true;
        for _ in 0..quiet {
            h.advance(16);
            h.frame();
        }
        let fx = h.live(21) == (2..8).collect::<Vec<u16>>();
        let m = h.counts();
        if remote {
            h.kill_and_key(format!("\r\x1b[2C\x1b[Ks{}", list(n)).as_bytes(), 50, 90);
        } else {
            h.advance(16);
            h.kill(format!("\r\x1b[2C\x1b[K{}", list(n)).as_bytes());
        }
        let o = h.verdict(m, 19);
        fixture(fx, "the band under the query", o)
    }

    // H4 / H4b: the ROW BUDGET full, the focus band's rows two away hidden.
    fn budget_rtt(pause: u64, bands: &[u16], paced: bool, rtt: u64) -> Outcome {
        let q = "src/li";
        let mut h = Host::at_row(0, paced);
        for &row in bands {
            h.program(format!("\x1b[{};1H", row + 1).as_bytes());
            h.type_with("ab", "", 80);
        }
        h.program(b"\x1b[22;1H> ");
        let list = "\x1b7\x1b[18;1H  tests/a.rs\x1b[K\x1b[19;1H  src/main.rs\x1b[K\x1b[20;1H> src/lib.rs\x1b[K\x1b[21;1H  3/3\x1b[K\x1b8";
        h.type_with(&q[..5], "", 90);
        h.type_with(&q[5..], list, 90);
        h.advance(pause);
        let named = h.named();
        let fx = h.live(21) == (2..8).collect::<Vec<u16>>();
        let m = h.counts();
        h.kill_and_key(format!("\r\x1b[2C\x1b[Ks{list}").as_bytes(), 50, rtt);
        let o = h
            .verdict(m, 19)
            .with_note(format!("idle_named_before_erase={named:?}"));
        fixture(fx, "the band under the query", o)
    }

    // H3: a strictly alternating flicker never becomes a twin.
    fn flicker(gap: u16, stand: u64, paced: bool) -> Outcome {
        let cmd = "git status";
        let mut h = Host::at_row(10, paced);
        h.program(b"\x1b[11;1H$ ");
        h.type_str(cmd);
        h.advance(100);
        let row1 = 10 - gap + 1;
        let copy = format!("\x1b7\x1b[{row1};1H$ {cmd}\x1b[K\x1b8");
        let torn = format!("\x1b7\x1b[{row1};1H\x1b[K\x1b8");
        let end = h.t() + stand;
        while h.t() < end {
            h.advance(16);
            h.term.process(copy.as_bytes());
            h.frame();
            h.advance(16);
            h.term.process(torn.as_bytes());
            h.frame();
        }
        h.advance(16);
        h.term.process(copy.as_bytes());
        h.frame();
        let m = h.counts();
        h.kill(format!("{}\x1b[K", "\x08".repeat(cmd.len())).as_bytes());
        h.verdict(m, 10 - gap)
    }

    // P1: a streamed row quotes the composer, Ctrl-U after a human pause.
    fn quote_then_kill(gap: u16, pause: u64, mode: u8) -> Outcome {
        let paced = mode > 0;
        let text = "please fix the tests";
        let mut h = Host::at_row_mode(20, paced, mode == 2);
        let top = 20 - gap;
        h.program(b"\x1b[21;1H> ");
        let stream = |i: usize| {
            format!(
                "\x1b7\x1b[{};1H\x1b[2K  thinking {}\x1b8",
                top + 1,
                ".".repeat(i % 7)
            )
        };
        for (i, ch) in text.chars().enumerate() {
            h.type_with(&ch.to_string(), &stream(i), 90);
        }
        h.program(stream(text.len()).as_bytes());
        h.advance(200);
        h.program(format!("\x1b7\x1b[{};1H\x1b[2K  {text}\x1b8", top + 1).as_bytes());
        let tq = h.t();
        h.advance(pause);
        let between = h.frames_since(tq);
        let m = h.counts();
        h.kill(b"\r\x1b[2C\x1b[K");
        h.verdict(m, top)
            .with_note(format!("sampled_frames_between={between}"))
    }

    // P2: fzf's list lands late, then Ctrl-U after a pause.
    fn fzf_late(k0: usize, reverse: bool, pause: u64, mode: u8) -> Outcome {
        let paced = mode > 0;
        let q = "src/li";
        let (query_row, item_row) = if reverse { (10u16, 11u16) } else { (21, 20) };
        let mut h = Host::at_row_mode(query_row, paced, mode == 2);
        h.program(format!("\x1b[{};1H> ", query_row + 1).as_bytes());
        let list = if reverse {
            "\x1b7\x1b[12;1H> src/lib.rs\x1b[K\x1b[13;1H  src/main.rs\x1b[K\x1b[14;1H  tests/a.rs\x1b[K\x1b8"
        } else {
            "\x1b7\x1b[19;1H  tests/a.rs\x1b[K\x1b[20;1H  src/main.rs\x1b[K\x1b[21;1H> src/lib.rs\x1b[K\x1b8"
        };
        h.type_str(&q[..k0]);
        h.advance(40);
        h.program(list.as_bytes());
        let tl = h.t();
        h.type_with(&q[k0..], list, 90);
        h.advance(pause);
        let between = h.frames_since(tl);
        let m = h.counts();
        h.kill(format!("\r\x1b[2C\x1b[K{list}").as_bytes());
        h.verdict(m, item_row)
            .with_note(format!("sampled_frames_between={between}"))
    }

    // P3: a copy drawn after the last key, erased after a pause.
    fn copy_after_last_key(pause: u64, erase_kill: bool, mode: u8) -> Outcome {
        let paced = mode > 0;
        let cmd = "git status";
        let mut h = Host::at_row_mode(4, paced, mode == 2);
        h.program(b"\r\n$ ");
        h.type_str(cmd);
        h.advance(100);
        h.program(format!("\x1b7\x1b[5;1H$ {cmd}\x1b8").as_bytes());
        let tc = h.t();
        h.advance(pause);
        let between = h.frames_since(tc);
        let m = h.counts();
        if erase_kill {
            h.kill(format!("{}\x1b[K", "\x08".repeat(cmd.len())).as_bytes());
        } else {
            h.program(b"\r$ \x1b[K");
        }
        h.verdict(m, 4)
            .with_note(format!("sampled_frames_between={between}"))
    }

    // P4: vim `yyp` then `k` `S`, each command a keystroke apart.
    fn vim_yyp(pause: u64, paced: bool) -> Outcome {
        let mut h = Host::at_row(3, paced);
        h.program(
            b"\x1b[1;1Hfn f() {\x1b[2;1H    let a = 1;\x1b[3;1H    \x1b[4;1H}\x1b[5;1H~\x1b[6;1H~\x1b[3;5H",
        );
        h.type_str("let total = 0;");
        h.advance(150);
        h.program(b"\x1b[3;18H");
        h.advance(150);
        h.program(b"\x1b[4;1H\x1b[L    let total = 0;\x1b[4;5H");
        let tc = h.t();
        h.advance(pause);
        let between = h.frames_since(tc);
        let m = h.counts();
        h.program(b"\x1b[3;5H\x1b[K");
        h.verdict(m, 3)
            .with_note(format!("sampled_frames_between={between}"))
    }

    const PAUSES: [u64; 9] = [20, 40, 60, 90, 150, 250, 400, 600, 900];

    pub fn cases(v: &mut Vec<Case>) {
        for layout in 0..4u8 {
            for paced in [false, true] {
                for remote in [false, true] {
                    case(
                        v,
                        format!(
                            "a3.h1.fzf_resorted_by_ctrl_u/layout{layout}/{}/remote{}",
                            mode_str(u8::from(paced)),
                            u8::from(remote)
                        ),
                        FF,
                        move || fzf_resort(layout, paced, remote),
                    );
                }
            }
        }
        for quiet in [0usize, 1, 2, 4] {
            for paced in [false, true] {
                for remote in [false, true] {
                    case(
                        v,
                        format!(
                            "a3.h5.composed_far0_while_streaming/quiet{quiet}/{}/remote{}",
                            mode_str(u8::from(paced)),
                            u8::from(remote)
                        ),
                        FF,
                        move || composed_loading(quiet, paced, remote),
                    );
                }
            }
        }
        let b4: [&'static [u16]; 3] = [&[3, 8], &[3, 8, 13], &[2, 6, 10, 14]];
        for bands in b4 {
            for pause in [60u64, 150, 300] {
                for paced in [false, true] {
                    case(
                        v,
                        format!(
                            "a3.h4.budget_full/bands{}/pause{pause}/{}/rtt90",
                            gstr(&bands.iter().map(|&b| b as usize).collect::<Vec<_>>()),
                            mode_str(u8::from(paced))
                        ),
                        FF,
                        move || budget_rtt(pause, bands, paced, 90),
                    );
                }
            }
        }
        let b4b: [&'static [u16]; 2] = [&[3, 8], &[2, 6, 10, 14]];
        for bands in b4b {
            for pause in [150u64, 600] {
                for paced in [false, true] {
                    for rtt in [51u64, 55, 60, 70] {
                        case(
                            v,
                            format!(
                                "a3.h4b.budget_full_echo_behind_key/bands{}/pause{pause}/{}/rtt{rtt}",
                                gstr(&bands.iter().map(|&b| b as usize).collect::<Vec<_>>()),
                                mode_str(u8::from(paced))
                            ),
                            FF,
                            move || budget_rtt(pause, bands, paced, rtt),
                        );
                    }
                }
            }
        }
        for gap in [1u16, 2] {
            for stand in [100u64, 300, 800] {
                for paced in [false, true] {
                    case(
                        v,
                        format!(
                            "a3.h3.alternating_flicker/gap{gap}/stand{stand}/{}",
                            mode_str(u8::from(paced))
                        ),
                        FF,
                        move || flicker(gap, stand, paced),
                    );
                }
            }
        }
        for mode in [0u8, 1, 2] {
            for gap in [1u16, 2] {
                for pause in sweep(&PAUSES, &[20, 60, 90, 900]) {
                    case(
                        v,
                        format!(
                            "a3.p1.streamed_quote_then_ctrl_u/gap{gap}/pause{pause}/{}",
                            mode_str(mode)
                        ),
                        FF,
                        move || quote_then_kill(gap, pause, mode),
                    );
                }
            }
        }
        for mode in [0u8, 1, 2] {
            for k0 in sweep(&[4usize, 5, 6], &[4, 6]) {
                for reverse in sweep(&[false, true], &[false]) {
                    for pause in sweep(&PAUSES, &[20, 150, 250, 900]) {
                        case(
                            v,
                            format!(
                                "a3.p2.fzf_late_then_ctrl_u/k{k0}/rev{}/pause{pause}/{}",
                                u8::from(reverse),
                                mode_str(mode)
                            ),
                            FF,
                            move || fzf_late(k0, reverse, pause, mode),
                        );
                    }
                }
            }
        }
        for mode in [0u8, 1, 2] {
            for erase_kill in [false, true] {
                for pause in sweep(&PAUSES, &[20, 90, 150, 900]) {
                    case(
                        v,
                        format!(
                            "a3.p3.copy_after_last_key/kill{}/pause{pause}/{}",
                            u8::from(erase_kill),
                            mode_str(mode)
                        ),
                        FF,
                        move || copy_after_last_key(pause, erase_kill, mode),
                    );
                }
            }
        }
        for paced in [false, true] {
            for pause in PAUSES {
                case(
                    v,
                    format!(
                        "a3.p4.vim_yyp_then_s/pause{pause}/{}",
                        mode_str(u8::from(paced))
                    ),
                    FF,
                    move || vim_yyp(pause, paced),
                );
            }
        }
    }
}

// ===========================================================================
// l3c — the laws of `tests/copy_beside_the_line.rs`, under the GUI's real
// frame pacing as well as the flat 16 ms train.
// Only the PACED cases are this file's; the flat ones are that file's own.
// ===========================================================================
mod l3c {
    use super::*;

    const ROWS: usize = 24;
    const COLS: usize = 80;

    struct Host {
        c: Core,
        paced: bool,
    }
    core_deref!(Host);

    impl Host {
        fn at_row(row: u16, paced: bool) -> Self {
            let mut c = Core::new(
                ROWS,
                COLS,
                8,
                16,
                Theme::Tokyo,
                Opts {
                    scroll_seam: true,
                    alt_rebaseline: true,
                    skip_caret: true,
                    honour_sync: false,
                    pane_columns: false,
                },
            );
            c.term.process(format!("\x1b[{};1H", row + 1).as_bytes());
            c.frame();
            Self { c, paced }
        }

        fn idle(&mut self, ms: u64) {
            let end = self.now + Duration::from_millis(ms);
            if !self.paced {
                while self.now < end {
                    self.now += Duration::from_millis(16);
                    self.frame();
                }
                return;
            }
            loop {
                let next = if self.glow.needs_frame_cadence() {
                    Some(self.now + Duration::from_millis(16))
                } else {
                    self.glow
                        .next_change_deadline(self.now, Duration::from_millis(16))
                };
                match next {
                    Some(t) if t <= end => {
                        self.now = t.max(self.now + Duration::from_millis(1));
                        self.frame();
                    }
                    _ => break,
                }
            }
            self.now = end;
        }

        /// Flat: `now += ms` with no frame. Paced: the frames due in `ms`.
        fn wait(&mut self, ms: u64) {
            if self.paced {
                self.idle(ms);
            } else {
                self.now += Duration::from_millis(ms);
            }
        }

        fn type_with(&mut self, s: &str, suffix: &str) {
            for ch in s.chars() {
                self.wait(90);
                let now = self.now;
                self.glow.note_typed_cells(now, 1);
                self.term.process(format!("{ch}{suffix}").as_bytes());
                self.frame();
            }
        }

        fn type_str(&mut self, s: &str) {
            self.type_with(s, "");
        }

        fn program(&mut self, bytes: &[u8]) {
            self.wait(16);
            self.term.process(bytes);
            self.frame();
        }

        fn kill(&mut self, bytes: &[u8]) {
            self.wait(16);
            let now = self.now;
            self.glow.note_kill(now, true);
            self.term.process(bytes);
            self.frame();
        }

        fn verdict(&mut self, law: &mut Law, m: (u64, u64), row: u16, what: &str) {
            let (f, r) = self.counts();
            let mut lit = 0;
            for _ in 0..90 {
                self.idle(16);
                lit += usize::from(self.lit(row));
            }
            law.add(f - m.0, r - m.1);
            law.wrong_lit += lit as u64;
            if (f - m.0, lit) != (0, 0) {
                law.fails
                    .push(format!("{what}: (followed, lit)=({}, {lit})", f - m.0));
            }
        }
    }

    fn fzf_list_lands_late(
        paced: bool,
        k0: usize,
        reverse: bool,
        redraw_after: Option<&str>,
    ) -> Outcome {
        let mut law = Law::default();
        let q = "src/li";
        let (query_row, item_row) = if reverse { (10u16, 11u16) } else { (21, 20) };
        let mut h = Host::at_row(query_row, paced);
        h.program(format!("\x1b[{};1H> ", query_row + 1).as_bytes());
        let list = if reverse {
            "\x1b7\x1b[12;1H> src/lib.rs\x1b[K\x1b[13;1H  src/main.rs\x1b[K\x1b[14;1H  tests/a.rs\x1b[K\x1b8"
        } else {
            "\x1b7\x1b[19;1H  tests/a.rs\x1b[K\x1b[20;1H  src/main.rs\x1b[K\x1b[21;1H> src/lib.rs\x1b[K\x1b8"
        };
        h.type_str(&q[..k0]);
        h.program(list.as_bytes());
        h.type_with(&q[k0..], list);
        h.idle(64);
        law.eq(
            h.live(query_row),
            (2..8).collect::<Vec<u16>>(),
            "fixture: the band is under the query",
        );
        let m = h.counts();
        let after = redraw_after.unwrap_or(list);
        h.kill(format!("\r\x1b[2C\x1b[K{after}").as_bytes());
        h.verdict(&mut law, m, item_row, "the query went nowhere");
        law.done()
    }

    fn fzf_best_match_two_up_remote(paced: bool) -> Outcome {
        let mut law = Law::default();
        let q = "src/li";
        let mut h = Host::at_row(21, paced);
        h.program(b"\x1b[22;1H> ");
        let list = "\x1b7\x1b[18;1H  tests/a.rs\x1b[K\x1b[19;1H  src/main.rs\x1b[K\x1b[20;1H> src/lib.rs\x1b[K\x1b[21;1H  3/3\x1b[K\x1b8";
        h.type_str(&q[..2]);
        h.program(list.as_bytes());
        h.type_with(&q[2..], list);
        h.idle(64);
        law.eq(h.live(21), (2..8).collect::<Vec<u16>>(), "fixture");
        let m = h.counts();
        h.now += Duration::from_millis(90);
        let t_kill = h.now;
        h.glow.note_kill(t_kill, true);
        h.glow
            .note_typed_cells(t_kill + Duration::from_millis(60), 1);
        h.now = t_kill + Duration::from_millis(120);
        h.term.process(format!("\r\x1b[2C\x1b[Ks{list}").as_bytes());
        h.frame();
        h.verdict(&mut law, m, 19, "Ctrl-U and `s` in one batch");
        law.done()
    }

    fn completion_popup_ctrl_w(paced: bool) -> Outcome {
        let mut law = Law::default();
        let word = "println";
        let mut h = Host::at_row(5, paced);
        h.program(b"\x1b[5;1Hfn main() {\x1b[6;1H    \x1b[7;1H}\x1b[6;5H");
        let menu = "\x1b7\x1b[7;5Hprintln!      Macro\x1b[K\x1b8";
        h.type_str(&word[..2]);
        h.type_with(&word[2..], menu);
        h.idle(48);
        law.eq(h.live(5), (4..11).collect::<Vec<u16>>(), "fixture");
        let m = h.counts();
        h.kill(b"\x1b[6;5H\x1b[K");
        h.program(b"\x1b7\x1b[7;1H}\x1b[K\x1b8");
        h.verdict(&mut law, m, 6, "Ctrl-W under an open popup");
        law.done()
    }

    fn copy_after_first_two_keys(paced: bool) -> Outcome {
        let mut law = Law::default();
        let cmd = "git status";
        let mut h = Host::at_row(4, paced);
        h.program(b"\r\n$ ");
        h.type_str(&cmd[..2]);
        h.program(format!("\x1b7\x1b[5;1H$ {cmd}\x1b8").as_bytes());
        h.type_str(&cmd[2..]);
        h.idle(64);
        law.eq(h.live(5), (2..12).collect::<Vec<u16>>(), "fixture");
        let m = h.counts();
        h.program(b"\r$ \x1b[K");
        h.verdict(&mut law, m, 4, "the redraw's erase");
        law.done()
    }

    fn vim_yyp_then_s(paced: bool) -> Outcome {
        let mut law = Law::default();
        let mut h = Host::at_row(3, paced);
        h.program(
            b"\x1b[1;1Hfn f() {\x1b[2;1H    let a = 1;\x1b[3;1H    \x1b[4;1H}\x1b[5;1H~\x1b[6;1H~\x1b[3;5H",
        );
        h.type_str("let total = 0;");
        h.idle(64);
        law.eq(h.live(2), (4..18).collect::<Vec<u16>>(), "fixture");
        let m = h.counts();
        h.program(b"\x1b[3;18H");
        h.program(b"\x1b[4;1H\x1b[L    let total = 0;\x1b[4;5H");
        h.idle(32);
        h.program(b"\x1b[3;5H\x1b[K");
        h.verdict(&mut law, m, 3, "`S` on the original");
        law.done()
    }

    fn streamed_quote(paced: bool, gap: u16) -> Outcome {
        let mut law = Law::default();
        let text = "please fix the tests";
        let mut h = Host::at_row(20, paced);
        let top = 20 - gap;
        h.program(b"\x1b[21;1H> ");
        let stream = |i: usize| {
            format!(
                "\x1b7\x1b[{};1H\x1b[2K  thinking {}\x1b8",
                top + 1,
                ".".repeat(i % 7)
            )
        };
        for (i, ch) in text.chars().enumerate() {
            h.type_with(&ch.to_string(), &stream(i));
        }
        h.program(stream(text.len()).as_bytes());
        law.eq(h.live(20), (2..22).collect::<Vec<u16>>(), "fixture");
        h.program(format!("\x1b7\x1b[{};1H\x1b[2K  {text}\x1b8", top + 1).as_bytes());
        h.idle(90);
        let m = h.counts();
        h.kill(b"\r\x1b[2C\x1b[K");
        h.verdict(&mut law, m, top, "the quote, then Ctrl-U");
        law.done()
    }

    fn copy_on_new_last_row_by_scroll(paced: bool) -> Outcome {
        let mut law = Law::default();
        let cmd = "git status";
        let last = (ROWS - 1) as u16;
        let mut h = Host::at_row(last, paced);
        h.program(b"$ ");
        h.type_str(cmd);
        h.idle(48);
        law.eq(h.live(last), (2..12).collect::<Vec<u16>>(), "fixture");
        let m = h.counts();
        h.program(format!("\r\n$ {cmd}").as_bytes());
        law.eq(
            h.live(last - 1),
            (2..12).collect::<Vec<u16>>(),
            "the scroll carried the band up with its line",
        );
        h.idle(48);
        h.program(format!("\x1b7\x1b[{};1H\x1b[K\x1b8", last).as_bytes());
        h.verdict(&mut law, m, last, "the original erased above its copy");
        law.done()
    }

    fn limit_less_than_twin_min_is_a_move(paced: bool, which: u8) -> Outcome {
        let mut law = Law::default();
        if which == 0 {
            let text = "please fix the tests";
            let mut h = Host::at_row(20, paced);
            h.program(b"\x1b[21;1H> ");
            h.type_str(text);
            h.idle(64);
            h.program(format!("\x1b7\x1b[20;1H\x1b[2K  {text}\x1b8").as_bytes());
            let (f0, r0) = h.counts();
            h.kill(b"\r\x1b[2C\x1b[K");
            let (f1, r1) = h.counts();
            law.add(f1 - f0, r1 - r0);
            law.eq(
                f1 - f0,
                20,
                "a copy seen on one frame before the erase is a torn repaint's new row",
            );
        } else {
            let cmd = "git status";
            let last = (ROWS - 1) as u16;
            let mut h = Host::at_row(last, paced);
            h.program(b"$ ");
            h.type_str(cmd);
            h.idle(48);
            h.program(b"\r\n");
            h.idle(32);
            let (f0, r0) = h.counts();
            h.program(
                format!(
                    "$ {cmd}\x1b[{};1H\x1b[K\x1b[{};{}H",
                    last,
                    last + 1,
                    cmd.len() + 3
                )
                .as_bytes(),
            );
            let (f1, r1) = h.counts();
            law.add(f1 - f0, r1 - r0);
            law.eq(
                f1 - f0,
                10,
                "a copy written with the erase onto a row seen empty is the line moved",
            );
        }
        law.done()
    }

    pub fn cases(v: &mut Vec<Case>) {
        for paced in [false, true] {
            let p = if paced { "paced" } else { "flat" };
            for (k0, reverse) in [(2usize, false), (2, true), (3, false), (3, true)] {
                case(
                    v,
                    format!(
                        "L3.copy.fzf_list_lands_after_first_keys/k{k0}/rev{}/{p}",
                        u8::from(reverse)
                    ),
                    FF,
                    move || fzf_list_lands_late(paced, k0, reverse, None),
                );
            }
            case(
                v,
                format!("L3.copy.fzf_list_resorted_as_query_cleared/{p}"),
                FF,
                move || {
                    let full = "\x1b7\x1b[19;1H  tests/a.rs\x1b[K\x1b[20;1H  src/lib.rs\x1b[K\x1b[21;1H> src/main.rs\x1b[K\x1b8";
                    fzf_list_lands_late(paced, 2, false, Some(full))
                },
            );
            case(
                v,
                format!("L3.copy.fzf_best_match_two_up_remote_link/{p}"),
                FF,
                move || fzf_best_match_two_up_remote(paced),
            );
            case(
                v,
                format!("L3.copy.completion_popup_then_ctrl_w/{p}"),
                FF,
                move || completion_popup_ctrl_w(paced),
            );
            case(
                v,
                format!("L3.copy.copy_drawn_after_first_two_keys/{p}"),
                FF,
                move || copy_after_first_two_keys(paced),
            );
            case(v, format!("L3.copy.vim_yyp_then_s/{p}"), FF, move || {
                vim_yyp_then_s(paced)
            });
            for gap in [1u16, 2] {
                case(
                    v,
                    format!("L3.copy.streamed_row_quotes_composer/gap{gap}/{p}"),
                    FF,
                    move || streamed_quote(paced, gap),
                );
            }
            case(
                v,
                format!("L3.copy.copy_on_new_last_row_by_scroll/{p}"),
                FF,
                move || copy_on_new_last_row_by_scroll(paced),
            );
            case(
                v,
                format!("L3.copy.limit_one_frame_copy_is_a_move/quote/{p}"),
                MF,
                move || limit_less_than_twin_min_is_a_move(paced, 0),
            );
            case(
                v,
                format!("L3.copy.limit_one_frame_copy_is_a_move/scroll_batch/{p}"),
                MF,
                move || limit_less_than_twin_min_is_a_move(paced, 1),
            );
        }
    }
}

// ===========================================================================
// d4 — A GLYPH CAUGHT UP, A TYPO FIXED, UNDER THE PREVIOUS COMMAND: `t4`,
// `t5`. Host: `trail_host::PacedHost`
// (the scroll seam, the alt re-baseline, LOCK A with the caret row not
// re-read) on a flat 16 ms or 8 ms train, the GUI's real pacing, or the
// pet's (`fr16`, `fr8`, `paced`, `pet`). Keys 90–110 ms apart. The reading is
// `(followed, frames of 90 the previous command's row was lit)` after the
// kill.
// ===========================================================================
mod d4 {
    use super::*;

    /// T4: a line typed under an IDENTICAL line (twins at arming), then an
    /// edit the shape pass reads as "not evidence" (the records catch up),
    /// then Ctrl-U `wait` ms later. `edit`: 0 none; 1 two interior glyphs
    /// overwritten, then restored 90 ms later; 2 a torn read (the row
    /// cleared and half-written for 16 ms); 3 as 1, two columns on.
    fn t4(pace: Pace, edit: u8, wait: u64) -> Outcome {
        let mut h = PacedHost::new(24, 80, pace, false);
        h.land(b"\x1b[10;1H$ git status --short\r\n$ ");
        h.type_plain("git status --short", 90);
        h.advance(300);
        match edit {
            1 => {
                h.land(b"\x1b[11;7Hxx\x1b[11;21H");
                h.advance(90);
                h.land(b"\x1b[11;7Hst\x1b[11;21H");
            }
            2 => {
                h.land(b"\x1b[11;1H\x1b[2K$ git st");
                h.advance(16);
                h.land(b"\x1b[11;1H$ git status --short\x1b[11;21H");
            }
            3 => {
                h.land(b"\x1b[11;9Hxx\x1b[11;21H");
                h.advance(90);
                h.land(b"\x1b[11;9Htu\x1b[11;21H");
            }
            _ => {}
        }
        h.advance(wait);
        let m = h.counts();
        let now = h.now;
        h.glow.note_kill(now, true);
        h.advance(2);
        h.land(b"\x1b[11;3H\x1b[K");
        let (f, r) = h.counts();
        let mut lit = 0;
        for _ in 0..90 {
            h.now += Duration::from_millis(16);
            h.frame();
            lit += usize::from(h.lit(9));
        }
        Outcome::ff(f - m.0, r - m.1, lit)
    }

    /// T5: a typo fixed IN PLACE in a line typed under the previous, similar
    /// command — the fix makes the two agree at those columns — then the line
    /// killed `wait` ms after the fix. `how`: 0 one batch (Ctrl-T's echo
    /// rewriting two interior cells); 1 two single-glyph replacements 150 ms
    /// apart (vi `r` twice). `above` on row 9 after `$ `, `typed` on row 10,
    /// `fix` = (0-based column, replacement).
    fn t5(
        pace: Pace,
        above: &'static str,
        typed: &'static str,
        fix: (u16, &'static str),
        how: u8,
        wait: u64,
    ) -> Outcome {
        let mut h = PacedHost::new(24, 80, pace, false);
        h.land(format!("\x1b[10;1H$ {above}\r\n$ ").as_bytes());
        h.type_plain(typed, 110);
        h.advance(250);
        let (col, rep) = fix;
        if how == 0 {
            h.advance(2);
            h.land(format!("\x1b[11;{}H{rep}", col + 1).as_bytes());
        } else {
            for (i, ch) in rep.chars().enumerate() {
                h.advance(150);
                let c = col + 1 + i as u16;
                h.land(format!("\x1b[11;{c}H{ch}\x1b[11;{c}H").as_bytes());
            }
        }
        h.advance(wait);
        let m = h.counts();
        let now = h.now;
        h.glow.note_kill(now, true);
        h.advance(2);
        h.land(b"\x1b[11;3H\x1b[K");
        let (f, r) = h.counts();
        let mut lit = 0;
        for _ in 0..90 {
            h.now += Duration::from_millis(16);
            h.frame();
            lit += usize::from(h.lit(9));
        }
        Outcome::ff(f - m.0, r - m.1, lit)
    }

    /// T7: a line MOVED onto a row beside an identical line, then killed.
    /// `test lib` typed on row 9 with `$ test lib` printed on row 11 and row
    /// 10 blank (`near` = false) or holding a near copy, `$ hest liv`
    /// (`near` = true); then the line relocated one row down onto row 10 in
    /// one write (the caret with it), and Ctrl-U `wait` ms later. Nothing
    /// moved at the kill: the identical line on row 11 must never light.
    fn t7(pace: Pace, near: bool, wait: u64) -> Outcome {
        let text = "test lib";
        let mut h = PacedHost::new(24, 80, pace, false);
        let land_row = if near { "$ hest liv" } else { "" };
        h.land(format!("\x1b[11;1H{land_row}\x1b[12;1H$ {text}\x1b[10;1H$ ").as_bytes());
        h.advance(100);
        h.type_plain(text, 110);
        h.advance(200);
        let m0 = h.counts();
        h.land(format!("\x1b[11;1H\x1b[2K$ {text}\x1b[10;1H\x1b[2K\x1b[11;11H").as_bytes());
        let m1 = h.counts();
        h.advance(wait);
        let now = h.now;
        h.glow.note_kill(now, true);
        h.advance(2);
        h.land(b"\x1b[11;3H\x1b[K");
        let (f, r) = h.counts();
        let mut lit = 0;
        for _ in 0..60 {
            h.now += Duration::from_millis(16);
            h.frame();
            lit += usize::from(h.lit(11));
        }
        Outcome::ff(f - m1.0, r - m1.1, lit).with_note(format!("the move followed {}", m1.0 - m0.0))
    }

    pub fn cases(v: &mut Vec<Case>) {
        for pace in PACES {
            for near in [false, true] {
                for wait in sweep(&[16u64, 32, 48, 64, 90, 130, 200, 400], &[32, 48, 90, 400]) {
                    case(
                        v,
                        format!(
                            "d4.t7.moved_then_killed_beside_its_twin/{}/near{}/wait{wait}",
                            pace.tag(),
                            u8::from(near)
                        ),
                        FF,
                        move || t7(pace, near, wait),
                    );
                }
            }
        }
        for pace in PACES {
            for edit in [0u8, 1, 2, 3] {
                for wait in sweep(&[0u64, 8, 16, 24, 32, 48, 90, 200], &[0, 32, 200]) {
                    case(
                        v,
                        format!(
                            "d4.t4.catchup_then_kill_under_twin/{}/edit{edit}/wait{wait}",
                            pace.tag()
                        ),
                        FF,
                        move || t4(pace, edit, wait),
                    );
                }
            }
        }
        let shapes: [(&str, &'static str, &'static str, (u16, &'static str)); 4] = [
            (
                "same",
                "git status --short",
                "git sattus --short",
                (7, "ta"),
            ),
            (
                "prefix",
                "git status --short",
                "git sattus --long",
                (7, "ta"),
            ),
            ("cd", "cd ../aterm", "cd ../atemr", (10, "rm")),
            ("make", "make test-all", "make tset-all", (7, "es")),
        ];
        for pace in PACES {
            for (name, above, typed, fix) in sweep(&shapes, &[shapes[0], shapes[2]]) {
                for how in [0u8, 1] {
                    for wait in sweep(
                        &[0u64, 16, 32, 48, 64, 90, 130, 180, 250, 400, 700],
                        &[0, 32, 90, 700],
                    ) {
                        case(
                            v,
                            format!(
                                "d4.t5.inplace_fix_then_kill/{}/{name}/how{how}/wait{wait}",
                                pace.tag()
                            ),
                            FF,
                            move || t5(pace, above, typed, fix, how, wait),
                        );
                    }
                }
            }
        }
    }
}

fn all_cases() -> Vec<Case> {
    let mut v = Vec::new();
    a2::cases(&mut v);
    a3::cases(&mut v);
    l3c::cases(&mut v);
    d4::cases(&mut v);
    v
}

/// **A LINE ERASED UNDER ITS TWIN GOES NOWHERE: `git status`**. The
/// command typed on the row under an identical line, the keys echoed one to
/// forty to a frame, the echo zero to three frames late, then erased by
/// Ctrl-U, Ctrl-W or Backspaces, on the full host and on the composed host
/// whose far-row read is dropped. RED on 0.93.0 240 of the 768 cases of the
/// four commands: keys armed on a frame that did not see the line above
/// read as arrived. The host samples a waiting key's row and its `±1`
/// (`Engine::ribbon_rows_for`), so the copy that was there first is seen on
/// the arming frame and is a twin at once; every other record learns it by
/// time (`TWIN_MIN`).
#[test]
fn a_line_erased_under_its_twin_goes_nowhere_git_status() {
    holds(all_cases(), "a2.f1.twin_erase/git_status/");
}

/// **…`cd ..`** — two glyphs, a space and two more.
#[test]
fn a_line_erased_under_its_twin_goes_nowhere_cd_dot_dot() {
    holds(all_cases(), "a2.f1.twin_erase/cd_../");
}

/// **…`cargo test -p aterm-effects`** — longer than a frame's keys.
#[test]
fn a_line_erased_under_its_twin_goes_nowhere_cargo_test() {
    holds(all_cases(), "a2.f1.twin_erase/cargo_test_-p_aterm-effects/");
}

/// **…`ls -la`.**
#[test]
fn a_line_erased_under_its_twin_goes_nowhere_ls_la() {
    holds(all_cases(), "a2.f1.twin_erase/ls_-la/");
}

/// **FZF'S LIST LANDS AFTER THE QUERY'S FIRST KEYS**: the input still
/// loading while the first one to four keys echo, then the list drawn with
/// the best match holding the query's text at its columns, above the prompt
/// or (`--layout=reverse`) below it; then Ctrl-U. RED on 0.93.0 12/24 +
/// 4/8: the list's glyphs read as arrived. The copy stood beside the query
/// long past `TWIN_MIN`, so its records are twins.
#[test]
fn fzf_s_list_landing_after_the_query_s_first_keys_is_not_where_the_query_went() {
    holds_where(all_cases(), |id| {
        id.starts_with("a2.f2.") || id.starts_with("a2.f2b.")
    });
}

/// **A COMPLETION MENU, A TWIN DRAWN LATE**: a popup opened after one to
/// three keys, standing zero to three frames; an identical line drawn beside
/// the command after its first keys; then the erase. GREEN on 0.93.0. The
/// records armed before the popup or the copy learn it by time; those armed
/// after it, at once.
#[test]
fn a_completion_menu_or_a_twin_drawn_late_is_not_where_the_line_went() {
    holds_where(all_cases(), |id| {
        id.starts_with("a2.f2c.") || id.starts_with("a2.f2d.")
    });
}

/// **A FULL ROW BUDGET STILL SEES THE TWIN**: six to ten other bands
/// resident, so the bands' rows overflow `WITNESS_ROWS`, then a line erased
/// under its twin. GREEN on 0.93.0. The arming row and its `±1` have slots
/// of their own after the bands' (`CURSOR_WITNESS_ROWS`), so the twin is
/// seen when the line is armed.
#[test]
fn a_full_row_budget_still_sees_the_twin() {
    holds(all_cases(), "a2.f3.");
}

/// **SCROLLS, BAND MOVES AND ALT SCREENS**: a line on the grid's bottom row
/// then a scroll that writes a job notice and a copy on the new last row
/// (`v0`); vim's duplicate-below (`yyp`, `:t.`) then `S` on the original; a
/// scroll and an erase on one frame; an alt-screen round trip. RED on
/// 0.93.0 2 of 7, for `yyp` (the copy put into the row a band move opened
/// read as arrived: `(14, 89)`). A band move leaves the row
/// it opens beside a line not clear until a frame shows it
/// (`Witness::translate_band`). LIMIT: `v1`, the line relocated onto a new
/// last row in the batch that erases it, before any frame sampled that row
/// — glyph for glyph a move
/// (`the_limit_a_copy_that_stood_less_than_twin_min_is_a_move` in
/// `tests/copy_beside_the_line.rs` asserts that it follows).
#[test]
fn scrolls_band_moves_and_alt_screens_do_not_make_a_copy_an_arrival() {
    holds_but(
        all_cases(),
        |id| {
            ["a2.f4", "a2.f5.", "a2.pre1."]
                .iter()
                .any(|p| id.starts_with(p))
        },
        |id| id == "a2.f4.bottom_edge_then_scroll/v1",
    );
}

/// **A STREAMED ROW QUOTES THE COMPOSER**: the last row of a streaming
/// region rewritten with each chunk; one chunk quotes the composer's text one
/// or two rows above it, and the hand clears the composer. The quote TWO
/// rows up is on a row no band names, and nothing is carried there (named,
/// it would be: `(20, 80)`). LIMIT, bad on 0.93.0 too: the quote ONE row up
/// (`gap1`), drawn on the one frame before the erase — the torn repaint's
/// shape, which `tests/copy_beside_the_line.rs`'s limit law requires to
/// follow.
#[test]
fn a_streamed_row_quoting_the_composer_two_rows_up_is_not_where_it_went() {
    holds_but(
        all_cases(),
        |id| id.starts_with("a2.f6."),
        |id| id.contains("/gap1/"),
    );
}

/// **FZF'S INFO LINE AND FIRST ITEM**: Ctrl-U and the next key in one SSH
/// batch under fzf's info line; a first item sharing only the query's
/// directory. GREEN on 0.93.0. The item two rows up is on a row no band
/// names; were it named, the query's `li` would read as arrived there past
/// `src/` standing as twins one row up (`ce1d`).
#[test]
fn fzf_s_info_line_and_first_item_are_not_where_the_query_went() {
    holds_where(all_cases(), |id| id.starts_with("a2.ce"));
}

/// **FZF RE-SORTS ITS LIST AS CTRL-U CLEARS THE QUERY**: the erase batch
/// itself redraws the full list, and an item holding the query's text lands
/// at the query's columns one or two rows away, with and without the next
/// key in the same batch. The two-rows-away layouts 0 and 2 hold because no
/// row two away is named. LIMIT, bad on 0.93.0 too: layouts 1 and 3, the
/// item ONE row away, written in the batch that erases the query — the line
/// relocated, glyph for glyph.
#[test]
fn fzf_re_sorting_its_list_under_ctrl_u_is_not_where_the_query_went() {
    holds_but(
        all_cases(),
        |id| id.starts_with("a3.h1."),
        |id| id.contains("/layout1/") || id.contains("/layout3/"),
    );
}

/// **THE COMPOSED HOST'S DROPPED FAR READ**: a split or zoomed pane reads
/// the rows past the caret's `±1` in a second, generation-checked lock and
/// drops them while a list streams above the query; then Ctrl-U (and the
/// next key). GREEN on 0.93.0. A row the host was asked for and did not
/// deliver defers only a run the follow pass could carry, for one walk, and
/// the rows it drops are never ones the copy stood on unseen.
#[test]
fn a_row_the_composed_host_drops_is_not_a_door_for_a_copy() {
    holds(all_cases(), "a3.h5.");
}

/// **A FULL ROW BUDGET, AN ECHO BEHIND A KEY**: two to four bands resident,
/// a query typed under a list, a pause, then Ctrl-U with the next key's
/// echo 51–90 ms behind it. GREEN on 0.93.0. The arming rows are the key's
/// row and its `±1` only, in slots of their own: naming the rows two away
/// on the key's frame let the list's copy read as arrived.
#[test]
fn a_full_row_budget_hides_no_copy_behind_a_key() {
    holds(all_cases(), "a3.h4");
}

/// **AN ALTERNATING FLICKER**: a copy of the command drawn and erased on
/// alternate frames one or two rows above it for 100–800 ms, then the line
/// erased. Two rows up holds: that row is not named. LIMIT, bad on 0.93.0
/// too: one row up (`gap1`), where the copy never stands `TWIN_MIN` at a
/// stretch.
#[test]
fn an_alternating_flicker_two_rows_up_is_not_where_the_line_went() {
    holds_but(
        all_cases(),
        |id| id.starts_with("a3.h3."),
        |id| id.contains("/gap1/"),
    );
}

/// The pause, in ms, of `id`'s `/pause<n>/` segment.
fn pause_of(id: &str) -> u64 {
    let at = id.find("/pause").expect("a paused case") + "/pause".len();
    id[at..]
        .split('/')
        .next()
        .and_then(|n| n.parse().ok())
        .expect("a pause")
}

/// **A COPY ERASED BEFORE THE WITNESS SAW IT STAND `TWIN_MIN`** — the torn
/// repaint's shape, a LIMIT of every `a3.p*` family: erased `pause` ms after
/// it landed. On the 16 ms trains (`flat`, and `pet`, whose frame demand
/// keeps them coming) a copy erased 40 ms after it landed was last seen at
/// most 32 ms after it was first seen. Under the no-pet pacing (`paced`) the
/// idle frames come 110–130 ms apart and the copy is seen on one frame only
/// until `paced_until` ms — measured per family, because it depends on
/// where the idle frame train stands when the copy lands.
fn erased_before_it_stood_twin_min(id: &str, paced_until: u64) -> bool {
    let pause = pause_of(id);
    pause <= 40 || (id.ends_with("/paced") && pause <= paced_until)
}

/// **A STREAMED QUOTE, THEN CTRL-U, AT EVERY PAUSE AND PACING**: the
/// composer quoted one or two rows up, then Ctrl-U 20–900 ms later, at the
/// flat 16 ms train, the no-pet style's real pacing, and the pet style's.
/// RED on 0.93.0 27/54 (every quote one row up). LIMIT, bad on 0.93.0 too:
/// one row up (`gap1`)
/// with the quote erased before it stood `TWIN_MIN`
/// ([`erased_before_it_stood_twin_min`], paced until 60 ms).
#[test]
fn a_streamed_quote_then_ctrl_u_at_every_pause_and_pacing() {
    holds_but(
        all_cases(),
        |id| id.starts_with("a3.p1."),
        |id| id.contains("/gap1/") && erased_before_it_stood_twin_min(id, 60),
    );
}

/// **FZF'S LATE LIST, THEN CTRL-U, AT EVERY PAUSE AND PACING**: the list
/// lands after four to six of the query's keys; Ctrl-U 20–900 ms after the
/// last. RED on 0.93.0 162/162. LIMIT, bad on 0.93.0 too: the list landing
/// after
/// the LAST key (`k6`) and erased before it stood `TWIN_MIN`
/// ([`erased_before_it_stood_twin_min`], paced until 150 ms).
#[test]
fn fzf_s_late_list_then_ctrl_u_at_every_pause_and_pacing() {
    holds_but(
        all_cases(),
        |id| id.starts_with("a3.p2."),
        |id| id.contains("/k6/") && erased_before_it_stood_twin_min(id, 150),
    );
}

/// **A COPY DRAWN AFTER THE LAST KEY, THEN ERASED**: a copy of the command
/// on the row above it, then the line cleared (a redraw or a kill) 20–900 ms
/// later. RED on 0.93.0 54/54. LIMIT, bad on 0.93.0 too: erased before the
/// copy stood `TWIN_MIN`
/// ([`erased_before_it_stood_twin_min`], paced until 90 ms).
#[test]
fn a_copy_drawn_after_the_last_key_then_erased_at_every_pause_and_pacing() {
    holds_but(
        all_cases(),
        |id| id.starts_with("a3.p3."),
        |id| erased_before_it_stood_twin_min(id, 90),
    );
}

/// **VIM'S `yyp`, THEN `S`, AT EVERY PAUSE AND PACING**: the put copy, then
/// `S` on the original 20–900 ms later. RED on 0.93.0 18/18. The row the
/// band move opened is not clear until a
/// frame shows it without the glyph, so the copy put into it is no arrival
/// however few frames sampled it (`Witness::translate_band`).
#[test]
fn vim_s_yyp_then_s_at_every_pause_and_pacing() {
    holds(all_cases(), "a3.p4.");
}

/// **THE LAWS OF `tests/copy_beside_the_line.rs` UNDER THE GUI'S REAL
/// PACING**: the same screens, framed as the GUI frames them once typing
/// stops — 16 ms only while a fade needs it, otherwise at the next change's
/// deadline, 110–130 ms apart. RED on 0.93.0 5 of 14. LIMIT, bad on 0.93.0
/// too: the
/// copy the scroll wrote on the new last row, erased with no sampled frame
/// between — the relocation's shape, which follows.
#[test]
fn the_laws_of_copy_beside_the_line_hold_under_the_gui_s_real_pacing() {
    holds_but(
        all_cases(),
        |id| id.starts_with("L3.copy.") && id.ends_with("/paced"),
        |id| id.starts_with("L3.copy.copy_on_new_last_row_by_scroll/"),
    );
}

/// **A GLYPH CAUGHT UP UNDER ITS TWIN IS ARMED AGAIN**: `git status --short`
/// typed under the identical line, then two interior glyphs overwritten and
/// restored 90 ms later (or a torn read of the row, or nothing), then Ctrl-U
/// 0–200 ms on — at 16 and 8 ms frames and under the GUI's pacing with and
/// without the pet. The shape pass forgives the interior rewrite and the
/// records catch up to the glyphs standing now; a record caught up is ARMED
/// again on that frame (`Witness::shape_verdicts`), so the restored glyphs
/// are twins of the line above at once and the kill is an erase under a
/// twin. GREEN on 0.93.0. Forgotten and learned again only by time, the
/// restored glyphs carry the band onto the previous command on every kill
/// within `TWIN_MIN` of the restore (24 of the 128).
#[test]
fn a_glyph_caught_up_under_its_twin_is_armed_again() {
    holds(all_cases(), "d4.t4.");
}

/// The wait, in ms, of `id`'s `/wait<n>` segment.
fn wait_of(id: &str) -> u64 {
    let at = id.find("/wait").expect("a waited case") + "/wait".len();
    id[at..]
        .split('/')
        .next()
        .and_then(|n| n.parse().ok())
        .expect("a wait")
}

/// **THE LIMIT: A LINE MOVED BESIDE ITS TWIN AND KILLED BEFORE THE TWIN WAS
/// LEARNED LIGHTS THE TWIN.** `test lib` typed on row 9 under a blank row
/// (or a near copy, `hest liv`) and over `$ test lib` two rows down; the
/// line relocated one row down in one write, onto row 10 beside the
/// identical line, and Ctrl-U 16–400 ms later — at 16 and 8 ms frames and
/// under the GUI's pacing with and without the pet. A record a follow
/// carries starts over (`Witness::translate_cells`): its evidence was about
/// the rows around its old row. The identical line one row below its new
/// row is a twin only once the witness has seen it stand there `TWIN_MIN`,
/// and it is first seen on the frame after the move. A kill before that
/// finds every glyph "arrived" one row down and carries the band onto the
/// identical line. Every rule-named case lights it, stated so that a change
/// is seen: on 16 ms frames, the pet's and the no-pet pacing a kill up to
/// 48 ms after the move, on 8 ms frames up to 32 ms. RESTATED 2026-09-26 on
/// the tree that carries main's rainbow flow (e437f05f7): the no-pet pacing
/// lit it up to 130 ms before, its frames after the move coming 110–130 ms
/// apart; the band the move carried now keeps the host framing it as it
/// flows, the twin is seen standing `TWIN_MIN` by 64 ms, and a kill at 64,
/// 90 or 130 ms goes nowhere (measured on the full grid). A later kill goes
/// nowhere. 0.93.0 lit the identical line on every case with the blank
/// row between — the move followed there too, and the same kill carried it
/// on; with the near copy between it missed the move itself (a near copy
/// read as the line rewritten in place), so there was no band to carry. Not
/// learning the copy seen on the carried record's first look as a twin at
/// once is deliberate: that same look, with the line then moved onto the
/// copy's row, is a real move, which it would refuse — glyph for glyph the
/// two cannot be told apart (the witness's THE LIMIT).
#[test]
fn the_limit_a_line_moved_beside_its_twin_and_killed_before_it_was_learned_lights_it() {
    holds_but(
        all_cases(),
        |id| id.starts_with("d4.t7."),
        |id| {
            let wait = wait_of(id);
            (id.contains("/fr8/") && wait <= 32)
                || ((id.contains("/fr16/") || id.contains("/pet/") || id.contains("/paced/"))
                    && wait <= 48)
        },
    );
}

/// **A TYPO FIXED IN PLACE UNDER THE PREVIOUS COMMAND, THEN KILLED, GOES
/// NOWHERE**: `git sattus --short` typed under `git status --short` (and
/// three more shapes), the typo fixed in place — Ctrl-T's echo in one
/// batch, or two vi `r`s 150 ms apart — which makes the line agree with the
/// previous command at those columns, then Ctrl-U 0–700 ms later. The fixed
/// glyphs are armed again on the frame that shows them, beside the copy
/// that was there first: twins at once. GREEN on 0.93.0. Learned only by
/// time, a kill within `TWIN_MIN` of the fix lights the previous command
/// (18 of the 352).
#[test]
fn a_typo_fixed_in_place_under_the_previous_command_then_killed_goes_nowhere() {
    holds(all_cases(), "d4.t5.");
}
