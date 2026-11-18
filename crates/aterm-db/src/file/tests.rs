// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared files on a stand-in lab: a scratch Commons whose registry holds made-up accounts, a
//! machine and a link, materialized under the stand-in home's own `~/.aterm/files`. No live
//! vault, registry or account is read.

use super::{
    Class, Computed, Store, Type,
    materialize::{self, Outcome, Pass},
    publish::{self, Answer},
    seam, write,
};
use crate::{
    alab::{
        Registry,
        command::views::{self, Noun, ViewIo},
        registry::{ACCOUNT, ACCOUNT_OBS, ADDRESS, MACHINE, PREFERENCE},
    },
    att, generation,
    table::tests::{Lab, repair_commons_among, repair_commons_among_first},
    typed::{grid, grid::tests::put, keys},
};
use aterm_file::{File, Name, Recorded, Root, Why, Word, cli};
use aterm_json::{Value, json};
use aterm_secrets::{
    client::{self, Paths},
    commons::roster::Role,
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

/// One Mac of `lab` writes `changes` into the journal's week ring at `t` (Unix seconds), as its
/// flusher's record would: one signed write of the ring's collections.
pub(super) fn state_ring(lab: &Lab, n: usize, changes: Vec<crate::database::Change>, t: u64) {
    use crate::alab::journal::{APPLICATION, ROLE};
    let bootstrap = lab.bootstrap(n);
    let session = crate::table::Session::open(&bootstrap).unwrap();
    let directory = crate::catalog::Directory::new(&lab.paths[n], bootstrap.trust());
    let request = crate::typed::request_id(APPLICATION, ROLE, &changes).unwrap();
    let payload = crate::database::Command::Write { request, changes }
        .encode()
        .unwrap();
    directory
        .append_journal(&session, t, t, &payload)
        .unwrap()
        .unwrap();
}

pub(crate) use super::fixture::declare;

/// The Commons machine uuid (32 hex) Mac `n` of `lab` writes its own rows at.
pub(super) fn uuid_of(lab: &Lab, n: usize) -> String {
    let bootstrap = lab.bootstrap(n);
    let session = crate::table::Session::open(&bootstrap).unwrap();
    let actor = session.actor();
    let members = session.members().unwrap();
    let binding = members
        .iter()
        .find(|binding| binding.actor() == actor)
        .unwrap();
    keys::uuid32(binding.machine())
}

/// The table ids of the journal ring's window at `t` (Unix seconds), as `[commons]
/// exclude_tables` names them.
pub(super) fn ring_tables(lab: &Lab, n: usize, t: u64) -> Vec<String> {
    use crate::catalog::ring::JOURNAL;
    let bootstrap = lab.bootstrap(n);
    let session = crate::table::Session::open(&bootstrap).unwrap();
    let directory = crate::catalog::Directory::new(&lab.paths[n], bootstrap.trust());
    directory
        .ring_view(&session, &JOURNAL)
        .window(&JOURNAL, t)
        .map(|epoch| {
            aterm_codec::hex::encode_lower(
                &JOURNAL.scope(bootstrap.trust(), epoch).unwrap().digest(),
            )
        })
        .collect()
}

/// A row of a ring collection at `key`.
pub(super) fn ring_row(
    collection: &crate::typed::Collection,
    key: &str,
    value: Value,
) -> crate::database::Change {
    crate::database::Change {
        collection: collection.name.to_owned(),
        key: key.to_owned(),
        value: Some(value),
    }
}

/// The week reset the stand-in readings name: three days after the pass clock, Unix milliseconds.
pub(super) const RESET: u64 = NOW + 3 * 86_400_000;

/// Made-up addresses: no account of the owner's is named in a test.
const ADA: &str = "ada.fb@alpha.example";
const BEA: &str = "bea.m2@beta.example";

/// The pass clock of every test here.
pub(super) const NOW: u64 = 1_800_000_000_000;

/// The Mac of the lab whose files are read: the second, so the first, the vault's admin, can
/// revoke it.
pub(super) const SEAT: usize = 1;

const PRESERVING: &str = "Commons writer succession is still preserving old records";

/// A stand-in lab whose second Mac holds a registry with three accounts on two addresses, one
/// machine and one link.
pub(super) struct Stand {
    pub(super) lab: Lab,
    /// The stand-in `aterm.toml`; absent until a test writes `[commons]` there.
    pub(super) config: PathBuf,
    /// The registry table's id, as `[commons] exclude_tables` names it.
    table: [u8; 32],
    claude_ada: String,
    pub(super) machine: String,
}

impl Stand {
    pub(super) fn new() -> Self {
        Self::with(&[Role::Writer, Role::Writer])
    }

    /// A stand whose lab has a Mac in each of `roles`; the second is the one whose files are read.
    pub(super) fn with(roles: &[Role]) -> Self {
        Self::seeded(roles, true)
    }

    /// A stand whose registry holds the two addresses and the machine but no account and no
    /// link: no write names a collection that opts into the authority rules, so no read of it
    /// judges (`typed::authority::read`). The model's reading and writing walks measure the
    /// machines file alone, and their windows must not enter the judgment the authority walk's
    /// steps are anchored on.
    pub(super) fn machines_only() -> Self {
        Self::seeded(&[Role::Writer, Role::Writer], false)
    }

    fn seeded(roles: &[Role], accounts: bool) -> Self {
        let lab = Lab::new(roles);
        let config = lab._home.path().join("aterm.toml");
        let registry = Registry::at(lab.paths[SEAT].clone());
        let table = registry.open().unwrap().digest();
        let (ada, bea) = (
            keys::address_pk(ADA).unwrap(),
            keys::address_pk(BEA).unwrap(),
        );
        let claude_ada = keys::account_pk("claude", ADA).unwrap();
        let oai_ada = keys::account_pk("codex", ADA).unwrap();
        let claude_bea = keys::account_pk("claude", BEA).unwrap();
        let machine = keys::machine_pk("m7");
        let link = keys::preference_pk(&machine, &claude_ada).unwrap();
        let account = |pk: &str, provider: &str, realm: &str, login: &str| {
            vec![
                put(&ACCOUNT, pk, "provider", json!(provider)),
                put(&ACCOUNT, pk, "realm", json!(realm)),
                put(&ACCOUNT, pk, "login", json!(login)),
                put(&ACCOUNT, pk, "status", json!("active")),
            ]
        };
        let mut changes = vec![
            put(&ADDRESS, &ada, "addr", json!(ADA)),
            put(&ADDRESS, &bea, "addr", json!(BEA)),
            put(&MACHINE, &machine, "label", json!("m7")),
            put(&MACHINE, &machine, "status", json!("active")),
            // A cell the face must quote as the grid does.
            put(&MACHINE, &machine, "note", json!("the \"desk\" Mac")),
        ];
        if accounts {
            changes.extend([
                put(&PREFERENCE, &link, "machine", json!(machine)),
                put(&PREFERENCE, &link, "account", json!(claude_ada)),
                put(&PREFERENCE, &link, "rank", json!(1)),
                put(&PREFERENCE, &link, "why", json!("owner")),
            ]);
            changes.extend(account(&claude_ada, "anthropic", "claude", &ada));
            changes.extend(account(&oai_ada, "openai", "codex", &ada));
            changes.extend(account(&claude_bea, "anthropic", "claude", &bea));
        }
        registry.write(vec![changes]).unwrap();
        // This Mac's program states one sample of a metric, a minute before the pass clock, and
        // one note: every file has a row.
        Stand::tracker_rows(&lab.paths[SEAT]);
        // This Mac's harness states its login, its reading and an event of the account in the
        // journal's week ring, as its flusher's record would.
        let me = uuid_of(&lab, SEAT);
        state_ring(
            &lab,
            SEAT,
            Stand::ring_rows(&claude_ada, &me, 400, RESET, 1),
            NOW / 1000,
        );
        Self {
            lab,
            config,
            table,
            claude_ada,
            machine,
        }
    }

    /// A Mac's login, usage reading (`w7d` tenths of a percent, at `reset`) and quota event
    /// for `account`, at its uuid `me`, its record's `n` being `n`. The reading says it was read
    /// `n` seconds after the pass clock, by that Mac's clock: a later record's reads later.
    pub(super) fn ring_rows(
        account: &str,
        me: &str,
        w7d: u64,
        reset: u64,
        n: u64,
    ) -> Vec<crate::database::Change> {
        use crate::alab::journal::{
            EV, LOGIN, USAGE,
            event::{Event, Kind, Pk, Source, Window, Word},
        };
        let key = format!("{account}.{me}");
        let event = Event {
            t: NOW / 1000,
            kind: Kind::Quota,
            account: Pk::new(account),
            cli: Word::new("claude").unwrap(),
            sub: None,
            window: Some(Window::Week),
            pct_x10: Some(u16::try_from(w7d).unwrap()),
            reset: Some(reset / 1000),
            res: None,
            until: None,
            plan: None,
            src: Source::Statusline,
            why: None,
            to: None,
            tab: None,
            harness: None,
            op: None,
            via: None,
            program: None,
        };
        vec![
            ring_row(
                &LOGIN,
                &key,
                json!({
                    "r": 1, "n": n, "account": account, "cli": "claude", "place": "default",
                    "state": "signed-in", "proof": "status", "src": "profile", "use": "idle",
                    "tabs": 0, "held": "linked", "since": NOW, "at": NOW, "attempt": "none",
                }),
            ),
            ring_row(
                &USAGE,
                &key,
                json!({
                    "r": 1, "n": n, "account": account, "w7d": w7d, "w7d_reset": reset,
                    "wall": "none", "src": "statusline", "read_at": NOW + n * 1000, "at": NOW,
                    "sessions_7d": 1,
                }),
            ),
            ring_row(
                &EV,
                &format!("{me}.{n}"),
                json!({"r": 1, "n": n, "e": [event.to_value()]}),
            ),
        ]
    }

    /// The Mac whose home is `paths` appends one sample of a metric, at `<uuid32>.1` of
    /// `lab/measures`, and one note, at `<uuid32>.1` of `lab/notes`.
    fn tracker_rows(paths: &Paths) {
        use crate::lab::tracker::{MEASURE, NOTE};
        let tracker = att::Handle::at(paths.clone(), &att::TRACKER);
        tracker
            .append_log(&MEASURE, &[metric("4", NOW - 60_000)])
            .unwrap();
        let note = json!({"about": "tk-05", "text": "a log outside the ring", "at": NOW});
        tracker
            .append_log(&NOTE, &[note.as_object().unwrap().clone()])
            .unwrap();
    }

    pub(super) fn paths(&self) -> &Paths {
        &self.lab.paths[SEAT]
    }

    fn root(&self) -> Root {
        super::root(self.paths())
    }

    pub(super) fn file(&self, file: &Type) -> File {
        File::at(&self.root(), &file.file())
    }

    /// One pass over every file, waiting `vault_wait` for a busy vault.
    fn pass_waiting(&self, again: bool, vault_wait: Duration) -> Vec<Outcome> {
        materialize::pass(
            self.paths(),
            &self.config,
            None,
            &Pass {
                read_by: Instant::now() + aterm_spec::wait::MUST_HAPPEN,
                vault_wait,
                again,
                now_ms: NOW,
            },
        )
    }

    pub(super) fn pass(&self, again: bool) -> Vec<Outcome> {
        self.pass_waiting(again, materialize::VAULT_WAIT)
    }

    fn commons(&self, text: &str) {
        aterm_uds::atomic_file::write(&self.config, text.as_bytes(), 0o600).unwrap();
    }
}

/// Every outcome of `outcomes` is `word` (`why`), and each file's status says the same.
#[track_caller]
fn all(stand: &Stand, outcomes: &[Outcome], word: Word, why: Option<Why>) {
    assert_eq!(outcomes.len(), super::FILES.len());
    for outcome in outcomes {
        assert_eq!(
            (outcome.word, outcome.why),
            (word, why),
            "{}: {:?}",
            outcome.name,
            outcome.detail
        );
        assert!(outcome.problem.is_none(), "{:?}", outcome.problem);
    }
    for file in super::FILES {
        let status = stand.file(file).status();
        assert_eq!((status.word, status.why), (word, why), "{}", file.name);
    }
}

fn words(list: &[&str]) -> Vec<String> {
    list.iter().map(|word| (*word).to_owned()).collect()
}

/// One read verb of `aterm file`, which must answer exit 0.
#[track_caller]
fn verb(root: &Root, list: &[&str]) -> String {
    match cli::run(root, &super::types::known(), &words(list)) {
        cli::Run::Answer(answer) => {
            assert_eq!(
                answer.code,
                aterm_file::exit::OK,
                "{list:?}: {}",
                answer.err
            );
            answer.out
        }
        cli::Run::Watch(..) => panic!("{list:?} is no watch"),
    }
}

/// One alias view (`aterm db ai-accounts`, `aterm db computers`) with nothing it may open: no
/// grid, no standard input, an empty secret store.
#[track_caller]
fn alias(paths: &Paths, noun: Noun, list: &[&str]) -> Value {
    let store = crate::typed::secret::Memory::default();
    let mut open = |_: &std::path::Path, _: usize| -> crate::Result<()> {
        panic!("an alias read opens no grid")
    };
    let mut read_stdin = || -> crate::Result<zeroize::Zeroizing<Vec<u8>>> {
        panic!("an alias read takes no standard input")
    };
    let mut io = ViewIo {
        open: &mut open,
        read_stdin: &mut read_stdin,
        store: &store,
        holding: &|| Ok(aterm_secrets::client::Holding::default()),
        now_ms: NOW,
    };
    views::run(paths, noun, &words(list), &mut io).unwrap()
}

/// What every read of the stand's files answers, by every way a reader has: the library read,
/// each `aterm file` read verb, and the `aterm db` aliases. The custody seam does not move
/// across any of them.
#[derive(Debug, PartialEq)]
struct Seen {
    /// Each file's face text as `cat` prints it, the revision in its header.
    cat: Vec<String>,
    /// `get ai/accounts claude-fb-alpha name`.
    name: String,
    /// The rows of `rows ai/links --join account --json`.
    joined: Value,
    /// The alias rows of `aterm db ai-accounts` and `aterm db computers`.
    aliases: Vec<Value>,
}

#[track_caller]
fn reads(stand: &Stand, word: Word, why: Option<Why>) -> Seen {
    let before = seam::custody_opens();
    let root = stand.root();
    let mut cat = Vec::new();
    for file in super::FILES {
        let read = File::at(&root, &file.file()).read();
        assert_eq!(
            (read.status.word, read.status.why),
            (word, why),
            "{}",
            file.name
        );
        assert!(read.status.problem.is_none());
        assert!(!read.rows().is_empty(), "{} reads its rows", file.name);
        let text = verb(&root, &["cat", file.name]);
        assert_eq!(text, read.snapshot.as_ref().unwrap().face());
        cat.push(text);
        let status: Value =
            aterm_json::from_str(&verb(&root, &["status", file.name, "--json"])).unwrap();
        assert_eq!(status["state"].as_str(), Some(word.as_str()), "{status}");
        assert_eq!(status["why"].as_str(), why.map(Why::as_str), "{status}");
        verb(&root, &["path", file.name]);
        verb(&root, &["history", file.name, "--json"]);
    }
    let listed = verb(&root, &["ls"]);
    for file in super::FILES {
        assert!(listed.contains(file.name), "{listed}");
    }
    let name = verb(&root, &["get", "ai/accounts", "claude-fb-alpha", "name"]);
    let joined: Value = aterm_json::from_str(&verb(
        &root,
        &["rows", "ai/links", "--join", "account", "--json"],
    ))
    .unwrap();
    let joined = joined["rows"].clone();
    let accounts = alias(stand.paths(), Noun::AiAccounts, &[]);
    let computers = alias(stand.paths(), Noun::Computers, &[]);
    for (answer, table) in [(&accounts, "ai-accounts"), (&computers, "computers")] {
        assert_eq!(answer["state"].as_str(), Some(word.as_str()), "{answer}");
        assert_eq!(answer["why"].as_str(), why.map(Why::as_str), "{answer}");
        assert!(!answer[table].as_array().unwrap().is_empty(), "{answer}");
    }
    assert_eq!(
        seam::custody_opens(),
        before,
        "a read of a shared file opened custody"
    );
    Seen {
        cat,
        name,
        joined,
        aliases: vec![
            accounts["ai-accounts"].clone(),
            computers["computers"].clone(),
        ],
    }
}

/// SF-02's exit test: a read of a shared file answers its rows from the snapshot in every state
/// this Mac's custody refuses in, and never opens custody.
///
/// One stand-in store is materialized once (`live`), then taken through each refusal m2 and the
/// lab have met: a writer succession whose import never completes (the state that answered
/// "Commons writer succession is still preserving old records" to `aterm db ai-accounts`), a
/// vault another program holds, `[commons]` excluding the table and then turning sharing off,
/// that succession's completion invalidated, and this Mac revoked from the vault. In
/// each, the strict path refuses or the pass records its word; the snapshot's revision and rows
/// stay what the live pass wrote; and the library read, every `aterm file` read verb and the
/// `aterm db` aliases answer those rows with the custody seam (every writer-generation
/// selection and every vault observation of this crate) at zero across the reads.
///
/// NEGATIVE CONTROLS, each observed: with `views::run` opening the registry before it reads the
/// snapshot, the alias read moves the seam: `` left: 7 right: 4 ``; with `File::record` removing
/// the snapshot on a refusal, the busy pass reads `` left: (Never, Some(Busy)) ``; with a fenced
/// pass recorded `live`, `` left: (Live, Some(Succession)) ``.
#[test]
fn a_read_answers_from_the_snapshot_while_custody_refuses_every_way() {
    let stand = Stand::new();
    let paths = stand.paths();
    // A read that must refuse by itself is waited for as long as a hang detector waits; only
    // the read beside a held lock is expected to run out of time.
    let must = || Instant::now() + aterm_spec::wait::MUST_HAPPEN;

    // Before any pass: `never`, no rows, no error, and no custody.
    let quiet = seam::custody_opens();
    for file in super::FILES {
        let read = stand.file(file).read();
        assert_eq!((read.status.word, read.rows().len()), (Word::Never, 0));
    }
    let nothing = alias(paths, Noun::AiAccounts, &[]);
    assert_eq!(nothing["state"].as_str(), Some("never"), "{nothing}");
    assert_eq!(nothing["ai-accounts"], json!([]));
    assert_eq!(seam::custody_opens(), quiet);

    // Custody open: the pass projects every file, and the seam counts its open (it is live).
    let first = stand.pass(false);
    all(&stand, &first, Word::Live, None);
    for outcome in &first {
        assert_eq!(
            outcome.recorded,
            Some(Recorded::Recorded { replaced: true })
        );
    }
    assert!(seam::custody_opens() > quiet, "the pass opened custody");
    let live = reads(&stand, Word::Live, None);
    assert_eq!(live.name.trim_end(), format!("claude {ADA}"));
    assert_eq!(
        live.joined[0]["account.cells"]["slug"].as_str(),
        Some("claude-fb-alpha"),
        "{}",
        live.joined
    );
    let revs = |stand: &Stand| -> Vec<u64> {
        super::FILES
            .iter()
            .map(|file| stand.file(file).status().rev())
            .collect()
    };
    assert_eq!(revs(&stand), vec![1; super::FILES.len()]);
    // What each refusal must leave: the same rows at the same revision, whatever the word.
    let untouched = |seen: Seen| {
        assert_eq!(seen, live, "the snapshot is untouched");
        assert_eq!(revs(&stand), vec![1; super::FILES.len()]);
    };

    // 1. FENCED-PENDING. A writer succession reserved a successor whose import never completes.
    generation::file_seams::fence_pending(paths, &stand.lab.bootstrap(SEAT));
    assert_eq!(
        generation::completed_generation_until(
            paths,
            Instant::now() + aterm_spec::wait::MUST_HAPPEN
        )
        .err()
        .unwrap()
        .to_string(),
        PRESERVING,
        "the strict selection refuses, as it did on m2"
    );
    // No pass has run under the fence: the reads still answer.
    untouched(reads(&stand, Word::Live, None));
    // The pass reads the records the fenced home holds and says `held`, never `live`.
    let fenced = stand.pass(true);
    all(&stand, &fenced, Word::Held, Some(Why::Succession));
    untouched(reads(&stand, Word::Held, Some(Why::Succession)));

    // 2. A BUSY VAULT. Another program holds the vault's state lock past the pass's wait.
    let held = client::test_hold_state_lock(paths).unwrap();
    assert!(
        Registry::at(paths.clone())
            .view_until(Instant::now() + Duration::from_millis(300))
            .is_err(),
        "a registry read refuses while the vault is held"
    );
    let busy = stand.pass_waiting(true, Duration::from_millis(50));
    all(&stand, &busy, Word::Held, Some(Why::Busy));
    untouched(reads(&stand, Word::Held, Some(Why::Busy)));
    drop(held);

    // 3. AN EXCLUDED TABLE, then sharing turned off: `off`, with the last rows still read.
    // The registry's table, `lab/tracker`'s and the journal ring's window: every table a file
    // is read from.
    let tracker = att::TRACKER
        .descriptor(stand.lab.bootstrap(SEAT).trust())
        .unwrap()
        .scope
        .digest();
    let mut tables = vec![
        aterm_codec::hex::encode_lower(&stand.table),
        aterm_codec::hex::encode_lower(&tracker),
    ];
    tables.extend(ring_tables(&stand.lab, SEAT, NOW / 1000));
    let quoted: Vec<String> = tables.iter().map(|table| format!("\"{table}\"")).collect();
    stand.commons(&format!(
        "[commons]\nexclude_tables = [{}]\n",
        quoted.join(", ")
    ));
    let excluded = stand.pass(true);
    all(&stand, &excluded, Word::Off, Some(Why::Policy));
    untouched(reads(&stand, Word::Off, Some(Why::Policy)));
    stand.commons("[commons]\nenabled = false\n");
    let before_off = seam::custody_opens();
    let off = stand.pass(true);
    all(&stand, &off, Word::Off, Some(Why::Policy));
    assert_eq!(
        seam::custody_opens(),
        before_off,
        "sharing off: the pass itself opens no custody"
    );
    untouched(reads(&stand, Word::Off, Some(Why::Policy)));
    std::fs::remove_file(&stand.config).unwrap();
    all(&stand, &stand.pass(true), Word::Held, Some(Why::Succession));

    // 4. THE COMPLETION INVALIDATED. A later source receipt: no home of the lineage is known to
    // hold the newest records, so every strict reader refuses. The materializer alone opens the
    // original home (slice SF-03) and says `held`; nothing arrived there, so the rows a reader
    // gets are the ones the live pass wrote.
    generation::file_seams::invalidate_completion(paths);
    let unsettled = Registry::at(paths.clone())
        .view_until(must())
        .expect_err("no home is known to hold the newest records");
    assert_eq!(unsettled.to_string(), PRESERVING);
    let views = materialize::PROJECTIONS.with(std::cell::Cell::get);
    let invalidated = stand.pass(true);
    assert!(
        materialize::PROJECTIONS.with(std::cell::Cell::get) > views,
        "the unsettled home's records are projected"
    );
    all(&stand, &invalidated, Word::Held, Some(Why::Succession));
    for outcome in &invalidated {
        assert_eq!(
            outcome.recorded,
            Some(Recorded::Settled),
            "they are the rows and the word the files here hold: {}",
            outcome.name
        );
    }
    untouched(reads(&stand, Word::Held, Some(Why::Succession)));

    // 5. NO MEMBERSHIP. The vault's admin revokes this Mac, and its next sync says so. The
    // strict read already refuses here for the unsettled lineage of step 4, so its refusal of
    // a revoked Mac is asserted on a stand no succession fenced (the next test).
    revoke(&stand);
    let vault = aterm_secrets::commons::observe_until_settled(paths, must()).unwrap();
    assert!(
        matches!(
            vault,
            aterm_secrets::commons::Observation::Unavailable(reason)
                if reason.code() == "vault-revoked"
        ),
        "the vault says this Mac is revoked"
    );
    let revoked = stand.pass(true);
    all(&stand, &revoked, Word::Held, Some(Why::Membership));
    for outcome in &revoked {
        assert_eq!(
            outcome.recorded,
            Some(Recorded::Recorded { replaced: false })
        );
    }
    untouched(reads(&stand, Word::Held, Some(Why::Membership)));
}

/// SF-42's exit test: every file this build compiles says what its columns are in its own
/// snapshot, and `aterm file schema` prints that with this machine's custody refusing.
///
/// After one live pass, each stored column of each snapshot states the type word and the
/// nullability its compiled column has (the registry's files against the generation in use, a
/// ring file's own rows against its collection, a log's members against the event's columns,
/// with no nullability, since an event's schema says none), and the key, a derived column and
/// an unknown one state neither. The unknown one is a member a later build's login row
/// carries, named with a tab in it: `schema` prints that name as its JSON string, so its line
/// is still one line of five cells. Sharing is then turned off, which a pass records as `off`,
/// and `schema <name>` answers the same columns as a table and as JSON, with the custody seam
/// at zero across every read. No file is a view with a stored column, so a view states none.
///
/// NEGATIVE CONTROLS, each observed: with the table pass stating no type for a schema column,
/// `` ai/accounts.provider states no type ``; with `schema --json` printing `false` where the
/// snapshot states no nullability, `` left: Some(false) right: None ``; with every event
/// column stated as `time`, the panic names `ai/events.kind`; with `name_cell` returning the
/// name as it is, that column's line has six cells: `` left: 6 right: 5 ``.
#[test]
fn every_compiled_files_snapshot_states_each_stored_columns_type_and_schema_prints_it_while_custody_refuses()
 {
    use aterm_file::snapshot::Source;
    // A member no build compiles, named with a tab in it.
    const LATER: &str = "later\tmember";
    let stand = Stand::new();
    // A later build's login row at the same key: every member this build compiles, and that
    // one.
    state_ring(
        &stand.lab,
        SEAT,
        vec![ring_row(
            &crate::alab::journal::LOGIN,
            &format!("{}.{}", stand.claude_ada, uuid_of(&stand.lab, SEAT)),
            json!({
                "r": 1, "n": 2, "account": stand.claude_ada, "cli": "claude", "place": "default",
                "state": "signed-in", "proof": "status", "src": "profile", "use": "idle",
                "tabs": 0, "held": "linked", "since": NOW, "at": NOW, "attempt": "none",
                LATER: "said",
            }),
        )],
        NOW / 1000,
    );
    all(&stand, &stand.pass(false), Word::Live, None);
    let compiled = Registry::at(stand.paths().clone()).generation();
    stand.commons("[commons]\nenabled = false\n");
    all(&stand, &stand.pass(true), Word::Off, Some(Why::Policy));

    let quiet = seam::custody_opens();
    let root = stand.root();
    let (mut stored, mut unknown) = (0, 0);
    for file in super::FILES {
        let snapshot = stand.file(file).published().snapshot.expect("a snapshot");
        // What the compiled schema says of each column, where this file projects one: a
        // registry table's against the generation in use, another family's (`lab/tracker`'s
        // logs) against its own, which its one generation compiles.
        let collection = match file.store {
            Store::Table { family, collection }
                if family.table.role == att::REGISTRY.table.role =>
            {
                Some(compiled.collection(collection.name).unwrap())
            }
            Store::Table { collection, .. } | Store::Journal(collection) => Some(collection),
            Store::View(_) => None,
        };
        // Only the journal ring's log states its members as an event's, with no nullability: a
        // table's log states its own columns.
        let says_null = collection.is_some_and(|collection| {
            collection.kind != crate::typed::Kind::Log || matches!(file.store, Store::Table { .. })
        });
        for column in &snapshot.columns {
            let at = format!("{}.{}", file.name, column.name);
            if column.source != Source::Schema {
                assert_eq!((&column.ty, column.null), (&None, None), "{at}");
                unknown += usize::from(column.source == Source::Unknown);
                continue;
            }
            stored += 1;
            let ty = column
                .ty
                .as_ref()
                .unwrap_or_else(|| panic!("{at} states no type"));
            assert_eq!(column.null.is_some(), says_null, "{at}");
            if says_null {
                let compiled = collection
                    .unwrap()
                    .columns
                    .iter()
                    .find(|compiled| compiled.name == column.name)
                    .unwrap_or_else(|| panic!("{at} is no compiled column"));
                assert_eq!(
                    (ty, column.null),
                    (&compiled.ty.word(), Some(compiled.null)),
                    "{at}"
                );
            } else {
                let (_, _, event) = materialize::EVENT_COLUMNS
                    .iter()
                    .find(|(name, _, _)| *name == column.name)
                    .unwrap_or_else(|| panic!("{at} is no event column"));
                assert_eq!(ty, &event.word(), "{at}");
            }
        }

        let answer: Value =
            aterm_json::from_str(&verb(&root, &["schema", file.name, "--json"])).unwrap();
        assert_eq!(answer["state"].as_str(), Some("off"), "{answer}");
        assert_eq!(answer["primary_key"].as_str(), Some("pk"), "{answer}");
        assert_eq!(
            answer["key"].as_str(),
            stand
                .file(file)
                .state()
                .and_then(|state| state.key)
                .as_deref(),
            "the status's projection key stays the status's: {answer}"
        );
        assert_eq!(answer["kind"].as_str(), Some(file.kind()), "{answer}");
        let handles: Vec<&str> = answer["handles"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(handles, file.handles, "{answer}");
        let printed = answer["columns"].as_array().unwrap();
        assert_eq!(printed.len(), snapshot.columns.len(), "{answer}");
        let table = verb(&root, &["schema", file.name]);
        let lines: Vec<Vec<&str>> = table
            .lines()
            .map(|line| line.split('\t').collect())
            .collect();
        assert_eq!(lines[0], ["column", "type", "null", "source", "ref"]);
        assert_eq!(lines.len(), snapshot.columns.len() + 1, "{table}");
        for ((column, json), line) in snapshot.columns.iter().zip(printed).zip(&lines[1..]) {
            assert_eq!(json["name"].as_str(), Some(column.name.as_str()));
            assert_eq!(json["type"], column.ty.clone().unwrap_or(Value::Null));
            assert_eq!(json["null"].as_bool(), column.null);
            assert_eq!(json["ref"].as_str(), column.refers.as_deref());
            assert_eq!(line.len(), 5, "{line:?}");
            if column.name == LATER {
                assert_eq!((line[0], line[3]), ("\"later\\tmember\"", "unknown"));
            } else {
                assert_eq!(line[0], column.name);
            }
            assert_eq!(line[1].is_empty(), column.ty.is_none(), "{line:?}");
            assert_eq!(line[4], column.refers.as_deref().unwrap_or_default());
        }
    }
    assert!(
        stored > 40,
        "the compiled files hold stored columns: {stored}"
    );
    assert_eq!(unknown, 1, "the later build's member is one unknown column");
    // A bare word prints as itself and a parameterised one as its JSON.
    let events = verb(&root, &["schema", "ai/events"]);
    assert!(events.contains("at\ttime\t\tschema\t\n"), "{events}");
    let links = verb(&root, &["schema", "ai/links"]);
    assert!(
        links.contains("account\t{\"ref\":\"account\"}\tno\tschema\tai/accounts\n"),
        "{links}"
    );
    assert_eq!(
        seam::custody_opens(),
        quiet,
        "a schema read of a shared file opened custody"
    );
}

/// A snapshot put back at its revision without the types, as a copy an earlier build wrote
/// would leave it, is projected again by the next unforced pass, which states the types at the
/// same revision. The content digest leaves the types out (`Snapshot::content`), so the skip
/// asks each stored column of the snapshot there for a type: this build's projection states
/// one for each.
///
/// NEGATIVE CONTROL, observed 2026-11-16: with the skip's type check left out, the unforced
/// pass records the untyped copy live, and `the types are stated again` fails with every stored
/// column of `lab/machines` at `ty: None` against the pass's words (`"label"`, `{"set":"label"}`).
#[test]
fn a_snapshot_put_back_without_its_types_is_projected_again_at_its_revision() {
    let stand = Stand::new();
    all(&stand, &stand.pass(false), Word::Live, None);
    let snap = stand.root().snapshot(&super::MACHINES.file());
    let typed = aterm_file::Snapshot::decode(&std::fs::read(&snap).unwrap()).unwrap();
    assert!(
        typed.columns.iter().any(|column| column.ty.is_some()),
        "CONTROL: the pass stated the types"
    );
    let mut untyped = typed.clone();
    for column in &mut untyped.columns {
        (column.ty, column.null) = (None, None);
    }
    std::fs::write(&snap, untyped.encode()).unwrap();
    let read = stand.file(&super::MACHINES).published().snapshot.unwrap();
    assert_eq!(
        (
            read.rev,
            read.columns.iter().all(|column| column.ty.is_none())
        ),
        (typed.rev, true),
        "CONTROL: the untyped copy reads, at its revision"
    );

    all(&stand, &stand.pass(false), Word::Live, None);
    let again = stand.file(&super::MACHINES).published().snapshot.unwrap();
    assert_eq!(again.columns, typed.columns, "the types are stated again");
    assert_eq!(again.rev, typed.rev, "at the same revision");
}

/// The vault's admin revokes the stand's Mac, and its next sync says so.
fn revoke(stand: &Stand) {
    let admin = client::admin_cached(&stand.lab.paths[0]).unwrap();
    client::admin_revoke(&stand.lab.paths[0], &admin, "m2", false).unwrap();
    client::sync(stand.paths(), None).unwrap();
}

/// The nickname this Mac's row shows as the naming read takes it ([`super::here_published`]),
/// a retained snapshot's included.
fn retained(paths: &Paths) -> Option<crate::alab::view::MachineNickname> {
    super::here_published(paths).map(|here| here.nickname())
}

/// A Mac revoked from the vault, its writer lineage settled (no succession ever fenced it): the
/// strict registry read refuses for the revocation, the pass records `held` (`membership`), and
/// the reads answer the rows the live pass wrote.
///
/// The exit test above revokes a Mac whose succession's completion is already invalidated,
/// where the strict read refuses for the lineage first; this is where its refusal of a revoked
/// Mac is seen for what it is.
/// NEGATIVE CONTROL, observed: the same assertion made at the exit test's revoke step fails on
/// `` Commons writer succession is still preserving old records ``.
#[test]
fn a_revoked_macs_strict_read_refuses_for_the_revocation_and_its_snapshot_still_reads() {
    let stand = Stand::new();
    let registry = Registry::at(stand.paths().clone());
    registry
        .write(vec![vec![
            put(&MACHINE, &stand.machine, "nickname", json!("Heron")),
            crate::database::Change {
                collection: crate::alab::registry::MACHINE_FACTS.name.into(),
                key: uuid_of(&stand.lab, SEAT),
                value: Some(json!({"r": 1, "n": 1, "mac": stand.machine, "label": "m7"})),
            },
        ]])
        .unwrap();
    all(&stand, &stand.pass(false), Word::Live, None);
    let live = reads(&stand, Word::Live, None);
    let nickname = super::live_here_nickname(stand.paths()).expect("the live own row");
    assert_eq!(nickname.0, stand.machine);
    assert_eq!(
        nickname.1,
        crate::alab::view::MachineNickname::Named {
            word: "Heron".into(),
            automatic: false,
        }
    );

    // A snapshot whose last status vouches for another revision is retained, never a source
    // for automatic naming. Re-admission of the same rows restores their eligibility.
    let status_path = stand.root().state(&super::MACHINES.file());
    let status_bytes = std::fs::read(&status_path).unwrap();
    let mut status = aterm_file::State::decode(&status_bytes).unwrap();
    status.rev = status.rev.wrapping_add(1);
    aterm_secrets::fsx::write_atomic(&status_path, &status.encode(), 0o600).unwrap();
    assert_eq!(retained(stand.paths()), Some(nickname.1.clone()));
    assert_eq!(super::live_here_nickname(stand.paths()), None);
    aterm_secrets::fsx::write_atomic(&status_path, &status_bytes, 0o600).unwrap();
    assert_eq!(
        super::live_here_nickname(stand.paths()),
        Some(nickname.clone())
    );

    aterm_secrets::fsx::write_atomic(&stand.config, b"[commons]\nenabled = false\n", 0o600)
        .unwrap();
    all(&stand, &stand.pass(true), Word::Off, Some(Why::Policy));
    assert_eq!(retained(stand.paths()), Some(nickname.1.clone()));
    assert_eq!(super::live_here_nickname(stand.paths()), None);
    aterm_secrets::fsx::write_atomic(&stand.config, b"", 0o600).unwrap();
    all(&stand, &stand.pass(true), Word::Live, None);
    assert_eq!(
        super::live_here_nickname(stand.paths()),
        Some(nickname.clone())
    );

    revoke(&stand);
    let refused = Registry::at(stand.paths().clone())
        .view_until(Instant::now() + aterm_spec::wait::MUST_HAPPEN)
        .expect_err("a revoked Mac's registry read refuses");
    assert!(refused.to_string().contains("vault-revoked"), "{refused}");
    let revoked = stand.pass(true);
    all(&stand, &revoked, Word::Held, Some(Why::Membership));
    for outcome in &revoked {
        assert_eq!(
            outcome.recorded,
            Some(Recorded::Recorded { replaced: false })
        );
    }
    assert_eq!(reads(&stand, Word::Held, Some(Why::Membership)), live);
    assert_eq!(retained(stand.paths()), Some(nickname.1));
    assert_eq!(super::live_here_nickname(stand.paths()), None);
}

/// The LabView the shared files hold names each account as the registry's own read does: the
/// pass's `=slug`, `=name`, `=email` and `=login_email` are what `account_names` derives, and
/// either view takes the same words for an account.
/// NEGATIVE CONTROL: with `LabView::of_files` reading each account's name from `=email`
/// (`name: shown(row, "email")`), the files name the account by its bare address: `` left:
/// Some("ada.fb@alpha.example") right: Some("claude ada.fb@alpha.example") ``.
#[test]
fn the_files_lab_view_names_each_account_as_the_registrys_does() {
    use crate::alab::view::LabView;
    use aterm_file::account::Found;
    let stand = Stand::new();
    all(&stand, &stand.pass(false), Word::Live, None);
    let view = Registry::at(stand.paths().clone()).view().unwrap();
    let empty = crate::typed::owned::OwnedRead::default();
    let registry = LabView::read_view(&view, &empty, &empty, None);
    let [accounts, links, machines] =
        [&super::ACCOUNTS, &super::LINKS, &super::MACHINES].map(|file| stand.file(file).read());
    let files = LabView::of_files(&accounts, &links, &machines);
    let claude = stand.claude_ada.as_str();
    assert_eq!(files.name(claude), Some(&*format!("claude {ADA}")));
    assert_eq!(files.slug(claude), Some("claude-fb-alpha"));
    assert_eq!(files.names, registry.names);
    for lab in [&files, &registry] {
        assert_eq!(
            lab.account_named(&format!("claude:{ADA}")),
            Found::One(claude)
        );
        assert_eq!(lab.account_named("claude-fb-alpha"), Found::One(claude));
        assert!(matches!(lab.account_named(ADA), Found::Many(_)));
        assert_eq!(lab.account_named("fb-alpha"), Found::None);
    }
}

/// A face's cells, split at tabs: no cell of these fixtures holds one.
fn grid_of(text: &str) -> Vec<Vec<&str>> {
    text.lines()
        .map(|line| line.split('\t').collect())
        .collect()
}

/// The face is the TSV `aterm db table export` writes for the collection, with the file's
/// computed columns before the key and the revision in the key's header cell: every
/// stored column's header and every stored cell, quoting included, is the grid's own.
/// NEGATIVE CONTROL: with `push_cell` never quoting, the machine's row differs from the export's:
/// `` right: ["m7", …, "the \"desk\" Mac", "", ""] ``.
#[test]
fn the_face_is_the_grid_export_with_computed_columns_and_the_revision() {
    let stand = Stand::new();
    all(&stand, &stand.pass(false), Word::Live, None);
    let registry = Registry::at(stand.paths().clone());
    let compiled = registry.generation();
    let view = registry.view().unwrap();
    for file in super::FILES {
        // A log, a file of the journal ring and a view have no grid export; the exit tests of
        // SF-08 and TK-05 read their faces.
        let Some((_, bound)) = file.edited() else {
            continue;
        };
        let collection = compiled.collection(bound.name).unwrap();
        let export = grid::export(compiled.table, collection, &view.cells, &[]).unwrap();
        let face = std::fs::read_to_string(stand.root().face(&file.file(), "tsv")).unwrap();
        let (theirs, ours) = (grid_of(&export.text), grid_of(&face));
        assert_eq!(theirs.len(), ours.len(), "{}", file.name);
        let computed = file.computed.columns();
        // The grid's columns but its key, then the computed ones, then the key at its revision.
        let stored = theirs[0].len() - 1;
        assert_eq!(theirs[0][stored], "=pk");
        assert_eq!(ours[0].len(), stored + computed.len() + 1, "{}", file.name);
        assert_eq!(ours[0][stored + computed.len()], "=pk@1");
        for (at, name) in computed.iter().enumerate() {
            assert_eq!(ours[0][stored + at], format!("={name}"));
        }
        for (theirs, ours) in theirs.iter().zip(&ours) {
            assert_eq!(theirs[..stored], ours[..stored], "{}", file.name);
        }
        for (theirs, ours) in theirs.iter().zip(&ours).skip(1) {
            assert_eq!(theirs[stored], ours[stored + computed.len()], "the key");
        }
    }
    let machines =
        std::fs::read_to_string(stand.root().face(&super::MACHINES.file(), "tsv")).unwrap();
    assert!(machines.contains("\"the \"\"desk\"\" Mac\""), "{machines}");
    // The computed cells: the account's name and slug, its address, and the machines linked.
    let accounts = stand.file(&super::ACCOUNTS).read();
    let row = accounts.named("claude-fb-alpha");
    assert_eq!(row.len(), 1);
    assert_eq!(row[0].pk, stand.claude_ada);
    assert_eq!(row[0].cell("name"), format!("claude {ADA}"));
    assert_eq!(row[0].cell("email"), ADA);
    assert_eq!(row[0].cell("links"), "m7");
    assert_eq!(
        accounts.named(&format!("oai {ADA}"))[0].cell("slug"),
        "oai-fb-alpha"
    );
    assert_eq!(accounts.named(ADA).len(), 2, "one address, two accounts");
    let links = stand.file(&super::LINKS).read();
    assert_eq!(links.rows()[0].cell("account_slug"), "claude-fb-alpha");
    assert_eq!(links.rows()[0].cell("machine_name"), "m7");
    assert_eq!(links.rows()[0].cell("machine"), stand.machine);
    // A `Ref` column names the file its cells are rows of.
    let snapshot = links.snapshot.as_ref().unwrap();
    assert_eq!(
        snapshot.column("account").unwrap().refers.as_deref(),
        Some("ai/accounts")
    );
    assert_eq!(
        snapshot.column("machine").unwrap().refers.as_deref(),
        Some("lab/machines")
    );
    // Local files only the owner reads.
    use std::os::unix::fs::PermissionsExt as _;
    for path in [
        stand.root().face(&super::ACCOUNTS.file(), "tsv"),
        stand.root().snapshot(&super::ACCOUNTS.file()),
        stand.root().state(&super::ACCOUNTS.file()),
    ] {
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{}", path.display());
    }
}

/// A shared file holds no `Secret` and no `Sealed` column: the second generation's `address`
/// collection ([`crate::alab::registry::SEALED_ADDRESS`]), whose `password` is a `Secret`, cannot
/// be one, and every file this build compiles passed the same check when it was compiled.
/// NEGATIVE CONTROL: with `holds_secret` answering `false` for a `Secret`, the address file
/// checks: `` called `Result::unwrap_err()` on an `Ok` value: () ``.
#[test]
fn a_file_with_a_secret_column_is_refused() {
    let emails = Type {
        name: "lab/emails",
        about: "",
        class: Class::Owner,
        store: Store::Table {
            family: &att::REGISTRY,
            collection: &crate::alab::registry::SEALED_ADDRESS,
        },
        computed: Computed::None,
        handles: &["addr"],
    };
    let refused = emails.check().unwrap_err();
    assert!(refused.contains("Secret"), "{refused}");
    for file in super::FILES {
        assert_eq!(file.check(), Ok(()), "{}", file.name);
    }
    for (name, why) in [
        ("Accounts", "<set>/<leaf>"),
        ("ai/accounts/x", "<set>/<leaf>"),
    ] {
        let named = Type {
            name,
            ..super::ACCOUNTS
        };
        assert!(named.check().unwrap_err().contains(why));
    }
    let handle = Type {
        handles: &["nope"],
        ..super::ACCOUNTS
    };
    assert!(handle.check().unwrap_err().contains("handle"));
}

/// A file's class fits what it is bound to: an owner file projects a cells collection of a
/// catalog table the owner writes; a Mac file an owned collection keyed by its writer's uuid or
/// a log, a file of the journal ring one of the ring's collections; a derived file a view, and
/// only a derived file a view. The publisher holds what a file's class does not take: an owner
/// file no Mac's statement, a Mac file no owner's write, a revert included, and a derived file
/// nothing at all.
/// NEGATIVE CONTROL, observed: with `Type::check`'s owner arm refusing nothing, the owner file
/// over an owned collection checks: `` called `Result::unwrap_err()` on an `Ok` value: () ``;
/// with `Class::refuses` answering `None` for a Mac file, the owner's write to one passes:
/// `` left: None right: Some("a Mac file takes no owner write") ``.
#[test]
fn a_files_class_fits_its_collection_and_takes_only_its_writers_intents() {
    // A Mac file over this build's registry: each Mac's account observations.
    let seen = Type {
        name: "ai/seen",
        about: "",
        class: Class::Mac,
        store: Store::Table {
            family: &att::REGISTRY,
            collection: &ACCOUNT_OBS,
        },
        computed: Computed::None,
        handles: &[],
    };
    let refused = |file: Type, why: &str| {
        let said = file.check().unwrap_err();
        assert!(said.contains(why), "{}: {said}", file.name);
    };
    refused(
        Type {
            class: Class::Owner,
            ..seen
        },
        "an owner file projects a cells collection",
    );
    refused(
        Type {
            class: Class::Mac,
            ..super::ACCOUNTS
        },
        "a Mac file projects an owned collection",
    );
    // A Mac file over an owned collection or a log fits its class, of a catalog table or of the
    // journal ring.
    for file in [
        seen,
        Type {
            store: Store::Journal(&crate::alab::journal::EV),
            ..seen
        },
        super::LOGINS,
    ] {
        assert_eq!(file.check(), Ok(()), "{}", file.name);
    }
    refused(
        Type {
            store: Store::Journal(&ACCOUNT_OBS),
            ..seen
        },
        "one of the ring's collections",
    );
    refused(
        Type {
            class: Class::Owner,
            ..super::LOGINS
        },
        "an owner file projects a cells collection",
    );
    // A view is a derived file's and only a derived file's.
    refused(
        Type {
            class: Class::Derived,
            ..seen
        },
        "a derived file is a view",
    );
    for class in [Class::Owner, Class::Mac] {
        refused(
            Type {
                class,
                ..super::LIMITS
            },
            "a view is a derived file",
        );
    }
    refused(
        Type {
            computed: Computed::Kept,
            ..super::LIMITS
        },
        "declares no computed column",
    );
    assert_eq!(
        super::FILES
            .iter()
            .map(|file| file.class)
            .collect::<Vec<_>>(),
        [
            Class::Owner,
            Class::Owner,
            Class::Owner,
            Class::Mac,
            Class::Mac,
            Class::Mac,
            Class::Derived,
            Class::Mac,
            Class::Mac
        ]
    );
    assert_eq!(
        super::FILES
            .iter()
            .map(|file| file.kind())
            .collect::<Vec<_>>(),
        [
            "table", "table", "table", "table", "table", "log", "view", "log", "log"
        ]
    );

    let intent = |key: &str, revert: Option<String>| aterm_file::Intent {
        id: "0".repeat(32),
        file: seen.file(),
        ms: NOW,
        by: "harness".to_owned(),
        session: None,
        born: false,
        edits: if revert.is_some() {
            Vec::new()
        } else {
            vec![aterm_file::Edit {
                key: key.to_owned(),
                column: "state".to_owned(),
                elem: None,
                value: json!("running"),
                text: "running".to_owned(),
                expected: Vec::new(),
            }]
        },
        revert,
        resolves: None,
    };
    let (mine, write) = (intent("ac-1.@self", None), intent("ac-1", None));
    let revert = intent("", Some("1".repeat(64)));
    assert_eq!(
        Class::Owner.refuses(&mine),
        Some("an owner file takes no Mac statement")
    );
    assert_eq!(Class::Owner.refuses(&write), None);
    assert_eq!(Class::Owner.refuses(&revert), None);
    assert_eq!(
        Class::Mac.refuses(&write),
        Some("a Mac file takes no owner write")
    );
    assert_eq!(
        Class::Mac.refuses(&revert),
        Some("a Mac file takes no owner write")
    );
    assert_eq!(Class::Mac.refuses(&mine), None);
    for intent in [&mine, &write, &revert] {
        assert_eq!(
            Class::Derived.refuses(intent),
            Some("a derived file takes no write: nobody writes a view")
        );
    }
}

/// The status counts the records of a file's collection the replica holds inert: a row a Mac
/// wrote at another Mac's key in an owned collection never projects and is counted, while the
/// owner files beside it hold none and say so.
/// NEGATIVE CONTROL, observed: with `materialize::inert` leaving out the owned read's count,
/// the Mac file's count is 0: `` left: 0 right: 1 ``.
#[test]
fn the_status_counts_the_records_a_file_holds_inert() {
    let stand = Stand::new();
    let registry = Registry::at(stand.paths().clone());
    // An observation row at a uuid that is not its signer's: inert to every reader.
    let foreign = format!("{}.{}", stand.claude_ada, "0".repeat(32));
    registry
        .write(vec![vec![crate::database::Change {
            collection: ACCOUNT_OBS.name.to_owned(),
            key: foreign,
            value: Some(json!({"n": 1, "account": stand.claude_ada, "tabs": 1})),
        }]])
        .unwrap();
    let view = registry.view().unwrap();
    let seen = Type {
        name: "ai/seen",
        about: "",
        class: Class::Mac,
        store: Store::Table {
            family: &att::REGISTRY,
            collection: &ACCOUNT_OBS,
        },
        computed: Computed::None,
        handles: &[],
    };
    assert_eq!(materialize::inert(&seen, &view), 1);
    all(&stand, &stand.pass(false), Word::Live, None);
    for file in super::FILES {
        assert_eq!(materialize::inert(file, &view), 0, "{}", file.name);
        assert_eq!(
            stand.file(file).state().unwrap().inert,
            Some(0),
            "{}",
            file.name
        );
    }
    let ls = verb(&stand.root(), &["ls"]);
    assert!(!ls.contains("inert"), "{ls}");

    // An older build rewrote a status without the count (a refused pass keeps the key it
    // read): at the same key the next pass projects again rather than trust a count nobody
    // recorded.
    // NEGATIVE CONTROL, observed: with `opened` keeping a status that does not say its count,
    // the pass projects nothing and the count stays unknown: `` left: None right: Some(0) ``.
    let accounts = stand.file(&super::ACCOUNTS);
    let path = stand.root().state(&super::ACCOUNTS.file());
    let mut state: Value = aterm_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    if let Value::Object(object) = &mut state {
        assert!(object.remove("inert").is_some());
    }
    std::fs::write(&path, state.to_string()).unwrap();
    assert_eq!(accounts.state().unwrap().inert, None);
    let outcomes = stand.pass(false);
    let outcome = outcomes
        .iter()
        .find(|outcome| outcome.name == super::ACCOUNTS.file())
        .unwrap();
    assert_eq!(outcome.word, Word::Live, "{outcome:?}");
    assert_eq!(accounts.state().unwrap().inert, Some(0));
}

/// A pass projects what arrived, moves the revision only when the content changed, projects
/// nothing again at a key it already holds, and never shows a value the guards refuse.
/// NEGATIVE CONTROL: with `table` writing a cell's stored value in place of the text the grid
/// shows, the note's cell is the stored run, which the `REFUSED` assertion prints.
#[test]
fn a_pass_projects_what_arrived_and_withholds_what_the_guards_refuse() {
    let stand = Stand::new();
    let first = stand.pass(false);
    all(&stand, &first, Word::Live, None);
    // Nothing arrived: nothing is projected again, and nothing is written, the status included.
    for outcome in stand.pass(false) {
        assert_eq!(
            outcome.recorded,
            Some(Recorded::Settled),
            "{}",
            outcome.name
        );
    }
    assert_eq!(stand.file(&super::ACCOUNTS).status().rev(), 1);
    // A projection of the same records keeps the revision, and the files with it.
    let views = materialize::PROJECTIONS.with(std::cell::Cell::get);
    for outcome in stand.pass(true) {
        assert_eq!(
            outcome.recorded,
            Some(Recorded::Settled),
            "{}",
            outcome.name
        );
    }
    assert!(
        materialize::PROJECTIONS.with(std::cell::Cell::get) > views,
        "the pass projected again"
    );
    assert_eq!(stand.file(&super::ACCOUNTS).status().rev(), 1);

    // A record arrives: a status, and a note shaped like a credential, stored as an older or
    // modified build would store it (no guard ran).
    let run = "k9".repeat(24);
    let unchecked = crate::database::Change {
        collection: ACCOUNT.name.to_owned(),
        key: format!("{}.note", stand.claude_ada),
        value: Some(crate::typed::cell::CellValue::owner(1, json!(run)).encode()),
    };
    Registry::at(stand.paths().clone())
        .write(vec![vec![
            put(&ACCOUNT, &stand.claude_ada, "status", json!("paused")),
            unchecked,
        ]])
        .unwrap();
    all(&stand, &stand.pass(false), Word::Live, None);
    let accounts = stand.file(&super::ACCOUNTS);
    let read = accounts.read();
    assert_eq!(read.status.rev(), 2);
    let row = &read.named("claude-fb-alpha")[0];
    assert_eq!(row.cell("status"), "paused");
    assert_eq!(row.values.get("status"), Some(&json!("paused")));
    assert!(
        row.cell("note").contains(grid::REFUSED),
        "{}",
        row.cell("note")
    );
    assert_eq!(row.values.get("note"), None);
    assert_eq!(read.status.state.as_ref().unwrap().withheld, 1);
    for path in [
        stand.root().snapshot(&super::ACCOUNTS.file()),
        stand.root().face(&super::ACCOUNTS.file(), "tsv"),
        stand.root().history(&super::ACCOUNTS.file()),
    ] {
        let bytes = std::fs::read_to_string(&path).unwrap();
        assert!(!bytes.contains(&run), "{}", path.display());
    }
    // The links and machines held the same content: their revision did not move.
    assert_eq!(stand.file(&super::LINKS).status().rev(), 1);
    assert_eq!(stand.file(&super::MACHINES).status().rev(), 1);
    // History names the operations and the cells they wrote, never a value.
    let history = accounts.history();
    assert_eq!(history.len(), 2, "{history:?}");
    let wrote = json!([
        format!("{}.status", stand.claude_ada),
        format!("{}.note", stand.claude_ada)
    ]);
    assert!(
        history.iter().any(|entry| entry["keys"] == wrote),
        "{history:?}"
    );
    let stamps: Vec<u64> = history
        .iter()
        .map(|entry| entry["ms"].as_u64().unwrap())
        .collect();
    assert!(stamps.is_sorted(), "oldest first: {stamps:?}");
}

/// `lab/machines` says beside each machine's label whether an automatic write or an import set it
/// rather than an owner (`=label_auto`) and the label the column's claim rule gives the machine
/// (`=label_claim`): its own where it holds it, a clash label where another row won it (tracker
/// package FL-170).
#[test]
fn the_machines_table_says_who_wrote_each_label_and_who_holds_it() {
    let stand = Stand::new();
    all(&stand, &stand.pass(false), Word::Live, None);
    let row = |pk: &str| {
        stand
            .file(&super::MACHINES)
            .read()
            .rows()
            .iter()
            .find(|row| row.pk == pk)
            .cloned()
            .expect("the row")
    };
    let first = row(&stand.machine);
    assert_eq!(
        (
            first.cell("label"),
            first.cell("label_auto"),
            first.cell("label_claim")
        ),
        ("m7", "", "m7"),
        "an owner's label, which the machine holds"
    );
    let automatic = crate::database::Change {
        collection: MACHINE.name.to_owned(),
        key: format!("{}.label", stand.machine),
        value: Some(crate::typed::cell::CellValue::automatic(1, json!("m7")).encode()),
    };
    let registry = Registry::at(stand.paths().clone());
    registry.write(vec![vec![automatic]]).unwrap();
    // A second machine wants m7 after the first holds it.
    let rival = keys::machine_pk("m8");
    registry
        .write(vec![vec![
            put(&MACHINE, &rival, "label", json!("m7")),
            put(&MACHINE, &rival, "status", json!("active")),
        ]])
        .unwrap();
    all(&stand, &stand.pass(false), Word::Live, None);
    let first = row(&stand.machine);
    assert_eq!(
        (first.cell("label_auto"), first.cell("label_claim")),
        ("yes", "m7"),
        "an automatic write now, and the machine that held m7 first keeps it"
    );
    let rival = row(&rival);
    assert_eq!(rival.cell("label"), "m7");
    assert_eq!(rival.cell("label_auto"), "");
    assert!(
        rival.cell("label_claim").starts_with("m7-"),
        "the later row goes by a clash label: {:?}",
        rival.cell("label_claim")
    );
    assert_eq!(
        crate::alab::view::MachineLabel::of_file_row(&rival),
        crate::alab::view::MachineLabel::Named {
            label: "m7".to_owned(),
            automatic: false,
            claim: Some(rival.cell("label_claim").to_owned()),
        }
    );
    // An import's label is no owner's either.
    let imported = keys::machine_pk("m9");
    registry
        .write(vec![vec![
            crate::database::Change {
                collection: MACHINE.name.to_owned(),
                key: format!("{imported}.label"),
                value: Some(
                    crate::typed::cell::CellValue::imported(
                        1,
                        json!("m11"),
                        &crate::typed::keys::source_pk(b"a sheet"),
                    )
                    .encode(),
                ),
            },
            put(&MACHINE, &imported, "status", json!("active")),
        ]])
        .unwrap();
    all(&stand, &stand.pass(false), Word::Live, None);
    assert_eq!(
        crate::alab::view::MachineLabel::of_file_row(&row(&imported)),
        crate::alab::view::MachineLabel::Named {
            label: "m11".to_owned(),
            automatic: true,
            claim: Some("m11".to_owned()),
        }
    );
}

/// A computed cell is held to the guards its source cell is: a machine's label stored unchecked
/// by an older or modified build, shaped like a credential, shows the grid's refusal text in
/// `lab/machines` and never reaches the cells computed from it (`ai/accounts` `=links`,
/// `ai/links` `=machine_name`), in the snapshot's shown text, its typed values or the face; and
/// each cell left out is counted.
/// NEGATIVE CONTROL: either guard alone holds the run back. With `machine_labels` taking the
/// stored label as it is and `table` writing computed text unchecked, `=links` is the run:
/// `` left: "k9k9k9…" right: "(refused value: not shown)" ``.
#[test]
fn a_computed_cell_never_carries_a_value_the_guards_refuse() {
    let stand = Stand::new();
    all(&stand, &stand.pass(false), Word::Live, None);
    assert_eq!(
        stand.file(&super::ACCOUNTS).read().named("claude-fb-alpha")[0].cell("links"),
        "m7"
    );
    let run = "k9".repeat(24);
    let unchecked = crate::database::Change {
        collection: MACHINE.name.to_owned(),
        key: format!("{}.label", stand.machine),
        value: Some(crate::typed::cell::CellValue::owner(1, json!(run)).encode()),
    };
    Registry::at(stand.paths().clone())
        .write(vec![vec![unchecked]])
        .unwrap();
    all(&stand, &stand.pass(false), Word::Live, None);

    let machines = stand.file(&super::MACHINES).read();
    assert!(machines.rows()[0].cell("label").contains(grid::REFUSED));
    let accounts = stand.file(&super::ACCOUNTS).read();
    let row = &accounts.named("claude-fb-alpha")[0];
    assert_eq!(row.cell("links"), grid::REFUSED);
    assert_eq!(row.values.get("links"), None);
    assert_eq!(accounts.status.state.as_ref().unwrap().withheld, 1);
    let links = stand.file(&super::LINKS).read();
    assert_eq!(links.rows()[0].cell("machine_name"), grid::REFUSED);
    assert_eq!(links.rows()[0].values.get("machine_name"), None);
    assert_eq!(
        links.rows()[0].cell("machine"),
        stand.machine,
        "the key stays"
    );
    assert_eq!(links.status.state.as_ref().unwrap().withheld, 1);
    for file in super::FILES {
        for path in [
            stand.root().snapshot(&file.file()),
            stand.root().face(&file.file(), "tsv"),
            stand.root().history(&file.file()),
        ] {
            let bytes = std::fs::read_to_string(&path).unwrap();
            assert!(!bytes.contains(&run), "{}", path.display());
        }
    }
}

/// A fence that falls after a pass read its home's standing and before it opened custody is
/// recorded by that pass: the generation check a session opens under admits a home whose own
/// succession is reserved and unfinished, so the pass reads the standing again once its tables
/// are read, and a home fenced by then is `held`, never `live`.
/// NEGATIVE CONTROL: with `with_custody` trusting its first look at the standing, the pass
/// records the fenced home's rows as `live`:
/// `` left: (Live, None) right: (Held, Some(Succession)) ``.
#[test]
fn a_fence_that_falls_while_a_pass_opens_custody_is_recorded_by_that_pass() {
    let stand = Stand::new();
    let paths = stand.paths().clone();
    let bootstrap = stand.lab.bootstrap(SEAT);
    seam::after_standing(move || generation::file_seams::fence_pending(&paths, &bootstrap));
    let fenced = stand.pass(false);
    all(&stand, &fenced, Word::Held, Some(Why::Succession));
    for outcome in &fenced {
        assert_eq!(
            outcome.recorded,
            Some(Recorded::Recorded { replaced: true }),
            "the fenced home's records are projected"
        );
    }
    assert_eq!(stand.file(&super::ACCOUNTS).read().rows().len(), 3);
}

fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// Files left by another replica (this Mac was in another lab, or set its vault aside and
/// enrolled again as a new Mac) are replaced by the first pass over the replica it has now: its
/// read is not behind theirs, whatever records they were projected from, and the rows a reader
/// gets under `live` are this replica's.
/// NEGATIVE CONTROL: with the frontier naming no replica, the other lab's four accounts stay,
/// read after a pass that opened this lab's custody: `` left: Some(Kept(Behind)) ``.
#[test]
fn files_left_by_another_replica_are_replaced_by_this_ones_read() {
    // Another lab, whose registry holds one account more.
    let other = Stand::new();
    let extra = keys::account_pk("grok", BEA).unwrap();
    Registry::at(other.paths().clone())
        .write(vec![vec![
            put(&ACCOUNT, &extra, "provider", json!("xai")),
            put(&ACCOUNT, &extra, "realm", json!("grok")),
            put(
                &ACCOUNT,
                &extra,
                "login",
                json!(keys::address_pk(BEA).unwrap()),
            ),
            put(&ACCOUNT, &extra, "status", json!("active")),
        ]])
        .unwrap();
    all(&other, &other.pass(false), Word::Live, None);
    assert_eq!(other.file(&super::ACCOUNTS).read().rows().len(), 4);

    let stand = Stand::new();
    copy_tree(other.root().path(), stand.root().path());
    assert_eq!(stand.file(&super::ACCOUNTS).read().rows().len(), 4);

    // The accounts are replaced, and so are the journal ring's files and `lab/tracker`'s logs,
    // whose rows each lab's Mac states at its own machine's key; the links and machines, and
    // the limits folded from the one reading of the one account, the same rows at the same
    // revision in both labs, keep their snapshot files, now vouched for by this replica's
    // frontier.
    let replaced = [
        &super::ACCOUNTS,
        &super::LOGINS,
        &super::USAGE,
        &super::EVENTS,
        &super::MEASURES,
        &super::NOTES,
    ]
    .map(Type::file);
    let first = stand.pass(false);
    for outcome in &first {
        assert_eq!(
            outcome.recorded,
            Some(Recorded::Recorded {
                replaced: replaced.contains(&outcome.name)
            }),
            "{}",
            outcome.name
        );
        assert_ne!(
            aterm_file::File::at(&stand.root(), &outcome.name).frontier(),
            aterm_file::File::at(&other.root(), &outcome.name).frontier(),
            "{}",
            outcome.name
        );
    }
    all(&stand, &first, Word::Live, None);
    let read = stand.file(&super::ACCOUNTS).read();
    assert_eq!(read.rows().len(), 3, "this lab's accounts");
    assert!(read.rows().iter().all(|row| row.pk != extra));
    assert_eq!(read.status.rev(), 2);
}

/// A pass keeps a snapshot it would not take without projecting it (the kept skip of
/// `materialize::opened`): a read behind the snapshot here makes no table view and records
/// `Kept(Behind)`; a projection too large to take at a key is not made again at that key (once
/// the status records `held` for it, a pass there writes nothing and still says why), and a
/// pass whose key moved projects again, while one whose key stands keeps it still. The files
/// ahead are another copy of the lab's, projected from a registry record this Mac's replica
/// lacks: the registry's files, and `ai/limits`, whose frontier is the registry's read it names
/// accounts by, are behind them, while the journal ring's files and `lab/tracker`'s logs, whose
/// window and table hold the same records in both copies, are not, and their key stands. The
/// projection too large is the status `aterm_file::File::record` leaves for one: the read's key
/// as the one too large ([`aterm_file::State::kept_key`]), and the snapshot kept at an earlier
/// read's key; a registry record moves the keys of the registry's files and the view's, and
/// none of the journal ring's files' or the tracker's logs'.
///
/// NEGATIVE CONTROLS (2026-11-14, run again 2026-11-15 once main's journal ring files joined
/// the pass): with `opened` judging no kept reason before it projects (`let kept = None;`), the
/// pass behind the snapshot made the registry's view: `a read behind the snapshot makes no
/// view` failed, `left: 3 right: 2`; with the skip passing over a key the status records too
/// large (`false && state.kept == Some(Kept::TooLarge) …`), the pass at that key projected
/// again, making the registry's view: `a projection too large at this key is not made again`
/// failed, `left: 4 right: 3` (2026-11-15, the status as `record` leaves it; while the test's
/// status recorded the snapshot at that same key, the pass found the snapshot unchanged, made no
/// view, and failed only on the word it recorded, `Recorded { replaced: false }`); and
/// (2026-11-15) with the view's frontier the ring's again, as the merge first had it, the pass
/// behind the snapshot projected `ai/limits` from the older registry, making the registry's view
/// to fold it: `a read behind the snapshot makes no view` failed, `left: 3 right: 2`.
#[test]
fn a_kept_snapshot_is_not_projected_again_until_its_key_moves() {
    use crate::alab::upkeep::tests::machine_row;
    use aterm_file::Kept;
    let views = || materialize::PROJECTIONS.with(std::cell::Cell::get);
    let kept = |outcomes: &[Outcome], why: Kept| {
        assert_eq!(outcomes.len(), super::FILES.len());
        for outcome in outcomes {
            assert_eq!(
                outcome.recorded,
                Some(Recorded::Kept(why)),
                "{}",
                outcome.name
            );
        }
    };

    // The files a registry record leaves where they are: the journal ring's, and the logs of
    // `lab/tracker`, another table family, whose read no record of the registry moves.
    let apart = |file: &Type| {
        matches!(file.store, Store::Journal(_))
            || file
                .table()
                .is_some_and(|(family, _)| family.table.role != att::REGISTRY.table.role)
    };

    let stand = Stand::new();
    all(&stand, &stand.pass(false), Word::Live, None);
    let fork = stand.lab.fork();
    Registry::at(fork.paths[SEAT].clone())
        .write(vec![machine_row("m8")])
        .unwrap();
    let ahead = materialize::pass(
        &fork.paths[SEAT],
        &fork._home.path().join("aterm.toml"),
        None,
        &Pass {
            read_by: Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            vault_wait: materialize::VAULT_WAIT,
            again: false,
            now_ms: NOW,
        },
    );
    every(&ahead, Word::Live, None);
    copy_tree(super::root(&fork.paths[SEAT]).path(), stand.root().path());
    let before = views();
    let behind = stand.pass(false);
    assert_eq!(views(), before, "a read behind the snapshot makes no view");
    assert_eq!(behind.len(), super::FILES.len());
    for (file, outcome) in super::FILES.iter().zip(&behind) {
        let expected = if apart(file) {
            Recorded::Settled
        } else {
            Recorded::Kept(Kept::Behind)
        };
        assert_eq!(outcome.recorded, Some(expected), "{}", outcome.name);
    }

    let stand = Stand::new();
    all(&stand, &stand.pass(false), Word::Live, None);
    // The status a projection too large to take leaves (`aterm_file::File::record`): the key of
    // this read as the one too large, and the snapshot kept at the key of an earlier read.
    for file in super::FILES {
        let mut state = stand.file(file).state().unwrap();
        state.kept = Some(Kept::TooLarge);
        state.kept_key.clone_from(&state.key);
        state.key = Some(aterm_codec::hex::encode_lower(&[0_u8; 32]));
        assert_ne!(state.key, state.kept_key, "{}", file.name);
        aterm_uds::atomic_file::write(&stand.root().state(&file.file()), &state.encode(), 0o600)
            .unwrap();
    }
    // The pass keeps the snapshot for the reason the status already records: it changes nothing
    // and writes nothing, and says why the snapshot is kept all the same.
    let settled = |outcomes: &[Outcome], why: Kept| {
        assert_eq!(outcomes.len(), super::FILES.len());
        for outcome in outcomes {
            assert_eq!(
                outcome.recorded,
                Some(Recorded::Settled),
                "{}",
                outcome.name
            );
            let state = aterm_file::File::at(&stand.root(), &outcome.name).state();
            assert_eq!(state.unwrap().kept, Some(why), "{}", outcome.name);
            assert!(outcome.detail.is_some(), "{}", outcome.name);
        }
    };
    // The first pass at the key records `held` for the reason the status gives (the status here
    // says `live` beside it), and the pass after changes nothing.
    let before = views();
    let outcomes = stand.pass(false);
    assert_eq!(
        views(),
        before,
        "a projection too large at this key is not made again"
    );
    assert_eq!(outcomes.len(), super::FILES.len());
    kept(&outcomes, Kept::TooLarge);
    settled(&stand.pass(false), Kept::TooLarge);
    assert_eq!(views(), before, "nor by the pass after");
    Registry::at(stand.paths().clone())
        .write(vec![machine_row("m8")])
        .unwrap();
    let outcomes = stand.pass(false);
    assert_eq!(outcomes.len(), super::FILES.len());
    for (file, outcome) in super::FILES.iter().zip(&outcomes) {
        if apart(file) {
            // Its key stands: the snapshot is kept still, and the pass writes nothing.
            assert_eq!(
                (outcome.word, outcome.recorded),
                (Word::Held, Some(Recorded::Settled)),
                "{}",
                outcome.name
            );
        } else {
            assert_eq!(
                (outcome.word, outcome.why),
                (Word::Live, None),
                "{}: {:?}",
                outcome.name,
                outcome.detail
            );
        }
    }
    assert!(views() > before, "a pass whose key moved projects again");
}

/// A pass that read live custody before a fence records nothing once the fenced pass has
/// recorded: `held` is never turned back into `live` by an older look.
/// NEGATIVE CONTROL: with `File::record` ignoring `look`, the overtaken pass records `live`:
/// `` left: Some(Recorded { replaced: true }) ``.
#[test]
fn a_pass_another_overtook_records_nothing() {
    let stand = Stand::new();
    all(&stand, &stand.pass(false), Word::Live, None);
    let paths = stand.paths().clone();
    let config = stand.config.clone();
    let bootstrap = stand.lab.bootstrap(SEAT);
    // Between the first pass's read and its record: the fence falls, and a second pass records.
    seam::before_record(move || {
        generation::file_seams::fence_pending(&paths, &bootstrap);
        let inner = materialize::pass(
            &paths,
            &config,
            None,
            &Pass {
                read_by: Instant::now() + aterm_spec::wait::MUST_HAPPEN,
                vault_wait: materialize::VAULT_WAIT,
                again: true,
                now_ms: NOW,
            },
        );
        for outcome in inner {
            assert_eq!(
                (outcome.word, outcome.why),
                (Word::Held, Some(Why::Succession))
            );
        }
    });
    let overtaken = stand.pass(true);
    for outcome in &overtaken {
        assert_eq!(outcome.recorded, Some(Recorded::Raced), "{}", outcome.name);
    }
    all(&stand, &overtaken, Word::Held, Some(Why::Succession));
}

/// `aterm file refresh` runs one pass and says what each file's state is; it names one file,
/// refuses a name this build has no file for, and is never an error for a state word.
#[test]
fn refresh_runs_a_pass_and_reports_each_file() {
    let stand = Stand::new();
    let pass = Pass {
        read_by: Instant::now() + aterm_spec::wait::MUST_HAPPEN,
        vault_wait: materialize::VAULT_WAIT,
        again: true,
        now_ms: NOW,
    };
    let refresh =
        |list: &[&str]| super::command::refresh(stand.paths(), &stand.config, &words(list), &pass);
    let (text, code) = refresh(&[]).unwrap();
    assert_eq!(code, aterm_file::exit::OK);
    assert_eq!(
        text,
        "ai/accounts: live · snapshot written\n\
         ai/links: live · snapshot written\n\
         lab/machines: live · snapshot written\n\
         ai/logins: live · snapshot written\n\
         ai/usage: live · snapshot written\n\
         ai/events: live · snapshot written\n\
         ai/limits: live · snapshot written\n\
         lab/measures: live · snapshot written\n\
         lab/notes: live · snapshot written\n"
    );
    // Again with nothing changed: the forced pass projects, finds what the files say, and
    // writes nothing.
    let (text, code) = refresh(&[]).unwrap();
    assert_eq!(code, aterm_file::exit::OK);
    assert_eq!(
        text,
        "ai/accounts: live · nothing changed\n\
         ai/links: live · nothing changed\n\
         lab/machines: live · nothing changed\n\
         ai/logins: live · nothing changed\n\
         ai/usage: live · nothing changed\n\
         ai/events: live · nothing changed\n\
         ai/limits: live · nothing changed\n\
         lab/measures: live · nothing changed\n\
         lab/notes: live · nothing changed\n"
    );
    stand.commons("[commons]\nenabled = false\n");
    let (text, code) = refresh(&["ai/links", "--json"]).unwrap();
    assert_eq!(code, aterm_file::exit::OK, "a state word is no error");
    let answer: Value = aterm_json::from_str(&text).unwrap();
    assert_eq!(answer["files"].as_array().unwrap().len(), 1);
    assert_eq!(answer["files"][0]["file"].as_str(), Some("ai/links"));
    assert_eq!(answer["files"][0]["state"].as_str(), Some("off"));
    assert_eq!(answer["files"][0]["why"].as_str(), Some("policy"));
    assert_eq!(answer["files"][0]["recorded"].as_str(), Some("kept"));
    // The other files were not passed over: they still say `live`.
    assert_eq!(stand.file(&super::ACCOUNTS).status().word, Word::Live);
    assert!(
        refresh(&["lab/emails"])
            .unwrap_err()
            .contains("no such shared file")
    );
    assert!(refresh(&["nope"]).is_err());
    assert!(refresh(&["--frob"]).is_err());
}

/// A Mac with no vault at all: every file is `never` (`membership`), a read answers no rows,
/// and nothing is an error.
#[test]
fn a_mac_with_no_vault_reads_never() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&home.path().join("m9"));
    let outcomes = materialize::pass(
        &paths,
        &home.path().join("aterm.toml"),
        None,
        &Pass {
            read_by: Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            vault_wait: Duration::from_millis(50),
            again: false,
            now_ms: NOW,
        },
    );
    assert_eq!(outcomes.len(), super::FILES.len());
    for outcome in &outcomes {
        assert_eq!(
            (outcome.word, outcome.why),
            (Word::Never, Some(Why::Membership)),
            "{}: {:?}",
            outcome.name,
            outcome.detail
        );
    }
    let root = super::root(&paths);
    let read = File::at(&root, &super::ACCOUNTS.file()).read();
    assert!(read.rows().is_empty() && read.snapshot.is_none());
    let name = Name::parse("ai/accounts").unwrap();
    assert_eq!(
        super::find(&name).map(|file| file.name),
        Some("ai/accounts")
    );
}

/// The one who writes in these tests: a person at a terminal, with the hang detector for the
/// publisher's bound (a test's publication must happen; two seconds is a product's patience).
fn env() -> write::Env<'static> {
    write::Env {
        by: "person",
        session: Some("s-test"),
        now_ms: NOW,
        attempt: aterm_spec::wait::MUST_HAPPEN,
        json: false,
        stdin: None,
        editor: &|_, _, _| Err("a test opens no editor".to_owned()),
        say: &|_| {},
    }
}

/// One write verb of `aterm file` over `paths`, as `env` runs it. The write verbs' clippy.toml
/// row lets only `aterm file`'s own entry run them; this is aterm-db's one test helper past it.
fn run_verb(
    paths: &Paths,
    config: &std::path::Path,
    list: &[&str],
    env: &write::Env<'_>,
) -> Result<(String, u8), write::Refusal> {
    #[expect(
        clippy::disallowed_methods,
        reason = "aterm-db's one test helper for the write verbs"
    )]
    let said = write::verb(paths, config, &words(list), env);
    said.map(write::Wrote::printed)
}

/// One write verb of `aterm file` over `paths`: what it printed and its exit code.
#[track_caller]
fn wrote(paths: &Paths, config: &std::path::Path, list: &[&str]) -> (String, u8) {
    run_verb(paths, config, list, &env()).unwrap_or_else(|refusal| panic!("{list:?}: {refusal:?}"))
}

/// The id a `queued <id>: <why>` or `held <id>: <why>` answer names.
#[track_caller]
fn id_of(answer: &str) -> String {
    let id = answer
        .split_whitespace()
        .nth(1)
        .unwrap_or_else(|| panic!("{answer}"))
        .trim_end_matches(':');
    assert!(aterm_file::outbox::is_id(id), "{answer}");
    id.to_owned()
}

/// The operation a `published <operation>` answer names.
#[track_caller]
fn op_of(answer: &str) -> String {
    let op = answer
        .strip_prefix("published ")
        .unwrap_or_else(|| panic!("{answer}"))
        .trim();
    assert!(aterm_file::outbox::digest(op).is_some(), "{answer}");
    op.to_owned()
}

pub(super) fn publish_pass(paths: &Paths, config: &std::path::Path) -> Vec<publish::Outcome> {
    publish::pass(
        paths,
        config,
        None,
        &publish::Pass {
            by: Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            now_ms: NOW,
        },
    )
}

/// What a pass did with the intent `id`.
#[track_caller]
fn answer_of(outcomes: &[publish::Outcome], id: &str) -> Answer {
    outcomes
        .iter()
        .find_map(|outcome| outcome.answer(id).cloned())
        .unwrap_or_else(|| panic!("no pass answered {id}: {outcomes:?}"))
}

/// The records of the registry at `paths` that carry the request `id`: (operation, actor).
fn carrying(paths: &Paths, id: &str) -> Vec<(String, astream_wire::sync::Actor)> {
    let mut request = [0; 16];
    assert!(aterm_codec::hex::decode_into(id.as_bytes(), &mut request));
    att::Handle::at(paths.clone(), &att::REGISTRY)
        .history()
        .unwrap()
        .into_iter()
        .filter(|entry| entry.request == Some(request))
        .map(|entry| {
            (
                aterm_codec::hex::encode_lower(&entry.operation),
                entry.actor,
            )
        })
        .collect()
}

/// What the registry at `paths` says the machine `pk`'s `column` is now.
fn stored(paths: &Paths, pk: &str, column: &str) -> Option<Value> {
    att::Handle::at(paths.clone(), &att::REGISTRY)
        .view()
        .unwrap()
        .project(&MACHINE)
        .rows
        .get(pk)
        .and_then(|row| row.get(column).cloned())
}

/// What a read of `lab/machines` under `root` shows in the row `pk`'s `column`.
fn shown(root: &Root, pk: &str, column: &str) -> String {
    let read = File::at(root, &super::MACHINES.file()).read();
    read.rows()
        .iter()
        .find(|row| row.pk == pk)
        .map(|row| row.cell(column).to_owned())
        .unwrap_or_default()
}

/// SF-05's exit test: a write made on a fenced Mac is kept, and its admitted successor
/// publishes it once.
///
/// A served stand-in lab's first Mac holds a registry and a materialized `lab/machines`. Its
/// table binding is lost (the retained device number changes, as on m114) and a recovery pass
/// reserves a successor nobody has admitted: the home is fenced. `aterm file set` there queues
/// one intent and answers `queued <id>: succession …`; the fenced writer signs nothing (its
/// table files are byte for byte what the fence left), a read shows the write laid over the
/// snapshot, and `cat --published` shows the snapshot alone. The runtime's own recovery pass
/// (SF-A) then signs the fenced home's statement, admits its successor and imports the
/// witnessed prefix. The next publisher pass, run over the original home as the upkeep thread
/// runs it, signs the intent under the successor's actor; a second pass and a third find
/// nothing to sign; exactly one record of the table carries the intent's id as its request,
/// the successor's; the table says the new value; and a materializer pass leaves the snapshot
/// saying it with nothing queued.
///
/// NEGATIVE CONTROLS, each observed: with a pass that cannot open the writer dropping the
/// intents it leaves (`Work::queue` calling `drop_intent`), the write is gone once the verb
/// answers: `` left: (0, 0, 0) right: (1, 0, 0) ``; with the publisher appending its record a
/// second time, the operation the pass answers is not the table's one record of the intent
/// (the `records[0].0 == op` comparison fails, two different digests).
#[test]
fn a_write_made_on_a_fenced_mac_is_kept_and_published_once_by_its_admitted_successor() {
    use generation::tests::{allowed_member, changed_device, completes, served};
    let mut served = served();
    allowed_member(&mut served, "m2");
    let lab = &served.lab;
    let original = lab.paths[0].clone();
    let config = original.home.join("config.toml");
    let machine = keys::machine_pk("m7");
    Registry::at(original.clone()).open().unwrap();
    declare(&original, &att::TRACKER);
    Registry::at(original.clone())
        .write(vec![vec![
            put(&MACHINE, &machine, "label", json!("m7")),
            put(&MACHINE, &machine, "status", json!("active")),
            put(&MACHINE, &machine, "note", json!("the desk Mac")),
        ]])
        .unwrap();
    let root = super::root(&original);
    let pass = |again: bool| {
        materialize::pass(
            &original,
            &config,
            None,
            &Pass {
                read_by: Instant::now() + aterm_spec::wait::MUST_HAPPEN,
                vault_wait: materialize::VAULT_WAIT,
                again,
                now_ms: NOW,
            },
        )
    };
    for outcome in pass(false) {
        assert_eq!(
            (outcome.word, outcome.why),
            (Word::Live, None),
            "{:?}",
            outcome.detail
        );
    }
    assert_eq!(shown(&root, &machine, "note"), "the desk Mac");
    let old_actor = att::Handle::at(original.clone(), &att::REGISTRY)
        .history()
        .unwrap()[0]
        .actor;

    // The loss, and the fence of a recovery pass whose successor nobody has admitted yet.
    changed_device(lab);
    generation::file_seams::fence_pending(&original, &lab.bootstrap(0));
    assert_eq!(
        generation::completed_generation_until(
            &original,
            Instant::now() + aterm_spec::wait::MUST_HAPPEN
        )
        .err()
        .unwrap()
        .to_string(),
        PRESERVING
    );
    let tables = original.dir.join("commons/tables");
    let fenced_records = crate::tests::files(&tables);

    // The write, made on the fenced Mac: kept, unsigned, and read back.
    let (answer, code) = wrote(
        &original,
        &config,
        &["set", "lab/machines", "m7", "note", "moved to the lab"],
    );
    assert_eq!(code, aterm_file::exit::OK, "{answer}");
    assert!(answer.starts_with("queued "), "{answer}");
    assert!(answer.contains(": succession: "), "{answer}");
    assert!(!answer.contains("moved to the lab"), "{answer}");
    let id = id_of(&answer);
    let held = File::at(&root, &super::MACHINES.file());
    let outbox = held.outbox();
    assert_eq!(
        (outbox.queued.len(), outbox.held.len(), outbox.done.len()),
        (1, 0, 0)
    );
    assert_eq!(outbox.queued[0].id, id);
    assert_eq!(outbox.queued[0].by, "person");
    assert_eq!(held.status().queued, 1);
    assert_eq!(shown(&root, &machine, "note"), "moved to the lab");
    assert!(verb(&root, &["cat", "lab/machines", "--published"]).contains("the desk Mac"));
    assert!(!verb(&root, &["cat", "lab/machines", "--published"]).contains("moved to the lab"));
    assert!(verb(&root, &["cat", "lab/machines"]).contains("moved to the lab"));
    // A pass under the fence keeps it queued, again and again, and signs nothing.
    for _ in 0..2 {
        let queued = answer_of(&publish_pass(&original, &config), &id);
        assert!(
            matches!(&queued, Answer::Queued { why } if why.starts_with("succession: ")),
            "{queued:?}"
        );
    }
    assert_eq!(held.outbox().queued.len(), 1);
    assert_eq!(
        crate::tests::files(&tables),
        fenced_records,
        "the fenced writer signed nothing"
    );

    // The runtime's recovery pass admits the successor and imports what the fenced home held.
    let next = generation::file_seams::advance(&original, &config).unwrap();
    assert_eq!(next, generation::candidate(&original));
    assert!(completes(&original));
    assert_eq!(
        held.outbox().queued.len(),
        1,
        "the succession kept the intent"
    );
    assert!(carrying(&next, &id).is_empty());

    // The publisher, over the original home as the upkeep thread runs it: once.
    let published = answer_of(&publish_pass(&original, &config), &id);
    let Answer::Published { op: Some(op) } = published else {
        panic!("{published:?}");
    };
    for _ in 0..2 {
        let again = publish_pass(&original, &config);
        assert!(
            again.iter().all(|outcome| outcome.intents.is_empty()),
            "{again:?}"
        );
    }
    let records = carrying(&next, &id);
    assert_eq!(records.len(), 1, "exactly one record carries the intent");
    assert_eq!(records[0].0, op);
    assert_ne!(records[0].1, old_actor, "the successor signed it");
    assert_eq!(
        crate::tests::files(&tables),
        fenced_records,
        "the fenced writer signed nothing"
    );
    assert_eq!(
        stored(&next, &machine, "note"),
        Some(json!("moved to the lab"))
    );
    let outbox = held.outbox();
    assert_eq!((outbox.queued.len(), outbox.held.len()), (0, 0));
    assert_eq!(outbox.done.len(), 1);
    assert_eq!(outbox.done[0].op.as_deref(), Some(op.as_str()));
    // Published and not yet materialized: the read still says it.
    assert_eq!(shown(&root, &machine, "note"), "moved to the lab");
    for outcome in pass(false) {
        assert_eq!(
            (outcome.word, outcome.why),
            (Word::Live, None),
            "{:?}",
            outcome.detail
        );
    }
    assert!(verb(&root, &["cat", "lab/machines", "--published"]).contains("moved to the lab"));
    assert_eq!(held.status().queued, 0);
    assert!(held.outbox().done[0].seen, "the snapshot holds the record");
}

/// A pass killed between its record and its marker, or between its marker and the dequeue,
/// signs nothing again: the next pass finds the record by the intent's id (or the marker), and
/// exactly one record carries each intent.
///
/// NEGATIVE CONTROL, observed: with `Log::carried` finding nothing, the pass after the kill
/// does not know its record and answers as if nothing had been signed:
/// `` left: Published { op: None } ``.
#[test]
fn a_pass_killed_after_it_signed_or_after_its_marker_republishes_nothing() {
    let stand = Stand::new();
    all(&stand, &stand.pass(false), Word::Live, None);
    let (paths, config) = (stand.paths(), &stand.config);
    for (stop, note) in [
        (publish::seam::Stop::AfterSign, "first"),
        (publish::seam::Stop::AfterMarker, "second"),
    ] {
        publish::seam::kill_at(stop);
        let (answer, _) = wrote(paths, config, &["set", "lab/machines", "m7", "note", note]);
        assert!(answer.starts_with("queued "), "{stop:?}: {answer}");
        let id = id_of(&answer);
        assert_eq!(
            carrying(paths, &id).len(),
            1,
            "{stop:?}: the record is signed"
        );
        let held = stand.file(&super::MACHINES);
        assert_eq!(
            held.outbox().queued.len(),
            1,
            "{stop:?}: still in the outbox"
        );
        let answer = answer_of(&publish_pass(paths, config), &id);
        let records = carrying(paths, &id);
        assert_eq!(records.len(), 1, "{stop:?}: nothing was signed again");
        assert_eq!(
            answer,
            Answer::Published {
                op: Some(records[0].0.clone())
            }
        );
        assert!(held.outbox().queued.is_empty());
        all(&stand, &stand.pass(false), Word::Live, None);
    }
    assert_eq!(stored(paths, &stand.machine, "note"), Some(json!("second")));
}

/// A write whose register moved since its author looked is held: durable, shown `⚠ mine ‖
/// theirs`, never applied, never dropped; `resolve … theirs` drops it and `resolve … mine`
/// writes it against what the table shows now.
#[test]
fn a_write_whose_register_moved_is_held_until_a_person_resolves_it() {
    let stand = Stand::new();
    all(&stand, &stand.pass(false), Word::Live, None);
    let (paths, config, root) = (stand.paths(), &stand.config, stand.root());
    let machine = stand.machine.clone();
    let theirs = |text: &str| {
        Registry::at(paths.clone())
            .write(vec![vec![put(&MACHINE, &machine, "note", json!(text))]])
            .unwrap();
    };
    for side in ["theirs", "mine"] {
        // Another writer's record lands on the register after this Mac's snapshot.
        theirs(&format!("peer says {side}"));
        let (answer, code) = wrote(
            paths,
            config,
            &["set", "lab/machines", "m7", "note", "my note"],
        );
        assert_eq!(code, aterm_file::exit::CONFLICT, "{answer}");
        assert!(answer.starts_with("held "), "{answer}");
        assert!(answer.contains("changed since it was written"), "{answer}");
        assert!(
            !answer.contains("my note") && !answer.contains("peer says"),
            "{answer}"
        );
        let id = id_of(&answer);
        let held = stand.file(&super::MACHINES);
        let outbox = held.outbox();
        assert_eq!((outbox.queued.len(), outbox.held.len()), (0, 1));
        assert_eq!(held.status().held, 1);
        assert!(
            carrying(paths, &id).is_empty(),
            "a held intent is never applied"
        );
        assert_eq!(
            stored(paths, &machine, "note"),
            Some(json!(format!("peer says {side}")))
        );
        // Pass after pass it stays held, and the read marks the cell once theirs is here.
        assert!(
            publish_pass(paths, config)
                .iter()
                .all(|o| o.intents.is_empty())
        );
        all(&stand, &stand.pass(false), Word::Live, None);
        assert_eq!(held.outbox().held.len(), 1);
        assert_eq!(
            shown(&root, &machine, "note"),
            format!("⚠ my note ‖ peer says {side}")
        );
        let listed = verb(&root, &["outbox", "lab/machines"]);
        assert!(listed.contains(&id) && listed.contains("held"), "{listed}");
        assert!(!listed.contains("my note"), "{listed}");
        let (answer, code) = wrote(paths, config, &["resolve", "lab/machines", &id, side]);
        assert_eq!(code, aterm_file::exit::OK, "{answer}");
        let outbox = held.outbox();
        assert_eq!((outbox.queued.len(), outbox.held.len()), (0, 0));
        if side == "theirs" {
            assert_eq!(answer, format!("dropped {id}: theirs kept\n"));
            assert_eq!(shown(&root, &machine, "note"), "peer says theirs");
        } else {
            op_of(&answer);
            assert_eq!(stored(paths, &machine, "note"), Some(json!("my note")));
            assert_eq!(shown(&root, &machine, "note"), "my note");
        }
    }
}

/// A write that follows this Mac's own published write of a cell, while a pass cut short left the
/// frontier ahead of the snapshot, or the snapshot ahead of the status, follows that write
/// (`intent:<id>`) and is published: only the frontier the status names for the snapshot at the
/// baseline's revision says what that baseline holds (tracker package FL-170).
///
/// NEGATIVE CONTROL, observed: with the write's `follow` reading the frontier unvouched
/// (`target.held.frontier()`), the second write expects the baseline's value and the publisher
/// holds it against the first, in the cut before the snapshot: `held <id>: changed since it was
/// written: <row>.note`.
#[test]
fn a_write_after_a_pass_cut_short_follows_this_macs_published_write() {
    // What the cut pass had not written yet: everything but its frontier, or only its status.
    for (cut, snapshot_too) in [("before the snapshot", true), ("before the status", false)] {
        let stand = Stand::new();
        all(&stand, &stand.pass(false), Word::Live, None);
        let (paths, config, root) = (stand.paths(), &stand.config, stand.root());
        let machine = stand.machine.clone();
        let held = stand.file(&super::MACHINES);
        let name = super::MACHINES.file();
        stand.commons("[commons]\nenabled = false\n");
        let (first, _) = wrote(paths, config, &["set", "lab/machines", "m7", "note", "one"]);
        let first = id_of(&first);
        std::fs::remove_file(config).unwrap();
        assert!(matches!(
            answer_of(&publish_pass(paths, config), &first),
            Answer::Published { op: Some(_) }
        ));
        let mut unwritten = vec![root.state(&name), root.done(&name)];
        if snapshot_too {
            unwritten.push(root.snapshot(&name));
        }
        let kept: Vec<_> = unwritten
            .into_iter()
            .map(|path| (std::fs::read(&path).unwrap(), path))
            .collect();
        all(&stand, &stand.pass(false), Word::Live, None);
        for (bytes, path) in &kept {
            std::fs::write(path, bytes).unwrap();
        }
        assert!(
            held.projected_from().is_none(),
            "{cut}: no status names this snapshot and this frontier"
        );
        assert!(
            held.projected_at(held.published().status.rev()).is_none(),
            "{cut}"
        );
        assert_eq!(shown(&root, &machine, "note"), "one", "{cut}");
        let (second, code) = wrote(paths, config, &["set", "lab/machines", "m7", "note", "two"]);
        assert_eq!(code, aterm_file::exit::OK, "{cut}: {second}");
        op_of(&second);
        assert_eq!(stored(paths, &machine, "note"), Some(json!("two")), "{cut}");
    }
}

/// This Mac's rename of its own row, number and nickname as one write, that met another writer's
/// number is held and stays pending, and gives way through the naming code's door only once the
/// row read here shows that number: until a pass brings it, the row still shows the number the
/// write replaced, which a follow must not take. The nickname's register never moved, so the
/// write gives way on the number alone, whole, as the one rename it is. Then the held write is
/// dropped, the registry's values stand, and no person is asked (the owner's words of
/// 2026-11-16, `docs/OWNER_RULINGS.md`).
///
/// NEGATIVE CONTROLS, observed: with `here_published` listing a held write whatever its row
/// shows (`shown` always true), the write is offered against the row that still shows m7:
/// `left: 1 right: 0`; with the write giving way only once every cell it writes shows another
/// value (`edits.iter().all(shows)` for the cells the hold names), it never gives way:
/// `left: [] right: [HeldName { .. }]`.
#[test]
fn a_held_name_write_gives_way_to_the_registrys_value() {
    use super::{HeldName, Renamed};
    use crate::alab::view::MachineLabel;
    let stand = Stand::new();
    let (paths, config) = (stand.paths(), &stand.config);
    let machine = stand.machine.clone();
    // This Mac's facts row binds the machine, so the pass marks its row `here`.
    Registry::at(paths.clone())
        .write(vec![vec![crate::database::Change {
            collection: crate::alab::registry::MACHINE_FACTS.name.to_owned(),
            key: uuid_of(&stand.lab, SEAT),
            value: Some(json!({"r": 1, "n": 1, "mac": machine, "label": "m7"})),
        }]])
        .unwrap();
    all(&stand, &stand.pass(false), Word::Live, None);
    // Another writer's number lands on the register after this Mac's snapshot.
    Registry::at(paths.clone())
        .write(vec![vec![put(&MACHINE, &machine, "label", json!("m9"))]])
        .unwrap();
    let cells = [(Renamed::Label, "m12"), (Renamed::Nickname, "Otter")];
    #[expect(
        clippy::disallowed_methods,
        reason = "the test of the rename door and of the door a held name write gives way through"
    )]
    let id = super::rename(paths, config, &machine, &cells, &env())
        .expect("queued")
        .expect("a write")
        .id;
    let held = stand.file(&super::MACHINES).outbox().held;
    assert_eq!(
        held.iter()
            .map(|held| (held.intent.id.as_str(), held.intent.edits.len()))
            .collect::<Vec<_>>(),
        [(id.as_str(), 2)],
        "one write holds the rename"
    );
    let label = |here: &super::HereRow| match here.label() {
        MachineLabel::Named { label, .. } => label,
        other => panic!("{other:?}"),
    };
    // The snapshot here is from before that number: the write waits, and nothing is offered.
    let here = super::here_published(paths).expect("this Mac's row");
    assert_eq!(label(&here), "m7");
    assert!(here.pending(Renamed::Label) && here.pending(Renamed::Nickname));
    assert_eq!(here.held(), []);
    // A pass brings it: the row shows what the write met, and the write gives way, whole.
    all(&stand, &stand.pass(false), Word::Live, None);
    let here = super::here_published(paths).expect("this Mac's row");
    assert_eq!(label(&here), "m9");
    assert!(here.pending(Renamed::Label));
    assert_eq!(
        here.held(),
        [HeldName {
            id: id.clone(),
            cells: vec![
                ("label".to_owned(), "m12".to_owned()),
                ("nickname".to_owned(), "Otter".to_owned())
            ],
        }]
    );
    #[expect(
        clippy::disallowed_methods,
        reason = "the test of the door a held name write gives way through"
    )]
    super::yield_held(paths, config, &id, &env()).expect("dropped");
    let here = super::here_published(paths).expect("this Mac's row");
    assert_eq!(label(&here), "m9");
    assert!(!here.pending(Renamed::Label) && !here.pending(Renamed::Nickname));
    assert_eq!(here.held(), []);
    assert_eq!(stored(paths, &machine, "label"), Some(json!("m9")));
    assert!(stand.file(&super::MACHINES).outbox().held.is_empty());
}

/// An owner's write of this Mac's own row that carries a name cell with another cell of the row
/// (`aterm file set lab/machines <row> nickname=Otter note=desk`) gives way, whole, as a
/// rename's does, once the row shows the value it met: no name cell of this Mac waits on a
/// person's `aterm file resolve`.
///
/// NEGATIVE CONTROL, observed: with `here_published` taking only writes of the name cells alone,
/// the write is never offered and its nickname stays pending for good: `left: [] right:
/// [HeldName { .. }]`.
#[test]
fn a_held_write_with_a_name_cell_and_another_cell_gives_way_whole() {
    use super::{HeldName, Renamed};
    let stand = Stand::new();
    let (paths, config) = (stand.paths(), &stand.config);
    let machine = stand.machine.clone();
    Registry::at(paths.clone())
        .write(vec![vec![crate::database::Change {
            collection: crate::alab::registry::MACHINE_FACTS.name.to_owned(),
            key: uuid_of(&stand.lab, SEAT),
            value: Some(json!({"r": 1, "n": 1, "mac": machine, "label": "m7"})),
        }]])
        .unwrap();
    all(&stand, &stand.pass(false), Word::Live, None);
    // Another writer's note lands on the register after this Mac's snapshot.
    Registry::at(paths.clone())
        .write(vec![vec![put(&MACHINE, &machine, "note", json!("rack"))]])
        .unwrap();
    let (said, code) = run_verb(
        paths,
        config,
        &[
            "set",
            "lab/machines",
            &machine,
            "nickname=Otter",
            "note=desk",
        ],
        &env(),
    )
    .expect("a write");
    assert_eq!(code, aterm_file::exit::CONFLICT, "{said}");
    let id = stand.file(&super::MACHINES).outbox().held[0]
        .intent
        .id
        .clone();
    let here = super::here_published(paths).expect("this Mac's row");
    assert!(here.pending(Renamed::Nickname));
    assert_eq!(here.held(), [], "the row does not show the note it met yet");
    all(&stand, &stand.pass(false), Word::Live, None);
    let here = super::here_published(paths).expect("this Mac's row");
    assert_eq!(
        here.held(),
        [HeldName {
            id: id.clone(),
            cells: vec![
                ("nickname".to_owned(), "Otter".to_owned()),
                ("note".to_owned(), "desk".to_owned())
            ],
        }]
    );
    #[expect(
        clippy::disallowed_methods,
        reason = "the test of the door a held name write gives way through"
    )]
    super::yield_held(paths, config, &id, &env()).expect("dropped");
    let here = super::here_published(paths).expect("this Mac's row");
    assert!(!here.pending(Renamed::Nickname));
    assert_eq!(stored(paths, &machine, "note"), Some(json!("rack")));
}

/// A follow-up edit of one cell is compared with what the earlier intent it follows left in that
/// cell's own register, as the publisher compares it, never with another register of that
/// intent: a rename writes the number and the nickname as one intent, and the registers it left
/// can differ (a peer had written the number first, so the number's register holds the peer's
/// operation).
///
/// NEGATIVE CONTROL, observed: with `against` reading the intent's first register
/// (`done.left.values().next()`), the nickname edit reads the number's:
/// `left: Some(["aaaa…"]) right: Some(["bbbb…"])`.
#[test]
fn a_follow_up_is_compared_with_its_own_register_of_the_intent_it_follows() {
    let pk = "mc-0123456789abcdef";
    let (peer, mine) = ("a".repeat(64), "b".repeat(64));
    let earlier = "1".repeat(32);
    let done = aterm_file::Done {
        intent: aterm_file::Intent {
            id: earlier.clone(),
            file: super::MACHINES.file(),
            ms: NOW,
            by: "person".to_owned(),
            session: None,
            born: false,
            revert: None,
            edits: Vec::new(),
            resolves: None,
        },
        op: Some(mine.clone()),
        ms: NOW,
        seen: true,
        generation: Some(1),
        source: None,
        left: std::collections::BTreeMap::from([
            (format!("{pk}.label"), vec![peer.clone()]),
            (format!("{pk}.nickname"), vec![mine.clone()]),
        ]),
    };
    let outbox = aterm_file::Outbox {
        done: vec![done],
        ..aterm_file::Outbox::default()
    };
    let nickname = aterm_file::Edit {
        key: pk.to_owned(),
        column: "nickname".to_owned(),
        elem: None,
        value: json!("Heron"),
        text: "Heron".to_owned(),
        expected: vec![format!("{}{earlier}", aterm_file::outbox::INTENT)],
    };
    assert_eq!(super::against(&outbox, &nickname), Some(vec![mine]));
    let label = aterm_file::Edit {
        column: "label".to_owned(),
        ..nickname
    };
    assert_eq!(super::against(&outbox, &label), Some(vec![peer]));
}

/// The write verbs on a stand-in lab: two writes of one cell queued while sharing is off are
/// published in order, the second following the first; `add` bears a row under the key the
/// publisher derives; `rm` retires it; `revert` restores what an operation replaced; a Mac's
/// own row (`@self`) has its key filled with the signing Mac's uuid under the writer; and
/// `outbox drop` drops a queued or held intent.
#[test]
fn the_write_verbs_queue_follow_bear_retire_and_revert() {
    let stand = Stand::new();
    all(&stand, &stand.pass(false), Word::Live, None);
    let (paths, config, root) = (stand.paths(), &stand.config, stand.root());
    let machine = stand.machine.clone();
    let held = stand.file(&super::MACHINES);

    // Sharing off: both writes of the cell queue, and the read shows the later one.
    stand.commons("[commons]\nenabled = false\n");
    let (first, _) = wrote(paths, config, &["set", "lab/machines", "m7", "note", "one"]);
    let (second, _) = wrote(paths, config, &["set", "lab/machines", "m7", "note", "two"]);
    assert!(
        first.contains(": off: ") && second.contains(": off: "),
        "{first}{second}"
    );
    let (first, second) = (id_of(&first), id_of(&second));
    assert_eq!(shown(&root, &machine, "note"), "two");
    assert_eq!(
        held.outbox().queued[1].edits[0].expected,
        [format!("intent:{first}")]
    );
    // An unchanged cell asks for nothing.
    let (same, _) = wrote(paths, config, &["set", "lab/machines", "m7", "note", "two"]);
    assert!(same.starts_with("unchanged: "), "{same}");
    assert_eq!(held.outbox().queued.len(), 2);
    std::fs::remove_file(config).unwrap();
    let outcomes = publish_pass(paths, config);
    let Answer::Published { op: Some(one) } = answer_of(&outcomes, &first) else {
        panic!("{outcomes:?}");
    };
    assert!(matches!(
        answer_of(&outcomes, &second),
        Answer::Published { op: Some(_) }
    ));
    assert_eq!(stored(paths, &machine, "note"), Some(json!("two")));
    all(&stand, &stand.pass(false), Word::Live, None);
    let history: Value =
        aterm_json::from_str(&verb(&root, &["history", "lab/machines", "--json"])).unwrap();
    assert!(
        history.to_string().contains(&first)
            && history.to_string().contains("person")
            && history.to_string().contains("s-test"),
        "the history names the intent, who said they made it and the session: {history}"
    );

    // Revert the second write: the cell says the first again.
    let two = carrying(paths, &second)[0].0.clone();
    let (answer, _) = wrote(paths, config, &["revert", "lab/machines", &two]);
    op_of(&answer);
    assert_eq!(stored(paths, &machine, "note"), Some(json!("one")));
    assert_ne!(one, two);
    // The same revert again: its operation's cell moved, so it is held, and keeping it as it is
    // would only be held again, so `resolve … mine` refuses it and leaves it held.
    let (twice, code) = wrote(paths, config, &["revert", "lab/machines", &two]);
    assert_eq!(code, aterm_file::exit::CONFLICT, "{twice}");
    let twice = id_of(&twice);
    assert!(matches!(
        run_verb(paths, config, &["resolve", "lab/machines", &twice, "mine"], &env()),
        Err(write::Refusal::Problem(why)) if why.contains("revert")
    ));
    assert_eq!(held.outbox().held.len(), 1);
    wrote(
        paths,
        config,
        &["resolve", "lab/machines", &twice, "theirs"],
    );

    // A row is born under the key the publisher derives, and retired.
    let (answer, _) = wrote(
        paths,
        config,
        &["add", "lab/machines", "label=m9", "status=planned"],
    );
    op_of(&answer);
    let born = keys::machine_pk("m9");
    assert_eq!(stored(paths, &born, "label"), Some(json!("m9")));
    assert_eq!(shown(&root, &born, "status"), "planned");
    // A second birth of the row that says the same asks for nothing; one that says something
    // else is held whole, never written into a row its author did not see.
    let (same, _) = wrote(
        paths,
        config,
        &["add", "lab/machines", "label=m9", "status=planned"],
    );
    assert!(
        same.starts_with("published: the table already says"),
        "{same}"
    );
    let (again, code) = wrote(
        paths,
        config,
        &["add", "lab/machines", "label=m9", "status=active"],
    );
    assert_eq!(code, aterm_file::exit::CONFLICT, "{again}");
    assert_eq!(stored(paths, &born, "status"), Some(json!("planned")));
    // A held birth would be held again as it is: `resolve … mine` refuses it, queues nothing,
    // and leaves it held for `set` on the row that is there or `theirs`.
    assert!(matches!(
        run_verb(
            paths,
            config,
            &["resolve", "lab/machines", &id_of(&again), "mine"],
            &env()
        ),
        Err(write::Refusal::Problem(why)) if why.contains("new row")
    ));
    let outbox = held.outbox();
    assert_eq!((outbox.queued.len(), outbox.held.len()), (0, 1));
    let (dropped, _) = wrote(
        paths,
        config,
        &["resolve", "lab/machines", &id_of(&again), "theirs"],
    );
    assert!(dropped.starts_with("dropped "), "{dropped}");
    let (answer, _) = wrote(paths, config, &["rm", "lab/machines", "m9"]);
    op_of(&answer);
    assert_eq!(stored(paths, &born, "row"), Some(json!("retired")));

    // A Mac's own row: the publisher fills the signing Mac's uuid.
    let uuid = keys::uuid32(
        crate::table::Session::observe(&stand.lab.bootstrap(SEAT))
            .unwrap()
            .members()
            .unwrap()
            .iter()
            .find(|binding| {
                binding.actor()
                    == crate::table::Session::observe(&stand.lab.bootstrap(SEAT))
                        .unwrap()
                        .actor()
            })
            .unwrap()
            .machine(),
    );
    assert_eq!(publish::filled("@self", &uuid), uuid);
    assert_eq!(
        publish::filled("claude.@self", &uuid),
        format!("claude.{uuid}")
    );
    assert_eq!(publish::filled(&machine, &uuid), machine);
    // A Mac's statement of its own row to `ai/accounts`, an owner file: the publisher holds it
    // with the class's rule, never signs it and never writes it at another key, and a person
    // drops it.
    // NEGATIVE CONTROL, observed: with the publisher's class check removed, the statement
    // reaches the typed grid and is held for the filled key instead: `` left: Held { why: "a
    // typed cell key breaks its grammar" } ``.
    let accounts = stand.file(&super::ACCOUNTS);
    let mine = accounts
        .state_mine(
            "harness",
            aterm_file::Draft {
                born: false,
                revert: None,
                edits: vec![aterm_file::Edit {
                    key: "@self".to_owned(),
                    column: "label".to_owned(),
                    elem: None,
                    value: json!("says the Mac itself"),
                    text: "says the Mac itself".to_owned(),
                    expected: Vec::new(),
                }],
            },
            NOW,
        )
        .unwrap();
    let outcomes = publish_pass(paths, config);
    assert_eq!(
        answer_of(&outcomes, &mine.id),
        Answer::Held {
            why: "an owner file takes no Mac statement".to_owned()
        },
        "{outcomes:?}"
    );
    assert!(carrying(paths, &mine.id).is_empty());
    assert_eq!(accounts.outbox().held.len(), 1);
    // A person cannot keep it as theirs: `resolve … mine` says so and how to drop it, and
    // queues nothing.
    // NEGATIVE CONTROL, observed: with `again` taking the statement as an owner's edits, the
    // requeue refuses its `@self` keys as a malformed draft, which says nothing of `theirs`:
    // `` Err(Problem("a Mac's own row is keyed `<pk>.@self`, and an owner's write never is")) ``.
    let kept = run_verb(
        paths,
        config,
        &["resolve", "ai/accounts", &mine.id, "mine"],
        &env(),
    );
    assert!(
        matches!(
            &kept,
            Err(write::Refusal::Problem(why))
                if why.contains("an owner file takes no Mac statement")
                    && why.contains(&format!("resolve ai/accounts {} theirs", mine.id))
        ),
        "{kept:?}"
    );
    let outbox = accounts.outbox();
    assert_eq!((outbox.queued.len(), outbox.held.len()), (0, 1));
    assert_eq!(
        verb(&root, &["outbox", "drop", &mine.id]),
        format!("dropped {} (ai/accounts)\n", mine.id)
    );

    // `outbox drop` is a person's word for a queued intent.
    stand.commons("[commons]\nenabled = false\n");
    let (queued, _) = wrote(
        paths,
        config,
        &["set", "lab/machines", "m7", "note", "never"],
    );
    let id = id_of(&queued);
    assert_eq!(
        verb(&root, &["outbox", "drop", &id]),
        format!("dropped {id} (lab/machines)\n")
    );
    assert!(held.outbox().queued.is_empty());
    std::fs::remove_file(config).unwrap();
    assert!(
        publish_pass(paths, config)
            .iter()
            .all(|o| o.intents.is_empty())
    );
    assert!(carrying(paths, &id).is_empty());
}

/// `set` takes the words its help and the design give, `set <name> <row> <column> [<value> |
/// --stdin]`: a value writes the cell, `--stdin` writes standard input less its line ending, no
/// value clears the cell, more words are a usage error, and `--json` answers one object.
///
/// NEGATIVE CONTROL, observed: with `set` reading `<column>=<value>` words again, the documented
/// form is refused: `Usage("`note` is not <column>=<value>")`.
#[test]
fn set_takes_the_documented_words_clears_reads_stdin_and_answers_json() {
    let stand = Stand::new();
    all(&stand, &stand.pass(false), Word::Live, None);
    let (paths, config, root) = (stand.paths(), &stand.config, stand.root());
    let machine = stand.machine.clone();
    let (answer, code) = wrote(
        paths,
        config,
        &["set", "lab/machines", "m7", "note", "moved"],
    );
    assert_eq!(code, aterm_file::exit::OK, "{answer}");
    op_of(&answer);
    assert_eq!(stored(paths, &machine, "note"), Some(json!("moved")));

    let piped = write::Env {
        stdin: Some("from the pipe\n"),
        ..env()
    };
    let set = |list: &[&str], env: &write::Env<'_>| run_verb(paths, config, list, env);
    let (answer, _) = set(&["set", "lab/machines", "m7", "note", "--stdin"], &piped).unwrap();
    op_of(&answer);
    assert_eq!(
        stored(paths, &machine, "note"),
        Some(json!("from the pipe"))
    );
    assert!(matches!(
        set(&["set", "lab/machines", "m7", "note", "--stdin"], &env()),
        Err(write::Refusal::Problem(_))
    ));

    let json_out = write::Env {
        json: true,
        ..env()
    };
    let (answer, code) = set(&["set", "lab/machines", "m7", "note", "said"], &json_out).unwrap();
    assert_eq!(code, aterm_file::exit::OK);
    let said: Value = aterm_json::from_str(answer.trim()).unwrap_or_else(|_| panic!("{answer}"));
    assert_eq!(said.get("state"), Some(&json!("published")), "{said}");
    assert_eq!(said.get("file"), Some(&json!("lab/machines")), "{said}");
    let op = said.get("op").and_then(Value::as_str).unwrap_or_default();
    assert!(aterm_file::outbox::digest(op).is_some(), "{said}");
    assert!(
        !answer.contains("said\""),
        "no value in an answer: {answer}"
    );

    // No value: the cell is cleared.
    let (answer, _) = wrote(paths, config, &["set", "lab/machines", "m7", "note"]);
    op_of(&answer);
    assert!(stored(paths, &machine, "note").is_none_or(|value| value.is_null()));
    assert_eq!(shown(&root, &machine, "note"), "");
    let (again, _) = wrote(paths, config, &["set", "lab/machines", "m7", "note"]);
    assert!(again.starts_with("unchanged: "), "{again}");

    for extra in [
        &["set", "lab/machines", "m7", "note", "two", "words"][..],
        &["set", "lab/machines", "m7"][..],
    ] {
        assert!(
            matches!(set(extra, &env()), Err(write::Refusal::Usage(_))),
            "{extra:?}"
        );
    }
}

/// `set <name> <row> <column>=<value>…` writes several cells of one row as one write: one
/// intent holds every cell's edit, the publisher signs it as one operation, and [`super::offer`]
/// says what became of it; an empty value clears its cell, and a column named twice or a word
/// that is no assignment is a usage error.
///
/// An offer of a write another pass settled first says what its outbox holds of it.
///
/// NEGATIVE CONTROLS, observed: with `set_cells` taking only the first `<column>=<value>`, the
/// write holds one edit: `left: [(.., 1)] right: [(.., 2)]`; with `said_of` not reading the
/// outbox, the second offer says `queued <id>: the publisher has not reached it`.
#[test]
fn set_writes_several_cells_of_one_row_as_one_write() {
    let stand = Stand::new();
    all(&stand, &stand.pass(false), Word::Live, None);
    let (paths, config) = (stand.paths(), &stand.config);
    let machine = stand.machine.clone();
    let queue = write::Env {
        attempt: Duration::ZERO,
        ..env()
    };
    let words = ["set", "lab/machines", "m7", "note=both", "status=planned"];
    let (answer, _) = run_verb(paths, config, &words, &queue).expect("queued");
    let id = id_of(&answer);
    let queued = stand.file(&super::MACHINES).outbox().queued;
    assert_eq!(
        queued
            .iter()
            .map(|intent| (intent.id.as_str(), intent.edits.len()))
            .collect::<Vec<_>>(),
        [(id.as_str(), 2)]
    );
    let write = super::Queued {
        id: id.clone(),
        unflushed: None,
    };
    let said = super::offer(paths, config, &write, aterm_spec::wait::MUST_HAPPEN, NOW);
    let op = op_of(&said);
    // Offered again, once another pass settled it: its outbox says what became of it.
    let again = super::offer(paths, config, &write, aterm_spec::wait::MUST_HAPPEN, NOW);
    assert_eq!(op_of(&again), op, "{again}");
    assert_eq!(stored(paths, &machine, "note"), Some(json!("both")));
    assert_eq!(stored(paths, &machine, "status"), Some(json!("planned")));
    let (answer, _) = wrote(
        paths,
        config,
        &["set", "lab/machines", "m7", "note=", "status=planned"],
    );
    op_of(&answer);
    assert!(stored(paths, &machine, "note").is_none_or(|value| value.is_null()));
    for refused in [
        &["set", "lab/machines", "m7", "note=a", "note=b"][..],
        &["set", "lab/machines", "m7", "note=a", "status"][..],
    ] {
        assert!(
            matches!(
                run_verb(paths, config, refused, &env()),
                Err(write::Refusal::Usage(_))
            ),
            "{refused:?}"
        );
    }
}

/// `aterm file set` replaces a version-1 account label outside the handle grammar (`Team A`,
/// which version 1's free-text label took): it reads raw under `Handle`, yet it is the owner's,
/// so the set queues the new label and the publisher signs it, where it once refused the cell
/// as one a newer build wrote. The account goes by `claude-team-a` until then, its handle
/// derived from the label, and by `claude-team-b` after.
/// NEGATIVE CONTROL: with `grid::replaceable` answering `false`, the set is refused: ``
/// panicked … Problem("ai/accounts: label holds a value a newer build wrote; it is read-only
/// here") ``.
#[test]
fn set_replaces_an_older_builds_label_the_handle_type_refuses() {
    let stand = Stand::new();
    let (paths, config) = (stand.paths(), &stand.config);
    let bea = keys::account_pk("claude", BEA).unwrap();
    let v1 = crate::database::Change {
        collection: ACCOUNT.name.to_owned(),
        key: format!("{bea}.label"),
        value: Some(crate::typed::cell::CellValue::owner(1, json!("Team A")).encode()),
    };
    Registry::at(paths.clone()).write(vec![vec![v1]]).unwrap();
    all(&stand, &stand.pass(false), Word::Live, None);
    let accounts = stand.file(&super::ACCOUNTS).read();
    assert_eq!(accounts.named("claude-team-a").len(), 1);
    let (answer, code) = wrote(
        paths,
        config,
        &["set", "ai/accounts", "claude-team-a", "label", "team-b"],
    );
    assert_eq!(code, aterm_file::exit::OK, "{answer}");
    publish_pass(paths, config);
    let label = att::Handle::at(paths.clone(), &att::REGISTRY)
        .view()
        .unwrap()
        .project(&ACCOUNT)
        .rows[&bea]
        .get("label")
        .cloned();
    assert_eq!(label, Some(json!("team-b")));
    all(&stand, &stand.pass(false), Word::Live, None);
    let accounts = stand.file(&super::ACCOUNTS).read();
    assert_eq!(accounts.named("claude-team-b").len(), 1);
    assert!(accounts.named("claude-team-a").is_empty());
}

/// `set` and `rm` name an account row as every account verb does, and a word that names no row,
/// or several, is a usage refusal (exit 2) that writes nothing: one that names none lists none,
/// so a typo prints no address, and the address two accounts sign in with lists both by slug.
/// NEGATIVE CONTROL: with `Target::row` answering a resolver's refusal as a problem (`problem(…)`
/// in place of `usage(…)`, exit 3, as before), each refusal is `Problem`: `` panicked … set
/// ai/accounts fb-alpha note x: Err(Problem(…)) ``.
#[test]
fn a_write_to_a_word_that_names_no_account_or_several_is_a_usage_refusal() {
    let stand = Stand::new();
    all(&stand, &stand.pass(false), Word::Live, None);
    let (paths, config) = (stand.paths(), &stand.config);
    let refused = |list: &[&str]| match run_verb(paths, config, list, &env()) {
        Err(write::Refusal::Usage(why)) => why,
        other => panic!("{}: {other:?}", list.join(" ")),
    };
    for list in [
        &["set", "ai/accounts", "fb-alpha", "note", "x"][..],
        &["rm", "ai/accounts", "fb-alpha"],
    ] {
        let why = refused(list);
        assert!(why.contains(&aterm_file::no_account("fb-alpha")), "{why}");
        assert!(!why.contains('@'), "{why}");
    }
    for list in [
        &["set", "ai/accounts", ADA, "note", "x"][..],
        &["rm", "ai/accounts", ADA],
    ] {
        let why = refused(list);
        assert!(
            why.contains("names 2 accounts")
                && why.contains("claude-fb-alpha")
                && why.contains("oai-fb-alpha"),
            "{why}"
        );
    }
    let accounts = att::Handle::at(paths.clone(), &att::REGISTRY)
        .view()
        .unwrap()
        .project(&ACCOUNT);
    for row in accounts.rows.values() {
        assert!(row.live() && row.get("note").is_none(), "{row:?}");
    }
}

/// An edit that follows an earlier intent of this Mac's (`intent:<id>`) is compared with what
/// that intent left in the register. Where the earlier one wrote nothing there, because a peer
/// had already written the same value (it settled as `unchanged`, or its record carries only its
/// other cells), the follow-up goes ahead over that peer's candidate, whose value its author saw;
/// it is never held against a record that is not there.
///
/// NEGATIVE CONTROL, observed: with `plan` taking the earlier intent's operation, or nothing, for
/// `intent:<id>` (as before `Earlier::left`), the follow-up is held in both cases:
/// `left: Held { why: "changed since it was written: …note" }`.
#[test]
fn a_follow_up_is_compared_with_what_the_earlier_intent_left_in_the_register() {
    let stand = Stand::new();
    all(&stand, &stand.pass(false), Word::Live, None);
    let (paths, config) = (stand.paths(), &stand.config);
    let machine = stand.machine.clone();
    let held = stand.file(&super::MACHINES);
    let peer = |text: &str| {
        Registry::at(paths.clone())
            .write(vec![vec![put(&MACHINE, &machine, "note", json!(text))]])
            .unwrap();
    };

    // The earlier intent settles as `unchanged`: the peer wrote its value first.
    stand.commons("[commons]\nenabled = false\n");
    let (a, _) = wrote(
        paths,
        config,
        &["set", "lab/machines", "m7", "note", "agreed"],
    );
    peer("agreed");
    let (b, _) = wrote(
        paths,
        config,
        &["set", "lab/machines", "m7", "note", "later"],
    );
    let (a, b) = (id_of(&a), id_of(&b));
    assert_eq!(
        held.outbox().queued[1].edits[0].expected,
        [format!("intent:{a}")]
    );
    std::fs::remove_file(config).unwrap();
    let outcomes = publish_pass(paths, config);
    assert_eq!(answer_of(&outcomes, &a), Answer::Published { op: None });
    assert!(
        matches!(answer_of(&outcomes, &b), Answer::Published { op: Some(_) }),
        "{outcomes:?}"
    );
    assert_eq!(stored(paths, &machine, "note"), Some(json!("later")));

    // The earlier intent's record carries only its other cell: the peer wrote its note first.
    all(&stand, &stand.pass(false), Word::Live, None);
    stand.commons("[commons]\nenabled = false\n");
    let note = write::draft_set(paths, "lab/machines", "m7", &[("note", Some("both"))])
        .unwrap()
        .unwrap();
    let status = write::draft_set(paths, "lab/machines", "m7", &[("status", Some("planned"))])
        .unwrap()
        .unwrap();
    let a = held
        .write(
            &seam::owner(),
            aterm_file::Draft {
                born: false,
                revert: None,
                edits: note.edits.into_iter().chain(status.edits).collect(),
            },
            NOW,
        )
        .unwrap()
        .intent
        .id;
    peer("both");
    let (b, _) = wrote(
        paths,
        config,
        &["set", "lab/machines", "m7", "note", "after"],
    );
    let b = id_of(&b);
    std::fs::remove_file(config).unwrap();
    let outcomes = publish_pass(paths, config);
    assert!(
        matches!(answer_of(&outcomes, &a), Answer::Published { op: Some(_) }),
        "{outcomes:?}"
    );
    assert!(
        matches!(answer_of(&outcomes, &b), Answer::Published { op: Some(_) }),
        "{outcomes:?}"
    );
    assert_eq!(stored(paths, &machine, "status"), Some(json!("planned")));
    assert_eq!(stored(paths, &machine, "note"), Some(json!("after")));
    // The marker keeps what the intent left, so a later pass compares the same way.
    let done = held.outbox().done;
    let marker = done.iter().find(|done| done.intent.id == a).unwrap();
    assert_eq!(marker.left.len(), 2, "{marker:?}");
}

/// `revert` through one file never undoes another collection: an operation that changed an
/// account as well as a machine is held, with the reason, and nothing is signed.
///
/// NEGATIVE CONTROL, observed: without the collection check the revert is signed under
/// lab/machines: `held: published <operation>`.
#[test]
fn a_revert_through_one_file_never_undoes_another_collection() {
    let stand = Stand::new();
    all(&stand, &stand.pass(false), Word::Live, None);
    let (paths, config) = (stand.paths(), &stand.config);
    let ops = || -> std::collections::BTreeSet<String> {
        att::Handle::at(paths.clone(), &att::REGISTRY)
            .history()
            .unwrap()
            .into_iter()
            .map(|entry| aterm_codec::hex::encode_lower(&entry.operation))
            .collect()
    };
    let before = ops();
    Registry::at(paths.clone())
        .write(vec![vec![
            put(&MACHINE, &stand.machine, "note", json!("both at once")),
            put(&ACCOUNT, &stand.claude_ada, "realm", json!("claude")),
        ]])
        .unwrap();
    let both: Vec<String> = ops().difference(&before).cloned().collect();
    let [both] = both.as_slice() else {
        panic!("{both:?}");
    };
    all(&stand, &stand.pass(false), Word::Live, None);
    let (answer, code) = wrote(paths, config, &["revert", "lab/machines", both]);
    assert_eq!(code, aterm_file::exit::CONFLICT, "held: {answer}");
    assert!(answer.contains("changes more than"), "{answer}");
    assert!(carrying(paths, &id_of(&answer)).is_empty());
    assert_eq!(
        stored(paths, &stand.machine, "note"),
        Some(json!("both at once"))
    );
}

/// The history keeps which of this Mac's intents an operation carries, who said they made it and
/// the session it was made in, after the intent's done marker is gone: each pass carries them
/// forward from the history it wrote.
///
/// NEGATIVE CONTROL, observed: with the history re-derived from the done markers alone, the
/// entry loses its provenance once the markers go: the `contains(&id)` assertion fails.
#[test]
fn the_history_keeps_an_intents_provenance_past_its_marker() {
    let stand = Stand::new();
    all(&stand, &stand.pass(false), Word::Live, None);
    let (paths, config, root) = (stand.paths(), &stand.config, stand.root());
    let (answer, _) = wrote(
        paths,
        config,
        &["set", "lab/machines", "m7", "note", "kept"],
    );
    let op = op_of(&answer);
    let id = held_id(&stand, &op);
    let history = || verb(&root, &["history", "lab/machines", "--json"]);
    let entry = |text: String| -> String {
        let value: Value = aterm_json::from_str(&text).unwrap();
        let entries = value
            .get("operations")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        entries
            .into_iter()
            .find(|entry| entry.get("op").and_then(Value::as_str) == Some(op.as_str()))
            .map(|entry| entry.to_string())
            .unwrap_or_else(|| panic!("no entry for {op}: {text}"))
    };
    let first = entry(history());
    for word in [id.as_str(), "\"person\"", "\"s-test\""] {
        assert!(first.contains(word), "{word}: {first}");
    }
    // The markers' bound passed: every marker is gone, and the next pass projects again.
    std::fs::remove_file(root.done(&super::MACHINES.file())).unwrap();
    assert!(stand.file(&super::MACHINES).outbox().done.is_empty());
    all(&stand, &stand.pass(true), Word::Live, None);
    let after = entry(history());
    for word in [id.as_str(), "\"person\"", "\"s-test\""] {
        assert!(after.contains(word), "{word}: {after}");
    }
}

/// The id of the intent the operation `op` published, from its done marker.
#[track_caller]
fn held_id(stand: &Stand, op: &str) -> String {
    stand
        .file(&super::MACHINES)
        .outbox()
        .done
        .into_iter()
        .find(|done| done.op.as_deref() == Some(op))
        .map(|done| done.intent.id)
        .unwrap_or_else(|| panic!("no marker for {op}"))
}

/// A person's `outbox drop` of an intent a pass has read but not yet claimed wins: the pass
/// signs nothing for it and never says it was published.
///
/// NEGATIVE CONTROL, observed: with `File::claim` claiming an intent whatever the outbox says
/// now, the pass signs the dropped write and answers that it published it (the
/// `answer(&id).is_none()` assertion fails).
#[test]
fn a_drop_before_the_publishers_claim_leaves_nothing_signed() {
    let stand = Stand::new();
    all(&stand, &stand.pass(false), Word::Live, None);
    let (paths, config) = (stand.paths(), &stand.config);
    stand.commons("[commons]\nenabled = false\n");
    let (answer, _) = wrote(
        paths,
        config,
        &["set", "lab/machines", "m7", "note", "dropped"],
    );
    let id = id_of(&answer);
    std::fs::remove_file(config).unwrap();
    let file = stand.file(&super::MACHINES);
    let (dropping, dropped) = (file.clone(), id.clone());
    publish::seam::before_next_claim(move || {
        assert_eq!(
            dropping.drop_intent(&seam::owner(), &dropped).unwrap(),
            aterm_file::Dropped::Dropped
        );
    });
    let outcomes = publish_pass(paths, config);
    assert!(
        outcomes.iter().all(|outcome| outcome.answer(&id).is_none()),
        "{outcomes:?}"
    );
    assert_eq!(carrying(paths, &id).len(), 0);
    let outbox = file.outbox();
    assert_eq!(
        (outbox.queued.len(), outbox.held.len(), outbox.done.len()),
        (0, 0, 0)
    );
    assert_ne!(
        stored(paths, &stand.machine, "note"),
        Some(json!("dropped"))
    );
}

/// A pass whose vault wait is spent before it starts still reads a vault nobody holds: each
/// wait for a holder tries before its deadline, and the writer-generation walks, which wait on
/// no holder, run under the pass's own read bound. A pass whose read bound is spent says it ran
/// out of time as `custody`, never as a succession nobody made.
///
/// NEGATIVE CONTROLS (observed 2026-11-14): with `with_custody`'s first walk bounded by the
/// vault wait again, the first pass records every file `never` (`custody`), "ran out of time",
/// where it reads them `live`; with `Refusal::of` lacking its out-of-time arm, the spent pass
/// records them `never` (`succession`). With both, and the reading walk's conformance test
/// (`model_read_tests`) given no vault wait, that test stops at its first pass on "a word the
/// reading walk has no state for: (Never, Some(Succession))", the panic a loaded Mac gave it
/// at 50 ms; with neither, it passes with no vault wait at all.
#[test]
fn a_spent_vault_wait_reads_an_unheld_vault_and_a_spent_pass_is_out_of_time_not_a_succession() {
    let stand = Stand::new();
    all(
        &stand,
        &stand.pass_waiting(false, Duration::ZERO),
        Word::Live,
        None,
    );
    let spent = Stand::new();
    let outcomes = materialize::pass(
        spent.paths(),
        &spent.config,
        None,
        &Pass {
            read_by: Instant::now(),
            vault_wait: Duration::ZERO,
            again: false,
            now_ms: NOW,
        },
    );
    all(&spent, &outcomes, Word::Never, Some(Why::Custody));
    for outcome in &outcomes {
        assert!(
            outcome
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("ran out of time")),
            "{}: {:?}",
            outcome.name,
            outcome.detail
        );
    }
}

/// The face of `file` under `root`, as text.
fn face_of(file: &File) -> String {
    String::from_utf8(file.face_bytes().unwrap().expect("a face is there")).unwrap()
}

/// `face` with the cell of the row keyed `pk` under the header `column` set to `text`, and every
/// row keyed one of `drop` left out, as a tool that rewrites the whole file writes it. The cells
/// the test touches hold no tab and no line break, so a line is a row and a tab a cell.
fn rewritten(face: &str, pk: &str, column: &str, text: &str, drop: &[&str]) -> String {
    let mut lines = face.lines();
    let header: Vec<&str> = lines.next().unwrap().split('\t').collect();
    let at = header.iter().position(|cell| *cell == column).unwrap();
    let mut out = format!("{}\n", header.join("\t"));
    for line in lines {
        let mut cells: Vec<String> = line.split('\t').map(str::to_owned).collect();
        let key = cells.last().unwrap().clone();
        if drop.contains(&key.as_str()) {
            continue;
        }
        if key == pk {
            cells[at] = text.to_owned();
        }
        out.push_str(&cells.join("\t"));
        out.push('\n');
    }
    out
}

/// SF-06's exit test: an edited face changes nothing until a commit by name, and a row missing
/// from a committed face retires nothing.
///
/// A stand-in lab materializes `lab/machines` with two machines, `m7` and `m9`. A tool rewrites
/// the face whole: `m7`'s note changed and `m9`'s row left out. A materializer pass and a
/// publisher pass, then another writer's record and the pass that projects it, change nothing
/// of it: the table holds no new value, the outbox is empty, the snapshot moves on without the
/// edit, the face is left as it was and `status` says `face=edited`. `aterm file commit` then
/// queues and publishes exactly the one changed cell against the revision the face names (an
/// older one than the snapshot's by then), and `m9` stays live in the table, in the snapshot and
/// in the face the pass writes after. A face whose header lost `=pk@<rev>` and one naming a
/// revision whose baseline is gone are refused: nothing queued, the bytes kept as
/// `machines.rejected.tsv` and the face written again; a bad value queues nothing and names its
/// line and column in `machines.problems`; `restore` writes the face again, keeping the edits.
///
/// NEGATIVE CONTROL, observed: with the commit reading a missing row as the grid export does
/// (`Absent::Retires` in `face::plan`), the commit queues and publishes a second intent, `m9`'s
/// retirement: `` one row changed, one intent: published 68aa…532f published ed82…94d0 ``.
#[test]
fn an_edited_face_changes_nothing_until_committed_and_a_missing_row_retires_nothing() {
    let stand = Stand::new();
    all(&stand, &stand.pass(false), Word::Live, None);
    let (paths, config, root) = (stand.paths(), &stand.config, stand.root());
    let m7 = stand.machine.clone();
    let m9 = keys::machine_pk("m9");
    let (added, _) = wrote(
        paths,
        config,
        &["add", "lab/machines", "label=m9", "status=planned"],
    );
    op_of(&added);
    let machines = stand.file(&super::MACHINES);
    let status_line = || verb(&root, &["status", "lab/machines"]);
    let note = stored(paths, &m7, "note");
    let lifecycle = stored(paths, &m9, "row");
    assert_eq!(lifecycle, Some(json!("live")));
    let face = face_of(&machines);
    let rev = machines.status().rev();
    assert!(
        face.lines()
            .next()
            .unwrap()
            .ends_with(&format!("=pk@{rev}")),
        "{face}"
    );
    assert!(face.contains(&m9), "{face}");
    assert!(status_line().contains("face=written"), "{}", status_line());

    // A tool rewrites the face whole: one cell changed, one row left out.
    let edited = rewritten(&face, &m7, "note", "edited in the face", &[&m9]);
    std::fs::write(machines.face_path(), &edited).unwrap();
    assert!(status_line().contains("face=edited"), "{}", status_line());

    // Nothing reads it: passes of the materializer and the publisher, and another writer's
    // record with the pass that projects it, leave the table, the outbox and the face as they
    // were, and the snapshot moves on without the edit.
    all(&stand, &stand.pass(true), Word::Live, None);
    publish_pass(paths, config);
    Registry::at(paths.clone())
        .write(vec![vec![put(
            &MACHINE,
            &m7,
            "location",
            json!("the shelf"),
        )]])
        .unwrap();
    all(&stand, &stand.pass(false), Word::Live, None);
    publish_pass(paths, config);
    assert_eq!(stored(paths, &m7, "note"), note, "no commit, no write");
    assert_eq!(stored(paths, &m9, "row"), lifecycle);
    let outbox = machines.outbox();
    assert_eq!((outbox.queued.len(), outbox.held.len()), (0, 0));
    assert_eq!(
        face_of(&machines),
        edited,
        "a pass leaves an edited face alone"
    );
    assert!(machines.status().rev() > rev, "the snapshot moved on");
    assert_eq!(shown(&root, &m7, "location"), "the shelf");
    assert_ne!(shown(&root, &m7, "note"), "edited in the face");
    assert!(status_line().contains("face=edited"), "{}", status_line());

    // The commit by name: the one changed cell, against the revision the face names.
    let (answer, code) = wrote(paths, config, &["commit", "lab/machines"]);
    assert_eq!(code, aterm_file::exit::OK, "{answer}");
    assert_eq!(
        answer.lines().count(),
        1,
        "one row changed, one intent: {answer}"
    );
    op_of(&answer);
    assert_eq!(
        stored(paths, &m7, "note"),
        Some(json!("edited in the face"))
    );
    // The row the face left out is as it was: live in the table, the snapshot and the face.
    assert_eq!(stored(paths, &m9, "row"), lifecycle);
    assert_eq!(shown(&root, &m9, "row"), "live");
    // The other writer's cell, which the face showed as it was before, is theirs still.
    assert_eq!(stored(paths, &m7, "location"), Some(json!("the shelf")));
    let face = face_of(&machines);
    assert!(
        face.contains(&m9) && face.contains("edited in the face"),
        "{face}"
    );
    assert!(status_line().contains("face=written"), "{}", status_line());
    let (again, _) = wrote(paths, config, &["commit", "lab/machines"]);
    assert!(again.starts_with("unchanged: "), "{again}");

    // A face that cannot be read against a baseline is refused and kept, and the face written
    // again; nothing is queued. Each refused face is kept at a path of its own, never over an
    // earlier one.
    let kept_at = |n| root.rejected_at(&super::MACHINES.file(), "tsv", n);
    assert_eq!(kept_at(1), root.rejected(&super::MACHINES.file(), "tsv"));
    let mut kept = Vec::new();
    let rev = machines.status().rev();
    let header_cell = format!("=pk@{rev}");
    let fresh = face_of(&machines);
    for (broken, why) in [
        (fresh.replacen(&header_cell, "=pk", 1), "lost its last cell"),
        (fresh.replacen(&header_cell, "=pk@999", 1), "no longer kept"),
    ] {
        let broken = broken.replacen("edited in the face", "refused", 1);
        std::fs::write(machines.face_path(), &broken).unwrap();
        match run_verb(paths, config, &["commit", "lab/machines"], &env()) {
            Err(write::Refusal::Problem(said)) => assert!(said.contains(why), "{said}"),
            other => panic!("{other:?}"),
        }
        kept.push(broken.clone());
        for (n, bytes) in kept.iter().enumerate() {
            let n = u32::try_from(n).unwrap() + 1;
            assert_eq!(std::fs::read_to_string(kept_at(n)).unwrap(), *bytes);
        }
        assert_eq!(face_of(&machines), fresh, "the face is written again");
        assert_eq!(machines.outbox().queued.len(), 0);
        assert_eq!(
            stored(paths, &m7, "note"),
            Some(json!("edited in the face"))
        );
    }

    // A bad value queues nothing and says where; restore writes the face again and keeps it.
    let bad = rewritten(&fresh, &m9, "status", "not-a-status", &[]);
    std::fs::write(machines.face_path(), &bad).unwrap();
    match run_verb(paths, config, &["commit", "lab/machines"], &env()) {
        Err(write::Refusal::Problem(said)) => assert!(said.contains("1 problem"), "{said}"),
        other => panic!("{other:?}"),
    }
    let problems = std::fs::read_to_string(root.problems(&super::MACHINES.file())).unwrap();
    let line = fresh.lines().position(|row| row.ends_with(&m9)).unwrap() + 1;
    let column = fresh
        .lines()
        .next()
        .unwrap()
        .split('\t')
        .position(|cell| cell == "status");
    assert!(
        problems.starts_with(&format!("{line}:{}: status: ", column.unwrap() + 1)),
        "{problems}"
    );
    assert!(
        !problems.contains("not-a-status"),
        "never a value: {problems}"
    );
    assert_eq!(machines.outbox().queued.len(), 0);
    assert_eq!(
        face_of(&machines),
        bad,
        "a face with problems is left for its author"
    );
    let restored = verb(&root, &["restore", "lab/machines"]);
    assert!(restored.contains("kept at"), "{restored}");
    assert_eq!(std::fs::read_to_string(kept_at(3)).unwrap(), bad);
    assert_eq!(std::fs::read_to_string(kept_at(1)).unwrap(), kept[0]);
    assert_eq!(face_of(&machines), fresh);
    assert_eq!(stored(paths, &m9, "status"), Some(json!("planned")));
}

/// A face saved again after its commit asks for nothing again: an Editor tab's buffer still
/// holds what was committed, and a second save (or a commit run twice) must not repeat it. A new
/// row the face added is not born twice, and a cell whose write was held (another Mac moved it
/// since the face's revision) is not queued again beside the held write, though the read shows
/// that cell `⚠ mine ‖ theirs`, never as the face's text.
///
/// NEGATIVE CONTROL, observed: with a born row never checked against what this Mac already asked
/// for (`asked` in `face::plan` skipped), the stale save's commit queues the row again: `` a
/// stale save asks for nothing: published: the table already says what 2d34…6c10d writes ``.
#[test]
fn a_face_saved_again_after_its_commit_asks_for_nothing_again() {
    let stand = Stand::new();
    all(&stand, &stand.pass(false), Word::Live, None);
    let (paths, config, root) = (stand.paths(), &stand.config, stand.root());
    let m7 = stand.machine.clone();
    let machines = stand.file(&super::MACHINES);
    let face = face_of(&machines);
    let header: Vec<&str> = face.lines().next().unwrap().split('\t').collect();
    let mut row = vec![""; header.len()];
    row[header.iter().position(|cell| *cell == "label").unwrap()] = "m10";
    row[header.iter().position(|cell| *cell == "status").unwrap()] = "planned";
    let stale = format!(
        "{}{}\n",
        rewritten(&face, &m7, "note", "noted in the face", &[]),
        row.join("\t")
    );
    std::fs::write(machines.face_path(), &stale).unwrap();
    let (answer, code) = wrote(paths, config, &["commit", "lab/machines"]);
    assert_eq!(code, aterm_file::exit::OK, "{answer}");
    assert_eq!(
        answer.lines().count(),
        2,
        "a new row and a changed cell: {answer}"
    );
    let m10 = keys::machine_pk("m10");
    assert_eq!(stored(paths, &m10, "status"), Some(json!("planned")));
    assert_ne!(
        face_of(&machines),
        stale,
        "the commit writes the face again"
    );

    // The tab's buffer, saved again as it was.
    std::fs::write(machines.face_path(), &stale).unwrap();
    let (again, code) = wrote(paths, config, &["commit", "lab/machines"]);
    assert_eq!(code, aterm_file::exit::OK, "{again}");
    assert!(
        again.starts_with("unchanged: "),
        "a stale save asks for nothing: {again}"
    );
    let outbox = machines.outbox();
    assert_eq!((outbox.waiting().count(), outbox.held.len()), (0, 0));

    // A held cell: the face moves `location`, another Mac moved it first.
    all(&stand, &stand.pass(false), Word::Live, None);
    let fresh = face_of(&machines);
    let moved = rewritten(&fresh, &m7, "location", "the desk", &[]);
    Registry::at(paths.clone())
        .write(vec![vec![put(
            &MACHINE,
            &m7,
            "location",
            json!("the bench"),
        )]])
        .unwrap();
    std::fs::write(machines.face_path(), &moved).unwrap();
    let (held, code) = wrote(paths, config, &["commit", "lab/machines"]);
    assert_eq!(code, aterm_file::exit::CONFLICT, "{held}");
    assert_eq!(machines.outbox().held.len(), 1, "{held}");
    assert!(shown(&root, &m7, "location").starts_with(aterm_file::outbox::CONFLICT));
    std::fs::write(machines.face_path(), &moved).unwrap();
    let (again, _) = wrote(paths, config, &["commit", "lab/machines"]);
    assert!(
        again.starts_with("unchanged: "),
        "a held cell is not asked for twice: {again}"
    );
    let outbox = machines.outbox();
    assert_eq!((outbox.waiting().count(), outbox.held.len()), (0, 1));
    assert_eq!(stored(paths, &m7, "location"), Some(json!("the bench")));
}

/// `aterm file edit`: the face opens, and once its tab closes it is committed; a face with
/// problems is said and opened again at the first problem's line until a commit takes it. In an
/// aterm window a save in the tab has committed already, so the commit after the tab closes
/// finds nothing left and says what the tab's commit did: committed, or refused with the copy of
/// the face it kept.
#[test]
fn edit_reopens_a_face_at_its_problem_and_says_what_the_tabs_own_commit_did() {
    use std::cell::RefCell;
    let stand = Stand::new();
    all(&stand, &stand.pass(false), Word::Live, None);
    let (paths, config) = (stand.paths(), &stand.config);
    let m7 = stand.machine.clone();
    let machines = stand.file(&super::MACHINES);
    let fresh = face_of(&machines);
    let line = fresh.lines().position(|row| row.ends_with(&m7)).unwrap() + 1;

    // Round one leaves a bad value, round two the good one.
    let rounds = [
        rewritten(&fresh, &m7, "status", "not-a-status", &[]),
        rewritten(&fresh, &m7, "status", "planned", &[]),
    ];
    let opened = RefCell::new(Vec::new());
    let said = RefCell::new(Vec::new());
    let editor = |face: &std::path::Path, at: usize, wait: bool| {
        assert!(wait);
        let round = opened.borrow().len();
        opened.borrow_mut().push(at);
        std::fs::write(face, &rounds[round]).map_err(|error| error.to_string())
    };
    let say = |text: &str| said.borrow_mut().push(text.to_owned());
    let mut person = env();
    person.editor = &editor;
    person.say = &say;
    let (answer, code) = run_verb(paths, config, &["edit", "lab/machines"], &person).unwrap();
    assert_eq!(code, aterm_file::exit::OK, "{answer}");
    op_of(&answer);
    assert_eq!(
        *opened.borrow(),
        [1, line],
        "opened again at the problem's line"
    );
    assert_eq!(said.borrow().len(), 1, "{:?}", said.borrow());
    assert!(
        said.borrow()[0].contains("1 problem"),
        "{:?}",
        said.borrow()
    );
    assert_eq!(stored(paths, &m7, "status"), Some(json!("planned")));

    // The tab's own commit took the save: edit says so.
    all(&stand, &stand.pass(false), Word::Live, None);
    let fresh = face_of(&machines);
    let tab = |face: &std::path::Path, _: usize, _: bool| {
        let noted = rewritten(&fresh, &m7, "note", "noted in the tab", &[]);
        std::fs::write(face, noted).map_err(|error| error.to_string())?;
        run_verb(paths, config, &["commit", "lab/machines"], &env())
            .map(|_| ())
            .map_err(|refusal| format!("{refusal:?}"))
    };
    let mut window = env();
    window.editor = &tab;
    let (answer, code) = run_verb(paths, config, &["edit", "lab/machines"], &window).unwrap();
    assert_eq!(code, aterm_file::exit::OK, "{answer}");
    assert!(
        answer.contains("the Editor tab's save committed it"),
        "{answer}"
    );
    assert_eq!(stored(paths, &m7, "note"), Some(json!("noted in the tab")));

    // The tab's own commit refused the save: edit names the copy it kept.
    let fresh = face_of(&machines);
    let rev = machines.status().rev();
    let lost = fresh.replacen(&format!("=pk@{rev}"), "=pk", 1);
    let refused = |face: &std::path::Path, _: usize, _: bool| {
        std::fs::write(face, &lost).map_err(|error| error.to_string())?;
        let _ = run_verb(paths, config, &["commit", "lab/machines"], &env());
        Ok(())
    };
    window.editor = &refused;
    match run_verb(paths, config, &["edit", "lab/machines"], &window) {
        Err(write::Refusal::Problem(why)) => {
            assert!(
                why.contains("the Editor tab's commit refused the face"),
                "{why}"
            );
            let kept = machines.rejected_kept();
            let (copy, _) = kept.last().unwrap();
            assert!(why.contains(&copy.display().to_string()), "{why}");
            assert_eq!(std::fs::read_to_string(copy).unwrap(), lost);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(face_of(&machines), fresh, "the face is written again");
}

/// Two Writers whose Commons the membership log backs, as every lab Mac's is now, their registry
/// rolled to its second generation: m1 writes both machine rows and its own facts row, m2 its
/// own, and m1's roll, once m2 has declared the second generation, carries each facts row into
/// it as a signed record of the first. Mac 0 (m1) is the Mac whose files are read.
fn carried_lab() -> Lab {
    use crate::alab::upkeep::tests::{bound_facts, machine_row};
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    client::test_sign_genesis(
        &lab.paths[0],
        &lab._home.path().join("f18.genesis"),
        &lab.signer,
    )
    .unwrap();
    client::save_records(
        &lab.paths[1],
        &client::load_records(&lab.paths[0])
            .unwrap()
            .read_encoded()
            .unwrap(),
    )
    .unwrap();
    assert!(
        crate::table::Session::observe(&lab.bootstrap(0))
            .unwrap()
            .is_log_backed(),
        "CONTROL: the membership log backs Commons"
    );
    let (a, b) = (
        Registry::at(lab.paths[0].clone()),
        Registry::at(lab.paths[1].clone()),
    );
    a.open().unwrap();
    declare(&lab.paths[0], &att::TRACKER);
    a.write(vec![
        machine_row("m1"),
        machine_row("m2"),
        bound_facts(&lab, 0, "m1"),
    ])
    .unwrap();
    crate::table::tests::repair_commons(&lab, 6);
    b.write(vec![bound_facts(&lab, 1, "m2")]).unwrap();
    crate::table::tests::repair_commons(&lab, 6);
    let now_ms = || aterm_time::unix_millis().unwrap();
    let staged = a.roll(true, now_ms()).unwrap();
    assert!(!staged.sealed && staged.records > 0, "{staged:?}");
    crate::table::tests::repair_commons(&lab, 6);
    b.release(now_ms()).unwrap();
    crate::table::tests::repair_commons(&lab, 6);
    let rolled = a.roll(true, now_ms()).unwrap();
    assert!(rolled.sealed && rolled.waiting.is_empty(), "{rolled:?}");
    assert_eq!(a.generation().generation, 2);
    lab
}

/// One pass over Mac 0's files of `lab`, forced where `again`.
fn first_mac_pass(lab: &Lab, again: bool) -> Vec<Outcome> {
    materialize::pass(
        &lab.paths[0],
        &lab._home.path().join("aterm.toml"),
        None,
        &Pass {
            read_by: Instant::now() + aterm_spec::wait::MUST_HAPPEN,
            vault_wait: materialize::VAULT_WAIT,
            again,
            now_ms: NOW,
        },
    )
}

/// Every one of `outcomes` is `word` (`why`).
#[track_caller]
fn every(outcomes: &[Outcome], word: Word, why: Option<Why>) {
    assert_eq!(outcomes.len(), super::FILES.len());
    for outcome in outcomes {
        assert_eq!(
            (outcome.word, outcome.why),
            (word, why),
            "{}: {:?}",
            outcome.name,
            outcome.detail
        );
    }
}

/// The `seats` cell of the machine row `label` in Mac 0's `lab/machines` of `lab`.
fn seats_of(lab: &Lab, label: &str) -> String {
    let machines = File::at(&super::root(&lab.paths[0]), &super::MACHINES.file()).published();
    let row = machines.named(label);
    assert_eq!(row.len(), 1, "{label}");
    row[0].cell("seats").to_owned()
}

/// C1's exit test: a carried record that cannot be authenticated because a vault writer (one
/// landing a membership record) holds the vault's state lock while the pass reads is no record
/// that is not there. The pass refuses (`held`, `busy`), so the owner tries again within seconds,
/// and `lab/machines` keeps what the last pass that read the record showed: the peer's carried
/// facts row still gives m2's machine its `seats`. Once the writer lets go, the next pass is
/// `live` again with the same `seats`.
///
/// NEGATIVE CONTROL (2026-11-14): with `att::carried` authenticating each carried wire on
/// its own and leaving out one that fails (`let Ok(record) = session.authenticate(scope, wire)
/// else { continue }`), the pass with the lock held recorded `live`: `ai/accounts: None; left:
/// (Live, None) right: (Held, Some(Busy))`.
#[test]
fn a_carried_row_is_refused_never_dropped_while_membership_moves() {
    let lab = carried_lab();
    let peer = keys::uuid32(&lab.machine(1));
    every(&first_mac_pass(&lab, false), Word::Live, None);
    assert_eq!(
        seats_of(&lab, "m2"),
        peer,
        "CONTROL: the peer's carried facts give its machine its seats"
    );
    let writer = std::rc::Rc::new(std::cell::RefCell::new(None));
    seam::before_carried({
        let writer = std::rc::Rc::clone(&writer);
        let vault = lab.paths[0].dir.clone();
        move || {
            *writer.borrow_mut() = Some(
                aterm_uds::private_file::PrivateLock::take(
                    &vault,
                    ".lock",
                    Duration::ZERO,
                    "a vault writer",
                )
                .unwrap(),
            );
        }
    });
    let outcomes = first_mac_pass(&lab, true);
    let held = writer.borrow_mut().take();
    assert!(
        held.is_some(),
        "CONTROL: the pass reached the carried records"
    );
    drop(held);
    every(&outcomes, Word::Held, Some(Why::Busy));
    assert_eq!(
        seats_of(&lab, "m2"),
        peer,
        "the refused pass leaves the peer's seats as the last read showed them"
    );
    every(&first_mac_pass(&lab, true), Word::Live, None);
    assert_eq!(seats_of(&lab, "m2"), peer);
}

/// The signed record `id` as Mac `n` of `lab` stores it in the registry's first generation.
fn first_generation_wire(lab: &Lab, n: usize, id: &astream_wire::sync::Digest) -> Vec<u8> {
    let bootstrap = lab.bootstrap(n);
    let session = crate::table::Session::observe(&bootstrap).unwrap();
    let directory = crate::catalog::Directory::new(&lab.paths[n], bootstrap.trust());
    let scope = att::REGISTRY.descriptor(bootstrap.trust()).unwrap().scope;
    let store = directory
        .observe_store_if_exists(&session, scope)
        .unwrap()
        .unwrap();
    store.wire(&session, id).unwrap().unwrap().to_vec()
}

/// Rotate the membership epoch of `lab`, a lab of one Writer, as [`Lab::rotate`] does, and
/// return the rotation's node, for another copy of the lab to hear later ([`hear`]).
fn rotate_alone(lab: &Lab) -> Vec<u8> {
    use aterm_secrets::commons::{journal::Journal, roster::Grant};
    let admin = client::admin_open(&lab.paths[0]).unwrap();
    let bootstrap = lab.bootstrap(0);
    let mut journal = Journal::open_existing(bootstrap.journal_dir(), bootstrap.trust()).unwrap();
    let grants = vec![Grant::recorded(
        "m1".to_owned(),
        bootstrap.request().unwrap().binding().clone(),
        Role::Writer,
    )];
    let node = admin.prepare_commons_rotation(&journal, grants).unwrap();
    journal.ingest(node.bytes()).unwrap();
    node.bytes().to_vec()
}

/// Mac `n` of `lab` hears the rotation `node` another copy of the lab made.
fn hear(lab: &Lab, n: usize, node: &[u8]) {
    let bootstrap = lab.bootstrap(n);
    aterm_secrets::commons::journal::Journal::open_existing(
        bootstrap.journal_dir(),
        bootstrap.trust(),
    )
    .unwrap()
    .ingest(node)
    .unwrap();
}

/// One Writer's lab whose registry, in its second generation, holds in an archived row m1's
/// own facts row rebinding its machine to m9, sealed under an epoch only a copy of the lab holds
/// (the copy's control journal rotated alone, as a peer's does before this Mac hears of it), as
/// any Writer may write one. m1's carried facts row, which the roll carried, binds its machine
/// to m1. Returns the lab, the facts row's key, the operation the roll carried and the rotation
/// only the copy holds.
fn unheld_carried_lab() -> (Lab, String, astream_wire::sync::Digest, Vec<u8>) {
    use crate::alab::{
        registry::{ARCHIVED, MACHINE_FACTS},
        upkeep::tests::{bound_facts, machine_row},
    };
    let lab = Lab::new(&[Role::Writer]);
    let registry = Registry::at(lab.paths[0].clone());
    registry.open().unwrap();
    declare(&lab.paths[0], &att::TRACKER);
    registry
        .write(vec![
            machine_row("m1"),
            machine_row("m9"),
            bound_facts(&lab, 0, "m1"),
        ])
        .unwrap();
    let key = keys::uuid32(&lab.machine(0));
    let fork = lab.fork();
    let rotation = rotate_alone(&fork);
    let moved = Registry::at(fork.paths[0].clone());
    moved.write(vec![bound_facts(&fork, 0, "m9")]).unwrap();
    let unheld = moved.view().unwrap().owned[MACHINE_FACTS.name].rows[&key].operation;
    let wire = first_generation_wire(&fork, 0, &unheld);
    registry
        .roll(true, aterm_time::unix_millis().unwrap())
        .unwrap();
    assert_eq!(registry.generation().generation, 2);
    let carried = registry.view().unwrap().owned[MACHINE_FACTS.name].rows[&key].operation;
    let archived = |col: &str, x: Value| {
        crate::typed::cell::change(
            &ARCHIVED,
            crate::typed::cell::CellKey {
                pk: &keys::archived_pk(&unheld),
                col,
                elem: None,
            },
            &crate::typed::cell::CellValue::owner(1, x),
        )
        .unwrap()
    };
    registry
        .write(vec![vec![
            archived("generation", json!(1)),
            archived("wire", json!(aterm_secrets::b64::encode_padded(&wire))),
        ]])
        .unwrap();
    (lab, key, carried, rotation)
}

/// A carried record whose roster the history this Mac holds cannot open is left out, and the
/// file stays `live`: that omission is decided by the record's bytes and the history held here
/// ([`aterm_secrets::ErrorKind::HistoryUnopened`]), and the same pass over the same history
/// leaves it out again, so it is no refusal ([`unheld_carried_lab`]).
///
/// NEGATIVE CONTROL (2026-11-14): with `Session::authenticate_carried` refusing a roster this Mac
/// cannot open as it refuses any other failure, the registry's own view refused before any pass:
/// `called Result::unwrap() on an Err value: Error("roster is outside the selected control
/// history")`.
#[test]
fn a_carried_row_whose_roster_this_mac_cannot_open_is_left_out_and_the_file_is_live() {
    use crate::alab::registry::MACHINE_FACTS;
    let (lab, key, carried, _) = unheld_carried_lab();
    let registry = Registry::at(lab.paths[0].clone());
    let facts = registry.view().unwrap().owned[MACHINE_FACTS.name].rows[&key].clone();
    assert_eq!(
        facts.operation, carried,
        "the record sealed under the epoch held only by the copy is left out"
    );
    every(&first_mac_pass(&lab, false), Word::Live, None);
    assert_eq!(seats_of(&lab, "m1"), key);
    assert_eq!(seats_of(&lab, "m9"), "");
}

/// What the last pass left of each shared file over `paths`: its snapshot's content digest (hex)
/// and revision, and the history beside it.
type Left = std::collections::BTreeMap<&'static str, (Option<(String, u64)>, Option<String>)>;

fn left_by(paths: &Paths) -> Left {
    let root = super::root(paths);
    super::FILES
        .iter()
        .map(|file| {
            let snapshot = File::at(&root, &file.file())
                .published()
                .snapshot
                .map(|snapshot| {
                    (
                        aterm_codec::hex::encode_lower(&snapshot.content()),
                        snapshot.rev,
                    )
                });
            let history = std::fs::read_to_string(root.history(&file.file())).ok();
            (file.name, (snapshot, history))
        })
        .collect()
}

/// One step of a walk: its name, what an unforced pass left, and what a forced pass then left.
type Step = (&'static str, Left, Left);

/// An unforced pass over the files of `paths`, which records each file `live`, then a forced
/// one: what each left.
#[track_caller]
fn unforced_then_forced(paths: &Paths, config: &std::path::Path, step: &'static str) -> Step {
    unforced_then_forced_at(paths, config, step, NOW)
}

/// [`unforced_then_forced`], both passes on the clock `now_ms`.
#[track_caller]
fn unforced_then_forced_at(
    paths: &Paths,
    config: &std::path::Path,
    step: &'static str,
    now_ms: u64,
) -> Step {
    let pass = |again| {
        materialize::pass(
            paths,
            config,
            None,
            &Pass {
                read_by: Instant::now() + aterm_spec::wait::MUST_HAPPEN,
                vault_wait: materialize::VAULT_WAIT,
                again,
                now_ms,
            },
        )
    };
    for outcome in pass(false) {
        assert!(outcome.problem.is_none(), "{step}: {:?}", outcome.problem);
        assert_eq!(
            (outcome.word, outcome.why),
            (Word::Live, None),
            "{step}: {}: {:?}",
            outcome.name,
            outcome.detail
        );
    }
    let unforced = left_by(paths);
    pass(true);
    (step, unforced, left_by(paths))
}

/// The step after `before` moved what the files hold: its forced pass left other than the one
/// before. Records the step in `steps`.
#[track_caller]
fn moved(steps: &mut Vec<Step>, step: Step) {
    let before = &steps.last().expect("a first step").2;
    assert_ne!(
        &step.2, before,
        "CONTROL: {} moves what the files hold",
        step.0
    );
    steps.push(step);
}

/// The law of each walk: in every step of `walk`, run as is, an unforced pass leaves what a
/// forced one would. With each `(part, step)` of `parts` left out of the key (or, for
/// `Unkeyed::History`, out of the skip) by `seam::leave_out` (the walk run again on a thread of
/// its own, whose seams start unset), exactly that step parts the two: the NEGATIVE CONTROL that
/// the part is needed, run with the law.
#[track_caller]
fn walked(walk: fn() -> Vec<Step>, parts: &[(materialize::Unkeyed, &'static str)]) {
    let apart = |steps: &[Step]| -> Vec<&'static str> {
        steps
            .iter()
            .filter(|(_, unforced, forced)| unforced != forced)
            .map(|(step, ..)| *step)
            .collect()
    };
    let steps = walk();
    assert_eq!(
        apart(&steps),
        Vec::<&str>::new(),
        "an unforced pass leaves what a forced one would: {:#?}",
        steps
            .iter()
            .find(|(_, unforced, forced)| unforced != forced)
    );
    for &(part, step) in parts {
        let steps = std::thread::spawn(move || {
            seam::leave_out(part);
            walk()
        })
        .join()
        .expect("the walk ran");
        assert_eq!(
            apart(&steps),
            [step],
            "NEGATIVE CONTROL: with {part:?} left out, the unforced pass of `{step}` alone \
             leaves other than the forced one"
        );
    }
}

/// C2's exit test: a pass projects again exactly where what its projection reads moved, so an
/// ordinary pass leaves what `aterm file refresh` would ([`walked`]). Each step of the stand's
/// walk moves one input of the projection: a record arrives (the frontier); an account is
/// relabelled (the registry's frontier, which is the frontier of `ai/limits` too, so the view
/// names the account by its new slug as `ai/accounts` does); the members bind this Mac's actor
/// to another machine, so `=here` leaves m7 (`me`); a later build's guards refuse a stored text
/// the build before showed (the rules); the publisher's done marker for an operation the
/// snapshot already holds says whose intent it carries (the provenance); the history file is
/// removed (the skip's history check); the snapshot is replaced by a whole one at its revision
/// that leaves a row out (the skip's check of the snapshot's content, recorded in the status as
/// `snapshot`). Each step is a CONTROL too: what the files hold moves. The binding moves by a
/// test's seam (`seam::bound_to`): a deny of this Mac's machine in the swarm roster revokes it,
/// and a revoked Mac's pass is `held`. What the carried records authenticate to and how the log
/// judges its records are walked on labs of their own
/// ([`a_rotation_that_opens_a_carried_record_is_projected_by_the_next_pass`],
/// [`a_remove_that_makes_a_cutoff_count_is_projected_by_the_next_pass`]), and so are the
/// `ai/usage` snapshot a view folds and the members it folds by
/// ([`a_reading_whose_span_passed_leaves_the_limits_view_by_the_next_pass`],
/// [`a_member_that_leaves_gives_way_in_the_limits_view_by_the_next_pass`]).
///
/// NEGATIVE CONTROLS, run with the law ([`walked`]), each part needed at its own step; first
/// seen 2026-11-14 as `lab/machines` at revision 3 against 4 (`Rules`), 2 against 3 (`Me`), the
/// same content at revision 5 with the histories apart (`Provenance`), and `None` against the
/// history written again (`History`); and, seen 2026-11-15 with the snapshot's content left out
/// of the skip (`Snapshot`), the unforced pass after the replacement serves it as it found it:
/// `lab/machines` at revision 5, its content without m8's row, against the forced pass's
/// revision 6 with it, the histories alike.
#[test]
fn an_unforced_pass_leaves_what_a_forced_one_would() {
    use materialize::Unkeyed::{History, Me, Provenance, Rules, Snapshot};
    walked(
        stand_walk,
        &[
            (Me, "this Mac is bound to another machine"),
            (Rules, "a later build"),
            (Provenance, "the done marker"),
            (History, "the history removed"),
            (Snapshot, "the snapshot replaced at its revision"),
        ],
    );
}

/// [`an_unforced_pass_leaves_what_a_forced_one_would`]'s walk.
fn stand_walk() -> Vec<Step> {
    use crate::alab::upkeep::tests::{bound_facts, machine_row};
    let stand = Stand::new();
    let (paths, config) = (stand.paths(), &stand.config);
    let registry = Registry::at(paths.clone());
    // This Mac's facts bind its machine to m7: `=here` names m7.
    registry
        .write(vec![bound_facts(&stand.lab, SEAT, "m7")])
        .unwrap();
    let mut steps = vec![unforced_then_forced(paths, config, "the first pass")];
    let here = || shown(&stand.root(), &stand.machine, "here");
    assert_eq!(here(), "yes");

    registry.write(vec![machine_row("m8")]).unwrap();
    moved(
        &mut steps,
        unforced_then_forced(paths, config, "a record arrives"),
    );

    // An account is relabelled: its slug moves in `ai/accounts`, and in `ai/limits`, which
    // names accounts by the registry's read.
    let slugs = || {
        let of = |file: &Type, column: &str| {
            stand
                .file(file)
                .read()
                .named(&stand.claude_ada)
                .first()
                .map(|row| row.cell(column).to_owned())
                .unwrap_or_default()
        };
        (
            of(&super::ACCOUNTS, "slug"),
            of(&super::LIMITS, "account_slug"),
        )
    };
    let before = slugs();
    assert_eq!(before.0, before.1, "CONTROL: the view names the account");
    registry
        .write(vec![vec![put(
            &ACCOUNT,
            &stand.claude_ada,
            "label",
            json!("desk"),
        )]])
        .unwrap();
    moved(
        &mut steps,
        unforced_then_forced(paths, config, "an account relabelled"),
    );
    let after = slugs();
    assert_ne!(after.0, before.0, "the account's slug moved");
    assert_eq!(
        after.0, after.1,
        "the view names the account as `ai/accounts` does"
    );

    // The members bind this Mac's actor to Mac 0's machine, which no facts row binds.
    seam::bound_to(stand.lab.machine(0));
    moved(
        &mut steps,
        unforced_then_forced(paths, config, "this Mac is bound to another machine"),
    );
    assert_eq!(here(), "", "`=here` left m7");

    seam::new_build("m8");
    moved(
        &mut steps,
        unforced_then_forced(paths, config, "a later build"),
    );

    // The publisher is killed once it has signed: the record arrives with no done marker.
    publish::seam::kill_at(publish::seam::Stop::AfterSign);
    let (answer, _) = wrote(
        paths,
        config,
        &["set", "lab/machines", "m7", "note", "kept"],
    );
    let id = id_of(&answer);
    assert_eq!(carrying(paths, &id).len(), 1, "the record is signed");
    moved(
        &mut steps,
        unforced_then_forced(paths, config, "the intent's record arrives"),
    );
    let machines = stand.file(&super::MACHINES);
    assert!(machines.outbox().done.is_empty());
    let _ = publish_pass(paths, config);
    assert_eq!(
        (carrying(paths, &id).len(), machines.outbox().done.len()),
        (1, 1),
        "CONTROL: the publisher found the record and wrote its marker, signing nothing"
    );
    moved(
        &mut steps,
        unforced_then_forced(paths, config, "the done marker"),
    );
    let history = || std::fs::read_to_string(stand.root().history(&super::MACHINES.file()));
    let written = history().unwrap();
    assert!(written.contains(&id), "{written}");

    std::fs::remove_file(stand.root().history(&super::MACHINES.file())).unwrap();
    let step = unforced_then_forced(paths, config, "the history removed");
    assert_eq!(
        step.2,
        steps.last().unwrap().2,
        "the history is written again as it was"
    );
    steps.push(step);

    // The snapshot is replaced by one at its revision that says otherwise, whole (its own end
    // digest holds), as a copy put back or another hand would leave it.
    let snap = stand.root().snapshot(&super::MACHINES.file());
    let mut swapped = aterm_file::Snapshot::decode(&std::fs::read(&snap).unwrap()).unwrap();
    let rev = swapped.rev;
    let m8 = keys::machine_pk("m8");
    let at = swapped
        .rows
        .iter()
        .position(|row| row.pk == m8)
        .expect("m8's row");
    swapped.rows.remove(at);
    std::fs::write(&snap, swapped.encode()).unwrap();
    assert_eq!(
        stand
            .file(&super::MACHINES)
            .published()
            .snapshot
            .map(|snapshot| snapshot.rev),
        Some(rev),
        "CONTROL: the replaced snapshot reads, at its revision"
    );
    moved(
        &mut steps,
        unforced_then_forced(paths, config, "the snapshot replaced at its revision"),
    );
    steps
}

/// A file of the journal ring shows a row its window no longer holds until the row's span
/// passes ([`aterm_file::retain::carry`]), and this Mac's clock moves that with no record: the
/// first unforced pass after an event's 30 days passed projects `ai/events` without it, as a
/// forced one would ([`walked`]). The stand's event is first seen by the pass at [`NOW`]; an
/// hour before its span ends the window has moved on four weeks, holds no table, and the event
/// is carried; an hour after, in the same week, so under the same window and frontier, it has
/// left. The `ai/logins` and `ai/usage` rows, whose span is 120 days, are carried through both.
///
/// NEGATIVE CONTROL, run with the law (first run 2026-11-15): with the aged rows left out of the
/// key (`Unkeyed::Aged`), the step `the event's span passed` alone leaves the unforced pass other
/// than the forced one.
#[test]
fn an_event_whose_span_passed_is_projected_out_by_the_next_pass() {
    walked(
        || {
            let stand = Stand::new();
            let (paths, config) = (stand.paths(), &stand.config);
            let span = aterm_file::retain::LOG.age_ms;
            let hour = 3_600_000;
            let rows = |file: &Type| {
                stand
                    .file(file)
                    .published()
                    .snapshot
                    .map_or(0, |snapshot| snapshot.rows.len())
            };
            let events = || rows(&super::EVENTS);
            let mut steps = vec![unforced_then_forced(paths, config, "the event read")];
            assert_eq!(events(), 1, "CONTROL: the window holds the stand's event");
            let (before, after) = (NOW + span - hour, NOW + span + hour);
            let week = |ms: u64| crate::catalog::ring::JOURNAL.epoch_of(ms / 1000);
            assert_eq!(
                week(before),
                week(after),
                "CONTROL: both clocks are in one week, so one window"
            );
            moved(
                &mut steps,
                unforced_then_forced_at(paths, config, "the event carried", before),
            );
            assert_eq!(events(), 1, "CONTROL: the moved window carries the event");
            moved(
                &mut steps,
                unforced_then_forced_at(paths, config, "the event's span passed", after),
            );
            assert_eq!(events(), 0, "the event left once its span passed");
            for file in [&super::LOGINS, &super::USAGE] {
                assert_eq!(rows(file), 1, "{}: carried still", file.name);
            }
            steps
        },
        &[(materialize::Unkeyed::Aged, "the event's span passed")],
    );
}

/// A view folds the `ai/usage` snapshot, which this Mac's clock moves with no record: once a
/// reading's 120 days pass, the next unforced pass projects `ai/usage` without it and
/// `ai/limits` without the account's row, as a forced one would ([`walked`]). The stand's
/// reading is first seen by the pass at [`NOW`]; an hour before its span ends the window has
/// moved on, holds no table, and the reading is carried and folded; an hour after, in the same
/// week, so under the same window and frontier, it and its fold have left.
///
/// NEGATIVE CONTROLS, run with the law (first run 2026-11-15): with the aged rows left out of
/// the ring files' key (`Unkeyed::Aged`), or the content of the `ai/usage` snapshot left out of
/// the view's (`Unkeyed::Usage`), the step `the reading's span passed` alone leaves the
/// unforced pass other than the forced one.
#[test]
fn a_reading_whose_span_passed_leaves_the_limits_view_by_the_next_pass() {
    walked(
        || {
            let stand = Stand::new();
            let (paths, config) = (stand.paths(), &stand.config);
            let span = aterm_file::retain::MINE.age_ms;
            let hour = 3_600_000;
            let rows = || {
                [&super::USAGE, &super::LIMITS].map(|file| {
                    stand
                        .file(file)
                        .published()
                        .snapshot
                        .map_or(0, |snapshot| snapshot.rows.len())
                })
            };
            let mut steps = vec![unforced_then_forced(paths, config, "the reading read")];
            assert_eq!(
                rows(),
                [1, 1],
                "CONTROL: the view folds the stand's reading"
            );
            let (before, after) = (NOW + span - hour, NOW + span + hour);
            let week = |ms: u64| crate::catalog::ring::JOURNAL.epoch_of(ms / 1000);
            assert_eq!(
                week(before),
                week(after),
                "CONTROL: both clocks are in one week, so one window"
            );
            moved(
                &mut steps,
                unforced_then_forced_at(paths, config, "the reading carried", before),
            );
            assert_eq!(
                rows(),
                [1, 1],
                "CONTROL: the moved window carries the reading"
            );
            moved(
                &mut steps,
                unforced_then_forced_at(paths, config, "the reading's span passed", after),
            );
            assert_eq!(
                rows(),
                [0, 0],
                "the reading and its fold left once its span passed"
            );
            steps
        },
        &[
            (materialize::Unkeyed::Aged, "the reading's span passed"),
            (materialize::Unkeyed::Usage, "the reading's span passed"),
        ],
    );
}

/// A view takes a current member's reading over its fenced predecessor's where the registry
/// binds both uuids to one Mac (`super::limits::Seats`), and who the current members are moves
/// with no record of any table: once the peer, whose facts row binds its uuid to this Mac's
/// machine, is no current member, the next unforced pass folds this Mac's reading alone, as a
/// forced one would ([`walked`]). The members move by a test's seam (`seam::leaves`); the
/// peer's facts row and reading stand, as a fenced predecessor's do.
///
/// NEGATIVE CONTROL, run with the law (first run 2026-11-15): with the members left out of the
/// view's key (`Unkeyed::Members`), the step `the peer is no member` alone leaves the unforced
/// pass other than the forced one.
#[test]
fn a_member_that_leaves_gives_way_in_the_limits_view_by_the_next_pass() {
    walked(
        members_walk,
        &[(materialize::Unkeyed::Members, "the peer is no member")],
    );
}

/// [`a_member_that_leaves_gives_way_in_the_limits_view_by_the_next_pass`]'s walk.
fn members_walk() -> Vec<Step> {
    use crate::alab::upkeep::tests::bound_facts;
    const PEER: usize = 0;
    let stand = Stand::new();
    let lab = &stand.lab;
    let (paths, config) = (stand.paths(), &stand.config);
    let t = NOW / 1000;
    let clocks = [t; 2];
    // Each Mac's facts row binds its uuid to m7: the registry binds both to one Mac, as it binds
    // a fenced predecessor's uuid and its successor's.
    Registry::at(paths.clone())
        .write(vec![bound_facts(lab, SEAT, "m7")])
        .unwrap();
    repair_commons_among(lab, 6, &[PEER, SEAT], &clocks);
    Registry::at(lab.paths[PEER].clone())
        .write(vec![bound_facts(lab, PEER, "m7")])
        .unwrap();
    // The peer reads the stand's account in the same week: 60% against this Mac's 40%.
    let peer = uuid_of(lab, PEER);
    assert_eq!(
        peer,
        keys::uuid32(&lab.machine(PEER)),
        "CONTROL: the peer's reading is at the uuid its facts row binds"
    );
    state_ring(
        lab,
        PEER,
        Stand::ring_rows(&stand.claude_ada, &peer, 600, RESET, 1),
        t,
    );
    repair_commons_among(lab, 6, &[PEER, SEAT], &clocks);
    let limit = || {
        let read = stand.file(&super::LIMITS).read();
        let row = read.named(&stand.claude_ada);
        assert_eq!(row.len(), 1, "{:?}", read.rows());
        (
            row[0].cell("readers").to_owned(),
            row[0].cell("w7d").to_owned(),
        )
    };
    let mut steps = vec![unforced_then_forced(paths, config, "both members read")];
    assert_eq!(
        limit(),
        ("2".to_owned(), "600".to_owned()),
        "CONTROL: both members' readings fold, the larger percent standing"
    );
    seam::leaves(lab.machine(PEER));
    moved(
        &mut steps,
        unforced_then_forced(paths, config, "the peer is no member"),
    );
    assert_eq!(
        limit(),
        ("1".to_owned(), "400".to_owned()),
        "this Mac's reading takes the place of its predecessor's"
    );
    steps
}

/// A view never folds without the registry it names accounts and seats by: where the
/// registry's read refuses once the ring has read (here busy: a vault writer takes the vault's
/// state lock as the registry's read is about to authenticate the records its roll carried),
/// `ai/limits` refuses with it and keeps what the last pass that read the registry showed, the
/// account's slug included, rather than record a fold with no names `live`. Once the writer
/// lets go, the next pass is `live` again with the same rows.
///
/// NEGATIVE CONTROL (2026-11-15): with a registry that refuses taken for none, as the merge
/// first had it (`projector(&att::REGISTRY).ok()`, the names left blank), the busy pass recorded
/// the view `live`: `left: (Live, None) right: (Held, Some(Busy))`.
#[test]
fn a_registry_that_refuses_refuses_the_limits_view_which_keeps_its_names() {
    let lab = carried_lab();
    let address = keys::address_pk(ADA).unwrap();
    let account = keys::account_pk("claude", ADA).unwrap();
    Registry::at(lab.paths[0].clone())
        .write(vec![vec![
            put(&ADDRESS, &address, "addr", json!(ADA)),
            put(&ACCOUNT, &account, "provider", json!("anthropic")),
            put(&ACCOUNT, &account, "realm", json!("claude")),
            put(&ACCOUNT, &account, "login", json!(address)),
            put(&ACCOUNT, &account, "status", json!("active")),
        ]])
        .unwrap();
    state_ring(
        &lab,
        0,
        Stand::ring_rows(&account, &uuid_of(&lab, 0), 400, RESET, 1),
        NOW / 1000,
    );
    every(&first_mac_pass(&lab, false), Word::Live, None);
    let root = super::root(&lab.paths[0]);
    let limits = || {
        let snapshot = File::at(&root, &super::LIMITS.file())
            .published()
            .snapshot
            .expect("a snapshot of the view");
        let slug = snapshot
            .rows
            .iter()
            .find(|row| row.pk == account)
            .map(|row| row.cell("account_slug").to_owned());
        (snapshot.rev, snapshot.content(), slug)
    };
    let shown = limits();
    assert!(
        shown.2.as_deref().is_some_and(|slug| !slug.is_empty()),
        "CONTROL: the view names the account: {shown:?}"
    );
    let over_limits = || {
        materialize::pass(
            &lab.paths[0],
            &lab._home.path().join("aterm.toml"),
            Some(&super::LIMITS.file()),
            &Pass {
                read_by: Instant::now() + aterm_spec::wait::MUST_HAPPEN,
                vault_wait: materialize::VAULT_WAIT,
                again: false,
                now_ms: NOW,
            },
        )
    };
    let writer = std::rc::Rc::new(std::cell::RefCell::new(None));
    seam::before_carried({
        let writer = std::rc::Rc::clone(&writer);
        let vault = lab.paths[0].dir.clone();
        move || {
            *writer.borrow_mut() = Some(
                aterm_uds::private_file::PrivateLock::take(
                    &vault,
                    ".lock",
                    Duration::ZERO,
                    "a vault writer",
                )
                .unwrap(),
            );
        }
    });
    let busy = over_limits();
    let held = writer.borrow_mut().take();
    assert!(
        held.is_some(),
        "CONTROL: the registry's read reached the carried records"
    );
    drop(held);
    assert_eq!(busy.len(), 1);
    assert_eq!(
        (busy[0].word, busy[0].why),
        (Word::Held, Some(Why::Busy)),
        "{:?}",
        busy[0].detail
    );
    assert_eq!(
        limits(),
        shown,
        "the refused pass leaves the view as the last read showed it"
    );
    let again = over_limits();
    assert_eq!(
        (again[0].word, again[0].why),
        (Word::Live, None),
        "{:?}",
        again[0].detail
    );
    assert_eq!(limits(), shown);
}

/// A roster that does not read leaves this Mac's machine and the members unknown: each file
/// that reads them refuses (`lab/machines`, for `=here`; `ai/limits`, for the members it folds
/// by) and keeps its snapshot, while the files whose projection reads neither (`ai/accounts`,
/// `ai/links` and the journal ring's) are projected and stay `live`. Once the roster reads,
/// every file is `live`.
///
/// NEGATIVE CONTROLS (2026-11-15): with the whole pass refused where the roster does not read,
/// as the merge first had it, the ring's files were held with the rest: `ai/logins`, `left:
/// (Held, Some(Membership)) right: (Live, None)`; with every catalog table's file reading the
/// roster in `read_files`, as the amended merge had it, `ai/accounts` was held: `left: (Held,
/// Some(Membership)) right: (Live, None)`; and with `Inputs::read` reading a catalog table's
/// family only where the roster read, as it also had it, `ai/accounts` was held for its family
/// unread (`the table was not read`): `left: (Held, Some(Custody)) right: (Live, None)`.
#[test]
fn a_roster_that_does_not_read_holds_only_the_files_that_read_it() {
    let stand = Stand::new();
    all(&stand, &stand.pass(false), Word::Live, None);
    let revs = || {
        super::FILES
            .iter()
            .map(|file| stand.file(file).status().rev())
            .collect::<Vec<_>>()
    };
    let before = revs();
    seam::roster_unread();
    let outcomes = stand.pass(true);
    seam::roster_reads();
    assert_eq!(outcomes.len(), super::FILES.len());
    for (file, outcome) in super::FILES.iter().zip(&outcomes) {
        let expected = if [super::MACHINES.name, super::LIMITS.name].contains(&file.name) {
            (Word::Held, Some(Why::Membership))
        } else {
            (Word::Live, None)
        };
        assert_eq!(
            (outcome.word, outcome.why),
            expected,
            "{}: {:?}",
            outcome.name,
            outcome.detail
        );
    }
    assert_eq!(
        revs(),
        before,
        "every snapshot stays as the last pass wrote it"
    );
    all(&stand, &stand.pass(false), Word::Live, None);
}

/// What the carried records authenticate to moves with no record of the registry: once this Mac
/// hears the rotation its copy made, the record the archived row carries under that epoch is
/// read ([`unheld_carried_lab`]), and the next unforced pass projects it, as a forced one would
/// ([`walked`]; the C2 exit test's step for the resolution,
/// [`an_unforced_pass_leaves_what_a_forced_one_would`]).
///
/// NEGATIVE CONTROL, run with the law: the resolution left out of the key, the pass after the
/// rotation projects nothing (first seen 2026-11-14 as `lab/machines` at revision 1 against 2).
#[test]
fn a_rotation_that_opens_a_carried_record_is_projected_by_the_next_pass() {
    walked(
        || {
            let (lab, key, _, rotation) = unheld_carried_lab();
            let (paths, config) = (&lab.paths[0], lab._home.path().join("aterm.toml"));
            let mut steps = vec![unforced_then_forced(paths, &config, "the record unheld")];
            assert_eq!(seats_of(&lab, "m9"), "");
            hear(&lab, 0, &rotation);
            moved(
                &mut steps,
                unforced_then_forced(paths, &config, "the rotation heard"),
            );
            assert_eq!(
                (seats_of(&lab, "m1"), seats_of(&lab, "m9")),
                (String::new(), key)
            );
            steps
        },
        &[(materialize::Unkeyed::Resolved, "the rotation heard")],
    );
}

/// How the log judges its records moves with no record of the registry: a cutoff counts only
/// once the reader's log shows its writer removed. Two Writers whose Commons the membership log
/// backs: m1 writes the machine rows and its facts, m2 its facts binding its machine to m2;
/// m1 signs its cutoff of m2 in each table it holds, as every removal path does before its
/// `Remove`; m2, which has not heard of it, rebinds its machine to m9, and the record arrives.
/// An unforced pass shows m9's `seats` (the cutoff alone counts for nothing). m1's log then takes
/// the `Remove`, which no table holds: the next unforced pass leaves m2's record past the cutoff
/// out, as a forced one would ([`walked`]; the C2 exit test's step for the judgment,
/// [`an_unforced_pass_leaves_what_a_forced_one_would`]).
///
/// NEGATIVE CONTROL, run with the law: the judgment left out of the key, the pass after the
/// `Remove` projects nothing (first seen 2026-11-14 as `lab/machines` at revision 1 against 2).
#[test]
fn a_remove_that_makes_a_cutoff_count_is_projected_by_the_next_pass() {
    walked(
        remove_walk,
        &[(materialize::Unkeyed::Judgment, "m2 removed")],
    );
}

/// [`a_remove_that_makes_a_cutoff_count_is_projected_by_the_next_pass`]'s walk.
fn remove_walk() -> Vec<Step> {
    use crate::alab::upkeep::tests::{bound_facts, machine_row};
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    client::test_sign_genesis(
        &lab.paths[0],
        &lab._home.path().join("f18.genesis"),
        &lab.signer,
    )
    .unwrap();
    client::save_records(
        &lab.paths[1],
        &client::load_records(&lab.paths[0])
            .unwrap()
            .read_encoded()
            .unwrap(),
    )
    .unwrap();
    let (a, b) = (
        Registry::at(lab.paths[0].clone()),
        Registry::at(lab.paths[1].clone()),
    );
    a.open().unwrap();
    declare(&lab.paths[0], &att::TRACKER);
    a.write(vec![
        machine_row("m1"),
        machine_row("m2"),
        machine_row("m9"),
        bound_facts(&lab, 0, "m1"),
    ])
    .unwrap();
    crate::table::tests::repair_commons(&lab, 6);
    b.write(vec![bound_facts(&lab, 1, "m2")]).unwrap();
    crate::table::tests::repair_commons(&lab, 6);
    let m2 = crate::table::Session::observe(&lab.bootstrap(1))
        .unwrap()
        .actor();
    assert!(
        crate::table::store::cut_removed_writer(&lab.paths[0], &m2, client::CutPass::BeforeRemove)
            > 0,
        "m1 signs its cutoff of m2"
    );
    b.write(vec![bound_facts(&lab, 1, "m9")]).unwrap();
    crate::table::tests::repair_commons(&lab, 6);
    let key = keys::uuid32(&lab.machine(1));
    let (paths, config) = (&lab.paths[0], lab._home.path().join("aterm.toml"));
    let mut steps = vec![unforced_then_forced(
        paths,
        &config,
        "m2's record past the cutoff arrives",
    )];
    assert_eq!(
        (seats_of(&lab, "m2"), seats_of(&lab, "m9")),
        (String::new(), key),
        "CONTROL: a cutoff whose writer is not removed counts for nothing"
    );
    client::remove_member_in_log(&lab.paths[0], m2).unwrap();
    moved(
        &mut steps,
        unforced_then_forced(paths, &config, "m2 removed"),
    );
    assert_eq!(
        seats_of(&lab, "m9"),
        "",
        "m2's record past the cutoff is out"
    );
    steps
}

/// Every collection of `compiled`'s table, asked of one [`crate::typed::view::Projections`] of
/// `view` in the table's order and then backwards, so that each second ask is answered from
/// what the first projected, projects what a fresh [`crate::typed::View::project`] of `view`
/// does.
#[track_caller]
fn memoized_is_fresh(compiled: &att::Compiled, view: &crate::typed::View, fixture: &str) {
    let projections = crate::typed::view::Projections::new(view);
    let collections = compiled.table.collections;
    for collection in collections.iter().chain(collections.iter().rev()) {
        assert_eq!(
            *projections.project(collection),
            view.project(collection),
            "{fixture}: {}",
            collection.name
        );
    }
}

/// C4's exit test (docs/DESIGN-shared-files-2026-11-11.md §4): a pass projects each collection
/// of a family's view once for every file of the family it projects, and what each file is
/// handed is what a fresh projection of that collection would be. Two registries are read. The
/// first is in its first generation and holds the stand's addresses, accounts, machine and
/// link, a note two Macs wrote at once, the label `m5` two machines claimed at once, and a
/// writer's automatic records one past its quota. The second is a roll's second generation,
/// whose machine labels and facts rows are those the roll carried from the first.
/// NEGATIVE CONTROL, observed: with `Projections::project` knowing a collection by its kind
/// (cells, owned or log) rather than its name, the first cells collection's projection (the
/// provider's, which holds no row) is handed out for MACHINE's: `` the first generation:
/// machine `` `` left: Projection { rows: {}, malformed: 0 } ``.
#[test]
fn a_memoized_projection_equals_a_fresh_one() {
    use crate::alab::registry::SELF_QUOTA;
    use crate::typed::cell;
    let stand = Stand::new();
    crate::table::tests::repair_commons(&stand.lab, 6);
    let (ours, theirs) = (
        Registry::at(stand.paths().clone()),
        Registry::at(stand.lab.paths[0].clone()),
    );
    let automatic = |column: &str, value: Value| {
        cell::change(
            &MACHINE,
            cell::CellKey {
                pk: &stand.machine,
                col: column,
                elem: None,
            },
            &cell::CellValue::automatic(1, value),
        )
        .unwrap()
    };
    // Written on each Mac before either hears the other's.
    let (five, six) = (keys::machine_pk("m5"), keys::machine_pk("m6"));
    ours.write(vec![
        vec![put(
            &MACHINE,
            &stand.machine,
            "note",
            json!("by the window"),
        )],
        vec![
            put(&MACHINE, &five, "label", json!("m5")),
            put(&MACHINE, &five, "status", json!("active")),
        ],
    ])
    .unwrap();
    let mut records: Vec<_> = (0..SELF_QUOTA)
        .map(|n| vec![automatic("location", json!(format!("rack {n}")))])
        .collect();
    records.push(vec![automatic("location", json!("past the quota"))]);
    records.push(vec![put(
        &MACHINE,
        &stand.machine,
        "note",
        json!("by the door"),
    )]);
    records.push(vec![
        put(&MACHINE, &six, "label", json!("m5")),
        put(&MACHINE, &six, "status", json!("active")),
    ]);
    theirs.write(records).unwrap();
    crate::table::tests::repair_commons(&stand.lab, 6);
    let compiled = ours.generation();
    assert_eq!(compiled.generation, 1);
    let view = ours.view().unwrap();
    assert_eq!(
        view.quota.over.len(),
        1,
        "CONTROL: one record past the quota"
    );
    let machines = view.project(&MACHINE);
    assert!(
        machines.rows[&stand.machine].fields["note"].conflicted(),
        "CONTROL: the note two Macs wrote at once is conflicted"
    );
    assert_ne!(
        machines,
        cell::project(&MACHINE, &view.cells),
        "CONTROL: the label two machines claim is settled by the claim rules"
    );
    memoized_is_fresh(compiled, &view, "the first generation");

    let lab = carried_lab();
    let registry = Registry::at(lab.paths[0].clone());
    let compiled = registry.generation();
    assert_eq!(compiled.generation, 2);
    let view = registry.view().unwrap();
    assert_eq!(
        view.owned[crate::alab::registry::MACHINE_FACTS.name]
            .rows
            .len(),
        2,
        "CONTROL: both facts rows are carried"
    );
    let machines = view.project(&MACHINE);
    for label in ["m1", "m2"] {
        assert_eq!(
            machines.rows[&keys::machine_pk(label)].get("label"),
            Some(&json!(label)),
            "CONTROL: the roll carried {label}'s label"
        );
    }
    memoized_is_fresh(compiled, &view, "the second generation");
}

/// A text set's snapshot value is its members' texts (LT-15B): on registry.4, `lab/machines`
/// keeps a Mac's nickname aliases as the aliases, never as nothing.
/// NEGATIVE CONTROL (run 2026-11-15): with the snapshot reading a set by `Field::members` (the
/// elements a candidate adds with `true`), the value is empty: `` left: Some(Array([])) ``.
#[test]
fn a_text_sets_snapshot_value_is_its_members_texts() {
    let machine = keys::machine_pk("m7");
    let mut book = crate::typed::grid::tests::Book::new();
    book.write(
        1,
        &[],
        vec![crate::typed::grid::tests::elem(
            &MACHINE,
            &machine,
            "nickname_aliases",
            "Red Balloon",
        )],
    );
    let machines = crate::typed::cell::project(&MACHINE, &book.cells());
    let column = MACHINE.column("nickname_aliases").unwrap();
    let field = &machines.rows[&machine].fields["nickname_aliases"];
    assert_eq!(
        super::materialize::typed(column, field),
        Some(json!(["Red Balloon"]))
    );
}

/// The journal ring's collections as v0.201.0, the release before this build, compiled them: an
/// older peer's readers read these and no other.
const JOURNAL_0_201: [&crate::typed::Collection; 6] = {
    use crate::alab::journal::{EV, PLACE, RELAY, SUM, UPD, USE};
    [&EV, &USE, &SUM, &RELAY, &PLACE, &UPD]
};

/// The event kinds v0.201.0 reads (`git show v0.201.0:crates/aterm-db/src/alab/journal/event.rs`).
const KINDS_0_201: [&str; 31] = [
    "session-start",
    "session-end",
    "signed-in",
    "signed-out",
    "sign-in-failed",
    "login-ended",
    "renewed",
    "wrong-account",
    "displaced",
    "auth",
    "quota",
    "wall",
    "wall-cleared",
    "reset-used",
    "plan-seen",
    "credits",
    "error",
    "code-requested",
    "code-received",
    "switch",
    "enrolled",
    "unenrolled",
    "probe",
    "token-minted",
    "token-served",
    "token-verified",
    "token-rejected",
    "token-fallback",
    "token-offered",
    "token-removed",
    "seen",
];

/// The event keys v0.201.0 reads: an event that carries any other is dropped whole.
const KEYS_0_201: [&str; 16] = [
    "a", "c", "h", "k", "p", "plan", "r", "rr", "s", "src", "t", "to", "u", "w", "why", "x",
];

/// Whether v0.201.0 reads `event`: its keys and its kind are words that build knows.
fn read_by_0_201(event: &Value) -> bool {
    let Some(object) = event.as_object() else {
        return false;
    };
    object.keys().all(|key| KEYS_0_201.contains(&key.as_str()))
        && event["k"]
            .as_str()
            .is_some_and(|kind| KINDS_0_201.contains(&kind))
}

/// SF-08's exit test: the journal ring's rows become shared files. A peer states its login, its
/// limit reading and its events in the ring beside the rows an older build writes; the record
/// reaches this Mac only through a peer running the release before this build, which reads it
/// exactly as it reads a record without the new rows. This Mac's pass projects `ai/logins`,
/// `ai/usage` and `ai/events` from the ring, holds a row the peer wrote at this Mac's key inert
/// (in no file, counted in the status), and folds `ai/limits`: the peer's hour-resolution
/// reading and this Mac's exact one of one week are one instance (the largest percent at the
/// latest reset), whichever Mac's clock read last; a later week replaces them.
///
/// NEGATIVE CONTROLS, observed: with the fold ordered by the reading Mac's clock
/// (`limits::seam::fold_by_clock` held across the pass), this Mac's later-read 55% wins over the
/// peer's 60%: `` left: Some(550) right: Some(600) ``; with `owned::read_owned` keeping a row at
/// any machine's key, not only its signer's, the peer's row at this Mac's key replaces this
/// Mac's own: `` left: "990" right: "550" ``.
#[test]
fn a_peers_login_and_usage_rows_fold_into_its_files_two_readings_of_one_week_merge_by_reset_and_a_foreign_row_is_inert()
 {
    use crate::alab::journal::{
        EV, USAGE, USE,
        event::{Event, Kind, Pk, Source, Word as EventWord},
        flush,
    };
    use crate::typed::owned;
    const PEER: usize = 0;
    const OLD: usize = 2;
    let stand = Stand::with(&[Role::Writer, Role::Writer, Role::Writer]);
    let lab = &stand.lab;
    let ada = stand.claude_ada.clone();
    let t = NOW / 1000;
    let (peer, me, old) = (uuid_of(lab, PEER), uuid_of(lab, SEAT), uuid_of(lab, OLD));

    // The peer, on this build: its login, its reading of the week to the hour (60% at RESET),
    // a row at this Mac's key, and events of the words only this build has.
    let mut record = Stand::ring_rows(&ada, &peer, 600, RESET, 1);
    record.push(ring_row(
        &USAGE,
        &format!("{ada}.{me}"),
        json!({
            "r": 1, "n": 9, "account": ada, "w7d": 990, "w7d_reset": RESET + 7 * 86_400_000,
            "wall": "none", "src": "statusline", "read_at": NOW, "at": NOW, "sessions_7d": 1,
        }),
    ));
    let event = |kind: Kind| Event {
        t,
        kind,
        account: Pk::new(&ada),
        cli: EventWord::new("claude").unwrap(),
        sub: None,
        window: None,
        pct_x10: None,
        reset: None,
        res: None,
        until: None,
        plan: None,
        src: Source::SigninFlow,
        why: None,
        to: None,
        tab: None,
        harness: None,
        op: None,
        via: None,
        program: None,
    };
    let decided = Event {
        account: None,
        cli: EventWord::new("aterm").unwrap(),
        src: Source::File,
        op: EventWord::new("0123456789abcdef"),
        via: EventWord::new("set"),
        program: EventWord::new("claude"),
        ..event(Kind::Decided)
    };
    let batch: Vec<Value> = [
        event(Kind::SessionStart),
        event(Kind::SignInBlocked),
        event(Kind::NoRoute),
        decided,
    ]
    .iter()
    .map(Event::to_value)
    .collect();
    record.push(ring_row(
        &EV,
        &format!("{peer}.2"),
        json!({"r": 1, "n": 2, "e": batch}),
    ));
    state_ring(lab, PEER, record, t);
    // The older peer writes what v0.201.0 writes: a `use` row and an event batch.
    state_ring(
        lab,
        OLD,
        vec![
            ring_row(
                &USE,
                &format!("{ada}.{old}"),
                json!({
                    "r": 2, "n": 1, "period": t / 604_800, "first": t, "last_start": t,
                    "last_end": t, "sessions_7d": 1, "quota": [], "wall": [], "account": ada,
                }),
            ),
            ring_row(
                &EV,
                &format!("{old}.1"),
                json!({"r": 1, "n": 1, "e": [event(Kind::SessionEnd).to_value()]}),
            ),
        ],
        t,
    );
    // This Mac restates its reading of the same week, to the second and half an hour later
    // than the peer's: 55% at RESET + 30 minutes, read later by its clock.
    state_ring(
        lab,
        SEAT,
        Stand::ring_rows(&ada, &me, 550, RESET + 1_800_000, 2),
        t,
    );

    // The peer's record reaches this Mac through the older peer only. A pair's rounds take the
    // tables whose witnesses differ one a round, and a round leads ahead of them a table the
    // lists newly share, which one side took up once an earlier round brought its declaration
    // (`crate::replica::Repair::lead`): this Mac declares two the older peer lacks, the
    // registry and `lab/tracker`, so the ring window that holds the peer's record may wait for
    // the fourth round, after the catalog's and each of theirs.
    let clocks = [t; 3];
    repair_commons_among(lab, 2, &[OLD, PEER], &clocks);
    repair_commons_among(lab, 4, &[OLD, SEAT], &clocks);

    // THE OLDER PEER reads the record as v0.201.0 does: every row of its collections, and
    // nothing of the new ones, exactly as if the record carried no new row.
    {
        let bootstrap = lab.bootstrap(OLD);
        let session = crate::table::Session::open(&bootstrap).unwrap();
        let directory = crate::catalog::Directory::new(&lab.paths[OLD], bootstrap.trust());
        let window = flush::window(&session, &directory, t).unwrap();
        let resolve = owned::commons::historical(&session);
        let new = |collection: &str| collection == "login" || collection == "usage";
        assert!(
            window.ops.iter().any(|op| op
                .changes
                .iter()
                .any(|change| change.key == format!("{ada}.{peer}") && new(&change.collection))),
            "the older peer holds the peer's new rows"
        );
        let stripped: Vec<owned::Op> = window
            .ops
            .iter()
            .cloned()
            .map(|mut op| {
                op.changes.retain(|change| !new(&change.collection));
                op
            })
            .collect();
        let read = |ops: &[owned::Op]| -> Vec<String> {
            JOURNAL_0_201
                .iter()
                .map(|collection| match collection.kind {
                    crate::typed::Kind::Log => {
                        format!("{:?}", owned::read_log(collection, ops, &resolve))
                    }
                    crate::typed::Kind::Owned | crate::typed::Kind::Cells => {
                        format!("{:?}", owned::read_owned(collection, ops, &resolve))
                    }
                })
                .collect()
        };
        assert_eq!(read(&window.ops), read(&stripped));
        let uses = owned::read_owned(&USE, &window.ops, &resolve);
        assert!(uses.rows.contains_key(&format!("{ada}.{old}")), "{uses:?}");
        // Of the peer's new events it reads the words it knows and drops the rest, one by one.
        let log = owned::read_log(&EV, &window.ops, &resolve);
        let batch = log
            .entries
            .iter()
            .find(|entry| keys::uuid32(&entry.machine) == peer && entry.n == 2)
            .expect("the peer's batch");
        let kinds: Vec<&str> = batch.value["e"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| read_by_0_201(event))
            .map(|event| event["k"].as_str().unwrap())
            .collect();
        assert_eq!(kinds, ["session-start"]);
    }

    // THIS MAC'S PASS projects every file from the ring.
    all(&stand, &stand.pass(true), Word::Live, None);
    let logins = stand.file(&super::LOGINS).read();
    let keyed: Vec<&str> = logins.rows().iter().map(|row| row.pk.as_str()).collect();
    assert!(
        keyed.contains(&format!("{ada}.{peer}").as_str()),
        "{keyed:?}"
    );
    assert!(keyed.contains(&format!("{ada}.{me}").as_str()), "{keyed:?}");
    assert!(!keyed.iter().any(|key| key.ends_with(&old)), "{keyed:?}");
    assert_eq!(
        logins.named(&format!("{ada}.{peer}"))[0].cell("state"),
        "signed-in"
    );

    let usage = stand.file(&super::USAGE).read();
    let mine = &usage.named(&format!("{ada}.{me}"));
    assert_eq!(mine.len(), 1);
    assert_eq!(
        mine[0].cell("w7d"),
        "550",
        "the row at this Mac's key is its own"
    );
    assert!(
        usage.status.state.as_ref().unwrap().inert.unwrap() >= 1,
        "the peer's row at this Mac's key is inert"
    );
    assert_eq!(usage.named(&format!("{ada}.{peer}"))[0].cell("w7d"), "600");

    let events = stand.file(&super::EVENTS).read();
    let kinds: Vec<&str> = events.rows().iter().map(|row| row.cell("kind")).collect();
    for kind in [
        "session-start",
        "sign-in-blocked",
        "no-route",
        "decided",
        "session-end",
        "quota",
    ] {
        assert!(kinds.contains(&kind), "{kind}: {kinds:?}");
    }
    let decided = events
        .rows()
        .iter()
        .find(|row| row.cell("kind") == "decided")
        .unwrap();
    assert_eq!(
        (
            decided.cell("op"),
            decided.cell("via"),
            decided.cell("program")
        ),
        ("0123456789abcdef", "set", "claude")
    );
    assert_eq!(
        decided.values.get("at"),
        Some(&json!(NOW)),
        "an event's time in milliseconds"
    );

    // AI/LIMITS: one instance of the week, the peer's 60% at this Mac's reset, two readers.
    let limit = |stand: &Stand| {
        let limits = stand.file(&super::LIMITS).read();
        let row = limits.named(&ada);
        assert_eq!(row.len(), 1, "{:?}", limits.rows());
        row[0].clone()
    };
    let week = limit(&stand);
    assert_eq!(week.values.get("w7d").and_then(Value::as_u64), Some(600));
    assert_eq!(
        week.values.get("w7d_reset").and_then(Value::as_u64),
        Some(RESET + 1_800_000)
    );
    assert_eq!(
        week.cell("readers"),
        "2",
        "the older peer states no reading"
    );
    assert_eq!(week.cell("account_slug"), "claude-fb-alpha");

    // A LATER WEEK: the peer reads the next instance at 3%, and it replaces both readings.
    state_ring(
        lab,
        PEER,
        Stand::ring_rows(&ada, &peer, 30, RESET + 7 * 86_400_000, 3),
        t,
    );
    // Four rounds, as above: the pair's first, whose rounds after the catalog's may lead the two
    // tables the lists newly share.
    repair_commons_among(lab, 4, &[PEER, SEAT], &clocks);
    all(&stand, &stand.pass(true), Word::Live, None);
    let next = limit(&stand);
    assert_eq!(next.values.get("w7d").and_then(Value::as_u64), Some(30));
    assert_eq!(
        next.values.get("w7d_reset").and_then(Value::as_u64),
        Some(RESET + 7 * 86_400_000)
    );
}

/// A metric's sample as an agent states it: `value`, measured at `at` (Unix milliseconds) by
/// `aterm db table ls`.
fn metric(value: &str, at: u64) -> aterm_json::Map {
    json!({
        "metric": "a10-lab-objects",
        "value": value,
        "at": at,
        "source": "aterm db table ls",
    })
    .as_object()
    .unwrap()
    .clone()
}

/// What two agents on the seat's Mac and one on its peer appended to `lab/tracker`'s `measure`
/// log, each one metric stated in the same millisecond, and how the seat reads it back.
struct Appended {
    /// Each seat agent's key, and the peer's.
    agents: [String; 2],
    peer: String,
    /// The typed read's entries (key, value, at) and the records it holds inert.
    typed: Vec<(String, String, u64)>,
    typed_inert: usize,
    /// `lab/measures`' rows (key, value, at), its kind word and the records its status holds
    /// inert.
    rows: Vec<(String, String, u64)>,
    kind: String,
    inert: u64,
}

/// Two agents on the seat's Mac append one metric each at once, through the one writer path
/// every program of a Mac takes (`att::Handle::append_log`), both stating the millisecond `NOW`;
/// the peer Mac appends one stated in that millisecond too, and repair brings it over. Keyed by
/// the clock (`by_clock`, [`att::log_seam`]) when the negative control asks.
fn two_agents_and_a_peer_append(by_clock: bool) -> (Stand, Appended) {
    use crate::lab::tracker::MEASURE;
    const PEER: usize = 0;
    let stand = Stand::new();
    let lab = &stand.lab;
    let append = |paths: &Paths, value: &str| {
        let _clock = by_clock.then(att::log_seam::key_by_clock);
        att::Handle::at(paths.clone(), &att::TRACKER)
            .append_log(&MEASURE, &[metric(value, NOW)])
            .unwrap()
            .remove(0)
    };
    let start = std::sync::Barrier::new(2);
    let agents: Vec<String> = std::thread::scope(|scope| {
        let agents: Vec<_> = ["5", "6"]
            .into_iter()
            .map(|value| {
                let (start, append, seat) = (&start, &append, stand.paths());
                scope.spawn(move || {
                    start.wait();
                    append(seat, value)
                })
            })
            .collect();
        agents
            .into_iter()
            .map(|agent| agent.join().unwrap())
            .collect()
    });
    let peer = append(&lab.paths[PEER], "7");
    // Both Macs declared `lab/tracker` when they appended, and each round repairs it first, as
    // the push of a write names its table: which table a regular scan would take first turns
    // on the tables' digests, which are each lab's own.
    let tracker = att::TRACKER
        .descriptor(lab.bootstrap(SEAT).trust())
        .unwrap()
        .scope
        .digest();
    repair_commons_among_first(lab, 2, &[PEER, SEAT], &[NOW / 1000; 2], tracker);

    let view = att::Handle::at(stand.paths().clone(), &att::TRACKER)
        .view()
        .unwrap();
    let log = &view.logs[MEASURE.name];
    let typed = log
        .entries
        .iter()
        .map(|entry| {
            (
                crate::typed::owned::log_entry_key(&entry.machine, entry.n),
                entry.value["value"].as_str().unwrap().to_owned(),
                entry.value["at"].as_u64().unwrap(),
            )
        })
        .collect();

    all(&stand, &stand.pass(true), Word::Live, None);
    let read = stand.file(&super::MEASURES).read();
    let rows = read
        .rows()
        .iter()
        .map(|row| {
            (
                row.pk.clone(),
                row.cell("value").to_owned(),
                row.values.get("at").and_then(Value::as_u64).unwrap(),
            )
        })
        .collect();
    let kind = read.snapshot.as_ref().unwrap().kind.clone();
    let inert = read.status.state.as_ref().unwrap().inert.unwrap();
    let appended = Appended {
        agents: [agents[0].clone(), agents[1].clone()],
        peer,
        typed,
        typed_inert: log.inert,
        rows,
        kind,
        inert,
    };
    (stand, appended)
}

/// TK-05's exit test (docs/DESIGN-lab-tracker-2026-11-14.md §6, finding 8 of §7): a log table
/// outside the journal's ring, `lab/tracker`'s `measure`, read typed and through its shared
/// file. Two agents on one Mac append one metric each at once, both stating the same
/// millisecond, and a peer Mac appends one stated in it too. The keys are per writer: the
/// signing Mac's uuid and its own counter, taken under the Mac's writer lock, so the two agents
/// hold `<seat>.2` and `<seat>.3` (past the stand's own sample at `<seat>.1`) in whichever order
/// the lock admitted them, and the peer `<peer>.1`, which the seat's own `.1` does not touch. Every entry reads back, with its value
/// and its millisecond, through the typed read ([`att::View::logs`]) and through `lab/measures`'
/// snapshot, where the file's kind is `log` and nothing is inert.
///
/// NEGATIVE CONTROL, observed: with each entry keyed by the millisecond its row states, as a
/// writer that reads its keys off a clock would ([`att::log_seam::key_by_clock`] on both agents
/// and the peer), this test fails at its first assertion: ``assertion `left != right` failed:
/// two agents on one Mac hold two keys``, ``left: "<seat>.1800000000000"`` and ``right:
/// "<seat>.1800000000000"`` (the second agent's key is the first's); past it, the typed read
/// keeps one of the two agents' entries and counts the other inert, and so does the file.
/// [`a_log_keyed_by_a_clock_loses_one_of_two_agents_metrics`] holds that loss as a test.
#[test]
fn two_agents_on_one_mac_append_one_metric_in_one_millisecond_and_both_rows_read_back() {
    let (stand, appended) = two_agents_and_a_peer_append(false);
    let seat = uuid_of(&stand.lab, SEAT);
    let peer = uuid_of(&stand.lab, 0);
    let [first, second] = &appended.agents;
    assert_ne!(first, second, "two agents on one Mac hold two keys");
    let mut agents = appended.agents.clone();
    agents.sort();
    // The stand's own sample holds `<seat>.1`: the agents' counters run on past it.
    assert_eq!(agents, [format!("{seat}.2"), format!("{seat}.3")]);
    assert_eq!(appended.peer, format!("{peer}.1"));

    // Each agent's value at its own key, whichever the lock admitted first.
    let value_of = |key: &str| if key == first { "5" } else { "6" };
    let mut expected: Vec<(String, String, u64)> = [first, second]
        .into_iter()
        .map(|key| (key.clone(), value_of(key).to_owned(), NOW))
        .chain([
            (format!("{seat}.1"), "4".to_owned(), NOW - 60_000),
            (appended.peer.clone(), "7".to_owned(), NOW),
        ])
        .collect();
    expected.sort();
    let sorted = |rows: &[(String, String, u64)]| {
        let mut rows = rows.to_vec();
        rows.sort();
        rows
    };
    assert_eq!(sorted(&appended.typed), expected, "the typed read");
    assert_eq!(appended.typed_inert, 0);
    assert_eq!(sorted(&appended.rows), expected, "lab/measures");
    assert_eq!(appended.kind, super::types::LOG);
    assert_eq!(appended.inert, 0);
}

/// The exit test's negative control as a test of its own: two agents on one Mac whose log keys
/// come from the clock both write `<seat>.<ms>` in one millisecond, and readers keep the first
/// entry at a key (`typed::owned::read_log`), so one agent's metric is lost to the typed read
/// and to `lab/measures`, counted inert in both. The peer's key, another Mac's uuid, survives.
#[test]
fn a_log_keyed_by_a_clock_loses_one_of_two_agents_metrics() {
    let (stand, appended) = two_agents_and_a_peer_append(true);
    let seat = uuid_of(&stand.lab, SEAT);
    let peer = uuid_of(&stand.lab, 0);
    let at_ms = format!("{seat}.{NOW}");
    assert_eq!(appended.agents, [at_ms.clone(), at_ms.clone()]);
    assert_eq!(appended.peer, format!("{peer}.{NOW}"));
    for (read, rows, inert) in [
        (
            "typed",
            &appended.typed,
            u64::try_from(appended.typed_inert).unwrap(),
        ),
        ("file", &appended.rows, appended.inert),
    ] {
        let mut keys: Vec<&str> = rows.iter().map(|(key, _, _)| key.as_str()).collect();
        keys.sort_unstable();
        let mut expected = [format!("{seat}.1"), at_ms.clone(), format!("{peer}.{NOW}")];
        expected.sort();
        assert_eq!(keys, expected, "{read}: one of the agents' metrics is lost");
        assert_eq!(inert, 1, "{read}");
    }
}

/// A log of a catalog table shows a cell as its table's grid shows it: a value its column's type
/// admits passed that type's own guard when it was appended (`typed::owned::log_row`), so a
/// whole commit, a command that names a test, prose that cites a commit and a commit as evidence
/// read back from `lab/measures` and `lab/notes` as the typed read holds them, with their typed
/// values and nothing withheld. The guards every surface keeps for a text of no known type
/// refuse any run of forty key characters, which every one of these holds.
///
/// NEGATIVE CONTROL, observed: with a log's cells held to those guards as a file of the journal
/// ring's are (`Held::Ring` in `materialize::log_table`), `lab/measures` shows the refusal text
/// in place of the commit: `` assertion `left == right` failed: lab/measures commit ``, `left:
/// "(refused value: not shown)"`, `right: "0123456789abcdef0123456789abcdef01234567"`.
#[test]
fn a_logs_file_shows_a_commit_a_command_and_evidence_as_their_types_admit_them() {
    use crate::lab::tracker::{MEASURE, NOTE};
    use crate::typed::values::Evidence;
    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    const SOURCE: &str = "targo --unverified test -p aterm-db --lib \
        file::tests::two_agents_on_one_mac_append_one_metric_in_one_millisecond_and_both_rows_read_back";
    let stand = Stand::new();
    let tracker = att::Handle::at(stand.paths().clone(), &att::TRACKER);
    let mut sample = metric("9", NOW);
    sample.insert("source".to_owned(), json!(SOURCE));
    sample.insert("commit".to_owned(), json!(COMMIT));
    let measured = tracker
        .append_log(&MEASURE, std::slice::from_ref(&sample))
        .unwrap();
    let evidence = Evidence::Commit {
        repo: "aterm".to_owned(),
        commit: COMMIT.to_owned(),
    };
    let note = json!({
        "about": "tk-05",
        "text": format!("landed at {COMMIT}"),
        "evidence": [evidence.to_value()],
        "at": NOW,
    })
    .as_object()
    .unwrap()
    .clone();
    let noted = tracker
        .append_log(&NOTE, std::slice::from_ref(&note))
        .unwrap();

    all(&stand, &stand.pass(false), Word::Live, None);
    for (file, collection, key, stated) in [
        (&super::MEASURES, &MEASURE, &measured[0], &sample),
        (&super::NOTES, &NOTE, &noted[0], &note),
    ] {
        let read = stand.file(file).read();
        let row = read
            .rows()
            .iter()
            .find(|row| row.pk == *key)
            .unwrap_or_else(|| panic!("{}: no row {key}", file.name));
        for (name, value) in stated {
            let column = collection.column(name).unwrap();
            assert_eq!(
                row.cell(name),
                grid::show(&column.ty, value),
                "{} {name}",
                file.name
            );
            assert_eq!(row.values.get(name), Some(value), "{} {name}", file.name);
        }
        assert_eq!(
            read.status.state.as_ref().unwrap().withheld,
            0,
            "{}",
            file.name
        );
    }
    // The commit and the command are the texts a person reads, not a spelling of a refusal.
    let measures = stand.file(&super::MEASURES).read();
    let row = measures.rows().iter().find(|row| row.pk == measured[0]);
    assert_eq!(row.unwrap().cell("commit"), COMMIT);
    assert_eq!(row.unwrap().cell("source"), SOURCE);
}
