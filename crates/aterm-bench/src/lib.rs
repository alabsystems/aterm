// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Deterministic corpora and helpers shared across `verify perf` benchmark harnesses.

/// Generate a trigram-diverse corpus of `lines` with a rotating 40-glyph body.
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

/// Generate a repetitive service-log corpus of `lines` cycling through 8 templates.
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
        out.extend_from_slice(b"2026-07-22T12:");
        out.extend_from_slice(format!("{:02}:{:02}", (line / 60) % 60, line % 60).as_bytes());
        out.extend_from_slice(format!(".{:03}Z ", line % 1000).as_bytes());
        out.extend_from_slice(templates[line % templates.len()].as_bytes());
        out.extend_from_slice(format!(" latency_ms={}", line % 250).as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out
}

/// Generate a hyperlink-heavy corpus of `lines` with OSC 8 URLs.
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

/// Generate the throughput lane's mixed-traffic workload of at least `bytes` length.
#[must_use]
pub fn throughput_corpus(bytes: usize) -> Vec<u8> {
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

/// Return the initial byte capacity reserved for `lines` in [`rotating_corpus`] (`lines * 50`, saturating).
#[must_use]
pub const fn rotating_corpus_min_cap(lines: usize) -> usize {
    lines.saturating_mul(50)
}

/// Return the number of 80-column grid rows occupied by `lines` of [`linkheavy_corpus`] (`lines * 2`, saturating).
#[must_use]
pub const fn linkheavy_grid_rows(lines: usize) -> usize {
    lines.saturating_mul(2)
}

/// Encode a benchmark sample (`elapsed_ns`, `bytes_processed`, `iterations`) into 16 LE bytes.
#[must_use]
pub const fn encode_bench_sample_le(
    elapsed_ns: u64,
    bytes_processed: u32,
    iterations: u32,
) -> [u8; 16] {
    [
        elapsed_ns as u8,
        (elapsed_ns >> 8) as u8,
        (elapsed_ns >> 16) as u8,
        (elapsed_ns >> 24) as u8,
        (elapsed_ns >> 32) as u8,
        (elapsed_ns >> 40) as u8,
        (elapsed_ns >> 48) as u8,
        (elapsed_ns >> 56) as u8,
        bytes_processed as u8,
        (bytes_processed >> 8) as u8,
        (bytes_processed >> 16) as u8,
        (bytes_processed >> 24) as u8,
        iterations as u8,
        (iterations >> 8) as u8,
        (iterations >> 16) as u8,
        (iterations >> 24) as u8,
    ]
}

/// Decode 16 LE bytes into a benchmark sample (`elapsed_ns`, `bytes_processed`, `iterations`).
#[must_use]
pub const fn decode_bench_sample_le(bytes: [u8; 16]) -> (u64, u32, u32) {
    let elapsed_ns = (bytes[0] as u64)
        | ((bytes[1] as u64) << 8)
        | ((bytes[2] as u64) << 16)
        | ((bytes[3] as u64) << 24)
        | ((bytes[4] as u64) << 32)
        | ((bytes[5] as u64) << 40)
        | ((bytes[6] as u64) << 48)
        | ((bytes[7] as u64) << 56);
    let bytes_processed = (bytes[8] as u32)
        | ((bytes[9] as u32) << 8)
        | ((bytes[10] as u32) << 16)
        | ((bytes[11] as u32) << 24);
    let iterations = (bytes[12] as u32)
        | ((bytes[13] as u32) << 8)
        | ((bytes[14] as u32) << 16)
        | ((bytes[15] as u32) << 24);
    (elapsed_ns, bytes_processed, iterations)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bench_sample_le_round_trips() {
        assert_eq!(rotating_corpus_min_cap(10), 500);
        assert_eq!(linkheavy_grid_rows(25), 50);
        let enc = encode_bench_sample_le(1_234_567, 65_536, 100);
        assert_eq!(decode_bench_sample_le(enc), (1_234_567, 65_536, 100));
    }
}
