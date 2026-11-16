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
    table::tests::{Lab, repair_commons_among},
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
            put(&PREFERENCE, &link, "machine", json!(machine)),
            put(&PREFERENCE, &link, "account", json!(claude_ada)),
            put(&PREFERENCE, &link, "rank", json!(1)),
            put(&PREFERENCE, &link, "why", json!("owner")),
        ];
        changes.extend(account(&claude_ada, "anthropic", "claude", &ada));
        changes.extend(account(&oai_ada, "openai", "codex", &ada));
        changes.extend(account(&claude_bea, "anthropic", "claude", &bea));
        registry.write(vec![changes]).unwrap();
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
        holding: &|| Ok(0),
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
    // The registry's table and the journal ring's window: every table a file is read from.
    let mut tables = vec![aterm_codec::hex::encode_lower(&stand.table)];
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
    let invalidated = stand.pass(true);
    all(&stand, &invalidated, Word::Held, Some(Why::Succession));
    for outcome in &invalidated {
        assert_eq!(
            outcome.recorded,
            Some(Recorded::Recorded { replaced: true }),
            "the unsettled home's records are projected"
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

/// The vault's admin revokes the stand's Mac, and its next sync says so.
fn revoke(stand: &Stand) {
    let admin = client::admin_cached(&stand.lab.paths[0]).unwrap();
    client::admin_revoke(&stand.lab.paths[0], &admin, "m2", false).unwrap();
    client::sync(stand.paths(), None).unwrap();
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
    all(&stand, &stand.pass(false), Word::Live, None);
    let live = reads(&stand, Word::Live, None);
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
        // A file of the journal ring or a view has no grid export; the exit test of SF-08
        // reads their faces.
        let Some((_, bound)) = file.table() else {
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
            Class::Derived
        ]
    );
    assert_eq!(
        super::FILES
            .iter()
            .map(|file| file.kind())
            .collect::<Vec<_>>(),
        ["table", "table", "table", "table", "table", "log", "view"]
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

    // An older build rewrote a status without the count (a refused pass keeps the projection
    // version it read): at the same frontier the next pass projects again rather than trust a
    // count nobody recorded.
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
/// nothing again for a frontier it already holds, and never shows a value the guards refuse.
/// NEGATIVE CONTROL: with `table` writing a cell's stored value in place of the text the grid
/// shows, the note's cell is the stored run, which the `REFUSED` assertion prints.
#[test]
fn a_pass_projects_what_arrived_and_withholds_what_the_guards_refuse() {
    let stand = Stand::new();
    let first = stand.pass(false);
    all(&stand, &first, Word::Live, None);
    // Nothing arrived: nothing is projected again, and the word is recorded all the same.
    for outcome in stand.pass(false) {
        assert_eq!(
            outcome.recorded,
            Some(Recorded::Recorded { replaced: false })
        );
    }
    assert_eq!(stand.file(&super::ACCOUNTS).status().rev(), 1);
    // A projection of the same records keeps the revision.
    for outcome in stand.pass(true) {
        assert_eq!(
            outcome.recorded,
            Some(Recorded::Recorded { replaced: true })
        );
    }
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

    let first = stand.pass(false);
    for outcome in &first {
        assert_eq!(
            outcome.recorded,
            Some(Recorded::Recorded { replaced: true }),
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
         ai/limits: live · snapshot written\n"
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
    said
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
    let note = write::draft_set(paths, "lab/machines", "m7", "note", Some("both"))
        .unwrap()
        .unwrap();
    let status = write::draft_set(paths, "lab/machines", "m7", "status", Some("planned"))
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

    // The peer's record reaches this Mac through the older peer only.
    let clocks = [t; 3];
    repair_commons_among(lab, 2, &[OLD, PEER], &clocks);
    repair_commons_among(lab, 2, &[OLD, SEAT], &clocks);

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
    repair_commons_among(lab, 2, &[PEER, SEAT], &clocks);
    all(&stand, &stand.pass(true), Word::Live, None);
    let next = limit(&stand);
    assert_eq!(next.values.get("w7d").and_then(Value::as_u64), Some(30));
    assert_eq!(
        next.values.get("w7d_reset").and_then(Value::as_u64),
        Some(RESET + 7 * 86_400_000)
    );
}
