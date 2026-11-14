// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Where a shared file's parts live on this Mac (docs/DESIGN-shared-files-2026-11-11.md §3):
//!
//! ```text
//! <root>/<set>/<leaf>.<ext>                         face (0600; folder 0700)
//! <root>/.ubd/state.noindex/<set>/<leaf>.snap       snapshot
//!                                 <leaf>.state.json status
//!                                 <leaf>.frontier   what the snapshot was projected from
//!                                 <leaf>.base/<rev>.json  baselines
//!                                 <leaf>.outbox.jsonl, .done, .held.jsonl
//!                                 <leaf>.outbox.lock  held while those three change
//!                                 <leaf>.history.jsonl    newest 4,096 operations
//!                                 <leaf>.lock       held while a pass records
//! ```
//!
//! The root is `~/.aterm/files`, and every caller hands it in ([`Root`]), so a test names a
//! scratch one. The state folder sits in a `.ubd` folder with a `.noindex` leaf, so neither
//! Spotlight nor fseventsd follows a pass's writes. The frontier is this crate's addition to the
//! design's listing: the monotone rule compares what two snapshots were projected from.

use crate::Name;
use std::path::{Path, PathBuf};

/// The state folder under a root.
pub const STATE: &str = ".ubd/state.noindex";
/// A snapshot's extension.
pub const SNAPSHOT: &str = "snap";

/// The folder every shared file of one Mac lives under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Root(PathBuf);

impl Root {
    /// The root at `path`.
    #[must_use]
    pub fn at(path: &Path) -> Self {
        Self(path.to_path_buf())
    }

    /// `<home>/.aterm/files`.
    #[must_use]
    pub fn for_home(home: &Path) -> Self {
        Self(home.join(".aterm").join("files"))
    }

    /// This user's root, `~/.aterm/files`; `None` with no home directory.
    #[must_use]
    pub fn live() -> Option<Self> {
        aterm_types::dirs::home_dir().map(|home| Self::for_home(&home))
    }

    /// The root folder.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.0
    }

    /// The folder `name`'s face is in.
    #[must_use]
    pub fn face_dir(&self, name: &Name) -> PathBuf {
        self.0.join(name.set())
    }

    /// `name`'s face, with the extension its kind writes (`tsv` for a table).
    #[must_use]
    pub fn face(&self, name: &Name, ext: &str) -> PathBuf {
        self.face_dir(name).join(format!("{}.{ext}", name.leaf()))
    }

    /// The folder `name`'s state is in.
    #[must_use]
    pub fn state_dir(&self, name: &Name) -> PathBuf {
        self.0.join(STATE).join(name.set())
    }

    fn sidecar(&self, name: &Name, suffix: &str) -> PathBuf {
        self.state_dir(name)
            .join(format!("{}.{suffix}", name.leaf()))
    }

    /// `name`'s snapshot.
    #[must_use]
    pub fn snapshot(&self, name: &Name) -> PathBuf {
        self.sidecar(name, SNAPSHOT)
    }

    /// `name`'s status.
    #[must_use]
    pub fn state(&self, name: &Name) -> PathBuf {
        self.sidecar(name, "state.json")
    }

    /// What `name`'s snapshot was projected from.
    #[must_use]
    pub fn frontier(&self, name: &Name) -> PathBuf {
        self.sidecar(name, "frontier")
    }

    /// The folder of `name`'s baselines.
    #[must_use]
    pub fn baselines(&self, name: &Name) -> PathBuf {
        self.sidecar(name, "base")
    }

    /// `name`'s baseline at revision `rev`.
    #[must_use]
    pub fn baseline(&self, name: &Name, rev: u64) -> PathBuf {
        self.baselines(name).join(format!("{rev}.json"))
    }

    /// `name`'s outbox of unsigned write intents.
    #[must_use]
    pub fn outbox(&self, name: &Name) -> PathBuf {
        self.sidecar(name, "outbox.jsonl")
    }

    /// The marker of `name`'s intents a record already carries.
    #[must_use]
    pub fn done(&self, name: &Name) -> PathBuf {
        self.sidecar(name, "done")
    }

    /// `name`'s held intents.
    #[must_use]
    pub fn held(&self, name: &Name) -> PathBuf {
        self.sidecar(name, "held.jsonl")
    }

    /// The lock a writer holds while it changes `name`'s outbox, held list or done markers.
    #[must_use]
    pub fn outbox_lock(&self, name: &Name) -> PathBuf {
        self.sidecar(name, "outbox.lock")
    }

    /// `name`'s newest operations.
    #[must_use]
    pub fn history(&self, name: &Name) -> PathBuf {
        self.sidecar(name, "history.jsonl")
    }

    /// The lock a pass holds while it records `name`.
    #[must_use]
    pub fn lock(&self, name: &Name) -> PathBuf {
        self.sidecar(name, "lock")
    }

    /// The shared file whose face `path` is, by its path alone: `<root>/<set>/<leaf>.<ext>`.
    #[must_use]
    pub fn name_of(&self, path: &Path) -> Option<Name> {
        let rest = path.strip_prefix(&self.0).ok()?;
        let mut parts = rest.components();
        let set = parts.next()?.as_os_str().to_str()?;
        let file = Path::new(parts.next()?.as_os_str());
        if parts.next().is_some() {
            return None;
        }
        file.extension()?;
        let leaf = file.file_stem()?.to_str()?;
        Name::parse(&format!("{set}/{leaf}")).ok()
    }

    /// Every file a pass has recorded here, by name: the status files under the state folder.
    #[must_use]
    pub fn recorded(&self) -> Vec<Name> {
        let mut names = Vec::new();
        let Ok(sets) = std::fs::read_dir(self.0.join(STATE)) else {
            return names;
        };
        for set in sets.flatten() {
            let Some(set_name) = set.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Ok(files) = std::fs::read_dir(set.path()) else {
                continue;
            };
            for file in files.flatten() {
                let Some(leaf) = file
                    .file_name()
                    .to_str()
                    .and_then(|name| name.strip_suffix(".state.json"))
                    .map(str::to_owned)
                else {
                    continue;
                };
                if let Ok(name) = Name::parse(&format!("{set_name}/{leaf}")) {
                    names.push(name);
                }
            }
        }
        names.sort();
        names
    }
}
