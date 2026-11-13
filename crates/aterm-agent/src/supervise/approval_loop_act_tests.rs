// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::acts::{Act, TaskFace};
use crate::supervise::approvals::Outcome;
use crate::supervise::policy::question::RULE_ANSWER_RECOMMENDED;
use crate::supervise::policy::turn_end::{
    RULE_ANSWER, RULE_COMPACT, RULE_CONTINUE, RULE_LAND, RULE_LAND_NUDGE, RULE_LOGIN,
};
use std::sync::Mutex;

struct Silent;

impl Ctl for Silent {
    fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
        Err(format!("no server for {args:?}"))
    }
}

#[derive(Debug, Default)]
struct Recorder {
    acts: Mutex<Vec<Act>>,
}

impl IdleHost for Recorder {
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn act_taken(&self, act: crate::harness::acts::TakenAct) {
        self.acts.lock().unwrap().push(act.act);
    }
    fn task_face(&self, face: TaskFace) {
        let _ = face;
    }
}

const ROWS: [(&str, Outcome, &str); 15] = [
    ("allow-once@v1", Outcome::Approved, "cargo test"),
    (RULE_ANSWER_RECOMMENDED, Outcome::Approved, ""),
    (RULE_ANSWER_RECOMMENDED, Outcome::Approved, ""),
    (RULE_ANSWER_RECOMMENDED, Outcome::Approved, ""),
    (RULE_ANSWER, Outcome::Typed, "Decide for yourself."),
    (RULE_CONTINUE, Outcome::Typed, "keep going"),
    (RULE_COMPACT, Outcome::Typed, "/compact"),
    (RULE_COMPACT, Outcome::Typed, "Carry on where you left off."),
    (RULE_LAND, Outcome::Typed, "[aterm harness] land"),
    (
        RULE_LAND_NUDGE,
        Outcome::Typed,
        "[aterm harness] still landing",
    ),
    ("decline@v1", Outcome::Declined, "rm -rf /"),
    (RULE_LOGIN, Outcome::Typed, "/login"),
    ("read-only@classify-v13", Outcome::Skipped, "ls"),
    ("-", Outcome::Escalated, ""),
    ("wall-wait@v1", Outcome::Waiting, ""),
];

fn write_rows(dir: &std::path::Path, host: Option<Arc<dyn IdleHost>>) -> Vec<Option<Act>> {
    let path = dir.join("s-1.jsonl");
    let mut ctl = Silent;
    let mut s = Session::new(&mut ctl, Some("s-1".to_string()));
    s.set_approval_ledger(Some(path.clone()));
    s.ledger = approvals::Ledger::open(Some(&path), Some("s-1"), &mut std::io::sink());
    s.stall_host = host;
    for (i, (rule, outcome, command)) in ROWS.into_iter().enumerate() {
        s.ledger_row(rule, outcome, command, "", i as u64);
    }
    let body = std::fs::read_to_string(&path).expect("the ledger");
    body.lines()
        .map(|line| {
            let v: aterm_json::Value = aterm_json::from_str(line).expect("a row");
            let text = |k: &str| {
                v.get(k)
                    .and_then(aterm_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            let outcome = Outcome::from_word(&text("decision")).expect("its decision");
            crate::harness::acts::sort(&text("rule_id"), outcome, &text("command")).act
        })
        .collect()
}

#[test]
fn the_ledgers_writer_tells_its_host_one_act_per_act_row() {
    let dir = crate::supervise::ScratchDir::new("acts", "told");
    let host = Arc::new(Recorder::default());
    let read = write_rows(&dir, Some(Arc::clone(&host) as Arc<dyn IdleHost>));
    assert_eq!(read.len(), ROWS.len(), "every row written");
    let told = host.acts.lock().unwrap().clone();
    let acts: Vec<Act> = read.into_iter().flatten().collect();
    assert_eq!(told, acts);
    assert_eq!(
        told,
        [
            Act::Approved,
            Act::Answered,
            Act::Answered,
            Act::Answered,
            Act::Answered,
            Act::Continued,
            Act::Resumed,
            Act::Landing,
            Act::Nudged,
            Act::Declined,
        ]
    );
    let bare = crate::supervise::ScratchDir::new("acts", "bare");
    assert_eq!(write_rows(&bare, None).len(), ROWS.len());
    assert_eq!(host.acts.lock().unwrap().len(), told.len());
}
