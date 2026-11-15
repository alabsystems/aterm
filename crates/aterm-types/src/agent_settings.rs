// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Typed requests for a running agent's native model selection. These are not
//! display metadata: the agent validates and applies the submitted selection.

/// One selection of a model, reasoning effort, or both for the current session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection<'a> {
    /// The provider's exact model identifier, when changing model.
    pub model: Option<&'a str>,
    /// The agent's named reasoning level, when changing effort.
    pub effort: Option<&'a str>,
}

/// The accepted session selection syntax.
pub const USAGE: &str =
    "agent set [model=<id>] [effort=<none|minimal|low|medium|high|xhigh|max|ultra>]";

impl<'a> Selection<'a> {
    /// Parse a single-line selection, refusing duplicates and empty selections.
    pub fn parse(rest: &'a str) -> Result<Self, &'static str> {
        if rest.chars().any(char::is_control) {
            return Err(USAGE);
        }
        let mut words = rest.split_whitespace();
        if words.next() != Some("set") {
            return Err(USAGE);
        }
        let mut result = Self {
            model: None,
            effort: None,
        };
        for word in words {
            match word.split_once('=') {
                Some(("model", model)) if result.model.is_none() && valid_model(model) => {
                    result.model = Some(model)
                }
                Some(("effort", effort)) if result.effort.is_none() && valid_effort(effort) => {
                    result.effort = Some(effort)
                }
                _ => return Err(USAGE),
            }
        }
        if result.model.is_none() && result.effort.is_none() {
            return Err(USAGE);
        }
        Ok(result)
    }

    /// Codex's native session-only selection. Its catalog, not aterm, determines
    /// which model/effort combinations are available to the current account.
    #[must_use]
    pub fn codex_command(self) -> String {
        let mut command = String::from("/model");
        if let Some(model) = self.model {
            command.push(' ');
            command.push_str(model);
        }
        if let Some(effort) = self.effort {
            command.push_str(" --reasoning=");
            command.push_str(effort);
        }
        command
    }
}

fn valid_model(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= 128
        && model.as_bytes()[0].is_ascii_alphanumeric()
        && model
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':' | b'/'))
}

/// Whether `effort` is one of the native Codex reasoning modes.
#[must_use]
pub fn valid_effort(effort: &str) -> bool {
    matches!(
        effort,
        "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max" | "ultra"
    )
}

/// Whether a remote metadata write changes only the session's Autopilot. Aterm's
/// ordinary metadata parser still validates timers and the value before writing.
#[must_use]
pub fn autopilot_write(rest: &str) -> bool {
    if rest.chars().any(char::is_control) {
        return false;
    }
    let words: Vec<_> = rest.split_whitespace().collect();
    match words.as_slice() {
        ["unset", "autopilot"] => true,
        ["set", "autopilot", "off" | "land" | "cruise"] => true,
        ["set", "autopilot", "cruise", until] => until.starts_with("until=") && until.len() > 6,
        _ => false,
    }
}

#[cfg(test)]
#[path = "agent_settings_tests.rs"]
mod tests;
