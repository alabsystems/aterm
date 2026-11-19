// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Out ring's bound, its order and the two functions its entries are made with.

use super::*;

fn claimant(pid: u32) -> Proven {
    Proven {
        pid,
        start: "Thu Nov 13 09:00:00 2026".to_string(),
    }
}

fn entry(pid: u32, hseq: u64, body: usize) -> OutEntry {
    OutEntry {
        claimant: claimant(pid),
        epoch: "e1".to_string(),
        hseq,
        kind: "msg",
        fp: fp(&hseq.to_string()),
        body: "x".repeat(body),
    }
}

/// THE RING HOLDS 1 MiB, OLDEST DROPPED FIRST: entries fill it to the bound, and the next one
/// drops the oldest until it fits; the rest keep their order. NEGATIVE CONTROL: a ring under the
/// bound drops nothing.
#[test]
fn the_ring_keeps_one_mib_and_drops_the_oldest_first() {
    let mut ring = OutRing::default();
    let quarter = OUT_RING_BYTES / 4 - 200;
    for hseq in 1..=4 {
        assert!(ring.push(entry(7, hseq, quarter)));
    }
    assert_eq!(ring.dropped(), 0, "CONTROL: under the bound");
    assert!(ring.bytes() <= OUT_RING_BYTES);
    assert!(ring.push(entry(7, 5, quarter)));
    assert_eq!(ring.dropped(), 1);
    assert_eq!(
        ring.entries().map(|e| e.hseq).collect::<Vec<_>>(),
        [2, 3, 4, 5],
        "the oldest went"
    );
    assert!(ring.push(entry(7, 6, 2 * quarter)));
    assert_eq!(
        ring.entries().map(|e| e.hseq).collect::<Vec<_>>(),
        [4, 5, 6]
    );
    assert!(ring.bytes() <= OUT_RING_BYTES);
    assert!(
        !ring.push(entry(7, 7, OUT_RING_BYTES)),
        "larger than the ring"
    );
    assert_eq!(ring.entries().count(), 3, "and nothing dropped for it");
}

/// The ring says whose conversation it holds, by claimant.
#[test]
fn the_ring_holds_a_claimants_conversation_by_its_proof() {
    let mut ring = OutRing::default();
    assert!(!ring.holds(&claimant(7)));
    assert!(ring.push(entry(7, 1, 10)));
    assert!(ring.holds(&claimant(7)));
    assert!(!ring.holds(&claimant(8)), "another claimant's");
    assert!(
        !ring.holds(&Proven {
            start: "Fri Nov 14 09:00:00 2026".to_string(),
            ..claimant(7)
        }),
        "the same pid under another start"
    );
}

/// A BODY IS ESCAPED AS THE LOG ESCAPES TEXT: hidden characters dropped, then percent-encoded,
/// with no clamp. Its digest is 40 lowercase hex characters, the same for the same line and not
/// for another.
#[test]
fn a_body_is_escaped_as_the_log_escapes_text_and_its_digest_is_forty_hex() {
    let line = "{\"k\":\"msg\",\"text\":\"a b\u{7}\\n%\"}";
    let escaped = escape(line);
    assert!(
        !escaped.contains(' ') && !escaped.contains("%07"),
        "{escaped}"
    );
    assert!(
        escaped.contains("a%20b") && escaped.contains("%25"),
        "{escaped}"
    );
    let long = "y".repeat(10_000);
    assert_eq!(escape(&long).len(), 10_000, "no clamp");
    let digest = fp(line);
    assert_eq!(digest.len(), FP_HEX);
    assert!(
        digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    assert_eq!(digest, fp(line));
    assert_ne!(digest, fp(&long), "CONTROL: another line");
    assert!(aterm_slog::ShortId::new(&digest).is_some(), "a log ID");
}
