// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm file` (docs/DESIGN-shared-files-2026-11-11.md §3): the read verbs and `outbox` are
//! `aterm_file::cli`'s, which cannot open custody because their crate cannot reach this one;
//! `refresh` is here, beside the materializer it runs, and the write verbs (`set`, `add`, `rm`,
//! `revert`, `resolve`) beside the publisher they give two seconds ([`super::write`]).

use super::{
    materialize::{self, Outcome, Pass},
    publish, write,
};
use aterm_file::{Name, Recorded, Root, cli, exit};
use aterm_json::{Map, Value};
use aterm_secrets::client::Paths;
use std::{ffi::OsString, io::Write as _, path::Path, process::ExitCode};

const VERB: &str = "aterm file";

/// The most bytes `set … --stdin` reads: more than any cell of a shared file holds.
const STDIN_BYTES: u64 = 1024 * 1024;

/// What `refresh` prints for one file: its name, the state the pass left, and what refused.
fn line(outcome: &Outcome) -> String {
    let mut line = format!("{}: {}", outcome.name, outcome.word.as_str());
    if let Some(why) = outcome.why {
        line.push_str(&format!(" ({})", why.as_str()));
    }
    match outcome.recorded {
        Some(Recorded::Recorded { replaced: true }) => line.push_str(" · snapshot written"),
        Some(Recorded::Recorded { replaced: false } | Recorded::Kept(_)) => {
            line.push_str(" · snapshot kept");
        }
        Some(Recorded::Raced) => line.push_str(" · another pass recorded first"),
        None => {}
    }
    if let Some(detail) = &outcome.detail {
        line.push_str(&format!(" · {detail}"));
    }
    if let Some(problem) = &outcome.problem {
        line.push_str(&format!(" · not recorded: {problem}"));
    }
    line
}

fn json(outcome: &Outcome) -> Value {
    let mut object = Map::new();
    object.insert("file".to_owned(), Value::from(outcome.name.to_string()));
    object.insert("state".to_owned(), Value::from(outcome.word.as_str()));
    object.insert(
        "why".to_owned(),
        Value::from(outcome.why.map(aterm_file::Why::as_str)),
    );
    object.insert(
        "recorded".to_owned(),
        Value::from(match outcome.recorded {
            Some(Recorded::Recorded { replaced: true }) => Some("written"),
            Some(Recorded::Recorded { replaced: false }) => Some("kept"),
            Some(Recorded::Kept(kept)) => Some(kept.as_str()),
            Some(Recorded::Raced) => Some("raced"),
            None => None,
        }),
    );
    object.insert("detail".to_owned(), Value::from(outcome.detail.as_deref()));
    object.insert(
        "problem".to_owned(),
        Value::from(outcome.problem.as_deref()),
    );
    Value::Object(object)
}

/// `refresh [<name>] [--json]`: one materializer pass over `paths`, and what to print. No state
/// word is an error; only a pass that could not record on this Mac's own disk exits
/// [`exit::PROBLEMS`].
///
/// # Errors
/// The words name no file of this build: the usage line, for exit [`exit::USAGE`].
pub fn refresh(
    paths: &Paths,
    config: &Path,
    words: &[String],
    pass: &Pass,
) -> Result<(String, u8), String> {
    let mut json_out = false;
    let mut only = None;
    for word in words {
        match word.as_str() {
            "--json" => json_out = true,
            name if only.is_none() && !name.starts_with('-') => {
                let name = Name::parse(name).map_err(|error| error.to_string())?;
                if super::find(&name).is_none() {
                    return Err(format!("{name}: no such shared file in this build"));
                }
                only = Some(name);
            }
            _ => return Err("refresh [<name>] [--json]".to_owned()),
        }
    }
    let outcomes = materialize::pass(paths, config, only.as_ref(), pass);
    let code = if outcomes.iter().any(|outcome| outcome.problem.is_some()) {
        exit::PROBLEMS
    } else {
        exit::OK
    };
    let text = if json_out {
        let mut object = Map::new();
        object.insert(
            "files".to_owned(),
            Value::Array(outcomes.iter().map(json).collect()),
        );
        format!("{}\n", Value::Object(object))
    } else {
        outcomes
            .iter()
            .map(|outcome| format!("{}\n", line(outcome)))
            .collect()
    };
    Ok((text, code))
}

fn usage(why: &str) -> ExitCode {
    let _ = writeln!(std::io::stderr(), "{VERB}: {why}");
    ExitCode::from(exit::USAGE)
}

/// `aterm file …` from the command line.
pub fn main_entry(args: Vec<OsString>) -> ExitCode {
    let Ok(words) = args
        .into_iter()
        .map(OsString::into_string)
        .collect::<Result<Vec<_>, _>>()
    else {
        return usage("arguments must be UTF-8");
    };
    if words.first().is_some_and(|word| word == "refresh") {
        let Ok(paths) = Paths::live() else {
            return usage("no home directory");
        };
        let Some(config) = aterm_types::dirs::aterm_config_path() else {
            return usage("no configuration folder");
        };
        return match refresh(&paths, &config, &words[1..], &Pass::now(true)) {
            Ok((text, code)) => aterm_uds::stdout::answer_exit(VERB, &text, code),
            Err(why) => usage(&why),
        };
    }
    if words.first().is_some_and(|word| write::is_verb(word)) {
        let Ok(paths) = Paths::live() else {
            return usage("no home directory");
        };
        let Some(config) = aterm_types::dirs::aterm_config_path() else {
            return usage("no configuration folder");
        };
        // Who says they ran it: `--by <word>`, else a person at a terminal, else a program.
        let mut by = None;
        let mut json = false;
        let mut rest = Vec::with_capacity(words.len());
        let mut iter = words.into_iter();
        while let Some(word) = iter.next() {
            if word == "--by" {
                let Some(said) = iter.next() else {
                    return usage("--by takes one word");
                };
                by = Some(said);
            } else if word == "--json" {
                json = true;
            } else {
                rest.push(word);
            }
        }
        // `set … --stdin` writes what standard input holds, read whole within a cell's bound.
        let stdin = if rest.first().is_some_and(|word| word == "set")
            && rest.get(4).is_some_and(|word| word == "--stdin")
        {
            use std::io::Read as _;
            let mut bytes = Vec::new();
            if let Err(error) = std::io::stdin()
                .lock()
                .take(STDIN_BYTES + 1)
                .read_to_end(&mut bytes)
            {
                return usage(&format!("--stdin: {error}"));
            }
            if bytes.len() as u64 > STDIN_BYTES {
                return usage("--stdin: standard input is larger than a cell holds");
            }
            match String::from_utf8(bytes) {
                Ok(text) => Some(text),
                Err(_) => return usage("--stdin: standard input is not UTF-8"),
            }
        } else {
            None
        };
        let by = by.unwrap_or_else(|| {
            use std::io::IsTerminal as _;
            if std::io::stdin().is_terminal() {
                "person".to_owned()
            } else {
                "program".to_owned()
            }
        });
        let session = std::env::var("ATERM_SESSION_ID").ok();
        let env = write::Env {
            by: &by,
            session: session.as_deref().filter(|session| !session.is_empty()),
            now_ms: aterm_time::unix_millis().unwrap_or(0),
            attempt: publish::INLINE,
            json,
            stdin: stdin.as_deref(),
        };
        return match write::verb(&paths, &config, &rest, &env) {
            Ok((text, code)) => aterm_uds::stdout::answer_exit(VERB, &text, code),
            Err(write::Refusal::Usage(why)) => usage(&why),
            Err(write::Refusal::Problem(why)) => {
                let _ = writeln!(std::io::stderr(), "{VERB}: {why}");
                ExitCode::from(exit::PROBLEMS)
            }
        };
    }
    let Some(root) = Root::live() else {
        return usage("no home directory");
    };
    match cli::run(&root, &super::types::known(), &words) {
        cli::Run::Answer(answer) => {
            if !answer.err.is_empty() {
                let _ = std::io::stderr().write_all(answer.err.as_bytes());
            }
            aterm_uds::stdout::answer_exit(VERB, &answer.out, answer.code)
        }
        cli::Run::Watch(file, watch) => {
            let mut emit =
                |line: &str| aterm_uds::stdout::write_all(format!("{line}\n").as_bytes()).is_ok();
            ExitCode::from(cli::watch(&file, watch, &mut emit, &mut |every| {
                std::thread::sleep(every);
            }))
        }
    }
}
