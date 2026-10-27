// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The colour scheme a program is told (`CSI ? 996 n`, `CSI ? 997 ; Ps n`) follows the background.

use aterm_core::config::{ConfigChange, TerminalConfig};
use aterm_core::terminal::{ClockReading, ColorSchemeSource, Rgb, Terminal};
use aterm_types::Appearance;

const PUSH_DARK: &[u8] = b"\x1b[?997;1n";
const PUSH_LIGHT: &[u8] = b"\x1b[?997;2n";
const OSC11_WHITE: &[u8] = b"\x1b]11;#ffffff\x07";
const OSC11_BLACK: &[u8] = b"\x1b]11;#000000\x07";
const DSR_996: &[u8] = b"\x1b[?996n";

const fn rgb(r: u8, g: u8, b: u8) -> Rgb {
    Rgb { r, g, b }
}

/// A host theme: a background and the appearance the theme declares.
fn theme(bg: Rgb, scheme: Appearance) -> TerminalConfig {
    TerminalConfig {
        default_background: bg,
        color_scheme: Some(scheme),
        ..TerminalConfig::default()
    }
}

fn dark_theme() -> TerminalConfig {
    theme(rgb(0x28, 0x2a, 0x36), Appearance::Dark)
}

fn light_theme() -> TerminalConfig {
    theme(rgb(0xfd, 0xf6, 0xe3), Appearance::Light)
}

/// Process `bytes` and return everything queued for the program.
fn reply(t: &mut Terminal, bytes: &[u8]) -> Vec<u8> {
    t.process(bytes);
    t.take_response().unwrap_or_default()
}

/// What DSR 996 answers now (draining anything queued first).
fn dsr(t: &mut Terminal) -> Vec<u8> {
    let _ = t.take_response();
    reply(t, DSR_996)
}

fn has_997(bytes: &[u8]) -> bool {
    bytes.windows(5).any(|w| w == b"[?997")
}

#[test]
fn dsr_996_answers_the_background_set_in_the_same_batch() {
    let mut t = Terminal::new(8, 40);
    t.set_color_scheme(Appearance::Dark);
    let mut batch = OSC11_WHITE.to_vec();
    batch.extend_from_slice(DSR_996);
    assert_eq!(reply(&mut t, &batch), PUSH_LIGHT);
    assert_eq!(t.color_scheme(), Appearance::Light);
    assert_eq!(t.color_scheme_source(), ColorSchemeSource::Program);

    let mut t = Terminal::new(8, 40);
    t.set_color_scheme(Appearance::Light);
    assert_eq!(reply(&mut t, b"\x1b]11;#101010\x07\x1b[?996n"), PUSH_DARK);
}

#[test]
fn osc11_pushes_once_when_subscribed_and_only_on_change() {
    let mut t = Terminal::new(8, 40);
    t.apply_config(&dark_theme());
    assert_eq!(reply(&mut t, b"\x1b[?2031h"), b"");
    assert_eq!(reply(&mut t, OSC11_WHITE), PUSH_LIGHT);

    assert_eq!(reply(&mut t, b"\x1b]11;#fefefe\x07"), b"");

    let mut batch = OSC11_BLACK.to_vec();
    batch.extend_from_slice(OSC11_WHITE);
    assert_eq!(reply(&mut t, &batch), b"");
    assert_eq!(reply(&mut t, OSC11_BLACK), PUSH_DARK);

    let mut t = Terminal::new(8, 40);
    t.apply_config(&dark_theme());
    assert_eq!(reply(&mut t, OSC11_WHITE), b"");
    assert_eq!(dsr(&mut t), PUSH_LIGHT);
}

#[test]
fn osc111_returns_to_the_baseline_and_pushes() {
    for reset in [&b"\x1b]111\x07"[..], &b"\x1b]21;background\x07"[..]] {
        let label = String::from_utf8_lossy(reset);
        let mut t = Terminal::new(8, 40);
        t.apply_config(&dark_theme());
        t.process(b"\x1b[?2031h");
        assert_eq!(reply(&mut t, OSC11_WHITE), PUSH_LIGHT, "{label:?}");
        assert_eq!(reply(&mut t, reset), PUSH_DARK, "{label:?}");
        assert_eq!(
            t.color_scheme_source(),
            ColorSchemeSource::Configured,
            "{label:?}"
        );
        assert_eq!(reply(&mut t, reset), b"", "{label:?}");
    }
}

#[test]
fn the_baseline_rules_while_the_background_is_the_configured_one() {
    let mid = rgb(0x80, 0x80, 0x80);
    let mut t = Terminal::new(8, 40);
    t.apply_config(&theme(mid, Appearance::Light));
    assert_eq!(dsr(&mut t), PUSH_LIGHT);
    assert_eq!(t.color_scheme_source(), ColorSchemeSource::Configured);

    t.process(b"\x1b]11;#808080\x07");
    assert_eq!(dsr(&mut t), PUSH_LIGHT);
    assert_eq!(t.color_scheme_source(), ColorSchemeSource::Configured);

    t.process(b"\x1b]11;#7f7f80\x07");
    assert_eq!(t.color_scheme_source(), ColorSchemeSource::Program);
    let measured = if aterm_messages::ink::bg_is_light([0x7f, 0x7f, 0x80]) {
        PUSH_LIGHT
    } else {
        PUSH_DARK
    };
    assert_eq!(dsr(&mut t), measured);
}

#[test]
fn apply_config_keeps_a_program_background_polarity() {
    let mut t = Terminal::new(8, 40);
    t.apply_config(&dark_theme());
    t.process(b"\x1b[?2031h");
    assert_eq!(reply(&mut t, OSC11_WHITE), PUSH_LIGHT);

    for (name, config) in [("light", light_theme()), ("dark", dark_theme())] {
        let changes = t.apply_config(&config);
        assert!(
            !changes.contains(&ConfigChange::ColorScheme),
            "to {name}: the program's view is unchanged"
        );
        assert_eq!(t.take_response(), None, "to {name}");
        assert_eq!(t.color_scheme(), Appearance::Light, "to {name}");
    }

    let mut t = Terminal::new(8, 40);
    t.apply_config(&dark_theme());
    t.process(b"\x1b[?2031h");
    let _ = t.take_response();
    let changes = t.apply_config(&light_theme());
    assert!(changes.contains(&ConfigChange::ColorScheme));
    assert_eq!(t.take_response().as_deref(), Some(PUSH_LIGHT));
}

#[test]
fn osc111_after_a_theme_switch_lands_on_the_new_baseline() {
    let mut t = Terminal::new(8, 40);
    t.apply_config(&dark_theme());
    t.process(b"\x1b[?2031h");
    assert_eq!(reply(&mut t, OSC11_WHITE), PUSH_LIGHT);
    t.apply_config(&light_theme());
    let _ = t.take_response();
    assert_eq!(reply(&mut t, b"\x1b]111\x07"), b"");
    assert_eq!(t.color_scheme_source(), ColorSchemeSource::Configured);

    t.apply_config(&dark_theme());
    let _ = t.take_response();
    assert_eq!(reply(&mut t, OSC11_WHITE), PUSH_LIGHT);
    assert_eq!(reply(&mut t, b"\x1b]111\x07"), PUSH_DARK);
}

#[test]
fn a_none_color_scheme_keeps_the_host_baseline() {
    let mut t = Terminal::new(8, 40);
    t.set_color_scheme(Appearance::Light);
    t.apply_config(&TerminalConfig::default());
    assert_eq!(t.color_scheme(), Appearance::Light);

    t.apply_config(&TerminalConfig {
        color_scheme: Some(Appearance::Dark),
        ..TerminalConfig::default()
    });
    assert_eq!(t.color_scheme(), Appearance::Dark);
}

#[test]
fn ris_returns_to_baseline_without_a_push() {
    let mut t = Terminal::new(8, 40);
    t.apply_config(&dark_theme());
    t.process(b"\x1b[?2031h");
    assert_eq!(reply(&mut t, OSC11_WHITE), PUSH_LIGHT);
    assert!(!has_997(&reply(&mut t, b"\x1bc")));
    assert_eq!(dsr(&mut t), PUSH_DARK);
    assert_eq!(t.color_scheme_source(), ColorSchemeSource::Configured);

    let mut t = Terminal::new(8, 40);
    t.apply_config(&dark_theme());
    t.process(b"\x1b[?2031h");
    assert_eq!(reply(&mut t, OSC11_WHITE), PUSH_LIGHT);
    t.reset();
    assert!(!has_997(&t.take_response().unwrap_or_default()));
    assert_eq!(dsr(&mut t), PUSH_DARK);

    let mut t = Terminal::new(8, 40);
    t.apply_config(&dark_theme());
    t.process(b"\x1b[?2031h");
    assert_eq!(reply(&mut t, OSC11_WHITE), PUSH_LIGHT);
    assert!(!has_997(&reply(&mut t, b"\x1b[!p")));
    assert_eq!(dsr(&mut t), PUSH_LIGHT);
}

#[test]
fn kitty_colour_stack_pop_restores_polarity() {
    let mut t = Terminal::new(8, 40);
    t.apply_config(&dark_theme());
    t.process(b"\x1b[?2031h");
    assert_eq!(reply(&mut t, b"\x1b]30001\x1b\\"), b"");
    assert_eq!(reply(&mut t, OSC11_WHITE), PUSH_LIGHT);
    assert_eq!(reply(&mut t, b"\x1b]30101\x1b\\"), PUSH_DARK);
    assert_eq!(t.color_scheme_source(), ColorSchemeSource::Configured);
    assert_eq!(reply(&mut t, b"\x1b]30101\x1b\\"), b"");
}

#[test]
fn decscnm_does_not_move_the_reported_scheme() {
    let mut t = Terminal::new(8, 40);
    t.apply_config(&dark_theme());
    t.process(b"\x1b[?2031h");
    assert_eq!(reply(&mut t, b"\x1b[?5h"), b"");
    assert_eq!(dsr(&mut t), PUSH_DARK);
    assert_eq!(reply(&mut t, b"\x1b[?5l"), b"");
    assert_eq!(reply(&mut t, OSC11_WHITE), PUSH_LIGHT);
}

#[test]
fn handoff_successor_pushes_only_a_stale_view() {
    let mut source = Terminal::new(8, 40);
    source.apply_config(&dark_theme());
    source.process(b"\x1b[?2031h");
    assert_eq!(reply(&mut source, OSC11_WHITE), PUSH_LIGHT);
    let carried = source.checkpoint();

    let mut successor = Terminal::new(8, 40);
    successor.apply_config(&dark_theme());
    successor.restore_checkpoint(&carried);
    assert!(!has_997(&successor.take_response().unwrap_or_default()));
    assert_eq!(dsr(&mut successor), PUSH_LIGHT);

    let mut rebuilt = Terminal::from_checkpoint(&carried);
    rebuilt.apply_config(&dark_theme());
    assert!(!has_997(&rebuilt.take_response().unwrap_or_default()));
    assert_eq!(rebuilt.color_scheme(), Appearance::Light);

    let mut source = Terminal::new(8, 40);
    source.apply_config(&dark_theme());
    source.process(b"\x1b[?2031h");
    let _ = source.take_response();
    let carried = source.checkpoint();
    let mut successor = Terminal::new(8, 40);
    successor.apply_config(&light_theme());
    let _ = successor.take_response();
    successor.restore_checkpoint(&carried);
    assert_eq!(successor.take_response().as_deref(), Some(PUSH_LIGHT));
    assert_eq!(reply(&mut successor, b""), b"");
}

#[test]
fn foreground_handback_leaves_the_polarity() {
    let mut t = Terminal::new(8, 40);
    t.apply_config(&dark_theme());
    t.process(b"\x1b[?2031h\x1b[?1000h");
    assert_eq!(reply(&mut t, OSC11_WHITE), PUSH_LIGHT);
    let handback = t.foreground_handback().expect("2031 and 1000 are evidence");
    assert!(
        handback.bytes.windows(8).any(|w| w == b"\x1b[?2031l"),
        "the handback unsubscribes the dead program"
    );
    assert_eq!(dsr(&mut t), PUSH_LIGHT);
    assert_eq!(t.color_scheme_source(), ColorSchemeSource::Program);
    assert_eq!(reply(&mut t, b"\x1b]111\x07"), b"");
    assert_eq!(dsr(&mut t), PUSH_DARK);
}

/// A policy whose `response any` rule drops every program reply, whatever
/// its origin; everything else executes.
fn silencing_policy() -> aterm_policy::engine::PolicyEngine {
    use aterm_policy::{Defaults, OriginTag, Policy, Profile, Response, Rule, SCHEMA_VERSION};
    aterm_policy::engine::PolicyEngine::new(Policy {
        schema_version: SCHEMA_VERSION,
        profile: Profile::Standard,
        defaults: Defaults {
            unmatched: Response::Execute,
            shell_integration_require_nonce: false,
        },
        rules: vec![Rule {
            sequence: "response any".to_owned(),
            origin_min: OriginTag::NetworkUntrusted,
            response: Response::Drop,
            rate_limit: None,
            prompt_id: None,
        }],
        rate_limits: vec![],
    })
}

/// A push the program's own output provokes (OSC 11 under DEC 2031) is a
/// reply it provoked: a policy that drops every response drops it, and the
/// latch stays so a later host-driven change still announces. Negative
/// control: with no policy the same bytes push.
#[test]
fn a_program_provoked_push_obeys_the_response_policy() {
    let provoke = b"\x1b[?2031h\x1b]11;#ffffff\x07";

    let mut open = Terminal::new(8, 40);
    open.apply_config(&dark_theme());
    assert_eq!(
        reply(&mut open, provoke),
        PUSH_LIGHT,
        "NEG: no policy pushes"
    );

    let mut t = Terminal::new(8, 40);
    t.apply_config(&dark_theme());
    t.apply_policy_engine(silencing_policy());
    assert!(
        reply(&mut t, b"\x1b[6n").is_empty(),
        "PRECONDITION: the policy drops a query reply"
    );
    assert!(
        !has_997(&reply(&mut t, provoke)),
        "the policy drops the push"
    );
    assert_eq!(
        t.color_scheme(),
        Appearance::Light,
        "DSR 996 state still moved"
    );
    assert!(
        !has_997(&reply(&mut t, OSC11_BLACK)),
        "a program alternating backgrounds gets nothing either"
    );
    assert!(!has_997(&reply(&mut t, OSC11_WHITE)));
    t.apply_config(&light_theme());
    assert_eq!(t.take_response().unwrap_or_default(), PUSH_LIGHT);
}

/// A push the reply rate limit refused stays owed and goes out at the first later batch
/// whose clock finds room in the bucket, with no host call. Negative control: a batch at
/// the same clock reading still finds the bucket spent.
#[test]
fn a_push_the_rate_limit_refused_goes_out_once_the_bucket_refills() {
    let mut t = Terminal::new(8, 40);
    t.apply_config(&dark_theme());
    let clock = ClockReading::now();
    t.process_at(&b"\x1b[6n".repeat(20_000), clock);
    let spent = t.take_response().unwrap_or_default();
    assert!(
        spent.len() < 20_000 * 6,
        "PRECONDITION: the queries spent the bucket ({} bytes answered)",
        spent.len()
    );
    t.process_at(b"\x1b[?2031h\x1b]11;#ffffff\x07", clock);
    assert!(
        !has_997(&t.take_response().unwrap_or_default()),
        "the spent bucket refuses the push"
    );
    t.process_at(b"x", clock);
    assert!(
        !has_997(&t.take_response().unwrap_or_default()),
        "NEG: still spent at the same reading"
    );
    let later = ClockReading {
        monotonic: clock.monotonic + std::time::Duration::from_secs(2),
        wall_ms: clock.wall_ms,
    };
    t.process_at(b"x", later);
    assert_eq!(t.take_response().unwrap_or_default(), PUSH_LIGHT);
}

/// A push the policy refused is dropped by a batch that ends with 2031 off, before a host
/// colour call announces it; one that turns it off and on again keeps it owed, and the same
/// host call announces it then (the negative control).
#[test]
fn turning_2031_off_drops_an_owed_push() {
    for off_at_batch_end in [true, false] {
        let mut t = Terminal::new(8, 40);
        t.apply_config(&dark_theme());
        t.apply_policy_engine(silencing_policy());
        assert!(!has_997(&reply(&mut t, b"\x1b[?2031h\x1b]11;#ffffff\x07")));
        if off_at_batch_end {
            assert!(reply(&mut t, b"\x1b[?2031l").is_empty());
            assert!(reply(&mut t, b"\x1b[?2031h").is_empty());
        } else {
            assert!(reply(&mut t, b"\x1b[?2031l\x1b[?2031h").is_empty());
        }
        t.apply_config(&light_theme());
        assert_eq!(
            has_997(&t.take_response().unwrap_or_default()),
            !off_at_batch_end,
            "owed only while no batch ended with 2031 off"
        );
    }
}
