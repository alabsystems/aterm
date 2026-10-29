// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fabric measurement's grammar ([`crate::spec`]), checked; mounted only by the ordinary
//! fabric_bench test target.

use std::path::PathBuf;

use crate::spec::*;

#[test]
fn the_defaults_fill_what_the_words_leave_out() {
    let spec = parse("out=/tmp/x").expect("parses");
    assert_eq!(spec.out, PathBuf::from("/tmp/x"));
    assert_eq!(spec.scenario, Scenario::All);
    assert!(
        !spec.words().contains("scenario="),
        "default words stay compatible"
    );
    assert_eq!(
        parse("out=/tmp/x scenario=all").expect("explicit all"),
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
    let again = parse(&format!("out=/tmp/x {}", spec.words())).expect("round trip");
    assert_eq!(again, spec);
}

#[test]
fn every_knob_is_read_and_a_bad_word_is_refused() {
    let spec =
        parse("out=/o samples=7 warmup=0 burst=101 pingpong=9 attach=2,4 rounds=1").expect("ok");
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
        let spec = parse(&format!("out=/o scenario={word}")).expect(word);
        assert_eq!(spec.scenario, scenario);
        assert!(spec.words().contains(&format!(" scenario={word} ")));
        assert_eq!(
            parse(&format!("out=/o {}", spec.words())).expect("round trip"),
            spec
        );
    }
    for bad in [
        "samples=5",
        "out=/o samples=0",
        "out=/o burst=x",
        "out=/o pingpong=0",
        "out=/o pingpong=-1",
        "out=/o attach=8,1",
        "out=/o attach=1,1",
        "out=/o attach=1,999",
        "out=/o burst=999999",
        "out=/o speed=11",
        "out=/o samples",
        "out=",
        "out=/o scenario=",
        "out=/o scenario=Pingpong",
        "out=/o scenario=pingpong,attach",
        "out=/o scenario=unknown",
    ] {
        assert!(parse(bad).is_err(), "{bad}");
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
        let spec = parse(&format!("out=/o {words}")).expect(words);
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
            parse(&format!("out=/o {said}")).expect("round trip"),
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
        let err = parse(&format!("out=/o {bad}")).expect_err(bad);
        assert!(
            err.contains("retention") || err.contains("log="),
            "{bad}: {err}"
        );
    }
}

#[test]
fn the_durability_word_names_the_tier_and_nothing_else() {
    for (word, tier) in [("strict", Tier::Strict), ("relaxed", Tier::Relaxed)] {
        let spec = parse(&format!("out=/o durability={word}")).expect(word);
        assert_eq!(spec.durability, tier, "{word}");
        assert_eq!(spec.horizon_ms, None, "{word}: no horizon");
        assert_eq!(tier.word(), word);
        assert!(spec.words().ends_with(&format!(" durability={word}")));
        let again = parse(&format!("out=/o {}", spec.words())).expect("round trip");
        assert_eq!(again, spec);
    }
    let ordered = parse("out=/o durability=ordered").expect("ordered");
    assert_eq!(
        (ordered.durability, ordered.horizon_ms),
        (Tier::Ordered, Some(ORDERED_HORIZON_MS))
    );
    assert!(ordered.words().ends_with(&format!(
        " durability=ordered horizon_ms={ORDERED_HORIZON_MS}"
    )));
    let fast = parse("out=/o durability=ordered horizon_ms=1000").expect("1 s");
    assert_eq!(fast.horizon_ms, Some(1000));
    assert_eq!(
        parse(&format!("out=/o {}", fast.words())).expect("round trip"),
        fast
    );
    for bad in [
        "durability=strict horizon_ms=100",
        "durability=relaxed horizon_ms=100",
    ] {
        let err = parse(&format!("out=/o {bad}")).expect_err(bad);
        assert!(err.contains("horizon_ms="), "{bad}: {err}");
    }
    assert_eq!(
        parse("out=/o horizon_ms=1000")
            .expect("the default arm's")
            .horizon_ms,
        Some(1000)
    );
    assert!(parse("out=/o durability=ordered horizon_ms=soon").is_err());
    for bad in [
        "Strict",
        "RELAXED",
        "Ordered",
        "replicated",
        "",
        "relaxed,strict",
    ] {
        let err = parse(&format!("out=/o durability={bad}")).expect_err(bad);
        assert!(err.contains("durability="), "{bad}: {err}");
    }
}

/// B10's two knobs: idle bystander sessions beside the talking two, and
/// the ping-pong arm's paced rounds.
#[test]
fn the_bystanders_and_the_paced_rounds_are_named_and_reproduce() {
    let bare = parse("out=/o").expect("bare");
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
        let spec = parse(&format!("out=/o {words}")).expect(words);
        assert_eq!(
            (spec.bystanders, spec.paced),
            (bystanders, paced),
            "{words}"
        );
        assert_eq!(
            parse(&format!("out=/o {}", spec.words())).expect("round trip"),
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
        assert!(parse(&format!("out=/o {bad}")).is_err(), "{bad}");
    }
}

/// B11's two knobs: the ask arm's posts per leg, and how often its
/// escalated mix puts a person-lane post before an ordinary message.
#[test]
fn the_asks_and_the_mix_are_named_and_reproduce() {
    let bare = parse("out=/o").expect("bare");
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
        let spec = parse(&format!("out=/o {words}")).expect(words);
        assert_eq!((spec.asks, spec.mix_every), (asks, mix_every), "{words}");
        assert_eq!(
            parse(&format!("out=/o {}", spec.words())).expect("round trip"),
            spec,
            "{words}"
        );
    }
    for bad in ["asks=0", "asks=x", "mix_every=0", "mix_every=-2"] {
        let err = parse(&format!("out=/o {bad}")).expect_err(bad);
        assert!(err.contains("positive"), "{bad}: {err}");
    }
}

/// B09's knob: the attach world's backlog, named only when it is there,
/// so every entry without it keeps its words.
#[test]
fn the_attach_backlog_is_named_only_when_set_and_reproduces() {
    let bare = parse("out=/o").expect("bare");
    assert_eq!(bare.backlog, 0);
    assert!(!bare.words().contains("backlog"), "{}", bare.words());
    for (words, backlog) in [
        ("backlog=0", 0),
        ("backlog=1", 1),
        ("scenario=attach attach=8,32 backlog=500000", 500_000),
        (&*format!("backlog={}", backlog_max(3)), backlog_max(3)),
    ] {
        let spec = parse(&format!("out=/o {words}")).expect(words);
        assert_eq!(spec.backlog, backlog, "{words}");
        assert_eq!(
            spec.words().contains(&format!(" backlog={backlog}")),
            backlog > 0,
            "{words}"
        );
        assert_eq!(
            parse(&format!("out=/o {}", spec.words())).expect("round trip"),
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
        let err = parse(&format!("out=/o {bad}")).expect_err(bad);
        assert!(err.contains("backlog="), "{bad}: {err}");
    }
}
