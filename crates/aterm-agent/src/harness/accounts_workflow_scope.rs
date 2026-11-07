// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Sign-in workflow custody, independent of authentication and login lifetime authority.

use super::*;

/// A nonzero store-local workflow identity issued after explicit policy admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorkflowToken(u64);

/// Durable custody of a workflow and the independent applicability of its retained guidance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowCustody {
    token: WorkflowToken,
    policy: Option<[u8; 32]>,
    row_absent: bool,
    own_dir: Option<String>,
    dir: Option<String>,
    task_retired: bool,
    attempt_retired: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Context {
    policy: Option<[u8; 32]>,
    own_dir: Option<String>,
    dir: Option<String>,
    known: bool,
}

impl Context {
    fn of(raw: Option<&Row>, known: bool, logins: &Logins, label: &str) -> Self {
        let held = logins.workflows.get(label);
        let own_dir = if known {
            raw.and_then(|row| row.dir.clone())
        } else {
            held.and_then(|held| held.own_dir.clone())
        };
        Self {
            policy: if known {
                raw.map(policy)
            } else {
                held.and_then(|held| held.policy)
            },
            dir: logins
                .get(label)
                .and_then(|binding| binding.dir.clone())
                .or_else(|| own_dir.clone()),
            own_dir,
            known: known || held.is_some_and(|held| held.policy.is_some() || held.row_absent),
        }
    }

    fn held(held: &WorkflowCustody) -> Self {
        Self {
            policy: held.policy,
            own_dir: held.own_dir.clone(),
            dir: held.dir.clone(),
            known: held.policy.is_some() || held.row_absent,
        }
    }

    fn matches(&self, current: &Self, allow_unknown: bool) -> bool {
        self.dir == current.dir
            && ((allow_unknown && !self.known && self.policy.is_none())
                || (self.policy == current.policy && self.own_dir == current.own_dir))
    }
}

fn policy(row: &Row) -> [u8; 32] {
    let value = aterm_json::json!({
        "email": row.email,
        "identity_email": row.identity_email,
        "own_dir": row.dir,
        "class": row.class.as_str(),
        "held_by": row.held_by,
        "browser": row.browser,
        "region": row.vendor.region.map(|region| region.as_str()),
        "flow": row.vendor.flow.map(|flow| flow.as_str()),
    });
    let mut digest = aterm_digest::Sha256::new();
    digest.update(b"aterm.sign-in-workflow-policy.v2\0");
    digest.update(value.to_string().as_bytes());
    digest.finalize()
}

#[derive(Clone, Default)]
struct Observation {
    fresh: Option<Context>,
    invalidated: bool,
}

#[derive(Clone)]
struct Capture {
    entry: Context,
    prior: Option<WorkflowCustody>,
    origin: String,
    task: Observation,
    attempt: Observation,
    admission: Observation,
    reconcile: bool,
    replaced: bool,
}

/// Transaction-local workflow snapshots. They never serialize or affect durable equality.
#[derive(Clone, Default)]
pub struct WorkflowContexts {
    entries: BTreeMap<String, Capture>,
    moved: BTreeSet<String>,
    active: bool,
}

impl std::fmt::Debug for WorkflowContexts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkflowContexts")
            .field("entries", &self.entries.len())
            .finish()
    }
}

impl PartialEq for WorkflowContexts {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl Eq for WorkflowContexts {}

#[derive(Clone, Copy)]
pub(super) enum Guidance {
    Task,
    Attempt,
}

impl Guidance {
    fn present(self, logins: &Logins, label: &str) -> bool {
        logins.get(label).is_some_and(|binding| match self {
            Self::Task => binding.sign_in_task.is_some(),
            Self::Attempt => binding.sign_in_attempt.is_some(),
        })
    }

    fn observation(self, capture: &Capture) -> &Observation {
        match self {
            Self::Task => &capture.task,
            Self::Attempt => &capture.attempt,
        }
    }

    fn observation_mut(self, capture: &mut Capture) -> &mut Observation {
        match self {
            Self::Task => &mut capture.task,
            Self::Attempt => &mut capture.attempt,
        }
    }

    fn retired(self, held: &WorkflowCustody) -> bool {
        match self {
            Self::Task => held.task_retired,
            Self::Attempt => held.attempt_retired,
        }
    }

    fn set_retired(self, held: &mut WorkflowCustody, retired: bool) {
        match self {
            Self::Task => held.task_retired = retired,
            Self::Attempt => held.attempt_retired = retired,
        }
    }
}

fn capture(logins: &Logins, label: &str, raw: Option<&Row>, known: bool) -> Capture {
    Capture {
        entry: Context::of(raw, known, logins, label),
        prior: logins.workflows.get(label).cloned(),
        origin: label.to_owned(),
        task: Observation::default(),
        attempt: Observation::default(),
        admission: Observation::default(),
        reconcile: false,
        replaced: false,
    }
}

pub(super) fn begin(raw: Option<&Roster>, logins: &mut Logins) {
    let labels: BTreeSet<_> = raw
        .into_iter()
        .flat_map(|raw| raw.rows.iter().map(|row| row.label.clone()))
        .chain(logins.labels.keys().cloned())
        .chain(logins.workflows.keys().cloned())
        .collect();
    let entries = labels
        .into_iter()
        .map(|label| {
            let entry = capture(
                logins,
                &label,
                raw.and_then(|raw| raw.row(&label)),
                raw.is_some(),
            );
            (label, entry)
        })
        .collect();
    logins.workflow_contexts = WorkflowContexts {
        entries,
        moved: BTreeSet::new(),
        active: true,
    };
}

fn ensure_capture(logins: &mut Logins, label: &str) {
    if !logins.workflow_contexts.entries.contains_key(label) {
        let entry = capture(logins, label, None, false);
        logins
            .workflow_contexts
            .entries
            .insert(label.to_owned(), entry);
    }
}

fn observed_context(logins: &Logins, label: &str) -> Context {
    let mut context = logins
        .workflow_contexts
        .entries
        .get(label)
        .map(|capture| capture.entry.clone())
        .unwrap_or_else(|| Context::of(None, false, logins, label));
    context.dir = logins
        .get(label)
        .and_then(|binding| binding.dir.clone())
        .or_else(|| context.own_dir.clone());
    context
}

pub(super) fn stamp(logins: &mut Logins, label: &str, guidance: Guidance) {
    if !admissible_label(label) {
        return;
    }
    ensure_capture(logins, label);
    let current = observed_context(logins, label);
    let entry = logins.workflow_contexts.entries.get_mut(label).unwrap();
    if !logins.workflow_contexts.active
        || entry.admission.invalidated
        || !entry.admission.fresh.as_ref().is_some_and(|admitted| {
            admitted.known && admitted.policy.is_some() && admitted.matches(&current, false)
        })
    {
        *guidance.observation_mut(entry) = Observation {
            fresh: None,
            invalidated: true,
        };
        if let Some(held) = logins.workflows.get_mut(label) {
            guidance.set_retired(held, true);
        }
        return;
    }
    *guidance.observation_mut(entry) = Observation {
        fresh: Some(current),
        invalidated: entry.replaced,
    };
    if let Some(held) = logins.workflows.get_mut(label) {
        guidance.set_retired(held, false);
    }
}

/// Authorize reconciliation only after the caller has admitted this exact sign-in policy.
pub(super) fn admit(logins: &mut Logins, label: &str) -> Result<(), String> {
    if !logins.workflow_contexts.active || !admissible_label(label) {
        return Err("workflow admission needs an active account transaction".into());
    }
    ensure_capture(logins, label);
    let current = observed_context(logins, label);
    if !current.known || current.policy.is_none() {
        return Err("workflow admission has no captured roster policy".into());
    }
    let entry = logins.workflow_contexts.entries.get_mut(label).unwrap();
    entry.reconcile |= entry
        .prior
        .as_ref()
        .is_some_and(|held| !Context::held(held).matches(&current, true));
    entry.admission = Observation {
        fresh: Some(current),
        invalidated: entry.replaced,
    };
    Ok(())
}

/// Preserve prior fresh intent so a later replacement cannot launder its stale result.
pub(super) fn replace(logins: &mut Logins, label: &str) {
    if !admissible_label(label) {
        return;
    }
    ensure_capture(logins, label);
    let task_present = Guidance::Task.present(logins, label);
    let attempt_present = Guidance::Attempt.present(logins, label);
    let entry = logins.workflow_contexts.entries.get_mut(label).unwrap();
    entry.replaced = true;
    for observation in [&mut entry.task, &mut entry.attempt, &mut entry.admission] {
        observation.invalidated |= observation.fresh.is_some();
    }
    if let Some(held) = logins.workflows.get_mut(label) {
        held.task_retired = task_present;
        held.attempt_retired = attempt_present;
    }
}

pub(super) fn clear_absent(logins: &mut Logins, label: &str) {
    for guidance in [Guidance::Task, Guidance::Attempt] {
        if !guidance.present(logins, label)
            && let Some(held) = logins.workflows.get_mut(label)
        {
            guidance.set_retired(held, false);
        }
    }
}

pub(super) fn move_contexts(logins: &mut Logins, moves: &[(&str, &str)]) {
    let moved: Vec<_> = moves
        .iter()
        .filter(|(from, to)| from != to)
        .map(|(from, to)| {
            ensure_capture(logins, from);
            (
                *from,
                *to,
                logins.workflow_contexts.entries.remove(*from).unwrap(),
                logins.workflows.remove(*from),
            )
        })
        .collect();
    let destinations: BTreeSet<_> = moved.iter().map(|(_, to, _, _)| *to).collect();
    for (from, to, entry, held) in moved {
        logins.workflow_contexts.moved.insert(from.to_owned());
        logins.workflow_contexts.moved.insert(to.to_owned());
        if !destinations.contains(from) {
            let mut tombstone = entry.clone();
            tombstone.replaced = true;
            tombstone.task = Observation::default();
            tombstone.attempt = Observation::default();
            tombstone.admission = Observation::default();
            tombstone.reconcile = false;
            logins
                .workflow_contexts
                .entries
                .insert(from.to_owned(), tombstone);
        }
        logins
            .workflow_contexts
            .entries
            .insert(to.to_owned(), entry);
        if let Some(held) = held {
            logins.workflows.insert(to.to_owned(), held);
        }
    }
}

fn custody_for_context<'a>(
    label: &str,
    logins: &'a Logins,
    current: &Context,
) -> Option<&'a WorkflowCustody> {
    if let Some(entry) = logins.workflow_contexts.entries.get(label)
        && (entry.replaced || !entry.entry.matches(current, false))
    {
        return None;
    }
    let held = logins.workflows.get(label)?;
    (held.token.0 > 0
        && held.policy.is_some()
        && !held.row_absent
        && current.known
        && current.policy.is_some()
        && Context::held(held).matches(current, false))
    .then_some(held)
}

fn current_custody<'a>(row: &Row, logins: &'a Logins) -> Option<&'a WorkflowCustody> {
    let current = Context::of(Some(row), true, logins, &row.label);
    custody_for_context(&row.label, logins, &current)
}

/// Current known-policy custody; unscoped historical metadata grants no workflow identity.
#[must_use]
pub fn current_workflow(row: &Row, logins: &Logins) -> Option<WorkflowToken> {
    current_custody(row, logins).map(|held| held.token)
}

pub(super) fn captured_workflow(logins: &Logins, label: &str) -> Option<WorkflowToken> {
    let context = Context::of(None, false, logins, label);
    custody_for_context(label, logins, &context).map(|held| held.token)
}

/// Whether a captured runtime claim still belongs to this row's workflow.
#[must_use]
pub fn workflow_token_current(row: &Row, logins: &Logins, token: WorkflowToken) -> bool {
    token.0 > 0 && current_custody(row, logins).is_some_and(|held| token == held.token)
}

/// Match captured nonzero workflow evidence across scoped relabels.
#[must_use]
pub fn workflow_event_matches(row: &Row, logins: &Logins, event: &Event) -> bool {
    current_custody(row, logins).is_some_and(|held| event.workflow == Some(held.token))
}

fn guidance_for_context(
    label: &str,
    logins: &Logins,
    guidance: Guidance,
    current: &Context,
) -> bool {
    if logins
        .workflows
        .get(label)
        .is_some_and(|held| guidance.retired(held))
    {
        return false;
    }
    if let Some(entry) = logins.workflow_contexts.entries.get(label) {
        let observation = guidance.observation(entry);
        if observation.invalidated {
            return false;
        }
        if let Some(fresh) = &observation.fresh {
            return logins.workflow_contexts.active
                && entry.admission.fresh.as_ref().is_some_and(|admitted| {
                    !entry.admission.invalidated && admitted.matches(current, false)
                })
                && fresh.known
                && fresh.policy.is_some()
                && fresh.matches(current, false);
        }
    }
    custody_for_context(label, logins, current).is_some()
}

fn guidance_current(row: &Row, logins: &Logins, guidance: Guidance) -> bool {
    let current = Context::of(Some(row), true, logins, &row.label);
    guidance_for_context(&row.label, logins, guidance, &current)
}

/// Whether the old summary belongs to the workflow about to be observed by a writer.
pub(super) fn attempt_current(logins: &Logins, label: &str) -> bool {
    Guidance::Attempt.present(logins, label)
        && guidance_for_context(
            label,
            logins,
            Guidance::Attempt,
            &observed_context(logins, label),
        )
}

/// The retained task only while it describes the current sign-in workflow.
#[must_use]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountWorkflowScope",
        action = "Read",
        project = "aterm_agent::harness::accounts::workflow_scope_tests::workflow_transition"
    )
)]
pub fn current_sign_in_task(row: &Row, logins: &Logins) -> Option<SignInTask> {
    guidance_current(row, logins, Guidance::Task)
        .then(|| logins.get(&row.label)?.sign_in_task)
        .flatten()
}

/// The retained attempt only while it describes the current sign-in workflow.
#[must_use]
pub fn current_sign_in_attempt<'a>(row: &Row, logins: &'a Logins) -> Option<&'a SignInAttempt> {
    guidance_current(row, logins, Guidance::Attempt)
        .then(|| logins.get(&row.label)?.sign_in_attempt.as_ref())
        .flatten()
}

/// Whether the current workflow's own page stopped its last automatic sign-in.
#[must_use]
pub fn current_sign_in_blocked(row: &Row, logins: &Logins) -> bool {
    current_sign_in_attempt(row, logins)
        .is_some_and(|attempt| attempt.outcome == AttemptOutcome::Blocked)
}

fn workflow_kind(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::SignIn | Kind::WrongAccount | Kind::Renewed | Kind::SignedIn
    )
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountWorkflowScope",
        action = "Transform",
        project = "aterm_agent::harness::accounts::workflow_scope_tests::workflow_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountWorkflowScope",
        action = "StaleTask",
        project = "aterm_agent::harness::accounts::workflow_scope_tests::workflow_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountWorkflowScope",
        action = "StaleAttempt",
        project = "aterm_agent::harness::accounts::workflow_scope_tests::workflow_transition"
    )
)]
pub(super) fn finish(
    before: &Logins,
    raw: Option<&Roster>,
    logins: &mut Logins,
    events: &mut [Event],
) -> Result<bool, String> {
    if !logins.workflow_contexts.active {
        return Err("workflow transaction capture was erased; nothing recorded".into());
    }
    if logins.workflow_clock != before.workflow_clock
        || before.workflows.iter().any(|(label, held)| {
            !logins.workflow_contexts.moved.contains(label)
                && logins
                    .workflows
                    .get(label)
                    .is_none_or(|now| now.token != held.token)
        })
    {
        return Err("workflow custody or clock was erased or altered; nothing recorded".into());
    }
    for event in events.iter().filter(|event| workflow_kind(event.kind)) {
        ensure_capture(logins, &event.account);
    }
    let captures = logins.workflow_contexts.entries.clone();
    let mut applicability_changed = false;
    for (label, entry) in &captures {
        let current = Context::of(
            raw.and_then(|raw| raw.row(label)),
            raw.is_some(),
            logins,
            label,
        );
        let has_terminal = events.iter().any(|event| {
            event.account == *label
                && matches!(
                    event.kind,
                    Kind::Renewed | Kind::SignedIn | Kind::WrongAccount
                )
        });
        let fresh_guidance = entry.task.fresh.is_some() || entry.attempt.fresh.is_some();
        let admitted = entry.admission.fresh.is_some();
        let observed = has_terminal || admitted || fresh_guidance || entry.reconcile;
        let replaced = entry.replaced
            || !entry.entry.matches(&current, !entry.entry.known)
            || (observed
                && entry
                    .prior
                    .as_ref()
                    .is_some_and(|held| !Context::held(held).matches(&current, false)));
        if entry
            .admission
            .fresh
            .as_ref()
            .is_some_and(|fresh| entry.admission.invalidated || !fresh.matches(&current, false))
        {
            return Err(
                "sign-in policy no longer matches its workflow admission; nothing recorded".into(),
            );
        }
        for guidance in [Guidance::Task, Guidance::Attempt] {
            let observation = guidance.observation(entry);
            if guidance.present(logins, label)
                && observation.fresh.as_ref().is_some_and(|fresh| {
                    observation.invalidated
                        || (current.known && current.policy.is_none())
                        || !fresh.matches(&current, false)
                })
            {
                return Err(
                    "sign-in guidance no longer matches the final workflow; nothing recorded"
                        .into(),
                );
            }
        }
        if !replaced && !observed {
            continue;
        }
        let token = match &entry.prior {
            Some(prior) if !replaced => prior.token,
            _ => {
                logins.workflow_clock = logins
                    .workflow_clock
                    .checked_add(1)
                    .ok_or("workflow clock is exhausted; nothing recorded")?;
                WorkflowToken(logins.workflow_clock)
            }
        };
        let mut held = WorkflowCustody {
            token,
            policy: current.policy,
            row_absent: current.known && current.policy.is_none(),
            own_dir: current.own_dir.clone(),
            dir: current.dir.clone(),
            task_retired: false,
            attempt_retired: false,
        };
        let prior_context = entry.prior.as_ref().map(Context::held);
        for guidance in [Guidance::Task, Guidance::Attempt] {
            let fresh = guidance.observation(entry).fresh.is_some();
            let was_retired = entry
                .prior
                .as_ref()
                .is_some_and(|held| guidance.retired(held));
            guidance.set_retired(
                &mut held,
                guidance.present(logins, label)
                    && !fresh
                    && (replaced
                        || was_retired
                        || guidance.observation(entry).invalidated
                        || entry
                            .prior
                            .as_ref()
                            .is_none_or(|held| held.policy.is_none() || held.row_absent)),
            );
            let was_present = before
                .get(&entry.origin)
                .is_some_and(|binding| match guidance {
                    Guidance::Task => binding.sign_in_task.is_some(),
                    Guidance::Attempt => binding.sign_in_attempt.is_some(),
                });
            let was_current = was_present
                && prior_context.as_ref().is_some_and(|prior| {
                    guidance_for_context(&entry.origin, before, guidance, prior)
                });
            let is_current = guidance.present(logins, label)
                && held.token.0 > 0
                && held.policy.is_some()
                && !held.row_absent
                && !guidance.retired(&held);
            applicability_changed |= was_present && was_current != is_current;
        }
        applicability_changed |= replaced
            && prior_context
                .as_ref()
                .is_some_and(|prior| custody_for_context(&entry.origin, before, prior).is_some());
        logins.workflows.insert(label.clone(), held);
    }
    let mut tokens = BTreeSet::new();
    if logins
        .workflows
        .values()
        .any(|held| !tokens.insert(held.token))
    {
        return Err("workflow custody identity was duplicated; nothing recorded".into());
    }
    for event in events.iter_mut().filter(|event| workflow_kind(event.kind)) {
        let admitted = captures.get(&event.account).is_some_and(|capture| {
            !capture.admission.invalidated && capture.admission.fresh.is_some()
        });
        if event.kind == Kind::SignIn && !admitted {
            event.workflow = None;
            continue;
        }
        let held = logins
            .workflows
            .get(&event.account)
            .ok_or("workflow event has no admitted custody; nothing recorded")?;
        match event.workflow {
            None => event.workflow = Some(held.token),
            Some(token) if token.0 > 0 && token == held.token => {}
            Some(_) => {
                return Err(
                    "sign-in event no longer matches the final workflow; nothing recorded".into(),
                );
            }
        }
    }
    logins.workflow_contexts = WorkflowContexts::default();
    Ok(applicability_changed)
}

pub(super) fn write_json(logins: &Logins, object: &mut Map) {
    if logins.workflow_clock == 0 && logins.workflows.is_empty() {
        return;
    }
    object.insert("workflow_clock".into(), Value::from(logins.workflow_clock));
    object.insert(
        "workflows".into(),
        Value::Object(
            logins
                .workflows
                .iter()
                .map(|(label, held)| {
                    let mut value = Map::new();
                    value.insert("token".into(), Value::from(held.token.0));
                    value.insert(
                        "policy".into(),
                        Value::from(
                            held.policy
                                .map(|digest| aterm_codec::hex::encode_lower(&digest)),
                        ),
                    );
                    if held.row_absent {
                        value.insert("row_absent".into(), Value::Bool(true));
                    }
                    value.insert("own_dir".into(), Value::from(held.own_dir.clone()));
                    value.insert("dir".into(), Value::from(held.dir.clone()));
                    if held.task_retired {
                        value.insert("task_retired".into(), Value::Bool(true));
                    }
                    if held.attempt_retired {
                        value.insert("attempt_retired".into(), Value::Bool(true));
                    }
                    (label.clone(), Value::Object(value))
                })
                .collect(),
        ),
    );
}

pub(super) fn from_json(doc: &Value) -> Result<(u64, BTreeMap<String, WorkflowCustody>), Unread> {
    let invalid = || Unread::NotOurs("invalid sign-in workflow custody".into());
    if doc.get("workflow_clock").is_none() && doc.get("workflows").is_none() {
        return Ok((0, BTreeMap::new()));
    }
    let clock = doc
        .get("workflow_clock")
        .and_then(Value::as_u64)
        .ok_or_else(invalid)?;
    let entries = doc
        .get("workflows")
        .and_then(Value::as_object)
        .ok_or_else(invalid)?;
    let mut workflows = BTreeMap::new();
    let mut tokens = BTreeSet::new();
    for (label, value) in entries {
        if !admissible_label(label) || !value.is_object() {
            return Err(invalid());
        }
        let token = value
            .get("token")
            .and_then(Value::as_u64)
            .filter(|token| *token != 0 && *token <= clock && tokens.insert(*token))
            .ok_or_else(invalid)?;
        let optional_dir = |key| match value.get(key) {
            Some(Value::Null) => Ok(None),
            Some(Value::String(dir)) if dir_refusal(dir).is_none() => Ok(Some(dir.clone())),
            _ => Err(invalid()),
        };
        let policy = match value.get("policy") {
            Some(Value::Null) => None,
            Some(value) => Some(google_principal(value).ok_or_else(invalid)?),
            None => return Err(invalid()),
        };
        let row_absent = match value.get("row_absent") {
            None => false,
            Some(Value::Bool(absent)) => *absent,
            _ => return Err(invalid()),
        };
        if row_absent && policy.is_some() {
            return Err(invalid());
        }
        let retired = |key| match value.get(key) {
            None => Ok(false),
            Some(Value::Bool(retired)) => Ok(*retired),
            _ => Err(invalid()),
        };
        workflows.insert(
            label.clone(),
            WorkflowCustody {
                token: WorkflowToken(token),
                policy,
                row_absent,
                own_dir: optional_dir("own_dir")?,
                dir: optional_dir("dir")?,
                task_retired: retired("task_retired")?,
                attempt_retired: retired("attempt_retired")?,
            },
        );
    }
    Ok((clock, workflows))
}

impl WorkflowToken {
    pub(in crate::harness) const fn value(self) -> u64 {
        self.0
    }
    pub(super) const fn from_value(value: u64) -> Self {
        Self(value)
    }
}
