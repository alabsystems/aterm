// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The power and Software Update reading `aterm machines macos-reading` prints and the System
//! Resources page reads back. `pmset` and `softwareupdate` are root tools only aterm-machines
//! names (census `machines-root-door`), so the window never runs them: it runs this aterm's
//! `machines macos-reading`, as the person, and judges what it printed with the parse and the
//! judgment aterm-machines' own items use ([`super::power`], [`super::updates`]).
//!
//! The text is one record a read: `<what>\t<ok|err>\t<bytes>\n`, then that many bytes of the
//! answer (or of why it failed) and a newline. A pref's answer is `true`, `false`, `unset`, or
//! `other ` and the value as `defaults export` printed it.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use super::{power, updates};

/// The two `pmset` reads and the schedule's, as recorded.
const CUSTOM: &str = "pmset -g custom";
const CAP: &str = "pmset -g cap";
const SCHEDULE: &str = "softwareupdate --schedule";
/// The head of a pref's answer that holds no switch.
const OTHER: &str = "other ";

/// What one machines-side read answered, keyed by what it read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Readout {
    answers: BTreeMap<String, Result<String, String>>,
}

fn pref_key(key: &str) -> String {
    format!("pref {key}")
}

impl Readout {
    /// Record every read the page judges: `pmset -g custom` and `-g cap` through `power`, and
    /// the schedule and each of [`updates::KEYS`] through `updates`.
    #[must_use]
    pub fn read(power: &dyn power::Reads, updates: &dyn updates::Reads) -> Self {
        let mut answers = BTreeMap::new();
        answers.insert(CUSTOM.to_owned(), power.pmset(&["-g", "custom"]));
        answers.insert(CAP.to_owned(), power.pmset(&["-g", "cap"]));
        answers.insert(SCHEDULE.to_owned(), updates.schedule());
        for key in updates::KEYS {
            let answer = updates.pref(key.key).map(|held| match held {
                updates::Held::Switch(true) => "true".to_owned(),
                updates::Held::Switch(false) => "false".to_owned(),
                updates::Held::Unset => "unset".to_owned(),
                updates::Held::Other(value) => format!("{OTHER}{value}"),
            });
            answers.insert(pref_key(key.key), answer);
        }
        Self { answers }
    }

    /// The text `aterm machines macos-reading` prints.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        for (what, answer) in &self.answers {
            let (word, body) = match answer {
                Ok(text) => ("ok", text),
                Err(why) => ("err", why),
            };
            let _ = write!(out, "{what}\t{word}\t{}\n{body}\n", body.len());
        }
        out
    }

    /// Read back [`Self::render`]'s text.
    ///
    /// # Errors
    /// Where the text is not that record format: a header that is not three tab-separated
    /// fields, an outcome that is neither `ok` nor `err`, a length past the text's end or not on
    /// a character boundary, or a body not followed by its newline.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut answers = BTreeMap::new();
        let mut rest = text;
        while !rest.is_empty() {
            let (header, after) = rest
                .split_once('\n')
                .ok_or_else(|| format!("a record header with no newline: {rest:?}"))?;
            let mut fields = header.split('\t');
            let (Some(what), Some(word), Some(len), None) =
                (fields.next(), fields.next(), fields.next(), fields.next())
            else {
                return Err(format!("not a record header: {header:?}"));
            };
            let len: usize = len
                .parse()
                .map_err(|_| format!("not a length: {header:?}"))?;
            let body = after
                .get(..len)
                .ok_or_else(|| format!("{what}: {len} bytes past the text's end"))?;
            rest = after[len..]
                .strip_prefix('\n')
                .ok_or_else(|| format!("{what}: its body is not followed by a newline"))?;
            let answer = match word {
                "ok" => Ok(body.to_owned()),
                "err" => Err(body.to_owned()),
                other => return Err(format!("{what}: neither ok nor err: {other:?}")),
            };
            answers.insert(what.to_owned(), answer);
        }
        Ok(Self { answers })
    }

    fn answer(&self, what: &str) -> Result<String, String> {
        self.answers
            .get(what)
            .cloned()
            .unwrap_or_else(|| Err(format!("aterm machines macos-reading did not read {what}")))
    }
}

impl power::Reads for Readout {
    fn pmset(&self, args: &[&str]) -> Result<String, String> {
        self.answer(&format!("pmset {}", args.join(" ")))
    }
}

impl updates::Reads for Readout {
    fn schedule(&self) -> Result<String, String> {
        self.answer(SCHEDULE)
    }

    fn pref(&self, key: &str) -> Result<updates::Held, String> {
        let answer = self.answer(&pref_key(key))?;
        match answer.as_str() {
            "true" => Ok(updates::Held::Switch(true)),
            "false" => Ok(updates::Held::Switch(false)),
            "unset" => Ok(updates::Held::Unset),
            other => other
                .strip_prefix(OTHER)
                .map(|value| updates::Held::Other(value.to_owned()))
                .ok_or_else(|| format!("{key} reads {other:?}, no answer a pref has")),
        }
    }
}

#[cfg(test)]
#[path = "readout_tests.rs"]
mod tests;
