// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live, bounded terminal descriptions for window and tab chrome.
//!
//! The terminal's OSC 0/2 title remains authoritative identity. This module owns a
//! separate, display-only description: an immediate deterministic summary of the
//! current shell block, optionally refined by one asynchronous model worker. Terminal
//! output is untrusted prompt data; a model result can only become sanitized label
//! text and can never drive a terminal action.

mod description;
mod managed_ollama;
mod model_store;
mod redaction;
mod transport;

use crate::app_config::{Config, TitleFormat, TitleSummaryProvider, TitleSummaryProxyMode};
use crate::{App, Wake, WindowId};
use aterm_core::terminal::Terminal;
use aterm_types::BlockState;
use description::{
    bounded_text, compose_presentation, deterministic_description, idle_prompt_description,
    is_bidi_control, title_is_presentation_clean,
};
use managed_ollama::{ManagedOllamaController, ManagedRuntimeExit, managed_ollama_paths};
use redaction::redact_context_line;
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};
use transport::{effective_transport, endpoint_is_loopback, worker_loop};
use winit::event_loop::EventLoopProxy;

// The credential predicates are crate API (`prefs` validates config values with
// them), so they keep resolving at `crate::title_summary::` even though they now
// live in the private `redaction` submodule.
pub(crate) use redaction::{
    endpoint_has_query_or_fragment, endpoint_is_credential_free_absolute_url,
    looks_like_raw_credential,
};

const MAX_COMMAND_CHARS: usize = 320;
const MAX_CONTEXT_LINE_CHARS: usize = 512;
const MAX_CONTEXT_BYTES: usize = 12 * 1024;
const MAX_RESPONSE_BYTES: u64 = 32 * 1024;
const MAX_BACKOFF: Duration = Duration::from_secs(5 * 60);

/// Whether an endpoint is durable user authority or a placeholder for an
/// aterm-owned, per-process endpoint. This bit is part of the exact authority:
/// deleting an explicit `:11434` value must not alias leaving the key absent.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum EndpointOrigin {
    AutomaticManaged,
    Configured,
}

/// One generation-checked, latest-wins request. Only one request can be in flight
/// process-wide; pending work is coalesced by session in [`Coordinator::pending`].
#[derive(Clone, Debug)]
struct Job {
    session: u64,
    session_epoch: u64,
    session_authority: Arc<AtomicU64>,
    generation: u64,
    authority_epoch: u64,
    config_fingerprint: u64,
    settings: ProviderSettings,
    snapshot: Snapshot,
}

#[derive(Clone, Debug)]
struct ProviderSettings {
    provider: TitleSummaryProvider,
    model: String,
    endpoint: String,
    endpoint_origin: EndpointOrigin,
    token_file: Option<String>,
    allow_remote: bool,
    timeout: Duration,
    proxy_mode: TitleSummaryProxyMode,
    ca_file: Option<String>,
}

/// Transport policy after provider and URL semantics have been resolved. The HTTP
/// client and Settings health card consume this same value so neither can claim a
/// proxy/CA policy different from the one the request actually uses.
#[derive(Clone, Debug, PartialEq, Eq)]
struct EffectiveTransport {
    proxy_mode: TitleSummaryProxyMode,
    ca_file: Option<String>,
}

#[derive(Clone, Debug)]
struct WorkerResult {
    session: u64,
    session_epoch: u64,
    generation: u64,
    authority_epoch: u64,
    config_fingerprint: u64,
    result: Result<String, String>,
    locality: TitleSummaryLocality,
    effective_endpoint: Option<String>,
    managed_install_present: bool,
}

#[derive(Clone, Debug)]
enum WorkerMessage {
    Result(WorkerResult),
    ManagedRuntimeExited(ManagedRuntimeExit),
}

/// Runtime state surfaced to native Settings. This is deliberately distinct from
/// persisted configuration: it describes what the bounded worker is doing now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TitleSummaryRuntimeState {
    Disabled,
    Builtin,
    Idle,
    Starting,
    Ready,
    BackingOff,
    Error,
}

/// Truthful locality of the endpoint that most recently handled (or rejected) work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TitleSummaryLocality {
    NotApplicable,
    ManagedLocal,
    UnattestedLoopback,
    Remote,
}

/// Read-only operational health for Settings and diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TitleSummaryHealth {
    pub(crate) state: TitleSummaryRuntimeState,
    pub(crate) provider: TitleSummaryProvider,
    pub(crate) model: Option<String>,
    pub(crate) endpoint: Option<String>,
    pub(crate) locality: TitleSummaryLocality,
    pub(crate) managed_install_present: bool,
    pub(crate) model_ready: bool,
    pub(crate) last_error: Option<String>,
    pub(crate) next_retry_after: Option<Duration>,
    pub(crate) next_refresh_after: Option<Duration>,
    pub(crate) timeout: Option<Duration>,
    pub(crate) proxy_mode: Option<TitleSummaryProxyMode>,
    pub(crate) ca_file: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AuthorityStamp {
    epoch: u64,
    fingerprint: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RequestStamp {
    generation: u64,
    authority: AuthorityStamp,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SemanticStamp {
    title_epoch: u64,
    block_id: Option<u64>,
    block_state: ActivityState,
    exit_code: Option<i32>,
    command_hash: u64,
    cwd_hash: u64,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum ActivityState {
    Unknown,
    Prompt,
    Entering,
    Executing,
    Complete,
}

/// The transitions the derived model's capacity-one observation retry moves on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ObservationRetryTransition {
    Contended,
    Succeeded,
    Disabled,
    Retired,
}

/// Shipping projection of the derived model's capacity-one observation retry.
///
/// Deliberately a named function rather than four inlined literals: it is the
/// ONE place the shipping code states the policy, which is what
/// `observation_retry_transitions_conform_to_derived_model` compares against
/// `title_summary_model()`. Fold it back into its call sites and the retry
/// policy is expressed only by which sites happen to clear `retries`, and the
/// model can drift away from the code with nothing left to notice.
fn retry_pending_after(transition: ObservationRetryTransition) -> bool {
    transition == ObservationRetryTransition::Contended
}

#[derive(Clone, Debug)]
struct Snapshot {
    title: String,
    cwd: String,
    command: String,
    state: ActivityState,
    exit_code: Option<i32>,
    recent_output: String,
}

impl Snapshot {
    fn metadata(term: &Terminal) -> (Self, SemanticStamp) {
        use crate::cwd_native::ReportedCwd as _;
        let stamp = semantic_stamp(term);
        let title = bounded_text(term.title(), MAX_COMMAND_CHARS);
        // This snapshot feeds the summarizer's prompt and the description shown
        // to the user, so it carries the native path rather than the engine's
        // RFC 8089 URI path — a model told the cwd is `/C:/Users//x` will happily
        // repeat that non-path back into a tab description.
        let cwd = bounded_text(
            term.native_working_directory().unwrap_or_default().as_ref(),
            MAX_COMMAND_CHARS,
        );
        let block = term.current_block().or_else(|| term.all_blocks().last());
        let (state, exit_code, command, block_cwd) = block.map_or(
            (ActivityState::Unknown, None, String::new(), None),
            |block| {
                let command = block
                    .commandline
                    .as_deref()
                    .map(|text| bounded_text(text, MAX_COMMAND_CHARS))
                    .filter(|text| !text.is_empty())
                    .unwrap_or_default();
                (
                    activity_state(block.state),
                    block.exit_code,
                    command,
                    block.working_directory.as_deref().map(str::to_owned),
                )
            },
        );
        let cwd = if cwd.is_empty() {
            bounded_text(block_cwd.as_deref().unwrap_or_default(), MAX_COMMAND_CHARS)
        } else {
            cwd
        };
        (
            Self {
                title,
                cwd,
                command,
                state,
                exit_code,
                recent_output: String::new(),
            },
            stamp,
        )
    }

    fn capture_recent_output(&mut self, term: &Terminal, wanted: usize) {
        if wanted == 0 {
            return;
        }
        let rows = i32::from(term.rows());
        let end = i32::from(term.cursor().row.saturating_add(1))
            .min(rows)
            .max(0);
        let wanted = i32::try_from(wanted).unwrap_or(i32::MAX);
        // Scan backward through a bounded 4× window so blank screen rows do not hide
        // recent non-empty scrollback, then retain the newest N non-empty lines.
        let scan = wanted.saturating_mul(4).min(320);
        let start = end.saturating_sub(scan);
        let mut lines = Vec::with_capacity(usize::try_from(wanted).unwrap_or(0));
        let col_range = (term.cols() > 0).then(|| (0, term.cols().saturating_sub(1).min(511)));
        for row in start..end {
            let Some(line) = term.get_line_text(row, col_range) else {
                continue;
            };
            let line = redact_context_line(&bounded_text(&line, MAX_CONTEXT_LINE_CHARS));
            if !line.trim().is_empty() {
                lines.push(line);
            }
        }
        if lines.len() > usize::try_from(wanted).unwrap_or(usize::MAX) {
            let drop = lines.len() - usize::try_from(wanted).unwrap_or(usize::MAX);
            lines.drain(..drop);
        }
        // Apply the byte cap from newest to oldest. Capping the forward prefix would
        // preserve old build chatter and discard the error/result that just arrived.
        let mut newest = Vec::with_capacity(lines.len());
        let mut bytes = 0usize;
        for line in lines.into_iter().rev() {
            if bytes.saturating_add(line.len() + 1) > MAX_CONTEXT_BYTES {
                break;
            }
            bytes = bytes.saturating_add(line.len() + 1);
            newest.push(line);
        }
        for line in newest.into_iter().rev() {
            if !self.recent_output.is_empty() {
                self.recent_output.push('\n');
            }
            self.recent_output.push_str(&line);
        }
    }
}

#[derive(Debug)]
struct Entry {
    deterministic: String,
    activity: String,
    revision: u64,
    semantic: SemanticStamp,
    session_epoch: u64,
    generation: u64,
    authority_epoch: u64,
    config_fingerprint: u64,
    last_request: Option<Instant>,
    next_refresh: Option<Instant>,
    backoff_until: Option<Instant>,
    failure_count: u32,
    /// Forces the next admissible request past the identical-snapshot dedup.
    /// Set at semantic boundaries, provider failures, managed-runtime exits, and
    /// authority transitions — every state where re-running inference over
    /// unchanged content is still meaningful; cleared when a request is queued.
    dirty: bool,
    /// [`snapshot_content_hash`] of the last queued request's snapshot. A timer
    /// refresh whose snapshot hashes identically (and whose entry is not
    /// `dirty`) would send a byte-identical prompt, so it is skipped.
    last_dispatched_snapshot: Option<u64>,
    last_error: Option<String>,
    /// The description this session would present at a settled PROMPT ("Ready
    /// in aterm"), captured from the same snapshot as `deterministic`. This is
    /// what an [`ActivityState::Entering`] claim decays TO once the phase
    /// classifier publishes `Idle` — see [`Coordinator::note_phase_settled`].
    prompt_fallback: String,
    /// The phase classifier's settled-idle verdict for this session, pushed by
    /// the host at publish time. `observe` runs only on OUTPUT wakes, so a
    /// half-typed command that is then abandoned would otherwise hold "Typing a
    /// command" in the titlebar forever while `status` honestly says
    /// `phase=idle` for minutes; this flag is how the presented subject follows
    /// the settled phase instead. Cleared by the next semantic boundary (fresh
    /// output is fresh evidence) as well as by a non-idle publish.
    settled_idle: bool,
}

/// Exact resolved inference authority. Equality, rather than only a hash, detects
/// every settings transition; the monotonic epoch below prevents A→Off→A ABA.
#[derive(Clone, Debug, PartialEq, Eq)]
struct AuthorityKey {
    enabled: bool,
    provider: TitleSummaryProvider,
    model: String,
    endpoint: Option<String>,
    endpoint_origin: EndpointOrigin,
    token_file: Option<String>,
    interval_seconds: u64,
    context_lines: usize,
    include_output: bool,
    allow_remote: bool,
    timeout_seconds: u64,
    proxy_mode: TitleSummaryProxyMode,
    ca_file: Option<String>,
}

impl AuthorityKey {
    fn resolve(config: &Config) -> Self {
        Self {
            enabled: config.descriptive_titles_or_default(),
            provider: config.title_summary_provider_or_default(),
            model: config.title_summary_model_or_default().to_string(),
            endpoint: config
                .title_summary_endpoint_or_default()
                .map(str::to_owned),
            endpoint_origin: endpoint_origin(config),
            token_file: config.title_summary_token_file().map(str::to_owned),
            interval_seconds: config.title_summary_interval_seconds_or_default(),
            context_lines: config.title_summary_context_lines_or_default(),
            include_output: config.title_summary_include_output_or_default(),
            allow_remote: config.title_summary_allow_remote_or_default(),
            timeout_seconds: config.title_summary_timeout_seconds_or_default(),
            proxy_mode: config.title_summary_proxy_mode_or_default(),
            ca_file: config.title_summary_ca_file().map(str::to_owned),
        }
    }

    fn inference_enabled(&self) -> bool {
        self.enabled
            && matches!(
                self.provider,
                TitleSummaryProvider::Ollama | TitleSummaryProvider::OpenAiCompatible
            )
    }
}

/// UI-thread owner for live descriptions. The UI only performs bounded snapshots,
/// map updates, and nonblocking channel operations; HTTP and token-file IO live on
/// the single worker.
pub(crate) struct Coordinator {
    entries: HashMap<u64, Entry>,
    retries: HashMap<u64, Instant>,
    /// Consecutive CONTENDED observation attempts per session, driving the retry
    /// backoff in [`Coordinator::defer_observation`]. Cleared on the first success.
    contended: HashMap<u64, u32>,
    /// Due observations already ordered for later event-loop turns. One call pops
    /// at most one item; keeping the remainder here prevents active-session
    /// priority or newly due work from starving the existing batch.
    due_observation_queue: VecDeque<u64>,
    pending: HashMap<u64, Job>,
    in_flight: Option<(u64, u64, u64, u64, u64)>,
    worker: Option<Worker>,
    proxy: Option<EventLoopProxy<Wake>>,
    authority: Option<AuthorityKey>,
    authority_epoch: u64,
    authority_fingerprint: u64,
    session_authorities: HashMap<u64, (u64, Arc<AtomicU64>)>,
    next_session_epoch: u64,
    last_dispatched_session: Option<u64>,
    /// One frontmost session may take the next worker slot, but two priority
    /// dispatches cannot pass a waiting background session.
    priority_pending_session: Option<u64>,
    last_dispatch_was_priority: bool,
    worker_retry_at: Option<Instant>,
    worker_authority_epoch: Arc<AtomicU64>,
    runtime_state: TitleSummaryRuntimeState,
    runtime_locality: TitleSummaryLocality,
    managed_install_present: bool,
    model_ready: bool,
    last_runtime_error: Option<String>,
    /// Endpoint actually used by the latest fresh managed request. Automatic
    /// Ollama leaves configured authority untouched and publishes this only after
    /// its per-process ephemeral listener has been selected.
    runtime_endpoint: Option<String>,
    /// Composed chrome labels keyed by `(session, format/separator flavor)`.
    /// The tab strip and the window titlebar both re-compose on every redraw;
    /// this cache turns an unchanged (title, description) frame into one hash
    /// pass plus a `clone_from`. A `Mutex` (uncontended: the cache is only
    /// touched from the UI thread) because every render path holds `&App`.
    compose_cache: Mutex<HashMap<(u64, u64), ComposedLabel>>,
    /// Count of full label compositions actually performed, so tests can pin
    /// that steady frames are served from the cache or the clean-title fast path.
    #[cfg(test)]
    compose_runs: AtomicU64,
}

/// One cached composed chrome label plus the hash of the exact pure-composition
/// inputs (raw title + resolved description) that produced it. The hash IS the
/// full input key — format and separator live in the map key — so a stale hit
/// is impossible and no invalidation hook is needed beyond session retirement.
struct ComposedLabel {
    input_hash: u64,
    composed: String,
}

struct Worker {
    request_tx: Option<SyncSender<Job>>,
    result_rx: Receiver<WorkerMessage>,
    join: Option<std::thread::JoinHandle<()>>,
    ollama: ManagedOllamaController,
}

impl Worker {
    fn spawn(
        proxy: Option<EventLoopProxy<Wake>>,
        authority_epoch: Arc<AtomicU64>,
        managed_ollama_authority: Option<u64>,
    ) -> std::io::Result<Self> {
        let (request_tx, request_rx) = mpsc::sync_channel::<Job>(1);
        let (result_tx, result_rx) = mpsc::sync_channel::<WorkerMessage>(1);
        let ollama = ManagedOllamaController::new(managed_ollama_authority);
        let worker_ollama = ollama.clone();
        let join = std::thread::Builder::new()
            .name("aterm-title-summary".to_string())
            .spawn(move || {
                worker_loop(request_rx, result_tx, proxy, authority_epoch, worker_ollama);
            })?;
        Ok(Self {
            request_tx: Some(request_tx),
            result_rx,
            join: Some(join),
            ollama,
        })
    }

    fn shutdown(mut self) {
        // Closing the only sender lets the worker finish its current (bounded)
        // request, discard any revoked queued job, and tear down its owned daemon.
        self.ollama.transition_to(None);
        self.request_tx.take();
        if let Some(join) = self.join.take()
            && join.is_finished()
        {
            let _ = join.join();
        }
    }
}

/// The separator a TAB label's two halves are joined with — the one string that
/// tells a composed chip title apart into its clauses.
///
/// Named because two subsystems have to agree on it and neither can check the
/// other at compile time: the composition writes it ([`Coordinator::compose`] /
/// [`Coordinator::compose_label_into`], through `compose_parts`), and the tab
/// strip's label pass reads it back to find where the SUBJECT half of a chip
/// title ends and the state half begins ([`crate::tab_bar::state_clause_bytes`]
/// — a narrow chip that can keep only one of the two must keep the subject).
/// A literal in either place would let the two drift silently, and the drift
/// would show up as chips painting `…a command` again.
///
/// The WINDOW titlebar composes with `" — "` instead; that flavour is not this
/// constant, and nothing reads it apart.
pub(crate) const TAB_LABEL_SEPARATOR: &str = " · ";

/// Strip a `"<state> in <place>"` description down to `"<state>"` when `title`
/// already names that place. Textual and cheap: the state sentence is built by
/// [`description::idle_prompt_description`] from the same cwd the title's own
/// rung uses, so a containment test is the exact question ("does the label say
/// this twice?"). Anything that is not that shape is returned untouched.
fn shed_place_already_in_title<'a>(title: &str, description: &'a str) -> &'a str {
    let shed = shed_place(title, description);
    // AND SHED A BARE "Ready" WHOLE. It is the state every idle tab is in, so it
    // tells them apart from nothing while costing each one the width of
    // " · Ready" — the width the label needed for the directory. Measured: two
    // tabs deep under /tmp both painted `…Ready`, the tail cut having kept the
    // one word every tab shared and dropped the one that differed.
    //
    // Only the bare word goes. A state that says something a title cannot
    // ("Running Rust tests", "Command failed (exit 1)") is exactly what this
    // suffix is for and is untouched, and an empty title keeps its "Ready"
    // rather than falling through to `compose_parts`'s "aterm".
    if shed == description::READY && !title.trim().is_empty() {
        return "";
    }
    shed
}

/// [`shed_place_already_in_title`] without the bare-state rule: `"<state> in
/// <place>"` down to `"<state>"` when `title` already names that place.
fn shed_place<'a>(title: &str, description: &'a str) -> &'a str {
    let Some((state, place)) = description.split_once(" in ") else {
        return description;
    };
    if place.is_empty() || state.is_empty() {
        return description;
    }
    // `~/aterm`, `/home/x/aterm` and a bare `aterm` all count as naming it.
    let names_place = title
        .rsplit(['/', ' ', ':'])
        .any(|token| !token.is_empty() && token == place);
    if names_place { state } else { description }
}

impl Coordinator {
    pub(crate) fn new(proxy: Option<EventLoopProxy<Wake>>) -> Self {
        let worker_authority_epoch = Arc::new(AtomicU64::new(0));
        Self {
            entries: HashMap::new(),
            retries: HashMap::new(),
            contended: HashMap::new(),
            due_observation_queue: VecDeque::new(),
            pending: HashMap::new(),
            in_flight: None,
            worker: None,
            proxy,
            authority: None,
            authority_epoch: 0,
            authority_fingerprint: 0,
            session_authorities: HashMap::new(),
            next_session_epoch: 1,
            last_dispatched_session: None,
            priority_pending_session: None,
            last_dispatch_was_priority: false,
            worker_retry_at: None,
            worker_authority_epoch,
            runtime_state: TitleSummaryRuntimeState::Idle,
            runtime_locality: TitleSummaryLocality::NotApplicable,
            managed_install_present: managed_ollama_paths()
                .is_some_and(|(binary, _)| binary.is_file()),
            model_ready: false,
            last_runtime_error: None,
            runtime_endpoint: None,
            compose_cache: Mutex::new(HashMap::new()),
            #[cfg(test)]
            compose_runs: AtomicU64::new(0),
        }
    }

    /// Observe one output wake while the caller holds a successful `try_lock` on the
    /// producing terminal. Returns true when the immediate description changed.
    pub(crate) fn observe(
        &mut self,
        session: u64,
        term: &Terminal,
        config: &Config,
        active: bool,
        now: Instant,
    ) -> bool {
        if self.authority.is_none() {
            self.sync_authority(config);
        }
        let Some(authority) = self.authority.as_ref() else {
            return false;
        };
        let provider = authority.provider;
        let descriptive_enabled = authority.enabled;
        let interval = Duration::from_secs(authority.interval_seconds);
        let include_output = authority.include_output;
        let context_lines = authority.context_lines;
        if !descriptive_enabled || provider == TitleSummaryProvider::Off {
            return false;
        }
        let fingerprint = self.authority_fingerprint;
        let semantic = semantic_stamp(term);
        let authority_changed = self.entries.get(&session).is_some_and(|entry| {
            entry.authority_epoch != self.authority_epoch || entry.config_fingerprint != fingerprint
        });
        let boundary = self
            .entries
            .get(&session)
            .is_none_or(|entry| entry.semantic != semantic || authority_changed);
        let model_provider = matches!(
            provider,
            TitleSummaryProvider::Ollama | TitleSummaryProvider::OpenAiCompatible
        );
        let refresh_due = self
            .entries
            .get(&session)
            .and_then(|entry| entry.next_refresh)
            .is_some_and(|deadline| now >= deadline);
        let interval_allows = self.entries.get(&session).is_none_or(|entry| {
            entry
                .last_request
                .is_none_or(|last| now.saturating_duration_since(last) >= interval)
        });
        let backoff_allows = self
            .entries
            .get(&session)
            .and_then(|entry| entry.backoff_until)
            .is_none_or(|deadline| now >= deadline);
        // A semantic boundary is a dirty signal, not permission to evade the
        // configured minimum. The same deadline drives quiet periodic refresh.
        let should_request =
            model_provider && (boundary || refresh_due) && interval_allows && backoff_allows;
        // Hot path: an output burst within the same command and before the next
        // configured refresh does zero owned text work and no context-row reads.
        if !boundary && !refresh_due {
            return false;
        }

        let (mut snapshot, captured_semantic) = Snapshot::metadata(term);
        debug_assert_eq!(semantic, captured_semantic);
        let immediate = deterministic_description(&snapshot);
        let (session_epoch, session_authority) = self.session_authority(session);
        if boundary {
            // A boundary revokes the prior snapshot even when the configured
            // minimum interval prevents admitting its replacement yet. Keep an
            // actually running worker in the single-flight lane, but make its
            // completion stale by advancing the session generation. Captured work
            // that has not crossed the worker channel is destroyed immediately.
            self.pending.remove(&session);
            if self.priority_pending_session == Some(session) {
                self.priority_pending_session = None;
            }
        }
        let entry = self.entries.entry(session).or_insert_with(|| Entry {
            deterministic: immediate.clone(),
            activity: String::new(),
            revision: 1,
            semantic: semantic.clone(),
            session_epoch,
            generation: 0,
            authority_epoch: self.authority_epoch,
            config_fingerprint: fingerprint,
            last_request: None,
            next_refresh: None,
            backoff_until: None,
            failure_count: 0,
            dirty: true,
            last_dispatched_snapshot: None,
            last_error: None,
            prompt_fallback: String::new(),
            settled_idle: false,
        });
        if boundary {
            entry.generation = entry.generation.saturating_add(1);
        }
        // `settled_idle` is deliberately NOT cleared on a boundary: the phase
        // classifier is the one idleness authority, and its published verdict is
        // pushed here by the host both at publish edges
        // (`refresh_session_status_chrome`) and after every observation
        // (`note_title_activity`). An early cut cleared the flag on semantic
        // boundaries "as fresh activity evidence" — and the live repro showed
        // why that is wrong: a sub-dwell blip (type + run a one-liner) never
        // re-publishes the unchanged `Idle`, so the cleared flag had no edge to
        // restore it and the decayed subject stuck at "Typing a command" again,
        // the very defect this exists to close.
        entry.semantic = semantic;
        entry.authority_epoch = self.authority_epoch;
        entry.config_fingerprint = fingerprint;
        if authority_changed {
            entry.last_error = None;
        }
        entry.deterministic.clone_from(&immediate);
        entry.prompt_fallback = idle_prompt_description(&snapshot);
        entry.dirty |= boundary;
        // A model refinement remains visible through ordinary output and a periodic
        // refresh. Reset it only at a semantic/config boundary (or when the selected
        // provider is deterministic), otherwise the chrome would visibly flash back
        // to built-in wording every time a replacement request starts.
        let reset_to_immediate =
            should_reset_description(provider, boundary, entry.activity.is_empty());
        let changed = reset_to_immediate && entry.activity != immediate;
        if reset_to_immediate {
            entry.activity.clone_from(&immediate);
            if changed {
                entry.revision = entry.revision.saturating_add(1);
            }
        }

        let mut queued = None;
        if should_request {
            if let Some(settings) = provider_settings(config) {
                if include_output {
                    snapshot.capture_recent_output(term, context_lines);
                }
                let content = snapshot_content_hash(&snapshot);
                if !boundary && !entry.dirty && entry.last_dispatched_snapshot == Some(content) {
                    // An idle terminal reproduces byte-identical prompt content
                    // every interval, and an identical prompt can only re-derive
                    // the answer already on screen. Skip the redundant inference
                    // and re-arm the periodic check; `dirty` marks the entries
                    // whose next admissible wake must dispatch regardless.
                    entry.next_refresh = Some(now + interval);
                } else {
                    // A semantic boundary already advanced the generation above. A
                    // timer refresh has no boundary of its own, so it advances here
                    // to supersede the previous periodic request.
                    if !boundary {
                        entry.generation = entry.generation.saturating_add(1);
                    }
                    entry.last_request = Some(now);
                    entry.next_refresh = Some(now + interval);
                    entry.backoff_until = None;
                    entry.dirty = false;
                    entry.last_dispatched_snapshot = Some(content);
                    queued = Some(Job {
                        session,
                        session_epoch,
                        session_authority,
                        generation: entry.generation,
                        authority_epoch: self.authority_epoch,
                        config_fingerprint: fingerprint,
                        settings,
                        snapshot,
                    });
                }
            } else {
                let error = "smart-title endpoint is not configured".to_string();
                entry.last_error = Some(error.clone());
                entry.failure_count = entry.failure_count.saturating_add(1);
                let retry = now + backoff_delay(entry.failure_count, session);
                entry.backoff_until = Some(retry);
                entry.next_refresh = Some(retry);
                self.runtime_state = TitleSummaryRuntimeState::Error;
                self.last_runtime_error = Some(error);
            }
        } else if model_provider {
            // A due wake gated by the minimum interval or an active backoff must
            // re-arm `next_refresh` to the next ADMISSIBLE instant — for the timer
            // case as much as for a boundary. Leaving an elapsed deadline behind
            // makes `next_retry()` feed the past instant into `about_to_wait`'s
            // `WaitUntil`, which fires immediately: the event loop busy-spins
            // through full snapshots until the gate finally opens.
            let interval_deadline = entry.last_request.map_or(now, |last| last + interval);
            let deadline = entry
                .backoff_until
                .map_or(interval_deadline, |backoff| backoff.max(interval_deadline));
            debug_assert!(
                deadline > now,
                "a gated refresh implies a closed interval/backoff gate, whose \
                 deadline lies strictly in the future"
            );
            entry.next_refresh = Some(deadline);
        }
        if let Some(job) = queued {
            // Replacement is latest-wins for this session; other sessions retain
            // their independent slot and are served round-robin.
            self.pending.insert(session, job);
            if active {
                self.priority_pending_session = Some(session);
            } else if self.priority_pending_session == Some(session) {
                self.priority_pending_session = None;
            }
            self.dispatch_next();
            self.reconcile_starting_state();
        }
        changed
    }

    fn reconcile_starting_state(&mut self) {
        if self.runtime_state != TitleSummaryRuntimeState::Starting
            || self.in_flight.is_some()
            || !self.pending.is_empty()
        {
            return;
        }
        self.runtime_state = baseline_runtime_state(self.authority.as_ref());
    }

    /// Drain all completed work without blocking. A result is applied only when its
    /// session generation and full provider configuration still match the latest UI
    /// state. Returns the sessions whose visible description changed.
    pub(crate) fn poll(&mut self, config: &Config) -> Vec<u64> {
        if self.authority.is_none() {
            self.sync_authority(config);
        }
        let current_fingerprint = self.authority_fingerprint;
        let enabled = self
            .authority
            .as_ref()
            .is_some_and(AuthorityKey::inference_enabled);
        let minimum_interval = Duration::from_secs(
            self.authority
                .as_ref()
                .map_or(1, |authority| authority.interval_seconds),
        );
        let mut changed = Vec::new();
        let mut completed = Vec::new();
        let mut runtime_exits = Vec::new();
        let mut worker_disconnected = false;
        if let Some(worker) = self.worker.as_ref() {
            loop {
                match worker.result_rx.try_recv() {
                    Ok(WorkerMessage::Result(result)) => completed.push(result),
                    Ok(WorkerMessage::ManagedRuntimeExited(exited)) => runtime_exits.push(exited),
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        worker_disconnected = true;
                        break;
                    }
                }
            }
        }
        for result in completed {
            if self.in_flight
                == Some((
                    result.session,
                    result.session_epoch,
                    result.generation,
                    result.authority_epoch,
                    result.config_fingerprint,
                ))
            {
                self.in_flight = None;
            }
            let Some(entry) = self.entries.get_mut(&result.session) else {
                self.dispatch_next();
                continue;
            };
            let fresh = result.session_epoch == entry.session_epoch
                && completion_is_fresh(
                    enabled,
                    AuthorityStamp {
                        epoch: self.authority_epoch,
                        fingerprint: current_fingerprint,
                    },
                    RequestStamp {
                        generation: entry.generation,
                        authority: AuthorityStamp {
                            epoch: entry.authority_epoch,
                            fingerprint: entry.config_fingerprint,
                        },
                    },
                    RequestStamp {
                        generation: result.generation,
                        authority: AuthorityStamp {
                            epoch: result.authority_epoch,
                            fingerprint: result.config_fingerprint,
                        },
                    },
                );
            if fresh {
                self.runtime_endpoint = result.effective_endpoint.clone();
                match result.result {
                    Ok(activity) => {
                        if entry.activity != activity {
                            entry.activity = activity;
                            entry.revision = entry.revision.saturating_add(1);
                            changed.push(result.session);
                        }
                        entry.last_error = None;
                        entry.failure_count = 0;
                        entry.backoff_until = None;
                        self.runtime_state = TitleSummaryRuntimeState::Ready;
                        self.runtime_locality = result.locality;
                        self.managed_install_present = result.managed_install_present;
                        self.model_ready = true;
                        self.last_runtime_error = None;
                    }
                    Err(error) => {
                        // Log each distinct failure once per session/config. The useful
                        // deterministic description remains visible and is never replaced
                        // with provider diagnostics.
                        if entry.last_error.as_deref() != Some(error.as_str()) {
                            eprintln!("aterm-gui: smart title provider: {error}");
                            entry.last_error = Some(error.clone());
                        }
                        entry.failure_count = entry.failure_count.saturating_add(1);
                        let now = Instant::now();
                        let retry = (now + backoff_delay(entry.failure_count, result.session)).max(
                            entry
                                .last_request
                                .map_or(now, |last| last + minimum_interval),
                        );
                        entry.backoff_until = Some(retry);
                        entry.next_refresh = Some(retry);
                        entry.dirty = true;
                        self.runtime_state = TitleSummaryRuntimeState::BackingOff;
                        self.runtime_locality = result.locality;
                        self.managed_install_present = result.managed_install_present;
                        self.model_ready = false;
                        self.last_runtime_error = Some(error);
                    }
                }
            }
            self.dispatch_next();
        }
        for exited in runtime_exits {
            if exited.authority_epoch == self.authority_epoch
                && self.runtime_endpoint.as_deref() == Some(exited.endpoint.as_str())
            {
                self.runtime_endpoint = None;
                self.runtime_locality = self.configured_runtime_locality();
                self.model_ready = false;
                self.runtime_state = TitleSummaryRuntimeState::Error;
                self.last_runtime_error =
                    Some("managed Ollama exited unexpectedly; relaunch pending".to_string());
                let now = Instant::now();
                for entry in self.entries.values_mut() {
                    entry.dirty = true;
                    entry.next_refresh = Some(now);
                }
            }
        }
        if worker_disconnected {
            self.handle_worker_disconnect();
        }
        self.reconcile_starting_state();
        changed
    }

    pub(crate) fn retire(&mut self, session: u64) {
        self.entries.remove(&session);
        self.compose_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|(owner, _), _| *owner != session);
        self.due_observation_queue
            .retain(|queued| *queued != session);
        if !retry_pending_after(ObservationRetryTransition::Retired) {
            self.retries.remove(&session);
        }
        // The contended-observation strike count is per-session state that must
        // die with the session; a session that happened to be contended when it
        // closed would otherwise leave its entry here for the life of the
        // process.
        self.contended.remove(&session);
        self.pending.remove(&session);
        if self.priority_pending_session == Some(session) {
            self.priority_pending_session = None;
        }
        if let Some((epoch, authority)) = self.session_authorities.remove(&session) {
            authority.store(epoch.saturating_add(1), Ordering::Release);
        }
        if self.in_flight.is_some_and(|identity| identity.0 == session) {
            // The worker still completes serially, but this session no longer owns
            // the UI lane; its cancellation epoch prevents further I/O/publication.
            self.in_flight = None;
        }
        if self.entries.is_empty() {
            // The managed warm-up pins model weights for the daemon lifetime.
            // Once the final terminal retires there is no reason to retain either
            // the process or its private state root; a later session gets a fresh
            // worker/authority controller.
            if let Some(worker) = self.worker.take() {
                worker.shutdown();
            }
            self.runtime_endpoint = None;
            self.runtime_locality = self.configured_runtime_locality();
            self.model_ready = false;
            self.runtime_state = baseline_runtime_state(self.authority.as_ref());
        } else {
            self.dispatch_next();
        }
        self.reconcile_starting_state();
    }

    pub(crate) fn reconfigure(&mut self, config: &Config) -> bool {
        self.sync_authority(config)
    }

    /// Current generated activity, independent of authored session metadata.
    ///
    /// SETTLED-PHASE DECAY (frame audit #3): an [`ActivityState::Entering`]
    /// subject ("Typing a command" / "Typing cargo") is only an honest claim
    /// while typing is plausibly ongoing. `observe` runs on OUTPUT wakes alone,
    /// so a half-typed, abandoned command line freezes that claim in the window
    /// title for as long as the shell sits untouched — minutes after `status`
    /// started answering `phase=idle`. Once the host pushes the classifier's
    /// settled-idle verdict ([`Self::note_phase_settled`]), the Entering claim
    /// presents as the session's prompt-state description instead ("Ready in
    /// aterm"). Read-time, not stored: the entry keeps its real state, so the
    /// subject snaps back the moment either the flag clears or fresh output
    /// moves the block.
    pub(crate) fn activity<'a>(&'a self, session: u64, config: &Config) -> Option<&'a str> {
        if !smart_titles_enabled(config) {
            return None;
        }
        self.entries.get(&session).map(|entry| {
            if entry.settled_idle && entry.semantic.block_state == ActivityState::Entering {
                return if entry.prompt_fallback.is_empty() {
                    "Ready"
                } else {
                    entry.prompt_fallback.as_str()
                };
            }
            if entry.authority_epoch == self.authority_epoch {
                entry.activity.as_str()
            } else {
                entry.deterministic.as_str()
            }
        })
    }

    /// Push the phase classifier's settled verdict for one session: `idle` is
    /// true exactly while the published record is a SETTLED `Idle`
    /// ([`crate::session_status::Status::settled_idle`]) — an `Idle` still
    /// carrying live keystroke echo pushes `false`, which is what lets the
    /// typing subject show WHILE the user is typing. Returns `true` when the
    /// PRESENTED subject changed (the Entering→prompt decay engaged or
    /// released), so the caller can refresh the title/tab chrome it owns; a
    /// verdict that changes nothing visible is free. See [`Self::activity`] for
    /// the decay itself.
    pub(crate) fn note_phase_settled(&mut self, session: u64, idle: bool) -> bool {
        let Some(entry) = self.entries.get_mut(&session) else {
            return false;
        };
        if entry.settled_idle == idle {
            return false;
        }
        entry.settled_idle = idle;
        if entry.semantic.block_state == ActivityState::Entering {
            // The presentation moved: bump the revision the native-chrome caches
            // key on, exactly as a model refinement does.
            entry.revision = entry.revision.saturating_add(1);
            return true;
        }
        false
    }

    /// Monotonic while a session is live; suitable for native-chrome cache keys.
    pub(crate) fn activity_revision(&self, session: u64) -> u64 {
        self.entries.get(&session).map_or(0, |entry| entry.revision)
    }

    #[cfg(test)]
    pub(crate) fn set_test_activity(&mut self, session: u64, activity: &str) {
        let entry = self
            .entries
            .get_mut(&session)
            .expect("test activity requires an observed session");
        entry.activity = activity.to_string();
        entry.revision = entry.revision.saturating_add(1);
    }

    /// Whether a session is already tracked or has a scheduled first observation.
    /// App-level quiet-session discovery uses this to seed newly inserted/restored
    /// pool members without repeatedly bypassing a contended-lock retry deadline.
    pub(crate) fn tracks_session(&self, session: u64) -> bool {
        self.entries.contains_key(&session)
            || self.pending.contains_key(&session)
            || self.in_flight.is_some_and(|identity| identity.0 == session)
            || self.retries.contains_key(&session)
            || self.due_observation_queue.contains(&session)
            || self.session_authorities.contains_key(&session)
    }

    pub(crate) fn health(&self, now: Instant, config: &Config) -> TitleSummaryHealth {
        let fallback;
        let authority = if let Some(authority) = self.authority.as_ref() {
            authority
        } else {
            fallback = AuthorityKey::resolve(config);
            &fallback
        };
        let provider = authority.provider;
        let inference = matches!(
            provider,
            TitleSummaryProvider::Ollama | TitleSummaryProvider::OpenAiCompatible
        );
        let next_retry_after = self
            .next_error_retry()
            .map(|deadline| display_countdown(deadline, now));
        let next_refresh_after = self
            .next_routine_refresh()
            .map(|deadline| display_countdown(deadline, now));
        let state = if self.authority.is_none() {
            baseline_runtime_state(Some(authority))
        } else {
            self.runtime_state
        };
        let transport = authority.endpoint.as_deref().map(|endpoint| {
            effective_transport(
                provider,
                endpoint,
                authority.proxy_mode,
                authority.ca_file.as_deref(),
            )
        });
        TitleSummaryHealth {
            state,
            provider,
            model: inference.then(|| authority.model.clone()),
            endpoint: if authority.endpoint_origin == EndpointOrigin::AutomaticManaged {
                self.runtime_endpoint.clone()
            } else {
                authority.endpoint.clone()
            },
            locality: if self.authority.is_some() {
                self.runtime_locality
            } else {
                configured_locality(authority)
            },
            managed_install_present: self.managed_install_present,
            model_ready: self.model_ready,
            last_error: self.last_runtime_error.clone(),
            next_retry_after,
            next_refresh_after,
            timeout: inference.then(|| Duration::from_secs(authority.timeout_seconds)),
            proxy_mode: inference.then(|| {
                if provider == TitleSummaryProvider::OpenAiCompatible {
                    transport
                        .as_ref()
                        .map_or(authority.proxy_mode, |transport| transport.proxy_mode)
                } else {
                    TitleSummaryProxyMode::Direct
                }
            }),
            ca_file: transport.and_then(|transport| transport.ca_file),
        }
    }

    /// Explicit shutdown is required because the application performs a final
    /// `process::exit` after forgetting its UI graph. Call this before that seam.
    pub(crate) fn shutdown(&mut self) {
        self.authority_epoch = self.authority_epoch.saturating_add(1);
        self.worker_authority_epoch
            .store(self.authority_epoch, Ordering::Release);
        for (session, (epoch, authority)) in &mut self.session_authorities {
            *epoch = epoch.saturating_add(1);
            authority.store(*epoch, Ordering::Release);
            if let Some(entry) = self.entries.get_mut(session) {
                entry.session_epoch = *epoch;
            }
        }
        self.pending.clear();
        self.priority_pending_session = None;
        self.last_dispatch_was_priority = false;
        self.due_observation_queue.clear();
        self.in_flight = None;
        if let Some(worker) = self.worker.take() {
            worker.shutdown();
        }
        self.compose_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        // A cold process replacement can fail and return to this App. Clearing the
        // resolved key makes the ordinary reconfigure path recreate a fresh worker
        // and exact authority instead of leaving shutdown state permanently inert.
        self.authority = None;
        self.runtime_state = TitleSummaryRuntimeState::Disabled;
        self.runtime_locality = TitleSummaryLocality::NotApplicable;
        self.model_ready = false;
        self.runtime_endpoint = None;
    }

    /// Compose the presented label as a fresh `String`. Same cache and fast path
    /// as [`Self::compose_label_into`]; per-frame paths that own a reusable slot
    /// should prefer that method to avoid the return-value allocation.
    ///
    /// TAB flavour callers pass [`TAB_LABEL_SEPARATOR`], the string the tab strip
    /// also reads a composed label back APART with.
    #[must_use]
    pub(crate) fn compose(
        &self,
        session: Option<u64>,
        raw_title: &str,
        authored_description: Option<&str>,
        format: TitleFormat,
        config: &Config,
        separator: &str,
    ) -> String {
        let mut label = raw_title.to_string();
        self.compose_label_into(
            session,
            authored_description,
            format,
            config,
            separator,
            &mut label,
        );
        label
    }

    /// Compose the presented label IN PLACE: `slot` arrives holding the raw
    /// title and leaves holding the composed label.
    ///
    /// Per-frame contract (the tab strip re-labels every tab on every redraw):
    /// - CLEAN FAST PATH: with no description to merge and a title the chrome
    ///   sanitizer would pass through unchanged, the raw title IS the label —
    ///   no sanitize/grapheme pass, no allocation, no cache traffic.
    /// - CACHE HIT: an unchanged (title, description) pair for this session and
    ///   format/separator flavor reuses the stored `String` via `clone_from`
    ///   into the resident slot — no fresh allocation after warmup.
    /// - CACHE MISS: sanitize + grapheme-cap + compose once, then store. Tab
    ///   (`" · "`) and window (`" — "`) flavors occupy separate keys so the two
    ///   per-frame callers cannot evict each other.
    pub(crate) fn compose_label_into(
        &self,
        session: Option<u64>,
        authored_description: Option<&str>,
        format: TitleFormat,
        config: &Config,
        separator: &str,
        slot: &mut String,
    ) {
        let activity = session
            .and_then(|id| self.activity(id, config))
            .unwrap_or_default();
        let description = authored_description
            .map(str::trim)
            .filter(|description| !description.is_empty())
            .unwrap_or(activity);
        // REDUNDANCY SHED. The prompt-state sentence names the place ("Ready in
        // aterm") because a WINDOW title reads as a sentence. A label whose own
        // title already carries that place says it twice — and on a tab chip the
        // second copy is what survives truncation, so the strip painted
        // `…in aterm` while the informative half was cut away. Where the title
        // already answers "where", the description keeps only the state word.
        let description = shed_place_already_in_title(slot, description);
        if description.is_empty() && !slot.is_empty() && title_is_presentation_clean(slot) {
            return;
        }
        let Some(session) = session else {
            // Session-less chrome (native surfaces, tests) has no stable cache
            // identity; compose directly.
            let composed = compose_presentation(slot, description, format, separator);
            #[cfg(test)]
            self.compose_runs.fetch_add(1, Ordering::Relaxed);
            slot.clone_from(&composed);
            return;
        };
        let mut input = std::collections::hash_map::DefaultHasher::new();
        slot.hash(&mut input);
        description.hash(&mut input);
        let input_hash = input.finish();
        let mut flavor = std::collections::hash_map::DefaultHasher::new();
        format.as_str().hash(&mut flavor);
        separator.hash(&mut flavor);
        let key = (session, flavor.finish());
        let mut cache = self
            .compose_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(cached) = cache.get(&key)
            && cached.input_hash == input_hash
        {
            slot.clone_from(&cached.composed);
            return;
        }
        #[cfg(test)]
        self.compose_runs.fetch_add(1, Ordering::Relaxed);
        let composed = compose_presentation(slot, description, format, separator);
        slot.clone_from(&composed);
        cache.insert(
            key,
            ComposedLabel {
                input_hash,
                composed,
            },
        );
    }

    /// Number of full label compositions performed so far (cache misses plus
    /// session-less composes). Frame-path tests pin cache hits with this.
    #[cfg(test)]
    pub(crate) fn compose_runs(&self) -> u64 {
        self.compose_runs.load(Ordering::Relaxed)
    }

    fn dispatch_next(&mut self) {
        self.prune_pending_authority();
        if self.in_flight.is_some() || self.pending.is_empty() {
            return;
        }
        if !self.ensure_worker() {
            return;
        }
        let (next, was_priority) = choose_dispatch_session(
            self.pending.keys().copied(),
            self.last_dispatched_session,
            self.priority_pending_session,
            self.last_dispatch_was_priority,
        );
        let Some(session) = next else { return };
        let Some(job) = self.pending.remove(&session) else {
            return;
        };
        let identity = (
            job.session,
            job.session_epoch,
            job.generation,
            job.authority_epoch,
            job.config_fingerprint,
        );
        let sent = match self
            .worker
            .as_ref()
            .and_then(|worker| worker.request_tx.as_ref())
        {
            Some(tx) => tx.try_send(job),
            None => Err(TrySendError::Disconnected(job)),
        };
        match sent {
            Ok(()) => {
                self.in_flight = Some(identity);
                self.last_dispatched_session = Some(session);
                self.last_dispatch_was_priority = was_priority;
                if was_priority && self.priority_pending_session == Some(session) {
                    self.priority_pending_session = None;
                }
                self.runtime_state = TitleSummaryRuntimeState::Starting;
            }
            Err(TrySendError::Full(job)) => {
                // Defensive: the one-in-flight invariant should make this unreachable,
                // but latest work is retained rather than silently lost.
                self.pending.insert(job.session, job);
            }
            Err(TrySendError::Disconnected(job)) => {
                self.pending.insert(job.session, job);
                self.handle_worker_disconnect();
            }
        }
    }

    fn ensure_worker(&mut self) -> bool {
        if self.worker.is_some() {
            return true;
        }
        let now = Instant::now();
        if self.worker_retry_at.is_some_and(|deadline| now < deadline) {
            return false;
        }
        self.runtime_state = TitleSummaryRuntimeState::Starting;
        let managed_ollama_authority = self.authority.as_ref().and_then(|authority| {
            (authority.inference_enabled() && authority.provider == TitleSummaryProvider::Ollama)
                .then_some(self.authority_epoch)
        });
        match Worker::spawn(
            self.proxy.clone(),
            self.worker_authority_epoch.clone(),
            managed_ollama_authority,
        ) {
            Ok(worker) => {
                self.worker = Some(worker);
                self.worker_retry_at = None;
                self.last_runtime_error = None;
                true
            }
            Err(error) => {
                self.note_worker_start_failure(format!(
                    "could not start smart-title worker: {error}"
                ));
                false
            }
        }
    }

    fn note_worker_start_failure(&mut self, error: String) {
        self.runtime_state = TitleSummaryRuntimeState::Error;
        self.last_runtime_error = Some(error);
        self.runtime_endpoint = None;
        self.worker_retry_at = Some(Instant::now() + Duration::from_secs(5));
    }

    /// Locality implied by the resolved authority alone, with no live endpoint.
    fn configured_runtime_locality(&self) -> TitleSummaryLocality {
        self.authority
            .as_ref()
            .map_or(TitleSummaryLocality::NotApplicable, configured_locality)
    }

    /// Tear down after the worker's channel disconnects: the worker is gone, so
    /// nothing stays in flight and endpoint/locality/model readiness revert to
    /// the configured baseline until a replacement worker proves otherwise.
    fn handle_worker_disconnect(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.shutdown();
        }
        self.in_flight = None;
        self.runtime_endpoint = None;
        self.runtime_locality = self.configured_runtime_locality();
        self.model_ready = false;
        self.note_worker_start_failure("smart-title worker disconnected".to_string());
    }

    /// Remove queued terminal snapshots as soon as provider/privacy authority changes.
    /// An HTTP request already in flight cannot be recalled, but it is result-rejected;
    /// no not-yet-sent snapshot may cross a newly revoked boundary.
    fn prune_pending_authority(&mut self) {
        let enabled = self
            .authority
            .as_ref()
            .is_some_and(AuthorityKey::inference_enabled);
        if !enabled {
            self.pending.clear();
            self.priority_pending_session = None;
            self.last_dispatch_was_priority = false;
            return;
        }
        let fingerprint = self.authority_fingerprint;
        let live: HashSet<u64> = self.entries.keys().copied().collect();
        self.pending.retain(|session, job| {
            live.contains(session)
                && job.authority_epoch == self.authority_epoch
                && job.config_fingerprint == fingerprint
                && job.session_authority.load(Ordering::Acquire) == job.session_epoch
        });
        if self
            .priority_pending_session
            .is_some_and(|session| !self.pending.contains_key(&session))
        {
            self.priority_pending_session = None;
        }
    }

    fn sync_authority(&mut self, config: &Config) -> bool {
        let resolved = AuthorityKey::resolve(config);
        if self.authority.as_ref() != Some(&resolved) {
            let disabled = !resolved.enabled || resolved.provider == TitleSummaryProvider::Off;
            self.authority_epoch = self.authority_epoch.saturating_add(1);
            self.authority_fingerprint = config_fingerprint(config);
            self.worker_authority_epoch
                .store(self.authority_epoch, Ordering::Release);
            // Work not yet sent loses authority immediately. In-flight work carries
            // its old epoch and is rejected both worker-side and at publication.
            self.pending.clear();
            self.priority_pending_session = None;
            self.last_dispatch_was_priority = false;
            self.in_flight = None;
            if disabled && !retry_pending_after(ObservationRetryTransition::Disabled) {
                self.retries.clear();
                self.contended.clear();
                self.due_observation_queue.clear();
            }
            self.worker_retry_at = None;
            self.last_runtime_error = None;
            self.model_ready = false;
            self.runtime_endpoint = None;
            self.runtime_locality = configured_locality(&resolved);
            let inference_enabled = resolved.inference_enabled();
            let now = Instant::now();
            for entry in self.entries.values_mut() {
                // `activity()` immediately falls back to deterministic text while
                // the old entry authority is stale; make that visible cache change
                // observable even before a quiet terminal can be re-snapshotted.
                entry.revision = entry.revision.saturating_add(1);
                entry.dirty = true;
                entry.backoff_until = None;
                entry.failure_count = 0;
                entry.last_error = None;
                entry.next_refresh = inference_enabled.then_some(now);
            }
            self.runtime_state = baseline_runtime_state(Some(&resolved));
            // The daemon belongs to the exact resolved authority, not merely to the
            // Ollama provider name. Revoke synchronously on every transition so an
            // old endpoint/model/privacy epoch cannot retain a process. The
            // controller serializes this with install, closing the late-install race.
            if inference_enabled {
                let managed_epoch = (resolved.inference_enabled()
                    && resolved.provider == TitleSummaryProvider::Ollama)
                    .then_some(self.authority_epoch);
                if let Some(worker) = self.worker.as_ref() {
                    worker.ollama.transition_to(managed_epoch);
                }
            } else if let Some(worker) = self.worker.take() {
                // Builtin/off/master-disabled modes perform no blocking work. Drop
                // the request lane entirely so a disabled feature has no 250 ms
                // reap wakeup, stale network thread, or retained controller.
                worker.shutdown();
            }
            self.authority = Some(resolved);
            true
        } else {
            false
        }
    }

    /// Base retry delay after ONE contended observation.
    const OBSERVE_RETRY_BASE: Duration = Duration::from_millis(4);
    /// Ceiling for the contended-observation backoff. A title description that lands
    /// half a second late during a `cat` is indistinguishable from one that lands 4 ms
    /// late; an event loop that cannot park is not.
    const OBSERVE_RETRY_CAP: Duration = Duration::from_millis(512);

    fn defer_observation(&mut self, session: u64, now: Instant) {
        if !retry_pending_after(ObservationRetryTransition::Contended) {
            return;
        }
        // BACK OFF UNDER SUSTAINED CONTENTION. The observation takes the terminal
        // by try_lock, and under a flood the PTY reader holds that mutex a large
        // fraction of the time — so the try_lock fails on essentially every burst.
        // A fixed 4 ms retry was therefore re-armed hundreds to thousands of times
        // per second and was ALWAYS in the future, which forced the event loop
        // awake at ~250 Hz to service a title heuristic and put a permanent 4 ms
        // floor under whatever deadline the frame pacing had computed. The
        // keystroke path pays for that twice: in loop turns it did not need, and
        // in a pacing deadline chosen by the wrong owner.
        //
        // Doubling per consecutive failure (4, 8, 16 ... 512 ms) makes the cost of
        // contention fall as contention persists, which is exactly backwards from
        // the fixed delay. The first success clears it, so an idle terminal is
        // still observed promptly.
        let strikes = self.contended.entry(session).or_insert(0);
        *strikes = strikes.saturating_add(1);
        let backoff = Self::OBSERVE_RETRY_BASE
            .saturating_mul(1u32 << (*strikes - 1).min(7))
            .min(Self::OBSERVE_RETRY_CAP);
        let retry_at = now + backoff;
        self.retries.insert(session, retry_at);
        if let Some(entry) = self.entries.get_mut(&session)
            && entry.next_refresh.is_some_and(|deadline| deadline <= now)
        {
            entry.next_refresh = Some(retry_at);
        }
    }

    fn observation_succeeded(&mut self, session: u64) {
        // The lock was free: this session is not contended, so the next failure
        // starts the ladder over rather than inheriting a stale ceiling.
        self.contended.remove(&session);
        if !retry_pending_after(ObservationRetryTransition::Succeeded) {
            self.retries.remove(&session);
        }
    }

    fn due_observations(&mut self, now: Instant, active: Option<u64>) -> Vec<u64> {
        let mut due = HashSet::new();
        due.extend(
            self.retries
                .iter()
                .filter(|(_, retry_at)| now >= **retry_at)
                .map(|(session, _)| *session),
        );
        due.extend(self.entries.iter().filter_map(|(session, entry)| {
            entry
                .next_refresh
                .is_some_and(|deadline| now >= deadline)
                .then_some(*session)
        }));
        self.due_observation_queue
            .retain(|session| due.contains(session));
        let queued: HashSet<u64> = self.due_observation_queue.iter().copied().collect();
        let mut newly_due: Vec<u64> = due
            .into_iter()
            .filter(|session| !queued.contains(session))
            .collect();
        newly_due.sort_unstable();
        // Active-session priority applies only when beginning a fresh batch. Once a
        // batch exists, its remainder retains position and therefore makes progress.
        if self.due_observation_queue.is_empty()
            && let Some(active) = active
            && let Some(position) = newly_due.iter().position(|session| *session == active)
        {
            self.due_observation_queue.push_back(active);
            newly_due.remove(position);
        }
        self.due_observation_queue.extend(newly_due);
        self.due_observation_queue.pop_front().into_iter().collect()
    }

    /// Seed every live session after a newly enabled descriptive-title authority.
    /// The explicit queue order makes the active session first while preserving the
    /// existing one-snapshot-per-event-loop-turn admission bound for the remainder.
    fn schedule_live_observations(
        &mut self,
        sessions: impl IntoIterator<Item = u64>,
        active: Option<u64>,
        now: Instant,
    ) {
        let mut sessions: Vec<u64> = sessions.into_iter().collect();
        sessions.sort_unstable();
        sessions.dedup();
        if let Some(active) = active
            && let Some(position) = sessions.iter().position(|session| *session == active)
        {
            sessions.remove(position);
            sessions.insert(0, active);
        }
        self.retries.clear();
        self.contended.clear();
        self.due_observation_queue.clear();
        for session in &sessions {
            self.retries.insert(*session, now);
        }
        self.due_observation_queue.extend(sessions);
    }

    pub(crate) fn next_retry(&self) -> Option<Instant> {
        self.retries
            .values()
            .copied()
            .chain(self.entries.values().filter_map(|entry| entry.next_refresh))
            .chain(self.worker_retry_at)
            .min()
    }

    fn next_error_retry(&self) -> Option<Instant> {
        self.entries
            .values()
            .filter_map(|entry| entry.backoff_until)
            .chain(self.worker_retry_at)
            .min()
    }

    fn next_routine_refresh(&self) -> Option<Instant> {
        self.entries
            .values()
            .filter(|entry| entry.backoff_until.is_none())
            .filter_map(|entry| entry.next_refresh)
            .min()
    }

    fn retry_dispatch(&mut self, now: Instant) {
        if self.worker_retry_at.is_some_and(|deadline| now >= deadline) {
            self.worker_retry_at = None;
            self.dispatch_next();
        }
    }

    fn session_authority(&mut self, session: u64) -> (u64, Arc<AtomicU64>) {
        if let Some((epoch, authority)) = self.session_authorities.get(&session) {
            return (*epoch, authority.clone());
        }
        let epoch = self.next_session_epoch;
        self.next_session_epoch = self.next_session_epoch.saturating_add(1);
        let authority = Arc::new(AtomicU64::new(epoch));
        self.session_authorities
            .insert(session, (epoch, authority.clone()));
        (epoch, authority)
    }
}

impl Drop for Coordinator {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl App {
    /// Snapshot one producing terminal without ever waiting for its parser mutex.
    /// A changed deterministic description immediately fans out to every tab/window
    /// that labels the session; optional inference is merely queued behind it.
    pub(crate) fn note_title_activity(&mut self, session: u64) {
        if !smart_titles_enabled(&self.config) {
            // This gate precedes both pool lookup and terminal locking. Disabled
            // smart titles cannot add parser-lock traffic, and an old contention
            // retry is retired immediately.
            self.title_summaries.observation_succeeded(session);
            return;
        }
        let active = self
            .frontmost_window
            .and_then(|wid| self.focused_session_id(wid))
            == Some(session);
        let changed = {
            let Some(live) = self.pool.get(session) else {
                self.title_summaries.observation_succeeded(session);
                return;
            };
            let term = match live.term.try_lock() {
                Ok(term) => term,
                Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
                // Another renderer/control/parser access won the mutex race. Defer
                // through the event loop's deadline fold so a one-shot OSC/block
                // transition is never lost, while UI latency still wins.
                Err(std::sync::TryLockError::WouldBlock) => {
                    self.title_summaries
                        .defer_observation(session, Instant::now());
                    return;
                }
            };
            self.title_summaries.observation_succeeded(session);
            self.title_summaries
                .observe(session, &term, &self.config, active, Instant::now())
        };
        // Reconcile the settled-phase decay against the CURRENT published
        // verdict on every observation, not only at publish edges: a session
        // whose entry was created after the classifier settled (or whose
        // sub-dwell activity blip never re-published the unchanged `Idle`)
        // would otherwise never learn it is idle, and the Entering subject
        // would stick — the frame-audit #3 defect, through the side door.
        // SETTLED idle, never bare `Idle`: output wakes are exactly what
        // typing produces, so a bare phase check here re-decayed the typing
        // subject on every keystroke and it never showed live (review finding
        // on the audit's fix — see `Status::settled_idle`).
        let idle = self
            .session_status
            .status(session)
            .is_some_and(crate::session_status::Status::settled_idle);
        let decayed = self.title_summaries.note_phase_settled(session, idle);
        if changed || decayed {
            self.refresh_title_presentation(session);
        }
        self.sync_settings_title_summary_health();
    }

    pub(crate) fn poll_title_summaries(&mut self) {
        let changed = self.title_summaries.poll(&self.config);
        for session in changed {
            self.refresh_title_presentation(session);
        }
        self.sync_settings_title_summary_health();
    }

    pub(crate) fn retry_title_observations(&mut self) {
        let now = Instant::now();
        self.title_summaries.retry_dispatch(now);
        let active = self
            .frontmost_window
            .and_then(|window| self.focused_session_id(window));
        if smart_titles_enabled(&self.config) {
            let mut missing: Vec<u64> = self
                .pool
                .iter()
                .map(|session| session.id)
                .filter(|session| !self.title_summaries.tracks_session(*session))
                .collect();
            missing.sort_unstable();
            if let Some(active) = active
                && let Some(position) = missing.iter().position(|session| *session == active)
            {
                missing.swap(0, position);
            }
            if let Some(session) = missing.first().copied() {
                // Central discovery covers initial, newly-created, restored, and
                // seamlessly adopted quiet sessions. It shares the same one-snapshot
                // per event-loop-turn budget as periodic refreshes.
                self.note_title_activity(session);
                self.sync_settings_title_summary_health();
                return;
            }
        }
        // Exactly one terminal may be snapshotted per event-loop turn. This bounds
        // aggregate scrollback copying and parser-lock residency even when hundreds
        // of quiet periodic refreshes become due together.
        let due = self.title_summaries.due_observations(now, active);
        for session in due {
            self.note_title_activity(session);
        }
        self.sync_settings_title_summary_health();
    }

    pub(crate) fn next_title_summary_retry(&self) -> Option<Instant> {
        let now = Instant::now();
        let scheduled = self.title_summaries.next_retry();
        let missing = smart_titles_enabled(&self.config)
            && self
                .pool
                .iter()
                .any(|session| !self.title_summaries.tracks_session(session.id));
        let deadline = if missing {
            Some(scheduled.map_or(now, |deadline| deadline.min(now)))
        } else {
            scheduled
        };
        // FLOOR AN ALREADY-DUE DEADLINE (busy-rearm audit, item 4). Due-but-
        // unserviced is legal coordinator state — the one-observation-per-turn
        // admission bound, a contended `try_lock`, an untracked session
        // awaiting discovery — but feeding the past instant (or a bare
        // `Instant::now()`, past by the time `about_to_wait` folds it) into
        // `WaitUntil` makes the wake fire immediately, get re-armed, and spin
        // the loop at turn rate until the queue drains. The work happens on
        // the next turn either way; re-time the WAKE to the retry ladder's
        // base so the loop parks between turns instead.
        deadline.map(|at| {
            if at <= now {
                now + Coordinator::OBSERVE_RETRY_BASE
            } else {
                at
            }
        })
    }

    pub(crate) fn retire_title_summary(&mut self, session: u64) {
        self.title_summaries.retire(session);
    }

    pub(crate) fn title_summary_activity(&self, session: u64) -> Option<&str> {
        self.title_summaries.activity(session, &self.config)
    }

    pub(crate) fn title_summary_activity_revision(&self, session: u64) -> u64 {
        self.title_summaries.activity_revision(session)
    }

    pub(crate) fn title_summary_health(&self) -> TitleSummaryHealth {
        self.title_summaries.health(Instant::now(), &self.config)
    }

    #[cfg(test)]
    pub(crate) fn title_summary_tracks_session(&self, session: u64) -> bool {
        self.title_summaries.tracks_session(session)
    }

    pub(crate) fn shutdown_title_summaries(&mut self) {
        self.title_summaries.shutdown();
    }

    /// Apply Settings/config authority immediately, even when every terminal is
    /// idle: revoke queued work and repaint both native tabs and window titles.
    pub(crate) fn reconfigure_title_summaries(&mut self) {
        let authority_changed = self.title_summaries.reconfigure(&self.config);
        let active = self
            .frontmost_window
            .and_then(|wid| self.focused_session_id(wid));
        let windows: Vec<WindowId> = self.windows.keys().copied().collect();
        for wid in windows {
            self.refresh_window_tabs(wid);
            if let Some(window) = self
                .windows
                .get(&wid)
                .and_then(|state| state.os_window.as_ref())
            {
                window.request_redraw();
            }
        }
        // A Settings enable/change must reach quiet background sessions too. Seed
        // all live pool entries into the same bounded retry queue; the active one is
        // observed first and each later event-loop turn admits at most one more.
        if authority_changed && smart_titles_enabled(&self.config) {
            let sessions: Vec<u64> = self.pool.iter().map(|session| session.id).collect();
            self.title_summaries
                .schedule_live_observations(sessions, active, Instant::now());
        }
        self.sync_settings_title_summary_health();
    }

    pub(crate) fn refresh_title_presentation(&mut self, session: u64) {
        let windows = self.windows_with_focused_session(session);
        self.refresh_tab_chrome_windows(windows);
    }

    /// Windows showing this session as some tab's FOCUS — the tabs whose label
    /// and composed tooltip are built from it.
    pub(crate) fn windows_with_focused_session(&self, session: u64) -> Vec<WindowId> {
        self.windows
            .iter()
            .filter(|(_, state)| {
                state.tab_set.tabs().iter().any(|tab| {
                    self.view_store
                        .get(tab.focus)
                        .copied()
                        .and_then(crate::tab_model::View::terminal_session)
                        == Some(session)
                })
            })
            .map(|(wid, _)| *wid)
            .collect()
    }

    /// Rebuild and push each window's tab chrome once, then repaint it.
    pub(crate) fn refresh_tab_chrome_windows(&mut self, windows: Vec<WindowId>) {
        for wid in windows {
            self.refresh_window_tabs(wid);
            if let Some(window) = self
                .windows
                .get(&wid)
                .and_then(|state| state.os_window.as_ref())
            {
                window.request_redraw();
            }
        }
    }
}

fn activity_state(state: BlockState) -> ActivityState {
    match state {
        BlockState::PromptOnly => ActivityState::Prompt,
        BlockState::EnteringCommand => ActivityState::Entering,
        BlockState::Executing => ActivityState::Executing,
        BlockState::Complete => ActivityState::Complete,
        _ => ActivityState::Unknown,
    }
}

/// Allocation-free semantic key for the hot Output-wake gate. Terminal title/cwd/
/// command bytes are hashed while borrowed; the bounded owned snapshot is built only
/// when this key changes or an inference refresh is actually due.
fn semantic_stamp(term: &Terminal) -> SemanticStamp {
    let block = term.current_block().or_else(|| term.all_blocks().last());
    let state = block.map_or(ActivityState::Unknown, |block| activity_state(block.state));
    let command = block
        .and_then(|block| block.commandline.as_deref())
        .unwrap_or_default();
    // Deliberately the RAW engine cwd, NOT the native-converted one: this is a
    // change-detection hash, never displayed, and `native_path` is a pure
    // deterministic function — the native form changes exactly when the raw form
    // does, so hashing the raw bytes is equivalent and skips the only allocation
    // the conversion can cost on this per-Output-wake path.
    let cwd = term
        .current_working_directory()
        .or_else(|| block.and_then(|block| block.working_directory.as_deref()))
        .unwrap_or_default();
    SemanticStamp {
        title_epoch: term.title_epoch(),
        block_id: block.map(|block| block.id),
        block_state: state,
        exit_code: block.and_then(|block| block.exit_code),
        command_hash: hash_semantic_prefix(command),
        cwd_hash: hash_semantic_prefix(cwd),
    }
}

/// Content hash over exactly the [`Snapshot`] fields that reach the provider
/// prompt (see [`snapshot_prompt`]). Two snapshots hashing equal produce
/// byte-identical prompts, which is what makes the timer-refresh dedup sound.
fn snapshot_content_hash(snapshot: &Snapshot) -> u64 {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    snapshot.title.hash(&mut hash);
    snapshot.cwd.hash(&mut hash);
    snapshot.command.hash(&mut hash);
    snapshot.state.hash(&mut hash);
    snapshot.exit_code.hash(&mut hash);
    snapshot.recent_output.hash(&mut hash);
    hash.finish()
}

fn hash_semantic_prefix(value: &str) -> u64 {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    for ch in value
        .chars()
        .filter(|ch| !ch.is_control() && !is_bidi_control(*ch))
        .take(MAX_COMMAND_CHARS)
    {
        hash.write_u32(ch as u32);
    }
    hash.finish()
}

/// The user-facing smart-titles gate: descriptive titles on and a provider
/// selected.
fn smart_titles_enabled(config: &Config) -> bool {
    config.descriptive_titles_or_default()
        && config.title_summary_provider_or_default() != TitleSummaryProvider::Off
}

fn provider_settings(config: &Config) -> Option<ProviderSettings> {
    let provider = config.title_summary_provider_or_default();
    let endpoint_origin = endpoint_origin(config);
    let endpoint = if provider == TitleSummaryProvider::Ollama
        && endpoint_origin == EndpointOrigin::AutomaticManaged
    {
        String::new()
    } else {
        config.title_summary_endpoint_or_default()?.to_string()
    };
    Some(ProviderSettings {
        provider,
        model: config.title_summary_model_or_default().to_string(),
        endpoint,
        endpoint_origin,
        token_file: config.title_summary_token_file().map(str::to_owned),
        allow_remote: config.title_summary_allow_remote_or_default(),
        timeout: Duration::from_secs(config.title_summary_timeout_seconds_or_default()),
        proxy_mode: config.title_summary_proxy_mode_or_default(),
        ca_file: config.title_summary_ca_file().map(str::to_owned),
    })
}

fn endpoint_origin(config: &Config) -> EndpointOrigin {
    let explicitly_configured = config
        .title_summary_endpoint
        .as_deref()
        .is_some_and(|endpoint| !endpoint.trim().is_empty());
    if config.title_summary_provider_or_default() == TitleSummaryProvider::Ollama
        && !explicitly_configured
    {
        EndpointOrigin::AutomaticManaged
    } else {
        EndpointOrigin::Configured
    }
}

fn config_fingerprint(config: &Config) -> u64 {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    config.descriptive_titles_or_default().hash(&mut hash);
    config
        .title_summary_provider_or_default()
        .as_str()
        .hash(&mut hash);
    config.title_summary_model_or_default().hash(&mut hash);
    config.title_summary_endpoint_or_default().hash(&mut hash);
    endpoint_origin(config).hash(&mut hash);
    config.title_summary_token_file().hash(&mut hash);
    config
        .title_summary_interval_seconds_or_default()
        .hash(&mut hash);
    config
        .title_summary_context_lines_or_default()
        .hash(&mut hash);
    config
        .title_summary_include_output_or_default()
        .hash(&mut hash);
    config
        .title_summary_allow_remote_or_default()
        .hash(&mut hash);
    config
        .title_summary_timeout_seconds_or_default()
        .hash(&mut hash);
    config
        .title_summary_proxy_mode_or_default()
        .as_str()
        .hash(&mut hash);
    config.title_summary_ca_file().hash(&mut hash);
    hash.finish()
}

/// Shipping completion guard, kept pure so Tier-1 can bind its exact decision to
/// the derived `TitleSummary` state machine (including negative stale controls).
fn completion_is_fresh(
    enabled: bool,
    current: AuthorityStamp,
    entry: RequestStamp,
    job: RequestStamp,
) -> bool {
    enabled
        && job.generation == entry.generation
        && job.authority == current
        && entry.authority == current
}

/// Shipping projection of the model's semantic `Request` versus timer `Refresh`.
/// A periodic inference request is not itself a presentation boundary.
fn should_reset_description(
    provider: TitleSummaryProvider,
    semantic_boundary: bool,
    description_empty: bool,
) -> bool {
    provider == TitleSummaryProvider::Builtin || semantic_boundary || description_empty
}

fn choose_round_robin(sessions: impl Iterator<Item = u64>, after: Option<u64>) -> Option<u64> {
    let mut first = None;
    let mut next = None;
    for session in sessions {
        first = Some(first.map_or(session, |current: u64| current.min(session)));
        if after.is_some_and(|last| session > last) {
            next = Some(next.map_or(session, |current: u64| current.min(session)));
        }
    }
    next.or(first)
}

fn choose_dispatch_session(
    sessions: impl Iterator<Item = u64>,
    after: Option<u64>,
    priority: Option<u64>,
    last_was_priority: bool,
) -> (Option<u64>, bool) {
    let sessions: Vec<u64> = sessions.collect();
    let priority = priority.filter(|priority| sessions.contains(priority));
    if let Some(priority) = priority
        && !last_was_priority
    {
        return (Some(priority), true);
    }
    // Once a priority job ran, a waiting background job gets the next slot. If
    // there is no background work, the priority session may proceed normally.
    let background_exists = priority.is_some() && sessions.len() > 1;
    let chosen = choose_round_robin(
        sessions
            .iter()
            .copied()
            .filter(|session| !background_exists || Some(*session) != priority),
        after,
    )
    .or_else(|| choose_round_robin(sessions.iter().copied(), after));
    (chosen, chosen.is_some() && chosen == priority)
}

fn backoff_delay(failures: u32, session: u64) -> Duration {
    let exponent = failures.saturating_sub(1).min(7);
    let seconds = 2u64.saturating_pow(exponent).min(MAX_BACKOFF.as_secs());
    // Stable per-session jitter prevents synchronized retry trains while keeping
    // the schedule deterministic and straightforward to model/test.
    let jitter_ms = session.wrapping_mul(1_103_515_245).wrapping_add(12_345) % 251;
    (Duration::from_secs(seconds) + Duration::from_millis(jitter_ms)).min(MAX_BACKOFF)
}

fn display_countdown(deadline: Instant, now: Instant) -> Duration {
    let remaining = deadline.saturating_duration_since(now);
    if remaining.is_zero() {
        return Duration::ZERO;
    }
    // Settings renders whole "about N seconds" values. Ceiling to that same unit
    // keeps Eq-stable health snapshots for the full display interval instead of
    // turning nanosecond drift into a self-sustaining redraw loop.
    Duration::from_secs(
        remaining
            .as_secs()
            .saturating_add(u64::from(remaining.subsec_nanos() != 0)),
    )
}

fn job_is_session_authorized(job: &Job) -> bool {
    job.session_authority.load(Ordering::Acquire) == job.session_epoch
}

fn job_is_authorized(job: &Job, authority_epoch: &AtomicU64) -> bool {
    authority_epoch.load(Ordering::Acquire) == job.authority_epoch && job_is_session_authorized(job)
}

fn cancelled_error() -> String {
    "discarded after smart-title authority was revoked".to_string()
}

fn configured_settings_locality(settings: &ProviderSettings) -> TitleSummaryLocality {
    if settings.endpoint_origin == EndpointOrigin::AutomaticManaged {
        TitleSummaryLocality::NotApplicable
    } else if endpoint_is_loopback(&settings.endpoint) {
        TitleSummaryLocality::UnattestedLoopback
    } else {
        TitleSummaryLocality::Remote
    }
}

fn configured_locality(authority: &AuthorityKey) -> TitleSummaryLocality {
    match authority.provider {
        TitleSummaryProvider::Builtin | TitleSummaryProvider::Off => {
            TitleSummaryLocality::NotApplicable
        }
        TitleSummaryProvider::Ollama
            if authority.endpoint_origin == EndpointOrigin::AutomaticManaged =>
        {
            TitleSummaryLocality::NotApplicable
        }
        _ if authority
            .endpoint
            .as_deref()
            .is_some_and(endpoint_is_loopback) =>
        {
            TitleSummaryLocality::UnattestedLoopback
        }
        _ => TitleSummaryLocality::Remote,
    }
}

/// Runtime state a quiescent coordinator reports for `authority` before (or
/// between) worker activity: structurally Disabled/Builtin, otherwise Idle.
fn baseline_runtime_state(authority: Option<&AuthorityKey>) -> TitleSummaryRuntimeState {
    authority.map_or(TitleSummaryRuntimeState::Disabled, |authority| {
        if !authority.enabled || authority.provider == TitleSummaryProvider::Off {
            TitleSummaryRuntimeState::Disabled
        } else if authority.provider == TitleSummaryProvider::Builtin {
            TitleSummaryRuntimeState::Builtin
        } else {
            TitleSummaryRuntimeState::Idle
        }
    })
}

#[cfg(test)]
mod tests {
    use super::description::*;
    use super::managed_ollama::*;
    use super::redaction::*;
    use super::transport::*;
    use super::*;

    fn snap(command: &str, state: ActivityState, exit_code: Option<i32>) -> Snapshot {
        Snapshot {
            title: "shell".to_string(),
            cwd: "/work/aterm".to_string(),
            command: command.to_string(),
            state,
            exit_code,
            recent_output: String::new(),
        }
    }

    fn install_test_worker(
        coordinator: &mut Coordinator,
    ) -> (Receiver<Job>, SyncSender<WorkerMessage>) {
        let (request_tx, request_rx) = mpsc::sync_channel(1);
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        coordinator.worker = Some(Worker {
            request_tx: Some(request_tx),
            result_rx,
            join: None,
            ollama: ManagedOllamaController::new(None),
        });
        (request_rx, result_tx)
    }

    /// A successful managed-local completion carrying `job`'s exact freshness
    /// stamps, so `poll` applies it.
    fn ok_result_for(job: &Job, text: &str, endpoint: &str) -> WorkerMessage {
        WorkerMessage::Result(WorkerResult {
            session: job.session,
            session_epoch: job.session_epoch,
            generation: job.generation,
            authority_epoch: job.authority_epoch,
            config_fingerprint: job.config_fingerprint,
            result: Ok(text.to_string()),
            locality: TitleSummaryLocality::ManagedLocal,
            effective_endpoint: Some(endpoint.to_string()),
            managed_install_present: true,
        })
    }

    #[cfg(unix)]
    fn spawn_dedicated_test_child() -> std::process::Child {
        let mut command = std::process::Command::new("sleep");
        command.arg("30");
        configure_dedicated_process_session(&mut command);
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap()
    }

    #[cfg(unix)]
    fn spawn_dedicated_group_with_descendant(wait_for_child: bool) -> (std::process::Child, u32) {
        use std::io::BufRead as _;

        let script = if wait_for_child {
            "sleep 30 & child=$!; printf '%s\\n' \"$child\"; wait"
        } else {
            "sleep 30 & child=$!; printf '%s\\n' \"$child\""
        };
        let mut command = std::process::Command::new("/bin/sh");
        command.arg("-c").arg(script);
        configure_dedicated_process_session(&mut command);
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        let mut child = command.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut line = String::new();
        std::io::BufReader::new(stdout)
            .read_line(&mut line)
            .unwrap();
        let descendant = line.trim().parse().unwrap();
        (child, descendant)
    }

    #[cfg(unix)]
    fn process_exists(pid: u32) -> bool {
        // SAFETY: signal 0 performs an existence/permission probe only.
        (unsafe { libc::kill(i32::try_from(pid).unwrap(), 0) }) == 0
    }

    #[derive(Debug)]
    struct CountingTransport {
        buffers: ureq::unversioned::transport::LazyBuffers,
        transmitted: Arc<AtomicU64>,
    }

    impl ureq::unversioned::transport::Transport for CountingTransport {
        fn buffers(&mut self) -> &mut dyn ureq::unversioned::transport::Buffers {
            &mut self.buffers
        }

        fn transmit_output(
            &mut self,
            _amount: usize,
            _timeout: ureq::unversioned::transport::NextTimeout,
        ) -> Result<(), ureq::Error> {
            self.transmitted.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn await_input(
            &mut self,
            _timeout: ureq::unversioned::transport::NextTimeout,
        ) -> Result<bool, ureq::Error> {
            Ok(false)
        }

        fn is_open(&mut self) -> bool {
            true
        }
    }

    #[test]
    fn the_label_says_where_once() {
        // A chip's title already carries the place, so the state sentence
        // sheds it — otherwise truncation keeps the redundant half and the
        // strip paints `…in aterm` (seen on glass). Nothing is left over: a
        // bare "Ready" is what EVERY idle tab would say, so it goes too.
        assert_eq!(
            shed_place("user@m17-tower: ~/aterm", "Ready in aterm"),
            "Ready"
        );
        assert_eq!(
            shed_place_already_in_title("user@m17-tower: ~/aterm", "Ready in aterm"),
            ""
        );
        assert_eq!(shed_place("~/wave/nn", "Ready in nn"), "Ready");
        assert_eq!(shed_place_already_in_title("~/wave/nn", "Ready in nn"), "");
        // A title that does NOT name the place keeps the full sentence: the
        // "where" would otherwise be lost entirely.
        assert_eq!(
            shed_place_already_in_title("cargo build", "Ready in aterm"),
            "Ready in aterm"
        );
        // Not the sentence shape at all — untouched.
        assert_eq!(
            shed_place_already_in_title("~/aterm", "Typing a command"),
            "Typing a command"
        );
        // A state that says something the title cannot is the whole point of
        // the suffix, and survives.
        assert_eq!(
            shed_place_already_in_title("~/aterm", "Running Rust tests"),
            "Running Rust tests"
        );
        assert_eq!(
            shed_place_already_in_title("~/aterm", "Command failed (exit 1)"),
            "Command failed (exit 1)"
        );
        // The bare word, reached without the sentence shape.
        assert_eq!(shed_place_already_in_title("~/aterm", "Ready"), "");
        // ...but a title-less surface keeps it, or `compose_parts` would have
        // nothing left and fall back to the bare product name.
        assert_eq!(shed_place_already_in_title("", "Ready"), "Ready");
        assert_eq!(shed_place_already_in_title("   ", "Ready"), "Ready");
    }

    #[test]
    fn transport_write_boundary_rechecks_global_and_session_authority() {
        use ureq::unversioned::transport::Transport as _;

        let global = Arc::new(AtomicU64::new(7));
        let session = Arc::new(AtomicU64::new(11));
        let writes = Arc::new(AtomicU64::new(0));
        let authority = RequestWriteAuthority {
            global: global.clone(),
            expected_global: 7,
            session: session.clone(),
            expected_session: 11,
        };
        let mut guarded = AuthorityGuardTransport {
            inner: CountingTransport {
                buffers: ureq::unversioned::transport::LazyBuffers::new(128, 128),
                transmitted: writes.clone(),
            },
            authority,
        };
        let timeout = ureq::unversioned::transport::NextTimeout {
            after: ureq::unversioned::transport::time::Duration::from_secs(1),
            reason: ureq::Timeout::SendBody,
        };
        guarded.transmit_output(1, timeout).unwrap();
        assert_eq!(writes.load(Ordering::Relaxed), 1);

        // This state projects DNS/connect/TLS already complete. Revocation is an
        // atomic store and cannot wait on that blocking lane; the next actual
        // transport write is denied before the inner transport sees any bytes.
        global.store(8, Ordering::Release);
        assert!(guarded.transmit_output(1, timeout).is_err());
        assert_eq!(writes.load(Ordering::Relaxed), 1);

        global.store(7, Ordering::Release);
        session.store(12, Ordering::Release);
        assert!(guarded.transmit_output(1, timeout).is_err());
        assert_eq!(writes.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn transport_write_guard_conforms_to_runtime_transmit_transition() {
        let model = aterm_spec::derive::title_summary_runtime_model();
        let started = model.successors("StartWorker", &model.init_state())[0].clone();
        let queued = model.successors("Queue1", &started)[0].clone();
        let dequeued = model.successors("Start", &queued)[0].clone();
        let connected = model.successors("BeginIo", &dequeued)[0].clone();
        assert!(model.action_enabled("Transmit", &connected));
        let retired = model.successors("Retire1", &connected)[0].clone();
        assert!(!model.action_enabled("Transmit", &retired));

        let authority = RequestWriteAuthority {
            global: Arc::new(AtomicU64::new(1)),
            expected_global: 1,
            session: Arc::new(AtomicU64::new(2)),
            expected_session: 1,
        };
        assert!(
            !authority.is_authorized(),
            "the shipping epoch predicate rejects the modeled retired state"
        );

        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let started = buggy.successors("StartWorker", &buggy.init_state())[0].clone();
        let queued = buggy.successors("Queue1", &started)[0].clone();
        let dequeued = buggy.successors("Start", &queued)[0].clone();
        let connected = buggy.successors("BeginIo", &dequeued)[0].clone();
        let retired = buggy.successors("Retire1", &connected)[0].clone();
        let transmitted = buggy.successors("Transmit", &retired)[0].clone();
        assert!(!buggy.check_invariant("NoTransmitAfterRetirement", &transmitted));
    }

    #[test]
    fn deterministic_summaries_cover_running_complete_and_failure() {
        assert_eq!(
            deterministic_description(&snap(
                "cargo test -p aterm-gui",
                ActivityState::Executing,
                None
            )),
            "Running Rust tests"
        );
        assert_eq!(
            deterministic_description(&snap("cargo test", ActivityState::Complete, Some(0))),
            "Tests passed"
        );
        assert_eq!(
            deterministic_description(&snap("cargo build", ActivityState::Complete, Some(101))),
            "The project failed (exit 101)"
        );
        assert_eq!(
            deterministic_description(&snap("", ActivityState::Prompt, None)),
            "Ready in aterm"
        );
    }

    /// THE SEAM TWO SUBSYSTEMS SHARE. This module WRITES a tab label's
    /// separator; the tab strip READS it back, to find where the half that
    /// names the tab ends and the half that says what it is doing begins — a
    /// narrow chip keeps the first and sheds the second
    /// ([`crate::tab_bar::state_clause_bytes`]). Nothing but this test checks
    /// that the two agree, and a silent drift would put `…a command` back on
    /// the chips the strip is there to name.
    ///
    /// Both composed formats, because the claim is not "the title comes first":
    /// it is that the LAST clause is whichever half `tab_title_format` ranks
    /// SECOND, so shedding it always keeps the half the user asked to lead
    /// with.
    #[test]
    fn a_composed_tab_label_splits_back_where_the_strip_looks_for_it() {
        for (format, leading) in [
            (TitleFormat::TitleDescription, "claude"),
            (TitleFormat::DescriptionTitle, "Ready in aterm"),
        ] {
            let composed = compose_parts("claude", "Ready in aterm", format, TAB_LABEL_SEPARATOR);
            let shed = crate::tab_bar::state_clause_bytes(&composed);
            assert!(
                shed > 0,
                "{format:?}: the strip finds no clause in {composed:?}"
            );
            assert_eq!(
                &composed[..composed.len() - shed],
                leading,
                "{format:?}: the strip must shed exactly the half this format \
                 ranks second, out of {composed:?}"
            );
        }
    }

    #[test]
    fn formatter_keeps_title_and_description_independent() {
        assert_eq!(
            compose_parts(
                "vim",
                "Editing release notes",
                TitleFormat::TitleDescription,
                " · "
            ),
            "vim · Editing release notes"
        );
        assert_eq!(
            compose_parts(
                "vim",
                "Editing release notes",
                TitleFormat::DescriptionTitle,
                " — "
            ),
            "Editing release notes — vim"
        );
        assert_eq!(
            compose_parts("vim", "Editing release notes", TitleFormat::Title, " · "),
            "vim"
        );
        assert_eq!(
            compose_parts("", "Running tests", TitleFormat::TitleDescription, " · "),
            "Running tests"
        );
        let coordinator = Coordinator::new(None);
        assert_eq!(
            coordinator.compose(
                None,
                "vim",
                Some("Authored project notes"),
                TitleFormat::Description,
                &Config::default(),
                " · ",
            ),
            "Authored project notes",
            "authored metadata must outrank generated activity"
        );
    }

    #[test]
    fn chrome_projection_caps_authored_text_on_grapheme_boundaries() {
        use aterm_grapheme::GraphemeClusters as _;

        let coordinator = Coordinator::new(None);
        let long = "x".repeat(1024);
        let projected = coordinator.compose(
            None,
            "shell",
            Some(&long),
            TitleFormat::Description,
            &Config::default(),
            " · ",
        );
        assert_eq!(projected.graphemes().count(), 97);
        assert!(projected.ends_with('…'));

        let combining = "e\u{301}".repeat(97);
        let projected = chrome_presentation_text(&combining, 96);
        assert_eq!(projected.graphemes().count(), 97);
        assert_eq!(projected.graphemes().nth(95), Some("e\u{301}"));
        assert!(projected.ends_with('…'));

        let family = "👩\u{200d}👩\u{200d}👧\u{200d}👦";
        let projected = chrome_presentation_text(&family.repeat(97), 96);
        assert_eq!(projected.graphemes().count(), 97);
        assert_eq!(projected.graphemes().nth(95), Some(family));
        assert!(projected.ends_with('…'));
        assert_eq!(
            chrome_presentation_text("  one\n\u{00ad}\u{200b}\u{202e}two\tthree\u{e0061}  ", 96),
            "one two three"
        );
    }

    #[test]
    fn endpoints_are_local_by_default_and_remote_requires_tls_and_consent() {
        assert!(
            validate_endpoint(
                TitleSummaryProvider::Ollama,
                "http://127.0.0.1:11434/api/chat",
                false
            )
            .is_ok()
        );
        assert!(
            validate_endpoint(
                TitleSummaryProvider::Ollama,
                "https://models.example.test/api/chat",
                true
            )
            .is_err()
        );
        assert!(
            validate_endpoint(
                TitleSummaryProvider::OpenAiCompatible,
                "http://models.example.test/v1/chat/completions",
                true
            )
            .is_err()
        );
        assert!(
            validate_endpoint(
                TitleSummaryProvider::OpenAiCompatible,
                "https://models.example.test/v1/chat/completions",
                false
            )
            .is_err()
        );
        assert!(
            validate_endpoint(
                TitleSummaryProvider::OpenAiCompatible,
                "https://models.example.test/v1/chat/completions",
                true
            )
            .is_ok()
        );

        let transport = effective_transport(
            TitleSummaryProvider::OpenAiCompatible,
            "https://127.0.0.1:9443/v1/chat/completions",
            TitleSummaryProxyMode::Environment,
            Some("/private/ca.pem"),
        );
        assert_eq!(transport.proxy_mode, TitleSummaryProxyMode::Direct);
        assert_eq!(transport.ca_file.as_deref(), Some("/private/ca.pem"));
        for endpoint in [
            "https://localhost.:9443/v1/chat/completions",
            "https://foo.localhost:9443/v1/chat/completions",
            "https://foo.localhost.:9443/v1/chat/completions",
            "https://127.0.0.1.:9443/v1/chat/completions",
            "https://[::ffff:127.0.0.1]:9443/v1/chat/completions",
            "https://[::ffff:7f00:1]:9443/v1/chat/completions",
        ] {
            assert!(endpoint_is_loopback(endpoint), "{endpoint}");
            assert_eq!(
                effective_transport(
                    TitleSummaryProvider::OpenAiCompatible,
                    endpoint,
                    TitleSummaryProxyMode::Environment,
                    None,
                )
                .proxy_mode,
                TitleSummaryProxyMode::Direct,
                "DNS-equivalent loopback endpoints must never use a proxy"
            );
        }
        assert_eq!(
            effective_transport(
                TitleSummaryProvider::OpenAiCompatible,
                "https://models.example.test/v1/chat/completions",
                TitleSummaryProxyMode::Environment,
                None,
            )
            .proxy_mode,
            TitleSummaryProxyMode::Environment,
            "only loopback overrides the configured proxy policy"
        );

        let mut coordinator = Coordinator::new(None);
        let config = Config {
            title_summary_provider: Some(TitleSummaryProvider::OpenAiCompatible),
            title_summary_endpoint: Some(
                "https://foo.localhost.:9443/v1/chat/completions".to_string(),
            ),
            title_summary_allow_remote: Some(true),
            title_summary_proxy_mode: Some(TitleSummaryProxyMode::Environment),
            title_summary_ca_file: Some("/private/ca.pem".to_string()),
            ..Config::default()
        };
        coordinator.reconfigure(&config);
        let health = coordinator.health(Instant::now(), &config);
        assert_eq!(health.proxy_mode, Some(transport.proxy_mode));
        assert_eq!(health.ca_file, transport.ca_file);
        assert_eq!(health.locality, TitleSummaryLocality::UnattestedLoopback);

        for endpoint in [
            "https://localhost:9443/v1/chat/completions",
            "https://localhost.:9443/v1/chat/completions",
            "https://foo.localhost:9443/v1/chat/completions",
            "https://foo.localhost.:9443/v1/chat/completions",
            "https://127.42.0.9:9443/v1/chat/completions",
            "https://127.0.0.1.:9443/v1/chat/completions",
            "https://[::1]:9443/v1/chat/completions",
            "https://[::ffff:127.0.0.1]:9443/v1/chat/completions",
        ] {
            assert!(endpoint_is_loopback(endpoint), "{endpoint} is loopback");
            assert!(
                validate_endpoint(TitleSummaryProvider::OpenAiCompatible, endpoint, true).is_ok(),
                "HTTPS loopback remains a valid explicitly trusted OpenAI endpoint"
            );
        }
        for endpoint in [
            "https://127.1:9443/v1/chat/completions",
            "https://127.000.000.001:9443/v1/chat/completions",
            "https://2130706433:9443/v1/chat/completions",
            "https://017700000001:9443/v1/chat/completions",
            "https://0x7f000001:9443/v1/chat/completions",
            "https://127.0x0.0.1:9443/v1/chat/completions",
        ] {
            assert!(
                validate_endpoint(TitleSummaryProvider::OpenAiCompatible, endpoint, true).is_err(),
                "ambiguous numeric host must be rejected before transport: {endpoint}"
            );
            assert!(!endpoint_is_credential_free_absolute_url(endpoint));
        }
        assert!(!endpoint_is_loopback(
            "https://models.example.test/v1/chat/completions"
        ));
        assert!(
            loopback_socket("https://127.0.0.1:11434/api/chat").is_none(),
            "managed Ollama socket ownership remains HTTP-only"
        );
        assert_eq!(
            loopback_socket("http://127.42.0.9:11434/api/chat").map(|(socket, _)| socket),
            Some("127.42.0.9:11434".parse().unwrap()),
            "all of 127/8 is classified and connected as loopback"
        );
        for endpoint in [
            "http://localhost.:11434/api/chat",
            "http://foo.localhost:11434/api/chat",
            "http://127.0.0.1.:11434/api/chat",
            "http://[::1]:11434/api/chat",
            "http://[::ffff:127.0.0.1]:11434/api/chat",
        ] {
            let (socket, _) = loopback_socket(endpoint)
                .unwrap_or_else(|| panic!("accepted loopback must resolve directly: {endpoint}"));
            assert!(
                match socket.ip() {
                    std::net::IpAddr::V4(ip) => ip.is_loopback(),
                    std::net::IpAddr::V6(ip) => ip.is_loopback(),
                },
                "{socket}"
            );
        }
        assert!(
            validate_endpoint(
                TitleSummaryProvider::Ollama,
                "http://127.0.0.1:0/api/chat",
                true,
            )
            .is_err(),
            "configured port zero must never masquerade as automatic selection"
        );
        for endpoint in [
            "https://models.example.test/v1/chat?api-version=2026-01-01",
            "https://models.example.test/v1/chat?api_key=secret",
            "https://models.example.test/v1/chat#token",
        ] {
            let error = validate_endpoint(TitleSummaryProvider::OpenAiCompatible, endpoint, true)
                .unwrap_err();
            assert!(error.contains("query string or fragment"));
            assert!(endpoint_has_query_or_fragment(endpoint));
        }
    }

    #[test]
    fn ca_bundle_io_errors_never_echo_the_configured_value() {
        let pasted = concat!("-----BEGIN ", "PRIVATE KEY-----runtime-secret");
        let error = load_ca_bundle(pasted).expect_err("inline PEM is not a readable path");
        assert!(!error.contains(pasted), "configured value leaked: {error}");
        assert_eq!(error, "could not open the configured CA bundle");
    }

    #[test]
    fn automatic_and_explicit_default_endpoints_are_distinct_authorities() {
        let automatic = Config {
            title_summary_provider: Some(TitleSummaryProvider::Ollama),
            ..Config::default()
        };
        let explicit = Config {
            title_summary_provider: Some(TitleSummaryProvider::Ollama),
            title_summary_endpoint: Some(
                crate::app_config::EXAMPLE_EXPLICIT_OLLAMA_TITLE_SUMMARY_ENDPOINT.to_string(),
            ),
            ..Config::default()
        };
        assert_eq!(
            provider_settings(&automatic).unwrap().endpoint_origin,
            EndpointOrigin::AutomaticManaged
        );
        assert_eq!(
            provider_settings(&explicit).unwrap().endpoint_origin,
            EndpointOrigin::Configured
        );
        assert_ne!(
            AuthorityKey::resolve(&automatic),
            AuthorityKey::resolve(&explicit)
        );
        assert_ne!(
            config_fingerprint(&automatic),
            config_fingerprint(&explicit)
        );

        let mut coordinator = Coordinator::new(None);
        coordinator.reconfigure(&automatic);
        let health = coordinator.health(Instant::now(), &automatic);
        assert_eq!(health.endpoint, None);
        assert_eq!(health.locality, TitleSummaryLocality::NotApplicable);
    }

    #[test]
    fn automatic_endpoint_reservations_are_ephemeral_and_process_distinct() {
        let first = reserve_managed_endpoint().unwrap();
        let second = reserve_managed_endpoint().unwrap();
        assert_ne!(first.target.socket, second.target.socket);
        assert_ne!(first.target.socket.port(), 11_434);
        assert_ne!(second.target.socket.port(), 11_434);
        assert!(first.target.endpoint.ends_with("/api/chat"));
        assert!(second.target.endpoint.ends_with("/api/chat"));
    }

    #[test]
    fn explicit_loopback_endpoint_stays_pinned_and_requires_consent() {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let endpoint = format!("http://{}/api/chat", listener.local_addr().unwrap());
        let authority_epoch = 17;
        let authority = Arc::new(AtomicU64::new(authority_epoch));
        let session_authority = Arc::new(AtomicU64::new(1));
        let make_job = |allow_remote| {
            let config = Config {
                title_summary_provider: Some(TitleSummaryProvider::Ollama),
                title_summary_endpoint: Some(endpoint.clone()),
                title_summary_allow_remote: Some(allow_remote),
                ..Config::default()
            };
            Job {
                session: 1,
                session_epoch: 1,
                session_authority: session_authority.clone(),
                generation: 1,
                authority_epoch,
                config_fingerprint: config_fingerprint(&config),
                settings: provider_settings(&config).unwrap(),
                snapshot: snap("cargo test", ActivityState::Executing, None),
            }
        };
        let controller = ManagedOllamaController::new(Some(authority_epoch));
        let mut ollama = ManagedOllama::new(controller);
        let allowed = ollama.ensure(&make_job(true), &authority).unwrap();
        assert_eq!(allowed.locality, TitleSummaryLocality::UnattestedLoopback);
        assert_eq!(allowed.effective_endpoint, endpoint);
        assert!(allowed.managed_process.is_none());
        assert!(ollama.ensure(&make_job(false), &authority).is_err());
        drop(listener);
    }

    #[test]
    fn model_text_is_single_line_bounded_and_direction_safe() {
        use aterm_grapheme::GraphemeClusters as _;

        let hostile = format!(
            "  Run\u{00ad}ning\n te\u{200b}sts\u{202e}\u{e0061} {}  ",
            "x".repeat(200)
        );
        let normalized = normalize_description(&hostile);
        assert!(!normalized.contains('\n'));
        assert!(!normalized.contains('\u{202e}'));
        assert!(!normalized.contains('\u{00ad}'));
        assert!(!normalized.contains('\u{200b}'));
        assert!(!normalized.contains('\u{e0061}'));
        assert!(normalized.graphemes().count() <= MAX_DESCRIPTION_GRAPHEMES);

        let combining_boundary = format!("{}e\u{301}tail", "x".repeat(95));
        let normalized = normalize_description(&combining_boundary);
        assert_eq!(normalized.graphemes().count(), MAX_DESCRIPTION_GRAPHEMES);
        assert!(normalized.ends_with("e\u{301}"));
        assert!(is_generic_description("Terminal state description."));
        assert!(!is_generic_description("Running Rust unit tests"));
    }

    #[test]
    fn likely_secret_lines_are_removed_before_prompting() {
        let stripe_shaped = concat!("sk_", "live_A1B2C3D4E5F6G7H8I9J0K1L2M3N4");
        assert_eq!(
            redact_context_line("export API_KEY=very-secret"),
            "[redacted potentially sensitive line]"
        );
        assert_eq!(redact_context_line("cargo test"), "cargo test");
        assert_eq!(
            redact_context_line(stripe_shaped),
            "[redacted potentially sensitive line]"
        );
        assert_eq!(
            deterministic_description(&snap(
                "export API_KEY=very-secret",
                ActivityState::Entering,
                None
            )),
            "Typing a command"
        );
        assert!(contains_sensitive_text(&format!(
            "Completed {stripe_shaped}"
        )));

        for secret_line in [
            "DATABASE_URL=postgres://alice:p4ssw0rd@db.example/prod",
            "export REDIS_URL=redis://cache.example/0",
            "connecting to postgres://alice:p4ssw0rd@db.example/prod",
        ] {
            assert!(contains_sensitive_text(secret_line), "missed {secret_line}");
            assert_eq!(
                redact_context_line(secret_line),
                "[redacted potentially sensitive line]"
            );
            assert_eq!(
                validate_provider_activity(secret_line).unwrap_err(),
                "provider returned potentially sensitive text"
            );
        }

        let mut prompt_secret = snap(
            "DATABASE_URL=postgres://alice:p4ssw0rd@db.example/prod",
            ActivityState::Entering,
            None,
        );
        prompt_secret.recent_output =
            "ok\npostgres://alice:p4ssw0rd@db.example/prod\ndone".to_string();
        let prompt = snapshot_prompt(&prompt_secret);
        assert!(!prompt.contains("alice"));
        assert!(!prompt.contains("p4ssw0rd"));
        assert_eq!(
            prompt
                .matches("[redacted potentially sensitive line]")
                .count(),
            2
        );
        assert_eq!(
            deterministic_description(&prompt_secret),
            "Typing a command"
        );

        for command in [
            concat!("sk_", "live_A1B2C3D4E5F6G7H8I9J0K1L2M3N4"),
            concat!("sk_", "live_abcdefghijklmnopqrstuvwxyz0123456789"),
            concat!("sk-", "proj-abcdefghijklmnopqrstuvwxyz0123456789"),
            "API_KEY=very-secret cargo test",
        ] {
            assert_eq!(
                deterministic_description(&snap(command, ActivityState::Executing, None)),
                "Command running"
            );
            assert_eq!(
                deterministic_description(&snap(command, ActivityState::Unknown, None)),
                "Command running"
            );
            assert_eq!(
                deterministic_description(&snap(command, ActivityState::Complete, Some(0))),
                "Command finished"
            );
            assert_eq!(
                deterministic_description(&snap(command, ActivityState::Complete, Some(17))),
                "Command failed (exit 17)"
            );
        }

        for credential in [
            concat!("sk-", "abcdefghijklmnopqrstuvwxyz012345"),
            concat!("sk", "_abcdefghijklmnopqrstuvwxyz012345"),
            "rk-abcdefghijklmnopqrstuvwxyz012345",
            "RK_ABCDEFGHIJKLMNOPQRSTUVWXYZ012345",
            concat!("xoxb-", "1234567890-abcdefghijklmnopqrstuvwxyz"),
            "xoxb_1234567890_abcdefghijklmnopqrstuvwxyz",
            concat!("xoxp-", "1234567890-abcdefghijklmnopqrstuvwxyz"),
            "xoxp_1234567890_abcdefghijklmnopqrstuvwxyz",
            concat!("ghp_", "abcdefghijklmnopqrstuvwxyz0123456789"),
            concat!("github_pat_", "abcdefghijklmnopqrstuvwxyz0123456789"),
            concat!("AKIA", "IOSFODNN7EXAMPLE"),
            "ASIAIOSFODNN7EXAMPLE",
            concat!("AIza", "SyD-abcdefghijklmnopqrstuvwxyz012345"),
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "abcdefghijklmnopqrstuvwxyz0123456789",
            concat!(
                "eyJhbGciOiJIUzI1NiJ9",
                ".eyJzdWIiOiIxMjM0NTY3ODkwIn0",
                ".signature_value"
            ),
        ] {
            assert!(looks_like_raw_credential(credential), "missed {credential}");
            assert!(
                contains_sensitive_text(credential),
                "did not redact {credential}"
            );
            assert_eq!(
                redact_context_line(credential),
                "[redacted potentially sensitive line]"
            );
            assert_eq!(
                validate_provider_activity(credential).unwrap_err(),
                "provider returned potentially sensitive text"
            );
        }
        for benign in [
            "api-version",
            "running-rust-tests",
            "0123456789abcdef",
            "Ready in project2026",
            "three.part.words",
            "risk-analysis",
        ] {
            assert!(
                !looks_like_raw_credential(benign),
                "false positive {benign}"
            );
        }

        let mut cwd_secret = snap("", ActivityState::Prompt, None);
        cwd_secret.cwd = "/work/abcdefghijklmnopqrstuvwxyz0123456789".to_string();
        assert_eq!(deterministic_description(&cwd_secret), "Ready");
        cwd_secret.cwd = "/work/eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.signature_value".to_string();
        assert_eq!(deterministic_description(&cwd_secret), "Ready");
    }

    #[test]
    fn structured_secret_labels_are_identifier_aware_and_bounded() {
        for sensitive in [
            "STRIPE_SECRET = sample-value",
            "export STRIPE_SECRET=sample-value",
            r#"{"STRIPE_SECRET":"sample-value"}"#,
            r#"{"stripeSecret":"sample-value"}"#,
            "dbPassword: sample-value",
            "service_passwd = sample-value",
            "refresh_token: sample-value",
            "api key: sample-value",
            r#"{"private key":"sample-value"}"#,
            "AWS_ACCESS_KEY_ID: sample-value",
            "GOOGLE_APPLICATION_CREDENTIALS=sample-value",
            "authorization: sample-value",
            "DATABASE_URL = sample-value",
        ] {
            assert!(
                has_sensitive_assignment_key(sensitive),
                "missed structured label in {sensitive}"
            );
            assert!(
                contains_sensitive_text(sensitive),
                "did not redact structured label in {sensitive}"
            );
            assert_eq!(
                redact_context_line(sensitive),
                "[redacted potentially sensitive line]"
            );
            let diagnostic = validate_provider_activity(sensitive).unwrap_err();
            assert_eq!(diagnostic, "provider returned potentially sensitive text");
            assert!(!diagnostic.contains("sample-value"));
        }

        for benign in [
            "keyboard_layout=ansi",
            "monkey: banana",
            r#"{"keyboard":"ansi"}"#,
            "tokenizer: enabled",
            "access_tokenizer: enabled",
            "auth_tokenizer: enabled",
            "apikeyboard=true",
            "bearerless=true",
            "passwordless=true",
            "secretary=present",
            "private keyboard: enabled",
            "access keyboard: enabled",
            "api keyboard: enabled",
            "A monkey uses a keyboard",
            "cargo test --package token-parser",
        ] {
            assert!(
                !has_sensitive_assignment_key(benign),
                "identifier false positive in {benign}"
            );
            assert!(
                !contains_sensitive_text(benign),
                "privacy false positive in {benign}"
            );
            assert_eq!(redact_context_line(benign), benign);
        }

        let oversized = "keyboard ".repeat(MAX_SENSITIVE_SCAN_BYTES / 2);
        assert!(oversized.len() > MAX_SENSITIVE_SCAN_BYTES);
        assert!(contains_sensitive_text(&oversized));
        assert_eq!(
            redact_context_line(&oversized),
            "[redacted potentially sensitive line]"
        );

        let excessive_fields = "safe=1:".repeat(MAX_SENSITIVE_FIELDS + 1);
        assert!(has_sensitive_assignment_key(&excessive_fields));
        assert!(contains_sensitive_text(&excessive_fields));
    }

    #[test]
    fn token_file_diagnostics_never_echo_the_configured_path_or_secret_text() {
        let marker = format!(
            "/tmp/aterm-missing-token-sk_live_PRIVATE_{}_do-not-echo",
            std::process::id()
        );
        let error = read_private_token(&marker).unwrap_err();
        assert!(!error.contains(&marker));
        assert!(!error.contains("sk_live_PRIVATE"));
        assert_eq!(error, "could not open the configured token file");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let path = std::env::temp_dir()
                .join(format!("aterm-private-token-error-{}", std::process::id()));
            std::fs::write(&path, [0xff, 0xfe]).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            let configured = path.to_string_lossy().into_owned();
            let error = read_private_token(&configured).unwrap_err();
            assert_eq!(error, "could not read the configured token file");
            assert!(!error.contains(&configured));
            std::fs::remove_file(path).unwrap();
        }
    }

    #[test]
    fn ollama_body_explicitly_disables_thinking() {
        let config = Config {
            title_summary_provider: Some(TitleSummaryProvider::Ollama),
            ..Config::default()
        };
        let settings = provider_settings(&config).unwrap();
        let body = request_body(
            &settings,
            &snap("cargo test", ActivityState::Executing, None),
            false,
        );
        assert_eq!(body.get("think"), Some(&serde_json::Value::Bool(false)));
        assert_eq!(
            body.pointer("/options/num_predict")
                .and_then(|v| v.as_u64()),
            Some(64)
        );
        assert_eq!(body.get("keep_alive").and_then(|v| v.as_str()), Some("10m"));

        let managed = request_body(
            &settings,
            &snap("cargo test", ActivityState::Executing, None),
            true,
        );
        assert_eq!(managed.get("keep_alive").and_then(|v| v.as_i64()), Some(-1));
    }

    #[test]
    fn worker_is_lazy_and_round_robin_does_not_starve_a_pending_session() {
        let coordinator = Coordinator::new(None);
        assert!(coordinator.worker.is_none());
        assert_eq!(choose_round_robin([7, 2, 9].into_iter(), None), Some(2));
        assert_eq!(choose_round_robin([7, 2, 9].into_iter(), Some(2)), Some(7));
        assert_eq!(choose_round_robin([7, 2, 9].into_iter(), Some(9)), Some(2));

        assert_eq!(
            choose_dispatch_session([1, 2, 3].into_iter(), None, Some(2), false),
            (Some(2), true),
            "frontmost work gets one immediate slot"
        );
        assert_eq!(
            choose_dispatch_session([1, 2, 3].into_iter(), Some(2), Some(2), true),
            (Some(3), false),
            "a second priority job cannot pass waiting background work"
        );
        assert_eq!(
            choose_dispatch_session([1, 2].into_iter(), Some(3), Some(2), false),
            (Some(2), true),
            "priority rearms only after a background dispatch"
        );
    }

    #[test]
    fn periodic_deadline_is_real_and_semantic_wakes_obey_the_minimum() {
        let mut coordinator = Coordinator::new(None);
        // Hold the lane synthetically so this scheduling test performs no worker or
        // provider I/O while observing the real Coordinator state transitions.
        coordinator.in_flight = Some((99, 99, 99, 99, 99));
        let config = Config {
            title_summary_provider: Some(TitleSummaryProvider::Ollama),
            title_summary_interval_seconds: Some(5),
            ..Config::default()
        };
        let mut term = Terminal::new(2, 20);
        let start = Instant::now();
        coordinator.observe(7, &term, &config, true, start);
        assert_eq!(coordinator.entries[&7].generation, 1);
        assert!(
            !coordinator
                .due_observations(start + Duration::from_secs(4), Some(7))
                .contains(&7)
        );

        coordinator.observe(7, &term, &config, true, start + Duration::from_secs(1));
        assert_eq!(coordinator.entries[&7].generation, 1);
        assert!(
            coordinator
                .due_observations(start + Duration::from_secs(5), Some(7))
                .contains(&7)
        );
        // New output within the same command changes prompt content without a
        // semantic boundary; the due periodic refresh must admit a replacement.
        term.process(b"progress line\r\n");
        coordinator.observe(7, &term, &config, true, start + Duration::from_secs(5));
        assert_eq!(coordinator.entries[&7].generation, 2);
        let health = coordinator.health(start + Duration::from_secs(5), &config);
        assert!(health.next_refresh_after.is_some());
        assert!(health.next_retry_after.is_none());
        coordinator.entries.get_mut(&7).unwrap().backoff_until =
            Some(start + Duration::from_secs(8));
        let health = coordinator.health(start + Duration::from_secs(5), &config);
        assert!(health.next_refresh_after.is_none());
        assert_eq!(health.next_retry_after, Some(Duration::from_secs(3)));
    }

    #[test]
    fn semantic_boundary_inside_minimum_rejects_the_running_a_completion() {
        let mut coordinator = Coordinator::new(None);
        let config = Config {
            title_summary_provider: Some(TitleSummaryProvider::Ollama),
            title_summary_interval_seconds: Some(30),
            ..Config::default()
        };
        assert!(coordinator.reconfigure(&config));
        let (request_rx, result_tx) = install_test_worker(&mut coordinator);
        let start = Instant::now();
        let mut term = Terminal::new(2, 20);
        term.set_title("semantic A");
        coordinator.observe(7, &term, &config, true, start);
        let a = request_rx.try_recv().expect("A entered the worker lane");
        assert_eq!(a.generation, 1);
        assert_eq!(coordinator.in_flight.map(|identity| identity.2), Some(1));

        term.set_title("semantic B");
        coordinator.observe(7, &term, &config, true, start + Duration::from_secs(1));
        assert_eq!(
            coordinator.entries[&7].generation, 2,
            "B revokes A even though B cannot request inside the minimum interval"
        );
        assert!(!coordinator.pending.contains_key(&7));

        let current_endpoint = "http://127.0.0.1:32123/api/chat".to_string();
        coordinator.runtime_endpoint = Some(current_endpoint.clone());

        result_tx
            .send(WorkerMessage::Result(WorkerResult {
                session: a.session,
                session_epoch: a.session_epoch,
                generation: a.generation,
                authority_epoch: a.authority_epoch,
                config_fingerprint: a.config_fingerprint,
                result: Ok("stale semantic A".to_string()),
                locality: TitleSummaryLocality::UnattestedLoopback,
                effective_endpoint: Some("http://127.0.0.1:39999/api/chat".to_string()),
                managed_install_present: false,
            }))
            .unwrap();
        assert!(coordinator.poll(&config).is_empty());
        assert_ne!(coordinator.activity(7, &config), Some("stale semantic A"));
        assert!(coordinator.in_flight.is_none());
        assert_eq!(
            coordinator.health(Instant::now(), &config).endpoint,
            Some(current_endpoint),
            "a stale completion cannot rewrite managed endpoint telemetry"
        );
        assert_ne!(
            coordinator.runtime_state,
            TitleSummaryRuntimeState::Starting,
            "a stale completion with no replacement cannot strand health in Starting"
        );
    }

    #[test]
    fn fresh_automatic_result_publishes_only_the_actual_managed_endpoint() {
        let mut coordinator = Coordinator::new(None);
        let config = Config {
            title_summary_provider: Some(TitleSummaryProvider::Ollama),
            ..Config::default()
        };
        assert!(coordinator.reconfigure(&config));
        let (request_rx, result_tx) = install_test_worker(&mut coordinator);
        let term = Terminal::new(2, 20);
        coordinator.observe(7, &term, &config, true, Instant::now());
        let job = request_rx.try_recv().unwrap();
        let effective = "http://127.0.0.1:32123/api/chat".to_string();
        result_tx
            .send(WorkerMessage::Result(WorkerResult {
                session: job.session,
                session_epoch: job.session_epoch,
                generation: job.generation,
                authority_epoch: job.authority_epoch,
                config_fingerprint: job.config_fingerprint,
                result: Ok("Running focused Rust tests".to_string()),
                locality: TitleSummaryLocality::ManagedLocal,
                effective_endpoint: Some(effective.clone()),
                managed_install_present: true,
            }))
            .unwrap();
        assert_eq!(coordinator.poll(&config), vec![7]);
        let health = coordinator.health(Instant::now(), &config);
        assert_eq!(health.endpoint, Some(effective));
        assert_eq!(health.locality, TitleSummaryLocality::ManagedLocal);
        assert_ne!(
            health.endpoint.as_deref(),
            Some(crate::app_config::EXAMPLE_EXPLICIT_OLLAMA_TITLE_SUMMARY_ENDPOINT)
        );
    }

    /// Regression: a worker that dies WITHOUT emitting a wake is discovered at
    /// dispatch time, not by `poll` — `poll` only runs on `Wake::TitleSummaryReady`,
    /// which a dead worker never sends. That path must clear `model_ready` like
    /// every other teardown, or the Settings health card keeps attesting a model
    /// proven by a worker that no longer exists.
    #[test]
    fn dispatch_time_worker_disconnect_clears_model_ready() {
        let mut coordinator = Coordinator::new(None);
        let config = Config {
            title_summary_provider: Some(TitleSummaryProvider::Ollama),
            ..Config::default()
        };
        assert!(coordinator.reconfigure(&config));
        let (request_rx, result_tx) = install_test_worker(&mut coordinator);
        let term = Terminal::new(2, 20);

        // One success proves the model, so `model_ready` is true going in.
        coordinator.observe(7, &term, &config, true, Instant::now());
        let job = request_rx.try_recv().unwrap();
        result_tx
            .send(ok_result_for(
                &job,
                "Running focused tests",
                "http://127.0.0.1:32123/api/chat",
            ))
            .unwrap();
        assert_eq!(coordinator.poll(&config), vec![7]);
        assert!(
            coordinator.health(Instant::now(), &config).model_ready,
            "a completed request must attest model readiness"
        );

        // The worker dies: its request receiver drops, so the next send is
        // `Disconnected`. No wake is emitted, so `poll` never runs again — the
        // timer-driven dispatch is what discovers it.
        drop(request_rx);
        coordinator.pending.insert(
            7,
            Job {
                session: 7,
                ..job.clone()
            },
        );
        coordinator.dispatch_next();

        assert!(
            coordinator.worker.is_none(),
            "the disconnected worker is torn down at dispatch"
        );
        assert!(
            !coordinator.health(Instant::now(), &config).model_ready,
            "readiness must not outlive the worker that proved it"
        );
    }

    #[test]
    fn idle_managed_runtime_crash_clears_health_and_rearms_live_sessions() {
        let mut coordinator = Coordinator::new(None);
        let config = Config {
            title_summary_provider: Some(TitleSummaryProvider::Ollama),
            ..Config::default()
        };
        assert!(coordinator.reconfigure(&config));
        let (request_rx, result_tx) = install_test_worker(&mut coordinator);
        let term = Terminal::new(2, 20);
        coordinator.observe(7, &term, &config, true, Instant::now());
        let job = request_rx.try_recv().unwrap();
        let endpoint = "http://127.0.0.1:32123/api/chat".to_string();
        result_tx
            .send(WorkerMessage::Result(WorkerResult {
                session: job.session,
                session_epoch: job.session_epoch,
                generation: job.generation,
                authority_epoch: job.authority_epoch,
                config_fingerprint: job.config_fingerprint,
                result: Ok("Running focused tests".to_string()),
                locality: TitleSummaryLocality::ManagedLocal,
                effective_endpoint: Some(endpoint.clone()),
                managed_install_present: true,
            }))
            .unwrap();
        assert_eq!(coordinator.poll(&config), vec![7]);
        assert!(coordinator.in_flight.is_none(), "the daemon is now idle");
        assert_eq!(coordinator.runtime_state, TitleSummaryRuntimeState::Ready);

        result_tx
            .send(WorkerMessage::ManagedRuntimeExited(ManagedRuntimeExit {
                endpoint: endpoint.clone(),
                authority_epoch: job.authority_epoch,
            }))
            .unwrap();
        assert!(coordinator.poll(&config).is_empty());
        let health = coordinator.health(Instant::now(), &config);
        assert_eq!(health.state, TitleSummaryRuntimeState::Error);
        assert_eq!(health.endpoint, None);
        assert_eq!(health.locality, TitleSummaryLocality::NotApplicable);
        assert!(!health.model_ready);
        assert!(coordinator.entries[&7].dirty);
        assert!(coordinator.entries[&7].next_refresh.is_some());

        let model = aterm_spec::derive::title_summary_managed_endpoint_model();
        let launched = model.successors("Launch1", &model.init_state())[0].clone();
        let ready = model.successors("Reuse1", &launched)[0].clone();
        let crashed = model.successors("Crash1", &ready)[0].clone();
        assert_eq!(crashed["endpoint1"], 0);
        assert_eq!(crashed["health_endpoint1"], 0);
        assert!(model.check_invariant("RevokedHealthIsClear", &crashed));
    }

    /// FRAME AUDIT #3, end to end over the REAL App wiring: the published
    /// `Idle` phase reaches the window-title subject through
    /// [`crate::App::note_title_activity`]'s reconcile — including when the
    /// phase was published BEFORE the title coordinator ever observed the
    /// session (the launch order on a quiet shell), which is exactly the case
    /// the publish-edge hook alone cannot cover.
    #[test]
    fn the_published_idle_phase_reaches_the_window_subject_through_the_app() {
        use crate::session_status::{ActivitySample, Evidence, ShellEvidence};
        let mut app = crate::App::headless_for_test();
        let term = app
            .front_terminal(crate::WindowId(0))
            .expect("front terminal")
            .term
            .clone();
        // The shell sits at a prompt with command input open (OSC 133 A + B):
        // the audit's exact stuck shape — block Entering, nothing typed.
        term.lock()
            .unwrap()
            .process(b"\x1b]133;A\x1b\\\x1b]133;B\x1b\\");

        // The classifier publishes Idle BEFORE any title observation exists.
        let evidence = Evidence {
            pin: None,
            shell: Some(ShellEvidence::Entering),
            lifecycle: None,
            foreground_job: Some(false),
            activity: ActivitySample {
                alt_screen: false,
                content_seq: 1,
                last_output: None,
                // A real keystroke: this test drives the LIVE typing marker,
                // which now needs input evidence, not bare movement.
                last_input: Some(Instant::now()),
            },
        };
        let t0 = Instant::now();
        assert!(!app.session_status.observe(0, &evidence, t0));
        assert!(
            app.session_status
                .observe(0, &evidence, t0 + Duration::from_secs(5)),
            "the dwelled candidate publishes Idle"
        );

        // An output wake observes the title — and must pick the verdict up.
        app.note_title_activity(0);
        assert_eq!(
            app.title_summaries.activity(0, &app.config),
            Some("Ready"),
            "the settled phase reaches the presented subject"
        );
    }

    /// FRAME AUDIT #3: a half-typed, abandoned command line must not hold
    /// "Typing a command" in the chrome forever. `observe` runs only on output
    /// wakes, so once typing stops the Entering claim freezes — while the phase
    /// classifier honestly publishes `Idle`. The pushed verdict
    /// ([`Coordinator::note_phase_settled`]) decays the presented subject to the
    /// prompt-state description, and the next semantic boundary (fresh output)
    /// snaps it straight back without waiting for the classifier's dwell.
    #[test]
    fn an_entering_subject_decays_to_the_prompt_description_on_settled_idle() {
        let mut coordinator = Coordinator::new(None);
        let config = Config::default(); // builtin provider, descriptive titles on
        let mut term = Terminal::new(4, 40);
        // OSC 133 A (prompt) then B (command input): the block is now
        // EnteringCommand with no commandline, the audit's exact stuck shape.
        term.process(b"\x1b]133;A\x1b\\\x1b]133;B\x1b\\");
        coordinator.observe(7, &term, &config, true, Instant::now());
        assert_eq!(coordinator.activity(7, &config), Some("Typing a command"));
        let revision_before = coordinator.activity_revision(7);

        // An unknown session takes no verdict.
        assert!(!coordinator.note_phase_settled(99, true));

        // The classifier publishes Idle: the stale typing claim decays to the
        // prompt description, visibly (revision moves for the chrome caches).
        assert!(coordinator.note_phase_settled(7, true));
        assert_eq!(coordinator.activity(7, &config), Some("Ready"));
        assert!(coordinator.activity_revision(7) > revision_before);
        assert!(
            !coordinator.note_phase_settled(7, true),
            "an unchanged verdict is free"
        );

        // A semantic boundary does NOT clear the decay on its own: the phase
        // classifier is the idleness authority, and a sub-dwell activity blip
        // never re-publishes the unchanged `Idle` — clearing here left the
        // subject stuck at "Typing a command" again (live repro). The host
        // re-pushes the current verdict after every observation instead.
        term.set_title("vim");
        coordinator.observe(7, &term, &config, true, Instant::now());
        assert_eq!(
            coordinator.activity(7, &config),
            Some("Ready"),
            "a boundary alone must not resurrect the stale Entering claim"
        );

        // Only the classifier's own non-idle verdict releases the decay.
        assert!(coordinator.note_phase_settled(7, false));
        assert_eq!(coordinator.activity(7, &config), Some("Typing a command"));
        assert!(coordinator.note_phase_settled(7, true));
        assert_eq!(coordinator.activity(7, &config), Some("Ready"));
    }

    /// Regression: a managed-runtime exit re-arms every session at `now`, so the
    /// next timer wake usually lands inside the minimum interval. That gated
    /// wake must move `next_refresh` to the next admissible instant — the old
    /// code re-armed only at semantic boundaries, so the elapsed deadline made
    /// `next_retry()` keep returning a past instant, `about_to_wait`'s
    /// `WaitUntil` fired immediately, and the event loop busy-spun through full
    /// snapshots (including the sensitive-text scan) for up to a whole interval.
    #[test]
    fn gated_wake_after_managed_runtime_exit_rearms_into_the_future() {
        let mut coordinator = Coordinator::new(None);
        let config = Config {
            title_summary_provider: Some(TitleSummaryProvider::Ollama),
            title_summary_interval_seconds: Some(300),
            ..Config::default()
        };
        assert!(coordinator.reconfigure(&config));
        let (request_rx, result_tx) = install_test_worker(&mut coordinator);
        let mut term = Terminal::new(2, 20);
        let start = Instant::now();
        coordinator.observe(7, &term, &config, true, start);
        let job = request_rx
            .try_recv()
            .expect("the first observation dispatches");
        let endpoint = "http://127.0.0.1:32123/api/chat";
        result_tx
            .send(ok_result_for(&job, "Running tests", endpoint))
            .unwrap();
        assert_eq!(coordinator.poll(&config), vec![7]);

        result_tx
            .send(WorkerMessage::ManagedRuntimeExited(ManagedRuntimeExit {
                endpoint: endpoint.to_string(),
                authority_epoch: job.authority_epoch,
            }))
            .unwrap();
        assert!(coordinator.poll(&config).is_empty());
        let woke = Instant::now();
        assert!(
            coordinator
                .next_retry()
                .is_some_and(|deadline| deadline <= woke),
            "the exit makes every session due for one immediate observation"
        );

        // The wake lands inside the 300 s minimum: timer-due but inadmissible.
        // It must not queue work and must leave no elapsed deadline behind.
        coordinator.observe(7, &term, &config, true, woke);
        assert!(coordinator.pending.is_empty());
        assert!(request_rx.try_recv().is_err());
        let rearmed = coordinator
            .next_retry()
            .expect("the session stays scheduled");
        assert!(
            rearmed > woke,
            "a gated wake must re-arm strictly into the future"
        );
        assert_eq!(
            coordinator.entries[&7].next_refresh,
            Some(start + Duration::from_secs(300)),
            "the re-arm is the next admissible instant"
        );

        // A due-and-admissible refresh still fires: past the minimum, the same
        // session queues exactly one replacement request.
        term.process(b"fresh output after the crash\r\n");
        coordinator.observe(7, &term, &config, true, start + Duration::from_secs(300));
        let refreshed = request_rx
            .try_recv()
            .expect("an admissible refresh dispatches");
        assert_eq!(refreshed.session, 7);
    }

    /// An idle session's timer refresh re-captures a byte-identical snapshot,
    /// and an identical prompt can only reproduce the label already shown. The
    /// refresh is skipped and re-armed; real new output re-admits.
    #[test]
    fn idle_timer_refreshes_skip_identical_snapshots_until_content_changes() {
        let mut coordinator = Coordinator::new(None);
        let config = Config {
            title_summary_provider: Some(TitleSummaryProvider::Ollama),
            title_summary_interval_seconds: Some(5),
            ..Config::default()
        };
        assert!(coordinator.reconfigure(&config));
        let (request_rx, result_tx) = install_test_worker(&mut coordinator);
        let mut term = Terminal::new(2, 20);
        let start = Instant::now();
        coordinator.observe(7, &term, &config, true, start);
        let job = request_rx
            .try_recv()
            .expect("the first observation dispatches");
        result_tx
            .send(ok_result_for(
                &job,
                "Quiet shell",
                "http://127.0.0.1:32123/api/chat",
            ))
            .unwrap();
        assert_eq!(coordinator.poll(&config), vec![7]);

        // Two timer refreshes over the unchanged terminal: neither may enqueue,
        // each re-arms the next periodic check, and the applied result's
        // generation survives (a skip must not revoke it).
        coordinator.observe(7, &term, &config, true, start + Duration::from_secs(5));
        assert!(request_rx.try_recv().is_err());
        assert!(coordinator.pending.is_empty());
        assert_eq!(coordinator.entries[&7].generation, 1);
        assert_eq!(
            coordinator.entries[&7].next_refresh,
            Some(start + Duration::from_secs(10))
        );
        coordinator.observe(7, &term, &config, true, start + Duration::from_secs(10));
        assert!(request_rx.try_recv().is_err());
        assert!(coordinator.pending.is_empty());
        assert_eq!(coordinator.activity(7, &config), Some("Quiet shell"));

        // New output changes prompt content without a semantic boundary: the
        // next due refresh admits exactly one superseding request.
        term.process(b"compiling aterm-gui\r\n");
        coordinator.observe(7, &term, &config, true, start + Duration::from_secs(15));
        let refreshed = request_rx.try_recv().expect("changed content re-admits");
        assert_eq!(refreshed.session, 7);
        assert_eq!(
            coordinator.entries[&7].generation, 2,
            "a real periodic request supersedes the applied result"
        );
    }

    /// The dedup must never absorb an error retry: a failed request sets
    /// `dirty`, so the post-backoff refresh re-dispatches even though snapshot
    /// content is unchanged (the failure was transport, not content).
    #[test]
    fn failure_retry_dispatches_over_an_unchanged_snapshot() {
        let mut coordinator = Coordinator::new(None);
        let config = Config {
            title_summary_provider: Some(TitleSummaryProvider::Ollama),
            title_summary_interval_seconds: Some(5),
            ..Config::default()
        };
        assert!(coordinator.reconfigure(&config));
        let (request_rx, result_tx) = install_test_worker(&mut coordinator);
        let term = Terminal::new(2, 20);
        let start = Instant::now();
        coordinator.observe(7, &term, &config, true, start);
        let job = request_rx
            .try_recv()
            .expect("the first observation dispatches");
        result_tx
            .send(WorkerMessage::Result(WorkerResult {
                session: job.session,
                session_epoch: job.session_epoch,
                generation: job.generation,
                authority_epoch: job.authority_epoch,
                config_fingerprint: job.config_fingerprint,
                result: Err("connection refused".to_string()),
                locality: TitleSummaryLocality::ManagedLocal,
                effective_endpoint: None,
                managed_install_present: true,
            }))
            .unwrap();
        assert!(coordinator.poll(&config).is_empty());
        assert!(
            coordinator.entries[&7].dirty,
            "a failure marks the entry for a real retry"
        );
        let retry_at = coordinator.entries[&7]
            .backoff_until
            .expect("a failure arms a backoff");

        coordinator.observe(7, &term, &config, true, retry_at + Duration::from_secs(5));
        let retried = request_rx
            .try_recv()
            .expect("the backoff retry must reach the provider");
        assert_eq!(retried.session, 7);
        assert_eq!(retried.generation, 2);
    }

    /// Tab and window chrome recompose labels every frame. Unchanged inputs
    /// must be served from the per-session compose cache rather than re-running
    /// sanitize/grapheme passes and allocations each frame.
    #[test]
    fn composed_labels_cache_per_session_until_title_or_description_changes() {
        let mut coordinator = Coordinator::new(None);
        let config = Config {
            title_summary_provider: Some(TitleSummaryProvider::Ollama),
            ..Config::default()
        };
        assert!(coordinator.reconfigure(&config));
        // Hold the lane synthetically: no worker, no provider I/O.
        coordinator.in_flight = Some((99, 99, 99, 99, 99));
        let term = Terminal::new(2, 20);
        coordinator.observe(7, &term, &config, true, Instant::now());
        coordinator.set_test_activity(7, "Compiling the release build");

        let first = coordinator.compose(
            Some(7),
            "make",
            None,
            TitleFormat::TitleDescription,
            &config,
            " · ",
        );
        assert_eq!(first, "make · Compiling the release build");
        let runs = coordinator.compose_runs();
        assert!(runs >= 1);
        let second = coordinator.compose(
            Some(7),
            "make",
            None,
            TitleFormat::TitleDescription,
            &config,
            " · ",
        );
        assert_eq!(second, first);
        assert_eq!(
            coordinator.compose_runs(),
            runs,
            "unchanged inputs reuse the cached label"
        );

        // The window flavor occupies its own slot: the two per-frame callers
        // alternate every frame and must not evict each other.
        let window_first = coordinator.compose(
            Some(7),
            "make",
            None,
            TitleFormat::TitleDescription,
            &config,
            " — ",
        );
        assert_eq!(window_first, "make — Compiling the release build");
        let warm = coordinator.compose_runs();
        let tab_again = coordinator.compose(
            Some(7),
            "make",
            None,
            TitleFormat::TitleDescription,
            &config,
            " · ",
        );
        let window_again = coordinator.compose(
            Some(7),
            "make",
            None,
            TitleFormat::TitleDescription,
            &config,
            " — ",
        );
        assert_eq!(tab_again, first);
        assert_eq!(window_again, window_first);
        assert_eq!(
            coordinator.compose_runs(),
            warm,
            "flavors are cached independently"
        );

        // A title change recomposes once; a description change recomposes once.
        let retitled = coordinator.compose(
            Some(7),
            "make check",
            None,
            TitleFormat::TitleDescription,
            &config,
            " · ",
        );
        assert_eq!(retitled, "make check · Compiling the release build");
        assert_eq!(coordinator.compose_runs(), warm + 1);
        coordinator.set_test_activity(7, "Linking objects");
        let redescribed = coordinator.compose(
            Some(7),
            "make check",
            None,
            TitleFormat::TitleDescription,
            &config,
            " · ",
        );
        assert_eq!(redescribed, "make check · Linking objects");
        assert_eq!(coordinator.compose_runs(), warm + 2);

        // Retirement drops the session's cache slots with the session.
        coordinator.retire(7);
        assert!(
            coordinator
                .compose_cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        );
    }

    /// The clean-title fast path must be indistinguishable from the full
    /// sanitize/grapheme composition, and must reject every title the
    /// sanitizer would actually rewrite.
    #[test]
    fn clean_title_fast_path_matches_full_composition() {
        let coordinator = Coordinator::new(None);
        let config = Config::default();
        let long_clean = "x".repeat(MAX_CHROME_TITLE_GRAPHEMES);
        let clean = ["vim", "cargo build --release", "a b c", long_clean.as_str()];
        for title in clean {
            assert!(title_is_presentation_clean(title), "{title:?}");
        }
        let over_cap = "y".repeat(MAX_CHROME_TITLE_GRAPHEMES + 1);
        let dirty = [
            " padded ",
            "two  spaces",
            "tab\there",
            "combining e\u{301}",
            "bidi \u{202e}spoof",
            over_cap.as_str(),
        ];
        for title in dirty {
            assert!(!title_is_presentation_clean(title), "{title:?}");
        }
        for title in clean.iter().copied().chain(dirty.iter().copied()) {
            let composed = coordinator.compose(
                None,
                title,
                None,
                TitleFormat::TitleDescription,
                &config,
                " · ",
            );
            let expected = compose_parts(
                &chrome_presentation_text(title, MAX_CHROME_TITLE_GRAPHEMES),
                "",
                TitleFormat::TitleDescription,
                " · ",
            );
            assert_eq!(composed, expected, "{title:?}");
        }
        let empty = coordinator.compose(
            None,
            "",
            None,
            TitleFormat::TitleDescription,
            &config,
            " · ",
        );
        assert_eq!(empty, "aterm", "an empty title still resolves the fallback");
    }

    #[test]
    fn reconfigure_and_shutdown_clear_automatic_runtime_endpoint_health() {
        let mut coordinator = Coordinator::new(None);
        let automatic = Config {
            title_summary_provider: Some(TitleSummaryProvider::Ollama),
            ..Config::default()
        };
        assert!(coordinator.reconfigure(&automatic));
        coordinator.runtime_endpoint = Some("http://127.0.0.1:32123/api/chat".to_string());
        coordinator.runtime_locality = TitleSummaryLocality::ManagedLocal;

        let mut changed = automatic.clone();
        changed.title_summary_model = Some("another-model".to_string());
        assert!(coordinator.reconfigure(&changed));
        assert_eq!(coordinator.health(Instant::now(), &changed).endpoint, None);
        assert_eq!(
            coordinator.health(Instant::now(), &changed).locality,
            TitleSummaryLocality::NotApplicable
        );

        coordinator.runtime_endpoint = Some("http://127.0.0.1:32124/api/chat".to_string());
        coordinator.shutdown();
        assert_eq!(coordinator.runtime_endpoint, None);
        assert_eq!(
            coordinator.runtime_locality,
            TitleSummaryLocality::NotApplicable
        );
    }

    #[test]
    fn semantic_boundary_inside_minimum_destroys_queued_a_work_and_priority() {
        let mut coordinator = Coordinator::new(None);
        let config = Config {
            title_summary_provider: Some(TitleSummaryProvider::Ollama),
            title_summary_interval_seconds: Some(30),
            ..Config::default()
        };
        assert!(coordinator.reconfigure(&config));
        // Another synthetic request holds the capacity-one lane, keeping A in the
        // real per-session pending map without starting a worker.
        coordinator.in_flight = Some((99, 99, 99, 99, 99));
        let start = Instant::now();
        let mut term = Terminal::new(2, 20);
        term.set_title("semantic A");
        coordinator.observe(7, &term, &config, true, start);
        assert_eq!(coordinator.pending[&7].generation, 1);
        assert_eq!(coordinator.priority_pending_session, Some(7));

        term.set_title("semantic B");
        coordinator.observe(7, &term, &config, true, start + Duration::from_secs(1));
        assert_eq!(coordinator.entries[&7].generation, 2);
        assert!(!coordinator.pending.contains_key(&7));
        assert_ne!(coordinator.priority_pending_session, Some(7));
        assert_eq!(
            coordinator.entries[&7].next_refresh,
            Some(start + Duration::from_secs(30)),
            "B is resampled at the original minimum deadline"
        );
    }

    #[test]
    fn health_countdowns_are_stable_within_one_display_second() {
        let mut coordinator = Coordinator::new(None);
        let config = Config {
            title_summary_provider: Some(TitleSummaryProvider::Ollama),
            ..Config::default()
        };
        coordinator.reconfigure(&config);
        let now = Instant::now();
        coordinator.worker_retry_at = Some(now + Duration::from_secs(10));
        let first = coordinator.health(now, &config);
        let same_display_value = coordinator.health(now + Duration::from_millis(100), &config);
        assert_eq!(first, same_display_value);
        let next_display_value = coordinator.health(now + Duration::from_millis(1_100), &config);
        assert_ne!(first, next_display_value);
        assert_eq!(first.next_retry_after, Some(Duration::from_secs(10)));
        assert_eq!(
            next_display_value.next_retry_after,
            Some(Duration::from_secs(9))
        );
    }

    #[test]
    fn due_observation_batches_are_active_first_capped_and_fair_under_stress() {
        let mut coordinator = Coordinator::new(None);
        let now = Instant::now();
        for session in 1..=512 {
            coordinator.retries.insert(session, now);
        }
        let active = 311;
        let mut served = Vec::new();
        for _ in 1..=512 {
            let admitted = coordinator.due_observations(now, Some(active));
            assert_eq!(admitted.len(), 1, "one event-loop turn may snapshot once");
            served.push(admitted[0]);
            coordinator.observation_succeeded(admitted[0]);
        }
        assert_eq!(served[0], active, "active session must start a fresh batch");
        served.sort_unstable();
        assert_eq!(served, (1..=512).collect::<Vec<_>>());
        assert!(coordinator.due_observations(now, Some(active)).is_empty());
    }

    /// REGRESSION: sustained lock contention must make the retry CHEAPER, not
    /// re-arm the same short deadline forever.
    ///
    /// The observation snapshots the terminal by try_lock. Under a flood the PTY
    /// reader holds that mutex most of the time, so the try_lock fails on
    /// essentially every output burst — and `Wake::Output` fires at the reader's
    /// BATCH rate. A fixed 4 ms retry was therefore always pending, forcing the event
    /// loop awake at ~250 Hz for a title heuristic and putting a permanent 4 ms floor
    /// under whatever deadline frame pacing had chosen. Backing off means the cost of
    /// contention falls as contention persists.
    #[test]
    fn contended_observations_back_off_instead_of_pinning_the_event_loop() {
        let mut coordinator = Coordinator::new(None);
        let now = Instant::now();
        let session = 7u64;

        let delay_after = |c: &mut Coordinator, strikes: usize| {
            c.retries.remove(&session);
            c.contended.remove(&session);
            for _ in 0..strikes {
                c.defer_observation(session, now);
            }
            c.retries.get(&session).map(|at| at.duration_since(now))
        };

        let base = Coordinator::OBSERVE_RETRY_BASE;
        assert_eq!(delay_after(&mut coordinator, 1), Some(base));
        assert_eq!(delay_after(&mut coordinator, 2), Some(base * 2));
        assert_eq!(delay_after(&mut coordinator, 3), Some(base * 4));
        // ...and it is CAPPED, so a permanently wedged reader cannot push the title
        // refresh out past usefulness (or overflow the shift).
        let capped = delay_after(&mut coordinator, 40);
        assert_eq!(capped, Some(Coordinator::OBSERVE_RETRY_CAP));

        // One success clears the ladder: an idle terminal is still observed promptly
        // rather than inheriting a stale ceiling from an earlier flood.
        coordinator.observation_succeeded(session);
        assert!(!coordinator.contended.contains_key(&session));
        coordinator.retries.remove(&session);
        coordinator.defer_observation(session, now);
        assert_eq!(
            coordinator
                .retries
                .get(&session)
                .map(|at| at.duration_since(now)),
            Some(base),
            "the ladder restarts at the base delay after a successful observation"
        );
    }

    #[test]
    fn enabling_descriptions_seeds_three_quiet_sessions_active_first_one_per_turn() {
        let mut coordinator = Coordinator::new(None);
        let now = Instant::now();
        coordinator.schedule_live_observations([30, 10, 20], Some(20), now);
        let mut served = Vec::new();
        for _ in 0..3 {
            let admitted = coordinator.due_observations(now, Some(20));
            assert_eq!(admitted.len(), 1);
            served.push(admitted[0]);
            coordinator.observation_succeeded(admitted[0]);
        }
        assert_eq!(served, vec![20, 10, 30]);
        assert!(coordinator.due_observations(now, Some(20)).is_empty());
    }

    #[test]
    fn app_enable_transition_schedules_every_live_pool_session() {
        let mut app = App::headless_for_test();
        app.pool.insert(crate::stub_session(41));
        app.pool.insert(crate::stub_session(42));
        let mut live: Vec<u64> = app.pool.iter().map(|session| session.id).collect();
        live.sort_unstable();
        assert_eq!(live.len(), 3);

        app.config.descriptive_titles = Some(false);
        app.config.title_summary_provider = Some(TitleSummaryProvider::Off);
        app.reconfigure_title_summaries();
        assert!(app.title_summaries.retries.is_empty());

        app.config.descriptive_titles = Some(true);
        app.config.title_summary_provider = Some(TitleSummaryProvider::Builtin);
        let active = app
            .frontmost_window
            .and_then(|window| app.focused_session_id(window));
        app.reconfigure_title_summaries();
        let mut scheduled: Vec<u64> = app.title_summaries.retries.keys().copied().collect();
        scheduled.sort_unstable();
        assert_eq!(scheduled, live);
        assert_eq!(
            app.title_summaries.due_observation_queue.front().copied(),
            active,
            "the live active session owns the first bounded observation turn"
        );
    }

    /// Tier-1 conformance for the derived observation-turn scheduler. Shipping
    /// selection must project active-first then preserve the sorted remainder; the
    /// historical bulk-drain negative control violates the modeled per-turn cap.
    #[test]
    fn due_observation_admission_conforms_to_derived_scheduler() {
        let model = aterm_spec::derive::title_summary_observation_scheduler_model();
        let now = Instant::now();
        let mut coordinator = Coordinator::new(None);
        for session in [1, 2, 3] {
            coordinator.retries.insert(session, now);
        }
        let mut shipping = Vec::new();
        let mut modeled = model.init_state();
        for _ in 0..3 {
            let admitted = coordinator.due_observations(now, Some(2));
            assert_eq!(admitted.len(), 1);
            shipping.push(i64::try_from(admitted[0]).unwrap());
            coordinator.observation_succeeded(admitted[0]);
            modeled = model.successors("ObserveTurn", &modeled)[0].clone();
            assert_eq!(shipping.last().copied(), Some(modeled["chosen"]));
        }
        assert_eq!(shipping, vec![2, 1, 3]);

        let mut worker_model = model.init_state();
        let mut last_priority = false;
        let mut after = None;
        for expected in [1_u64, 2, 1] {
            let (chosen, was_priority) =
                choose_dispatch_session([1, 2].into_iter(), after, Some(1), last_priority);
            assert_eq!(chosen, Some(expected));
            worker_model = model.successors("DispatchWorker", &worker_model)[0].clone();
            assert_eq!(
                worker_model["worker_chosen"],
                i64::try_from(expected).unwrap()
            );
            after = chosen;
            last_priority = was_priority;
        }

        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let bulk = buggy.successors("ObserveTurn", &buggy.init_state())[0].clone();
        assert!(
            !buggy.check_invariant("OneObservationPerTurn", &bulk),
            "negative control must prove a bulk drain is rejected"
        );
        let priority_once = buggy.successors("DispatchWorker", &buggy.init_state())[0].clone();
        let priority_twice = buggy.successors("DispatchWorker", &priority_once)[0].clone();
        assert!(
            !buggy.check_invariant("PriorityCannotStarveBackground", &priority_twice),
            "negative control must reject repeated priority bypass"
        );
    }

    #[test]
    fn retirement_revokes_a_dequeued_jobs_session_authority() {
        let mut coordinator = Coordinator::new(None);
        let config = Config {
            title_summary_provider: Some(TitleSummaryProvider::Ollama),
            ..Config::default()
        };
        coordinator.reconfigure(&config);
        let (session_epoch, session_authority) = coordinator.session_authority(41);
        let job = Job {
            session: 41,
            session_epoch,
            session_authority,
            generation: 1,
            authority_epoch: coordinator.authority_epoch,
            config_fingerprint: coordinator.authority_fingerprint,
            settings: provider_settings(&config).unwrap(),
            snapshot: snap("cargo test", ActivityState::Executing, None),
        };
        assert!(job_is_authorized(&job, &coordinator.worker_authority_epoch));
        coordinator.retire(41);
        assert!(!job_is_authorized(
            &job,
            &coordinator.worker_authority_epoch
        ));
    }

    #[test]
    fn recent_context_includes_scrolled_off_lines() {
        let mut term = Terminal::new(2, 24);
        term.process(b"old-history\r\nmiddle\r\nnew-visible");
        assert_eq!(term.get_line_text(-1, None).as_deref(), Some("old-history"));
        let mut snapshot = snap("cargo test", ActivityState::Executing, None);
        snapshot.capture_recent_output(&term, 3);
        assert!(snapshot.recent_output.contains("old-history"));
        assert!(snapshot.recent_output.contains("new-visible"));
    }

    #[test]
    fn managed_ollama_command_is_cloud_disabled() {
        let command = managed_ollama_command(
            std::path::Path::new("ollama"),
            "127.0.0.1:11434",
            std::path::Path::new("/tmp/aterm-models"),
            std::path::Path::new("/tmp/aterm-managed-home"),
        );
        let env: HashMap<_, _> = command.get_envs().collect();
        assert_eq!(
            env.get(&std::ffi::OsStr::new("OLLAMA_NO_CLOUD")),
            Some(&Some(std::ffi::OsStr::new("1")))
        );
        assert_eq!(
            command.get_args().next(),
            Some(std::ffi::OsStr::new("serve"))
        );
    }

    #[test]
    fn managed_ollama_environment_is_an_exact_minimal_set() {
        let mut command = std::process::Command::new("/usr/bin/env");
        configure_managed_ollama_environment(
            &mut command,
            "127.0.0.1:11434",
            std::path::Path::new("/managed/models"),
            std::path::Path::new("/managed/home"),
        );
        let output = command.output().unwrap();
        assert!(output.status.success());
        let mut lines: Vec<_> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        lines.sort();
        assert_eq!(
            lines,
            [
                "HOME=/managed/home",
                "OLLAMA_HOST=127.0.0.1:11434",
                "OLLAMA_MODELS=/managed/models",
                "OLLAMA_NOHISTORY=1",
                "OLLAMA_NO_CLOUD=1",
            ]
            .map(str::to_owned)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn managed_runtime_policy_and_code_requirement_fail_closed() {
        let safe = ManagedPathPolicyFacts {
            within_root: true,
            kind_matches: true,
            owner_matches: true,
            mode: 0o100555,
        };
        assert!(validate_managed_path_policy(ManagedPathKind::Executable, safe).is_ok());
        assert!(
            validate_managed_path_policy(
                ManagedPathKind::Executable,
                ManagedPathPolicyFacts {
                    within_root: false,
                    ..safe
                },
            )
            .is_err()
        );
        assert!(
            validate_managed_path_policy(
                ManagedPathKind::Executable,
                ManagedPathPolicyFacts {
                    mode: 0o100755,
                    ..safe
                },
            )
            .is_err()
        );
        let requirement =
            ollama_designated_requirement(OLLAMA_TEAM_ID, OLLAMA_CODE_IDENTIFIER).unwrap();
        assert!(requirement.contains("anchor apple generic"));
        assert!(requirement.contains("3MU9H2V9Y9"));
        assert!(requirement.contains("ai.ollama.ollama"));
        assert!(ollama_designated_requirement("x\" or true", "ai.ollama.ollama").is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn established_socket_parser_selects_only_the_server_side_tuple() {
        let model = aterm_spec::derive::title_summary_socket_owner_retry_model();
        let initial = model.init_state();
        let server: std::net::SocketAddr = "127.0.0.1:11434".parse().unwrap();
        let client: std::net::SocketAddr = "127.0.0.1:53111".parse().unwrap();
        let output = b"p700\nf9\nn127.0.0.1:11434->127.0.0.1:53111\nTST=ESTABLISHED\n\
p701\nf8\nn127.0.0.1:53111->127.0.0.1:11434\nTST=ESTABLISHED\n";
        assert_eq!(
            parse_established_server_pid(output, server, client).unwrap(),
            700
        );
        let unique = model.successors("ObserveUnique", &initial)[0].clone();
        assert_eq!(unique["phase"], 2);

        let client_only = b"p701\nf8\nn127.0.0.1:53111->127.0.0.1:11434\nTST=ESTABLISHED\n";
        let missing_error = parse_established_server_pid(client_only, server, client).unwrap_err();
        assert_eq!(missing_error, NO_ESTABLISHED_SERVER_OWNER);
        let missing = model.successors("ObserveMissing", &initial)[0].clone();
        assert_eq!(
            socket_owner_observation_is_transient(&missing_error),
            missing["phase"] == 1
        );

        let ambiguous = b"p700\nf9\nn127.0.0.1:11434->127.0.0.1:53111\nTST=ESTABLISHED\n\
p702\nf4\nn127.0.0.1:11434->127.0.0.1:53111\nTST=ESTABLISHED\n";
        let ambiguous_error = parse_established_server_pid(ambiguous, server, client).unwrap_err();
        assert_eq!(ambiguous_error, MULTIPLE_ESTABLISHED_SERVER_OWNERS);
        let ambiguous_model = model.successors("ObserveAmbiguous", &initial)[0].clone();
        assert_eq!(
            socket_owner_observation_is_transient(&ambiguous_error),
            ambiguous_model["phase"] == 1
        );

        assert!(socket_owner_observation_is_transient(
            NO_ESTABLISHED_SERVER_OWNER
        ));
        assert!(socket_owner_observation_is_transient(
            MULTIPLE_ESTABLISHED_SERVER_OWNERS
        ));
        assert!(!socket_owner_observation_is_transient(
            "listener ownership output exceeded its structural bound"
        ));
        let structural = model.successors("ObserveStructuralError", &initial)[0].clone();
        assert_eq!(structural["phase"], 3);

        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let premature = buggy.successors("ObserveAmbiguous", &initial)[0].clone();
        assert_ne!(
            socket_owner_observation_is_transient(&ambiguous_error),
            premature["phase"] == 1,
            "negative control: the old ambiguity-drop transition must disagree"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn exact_connected_socket_owner_is_observed_and_unrelated_root_is_rejected() {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let server = listener.local_addr().unwrap();
        let (accepted_tx, accepted_rx) = std::sync::mpsc::sync_channel(0);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
        let join = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            accepted_tx.send(()).unwrap();
            let _ = release_rx.recv();
            drop(stream);
        });
        let stream = std::net::TcpStream::connect(server).unwrap();
        accepted_rx.recv().unwrap();
        assert_eq!(established_server_pid(&stream).unwrap(), std::process::id());

        let mut unrelated = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let unrelated_identity = managed_process_identity(unrelated.id()).unwrap();
        assert!(validate_managed_process_ancestry(std::process::id(), unrelated_identity).is_err());
        let _ = unrelated.kill();
        let _ = unrelated.wait();
        drop(stream);
        release_tx.send(()).unwrap();
        join.join().unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn managed_runtime_rejects_an_unsigned_or_tampered_helper() {
        let helper = std::env::temp_dir().join(format!(
            "aterm-ollama-tampered-helper-{}",
            std::process::id()
        ));
        std::fs::write(&helper, b"not signed Ollama code").unwrap();
        assert!(verify_ollama_code_paths(std::slice::from_ref(&helper)).is_err());
        std::fs::remove_file(helper).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn installed_managed_runtime_closure_and_dynamic_pid_attest() {
        let Some((binary, models)) = managed_ollama_paths() else {
            return;
        };
        if !binary.is_file() {
            return;
        }
        let attested = attest_managed_ollama(&binary, &models).unwrap();
        assert!(
            attested._closure_guards.len() > 1,
            "the complete helper/library closure, not only ollama, must be held"
        );
        let port = 20_000 + (std::process::id() % 20_000);
        let bind = format!("127.0.0.1:{port}");
        let private_home = create_private_managed_home().unwrap();
        let mut child =
            managed_ollama_command(&attested.binary, &bind, &attested.models, &private_home)
                .spawn()
                .unwrap();
        let process = managed_process_identity(child.id()).unwrap();
        attest_running_managed_ollama(child.id()).unwrap();
        let socket: std::net::SocketAddr = bind.parse().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let stream = loop {
            match std::net::TcpStream::connect_timeout(&socket, Duration::from_millis(100)) {
                Ok(stream) => break stream,
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(error) => panic!("managed Ollama did not listen: {error}"),
            }
        };
        let result = attest_managed_server_stream(&stream, process);
        drop(stream);
        let _ = child.kill();
        let _ = child.wait();
        cleanup_private_managed_home(Some(&private_home));
        result.unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires the private Ollama runtime and qwen3.5 model"]
    fn installed_managed_runtime_serves_inference_over_attested_stream() {
        let Some((binary, _)) = managed_ollama_paths() else {
            return;
        };
        if !binary.is_file() {
            return;
        }
        let authority_epoch = 91;
        let controller = ManagedOllamaController::new(Some(authority_epoch));
        let mut ollama = ManagedOllama::new(controller);
        let authority = Arc::new(AtomicU64::new(authority_epoch));
        let session_authority = Arc::new(AtomicU64::new(3));
        let config = Config {
            title_summary_provider: Some(TitleSummaryProvider::Ollama),
            title_summary_model: Some("qwen3.5:4b-q4_K_M".to_string()),
            title_summary_timeout_seconds: Some(60),
            title_summary_allow_remote: Some(false),
            ..Config::default()
        };
        let job = Job {
            session: 1,
            session_epoch: 3,
            session_authority,
            generation: 1,
            authority_epoch,
            config_fingerprint: config_fingerprint(&config),
            settings: provider_settings(&config).unwrap(),
            snapshot: snap("cargo test -p aterm-gui", ActivityState::Executing, None),
        };
        let outcome = request_summary(&job, &authority, &mut ollama).unwrap();
        ollama.stop();
        assert_eq!(outcome.locality, TitleSummaryLocality::ManagedLocal);
        assert!(outcome.effective_endpoint.starts_with("http://127.0.0.1:"));
        assert_ne!(
            outcome.effective_endpoint,
            crate::app_config::EXAMPLE_EXPLICIT_OLLAMA_TITLE_SUMMARY_ENDPOINT
        );
        assert!(!outcome.activity.trim().is_empty());
        eprintln!("attested Ollama activity: {}", outcome.activity);
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires enough memory for two private Ollama runtimes and the pinned model"]
    fn two_real_managed_runtimes_use_distinct_exactly_attested_streams() {
        let Some((binary, _)) = managed_ollama_paths() else {
            return;
        };
        if !binary.is_file() {
            return;
        }
        let config = Config {
            title_summary_provider: Some(TitleSummaryProvider::Ollama),
            title_summary_model: Some("qwen3.5:4b-q4_K_M".to_string()),
            title_summary_timeout_seconds: Some(120),
            ..Config::default()
        };
        let first_controller = ManagedOllamaController::new(Some(201));
        let second_controller = ManagedOllamaController::new(Some(202));
        let launch = |authority_epoch: u64, controller: ManagedOllamaController| {
            let config = config.clone();
            std::thread::spawn(move || {
                let authority = Arc::new(AtomicU64::new(authority_epoch));
                let job = Job {
                    session: authority_epoch,
                    session_epoch: 1,
                    session_authority: Arc::new(AtomicU64::new(1)),
                    generation: 1,
                    authority_epoch,
                    config_fingerprint: config_fingerprint(&config),
                    settings: provider_settings(&config).unwrap(),
                    snapshot: snap("cargo test", ActivityState::Executing, None),
                };
                let mut ollama = ManagedOllama::new(controller);
                request_summary(&job, &authority, &mut ollama)
                    .unwrap()
                    .effective_endpoint
            })
        };
        let first = launch(201, first_controller.clone());
        let second = launch(202, second_controller.clone());
        let first_endpoint = first.join().unwrap();
        let second_endpoint = second.join().unwrap();
        assert_ne!(first_endpoint, second_endpoint);

        for (controller, epoch, endpoint) in [
            (&first_controller, 201, &first_endpoint),
            (&second_controller, 202, &second_endpoint),
        ] {
            let process = controller.endpoint_process(endpoint, epoch).unwrap();
            let (socket, _) = loopback_socket(endpoint).unwrap();
            let stream =
                std::net::TcpStream::connect_timeout(&socket, Duration::from_secs(1)).unwrap();
            attest_managed_server_stream(&stream, process).unwrap();
        }
        first_controller.stop();
        second_controller.stop();
    }

    #[cfg(unix)]
    #[test]
    fn ca_bundle_rejects_symlinks_and_fifos_without_blocking() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::symlink;

        let root =
            std::env::temp_dir().join(format!("aterm-title-ca-safe-open-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir(&root).unwrap();
        let target = root.join("target.pem");
        let link = root.join("link.pem");
        let fifo = root.join("fifo.pem");
        std::fs::write(&target, b"not a certificate").unwrap();
        symlink(&target, &link).unwrap();
        assert!(load_ca_bundle(link.to_str().unwrap()).is_err());
        let fifo_c = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: fifo_c is a valid NUL-terminated path owned for this call.
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        let start = Instant::now();
        assert!(load_ca_bundle(fifo.to_str().unwrap()).is_err());
        // Failure bound only — a genuine regression blocks or never happens at all,
        // so any finite deadline catches it and tightness only lets load fake a
        // failure. This bound has already crossed under full-suite load elsewhere
        // in this repo (cb8c0cff, c1281c6a).
        assert!(start.elapsed() < Duration::from_secs(30));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn security_subprocess_runner_bounds_time_and_output() {
        let start = Instant::now();
        let timeout = run_command_bounded(
            std::process::Command::new("/bin/sh").args(["-c", "sleep 5"]),
            Duration::from_millis(50),
            128,
            "test helper",
        )
        .unwrap_err();
        assert!(timeout.contains("timed out"));
        // Failure bound only — a genuine regression blocks or never happens at all,
        // so any finite deadline catches it and tightness only lets load fake a
        // failure. This bound has already crossed under full-suite load elsewhere
        // in this repo (cb8c0cff, c1281c6a).
        assert!(start.elapsed() < Duration::from_secs(30));

        let start = Instant::now();
        let inherited_pipe = run_command_bounded(
            std::process::Command::new("/bin/sh").args(["-c", "sleep 5 & exit 0"]),
            Duration::from_millis(50),
            128,
            "test helper descendant",
        )
        .unwrap_err();
        assert!(inherited_pipe.contains("timed out"));
        // Failure bound only — a genuine regression blocks or never happens at all,
        // so any finite deadline catches it and tightness only lets load fake a
        // failure. This bound has already crossed under full-suite load elsewhere
        // in this repo (cb8c0cff, c1281c6a).
        assert!(start.elapsed() < Duration::from_secs(30));

        let oversized = run_command_bounded(
            std::process::Command::new("/bin/sh").args(["-c", "printf '%02048d' 0"]),
            Duration::from_secs(1),
            128,
            "test helper",
        )
        .unwrap_err();
        assert!(oversized.contains("byte limit"));
    }

    #[cfg(unix)]
    #[test]
    fn managed_private_home_is_fresh_owner_only_and_substitution_safe() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};

        let home = create_private_managed_home().unwrap();
        let metadata = std::fs::symlink_metadata(&home).unwrap();
        assert!(metadata.file_type().is_dir());
        assert_eq!(metadata.mode() & 0o0777, 0o700);
        validate_private_managed_home(&home, true).unwrap();

        std::fs::write(home.join("injected-state"), b"not fresh").unwrap();
        assert!(validate_private_managed_home(&home, true).is_err());
        std::fs::remove_file(home.join("injected-state")).unwrap();
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(validate_private_managed_home(&home, true).is_err());
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();

        let link = home.with_file_name(format!(
            "aterm-ollama-home-{}-symlink-test",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&link);
        symlink(&home, &link).unwrap();
        assert!(validate_private_managed_home(&link, true).is_err());
        cleanup_private_managed_home(Some(&link));
        assert!(
            link.is_symlink(),
            "cleanup must never follow a substituted link"
        );
        std::fs::remove_file(link).unwrap();

        let command = managed_ollama_command(
            std::path::Path::new("/bin/echo"),
            "127.0.0.1:1",
            std::path::Path::new("/tmp/models"),
            &home,
        );
        assert_eq!(command.get_current_dir(), Some(home.as_path()));
        cleanup_private_managed_home(Some(&home));
        assert!(!home.exists());
    }

    #[cfg(unix)]
    #[test]
    fn controller_rejects_shared_caller_group_without_harming_caller() {
        // SAFETY: getpgrp/getpid only read caller identity.
        let caller_group = unsafe { libc::getpgrp() };
        let caller_pid = unsafe { libc::getpid() };
        let controller = ManagedOllamaController::new(Some(7));
        let child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        assert_eq!(
            i32::try_from(unix_process_group(child.id()).unwrap()).unwrap(),
            caller_group
        );
        assert!(
            controller
                .install(child, "unsafe".to_string(), 7, None)
                .is_err()
        );
        // SAFETY: signal 0 probes that this test process survived the fail-safe path.
        assert_eq!(unsafe { libc::kill(caller_pid, 0) }, 0);
        assert_eq!(unsafe { libc::getpgrp() }, caller_group);
    }

    #[cfg(unix)]
    #[test]
    fn failed_admission_kills_fast_exit_leaders_descendant_group() {
        let (mut child, descendant) = spawn_dedicated_group_with_descendant(false);
        let private_home = create_private_managed_home().unwrap();
        let _ = child.wait();
        assert!(process_exists(descendant));

        terminate_unadmitted_managed_child(child, Some(private_home.clone()));
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline && (process_exists(descendant) || private_home.exists()) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            !process_exists(descendant),
            "failed admission left a managed descendant alive"
        );
        assert!(
            !private_home.exists(),
            "private HOME was not cleaned after the failed group stopped"
        );
    }

    #[cfg(unix)]
    #[test]
    fn revocation_kills_the_entire_dedicated_descendant_group() {
        let controller = ManagedOllamaController::new(Some(7));
        let (child, descendant) = spawn_dedicated_group_with_descendant(true);
        let root = child.id();
        controller
            .install(child, "group".to_string(), 7, None)
            .unwrap();
        assert!(process_exists(root));
        assert!(process_exists(descendant));
        let start = Instant::now();
        controller.transition_to(None);
        // The signal + non-blocking spawn is microseconds; 100ms measured the
        // scheduler. Reaping is outstanding either way, so this is a failure bound.
        assert!(start.elapsed() < Duration::from_secs(5));
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline && (process_exists(root) || process_exists(descendant)) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            !process_exists(root),
            "managed group leader survived revocation"
        );
        assert!(
            !process_exists(descendant),
            "managed descendant survived revocation"
        );
    }

    #[cfg(unix)]
    #[test]
    fn direct_child_crash_reaps_and_kills_surviving_group_descendant() {
        let controller = ManagedOllamaController::new(Some(9));
        let (child, descendant) = spawn_dedicated_group_with_descendant(true);
        let root = child.id();
        controller
            .install(child, "crash".to_string(), 9, None)
            .unwrap();
        // Crash only the admitted root. Its background descendant remains in the
        // dedicated group until controller.reap observes the root exit.
        // SAFETY: root is the exact child identity just installed above.
        assert_eq!(
            unsafe { libc::kill(i32::try_from(root).unwrap(), libc::SIGKILL) },
            0
        );
        let deadline = Instant::now() + Duration::from_secs(60);
        let event = loop {
            if let Some(event) = controller.reap() {
                break event;
            }
            assert!(Instant::now() < deadline, "direct child did not exit");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(event.endpoint, "crash");
        assert_eq!(event.authority_epoch, 9);
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline && process_exists(descendant) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            !process_exists(descendant),
            "crashed root left a daemon descendant"
        );
    }

    #[cfg(unix)]
    #[test]
    fn managed_runtime_controller_kills_without_waiting_on_request_timeout() {
        let controller = ManagedOllamaController::new(Some(7));
        let child = spawn_dedicated_test_child();
        let pid = child.id();
        controller
            .install(child, "test".to_string(), 7, None)
            .unwrap();
        let (process, selected) = controller
            .owned_for_authority(7)
            .expect("the authority reuses its selected endpoint");
        assert_eq!(process.pid, pid);
        assert_eq!(selected, "test");
        assert!(controller.owned_for_authority(8).is_none());
        let start = Instant::now();
        controller.stop();
        // Failure bound only — a genuine regression blocks or never happens at all,
        // so any finite deadline catches it and tightness only lets load fake a
        // failure. This bound has already crossed under full-suite load elsewhere
        // in this repo (cb8c0cff, c1281c6a).
        assert!(start.elapsed() < Duration::from_secs(30));
        for _ in 0..100 {
            // SAFETY: signal 0 only queries the child PID's existence.
            if unsafe { libc::kill(i32::try_from(pid).unwrap(), 0) } == -1 {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("managed child was not killed and reaped");
    }

    #[cfg(unix)]
    #[test]
    fn final_session_retirement_stops_pinned_daemon_and_cleans_private_home() {
        let mut coordinator = Coordinator::new(None);
        let config = Config {
            title_summary_provider: Some(TitleSummaryProvider::Ollama),
            ..Config::default()
        };
        assert!(coordinator.reconfigure(&config));
        let (request_rx, _result_tx) = install_test_worker(&mut coordinator);
        let epoch = coordinator.authority_epoch;
        let controller = coordinator.worker.as_ref().unwrap().ollama.clone();
        controller.transition_to(Some(epoch));
        let term = Terminal::new(2, 20);
        coordinator.observe(7, &term, &config, true, Instant::now());
        let _ = request_rx.try_recv().unwrap();

        let home = create_private_managed_home().unwrap();
        let child = spawn_dedicated_test_child();
        let pid = child.id();
        controller
            .install(
                child,
                "http://owned.test".to_string(),
                epoch,
                Some(home.clone()),
            )
            .unwrap();
        assert!(controller.owns_endpoint("http://owned.test", epoch));

        coordinator.retire(7);
        assert!(coordinator.worker.is_none());
        assert!(!controller.owns_endpoint("http://owned.test", epoch));
        assert_eq!(coordinator.runtime_state, TitleSummaryRuntimeState::Idle);
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline && (process_exists(pid) || home.exists()) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!process_exists(pid));
        assert!(
            !home.exists(),
            "private managed HOME survived final retirement"
        );
    }

    #[cfg(unix)]
    #[test]
    fn owned_security_failure_tears_down_process_and_clears_guards() {
        let authority_epoch = 13;
        let controller = ManagedOllamaController::new(Some(authority_epoch));
        let child = spawn_dedicated_test_child();
        let pid = child.id();
        controller
            .install(child, "secured".to_string(), authority_epoch, None)
            .unwrap();
        let mut ollama = ManagedOllama::new(controller.clone());
        ollama.runtime_paths = Some(("/tampered/runtime".into(), "/tampered/models".into()));
        // model_store's coherent-replacement test proves detection; this binds the
        // shipping failure transition to group teardown and guard clearing.
        ollama.invalidate_owned("secured", authority_epoch);
        assert!(!controller.owns_endpoint("secured", authority_epoch));
        assert!(ollama.runtime_paths.is_none());
        assert!(ollama._model_attestation.is_none());
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline && process_exists(pid) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!process_exists(pid));
    }

    /// Tier-1 binding for the derived two-process endpoint lifecycle. Real
    /// reservations and controller ownership project onto the model's abstract
    /// endpoint identities; the mutant's shared identity is the negative control.
    #[cfg(unix)]
    #[test]
    fn managed_endpoint_ownership_conforms_to_derived_lifecycle_model() {
        let model = aterm_spec::derive::title_summary_managed_endpoint_model();
        let first_reservation = reserve_managed_endpoint().unwrap();
        let second_reservation = reserve_managed_endpoint().unwrap();
        let first_target = first_reservation.into_target();
        let second_target = second_reservation.into_target();
        assert_ne!(first_target.endpoint, second_target.endpoint);

        let first = ManagedOllamaController::new(Some(1));
        let second = ManagedOllamaController::new(Some(1));
        first
            .install(
                spawn_dedicated_test_child(),
                first_target.endpoint.clone(),
                1,
                None,
            )
            .unwrap();
        second
            .install(
                spawn_dedicated_test_child(),
                second_target.endpoint.clone(),
                1,
                None,
            )
            .unwrap();
        assert_eq!(
            first.owned_for_authority(1).unwrap().1,
            first_target.endpoint
        );
        assert_eq!(
            second.owned_for_authority(1).unwrap().1,
            second_target.endpoint
        );

        let launched1 = model.successors("Launch1", &model.init_state())[0].clone();
        let launched2 = model.successors("Launch2", &launched1)[0].clone();
        assert!(model.check_invariant("ConcurrentAutomaticEndpointsAreDistinct", &launched2));
        let reused = model.successors("Reuse1", &launched2)[0].clone();
        first.transition_to(None);
        let revoked = model.successors("Reconfigure1", &reused)[0].clone();
        assert!(first.owned_for_authority(1).is_none());
        assert_eq!(revoked["endpoint1"], 0);
        assert_eq!(revoked["health_endpoint1"], 0);
        second.stop();

        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let first = buggy.successors("Launch1", &buggy.init_state())[0].clone();
        let collision = buggy.successors("Launch2", &first)[0].clone();
        assert!(!buggy.check_invariant("ConcurrentAutomaticEndpointsAreDistinct", &collision,));
    }

    #[cfg(unix)]
    #[test]
    fn managed_runtime_rejects_a_late_install_after_revocation() {
        let controller = ManagedOllamaController::new(Some(11));
        controller.transition_to(Some(12));
        let child = spawn_dedicated_test_child();
        let pid = child.id();
        assert!(
            controller
                .install(child, "test".to_string(), 11, None)
                .is_err()
        );
        assert!(!controller.owns_endpoint("test", 11));
        for _ in 0..100 {
            // SAFETY: signal 0 only queries the child PID's existence.
            if unsafe { libc::kill(i32::try_from(pid).unwrap(), 0) } == -1 {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("revoked late install was not killed and reaped");
    }

    #[cfg(unix)]
    #[test]
    fn every_exact_authority_transition_revokes_the_owned_runtime() {
        let mut coordinator = Coordinator::new(None);
        let mut config = Config {
            title_summary_provider: Some(TitleSummaryProvider::Ollama),
            ..Config::default()
        };
        assert!(coordinator.reconfigure(&config));
        assert!(coordinator.ensure_worker());
        let epoch = coordinator.authority_epoch;
        let controller = coordinator.worker.as_ref().unwrap().ollama.clone();
        let child = spawn_dedicated_test_child();
        let pid = child.id();
        controller
            .install(child, "http://owned.test".to_string(), epoch, None)
            .unwrap();
        assert!(controller.owns_endpoint("http://owned.test", epoch));

        config.title_summary_model = Some("different-model".to_string());
        assert!(coordinator.reconfigure(&config));
        assert!(!controller.owns_endpoint("http://owned.test", epoch));
        for _ in 0..100 {
            // SAFETY: signal 0 only queries the child PID's existence.
            if unsafe { libc::kill(i32::try_from(pid).unwrap(), 0) } == -1 {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("authority transition did not kill and reap managed runtime");
    }

    #[test]
    fn failed_cold_replacement_can_recreate_a_fresh_worker_authority() {
        let mut coordinator = Coordinator::new(None);
        let config = Config {
            title_summary_provider: Some(TitleSummaryProvider::Ollama),
            ..Config::default()
        };
        assert!(coordinator.reconfigure(&config));
        assert!(coordinator.ensure_worker());
        let before = coordinator.authority_epoch;
        coordinator.shutdown();
        assert!(coordinator.worker.is_none());
        assert!(coordinator.authority.is_none());
        assert!(coordinator.reconfigure(&config));
        assert!(coordinator.authority_epoch > before);
        assert!(coordinator.ensure_worker());
    }

    #[test]
    fn non_inference_authorities_retire_worker_and_reenable_starts_fresh() {
        let mut coordinator = Coordinator::new(None);
        let inference = Config {
            title_summary_provider: Some(TitleSummaryProvider::Ollama),
            ..Config::default()
        };
        assert!(coordinator.reconfigure(&inference));
        assert!(coordinator.ensure_worker());
        assert!(coordinator.worker.is_some());

        for config in [
            Config {
                title_summary_provider: Some(TitleSummaryProvider::Off),
                ..Config::default()
            },
            Config {
                title_summary_provider: Some(TitleSummaryProvider::Builtin),
                ..Config::default()
            },
            Config {
                descriptive_titles: Some(false),
                title_summary_provider: Some(TitleSummaryProvider::OpenAiCompatible),
                ..Config::default()
            },
        ] {
            assert!(coordinator.reconfigure(&config));
            assert!(
                coordinator.worker.is_none(),
                "non-inference authority retained a background worker"
            );
            assert!(coordinator.reconfigure(&inference));
            assert!(coordinator.ensure_worker());
            assert!(coordinator.worker.is_some());
        }
    }

    /// Tier-1 conformance: project real completion-guard inputs from reachable states
    /// of the same derived model checked in `aterm-spec`. The stale-generation,
    /// stale-config, and disable/re-enable controls must all be rejected, so this pass
    /// cannot be vacuous.
    #[test]
    fn completion_guard_conforms_to_derived_title_summary_model() {
        let model = aterm_spec::derive::title_summary_model();
        let requested = model.successors("Request", &model.init_state())[0].clone();
        let running = model.successors("Start", &requested)[0].clone();
        let fresh = |state: &aterm_spec::interp::State, enabled: bool| {
            let config_generation = u64::try_from(state["config_generation"]).unwrap();
            let current = AuthorityStamp {
                epoch: config_generation,
                fingerprint: 99,
            };
            completion_is_fresh(
                enabled,
                current,
                RequestStamp {
                    generation: u64::try_from(state["current_generation"]).unwrap(),
                    authority: current,
                },
                RequestStamp {
                    generation: u64::try_from(state["job_generation"]).unwrap(),
                    authority: AuthorityStamp {
                        epoch: u64::try_from(state["job_config"]).unwrap(),
                        fingerprint: 99,
                    },
                },
            )
        };
        assert!(fresh(&running, true), "current completion must publish");

        let superseded = model.successors("Request", &running)[0].clone();
        assert!(!fresh(&superseded, true), "stale content must be rejected");

        let throttled_boundary = model.successors("Boundary", &running)[0].clone();
        assert!(
            !fresh(&throttled_boundary, true),
            "a boundary with no admitted replacement must still revoke A"
        );

        let reconfigured = model.successors("Reconfigure", &running)[0].clone();
        assert!(!fresh(&reconfigured, true), "stale config must be rejected");

        let disabled = model.successors("Disable", &running)[0].clone();
        let reenabled = model.successors("Enable", &disabled)[0].clone();
        assert!(
            !fresh(&reenabled, true),
            "pre-disable completion must stay stale after re-enable"
        );
        assert!(!fresh(&running, false), "disabled completion must be inert");
        // Explicit ABA negative control: resolved settings/fingerprint returned to A,
        // but the monotonic authority epoch proves this job predates A→Off→A.
        assert!(!completion_is_fresh(
            true,
            AuthorityStamp {
                epoch: 3,
                fingerprint: 99,
            },
            RequestStamp {
                generation: 1,
                authority: AuthorityStamp {
                    epoch: 3,
                    fingerprint: 99,
                },
            },
            RequestStamp {
                generation: 1,
                authority: AuthorityStamp {
                    epoch: 1,
                    fingerprint: 99,
                },
            },
        ));
    }

    /// Tier-1 conformance for the presentation half of periodic refresh: real UI
    /// code retains an authorized refinement exactly when the derived `Refresh`
    /// transition does, while semantic/provider boundaries reset immediately.
    #[test]
    fn refresh_presentation_decision_conforms_to_derived_model() {
        let model = aterm_spec::derive::title_summary_model();
        let requested = model.successors("Request", &model.init_state())[0].clone();
        let running = model.successors("Start", &requested)[0].clone();
        let refined = model.successors("Complete", &running)[0].clone();
        let refreshed = model.successors("Refresh", &refined)[0].clone();
        assert_eq!(
            refreshed["applied_generation"],
            refined["applied_generation"]
        );
        assert!(!should_reset_description(
            TitleSummaryProvider::Ollama,
            false,
            false,
        ));

        let semantic_boundary = model.successors("Request", &refined)[0].clone();
        assert_eq!(semantic_boundary["applied_generation"], 0);
        assert!(should_reset_description(
            TitleSummaryProvider::Ollama,
            true,
            false,
        ));
        assert!(should_reset_description(
            TitleSummaryProvider::Ollama,
            false,
            true,
        ));
        assert!(should_reset_description(
            TitleSummaryProvider::Builtin,
            false,
            false,
        ));
    }

    #[test]
    fn coordinator_authority_epoch_rejects_real_a_off_a_aba() {
        let mut coordinator = Coordinator::new(None);
        let a = Config {
            title_summary_provider: Some(TitleSummaryProvider::Ollama),
            ..Config::default()
        };
        assert!(coordinator.reconfigure(&a));
        let epoch_a = coordinator.authority_epoch;
        let fingerprint_a = coordinator.authority_fingerprint;
        let (session_epoch, session_authority) = coordinator.session_authority(7);
        let stale_job = Job {
            session: 7,
            session_epoch,
            session_authority,
            generation: 1,
            authority_epoch: epoch_a,
            config_fingerprint: fingerprint_a,
            settings: provider_settings(&a).unwrap(),
            snapshot: snap("cargo test", ActivityState::Executing, None),
        };
        coordinator.pending.insert(7, stale_job.clone());

        let mut off = a.clone();
        off.title_summary_provider = Some(TitleSummaryProvider::Off);
        assert!(coordinator.reconfigure(&off));
        assert!(coordinator.pending.is_empty());
        assert!(coordinator.reconfigure(&a));
        let epoch_restored_a = coordinator.authority_epoch;
        assert!(epoch_restored_a > epoch_a);
        assert_eq!(coordinator.authority_fingerprint, fingerprint_a);
        assert_eq!(
            coordinator.worker_authority_epoch.load(Ordering::Acquire),
            epoch_restored_a
        );
        // Defensive dispatch gate rejects even a stale job reintroduced after the
        // transition (negative control for the modeled Start/send guard).
        coordinator.pending.insert(7, stale_job);
        coordinator.dispatch_next();
        assert!(coordinator.pending.is_empty());
        assert!(coordinator.in_flight.is_none());
        assert!(!completion_is_fresh(
            true,
            AuthorityStamp {
                epoch: epoch_restored_a,
                fingerprint: fingerprint_a,
            },
            RequestStamp {
                generation: 1,
                authority: AuthorityStamp {
                    epoch: epoch_restored_a,
                    fingerprint: fingerprint_a,
                },
            },
            RequestStamp {
                generation: 1,
                authority: AuthorityStamp {
                    epoch: epoch_a,
                    fingerprint: fingerprint_a,
                },
            },
        ));
    }

    /// The shipping retry projection ([`retry_pending_after`]) and the derived
    /// `title_summary_model` must agree on which transitions leave a retry
    /// armed. Drives the model through the real transition names and compares
    /// its `retry_pending` against the shipping answer, so a policy change on
    /// either side that is not made on both fails here.
    #[test]
    fn observation_retry_transitions_conform_to_derived_model() {
        let model = aterm_spec::derive::title_summary_model();
        let armed = model.successors("LockContended", &model.init_state())[0].clone();
        assert_eq!(
            armed["retry_pending"] == 1,
            retry_pending_after(ObservationRetryTransition::Contended)
        );
        let still_armed = model.successors("RetryContended", &armed)[0].clone();
        assert_eq!(
            still_armed["retry_pending"] == 1,
            retry_pending_after(ObservationRetryTransition::Contended)
        );
        let succeeded = model.successors("ObserveSuccess", &still_armed)[0].clone();
        assert_eq!(
            succeeded["retry_pending"] == 1,
            retry_pending_after(ObservationRetryTransition::Succeeded)
        );
        let disabled = model.successors("Disable", &armed)[0].clone();
        assert_eq!(
            disabled["retry_pending"] == 1,
            retry_pending_after(ObservationRetryTransition::Disabled)
        );
        let retired = model.successors("Retire", &armed)[0].clone();
        assert_eq!(
            retired["retry_pending"] == 1,
            retry_pending_after(ObservationRetryTransition::Retired)
        );
    }

    /// THE FOLD-SEAM FLOOR (busy-rearm audit, item 4). Due-but-unserviced is
    /// legal coordinator state — the one-observation-per-turn admission bound,
    /// a contended `try_lock`, an untracked session awaiting discovery — but
    /// `about_to_wait` arms whatever this seam reports as a `WaitUntil`, and a
    /// past instant (or a bare `Instant::now()`, past by fold time) fires
    /// immediately, re-arms, and spins the loop at turn rate until the queue
    /// drains. The seam must floor an already-due deadline to the retry
    /// ladder's base; the paced work happens on the next turn either way.
    #[test]
    fn an_already_due_title_deadline_reaches_the_event_loop_floored() {
        let mut app = crate::App::headless_for_test();
        app.config.descriptive_titles = Some(true);
        app.config.title_summary_provider = Some(TitleSummaryProvider::Builtin);

        // An untracked session owes discovery: the old code reported a bare
        // `Instant::now()` for it, every turn, until discovery completed.
        assert!(!app.title_summary_tracks_session(0));
        let deadline = app
            .next_title_summary_retry()
            .expect("a missing session owes discovery");
        let after = Instant::now();
        assert!(
            deadline > after,
            "the arm must still be in the future when the fold reads it"
        );
        assert!(
            deadline <= after + Coordinator::OBSERVE_RETRY_BASE,
            "but only by the retry ladder's base, so discovery stays prompt"
        );

        // Same law for a TRACKED session whose retry stamp already elapsed
        // (the drain case): the stamp stays honest inside the coordinator,
        // and only the WAKE reported to the loop is re-timed.
        app.retry_title_observations();
        assert!(app.title_summary_tracks_session(0));
        app.title_summaries
            .retries
            .insert(0, Instant::now() - Duration::from_secs(1));
        let deadline = app
            .next_title_summary_retry()
            .expect("an elapsed retry stays scheduled");
        let after = Instant::now();
        assert!(
            deadline > after,
            "an elapsed retry is floored, never fed back verbatim"
        );
        assert!(deadline <= after + Coordinator::OBSERVE_RETRY_BASE);
    }
}
