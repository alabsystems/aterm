// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fabric measurement's grammar ([`crate::spec`]), checked; mounted only by the ordinary
//! fabric_bench test target.

use std::path::{Path, PathBuf};

use crate::spec::*;

#[test]
fn an_output_path_is_required_separately_from_the_knobs() {
    assert!(parse("", Path::new("")).is_err());
    assert!(parse("out=/retired", Path::new("/typed-output")).is_err());
}

#[test]
fn the_defaults_fill_what_the_words_leave_out() {
    let spec = parse("", Path::new("/tmp/x")).expect("parses");
    assert_eq!(spec.out, PathBuf::from("/tmp/x"));
    assert_eq!(spec.scenario, Scenario::All);
    assert!(
        !spec.words().contains("scenario="),
        "default words stay compatible"
    );
    assert_eq!(
        parse("scenario=all", Path::new("/tmp/x")).expect("explicit all"),
        spec
    );
    assert_eq!(spec.samples, DEFAULT_SAMPLES);
    assert_eq!(spec.burst, DEFAULT_BURST);
    assert_eq!(spec.pingpong, DEFAULT_PINGPONG);
    assert_eq!(spec.attach, DEFAULT_ATTACH.to_vec());
    assert_eq!(spec.burst_senders(), 5);
    assert_eq!(
        (spec.durability, spec.horizon_ms),
        (Tier::Ordered, Some(ORDERED_HORIZON_MS)),
        "aterm opens its guarded broker ordered at its horizon (run.rs holds it to aterm's)"
    );
    assert_eq!(
        (spec.retention, spec.log, spec.retention_interval_ms),
        (true, Log::Auto, None),
        "what ships: the default rules at every open, the auto mirror, astream's minute"
    );
    assert!(spec.at_rest, "what ships: a fabric root's log is sealed");
    let again = parse(&spec.words(), Path::new("/tmp/x")).expect("round trip");
    assert_eq!(again, spec);
}

#[test]
fn every_knob_is_read_and_a_bad_word_is_refused() {
    let spec = parse(
        "samples=7 warmup=0 burst=101 pingpong=9 attach=2,4 rounds=1",
        Path::new("/tmp/x"),
    )
    .expect("ok");
    assert_eq!(
        (
            spec.samples,
            spec.warmup,
            spec.burst,
            spec.pingpong,
            spec.rounds
        ),
        (7, 0, 101, 9, 1)
    );
    assert_eq!(spec.attach, vec![2, 4]);
    assert_eq!(spec.burst_senders(), 2, "101 posts need a second sender");
    for scenario in [
        Scenario::Messaging,
        Scenario::Pingpong,
        Scenario::Attach,
        Scenario::Handoff,
        Scenario::Ask,
    ] {
        let word = scenario.word();
        let spec = parse(&format!("scenario={word}"), Path::new("/tmp/x")).expect(word);
        assert_eq!(spec.scenario, scenario);
        assert!(spec.words().contains(&format!(" scenario={word} ")));
        assert_eq!(
            parse(&spec.words(), Path::new("/tmp/x")).expect("round trip"),
            spec
        );
    }
    for bad in [
        "samples=0",
        "burst=x",
        "pingpong=0",
        "pingpong=-1",
        "attach=8,1",
        "attach=1,1",
        "attach=1,999",
        "burst=999999",
        "speed=11",
        "samples",
        "out=",
        "scenario=",
        "scenario=Pingpong",
        "scenario=pingpong,attach",
        "scenario=unknown",
    ] {
        assert!(parse(bad, Path::new("/tmp/x")).is_err(), "{bad}");
    }
}

#[test]
fn retention_the_mirror_and_the_tick_are_named_and_reproduce() {
    for (words, retention, log, every) in [
        ("retention=on log=auto", true, Log::Auto, None),
        ("retention=on log=off", true, Log::Off, None),
        ("retention=off", false, Log::Auto, None),
        ("retention=off log=off", false, Log::Off, None),
        ("retention_interval_ms=1000", true, Log::Auto, Some(1000)),
    ] {
        let spec = parse(words, Path::new("/tmp/x")).expect(words);
        assert_eq!(
            (spec.retention, spec.log, spec.retention_interval_ms),
            (retention, log, every),
            "{words}"
        );
        let said = spec.words();
        assert!(
            said.contains(&format!(
                " retention={} log={}",
                if retention { "on" } else { "off" },
                log.word()
            )),
            "{said}"
        );
        assert_eq!(
            parse(&said, Path::new("/tmp/x")).expect("round trip"),
            spec,
            "{said}"
        );
    }
    for bad in [
        "retention=yes",
        "retention=",
        "log=turns",
        "log=on",
        "retention_interval_ms=0",
        "retention_interval_ms=soon",
        "retention=off retention_interval_ms=1000",
    ] {
        let err = parse(bad, Path::new("/tmp/x")).expect_err(bad);
        assert!(
            err.contains("retention") || err.contains("log="),
            "{bad}: {err}"
        );
    }
}

#[test]
fn the_durability_word_names_the_tier_and_nothing_else() {
    for (word, tier) in [("strict", Tier::Strict), ("relaxed", Tier::Relaxed)] {
        let spec = parse(&format!("durability={word}"), Path::new("/tmp/x")).expect(word);
        assert_eq!(spec.durability, tier, "{word}");
        assert_eq!(spec.horizon_ms, None, "{word}: no horizon");
        assert_eq!(tier.word(), word);
        assert!(spec.words().ends_with(&format!(" durability={word}")));
        let again = parse(&spec.words(), Path::new("/tmp/x")).expect("round trip");
        assert_eq!(again, spec);
    }
    let ordered = parse("durability=ordered", Path::new("/tmp/x")).expect("ordered");
    assert_eq!(
        (ordered.durability, ordered.horizon_ms),
        (Tier::Ordered, Some(ORDERED_HORIZON_MS))
    );
    assert!(ordered.words().ends_with(&format!(
        " durability=ordered horizon_ms={ORDERED_HORIZON_MS}"
    )));
    let fast = parse("durability=ordered horizon_ms=1000", Path::new("/tmp/x")).expect("1 s");
    assert_eq!(fast.horizon_ms, Some(1000));
    assert_eq!(
        parse(&fast.words(), Path::new("/tmp/x")).expect("round trip"),
        fast
    );
    for bad in [
        "durability=strict horizon_ms=100",
        "durability=relaxed horizon_ms=100",
    ] {
        let err = parse(bad, Path::new("/tmp/x")).expect_err(bad);
        assert!(err.contains("horizon_ms="), "{bad}: {err}");
    }
    assert_eq!(
        parse("horizon_ms=1000", Path::new("/tmp/x"))
            .expect("the default arm's")
            .horizon_ms,
        Some(1000)
    );
    assert!(parse("durability=ordered horizon_ms=soon", Path::new("/tmp/x")).is_err());
    for bad in [
        "Strict",
        "RELAXED",
        "Ordered",
        "replicated",
        "",
        "relaxed,strict",
    ] {
        let err = parse(&format!("durability={bad}"), Path::new("/tmp/x")).expect_err(bad);
        assert!(err.contains("durability="), "{bad}: {err}");
    }
}

/// B10's two knobs: idle bystander sessions beside the talking two, and
/// the ping-pong arm's paced rounds.
#[test]
fn the_bystanders_and_the_paced_rounds_are_named_and_reproduce() {
    let bare = parse("", Path::new("/tmp/x")).expect("bare");
    assert_eq!((bare.bystanders, bare.paced), (0, DEFAULT_PACED));
    assert!(
        bare.words()
            .contains(&format!(" bystanders=0 paced={DEFAULT_PACED} ")),
        "{}",
        bare.words()
    );
    for (words, bystanders, paced) in [
        ("bystanders=64", 64, DEFAULT_PACED),
        ("bystanders=16 paced=0", 16, 0),
        ("paced=10", 0, 10),
    ] {
        let spec = parse(words, Path::new("/tmp/x")).expect(words);
        assert_eq!(
            (spec.bystanders, spec.paced),
            (bystanders, paced),
            "{words}"
        );
        assert_eq!(
            parse(&spec.words(), Path::new("/tmp/x")).expect("round trip"),
            spec,
            "{words}"
        );
    }
    for bad in [
        "bystanders=-1",
        "bystanders=x",
        "paced=many",
        "bystanders=999",
    ] {
        assert!(parse(bad, Path::new("/tmp/x")).is_err(), "{bad}");
    }
}

/// B11's two knobs: the ask arm's posts per leg, and how often its
/// escalated mix puts a person-lane post before an ordinary message.
#[test]
fn the_asks_and_the_mix_are_named_and_reproduce() {
    let bare = parse("", Path::new("/tmp/x")).expect("bare");
    assert_eq!(
        (bare.asks, bare.mix_every),
        (DEFAULT_ASKS, DEFAULT_MIX_EVERY)
    );
    assert!(
        bare.words().contains(&format!(
            " asks={DEFAULT_ASKS} mix_every={DEFAULT_MIX_EVERY} "
        )),
        "{}",
        bare.words()
    );
    for (words, asks, mix_every) in [
        ("asks=40", 40, DEFAULT_MIX_EVERY),
        ("mix_every=1", DEFAULT_ASKS, 1),
        ("scenario=ask asks=7 mix_every=3", 7, 3),
    ] {
        let spec = parse(words, Path::new("/tmp/x")).expect(words);
        assert_eq!((spec.asks, spec.mix_every), (asks, mix_every), "{words}");
        assert_eq!(
            parse(&spec.words(), Path::new("/tmp/x")).expect("round trip"),
            spec,
            "{words}"
        );
    }
    for bad in ["asks=0", "asks=x", "mix_every=0", "mix_every=-2"] {
        let err = parse(bad, Path::new("/tmp/x")).expect_err(bad);
        assert!(err.contains("positive"), "{bad}: {err}");
    }
}

/// B09's knob: the attach world's backlog, named only when it is there,
/// so every entry without it keeps its words.
#[test]
fn the_attach_backlog_is_named_only_when_set_and_reproduces() {
    let bare = parse("", Path::new("/tmp/x")).expect("bare");
    assert_eq!(bare.backlog, 0);
    assert!(!bare.words().contains("backlog"), "{}", bare.words());
    for (words, backlog) in [
        ("backlog=0", 0),
        ("backlog=1", 1),
        ("scenario=attach attach=8,32 backlog=500000", 500_000),
        (&*format!("backlog={}", backlog_max(3)), backlog_max(3)),
    ] {
        let spec = parse(words, Path::new("/tmp/x")).expect(words);
        assert_eq!(spec.backlog, backlog, "{words}");
        assert_eq!(
            spec.words().contains(&format!(" backlog={backlog}")),
            backlog > 0,
            "{words}"
        );
        assert_eq!(
            parse(&spec.words(), Path::new("/tmp/x")).expect("round trip"),
            spec,
            "{words}"
        );
    }
    for bad in [
        "backlog=-1",
        "backlog=many",
        &*format!("backlog={}", backlog_max(3) + 1),
        "scenario=attach backlog=500000",
        "attach=32 backlog=1300000",
        "scenario=pingpong backlog=10",
        "scenario=handoff backlog=10",
    ] {
        let err = parse(bad, Path::new("/tmp/x")).expect_err(bad);
        assert!(err.contains("backlog="), "{bad}: {err}");
    }
}

/// C04's knob: the ask world's asked table pre-filled, named only when it
/// is there, so every entry without it keeps its words.
#[test]
fn the_asked_prefill_is_named_only_when_set_and_reproduces() {
    let bare = parse("", Path::new("/tmp/x")).expect("bare");
    assert_eq!(bare.asked_prefill, 0);
    assert!(!bare.words().contains("asked_prefill"), "{}", bare.words());
    for (words, rows) in [
        ("asked_prefill=0", 0),
        ("scenario=ask asked_prefill=1", 1),
        (
            &*format!("scenario=ask asked_prefill={ASKED_PREFILL_MAX}"),
            ASKED_PREFILL_MAX,
        ),
    ] {
        let spec = parse(words, Path::new("/tmp/x")).expect(words);
        assert_eq!(spec.asked_prefill, rows, "{words}");
        assert_eq!(
            spec.words().contains(&format!(" asked_prefill={rows}")),
            rows > 0,
            "{words}"
        );
        assert_eq!(
            parse(&spec.words(), Path::new("/tmp/x")).expect("round trip"),
            spec,
            "{words}"
        );
    }
    for bad in [
        "asked_prefill=-1",
        "asked_prefill=many",
        &*format!("scenario=ask asked_prefill={}", ASKED_PREFILL_MAX + 1),
        "scenario=attach asked_prefill=10",
        "scenario=messaging asked_prefill=10",
    ] {
        let err = parse(bad, Path::new("/tmp/x")).expect_err(bad);
        assert!(err.contains("asked_prefill="), "{bad}: {err}");
    }
}

/// C13's knob: the messaging world's paced notes, named only when they are there, so every
/// entry without them keeps its words; refused past their cap and on a run with no messaging.
#[test]
fn the_paced_notes_are_named_only_when_set_and_reproduce() {
    let bare = parse("", Path::new("/tmp/x")).expect("bare");
    assert_eq!(bare.paced_notes, 0);
    assert!(!bare.words().contains("paced_notes"), "{}", bare.words());
    for (words, notes) in [
        ("paced_notes=0", 0),
        ("scenario=messaging paced_notes=60", 60),
        ("paced_notes=1", 1),
        (
            &*format!("scenario=messaging paced_notes={PACED_NOTES_MAX}"),
            PACED_NOTES_MAX,
        ),
    ] {
        let spec = parse(words, Path::new("/tmp/x")).expect(words);
        assert_eq!(spec.paced_notes, notes, "{words}");
        assert_eq!(
            spec.words().contains(&format!(" paced_notes={notes}")),
            notes > 0,
            "{words}"
        );
        assert_eq!(
            parse(&spec.words(), Path::new("/tmp/x")).expect("round trip"),
            spec,
            "{words}"
        );
    }
    for bad in [
        "paced_notes=-1",
        "paced_notes=many",
        &*format!("scenario=messaging paced_notes={}", PACED_NOTES_MAX + 1),
        "scenario=pingpong paced_notes=10",
        "scenario=ask paced_notes=10",
    ] {
        let err = parse(bad, Path::new("/tmp/x")).expect_err(bad);
        assert!(err.contains("paced_notes="), "{bad}: {err}");
    }
}

/// `at_rest=` reads, a plain run is sealed as what ships is, every entry says which it measured,
/// and a word it does not know is refused.
#[test]
fn the_at_rest_word_round_trips() {
    let plain = parse("", Path::new("/tmp/x")).expect("parses");
    assert!(plain.at_rest, "what ships");
    assert!(plain.words().contains(" at_rest=on"), "{}", plain.words());
    let off = parse("at_rest=off", Path::new("/tmp/x")).expect("parses");
    assert!(!off.at_rest);
    assert!(off.words().contains(" at_rest=off"), "{}", off.words());
    assert!(parse("at_rest=sealed", Path::new("/tmp/x")).is_err());
}

/// D02's knob: both tallies in the messaging, attach, handoff and ask worlds, named only when it
/// is on, so every entry without it keeps its words; refused on a run that measures only the
/// ping-pong world, which counts with both tallies in every run.
#[test]
fn the_tally_is_named_only_when_on_and_reproduces() {
    let bare = parse("", Path::new("/tmp/x")).expect("bare");
    assert!(
        !bare.tally,
        "off by default: the worlds boot as their baselines did"
    );
    assert!(!bare.words().contains("tally"), "{}", bare.words());
    for (words, on) in [
        ("tally=off", false),
        ("tally=on", true),
        ("scenario=attach attach=8,32 tally=on", true),
        ("scenario=handoff tally=on", true),
        ("scenario=messaging tally=on", true),
        ("scenario=ask tally=on", true),
        ("scenario=pingpong tally=off", false),
    ] {
        let spec = parse(words, Path::new("/tmp/x")).expect(words);
        assert_eq!(spec.tally, on, "{words}");
        assert_eq!(spec.words().contains(" tally=on"), on, "{words}");
        assert!(!spec.words().contains("tally=off"), "{words}");
        assert_eq!(
            parse(&spec.words(), Path::new("/tmp/x")).expect("round trip"),
            spec,
            "{words}"
        );
    }
    for bad in [
        "tally=yes",
        "tally=",
        "tally=1",
        "scenario=pingpong tally=on",
    ] {
        let err = parse(bad, Path::new("/tmp/x")).expect_err(bad);
        assert!(err.contains("tally="), "{bad}: {err}");
    }
}

#[cfg(target_os = "macos")]
#[test]
fn the_broker_process_is_named_only_out_of_process_and_refuses_what_its_argv_cannot_carry() {
    let out = Path::new("/tmp/x");
    let inproc = parse("", out).expect("the default");
    assert_eq!(inproc.broker_process, BrokerProcess::Inproc);
    assert!(
        !inproc.words().contains("broker_process="),
        "an in-process run's words stay what every earlier entry's were"
    );
    assert_eq!(
        parse("broker_process=inproc", out).expect("explicit"),
        inproc
    );
    for (word, process) in [
        ("background", BrokerProcess::Background),
        ("interactive", BrokerProcess::Interactive),
    ] {
        let spec = parse(&format!("broker_process={word} attach=8,32"), out).expect(word);
        assert_eq!(spec.broker_process, process);
        assert!(spec.words().contains(&format!(" broker_process={word}")));
        assert_eq!(parse(&spec.words(), out).expect("round trip"), spec);
        for tier in ["strict", "relaxed"] {
            let tiered = format!("broker_process={word} durability={tier}");
            assert!(
                parse(&tiered, out).is_ok(),
                "{tiered}: the argv names a tier"
            );
        }
        assert!(parse(&format!("broker_process={word} at_rest=off"), out).is_ok());
        for refused in [
            "scenario=handoff",
            "scenario=ask",
            "horizon_ms=1000",
            "retention=off",
            "retention_interval_ms=1000",
            "rate_limit=off",
            "log=off",
            "backlog=10 scenario=attach",
            "paced_notes=1 scenario=messaging",
            "bystanders=4",
            "tally=on",
        ] {
            let run = format!("broker_process={word} {refused}");
            let why = parse(&run, out).expect_err(&run);
            assert!(
                why.contains("runs `aterm link broker` as it ships"),
                "{run}: {why}"
            );
        }
        assert!(
            parse(
                &format!("broker_process={word} horizon_ms={ORDERED_HORIZON_MS}"),
                out
            )
            .is_ok(),
            "the shipped horizon named is the shipped horizon"
        );
    }
    assert!(parse("broker_process=launchd", out).is_err());
}
