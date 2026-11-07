// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Runs of text lines handed to a sink whole (`ActionSink::print_lines`) against the same bytes
//! delivered one action at a time: for a sink that takes runs and delivers them as the trait's
//! default does ([`crate::deliver_lines`]), every generated stream, read in any chunks, must
//! give the same actions in the same order and grouping as for a sink that takes none.

use aterm_provenance::{Provenance, Pty};

use super::*;

/// One action as the sink saw it, bulk prints kept whole so their grouping is compared too.
#[derive(Debug, PartialEq)]
enum Seen {
    Print(char),
    Ascii(Vec<u8>),
    Unicode(Vec<char>),
    Execute(u8),
    Csi(Vec<u16>, Vec<u8>, u8, u32),
    Esc(Vec<u8>, u8),
    Osc(Vec<Vec<u8>>, bool),
    DcsHook(Vec<u16>, Vec<u8>, u8),
    DcsPut(Vec<u8>),
    DcsUnhook(bool),
    ApcStart,
    ApcPut(Vec<u8>),
    ApcEnd(bool),
}

/// Records every action in order. `LINES` says whether it takes runs of lines; `drop_last`
/// plants the fault the negative control looks for, a run delivered short of its last byte.
#[derive(Default)]
struct Ordered<const LINES: bool> {
    seen: Vec<Seen>,
    drop_last: bool,
    runs: u64,
}

impl<const LINES: bool> ActionSink for Ordered<LINES> {
    const TAKES_LINES: bool = LINES;

    fn print(&mut self, c: char) {
        self.seen.push(Seen::Print(c));
    }
    fn print_ascii_bulk(&mut self, data: &Provenance<[u8], Pty>) {
        self.seen.push(Seen::Ascii(data.as_ref().to_vec()));
    }
    fn print_unicode_bulk(&mut self, chars: &Provenance<[char], Pty>) {
        self.seen.push(Seen::Unicode(chars.as_ref().to_vec()));
    }
    fn print_lines(&mut self, lines: &Provenance<[u8], Pty>) {
        let lines = lines.as_ref();
        assert_eq!(lines.first(), Some(&b'\n'), "a run starts at a line feed");
        assert!(
            lines
                .iter()
                .all(|&b| (0x20..=0x7E).contains(&b) || b == b'\n' || b == b'\r'),
            "a run holds only printable ASCII, CR and LF: {lines:?}"
        );
        self.runs += 1;
        let keep = lines.len() - usize::from(self.drop_last);
        crate::deliver_lines(self, &lines[..keep]);
    }
    fn execute(&mut self, byte: u8) {
        self.seen.push(Seen::Execute(byte));
    }
    fn csi_dispatch(
        &mut self,
        params: &Provenance<[u16], Pty>,
        intermediates: &Provenance<[u8], Pty>,
        final_byte: u8,
    ) {
        self.csi_dispatch_with_subparams(params, intermediates, final_byte, 0);
    }
    fn csi_dispatch_with_subparams(
        &mut self,
        params: &Provenance<[u16], Pty>,
        intermediates: &Provenance<[u8], Pty>,
        final_byte: u8,
        subparam_mask: u32,
    ) {
        self.seen.push(Seen::Csi(
            params.as_ref().to_vec(),
            intermediates.as_ref().to_vec(),
            final_byte,
            subparam_mask,
        ));
    }
    fn esc_dispatch(&mut self, intermediates: &Provenance<[u8], Pty>, final_byte: u8) {
        self.seen
            .push(Seen::Esc(intermediates.as_ref().to_vec(), final_byte));
    }
    fn osc_dispatch_with_terminator(
        &mut self,
        params: &Provenance<[&[u8]], Pty>,
        bel_terminated: bool,
    ) {
        self.seen.push(Seen::Osc(
            params.as_ref().iter().map(|p| p.to_vec()).collect(),
            bel_terminated,
        ));
    }
    fn dcs_hook(
        &mut self,
        params: &Provenance<[u16], Pty>,
        intermediates: &Provenance<[u8], Pty>,
        final_byte: u8,
    ) {
        self.seen.push(Seen::DcsHook(
            params.as_ref().to_vec(),
            intermediates.as_ref().to_vec(),
            final_byte,
        ));
    }
    fn dcs_put(&mut self, byte: u8) {
        self.seen.push(Seen::DcsPut(vec![byte]));
    }
    fn dcs_put_bulk(&mut self, data: &Provenance<[u8], Pty>) {
        self.seen.push(Seen::DcsPut(data.as_ref().to_vec()));
    }
    fn dcs_unhook(&mut self, canceled: bool) {
        self.seen.push(Seen::DcsUnhook(canceled));
    }
    fn apc_start(&mut self) {
        self.seen.push(Seen::ApcStart);
    }
    fn apc_put(&mut self, byte: u8) {
        self.seen.push(Seen::ApcPut(vec![byte]));
    }
    fn apc_put_bulk(&mut self, data: &Provenance<[u8], Pty>) {
        self.seen.push(Seen::ApcPut(data.as_ref().to_vec()));
    }
    fn apc_end(&mut self, canceled: bool) {
        self.seen.push(Seen::ApcEnd(canceled));
    }
}

/// A seeded xorshift stream.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        usize::try_from(self.0 % n.max(1) as u64).unwrap_or(0)
    }
}

const TOKENS: &[&[u8]] = &[
    b"\n",
    b"\r\n",
    b"\n\n\n",
    b"\r",
    b"y\n",
    b"y\ny\ny\ny\n",
    b"the quick brown fox",
    b" ",
    b"~}|",
    b"\x7f",
    b"\x08\t\x07",
    b"\x0b\x0c",
    b"\x1b[31m",
    b"\x1b[1;2r",
    b"\x1b[38:2::1:2:3m",
    b"\x1b]0;title\x07",
    b"\x1b]8;;http://x/\x1b\\",
    b"\x1bP$qm\x1b\\",
    b"\x1b_Gi=1;AAAA\x1b\\",
    b"\x1bPtmux;\x1b\x1b[5;7Hx\n\x1b\\",
    b"\x1bPtmux;y\ny\r\ny\n",
    b"\x1b\\",
    b"\x1bD",
    b"\x1b",
    b"\x1b[",
    b"\x85\x9b1m",
    b"\xc3\xa9",
    b"\xe4\xb8\x80",
    b"\xf0\x9f\x98\x80",
    b"\xe4\xb8",
    b"\xff",
    b"\x18",
];

fn stream(rng: &mut Rng) -> Vec<u8> {
    let mut out = Vec::new();
    for _ in 0..2 + rng.below(40) {
        out.extend_from_slice(TOKENS[rng.below(TOKENS.len())]);
    }
    out
}

/// Feed `bytes` in the chunks `cuts` names to a fresh parser and `sink`.
fn feed<S: ActionSink>(c1: bool, bytes: &[u8], cuts: &[usize], sink: &mut S) {
    let mut parser = Parser::new();
    parser.c1_controls_enabled = c1;
    let mut from = 0;
    for &cut in cuts {
        parser.advance_fast(&bytes[from..cut], sink);
        from = cut;
    }
}

/// The first generated stream on which a run-taking sink (with `drop_last` planted or not) sees
/// other actions than a sink that takes none; else how many runs the parser handed over.
fn first_divergence(drop_last: bool, cases: u64) -> Result<u64, String> {
    let mut runs = 0;
    for seed in 1..=cases {
        let mut rng = Rng(0xD1B5_4A32_D192_ED03 ^ seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let bytes = stream(&mut rng);
        let mut cuts: Vec<usize> = (0..rng.below(5))
            .map(|_| rng.below(bytes.len() + 1))
            .collect();
        cuts.push(bytes.len());
        cuts.sort_unstable();
        let c1 = rng.below(4) == 0;
        let mut lines = Ordered::<true> {
            drop_last,
            ..Ordered::default()
        };
        let mut single = Ordered::<false>::default();
        feed(c1, &bytes, &cuts, &mut lines);
        feed(c1, &bytes, &cuts, &mut single);
        if lines.seen != single.seen {
            return Err(format!(
                "seed {seed} c1={c1} cuts {cuts:?} of {:?}\nruns:\n{:?}\none at a time:\n{:?}",
                String::from_utf8_lossy(&bytes),
                lines.seen,
                single.seen
            ));
        }
        runs += lines.runs;
    }
    Ok(runs)
}

#[test]
fn a_run_of_lines_reads_as_its_actions_one_at_a_time() {
    match first_divergence(false, 20_000) {
        Err(divergence) => {
            panic!("runs of lines deliver other actions than the bytes one at a time: {divergence}")
        }
        Ok(runs) => assert!(runs > 20_000, "only {runs} runs were handed over"),
    }
}

#[test]
fn the_differential_catches_a_run_delivered_short() {
    assert!(
        first_divergence(true, 2_000).is_err(),
        "a sink that drops a run's last byte saw the actions a sink that takes none sees"
    );
}

#[test]
fn the_line_run_scanner_stops_at_the_first_other_byte() {
    for len in 0..70 {
        for at in 0..=len {
            let mut bytes: Vec<u8> = (0..len)
                .map(|i| match i % 5 {
                    0 => b'\n',
                    1 => b'\r',
                    2 => b' ',
                    3 => b'~',
                    _ => b'y',
                })
                .collect();
            for other in [0x00, 0x08, 0x1B, 0x1F, 0x7F, 0x80, 0xFF] {
                if at < len {
                    bytes[at] = other;
                }
                let want = if at < len { at } else { len };
                assert_eq!(
                    crate::simd::count_line_run(&bytes),
                    want,
                    "len {len}, {other:#04x} at {at}"
                );
            }
        }
    }
}

#[test]
fn a_line_ends_at_its_first_byte_outside_printable_ascii() {
    for len in 0..70 {
        for at in 0..=len {
            let mut bytes: Vec<u8> = (0..len)
                .map(|i| match i % 4 {
                    0 => b' ',
                    1 => b'~',
                    2 => b'y',
                    _ => b'A',
                })
                .collect();
            for other in [0x00, b'\n', b'\r', 0x1B, 0x1F, 0x7F, 0x80, 0xFF] {
                if at < len {
                    bytes[at] = other;
                }
                let want = if at < len { at } else { len };
                assert_eq!(
                    crate::printable_len(&bytes),
                    want,
                    "len {len}, {other:#04x} at {at}"
                );
            }
        }
    }
}
