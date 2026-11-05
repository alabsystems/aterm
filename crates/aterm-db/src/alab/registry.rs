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
//! **Secrets are `Secret` cells** (docs/DESIGN-alab-one-mesh-2026-11-05.md §4.7, §4.8): an
//! address's `password` and a phone's `number` (version 2) hold an authenticated inner seal,
//! masked on every surface and opened only through the audited reveal
//! ([`crate::alab::secret::reveal`]); a column whose name says secret must be `Secret` (checked
//! at compile time). A row whose `protected` cell is true holds no `Secret` cell
//! ([`crate::typed::check::protected`]). The one column typed `email` is `address.addr`, masked
//! on every surface; every other address is a reference to its `address` row, a mailbox's
//! `login` included, and a mailbox's key derives from that login's key
//! ([`keys::mailbox_pk`]). A phone's key is random, never a digest of the number: the importer
//! finds a phone by opening its `number` cell. The version-1 `vault_ref` columns of `mailbox`
//! and `phone` stay, unwritten, until every Mac reads version 2. An imported note's text goes in
//! the row's `note` only when the note guard passes it (design §0); a refused note and every
//! losing value stay coordinates in `notes_src`.

use crate::{
    Error, Result,
    typed::{
        Collection, Column, Derive, Kind, Member, OnConflict, Table, Ty, Writer, Writers,
        cell::{self, Cells},
        keys,
    },
};
use aterm_json::Value;
use std::collections::BTreeMap;

/// The registry's application word.
pub const APPLICATION: &str = "alab";
/// The registry's role.
pub const ROLE: &str = "registry";
/// The table descriptor's version: 1 for the table's life. A collection's own version moves
/// instead (design §4.1); a breaking change is a new role (`registry.2`).
pub const VERSION: u32 = 1;
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
    }
);

/// The machine statuses, safest first for a conflict.
pub const MACHINE_STATUS: &[&str] = &["planned", "active", "parked", "lost"];

crate::att_collection!(
    /// One physical Mac across relabels and reinstalls, keyed `mc-<16hex>` from its birth
    /// identity, minted once. No serial number and no IOPlatformUUID are stored.
    pub MACHINE = Collection {
        name: "machine",
        version: 1,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "mc",
        derive: Derive::BirthSha256,
        columns: &[
            Column::new("label", Ty::Label).writer(Writer::Birth),
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
            Column::new("nickname_aliases", Ty::List(&Ty::Text(64), 8)).null(),
            Column::new("clis", Ty::Set(&Ty::Ref("cli"))),
            Column::new("location", Ty::Text(64)).null(),
            Column::new("fleet_node", Ty::Hex(32)).null().writer(Writer::Birth),
            Column::new("hosts", Ty::List(&Ty::Text(64), 4)).null().writer(Writer::Birth),
            // Display-only unless the merged Mac's own facts row names the same target (§6).
            Column::new("merged_into", Ty::Ref("machine")).null(),
            row().writer(Writer::Birth),
            note(),
            src(),
            notes_src(),
        ],
        // A fleet node names one Mac: two live rows naming it are a finding, and adoption by
        // node binds neither (`import::Kind::NodeAmbiguous`).
        unique: &[&["label"], &["fleet_node"]],
        max_row: 0,
    }
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
    }
);

crate::att_collection!(
    /// A login or mail address, keyed `ad-<16hex>` from the address, which is the one value
    /// typed `email` in the registry. Version 2 adds its mailbox `password`, a `Secret`, which
    /// a `protected` row never holds.
    pub ADDRESS = Collection {
        name: "address",
        version: 2,
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
            Column::new("password", Ty::Secret).null().since(2),
        ],
        unique: &[&["addr"]],
        max_row: 0,
    }
);

crate::att_collection!(
    /// A mailbox that receives codes, keyed `mb-<16hex>` from its login address's key
    /// ([`keys::mailbox_pk`]). Version 2 adds `login`, that address row, whose own `password`
    /// is the mailbox's login password (one fact, one cell); the `Secret`s `mailbox_password`
    /// (a two-password webmail's second) and `totp` (the login's RFC 6238 seed or `otpauth://`
    /// URI); and what `ai-mailbox.toml`'s `receives` said it receives: `receives`, the exact
    /// addresses, and `receives_domains`, each `*@<domain>` as its domain and `*` as itself
    /// (`harness/mailbox_routes.rs`); and its `imap_url` and `http_url`, `imap` and `http`, which
    /// hold no user part. The version-1 `vault_ref` stays unwritten until every Mac reads
    /// version 2.
    pub MAILBOX = Collection {
        name: "mailbox",
        version: 2,
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
            Column::new("mailbox_password", Ty::Secret).null().since(2),
            Column::new("totp", Ty::Secret).null().since(2),
            Column::new("receives", Ty::Set(&Ty::Ref("address"))).since(2),
            // A domain is no set element (its dots break the cell key's grammar): a list.
            Column::new("receives_domains", Ty::List(&Ty::Text(253), 64))
                .null()
                .since(2),
            // `imaps://imap.example.test:993`, a local Bridge's `imap://127.0.0.1:1143`, a mail
            // API's `http://127.0.0.1:8025`: `Url` takes only `https`, and the importer refuses
            // one naming a user.
            Column::new("imap", Ty::Text(512)).null().since(2),
            Column::new("http", Ty::Text(512)).null().since(2),
        ],
        unique: &[&["label"]],
        max_row: 0,
    }
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
    }
);

crate::att_collection!(
    /// A phone, keyed `ph-<16 random hex>`, never a digest of the number: with the country
    /// code and the last four digits known, an unkeyed hash falls to about 10⁶ guesses.
    /// Version 2 holds the full number as a `Secret` (`number`, digits only); the importer
    /// finds a phone by opening it, and `check` lists two rows holding one number by key.
    pub PHONE = Collection {
        name: "phone",
        version: 2,
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
            Column::new("number", Ty::Secret).null().since(2),
        ],
        unique: &[],
        max_row: 0,
    }
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
    pub ACCOUNT = Collection {
        name: "account",
        version: 1,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "ac",
        derive: Derive::BirthSha256,
        columns: &[
            Column::new("provider", Ty::Ref("provider")),
            Column::new("realm", Ty::Text(32)),
            Column::new("login", Ty::Ref("address")),
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
                .on_conflict(OnConflict::Prefer(&["banned", "broken", "paused", "pending", "active"])),
            // A name the owner can change, never a machine (`guard::account_label`); empty reads
            // as the default, `keys::default_account_label`.
            Column::new("label", Ty::AccountLabel(32)).null(),
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
            row(),
            note(),
            src(),
            notes_src(),
        ],
        // Two live accounts under one name are a finding; the name is never a key.
        unique: &[&["label"]],
        max_row: 0,
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
    }
);

crate::att_collection!(
    /// One machine prefers one account, keyed `pf-<22hex>` from the pair (design §0). A mapping,
    /// never an identity or a limit: nothing refuses, drops or signs out an account because no
    /// preference names it, and any number of machines may prefer one account. Owner and import
    /// rows only: this Mac's automatic use of an account is a journal `place` row, which never
    /// spends the registry's lifetime records.
    pub PREFERENCE = Collection {
        name: "preference",
        version: 1,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "pf",
        derive: Derive::Composite,
        columns: &[
            Column::new("machine", Ty::Ref("machine")).fixed(),
            Column::new("account", Ty::Ref("account")).fixed(),
            // Smaller is tried first; none sorts after every number.
            Column::new("rank", Ty::Uint).null(),
            Column::new("why", Ty::Enum(&["owner", "import"])),
            Column::new("since", Ty::Time).null(),
            row(),
            note(),
            src(),
            notes_src(),
        ],
        unique: &[],
        max_row: 0,
    }
);

crate::att_collection!(
    /// One import run's provenance, keyed by a digest of the source's bytes, so a re-run
    /// collapses. `name` is a file name only. Version 2 adds the `fleet-db` kind (a replica's
    /// dump, one source per table) and `differs`: the cells this source stated differently from
    /// the registry when it was imported, by row key and column only, never a value, which the
    /// table view shows until the owner settles them (one-mesh design §8).
    pub SOURCE = Collection {
        name: "source",
        version: 2,
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
/// `Secret` column's name passes where the free-text guard would refuse it. A column added
/// later and not named here is reported by the import and left out of the source row.
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
        ty: Ty::Text(32),
        null: false,
    },
    Member {
        name: "column",
        ty: Ty::Enum(DIFFER_COLUMNS),
        null: false,
    },
    Member {
        name: "elem",
        ty: Ty::Text(64),
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
    }
);

/// Every collection of the registry, besides the reserved `schema` collection.
pub const COLLECTIONS: [&Collection; 15] = [
    &PROVIDER,
    &CLI,
    &PLAN,
    &MODEL,
    &MACHINE,
    &MACHINE_FACTS,
    &ADDRESS,
    &MAILBOX,
    &MAILBOX_OBS,
    &PHONE,
    &ACCOUNT,
    &SUBACCOUNT,
    &AUTH_ROUTE,
    &PREFERENCE,
    &SOURCE,
];

/// `alab/registry`.
pub const REGISTRY: Table = Table {
    application: APPLICATION,
    role: ROLE,
    collections: &COLLECTIONS,
    self_quota: SELF_QUOTA,
    ends: &["preference"],
};

/// The second generation's role (design §3.8, §11 step 13).
pub const ROLE_2: &str = "registry.2";

/// The second generation's collections: the first's, the rows a roll carries archived, and its
/// cover rows.
pub const COLLECTIONS_2: [&Collection; 17] = [
    &PROVIDER,
    &CLI,
    &PLAN,
    &MODEL,
    &MACHINE,
    &MACHINE_FACTS,
    &ADDRESS,
    &MAILBOX,
    &MAILBOX_OBS,
    &PHONE,
    &ACCOUNT,
    &SUBACCOUNT,
    &AUTH_ROUTE,
    &PREFERENCE,
    &SOURCE,
    &ARCHIVED,
    &ROLL,
];

/// `alab/registry.2`: the registry restated by a roll, each writer's quota whole again. Readers
/// take it up once `alab/registry` is sealed (`att::current`).
pub const REGISTRY_2: Table = Table {
    application: APPLICATION,
    role: ROLE_2,
    collections: &COLLECTIONS_2,
    self_quota: SELF_QUOTA,
    ends: &["preference"],
};

/// The registry's generations, oldest first: generation `n` is `GENERATIONS[n - 1]`.
pub const GENERATIONS: [&Table; 2] = [&REGISTRY, &REGISTRY_2];

/// The key a new grid row of `collection` is born under (design §5, §8), from the typed values
/// the owner gave its cells: a mailbox's from its login address's key; a random key for a
/// phone, never a digest of its number; an address's digest; **the canonical account key** from the account's realm and the
/// address of its identity, else its login (`keys::claims_email`), read from `cells`; a
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
    let text = |column: &str| values.get(column).and_then(Value::as_str);
    let need = |column: &str| {
        text(column).ok_or_else(|| {
            Error::new(&format!(
                "{}.{column}: a new row's key is made from this column; fill it in",
                collection.name
            ))
        })
    };
    let address_of = |pk: &str| {
        cell::project(&ADDRESS, cells)
            .rows
            .get(pk)
            .filter(|row| row.live())
            .and_then(|row| row.get("addr"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                Error::new(&format!(
                    "{}: the address it names is not a live row here, or is conflicted",
                    collection.name
                ))
            })
    };
    match collection.name {
        "mailbox" => keys::mailbox_pk(need("login")?),
        "phone" => keys::random_pk(keys::Random::Phone),
        "address" => keys::address_pk(need("addr")?),
        "machine" => Ok(keys::machine_pk(need("label")?)),
        "account" => {
            let login = address_of(need("login")?)?;
            let identity = text("identity").map(address_of).transpose()?;
            let email = keys::claims_email(identity.as_deref(), Some(&login))?;
            keys::account_pk(need("realm")?, &email)
        }
        "subaccount" => keys::subaccount_pk(need("account")?, need("key")?),
        "auth-route" => keys::auth_route_pk(need("account")?, need("method")?),
        "preference" => keys::preference_pk(need("machine")?, need("account")?),
        other => Err(Error::new(&format!(
            "{other}: the grid adds no rows here; a lab verb or the importer keys them"
        ))),
    }
}

/// What an account row is called: the owner's or the import's `label`, else its default, from
/// the key and the realm. `None` only for a row with neither a label nor a known realm.
pub fn account_label(pk: &str, row: &cell::Row) -> Option<String> {
    row.get("label")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            let realm = row.get("realm").and_then(Value::as_str)?;
            keys::default_account_label(realm, pk).ok()
        })
}
