// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! LabView's cache (`docs/DESIGN-alab-lab-model-2026-11-01.md` §7, step 11): the harness's
//! `aterm_agent::harness::alab_roster::Source`, filled from `aterm_db::alab::Registry::lab`.
//!
//! A projection is read as an observer, writes nothing and calls no provider. The first caller in
//! a process projects once; after that a background thread reads the registry's log and its
//! inventory every five minutes and projects again only when the inventory moved
//! (`LabRead::Unchanged`), while every caller gets the held projection at once. Once a projection
//! names this Mac, an owner-private `named` file beside `settled` records it; while a failed read
//! then leaves no projection held, the source is `pending` (the harness reads no sheet meanwhile)
//! and a caller looks again after 15 seconds. Only syncs, the accounts list and the mail routes ask, never a typing
//! path, so typing never waits on it. `[alab] roster = false` in `aterm.toml` turns the source
//! off on this Mac. A test, a debug build and a build tool's run hold no lab (inert, as the other
//! lab hooks are), so no test reads this Mac's registry.
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
        alab_roster::{self, Lab, LabAccount, Placements},
    },
};
use aterm_db::alab::{
    Inventory, LabRead, Registry, import::MailboxKeys, journal::place::Places, view::LabView,
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
/// scratch state root.
fn holds() -> bool {
    !cfg!(debug_assertions)
        && std::env::var_os("CARGO_MANIFEST_DIR").is_none()
        && std::env::var_os("ATERM_STATE_HOME").is_none()
        && std::env::var_os("CI").is_none()
}

/// Whether `aterm.toml` at `config` turns the roster source off (`[alab] roster = false`).
fn turned_off(config: &Path) -> bool {
    let Ok(Some(bytes)) = aterm_secrets::fsx::read_aterm_config(config, 1024 * 1024) else {
        return false;
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return false;
    };
    aterm_toml::from_str::<aterm_toml::Table>(text)
        .ok()
        .and_then(|table| table.get("alab")?.get("roster")?.as_bool())
        == Some(false)
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
    std::str::from_utf8(&bytes)
        .is_ok_and(|held| held.trim() == aterm_codec::hex::encode_lower(table))
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
    let Some(witness) = report
        .matching_inventory()
        .filter(|witness| witness.inventory().count > 0)
    else {
        return;
    };
    let table = witness.scope().digest();
    if Registry::at(paths.clone()).table().ok().flatten() != Some(table) {
        return;
    }
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

/// Each mailbox key this Mac can name the login of, to that login lower-cased: a login of
/// `ai-mailbox.toml` (`logins`) whose digest the importer's mailbox keys hold (`keys`). Only an
/// admin Mac that imported holds both; elsewhere it is empty.
fn mailbox_logins(keys: &MailboxKeys, logins: &[String]) -> BTreeMap<String, String> {
    logins
        .iter()
        .filter_map(|login| {
            Some((
                keys.pk_of(login)?.to_owned(),
                login.trim().to_ascii_lowercase(),
            ))
        })
        .collect()
}

/// What this Mac reads to name a mailbox key's login ([`mailbox_logins`]): the importer's keys
/// beside the vault state, and the logins of `~/.secrets/ai-mailbox.toml`. Nothing where either
/// is missing or unreadable.
fn mailbox_names(paths: &Paths) -> BTreeMap<String, String> {
    let Ok(keys) = aterm_db::alab::command::read_keys(paths) else {
        return BTreeMap::new();
    };
    if keys.is_empty() {
        return BTreeMap::new();
    }
    let file = paths
        .home
        .join(".secrets")
        .join(aterm_agent::harness::headless_oauth::SECRETS_MAILBOX_TOML_FILE);
    mailbox_logins(
        &keys,
        &aterm_agent::harness::mailbox_routes::mailbox_logins(&file),
    )
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
    let mut accounts = Vec::new();
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
    let mailboxes = named_mailboxes(&view.mailboxes(), logins);
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
    inventory: Option<Inventory>,
    looked: Option<Instant>,
    /// The last read failed, no projection is held, and a projection named this Mac before.
    failed: bool,
}

struct Cache {
    paths: Paths,
    config: PathBuf,
    held: Mutex<Held>,
    refresher: OnceLock<()>,
}

impl Cache {
    /// Project again where the inventory moved since the held projection; where it did not,
    /// only the held lab's `settled` is brought up to date.
    fn refresh(&self) {
        let registry = Registry::at(self.paths.clone());
        let was = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .inventory;
        let read = if turned_off(&self.config) {
            Ok(None)
        } else {
            registry.lab(&self.config, was)
        };
        let settled = settled(&registry);
        let logins = mailbox_names(&self.paths);
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held.looked = Some(Instant::now());
        held.failed =
            read.is_err() && held.lab.is_none() && settled_dir().is_some_and(|dir| named_in(&dir));
        match read {
            Ok(Some(LabRead::Projected(view, inventory))) => {
                if view.my_label().is_some()
                    && let Some(dir) = settled_dir()
                {
                    note_named(&dir);
                }
                held.inventory = Some(inventory);
                held.placed = view.mailboxes();
                held.lab = Some(Arc::new(harness_lab(&view, settled, &logins)));
            }
            Ok(Some(LabRead::Unchanged)) => {
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
            }
            Ok(None) => {
                held.inventory = None;
                held.placed.clear();
                held.lab = None;
            }
            // A failed read keeps what was held; the next look reads again.
            Err(why) => aterm_log::debug!("lab view: {why}"),
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
        let (first, retry) = {
            let held = self
                .held
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (
                held.looked.is_none(),
                held.failed && held.looked.is_some_and(|at| at.elapsed() >= RETRY_FAILED),
            )
        };
        if first || retry {
            self.refresh();
            self.start_refresher();
        }
        self.held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .lab
            .clone()
    }

    fn pending(&self) -> bool {
        self.held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .failed
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

    /// A mailbox key names its login only where the importer's keys record that login and the
    /// mailbox config lists it: each listed login the keys know maps its key to the login,
    /// lower-cased, and a login the keys do not know, or a key no listed login has, maps nothing.
    /// NEGATIVE CONTROL: with `mailbox_logins` keeping logins whose `pk_of` is `None` (keyed by
    /// the login itself), the unknown login is mapped (`left: 2 right: 1`).
    #[test]
    fn a_mailbox_key_names_only_a_login_the_importer_recorded() {
        let mut keys = MailboxKeys::default();
        keys.record("hub@example.test", "mb-0000000000000001", "hub");
        keys.record("away@example.test", "mb-0000000000000002", "away");
        let logins = [
            "Hub@Example.test".to_owned(),
            "other@example.test".to_owned(),
        ];
        let named = mailbox_logins(&keys, &logins);
        assert_eq!(named.len(), 1);
        assert_eq!(
            named.get("mb-0000000000000001").map(String::as_str),
            Some("hub@example.test")
        );
    }

    /// A test process holds no lab, so no test reads this Mac's registry.
    /// NEGATIVE CONTROL: with `holds` returning `true`, this fails.
    #[test]
    fn a_test_holds_no_lab() {
        assert!(!holds());
    }

    /// `[alab] roster = false` turns the source off; anything else, or no file, leaves it on.
    #[test]
    fn the_roster_source_is_on_unless_turned_off() {
        let dir = aterm_tempfile::TempDir::new().unwrap();
        let config = dir.path().join("aterm.toml");
        assert!(!turned_off(&config));
        std::fs::write(&config, "[alab]\nroster = true\n").unwrap();
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
