// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The shared files this build compiles (docs/DESIGN-shared-files-2026-11-11.md §3, §10): each a
//! name bound to one cells collection of a typed table family that already replicates. A file
//! adds no storage and no wire: the stored collection keeps its name, so old and new builds
//! share one history.
//!
//! Every enrolled member reads every shared file, and its snapshot and face sit in plain local
//! files (§5, §13 decision 2). A file therefore holds no `Secret` and no `Sealed` column, at any
//! depth of a column's type: [`Type::check`] says so, and [`FILES`] is built through it, so a
//! file bound to such a collection does not compile.

use crate::{
    alab::registry::{ACCOUNT, MACHINE, PREFERENCE},
    att::{self, Compiled},
    typed::{Collection, Kind, Ty},
};
use aterm_file::Name;

/// The columns a file derives at projection, never stored as cells (§3, "Face format").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Derived {
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

impl Derived {
    /// The derived columns' names, in face order.
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
    /// The cells collection it projects.
    pub collection: &'static Collection,
    pub derived: Derived,
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
    /// The first rule it breaks: its name is not `<set>/<leaf>`; it projects anything but a
    /// cells collection; a column of the collection, live or retired, can hold a `Secret` or a
    /// `Sealed` record (a retired column's cells still replicate); or a handle or derived
    /// column takes a stored column's name or the key's.
    pub const fn check(&self) -> Result<(), &'static str> {
        if !aterm_file::name::well_formed(self.name) {
            return Err("a shared file is named <set>/<leaf>, each part [a-z][a-z0-9-]{0,19}");
        }
        if !matches!(self.collection.kind, Kind::Cells) {
            return Err("a shared table projects a cells collection");
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
        let derived = self.derived.columns();
        let mut d = 0;
        while d < derived.len() {
            if same(derived[d], "pk") {
                return Err("a derived column cannot take the key's name");
            }
            let mut i = 0;
            while i < columns.len() {
                if same(derived[d], columns[i].name) {
                    return Err("a derived column cannot take a stored column's name");
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
            while d < derived.len() {
                found = found || same(self.handles[h], derived[d]);
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
    collection: &ACCOUNT,
    derived: Derived::Accounts,
    handles: &["slug", "name", "email"],
};

/// `ai/links`: which machine prefers which account, in what order (§10, file 2), the registry's
/// `preference` rows. A link orders a pool; it is never identity and never a limit.
pub const LINKS: Type = Type {
    name: "ai/links",
    about: "which machine prefers which account, and in what order",
    family: &att::REGISTRY,
    collection: &PREFERENCE,
    derived: Derived::Links,
    handles: &[],
};

/// `lab/machines`: the lab's Macs, the registry's `machine` rows.
pub const MACHINES: Type = Type {
    name: "lab/machines",
    about: "the lab's Macs: label, status, role, hardware",
    family: &att::REGISTRY,
    collection: &MACHINE,
    derived: Derived::Machines,
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
