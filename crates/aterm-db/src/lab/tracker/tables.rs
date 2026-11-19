// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `lab/tracker`'s cells tables (docs/DESIGN-lab-tracker-2026-11-14.md §3): what the lab plans
//! and works from. Each is owner data, one causal register per column, so two agents editing
//! different columns of one package never collide, and each keeps the lifecycle every cells
//! collection has (`row`: a row is retired, never deleted) and a `note`.
//!
//! A row named by a word (a package's `TK-10`, a lane's `tables`, a goal's `A11`) is keyed by
//! that word where the key grammar takes it and by its mapped form where it does not
//! ([`super::mapped::key`]), and keeps the word in `id` either way: 37 package ids of
//! publication 384b3a9f hold a dot and 9 decision ids pass 26 bytes (§7, finding 7). A row with
//! no word of its own (a ruling, a stop, a change) is keyed by a digest of what it says
//! ([`super::mapped::birth`]).
//!
//! Text here is the tracker's prose ([`Ty::Prose`]), never the registry's free text, which
//! refuses an `@`, a commit and a long path (§7, finding 13). A computed value is no column: a
//! group's number, a goal's state, a request's progress and a done package's release are
//! relations over these rows and their history (§2, TK-34).

use crate::alab::registry::MACHINE_LINK;
use crate::typed::{
    Collection, Column, Derive, Kind, Member, OnConflict, Ty, Writers,
    values::{MAX_COMMAND, MAX_NOTE},
};

/// Longest word a row is named by (its `id`).
pub const MAX_ID: usize = 128;
/// Longest line: a title, a name, a summary.
pub const LINE: usize = 512;
/// Longest body: an outcome, a decision and its reason, a quote.
pub const BODY: usize = 16 * 1024;

/// The lifecycle words: a row is live until someone retires it, and stays in history after.
pub const LIFE: &[&str] = &["live", "retired"];

/// A lifecycle conflict's provisional word: a retire concurrent with a `live` write reads as
/// retired on every machine.
const LIFE_FINAL_FIRST: &[&str] = &["retired", "live"];

const fn row() -> Column {
    Column::new("row", Ty::Enum(LIFE)).on_conflict(OnConflict::Prefer(LIFE_FINAL_FIRST))
}
/// A row's note: the tracker's prose.
const fn note() -> Column {
    Column::new("note", Ty::Prose(MAX_NOTE)).null()
}
/// The word a row is named by, as written (`TI-W0.1`): the row's key derives from it
/// ([`super::mapped::key`]), so it is written at the row's birth and never edited.
const fn id() -> Column {
    Column::new("id", Ty::Prose(MAX_ID)).fixed()
}

/// A group's horizons.
pub const HORIZONS: &[&str] = &["short", "medium", "long"];

crate::att_collection!(
    /// A group of goals (§3 `group`), keyed by its letter: its horizon, its name and what its
    /// scoreboard number counts. The number itself is computed.
    pub GROUP = Collection {
        name: "group",
        version: 1,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "",
        derive: Derive::Word,
        columns: &[
            id(),
            Column::new("horizon", Ty::Enum(HORIZONS)),
            Column::new("name", Ty::Prose(LINE)),
            Column::new("summary", Ty::Prose(LINE)).null(),
            Column::new("scoreboard", Ty::Prose(LINE)).null(),
            row(),
            note(),
        ],
        unique: &[&["id"]],
        max_row: 0,
        authority: None,
    }
);

/// Who set a goal's target.
pub const TARGET_BY: &[&str] = &["owner", "proposed"];

crate::att_collection!(
    /// A goal (§3 `goal`), keyed by its id (`A11`): its group, what it is, the metric it is
    /// measured by, and its baseline and target as numbers in that metric's unit, or as the
    /// prose the lab tracker held where it stated no number. Its state is computed from its
    /// measurements (TK-16).
    pub GOAL = Collection {
        name: "goal",
        version: 1,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "",
        derive: Derive::Word,
        columns: &[
            id(),
            Column::new("group", Ty::Ref("group")),
            Column::new("title", Ty::Prose(LINE)),
            Column::new("detail", Ty::Prose(BODY)).null(),
            Column::new("metric", Ty::Ref("metric")).null(),
            Column::new("baseline", Ty::Decimal).null(),
            Column::new("baseline_text", Ty::Prose(BODY)).null(),
            Column::new("target", Ty::Decimal).null(),
            Column::new("target_text", Ty::Prose(BODY)).null(),
            Column::new("target_by", Ty::Enum(TARGET_BY)).null(),
            row(),
            note(),
        ],
        unique: &[&["id"]],
        max_row: 0,
        authority: None,
    }
);

/// What a metric counts.
pub const QUANTITIES: &[&str] = &["time", "count", "ratio", "bytes", "instructions"];
/// Which way a metric gets better.
pub const DIRECTIONS: &[&str] = &["up", "down"];
/// Which of a metric's measurements may be compared.
pub const COMPARABLE_ON: &[&str] = &["any", "same-machine", "same-class"];

crate::att_collection!(
    /// A metric (§3 `metric`), keyed by its id (`A10.lab_objects`, so by its mapped form): the
    /// goal it measures, what it is called, its quantity, unit and scale, which way is better,
    /// which measurements compare, and the command that measures it. Its samples are the
    /// `measure` log's.
    pub METRIC = Collection {
        name: "metric",
        version: 1,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "",
        derive: Derive::Composite,
        columns: &[
            id(),
            Column::new("goal", Ty::Ref("goal")),
            Column::new("title", Ty::Prose(LINE)).null(),
            Column::new("quantity", Ty::Enum(QUANTITIES)).null(),
            Column::new("unit", Ty::Prose(LINE)).null(),
            Column::new("scale", Ty::Decimal).null(),
            Column::new("denominator", Ty::Prose(LINE)).null(),
            Column::new("direction", Ty::Enum(DIRECTIONS)).null(),
            Column::new("comparable_on", Ty::Enum(COMPARABLE_ON)).null(),
            Column::new("source", Ty::Command(MAX_COMMAND)).null(),
            row(),
            note(),
        ],
        unique: &[&["id"]],
        max_row: 0,
        authority: None,
    }
);

crate::att_collection!(
    /// A lane (§3 `lane`), keyed by its id (`tables`): its name, the prefix its packages' ids
    /// start with, the repositories and goals it serves, its bottleneck, its rank among the
    /// lanes and who coordinates it. One live row holds a prefix: a second is refused where its
    /// writer sees the first, and reported where two machines wrote at once (`unique`).
    pub LANE = Collection {
        name: "lane",
        version: 1,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "",
        derive: Derive::Word,
        columns: &[
            id(),
            Column::new("name", Ty::Prose(LINE)),
            Column::new("prefix", Ty::Prose(8)),
            Column::new("repos", Ty::Set(&Ty::Ref("repo"))),
            Column::new("goals", Ty::Set(&Ty::Ref("goal"))),
            Column::new("bottleneck", Ty::Prose(BODY)).null(),
            Column::new("rank", Ty::Decimal).null(),
            Column::new("coordinator", Ty::Principal).null(),
            row(),
            note(),
        ],
        unique: &[&["id"], &["prefix"]],
        max_row: 0,
        authority: None,
    }
);

crate::att_collection!(
    /// A repository (§3 `repo`), keyed by its name (`aterm`): its remote and where its checkout
    /// sits under a home folder.
    pub REPO = Collection {
        name: "repo",
        version: 1,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "",
        derive: Derive::Word,
        columns: &[
            id(),
            Column::new("remote", Ty::Prose(LINE)).null(),
            Column::new("path", Ty::Path).null(),
            row(),
            note(),
        ],
        unique: &[&["id"]],
        max_row: 0,
        authority: None,
    }
);

/// What an exit test needs of the machine that runs it.
pub const EXIT_NEEDS: &[&str] = &[
    "stage-1",
    "stage-2",
    "macos",
    "linux",
    "windows",
    "metal",
    "release-build",
    "quiet-machine",
];
/// What passing an exit test means.
pub const EXIT_PASSES: &[&str] = &["exit-0", "pattern", "count", "measure"];

/// A package's exit test (§3 `exit`), a typed value and no shell line: the repositories it
/// needs (each pinned at its remote main), the one it runs in, the command, what the machine
/// must have, the machine class, a timeout, and what passing means (`passes`, with the pattern,
/// count or metric that word asks for). `expect` is what the lab tracker said passing means, in
/// its own words, for an exit no one has typed yet (`aterm goals lint`, TK-19).
pub const EXIT: &[Member] = &[
    Member {
        name: "repos",
        ty: Ty::List(&Ty::Ref("repo"), 16),
        null: true,
    },
    Member {
        name: "in",
        ty: Ty::Ref("repo"),
        null: true,
    },
    Member {
        name: "command",
        ty: Ty::Command(MAX_COMMAND),
        null: false,
    },
    Member {
        name: "needs",
        ty: Ty::List(&Ty::Enum(EXIT_NEEDS), 8),
        null: true,
    },
    Member {
        name: "class",
        ty: Ty::Handle(64),
        null: true,
    },
    Member {
        name: "timeout_s",
        ty: Ty::Uint,
        null: true,
    },
    Member {
        name: "passes",
        ty: Ty::Enum(EXIT_PASSES),
        null: true,
    },
    Member {
        name: "pattern",
        ty: Ty::Prose(LINE),
        null: true,
    },
    Member {
        name: "count",
        ty: Ty::Uint,
        null: true,
    },
    Member {
        name: "metric",
        ty: Ty::Ref("metric"),
        null: true,
    },
    Member {
        name: "expect",
        ty: Ty::Prose(MAX_COMMAND),
        null: true,
    },
];

/// Who holds a package (§4.2): the principal, the day and the millisecond it claimed, and the
/// digest of the exit test it claimed (§4.3). How a lease is renewed and lapses is TK-14's.
pub const CLAIM: &[Member] = &[
    Member {
        name: "by",
        ty: Ty::Principal,
        null: false,
    },
    Member {
        name: "on",
        ty: Ty::Day,
        null: true,
    },
    Member {
        name: "at",
        ty: Ty::Time,
        null: true,
    },
    Member {
        name: "exit",
        ty: Ty::Digest,
        null: true,
    },
];

/// One thing a package is blocked on: a package, a decision, a release or a ruling, each a
/// reference of its own, so the table's rules hold it as they hold `depends` (it names a row
/// the table holds, and a row a live package is blocked on is not retired).
pub const BLOCKER: &[Member] = &[
    Member {
        name: "package",
        ty: Ty::Ref("package"),
        null: true,
    },
    Member {
        name: "decision",
        ty: Ty::Ref("decision"),
        null: true,
    },
    Member {
        name: "release",
        ty: Ty::Ref("release"),
        null: true,
    },
    Member {
        name: "ruling",
        ty: Ty::Ref("ruling"),
        null: true,
    },
];

/// The tables a note can be about a row of: the tracker's cells tables.
pub const NOTED: &[&str] = &[
    "group",
    "goal",
    "metric",
    "lane",
    "repo",
    "package",
    "change",
    "decision",
    "ruling",
    "stop",
    "competition",
    "project",
    "plan",
    "request",
    "release",
    "view",
];

/// A package's sizes.
pub const SIZES: &[&str] = &["S", "M", "L", "XL"];
/// A package's states (§3).
pub const PACKAGE_STATES: &[&str] = &["ready", "running", "blocked", "done", "dropped"];
/// How concurrent package states settle (§4.1): done before dropped before blocked before
/// running before ready. The state rule settles by it over the changes that count
/// ([`super::state`]); a conflicted cell read alone shows its first word by the same order.
pub const PACKAGE_STATE_FIRST: &[&str] = &["done", "dropped", "blocked", "running", "ready"];

crate::att_collection!(
    /// A package (§3 `package`), keyed by its id (`TK-10`; `TI-W0.1` by its mapped form): its
    /// lane and goal, its rank in the lane, what it is and what done means, the repositories
    /// and paths it owns, what it depends on and is blocked on, the request it came from and
    /// the release it is planned for, its exit test, size and state, who holds it, what it was
    /// split from and its design. `state_said` holds the state word the lab tracker gave it
    /// where that is none of [`PACKAGE_STATES`] (`waiting`): the row then states the word it
    /// maps to ([`super::mapped::package_state`]) and is a finding until someone clears it.
    pub PACKAGE = Collection {
        name: "package",
        version: 1,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "",
        derive: Derive::Composite,
        columns: &[
            id(),
            Column::new("lane", Ty::Ref("lane")),
            Column::new("goal", Ty::Ref("goal")).null(),
            Column::new("rank", Ty::Decimal).null(),
            Column::new("title", Ty::Prose(LINE)),
            Column::new("outcome", Ty::Prose(BODY)).null(),
            Column::new("repos", Ty::Set(&Ty::Ref("repo"))),
            Column::new("owns", Ty::Set(&Ty::Path)),
            Column::new("depends", Ty::Set(&Ty::Ref("package"))),
            Column::new("blocked_on", Ty::List(&Ty::Struct(BLOCKER), 16)).null(),
            Column::new("request", Ty::Ref("request")).null(),
            Column::new("release", Ty::Ref("release")).null(),
            Column::new("exit", Ty::Struct(EXIT)).null(),
            Column::new("size", Ty::Enum(SIZES)).null(),
            Column::new("state", Ty::Enum(PACKAGE_STATES))
                .on_conflict(OnConflict::Prefer(PACKAGE_STATE_FIRST)),
            Column::new("state_said", Ty::Prose(64)).null(),
            Column::new("claim", Ty::Struct(CLAIM)).null(),
            Column::new("split_from", Ty::Ref("package")).null(),
            Column::new("design", Ty::Path).null(),
            row(),
            note(),
        ],
        unique: &[&["id"]],
        max_row: 0,
        authority: None,
    }
);

/// The package columns frozen once a package leaves ready (§4.3).
pub const FROZEN: &[&str] = &["exit", "outcome"];

crate::att_collection!(
    /// A change to a frozen column of a package (§3 `change`, §4.3), keyed by a digest of the
    /// package, the column and the value asked for: the digest of the value it replaces, the
    /// value, the reason and who approved it. The value asked for is held in the column of the
    /// package's own name and type (`exit` or `outcome`, whichever `column` says), so an exit
    /// asked for is checked as an exit is: free JSON would hold it to the free-text guard, which
    /// refuses a test's long path. What an approved change lets through is TK-15's.
    pub CHANGE = Collection {
        name: "change",
        version: 1,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "ch",
        derive: Derive::BirthSha256,
        columns: &[
            Column::new("package", Ty::Ref("package")).fixed(),
            Column::new("column", Ty::Enum(FROZEN)).fixed(),
            Column::new("before", Ty::Digest).null(),
            Column::new("exit", Ty::Struct(EXIT)).null().fixed(),
            Column::new("outcome", Ty::Prose(BODY)).null().fixed(),
            Column::new("reason", Ty::Prose(BODY)),
            Column::new("approved_by", Ty::Principal).null(),
            row(),
            note(),
        ],
        unique: &[],
        max_row: 0,
        authority: None,
    }
);

/// A decision's states (§3).
pub const DECISION_STATES: &[&str] = &["open", "decided", "standing", "superseded"];
/// A conflicted decision state's provisional word: the most final first.
const DECISION_STATE_FIRST: &[&str] = &["superseded", "standing", "decided", "open"];

crate::att_collection!(
    /// A decision (§3 `decision`), keyed by its id (`TK-D14`; one over 26 bytes by its mapped
    /// form): its lane, what was decided and why, who decided and on what day, its state, and
    /// the decision it supersedes. `decided_by` is a principal; one the lab tracker kept as free
    /// text (`AI`, `owner`, an agent's name) holds that text as what it said, which is a
    /// finding ([`super::checks`]). `state_said` is [`PACKAGE`]'s, for a decision (`active`).
    pub DECISION = Collection {
        name: "decision",
        version: 1,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "",
        derive: Derive::Composite,
        columns: &[
            id(),
            Column::new("lane", Ty::Ref("lane")).null(),
            Column::new("decision", Ty::Prose(BODY)),
            Column::new("reason", Ty::Prose(BODY)).null(),
            Column::new("decided_by", Ty::Principal).null(),
            Column::new("decided_on", Ty::Day).null(),
            Column::new("state", Ty::Enum(DECISION_STATES))
                .on_conflict(OnConflict::Prefer(DECISION_STATE_FIRST)),
            Column::new("state_said", Ty::Prose(64)).null(),
            Column::new("supersedes", Ty::Ref("decision")).null(),
            row(),
            note(),
        ],
        unique: &[&["id"]],
        max_row: 0,
        authority: None,
    }
);

/// How a ruling got here (§4.5): `attested`, an utterance aterm recorded from a person's own
/// keystrokes (TK-25), or `carried`, imported.
pub const GRADES: &[&str] = &["attested", "carried"];

crate::att_collection!(
    /// One of the owner's rulings, quoted (§3 `ruling`), keyed by a digest of its day and its
    /// words: what it is about, the quote, the repositories it rules on (none: the whole lab),
    /// where it was recorded and its grade.
    pub RULING = Collection {
        name: "ruling",
        version: 1,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "ru",
        derive: Derive::BirthSha256,
        columns: &[
            Column::new("date", Ty::Day).fixed(),
            Column::new("about", Ty::Prose(LINE)),
            Column::new("quote", Ty::Prose(BODY)).fixed(),
            Column::new("repos", Ty::Set(&Ty::Ref("repo"))),
            Column::new("source", Ty::Prose(BODY)).null(),
            Column::new("grade", Ty::Enum(GRADES)),
            row(),
            note(),
        ],
        unique: &[],
        max_row: 0,
        authority: None,
    }
);

crate::att_collection!(
    /// Something a lane will not do (§3 `stop`, as `stops.tsv` has it), keyed by a digest of
    /// its lane and its words.
    pub STOP = Collection {
        name: "stop",
        version: 1,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "sp",
        derive: Derive::BirthSha256,
        columns: &[
            Column::new("lane", Ty::Ref("lane")).fixed(),
            Column::new("item", Ty::Prose(BODY)).fixed(),
            row(),
            note(),
        ],
        unique: &[],
        max_row: 0,
        authority: None,
    }
);

/// How a competition is scored against others.
pub const COMPETITION_KINDS: &[&str] = &["ranked", "unranked"];
/// A competition's states, as the lab tracker states them.
pub const COMPETITION_STATES: &[&str] = &["no-baseline", "measured", "blocked", "done"];

crate::att_collection!(
    /// A competition the lab enters (§3 `competition`, as `competitions.tsv` has it), keyed by
    /// its id (`C7`), which is also its goal's.
    pub COMPETITION = Collection {
        name: "competition",
        version: 1,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "",
        derive: Derive::Word,
        columns: &[
            id(),
            Column::new("name", Ty::Prose(LINE)),
            Column::new("kind", Ty::Enum(COMPETITION_KINDS)),
            Column::new("edition", Ty::Prose(64)).null(),
            Column::new("tracks", Ty::Prose(LINE)).null(),
            Column::new("catalog", Ty::Uint).null(),
            Column::new("scoring", Ty::Prose(LINE)).null(),
            Column::new("limits", Ty::Prose(LINE)).null(),
            Column::new("engine", Ty::Prose(LINE)).null(),
            Column::new("next", Ty::Prose(LINE)).null(),
            Column::new("today", Ty::Prose(LINE)).null(),
            Column::new("state", Ty::Enum(COMPETITION_STATES)),
            Column::new("url", Ty::Url).null(),
            row(),
            note(),
        ],
        unique: &[&["id"]],
        max_row: 0,
        authority: None,
    }
);

crate::att_collection!(
    /// A project the lab builds (§3 `project`, as `projects.tsv` has it), keyed by its name
    /// (`agent harnesses` by its mapped form): its size, what is real, the gap and its score,
    /// each in words.
    pub PROJECT = Collection {
        name: "project",
        version: 1,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "",
        derive: Derive::Composite,
        columns: &[
            id(),
            Column::new("size", Ty::Prose(LINE)).null(),
            Column::new("real", Ty::Prose(BODY)).null(),
            Column::new("gap", Ty::Prose(BODY)).null(),
            Column::new("score", Ty::Prose(LINE)).null(),
            row(),
            note(),
        ],
        unique: &[&["id"]],
        max_row: 0,
        authority: None,
    }
);

crate::att_collection!(
    /// One line of the lab's plan (§3 `plan`, as `plan.tsv` has it), keyed by its key
    /// (`north_star`).
    pub PLAN = Collection {
        name: "plan",
        version: 1,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "",
        derive: Derive::Word,
        columns: &[
            id(),
            Column::new("value", Ty::Prose(BODY)),
            row(),
            note(),
        ],
        unique: &[&["id"]],
        max_row: 0,
        authority: None,
    }
);

/// What a request asks for.
pub const REQUEST_KINDS: &[&str] = &["feature", "bug", "chore", "question", "ask"];
/// When a request is wanted.
pub const PRIORITIES: &[&str] = &["now", "next", "later", "someday"];
/// A request's states (§3). Whether it is in progress, done or shipped is computed from its
/// packages and their release.
pub const REQUEST_STATES: &[&str] = &["new", "triaged", "planned", "declined", "duplicate"];
/// A conflicted request state's provisional word: the most final first.
const REQUEST_STATE_FIRST: &[&str] = &["duplicate", "declined", "planned", "triaged", "new"];

crate::att_collection!(
    /// A request (§3 `request`: the backlog and feature queue), keyed by its id (`RQ-41`):
    /// its kind, what it asks, who asked and where it came from, then what triage gave it (a
    /// lane, a goal, a priority, a rank), its state, the request it duplicates, the packages it
    /// became and the release it is planned for.
    pub REQUEST = Collection {
        name: "request",
        version: 1,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "",
        derive: Derive::Composite,
        columns: &[
            id(),
            Column::new("kind", Ty::Enum(REQUEST_KINDS)),
            Column::new("title", Ty::Prose(LINE)),
            Column::new("body", Ty::Prose(BODY)).null(),
            Column::new("requested_by", Ty::Principal).null(),
            Column::new("source", Ty::Prose(LINE)).null(),
            Column::new("lane", Ty::Ref("lane")).null(),
            Column::new("goal", Ty::Ref("goal")).null(),
            Column::new("priority", Ty::Enum(PRIORITIES)).null(),
            Column::new("rank", Ty::Decimal).null(),
            Column::new("state", Ty::Enum(REQUEST_STATES))
                .on_conflict(OnConflict::Prefer(REQUEST_STATE_FIRST)),
            Column::new("duplicate_of", Ty::Ref("request")).null(),
            Column::new("packages", Ty::Set(&Ty::Ref("package"))),
            Column::new("release", Ty::Ref("release")).null(),
            Column::new("triaged_by", Ty::Principal).null(),
            row(),
            note(),
        ],
        unique: &[&["id"]],
        max_row: 0,
        authority: None,
    }
);

/// A release's states.
pub const RELEASE_STATES: &[&str] = &["planned", "cutting", "shipped"];
/// A conflicted release state's provisional word: the most final first.
const RELEASE_STATE_FIRST: &[&str] = &["shipped", "cutting", "planned"];

crate::att_collection!(
    /// A release of a repository (§3 `release`), keyed by its id (`aterm/0.204`, so by its
    /// mapped form): the repository and version, its state, the day it is planned for, the day
    /// it shipped and its tag's commit. One live row holds a repository's version. A done
    /// package's release is computed, never written here (TK-29).
    pub RELEASE = Collection {
        name: "release",
        version: 1,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "",
        derive: Derive::Composite,
        columns: &[
            id(),
            Column::new("repo", Ty::Ref("repo")),
            Column::new("version", Ty::Prose(64)),
            Column::new("state", Ty::Enum(RELEASE_STATES))
                .on_conflict(OnConflict::Prefer(RELEASE_STATE_FIRST)),
            Column::new("target", Ty::Day).null(),
            Column::new("shipped_on", Ty::Day).null(),
            Column::new("tag", Ty::Commit).null(),
            row(),
            note(),
        ],
        unique: &[&["id"], &["repo", "version"]],
        max_row: 0,
        authority: None,
    }
);

crate::att_collection!(
    /// A shared saved filter of one of the product's relations (§3 `view`), keyed by its name:
    /// the relation it is of, its filter and its sort. The relation itself is code with one
    /// schema (TK-34), so `of` is that relation's name, which nothing here checks.
    pub VIEW = Collection {
        name: "view",
        version: 1,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "",
        derive: Derive::Composite,
        columns: &[
            id(),
            Column::new("of", Ty::Handle(64)),
            Column::new("where", Ty::Prose(BODY)).null(),
            Column::new("sort", Ty::Prose(LINE)).null(),
            row(),
            note(),
        ],
        unique: &[&["id"]],
        max_row: 0,
        authority: None,
    }
);

/// The machine a measurement was taken on (§3 `measure.machine`, §4.6): a link to its registry
/// machine row, by its Commons machine uuid.
pub(super) const MEASURED_ON: Ty = Ty::Link(MACHINE_LINK);
