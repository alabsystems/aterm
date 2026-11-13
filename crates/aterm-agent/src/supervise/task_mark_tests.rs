// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A server that answers `task` with `record` (`None`: `OK task -`), the
/// tab's `blocks 2 --json` with `blocks` (`None`: refused), and every
/// mark `OK task … changed=1`, keeping the requests.
struct Tab {
    record: Option<&'static str>,
    blocks: Option<&'static str>,
    asked: Vec<String>,
}

impl Ctl for Tab {
    fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
        let line = args.join(" ");
        self.asked.push(line.clone());
        let ok = |stdout: String| CtlReply {
            code: 0,
            stdout,
            stderr: String::new(),
        };
        let words: Vec<&str> = args.iter().copied().skip(1).collect();
        Ok(match words.as_slice() {
            ["task"] => ok(self.record.map_or_else(
                || "OK task -\n".to_string(),
                |r| format!("OK task {r} agent=- attention=-\n"),
            )),
            ["task", "mark", ..] => ok("OK task rev=9 state=exited changed=1\n".to_string()),
            ["blocks", "2", "--json"] => match self.blocks {
                Some(b) => ok(format!("{b}\n")),
                None => CtlReply {
                    code: 1,
                    stdout: String::new(),
                    stderr: "aterm-ctl: ERR no blocks\n".to_string(),
                },
            },
            _ => return Err(format!("unscripted: {line}")),
        })
    }
}

/// The shell back at its prompt after the agent's block ended with 130.
const BACK: &str = r#"{"blocks":[{"id":3,"state":"complete","prompt":10,"end":20,"exit":130},{"id":4,"state":"entering","prompt":20}]}"#;

fn marks(tab: &Tab) -> Vec<&str> {
    tab.asked
        .iter()
        .map(String::as_str)
        .filter(|r| r.contains(" task mark "))
        .collect()
}

/// THE AGENT GONE FOR GOOD MARKS ITS TASK `exited`: with the code its
/// shell's block ended with, keeping a done task's result.
#[test]
fn an_agent_gone_for_good_marks_its_task_exited_with_its_code() {
    let run = |record, blocks| {
        let mut tab = Tab {
            record,
            blocks,
            asked: Vec::new(),
        };
        let said = mark_task_exited(&mut tab, "@s-1").expect("served");
        (tab, said)
    };
    let (tab, said) = run(
        Some("rev=1 state=working from=ctl since_ms=5 result_bytes=0"),
        Some(BACK),
    );
    assert_eq!(marks(&tab), ["@s-1 task mark exited why=exit exit=130"]);
    assert_eq!(
        said.as_deref(),
        Some("OK task rev=9 state=exited changed=1")
    );
    let (tab, _) = run(
        Some(
            "rev=2 state=done from=ctl since_ms=5 result_bytes=17 why=marker result_path=/s/tasks/s-1-1.result.md",
        ),
        Some(BACK),
    );
    assert_eq!(
        marks(&tab),
        [
            "@s-1 task mark exited why=exit exit=130 result_bytes=17 result_path=/s/tasks/s-1-1.result.md"
        ]
    );
    let (tab, _) = run(
        Some("rev=1 state=working from=ctl since_ms=5 result_bytes=0"),
        Some(r#"{"blocks":[{"id":3,"state":"executing","prompt":10}]}"#),
    );
    assert_eq!(marks(&tab), ["@s-1 task mark exited why=exit"]);
    let (tab, _) = run(
        Some("rev=1 state=stalled from=ctl since_ms=5 result_bytes=0"),
        None,
    );
    assert_eq!(marks(&tab), ["@s-1 task mark exited why=exit"]);
    for record in [
        None,
        Some("rev=3 state=exited from=ctl since_ms=5 result_bytes=0 why=exit"),
    ] {
        let (tab, said) = run(record, Some(BACK));
        assert_eq!(said, None);
        assert_eq!(tab.asked, ["@s-1 task"], "{record:?}");
    }
}

/// A FAILED CONCLUSION'S RESULT OUTLIVES THE AGENT'S EXIT, as a done task's does; the record
/// keeps what `why=` said (`TaskMark::apply`).
#[test]
fn an_agent_gone_after_it_failed_keeps_its_result() {
    let mut tab = Tab {
        record: Some(
            "rev=2 state=failed from=ctl since_ms=5 result_bytes=17 why=could-not-finish result_path=/s/tasks/s-1-1.result.md",
        ),
        blocks: None,
        asked: Vec::new(),
    };
    mark_task_exited(&mut tab, "@s-1").expect("served");
    assert_eq!(
        marks(&tab),
        ["@s-1 task mark exited why=exit result_bytes=17 result_path=/s/tasks/s-1-1.result.md"]
    );
}

/// The record is read by key off the `task` reply and the mark's own;
/// `OK task -`, a refusal and a server before the verb are no record.
#[test]
fn a_task_reply_is_read_by_key() {
    let reply = |code: i32, stdout: &str| CtlReply {
        code,
        stdout: stdout.to_string(),
        stderr: String::new(),
    };
    assert_eq!(
        task_read(&reply(
            0,
            "OK task rev=4 state=done from=s-1a since_ms=9 result_bytes=12 why=marker result_path=/t/x.md agent=idle attention=- changed=1\n"
        )),
        Some(TaskRead {
            rev: 4,
            state: "done".to_string(),
            result_bytes: 12,
            result_path: Some("/t/x.md".to_string()),
            why: Some("marker".to_string()),
            fact: None,
        })
    );
    let noted = task_read(&reply(
        0,
        "OK task rev=4 state=working from=ctl since_ms=9 result_bytes=0 fact=hold-room@v1#2 \
         fact_rev=3 agent=idle attention=x%20fact=wake@v1#7 changed=0\n",
    ))
    .expect("a record");
    assert_eq!(
        noted.fact.map(crate::supervise::occupancy::FactWord::word),
        Some("hold-room@v1#2".to_string())
    );
    for (spelled, read) in [
        ("fact=-", None),
        ("fact=hold-room@v1#0", None),
        ("fact=hold-room#1", None),
        ("fact=hold-room@v1#01", None),
        ("fact=wait-nudge@v1#1", Some("wait-nudge@v1#1")),
        ("", None),
    ] {
        let line = format!("OK task rev=4 state=working from=ctl result_bytes=0 {spelled}\n");
        assert_eq!(
            task_read(&reply(0, &line))
                .expect("a record")
                .fact
                .map(crate::supervise::occupancy::FactWord::word)
                .as_deref(),
            read,
            "{spelled:?}"
        );
    }
    for none in [
        reply(0, "OK task -\n"),
        reply(1, ""),
        reply(0, "OK\n"),
        reply(0, "OK task state=done\n"),
    ] {
        assert_eq!(task_read(&none), None, "{none:?}");
    }
    assert_eq!(result::result_stem(Some("@s-0a1b"), 3), "s-0a1b-3");
    assert_eq!(result::result_stem(None, 1), "session-1");
    assert_eq!(result::result_stem(Some("@../x"), 1), "x-1");
}
