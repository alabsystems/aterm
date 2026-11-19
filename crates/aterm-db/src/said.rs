// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a version 2 table record says of itself (docs/DESIGN-lab-tracker-2026-11-14.md §2, §4.5,
//! package TK-03): who made the change (its [`Principal`]), why (a one-line reason) and on what
//! evidence. The Mac signs the record, so this is the Mac's statement of who acted, read from
//! the process tree where it could be, and a guard against mistakes, not a proof against a member
//! that lies (§4.5).
//!
//! It travels inside the signed, encrypted record (`database::Command::Write` and `Revert`,
//! spelled `"v":2`), and only in a table generation that takes version 2 records
//! (`att::Compiled::records`): a build that reads a generation decodes every record of it the
//! same way, so a version 2 record never stands in a generation an older build reads and
//! refuses (§7, finding 2). Its size is bounded per record, by the reason's and each piece of
//! evidence's byte bounds and the record's body limit; nothing counts records.

use crate::typed::values::{self, Evidence, Principal};
use aterm_json::Value;

/// Longest reason, in bytes: one line of the tracker's prose (`guard::prose`).
pub const MAX_REASON: usize = 1024;

/// The record version a [`Said`] is spelled in.
pub const VERSION: u64 = 2;

/// Who made a record's change, why, and on what evidence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Said {
    /// Who made it, as the signing Mac states it.
    pub principal: Principal,
    /// Why, in one line, where the writer gave a reason.
    pub reason: Option<String>,
    /// What it cites, at most [`values::MAX_EVIDENCE`] pieces.
    pub evidence: Vec<Evidence>,
}

impl Said {
    /// Who made a change, with no reason and no evidence.
    #[must_use]
    pub const fn by(principal: Principal) -> Self {
        Self {
            principal,
            reason: None,
            evidence: Vec::new(),
        }
    }

    /// Check every part: the principal by its type ([`values::principal`]), the reason one line
    /// of at most [`MAX_REASON`] bytes with no secret in it, at most [`values::MAX_EVIDENCE`]
    /// pieces of evidence, each by its type ([`values::evidence`]).
    ///
    /// # Errors
    /// The rule a part breaks; no value is quoted.
    pub fn check(&self) -> Result<(), &'static str> {
        values::principal(&self.principal.to_value())?;
        if let Some(reason) = &self.reason {
            self::reason(reason)?;
        }
        if self.evidence.len() > values::MAX_EVIDENCE {
            return Err("a record cites at most 16 pieces of evidence");
        }
        for piece in &self.evidence {
            values::evidence(&piece.to_value())?;
        }
        Ok(())
    }

    /// The principal as a record spells it.
    #[must_use]
    pub fn principal_value(&self) -> Value {
        self.principal.to_value()
    }

    /// The evidence as a record spells it.
    #[must_use]
    pub fn evidence_values(&self) -> Vec<Value> {
        self.evidence.iter().map(Evidence::to_value).collect()
    }

    /// The whole statement as one value: `principal`, and `reason` and `evidence` where it has
    /// them. What an outbox intent keeps and what history prints.
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut object = aterm_json::Map::new();
        object.insert("principal".into(), self.principal_value());
        if let Some(reason) = &self.reason {
            object.insert("reason".into(), Value::String(reason.clone()));
        }
        if !self.evidence.is_empty() {
            object.insert("evidence".into(), Value::Array(self.evidence_values()));
        }
        Value::Object(object)
    }

    /// The statement [`Self::to_value`] spells, checked ([`Self::check`]).
    ///
    /// # Errors
    /// The value has a member a statement does not name, or a part breaks its rule.
    pub fn from_value(x: &Value) -> Result<Self, &'static str> {
        let object = x
            .as_object()
            .ok_or("a statement of who acted is a record")?;
        if object
            .keys()
            .any(|key| !["principal", "reason", "evidence"].contains(&key.as_str()))
        {
            return Err("a statement of who acted has a member it does not name");
        }
        Self::of_parts(
            object.get("principal"),
            object.get("reason"),
            object.get("evidence"),
        )
    }

    /// The statement a record's `principal`, `reason` and `evidence` members spell, checked. A
    /// reason, where present, is text; evidence, where present, a list of one or more pieces.
    ///
    /// # Errors
    /// No principal, or a part breaks its rule.
    pub fn of_parts(
        principal: Option<&Value>,
        reason: Option<&Value>,
        evidence: Option<&Value>,
    ) -> Result<Self, &'static str> {
        let principal =
            Principal::from_value(principal.ok_or("a version 2 record names its principal")?)?;
        let reason = match reason {
            None => None,
            Some(Value::String(text)) => Some(text.clone()),
            Some(_) => return Err("a record's reason is text"),
        };
        let evidence = match evidence {
            None => Vec::new(),
            Some(Value::Array(pieces)) if !pieces.is_empty() => pieces
                .iter()
                .map(Evidence::from_value)
                .collect::<Result<_, _>>()?,
            Some(_) => return Err("a record's evidence is a list of one or more pieces"),
        };
        let said = Self {
            principal,
            reason,
            evidence,
        };
        said.check()?;
        Ok(said)
    }

    /// The principal in a few words a person reads: its program and session (`claude
    /// s-4705…`); a session aterm read no program of, with what the writer said of itself
    /// marked as its own word (`s-4705… (said: person)`); else what it said of itself.
    #[must_use]
    pub fn who(&self) -> String {
        let p = &self.principal;
        let words: Vec<&str> = [p.program.as_deref(), p.session.as_deref()]
            .into_iter()
            .flatten()
            .collect();
        match (words.is_empty(), &p.program, &p.said) {
            (true, _, said) => said.clone().unwrap_or_default(),
            (false, None, Some(said)) => format!("{} (said: {said})", words.join(" ")),
            (false, ..) => words.join(" "),
        }
    }
}

/// Who makes a write here, read now: [`principal_of`] this process's lineage
/// (`alab::command::session::lineage`), with `reason` and `evidence`, checked.
///
/// # Errors
/// A part breaks its rule ([`Said::check`]).
#[cfg(unix)]
pub fn here(
    by: &str,
    reason: Option<String>,
    evidence: Vec<Evidence>,
) -> Result<Said, &'static str> {
    let said = Said {
        principal: principal_of(crate::alab::command::session::lineage(), by),
        reason,
        evidence,
    };
    said.check()?;
    Ok(said)
}

/// The principal a write states (design §4.5), from what an aterm instance answered of this
/// process's `lineage` and the verb's own word `by` (`person`, `program`, a `--by` word):
///
/// * in an aterm session, that session, and the agent nearest this process in its lineage, or
///   `person` where the instance found none there and nothing else acts in the session (a person
///   typed it at the session's shell); where the instance leaves who acted unknown (one before
///   `program=`, or a session `aterm ctl` started or drives with no agent in this process's
///   lineage), the session and, as what the writer said of itself, `by`;
/// * outside every aterm session, nothing aterm read: `by` as what the writer said of itself.
///
/// Never `ATERM_SESSION_ID` or any other variable: a variable is the environment's claim (§7,
/// finding 5).
#[cfg(unix)]
#[must_use]
pub fn principal_of(
    lineage: Option<crate::alab::command::session::Lineage>,
    by: &str,
) -> Principal {
    match lineage {
        Some(lineage) => {
            let program = lineage
                .program
                .or_else(|| lineage.person.then(|| "person".to_owned()));
            Principal {
                session: Some(lineage.session),
                said: program.is_none().then(|| by.to_owned()),
                program,
                ..Principal::default()
            }
        }
        None => Principal::said(by),
    }
}

/// Check a record's reason: one line, not blank, at most [`MAX_REASON`] bytes, no control
/// character and no secret (`guard::prose`).
///
/// # Errors
/// The rule it breaks; the reason is never quoted.
pub fn reason(text: &str) -> Result<(), &'static str> {
    if text.trim().is_empty() || text.trim() != text {
        return Err("a reason is one line with no white space around it");
    }
    crate::typed::guard::prose(text, MAX_REASON)
        .map_err(|_| "a reason is one line of at most 1,024 bytes with no secret in it")
}

#[cfg(test)]
#[path = "said_tests.rs"]
mod tests;
