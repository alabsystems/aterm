// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm-wayland's generated tables against wayland-scanner 0.31.10's,
//! recorded in tests/recorded/wayland-scanner-0.31.10-interfaces.tsv from the
//! same protocol files: every interface's name, version and message counts,
//! and every message's name, signature and argument types, in order.
//!
//! Negative controls: the recording covers every table and nothing else; a
//! table one signature character or one type away is told apart.

use std::ffi::CStr;

use aterm_wayland::iface::{Interface, Message, signature_args};
use aterm_wayland::protocols::ALL;

const RECORDING: &str = "wayland-scanner-0.31.10-interfaces.tsv";

fn recorded() -> Vec<String> {
    let path = format!("{}/tests/recorded/{RECORDING}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{path}: {e}"))
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

fn name(c: &CStr) -> &str {
    c.to_str().expect("an ASCII name")
}

fn message_row(kind: &str, iface: &str, opcode: usize, m: &Message) -> String {
    let sig = m.signature().to_bytes();
    let types: Vec<String> = (0..signature_args(sig))
        .map(|i| {
            m.arg_interface(i)
                .map_or("-".to_owned(), |t| name(t.name()).to_owned())
        })
        .collect();
    let types = if types.is_empty() {
        "-".to_owned()
    } else {
        types.join(",")
    };
    format!(
        "{kind}\t{iface}\t{opcode}\t{}\t{}\t{types}",
        name(m.name()),
        String::from_utf8_lossy(sig)
    )
}

fn rows(interfaces: &[&Interface]) -> Vec<String> {
    let mut out = Vec::new();
    for i in interfaces {
        let n = name(i.name());
        out.push(format!(
            "iface\t{n}\t{}\t{}\t{}",
            i.version(),
            i.requests().len(),
            i.events().len()
        ));
        for (op, m) in i.requests().iter().enumerate() {
            out.push(message_row("req", n, op, m));
        }
        for (op, m) in i.events().iter().enumerate() {
            out.push(message_row("evt", n, op, m));
        }
    }
    out
}

#[test]
fn every_table_is_wayland_scanners() {
    let want = recorded();
    let got = rows(ALL);
    assert!(want.len() >= 280, "the corpus is whole: {}", want.len());
    let missing: Vec<_> = want.iter().filter(|r| !got.contains(r)).collect();
    let extra: Vec<_> = got.iter().filter(|r| !want.contains(r)).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "recorded, not generated:\n{}\ngenerated, not recorded:\n{}",
        missing
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        extra
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert_eq!(got, want, "the same rows, in the same order");
}

/// Negative control: the comparison tells one character of one table apart.
#[test]
fn a_table_one_character_away_is_told_apart() {
    static WRONG_SIG: [Message; 1] = [Message::new(
        c"attach",
        c"oii",
        &aterm_wayland::iface::NULLS,
    )];
    static WRONG_SURFACE: Interface = Interface::new(c"wl_surface", 6, &WRONG_SIG, &[]);
    let want = recorded();
    let row = &rows(&[&WRONG_SURFACE])[1];
    assert!(row.starts_with("req\twl_surface\t0\tattach\toii\t"));
    assert!(!want.contains(row), "a lost `?` must miss: {row}");
    let typed = rows(&[&aterm_wayland::protocols::wayland::wl_surface::INTERFACE]);
    assert!(
        typed
            .iter()
            .any(|r| r == "req\twl_surface\t1\tattach\t?oii\twl_buffer,-,-"),
        "and the right one hits"
    );
}

/// A safe table refuses missing type slots before any pointer lookup can occur.
#[test]
fn a_message_refuses_a_types_slice_shorter_than_its_signature() {
    use aterm_wayland::iface::{NULLS, TypeRef};

    static ITEM: Interface = Interface::new(c"typed", 1, &[], &[]);
    static TYPES: [TypeRef; 2] = [TypeRef::to(&ITEM), TypeRef::NULL];
    let empty = Message::new(c"empty", c"", &[]);
    assert!(empty.arg_interface(0).is_none());
    assert!(empty.arg_interface(usize::MAX).is_none());
    assert_eq!(signature_args(b"12?os"), 2);
    let typed = Message::new(c"typed", c"12?os", &TYPES);
    assert!(std::ptr::eq(typed.arg_interface(0).unwrap(), &ITEM));
    assert!(typed.arg_interface(1).is_none());
    assert!(typed.arg_interface(2).is_none());
    let full = Message::new(c"full", c"iufsonah", &NULLS);
    assert_eq!(signature_args(full.signature().to_bytes()), 8);
    assert!((0..=8).all(|i| full.arg_interface(i).is_none()));
    let bind = Message::new(c"bind", c"sun", &NULLS);
    assert_eq!(signature_args(bind.signature().to_bytes()), 3);
    assert!((0..=3).all(|i| bind.arg_interface(i).is_none()));

    let mut accepted = Vec::new();
    for (signature, supplied) in [(c"o", 0), (c"12?os", 1), (c"iufsonah", 7), (c"sun", 2)] {
        // Construct only: an invalid baseline table is never dereferenced.
        if std::panic::catch_unwind(|| Message::new(c"short", signature, &NULLS[..supplied]))
            .is_ok()
        {
            accepted.push((signature, supplied));
        }
    }
    assert!(
        accepted.is_empty(),
        "MESSAGE_TYPES_MUST_COVER_SIGNATURE: {accepted:?}"
    );
}
