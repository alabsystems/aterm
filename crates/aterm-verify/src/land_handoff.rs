// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The landing's handoff to another build of itself ([`super`]). Before a judgment or a recheck
//! whose tree's own verifier is not the running image, the landing writes this file in the
//! checkout's git directory and replaces itself in place with that verifier, as
//! `land --resume <file>` ([`hand_over`]). The receiver resumes the same step: the argv a
//! judgment's child gets, its parse, the reading of its record and every rule after it are
//! then one build's, the build of the tree it judges.
//!
//! The file is the only thing that crosses builds. [`SCHEMA`] numbers its encoding, and a
//! receiver that does not read a schema refuses it by name: the remedy is then to run
//! `targo --unverified verify land` again, from the merge the checkout holds, committed. A
//! change to the encoding, or to what a step it names means, bumps [`SCHEMA`]. Schema 2
//! carries no count of re-judges, since no such count ends a landing, and its recheck step is
//! a round: the receiver judges the merge beyond the source readers, by its own reading of
//! the commit and the receipt.
//!
//! This build also reads the schema before it ([`EARLIER`]), whose handoff differs only in
//! carrying that count, which is dropped: every branch cut before this build reached main
//! hands its landing over in it, at the first merge that brings this build in. Its judgment
//! step means what this build's does. Its recheck step meant the source readers alone, and
//! this build runs it as a round, which re-derives what it judges and never judges less. The
//! reverse is refused by the earlier build itself, which reads its own schema alone, so a
//! round is never run as a recheck of the source readers.

use super::Branch;
use crate::exit;
use aterm_json::{Map, Value};
use aterm_land::Step;
use aterm_types::land_trailer::{self, Stated};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The handoff encoding's number, which this build writes.
pub const SCHEMA: u64 = 2;
/// The encoding before it, which this build also reads: [`SCHEMA`]'s fields and [`REJUDGES`].
const EARLIER: u64 = 1;
/// The field only an [`EARLIER`] handoff holds: the count of re-judges its sender had spent.
const REJUDGES: &str = "rejudges";
/// The `land` flag that resumes a landing from its handoff: the landing's own word to the
/// image it hands itself to, which a person never types.
pub const RESUME: &str = "--resume";
/// The handoff's file name in the checkout's git directory.
pub const FILE: &str = "aterm-land-handoff.json";
/// The most a handoff may hold.
const MAX_BYTES: u64 = 1 << 20;
/// Every field of a schema-2 handoff.
const FIELDS: [&str; 12] = [
    "schema", "root", "image", "words", "step", "hop", "at", "base", "composes", "cone", "branch",
    "stated",
];

/// A verifier executable and its sha256 digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// Where it is.
    pub path: PathBuf,
    /// Its bytes' sha256, lowercase hex.
    pub digest: String,
}

/// A landing stopped before a step ([`aterm_land::Checkpoint`]), as another build of itself
/// resumes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Handoff {
    /// The checkout it lands.
    pub root: PathBuf,
    /// The digest of the image it is handed to.
    pub image: String,
    /// The person's `land` words, which the receiver parses again.
    pub words: Vec<String>,
    /// The step it stopped before.
    pub step: Step,
    /// Which handoff of that step this is, from 1: the first comes from the image that ran the
    /// steps before, and each later one from an image built from the step's own tree
    /// ([`super::HOPS`] bounds them).
    pub hop: usize,
    /// The committed checkout the step runs on (HEAD).
    pub at: String,
    /// The main commit the step's cone starts at.
    pub base: String,
    /// The compositions spent since the last whole judgment (0 before a judgment).
    pub composes: usize,
    /// The main commit the landing's first judgment merged: the cone the branch's crates are
    /// read over (the step's base before any judgment).
    pub cone: String,
    /// The branch's crates, read before the first judgment that ran (`None` before it, and on
    /// a landing resumed from a kept judgment that has judged nothing since).
    pub branch: Option<Branch>,
    /// The receipt so far.
    pub stated: Stated,
}

impl Handoff {
    /// One JSON object, its keys in order, then a newline.
    ///
    /// # Errors
    /// The checkout's path is not UTF-8, or a count does not fit.
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        self.encoded(SCHEMA, None)
    }

    /// [`Self::encode`]'s bytes as a sender of `schema` wrote them: with `rejudges`, the count
    /// an [`EARLIER`] handoff carried.
    fn encoded(&self, schema: u64, rejudges: Option<u64>) -> Result<Vec<u8>, String> {
        let root = self
            .root
            .to_str()
            .ok_or_else(|| format!("the checkout {} is not UTF-8", self.root.display()))?;
        let count = |n: usize| u64::try_from(n).map_err(|_| format!("the count {n} does not fit"));
        let branch = match &self.branch {
            None => Value::Null,
            Some(Branch::Crates(crates)) => Value::Object(Map::from([(
                "crates".to_owned(),
                Value::from(crates.iter().cloned().collect::<Vec<_>>()),
            )])),
            Some(Branch::Everything(why)) => Value::Object(Map::from([(
                "everything".to_owned(),
                Value::from(why.as_str()),
            )])),
        };
        let mut fields = Map::from([
            ("schema".to_owned(), Value::from(schema)),
            ("root".to_owned(), Value::from(root)),
            ("image".to_owned(), Value::from(self.image.as_str())),
            ("words".to_owned(), Value::from(self.words.clone())),
            ("step".to_owned(), Value::from(step_word(self.step))),
            ("hop".to_owned(), Value::from(count(self.hop)?)),
            ("at".to_owned(), Value::from(self.at.as_str())),
            ("base".to_owned(), Value::from(self.base.as_str())),
            ("composes".to_owned(), Value::from(count(self.composes)?)),
            ("cone".to_owned(), Value::from(self.cone.as_str())),
            ("branch".to_owned(), branch),
            ("stated".to_owned(), Value::from(self.stated.trailers())),
        ]);
        if let Some(rejudges) = rejudges {
            fields.insert(REJUDGES.to_owned(), Value::from(rejudges));
        }
        let value = Value::Object(fields);
        Ok(format!("{value}\n").into_bytes())
    }

    /// The handoff `bytes` hold: exactly [`Self::encode`]'s bytes of a schema-2 handoff whose
    /// step and receipt agree, or those of the schema before it ([`EARLIER`]), whose count of
    /// re-judges is read and dropped.
    ///
    /// # Errors
    /// Why not, naming the field: not JSON or not an object, a schema this build does not
    /// read, a missing or unknown field, a root that is not absolute, an image that is no
    /// sha256 digest, words that are not strings, an unknown step, a hop that is no count from
    /// 1, a composition or re-judge
    /// count that is no count, an `at`, `base` or `cone` that is no object id, a composition
    /// count at a judgment, a receipt that does not read, branch crates before any judgment, a
    /// recheck whose receipt holds no passed judgment (the one it extends), a cone other than
    /// the base before any judgment, or bytes that are not the handoff's own encoding.
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        let value: Value =
            aterm_json::from_slice(bytes).map_err(|error| format!("it is not JSON: {error}"))?;
        let Value::Object(map) = &value else {
            return Err("it is not a JSON object".into());
        };
        let schema = match map.get("schema") {
            None => return Err("it names no schema".into()),
            Some(schema) => match schema.as_u64() {
                Some(schema @ (SCHEMA | EARLIER)) => schema,
                _ => {
                    return Err(format!(
                        "it was handed over in schema {schema}; this tree's verify land reads \
                         schemas {EARLIER} and {SCHEMA}"
                    ));
                }
            },
        };
        let held: BTreeSet<&str> = map.keys().map(String::as_str).collect();
        let mut want: BTreeSet<&str> = FIELDS.into_iter().collect();
        if schema == EARLIER {
            want.insert(REJUDGES);
        }
        if let Some(missing) = want.difference(&held).next() {
            return Err(format!("it lacks its `{missing}` field"));
        }
        if let Some(unknown) = held.difference(&want).next() {
            return Err(format!("it holds an unknown field `{unknown}`"));
        }
        let text = |key: &str| {
            map[key]
                .as_str()
                .ok_or_else(|| format!("its `{key}` is not a string"))
        };
        let commit = |key: &str| {
            let id = text(key)?;
            if land_trailer::object_id(id) {
                Ok(id.to_owned())
            } else {
                Err(format!("its `{key}` `{id}` is not a git object id"))
            }
        };
        let root = PathBuf::from(text("root")?);
        if !root.is_absolute() {
            return Err(format!("its root {} is not absolute", root.display()));
        }
        let image = text("image")?;
        if !aterm_codec::hex::is_lower(image, 64) {
            return Err(format!("its image `{image}` is not a sha256 digest"));
        }
        let words = map["words"]
            .as_array()
            .and_then(|words| {
                words
                    .iter()
                    .map(|word| word.as_str().map(str::to_owned))
                    .collect::<Option<Vec<_>>>()
            })
            .ok_or("its `words` are not a list of strings")?;
        let step = match text("step")? {
            "judge" => Step::Qualify,
            "recheck" => Step::Recheck,
            other => return Err(format!("its step `{other}` is neither judge nor recheck")),
        };
        let hop = map["hop"]
            .as_u64()
            .and_then(|hop| usize::try_from(hop).ok())
            .filter(|hop| *hop >= 1)
            .ok_or("its `hop` is not a count from 1")?;
        let count = |key: &str| {
            map[key]
                .as_u64()
                .and_then(|count| usize::try_from(count).ok())
                .ok_or_else(|| format!("its `{key}` is not a count"))
        };
        let rejudges = match schema {
            EARLIER => Some(
                map[REJUDGES]
                    .as_u64()
                    .ok_or_else(|| format!("its `{REJUDGES}` is not a count"))?,
            ),
            _ => None,
        };
        let composes = count("composes")?;
        if step == Step::Qualify && composes != 0 {
            return Err(format!(
                "a judgment's handoff carries a composition count ({composes})"
            ));
        }
        let stated = Stated::of_block(text("stated")?).ok_or("its receipt does not read")?;
        let branch = match &map["branch"] {
            Value::Null => None,
            Value::Object(branch) if branch.len() == 1 => {
                match (branch.get("crates"), branch.get("everything")) {
                    (Some(Value::Array(crates)), None) => Some(Branch::Crates(
                        crates
                            .iter()
                            .map(|name| name.as_str().map(str::to_owned))
                            .collect::<Option<BTreeSet<_>>>()
                            .ok_or("its branch crates are not names")?,
                    )),
                    (None, Some(Value::String(why))) => Some(Branch::Everything(why.clone())),
                    _ => return Err("its branch is neither crates nor everything".into()),
                }
            }
            _ => return Err("its branch is neither null, crates nor everything".into()),
        };
        let judged = stated.checked.len() + stated.interrupted.len();
        if branch.is_some() && judged == 0 {
            return Err("it carries branch crates, but its receipt holds no judgment".into());
        }
        if step == Step::Recheck && stated.checked.is_empty() {
            return Err("a recheck's receipt holds no passed judgment".into());
        }
        let (at, base, cone) = (commit("at")?, commit("base")?, commit("cone")?);
        if judged == 0 && cone != base {
            return Err(format!(
                "its cone {cone} is not its base {base}, before any judgment"
            ));
        }
        let handoff = Self {
            root,
            image: image.to_owned(),
            words,
            step,
            hop,
            at,
            base,
            composes,
            cone,
            branch,
            stated,
        };
        if handoff.encoded(schema, rejudges)? != bytes {
            return Err("it is not in the handoff's own encoding".into());
        }
        Ok(handoff)
    }

    /// Read the handoff at `file` and remove the file, whether or not it decodes: a handoff is
    /// read once. `land --resume` calls it only on the checkout's own handoff file, which it
    /// checks first, so no other file is read or removed.
    ///
    /// # Errors
    /// No file is there, it cannot be read or removed, or [`Self::decode`] refuses it.
    pub fn take(file: &Path) -> Result<Self, String> {
        let read = aterm_uds::atomic_file::read_regular_text(file, MAX_BYTES);
        if let Err(error) = &read
            && error.kind() == std::io::ErrorKind::NotFound
        {
            return Err("it is not there".into());
        }
        let removed = std::fs::remove_file(file);
        let text = read.map_err(|error| format!("it could not be read: {error}"))?;
        removed.map_err(|error| format!("it could not be removed: {error}"))?;
        Self::decode(text.as_bytes())
    }
}

/// A step's word in the handoff.
fn step_word(step: Step) -> &'static str {
    match step {
        Step::Qualify => "judge",
        Step::Recheck => "recheck",
    }
}

/// Write `handoff` at `file`, readable by its owner alone.
///
/// # Errors
/// It does not encode, or the file could not be written.
pub fn write(file: &Path, handoff: &Handoff) -> Result<(), String> {
    let bytes = handoff.encode()?;
    aterm_uds::atomic_file::write_flushed(file, &bytes, 0o600, None).map_err(|error| {
        format!(
            "the handoff {} could not be written: {error}",
            file.display()
        )
    })
}

/// Replace this process in place with `image` as `land --resume <file>`: the same pid,
/// environment, process group and session, in the checkout's `root` as its working directory,
/// so the receiver finds the checkout whatever became of the directory the landing was started
/// in (a merge may have removed it). Both output streams are flushed first, so every line said
/// before reaches the person, and this process's macOS remote host copy is released, since the
/// new image captures its own. Returns only when the exec failed, having said why:
/// [`exit::COULD_NOT_RUN`].
#[must_use]
pub fn hand_over(image: &Image, file: &Path, root: &Path) -> i32 {
    use std::io::Write as _;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    if let Err(why) = crate::remote_host::release_before_exec() {
        println!(
            "verify land: COULD NOT RUN — the landing could not release its remote host before \
             handing itself to {}: {why}; nothing was pushed",
            image.path.display()
        );
        return exit::COULD_NOT_RUN;
    }
    // The exec keeps this process's working directory: it becomes the checkout's root here,
    // between the steps, where nothing else of the landing runs.
    if let Err(error) = std::env::set_current_dir(root) {
        println!(
            "verify land: COULD NOT RUN — the landing could not enter its checkout {} before \
             handing itself to {}: {error}; the checkout holds the merge and nothing was pushed. \
             Fix: run `targo --unverified verify land` again",
            root.display(),
            image.path.display()
        );
        return exit::COULD_NOT_RUN;
    }
    #[cfg(unix)]
    let error = aterm_uds::spawnfd::transparent(&image.path)
        .arg("land")
        .arg(RESUME)
        .arg(file)
        .exec();
    #[cfg(not(unix))]
    let error = std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "an in-place exec is unavailable on this host",
    );
    println!(
        "verify land: COULD NOT RUN — the landing could not hand itself to {}: {error}; the \
         checkout holds the merge and nothing was pushed. Fix: run `targo --unverified verify \
         land` again",
        image.path.display()
    );
    exit::COULD_NOT_RUN
}

#[cfg(test)]
#[path = "land_handoff_tests.rs"]
mod tests;
