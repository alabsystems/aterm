// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests that palette sets refused by `allow_palette_reconfigure` notify the host once per closed spell.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use aterm_core::config::TerminalConfig;
use aterm_core::terminal::{Rgb, Terminal};

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

#[test]
fn a_gate_kept_closed_tells_once() {
    let (mut t, told) = counted(false);
    t.process(b"\x1b]4;4;rgb:28/2a/36\x07");
    t.set_allow_palette_reconfigure(false);
    t.apply_config(&TerminalConfig::default());
    t.process(b"\x1b]4;4;rgb:28/2a/36\x07");
    assert_eq!(told.load(Ordering::SeqCst), 1);
}
