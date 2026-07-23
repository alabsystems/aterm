// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

// Production FFI functions extracted to aterm-core-ffi::triggers (#2584).
// Types remain here for FFI bridge structs; methods are #[cfg(test)].

//! Trigger system for pattern-based actions on terminal output.
//!
//! This module provides a regex-based trigger system similar to Terminal's triggers,
//! allowing pattern matching on terminal output to invoke actions like highlighting,
//! alerts, running commands, or agent notifications.
//!
//! # Architecture
//!
//! The trigger system is designed with these principles:
//!
//! - **Hot/Cold path separation**: Trigger evaluation should not block parsing
//! - **Rate limiting**: Prevents CPU thrashing on rapid output
//! - **Idempotent actions**: Safe to re-evaluate the same content
//! - **Post-processing**: Clean match boundaries for URLs and paths
//!
//! # Example
//!
//! Using the builder pattern (recommended):
//!
//! ```text
//! use aterm_core::prelude::{TriggerBuilder, TriggerAction, TriggerSet};
//!
//! let mut triggers = TriggerSet::new();
//! triggers.add(TriggerBuilder::new()
//!     .pattern(r"error:.*")
//!     .action(TriggerAction::Highlight {
//!         foreground: Some([255, 0, 0]),
//!         background: None,
//!     })
//!     .build()
//!     .expect("builder should produce a valid trigger"));
//! ```
//!
//! Or using the direct constructor:
//!
//! ```text
//! use aterm_core::prelude::{Trigger, TriggerAction, TriggerSet};
//!
//! let mut triggers = TriggerSet::new();
//! triggers.add(Trigger::new(
//!     r"error:.*",
//!     TriggerAction::Highlight {
//!         foreground: Some([255, 0, 0]),
//!         background: None,
//!     },
//! ).expect("constructor should validate pattern"));
//! ```

#[cfg(test)]
mod evaluator;
mod match_utils;

#[cfg(test)]
pub use evaluator::TriggerEvaluator;
#[cfg(test)]
pub(crate) use match_utils::patterns;
#[cfg(test)]
pub(crate) use match_utils::post_process_match;

/// Builder for creating [`Trigger`] instances.
///
/// This provides a clean builder pattern where all configuration is set first,
/// then validated at `build()` time. This is the recommended way to create triggers.
///
/// # Example
///
/// ```
/// use aterm_core::prelude::{TriggerBuilder, TriggerAction};
///
/// let trigger = TriggerBuilder::new()
///     .pattern(r"error:.*")
///     .action(TriggerAction::Bell)
///     .name("error_alert")
///     .partial_line(true)
///     .build()
///     .expect("builder should produce a valid trigger");
/// ```
#[cfg(test)]
#[derive(Debug, Clone)]
pub struct TriggerBuilder {
    pattern: Option<String>,
    action: Option<TriggerAction>,
    name: Option<String>,
    partial_line: bool,
    idempotent: bool,
    enabled: bool,
}

#[cfg(test)]
impl Default for TriggerBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
impl TriggerBuilder {
    /// Create a new trigger builder with default values.
    ///
    /// Default values:
    /// - `partial_line`: false
    /// - `idempotent`: true
    /// - `enabled`: true
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            pattern: None,
            action: None,
            name: None,
            partial_line: false,
            idempotent: true,
            enabled: true,
        }
    }

    /// Set the regex pattern for the trigger.
    ///
    /// This pattern will be validated when `build()` is called.
    #[must_use]
    pub(crate) fn pattern(mut self, pattern: &str) -> Self {
        self.pattern = Some(pattern.to_string());
        self
    }

    /// Set the action to execute when the pattern matches.
    #[must_use]
    pub(crate) fn action(mut self, action: TriggerAction) -> Self {
        self.action = Some(action);
        self
    }

    /// Set a human-readable name for this trigger.
    #[must_use]
    pub(crate) fn name(mut self, name: &str) -> Self {
        self.name = Some(name.to_string());
        self
    }

    /// Set whether to fire on partial lines (before newline).
    ///
    /// Default: false
    #[must_use]
    pub(crate) fn partial_line(mut self, enabled: bool) -> Self {
        self.partial_line = enabled;
        self
    }

    /// Set whether this action is safe to re-run on the same match.
    ///
    /// Default: true
    #[must_use]
    pub(crate) fn idempotent(mut self, idempotent: bool) -> Self {
        self.idempotent = idempotent;
        self
    }

    /// Set whether the trigger is enabled.
    ///
    /// Default: true
    #[must_use]
    pub(crate) fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Build the trigger, validating the pattern.
    ///
    /// Returns an error if:
    /// - No pattern was specified
    /// - No action was specified
    /// - The pattern is invalid regex
    pub(crate) fn build(self) -> Result<Trigger, TriggerError> {
        let pattern = self.pattern.ok_or(TriggerError::MissingPattern)?;
        let action = self.action.ok_or(TriggerError::MissingAction)?;

        // Validate regex on build
        regex::Regex::new(&pattern).map_err(|e| TriggerError::InvalidPattern {
            pattern: pattern.clone(),
            reason: e.to_string(),
        })?;

        Ok(Trigger {
            name: self.name.unwrap_or_default(),
            pattern,
            #[cfg(test)]
            compiled: None,
            action,
            partial_line: self.partial_line,
            idempotent: self.idempotent,
            enabled: self.enabled,
        })
    }
}

/// A compiled trigger pattern with associated action.
#[cfg(test)]
#[derive(Debug, Clone)]
pub struct Trigger {
    /// Human-readable name for this trigger
    pub name: String,
    /// The regex pattern (as string, compiled lazily).
    /// Read via `pattern()` accessor.
    pattern: String,
    /// Compiled regex (lazily populated). Only needed for evaluate() path.
    #[cfg(test)]
    compiled: Option<regex::Regex>,
    /// The action to execute on match
    pub action: TriggerAction,
    /// Whether to fire on partial lines (before newline)
    pub partial_line: bool,
    /// Whether this action is safe to re-run on the same match
    pub idempotent: bool,
    /// Enable/disable this trigger
    pub enabled: bool,
}

#[cfg(test)]
impl Trigger {
    /// Create a new trigger with the given pattern and action.
    ///
    /// Returns an error if the pattern is invalid regex.
    pub fn new(pattern: &str, action: TriggerAction) -> Result<Self, TriggerError> {
        // Validate regex on creation
        regex::Regex::new(pattern).map_err(|e| TriggerError::InvalidPattern {
            pattern: pattern.to_string(),
            reason: e.to_string(),
        })?;

        Ok(Self {
            name: String::new(),
            pattern: pattern.to_string(),
            #[cfg(test)]
            compiled: None,
            action,
            partial_line: false,
            idempotent: true,
            enabled: true,
        })
    }

    /// Create a named trigger.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_name(mut self, name: &str) -> Self {
        self.name = name.to_string();
        self
    }

    /// Set partial line matching.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_partial_line(mut self, enabled: bool) -> Self {
        self.partial_line = enabled;
        self
    }

    /// Set idempotency flag.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_idempotent(mut self, idempotent: bool) -> Self {
        self.idempotent = idempotent;
        self
    }

    /// Get the pattern string.
    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    /// Get or compile the regex.
    #[cfg(test)]
    fn regex(&mut self) -> &regex::Regex {
        if self.compiled.is_none() {
            // Pattern was validated in new(), so this should never fail
            self.compiled = Some(
                regex::Regex::new(&self.pattern)
                    .expect("invariant: pattern validated in constructor"),
            );
        }
        self.compiled
            .as_ref()
            .expect("invariant: just populated above")
    }

    /// Check if the pattern matches the given text.
    ///
    /// Returns the match range if found.
    #[cfg(test)]
    pub(crate) fn matches(&mut self, text: &str) -> Option<TriggerMatch> {
        let regex = self.regex();
        regex.find(text).map(|m| TriggerMatch {
            start: m.start(),
            end: m.end(),
            text: m.as_str().to_string(),
        })
    }

    /// Find all matches in the given text.
    #[cfg(test)]
    pub(crate) fn find_all(&mut self, text: &str) -> Vec<TriggerMatch> {
        let regex = self.regex();
        regex
            .find_iter(text)
            .map(|m| TriggerMatch {
                start: m.start(),
                end: m.end(),
                text: m.as_str().to_string(),
            })
            .collect()
    }
}

// Data types extracted to types.rs (#4613).
#[cfg(test)]
mod types;
#[cfg(test)]
pub use types::TriggerMatch;
#[cfg(test)]
pub use types::{TriggerAction, TriggerError};

/// A collection of triggers.
#[cfg(test)]
#[derive(Debug, Clone, Default)]
pub struct TriggerSet {
    /// Backing storage for triggers.
    triggers: Vec<Trigger>,
}

#[cfg(test)]
impl TriggerSet {
    /// Create an empty trigger set.
    pub fn new() -> Self {
        Self {
            triggers: Vec::new(),
        }
    }

    /// Add a trigger to the set.
    pub fn add(&mut self, trigger: Trigger) {
        self.triggers.push(trigger);
    }

    /// Add a trigger and return its index.
    pub fn add_with_index(&mut self, trigger: Trigger) -> usize {
        let idx = self.triggers.len();
        self.triggers.push(trigger);
        idx
    }

    /// Remove a trigger by index.
    pub fn remove(&mut self, index: usize) -> Option<Trigger> {
        if index < self.triggers.len() {
            Some(self.triggers.remove(index))
        } else {
            None
        }
    }

    /// Get a trigger by index.
    pub fn get(&self, index: usize) -> Option<&Trigger> {
        self.triggers.get(index)
    }

    /// Get the number of triggers.
    pub fn len(&self) -> usize {
        self.triggers.len()
    }

    /// Check if the set is empty.
    pub fn is_empty(&self) -> bool {
        self.triggers.is_empty()
    }

    /// Iterate over triggers.
    #[cfg(test)]
    pub(crate) fn iter(&self) -> impl Iterator<Item = &Trigger> {
        self.triggers.iter()
    }

    /// Iterate over triggers mutably.
    #[cfg(test)]
    pub(crate) fn iter_mut(&mut self) -> impl Iterator<Item = &mut Trigger> {
        self.triggers.iter_mut()
    }

    /// Clear all triggers.
    #[cfg(test)]
    pub(crate) fn clear(&mut self) {
        self.triggers.clear();
    }
}

#[cfg(test)]
mod tests;
