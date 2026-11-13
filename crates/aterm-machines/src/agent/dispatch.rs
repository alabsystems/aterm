// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `agent ssh`: the fleet key's forced command.

use super::Layout;
use crate::port::{Cmd, System};
use std::io::{Read, Write};
use std::process::ExitCode;

/// Build the `sudo` command for an `apply` request.
#[must_use]
pub fn sudo_apply() -> Cmd {
    Cmd::new("/usr/bin/sudo").args([
        "-n",
        crate::protocol::AGENT_EXE,
        "machines",
        "agent",
        "apply",
    ])
}

/// What the client asked for, split, or why it is refused.
/// # Errors
/// The refusal's one line.
pub fn parse(original: Option<&str>) -> Result<Vec<&str>, String> {
    let Some(original) = original else {
        return Err(
            "this key runs only `hello`, `put <sha256>`, `apply`, `attach <run>` and `screen`; \
             it opens no shell"
                .into(),
        );
    };
    if original.bytes().any(|b| b.is_ascii_control()) {
        return Err("the request holds a control character".into());
    }
    let words: Vec<&str> = original.split(' ').collect();
    if words.iter().any(|w| w.is_empty()) {
        return Err(format!(
            "{original:?}: words are separated by exactly one space"
        ));
    }
    Ok(words)
}

/// `agent ssh` with its input explicit.
pub fn ssh(
    sys: &dyn System,
    layout: &Layout,
    original: Option<&str>,
    stdin: &mut dyn Read,
    out: &mut dyn Write,
) -> ExitCode {
    let words = match parse(original) {
        Ok(w) => w,
        Err(why) => return super::refuse(out, &why),
    };
    match words.as_slice() {
        ["hello"] => super::hello::print(sys, layout, out),
        ["put", sha] => super::inbox::put(sys, sha, stdin, out),
        ["attach", run] => super::apply::attach(sys, layout, run, out),
        ["screen"] => match super::relay::own_stdin() {
            Ok(input) => super::relay::screen(sys, super::relay::SCREEN_SHARING_PORT, input, out),
            Err(e) => super::refuse(out, &format!("screen: standard input: {e}")),
        },
        ["apply"] => {
            let _ = out.flush();
            match sys.run_tty(&sudo_apply(), &mut |_| Ok(())) {
                Ok(Some(code)) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
                Ok(None) => ExitCode::FAILURE,
                Err(e) => super::refuse(out, &format!("apply: sudo did not start: {e}")),
            }
        }
        _ => super::refuse(
            out,
            &format!(
                "{:?} is not a request this key may make (hello, put <sha256>, apply, attach \
                 <run>, screen)",
                original.unwrap_or_default()
            ),
        ),
    }
}

#[cfg(test)]
#[path = "dispatch_tests.rs"]
mod tests;
