// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The System Resources page: a read-only tab application, one instance a process and at most
//! one view a window ([`crate::app_resources`] places it). Its head is the deck's Full face
//! ([`crate::stats_deck::deck_paint_kept`]) at the page's width, painted from the reading the host
//! hands it; the page probes nothing and does no I/O of its own.
//!
//! The page is one scrolling column whose documents and actions each keep a stable key, so a
//! later configuration section joins the same column and a reveal of its key scrolls to it
//! ([`ResourcesViewState::reveal_listed_control`]).
//!
//! A scope row heads the column: This Mac, aterm or one session ([`PageScope`]), which the head's
//! Tab lane and every document take, as a pinned lens takes its bar. After the documents a
//! Sessions table lists every session's cost in the host's order, then aterm's and this Mac's;
//! pressing a row chooses its scope. The row and the table are native text and buttons, so
//! `inspect <view> text` and `controls` read them and `act` chooses.
//!
//! Spotlight's document follows Processes where this Mac splits by coalition (SD-95): its figures
//! ride the reading, and what it says is indexed is the host's probe's
//! ([`crate::resources_spotlight`]), handed in with the reading.
//!
//! The Disk document's used-space chart draws a day or three weeks, one a view and never saved
//! (SD-82): a span button lies over the chart's `24 h \u{b7} 3 weeks` labels, where the document
//! reports it drew them ([`crate::stats_lens::PageLaid::span_chips`]), and its press or `act`
//! switches the span. Where the chart offers one span the labels are not drawn and no button lies;
//! on either span the page's labels, its one way to switch, give up the heading's growth words and
//! name before they go, so the button lies wherever the line holds them, whatever growth words a
//! reading brings.
//!
//! On macOS the CPU document carries Collect CPU Policy and Cancel (SD-83) beside its copies, and
//! a line under them that says whether a collection is pending, as the host last told the page
//! ([`ResourcesApp::policy`]). Their action ids share [`CPU_POLICY_PREFIX`], a person-only press
//! (`app_control`'s `PERSON_ONLY_PREFIXES`): a press asks the host
//! ([`crate::native_app::AppEffect::CpuPolicy`]) to bind one collection to the pressing view,
//! whose password sheet hangs on the view's window and goes when the view leaves the front. They
//! are a row of their own, not laid over the document's Hardware & limits section: that section
//! is laid only when the reading holds a CPU Stats sample or a policy report, and its 13 pt rows
//! have no room a button could take without hiding the words the collection reports.
//!
//! Spotlight's document carries its four buttons (SD-96) in a row just above it, keyed
//! [`SPOTLIGHT_CONTROL_KEY`]: Apply scope… (primary), Pause indexing or Resume indexing, Rebuild
//! index… and Keep a folder out…, each live only where the page's own model says its step would
//! do something now ([`crate::resources_spotlight::controls`]), with a line under them saying what
//! the last press did. Their action ids share [`SPOTLIGHT_CONTROL_PREFIX`], a person-only press;
//! each runs its root step under the password sheet bound to the pressing view
//! ([`crate::native_app::AppEffect::SpotlightControl`]). They are a row of their own rather than
//! laid over the document's head, whose one line carries the state and wanted words they would
//! hide.
//!
//! The column ends with the macOS configuration section (SD-97, [`crate::resources_macos`]):
//! each macOS setting that changes what this Mac spends, its Now against its Wanted in words, and
//! where the host's judgment offers one, an Apply… whose action id shares [`MACOS_APPLY_PREFIX`],
//! a person-only press like CPU policy's. `aterm ctl open app system-resources
//! section=macos-configuration` opens the page scrolled to it ([`ResourcesViewState::reveal_on_lay`]).

use std::cell::{Cell, RefCell};
use std::sync::Arc;

use aterm_vitals::Snapshot;

use crate::native_app::{
    AppDescriptor, AppEvent, AppIcon, AppIndicators, AppKind, AppPresentation, ClipboardOutcome,
    ClipboardRequest, CloseReadiness, CloseRequest, Command, CommonViewState, CpuPolicyPress,
    DamageRegion, EventResult, NativeAppModel, OperationId, UpdateCx, ViewCx,
};
use crate::native_ui::{
    ActionId, ButtonSpec, Control, ControlState, Flow, GroupSpec, Insets, Layout, Length,
    LogicalRect, SemanticRole, SemanticValue, StyleRef, UiContent, UiKey, UiNode, UiTree,
};
use crate::resources_spotlight::ScopeReading;
use crate::stats_lens::{DiskSpan, Place};
use crate::stats_paint::{Bar, Instrument};
use crate::widget::DrawPrim;
use aterm_types::macos_setup::root_only::Step;

/// The page's tab title and name.
pub(crate) const RESOURCES_TITLE: &str = "System Resources";
/// The machine's noun on this platform (`this Mac`, `this computer`), as the deck's legend says it.
const MACHINE: &str = aterm_vitals::words::Platform::host().machine();

/// The machine's noun as a title: `This Mac` on a Mac, `This computer` elsewhere.
fn machine_title() -> String {
    let mut words = MACHINE.chars();
    let first = words.next().map(|c| c.to_uppercase().to_string());
    format!("{}{}", first.unwrap_or_default(), words.as_str())
}

/// The tab's hover help: `This Mac's resources, live` on a Mac, in the deck's own noun elsewhere.
pub(crate) fn resources_tooltip() -> String {
    format!("{}'s resources, live", machine_title())
}

const TITLE_KEY: &str = "resources/title";
pub(crate) const HEAD_KEY: &str = "resources/head";
const READING_KEY: &str = "resources/reading";

/// The Spotlight document's rank among the page's documents ([`Instrument::page`] leaves it
/// free) and its key's slug.
const SPOTLIGHT_PAGE: (u8, &str) = (6, "spotlight");

/// What the page's head and documents are of: the scope row's choice, one a view.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum PageScope {
    /// The machine: the deck's Global bar.
    #[default]
    Mac,
    /// This aterm: the Aterm bar.
    Aterm,
    /// One session and the other panes of its tab ([`Snapshot::tab_of`] sums them), the session
    /// first, under its tab's label: the Tab bar.
    Session {
        key: u64,
        panes: Vec<u64>,
        label: String,
    },
}

impl PageScope {
    /// A page opened over a terminal tab whose sessions are `panes` (its focused pane first,
    /// `App::deck_tab_panes`) and whose name is `label` is scoped to that tab; over no terminal
    /// tab, to this Mac.
    pub(crate) fn opened(panes: Vec<u64>, label: String) -> Self {
        match panes.first() {
            Some(&key) => Self::Session { key, panes, label },
            None => Self::Mac,
        }
    }

    /// The session `key` of `snap` and its tab's other panes, in the host's order.
    fn session(snap: &Snapshot, key: u64) -> Option<Self> {
        let lead = snap.tab(key)?;
        let panes = std::iter::once(key)
            .chain(
                snap.tabs
                    .iter()
                    .filter(|view| view.tab == lead.tab && view.key != key)
                    .map(|view| view.key),
            )
            .collect();
        Some(Self::Session {
            key,
            panes,
            label: lead.label.clone(),
        })
    }

    /// The scope at the reading `snap`: a session the reading's sweep still holds takes its
    /// tab's panes as they now stand, and one it no longer holds has ended, which returns the
    /// scope to this Mac. A reading with no sweep of sessions (before two sweeps, or a
    /// machine-only one) says nothing of them and keeps the scope.
    fn at_reading(&self, snap: &Snapshot) -> Self {
        let Self::Session { key, .. } = self else {
            return self.clone();
        };
        if snap.machine_only || snap.aterm.is_none() {
            return self.clone();
        }
        Self::session(snap, *key).unwrap_or(Self::Mac)
    }

    /// The deck bar its documents take.
    pub(crate) const fn bar(&self) -> Bar {
        match self {
            Self::Mac => Bar::Global,
            Self::Aterm => Bar::Aterm,
            Self::Session { .. } => Bar::Tab,
        }
    }

    /// The sessions of its Tab lane: none outside a session's scope.
    pub(crate) fn panes(&self) -> &[u64] {
        match self {
            Self::Session { panes, .. } => panes,
            Self::Mac | Self::Aterm => &[],
        }
    }

    /// Its Tab lane's name: empty outside a session's scope.
    pub(crate) fn tab_label(&self) -> &str {
        match self {
            Self::Session { label, .. } => label,
            Self::Mac | Self::Aterm => "",
        }
    }

    fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        self.hash(&mut hash);
        hash.finish()
    }
}

const SCOPE_KEY: &str = "resources/scope";
const SCOPE_MAC: &str = "resources/scope/mac";
const SCOPE_ATERM: &str = "resources/scope/aterm";
/// A session's scope action: this prefix and the session's key.
const SCOPE_SESSION: &str = "resources/scope/session/";
const SESSIONS_KEY: &str = "resources/sessions";
/// The Disk document's span button, its key and its action.
const SPAN_KEY: &str = "resources/doc/disk/span";
/// The macOS configuration section's heading, the key its open and System State's links reveal.
pub(crate) const MACOS_KEY: &str =
    aterm_types::app_inspection::ResourcesSection::MacosConfiguration.key();
/// The section's Apply buttons' action ids: one a row, ending in its slug
/// ([`crate::resources_macos::SLUGS`]); a person-only press (`app_control`'s
/// `PERSON_ONLY_PREFIXES`).
pub(crate) const MACOS_APPLY_PREFIX: &str = "resources/macos-configuration/apply/";
/// The Spotlight document's buttons' group, keyed just above the document, and the line under
/// it.
pub(crate) const SPOTLIGHT_CONTROL_KEY: &str = "resources/doc/spotlight/control";
const SPOTLIGHT_SAID: &str = "resources/doc/spotlight/said";
/// The Spotlight document's buttons' action ids: one a step, ending in its word
/// ([`spotlight_control_word`]); a person-only press (`app_control`'s `PERSON_ONLY_PREFIXES`).
pub(crate) const SPOTLIGHT_CONTROL_PREFIX: &str = "resources/doc/spotlight/control/";

/// A Spotlight document button's word in its action id.
pub(crate) const fn spotlight_control_word(step: Step) -> &'static str {
    match step {
        Step::SpotlightScope => "apply-scope",
        Step::SpotlightPause => "pause",
        Step::SpotlightResume => "resume",
        Step::SpotlightRebuild => "rebuild",
        Step::SpotlightKeepOut => "keep-out",
        Step::Power => "power",
        Step::Updates => "updates",
    }
}

/// The Spotlight document's step whose button's action is `action`.
fn spotlight_control_step(action: &str) -> Option<Step> {
    let word = action.strip_prefix(SPOTLIGHT_CONTROL_PREFIX)?;
    Step::ALL
        .into_iter()
        .find(|step| step.spotlight() && spotlight_control_word(*step) == word)
}

/// A Spotlight document button's words, its name too (a visible label is part of its name; the
/// group, "Spotlight actions", says what they act on).
const fn spotlight_control_words(step: Step) -> &'static str {
    match step {
        Step::SpotlightScope => "Apply scope…",
        Step::SpotlightPause => "Pause indexing",
        Step::SpotlightResume => "Resume indexing",
        Step::SpotlightRebuild => "Rebuild index…",
        Step::SpotlightKeepOut => "Keep a folder out…",
        Step::Power | Step::Updates => "",
    }
}

/// The CPU document's policy buttons' prefix: every action under it is a person's own press.
pub(crate) const CPU_POLICY_PREFIX: &str = "resources/doc/cpu/policy/";
/// Collect CPU Policy, its key and its action.
pub(crate) const CPU_POLICY_COLLECT: &str = "resources/doc/cpu/policy/collect";
/// Cancel, its key and its action.
pub(crate) const CPU_POLICY_CANCEL: &str = "resources/doc/cpu/policy/cancel";
/// The policy buttons' group and the line under them.
const CPU_POLICY_KEY: &str = "resources/doc/cpu/policy";
const CPU_POLICY_SAID: &str = "resources/doc/cpu/policy-said";
/// Whether the page offers CPU policy collection: macOS alone collects it.
const CPU_POLICY_OFFERED: bool = cfg!(target_os = "macos");

/// The controls whose press chooses the scope `action`: the scope row's and the Sessions
/// table's row.
fn scope_controls(action: &str) -> [UiKey; 2] {
    let (top, row) = match action {
        SCOPE_MAC => (SCOPE_MAC.to_owned(), "mac"),
        SCOPE_ATERM => (SCOPE_ATERM.to_owned(), "aterm"),
        _ => (
            format!("{SCOPE_KEY}/session"),
            action.strip_prefix(SCOPE_SESSION).unwrap_or_default(),
        ),
    };
    [
        UiKey::new(top),
        UiKey::new(format!("{SESSIONS_KEY}/{row}/choose")),
    ]
}

/// The scope an action of the scope row or the Sessions table chooses at `snap`: `None` for
/// another action, `Some(None)` for a session the reading does not hold.
fn scope_target(action: &str, snap: Option<&Snapshot>) -> Option<Option<PageScope>> {
    match action {
        SCOPE_MAC => Some(Some(PageScope::Mac)),
        SCOPE_ATERM => Some(Some(PageScope::Aterm)),
        _ => {
            let key = action.strip_prefix(SCOPE_SESSION)?.parse().ok()?;
            Some(snap.and_then(|snap| PageScope::session(snap, key)))
        }
    }
}

/// One Sessions row's cost: cores-worth, memory footprint, processes, disk read and write, and
/// GPU share. A figure the reading did not read says so, never 0; a footprint with unread
/// members, and disk rates with members whose counters the OS refused (another user's
/// processes), are lower bounds. `gpu` is the GPU row's figure for the same scope, percent of
/// the device ([`gpu_figures`]), so the table says what the head and the GPU document say.
pub(crate) fn cost_words(
    scope: Option<&aterm_vitals::model::ScopeView>,
    gpu: Option<f64>,
) -> String {
    use aterm_vitals::words;
    let unread = "unread";
    let gpu = gpu.map_or_else(
        || unread.to_owned(),
        |pct| words::pct(Some((pct / 100.0) as f32)),
    );
    let Some(scope) = scope else {
        return format!(
            "cores {unread} \u{b7} memory {unread} \u{b7} processes {unread} \u{b7} disk read \
             {unread} \u{b7} disk write {unread} \u{b7} GPU {gpu}"
        );
    };
    let bound = if scope.refused > 0 { "at least " } else { "" };
    let rate = |bps: Option<f64>| {
        bps.map_or_else(
            || unread.to_owned(),
            |bps| format!("{bound}{}", words::rate_mb_lone(bps)),
        )
    };
    let memory = match scope.footprint {
        Some(bytes) if scope.footprint_unread > 0 => format!("at least {}", words::mem(bytes)),
        Some(bytes) => words::mem(bytes),
        None => unread.to_owned(),
    };
    format!(
        "{} cores \u{b7} memory {memory} \u{b7} {} {} \u{b7} disk read {} \u{b7} \
         disk write {} \u{b7} GPU {}",
        words::cores(scope.cores.level),
        scope.procs,
        if scope.procs == 1 {
            "process"
        } else {
            "processes"
        },
        rate(scope.read_bps),
        rate(scope.write_bps),
        gpu,
    )
}

/// The GPU row's figures for the Sessions table at `snap`, percent of the device: each
/// session's and aterm's client time as the row draws it (scaled to the device's busy share
/// over the client window and held at the device's busy, [`Snapshot::resource`]), and this
/// Mac's the device's busy share. `None` where the row has no figure.
fn gpu_figures(snap: &Snapshot) -> (Vec<Option<f64>>, Option<f64>, Option<f64>) {
    use aterm_vitals::model::Resource;
    let sessions = snap
        .tabs
        .iter()
        .map(|view| snap.resource(Resource::Gpu, &[view.key]).tab.now)
        .collect();
    let row = &snap.resources[Resource::Gpu.index()];
    (sessions, row.aterm.now, row.now)
}

/// The page's model: the reading the host last handed it, and the Spotlight scope as the host's
/// probe last read it, with its revision.
#[derive(Default)]
pub(crate) struct ResourcesApp {
    snap: Option<Arc<Snapshot>>,
    scope: Option<(Arc<ScopeReading>, u64)>,
    on_screen: bool,
    /// Whether a CPU policy collection is pending, as the host last said.
    policy_pending: bool,
    /// The macOS configuration section's readings and presses, as the host last said.
    macos: crate::resources_macos::MacosPage,
    /// The Spotlight document's buttons' state, as the host last said.
    spotlight: SpotlightPage,
}

/// What the host tells the page of the Spotlight document's buttons: the root step running, if
/// any (one runs at a time, whichever page part pressed it), and what the last press did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SpotlightPage {
    pub(crate) pending: Option<Step>,
    pub(crate) said: Option<String>,
}

impl ResourcesApp {
    /// Take `snap` as the page's reading and `scope` as its Spotlight scope. Whether either is
    /// another than the one held.
    pub(crate) fn publish(
        &mut self,
        snap: Arc<Snapshot>,
        scope: Option<(Arc<ScopeReading>, u64)>,
    ) -> bool {
        let moved = self
            .snap
            .as_ref()
            .is_none_or(|held| !Arc::ptr_eq(held, &snap) && held.seq != snap.seq)
            || self.scope.as_ref().map(|(_, rev)| *rev) != scope.as_ref().map(|(_, rev)| *rev);
        self.snap = Some(snap);
        self.scope = scope;
        moved
    }

    /// Take whether a CPU policy collection is pending. Whether that changed.
    pub(crate) fn policy(&mut self, pending: bool) -> bool {
        std::mem::replace(&mut self.policy_pending, pending) != pending
    }

    /// Take the Spotlight document's buttons' state. Whether it changed.
    pub(crate) fn spotlight(&mut self, page: SpotlightPage) -> bool {
        std::mem::replace(&mut self.spotlight, page.clone()) != page
    }

    /// The Spotlight document's buttons as the page offers them now.
    fn spotlight_controls(&self) -> crate::resources_spotlight::Controls {
        crate::resources_spotlight::controls(
            self.scope.as_ref().map(|(scope, _)| &**scope),
            self.snap.as_deref(),
        )
    }

    /// Take the macOS configuration section's state. Whether it changed.
    pub(crate) fn macos(&mut self, page: crate::resources_macos::MacosPage) -> bool {
        std::mem::replace(&mut self.macos, page.clone()) != page
    }

    /// The section's rows, as the page draws them.
    fn macos_rows(&self) -> Vec<crate::resources_macos::ConfigRow> {
        crate::resources_macos::rows(
            self.macos.reading.as_ref().map(|(reading, _)| &**reading),
            self.scope.as_ref().map(|(scope, _)| &**scope),
            self.snap.as_deref(),
            self.macos.alab,
        )
    }

    /// Record whether any view is on screen, returning its rising edge.
    pub(crate) fn seen(&mut self, on: bool) -> bool {
        let rising = on && !self.on_screen;
        self.on_screen = on;
        rising
    }

    #[cfg(test)]
    pub(crate) fn reading(&self) -> Option<&Arc<Snapshot>> {
        self.snap.as_ref()
    }
}

struct Span {
    key: UiKey,
    top: f32,
    bottom: f32,
}

/// A copy is bound to its document while the host completes the clipboard request.
struct PendingCopy {
    operation: OperationId,
    instrument: Instrument,
    report: bool,
}

struct MeasuredDoc {
    face: DocFace,
    height: f32,
    said: Arc<str>,
    /// Where a button over its span labels lies, document pt
    /// ([`crate::stats_lens::PageLaid::span_chips`]).
    chips: Option<Place>,
}

/// What the measured documents were laid for: the reading's sequence, the scope's revision and
/// fingerprint, the Disk chart's span, and the width, scale, text size and font's bits.
type DocKey = (u64, u64, u64, DiskSpan, u32, u32, u32, u32);

#[derive(Default)]
struct DocMemo {
    key: Option<DocKey>,
    docs: Vec<MeasuredDoc>,
    metrics: Option<Arc<crate::deck_face::FaceMetrics>>,
}

#[derive(Default)]
pub(crate) struct ResourcesViewState {
    pub(crate) common: CommonViewState,
    scroll: Cell<f32>,
    room: Cell<f32>,
    limit: Cell<f32>,
    spans: RefCell<Vec<Span>>,
    docs: RefCell<DocMemo>,
    pending: Option<PendingCopy>,
    feedback: std::collections::HashMap<Instrument, String>,
    scope: PageScope,
    /// The span the Disk document's used-space chart draws: the day at open, one a view, never
    /// saved.
    disk_span: DiskSpan,
    /// The control that chose a new scope and where it stood in the room (its top less the
    /// scroll): the next layout scrolls so it stands there again.
    held: Cell<Option<(UiKey, f32)>>,
    /// A block each layout brings to the top of the room until the person scrolls, presses or
    /// moves the focus ([`Self::reveal_on_lay`]): readings that land after an open at a section
    /// grow the documents above it, and the section stays where the open put it.
    pending_reveal: RefCell<Option<UiKey>>,
}

impl ResourcesViewState {
    /// A new view of the page scoped to `scope` ([`PageScope::opened`]).
    pub(crate) fn scoped(scope: PageScope) -> Self {
        Self {
            scope,
            ..Self::default()
        }
    }

    /// Take the reading `snap` ([`PageScope::at_reading`]): a chosen session that ended returns
    /// the view to this Mac.
    pub(crate) fn follow_reading(&mut self, snap: &Snapshot) {
        self.scope = self.scope.at_reading(snap);
    }

    /// A new scope lays every document anew at its own height, which moves the Sessions table
    /// under a still pointer: hold the control of `controls` that chose it where it stands, so
    /// the next layout scrolls by however far it moved. The pressed one is the focused one (a
    /// press or the keyboard focuses it); else the first in sight (the scope row, which heads
    /// the column and no document moves, before the table's row), as `act` reveals its control
    /// before it presses. A control out of sight holds nothing.
    fn hold_in_place(&self, controls: &[UiKey]) {
        let scroll = self.scroll.get();
        let room = self.room.get();
        let spans = self.spans.borrow();
        let in_sight = |key: &UiKey| {
            spans
                .iter()
                .find(|span| span.key == *key)
                .filter(|span| span.bottom > scroll && span.top < scroll + room)
                .map(|span| (span.key.clone(), span.top - scroll))
        };
        let focused = self
            .common
            .last_focus
            .as_ref()
            .filter(|key| controls.contains(key))
            .and_then(in_sight);
        self.held
            .set(focused.or_else(|| controls.iter().find_map(in_sight)));
    }

    /// Bring the block keyed `key` to the top of the room at each layout, and keep it there as
    /// readings land and the documents above it grow, until the person scrolls, presses or moves
    /// the focus ([`Self::release_reveal`]): an open at a section, laid before or after the
    /// reading arrives.
    pub(crate) fn reveal_on_lay(&self, key: UiKey) {
        *self.pending_reveal.borrow_mut() = Some(key);
    }

    /// The person took the column: an open's section no longer holds it ([`Self::reveal_on_lay`]).
    fn release_reveal(&self) {
        self.pending_reveal.borrow_mut().take();
    }

    /// Scroll the column so the block keyed `key` is in sight. Whether the column moved.
    pub(crate) fn reveal_listed_control(&self, key: &UiKey) -> bool {
        let Some((top, bottom)) = self
            .spans
            .borrow()
            .iter()
            .find(|span| span.key == *key)
            .map(|span| (span.top, span.bottom))
        else {
            return false;
        };
        let before = self.scroll.get();
        let room = self.room.get();
        let next = if top < before || bottom - top > room {
            top
        } else if bottom > before + room {
            bottom - room
        } else {
            before
        };
        self.scroll.set(next.clamp(0.0, self.limit.get()));
        (self.scroll.get() - before).abs() > 0.5
    }

    /// Scroll the column by `points`, held to its limit. Whether it moved.
    fn scroll_by(&self, points: f32) -> bool {
        let before = self.scroll.get();
        self.scroll
            .set((before + points).clamp(0.0, self.limit.get()));
        (self.scroll.get() - before).abs() > f32::EPSILON
    }
}

/// Which face a [`ResourcesPaint`] draws: the deck head or one whole document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ResourcesFace {
    /// The deck's Full face across the page.
    Head,
    /// One whole document.
    Doc(DocFace),
}

/// One whole document of the page: a type with no head, so a document is never laid for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum DocFace {
    /// A lens document, with every section in its first form.
    Lens(Instrument),
    /// Spotlight's document, which no band row opens.
    Spotlight,
}

impl ResourcesFace {
    const fn word(self) -> &'static str {
        match self {
            Self::Head => "head",
            Self::Doc(doc) => doc.word(),
        }
    }

    /// Its document's name.
    fn name(self) -> &'static str {
        match self {
            Self::Head => RESOURCES_TITLE,
            Self::Doc(doc) => doc.name(),
        }
    }
}

impl DocFace {
    const fn word(self) -> &'static str {
        match self {
            Self::Lens(instrument) => match instrument.page() {
                Some((_, slug)) => slug,
                None => "unlisted",
            },
            Self::Spotlight => SPOTLIGHT_PAGE.1,
        }
    }

    /// The page's documents for `snap`, in reading order: every lens document
    /// ([`Instrument::page_order`]), with Spotlight's at its rank where this Mac splits by
    /// coalition, the reading Spotlight's figures come from (as `sys` shows its line).
    fn documents(snap: &Snapshot) -> Vec<Self> {
        let mut docs: Vec<(u8, Self)> = Instrument::page_order()
            .into_iter()
            .filter_map(|instrument| Some((instrument.page()?.0, Self::Lens(instrument))))
            .collect();
        if snap.coal_lane != aterm_vitals::explain::CoalLane::Unsupported {
            docs.push((SPOTLIGHT_PAGE.0, Self::Spotlight));
        }
        docs.sort_by_key(|(rank, _)| *rank);
        docs.into_iter().map(|(_, face)| face).collect()
    }

    /// Its document's name.
    fn name(self) -> &'static str {
        match self {
            Self::Lens(instrument) => instrument.word(),
            Self::Spotlight => "Spotlight",
        }
    }
}

/// A painted node of the System Resources page. A node's painter sees only the node, the theme
/// and the roles, so the spec carries the reading and how the window draws: the deck's inks are
/// solved at paint from the page's ground, and its marks and locale are the reading's own.
#[derive(Clone)]
pub(crate) struct ResourcesPaint {
    pub(crate) face: ResourcesFace,
    /// The scope's bar ([`PageScope::bar`]), which each document takes.
    pub(crate) bar: Bar,
    /// The scope's sessions and their tab's name: the head's Tab lane and each document's.
    pub(crate) panes: Arc<[u64]>,
    pub(crate) tab_label: Arc<str>,
    /// The span the Disk document's used-space chart draws, as a card's Ctx takes it.
    pub(crate) disk_span: DiskSpan,
    /// Document words measured once per reading, retained even while this node is offscreen.
    pub(crate) said: Arc<str>,
    pub(crate) snap: Arc<Snapshot>,
    /// The Spotlight scope as the host's probe last read it, and its revision.
    pub(crate) scope: Option<Arc<ScopeReading>>,
    pub(crate) scope_rev: u64,
    /// Differentiate Without Color.
    pub(crate) dwc: bool,
    /// Increase Contrast.
    pub(crate) contrast: bool,
    /// Device pixels per point of the window the page is in.
    pub(crate) scale: f32,
    /// The window's terminal font, device px: the deck's chrome text base.
    pub(crate) font_px: f32,
    /// The native pages' text scale ([`crate::native_appearance::text_scale`]): the head is
    /// the deck magnified by it, text and rows alike, as the page's other text is.
    pub(crate) text: f32,
}

/// Two specs paint alike when they draw the same reading the same way. A [`Snapshot`] is compared
/// by identity or sequence, never field by field: the damage pass compares content on every
/// compile.
impl PartialEq for ResourcesPaint {
    fn eq(&self, other: &Self) -> bool {
        self.face == other.face
            && self.bar == other.bar
            && self.panes == other.panes
            && self.tab_label == other.tab_label
            && self.disk_span == other.disk_span
            && (Arc::ptr_eq(&self.snap, &other.snap) || self.snap.seq == other.snap.seq)
            && self.scope_rev == other.scope_rev
            && self.dwc == other.dwc
            && self.contrast == other.contrast
            && self.scale.to_bits() == other.scale.to_bits()
            && self.font_px.to_bits() == other.font_px.to_bits()
            && self.text.to_bits() == other.text.to_bits()
    }
}

/// The reading by its sequence alone: a node's catch-all fingerprint formats its content.
impl std::fmt::Debug for ResourcesPaint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResourcesPaint")
            .field("face", &self.face)
            .field("bar", &self.bar)
            .field("panes", &self.panes)
            .field("tab_label", &self.tab_label)
            .field("disk_span", &self.disk_span)
            .field("seq", &self.snap.seq)
            .field("scope_rev", &self.scope_rev)
            .field("dwc", &self.dwc)
            .field("contrast", &self.contrast)
            .field("scale", &self.scale)
            .field("font_px", &self.font_px)
            .field("text", &self.text)
            .finish()
    }
}

impl ResourcesPaint {
    /// The deck's input for this reading, inked with `inks`: the Full form, no network band, the
    /// still edge, the scope's sessions as its Tab lane (none outside a session's scope), and the
    /// view's span for the Disk document's used-space chart.
    fn input(&self, inks: aterm_vitals::ink::VitalsInks) -> crate::stats_deck::DeckInput<'_> {
        crate::stats_deck::DeckInput {
            network: None,
            network_place: None,
            network_target: crate::network_deck::Target::Overview,
            disk_span: self.disk_span,
            // The page's CPU document takes no keys, so no key line of it says a collection
            // is pending: the page's own row of policy buttons says it.
            cpu_policy_pending: false,
            lab: None,
            fleet: None,
            snap: Some(&self.snap),
            prev_snap: None,
            tab_panes: self.panes.to_vec(),
            tab_label: self.tab_label.to_string(),
            tab_count: 0,
            inks,
            dwc: self.dwc,
            marks: self
                .snap
                .jobs
                .as_deref()
                .map(crate::stats_paint::marks_of)
                .unwrap_or_default(),
            plate: None,
            form: crate::stats_plan::DeckForm::Full,
            contrast: self.contrast,
            locale: crate::stats_deck::host_locale(Some(&self.snap)),
            live: crate::stats_live::DeckLive::STILL,
        }
    }

    /// Where the deck lays out for a node `width` points wide: in device px, as the band does,
    /// so its boxes and ticks snap to the same device grid. The text scale `t` magnifies the
    /// deck: it lays out `width / t` of its points across the node's device px, at `t` times
    /// the window's scale and font, so the Full table stands `t` times as tall and the node's
    /// device px stay the window's.
    pub(crate) fn geom(&self, width: f32) -> crate::stats_deck::DeckGeom {
        let s = self.scale.max(f32::EPSILON);
        let t = self.text.max(f32::EPSILON);
        crate::stats_deck::DeckGeom {
            w: (width * s).round(),
            h: (crate::stats_deck::FULL_PT * f64::from(s * t)).round() as f32,
            pad: 0.0,
            scale: s * t,
            font_px: self.font_px * t,
        }
    }

    /// The deck's prims and hits for a node `width` points wide, in its own device px.
    pub(crate) fn deck(
        &self,
        width: f32,
        inks: aterm_vitals::ink::VitalsInks,
    ) -> (Vec<DrawPrim>, Vec<crate::stats_paint::Hit>) {
        let (prims, hits, _) = crate::stats_deck::deck_paint_kept(
            &self.geom(width),
            &self.input(inks),
            &mut crate::stats_deck::DeckStill::default(),
        );
        (prims, hits)
    }

    /// Lay a whole document at the page's text scale. Primitives remain in device pixels;
    /// its stated height remains in the unmagnified document's points.
    fn document(
        &self,
        width: f32,
        face: DocFace,
        inks: aterm_vitals::ink::VitalsInks,
        draw: bool,
        metrics: &crate::deck_face::FaceMetrics,
    ) -> crate::stats_lens::PageLaid {
        let text = self.text.max(f32::EPSILON);
        let what = match face {
            DocFace::Lens(instrument) => crate::stats_lens::PageDoc::Lens(instrument),
            DocFace::Spotlight => crate::stats_lens::PageDoc::Spotlight(self.scope.as_deref()),
        };
        crate::stats_lens::lay_page(
            f64::from(width / text),
            &self.input(inks),
            what,
            self.bar,
            self.scale * text,
            self.font_px * text,
            metrics,
            draw,
        )
    }

    /// The inks the deck takes on the page's ground, words and accent.
    pub(crate) fn inks(roles: &crate::settings::Roles) -> aterm_vitals::ink::VitalsInks {
        crate::stats_deck::cached_inks(roles.surface, roles.text_primary, roles.accent)
    }

    /// Paint into the native tray at `rect`: the deck in device px, brought to points.
    pub(crate) fn paint(
        &self,
        prims: &mut Vec<DrawPrim>,
        rect: LogicalRect,
        roles: &crate::settings::Roles,
    ) {
        let inks = Self::inks(roles);
        let mut deck = match self.face {
            ResourcesFace::Head => self.deck(rect.width, inks).0,
            ResourcesFace::Doc(face) => {
                self.document(
                    rect.width,
                    face,
                    inks,
                    true,
                    &crate::deck_face::deck_metrics(),
                )
                .prims
            }
        };
        crate::widget::scale_prims(&mut deck, 1.0 / self.scale.max(f32::EPSILON));
        crate::widget::translate_prims(&mut deck, rect.x, rect.y);
        prims.extend(deck);
    }

    pub(crate) fn semantic_label(&self) -> String {
        match self.face {
            ResourcesFace::Head => format!("{RESOURCES_TITLE} now"),
            face => face.name().to_string(),
        }
    }

    /// Each row's sentence ([`aterm_vitals::words::row_sentence`]), one a line: what
    /// `inspect app/v1 view <id> text` says of the head.
    pub(crate) fn semantic_value(&self) -> String {
        if !matches!(self.face, ResourcesFace::Head) {
            return self.said.to_string();
        }
        let locale = crate::stats_deck::host_locale(Some(&self.snap));
        crate::stats_deck::DECK_RESOURCES
            .iter()
            .map(|&res| aterm_vitals::words::row_sentence(res, &self.snap, &self.panes, &locale))
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub(crate) fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        self.face.hash(&mut hash);
        self.bar.hash(&mut hash);
        self.panes.hash(&mut hash);
        self.tab_label.hash(&mut hash);
        self.disk_span.hash(&mut hash);
        self.snap.seq.hash(&mut hash);
        self.scope_rev.hash(&mut hash);
        (self.dwc, self.contrast).hash(&mut hash);
        (
            self.scale.to_bits(),
            self.font_px.to_bits(),
            self.text.to_bits(),
        )
            .hash(&mut hash);
        hash.finish() | 1
    }

    /// The paint audit's line for the node keyed `key`.
    pub(crate) fn audit_line(&self, key: &str) -> String {
        format!(
            "paint-resources key={key:?} face={} seq={} rows={} paint-fingerprint={:016x}",
            self.face.word(),
            self.snap.seq,
            if matches!(self.face, ResourcesFace::Head) {
                crate::stats_deck::DECK_RESOURCES.len()
            } else {
                1
            },
            self.fingerprint(),
        )
    }
}

fn copy_target(action: &str) -> Option<(Instrument, bool)> {
    Instrument::page_order().into_iter().find_map(|instrument| {
        let (_, slug) = instrument.page()?;
        if instrument.resource().is_some() && action == format!("resources/doc/{slug}/copy") {
            Some((instrument, false))
        } else if instrument == Instrument::Cpu && action == "resources/doc/cpu/copy-report" {
            Some((instrument, true))
        } else {
            None
        }
    })
}

fn copy_actions(
    view: &ResourcesViewState,
    instrument: Instrument,
    key: &str,
    width: f32,
    text: f32,
) -> UiNode {
    let mut buttons = vec![(
        format!("{key}/copy"),
        format!("{key}/copy"),
        format!("Copy {}", instrument.word()),
        "Copy".to_string(),
    )];
    if instrument == Instrument::Cpu {
        buttons.push((
            format!("{key}/copy-report"),
            format!("{key}/copy-report"),
            "Copy CPU Report".to_string(),
            "Copy CPU Report".to_string(),
        ));
    }
    let buttons = buttons
        .into_iter()
        .map(|(node, action, name, face)| {
            let busy = view.pending.as_ref().is_some_and(|pending| {
                pending.instrument == instrument
                    && pending.report == action.ends_with("/copy-report")
            });
            let state = ControlState {
                enabled: view.pending.is_none(),
                busy,
                ..ControlState::default()
            };
            (node, action, name, face, state)
        })
        .collect();
    button_row(
        &format!("{key}/actions"),
        &format!("{} actions", instrument.word()),
        buttons,
        StyleRef::Secondary,
        width,
        text,
    )
}

/// The CPU document's policy buttons and the line under them: Collect is live and Cancel idle
/// until a collection is pending, then the other way round.
fn policy_actions(pending: bool, width: f32, text: f32) -> [UiNode; 2] {
    let live = |enabled| ControlState {
        enabled,
        ..ControlState::default()
    };
    let buttons = vec![
        (
            CPU_POLICY_COLLECT.to_owned(),
            CPU_POLICY_COLLECT.to_owned(),
            "Collect CPU Policy".to_owned(),
            "Collect CPU Policy".to_owned(),
            live(!pending),
        ),
        (
            CPU_POLICY_CANCEL.to_owned(),
            CPU_POLICY_CANCEL.to_owned(),
            "Cancel CPU Policy Collection".to_owned(),
            "Cancel".to_owned(),
            live(pending),
        ),
    ];
    let said = if pending {
        "Authorized collection pending; Cancel ends it. No new collection until its exact \
         collector exits."
    } else {
        "Collect CPU Policy reads CPU policy once, by password; changes no policy, keeps no \
         password."
    };
    [
        button_row(
            CPU_POLICY_KEY,
            "CPU policy",
            buttons,
            StyleRef::Secondary,
            width,
            text,
        ),
        crate::native_about::wrapped(
            CPU_POLICY_SAID,
            said,
            width,
            SemanticRole::Status,
            StyleRef::Quiet,
        ),
    ]
}

/// A `style` button's width for its visual words `face`; a chip's at the wider of its two
/// weights, as Settings' chips measure theirs, so a chosen chip's words fit.
fn button_width(face: &str, style: StyleRef) -> f32 {
    let (px, font) = crate::native_ui::text_paint_metrics(SemanticRole::Button, style);
    let mut wide = crate::tray_raster::ui_text_width_for(font, face, px);
    if style == StyleRef::Chip {
        wide = wide.max(crate::tray_raster::ui_text_width_for(
            crate::widget::TextFace::UiBold,
            face,
            px,
        ));
    }
    wide.ceil() + 32.0
}

/// A group keyed `key` of `style` buttons, each `(key, action, name, visual words, state)`:
/// one row where they fit across `width`, else a column.
fn button_row(
    key: &str,
    label: &str,
    buttons: Vec<(String, String, String, String, ControlState)>,
    style: StyleRef,
    width: f32,
    text: f32,
) -> UiNode {
    styled_button_row(
        key,
        label,
        buttons
            .into_iter()
            .map(|(node, action, name, face, state)| (node, action, name, face, state, style))
            .collect(),
        width,
        text,
    )
}

/// [`button_row`] with each button's own style.
fn styled_button_row(
    key: &str,
    label: &str,
    buttons: Vec<(String, String, String, String, ControlState, StyleRef)>,
    width: f32,
    text: f32,
) -> UiNode {
    let height = 36.0 * text;
    let gap = 12.0 * text;
    let row = buttons
        .iter()
        .map(|(_, _, _, face, _, style)| button_width(face, *style))
        .sum::<f32>()
        + gap * buttons.len().saturating_sub(1) as f32
        <= width;
    let count = buttons.len();
    let children = buttons
        .into_iter()
        .map(|(node, action, name, face, state, style)| {
            let mut node = UiNode::new(
                node,
                UiContent::Button(
                    Control::new(
                        ButtonSpec::new(name).visual_label(face.clone()),
                        ActionId::new(action),
                    )
                    .state(state)
                    .style(style),
                ),
            )
            .layout(Layout::default().height(Length::Fixed(height)));
            if row {
                node.layout.width = Length::Fixed(button_width(&face, style));
            }
            node
        })
        .collect();
    UiNode::new(key, UiContent::Group(GroupSpec::new(label)))
        .layout(
            (if row { Layout::row() } else { Layout::column() })
                .height(Length::Fixed(if row {
                    height
                } else {
                    height * count as f32 + gap * count.saturating_sub(1) as f32
                }))
                .gap(gap),
        )
        .children(children)
}

/// The Spotlight document's buttons and, once a press said something, the line under them: each
/// live as `controls` says, idle while any root step runs, the running one's busy.
fn spotlight_actions(
    controls: &crate::resources_spotlight::Controls,
    page: &SpotlightPage,
    width: f32,
    text: f32,
) -> Vec<UiNode> {
    let buttons = controls
        .buttons
        .iter()
        .map(|&(step, live)| {
            let action = format!("{SPOTLIGHT_CONTROL_PREFIX}{}", spotlight_control_word(step));
            let face = spotlight_control_words(step);
            (
                action.clone(),
                action,
                face.to_owned(),
                face.to_owned(),
                ControlState {
                    enabled: live && page.pending.is_none(),
                    busy: page.pending == Some(step),
                    ..ControlState::default()
                },
                if step == Step::SpotlightScope {
                    StyleRef::Primary
                } else {
                    StyleRef::Secondary
                },
            )
        })
        .collect();
    let mut blocks = vec![styled_button_row(
        SPOTLIGHT_CONTROL_KEY,
        "Spotlight actions",
        buttons,
        width,
        text,
    )];
    if let Some(said) = &page.said {
        blocks.push(crate::native_about::wrapped(
            SPOTLIGHT_SAID,
            said,
            width,
            SemanticRole::Status,
            StyleRef::Quiet,
        ));
    }
    blocks
}

/// A selectable button's state: `selected` while it is the scope.
fn chosen(selected: bool) -> ControlState {
    ControlState {
        selected,
        ..ControlState::default()
    }
}

/// The scope row: This Mac, aterm, and the chosen session while one is. It is one segmented
/// control of chips (one Tab stop), whose chosen chip is the accent's fill, a luminance step
/// from a resting one that holds in High Contrast, not a tint.
fn scope_row(scope: &PageScope, width: f32, text: f32) -> UiNode {
    let mut buttons = vec![
        (
            SCOPE_MAC.to_owned(),
            SCOPE_MAC.to_owned(),
            machine_title(),
            machine_title(),
            chosen(*scope == PageScope::Mac),
        ),
        (
            SCOPE_ATERM.to_owned(),
            SCOPE_ATERM.to_owned(),
            "aterm".to_owned(),
            "aterm".to_owned(),
            chosen(*scope == PageScope::Aterm),
        ),
    ];
    if let PageScope::Session { key, label, .. } = scope {
        buttons.push((
            format!("{SCOPE_KEY}/session"),
            format!("{SCOPE_SESSION}{key}"),
            label.clone(),
            label.clone(),
            chosen(true),
        ));
    }
    button_row(SCOPE_KEY, "Scope", buttons, StyleRef::Chip, width, text).segmented()
}

/// One Sessions row keyed `resources/sessions/<id>`: a chip named `name` whose press chooses
/// `action`'s scope (chosen, the accent's fill), and the row's cost under it.
fn session_row(
    id: &str,
    name: &str,
    action: String,
    selected: bool,
    cost: &str,
    width: f32,
    text: f32,
) -> UiNode {
    let key = format!("{SESSIONS_KEY}/{id}");
    let height = 36.0 * text;
    let gap = 4.0 * text;
    let mut button = UiNode::new(
        format!("{key}/choose"),
        UiContent::Button(
            Control::new(
                ButtonSpec::new(name).visual_label(name),
                ActionId::new(action),
            )
            .state(chosen(selected))
            .style(StyleRef::Chip),
        ),
    )
    .layout(Layout::default().height(Length::Fixed(height)));
    button.layout.width = Length::Fixed(button_width(name, StyleRef::Chip).min(width));
    let cost = crate::native_about::wrapped(
        &format!("{key}/cost"),
        cost,
        width,
        SemanticRole::Status,
        StyleRef::Quiet,
    );
    let total = height + gap + node_height(&cost);
    UiNode::new(
        key,
        UiContent::Group(GroupSpec {
            label: Some(name.to_owned()),
            role: SemanticRole::Group,
            style: StyleRef::Plain,
        }),
    )
    .layout(Layout::column().height(Length::Fixed(total)).gap(gap))
    .children(vec![button, cost])
}

/// A Sessions row's name: its tab's label, the pane's place among its tab's panes in the
/// host's order where the tab is split (`Tab 2 \u{b7} pane 2`), and the program in front.
pub(crate) fn session_name(snap: &Snapshot, view: &aterm_vitals::model::TabView) -> String {
    let mut name = view.label.clone();
    let mut panes = snap.tabs.iter().filter(|pane| pane.tab == view.tab);
    if panes.clone().nth(1).is_some()
        && let Some(at) = panes.position(|pane| pane.key == view.key)
    {
        name.push_str(&format!(" \u{b7} pane {}", at + 1));
    }
    if !view.program.is_empty() {
        name.push_str(&format!(" \u{b7} {}", view.program));
    }
    name
}

/// The Sessions table: a row for each session `snap` holds, in the host's order, then aterm's
/// and this Mac's.
fn session_rows(snap: &Snapshot, scope: &PageScope, width: f32, text: f32) -> Vec<UiNode> {
    let mut rows = vec![crate::native_about::wrapped(
        SESSIONS_KEY,
        "Sessions",
        width,
        SemanticRole::Heading,
        StyleRef::Plain,
    )];
    let (gpu, aterm_gpu, mac_gpu) = gpu_figures(snap);
    for (view, gpu) in snap.tabs.iter().zip(gpu) {
        rows.push(session_row(
            &view.key.to_string(),
            &session_name(snap, view),
            format!("{SCOPE_SESSION}{}", view.key),
            matches!(scope, PageScope::Session { key, .. } if *key == view.key),
            &cost_words(Some(&view.scope), gpu),
            width,
            text,
        ));
    }
    rows.push(session_row(
        "aterm",
        "aterm",
        SCOPE_ATERM.to_owned(),
        *scope == PageScope::Aterm,
        &cost_words(snap.aterm.as_ref(), aterm_gpu),
        width,
        text,
    ));
    rows.push(session_row(
        "mac",
        &machine_title(),
        SCOPE_MAC.to_owned(),
        *scope == PageScope::Mac,
        &cost_words(Some(&snap.global), mac_gpu),
        width,
        text,
    ));
    rows
}

/// The Disk document's node `doc` with its span button laid over the span labels it drew at
/// `chips` (document pt, `text` page points to one): an [`StyleRef::Over`] button, so the labels
/// read through it, named for what it switches and valued at the span `span` drawn. Its press
/// switches the span ([`SPAN_KEY`]); it writes nothing and resizes nothing, so it is no
/// person-only press.
fn span_button(doc: UiNode, chips: Place, span: DiskSpan, text: f32) -> UiNode {
    let button = UiNode::new(
        SPAN_KEY,
        UiContent::Button(
            Control::new(
                ButtonSpec::new("Disk chart span")
                    .description("Switches GB used between the last 24 hours and the last 3 weeks"),
                ActionId::new(SPAN_KEY),
            )
            .value(SemanticValue::Text(span.word().to_owned()))
            .style(StyleRef::Over),
        ),
    )
    .layout(
        Layout::default()
            .width(Length::Fixed(chips.w as f32 * text))
            .height(Length::Fixed(chips.h as f32 * text)),
    );
    let mut layout = doc.layout;
    layout.padding = Insets {
        top: chips.y as f32 * text,
        left: chips.x as f32 * text,
        ..Insets::default()
    };
    doc.layout(layout).children(vec![button])
}

/// What a laid document says: the head's line as its longest source says it (what the page's
/// name and state read: Spotlight's state and wanted scope), its explainer and each well.
pub(crate) fn document_words(laid: crate::stats_lens::PageLaid) -> String {
    std::iter::once(laid.title)
        .chain(laid.source)
        .chain(std::iter::once(laid.explainer))
        .chain(
            laid.wells
                .into_iter()
                .map(|(name, words)| format!("{name}: {words}")),
        )
        .collect::<Vec<_>>()
        .join("\n")
}

/// The macOS configuration section: its heading, what it is, and each row's words, its Apply…
/// where one is offered, and what the last press did. Every block is native text or buttons at a
/// fixed height, so `inspect text` and `controls` read it and a reveal finds it.
fn macos_section(
    rows: &[crate::resources_macos::ConfigRow],
    page: &crate::resources_macos::MacosPage,
    width: f32,
    text: f32,
) -> Vec<UiNode> {
    let mut blocks = vec![
        crate::native_about::wrapped(
            MACOS_KEY,
            "macOS configuration",
            width,
            SemanticRole::Heading,
            StyleRef::Plain,
        ),
        crate::native_about::wrapped(
            &format!("{MACOS_KEY}/about"),
            "Settings of this Mac that change what it spends, each now against wanted. Apply… \
             asks for your Mac password where macOS needs it.",
            width,
            SemanticRole::Status,
            StyleRef::Quiet,
        ),
    ];
    for row in rows {
        let key = format!("{MACOS_KEY}/{}", row.slug);
        blocks.push(crate::native_about::wrapped(
            &key,
            &row.said(),
            width,
            SemanticRole::Text,
            StyleRef::Plain,
        ));
        if row.action.is_some() {
            let action = format!("{MACOS_APPLY_PREFIX}{}", row.slug);
            let busy = page.pending == Some(row.slug);
            blocks.push(button_row(
                &format!("{key}/actions"),
                &format!("{} actions", row.name),
                vec![(
                    action.clone(),
                    action,
                    format!("Apply {}…", row.name),
                    "Apply…".to_owned(),
                    ControlState {
                        enabled: page.pending.is_none(),
                        busy,
                        ..ControlState::default()
                    },
                )],
                StyleRef::Secondary,
                width,
                text,
            ));
        }
        if let Some((_, said)) = page.said.as_ref().filter(|(slug, _)| *slug == row.slug) {
            blocks.push(crate::native_about::wrapped(
                &format!("{key}/said"),
                said,
                width,
                SemanticRole::Status,
                StyleRef::Quiet,
            ));
        }
    }
    blocks
}

fn node_height(node: &UiNode) -> f32 {
    match node.layout.height {
        Length::Fixed(height) => height,
        _ => 0.0,
    }
}

impl ResourcesApp {
    fn document_inks() -> aterm_vitals::ink::VitalsInks {
        ResourcesPaint::inks(&crate::settings::Roles::from_theme(
            aterm_render::Theme::default(),
        ))
    }
}

impl NativeAppModel for ResourcesApp {
    type ViewState = ResourcesViewState;

    fn descriptor(&self) -> AppDescriptor {
        AppDescriptor {
            kind: AppKind::SystemResources,
            name: RESOURCES_TITLE,
            icon: AppIcon::SystemResources,
            singleton: true,
        }
    }

    fn update(
        &mut self,
        view: &mut ResourcesViewState,
        event: AppEvent,
        cx: &mut UpdateCx<'_>,
    ) -> EventResult {
        // The person's scroll, press or focus move takes the column from an open's section.
        if matches!(
            event,
            AppEvent::ScrollLines(_)
                | AppEvent::ScrollAt { .. }
                | AppEvent::ScrollPage(_)
                | AppEvent::Action(_)
                | AppEvent::FocusChanged(Some(_))
        ) {
            view.release_reveal();
        }
        // A scroll held at the page's limit is taken and draws nothing.
        let moved = match event {
            AppEvent::FocusChanged(focus) => {
                if let Some(key) = &focus {
                    view.reveal_listed_control(key);
                }
                view.common.last_focus = focus;
                true
            }
            AppEvent::ScrollLines(lines) | AppEvent::ScrollAt { lines, .. } => {
                view.scroll_by(lines as f32 * 28.0 * crate::native_appearance::text_scale())
            }
            AppEvent::ScrollPage(direction) => {
                view.scroll_by(direction as f32 * (view.room.get() - 28.0).max(28.0))
            }
            #[cfg(any(a11y_tree, test))]
            AppEvent::RevealControl(key) => view.reveal_listed_control(&key),
            // Choosing a scope resizes nothing and writes nothing: the view draws anew.
            AppEvent::Action(action)
                if scope_target(action.id.as_str(), self.snap.as_deref()).is_some() =>
            {
                let Some(Some(scope)) = scope_target(action.id.as_str(), self.snap.as_deref())
                else {
                    return EventResult::Bubble;
                };
                if scope == view.scope {
                    return EventResult::Handled;
                }
                view.hold_in_place(&scope_controls(action.id.as_str()));
                view.scope = scope;
                true
            }
            // Switching the span resizes nothing and writes nothing: the Disk document draws anew,
            // its button held where it stands. Only a laid button switches it.
            AppEvent::Action(action) if action.id.as_str() == SPAN_KEY => {
                let laid = view
                    .docs
                    .borrow()
                    .docs
                    .iter()
                    .any(|doc| doc.chips.is_some());
                if !laid {
                    return EventResult::Bubble;
                }
                view.hold_in_place(&[UiKey::new(SPAN_KEY)]);
                view.disk_span = view.disk_span.other();
                true
            }
            // A person's policy press asks the host, which binds the collection to this view and
            // tells the page it is pending. A press on the idle button does nothing.
            AppEvent::Action(action) if action.id.as_str().starts_with(CPU_POLICY_PREFIX) => {
                let press = match action.id.as_str() {
                    CPU_POLICY_COLLECT if !self.policy_pending => CpuPolicyPress::Collect,
                    CPU_POLICY_CANCEL if self.policy_pending => CpuPolicyPress::Cancel,
                    CPU_POLICY_COLLECT | CPU_POLICY_CANCEL => return EventResult::Handled,
                    _ => return EventResult::Bubble,
                };
                cx.cpu_policy(press);
                return EventResult::Handled;
            }
            // A person's press of a Spotlight document button asks the host to run its step; only
            // a live one does anything, and none while a root step runs.
            AppEvent::Action(action)
                if action.id.as_str().starts_with(SPOTLIGHT_CONTROL_PREFIX) =>
            {
                let Some(step) = spotlight_control_step(action.id.as_str()) else {
                    return EventResult::Bubble;
                };
                let live = self.spotlight_controls().buttons.contains(&(step, true));
                if live && self.spotlight.pending.is_none() {
                    cx.spotlight_control(step);
                }
                return EventResult::Handled;
            }
            // A person's Apply asks the host to apply that row's Wanted; only a press the page
            // offers now does anything.
            AppEvent::Action(action) if action.id.as_str().starts_with(MACOS_APPLY_PREFIX) => {
                let slug = &action.id.as_str()[MACOS_APPLY_PREFIX.len()..];
                let offered = self
                    .macos_rows()
                    .into_iter()
                    .find(|row| row.slug == slug)
                    .and_then(|row| row.action);
                match offered {
                    Some(act) if self.macos.pending.is_none() => cx.macos_configuration(act),
                    Some(_) => {}
                    None if crate::resources_macos::SLUGS.contains(&slug) => {}
                    None => return EventResult::Bubble,
                }
                return EventResult::Handled;
            }
            AppEvent::Action(action) if view.pending.is_none() => {
                let Some((instrument, report)) = copy_target(action.id.as_str()) else {
                    return EventResult::Bubble;
                };
                let Some(snap) = &self.snap else {
                    return EventResult::Bubble;
                };
                let text = if report {
                    crate::stats_lens::cpu_diagnostic_report(snap)
                } else {
                    crate::stats_lens::lens_sentence(
                        snap,
                        instrument,
                        view.scope.panes(),
                        &crate::stats_deck::host_locale(Some(snap)),
                    )
                };
                let operation = cx.clipboard(ClipboardRequest::CopyText {
                    text,
                    sensitive: false,
                });
                view.pending = Some(PendingCopy {
                    operation,
                    instrument,
                    report,
                });
                view.feedback.insert(
                    instrument,
                    format!(
                        "Copying {}{}…",
                        instrument.word(),
                        if report { " report" } else { "" }
                    ),
                );
                true
            }
            AppEvent::ClipboardFinished { operation, outcome }
                if view
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.operation == operation) =>
            {
                let pending = view.pending.take().expect("the matching copy");
                let name = format!(
                    "{}{}",
                    pending.instrument.word(),
                    if pending.report { " report" } else { "" }
                );
                view.feedback.insert(
                    pending.instrument,
                    match outcome {
                        ClipboardOutcome::Copied => format!("{name} copied"),
                        ClipboardOutcome::Failed { message } => {
                            format!("Couldn’t copy {name}: {message}")
                        }
                    },
                );
                true
            }
            _ => return EventResult::Bubble,
        };
        if !moved {
            return EventResult::Handled;
        }
        view.common.presentation_revision = view.common.presentation_revision.wrapping_add(1);
        cx.repaint(DamageRegion::All);
        EventResult::Handled
    }

    fn view(&self, view: &ResourcesViewState, cx: &ViewCx<'_>) -> UiTree {
        let text = crate::native_appearance::text_scale();
        let pad = if cx.viewport.width < 480.0 * text {
            12.0
        } else {
            24.0
        };
        let width = (cx.viewport.width - pad * 2.0).max(1.0);
        let room = (cx.viewport.height - pad * 2.0).max(1.0);
        let gap = 12.0 * text;
        let mut blocks = vec![crate::native_about::wrapped(
            TITLE_KEY,
            RESOURCES_TITLE,
            width,
            SemanticRole::Heading,
            StyleRef::Plain,
        )];
        blocks.push(scope_row(&view.scope, width, text));
        let panes: Arc<[u64]> = Arc::from(view.scope.panes());
        let tab_label: Arc<str> = Arc::from(view.scope.tab_label());
        blocks.push(match &self.snap {
            Some(snap) => {
                let prefs = crate::native_appearance::current_preferences();
                let spec = ResourcesPaint {
                    face: ResourcesFace::Head,
                    bar: view.scope.bar(),
                    panes: Arc::clone(&panes),
                    tab_label: Arc::clone(&tab_label),
                    disk_span: view.disk_span,
                    said: Arc::from(""),
                    snap: Arc::clone(snap),
                    scope: None,
                    scope_rev: 0,
                    dwc: prefs.differentiate_without_color,
                    contrast: prefs.high_contrast,
                    scale: cx.terminal_scale,
                    font_px: cx.terminal_font_px,
                    text,
                };
                // The deck lays out in whole device px: a point height that is not a whole
                // number of them at this scale is the device height brought back to points.
                let height = spec.geom(width).h / spec.scale.max(f32::EPSILON);
                UiNode::new(HEAD_KEY, UiContent::Resources(Box::new(spec)))
                    .layout(Layout::default().height(Length::Fixed(height)))
            }
            None => crate::native_about::wrapped(
                READING_KEY,
                &format!("Reading {MACHINE}\u{2026}"),
                width,
                SemanticRole::Status,
                StyleRef::Quiet,
            ),
        });
        if let Some(snap) = &self.snap {
            let prefs = crate::native_appearance::current_preferences();
            let base = ResourcesPaint {
                face: ResourcesFace::Head,
                bar: view.scope.bar(),
                panes,
                tab_label,
                disk_span: view.disk_span,
                said: Arc::from(""),
                snap: Arc::clone(snap),
                scope: self.scope.as_ref().map(|(scope, _)| Arc::clone(scope)),
                scope_rev: self.scope.as_ref().map_or(0, |(_, rev)| *rev),
                dwc: prefs.differentiate_without_color,
                contrast: prefs.high_contrast,
                scale: cx.terminal_scale,
                font_px: cx.terminal_font_px,
                text,
            };
            // Font changes alter wraps too, so they are part of the geometry key; the scope
            // changes every document's words, and the span the Disk document's.
            let key = (
                snap.seq,
                base.scope_rev,
                view.scope.fingerprint(),
                view.disk_span,
                width.to_bits(),
                cx.terminal_scale.to_bits(),
                text.to_bits(),
                cx.terminal_font_px.to_bits(),
            );
            let metrics = crate::deck_face::deck_metrics();
            let mut memo = view.docs.borrow_mut();
            if memo.key != Some(key)
                || !memo
                    .metrics
                    .as_ref()
                    .is_some_and(|held| Arc::ptr_eq(held, &metrics))
            {
                let inks = Self::document_inks();
                memo.docs = DocFace::documents(snap)
                    .into_iter()
                    .map(|face| {
                        let laid = base.document(width, face, inks, false, &metrics);
                        let height = laid.height_pt as f32 * text;
                        MeasuredDoc {
                            face,
                            height,
                            chips: laid.span_chips,
                            said: Arc::from(document_words(laid)),
                        }
                    })
                    .collect();
                memo.key = Some(key);
                memo.metrics = Some(metrics);
            }
            for doc in &memo.docs {
                let key = format!("resources/doc/{}", doc.face.word());
                if let DocFace::Lens(instrument) = doc.face
                    && instrument.resource().is_some()
                {
                    blocks.push(copy_actions(view, instrument, &key, width, text));
                    if let Some(said) = view.feedback.get(&instrument) {
                        blocks.push(crate::native_about::wrapped(
                            &format!("{key}/said"),
                            said,
                            width,
                            SemanticRole::Status,
                            StyleRef::Quiet,
                        ));
                    }
                    if CPU_POLICY_OFFERED && instrument == Instrument::Cpu {
                        blocks.extend(policy_actions(self.policy_pending, width, text));
                    }
                }
                if doc.face == DocFace::Spotlight {
                    blocks.extend(spotlight_actions(
                        &self.spotlight_controls(),
                        &self.spotlight,
                        width,
                        text,
                    ));
                }
                let spec = ResourcesPaint {
                    face: ResourcesFace::Doc(doc.face),
                    said: Arc::clone(&doc.said),
                    ..base.clone()
                };
                let node = UiNode::new(key, UiContent::Resources(Box::new(spec)))
                    .layout(Layout::default().height(Length::Fixed(doc.height)));
                blocks.push(match doc.chips {
                    Some(chips) => span_button(node, chips, view.disk_span, text),
                    None => node,
                });
            }
            drop(memo);
            blocks.extend(session_rows(snap, &view.scope, width, text));
        }
        blocks.extend(macos_section(&self.macos_rows(), &self.macos, width, text));
        let content = blocks.iter().map(node_height).sum::<f32>()
            + gap * blocks.len().saturating_sub(1) as f32;
        let limit = (content - room).max(0.0);
        let mut spans = Vec::with_capacity(blocks.len());
        let mut top = 0.0;
        for node in &blocks {
            let bottom = top + node_height(node);
            spans.push(Span {
                key: node.key.clone(),
                top,
                bottom,
            });
            for child in &node.children {
                // A child laid over its block (the span button) stands at its own place in it.
                let (top, bottom) = match (node.layout.flow, child.layout.height) {
                    (Flow::Overlay, Length::Fixed(height)) => {
                        let at = top + node.layout.padding.top;
                        (at, at + height)
                    }
                    _ => (top, bottom),
                };
                spans.push(Span {
                    key: child.key.clone(),
                    top,
                    bottom,
                });
            }
            top = bottom + gap;
        }
        view.room.set(room);
        view.limit.set(limit);
        if let Some((key, at)) = view.held.take()
            && let Some(span) = spans.iter().find(|span| span.key == key)
        {
            view.scroll.set(span.top - at);
        }
        // An open's section stays at the top of the room through every layout until the person
        // takes the column, however the documents above it grow as readings land.
        if let Some(span) = view
            .pending_reveal
            .borrow()
            .as_ref()
            .and_then(|key| spans.iter().find(|span| span.key == *key))
        {
            view.scroll.set(span.top);
        }
        view.scroll.set(view.scroll.get().clamp(0.0, limit));
        *view.spans.borrow_mut() = spans;
        let offset = view.scroll.get();
        let mut top = 0.0;
        let blocks = blocks
            .into_iter()
            .map(|node| {
                let bottom = top + node_height(&node);
                let hidden = bottom <= offset || top >= offset + room;
                top = bottom + gap;
                if hidden { node.offscreen() } else { node }
            })
            .collect();
        UiTree::new(
            UiNode::new(
                "resources/root",
                UiContent::Group(GroupSpec {
                    label: Some(RESOURCES_TITLE.to_string()),
                    role: SemanticRole::Application,
                    style: StyleRef::Plain,
                }),
            )
            .layout(Layout::column().padding(Insets::all(pad)))
            .children(vec![
                UiNode::new(
                    "resources/content",
                    UiContent::Group(GroupSpec::new("System Resources content")),
                )
                .layout(
                    Layout::column()
                        .height(Length::Fixed(room))
                        .gap(gap)
                        .scrolled(offset)
                        .clipped(),
                )
                .room(room, limit > 0.0)
                .lists_every_row()
                .children(blocks),
            ]),
        )
    }

    fn commands(&self, _: &ResourcesViewState, _: &mut Vec<Command>) {}

    fn presentation(&self, _: &ResourcesViewState) -> AppPresentation {
        AppPresentation {
            title: RESOURCES_TITLE.into(),
            icon: AppIcon::SystemResources,
            indicators: AppIndicators::default(),
            closable: true,
            tooltip: Some(resources_tooltip()),
        }
    }

    fn prepare_close(&mut self, _: CloseRequest, _: &mut UpdateCx<'_>) -> CloseReadiness {
        // Visibility belongs to all views: preflighting one close must not hide another.
        // Removing the last view drops the model and its visibility bit.
        CloseReadiness::Ready
    }
}

#[cfg(test)]
#[path = "native_resources_tests.rs"]
mod tests;
