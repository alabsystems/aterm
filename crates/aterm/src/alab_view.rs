// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! LabView's cache (`docs/DESIGN-alab-lab-model-2026-11-01.md` §7, step 11): the harness's
//! `aterm_agent::harness::alab_roster::Source`, filled from
//! `aterm_db::alab::Registry::observe_source`.
//!
//! A projection is read as an observer, writes nothing and calls no provider. The first caller in
//! a process projects once; after that a background thread reads the registry's log and its
//! source revision every five minutes, while every caller gets the held projection at once.
//! Each projection keeps the exact native source it came from. A failed or partial
//! read retains an existing projection. Without one the source is pending, so neither naming
//! nor account discovery can interpret uncertainty as an absent registry binding,
//! and a caller looks again after 15 seconds. Syncs, the accounts list, mail routes and first-name
//! creation ask, never a typing path, so typing never waits on it. `[alab] roster = false` in `aterm.toml` turns the source
//! off on this Mac. A test, a debug build and a build tool's run hold no lab (inert, as the other
//! lab hooks are), so no test reads this Mac's registry.
//! Every getter admits current local policy before exposing a held projection. Unavailable
//! policy hides the projection and blocks automatic fallback without erasing its history.
//!
//! [`round`] records, per registry table, that an authenticated peer reported exactly this Mac's
//! retained registry operations in a round that completed (`RepairRound::matching_inventory`, the
//! one gate that admits a witness): `Lab::settled`, which lets migration rename rows. It stays
//! set for that table: it says this Mac's history is no partial one, which later operations
//! extend and cannot undo.
//!
//! The cached lab carries no `place` rows (`Lab::places` stays empty): no sync, pool or outlook
//! reads them, so no process decodes the journal window for them; the exports read their own
//! ([`placements`], from `alab_sheet`), once per run of the owner verb.

#[cfg(test)]
use aterm_agent::machine_name::RegistryNameRead;
use aterm_agent::{
    ai::connector::{Flow, Region},
    harness::{
        account_sync_error::{Retry, SyncError},
        accounts::{Cli, Vendor},
        alab_roster::{
            self, Lab, LabAccount, LabMailbox, MachineRef, Part, Placements, SourceSnapshot,
        },
    },
    machine_name::RegistryName,
};
#[cfg(test)]
use aterm_db::alab::Inventory;
use aterm_db::alab::{
    Registry, SourceRead,
    journal::place::Places,
    mail,
    registry::AddressIndex,
    secret::Journal,
    view::{LabView, MachineNickname},
};
use aterm_db::catalog::Policy;
use aterm_secrets::client::Paths;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The least time between two looks at the registry's source.
const LOOK_EVERY: Duration = Duration::from_secs(300);

/// The least time between two looks while no projection is held because a read failed: until
/// one succeeds the harness reads no sheet ([`alab_roster::pending`]), so it looks again sooner.
const RETRY_FAILED: Duration = Duration::from_secs(15);

/// Whether this process may hold a lab: a release build outside a build tool's tests, with no
/// scratch state root and no CI. The lab's roster source here, [`crate::alab_mailbox`],
/// [`crate::alab_register`] and [`crate::alab_seen`] hold on this answer; the journal sink
/// (`crate::alab_journal`) does not ask it.
pub(crate) fn holds() -> bool {
    lab_hooks_on(!cfg!(debug_assertions), |key| std::env::var_os(key))
}

/// [`holds`] for a build (`release`) and an environment (`var`) passed in, so a test reaches a
/// shipped build's answer.
fn lab_hooks_on(release: bool, var: impl Fn(&str) -> Option<std::ffi::OsString>) -> bool {
    release
        && var("CARGO_MANIFEST_DIR").is_none()
        && var("ATERM_STATE_HOME").is_none()
        && var("CI").is_none()
}

/// Current local authority for exposing a cached lab projection.
pub(crate) enum LocalRosterPolicy {
    Enabled,
    Disabled,
    Unavailable,
}

/// Read current policy through the common ownership and size admission boundary.
/// Only a missing file or key inherits the enabled default; an unreadable or invalid policy
/// cannot authorize either the cached lab or an automatic fallback source.
pub(crate) fn roster_policy(config: &Path) -> LocalRosterPolicy {
    let Ok(table) = policy_document(config) else {
        return LocalRosterPolicy::Unavailable;
    };
    roster_policy_value(&table)
}

fn policy_document(config: &Path) -> Result<aterm_toml::Value, ()> {
    let text = aterm_types::dirs::read_aterm_config_text(config)
        .map_err(|_| ())?
        .unwrap_or_default();
    aterm_toml::from_str(&text).map_err(|_| ())
}

fn roster_policy_value(table: &aterm_toml::Value) -> LocalRosterPolicy {
    let Some(alab) = table.get("alab") else {
        return LocalRosterPolicy::Enabled;
    };
    let Some(alab) = alab.as_table() else {
        return LocalRosterPolicy::Unavailable;
    };
    match alab.get("roster") {
        None | Some(aterm_toml::Value::Boolean(true)) => LocalRosterPolicy::Enabled,
        Some(aterm_toml::Value::Boolean(false)) => LocalRosterPolicy::Disabled,
        Some(_) => LocalRosterPolicy::Unavailable,
    }
}

/// The private directory recording which registry table's inventory a peer matched.
fn settled_dir() -> Option<PathBuf> {
    Some(aterm_types::dirs::state_dir()?.join("alab"))
}

/// Whether `alab_dir` holds an owner-private `settled` witness for `table`.
fn settled_in(alab_dir: &Path, table: &[u8; 32]) -> bool {
    let Ok(bytes) = aterm_uds::private_file::read_private(alab_dir, "settled", 256) else {
        return false;
    };
    let trimmed = bytes.trim_ascii();
    let mut decoded = [0u8; 32];
    !trimmed.iter().any(u8::is_ascii_uppercase)
        && aterm_codec::hex::decode_into(trimmed, &mut decoded)
        && &decoded == table
}

/// One completed repair round: where it showed an authenticated peer holding exactly this Mac's
/// retained registry operations, record that table, once.
pub(crate) fn round(paths: &Paths, report: &aterm_db::runtime::RepairRound) {
    if !holds() {
        return;
    }
    let Some(dir) = settled_dir() else {
        return;
    };
    // Only the round's completion gate admits a witness: a cancelled, interrupted, deferred or
    // failed round, or one whose witness names another peer, table or inventory, settles nothing.
    let Some(table) = witness_table(
        report
            .matching_inventory()
            .map(|witness| (witness.inventory().count, witness.scope())),
        |table| settled_in(&dir, table),
        || Registry::at(paths.clone()).table().ok().flatten(),
    ) else {
        return;
    };
    let hex = aterm_codec::hex::encode_lower(&table);
    if aterm_types::fs_restricted::ensure_private_dir(&dir).is_ok() {
        let _ = aterm_agent::harness::atomic_write::write_atomic_mode(
            &dir.join("settled"),
            &hex,
            Some(0o600),
        );
    }
}

/// The registry table a completed round's witness (`witness`: the operations it counts and its
/// table's scope) settles: its table where it counts at least one operation, is one of the
/// registry's generations ([`Registry::is_generation`]), is not `settled` already, and is the
/// table the registry holds now (`held`, a read of custody and the catalog, made only where all
/// the rest hold), else none.
fn witness_table(
    witness: Option<(usize, aterm_db::table::Scope)>,
    settled: impl FnOnce(&[u8; 32]) -> bool,
    held: impl FnOnce() -> Option<[u8; 32]>,
) -> Option<[u8; 32]> {
    let (_, scope) =
        witness.filter(|(count, scope)| *count > 0 && Registry::is_generation(*scope))?;
    let table = scope.digest();
    (!settled(&table) && held() == Some(table)).then_some(table)
}

/// Each mailbox key a login of `ai-mailbox.toml` (`logins`) names, to that login lower-cased:
/// the key of the row in `addresses` that says it now, as discovery keys a mailbox
/// ([`mail::login_mailbox`]), so a renamed row's mailbox is named by the address it says.
fn mailbox_logins(addresses: &AddressIndex, logins: &[String]) -> BTreeMap<String, String> {
    logins
        .iter()
        .filter_map(|login| {
            Some((
                mail::login_mailbox(addresses, login.trim())?,
                login.trim().to_ascii_lowercase(),
            ))
        })
        .collect()
}

/// What this Mac reads to name a mailbox key's login ([`mailbox_logins`]): the logins of
/// `~/.secrets/ai-mailbox.toml`, the fallback for a mailbox the registry names no login of
/// (a version-1 row) until B13. Nothing where it is missing or unreadable.
fn mailbox_names(paths: &Paths) -> Vec<String> {
    let file = paths
        .home
        .join(".secrets")
        .join(aterm_agent::harness::headless_oauth::SECRETS_MAILBOX_TOML_FILE);
    aterm_agent::harness::mailbox_routes::mailbox_logins(&file)
}

fn to_lab_mailbox(m: &mail::Mailbox) -> LabMailbox {
    LabMailbox {
        pk: m.pk.clone(),
        login: m.login.to_ascii_lowercase(),
        service: m.service.clone(),
        webmail: m.webmail.clone(),
        receives: m.receives.clone(),
        held: m
            .held
            .iter()
            .map(|which| match which {
                mail::Credential::Password => Part::Password,
                mail::Credential::MailboxPassword => Part::MailboxPassword,
                mail::Credential::Totp => Part::Totp,
            })
            .collect(),
    }
}

/// The registry's mailboxes as the harness reads them ([`LabMailbox`]): never a value.
pub(crate) fn lab_mail(mailboxes: &[mail::Mailbox]) -> Vec<LabMailbox> {
    mailboxes.iter().map(to_lab_mailbox).collect()
}

/// Each mailbox key to its login: the registry's own `login` (one-mesh design step A9), and
/// `toml`'s for a key the registry names none of.
fn logins_of(mail: &[LabMailbox], toml: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut out = toml.clone();
    for mailbox in mail {
        out.insert(mailbox.pk.clone(), mailbox.login.clone());
    }
    out
}

/// Each address of `placed` (address to the mailbox key the lab places it in) whose mailbox
/// `logins` names, to that mailbox's login ([`Lab::mailboxes`]).
fn named_mailboxes(
    placed: &BTreeMap<String, String>,
    logins: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    placed
        .iter()
        .filter_map(|(address, mailbox)| Some((address.clone(), logins.get(mailbox)?.clone())))
        .collect()
}

/// What the journal's live `places` say of `view`'s machines and accounts ([`Lab::places`]): each
/// account to the machines, by label, that run it without preferring it. A row whose writer binds
/// no live machine with a label names no Mac.
pub(crate) fn placements(view: &LabView, places: &Places) -> Placements {
    let mut by_account: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for placed in view.placements(places) {
        let Some(label) = view.machine_label(&placed.machine) else {
            continue;
        };
        by_account
            .entry(placed.account)
            .or_default()
            .push((label.to_owned(), placed.why.word().to_owned()));
    }
    Placements { by_account }
}

/// `view` as the harness reads it, each address the lab places in a mailbox `logins` names
/// given that mailbox's login ([`Lab::mailboxes`]).
pub(crate) fn harness_lab(view: &LabView, settled: bool, logins: &BTreeMap<String, String>) -> Lab {
    let placed = if logins.is_empty() {
        BTreeMap::new()
    } else {
        view.mailboxes()
    };
    harness_lab_with_placed(view, &placed, settled, logins)
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "LabPreferenceIdentity",
        action = "Project",
        project = "aterm::alab_view::preference_model_tests::project"
    )
)]
fn harness_lab_with_placed(
    view: &LabView,
    placed: &BTreeMap<String, String>,
    settled: bool,
    logins: &BTreeMap<String, String>,
) -> Lab {
    let mut accounts = Vec::with_capacity(view.accounts.len());
    for account in view.accounts.values() {
        let (Some(realm), Some(label), Some(email)) =
            (account.realm.as_deref(), &account.label, &account.login)
        else {
            continue;
        };
        let (cli, region) = match realm {
            "claude" => (Cli::Claude, None),
            "codex" => (Cli::Codex, None),
            "grok" => (Cli::Grok, None),
            "kimi:global" => (Cli::Kimi, Some(Region::Global)),
            "kimi:mainland-cn" => (Cli::Kimi, Some(Region::MainlandCn)),
            _ => continue,
        };
        let preferred = view
            .preferring(&account.pk)
            .into_iter()
            .filter_map(|p| {
                let machine = view
                    .machines
                    .get(&p.machine)
                    .filter(|machine| machine.live)?;
                Some((
                    MachineRef::from_registry(p.machine.clone(), machine.label.clone()),
                    p.rank,
                ))
            })
            .collect();
        accounts.push(LabAccount {
            pk: account.pk.clone(),
            cli,
            vendor: Vendor {
                region,
                flow: account.flow.as_deref().and_then(Flow::parse),
            },
            label: label.clone(),
            legacy_label: account.legacy_label.clone(),
            email: email.clone(),
            identity: account.identity.clone(),
            browser: account.browser.clone(),
            joinable: account.joinable(),
            preferred,
        });
    }
    let mailboxes = named_mailboxes(placed, logins);
    Lab {
        me: view
            .my_machine()
            .map(|machine| MachineRef::from_registry(machine.pk.clone(), machine.label.clone())),
        machine_name: registry_name(view),
        settled,
        accounts,
        aliases: view.aliases(),
        mailboxes,
        places: Default::default(),
    }
}

/// Only this Mac's authenticated live facts binding supplies a name. Keep a bound invalid
/// value as an error, never as an absent binding that could authorize a fallback number.
fn registry_name(view: &LabView) -> Option<Result<RegistryName, String>> {
    use aterm_net::fleet_member::name_record::NickClass;
    use aterm_types::machine_name::{Nickname, Number};

    let machine = view.my_machine()?;
    Some((|| {
        let label = machine
            .label
            .as_deref()
            .ok_or("the bound registry machine label is unavailable or conflicted")?;
        let num = Number::parse(label)
            .map_err(|why| format!("bound registry machine label {label:?}: {why}"))?;
        if num.to_string() != label {
            return Err(format!(
                "bound registry machine label {label:?} is not a canonical machine number"
            ));
        }
        let nick = match &machine.nickname {
            MachineNickname::Unset => None,
            MachineNickname::Conflict => {
                return Err(
                    "the bound registry machine nickname is unreadable or conflicted".into(),
                );
            }
            MachineNickname::Named { word, automatic } => Some((
                Nickname::parse(word)
                    .map_err(|why| format!("bound registry machine nickname {word:?}: {why}"))?,
                if *automatic {
                    NickClass::Auto
                } else {
                    NickClass::Chosen
                },
            )),
        };
        Ok(RegistryName { num, nick })
    })())
}

#[derive(Clone)]
struct Projected<S> {
    lab: Arc<Lab>,
    source: S,
}

struct Held<S> {
    /// A projection and its source are installed, retained and cleared together.
    projected: Option<Projected<S>>,
    looked: Option<Instant>,
    /// No complete projection or successful absence has been observed, or an unreadable,
    /// partial read left no projection to use.
    pending: bool,
}

impl<S> Held<S> {
    #[cfg(test)]
    fn lab(&self) -> Option<&Arc<Lab>> {
        self.projected.as_ref().map(|projected| &projected.lab)
    }
}

/// One look's change to what is held, from what it `read`, at `now`. Only a complete projection
/// or a successful absence resolves a first read. Uncertainty retains an existing projection.
fn look<S>(held: &mut Held<S>, read: Result<Option<Projected<S>>, String>, now: Instant) {
    held.looked = Some(now);
    held.pending = read.is_err() && held.projected.is_none();
    match read {
        Ok(projected) => held.projected = projected,
        Err(why) => {
            aterm_log::debug!("lab view: {why}");
        }
    }
}

/// Whether a caller looks at the registry at `now`: at the first look, and once [`RETRY_FAILED`]
/// passed since one that left the source pending; the refresher looks otherwise.
fn due(looked: Option<Instant>, pending: bool, now: Instant) -> bool {
    looked.is_none_or(|at| pending && now.saturating_duration_since(at) >= RETRY_FAILED)
}

struct Cache<S = SourceRead> {
    paths: Paths,
    config: PathBuf,
    settled: Option<PathBuf>,
    held: Mutex<Held<S>>,
    /// Keep an older in-flight read from replacing a newer projection.
    refreshing: Mutex<()>,
    refresher: OnceLock<()>,
}

/// A source's immutable scope identity, without opening its native storage.
trait CachedSource: Clone {
    fn included_by(&self, policy: &Policy) -> bool;
}

impl CachedSource for SourceRead {
    fn included_by(&self, policy: &Policy) -> bool {
        SourceRead::included_by(self, policy)
    }
}

#[cfg(test)]
impl CachedSource for Inventory {
    fn included_by(&self, policy: &Policy) -> bool {
        policy.includes([0; 32], self.0)
    }
}

/// Both local roster selection and Commons subscriptions must currently permit
/// a cached source. This reads configuration only, never vault or table storage.
fn cache_policy(config: &Path) -> Result<Policy, SourceSnapshot> {
    let document = policy_document(config).map_err(|()| SourceSnapshot::Pending)?;
    cache_policy_value(&document)
}

fn cache_policy_value(document: &aterm_toml::Value) -> Result<Policy, SourceSnapshot> {
    match roster_policy_value(document) {
        LocalRosterPolicy::Unavailable => return Err(SourceSnapshot::Pending),
        LocalRosterPolicy::Disabled => return Err(SourceSnapshot::Disabled),
        LocalRosterPolicy::Enabled => {}
    }
    let policy = Policy::from_value(document).map_err(|_| SourceSnapshot::Pending)?;
    if policy.enabled {
        Ok(policy)
    } else {
        Err(SourceSnapshot::Disabled)
    }
}

impl<S: CachedSource> Cache<S> {
    /// Read policy while the held view is fixed, then return one immutable decision input.
    /// Tests inspect prepared state here without opening the live registry.
    fn projection(&self) -> SourceSnapshot {
        self.capture().0
    }

    /// Keep the policy decision and the source of the returned lab in one observation.
    /// A retained token grants no write authority; native admission must validate it again.
    fn capture(&self) -> (SourceSnapshot, Option<Projected<S>>) {
        let (snapshot, projected, _) = self.capture_with_image();
        (snapshot, projected)
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "LabSourceAvailability",
            action = "Project",
            project = "aterm::alab_view::source_model_tests::project"
        )
    )]
    fn capture_with_image(&self) -> (SourceSnapshot, Option<Projected<S>>, Option<String>) {
        let held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let image = match aterm_types::dirs::read_aterm_config_text(&self.config) {
            Ok(image) => image,
            Err(_) => return (SourceSnapshot::Pending, None, None),
        };
        let policy = aterm_toml::from_str::<aterm_toml::Value>(image.as_deref().unwrap_or(""))
            .map_err(|_| SourceSnapshot::Pending)
            .and_then(|document| cache_policy_value(&document));
        let (snapshot, projected) = match policy {
            Err(observation) => (observation, None),
            Ok(policy) => match &held.projected {
                Some(projected) if !projected.source.included_by(&policy) => {
                    (SourceSnapshot::Disabled, None)
                }
                Some(projected) => (
                    SourceSnapshot::Present(projected.lab.clone()),
                    Some(projected.clone()),
                ),
                None if held.pending => (SourceSnapshot::Pending, None),
                None => (SourceSnapshot::Absent, None),
            },
        };
        (snapshot, projected, image)
    }

    /// Serialize complete reads and install the projection with its source in one step.
    fn refresh_with(&self, read: impl FnOnce() -> Result<Option<Projected<S>>, String>) {
        let _refresh = self
            .refreshing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if cache_policy(&self.config).is_err() {
            return;
        }
        let read = read();
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Ok(policy) = cache_policy(&self.config) else {
            return;
        };
        // An excluded old generation must not prevent discovering an included successor.
        // But policy-induced absence is not evidence to erase that old observation.
        if (matches!(&read, Ok(None))
            && held
                .projected
                .as_ref()
                .is_some_and(|held| !held.source.included_by(&policy)))
            || read
                .as_ref()
                .ok()
                .and_then(Option::as_ref)
                .is_some_and(|read| !read.source.included_by(&policy))
        {
            return;
        }
        look(&mut held, read, Instant::now());
    }
}

impl Cache {
    fn lab_for_source(&self, source: &SourceRead) -> Arc<Lab> {
        let settled = self
            .settled
            .as_ref()
            .is_some_and(|dir| settled_in(dir, &source.table()));
        let mail = lab_mail(source.mailboxes());
        let toml = mailbox_logins(source.addresses(), &mailbox_names(&self.paths));
        Arc::new(harness_lab(
            source.view(),
            settled,
            &logins_of(&mail, &toml),
        ))
    }

    fn refresh(&self) {
        self.refresh_with(|| {
            let Some(source) = Registry::at(self.paths.clone())
                .observe_source(&self.config)
                .map_err(|why| why.to_string())?
            else {
                return Ok(None);
            };
            let lab = self.lab_for_source(&source);
            Ok(Some(Projected { lab, source }))
        });
    }

    fn start_refresher(&'static self) {
        self.refresher.get_or_init(|| {
            let _ = std::thread::Builder::new()
                .name("alab-view".into())
                .spawn(move || {
                    loop {
                        std::thread::sleep(LOOK_EVERY);
                        self.refresh();
                    }
                });
        });
    }
    fn refresh_if_due(&'static self) {
        let due = {
            let held = self
                .held
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            due(held.looked, held.pending, Instant::now())
        };
        if due {
            self.refresh();
            self.start_refresher();
        }
    }
}

struct NativeReceiptGuard {
    cache: &'static Cache,
    source: SourceRead,
    lab: Arc<Lab>,
    config_image: Option<String>,
    caller_config: PathBuf,
    caller_image: Option<String>,
}

impl alab_roster::SourceGuard for NativeReceiptGuard {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn same_source(&self, other: &dyn alab_roster::SourceGuard) -> bool {
        other.as_any().downcast_ref::<Self>().is_some_and(|other| {
            std::ptr::eq(self.cache, other.cache)
                && self.source.revision() == other.source.revision()
                && self.lab == other.lab
                && self.config_image == other.config_image
                && self.caller_config == other.caller_config
                && self.caller_image == other.caller_image
        })
    }

    fn with_admitted(
        &self,
        save: Box<dyn FnOnce() -> Result<(), SyncError> + '_>,
    ) -> Result<(), SyncError> {
        let entered = std::cell::Cell::new(false);
        let result = Registry::at(self.cache.paths.clone()).with_admitted_source(
            &self.cache.config,
            Some(self.source.revision()),
            |current| {
                let config = aterm_types::dirs::read_aterm_config_text(&self.cache.config);
                let caller = aterm_types::dirs::read_aterm_config_text(&self.caller_config);
                let caller_permitted = caller.as_ref().ok()
                    .and_then(|image| aterm_toml::from_str::<aterm_toml::Value>(image.as_deref().unwrap_or("")).ok())
                    .and_then(|document| cache_policy_value(&document).ok())
                    .is_some_and(|policy| current.included_by(&policy));
                let permitted = config.as_ref().ok()
                    .and_then(|image| aterm_toml::from_str::<aterm_toml::Value>(image.as_deref().unwrap_or("")).ok())
                    .and_then(|document| cache_policy_value(&document).ok())
                    .is_some_and(|policy| current.included_by(&policy));
                if config.as_ref().ok() != Some(&self.config_image)
                    || caller.as_ref().ok() != Some(&self.caller_image)
                    || !caller_permitted
                    || !permitted
                    || self.cache.lab_for_source(current) != self.lab
                {
                    return Ok(Err(SyncError::retry(Retry::Changed, "native account source or local policy changed before receipt admission")));
                }
                entered.set(true);
                Ok(save())
            },
        ).map_err(|why| SyncError::retry(Retry::Unavailable, why.to_string())).and_then(|result| result);
        // A refusal before the save asks for a new observation without discarding
        // retained history or invalidating a newer entry installed meanwhile.
        if result.is_err() && !entered.get() {
            let mut held = self
                .cache
                .held
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if held.projected.as_ref().is_some_and(|held| {
                held.source.revision() == self.source.revision()
                    && Arc::ptr_eq(&held.lab, &self.lab)
            }) {
                held.looked = None;
            }
        }
        result
    }
}

impl alab_roster::Source for &'static Cache {
    fn snapshot(&self) -> SourceSnapshot {
        self.refresh_if_due();
        self.projection()
    }

    fn capture(&self) -> alab_roster::SourceCapture {
        self.capture_for(&self.config)
    }

    fn capture_for(&self, caller_config: &Path) -> alab_roster::SourceCapture {
        self.refresh_if_due();
        let (snapshot, projected, config_image) = self.capture_with_image();
        let caller_image = if caller_config == self.config.as_path() {
            config_image.clone()
        } else {
            let Ok(image) = aterm_types::dirs::read_aterm_config_text(caller_config) else {
                return alab_roster::SourceCapture::observed(SourceSnapshot::Pending);
            };
            image
        };
        let policy =
            aterm_toml::from_str::<aterm_toml::Value>(caller_image.as_deref().unwrap_or(""))
                .map_err(|_| SourceSnapshot::Pending)
                .and_then(|document| cache_policy_value(&document));
        let policy = match policy {
            Err(snapshot) => return alab_roster::SourceCapture::observed(snapshot),
            Ok(policy) => policy,
        };
        let Some(projected) = projected else {
            return alab_roster::SourceCapture::observed(snapshot);
        };
        if !projected.source.included_by(&policy) {
            return alab_roster::SourceCapture::observed(SourceSnapshot::Disabled);
        }
        alab_roster::SourceCapture::guarded(
            snapshot,
            alab_roster::ReceiptGuard::new(NativeReceiptGuard {
                cache: self,
                source: projected.source,
                lab: projected.lab,
                config_image,
                caller_config: caller_config.to_path_buf(),
                caller_image,
            }),
        )
    }

    fn mail(&self) -> Vec<LabMailbox> {
        self.refresh_if_due();
        Cache::capture(self)
            .1
            .map(|projected| lab_mail(projected.source.mailboxes()))
            .unwrap_or_default()
    }

    fn reveal_bound(
        &self,
        expected: &LabMailbox,
        parts: &[Part],
        consume: &mut dyn FnMut(Part, &str),
    ) -> Result<(), &'static str> {
        if self.snapshot().lab().is_none() {
            return Err("no lab registry is held here");
        }
        let journal = Journal::local().map_err(|_| "the local reveal journal is unavailable")?;
        let which: Vec<_> = parts
            .iter()
            .map(|part| match part {
                Part::Password => mail::Credential::Password,
                Part::MailboxPassword => mail::Credential::MailboxPassword,
                Part::Totp => mail::Credential::Totp,
            })
            .collect();
        let result = mail::reveal_bound(
            &Registry::at(self.paths.clone()),
            &journal,
            &expected.pk,
            &which,
            aterm_time::unix_millis().unwrap_or(0),
            |found| &to_lab_mailbox(found) == expected,
            |values| {
                for (part, value) in parts.iter().zip(values) {
                    consume(*part, value.as_str().expect("registry Secret is UTF-8"));
                }
            },
        );
        match result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(why)) => Err(why.words()),
            Err(_) => Err("registry binding or secret changed or is unavailable"),
        }
    }

    fn reveal(
        &self,
        mailbox: &str,
        part: Part,
        consume: &mut dyn FnMut(&str),
    ) -> Result<(), &'static str> {
        if self.snapshot().lab().is_none() {
            return Err("no lab registry is held here");
        }
        let journal = Journal::local().map_err(|_| "the local reveal journal is unavailable")?;
        let which = match part {
            Part::Password => mail::Credential::Password,
            Part::MailboxPassword => mail::Credential::MailboxPassword,
            Part::Totp => mail::Credential::Totp,
        };
        let ask = mail::Ask {
            mailbox,
            which,
            when_ms: aterm_time::unix_millis().unwrap_or(0),
        };
        match mail::reveal(&Registry::at(self.paths.clone()), &journal, ask, |value| {
            consume(value);
        }) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(withheld)) => Err(withheld.words()),
            Err(_) => Err("the registry's cell did not open or journal here"),
        }
    }
}

static CACHE: OnceLock<Cache> = OnceLock::new();

/// Install the harness's lab source: inert where this process holds no lab ([`holds`]).
pub(crate) fn install() {
    if !holds() {
        return;
    }
    let (Some(home), Some(config)) = (
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|h| h.is_absolute()),
        aterm_types::dirs::aterm_config_path(),
    ) else {
        return;
    };
    let cache = CACHE.get_or_init(|| Cache {
        paths: Paths::for_home(&home),
        config,
        settled: settled_dir(),
        held: Mutex::new(Held {
            projected: None,
            looked: None,
            pending: true,
        }),
        refreshing: Mutex::new(()),
        refresher: OnceLock::new(),
    });
    let _ = alab_roster::install(Box::new(cache));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With no address row saying it, a mailbox key names its login by derivation: each listed
    /// login maps the key its address key derives to the login, lower-cased, and a line that is
    /// no address maps nothing. (A row that says a login now keys it instead:
    /// `aterm_db::alab::mail`'s `a_login_names_the_mailbox_of_the_row_that_says_it_now`.)
    /// NEGATIVE CONTROL: with `mailbox_logins` keying by the login itself, the lookup by the
    /// derived key finds nothing.
    #[test]
    fn a_mailbox_key_names_the_login_it_derives_from() {
        let logins = ["Hub@Example.test".to_owned(), "not an address".to_owned()];
        let named = mailbox_logins(&AddressIndex::default(), &logins);
        assert_eq!(named.len(), 1);
        let key = aterm_db::typed::keys::mailbox_pk(
            &aterm_db::typed::keys::address_pk("hub@example.test").unwrap(),
        )
        .unwrap();
        assert_eq!(
            named.get(&key).map(String::as_str),
            Some("hub@example.test")
        );
    }

    /// A test process holds no lab, so no test reads this Mac's registry.
    /// NEGATIVE CONTROL: with `holds` returning `true`, this fails.
    #[test]
    fn a_test_holds_no_lab() {
        assert!(!holds());
    }

    /// The registry names a mailbox key's login by its own `login` row (step A9), with no
    /// `ai-mailbox.toml`; the file's logins name only a key the registry names none of, and an
    /// address the lab places in a registry mailbox gets that mailbox's login.
    /// NEGATIVE CONTROL: with `logins_of` letting the file's login win, the registry key reads
    /// `old@example.test` and the first assertion fails.
    #[test]
    fn a_registry_mailbox_names_its_own_login() {
        let mailboxes = [aterm_db::alab::mail::Mailbox {
            pk: "mb-registry".into(),
            label: "proton-1".into(),
            login: "Owner@example.test".into(),
            login_pk: "ad-0000000000000000".into(),
            service: Some("proton".into()),
            code_path: Some("webmail".into()),
            webmail: None,
            receives: vec!["*".into()],
            held: vec![mail::Credential::Password, mail::Credential::Totp],
        }];
        let mail = lab_mail(&mailboxes);
        assert_eq!(mail[0].login, "owner@example.test");
        assert_eq!(mail[0].held, [Part::Password, Part::Totp]);
        let toml = BTreeMap::from([
            ("mb-registry".to_owned(), "old@example.test".to_owned()),
            ("mb-file".to_owned(), "file@example.test".to_owned()),
        ]);
        let logins = logins_of(&mail, &toml);
        assert_eq!(logins["mb-registry"], "owner@example.test");
        assert_eq!(logins["mb-file"], "file@example.test");
        let placed = BTreeMap::from([("ai1@example.test".to_owned(), "mb-registry".to_owned())]);
        assert_eq!(
            named_mailboxes(&placed, &logins_of(&mail, &BTreeMap::new()))["ai1@example.test"],
            "owner@example.test"
        );
    }

    pub(super) fn nothing_held() -> Held<Inventory> {
        Held {
            projected: None,
            looked: None,
            pending: true,
        }
    }

    #[cfg(unix)]
    #[test]
    fn current_local_policy_withholds_a_cached_projection_without_erasing_it() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = aterm_tempfile::tempdir().unwrap();
        let config = dir.path().join("aterm.toml");
        aterm_uds::atomic_file::write(&config, b"[alab]\nroster = true\n", 0o600).unwrap();
        let mut held = nothing_held();
        look(&mut held, Ok(Some(naming(Some("m3")))), Instant::now());
        let cache = Cache {
            paths: Paths::for_home(dir.path()),
            config: config.clone(),
            settled: None,
            held: Mutex::new(held),
            refreshing: Mutex::new(()),
            refresher: OnceLock::new(),
        };
        let observed = cache.projection();
        let original = observed.lab().unwrap();
        let revision = cache
            .held
            .lock()
            .unwrap()
            .projected
            .as_ref()
            .unwrap()
            .source;
        assert!(!cache.projection().pending());

        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o664)).unwrap();
        assert!(observed.names_this_mac() && !observed.pending());
        assert!(
            cache.projection().lab().is_none(),
            "an unavailable current policy must not expose the cached lab"
        );
        assert!(
            cache.projection().pending(),
            "unknown policy cannot select a sheet instead"
        );
        assert!(Arc::ptr_eq(
            cache.held.lock().unwrap().lab().unwrap(),
            &original
        ));
        assert_eq!(
            cache
                .held
                .lock()
                .unwrap()
                .projected
                .as_ref()
                .unwrap()
                .source,
            revision
        );

        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
        for invalid in ["[alab", "[alab]\nroster = 'off'\n"] {
            std::fs::write(&config, invalid).unwrap();
            assert!(cache.projection().lab().is_none());
            assert!(cache.projection().pending());
        }
        std::fs::write(&config, "[alab]\nroster = false\n").unwrap();
        cache.refresh_with(|| panic!("excluded policy must not read the source"));
        assert!(
            cache
                .held
                .lock()
                .unwrap()
                .lab()
                .is_some_and(|lab| Arc::ptr_eq(lab, &original)),
            "a disabled background refresh must preserve the view for re-enablement"
        );
        assert!(cache.projection().lab().is_none());
        assert!(
            !cache.projection().pending(),
            "explicit off is a known policy"
        );
        std::fs::write(&config, "[alab]\nroster = true\n").unwrap();
        assert!(Arc::ptr_eq(&cache.projection().lab().unwrap(), &original));
        assert!(!cache.projection().pending());
        std::fs::remove_file(&config).unwrap();
        assert!(Arc::ptr_eq(&cache.projection().lab().unwrap(), &original));
        assert!(
            !cache.projection().pending(),
            "real absence keeps its defaults"
        );
    }

    /// A projection whose facts row binds this Mac to a machine labelled `label` (`None`: the
    /// label is conflicted).
    pub(super) fn naming(label: Option<&str>) -> Projected<Inventory> {
        Projected {
            lab: Arc::new(harness_lab(
                &naming_view(label, MachineNickname::Unset),
                false,
                &BTreeMap::new(),
            )),
            source: Inventory([7; 32]),
        }
    }

    fn naming_view(label: Option<&str>, nickname: MachineNickname) -> LabView {
        let mut view = LabView::default();
        view.me = Some("pk-me".into());
        view.machines.insert(
            "pk-me".into(),
            aterm_db::alab::view::Machine {
                pk: "pk-me".into(),
                label: label.map(str::to_owned),
                nickname,
                aliases: Vec::new(),
                live: true,
            },
        );
        view
    }

    #[test]
    fn equal_machine_labels_do_not_transfer_a_preference_between_registry_keys() {
        use aterm_agent::harness::{accounts::Class, alab_roster};
        use aterm_db::alab::view::{Account, Machine, Preference};

        let mut view = naming_view(Some("m3"), MachineNickname::Unset);
        view.machines.insert(
            "pk-other".into(),
            Machine {
                pk: "pk-other".into(),
                label: Some("m3".into()),
                nickname: MachineNickname::Unset,
                aliases: Vec::new(),
                live: true,
            },
        );
        view.accounts.insert(
            "account-one".into(),
            Account {
                pk: "account-one".into(),
                realm: Some("claude".into()),
                label: Some("claude-one".into()),
                legacy_label: None,
                login: Some("one@example.test".into()),
                identity: None,
                status: Some("active".into()),
                region: None,
                flow: None,
                browser: None,
                live: true,
            },
        );
        view.preferences.push(Preference {
            machine: "pk-other".into(),
            account: "account-one".into(),
            rank: Some(1),
        });

        let project = |view: &LabView| {
            harness_lab_with_placed(view, &BTreeMap::new(), true, &BTreeMap::new())
        };
        let foreign = project(&view);
        assert_eq!(foreign.class_of(&foreign.accounts[0]), Class::Pool);
        assert!(
            alab_roster::fleet_for(&foreign, Cli::Claude, None)
                .unwrap()
                .accounts
                .is_empty(),
            "a different registry key must not enroll an account through a shared label"
        );

        view.preferences[0].machine = "pk-me".into();
        let own = project(&view);
        assert_eq!(own.class_of(&own.accounts[0]), Class::Preferred);
        assert_eq!(
            alab_roster::fleet_for(&own, Cli::Claude, None)
                .unwrap()
                .accounts
                .len(),
            1,
            "the same label collision must not suppress this machine's own preference"
        );
    }

    #[test]
    fn a_bound_registry_name_creates_the_same_local_mirror_the_cli_and_menu_read() {
        use aterm_agent::machine_name::{Origin, ensure_from_registry_for_test, file};
        use aterm_net::fleet_member::name_record::NickClass;
        use aterm_types::machine_name::{Class, IMPLIED_SINCE};
        for automatic in [false, true] {
            let view = naming_view(
                Some("m86"),
                MachineNickname::Named {
                    word: "MissBunny".into(),
                    automatic,
                },
            );
            let lab = harness_lab(&view, false, &BTreeMap::new());
            let expected = registry_name(&view).unwrap().unwrap();
            assert_eq!(lab.machine_name, Some(Ok(expected.clone())));
            let state = aterm_tempfile::tempdir().unwrap();
            aterm_uds::private_file::ensure_private_dir(state.path()).unwrap();
            let (name, origin) = ensure_from_registry_for_test(
                state.path(),
                || RegistryNameRead::Bound(lab.machine_name.unwrap()),
                || panic!("the bound registry row outranks the roster"),
                |_| panic!("the registry already supplies the nickname"),
                1_800_000_000_000,
            )
            .unwrap();
            assert_eq!(origin, Origin::Registry);
            assert_eq!(name.title(), "m86 MissBunny");
            assert_eq!(
                (name.class, name.since, name.rev, name.migration),
                (Class::Seeded, IMPLIED_SINCE, 0, None)
            );
            assert_eq!(
                name.nick_class,
                if automatic {
                    NickClass::Auto
                } else {
                    NickClass::Chosen
                }
            );
            assert_eq!(file::read(state.path()).unwrap(), Some(name));
        }
    }

    #[test]
    fn only_a_valid_canonical_bound_registry_name_can_create_a_mirror() {
        assert_eq!(registry_name(&LabView::default()), None);
        let zero = registry_name(&naming_view(Some("m0"), MachineNickname::Unset))
            .unwrap()
            .unwrap();
        assert_eq!(zero.num.get(), 0);
        let mut retired = naming_view(Some("m86"), MachineNickname::Unset);
        retired.machines.get_mut("pk-me").unwrap().live = false;
        assert_eq!(registry_name(&retired), None);
        for view in [
            naming_view(None, MachineNickname::Unset),
            naming_view(Some("m40-3fa2"), MachineNickname::Unset),
            naming_view(Some("M86"), MachineNickname::Unset),
            naming_view(Some("m86"), MachineNickname::Conflict),
            naming_view(
                Some("m86"),
                MachineNickname::Named {
                    word: "m7".into(),
                    automatic: false,
                },
            ),
        ] {
            assert!(registry_name(&view).unwrap().is_err(), "{view:?}");
        }
        assert!(
            registry_name(&naming_view(Some("m86"), MachineNickname::Unset))
                .unwrap()
                .unwrap()
                .nick
                .is_none()
        );
    }

    #[test]
    fn naming_policy_and_pending_status_are_read_from_one_retained_snapshot() {
        let root = aterm_tempfile::tempdir().unwrap();
        let config = root.path().join("aterm.toml");
        let mut held = nothing_held();
        look(&mut held, Ok(Some(naming(Some("m86")))), Instant::now());
        let cache = Cache {
            paths: Paths::for_home(root.path()),
            config: config.clone(),
            settled: None,
            held: Mutex::new(held),
            refreshing: Mutex::new(()),
            refresher: OnceLock::new(),
        };
        let expected = cache.projection().machine_name();
        assert!(matches!(&expected, RegistryNameRead::Bound(Ok(_))));
        look(
            &mut cache.held.lock().unwrap(),
            Err("temporarily unavailable".into()),
            Instant::now(),
        );
        assert_eq!(cache.projection().machine_name(), expected);
        aterm_uds::atomic_file::write(&config, b"[alab\n", 0o600).unwrap();
        assert_eq!(cache.projection().machine_name(), RegistryNameRead::Pending);
        aterm_uds::atomic_file::write(&config, b"[alab]\nroster = false\n", 0o600).unwrap();
        assert_eq!(cache.projection().machine_name(), RegistryNameRead::Unbound);
        std::fs::remove_file(&config).unwrap();
        assert_eq!(cache.projection().machine_name(), expected);
        *cache.held.lock().unwrap() = nothing_held();
        assert_eq!(cache.projection().machine_name(), RegistryNameRead::Pending);
        cache.held.lock().unwrap().pending = false;
        assert_eq!(cache.projection().machine_name(), RegistryNameRead::Unbound);
        for read in [
            Err("first read unavailable".into()),
            Err("source history incomplete".into()),
        ] {
            look(&mut cache.held.lock().unwrap(), read, Instant::now());
            assert_eq!(
                cache.projection().machine_name(),
                RegistryNameRead::Pending,
                "an unread first registry is not proof that no binding exists"
            );
            let held = cache.held.lock().unwrap();
            assert!(held.pending);
            assert!(due(
                held.looked,
                held.pending,
                Instant::now() + RETRY_FAILED
            ));
        }
    }

    #[test]
    fn an_unread_registry_keeps_both_consumers_pending_without_a_history_marker() {
        let root = aterm_tempfile::tempdir().unwrap();
        let marker_dir = root.path().join("alab");
        assert!(!marker_dir.exists());
        for read in [
            Err("registry is temporarily unreadable".to_owned()),
            Err("source history incomplete".to_owned()),
        ] {
            let mut held = nothing_held();
            look(&mut held, read, Instant::now());
            let cache = Cache {
                paths: Paths::for_home(root.path()),
                config: root.path().join("aterm.toml"),
                settled: None,
                held: Mutex::new(held),
                refreshing: Mutex::new(()),
                refresher: OnceLock::new(),
            };
            let observed = cache.projection();
            assert_eq!(observed.machine_name(), RegistryNameRead::Pending);
            assert!(
                observed.pending() && !observed.permits_sheet(),
                "an unread registry must not enable legacy account discovery"
            );
        }
    }

    /// Failed and partial reads preserve the exact projection and its source. Only a
    /// successful complete read replaces the pair, and successful absence clears it.
    #[test]
    fn a_look_preserves_a_projection_and_keeps_unknown_absence_pending() {
        let now = Instant::now();
        let failed = || Err::<Option<Projected<Inventory>>, String>("custody unreadable".into());
        for (read, what) in [
            (failed(), "a failed read"),
            (Err("source history incomplete".into()), "a partial history"),
        ] {
            let mut held = nothing_held();
            look(&mut held, read, now);
            assert!(held.pending, "{what}");
            assert!(held.projected.is_none(), "{what}");
            assert_eq!(held.looked, Some(now), "{what}");
        }

        let mut held = nothing_held();
        look(&mut held, Ok(Some(naming(Some("m3")))), now);
        let lab = held.lab().cloned().expect("the projection is held");
        assert_eq!(
            lab.me.as_ref().and_then(|me| me.label.as_deref()),
            Some("m3")
        );
        assert_eq!(lab.me.as_ref().map(|me| me.key.as_str()), Some("pk-me"));
        assert!(!lab.settled && !held.pending);
        assert_eq!(held.projected.as_ref().unwrap().source, Inventory([7; 32]));
        for (read, what) in [
            (Err("source history incomplete".into()), "a partial history"),
            (failed(), "a failed read"),
        ] {
            look(&mut held, read, now);
            assert!(
                held.lab().is_some_and(|kept| Arc::ptr_eq(kept, &lab)),
                "{what} keeps the lab"
            );
            assert!(!held.pending, "{what}: a held lab is not pending");
            assert_eq!(
                held.projected.as_ref().unwrap().source,
                Inventory([7; 32]),
                "{what}"
            );
        }
        let mut next = naming(Some("m3"));
        Arc::make_mut(&mut next.lab).settled = true;
        next.source = Inventory([8; 32]);
        look(&mut held, Ok(Some(next)), now);
        assert!(
            held.lab().is_some_and(|lab| lab.settled),
            "a complete read installs local overlays with the new source"
        );
        assert_eq!(held.projected.as_ref().unwrap().source, Inventory([8; 32]));
        assert!(!Arc::ptr_eq(held.lab().unwrap(), &lab));
        look(&mut held, Ok(None), now);
        assert!(
            held.projected.is_none(),
            "absence clears the projection and its source together"
        );
        assert!(!held.pending, "a successful absence resolves uncertainty");
    }

    #[test]
    fn current_commons_policy_hides_a_source_and_preserves_its_history() {
        let root = aterm_tempfile::tempdir().unwrap();
        let cache = Cache {
            paths: Paths::for_home(root.path()),
            config: root.path().join("aterm.toml"),
            settled: None,
            held: Mutex::new(nothing_held()),
            refreshing: Mutex::new(()),
            refresher: OnceLock::new(),
        };
        cache.refresh_with(|| Ok(Some(naming(Some("m3")))));
        let original = cache.capture().1.unwrap();
        let exclusion = |scope: [u8; 32]| {
            format!(
                "[commons]\nexclude_tables = [\"{}\"]\n",
                aterm_codec::hex::encode_lower(&scope)
            )
        };
        for (text, pending, scan) in [
            ("[commons]\nenabled = false\n".to_owned(), false, false),
            ("[swarm]\nenabled = false\n".to_owned(), false, false),
            (exclusion([0; 32]), false, true),
            (exclusion([7; 32]), false, true),
            ("[commons]\nenabled = 'yes'\n".to_owned(), true, false),
            (
                "[commons]\nexclude_tables = ['invalid']\n".to_owned(),
                true,
                false,
            ),
        ] {
            aterm_uds::atomic_file::write(&cache.config, text.as_bytes(), 0o600).unwrap();
            let (snapshot, projected) = cache.capture();
            assert!(projected.is_none());
            assert_eq!(snapshot.pending(), pending);
            assert_eq!(snapshot.disabled(), !pending);
            let called = std::cell::Cell::new(false);
            cache.refresh_with(|| {
                called.set(true);
                Ok(None)
            });
            assert_eq!(
                called.get(),
                scan,
                "only an enabled scope scan may discover an included successor"
            );
            let held = cache.held.lock().unwrap();
            assert!(Arc::ptr_eq(held.lab().unwrap(), &original.lab));
            assert_eq!(held.projected.as_ref().unwrap().source, original.source);
        }
        aterm_uds::atomic_file::write(&cache.config, b"", 0o600).unwrap();
        cache.refresh_with(|| {
            aterm_uds::atomic_file::write(&cache.config, exclusion([7; 32]).as_bytes(), 0o600)
                .unwrap();
            Ok(None)
        });
        assert!(cache.capture().0.disabled());
        assert!(Arc::ptr_eq(
            cache.held.lock().unwrap().lab().unwrap(),
            &original.lab
        ));

        // A different table can become excluded while an older held table is included.
        aterm_uds::atomic_file::write(&cache.config, exclusion([9; 32]).as_bytes(), 0o600).unwrap();
        cache.refresh_with(|| {
            let mut next = naming(Some("m4"));
            next.source = Inventory([9; 32]);
            Ok(Some(next))
        });
        let kept = cache.capture().1.unwrap();
        assert!(Arc::ptr_eq(&kept.lab, &original.lab));
        assert_eq!(kept.source, original.source);
        aterm_uds::atomic_file::write(&cache.config, exclusion([7; 32]).as_bytes(), 0o600).unwrap();
        cache.refresh_with(|| {
            let mut next = naming(Some("m4"));
            next.source = Inventory([9; 32]);
            Ok(Some(next))
        });
        assert_eq!(
            cache.capture().1.unwrap().source,
            Inventory([9; 32]),
            "an excluded old generation must not freeze a permitted new one"
        );
    }

    #[test]
    fn native_cache_projects_mail_and_settlement_from_its_captured_source() {
        use aterm_db::alab::registry::{ADDRESS, MAILBOX};
        use aterm_db::typed::{cell, keys};
        use aterm_json::json;
        use aterm_secrets::commons::roster::Role;

        let lab = aterm_db::table::fixture::Lab::new(&[Role::Writer]);
        let paths = &lab.paths[0];
        let registry = Registry::at(paths.clone());
        let table = registry.open().unwrap();
        let login = keys::address_pk("owner@example.test").unwrap();
        let alias = keys::address_pk("ai@example.test").unwrap();
        let mailbox = keys::mailbox_pk(&login).unwrap();
        let changes = [
            (
                &ADDRESS,
                login.as_str(),
                "addr",
                json!("owner@example.test"),
            ),
            (&ADDRESS, alias.as_str(), "addr", json!("ai@example.test")),
            (&ADDRESS, alias.as_str(), "mailbox", json!(mailbox)),
            (&MAILBOX, mailbox.as_str(), "login", json!(login)),
        ]
        .into_iter()
        .map(|(collection, pk, col, value)| {
            cell::change(
                collection,
                cell::CellKey {
                    pk,
                    col,
                    elem: None,
                },
                &cell::CellValue::owner(1, value),
            )
            .unwrap()
        })
        .collect();
        registry.write(vec![changes]).unwrap();
        let config = paths.home.join("aterm.toml");
        let source = registry.observe_source(&config).unwrap().unwrap();
        let settled = paths.home.join("settled-observation");
        aterm_types::fs_restricted::ensure_private_dir(&settled).unwrap();
        aterm_uds::atomic_file::write(
            &settled.join("settled"),
            aterm_codec::hex::encode_lower(&table.digest()).as_bytes(),
            0o600,
        )
        .unwrap();
        let cache = Cache {
            paths: paths.clone(),
            config,
            settled: Some(settled.clone()),
            held: Mutex::new(Held {
                projected: None,
                looked: None,
                pending: true,
            }),
            refreshing: Mutex::new(()),
            refresher: OnceLock::new(),
        };
        aterm_uds::atomic_file::write(
            &cache.config,
            format!(
                "[commons]\nexclude_tables = [\"{}\"]\n",
                aterm_codec::hex::encode_lower(&table.digest())
            )
            .as_bytes(),
            0o600,
        )
        .unwrap();
        cache.refresh();
        assert!(
            cache.capture().0.pending(),
            "a cold excluded registry must not become proof of absence"
        );
        aterm_uds::atomic_file::write(&cache.config, b"", 0o600).unwrap();
        cache.refresh();
        let (snapshot, projected) = cache.capture();
        let projected = projected.unwrap();
        assert!(Arc::ptr_eq(&snapshot.lab().unwrap(), &projected.lab));
        assert_eq!(projected.source.revision(), source.revision());
        assert_eq!(projected.source.mailboxes()[0].pk, mailbox);
        assert_eq!(
            projected.lab.mailboxes["ai@example.test"],
            "owner@example.test"
        );
        assert!(projected.lab.settled);
        aterm_uds::atomic_file::write(
            &settled.join("settled"),
            aterm_codec::hex::encode_lower(&[0; 32]).as_bytes(),
            0o600,
        )
        .unwrap();
        cache.refresh();
        assert!(
            !cache.capture().1.unwrap().lab.settled,
            "a different table's settlement cannot attach to this source"
        );
    }

    #[test]
    fn native_receipt_guard_rechecks_history_policy_and_overlays_before_account_save() {
        use aterm_db::typed::cell;
        use aterm_secrets::commons::roster::Role;

        for change in 0..5 {
            let fixture = aterm_db::table::fixture::Lab::new(&[Role::Writer]);
            let paths = &fixture.paths[0];
            let registry = Registry::at(paths.clone());
            let table = registry.open().unwrap();
            let config = paths.home.join("aterm.toml");
            aterm_uds::atomic_file::write(&config, b"", 0o600).unwrap();
            let settled = paths.home.join("settlement");
            aterm_uds::private_file::create_private_dir(&settled).unwrap();
            let write_settlement = |digest: &[u8]| {
                aterm_uds::atomic_file::write(
                    &settled.join("settled"),
                    aterm_codec::hex::encode_lower(digest).as_bytes(),
                    0o600,
                )
                .unwrap()
            };
            write_settlement(&table.digest());
            let cache: &'static Cache = Box::leak(Box::new(Cache {
                paths: paths.clone(),
                config: config.clone(),
                settled: Some(settled.clone()),
                held: Mutex::new(Held {
                    projected: None,
                    looked: None,
                    pending: true,
                }),
                refreshing: Mutex::new(()),
                refresher: OnceLock::new(),
            }));
            cache.refresh();
            let captured = alab_roster::Source::capture(&cache);
            let source = captured.native_guard().unwrap();
            let old_lab = captured.snapshot().lab().unwrap();
            match change {
                1 | 4 => {
                    registry
                        .write(vec![vec![
                            cell::change(
                                &aterm_db::alab::registry::MODEL,
                                cell::CellKey {
                                    pk: "invisible-model",
                                    col: "row",
                                    elem: None,
                                },
                                &cell::CellValue::automatic(1, aterm_json::json!("live")),
                            )
                            .unwrap(),
                        ]])
                        .unwrap();
                    assert_eq!(
                        cache.lab_for_source(&registry.observe_source(&config).unwrap().unwrap()),
                        old_lab
                    );
                    if change == 4 {
                        cache.refresh();
                    }
                }
                2 => aterm_uds::atomic_file::write(&config, b"[alab]\nroster = false\n", 0o600)
                    .unwrap(),
                3 => write_settlement(&[0; 32]),
                _ => {}
            }
            let store = aterm_agent::harness::accounts::Store::at(&paths.home.join("account-test"));
            let roster = paths.home.join("accounts.toml");
            let result =
                alab_roster::test_publish_source_roster(&store, &roster, "# admitted\n", source);
            assert_eq!(result.is_ok(), change == 0, "change={change}: {result:?}");
            assert_eq!(roster.exists(), change == 0);
            assert_eq!(store.dir.join("sync-commit.json").exists(), change == 0);
            assert_eq!(
                cache.held.lock().unwrap().looked.is_some(),
                change == 0 || change == 4,
                "only the rejected cached revision is invalidated"
            );
            if change == 0 {
                let caller = paths.home.join("caller.toml");
                let excluded = format!(
                    "[commons]\nexclude_tables = [\"{}\"]\n",
                    aterm_codec::hex::encode_lower(&table.digest())
                );
                for policy in [
                    "[alab]\nroster = false\n",
                    "[commons]\nenabled = false\n",
                    excluded.as_str(),
                ] {
                    aterm_uds::atomic_file::write(&caller, policy.as_bytes(), 0o600).unwrap();
                    let blocked = alab_roster::Source::capture_for(&cache, &caller);
                    assert!(blocked.snapshot().disabled());
                    assert!(blocked.native_guard().is_err());
                }
                aterm_uds::atomic_file::write(&caller, b"", 0o600).unwrap();
                let allowed = alab_roster::Source::capture_for(&cache, &caller);
                aterm_uds::atomic_file::write(&caller, excluded.as_bytes(), 0o600).unwrap();
                assert!(
                    alab_roster::test_publish_source_roster(
                        &store,
                        &roster,
                        "# refused\n",
                        allowed.native_guard().unwrap()
                    )
                    .is_err()
                );
                assert_eq!(std::fs::read_to_string(&roster).unwrap(), "# admitted\n");
            }
        }
    }

    #[test]
    fn caller_policy_is_observed_even_without_a_host_projection() {
        let root = aterm_tempfile::tempdir().unwrap();
        let config = root.path().join("aterm.toml");
        let caller = root.path().join("caller.toml");
        let cache: &'static Cache = Box::leak(Box::new(Cache {
            paths: Paths::for_home(root.path()),
            config,
            settled: None,
            held: Mutex::new(Held {
                projected: None,
                looked: Some(Instant::now()),
                pending: false,
            }),
            refreshing: Mutex::new(()),
            refresher: OnceLock::new(),
        }));
        aterm_uds::atomic_file::write(&caller, b"[alab]\nroster=false\n", 0o600).unwrap();
        assert!(
            alab_roster::Source::capture_for(&cache, &caller)
                .snapshot()
                .disabled()
        );
        aterm_uds::atomic_file::write(&caller, b"[alab", 0o600).unwrap();
        assert!(
            alab_roster::Source::capture_for(&cache, &caller)
                .snapshot()
                .pending()
        );
    }

    #[test]
    fn equal_visible_labs_keep_the_source_of_each_complete_refresh() {
        let root = aterm_tempfile::tempdir().unwrap();
        let cache = Cache {
            paths: Paths::for_home(root.path()),
            config: root.path().join("aterm.toml"),
            settled: None,
            held: Mutex::new(nothing_held()),
            refreshing: Mutex::new(()),
            refresher: OnceLock::new(),
        };
        cache.refresh_with(|| Ok(Some(naming(Some("m3")))));
        let (snapshot, captured) = cache.capture();
        let captured = captured.unwrap();
        assert!(Arc::ptr_eq(&snapshot.lab().unwrap(), &captured.lab));
        let mut later = naming(Some("m3"));
        later.source = Inventory([9; 32]);
        cache.refresh_with(|| Ok(Some(later)));
        let current = cache.capture().1.unwrap();
        assert_eq!(captured.lab, current.lab);
        assert_ne!(captured.source, current.source);
        assert_eq!(captured.source, Inventory([7; 32]));
        cache.refresh_with(|| Err("temporary native source failure".into()));
        let kept = cache.capture().1.unwrap();
        assert!(Arc::ptr_eq(&kept.lab, &current.lab));
        assert_eq!(kept.source, current.source);
        aterm_uds::atomic_file::write(&cache.config, b"[alab]\nroster = false\n", 0o600).unwrap();
        let (disabled, hidden) = cache.capture();
        assert!(disabled.disabled() && hidden.is_none());
        aterm_uds::atomic_file::write(&cache.config, b"[alab\n", 0o600).unwrap();
        let (pending, hidden) = cache.capture();
        assert!(pending.pending() && hidden.is_none());
    }

    /// A caller looks at the first look and, while a failed look left the source pending, again
    /// once 15 seconds passed; otherwise the refresher looks.
    /// NEGATIVE CONTROL: with `failed` ignored, `` the refresher looks while a lab is held ``
    /// fails; with `>` for `>=`, `` 15 s after a pending look `` fails.
    #[test]
    fn a_caller_looks_first_and_again_only_15_s_after_a_pending_look() {
        let at = Instant::now();
        let after = |secs| at + Duration::from_secs(secs);
        assert!(
            !due(Some(at), false, after(3600)),
            "the refresher looks while a lab is held"
        );
        assert!(due(None, false, at), "the first look");
        assert!(due(None, true, at), "the first look");
        assert!(!due(Some(at), true, after(14)), "14 s after a pending look");
        assert!(due(Some(at), true, after(15)), "15 s after a pending look");
        assert!(!due(Some(after(1)), true, at), "a look stamped after now");
    }

    /// A completed round settles a table only where its witness counts an operation of one of
    /// the registry's generations, not settled yet, that the registry holds here, and the held
    /// table, a read of custody and the catalog, is read only for such a witness.
    /// NEGATIVE CONTROL: without the `count > 0` filter, `` an empty witness `` settles the held
    /// table; without the `is_generation` filter, `` another application `` reads the held table.
    #[test]
    fn a_round_settles_only_a_witness_that_counts_an_operation_of_the_table_held_here() {
        use aterm_secrets::commons::journal::Trust;
        let trust = Trust {
            network: [3; 16],
            authority: [4; 32],
            genesis: [5; 32],
        };
        let registry =
            |compiled: &aterm_db::att::Compiled| compiled.descriptor(trust).unwrap().scope;
        let held = registry(&aterm_db::att::REGISTRY);
        let next = registry(&aterm_db::att::REGISTRY_2);
        let other = aterm_db::catalog::Descriptor::application(trust, "aterm.lab", "tables", 1)
            .unwrap()
            .scope;
        let unread = || -> Option<[u8; 32]> { panic!("the held table is read only for a witness") };
        let unsettled = |_: &[u8; 32]| false;
        assert_eq!(
            witness_table(Some((0, held)), unsettled, || Some(held.digest())),
            None,
            "an empty witness"
        );
        assert_eq!(
            witness_table(Some((3, next)), unsettled, || Some(held.digest())),
            None,
            "another generation"
        );
        assert_eq!(
            witness_table(Some((3, held)), unsettled, || None),
            None,
            "no table held here"
        );
        assert_eq!(witness_table(None, unsettled, unread), None, "no witness");
        assert_eq!(
            witness_table(Some((0, held)), unsettled, unread),
            None,
            "an empty witness"
        );
        assert_eq!(
            witness_table(Some((3, other)), unsettled, unread),
            None,
            "another application"
        );
        assert_eq!(
            witness_table(Some((3, held)), |table| *table == held.digest(), unread),
            None,
            "settled already"
        );
        assert_eq!(
            witness_table(Some((3, held)), unsettled, || Some(held.digest())),
            Some(held.digest())
        );
    }

    /// A shipped build holds a lab only in the person's own environment: a build tool's run, a
    /// scratch state root and CI each hold it off, a variable that only resembles one does not,
    /// and a debug build holds none whatever its environment.
    /// NEGATIVE CONTROL: with the `ATERM_STATE_HOME` clause dropped, the scratch-root row holds a
    /// lab: `` a release build with ATERM_STATE_HOME set ``.
    #[test]
    fn a_shipped_build_holds_a_lab_only_in_the_persons_own_environment() {
        let env = |set: &'static [&'static str]| {
            move |key: &str| set.contains(&key).then(|| std::ffi::OsString::from("1"))
        };
        const LOOKALIKES: &[&str] = &[
            "XDG_STATE_HOME",
            "ATERM_STATE_HOME_X",
            "CIRCLECI",
            "CARGO_HOME",
        ];
        assert!(
            lab_hooks_on(true, env(LOOKALIKES)),
            "only lookalikes are set"
        );
        assert!(
            !lab_hooks_on(true, env(&["XDG_STATE_HOME", "CARGO_MANIFEST_DIR"])),
            "a release build with CARGO_MANIFEST_DIR set"
        );
        assert!(
            !lab_hooks_on(true, env(&["ATERM_STATE_HOME_X", "ATERM_STATE_HOME"])),
            "a release build with ATERM_STATE_HOME set"
        );
        assert!(
            !lab_hooks_on(true, env(&["CIRCLECI", "CI"])),
            "a release build with CI set"
        );
        assert!(!lab_hooks_on(false, env(&[])), "a debug build");
    }

    /// Only admitted `[alab] roster = false` is an explicit Off switch.
    #[test]
    fn the_roster_source_is_on_unless_turned_off() {
        let dir = aterm_tempfile::TempDir::new().unwrap();
        let config = dir.path().join("aterm.toml");
        assert!(matches!(roster_policy(&config), LocalRosterPolicy::Enabled));
        aterm_uds::atomic_file::write(&config, b"[alab]\nroster = true\n", 0o600).unwrap();
        assert!(matches!(roster_policy(&config), LocalRosterPolicy::Enabled));
        std::fs::write(&config, "[alab]\nroster = false\n").unwrap();
        assert!(matches!(
            roster_policy(&config),
            LocalRosterPolicy::Disabled
        ));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o666)).unwrap();
            assert!(
                matches!(roster_policy(&config), LocalRosterPolicy::Unavailable),
                "world-writable aterm.toml must not be trusted"
            );
        }
    }

    /// The lab's placements are named from the mailbox logins of each look: an address whose
    /// mailbox a look names gets that login, and one whose mailbox it does not name gets none,
    /// so a mailbox added to `ai-mailbox.toml` is seen with the registry unchanged.
    /// NEGATIVE CONTROL: naming every placement from the first look's logins leaves the hub's
    /// address unnamed after the hub is added (`seen once added`).
    #[test]
    fn placements_are_named_from_each_looks_logins() {
        let placed: BTreeMap<String, String> = [
            ("a@example.test".to_owned(), "mb-hub".to_owned()),
            ("b@example.test".to_owned(), "mb-side".to_owned()),
        ]
        .into();
        let side: BTreeMap<String, String> =
            [("mb-side".to_owned(), "side@example.test".to_owned())].into();
        assert_eq!(
            named_mailboxes(&placed, &side),
            [("b@example.test".to_owned(), "side@example.test".to_owned())].into()
        );
        let mut both = side.clone();
        both.insert("mb-hub".into(), "hub@example.test".into());
        assert_eq!(
            named_mailboxes(&placed, &both)
                .get("a@example.test")
                .map(String::as_str),
            Some("hub@example.test"),
            "seen once added"
        );
    }

    /// `settled_in` admits only an owner-private `settled` witness inside a `0700` `alab`
    /// directory and refuses symlinks or group/other-readable files.
    /// NEGATIVE CONTROL: `std::fs::read_to_string` accepted a `0644` file and a symlink target.
    #[test]
    fn settled_witness_must_be_owner_private() {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = aterm_tempfile::TempDir::new().unwrap();
        let alab_dir = temp.path().join("alab");
        aterm_types::fs_restricted::ensure_private_dir(&alab_dir).unwrap();
        let table = [0x5a_u8; 32];
        let hex = aterm_codec::hex::encode_lower(&table);
        let settled_path = alab_dir.join("settled");
        aterm_agent::harness::atomic_write::write_atomic_mode(&settled_path, &hex, Some(0o600))
            .unwrap();
        assert!(settled_in(&alab_dir, &table));
        assert!(!settled_in(&alab_dir, &[0x5b_u8; 32]));

        std::fs::set_permissions(&settled_path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!settled_in(&alab_dir, &table));

        std::fs::remove_file(&settled_path).unwrap();
        let outside = temp.path().join("outside");
        aterm_agent::harness::atomic_write::write_atomic_mode(&outside, &hex, Some(0o600)).unwrap();
        std::os::unix::fs::symlink(&outside, &settled_path).unwrap();
        assert!(!settled_in(&alab_dir, &table));
    }
}

#[cfg(test)]
#[path = "alab_preference_model_tests.rs"]
mod preference_model_tests;

#[cfg(test)]
#[path = "alab_source_model_tests.rs"]
mod source_model_tests;
