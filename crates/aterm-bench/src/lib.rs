// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm performance harnesses: the examples `verify perf` runs. This lib holds
//! the deterministic corpora shared between them.

/// Trigram-diverse corpus: line counter + rotating 40-glyph body. Every line
/// differs and trigram space is saturated: the worst case for the search index,
/// and real (compressible) work for the scroll-scrub tiers.
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

/// Repetitive-log corpus: 8 real-shaped service-log templates cycling, only
/// digits varying. The trigram map stays small while per-line postings do
/// not, the shape where a regression hidden by a shrinking map would show.
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

/// Hyperlink-heavy corpus: package-registry fetch logs where every line prints
/// a long URL wrapped in an OSC 8 hyperlink (the URL as its own visible text).
/// Exercises punctuation-dense high-entropy trigrams, lines that soft-wrap at
/// 80 columns, and the OSC 8 parser path in the fill itself.
///
/// `lines` counts logical lines; at 80 columns each occupies 2 grid rows, so
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

/// The throughput lane's workload, at least `bytes` long: a repeating mix of plain
/// text, SGR colour/style runs, CSI cursor moves + erases, and newline-driven
/// scrolling. Deterministic byte-for-byte across machines (no RNG, no clock) so
/// the corpus — and therefore the work performed — is identical everywhere.
/// Shared by `perf_harness` (the `throughput` lane) and `crash_restore_harness`
/// (the tail tap's overhead on the same bytes).
#[must_use]
pub fn throughput_corpus(bytes: usize) -> Vec<u8> {
    // One "frame" of mixed traffic. Hand-built so the proportions are stable:
    //   - plain printable ASCII (the cheap path),
    //   - SGR runs (parser-heavy: params + dispatch),
    //   - CSI cursor positioning + erase-in-line (state mutation),
    //   - explicit newlines so the 24-row window scrolls constantly.
    let frame: &[&[u8]] = &[
        b"the quick brown fox jumps over the lazy dog 0123456789\r\n",
        b"\x1b[1;38;5;202mERROR\x1b[0m \x1b[4;48;5;19mwarning\x1b[0m normal text here\r\n",
        b"\x1b[2K\x1b[1Gredrawn line after erase-in-line and cursor-to-col-1\r\n",
        b"\x1b[7minverse\x1b[27m \x1b[3mitalic\x1b[23m \x1b[9mstrike\x1b[29m mixed sgr\r\n",
        b"\x1b[10;5Hpositioned via CUP \x1b[K then erased to end of line\r\n",
        b"plain throughput line with a tab\tand more ascii content padding 42\r\n",
        b"\x1b[38;2;120;200;255mtruecolor\x1b[0m run plus \x1b[48;2;10;10;10mbg\x1b[0m\r\n",
        b"\x1b[H\x1b[2Jfull clear then home, forcing a screen reset every frame\r\n",
    ];
    let unit: Vec<u8> = frame.concat();
    let mut out = Vec::with_capacity(bytes + unit.len());
    while out.len() < bytes {
        out.extend_from_slice(&unit);
    }
    out
}
