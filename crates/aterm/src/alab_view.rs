// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! LabView's cache (`docs/DESIGN-alab-lab-model-2026-11-01.md` §7, step 11): the harness's
//! `aterm_agent::harness::alab_roster::Source`, served from this Mac's shared files
//! (`docs/DESIGN-shared-files-2026-11-11.md` §4 and §11, SF-04).
//!
//! The lab is read from the snapshots of `ai/accounts`, `ai/links` and `lab/machines`
//! (`aterm_file::File::read`, through `aterm_db::alab::view::LabView::of_files`): local files
//! that answer in every custody state, so the lab is served across a restart while the registry
//! refuses every strict reader (a pending writer succession, ATERM-KI-1109-1), and a held
//! snapshot is served as it is. A read opens no custody, takes no vault and calls no provider;
//! each caller reads the snapshots' status files, and the snapshots themselves only once a pass
//! moved them. No thread looks at the registry on a timer: the materializer
//! (`aterm_db::file::materialize`) moves the snapshots after the registry grows. With no
//! snapshot here the source is absent only on an installation that never configured a Commons,
//! and pending otherwise, so neither naming nor account discovery reads uncertainty as an
//! absent registry binding; a file whose last pass says `off` (`[commons]` keeps the registry
//! from this Mac) is no source either.
//!
//! A preference orders a pool and never limits it (`link_never_limits`): the harness's sync
//! keeps every row this Mac holds whatever the lab says (`alab_roster::retain`), so an empty,
//! stale, held or missing snapshot removes nothing and signs nothing out.
//!
//! What no shared file carries is still read from the registry, strictly, as a sign-in's
//! mailbox always was (§10): the mail metadata (mailboxes, where an address's codes arrive, the
//! alias links), whose collections hold `Secret` columns (LT-04), and the registry table a
//! peer's round settled here. It is read when the snapshots' records move, and again 15
//! seconds after a refusal; a refusal leaves the mail routes empty and the lab unsettled, and
//! never withholds the lab. Writers stay strict: a receipt decided from the served lab is saved
//! only under the registry's admitted projection, and only where that projection makes the same
//! lab (`NativeReceiptGuard`). Reveals read the registry as they always did.
//!
//! Syncs, the accounts list, mail routes and first-name creation ask, never a typing path.
//! `[alab] roster = false` in `aterm.toml` turns the source off on this Mac. A test, a debug
//! build and a build tool's run hold no lab (inert, as the other lab hooks are), so no test
//! reads this Mac's registry. Every getter admits current local policy before exposing a held
//! projection. Unavailable policy hides the projection and blocks automatic fallback without
//! erasing its history.
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
use aterm_file::{File, Word};
use aterm_secrets::client::Paths;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

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
    /// No snapshot or successful absence has been observed, or an unreadable read left no
    /// projection to use.
    pending: bool,
}

impl<S> Held<S> {
    #[cfg(test)]
    fn lab(&self) -> Option<&Arc<Lab>> {
        self.projected.as_ref().map(|projected| &projected.lab)
    }
}

/// One read's change to what is held. Only a snapshot or a successful absence resolves a first
/// read. Uncertainty retains an existing projection.
fn look<S>(held: &mut Held<S>, read: Result<Option<Projected<S>>, String>) {
    held.pending = read.is_err() && held.projected.is_none();
    match read {
        Ok(projected) => held.projected = projected,
        Err(why) => {
            aterm_log::debug!("lab view: {why}");
        }
    }
}

/// The least time between two reads of the registry's mail metadata after one was refused.
const RETRY_MAIL: Duration = Duration::from_secs(15);

/// What one shared file's status says of its snapshot: its frontier's fingerprint, its revision
/// and the last pass's word. Equal marks mean the same snapshot of the same records.
type Mark = Option<(String, u64, Option<Word>)>;

/// What a held lab was read from: the marks of `ai/accounts`, `ai/links` and `lab/machines`,
/// and the registry read its mail metadata came from, where one answered.
#[derive(Clone)]
struct Seen {
    files: [Mark; 3],
    mail: Option<ProjectionRead>,
}

/// The three shared files the lab is read from, in [`Seen::files`]' order.
const FILES: [&aterm_db::file::Type; 3] = [
    &aterm_db::file::ACCOUNTS,
    &aterm_db::file::LINKS,
    &aterm_db::file::MACHINES,
];

struct Cache<S = Seen> {
    paths: Paths,
    config: PathBuf,
    settled: Option<PathBuf>,
    held: Mutex<Held<S>>,
    /// Keep an older in-flight read from replacing a newer projection.
    refreshing: Mutex<()>,
    /// When the registry last refused a read of its mail metadata.
    mail_refused: Mutex<Option<Instant>>,
}

/// A source's immutable scope identity, without opening its native storage.
trait CachedSource: Clone {
    fn included_by(&self, policy: &Policy) -> bool;
}

/// The snapshots answer whatever `[commons]` says (a pass under an excluding policy records
/// `off`, which a read takes as no source); the registry read the mail metadata came from is
/// held to the current policy as a native source is.
impl CachedSource for Seen {
    fn included_by(&self, policy: &Policy) -> bool {
        self.mail
            .as_ref()
            .is_none_or(|mail| mail.source().included_by(policy))
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
        look(&mut held, read);
    }
}

/// The parts of `lab` a native receipt is decided from: everything but the mail routes, which
/// no receipt reads.
fn decided(lab: &Lab) -> Lab {
    Lab {
        aliases: BTreeMap::new(),
        mailboxes: BTreeMap::new(),
        places: Placements::default(),
        ..lab.clone()
    }
}

impl Cache {
    /// Whether `table` is the registry table a peer's completed round settled here.
    fn settled_for(&self, table: &[u8; 32]) -> bool {
        self.settled
            .as_ref()
            .is_some_and(|dir| settled_in(dir, table))
    }

    /// The lab the shared files' `view` and the registry's mail metadata (`mail`, where a read
    /// answered) say: accounts, machines, links and this Mac's binding from the snapshot, and the
    /// mail routes and the settled witness, which no shared file carries, from the registry.
    fn lab_of(&self, view: &LabView, mail: Option<&SourceRead>) -> Lab {
        let Some(source) = mail else {
            return harness_lab_with_placed(view, &BTreeMap::new(), false, &BTreeMap::new());
        };
        let held = lab_mail(source.mailboxes());
        let toml = mailbox_logins(source.addresses(), &mailbox_names(&self.paths));
        let logins = logins_of(&held, &toml);
        let placed = if logins.is_empty() {
            BTreeMap::new()
        } else {
            source.view().mailboxes()
        };
        let mut lab =
            harness_lab_with_placed(view, &placed, self.settled_for(&source.table()), &logins);
        lab.aliases = source.view().aliases();
        lab
    }

    /// The lab as the registry projects it now (`source`): what a receipt's admission compares
    /// the captured lab with.
    fn lab_for_source(&self, source: &SourceRead) -> Lab {
        harness_lab(
            source.view(),
            self.settled_for(&source.table()),
            &BTreeMap::new(),
        )
    }

    /// Whether a read of the registry's mail metadata is due though the snapshot did not move:
    /// [`RETRY_MAIL`] after the last one was refused.
    fn mail_due(&self) -> bool {
        self.mail_refused
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none_or(|at| at.elapsed() >= RETRY_MAIL)
    }

    /// Read the registry's mail metadata strictly, as a sign-in's mailbox has always been read
    /// (docs/DESIGN-shared-files-2026-11-11.md §10): its collections hold `Secret` columns, so
    /// no shared file carries them yet (LT-04). `None` on an installation with no Commons, and
    /// where the registry refuses, which is noted for [`Self::mail_due`].
    fn read_mail(&self) -> Option<ProjectionRead> {
        let read = Registry::at(self.paths.clone()).observe_installed_projection(&self.config);
        *self
            .mail_refused
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = read.is_err().then(Instant::now);
        match read {
            Ok(mail) => mail,
            Err(why) => {
                aterm_log::debug!("lab view: the registry's mail metadata: {why}");
                None
            }
        }
    }

    /// Read the lab from this Mac's shared files: no custody, no vault and no registry
    /// session, so it answers while every strict reader refuses (a pending writer succession,
    /// ATERM-KI-1109-1). A snapshot answers whatever word its last pass recorded (`live` or
    /// `held`); with none here the source is absent only on an installation that never
    /// configured a Commons, and pending otherwise. A file whose last pass says `off` is no
    /// source: `[commons]` keeps the registry from this Mac.
    fn read(&self) -> Result<Option<Projected<Seen>>, String> {
        let root = aterm_db::file::root(&self.paths);
        let files = FILES.map(|file| File::at(&root, &file.file()));
        let marks: [Mark; 3] = files.each_ref().map(|file| {
            file.state()
                .map(|state| (state.fingerprint, state.rev, state.word))
        });
        if marks
            .iter()
            .flatten()
            .any(|(_, _, word)| *word == Some(Word::Off))
        {
            return Err("[commons] keeps the registry's shared files off on this Mac".into());
        }
        let held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .projected
            .clone()
            .filter(|held| held.source.files == marks);
        // A settlement recorded since the lab was read rebuilds it from the same reads.
        let settled_moved = |held: &Projected<Seen>| {
            held.source
                .mail
                .as_ref()
                .is_some_and(|mail| self.settled_for(&mail.source().table()) != held.lab.settled)
        };
        // Nothing moved: the held lab stands, unless its mail metadata is still to be read.
        if let Some(held) = &held
            && !settled_moved(held)
            && (held.source.mail.is_some() || !self.mail_due())
        {
            return Ok(Some(held.clone()));
        }
        let [accounts, links, machines] = files.map(|file| file.read());
        if [&accounts, &links, &machines]
            .iter()
            .any(|read| read.snapshot.is_none())
        {
            return match Registry::at(self.paths.clone()).never_configured() {
                Ok(true) => Ok(None),
                Ok(false) => Err("no snapshot of the registry's shared files is here yet".into()),
                Err(why) => Err(why.to_string()),
            };
        }
        let mail = match held.and_then(|held| held.source.mail) {
            Some(mail) => Some(mail),
            None => self.read_mail(),
        };
        let view = LabView::of_files(&accounts, &links, &machines);
        let lab = self.lab_of(&view, mail.as_ref().map(ProjectionRead::source));
        Ok(Some(Projected {
            lab: Arc::new(lab),
            source: Seen { files: marks, mail },
        }))
    }

    fn refresh(&self) {
        self.refresh_with(|| self.read());
    }
}

/// A receipt decided from the lab the shared files serve is saved only where the registry,
/// read strictly under admission, projects that same lab now: writers stay strict (design §4,
/// "Callers moved off the strict path"), and a held or stale snapshot admits no receipt.
struct NativeReceiptGuard {
    cache: &'static Cache,
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
        Registry::at(self.cache.paths.clone()).with_admitted_projection(
            &self.cache.config,
            &self.cache.paths,
            &self.vault_config,
            None,
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
                    || decided(&self.cache.lab_for_source(current)) != decided(&self.lab)
                {
                    return Ok(Err(SyncError::retry(Retry::Changed, "native account source or local policy changed before receipt admission")));
                }
                Ok(save())
            },
        ).map_err(|why| SyncError::retry(Retry::Unavailable, why.to_string())).and_then(|result| result)
    }
}

impl alab_roster::Source for &'static Cache {
    fn snapshot(&self) -> SourceSnapshot {
        self.refresh();
        self.projection()
    }

    fn capture(&self) -> alab_roster::SourceCapture {
        self.capture_for(&self.config)
    }

    fn capture_for(&self, caller_config: &Path) -> alab_roster::SourceCapture {
        self.refresh();
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
                vault_config,
                lab: projected.lab,
                config_image,
                caller_config: caller_config.to_path_buf(),
                caller_image,
            }),
        )
    }

    fn mail(&self) -> Vec<LabMailbox> {
        self.refresh();
        Cache::capture(self)
            .1
            .and_then(|projected| projected.source.mail)
            .map(|mail| lab_mail(mail.source().mailboxes()))
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
            pending: true,
        }),
        refreshing: Mutex::new(()),
        mail_refused: Mutex::new(None),
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
