// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The harness's side of LabView (`docs/DESIGN-alab-lab-model-2026-11-01.md` §0, §7, §9, step
//! 11): a roster from the lab registry's preferences instead of a sheet.
//!
//! The registry reaches this module as a [`Lab`] the `aterm` executable projects in the
//! background and hands over through a [`Source`] it installs at
//! start (`crates/aterm/src/alab_view.rs`), as the journal's sink is. With none installed there is
//! no lab and every sync reads the sources it always read: a Mac without the registry keeps its
//! rows and its fleet files.
//!
//! - [`fleet_for`]: the `alab` source, the fleet a sync of one CLI reads: this Mac's preferred
//!   accounts, preferred first in rank order. A preference orders a pool and never limits it, so
//!   the sync keeps every row already on this Mac whatever the lab says of it ([`retain`]) and
//!   adds only accounts this Mac prefers, and the default login's account under its lab label;
//!   nothing here drops, refuses or signs out a row.
//! - [`asked_row`]: an account asked for by name (`accounts add <lab label|account key>`), which
//!   joins this Mac's roster for that request and writes no preference.
//! - [`names_this_mac`]: where it holds, the harness reads no sheet: the upkeep's triggers
//!   ([`sync_due`], [`fleet_digest`]), the mailbox labels ([`mailbox_labels`]) and the mail
//!   routes ([`mail_targets`], [`mailbox_of`]) come from the lab.
//! - Native label migration: once this Mac's registry inventory has equalled a peer's ([`Lab::settled`]),
//!   each row whose account the lab calls by another label takes that label, its login
//!   directory pinned where it is, its records moved with it ([`accounts::move_accounts`]) in one
//!   crash-safe commit (the sync receipt, `accounts_sync_commit.rs`).
//! - [`mail_targets`]: the addresses an alias's auth codes arrive at, for the mail routes, and
//!   [`mailbox_labels`]: the labels (each lab label, and the legacy label a renamed row's
//!   label-keyed sign-in secrets still name) the mailbox config maps to addresses.
//! - [`holdings`]: what this Mac runs that it does not prefer, from its rosters, which the
//!   journal publishes as `place` rows; [`Lab::places`] is what every Mac's rows say, and
//!   [`Lab::holders`] reads them beside the preferences ("which Macs hold A"), never as a limit.

use super::account_sync_error::SyncError;
use super::accounts::{self, Class, Cli, Roster, Row, Store, Vendor};
use super::accounts_sync::{self, FleetAccount, FleetRoster};
use std::path::Path;
use std::sync::{Arc, OnceLock};

#[path = "alab_capture.rs"]
mod capture;
pub use capture::{ReceiptGuard, SourceCapture, SourceGuard};

/// The source word a sync names the lab by (`aterm harness accounts sync alab`), and the
/// [`FleetRoster::source`] of the fleet it reads.
pub const SOURCE: &str = "alab";

/// A machine's primary key in the registry, independent of its displayed name.
/// This value grants no authority; the registry's facts binding selects this Mac's key.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MachineKey(String);

impl MachineKey {
    /// Keep the exact key from the registry projection, without interpreting it as a label.
    #[must_use]
    pub fn from_registry(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A registry identity and its optional display label. Identity comparisons use `key`;
/// equality of this complete observation also notices a renamed or unavailable label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MachineRef {
    pub key: MachineKey,
    pub label: Option<String>,
}

impl MachineRef {
    #[must_use]
    pub fn from_registry(key: impl Into<String>, label: Option<String>) -> Self {
        Self {
            key: MachineKey::from_registry(key),
            label,
        }
    }

    /// Presentation only: a missing label is shown by its stable key, never guessed.
    #[must_use]
    pub fn display_or_key(&self) -> &str {
        self.label.as_deref().unwrap_or_else(|| self.key.as_str())
    }
}

/// A synthetic machine whose key deliberately differs from its display label.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn test_machine(label: &str) -> MachineRef {
    MachineRef::from_registry(format!("test-machine:{label}"), Some(label.to_owned()))
}

/// Inputs supplied by the registry projection, not steps performed by the harness.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "LabPreferenceIdentity",
    action = "Bind",
    reason = "The authenticated live facts binding is an input to this projection; its creation is outside this model."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "LabPreferenceIdentity",
    action = "Prefer",
    reason = "The validated preference owner is an input; registry editing and replication are outside this model."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "LabPreferenceIdentity",
    action = "LabelA",
    reason = "A machine display label is an independent input, including an absent or conflicting label."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "LabPreferenceIdentity",
    action = "LabelB",
    reason = "The other machine display label is an independent input, including collisions."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "LabPreferenceIdentity",
    action = "Joinable",
    reason = "Account status is an input; provider authentication is outside this projection."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "LabPreferenceIdentity",
    action = "Rank",
    reason = "Preference rank is an input from the registry, including an unranked preference."
)]
pub fn preference_projection_inputs() {}

/// Inputs to one local source observation; persistence and network behavior are outside it.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "LabSourceAvailability",
    action = "Held",
    reason = "The previously retained projection is an input to this local observation."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "LabSourceAvailability",
    action = "Read",
    reason = "The validated registry reader supplies absence, a complete projection, incomplete history or failure."
)]
#[aterm_spec_anchor::spec_unmodeled(
    machine = "LabSourceAvailability",
    action = "Policy",
    reason = "Local policy is supplied by its admitted file read; editing that file is outside this observation."
)]
pub fn source_observation_inputs() {}

/// The registry as this Mac's harness reads it: what `LabView` projects, with the addresses an
/// account row holds. It stays in this process and is never printed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lab {
    /// The live machine identity this Mac's own facts row binds, with its display label kept
    /// separately. Only an absent binding is `None`; an unavailable label keeps its key.
    pub me: Option<MachineRef>,
    /// The live facts-bound machine's naming projection: no binding, a validated name, or a
    /// bound row whose name is conflicted or invalid. Kept separately from `me` so a missing
    /// display label never becomes permission to invent another name.
    #[cfg(unix)]
    pub machine_name: Option<Result<crate::machine_name::RegistryName, String>>,
    /// This Mac's registry inventory has equalled an authenticated peer's: until then the
    /// registry here may be a part of the lab's, so the lab adds rows and renames none.
    pub settled: bool,
    /// Every account of a pooled CLI, live or not.
    pub accounts: Vec<LabAccount>,
    /// Each address with an `alias_of` link, lower-cased, to the live addresses its auth codes
    /// arrive at, nearest first ([`mail_targets`]).
    pub aliases: std::collections::BTreeMap<String, Vec<String>>,
    /// Each address, lower-cased, to the login (lower-cased) of the mailbox its codes arrive in,
    /// where the lab decides that mailbox (its `mailbox` column, through `alias_of`, or a
    /// mailbox's own address list; never a route or a guess) and this Mac can name the
    /// mailbox's login (the importer's mailbox keys and `ai-mailbox.toml`, an admin Mac's alone):
    /// [`mailbox_of`].
    pub mailboxes: std::collections::BTreeMap<String, String>,
    /// What the journal's live `place` rows say each Mac runs that it does not prefer, where a
    /// reader asks for them (the exports, `aterm db alab export`); the lab every process caches
    /// for its syncs leaves this empty, since none of them reads it.
    pub places: Placements,
}

/// What the journal's live `place` rows say (design §0, §4.17, §7): which Macs run an account
/// they do not prefer, beside the preferences. A place row says what a Mac does, never what it
/// may do: nothing reads one as a limit.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Placements {
    /// Each account key to the machines, by label and smaller number first, whose live place rows
    /// say they run it, each with its reason (`fill`, `borrow`, `stranded`, `probation`).
    pub by_account: std::collections::BTreeMap<String, Vec<(String, String)>>,
}

/// One account of a pooled CLI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LabAccount {
    /// The canonical account key (`ac-…`).
    pub pk: String,
    pub cli: Cli,
    /// `region` for Kimi Code, `flow` for Codex and Grok Build.
    pub vendor: Vendor,
    /// What the lab calls it: a name that names no machine (`claude-1a2b3c`, or the owner's).
    pub label: String,
    /// What a Mac called its row before (`m8-2`, `m106-m38`, `kimi-m16`).
    pub legacy_label: Option<String>,
    /// The login address.
    pub email: String,
    /// The provider identity, where the login is an alias.
    pub identity: Option<String>,
    pub browser: Option<String>,
    /// A live row whose status lets a sign-in be tried: only such an account is added unasked.
    pub joinable: bool,
    /// The machines that prefer it, by registry identity, with labels for display. Smaller
    /// rank comes first, then the smaller machine number, as the registry orders them.
    pub preferred: Vec<(MachineRef, Option<u64>)>,
}

impl LabAccount {
    /// Its rank on `machine`, where `machine` prefers it.
    #[must_use]
    pub fn preferred_on(&self, machine: &MachineKey) -> Option<Option<u64>> {
        self.preferred
            .iter()
            .find(|(held, _)| &held.key == machine)
            .map(|(_, rank)| *rank)
    }

    /// The legacy export format names machines by label. This predicate is for rendering
    /// that format only; local preference decisions use [`Self::preferred_on`].
    pub(crate) fn prefers_label_for_export(&self, label: &str) -> bool {
        self.preferred
            .iter()
            .any(|(machine, _)| machine.label.as_deref() == Some(label))
    }

    /// Whether `row` is this account's: its address or provider identity, in the same realm.
    #[must_use]
    pub fn is_row(&self, row: &Row) -> bool {
        accounts::same_provider_realm(self.cli, row.vendor.region, self.vendor.region)
            && [Some(self.email.as_str()), self.identity.as_deref()]
                .into_iter()
                .flatten()
                .any(|email| row.matches_email(email))
    }
}

impl Lab {
    /// The order a row of `account` takes on this Mac: [`Class::Preferred`] where this Mac
    /// prefers it, else [`Class::Pool`]. An order alone: several Macs may prefer one account.
    #[must_use]
    pub fn class_of(&self, account: &LabAccount) -> Class {
        match self.me.as_ref() {
            Some(me) if account.preferred_on(&me.key).is_some() => Class::Preferred,
            _ => Class::Pool,
        }
    }

    /// Which Macs hold `account` (§7), by label: those that prefer it, in its preference order,
    /// then those whose live place rows say they run it. Neither half is a limit.
    #[must_use]
    pub fn holders(&self, account: &LabAccount) -> Vec<String> {
        let mut out: Vec<String> = account
            .preferred
            .iter()
            .filter_map(|(machine, _)| machine.label.clone())
            .collect();
        for (machine, _) in self
            .places
            .by_account
            .get(&account.pk)
            .into_iter()
            .flatten()
        {
            if !out.contains(machine) {
                out.push(machine.clone());
            }
        }
        out
    }

    /// `cli`'s accounts.
    pub fn of(&self, cli: Cli) -> impl Iterator<Item = &LabAccount> {
        self.accounts.iter().filter(move |a| a.cli == cli)
    }

    /// The one account of `cli` that `row` is; `None` for a row the lab does not know, or one
    /// two accounts could be.
    #[must_use]
    pub fn account_of(&self, cli: Cli, row: &Row) -> Option<&LabAccount> {
        let mut found = self.of(cli).filter(|account| account.is_row(row));
        let first = found.next()?;
        found.next().is_none().then_some(first)
    }
}

/// A credential a mailbox sign-in types, held in a registry `Secret` cell (one-mesh design §4.8):
/// the login address's `password`, the mailbox's `mailbox_password` or its `totp` seed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Part {
    Password,
    MailboxPassword,
    Totp,
}

/// A mailbox the registry describes (`aterm_db::alab::mail::Mailbox`), as a sign-in reads it:
/// its login, webmail and `receives` patterns, and which credentials its cells hold. Never a
/// value: [`reveal`] hands one over when a sign-in types it.
#[derive(Clone, PartialEq, Eq)]
pub struct LabMailbox {
    /// Its registry key (`mb-…`), what [`reveal`] names it by.
    pub pk: String,
    /// Its login address, lower-case.
    pub login: String,
    /// The registry's declared service; it is never inferred from an address.
    pub service: Option<String>,
    /// Its webmail inbox URL.
    pub webmail: Option<String>,
    /// The normalized patterns of what it receives: exact addresses, `*@<domain>`, `*`.
    pub receives: Vec<String>,
    /// The parts whose cells hold a value.
    pub held: Vec<Part>,
}

impl std::fmt::Debug for LabMailbox {
    /// Its key, its masked login and what it holds; never an address or a value.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LabMailbox")
            .field("pk", &self.pk)
            .field("login", &super::mailbox_routes::mask_login(&self.login))
            .field("webmail", &self.webmail.is_some())
            .field("receives", &self.receives.len())
            .field("held", &self.held)
            .finish()
    }
}

/// One admitted source observation. A decision keeps this snapshot even if the
/// source refreshes while the decision is being made.
#[derive(Clone, Debug)]
pub enum SourceSnapshot {
    /// A registry read or local policy is unresolved.
    Pending,
    /// A successful read found no registry source for this installation.
    Absent,
    /// Local policy explicitly disables the registry source.
    Disabled,
    /// A complete retained projection, including one with no local binding.
    Present(Arc<Lab>),
}

impl SourceSnapshot {
    #[must_use]
    pub fn same_observation(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Present(a), Self::Present(b)) => Arc::ptr_eq(a, b) || a == b,
            (Self::Pending, Self::Pending)
            | (Self::Absent, Self::Absent)
            | (Self::Disabled, Self::Disabled) => true,
            _ => false,
        }
    }

    /// The admitted retained projection, if any.
    #[must_use]
    pub fn lab(&self) -> Option<Arc<Lab>> {
        match self {
            Self::Present(lab) => Some(lab.clone()),
            _ => None,
        }
    }

    /// Whether automatic discovery must wait for a usable source observation.
    #[must_use]
    pub fn pending(&self) -> bool {
        matches!(self, Self::Pending)
    }

    /// Whether the local roster policy explicitly disables the registry.
    #[must_use]
    pub fn disabled(&self) -> bool {
        matches!(self, Self::Disabled)
    }

    /// Whether this projection binds the local machine, including a name conflict.
    #[must_use]
    pub fn names_this_mac(&self) -> bool {
        let Self::Present(lab) = self else {
            return false;
        };
        #[cfg(unix)]
        if lab.machine_name.is_some() {
            return true;
        }
        lab.me.is_some()
    }

    /// A sheet is admitted only after a known absence of a local registry binding.
    #[must_use]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LabSourceAvailability",
            action = "Project",
            project = "aterm::alab_view::source_model_tests::project"
        )
    )]
    pub fn permits_sheet(&self) -> bool {
        !self.pending() && !self.names_this_mac()
    }

    /// The naming decision from this same admitted snapshot.
    #[cfg(unix)]
    #[must_use]
    pub fn machine_name(&self) -> crate::machine_name::RegistryNameRead {
        use crate::machine_name::RegistryNameRead;
        match self {
            Self::Pending => RegistryNameRead::Pending,
            Self::Absent | Self::Disabled => RegistryNameRead::Unbound,
            Self::Present(lab) => lab
                .machine_name
                .clone()
                .map_or(RegistryNameRead::Unbound, RegistryNameRead::Bound),
        }
    }
}

/// What gives the harness its [`Lab`]: the `aterm` executable's cache.
pub trait Source: Send + Sync {
    /// Observe local policy and the held projection once. Callers must make each
    /// source-selection decision from the returned snapshot.
    fn snapshot(&self) -> SourceSnapshot;

    /// Capture receipt admission with the exact observation being selected.
    /// Sources that only observe cannot authorize native writes.
    fn capture(&self) -> SourceCapture {
        SourceCapture::observed(self.snapshot())
    }

    /// Apply the requesting caller's subscription policy in addition to the host's.
    fn capture_for(&self, _config: &Path) -> SourceCapture {
        self.capture()
    }

    /// The registry's live mailboxes with a live login, as last projected; empty where no lab
    /// is held.
    fn mail(&self) -> Vec<LabMailbox> {
        Vec::new()
    }

    /// Hand `part` of mailbox `mailbox` to `consume`, journalled on this Mac as a sign-in's
    /// reveal; value-free words where it cannot.
    ///
    /// # Errors
    /// No registry is held here, the cell is empty or holds two values, or it does not open.
    /// Reveal only while the current registry view still has this exact mailbox binding.
    /// Implementations must check binding and secret under the same retained view.
    fn reveal_bound(
        &self,
        expected: &LabMailbox,
        parts: &[Part],
        consume: &mut dyn FnMut(Part, &str),
    ) -> Result<(), &'static str> {
        let _ = (expected, parts, consume);
        Err("bound registry reveal is unavailable")
    }

    fn reveal(
        &self,
        mailbox: &str,
        part: Part,
        consume: &mut dyn FnMut(&str),
    ) -> Result<(), &'static str> {
        let _ = (mailbox, part, consume);
        Err("no lab registry is held here")
    }
}

static INSTALLED: OnceLock<Box<dyn Source>> = OnceLock::new();

/// Install the process's [`Source`], once; a second install is refused.
pub fn install(source: Box<dyn Source>) -> bool {
    INSTALLED.set(source).is_ok()
}

#[cfg(test)]
type Secrets = std::collections::BTreeMap<(String, Part), String>;

#[cfg(test)]
thread_local! {
    static MAIL: std::cell::RefCell<Option<(Vec<LabMailbox>, Secrets)>> =
        const { std::cell::RefCell::new(None) };
    static REVEALS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static OVERRIDE: std::cell::RefCell<Option<SourceSnapshot>> = const { std::cell::RefCell::new(None) };
    static GUARD_OVERRIDE: std::cell::RefCell<Option<ReceiptGuard>> = const { std::cell::RefCell::new(None) };
    static PENDING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static TURNED_OFF: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether the lab this process may hold could not be read yet: a Mac the
/// lab may name then reads no sheet ([`chosen`] refuses a sync with no source named), so a
/// failed first look never labels a row after a machine that a later look would have named.
#[must_use]
pub fn pending() -> bool {
    snapshot().pending()
}

/// Run `f` with this thread's lab [`pending`].
#[cfg(test)]
pub(crate) fn while_pending<T>(f: impl FnOnce() -> T) -> T {
    PENDING.with(|cell| cell.set(true));
    let out = f();
    PENDING.with(|cell| cell.set(false));
    out
}

/// Whether the installed source is explicitly turned off by local policy.
fn turned_off() -> bool {
    snapshot().disabled()
}

/// Run `f` with this thread's source [`turned_off`].
#[cfg(test)]
pub(crate) fn while_turned_off<T>(f: impl FnOnce() -> T) -> T {
    TURNED_OFF.with(|cell| cell.set(true));
    let out = f();
    TURNED_OFF.with(|cell| cell.set(false));
    out
}

/// Whether the lab this process holds names this Mac: its facts row binds a live machine. Where
/// it does, the harness reads no sheet (§0: the sheet is the importer's input alone).
#[must_use]
pub fn names_this_mac() -> bool {
    snapshot().names_this_mac()
}

/// The registry's first-name source, taken in one call so a concurrent refresh cannot turn a
/// pending or conflicted binding into an apparently unbound machine.
#[must_use]
#[cfg(unix)]
pub fn machine_name() -> crate::machine_name::RegistryNameRead {
    snapshot().machine_name()
}

/// One observation for naming, source selection and automatic discovery.
#[must_use]
pub fn snapshot() -> SourceSnapshot {
    #[cfg(test)]
    {
        if TURNED_OFF.with(std::cell::Cell::get) {
            return SourceSnapshot::Disabled;
        }
        if let Some(source) = OVERRIDE.with(|cell| cell.borrow().clone()) {
            return source;
        }
        if PENDING.with(std::cell::Cell::get) {
            return SourceSnapshot::Pending;
        }
    }
    INSTALLED
        .get()
        .map_or(SourceSnapshot::Absent, |source| source.snapshot())
}

/// Capture one source revision for planning and later local receipt admission.
#[must_use]
pub fn capture() -> SourceCapture {
    #[cfg(test)]
    {
        if TURNED_OFF.with(std::cell::Cell::get) || PENDING.with(std::cell::Cell::get) {
            return SourceCapture::observed(snapshot());
        }
        if let Some(source) = OVERRIDE.with(|cell| cell.borrow().clone()) {
            return SourceCapture::guarded(
                source.clone(),
                GUARD_OVERRIDE
                    .with(|cell| cell.borrow().clone())
                    .unwrap_or_else(|| ReceiptGuard::new(capture::TestGuard(source))),
            );
        }
    }
    INSTALLED.get().map_or_else(
        || SourceCapture::observed(SourceSnapshot::Absent),
        |source| source.capture(),
    )
}

/// Capture a source under both host policy and the requesting configuration.
#[must_use]
pub fn capture_for(config: &Path) -> SourceCapture {
    #[cfg(test)]
    if OVERRIDE.with(|cell| cell.borrow().is_some()) || TURNED_OFF.get() || PENDING.get() {
        return capture();
    }
    INSTALLED.get().map_or_else(
        || SourceCapture::observed(SourceSnapshot::Absent),
        |source| source.capture_for(config),
    )
}

/// The lab as this process holds it now.
#[must_use]
pub fn current() -> Option<Arc<Lab>> {
    snapshot().lab()
}

/// Run `f` with `lab` as this thread's [`current`] lab.
#[cfg(test)]
pub(crate) fn with_lab<T>(lab: Lab, f: impl FnOnce() -> T) -> T {
    let _source = TestSourceOverride::new(SourceSnapshot::Present(Arc::new(lab)));
    f()
}

/// A controlled source refresh in a test, restored even when an assertion unwinds.
#[cfg(test)]
pub(crate) struct TestSourceOverride(Option<SourceSnapshot>, Option<ReceiptGuard>);

#[cfg(test)]
impl TestSourceOverride {
    pub(crate) fn new(source: SourceSnapshot) -> Self {
        Self(
            OVERRIDE.with(|cell| cell.replace(Some(source))),
            GUARD_OVERRIDE.with(|cell| cell.take()),
        )
    }

    pub(crate) fn with_guard(self, guard: ReceiptGuard) -> Self {
        GUARD_OVERRIDE.with(|cell| cell.replace(Some(guard)));
        self
    }

    pub(crate) fn set(&self, source: SourceSnapshot) {
        OVERRIDE.with(|cell| *cell.borrow_mut() = Some(source));
    }
}

#[cfg(test)]
impl Drop for TestSourceOverride {
    fn drop(&mut self) {
        OVERRIDE.with(|cell| *cell.borrow_mut() = self.0.take());
        GUARD_OVERRIDE.with(|cell| *cell.borrow_mut() = self.1.take());
    }
}

/// The registry's mailboxes this process holds ([`Source::mail`]): what a mailbox sign-in
/// resolves a login, its webmail and its patterns from before any `ai-mailbox.toml` entry
/// (`headless_oauth::discover_mailbox_config`). Empty where none is held.
#[must_use]
pub fn mail() -> Vec<LabMailbox> {
    #[cfg(test)]
    if let Some(mail) = MAIL.with(|cell| cell.borrow().as_ref().map(|(mail, _)| mail.clone())) {
        return mail;
    }
    if turned_off() {
        return Vec::new();
    }
    INSTALLED
        .get()
        .map(|source| source.mail())
        .unwrap_or_default()
}

/// `part` of registry mailbox `mailbox`, revealed for a sign-in that types it now
/// ([`Source::reveal`]); each call is one journalled reveal, so a caller reveals once per
/// sign-in (`headless_oauth::MailboxConfig`'s memo).
///
/// # Errors
/// As [`Source::reveal`], in value-free words.
pub fn reveal(mailbox: &str, part: Part) -> Result<zeroize::Zeroizing<String>, &'static str> {
    #[cfg(test)]
    if let Some(found) = MAIL.with(|cell| {
        cell.borrow().as_ref().map(|(_, secrets)| {
            REVEALS.with(|n| n.set(n.get() + 1));
            secrets
                .get(&(mailbox.to_owned(), part))
                .cloned()
                .ok_or("not set in the registry")
        })
    }) {
        return found.map(zeroize::Zeroizing::new);
    }
    if turned_off() {
        return Err("the lab roster is turned off here");
    }
    let source = INSTALLED.get().ok_or("no lab registry is held here")?;
    let mut out = zeroize::Zeroizing::new(String::new());
    source.reveal(mailbox, part, &mut |value| out.push_str(value))?;
    Ok(out)
}

/// Reveal a credential only under the exact mailbox metadata the consumer admitted.
///
/// # Errors
/// Binding changed, no registry is held, or its secret cannot be revealed and journalled.
pub fn reveal_bound(
    expected: &LabMailbox,
    parts: &[Part],
) -> Result<Vec<zeroize::Zeroizing<String>>, &'static str> {
    #[cfg(test)]
    if let Some(result) = MAIL.with(|cell| {
        cell.borrow().as_ref().map(|(mail, secrets)| {
            if mail
                .iter()
                .filter(|m| m.pk == expected.pk)
                .collect::<Vec<_>>()
                .as_slice()
                != [expected]
            {
                return Err("registry mailbox binding changed");
            }
            let values: Result<Vec<_>, _> = parts
                .iter()
                .map(|part| {
                    secrets
                        .get(&(expected.pk.clone(), *part))
                        .cloned()
                        .map(zeroize::Zeroizing::new)
                        .ok_or("not set in the registry")
                })
                .collect();
            REVEALS.with(|n| n.set(n.get() + parts.len()));
            values
        })
    }) {
        return result;
    }
    if turned_off() {
        return Err("the lab roster is turned off here");
    }
    let source = INSTALLED.get().ok_or("no lab registry is held here")?;
    let mut out = Vec::new();
    source.reveal_bound(expected, parts, &mut |part, value| {
        out.push((part, zeroize::Zeroizing::new(value.to_owned())))
    })?;
    if out.len() != parts.len()
        || out
            .iter()
            .zip(parts)
            .any(|((part, _), wanted)| part != wanted)
    {
        return Err("registry credential bundle is incomplete");
    }
    Ok(out.into_iter().map(|(_, value)| value).collect())
}

/// Run `f` with `mail` as this thread's registry mailboxes and `secrets` as what [`reveal`]
/// hands over, each `(mailbox key, part)` to its value.
#[cfg(test)]
pub(crate) fn with_mail<T>(
    mail: Vec<LabMailbox>,
    secrets: Vec<((&str, Part), &str)>,
    f: impl FnOnce() -> T,
) -> T {
    let secrets = secrets
        .into_iter()
        .map(|((pk, part), value)| ((pk.to_owned(), part), value.to_owned()))
        .collect();
    MAIL.with(|cell| *cell.borrow_mut() = Some((mail, secrets)));
    REVEALS.with(|n| n.set(0));
    let out = f();
    MAIL.with(|cell| *cell.borrow_mut() = None);
    out
}

/// How many reveals this thread asked for since its [`with_mail`] began.
#[cfg(test)]
pub(crate) fn reveals() -> usize {
    REVEALS.with(std::cell::Cell::get)
}

/// The `alab` fleet of `cli` for this Mac ([`SOURCE`]): every account it prefers that may join,
/// smaller rank first, each under the label `existing` already gives it until a source-admitted
/// label migration, else the lab's. `None` where the lab names no machine for
/// this Mac. Every row has this Mac alone as its machine and [`Class::Preferred`]; a row this Mac
/// has that the fleet does not list stays, by [`retain`]. Every other account of `cli` the lab
/// knows that may join is in [`FleetRoster::known`] under its lab label, so the default login's account is
/// named by the lab ([`accounts_sync::build_machine_roster`]) and never by a machine or a slot
/// read from its address; nothing adds one of those unasked.
#[must_use]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AlabCells",
        action = "Sync",
        project = "aterm_agent::harness::alab_roster::tests::preference_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "FleetMachineAssignment",
        action = "Sync",
        project = "aterm_agent::harness::alab_roster::assignment_tests::order_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "FleetMachineAssignment",
        action = "SyncLabels",
        project = "aterm_agent::harness::alab_roster::assignment_tests::labels_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "LabPreferenceIdentity",
        action = "Project",
        project = "aterm::alab_view::preference_model_tests::project"
    )
)]
pub fn fleet_for(lab: &Lab, cli: Cli, existing: Option<&Roster>) -> Option<FleetRoster> {
    let me = lab.me.clone()?;
    let mut mine: Vec<(&LabAccount, Option<u64>)> = lab
        .of(cli)
        .filter(|account| account.joinable)
        .filter_map(|account| Some((account, account.preferred_on(&me.key)?)))
        .collect();
    mine.sort_by(|(a, ra), (b, rb)| {
        (ra.is_none(), ra, &a.label).cmp(&(rb.is_none(), rb, &b.label))
    });
    let mut accounts: Vec<FleetAccount> = Vec::with_capacity(mine.len());
    let mut clashes = Vec::new();
    for (account, _) in mine {
        let kept = existing.and_then(|roster| {
            let mut rows = roster.rows.iter().filter(|row| account.is_row(row));
            let row = rows.next()?;
            rows.next().is_none().then_some(row)
        });
        let label = kept.map_or_else(|| account.label.clone(), |row| row.label.clone());
        // A label an existing row of another account holds is that row's: the account is not
        // added under it, so a sync never rebuilds that row with another address in its login.
        let held_by_another = kept.is_none()
            && existing.is_some_and(|roster| {
                roster
                    .rows
                    .iter()
                    .any(|row| row.label == label && !account.is_row(row))
            });
        if held_by_another {
            clashes.push((label, account.email.clone()));
            continue;
        }
        if accounts::label_refusal(&label).is_some() || accounts.iter().any(|a| a.label == label) {
            continue;
        }
        accounts.push(fleet_account(account, cli, &me, label, Class::Preferred));
    }
    // Only an account that may be signed in names the default login's row: a paused, retired or
    // barred one joins no roster unasked, as [`asked_row`] refuses it asked (a row it already has
    // stays, by [`retain`]).
    let known = lab
        .of(cli)
        .filter(|account| account.joinable && account.preferred_on(&me.key).is_none())
        .filter(|account| accounts::label_refusal(&account.label).is_none())
        .map(|account| fleet_account(account, cli, &me, account.label.clone(), Class::Pool))
        .collect();
    Some(FleetRoster {
        source: SOURCE.to_owned(),
        accounts,
        alab: Some(me),
        lab_snapshot: Some(Arc::new(lab.clone())),
        known,
        clashes,
        ..FleetRoster::default()
    })
}

/// `account` as a fleet row of `me` under `label`.
fn fleet_account(
    account: &LabAccount,
    cli: Cli,
    me: &MachineRef,
    label: String,
    class: Class,
) -> FleetAccount {
    FleetAccount {
        label,
        machines: vec![me.key.as_str().to_owned()],
        class,
        email: account.email.clone(),
        browser: account.browser.clone(),
        vendor: match cli {
            Cli::Claude => Vendor::default(),
            _ => account.vendor,
        },
    }
}

/// The `alab` fleet a sync of `cli` reads, given the source it was `named` (none, or a path or
/// URL), its roster at `roster_path` and the `aterm.toml` at `config`: with [`SOURCE`] named,
/// the lab's fleet or a refusal where no lab names this Mac; with another source named, or for
/// Claude Code a source the owner set in `config` (`[fleet] accounts_source`), none, so that
/// source is read; with none named, the lab's fleet ahead of every other default once the lab
/// names this Mac, even one that prefers nothing here: an empty lab roster stays empty (a sync
/// keeps every row the roster has and adds none), and no legacy discovery labels a row after a
/// machine or reads a slot from an address on a Mac the lab names. A Mac the lab does not name
/// keeps reading what it always read. The roster is read without the lock, only to keep each
/// account's label; the sync reads it again under the lock.
///
/// # Errors
/// [`SOURCE`] was named and no lab names this Mac; or no other source was named and the lab is
/// [`pending`], so whether it names this Mac is not known.
pub(super) fn chosen(
    cli: Cli,
    roster_path: Option<&Path>,
    named: Option<&str>,
    config: Option<&Path>,
) -> Result<Option<FleetRoster>, String> {
    // An explicit external source need not open the registry at all.
    let named = named.map(str::trim).filter(|s| !s.is_empty());
    if named.is_some_and(|named| named != SOURCE)
        || (named.is_none()
            && cli == Cli::Claude
            && config.and_then(accounts_sync::source_key).is_some())
    {
        return Ok(None);
    }
    chosen_from(&snapshot(), cli, roster_path, named, config)
}

/// Select from the observation a caller retained, without reading another projection.
pub(super) fn chosen_from(
    source: &SourceSnapshot,
    cli: Cli,
    roster_path: Option<&Path>,
    named: Option<&str>,
    config: Option<&Path>,
) -> Result<Option<FleetRoster>, String> {
    chosen_captured(
        source,
        cli,
        roster_path,
        named,
        config.and_then(accounts_sync::source_key).is_some(),
    )
}

pub(super) fn chosen_captured(
    source: &SourceSnapshot,
    cli: Cli,
    roster_path: Option<&Path>,
    named: Option<&str>,
    has_key: bool,
) -> Result<Option<FleetRoster>, String> {
    let named = named.map(str::trim).filter(|s| !s.is_empty());
    if named.is_some_and(|named| named != SOURCE) {
        return Ok(None);
    }
    if named.is_none() && cli == Cli::Claude && has_key {
        return Ok(None);
    }
    let existing = roster_path.and_then(|path| Roster::load_for(cli, path).ok());
    let fleet = source
        .lab()
        .and_then(|lab| fleet_for(&lab, cli, existing.as_ref()));
    if source.pending() {
        return Err(
            "the lab source or its local policy could not be read, so this Mac may be one it \
             names: nothing was synced (no sheet is read meanwhile); a later sync reads it again"
                .to_owned(),
        );
    }
    if source.names_this_mac() && fleet.is_none() {
        return Err(
            "the lab binds this Mac but supplies no usable machine key; nothing was synced"
                .to_owned(),
        );
    }
    if named.is_some() {
        return fleet.map(Some).ok_or_else(|| {
            if source.disabled() {
                "`[alab] roster = false` in aterm.toml turns the lab source off, so the `alab` \
                 source has nothing to read"
                    .to_owned()
            } else {
                "the lab registry names no machine for this Mac yet (its facts row binds none, or \
                 this Mac does not hold the registry), so the `alab` source has nothing to read"
                    .to_owned()
            }
        });
    }
    Ok(fleet)
}

/// The roster row `name` asks for (§0, §7: "any account asked for by name"): the one account of
/// `cli` the lab this process holds calls `name` (its label, legacy label or account key `ac-…`;
/// an address is never a name), under its lab label, with its address, provider identity,
/// browser and vendor keys and its derived order. `accounts add` writes it to this Mac's roster
/// and no preference anywhere, and every later sync keeps it, as it keeps every row a Mac has.
/// `None` where no lab names this Mac or no account of `cli` has that name, so `name` is a new
/// label as it always was.
///
/// # Errors
/// Several accounts have that name (a label is unique only as a finding), or the account may
/// not be signed in (paused, broken, banned or retired), so a sign-in nobody may try is never
/// set up.
pub fn asked_row(cli: Cli, name: &str) -> Option<Result<Row, String>> {
    asked_row_from(&snapshot(), cli, name)
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "FleetMachineAssignment",
        action = "Ask",
        project = "aterm_agent::harness::alab_roster::assignment_tests::order_transition"
    )
)]
pub(super) fn asked_row_from(
    source: &SourceSnapshot,
    cli: Cli,
    name: &str,
) -> Option<Result<Row, String>> {
    let lab = source.lab()?;
    lab.me.as_ref()?;
    let name = name.trim();
    let mut found = lab.of(cli).filter(|account| {
        account.pk == name || account.label == name || account.legacy_label.as_deref() == Some(name)
    });
    let account = match (found.next(), found.next()) {
        (None, _) => return None,
        (Some(account), None) => account,
        (Some(_), Some(_)) => {
            return Some(Err(format!(
                "several {} accounts of the lab are called `{}`: name one by its account key",
                cli.connector().product,
                super::one_line(name, 64)
            )));
        }
    };
    if !account.joinable {
        return Some(Err(format!(
            "the lab's `{}` may not be signed in (it is paused, retired or barred), so it joins \
             no roster",
            account.label
        )));
    }
    if let Some(why) = accounts::label_refusal(&account.label) {
        return Some(Err(format!("the lab's label `{}` {why}", account.label)));
    }
    Some(Ok(Row {
        label: account.label.clone(),
        class: lab.class_of(account),
        email: Some(account.email.clone()),
        identity_email: account
            .identity
            .clone()
            .filter(|_| matches!(cli, Cli::Claude | Cli::Codex)),
        browser: account.browser.clone(),
        vendor: match cli {
            Cli::Claude => Vendor::default(),
            _ => account.vendor,
        },
        ..Row::default()
    }))
}

/// Whether a sync of `cli`'s roster at `roster_path` would change it under the lab this process
/// holds, read in memory with no sheet, fleet file or provider read: the lab names this Mac and
/// prefers an account no row holds while the roster has room for it, or one whose row names
/// another browser profile than the lab's, a row holds a hold an old pool file left, a row the
/// lab knows stands in another order than its derived one, or, once the lab is [`Lab::settled`],
/// a row would take its lab label ([`renames`], against `retained`). The upkeep's trigger where the lab
/// names this Mac.
#[must_use]
pub fn sync_due(cli: Cli, roster_path: &Path, retained: impl Fn(&str) -> bool) -> bool {
    let Some(lab) = current() else {
        return false;
    };
    let existing = Roster::load_for(cli, roster_path).unwrap_or_default();
    let Some(fleet) = fleet_for(&lab, cli, Some(&existing)) else {
        return false;
    };
    // New accounts and changed browser profiles need a roster refresh.
    fleet
        .accounts
        .iter()
        .any(|account| match existing.row(&account.label) {
            None => true,
            // A sync writes the lab's browser profile into the row of an account it lists.
            Some(row) => account.browser.is_some() && row.browser != account.browser,
        })
        || existing.rows.iter().any(|row| {
            row.held_by.is_some()
                || lab
                    .account_of(cli, row)
                    .is_some_and(|account| lab.class_of(account) != row.class)
        })
        || lab
            .me
            .as_ref()
            .is_some_and(|me| order_rows(Some(&lab), cli, &me.key, &mut existing.rows.clone()))
        || (lab.settled && !renames(&lab, cli, &existing, retained).moved.is_empty())
}

/// A digest of what a sync of `cli` reads from the lab this process holds: this Mac's key and label,
/// whether the lab is settled, and every account of `cli` (key, labels, address, identity,
/// vendor keys, whether it may join and the Macs that prefer it). The upkeep's sync inputs where
/// the lab names this Mac, in place of the sheet's and fleet files' bytes; `None` where it does
/// not.
#[must_use]
pub fn fleet_digest(cli: Cli) -> Option<[u8; 32]> {
    let lab = current()?;
    let me = lab.me.as_ref()?;
    let mut hash = aterm_digest::Sha256::new();
    hash.update(b"aterm-alab-fleet-v2");
    let mut field = |value: &str| {
        hash.update((value.len() as u64).to_be_bytes());
        hash.update(value.as_bytes());
    };
    field(me.key.as_str());
    field(me.label.as_deref().unwrap_or(""));
    field(if lab.settled { "settled" } else { "partial" });
    for account in lab.of(cli) {
        field(&account.pk);
        field(&account.label);
        field(account.legacy_label.as_deref().unwrap_or(""));
        field(&account.email);
        field(account.identity.as_deref().unwrap_or(""));
        field(account.browser.as_deref().unwrap_or(""));
        field(account.vendor.region.map_or("", |r| r.as_str()));
        field(account.vendor.flow.map_or("", |f| f.as_str()));
        field(if account.joinable { "joinable" } else { "held" });
        for (machine, rank) in &account.preferred {
            field(machine.key.as_str());
            field(machine.label.as_deref().unwrap_or(""));
            field(&rank.map_or_else(String::new, |rank| rank.to_string()));
        }
    }
    Some(hash.finalize())
}

/// A digest of the registry mailboxes this process holds ([`mail`]): each key, login, webmail,
/// pattern and held part, never a value. The sign-in ladder's retry inputs, so a mailbox the
/// registry adds, or a password it gains, releases a row held after a failed sign-in, as a new
/// `ai-mailbox.toml` does.
#[must_use]
pub fn mail_digest() -> [u8; 32] {
    let mut hash = aterm_digest::Sha256::new();
    hash.update(b"aterm-alab-mail-v1");
    let mut field = |value: &str| {
        hash.update((value.len() as u64).to_be_bytes());
        hash.update(value.as_bytes());
    };
    for mailbox in mail() {
        field(&mailbox.pk);
        field(&mailbox.login);
        field(mailbox.webmail.as_deref().unwrap_or(""));
        for pattern in &mailbox.receives {
            field(pattern);
        }
        for part in &mailbox.held {
            field(&format!("{part:?}"));
        }
        field("");
    }
    hash.finalize()
}

/// `roster`, a sync's rebuild from the `alab` fleet, with every row of `existing` it lost put
/// back as it was (its label, login directory, address and keys), so a preference never drops a
/// row; and each row the lab knows given its derived order: [`Class::Preferred`] where `machine`
/// prefers its account, else [`Class::Pool`], which a pool tries after the preferred ones. A
/// row the lab does not know keeps its class. A row is matched by its account, and a rebuilt row
/// holding an existing row's label under another address gives the label back to that row. No
/// row keeps a `held_by`: nothing under the lab limits who runs an account.
#[must_use]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AlabCells",
        action = "Sync",
        project = "aterm_agent::harness::alab_roster::tests::preference_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "FleetMachineAssignment",
        action = "Sync",
        project = "aterm_agent::harness::alab_roster::assignment_tests::order_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "FleetMachineAssignment",
        action = "SyncLabels",
        project = "aterm_agent::harness::alab_roster::assignment_tests::labels_transition"
    )
)]
pub fn retain(
    mut roster: Roster,
    existing: &Roster,
    lab: Option<&Lab>,
    cli: Cli,
    machine: &MachineKey,
) -> Roster {
    for prev in &existing.rows {
        if roster
            .rows
            .iter()
            .any(|row| accounts_sync::same_account(prev, row))
        {
            continue;
        }
        match roster.rows.iter().position(|row| row.label == prev.label) {
            // A row that names no address is known by its label alone.
            Some(_) if prev.expected_identity_email().is_none() => {}
            // Another account rebuilt under this row's label: the row this Mac had takes it
            // back, so its login directory never holds another account's sign-in.
            Some(at) => roster.rows[at] = prev.clone(),
            None => roster.rows.push(prev.clone()),
        }
    }
    for row in &mut roster.rows {
        // A hold an old pool file set would outlive its source: under the lab nothing limits
        // who runs an account here.
        row.held_by = None;
        if let Some(account) = lab.and_then(|lab| lab.account_of(cli, row)) {
            row.class = if account.preferred_on(machine).is_some() {
                Class::Preferred
            } else {
                Class::Pool
            };
        }
    }
    order_rows(lab, cli, machine, &mut roster.rows);
    roster
}

/// Keep the default login first, then class and this key's preference rank.
/// Unknown and unranked rows keep a deterministic label tie-break. Returns whether order moved.
pub(super) fn order_rows(
    lab: Option<&Lab>,
    cli: Cli,
    machine: &MachineKey,
    rows: &mut [Row],
) -> bool {
    let key = |row: &Row| {
        let rank = lab
            .and_then(|lab| lab.account_of(cli, row))
            .and_then(|account| account.preferred_on(machine))
            .flatten();
        (
            row.dir.is_some(),
            row.class,
            rank.is_none(),
            rank,
            row.label.clone(),
        )
    };
    let changed = !rows.is_sorted_by_key(&key);
    rows.sort_by_key(key);
    changed
}

/// `AlabCells`' preference area besides its `Sync`: the owner's verbs and a person's acts, which
/// `a_sync_takes_the_preference_models_steps` sets in the lab and roster each sync reads.
#[cfg(any(test, debug_assertions))]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AlabCells",
        action = "Prefer",
        reason = "A preference is the owner's `lab prefer`, a row of the registry aterm-db writes: the conformance test names this Mac among the account's preferred machines in the Lab the sync reads."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AlabCells",
        action = "Unprefer",
        reason = "Withdrawing a preference is the owner's verb in the registry: the conformance test names another Mac as the account's preferred machine in the Lab the sync reads."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AlabCells",
        action = "Pause",
        reason = "A pause is the account's status in the registry, which LabView projects as `joinable`: the conformance test clears it in the Lab the sync reads."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AlabCells",
        action = "Ask",
        reason = "A person asking for an account by name is `accounts add <lab label|account key>` ([`asked_row`]), which writes a roster row and no preference: the conformance test writes that row into the roster the sync starts from, and `an_account_asked_for_by_name_joins_with_no_preference` holds `asked_row` to it."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AlabCells",
        action = "Hold",
        reason = "A hold is the `held_by` an old pool file left on a row: the conformance test writes it into the roster the sync starts from."
    )
)]
pub fn alab_preference_environment() {}

/// Changes produced by a source-admitted label migration.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Migrated {
    /// `(old label, new label)` for each row renamed.
    pub moved: Vec<(String, String)>,
    /// Rows left under their label: the row's label is not its account's legacy label, or the
    /// new label is refused, held by another row or retained in the store ([`renames`]).
    pub kept: Vec<String>,
}

/// The renames `lab` asks of `roster`: each row whose one lab account has another label, where
/// the row's label is the one the lab recorded the account under before
/// ([`LabAccount::legacy_label`]), and the new label is admissible, held by no other row and
/// not `retained` (records the store still keeps under it, which [`accounts::move_accounts`]
/// refuses). The legacy label is what keeps a label-keyed sign-in secret (a password, a mailbox
/// password, a TOTP seed named `<label>`) reaching the account after the rename: the mailbox
/// config maps it to the account's address from the lab (`headless_oauth::mailbox_aliases`), so
/// a row under any other label, whose secrets nothing would map, keeps its label. Pure.
#[must_use]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "FleetMachineAssignment",
        action = "Migrate",
        project = "aterm_agent::harness::alab_roster::assignment_tests::labels_transition"
    )
)]
pub fn renames(lab: &Lab, cli: Cli, roster: &Roster, retained: impl Fn(&str) -> bool) -> Migrated {
    let mut out = Migrated::default();
    for row in &roster.rows {
        let Some(account) = lab.account_of(cli, row) else {
            continue;
        };
        if account.label == row.label {
            continue;
        }
        let taken = roster.rows.iter().any(|other| other.label == account.label)
            || out.moved.iter().any(|(_, to)| *to == account.label)
            || retained(&account.label);
        let bridged = account.legacy_label.as_deref() == Some(row.label.as_str());
        if taken || !bridged || accounts::label_refusal(&account.label).is_some() {
            out.kept.push(row.label.clone());
        } else {
            out.moved.push((row.label.clone(), account.label.clone()));
        }
    }
    out
}

/// Give each row of `cli`'s roster at `roster_path` the label the lab calls its account by
/// ([`renames`], against the store's records as the lock holds them), once the lab is
/// [`Lab::settled`]: the roster's new text, the state's and the
/// bindings' moved records ([`accounts::move_accounts`]: observations, selections, sessions,
/// claims) go in one commit under the store's writer, so a crash leaves the old files or a
/// receipt the next run (or any reader's recovery) completes. A row's login directory is pinned
/// where it is: the default login stays the default (no `dir`), and every other row keeps the
/// exact `dir` it had, so a running session's [`accounts::LOGIN_ENV`] still names its row and
/// nobody is signed out. A second run finds every label already the lab's and writes nothing.
///
/// # Errors
/// The writer lock is held past `ladder`, an interrupted commit cannot be completed, the roster
/// or the store cannot be read, or the moves are refused (a destination label still holds
/// records): nothing is written.
#[cfg(test)]
pub fn migrate(
    lab: &Lab,
    cli: Cli,
    store: &Store,
    roster_path: &Path,
    now: i64,
    ladder: &[std::time::Duration],
) -> Result<Migrated, String> {
    if !lab.settled {
        return Ok(Migrated::default());
    }
    let held = store
        .acquire_within(ladder)
        .map_err(|why| format!("{}: {why}", store.dir.display()))?
        .ok_or_else(|| format!("{} at {}", accounts::LOCK_HELD, store.dir.display()))?;
    migrate_held(lab, cli, store, &held, roster_path, now, (false, None))
        .map_err(SyncError::into_message)
}

/// Apply native label migration while the caller retains this store's writer.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "FleetMachineAssignment",
        action = "Migrate",
        project = "aterm_agent::harness::alab_roster::assignment_tests::labels_transition"
    )
)]
pub(super) fn migrate_held(
    lab: &Lab,
    cli: Cli,
    store: &Store,
    held: &accounts::Lock,
    roster_path: &Path,
    now: i64,
    (retire, source): (bool, Option<&ReceiptGuard>),
) -> Result<Migrated, SyncError> {
    if !lab.settled {
        return Ok(Migrated::default());
    }
    store.recover_sync_held(held, roster_path)?;
    let (write_to, keep_mode) = super::cli::roster_target(roster_path)?;
    let before = accounts::roster_text(&write_to)?;
    let Some(text) = before.as_deref() else {
        return Ok(Migrated::default());
    };
    let roster = Roster::parse_for(cli, text)?;
    let (state, _) = store.load(now)?;
    let (logins, _) = store.load_logins(now)?;
    let planned = renames(lab, cli, &roster, |label| {
        accounts::label_is_retained(&state, &logins, label)
    });
    if planned.moved.is_empty() {
        return Ok(planned);
    }
    let mut renamed = roster.clone();
    for row in &mut renamed.rows {
        if let Some((_, to)) = planned.moved.iter().find(|(from, _)| *from == row.label) {
            row.label.clone_from(to);
        }
    }
    let after = accounts::append_rows_for(cli, "", &renamed.rows)?;
    let pairs: Vec<(&str, &str)> = planned
        .moved
        .iter()
        .map(|(from, to)| (from.as_str(), to.as_str()))
        .collect();
    store.transact_sync_source_held(
        held,
        now,
        &mut None,
        (
            accounts::SyncTarget {
                fleet: None,
                path: &write_to,
                before: Some(text),
                after: Some(&after),
                mode: keep_mode,
            },
            source,
        ),
        (retire, |state, logins| {
            accounts::move_accounts(state, logins, &pairs)?;
            Ok(Vec::new())
        }),
    )?;
    Ok(planned)
}

/// What this Mac runs of `cli` that it does not prefer, from its `roster` and the lab: each row
/// the lab knows as one account this Mac has no preference for, by account key, with the reason
/// the journal's `place` row gives it (design §0, §4.17): `borrow` where another Mac prefers the
/// account, else `fill` where it may be signed in (the default login, an account asked for by
/// name, a row an earlier source gave this Mac), else `stranded` (the lab bars a sign-in: the row
/// stays, as every row does). The journal says `probation` instead until it saw the account run
/// or sign in here. A row the lab does not know, or one two accounts could be, is not reported.
/// Empty where the lab names no machine for this Mac. Pure.
#[must_use]
pub fn holdings(lab: &Lab, cli: Cli, roster: &Roster) -> Vec<(String, &'static str)> {
    let Some(me) = lab.me.as_ref() else {
        return Vec::new();
    };
    let mut out: Vec<(String, &'static str)> = Vec::new();
    for row in &roster.rows {
        let Some(account) = lab.account_of(cli, row) else {
            continue;
        };
        if account.preferred_on(&me.key).is_some() || out.iter().any(|(pk, _)| *pk == account.pk) {
            continue;
        }
        let why = if account
            .preferred
            .iter()
            .any(|(machine, _)| machine.key != me.key)
        {
            "borrow"
        } else if account.joinable {
            "fill"
        } else {
            "stranded"
        };
        out.push((account.pk.clone(), why));
    }
    out
}

/// The login of the mailbox the lab says `email`'s codes arrive in ([`Lab::mailboxes`]), which
/// `mailbox_routes::resolve_index` takes before any `receives` pattern; `None` where no lab is
/// held or it cannot name one here.
#[must_use]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "FleetMachineAssignment",
        action = "Resolve",
        project = "aterm_agent::harness::alab_roster::assignment_tests::mail_transition"
    )
)]
pub fn mailbox_of(email: &str) -> Option<String> {
    current()?
        .mailboxes
        .get(&email.trim().to_ascii_lowercase())
        .cloned()
}

/// The addresses whose mail routes receive `email`'s auth codes besides its own: the lab's
/// `alias_of` chain, nearest first (§0: an alias is linked with the address its auth login is
/// received at). Empty where no lab is held.
#[must_use]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "FleetMachineAssignment",
        action = "Resolve",
        project = "aterm_agent::harness::alab_roster::assignment_tests::mail_transition"
    )
)]
pub fn mail_targets(email: &str) -> Vec<String> {
    current()
        .and_then(|lab| lab.aliases.get(&email.to_ascii_lowercase()).cloned())
        .unwrap_or_default()
}

/// Each `(cli, label, login address)` the mailbox config maps a `<label>` password, mailbox
/// password or TOTP seed through, from the lab this process holds where it names this Mac: every
/// account's lab label, and its legacy label where that differs, since native migration renames a row
/// only from its legacy label ([`renames`]) and the secrets kept that name. Empty where no lab
/// names this Mac, which then reads its sheet and fleet files as it always did.
#[must_use]
pub fn mailbox_labels() -> Vec<(Cli, String, String)> {
    let Some(lab) = current().filter(|lab| lab.me.is_some()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for account in &lab.accounts {
        out.push((account.cli, account.label.clone(), account.email.clone()));
        if let Some(legacy) = account
            .legacy_label
            .as_deref()
            .filter(|legacy| *legacy != account.label)
        {
            out.push((account.cli, legacy.to_owned(), account.email.clone()));
        }
    }
    out
}

/// `FleetMachineAssignment`'s steps no code here takes: which area a walk checks, and the owner's
/// verbs, a Mac's own rows, the inventory round, the registry's address rows and the mailbox
/// routes, each an input `assignment_tests` (or, for the address rows, `aterm-db`'s chain test)
/// sets in the lab, roster, registry or routes the code then reads.
#[cfg(any(test, debug_assertions))]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "FleetMachineAssignment",
        action = "ChooseFile",
        reason = "The model's choice of the law one walk checks: no code chooses it. fleet_assignment_conforms_and_unsafe_coercions_are_detected takes the file area's steps."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "FleetMachineAssignment",
        action = "ChooseOrder",
        reason = "The model's choice of the law one walk checks: no code chooses it. the_lab_sync_orders_by_this_macs_preference_and_limits_nothing takes the order area's steps."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "FleetMachineAssignment",
        action = "ChooseLabels",
        reason = "The model's choice of the law one walk checks: no code chooses it. labels_come_from_the_lab_and_only_a_legacy_label_is_renamed takes the label area's steps."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "FleetMachineAssignment",
        action = "ChooseMail",
        reason = "The model's choice of the law one walk checks: no code chooses it. an_alias_resolves_to_one_effective_mailbox takes the mail area's steps."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "FleetMachineAssignment",
        action = "PreferHere",
        reason = "A preference is the owner's `lab prefer`, a registry row aterm-db writes: the conformance test names this Mac among the account's preferred machines in the Lab the sync reads."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "FleetMachineAssignment",
        action = "PreferElsewhere",
        reason = "Another Mac's preference is the owner's `lab prefer` for that Mac: the conformance test names m2 among the account's preferred machines in the Lab the sync reads."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "FleetMachineAssignment",
        action = "Unprefer",
        reason = "Withdrawing this Mac's preference is the owner's `lab unprefer`, a registry row aterm-db writes: the conformance test leaves this Mac out of the account's preferred machines in the Lab the next sync reads."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "FleetMachineAssignment",
        action = "RowLegacy",
        reason = "A row a Mac already had under the account's legacy label is the roster an earlier source wrote: the conformance test writes it into the roster the sync starts from."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "FleetMachineAssignment",
        action = "RowOwner",
        reason = "A row under a label the owner gave is `accounts add` or a hand edit: the conformance test writes it into the roster the sync starts from."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "FleetMachineAssignment",
        action = "DefaultLogin",
        reason = "Which account the CLI's default login holds is what the provider's profile reports: the conformance test passes the account's address as the sync's default login."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "FleetMachineAssignment",
        action = "Settle",
        reason = "A repair round's matching inventory (`alab_view::round`) settles the registry: the conformance test sets `Lab::settled`."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "FleetMachineAssignment",
        action = "LabMailbox",
        reason = "An address's own `mailbox` column is the owner's registry row: aterm-db's an_alias_chain_takes_the_harness_models_steps writes it into the registry rows LabView reads, and this crate's test is handed what that read gives."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "FleetMachineAssignment",
        action = "AliasLink",
        reason = "An alias link is the registry's `address.alias_of`: aterm-db's an_alias_chain_takes_the_harness_models_steps writes it into the registry rows LabView reads, and this crate's test is handed what that read gives."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "FleetMachineAssignment",
        action = "SelfLink",
        reason = "An `alias_of` naming the address itself, a cycle of one, is a registry row: aterm-db's an_alias_chain_takes_the_harness_models_steps writes it into the registry rows LabView reads, and this crate's test is handed what that read gives."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "FleetMachineAssignment",
        action = "ChainLink",
        reason = "The linked address's own `alias_of`, a chain's second link, is a registry row: aterm-db's an_alias_chain_takes_the_harness_models_steps writes it into the registry rows LabView reads, and this crate's test is handed what that read gives."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "FleetMachineAssignment",
        action = "CycleLink",
        reason = "An `alias_of` back to the alias, a cycle, is a registry row: aterm-db's an_alias_chain_takes_the_harness_models_steps writes it into the registry rows LabView reads, and this crate's test is handed what that read gives."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "FleetMachineAssignment",
        action = "RetireHub",
        reason = "Retiring an address is a registry row (`row = retired`): aterm-db's an_alias_chain_takes_the_harness_models_steps writes it into the registry rows LabView reads, and this crate's test is handed what that read gives."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "FleetMachineAssignment",
        action = "RetireRoot",
        reason = "Retiring an address is a registry row (`row = retired`): aterm-db's an_alias_chain_takes_the_harness_models_steps writes it into the registry rows LabView reads, and this crate's test is handed what that read gives."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "FleetMachineAssignment",
        action = "RootMailbox",
        reason = "The chain's end's `mailbox` column is the owner's registry row: aterm-db's an_alias_chain_takes_the_harness_models_steps writes it into the registry rows LabView reads, and this crate's test is handed what that read gives."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "FleetMachineAssignment",
        action = "OwnRoute",
        reason = "A route naming the address is a row of ai-mailbox.toml: the conformance test adds it to the routes resolve_index reads."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "FleetMachineAssignment",
        action = "HubRoute",
        reason = "A route naming the chain's first address is a row of ai-mailbox.toml: the conformance test adds it to the routes resolve_index reads."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "FleetMachineAssignment",
        action = "RootRoute",
        reason = "A route naming the chain's second address is a row of ai-mailbox.toml: the conformance test adds it to the routes resolve_index reads."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "FleetMachineAssignment",
        action = "DomainRoute",
        reason = "A `*@domain` route is a row of ai-mailbox.toml: the conformance test adds it to the routes resolve_index reads."
    )
)]
pub fn fleet_assignment_environment() {}

/// Exercise the production source-to-receipt boundary from the host's native
/// registry fixture. The caller supplies an isolated store and roster.
///
/// # Errors
/// The writer, source, original images or durable publication are unavailable.
#[cfg(any(test, debug_assertions))]
pub fn test_publish_source_roster(
    store: &Store,
    path: &Path,
    after: &str,
    source: &ReceiptGuard,
) -> Result<bool, String> {
    let held = store.acquire().map_err(|error| error.to_string())?;
    let before = accounts::roster_text(path)?;
    store
        .transact_roster_admitted_held(
            &held,
            1,
            &mut None,
            accounts::SyncTarget {
                path,
                before: before.as_deref(),
                after: Some(after),
                mode: None,
                fleet: None,
            },
            false,
            Some(source),
        )
        .map_err(SyncError::into_message)
}

#[cfg(test)]
#[path = "alab_roster_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "alab_assignment_tests.rs"]
mod assignment_tests;
