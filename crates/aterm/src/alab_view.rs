// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! LabView's cache (`docs/DESIGN-alab-lab-model-2026-11-01.md` §7, step 11): the harness's
//! `aterm_agent::harness::alab_roster::Source`, filled from
//! `aterm_db::alab::Registry::observe_installed_projection`.
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
//! Native receipt admission retains both physical replicas and the local vault observation.
//! An installation that never configured a Commons is absent, read at once. Inside an
//! established vault a missing projection stays pending: the catalog's separate local-absence
//! capability is not yet a receipt capability here, and cannot authorize a fallback through
//! this cache.
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
    ProjectionRead, Registry, SourceRead,
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
/// [`crate::alab_register`], [`crate::alab_seen`] and [`crate::alab_update`] hold on this
/// answer; the journal sink
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

struct Cache<S = ProjectionRead> {
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

impl CachedSource for ProjectionRead {
    fn included_by(&self, policy: &Policy) -> bool {
        self.source().included_by(policy)
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
                .observe_installed_projection(&self.config)
                .map_err(|why| why.to_string())?
            else {
                return Ok(None);
            };
            let lab = self.lab_for_source(source.source());
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
    source: ProjectionRead,
    /// This callback saves only local metadata. Its vault is the native
    /// projection's own context, never an inferred credential candidate root.
    vault_config: aterm_secrets::client::Config,
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
                && self.vault_config == other.vault_config
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
        let result = Registry::at(self.cache.paths.clone()).with_admitted_projection(
            &self.cache.config,
            &self.cache.paths,
            &self.vault_config,
            Some(self.source.revision()),
            |projection, _reader| {
                // Metadata persistence does not read credentials. A future
                // credential decision must receive this borrowed reader and
                // compare its complete candidate set before saving an intent.
                let current = projection.source();
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
        let Ok(Some(vault_config)) = aterm_secrets::client::config(&self.paths) else {
            return alab_roster::SourceCapture::observed(SourceSnapshot::Pending);
        };
        alab_roster::SourceCapture::guarded(
            snapshot,
            alab_roster::ReceiptGuard::new(NativeReceiptGuard {
                cache: self,
                source: projected.source,
                vault_config,
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
            .map(|projected| lab_mail(projected.source.source().mailboxes()))
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
#[path = "alab_view_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "alab_preference_model_tests.rs"]
mod preference_model_tests;

#[cfg(test)]
#[path = "alab_source_model_tests.rs"]
mod source_model_tests;
