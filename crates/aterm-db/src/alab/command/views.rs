// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm db emails|computers|phones|ai-accounts`: the owner's tables under the names the owner
//! learned from fleet_db (docs/DESIGN-alab-one-mesh-2026-11-05.md §4.7, §4.8, §11 A7), now
//! aliases of the shared files (docs/DESIGN-shared-files-2026-11-11.md §3). None of them reads
//! fleet_db, whatever this Mac's registry holds or refuses.
//!
//! - **The view.** Every row, each column as the grid shows it, with the state word of the
//!   answer (`live`, `held`, `never`, `off`) and why. `ai-accounts` is the shared file
//!   `ai/accounts` and `computers` is `lab/machines`: each is read from that file's snapshot on
//!   this Mac ([`aterm_file::File::read`]), so it answers the last materialized rows whatever
//!   this Mac's custody is doing, and opens none. `emails` and `phones` hold a `Secret` column,
//!   which no shared file may (their files wait for LT-04), so their view is still projected
//!   from the registry; where that read refuses, the answer is no rows and `never` with the
//!   reason. An address is masked unless `--show-addresses`, a conflict shows as `⚠ a ‖ b`, a
//!   `Secret` cell as `••••••` when set and empty when not.
//! - **`--registry`** (`ai-accounts`, `computers`) is the noun's own projection of the
//!   registry, read through this Mac's Commons custody, so it refuses where custody does. It
//!   carries what the snapshot does not hold yet: for `ai-accounts` the `seen_on` and
//!   `last_seen` columns and the observed `status` (from every Mac's `account-obs` rows and the
//!   journal's `seen` events, judged against this Mac's clock), for `computers` each live seat
//!   no machine row binds. It stays until the observation files `ai/logins` and `ai/usage`
//!   (shared-files slice SF-08) carry those facts.
//! - **`--show-passwords`** (`emails`) and **`--show-numbers`** (`phones`) show the `Secret`
//!   column in plaintext, each cell through the audited reveal (`att::open_secrets`), and only
//!   when the session may ([`super::session`]). In a person's aterm tab they print no value: text
//!   printed into a tab stays in its scrollback, blocks and session log, where `aterm ctl` reads it
//!   back. They open the `--edit` grid read-only in an Editor tab beside it (`att::show_grid`),
//!   whose document amacs keeps no draft of and serves no reader, and the tab gets one line naming
//!   it. Outside aterm, where no session answers, they print the table: that terminal's history is
//!   the person's own (docs/DESIGN-alab-one-mesh-2026-11-05.md §4.8, A17).
//! - **`--edit`** opens the collection in the amacs grid, the `Secret` column revealed and
//!   editable (`att::edit_grid`), under the same session rule: in an aterm tab always in an Editor
//!   tab beside it, never drawn in the terminal.
//! - **`--digest`** prints the table's keyed digest under the newest `[everyone]` epoch this Mac
//!   holds (its first 8 hex), its row count and how many rows hold the secret: what agents and
//!   other members compare, never a value.
//! - **`set <row> <column> [--stdin]`** writes one cell as the owner. A `Secret` value comes
//!   only from standard input, never argv, with one trailing newline from a pipe removed; it is
//!   not session-gated, since writing a value shows none: it is how an agent edits.

use super::{named_row, verbs};
use crate::{
    Error, Result,
    alab::{
        registry::{self, ACCOUNT, ADDRESS, MACHINE, PHONE},
        secret::{Journal, Surface},
        view::LabView,
    },
    att::{self, Handle},
    typed::{
        Collection, Ty,
        cell::{self, CellKey, CellValue},
        check, grid,
        secret::Secret,
    },
};
use aterm_file::{Why, Word};
use aterm_json::{Map, Value, json};
use aterm_secrets::client::Paths;
use std::collections::BTreeMap;
use std::path::Path;

pub use super::session::Place;

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

    fn collection(self) -> &'static Collection {
        match self {
            Self::Emails => &ADDRESS,
            Self::Computers => &MACHINE,
            Self::Phones => &PHONE,
            Self::AiAccounts => &ACCOUNT,
        }
    }

    /// The shared file this noun is an alias of, when its collection may be one: `emails` and
    /// `phones` hold a `Secret` column, so theirs wait for LT-04.
    const fn file(self) -> Option<&'static crate::file::Type> {
        match self {
            Self::AiAccounts => Some(&crate::file::ACCOUNTS),
            Self::Computers => Some(&crate::file::MACHINES),
            Self::Emails | Self::Phones => None,
        }
    }

    /// The flag that shows this table's `Secret` column, and that column, with the surface a
    /// reveal journals.
    fn shown(self) -> Option<(&'static str, &'static str, Surface)> {
        match self {
            Self::Emails => Some(("--show-passwords", "password", Surface::Terminal)),
            Self::Phones => Some(("--show-numbers", "number", Surface::Phone)),
            Self::Computers | Self::AiAccounts => None,
        }
    }
}

/// What one view invocation asks for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Opts {
    show_addresses: bool,
    show_secrets: bool,
    /// The noun's own registry projection rather than its shared file's snapshot.
    registry: bool,
    digest: bool,
    edit: bool,
    /// `set <row> <column> [<value>]`, and whether the value comes from stdin.
    set: Option<(String, String, Option<String>)>,
    stdin: bool,
}

const USAGE: &str = "aterm db emails|computers|phones|ai-accounts — the owner's tables

  aterm db <table> [--show-addresses]
                    every row, each column as the grid shows it: addresses masked, a password
                    or number as •••••• when set, a conflict as ⚠ a ‖ b; with `state` (live,
                    held, never or off) and `why`, which say how current the answer is and are
                    never an error. `ai-accounts` is the shared file ai/accounts and
                    `computers` is lab/machines (`aterm file cat <name>`): read from this Mac's
                    snapshot, whatever its Commons custody is doing. `emails` and `phones` are
                    read from the registry; where that refuses they answer no rows and `never`
  aterm db ai-accounts --registry | aterm db computers --registry
                    the table's own view of the registry, read through this Mac's Commons
                    custody (it refuses where custody does), with what the snapshot does not
                    hold yet. `ai-accounts` adds seen_on, the Macs a tab ran it on in the last
                    two hours, and last_seen, from every Mac's account-obs rows and the
                    journal's seen events; a pending account a Mac runs reads `signed in · on
                    <mac>`, and a paused, broken or banned one `<status> · signed in on <mac>`.
                    `computers` adds each live seat no machine row binds
  aterm db emails --show-passwords | aterm db phones --show-numbers
                    the same with the secret column in plaintext: only in a person's own aterm
                    tab or terminal (not one `aterm ctl spawn` started, held, driven, or running
                    an agent), with standard input and output a terminal; in an aterm tab it
                    opens read-only in an Editor tab beside it and prints only where, since a
                    tab keeps what is printed in it
  aterm db <table> --edit
                    the table in the amacs grid, its secret column revealed and editable; the
                    same session rule, and in an aterm tab always in an Editor tab beside it
  aterm db <table> --digest
                    the table's keyed digest under the newest [everyone] epoch held here (its
                    first 8 hex), its rows and how many hold the secret; no value
  aterm db <table> set <row> <column> [<value> | --stdin]
                    write one cell as the owner; a password or number only from --stdin (one
                    trailing newline from a pipe is removed), never on the command line
";

fn parse(noun: Noun, words: &[String]) -> Result<Opts> {
    let mut opts = Opts::default();
    let mut operands = Vec::new();
    for word in words {
        match word.as_str() {
            "--show-addresses" => opts.show_addresses = true,
            "--registry" if noun.file().is_some() => opts.registry = true,
            "--digest" => opts.digest = true,
            "--edit" => opts.edit = true,
            "--stdin" => opts.stdin = true,
            flag if noun.shown().is_some_and(|(shown, _, _)| shown == flag) => {
                opts.show_secrets = true;
                opts.show_addresses = true;
            }
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
    let modes = [
        opts.show_secrets,
        opts.digest,
        opts.edit,
        opts.set.is_some(),
    ]
    .iter()
    .filter(|on| **on)
    .count();
    if modes > 1 {
        return Err(Error::new(
            "choose one of --show-passwords/--show-numbers, --digest, --edit and set",
        ));
    }
    if opts.registry && modes > 0 {
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

/// One grid file to open for the person, and where.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Opening<'p> {
    pub file: &'p Path,
    /// The 1-based line the caret lands on.
    pub line: usize,
    /// The person's aterm tab, by session (`s-<hex>`): the file opens in an Editor tab beside
    /// it. `None`: outside aterm, in this terminal.
    pub beside: Option<&'p str>,
    /// Opened to be read only (`--show-passwords`, `--show-numbers`).
    pub read_only: bool,
    /// The headers of the grid's `Secret` columns, whose cells no reader of the window but the
    /// person's eyes gets (`edit … secret=`, `att::Opener`).
    pub secret: &'p [String],
}

/// What a view reads and opens beyond the registry: whether and where the session may reveal,
/// the grid opener (which returns once the person closes the file), standard input for
/// `set --stdin`, the reveal journal, and the clock.
pub struct ViewIo<'a> {
    pub gate: &'a dyn Fn() -> Result<Place>,
    pub open: &'a mut dyn FnMut(&Opening<'_>) -> Result<()>,
    pub read_stdin: &'a mut dyn FnMut() -> Result<zeroize::Zeroizing<Vec<u8>>>,
    pub journal: &'a Journal,
    pub now_ms: u64,
}

/// Run one view: `words` after the noun.
///
/// # Errors
/// The words are not a view's, the registry cannot be read or written, the session may not
/// reveal, or the cell named cannot take the value.
pub fn run(paths: &Paths, noun: Noun, words: &[String], io: &mut ViewIo<'_>) -> Result<Value> {
    if words.iter().any(|w| w == "--help" || w == "-h") {
        return Ok(json!({"usage": USAGE}));
    }
    let opts = parse(noun, words)?;
    if let Some(file) = noun.file()
        && opts.set.is_none()
        && !opts.digest
        && !opts.edit
        && !opts.registry
    {
        // An alias of `aterm file cat`: this Mac's snapshot, read before anything here reaches
        // for the registry, so no custody is opened.
        let root = crate::file::root(&crate::generation::original(paths));
        return Ok(from_snapshot(&root, noun, file, opts.show_addresses));
    }
    let handle = Handle::at(paths.clone(), &att::REGISTRY);
    if let Some((row, column, value)) = &opts.set {
        return set(&handle, noun, row, column, value.as_deref(), opts.stdin, io);
    }
    if opts.digest {
        return digest(paths, &handle, noun);
    }
    if opts.edit {
        let place = (io.gate)()?;
        return edit(&handle, noun, &place, io);
    }
    if opts.show_secrets
        && let Place::Tab(sid) = (io.gate)()?
    {
        return show_in_tab(&handle, noun, &sid, io);
    }
    let view = match handle.view() {
        Ok(view) => view,
        // `--registry` asks for the registry itself: where custody refuses, so does it. The
        // noun's plain view still answers, from the snapshot.
        Err(error) if opts.registry => return Err(error),
        Err(error) => return Ok(unread(paths, noun, &error)),
    };
    let collection = noun.collection();
    let projection = view.project(collection);
    let revealed = if opts.show_secrets {
        reveal_all(&handle, noun, &view.cells, io)?
    } else {
        grid::Opened::new()
    };
    let mut rows: Vec<Value> = projection
        .rows
        .iter()
        .map(|(pk, row)| shown_row(collection, pk, row, opts.show_addresses, &revealed))
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
    out.insert(noun.word().to_owned(), Value::Array(rows));
    Ok(Value::Object(out))
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

/// `text` with every address in it masked ([`grid::mask`]), as the views show one unless the
/// owner asks for addresses.
fn masked(text: &str) -> String {
    if !text.contains('@') {
        return text.to_owned();
    }
    text.split(' ')
        .map(|word| {
            if word.contains('@') {
                grid::mask(word.trim_start_matches(['"', '(', '[', '<']))
            } else {
                word.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A noun that is a shared file's alias, from that file's snapshot under `root`: its rows as
/// the last pass left them, with the file's state word. It reads local files only.
fn from_snapshot(
    root: &aterm_file::Root,
    noun: Noun,
    file: &crate::file::Type,
    show_addresses: bool,
) -> Value {
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
                    } else if show_addresses {
                        json!(cell)
                    } else {
                        json!(masked(cell))
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

/// One row as the view shows it: its key and each live column, by name.
fn shown_row(
    collection: &'static Collection,
    pk: &str,
    row: &cell::Row,
    show_addresses: bool,
    revealed: &grid::Opened,
) -> Value {
    let mut out = Map::new();
    out.insert(collection.prefix.to_owned(), json!(pk));
    for column in collection
        .columns
        .iter()
        .filter(|c| c.live_at(collection.version))
    {
        let opened = (!revealed.is_empty() && matches!(column.ty, crate::typed::Ty::Secret))
            .then(|| revealed.get(&(pk.to_owned(), column.name.to_owned())))
            .flatten();
        let shown = match opened {
            Some(values) => grid::show_opened(values),
            None => grid::show_field(column, row.fields.get(column.name), show_addresses),
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

/// Every set `Secret` cell of `noun`'s shown column, revealed through the audited exit
/// ([`att::open_secrets`]): by row, each candidate of a conflict. A protected row's cell is
/// never revealed, and one that does not open here stays masked.
fn reveal_all(
    handle: &Handle,
    noun: Noun,
    cells: &cell::Cells,
    io: &ViewIo<'_>,
) -> Result<grid::Opened> {
    let Some((_, column, surface)) = noun.shown() else {
        return Ok(grid::Opened::new());
    };
    att::open_secrets(
        handle,
        noun.collection(),
        Some(column),
        cells,
        io.journal,
        surface,
        io.now_ms,
    )
}

/// How long a sighting keeps an account on a Mac: two of a running tab's hourly `seen`
/// refreshes (`docs/DESIGN-account-discovery-2026-11-09.md`, "In use on <mac>").
const SEEN_LIVE_MS: u64 = 2 * 3_600_000;

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

/// One journal event a view folds: a tab's `seen`, or the `session-end` that closes it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Heard {
    /// The writer's Commons uuid, 32 hex.
    uuid: String,
    account: String,
    tab: String,
    /// When, Unix milliseconds.
    t_ms: u64,
    /// A `session-end` of that tab, rather than a sighting.
    ended: bool,
}

/// The `seen` and tabbed `session-end` events of the journal window this Mac holds at `now_ms`;
/// none where it holds no Commons or the window cannot be read (the view still shows the rows).
fn heard(paths: &Paths, now_ms: u64) -> Vec<Heard> {
    use crate::alab::journal::{Kind, seen};
    let Ok(Some(window)) = seen::read(paths, None, now_ms / 1_000) else {
        return Vec::new();
    };
    window
        .events
        .iter()
        .filter_map(|logged| {
            let event = &logged.event;
            let ended = match event.kind {
                Kind::Seen => false,
                Kind::SessionEnd => true,
                _ => return None,
            };
            Some(Heard {
                uuid: logged.uuid.clone(),
                account: event.account.as_ref()?.as_str().to_owned(),
                tab: event.tab.as_ref()?.as_str().to_owned(),
                t_ms: event.t.saturating_mul(1_000),
                ended,
            })
        })
        .collect()
}

/// Every account's [`SeenOn`] at `now_ms` from the `account-obs` rows `obs` and the heard events
/// `heard`, each Mac named by `name` (from its Commons uuid). A Mac runs an account now where its
/// row says `running` with a tab and was written within [`SEEN_LIVE_MS`], or where one of its tabs
/// was seen on it within that span and has not ended since; its tabs are the more of the row's
/// count and the distinct tabs heard. A row whose key does not end in a uuid, or whose prefix is
/// not the account it names, is no Mac's word and is skipped, as the collection's writers rule
/// admits only its signer's own suffix. Every row and sighting dates `last_ms`, however old.
fn seen_on(
    obs: &crate::typed::owned::OwnedRead,
    heard: &[Heard],
    name: &dyn Fn(&str) -> String,
    now_ms: u64,
) -> BTreeMap<String, SeenOn> {
    let live = |at: u64| now_ms.saturating_sub(at) <= SEEN_LIVE_MS;
    let mut out: BTreeMap<String, SeenOn> = BTreeMap::new();
    for (key, row) in &obs.rows {
        let row = &row.value;
        let Some(account) = row.get("account").and_then(Value::as_str) else {
            continue;
        };
        let Some((prefix, uuid)) = key.rsplit_once('.') else {
            continue;
        };
        if prefix != account || crate::typed::keys::parse_uuid32(uuid).is_none() {
            continue;
        }
        let at = row.get("at").and_then(Value::as_u64).unwrap_or(0);
        let running = row.get("state").and_then(Value::as_str) == Some("running");
        let tabs = row.get("tabs").and_then(Value::as_u64).unwrap_or(0);
        let seen = out.entry(account.to_owned()).or_default();
        seen.last_ms = seen.last_ms.max(at);
        if running && tabs > 0 && live(at) {
            let mac = seen.macs.entry(name(uuid)).or_default();
            *mac = (*mac).max(tabs);
        }
    }
    // Each (account, Mac, tab)'s newest word: a sighting, or its session's end.
    let mut tabs: BTreeMap<(&str, &str, &str), (u64, bool)> = BTreeMap::new();
    for event in heard {
        let newest = tabs
            .entry((&event.account, &event.uuid, &event.tab))
            .or_insert((event.t_ms, event.ended));
        if event.t_ms >= newest.0 {
            *newest = (event.t_ms, event.ended);
        }
        if !event.ended {
            let seen = out.entry(event.account.clone()).or_default();
            seen.last_ms = seen.last_ms.max(event.t_ms);
        }
    }
    let mut running: BTreeMap<(&str, &str), u64> = BTreeMap::new();
    for ((account, uuid, _), (at, ended)) in tabs {
        if !ended && live(at) {
            *running.entry((account, uuid)).or_default() += 1;
        }
    }
    for ((account, uuid), n) in running {
        let mac = out
            .entry(account.to_owned())
            .or_default()
            .macs
            .entry(name(uuid))
            .or_default();
        *mac = (*mac).max(n);
    }
    out
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

/// `--digest`: the collection's keyed digest under the newest `[everyone]` epoch held here.
fn digest(paths: &Paths, handle: &Handle, noun: Noun) -> Result<Value> {
    let (epoch, key) = super::everyone(paths).ok_or_else(|| {
        Error::new("this Mac holds no [everyone] key, so the table cannot be digested")
    })?;
    let digest = handle.secret_digest(noun.collection().name, &epoch, &key)?;
    let mut out = json!({
        "table": noun.word(),
        "epoch": digest.epoch.get(..8).unwrap_or_default(),
        "rows": digest.rows,
        "digest": digest.digest,
    });
    if let (Some((_, column, _)), Value::Object(fields)) = (noun.shown(), &mut out) {
        fields.insert(format!("with_{column}"), json!(digest.with_secret));
    }
    Ok(out)
}

/// `--show-passwords` / `--show-numbers` in a person's aterm tab `sid`: the `--edit` grid,
/// read-only, in an Editor tab beside it, and nothing printed here but what the opener says when
/// the tab opens (`Null`).
fn show_in_tab(handle: &Handle, noun: Noun, sid: &str, io: &mut ViewIo<'_>) -> Result<Value> {
    let secrets = att::GridSecrets {
        journal: io.journal,
        when_ms: io.now_ms,
    };
    let open = &mut *io.open;
    let mut opener = |file: &Path, line: usize, secret: &[String]| {
        open(&Opening {
            file,
            line,
            beside: Some(sid),
            read_only: true,
            secret,
        })
    };
    att::show_grid(handle, noun.collection(), &secrets, &mut opener)?;
    Ok(Value::Null)
}

/// `--edit`: the collection in the grid, its `Secret` column revealed and editable, addresses
/// shown: beside the person's tab in an aterm tab, in this terminal outside aterm.
fn edit(handle: &Handle, noun: Noun, place: &Place, io: &mut ViewIo<'_>) -> Result<Value> {
    let collection = noun.collection();
    let mut problems = Vec::new();
    let mut report = |line: &str| problems.push(line.to_owned());
    let secrets = att::GridSecrets {
        journal: io.journal,
        when_ms: io.now_ms,
    };
    let beside = match place {
        Place::Tab(sid) => Some(sid.as_str()),
        Place::Outside => None,
    };
    let open = &mut *io.open;
    let mut opener = |file: &Path, line: usize, secret: &[String]| {
        open(&Opening {
            file,
            line,
            beside,
            read_only: false,
            secret,
        })
    };
    let edited = att::edit_grid(
        handle,
        collection,
        &[],
        true,
        Some(&secrets),
        &mut opener,
        &mut report,
    )?;
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

/// `set <row> <column> [<value> | --stdin]`: one owner write. A `Secret` value comes only from
/// stdin, is refused when shaped like a flag and on a protected row, and is sealed inside the
/// writing session; any other value is read in the grid's spelling of its type. Every write
/// passes the registry's rules on the owner's writes ([`registry::guard`]): an `emails … addr`
/// that another row says, whose birth key another row holds, or that renames an account's login
/// is refused, and one that passes writes the row's `domain` in the same record.
fn set(
    handle: &Handle,
    noun: Noun,
    row: &str,
    column: &str,
    value: Option<&str>,
    stdin: bool,
    io: &mut ViewIo<'_>,
) -> Result<Value> {
    let collection = noun.collection();
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
    let secret = matches!(schema.ty, Ty::Secret);
    if !secret && grid::read_only(schema) {
        return Err(Error::new(&format!(
            "{}.{column} is written by tools, never typed",
            collection.name
        )));
    }
    let text: zeroize::Zeroizing<String> = match (value, stdin) {
        (Some(_), _) if secret => {
            return Err(Error::new(
                "a password or number never goes on the command line: pipe it to `set … --stdin`",
            ));
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
        (None, false) if secret => {
            return Err(Error::new(
                "a password or number comes from standard input: `set … --stdin`",
            ));
        }
        (None, false) => {
            return Err(Error::new("set takes the value, or --stdin"));
        }
    };
    let (pk, applied) = handle.commit_owner_sealed(|view, sealer| {
        let pk = row_named(view, noun, row)?;
        let projection = view.project(collection);
        let held = projection.rows.get(&pk);
        let x = if secret {
            if text.is_empty() {
                Value::Null
            } else {
                let value = Secret::new(text.as_str().to_owned());
                if crate::typed::secret::flag_like(&value) {
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
                Value::String(sealer.seal(collection.name, &pk, column, &value)?)
            }
        } else {
            grid::read(&schema.ty, &text)
                .map_err(|rule| Error::new(&format!("{}.{column}: {rule}", collection.name)))?
        };
        if !secret && held.and_then(|r| r.get(column)) == Some(&x) {
            return Ok((pk, Vec::new()));
        }
        let change = cell::change(
            collection,
            CellKey {
                pk: &pk,
                col: column,
                elem: None,
            },
            &CellValue::owner(collection.version, x),
        )?;
        Ok((pk, vec![vec![change]]))
    })?;
    let mut out = Map::new();
    out.insert("table".to_owned(), json!(noun.word()));
    out.insert(collection.prefix.to_owned(), json!(pk));
    out.insert("column".to_owned(), json!(column));
    out.insert("written".to_owned(), json!(applied.cells));
    Ok(Value::Object(out))
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
    ) && opts
        == (Opts {
            show_addresses: opts.show_addresses,
            ..Opts::default()
        })
        && !words.iter().any(|word| word == "--help" || word == "-h")
    {
        let value = from_snapshot(&root, noun, file, opts.show_addresses);
        return Some(match aterm_json::to_string(&value) {
            Ok(text) => aterm_uds::stdout::answer_exit("aterm db", &format!("{text}\n"), 0),
            Err(_) => std::process::ExitCode::from(1),
        });
    }
    let paths = match Paths::live() {
        Ok(paths) => paths,
        Err(error) => return Some(report(&Error::new(&error.to_string()))),
    };
    let journal = match Journal::local() {
        Ok(journal) => journal,
        Err(error) => return Some(report(&error)),
    };
    let gate = || {
        #[cfg(unix)]
        {
            super::session::decide(&super::session::facts())
        }
        #[cfg(not(unix))]
        {
            Err(Error::new("plaintext is shown only on macOS and Linux"))
        }
    };
    let mut open = |opening: &Opening<'_>| open_for_person(noun, opening);
    let mut stdin = read_stdin;
    let mut view_io = ViewIo {
        gate: &gate,
        open: &mut open,
        read_stdin: &mut stdin,
        journal: &journal,
        now_ms: aterm_time::unix_millis().unwrap_or(0),
    };
    Some(match run(&paths, noun, &words[1..], &mut view_io) {
        Ok(Value::Null) => std::process::ExitCode::SUCCESS,
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

/// The production grid opener: beside the person's tab in an Editor tab
/// ([`att::in_editor_tab`]), naming a read-only one in one line here once it opens; outside
/// aterm, `aterm edit` in this terminal, which plaintext reaches only outside aterm.
fn open_for_person(noun: Noun, opening: &Opening<'_>) -> Result<()> {
    match opening.beside {
        Some(sid) => {
            let mut said = |ok: &aterm_ctl::edit::EditOk| {
                if opening.read_only {
                    aterm_uds::stdout::println(format_args!("{}", opened_line(noun, ok.view)));
                }
            };
            att::in_editor_tab(
                sid,
                opening.file,
                opening.line,
                opening.read_only,
                opening.secret,
                &mut said,
            )
        }
        None => att::amacs(opening.file, opening.line, Some(opening.secret)),
    }
}

/// The one line a person's tab gets for a read-only grid: where the plaintext is, never a value.
fn opened_line(noun: Noun, view: u64) -> String {
    let shown = match noun.shown() {
        Some((_, "number", _)) => "numbers",
        Some(_) => "passwords",
        None => "table",
    };
    format!(
        "aterm db {}: the {shown} are in the read-only Editor tab beside this one (view {view}); \
         close it when done",
        noun.word()
    )
}

#[cfg(test)]
mod tests;
