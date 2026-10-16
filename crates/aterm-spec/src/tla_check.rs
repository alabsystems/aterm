// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Parses `.tla` files for their module and definition names, to cross-reference Rust refinements
//! against TLA+.

use std::collections::BTreeSet;
use std::path::Path;

/// Error parsing a TLA+ file.
#[non_exhaustive]
#[derive(Debug)]
pub enum TlaParseError {
    Io(std::io::Error),
    NoModule { path: String },
}

impl std::fmt::Display for TlaParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => {
                f.write_str("failed to read TLA+ file: ")?;
                std::fmt::Display::fmt(e, f)
            }
            Self::NoModule { path } => {
                f.write_str("no MODULE declaration found in ")?;
                f.write_str(path)
            }
        }
    }
}

impl std::error::Error for TlaParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::NoModule { .. } => None,
        }
    }
}

impl From<std::io::Error> for TlaParseError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// A parsed TLA+ specification with module name and extracted definitions.
#[derive(Debug, Clone)]
pub struct TlaSpec {
    /// Module name from the `---- MODULE <name> ----` header.
    pub module_name: String,
    /// File path (for reporting).
    pub file_path: String,
    /// Every named top-level definition (actions, invariants, `Next`, `Spec`, named constants):
    /// the set an anchor must name a member of (obligation 1).
    pub actions: BTreeSet<String>,
    /// The action names, the disjuncts of `Next == A \/ B \/ …`: the coverage set (obligation 3)
    /// that every actively bound external machine must bind or waive. Excludes `Init`, `Spec`,
    /// `TypeOK`, invariants and constants.
    pub next_actions: BTreeSet<String>,
}

impl TlaSpec {
    /// Parse a TLA+ file from disk.
    // Skip: fs read + line-scan iterators (absent std bodies); every malformed
    // input returns Err (fail-closed). Spec tooling, not shipping runtime.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn parse_file(path: &Path) -> Result<Self, TlaParseError> {
        let bytes = std::fs::read(path)?;
        // An explicit `String::from_utf8` so the hardened gate sees the reject path; the error is
        // the one `read_to_string` returns on invalid UTF-8.
        let content = match String::from_utf8(bytes) {
            Ok(s) => s,
            Err(_) => {
                return Err(TlaParseError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "stream did not contain valid UTF-8",
                )));
            }
        };
        let file_path = path.display().to_string();
        Self::parse_str(&content, &file_path)
    }

    /// Parse TLA+ content from a string.
    pub fn parse_str(content: &str, file_path: &str) -> Result<Self, TlaParseError> {
        let module_name = extract_module_name(content).ok_or_else(|| TlaParseError::NoModule {
            path: file_path.to_string(),
        })?;
        let actions = extract_definitions(content);
        let next_actions = extract_next_disjuncts(content);
        Ok(TlaSpec {
            module_name,
            file_path: file_path.to_string(),
            actions,
            next_actions,
        })
    }
}

/// Extract the module name from `---- MODULE <name> ----` header.
// Skip: the line-scan drives std iterator `next` (absent body); every
// malformed input returns None (fail-closed). Spec tooling.
#[cfg_attr(trust_verify, trust::skip)]
fn extract_module_name(content: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if !trimmed.contains("MODULE") {
            continue;
        }
        // No intermediate `Vec`: Trust L0 cannot bound its allocation. The token after the first
        // `MODULE` is the name.
        let mut tokens = trimmed.split_whitespace();
        for tok in tokens.by_ref() {
            if tok == "MODULE" {
                if let Some(next_tok) = tokens.next() {
                    let name = next_tok.trim_matches('-').trim();
                    if !name.is_empty() {
                        return Some(name.to_string());
                    }
                }
                // Only the first `MODULE` on a line counts.
                break;
            }
        }
    }
    None
}

/// The block-comment nesting depth after `line`, from `depth`: `(*` opens, `*)` closes, and
/// comments nest.
fn update_comment_depth(line: &str, mut depth: u32) -> u32 {
    // `pending` is the previous unconsumed byte; a matched `(*` or `*)` consumes both. No index
    // arithmetic, so no bounds obligation.
    let mut pending: Option<u8> = None;
    for &b in line.as_bytes() {
        match pending {
            Some(b'(') if b == b'*' => {
                depth = depth.saturating_add(1);
                pending = None;
            }
            Some(b'*') if b == b')' => {
                depth = depth.saturating_sub(1);
                pending = None;
            }
            _ => pending = Some(b),
        }
    }
    depth
}

/// Every top-level definition: `Name ==` or `Name(params) ==` indented at most four columns, with
/// an optional `THEOREM` or `LOCAL` prefix. Skips TLA+ keywords and comments.
// Skip: line-scan iterator `next` + BTreeSet keyed build (absent std
// bodies). Spec tooling; malformed input yields an empty set.
#[cfg_attr(trust_verify, trust::skip)]
fn extract_definitions(content: &str) -> BTreeSet<String> {
    let mut defs = BTreeSet::new();
    let mut comment_depth: u32 = 0;

    for line in content.lines() {
        let trimmed = line.trim();

        // Skip every line that opens, closes or sits inside a block comment (they nest).
        if comment_depth > 0 || trimmed.contains("(*") {
            comment_depth = update_comment_depth(trimmed, comment_depth);
            continue;
        }

        // Skip line comments
        if trimmed.starts_with("\\*") {
            continue;
        }

        // Skip lines indented past four columns (LET bodies and the like).
        if !line.is_empty() && line.starts_with(|c: char| c.is_whitespace()) {
            // Saturating only to discharge the underflow obligation (Trust L0): `trim_start`
            // returns a suffix.
            let indent = line.len().saturating_sub(line.trim_start().len());
            if indent > 4 {
                continue;
            }
        }

        if let Some(name) = extract_definition_name(trimmed)
            && !is_tla_keyword(&name)
        {
            defs.insert(name);
        }
    }
    defs
}

/// The action names: the disjuncts of `Next == …`, not every top-level definition (which includes
/// `Init`, `Spec`, `TypeOK`, invariants and constants). The coverage obligation's set.
///
/// Handles the ISOLATION specs' two shapes: a plain disjunction (`Next == Fork \/ … \/ Exec`) and
/// quantified disjuncts (`\/ \E c \in … : WriteMain(c, v)`), of which the called action is taken.
/// The body runs to the next unindented definition, blank line or module end.
// Skip: line-scan iterator + BTreeSet keyed build (absent std bodies).
#[cfg_attr(trust_verify, trust::skip)]
fn extract_next_disjuncts(content: &str) -> BTreeSet<String> {
    // The `Next == …` body, possibly over several lines.
    let mut body = String::new();
    let mut in_next = false;
    let mut comment_depth: u32 = 0;
    for line in content.lines() {
        let trimmed = line.trim();
        if comment_depth > 0 || trimmed.contains("(*") {
            comment_depth = update_comment_depth(trimmed, comment_depth);
            continue;
        }
        let code = match trimmed.split_once("\\*") {
            Some((before, _)) => before.trim_end(),
            None => trimmed,
        };
        if in_next {
            // An unindented definition, end marker or blank line ends the body.
            let is_new_def = !line.starts_with(char::is_whitespace)
                && (code.contains("==") || code.starts_with("===") || code.is_empty());
            if is_new_def {
                break;
            }
            body.push(' ');
            body.push_str(code);
        } else if code.starts_with("Next")
            && let Some((lhs, rhs)) = code.split_once("==")
            && lhs.trim() == "Next"
        {
            in_next = true;
            body.push_str(rhs);
        }
    }

    // Per `\/` disjunct, the first identifier after any quantifier prelude's `:`
    // (`\E c \in 1..Cells, v \in 1..MaxVal :`).
    let mut actions = BTreeSet::new();
    for disj in body.split("\\/") {
        let segment = match disj.rsplit_once(':') {
            Some((_, after)) => after,
            None => disj,
        };
        if let Some(name) = leading_identifier(segment)
            && !is_tla_keyword(&name)
            && name != "UNCHANGED"
        {
            actions.insert(name);
        }
    }
    actions
}

/// The first TLA+ identifier in `s` (leading whitespace skipped), or `None`.
fn leading_identifier(s: &str) -> Option<String> {
    let s = s.trim_start();
    let mut chars = s.chars();
    let first = chars.next()?;
    if !first.is_alphabetic() && first != '_' {
        return None;
    }
    let mut name = String::new();
    name.push(first);
    for c in chars {
        if c.is_alphanumeric() || c == '_' {
            name.push(c);
        } else {
            break;
        }
    }
    Some(name)
}

/// Try to extract a definition name from a single line.
fn extract_definition_name(line: &str) -> Option<String> {
    // Handle `THEOREM Name ==` and `LOCAL Name ==` prefixes
    let effective = if line.starts_with("THEOREM ") {
        line.strip_prefix("THEOREM ")?.trim_start()
    } else if line.starts_with("LOCAL ") {
        line.strip_prefix("LOCAL ")?.trim_start()
    } else {
        line
    };

    // `split_once`, not `find` + slicing: no slice-bounds obligation (Trust L0).
    let (before, after) = effective.split_once("==")?;

    // `====` is the end-of-module marker, not a definition.
    if after.starts_with("==") {
        return None;
    }

    let before = before.trim();
    if before.is_empty() {
        return None;
    }

    // Strip parameters: `Name(params)` → `Name`
    let name = match before.split_once('(') {
        Some((n, _)) => n.trim(),
        None => before,
    };

    // Validate identifier
    if name.is_empty() {
        return None;
    }
    let mut chars = name.chars();
    let first = chars.next()?;
    if !first.is_alphabetic() && first != '_' {
        return None;
    }
    if !chars.all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }

    Some(name.to_string())
}

fn is_tla_keyword(name: &str) -> bool {
    matches!(
        name,
        "CONSTANTS"
            | "CONSTANT"
            | "VARIABLES"
            | "VARIABLE"
            | "ASSUME"
            | "EXTENDS"
            | "INSTANCE"
            | "MODULE"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_module() {
        let spec = TlaSpec::parse_str(
            r#"
---- MODULE Modes ----
VARIABLES mode

SetMode == mode' = TRUE
ResetMode == mode' = FALSE

Next == SetMode \/ ResetMode
====
"#,
            "tla/Modes.tla",
        )
        .unwrap();

        assert_eq!(spec.module_name, "Modes");
        assert_eq!(spec.file_path, "tla/Modes.tla");
        assert!(spec.actions.contains("SetMode"));
        assert!(spec.actions.contains("ResetMode"));
        assert!(spec.actions.contains("Next"));
    }

    #[test]
    fn test_parse_parametric_definition() {
        let spec = TlaSpec::parse_str(
            r#"
---- MODULE Test ----
VARIABLES x

Inc(n) == x' = x + n
Dec == x' = x - 1

Next == Inc(1) \/ Dec
====
"#,
            "test.tla",
        )
        .unwrap();

        assert!(spec.actions.contains("Inc"));
        assert!(spec.actions.contains("Dec"));
        assert!(spec.actions.contains("Next"));
    }

    #[test]
    fn test_parse_theorem_definitions() {
        let spec = TlaSpec::parse_str(
            r#"
---- MODULE Thm ----
VARIABLES x

Safety == x >= 0

THEOREM SafetyHolds == Spec => []Safety
====
"#,
            "thm.tla",
        )
        .unwrap();

        assert!(spec.actions.contains("Safety"));
        assert!(spec.actions.contains("SafetyHolds"));
    }

    #[test]
    fn test_no_module_returns_error() {
        let result = TlaSpec::parse_str("no module here", "bad.tla");
        assert!(result.is_err());
    }

    #[test]
    fn test_skips_keywords() {
        let spec = TlaSpec::parse_str(
            r#"
---- MODULE KW ----
VARIABLES x
Init == x = 0
====
"#,
            "kw.tla",
        )
        .unwrap();

        assert!(spec.actions.contains("Init"));
        assert!(!spec.actions.contains("VARIABLES"));
    }

    #[test]
    fn test_end_marker_not_parsed_as_definition() {
        let spec = TlaSpec::parse_str(
            r#"
---- MODULE End ----
VARIABLES x
Init == x = 0
=============================================================================
"#,
            "end.tla",
        )
        .unwrap();

        assert!(spec.actions.contains("Init"));
        // The ==== end marker should not produce a definition
        assert_eq!(spec.actions.len(), 1);
    }

    #[test]
    fn test_nested_block_comments_parse_correctly() {
        // The first `*)` closes only the inner comment.
        let spec = TlaSpec::parse_str(
            r#"
---- MODULE Nested ----
VARIABLES x

(* outer comment
   (* inner comment *)
   still inside outer comment
*)

Init == x = 0
Next == x' = x + 1
====
"#,
            "nested.tla",
        )
        .unwrap();

        assert_eq!(spec.module_name, "Nested");
        assert!(spec.actions.contains("Init"));
        assert!(spec.actions.contains("Next"));
        // Only Init and Next should be extracted; nothing from inside the comment.
        assert_eq!(spec.actions.len(), 2);
    }

    #[test]
    fn test_deeply_nested_block_comments() {
        let spec = TlaSpec::parse_str(
            r#"
---- MODULE Deep ----
VARIABLES x

(* level 1
   (* level 2
      (* level 3 *)
   *)
*)

Visible == x = 0
====
"#,
            "deep.tla",
        )
        .unwrap();

        assert!(spec.actions.contains("Visible"));
        assert_eq!(spec.actions.len(), 1);
    }

    #[test]
    fn next_disjuncts_plain_and_quantified() {
        // Plain disjunction.
        let plain = extract_next_disjuncts(
            "Next == Fork \\/ Setrlimit \\/ Chdir \\/ CloseMaster \\/ UnsafeEnvOp \\/ Exec\n",
        );
        assert_eq!(
            plain,
            [
                "Fork",
                "Setrlimit",
                "Chdir",
                "CloseMaster",
                "UnsafeEnvOp",
                "Exec"
            ]
            .iter()
            .map(|s| s.to_string())
            .collect()
        );
        // Quantified and multi-line (the AltScreen shape): the action after the quantifier's `:`.
        let quant = extract_next_disjuncts(
            "Next ==\n    \\/ \\E c \\in 1..Cells, v \\in 1..MaxVal : WriteMain(c, v)\n    \\/ Enter\n    \\/ \\E c \\in 1..Cells, v \\in 1..MaxVal : Scribble(c, v)\n    \\/ Leave\n\nSpec == Init\n",
        );
        assert_eq!(
            quant,
            ["WriteMain", "Enter", "Scribble", "Leave"]
                .iter()
                .map(|s| s.to_string())
                .collect()
        );
    }

    #[test]
    fn next_disjuncts_single_action() {
        // A single action: the Sandbox / PathConfine shape.
        assert_eq!(
            extract_next_disjuncts("Next == Apply\n"),
            ["Apply"].iter().map(|s| s.to_string()).collect()
        );
        assert_eq!(
            extract_next_disjuncts("Next == Confine\n"),
            ["Confine"].iter().map(|s| s.to_string()).collect()
        );
    }

    #[test]
    fn test_single_line_block_comment() {
        let spec = TlaSpec::parse_str(
            r#"
---- MODULE Inline ----
VARIABLES x

(* this is a single-line block comment *)

Init == x = 0
====
"#,
            "inline.tla",
        )
        .unwrap();

        assert!(spec.actions.contains("Init"));
        assert_eq!(spec.actions.len(), 1);
    }
}
