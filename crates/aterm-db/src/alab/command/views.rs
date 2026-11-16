// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm db emails|computers|phones|ai-accounts`: the owner's tables under the names the owner
//! learned from fleet_db (docs/DESIGN-alab-one-mesh-2026-11-05.md §4.7, §4.8, §11 A7), now
//! aliases of the shared files (docs/DESIGN-shared-files-2026-11-11.md §3). None of them reads
//! fleet_db, whatever this Mac's registry holds or refuses.
//!
//! - **The view.** Every row, each column as the grid shows it, with the state word of the
//!   answer (`live`, `held`, `never`, `off`) and why: addresses and phone numbers as they are
//!   (ordinary data), a conflict as `⚠ a ‖ b`. A password, mailbox password or TOTP seed never
//!   shows: where its cell names the secret-store entry that holds it (`SecretRef`), the view
//!   shows that entry's name, and in a registry generation builds up to 0.200.0 read, which
//!   holds it as a sealed token, `••••••` when set. Such a generation holds a phone's `number`
//!   sealed too, because those builds read no other form: it is opened for the view
//!   (`att::open_ordinary`), journaled nowhere, and one that does not open on this Mac shows as
//!   `••••••`. `ai-accounts` is the shared file `ai/accounts` and `computers` is
//!   `lab/machines`: each is read from that file's snapshot on this Mac
//!   ([`aterm_file::File::read`]), so it answers the last materialized rows whatever this Mac's
//!   custody is doing, and opens none. `emails` and `phones` have no shared file yet, so their
//!   view is projected from the registry; where that read refuses, the answer is no rows and
//!   `never` with the reason. `emails` adds one `secrets` line of counts only: how many
//!   secret cells name a store entry, how many the table still holds sealed, and how many Macs
//!   reading the vault hold the roll that moves them back.
//! - **`--registry`** (`ai-accounts`, `computers`) is the noun's own projection of the
//!   registry, read through this Mac's Commons custody, so it refuses where custody does. It
//!   carries what the snapshot does not hold yet: for `ai-accounts` the `seen_on` and
//!   `last_seen` columns and the observed `status` (from every Mac's `account-obs` rows and the
//!   journal's `seen` events, judged against this Mac's clock), for `computers` each live seat
//!   no machine row binds. It stays until the observation files `ai/logins` and `ai/usage`
//!   (shared-files slice SF-08) carry those facts.
//! - **`--edit`** opens the collection in the amacs grid (`att::edit_grid`): inside aterm in an
//!   Editor tab beside this one, elsewhere in this terminal. A secret column and a number a
//!   generation holds sealed are read-only there.
//! - **`set <row> <column> [<value> | --stdin]`** writes one cell as the owner, as the
//!   generation this Mac's registry is at holds the column ([`Holds`]). A secret comes only
//!   from standard input, never argv, with one trailing newline from a pipe removed. Where the
//!   cell names a store entry, the value goes into the secret store
//!   (`typed::secret::store_cell`) and only the reference to its entry into the table; an empty
//!   value clears the reference and removes the entry. In a generation that holds a secret as a
//!   sealed token (the form builds up to 0.200.0 read) the value is sealed at its cell's
//!   coordinates and the token written, and an empty value clears the cell. A phone's number is
//!   given either way, held to a number's shape, and sealed where the generation holds it so.

use super::{named_row, verbs};
use crate::{
    Error, Result,
    alab::{
        registry::{self, ACCOUNT, ADDRESS, MACHINE, PHONE},
        sighted::{self, Heard},
        view::LabView,
    },
    att::{self, Handle},
    typed::{
        self, Collection, Ty,
        cell::{self, CellKey, CellValue},
        check, grid,
        secret::{self, Secret, Store},
    },
};
use aterm_file::{Why, Word};
use aterm_json::{Map, Value, json};
use aterm_secrets::client::Paths;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// One of the owner's four tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Noun {
    Emails,
    Computers,
    Phones,
    AiAccounts,
}

impl Noun {
    /// The noun a first word names.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "emails" => Some(Self::Emails),
            "computers" => Some(Self::Computers),
            "phones" => Some(Self::Phones),
            "ai-accounts" | "ai_accounts" => Some(Self::AiAccounts),
            _ => None,
        }
    }

    fn word(self) -> &'static str {
        match self {
            Self::Emails => "emails",
            Self::Computers => "computers",
            Self::Phones => "phones",
            Self::AiAccounts => "ai-accounts",
        }
    }

    /// This table's collection as the registry's newest generation holds it.
    fn collection(self) -> &'static Collection {
        match self {
            Self::Emails => &ADDRESS,
            Self::Computers => &MACHINE,
            Self::Phones => &PHONE,
            Self::AiAccounts => &ACCOUNT,
        }
    }

    /// This table's collection as the generation `handle` reaches holds it: every read and
    /// write of the table goes by it. A generation builds up to 0.200.0 read types a password
    /// and a phone's number as a sealed token, the newest as a store reference and a plain
    /// number, and a cell is read and written in its own generation's form.
    fn collection_at(self, handle: &Handle) -> &'static Collection {
        let newest = self.collection();
        handle
            .compiled()
            .table
            .collection(newest.name)
            .unwrap_or(newest)
    }

    /// The column of this table whose values are ordinary data that a generation holds sealed:
    /// a phone's `number`, shown as it is by every view and written from the command line.
    const fn ordinary(self) -> Option<&'static str> {
        match self {
            Self::Phones => Some("number"),
            Self::Emails | Self::Computers | Self::AiAccounts => None,
        }
    }

    /// The shared file this noun is an alias of, where one is built: `emails` and `phones` have
    /// none yet.
    const fn file(self) -> Option<&'static crate::file::Type> {
        match self {
            Self::AiAccounts => Some(&crate::file::ACCOUNTS),
            Self::Computers => Some(&crate::file::MACHINES),
            Self::Emails | Self::Phones => None,
        }
    }
}

/// What one view invocation asks for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Opts {
    /// The noun's own registry projection rather than its shared file's snapshot.
    registry: bool,
    edit: bool,
    /// `set <row> <column> [<value>]`, and whether the value comes from stdin.
    set: Option<(String, String, Option<String>)>,
    stdin: bool,
}

const USAGE: &str = "aterm db emails|computers|phones|ai-accounts — the owner's tables

  aterm db <table>  every row, each column as the grid shows it: addresses and phone numbers as
                    they are, a conflict as ⚠ a ‖ b; a password never shows: its cell names the
                    `aterm secrets` entry that holds it, or shows •••••• when set where the
                    registry still holds it sealed; with `state` (live, held, never or off) and
                    `why`, which say how current the answer is and are never an error.
                    `ai-accounts` is the shared file ai/accounts and `computers` is lab/machines
                    (`aterm file cat <name>`): read from this Mac's snapshot, whatever its
                    Commons custody is doing. `emails` and `phones` are read from the registry;
                    where that refuses they answer no rows and `never`
  aterm db ai-accounts --registry | aterm db computers --registry
                    the table's own view of the registry, read through this Mac's Commons
                    custody (it refuses where custody does), with what the snapshot does not
                    hold yet. `ai-accounts` adds seen_on, the Macs a tab ran it on in the last
                    two hours, and last_seen, from every Mac's account-obs rows and the
                    journal's seen events; a pending account a Mac runs reads `signed in · on
                    <mac>`, and a paused, broken or banned one `<status> · signed in on <mac>`.
                    `computers` adds each live seat no machine row binds
  aterm db <table> --edit
                    the table in the amacs grid (in an aterm tab, in an Editor tab beside it);
                    a password column is read-only there
  aterm db <table> set <row> <column> [<value> | --stdin]
                    write one cell as the owner; a password only from --stdin (one trailing
                    newline from a pipe is removed), never on the command line; an empty one
                    clears the cell. A phone's number is given either way
";

fn parse(noun: Noun, words: &[String]) -> Result<Opts> {
    let mut opts = Opts::default();
    let mut operands = Vec::new();
    for word in words {
        match word.as_str() {
            "--registry" if noun.file().is_some() => opts.registry = true,
            "--edit" => opts.edit = true,
            "--stdin" => opts.stdin = true,
            flag if flag.starts_with("--") => {
                return Err(Error::new(&format!(
                    "{}: unknown option (see `aterm db {} --help`)",
                    noun.word(),
                    noun.word()
                )));
            }
            operand => operands.push(operand.to_owned()),
        }
    }
    match operands.as_slice() {
        [] => {}
        [set, row, column] if set == "set" => {
            opts.set = Some((row.clone(), column.clone(), None));
        }
        [set, row, column, value] if set == "set" => {
            opts.set = Some((row.clone(), column.clone(), Some(value.clone())));
        }
        _ => {
            return Err(Error::new(&format!(
                "{}: takes `set <row> <column> [<value> | --stdin]` or options only",
                noun.word()
            )));
        }
    }
    if opts.edit && opts.set.is_some() {
        return Err(Error::new("choose one of --edit and set"));
    }
    if opts.registry && (opts.edit || opts.set.is_some()) {
        return Err(Error::new("--registry goes with the plain view only"));
    }
    if opts.stdin && opts.set.is_none() {
        return Err(Error::new("--stdin goes with set"));
    }
    if let Some((_, _, Some(_))) = &opts.set
        && opts.stdin
    {
        return Err(Error::new("give the value or --stdin, not both"));
    }
    Ok(opts)
}

/// What a view reads and opens beyond the registry: the grid opener (a grid file and the 1-based
/// line its caret lands on; it returns once the person closes the file), standard input for
/// `set --stdin`, the secret store a secret column's value goes into, and the clock.
pub struct ViewIo<'a> {
    pub open: &'a mut dyn FnMut(&Path, usize) -> Result<()>,
    pub read_stdin: &'a mut dyn FnMut() -> Result<zeroize::Zeroizing<Vec<u8>>>,
    pub store: &'a dyn Store,
    /// What holds the roll into the secret store back, counted by kind, for the `emails` view's
    /// count line.
    pub holding: &'a dyn Fn() -> Result<aterm_secrets::client::Holding>,
    pub now_ms: u64,
}

/// Run one view: `words` after the noun.
///
/// # Errors
/// The words are not a view's, the registry cannot be read or written, the grid cannot be
/// opened, the cell named cannot take the value, or the secret store refuses it.
pub fn run(paths: &Paths, noun: Noun, words: &[String], io: &mut ViewIo<'_>) -> Result<Value> {
    if words.iter().any(|w| w == "--help" || w == "-h") {
        return Ok(json!({"usage": USAGE}));
    }
    let opts = parse(noun, words)?;
    if let Some(file) = noun.file()
        && opts.set.is_none()
        && !opts.edit
        && !opts.registry
    {
        // An alias of `aterm file cat`: this Mac's snapshot, read before anything here reaches
        // for the registry, so no custody is opened.
        let root = crate::file::root(&crate::generation::original(paths));
        return Ok(from_snapshot(&root, noun, file));
    }
    let handle = Handle::at(paths.clone(), &att::REGISTRY);
    if let Some((row, column, value)) = &opts.set {
        return set(&handle, noun, row, column, value.as_deref(), opts.stdin, io);
    }
    if opts.edit {
        return edit(&handle, noun, io);
    }
    let view = match handle.view() {
        Ok(view) => view,
        // `--registry` asks for the registry itself: where custody refuses, so does it. The
        // noun's plain view still answers, from the snapshot.
        Err(error) if opts.registry => return Err(error),
        Err(error) => return Ok(unread(paths, noun, &error)),
    };
    let collection = noun.collection_at(&handle);
    let projection = view.project(collection);
    let opened = match noun.ordinary() {
        // A refusal here is answered as the view's own read one statement above is: the plain
        // view says `never` and why, and is never an error.
        Some(column) => match att::open_ordinary(&handle, collection, column, &view.cells) {
            Ok(opened) => opened,
            Err(error) => return Ok(unread(paths, noun, &error)),
        },
        None => grid::Opened::new(),
    };
    let mut rows: Vec<Value> = projection
        .rows
        .iter()
        .map(|(pk, row)| shown_row(collection, pk, row, &opened))
        .collect();
    if opts.registry && noun == Noun::Computers {
        let unbound = seats(paths, &view, &mut rows)?;
        rows.extend(unbound);
    }
    if opts.registry && noun == Noun::AiAccounts {
        seen_columns(paths, &view, &mut rows, io.now_ms);
    }
    // The records of a home an unfinished succession fences are held, never live; nor are
    // those of a home whose standing could not be judged.
    let (word, why) = crate::file::materialize::standing(paths, crate::admission::deadline());
    let mut out = Map::new();
    out.insert("complete".to_owned(), json!(view.complete));
    out.insert("table".to_owned(), json!(noun.word()));
    out.insert("state".to_owned(), json!(word.as_str()));
    out.insert("why".to_owned(), json!(why.map(Why::as_str)));
    if noun == Noun::Emails && !opts.registry {
        let cells = secret_cells(handle.compiled().table, &view);
        out.insert(
            "secrets".to_owned(),
            json!(secrets_line(cells, &(io.holding)())),
        );
    }
    out.insert(noun.word().to_owned(), Value::Array(rows));
    Ok(Value::Object(out))
}

/// How many of the registry's secret cells (passwords, mailbox passwords, TOTP seeds) name a
/// store entry and how many still hold a sealed token, counting each cell once (a conflict with
/// any sealed candidate counts as sealed). It reads no value.
fn secret_cells(table: &typed::Table, view: &typed::view::View) -> (usize, usize) {
    let (mut references, mut sealed) = (0, 0);
    for collection in table.collections {
        let secret: Vec<&str> = collection
            .columns
            .iter()
            .filter(|column| {
                typed::secret_named(column.name) && matches!(column.ty, Ty::SecretRef | Ty::Secret)
            })
            .map(|column| column.name)
            .collect();
        if secret.is_empty() {
            continue;
        }
        for row in view.project(collection).rows.values() {
            for field in secret.iter().filter_map(|name| row.fields.get(*name)) {
                let values = field.candidates_x();
                if values
                    .iter()
                    .any(|x| x.as_str().is_some_and(secret::is_sealed))
                {
                    sealed += 1;
                } else if values.iter().any(|x| secret::entry_of(x).is_some()) {
                    references += 1;
                }
            }
        }
    }
    (references, sealed)
}

/// The owner's one line on the registry's secrets: how many cells name a store entry, how many
/// the table still holds sealed, and how many Macs reading the vault hold the roll that moves
/// them back and why, by kind (`aterm_secrets::client::fleet_holding`): an unheard Mac is a
/// different act for the owner from one on an older build. Counts only; where the holding cannot
/// be read it says so.
fn secrets_line(
    (references, sealed): (usize, usize),
    holding: &Result<aterm_secrets::client::Holding>,
) -> String {
    let held = match holding {
        Ok(holding) if holding.clear() => {
            "no Mac holds the move into the secret store back".to_owned()
        }
        Ok(holding) => {
            let macs = holding.macs();
            let head = if macs == 0 {
                "the move into the secret store waits".to_owned()
            } else {
                format!(
                    "{} reading the vault {} the move into the secret store back",
                    plural(macs, "Mac"),
                    if macs == 1 { "holds" } else { "hold" }
                )
            };
            format!("{head}: {}", holding.reasons().join(", "))
        }
        Err(error) => format!("which Macs hold the move back is unknown: {error}"),
    };
    format!(
        "secrets: {} {} a secret-store entry, {} still sealed in the table; {held}",
        plural(references, "cell"),
        if references == 1 { "names" } else { "name" },
        plural(sealed, "cell")
    )
}

/// `n` and `word`, with an `s` unless `n` is 1.
fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

/// What a noun with no shared file yet answers where its registry read refused: no rows, the
/// word `never`, and why. Never an error, and never a read of fleet_db.
fn unread(paths: &Paths, noun: Noun, error: &Error) -> Value {
    let why = if error.is_busy() {
        Why::Busy
    } else if error.is_awaiting_membership() || error.is_enrollment_pending() {
        Why::Membership
    } else if matches!(
        crate::alab::Registry::at(paths.clone()).declared(),
        Ok(false)
    ) {
        Why::Catalog
    } else {
        Why::Custody
    };
    let mut out = Map::new();
    out.insert("table".to_owned(), json!(noun.word()));
    out.insert("state".to_owned(), json!(Word::Never.as_str()));
    out.insert("why".to_owned(), json!(why.as_str()));
    out.insert("detail".to_owned(), json!(error.to_string()));
    out.insert(noun.word().to_owned(), Value::Array(Vec::new()));
    Value::Object(out)
}

/// A noun that is a shared file's alias, from that file's snapshot under `root`: its rows as
/// the last pass left them, with the file's state word. It reads local files only.
fn from_snapshot(root: &aterm_file::Root, noun: Noun, file: &crate::file::Type) -> Value {
    let shared = aterm_file::File::at(root, &file.file());
    let read = shared.read();
    let rows: Vec<Value> = read
        .rows()
        .iter()
        .map(|row| {
            let mut out = Map::new();
            out.insert(noun.collection().prefix.to_owned(), json!(row.pk));
            for column in read
                .columns()
                .iter()
                .filter(|column| column.source != aterm_file::Source::Pk)
            {
                let cell = row.cell(&column.name);
                out.insert(
                    column.name.clone(),
                    if cell.is_empty() {
                        Value::Null
                    } else {
                        json!(cell)
                    },
                );
            }
            Value::Object(out)
        })
        .collect();
    let mut out = Map::new();
    out.insert(
        "complete".to_owned(),
        json!(
            read.status
                .state
                .as_ref()
                .is_some_and(|state| state.complete)
        ),
    );
    out.insert("table".to_owned(), json!(noun.word()));
    out.insert("file".to_owned(), json!(file.name));
    out.insert("state".to_owned(), json!(read.status.word.as_str()));
    out.insert("why".to_owned(), json!(read.status.why.map(Why::as_str)));
    out.insert("rev".to_owned(), json!(read.status.rev()));
    if let Some(problem) = &read.status.problem {
        out.insert("problem".to_owned(), json!(problem));
    }
    out.insert(noun.word().to_owned(), Value::Array(rows));
    Value::Object(out)
}

/// One row as the view shows it: its key and each live column, by name, as the grid shows it
/// ([`grid::show_field_at`]: a secret reference by its entry name where that entry is the
/// cell's own, a sealed secret as the mask), a number a generation holds sealed as its value
/// where `opened` holds it.
fn shown_row(
    collection: &'static Collection,
    pk: &str,
    row: &cell::Row,
    opened: &grid::Opened,
) -> Value {
    let mut out = Map::new();
    out.insert(collection.prefix.to_owned(), json!(pk));
    for column in collection
        .columns
        .iter()
        .filter(|c| c.live_at(collection.version))
    {
        let number = column
            .sealed_ordinary()
            .then(|| opened.get(&(pk.to_owned(), column.name.to_owned())))
            .flatten();
        let shown = match number {
            Some(values) => grid::show_opened(values),
            None => grid::show_field_at(collection.name, column, pk, row.fields.get(column.name)),
        };
        out.insert(
            column.name.to_owned(),
            if shown.is_empty() {
                Value::Null
            } else {
                json!(shown)
            },
        );
    }
    Value::Object(out)
}

/// Where and when one account was observed running (`account-obs` and heard `seen` events,
/// `docs/DESIGN-account-discovery-2026-11-09.md`): each Mac it runs on now by name with its tabs,
/// and the newest sighting anywhere.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SeenOn {
    /// Each Mac a tab runs it on now, by name, with how many tabs.
    macs: BTreeMap<String, u64>,
    /// The newest sighting on any Mac, Unix milliseconds.
    last_ms: u64,
}

/// The `seen` and tabbed `session-end` events of the journal window this Mac holds at `now_ms`;
/// none where it holds no Commons or the window cannot be read (the view still shows the rows).
fn heard(paths: &Paths, now_ms: u64) -> Vec<Heard> {
    match crate::alab::journal::seen::read(paths, None, now_ms / 1_000) {
        Ok(Some(window)) => sighted::heard(&window),
        _ => Vec::new(),
    }
}

/// Every account's [`SeenOn`] at `now_ms` from the `account-obs` rows `obs` and the heard events
/// `heard`, both read then, each Mac named by `name` (from its Commons uuid): the one fold of
/// sightings ([`sighted::fold`], which the fleet tables' accounts relation reads too), with the
/// Macs a tab runs the account on now by name, two uuids of one name counted once at the more
/// tabs. Every row and sighting dates `last_ms`, however old; an account no Mac sighted (one a
/// row only says `signed-out` of) has no entry.
fn seen_on(
    obs: &crate::typed::owned::OwnedRead,
    heard: &[Heard],
    name: &dyn Fn(&str) -> String,
    now_ms: u64,
) -> BTreeMap<String, SeenOn> {
    sighted::fold(&sighted::obs_rows(obs), heard, now_ms, now_ms)
        .into_iter()
        .filter_map(|(account, sighted)| {
            let (last_ms, _) = sighted.last()?;
            let mut macs: BTreeMap<String, u64> = BTreeMap::new();
            for (uuid, on) in sighted.macs.iter().filter(|(_, on)| on.tabs > 0) {
                let mac = macs.entry(name(uuid)).or_default();
                *mac = (*mac).max(on.tabs);
            }
            Some((account, SeenOn { macs, last_ms }))
        })
        .collect()
}

/// An age as the view says it: `12 s`, `4 min`, `3 h`, `2 d`.
fn age(ms: u64) -> String {
    let secs = ms / 1_000;
    match secs {
        0..60 => format!("{secs} s"),
        60..3_600 => format!("{} min", secs / 60),
        3_600..86_400 => format!("{} h", secs / 3_600),
        _ => format!("{} d", secs / 86_400),
    }
}

/// A Mac's name in the view: `m65 (cheetah)` from its machine row, else its label, else the
/// first 8 digits of its Commons uuid.
fn mac_name(lab: &LabView, uuid: &str) -> String {
    let machine = lab.bound.get(uuid).and_then(|pk| lab.machines.get(pk));
    match machine.map(|m| (m.label.as_deref(), &m.nickname)) {
        Some((Some(label), crate::alab::view::MachineNickname::Named { word, .. })) => {
            format!("{label} ({word})")
        }
        Some((Some(label), _)) => label.to_owned(),
        _ => uuid.chars().take(8).collect(),
    }
}

/// `ai-accounts --registry`: the derived `seen_on` and `last_seen` columns, never typed, from
/// every Mac's `account-obs` rows and the journal's heard `seen` events ([`seen_on`]); and where
/// a Mac runs the account now, `status` says so ahead of a catalog `pending`: observation wins
/// over belief.
fn seen_columns(paths: &Paths, view: &att::View, rows: &mut [Value], now_ms: u64) {
    let empty = crate::typed::owned::OwnedRead::default();
    let owned = |name: &str| view.owned.get(name).unwrap_or(&empty);
    let lab = LabView::read_view(
        view,
        owned(registry::MACHINE_FACTS.name),
        owned(registry::MAILBOX_OBS.name),
        None,
    );
    let seen = seen_on(
        owned(registry::ACCOUNT_OBS.name),
        &heard(paths, now_ms),
        &|uuid| mac_name(&lab, uuid),
        now_ms,
    );
    shown_seen(rows, &seen, now_ms);
}

/// [`seen_columns`] over `seen`, by each row's account key.
fn shown_seen(rows: &mut [Value], seen: &BTreeMap<String, SeenOn>, now_ms: u64) {
    for row in rows {
        let Some(fields) = row.as_object_mut() else {
            continue;
        };
        let held = fields
            .get(ACCOUNT.prefix)
            .and_then(Value::as_str)
            .and_then(|pk| seen.get(pk));
        let (on, last) = match held {
            Some(held) => (
                held.macs
                    .iter()
                    .map(|(mac, tabs)| format!("{mac} \u{d7}{tabs}"))
                    .collect::<Vec<_>>()
                    .join(", "),
                age(now_ms.saturating_sub(held.last_ms)),
            ),
            None => (String::new(), String::new()),
        };
        // A Mac running the account now is an observation, and the status says it where the
        // owner's word does not already: a pending (or unset) account reads as signed in there;
        // a held one (paused, broken, banned: always a person's word, which bars sign-ins) keeps
        // that word first, since a Mac running it anyway is what the owner needs to see, not a
        // reason to drop the hold; an active one already says it serves.
        let running = held.and_then(|held| held.macs.keys().next());
        if let Some(mac) = running {
            let status = fields.get("status").and_then(Value::as_str);
            let shown = match status {
                None | Some("pending") => Some(format!("signed in \u{b7} on {mac}")),
                Some(hold @ ("paused" | "broken" | "banned")) => {
                    Some(format!("{hold} \u{b7} signed in on {mac}"))
                }
                Some(_) => None,
            };
            if let Some(shown) = shown {
                fields.insert("status".to_owned(), json!(shown));
            }
        }
        for (column, value) in [("seen_on", on), ("last_seen", last)] {
            fields.insert(
                column.to_owned(),
                if value.is_empty() {
                    Value::Null
                } else {
                    json!(value)
                },
            );
        }
    }
}

/// `computers --registry`: each machine row with the Commons seats bound to it, then each live
/// seat no machine row binds.
fn seats(paths: &Paths, view: &att::View, rows: &mut [Value]) -> Result<Vec<Value>> {
    let empty = crate::typed::owned::OwnedRead::default();
    let owned = |name: &str| view.owned.get(name).unwrap_or(&empty);
    let lab = LabView::read_view(
        view,
        owned(registry::MACHINE_FACTS.name),
        owned(registry::MAILBOX_OBS.name),
        None,
    );
    let mut bound_by_machine: BTreeMap<&str, Vec<&String>> = BTreeMap::new();
    for (uuid, machine) in &lab.bound {
        bound_by_machine
            .entry(machine.as_str())
            .or_default()
            .push(uuid);
    }
    for row in rows.iter_mut() {
        let Some(seats) = row.get(MACHINE.prefix).and_then(Value::as_str).map(|pk| {
            let bound = bound_by_machine.get(pk).map_or(&[][..], Vec::as_slice);
            json!(bound)
        }) else {
            continue;
        };
        if let Value::Object(fields) = row {
            fields.insert("seats".to_owned(), seats);
        }
    }
    let live = crate::alab::Registry::at(paths.clone()).live_uuids()?;
    Ok(live
        .into_iter()
        .filter(|uuid| !lab.bound.contains_key(uuid))
        .map(|uuid| {
            let mut seat = Map::new();
            seat.insert(MACHINE.prefix.to_owned(), Value::Null);
            seat.insert("seats".to_owned(), json!([uuid]));
            Value::Object(seat)
        })
        .collect())
}

/// `--edit`: the collection in the grid as the generation here holds it, each value as it is
/// stored; a secret reference by its entry name, a sealed secret as the mask and a number a
/// generation holds sealed as it is, those three read-only.
fn edit(handle: &Handle, noun: Noun, io: &mut ViewIo<'_>) -> Result<Value> {
    let collection = noun.collection_at(handle);
    let mut problems = Vec::new();
    let mut report = |line: &str| problems.push(line.to_owned());
    let edited = att::edit_grid(handle, collection, &[], &mut *io.open, &mut report)?;
    Ok(json!({
        "table": noun.word(),
        "rounds": edited.rounds,
        "records": edited.applied.iter().map(|a| a.records).sum::<usize>(),
        "cells": edited.applied.iter().map(|a| a.cells).sum::<usize>(),
        "born": edited.applied.iter().map(|a| a.born.len()).sum::<usize>(),
        "retired": edited.applied.iter().map(|a| a.retired.len()).sum::<usize>(),
        "abandoned": edited.abandoned,
        "problems": problems,
    }))
}

/// The most bytes `set --stdin` reads.
const MAX_STDIN: usize = 16 * 1024;

/// Standard input whole, at most [`MAX_STDIN`] bytes, zeroed when dropped.
///
/// # Errors
/// It cannot be read, or holds more.
pub fn read_stdin() -> Result<zeroize::Zeroizing<Vec<u8>>> {
    use std::io::Read as _;
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    std::io::stdin()
        .lock()
        .take(MAX_STDIN as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::new("stdin could not be read"))?;
    if bytes.len() > MAX_STDIN {
        return Err(Error::new("stdin holds more than a cell takes"));
    }
    Ok(bytes)
}

/// One line piped in keeps its text and loses its newline (`\n` or `\r\n`), as `aterm secrets
/// set` reads one; anything else is kept whole.
fn trim_piped_line(bytes: &mut Vec<u8>) {
    let Some(body) = bytes.strip_suffix(b"\n") else {
        return;
    };
    let body = body.strip_suffix(b"\r").unwrap_or(body);
    if std::str::from_utf8(body).is_ok() && !body.iter().any(|b| matches!(b, b'\r' | b'\n' | 0)) {
        let keep = body.len();
        bytes.truncate(keep);
    }
}

/// The row `name` names in `noun`'s collection: its key, or an address (`emails`: the row whose
/// `addr` says it now, [`registry::AddressIndex`], so a renamed row goes by its new address and
/// never by the one it was born with), a machine label (`computers`), a phone's label, or an
/// account's label. A refusal never quotes the address.
fn row_named(view: &att::View, noun: Noun, name: &str) -> Result<String> {
    let collection = noun.collection();
    let projection = view.project(collection);
    if projection.rows.contains_key(name) {
        return Ok(name.to_owned());
    }
    match noun {
        Noun::Emails => {
            let index = registry::AddressIndex::of(&projection);
            match index
                .holder(name)
                .map_err(|_| Error::new("an email row is named by its key or its address"))?
            {
                registry::AddressHolder::Row(pk) => Ok(pk.to_owned()),
                registry::AddressHolder::Several(_) => Err(Error::new(
                    "more than one email row holds that address: name the row by its key",
                )),
                registry::AddressHolder::Free {
                    derived,
                    taken: true,
                } => Err(Error::new(&format!(
                    "no email row holds that address now (row {} was born with it and says \
                     another): name a row by its key",
                    att::command::safe_key(&derived)
                ))),
                registry::AddressHolder::Free { .. } => {
                    Err(Error::new("no email row holds that address"))
                }
            }
        }
        Noun::Computers => {
            let empty = crate::typed::owned::OwnedRead::default();
            let owned = |n: &str| view.owned.get(n).unwrap_or(&empty);
            let lab = LabView::read_view(
                view,
                owned(registry::MACHINE_FACTS.name),
                owned(registry::MAILBOX_OBS.name),
                None,
            );
            verbs::machine_named(&lab, name)
                .ok_or_else(|| Error::new("no one machine goes by that name: name it by its key"))
        }
        Noun::Phones | Noun::AiAccounts => {
            named_row(collection.name, &projection, name, false, |_, row| {
                row.get("label").and_then(Value::as_str) == Some(name)
            })
        }
    }
}

/// How the generation a `set` writes holds the column it names: what the value is read as and
/// where it goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Holds {
    /// A value of the column's type, read in the grid's spelling.
    Plain,
    /// Ordinary data the generation holds sealed ([`typed::Column::sealed_ordinary`]: a phone's
    /// `number` where builds up to 0.200.0 read it): given either way, held to a number's shape
    /// and sealed inside the writing session.
    SealedOrdinary,
    /// A secret the generation holds as a sealed token, the form builds up to 0.200.0 read:
    /// sealed inside the writing session at its cell's coordinates; an empty value clears the
    /// cell.
    SealedSecret,
    /// A secret whose cell names the secret-store entry that holds its value.
    Entry,
}

impl Holds {
    fn of(schema: &typed::Column) -> Self {
        match schema.ty {
            Ty::SecretRef if typed::secret_named(schema.name) => Self::Entry,
            Ty::Secret if schema.sealed_ordinary() => Self::SealedOrdinary,
            Ty::Secret => Self::SealedSecret,
            _ => Self::Plain,
        }
    }

    /// Whether the column's value is a secret: read from standard input only.
    const fn secret(self) -> bool {
        matches!(self, Self::SealedSecret | Self::Entry)
    }
}

/// A phone row's column of its number's last four digits.
const LAST4: &str = "last4";

/// The stored form of the phone number a person typed for `collection.column`
/// ([`cell::typed_phone`]): what is typed is held to a number's shape (7 to 15 digits, an
/// optional leading `+`), and what is stored is the registry's one form of it, the importer's
/// (its digits, a US number's leading `1` dropped), so the importer and `check` find the row
/// by it.
fn stored_number(collection: &Collection, column: &str, typed: &str) -> Result<String> {
    cell::typed_phone(typed).ok_or_else(|| {
        Error::new(&format!(
            "{}.{column}: a number is 7 to 15 digits, with an optional leading +",
            collection.name
        ))
    })
}

/// `set <row> <column> [<value> | --stdin]`: one owner write, as the generation `handle` reaches
/// holds the column ([`Holds`]). A secret's value comes only from stdin.
///
/// Where the cell names a store entry, the value goes into the secret store
/// ([`secret::store_cell`], which refuses one shaped like a flag), never into the table: it takes
/// a free entry of the cell ([`secret::fresh_entry`]: tagged with fresh randomness, so no other
/// run, here or on another Mac, takes it), the cell is written with the reference to it, and a
/// protected row takes none. An empty one clears the cell. The new entry and each one the cell's
/// replaced values named are owed removal first ([`secret::Store::owe`]) by this run. Once the
/// write lands, its entry is paid and the replaced entries are removed, exactly those
/// ([`secret::retire`]); a removal that fails stays owed (`owed` in the output), and a refused
/// write leaves its own put owed. What ended runs left owed is swept ([`secret::sweep`]), never
/// what a live run owes; the retire and the sweep run only while the registry has one generation
/// here ([`att::sole_generation`]). The write's outcome is reported whatever the settle after it
/// meets (`settle_error`).
///
/// Where the generation holds the secret as a sealed token (the form builds up to 0.200.0
/// read), the value is sealed at its cell's coordinates under that generation's key and the
/// token written, whatever the cell holds, so a write never says whether a value was the held
/// one; one shaped like a flag is refused, a protected row takes none, and an empty value
/// clears the cell. Nothing goes to the store. A phone's number is held to a number's shape and stored in
/// the registry's one form ([`stored_number`]) with the row's `last4` in the same record:
/// sealed, as those builds read it, where the generation holds it sealed, and as plain text
/// where it does not; one the cell already holds is not written again, and clearing the number
/// leaves `last4` as it is. Any other value is read in the grid's spelling of its type. Every
/// write passes the registry's rules on the owner's writes ([`registry::guard`]): an `emails …
/// addr` that another row says, whose birth key another row holds, or that renames an account's
/// login is refused, and one that passes writes the row's `domain` in the same record.
fn set(
    handle: &Handle,
    noun: Noun,
    row: &str,
    column: &str,
    value: Option<&str>,
    stdin: bool,
    io: &mut ViewIo<'_>,
) -> Result<Value> {
    let collection = noun.collection_at(handle);
    let schema = collection
        .columns
        .iter()
        .find(|c| c.name == column && c.live_at(collection.version))
        .ok_or_else(|| {
            Error::new(&format!(
                "{}: no column `{column}` (see `aterm db {} --help`)",
                noun.word(),
                noun.word()
            ))
        })?;
    let holds = Holds::of(schema);
    if holds == Holds::Plain && grid::read_only(schema) {
        return Err(Error::new(&format!(
            "{}.{column} is written by tools, never typed",
            collection.name
        )));
    }
    let text: zeroize::Zeroizing<String> = match (value, stdin) {
        (Some(_), _) if holds.secret() => {
            return Err(Error::new(&format!(
                "a {column} never goes on the command line: pipe it to `set … --stdin`"
            )));
        }
        (Some(value), _) => zeroize::Zeroizing::new(value.to_owned()),
        (None, true) => {
            let mut bytes = (io.read_stdin)()?;
            trim_piped_line(&mut bytes);
            zeroize::Zeroizing::new(
                std::str::from_utf8(&bytes)
                    .map_err(|_| Error::new("stdin is not UTF-8"))?
                    .to_owned(),
            )
        }
        (None, false) if holds.secret() => {
            return Err(Error::new(&format!(
                "a {column} comes from standard input: `set … --stdin`"
            )));
        }
        (None, false) => {
            return Err(Error::new("set takes the value, or --stdin"));
        }
    };
    // A secret goes into the store before its reference is written, outside the registry's
    // write: the store has its own pass, and a reference never names an entry not yet put. The
    // entry is owed ([`secret::Store::owe`]) by this run before the put, so a write refused
    // below, or a run cut short, leaves a debt a sweep collects once this run has ended.
    let stored = if holds == Holds::Entry && !text.is_empty() {
        let view = handle.view()?;
        let pk = row_named(&view, noun, row)?;
        if view
            .project(collection)
            .rows
            .get(&pk)
            .is_some_and(check::protected)
        {
            return Err(Error::new(&format!(
                "{}.{column}: this row is protected and holds no {column}",
                collection.name
            )));
        }
        let value = Secret::new(text.as_str().to_owned());
        Some((
            pk.clone(),
            secret::store_cell(io.store, collection.name, column, &pk, &value)?,
        ))
    } else {
        None
    };
    let committed = handle.commit_owner_sealed(|view, sealer| {
        let pk = row_named(view, noun, row)?;
        let held_here = if holds == Holds::Entry {
            held_entries(view, collection, column, &pk)
        } else {
            BTreeSet::new()
        };
        let projection = view.project(collection);
        let held = projection.rows.get(&pk);
        let one_value = held
            .and_then(|r| r.fields.get(column))
            .is_some_and(|f| f.candidates().len() == 1);
        // A set is written member by member: each one the text names that the set lacks is
        // added, and each one it holds that the text leaves out is removed, so another
        // writer's concurrent member stands.
        if matches!(schema.ty, Ty::Set(_)) {
            let after = grid::read_set(&text).ok_or_else(|| {
                Error::new(&format!(
                    "{}.{column}: a set is `a, b` or a JSON list of texts",
                    collection.name
                ))
            })?;
            let before: BTreeSet<String> =
                cell::members(schema, held.and_then(|r| r.fields.get(column)))
                    .into_iter()
                    .map(str::to_owned)
                    .collect();
            let mut changes = Vec::new();
            for (members, present) in [
                (after.difference(&before), true),
                (before.difference(&after), false),
            ] {
                for member in members {
                    changes.push(cell::member_change(
                        collection,
                        &pk,
                        column,
                        member,
                        present,
                        |x| CellValue::owner(collection.version, x),
                    )?);
                }
            }
            let records = if changes.is_empty() {
                Vec::new()
            } else {
                vec![changes]
            };
            return Ok(((pk, BTreeSet::new()), records));
        }
        // A number's last four digits, which its row keeps beside it.
        let mut last4 = None;
        // The value to write; `None` where the cell already holds the number given.
        let x = match (&stored, holds) {
            (Some((stored_pk, reference)), _) => {
                if *stored_pk != pk || held.is_some_and(check::protected) {
                    return Err(Error::new(&format!(
                        "{}.{column}: the row changed while its value was stored; run it again",
                        collection.name
                    )));
                }
                Some(reference.clone())
            }
            (None, Holds::Entry) => Some(Value::Null),
            (None, Holds::SealedSecret) if text.is_empty() => Some(Value::Null),
            (None, Holds::SealedSecret) => {
                // The form this generation holds a secret in, and the only one builds up to
                // 0.200.0 read: a token sealed at the cell's coordinates. It is sealed again
                // whatever the cell holds, so a write never says whether a value was the held one.
                let value = Secret::new(text.as_str().to_owned());
                if secret::flag_like(&value) {
                    return Err(Error::new(&format!(
                        "{}.{column}: a value shaped like a flag (`1`, `yes`, under six \
                         characters) is not a {column}",
                        collection.name
                    )));
                }
                if held.is_some_and(check::protected) {
                    return Err(Error::new(&format!(
                        "{}.{column}: this row is protected and holds no {column}",
                        collection.name
                    )));
                }
                Some(Value::String(sealer.seal(
                    collection.name,
                    &pk,
                    column,
                    &value,
                )?))
            }
            (None, Holds::SealedOrdinary) if text.trim().is_empty() => Some(Value::Null),
            (None, Holds::SealedOrdinary) => {
                let digits = stored_number(collection, column, &text)?;
                last4 = Some(digits[digits.len() - 4..].to_owned());
                let number = Secret::new(digits);
                // A token is sealed with fresh randomness, so the cell's own is compared by what
                // it opens to: the number it already holds alone is not written again.
                let held_alone = one_value
                    && held
                        .and_then(|r| r.get(column))
                        .and_then(Value::as_str)
                        .is_some_and(|token| {
                            sealer.holds(collection.name, &pk, column, token, &number)
                        });
                if held_alone {
                    None
                } else {
                    Some(Value::String(sealer.seal(
                        collection.name,
                        &pk,
                        column,
                        &number,
                    )?))
                }
            }
            (None, Holds::Plain) if matches!(schema.ty, Ty::Phone) && !text.trim().is_empty() => {
                let digits = stored_number(collection, column, &text)?;
                last4 = Some(digits[digits.len() - 4..].to_owned());
                Some(Value::String(digits))
            }
            (None, Holds::Plain) => Some(
                grid::read(&schema.ty, &text)
                    .map_err(|rule| Error::new(&format!("{}.{column}: {rule}", collection.name)))?,
            ),
        };
        let x = x.filter(|x| !(one_value && held.and_then(|r| r.get(column)) == Some(x)));
        let mut superseded = BTreeSet::new();
        let mut changes = Vec::new();
        if let Some(x) = x {
            superseded = held_here;
            if let Some(entry) = secret::entry_of(&x) {
                superseded.remove(entry);
            }
            // Owed before the write that replaces them lands: if this run ends between the write
            // and the retire below, a later sweep collects them.
            if !superseded.is_empty() {
                io.store
                    .owe(&superseded.iter().cloned().collect::<Vec<_>>())?;
            }
            changes.push(cell::change(
                collection,
                CellKey {
                    pk: &pk,
                    col: column,
                    elem: None,
                },
                &CellValue::owner(collection.version, x),
            )?);
        }
        // The row's `last4` follows the number given even where the number itself stands: a row
        // whose `last4` an earlier write left behind is repaired by setting its number again.
        let last4 = last4.map(Value::String);
        if let Some(last4) = last4.filter(|last4| held.and_then(|r| r.get(LAST4)) != Some(last4)) {
            changes.push(cell::change(
                collection,
                CellKey {
                    pk: &pk,
                    col: LAST4,
                    elem: None,
                },
                &CellValue::owner(collection.version, last4),
            )?);
        }
        if changes.is_empty() {
            return Ok(((pk, superseded), Vec::new()));
        }
        Ok(((pk, superseded), vec![changes]))
    });
    let entry = stored
        .as_ref()
        .and_then(|(_, reference)| secret::entry_of(reference).map(str::to_owned));
    // The write's outcome is reported whatever the settle after it meets: a landed reference is
    // in the registry, and a settle that failed leaves what it owes to a later sweep.
    let settled =
        (holds == Holds::Entry).then(|| settle_set(handle, io.store, entry.as_deref(), &committed));
    let ((pk, superseded), applied) = committed?;
    let mut out = Map::new();
    out.insert("table".to_owned(), json!(noun.word()));
    out.insert(collection.prefix.to_owned(), json!(pk));
    out.insert("column".to_owned(), json!(column));
    out.insert("written".to_owned(), json!(applied.cells));
    if let Some(settled) = settled {
        out.insert("entry".to_owned(), json!(entry));
        out.insert("replaced".to_owned(), json!(superseded.len()));
        match settled {
            Ok(settled) => {
                out.insert("removed".to_owned(), json!(settled.removed));
                out.insert("owed".to_owned(), json!(settled.left));
            }
            Err(error) => {
                // Nothing is known removed, and the put may still be on the owed record beside
                // the entries the write replaced: a later sweep settles each.
                out.insert("removed".to_owned(), json!(0));
                out.insert(
                    "owed".to_owned(),
                    json!(superseded.len() + usize::from(entry.is_some())),
                );
                out.insert("settle_error".to_owned(), json!(error.to_string()));
            }
        }
    }
    Ok(Value::Object(out))
}

/// After a secret cell's write ([`crate::alab::secret::settle_write`]): a landed write pays the
/// entry it put and retires the entries it replaced; a refused one leaves its put owed.
fn settle_set(
    handle: &Handle,
    store: &dyn secret::Store,
    entry: Option<&str>,
    committed: &Result<((String, BTreeSet<String>), att::Applied)>,
) -> Result<secret::Settled> {
    let landed = committed.as_ref().ok().map(|((_, superseded), _)| {
        (
            entry
                .map(|entry| BTreeSet::from([entry.to_owned()]))
                .unwrap_or_default(),
            superseded,
        )
    });
    crate::alab::secret::settle_write(
        handle,
        store,
        landed.as_ref().map(|(put, replaced)| (put, *replaced)),
    )
}

/// The store entries the values of `collection.column` of row `pk` name in `view`, each
/// candidate's, those bound to the cell only ([`secret::bound`]): the entries a write of the cell
/// must not take and, once it lands, removes.
fn held_entries(
    view: &att::View,
    collection: &Collection,
    column: &str,
    pk: &str,
) -> BTreeSet<String> {
    view.project(collection)
        .rows
        .get(pk)
        .and_then(|row| row.fields.get(column))
        .map(|field| {
            field
                .candidates()
                .iter()
                .filter_map(|c| secret::entry_of(&c.value.x))
                .filter(|entry| secret::bound(collection.name, column, pk, entry))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// `aterm db emails|computers|phones|ai-accounts …` from the command line: `None` only where
/// `words` name none of them. A noun never falls through to fleet_db.
#[must_use]
pub fn main_entry(words: &[String]) -> Option<std::process::ExitCode> {
    use std::io::Write as _;
    let noun = Noun::parse(words.first()?)?;
    let report = |error: &Error| {
        let _ = writeln!(std::io::stderr(), "aterm db {}: {error}", noun.word());
        std::process::ExitCode::from(1)
    };
    // The plain view of a shared file's alias reads this Mac's snapshot and nothing of the
    // vault: it is answered before any vault path is even located for writing.
    if let (Some(file), Ok(opts), Some(root)) = (
        noun.file(),
        parse(noun, &words[1..]),
        aterm_file::Root::live(),
    ) && opts == Opts::default()
        && !words.iter().any(|word| word == "--help" || word == "-h")
    {
        let value = from_snapshot(&root, noun, file);
        return Some(match aterm_json::to_string(&value) {
            Ok(text) => aterm_uds::stdout::answer_exit("aterm db", &format!("{text}\n"), 0),
            Err(_) => std::process::ExitCode::from(1),
        });
    }
    let paths = match Paths::live() {
        Ok(paths) => paths,
        Err(error) => return Some(report(&Error::new(&error.to_string()))),
    };
    let store = crate::alab::secret::store(&paths);
    // The grid names no concealed column (`Some(&[])`): no grid cell holds a secret.
    let mut open = |file: &Path, line: usize| att::amacs(file, line, Some(&[]));
    let mut stdin = read_stdin;
    let now_ms = aterm_time::unix_millis().unwrap_or(0);
    let holding = || {
        aterm_secrets::client::fleet_holding(&paths, now_ms)
            .map_err(|error| Error::new(&error.to_string()))
    };
    let mut view_io = ViewIo {
        open: &mut open,
        read_stdin: &mut stdin,
        store: &store,
        holding: &holding,
        now_ms,
    };
    Some(match run(&paths, noun, &words[1..], &mut view_io) {
        Ok(Value::Object(fields)) if fields.contains_key("usage") => {
            aterm_uds::stdout::answer_exit(
                "aterm db",
                fields.get("usage").and_then(Value::as_str).unwrap_or(USAGE),
                0,
            )
        }
        Ok(value) => match aterm_json::to_string(&value) {
            Ok(text) => aterm_uds::stdout::answer_exit("aterm db", &format!("{text}\n"), 0),
            Err(_) => std::process::ExitCode::from(1),
        },
        Err(error) => report(&error),
    })
}

#[cfg(test)]
mod tests;
