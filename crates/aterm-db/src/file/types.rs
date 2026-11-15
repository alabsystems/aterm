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
//! statement of its own rows. The class is checked against the collection's kind when the build
//! compiles ([`Type::check`]), and the publisher holds an intent the file's class does not take
//! ([`Class::refuses`]). That is all the class enforces yet: code that writes the registry
//! itself (a Mac's self-registration, the importer) passes no outbox, and the materializer
//! projects its cells as any other until it judges them by the merge rules (`file::admit`, not
//! built yet).

use crate::{
    alab::registry::{ACCOUNT, MACHINE, PREFERENCE},
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
        }
    }
}

/// One shared file of this build.
#[derive(Clone, Copy, Debug)]
pub struct Type {
    /// `<set>/<leaf>`.
    pub name: &'static str,
    /// What `ls` says of it.
    pub about: &'static str,
    /// The first generation of the table family whose replica holds it.
    pub family: &'static Compiled,
    /// Who writes it.
    pub class: Class,
    /// The collection it projects.
    pub collection: &'static Collection,
    pub computed: Computed,
    /// The columns a row is named by besides its key, in the order a name is tried.
    pub handles: &'static [&'static str],
}

/// A file's kind word and its face's extension: every file of this slice is a table.
pub const TABLE: &str = "table";
/// The extension of a table's face.
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

impl Type {
    /// Whether the declaration may be a shared file.
    ///
    /// # Errors
    /// The first rule it breaks: its name is not `<set>/<leaf>`; its class does not fit its
    /// collection (an owner file projects a cells collection the owner writes, a Mac file an
    /// owned collection keyed by its writer's uuid or a log); it projects anything but a cells
    /// collection, the only kind this build's materializer projects, so no Mac file compiles
    /// yet; a column of the collection, live or retired, can hold a `Secret` or a `Sealed`
    /// record (a retired column's cells still replicate); or a handle or computed column takes
    /// a stored column's name or the key's.
    pub const fn check(&self) -> Result<(), &'static str> {
        if !aterm_file::name::well_formed(self.name) {
            return Err("a shared file is named <set>/<leaf>, each part [a-z][a-z0-9-]{0,19}");
        }
        // Every kind and writer is placed in each class's table, so a new one fails to compile
        // here until someone says which class takes it.
        let (kind, writers) = (self.collection.kind, self.collection.writers);
        match self.class {
            Class::Owner => match (kind, writers) {
                (Kind::Cells, Writers::Owner) => {}
                (Kind::Cells, Writers::SelfKey | Writers::SelfSuffix)
                | (
                    Kind::Owned | Kind::Log,
                    Writers::Owner | Writers::SelfKey | Writers::SelfSuffix,
                ) => {
                    return Err("an owner file projects a cells collection the owner writes");
                }
            },
            Class::Mac => match (kind, writers) {
                (Kind::Owned, Writers::SelfKey | Writers::SelfSuffix)
                | (Kind::Log, Writers::SelfSuffix) => {}
                (Kind::Cells, Writers::Owner | Writers::SelfKey | Writers::SelfSuffix)
                | (Kind::Owned, Writers::Owner)
                | (Kind::Log, Writers::Owner | Writers::SelfKey) => {
                    return Err(
                        "a Mac file projects an owned collection keyed by its writer's uuid, or a log",
                    );
                }
            },
        }
        if !matches!(self.collection.kind, Kind::Cells) {
            return Err(
                "this build projects only cells collections: no Mac file compiles until its \
                 materializer does",
            );
        }
        let columns = self.collection.columns;
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
        let computed = self.computed.columns();
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
            kind: TABLE,
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
    family: &att::REGISTRY,
    class: Class::Owner,
    collection: &ACCOUNT,
    computed: Computed::Accounts,
    handles: &["slug", "name", "email"],
};

/// `ai/links`: which machine prefers which account, in what order (§10, file 2), the registry's
/// `preference` rows. A link orders a pool; it is never identity and never a limit.
pub const LINKS: Type = Type {
    name: "ai/links",
    about: "which machine prefers which account, and in what order",
    family: &att::REGISTRY,
    class: Class::Owner,
    collection: &PREFERENCE,
    computed: Computed::Links,
    handles: &[],
};

/// `lab/machines`: the lab's Macs, the registry's `machine` rows.
pub const MACHINES: Type = Type {
    name: "lab/machines",
    about: "the lab's Macs: label, status, role, hardware",
    family: &att::REGISTRY,
    class: Class::Owner,
    collection: &MACHINE,
    computed: Computed::Machines,
    handles: &["label", "nickname"],
};

const COMPILED: [Type; 3] = checked([ACCOUNTS, LINKS, MACHINES]);

/// Every shared file this build compiles. `lab/emails`, `lab/mailboxes` and `lab/phones` wait
/// for LT-04: their collections hold a `Secret` column, which [`Type::check`] refuses.
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
    FILES.iter().find(|file| file.collection.name == collection)
}

/// What the reader's verbs know of every file here.
#[must_use]
pub fn known() -> Vec<aterm_file::cli::Known> {
    FILES.iter().map(Type::known).collect()
}
