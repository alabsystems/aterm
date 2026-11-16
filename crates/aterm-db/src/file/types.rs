// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The shared files this build compiles (docs/DESIGN-shared-files-2026-11-11.md §3, §10): each a
//! name bound to one collection of a typed table family that already replicates. A file adds
//! no storage and no wire: the stored collection keeps its name, so old and new builds share one
//! history.
//!
//! Every enrolled member reads every shared file, and its snapshot and face sit in plain local
//! files (§5, §13 decision 2). A file therefore holds no `Secret` and no `Sealed` column, at any
//! depth of a column's type: [`Type::check`] says so, and [`FILES`] is built through it, so a
//! file bound to such a collection does not compile.
//!
//! Each file declares its class (§5, "File classes"), which says who writes it through its
//! outbox: an owner file takes only an owner's write there, a Mac file only each Mac's
//! statement of its own rows, and a derived file nothing. The class is checked against what the
//! file is bound to when the build compiles ([`Type::check`]), and the publisher holds an intent
//! the file's class does not take ([`Class::refuses`]). Code that writes the registry itself (a
//! Mac's self-registration, the importer) passes no outbox, and the materializer projects its
//! cells as any other until it judges them by the merge rules (`file::admit`, SF-07); a Mac
//! file's rows are judged by their keys when they are read (`typed::owned`): a row or an entry
//! at a key that does not name its signer's machine is inert.
//!
//! A file is bound to one of three stores ([`Store`]): a collection of a table family the
//! catalog declares (the registry's), a collection of the lab journal's week ring (each Mac's
//! own rows and events), or a fold of other files' projections, stored nowhere (a view).

use crate::{
    alab::{
        journal::{self, EV, LOGIN},
        registry::{ACCOUNT, MACHINE, PREFERENCE},
    },
    att::{self, Compiled},
    typed::{Collection, Kind, Ty, Writers},
};
use aterm_file::{Intent, Name};

/// Who writes a shared file (§5, "File classes").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// The owner, through an explicit `aterm file` verb on any member Mac: a cells collection
    /// any current writer writes for the owner. Its outbox takes no Mac's statement of its own
    /// row; a cell an automatic writer puts in the registry itself still projects until
    /// `file::admit` is built.
    Owner,
    /// Each Mac, only at keys that name its own Commons machine uuid: an owned collection, or a
    /// log. Its outbox takes no owner's write, and a row at another Mac's key is inert.
    Mac,
    /// Nobody: a fold of other files' projections, made on each Mac when it projects them and
    /// stored as no cell (§2, "Views across Macs"). Its outbox takes nothing.
    Derived,
}

impl Class {
    /// Why a file of this class takes no `intent`, value-free; `None` where it takes it. An
    /// intent is a Mac's statement when every key it writes ends `@self`
    /// ([`Intent::mine`]); any other, a revert included, is an owner's write.
    #[must_use]
    pub fn refuses(self, intent: &Intent) -> Option<&'static str> {
        match (self, intent.mine()) {
            (Self::Owner, true) => Some("an owner file takes no Mac statement"),
            (Self::Mac, false) => Some("a Mac file takes no owner write"),
            (Self::Derived, _) => Some("a derived file takes no write: nobody writes a view"),
            (Self::Owner, false) | (Self::Mac, true) => None,
        }
    }
}

/// The columns a file computes at projection, never stored as cells (§3, "Face format").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Computed {
    /// No computed column.
    None,
    /// `ai/accounts`: `=name`, `=slug`, `=email`, `=login_email`, `=links`.
    Accounts,
    /// `ai/links`: `=account_slug`, `=account_name`, `=machine_name`.
    Links,
    /// `lab/machines`: `=seats`, the Commons seats whose own facts row binds the machine;
    /// `=here`, on the machine this Mac's own facts row binds; `=nickname_auto`, on a machine
    /// whose one nickname an automatic write set.
    Machines,
    /// A file of the journal ring: `=seen`, when this Mac last saw the row change, which is how
    /// long its snapshot keeps the row once the ring's week tables no longer hold it
    /// (`aterm_file::retain`).
    Kept,
}

impl Computed {
    /// The computed columns' names, in face order.
    #[must_use]
    pub const fn columns(self) -> &'static [&'static str] {
        match self {
            Self::None => &[],
            Self::Accounts => &["name", "slug", "email", "login_email", "links"],
            Self::Links => &["account_slug", "account_name", "machine_name"],
            Self::Machines => &["seats", "here", "nickname_auto"],
            Self::Kept => &[aterm_file::retain::SEEN],
        }
    }
}

/// A fold of other files' projections (§2, "Views across Macs").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fold {
    /// `ai/limits`: every Mac's `ai/usage` rows, one row per account or sub-account, by the
    /// provider's reset instance (`super::limits`).
    Limits,
}

impl Fold {
    /// The view's columns, in face order: all computed, all read-only.
    #[must_use]
    pub const fn columns(self) -> &'static [&'static str] {
        match self {
            Self::Limits => super::limits::COLUMNS,
        }
    }
}

/// What a shared file is bound to.
#[derive(Clone, Copy, Debug)]
pub enum Store {
    /// A collection of a table family this Mac's catalog declares: `family` is the family's
    /// first generation, whose successors the catalog names.
    Table {
        family: &'static Compiled,
        collection: &'static Collection,
    },
    /// A collection of the lab journal's week ring (`catalog::ring::JOURNAL`,
    /// [`journal::JOURNAL`]): each Mac's own rows or entries, in the window's week tables.
    Journal(&'static Collection),
    /// A fold of other files' projections, stored nowhere.
    View(Fold),
}

/// One shared file of this build.
#[derive(Clone, Copy, Debug)]
pub struct Type {
    /// `<set>/<leaf>`.
    pub name: &'static str,
    /// What `ls` says of it.
    pub about: &'static str,
    /// Who writes it.
    pub class: Class,
    /// What it projects.
    pub store: Store,
    pub computed: Computed,
    /// The columns a row is named by besides its key, in the order a name is tried.
    pub handles: &'static [&'static str],
}

/// The kind word of a file whose rows are keyed records: a cells table, or each Mac's rows.
pub const TABLE: &str = "table";
/// The kind word of a log: each Mac's entries, never edited.
pub const LOG: &str = "log";
/// The kind word of a view: a fold of other files, stored nowhere.
pub const VIEW: &str = "view";
/// The extension of every face this build writes.
pub const TSV: &str = "tsv";

/// Whether a value of `ty` can hold a secret: a `Secret`, or a `Sealed` record, at any depth.
const fn holds_secret(ty: &Ty) -> bool {
    match ty {
        Ty::Secret | Ty::Sealed(_) => true,
        Ty::Set(inner) | Ty::List(inner, _) => holds_secret(inner),
        Ty::Struct(members) => {
            let mut i = 0;
            while i < members.len() {
                if holds_secret(&members[i].ty) {
                    return true;
                }
                i += 1;
            }
            false
        }
        _ => false,
    }
}

const fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// Whether a file of `class` may project `collection`. Every kind and writer is placed in each
/// class's table, so a new one fails to compile here until someone says which class takes it.
const fn fits(class: Class, collection: &Collection) -> Result<(), &'static str> {
    let (kind, writers) = (collection.kind, collection.writers);
    match class {
        Class::Owner => match (kind, writers) {
            (Kind::Cells, Writers::Owner) => Ok(()),
            (Kind::Cells, Writers::SelfKey | Writers::SelfSuffix)
            | (Kind::Owned | Kind::Log, Writers::Owner | Writers::SelfKey | Writers::SelfSuffix) => {
                Err("an owner file projects a cells collection the owner writes")
            }
        },
        Class::Mac => match (kind, writers) {
            (Kind::Owned, Writers::SelfKey | Writers::SelfSuffix)
            | (Kind::Log, Writers::SelfSuffix) => Ok(()),
            (Kind::Cells, Writers::Owner | Writers::SelfKey | Writers::SelfSuffix)
            | (Kind::Owned, Writers::Owner)
            | (Kind::Log, Writers::Owner | Writers::SelfKey) => {
                Err("a Mac file projects an owned collection keyed by its writer's uuid, or a log")
            }
        },
        Class::Derived => Err("a derived file is a view: it binds no collection"),
    }
}

/// Whether `collection` is one of the journal ring's.
const fn in_journal(collection: &Collection) -> bool {
    let all = journal::JOURNAL.collections;
    let mut i = 0;
    while i < all.len() {
        if same(all[i].name, collection.name) {
            return true;
        }
        i += 1;
    }
    false
}

impl Type {
    /// The collection the file projects; `None` for a view.
    #[must_use]
    pub const fn collection(&self) -> Option<&'static Collection> {
        match self.store {
            Store::Table { collection, .. } | Store::Journal(collection) => Some(collection),
            Store::View(_) => None,
        }
    }

    /// The table family and collection of a file the catalog declares; `None` for a file of
    /// the journal ring and for a view, which no owner's write reaches.
    #[must_use]
    pub const fn table(&self) -> Option<(&'static Compiled, &'static Collection)> {
        match self.store {
            Store::Table { family, collection } => Some((family, collection)),
            Store::Journal(_) | Store::View(_) => None,
        }
    }

    /// The file's kind word: `table`, `log` or `view`.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self.store {
            Store::View(_) => VIEW,
            Store::Table { collection, .. } | Store::Journal(collection) => match collection.kind {
                Kind::Log => LOG,
                Kind::Cells | Kind::Owned => TABLE,
            },
        }
    }

    /// The columns stored as cells or rows, the key's aside: the collection's, and none for a
    /// view.
    const fn stored(&self) -> &'static [crate::typed::Column] {
        match self.collection() {
            Some(collection) => collection.columns,
            None => &[],
        }
    }

    /// The columns computed at projection: the declared ones, and every column of a view.
    #[must_use]
    pub const fn computed_columns(&self) -> &'static [&'static str] {
        match self.store {
            Store::View(fold) => fold.columns(),
            Store::Table { .. } | Store::Journal(_) => self.computed.columns(),
        }
    }

    /// Whether the declaration may be a shared file.
    ///
    /// # Errors
    /// The first rule it breaks: its name is not `<set>/<leaf>`; its class does not fit what it
    /// is bound to (an owner file projects a cells collection of a catalog table the owner
    /// writes; a Mac file an owned collection keyed by its writer's uuid, or a log, and one of
    /// the journal ring is one of the ring's collections; a derived file a view, and only a
    /// derived file a view); a column of the collection, live or retired, can hold a `Secret`
    /// or a `Sealed` record (a retired column's cells still replicate); a view declares
    /// computed columns of its own; or a handle or computed column takes a stored column's name
    /// or the key's.
    pub const fn check(&self) -> Result<(), &'static str> {
        if !aterm_file::name::well_formed(self.name) {
            return Err("a shared file is named <set>/<leaf>, each part [a-z][a-z0-9-]{0,19}");
        }
        let collection = match (self.class, self.store) {
            (Class::Derived, Store::View(_)) => None,
            (Class::Derived, Store::Table { .. } | Store::Journal(_)) => {
                return Err("a derived file is a view: it binds no collection");
            }
            (Class::Owner | Class::Mac, Store::View(_)) => {
                return Err("a view is a derived file: nobody writes it");
            }
            (Class::Owner, Store::Journal(_)) => {
                return Err("an owner file projects a cells collection the owner writes");
            }
            (Class::Mac, Store::Journal(collection)) => {
                if !in_journal(collection) {
                    return Err(
                        "a file of the journal ring projects one of the ring's collections",
                    );
                }
                Some(collection)
            }
            (Class::Owner | Class::Mac, Store::Table { collection, .. }) => Some(collection),
        };
        if let Some(collection) = collection {
            if let Err(why) = fits(self.class, collection) {
                return Err(why);
            }
        } else if !matches!(self.computed, Computed::None) {
            return Err("a view's columns are its fold's; it declares no computed column");
        }
        let columns = self.stored();
        let mut i = 0;
        while i < columns.len() {
            if holds_secret(&columns[i].ty) {
                return Err(
                    "a shared file holds no Secret or Sealed column: every member reads it, \
                     and its snapshot is a plain local file",
                );
            }
            i += 1;
        }
        let computed = self.computed_columns();
        let mut d = 0;
        while d < computed.len() {
            if same(computed[d], "pk") {
                return Err("a computed column cannot take the key's name");
            }
            let mut i = 0;
            while i < columns.len() {
                if same(computed[d], columns[i].name) {
                    return Err("a computed column cannot take a stored column's name");
                }
                i += 1;
            }
            d += 1;
        }
        let mut h = 0;
        while h < self.handles.len() {
            let mut found = false;
            let mut i = 0;
            while i < columns.len() {
                found = found || same(self.handles[h], columns[i].name);
                i += 1;
            }
            let mut d = 0;
            while d < computed.len() {
                found = found || same(self.handles[h], computed[d]);
                d += 1;
            }
            if !found {
                return Err("a handle names a column of the file");
            }
            h += 1;
        }
        Ok(())
    }

    /// The file's name, parsed.
    ///
    /// # Panics
    /// Never for a member of [`FILES`], whose names [`Type::check`] held to the grammar when
    /// this crate was compiled.
    #[must_use]
    pub fn file(&self) -> Name {
        match Name::parse(self.name) {
            Ok(name) => name,
            Err(error) => unreachable!("{}: {error}", self.name),
        }
    }

    /// The row `aterm file ls` and the reader's verbs know this file by.
    #[must_use]
    pub const fn known(&self) -> aterm_file::cli::Known {
        aterm_file::cli::Known {
            name: self.name,
            kind: self.kind(),
            about: self.about,
        }
    }
}

/// `files` once every one passes [`Type::check`]; the build fails on the first that does not.
const fn checked<const N: usize>(files: [Type; N]) -> [Type; N] {
    let mut i = 0;
    while i < N {
        if let Err(why) = files[i].check() {
            panic!("{}", why);
        }
        let mut j = 0;
        while j < i {
            assert!(
                !same(files[i].name, files[j].name),
                "two shared files share a name"
            );
            j += 1;
        }
        i += 1;
    }
    files
}

/// `ai/accounts`: the accounts at AI providers (§10, file 1), the registry's `account` rows.
pub const ACCOUNTS: Type = Type {
    name: "ai/accounts",
    about: "accounts at AI providers: provider word, address, slug, status",
    class: Class::Owner,
    store: Store::Table {
        family: &att::REGISTRY,
        collection: &ACCOUNT,
    },
    computed: Computed::Accounts,
    handles: &["slug", "name", "email"],
};

/// `ai/links`: which machine prefers which account, in what order (§10, file 2), the registry's
/// `preference` rows. A link orders a pool; it is never identity and never a limit.
pub const LINKS: Type = Type {
    name: "ai/links",
    about: "which machine prefers which account, and in what order",
    class: Class::Owner,
    store: Store::Table {
        family: &att::REGISTRY,
        collection: &PREFERENCE,
    },
    computed: Computed::Links,
    handles: &[],
};

/// `lab/machines`: the lab's Macs, the registry's `machine` rows.
pub const MACHINES: Type = Type {
    name: "lab/machines",
    about: "the lab's Macs: label, status, role, hardware",
    class: Class::Owner,
    store: Store::Table {
        family: &att::REGISTRY,
        collection: &MACHINE,
    },
    computed: Computed::Machines,
    handles: &["label", "nickname"],
};

/// `ai/logins`: each Mac's logins, one row per account it holds a login for (§10, file 3), the
/// journal ring's `login` rows. A Mac states only its own rows; one at another Mac's key is
/// inert.
pub const LOGINS: Type = Type {
    name: "ai/logins",
    about: "each Mac's logins: which account, its state, how it was proved and last tried",
    class: Class::Mac,
    store: Store::Journal(&LOGIN),
    computed: Computed::Kept,
    handles: &[],
};

/// `ai/usage`: each Mac's latest reading of each account's limit windows (§10, file 4), the
/// journal ring's `usage` rows.
pub const USAGE: Type = Type {
    name: "ai/usage",
    about: "each Mac's latest reading of each account's windows, walls and sessions",
    class: Class::Mac,
    store: Store::Journal(&journal::USAGE),
    computed: Computed::Kept,
    handles: &[],
};

/// `ai/events`: what each Mac saw, one row per event of its `ev` batches (§10, file 5), in the
/// journal's closed vocabulary.
pub const EVENTS: Type = Type {
    name: "ai/events",
    about: "what each Mac saw while it used the accounts, in a closed vocabulary",
    class: Class::Mac,
    store: Store::Journal(&EV),
    computed: Computed::Kept,
    handles: &[],
};

/// `ai/limits`: every Mac's `ai/usage` rows folded, one row per account or sub-account (§10,
/// file 6). The first derived file: nobody writes it.
pub const LIMITS: Type = Type {
    name: "ai/limits",
    about: "each account's windows and walls, folded from every Mac's readings",
    class: Class::Derived,
    store: Store::View(Fold::Limits),
    computed: Computed::None,
    handles: &["account_slug"],
};

const COMPILED: [Type; 7] = checked([ACCOUNTS, LINKS, MACHINES, LOGINS, USAGE, EVENTS, LIMITS]);

/// Every shared file this build compiles; a view after the files it folds. `lab/emails`,
/// `lab/mailboxes` and `lab/phones` wait for LT-04: their collections hold a `Secret` column,
/// which [`Type::check`] refuses.
pub const FILES: &[Type] = &COMPILED;

/// The file named `name`.
#[must_use]
pub fn find(name: &Name) -> Option<&'static Type> {
    let name = name.to_string();
    FILES.iter().find(|file| file.name == name)
}

/// The file that projects the collection named `collection`: what a `Ref` column's cells name
/// the rows of.
#[must_use]
pub fn of_collection(collection: &str) -> Option<&'static Type> {
    FILES.iter().find(|file| {
        file.table()
            .is_some_and(|(_, bound)| bound.name == collection)
    })
}

/// What the reader's verbs know of every file here.
#[must_use]
pub fn known() -> Vec<aterm_file::cli::Known> {
    FILES.iter().map(Type::known).collect()
}
