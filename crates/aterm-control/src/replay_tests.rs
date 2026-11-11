// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::wire::visible_row;
use aterm_core::terminal::AltArchiveQuery;

fn archiving(term: &Terminal) -> bool {
    term.alt_archive_read(AltArchiveQuery::oldest(0, 0)).enabled
}

fn rows(term: &Terminal) -> Vec<String> {
    (0..term.rows() as usize)
        .map(|r| visible_row(term, r))
        .collect()
}

fn replays(term: &Terminal, policy: ResizePolicy) -> bool {
    let console_host = policy == ResizePolicy::ConPty;
    let modes = term.modes();
    !archiving(term)
        && modes.win32_input_enabled == console_host
        && modes.kitty_keyboard_enabled != console_host
}

/// A replay under the session's config, and a fork of it, answers a colour query and resets its modes as the live engine does; one under the defaults does neither.
#[test]
fn a_replay_with_the_sessions_config_answers_as_the_live_engine() {
    let config = Arc::new(TerminalConfig {
        default_background: aterm_core::terminal::Rgb { r: 1, g: 2, b: 3 },
        cursor_style: aterm_core::terminal::CursorStyle::SteadyBar,
        ..TerminalConfig::default()
    });
    let live = || {
        let mut t = Terminal::new(4, 20);
        let _ = t.apply_config(&config);
        t.process(b"\x1b[2 qtext");
        t
    };
    let cp = live().checkpoint();
    for (configured, forked) in [(false, false), (true, false), (false, true), (true, true)] {
        let engine = ReplayEngine::from_checkpoint(&cp, ResizePolicy::Native);
        let engine = if configured {
            engine.with_config(config.clone())
        } else {
            engine
        };
        let mut replay = if forked {
            engine.fork().expect("a ground engine forks")
        } else {
            engine
        };
        let mut probe = live();
        let ask = b"\x1b]11;?\x07\x1b[!p";
        replay.process(ask);
        probe.process(ask);
        let mut term = replay.into_terminal();
        assert_eq!(
            (
                term.take_response() == probe.take_response(),
                term.cursor_style() == probe.cursor_style()
            ),
            (configured, configured),
            "configured: {configured}, forked: {forked}"
        );
    }
}

/// A replayed config install is the config the engine replays under from there on, and so its forks: a soft reset after it takes the installed default cursor, as the live engine's does.
#[test]
fn a_replayed_config_install_is_the_config_the_engine_and_its_forks_replay_under() {
    use aterm_core::terminal::CursorStyle;
    let base = Arc::new(TerminalConfig {
        cursor_style: CursorStyle::SteadyBar,
        ..TerminalConfig::default()
    });
    let installed = Arc::new(TerminalConfig {
        cursor_style: CursorStyle::SteadyUnderline,
        ..TerminalConfig::default()
    });
    let mut live = Terminal::new(4, 20);
    let _ = live.apply_config(&base);
    let cp = live.checkpoint();
    let from = live.host_ordinal();
    let _ = live.apply_config_shared(installed.clone());
    let (reports, _) = live.host_journal_since(from);
    let [report] = reports.as_slice() else {
        panic!("one install journaled: {reports:?}");
    };
    let mut replay =
        ReplayEngine::from_checkpoint(&cp, ResizePolicy::Native).with_config(base.clone());
    assert_eq!(replay.config(), Some(&base));
    replay.apply_host_op(report.op.clone(), report.clock);
    assert_eq!(
        replay.config().map(|c| c.cursor_style),
        Some(installed.cursor_style)
    );
    let mut fork = replay.fork().expect("a ground engine forks");
    assert_eq!(
        fork.config().map(|c| c.cursor_style),
        Some(installed.cursor_style)
    );
    live.process(b"\x1b[2 q\x1b[!p");
    fork.process(b"\x1b[2 q\x1b[!p");
    assert_eq!(fork.terminal().cursor_style(), live.cursor_style());
    assert_eq!(live.cursor_style(), CursorStyle::SteadyUnderline, "fixture");
}

#[test]
fn every_replay_sets_archive_posture_and_policy() {
    let lines: Vec<u8> = (0..12)
        .flat_map(|i| format!("L{i:02}\r\n").into_bytes())
        .collect();
    let live_after_grow = |policy: ResizePolicy| {
        let mut live = Terminal::new(6, 20);
        live.set_console_host_posture(policy);
        live.process(&lines);
        live.resize_with_policy(10, 20, policy);
        rows(&live)
    };
    for (policy, other) in [
        (ResizePolicy::ConPty, ResizePolicy::Native),
        (ResizePolicy::Native, ResizePolicy::ConPty),
    ] {
        let mut foreign = Terminal::new(6, 20);
        foreign.set_console_host_posture(other);
        let fresh = ReplayEngine::new(6, 20, policy);
        let restored = ReplayEngine::from_checkpoint(&foreign.checkpoint(), policy);
        let bounded = ReplayEngine::from_checkpoint_bounded(&foreign.checkpoint(), policy, 64);
        for (how, mut engine) in [
            ("new", fresh),
            ("from_checkpoint", restored),
            ("from_checkpoint_bounded", bounded),
        ] {
            assert!(replays(engine.terminal(), policy), "{how} {policy:?}");
            engine.process(&lines);
            engine.resize(10, 20);
            assert_eq!(
                rows(engine.terminal()),
                live_after_grow(policy),
                "{how} {policy:?}: the grow lands as the live engine's"
            );
        }
    }
    assert_ne!(
        live_after_grow(ResizePolicy::ConPty),
        live_after_grow(ResizePolicy::Native),
        "the grow must tell the policies apart"
    );

    let mut bare = Terminal::new(6, 20);
    assert!(!replays(&bare, ResizePolicy::ConPty));
    assert!(!replays(&bare, ResizePolicy::Native), "the archive is on");
    bare.process(&lines);
    bare.resize(10, 20);
    assert_ne!(rows(&bare), live_after_grow(ResizePolicy::ConPty));
}
