// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `since=<cursor>` deltas (§13.5). THE SERVER REMEMBERS REPLIES, NOT
//! SNAPSHOTS: for each reply it keeps the fingerprint of every line it
//! displayed, keyed by the line's key (`global`, `tab 1`, `core S1`, `proc
//! 52114`). A delta holds only the lines whose displayed text changed since
//! that reply, plus the events recorded since; unchanged lines collapse to one
//! `same <keys>` line. The text is already quantised to what the words print
//! (0.1c, whole percent, two or three significant figures), so a figure that
//! moves within its quantum is unchanged.

use super::view::{Line, Reply};

/// One reply's line fingerprints: `(key, hash of the displayed text)`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fingerprints {
    /// In the reply's order.
    pub lines: Vec<(String, u64)>,
}

/// FNV-1a, 64-bit: stable across builds, so a cursor kept over a live
/// upgrade's epoch change is refused by its epoch, never misread.
#[must_use]
pub fn fnv(text: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// The fingerprints of `reply`'s value lines (record and sentence lines;
/// `more=`, events and notes are not values).
#[must_use]
pub fn fingerprints(reply: &Reply) -> Fingerprints {
    Fingerprints {
        lines: reply
            .lines
            .iter()
            .filter(|l| l.value)
            .map(|l| (l.key.clone(), fnv(&l.text)))
            .collect(),
    }
}

/// `reply` as a delta against `base`: value lines whose text is unchanged
/// fold into one `same <k1>,<k2>,…` line (keys with their spaces as `_`),
/// placed where the first of them stood; every other line stays. Returns the
/// keys that folded.
pub fn apply_delta(reply: &mut Reply, base: &Fingerprints) -> Vec<String> {
    let mut same = Vec::new();
    let mut first_same: Option<usize> = None;
    let mut kept: Vec<Line> = Vec::with_capacity(reply.lines.len());
    for line in reply.lines.drain(..) {
        let unchanged = line.value
            && base
                .lines
                .iter()
                .any(|(k, h)| *k == line.key && *h == fnv(&line.text));
        if unchanged {
            if first_same.is_none() {
                first_same = Some(kept.len());
            }
            same.push(line.key.clone());
        } else {
            kept.push(line);
        }
    }
    if let Some(at) = first_same {
        let keys: Vec<String> = same.iter().map(|k| k.replace(' ', "_")).collect();
        kept.insert(
            at,
            Line {
                key: "same".into(),
                text: format!("same {}", keys.join(",")),
                value: false,
            },
        );
    }
    reply.lines = kept;
    same
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::query::View;

    fn line(key: &str, text: &str) -> Line {
        Line {
            key: key.into(),
            text: text.into(),
            value: true,
        }
    }

    fn reply(lines: Vec<Line>) -> Reply {
        Reply {
            view: View::Overview,
            scope: None,
            subject: None,
            lines,
            json: None,
        }
    }

    #[test]
    fn unchanged_lines_fold_into_one_same_line_and_changed_ones_stay() {
        let before = reply(vec![
            line("global", "global  3.1 of 10 cores busy"),
            line("aterm", "aterm   1.6 cores"),
            line("tab 1", "tab 1   claude; 0.9 cores"),
        ]);
        let base = fingerprints(&before);
        let mut now = reply(vec![
            line("global", "global  3.1 of 10 cores busy"),
            line("aterm", "aterm   2.6 cores"),
            line("tab 1", "tab 1   claude; 0.9 cores"),
            Line {
                key: "e".into(),
                text: "e seq=9 kind=tab".into(),
                value: false,
            },
        ]);
        let same = apply_delta(&mut now, &base);
        assert_eq!(same, ["global", "tab 1"]);
        let texts: Vec<&str> = now.lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(
            texts,
            ["same global,tab_1", "aterm   2.6 cores", "e seq=9 kind=tab"]
        );
    }

    #[test]
    fn a_new_key_is_a_change_and_events_are_never_folded() {
        let base = fingerprints(&reply(vec![line("global", "x")]));
        let mut now = reply(vec![line("global", "x"), line("tab 2", "y")]);
        assert_eq!(apply_delta(&mut now, &base), ["global"]);
        assert_eq!(now.lines.len(), 2);
        assert_eq!(now.lines[1].key, "tab 2");
    }

    #[test]
    fn fnv_is_stable() {
        assert_eq!(fnv(""), 0xcbf2_9ce4_8422_2325);
        assert_ne!(fnv("a"), fnv("b"));
    }
}
