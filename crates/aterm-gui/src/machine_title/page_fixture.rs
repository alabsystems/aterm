// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Settings ▸ This Mac's reads over scratch state, for its tests: a name file claimed one way,
//! the read the page's writer would make of it, this Mac's History as its lab registry row would
//! hold it, built by hand, and macOS's names and naming permission from a stub, so no test reads
//! or sets this Mac's own, or reads a registry.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::sync::Arc;

use aterm_agent::machine_name::file::MachineName;
use aterm_agent::machine_name::history::{History, NameCell, NameWrite};
use aterm_agent::machine_name::{InFleet, NameTo, file, publish, rename_preview, set_name};
use aterm_db::att::EntryKind;
use aterm_db::file::Renamed;
use aterm_net::fleet_member::name_record::{ByKind, FLOOR_MS};
use aterm_secrets::client::{LabLabels, LabelHold};
use aterm_types::machine_name::{Class, GeneratedNames, Nickname, Number};

use super::{PageFound, PageSnapshot, page_read};
use crate::machine_rename_sheet::Snapshot;
use crate::machine_title::naming::NamingStatus;
use crate::native_settings::{MachinePageState, PageRead};

pub(crate) const NOW: u64 = FLOOR_MS + 300 * 86_400_000;

/// The widest nickname the grammar takes (`Nickname::MAX_CHARS` 24, `MAX_BYTES` 64): 20
/// fullwidth letters (3 bytes each) and 4 ASCII `W`s, 24 letters in 64 bytes. It is wider than
/// 21 fullwidth letters (63 bytes, the most a fullwidth-only nickname fits), by the fit audit's
/// measure and in a real font alike, where 4 ASCII `W`s outspan one fullwidth letter.
pub(crate) const WIDEST_NICK: &str = "\u{FF37}\u{FF37}\u{FF37}\u{FF37}\u{FF37}\u{FF37}\u{FF37}\u{FF37}\u{FF37}\u{FF37}\
     \u{FF37}\u{FF37}\u{FF37}\u{FF37}\u{FF37}\u{FF37}\u{FF37}\u{FF37}\u{FF37}\u{FF37}WWWW";

/// A scratch state folder, private as `aterm name` makes it, beside its own `aterm.toml`.
pub(crate) struct Scratch {
    _root: aterm_tempfile::TempDir,
    pub(crate) state: PathBuf,
    pub(crate) config: PathBuf,
}

fn scratch() -> Scratch {
    let root = aterm_tempfile::tempdir().expect("a scratch root");
    // The state sits in its own scratch parent: publication must find no vault beside it.
    let state = root.path().join("state");
    std::fs::create_dir(&state).expect("a state folder");
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))
        .expect("a private state folder");
    let config = root.path().join("aterm.toml");
    Scratch {
        _root: root,
        state,
        config,
    }
}

/// Give `scratch` a node key, so a read of it names this Mac's node.
pub(crate) fn with_node(scratch: &Scratch) {
    aterm_net::fleet_member::node_identity(&scratch.state).expect("a node key");
}

pub(crate) fn nick(word: &str) -> Nickname {
    Nickname::parse(word).expect("a nickname")
}

pub(crate) fn num(n: u32) -> Number {
    Number::new(n).expect("a number")
}

/// A state whose name is `n` with nickname `word`, claimed as `class`: chosen through
/// [`set_name`] as a person's rename makes it, automatic or carried over as the file holds it.
pub(crate) fn named(class: Class, n: u32, word: &str) -> Scratch {
    let scratch = scratch();
    rename(&scratch, class, n, word);
    scratch
}

/// Rename `scratch` to `n` and `word`, claimed as `class`.
pub(crate) fn rename(scratch: &Scratch, class: Class, n: u32, word: &str) {
    if class == Class::Chosen {
        set_name(
            &scratch.state,
            &NameTo {
                num: num(n),
                nick: Some(nick(word)),
            },
            || Ok(LabLabels::default()),
            ByKind::Person,
            || Ok(None),
            NOW,
            false,
        )
        .expect("sets the name");
        return;
    }
    let make = || {
        let mut name = MachineName::unnumbered(nick(word), NOW);
        name.num = Some(num(n));
        name.class = class;
        name
    };
    if file::read(&scratch.state).expect("reads").is_none() {
        file::read_or_create(&scratch.state, || Ok(make())).expect("writes the name");
    } else {
        file::replace(&scratch.state, |name| {
            let fresh = make();
            name.num = fresh.num;
            name.class = fresh.class;
            name.nick = fresh.nick;
            name.rev += 1;
            Ok(())
        })
        .expect("renames");
    }
}

/// macOS's names and naming permission as a test sets them: the names it applies are read back.
pub(crate) struct Names {
    pub(crate) authorization: RefCell<Result<bool, String>>,
    values: RefCell<[Option<String>; 3]>,
}

impl Names {
    pub(crate) fn authorized(authorization: Result<bool, String>) -> Self {
        Self {
            authorization: RefCell::new(authorization),
            values: RefCell::new([None, None, None]),
        }
    }
}

impl publish::SystemNames for Names {
    fn read(&self) -> [Option<String>; 3] {
        self.values.borrow().clone()
    }

    fn apply(
        &self,
        _: &std::path::Path,
        _: &std::path::Path,
        names: &GeneratedNames,
    ) -> Result<(), String> {
        *self.values.borrow_mut() = [
            Some(names.computer_name().to_owned()),
            Some(names.host_name().to_owned()),
            Some(names.host_name().to_owned()),
        ];
        Ok(())
    }

    fn authorization(&self, _: &std::path::Path, _: &std::path::Path) -> Result<bool, String> {
        self.authorization.borrow().clone()
    }
}

/// Publish `scratch`'s chosen number everywhere it goes, so nothing is pending.
pub(crate) fn publish_all(scratch: &Scratch, names: &Names) {
    let name = file::read(&scratch.state).expect("reads").expect("a name");
    let number = name.num.expect("a number");
    let policy = publish::Policy::read(&scratch.config, number).expect("a naming policy");
    publish::publish(&scratch.state, &scratch.config, &policy, Some(names))
        .unwrap_or_else(|pending| panic!("publishes: {}", pending.phrase()));
}

/// The vault's labels holding `held` for other Macs.
pub(crate) fn labels(held: &[u32]) -> LabLabels {
    LabLabels {
        held: held
            .iter()
            .map(|n| (num(*n), LabelHold::Label))
            .collect::<BTreeMap<_, _>>(),
        ..LabLabels::default()
    }
}

/// A read of `scratch` with the vault's labels `lab` gives, with the naming status `status`
/// makes of it.
fn found_with(
    scratch: &Scratch,
    lab: impl FnOnce() -> std::io::Result<LabLabels>,
    status: impl FnOnce(&MachineName) -> NamingStatus,
) -> PageFound {
    let preview = rename_preview(&scratch.state, lab, || Ok(None), NOW + 1).expect("a page read");
    let status = status(preview.name()).with_fleet_membership(InFleet::Out);
    PageFound {
        snapshot: Snapshot::named(preview, status),
        fleet: Some(InFleet::Out),
        // What `history::read` answers for a name kept in a scratch folder: no registry row is
        // its, so it has no History.
        history: Ok(History::default()),
    }
}

/// A read of `scratch`, the vault holding `held` for other Macs, whose naming status also
/// observed macOS's names and permission through `names`, as the Rename panel's diagnostics do.
pub(crate) fn found(scratch: &Scratch, held: &[u32], names: &Names) -> PageFound {
    let lab = labels(held);
    found_with(
        scratch,
        || Ok(lab),
        |name| NamingStatus::read(&scratch.state, &scratch.config, name, names),
    )
}

/// [`found`] with `history` as the page's History read answered it.
pub(crate) fn found_with_history(
    scratch: &Scratch,
    held: &[u32],
    names: &Names,
    history: Result<History, String>,
) -> PageFound {
    PageFound {
        history,
        ..found(scratch, held, names)
    }
}

/// A read of `scratch`, the vault holding `held` for other Macs, as the page's own reader makes
/// it ([`super::read_page`]): its naming status from local files alone, observing no OS name and
/// no permission.
pub(crate) fn found_local(scratch: &Scratch, held: &[u32]) -> PageFound {
    let lab = labels(held);
    found_with(
        scratch,
        || Ok(lab),
        |name| NamingStatus::read_local(&scratch.state, &scratch.config, name),
    )
}

/// A read of `scratch` as the page's own reader makes it where the vault's labels could not be
/// read, so a number that moves this Mac's is left for the write to check.
pub(crate) fn found_unread_labels(scratch: &Scratch) -> PageFound {
    found_with(
        scratch,
        || Err(std::io::Error::other("the vault is busy")),
        |name| NamingStatus::read_local(&scratch.state, &scratch.config, name),
    )
}

/// Another node's claim on `n` in `scratch`'s books, as an older build makes one: an implied
/// entry, its signed primary name `m<n>` with no name record, which a rename to `n` is refused
/// over ([`aterm_types::machine_name::Verdict::HeldByOlderBuild`]). It replaces the books.
pub(crate) fn held_by_older_build(scratch: &Scratch, n: u32) {
    use aterm_net::fleet_member::{PEERS_FILE, Peer};
    let other = aterm_net::fleet::NodeIdentity::from_seed(&"b2".repeat(16), &[0x22; 32])
        .expect("another identity");
    let host = format!("m{n}.local");
    let line = Peer::own(&other, (host.as_str(), 7485, "0.160.0"), NOW)
        .with_primary_name(&other, Some(&format!("m{n}")))
        .line()
        + "\n";
    aterm_uds::private_file::write_private(&scratch.state, PEERS_FILE, line.as_bytes())
        .expect("their claim");
}

/// The key of this Mac's lab registry row in a hand-built History.
pub(crate) const ROW: &str = "mc-0123456789abcdef";

/// The Mac each hand-built write names as its writer.
pub(crate) const WRITER: &str = "m8";

/// What one write left in `cell`: `value`, standing or not, over the values it `replaced`.
pub(crate) fn cell(cell: Renamed, value: &str, stands: bool, replaced: &[&str]) -> NameCell {
    NameCell {
        cell,
        value: Some(value.to_owned()),
        stands,
        replaced: replaced
            .iter()
            .map(|value| Some((*value).to_owned()))
            .collect(),
        unseen: Vec::new(),
    }
}

/// Write `op` (its digest every byte `op`) of `kind` by Mac `mac`, authored `op` seconds after
/// [`NOW`], leaving `cells`.
pub(crate) fn write(op: u8, mac: &str, kind: EntryKind, cells: Vec<NameCell>) -> NameWrite {
    NameWrite {
        op: [op; 32],
        at_ms: NOW + u64::from(op) * 1_000,
        mac: mac.to_owned(),
        kind,
        reverts: None,
        carried: false,
        cells,
    }
}

/// This Mac's row ([`ROW`]) holding `writes`, newest first, with nothing of its on the way.
pub(crate) fn history(writes: Vec<NameWrite>) -> History {
    History {
        row: Some(ROW.to_owned()),
        pending: Vec::new(),
        writes,
    }
}

/// The History this Mac's row holds after `names` (number and nickname, oldest first), each a
/// person's rename at [`WRITER`] and so an explicit write: the first writes both cells, each
/// later one the cells it changed over what it replaced, and a cell's newest write alone stands.
pub(crate) fn renames(names: &[(u32, &str)]) -> History {
    let mut writes: Vec<NameWrite> = Vec::new();
    let mut last: [Option<String>; 2] = [None, None];
    for (at, (n, word)) in names.iter().enumerate() {
        let op = u8::try_from(at + 1).expect("at most 255 renames");
        let mut cells = Vec::new();
        for (slot, (column, value)) in [
            (Renamed::Label, format!("m{n}")),
            (Renamed::Nickname, (*word).to_owned()),
        ]
        .into_iter()
        .enumerate()
        {
            if last[slot].as_ref() == Some(&value) {
                continue;
            }
            for older in &mut writes {
                for written in &mut older.cells {
                    if written.cell == column {
                        written.stands = false;
                    }
                }
            }
            let replaced: Vec<&str> = last[slot].iter().map(String::as_str).collect();
            cells.push(cell(column, &value, true, &replaced));
            last[slot] = Some(value);
        }
        writes.push(write(op, WRITER, EntryKind::Explicit, cells));
    }
    writes.reverse();
    history(writes)
}

/// A scratch name renamed through `names` (number and nickname, oldest first), each chosen as a
/// person's rename makes it, and the History its registry row would hold after them
/// ([`renames`]).
pub(crate) fn renamed(names: &[(u32, &str)]) -> (Scratch, History) {
    let (first, rest) = names.split_first().expect("at least one name");
    let scratch = named(Class::Chosen, first.0, first.1);
    for (n, word) in rest {
        rename(&scratch, Class::Chosen, *n, word);
    }
    (scratch, renames(names))
}

/// `found` as read `rev` shows it.
pub(crate) fn snapshot(rev: u64, found: PageFound) -> Arc<PageSnapshot> {
    match page_read(rev, found, None, None) {
        PageRead::Read(read) => read,
        other => panic!("a read with a name: {other:?}"),
    }
}

/// The page's state over read `rev` of `found`.
pub(crate) fn page(rev: u64, found: PageFound, windowed: bool) -> MachinePageState {
    MachinePageState {
        read: PageRead::Read(snapshot(rev, found)),
        notice: None,
        busy: false,
        windowed,
    }
}
