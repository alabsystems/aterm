// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The state word every answer about a shared file carries
//! (docs/DESIGN-shared-files-2026-11-11.md §3, "State words"), and the status file a pass
//! records it in. None of the words is an error.

use aterm_json::{Map, Value};

/// How current a file's snapshot is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Word {
    /// The last pass opened custody on the newest home.
    Live,
    /// Custody refused, or the records come from a fenced home.
    Held,
    /// Nothing materialized here yet: a read answers empty.
    Never,
    /// Commons disabled, or the table excluded.
    Off,
}

impl Word {
    /// The word as every surface spells it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Held => "held",
            Self::Never => "never",
            Self::Off => "off",
        }
    }

    /// The word `text` spells.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        [Self::Live, Self::Held, Self::Never, Self::Off]
            .into_iter()
            .find(|word| word.as_str() == text)
    }
}

/// Why the last pass did not read live custody.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// A writer succession is unfinished, or its completion was invalidated.
    Succession,
    /// This Mac is not a member the vault lets read Commons.
    Membership,
    /// Another local holder kept the vault, custody or the table past the pass's wait.
    Busy,
    /// `[commons]` turns sharing off or excludes the table, or its file cannot be read.
    Policy,
    /// The identity or pin the vault's source is read with is missing or untrusted.
    Keys,
    /// This Mac's catalog does not declare the table, or declares one it cannot use.
    Catalog,
    /// Custody or the table's storage refused for any other reason.
    Custody,
}

impl Why {
    /// The word as every surface spells it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Succession => "succession",
            Self::Membership => "membership",
            Self::Busy => "busy",
            Self::Policy => "policy",
            Self::Keys => "keys",
            Self::Catalog => "catalog",
            Self::Custody => "custody",
        }
    }

    /// The word `text` spells.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        [
            Self::Succession,
            Self::Membership,
            Self::Busy,
            Self::Policy,
            Self::Keys,
            Self::Catalog,
            Self::Custody,
        ]
        .into_iter()
        .find(|why| why.as_str() == text)
    }
}

/// Why a pass that opened custody left the snapshot as it was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kept {
    /// The replica it read is behind the snapshot: an earlier home of this Mac's writer
    /// lineage, or the same home without a record the snapshot was projected from. A fenced
    /// home's read is behind too where the snapshot's frontier is not here to compare it with.
    Behind,
    /// Its projection is larger than a snapshot a read takes, or its frontier larger than one
    /// the next pass reads back ([`crate::snapshot::MAX_BYTES`] each).
    TooLarge,
}

impl Kept {
    /// The word as every surface spells it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Behind => "behind",
            Self::TooLarge => "too-large",
        }
    }

    /// The word `text` spells.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        [Self::Behind, Self::TooLarge]
            .into_iter()
            .find(|kept| kept.as_str() == text)
    }
}

/// The format word of a status file.
pub const STATE_FORMAT: &str = "aterm-file-state/1";

/// What the last recorded pass left: its word, and what the snapshot beside it was projected
/// from. Written only by a pass, under the file's lock.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State {
    /// `live`, `held` or `off`; `None` before any pass recorded.
    pub word: Option<Word>,
    pub why: Option<Why>,
    /// Why the last pass, which opened custody, did not take its projection; `None` where it
    /// took it, projected nothing new, or was refused.
    pub kept: Option<Kept>,
    /// How many passes have recorded. A pass that opened custody at one count records nothing
    /// once the count has moved (section 4, Materializer).
    pub seq: u64,
    /// When the last pass recorded, Unix milliseconds: this Mac's clock, for display only.
    pub pass_ms: u64,
    /// The snapshot's revision; 0 with none.
    pub rev: u64,
    pub rows: u64,
    /// Which home of this Mac's writer lineage the snapshot came from (0 the original).
    pub era: u64,
    /// The generation of the table family the snapshot came from.
    pub generation: u32,
    /// The fingerprint of the frontier the snapshot was projected from, lower-case hex.
    pub fingerprint: String,
    /// The records the table held at that frontier.
    pub records: u64,
    /// The projection rules' version the snapshot was made with.
    pub projection: u32,
    /// No retained record could still become visible at that frontier.
    pub complete: bool,
    /// Cells left out because their value was shaped like a credential.
    pub withheld: u64,
    /// The file's kind (`table`) and its face's extension (`tsv`).
    pub kind: String,
    pub ext: String,
    /// SHA-256 of the face bytes the pass last wrote, lower-case hex; `None` when none is
    /// written.
    pub face: Option<String>,
}

impl State {
    /// The status file's bytes.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut object = Map::new();
        let mut put = |name: &str, value: Value| {
            object.insert(name.to_owned(), value);
        };
        put("format", Value::from(STATE_FORMAT));
        put("word", Value::from(self.word.map(Word::as_str)));
        put("why", Value::from(self.why.map(Why::as_str)));
        put("kept", Value::from(self.kept.map(Kept::as_str)));
        put("seq", Value::from(self.seq));
        put("pass_ms", Value::from(self.pass_ms));
        put("rev", Value::from(self.rev));
        put("rows", Value::from(self.rows));
        put("era", Value::from(self.era));
        put("generation", Value::from(u64::from(self.generation)));
        put("fingerprint", Value::from(self.fingerprint.as_str()));
        put("records", Value::from(self.records));
        put("projection", Value::from(u64::from(self.projection)));
        put("complete", Value::from(self.complete));
        put("withheld", Value::from(self.withheld));
        put("kind", Value::from(self.kind.as_str()));
        put("ext", Value::from(self.ext.as_str()));
        put("face", Value::from(self.face.as_deref()));
        let mut bytes = Value::Object(object).to_string().into_bytes();
        bytes.push(b'\n');
        bytes
    }

    /// Read a status file's bytes; `None` when they are not one this build reads.
    #[must_use]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let value: Value = aterm_json::from_slice(bytes).ok()?;
        if value.get("format")?.as_str()? != STATE_FORMAT {
            return None;
        }
        let number = |name: &str| value.get(name).and_then(Value::as_u64).unwrap_or(0);
        let text = |name: &str| {
            value
                .get(name)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let word = match value.get("word").and_then(Value::as_str) {
            Some(word) => Some(Word::parse(word).filter(|word| *word != Word::Never)?),
            None => None,
        };
        Some(Self {
            word,
            why: value
                .get("why")
                .and_then(Value::as_str)
                .and_then(Why::parse),
            kept: value
                .get("kept")
                .and_then(Value::as_str)
                .and_then(Kept::parse),
            seq: number("seq"),
            pass_ms: number("pass_ms"),
            rev: number("rev"),
            rows: number("rows"),
            era: number("era"),
            generation: u32::try_from(number("generation")).ok()?,
            fingerprint: text("fingerprint"),
            records: number("records"),
            projection: u32::try_from(number("projection")).ok()?,
            complete: value.get("complete").and_then(Value::as_bool) == Some(true),
            withheld: number("withheld"),
            kind: text("kind"),
            ext: text("ext"),
            face: value.get("face").and_then(Value::as_str).map(str::to_owned),
        })
    }
}

/// A face against what a pass last wrote there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Face {
    /// The bytes a pass wrote.
    Written,
    /// Other bytes: a person or a tool changed it. Passes leave it alone, and programs keep
    /// reading the snapshot.
    Edited,
    /// No face is there.
    Missing,
}

impl Face {
    /// The word as `status` says it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Written => "written",
            Self::Edited => "edited",
            Self::Missing => "missing",
        }
    }
}

/// What one read of a file says of it: the state word, why, and what the last pass recorded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub word: Word,
    pub why: Option<Why>,
    /// The last recorded pass, when one recorded.
    pub state: Option<State>,
    /// A local file of this Mac's that could not be read (the snapshot, the status, the
    /// outbox): the read still answers, as if the file were not there.
    pub problem: Option<String>,
    /// This Mac's writes waiting for the publisher.
    pub queued: u64,
    /// This Mac's writes held: their register moved, or this build refuses them.
    pub held: u64,
}

impl Status {
    /// The snapshot's revision; 0 with none.
    #[must_use]
    pub fn rev(&self) -> u64 {
        self.state.as_ref().map_or(0, |state| state.rev)
    }

    /// The status as one line: `live · rev 7 · 12 rows`, `held (succession) · rev 7 · 12 rows`,
    /// `never (membership)`, `held (custody) · rev 7 · 12 rows · last read not taken (behind)`,
    /// `held (succession) · rev 7 · 12 rows · 2 queued · 1 held back`.
    #[must_use]
    pub fn line(&self) -> String {
        let mut line = self.word.as_str().to_owned();
        if let Some(why) = self.why {
            line.push_str(&format!(" ({})", why.as_str()));
        }
        if self.word != Word::Never
            && let Some(state) = self.state.as_ref().filter(|state| state.rev > 0)
        {
            line.push_str(&format!(" · rev {} · {} rows", state.rev, state.rows));
            if !state.complete {
                line.push_str(" · history incomplete");
            }
        }
        if let Some(kept) = self.state.as_ref().and_then(|state| state.kept) {
            line.push_str(&format!(" · last read not taken ({})", kept.as_str()));
        }
        if self.queued > 0 {
            line.push_str(&format!(" · {} queued", self.queued));
        }
        if self.held > 0 {
            line.push_str(&format!(" · {} held back", self.held));
        }
        line
    }

    /// The status as JSON, for `--json`.
    #[must_use]
    pub fn json(&self) -> Value {
        let mut object = Map::new();
        object.insert("state".to_owned(), Value::from(self.word.as_str()));
        object.insert("why".to_owned(), Value::from(self.why.map(Why::as_str)));
        if let Some(state) = &self.state {
            for (name, value) in [
                ("rev", state.rev),
                ("rows", state.rows),
                ("pass_ms", state.pass_ms),
                ("passes", state.seq),
                ("era", state.era),
                ("generation", u64::from(state.generation)),
                ("records", state.records),
                ("withheld", state.withheld),
            ] {
                object.insert(name.to_owned(), Value::from(value));
            }
            object.insert("complete".to_owned(), Value::from(state.complete));
            object.insert("kept".to_owned(), Value::from(state.kept.map(Kept::as_str)));
            object.insert(
                "last_pass".to_owned(),
                Value::from(state.word.map(Word::as_str)),
            );
        }
        if let Some(problem) = &self.problem {
            object.insert("problem".to_owned(), Value::from(problem.as_str()));
        }
        object.insert("queued".to_owned(), Value::from(self.queued));
        object.insert("held_back".to_owned(), Value::from(self.held));
        Value::Object(object)
    }
}
