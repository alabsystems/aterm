// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The compiled schemas of `alab/registry` (docs/DESIGN-alab-lab-model-2026-11-01.md §4), the
//! catalog table that holds the owner's data: providers and their CLIs, plans, Mac models,
//! machines, login addresses, the mailboxes those deliver to (each keyed by its login address),
//! phones, accounts, sub-accounts, sign-in routes and the accounts machines
//! prefer, plus two small
//! collections each Mac states about itself (`machine-facts`, `mailbox-obs`) under the per-Mac
//! quota.
//!
//! **Secrets** (docs/PLAN.md LT-04) are an address's `password` and a mailbox's
//! `mailbox_password` and `totp`. In the first two generations, the ones builds up to 0.200.0
//! read and every Mac holds until a roll, each is a sealed token in its cell (`Secret`), which no
//! table view opens. From the third each is a `SecretRef` cell naming the `aterm secrets` entry
//! that holds its value ([`crate::typed::secret`]): the value is never a cell there, so no table,
//! history, export or page holds it. A column whose name says secret must be one of the two
//! (checked at compile time). A row whose `protected` cell is true holds no secret
//! ([`crate::typed::check::protected`]). Contacts are ordinary data: the one column typed
//! `email`, `address.addr`, and a phone's `number` (typed `phone`) show as they are. Every other
//! address is a reference to its `address` row, a mailbox's `login` included, and a mailbox's key
//! derives from that login's key ([`keys::mailbox_pk`]). A phone's key is random, never a digest
//! of the number, and the importer finds a phone by its `number`. The version-1 `vault_ref`
//! columns of `mailbox` and `phone` stay unwritten. An imported note's text goes in
//! the row's `note` only when the note guard passes it (design §0); a refused note and every
//! losing value stay coordinates in `notes_src`.

use crate::{
    Error, Result,
    database::Change,
    typed::{
        ClaimRule, Collection, Column, Derive, Face, Faced, Kind, Member, OnConflict, ROW_KEY,
        Refused, Table, Ty, View, Writer, Writers,
        cell::{self, CellKey, CellValue, Cells},
        keys,
    },
};
use aterm_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// The registry's application word.
pub const APPLICATION: &str = "alab";
/// The registry's role.
pub const ROLE: &str = "registry";

/// A link to a registry machine from a row of another table family
/// (docs/DESIGN-lab-tracker-2026-11-14.md §4.6): the machine by its Commons machine uuid,
/// whatever generation of the registry holds its row now.
pub const MACHINE_LINK: crate::typed::Link = crate::typed::Link {
    application: APPLICATION,
    role: ROLE,
    collection: "machine",
    named: crate::typed::Named::Machine,
};
/// The table descriptor's version: 1 for the table's life. A collection's own version moves
/// instead (design §4.1); a breaking change is a new role (`registry.2`).
pub const VERSION: u32 = 1;
/// What a person calls the family: the heading the Tables page lists its tables under.
pub const TITLE: &str = "Registry";
/// Automatic records (births, seeds, owned rows) one writer may retain in the table: 128 Macs
/// fill at most 3,072 of its 8,192 lifetime records, leaving the rest to the owner (§3.9).
pub const SELF_QUOTA: usize = 24;

/// The lifecycle words: removal keeps history (§3.2).
pub const LIFE: &[&str] = &["live", "retired", "ended", "merged", "locked"];

/// A lifecycle conflict's provisional word: the most final first, so a retire concurrent with
/// a `live` write (an unsynced import or birth) reads as retired on every Mac, whichever
/// record has the smaller digest.
const LIFE_FINAL_FIRST: &[&str] = &["locked", "merged", "retired", "ended", "live"];

const fn row() -> Column {
    Column::new("row", Ty::Enum(LIFE)).on_conflict(OnConflict::Prefer(LIFE_FINAL_FIRST))
}
/// A row's note, the owner's or a sheet's: free text, refused when it names a secret or holds an
/// address.
const fn note() -> Column {
    Column::new("note", Ty::Text(512)).null()
}
/// The import source whose value won.
const fn src() -> Column {
    Column::new("src", Ty::SrcRef).null()
}
/// Coordinates of imported note cells, the lead's reasoning and losing values: where each came
/// from, never a refused cell's or a losing value's text.
const fn notes_src() -> Column {
    Column::new("notes_src", Ty::List(&Ty::SrcRef, 8))
        .null()
        .writer(Writer::Owner)
}

crate::att_collection!(
    /// The six providers, keyed by the one provider word each (`typed::keys::PROVIDERS`).
    pub PROVIDER = Collection {
        name: "provider",
        version: 1,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "",
        derive: Derive::Word,
        columns: &[
            Column::new("name", Ty::Text(64)).writer(Writer::Seed),
            Column::new("realms", Ty::List(&Ty::Text(32), 4)).writer(Writer::Seed),
            Column::new("site", Ty::Url).null(),
            Column::new("status", Ty::Enum(&["active", "watch", "dropped"])).writer(Writer::Seed),
            row().writer(Writer::Seed),
            note(),
        ],
        unique: &[],
        max_row: 0,
        authority: None,
    }
);

/// Where a CLI's quota figures come from, passively.
pub const READINGS: &[&str] = &["statusline", "rollout", "session-usage", "cli-output"];

crate::att_collection!(
    /// One AI CLI, keyed by its connector id word. The compiled connector table still decides
    /// behaviour; this row describes it.
    pub CLI = Collection {
        name: "cli",
        version: 1,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "",
        derive: Derive::Word,
        columns: &[
            Column::new("provider", Ty::Ref("provider")).writer(Writer::Seed),
            Column::new(
                "pool_realm",
                Ty::Enum(&["claude", "codex", "grok", "kimi:global", "kimi:mainland-cn", "none"])
            )
            .writer(Writer::Seed),
            Column::new(
                "support",
                Ty::Enum(&["pooled", "quota-only", "launch-only", "none"])
            )
            .writer(Writer::Seed),
            Column::new("readings", Ty::Set(&Ty::Enum(READINGS))).writer(Writer::Seed),
            Column::new("roster", Ty::Text(64)).null().writer(Writer::Seed),
            row().writer(Writer::Seed),
            note(),
        ],
        unique: &[],
        max_row: 0,
        authority: None,
    }
);

/// A plan's limit windows.
pub const WINDOWS: &[&str] = &["5h", "7d", "7d-model", "day", "month", "credits"];
/// The units a nominal limit counts.
pub const UNITS: &[&str] = &["pct", "messages", "tokens", "usd-cents"];
/// One nominal limit of a plan: the reading the provider shows is authoritative.
pub const LIMIT: &[Member] = &[
    Member {
        name: "w",
        ty: Ty::Enum(WINDOWS),
        null: false,
    },
    Member {
        name: "model",
        ty: Ty::Text(64),
        null: true,
    },
    Member {
        name: "unit",
        ty: Ty::Enum(UNITS),
        null: false,
    },
    Member {
        name: "nominal",
        ty: Ty::Uint,
        null: true,
    },
];

crate::att_collection!(
    /// A provider's plan, keyed `<provider>-<slug>`.
    pub PLAN = Collection {
        name: "plan",
        version: 1,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "",
        derive: Derive::Composite,
        columns: &[
            Column::new("provider", Ty::Ref("provider")).writer(Writer::Seed),
            Column::new("name", Ty::Text(64)).writer(Writer::Seed),
            Column::new("tier", Ty::Text(32)).null(),
            Column::new("period", Ty::Enum(&["month", "year", "free", "usage"]))
                .null()
                .writer(Writer::Seed),
            Column::new("price_cents", Ty::Uint).null(),
            Column::new("currency", Ty::Text(3)).null(),
            Column::new("limits", Ty::List(&Ty::Struct(LIMIT), 8)).writer(Writer::Seed),
            Column::new("status", Ty::Enum(&["current", "legacy"])).writer(Writer::Seed),
            row().writer(Writer::Seed),
            note(),
        ],
        unique: &[],
        max_row: 0,
        authority: None,
    }
);

crate::att_collection!(
    /// A Mac model, keyed by `hw.model` with `,` as `-` (`Mac16-8`) or `vm-<name>`. The first
    /// Mac to report an unseen model births it (under its quota); the owner fills in the rest.
    pub MODEL = Collection {
        name: "model",
        version: 1,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "",
        derive: Derive::Word,
        columns: &[
            Column::new("marketing", Ty::Text(96)).null(),
            Column::new("form", Ty::Enum(&["laptop", "mini", "studio", "imac", "pro", "vm", "other"]))
                .null(),
            Column::new("chip_family", Ty::Text(32)).null().writer(Writer::Birth),
            Column::new("year", Ty::Uint).null(),
            row().writer(Writer::Birth),
            note(),
        ],
        unique: &[],
        max_row: 0,
        authority: None,
    }
);

/// The machine statuses, safest first for a conflict.
pub const MACHINE_STATUS: &[&str] = &["planned", "active", "parked", "lost"];

macro_rules! machine_collection {
    ($(#[$meta:meta])* $name:ident, $version:expr, $nickname_aliases:expr, $hosts:expr) => {
        crate::att_collection!(
            $(#[$meta])*
            pub $name = Collection {
        name: "machine",
        version: $version,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "mc",
        derive: Derive::BirthSha256,
        columns: &[
            Column::new("label", Ty::Label).claim(ClaimRule::Label),
            Column::new("aliases", Ty::Set(&Ty::Label)),
            Column::new("status", Ty::Enum(MACHINE_STATUS))
                .on_conflict(OnConflict::Prefer(&["lost", "parked", "active", "planned"]))
                .writer(Writer::Birth),
            Column::new(
                "role",
                Ty::Enum(&["worker", "desk", "release", "vault", "lab", "spare"])
            )
            .null(),
            Column::new("model", Ty::Ref("model")).null().writer(Writer::Birth),
            Column::new("make", Ty::Text(32)).null(),
            Column::new("arch", Ty::Text(16)).null(),
            Column::new("os", Ty::Text(32)).null(),
            Column::new("chip", Ty::Text(32)).null(),
            Column::new("ram_gb", Ty::Uint).null(),
            Column::new("disk_gb", Ty::Uint).null(),
            Column::new("nickname", Ty::Text(64)).null(),
            $nickname_aliases,
            Column::new("clis", Ty::Set(&Ty::Ref("cli"))),
            Column::new("location", Ty::Text(64)).null(),
            Column::new("fleet_node", Ty::Hex(32)).null().writer(Writer::Birth),
            Column::new("node", Ty::Hex(16)).null().since(2).claim(ClaimRule::One),
            $hosts,
            // Display-only unless the merged Mac's own facts row names the same target (§6).
            Column::new("merged_into", Ty::Ref("machine")).null(),
            row().writer(Writer::Birth),
            note(),
            src(),
            notes_src(),
        ],
        unique: &[],
        max_row: 0,
        authority: None,
    }
        );
    };
}

machine_collection!(
    /// One physical Mac across relabels and reinstalls, keyed `mc-<16hex>` from its birth
    /// identity, minted once. No serial number and no IOPlatformUUID are stored. Version 4 (the
    /// fourth generation's) holds `nickname_aliases` and `hosts` as text sets, one element per
    /// member (LT-15B), so two Macs adding a member at once keep both and a removal is one
    /// element's.
    MACHINE,
    4,
    Column::new("nickname_aliases", Ty::Set(&Ty::Text(64))),
    Column::new("hosts", Ty::Set(&Ty::Text(64))).writer(Writer::Birth)
);

machine_collection!(
    /// [`MACHINE`] as the registry's first three generations hold it: version 3, `nickname_aliases`
    /// and `hosts` each one whole list, the shape every build that reads those generations knows.
    LIST_MACHINE,
    3,
    Column::new("nickname_aliases", Ty::List(&Ty::Text(64), 8)).null(),
    Column::new("hosts", Ty::List(&Ty::Text(64), 4))
        .null()
        .writer(Writer::Birth)
);

crate::att_collection!(
    /// What a Mac states about itself, at its own Commons machine uuid: the only binding from
    /// an enrollment to a machine row. `hw_hint` is a keyed hint in its own field.
    pub MACHINE_FACTS = Collection {
        name: "machine-facts",
        version: 1,
        kind: Kind::Owned,
        writers: Writers::SelfKey,
        prefix: "",
        derive: Derive::SignerUuid,
        columns: &[
            Column::new("n", Ty::Uint).writer(Writer::Mine),
            Column::new("mac", Ty::Ref("machine")).writer(Writer::Mine),
            Column::new("predecessor", Ty::Hex(32)).null().writer(Writer::Mine),
            Column::new("merged_into", Ty::Ref("machine")).null().writer(Writer::Mine),
            Column::new("label", Ty::Label).writer(Writer::Mine),
            Column::new("hosts", Ty::List(&Ty::Text(64), 4)).writer(Writer::Mine),
            Column::new("hw_model", Ty::Ref("model")).writer(Writer::Mine),
            Column::new("chip", Ty::Text(32)).null().writer(Writer::Mine),
            Column::new("cores_p", Ty::Uint).null().writer(Writer::Mine),
            Column::new("cores_e", Ty::Uint).null().writer(Writer::Mine),
            Column::new("gpu_cores", Ty::Uint).null().writer(Writer::Mine),
            Column::new("ram_gb", Ty::Uint).null().writer(Writer::Mine),
            Column::new("disk_gb", Ty::Uint).null().writer(Writer::Mine),
            Column::new("arch", Ty::Text(16)).null().writer(Writer::Mine),
            Column::new("os_major", Ty::Uint).null().writer(Writer::Mine),
            Column::new("hw_hint", Ty::Hex(16)).writer(Writer::Mine),
            Column::new("node", Ty::Hex(32)).null().writer(Writer::Mine),
            Column::new("seen", Ty::Day).writer(Writer::Mine),
            Column::new("at", Ty::Time).writer(Writer::Mine),
        ],
        unique: &[],
        max_row: 4096,
        authority: None,
    }
);

/// The machine a `machine-facts` row says its Mac is: the one its own `merged_into` names (an
/// owner merge it acknowledged, or its fold of a successor's birth, lab model §6), else the one
/// its `mac` binds. Every reader that asks which machine an enrollment is asks this.
pub fn bound_machine(facts: &Value) -> Option<&str> {
    facts
        .get("merged_into")
        .and_then(Value::as_str)
        .or_else(|| facts.get("mac").and_then(Value::as_str))
}

/// Every machine a `machine-facts` row holds, each once: the one it is ([`bound_machine`]) and
/// the `mac` it was bound to before a merge, so neither looks free to another Mac's adoption.
pub fn held_machines(facts: &Value) -> Vec<&str> {
    let mut held: Vec<&str> = ["merged_into", "mac"]
        .into_iter()
        .filter_map(|column| facts.get(column).and_then(Value::as_str))
        .collect();
    held.dedup();
    held
}

/// The `address` collection of a registry generation: version 3 (the third generation's)
/// with its secret columns typed as this build reads them, or version 2, sealed, as builds up to
/// 0.200.0 read the first and second generations.
macro_rules! address_collection {
    ($(#[$meta:meta])* $name:ident, $version:expr, $password:expr) => {
        crate::att_collection!(
            $(#[$meta])*
            pub $name = Collection {
        name: "address",
        version: $version,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "ad",
        derive: Derive::BirthSha256,
        columns: &[
            Column::new("addr", Ty::Email).writer(Writer::Birth),
            Column::new("domain", Ty::Text(253)).writer(Writer::Birth),
            Column::new("kind", Ty::Enum(&["alias", "standalone", "unknown"])).null(),
            Column::new(
                "status",
                Ty::Enum(&["active", "suspended", "disabled", "unknown"])
            )
            .null(),
            Column::new(
                "provider",
                Ty::Enum(&["proton", "gmail", "google", "outlook", "privateemail", "other"])
            )
            .null(),
            Column::new("phone", Ty::Ref("phone")).null(),
            Column::new("protected", Ty::Bool).null(),
            // The owner's word: wins over discovery.
            Column::new("mailbox", Ty::Ref("mailbox")).null(),
            // A guess, used only when nothing else exists.
            Column::new("mailbox_hint", Ty::Ref("mailbox")).null(),
            Column::new("alias_of", Ty::Ref("address")).null(),
            Column::new(
                "origin",
                Ty::Enum(&[
                    "sheet",
                    "logins-csv",
                    "emails-csv",
                    "harness",
                    "discovered",
                    "owner",
                    "fleet-db",
                ])
            )
            .writer(Writer::Birth),
            row().writer(Writer::Birth),
            note(),
            src(),
            notes_src(),
            Column::new("password", $password).null().since(2),
        ],
        unique: &[&["addr"]],
        max_row: 0,
        authority: None,
    }
        );
    };
}

address_collection!(
    /// A login or mail address, keyed `ad-<16hex>` from the address it was born with, which is
    /// the one value typed `email` in the registry. A rename keeps the key, so a row is found
    /// by what its `addr` says now ([`AddressIndex`]). Version 2 adds its mailbox `password`;
    /// version 3 makes it the name of its secret-store entry, which a `protected` row never names.
    ADDRESS,
    3,
    Ty::SecretRef
);

address_collection!(
    /// [`ADDRESS`] as the registry's first and second generations hold it: version 2, each secret
    /// column a sealed token ([`Ty::Secret`]), the shape builds up to 0.200.0 read.
    SEALED_ADDRESS,
    2,
    Ty::Secret
);

/// Which `address` row holds an address now. A rename keeps the row's `BirthSha256` key
/// (docs/DESIGN-alab-one-mesh-2026-11-05.md §11 E1b), so an address's row is the one whose
/// `addr` cell says it now, not the key the address derives ([`keys::address_pk`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AddressHolder<'a> {
    /// The one live row whose `addr` says it, else, with none live, the one retired row that
    /// does.
    Row(&'a str),
    /// Several live rows say it (or, with none live, several retired ones): nothing picks one.
    /// `check` reports live rows whose unconflicted `addr` cells hold the same text
    /// (`typed::check::Finding::Unique`); retired rows, a conflicted `addr`'s candidates and two
    /// spellings of one address it does not.
    Several(Vec<&'a str>),
    /// No row says it. `derived` is its birth key; `taken`, whether a row holds that key anyway
    /// (one renamed away from it, or one with no `addr`), so no row can be born for it.
    Free { derived: String, taken: bool },
}

/// One projection's `address` rows by what their `addr` cells say now, built once: what every
/// lookup of a row by its address reads (a `set` or a verb naming it, discovery's list, a
/// `+tag`'s base, an alias's chain, the importer's match). It owns its strings, so a view keeps
/// it beside the projection it was built from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AddressIndex {
    /// Each address in the claims spelling ([`keys::address`]) to the rows any candidate of
    /// whose `addr` cell says it, as `(retired, key)`: live rows first, then by key. A
    /// conflicted `addr` is found under each of its values.
    by_addr: BTreeMap<String, Vec<(bool, String)>>,
    /// Every row key the projection holds, for [`AddressHolder::Free`]'s `taken`.
    keys: BTreeSet<String>,
}

impl AddressIndex {
    /// Index `addresses`, the `address` projection.
    pub fn of(addresses: &cell::Projection) -> Self {
        let mut by_addr: BTreeMap<String, Vec<(bool, String)>> = BTreeMap::new();
        for (pk, row) in &addresses.rows {
            let retired = !row.live();
            let candidates = row
                .fields
                .get("addr")
                .map_or(&[][..], cell::Field::candidates);
            if let [only] = candidates {
                if let Some(address) = only.value.x.as_str().and_then(|t| keys::address(t).ok()) {
                    by_addr
                        .entry(address)
                        .or_default()
                        .push((retired, pk.clone()));
                }
            } else if !candidates.is_empty() {
                let said: BTreeSet<String> = candidates
                    .iter()
                    .filter_map(|c| c.value.x.as_str())
                    .filter_map(|text| keys::address(text).ok())
                    .collect();
                for address in said {
                    by_addr
                        .entry(address)
                        .or_default()
                        .push((retired, pk.clone()));
                }
            }
        }
        for holders in by_addr.values_mut() {
            holders.sort_unstable();
        }
        Self {
            by_addr,
            keys: addresses.rows.keys().cloned().collect(),
        }
    }

    /// Every row any candidate of whose `addr` says `raw`, live rows first, then by key; none
    /// for a text that is not an address.
    pub fn saying(&self, raw: &str) -> Vec<&str> {
        keys::address_cow(raw)
            .ok()
            .and_then(|address| self.by_addr.get(address.as_ref()))
            .map(|holders| holders.iter().map(|(_, pk)| pk.as_str()).collect())
            .unwrap_or_default()
    }

    /// Whether the projection holds row `pk`, in any column.
    pub fn holds(&self, pk: &str) -> bool {
        self.keys.contains(pk)
    }

    /// The row that says `raw` now ([`AddressHolder`]).
    ///
    /// # Errors
    /// `raw` is not an address; the refusal never quotes it.
    pub fn holder(&self, raw: &str) -> Result<AddressHolder<'_>> {
        let address = keys::address_cow(raw)?;
        let holders = self
            .by_addr
            .get(address.as_ref())
            .map_or(&[][..], Vec::as_slice);
        let live_len = holders.iter().take_while(|(retired, _)| !*retired).count();
        match live_len {
            1 => return Ok(AddressHolder::Row(&holders[0].1)),
            n if n > 1 => {
                return Ok(AddressHolder::Several(
                    holders[..n].iter().map(|(_, pk)| pk.as_str()).collect(),
                ));
            }
            _ => {}
        }
        match holders {
            [(_, one)] => return Ok(AddressHolder::Row(one)),
            [_, _, ..] => {
                return Ok(AddressHolder::Several(
                    holders.iter().map(|(_, pk)| pk.as_str()).collect(),
                ));
            }
            [] => {}
        }
        let derived = keys::address_pk(&address)?;
        let taken = self.keys.contains(&derived);
        Ok(AddressHolder::Free { derived, taken })
    }

    /// [`Self::holder`]'s one row, else `None`: several rows, none, or not an address.
    pub fn row(&self, raw: &str) -> Option<&str> {
        match self.holder(raw).ok()? {
            AddressHolder::Row(pk) => Some(pk),
            AddressHolder::Several(_) | AddressHolder::Free { .. } => None,
        }
    }

    /// The row an import of `raw` writes to: the row its derived key holds, whatever that row
    /// says now (the import matches rows by key, and where the table and the sheet disagree the
    /// table wins), else [`Self::holder`]'s.
    ///
    /// # Errors
    /// `raw` is not an address; the refusal never quotes it.
    pub fn matched(&self, raw: &str) -> Result<AddressHolder<'_>> {
        let derived = keys::address_pk(raw)?;
        match self.keys.get(&derived) {
            Some(pk) => Ok(AddressHolder::Row(pk)),
            None => self.holder(raw),
        }
    }
}

/// The registry's rules on what the owner writes (`att::Compiled::guard`), over `rows` (each
/// row's changes, one record each) and the table's `cells` as they are under the writer, read as
/// the table reads once the save lands ([`landed`]), so the table stays one an address lookup and
/// an account's key can read whichever path wrote it: a grid save, `aterm db emails set`, an
/// owner verb, a revert.
///
/// - An `address.addr` write is refused when another row says that address once the save lands,
///   when the address's own birth key is another row's (a rename back to the row's birth address
///   always passes), and when it renames a row a live account names as its `login` or
///   `identity` once the save lands, since the account's key and the harness's names for it
///   derive from the address it signs in as ([`keys::account_pk`]); it carries the row's
///   `domain`, the part after its `@`, in the same record. An `address.domain` write that
///   disagrees with the row's `addr` is refused.
/// - A write of an account's `realm`, `login` or `identity`, or of its lifecycle to `live`, that
///   leaves the account live is refused unless the account's key is the one its realm and the
///   address it signs in as derive ([`signs_in`]: its identity row's address, else its login
///   row's, as those rows say once the save lands) and no other live account of that realm signs
///   in as that address. So an account keyed from an address its row no longer says (retired for
///   a rename and brought back, or renamed past by two Macs' concurrent edits) is never made live
///   again, and no second account is added beside it.
/// - An account's `realm` holds the realm's own word ([`keys::REALMS`]): `oai`, `openai` or
///   `Claude` is refused, naming the word to write, since a Mac matches the cell exactly (the
///   harness's rotation, an older build's sightings) and would not know the row.
/// - An account's `provider` is its realm's ([`keys::provider_of_realm`]): a save that writes
///   either and leaves them disagreeing is refused, naming the provider to write.
/// - An account's `label` is refused where it makes no slug ([`keys::owner_slug`]: its handle
///   empty or too long, or a slug the guards withhold), and where its slug is one another
///   account goes by now, or another label of the save makes: an owner's label never moves the
///   name a person or a script already uses for another account, so of two owners' labels that
///   make one slug only concurrent writes and older builds leave a pair, which the lowest
///   operation digest settles (`keys::account_slugs`).
///
/// Every refusal names row keys and columns, never an address.
///
/// # Errors
/// A change of an address or account row is not a typed cell.
pub fn guard(rows: &mut [Vec<Change>], cells: &Cells) -> Result<Option<Refused>> {
    // Each `addr` this save writes, by row: its record, the address (claims spelling) and value.
    let mut renamed: BTreeMap<String, (usize, String, CellValue)> = BTreeMap::new();
    let mut domains: Vec<(usize, String, Value)> = Vec::new();
    // Each account this save writes a column of its key in, or makes live: the first such column.
    let mut keyed: BTreeMap<String, String> = BTreeMap::new();
    // Each account label this save writes, by row.
    let mut labels: BTreeMap<String, String> = BTreeMap::new();
    // Each account this save writes its provider or realm in.
    let mut providers: BTreeSet<String> = BTreeSet::new();
    for (record, changes) in rows.iter().enumerate() {
        for change in changes {
            let account = change.collection == ACCOUNT.name;
            if !account && change.collection != ADDRESS.name {
                continue;
            }
            let key = CellKey::parse(&change.key)?;
            // A restored absence (a revert of a birth) asks for no rule.
            let Some(value) = change.value.as_ref().map(CellValue::decode).transpose()? else {
                continue;
            };
            if account {
                match (key.col, value.x.as_str()) {
                    ("realm", Some(text)) if keys::canonical_realm(text) != Some(text) => {
                        return Ok(Some(refused(&ACCOUNT, key.pk, "realm", realm_rule(text))));
                    }
                    ("label", Some(text)) => {
                        labels.insert(key.pk.to_owned(), text.to_owned());
                    }
                    ("provider" | "realm", _) => {
                        providers.insert(key.pk.to_owned());
                    }
                    _ => {}
                }
                let makes_live =
                    key.col == crate::typed::LIFECYCLE && value.x.as_str() == Some("live");
                if makes_live || ["realm", "login", "identity"].contains(&key.col) {
                    keyed
                        .entry(key.pk.to_owned())
                        .or_insert_with(|| key.col.to_owned());
                }
                continue;
            }
            match key.col {
                "addr" => {
                    if let Some(text) = value.x.as_str() {
                        let Ok(address) = keys::address(text) else {
                            return Ok(Some(refused(
                                &ADDRESS,
                                key.pk,
                                "addr",
                                "this row says no address".to_owned(),
                            )));
                        };
                        renamed.insert(key.pk.to_owned(), (record, address, value));
                    }
                }
                "domain" => domains.push((record, key.pk.to_owned(), value.x)),
                _ => {}
            }
        }
    }
    if renamed.is_empty()
        && domains.is_empty()
        && keyed.is_empty()
        && labels.is_empty()
        && providers.is_empty()
    {
        return Ok(None);
    }
    let addresses = cell::project(&ADDRESS, cells);
    let landed = landed(rows, cells);
    let after = cell::project(&ADDRESS, &landed);
    let index = AddressIndex::of(&after);
    let accounts = cell::project(&ACCOUNT, &landed);
    for (pk, (record, address, value)) in &renamed {
        let refuse = |rule: String| Ok(Some(refused(&ADDRESS, pk, "addr", rule)));
        if let Some(other) = index
            .saying(address)
            .into_iter()
            .find(|other| *other != pk.as_str())
        {
            return refuse(format!(
                "email row {} holds that address; name that row, or rename it first",
                safe(other)
            ));
        }
        let derived = keys::address_pk(address)?;
        if derived != *pk && after.rows.contains_key(&derived) {
            return refuse(format!(
                "that address's birth row {} says another address now; rename that row back \
                 instead",
                safe(&derived)
            ));
        }
        let said = addresses
            .rows
            .get(pk)
            .and_then(|row| row.fields.get("addr"))
            .map_or(&[][..], cell::Field::candidates);
        let renames = !said.is_empty()
            && !said
                .iter()
                .filter_map(|c| c.value.x.as_str())
                .any(|text| keys::address_cow(text).ok().as_deref() == Some(address));
        if renames
            && let Some((account, _)) = accounts.rows.iter().find(|(_, row)| {
                row.live()
                    && ["login", "identity"].into_iter().any(|column| {
                        row.fields.get(column).is_some_and(|field| {
                            field
                                .candidates()
                                .iter()
                                .any(|c| c.value.x.as_str() == Some(pk.as_str()))
                        })
                    })
            })
        {
            return refuse(format!(
                "account {} signs in with this row, and its key derives from the address: \
                 retire that account (`aterm db alab retire account …`) and add it again under \
                 the new address",
                safe(account)
            ));
        }
        // The domain follows, in the same record.
        let domain = Value::from(address.split_once('@').map_or("", |(_, d)| d));
        let follows = CellValue {
            x: domain.clone(),
            ..value.clone()
        };
        let changes = &mut rows[*record];
        let at = format!("{pk}.domain");
        match changes
            .iter_mut()
            .find(|c| c.collection == ADDRESS.name && c.key == at)
        {
            Some(change) => change.value = Some(follows.encode()),
            None => {
                let held = addresses
                    .rows
                    .get(pk)
                    .and_then(|row| row.fields.get("domain"))
                    .is_some_and(
                        |field| matches!(field.candidates(), [only] if only.value.x == domain),
                    );
                if !held {
                    changes.push(cell::change(
                        &ADDRESS,
                        CellKey {
                            pk,
                            col: "domain",
                            elem: None,
                        },
                        &follows,
                    )?);
                }
            }
        }
    }
    for (_, pk, x) in domains {
        if renamed.contains_key(&pk) {
            continue;
        }
        let says = addresses
            .rows
            .get(&pk)
            .and_then(|row| row.get("addr"))
            .and_then(Value::as_str)
            .and_then(|text| keys::address_cow(text).ok());
        if let Some(address) = says
            && x.as_str() != address.split_once('@').map(|(_, d)| d)
        {
            return Ok(Some(refused(
                &ADDRESS,
                &pk,
                "domain",
                "the domain follows the row's addr; edit addr instead".to_owned(),
            )));
        }
    }
    for (pk, column) in &keyed {
        let Some(row) = accounts.rows.get(pk).filter(|row| row.live()) else {
            continue;
        };
        let refuse = |rule: String| Ok(Some(refused(&ACCOUNT, pk, column, rule)));
        let Some((realm, email)) = signs_in(row, &after) else {
            return refuse(
                "a live account names a realm and a login row that says one address; fill them \
                 in, or retire the row"
                    .to_owned(),
            );
        };
        let derived = keys::account_pk(realm, &email)?;
        if derived != *pk {
            return refuse(format!(
                "this row's key derives from an address it no longer signs in as (the key now \
                 is {}): add the account under the address it signs in as (`aterm db alab add \
                 account …`) instead",
                safe(&derived)
            ));
        }
        if let Some((other, _)) = accounts.rows.iter().find(|(other, row)| {
            *other != pk
                && row.live()
                && signs_in(row, &after).is_some_and(|(r, e)| r == realm && e == email)
        }) {
            return refuse(format!(
                "live account {} of that realm signs in as this address too, keyed from an \
                 address its rows no longer say; retire that row first",
                safe(other)
            ));
        }
    }
    for pk in &providers {
        let Some(row) = accounts.rows.get(pk) else {
            continue;
        };
        let one = |column: &str| {
            row.fields
                .get(column)
                .filter(|field| !field.conflicted())
                .and_then(cell::Field::value)
                .and_then(Value::as_str)
        };
        if let (Some(provider), Some(realm)) = (one("provider"), one("realm"))
            && let Some(own) = keys::provider_of_realm(realm)
            && provider != own
        {
            return Ok(Some(refused(
                &ACCOUNT,
                pk,
                "provider",
                format!("this realm's provider is `{own}`; write that"),
            )));
        }
    }
    if let Some(refusal) = label_refusal(&labels, &accounts, cells) {
        return Ok(Some(refusal));
    }
    Ok(None)
}

/// Why an account's `realm` cell refuses `text`: it holds the realm's own word. The typed text is
/// never quoted.
fn realm_rule(text: &str) -> String {
    match keys::canonical_realm(text) {
        Some(realm) => format!("write this realm as `{realm}`, the word every Mac reads it by"),
        None => format!(
            "no provider realm goes by that word; write one of {}",
            keys::REALMS.join(", ")
        ),
    }
}

/// The refusal of the first account label of `labels` that names no account, or whose slug is
/// another account's slug or a label a Mac's roster row of another account carries ([`guard`]),
/// over `accounts` as they read once the save lands and the table's `cells` as they were before
/// it. A label on a row whose realm does not read as one value is judged once the realm does.
fn label_refusal(
    labels: &BTreeMap<String, String>,
    accounts: &cell::Projection,
    cells: &Cells,
) -> Option<Refused> {
    if labels.is_empty() {
        return None;
    }
    let names = super::account_names::of_cells(cells);
    let realm_of = |pk: &str| {
        accounts
            .rows
            .get(pk)
            .and_then(|row| row.fields.get("realm"))
            .filter(|field| !field.conflicted())
            .and_then(cell::Field::value)
            .and_then(Value::as_str)
    };
    // Every slug another account holds (its slug now, and its 16-hex one), and every label a
    // Mac's roster row of it may carry (its legacy label, and the default an earlier build gave
    // rows), which the fleet fold resolves to it as it resolves a slug (`fleet::fold`): a slug
    // spelled like one would make that word name neither account.
    let mut held: BTreeMap<String, (&str, bool)> = BTreeMap::new();
    for (pk, theirs) in &names {
        held.insert(theirs.slug.clone(), (pk.as_str(), false));
        if let Some(realm) = realm_of(pk) {
            held.insert(keys::reserved_slug(realm, pk), (pk.as_str(), false));
        }
    }
    for (pk, row) in &accounts.rows {
        let legacy = row
            .fields
            .get("legacy_label")
            .filter(|field| !field.conflicted())
            .and_then(cell::Field::value)
            .and_then(Value::as_str)
            .map(str::to_owned);
        let earlier = realm_of(pk).and_then(|realm| keys::earlier_row_label(realm, pk));
        for label in legacy.into_iter().chain(earlier) {
            held.entry(label).or_insert((pk.as_str(), true));
        }
    }
    let mut making: BTreeMap<String, &str> = BTreeMap::new();
    for (pk, label) in labels {
        let Some(realm) = realm_of(pk) else {
            continue;
        };
        let refuse = |rule: String| Some(refused(&ACCOUNT, pk, "label", rule));
        let Some(slug) = keys::owner_slug(realm, label) else {
            return refuse(format!(
                "this label names no account: its slug is the provider word, a `-` and the \
                 label's letters and digits, at most {} of them and never shaped like a \
                 credential (an xAI account's at most 19); write a shorter label",
                keys::MAX_HANDLE
            ));
        };
        match held.get(&slug).filter(|(other, _)| *other != pk.as_str()) {
            Some((other, false)) => {
                return refuse(format!(
                    "account {} goes by `{slug}` now; write another label, or rename that \
                     account first",
                    safe(other)
                ));
            }
            Some((other, true)) => {
                return refuse(format!(
                    "a Mac's roster row of account {} is labelled `{slug}`; write another label",
                    safe(other)
                ));
            }
            None => {}
        }
        if let Some(other) = making.insert(slug.clone(), pk.as_str()) {
            return refuse(format!(
                "account {} is labelled `{slug}` in this save too; write another label",
                safe(other)
            ));
        }
    }
    None
}

/// A refusal of `collection.column` in row `pk` ([`crate::typed::Refused`]).
fn refused(collection: &Collection, pk: &str, column: &str, rule: String) -> Refused {
    Refused {
        collection: collection.name,
        pk: pk.to_owned(),
        column: column.to_owned(),
        rule,
    }
}

/// `cells`' address and account cells as they read once `rows` land: each cell the save writes
/// holds that write alone, since an owner write follows every candidate it was made over and
/// replaces them, and a restored absence reads as empty.
fn landed(rows: &[Vec<Change>], cells: &Cells) -> Cells {
    let ours = |collection: &str| collection == ADDRESS.name || collection == ACCOUNT.name;
    let mut landed: Cells = cells
        .iter()
        .filter(|((collection, _), _)| ours(collection))
        .map(|(key, cell)| (key.clone(), cell.clone()))
        .collect();
    for change in rows.iter().flatten().filter(|c| ours(&c.collection)) {
        landed.insert(
            (change.collection.clone(), change.key.clone()),
            crate::database::Cell {
                stopped: false,
                candidates: vec![crate::database::Candidate {
                    operation: [0; 32],
                    value: change.value.clone().unwrap_or(Value::Null),
                }],
            },
        );
    }
    landed
}

/// The realm and the address account `row` signs in as ([`keys::claims_email`]): its identity
/// row's `addr`, else its login row's, each one unconflicted value of `addresses`; `None` where
/// either is missing, conflicted or no address, or the realm is no known one.
fn signs_in(row: &cell::Row, addresses: &cell::Projection) -> Option<(&'static str, String)> {
    fn one<'a>(row: &'a cell::Row, column: &str) -> Option<&'a str> {
        row.fields
            .get(column)
            .filter(|field| !field.conflicted())
            .and_then(cell::Field::value)
            .and_then(Value::as_str)
    }
    let address = |pk: &str| one(addresses.rows.get(pk)?, "addr");
    let realm = keys::canonical_realm(one(row, "realm")?)?;
    let login = address(one(row, "login")?)?;
    let identity = match one(row, "identity") {
        Some(pk) => Some(address(pk)?),
        None => None,
    };
    let email = keys::claims_email(identity, Some(login)).ok()?;
    Some((realm, email))
}

/// A row key as a refusal prints it.
fn safe(pk: &str) -> &str {
    keys::safe_key(pk)
}

/// The `mailbox` collection of a registry generation: version 4 (the fourth generation's), its
/// secret columns naming secret-store entries and its domains a text set; version 3 (the
/// third's), the same with the domains one list; or version 2, sealed, as builds up to 0.200.0
/// read the first and second generations.
macro_rules! mailbox_collection {
    ($(#[$meta:meta])* $name:ident, $version:expr, $password:expr, $totp:expr, $domains:expr) => {
        crate::att_collection!(
            $(#[$meta])*
            pub $name = Collection {
        name: "mailbox",
        version: $version,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "mb",
        derive: Derive::Composite,
        columns: &[
            // A plain name the owner picks (`proton-1`), never an address.
            Column::new("label", Ty::Text(32)),
            Column::new("service", Ty::Enum(&["proton", "gmail", "outlook", "imap", "other"])),
            Column::new(
                "code_path",
                Ty::Enum(&["proton-api", "imap", "webmail", "relay", "manual"])
            ),
            Column::new("vault_ref", Ty::SecretRef).null(),
            Column::new("webmail", Ty::Url).null(),
            row(),
            note(),
            Column::new("login", Ty::Ref("address")).null().since(2),
            Column::new("mailbox_password", $password).null().since(2),
            Column::new("totp", $totp).null().since(2),
            Column::new("receives", Ty::Set(&Ty::Ref("address"))).since(2),
            $domains,
            // `imaps://imap.example.test:993`, a local Bridge's `imap://127.0.0.1:1143`, a mail
            // API's `http://127.0.0.1:8025`: `Url` takes only `https`, and the importer refuses
            // one naming a user.
            Column::new("imap", Ty::Text(512)).null().since(2),
            Column::new("http", Ty::Text(512)).null().since(2),
        ],
        unique: &[&["label"]],
        max_row: 0,
        authority: None,
    }
        );
    };
}

mailbox_collection!(
    /// A mailbox that receives codes, keyed `mb-<16hex>` from its login address's key
    /// ([`keys::mailbox_pk`]). Version 2 adds `login`, that address row, whose own `password`
    /// is the mailbox's login password (one fact, one cell); `mailbox_password` (a two-password
    /// webmail's second) and `totp` (the login's RFC 6238 seed or `otpauth://` URI), which version 3
    /// makes the names of their secret-store entries; and what `ai-mailbox.toml`'s `receives` said it receives: `receives`, the exact
    /// addresses, and `receives_domains`, each `*@<domain>` as its domain and `*` as itself
    /// (`harness/mailbox_routes.rs`); and its `imap_url` and `http_url`, `imap` and `http`, which
    /// hold no user part. The version-1 `vault_ref` stays unwritten. Version 4 holds
    /// `receives_domains` as a text set, one element per domain (LT-15B).
    MAILBOX,
    4,
    Ty::SecretRef,
    Ty::SecretRef,
    Column::new("receives_domains", Ty::Set(&Ty::Text(253))).since(2)
);

mailbox_collection!(
    /// [`MAILBOX`] as the registry's third generation holds it: version 3, `receives_domains` one
    /// whole list, the shape builds that read that generation know.
    MAILBOX_3,
    3,
    Ty::SecretRef,
    Ty::SecretRef,
    Column::new("receives_domains", Ty::List(&Ty::Text(253), 64))
        .null()
        .since(2)
);

mailbox_collection!(
    /// [`MAILBOX`] as the registry's first and second generations hold it: version 2, each secret
    /// column a sealed token ([`Ty::Secret`]), the shape builds up to 0.200.0 read.
    SEALED_MAILBOX,
    2,
    Ty::Secret,
    Ty::Secret,
    Column::new("receives_domains", Ty::List(&Ty::Text(253), 64))
        .null()
        .since(2)
);

crate::att_collection!(
    /// The address list a Mac read inside a signed-in mailbox it had open anyway, keyed
    /// `<mailbox pk>.<signer uuid32>`: address keys only.
    pub MAILBOX_OBS = Collection {
        name: "mailbox-obs",
        version: 1,
        kind: Kind::Owned,
        writers: Writers::SelfSuffix,
        prefix: "",
        derive: Derive::Composite,
        columns: &[
            Column::new("n", Ty::Uint).writer(Writer::Mine),
            Column::new("mailbox", Ty::Ref("mailbox")).writer(Writer::Mine),
            Column::new(
                "method",
                Ty::Enum(&["proton-api", "bridge", "imap-delivered-to", "webmail"])
            )
            .writer(Writer::Mine),
            Column::new("complete", Ty::Bool).writer(Writer::Mine),
            Column::new("addresses", Ty::List(&Ty::Ref("address"), 512)).writer(Writer::Mine),
            Column::new("at", Ty::Time).writer(Writer::Mine),
        ],
        unique: &[],
        max_row: 16 * 1024,
        authority: None,
    }
);

crate::att_collection!(
    /// Which account a Mac's tabs really run, as their CLIs' login files say, keyed
    /// `<account pk>.<signer uuid32>` (`docs/DESIGN-account-discovery-2026-11-09.md`): the
    /// account, the CLI, whether a tab runs it, how many, since when, when last written, and
    /// where its login was read. Each Mac writes only its own rows. Version 2 adds the source
    /// word `browser` (design §3.8: a new enum word is version n+1).
    pub ACCOUNT_OBS = Collection {
        name: "account-obs",
        version: 2,
        kind: Kind::Owned,
        writers: Writers::SelfSuffix,
        prefix: "",
        derive: Derive::Composite,
        columns: &[
            Column::new("n", Ty::Uint).writer(Writer::Mine),
            Column::new("account", Ty::Ref("account")).writer(Writer::Mine),
            Column::new("cli", Ty::Enum(super::observe::CLIS)).writer(Writer::Mine),
            Column::new("state", Ty::Enum(super::observe::STATES)).writer(Writer::Mine),
            Column::new("tabs", Ty::Uint).writer(Writer::Mine),
            Column::new("first_at", Ty::Time).writer(Writer::Mine),
            Column::new("at", Ty::Time).writer(Writer::Mine),
            Column::new("src", Ty::Enum(super::observe::SOURCES)).writer(Writer::Mine),
        ],
        unique: &[],
        max_row: 1024,
        authority: None,
    }
);

/// The `phone` collection of a registry generation: version 3 (the third generation's)
/// with its secret columns typed as this build reads them, or version 2, sealed, as builds up to
/// 0.200.0 read the first and second generations.
macro_rules! phone_collection {
    ($(#[$meta:meta])* $name:ident, $version:expr, $number:expr) => {
        crate::att_collection!(
            $(#[$meta])*
            pub $name = Collection {
        name: "phone",
        version: $version,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "ph",
        derive: Derive::Random,
        columns: &[
            Column::new("last4", Ty::Digits(4)),
            Column::new("label", Ty::Text(64)).null(),
            Column::new("carrier", Ty::Text(64)).null(),
            Column::new("line_type", Ty::Text(32)).null(),
            Column::new("device", Ty::Text(64)).null(),
            Column::new("vault_ref", Ty::SecretRef).null(),
            row(),
            note(),
            src(),
            notes_src(),
            Column::new("number", $number).null().since(2),
        ],
        unique: &[],
        max_row: 0,
        authority: None,
    }
        );
    };
}

phone_collection!(
    /// A phone, keyed `ph-<16 random hex>`, never a digest of the number: with the country
    /// code and the last four digits known, an unkeyed hash falls to about 10⁶ guesses.
    /// Version 2 adds the full number; version 3 holds it as ordinary data (`number`, typed
    /// `phone`): the importer finds a phone by it, and `check` lists two rows holding one number.
    PHONE,
    3,
    Ty::Phone
);

phone_collection!(
    /// [`PHONE`] as the registry's first and second generations hold it: version 2, each secret
    /// column a sealed token ([`Ty::Secret`]), the shape builds up to 0.200.0 read.
    SEALED_PHONE,
    2,
    Ty::Secret
);

/// The account statuses, safest first for a conflict.
pub const ACCOUNT_STATUS: &[&str] = &["pending", "active", "paused", "broken", "banned"];
/// The services an account can run.
pub const SERVICES: &[&str] = &[
    "claude-code",
    "claude-web",
    "codex",
    "chatgpt",
    "agy",
    "gemini-web",
    "grok",
    "kimi-code",
    "meta-web",
    "muse-code",
];

crate::att_collection!(
    /// A provider account: one claims identity at one provider realm, keyed by **the canonical
    /// account key** (`typed::keys::account_pk`), so the importer, the harness, the claims rows
    /// and journal events all name one row without a lookup.
    ///
    /// Version 2 (docs/DESIGN-shared-files-2026-11-11.md §5, §10) puts the collection under the
    /// authority rules (`typed::authority`): a Mac that observes an account no row names births it
    /// (provider, realm, login, `status=pending` and its lifecycle, in one record), and every
    /// other cell is the owner's; provider and realm are fixed at birth; `label`, the handle the
    /// account's slug takes, only an owner's verb writes, so the default `<cli>-<6 hex>` label the
    /// importer and an older observer stored names nothing, at whatever version they wrote it;
    /// and two live accounts of one realm under one label are a finding.
    pub ACCOUNT = Collection {
        name: "account",
        version: 2,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "ac",
        derive: Derive::BirthSha256,
        columns: &[
            Column::new("provider", Ty::Ref("provider"))
                .writer(Writer::Birth)
                .fixed(),
            Column::new("realm", Ty::Text(32)).writer(Writer::Birth).fixed(),
            Column::new("login", Ty::Ref("address")).writer(Writer::Birth),
            // The provider identity when the login is an alias.
            Column::new("identity", Ty::Ref("address")).null(),
            Column::new(
                "method",
                Ty::Enum(&[
                    "oauth",
                    "device",
                    "browser",
                    "password",
                    "email-link",
                    "passkey",
                    "api-key",
                ])
            )
            .null(),
            Column::new("status", Ty::Enum(ACCOUNT_STATUS))
                .on_conflict(OnConflict::Prefer(&["banned", "broken", "paused", "pending", "active"]))
                .writer(Writer::Birth),
            // The handle the owner names the account by, which its slug takes after the provider
            // word (`alab::account_names`); empty, the slug's handle derives from the address. A
            // word shaped like a machine name is allowed (`guard::handle`).
            Column::new("label", Ty::Handle(keys::MAX_HANDLE))
                .null()
                .writer(Writer::Verb),
            Column::new("services", Ty::Set(&Ty::Enum(SERVICES))),
            Column::new("plan", Ty::Ref("plan")).null(),
            Column::new("plan_since", Ty::Day).null(),
            Column::new("renews", Ty::Day).null(),
            // References only (`billing::Reference`).
            Column::new("billing", Ty::Json(1024)).null(),
            Column::new("browser", Ty::Text(32)).null(),
            Column::new("region", Ty::Enum(&["global", "mainland-cn"])).null(),
            Column::new("flow", Ty::Enum(&["device", "browser", "window"])).null(),
            Column::new("vault_ref", Ty::SecretRef).null(),
            // The harness label at migration (`m8-2`, `m106-m38`, `kimi-m16`): what a Mac already
            // calls this row, so its login directory never moves.
            Column::new("legacy_label", Ty::Text(64)).null(),
            row().writer(Writer::Birth),
            note(),
            src(),
            notes_src(),
        ],
        // Two live accounts of one realm under one label are a finding; the label is never a
        // key, and slugs, which start with the realm's provider word, are unique across the lab.
        unique: &[&["realm", "label"]],
        max_row: 0,
        authority: Some(2),
    }
);

crate::att_collection!(
    /// A provider workspace under one account (Codex/ChatGPT workspaces under one login), keyed
    /// from the account and the observed `AccountKey`.
    pub SUBACCOUNT = Collection {
        name: "subaccount",
        version: 1,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "sa",
        derive: Derive::BirthSha256,
        columns: &[
            Column::new("account", Ty::Ref("account")),
            Column::new("key", Ty::Hex(12)),
            Column::new("label", Ty::Text(32)).null(),
            row(),
            note(),
        ],
        unique: &[],
        max_row: 0,
        authority: None,
    }
);

/// The sign-in methods, the long words of `typed::keys::METHOD_CODES`.
pub const METHODS: &[&str] = &[
    "email-code",
    "magic-link",
    "password",
    "totp",
    "google-sso",
    "apple-sso",
    "sms",
    "device-code",
    "passkey",
];

crate::att_collection!(
    /// How an account signs in and where its codes arrive, keyed `ar-<account 16hex>-<code>`.
    pub AUTH_ROUTE = Collection {
        name: "auth-route",
        version: 1,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "ar",
        derive: Derive::Composite,
        columns: &[
            Column::new("account", Ty::Ref("account")),
            Column::new("method", Ty::Enum(METHODS)),
            Column::new("address", Ty::Ref("address")).null(),
            Column::new("phone", Ty::Ref("phone")).null(),
            Column::new("vault_ref", Ty::SecretRef).null(),
            Column::new("rank", Ty::Uint).null(),
            Column::new("requirements", Ty::Text(256)).null(),
            row(),
            note(),
        ],
        unique: &[],
        max_row: 0,
        authority: None,
    }
);

crate::att_collection!(
    /// One machine prefers one account, keyed `pf-<22hex>` from the pair (design §0). A mapping,
    /// never an identity or a limit: nothing refuses, drops or signs out an account because no
    /// preference names it, and any number of machines may prefer one account. Owner and import
    /// rows only: this Mac's automatic use of an account is a journal `place` row, which never
    /// spends the registry's lifetime records. Version 2 puts it under the authority rules
    /// (`typed::authority`), so an automatic cell never projects and the pair is fixed at birth,
    /// and a conflicted `rank` reads as the smaller, tried first.
    pub PREFERENCE = Collection {
        name: "preference",
        version: 2,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "pf",
        derive: Derive::Composite,
        columns: &[
            Column::new("machine", Ty::Ref("machine")).fixed(),
            Column::new("account", Ty::Ref("account")).fixed(),
            // Smaller is tried first; none sorts after every number.
            Column::new("rank", Ty::Uint)
                .null()
                .on_conflict(OnConflict::Min),
            Column::new("why", Ty::Enum(&["owner", "import"])),
            Column::new("since", Ty::Time).null(),
            row(),
            note(),
            src(),
            notes_src(),
        ],
        unique: &[],
        max_row: 0,
        authority: Some(2),
    }
);

crate::att_collection!(
    /// One import run's provenance, keyed by a digest of the source's bytes, so a re-run
    /// collapses. `name` is a file name only. Version 2 adds the `fleet-db` kind (a replica's
    /// dump, one source per table) and `differs`: the cells this source stated differently from
    /// the registry when it was imported, by row key and column only, never a value, which the
    /// table view shows until the owner settles them (one-mesh design §8). Version 3 types the
    /// differing row and element as structural keys, including decimal-only digest tails.
    pub SOURCE = Collection {
        name: "source",
        version: 3,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "sr",
        derive: Derive::BirthSha256,
        columns: &[
            Column::new(
                "kind",
                Ty::Enum(&[
                    "vault-sheet",
                    "logins-csv",
                    "computers-csv",
                    "emails-csv",
                    "phones-csv",
                    "fleet-accounts",
                    "accounts-toml",
                    "ai-pool",
                    "ai-mailbox",
                    "fleet-book",
                    "swarm-roster",
                    "commons-members",
                    "proposals",
                    "fleet-db",
                ])
            ),
            Column::new("name", Ty::Text(128)),
            Column::new("sha256", Ty::Hex(64)),
            Column::new("rows", Ty::Uint),
            Column::new("at", Ty::Time),
            Column::new("by", Ty::Ref("machine")).null(),
            Column::new("counts", Ty::Json(1024)).null(),
            row(),
            Column::new("differs", Ty::List(&Ty::Struct(DIFFERS), MAX_DIFFERS))
                .null()
                .since(2),
        ],
        unique: &[],
        max_row: 0,
        authority: None,
    }
);

/// The most differing cells one source row names; past it the import's report says how many
/// more.
pub const MAX_DIFFERS: usize = 256;

/// The collections an import writes, as a differing cell names them.
pub const DIFFER_COLLECTIONS: &[&str] = &[
    "address",
    "account",
    "machine",
    "preference",
    "mailbox",
    "phone",
];

/// The columns of [`DIFFER_COLLECTIONS`] by name: a column is a name, never free text, so a
/// secret column's name passes where the free-text guard would refuse it. A column added
/// later and not named here (a mailbox's `mailbox_password` and `totp`) is reported by the
/// import and left out of the source row: the list is a source row's enum in every generation,
/// and a word builds up to 0.200.0 do not know is one they cannot read in a generation they
/// read.
pub const DIFFER_COLUMNS: &[&str] = &[
    "account",
    "addr",
    "alias_of",
    "aliases",
    "arch",
    "billing",
    "browser",
    "carrier",
    "chip",
    "clis",
    "code_path",
    "device",
    "disk_gb",
    "domain",
    "fleet_node",
    "flow",
    "hosts",
    "identity",
    "kind",
    "label",
    "last4",
    "legacy_label",
    "line_type",
    "location",
    "login",
    "machine",
    "mailbox",
    "mailbox_hint",
    "make",
    "merged_into",
    "method",
    "model",
    "nickname",
    "nickname_aliases",
    "node",
    "note",
    "number",
    "origin",
    "os",
    "password",
    "phone",
    "plan",
    "plan_since",
    "protected",
    "provider",
    "ram_gb",
    "rank",
    "realm",
    "region",
    "renews",
    "role",
    "row",
    "service",
    "services",
    "since",
    "status",
    "vault_ref",
    "webmail",
    "why",
];

/// One cell a source stated differently from the registry: where, never what. A set's element
/// is `elem`.
pub const DIFFERS: &[Member] = &[
    Member {
        name: "collection",
        ty: Ty::Enum(DIFFER_COLLECTIONS),
        null: false,
    },
    Member {
        name: "pk",
        ty: Ty::Key,
        null: false,
    },
    Member {
        name: "column",
        ty: Ty::Enum(DIFFER_COLUMNS),
        null: false,
    },
    Member {
        name: "elem",
        ty: Ty::Key,
        null: true,
    },
];

/// The largest earlier record an archived row carries: its padded base64 and the row's key fit
/// one record's 64 KiB body.
pub const MAX_ARCHIVED_WIRE: usize = 46 * 1024;

crate::att_collection!(
    /// An owned row of an earlier generation, carried by a roll (design §3.8): the signed record
    /// that wrote it, still sealed under its own table's scope, so a reader authenticates its
    /// signer and operation digest itself and no writer of this generation can speak for another
    /// Mac. Keyed by a digest of the operation (`keys::archived_pk`); `generation` is the table
    /// generation whose scope the record was sealed under. A row of this Mac's own in this
    /// generation outranks it by the owned rule. Written by the roll alone, never edited.
    pub ARCHIVED = Collection {
        name: "archived",
        version: 1,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "ar",
        derive: Derive::BirthSha256,
        columns: &[
            Column::new("generation", Ty::Uint).fixed(),
            Column::new("wire", Ty::Sealed(MAX_ARCHIVED_WIRE)).fixed(),
            row(),
        ],
        unique: &[],
        max_row: 0,
        authority: None,
    }
);

/// The largest body one history row carries: its padded base64 and the row's key fit one
/// record's 64 KiB body.
pub const MAX_HISTORY_ENTRY: usize = 46 * 1024;

crate::att_collection!(
    /// One entry of an earlier generation's history, carried by a roll before the old
    /// generation's storage goes (`alab::history`, docs/DESIGN-tables-app-2026-11-14.md §4): the
    /// operation's digest, writer, machine, authored time, derived causal clock, request, what it
    /// reverted and its changes, a secret's value withheld; or one roller's horizon, naming the
    /// entries its bound left out. Keyed by a digest of the roller and the operation
    /// (`keys::history_pk`) or of the generation and the roller (`keys::history_horizon_pk`),
    /// each named in the row. Written by the roll alone and counted only from a Mac that wrote
    /// its own cover row (`alab::roll::rollers`), or by a roll from its own Mac's; the grid
    /// shows it read-only (`Writer::Mine`). Owner values, never automatic, so a roll spends no
    /// quota, and a build that does not know the collection reads past it.
    pub HISTORY = Collection {
        name: "history",
        version: 1,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "hy",
        derive: Derive::BirthSha256,
        columns: &[
            Column::new("entry", Ty::Sealed(MAX_HISTORY_ENTRY))
                .fixed()
                .writer(Writer::Mine),
            row(),
        ],
        unique: &[],
        max_row: 0,
        authority: None,
    }
);

/// The most writers one cover row names.
pub const MAX_COVER_WRITERS: usize = 512;

/// One writer's covered history in a cover row: its actor's key (`keys::actor_key`) and the
/// last sequence of its records the roll carried.
pub const COVERED: &[Member] = &[
    Member {
        name: "a",
        ty: Ty::Hex(32),
        null: false,
    },
    Member {
        name: "s",
        ty: Ty::Uint,
        null: false,
    },
];

crate::att_collection!(
    /// What one Mac's restatement of an earlier generation carried (design §3.8): for each
    /// writer of that table, the last record sequence whose effect is now in this one. A Mac may
    /// release its storage of the sealed table once these rows, merged, cover every record it
    /// holds there (`alab::roll::covered`). Keyed by the generation and the restating Mac
    /// (`keys::roll_pk`); written by that Mac's roll alone: a reader counts a cover only as the
    /// Mac its key names signed it (`alab::roll::covers`), and the grid shows its columns
    /// read-only (`Writer::Mine`).
    pub ROLL = Collection {
        name: "roll",
        version: 1,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "rl",
        derive: Derive::BirthSha256,
        columns: &[
            Column::new("from", Ty::Uint).fixed().writer(Writer::Mine),
            Column::new("covered", Ty::List(&Ty::Struct(COVERED), MAX_COVER_WRITERS))
                .writer(Writer::Mine),
            Column::new("ops", Ty::Uint).writer(Writer::Mine),
            Column::new("at", Ty::Time).writer(Writer::Mine),
            row(),
        ],
        unique: &[],
        max_row: 0,
        authority: None,
    }
);

/// Every collection of the registry, besides the reserved `schema` collection.
pub const COLLECTIONS: [&Collection; 16] = [
    &PROVIDER,
    &CLI,
    &PLAN,
    &MODEL,
    &LIST_MACHINE,
    &MACHINE_FACTS,
    &SEALED_ADDRESS,
    &SEALED_MAILBOX,
    &MAILBOX_OBS,
    &ACCOUNT_OBS,
    &SEALED_PHONE,
    &ACCOUNT,
    &SUBACCOUNT,
    &AUTH_ROUTE,
    &PREFERENCE,
    &SOURCE,
];

/// How a person is shown each collection of every registry generation, in the order the Tables
/// page lists them within a face (docs/DESIGN-tables-app-2026-11-14.md §3.1): the owner's data,
/// what each Mac states and the importer's runs, then the reference rows every build compiles
/// (a Mac model is born by the first Mac that reports it). `archived` carries owned rows of an
/// earlier generation, which the view reads as those rows; `history` carries an earlier
/// generation's history entries, which history, as of and diff read as those entries.
///
/// Each names its columns' display order ([`Faced::order`]), the column that names a row first:
/// a row is read by its name, then what it is and where it stands, then the rest.
pub const FACES: &[Faced] = &[
    face(
        "machine",
        ("Machines", "Machine"),
        Face::Owner,
        &[
            "label", "nickname", "role", "status", "location", "model", "chip", "ram_gb",
        ],
    ),
    face(
        "account",
        ("AI accounts", "AI account"),
        Face::Owner,
        &[
            "label", "provider", "login", "status", "plan", "services", "renews",
        ],
    ),
    face(
        "preference",
        ("Links", "Link"),
        Face::Owner,
        &["machine", "account", "rank", "why", "since"],
    ),
    face(
        "address",
        ("Emails", "Email"),
        Face::Owner,
        &["addr", "kind", "domain", "protected", "mailbox"],
    ),
    face(
        "mailbox",
        ("Mailboxes", "Mailbox"),
        Face::Owner,
        &["label", "service", "login", "webmail"],
    ),
    face(
        "phone",
        ("Phones", "Phone"),
        Face::Owner,
        &["label", "last4", "carrier", "line_type", "device"],
    ),
    face(
        "auth-route",
        ("Sign-in routes", "Sign-in route"),
        Face::Owner,
        &["account", "method", "rank", "address", "phone"],
    ),
    face(
        "subaccount",
        ("Sub-accounts", "Sub-account"),
        Face::Owner,
        &["label", "account", "key"],
    ),
    face(
        "machine-facts",
        ("Machine facts", "Machine's facts"),
        Face::Mac,
        &["label", "mac", "hosts", "hw_model", "chip", "ram_gb", "at"],
    ),
    face(
        "account-obs",
        ("Accounts in use", "Account in use"),
        Face::Mac,
        &["account", "cli", "state", "tabs", "at"],
    ),
    face(
        "mailbox-obs",
        ("Mailbox readings", "Mailbox reading"),
        Face::Mac,
        &["mailbox", "complete", "addresses", "at"],
    ),
    face(
        "roll",
        ("Rolls", "Roll"),
        Face::Mac,
        &["from", "covered", "ops", "at"],
    ),
    face(
        "source",
        ("Imports", "Import"),
        Face::Tool,
        &["name", "kind", "at", "rows", "by"],
    ),
    face(
        "provider",
        ("Providers", "Provider"),
        Face::Seeded,
        &[ROW_KEY, "name", "site", "status"],
    ),
    face("cli", ("CLIs", "CLI"), Face::Seeded, &[ROW_KEY, "provider"]),
    face(
        "plan",
        ("Plans", "Plan"),
        Face::Seeded,
        &[ROW_KEY, "provider", "name", "tier", "status"],
    ),
    face(
        "model",
        ("Machine models", "Machine model"),
        Face::Seeded,
        &[ROW_KEY, "marketing", "form", "chip_family", "year"],
    ),
    face(
        "archived",
        ("Archived rows", "Archived row"),
        Face::Carried,
        &["generation"],
    ),
    face(
        "history",
        ("Carried history", "Carried history entry"),
        Face::Carried,
        &[],
    ),
];

/// A collection's face: `(title, singular)` are its names for many rows and for one.
const fn face(
    collection: &'static str,
    (title, singular): (&'static str, &'static str),
    face: Face,
    order: &'static [&'static str],
) -> Faced {
    Faced {
        collection,
        title,
        singular,
        face,
        order,
    }
}

// Every generation the build knows, read from the one list of them: a generation added there
// whose collection has no face does not compile.
const _: () = {
    let mut n = 0;
    while n < GENERATIONS.len() {
        assert!(
            GENERATIONS[n].faces_every_collection(),
            "every registry collection says how a person is shown it, once"
        );
        assert!(
            GENERATIONS[n].orders_name_columns(),
            "every display order names its collection's columns, each once"
        );
        n += 1;
    }
};

/// `alab/registry`.
pub const REGISTRY: Table = Table {
    application: APPLICATION,
    role: ROLE,
    collections: &COLLECTIONS,
    self_quota: SELF_QUOTA,
    ends: &["preference"],
    faces: FACES,
};

/// The second generation's role (design §3.8, §11 step 13).
pub const ROLE_2: &str = "registry.2";

/// The second generation's collections: the first's, the rows a roll carries archived, its cover
/// rows and the history it carries.
pub const COLLECTIONS_2: [&Collection; 19] = [
    &PROVIDER,
    &CLI,
    &PLAN,
    &MODEL,
    &LIST_MACHINE,
    &MACHINE_FACTS,
    &SEALED_ADDRESS,
    &SEALED_MAILBOX,
    &MAILBOX_OBS,
    &ACCOUNT_OBS,
    &SEALED_PHONE,
    &ACCOUNT,
    &SUBACCOUNT,
    &AUTH_ROUTE,
    &PREFERENCE,
    &SOURCE,
    &ARCHIVED,
    &ROLL,
    &HISTORY,
];

/// The third generation's collections: the second's, with `address`, `mailbox` and `phone` at
/// version 3, each secret column naming a secret-store entry and a phone number ordinary data.
pub const COLLECTIONS_3: [&Collection; 19] = [
    &PROVIDER,
    &CLI,
    &PLAN,
    &MODEL,
    &LIST_MACHINE,
    &MACHINE_FACTS,
    &ADDRESS,
    &MAILBOX_3,
    &MAILBOX_OBS,
    &ACCOUNT_OBS,
    &PHONE,
    &ACCOUNT,
    &SUBACCOUNT,
    &AUTH_ROUTE,
    &PREFERENCE,
    &SOURCE,
    &ARCHIVED,
    &ROLL,
    &HISTORY,
];

/// The fourth generation's collections: the third's, with `machine` and `mailbox` at version 4,
/// `nickname_aliases`, `hosts` and `receives_domains` each a text set (LT-15B).
pub const COLLECTIONS_4: [&Collection; 19] = [
    &PROVIDER,
    &CLI,
    &PLAN,
    &MODEL,
    &MACHINE,
    &MACHINE_FACTS,
    &ADDRESS,
    &MAILBOX,
    &MAILBOX_OBS,
    &ACCOUNT_OBS,
    &PHONE,
    &ACCOUNT,
    &SUBACCOUNT,
    &AUTH_ROUTE,
    &PREFERENCE,
    &SOURCE,
    &ARCHIVED,
    &ROLL,
    &HISTORY,
];

/// `alab/registry.2`: the registry restated by a roll, each writer's quota whole again. Readers
/// take it up once `alab/registry` is sealed (`att::current`).
pub const REGISTRY_2: Table = Table {
    application: APPLICATION,
    role: ROLE_2,
    collections: &COLLECTIONS_2,
    self_quota: SELF_QUOTA,
    ends: &["preference"],
    faces: FACES,
};

/// The third generation's role: the first whose history holds no secret (docs/PLAN.md LT-04).
pub const ROLE_3: &str = "registry.3";

/// `alab/registry.3`: the second's collections as a roll restates them once every sealed value
/// earlier generations hold has moved into the secret store, under a table scope of its own, so
/// its audience key opens nothing of theirs ([`crate::alab::Registry::roll`]).
pub const REGISTRY_3: Table = Table {
    application: APPLICATION,
    role: ROLE_3,
    collections: &COLLECTIONS_3,
    self_quota: SELF_QUOTA,
    ends: &["preference"],
    faces: FACES,
};

/// The fourth generation's role: the first whose free-text lists are text sets (LT-15B).
pub const ROLE_4: &str = "registry.4";

/// `alab/registry.4`: the third's collections with a Mac's nickname aliases and hosts and a
/// mailbox's domains as text sets, one element per member, under a table scope of its own. Only
/// a build that reads text-set elements compiles it, so no build that writes those columns as
/// one list ever reads or writes this generation; a roll into it restates each list as its
/// members (`alab::roll`).
pub const REGISTRY_4: Table = Table {
    application: APPLICATION,
    role: ROLE_4,
    collections: &COLLECTIONS_4,
    self_quota: SELF_QUOTA,
    ends: &["preference"],
    faces: FACES,
};

/// The registry's generations, oldest first: generation `n` is `GENERATIONS[n - 1]`.
pub const GENERATIONS: [&Table; 4] = [&REGISTRY, &REGISTRY_2, &REGISTRY_3, &REGISTRY_4];

/// The key a new grid row of `collection` is born under (design §5, §8), from the typed values
/// the owner gave its cells: a mailbox's from its login address's key; a random key for a
/// phone, never a digest of its number; an address's digest, only while no row says that
/// address and no row holds that key ([`AddressHolder::Free`]); **the canonical account key**
/// from the account's realm and the address of its identity, else its login
/// (`keys::claims_email`), read from `cells`; a
/// sub-account from its account and observed key; a sign-in route from its account and method;
/// a preference from its machine and account; a planned machine from its label, the sheet
/// placeholder's birth identity. A provider, CLI, plan, model or source row is keyed by a lab
/// verb or the importer, never the grid.
///
/// # Errors
/// A value the key needs is missing or malformed, or the collection's keys come from elsewhere.
pub fn birth(
    collection: &Collection,
    values: &BTreeMap<String, Value>,
    cells: &Cells,
) -> Result<String> {
    if let Some(elsewhere) = born_elsewhere(collection.name) {
        return Err(Error::new(&format!(
            "{}: the grid adds no rows here; {elsewhere}",
            collection.name
        )));
    }
    let text = |column: &str| values.get(column).and_then(Value::as_str);
    let need = |column: &str| {
        text(column).ok_or_else(|| {
            Error::new(&format!(
                "{}.{column}: a new row's key is made from this column; fill it in",
                collection.name
            ))
        })
    };
    match collection.name {
        "mailbox" => keys::mailbox_pk(need("login")?),
        "phone" => keys::random_pk(keys::Random::Phone),
        "address" => {
            let addresses = cell::project(&ADDRESS, cells);
            let index = AddressIndex::of(&addresses);
            let refuse = |rule: String| Err(Error::new(&format!("address.addr: {rule}")));
            match index.holder(need("addr")?)? {
                AddressHolder::Free {
                    derived,
                    taken: false,
                } => Ok(derived),
                AddressHolder::Row(pk) => refuse(format!(
                    "email row {} already holds this address; edit that row",
                    safe(pk)
                )),
                AddressHolder::Several(_) => {
                    refuse("more than one email row holds this address".to_owned())
                }
                AddressHolder::Free { derived, .. } => refuse(format!(
                    "this address's key {} is held by a row that says another address now; \
                     rename that row back instead",
                    safe(&derived)
                )),
            }
        }
        "machine" => Ok(keys::machine_pk(need("label")?)),
        "account" => {
            let addresses = cell::project(&ADDRESS, cells);
            let address_of = |pk: &str| {
                addresses
                    .rows
                    .get(pk)
                    .filter(|row| row.live())
                    .and_then(|row| row.get("addr"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        Error::new(&format!(
                            "{}: the address it names is not a live row here, or is conflicted",
                            collection.name
                        ))
                    })
            };
            let realm = need("realm")?;
            if keys::canonical_realm(realm) != Some(realm) {
                return Err(Error::new(&format!(
                    "{}.realm: {}",
                    collection.name,
                    realm_rule(realm)
                )));
            }
            let login = address_of(need("login")?)?;
            let identity = text("identity").map(address_of).transpose()?;
            let email = keys::claims_email(identity, Some(login))?;
            keys::account_pk(realm, &email)
        }
        "subaccount" => keys::subaccount_pk(need("account")?, need("key")?),
        "auth-route" => keys::auth_route_pk(need("account")?, need("method")?),
        "preference" => keys::preference_pk(need("machine")?, need("account")?),
        other => Err(Error::new(&format!(
            "{other}: the grid adds no rows here; a lab verb or the importer keys them"
        ))),
    }
}

/// Whether [`birth`] makes a new row of `collection` a random key as it is written (a phone's,
/// never a digest of its number), so no reading of its cells before the write names it.
pub fn born_at_random(collection: &str) -> bool {
    collection == "phone"
}

/// Who adds a new row of `collection` where the grid adds none ([`birth`] refuses one with
/// these words, and the Tables page says them in place of its Add): `None` for a collection a
/// grid row is born into. A compiled seed's rows (a provider, a CLI, a plan) come from every
/// build, a machine model's from the first machine of that model to register, an import's from
/// the importer, and each machine's own rows from that machine.
pub fn born_elsewhere(collection: &str) -> Option<&'static str> {
    match collection {
        "mailbox" | "phone" | "address" | "machine" | "account" | "subaccount" | "auth-route"
        | "preference" => None,
        "provider" | "cli" | "plan" => Some("every build writes these from its reference rows"),
        "model" => Some("a machine adds its model's row as it registers"),
        "source" => Some("aterm db alab import --apply adds these rows"),
        "machine-facts" | "account-obs" | "mailbox-obs" | "roll" => {
            Some("each machine writes its own row")
        }
        _ => Some("a lab verb or the importer adds these rows"),
    }
}

/// The verb that sets `collection`'s `column`, where no grid types it: a secret (a mailbox
/// address's `password`, a mailbox's `mailbox_password` and `totp`), whose value only the secret
/// store holds and which is read from standard input, never typed into a page; and a number a
/// generation holds sealed (a phone's `number` in a generation builds up to 0.200.0 read). `<row>`
/// stands for the row's key, which a new row has once it is written.
pub fn set_by(collection: &str, column: &str) -> Option<&'static str> {
    match (collection, column) {
        ("address", "password") => Some("aterm db emails set <row> password --stdin"),
        ("mailbox", "mailbox_password" | "totp") => {
            Some("aterm db alab import --mailbox-toml <file> --apply")
        }
        ("phone", "number") => Some("aterm db phones set <row> number <number>"),
        _ => None,
    }
}

/// What a person reads for a reference to a row, as one read of a registry generation holds it:
/// a machine by its label, an account by its provider word and its login's address (the owner,
/// 2026-11-11, "An account's name and slug" in docs/OWNER_RULINGS.md). Every surface that
/// names a referenced row in words asks this; a slug joins the account's words here.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Names {
    machines: BTreeMap<String, String>,
    accounts: BTreeMap<String, String>,
}

impl Names {
    /// The words for every machine and account row `view` of `table` holds, retired ones too: a
    /// reference outlives the row's life.
    pub fn of(table: &Table, view: &View) -> Self {
        Self::with_projections(table, view, |_| None)
    }

    /// Name references using projections the caller already holds for this view. A collection
    /// the caller has not projected is read from the same view; a generation may omit it.
    pub fn with_projections<'p>(
        table: &Table,
        view: &View,
        known: impl Fn(&str) -> Option<&'p cell::Projection>,
    ) -> Self {
        let rows = |name| {
            let collection = table.collection(name)?;
            Some(match known(name) {
                Some(rows) => std::borrow::Cow::Borrowed(rows),
                None => std::borrow::Cow::Owned(view.project(collection)),
            })
        };
        fn text<'r>(row: &'r cell::Row, column: &str) -> Option<&'r str> {
            row.get(column)
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
        }
        let mut names = Self::default();
        if let Some(machines) = rows(MACHINE.name) {
            for (pk, row) in &machines.rows {
                if let Some(label) = text(row, "label") {
                    names.machines.insert(pk.clone(), label.to_owned());
                }
            }
        }
        if let Some(accounts) = rows(ACCOUNT.name) {
            let addresses = rows(ACCOUNT_ADDRESS);
            for (pk, row) in &accounts.rows {
                let address = text(row, "login")
                    .and_then(|login| addresses.as_deref()?.rows.get(login))
                    .and_then(|address| text(address, "addr"));
                // Without an address, the label the owner gave it: a read admits no other
                // (`typed::authority`), and an account has no default label.
                let words = match (text(row, "provider"), address) {
                    (Some(provider), Some(address)) => format!("{provider} \u{b7} {address}"),
                    (Some(provider), None) => match text(row, "label") {
                        Some(label) => format!("{provider} \u{b7} {label}"),
                        None => provider.to_owned(),
                    },
                    (None, Some(address)) => address.to_owned(),
                    (None, None) => continue,
                };
                names.accounts.insert(pk.clone(), words);
            }
        }
        names
    }

    /// The words for row `pk` of collection `target`, where this read names it.
    pub fn words(&self, target: &str, pk: &str) -> Option<&str> {
        match target {
            "machine" => self.machines.get(pk),
            "account" => self.accounts.get(pk),
            _ => None,
        }
        .map(String::as_str)
    }
}

/// The collection an account's login names.
const ACCOUNT_ADDRESS: &str = "address";

#[cfg(test)]
mod tests;
