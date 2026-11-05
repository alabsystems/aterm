// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The immutable recipe boundary for replacing one stored account. This is a
//! structural check, not an execution grant or evidence that the predecessor closed.

use super::accounts::LOGIN_ENV;
use super::accounts_pool::ExecutionIntent;
use super::native_act::{ImagePhase, NativePlan, Prepare, Request, RestartCommand};

impl Prepare {
    /// Construct a fresh recipe by changing only the selected stored account.
    ///
    /// Returns `None` for a missing active command, a non-stored or malformed
    /// intent, an invalid new context, or a command that exceeds wire/line bounds.
    /// The original recipe is untouched, and the result still needs independent
    /// NoExec evidence, caller authority, durable claim and current admission.
    #[must_use]
    pub fn with_account_successor(
        &self,
        phase: ImagePhase,
        id: String,
        revision: u64,
        human_seq: u64,
        deadline_us: u64,
        account: ExecutionIntent,
    ) -> Option<Self> {
        let ExecutionIntent::Stored { login, .. } = &account else {
            return None;
        };
        let mut next = self.clone();
        next.id = id;
        next.revision = revision;
        next.expected_human_seq = Some(human_seq);
        next.deadline_us = deadline_us;
        let NativePlan::Restart {
            forward,
            rollback,
            cold: None,
            ..
        } = &mut next.plan
        else {
            return None;
        };
        let command = match phase {
            ImagePhase::Forward => forward.as_mut(),
            ImagePhase::Rollback => rollback.as_deref_mut()?,
        };
        command.env.retain(|(name, _)| name != LOGIN_ENV);
        let mut removed = command.unset.take().unwrap_or_default();
        removed.retain(|name| name != LOGIN_ENV);
        match login {
            Some(login) => command.env.push((LOGIN_ENV.into(), login.clone())),
            None => removed.push(LOGIN_ENV.into()),
        }
        command.unset = (!removed.is_empty()).then_some(removed);
        command.account_intent = Some(Box::new(account));
        self.account_successor_recipe(&next, phase).then_some(next)
    }

    /// Whether `next` changes only the active command's stored-account binding.
    ///
    /// The operation ID must be new. Revision, human-input sequence and deadline
    /// may be recaptured, but the host epoch, duty, process identities, conversation
    /// arguments and inactive rollback/forward command remain exact. Default-account
    /// selection explicitly clears the login variable. Both intents must name the
    /// same local account namespace. The same account may serve a fresh attempt;
    /// it still needs independent current admission and a new operation identity.
    ///
    /// This reads no credentials or filesystem state. A caller must independently
    /// prove NoExec, retained quiescence, current caller authority, deadline and
    /// account admission before claiming or executing a successor.
    #[must_use]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "NativeAccountSuccessorRecipe",
            action = "ChangeContext",
            reason = "The caller supplies a candidate with changed operation context"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "NativeAccountSuccessorRecipe",
            action = "ChangeNative",
            reason = "The caller supplies changed native process or endpoint identities"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "NativeAccountSuccessorRecipe",
            action = "ChangeCommand",
            reason = "The caller supplies changed non-account command fields"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "NativeAccountSuccessorRecipe",
            action = "ExternalAccount",
            reason = "The caller supplies an external or absent account intent"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "NativeAccountSuccessorRecipe",
            action = "ChangeNamespace",
            reason = "The caller supplies a different account namespace"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "NativeAccountSuccessorRecipe",
            action = "MismatchRouting",
            reason = "The caller supplies selector exports inconsistent with account intent"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "NativeAccountSuccessorRecipe",
            action = "ReuseOperation",
            reason = "The caller supplies the predecessor operation identity again"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "NativeAccountSuccessorRecipe",
            action = "InvalidEncoding",
            reason = "The caller supplies a noncanonical bounded request"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeAccountSuccessorRecipe",
            action = "Check",
            project = "aterm_agent::harness::native_account_recipe::tests::transition"
        )
    )]
    pub fn account_successor_recipe(&self, next: &Self, phase: ImagePhase) -> bool {
        if self.id == next.id
            || Request::prepare(self.clone()).encode().is_none()
            || Request::prepare(next.clone()).encode().is_none()
        {
            return false;
        }
        let (Some(previous), Some(candidate)) = (
            active_command(&self.plan, phase),
            active_command(&next.plan, phase),
        ) else {
            return false;
        };
        if !same_command_except_account(previous, candidate) {
            return false;
        }
        let mut allowed = self.clone();
        allowed.id.clone_from(&next.id);
        allowed.revision = next.revision;
        allowed.expected_human_seq = next.expected_human_seq;
        allowed.deadline_us = next.deadline_us;
        let NativePlan::Restart {
            forward, rollback, ..
        } = &mut allowed.plan
        else {
            return false;
        };
        match phase {
            ImagePhase::Forward => **forward = candidate.clone(),
            ImagePhase::Rollback => match rollback {
                Some(back) => **back = candidate.clone(),
                None => return false,
            },
        }
        allowed == *next
    }
}

fn active_command(plan: &NativePlan, phase: ImagePhase) -> Option<&RestartCommand> {
    match plan {
        NativePlan::Restart {
            forward,
            rollback,
            cold: None,
            ..
        } => match phase {
            ImagePhase::Forward => Some(forward),
            ImagePhase::Rollback => rollback.as_deref(),
        },
        _ => None,
    }
}

fn same_command_except_account(previous: &RestartCommand, candidate: &RestartCommand) -> bool {
    let (Some(a), Some(b)) = (
        previous.account_intent.as_deref(),
        candidate.account_intent.as_deref(),
    ) else {
        return false;
    };
    let (
        ExecutionIntent::Stored { namespace: old, .. },
        ExecutionIntent::Stored { namespace: new, .. },
    ) = (a, b)
    else {
        return false;
    };
    // Stored intents contain canonical paths; an old captured command can
    // spell that path through an alias. Do not re-resolve or re-authorize the
    // old NoExec command here. Only the replacement will be executed, and its
    // explicit selector must agree with its freshly captured canonical intent.
    if old != new || !route_agrees(candidate, b) {
        return false;
    }
    let strip = |command: &RestartCommand| {
        let mut stripped = command.clone();
        stripped.account_intent = None;
        stripped.env.retain(|(name, _)| name != LOGIN_ENV);
        // None and a list containing only the account removal have the same
        // non-account effects. Preserve order and every other removal exactly.
        stripped.unset = Some(
            stripped
                .unset
                .take()
                .unwrap_or_default()
                .into_iter()
                .filter(|name| name != LOGIN_ENV)
                .collect(),
        );
        stripped
    };
    strip(previous) == strip(candidate)
}

fn route_agrees(command: &RestartCommand, intent: &ExecutionIntent) -> bool {
    if command.unset.as_ref().is_some_and(Vec::is_empty) {
        return false;
    }
    let ExecutionIntent::Stored { login, .. } = intent else {
        return false;
    };
    let exports = command
        .env
        .iter()
        .filter(|(name, _)| name == LOGIN_ENV)
        .map(|(_, value)| value.as_str())
        .collect::<Vec<_>>();
    let removals = command.unset.as_ref().map_or(0, |names| {
        names.iter().filter(|name| *name == LOGIN_ENV).count()
    });
    match login {
        Some(login) => exports == [login.as_str()] && removals == 0,
        None => exports.is_empty() && removals == 1,
    }
}

#[cfg(test)]
#[path = "native_account_recipe_tests.rs"]
mod tests;
