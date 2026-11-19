// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The harness's account names over a hand-built lab and roster; every address is synthetic
//! (`x.example`).

use super::{Candidates, Login, Named, names_of_login, names_of_row, resolve, words};
use crate::ai::connector::Region;
use crate::harness::accounts::{self, Auth, Cli, Event, Kind, Row, Vendor};
use crate::harness::accounts_token_run;
use crate::harness::alab_roster::{self, Lab, LabAccount};
use crate::harness::cli::{self, AccountsCmd, AccountsOf, Cmd, Env, LedgerFile};
use aterm_db::typed::keys;
use aterm_file::{AccountNames, many_accounts, no_account};
use aterm_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::process::ExitCode;

/// Signed in to Claude and to OpenAI: two accounts.
const ADA: &str = "ada@x.example";
const BEA: &str = "bea@y.example";
const CY: &str = "cy@z.example";
const DEE: &str = "dee@w.example";
/// Signed in to OpenAI alone.
const EVE: &str = "eve@v.example";

fn key(realm: &str, email: &str) -> String {
    keys::account_pk(realm, email).expect("an account key")
}

/// The label `accounts add` and `adopt-default` give the row of `cli`'s account at `email` (in
/// its pool's first realm): the account's reserved slug, which no other account can come to hold.
pub(crate) fn added_label(cli: Cli, email: &str) -> String {
    let realm = super::realms(cli)[0];
    keys::reserved_slug(realm, &key(realm, email))
}

/// What a verb's lines name `cli`'s roster row `label`, in the roster file `roster`, by with no
/// lab ([`super::RowName`]): its name in a message, the word its steps type and its name in a
/// log. The names a test of those lines expects; a label no row holds names itself.
pub(crate) fn row_name(cli: Cli, roster: &std::path::Path, label: &str) -> super::RowName {
    row_name_in(cli, roster, label, None)
}

/// [`row_name`] under the lab `lab`, as a verb run under that lab names the row.
///
/// The names it hands a test are the code's own, so it first holds them to what Decision 3 says
/// of a row with an address, from the row's address alone: its message name holds that address,
/// its step word is its account's key (made here from its realm and address) or the lab's slug of
/// it, never its label, and its log name is that key with no address and no label in it. A test
/// that expects these names then expects names that obey the decision, whatever the code makes.
pub(crate) fn row_name_in(
    cli: Cli,
    roster: &std::path::Path,
    label: &str,
    lab: Option<&Lab>,
) -> super::RowName {
    let rows = accounts::Roster::load_for(cli, roster)
        .expect("the roster")
        .rows;
    let named = super::RowName::of_label(cli, label, &rows, lab);
    if let Some(row) = rows.iter().find(|row| row.label == label)
        && let Some(address) = row.expected_identity_email()
    {
        held_to_decision_3(cli, row, address, &named, lab);
    }
    named
}

/// The realm of `cli`'s row `row`, as the lab spells it: Kimi Code's by the row's region.
fn realm_of_row(cli: Cli, row: &Row) -> &'static str {
    match (cli, row.vendor.region) {
        (Cli::Claude, _) => "claude",
        (Cli::Codex, _) => "codex",
        (Cli::Grok, _) => "grok",
        (Cli::Agy, _) => "google",
        (Cli::Kimi, Some(Region::MainlandCn)) => "kimi:mainland-cn",
        (Cli::Kimi, _) => "kimi:global",
    }
}

/// Panics where `named`, the names of `cli`'s row `row` whose account's address is `address`,
/// breaks Decision 3 ([`row_name_in`]).
fn held_to_decision_3(
    cli: Cli,
    row: &Row,
    address: &str,
    named: &super::RowName,
    lab: Option<&Lab>,
) {
    let realm = realm_of_row(cli, row);
    let pk = key(realm, address);
    let slug = lab
        .and_then(|lab| lab.names.get(&pk))
        .map(|names| names.slug.clone())
        .filter(|slug| !slug.is_empty());
    let word = named.word.as_str();
    assert!(
        word == pk || slug.as_deref() == Some(word),
        "`{}`'s step word is {word}, not its key {pk} or its slug {slug:?}",
        row.label
    );
    assert!(
        named.logged == pk
            || slug
                .as_ref()
                .is_some_and(|slug| named.logged == format!("{pk} ({slug})")),
        "`{}`'s log name is {}, not {pk} with its slug {slug:?}",
        row.label,
        named.logged
    );
    let shown = named.shown.to_ascii_lowercase();
    let address = address.to_ascii_lowercase();
    assert!(
        shown.contains(&address),
        "`{}`'s message name {} holds no address",
        row.label,
        named.shown
    );
    assert!(
        !word.contains('@') && !named.logged.contains('@'),
        "`{}`'s step word {word} or log name {} holds an address",
        row.label,
        named.logged
    );
    // A label may be its address's local part (`kimi-one`, `kimi-one@…`), so the message name
    // is read with its address taken out.
    let unaddressed = shown.replace(&address, "");
    let own_slug = slug.as_deref() == Some(row.label.as_str());
    if !own_slug {
        for text in [word, named.logged.as_str(), unaddressed.as_str()] {
            assert!(
                !tokens(text).any(|token| token.eq_ignore_ascii_case(&row.label)),
                "`{}`'s names print its label: {text}",
                row.label
            );
        }
    }
}

/// [`row_name`] in the roster `env` reads for `cli`.
pub(crate) fn row_name_of(cli: Cli, env: &Env, label: &str) -> super::RowName {
    row_name(
        cli,
        &env.accounts_roster_for(cli).expect("a roster path"),
        label,
    )
}

/// The row `label` of the roster `env` reads for `cli` as a report's JSON names it
/// ([`super::Listing::id`]): its account's key, else (a row with no address) its label. Unix
/// alone, as its callers' reports are (they read the vault tokens section).
#[cfg(unix)]
pub(crate) fn report_id(cli: Cli, env: &Env, label: &str) -> String {
    let rows =
        accounts::Roster::load_for(cli, &env.accounts_roster_for(cli).expect("a roster path"))
            .expect("the roster")
            .rows;
    super::Listing::of(cli, &rows, None).id(label).to_owned()
}

/// The report JSON `doc`'s row of the roster row `label` ([`report_id`]): the one whose `ac` is
/// that row's key, else the one of a row with no address whose `label` it is.
#[cfg(unix)]
pub(crate) fn report_row<'a>(
    cli: Cli,
    env: &Env,
    label: &str,
    doc: &'a Value,
) -> Option<&'a Value> {
    let id = Value::from(report_id(cli, env, label));
    doc["accounts"]
        .as_array()?
        .iter()
        .find(|row| row["ac"] == id || (row["ac"].is_null() && row["label"] == id))
}

/// Each account of `accounts` (realm, claims address) by key, named as the lab names it: its
/// slug derived from its address among these accounts (`keys::account_slugs`), its provider
/// word and address, and its address as its login.
fn slugged(accounts: &[(&str, &str)]) -> BTreeMap<String, AccountNames> {
    let keyed: Vec<(String, &str, &str)> = accounts
        .iter()
        .map(|(realm, email)| (key(realm, email), *realm, *email))
        .collect();
    let rows: Vec<keys::Slugged<'_>> = keyed
        .iter()
        .map(|(pk, realm, email)| keys::Slugged {
            pk,
            realm,
            handle: None,
            claims_email: Some(email),
            live: true,
        })
        .collect();
    let mut slugs = keys::account_slugs(&rows);
    keyed
        .iter()
        .map(|(pk, realm, email)| {
            let names = AccountNames {
                realm: (*realm).to_owned(),
                slug: slugs.remove(pk).expect("a slug"),
                name: keys::account_name(realm, email),
                email: Some((*email).to_owned()),
                login: Some((*email).to_owned()),
                live: true,
            };
            (pk.clone(), names)
        })
        .collect()
}

/// The lab and Claude Code roster of these tests. The roster's rows are labelled `m2-2` (a
/// machine's word), the label an earlier build gave `bea`'s row (`claude-<6 hex of its key>`,
/// `keys::earlier_row_label`), `cy`'s reserved slug (what a sync labels a row it adds) and, for
/// `dee`, the account's own slug. The lab names five accounts, `ada`'s twice (Claude and
/// OpenAI), each slugged as the lab derives a slug from its address, and holds each as a pooled
/// account labelled with its reserved slug, `bea`'s with the label a Mac called its row before
/// (`m8-2`).
struct Fixture {
    roster: Vec<Row>,
    lab: Lab,
}

fn row(label: &str, email: &str) -> Row {
    Row {
        label: label.to_owned(),
        email: Some(email.to_owned()),
        ..Row::default()
    }
}

/// The lab's pooled account `pk` of `realm`, as LabView makes it: labelled with its reserved
/// slug, and `legacy` the label a Mac called its row before.
fn pooled(pk: &str, realm: &str, email: &str, legacy: Option<&str>) -> LabAccount {
    LabAccount {
        pk: pk.to_owned(),
        cli: if realm == "codex" {
            Cli::Codex
        } else {
            Cli::Claude
        },
        vendor: Vendor::default(),
        label: keys::reserved_slug(realm, pk),
        legacy_label: legacy.map(str::to_owned),
        email: email.to_owned(),
        identity: None,
        browser: None,
        joinable: true,
        preferred: Vec::new(),
    }
}

fn fixture() -> Fixture {
    let accounts = [
        ("claude", ADA),
        ("codex", ADA),
        ("claude", BEA),
        ("claude", CY),
        ("claude", DEE),
    ];
    let pooled = accounts
        .iter()
        .map(|(realm, email)| {
            let legacy = (*email == BEA).then_some("m8-2");
            pooled(&key(realm, email), realm, email, legacy)
        })
        .collect();
    let earlier = keys::earlier_row_label("claude", &key("claude", BEA)).expect("a label");
    let roster = vec![
        row("m2-2", ADA),
        row(&earlier, BEA),
        row(&keys::reserved_slug("claude", &key("claude", CY)), CY),
        row("claude-dee-w", DEE),
    ];
    Fixture {
        roster,
        lab: Lab {
            accounts: pooled,
            names: slugged(&accounts),
            ..Lab::default()
        },
    }
}

impl Fixture {
    fn candidates(&self) -> Candidates<'_> {
        Candidates {
            roster: &self.roster,
            logins: &[],
            lab: Some(&self.lab),
            held: &[],
        }
    }

    /// Every label this Mac or the lab holds for an account, with that account's slug: each
    /// roster row's label, each pooled account's label (its reserved slug) and each label a Mac
    /// called its row before.
    fn labels(&self) -> Vec<(String, Option<String>)> {
        let mut out = Vec::new();
        for row in &self.roster {
            let slug = names_of_row(Cli::Claude, row, Some(&self.lab)).map(|(_, names)| names.slug);
            out.push((row.label.clone(), slug));
        }
        for account in &self.lab.accounts {
            let slug = self
                .lab
                .names
                .get(&account.pk)
                .map(|names| names.slug.clone());
            out.push((account.label.clone(), slug.clone()));
            if let Some(legacy) = &account.legacy_label {
                out.push((legacy.clone(), slug));
            }
        }
        out
    }
}

/// Every word of `text` a label may be: a run of the label grammar's bytes, with no trailing
/// full stop.
fn tokens(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
        .map(|token| token.trim_end_matches('.'))
        .filter(|token| !token.is_empty())
}

/// Each of `labels` that `text` says as a word of its own, for a test whose rows are labelled as
/// their addresses' local parts (`old` beside `old@example.com`): a run of the label grammar's
/// bytes with `@` among them, so an address is one word, which no label is, and no trailing full
/// stop. A receipt that names each row by its account says none of a row's labels whose row has
/// an address (Decision 3).
pub(crate) fn labels_said<'a>(text: &str, labels: &[&'a str]) -> Vec<&'a str> {
    let words: BTreeSet<&str> = text
        .split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '@')))
        .map(|word| word.trim_end_matches('.'))
        .collect();
    labels
        .iter()
        .copied()
        .filter(|label| words.contains(label))
        .collect()
}

/// Each label of `fixture` ([`Fixture::labels`]) that is a word of `text`, unless it is its own
/// account's slug or `typed`, the word a refusal repeats: what `text` prints that it must not.
fn labels_printed(text: &str, fixture: &Fixture, typed: Option<&str>) -> Vec<String> {
    fixture
        .labels()
        .into_iter()
        .filter(|(label, slug)| {
            slug.as_deref() != Some(label.as_str())
                && typed != Some(label.as_str())
                && tokens(text).any(|token| token == label)
        })
        .map(|(label, _)| format!("the label `{label}` is printed: {text}"))
        .collect()
}

/// A verb of `aterm harness accounts` that takes or prints an account: a form of the accounts
/// parse, and the ones the parse does not hand on as a form: the token wrapper, which the
/// front door runs ahead of the parse (`accounts_token_run::parse`), the report of every pool
/// (`accounts --cli all`) and `mailboxes import`; and the accounts ledger's reader (`ledger
/// accounts`), which lists what the ledger says of each account. (`table` the parse hands on as
/// a command of its own; [`verb_of`] maps it back to its form.)
#[derive(Debug)]
enum Verb {
    Form(AccountsCmd),
    /// Its parse took the argv; the slice that resolves its word carries the `Run` here.
    TokenRun,
    AllPools,
    Mailboxes,
    Ledger,
}

/// `argv` read as `aterm harness` reads it.
fn verb_of(argv: &[&str]) -> Verb {
    if argv.get(1) == Some(&"token-run") {
        let args: Vec<OsString> = argv[2..].iter().map(OsString::from).collect();
        accounts_token_run::parse(&args).unwrap_or_else(|why| panic!("{argv:?}: {why}"));
        return Verb::TokenRun;
    }
    let args: Vec<String> = argv.iter().map(|word| (*word).to_owned()).collect();
    match cli::parse(&args) {
        Ok((Cmd::Accounts(form) | Cmd::AccountsOf(AccountsOf { form, .. }), _)) => Verb::Form(form),
        Ok((Cmd::AccountsSourceTable { json, .. }, _)) => Verb::Form(AccountsCmd::Table { json }),
        Ok((Cmd::AccountsAll { .. }, _)) => Verb::AllPools,
        Ok((Cmd::MailboxesImport { .. }, _)) => Verb::Mailboxes,
        Ok((
            Cmd::Ledger {
                file: LedgerFile::Accounts(_),
                ..
            },
            _,
        )) => Verb::Ledger,
        other => panic!("{argv:?} parses as {other:?}"),
    }
}

/// What a verb does with an account word, as SF-07H brings each one over: every verb starts
/// `Pending`; the slice that makes a verb resolve the four words moves it to [`Case::Takes`],
/// which runs them, and the one that makes it print names to a case that scans what it prints,
/// until no verb is pending and no verb holds a printed line back ([`Took::held_back`], S13).
enum Case {
    Pending,
    /// It takes its account word through [`resolve`]: the function runs the verb on the
    /// [`Bench`] naming an account by the argv words it is given (none: the verb's form that
    /// names no account), and says the account it reached and everything it printed.
    Takes(fn(&Bench, &[&str]) -> Took),
    /// As [`Case::Takes`], for a verb the loop never runs with no word: one that names exactly
    /// one account and has no form that names none (`token`, `token-run`, `use`), or whose form
    /// that names none acts on this machine's own login rather than listing rows, which its case
    /// runs itself (`adopt-default --email <address>`, [`listed_or_added`]).
    TakesOne(fn(&Bench, &[&str]) -> Took),
    /// It takes no word but prints the names of an account it acted on (`use --auto`, the
    /// account whose pin it ended) or of every row (the report, `accounts --cli all`): the
    /// function runs it on the [`Bench`] and says whether each run exited 0, everything it
    /// printed and what it found wrong with what it printed. Its [`Took::reached`] and
    /// [`Took::listed`] are not read.
    Prints(fn(&Bench) -> Took),
}

/// What a verb did with an account word ([`Case::Takes`]).
struct Took {
    /// The key of the account it reached, or its refusal past the verb's own prefix.
    reached: Result<String, String>,
    /// Whether each form it was run in (its JSON, its text) exited 0.
    exited: Vec<bool>,
    /// Run naming no account: the key of each account its JSON listed, in order (a row with no
    /// address as its label); empty where it named one.
    listed: Vec<String>,
    /// Everything it printed, on stdout and stderr, in each form it was run in.
    printed: Vec<String>,
    /// What it printed that still names a row by its label until a package after SF-07H's slice
    /// moves it (token-run's start-nothing reasons, `<label> is held by m7`, which HX-33 deletes
    /// with the wrapper's judgment; `sync --cli`'s refusal of a Mac that left a pool, which says
    /// the label a restored roster writes until SF-10 re-keys the records under it): not scanned
    /// as done, and each must still print a roster label (else it belongs in `printed`). A verb
    /// that holds any line back is not finished, so S13 cannot close over it.
    held_back: Vec<String>,
    /// What the case found wrong in its own run that the checks over `reached`, `exited` and
    /// `listed` do not read (token-run's judgment of each row).
    faults: Vec<String>,
}

/// One run of a verb on the [`Bench`]: whether it exited 0, and what it printed.
struct Ran {
    ok: bool,
    out: String,
    err: String,
}

/// The fixture on disk, for the verbs that read this Mac's records: a scratch harness state whose
/// Claude Code roster is the fixture's rows, each with a login folder of its own and a record
/// carrying its lab key (as a transaction learns it), with a ledger row about each row and one
/// rotating it to the next (both by label, as the ledger holds them), each verb run under the
/// fixture's lab, and a Claude Code build in a store of atpkg's shape for the token wrapper to
/// judge.
struct Bench {
    _tmp: aterm_tempfile::TempDir,
    env: Env,
    lab: Lab,
    /// The store build (`<store>/claude/7/bin/claude`).
    build: std::path::PathBuf,
}

impl Bench {
    fn new(fixture: &Fixture) -> Bench {
        let tmp = aterm_tempfile::Builder::new()
            .prefix("aterm-account-names-")
            .tempdir()
            .expect("a scratch folder");
        let env = Env {
            state: tmp.path().join("state"),
            aterm_state: Some(tmp.path().join("aterm-state")),
            cwd: tmp.path().join("work"),
            home: Some(tmp.path().join("home")),
            claude_dir: None,
            claude_config_unusable: None,
            claude_profile: None,
            claude_custom_oauth: false,
            claude_session: None,
            claude_pid: None,
            codex_thread: None,
            agy_conversation: None,
            codex_home: None,
            xdg_config_home: None,
            codex_login: None,
            now: 1_758_412_800,
            sid: "s-test".to_owned(),
            sock: Some(tmp.path().join("no.sock").to_string_lossy().into_owned()),
            config: Some(tmp.path().join("aterm.toml")),
            claude_tmp: Some(tmp.path().join("claude-tmp")),
            claude_running: None,
            tz_s: 0,
        };
        let roster = &fixture.roster;
        let placed: Vec<Row> = roster
            .iter()
            .map(|row| {
                let dir = tmp.path().join("logins").join(&row.label);
                std::fs::create_dir_all(&dir).expect("a login folder");
                std::fs::write(dir.join("a-login-file"), "{}").expect("a login file");
                Row {
                    dir: dir.to_str().map(str::to_owned),
                    ..row.clone()
                }
            })
            .collect();
        std::fs::write(
            env.accounts_roster().expect("a roster path"),
            accounts::append_rows_for(Cli::Claude, "", &placed).expect("the fixture's rows admit"),
        )
        .expect("the roster");
        let mut ledger = Vec::new();
        for (at, row) in roster.iter().enumerate() {
            let t = env.now - 3_600 * (i64::try_from(at).expect("a few rows") + 1);
            ledger.push(Event::new(t, Kind::Auth, &row.label));
            let mut rotated = Event::new(t + 60, Kind::Rotate, &row.label);
            rotated.to = Some(roster[(at + 1) % roster.len()].label.clone());
            ledger.push(rotated);
        }
        env.accounts_store()
            .transact(env.now, &mut None, |state| {
                for row in roster {
                    let pk = names_of_row(Cli::Claude, row, None).map(|(pk, _)| pk);
                    state.accounts.entry(row.label.clone()).or_default().pk = pk;
                }
                Ok(ledger)
            })
            .expect("the ledger");
        let build = tmp.path().join("store/claude/7/bin/claude");
        std::fs::create_dir_all(build.parent().expect("the build's folder")).expect("a folder");
        std::fs::write(&build, "#!/bin/sh\n").expect("the build");
        Bench {
            _tmp: tmp,
            env,
            lab: fixture.lab.clone(),
            build,
        }
    }

    /// Edits the bench's records under their lock, writing no ledger row.
    fn edit(&self, edit: impl FnOnce(&mut accounts::State)) {
        self.env
            .accounts_store()
            .transact(self.env.now, &mut None, |state| {
                edit(state);
                Ok(Vec::new())
            })
            .expect("the bench's records");
    }

    /// The roster row the pin in the bench's records holds, where one is pinned.
    fn pinned(&self) -> Option<Row> {
        let (state, _) = self
            .env
            .accounts_store()
            .load(self.env.now)
            .expect("the bench's records");
        let label = state.active.filter(|_| state.pinned)?;
        self.roster().row(&label).cloned()
    }

    /// The bench's Claude Code roster, as the verbs load it.
    fn roster(&self) -> accounts::Roster {
        accounts::Roster::load_for(
            Cli::Claude,
            &self.env.accounts_roster().expect("a roster path"),
        )
        .expect("the bench's roster")
    }

    /// What the token wrapper reads of a launch on `row`'s stored login under `roster`: this
    /// bench's home, its store build and no vault.
    fn launch_on(&self, row: &Row, roster: &accounts::Roster) -> accounts_token_run::Ctx {
        let home = self.env.home.clone().expect("a home");
        accounts_token_run::Ctx {
            env: vec![
                ("HOME".to_owned(), home.display().to_string()),
                (
                    accounts::LOGIN_ENV.to_owned(),
                    row.dir.clone().unwrap_or_default(),
                ),
            ],
            home: Some(home),
            cwd: self.env.cwd.clone(),
            roster: Some(roster.clone()),
            store: Some(self.env.accounts_store()),
            builds: self
                .build
                .parent()
                .and_then(std::path::Path::parent)
                .and_then(std::path::Path::parent)
                .map(std::path::Path::to_path_buf),
            #[cfg(unix)]
            vault: None,
            now: self.env.now,
            macos: true,
        }
    }

    /// `argv`, an accounts verb's (or the report of every pool, `accounts --cli all`), parsed as
    /// `aterm harness` parses it and run on `host` under the fixture's lab ([`Bench::run`] runs
    /// on this Mac's own). A parse refusal is a run that printed it on stderr.
    fn run_on(&self, argv: &[&str], host: &dyn cli::AccountsHost) -> Ran {
        let args: Vec<String> = argv.iter().map(|word| (*word).to_owned()).collect();
        let cmd = match cli::parse(&args) {
            Ok((cmd @ (Cmd::Accounts(_) | Cmd::AccountsAll { .. }), _)) => cmd,
            Ok(other) => panic!("{args:?} parses as {other:?}"),
            Err(why) => {
                return Ran {
                    ok: false,
                    out: String::new(),
                    err: why,
                };
            }
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = alab_roster::with_lab(self.lab.clone(), || match &cmd {
            Cmd::AccountsAll { json } => {
                cli::run_accounts_all(*json, &self.env, host, &mut out, &mut err)
            }
            Cmd::Accounts(form) => cli::run_accounts(form, &self.env, host, &mut out, &mut err),
            _ => unreachable!("an accounts verb"),
        });
        Ran {
            ok: code == ExitCode::SUCCESS,
            out: String::from_utf8_lossy(&out).into_owned(),
            err: String::from_utf8_lossy(&err).into_owned(),
        }
    }

    /// `argv` run as `aterm harness` runs it: parsed, then run under the fixture's lab. A parse
    /// refusal is a run that printed it on stderr.
    fn run(&self, argv: &[&str]) -> Ran {
        self.run_as(argv, &self.env, &self.lab)
    }

    /// [`Bench::run`] in `env` (the bench's, with what a verb reads of its home changed) under
    /// `lab`.
    fn run_as(&self, argv: &[&str], env: &Env, lab: &Lab) -> Ran {
        let args: Vec<String> = argv.iter().map(|word| (*word).to_owned()).collect();
        let cmd = match cli::parse(&args) {
            Ok((cmd, _)) => cmd,
            Err(why) => {
                return Ran {
                    ok: false,
                    out: String::new(),
                    err: why,
                };
            }
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = alab_roster::with_lab(lab.clone(), || cli::run(&cmd, env, &mut out, &mut err));
        Ran {
            ok: code == ExitCode::SUCCESS,
            out: String::from_utf8_lossy(&out).into_owned(),
            err: String::from_utf8_lossy(&err).into_owned(),
        }
    }
}

/// `err` past the prefix `aterm harness accounts <verb>: ` its refusal is printed with.
fn refusal(verb: &str, err: &str) -> String {
    let err = err.trim_end();
    err.strip_prefix(&format!("aterm harness accounts {verb}: "))
        .unwrap_or(err)
        .to_owned()
}

/// `accounts history` naming the account by `words` (none: every row's history), in JSON, whose
/// `ac` is the account it reached (with no word, whose `accounts` it listed), and in text.
fn history(bench: &Bench, words: &[&str]) -> Took {
    let run = |json: bool| {
        let mut argv = vec!["accounts", "history"];
        argv.extend_from_slice(words);
        if json {
            argv.push("--json");
        }
        bench.run(&argv)
    };
    let (json, text) = (run(true), run(false));
    let doc = || -> Value {
        aterm_json::from_str(json.out.trim())
            .unwrap_or_else(|why| panic!("{words:?}: {why}: {}", json.out))
    };
    let reached = if json.ok {
        Ok(doc()["ac"].as_str().unwrap_or_default().to_owned())
    } else {
        Err(refusal("history", &json.err))
    };
    let listed = if words.is_empty() && json.ok {
        doc()["accounts"]
            .as_array()
            .map(|accounts| {
                accounts
                    .iter()
                    .map(|account| {
                        account["ac"]
                            .as_str()
                            .or_else(|| account["label"].as_str())
                            .unwrap_or_default()
                            .to_owned()
                    })
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    Took {
        reached,
        exited: vec![json.ok, text.ok],
        listed,
        printed: vec![json.out, json.err, text.out, text.err],
        held_back: Vec::new(),
        faults: Vec::new(),
    }
}

/// `accounts use` naming the account by `words`, run with no pin standing, so the pin read back is
/// this run's: the key of the account of the row its pin holds, read back from the bench's
/// records, else its refusal; and what it printed. Its pinned line must name that account by its
/// names under the lab. The account it reached is then asked again while its row is recorded
/// signed out, and while its row has no login here: each is refused, naming the account by its
/// names first (`<shown> is signed-out`, `<shown> is not-enrolled`). Those refusals end in the
/// row's sign-in step, which names the account by its word ([`super::sign_in_word`]), and are
/// scanned with the rest.
fn use_pin(bench: &Bench, words: &[&str]) -> Took {
    let now = bench.env.now;
    let mut argv = vec!["accounts", "use"];
    argv.extend_from_slice(words);
    let typed = words.join(" ");
    let unpin = || {
        bench.edit(|state| {
            let _ = accounts::unpin(state, "auto", now);
        });
    };
    unpin();
    let ran = bench.run(&argv);
    let (mut refusals, mut faults) = (Vec::new(), Vec::new());
    let pinned = bench
        .pinned()
        .and_then(|row| names_of_row(Cli::Claude, &row, None).map(|(pk, _)| (pk, row)));
    let reached = match pinned {
        _ if !ran.ok => Err(refusal("use", &ran.err)),
        None => Err(format!("no row with an address is pinned: {}", ran.out)),
        Some((pk, row)) => {
            let shown = super::shown_row(Cli::Claude, &row, Some(&bench.lab));
            if !ran
                .out
                .starts_with(&format!("{shown}: the active account, pinned\n"))
            {
                faults.push(format!(
                    "`accounts use {typed}` does not name {pk} by its names ({shown}): {}",
                    ran.out
                ));
            }
            let login = std::path::Path::new(row.dir.as_deref().expect("a login folder"))
                .join("a-login-file");
            for condition in ["signed-out", "not-enrolled"] {
                unpin();
                let mut saved = None;
                if condition == "signed-out" {
                    bench.edit(|state| {
                        saved = state.accounts.get(&row.label).cloned();
                        let _ = accounts::note_auth(state, &row.label, Auth::SignedOut, now);
                    });
                } else {
                    std::fs::remove_file(&login).expect("the row's login");
                }
                let refused = bench.run(&argv);
                if condition == "signed-out" {
                    bench.edit(|state| match saved.take() {
                        Some(record) => {
                            state.accounts.insert(row.label.clone(), record);
                        }
                        None => {
                            state.accounts.remove(&row.label);
                        }
                    });
                } else {
                    std::fs::write(&login, "{}").expect("the row's login");
                }
                let why = refusal("use", &refused.err);
                if refused.ok || !why.starts_with(&format!("{shown} is {condition}")) {
                    faults.push(format!(
                        "`accounts use {typed}` with {pk}'s row {condition} gave ok={}: {} {}",
                        refused.ok, refused.out, refused.err
                    ));
                }
                refusals.push(why);
            }
            Ok(pk)
        }
    };
    let mut printed = vec![ran.out, ran.err];
    printed.extend(refusals);
    Took {
        reached,
        exited: vec![ran.ok],
        listed: Vec::new(),
        printed,
        held_back: Vec::new(),
        faults,
    }
}

/// The host `renew` and `login` run on over the bench: no terminal, and every sign-in a panic, so
/// a run that would start one fails the test and signs nothing in.
struct NoSignIn;

impl cli::AccountsHost for NoSignIn {
    fn stdin_is_terminal(&self) -> bool {
        false
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn now(&self) -> i64 {
        1_758_412_800
    }
    fn auth_status(&self, _: &str) -> Result<crate::harness::align::Captured, String> {
        panic!("no auth probe")
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        panic!("no login")
    }
    fn renew_flow(&self, _: &str, _: &str, _: &cli::RenewBrowser) -> Result<i32, String> {
        panic!("no renewal")
    }
    fn vendor_login(&self, _: &crate::harness::login_drive::Request<'_>) -> Result<i32, String> {
        panic!("no vendor login")
    }
    fn vendor_sign_in(
        &self,
        _: &crate::harness::login_drive::Request<'_>,
    ) -> crate::harness::login_drive::Drove {
        panic!("no vendor renewal")
    }
}

/// [`NoSignIn`], but Claude Code's status of the default login says it signs in as its address:
/// what `adopt-default` checks of the account it names.
struct StatusSays(String);

impl cli::AccountsHost for StatusSays {
    fn stdin_is_terminal(&self) -> bool {
        false
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn now(&self) -> i64 {
        1_758_412_800
    }
    fn auth_status(&self, login: &str) -> Result<crate::harness::align::Captured, String> {
        assert_eq!(login, "", "only the default login's status is read");
        Ok(crate::harness::align::Captured {
            stdout: format!(
                r#"{{"loggedIn":true,"authMethod":"claude.ai","email":"{}"}}"#,
                self.0
            )
            .into_bytes(),
            code: Some(0),
        })
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        panic!("no login")
    }
    fn renew_flow(&self, _: &str, _: &str, _: &cli::RenewBrowser) -> Result<i32, String> {
        panic!("no renewal")
    }
    fn vendor_login(&self, _: &crate::harness::login_drive::Request<'_>) -> Result<i32, String> {
        panic!("no vendor login")
    }
    fn vendor_sign_in(
        &self,
        _: &crate::harness::login_drive::Request<'_>,
    ) -> crate::harness::login_drive::Drove {
        panic!("no vendor renewal")
    }
}

/// The key of the bench's roster row whose account a message names `shown` (under the lab).
fn key_shown(bench: &Bench, shown: &str) -> Option<String> {
    bench.roster().rows.iter().find_map(|row| {
        (super::shown_row(Cli::Claude, row, Some(&bench.lab)) == shown)
            .then(|| names_of_row(Cli::Claude, row, None).map(|(pk, _)| pk))
            .flatten()
    })
}

/// `accounts renew` naming the account by `words`, on a host with no terminal ([`NoSignIn`]): a
/// renewal named there opens no page, so it says what is due here by the accounts' names (`due
/// here: <shown>`) and exits 1 having started nothing. The account it reached is the roster row's
/// that line names, else its refusal; it ended as the case expects (its `exited`) where it exited
/// 1 with that line and the no-terminal refusal. Its `--due` form picks what is due rather than
/// naming accounts, so `renew` names the accounts it takes ([`Case::TakesOne`]).
fn renew(bench: &Bench, words: &[&str]) -> Took {
    let mut argv = vec!["accounts", "renew"];
    argv.extend_from_slice(words);
    let ran = bench.run_on(&argv, &NoSignIn);
    let due = ran
        .out
        .lines()
        .find_map(|line| line.strip_prefix("due here: "));
    let reached = match due {
        Some(due) => key_shown(bench, due)
            .ok_or_else(|| format!("`due here:` names no roster row's account: {due}")),
        None => Err(refusal("renew", &ran.err)),
    };
    let ended = !ran.ok && due.is_some() && ran.err.contains("not run: stdin is not a terminal");
    Took {
        reached,
        exited: vec![ended],
        listed: Vec::new(),
        printed: vec![ran.out, ran.err],
        held_back: Vec::new(),
        faults: Vec::new(),
    }
}

/// `accounts login` naming the account by `words`, on a host that signs nothing in
/// ([`NoSignIn`]). Claude Code's `login` signs in only a row with no address, so a row with one is
/// refused naming its account first and the step that signs it in: its renewal, by the account's
/// word (`<shown> has an email, so it signs in through `aterm harness accounts renew <word>``),
/// which must name that account again. The account it reached is the roster row's that refusal
/// names, else its refusal; it ended as the case expects (its `exited`) where it exited 1 so.
fn login(bench: &Bench, words: &[&str]) -> Took {
    let mut argv = vec!["accounts", "login"];
    argv.extend_from_slice(words);
    let ran = bench.run_on(&argv, &NoSignIn);
    let why = refusal("login", &ran.err);
    let mut faults = Vec::new();
    let named = why
        .split_once(" has an email, so it signs in through `aterm harness accounts renew ")
        .and_then(|(shown, rest)| Some((key_shown(bench, shown)?, rest.split_once('`')?.0)));
    let reached = match named {
        Some((pk, word)) => {
            let typed = format!("accounts login {}", words.join(" "));
            faults.extend(offers_again(bench, &typed, ("renew", word), &pk));
            Ok(pk)
        }
        None => Err(why),
    };
    Took {
        exited: vec![!ran.ok && reached.is_ok()],
        reached,
        listed: Vec::new(),
        printed: vec![ran.out, ran.err],
        held_back: Vec::new(),
        faults,
    }
}

/// The key of the account `word` names on the bench (under its lab), where it names one and is
/// a word a step may type: no address, which a step never types (Decision 3).
fn step_names(bench: &Bench, word: &str) -> Result<String, String> {
    if word.contains('@') {
        return Err(format!("an address, which a step never types: `{word}`"));
    }
    let roster = bench.roster();
    let candidates = Candidates {
        roster: &roster.rows,
        logins: &[],
        lab: Some(&bench.lab),
        held: &[],
    };
    resolve(Cli::Claude, word, &candidates).map(|named| named.pk)
}

/// The fault where the step `typed` offers, `<verb> <word>`, names another account than `pk`
/// on the bench (under its lab), or none, or types an address ([`step_names`]).
fn offers_again(
    bench: &Bench,
    typed: &str,
    (verb, word): (&str, &str),
    pk: &str,
) -> Option<String> {
    match step_names(bench, word) {
        Ok(again) if again == pk => None,
        other => Some(format!(
            "`{typed}` offers `{verb} {word}`, which names {other:?}, not {pk}"
        )),
    }
}

/// `accounts add` naming the account by `words` ([`listed_or_added`]).
fn add(bench: &Bench, words: &[&str]) -> Took {
    listed_or_added(bench, "add", words)
}

/// `accounts adopt-default` naming the account by `words` ([`listed_or_added`]).
fn adopt_default(bench: &Bench, words: &[&str]) -> Took {
    listed_or_added(bench, "adopt-default", words)
}

/// One file as [`written_by_listing`] reads it: its bytes and when it was last written, so a
/// file replaced with the same bytes (the wake marker) is a write too.
type Written = (Option<Vec<u8>>, Option<std::time::SystemTime>);

/// What a refused `add` or `adopt-default` must leave as it was on `bench`: its roster, its
/// account store's records (`state.json`, `logins.json`, the ledger), the wake marker a written
/// row replaces, and the logins folder's listing (a login folder made for the account).
fn written_by_listing(bench: &Bench) -> Vec<(std::path::PathBuf, Written)> {
    let store = bench.env.accounts_store();
    let files = [
        bench.env.accounts_roster().expect("a roster path"),
        store.state_file(),
        store.logins_file(),
        store.ledger_path(),
        store.wake_path(),
    ];
    let mut out: Vec<(std::path::PathBuf, Written)> = files
        .into_iter()
        .map(|file| {
            let written = (
                std::fs::read(&file).ok(),
                std::fs::metadata(&file)
                    .and_then(|meta| meta.modified())
                    .ok(),
            );
            (file, written)
        })
        .collect();
    let mut listing: Vec<String> = std::fs::read_dir(store.logins_dir())
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    listing.sort();
    out.push((
        store.logins_dir(),
        (Some(listing.join("\n").into_bytes()), None),
    ));
    out
}

/// `verb` (`add`, `adopt-default`) naming the account by `words`, on a host that signs nothing in
/// ([`NoSignIn`]). Every account of the bench has a row, so the verb is refused naming the account
/// first and the step that signs its row in, which must name that account again (`<shown> is
/// already listed in <roster>; `aterm harness accounts renew <word>` signs it in`), and writes
/// nothing there: no roster, record, ledger row, wake marker or login folder
/// ([`written_by_listing`]). The account it reached is the roster row's that refusal names, else
/// its refusal; it ended as the case expects (its `exited`) where it exited 1 so. The account it
/// reached is then listed on a bench of its own whose roster lacks its row
/// ([`listed_on_a_bench_of_its_own`]), and `adopt-default` lists it once more there by the
/// address alone (`--email`, the form that names no account).
fn listed_or_added(bench: &Bench, verb: &str, words: &[&str]) -> Took {
    let mut argv = vec!["accounts", verb];
    argv.extend_from_slice(words);
    let typed = format!("accounts {verb} {}", words.join(" "));
    let before = written_by_listing(bench);
    let ran = bench.run_on(&argv, &NoSignIn);
    let why = refusal(verb, &ran.err);
    let mut faults = Vec::new();
    let named = why
        .split_once(" is already listed in ")
        .and_then(|(shown, rest)| {
            let (_, step) = rest.split_once("; `aterm harness accounts renew ")?;
            Some((key_shown(bench, shown)?, step.split_once('`')?.0))
        });
    let reached = match named {
        Some((pk, word)) => {
            faults.extend(offers_again(bench, &typed, ("renew", word), &pk));
            Ok(pk)
        }
        None => Err(why),
    };
    for ((file, was), (_, now)) in before.iter().zip(written_by_listing(bench)) {
        if *was != now {
            faults.push(format!("`{typed}` wrote {}: {}", file.display(), ran.out));
        }
    }
    let mut printed = vec![ran.out, ran.err];
    if let Ok(pk) = &reached {
        let (own_printed, own_faults) = listed_on_a_bench_of_its_own(bench, verb, words, pk);
        printed.extend(own_printed);
        faults.extend(own_faults);
        if verb == "adopt-default" {
            let address = bench
                .roster()
                .rows
                .iter()
                .find(|row| names_of_row(Cli::Claude, row, None).is_some_and(|(key, _)| key == *pk))
                .and_then(|row| row.email.clone())
                .unwrap_or_default();
            let (own_printed, own_faults) =
                listed_on_a_bench_of_its_own(bench, verb, &["--email", &address], pk);
            printed.extend(own_printed);
            faults.extend(own_faults);
        }
    }
    Took {
        exited: vec![!ran.ok && reached.is_ok()],
        reached,
        listed: Vec::new(),
        printed,
        held_back: Vec::new(),
        faults,
    }
}

/// `verb` (`add`, `adopt-default`) naming account `pk` by `words`, on a bench of its own over the
/// same lab whose roster lacks `pk`'s row, where Claude Code's status says the default login signs
/// in to that account ([`StatusSays`]): it exits 0 and lists one row of `pk`'s, labelled with
/// the account's reserved slug ([`keys::reserved_slug`]), and its receipt names the account by its
/// names under the lab (`listed <shown> (preferred) in <roster>`, `adopt-default`'s `listed <shown>
/// (preferred): this machine's own login`), and the step it offers to sign the row in (`run:
/// aterm harness accounts renew <word>`, `renew it with: …`) types a word that names `pk` again
/// among the roster as written, never an address ([`offers_again`]). What it printed, and what is
/// wrong with the run.
fn listed_on_a_bench_of_its_own(
    bench: &Bench,
    verb: &str,
    words: &[&str],
    pk: &str,
) -> (Vec<String>, Vec<String>) {
    let typed = format!("accounts {verb} {}", words.join(" "));
    let key_of_row = |row: &Row| names_of_row(Cli::Claude, row, None).map(|(pk, _)| pk);
    // The status `adopt-default` reads says the default login signs in as the account named.
    let address = bench
        .roster()
        .rows
        .iter()
        .find(|row| key_of_row(row).as_deref() == Some(pk))
        .and_then(|row| row.expected_identity_email().map(str::to_owned))
        .unwrap_or_default();
    let own = Bench::new(&Fixture {
        roster: bench
            .roster()
            .rows
            .into_iter()
            .filter(|row| key_of_row(row).as_deref() != Some(pk))
            .collect(),
        lab: bench.lab.clone(),
    });
    let mut argv = vec!["accounts", verb];
    argv.extend_from_slice(words);
    let ran = own.run_on(&argv, &StatusSays(address));
    let mut faults = Vec::new();
    let label = keys::reserved_slug("claude", pk);
    let roster = own.roster();
    let of_pk: Vec<&Row> = roster
        .rows
        .iter()
        .filter(|row| key_of_row(row).as_deref() == Some(pk))
        .collect();
    match of_pk.as_slice() {
        [row] if row.label == label => {
            let shown = super::shown_row(Cli::Claude, row, Some(&own.lab));
            let receipt = if verb == "add" {
                format!("listed {shown} (preferred) in ")
            } else {
                format!("listed {shown} (preferred): this machine's own login")
            };
            if !ran.ok || !ran.out.contains(&receipt) {
                faults.push(format!(
                    "`{typed}` on a roster without its row does not say `{receipt}`: {} {}",
                    ran.out, ran.err
                ));
            }
            let lead = if verb == "add" {
                "run: aterm harness accounts renew "
            } else {
                "renew it with: aterm harness accounts renew "
            };
            match ran.out.lines().find_map(|line| line.strip_prefix(lead)) {
                Some(word) => faults.extend(offers_again(&own, &typed, ("renew", word), pk)),
                None => faults.push(format!(
                    "`{typed}` on a roster without its row offers no `{lead}<word>` step: {}",
                    ran.out
                )),
            }
        }
        rows => faults.push(format!(
            "`{typed}` on a roster without its row listed {:?}, not one row labelled {label}: \
             ok={} {} {}",
            rows.iter()
                .map(|row| row.label.as_str())
                .collect::<Vec<_>>(),
            ran.ok,
            ran.out,
            ran.err
        )),
    }
    (vec![ran.out, ran.err], faults)
}

/// `accounts import` naming the account by `words`: Claude Code's pool has no shared source rows
/// to import, so the verb is refused naming the account first (`<shown>: Claude Code has no
/// pending source rows; `aterm harness accounts add <account>` lists an account`). The account it
/// reached is the roster row's that refusal names, else its refusal; it ended as the case expects
/// (its `exited`) where it exited 1 so.
fn import(bench: &Bench, words: &[&str]) -> Took {
    let mut argv = vec!["accounts", "import"];
    argv.extend_from_slice(words);
    let ran = bench.run_on(&argv, &NoSignIn);
    let why = refusal("import", &ran.err);
    let reached = match why
        .split_once(": Claude Code has no pending source rows")
        .and_then(|(shown, _)| key_shown(bench, shown))
    {
        Some(pk) => Ok(pk),
        None => Err(why),
    };
    Took {
        exited: vec![!ran.ok && reached.is_ok()],
        reached,
        listed: Vec::new(),
        printed: vec![ran.out, ran.err],
        held_back: Vec::new(),
        faults: Vec::new(),
    }
}

/// The host `browser` runs on at a desk over the bench: a terminal on stdin, a window browser
/// installed (a path nothing runs), a window that ends `done` (running `during` while it is
/// open) and a pass after it that drops no cookie and asks after no session (Claude Code's, which
/// aterm cannot see); every sign-in a panic, as [`NoSignIn`]'s.
struct Desk {
    during: Option<Box<dyn Fn()>>,
}

impl cli::AccountsHost for Desk {
    fn stdin_is_terminal(&self) -> bool {
        true
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn now(&self) -> i64 {
        1_758_412_800
    }
    fn auth_status(&self, _: &str) -> Result<crate::harness::align::Captured, String> {
        panic!("no auth probe")
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        panic!("no login")
    }
    fn renew_flow(&self, _: &str, _: &str, _: &cli::RenewBrowser) -> Result<i32, String> {
        panic!("no renewal")
    }
    fn vendor_login(&self, _: &crate::harness::login_drive::Request<'_>) -> Result<i32, String> {
        panic!("no vendor login")
    }
    fn vendor_sign_in(
        &self,
        _: &crate::harness::login_drive::Request<'_>,
    ) -> crate::harness::login_drive::Drove {
        panic!("no vendor renewal")
    }
    fn window_browser(&self) -> Option<std::path::PathBuf> {
        Some(std::path::PathBuf::from("/nowhere/Chromium"))
    }
    fn open_window(
        &self,
        _: &std::path::Path,
        _: &std::path::Path,
        _: &str,
    ) -> Result<crate::harness::browser_window::WindowEnd, String> {
        if let Some(during) = &self.during {
            during();
        }
        Ok(crate::harness::browser_window::WindowEnd::Done)
    }
    fn tidy_profile(
        &self,
        _: &std::path::Path,
        _: &std::path::Path,
        _: Option<(&str, &str)>,
    ) -> Result<crate::harness::browser_window::Tidied, String> {
        Ok(crate::harness::browser_window::Tidied {
            dropped: 0,
            session: None,
        })
    }
}

/// `accounts browser` naming the account by `words`, on a host with no terminal ([`NoSignIn`]):
/// the window it opens waits for a person to press Enter at a terminal, so it opens nothing and
/// says so, naming the account by the word of the step that opens it alone (`Run `aterm harness
/// accounts browser <word>``), never its address, as what reads it is captured stderr. The
/// account it reached is the one that word names, else its refusal; it ended as the case expects
/// (its `exited`) where it exited 1 so. The account it reached is then run again at a desk
/// ([`browser_at_a_desk`]).
fn browser(bench: &Bench, words: &[&str]) -> Took {
    let mut argv = vec!["accounts", "browser"];
    argv.extend_from_slice(words);
    let ran = bench.run_on(&argv, &NoSignIn);
    let why = refusal("browser", &ran.err);
    let mut faults = Vec::new();
    let word = why
        .strip_prefix(
            "not run: stdin is not a terminal (the window waits for you to sign in and press \
             Enter here). Run `aterm harness accounts browser ",
        )
        .and_then(|rest| rest.split_once('`'))
        .map(|(word, _)| word);
    let reached = match word.map(|word| (word, step_names(bench, word))) {
        Some((_, Ok(pk))) => Ok(pk),
        Some((word, Err(names))) => Err(format!("its step `browser {word}` names {names}")),
        None => Err(why.clone()),
    };
    if ran.err.contains('@') {
        faults.push(format!(
            "`accounts browser {}` with no terminal prints an address: {}",
            words.join(" "),
            ran.err
        ));
    }
    let mut printed = vec![ran.out, ran.err];
    if let Ok(pk) = &reached {
        let typed = format!("accounts browser {}", words.join(" "));
        let (desk_printed, desk_faults) = browser_at_a_desk(bench, &typed, pk);
        printed.extend(desk_printed);
        faults.extend(desk_faults);
    }
    Took {
        exited: vec![!ran.ok && reached.is_ok()],
        reached,
        listed: Vec::new(),
        printed,
        held_back: Vec::new(),
        faults,
    }
}

/// `accounts browser <pk>`, the account `typed` reached, run at a desk ([`Desk`]) on a bench of
/// its own over the same roster and lab, three times: a window that closes done, which names the
/// account by its names as it opens and closes (`opening the browser profile aterm keeps for
/// <shown> …`), exits 0 and offers its renewal by the account's word ([`offers_again`]); a window
/// during which the roster changes, whose result is not recorded, said with no label (`result
/// was not recorded: …`), exiting 1; and one while the row is in shared custody, refused naming
/// the account by its names (`<shown>: this account requires a fresh root-authorized shared
/// renewal`). What each printed, and what is wrong with each run.
fn browser_at_a_desk(bench: &Bench, typed: &str, pk: &str) -> (Vec<String>, Vec<String>) {
    let (mut printed, mut faults) = (Vec::new(), Vec::new());
    let own = Bench::new(&Fixture {
        roster: bench.roster().rows,
        lab: bench.lab.clone(),
    });
    let Some(row) = own
        .roster()
        .rows
        .into_iter()
        .find(|row| names_of_row(Cli::Claude, row, None).is_some_and(|(key, _)| key == pk))
    else {
        return (
            printed,
            vec![format!("`{typed}` reached {pk}, which no row holds")],
        );
    };
    let shown = super::shown_row(Cli::Claude, &row, Some(&own.lab));
    let argv = ["accounts", "browser", pk];
    let done = own.run_on(&argv, &Desk { during: None });
    let opened = done
        .out
        .lines()
        .find_map(|line| line.strip_prefix("opening the browser profile aterm keeps for "))
        .and_then(|rest| rest.split_once(" at https://"))
        .and_then(|(shown, _)| key_shown(&own, shown));
    if !done.ok || opened.as_deref() != Some(pk) {
        faults.push(format!(
            "`{typed}` at a desk gave ok={} opening {opened:?}, not {pk}: {}{}",
            done.ok, done.out, done.err
        ));
    }
    if !done
        .out
        .contains(&format!("{shown}: the window closed (done)"))
    {
        faults.push(format!(
            "`{typed}` at a desk does not name {pk} by its names ({shown}) as its window closes: \
             {}",
            done.out
        ));
    }
    match done
        .out
        .lines()
        .find_map(|line| line.strip_prefix("next: aterm harness accounts renew "))
        .and_then(|rest| rest.split_once(' '))
    {
        Some((word, _)) => faults.extend(offers_again(&own, typed, ("renew", word), pk)),
        None => faults.push(format!(
            "`{typed}` at a desk offers no renewal: {}",
            done.out
        )),
    }
    printed.extend([done.out, done.err]);
    // The roster changes while the window is open: another row is listed.
    let roster = own.env.accounts_roster().expect("a roster path");
    let moved = Desk {
        during: Some(Box::new(move || {
            let text = std::fs::read_to_string(&roster).expect("the roster");
            let zed = Row {
                label: "zed".to_owned(),
                email: Some("zed@x.example".to_owned()),
                ..Row::default()
            };
            let more = accounts::append_rows_for(Cli::Claude, &text, &[zed]).expect("a row");
            std::fs::write(&roster, more).expect("the roster");
        })),
    };
    let unrecorded = own.run_on(&argv, &moved);
    if unrecorded.ok || !unrecorded.err.contains("result was not recorded: ") {
        faults.push(format!(
            "`{typed}` at a desk whose roster changed during the window gave ok={}: {}{}",
            unrecorded.ok, unrecorded.out, unrecorded.err
        ));
    }
    printed.extend([unrecorded.out, unrecorded.err]);
    // The row in shared custody: a root's decision renews it, which no window here makes.
    let email = row.email.clone().expect("an address");
    let store = own.env.accounts_store();
    let staging = store.claim_staging_dir(&row.label).expect("staging");
    store
        .transact_logins(own.env.now - 10, &mut None, |_, logins| {
            let token = logins.shared.begin(
                accounts::shared::Intent {
                    source: accounts::shared::Source {
                        network: [1; 32],
                        account: [2; 32],
                    },
                    decision: [1; 32],
                    generation: 1,
                    configuration: [4; 32],
                    request: [5; 16],
                    label: row.label.clone(),
                    identity: email,
                    staging: staging.to_str().expect("utf-8").into(),
                    before_generation: None,
                    begun_at: own.env.now - 10,
                },
                None,
            )?;
            logins.shared.finish(
                token,
                accounts::shared::Outcome::Unknown,
                None,
                own.env.now - 10,
            )?;
            Ok(Vec::new())
        })
        .expect("custody");
    let held = own.run_on(&argv, &Desk { during: None });
    let custody = format!(
        "aterm harness accounts browser: {shown}: this account requires a fresh root-authorized \
         shared renewal"
    );
    if held.ok || !held.err.contains(&custody) {
        faults.push(format!(
            "`{typed}` at a desk under shared custody gave ok={}: {}{}",
            held.ok, held.out, held.err
        ));
    }
    printed.extend([held.out, held.err]);
    (printed, faults)
}

/// `accounts use --auto` after the pin of each roster row's account (by its key), whose line must
/// name that account by its names under the lab, and once more with no pin to end: whether each
/// run exited 0, and what each printed.
fn use_auto(bench: &Bench) -> Took {
    let (mut exited, mut printed, mut faults) = (Vec::new(), Vec::new(), Vec::new());
    for row in &bench.roster().rows {
        let Some((pk, _)) = names_of_row(Cli::Claude, row, None) else {
            continue;
        };
        let pinned = bench.run(&["accounts", "use", &pk]);
        if !pinned.ok {
            faults.push(format!("`accounts use {pk}` refused: {}", pinned.err));
            continue;
        }
        let ended = bench.run(&["accounts", "use", "--auto"]);
        let shown = super::shown_row(Cli::Claude, row, Some(&bench.lab));
        if ended.ok && !ended.out.starts_with(&format!("{shown}: no longer pinned")) {
            faults.push(format!(
                "`accounts use --auto` after {pk}'s pin does not name it ({shown}): {}",
                ended.out
            ));
        }
        exited.push(ended.ok);
        printed.extend([ended.out, ended.err]);
    }
    let none = bench.run(&["accounts", "use", "--auto"]);
    exited.push(none.ok);
    printed.extend([none.out, none.err]);
    Took {
        reached: Ok(String::new()),
        exited,
        listed: Vec::new(),
        printed,
        held_back: Vec::new(),
        faults,
    }
}

/// The report (`accounts`), in its text and its JSON, with the vault tokens section after it and
/// the lab journal's section of what every Mac saw of each row ([`seen_of`]), over the bench as
/// it is, then with a session on each of two rows, a third row chosen (not pinned) and the
/// fourth's record signed out, so its header, its rows and its `next:` and `not next:` lines each
/// name accounts, which [`report_faults`] holds it to printing; the bench's records are then put
/// back. Then over a bench of its own whose roster lists no row and whose default home's login
/// file names Ada's address ([`own_report`]), which the report stands for as one row. The shared
/// catalog's section is not run: the bench sets up no catalog; its `local` cell names a row as
/// the report does (`account_catalog::Local::words` under the report's listing), and its JSON
/// keys it (`local`, `local_own`), which `account_catalog`'s
/// `a_local_row_is_named_by_its_account_in_the_json_and_the_text` holds.
#[cfg(unix)]
fn report(bench: &Bench) -> Took {
    let (host, _) = token_host(bench);
    let (mut exited, mut printed, mut held_back) = (Vec::new(), Vec::new(), Vec::new());
    let mut faults = Vec::new();
    let mut each = |faults: &mut Vec<String>, check: bool| {
        let ran = report_runs(bench, &host, &["accounts"], &mut held_back);
        if check {
            faults.extend(report_faults(bench, &ran));
        }
        for (ok, out, err) in ran {
            exited.push(ok);
            printed.extend([out, err]);
        }
    };
    each(&mut faults, false);
    let labels: Vec<String> = bench
        .roster()
        .rows
        .iter()
        .map(|r| r.label.clone())
        .collect();
    let now = bench.env.now;
    let mut before = None;
    bench.edit(|state| {
        before = Some(state.clone());
        for (sid, label) in ["s-one", "s-two"].iter().zip(&labels) {
            state.sessions.insert(
                (*sid).to_owned(),
                accounts::SessionRow {
                    account: label.clone(),
                    at: now - 60,
                    held: None,
                    owner: None,
                },
            );
        }
        state.active = labels.get(2).cloned();
        state.pinned = false;
        if let Some(last) = labels.get(3) {
            let _ = accounts::note_auth(state, last, Auth::SignedOut, now - 120);
        }
    });
    each(&mut faults, true);
    bench.edit(|state| *state = before.take().expect("the bench's records"));
    let (own_exited, own_printed, own_faults) = own_report(bench);
    exited.extend(own_exited);
    printed.extend(own_printed);
    faults.extend(own_faults);
    Took {
        reached: Ok(String::new()),
        exited,
        listed: Vec::new(),
        printed,
        held_back,
        faults,
    }
}

/// `argv` (a report's, `accounts` or `accounts --cli all`) run on the bench in its JSON and its
/// text, on `host`, under the lab journal's keyer and its snapshot of the bench's rows
/// ([`seen_of`]): whether each exited 0, its stdout (the JSON's with each row's login folder held
/// back into `held`, [`login_folders_held_back`]) and its stderr, the JSON's first.
#[cfg(unix)]
fn report_runs(
    bench: &Bench,
    host: &dyn cli::AccountsHost,
    argv: &[&str],
    held: &mut Vec<String>,
) -> Vec<(bool, String, String)> {
    [true, false]
        .into_iter()
        .map(|json| {
            let mut argv = argv.to_vec();
            if json {
                argv.push("--json");
            }
            let ran = crate::harness::alab_seen::with_seen(seen_of(bench), || {
                crate::harness::journal::with_keyer(lab_keyer, || bench.run_on(&argv, host))
            });
            let out = if json {
                login_folders_held_back(&ran.out, held)
            } else {
                ran.out
            };
            (ran.ok, out, ran.err)
        })
        .collect()
}

/// What the lab journal says of each roster row of the bench: a Mac (`m9`, no roster label) used
/// it ten minutes ago. Every row then has a line of the report's journal section.
#[cfg(unix)]
fn seen_of(bench: &Bench) -> crate::harness::alab_seen::Seen {
    use crate::harness::alab_seen::{AccountSeen, LastUse, Seen};
    let at = bench.env.now;
    let accounts = bench
        .roster()
        .rows
        .iter()
        .filter_map(|row| names_of_row(Cli::Claude, row, None))
        .map(|(pk, _)| {
            let seen = AccountSeen {
                last_use: Some(LastUse {
                    by: "m9".into(),
                    t: at - 600,
                }),
                ..AccountSeen::default()
            };
            (pk, seen)
        })
        .collect();
    Seen {
        accounts,
        at,
        me: BTreeSet::new(),
    }
}

/// What is wrong with the report `ran` ([`report_runs`], its JSON then its text) over the bench
/// with sessions on its first two rows, its third chosen and its fourth signed out ([`report`]):
/// each line the scan is to read must be there and name its accounts by their names (the
/// `active:` line both rows with a session and the chosen one, a `next:` line an account, a `not
/// next:` line the signed-out one, the journal section each row), and the JSON each by its key
/// (`running` the two rows with a session, `active` the chosen one, `next.to` a row and
/// `next.not[]` the signed-out one, the journal each row's key with its name).
#[cfg(unix)]
fn report_faults(bench: &Bench, ran: &[(bool, String, String)]) -> Vec<String> {
    let mut faults = Vec::new();
    let [(_, json, _), (_, text, _)] = ran else {
        return vec![format!("the report ran {} forms, not 2", ran.len())];
    };
    let rows = bench.roster().rows;
    let shown: Vec<String> = rows
        .iter()
        .map(|row| super::shown_row(Cli::Claude, row, Some(&bench.lab)))
        .collect();
    let keys: Vec<String> = rows
        .iter()
        .filter_map(|row| names_of_row(Cli::Claude, row, None).map(|(pk, _)| pk))
        .collect();
    if shown.len() < 4 || keys.len() < 4 {
        return vec![format!(
            "the bench has {} rows with an address, not 4",
            keys.len()
        )];
    }
    let line = |head: &str| text.lines().find(|line| line.starts_with(head));
    match line("active: ") {
        Some(active)
            if active.contains(&format!("{} (1)", shown[0]))
                && active.contains(&format!("{} (1)", shown[1]))
                && active.contains(&format!("(chosen: {})", shown[2])) => {}
        other => faults.push(format!(
            "the report's `active:` line does not name {}, {} and the chosen {}: {other:?}",
            shown[0], shown[1], shown[2]
        )),
    }
    match line("next: ") {
        Some(next)
            if shown
                .iter()
                .any(|shown| next.starts_with(&format!("next: {shown}"))) => {}
        other => faults.push(format!(
            "the report's `next:` line names no account by its names: {other:?}"
        )),
    }
    match line("not next: ") {
        Some(not) if not.contains(&format!("{}: ", shown[3])) => {}
        other => faults.push(format!(
            "the report's `not next:` line does not name the signed-out {}: {other:?}",
            shown[3]
        )),
    }
    let journal = text
        .split_once("\nlab journal")
        .map_or("", |(_, journal)| journal);
    for shown in &shown {
        if !journal.contains(&format!("  {shown}: last used by m9")) {
            faults.push(format!(
                "the report's journal section does not name {shown}: {journal:?}"
            ));
        }
    }
    let Ok(doc) = aterm_json::from_str::<Value>(json.trim()) else {
        faults.push(format!("the report's JSON does not parse: {json}"));
        return faults;
    };
    let ids = |v: &Value| -> Vec<String> {
        v.as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect()
    };
    if ids(&doc["running"]) != keys[..2] {
        faults.push(format!(
            "the JSON's `running` is {}, not the keys {:?}",
            doc["running"],
            &keys[..2]
        ));
    }
    if doc["active"] != Value::from(keys[2].clone()) {
        faults.push(format!(
            "the JSON's `active` is {}, not the chosen key {}",
            doc["active"], keys[2]
        ));
    }
    if !doc["next"]["to"]
        .as_str()
        .is_some_and(|to| keys.iter().any(|key| key == to))
    {
        faults.push(format!(
            "the JSON's `next.to` is {}, no row's key",
            doc["next"]["to"]
        ));
    }
    let not: Vec<&Value> = doc["next"]["not"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|not| &not["account"])
        .collect();
    if !not.contains(&&Value::from(keys[3].clone())) {
        faults.push(format!(
            "the JSON's `next.not` does not name the signed-out key {}: {}",
            keys[3], doc["next"]["not"]
        ));
    }
    for (key, row) in keys.iter().zip(&rows) {
        let names = names_of_row(Cli::Claude, row, Some(&bench.lab)).map(|(_, names)| names);
        let journal = &doc["journal"][key.as_str()];
        if journal["ac"] != Value::from(key.clone())
            || journal["name"] != Value::from(names.and_then(|names| names.name))
        {
            faults.push(format!(
                "the JSON's journal does not name {key} by its key and name: {}",
                doc["journal"]
            ));
        }
    }
    faults
}

/// The report over a bench of its own whose roster lists no row and whose default home's login
/// file names Ada's address, its own login's record holding a reading, so the report stands for
/// that login as one row (`Roster::own`): its text names it by Ada's account's names, and its
/// JSON row by Ada's key, slug and name, with no label. Whether each run exited 0, what each
/// printed, and what is wrong.
#[cfg(unix)]
fn own_report(bench: &Bench) -> (Vec<bool>, Vec<String>, Vec<String>) {
    let own = Bench::new(&Fixture {
        roster: Vec::new(),
        lab: bench.lab.clone(),
    });
    let home = own.env.home.clone().expect("a home");
    std::fs::create_dir_all(&home).expect("the home");
    std::fs::write(
        home.join(".claude.json"),
        format!(
            "{{\"oauthAccount\":{{\"accountUuid\":\"uuid-own\",\"emailAddress\":\"{ADA}\",\
             \"organizationType\":\"claude_max\"}}}}"
        ),
    )
    .expect("the own login's file");
    let now = own.env.now;
    own.edit(|state| {
        state
            .accounts
            .entry(cli::ACCOUNTS_OWN_LABEL.to_owned())
            .or_default()
            .sample = Some(accounts::Sample {
            week_pct: Some(30),
            at: now - 60,
            ..accounts::Sample::default()
        });
    });
    let (host, _) = token_host(&own);
    let ran = report_runs(&own, &host, &["accounts"], &mut Vec::new());
    let pk = key("claude", ADA);
    let names = &own.lab.names[&pk];
    let shown = Named {
        pk: pk.clone(),
        row: None,
        names: names.clone(),
    }
    .shown();
    let mut faults = Vec::new();
    if let [(_, json, _), (_, text, _)] = ran.as_slice() {
        let doc = aterm_json::from_str::<Value>(json.trim()).unwrap_or(Value::Null);
        let row = &doc["accounts"][0];
        if row["ac"] != Value::from(pk.clone())
            || row["slug"] != Value::from(names.slug.clone())
            || row["name"] != Value::from(names.name.clone())
            || row.get("label").is_some()
        {
            faults.push(format!(
                "a roster of no row reports its own login as {row}, not Ada's account {pk}"
            ));
        }
        if !text
            .lines()
            .any(|line| line.starts_with(&format!("{shown}, read by ")))
        {
            faults.push(format!(
                "a roster of no row does not name its own login {shown}: {text}"
            ));
        }
    } else {
        faults.push(format!("the own report ran {} forms, not 2", ran.len()));
    }
    let mut exited = Vec::new();
    let mut printed = Vec::new();
    for (ok, out, err) in ran {
        exited.push(ok);
        printed.extend([out, err]);
    }
    (exited, printed, faults)
}

/// `accounts --cli all`, in its JSON and its text, over the bench under the journal's snapshot
/// ([`report_runs`]): the report of each pool this Mac keeps a roster of (the bench's Claude Code
/// pool), then the vault tokens section. Its pool names each row as the report does: its text
/// each by its names, its JSON each by its key (no row keeps a label, each having an address).
/// The shared catalog is not run, as in [`report`].
#[cfg(unix)]
fn all_pools(bench: &Bench) -> Took {
    let (host, _) = token_host(bench);
    let mut held_back = Vec::new();
    let ran = report_runs(bench, &host, &["accounts", "--cli", "all"], &mut held_back);
    let mut faults = Vec::new();
    if let [(_, json, _), (_, text, _)] = ran.as_slice() {
        let doc = aterm_json::from_str::<Value>(json.trim()).unwrap_or(Value::Null);
        let pool = &doc["pools"][0];
        let listed: Vec<Value> = pool["report"]["accounts"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|row| row["ac"].clone())
            .collect();
        let rows = bench.roster().rows;
        let keys: Vec<Value> = rows
            .iter()
            .filter_map(|row| names_of_row(Cli::Claude, row, None).map(|(pk, _)| Value::from(pk)))
            .collect();
        if pool["cli"] != Value::from("claude") || listed != keys {
            faults.push(format!(
                "`accounts --cli all --json` lists {listed:?}, not the keys {keys:?}: {pool}"
            ));
        }
        for row in &rows {
            let shown = super::shown_row(Cli::Claude, row, Some(&bench.lab));
            if !text
                .lines()
                .any(|line| line.starts_with(&format!("{shown} ")))
            {
                faults.push(format!(
                    "`accounts --cli all` has no row line of {shown}: {text}"
                ));
            }
        }
    } else {
        faults.push(format!(
            "`accounts --cli all` ran {} forms, not 2",
            ran.len()
        ));
    }
    let mut exited = Vec::new();
    let mut printed = Vec::new();
    for (ok, out, err) in ran {
        exited.push(ok);
        printed.extend([out, err]);
    }
    Took {
        reached: Ok(String::new()),
        exited,
        listed: Vec::new(),
        printed,
        held_back,
        faults,
    }
}

/// The JSON report `json` with each row's `dir` taken out into `held`, in every pool's report of
/// `--cli all` too: a login folder is named by its row's label until SF-10 names them by their
/// accounts, so the path is held back ([`Took::held_back`]), never scanned as done.
#[cfg(unix)]
fn login_folders_held_back(json: &str, held: &mut Vec<String>) -> String {
    fn take(value: &mut Value, held: &mut Vec<String>) {
        if let Some(object) = value.as_object_mut() {
            if let Some(rows) = object.get_mut("accounts").and_then(Value::as_array_mut) {
                for row in rows.iter_mut().filter_map(Value::as_object_mut) {
                    if let Some(Value::String(dir)) = row.remove("dir") {
                        held.push(dir);
                    }
                }
            }
            for pool in object
                .get_mut("pools")
                .and_then(Value::as_array_mut)
                .into_iter()
                .flatten()
            {
                if let Some(report) = pool.as_object_mut().and_then(|pool| pool.get_mut("report")) {
                    take(report, held);
                }
            }
        }
    }
    let Ok(mut doc) = aterm_json::from_str::<Value>(json.trim()) else {
        return json.to_owned();
    };
    take(&mut doc, held);
    doc.to_string()
}

/// The report's case: Pending where the token section is not built.
#[cfg(not(unix))]
fn report_case() -> Case {
    Case::Pending
}

/// The report's case ([`report`]).
#[cfg(unix)]
fn report_case() -> Case {
    Case::Prints(report)
}

/// `accounts --cli all`'s case: Pending where the token section is not built.
#[cfg(not(unix))]
fn all_pools_case() -> Case {
    Case::Pending
}

/// `accounts --cli all`'s case ([`all_pools`]).
#[cfg(unix)]
fn all_pools_case() -> Case {
    Case::Prints(all_pools)
}

/// `text` with each of `dirs` (login folders, which a place prints and which are named by their
/// rows' labels until SF-10 names them by their accounts, as [`login_folders_held_back`] holds
/// back the report's) put as `<login folder>`, each one found held back into `held`.
fn login_paths_held_back(text: &str, dirs: &[String], held: &mut Vec<String>) -> String {
    let mut text = text.to_owned();
    for dir in dirs {
        if text.contains(dir.as_str()) {
            held.push(dir.clone());
            text = text.replace(dir.as_str(), "<login folder>");
        }
    }
    text
}

/// A [`Case::Prints`] case's [`Took`] from its runs (each whether it ended as the case expects,
/// its stdout and its stderr), what it held back and what it found wrong.
fn printed_by(
    ran: Vec<(bool, String, String)>,
    held_back: Vec<String>,
    faults: Vec<String>,
) -> Took {
    let mut exited = Vec::new();
    let mut printed = Vec::new();
    for (ok, out, err) in ran {
        exited.push(ok);
        printed.extend([out, err]);
    }
    Took {
        reached: Ok(String::new()),
        exited,
        listed: Vec::new(),
        printed,
        held_back,
        faults,
    }
}

/// `accounts stats`, in its JSON and its text, on each roster row's home (the environment naming
/// the row's login folder as Claude Code's home, with no profile there): its JSON keys the row's
/// account with its names and no label, and its text names it by its names under the lab; then
/// on the default home, which no row holds. Then `accounts stats --cli codex` on a bench of its
/// own whose Codex roster lists Ada's OpenAI account under the label Ada's Claude Code row has
/// here, whose JSON keys that account and whose text names it the same way.
fn stats(bench: &Bench) -> Took {
    let (mut ran, mut faults) = (Vec::new(), Vec::new());
    for row in &bench.roster().rows {
        let (Some(dir), Some((pk, names))) = (
            row.dir.as_deref(),
            names_of_row(Cli::Claude, row, Some(&bench.lab)),
        ) else {
            continue;
        };
        let mut env = bench.env.clone();
        env.claude_dir = Some(dir.into());
        env.claude_profile = Some(std::path::Path::new(dir).join(".claude.json"));
        let json = bench.run_as(&["accounts", "stats", "--json"], &env, &bench.lab);
        let text = bench.run_as(&["accounts", "stats"], &env, &bench.lab);
        let doc = aterm_json::from_str::<Value>(json.out.trim()).unwrap_or(Value::Null);
        let home = &doc["accounts"][0];
        if home["ac"] != Value::from(pk.as_str())
            || home["slug"] != Value::from(names.slug.clone())
            || home["name"] != Value::from(names.name.clone())
            || home.get("label").is_some()
        {
            faults.push(format!(
                "`accounts stats --json` on {pk}'s home does not key it by its names: {}",
                json.out
            ));
        }
        let shown = super::shown_row(Cli::Claude, row, Some(&bench.lab));
        if !text
            .out
            .lines()
            .any(|line| line.starts_with(&format!("{shown} \u{b7} claude")))
        {
            faults.push(format!(
                "`accounts stats` on {pk}'s home does not name it ({shown}): {}",
                text.out
            ));
        }
        for form in [json, text] {
            ran.push((form.ok, form.out, form.err));
        }
    }
    for argv in [&["accounts", "stats", "--json"][..], &["accounts", "stats"]] {
        let form = bench.run(argv);
        ran.push((form.ok, form.out, form.err));
    }
    let own = Bench::new(&fixture());
    let codex = row("m2-2", ADA);
    std::fs::write(
        own.env
            .accounts_roster_for(Cli::Codex)
            .expect("a roster path"),
        accounts::append_rows_for(Cli::Codex, "", std::slice::from_ref(&codex))
            .expect("the row admits"),
    )
    .expect("Codex's roster");
    let (pk, names) = names_of_row(Cli::Codex, &codex, Some(&own.lab)).expect("a key");
    assert_eq!(pk, key("codex", ADA));
    let json = own.run(&["accounts", "stats", "--cli", "codex", "--json"]);
    let text = own.run(&["accounts", "stats", "--cli", "codex"]);
    let doc = aterm_json::from_str::<Value>(json.out.trim()).unwrap_or(Value::Null);
    let rows: Vec<&Value> = doc["rows"].as_array().into_iter().flatten().collect();
    if !matches!(rows.as_slice(), [one] if one["ac"] == Value::from(pk.as_str())
        && one["slug"] == Value::from(names.slug.clone())
        && one["name"] == Value::from(names.name.clone())
        && one.get("label").is_none())
    {
        faults.push(format!(
            "`accounts stats --cli codex --json` does not key its one row as {pk}: {}",
            json.out
        ));
    }
    let shown = super::shown_row(Cli::Codex, &codex, Some(&own.lab));
    if !text.out.contains(&format!("  {shown}: ")) {
        faults.push(format!(
            "`accounts stats --cli codex` does not name its row ({shown}): {}",
            text.out
        ));
    }
    for form in [json, text] {
        ran.push((form.ok, form.out, form.err));
    }
    printed_by(ran, Vec::new(), faults)
}

/// `accounts observed`, in its JSON and its text: each roster row's place believes the row's
/// account, in the JSON (a schema 2 `accounts-observed` document) by its key, slug and name with
/// no label (`believed`'s `ac`, `slug`, `name`) and in the text by its names. Each place is its
/// row's login folder, which is held back ([`login_paths_held_back`]).
fn observed(bench: &Bench) -> Took {
    let rows = bench.roster().rows;
    let dirs: Vec<String> = rows.iter().filter_map(|row| row.dir.clone()).collect();
    let mut held_back = Vec::new();
    let json = bench.run(&["accounts", "observed", "--json"]);
    let text = bench.run(&["accounts", "observed"]);
    let json_out = login_paths_held_back(&json.out, &dirs, &mut held_back);
    let text_out = login_paths_held_back(&text.out, &dirs, &mut held_back);
    let doc = aterm_json::from_str::<Value>(json_out.trim()).unwrap_or(Value::Null);
    let mut faults = Vec::new();
    if (&doc["kind"], &doc["schema"]) != (&Value::from("accounts-observed"), &Value::from(2u64)) {
        faults.push(format!(
            "`accounts observed --json` is not a schema 2 `accounts-observed` document: {json_out}"
        ));
    }
    let places: Vec<&Value> = doc["places"].as_array().into_iter().flatten().collect();
    for row in &rows {
        let Some((pk, names)) = names_of_row(Cli::Claude, row, Some(&bench.lab)) else {
            continue;
        };
        if !places.iter().any(|place| {
            let believed = &place["believed"];
            believed["ac"] == Value::from(pk.as_str())
                && believed["slug"] == Value::from(names.slug.clone())
                && believed["name"] == Value::from(names.name.clone())
                && believed.get("label").is_none()
        }) {
            faults.push(format!(
                "`accounts observed --json` believes no place is {pk}: {json_out}"
            ));
        }
        let shown = super::shown_row(Cli::Claude, row, Some(&bench.lab));
        if !text_out
            .lines()
            .any(|line| line.contains(&format!(" {shown} ")))
        {
            faults.push(format!(
                "`accounts observed` believes no place is {shown}: {text_out}"
            ));
        }
    }
    printed_by(
        vec![(json.ok, json_out, json.err), (text.ok, text_out, text.err)],
        held_back,
        faults,
    )
}

/// `accounts refresh --cli kimi` over a quota tab's control socket ([`RefreshCtl`]), on a bench
/// of its own whose Kimi roster lists Ada's address under the label Ada's Claude Code row has
/// here: a read that succeeds, in its text and its JSON, and once more with Kimi's quota wall on
/// the panel's screen, which prints the wall and no line of Claude Code's rotation (whose roster
/// lists Ada's row by the same label); one that meets Kimi's sign-in wall,
/// which names the row's account by its names as it records it signed out (exit 1); the next,
/// refused before any tab opens, naming it the same way (exit 1); and, on a roster whose row has a
/// login folder no login is enrolled in, the refusal that says so by its names (exit 1).
fn refresh(bench: &Bench) -> Took {
    use crate::harness::cli::tests::RefreshCtl;
    let own = Bench::new(&fixture());
    let path = own
        .env
        .accounts_roster_for(Cli::Kimi)
        .expect("a roster path");
    let roster = |label: &str, email: &str, dir: Option<&str>| {
        let dir = dir.map_or_else(String::new, |dir| format!("dir = '{dir}'\n"));
        std::fs::write(
            &path,
            format!(
                "[[account]]\nlabel = '{label}'\nclass = 'dedicated'\nemail = '{email}'\n\
                 region = 'global'\n{dir}"
            ),
        )
        .expect("Kimi's roster");
        let rows = accounts::Roster::load_for(Cli::Kimi, &path)
            .expect("Kimi's roster")
            .rows;
        super::shown_row(Cli::Kimi, &rows[0], Some(&bench.lab))
    };
    let shown = roster("m2-2", ADA, None);
    let (mut ran, mut faults) = (Vec::new(), Vec::new());
    let run = |ctl: &mut RefreshCtl, json: bool| {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = alab_roster::with_lab(bench.lab.clone(), || {
            cli::run_kimi_refresh_with(ctl, &own.env, json, &mut out, &mut err)
        });
        (
            code,
            String::from_utf8_lossy(&out).into_owned(),
            String::from_utf8_lossy(&err).into_owned(),
        )
    };
    for json in [false, true] {
        let (code, out, err) = run(&mut RefreshCtl::default(), json);
        ran.push((code == ExitCode::SUCCESS, out, err));
    }
    // The panel read under Kimi Code's quota wall: the read's wall line is Kimi's, and no line
    // about Claude Code's rotation follows it.
    let mut quota = RefreshCtl::default();
    quota.quota_wall = true;
    let (code, out, err) = run(&mut quota, false);
    if !out.starts_with("wall=usage-session")
        || out.lines().any(|line| line.starts_with("account="))
    {
        faults.push(format!(
            "`accounts refresh --cli kimi` under Kimi's quota wall does not print its wall alone: \
             {out}"
        ));
    }
    ran.push((code == ExitCode::SUCCESS, out, err));
    let mut walled = RefreshCtl::default();
    walled.timeout_at = Some(5);
    walled.signed_out_screen = true;
    let (code, out, err) = run(&mut walled, false);
    let recorded =
        format!("aterm harness accounts refresh: {shown} is signed out on this Mac (recorded)");
    if !err.contains(&recorded) {
        faults.push(format!(
            "`accounts refresh --cli kimi` at Kimi's sign-in wall does not say `{recorded}`: {err}"
        ));
    }
    ran.push((code == ExitCode::from(1), out, err));
    let mut refused = RefreshCtl::default();
    let (code, out, err) = run(&mut refused, false);
    let signed_out = format!(
        "aterm harness accounts refresh: {shown} is signed out on this Mac; no Kimi was started"
    );
    if !err.starts_with(&signed_out) || !refused.calls.is_empty() {
        faults.push(format!(
            "`accounts refresh --cli kimi` on a row signed out does not refuse it as `{signed_out}` \
             (calls {:?}): {err}",
            refused.calls
        ));
    }
    ran.push((code == ExitCode::from(1), out, err));
    let folder = own.env.state.join("kimi-login");
    let shown = roster("claude-05b50c", BEA, folder.to_str());
    let mut refused = RefreshCtl::default();
    let (code, out, err) = run(&mut refused, false);
    let unenrolled = format!("aterm harness accounts refresh: {shown} has no enrolled login here");
    if err.trim_end() != unenrolled {
        faults.push(format!(
            "`accounts refresh --cli kimi` on a row with no enrolled login does not say \
             `{unenrolled}`: {err}"
        ));
    }
    ran.push((code == ExitCode::from(1), out, err));
    printed_by(ran, Vec::new(), faults)
}

/// `accounts table`, in its JSON and its text, under the fixture's lab, which names no machine
/// here, so no pool has a source; then under that lab naming this Mac, which prefers Ada's and
/// Bea's Claude accounts and Ada's OpenAI one, so each pool's source is the lab: each preferred
/// account is listed by its key and names (Ada's and Bea's by their rows here), and each other
/// roster row as a row of this Mac alone, by its names; the JSON keys each by its key and slug
/// and keeps no row's label.
fn table(bench: &Bench) -> Took {
    let mut ran = Vec::new();
    for argv in [&["accounts", "table", "--json"][..], &["accounts", "table"]] {
        let form = bench.run(argv);
        ran.push((form.ok, form.out, form.err));
    }
    let me = alab_roster::test_machine("m9");
    let mut lab = bench.lab.clone();
    lab.me = Some(me.clone());
    for account in &mut lab.accounts {
        if account.email == ADA || account.email == BEA {
            account.preferred = vec![(me.clone(), Some(1))];
        }
    }
    let json = bench.run_as(&["accounts", "table", "--json"], &bench.env, &lab);
    let text = bench.run_as(&["accounts", "table"], &bench.env, &lab);
    let doc = aterm_json::from_str::<Value>(json.out.trim()).unwrap_or(Value::Null);
    let pool = |cli: Cli| {
        doc["pools"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|pool| pool["cli"] == Value::from(cli.id()))
            .map(|pool| pool["accounts"].as_array().cloned().unwrap_or_default())
            .unwrap_or_default()
    };
    let mut faults = Vec::new();
    let mut each = |cli: Cli, pk: String, slug: String, shown: String| {
        if !pool(cli).iter().any(|row| {
            row["ac"] == Value::from(pk.as_str())
                && row["slug"] == Value::from(slug.as_str())
                && row.get("label").is_none()
        }) {
            faults.push(format!(
                "`accounts table --json` lists no {} row as {pk}: {}",
                cli.id(),
                json.out
            ));
        }
        let start = format!("{:<6} {shown} ", cli.id());
        if !text.out.lines().any(|line| line.starts_with(&start)) {
            faults.push(format!(
                "`accounts table` has no {} row of {shown}: {}",
                cli.id(),
                text.out
            ));
        }
    };
    for row in &bench.roster().rows {
        if let Some((pk, names)) = names_of_row(Cli::Claude, row, Some(&lab)) {
            each(
                Cli::Claude,
                pk,
                names.slug,
                super::shown_row(Cli::Claude, row, Some(&lab)),
            );
        }
    }
    let ada = names_of_login(Cli::Codex, None, ADA, Some(&lab)).expect("Ada's OpenAI account");
    let slug = ada.1.slug.clone();
    let shown = Named {
        pk: ada.0.clone(),
        row: None,
        names: ada.1,
    }
    .shown();
    each(Cli::Codex, ada.0, slug, shown);
    for form in [json, text] {
        ran.push((form.ok, form.out, form.err));
    }
    printed_by(ran, Vec::new(), faults)
}

/// `ledger accounts`, on a bench of its own: its text names the account of each of the bench's
/// ledger rows (an auth row about each roster row and a rotation from each to the next, both kept
/// by label) by its key and slug ([`Named::logged`]), and so does a reason whose grammar puts an
/// account in it: a displacement of Ada's row by Bea's, `default login now <Bea's label>` as the
/// ledger keeps it ([`accounts::displaced_why`]), reads `default login now` and Bea's key and
/// slug. A row this build cannot read, which holds a label, is printed as it stands, as the JSON
/// prints every row: it is held back with them, the JSON printing the rows verbatim, as the
/// ledger keeps them, by label until SF-10 keys the ledger by account. Then its text over the
/// bench every verb ran on, every row the ledger keeps, which the verbs before it wrote to (pins,
/// sign-ins), named the same way.
fn ledger(bench: &Bench) -> Took {
    let own = Bench::new(&fixture());
    let roster = own.roster().rows;
    let logged = |row: &Row| {
        names_of_row(Cli::Claude, row, Some(&own.lab)).map(|(pk, names)| {
            Named {
                pk,
                row: Some(row),
                names,
            }
            .logged()
        })
    };
    let (ada, bea) = (&roster[0], &roster[1]);
    own.env
        .accounts_store()
        .transact(own.env.now, &mut None, |_| {
            Ok(vec![
                Event::new(own.env.now - 30, Kind::Displaced, &ada.label)
                    .with_to(&bea.label)
                    .because(&accounts::displaced_why(&bea.label)),
            ])
        })
        .expect("the displacement");
    let unread = format!("{{\"account\":\"{}\",", ada.label);
    let ledger = own.env.accounts_store().ledger_path();
    let mut kept = std::fs::read_to_string(&ledger).expect("the ledger");
    kept.push_str(&unread);
    kept.push('\n');
    std::fs::write(&ledger, kept).expect("a row this build cannot read");
    let text = own.run(&["ledger", "accounts", "64"]);
    let json = own.run(&["ledger", "accounts", "64", "--json"]);
    let mut faults = Vec::new();
    for row in &roster {
        let Some(logged) = logged(row) else {
            continue;
        };
        let named = |kind: &str| {
            text.out
                .lines()
                .any(|line| line.contains(&format!(" {kind} {logged}")))
        };
        if !named("auth") || !named("rotate") || !text.out.contains(&format!(" -> {logged}")) {
            faults.push(format!(
                "`ledger accounts` does not name {logged} in its auth row, its rotation and the \
                 rotation to it: {}",
                text.out
            ));
        }
    }
    let to = logged(bea).expect("Bea's account");
    let displaced = format!(
        " displaced {} -> {to} \u{2014} default login now {to}",
        logged(ada).expect("Ada's account")
    );
    if !text.out.lines().any(|line| line.ends_with(&displaced)) {
        faults.push(format!(
            "`ledger accounts` does not name the account a displacement's reason names \
             (`{displaced}`): {}",
            text.out
        ));
    }
    let mut held_back = Vec::new();
    let text_out: String = text
        .out
        .lines()
        .filter(|line| {
            let unreadable = *line == unread;
            if unreadable {
                held_back.push((*line).to_owned());
            }
            !unreadable
        })
        .map(|line| format!("{line}\n"))
        .collect();
    if held_back.len() != 1 {
        faults.push(format!(
            "`ledger accounts` does not print the row it cannot read as it stands, once \
             (`{unread}`): {}",
            text.out
        ));
    }
    let rows = json.out.lines().count();
    if rows != 2 * roster.len() + 2 {
        faults.push(format!(
            "`ledger accounts --json` printed {rows} rows, not one per ledger row: {}",
            json.out
        ));
    }
    held_back.extend(json.out.lines().map(str::to_owned));
    let every = bench.run(&["ledger", "accounts", "1024"]);
    printed_by(
        vec![
            (text.ok, text_out, text.err),
            (json.ok, String::new(), json.err),
            (every.ok, every.out, every.err),
        ],
        held_back,
        faults,
    )
}

/// `accounts sync alab`, in its JSON and its text, on a bench of its own under the fixture's lab
/// naming this Mac (m9), which prefers Ada's and Bea's Claude accounts here, and Eve's, which the
/// lab labels with the label Ada's row has here (`m2-2`), so the sync leaves Eve's account out for
/// that clash; Cy's and Dee's rows, preferred here before the sync, move to the pool's order. Its
/// JSON (a schema 2 `accounts-sync` document) keys each roster row by its account's key, slug and
/// name with no label, the clash by Eve's key with Ada's row as `held_by`, and each order it
/// changed by its row's key; its text names each row, the clash and each order it changed by
/// their names. Then [`sync_codex`], and [`sync_codex_left`], whose refusal is held back.
fn sync(_: &Bench) -> Took {
    let base = fixture();
    let me = alab_roster::test_machine("m9");
    let mut lab = base.lab.clone();
    lab.me = Some(me.clone());
    let eve = key("claude", EVE);
    let mut clashing = pooled(&eve, "claude", EVE, None);
    clashing.label = "m2-2".to_owned();
    lab.accounts.push(clashing);
    lab.names.extend(slugged(&[("claude", EVE)]));
    for account in &mut lab.accounts {
        if [ADA, BEA, EVE].contains(&account.email.as_str()) {
            account.preferred = vec![(me.clone(), Some(1))];
        }
    }
    let mut roster = base.roster.clone();
    for row in &mut roster {
        if row
            .email
            .as_deref()
            .is_some_and(|email| [CY, DEE].contains(&email))
        {
            row.class = accounts::Class::Preferred;
        }
    }
    // Each form syncs a bench of its own, since the first sync's changes leave the second none.
    let bench = || {
        Bench::new(&Fixture {
            roster: roster.clone(),
            lab: lab.clone(),
        })
    };
    let own = bench();
    let json = own.run_on(&["accounts", "sync", "alab", "--json"], &NoSignIn);
    let text = bench().run_on(&["accounts", "sync", "alab"], &NoSignIn);
    let doc = aterm_json::from_str::<Value>(json.out.trim()).unwrap_or(Value::Null);
    let mut faults = Vec::new();
    if (&doc["kind"], &doc["schema"]) != (&Value::from("accounts-sync"), &Value::from(2u64)) {
        faults.push(format!(
            "`accounts sync alab --json` is not a schema 2 `accounts-sync` document: {}",
            json.out
        ));
    }
    let listed: Vec<&Value> = doc["machine_accounts"]
        .as_array()
        .into_iter()
        .flatten()
        .collect();
    let rows = own.roster().rows;
    let shown = |label: &str| {
        let row = rows.iter().find(|row| row.label == label).expect("a row");
        super::shown_row(Cli::Claude, row, Some(&lab))
    };
    for row in &rows {
        let Some((pk, names)) = names_of_row(Cli::Claude, row, Some(&lab)) else {
            continue;
        };
        if !listed.iter().any(|listed| {
            listed["ac"] == Value::from(pk.as_str())
                && listed["slug"] == Value::from(names.slug.clone())
                && listed["name"] == Value::from(names.name.clone())
                && listed.get("label").is_none()
        }) {
            faults.push(format!(
                "`accounts sync alab --json` lists no row of {pk} by its names: {}",
                json.out
            ));
        }
        let line = format!("  {} ", shown(&row.label));
        if !text.out.lines().any(|said| said.starts_with(&line)) {
            faults.push(format!(
                "`accounts sync alab` has no row line of {}: {}",
                shown(&row.label),
                text.out
            ));
        }
    }
    let ada = key("claude", ADA);
    let clashes = doc["label_clashes"].as_array().cloned().unwrap_or_default();
    if !matches!(clashes.as_slice(), [one] if one["ac"] == Value::from(eve.as_str())
        && one["slug"] == Value::from(lab.names[&eve].slug.clone())
        && one.get("label").is_none()
        && one["held_by"]["ac"] == Value::from(ada.as_str())
        && one["held_by"].get("label").is_none())
    {
        faults.push(format!(
            "`accounts sync alab --json` does not name the clash by Eve's account and Ada's row: \
             {}",
            json.out
        ));
    }
    let eve_shown = Named {
        pk: eve.clone(),
        row: None,
        names: lab.names[&eve].clone(),
    }
    .shown();
    let left_out = format!(
        "left out: {eve_shown}, which the lab prefers here, since this Mac's row of {} holds",
        shown("m2-2")
    );
    if !text.out.lines().any(|said| said.starts_with(&left_out)) {
        faults.push(format!(
            "`accounts sync alab` does not say `{left_out}`: {}",
            text.out
        ));
    }
    let moved: BTreeSet<String> = doc["orders_changed"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|order| order.get("label").is_none())
        .filter_map(|order| order["ac"].as_str().map(str::to_owned))
        .collect();
    let pool: BTreeSet<String> = [CY, DEE].map(|email| key("claude", email)).into();
    if moved != pool {
        faults.push(format!(
            "`accounts sync alab --json` names the orders it changed as {moved:?}, not Cy's and \
             Dee's keys {pool:?}: {}",
            json.out
        ));
    }
    for row in rows.iter().filter(|row| {
        row.email
            .as_deref()
            .is_some_and(|email| [CY, DEE].contains(&email))
    }) {
        let order = format!(
            "order changed: {} from preferred to pool",
            shown(&row.label)
        );
        if !text.out.lines().any(|said| said == order) {
            faults.push(format!(
                "`accounts sync alab` does not say `{order}`: {}",
                text.out
            ));
        }
    }
    let mut ran = vec![(json.ok, json.out, json.err), (text.ok, text.out, text.err)];
    ran.extend(sync_codex(&lab, &mut faults));
    let mut held_back = Vec::new();
    ran.push(sync_codex_left(&lab, &mut held_back, &mut faults));
    printed_by(ran, held_back, faults)
}

/// [`sync`]'s `accounts sync --cli codex alab`, in its JSON and its text, on a bench of its own
/// under `lab` (which names this Mac and prefers Ada's OpenAI account here, with no sign-in flow),
/// whose Codex roster lists the default login's row (`main`, no address) and Ada's OpenAI account
/// under the label `m2-2`, with a login of its own and the device sign-in flow, while Codex's
/// default login names Ada. The sync leaves `m2-2` as the roster lists it and names it by its
/// account (`kept`), and the default login's row and Ada's are one account, both kept, each named
/// by its names (the default login's row, which names no address, by its label marked so). Each
/// run, and what is wrong with what it printed into `faults`.
fn sync_codex(lab: &Lab, faults: &mut Vec<String>) -> Vec<(bool, String, String)> {
    let own = Bench::new(&Fixture {
        roster: fixture().roster,
        lab: lab.clone(),
    });
    crate::test_config::write(own.env.config.as_deref().expect("a config"), "")
        .expect("aterm.toml");
    let store = own
        .env
        .accounts_store_for(Cli::Codex)
        .expect("Codex's store");
    let dir = store.logins_dir().join("m2-2");
    std::fs::create_dir_all(&dir).expect("a login folder");
    std::fs::write(dir.join("auth.json"), "{}").expect("a login");
    let rows = [
        Row {
            label: "main".to_owned(),
            ..Row::default()
        },
        Row {
            dir: dir.to_str().map(str::to_owned),
            vendor: Vendor {
                flow: Some(crate::ai::connector::Flow::Device),
                ..Vendor::default()
            },
            ..row("m2-2", ADA)
        },
    ];
    let roster = own
        .env
        .accounts_roster_for(Cli::Codex)
        .expect("a roster path");
    std::fs::write(
        &roster,
        accounts::append_rows_for(Cli::Codex, "", &rows).expect("the rows admit"),
    )
    .expect("Codex's roster");
    let home = own.env.home.clone().expect("a home").join(".codex");
    std::fs::create_dir_all(&home).expect("Codex's home");
    let token = crate::harness::codex_app_login::tests::make_chatgpt_jwt(
        Some(ADA),
        "plus",
        own.env.now + 3600,
    );
    std::fs::write(
        home.join("auth.json"),
        aterm_json::json!({"tokens": {"access_token": token}}).to_string(),
    )
    .expect("the default login");
    let run = |json: bool| {
        let fetch =
            |_: &Env, _: &str| -> Result<String, String> { panic!("the lab is the source") };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = alab_roster::with_lab(lab.clone(), || {
            crate::harness::accounts_sync::run_cli_accounts_sync_with(
                Cli::Codex,
                (&own.env, None),
                (Some(alab_roster::SOURCE), None, json),
                (&mut out, &mut err),
                &fetch,
            )
        });
        (
            code == ExitCode::SUCCESS,
            String::from_utf8_lossy(&out).into_owned(),
            String::from_utf8_lossy(&err).into_owned(),
        )
    };
    let json = run(true);
    let text = run(false);
    let doc = aterm_json::from_str::<Value>(json.1.trim()).unwrap_or(Value::Null);
    let ada = names_of_login(Cli::Codex, None, ADA, Some(lab)).expect("Ada's OpenAI account");
    let ada_shown = Named {
        pk: ada.0.clone(),
        row: None,
        names: ada.1.clone(),
    }
    .shown();
    let kept = doc["kept"].as_array().cloned().unwrap_or_default();
    if !matches!(kept.as_slice(), [one] if one["ac"] == Value::from(ada.0.as_str())
        && one["slug"] == Value::from(ada.1.slug.clone())
        && one.get("label").is_none())
    {
        faults.push(format!(
            "`accounts sync --cli codex alab --json` does not key the row it kept as {}: {}",
            ada.0, json.1
        ));
    }
    let twin = &doc["twin"];
    if twin["row"]["ac"] != Value::Null
        || twin["row"]["label"] != Value::from("main")
        || twin["twin"]["ac"] != Value::from(ada.0.as_str())
        || twin["twin"].get("label").is_some()
    {
        faults.push(format!(
            "`accounts sync --cli codex alab --json` does not name the default login's twin by \
             its account: {}",
            json.1
        ));
    }
    let said = [
        format!(
            "left as the roster lists them (conflicting accounts or sign-in settings): {ada_shown}"
        ),
        format!(
            "one account: main (no address) on the default login and {ada_shown} are both \
             {ADA}; {ada_shown} holds a login or a record of its own, so both stay"
        ),
    ];
    for line in said {
        if !text.1.lines().any(|out| out.starts_with(&line)) {
            faults.push(format!(
                "`accounts sync --cli codex alab` does not say `{line}`: {}",
                text.1
            ));
        }
    }
    vec![json, text]
}

/// [`sync`]'s `accounts sync --cli codex`, no source named, on a bench of its own under `lab`
/// (which names this Mac and prefers Ada's OpenAI account here), with the lab labelling that
/// account `m2-2`: this Mac kept a Codex roster once and removed it, and Codex's store still holds
/// the records of `m2-2`, a row the sync would add, so the sync takes nothing
/// ([`crate::harness::accounts_sync::CliSync::Left`]) and its refusal names that row by Ada's
/// OpenAI account. The refusal says the label once more, as the row a restored roster must write
/// (`the row labelled `m2-2``): the records under it are re-keyed by SF-10, so that line is held
/// back ([`Took::held_back`]), never scanned as done. Its run (whether it refused, its stdout and
/// the rest of its stderr), and what is wrong with what it printed into `faults`.
fn sync_codex_left(
    lab: &Lab,
    held_back: &mut Vec<String>,
    faults: &mut Vec<String>,
) -> (bool, String, String) {
    let mut lab = lab.clone();
    let ada = key("codex", ADA);
    for account in &mut lab.accounts {
        if account.pk == ada {
            account.label = "m2-2".to_owned();
        }
    }
    let own = Bench::new(&Fixture {
        roster: fixture().roster,
        lab: lab.clone(),
    });
    crate::test_config::write(own.env.config.as_deref().expect("a config"), "")
        .expect("aterm.toml");
    let store = own
        .env
        .accounts_store_for(Cli::Codex)
        .expect("Codex's store");
    let mut state = accounts::State::default();
    state
        .accounts
        .insert("m2-2".to_owned(), accounts::Account::default());
    store.save(&state).expect("Codex's records");
    let fetch = |_: &Env, _: &str| -> Result<String, String> { panic!("the lab is the source") };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = alab_roster::with_lab(lab.clone(), || {
        crate::harness::accounts_sync::run_cli_accounts_sync_with(
            Cli::Codex,
            (&own.env, None),
            (None, None, false),
            (&mut out, &mut err),
            &fetch,
        )
    });
    let (out, err) = (
        String::from_utf8_lossy(&out).into_owned(),
        String::from_utf8_lossy(&err).into_owned(),
    );
    let names = names_of_login(Cli::Codex, None, ADA, Some(&lab)).expect("Ada's OpenAI account");
    let ada_shown = Named {
        pk: names.0,
        row: None,
        names: names.1,
    }
    .shown();
    let named = format!(
        "its store still holds the records of {ada_shown}, a row the sync would add, which no new \
         row may inherit; to rejoin, restore accounts-codex.toml with the rows it held, the row \
         labelled `m2-2` among them"
    );
    let mut rest = Vec::new();
    for line in err.lines() {
        if line.contains("the row labelled `m2-2`") {
            if !line.ends_with(&named) || tokens(line).filter(|token| *token == "m2-2").count() != 1
            {
                faults.push(format!(
                    "`accounts sync --cli codex` of a Mac that left the pool does not say \
                     `{named}`, with the label once: {line}"
                ));
            }
            held_back.push(line.to_owned());
        } else {
            rest.push(line);
        }
    }
    if rest.len() == err.lines().count() {
        faults.push(format!(
            "`accounts sync --cli codex` of a Mac that left the pool does not refuse as one: {err}"
        ));
    }
    (code == ExitCode::FAILURE, out, rest.join("\n"))
}

/// The lab journal's keyer as the app installs it (`crates/aterm/src/alab_journal.rs`): aterm-db's
/// account key, so a token's vault entry is the key the resolver names.
fn lab_keyer(realm: &str, email: &str) -> Option<String> {
    keys::account_pk(realm, email).ok()
}

/// The host the token verb and its report run on over the bench: a stand-in vault that holds no
/// token and records the key of each account it is asked of, a session here on each roster row's
/// account, and this aterm's survey naming a session on each row by its label, as the survey does.
#[cfg(unix)]
fn token_host(bench: &Bench) -> (crate::harness::accounts_tokens::tests::Host, FakeVaultOf) {
    use crate::harness::accounts_tokens::SessionLogin;
    use crate::harness::accounts_tokens::tests::{Answer, FakeVault, Host, VaultOf};
    let vault = FakeVault::new(Answer::Absent);
    let mut host = Host::new(VaultOf::Fake(vault.clone()));
    let roster = bench.roster();
    host.logins = roster
        .rows
        .iter()
        .filter_map(|row| row.email.clone())
        .map(|email| SessionLogin {
            email,
            account_id: None,
            default: false,
        })
        .collect();
    host.survey = Some(
        roster
            .rows
            .iter()
            .enumerate()
            .map(|(at, row)| {
                format!(
                    "session local={at} sid=t{at} account={} auth=stored token=-",
                    row.label
                )
            })
            .collect(),
    );
    (host, vault)
}

#[cfg(unix)]
type FakeVaultOf = crate::harness::accounts_tokens::tests::FakeVault;

/// The report's vault tokens section over the bench ([`token_host`]), under the fixture's lab and
/// the lab journal's keyer.
#[cfg(unix)]
fn token_section(bench: &Bench) -> crate::harness::accounts_token_view::Report {
    let (host, _) = token_host(bench);
    alab_roster::with_lab(bench.lab.clone(), || {
        crate::harness::journal::with_keyer(lab_keyer, || cli::token_report(&bench.env, &host))
    })
}

/// What is wrong with the vault tokens section `section` over the bench ([`token_section`]):
/// each roster row's account must be listed by its name and slug and offered its step by the word
/// the verb takes for it among the roster ([`Named::word_among`]), its JSON carrying each slug, and
/// no step may name a word the verb refuses here.
#[cfg(unix)]
fn token_section_faults(
    bench: &Bench,
    section: &crate::harness::accounts_token_view::Report,
) -> Vec<String> {
    let roster = bench.roster();
    let candidates = Candidates {
        roster: &roster.rows,
        logins: &[],
        lab: Some(&bench.lab),
        held: &[],
    };
    let (text, json) = (section.text(), section.json());
    let mut faults = Vec::new();
    for row in &roster.rows {
        let Some((pk, names)) = names_of_row(Cli::Claude, row, Some(&bench.lab)) else {
            continue;
        };
        let named = Named {
            pk,
            row: None,
            names,
        };
        let word = named.word_among(Cli::Claude, &candidates);
        if !text.contains(&format!("  {}: ", named.shown())) {
            faults.push(format!(
                "the vault tokens section lists no {}: {text}",
                named.shown()
            ));
        }
        let offered = format!("offer: run `aterm harness accounts token {word}`");
        if !text.contains(&offered) {
            faults.push(format!(
                "the vault tokens section offers {} no `{offered}`: {text}",
                named.shown()
            ));
        }
        let slugs: Vec<&str> = json["accounts"]
            .as_array()
            .map(|accounts| {
                accounts
                    .iter()
                    .filter_map(|account| account["slug"].as_str())
                    .collect()
            })
            .unwrap_or_default();
        if !named.names.slug.is_empty() && !slugs.contains(&named.names.slug.as_str()) {
            faults.push(format!(
                "the vault tokens JSON lists no slug {}: {json}",
                named.names.slug
            ));
        }
    }
    for line in text.lines() {
        let Some(step) = line.trim().strip_prefix("offer: run `") else {
            continue;
        };
        let word = step
            .trim_end_matches('`')
            .trim_start_matches("aterm harness accounts token ")
            .trim_end_matches(" --replace");
        if let Err(why) = resolve(Cli::Claude, word, &candidates) {
            faults.push(format!(
                "the section offers a step the verb refuses: {line}: {why}"
            ));
        }
    }
    faults
}

/// `accounts token` naming the account by `words`, as `aterm harness` routes it, with
/// `--remove` against a stand-in vault (it removes; nothing is made or spawned), under the lab
/// journal's keyer: the key of the account whose vault entry it asked for, else its refusal; and,
/// at every run that reached its account, the report's vault tokens section (text and JSON) over
/// the bench ([`token_section`]), each row's account listed and offered its step
/// ([`token_section_faults`]), a session here on each row named by the account that row holds.
#[cfg(unix)]
fn token(bench: &Bench, words: &[&str]) -> Took {
    let (host, vault) = token_host(bench);
    let named = words.iter().map(|word| (*word).to_owned());
    let args: Vec<String> = ["accounts", "token"]
        .map(str::to_owned)
        .into_iter()
        .chain(named)
        .chain(["--remove".to_owned()])
        .collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let ok = match cli::parse(&args) {
        Ok((Cmd::Accounts(form @ AccountsCmd::Token { .. }), _)) => {
            let code = alab_roster::with_lab(bench.lab.clone(), || {
                crate::harness::journal::with_keyer(lab_keyer, || {
                    cli::run_accounts(&form, &bench.env, &host, &mut out, &mut err)
                })
            });
            code == ExitCode::SUCCESS
        }
        Ok(other) => panic!("{args:?} parses as {other:?}"),
        Err(why) => {
            err.extend_from_slice(why.as_bytes());
            false
        }
    };
    let (out, err) = (
        String::from_utf8_lossy(&out).into_owned(),
        String::from_utf8_lossy(&err).into_owned(),
    );
    let asked = vault.asked.borrow().last().cloned();
    let reached = match asked {
        Some(key) if ok => Ok(key),
        _ => Err(refusal("token", &err)),
    };
    let mut printed = vec![out, err];
    let mut faults = Vec::new();
    if ok {
        let section = token_section(bench);
        faults = token_section_faults(bench, &section);
        printed.extend([section.text(), section.json().to_string()]);
    }
    Took {
        reached,
        exited: vec![ok],
        listed: Vec::new(),
        printed,
        held_back: Vec::new(),
        faults,
    }
}

/// `accounts token` before its case runs it: Pending where the verb is not built.
#[cfg(not(unix))]
fn token_case() -> Case {
    Case::Pending
}

/// `accounts token`'s case ([`token`]).
#[cfg(unix)]
fn token_case() -> Case {
    Case::TakesOne(token)
}

/// `accounts token-run <words> -- <the store build>` as the front door parses it, over the bench's
/// roster and the fixture's lab, under the lab journal's keyer: the key its word reached
/// ([`accounts_token_run::account_key_of`]), else its refusal; and the wrapper's own judgment
/// ([`accounts_token_run::judged`]) of a launch with that word on each roster row's stored login,
/// every row held by another Mac so that a launch held to its row starts nothing and says why. A
/// word that reached an account must have the launch on that account's row judged on it, and the
/// launch on every other row `outside` (a fault otherwise); a word that names no account (a roster
/// label an older aterm typed) must put the launch on the row labelled with it `outside`, and
/// still start nothing there. What the wrapper says when it starts nothing names the row by its
/// label (`<label> is held by m7`) until HX-33 deletes the wrapper's judgment, so those lines are
/// held back ([`Took::held_back`]), never scanned as done.
fn token_run(bench: &Bench, words: &[&str]) -> Took {
    use crate::harness::token_route::{Decision, Fallback};
    let build = bench.build.to_string_lossy().into_owned();
    let args: Vec<OsString> = words
        .iter()
        .copied()
        .chain(["--", build.as_str()])
        .map(OsString::from)
        .collect();
    let run = match accounts_token_run::parse(&args) {
        Ok(run) => run,
        Err(why) => {
            return Took {
                reached: Err(why.clone()),
                exited: vec![false],
                listed: Vec::new(),
                printed: vec![why],
                held_back: Vec::new(),
                faults: Vec::new(),
            };
        }
    };
    let mut roster = bench.roster();
    for row in &mut roster.rows {
        row.held_by = Some("m7".to_owned());
    }
    let word = run.named.clone().unwrap_or_default();
    let under = |f: &dyn Fn() -> accounts_token_run::Judged| {
        alab_roster::with_lab(bench.lab.clone(), || {
            crate::harness::journal::with_keyer(lab_keyer, f)
        })
    };
    let judged =
        |row: &Row| under(&|| accounts_token_run::judged(&run, &bench.launch_on(row, &roster)));
    let outside = |judged: &accounts_token_run::Judged| {
        matches!(judged.decision, Decision::AsTyped(Fallback::Outside(_)))
    };
    let reached = alab_roster::with_lab(bench.lab.clone(), || {
        crate::harness::journal::with_keyer(lab_keyer, || {
            accounts_token_run::account_key_of(&word, Some(&roster), &alab_roster::current, None)
        })
    })
    .map(|keyed| keyed.key);
    let (mut printed, mut held_back, mut faults) = (Vec::new(), Vec::new(), Vec::new());
    match &reached {
        Ok(key) => {
            let mut held_to: Vec<String> = Vec::new();
            for row in &roster.rows {
                let judged = judged(row);
                let pk = names_of_row(Cli::Claude, row, None).map(|(pk, _)| pk);
                if !outside(&judged) {
                    held_to.extend(pk.clone());
                }
                held_back.extend(judged.refusal);
            }
            if held_to != [key.clone()] {
                faults.push(format!(
                    "`token-run {word}` held the launches on the rows of {held_to:?} to its \
                     account, not only {key}'s"
                ));
            }
        }
        Err(why) => {
            printed.push(why.clone());
            if let Some(row) = roster.rows.iter().find(|row| row.label == word) {
                let judged = judged(row);
                if !outside(&judged) {
                    faults.push(format!(
                        "`token-run {word}` on its row is judged {:?}",
                        judged.decision
                    ));
                }
                match judged.refusal {
                    Some(why) => held_back.push(why),
                    None => faults.push(format!(
                        "`token-run {word}` started on a row another Mac holds"
                    )),
                }
            }
        }
    }
    Took {
        reached,
        exited: vec![true],
        listed: Vec::new(),
        printed,
        held_back,
        faults,
    }
}

/// Each verb's arm, written once: its pattern (one variant to an arm), its case and one argv of
/// it as a person types it, naming an account by its slug where it takes one. The macro makes
/// [`case`], an exhaustive match with no wildcard, so a new verb has no case until it has an
/// arm, and [`arms`], so an arm cannot be written without the argv the exit test parses back to
/// that arm's verb.
macro_rules! verbs {
    ($($verb:pat => $case:expr, $argv:expr;)*) => {
        fn case(verb: &Verb) -> Case {
            match verb {
                $($verb => $case,)*
            }
        }

        fn arms() -> Vec<(Vec<&'static str>, fn(&Verb) -> bool)> {
            vec![$((
                $argv.to_vec(),
                (|verb: &Verb| matches!(verb, $verb)) as fn(&Verb) -> bool,
            ),)*]
        }
    };
}

verbs! {
    Verb::Form(AccountsCmd::Table { .. }) => Case::Prints(table), ["accounts", "table"];
    Verb::Form(AccountsCmd::Import { .. }) => Case::TakesOne(import),
        ["accounts", "import", "claude-ada-x"];
    Verb::Form(AccountsCmd::Report { .. }) => report_case(), ["accounts"];
    Verb::Form(AccountsCmd::Add { .. }) => Case::TakesOne(add), ["accounts", "add", "claude-ada-x"];
    Verb::Form(AccountsCmd::AdoptDefault { .. }) => Case::TakesOne(adopt_default),
        ["accounts", "adopt-default", "claude-ada-x"];
    Verb::Form(AccountsCmd::Renew { .. }) => Case::TakesOne(renew), ["accounts", "renew", "claude-ada-x"];
    Verb::Form(AccountsCmd::Login { .. }) => Case::TakesOne(login), ["accounts", "login", "claude-ada-x"];
    Verb::Form(AccountsCmd::Browser { .. }) => Case::TakesOne(browser),
        ["accounts", "browser", "claude-ada-x"];
    Verb::Form(AccountsCmd::Use { .. }) => Case::TakesOne(use_pin),
        ["accounts", "use", "claude-ada-x"];
    Verb::Form(AccountsCmd::Auto) => Case::Prints(use_auto), ["accounts", "use", "--auto"];
    Verb::Form(AccountsCmd::History { .. }) => Case::Takes(history),
        ["accounts", "history", "claude-ada-x"];
    Verb::Form(AccountsCmd::Observed { .. }) => Case::Prints(observed), ["accounts", "observed"];
    Verb::Form(AccountsCmd::Token { .. }) => token_case(), ["accounts", "token", "claude-ada-x"];
    Verb::Form(AccountsCmd::Stats { .. }) => Case::Prints(stats), ["accounts", "stats"];
    Verb::Form(AccountsCmd::Refresh { .. }) => Case::Prints(refresh),
        ["accounts", "refresh", "--cli", "kimi"];
    Verb::Form(AccountsCmd::Sync { .. }) => Case::Prints(sync), ["accounts", "sync"];
    Verb::Form(AccountsCmd::Serve { .. }) => Case::Pending, ["accounts", "serve"];
    Verb::TokenRun => Case::TakesOne(token_run),
        ["accounts", "token-run", "claude-ada-x", "--", "claude"];
    Verb::AllPools => all_pools_case(), ["accounts", "--cli", "all"];
    Verb::Mailboxes => Case::Pending,
        ["accounts", "mailboxes", "import", "mail.csv", "--into", "ai-mailbox.toml"];
    Verb::Ledger => Case::Prints(ledger), ["ledger", "accounts"];
}

/// SF-07H's exit test. Every verb has an arm ([`verbs!`]), whose argv parses to that arm's verb
/// and no other's, and a case ([`case`]). The resolver every verb is to take its word through
/// reaches each roster row of the fixture by the account's slug, its key, its provider word and
/// address in either spelling, and its address alone (in Claude Code's pool, though `ada`'s
/// address is an OpenAI account's too). No label names anything: a roster row's (`dee`'s, its
/// account's own slug, apart), the lab's (each account's reserved slug) and the label a Mac
/// called its row before; each gets the resolver's no-account words. Each verb that takes its
/// word ([`Case::Takes`]) is run on the fixture on disk ([`Bench`]) with each of those words, the
/// provider word and address typed as two argv words: each reaches the row's account in every
/// form it is run in (each exits 0), and each label gets the no-account words; run with no word,
/// every form exits 0 and lists every roster row in order. A verb that names exactly one account
/// ([`Case::TakesOne`]: `token`, whose runs also print the report's vault tokens section with each
/// account's step, `token-run`, whose reached account a launch on that row's stored login is
/// held to, and `use`, run with no pin standing, whose reached account is the one its pin holds,
/// whose pinned line names it under the lab, and which refuses that account while its row is
/// signed out or has no login here, naming it first and ending in the row's sign-in step by the
/// account's word; `renew`, on a host with no terminal, whose reached account is the one its `due
/// here:` line names; `login`, whose reached account is the one its refusal of a row with an
/// address names, the renewal step it offers naming that account again; `browser`, on a host
/// with no terminal, whose reached account is the one the window step its no-terminal refusal
/// offers names, a refusal that prints no address, and which is then run on a bench of its own
/// at a desk, where a window that closes done names that account by its names as it opens and as
/// it closes and offers its renewal by the account's word, a window during which the roster
/// changes is not recorded, and a row in shared custody is refused by its names; `add` and
/// `adopt-default`, whose reached account is the one their refusal of an account already listed
/// names, the renewal step it offers naming that account again, which write nothing there (no
/// roster, record, ledger row, wake marker or login folder), and which are then run on a bench of
/// their own whose roster lacks that account's row, where each lists one row of it under its
/// reserved slug, its receipt names it by its names under the lab and the sign-in step it offers
/// names it again, `adopt-default` once more by the address alone (`--email`, its form that names
/// no account); and `import`, whose reached account is the one Claude Code's refusal (it has no source
/// rows) names) is never run with no word. A run of `renew`, `login`, `browser`, `add`,
/// `adopt-default` or `import` that ends as its case expects (exit 1, nothing started, opened or
/// written) counts as exited. A step `login` or `browser` offers that types an
/// address names nothing a step may ([`offers_again`]).
/// A verb that takes no word but prints an account's names ([`Case::Prints`]: `use --auto`, run
/// after each row's pin; the report, with sessions, a chosen row and a signed-out row, and with
/// the lab journal's section, then on a roster of no row; `accounts --cli all`; `stats`, on each
/// row's home, on the default home and over a Codex pool; `observed`, whose places are the rows'
/// login folders, held back; `refresh --cli kimi` over a quota tab's control socket, through a
/// read, Kimi's sign-in wall and the refusals after it; `table`, under a lab that names no machine
/// here and one that names this Mac; `ledger accounts`, whose verbatim JSON is held back; `sync
/// alab` under a lab that names this Mac, with a label clash and orders it changes, and `sync --cli
/// codex alab` with a row it keeps as the roster lists it and the default login's twin) exits 0
/// in each run (`refresh`'s refusals, which exit 1, ending as its case expects), prints each line
/// its case says it names accounts in, and what it prints is scanned as a taken verb's is.
/// Under a fixture whose row is labelled with another account's slug, that label is refused
/// naming no address and no other label, and the report's sign-in step for the account whose slug
/// it is (a renewal task on its row), in its text and its JSON, types a word that resolves to that
/// account: its key, since its slug names the other row ([`report_step_faults`]); so does the step
/// `add` offers once it lists that account on a roster without its row.
/// Every refusal repeats the word typed and is scanned for every other label; what the resolved
/// accounts print, and everything a verb that takes its word printed (with no word too), holds
/// no label ([`labels_printed`]), except a label that is its own account's slug. Every label
/// that names an account and every one printed is reported at once.
///
/// Not covered here, being parsed outside this crate: `accounts shared-renew <account>` (crate
/// aterm's `shared_login.rs`, whose parse takes one account word, its own test's, and whose run
/// resolves it as `login` does: `cli_shared_renewal`'s
/// `shared_renew_takes_each_account_word_and_refuses_the_rows_label` runs it with each word and
/// the row's label), and the native verbs (`native-run`,
/// `native-policy`, `native-status`, `native-key`, crate aterm's `shared_native.rs`), which name
/// an account by its native digest, not a word. Nothing checks that an arm's pattern holds one variant: an arm that held two
/// would cover the second with no argv.
///
/// NEGATIVE CONTROL: with `sign_in_step` and `sign_in_task_json` naming the row's word among that
/// row alone (`std::slice::from_ref(raw)`), it reports `` "the report's text step for `m3-1`
/// (Some(\"Run aterm harness accounts renew claude-dee-w in a terminal on this Mac\")) names
/// Some((\"claude-dee-w\", Err(…))), not ac-00862b922c5adb47" `` and the same of its JSON step
/// (2026-11-16, by hand, then restored).
/// NEGATIVE CONTROL: with `resolve` first taking the roster row whose label is the word (the
/// lookup the verbs make before SF-07H) and a lab account whose legacy label is the word
/// (`asked_row_from`'s `legacy_label` arm), and `Named::shown` printing the row's label beside
/// the names, the test reports each: `` "`claude-05b50c` names ac-05b50c…" `` (Bea's earlier
/// label), `` "`claude-fbb8979d074ab2bd` names ac-fbb8979d074ab2bd" `` (Cy's reserved slug), ``
/// "`m2-2` names ac-…" ``, `` "`m8-2` names ac-05b50c…" `` and `` "the label `m2-2` is printed:
/// claude ada@x.example (claude-ada-x) [m2-2]" ``, among others. With `cli_history`'s
/// `resolve` first taking the roster row labelled with the word and its header printing the
/// row's label beside the names, it reports `` "`accounts history m2-2` gave
/// Ok(\"ac-f44d6a51ca391c98\")" `` and `` "the label `m2-2` is printed: … claude ada@x.example
/// (claude-ada-x) [m2-2] · claude · this Mac …" ``; with `alab_seen::history` printing a ledger
/// row's labels as stored, `` "the label `claude-05b50c` is printed: … here rotate m2-2
/// to=claude-05b50c" ``. With history's every-row form refusing (`no account was named`), it
/// reports `` "`accounts history` exited [false, false] (JSON, text)" `` and `` "`accounts
/// history` listed [], not [\"ac-f44d6a51ca391c98\", …]" ``; with its named text form exiting
/// 1, `` "`accounts history claude-ada-x` exited [true, false] (JSON, text)" `` for every word;
/// with `label_clash` naming both accounts by `Named::shown`, `` "an address is printed:
/// aterm harness accounts history: `claude-dee-w` names claude dee@w.example (claude-dee-w), …"
/// ``. With `accounts_tokens::account_of` first taking the roster row labelled with the word and
/// `Account::of_named` showing the row's label beside the names, and `account_key_of` first
/// taking the row labelled with the word, it reports `` "`accounts token m2-2` gave
/// Ok(\"ac-f44d6a51ca391c98\")" ``, `` "`accounts token-run claude-05b50c` gave
/// Ok(\"ac-05b50c6fb48b61cd\")" ``, `` "`accounts token-run claude-dee-w` under a clash gave
/// Ok(\"ac-f44d6a51ca391c98\")" `` and `` "the label `m2-2` is printed: removed the Claude Code
/// token for claude ada@x.example (claude-ada-x) [m2-2] from the vault; …" ``, among others.
/// With the wrapper running a label's or another account's line as typed on a held row (no
/// `unstored` for a named `outside`), it reports `` "`token-run m2-2` started on a row another
/// Mac holds" `` for each label; with `Named::word_among` ignoring the clash, `` "the section
/// offers a step the verb refuses: … `aterm harness accounts token claude-dee-w`" ``; with
/// token-run's held-back reasons scanned as done, `` "the label `claude-05b50c` is printed:
/// claude-05b50c is held by m7 (absent)" ``. With `use_row_named` first taking the roster row
/// labelled with the word, and the pinned and unpinned lines printing the row's label beside
/// the names, it reports `` "`accounts use m2-2` gave Ok(\"ac-f44d6a51ca391c98\")" ``, ``
/// "`accounts use claude-dee-w` under a clash gave Ok(\"ac-f44d6a51ca391c98\")" ``, `` "the
/// label `m2-2` is printed: claude ada@x.example (claude-ada-x) [m2-2]: the active account,
/// pinned …" `` and `` "`accounts use --auto` after ac-f44d6a51ca391c98's pin does not name it
/// (claude ada@x.example (claude-ada-x)): … [m2-2]: no longer pinned …" ``, among others. With
/// the pinned line naming the account with no lab (`shown_row(cli, row, None)`), it reports ``
/// "`accounts use claude-ada-x` does not name ac-f44d6a51ca391c98 by its names (claude
/// ada@x.example (claude-ada-x)): claude ada@x.example: the active account, pinned …" ``; with
/// `run_accounts_use` exiting 0 at once for a word that starts `ac-`, `` "`accounts use
/// ac-f44d6a51ca391c98` reached Err(\"no row with an address is pinned: \"), not …" ``; with
/// use's sign-in remedy scanned as done, `` "the label `m2-2` is printed: … is signed-out; sign
/// it in with `aterm harness accounts renew m2-2`, in a terminal" `` (all 2026-11-16, by hand,
/// then restored). With `row_named` first taking the roster row labelled with the word, and
/// `RowName::of` showing the row's label beside the names, it reports `` "`accounts login m2-2`
/// gave Err(\"claude ada@x.example (claude-ada-x) [m2-2] has an email, so it signs in through
/// `aterm harness accounts renew claude-ada-x`: …\")" ``, `` "`accounts renew m2-2` gave
/// Err(\"`due here:` names no roster row's account: claude ada@x.example (claude-ada-x)
/// [m2-2]\")" ``, the same for `claude-05b50c`, `claude-fbb8979d074ab2bd` and `claude-dee-w`
/// under a clash, and `` "the label `m2-2` is printed: due here: claude ada@x.example
/// (claude-ada-x) [m2-2]" `` (2026-11-16, by hand on SF-07H S4's commit, then restored). With
/// `run_accounts_browser` first taking the roster row labelled with the word, it reports ``
/// "`accounts browser m2-2` gave Ok(\"ac-f44d6a51ca391c98\")" ``, the same for `claude-05b50c`
/// and `claude-fbb8979d074ab2bd`, and `` "`accounts browser claude-dee-w` under a clash gave
/// Ok(\"ac-f44d6a51ca391c98\")" `` (2026-11-16, by hand on SF-07H S5's commit, then restored).
/// With its no-terminal line typing the row's label in its step, it reports `` "`accounts browser
/// claude-ada-x` reached Err(\"its step `browser m2-2` names no account is named `m2-2`: …\"),
/// not ac-f44d6a51ca391c98" `` and `` "the label `m2-2` is printed: aterm harness accounts
/// browser: not run: stdin is not a terminal (the window waits … Run `aterm harness accounts
/// browser m2-2` …" ``; with that line naming the account by its names after its step, ``
/// "`accounts browser claude-ada-x` with no terminal prints an address: …" ``; with
/// `signin_policy_ready` naming the row by its label, `` "the label `m2-2` is printed: aterm
/// harness accounts browser: m2-2: this account requires a fresh root-authorized shared renewal;
/// nothing was run" ``; with `RenewalPolicy::require_current` naming it by its label, `` "the
/// label `m2-2` is printed: aterm harness accounts browser: result was not recorded: account
/// `m2-2` configuration or login changed during sign-in; nothing recorded" ``; with the closed
/// window's line naming it by its label, `` "`accounts browser claude-ada-x` at a desk does not
/// name ac-f44d6a51ca391c98 by its names (claude ada@x.example (claude-ada-x)) as its window
/// closes: …" ``; and with its `next:` step typing the row's address, `` "`accounts browser
/// claude-ada-x` offers `renew ada@x.example`, which names Err(\"an address, which a step never
/// types: `ada@x.example`\"), not ac-f44d6a51ca391c98" `` (2026-11-16, by hand on SF-07H S5's
/// review settle, then restored).
/// NEGATIVE CONTROL: with `add_listed`'s `run:` step typing the row's address, it reports ``
/// "`accounts add claude-ada-x` offers `renew ada@x.example`, which names Err(\"an address, which
/// a step never types: `ada@x.example`\"), not ac-f44d6a51ca391c98" `` for every word of every
/// account; with a refused `add` replacing the wake marker (same bytes), `` "`accounts add
/// claude-ada-x` wrote …/wake: " `` for every word; with `adopt-default`'s receipt naming the
/// login's own account by its row's label, `` "`accounts adopt-default --email ada@x.example` on a
/// roster without its row does not say `listed claude ada@x.example (claude-ada-x) (preferred):
/// this machine's own login`: listed claude-f44d6a51ca391c98 (preferred): …" `` (2026-11-16, by hand on SF-07H S6's review settle, then restored).
/// NEGATIVE CONTROL: with `listed_row` labelling the new row `m-` and six hex digits of its key
/// in place of its reserved slug, it reports `` "`accounts add ac-00862b922c5adb47` on a roster
/// without its row listed [\"m-00862b\"], not one row labelled claude-00862b922c5adb47: ok=true
/// listed claude dee@w.example (claude-dee-w) (preferred) in …" `` and the same of every account
/// word `add` and `adopt-default` take (2026-11-16, by hand on SF-07H S6, then restored).
/// NEGATIVE CONTROL: with `accounts_text`'s row line printing the row's label and
/// `put_account_names` keeping every row's `label` in the JSON, the report reports `` "the label
/// `m2-2` is printed: active: claude dee@w.example (claude-dee-w), 0 of 0 sessions on it\nm2-2
/// preferred  unseen …" `` and `` "the label `m2-2` is printed: {\"accounts\":[{\"ac\":
/// \"ac-f44d6a51ca391c98\", …" ``, and the same of `claude-05b50c` and `claude-fbb8979d074ab2bd`,
/// over the bench as it is and with sessions on two rows, 12 in all (2026-11-16, by hand on
/// SF-07H S7a, then restored). With `active_header_line` naming each row a session runs on by its
/// label, the `next:` line naming its row by its label, `alab_seen::rows_text` and `rows_json`
/// naming and keying the journal's rows by label, the report of a roster of no row naming its own
/// login with no account (no `Listing::with_login`) and the row line printing the row's label, it
/// reports `` "the report's `active:` line does not name claude ada@x.example (claude-ada-x),
/// claude bea@y.example (claude-bea-y) and the chosen claude cy@z.example (claude-cy-z): …" ``,
/// `` "the report's `next:` line names no account by its names: …" ``, `` "the report's journal
/// section does not name claude ada@x.example (claude-ada-x): …" `` and `` "the JSON's journal
/// does not name ac-f44d6a51ca391c98 by its key and name: …" `` for each row, `` "a roster of no
/// row reports its own login as {\"ac\":null, … \"label\":\"main\", …}, not Ada's account …" ``
/// and `` "`accounts --cli all` has no row line of claude ada@x.example (claude-ada-x): …" `` for
/// each row (2026-11-16, by hand on SF-07H S7a's review settle, then restored).
/// NEGATIVE CONTROL: with `account_stats::text_local` printing the home's label, the pool stats
/// line printing its row's label, `source_table_rows`' text naming each row by its label,
/// `accounts observed`'s BELIEVED column printing the label, `accounts_ledger_line` printing each
/// row's account as stored and Kimi's `(recorded)` line naming its row by its label, it reports
/// `` "`accounts stats` on ac-f44d6a51ca391c98's home does not name it (claude ada@x.example
/// (claude-ada-x)): … m2-2 · claude · plan — …" `` and the same of each row, `` "`accounts stats
/// --cli codex` does not name its row (oai ada@x.example (oai-ada-x)): …" ``, `` "`accounts
/// table` has no claude row of claude ada@x.example (claude-ada-x): …" `` for each row and Ada's
/// OpenAI one, `` "`accounts observed` believes no place is claude ada@x.example
/// (claude-ada-x): …" `` for each row, `` "`ledger accounts` does not name ac-f44d6a51ca391c98
/// (claude-ada-x) in its auth row, …" `` for each row, `` "`accounts refresh --cli kimi` at Kimi's
/// sign-in wall does not say `aterm harness accounts refresh: kimi ada@x.example is signed out
/// on this Mac (recorded)`: … m2-2 is signed out …" `` and `` "the label `m2-2` is printed: …" ``
/// of each, 37 in all (2026-11-16, by hand on SF-07H S7b, then restored).
/// NEGATIVE CONTROL (S7b's review settle): with `run_limits_read` printing Claude Code's accounts
/// line on every tab, it reports `` "`accounts refresh --cli kimi` under Kimi's quota wall does
/// not print its wall alone: … account=- next=claude bea@y.example (claude-bea-y)" `` in a run of
/// that alone; and with, together, the limits line printing the tab's label, a displacement's
/// reason printed as stored, `observed`'s `believed` the row's label, stats' JSON dropping every
/// slug and the table's `slug` the account's name, it reports that refresh fault, `` "`ledger
/// accounts` does not name the account a displacement's reason names (` displaced
/// ac-f44d6a51ca391c98 (claude-ada-x) -> ac-05b50c6fb48b61cd (claude-bea-y) — default login now
/// ac-05b50c6fb48b61cd (claude-bea-y)`): …" ``, `` "`accounts observed --json` believes no place
/// is ac-f44d6a51ca391c98: …" ``, `` "`accounts stats --json` on ac-f44d6a51ca391c98's home does
/// not key it by its names: …" ``, `` "`accounts table --json` lists no claude row as
/// ac-f44d6a51ca391c98: …" `` and each of those of every row, Ada's OpenAI one in the table
/// among them, and `` "the label `m2-2` is printed: {\"kind\":\"accounts-observed\", …" ``
/// (2026-11-16, mutation runs, then restored from HEAD).
/// NEGATIVE CONTROL (S8): with `Naming::shown` naming each row of a sync's receipt by its label
/// and `machine_accounts` keeping each row's `label`, it reports `` "`accounts sync alab` has no
/// row line of claude ada@x.example (claude-ada-x): …" `` and `` "`accounts sync alab --json`
/// lists no row of ac-f44d6a51ca391c98 by its names: …" `` for each row, `` "`accounts sync
/// alab` does not say `left out: claude eve@v.example (claude-eve-v), which the lab prefers here,
/// since this Mac's row of claude ada@x.example (claude-ada-x) holds`: …" ``, `` "`accounts sync
/// alab` does not say `order changed: claude cy@z.example (claude-cy-z) from preferred to pool`:
/// …" `` and Dee's, `` "`accounts sync --cli codex alab` does not say `one account: main (no
/// address) on the default login and oai ada@x.example (oai-ada-x) are both …`: …" `` and `` "the
/// label `m2-2` is printed: synced machine `m9` …" ``, with the same of `claude-05b50c` and
/// `claude-fbb8979d074ab2bd` in its text and JSON and of `m2-2` in the Codex receipt, 19 in all
/// (2026-11-16, a mutation run on SF-07H S8, then restored from HEAD).
/// NEGATIVE CONTROLS (S8's review settle), each a mutation run of its own, then restored from
/// HEAD (2026-11-17): with `cli::put_names` writing each row's `label` whatever its key, it
/// reports 31 faults, among them `` "`accounts sync --cli codex alab --json` does not key the row
/// it kept as ac-…: …" ``, `` "`accounts sync --cli codex alab --json` does not name the default
/// login's twin by its account: …" ``, each row, the clash and the orders of `accounts sync alab
/// --json`, and `m2-2`, `claude-05b50c` and `claude-fbb8979d074ab2bd` printed in both syncs' and
/// the report's, `observed`'s and `stats`' JSON; with `Naming::listed_shown` naming a source's row
/// by its label, it reports `` "`accounts sync --cli codex alab` does not say `left as the roster
/// lists them (conflicting accounts or sign-in settings): oai ada@x.example (oai-ada-x)`: …" ``,
/// `` "`accounts sync --cli codex` of a Mac that left the pool does not say `its store still holds
/// the records of oai ada@x.example (oai-ada-x), a row the sync would add, …`, with the label
/// once: …" ``, the `left out:` line of `accounts sync alab`, and `m2-2` printed in both
/// receipts, 5 in all.
#[test]
fn every_accounts_verb_takes_a_slug_a_key_or_a_provider_word_and_address_and_no_surface_prints_a_roster_label()
 {
    let arms = arms();
    // Each verb that takes its word, and whether it has a form that names no account.
    let mut takes: Vec<(String, fn(&Bench, &[&str]) -> Took, bool)> = Vec::new();
    // Each verb that takes no word but prints an account's names.
    let mut prints: Vec<(String, fn(&Bench) -> Took)> = Vec::new();
    for (at, (argv, _)) in arms.iter().enumerate() {
        let verb = verb_of(argv);
        let own: Vec<usize> = arms
            .iter()
            .enumerate()
            .filter(|(_, (_, is))| is(&verb))
            .map(|(at, _)| at)
            .collect();
        assert_eq!(own, [at], "{argv:?} parses as {verb:?}");
        match case(&verb) {
            Case::Pending => {}
            Case::Takes(run) => takes.push((argv[..2].join(" "), run, true)),
            Case::TakesOne(run) => takes.push((argv[..2].join(" "), run, false)),
            Case::Prints(run) => prints.push((
                argv.strip_prefix(&["accounts"][..])
                    .unwrap_or(argv)
                    .join(" "),
                run,
            )),
        }
    }

    let fixture = fixture();
    let candidates = fixture.candidates();
    let dee = key("claude", DEE);
    assert_eq!(
        fixture.lab.names[&dee].slug, fixture.roster[3].label,
        "dee's row is labelled with its account's own slug"
    );
    let mut printed: Vec<(String, Option<String>)> = Vec::new();
    for row in &fixture.roster {
        let email = row.email.as_deref().expect("an address");
        let pk = key("claude", email);
        let slug = fixture.lab.names[&pk].slug.clone();
        for word in [
            slug.clone(),
            pk.clone(),
            format!("claude {email}"),
            format!("claude:{email}"),
            email.to_owned(),
        ] {
            let named = resolve(Cli::Claude, &word, &candidates)
                .unwrap_or_else(|why| panic!("`{word}`: {why}"));
            assert_eq!(named.pk, pk, "`{word}`");
            assert_eq!(named.row, Some(row), "`{word}`");
            for text in [
                named.shown(),
                named.wide().to_owned(),
                named.narrow().to_owned(),
                named.logged(),
                named.word().to_string(),
            ] {
                printed.push((text, None));
            }
        }
    }
    let mut wrong = BTreeSet::new();
    for (label, slug) in fixture.labels() {
        if slug.as_deref() == Some(label.as_str()) {
            continue;
        }
        let fault = match resolve(Cli::Claude, &label, &candidates) {
            Ok(named) => format!("`{label}` names {}", named.pk),
            Err(why) if why == no_account(&label) => {
                printed.push((why, Some(label)));
                continue;
            }
            Err(why) => format!("`{label}` is refused as {why}"),
        };
        wrong.insert(fault);
    }
    let word = "oai-ada-x";
    let why = resolve(Cli::Claude, word, &candidates).expect_err("an OpenAI account's slug");
    printed.push((why, Some(word.to_owned())));

    let bench = Bench::new(&fixture);
    // Each line a verb held back, which must still print a roster label (else it belongs with
    // the lines scanned as done), and each fault a case found in its own run.
    let keep = |verb: &str, took: &mut Took, wrong: &mut BTreeSet<String>| {
        for text in took.held_back.drain(..) {
            let labelled = fixture
                .roster
                .iter()
                .any(|row| tokens(&text).any(|token| token == row.label));
            if !labelled {
                wrong.insert(format!(
                    "`{verb}` holds back a line that prints no roster label, so the scan should \
                     read it: {text}"
                ));
            }
        }
        wrong.extend(took.faults.drain(..));
    };
    for (verb, run, every) in &takes {
        for row in &fixture.roster {
            let email = row.email.as_deref().expect("an address");
            let pk = key("claude", email);
            let slug = fixture.lab.names[&pk].slug.as_str();
            let colon = format!("claude:{email}");
            let forms: [&[&str]; 5] = [
                &[slug],
                &[pk.as_str()],
                &["claude", email],
                &[colon.as_str()],
                &[email],
            ];
            for words in forms {
                let mut took = run(&bench, words);
                keep(verb, &mut took, &mut wrong);
                if took.reached.as_ref() != Ok(&pk) {
                    wrong.insert(format!(
                        "`{verb} {}` reached {:?}, not {pk}",
                        words.join(" "),
                        took.reached
                    ));
                }
                if took.exited.iter().any(|ok| !ok) {
                    wrong.insert(format!(
                        "`{verb} {}` exited {:?} (JSON, text)",
                        words.join(" "),
                        took.exited
                    ));
                }
                printed.extend(took.printed.into_iter().map(|text| (text, None)));
            }
        }
        for (label, slug) in fixture.labels() {
            if slug.as_deref() == Some(label.as_str()) {
                continue;
            }
            let mut took = run(&bench, &[&label]);
            keep(verb, &mut took, &mut wrong);
            if took.reached != Err(no_account(&label)) {
                wrong.insert(format!("`{verb} {label}` gave {:?}", took.reached));
            }
            printed.extend(
                took.printed
                    .into_iter()
                    .map(|text| (text, Some(label.clone()))),
            );
        }
        // With no word, every row's: each run exits 0 and lists every roster row, in order, so
        // the scan below reads every row's block and ledger lines, never a lone refusal.
        if !every {
            continue;
        }
        let mut every = run(&bench, &[]);
        keep(verb, &mut every, &mut wrong);
        if every.exited.iter().any(|ok| !ok) {
            wrong.insert(format!("`{verb}` exited {:?} (JSON, text)", every.exited));
        }
        let rows: Vec<String> = fixture
            .roster
            .iter()
            .map(|row| key("claude", row.email.as_deref().unwrap_or_default()))
            .collect();
        if every.listed != rows {
            wrong.insert(format!("`{verb}` listed {:?}, not {rows:?}", every.listed));
        }
        printed.extend(every.printed.into_iter().map(|text| (text, None)));
    }
    for (verb, run) in &prints {
        let mut took = run(&bench);
        keep(verb, &mut took, &mut wrong);
        if took.exited.is_empty() || took.exited.iter().any(|ok| !ok) {
            wrong.insert(format!("`{verb}` exited {:?}", took.exited));
        }
        printed.extend(took.printed.into_iter().map(|text| (text, None)));
    }
    // A label that is another account's slug: Ada's row here labelled with Dee's slug, the word
    // the Account menu once typed for it. Each verb that takes its word refuses it, naming both
    // accounts by key and slug, never by address, and repeats no label but the word typed.
    let mut clashing = Fixture {
        roster: fixture.roster.clone(),
        lab: fixture.lab.clone(),
    };
    clashing.roster[0].label = "claude-dee-w".to_owned();
    clashing.roster[3].label = "m3-1".to_owned();
    let bench = Bench::new(&clashing);
    for (verb, run, _) in &takes {
        let mut took = run(&bench, &["claude-dee-w"]);
        keep(verb, &mut took, &mut wrong);
        match &took.reached {
            Err(why) if why.contains("roster row labelled `claude-dee-w`") => {}
            other => {
                wrong.insert(format!(
                    "`{verb} claude-dee-w` under a clash gave {other:?}"
                ));
            }
        }
        for text in &took.printed {
            if text.contains('@') {
                wrong.insert(format!("an address is printed: {text}"));
            }
            wrong.extend(labels_printed(text, &clashing, Some("claude-dee-w")));
        }
    }
    // The steps the token surfaces offer under the clash: Dee's account is offered by its key,
    // never by its slug, which names Ada's row here, and every step offered is one the verb takes.
    #[cfg(unix)]
    {
        let section = token_section(&bench);
        wrong.extend(token_section_faults(&bench, &section));
        let text = section.text();
        if text.contains("accounts token claude-dee-w") {
            wrong.insert(format!(
                "under a clash the section offers the slug a roster row is labelled with: {text}"
            ));
        }
        if !text.contains(&format!("accounts token {dee}")) {
            wrong.insert(format!(
                "under a clash Dee is offered no step by its key: {text}"
            ));
        }
    }
    // The report's step for a row with a sign-in task under the clash, in its text and its JSON
    // (`sign_in_step`, `sign_in_task_json`): Dee's row, labelled `m3-1`, is named by the word the
    // verb takes for it among the whole roster, its key, since its slug names the row labelled
    // with it; the step resolves back to Dee's account.
    wrong.extend(report_step_faults(&bench, "m3-1", &dee));
    // `add` under the clash, on a roster without Dee's row: Dee is listed by its key, and the
    // step its receipt offers types Dee's key, since its slug names Ada's row here.
    let mut dropped = Fixture {
        roster: clashing.roster.clone(),
        lab: clashing.lab.clone(),
    };
    dropped.roster.retain(|row| row.label != "m3-1");
    let (texts, faults) = listed_on_a_bench_of_its_own(&Bench::new(&dropped), "add", &[&dee], &dee);
    wrong.extend(faults);
    for text in &texts {
        wrong.extend(labels_printed(text, &dropped, Some("claude-dee-w")));
    }
    for (text, typed) in &printed {
        wrong.extend(labels_printed(text, &fixture, typed.as_deref()));
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// What is wrong with the report's sign-in step for the bench's row `label`, the row of account
/// `pk`, once that row has a renewal task on record (admitted as a sign-in records one): its
/// text's one `sign-in needed:` line and its JSON's `sign_in_task.next_step` must each type
/// `accounts renew <word>` with a word that resolves among the bench's roster to `pk`.
fn report_step_faults(bench: &Bench, label: &str, pk: &str) -> Vec<String> {
    let roster = bench.roster();
    let dir = roster.row(label).and_then(|row| row.dir.clone());
    bench
        .env
        .accounts_store()
        .transact_pool_logins_within(
            &accounts::LOCK_LADDER,
            &bench.env.accounts_roster().expect("a roster path"),
            bench.env.now,
            &mut None,
            |_, _, logins| {
                logins
                    .labels
                    .entry(label.to_owned())
                    .or_insert_with(|| accounts::Binding {
                        dir,
                        ..accounts::Binding::default()
                    });
                accounts::admit_workflow(logins, label)?;
                accounts::note_sign_in_task(
                    logins,
                    label,
                    accounts::SignInAction::Renew,
                    bench.env.now,
                );
                Ok(Vec::new())
            },
        )
        .expect("the bench's logins");
    let candidates = Candidates {
        roster: &roster.rows,
        logins: &[],
        lab: Some(&bench.lab),
        held: &[],
    };
    let mut faults = Vec::new();
    let mut check = |surface: &str, step: Option<&str>| {
        let word = step
            .and_then(|step| step.split_once("aterm harness accounts renew "))
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .map(|word| word.trim_end_matches('`'));
        match word.map(|word| (word, resolve(Cli::Claude, word, &candidates))) {
            Some((_, Ok(named))) if named.pk == pk => {}
            other => faults.push(format!(
                "the report's {surface} step for `{label}` ({step:?}) names {other:?}, not {pk}"
            )),
        }
    };
    let text = bench.run(&["accounts"]);
    let needed: Vec<&str> = text
        .out
        .lines()
        .filter_map(|line| line.trim().strip_prefix("sign-in needed: "))
        .collect();
    let json = bench.run(&["accounts", "--json"]);
    let doc: Value = aterm_json::from_str(json.out.trim()).unwrap_or(Value::Null);
    let next = doc["accounts"].as_array().and_then(|rows| {
        rows.iter()
            .find(|row| row["ac"].as_str() == Some(pk))
            .and_then(|row| row["sign_in_task"]["next_step"].as_str())
    });
    check("JSON", next);
    match needed.as_slice() {
        [one] => check("text", Some(one)),
        _ => check("text", None),
    }
    if needed.len() > 1 {
        faults.push(format!(
            "the report names {} sign-in steps, not one: {}",
            needed.len(),
            text.out
        ));
    }
    faults
}

/// A row takes the lab's names for its account's key; with no lab, or an account the lab does
/// not hold, names made here from its realm and claims address, with no slug. A session login is
/// named the same way, Kimi Code's by its region. A row with no address has no key.
#[test]
fn a_row_and_a_login_take_the_labs_names_else_names_made_here_with_no_slug() {
    let fixture = fixture();
    let ada = &fixture.roster[0];
    let (pk, names) = names_of_row(Cli::Claude, ada, Some(&fixture.lab)).expect("a key");
    assert_eq!(pk, key("claude", ADA));
    assert_eq!(names, fixture.lab.names[&pk]);
    assert_eq!(names.slug, "claude-ada-x");
    let (here, made) = names_of_row(Cli::Claude, ada, None).expect("a key");
    assert_eq!(here, pk);
    assert_eq!(
        made,
        AccountNames {
            realm: "claude".to_owned(),
            slug: String::new(),
            name: Some(format!("claude {ADA}")),
            email: Some(ADA.to_owned()),
            login: Some(ADA.to_owned()),
            live: true,
        }
    );
    // An alias login keys the account by its identity, and keeps its login.
    let alias = Row {
        identity_email: Some(BEA.to_owned()),
        ..row("alias", "Ada.Login@X.example")
    };
    let (pk, made) = names_of_row(Cli::Codex, &alias, None).expect("a key");
    assert_eq!(pk, key("codex", BEA));
    assert_eq!(made.name.as_deref(), Some(&*format!("oai {BEA}")));
    assert_eq!(made.login.as_deref(), Some("ada.login@x.example"));
    // agy's realm is Google's.
    let (pk, made) = names_of_row(Cli::Agy, ada, Some(&fixture.lab)).expect("a key");
    assert_eq!((pk, made.realm.as_str()), (key("google", ADA), "google"));
    let kimi = names_of_login(Cli::Kimi, Some(Region::MainlandCn), CY, Some(&fixture.lab));
    let (pk, made) = kimi.expect("a key");
    assert_eq!(pk, key("kimi:mainland-cn", CY));
    assert_eq!(made.name.as_deref(), Some(&*format!("kimicn {CY}")));
    assert_eq!(made.slug, "");
    let nameless = Row {
        label: "bare".to_owned(),
        ..Row::default()
    };
    assert_eq!(
        names_of_row(Cli::Claude, &nameless, Some(&fixture.lab)),
        None
    );
    assert_eq!(names_of_login(Cli::Claude, None, "no-address", None), None);
}

/// A row whose account the lab holds without an address (its address row has not replicated
/// here yet, or is conflicted) keeps the name and address made here from the realm and claims
/// address its key was hashed from, beside the lab's slug: the address alone and the provider
/// word and address reach the row in Claude Code's pool, though `ada`'s address is an OpenAI
/// account's too, and a message shows the name.
/// NEGATIVE CONTROL: with `lab_or` taking the lab's copy whole, the address names no Claude
/// account and is refused as the OpenAI account's: `` `ada@x.example`: `ada@x.example` is an
/// OpenAI account: add `--cli codex` ``.
#[test]
fn a_labs_copy_with_no_address_keeps_the_name_and_address_a_row_holds() {
    let mut fixture = fixture();
    let pk = key("claude", ADA);
    if let Some(held) = fixture.lab.names.get_mut(&pk) {
        held.name = None;
        held.email = None;
        held.login = None;
    }
    let candidates = fixture.candidates();
    for word in [
        ADA.to_owned(),
        format!("claude {ADA}"),
        format!("claude:{ADA}"),
        "claude-ada-x".to_owned(),
    ] {
        let named = resolve(Cli::Claude, &word, &candidates)
            .unwrap_or_else(|why| panic!("`{word}`: {why}"));
        assert_eq!(named.pk, pk, "`{word}`");
        assert_eq!(named.row, Some(&fixture.roster[0]), "`{word}`");
        assert_eq!(named.shown(), format!("claude {ADA} (claude-ada-x)"));
        assert_eq!(named.wide(), format!("claude {ADA}"));
    }
}

/// A word names an account of the pool only: an OpenAI account's slug, `oai` and its address in
/// either spelling, its key, or an address only OpenAI holds says which `--cli` takes it, while
/// Codex's pool names it; a Meta account, which no pool holds, says so, and a word that names
/// accounts of several other pools names each. A session's login is a candidate with no row,
/// named here where the lab does not hold it; a word that names nothing in any pool says the
/// resolver's words.
#[test]
fn a_word_names_only_the_pools_accounts_and_another_pools_says_which_cli_takes_it() {
    let fixture = fixture();
    let mut lab = fixture.lab.clone();
    lab.names.extend(slugged(&[("codex", EVE), ("meta", ADA)]));
    let candidates = Candidates {
        lab: Some(&lab),
        ..fixture.candidates()
    };
    for word in [
        "oai-ada-x".to_owned(),
        format!("oai {ADA}"),
        format!("oai:{ADA}"),
        key("codex", ADA),
        EVE.to_owned(),
    ] {
        assert_eq!(
            resolve(Cli::Claude, &word, &candidates),
            Err(format!("`{word}` is an OpenAI account: add `--cli codex`"))
        );
    }
    assert_eq!(
        resolve(Cli::Claude, "meta-ada-x", &candidates),
        Err("`meta-ada-x` is a Meta account, which no pool here holds".to_owned())
    );
    let grok = Candidates {
        roster: &[],
        ..candidates
    };
    assert_eq!(
        resolve(Cli::Grok, ADA, &grok),
        Err(format!(
            "`{ADA}` names no Grok Build account; it names a Claude account (`--cli claude`) and \
             an OpenAI account (`--cli codex`) and a Meta account, which no pool here holds"
        ))
    );
    let codex = Candidates {
        roster: &[],
        ..candidates
    };
    let named = resolve(Cli::Codex, "oai-ada-x", &codex).expect("Codex's account");
    assert_eq!((named.pk, named.row), (key("codex", ADA), None));
    let named = resolve(Cli::Codex, ADA, &codex).expect("the one account of the pool");
    assert_eq!(named.names.slug, "oai-ada-x");

    let candidates = fixture.candidates();
    let logins = [Login {
        region: None,
        email: "fay@u.example",
    }];
    let signed_in = Candidates {
        logins: &logins,
        ..candidates
    };
    let named = resolve(Cli::Claude, "claude:fay@u.example", &signed_in).expect("a login");
    assert_eq!(
        named,
        Named {
            pk: key("claude", "fay@u.example"),
            row: None,
            names: AccountNames {
                realm: "claude".to_owned(),
                slug: String::new(),
                name: Some("claude fay@u.example".to_owned()),
                email: Some("fay@u.example".to_owned()),
                login: Some("fay@u.example".to_owned()),
                live: true,
            },
        }
    );
    assert_eq!(named.word().as_str(), key("claude", "fay@u.example"));
    assert_eq!(named.shown(), "claude fay@u.example");
    assert_eq!(
        resolve(Cli::Claude, "nobody", &candidates),
        Err(no_account("nobody"))
    );
    // With no lab, a slug names nothing and the key, the name and the address still do.
    let alone = Candidates {
        lab: None,
        ..candidates
    };
    assert!(resolve(Cli::Claude, "claude-ada-x", &alone).is_err());
    for word in [key("claude", ADA), format!("claude {ADA}"), ADA.to_owned()] {
        let named = resolve(Cli::Claude, &word, &alone).expect("named here");
        assert_eq!(named.row, Some(&fixture.roster[0]), "{word}");
    }
}

/// An account is its pool's by its canonical realm: a lab account whose realm an older build
/// spelled `openai` is Codex's, named there and pointed to `--cli codex` from Claude Code's
/// pool. One whose realm is unknown (a missing or conflicted cell) is no pool's: its slug names
/// nothing, and the refusal is the resolver's own, never one that names a provider.
/// NEGATIVE CONTROLS: with pool membership comparing the realm's text
/// (`pool.contains(&names.realm.as_str())`), the older spelling is no pool's, and Codex's pool
/// refuses its slug (the `Codex's account` expectation fails). With `elsewhere_in` taking the
/// accounts of an unknown realm too, the refusal points at no pool: `` left: Err("`claude-fay-u`
/// names no Claude Code account; it names ") ``.
#[test]
fn an_account_is_its_pools_by_its_canonical_realm_and_an_unknown_realm_is_no_pools() {
    let older = key("codex", EVE);
    let unknown = key("claude", "fay@u.example");
    let mut names = slugged(&[("codex", EVE), ("claude", "fay@u.example")]);
    names.get_mut(&older).expect("eve").realm = "openai".to_owned();
    names.get_mut(&unknown).expect("fay").realm = String::new();
    let lab = Lab {
        names,
        ..Lab::default()
    };
    let candidates = Candidates {
        lab: Some(&lab),
        ..Candidates::default()
    };
    let named = resolve(Cli::Codex, "oai-eve-v", &candidates).expect("Codex's account");
    assert_eq!(named.pk, older);
    assert_eq!(
        resolve(Cli::Claude, "oai-eve-v", &candidates),
        Err("`oai-eve-v` is an OpenAI account: add `--cli codex`".to_owned())
    );
    for cli in [Cli::Claude, Cli::Codex] {
        assert_eq!(
            resolve(cli, "claude-fay-u", &candidates),
            Err(no_account("claude-fay-u")),
            "{cli:?}"
        );
    }
}

/// One address Kimi Code holds in both regions is two accounts of its one pool, so the address
/// alone names both and is refused with each, and the provider word tells them apart. The lab's
/// accounts, with no row here, are the pool's by both realms and listed by their slug, name and
/// key; with no lab the rows here are listed by their name and key, since no lab slugged them,
/// with no empty slug column.
/// NEGATIVE CONTROL: with Kimi Code's pool holding `kimi:global` alone (`realms`), the address
/// names the global account: `` left: Ok(Named { pk: "ac-…", … }) right: Err("`cy@z.example`
/// names 2 accounts; …") ``.
#[test]
fn an_address_both_kimi_regions_hold_names_two_accounts_of_the_pool() {
    let global = key("kimi:global", CY);
    let mainland = key("kimi:mainland-cn", CY);
    let region = |region, label| Row {
        vendor: Vendor {
            region: Some(region),
            flow: None,
        },
        ..row(label, CY)
    };
    let roster = [
        region(Region::Global, "kimi-a"),
        region(Region::MainlandCn, "kimi-b"),
    ];
    let lab = Lab {
        names: slugged(&[("kimi:global", CY), ("kimi:mainland-cn", CY)]),
        ..Lab::default()
    };
    let mut pks = [global, mainland.clone()];
    pks.sort();
    // The lab's accounts alone: no row here, so only the pool's realms make them candidates.
    let with_lab = Candidates {
        lab: Some(&lab),
        ..Candidates::default()
    };
    let listed = many_accounts(
        CY,
        pks.iter().map(|pk| {
            let names = &lab.names[pk];
            (
                names.slug.as_str(),
                names.name.as_deref().unwrap_or_default(),
                pk.as_str(),
            )
        }),
    );
    assert_eq!(resolve(Cli::Kimi, CY, &with_lab), Err(listed));
    let alone = Candidates {
        roster: &roster,
        ..Candidates::default()
    };
    let names: Vec<String> = pks
        .iter()
        .map(|pk| {
            let word = if *pk == mainland { "kimicn" } else { "kimi" };
            format!("{word} {CY}")
        })
        .collect();
    let why = resolve(Cli::Kimi, CY, &alone).expect_err("two accounts");
    assert_eq!(
        why,
        many_accounts(
            CY,
            pks.iter()
                .zip(&names)
                .map(|(pk, name)| ("", name.as_str(), pk.as_str()))
        )
    );
    assert!(!why.contains("\n    "), "{why}");
    let named = resolve(Cli::Kimi, &format!("kimicn {CY}"), &alone).expect("the mainland account");
    assert_eq!((named.pk, named.row), (mainland, Some(&roster[1])));
}

/// A REPORT NEVER PRINTS A LABEL NO ROW HOLDS BARE: a record of a row since removed (two
/// sessions and the chosen row on `claude-dee-w`, the label a sync removed from Ada's row, while
/// Dee's account is slugged `claude-dee-w` and listed under another label) names a word that names
/// another account now, so the report's words mark it `(no longer listed)`, and its JSON gives the
/// label, which no row's `ac` or `label` matches. The empty listing (a caller that names no
/// account) still names each row by its label. CONTROL: the report's `active:` line over that
/// record says it so. NEGATIVE CONTROL: with `Listing::shown` answering a label no row holds
/// bare, `left: "claude-dee-w" right: "claude-dee-w (no longer listed)"` (2026-11-16, by hand on
/// SF-07H S7a's review settle, then restored).
#[cfg(unix)]
#[test]
fn a_report_marks_a_label_no_row_holds() {
    let mut moved = fixture();
    moved.roster[3].label = "m3-1".to_owned();
    let names = super::Listing::of(Cli::Claude, &moved.roster, Some(&moved.lab));
    assert_eq!(
        names.shown("claude-dee-w"),
        "claude-dee-w (no longer listed)"
    );
    assert_eq!(names.id("claude-dee-w"), "claude-dee-w");
    assert_eq!(
        names.shown("m3-1"),
        super::shown_row(Cli::Claude, &moved.roster[3], Some(&moved.lab))
    );
    assert_eq!(
        super::Listing::default().shown("claude-dee-w"),
        "claude-dee-w"
    );
    let bench = Bench::new(&moved);
    let now = bench.env.now;
    bench.edit(|state| {
        for sid in ["s-one", "s-two"] {
            state.sessions.insert(
                sid.to_owned(),
                accounts::SessionRow {
                    account: "claude-dee-w".to_owned(),
                    at: now - 60,
                    held: None,
                    owner: None,
                },
            );
        }
        state.active = Some("claude-dee-w".to_owned());
    });
    let (host, _) = token_host(&bench);
    let ran = report_runs(&bench, &host, &["accounts"], &mut Vec::new());
    let [(_, json, _), (ok, text, err)] = ran.as_slice() else {
        panic!("two forms: {}", ran.len());
    };
    assert!(ok, "{err}");
    let active = text
        .lines()
        .find(|line| line.starts_with("active: "))
        .unwrap_or_else(|| panic!("an `active:` line in {text}"));
    assert!(
        active.starts_with("active: claude-dee-w (no longer listed), 2 of 2 sessions on it"),
        "{active}"
    );
    let doc: Value = aterm_json::from_str(json.trim()).expect("the report's JSON");
    assert_eq!(doc["active"], Value::from("claude-dee-w"), "{doc}");
    let rows = doc["accounts"].as_array().expect("rows");
    assert!(
        rows.iter()
            .all(|row| row["ac"] != doc["active"] && row["label"] != doc["active"]),
        "no row is named by the removed label: {doc}"
    );
}

/// A word that names one account while a roster row here labelled with it holds another is
/// refused with both, by key and slug and never by address (the person typed none, and the
/// refusal reaches captured stderr), as is one whose labelled row holds no address: the owner
/// may have slugged Dee's account with the very word Ada's row here is labelled, and that word
/// must never quietly name Dee's account where a habit, a step a log kept or a script meant
/// Ada's. Every other word still names each account, a row labelled with its own account's slug
/// is named by it, and an account's key names it even where a row here is labelled with that
/// very text: a key is exact.
/// NEGATIVE CONTROL: with `resolve` answering `Ok` without `label_clash`, the label names Dee's
/// account: `` left: Ok(Named { pk: "ac-…", row: Some(Row { label: "m3-1", … }) … }) ``; with
/// `label_clash` naming the accounts by `Named::shown`, the refusal reads `` `claude-dee-w`
/// names claude dee@w.example (claude-dee-w), but … holds claude ada@x.example (claude-ada-x)
/// ``; with no exact-key arm, Dee's key is refused as the label of Ada's row.
#[test]
fn a_word_a_row_here_is_labelled_never_names_another_account() {
    let fixture = fixture();
    let (ada, dee) = (key("claude", ADA), key("claude", DEE));
    let mut roster = fixture.roster.clone();
    roster[0].label = "claude-dee-w".to_owned();
    roster[3].label = "m3-1".to_owned();
    let candidates = Candidates {
        roster: &roster,
        ..fixture.candidates()
    };
    let why = resolve(Cli::Claude, "claude-dee-w", &candidates).expect_err("a clash");
    assert_eq!(
        why,
        format!(
            "`claude-dee-w` names {dee} (claude-dee-w), but this Mac's roster row labelled \
             `claude-dee-w` holds {ada} (claude-ada-x): name the account by its key, or by its \
             provider word and address"
        )
    );
    assert!(!why.contains('@'), "{why}");
    let mut keyed = roster.clone();
    keyed[0].label.clone_from(&dee);
    let named = resolve(
        Cli::Claude,
        &dee,
        &Candidates {
            roster: &keyed,
            ..fixture.candidates()
        },
    );
    assert_eq!(named.map(|named| named.row), Ok(Some(&keyed[3])));
    for (word, row) in [
        (key("claude", DEE), &roster[3]),
        (format!("claude {DEE}"), &roster[3]),
        (ADA.to_owned(), &roster[0]),
        ("claude-ada-x".to_owned(), &roster[0]),
    ] {
        let named = resolve(Cli::Claude, &word, &candidates).map(|named| named.row);
        assert_eq!(named, Ok(Some(row)), "{word}");
    }
    let mut bare = fixture.roster.clone();
    bare[3].label = "m3-1".to_owned();
    bare.push(Row {
        label: "claude-dee-w".to_owned(),
        ..Row::default()
    });
    let candidates = Candidates {
        roster: &bare,
        ..fixture.candidates()
    };
    assert_eq!(
        resolve(Cli::Claude, "claude-dee-w", &candidates),
        Err(format!(
            "`claude-dee-w` names {dee} (claude-dee-w), but this Mac's roster row labelled \
             `claude-dee-w` holds no address: name the account by its key, or by its provider \
             word and address"
        ))
    );
    let named = resolve(Cli::Claude, "claude-dee-w", &fixture.candidates()).expect("dee");
    assert_eq!(named.row, Some(&fixture.roster[3]));
}

/// `accounts use` refuses an account of the pool that the lab names and no roster row here holds
/// (Fay's, in the lab alone), by its slug and by its key alike: the refusal names it by its key
/// and slug, the words that name it exactly, and prints no address and no label, and nothing is
/// pinned. NEGATIVE CONTROL: with `use_row_named`'s no-row refusal naming the account by
/// `Named::shown`, it fails on `` "`claude-fay-u` names claude fay@u.example (claude-fay-u),
/// which no row of this Mac's roster holds: …" `` (2026-11-16, by hand, then restored).
#[test]
fn use_refuses_an_account_no_roster_row_here_holds() {
    let fay = "fay@u.example";
    let mut fixture = fixture();
    let pk = key("claude", fay);
    fixture.lab.names = slugged(&[
        ("claude", ADA),
        ("codex", ADA),
        ("claude", BEA),
        ("claude", CY),
        ("claude", DEE),
        ("claude", fay),
    ]);
    fixture.lab.accounts.push(pooled(&pk, "claude", fay, None));
    let slug = fixture.lab.names[&pk].slug.clone();
    let bench = Bench::new(&fixture);
    for word in [slug.as_str(), pk.as_str()] {
        let ran = bench.run(&["accounts", "use", word]);
        assert!(!ran.ok, "`{word}`: {}", ran.out);
        let why = refusal("use", &ran.err);
        assert!(
            why.starts_with(&format!(
                "`{word}` names {pk} ({slug}), which no row of this Mac's roster holds"
            )),
            "{why}"
        );
        assert!(!ran.err.contains('@'), "an address is printed: {}", ran.err);
        assert_eq!(
            labels_printed(&ran.err, &fixture, Some(word)),
            Vec::<String>::new()
        );
        assert_eq!(bench.pinned(), None, "`{word}`");
    }
}

/// An account key this Mac's records hold ([`Candidates::held`]) names its account though no
/// row here holds it and no lab copy names it: by the key alone, with no row and no names, so
/// it is shown by its key. A held key the lab names takes the lab's names, one a row holds
/// takes the row, and held text that is no account key (a sub-account key, a label) is no
/// candidate: it names nothing, nor does anything but the key.
/// NEGATIVE CONTROL: with `resolve` ignoring `held`, the removed row's key is refused: ``no
/// account is named `ac-…` ``.
#[test]
fn a_key_this_macs_records_hold_names_its_account_by_key_alone() {
    let fixture = fixture();
    let gone = key("claude", "gone@q.example");
    let held = [
        gone.clone(),
        key("claude", BEA),
        key("claude", ADA),
        "sa-0123456789abcdef".to_owned(),
        "m2-2".to_owned(),
    ];
    let mut lab = fixture.lab.clone();
    lab.names.remove(&key("claude", DEE));
    let candidates = Candidates {
        roster: &fixture.roster[..1],
        lab: Some(&lab),
        held: &held,
        ..Candidates::default()
    };
    let named = resolve(Cli::Claude, &gone, &candidates).unwrap_or_else(|why| panic!("{why}"));
    assert_eq!((named.pk.as_str(), named.row), (gone.as_str(), None));
    assert_eq!(named.shown(), gone);
    assert_eq!(named.word().as_str(), gone);
    let bea = resolve(Cli::Claude, &key("claude", BEA), &candidates).expect("bea");
    assert_eq!(bea.names, lab.names[&key("claude", BEA)], "the lab's names");
    let ada = resolve(Cli::Claude, &key("claude", ADA), &candidates).expect("ada");
    assert_eq!(ada.row, Some(&fixture.roster[0]), "the row");
    for word in [
        "sa-0123456789abcdef",
        "m2-2",
        "gone@q.example",
        "claude gone@q.example",
    ] {
        assert_eq!(
            resolve(Cli::Claude, word, &candidates),
            Err(no_account(word)),
            "{word}"
        );
    }
    let without = Candidates {
        held: &[],
        ..candidates
    };
    assert_eq!(
        resolve(Cli::Claude, &gone, &without),
        Err(no_account(&gone))
    );
}

/// How a sign-in surface names a roster row ([`super::RowName`], [`super::sign_in_word`],
/// [`super::shown_row`], [`super::logged_row`]), against names written out here from the row's
/// address: under the lab, a message shows the account's name and slug, a step types the slug and
/// a log line the key and slug; with no lab, the name, the key and the key; under a clash (a row
/// here labelled with the account's slug holding another account), the step types the key, which
/// only the whole roster shows; a row with no address is printed by its label marked so, and a
/// step types its label. A label no row holds names itself, which no verb takes.
/// [`super::row_named`] reaches a row from its slug, its key or its address, refuses the label of
/// a row with an address with the resolver's no-account words, takes the label of a row with no
/// address, and refuses the label of a row with no address that is another account's slug with
/// the clash, never that row.
/// NEGATIVE CONTROL: with `row_named`'s fallback taking the row labelled with the word whatever
/// the resolver refused (no `why == no_account(word)`), it fails on `` `claude-dee-w` reached
/// Row { label: "claude-dee-w", …, email: None, … } `` (2026-11-16, by hand, then restored).
#[test]
fn a_rows_sign_in_names_are_its_slug_or_key_and_never_its_label() {
    let fixture = fixture();
    let ada = &fixture.roster[0];
    assert_eq!(ada.label, "m2-2");
    let pk = key("claude", ADA);
    let named = super::RowName::of(Cli::Claude, ada, &fixture.roster, Some(&fixture.lab));
    assert_eq!(named.word.as_str(), "claude-ada-x");
    assert_eq!(named.shown, format!("claude {ADA} (claude-ada-x)"));
    assert_eq!(named.logged, format!("{pk} (claude-ada-x)"));
    let named = super::RowName::of(Cli::Claude, ada, &fixture.roster, None);
    assert_eq!(named.word.as_str(), pk);
    assert_eq!(named.shown, format!("claude {ADA}"));
    assert_eq!(named.logged, pk);
    assert_eq!(
        super::sign_in_word(Cli::Claude, ada, &fixture.roster, None).as_str(),
        pk
    );
    assert_eq!(
        super::shown_row(Cli::Claude, ada, None),
        format!("claude {ADA}")
    );
    assert_eq!(super::logged_row(Cli::Claude, ada, None), pk);
    assert_eq!(
        super::RowName::of_label(Cli::Claude, "m2-2", &fixture.roster, None),
        named,
        "a row named by its label is the row's names"
    );

    // Ada's row labelled with Dee's slug: Dee's step types its key, which only the whole roster
    // shows; Dee's row alone shows no clash.
    let mut clashing = fixture.roster.clone();
    clashing[0].label = "claude-dee-w".to_owned();
    clashing[3].label = "m3-1".to_owned();
    let dee = &clashing[3];
    let dee_pk = key("claude", DEE);
    assert_eq!(
        super::sign_in_word(Cli::Claude, dee, &clashing, Some(&fixture.lab)).as_str(),
        dee_pk
    );
    assert_eq!(
        super::sign_in_word(
            Cli::Claude,
            dee,
            std::slice::from_ref(dee),
            Some(&fixture.lab)
        )
        .as_str(),
        "claude-dee-w",
        "one row alone cannot see the clash, so a step is named among the whole roster"
    );

    // A row with no address: its label, marked so in a message and a log.
    let bare = Row {
        label: "bare".to_owned(),
        ..Row::default()
    };
    let with_bare: Vec<Row> = fixture
        .roster
        .iter()
        .cloned()
        .chain([bare.clone()])
        .collect();
    let named = super::RowName::of(Cli::Claude, &bare, &with_bare, Some(&fixture.lab));
    assert_eq!(named.word.as_str(), "bare");
    assert_eq!(named.shown, "bare (no address)");
    assert_eq!(named.logged, "bare (no address)");
    let gone = super::RowName::of_label(Cli::Claude, "gone", &with_bare, Some(&fixture.lab));
    assert_eq!(
        (
            gone.word.as_str(),
            gone.shown.as_str(),
            gone.logged.as_str()
        ),
        ("gone", "gone", "gone")
    );

    let candidates = Candidates {
        roster: &with_bare,
        lab: Some(&fixture.lab),
        ..Candidates::default()
    };
    let label_of = |word: &str| {
        super::row_named(Cli::Claude, word, &candidates, "nothing was run")
            .map(|row| row.label.clone())
    };
    for word in ["claude-ada-x", pk.as_str(), ADA, &format!("claude {ADA}")] {
        assert_eq!(label_of(word), Ok("m2-2".to_owned()), "`{word}`");
    }
    assert_eq!(label_of("m2-2"), Err(no_account("m2-2")));
    assert_eq!(label_of("bare"), Ok("bare".to_owned()));
    assert_eq!(label_of("nobody"), Err(no_account("nobody")));

    // A row with no address labelled with Dee's slug, Dee's own row labelled otherwise: the
    // slug names Dee's account, and the resolver refuses it for the row labelled with it, which
    // holds no address; that row is never what the word reaches.
    let mut unaddressed_clash = fixture.roster.clone();
    unaddressed_clash[3].label = "m3-1".to_owned();
    unaddressed_clash.push(Row {
        label: "claude-dee-w".to_owned(),
        ..Row::default()
    });
    let candidates = Candidates {
        roster: &unaddressed_clash,
        lab: Some(&fixture.lab),
        ..Candidates::default()
    };
    let reached = super::row_named(Cli::Claude, "claude-dee-w", &candidates, "nothing was run");
    match reached {
        Err(why) => assert!(
            why.contains("roster row labelled `claude-dee-w` holds no address"),
            "{why}"
        ),
        Ok(row) => panic!("`claude-dee-w` reached {row:?}"),
    }
}

/// Each surface's form of an account: a message shows the name and the slug, the wide footer
/// the name, the narrow one the slug, a log line the key and the slug, a step the slug; with
/// no slug, the key stands where the slug would.
#[test]
fn each_surface_prints_the_names_it_is_given() {
    let fixture = fixture();
    let named = resolve(Cli::Claude, ADA, &fixture.candidates()).expect("ada");
    let pk = key("claude", ADA);
    assert_eq!(named.shown(), format!("claude {ADA} (claude-ada-x)"));
    assert_eq!(named.wide(), format!("claude {ADA}"));
    assert_eq!(named.narrow(), "claude-ada-x");
    assert_eq!(named.logged(), format!("{pk} (claude-ada-x)"));
    assert_eq!(named.word().to_string(), "claude-ada-x");
    let bare = Named {
        pk: pk.clone(),
        row: None,
        names: AccountNames::default(),
    };
    assert_eq!(
        [
            bare.shown(),
            bare.wide().to_owned(),
            bare.narrow().to_owned(),
            bare.logged(),
            bare.word().to_string()
        ],
        [pk.clone(), pk.clone(), pk.clone(), pk.clone(), pk]
    );
}

/// A provider word and the address after it are one word; any other pair is left as it is. A
/// raw argv is no input of `words`, and still fails safe: a provider word that is a flag's
/// value keeps the address after it apart, and only an address joins.
/// NEGATIVE CONTROL: with `words` joining a provider word right after a flag, `--cli` loses its
/// value: `` left: ["history", "--cli", "claude ada@x.example"] ``.
#[test]
fn words_joins_a_provider_word_and_the_address_after_it() {
    let joined = |args: &[&str]| words(args);
    assert_eq!(
        joined(&["use", "claude", ADA]),
        ["use".to_owned(), format!("claude {ADA}")]
    );
    assert_eq!(
        joined(&["renew", "kimicn", CY, "oai", BEA]),
        [
            "renew".to_owned(),
            format!("kimicn {CY}"),
            format!("oai {BEA}")
        ]
    );
    for args in [
        &["use", "claude", "m2-2"][..],
        &["use", "codex", ADA],
        &["use", "claude"],
        &["history", "claude:ada@x.example"],
        &["use", ADA],
        &["history", "--cli", "claude", ADA],
        &["history", "--cli", "kimi", CY],
        &["browser", "--browser", "google", ADA],
        &["use", "claude", "@t12"],
        &["use", "claude", "ada@"],
    ] {
        assert_eq!(joined(args), args, "{args:?}");
    }
}
