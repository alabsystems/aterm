// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// agy 1.4.6's `-p /usage --output-format json`, read on 2026-10-24 with the Gemini week spent.
const SPENT_GEMINI: &str = r#"{"conversation_id":"","status":"SUCCESS","response":"Gemini Models\tWeekly Limit Remaining\t0%\t2026-10-28T00:19:03Z\n","duration_seconds":0,"num_turns":0,"usage":{"input_tokens":0,"output_tokens":0,"thinking_tokens":0,"cache_read_tokens":0,"total_tokens":0},"command":{"name":"usage","data":{"description":"Within each group, models share a weekly limit and a 5-hour limit.","groups":[{"name":"Gemini Models","description":"Models within this group: Gemini Flash, Gemini Pro","buckets":[{"id":"gemini-weekly","name":"Weekly Limit Remaining","description":"You have hit your weekly limit, it refreshes in 3 days, 23 hours.","window":"weekly","remaining_fraction":0,"reset_time":"2026-10-28T00:19:03Z"},{"id":"gemini-5h","name":"Five Hour Limit Remaining","description":"You have hit your weekly limit, the 5-hour limit does not currently apply.","window":"5h","disabled":true,"remaining_fraction":0.9999967217445374}]},{"name":"Claude and GPT models","description":"Models within this group: Claude Opus, Claude Sonnet, GPT-OSS","buckets":[{"id":"3p-weekly","name":"Weekly Limit Remaining","description":"You have used some of your weekly limit.","window":"weekly","remaining_fraction":0.9589522480964661,"reset_time":"2026-10-31T00:49:13Z"},{"id":"3p-5h","name":"Five Hour Limit Remaining","description":"You have used some of your 5-hour limit.","window":"5h","remaining_fraction":0.9179044961929321,"reset_time":"2026-10-24T05:49:13Z"}]}]}}}"#;

/// 2026-10-24T00:55:13Z.
const NOW: i64 = 1_792_803_313;

#[test]
fn the_two_pools_are_read_apart() {
    let q = parse(SPENT_GEMINI).unwrap();
    assert_eq!(q.groups.len(), 2);
    let (gemini, third) = (&q.groups[0], &q.groups[1]);
    assert_eq!(
        (gemini.short().as_str(), third.short().as_str()),
        ("gemini", "claude/gpt")
    );
    assert!(gemini.blocked(), "a spent week blocks the group");
    assert!(!third.blocked());
    assert_eq!(
        gemini.unblocks_at(),
        aterm_types::rfc3339::parse_utc_fractional("2026-10-28T00:19:03Z")
    );
    let five = gemini.bucket(&Window::FiveHour).unwrap();
    assert!(
        five.disabled && !five.spent(),
        "a disabled limit is never the one that blocks"
    );
    assert_eq!(
        third.bucket(&Window::Weekly).unwrap().remaining,
        Some(0.958_952_248_096_466_1)
    );
}

#[test]
fn the_hud_names_each_pool_and_the_spent_one() {
    let line = hud_line(&parse(SPENT_GEMINI).unwrap(), NOW);
    assert_eq!(
        line,
        "agy quota \u{b7} gemini SPENT: week 0% left (refills in 3d23h), 5h off | claude/gpt: week 96% \
         left (refills in 6d23h), 5h 92% left (refills in 4h54m)"
    );
}

#[test]
fn the_json_keeps_every_bucket() {
    let v = to_json(&parse(SPENT_GEMINI).unwrap(), NOW);
    let groups = v.as_object().unwrap()["groups"].as_array().unwrap();
    let gemini = groups[0].as_object().unwrap();
    assert_eq!(gemini["blocked"].as_bool(), Some(true));
    let buckets = gemini["buckets"].as_array().unwrap();
    assert_eq!(
        buckets[0].as_object().unwrap()["spent"].as_bool(),
        Some(true)
    );
    assert_eq!(
        buckets[1].as_object().unwrap()["disabled"].as_bool(),
        Some(true)
    );
    assert_eq!(v.as_object().unwrap()["kind"].as_str(), Some("agy-quota"));
}

#[test]
fn what_is_not_a_usage_document_is_refused() {
    assert!(
        parse("Gemini Models\tWeekly Limit Remaining\t0%").is_err(),
        "the text form"
    );
    assert!(
        parse(r#"{"status":"ERROR","response":"not signed in"}"#)
            .unwrap_err()
            .contains("not signed in")
    );
    assert!(parse(r#"{"status":"SUCCESS","command":{"name":"credits","data":{}}}"#).is_err());
    assert!(
        parse(r#"{"status":"SUCCESS","command":{"name":"usage","data":{"groups":[]}}}"#).is_err()
    );
}

#[test]
fn fractions_are_clamped_and_never_round_to_a_lie() {
    let doc = r#"{"status":"SUCCESS","command":{"name":"usage","data":{"groups":[{"name":"X models","buckets":[{"id":"a","window":"weekly","remaining_fraction":1.7},{"id":"b","window":"hourly","remaining_fraction":-1}]}]}}}"#;
    let q = parse(doc).unwrap();
    let g = &q.groups[0];
    assert_eq!(g.short(), "x");
    assert_eq!(g.buckets[0].remaining, Some(1.0));
    assert_eq!(g.buckets[1].window, Window::Other("hourly".to_owned()));
    assert!(g.buckets[1].spent());
    assert_eq!(
        (percent(0.001), percent(0.999), percent(0.0), percent(1.0)),
        (
            "<1%".to_owned(),
            "99%".to_owned(),
            "0%".to_owned(),
            "100%".to_owned()
        )
    );
    assert_eq!(
        (
            span(-5),
            span(30),
            span(3_600 * 5 + 60),
            span(86_400 * 2 + 3_600)
        ),
        (
            "now".to_owned(),
            "1m".to_owned(),
            "5h01m".to_owned(),
            "2d1h".to_owned()
        )
    );
}

fn pools(gemini: (f64, bool), third: (f64, f64)) -> Quota {
    let doc = format!(
        r#"{{"status":"SUCCESS","command":{{"name":"usage","data":{{"groups":[{{"name":"Gemini Models","description":"Models within this group: Gemini Flash, Gemini Pro","buckets":[{{"id":"gemini-weekly","window":"weekly","remaining_fraction":{},"reset_time":"2026-10-28T00:19:03Z"}},{{"id":"gemini-5h","window":"5h","disabled":{},"remaining_fraction":1}}]}},{{"name":"Claude and GPT models","description":"Models within this group: Claude Opus, Claude Sonnet, GPT-OSS","buckets":[{{"id":"3p-weekly","window":"weekly","remaining_fraction":{},"reset_time":"2026-10-31T00:49:13Z"}},{{"id":"3p-5h","window":"5h","remaining_fraction":{},"reset_time":"2026-10-24T05:49:13Z"}}]}}]}}}}}}"#,
        gemini.0, gemini.1, third.0, third.1
    );
    parse(&doc).unwrap()
}

#[test]
fn every_model_finds_its_pool_by_family() {
    let q = parse(SPENT_GEMINI).unwrap();
    for (model, pool) in [
        ("gemini-4-argon-high", "Gemini Models"),
        ("gemini-3.8-flash-low", "Gemini Models"),
        ("Gemini 4 Argon (High)", "Gemini Models"),
        ("claude-opus-5-5-medium", "Claude and GPT models"),
        ("Claude Sonnet 5.5 (Low)", "Claude and GPT models"),
        ("gpt-oss-120b-medium", "Claude and GPT models"),
        ("GPT-OSS 120B (Medium)", "Claude and GPT models"),
    ] {
        assert_eq!(
            q.group_of(model).map(|g| g.name.as_str()),
            Some(pool),
            "{model}"
        );
    }
    assert!(q.group_of("llama-9").is_none());
    assert_eq!(family("GPT-OSS 120B (Medium)"), "gpt-oss");
}

#[test]
fn a_spent_pool_rotates_the_launch_to_the_other_pool() {
    let r = Rotation::default();
    let q = parse(SPENT_GEMINI).unwrap();
    assert_eq!(
        pick(&q, "gemini-4-argon-high", true, &r),
        Pick::Rotate {
            from: "gemini-4-argon-high".to_owned(),
            from_standing: Standing::Spent,
            to: "claude-opus-5-5-medium".to_owned(),
            to_standing: Standing::Open,
        },
        "even a named model leaves a spent pool"
    );
    assert_eq!(
        pick(&q, "claude-opus-5-5-medium", false, &r),
        Pick::Keep {
            model: "claude-opus-5-5-medium".to_owned(),
            standing: Standing::Open,
        }
    );
}

#[test]
fn a_low_pool_rotates_the_default_and_keeps_a_named_model() {
    let r = Rotation::default();
    let q = pools((0.9, false), (0.06, 0.8));
    let p = pick(&q, "claude-opus-5-5-medium", false, &r);
    assert_eq!(p.model(), Some("gemini-4-argon-high"), "{}", p.describe());
    assert!(matches!(
        pick(&q, "claude-opus-5-5-medium", true, &r),
        Pick::Keep {
            standing: Standing::Low,
            ..
        }
    ));
    let both_low = pools((0.05, false), (0.06, 0.8));
    assert!(
        matches!(
            pick(&both_low, "claude-opus-5-5-medium", false, &r),
            Pick::Keep {
                standing: Standing::Low,
                ..
            }
        ),
        "no open pool to go to: a low one is still usable"
    );
}

#[test]
fn a_spent_pool_with_only_a_low_one_left_takes_it_and_with_none_waits() {
    let r = Rotation::default();
    let q = pools((0.0, true), (0.04, 0.5));
    assert_eq!(
        pick(&q, "gemini-3.8-flash-low", false, &r).model(),
        Some("claude-opus-5-5-medium")
    );
    let none = pools((0.0, true), (0.0, 0.5));
    let p = pick(&none, "gemini-4-argon-high", false, &r);
    assert_eq!(
        p,
        Pick::Wait {
            model: "gemini-4-argon-high".to_owned(),
            until: aterm_types::rfc3339::parse_utc_fractional("2026-10-28T00:19:03Z"),
        },
        "the earliest refill among the spent pools"
    );
    assert_eq!(p.model(), None);
    assert!(
        p.describe().contains("2026-10-28T00:19:03Z"),
        "{}",
        p.describe()
    );
}

#[test]
fn rotation_off_or_an_unknown_model_changes_nothing() {
    let q = parse(SPENT_GEMINI).unwrap();
    let off = Rotation {
        enabled: false,
        ..Rotation::default()
    };
    assert!(matches!(
        pick(&q, "gemini-4-argon-high", false, &off),
        Pick::Keep {
            standing: Standing::Spent,
            ..
        }
    ));
    assert_eq!(
        pick(&q, "llama-9", false, &Rotation::default()),
        Pick::Unknown {
            model: "llama-9".to_owned()
        }
    );
}

#[test]
fn standing_reads_only_the_limits_that_apply() {
    let q = parse(SPENT_GEMINI).unwrap();
    assert_eq!(q.groups[0].standing(DEFAULT_LOW), Standing::Spent);
    assert_eq!(
        q.groups[0].least_left(),
        Some(0.0),
        "the disabled 5h is not counted"
    );
    assert_eq!(q.groups[1].standing(DEFAULT_LOW), Standing::Open);
    assert_eq!(q.groups[1].standing(0.95), Standing::Low);
}

#[test]
fn harness_agy_sets_rotation_and_the_supervisor_takes_the_keys_quietly() {
    let text = "[harness.agy]\nrotate = false\nlow = 15\norder = [\"gemini-4-argon-high\", \"claude-sonnet-5-5-medium\"]\n";
    let (r, notes) = Rotation::from_aterm_toml(text);
    assert!(notes.is_empty(), "{notes:?}");
    assert_eq!(
        r,
        Rotation {
            enabled: false,
            low: 0.15,
            order: vec![
                "gemini-4-argon-high".to_owned(),
                "claude-sonnet-5-5-medium".to_owned()
            ],
        }
    );
    let (cfg_notes, cfg) = {
        let (cfg, notes) = crate::supervise::config::SupervisorConfig::from_aterm_toml(text);
        (notes, cfg)
    };
    assert!(cfg_notes.is_empty(), "{cfg_notes:?}");
    assert_eq!(
        cfg,
        crate::supervise::config::SupervisorConfig::default(),
        "rotation keys set no supervisor field"
    );
    let (r, notes) =
        Rotation::from_aterm_toml("[harness.agy]\nrotate = \"yes\"\nlow = 1.5e3\norder = []\n");
    assert_eq!(r, Rotation::default());
    assert_eq!(notes.len(), 3, "{notes:?}");
    assert_eq!(Rotation::from_aterm_toml("").0, Rotation::default());
}
