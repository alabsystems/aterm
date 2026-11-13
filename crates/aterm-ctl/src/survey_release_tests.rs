// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_json::{Value, json};

const NODE: &str = "0399e3f9afdc79a108bd9b40c24cb837";
const SID: &str = "s-e83cb299a3edf3ea8d1e";

fn record() -> Value {
    json!({
        "line": {"version":"0.198.0","base":"base","started":1,"hops":[]},
        "holder": {"node":NODE,"host":"m3","sid":SID,"pid":53173,
            "boot":"7bb572ebe6e14df3961d8955aaf39a61","start":u64::MAX - 1},
        "step":"bundle","boundary":"step boundary","at":1,"p95_s":874,"rows":[]
    })
}

fn wire(alive: Option<&str>, value: &Value) -> String {
    format!(
        "release_line {}data={}",
        alive.map_or_else(String::new, |value| format!("alive={value} ")),
        aterm_codec::pct::encode(&value.to_string())
    )
}

fn reply(rows: Vec<String>) -> HostReply {
    let mut lines = vec![format!(
        "instance pid=2100 node={NODE} host=m3.local version=0.197.0 sessions=0 level=facts at_ms=10"
    )];
    lines.extend(rows);
    HostReply {
        label: "m3".into(),
        remote: true,
        unproven: false,
        node: Some(NODE.into()),
        reason: None,
        answer: parse_reply(&lines),
    }
}

fn rendered(host: HostReply) -> Value {
    aterm_json::from_str(&render_json(&[host])).unwrap()
}

#[test]
fn release_observations_keep_native_liveness_and_exact_holder_identity() {
    for (wire_alive, expected) in [
        (Some("1"), Some(true)),
        (Some("0"), Some(false)),
        (Some("-"), None),
        (None, None),
        (Some("false"), None),
    ] {
        let host = reply(vec![wire(wire_alive, &record())]);
        let value = rendered(host.clone());
        let row = &value[0]["release_lines"][0];
        assert_eq!(row["alive"].as_bool(), expected);
        assert_eq!(row["holder"]["node"].as_str(), Some(NODE));
        assert_eq!(row["holder"]["sid"].as_str(), Some(SID));
        assert_eq!(row["holder"]["pid"].as_u64(), Some(53173));
        assert_eq!(row["holder"]["start"].as_u64(), Some(u64::MAX - 1));
        // An old progress timestamp and a missing session do not override native liveness.
        assert_eq!(row["at"].as_u64(), Some(1));
        assert!(only_sid(std::slice::from_ref(&host), SID).is_empty());
        assert!(render_sid(&[host], SID, None).is_none());
    }
    let mut hopped = record();
    hopped
        .as_object_mut()
        .unwrap()
        .get_mut("line")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert("hops".into(), json!([{"commit":"last-hop"}]));
    let value = rendered(reply(vec![wire(Some("0"), &hopped)]));
    assert_eq!(
        value[0]["release_lines"][0]["line"]["head"].as_str(),
        Some("last-hop")
    );
}

#[test]
fn absent_cut_or_invalid_release_rows_leave_observations_unknown() {
    let mut invalid = vec![
        vec![],
        vec!["release_line alive=0 data=%7B".into()],
        vec![format!(
            "release_line alive=0 {}",
            wire(Some("1"), &record())
                .strip_prefix("release_line ")
                .unwrap()
        )],
        vec!["release_line alive=0 data=null".into()],
    ];
    for field in ["node", "sid", "boot", "pid", "start"] {
        let mut missing = record();
        missing
            .as_object_mut()
            .unwrap()
            .get_mut("holder")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove(field);
        invalid.push(vec![wire(Some("0"), &missing)]);
    }
    for rows in invalid {
        let value = rendered(reply(rows));
        assert!(value[0]["release_lines"].is_null(), "{value}");
    }
    let cut = HostReply {
        answer: parse_reply(&[
            format!("instance node={NODE} sessions=100 level=facts"),
            "session sid=s-one program=zsh".into(),
        ]),
        ..reply(vec![])
    };
    assert!(rendered(cut)[0]["release_lines"].is_null());
}

#[test]
fn unproven_or_different_observers_cannot_assert_release_holder_death() {
    let observed = reply(vec![wire(Some("0"), &record())]);
    for host in [
        HostReply {
            unproven: true,
            ..observed.clone()
        },
        HostReply {
            node: None,
            ..observed.clone()
        },
        HostReply {
            node: Some("another-node".into()),
            ..observed
        },
    ] {
        let text = render_full(std::slice::from_ref(&host), None);
        assert!(text.contains("holder=unknown"), "{text}");
        assert!(!text.contains("holder=dead"));
        assert!(rendered(host)[0]["release_lines"][0]["alive"].is_null());
    }
}

#[test]
fn retained_release_observations_do_not_displace_pressing_agent_excerpts() {
    for agent in ["prompt", "wall:quota", "busy"] {
        let mut lines = vec![format!(
            "instance node={NODE} host=m3.local version=0.197.0 sessions=1 level=content"
        )];
        lines.extend((0..40).map(|_| wire(Some("0"), &record())));
        lines.push(format!(
            "session sid=s-agent program=claude agent={agent} prompt=pressing-observation"
        ));
        let host = HostReply {
            answer: parse_reply(&lines),
            ..reply(vec![])
        };
        let text = render_read(&[host], 800, &[], None);
        assert!(text.chars().count() <= 800);
        assert!(text.contains("pressing-observation"), "{agent}: {text}");
        assert!(
            text.lines()
                .filter(|line| line.trim_start().starts_with("release "))
                .count()
                < 40
        );
    }
}

#[test]
fn release_details_escape_terminal_controls_and_fold_with_the_text_budget() {
    let mut value = record();
    let boundary = "step\n\u{1b}[31m\"quoted\"";
    let object = value.as_object_mut().unwrap();
    object.insert("boundary".into(), boundary.into());
    object.insert("step".into(), "bundle\n\u{1b}[31m".into());
    let host = reply((0..12).map(|_| wire(Some("1"), &value)).collect());
    let parsed = rendered(host.clone());
    assert_eq!(
        parsed[0]["release_lines"][0]["boundary"].as_str(),
        Some(boundary)
    );
    let full = render_full(std::slice::from_ref(&host), None);
    assert!(!full.contains('\u{1b}'));
    assert_eq!(
        full.lines()
            .filter(|line| line.trim_start().starts_with("release "))
            .count(),
        12
    );
    for budget in [100, 250, 500] {
        let text = render_read(std::slice::from_ref(&host), budget, &[], None);
        assert!(text.chars().count() <= budget, "{budget}: {text}");
        assert!(!text.contains('\u{1b}'));
    }
}
