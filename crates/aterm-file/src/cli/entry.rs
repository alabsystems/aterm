// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Command-line output and the entry point for systems that read previously recorded files.

use super::{Answer, Run};
use crate::{Root, exit};
use std::{ffi::OsString, io::Write as _, process::ExitCode};

const USAGE: &str = "\
usage: aterm file <verb> [--json]
  ls | status [<name>] | schema <name> | history <name> | outbox [<name>]
  cat <name> [--published] | export <name> | path <name> [--local]
  get <name> <row> [<column>]
  rows <name> | search <name> <text>  [--where col=val]... [--join <ref column>]
  watch <name> [--timeout <seconds>] [--changes <n>]
Reads files already recorded under the user's .aterm/files directory.\n";

/// Read the files actually recorded here on a system without a Commons writer.
pub fn retained_main(args: Vec<OsString>) -> ExitCode {
    let Ok(input) = args
        .into_iter()
        .map(OsString::into_string)
        .collect::<Result<Vec<_>, _>>()
    else {
        return emit(Run::Answer(Answer::usage("arguments must be UTF-8")));
    };
    let Some(root) = Root::live() else {
        return emit(Run::Answer(Answer::usage("no home directory")));
    };
    emit(retained(&root, &input))
}

pub(crate) fn retained(root: &Root, input: &[String]) -> Run {
    let parsed = super::words(input);
    match parsed.rest.as_slice() {
        [] | ["help" | "--help" | "-h", ..] => Run::Answer(Answer {
            out: USAGE.to_owned(),
            ..Answer::default()
        }),
        [
            "ls" | "status" | "schema" | "history" | "cat" | "export" | "path" | "get" | "rows"
            | "search" | "watch",
            ..,
        ]
        | ["outbox"]
        | ["outbox", _] => super::run_words(root, &[], parsed),
        [verb, ..] => Run::Answer(Answer {
            err: format!("aterm file: `{verb}` is unavailable in the offline reader\n{USAGE}"),
            code: exit::USAGE,
            ..Answer::default()
        }),
    }
}

/// Print an immediate answer, or stream a watch to the command's stdout until it ends.
pub fn emit(run: Run) -> ExitCode {
    match run {
        Run::Answer(answer) => {
            if !answer.err.is_empty() {
                let _ = std::io::stderr().write_all(answer.err.as_bytes());
            }
            aterm_uds::stdout::answer_exit("aterm file", &answer.out, answer.code)
        }
        Run::Watch(file, watch) => {
            let mut emit =
                |line: &str| aterm_uds::stdout::write_all(format!("{line}\n").as_bytes()).is_ok();
            ExitCode::from(super::watch(
                &file,
                watch,
                &mut emit,
                &mut std::thread::sleep,
            ))
        }
    }
}
