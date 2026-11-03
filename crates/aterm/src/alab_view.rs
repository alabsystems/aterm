// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! LabView's cache (`docs/DESIGN-alab-lab-model-2026-11-01.md` §7, step 11): the harness's
//! `aterm_agent::harness::alab_roster::Source`, filled from `aterm_db::alab::Registry::lab`.
//!
//! A projection is read as an observer, writes nothing and calls no provider. The first caller in
//! a process projects once; after that a background thread reads the registry's log and its
//! inventory every five minutes and projects again only when the inventory moved
//! (`LabRead::Unchanged`), while every caller gets the held projection at once. Only syncs, the accounts list and the mail routes ask, never a typing
//! path, so typing never waits on it. `[alab] roster = false` in `aterm.toml` turns the source
//! off on this Mac. A test, a debug build and a build tool's run hold no lab (inert, as the other
//! lab hooks are), so no test reads this Mac's registry.
//!
//! [`round`] records, per registry table, that an authenticated peer reported exactly this Mac's
//! retained registry operations in a round that completed (`RepairRound::matching_inventory`, the
//! one gate that admits a witness): `Lab::settled`, which lets migration rename rows. It stays
//! set for that table: it says this Mac's history is no partial one, which later operations
//! extend and cannot undo.

use aterm_agent::{
    ai::connector::{Flow, Region},
    harness::{
        accounts::{Cli, Vendor},
        alab_roster::{self, Lab, LabAccount},
    },
};
use aterm_db::alab::{Inventory, LabRead, Registry, view::LabView};
use aterm_secrets::client::Paths;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The least time between two looks at the registry's inventory.
const LOOK_EVERY: Duration = Duration::from_secs(300);

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

/// `view` as the harness reads it.
fn harness_lab(view: &LabView, settled: bool) -> Lab {
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
    Lab {
        me: view.my_label().map(str::to_owned),
        settled,
        accounts,
        aliases: view.aliases(),
    }
}

struct Held {
    lab: Option<Arc<Lab>>,
    inventory: Option<Inventory>,
    looked: Option<Instant>,
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
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held.looked = Some(Instant::now());
        match read {
            Ok(Some(LabRead::Projected(view, inventory))) => {
                held.inventory = Some(inventory);
                held.lab = Some(Arc::new(harness_lab(&view, settled)));
            }
            Ok(Some(LabRead::Unchanged)) => {
                if let Some(lab) = held.lab.as_mut().filter(|lab| lab.settled != settled) {
                    Arc::make_mut(lab).settled = settled;
                }
            }
            Ok(None) => {
                held.inventory = None;
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
        let first = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .looked
            .is_none();
        if first {
            self.refresh();
            self.start_refresher();
        }
        self.held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .lab
            .clone()
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
            inventory: None,
            looked: None,
        }),
        refresher: OnceLock::new(),
    });
    let _ = alab_roster::install(Box::new(cache));
}

#[cfg(test)]
mod tests {
    use super::*;

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
