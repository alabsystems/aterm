// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A palette set the `allow_palette_reconfigure` gate refuses (OSC 4, numeric
//! OSC 21) is told to the host ONCE per session, so a theme tool that did
//! nothing (base16-shell, pywal, `theme.sh`) can be found, without a theme
//! script's dozens of sets each raising something. The gate itself stays
//! default-off.
//!
//! Negative control: with the gate open nothing is told and the palette moves.
//! Opening the gate re-arms the report, so closing it again tells anew.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use aterm_core::config::TerminalConfig;
use aterm_core::terminal::{Rgb, Terminal};

/// A terminal configured with the palette gate at `open`, counting the
/// refusals its host is told about.
fn counted(open: bool) -> (Terminal, Arc<AtomicUsize>) {
    let mut t = Terminal::new(4, 20);
    t.apply_config(&TerminalConfig {
        allow_palette_reconfigure: open,
        ..TerminalConfig::default()
    });
    let told = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&told);
    t.set_palette_refused_callback(move || {
        count.fetch_add(1, Ordering::SeqCst);
    });
    (t, told)
}

const TARGET: Rgb = Rgb {
    r: 0x28,
    g: 0x2a,
    b: 0x36,
};

#[test]
fn the_gate_is_off_by_default() {
    assert!(!TerminalConfig::default().allow_palette_reconfigure);
}

#[test]
fn refused_palette_sets_are_told_once() {
    let (mut t, told) = counted(false);
    let before = t.palette_color(4);
    t.process(b"\x1b]4;4;rgb:28/2a/36\x07");
    t.process(b"\x1b]4;5;rgb:28/2a/36\x07\x1b]21;6=rgb:28/2a/36\x07");
    assert_eq!(told.load(Ordering::SeqCst), 1, "one event for three sets");
    assert!(t.palette_set_refused());
    assert_eq!(t.palette_color(4), before, "the gate still refuses");

    // RIS is not a new session: it does not re-arm the event.
    t.process(b"\x1bc\x1b]4;4;rgb:28/2a/36\x07");
    assert_eq!(told.load(Ordering::SeqCst), 1);
}

#[test]
fn a_numeric_osc_21_set_alone_is_told() {
    let (mut t, told) = counted(false);
    t.process(b"\x1b]21;4=rgb:28/2a/36\x07");
    assert_eq!(told.load(Ordering::SeqCst), 1);
}

#[test]
fn what_the_gate_never_held_is_not_told() {
    let (mut t, told) = counted(false);
    // Queries, invalid specs and the ungated dynamic colours are not refusals.
    t.process(b"\x1b]4;4;?\x07\x1b]4;4;not-a-colour\x07\x1b]11;rgb:10/11/12\x07");
    t.process(b"\x1b]21;foreground=rgb:10/11/12\x07");
    assert_eq!(told.load(Ordering::SeqCst), 0);
    assert!(!t.palette_set_refused());
}

#[test]
fn an_open_gate_tells_nothing_and_the_palette_moves() {
    let (mut t, told) = counted(true);
    t.process(b"\x1b]4;4;rgb:28/2a/36\x07\x1b]21;5=rgb:28/2a/36\x07");
    assert_eq!(told.load(Ordering::SeqCst), 0);
    assert!(!t.palette_set_refused());
    assert_eq!(t.palette_color(4), TARGET);
    assert_eq!(t.palette_color(5), TARGET);
}

/// Closed, open, closed again: each closed spell's first refusal is told,
/// through `apply_config` and through the setter alike.
#[test]
fn reopening_the_gate_rearms_the_report() {
    let (mut t, told) = counted(false);
    let refuse = |t: &mut Terminal| t.process(b"\x1b]4;4;rgb:28/2a/36\x07");
    refuse(&mut t);
    assert_eq!(told.load(Ordering::SeqCst), 1);
    let gate = |t: &mut Terminal, open: bool| {
        t.apply_config(&TerminalConfig {
            allow_palette_reconfigure: open,
            ..TerminalConfig::default()
        });
    };
    gate(&mut t, true);
    assert!(!t.palette_set_refused(), "opened, nothing stands refused");
    gate(&mut t, false);
    refuse(&mut t);
    assert_eq!(
        told.load(Ordering::SeqCst),
        2,
        "the second closed spell is told"
    );

    t.set_allow_palette_reconfigure(true);
    t.set_allow_palette_reconfigure(false);
    refuse(&mut t);
    assert_eq!(told.load(Ordering::SeqCst), 3, "the setter re-arms too");
}

/// Control: a gate that stays closed (re-applied closed) tells once.
#[test]
fn a_gate_kept_closed_tells_once() {
    let (mut t, told) = counted(false);
    t.process(b"\x1b]4;4;rgb:28/2a/36\x07");
    t.set_allow_palette_reconfigure(false);
    t.apply_config(&TerminalConfig::default());
    t.process(b"\x1b]4;4;rgb:28/2a/36\x07");
    assert_eq!(told.load(Ordering::SeqCst), 1);
}
