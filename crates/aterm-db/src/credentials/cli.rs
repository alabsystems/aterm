// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm secrets need` (docs/DESIGN-credential-requests-2026-10-27.md §13): raise, list and
//! retire this Mac's needs, and run a pass over the lab's table now. Every verb moves the need
//! ledger first, except `peek`, which reads existing state only. The table pass after a
//! changing verb is best effort, and its line says what stopped it.

use super::{Observation, Pass, Thread};
use aterm_codec::hex;
use aterm_json::{Map, Value};
use aterm_secrets::{
    client::Paths,
    need::{self, Need, Purpose, Want},
};
use std::ffi::OsString;
use std::process::ExitCode;

/// The verbs' usage.
pub const USAGE: &str = "usage:
  aterm secrets need [ls] [--all] [--json]        this Mac's needs; --all adds every request in the lab's table
  aterm secrets need peek [--all] [--json]        read retained state only; never posts, sweeps or advances needs
  aterm secrets need <want> [--purpose <id>]       raise a need by hand (want: site-deployer-key, notary, mailbox:<label>, gh:dev, gh:release, entry:<name>, ...)
  aterm secrets need retire <want|id> [--declined] [--note <text>]
                                                   withdraw a need, or decline one a person was asked for
  aterm secrets need pass                          run this Mac's pass over credentials/requests/1 now (and the custodian's, on the vault's admin Mac)
  aterm secrets need give <want|id>                a holder's give (not in this build: see below)
";

fn say(text: &str) -> ExitCode {
    aterm_uds::stdout::answer_exit("aterm secrets need", text, 0)
}

fn fail(text: &str, code: u8) -> ExitCode {
    use std::io::Write as _;
    let _ = writeln!(std::io::stderr(), "aterm secrets need: {text}");
    ExitCode::from(code)
}

/// Entry point for `aterm secrets need …` (the words after `need`).
#[must_use]
pub fn main_entry(args: Vec<OsString>) -> ExitCode {
    let Ok(words) = args
        .into_iter()
        .map(OsString::into_string)
        .collect::<Result<Vec<_>, _>>()
    else {
        return fail("arguments must be UTF-8", 2);
    };
    if words.iter().any(|w| w == "--help" || w == "-h") {
        return say(USAGE);
    }
    let words = if words.is_empty() {
        vec!["ls".to_owned()]
    } else {
        words
    };
    let paths = match Paths::live() {
        Ok(paths) => paths,
        Err(error) => return fail(&error.to_string(), 1),
    };
    match run(&paths, &words) {
        Ok(text) => say(&text),
        Err((text, code)) => fail(&text, code),
    }
}

fn option<'a>(words: &'a [String], flag: &str) -> Result<Option<&'a str>, (String, u8)> {
    match words.iter().position(|word| word == flag) {
        None => Ok(None),
        Some(at) => words
            .get(at + 1)
            .map(|value| Some(value.as_str()))
            .ok_or_else(|| (format!("{flag} needs a value\n{USAGE}"), 2)),
    }
}

fn known(words: &[String], flags: &[&str], valued: &[&str]) -> Result<(), (String, u8)> {
    let mut skip = false;
    for word in words {
        if skip {
            skip = false;
            continue;
        }
        if valued.contains(&word.as_str()) {
            skip = true;
        } else if word.starts_with("--") && !flags.contains(&word.as_str()) {
            return Err((format!("unknown option {word}\n{USAGE}"), 2));
        }
    }
    Ok(())
}

/// Run one verb for this Mac's `paths`: the text to print, or the refusal and its exit code.
///
/// # Errors
/// The words do not parse, or the ledger cannot be read or written.
pub fn run(paths: &Paths, words: &[String]) -> Result<String, (String, u8)> {
    let ledger = |error: aterm_secrets::Error| (error.to_string(), 1);
    match words[0].as_str() {
        "peek" => {
            known(&words[1..], &["--all", "--json"], &[])?;
            let needs = need::peek(paths).map_err(ledger)?;
            let table = words
                .iter()
                .any(|word| word == "--all")
                .then(|| super::observe(paths).map_err(|error| error.to_string()));
            if words.iter().any(|word| word == "--json") {
                let mut value = needs_json(&needs);
                if let Some(table) = &table {
                    value.insert("table".into(), observation_json(table));
                }
                return Ok(format!("{}\n", Value::Object(value)));
            }
            let mut out = listing(&needs, None);
            if let Some(table) = &table {
                match table {
                    Ok(observed) => {
                        out.push_str(&format!(
                            "lab table: retained here at {} ms; {} current members; {}/{} operations interpreted; {} inert records\n",
                            observed.at_ms,
                            observed.members.len(),
                            observed.interpreted_operations,
                            observed.retained_operations,
                            observed.book.inert.len()
                        ));
                        out.extend(observed.book.threads.values().map(thread_row));
                    }
                    Err(why) => out.push_str(&format!("lab table: unavailable ({why})\n")),
                }
            }
            Ok(out)
        }
        "ls" => {
            known(&words[1..], &["--all", "--json"], &[])?;
            let pass = table_pass(paths);
            let needs = need::list(paths).map_err(ledger)?;
            let all = words.iter().any(|word| word == "--all");
            if words.iter().any(|word| word == "--json") {
                return Ok(format!("{}\n", json(&needs, all.then_some(&pass))));
            }
            Ok(listing(&needs, all.then_some(&pass)))
        }
        "retire" => {
            known(&words[1..], &["--declined"], &["--note"])?;
            let Some(selector) = words.get(1).filter(|word| !word.starts_with("--")) else {
                return Err((format!("retire names a want or a request id\n{USAGE}"), 2));
            };
            let note = option(words, "--note")?.unwrap_or_default();
            let declined = words.iter().any(|word| word == "--declined");
            let retired = need::retire(paths, selector, declined, note).map_err(ledger)?;
            let pass = table_pass(paths);
            let mut out: String = retired.iter().map(row).collect();
            out.push_str(&pass_line(&pass));
            Ok(out)
        }
        "pass" => {
            known(&words[1..], &[], &[])?;
            let pass = table_pass(paths);
            let mut out = pass_line(&pass);
            match super::custodian_pass(paths) {
                Ok(None) => {}
                Ok(Some(custodian)) => out.push_str(&format!(
                    "custodian: {} published, {} handed to a person, {} retired, {} waiting\n",
                    custodian.published.len(),
                    custodian.asked.len(),
                    custodian.retired.len(),
                    custodian.waiting.len()
                )),
                Err(error) => out.push_str(&format!("custodian: {error}\n")),
            }
            Ok(out)
        }
        "give" => Err((
            "a holder's give (a value a person types here, sent to the custodian in a handoff) is the handoff step's, not in this build. On the vault's admin Mac, aterm secrets set puts the value in the vault, and that Mac's next pass copies it to the requester\n".to_owned(),
            2,
        )),
        word => {
            known(&words[1..], &[], &["--purpose"])?;
            let want = Want::parse(word).map_err(|why| (format!("{why}\n{USAGE}"), 2))?;
            let purpose = match option(words, "--purpose")? {
                None => Purpose::ByHand,
                Some(id) => Purpose::parse(id)
                    .ok_or_else(|| (format!("unknown purpose {id}\n{USAGE}"), 2))?,
            };
            need::raise(paths, want, purpose).map_err(ledger)?;
            let pass = table_pass(paths);
            let needs = need::list(paths).map_err(ledger)?;
            let mut out: String = needs
                .iter()
                .filter(|need| need.want.key() == word)
                .map(row)
                .collect();
            out.push_str(&pass_line(&pass));
            Ok(out)
        }
    }
}

fn table_pass(paths: &Paths) -> Result<Pass, String> {
    super::pass(paths).map_err(|error| error.to_string())
}

fn pass_line(pass: &Result<Pass, String>) -> String {
    match pass {
        Ok(pass) if !pass.writer => {
            "lab table: read only here (this Mac is not a current Commons Writer); its needs wait in the ledger\n".to_owned()
        }
        Ok(pass) => format!(
            "lab table: {} posted, {} answered, {} decided here, {} offered, {} swept\n",
            pass.posted, pass.answered, pass.decided, pass.offered, pass.swept
        ),
        Err(why) => format!("lab table: not reached ({why}); the ledger holds each need until it is\n"),
    }
}

fn row(need: &Need) -> String {
    format!(
        "{}  {}  {}  {}\n    {}\n",
        need.want.key(),
        need.state.word(),
        need.purpose.id(),
        hex::encode_lower(&need.id),
        need.line
    )
}

fn thread_row(thread: &Thread) -> String {
    let state = if let Some((retired, _)) = &thread.retired {
        format!("retired {}", retired.reason.word())
    } else if thread.fulfilled.is_some() {
        "fulfilled".to_owned()
    } else if let Some((vault, seq)) = &thread.published {
        format!("published {vault} seq {seq}")
    } else if thread.needs_person {
        "awaiting-person".to_owned()
    } else if thread.offers.is_empty() {
        format!("open, swept by {}", thread.swept.len())
    } else {
        format!("offered by {}", thread.offers.len())
    };
    format!(
        "{}  {}  {}  {}\n",
        thread.want.key(),
        state,
        thread.purpose.id(),
        hex::encode_lower(&thread.id)
    )
}

fn listing(needs: &[Need], all: Option<&Result<Pass, String>>) -> String {
    let mut out = if needs.is_empty() {
        "no needs on this Mac\n".to_owned()
    } else {
        needs.iter().map(row).collect()
    };
    for line in need::needs_you(needs) {
        out.push_str(&line);
        out.push('\n');
    }
    if let Some(pass) = all {
        out.push_str(&pass_line(pass));
        if let Ok(pass) = pass {
            out.extend(pass.threads.iter().map(thread_row));
            for (id, why) in &pass.inert {
                out.push_str(&format!("inert {}: {why}\n", hex::encode_lower(id)));
            }
        }
    }
    out
}

fn needs_json(needs: &[Need]) -> Map {
    let rows: Vec<Value> = needs
        .iter()
        .map(|need| {
            let mut row = Map::new();
            row.insert("id".into(), Value::from(hex::encode_lower(&need.id)));
            row.insert("want".into(), Value::from(need.want.key()));
            row.insert("purpose".into(), Value::from(need.purpose.id()));
            row.insert("state".into(), Value::from(need.state.word()));
            row.insert("waits".into(), Value::from(need.wait.word()));
            row.insert("line".into(), Value::from(need.line.clone()));
            row.insert(
                "reason".into(),
                need.retired
                    .as_ref()
                    .map_or(Value::Null, |retired| Value::from(retired.reason.word())),
            );
            Value::Object(row)
        })
        .collect();
    let mut top = Map::new();
    top.insert("needs".into(), Value::Array(rows));
    top
}

fn observation_json(table: &Result<Observation, String>) -> Value {
    let observed = match table {
        Ok(observed) => observed,
        Err(why) => return aterm_json::json!({"available": false, "reason": why}),
    };
    let requests: Vec<Value> = observed
        .book
        .threads
        .values()
        .map(|thread| {
            let required: Vec<_> = observed
                .members
                .iter()
                .filter(|member| **member != thread.requester)
                .collect();
            aterm_json::json!({
                "id": hex::encode_lower(&thread.id),
                "want": thread.want.key(),
                "purpose": thread.purpose.id(),
                "own": thread.requester == observed.observer,
                "requester_current": observed.members.contains(&thread.requester),
                "offers": thread.offers.len(),
                "swept_current": required.iter().filter(|member| thread.swept.contains(**member)).count(),
                "required_sweeps": required.len(),
                "sweeps_complete": thread.swept_by_all(&observed.members),
                "want_window_ended": thread.window_ended(observed.at_ms),
                "needs_person": thread.needs_person,
                "published_seq": thread.published.as_ref().map(|(_, seq)| *seq),
                "fulfilled_seq": thread.fulfilled,
                "retired_reason": thread.retired.as_ref().map(|(retired, _)| retired.reason.word()),
            })
        })
        .collect();
    aterm_json::json!({
        "available": true,
        "observed_at_ms": observed.at_ms,
        "current_members": observed.members.len(),
        "retained_operations": observed.retained_operations,
        "interpreted_operations": observed.interpreted_operations,
        "requests": requests,
        "inert_records": observed.book.inert.len(),
    })
}

fn json(needs: &[Need], all: Option<&Result<Pass, String>>) -> String {
    let mut top = needs_json(needs);
    if let Some(pass) = all {
        let table = match pass {
            Ok(pass) => Value::Array(
                pass.threads
                    .iter()
                    .map(|thread| Value::from(thread_row(thread).trim_end().to_owned()))
                    .collect(),
            ),
            Err(why) => Value::from(why.clone()),
        };
        top.insert("table".into(), table);
    }
    aterm_json::to_string(&Value::Object(top)).unwrap_or_default()
}
