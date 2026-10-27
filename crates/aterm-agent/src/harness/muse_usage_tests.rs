// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`parse`] against the recorded `usage/refresh`/`usage/read` answer shapes (Muse
//! 1.4.13, values invented): the fresh read, the absent usage, and out-of-range figures.

use std::io::Cursor;
use std::sync::{Arc, Mutex};

use super::AppServer;
use super::*;
use aterm_json::json;

/// A refresh answer: plan words and a dated weekly figure.
#[test]
fn a_refresh_answer_reads_its_plan_and_weekly_figure() {
    let got = parse(&json!({
        "subscriptionActive": true,
        "tierName": "Muse Code Max Usage",
        "usage": {
            "observedAtMs": 1793061227023_i64,
            "tier": "99",
            "weekly": {"resetsAtMs": 1793577600000_i64, "usedPercent": 46},
            "window": {"resetsAtMs": 1793063035000_i64, "usedPercent": 36, "windowDurationMins": 300}
        }
    }))
    .expect("a reading");
    assert_eq!(got.plan.as_deref(), Some("Muse Code Max Usage"));
    assert_eq!(got.weekly_used_x10, Some(460));
    assert_eq!(got.weekly_resets_at, Some(1_793_577_600));
    assert_eq!(got.observed_at, Some(1_793_061_227));
}

/// A read answer (no tierName) still gives its figure.
#[test]
fn a_read_answer_without_a_plan_gives_its_figure() {
    let got = parse(&json!({
        "usage": {
            "observedAtMs": 1793061227023_i64,
            "weekly": {"resetsAtMs": 1793577600000_i64, "usedPercent": 9}
        }
    }))
    .expect("a reading");
    assert_eq!(got.plan, None);
    assert_eq!(got.weekly_used_x10, Some(90));
}

/// Absent usage keeps only the plan words a mint named; with neither, nothing.
#[test]
fn an_absent_usage_keeps_only_named_plan_words() {
    let got = parse(&json!({"subscriptionActive": true, "tierName": "Muse Code Max Usage"}))
        .expect("plan words alone");
    assert_eq!(got.weekly_used_x10, None);
    assert_eq!(parse(&json!({})), None);
    assert_eq!(parse(&json!({"subscriptionActive": false})), None);
}

/// An over-quota percent reads whole and clamps to the words' range; a missing or
/// undated weekly half is no figure.
#[test]
fn figures_outside_their_halves_do_not_read() {
    let over = parse(&json!({
        "usage": {"weekly": {"resetsAtMs": 1793577600000_i64, "usedPercent": 137}}
    }))
    .expect("over-quota reads");
    assert_eq!(over.weekly_used_x10, Some(1000));
    assert_eq!(
        parse(&json!({"usage": {"weekly": {"usedPercent": 10}}})),
        None,
        "undated"
    );
    assert_eq!(
        parse(&json!({"usage": {"weekly": {"resetsAtMs": 1793577600000_i64, "usedPercent": -1}}})),
        None,
        "negative"
    );
}

/// A writer that keeps every line it is handed, for the wire below.
#[derive(Clone, Default)]
struct Sink(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn dialogue(answers: &str) -> (Option<super::Reading>, Vec<String>) {
    let sink = Sink::default();
    let server = AppServer {
        from: Box::new(Cursor::new(answers.as_bytes().to_vec())),
        to: Box::new(sink.clone()),
    };
    let got = read_usage(server, Instant::now() + aterm_spec::wait::MUST_HAPPEN);
    let sent = String::from_utf8(
        sink.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone(),
    )
    .expect("utf8 wire");
    let methods = sent
        .lines()
        .map(|line| {
            let message: aterm_json::Value = aterm_json::from_str(line).expect("a message");
            message
                .get("method")
                .and_then(aterm_json::Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    (got, methods)
}

/// The dialogue stamps `jsonrpc` (Muse's MSP refuses a message without it), opts into
/// the experimental API, and reads the refresh back into a figure.
#[test]
fn the_dialogue_stamps_jsonrpc_and_reads_the_refresh() {
    let answers = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"serverInfo\":{\"name\":\"muse\",\"version\":\"1.4.13\"}}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{",
        "\"subscriptionActive\":true,\"tierName\":\"Muse Code Max Usage\",",
        "\"usage\":{\"observedAtMs\":1793061227023,",
        "\"weekly\":{\"resetsAtMs\":1793577600000,\"usedPercent\":46}}}}\n"
    );
    let sink = Sink::default();
    let server = AppServer {
        from: Box::new(Cursor::new(answers.as_bytes().to_vec())),
        to: Box::new(sink.clone()),
    };
    let got =
        read_usage(server, Instant::now() + aterm_spec::wait::MUST_HAPPEN).expect("a reading");
    assert_eq!(got.weekly_used_x10, Some(460));
    assert_eq!(got.plan.as_deref(), Some("Muse Code Max Usage"));
    let sent = String::from_utf8(
        sink.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone(),
    )
    .expect("utf8 wire");
    let lines: Vec<&str> = sent.lines().collect();
    assert_eq!(lines.len(), 3, "{sent}");
    for line in &lines {
        let message: aterm_json::Value = aterm_json::from_str(line).expect("a message");
        assert_eq!(
            message.get("jsonrpc").and_then(aterm_json::Value::as_str),
            Some("2.0"),
            "{line}"
        );
    }
    assert!(
        lines[0].contains("\"experimentalApi\":true"),
        "{}",
        lines[0]
    );
    assert!(lines[1].contains("\"initialized\""), "{}", lines[1]);
    assert!(lines[2].contains("\"usage/refresh\""), "{}", lines[2]);
}

/// A mint that names a plan but carries no weekly figure does not mask the host's
/// complete last-observed read, and the fresher mint names the merged reading.
#[test]
fn a_plan_only_refresh_falls_back_to_a_complete_read() {
    let (got, methods) = dialogue(concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"serverInfo\":{\"name\":\"muse\"}}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{",
        "\"subscriptionActive\":true,\"tierName\":\"Muse Code Max Usage\"}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"usage\":{",
        "\"observedAtMs\":1793061227023,",
        "\"weekly\":{\"resetsAtMs\":1793577600000,\"usedPercent\":46}}}}\n"
    ));
    let got = got.expect("the complete read");
    assert_eq!(got.weekly_used_x10, Some(460));
    assert_eq!(got.weekly_resets_at, Some(1_793_577_600));
    assert_eq!(got.plan.as_deref(), Some("Muse Code Max Usage"));
    assert_eq!(
        methods,
        ["initialize", "initialized", "usage/refresh", "usage/read"]
    );
}

/// A refused mint reads the host's last observation, as before.
#[test]
fn a_refused_refresh_falls_back_to_the_last_observed_read() {
    let (got, methods) = dialogue(concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"serverInfo\":{\"name\":\"muse\"}}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"error\":{\"code\":-32080,",
        "\"message\":\"usage/refresh is rate limited\"}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"usage\":{",
        "\"observedAtMs\":1793061227023,",
        "\"weekly\":{\"resetsAtMs\":1793577600000,\"usedPercent\":46}}}}\n"
    ));
    let got = got.expect("the last-observed read");
    assert_eq!(got.weekly_used_x10, Some(460));
    assert_eq!(
        methods,
        ["initialize", "initialized", "usage/refresh", "usage/read"]
    );
}

/// An explicitly inactive subscription is final: stale tier and usage words in
/// the same answer name no reading, and no last-observed read is even asked.
#[test]
fn an_inactive_refresh_never_falls_back_to_a_stale_read() {
    let stale = json!({
        "subscriptionActive": false,
        "tierName": "Muse Code Max Usage",
        "usage": {
            "observedAtMs": 1_793_061_227_023_i64,
            "weekly": {"usedPercent": 46, "resetsAtMs": 1_793_577_600_000_i64}
        }
    });
    assert_eq!(parse(&stale), None, "inactive outranks stale words");
    let (got, methods) = dialogue(concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"serverInfo\":{\"name\":\"muse\"}}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{",
        "\"subscriptionActive\":false,\"tierName\":\"Muse Code Max Usage\",",
        "\"usage\":{\"observedAtMs\":1793061227023,",
        "\"weekly\":{\"resetsAtMs\":1793577600000,\"usedPercent\":46}}}}\n"
    ));
    assert_eq!(got, None);
    assert_eq!(methods, ["initialize", "initialized", "usage/refresh"]);
}
