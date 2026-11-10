// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The vault this Mac's fleet state is of (`docs/DESIGN-fluid-swarm-2026-10-28.md` F17,
//! `docs/KNOWN_ISSUES.md` ATERM-KI-1105-10): the vault configured when this Mac took the fleet's
//! key, its book and its drive grants, which only then admit a drive.
//!
//! Fleet admission judges a key this Mac's membership log does not bind as with no log (F17): the
//! book's keyring binds it, and a drive grant (`[fleet] drive_from`, a named node or `"*"`) lets
//! it drive. That is what keeps a founder Genesis v2 lists with no node key, or a Mac whose build
//! signs no `Node` record yet, driving the Macs of its vault. Once this Mac moves to another vault
//! (`aterm secrets join`, or `aterm members join` with a person's phrase), the new vault's log
//! binds none of the old vault's Macs, and every one of them the book binds that holds this Mac's
//! fleet secret would drive it by the grants made under the vault it left, a Mac that vault held
//! out included. So each grant is bound to the vault it was made under: the vault this Mac's
//! fleet state was taken under ([`fleet_member::VAULT_FILE`]), which an init writes in the take
//! of its key, and a join (a sync's of the vault's default fleet included) in the take of its
//! grants, once the grants of any other vault are out of aterm.toml and before a grant made under
//! this one; and a grant can be made only while that vault is the one configured (`aterm fleet
//! allow drive` refuses otherwise). A take records the vault it was decided under ([`take`]): the
//! one configured when a person's join began, or when a sync read the vault's default fleet code
//! it follows, never one configured while it dialled, so a Mac that moves to another vault while
//! a join of the vault it left dials holds that fleet as the vault left's, and its grants admit
//! nothing. While the
//! vault configured is another, or none, no grant admits any Mac ([`grants_in_effect`], read by
//! every reader of the grants: the window's listener, the node job's, a service activation, and
//! `aterm fleet peers` and `allow`), a member the new vault's log binds included, until a fleet
//! is taken under the vault configured; a join that takes the fleet held again under it takes
//! the old grants out, as a join of another fleet does. The book's rows are of the fleet held,
//! and so of the vault it was taken under: a heartbeat that refills the book from a member under
//! the shared secret writes no vault, so no entry another Mac sends moves which vault the book is
//! of.
//!
//! A vault is told from another as the history this Mac acknowledged ties a vault's membership to
//! it ([`Vault`]): its name, and a key line that keeps a key of the one before. That survives a
//! mirror added, a host renamed, a signer rotation, and the vault's genesis seeded after the
//! fleet was taken; a join of another vault, or the vault's name on a line that keeps no key of
//! the one before (a replacement with no handoff chain), is another vault. Each writer that finds
//! the vault configured to be the one recorded widens the record by the keys of the line
//! configured now, so a chain of rotations stays one vault.
//!
//! A record written by no take (a fleet joined by an older build, or under no vault) is adopted:
//! readers read it as the vault configured now, and the first writer that reads a vault
//! configured under the book's lock records that one ([`keep`]: every heartbeat round, a read of
//! the issuer's book), or the vault a sync read its code under where that sync finds this Mac in
//! the vault's fleet or rekeys it ([`keep_as`]). A state that holds no fleet's key records none.
//! A Mac that moved to another vault before any such writer ran keeps the defect the record
//! ends: the vault it adopts is the one it moved to, and a join of the same fleet again keeps
//! the grants, as the record is of the vault configured by then. A record that cannot be read,
//! or a vault configuration that cannot, admits no grant ([`Tenure::Unread`]), and a take of the
//! fleet under the vault configured takes the grants out, as it cannot tell they were made under
//! it.

use aterm_net::fleet::DriveGrant;
use aterm_net::fleet_member::{self, Began, HeldBook};
use aterm_secrets::client::Paths;
use std::collections::BTreeSet;
use std::path::Path;

/// A vault as fleet admission tells one from another: its name and the key line its
/// configuration pins (its signer and the keys that signer was reached from). It is the same
/// vault while the name stays and the line keeps a key of the one before ([`Vault::is`]), as the
/// history this Mac acknowledged ties a vault's membership to its name and keys: a mirror added
/// or a host renamed changes neither, and a signer rotation pins a new signer reached from the
/// old one, keeping the old among the keys it followed. A join elsewhere names another vault or
/// pins another line, and a replacement (the pin giving way to the key this build names for a
/// retired one) pins a line that keeps no key of the one before unless its handoff chain reached
/// from one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Vault {
    name: String,
    keys: BTreeSet<[u8; 32]>,
}

/// The first line of [`fleet_member::VAULT_FILE`]: what it is, for a person who opens it.
const HEAD: &str = "# The vault this machine's fleet state (its key, its book and its drive grants) was \
                    taken under: written by aterm at each fleet join, init and sync, never by hand.";

impl Vault {
    /// The vault `paths` configures now: `None` with none, one whose configuration does not
    /// read, or one that pins no signer. Read through the configuration's memo
    /// ([`aterm_secrets::client::config_now`]), as [`tenure`] reads it: the node's heartbeat
    /// ([`keep`]) and its membership log's reads ask it too.
    pub(crate) fn configured(paths: &Paths) -> Option<Self> {
        Self::of_config(&aterm_secrets::client::config_now(paths).ok().flatten()?)
    }

    /// The vault `config` configures: `None` where it pins no signer.
    fn of_config(config: &aterm_secrets::client::Config) -> Option<Self> {
        let signer = config.signer?;
        Some(Self {
            name: config.name.clone(),
            keys: std::iter::once(signer)
                .chain(config.followed.iter().copied())
                .collect(),
        })
    }

    /// Whether `now` is this vault still: the same name, and a key of this line on its line.
    pub(crate) fn is(&self, now: &Self) -> bool {
        self.name == now.name && !self.keys.is_disjoint(&now.keys)
    }

    /// This vault's record widened by the keys `now`'s line holds and it does not, where `now` is
    /// this vault still ([`Self::is`]): `None` where there is nothing to add.
    fn widened(&self, now: &Self) -> Option<Self> {
        (self.is(now) && !now.keys.is_subset(&self.keys)).then(|| Self {
            name: self.name.clone(),
            keys: self.keys.union(&now.keys).copied().collect(),
        })
    }

    /// Its [`fleet_member::VAULT_FILE`] text: the head, `name <hex of its UTF-8>`, then one
    /// `key <64 hex>` line for each key of its line.
    fn text(&self) -> String {
        let mut text = format!(
            "{HEAD}\nname {}\n",
            aterm_codec::hex::encode_lower(self.name.as_bytes())
        );
        for key in &self.keys {
            text.push_str(&format!("key {}\n", aterm_codec::hex::encode_lower(key)));
        }
        text
    }

    /// The vault `text` records ([`Self::text`]): `None` for any other text, a record with no
    /// name, no key, a line it does not know, or a name or key twice.
    fn of_text(text: &str) -> Option<Self> {
        let mut name = None;
        let mut keys = BTreeSet::new();
        for line in text.lines().filter(|line| !line.starts_with('#')) {
            match line.split_once(' ')? {
                ("name", hex) if name.is_none() && aterm_codec::hex::is_lower(hex, hex.len()) => {
                    let bytes = aterm_codec::hex::decode_vec(hex.as_bytes())?;
                    name = Some(String::from_utf8(bytes).ok()?);
                }
                ("key", hex) if aterm_codec::hex::is_lower(hex, 64) => {
                    let mut key = [0; 32];
                    if !aterm_codec::hex::decode_into(hex.as_bytes(), &mut key) || !keys.insert(key)
                    {
                        return None;
                    }
                }
                _ => return None,
            }
        }
        let name = name.filter(|name| !name.is_empty())?;
        (!keys.is_empty()).then_some(Self { name, keys })
    }
}

/// Whether the drive grants of the fleet state in `state` admit now: where the vault configured
/// in `vault` is the one that fleet state was taken under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tenure {
    /// They admit: the fleet state was taken under the vault configured now, or no take recorded
    /// one (a fleet an older build or no vault took, adopted as the vault configured now), or
    /// this process reads no vault (under a private state root: a test, a lab's scratch state).
    Here,
    /// They admit no Mac: the fleet state was taken under the vault `taken` names, and the one
    /// it is read against (configured now, or the one a take is decided under: [`tenure_under`])
    /// is another (`now`, its name: another name, or the same name on a key line that keeps no
    /// key of the one it was taken under), or none (`None`).
    Elsewhere {
        /// The name of the vault the fleet state was taken under.
        taken: String,
        /// The name of the vault it is read against, if any.
        now: Option<String>,
    },
    /// They admit no Mac: the record of the vault the fleet state was taken under, or the
    /// configuration of the vault configured now, cannot be read (why), so no vault can be told
    /// to be it.
    Unread(String),
}

impl Tenure {
    /// Why no grant admits any Mac, in the words `aterm fleet peers` and `allow` say it; `None`
    /// where they admit.
    #[must_use]
    pub fn void(&self) -> Option<String> {
        match self {
            Self::Here => None,
            Self::Elsewhere { taken, now } => Some(format!(
                "this Mac's fleet, its book and its drive grants were taken under vault \"{}\", \
                 and {}, so no drive grant made under it lets a Mac drive this one",
                aterm_codec::tty::one_line(taken, 80),
                match now.as_deref() {
                    None => "this Mac is in no vault now".to_owned(),
                    Some(now) if now == taken => format!(
                        "the vault \"{}\" this Mac is in now pins another signer, reached from \
                         none of that one's",
                        aterm_codec::tty::one_line(now, 80)
                    ),
                    Some(now) => format!(
                        "this Mac is in vault \"{}\" now",
                        aterm_codec::tty::one_line(now, 80)
                    ),
                }
            )),
            Self::Unread(why) => Some(format!(
                "{}, so no vault can be told to be the one this Mac's fleet was taken under, and \
                 no drive grant lets a Mac drive this one",
                aterm_codec::tty::one_line(why, 160)
            )),
        }
    }
}

/// Where the fleet state in `state` stands against the vault `vault` configures now
/// ([`Tenure`]): `vault` is `None` where this process reads no vault. The node's watch asks it at
/// each tick and each fleet handshake, so the configuration is read through its memo
/// ([`aterm_secrets::client::config_now`]), again only when `source.toml` or the envelope it takes
/// the signer's chain from moved.
#[must_use]
pub fn tenure(state: &Path, vault: Option<&Paths>) -> Tenure {
    let Some(paths) = vault else {
        return Tenure::Here;
    };
    let taken = match recorded(state) {
        Ok(None) => return Tenure::Here,
        Ok(Some(taken)) => taken,
        Err(why) => return Tenure::Unread(why),
    };
    let config = match aterm_secrets::client::config_now(paths) {
        Ok(config) => config,
        Err(e) => {
            return Tenure::Unread(format!(
                "the configuration of the vault this Mac is in cannot be read: {e}"
            ));
        }
    };
    match config.as_ref().and_then(Vault::of_config) {
        Some(now) if taken.is(&now) => Tenure::Here,
        _ => Tenure::Elsewhere {
            taken: taken.name,
            now: config.map(|config| config.name),
        },
    }
}

/// Where the fleet state in `state` stands against `under`, the vault a take of the fleet is
/// decided under (`None`: none), as [`tenure`] against the one configured: what such a take
/// finds of the grants beside the record (`vault` is `None` where this process reads no vault,
/// and every grant admits).
#[must_use]
pub(crate) fn tenure_under(state: &Path, vault: Option<&Paths>, under: Option<&Vault>) -> Tenure {
    if vault.is_none() {
        return Tenure::Here;
    }
    match recorded(state) {
        Ok(None) => Tenure::Here,
        Ok(Some(taken)) if under.is_some_and(|under| taken.is(under)) => Tenure::Here,
        Ok(Some(taken)) => Tenure::Elsewhere {
            taken: taken.name,
            now: under.map(|under| under.name.clone()),
        },
        Err(why) => Tenure::Unread(why),
    }
}

/// The vault the record in `state` names (`None`: no record), or why it cannot be read.
fn recorded(state: &Path) -> Result<Option<Vault>, String> {
    let record = state.join(fleet_member::VAULT_FILE);
    match fleet_member::read_vault(state) {
        Ok(None) => Ok(None),
        Ok(Some(text)) => Vault::of_text(&text)
            .map(Some)
            .ok_or_else(|| format!("{} holds no vault's name and keys", record.display())),
        Err(e) => Err(format!("{} cannot be read: {e}", record.display())),
    }
}

/// The grants among `grants` that admit now: all of them where the fleet state in `state` is of
/// the vault `vault` configures now ([`tenure`]), none where it is of another or none.
#[must_use]
pub fn grants_in_effect(
    state: &Path,
    vault: Option<&Paths>,
    grants: Vec<DriveGrant>,
) -> Vec<DriveGrant> {
    if grants.is_empty() || tenure(state, vault).void().is_some() {
        return Vec::new();
    }
    grants
}

/// Record `under`, the vault a take of the fleet is decided under, as the one the fleet state was
/// taken under, beside `book`: an init's take of its key, a join's take of its grants, a sync's
/// take of the fleet it issues again. The vault is the one configured when the take was decided
/// (a person's verb began, or a sync read the code it follows), never one read here, so a move
/// while a join dialled leaves a record of the vault left, beside which no grant admits under
/// the one moved to. With none (no vault, none read by this process, or one that pins no
/// signer), no record, which the first writer to read a vault adopts ([`keep`]).
/// # Errors
/// The record cannot be written or removed.
pub(crate) fn take(book: &HeldBook<'_>, under: Option<&Vault>) -> std::io::Result<()> {
    book.keep_vault(under.map(Vault::text).as_deref())
}

/// What a writer under the book's lock keeps of the record in `state`: with none there, the
/// vault `vault` configures now, adopted; with one of the vault configured, the keys of the line
/// configured now added; nothing else. Best effort: a lock not taken, or a record that cannot be
/// read or written, keeps the record as it is, and the next writer tries again.
pub fn keep(state: &Path, vault: Option<&Paths>) {
    keep_as(state, vault.and_then(Vault::configured));
}

/// [`keep`] for the vault `now` (none: nothing): a heartbeat's, the vault configured now; a sync's
/// in the vault's fleet, the vault it read that fleet's code under, which the fleet state is of
/// whichever vault is configured by the time it writes.
pub(crate) fn keep_as(state: &Path, now: Option<Vault>) {
    let Some(now) = now else {
        return;
    };
    // Read first without the lock: most rounds have nothing to write.
    if !fleet_member::read_vault(state).is_ok_and(|text| kept(text.as_deref(), &now).is_some()) {
        return;
    }
    let _ = fleet_member::write_held(state, Began::Here, |book| {
        keep_in(book, state, Some(now));
    });
}

/// [`keep`] in a take of the book's lock a writer holds already (`book`, of the state directory
/// `state`), for the vault `now` configured now (none: nothing). A state that holds no fleet's
/// key has no fleet state to record the vault of: nothing.
pub(crate) fn keep_in(book: &HeldBook<'_>, state: &Path, now: Option<Vault>) {
    if let Some(now) = now
        && matches!(book.held(), fleet_member::Held::Key(_))
        && let Ok(text) = fleet_member::read_vault(state)
        && let Some(kept) = kept(text.as_deref(), &now)
    {
        let _ = book.keep_vault(Some(&kept.text()));
    }
}

/// The record a writer keeps in place of `text` (none there: `None`) for the vault `now`
/// configured now: `now`, adopted, where there is none; the record widened by `now`'s keys where
/// it is of `now` and lacks some; `None` where it stays as it is.
fn kept(text: Option<&str>, now: &Vault) -> Option<Vault> {
    match text {
        None => Some(now.clone()),
        Some(text) => Vault::of_text(text)?.widened(now),
    }
}

/// The vault a fleet state directory `state` reads its deny list, its membership log and the
/// vault its grants are of from: a scratch test root holding `.secrets/` or its vault folder
/// beside `state`, or the live home's when `state` is the live account's state directory.
pub(crate) fn vault_paths_for(state: &Path) -> Option<Paths> {
    if let Some(root) = state.parent() {
        let candidate = Paths::for_home(root);
        if root.join(".secrets").is_dir() || candidate.dir.is_dir() {
            return Some(candidate);
        }
    }
    if aterm_types::dirs::state_dir().as_deref() == Some(state) {
        return Paths::live().ok();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault(name: &str, keys: &[u8]) -> Vault {
        Vault {
            name: name.to_owned(),
            keys: keys.iter().map(|byte| [*byte; 32]).collect(),
        }
    }

    /// A record reads back as the vault it records, and nothing else reads as one.
    #[test]
    fn a_record_reads_back_as_its_vault_and_nothing_else_does() {
        let alab = vault("alab", &[1, 2]);
        assert_eq!(Vault::of_text(&alab.text()), Some(alab.clone()));
        let named = vault("a vault, named \"so\"", &[3]);
        assert_eq!(Vault::of_text(&named.text()), Some(named));
        let text = alab.text();
        for broken in [
            String::new(),
            HEAD.to_owned(),
            text.replace("name ", "nom "),
            text.lines()
                .filter(|line| !line.starts_with("key"))
                .collect::<Vec<_>>()
                .join("\n"),
            text.lines()
                .filter(|line| !line.starts_with("name"))
                .collect::<Vec<_>>()
                .join("\n"),
            format!("{text}name 61\n"),
            format!("{text}key {}\n", "01".repeat(32)),
            format!("{text}key {}\n", "AB".repeat(32)),
            format!("{text}key 01\n"),
            "name \nkey 0101010101010101010101010101010101010101010101010101010101010101\n"
                .to_owned(),
        ] {
            assert_eq!(Vault::of_text(&broken), None, "{broken:?}");
        }
    }

    /// The same vault through a rotation, and none other: the name kept and a key of the line
    /// kept. A record widened by the line configured now stays that vault through a chain of
    /// rotations whose line alone keeps no key of the first.
    #[test]
    fn a_vault_is_itself_through_its_rotations_and_no_other() {
        let first = vault("alab", &[1]);
        assert!(first.is(&vault("alab", &[2, 1])), "a rotation");
        assert!(
            !first.is(&vault("alab", &[3])),
            "another line under the name"
        );
        assert!(!first.is(&vault("other", &[1])), "another name");
        let widened = first.widened(&vault("alab", &[2, 1])).expect("a key added");
        assert_eq!(widened, vault("alab", &[1, 2]));
        assert_eq!(
            widened.widened(&vault("alab", &[2])),
            None,
            "nothing to add"
        );
        assert!(
            widened.is(&vault("alab", &[2])),
            "the line re-pinned at the rotated key alone (a join at another place)"
        );
        assert!(
            !first.is(&vault("alab", &[2])),
            "CONTROL: the record not widened"
        );
        assert_eq!(first.widened(&vault("other", &[1])), None);
    }
}
