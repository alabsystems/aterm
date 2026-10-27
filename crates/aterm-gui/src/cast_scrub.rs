// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A streaming filter over a cast's output that keeps what paints and drops what a shared file must not carry: the shell-integration nonce, clipboard, notification and user-variable payloads, queries, file-backed images and passthrough strings.

/// The longest OSC kept whole; a longer one is dropped.
const MAX_KEPT_OSC: usize = 64 * 1024;

/// The longest DCS or APC header read before its string is judged; a longer one is dropped.
const MAX_HEAD: usize = 4096;

/// The OSC commands a cast keeps: titles, the working directory, hyperlinks, colours and their resets, and the shell marks (less their nonce).
const KEPT_OSC: &[u32] = &[
    0, 1, 2, 4, 7, 8, 10, 11, 12, 17, 19, 21, 104, 110, 111, 112, 117, 119, 133, 633,
];

/// The OSC 1337 prefix of an iTerm2 image or file, whose arguments end at the first `:`.
const ITERM_FILE: &[u8] = b"1337;File=";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Ground,
    /// After an ESC.
    Esc,
    /// Inside an OSC, buffering it.
    Osc,
    /// Inside an OSC, after an ESC that may begin its ST.
    OscEsc,
    /// A DCS (`apc` false) or APC header, buffered until it shows whether the string paints.
    Head {
        apc: bool,
    },
    /// [`State::Head`] after an ESC that may begin its ST.
    HeadEsc {
        apc: bool,
    },
    /// Inside a string that paints (a sixel or kitty image, or an inline iTerm2 image when `osc`), passed through as it comes.
    Pass {
        osc: bool,
    },
    /// [`State::Pass`] after an ESC that may begin its ST.
    PassEsc {
        osc: bool,
    },
    /// Inside a string that is dropped; `osc` when BEL ends it.
    Drop {
        osc: bool,
    },
    /// Inside a dropped string, after an ESC that may begin its ST (a doubled ESC, as tmux passthrough escapes one, stays inside).
    DropEsc {
        osc: bool,
    },
    /// At a start whose parser state is not known: text held until a terminator shows it was a string's tail, or an ESC shows it was text.
    Unsure,
    /// [`State::Unsure`] after an ESC.
    UnsureEsc,
}

/// One cast's filter: its state runs across every slice, so a sequence split between two slices is judged whole.
pub(crate) struct CastScrub {
    state: State,
    buf: Vec<u8>,
}

impl CastScrub {
    /// A filter for a stream that starts with the parser at ground (`true`), or at a state not known (`false`), as a recording that dropped its oldest bytes does.
    pub(crate) const fn starting(at_ground: bool) -> Self {
        Self {
            state: if at_ground {
                State::Ground
            } else {
                State::Unsure
            },
            buf: Vec::new(),
        }
    }

    /// The bytes of `bytes` a cast keeps, appended to `out`.
    pub(crate) fn feed(&mut self, bytes: &[u8], out: &mut Vec<u8>) {
        for &b in bytes {
            self.step(b, out);
        }
    }

    /// Start over at `at_ground`, dropping any string still open: a painted screen stands in for what came before it.
    pub(crate) fn restart(&mut self, at_ground: bool) {
        *self = Self::starting(at_ground);
    }

    /// The end of the stream: text still held at an unknown start is let out, an open string is dropped.
    pub(crate) fn finish(&mut self, out: &mut Vec<u8>) {
        if matches!(self.state, State::Unsure | State::UnsureEsc) {
            out.append(&mut self.buf);
            if self.state == State::UnsureEsc {
                out.push(0x1b);
            }
        }
        self.restart(true);
    }

    fn step(&mut self, b: u8, out: &mut Vec<u8>) {
        const ESC: u8 = 0x1b;
        const BEL: u8 = 0x07;
        let cancel = b == 0x18 || b == 0x1a;
        match self.state {
            State::Ground => {
                if b == ESC {
                    self.state = State::Esc;
                } else {
                    out.push(b);
                }
            }
            State::Esc => match b {
                b']' | b'P' | b'_' => {
                    self.buf.clear();
                    self.state = match b {
                        b']' => State::Osc,
                        b'P' => State::Head { apc: false },
                        _ => State::Head { apc: true },
                    };
                }
                b'^' | b'X' => self.state = State::Drop { osc: false },
                ESC => out.push(ESC),
                _ => {
                    out.extend_from_slice(&[ESC, b]);
                    self.state = State::Ground;
                }
            },
            State::Osc => match b {
                BEL => self.end_osc(false, out),
                ESC => self.state = State::OscEsc,
                _ if cancel => self.abort(b, out),
                _ if self.buf.len() >= MAX_KEPT_OSC => {
                    self.buf.clear();
                    self.state = State::Drop { osc: true };
                }
                b':' if self.buf.starts_with(ITERM_FILE) => {
                    let args = &self.buf[ITERM_FILE.len()..];
                    if args.split(|&c| c == b';').any(|a| a == b"inline=1") {
                        out.extend_from_slice(b"\x1b]");
                        out.append(&mut self.buf);
                        out.push(b':');
                        self.state = State::Pass { osc: true };
                    } else {
                        self.buf.clear();
                        self.state = State::Drop { osc: true };
                    }
                }
                _ => self.buf.push(b),
            },
            State::OscEsc => {
                self.end_osc(true, out);
                if b != b'\\' {
                    self.state = State::Esc;
                    self.step(b, out);
                }
            }
            State::Head { apc } => match b {
                ESC => self.state = State::HeadEsc { apc },
                _ if cancel => self.abort(b, out),
                _ if self.buf.len() >= MAX_HEAD => {
                    self.buf.clear();
                    self.state = State::Drop { osc: false };
                }
                _ if apc && b == b';' => self.judge_apc(Some(b), out),
                _ if !apc && (0x40..=0x7e).contains(&b) => {
                    let sixel = b == b'q' && self.buf.iter().all(|c| (0x30..=0x3f).contains(c));
                    if sixel {
                        out.extend_from_slice(b"\x1bP");
                        out.append(&mut self.buf);
                        out.push(b);
                        self.state = State::Pass { osc: false };
                    } else {
                        self.buf.clear();
                        self.state = State::Drop { osc: false };
                    }
                }
                _ => self.buf.push(b),
            },
            State::HeadEsc { apc } => {
                if b == b'\\' {
                    if apc {
                        self.judge_apc(None, out);
                        if let State::Pass { .. } = self.state {
                            out.extend_from_slice(b"\x1b\\");
                        }
                    }
                    self.buf.clear();
                    self.state = State::Ground;
                } else {
                    self.buf.clear();
                    self.state = State::Esc;
                    self.step(b, out);
                }
            }
            State::Pass { osc } => match b {
                BEL if osc => {
                    out.push(BEL);
                    self.state = State::Ground;
                }
                ESC => self.state = State::PassEsc { osc },
                _ if cancel => self.abort(b, out),
                _ => out.push(b),
            },
            State::PassEsc { .. } => {
                out.extend_from_slice(b"\x1b\\");
                self.state = State::Ground;
                if b != b'\\' {
                    self.state = State::Esc;
                    self.step(b, out);
                }
            }
            State::Drop { osc } => match b {
                BEL if osc => self.state = State::Ground,
                ESC => self.state = State::DropEsc { osc },
                _ if cancel => self.abort(b, out),
                _ => {}
            },
            State::DropEsc { osc } => match b {
                b'\\' => self.state = State::Ground,
                ESC => self.state = State::Drop { osc },
                _ => {
                    self.state = State::Esc;
                    self.step(b, out);
                }
            },
            State::Unsure => match b {
                BEL => {
                    self.buf.clear();
                    self.state = State::Ground;
                }
                ESC => self.state = State::UnsureEsc,
                _ if cancel => self.abort(b, out),
                _ => self.buf.push(b),
            },
            State::UnsureEsc => {
                if b == b'\\' {
                    self.buf.clear();
                    self.state = State::Ground;
                } else {
                    out.append(&mut self.buf);
                    self.state = State::Esc;
                    self.step(b, out);
                }
            }
        }
    }

    /// CAN or SUB `b` cancelled the string: drop what is buffered and keep `b`, which the parser acts on.
    fn abort(&mut self, b: u8, out: &mut Vec<u8>) {
        self.buf.clear();
        out.push(b);
        self.state = State::Ground;
    }

    /// The OSC just ended (by ST when `st`, else BEL): write what a cast keeps of it.
    fn end_osc(&mut self, st: bool, out: &mut Vec<u8>) {
        self.state = State::Ground;
        let osc = std::mem::take(&mut self.buf);
        if let Some(kept) = kept_osc(&osc) {
            out.extend_from_slice(b"\x1b]");
            out.extend_from_slice(&kept);
            out.extend_from_slice(if st { b"\x1b\\" } else { b"\x07" });
        }
    }

    /// A kitty graphics APC's control data is buffered, ended by `sep` (`;`) or by its ST: pass it when it carries its image itself, drop it when it names a file or shared memory, queries, or is no kitty command.
    fn judge_apc(&mut self, sep: Option<u8>, out: &mut Vec<u8>) {
        let head = std::mem::take(&mut self.buf);
        let paints = head.first() == Some(&b'G')
            && !head[1..]
                .split(|&c| c == b',')
                .any(|kv| matches!(kv, b"t=f" | b"t=t" | b"t=s" | b"a=q"));
        if paints {
            out.extend_from_slice(b"\x1b_");
            out.extend_from_slice(&head);
            out.extend(sep);
            self.state = State::Pass { osc: false };
        } else {
            self.state = State::Drop { osc: false };
        }
    }
}

/// `events` with each output's bytes scrubbed by one filter over the whole stream, which starts at parser ground when `at_ground`; an output left with no bytes is dropped.
pub(crate) fn scrubbed<D: AsRef<[u8]>>(
    events: &[aterm_control::asciicast::CastEvent<D>],
    at_ground: bool,
) -> Vec<aterm_control::asciicast::CastEvent<Vec<u8>>> {
    use aterm_control::asciicast::CastEvent;
    let mut scrub = CastScrub::starting(at_ground);
    let mut out = Vec::with_capacity(events.len());
    let mut last_t = 0.0;
    for ev in events {
        match ev {
            CastEvent::Output { t, data } => {
                let mut kept = Vec::new();
                scrub.feed(data.as_ref(), &mut kept);
                last_t = *t;
                if !kept.is_empty() {
                    out.push(CastEvent::Output { t: *t, data: kept });
                }
            }
            CastEvent::Resize { t, cols, rows } => out.push(CastEvent::Resize {
                t: *t,
                cols: *cols,
                rows: *rows,
            }),
        }
    }
    let mut rest = Vec::new();
    scrub.finish(&mut rest);
    if !rest.is_empty() {
        out.push(CastEvent::Output {
            t: last_t,
            data: rest,
        });
    }
    out
}

/// What a cast keeps of OSC body `osc` (between `ESC ]` and its terminator): `None` drops it.
fn kept_osc(osc: &[u8]) -> Option<Vec<u8>> {
    let mut params = osc.split(|&b| b == b';');
    let command = std::str::from_utf8(params.next()?)
        .ok()?
        .parse::<u32>()
        .ok()?;
    if !KEPT_OSC.contains(&command) {
        return None;
    }
    let rest: Vec<&[u8]> = params.collect();
    if matches!(command, 133 | 633) {
        let mut kept = command.to_string().into_bytes();
        for (i, param) in rest.iter().enumerate() {
            if i >= 1 && param.starts_with(b"id=") {
                continue;
            }
            kept.push(b';');
            kept.extend_from_slice(param);
        }
        return Some(kept);
    }
    let colour = matches!(command, 4 | 10 | 11 | 12 | 17 | 19 | 21);
    let query = colour && rest.iter().any(|p| *p == b"?" || p.ends_with(b"=?"));
    (!query).then(|| osc.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scrub(slices: &[&[u8]]) -> Vec<u8> {
        let mut scrub = CastScrub::starting(true);
        let mut out = Vec::new();
        for slice in slices {
            scrub.feed(slice, &mut out);
        }
        scrub.finish(&mut out);
        out
    }

    #[test]
    fn a_shell_mark_keeps_its_kind_and_loses_its_nonce_even_split_across_slices() {
        let nonce = "3c".repeat(32);
        let marks = format!(
            "\x1b]133;A;id={nonce}\x07$ \x1b]133;B;id={nonce};aterm_input=start\x1b\\ls\r\n\
             \x1b]133;D;0;id={nonce}\x07\x1b]633;E;ls -la;id={nonce}\x07\x1b]633;P;Cwd=/tmp;id={nonce}\x07"
        );
        let bytes = marks.as_bytes();
        for cut in 0..bytes.len() {
            let out = scrub(&[&bytes[..cut], &bytes[cut..]]);
            let text = String::from_utf8(out).expect("utf-8");
            assert!(!text.contains(&nonce), "cut at {cut}: {text:?}");
            assert_eq!(
                text,
                "\x1b]133;A\x07$ \x1b]133;B;aterm_input=start\x1b\\ls\r\n\x1b]133;D;0\x07\
                 \x1b]633;E;ls -la\x07\x1b]633;P;Cwd=/tmp\x07",
                "cut at {cut}"
            );
        }
    }

    #[test]
    fn invisible_payloads_and_queries_go_and_what_paints_stays() {
        let out = scrub(&[
            b"a\x1b]52;c;c2VjcmV0\x07b",
            b"\x1b]1337;SetUserVar=k=dg==\x07c\x1bPtmux;\x1b\x1b]52;c;eA==\x07\x1b\\d",
            b"\x1bP$qm\x1b\\e\x1b]9;hi\x07\x1b]10;?\x07\x1b]4;1;?\x07\x1b_Ga=q,i=1;AAAA\x1b\\",
            b"\x1b]0;title\x07\x1b]8;;https://x\x1b\\link\x1b]8;;\x1b\\\x1b[31mred\x1b[m\x1b(0q\x1b(B",
        ]);
        assert_eq!(
            out,
            b"abcde\x1b]0;title\x07\x1b]8;;https://x\x1b\\link\x1b]8;;\x1b\\\x1b[31mred\x1b[m\x1b(0q\x1b(B"
        );
    }

    #[test]
    fn an_image_that_carries_its_pixels_passes_and_one_naming_a_file_goes() {
        let sixel = b"\x1bP0;1q\"1;1;2;2#0;2;0;0;0#0~~\x1b\\";
        let kitty = b"\x1b_Ga=T,f=100;iVBORw0KGgo=\x1b\\";
        let iterm = b"\x1b]1337;File=name=eA==;inline=1:R0lGODlh\x07";
        let mut shown = Vec::new();
        for image in [&sixel[..], kitty, iterm] {
            shown.extend_from_slice(image);
            shown.push(b'.');
        }
        for cut in 0..shown.len() {
            assert_eq!(
                scrub(&[&shown[..cut], &shown[cut..]]),
                shown,
                "cut at {cut}"
            );
        }
        let hidden: &[&[u8]] = &[
            b"\x1b_Ga=T,t=f;L2V0Yy9wYXNzd2Q=\x1b\\",
            b"\x1b_Gt=s,a=T;c2htLW5hbWU=\x1b\\",
            b"\x1b]1337;File=name=eA==;size=3:c2VjcmV0\x07",
            b"\x1bP1$r0m\x1b\\",
            b"\x1bP+q544e\x1b\\",
        ];
        for bytes in hidden {
            assert_eq!(scrub(&[bytes, b"."]), b".", "{bytes:?}");
        }
    }

    #[test]
    fn an_unknown_start_drops_a_strings_tail_and_lets_text_out() {
        let tail = |bytes: &[u8]| {
            let mut scrub = CastScrub::starting(false);
            let mut out = Vec::new();
            scrub.feed(bytes, &mut out);
            scrub.finish(&mut out);
            out
        };
        let nonce = "ab".repeat(32);
        assert_eq!(tail(format!(";id={nonce}\x07$ ").as_bytes()), b"$ ");
        assert_eq!(tail(format!(";id={nonce}\x1b\\$ ").as_bytes()), b"$ ");
        assert_eq!(tail(b"plain text\x1b[1mbold"), b"plain text\x1b[1mbold");
        assert_eq!(tail(b"no escape at all"), b"no escape at all");
    }

    #[test]
    fn a_restart_drops_a_string_left_open() {
        let mut scrub = CastScrub::starting(true);
        let mut out = Vec::new();
        scrub.feed(b"x\x1b]52;c;c2Vj", &mut out);
        scrub.restart(true);
        scrub.feed(b"y\x1b]2;t\x07", &mut out);
        assert_eq!(out, b"xy\x1b]2;t\x07");
    }
}
