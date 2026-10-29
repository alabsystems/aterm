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
