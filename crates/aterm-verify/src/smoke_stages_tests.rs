// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::EnvSnapshot;
use crate::scope::Scope;

#[test]
fn required_initial_present_waits_for_a_scheduled_frame() {
    let clock = std::cell::Cell::new(Duration::from_secs(16));
    let mut polls = 0;
    let result = wait_for_initial_present(
        || {
            polls += 1;
            Ok(format!("OK frames={}", u8::from(polls == 151)))
        },
        || clock.get(),
        || clock.set(clock.get() + POLL_GAP),
    );
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(polls, 151);
    assert_eq!(metric_u64(&result.unwrap(), "frames"), Some(1));

    let mut polls = 0;
    let result = wait_for_initial_present(
        || {
            polls += 1;
            Ok("OK frames=0".into())
        },
        || Duration::from_secs(61),
        || panic!("expired observation never pauses"),
    );
    assert!(matches!(result, Err(InitialPresent::Unpresented(_))));
    assert_eq!(polls, 1);

    let result = wait_for_initial_present(
        || Err(InitialPresent::Exited),
        || Duration::ZERO,
        || panic!("an actual exit never pauses"),
    );
    assert!(matches!(result, Err(InitialPresent::Exited)));
}

fn ctx() -> Ctx {
    Ctx::new(
        PathBuf::from("/repo"),
        Scope::workspace(),
        EnvSnapshot::default(),
        PathBuf::from("/tmp"),
    )
}

fn summary(frames: u64, max_input: &str, retries: u64, drops: u64, sync: u64) -> String {
    format!(
        "OK backend=gpu rows=24 cols=80 scrollback_truncated_lines=0 frames={frames} \
         last_input_present_ms=6.10 max_input_present_ms={max_input} n_input=30 \
         sync_armed=0 sync_rel_end=0 sync_rel_timeout={sync} sync_holding=0 \
         perf_reduced=0 shed_transitions=0 wake_heals=0 redraw_retry_gated={retries} \
         fx_under_frames=0 present_drops={drops} deadline_arms_by_owner=cursor_effect:0/0 "
    )
}

fn hv(reply: &str) -> Result<String, Miss> {
    hardware_key_verdict(30, &summary(41, "8.90", 0, 0, 0), reply)
}

/// A busy CPU window and load, as a loaded admission or entry reads them.
fn busy() -> aterm_capacity::Observed {
    aterm_capacity::Observed {
        window: aterm_capacity::CpuWindow {
            interval_ns: 1_000_000_000,
            observed_ms: 1,
            cores: 14,
            busy_ticks: 63,
            total_ticks: 100,
        },
        load1: 9.0,
        max: 4.0,
    }
}

fn percentiles(
    n_key_write: u64,
    key_write: &str,
    press_max: &str,
    acquire: &str,
    input: &str,
) -> String {
    format!(
        "OK n_input=30 input_p50_ms=9.11 input_p95_ms=12.30 input_p99_ms={input} \
         n_present=31 present_p50_ms=8.20 present_p95_ms=10.10 present_p99_ms=11.40 \
         n_key_write={n_key_write} key_write_p50_ms=1.10 key_write_p95_ms=2.30 \
         key_queue_unavailable=0 key_write_unavailable=0 \
         key_write_p99_ms={key_write} n_pre_present=31 pre_present_p50_ms=0.90 \
         n_acquire=31 acquire_p50_ms=0.02 acquire_p95_ms=1.10 acquire_p99_ms={acquire} \
         last_acquire_wait_ms=0.02 max_acquire_wait_ms=4.80 \
         n_term_wait_redraw=40 term_wait_redraw_p99_ms=0.02 max_term_wait_redraw_ms=0.10 \
         n_term_wait_press=60 term_wait_press_p50_ms=0.00 term_wait_press_p95_ms=0.01 \
         term_wait_press_p99_ms=0.02 max_term_wait_press_ms={press_max} \
         n_echo=30 echo_p50_ms=3.10 echo_p95_ms=40.20 echo_p99_ms=150.04 echo_max_ms=160.00 \
         n_present_glass=31 present_glass_p50_ms=6.10 present_glass_p95_ms=8.20 \
         present_glass_p99_ms=8.90 last_present_glass_ms=6.00 max_present_glass_ms=12.40 \
         present_glass_skipped=0\n"
    )
}

fn with_max(reply: &str, field: &str, value: &str) -> String {
    let at = reply
        .find(&format!("{field}="))
        .expect("the field is in the reply");
    let from = at + field.len() + 1;
    let end = from + reply[from..].find(' ').expect("a field ends in a space");
    format!("{}{value}{}", &reply[..from], &reply[end..])
}

fn with_glass(n: u64, p99: &str) -> String {
    percentiles(30, "6.00", "0.10", "1.00", "10.00")
        .replace("n_present_glass=31", &format!("n_present_glass={n}"))
        .replace(
            "present_glass_p99_ms=8.90",
            &format!("present_glass_p99_ms={p99}"),
        )
}

fn burst_readings() -> BurstReadings {
    BurstReadings {
        posted: 30,
        summary: summary(40, "10.00", 0, 0, 0)
            .replace("fx_under_frames=0", "fx_under_frames=20")
            .replace("cursor_effect:0/0", "cursor_effect:100/0"),
        percentiles: percentiles(30, "6.00", "0.10", "1.00", "10.00"),
    }
}

struct BurstDrop<'a>(&'a std::cell::Cell<bool>);
impl Drop for BurstDrop<'_> {
    fn drop(&mut self) {
        self.0.set(true);
    }
}

#[test]
fn burst_entry_refusal_starts_no_reset_or_keys() {
    let mut r = Report::new("burst");
    guarded_burst::<()>(
        &mut r,
        || Err("entry unavailable".into()),
        |_| panic!("collection before entry"),
        |_| panic!("postcheck without entry"),
    );
    assert_eq!(
        r.outcomes().map(|(o, _)| o).collect::<Vec<_>>(),
        [crate::ladder::Outcome::Fail(
            crate::ladder::Severity::CouldNotRun
        )]
    );
}

#[test]
fn burst_foreground_and_post_activation_guards_suppress_input() {
    for scenario in 0..4 {
        let mut r = Report::new("burst");
        let dropped = std::cell::Cell::new(false);
        let alive_calls = std::cell::Cell::new(0);
        guarded_burst(
            &mut r,
            || Ok((BurstDrop(&dropped), String::new())),
            |_| {
                burst_target_ready(
                    || {
                        let n = alive_calls.get();
                        alive_calls.set(n + 1);
                        scenario != 0 && !(scenario == 2 && n == 1)
                    },
                    || scenario != 1,
                    || Err("outer hold expired during activation".into()),
                )?;
                panic!("reset/keys after unavailable target");
            },
            |_| panic!("no readings to qualify"),
        );
        assert!(dropped.get());
        assert!(!r.outcomes().any(|(o, _)| o == crate::ladder::Outcome::Ok));
        assert_eq!(alive_calls.get(), if scenario < 2 { 1 } else { 2 });
    }
}

#[test]
fn burst_retains_full_success_metrics_and_guard_until_postcheck() {
    let dropped = std::cell::Cell::new(false);
    let phase = std::cell::Cell::new(0);
    let mut r = Report::new("burst");
    let expected = burst_readings();
    guarded_burst(
        &mut r,
        || {
            phase.set(1);
            Ok((BurstDrop(&dropped), String::new()))
        },
        |_| {
            assert_eq!(phase.replace(2), 1);
            assert!(!dropped.get());
            Ok(burst_readings())
        },
        |_| {
            assert_eq!(phase.replace(3), 2);
            assert!(!dropped.get());
            Ok(Vec::new())
        },
    );
    assert!(dropped.get());
    assert_eq!(phase.get(), 3);
    assert_eq!(
        r.outcomes().map(|(o, _)| o).collect::<Vec<_>>(),
        [crate::ladder::Outcome::Ok, crate::ladder::Outcome::Ok]
    );
    for raw in [expected.summary, expected.percentiles] {
        assert_eq!(
            r.entries
                .iter()
                .filter(
                    |e| matches!(e, crate::ladder::Entry::Raw(text) if text.ends_with(raw.trim_end_matches('\n')))
                )
                .count(),
            1
        );
    }
}

#[test]
fn burst_failure_retains_bounded_summary_and_percentiles() {
    let mut readings = burst_readings();
    readings.summary = readings
        .summary
        .replace("fx_under_frames=20", "fx_under_frames=0");
    let expected = [readings.summary.clone(), readings.percentiles.clone()];
    let mut r = Report::new("burst");
    guarded_burst(
        &mut r,
        || Ok(((), String::new())),
        |_| Ok(readings),
        |_| Ok(Vec::new()),
    );
    assert!(r.outcomes().any(|(outcome, _)| {
        outcome == crate::ladder::Outcome::Fail(crate::ladder::Severity::GateFailed)
    }));
    for raw in expected {
        assert_eq!(
            r.entries.iter().filter(|entry| {
                matches!(entry, crate::ladder::Entry::Raw(text) if text.ends_with(raw.trim_end_matches('\n')))
            }).count(),
            1
        );
    }
}

#[test]
fn burst_post_protection_failure_keeps_readings_but_never_passes() {
    let dropped = std::cell::Cell::new(false);
    let mut r = Report::new("burst");
    guarded_burst(
        &mut r,
        || Ok((BurstDrop(&dropped), String::new())),
        |_| Ok(burst_readings()),
        |_| Err("protection expired".into()),
    );
    assert!(dropped.get());
    assert_eq!(
        r.outcomes().map(|(o, _)| o).collect::<Vec<_>>(),
        [crate::ladder::Outcome::Fail(
            crate::ladder::Severity::CouldNotRun
        )]
    );
    assert_eq!(r.entries.iter().filter(|e| matches!(e, crate::ladder::Entry::Raw(text) if text.starts_with("gui smoke metrics"))).count(), 2);
}

/// A BURST THAT RAN WITHOUT A QUALIFIED QUIET ADMISSION PASSES AND SAYS WHY ON EACH PASS: a
/// busy machine never blocks a release. NEGATIVE CONTROL: a
/// qualified burst's passes say nothing of it
/// ([`burst_retains_full_success_metrics_and_guard_until_postcheck`]).
#[test]
fn a_loaded_burst_passes_and_states_why() {
    let mut r = Report::new("burst");
    guarded_burst(
        &mut r,
        || Ok(((), String::new())),
        |_| Ok(burst_readings()),
        |_| Ok(vec![aterm_capacity::Unqualified::Admission(busy())]),
    );
    let passes: Vec<&str> = r
        .outcomes()
        .filter(|(o, _)| *o == crate::ladder::Outcome::Ok)
        .map(|(_, line)| line)
        .collect();
    assert_eq!(passes.len(), 2);
    for line in passes {
        assert!(
            line.contains(
                "ran without a qualified quiet admission (stated, not a finding): admitted loaded"
            ),
            "{line}"
        );
    }
    let mut quiet = Report::new("burst");
    guarded_burst(
        &mut quiet,
        || Ok(((), String::new())),
        |_| Ok(burst_readings()),
        |_| Ok(Vec::new()),
    );
    assert!(
        !quiet
            .outcomes()
            .any(|(_, line)| line.contains("qualified quiet"))
    );
}

/// A CEILING OR FLOOR A LOADED BURST MISSED IS UNDECIDED, STATED BESIDE THE PASS: its takes
/// are no evidence either way on that load. NEGATIVE CONTROLS: the
/// same breaches on a quiet admission, and on one whose hold only lapsed, fail; and what no
/// load explains fails however loaded the burst ran.
#[test]
fn a_breach_on_a_loaded_burst_is_undecided_and_on_a_quiet_one_fails() {
    let breaching = || {
        let mut readings = burst_readings();
        readings.summary = readings
            .summary
            .replace("cursor_effect:100/0", "cursor_effect:100/10");
        readings.percentiles = percentiles(30, "45.00", "0.10", "1.00", "10.00");
        readings
    };
    let run = |readings: BurstReadings, unqualified: Vec<aterm_capacity::Unqualified>| {
        let mut r = Report::new("burst");
        guarded_burst(
            &mut r,
            || Ok(((), String::new())),
            |_| Ok(readings),
            |_| Ok(unqualified),
        );
        r.outcomes()
            .map(|(o, line)| (o, line.to_owned()))
            .collect::<Vec<_>>()
    };
    let failed = crate::ladder::Outcome::Fail(crate::ladder::Severity::GateFailed);
    for loaded in [
        aterm_capacity::Unqualified::Admission(busy()),
        aterm_capacity::Unqualified::Entry(busy()),
    ] {
        let rows = run(
            breaching(),
            vec![loaded, aterm_capacity::Unqualified::Lapsed],
        );
        assert_eq!(rows.len(), 2, "{rows:?}");
        for (outcome, line) in &rows {
            assert_eq!(*outcome, crate::ladder::Outcome::Ok, "{line}");
            assert!(
                line.contains("; undecided, stated and not a finding"),
                "{line}"
            );
            assert!(line.contains("qualified quiet admission"), "{line}");
        }
        assert!(rows[0].1.contains("late arms"), "{rows:?}");
        assert!(
            rows[1].1.contains("hardware key→write — p99 45ms"),
            "{rows:?}"
        );
        let mut unread = breaching();
        unread.percentiles = unread.percentiles.replace("key_queue_unavailable=0 ", "");
        let rows = run(unread, vec![loaded]);
        assert_eq!(rows[1].0, failed, "unreported clocks fail under any load");
    }
    for quiet in [Vec::new(), vec![aterm_capacity::Unqualified::Lapsed]] {
        let rows = run(breaching(), quiet);
        assert_eq!(
            rows.iter().map(|(o, _)| *o).collect::<Vec<_>>(),
            [failed, failed],
            "{rows:?}"
        );
        assert!(!rows.iter().any(|(_, line)| line.contains("undecided")));
    }
}

/// EACH CEILING, AND EACH FLOOR A COUNT MISSED WITH SOME SAMPLES, IS A BREACH; WHAT NO LOAD
/// EXPLAINS IS A FAILURE: a count that booked nothing, an unreadable reply, clocks that did
/// not report, a slice without its maximum, a present retry, drop or sync timeout-release,
/// and effects that never engaged. A failure beside a breach is the one named, so an
/// undecided breach never hides it.
#[test]
fn the_verdicts_tell_a_breach_from_what_no_load_explains() {
    let breach = |got: Result<String, Miss>| matches!(got, Err(Miss::Breach(_)));
    let fail = |got: Result<String, Miss>| matches!(got, Err(Miss::Fail(_)));
    let effects = |ledger: &str, under: u64| {
        effects_verdict(&format!(
            "OK frames=40 fx_under_frames={under} deadline_arms_by_owner={ledger}"
        ))
    };
    assert!(breach(effects("cursor_effect:430/40", 30)), "late arms");
    assert!(fail(effects("cursor_effect:80/0", 0)), "no ribbon composed");
    assert!(fail(effects("session_status:12/0", 30)), "no arm at all");
    assert!(
        fail(effects_verdict("OK frames=40 fx_under_frames=3")),
        "no ledger"
    );
    let hw = |n_key_write: u64, key_write: &str, press: &str, acquire: &str, input: &str| {
        hv(&percentiles(n_key_write, key_write, press, acquire, input))
    };
    assert!(
        hw(30, "6.00", "0.10", "1.00", "10.00").is_ok(),
        "the control"
    );
    assert!(
        breach(hw(30, "46.13", "0.10", "1.00", "10.00")),
        "key→write"
    );
    assert!(
        breach(hw(30, "6.00", "30.00", "1.00", "10.00")),
        "press wait"
    );
    assert!(
        breach(hw(30, "6.00", "0.10", "1.00", "260.00")),
        "input p99"
    );
    assert!(
        breach(hw(14, "6.00", "0.10", "1.00", "10.00")),
        "too few writes"
    );
    assert!(
        fail(hw(0, "0.00", "0.00", "0.02", "9.00")),
        "no write at all"
    );
    let acquire = with_max(
        &percentiles(30, "6.00", "0.10", "56.00", "10.00"),
        "max_acquire_wait_ms",
        "60.00",
    );
    assert!(breach(hv(&acquire)), "acquire");
    assert!(breach(hv(&with_max(
        &with_glass(31, "92.00"),
        "max_present_glass_ms",
        "91.50"
    ))));
    let healthy = percentiles(30, "6.00", "0.10", "1.00", "10.00");
    let input = |n: u64| hv(&healthy.replace("n_input=30", &format!("n_input={n}")));
    assert!(breach(input(9)), "too few input samples");
    assert!(fail(input(0)), "no input sample at all");
    let with = |frames, max_input: &str, retries, drops, sync| {
        hardware_key_verdict(
            30,
            &summary(frames, max_input, retries, drops, sync),
            &healthy,
        )
    };
    assert!(breach(with(10, "8.90", 0, 0, 0)), "starved");
    assert!(fail(with(0, "8.90", 0, 0, 0)), "no frame at all");
    assert!(breach(with(41, "530.10", 0, 0, 0)), "slow");
    assert!(fail(with(41, "8.90", 4, 0, 0)), "retries");
    assert!(fail(with(41, "8.90", 0, 7, 0)), "drops");
    assert!(fail(with(41, "8.90", 0, 0, 2)), "sync timeouts");
    assert!(
        fail(with(41, "530.10", 0, 7, 0)),
        "a drop beside a slow maximum"
    );
    let slow_write = percentiles(30, "46.13", "0.10", "1.00", "10.00");
    let beside = |summary: &str, reply: &str| hardware_key_verdict(30, summary, reply);
    assert!(breach(beside(&summary(41, "8.90", 0, 0, 0), &slow_write)));
    assert!(
        fail(beside(&summary(0, "8.90", 0, 0, 0), &slow_write)),
        "no frame at all beside a slow write"
    );
    assert!(
        fail(beside(
            &summary(41, "8.90", 0, 0, 0),
            &slow_write.replace(" max_acquire_wait_ms=4.80", "")
        )),
        "a slice without its maximum beside a slow write"
    );
    assert!(fail(hv("OK n_input=30")), "unparsable");
    assert!(
        fail(hv(&healthy.replace(
            "key_write_unavailable=0",
            "key_write_unavailable=3"
        ))),
        "unavailable clocks"
    );
    let unmaxed = healthy.replace(" max_acquire_wait_ms=4.80", "");
    assert!(fail(hv(&unmaxed)), "a sampled slice without its maximum");
}

#[test]
fn burst_oversized_receipt_cannot_pass_or_be_retained() {
    for percentiles in [false, true] {
        let mut readings = burst_readings();
        let oversized = "x".repeat(METRICS_RECEIPT_MAX + 1);
        if percentiles {
            readings.percentiles = oversized.clone();
        } else {
            readings.summary = oversized.clone();
        }
        let mut r = Report::new("burst");
        guarded_burst(
            &mut r,
            || Ok(((), String::new())),
            |_| Ok(readings),
            |_| Ok(Vec::new()),
        );
        assert_eq!(
            r.outcomes().map(|(o, _)| o).collect::<Vec<_>>(),
            [crate::ladder::Outcome::Fail(
                crate::ladder::Severity::GateFailed
            )]
        );
        assert!(
            !r.entries
                .iter()
                .any(|e| matches!(e, crate::ladder::Entry::Raw(text) if text.contains(&oversized)))
        );
    }
}

#[test]
fn the_effects_verdict_demands_engagement_and_bounds_late_arms() {
    let reply = |ledger: &str, under: u64| {
        format!("OK frames=40 fx_under_frames={under} deadline_arms_by_owner={ledger} turns=9")
    };
    assert_eq!(
        cursor_effect_arms(&reply("session_status:12/0,cursor_effect:80/0", 5)),
        Some((80, 0))
    );
    assert_eq!(
        cursor_effect_arms(&reply("session_status:12/0", 5)),
        Some((0, 0))
    );
    assert_eq!(cursor_effect_arms("OK frames=40"), None);
    assert!(effects_verdict(&reply("cursor_effect:430/0", 30)).is_ok());
    assert!(
        effects_verdict(&reply("cursor_effect:430/40", 30)).is_err(),
        "over the absolute bound"
    );
    assert!(
        effects_verdict(&reply("cursor_effect:80/3", 30)).is_err(),
        "over one in fifty"
    );
    assert!(
        effects_verdict(&reply("session_status:12/0", 30)).is_err(),
        "a burst that armed no cursor_effect deadline measured nothing"
    );
    assert!(
        effects_verdict(&reply("cursor_effect:80/0", 0)).is_err(),
        "a burst that composed no ribbon measured nothing"
    );
    assert!(
        effects_verdict("OK frames=40 fx_under_frames=3").is_err(),
        "no ledger is not a pass"
    );
    assert!(
        effects_verdict("OK frames=40 deadline_arms_by_owner=cursor_effect:9/0").is_err(),
        "no ribbon witness is not a pass"
    );
}

#[test]
fn a_40ms_press_path_regression_fails_the_hardware_gate() {
    let hw = percentiles(30, "46.13", "0.10", "5.24", "58.00");
    assert!(hv(&hw).is_err());
}

#[test]
fn a_healthy_hardware_burst_passes_and_the_childs_echo_is_never_gated() {
    let hw = percentiles(30, "6.29", "0.10", "5.24", "14.20");
    hv(&hw).expect("a pass");
    let with = |frames, max_input: &str, retries, drops, sync| {
        hardware_key_verdict(30, &summary(frames, max_input, retries, drops, sync), &hw)
    };
    for (case, frames, max_input, retries, drops, sync) in [
        ("starved", 10, "8.90", 0, 0, 0),
        ("slow", 41, "530.10", 0, 0, 0),
        ("retries", 41, "8.90", 4, 0, 0),
        ("drops", 41, "8.90", 0, 7, 0),
        ("sync timeouts", 41, "8.90", 0, 0, 2),
    ] {
        assert!(
            with(frames, max_input, retries, drops, sync).is_err(),
            "{case}"
        );
    }
}

#[test]
fn a_burst_without_measured_hardware_writes_is_a_failure_not_a_pass() {
    let controller_shaped = percentiles(0, "0.00", "0.00", "0.02", "9.00");
    assert!(hv(&controller_shaped).is_err());
    let unavailable = controller_shaped
        .replace("key_queue_unavailable=0", "key_queue_unavailable=30")
        .replace("key_write_unavailable=0", "key_write_unavailable=30");
    assert!(hv(&unavailable).is_err());
}

#[test]
fn native_clock_quality_requires_every_observed_press_and_write() {
    let measured = percentiles(30, "6.00", "0.10", "1.00", "10.00");
    let known = hv(&measured).expect("all measured clocks pass");
    assert_eq!(reply_field(&known, "input_clock"), "native");
    let floor = measured.replace("n_key_write=30", "n_key_write=15");
    hv(&floor).expect("the measured-write floor remains sufficient for known clocks");
    for (queue, write) in [(1, 0), (0, 1), (1, 1), (15, 15)] {
        let mixed = floor
            .replace(
                "key_queue_unavailable=0",
                &format!("key_queue_unavailable={queue}"),
            )
            .replace(
                "key_write_unavailable=0",
                &format!("key_write_unavailable={write}"),
            );
        let fallback = hv(&mixed).expect_err("partial clocks cannot confirm whole-burst latency");
        assert!(matches!(fallback, Miss::Fail(_)), "{fallback:?}");
        let fallback = fallback.why();
        assert_eq!(reply_field(fallback, "input_clock"), "unconfirmed");
        assert_eq!(
            reply_field(fallback, "key_queue_unavailable"),
            queue.to_string()
        );
        assert_eq!(
            reply_field(fallback, "key_write_unavailable"),
            write.to_string()
        );
    }
    let legacy = measured
        .replace("key_queue_unavailable=0 ", "")
        .replace("key_write_unavailable=0 ", "");
    assert!(hv(&legacy).is_err());
    for field in ["key_queue_unavailable", "key_write_unavailable"] {
        for replacement in [String::new(), format!("{field}=unknown ")] {
            let unsupported = measured.replace(&format!("{field}=0 "), &replacement);
            assert!(hv(&unsupported).is_err(), "{unsupported}");
        }
    }
}

#[test]
fn a_burst_that_booked_no_input_samples_is_a_failure_not_a_pass() {
    let hw = |n: u64| {
        percentiles(30, "6.00", "0.10", "1.00", "0.00")
            .replace("n_input=30", &format!("n_input={n}"))
    };
    assert!(hardware_key_verdict(30, &summary(41, "0.00", 0, 0, 0), &hw(0)).is_err());
    assert!(hv(&hw(30)).is_ok(), "the control");
}

#[test]
fn a_sampled_slice_without_its_maximum_fails_closed() {
    let without = |reply: &str, field: &str| {
        let at = reply
            .find(&format!(" {field}="))
            .expect("the field is in the reply");
        let end = at + 1 + reply[at + 1..].find(' ').expect("a field ends in a space");
        format!("{}{}", &reply[..at], &reply[end..])
    };
    let healthy = percentiles(30, "6.00", "0.10", "1.00", "10.00");
    for field in ["max_acquire_wait_ms", "max_present_glass_ms"] {
        assert!(hv(&without(&healthy, field)).is_err(), "{field}");
    }
    let slow = percentiles(30, "6.00", "0.10", "56.00", "10.00");
    assert!(hv(&without(&slow, "max_acquire_wait_ms")).is_err());
    let unsampled = without(
        &without(&healthy, "max_acquire_wait_ms"),
        "max_present_glass_ms",
    )
    .replace("n_acquire=31", "n_acquire=0")
    .replace("n_present_glass=31", "n_present_glass=0");
    assert!(hv(&unsampled).is_ok(), "{unsampled}");
}

#[test]
fn a_compositor_that_holds_frames_fails_while_every_present_return_slice_stays_green() {
    let held = with_max(&with_glass(31, "92.00"), "max_present_glass_ms", "91.50");
    assert!(
        hv(&with_glass(31, "8.90")).is_ok(),
        "the same reply with a healthy compositor leg passes"
    );
    for (slice, healthy) in [
        ("key_write_p99_ms", 6u64),
        ("acquire_p99_ms", 1),
        ("input_p99_ms", 10),
    ] {
        assert_eq!(
            metric_ms_whole(&held, slice),
            Some(healthy),
            "{slice} is untouched by a compositor regression"
        );
    }
    assert!(hv(&held).is_err());
    assert!(hv(&with_glass(31, "56.00")).is_ok());
    assert!(hv(&with_glass(0, "900.00")).is_ok());
}

#[test]
fn a_verb_the_subject_lacks_is_told_from_its_answer() {
    assert!(refuses_verb("ERR unknown verb (try: help)\n", "aos"));
    assert!(refuses_verb(
        "aterm-ctl: ERR unknown verb (try: help)\n",
        "hwkey"
    ));
    assert!(refuses_verb("ERR aos needs a debug build\n", "aos"));
    assert!(!refuses_verb(AOS_BUNDLE_REFUSAL, "aos"));
    assert!(!refuses_verb(
        "aterm-ctl: ERR aos bundle needs a debug build",
        "aos"
    ));
    assert!(!refuses_verb("ERR aos attach refused: no window", "aos"));
    assert!(!refuses_verb("OK window=0 attached=false", "aos"));
    assert!(!refuses_verb("", "aos"));
}

#[test]
fn the_release_aos_step_passes_only_a_release_builds_answers() {
    let attach = "OK window=0 attached=true code=shipped hot=false active=false settling=false \
                  build=0.140.0 paused=false";
    let detach = "OK window=0 attached=false code=shipped hot=false active=false settling=false";
    assert!(aos_release_verdict(attach, detach, AOS_BUNDLE_REFUSAL).is_ok());
    assert!(
        aos_release_verdict(
            attach,
            detach,
            "aterm-ctl: ERR aos bundle needs a debug build\n"
        )
        .is_ok(),
        "the reply as `aterm ctl` prints it"
    );
    let old_release = "ERR aos needs a debug build";
    for (a, d, b) in [
        (old_release, detach, AOS_BUNDLE_REFUSAL),
        (attach, old_release, AOS_BUNDLE_REFUSAL),
        (attach, detach, old_release),
        (
            attach,
            detach,
            "ERR aos bundle: no plan was published over the glow",
        ),
        (
            "OK window=0 attached=false code=shipped hot=false",
            detach,
            AOS_BUNDLE_REFUSAL,
        ),
        (
            "ERR aos attach refused: no window",
            detach,
            AOS_BUNDLE_REFUSAL,
        ),
        (attach, detach, "aterm-ctl: ERR aos needs a debug build"),
        ("", "", ""),
    ] {
        assert!(aos_release_verdict(a, d, b).is_err(), "{a:?} {d:?} {b:?}");
    }
}

#[test]
fn an_unparsable_hardware_reply_fails_closed() {
    assert!(hv("").is_err());
    let summary =
        "OK frames=41 max_input_present_ms=8.100 last_key_write_ms=0.00 max_key_write_ms=0.00 ";
    assert!(hv(summary).is_err());
    let hw = percentiles(30, "6.00", "0.10", "1.00", "10.00");
    assert!(hardware_key_verdict(30, "OK frames=41", &hw).is_err());
    assert!(hardware_key_verdict(30, "", &hw).is_err());
}

#[test]
fn an_ssh_session_cannot_present_and_says_so() {
    let mut c = ctx();
    c.env.ssh_connection = Some("10.0.0.1 22 10.0.0.2 22".into());
    assert!(gui_smoke_unavailable(&c).is_some());
}

#[test]
fn the_sandbox_is_private_and_short_enough_for_a_unix_socket() {
    let mut sb = Sandbox::with_config("atl", SMOKE_CONFIG).expect("sandbox");
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&sb.rundir)
        .expect("stat")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        mode, 0o700,
        "the same-uid control-socket check depends on 0700"
    );
    assert!(sb.sock().as_os_str().len() < crate::smoke::SUN_LEN);
    assert!(sb.rundir.join("aterm").is_dir());
    let toml = std::fs::read_to_string(sb.cfgdir.join("aterm/aterm.toml")).expect("written");
    let doc = aterm_toml::from_str::<aterm_toml::Value>(&toml).expect("one document");
    assert_eq!(
        doc.get("bell_sound").and_then(aterm_toml::Value::as_bool),
        Some(false),
        "{toml}"
    );
    assert_eq!(
        lanes_left_on(&doc),
        Vec::<String>::new(),
        "the bundle's window starts no lane of the login's account: {toml}"
    );

    let mut r = Report::new("t");
    sb.teardown(&mut r, None);
    assert!(!sb.tmp.exists(), "the sandbox removes itself");
    assert_eq!(r.outcomes().count(), 0, "a clean teardown says nothing");
}

/// Each entry of [`aterm_types::world::LANES_OFF`] that `doc` does not hold with the same
/// value, as `table.key`.
pub(crate) fn lanes_left_on(doc: &aterm_toml::Value) -> Vec<String> {
    let off = aterm_toml::from_str::<aterm_toml::Value>(aterm_types::world::LANES_OFF)
        .expect("the lanes-off tables are one document");
    let mut on = Vec::new();
    for (table, keys) in off.as_table().expect("a document is a table") {
        for (key, value) in keys.as_table().expect("each entry is a table") {
            if doc.get(table).and_then(|held| held.get(key)) != Some(value) {
                on.push(format!("{table}.{key}"));
            }
        }
    }
    on
}

/// The handed bundle is a release build: its window reads no development seam, so the
/// converge runner's window lane stays off only for the scratch world's marker, and the
/// `_alab._tcp` browse for the marker and the world's `[swarm]` table both. The strip of the
/// run's own `ATERM_*` variables would otherwise leave the marker out. The window's command
/// carries it at the state folder under the world's HOME.
#[test]
fn the_smoke_window_carries_the_scratch_worlds_marker() {
    let ctx = ctx();
    let mut sb = Sandbox::with_config("atl", SMOKE_CONFIG).expect("sandbox");
    let cmd = sb.gui_command(&ctx, Path::new("/bin/true"));
    let marker = cmd
        .get_envs()
        .find(|(name, _)| name.to_str() == Some(aterm_types::world::MARKER))
        .and_then(|(_, value)| value.map(std::ffi::OsStr::to_owned));
    let state = aterm_types::world::state_home(&sb.tmp.join("home")).expect("a state folder");
    assert_eq!(marker, Some(state.into_os_string()), "{cmd:?}");
    let mut r = Report::new("t");
    sb.teardown(&mut r, None);
}

#[test]
fn the_smoke_instance_carries_the_runs_mark() {
    let mut ctx = ctx();
    ctx.child_env_add
        .push((crate::runs::VERIFY_RUN_ENV.into(), "71-1".into()));
    let mut cmd = Command::new("/bin/true");
    gui_env(&ctx, &mut cmd);
    let set: Vec<_> = cmd
        .get_envs()
        .filter_map(|(k, v)| v.map(|v| (k.to_owned(), v.to_owned())))
        .collect();
    assert_eq!(
        set,
        [(
            std::ffi::OsString::from(crate::runs::VERIFY_RUN_ENV),
            std::ffi::OsString::from("71-1")
        )],
        "{cmd:?}"
    );
}

#[test]
fn a_teardown_that_loses_its_child_is_a_failure() {
    let mut sb = Sandbox::with_config("atl", "").expect("sandbox");
    sb.child = Some(
        SmokeChild::spawn(
            Command::new("/bin/sh")
                .args(["-c", "exit 7"])
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
            |_| {},
        )
        .expect("spawn"),
    );
    let pid = sb.child.as_ref().expect("child").id();
    assert!(
        crate::smoke::exited_on_its_own(pid),
        "`exit 7` never exited within a minute"
    );
    let mut r = Report::new("t");
    sb.teardown(&mut r, None);
    assert_eq!(
        r.outcomes().map(|(o, _)| o).collect::<Vec<_>>(),
        [crate::Outcome::Fail(crate::Severity::GateFailed)]
    );
}
