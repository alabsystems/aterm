// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::time::Instant;

use aterm_session::input_backlog::InputWord;

use super::{
    AgentStamp, META_ATTENTION_KEYED_MAX, MetaEdit, MetaField, SessionTimeline,
    write_attention_owned, write_server_attention,
};
use crate::input_stall::{InputStallFact, SERVER_ATTENTION_OWNER};

fn stamp() -> AgentStamp {
    AgentStamp {
        generation: crate::control::ScreenGen { epoch: 1, seq: 7 },
        fp: 0x5eed,
    }
}

fn stall(stopped: bool) -> InputStallFact {
    InputStallFact {
        word: if stopped {
            InputWord::Stopped
        } else {
            InputWord::Stalled
        },
        since: Instant::now(),
        since_unix: None,
        bytes: 1,
        stopped,
        footprint_mb: Some(39_731),
        restart: crate::input_stall::Restart::default(),
        act: None,
    }
}

fn agent_changes(tl: &SessionTimeline) -> Vec<String> {
    tl.since(None)
        .filter(|event| event.kind == "agent-change")
        .map(|event| event.payload.to_string())
        .collect()
}

fn publish(tl: &mut SessionTimeline, word: &'static str, claude: bool) -> bool {
    let reader = claude.then_some(aterm_phase::Program::Claude);
    tl.publish_agent(word, None, None, reader, stamp())
}

#[test]
fn a_stall_on_an_agent_is_wall_unresponsive_and_its_clear_hands_back_the_screen() {
    let mut tl = SessionTimeline::default();
    assert!(publish(&mut tl, "prompt", true));
    assert_eq!((tl.agent().word, tl.agent().rev), ("prompt", 1));

    assert!(tl.set_input(Some(stall(false))));
    assert_eq!(tl.agent().word, "wall:unresponsive");
    assert_eq!(tl.agent().detail, None, "a stall's detail is `-`");
    assert_eq!(tl.agent().rev, 2);
    assert!(
        tl.agent()
            .wire_fields()
            .contains(" agent=wall:unresponsive agent_detail=- agent_rev=2 "),
        "{}",
        tl.agent().wire_fields()
    );

    assert!(!publish(&mut tl, "busy", true));
    assert_eq!((tl.agent().word, tl.agent().rev), ("wall:unresponsive", 2));
    assert_eq!(tl.agent().screen_word, "busy");
    assert!(!tl.set_input(Some(stall(false))));
    assert_eq!(tl.agent().rev, 2);

    assert!(tl.set_input(None));
    assert_eq!((tl.agent().word, tl.agent().rev), ("busy", 3));
    assert_eq!(
        agent_changes(&tl)
            .iter()
            .map(|p| p.split_once(' ').map_or(p.as_str(), |(w, _)| w))
            .collect::<Vec<_>>(),
        ["prompt", "wall:unresponsive", "busy"],
        "exactly one agent-change per effective move"
    );
}

#[test]
fn a_stopped_job_is_wall_unresponsive_with_detail_stopped() {
    let mut tl = SessionTimeline::default();
    publish(&mut tl, "busy", true);
    assert!(tl.set_input(Some(stall(false))));
    assert!(tl.set_input(Some(stall(true))));
    assert_eq!(tl.agent().word, "wall:unresponsive");
    assert_eq!(tl.agent().detail.as_deref(), Some("stopped"));
    assert_eq!(tl.agent().rev, 3);
}

#[test]
fn a_stall_on_no_agent_leaves_the_agent_word_alone() {
    let mut tl = SessionTimeline::default();
    publish(&mut tl, "-", false);
    assert!(!tl.set_input(Some(stall(false))));
    assert_eq!((tl.agent().word, tl.agent().rev), ("-", 0));
    assert!(
        tl.agent().input.is_some(),
        "kept for when a reader names it"
    );
    assert!(publish(&mut tl, "prompt", true));
    assert_eq!(tl.agent().word, "wall:unresponsive");
    assert!(tl.set_input(None));
    assert_eq!(tl.agent().word, "prompt");
}

#[test]
fn the_server_entry_outranks_the_supervisor_and_its_clear_uncovers_it() {
    let ctx = crate::stub_session(0).ctx.clone();
    let effective = || {
        let meta = ctx.meta.lock().unwrap();
        (
            meta.attention.clone(),
            meta.attention_owners.effective_owner().map(str::to_owned),
        )
    };
    let frozen = "claude is frozen: not reading input since 14:02 (1 B queued)";
    assert!(write_server_attention(
        &ctx,
        SERVER_ATTENTION_OWNER,
        Some(frozen.to_string())
    ));
    assert!(
        !ctx.meta.lock().unwrap().driver_wrote(MetaField::Attention),
        "no driver wrote it"
    );
    assert!(write_server_attention(&ctx, SERVER_ATTENTION_OWNER, None));
    assert_eq!(effective(), (None, None));

    write_attention_owned(
        &ctx,
        "supervisor",
        MetaEdit::Set("claude other: answer this box"),
        aterm_slog::Actor::Ctl,
    )
    .expect("accepted");
    assert!(write_server_attention(
        &ctx,
        SERVER_ATTENTION_OWNER,
        Some(frozen.to_string())
    ));
    assert_eq!(
        effective(),
        (Some(frozen.to_string()), Some("aterm".to_string()))
    );
    assert!(!write_server_attention(
        &ctx,
        SERVER_ATTENTION_OWNER,
        Some(frozen.to_string())
    ));
    assert!(write_server_attention(&ctx, SERVER_ATTENTION_OWNER, None));
    assert_eq!(
        effective(),
        (
            Some("claude other: answer this box".to_string()),
            Some("supervisor".to_string())
        )
    );
    let events: Vec<String> = ctx
        .timeline
        .lock()
        .unwrap()
        .since(None)
        .filter(|event| event.kind == "meta-change")
        .map(|event| event.payload.to_string())
        .collect();
    assert_eq!(events.len(), 5, "{events:?}");
    assert_eq!(
        events.last().map(String::as_str),
        Some("field=attention value=claude%20other:%20answer%20this%20box")
    );
}

#[test]
fn the_server_entry_is_capped_and_sanitized() {
    let ctx = crate::stub_session(0).ctx.clone();
    let long = format!("evil\u{202e}prog {}", "x".repeat(400));
    assert!(write_server_attention(
        &ctx,
        SERVER_ATTENTION_OWNER,
        Some(long)
    ));
    let shown = ctx.meta.lock().unwrap().attention.clone().expect("set");
    assert!(shown.len() <= META_ATTENTION_KEYED_MAX, "{}", shown.len());
    assert!(shown.starts_with("evilprog "), "{shown}");
    assert!(write_server_attention(
        &ctx,
        SERVER_ATTENTION_OWNER,
        Some("\u{202e}".to_string())
    ));
    assert_eq!(ctx.meta.lock().unwrap().attention, None);
}
