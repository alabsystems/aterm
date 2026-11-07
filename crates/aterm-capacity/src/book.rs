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
//! for the same work; only with none of those, a wider selection's in its checkout; then the most
//! of a wider named selection's in another checkout, its own or its analogue's, refused where a
//! narrower record or a unit's peak shows that run warm ([`Records::wider_elsewhere`], as verify
//! reads it), claimed at most at the cold figure ([`need`]); and with nothing at all, the cold
//! figure.

use std::fmt;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use aterm_json::{Map, Value};

/// The peaks one record keeps, newest last.
pub const KEPT: usize = 8;

/// The records a file keeps: past this, the one whose newest peak is oldest goes, so change
/// cones that never come back do not grow it without end.
pub const ROWS_KEPT: usize = 512;

/// The encoded book's read and publication bound. A row-count limit alone does not bound
/// the bytes of its public key strings.
const BOOK_MAX_BYTES: u64 = 16 << 20;

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

/// The stage verify's test compile is kept under (`aterm_verify`'s `StageId::TestCompile`): a cold
/// run of it over a named selection compiles every unit of the selection's crates, each of which
/// its [`UNIT_STAGE`] records weigh.
pub const TEST_COMPILE: &str = "TestCompile";

/// The stage verify's test compile keeps each heavy unit's peak under (`aterm_verify::shape`'s
/// `unit_key`): a key whose crates name the unit's package alone and whose variant names its kind
/// (`aterm_gui lib`), in the checkout it compiled in.
pub const UNIT_STAGE: &str = "TestCompile unit";

/// Verify's test binaries' run history (`aterm_verify::test_history`), beside its stages' memory
/// records ([`VERIFY_RECORD`]).
pub const TEST_HISTORY: &str = "verify/test-binaries.json";

/// The lock beside the record at `path` (`stage-memory.json.lock`), which a writer holds from its
/// read of the record to its write of it ([`Records::update`]).
#[must_use]
pub fn lock_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}

/// The text of the regular file at `path`, at most `max` bytes: `None` where it is absent. Any
/// other refusal (a symlink, a directory, a FIFO, text past `max`, bytes that are not UTF-8, an
/// I/O error) is an error, never an empty record: a writer must not replace evidence it could
/// not read ([`update_text`]).
///
/// # Errors
/// As above.
pub fn read_text(path: &Path, max: u64) -> io::Result<Option<String>> {
    match crate::read_regular_text(path, max) {
        Ok(text) => Ok(Some(text)),
        Err(why) if why.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(why) => Err(why),
    }
}

/// THE BOOK'S ATOMIC WRITE: `bytes` written to `path` staged in a private folder beside it (named
/// `prefix`…, removed after), in a file made new (`0600`, never through a symlink), then renamed
/// over `path`, so a reader finds the old file or the new one whole and a failed write leaves
/// the old one standing. The directory is made private (`0700`) if absent and must be a real
/// directory. `bytes` past `max`, the bound its readers read ([`read_text`]), are refused before
/// anything is written: a record no reader could read back is no record.
///
/// # Errors
/// `bytes` exceed `max` (`FileTooLarge`), the directory could not be made or is not one, or the
/// write or the rename failed: the file kept before stands.
pub fn write_staged(path: &Path, bytes: &[u8], prefix: &str, max: u64) -> io::Result<()> {
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max {
        return Err(io::Error::new(
            io::ErrorKind::FileTooLarge,
            "the encoded record exceeds its read bound; the one kept before stands",
        ));
    }
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    crate::ledger::ensure_dir(dir)?;
    let stage = aterm_tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(dir)?;
    let staged = stage.path().join("data");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    #[cfg(unix)]
    let mut file = aterm_uds::kept_out(|| options.open(&staged))?;
    #[cfg(not(unix))]
    let mut file = options.open(&staged)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if file.metadata()?.permissions().mode() & 0o7777 != 0o600 {
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
    }
    file.write_all(bytes)?;
    drop(file);
    std::fs::rename(&staged, path)
}

/// THE BOOK'S LOCKED UPDATE: read the text at `path` ([`read_text`], `None` where absent), let
/// `change` answer the text to write, and write it ([`write_staged`]) when it answers one,
/// holding the record's lock ([`lock_path`]) from the read to the write, so two writers at once,
/// in this process or another, each read what the other wrote and neither loses a change. A
/// refused read never invokes `change` and never replaces the file. Whether it wrote.
///
/// # Errors
/// The directory or the lock could not be made or taken, the existing file could not be read
/// ([`read_text`]), or as [`write_staged`].
pub fn update_text(
    path: &Path,
    max: u64,
    prefix: &str,
    change: impl FnOnce(Option<&str>) -> Option<String>,
) -> io::Result<bool> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    crate::ledger::ensure_dir(dir)?;
    let lock = crate::ledger::open_rendezvous(&lock_path(path))?;
    let _held = aterm_toolchain::record_lock::RecordLock::lock(lock, 0)?;
    let text = read_text(path, max)?;
    let Some(next) = change(text.as_deref()) else {
        return Ok(false);
    };
    write_staged(path, next.as_bytes(), prefix, max).map(|()| true)
}

/// The package name a `-p` spec names: what follows a path spec's `#`, up to its `@<version>`.
/// `None` for a spec whose `@` has no version after it (`name@`), which names no package.
#[must_use]
pub fn spec_name(spec: &str) -> Option<&str> {
    let tail = spec.rsplit_once('#').map_or(spec, |(_, t)| t);
    match tail.split_once('@') {
        None => Some(tail),
        Some((name, version)) => (!version.is_empty()).then_some(name),
    }
}

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
    /// What else of the run changes the work: the cells asked for (`cells linux-arm`), a plain
    /// build's profile and targets (`release`).
    pub variant: Option<String>,
}

impl Key {
    /// Whether this key's selection holds `other`'s, for the same work, checkout and variant:
    /// the workspace holds every selection, and a list holds one whose every crate it names.
    #[must_use]
    pub fn holds(&self, other: &Key) -> bool {
        self.checkout == other.checkout && self.holds_work(other)
    }

    /// Whether this key is a wider NAMED selection than `other`'s in another checkout: the same
    /// work and variant, a crate list that holds `other`'s as [`Self::holds`] reads it, and not
    /// the same selection, which [`Self::elsewhere`] reads. Never the workspace (`crates: None`):
    /// another checkout's may be another repository's, whose build compiled none of this
    /// selection's units, and a virtual manifest's bare build covers only its default members.
    #[must_use]
    pub fn wider_elsewhere(&self, other: &Key) -> bool {
        self.checkout != other.checkout
            && self.crates.is_some()
            && self.crates != other.crates
            && self.holds_work(other)
    }

    /// Whether this key's selection holds `other`'s, for the same work and variant, in any
    /// checkout.
    fn holds_work(&self, other: &Key) -> bool {
        self.stage == other.stage
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
            Some(c) => {
                let cap = c.iter().map(|n| n.len() + 4).sum::<usize>();
                let mut out = String::with_capacity(cap);
                for (i, n) in c.iter().enumerate() {
                    if i > 0 {
                        out.push(' ');
                    }
                    out.push_str("-p ");
                    out.push_str(n);
                }
                out
            }
        }
    }

    /// The key as one line of JSON, as the file holds it ([`Self::from_json`] reads it back): what
    /// a build's claim holder is handed on its command line (BUILD-ADMIT).
    #[must_use]
    pub fn to_json(&self) -> String {
        aterm_json::to_string(&self.encode()).unwrap_or_else(|_| "{}".to_owned())
    }

    /// The key [`Self::to_json`] wrote; `None` for any other text.
    #[must_use]
    pub fn from_json(text: &str) -> Option<Key> {
        Key::decode(&aterm_json::from_str::<Value>(text).ok()?)
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

/// What a wider named selection kept in another checkout ([`Records::wider_elsewhere`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wider {
    /// Its record's key: the wider selection, in the checkout it ran in.
    pub key: Key,
    /// The most its record keeps, bytes.
    pub peak: u64,
    /// How many peaks its record keeps.
    pub runs: usize,
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

    /// WHAT A WIDER SELECTION OF `key`'S WORK TOOK IN ANOTHER CHECKOUT, where it bounds anything:
    /// the most any record of the same work and variant there, over a named selection that holds
    /// `key`'s and is not it, keeps ([`Key::wider_elsewhere`]), with that record's key; an upper
    /// bound on a premise, the one [`Self::learned`] reads a wider selection in its checkout on: a
    /// superset's build compiles the subset's crates too. The most, not the least, so one heavy
    /// superset kept anywhere holds the claim to its peak (the cold figure, for one with
    /// `aterm-gui`) beside a lighter one: a light superset is as likely a warm run as a tighter
    /// bound. Its cache was not this checkout's, and a warm run compiled little and keeps a light
    /// peak, which bounds nothing. So it is REFUSED (`None`, as where none is kept) where it is
    /// lighter than what a record of a selection inside `key`'s, of its work and variant, in any
    /// checkout, kept, or than `floor`, the least a cold run of `key`'s work holds where the caller
    /// knows it ([`Self::unit_floor`]; 0 where it does not): the wider run compiled all of that
    /// work, so it was warm, and every wider record elsewhere with it, since this is the most of
    /// them. It is refused, not raised: a raised figure would bound only the narrower record's
    /// crates. LIMIT: a warm wider run that neither outweighs answers, and nothing here can tell;
    /// a one-crate selection has no narrower named selection inside it, so where `floor` is 0 its
    /// most wider record elsewhere answers however warm that run was. `floor` is 0 for every stage
    /// but a test compile of no variant, for a test compile none of whose crates kept a unit (verify
    /// keeps a unit's peak only at 1 GiB or more), and for every plain build but a `targo test` of
    /// no variant whose analogue is verify's test compile.
    #[must_use]
    pub fn wider_elsewhere(&self, key: &Key, floor: u64) -> Option<Wider> {
        let (peak, row) = self
            .rows
            .iter()
            .filter(|r| r.key.wider_elsewhere(key))
            .filter_map(|r| r.most().map(|m| (m, r)))
            .max_by_key(|(m, _)| *m)?;
        (peak >= floor.max(self.narrower(key))).then(|| Wider {
            key: row.key.clone(),
            peak,
            runs: row.peaks.len(),
        })
    }

    /// The most any record of `key`'s work and variant, in any checkout, over a named selection
    /// inside `key`'s and not it, kept; 0 where none is, or `key` names no selection.
    fn narrower(&self, key: &Key) -> u64 {
        key.crates.as_deref().map_or(0, |crates| {
            self.rows
                .iter()
                .filter(|r| r.key.stage == key.stage && r.key.variant == key.variant)
                .filter(|r| {
                    r.key.crates.as_deref().is_some_and(|theirs| {
                        !theirs.is_empty()
                            && theirs.len() < crates.len()
                            && theirs.iter().all(|c| crates.contains(c))
                    })
                })
                .filter_map(Row::most)
                .max()
                .unwrap_or(0)
        })
    }

    /// THE LEAST A COLD TEST COMPILE OF `key` HOLDS: the most any [`UNIT_STAGE`] record, in any
    /// checkout and of any kind, kept for a crate `key`'s selection names (a `-p` spec read by its
    /// package name, [`spec_name`]), since a cold run of the selection compiles that unit, and a
    /// stage's peak is never less than what one of its compilers held. 0 for any key but verify's
    /// test compile ([`TEST_COMPILE`]) of a named selection with no variant (a shaped call's
    /// variant compiles part of its work), and where no unit of its crates is kept. LIMIT: a unit
    /// kept by a call that compiled more than the selection's tests is read the same way (a
    /// release's workspace test compile folds in the measuring benches, and keeps their units
    /// beside the rest); one heavier than every unit the tests compile refuses a wider record that
    /// held, and the claim falls to the cover or the cold figure, never below what it was.
    #[must_use]
    pub fn unit_floor(&self, key: &Key) -> u64 {
        let Some(crates) = key.crates.as_deref() else {
            return 0;
        };
        if key.stage != TEST_COMPILE || key.variant.is_some() {
            return 0;
        }
        self.rows
            .iter()
            .filter(|r| {
                r.key.stage == UNIT_STAGE
                    && matches!(r.key.crates.as_deref(), Some([package])
                        if crates.iter().any(|c| spec_name(c).unwrap_or(c) == package.as_str()))
            })
            .filter_map(Row::most)
            .max()
            .unwrap_or(0)
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

    /// The records kept in `path`; none where it is absent or unreadable, including a file
    /// past 16 MiB. This soft lookup may fall back to cold estimates; [`Self::update`] instead
    /// preserves a read-refused existing file and reports the refusal.
    #[must_use]
    pub fn load(path: &Path) -> Records {
        read_text(path, BOOK_MAX_BYTES)
            .ok()
            .flatten()
            .map(|text| Self::decode(&text))
            .unwrap_or_default()
    }

    /// Write the records to `path` ([`write_staged`]), so a reader finds the old file or the new
    /// one whole. Two writers at once can lose one of their peaks, unless each reads and writes
    /// under the record's lock ([`Self::update`]); neither leaves a torn file.
    ///
    /// # Errors
    /// As [`write_staged`]: the records kept before stand.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        write_staged(
            path,
            self.encode().as_bytes(),
            ".stage-memory-",
            BOOK_MAX_BYTES,
        )
    }

    /// Read the records at `path`, let `change` add to them, and write them back whole
    /// ([`Self::save`]) when it says to, under the record's lock from the read to the write
    /// ([`update_text`]): two writers at once, in this process or another (a verify run's
    /// stages, another checkout's run, a plain build), each read what the other wrote, and
    /// neither loses a peak or a row. Whether it wrote. An absent book starts empty; a refused
    /// existing read never invokes `change` or replaces that file.
    ///
    /// # Errors
    /// As [`update_text`].
    pub fn update(path: &Path, change: impl FnOnce(&mut Records) -> bool) -> io::Result<bool> {
        update_text(path, BOOK_MAX_BYTES, ".stage-memory-", |text| {
            let mut records = text.map(Records::decode).unwrap_or_default();
            change(&mut records).then(|| records.encode())
        })
    }

    /// Keep `bytes`, measured at `at_ms`, under `key` in the file at `path`: read, pushed
    /// ([`Self::push`]) and written back whole, under the record's lock ([`Self::update`]).
    ///
    /// # Errors
    /// As [`Self::update`].
    pub fn keep(path: &Path, key: Key, bytes: u64, at_ms: u64) -> io::Result<()> {
        Records::update(path, |records| {
            records.push(key, bytes, at_ms);
            true
        })
        .map(|_| ())
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
    /// As [`Self::update`].
    pub fn keep_unless_light(path: &Path, key: Key, bytes: u64, at_ms: u64) -> io::Result<bool> {
        Records::update(path, |records| {
            if records.light(&key, bytes, at_ms) {
                return false;
            }
            records.push(key, bytes, at_ms);
            true
        })
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
    /// A wider selection's record in another checkout, its claim at most the cold figure.
    WiderElsewhere(Key),
    /// Verify's record of the stage whose work holds its own over a wider selection in another
    /// checkout, its claim at most the cold figure.
    AnalogueWiderElsewhere(Key),
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
/// Only where none of that is kept either, the more of two wider named selections' records in
/// another checkout, each read by the one rule verify reads its own by
/// ([`Records::wider_elsewhere`]), so each is refused where a narrower record of its book shows
/// that wider run warm: where `analogue` names verify's records, the analogue's key there, with
/// the floor of its heaviest unit ([`Records::unit_floor`]: verify's test compile's, 0 for its
/// lint); and `key` in `own`, where `key` names no variant and the analogue is verify's test
/// compile, with that same floor or the most verify kept for a selection inside the analogue's,
/// whichever is more (a wider `targo test` of no variant built every test target of its crates,
/// so all that verify's narrower run did), else none.
/// Its claim is at most the cold figure ([`need`]), so it never claims more than a build with
/// nothing kept would. `None` where nothing is kept: the build claims the cold figure. A tie is
/// read as the build's own.
#[must_use]
pub fn chain(own: &Records, key: &Key, analogue: Option<(&Records, &Key)>) -> Option<Found> {
    let found = |peak: u64, runs: usize, via: Via| Found { peak, runs, via };
    let like = analogue.and_then(|(records, like)| {
        records
            .learned(like)
            .or_else(|| records.elsewhere(like))
            .map(|l| {
                let from = l.wider.unwrap_or_else(|| like.clone());
                found(l.peak, l.runs, Via::Analogue(from))
            })
    });
    let far = own.elsewhere(key).map(|l| {
        let from = l.wider.unwrap_or_else(|| key.clone());
        found(l.peak, l.runs, Via::Elsewhere(from))
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
    if let Some(wide) = own.most_of(|k| k != key && k.holds(key)) {
        let from = wide.wider.unwrap_or_else(|| key.clone());
        return Some(found(wide.peak, wide.runs, Via::Wider(from)));
    }
    let floor = analogue.map_or(0, |(records, like)| records.unit_floor(like));
    let like_wide = analogue.and_then(|(records, like)| {
        records
            .wider_elsewhere(like, floor)
            .map(|w| found(w.peak, w.runs, Via::AnalogueWiderElsewhere(w.key)))
    });
    let own_floor = match analogue {
        Some((records, like)) if key.variant.is_none() && like.stage == TEST_COMPILE => {
            floor.max(records.narrower(like))
        }
        _ => 0,
    };
    let own_wide = own
        .wider_elsewhere(key, own_floor)
        .map(|w| found(w.peak, w.runs, Via::WiderElsewhere(w.key)));
    [like_wide, own_wide]
        .into_iter()
        .flatten()
        .max_by_key(|f| f.peak)
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
/// more ([`crate::peak_claim`]), never more than the cold figure where a wider selection's record
/// in another checkout is what it found ([`Via::WiderElsewhere`], [`Via::AnalogueWiderElsewhere`]:
/// that rule stands in for the cold figure, so it only ever lowers it); with none found, the cold
/// figure ([`crate::cold_claim`]).
#[must_use]
pub fn need(
    own: &Records,
    key: &Key,
    analogue: Option<(&Records, &Key)>,
    physical: Option<u64>,
) -> BuildNeed {
    match chain(own, key, analogue) {
        Some(found) => BuildNeed {
            bytes: match found.via {
                Via::WiderElsewhere(_) | Via::AnalogueWiderElsewhere(_) => {
                    crate::peak_claim(found.peak).min(crate::cold_claim(physical))
                }
                Via::Own | Via::Wider(_) | Via::Elsewhere(_) | Via::Analogue(_) => {
                    crate::peak_claim(found.peak)
                }
            },
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
        f.write_str(&crate::size(self.bytes))?;
        let Some(found) = &self.found else {
            return f.write_str(
                " cold: no build of it is kept here or elsewhere, nor verify's run of it \
                 (16 GiB, at most half of physical memory)",
            );
        };
        match &found.via {
            Via::Own => write!(f, ", the most its last {} build(s) used", found.runs)?,
            Via::Wider(k) => write!(
                f,
                ", the most a build of a wider selection ({}) used",
                k.selection()
            )?,
            Via::Elsewhere(k) => write!(f, ", the most a build of it in {} used", k.checkout)?,
            Via::WiderElsewhere(k) => write!(
                f,
                ", the most a build of a wider selection ({}) in {} used",
                k.selection(),
                k.checkout
            )?,
            Via::Analogue(k) => write!(
                f,
                ", the most verify's {} of {} in {} used",
                k.stage,
                k.selection(),
                k.checkout
            )?,
            Via::AnalogueWiderElsewhere(k) => write!(
                f,
                ", the most verify's {} of a wider selection ({}) in {} used",
                k.stage,
                k.selection(),
                k.checkout
            )?,
        }
        let bound = if crate::peak_claim(found.peak) > self.bytes {
            "at most the cold figure (16 GiB, at most half of physical memory)"
        } else {
            "at least 1 GiB"
        };
        write!(
            f,
            " ({}) and a fifth more, {bound}",
            crate::size(found.peak)
        )
    }
}

#[cfg(test)]
#[path = "book_tests.rs"]
mod tests;
