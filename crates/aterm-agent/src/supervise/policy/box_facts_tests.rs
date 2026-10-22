// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::*;
use crate::supervise::policy::box_command::BoxCommand;
use crate::supervise::policy::claude_settings::AskKind;
use crate::supervise::policy::probe::LiveProbe;
use crate::supervise::run::WorkerSource;
use crate::supervise::tape::header::Roots;

/// Roots a test's paths are written under.
fn roots() -> Roots {
    Roots::of(vec![
        ("home", PathBuf::from("/Users//owner")),
        ("state", PathBuf::from("/Users//owner/state")),
    ])
}

/// Every fact set, the worker known by its environment (with a secret in
/// it), and a question answered by a text priority.
fn every_fact() -> BoxFacts {
    BoxFacts {
        program: Some("claude".to_string()),
        cwd: Some(PathBuf::from("/Users//owner/src/app")),
        trusted_cwd: Some(PathBuf::from("/Users//owner/src/app")),
        bypass: true,
        limit_reset_passed: true,
        limit_retry: true,
        worker: Some(Ok(WorkerEnv::from_env(vec![
            "ANTHROPIC_API_KEY=sk-planted-secret".to_string(),
            "PATH=/usr/bin".to_string(),
        ])
        .with_pid(4242))),
        shell_cwds: vec![PathBuf::from("/Users//owner/src/app/sub")],
        shell: Some(Ok(ShellStartup::of_parts(
            vec![("alias".to_string(), "ls".to_string(), true)],
            false,
        ))),
        owner_asks: Some(Ok(vec![OwnerAsk {
            kind: AskKind::Rule {
                tool: "Bash".to_string(),
                bare: false,
            },
            text: "Bash(sed:*)".to_string(),
            source: "/Users//owner/.claude/settings.json".to_string(),
        }])),
        screen_cols: 120,
        screen_rows: 40,
        question: Some(QuestionFacts {
            answer: true,
            priority: QuestionPriority::LongTerm,
            own: Some(QuestionPriority::ShortTerm),
            fell_back: true,
        }),
        box_call: Some("Bash".to_string()),
        box_command: Some(Ok(BoxCommand {
            id: "toolu_lab16914d0fee39f3eb".to_string(),
            command: "echo $(true)\n$(true)".to_string(),
        })),
        login_carry: true,
        login_back: true,
        login_renewed: true,
        login_method: Some(aterm_phase::LoginMethod::Subscription),
        worker_agent: Some(aterm_phase::Program::Gemini),
    }
}

/// `facts` written as a recorder writes them under [`roots`].
fn written(facts: &BoxFacts) -> String {
    let roots = roots();
    facts.to_json(7, &|p: &Path| roots.encode(p))
}

/// `text` read back under [`roots`].
fn read(text: &str) -> (BoxFacts, Vec<&'static str>) {
    let roots = roots();
    BoxFacts::from_json(text, &|raw| roots.decode(raw)).expect("the facts read")
}

/// THE FACTS READ BACK AS WRITTEN: every field, written and read, writes the
/// same text again, and a mark that carries them all needs nothing. Paths
/// are written under the run's roots, and the worker by its digest and pid.
#[test]
fn facts_read_back_as_written() {
    let facts = every_fact();
    let text = written(&facts);
    assert!(text.contains("\"@home/src/app\""), "{text}");
    let (back, needs) = read(&text);
    assert!(needs.is_empty(), "{needs:?}");
    assert_eq!(written(&back), text);
    assert_eq!(back.cwd, facts.cwd);
    assert_eq!(back.trusted_cwd, facts.trusted_cwd);
    assert_eq!(back.shell_cwds, facts.shell_cwds);
    assert_eq!(back.worker_agent, Some(aterm_phase::Program::Gemini));
    assert!(text.contains("\"worker_agent\":\"gemini\""), "{text}");
    assert_eq!(back.box_command, facts.box_command, "the transcript's call");
    let refused = BoxFacts {
        box_command: Some(Err("2 calls waiting".to_string())),
        ..BoxFacts::default()
    };
    let (none_taken, _) = read(&written(&refused));
    assert_eq!(
        none_taken.box_command, refused.box_command,
        "why none was taken"
    );
    let worker = back.worker.expect("read").expect("ok");
    assert_eq!(worker.pid(), Some(4242));
    assert_eq!(
        worker.digest(0),
        facts.worker.expect("set").expect("ok").digest(7),
        "the digest the tape's seed gave, whatever seed reads it back"
    );
}

/// THE WORKER'S ENVIRONMENT NEVER REACHES A MARK: a planted secret in it is
/// nowhere in the facts' text. Negative control: the same value as a fact
/// the facts do hold (a transcript candidate) is found, so the search sees
/// what is written.
#[test]
fn the_workers_environment_is_never_written() {
    let text = written(&every_fact());
    assert!(!text.contains("sk-planted-secret"), "{text}");
    assert!(!text.contains("ANTHROPIC_API_KEY"), "{text}");
    let control = BoxFacts {
        box_call: Some("sk-planted-secret".to_string()),
        ..BoxFacts::default()
    };
    assert!(written(&control).contains("sk-planted-secret"));
}

/// A FACT A LATER BUILD GREW, READ BY THIS ONE, AND ONE AN EARLIER BUILD
/// LACKED: a key this build does not know is ignored; a field the mark does
/// not carry reads at its default and is NAMED among what the decision
/// needs, never presented as what the recorded run knew.
#[test]
fn a_missing_fact_is_named_and_an_unknown_one_ignored() {
    let text = written(&every_fact());
    let older = text.replacen("\"shell_cwds\":[\"@home/src/app/sub\"],", "", 1);
    assert_ne!(older, text, "the field was taken out");
    let newer = older.replacen('{', "{\"a_later_fact\":[1,2],", 1);
    let legacy = newer.replacen(
        '{',
        "{\"model_restore\":{\"model\":\"retired\",\"effort\":null},\"goal_resume\":true,\"nudge\":{\"open\":true},",
        1,
    );
    let (back, needs) = read(&legacy);
    assert_eq!(needs, vec!["shell_cwds"]);
    assert!(back.shell_cwds.is_empty());
    assert_eq!(back.screen_cols, 120, "every other fact as written");
    let (_, all) = read("{}");
    assert_eq!(all, FIELDS.to_vec(), "an empty object needs every fact");
}

/// Facts this build cannot read are refused, never guessed: a worker's
/// digest that is not hex, a priority it does not know, a path under a root
/// the header does not name, text that is no object.
#[test]
fn facts_this_build_cannot_read_are_refused() {
    let roots = roots();
    let refuse = |text: &str| BoxFacts::from_json(text, &|raw| roots.decode(raw)).is_err();
    assert!(refuse("[]"));
    assert!(refuse(
        "{\"worker\":{\"ok\":{\"digest\":\"xyz\",\"pid\":null}}}"
    ));
    assert!(refuse(
        "{\"question\":{\"answer\":true,\"priority\":\"eventually\",\"own\":null,\"fell_back\":false}}"
    ));
    assert!(refuse("{\"cwd\":\"@nowhere/x\"}"));
    assert!(
        refuse("{\"worker_agent\":\"vim\"}"),
        "an agent it does not know"
    );
    assert!(
        !refuse("{\"worker_agent\":\"muse\"}"),
        "the control: one it does"
    );
}

/// THE CONTEXT IS THE FACTS' AND THE POLICY'S: what the loop learned lands
/// where the policy reads it — the cwd known, the footer's bypass, the grant
/// row's rule withheld at a mount the reset was passed over for, the
/// question's answer by its priority — and with no facts the context is the
/// policy's own (the cwd unknown, the switches as the owner set them).
#[test]
fn the_context_is_built_from_the_facts_and_the_policy() {
    let config = SupervisorConfig {
        limit_reset: true,
        answer_questions: false,
        ..SupervisorConfig::default()
    };
    let env = ApprovalEnv {
        home: Some(PathBuf::from("/Users//owner")),
        uid: 501,
        tmpdir: None,
        worker: WorkerSource::Session,
    };
    let ctx = ApprovalCtx::from(
        &config,
        &env,
        &[],
        &BoxFacts::default(),
        Arc::new(LiveProbe),
    );
    assert!(!ctx.cwd_known);
    assert_eq!(ctx.trusted_cwd, None);
    assert_eq!(ctx.worker_agent, None, "no spawn's worker");
    assert!(ctx.limit_reset);
    assert!(!ctx.answer_questions);
    // `[harness] usage_credits` reaches the context as written, off by
    // default and on where the policy says so; no fact moves it.
    assert!(!ctx.usage_credits, "the subscription by default");
    let credits = SupervisorConfig {
        usage_credits: true,
        ..config.clone()
    };
    for facts in [BoxFacts::default(), every_fact()] {
        let on = ApprovalCtx::from(&credits, &env, &[], &facts, Arc::new(LiveProbe));
        assert!(on.usage_credits, "{facts:?}");
    }
    let facts = every_fact();
    let ctx = ApprovalCtx::from(
        &config,
        &env,
        &["x*.py".to_string()],
        &facts,
        Arc::new(LiveProbe),
    );
    assert!(ctx.cwd_known);
    assert_eq!(ctx.cwd, PathBuf::from("/Users//owner/src/app"));
    assert_eq!(ctx.trusted_cwd, facts.trusted_cwd, "the starter's folder");
    assert!(ctx.bypass_mode);
    assert!(!ctx.limit_reset, "passed over at this mount");
    assert!(!ctx.usage_credits, "no fact turns it on");
    assert!(ctx.answer_questions, "the session's word answers");
    assert_eq!(ctx.question_priority, QuestionPriority::LongTerm);
    assert_eq!(ctx.question_own, Some(QuestionPriority::ShortTerm));
    assert!(ctx.question_fell_back);
    assert_eq!(ctx.python_allow, vec!["x*.py".to_string()]);
    assert_eq!(ctx.box_call.as_deref(), Some("Bash"));
    assert_eq!(ctx.box_command, facts.box_command);
    assert_eq!(ctx.shell_cwds, facts.shell_cwds);
    assert_eq!(ctx.worker_agent, Some(aterm_phase::Program::Gemini));
}
