// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{MetaField, apply_meta_value};

#[test]
fn racing_meta_sets_keep_the_last_event_matching_final_state() {
    let ctx = crate::stub_session(0).ctx.clone();
    let threads: Vec<_> = (0..8)
        .map(|i| {
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                for j in 0..50 {
                    apply_meta_value(
                        &ctx,
                        MetaField::Title,
                        Some(format!("t{i}-{j}")),
                        aterm_slog::Actor::Ctl,
                    );
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    let final_title = ctx
        .meta
        .lock()
        .unwrap()
        .get("title")
        .expect("some writer won")
        .to_string();
    let tl = ctx.timeline.lock().unwrap();
    let last = tl
        .since(None)
        .rfind(|e| e.kind == "meta-change")
        .expect("changes were recorded");
    assert_eq!(
        last.payload.to_string(),
        format!(
            "field=title value={}",
            crate::control::pct_encode(&final_title)
        ),
        "the final meta-change event names the finally-stored value"
    );
}
