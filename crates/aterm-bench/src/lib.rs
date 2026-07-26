// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm performance benchmarks (ROADMAP WS-K).
//!
//! Run with `cargo bench -p aterm-bench`. Performance targets in `ATERM_DESIGN.md`
//! §7 are **RED** (unproven) until reproduced here on the shipped binary; this
//! crate exists so the numbers are measured, not asserted.
//!
//! The deterministic SEARCH corpora live here (not in one example) so the
//! committed floor lane (`search_harness`) and the posting-container decision
//! bench (`posting_container_harness`) measure the SAME bytes — a container
//! verdict taken on different data than the floors gate would be dishonest.

/// Trigram-DIVERSE corpus: line counter + rotating 40-glyph body (same shape as
/// the scroll-scrub fill). Every line differs, trigram space is saturated — the
/// worst case the audit's external harness measured at ~1283 B/line.
#[must_use]
pub fn rotating_corpus(lines: usize) -> Vec<u8> {
    const GLYPHS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789+/-=";
    let mut out = Vec::with_capacity(lines * 50);
    for line in 0..lines {
        out.extend_from_slice(line.to_string().as_bytes());
        out.push(b' ');
        for c in 0..40usize {
            out.push(GLYPHS[(line + c) % GLYPHS.len()]);
        }
        out.extend_from_slice(b"\r\n");
    }
    out
}

/// REPETITIVE-LOG corpus (the audit's caveat): 8 real-shaped service-log
/// templates cycling, only digits varying. Trigram-map small, per-line
/// postings/Strings unchanged — the shape where a "the map shrank so we're
/// fine" regression would hide.
#[must_use]
pub fn replog_corpus(lines: usize) -> Vec<u8> {
    let templates: [&str; 8] = [
        "INFO  svc-api    request completed method=GET path=/api/v1/items status=200",
        "INFO  svc-api    request completed method=POST path=/api/v1/items status=201",
        "DEBUG svc-cache  entry refreshed key=items shard=4 hit_rate=0.97",
        "INFO  svc-worker job dequeued queue=default attempts=1",
        "WARN  svc-api    slow request method=GET path=/api/v1/search status=200",
        "ERROR svc-db     connection reset pool=primary retrying",
        "INFO  svc-worker job finished queue=default result=ok",
        "DEBUG svc-gc     sweep complete freed_kb=128 live_objects=40213",
    ];
    let mut out = Vec::with_capacity(lines * 110);
    for line in 0..lines {
        // Timestamp-ish prefix + latency suffix: digits vary per line, the
        // template text (and its trigrams) repeats.
        out.extend_from_slice(b"2026-07-22T12:");
        out.extend_from_slice(format!("{:02}:{:02}", (line / 60) % 60, line % 60).as_bytes());
        out.extend_from_slice(format!(".{:03}Z ", line % 1000).as_bytes());
        out.extend_from_slice(templates[line % templates.len()].as_bytes());
        out.extend_from_slice(format!(" latency_ms={}", line % 250).as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out
}

/// HYPERLINK-HEAVY corpus (Wave-4A P7): package-registry fetch logs where
/// every line prints a long URL wrapped in a real OSC 8 hyperlink (URL as its
/// own visible text — the common registry/CI shape). Exercises what neither
/// committed corpus does: punctuation-dense high-entropy trigrams from URLs,
/// visible lines wide enough to soft-wrap at 80 columns (2 grid rows per
/// logical line), and the OSC 8 parser path in the fill itself.
///
/// `lines` counts LOGICAL lines; at 80 columns each occupies 2 grid rows, so
/// callers sizing a ring must budget `2 * lines` rows.
#[must_use]
pub fn linkheavy_corpus(lines: usize) -> Vec<u8> {
    let pkgs: [&str; 8] = [
        "left-pad",
        "react-dom",
        "tokio-util",
        "serde-json",
        "webpack-cli",
        "numpy-core",
        "clang-tools",
        "proto-gen",
    ];
    let mut out = Vec::with_capacity(lines * 200);
    for line in 0..lines {
        let pkg = pkgs[line % pkgs.len()];
        let url = format!(
            "https://registry.example.com/{pkg}/-/{pkg}-{}.{}.{}.tgz",
            line % 9,
            line % 23,
            line % 251,
        );
        out.extend_from_slice(b"fetch \x1b]8;;");
        out.extend_from_slice(url.as_bytes());
        out.extend_from_slice(b"\x1b\\");
        out.extend_from_slice(url.as_bytes());
        out.extend_from_slice(b"\x1b]8;;\x1b\\");
        out.extend_from_slice(format!(" 200 in {}ms", line % 900).as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out
}
