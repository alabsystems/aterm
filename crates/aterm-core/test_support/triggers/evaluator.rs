// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Trigger evaluator with rate limiting.
//!
//! Runs triggers on terminal output, tracking evaluated lines for
//! idempotent re-evaluation and respecting rate limits.
//!
//! Extracted from `mod.rs` for file size management.

#[cfg(test)]
use std::time::Duration;
#[cfg(test)]
use std::time::Instant;

#[cfg(test)]
use std::collections::HashMap;

#[cfg(test)]
use super::types::EvaluatedTrigger;
use super::{Trigger, TriggerSet};

// Type alias for Instant.
#[cfg(test)]
type TimeInstant = Instant;

// Type alias for the rate-limit tracking map.
#[cfg(test)]
type RateLimitMap = HashMap<u64, TimeInstant>;

/// Evaluator that runs triggers on terminal output.
///
/// Handles rate limiting and tracking of evaluated lines to support
/// idempotent re-evaluation.
#[derive(Debug)]
pub struct TriggerEvaluator {
    /// The triggers to evaluate.
    triggers: TriggerSet,
    /// Last evaluation time per line (for rate limiting).
    /// Uses the RateLimitMap alias (HashMap).
    #[cfg(test)]
    last_evaluated: RateLimitMap,
    /// Rate limit duration (default 500ms)
    #[cfg(test)]
    rate_limit: Duration,
    /// Maximum lines to track for rate limiting
    #[cfg(test)]
    max_tracked_lines: usize,
    /// Counter for line IDs
    #[cfg(test)]
    pub(super) next_line_id: u64,
}

impl TriggerEvaluator {
    /// Create a new trigger evaluator.
    pub fn new() -> Self {
        Self {
            triggers: TriggerSet::new(),
            #[cfg(test)]
            last_evaluated: RateLimitMap::new(),
            #[cfg(test)]
            rate_limit: Duration::from_millis(500),
            #[cfg(test)]
            max_tracked_lines: 1000,
            #[cfg(test)]
            next_line_id: 0,
        }
    }

    /// Create with a pre-populated trigger set.
    #[cfg(test)]
    pub(crate) fn with_triggers(triggers: TriggerSet) -> Self {
        Self {
            triggers,
            last_evaluated: RateLimitMap::new(),
            rate_limit: Duration::from_millis(500),
            max_tracked_lines: 1000,
            next_line_id: 0,
        }
    }

    /// Set the rate limit duration.
    #[cfg(test)]
    pub(crate) fn set_rate_limit(&mut self, duration: Duration) {
        self.rate_limit = duration;
    }

    /// Get a reference to the trigger set.
    pub fn triggers(&self) -> &TriggerSet {
        &self.triggers
    }

    /// Add a trigger.
    pub fn add_trigger(&mut self, trigger: Trigger) {
        self.triggers.add(trigger);
    }

    /// Allocate a new line ID for tracking.
    #[cfg(test)]
    pub(crate) fn allocate_line_id(&mut self) -> u64 {
        let id = self.next_line_id;
        self.next_line_id = self.next_line_id.wrapping_add(1);
        id
    }

    /// Check if a line should be evaluated (respects rate limiting).
    #[cfg(test)]
    fn should_evaluate(&self, line_id: u64) -> bool {
        match self.last_evaluated.get(&line_id) {
            Some(last) => TimeInstant::now().duration_since(*last) >= self.rate_limit,
            None => true,
        }
    }

    /// Mark a line as evaluated.
    #[cfg(test)]
    fn mark_evaluated(&mut self, line_id: u64) {
        // Clean up old entries if we have too many
        if self.last_evaluated.len() >= self.max_tracked_lines {
            let cutoff = TimeInstant::now()
                .checked_sub(self.rate_limit * 2)
                .unwrap_or(TimeInstant::now());
            self.last_evaluated.retain(|_, v| *v > cutoff);
        }

        self.last_evaluated.insert(line_id, TimeInstant::now());
    }

    /// Evaluate all triggers on a line of text.
    ///
    /// # Arguments
    /// * `text` - The line text to evaluate
    /// * `line_id` - A unique identifier for this line (for rate limiting)
    /// * `is_partial` - Whether this is a partial line (no newline yet)
    ///
    /// # Returns
    /// A vector of evaluated triggers for all matches found.
    #[cfg(test)]
    pub(crate) fn evaluate(
        &mut self,
        text: &str,
        line_id: u64,
        is_partial: bool,
    ) -> Vec<EvaluatedTrigger> {
        // Check rate limiting
        if !self.should_evaluate(line_id) {
            return Vec::new();
        }

        let mut results = Vec::new();

        for (index, trigger) in self.triggers.iter_mut().enumerate() {
            // Skip disabled triggers
            if !trigger.enabled {
                continue;
            }

            // Skip non-partial triggers on partial lines
            if is_partial && !trigger.partial_line {
                continue;
            }

            // Find all matches
            for match_info in trigger.find_all(text) {
                results.push(EvaluatedTrigger {
                    trigger_index: index,
                    match_info,
                    action: trigger.action.clone(),
                });
            }
        }

        // Mark as evaluated
        self.mark_evaluated(line_id);

        results
    }

    /// Evaluate triggers on multiple lines.
    ///
    /// Returns results grouped by line.
    #[cfg(test)]
    pub(crate) fn evaluate_lines(
        &mut self,
        lines: &[(u64, &str, bool)],
    ) -> Vec<(u64, Vec<EvaluatedTrigger>)> {
        lines
            .iter()
            .map(|(line_id, text, is_partial)| {
                let results = self.evaluate(text, *line_id, *is_partial);
                (*line_id, results)
            })
            .filter(|(_, results)| !results.is_empty())
            .collect()
    }

    /// Clear rate limiting state.
    #[cfg(test)]
    pub(crate) fn clear_rate_limit_state(&mut self) {
        self.last_evaluated.clear();
    }
}

impl Default for TriggerEvaluator {
    fn default() -> Self {
        Self::new()
    }
}
