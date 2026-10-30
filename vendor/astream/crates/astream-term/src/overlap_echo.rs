// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Fine-grained per-byte PTY write->echo causal attribution across overlapping in-flight
//! inputs (`docs/ROADMAP.md` line 179, `Astream.Term.TlaOverlapEcho`).
//!
//! When multiple un-echoed PTY writes overlap in flight (`drive_input` / `predict_at` at
//! `in_offset = 10`, `11`, `12` before the kernel `termios` line discipline echoes any of
//! them), coarse `last_in_offset` attribution stamps every echoed `Out` chunk with only the
//! newest `in_offset`, and single-entry `apply_output_caused` retires at most one byte.
//!
//! [`InFlightEchoQueue`] maintains a bounded FIFO of un-echoed input bytes (`<= MAX_IN_FLIGHT_ECHO_BYTES`,
//! certified by [`echo_queue_within_cap`] and Clean-origin [`echo_queue_len_clamp`]) along
//! with each byte's expected POSIX `termios` cooked/ECHOCTL/ONLCR echo expansion, and
//! attributes arbitrary split or coalesced PTY `Out` chunks back to the exact sequence of
//! `(in_offset, client_seq)` writes they echo, advancing `last_retired_in_offset`
//! monotonically via Clean-origin [`echo_matched_advance`].

#[cfg(clean_islands)]
#[path = "overlap_echo_island.rs"]
mod overlap_echo_island;
#[cfg(not(clean_islands))]
pub use crate::twins::{echo_matched_advance, echo_queue_len_clamp};
#[cfg(clean_islands)]
pub use overlap_echo_island::{echo_matched_advance, echo_queue_len_clamp};

contract_fn! {
    /// Kernel-certified check that the queued in-flight byte count fits within `cap`
    /// (`queued <= cap`), linked via `clean-kernel-defeq` to `echo_queue_within_cap_isl`.
    #[must_use]
    pub const fn echo_queue_within_cap(queued: usize, cap: usize) -> bool
    clauses [ensures result == (queued <= cap)]
    {
        queued <= cap
    }
}

/// Maximum number of un-echoed input bytes retained in [`InFlightEchoQueue`].
pub const MAX_IN_FLIGHT_ECHO_BYTES: usize = 4096;

/// Expected POSIX `termios` (`ECHO | ECHOE | ECHOCTL | ONLCR`) echo expansion for one input byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpectedEcho {
    bytes: [u8; 3],
    len: u8,
}

impl ExpectedEcho {
    /// Empty echo (used for escape-sequence bytes that produce no direct line-discipline echo).
    pub const NONE: Self = Self {
        bytes: [0; 3],
        len: 0,
    };

    /// Expected echo slice (`0..=3` bytes).
    #[must_use]
    pub const fn as_slice(&self) -> &[u8] {
        match self.len {
            0 => &[],
            1 => match self.bytes.first_chunk::<1>() {
                Some(c) => c,
                None => &[],
            },
            2 => match self.bytes.first_chunk::<2>() {
                Some(c) => c,
                None => &[],
            },
            _ => &self.bytes,
        }
    }

    /// Derive the standard cooked-mode (`ECHO | ECHOE | ECHOCTL | ONLCR`) echo for `input_byte`.
    #[must_use]
    pub const fn cooked_for_byte(input_byte: u8) -> Self {
        if matches!(input_byte, 0x20..=0x7e) {
            Self {
                bytes: [input_byte, 0, 0],
                len: 1,
            }
        } else if input_byte == b'\r' || input_byte == b'\n' {
            Self {
                bytes: [b'\r', b'\n', 0],
                len: 2,
            }
        } else if input_byte == 0x7f || input_byte == 0x08 {
            // Cooked `ECHOE` rubout (`BS SP BS`).
            Self {
                bytes: [0x08, b' ', 0x08],
                len: 3,
            }
        } else if input_byte == b'\t' {
            Self {
                bytes: [b'\t', 0, 0],
                len: 1,
            }
        } else if input_byte < 0x20 && input_byte != 0x1b {
            // `ECHOCTL` caret notation (`^A` .. `^_`).
            Self {
                bytes: [b'^', input_byte.wrapping_add(0x40), 0],
                len: 2,
            }
        } else {
            Self::NONE
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InFlightByte {
    in_offset: u64,
    client_seq: u64,
    input_byte: u8,
    expected: ExpectedEcho,
    matched_echo_pos: u8,
    is_last_of_write: bool,
}

/// Contiguous attributed slice of a PTY `Out` chunk matched to one `(in_offset, client_seq)` write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttributedEchoSpan {
    /// Engine `In` offset that caused this echoed byte range.
    pub in_offset: u64,
    /// Client sequence number of the originating input proposal.
    pub client_seq: u64,
    /// Start..end byte indices inside the `Out` chunk attributed to `in_offset`.
    pub out_range: (usize, usize),
    /// Number of input bytes from `in_offset` whose expected echo completed in this span.
    pub completed_input_bytes: usize,
    /// `true` iff the final byte of the `in_offset` write completed its echo in this span.
    pub write_completed: bool,
}

/// Result of attributing one PTY `Out` chunk against [`InFlightEchoQueue`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EchoAttribution {
    /// Ordered causal spans within `out_bytes` across one or more overlapping writes.
    pub spans: Vec<AttributedEchoSpan>,
    /// Number of leading bytes in `out_bytes` accounted for as line-discipline echoes.
    pub echoed_out_bytes: usize,
    /// Monotone highest `in_offset` echoed in this chunk (suitable for `CausedBy` stamping).
    pub highest_in_offset: Option<u64>,
    /// Earliest `in_offset` echoed in this chunk.
    pub earliest_in_offset: Option<u64>,
}

/// Bounded per-byte FIFO tracking overlapping un-echoed PTY writes and attributing echoes.
#[derive(Debug, Clone)]
pub struct InFlightEchoQueue {
    entries: Vec<InFlightByte>,
    cap: usize,
    last_retired_in_offset: Option<u64>,
}

impl Default for InFlightEchoQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl InFlightEchoQueue {
    /// Create a queue with [`MAX_IN_FLIGHT_ECHO_BYTES`] capacity.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
            cap: MAX_IN_FLIGHT_ECHO_BYTES,
            last_retired_in_offset: None,
        }
    }

    /// Create a queue with an explicit capacity clamped to `[1, MAX_IN_FLIGHT_ECHO_BYTES]`.
    #[must_use]
    pub fn with_capacity_cap(cap: usize) -> Self {
        let bounded = echo_queue_len_clamp(cap as u64, MAX_IN_FLIGHT_ECHO_BYTES as u64) as usize;
        let effective = if bounded == 0 { 1 } else { bounded };
        Self {
            entries: Vec::new(),
            cap: effective,
            last_retired_in_offset: None,
        }
    }

    /// Number of un-echoed input bytes currently in flight (`<= self.capacity_cap()`).
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no un-echoed input bytes remain in flight.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Configured capacity ceiling (`<= MAX_IN_FLIGHT_ECHO_BYTES`).
    #[must_use]
    pub const fn capacity_cap(&self) -> usize {
        self.cap
    }

    /// Highest `in_offset` retired by echo attribution so far.
    #[must_use]
    pub const fn last_retired_in_offset(&self) -> Option<u64> {
        self.last_retired_in_offset
    }

    /// Clear all pending un-echoed bytes (e.g. on no-echo password mode or TUI altscreen).
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Record an in-flight PTY write `(in_offset, client_seq, bytes)`.
    ///
    /// Escape sequences (`ESC ...`) produce no direct line-discipline echo and are skipped;
    /// if the queue reaches `self.cap`, the oldest un-echoed prefix is dropped so memory
    /// stays strictly bounded (`echo_queue_within_cap(self.len(), self.cap)`).
    pub fn push_input(&mut self, in_offset: u64, client_seq: u64, bytes: &[u8]) -> usize {
        if bytes.is_empty() || bytes.first() == Some(&0x1b) {
            return 0;
        }
        let mut echoable_count = 0usize;
        let mut scan = bytes;
        while let &[b, ref tail @ ..] = scan {
            let exp = ExpectedEcho::cooked_for_byte(b);
            if exp.len > 0 {
                echoable_count = echoable_count.saturating_add(1);
            }
            scan = tail;
        }
        if echoable_count == 0 {
            return 0;
        }
        // Keep only the suffix that fits in `self.cap` if a single burst exceeds `self.cap`.
        let mut added = 0usize;
        let mut seen_echoable = 0usize;
        let skip_echoable = echoable_count.saturating_sub(self.cap);
        let mut rest = bytes;
        while let &[b, ref tail @ ..] = rest {
            let exp = ExpectedEcho::cooked_for_byte(b);
            if exp.len > 0 {
                seen_echoable = seen_echoable.saturating_add(1);
                if seen_echoable > skip_echoable {
                    if self.entries.len() >= self.cap && !self.entries.is_empty() {
                        let _ = self.entries.remove(0);
                    }
                    if echo_queue_within_cap(self.entries.len().saturating_add(1), self.cap) {
                        self.entries.push(InFlightByte {
                            in_offset,
                            client_seq,
                            input_byte: b,
                            expected: exp,
                            matched_echo_pos: 0,
                            is_last_of_write: seen_echoable == echoable_count,
                        });
                        added = added.saturating_add(1);
                    }
                }
            }
            rest = tail;
        }
        added
    }

    /// Attribute a PTY `Out` byte chunk against the FIFO of overlapping in-flight writes.
    ///
    /// Handles:
    /// - **Coalesced echoes** where one `Out` chunk (`"abcd"`) answers multiple writes
    ///   (`in_offset = 10` `"ab"` and `in_offset = 11` `"cd"`), emitting one
    ///   [`AttributedEchoSpan`] per write in exact causal order.
    /// - **Split / multi-byte echoes** where an `Out` chunk splits mid-write (`"ab"` + `"c"`)
    ///   or mid-expansion (`"\r"` + `"\n"` for `ONLCR`, `"^"` + `"C"` for `ECHOCTL`, or
    ///   `"\x08 "` + `"\x08"` for `ECHOE`).
    /// - **Raw newline fallback** where `\n` echoes as bare `\n` (`!ONLCR`) as well as `\r\n`.
    pub fn attribute_output(&mut self, out_bytes: &[u8]) -> EchoAttribution {
        let mut spans: Vec<AttributedEchoSpan> = Vec::new();
        let mut out_idx = 0usize;
        let mut head = 0usize;
        let mut earliest: Option<u64> = None;
        let mut highest: Option<u64> = None;

        while out_idx < out_bytes.len() && head < self.entries.len() {
            let (Some(&out_b), Some(&entry)) = (out_bytes.get(out_idx), self.entries.get(head))
            else {
                break;
            };
            let exp_slice = entry.expected.as_slice();
            let pos = entry.matched_echo_pos as usize;
            let Some(&want_b) = exp_slice.get(pos) else {
                head = head.saturating_add(1);
                continue;
            };

            let (matched, completed_entry) = if out_b == want_b {
                let next_pos = pos.saturating_add(1);
                (true, next_pos >= exp_slice.len())
            } else if (entry.input_byte == b'\n' || entry.input_byte == b'\r')
                && pos == 0
                && out_b == b'\n'
            {
                // Raw / non-ONLCR line discipline echoed bare LF for CR/LF input.
                (true, true)
            } else if (entry.input_byte == 0x7f || entry.input_byte == 0x08)
                && pos == 0
                && out_b == b'^'
                && out_bytes.get(out_idx.saturating_add(1)) == Some(&b'?')
            {
                // `ECHOCTL` without `ECHOE` echoed `^?` for DEL; rewrite expected to `^?`.
                if let Some(slot) = self.entries.get_mut(head) {
                    slot.expected = ExpectedEcho {
                        bytes: [b'^', b'?', 0],
                        len: 2,
                    };
                }
                (true, false)
            } else {
                (false, false)
            };

            if !matched {
                break;
            }

            if earliest.is_none() {
                earliest = Some(entry.in_offset);
            }
            highest = Some(match highest {
                Some(cur) => echo_matched_advance(cur, entry.in_offset),
                None => entry.in_offset,
            });
            self.last_retired_in_offset = Some(match self.last_retired_in_offset {
                Some(cur) => echo_matched_advance(cur, entry.in_offset),
                None => entry.in_offset,
            });

            let next_out_idx = out_idx.saturating_add(1);
            let done_input_inc = usize::from(completed_entry);
            let write_done = completed_entry && entry.is_last_of_write;

            if let Some(last_span) = spans.last_mut() {
                if last_span.in_offset == entry.in_offset
                    && last_span.client_seq == entry.client_seq
                    && last_span.out_range.1 == out_idx
                {
                    last_span.out_range.1 = next_out_idx;
                    last_span.completed_input_bytes = last_span
                        .completed_input_bytes
                        .saturating_add(done_input_inc);
                    if write_done {
                        last_span.write_completed = true;
                    }
                } else {
                    spans.push(AttributedEchoSpan {
                        in_offset: entry.in_offset,
                        client_seq: entry.client_seq,
                        out_range: (out_idx, next_out_idx),
                        completed_input_bytes: done_input_inc,
                        write_completed: write_done,
                    });
                }
            } else {
                spans.push(AttributedEchoSpan {
                    in_offset: entry.in_offset,
                    client_seq: entry.client_seq,
                    out_range: (out_idx, next_out_idx),
                    completed_input_bytes: done_input_inc,
                    write_completed: write_done,
                });
            }

            if completed_entry {
                head = head.saturating_add(1);
            } else if let Some(slot) = self.entries.get_mut(head) {
                slot.matched_echo_pos = entry.matched_echo_pos.saturating_add(1);
            }
            out_idx = next_out_idx;
        }

        if head > 0 {
            let mut remaining = Vec::new();
            if let Some(tail) = self.entries.get(head..) {
                remaining.extend_from_slice(tail);
            }
            self.entries = remaining;
        }

        EchoAttribution {
            spans,
            echoed_out_bytes: out_idx,
            highest_in_offset: highest,
            earliest_in_offset: earliest,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coalesced_overlapping_writes_attribute_exact_per_write_spans() {
        let mut q = InFlightEchoQueue::new();
        assert_eq!(q.push_input(10, 1, b"ab"), 2);
        assert_eq!(q.push_input(11, 2, b"cd"), 2);
        assert_eq!(q.push_input(12, 3, b"\n"), 1);

        // Coalesced echo `"abcd\r\n$ "` attributes `[0..2)` -> 10, `[2..4)` -> 11, `[4..6)` -> 12,
        // stopping cleanly before the shell prompt `"$ "`!
        let att = q.attribute_output(b"abcd\r\n$ ");
        assert_eq!(att.echoed_out_bytes, 6);
        assert_eq!(att.earliest_in_offset, Some(10));
        assert_eq!(att.highest_in_offset, Some(12));
        assert_eq!(q.last_retired_in_offset(), Some(12));
        assert!(q.is_empty());
        assert_eq!(
            att.spans,
            vec![
                AttributedEchoSpan {
                    in_offset: 10,
                    client_seq: 1,
                    out_range: (0, 2),
                    completed_input_bytes: 2,
                    write_completed: true,
                },
                AttributedEchoSpan {
                    in_offset: 11,
                    client_seq: 2,
                    out_range: (2, 4),
                    completed_input_bytes: 2,
                    write_completed: true,
                },
                AttributedEchoSpan {
                    in_offset: 12,
                    client_seq: 3,
                    out_range: (4, 6),
                    completed_input_bytes: 1,
                    write_completed: true,
                },
            ]
        );
    }

    #[test]
    fn split_echoctl_and_echoe_chunks_resume_across_boundaries() {
        let mut q = InFlightEchoQueue::with_capacity_cap(8);
        assert_eq!(q.push_input(20, 1, b"\x03\x7f"), 2); // ^C (2 bytes) + BS SP BS (3 bytes)

        let a1 = q.attribute_output(b"^");
        assert_eq!(a1.echoed_out_bytes, 1);
        assert_eq!(a1.spans[0].completed_input_bytes, 0);
        assert!(!a1.spans[0].write_completed);
        assert_eq!(q.len(), 2);

        let a2 = q.attribute_output(b"C\x08 ");
        assert_eq!(a2.echoed_out_bytes, 3);
        assert_eq!(a2.spans[0].completed_input_bytes, 1);
        assert!(!a2.spans[0].write_completed);
        assert_eq!(q.len(), 1);

        let a3 = q.attribute_output(b"\x08");
        assert_eq!(a3.echoed_out_bytes, 1);
        assert_eq!(a3.spans[0].completed_input_bytes, 1);
        assert!(a3.spans[0].write_completed);
        assert!(q.is_empty());
    }
}
