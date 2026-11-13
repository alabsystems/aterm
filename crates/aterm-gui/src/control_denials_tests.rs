// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn peer(pid: u32, program: &str) -> Peer {
    Peer {
        pid: Some(pid),
        program: Some(program.to_string()),
    }
}

/// A steady stream of blind dials from one program writes its 1st, 2nd, 4th and 8th,
/// the later ones with the run's count; another kind's first refusal is written at once.
#[test]
fn a_steady_stream_is_written_at_powers_of_two_per_kind() {
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    let d = AuthDenials::new();
    let written: Vec<u64> = (1..=13_u64)
        .filter(|&n| {
            d.record(NO_TOKEN, &peer(1000 + n as u32, "aterm"), at(n))
                .is_some()
        })
        .collect();
    assert_eq!(written, [1, 2, 4, 8]);

    let ms = |m: u64| t0 + Duration::from_millis(m);
    let d = AuthDenials::new();
    let p = peer(7, "aterm");
    assert!(d.record(NO_TOKEN, &p, ms(0)).is_some());
    assert!(d.record(NO_TOKEN, &p, ms(1000)).is_some());
    assert_eq!(
        d.record(NO_TOKEN, &p, ms(2100)),
        None,
        "jitter is not quiet"
    );
    assert!(d.record(NO_TOKEN, &p, ms(3000)).is_some(), "the run's 4th");

    let d = AuthDenials::new();
    let p = peer(7, "aterm");
    assert!(
        d.record(NO_TOKEN, &p, at(1)).is_some(),
        "the 1st is written"
    );
    assert!(
        d.record(NO_TOKEN, &p, at(2)).is_some(),
        "the 2nd is written"
    );
    assert_eq!(
        d.record(NO_TOKEN, &p, at(3)),
        None,
        "the 3rd is only counted"
    );
    assert_eq!(d.total(NO_TOKEN), 3, "but counted");
    let other = d
        .record(WRONG_TOKEN, &p, at(4))
        .expect("another kind at once");
    assert!(other.starts_with(WRONG_TOKEN), "{other}");
    assert!(d.record("something new", &p, at(5)).is_some());
}

/// Three dials a second apart, then two more 50 s later: the quiet starts a new run, so
/// the 4th is written and says so, and the 5th is the new run's 2nd.
#[test]
fn quiet_longer_than_the_run_restarts_the_spacing() {
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    let d = AuthDenials::new();
    let p = peer(7, "aterm");
    assert!(d.record(NO_TOKEN, &p, at(0)).is_some());
    assert!(d.record(NO_TOKEN, &p, at(1)).is_some());
    assert!(d.record(NO_TOKEN, &p, at(2)).is_none());
    assert!(
        d.record(NO_TOKEN, &p, at(52)).is_some(),
        "50 s quiet after a 2 s run starts a new run"
    );
    assert!(
        d.record(NO_TOKEN, &p, at(53)).is_some(),
        "the new run's 2nd"
    );
    assert!(d.record(NO_TOKEN, &p, at(55)).is_none());
}

/// A different peer program is never folded into another's run: it is written at once,
/// though its quiet (1 s) is no longer than the run.
#[test]
fn a_different_peer_program_starts_a_new_run() {
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    let d = AuthDenials::new();
    for s in 0..4 {
        let _ = d.record(NO_TOKEN, &peer(100 + s as u32, "aterm"), at(s));
    }
    assert!(
        d.record(NO_TOKEN, &peer(21897, "python3"), at(4)).is_some(),
        "a new program is written at once"
    );
}

/// A dialer refusing in bursts (two 150 ms apart every 5 s) starts one new run on its
/// first quiet and is then thinned: 60 refusals write a handful of lines.
#[test]
fn a_periodic_burst_is_thinned_after_its_first_quiet() {
    let t0 = Instant::now();
    let ms = |m: u64| t0 + Duration::from_millis(m);
    let d = AuthDenials::new();
    let p = peer(7, "aterm");
    let written = (0..30_u64)
        .flat_map(|round| [round * 5000, round * 5000 + 150])
        .filter(|&m| d.record(NO_TOKEN, &p, ms(m)).is_some())
        .count();
    assert!(written <= 10, "60 refusals in bursts wrote {written} lines");
    assert_eq!(d.total(NO_TOKEN), 60, "every refusal counted");
    let last = 29 * 5000 + 150;
    assert!(d.record(NO_TOKEN, &p, ms(last + 11_000)).is_none());
    assert!(
        d.record(NO_TOKEN, &p, ms(last + 11_000 + 400_000))
            .is_some()
    );
}

/// Two programs taking turns are each thinned on their own: `aterm` and a
/// python3 poller alternating a second apart, forty in all, write each
/// program's 1st, 2nd, 4th, 8th and 16th — ten lines, not forty.
#[test]
fn alternating_programs_are_thinned_independently() {
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    let d = AuthDenials::new();
    let written: Vec<(u64, &str)> = (0..40_u64)
        .map(|n| (n, if n % 2 == 0 { "aterm" } else { "python3" }))
        .filter(|&(n, program)| {
            d.record(NO_TOKEN, &peer(100 + n as u32, program), at(n))
                .is_some()
        })
        .collect();
    for program in ["aterm", "python3"] {
        let nth: Vec<usize> = written
            .iter()
            .filter(|(_, p)| *p == program)
            .map(|(n, _)| *n as usize / 2 + 1)
            .collect();
        assert_eq!(nth, [1, 2, 4, 8, 16], "{program}");
    }
}

/// The kinds are the handshake's own words: each refusal the handshake
/// answers lands in its own slot, and a blind dial, a wrong token and a
/// token with no context to check it against are three different kinds.
#[test]
fn the_kinds_are_the_handshakes_reasons() {
    let refuse = |first: &str| {
        let Err(denial) = super::super::first_line_scope(first, "feedbeef", || None) else {
            panic!("`{first}` is refused");
        };
        denial
    };
    assert_eq!(refuse("sessions"), NO_TOKEN);
    assert_eq!(refuse("AUTH abab"), NO_CONTEXT);
    assert_eq!(refuse("TOKEN abab sessions"), NO_CONTEXT);
    assert_eq!(refuse("AUTHX abab"), NO_TOKEN);
}

/// THE AUDIT FLOOD: an unauthenticated peer that opens and closes a
/// connection a millisecond, from a new address each time, writes the 1st,
/// 2nd, 4th … of its refusals, each saying how many there were, not one line
/// per connection.
#[test]
fn a_flood_of_network_refusals_is_one_thinned_run() {
    let refusals = AuthDenials::new();
    let t0 = Instant::now();
    let line = |i: u64| {
        format!(
            "fleet: 198.51.100.{}:{} not admitted: received corrupt message",
            1 + i % 250,
            10_000 + i
        )
    };
    let written: Vec<String> = (0..1000_u64)
        .filter_map(|i| refusals.record_network(&line(i), t0 + Duration::from_millis(i)))
        .collect();
    assert_eq!(
        written.len(),
        10,
        "the 1st, 2nd, 4th … 512th of 1000 refusals: {written:#?}"
    );
    assert_eq!(written[0], line(0), "the first refusal is written as it is");
    assert_eq!(
        written[9],
        format!("{} (512 refused in 511.0ms, 512 since launch)", line(511))
    );
    assert_eq!(refusals.network_total(), 1000);

    let later = t0 + Duration::from_secs(600);
    let again = refusals
        .record_network("fleet: 198.51.100.251:4242 not admitted: no proof", later)
        .expect("a refusal after a long quiet is written");
    assert!(
        again.starts_with("fleet: 198.51.100.251:4242 not admitted: no proof (again after ")
            && again.ends_with("quiet; 1001 since launch)"),
        "{again}"
    );
}
