// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Stateless classification of one raw PTY chunk for job progress.

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Lines {
    /// A line feed.
    pub(super) ends: bool,
    /// A line feed after text in the chunk itself.
    pub(super) ended: bool,
    /// Text after its last line feed, or all of it when it holds none.
    pub(super) tail: bool,
    /// Raw CSI CUP, HVP, VPA, DECSTBM, IL, DL, SU or SD: row drawing,
    /// rather than a spinner's carriage return and erase.
    pub(super) drawn: bool,
}

impl Lines {
    pub(super) fn of(chunk: &[u8]) -> Self {
        let mut lines = Self {
            ends: false,
            ended: false,
            tail: false,
            drawn: false,
        };
        // Slice contains uses the standard library's byte search. Ordinary output
        // needs no CSI inspection; an escape candidate still uses the raw-chunk
        // rule, including malformed sequences and sequences cut by the PTY read.
        lines.drawn = chunk.contains(&0x1b)
            && chunk.iter().enumerate().any(|(i, &b)| {
                if b != 0x1b {
                    return false;
                }
                let Some(rest) = chunk[i + 1..].strip_prefix(b"[") else {
                    return false;
                };
                rest.iter()
                    .find(|b| (0x40..=0x7e).contains(*b))
                    .is_some_and(|b| {
                        matches!(b, b'H' | b'f' | b'd' | b'r' | b'L' | b'M' | b'S' | b'T')
                    })
            });
        // Every byte other than CR/LF is text for this raw-chunk classifier.
        // A completed line exists exactly when text precedes the last LF;
        // only the suffix after that LF contributes to the next line.
        lines.ends = chunk.contains(&b'\n');
        let tail = if lines.ends {
            let last = chunk.iter().rposition(|&b| b == b'\n').unwrap();
            lines.ended = chunk[..last].iter().any(|b| !matches!(b, b'\r' | b'\n'));
            &chunk[last + 1..]
        } else {
            chunk
        };
        lines.tail = tail.iter().any(|&b| b != b'\r');
        lines
    }
}
