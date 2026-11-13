// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A request's why (the design's §5): the requester's own `why=` where it gave one, else the
//! documented fallback, recorded as a fallback.

use crate::record::{Via, WhySrc};
use aterm_types::control_verbs::{REQUEST_WHY_BYTES, REQUEST_WHY_MAX};

/// What a request with no why reads.
pub const NONE_GIVEN: &str = "(none given)";

/// A request's why and where it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Why {
    /// The one line.
    pub text: String,
    /// Where it came from.
    pub src: WhySrc,
}

/// The why of a request that came in `via`: `given` (a spawn's `why=`, already held to
/// [`aterm_types::control_verbs::request_why`]) first; else, for a fleet worker, its `brief`'s
/// first line that is not blank; else, for a local spawn, the requesting tab's `goal` line, then
/// its `asked` line; else [`NONE_GIVEN`]. A fallback is cut to one visible line ([`line_of`]).
/// A plain fleet shell with no `why=` never gets here: the drive refuses it.
#[must_use]
pub fn why_of(
    via: Via,
    given: Option<&str>,
    brief: Option<&[u8]>,
    goal: Option<&str>,
    asked: Option<&str>,
) -> Why {
    if let Some(text) = given.and_then(line_of) {
        return Why {
            text,
            src: WhySrc::Given,
        };
    }
    let fallback = match via {
        Via::FleetWorker => brief
            .and_then(|brief| line_of(&String::from_utf8_lossy(brief)))
            .map(|text| (text, WhySrc::Brief)),
        Via::CtlSpawn => goal
            .and_then(line_of)
            .map(|text| (text, WhySrc::Goal))
            .or_else(|| asked.and_then(line_of).map(|text| (text, WhySrc::Asked))),
        Via::FleetShell => None,
    };
    let (text, src) = fallback.unwrap_or_else(|| (NONE_GIVEN.to_owned(), WhySrc::None));
    Why { text, src }
}

/// The first line of `text` that is not blank, as a request keeps a line it did not ask for:
/// trimmed, with every control and hidden character
/// ([`aterm_types::hidden_char::is_hidden_char`]) dropped and a quote made an apostrophe, cut to
/// [`REQUEST_WHY_MAX`] characters and [`REQUEST_WHY_BYTES`] encoded bytes at a character
/// boundary. `None` where nothing is left.
#[must_use]
pub fn line_of(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    let cleaned: String = line
        .chars()
        .filter(|c| !c.is_control() && !aterm_types::hidden_char::is_hidden_char(*c))
        .map(|c| if c == '"' { '\'' } else { c })
        .take(REQUEST_WHY_MAX)
        .collect();
    let cut = aterm_slog::field::clamp_encoded(cleaned.trim(), REQUEST_WHY_BYTES).trim_end();
    (!cut.is_empty()).then(|| cut.to_owned())
}
