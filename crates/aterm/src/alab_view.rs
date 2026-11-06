// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! LabView's cache (`docs/DESIGN-alab-lab-model-2026-11-01.md` §7, step 11): the harness's
//! `aterm_agent::harness::alab_roster::Source`, filled from `aterm_db::alab::Registry::lab`.
//!
//! A projection is read as an observer, writes nothing and calls no provider. The first caller in
//! a process projects once; after that a background thread reads the registry's log and its
//! inventory every five minutes and projects again only when the inventory moved
//! (`LabRead::Unchanged`), while every caller gets the held projection at once. Once a projection
//! names this Mac, an owner-private `named` file beside `settled` records it; while a failed read,
//! or one that finds this Mac's history of the registry partial, then leaves no projection held,
//! the source is `pending` (the harness reads no sheet meanwhile)
//! and a caller looks again after 15 seconds. Only syncs, the accounts list and the mail routes ask, never a typing
//! path, so typing never waits on it. `[alab] roster = false` in `aterm.toml` turns the source
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

use aterm_agent::{
    ai::connector::{Flow, Region},
    harness::{
        accounts::{Cli, Vendor},
        alab_roster::{self, Lab, LabAccount, LabMailbox, Part, Placements},
    },
};
use aterm_db::alab::{
    Inventory, LabRead, Registry, journal::place::Places, mail, secret::Journal, view::LabView,
};
use aterm_secrets::client::Paths;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The least time between two looks at the registry's inventory.
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
    let text = match aterm_types::dirs::read_aterm_config_text(config) {
        Ok(None) => return LocalRosterPolicy::Enabled,
        Ok(Some(text)) => text,
        Err(_) => return LocalRosterPolicy::Unavailable,
    };
    let Ok(table) = aterm_toml::from_str::<aterm_toml::Table>(&text) else {
        return LocalRosterPolicy::Unavailable;
    };
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

/// Whether admitted local policy explicitly turns the roster source off.
fn turned_off(config: &Path) -> bool {
    matches!(roster_policy(config), LocalRosterPolicy::Disabled)
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
    let Ok(held) = std::str::from_utf8(&bytes) else {
        return false;
    };
    let trimmed = held.trim().as_bytes();
    let mut decoded = [0u8; 32];
    !trimmed.iter().any(u8::is_ascii_uppercase)
        && aterm_codec::hex::decode_into(trimmed, &mut decoded)
        && &decoded == table
}

/// Whether `alab_dir` records that a projection of the lab named this Mac ([`note_named`]).
fn named_in(alab_dir: &Path) -> bool {
    aterm_uds::private_file::read_private(alab_dir, "named", 64).is_ok()
}

/// Record, once, that a projection of the lab named this Mac: while a later read fails, the
/// harness then reads no sheet ([`alab_roster::Source::pending`]), so a failed look never lets
/// the sheet path label a row after a machine the lab names. A Mac the lab never named keeps
/// reading the sheet when its registry cannot be read.
fn note_named(alab_dir: &Path) {
    if named_in(alab_dir) {
        return;
    }
    if aterm_types::fs_restricted::ensure_private_dir(alab_dir).is_ok() {
        let _ = aterm_agent::harness::upgrade_models::write_atomic_mode(
            &alab_dir.join("named"),
            "1\n",
            Some(0o600),
        );
    }
}

/// Whether a peer matched this Mac's inventory of the registry table it holds now.
fn settled(registry: &Registry) -> bool {
    let (Some(dir), Ok(Some(table))) = (settled_dir(), registry.table()) else {
        return false;
    };
    settled_in(&dir, &table)
}

/// One completed repair round: where it showed an authenticated peer holding exactly this Mac's
/// retained registry operations, record that table, once.
pub(crate) fn round(paths: &Paths, report: &aterm_db::runtime::RepairRound) {
    if !holds() {
        return;
    }
    // Only the round's completion gate admits a witness: a cancelled, interrupted, deferred or
    // failed round, or one whose witness names another peer, table or inventory, settles nothing.
    let Some(table) = witness_table(
        report
            .matching_inventory()
            .map(|witness| (witness.inventory().count, witness.scope().digest())),
        || Registry::at(paths.clone()).table().ok().flatten(),
    ) else {
        return;
    };
    let Some(dir) = settled_dir() else {
        return;
    };
    if settled_in(&dir, &table) {
        return;
    }
    let hex = aterm_codec::hex::encode_lower(&table);
    if aterm_types::fs_restricted::ensure_private_dir(&dir).is_ok() {
        let _ = aterm_agent::harness::upgrade_models::write_atomic_mode(
            &dir.join("settled"),
            &hex,
            Some(0o600),
        );
    }
}

/// The registry table a completed round's witness (`witness`: the operations it counts and its
/// table) settles: its table where it counts at least one operation and the registry holds that
/// table now (`held`, read only where there is such a witness), else none.
fn witness_table(
    witness: Option<(usize, [u8; 32])>,
    held: impl FnOnce() -> Option<[u8; 32]>,
) -> Option<[u8; 32]> {
    let (_, table) = witness.filter(|(count, _)| *count > 0)?;
    (held() == Some(table)).then_some(table)
}

/// Each mailbox key a login of `ai-mailbox.toml` (`logins`) derives
/// (`keys::mailbox_pk` of its address key), to that login lower-cased.
fn mailbox_logins(logins: &[String]) -> BTreeMap<String, String> {
    logins
        .iter()
        .filter_map(|login| {
            let address = aterm_db::typed::keys::address_pk(login.trim()).ok()?;
            Some((
                aterm_db::typed::keys::mailbox_pk(&address).ok()?,
                login.trim().to_ascii_lowercase(),
            ))
        })
        .collect()
}

/// What this Mac reads to name a mailbox key's login ([`mailbox_logins`]): the logins of
/// `~/.secrets/ai-mailbox.toml`, the fallback for a mailbox the registry names no login of
/// (a version-1 row) until B13. Nothing where it is missing or unreadable.
fn mailbox_names(paths: &Paths) -> BTreeMap<String, String> {
    let file = paths
        .home
        .join(".secrets")
        .join(aterm_agent::harness::headless_oauth::SECRETS_MAILBOX_TOML_FILE);
    mailbox_logins(&aterm_agent::harness::mailbox_routes::mailbox_logins(&file))
}

/// The registry's mailboxes as the harness reads them ([`LabMailbox`]): never a value.
pub(crate) fn lab_mail(mailboxes: &[mail::Mailbox]) -> Vec<LabMailbox> {
    mailboxes
        .iter()
        .map(|m| LabMailbox {
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
        })
        .collect()
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
            .filter_map(|p| Some((view.machine_label(&p.machine)?.to_owned(), p.rank)))
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
        me: view.my_label().map(str::to_owned),
        settled,
        accounts,
        aliases: view.aliases(),
        mailboxes,
        places: Default::default(),
    }
}

struct Held {
    lab: Option<Arc<Lab>>,
    /// The held projection's addresses, each to the mailbox key the lab places it in: the
    /// mailbox logins are named again at every look ([`mailbox_names`]), so a mailbox added to
    /// or removed from `ai-mailbox.toml` is seen with the registry unchanged.
    placed: BTreeMap<String, String>,
    /// The held projection's registry mailboxes ([`lab_mail`]), read with it.
    mail: Vec<LabMailbox>,
    inventory: Option<Inventory>,
    looked: Option<Instant>,
    /// The last read failed or found this Mac's history of the registry partial, no projection
    /// is held, and a projection named this Mac before.
    failed: bool,
}

/// One look's change to what is held, from what it `read` (a failed read as its error's words),
/// this Mac's `settled` witness and mailbox `logins`, and whether a projection named this Mac
/// before (`named`, read only where a failed or partial read leaves nothing held), at `now`.
/// Returns whether the read's projection names this Mac, which the caller records
/// ([`note_named`]).
fn look(
    held: &mut Held,
    read: Result<Option<LabRead>, String>,
    settled: bool,
    toml_logins: &BTreeMap<String, String>,
    named: impl FnOnce() -> bool,
    now: Instant,
) -> bool {
    held.looked = Some(now);
    // A failed read, and one that finds this Mac's history of the registry partial, says nothing
    // of the lab: what was held stays, and with nothing held a Mac the lab named is pending, so
    // the sheet path never labels a row after a machine the lab names meanwhile.
    held.failed =
        matches!(read, Err(_) | Ok(Some(LabRead::Partial))) && held.lab.is_none() && named();
    match read {
        Ok(Some(LabRead::Projected(view, inventory))) => {
            held.inventory = Some(inventory);
            held.placed = view.mailboxes();
            let logins = logins_of(&held.mail, toml_logins);
            held.lab = Some(Arc::new(harness_lab_with_placed(
                &view,
                &held.placed,
                settled,
                &logins,
            )));
            view.my_label().is_some()
        }
        Ok(Some(LabRead::Unchanged)) => {
            let logins = logins_of(&held.mail, toml_logins);
            let mailboxes = named_mailboxes(&held.placed, &logins);
            if let Some(lab) = held
                .lab
                .as_mut()
                .filter(|lab| lab.settled != settled || lab.mailboxes != mailboxes)
            {
                let lab = Arc::make_mut(lab);
                lab.settled = settled;
                lab.mailboxes = mailboxes;
            }
            false
        }
        Ok(None) => {
            held.inventory = None;
            held.placed.clear();
            held.mail.clear();
            held.lab = None;
            false
        }
        // A partial history keeps what was held; the next look reads again.
        Ok(Some(LabRead::Partial)) => false,
        // A failed read keeps what was held; the next look reads again.
        Err(why) => {
            aterm_log::debug!("lab view: {why}");
            false
        }
    }
}

/// Whether a caller looks at the registry at `now`: at the first look, and once [`RETRY_FAILED`]
/// passed since one that left the source pending (`failed`); the refresher looks otherwise.
fn due(looked: Option<Instant>, failed: bool, now: Instant) -> bool {
    looked.is_none_or(|at| failed && now.saturating_duration_since(at) >= RETRY_FAILED)
}

struct Cache {
    paths: Paths,
    config: PathBuf,
    held: Mutex<Held>,
    refresher: OnceLock<()>,
}

impl Cache {
    fn current_projection(&self) -> Option<Arc<Lab>> {
        if !matches!(roster_policy(&self.config), LocalRosterPolicy::Enabled) {
            return None;
        }
        self.held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .lab
            .clone()
    }

    fn source_pending(&self) -> bool {
        match roster_policy(&self.config) {
            LocalRosterPolicy::Unavailable => return true,
            LocalRosterPolicy::Disabled => return false,
            LocalRosterPolicy::Enabled => {}
        }
        self.held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .failed
    }

    /// Project again where the inventory moved since the held projection; where it did not,
    /// only the held lab's `settled` is brought up to date.
    fn refresh(&self) {
        let policy = roster_policy(&self.config);
        if matches!(policy, LocalRosterPolicy::Unavailable) {
            return;
        }
        let registry = Registry::at(self.paths.clone());
        let was = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .inventory;
        let read = if matches!(policy, LocalRosterPolicy::Disabled) {
            Ok(None)
        } else {
            registry
                .lab(&self.config, was)
                .map_err(|why| why.to_string())
        };
        // A new projection reads the registry's mailboxes too: names and patterns, never a value
        // (`mail::sign_in_mailboxes`). One that does not read keeps the held ones.
        let mail = matches!(read, Ok(Some(LabRead::Projected(..))))
            .then(|| mail::sign_in_mailboxes(&registry).ok())
            .flatten()
            .map(|found| lab_mail(&found));
        let settled = settled(&registry);
        let logins = mailbox_names(&self.paths);
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let named = || settled_dir().is_some_and(|dir| named_in(&dir));
        if let Some(mail) = mail {
            held.mail = mail;
        }
        if look(&mut held, read, settled, &logins, named, Instant::now())
            && let Some(dir) = settled_dir()
        {
            note_named(&dir);
        }
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
}

impl alab_roster::Source for &'static Cache {
    fn lab(&self) -> Option<Arc<Lab>> {
        let due = {
            let held = self
                .held
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            due(held.looked, held.failed, Instant::now())
        };
        if due {
            self.refresh();
            self.start_refresher();
        }
        self.current_projection()
    }

    fn pending(&self) -> bool {
        self.source_pending()
    }

    fn turned_off(&self) -> bool {
        turned_off(&self.config)
    }

    fn mail(&self) -> Vec<LabMailbox> {
        if self.lab().is_none() {
            return Vec::new();
        }
        self.held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .mail
            .clone()
    }

    fn reveal_bound(
        &self,
        expected: &LabMailbox,
        parts: &[Part],
        consume: &mut dyn FnMut(Part, &str),
    ) -> Result<(), &'static str> {
        if self.lab().is_none() {
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
            |found| {
                lab_mail(std::slice::from_ref(found)).as_slice() == std::slice::from_ref(expected)
            },
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
        if self.lab().is_none() {
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
        held: Mutex::new(Held {
            lab: None,
            placed: BTreeMap::new(),
            mail: Vec::new(),
            inventory: None,
            looked: None,
            failed: false,
        }),
        refresher: OnceLock::new(),
    });
    let _ = alab_roster::install(Box::new(cache));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mailbox key names its login by derivation: each listed login maps the key its address
    /// key derives to the login, lower-cased, and a line that is no address maps nothing.
    /// NEGATIVE CONTROL: with `mailbox_logins` keying by the login itself, the lookup by the
    /// derived key finds nothing.
    #[test]
    fn a_mailbox_key_names_the_login_it_derives_from() {
        let logins = ["Hub@Example.test".to_owned(), "not an address".to_owned()];
        let named = mailbox_logins(&logins);
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

    fn nothing_held() -> Held {
        Held {
            lab: None,
            placed: BTreeMap::new(),
            mail: Vec::new(),
            inventory: None,
            looked: None,
            failed: false,
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
        look(
            &mut held,
            Ok(Some(naming(Some("m3")))),
            false,
            &BTreeMap::new(),
            || false,
            Instant::now(),
        );
        let cache = Cache {
            paths: Paths::for_home(dir.path()),
            config: config.clone(),
            held: Mutex::new(held),
            refresher: OnceLock::new(),
        };
        let original = cache.current_projection().unwrap();
        let inventory = cache.held.lock().unwrap().inventory;
        assert!(!cache.source_pending());

        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o664)).unwrap();
        assert!(
            cache.current_projection().is_none(),
            "an unavailable current policy must not expose the cached lab"
        );
        assert!(
            cache.source_pending(),
            "unknown policy cannot select a sheet instead"
        );
        assert!(Arc::ptr_eq(
            cache.held.lock().unwrap().lab.as_ref().unwrap(),
            &original
        ));
        assert_eq!(cache.held.lock().unwrap().inventory, inventory);

        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
        for invalid in ["[alab", "[alab]\nroster = 'off'\n"] {
            std::fs::write(&config, invalid).unwrap();
            assert!(cache.current_projection().is_none());
            assert!(cache.source_pending());
        }
        std::fs::write(&config, "[alab]\nroster = false\n").unwrap();
        assert!(cache.current_projection().is_none());
        assert!(!cache.source_pending(), "explicit off is a known policy");
        std::fs::write(&config, "[alab]\nroster = true\n").unwrap();
        assert!(Arc::ptr_eq(&cache.current_projection().unwrap(), &original));
        assert!(!cache.source_pending());
        std::fs::remove_file(&config).unwrap();
        assert!(Arc::ptr_eq(&cache.current_projection().unwrap(), &original));
        assert!(!cache.source_pending(), "real absence keeps its defaults");
    }

    /// A projection whose facts row binds this Mac to a machine labelled `label` (`None`: the
    /// label is conflicted).
    fn naming(label: Option<&str>) -> LabRead {
        let mut view = LabView::default();
        view.me = Some("pk-me".into());
        view.machines.insert(
            "pk-me".into(),
            aterm_db::alab::view::Machine {
                pk: "pk-me".into(),
                label: label.map(str::to_owned),
                aliases: Vec::new(),
                live: true,
            },
        );
        LabRead::Projected(Box::new(view), Inventory([7; 32]))
    }

    /// What a look holds. A failed read and a partial history keep the held lab, and with none
    /// held are pending only on a Mac the lab named; a projection is held and says whether it
    /// names this Mac; an unchanged read brings the held lab's witness up to date; no lab clears
    /// what was held. Whether the lab named this Mac is read only where it decides.
    /// NEGATIVE CONTROL: with a partial history read as no lab, `` a partial history: a Mac the
    /// lab named is pending `` fails; with `named` dropped from `failed`, `` a failed read: a Mac
    /// the lab never named reads the sheet `` fails.
    #[test]
    fn a_look_keeps_the_lab_through_an_unread_history_and_pends_only_where_the_lab_named_this_mac()
    {
        let logins = BTreeMap::new();
        let now = Instant::now();
        let unasked =
            || -> bool { panic!("`named` is read only where a look leaves nothing held") };
        let failed = || Err::<Option<LabRead>, String>("custody unreadable".into());
        for (read, what) in [
            (failed(), "a failed read"),
            (Ok(Some(LabRead::Partial)), "a partial history"),
        ] {
            let mut held = nothing_held();
            assert!(!look(
                &mut held,
                read.clone(),
                false,
                &logins,
                || false,
                now
            ));
            assert!(
                !held.failed,
                "{what}: a Mac the lab never named reads the sheet"
            );
            let mut held = nothing_held();
            assert!(!look(&mut held, read, false, &logins, || true, now));
            assert!(held.failed, "{what}: a Mac the lab named is pending");
            assert!(held.lab.is_none(), "{what}");
            assert_eq!(held.looked, Some(now), "{what}");
        }

        let mut held = nothing_held();
        assert!(
            look(
                &mut held,
                Ok(Some(naming(Some("m3")))),
                false,
                &logins,
                unasked,
                now
            ),
            "a projection that names this Mac"
        );
        let lab = held.lab.clone().expect("the projection is held");
        assert_eq!(lab.me.as_deref(), Some("m3"));
        assert!(!lab.settled && !held.failed);
        assert_eq!(held.inventory, Some(Inventory([7; 32])));
        for (read, what) in [
            (Ok(Some(LabRead::Partial)), "a partial history"),
            (failed(), "a failed read"),
        ] {
            assert!(!look(&mut held, read, false, &logins, unasked, now));
            assert!(
                held.lab
                    .as_ref()
                    .is_some_and(|kept| Arc::ptr_eq(kept, &lab)),
                "{what} keeps the lab"
            );
            assert!(!held.failed, "{what}: a held lab is not pending");
            assert_eq!(held.inventory, Some(Inventory([7; 32])), "{what}");
        }
        assert!(!look(
            &mut held,
            Ok(Some(LabRead::Unchanged)),
            true,
            &logins,
            unasked,
            now
        ));
        assert!(
            held.lab.as_ref().is_some_and(|lab| lab.settled),
            "an unchanged read brings the held lab's witness up to date"
        );
        assert!(!look(&mut held, Ok(None), true, &logins, unasked, now));
        assert!(
            held.lab.is_none() && held.inventory.is_none() && held.placed.is_empty(),
            "no lab clears what was held"
        );
        assert!(
            !look(
                &mut nothing_held(),
                Ok(Some(naming(None))),
                false,
                &logins,
                unasked,
                now
            ),
            "a projection whose label for this Mac is conflicted does not name it"
        );
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

    /// A completed round settles a table only where its witness counts an operation and names
    /// the table the registry holds here, and the held table is read only for such a witness.
    /// NEGATIVE CONTROL: without the `count > 0` filter, `` an empty witness `` settles the held
    /// table.
    #[test]
    fn a_round_settles_only_a_witness_that_counts_an_operation_of_the_table_held_here() {
        const HELD: [u8; 32] = [1; 32];
        const OTHER: [u8; 32] = [2; 32];
        let unread = || -> Option<[u8; 32]> { panic!("the held table is read only for a witness") };
        assert_eq!(
            witness_table(Some((0, HELD)), || Some(HELD)),
            None,
            "an empty witness"
        );
        assert_eq!(
            witness_table(Some((3, OTHER)), || Some(HELD)),
            None,
            "another table"
        );
        assert_eq!(
            witness_table(Some((3, HELD)), || None),
            None,
            "no table held here"
        );
        assert_eq!(witness_table(None, unread), None, "no witness");
        assert_eq!(
            witness_table(Some((0, HELD)), unread),
            None,
            "an empty witness"
        );
        assert_eq!(witness_table(Some((3, HELD)), || Some(HELD)), Some(HELD));
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
        assert!(!turned_off(&config));
        aterm_uds::atomic_file::write(&config, b"[alab]\nroster = true\n", 0o600).unwrap();
        assert!(!turned_off(&config));
        std::fs::write(&config, "[alab]\nroster = false\n").unwrap();
        assert!(turned_off(&config));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o666)).unwrap();
            assert!(
                !turned_off(&config),
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

    /// `note_named` records once that the lab named this Mac, owner-private, and `named_in`
    /// reads it back: a fresh directory, or a witness others can read, records nothing, so a Mac
    /// the lab never named keeps reading its sheet when the registry cannot be read.
    /// NEGATIVE CONTROL: `named_in` returning true for a missing file makes the fresh directory
    /// pending (`never named`).
    #[test]
    fn the_named_witness_is_owner_private() {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = aterm_tempfile::TempDir::new().unwrap();
        let alab_dir = temp.path().join("alab");
        assert!(!named_in(&alab_dir), "never named");
        note_named(&alab_dir);
        assert!(named_in(&alab_dir));
        note_named(&alab_dir);
        assert!(named_in(&alab_dir), "a second note keeps it");
        std::fs::set_permissions(
            alab_dir.join("named"),
            std::fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(!named_in(&alab_dir), "a readable witness records nothing");
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
        aterm_agent::harness::upgrade_models::write_atomic_mode(&settled_path, &hex, Some(0o600))
            .unwrap();
        assert!(settled_in(&alab_dir, &table));
        assert!(!settled_in(&alab_dir, &[0x5b_u8; 32]));

        std::fs::set_permissions(&settled_path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!settled_in(&alab_dir, &table));

        std::fs::remove_file(&settled_path).unwrap();
        let outside = temp.path().join("outside");
        aterm_agent::harness::upgrade_models::write_atomic_mode(&outside, &hex, Some(0o600))
            .unwrap();
        std::os::unix::fs::symlink(&outside, &settled_path).unwrap();
        assert!(!settled_in(&alab_dir, &table));
    }
}
