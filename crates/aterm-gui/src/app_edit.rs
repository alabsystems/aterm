// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `edit` tickets on the main thread (amacs design §6.3-§6.6; slice N1).
//!
//! A program in a tab asks, through `@s-<sid> edit <file-uri>`
//! (`control_edit`), for a NEW Editor view of a file in a tab beside its own,
//! then waits with `await edit` until that view is gone. One ticket names one
//! such request from admission to its one outcome:
//!
//! - **Admitting.** [`App::begin_edit`] resolves the window hosting the
//!   session (never the front one), records the ticket and starts the
//!   two-phase document admission under a key no gesture shares
//!   (`DocumentAdmissionKey::edit`), so it never coalesces.
//! - **Open.** [`App::finish_edit_admission`] installs a fresh view
//!   ([`Placement::NewTabAfter`]: a view of the file the person already has
//!   open is never reused or focused), places its tab right of the caller's
//!   ([`App::place_admitted_view`], the askpass focus rule), moves the caret,
//!   holds the session (`control_edit::hold`) and answers `OK edit ticket=…`.
//!   A view opened behind the person's tab marks its tab for attention until
//!   the person brings it to the front ([`App::edit_view_attention`], folded
//!   into the tab's presentation, so no presentation refresh drops it).
//! - **Resolved.** [`App::edit_view_retired`], called from the one
//!   view-retirement seam (`App::remove_view_link`), resolves the ticket when
//!   ITS view leaves. A dirty view's close reaches that seam only after the
//!   close protocol's `checkpoint_ack`, so an answer never precedes the save;
//!   another view of the same file closing answers nothing. Outcomes are kept
//!   in a ring of [`RESOLVED_RING`] for a waiter that arrives late.
//! - **Orphaned.** When the last waiter leaves (a hangup, a timeout) and none
//!   returns within [`ORPHAN_AFTER`], the ticket is orphaned: its hold lapses
//!   and its view stays (an orphan never closes its view). A waiter that comes
//!   back adopts it again. An `edit` that gave up just as its view opened
//!   ([`App::cancel_edit`]) orphans it at once, unless a waiter already holds it.
//!
//! A waiter no open ticket covers any more (its ticket resolved, was refused
//! or was cancelled) is answered what it would be answered arriving now
//! ([`App::settle_uncovered_edit_waiters`]).
//!
//! The laws are the `AmacsEditTicket` model's
//! (`aterm_models_native::models_amacs_ticket`), bound here by `refines`
//! anchors and checked against this code by `edit_ticket_conformance`.

use std::collections::{BTreeMap, VecDeque};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use amacs::protocol::edit::{AwaitKey, EditOk, EditOutcome, EditResult, Ticket};

use crate::app_documents::{DocumentAdmissionKey, DocumentOpenFailure, LineReveal};
use crate::control_edit::{AwaitCall, EditCall, WaiterId, hold};
use crate::document_store::DocumentId;
use crate::native_app::AppKind;
use crate::native_document_host::AdmittedDocument;
use crate::tab_model::{TabId, ViewId};
use crate::{App, WindowId};

/// An `edit` ticket's identity: the wire's [`Ticket`].
pub(crate) type EditTicketId = Ticket;

/// Where an admitted document's view goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Placement {
    /// A gesture's open: focus a view of the document already in the window,
    /// else open a new tab at the end.
    FocusOrNewTab,
    /// An `edit` ticket's view: always a fresh view, in a new tab right of the
    /// caller's ([`App::place_admitted_view`]).
    NewTabAfter { ticket: EditTicketId },
}

/// How many resolved outcomes are kept for a waiter that arrives after its
/// ticket resolved.
pub(crate) const RESOLVED_RING: usize = 64;

/// How long a ticket nobody waits on keeps its waiter's place: past it the
/// ticket is orphaned (design §6.6).
pub(crate) const ORPHAN_AFTER: Duration = Duration::from_secs(5);

/// One open ticket's stage.
#[derive(Debug)]
enum Phase {
    /// The admission is in flight; `reply` answers the `edit` request.
    Admitting { reply: Sender<String> },
    /// The view is installed.
    Open {
        view: ViewId,
        tab: TabId,
        document: DocumentId,
        canonical_uri: String,
        /// The document's durable checkpoint when the view opened: a save
        /// counts for `saved=1` only once the checkpoint passes it.
        start: aterm_buffer::Seq,
    },
}

/// A ticket from its request until it resolves.
#[derive(Debug)]
struct OpenTicket {
    session: u64,
    window: WindowId,
    /// The caller's tab, the new tab's left neighbour.
    caller_tab: TabId,
    /// The file as requested, in the codec's canonical spelling.
    uri: String,
    /// The file's realpath spelling, when the request's lane resolved it
    /// (`control_edit::real_file_uri`): what the admission's grant will name.
    real_uri: Option<String>,
    line: Option<(u32, Option<u32>)>,
    phase: Phase,
    /// Since when no waiter has been parked on it: the orphan clock.
    unwaited_since: Option<Instant>,
    /// The clock ran out ([`App::sweep_orphaned_edits`]).
    orphaned: bool,
    /// The view opened behind the tab the person is on: its tab carries the
    /// attention mark until the person brings it to the front.
    attention: bool,
}

impl OpenTicket {
    /// Whether `uri` (canonical spelling) names this ticket's file, as
    /// requested, resolved or admitted.
    fn names(&self, uri: &str) -> bool {
        self.uri == uri
            || self.real_uri.as_deref() == Some(uri)
            || matches!(&self.phase, Phase::Open { canonical_uri, .. } if canonical_uri == uri)
    }

    fn view(&self) -> Option<ViewId> {
        match self.phase {
            Phase::Open { view, .. } => Some(view),
            Phase::Admitting { .. } => None,
        }
    }
}

/// A ticket's one outcome, kept in the ring.
#[derive(Debug)]
struct Resolved {
    ticket: Ticket,
    uri: String,
    canonical_uri: String,
    outcome: EditOutcome,
}

/// A parked `await edit`.
#[derive(Debug)]
struct Waiter {
    key: AwaitKey,
    /// A `uri=` key's spellings of the file: the codec's canonical one and,
    /// when the lane resolved it, the realpath one. Empty for `ticket=`.
    spellings: Vec<String>,
    reply: Sender<String>,
    /// A `uri=` wait this process has no ticket for (its server was replaced):
    /// done when no Editor view of the file remains.
    by_views: bool,
}

impl Waiter {
    /// Whether this waiter waits on open ticket `ticket`.
    fn covers(&self, ticket: Ticket, entry: &OpenTicket) -> bool {
        match &self.key {
            AwaitKey::Ticket(t) => *t == ticket,
            AwaitKey::Uri(_) => !self.by_views && self.spellings.iter().any(|uri| entry.names(uri)),
        }
    }

    /// Whether some ticket in `open` covers this waiter.
    fn covered_by_any(&self, open: &BTreeMap<Ticket, OpenTicket>) -> bool {
        open.iter()
            .any(|(ticket, entry)| self.covers(*ticket, entry))
    }
}

/// What a waiter no open ticket covers is told.
enum Uncovered {
    /// This line, now.
    Answer(String),
    /// Nothing yet: it waits until no Editor view of its file remains.
    ByViews,
}

/// Every edit ticket this process issued and still knows.
#[derive(Debug, Default)]
pub(crate) struct EditTickets {
    open: BTreeMap<Ticket, OpenTicket>,
    resolved: VecDeque<Resolved>,
    waiters: BTreeMap<WaiterId, Waiter>,
    /// Test seam: serve `edit` on the headless test App, which production
    /// refuses (`ERR unsupported headless`).
    #[cfg(test)]
    serve_headless: bool,
}

impl EditTickets {
    fn serves_headless(&self) -> bool {
        #[cfg(test)]
        {
            self.serve_headless
        }
        #[cfg(not(test))]
        {
            false
        }
    }

    /// Whether some parked waiter is waiting on `ticket`.
    fn waited(&self, ticket: Ticket) -> bool {
        let Some(entry) = self.open.get(&ticket) else {
            return false;
        };
        self.waiters
            .values()
            .any(|waiter| waiter.covers(ticket, entry))
    }

    fn ticket_of_view(&self, view: ViewId) -> Option<Ticket> {
        self.open
            .iter()
            .find(|(_, entry)| entry.view() == Some(view))
            .map(|(ticket, _)| *ticket)
    }

    fn remember(&mut self, resolved: Resolved) {
        if self.resolved.len() == RESOLVED_RING {
            self.resolved.pop_front();
        }
        self.resolved.push_back(resolved);
    }

    fn outcome_of(&self, ticket: Ticket) -> Option<EditOutcome> {
        self.resolved
            .iter()
            .rev()
            .find(|resolved| resolved.ticket == ticket)
            .map(|resolved| resolved.outcome)
    }

    fn latest_outcome_for(&self, spellings: &[String]) -> Option<EditOutcome> {
        self.resolved
            .iter()
            .rev()
            .find(|resolved| {
                spellings
                    .iter()
                    .any(|uri| resolved.uri == *uri || resolved.canonical_uri == *uri)
            })
            .map(|resolved| resolved.outcome)
    }
}

/// The codec's canonical spelling of `uri` (`%XX` upper case, `localhost`
/// dropped), so two spellings of one file meet; `uri` itself when it names no
/// local absolute path.
fn canonical_spelling(uri: &str) -> String {
    amacs::protocol::uri::file_uri_to_path(uri)
        .and_then(|path| amacs::protocol::uri::path_to_file_uri(&path))
        .unwrap_or_else(|_| uri.to_string())
}

/// The outcome's reply line.
fn outcome_line(outcome: EditOutcome) -> String {
    format!("{}\n", outcome.format())
}

/// `text` on one line, for an `ERR` reply.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl App {
    /// `Wake::EditRequest`: start one `edit` (see the module doc). Every
    /// refusal answers the request at once; an admitted request is answered
    /// by [`Self::finish_edit_admission`].
    pub(crate) fn begin_edit(&mut self, call: EditCall) {
        let ticket = call.ticket;
        let Some((window, uri)) = self.record_edit_ticket(call) else {
            return;
        };
        if let Err(why) = self.request_edit_admission(window, &uri, ticket)
            && let Some(OpenTicket {
                phase: Phase::Admitting { reply },
                ..
            }) = self.native_documents.edits.open.remove(&ticket)
        {
            let _ = reply.send(format!("ERR {}\n", one_line(&why)));
            self.settle_uncovered_edit_waiters();
        }
    }

    /// [`Self::begin_edit`]'s checks: answer a refusal and return `None`, or
    /// record the ticket as admitting and return its window and the file's
    /// canonical spelling.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AmacsEditTicket",
            action = "Request",
            project = "aterm_gui::app_edit::edit_ticket_conformance::Rig::project"
        )
    )]
    fn record_edit_ticket(&mut self, call: EditCall) -> Option<(WindowId, String)> {
        self.sweep_orphaned_edits(Instant::now());
        let EditCall {
            ticket,
            session,
            request,
            real_uri,
            reply,
        } = call;
        let refuse = |line: &str| {
            let _ = reply.send(line.to_string());
            None
        };
        // A daemon with no glass has nobody to edit for; the client edits in
        // its own terminal instead.
        if self.headless && !self.native_documents.edits.serves_headless() {
            return refuse("ERR unsupported headless\n");
        }
        if request.create {
            return refuse("ERR unsupported create\n");
        }
        let Some(window) = self.window_of_session(session) else {
            return refuse("ERR no-window\n");
        };
        let Some(caller_tab) = self.tab_hosting_session(window, session) else {
            return refuse("ERR no-window\n");
        };
        // The control surface may not raise the file-provider consent dialog
        // (`App::refuse_file_provider_open`).
        if Self::refuse_file_provider_open(&request.uri).is_err() {
            return refuse("ERR refused file-provider\n");
        }
        let uri = canonical_spelling(&request.uri);
        self.native_documents.edits.open.insert(
            ticket,
            OpenTicket {
                session,
                window,
                caller_tab,
                uri: uri.clone(),
                real_uri,
                line: request.line,
                phase: Phase::Admitting { reply },
                unwaited_since: None,
                orphaned: false,
                attention: false,
            },
        );
        Some((window, uri))
    }

    /// Test seam: [`Self::begin_edit`] with its admission left in flight, as
    /// an event loop's worker would leave it: the ticket is admitting and the
    /// admission minted; the caller completes it
    /// (`complete_document_admission`) or cancels it.
    #[cfg(test)]
    pub(crate) fn begin_edit_in_flight_for_test(
        &mut self,
        call: EditCall,
    ) -> Option<(
        WindowId,
        DocumentAdmissionKey,
        crate::app_documents::DocumentAdmissionTicket,
    )> {
        let ticket = call.ticket;
        let (window, uri) = self.record_edit_ticket(call)?;
        let key = DocumentAdmissionKey {
            uri,
            kind: AppKind::Editor,
            edit: Some(ticket),
        };
        let admission = self.mint_document_admission(window, key.clone());
        Some((window, key, admission))
    }

    /// The tab of window `wid` one of whose leaves is session `session`.
    fn tab_hosting_session(&self, wid: WindowId, session: u64) -> Option<TabId> {
        self.windows
            .get(&wid)?
            .tab_set
            .tabs()
            .iter()
            .find_map(|tab| {
                tab.root
                    .leaves()
                    .into_iter()
                    .any(|view| {
                        self.view_store
                            .get(view)
                            .copied()
                            .and_then(crate::tab_model::View::terminal_session)
                            == Some(session)
                    })
                    .then_some(tab.id)
            })
    }

    /// Phase two of an `edit` admission (`complete_document_admission`, or
    /// inline without an event loop): install the ticket's fresh view, or
    /// answer why not. `Ok` is the reply sent; `Err` says why nothing was
    /// installed, for the log only (the client has its answer, and nothing is
    /// shown to the person).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AmacsEditTicket",
            action = "Admit",
            project = "aterm_gui::app_edit::edit_ticket_conformance::Rig::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AmacsEditTicket",
            action = "Refuse",
            project = "aterm_gui::app_edit::edit_ticket_conformance::Rig::project"
        )
    )]
    pub(crate) fn finish_edit_admission(
        &mut self,
        wid: WindowId,
        key: DocumentAdmissionKey,
        result: Result<Box<AdmittedDocument>, DocumentOpenFailure>,
    ) -> Result<String, String> {
        let Some(ticket) = key.edit else {
            return Err(format!("{} is no edit admission", key.uri));
        };
        let admitting = self
            .native_documents
            .edits
            .open
            .get(&ticket)
            .is_some_and(|entry| matches!(entry.phase, Phase::Admitting { .. }));
        if !admitting {
            // Cancelled (its `edit` timed out or hung up): a late admission
            // opens nothing.
            return Err(format!(
                "edit ticket {ticket} was cancelled; its admission of {} is dropped",
                key.uri
            ));
        }
        let refusal = if !self.windows.contains_key(&wid) {
            Some("ERR no-window\n".to_string())
        } else {
            match &result {
                Ok(_) => None,
                Err(failure) => Some(format!("ERR refused {}\n", failure.0.refusal_word())),
            }
        };
        let installed = match (refusal, result) {
            (None, Ok(admitted)) => self.install_admitted_document(
                wid,
                AppKind::Editor,
                &key.uri,
                *admitted,
                Placement::NewTabAfter { ticket },
            ),
            (Some(line), _) => Err(line),
            (None, Err(failure)) => Err(format!("ERR refused {}\n", failure.0.refusal_word())),
        };
        let installed = match installed {
            Ok(installed) => installed,
            Err(why) => {
                let line = if why.starts_with("ERR ") {
                    why
                } else {
                    format!("ERR refused unreadable ({})\n", one_line(&why))
                };
                if let Some(OpenTicket {
                    phase: Phase::Admitting { reply },
                    ..
                }) = self.native_documents.edits.open.remove(&ticket)
                {
                    let _ = reply.send(line.clone());
                }
                self.settle_uncovered_edit_waiters();
                return Err(line.trim_end().to_string());
            }
        };
        let Some(entry) = self.native_documents.edits.open.get_mut(&ticket) else {
            return Err(format!("edit ticket {ticket} disappeared while installing"));
        };
        let reveal = entry.line;
        let session = entry.session;
        let attention = entry.attention;
        let start = self
            .document_store
            .checkpoint_seq(installed.document)
            .unwrap_or(aterm_buffer::Seq(0));
        let phase = std::mem::replace(
            &mut entry.phase,
            Phase::Open {
                view: installed.view,
                tab: installed.tab,
                document: installed.document,
                canonical_uri: installed.canonical_uri.clone(),
                start,
            },
        );
        let Phase::Admitting { reply } = phase else {
            return Err(format!("edit ticket {ticket} was not admitting"));
        };
        if let Some((line, col)) = reveal
            && let Err(why) =
                self.reveal_document_line_in_view(installed.view, LineReveal { line, col })
        {
            aterm_log::warn!("edit ticket {ticket}: could not move the caret: {why}");
        }
        if attention {
            // The mark is folded in from the ticket (`edit_view_attention`),
            // now that the ticket names its view.
            self.refresh_aggregate_tab_presentation(wid, installed.tab);
            self.refresh_window_tabs(wid);
            self.request_native_redraw(wid);
        }
        hold::set(
            session,
            hold::Hold {
                ticket,
                view: installed.view.get(),
            },
        );
        self.start_edit_orphan_clock_if_unwaited(ticket, Instant::now());
        let ok = EditOk {
            ticket,
            view: installed.view.get(),
            doc: installed.document.get(),
            uri: installed.canonical_uri,
        };
        let line = match ok.format() {
            Ok(line) => format!("{line}\n"),
            Err(error) => format!("ERR {error}\n"),
        };
        let _ = reply.send(line.clone());
        Ok(line.trim_end().to_string())
    }

    /// Place an `edit` ticket's new tab `tab` (just installed in window `wid`,
    /// and active) right of the caller's tab, by the askpass rule: it keeps
    /// focus when the caller's tab was the active tab of the front window
    /// (`previously_active`, captured before the install); otherwise the tab
    /// active before is re-selected and the new tab carries the attention mark
    /// (the ticket's, [`Self::edit_view_attention`]). No window is raised and
    /// nothing is posted.
    pub(crate) fn place_admitted_view(
        &mut self,
        wid: WindowId,
        tab: TabId,
        ticket: EditTicketId,
        previously_active: Option<TabId>,
    ) {
        let Some(caller_tab) = self
            .native_documents
            .edits
            .open
            .get(&ticket)
            .map(|entry| entry.caller_tab)
        else {
            return;
        };
        let front = self.frontmost_window == Some(wid);
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        if let Some(caller) = ws.tab_set.tabs().iter().position(|t| t.id == caller_tab) {
            ws.tab_set.reorder(tab, caller + 1);
        }
        let keeps_focus = front && previously_active == Some(caller_tab);
        if !keeps_focus
            && let Some(previous) = previously_active
            && ws.tab_set.switch_to(previous)
        {
            crate::app_tabs::align_terminal_projection_to_active(ws, &self.view_store);
        }
        let behind = ws.tab_set.active_id() != Some(tab);
        ws.last_present = None;
        if let Some(entry) = self.native_documents.edits.open.get_mut(&ticket) {
            entry.attention = behind;
        }
        self.resync_active_or_window(wid);
        self.request_native_redraw(wid);
    }

    /// `Wake::EditCancel`: the `edit` request stopped waiting. A ticket still
    /// admitting is forgotten, so its admission is dropped when it lands. One
    /// whose view opened meanwhile keeps its view: nobody learned its ticket,
    /// so it is orphaned at once, unless a waiter (by `uri=`) already holds it,
    /// which keeps it as any waiter does.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AmacsEditTicket",
            action = "Refuse",
            project = "aterm_gui::app_edit::edit_ticket_conformance::Rig::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AmacsEditTicket",
            action = "Orphan",
            project = "aterm_gui::app_edit::edit_ticket_conformance::Rig::project"
        )
    )]
    pub(crate) fn cancel_edit(&mut self, ticket: EditTicketId) {
        let now = Instant::now();
        let edits = &mut self.native_documents.edits;
        let waited = edits.waited(ticket);
        match edits.open.get_mut(&ticket) {
            Some(OpenTicket {
                phase: Phase::Admitting { .. },
                ..
            }) => {
                edits.open.remove(&ticket);
            }
            Some(entry) if !waited => {
                entry.unwaited_since = Some(now.checked_sub(ORPHAN_AFTER).unwrap_or(now));
                entry.orphaned = true;
                hold::clear(entry.session, ticket);
            }
            Some(_) | None => {}
        }
        self.sweep_orphaned_edits(now);
        self.settle_uncovered_edit_waiters();
    }

    /// `Wake::EditAwait`: answer at once from what is known, or park the
    /// waiter until its ticket (or every edit view of its file) is gone.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AmacsEditTicket",
            action = "Await",
            project = "aterm_gui::app_edit::edit_ticket_conformance::Rig::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AmacsEditTicket",
            action = "AwaitResolved",
            project = "aterm_gui::app_edit::edit_ticket_conformance::Rig::project"
        )
    )]
    pub(crate) fn await_edit(&mut self, call: AwaitCall) {
        let now = Instant::now();
        self.sweep_orphaned_edits(now);
        let AwaitCall {
            waiter: id,
            key,
            real_uri,
            reply,
        } = call;
        let spellings = match &key {
            AwaitKey::Uri(uri) => {
                let spelled = canonical_spelling(uri);
                let real = real_uri.filter(|real| *real != spelled);
                std::iter::once(spelled).chain(real).collect()
            }
            AwaitKey::Ticket(_) => Vec::new(),
        };
        let mut waiter = Waiter {
            key,
            spellings,
            reply,
            by_views: false,
        };
        let covered: Vec<Ticket> = self
            .native_documents
            .edits
            .open
            .iter()
            .filter(|(ticket, entry)| waiter.covers(**ticket, entry))
            .map(|(ticket, _)| *ticket)
            .collect();
        if covered.is_empty() {
            match self.uncovered_edit_waiter(&waiter) {
                Uncovered::Answer(line) => {
                    let _ = waiter.reply.send(line);
                    return;
                }
                Uncovered::ByViews => waiter.by_views = true,
            }
        }
        self.native_documents.edits.waiters.insert(id, waiter);
        // A waiter adopts its tickets: the orphan clock stops and the hold
        // stands again.
        for ticket in covered {
            let Some(entry) = self.native_documents.edits.open.get_mut(&ticket) else {
                continue;
            };
            entry.unwaited_since = None;
            entry.orphaned = false;
            if let Some(view) = entry.view() {
                hold::set(
                    entry.session,
                    hold::Hold {
                        ticket,
                        view: view.get(),
                    },
                );
            }
        }
    }

    /// `Wake::EditAwaitWithdraw`: an `await edit` returned. A ticket it leaves
    /// with no waiter starts its orphan clock.
    pub(crate) fn withdraw_edit_waiter(&mut self, waiter: WaiterId) {
        self.withdraw_edit_waiter_at(waiter, Instant::now());
    }

    /// [`Self::withdraw_edit_waiter`] at `now`, the orphan clock's start.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AmacsEditTicket",
            action = "Withdraw",
            project = "aterm_gui::app_edit::edit_ticket_conformance::Rig::project"
        )
    )]
    pub(crate) fn withdraw_edit_waiter_at(&mut self, waiter: WaiterId, now: Instant) {
        if self
            .native_documents
            .edits
            .waiters
            .remove(&waiter)
            .is_some()
        {
            let tickets: Vec<Ticket> = self.native_documents.edits.open.keys().copied().collect();
            for ticket in tickets {
                self.start_edit_orphan_clock_if_unwaited(ticket, now);
            }
        }
        self.sweep_orphaned_edits(now);
    }

    /// Start `ticket`'s orphan clock at `now` when its view is open, nobody
    /// waits on it and the clock is not already running.
    fn start_edit_orphan_clock_if_unwaited(&mut self, ticket: Ticket, now: Instant) {
        if self.native_documents.edits.waited(ticket) {
            return;
        }
        let Some(entry) = self.native_documents.edits.open.get_mut(&ticket) else {
            return;
        };
        if entry.view().is_none() || entry.unwaited_since.is_some() {
            return;
        }
        entry.unwaited_since = Some(now);
        hold::set_expiry(entry.session, ticket, Some(now + ORPHAN_AFTER));
    }

    /// Orphan every open ticket whose clock ran out by `now`: its hold is
    /// cleared and its view stays. Lazy: run on every edit wake, so a ticket
    /// nobody asks about is orphaned the next time anyone does (its hold has
    /// lapsed on its own by then, `control_edit::hold`).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AmacsEditTicket",
            action = "Orphan",
            project = "aterm_gui::app_edit::edit_ticket_conformance::Rig::project"
        )
    )]
    pub(crate) fn sweep_orphaned_edits(&mut self, now: Instant) {
        for (ticket, entry) in &mut self.native_documents.edits.open {
            if !entry.orphaned
                && entry
                    .unwaited_since
                    .is_some_and(|since| since + ORPHAN_AFTER <= now)
            {
                entry.orphaned = true;
                hold::clear(entry.session, *ticket);
            }
        }
    }

    /// The one view-retirement seam's hook (`App::remove_view_link`): `view`
    /// left its window. When it was an `edit` ticket's view the ticket resolves
    /// `closed`; a dirty one reached here only after its save was durable.
    /// `uri=` waiters with no ticket are re-asked whether any Editor view of
    /// their file remains.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AmacsEditTicket",
            action = "Retire",
            project = "aterm_gui::app_edit::edit_ticket_conformance::Rig::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AmacsEditTicket",
            action = "PersonCloses",
            project = "aterm_gui::app_edit::edit_ticket_conformance::Rig::project"
        )
    )]
    pub(crate) fn edit_view_retired(&mut self, view: ViewId) {
        if let Some(ticket) = self.native_documents.edits.ticket_of_view(view) {
            self.resolve_edit(ticket, EditResult::Closed);
        }
        self.answer_view_waiters();
    }

    /// Record `ticket`'s one outcome, answer its waiters and clear its hold;
    /// when its whole tab is gone, the caller's tab is selected again.
    fn resolve_edit(&mut self, ticket: Ticket, result: EditResult) {
        let Some(entry) = self.native_documents.edits.open.remove(&ticket) else {
            return;
        };
        let Phase::Open {
            tab,
            document,
            canonical_uri,
            start,
            ..
        } = entry.phase
        else {
            return;
        };
        let durable = self
            .document_store
            .checkpoint_seq(document)
            .unwrap_or(start);
        let head = self
            .document_store
            .snapshot(document)
            .map_or(durable, |snapshot| snapshot.seq);
        // `saved=1` only for a save that landed during the edit and covers the
        // text the view last showed (`SavedOnlyWhenAcked`).
        let outcome = EditOutcome {
            result,
            saved: durable > start && durable >= head,
            seq: durable.0,
        };
        hold::clear(entry.session, ticket);
        self.native_documents.edits.remember(Resolved {
            ticket,
            uri: entry.uri.clone(),
            canonical_uri,
            outcome,
        });
        // Its waiters read the outcome just recorded: a `ticket=` one by its
        // ticket, a `uri=` one once no other open ticket names the file.
        self.settle_uncovered_edit_waiters();
        // A whole-tab close closes the tab in front (`close_active_native_tab`;
        // a dirty one is refused until its save is durable, then closed again),
        // so the edit tab was the one in front: hand the window back to the
        // program that asked. The caller of this seam resyncs the window.
        if let Some(ws) = self.windows.get_mut(&entry.window)
            && ws.tab_set.get(tab).is_none()
            && ws.tab_set.switch_to(entry.caller_tab)
        {
            crate::app_tabs::align_terminal_projection_to_active(ws, &self.view_store);
            ws.last_present = None;
        }
    }

    /// What `waiter`, which no open ticket covers, is told: a `ticket=` one
    /// its ticket's outcome from the ring, else `ERR unknown ticket`; a `uri=`
    /// one the file's latest outcome from the ring, else (no ticket for this
    /// file in this process: a successor after a seamless update) it waits for
    /// the file's Editor views, or is done at once when there are none.
    fn uncovered_edit_waiter(&self, waiter: &Waiter) -> Uncovered {
        let edits = &self.native_documents.edits;
        match &waiter.key {
            AwaitKey::Ticket(ticket) => Uncovered::Answer(
                edits
                    .outcome_of(*ticket)
                    .map_or_else(|| "ERR unknown ticket\n".to_string(), outcome_line),
            ),
            AwaitKey::Uri(_) => {
                if let Some(outcome) = edits.latest_outcome_for(&waiter.spellings) {
                    Uncovered::Answer(outcome_line(outcome))
                } else if self.editor_views_of(&waiter.spellings) == 0 {
                    Uncovered::Answer(outcome_line(self.view_wait_outcome(&waiter.spellings)))
                } else {
                    Uncovered::ByViews
                }
            }
        }
    }

    /// Settle every parked waiter no open ticket covers any more (its ticket
    /// resolved, was refused or was cancelled) as [`Self::await_edit`] would
    /// arriving now. Ticketless `uri=` waiters already waiting on views are
    /// left to [`Self::answer_view_waiters`].
    pub(crate) fn settle_uncovered_edit_waiters(&mut self) {
        let edits = &self.native_documents.edits;
        let uncovered: Vec<WaiterId> = edits
            .waiters
            .iter()
            .filter(|(_, waiter)| !waiter.by_views && !waiter.covered_by_any(&edits.open))
            .map(|(id, _)| *id)
            .collect();
        for id in uncovered {
            let Some(waiter) = self.native_documents.edits.waiters.get(&id) else {
                continue;
            };
            match self.uncovered_edit_waiter(waiter) {
                Uncovered::Answer(line) => {
                    if let Some(waiter) = self.native_documents.edits.waiters.remove(&id) {
                        let _ = waiter.reply.send(line);
                    }
                }
                Uncovered::ByViews => {
                    if let Some(waiter) = self.native_documents.edits.waiters.get_mut(&id) {
                        waiter.by_views = true;
                    }
                }
            }
        }
    }

    /// Answer each ticketless `uri=` waiter whose file has no Editor view left.
    fn answer_view_waiters(&mut self) {
        let done: Vec<(WaiterId, EditOutcome)> = self
            .native_documents
            .edits
            .waiters
            .iter()
            .filter(|(_, waiter)| waiter.by_views && self.editor_views_of(&waiter.spellings) == 0)
            .map(|(id, waiter)| (*id, self.view_wait_outcome(&waiter.spellings)))
            .collect();
        for (id, outcome) in done {
            if let Some(waiter) = self.native_documents.edits.waiters.remove(&id) {
                let _ = waiter.reply.send(outcome_line(outcome));
            }
        }
    }

    /// The documents some spelling in `spellings` names, each once.
    fn documents_named(&self, spellings: &[String]) -> Vec<DocumentId> {
        let mut documents: Vec<DocumentId> = Vec::new();
        for document in spellings
            .iter()
            .filter_map(|uri| self.document_store.id_for_uri(uri))
        {
            if !documents.contains(&document) {
                documents.push(document);
            }
        }
        documents
    }

    /// How many Editor views of the file `spellings` name are open.
    fn editor_views_of(&self, spellings: &[String]) -> usize {
        self.documents_named(spellings)
            .into_iter()
            .map(|document| {
                self.document_native_views(document)
                    .into_iter()
                    .filter(|(_, instance, _)| {
                        self.native_runtime
                            .app(*instance)
                            .is_some_and(|app| app.kind() == AppKind::Editor)
                    })
                    .count()
            })
            .sum()
    }

    /// A ticketless `uri=` wait's outcome: `closed`, the durable checkpoint
    /// when the document is known, and never `saved=1` (no save is known to
    /// have landed during an edit this process did not see begin).
    fn view_wait_outcome(&self, spellings: &[String]) -> EditOutcome {
        let seq = self
            .documents_named(spellings)
            .into_iter()
            .find_map(|document| self.document_store.checkpoint_seq(document))
            .map_or(0, |seq| seq.0);
        EditOutcome {
            result: EditResult::Closed,
            saved: false,
            seq,
        }
    }

    /// Whether `view` is an open ticket's view whose tab carries the attention
    /// mark: `live_view_presentation` folds it into the tab's presentation, so
    /// a refresh of that presentation (a reveal, a title disambiguation) keeps
    /// the mark.
    pub(crate) fn edit_view_attention(&self, view: ViewId) -> bool {
        self.native_documents
            .edits
            .open
            .values()
            .any(|entry| entry.attention && entry.view() == Some(view))
    }

    /// Whether the tab of window `wid` holding `view` carries an `edit`
    /// view's attention mark.
    pub(crate) fn edit_attention_in_tab_of(&self, wid: WindowId, view: ViewId) -> bool {
        let Some(tab) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.tab_set.tabs().iter().find(|tab| tab.root.contains(view)))
        else {
            return false;
        };
        tab.root
            .leaves()
            .into_iter()
            .any(|leaf| self.edit_view_attention(leaf))
    }

    /// `sync_window`'s hook: every `edit` view in window `wid`'s active tab has
    /// been seen, so its mark is spent and does not come back when the person
    /// moves on.
    pub(crate) fn edit_tabs_seen(&mut self, wid: WindowId) {
        let Some(active) = self.windows.get(&wid).and_then(|ws| ws.tab_set.active()) else {
            return;
        };
        for entry in self.native_documents.edits.open.values_mut() {
            if entry.attention && entry.view().is_some_and(|view| active.root.contains(view)) {
                entry.attention = false;
            }
        }
    }

    /// Test seam: serve `edit` on this headless App.
    #[cfg(test)]
    pub(crate) fn serve_edits_headless_for_test(&mut self) {
        self.native_documents.edits.serve_headless = true;
    }
}

/// The model's two actions only a mutant enables have no seam to bind. A
/// module-level carrier under the anchors' own cfg, not one in the cfg(test)
/// conformance file, so aterm-models' closure links these waivers too.
#[cfg(any(test, debug_assertions))]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "AmacsEditTicket",
    action = "AnswerAtRequest",
    reason = "Buggy = 1 only: an answer before checkpoint_ack, which no shipping seam performs; \
              answer_before_ack_is_caught drives the rigged run and requires it be rejected."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "AmacsEditTicket",
    action = "ResolveAgain",
    reason = "Buggy = 2 only: a second outcome for a resolved ticket; resolve_edit removes the \
              ticket from the open map before it records the outcome, so none exists to bind."
)]
#[allow(
    dead_code,
    reason = "carrier for the `spec_unmodeled` waivers above; nothing calls it. An allow, \
              not an expect: rustc does not lint a fn the declarative anchors rebuild"
)]
fn mutant_only_actions() {}

#[cfg(test)]
#[path = "edit_ticket_conformance.rs"]
mod edit_ticket_conformance;

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc::{Receiver, TryRecvError};

    use amacs::protocol::edit::EditRequest;

    use super::*;
    use crate::app_documents::{DocumentAdmissionOutcome, DocumentAdmissionRejected};
    use crate::native_app::{AppEvent, AppViewState, TextInputEvent};

    /// One scratch file to edit, removed with the fixture.
    pub(super) struct Fixture {
        _dir: aterm_tempfile::TempDir,
        pub(super) path: PathBuf,
        pub(super) uri: String,
    }

    pub(super) fn fixture(name: &str, bytes: &[u8]) -> Fixture {
        let dir = aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-edit-{name}-"))
            .tempdir()
            .unwrap();
        let path = dir.path().join("COMMIT_EDITMSG");
        std::fs::write(&path, bytes).unwrap();
        let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
        Fixture {
            _dir: dir,
            path,
            uri,
        }
    }

    /// A headless App that serves `edit` (production refuses it headless).
    pub(super) fn app() -> App {
        let mut app = App::headless_for_test();
        app.serve_edits_headless_for_test();
        app
    }

    /// A terminal tab of window `wid`, in front, whose session id no other
    /// test's App mints: the process-wide hold map is keyed by session, and
    /// tests run in parallel.
    pub(super) fn caller(app: &mut App, wid: WindowId) -> u64 {
        let sid = fresh_session(app);
        app.push_stub_tab(wid, crate::stub_session(sid));
        app.next_session_id = sid + 1;
        sid
    }

    /// A session id no other test's App mints, made `app`'s next: each call
    /// reserves a block, so the sessions its App mints after it (a split's
    /// terminal) meet no other test's either. An edit on a session another
    /// test feeds (`@1 feed-bin`) would refuse that test's typing.
    pub(super) fn fresh_session(app: &mut App) -> u64 {
        static NEXT: AtomicU64 = AtomicU64::new(1 << 40);
        let sid = NEXT.fetch_add(1 << 12, Ordering::Relaxed);
        app.next_session_id = sid;
        sid
    }

    pub(super) fn request(uri: &str, line: Option<(u32, Option<u32>)>) -> EditRequest {
        EditRequest {
            uri: uri.to_string(),
            line,
            create: false,
            for_word: None,
            wait: true,
        }
    }

    /// `@<session> edit …`, answered inline (no event loop).
    pub(super) fn edit(app: &mut App, session: u64, request: EditRequest) -> (Ticket, String) {
        let ticket = crate::control_edit::mint_ticket();
        let (reply, answer) = std::sync::mpsc::channel();
        app.begin_edit(call(ticket, session, request, reply));
        (
            ticket,
            answer.try_recv().expect("an inline edit answers at once"),
        )
    }

    /// The `edit` call its lane posts, the realpath spelling resolved as the
    /// lane resolves it.
    pub(super) fn call(
        ticket: Ticket,
        session: u64,
        request: EditRequest,
        reply: std::sync::mpsc::Sender<String>,
    ) -> EditCall {
        EditCall {
            ticket,
            session,
            real_uri: crate::control_edit::real_file_uri(&request.uri),
            request,
            reply,
        }
    }

    /// `await edit …`, parked.
    pub(super) fn wait(app: &mut App, key: AwaitKey) -> (WaiterId, Receiver<String>) {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let waiter = WaiterId(NEXT.fetch_add(1, Ordering::Relaxed));
        let (reply, answer) = std::sync::mpsc::channel();
        let real_uri = match &key {
            AwaitKey::Uri(uri) => crate::control_edit::real_file_uri(uri),
            AwaitKey::Ticket(_) => None,
        };
        app.await_edit(AwaitCall {
            waiter,
            key,
            real_uri,
            reply,
        });
        (waiter, answer)
    }

    pub(super) fn tab_ids(app: &App, wid: WindowId) -> Vec<TabId> {
        app.windows[&wid]
            .tab_set
            .tabs()
            .iter()
            .map(|tab| tab.id)
            .collect()
    }

    pub(super) fn active_tab(app: &App, wid: WindowId) -> TabId {
        app.windows[&wid]
            .tab_set
            .active_id()
            .expect("an active tab")
    }

    /// The tab of window `wid` holding `view`.
    pub(super) fn tab_of(app: &App, wid: WindowId, view: ViewId) -> TabId {
        app.windows[&wid]
            .tab_set
            .tabs()
            .iter()
            .find(|tab| tab.root.contains(view))
            .map(|tab| tab.id)
            .expect("the view's tab")
    }

    pub(super) fn select(app: &mut App, wid: WindowId, tab: TabId) {
        assert!(app.windows.get_mut(&wid).unwrap().tab_set.switch_to(tab));
        app.sync_window(wid);
    }

    /// Focus leaf `view` of the active tab.
    pub(super) fn focus_leaf(app: &mut App, wid: WindowId, view: ViewId) {
        app.windows
            .get_mut(&wid)
            .unwrap()
            .tab_set
            .active_mut()
            .unwrap()
            .set_focus(view);
        app.sync_window(wid);
    }

    fn attention(app: &App, wid: WindowId, tab: TabId) -> bool {
        app.windows[&wid]
            .tab_set
            .get(tab)
            .is_some_and(|tab| tab.presentation.indicators.attention)
    }

    pub(super) fn type_text(app: &mut App, wid: WindowId, text: &str) {
        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit(text.to_string())),
        )
        .unwrap();
    }

    fn ok(line: &str) -> EditOk {
        EditOk::parse(line).unwrap_or_else(|error| panic!("{line:?}: {error}"))
    }

    fn view(ok: &EditOk) -> ViewId {
        ViewId::from_stored(ok.view)
    }

    fn document_of(app: &App, view: ViewId) -> DocumentId {
        let Some(crate::tab_model::View::Native(native)) = app.view_store.get(view).copied() else {
            panic!("view {view:?} is not a native view");
        };
        app.native_runtime
            .document_id(native.instance)
            .expect("a document view")
    }

    fn orphaned(app: &App, ticket: Ticket) -> bool {
        app.native_documents.edits.open[&ticket].orphaned
    }

    #[test]
    fn edit_opens_fresh_view_beside_caller_even_when_document_already_open() {
        let text = "one\ntwo\nthree\nfour\n";
        let f = fixture("fresh", text.as_bytes());
        let mut app = app();
        let wid = WindowId(0);
        let sid = caller(&mut app, wid);
        let caller_tab = active_tab(&app, wid);
        // A tab right of the caller's, so "beside" is not "at the end".
        let neighbour = caller(&mut app, wid);
        let _ = neighbour;
        // The person's own view of the file, opened earlier.
        app.open_document_tab(AppKind::Editor, &f.uri).unwrap();
        let (_, person_view) = app.active_native_view(wid).unwrap();
        let person_tab = active_tab(&app, wid);
        select(&mut app, wid, caller_tab);

        // Read the hold as of before the edit: its orphan clock (no waiter yet)
        // starts at admission, so no wall time between the two can lapse it.
        let before = Instant::now();
        let (ticket, line) = edit(&mut app, sid, request(&f.uri, Some((3, None))));
        let ok = ok(&line);
        assert_eq!(ok.ticket, ticket);
        let canonical =
            crate::native_document_host::path_to_file_uri(&std::fs::canonicalize(&f.path).unwrap())
                .unwrap();
        assert_eq!(ok.uri, canonical);
        let tabs = tab_ids(&app, wid);
        let at = tabs.iter().position(|tab| *tab == caller_tab).unwrap();
        let edit_tab = tabs[at + 1];
        assert_ne!(edit_tab, person_tab, "a new tab, right of the caller's");
        assert_eq!(active_tab(&app, wid), edit_tab, "it takes focus");
        let (_, front_view) = app.active_native_view(wid).unwrap();
        assert_eq!(front_view, view(&ok));
        assert_ne!(view(&ok), person_view, "the person's view is never reused");
        assert_eq!(document_of(&app, view(&ok)), document_of(&app, person_view));
        assert_eq!(ok.doc, document_of(&app, person_view).get());
        assert!(!attention(&app, wid, person_tab), "nor marked");
        // The caret is on line 3.
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view(&ok)) else {
            panic!("an Editor view");
        };
        let caret = crate::path_link::line_col_offset(text, 3, None);
        assert_eq!(
            state.buffer.as_ref().unwrap().selections,
            vec![crate::native_editor::Selection::caret(caret)]
        );
        assert_eq!(
            hold::get_at(sid, before),
            Some(hold::Hold {
                ticket,
                view: ok.view
            })
        );
    }

    #[test]
    fn edit_admission_never_coalesces_with_a_gesture_open() {
        let f = fixture("coalesce", b"draft\n");
        let mut app = app();
        let wid = WindowId(0);
        let sid = caller(&mut app, wid);
        // A gesture's read of the same file is in flight.
        let gesture = DocumentAdmissionKey {
            uri: f.uri.clone(),
            kind: AppKind::Editor,
            edit: None,
        };
        let pending = app.mint_document_admission(wid, gesture.clone());

        let (first, line) = edit(&mut app, sid, request(&f.uri, None));
        let first_ok = ok(&line);
        assert_eq!(
            app.document_admission_pending(wid, &gesture),
            Some(pending),
            "the edit read on its own; the gesture's read is untouched"
        );
        let (second, line) = edit(&mut app, sid, request(&f.uri, None));
        let second_ok = ok(&line);
        assert_ne!(first, second);
        assert_ne!(
            first_ok.view, second_ok.view,
            "each ticket owns its own view"
        );
        assert_eq!(first_ok.doc, second_ok.doc);

        // Two edits in flight for one file are two admissions, neither the
        // gesture's.
        let posted = |request| {
            let (reply, _answer) = std::sync::mpsc::channel();
            call(crate::control_edit::mint_ticket(), sid, request, reply)
        };
        let (_, a, a_ticket) = app
            .begin_edit_in_flight_for_test(posted(request(&f.uri, None)))
            .unwrap();
        let (_, b, b_ticket) = app
            .begin_edit_in_flight_for_test(posted(request(&f.uri, None)))
            .unwrap();
        assert_ne!(a, b);
        assert_ne!(a, gesture);
        assert_ne!(a_ticket, b_ticket);
        assert_eq!(app.document_admission_pending(wid, &a), Some(a_ticket));
        assert_eq!(app.document_admission_pending(wid, &b), Some(b_ticket));
    }

    #[test]
    fn edit_uses_the_callers_window_not_the_frontmost() {
        let f = fixture("window", b"x\n");
        let mut app = app();
        let front = WindowId(0);
        let sid = fresh_session(&mut app);
        let other = app.insert_logical_window(crate::stub_session(sid), 24, 80);
        app.frontmost_window = Some(front);
        let front_tabs = tab_ids(&app, front);
        let caller_tab = active_tab(&app, other);

        let (_, line) = edit(&mut app, sid, request(&f.uri, None));
        let ok = ok(&line);
        assert_eq!(
            tab_ids(&app, front),
            front_tabs,
            "nothing lands in the front window"
        );
        let tabs = tab_ids(&app, other);
        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs[0], caller_tab);
        assert!(
            app.windows[&other]
                .tab_set
                .get(tabs[1])
                .unwrap()
                .root
                .contains(view(&ok)),
            "the view is in the caller's window"
        );
        // That window is not the front one: the view opens behind, marked.
        assert_eq!(active_tab(&app, other), caller_tab);
        assert!(attention(&app, other, tabs[1]));
    }

    #[test]
    fn focus_follows_the_askpass_rule() {
        let f = fixture("focus", b"x\n");
        let mut app = app();
        let wid = WindowId(0);
        let sid = caller(&mut app, wid);
        let caller_tab = active_tab(&app, wid);

        // The caller's tab is the front window's active tab: the view takes focus.
        let (_, line) = edit(&mut app, sid, request(&f.uri, None));
        let first = ok(&line);
        let tabs = tab_ids(&app, wid);
        let at = tabs.iter().position(|tab| *tab == caller_tab).unwrap();
        assert_eq!(active_tab(&app, wid), tabs[at + 1]);
        assert!(!attention(&app, wid, tabs[at + 1]));
        assert!(
            app.windows[&wid]
                .tab_set
                .get(tabs[at + 1])
                .unwrap()
                .root
                .contains(view(&first))
        );

        // Another tab is in front: the view opens behind it with the mark, and
        // the tab in front stays in front.
        let _other = caller(&mut app, wid);
        let in_front = active_tab(&app, wid);
        let (_, line) = edit(&mut app, sid, request(&f.uri, None));
        let second = ok(&line);
        assert_eq!(active_tab(&app, wid), in_front);
        let tabs = tab_ids(&app, wid);
        let at = tabs.iter().position(|tab| *tab == caller_tab).unwrap();
        let placed = tabs[at + 1];
        assert!(
            app.windows[&wid]
                .tab_set
                .get(placed)
                .unwrap()
                .root
                .contains(view(&second))
        );
        assert!(attention(&app, wid, placed));
    }

    #[test]
    fn dirty_close_answers_closed_only_after_checkpoint_ack() {
        let wid = WindowId(0);
        let mut app = app();
        let sid = caller(&mut app, wid);
        let caller_tab = active_tab(&app, wid);

        // A split leaf: the close waits in the close protocol for the save in
        // flight and completes at its `checkpoint_ack`.
        let f = fixture("dirty-leaf", b"base\n");
        let (ticket, line) = edit(&mut app, sid, request(&f.uri, None));
        let ok = ok(&line);
        let edit_tab = active_tab(&app, wid);
        let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
        let (_, terminal) =
            app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
        focus_leaf(&mut app, wid, view(&ok));
        type_text(&mut app, wid, "edited ");
        let document = document_of(&app, view(&ok));
        let held = app.hold_document_save_for_test(document);
        assert!(
            app.close_focused_mixed_leaf(wid).is_err(),
            "the close waits"
        );
        assert!(app.document_close_pending_for_test(document));
        assert_eq!(
            answer.try_recv(),
            Err(TryRecvError::Empty),
            "no answer before the ack"
        );
        assert!(app.view_store.get(view(&ok)).is_some());

        app.land_held_document_save_for_test(document, view(&ok), held)
            .unwrap();
        let line = answer
            .try_recv()
            .expect("answered once the save is durable");
        let outcome = EditOutcome::parse(&line).unwrap();
        let durable = app.document_store.checkpoint_seq(document).unwrap();
        assert_eq!(
            outcome,
            EditOutcome {
                result: EditResult::Closed,
                saved: true,
                seq: durable.0
            }
        );
        assert_eq!(std::fs::read_to_string(&f.path).unwrap(), "edited base\n");
        assert!(app.view_store.get(view(&ok)).is_none());
        assert!(
            app.view_store.get(terminal).is_some(),
            "the rest of the tab stays"
        );
        assert_eq!(active_tab(&app, wid), edit_tab, "its tab is still there");
        assert_ne!(hold::get(sid).map(|hold| hold.ticket), Some(ticket));
        // A waiter that comes after the outcome reads it from the ring.
        let (_, late) = wait(&mut app, AwaitKey::Ticket(ticket));
        assert_eq!(late.try_recv().unwrap(), line);

        // A whole tab: its close is refused while the save is in flight and the
        // view stays (no answer, even once the save lands); closed again, it
        // answers, and the caller's tab comes back to the front.
        select(&mut app, wid, caller_tab);
        let g = fixture("dirty-tab", b"base\n");
        let (ticket, line) = edit(&mut app, sid, request(&g.uri, None));
        let tab_view = view(&self::ok(&line));
        let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
        type_text(&mut app, wid, "again ");
        let document = document_of(&app, tab_view);
        let held = app.hold_document_save_for_test(document);
        assert!(
            app.close_active_native_tab(wid).is_err(),
            "refused while saving"
        );
        assert_eq!(answer.try_recv(), Err(TryRecvError::Empty));
        app.land_held_document_save_for_test(document, tab_view, held)
            .unwrap();
        assert_eq!(
            answer.try_recv(),
            Err(TryRecvError::Empty),
            "the view is still open"
        );
        assert!(app.view_store.get(tab_view).is_some());
        app.close_active_native_tab(wid).unwrap();
        let outcome = EditOutcome::parse(&answer.try_recv().unwrap()).unwrap();
        assert_eq!((outcome.result, outcome.saved), (EditResult::Closed, true));
        assert_eq!(std::fs::read_to_string(&g.path).unwrap(), "again base\n");
        assert_eq!(
            active_tab(&app, wid),
            caller_tab,
            "handed back to the caller"
        );
    }

    #[test]
    fn hangup_withdraws_and_orphans_after_5s_and_keeps_the_view() {
        let f = fixture("orphan", b"keep\n");
        let mut app = app();
        let wid = WindowId(0);
        let sid = caller(&mut app, wid);
        let (ticket, line) = edit(&mut app, sid, request(&f.uri, None));
        let ok = ok(&line);
        let (waiter, _answer) = wait(&mut app, AwaitKey::Ticket(ticket));
        let held = Some(hold::Hold {
            ticket,
            view: ok.view,
        });
        assert_eq!(hold::get(sid), held);

        // The waiter hangs up at `left`; within 5 s the ticket keeps its place.
        let left = Instant::now();
        app.withdraw_edit_waiter_at(waiter, left);
        app.sweep_orphaned_edits(left);
        assert!(!orphaned(&app, ticket));
        assert_eq!(hold::get_at(sid, left), held);
        // Past 5 s it is orphaned: the hold is gone and the view stays.
        let later = left + ORPHAN_AFTER;
        app.sweep_orphaned_edits(later);
        assert!(orphaned(&app, ticket));
        assert_eq!(hold::get(sid), None);
        assert!(
            app.view_store.get(view(&ok)).is_some(),
            "an orphan keeps its view"
        );
        assert_eq!(app.active_native_view(wid).map(|(_, v)| v), Some(view(&ok)));

        // A waiter that comes back adopts it, and the close still answers.
        let (_, answer) = wait(&mut app, AwaitKey::Ticket(ticket));
        assert!(!orphaned(&app, ticket));
        assert_eq!(hold::get(sid), held);
        app.close_active_native_tab(wid).unwrap();
        let outcome = EditOutcome::parse(&answer.try_recv().unwrap()).unwrap();
        assert_eq!(outcome.result, EditResult::Closed);
        assert!(!outcome.saved, "nothing was saved");
        assert_eq!(std::fs::read_to_string(&f.path).unwrap(), "keep\n");
    }

    #[test]
    fn await_by_uri_ignores_views_the_person_opened() {
        let f = fixture("uri", b"shared\n");
        let mut app = app();
        let wid = WindowId(0);
        let sid = caller(&mut app, wid);
        let caller_tab = active_tab(&app, wid);
        app.open_document_tab(AppKind::Editor, &f.uri).unwrap();
        let person_tab = active_tab(&app, wid);
        select(&mut app, wid, caller_tab);
        let (_, line) = edit(&mut app, sid, request(&f.uri, None));
        let edit_tab = active_tab(&app, wid);
        let ok = ok(&line);
        // By the requested spelling, and by the canonical one.
        let (_, by_request) = wait(&mut app, AwaitKey::Uri(f.uri.clone()));
        let (_, by_canonical) = wait(&mut app, AwaitKey::Uri(ok.uri.clone()));

        // The person's own view closes: the edit is not done.
        select(&mut app, wid, person_tab);
        app.close_active_native_tab(wid).unwrap();
        assert_eq!(by_request.try_recv(), Err(TryRecvError::Empty));
        assert_eq!(by_canonical.try_recv(), Err(TryRecvError::Empty));

        // The edit's view closes: it is.
        select(&mut app, wid, edit_tab);
        app.close_active_native_tab(wid).unwrap();
        for answer in [by_request, by_canonical] {
            let outcome = EditOutcome::parse(&answer.try_recv().unwrap()).unwrap();
            assert_eq!(outcome.result, EditResult::Closed);
        }

        // With no ticket for the file (a successor after a seamless update), a
        // `uri=` wait is done when no Editor view of it remains.
        let g = fixture("uri-successor", b"theirs\n");
        app.open_document_tab(AppKind::Editor, &g.uri).unwrap();
        let canonical =
            crate::native_document_host::path_to_file_uri(&std::fs::canonicalize(&g.path).unwrap())
                .unwrap();
        let (_, successor) = wait(&mut app, AwaitKey::Uri(canonical));
        assert_eq!(successor.try_recv(), Err(TryRecvError::Empty));
        app.close_active_native_tab(wid).unwrap();
        let outcome = EditOutcome::parse(&successor.try_recv().unwrap()).unwrap();
        assert_eq!((outcome.result, outcome.saved), (EditResult::Closed, false));
    }

    #[test]
    fn edit_on_headless_answers_unsupported() {
        let f = fixture("headless", b"x\n");
        let mut app = App::headless_for_test();
        let (_, line) = edit(&mut app, 0, request(&f.uri, None));
        assert_eq!(line, "ERR unsupported headless\n");
        assert!(app.native_documents.edits.open.is_empty());

        let mut app = self::app();
        let sid = caller(&mut app, WindowId(0));
        let mut create = request(&f.uri, None);
        create.create = true;
        assert_eq!(edit(&mut app, sid, create).1, "ERR unsupported create\n");
        assert_eq!(
            edit(&mut app, sid + 1_000, request(&f.uri, None)).1,
            "ERR no-window\n"
        );
        assert!(app.native_documents.edits.open.is_empty());
    }

    #[test]
    fn edit_refusals_name_the_reason() {
        let f = fixture("refusals", b"\xff\xfe not utf-8\n");
        let mut app = app();
        let sid = caller(&mut app, WindowId(0));
        let tabs = tab_ids(&app, WindowId(0));
        let missing =
            crate::native_document_host::path_to_file_uri(&f.path.with_extension("gone")).unwrap();
        let directory =
            crate::native_document_host::path_to_file_uri(f.path.parent().unwrap()).unwrap();
        for (uri, refusal) in [
            (f.uri.as_str(), "ERR refused invalid-utf8\n"),
            (missing.as_str(), "ERR refused missing\n"),
            (directory.as_str(), "ERR refused not-regular\n"),
        ] {
            assert_eq!(edit(&mut app, sid, request(uri, None)).1, refusal, "{uri}");
        }
        assert_eq!(tab_ids(&app, WindowId(0)), tabs, "nothing opened");
        assert!(app.native_documents.edits.open.is_empty());
    }

    #[test]
    fn timeout_cancels_and_a_late_admission_is_dropped() {
        let f = fixture("late", b"late\n");
        let mut app = app();
        let wid = WindowId(0);
        let sid = caller(&mut app, wid);
        let tabs = tab_ids(&app, wid);
        let ticket = crate::control_edit::mint_ticket();
        let (reply, answer) = std::sync::mpsc::channel();
        let (window, key, admission) = app
            .begin_edit_in_flight_for_test(call(ticket, sid, request(&f.uri, None), reply))
            .unwrap();
        // `edit` gave up at its deadline (`control_edit::serve_edit`).
        app.cancel_edit(ticket);
        assert_eq!(answer.try_recv(), Err(TryRecvError::Disconnected));

        let result = crate::native_document_host::admit_local_file(
            &key.uri,
            crate::native_document_host::DEFAULT_DOCUMENT_LIMIT,
            false,
        )
        .map(Box::new)
        .map_err(DocumentOpenFailure);
        let rejected = app
            .complete_document_admission(DocumentAdmissionOutcome {
                ticket: admission,
                wid: window,
                key,
                result,
            })
            .unwrap_err();
        assert!(
            matches!(rejected, DocumentAdmissionRejected::Dropped(_)),
            "{rejected:?}"
        );
        assert_eq!(tab_ids(&app, wid), tabs, "a late admission opens nothing");
        let (_, unknown) = wait(&mut app, AwaitKey::Ticket(ticket));
        assert_eq!(unknown.try_recv().unwrap(), "ERR unknown ticket\n");
    }

    #[test]
    fn a_background_edit_keeps_its_mark_until_the_person_sees_it() {
        let text = "one\ntwo\nthree\n";
        let f = fixture("mark", text.as_bytes());
        let mut app = app();
        let wid = WindowId(0);
        let sid = caller(&mut app, wid);
        let caller_tab = active_tab(&app, wid);
        // The person's own view of the file is in front, not the caller's tab.
        app.open_document_tab(AppKind::Editor, &f.uri).unwrap();
        let (_, person_view) = app.active_native_view(wid).unwrap();
        let person_tab = active_tab(&app, wid);

        // `line=`: the reveal refreshes the presentation of every view of the
        // document, and the mark survives it.
        let (_, line) = edit(&mut app, sid, request(&f.uri, Some((2, None))));
        let ok = ok(&line);
        let placed = tab_of(&app, wid, view(&ok));
        let tabs = tab_ids(&app, wid);
        let at = tabs.iter().position(|tab| *tab == caller_tab).unwrap();
        assert_eq!(tabs[at + 1], placed, "right of the caller's tab");
        assert_eq!(active_tab(&app, wid), person_tab, "the person's tab stays");
        assert!(attention(&app, wid, placed), "opened behind, marked");
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view(&ok)) else {
            panic!("an Editor view");
        };
        assert_eq!(
            state.buffer.as_ref().unwrap().selections,
            vec![crate::native_editor::Selection::caret(
                crate::path_link::line_col_offset(text, 2, None)
            )],
            "the caret is on line 2 behind the person's tab"
        );

        // No later refresh of that tab's presentation drops it.
        app.refresh_disambiguated_document_titles();
        app.refresh_aggregate_tab_presentation(wid, placed);
        assert!(attention(&app, wid, placed), "a refresh keeps the mark");
        // A save through the person's own view lifts a failure's mark, not
        // this one: the program that asked still waits.
        type_text(&mut app, wid, "saved ");
        let document = document_of(&app, person_view);
        let held = app.hold_document_save_for_test(document);
        app.land_held_document_save_for_test(document, person_view, held)
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&f.path).unwrap(),
            "saved one\ntwo\nthree\n"
        );
        assert!(attention(&app, wid, placed), "a save elsewhere keeps it");

        // The person brings it to the front: the mark is spent, and it does
        // not come back once they move on.
        select(&mut app, wid, placed);
        assert!(!attention(&app, wid, placed));
        select(&mut app, wid, person_tab);
        app.refresh_aggregate_tab_presentation(wid, placed);
        assert!(!attention(&app, wid, placed), "seen once, spent");
    }

    #[test]
    fn a_cancel_after_the_view_opened_orphans_it_unless_a_waiter_holds_it() {
        let f = fixture("cancel-open", b"x\n");
        let g = fixture("cancel-held", b"y\n");
        let mut app = app();
        let wid = WindowId(0);
        let sid = caller(&mut app, wid);
        // The view opened just as `edit` gave up: nobody learned the ticket,
        // so it is orphaned at once and its view stays.
        let (ticket, line) = edit(&mut app, sid, request(&f.uri, None));
        let first = view(&ok(&line));
        app.cancel_edit(ticket);
        assert!(orphaned(&app, ticket));
        assert_ne!(hold::get(sid).map(|hold| hold.ticket), Some(ticket));
        assert!(
            app.view_store.get(first).is_some(),
            "an orphan keeps its view"
        );

        // A waiter that found the ticket first (by `uri=`) holds it: the
        // cancel orphans nothing, and the close answers that waiter.
        let (ticket, line) = edit(&mut app, sid, request(&g.uri, None));
        let second = view(&ok(&line));
        let (_, answer) = wait(&mut app, AwaitKey::Uri(g.uri.clone()));
        app.cancel_edit(ticket);
        assert!(!orphaned(&app, ticket), "the waiter holds it");
        assert_eq!(hold::get(sid).map(|hold| hold.ticket), Some(ticket));
        assert_eq!(answer.try_recv(), Err(TryRecvError::Empty));
        let tab = tab_of(&app, wid, second);
        select(&mut app, wid, tab);
        app.close_active_native_tab(wid).unwrap();
        let outcome = EditOutcome::parse(&answer.try_recv().unwrap()).unwrap();
        assert_eq!(outcome.result, EditResult::Closed);
    }

    #[test]
    fn a_refused_or_cancelled_admission_settles_the_waiters_it_covered() {
        let f = fixture("settle", b"x\n");
        let mut app = app();
        let wid = WindowId(0);
        let sid = caller(&mut app, wid);

        // Waiters park on an admission in flight, which is then cancelled: no
        // ticket names the file any more and no view of it is open.
        let ticket = crate::control_edit::mint_ticket();
        let (reply, _cancelled) = std::sync::mpsc::channel();
        app.begin_edit_in_flight_for_test(call(ticket, sid, request(&f.uri, None), reply))
            .unwrap();
        let (_, by_uri) = wait(&mut app, AwaitKey::Uri(f.uri.clone()));
        let (_, by_ticket) = wait(&mut app, AwaitKey::Ticket(ticket));
        assert_eq!(by_uri.try_recv(), Err(TryRecvError::Empty));
        assert_eq!(by_ticket.try_recv(), Err(TryRecvError::Empty));
        app.cancel_edit(ticket);
        let outcome =
            EditOutcome::parse(&by_uri.try_recv().expect("settled at the cancel")).unwrap();
        assert_eq!((outcome.result, outcome.saved), (EditResult::Closed, false));
        assert_eq!(by_ticket.try_recv().unwrap(), "ERR unknown ticket\n");

        // A refused admission (a missing file) settles its waiter the same way.
        let missing =
            crate::native_document_host::path_to_file_uri(&f.path.with_extension("gone")).unwrap();
        let ticket = crate::control_edit::mint_ticket();
        let (reply, refused) = std::sync::mpsc::channel();
        let (window, key, admission) = app
            .begin_edit_in_flight_for_test(call(ticket, sid, request(&missing, None), reply))
            .unwrap();
        let (_, by_uri) = wait(&mut app, AwaitKey::Uri(missing));
        let result = crate::native_document_host::admit_local_file(
            &key.uri,
            crate::native_document_host::DEFAULT_DOCUMENT_LIMIT,
            false,
        )
        .map(Box::new)
        .map_err(DocumentOpenFailure);
        let _ = app.complete_document_admission(DocumentAdmissionOutcome {
            ticket: admission,
            wid: window,
            key,
            result,
        });
        assert_eq!(refused.try_recv().unwrap(), "ERR refused missing\n");
        let outcome =
            EditOutcome::parse(&by_uri.try_recv().expect("settled at the refusal")).unwrap();
        assert_eq!(
            outcome,
            EditOutcome {
                result: EditResult::Closed,
                saved: false,
                seq: 0
            }
        );
        assert!(app.native_documents.edits.waiters.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_uri_wait_by_another_spelling_of_the_file_finds_it() {
        // A second spelling of each file, through a symlink to its directory.
        let alias = |fixture: &Fixture| {
            let dir = fixture.path.parent().unwrap();
            std::os::unix::fs::symlink(dir, dir.join("alias")).unwrap();
            crate::native_document_host::path_to_file_uri(&dir.join("alias/COMMIT_EDITMSG"))
                .unwrap()
        };
        let f = fixture("spelling-ticket", b"x\n");
        let g = fixture("spelling-views", b"y\n");
        let (f_alias, g_alias) = (alias(&f), alias(&g));
        let mut app = app();
        let wid = WindowId(0);
        let sid = caller(&mut app, wid);

        // A ticket asked for by one spelling is waited on by the other.
        let (_, line) = edit(&mut app, sid, request(&f.uri, None));
        let edit_view = view(&ok(&line));
        let (_, by_alias) = wait(&mut app, AwaitKey::Uri(f_alias));
        assert_eq!(by_alias.try_recv(), Err(TryRecvError::Empty));
        let tab = tab_of(&app, wid, edit_view);
        select(&mut app, wid, tab);
        app.close_active_native_tab(wid).unwrap();
        let outcome = EditOutcome::parse(&by_alias.try_recv().unwrap()).unwrap();
        assert_eq!(outcome.result, EditResult::Closed);

        // With no ticket for the file (a successor after an update), the
        // other spelling still finds the Editor view open on it.
        app.open_document_tab(AppKind::Editor, &g.uri).unwrap();
        let (_, successor) = wait(&mut app, AwaitKey::Uri(g_alias));
        assert_eq!(
            successor.try_recv(),
            Err(TryRecvError::Empty),
            "the view is still open"
        );
        app.close_active_native_tab(wid).unwrap();
        let outcome = EditOutcome::parse(&successor.try_recv().unwrap()).unwrap();
        assert_eq!((outcome.result, outcome.saved), (EditResult::Closed, false));
    }

    /// The slice's acceptance on the wire, headless (production refuses `edit`
    /// without a window): `@<sid> edit <file-uri> line=3` through the `edit`
    /// lane answers `OK edit ticket=… view=… doc=… uri=…` with a new tab right
    /// of the caller's and the caret on line 3; `await edit ticket=<t>` on its
    /// own lane answers `OK edit done result=closed saved=1 seq=<n>` once the
    /// person saves and closes that view.
    #[test]
    fn edit_and_await_edit_answer_on_the_wire() {
        use crate::control::control_media::{Hops, MainPost, take_hop};
        fn slice(left: Duration) -> Duration {
            left.min(Duration::from_millis(5))
        }
        // A hang detector, never a budget.
        const HANG: Duration = Duration::from_secs(60);
        let text = "one\ntwo\nthree\nfour\n";
        let f = fixture("wire", text.as_bytes());
        let mut app = app();
        let wid = WindowId(0);
        let sid = caller(&mut app, wid);
        let caller_tab = active_tab(&app, wid);

        // A count of hops of this test's own.
        let hops: &'static Hops = Box::leak(Box::new(Hops::new()));
        let line = crate::control_edit::serve_edit(
            crate::control::Scope::Owner,
            Some(sid),
            &format!("{} line=3", f.uri),
            &mut MainPost::injected(
                |wake| match take_hop(wake) {
                    crate::Wake::EditRequest(call) => {
                        app.begin_edit(*call);
                        true
                    }
                    other => panic!("unexpected {other:?}"),
                },
                hops,
            ),
            &mut || false,
            HANG,
            slice,
        );
        assert!(line.starts_with("OK edit ticket="), "{line}");
        let ok = ok(&line);
        let tabs = tab_ids(&app, wid);
        let at = tabs.iter().position(|tab| *tab == caller_tab).unwrap();
        assert_eq!(tab_of(&app, wid, view(&ok)), tabs[at + 1]);
        let Some(AppViewState::Editor(state)) = app.native_runtime.view_state(view(&ok)) else {
            panic!("an Editor view");
        };
        assert_eq!(
            state.buffer.as_ref().unwrap().selections,
            vec![crate::native_editor::Selection::caret(
                crate::path_link::line_col_offset(text, 3, None)
            )]
        );

        // `await edit` parks on its own lane; the App answers on this thread.
        let (wakes, posted) = std::sync::mpsc::channel::<crate::Wake>();
        let rest = format!("edit ticket={} timeout=60000", ok.ticket);
        let lane = std::thread::spawn(move || {
            crate::control_edit::serve_await_edit(
                crate::control::Scope::Owner,
                &rest,
                &mut MainPost::injected(|wake| wakes.send(wake).is_ok(), hops),
                &mut || false,
                slice,
            )
        });
        let deliver = |app: &mut App, wake| match take_hop(wake) {
            crate::Wake::EditAwait(call) => app.await_edit(*call),
            crate::Wake::EditAwaitWithdraw { waiter } => app.withdraw_edit_waiter(waiter),
            other => panic!("unexpected {other:?}"),
        };
        deliver(&mut app, posted.recv_timeout(HANG).expect("the wait"));
        // The person edits, saves and closes the view.
        type_text(&mut app, wid, "edited ");
        let document = document_of(&app, view(&ok));
        let held = app.hold_document_save_for_test(document);
        app.land_held_document_save_for_test(document, view(&ok), held)
            .unwrap();
        let durable = app.document_store.checkpoint_seq(document).unwrap();
        app.close_active_native_tab(wid).unwrap();
        deliver(&mut app, posted.recv_timeout(HANG).expect("the withdrawal"));
        assert_eq!(
            lane.join().unwrap(),
            format!("OK edit done result=closed saved=1 seq={}\n", durable.0)
        );
        assert_eq!(
            std::fs::read_to_string(&f.path).unwrap(),
            "one\ntwo\nedited three\nfour\n"
        );
        assert!(app.native_documents.edits.waiters.is_empty());
    }
}
