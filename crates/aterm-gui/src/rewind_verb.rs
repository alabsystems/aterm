// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Control-thread parser and main-thread dispatch for the `rewind` verb.

use aterm_buffer::Ticks;

use crate::App;
use crate::rewind::{Reply, Target};

/// One parsed `rewind` request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Asked {
    Status,
    Open(Target),
    Step(i64),
    Text { trim: bool },
    Close,
}

const USAGE: &str =
    "usage: rewind [status] | open [ago=<secs>|at=<tick>] | step <+-n> | text [trim] | close";

/// Parse the words after `rewind`.
pub(crate) fn parse(rest: &str) -> Result<Asked, String> {
    let words: Vec<&str> = rest.split_whitespace().collect();
    let usage = || USAGE.to_string();
    match words.as_slice() {
        [] | ["status"] => Ok(Asked::Status),
        ["open"] => Ok(Asked::Open(Target::Latest)),
        ["open", arg] => {
            let (key, value) = arg.split_once('=').ok_or_else(usage)?;
            let n = value.parse::<u64>().map_err(|_| usage())?;
            match key {
                "ago" => Ok(Asked::Open(Target::Ago(n))),
                "at" => Ok(Asked::Open(Target::At(Ticks(n)))),
                _ => Err(usage()),
            }
        }
        ["step", n] => n
            .strip_prefix('+')
            .unwrap_or(n)
            .parse::<i64>()
            .map(Asked::Step)
            .map_err(|_| usage()),
        ["text"] => Ok(Asked::Text { trim: false }),
        ["text", "trim"] => Ok(Asked::Text { trim: true }),
        ["close"] => Ok(Asked::Close),
        _ => Err(usage()),
    }
}

/// `rewind text`'s reply: `OK <nbytes>[ trimmed=<k>]` and the shown screen, as `temporal` frames it.
pub(crate) fn text_reply(term: &aterm_core::terminal::Terminal, trim: bool) -> String {
    let mut body = crate::control::screen_text(term);
    if !trim {
        return format!("OK {}\n{body}", body.len());
    }
    let rows = usize::from(term.rows());
    let sent = crate::control::trimmed_len(body.lines());
    let keep: usize = body.split_inclusive('\n').take(sent).map(str::len).sum();
    body.truncate(keep);
    format!("OK {} trimmed={}\n{body}", body.len(), rows - sent)
}

/// The verb's control-thread half: the ask on the main thread (`Wake::Rewind`).
pub(crate) fn cmd(
    proxy: &aterm_window::event_loop::EventLoopProxy<crate::Wake>,
    session: u64,
    rest: &str,
) -> String {
    let asked = match parse(rest) {
        Ok(asked) => asked,
        Err(why) => return format!("ERR {why}\n"),
    };
    let framed = matches!(asked, Asked::Text { .. });
    match crate::control::control_media::call_main(proxy, |reply| crate::Wake::Rewind {
        session,
        asked,
        reply,
    }) {
        Ok(Ok(reply)) if framed => reply,
        Ok(Ok(line)) => format!("OK {line}\n"),
        Ok(Err(why)) => format!("ERR rewind: {why}\n"),
        Err(e) => format!("ERR {e}\n"),
    }
}

impl App {
    /// The `Wake::Rewind` handler: `asked` of `session`, answered on `reply` (an open or a step once its fold is shown).
    pub(crate) fn control_rewind(&mut self, session: u64, asked: Asked, reply: Reply) {
        let done = match asked {
            Asked::Status => Some(self.rewind_status(session)),
            Asked::Open(target) => self
                .rewind_open(session, target, Some(reply.clone()))
                .err()
                .map(Err),
            Asked::Step(delta) => self
                .rewind_step(session, delta, Some(reply.clone()))
                .err()
                .map(Err),
            Asked::Text { trim } => Some(
                self.rewind_shown(session)
                    .map(|term| text_reply(&crate::term_lock(&term), trim)),
            ),
            Asked::Close => {
                self.rewind_close(session);
                Some(self.rewind_status(session))
            }
        };
        if let Some(done) = done {
            let _ = reply.send(done);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rewind_verb_parses_its_forms_and_refuses_the_rest() {
        for (rest, want) in [
            ("", Asked::Status),
            (" status ", Asked::Status),
            ("open", Asked::Open(Target::Latest)),
            ("open ago=42", Asked::Open(Target::Ago(42))),
            ("open at=1200", Asked::Open(Target::At(Ticks(1200)))),
            ("step -1", Asked::Step(-1)),
            ("step +3", Asked::Step(3)),
            ("step 2", Asked::Step(2)),
            ("text", Asked::Text { trim: false }),
            ("text trim", Asked::Text { trim: true }),
            ("close", Asked::Close),
        ] {
            assert_eq!(parse(rest), Ok(want), "{rest:?}");
        }
        for rest in [
            "open ago=",
            "open ago=-1",
            "open when=3",
            "open ago=1 at=2",
            "step",
            "step x",
            "text all",
            "close now",
            "rewind",
            "status extra",
        ] {
            assert_eq!(parse(rest), Err(USAGE.to_string()), "{rest:?}");
        }
    }
}
