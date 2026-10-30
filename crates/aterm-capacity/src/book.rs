// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BOOK OF MEASURED PEAKS (`docs/DESIGN-adaptive-build-2026-10-24.md`, VERIFY-MEM and
//! BUILD-ADMIT): what a kind of work took at most, kept so its next run claims that and a margin
//! ([`crate::peak_claim`]) and not the cold figure ([`crate::cold_claim`]).
//!
//! A record is kept for a [`Key`]: the work (a verify stage, `TestCompile`; a plain build,
//! `targo test`), the checkout it ran in (each builds into a cache of its own, and a warm cache
//! compiles less than a cold one), the sorted crate selection where the work reads one, and what
//! else changes the work (its variant). It keeps the last [`KEPT`] peaks, so a light run never
//! hides the heavy one before it, and a file keeps at most [`ROWS_KEPT`] records. A plain build
//! keeps no light run at all while its key's heaviest peak is young ([`Records::light`]): it is
//! run over an unchanged tree far more often than verify runs a stage.
//!
//! Two files hold records, one beside the other in this user's aterm state directory: verify's
//! stages ([`VERIFY_RECORD`]) and plain `targo` builds ([`TARGO_RECORD`]). Verify reads its own
//! with [`Records::learned`]: the key's record, else the most of a wider selection's in the same
//! checkout. A plain build reads further ([`chain`]): the most of every record of the same work,
//! its key's own, its key's in another checkout (a new worktree is the common case), and verify's
//! record of the stage whose work holds its own (its analogue: verify's test compile for
//! `targo test`, its lint for `tippy`, read as verify reads it, or in another checkout), so a
//! light run (a fresh build that compiled nothing) never bounds it below a heavy one measured
//! for the same work; only with none of those, a wider selection's in its checkout; and with
//! nothing at all, the cold figure.

use std::fmt;
use std::io::{self, Write as _};
use std::path::Path;

use aterm_json::{Map, Value};

/// The peaks one record keeps, newest last.
pub const KEPT: usize = 8;

/// The records a file keeps: past this, the one whose newest peak is oldest goes, so change
/// cones that never come back do not grow it without end.
pub const ROWS_KEPT: usize = 512;

/// The records' schema: 2 keys each record by its checkout and variant as well ([`Key`]); a file
/// of schema 1, keyed by the work and selection alone, is read as none, and the next keep writes
/// schema 2 over it.
pub const SCHEMA: u64 = 2;

/// How long a plain build's heaviest kept peak stands against light runs ([`Records::light`]):
/// two weeks, unix ms.
pub const HEAVY_STANDS_MS: u64 = 14 * 24 * 60 * 60 * 1000;

/// A plain build's run is light beside its key's heaviest kept peak below this share of it
/// ([`Records::light`]): a quarter.
pub const LIGHT_SHARE: u64 = 4;

/// Verify's stages' records, under this user's aterm state directory.
pub const VERIFY_RECORD: &str = "verify/stage-memory.json";

/// Plain `targo` builds' records, beside verify's ([`VERIFY_RECORD`]).
pub const TARGO_RECORD: &str = "verify/targo-memory.json";

/// WHAT A RECORD IS KEPT FOR: a kind of work, the checkout it ran in, its crate selection,
/// sorted, where the work reads one, and what else of the run changes its work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Key {
    /// The work: a verify stage by its name (`TestCompile`), a plain build by its verb
    /// (`targo test`).
    pub stage: String,
    /// The checkout's root, as the run named it: each builds into a cache of its own.
    pub checkout: String,
    /// The selection's crates, sorted and without repeats; `None` for the whole workspace, which
    /// holds every selection, and for work that is the same whatever is selected.
    pub crates: Option<Vec<String>>,
    /// What else of the run changes the work: the test binaries run at once for verify's test
    /// run (`jobs 4`), the cells asked for, a plain build's profile and targets (`release`).
    pub variant: Option<String>,
}

impl Key {
    /// Whether this key's selection holds `other`'s, for the same work, checkout and variant:
    /// the workspace holds every selection, and a list holds one whose every crate it names.
    #[must_use]
    pub fn holds(&self, other: &Key) -> bool {
        self.stage == other.stage
            && self.checkout == other.checkout
            && self.variant == other.variant
            && match (&self.crates, &other.crates) {
                (None, _) => true,
                (Some(_), None) => false,
                (Some(mine), Some(theirs)) => theirs.iter().all(|c| mine.contains(c)),
            }
    }

    /// Whether this key is `other` in another checkout: the same work, selection and variant.
    #[must_use]
    pub fn elsewhere(&self, other: &Key) -> bool {
        self.stage == other.stage
            && self.checkout != other.checkout
            && self.crates == other.crates
            && self.variant == other.variant
    }

    /// The selection as a person reads it: `the workspace`, or `-p a -p b`.
    #[must_use]
    pub fn selection(&self) -> String {
        match &self.crates {
            None => "the workspace".to_owned(),
            Some(c) if c.is_empty() => "no crate".to_owned(),
            Some(c) => c
                .iter()
                .map(|n| format!("-p {n}"))
                .collect::<Vec<_>>()
                .join(" "),
        }
    }

    /// The key as the file holds it: `crates` and `variant` left out when unset.
    fn encode(&self) -> Value {
        let mut o = Map::new();
        o.insert("stage".to_owned(), Value::from(self.stage.as_str()));
        o.insert("checkout".to_owned(), Value::from(self.checkout.as_str()));
        if let Some(crates) = &self.crates {
            o.insert(
                "crates".to_owned(),
                Value::Array(crates.iter().map(|c| Value::from(c.as_str())).collect()),
            );
        }
        if let Some(variant) = &self.variant {
            o.insert("variant".to_owned(), Value::from(variant.as_str()));
        }
        Value::Object(o)
    }

    /// The key `v` holds; `None` where it holds none: `stage` and `checkout` are strings,
    /// `crates` (absent or null for none) a list of strings, `variant` (likewise) a string.
    fn decode(v: &Value) -> Option<Key> {
        let text = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
        let crates = match v.get("crates") {
            None | Some(Value::Null) => None,
            Some(list) => Some(
                list.as_array()?
                    .iter()
                    .map(|c| c.as_str().map(str::to_owned))
                    .collect::<Option<Vec<_>>>()?,
            ),
        };
        let variant = match v.get("variant") {
            None | Some(Value::Null) => None,
            Some(s) => Some(s.as_str()?.to_owned()),
        };
        Some(Key {
            stage: text("stage")?,
            checkout: text("checkout")?,
            crates,
            variant,
        })
    }
}

/// One measured peak.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Peak {
    /// The peak, bytes.
    pub bytes: u64,
    /// When the work ended, unix ms.
    pub at_ms: u64,
}

/// The peaks one key keeps, newest last, at most [`KEPT`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// What they were measured for.
    pub key: Key,
    /// The peaks.
    pub peaks: Vec<Peak>,
}

impl Row {
    /// The most it keeps.
    fn most(&self) -> Option<u64> {
        self.peaks.iter().map(|p| p.bytes).max()
    }

    /// When its newest peak was kept.
    fn newest(&self) -> u64 {
        self.peaks.iter().map(|p| p.at_ms).max().unwrap_or(0)
    }
}

/// What a lookup read ([`Records::learned`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Learned {
    /// The peak read, bytes.
    pub peak: u64,
    /// How many peaks its record keeps.
    pub runs: usize,
    /// The other record it was read from (a wider selection's, or another checkout's); `None`
    /// for the key's own record.
    pub wider: Option<Key>,
}

/// EVERY KEPT PEAK OF ONE FILE ([`Row`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Records {
    rows: Vec<Row>,
}

impl Records {
    /// The records.
    #[must_use]
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Keep `bytes`, measured at `at_ms`, under `key`: its record's oldest peak goes past
    /// [`KEPT`], and past [`ROWS_KEPT`] records the one whose newest peak is oldest goes.
    pub fn push(&mut self, key: Key, bytes: u64, at_ms: u64) {
        let at = match self.rows.iter().position(|r| r.key == key) {
            Some(at) => at,
            None => {
                self.rows.push(Row {
                    key,
                    peaks: Vec::new(),
                });
                self.rows.len() - 1
            }
        };
        let peaks = &mut self.rows[at].peaks;
        peaks.push(Peak { bytes, at_ms });
        let over = peaks.len().saturating_sub(KEPT);
        peaks.drain(..over);
        while self.rows.len() > ROWS_KEPT {
            let oldest = (0..self.rows.len())
                .filter(|i| *i != at)
                .min_by_key(|i| self.rows[*i].newest());
            match oldest {
                Some(i) => {
                    self.rows.remove(i);
                }
                None => break,
            }
        }
    }

    /// What `key`'s work took: the most its own record keeps; else the most any record of a wider
    /// selection of the same work, checkout and variant keeps ([`Key::holds`]: a superset's build
    /// compiles the subset's crates too, and the most of them, not the least, since a wider run
    /// over a warmer cache can have taken less than this one will); `None` where neither is kept.
    #[must_use]
    pub fn learned(&self, key: &Key) -> Option<Learned> {
        if let Some(row) = self.rows.iter().find(|r| r.key == *key)
            && let Some(peak) = row.most()
        {
            return Some(Learned {
                peak,
                runs: row.peaks.len(),
                wider: None,
            });
        }
        self.most_of(|k| k != key && k.holds(key))
    }

    /// What `key`'s work took in another checkout: the most any record of the same work,
    /// selection and variant there keeps ([`Key::elsewhere`]); `None` where none is kept.
    #[must_use]
    pub fn elsewhere(&self, key: &Key) -> Option<Learned> {
        self.most_of(|k| k.elsewhere(key))
    }

    /// The most any record whose key `pick` takes keeps, and that key.
    fn most_of(&self, pick: impl Fn(&Key) -> bool) -> Option<Learned> {
        self.rows
            .iter()
            .filter(|r| pick(&r.key))
            .filter_map(|r| r.most().map(|m| (m, r)))
            .max_by_key(|(m, _)| *m)
            .map(|(peak, r)| Learned {
                peak,
                runs: r.peaks.len(),
                wider: Some(r.key.clone()),
            })
    }

    /// The records as the file holds them, schema [`SCHEMA`].
    #[must_use]
    pub fn encode(&self) -> String {
        let rows = self
            .rows
            .iter()
            .map(|r| {
                let mut o = Map::new();
                o.insert("key".to_owned(), r.key.encode());
                o.insert(
                    "peaks".to_owned(),
                    Value::Array(
                        r.peaks
                            .iter()
                            .map(|p| {
                                let mut q = Map::new();
                                q.insert("bytes".to_owned(), Value::from(p.bytes));
                                q.insert("at_ms".to_owned(), Value::from(p.at_ms));
                                Value::Object(q)
                            })
                            .collect(),
                    ),
                );
                Value::Object(o)
            })
            .collect();
        let mut file = Map::new();
        file.insert("schema".to_owned(), Value::from(SCHEMA));
        file.insert("rows".to_owned(), Value::Array(rows));
        aterm_json::to_string(&Value::Object(file)).unwrap_or_else(|_| "{}".to_owned())
    }

    /// The records `text` holds; none for text that is not [`Self::encode`]'s, of another
    /// schema, or with any row it cannot read whole.
    #[must_use]
    pub fn decode(text: &str) -> Records {
        let decoded = || -> Option<Records> {
            let file: Value = aterm_json::from_str(text).ok()?;
            if file.get("schema").and_then(Value::as_u64) != Some(SCHEMA) {
                return None;
            }
            let rows = file
                .get("rows")?
                .as_array()?
                .iter()
                .map(|r| {
                    let key = Key::decode(r.get("key")?)?;
                    let peaks = r
                        .get("peaks")?
                        .as_array()?
                        .iter()
                        .map(|p| {
                            Some(Peak {
                                bytes: p.get("bytes").and_then(Value::as_u64)?,
                                at_ms: p.get("at_ms").and_then(Value::as_u64)?,
                            })
                        })
                        .collect::<Option<Vec<_>>>()?;
                    Some(Row { key, peaks })
                })
                .collect::<Option<Vec<_>>>()?;
            Some(Records { rows })
        };
        decoded().unwrap_or_default()
    }

    /// The records kept in `path`; none where it is absent or unreadable.
    #[must_use]
    pub fn load(path: &Path) -> Records {
        std::fs::read_to_string(path)
            .map(|t| Records::decode(&t))
            .unwrap_or_default()
    }

    /// Write the records to `path`: staged in a private folder beside it, then renamed over it,
    /// so a reader finds the old file or the new one whole. Two writers at once can lose one of
    /// their peaks; neither leaves a torn file.
    ///
    /// # Errors
    /// Its directory could not be made, or the write failed: the records kept before stand.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(dir)?;
        let stage = aterm_tempfile::Builder::new()
            .prefix(".stage-memory-")
            .tempdir_in(dir)?;
        let staged = stage.path().join("data");
        std::fs::File::create(&staged)?.write_all(self.encode().as_bytes())?;
        std::fs::rename(&staged, path)
    }

    /// Keep `bytes`, measured at `at_ms`, under `key` in the file at `path`: read, pushed
    /// ([`Self::push`]) and written back whole ([`Self::save`]).
    ///
    /// # Errors
    /// As [`Self::save`].
    pub fn keep(path: &Path, key: Key, bytes: u64, at_ms: u64) -> io::Result<()> {
        let mut records = Records::load(path);
        records.push(key, bytes, at_ms);
        records.save(path)
    }

    /// Whether a plain build's run of `bytes` at `at_ms` is LIGHT beside what `key`'s own record
    /// keeps: below a [`LIGHT_SHARE`] of its heaviest peak, which is younger than
    /// [`HEAVY_STANDS_MS`]. A plain build is run again over an unchanged tree far more often than
    /// verify runs a stage, and such a run compiles nothing: kept, [`KEPT`] of them would push the
    /// compile that set the key's peak out of its record, and its next compile would claim a fresh
    /// run's few MiB. `false` for a key with no record.
    #[must_use]
    pub fn light(&self, key: &Key, bytes: u64, at_ms: u64) -> bool {
        self.rows
            .iter()
            .find(|r| r.key == *key)
            .and_then(|r| r.peaks.iter().max_by_key(|p| (p.bytes, p.at_ms)))
            .is_some_and(|heavy| {
                bytes < heavy.bytes / LIGHT_SHARE
                    && at_ms.saturating_sub(heavy.at_ms) < HEAVY_STANDS_MS
            })
    }

    /// Keep a plain build's peak `bytes`, measured at `at_ms`, under `key` in the file at `path`
    /// ([`Self::keep`]), unless the run was light beside the key's heaviest peak ([`Self::light`]),
    /// which then stands in its record unchanged. Whether it was kept.
    ///
    /// # Errors
    /// As [`Self::save`].
    pub fn keep_unless_light(path: &Path, key: Key, bytes: u64, at_ms: u64) -> io::Result<bool> {
        let mut records = Records::load(path);
        if records.light(&key, bytes, at_ms) {
            return Ok(false);
        }
        records.push(key, bytes, at_ms);
        records.save(path).map(|()| true)
    }
}

/// Which rule of the [`chain`] a plain build's figure came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Via {
    /// Its own record.
    Own,
    /// A wider selection's record in the same checkout.
    Wider(Key),
    /// Its key's record in another checkout.
    Elsewhere(Key),
    /// Verify's record of the stage whose work holds its own: verify's own, a wider selection's
    /// in its checkout, or its key's in another checkout.
    Analogue(Key),
}

/// What [`chain`] found: the peak, how many peaks its record keeps, and where it was read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// The peak, bytes.
    pub peak: u64,
    /// How many peaks its record keeps.
    pub runs: usize,
    /// The rule it came from.
    pub via: Via,
}

/// WHAT A PLAIN BUILD TOOK, read further than verify reads its own: the most of every record of
/// the same work, `key`'s own in `own`, its key's in another checkout ([`Records::elsewhere`]),
/// and, where `analogue` names verify's records and the key of the stage whose work holds this
/// one's, that key as verify reads it ([`Records::learned`]) or in another checkout. A record of
/// the same work bounds the build however light its last runs were, since a fresh build that
/// compiled nothing keeps a peak of a few MiB, and the most of them is what a run that compiles
/// can take. Only where none of those is kept, the most of a wider selection's in `own` in its
/// checkout ([`Key::holds`]): that work holds this one's, but a light run of it would bound a
/// heavy compile of this one, and a heavy one would hold a light build to a workspace's peak.
/// `None` where nothing is kept: the build claims the cold figure. A tie is read as the build's
/// own.
#[must_use]
pub fn chain(own: &Records, key: &Key, analogue: Option<(&Records, &Key)>) -> Option<Found> {
    let found = |l: Learned, via: Via| Found {
        peak: l.peak,
        runs: l.runs,
        via,
    };
    let like = analogue.and_then(|(records, like)| {
        records
            .learned(like)
            .or_else(|| records.elsewhere(like))
            .map(|l| {
                let from = l.wider.clone().unwrap_or_else(|| like.clone());
                found(l, Via::Analogue(from))
            })
    });
    let far = own.elsewhere(key).map(|l| {
        let from = l.wider.clone().unwrap_or_else(|| key.clone());
        found(l, Via::Elsewhere(from))
    });
    let mine = own.rows.iter().find(|r| r.key == *key).and_then(|r| {
        r.most().map(|peak| Found {
            peak,
            runs: r.peaks.len(),
            via: Via::Own,
        })
    });
    if let Some(most) = [like, far, mine]
        .into_iter()
        .flatten()
        .max_by_key(|f| f.peak)
    {
        return Some(most);
    }
    own.most_of(|k| k != key && k.holds(key)).map(|l| {
        let from = l.wider.clone().unwrap_or_else(|| key.clone());
        found(l, Via::Wider(from))
    })
}

/// WHAT A PLAIN BUILD CLAIMS ([`need`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildNeed {
    /// Bytes.
    pub bytes: u64,
    /// Why: what [`chain`] found, or `None` for the cold figure.
    pub found: Option<Found>,
}

/// What a plain build whose record is kept under `key` claims on a machine of `physical` bytes
/// (`None`: unread): what [`chain`] finds in `own` and `analogue` and a fifth more, at least 1 GiB
/// more ([`crate::peak_claim`]); with none found, the cold figure ([`crate::cold_claim`]).
#[must_use]
pub fn need(
    own: &Records,
    key: &Key,
    analogue: Option<(&Records, &Key)>,
    physical: Option<u64>,
) -> BuildNeed {
    match chain(own, key, analogue) {
        Some(found) => BuildNeed {
            bytes: crate::peak_claim(found.peak),
            found: Some(found),
        },
        None => BuildNeed {
            bytes: crate::cold_claim(physical),
            found: None,
        },
    }
}

impl fmt::Display for BuildNeed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bytes = crate::size(self.bytes);
        let Some(found) = &self.found else {
            return write!(
                f,
                "{bytes} cold: no build of it is kept here or elsewhere, nor verify's run of it \
                 (16 GiB, at most half of physical memory)"
            );
        };
        let peak = crate::size(found.peak);
        let from = match &found.via {
            Via::Own => format!("the most its last {} build(s) used", found.runs),
            Via::Wider(k) => format!(
                "the most a build of a wider selection ({}) used",
                k.selection()
            ),
            Via::Elsewhere(k) => format!("the most a build of it in {} used", k.checkout),
            Via::Analogue(k) => format!(
                "the most verify's {} of {} in {} used",
                k.stage,
                k.selection(),
                k.checkout
            ),
        };
        write!(
            f,
            "{bytes}, {from} ({peak}) and a fifth more, at least 1 GiB"
        )
    }
}

#[cfg(test)]
#[path = "book_tests.rs"]
mod tests;
