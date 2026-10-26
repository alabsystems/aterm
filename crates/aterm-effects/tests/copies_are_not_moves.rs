// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A copy of a line is not where the line went, at every timing and host.

use crate::trail_host::*;
use aterm_effects::kitty_pet::PetBrain;
use std::time::Duration;

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
        case(
            v,
            "a2.ce1d.fzf_first_item_shares_directory",
            FF,
            ce1d_first_item_shares_only_the_directory,
        );
    }
}

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

mod d4 {
    use super::*;

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

#[test]
fn a_line_erased_under_its_twin_goes_nowhere() {
    holds(all_cases(), "a2.f1.twin_erase/");
}

#[test]
fn fzf_s_list_landing_after_the_query_s_first_keys_is_not_where_the_query_went() {
    holds_where(all_cases(), |id| {
        id.starts_with("a2.f2.") || id.starts_with("a2.f2b.")
    });
}

#[test]
fn a_completion_menu_or_a_twin_drawn_late_is_not_where_the_line_went() {
    holds_where(all_cases(), |id| {
        id.starts_with("a2.f2c.") || id.starts_with("a2.f2d.")
    });
}

#[test]
fn a_full_row_budget_still_sees_the_twin() {
    holds(all_cases(), "a2.f3.");
}

#[test]
fn scrolls_band_moves_and_alt_screens_do_not_make_a_copy_an_arrival() {
    holds_but(
        all_cases(),
        |id| ["a2.f4", "a2.f5."].iter().any(|p| id.starts_with(p)),
        |id| id == "a2.f4.bottom_edge_then_scroll/v1",
    );
}

#[test]
fn a_streamed_row_quoting_the_composer_two_rows_up_is_not_where_it_went() {
    holds_but(
        all_cases(),
        |id| id.starts_with("a2.f6."),
        |id| id.contains("/gap1/"),
    );
}

#[test]
fn fzf_s_info_line_and_first_item_are_not_where_the_query_went() {
    holds_where(all_cases(), |id| id.starts_with("a2.ce"));
}

#[test]
fn fzf_re_sorting_its_list_under_ctrl_u_is_not_where_the_query_went() {
    holds_but(
        all_cases(),
        |id| id.starts_with("a3.h1."),
        |id| id.contains("/layout1/") || id.contains("/layout3/"),
    );
}

#[test]
fn a_row_the_composed_host_drops_is_not_a_door_for_a_copy() {
    holds(all_cases(), "a3.h5.");
}

#[test]
fn a_full_row_budget_hides_no_copy_behind_a_key() {
    holds(all_cases(), "a3.h4");
}

#[test]
fn an_alternating_flicker_two_rows_up_is_not_where_the_line_went() {
    holds_but(
        all_cases(),
        |id| id.starts_with("a3.h3."),
        |id| id.contains("/gap1/"),
    );
}

fn pause_of(id: &str) -> u64 {
    let at = id.find("/pause").expect("a paused case") + "/pause".len();
    id[at..]
        .split('/')
        .next()
        .and_then(|n| n.parse().ok())
        .expect("a pause")
}

fn erased_before_it_stood_twin_min(id: &str, paced_until: u64) -> bool {
    let pause = pause_of(id);
    pause <= 40 || (id.ends_with("/paced") && pause <= paced_until)
}

#[test]
fn a_streamed_quote_then_ctrl_u_at_every_pause_and_pacing() {
    holds_but(
        all_cases(),
        |id| id.starts_with("a3.p1."),
        |id| id.contains("/gap1/") && erased_before_it_stood_twin_min(id, 60),
    );
}

#[test]
fn fzf_s_late_list_then_ctrl_u_at_every_pause_and_pacing() {
    holds_but(
        all_cases(),
        |id| id.starts_with("a3.p2."),
        |id| id.contains("/k6/") && erased_before_it_stood_twin_min(id, 150),
    );
}

#[test]
fn a_copy_drawn_after_the_last_key_then_erased_at_every_pause_and_pacing() {
    holds_but(
        all_cases(),
        |id| id.starts_with("a3.p3."),
        |id| erased_before_it_stood_twin_min(id, 90),
    );
}

#[test]
fn vim_s_yyp_then_s_at_every_pause_and_pacing() {
    holds(all_cases(), "a3.p4.");
}

#[test]
fn a_copy_beside_the_line_is_not_where_the_line_went_flat_and_paced() {
    holds_but(
        all_cases(),
        |id| id.starts_with("L3.copy."),
        |id| id.starts_with("L3.copy.copy_on_new_last_row_by_scroll/") && id.ends_with("/paced"),
    );
}

#[test]
fn a_glyph_caught_up_under_its_twin_is_armed_again() {
    holds(all_cases(), "d4.t4.");
}

fn wait_of(id: &str) -> u64 {
    let at = id.find("/wait").expect("a waited case") + "/wait".len();
    id[at..]
        .split('/')
        .next()
        .and_then(|n| n.parse().ok())
        .expect("a wait")
}

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

#[test]
fn a_typo_fixed_in_place_under_the_previous_command_then_killed_goes_nowhere() {
    holds(all_cases(), "d4.t5.");
}
