// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Vocabulary mirror tests between `aterm-slog`, `aterm-phase`, and fabric grammar.

use aterm_phase::{ApiCause, PromptKind, WallKind as PhaseWall};
use aterm_slog::vocab::{BOX_WORDS, WALL_WORDS};
use aterm_slog::{BoxWord, Verdict, WallKind};

fn prompt_kinds() -> Vec<PromptKind> {
    use PromptKind as P;
    let all = [
        P::Bash,
        P::Edit,
        P::Write,
        P::Overwrite,
        P::Read,
        P::Workflow,
        P::Trust,
        P::PowerShell,
        P::Fetch,
        P::NetworkRequest,
        P::Browser,
        P::Skill,
        P::Monitor,
        P::Tool,
        P::Question,
        P::PlanEnter,
        P::PlanExit,
        P::HeldMessage,
        P::GoalProposal,
        P::ComputerUse,
        P::ReadOutsideSetting,
        P::UsageLimit,
        P::ModelSwitch,
        P::AutocompactOffer,
        P::McpServer,
        P::Other,
    ];
    for k in all {
        match k {
            P::Bash
            | P::Edit
            | P::Write
            | P::Overwrite
            | P::Read
            | P::Workflow
            | P::Trust
            | P::PowerShell
            | P::Fetch
            | P::NetworkRequest
            | P::Browser
            | P::Skill
            | P::Monitor
            | P::Tool
            | P::Question
            | P::PlanEnter
            | P::PlanExit
            | P::HeldMessage
            | P::GoalProposal
            | P::ComputerUse
            | P::ReadOutsideSetting
            | P::UsageLimit
            | P::ModelSwitch
            | P::AutocompactOffer
            | P::McpServer
            | P::Other => {}
        }
    }
    all.to_vec()
}

fn walls() -> Vec<PhaseWall> {
    let all = [
        PhaseWall::UsageSession,
        PhaseWall::UsageWeekly,
        PhaseWall::ModelBucket { consent: false },
        PhaseWall::Spend,
        PhaseWall::Context,
        PhaseWall::Auth,
        PhaseWall::ApiError {
            code: Some(500),
            retryable: true,
            cause: ApiCause::Server,
        },
        PhaseWall::Overloaded,
        PhaseWall::Memory,
    ];
    for w in &all {
        match w {
            PhaseWall::UsageSession
            | PhaseWall::UsageWeekly
            | PhaseWall::ModelBucket { .. }
            | PhaseWall::Spend
            | PhaseWall::Context
            | PhaseWall::Auth
            | PhaseWall::ApiError { .. }
            | PhaseWall::Overloaded
            | PhaseWall::Memory => {}
        }
    }
    all.to_vec()
}

#[test]
fn vocab_mirrors_the_screen_reader() {
    for k in prompt_kinds() {
        assert!(BOX_WORDS.contains(&k.name()), "box word {:?}", k.name());
        let word = BoxWord::from_detail(k.name());
        assert_eq!(word.word(), k.name());
        let need = if word.is_question() {
            "question".to_string()
        } else {
            format!("approval:{}", k.name())
        };
        assert_eq!(word.need(), need);
    }
    for v in ["bash:read-only", "bash:not-read-only"] {
        assert_eq!(BoxWord::from_detail(v).kind(), "bash");
        assert_eq!(BoxWord::from_detail(v).need(), "approval:bash");
    }
    let retired = ["rate-nudge", "model-pick", "goal-resume"];
    for w in retired {
        assert!(BOX_WORDS.contains(&w), "{w}");
        assert!(prompt_kinds().iter().all(|k| k.name() != w), "{w}");
        assert_eq!(BoxWord::from_wire(w).word(), w);
    }
    assert_eq!(BOX_WORDS.len(), prompt_kinds().len() + 2 + retired.len());
    let ours: Vec<&str> = WallKind::ALL.iter().map(|k| k.name()).collect();
    for w in walls() {
        assert!(WALL_WORDS.contains(&w.name()), "wall word {:?}", w.name());
        assert!(ours.contains(&w.name()));
        assert_eq!(
            Verdict::from_agent_word(w.wire(), None),
            Verdict::Wall(WallKind::from_name(w.name()).expect("known"))
        );
    }
    assert_eq!(WALL_WORDS.len(), walls().len());
    assert_eq!(
        Verdict::from_agent_word("wall:unresponsive", Some("stopped")),
        Verdict::Unresponsive
    );
    assert_eq!(
        Verdict::from_agent_word("wall:new-kind", None),
        Verdict::Unknown
    );
}

#[test]
fn mail_story_and_verdict_words_mirror_the_fabric_and_the_story() {
    use aterm_slog::vocab::{MAIL_KIND_WORDS, WORD_WORDS};
    for k in aterm_types::fabric_grammar::KINDS {
        assert!(MAIL_KIND_WORDS.contains(&k), "mail kind {k:?}");
    }
    for v in aterm_types::fabric_grammar::VERDICTS {
        assert!(WORD_WORDS.contains(&v), "inbox verdict {v:?}");
    }
    for w in aterm_messages::presence::StoryVerb::TOLD_WORDS {
        assert!(WORD_WORDS.contains(&w), "story verb {w:?}");
    }
    assert!(!aterm_types::fabric_grammar::is_kind("teleport"));
    assert!(!MAIL_KIND_WORDS.contains(&"teleport"));
}
