// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared files on a stand-in lab: a scratch Commons whose registry holds made-up accounts, a
//! machine and a link, materialized under the stand-in home's own `~/.aterm/files`. No live
//! vault, registry or account is read.

use super::{
    Derived, Type,
    materialize::{self, Outcome, Pass},
    publish::{self, Answer},
    seam, write,
};
use crate::{
    alab::{
        Registry,
        command::views::{self, Noun, Opening, Place, ViewIo},
        registry::{ACCOUNT, ADDRESS, MACHINE, PREFERENCE},
        secret::Journal,
    },
    att, generation,
    table::tests::Lab,
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
        let lab = Lab::new(&[Role::Writer, Role::Writer]);
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
        Self {
            lab,
            config,
            table,
            claude_ada,
            machine,
        }
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
/// session gate, no grid, no standard input.
#[track_caller]
fn alias(paths: &Paths, noun: Noun, list: &[&str]) -> Value {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let journal = Journal::at(scratch.path());
    let gate = || -> crate::Result<Place> { panic!("an alias read asks no session") };
    let mut open = |_: &Opening<'_>| -> crate::Result<()> { panic!("an alias read opens no grid") };
    let mut read_stdin = || -> crate::Result<zeroize::Zeroizing<Vec<u8>>> {
        panic!("an alias read takes no standard input")
    };
    let mut io = ViewIo {
        gate: &gate,
        open: &mut open,
        read_stdin: &mut read_stdin,
        journal: &journal,
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
    /// The alias rows of `aterm db ai-accounts --show-addresses` and `aterm db computers`.
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
    let accounts = alias(stand.paths(), Noun::AiAccounts, &["--show-addresses"]);
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
    assert_eq!(revs(&stand), [1, 1, 1]);
    // What each refusal must leave: the same rows at the same revision, whatever the word.
    let untouched = |seen: Seen| {
        assert_eq!(seen, live, "the snapshot is untouched");
        assert_eq!(revs(&stand), [1, 1, 1]);
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
    stand.commons(&format!(
        "[commons]\nexclude_tables = [\"{}\"]\n",
        aterm_codec::hex::encode_lower(&stand.table)
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

/// The face is the TSV `aterm db table export` writes for the collection, addresses shown, with
/// the file's derived columns before the key and the revision in the key's header cell: every
/// stored column's header and every stored cell, quoting included, is the grid's own.
/// NEGATIVE CONTROL: with `push_cell` never quoting, the machine's row differs from the export's:
/// `` right: ["m7", …, "the \"desk\" Mac", "", ""] ``.
#[test]
fn the_face_is_the_grid_export_with_derived_columns_and_the_revision() {
    let stand = Stand::new();
    all(&stand, &stand.pass(false), Word::Live, None);
    let registry = Registry::at(stand.paths().clone());
    let compiled = registry.generation();
    let view = registry.view().unwrap();
    for file in super::FILES {
        let collection = compiled.collection(file.collection.name).unwrap();
        let export = grid::export(compiled.table, collection, &view.cells, &[], true).unwrap();
        let face = std::fs::read_to_string(stand.root().face(&file.file(), "tsv")).unwrap();
        let (theirs, ours) = (grid_of(&export.text), grid_of(&face));
        assert_eq!(theirs.len(), ours.len(), "{}", file.name);
        let derived = file.derived.columns();
        // The grid's columns but its key, then the derived ones, then the key at its revision.
        let stored = theirs[0].len() - 1;
        assert_eq!(theirs[0][stored], "=pk");
        assert_eq!(ours[0].len(), stored + derived.len() + 1, "{}", file.name);
        assert_eq!(ours[0][stored + derived.len()], "=pk@1");
        for (at, name) in derived.iter().enumerate() {
            assert_eq!(ours[0][stored + at], format!("={name}"));
        }
        for (theirs, ours) in theirs.iter().zip(&ours) {
            assert_eq!(theirs[..stored], ours[..stored], "{}", file.name);
        }
        for (theirs, ours) in theirs.iter().zip(&ours).skip(1) {
            assert_eq!(theirs[stored], ours[stored + derived.len()], "the key");
        }
    }
    let machines =
        std::fs::read_to_string(stand.root().face(&super::MACHINES.file(), "tsv")).unwrap();
    assert!(machines.contains("\"the \"\"desk\"\" Mac\""), "{machines}");
    // The derived cells: the account's name and slug, its address, and the machines linked.
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

/// A shared file holds no `Secret` and no `Sealed` column: the registry's `address` collection,
/// whose `password` is a `Secret`, cannot be one, and every file this build compiles passed the
/// same check when it was compiled.
/// NEGATIVE CONTROL: with `holds_secret` answering `false` for a `Secret`, the address file
/// checks: `` called `Result::unwrap_err()` on an `Ok` value: () ``.
#[test]
fn a_file_with_a_secret_column_is_refused() {
    let emails = Type {
        name: "lab/emails",
        about: "",
        family: &att::REGISTRY,
        collection: &ADDRESS,
        derived: Derived::None,
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

/// A derived cell is held to the guards its source cell is: a machine's label stored unchecked
/// by an older or modified build, shaped like a credential, shows the grid's refusal text in
/// `lab/machines` and never reaches the cells derived from it (`ai/accounts` `=links`,
/// `ai/links` `=machine_name`), in the snapshot's shown text, its typed values or the face; and
/// each cell left out is counted.
/// NEGATIVE CONTROL: either guard alone holds the run back. With `machine_labels` taking the
/// stored label as it is and `table` writing derived text unchecked, `=links` is the run:
/// `` left: "k9k9k9…" right: "(refused value: not shown)" ``.
#[test]
fn a_derived_cell_never_carries_a_value_the_guards_refuse() {
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
         lab/machines: live · snapshot written\n"
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

/// One write verb of `aterm file` over `paths`: what it printed and its exit code.
#[track_caller]
fn wrote(paths: &Paths, config: &std::path::Path, list: &[&str]) -> (String, u8) {
    write::verb(paths, config, &words(list), &env())
        .unwrap_or_else(|refusal| panic!("{list:?}: {refusal:?}"))
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
        write::verb(paths, config, &words(&["resolve", "lab/machines", &twice, "mine"]), &env()),
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
        write::verb(
            paths,
            config,
            &words(&["resolve", "lab/machines", &id_of(&again), "mine"]),
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
    let mine = held
        .state_mine(
            "harness",
            aterm_file::Draft {
                born: false,
                revert: None,
                edits: vec![aterm_file::Edit {
                    key: "@self".to_owned(),
                    column: "note".to_owned(),
                    elem: None,
                    value: json!("says the Mac itself"),
                    text: "says the Mac itself".to_owned(),
                    expected: Vec::new(),
                }],
            },
            NOW,
        )
        .unwrap();
    // The files of this build are owner-written cells, whose row keys are shorter than a
    // machine uuid: the filled key is refused by the cell grammar under the writer, and the
    // statement is held with that rule, never dropped and never written at another key.
    let outcomes = publish_pass(paths, config);
    assert_eq!(
        answer_of(&outcomes, &mine.id),
        Answer::Held {
            why: "a typed cell key breaks its grammar".to_owned()
        },
        "{outcomes:?}"
    );
    assert!(carrying(paths, &mine.id).is_empty());
    assert_eq!(held.outbox().held.len(), 1);
    assert_eq!(
        verb(&root, &["outbox", "drop", &mine.id]),
        format!("dropped {} (lab/machines)\n", mine.id)
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
    let set = |list: &[&str], env: &write::Env<'_>| write::verb(paths, config, &words(list), env);
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
            &aterm_file::OwnerAct::verb("person", None).unwrap(),
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
            dropping.drop_intent(&dropped).unwrap(),
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
