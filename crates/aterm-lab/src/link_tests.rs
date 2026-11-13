// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn v(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn a_way_hands_each_chunk_on_half_a_round_trip_after_it_was_read_in_order() {
    let t0 = Instant::now();
    let half = Duration::from_millis(50);
    let mut way = Way::new(half);
    assert_eq!(way.next_due(), None);
    way.push(t0, b"a");
    way.push(t0 + Duration::from_millis(10), b"\x1b[D");
    way.push(t0 + Duration::from_millis(10), b"");
    assert_eq!(way.next_due(), Some(t0 + half));
    assert_eq!(way.pop_due(t0 + Duration::from_millis(49)), None);
    assert_eq!(way.pop_due(t0 + half).as_deref(), Some(&b"a"[..]));
    assert_eq!(way.pop_due(t0 + half), None, "the second is not due yet");
    assert_eq!(way.next_due(), Some(t0 + Duration::from_millis(60)));
    assert_eq!(
        way.pop_due(t0 + Duration::from_millis(500)).as_deref(),
        Some(&b"\x1b[D"[..])
    );
    assert!(way.is_empty(), "an empty read holds nothing");
}

#[test]
fn the_wait_is_until_the_earlier_due_chunk_rounded_up() {
    let t0 = Instant::now();
    let ms = Duration::from_millis;
    assert_eq!(wait_ms(t0, None, None), -1);
    assert_eq!(wait_ms(t0, Some(t0 + ms(30)), Some(t0 + ms(7))), 7);
    assert_eq!(wait_ms(t0, Some(t0 + Duration::from_micros(1)), None), 1);
    assert_eq!(wait_ms(t0 + ms(9), Some(t0 + ms(7)), None), 0);
}

#[test]
fn the_relay_by_hand_takes_a_round_trip_and_a_take_then_dashes_then_a_command() {
    let a = v(&["--rtt", "250", "--take", "/t/x.take", "--", "zsh", "-f"]);
    let (s, cmd) = parse_args(&a).unwrap();
    assert_eq!(s.rtt, Duration::from_millis(250));
    assert_eq!(s.one_way(), Duration::from_millis(125));
    assert_eq!(s.take.as_deref(), Some("/t/x.take"));
    assert_eq!(cmd, &v(&["zsh", "-f"])[..]);
    let (s, _) = parse_args(&v(&["--", "true"])).unwrap();
    assert_eq!(s.rtt, Duration::from_millis(DEFAULT_RTT_MS));
    assert!(parse_args(&v(&["--rtt", "x", "--", "true"])).is_none());
    assert!(parse_args(&v(&["--rtt", "5"])).is_none());
    assert!(parse_args(&v(&["zsh"])).is_none());
}

#[test]
fn under_a_shells_name_it_starts_that_shell_with_the_environments_link() {
    let env = |k: &str| match k {
        ENV_RTT => Some("80".to_string()),
        ENV_TAKE => Some("/w/takes/".to_string()),
        _ => None,
    };
    let s = Settings::from_env(env, 42).unwrap();
    assert_eq!(s.rtt, Duration::from_millis(80));
    assert_eq!(s.take.as_deref(), Some("/w/takes/42.take"));
    let bare = Settings::from_env(|_| None, 1).unwrap();
    assert_eq!(bare.rtt, Duration::from_millis(DEFAULT_RTT_MS));
    assert_eq!(bare.take, None);
    assert!(Settings::from_env(|k| (k == ENV_RTT).then(|| "fast".into()), 1).is_err());
    assert_eq!(shell_for("-zsh", None), "/bin/zsh");
    assert_eq!(shell_for("bash", None), "/bin/bash");
    assert_eq!(
        shell_for("bash", Some("/opt/bash".into())),
        "/opt/bash",
        "LAB_LINK_EXEC names another"
    );
}
