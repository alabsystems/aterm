// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm ctl <verb> @<sid> [args…]`: a read's session named after its verb.
//!
//! The protocol names a session in front of the verb (`@s-ab12 text`). People and agents also
//! type it after (`aterm ctl text @s-ab12`, `aterm ctl status s-ab12` with the sid as `aterm ctl
//! ls` prints it), and `read` for `text`. [`rewrite`] turns those into the protocol's one form
//! before anything else reads the request, so `--host`, the empty-selector refusal, the
//! multiplexer boundary, `@self`'s expansion and the socket a sid resolves to all read the
//! selector where they look for it.
//!
//! Which verbs take it is the protocol table's: a verb whose request resolves
//! ([`control_verbs::spec_for`]) to a read the server answers at once, aimed at a session or at
//! the instance hosting one. A wait (`await`, `ready`, `wait`, `sys`, `video`) keeps its
//! selector in front, since its words name what it waits for (`await task @a,@b`); `subscribe`
//! already takes its targets after the verb; a meta verb (`who`, `hold <sid>`, `raise <sid>`,
//! `survey`) takes no session selector at all, and a write (`send`, `turn`, `meta set`) keeps
//! the one spelling the protocol defines. `close` is the one write that moves: the session is
//! all it can name, so `close s-ab12` means nothing but `@s-ab12 close`.

use aterm_types::control_request::{Selector, Sid};
use aterm_types::control_verbs::{self, Framing, Lane, OpClass, Target};

/// The word this client reads as `text` beside a session selector, on either side of it.
const READ: &str = "read";

/// The verb [`READ`] stands for.
const READ_AS: &str = "text";

/// The verbs whose words after the verb are free text the server reads whole: `search`'s
/// pattern runs up to its trailing `case`/`regex`, so a bare `s-ab12` or an `@foo` there is a
/// word to find. Only an `@` that names a session is a selector after them.
const TEXT_AFTER_VERB: [&str; 1] = ["search"];

/// The one write whose session may follow its verb ([the module](self)).
const CLOSE: &str = "close";

/// `parts` with a read's session moved in front of its verb (and [`READ`] read as `text`), or
/// `None` when it names no session after a read's verb.
pub(crate) fn rewrite(parts: &[String]) -> Option<Vec<String>> {
    let (selector, verb, rest) = match parts {
        [selector, verb, rest @ ..] if selector.starts_with('@') && verb == READ => {
            (selector.clone(), READ_AS, rest)
        }
        [verb, word] if verb == CLOSE => (session_of_close(word)?, CLOSE, &[][..]),
        [verb, word, rest @ ..] => {
            let verb = if verb == READ { READ_AS } else { verb.as_str() };
            (session_after(verb, word, rest)?, verb, rest)
        }
        _ => return None,
    };
    let mut rewritten = Vec::with_capacity(parts.len());
    rewritten.push(selector);
    rewritten.push(verb.to_owned());
    rewritten.extend_from_slice(rest);
    Some(rewritten)
}

/// The selector `word`, standing right after `verb` and before `rest`, names: an `@` word as
/// typed (an empty or malformed one included, so the client's own refusal answers it), or a bare
/// session id with its `@`. `None` when `verb` with `rest` is no read answered at once, or
/// `word` names no session there.
fn session_after(verb: &str, word: &str, rest: &[String]) -> Option<String> {
    if !reads_at_once(verb, rest) {
        return None;
    }
    let text_after = TEXT_AFTER_VERB.contains(&verb);
    match word.strip_prefix('@') {
        Some(body) if !text_after || names_a_session(body) => Some(word.to_owned()),
        None if !text_after && Sid::parse(word).is_some() => Some(format!("@{word}")),
        _ => None,
    }
}

/// The selector `close`'s one word names: an `@` word as typed (the client's own refusal answers
/// an empty or malformed one), or a bare session id with its `@`.
fn session_of_close(word: &str) -> Option<String> {
    if word.starts_with('@') {
        Some(word.to_owned())
    } else {
        Sid::parse(word).map(|sid| format!("@{}", sid.as_str()))
    }
}

/// Whether an `@`'s `body` names one session: `@self` or `@env` (expanded by this client),
/// `@.`, a local number or a session id.
fn names_a_session(body: &str) -> bool {
    matches!(body, "self" | "env") || !matches!(Selector::parse(body), Selector::Invalid(_))
}

/// Whether `verb` followed by `rest` resolves to a read the server answers at once, aimed at a
/// session or the instance hosting one ([the module](self)).
fn reads_at_once(verb: &str, rest: &[String]) -> bool {
    control_verbs::spec_for(verb, &rest.join(" ")).is_some_and(|form| {
        form.op == OpClass::Read
            && form.lane == Lane::Rpc
            && form.framing != Framing::Push
            && form.row.target != Target::Meta
    })
}

#[cfg(test)]
#[path = "verb_first_tests.rs"]
mod tests;
