// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The closed word lists a row prints (docs/DESIGN-disk-map-2026-11-18.md §3.2, §6.1).
//! Each list reads back what it prints and nothing else.

macro_rules! words {
    ($(#[$doc:meta])* $name:ident { $($(#[$vdoc:meta])* $variant:ident = $word:literal,)+ }) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum $name { $($(#[$vdoc])* $variant,)+ }

        impl $name {
            /// Every word, in list order.
            pub const ALL: [Self; [$(Self::$variant),+].len()] = [$(Self::$variant),+];

            /// The word a row prints.
            #[must_use]
            pub const fn word(self) -> &'static str {
                match self { $(Self::$variant => $word,)+ }
            }

            /// The value a printed word names; `None` for any word not on the list.
            #[must_use]
            pub fn from_word(word: &str) -> Option<Self> {
                Self::ALL.into_iter().find(|v| v.word() == word)
            }
        }
    };
}

words! {
    /// A fixed region of the volume.
    Region {
        /// The system's own bytes.
        Sys = "sys",
        /// The home folder outside repositories and temp.
        Home = "home",
        /// Repositories and their checkouts.
        Git = "git",
        /// Temp roots.
        Tmp = "tmp",
        /// Free space.
        Free = "free",
    }
}

words! {
    /// A class: what the bytes are, from evidence. Bytes with none are `unexplained`.
    Class {
        /// The system's own.
        System = "system",
        /// A person's files.
        Personal = "personal",
        /// In git, and a remote has it.
        Pushed = "pushed",
        /// Commits no remote has.
        Unpushed = "unpushed",
        /// Edited, untracked or snapshotted.
        Unique = "unique",
        /// A git store.
        Store = "store",
        /// Build output.
        Build = "build",
        /// A tool cache, proof certificates and proof-run output among them.
        Cache = "cache",
        /// Temporary.
        Temp = "temp",
        /// Re-fetchable, by a rule.
        Data = "data",
        /// Hand-made VM images.
        Costly = "costly",
        /// Read, and no rule says what it is.
        Unexplained = "unexplained",
        /// Not read yet.
        Unread = "unread",
    }
}

words! {
    /// The proof words a row prints: why bytes are temporary or held elsewhere.
    Proof {
        /// A build tool's marker at the candidate's own root.
        Marker = "marker",
        /// Git's registry of linked worktrees names it.
        GitRegistry = "git-registry",
        /// Every pin was read back.
        PinsReadBack = "pins-read-back",
        /// Every session tied to it has a recorded end.
        Ended = "ended",
        /// A tag with a known producer, or a known cache layout.
        ToolCache = "tool-cache",
        /// Under a temp root.
        TempRoot = "temp-root",
        /// Its own content hash is in the source's listing.
        HashListed = "hash-listed",
        /// An installer receipt for a build still published.
        Receipt = "receipt",
        /// A duplicate clone.
        Duplicate = "duplicate",
    }
}

words! {
    /// A kind of file, for bytes by kind.
    FileKind {
        /// Source.
        Source = "source",
        /// Git objects.
        GitObjects = "git-objects",
        /// Compiled libraries.
        Libraries = "libraries",
        /// Executables.
        Executables = "executables",
        /// Incremental state.
        Incremental = "incremental",
        /// Package stores.
        Packages = "packages",
        /// Documents and a person's text.
        Documents = "documents",
        /// Media.
        Media = "media",
        /// Data.
        Data = "data",
        /// Logs.
        Logs = "logs",
        /// Archives.
        Archives = "archives",
        /// Anything else.
        Other = "other",
    }
}

words! {
    /// Whether a row's bytes were measured by the survey or carried from the ledger.
    Measured {
        /// Measured by the survey.
        Yes = "yes",
        /// Carried from the holders ledger at second 0.
        Carried = "carried",
    }
}

/// A named part of a checkout.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Part {
    /// Tracked files a remote has.
    Pushed,
    /// Commits no remote has.
    Unpushed,
    /// Edited tracked files.
    Dirty,
    /// Untracked files.
    Untracked,
    /// Git-ignored files under one prefix.
    Ignored(String),
    /// The git store.
    Store,
}

impl Part {
    /// The word a row prints: `pushed` … `ignored/<prefix>` … `store`.
    #[must_use]
    pub fn word(&self) -> std::borrow::Cow<'static, str> {
        match self {
            Self::Pushed => "pushed".into(),
            Self::Unpushed => "unpushed".into(),
            Self::Dirty => "dirty".into(),
            Self::Untracked => "untracked".into(),
            Self::Ignored(prefix) => format!("ignored/{prefix}").into(),
            Self::Store => "store".into(),
        }
    }

    /// The part a printed word names. An `ignored/` prefix is one or more components,
    /// none empty, `.` or `..`.
    #[must_use]
    pub fn from_word(word: &str) -> Option<Self> {
        Some(match word {
            "pushed" => Self::Pushed,
            "unpushed" => Self::Unpushed,
            "dirty" => Self::Dirty,
            "untracked" => Self::Untracked,
            "store" => Self::Store,
            _ => {
                let prefix = word.strip_prefix("ignored/")?;
                if prefix
                    .split('/')
                    .any(|c| c.is_empty() || c == "." || c == "..")
                {
                    return None;
                }
                Self::Ignored(prefix.to_owned())
            }
        })
    }

    /// The class of a part's bytes: a part is its own evidence, except ignored bytes,
    /// which only a marker proves to be build output (§6.2, DC-D19).
    #[must_use]
    pub fn class(&self, marker_proves: bool) -> crate::Class {
        use crate::Class;
        match self {
            Self::Pushed => Class::Pushed,
            Self::Unpushed => Class::Unpushed,
            Self::Dirty | Self::Untracked => Class::Unique,
            Self::Store => Class::Store,
            Self::Ignored(_) if marker_proves => Class::Build,
            Self::Ignored(_) => Class::Unexplained,
        }
    }
}

/// What the cleaner does with a node.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Fate {
    /// Stays.
    Stays,
    /// Trimmed at build end.
    Trim,
    /// Removed when its lifetime runs out.
    Remove,
    /// Pinned in its repository's store, then removed.
    Pin,
    /// Only its build output goes.
    Strip,
    /// Would go, but something holds it.
    Held(String),
    /// No fate yet.
    None,
}

impl Fate {
    /// The word a row prints.
    #[must_use]
    pub fn word(&self) -> std::borrow::Cow<'static, str> {
        match self {
            Self::Stays => "stays".into(),
            Self::Trim => "trim".into(),
            Self::Remove => "remove".into(),
            Self::Pin => "pin".into(),
            Self::Strip => "strip".into(),
            Self::Held(why) => format!("held({why})").into(),
            Self::None => "none".into(),
        }
    }

    /// The fate a printed word names; a `held` reason is non-empty and has no parenthesis.
    #[must_use]
    pub fn from_word(word: &str) -> Option<Self> {
        Some(match word {
            "stays" => Self::Stays,
            "trim" => Self::Trim,
            "remove" => Self::Remove,
            "pin" => Self::Pin,
            "strip" => Self::Strip,
            "none" => Self::None,
            _ => {
                let why = word.strip_prefix("held(")?.strip_suffix(')')?;
                if why.is_empty() || why.contains(['(', ')']) {
                    return None;
                }
                Self::Held(why.to_owned())
            }
        })
    }

    /// Whether the fate removes bytes: `trim`, `remove`, `pin` and `strip` do.
    #[must_use]
    pub const fn removes(&self) -> bool {
        matches!(self, Self::Trim | Self::Remove | Self::Pin | Self::Strip)
    }
}
