// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_fill_is_deterministic_and_rotates() {
    assert_eq!(fill_line(0), "0 abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMN\n");
    assert_eq!(fill_line(1), "1 bcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNO\n");
    assert_eq!(fill_line(66), fill_line(0).replacen('0', "66", 1));
    let max_mod = u64::MAX % 66;
    assert_eq!(
        fill_line(u64::MAX),
        fill_line(max_mod).replacen(&max_mod.to_string(), &u64::MAX.to_string(), 1)
    );
}

#[test]
fn the_scroll_window_starts_with_the_audible_bell_off() {
    let doc = aterm_toml::from_str::<aterm_toml::Value>(&config()).expect("one document");
    assert_eq!(
        doc.get("bell_sound").and_then(aterm_toml::Value::as_bool),
        Some(false)
    );
    assert_eq!(
        doc.get("scrollback_lines")
            .and_then(aterm_toml::Value::as_integer),
        Some(200_000)
    );
    assert_eq!(
        crate::smoke_stages::tests::lanes_left_on(&doc),
        Vec::<String>::new()
    );
}

#[test]
fn a_window_is_quoted_by_its_present_and_render_percentiles() {
    let pct = json(
        r#"{"n_present":250,"present_p50_ms":4.1,"present_p95_ms":6.25,"present_p99_ms":8,
                "n_render":250,"render_p50_ms":0.5,"render_p95_ms":1,"render_p99_ms":1.44}"#,
    )
    .expect("percentiles");
    assert_eq!(
        presents(&pct),
        "n_present=250 present_p50/p95/p99=4.1/6.25/8ms \
         n_render=250 render_p50/p95/p99=0.5/1/1.44ms"
    );
    let scrolled = json(
        r#"{"n_present":0,"present_p50_ms":0,"present_p95_ms":0,"present_p99_ms":0,
                "n_render":178,"render_p50_ms":0.39,"render_p95_ms":0.46,"render_p99_ms":0.72}"#,
    )
    .expect("a scroll's percentiles");
    assert_eq!(
        presents(&scrolled),
        "n_present=0 n_render=178 render_p50/p95/p99=0.39/0.46/0.72ms"
    );
    assert_eq!(
        presents(&json("{}").expect("empty")),
        "n_present=? present_p50/p95/p99=?/?/?ms n_render=? render_p50/p95/p99=?/?/?ms"
    );
}

#[test]
fn the_jump_is_read_once_its_frame_has_rendered() {
    let mut seen = [0.0, 0.0, 1.0].into_iter();
    let mut reads = 0;
    let pct = await_rendered(FRAME_HANG, || {
        reads += 1;
        json(&format!("{{\"n_render\":{}}}", seen.next().unwrap_or(2.0)))
    })
    .expect("the third read has the frame");
    assert_eq!(
        (reads, pct.get("n_render").and_then(Value::as_f64)),
        (3, Some(1.0))
    );
    let e = await_rendered(Duration::ZERO, || json("{\"n_render\":0}"))
        .expect_err("no frame by the wait");
    assert!(e.contains("no frame") && e.ends_with("within 0s"), "{e}");
    let e =
        await_rendered(Duration::ZERO, || Err("no socket".to_string())).expect_err("a failed read");
    assert_eq!(e, "no socket");
}

#[test]
fn the_depth_is_the_third_field_of_the_scroll_report() {
    assert_eq!(depth("OK 0 120031"), Some(120_031));
    assert_eq!(depth("ERR nope"), None);
}
