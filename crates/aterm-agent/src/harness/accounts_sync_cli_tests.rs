// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The other pooled CLIs' fleet (`docs/DESIGN-ai-accounts-multi-cli-2026-10-23.md`): the vault's
//! `ai-accounts-<cli>.toml` and the sheet's columns read for one CLI's pool, the sync that writes
//! its roster beside `aterm.toml` alone, and the opt-in that keeps a machine with no roster of a
//! CLI's out of it. Every fixture email is `example.com`.

use std::sync::{Arc, Mutex};

use super::*;

#[path = "accounts_sync_new_label_tests.rs"]
mod new_label_tests;

#[path = "accounts_provider_realm_tests.rs"]
mod provider_realm_tests;

#[cfg(test)]
#[path = "accounts_source_presence_tests.rs"]
mod source_presence;

const NOW: i64 = 1_792_660_000;

#[test]
fn malformed_automatic_source_is_a_refusal_and_preserves_account_records() {
    for cli in [Cli::Claude, Cli::Codex, Cli::Grok, Cli::Kimi] {
        let member = csv_columns(cli)[0];
        let alias = csv_columns(cli).last().unwrap();
        let value = if cli == Cli::Kimi { "global" } else { "1" };
        let region = if cli == Cli::Kimi {
            "region='global'\n"
        } else {
            ""
        };
        let mut sources = vec![(
            ACCOUNTS_CSV_FILE.to_owned(),
            format!("email,computer,{member},{alias}\nnew@example.test,m16,{value},0\n"),
            "ambiguous",
        )];
        for second in [
            format!("NEW@example.test,m22,{value},\n"),
            format!("NEW@example.test,m16,{value},banned\n"),
            format!("NEW@example.test,?,{value},\n"),
            "NEW@example.test,m16,0,banned\n".to_owned(),
            "NEW@example.test,m16,unsupported-membership,\n".to_owned(),
        ] {
            let first = format!("new@example.test,m16,{value},\n");
            for rows in [format!("{first}{second}"), format!("{second}{first}")] {
                sources.push((
                    ACCOUNTS_CSV_FILE.to_owned(),
                    format!("email,computer,{member},notes\n{rows}"),
                    "conflicting duplicate",
                ));
            }
        }
        for (fields, diagnostic) in [
            ("machines=['m16', 17]", "`machines`"),
            ("machines=['m16', 'unassigned']", "`machines`"),
            ("machines=['m16']\nclass='sharedd'", "`class`"),
            ("machines=['m16']\nclass=false", "`class`"),
        ] {
            sources.push((
                if cli == Cli::Claude {
                    FLEET_ROSTER_FILE.to_owned()
                } else {
                    cli_fleet_file(cli)
                },
                format!(
                    "[[account]]\nlabel='valid'\nemail='valid@example.test'\nmachines=['m16']\n{region}\n\
                     [[account]]\nlabel='invalid'\nemail='invalid@example.test'\n{region}{fields}\n"
                ),
                diagnostic,
            ));
        }
        for (name, source, diagnostic) in sources {
            let bed = Bed::new("malformed-source");
            bed.secret(&name, &source);
            let store = bed.seed_records(cli);
            let paths = [
                bed.roster(cli),
                store.state_file(),
                store.state_witness_file(),
                store.logins_file(),
                store.logins_witness_file(),
                store.ledger_path(),
                store.wake_path(),
            ];
            let before: Vec<_> = paths.iter().map(fs::read).collect();
            let secrets = Bed::tree(&bed.secrets());
            let result = if cli == Cli::Claude {
                sync_accounts_for_init(&bed.home, &bed.state, &bed.roster(cli), NOW).map(|_| ())
            } else {
                bed.sync(cli, None).map(|_| ())
            };
            assert!(
                result.is_err(),
                "{cli:?}: malformed source was skipped or accepted: {result:?}"
            );
            assert!(result.unwrap_err().contains(diagnostic));
            let after: Vec<_> = paths.iter().map(fs::read).collect();
            for (before, after) in before.iter().zip(&after) {
                assert_eq!(before.as_ref().ok(), after.as_ref().ok());
            }
            assert_eq!(Bed::tree(&bed.secrets()), secrets);
        }
    }
}

#[test]
fn automatic_sync_refuses_a_new_label_with_retained_login_state() {
    for cli in OTHER_CLIS {
        let bed = Bed::new("retained-sync-label");
        bed.keep_roster(cli);
        let region = if cli == Cli::Kimi {
            "region='global'\n"
        } else {
            ""
        };
        bed.secret(
            &cli_fleet_file(cli),
            &format!(
                "[[account]]\nlabel='restored'\nemail='new@example.com'\nmachines=['m16']\n{region}"
            ),
        );
        let store = accounts::Store::at_for(&bed.state, cli, &bed.home);
        let mut logins = accounts::Logins::default();
        logins.labels.insert(
            "restored".into(),
            accounts::Binding {
                dir: Some(
                    bed.home
                        .join("previous-login")
                        .to_string_lossy()
                        .into_owned(),
                ),
                ..accounts::Binding::default()
            },
        );
        store.save_logins(&logins).unwrap();
        let before = fs::read(store.logins_file()).unwrap();
        let result = bed.sync(cli, None);
        assert!(
            result.is_err(),
            "{cli:?}: sync restored a label onto an old binding: {result:?}"
        );
        assert!(bed.rows(cli).is_empty());
        assert_eq!(fs::read(store.logins_file()).unwrap(), before);
    }
}

/// A scratch home, state and config directory, `[fleet] machine = "m16"`.
struct Bed {
    _dir: aterm_tempfile::TempDir,
    home: PathBuf,
    state: PathBuf,
    config: PathBuf,
}

impl Bed {
    fn new(tag: &str) -> Self {
        let dir = aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-cli-pool-{tag}-"))
            .tempdir()
            .expect("tempdir");
        let root = dir.path().to_path_buf();
        let (home, config) = (root.join("home"), root.join("config"));
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&config).unwrap();
        fs::write(config.join("aterm.toml"), "[fleet]\nmachine = \"m16\"\n").unwrap();
        Self {
            _dir: dir,
            home,
            state: root.join("state"),
            config,
        }
    }

    fn aterm_toml(&self) -> PathBuf {
        self.config.join("aterm.toml")
    }

    fn roster(&self, cli: Cli) -> PathBuf {
        accounts::roster_beside_for(&self.aterm_toml(), cli).expect("a roster path")
    }

    fn secrets(&self) -> PathBuf {
        self.home.join(".secrets")
    }

    fn secret(&self, name: &str, text: &str) {
        aterm_types::fs_restricted::ensure_private_dir(&self.secrets()).unwrap();
        fs::write(self.secrets().join(name), text).unwrap();
    }

    /// Opts this machine into `cli`'s pool the one way that does: a roster of its own.
    fn keep_roster(&self, cli: Cli) {
        fs::write(self.roster(cli), "").unwrap();
    }

    fn seed_records(&self, cli: Cli) -> accounts::Store {
        fs::write(
            self.roster(cli),
            "[[account]]\nlabel='kept'\nclass='shared'\n",
        )
        .unwrap();
        let store = accounts::Store::at_for(&self.state, cli, &self.home);
        let mut state = accounts::State {
            machine: "m16".into(),
            active: Some("kept".into()),
            pinned: true,
            ..accounts::State::default()
        };
        state.accounts.insert(
            "kept".into(),
            accounts::Account {
                auth: accounts::Auth::SignedOut,
                auth_at: Some(NOW),
                walls: BTreeMap::from([(
                    "seven_day".into(),
                    accounts::Wall {
                        at: NOW,
                        until: Some(NOW + 3600),
                        by: "m16".into(),
                        attempts: 1,
                        cleared: false,
                    },
                )]),
                ..accounts::Account::default()
            },
        );
        store.save(&state).unwrap();
        let mut logins = accounts::Logins::default();
        logins.labels.insert(
            "kept".into(),
            accounts::Binding {
                renewed_at: Some(NOW - 3600),
                renewal_generation: 2,
                ..accounts::Binding::default()
            },
        );
        store.save_logins(&logins).unwrap();
        store
            .append(&[accounts::Event::new(NOW, accounts::Kind::Wall, "kept")])
            .unwrap();
        fs::write(store.wake_path(), b"retained wake").unwrap();
        store
    }

    fn sync(&self, cli: Cli, source: Option<&str>) -> Result<CliSync, String> {
        sync_cli_with(
            cli,
            (&self.home, &self.state, &self.aterm_toml()),
            (source, None),
            NOW,
            (&read_named, &[]),
        )
    }

    fn rows(&self, cli: Cli) -> Vec<Row> {
        Roster::load_for(cli, &self.roster(cli))
            .expect("the roster admits")
            .rows
    }

    /// Every file under `dir` with its bytes, for a before/after look that names what changed.
    fn tree(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut out = BTreeMap::new();
        let mut walk = vec![dir.to_path_buf()];
        while let Some(at) = walk.pop() {
            let Ok(entries) = fs::read_dir(&at) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk.push(path);
                } else {
                    out.insert(path.clone(), fs::read(&path).unwrap_or_default());
                }
            }
        }
        out
    }
}

/// A named source read as the local file it names; no network in a test.
fn read_named(_: &Env, source: &str) -> Result<String, String> {
    fs::read_to_string(source).map_err(|why| format!("{source}: {why}"))
}

#[test]
fn background_signins_refuse_unknown_records_before_dispatch_or_locks() {
    for cli in [Cli::Claude, Cli::Codex, Cli::Grok, Cli::Kimi] {
        for bad in ["state", "bindings", "lost", "lost-bindings"] {
            let bed = Bed::new("signin-read-refusal");
            let store = accounts::Store::at_for(&bed.state, cli, &bed.home);
            store.ensure().unwrap();
            if bad == "lost" {
                fs::write(store.state_witness_file(), "").unwrap();
            } else {
                fs::write(
                    store.state_file(),
                    if bad == "state" {
                        "{".to_owned()
                    } else {
                        accounts::State::default().to_json()
                    },
                )
                .unwrap();
            }
            if bad == "bindings" {
                fs::write(store.logins_file(), "{").unwrap();
            }
            if bad == "lost-bindings" {
                fs::write(store.logins_witness_file(), "").unwrap();
            }
            let browser = bed.home.join(
                ".cache/aterm/browser.noindex/chrome-headless-shell-fixture/chrome-headless-shell",
            );
            fs::create_dir_all(browser.parent().unwrap()).unwrap();
            fs::write(browser, "inert browser fixture").unwrap();
            let region = if cli == Cli::Kimi {
                "region='global'\n"
            } else {
                ""
            };
            let roster = Roster::parse_for(cli, &format!("[[account]]\nlabel='main'\nclass='dedicated'\nemail='owner@example.test'\nbrowser='Main'\n{region}")).unwrap();
            let row = roster.row("main").unwrap().clone();
            let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
            let before = Bed::tree(bed._dir.path());
            let dispatch = spawn_row_renewal(
                cli,
                &bed.home,
                &bed.state,
                &bed.roster(cli),
                roster,
                &accounts::Logins::default(),
                row,
                (None, true),
                NOW,
                &guard,
                &|_| panic!("unknown records cannot dispatch a sign-in"),
                None,
            );
            assert_eq!(dispatch, RenewalDispatch::Fenced, "{cli:?}/{bad}");
            assert!(guard.lock().unwrap().last_attempt.is_empty());
            assert_eq!(Bed::tree(bed._dir.path()), before);
        }
    }
}

#[test]
fn codex_sync_preserves_explicit_identity_when_appending_a_row() {
    for login in ["email='alias@example.test'\n", ""] {
        for (label, email) in [
            ("account", "alias@example.test"),
            ("account", "owner@example.test"),
            ("different", "owner@example.test"),
        ] {
            let bed = Bed::new("codex-identity-policy");
            let text = format!(
                "[[account]]\nlabel='account'\nclass='dedicated'\n{login}identity_email='owner@example.test'\n"
            );
            fs::write(bed.roster(Cli::Codex), &text).unwrap();
            bed.secret(&cli_fleet_file(Cli::Codex), &format!("[[account]]\nlabel='{label}'\nemail='{email}'\nmachines=['m16']\n\n[[account]]\nlabel='second'\nemail='second@example.test'\nmachines=['m16']\n"));
            assert!(matches!(bed.sync(Cli::Codex, None), Ok(CliSync::Synced(_))));
            let after = fs::read_to_string(bed.roster(Cli::Codex)).unwrap();
            assert!(after.starts_with(&text));
            let roster = Roster::load_for(Cli::Codex, &bed.roster(Cli::Codex)).unwrap();
            assert_eq!(roster.rows.len(), 2);
            assert_eq!(
                roster.row("account").unwrap().expected_identity_email(),
                Some("owner@example.test")
            );
        }
    }
}

const KIMI_FLEET: &str = "\
[[account]]
label = \"kimi-1\"
email = \"kimi-one@example.com\"
machines = [\"m16\"]
region = \"global\"

[[account]]
label = \"kimi-22\"
email = \"kimi-other@example.com\"
machines = [\"m22\"]
region = \"mainland-cn\"
";

/// Claude Code's old fleet file, whose rows are Claude Code's alone.
const CLAUDE_FLEET: &str = "\
[[account]]
label = \"m16\"
email = \"claude-one@example.com\"
machines = [\"m16\"]
";

fn labels(rows: &[&FleetAccount]) -> Vec<String> {
    rows.iter().map(|a| a.label.clone()).collect()
}

/// KIMI'S FLEET FILE gives m16 its one row, with its region; a row of m22's is not m16's, and a
/// Kimi row with no region is refused. CONTROLS: m22 reads its own row from the same file, and
/// Claude Code's old `fleet-accounts.toml` never fills Kimi's pool, beside the vault's file or
/// alone.
#[test]
fn kimis_fleet_file_gives_this_machine_its_rows_and_never_claudes() {
    let fleet = FleetRoster::from_toml_for(Cli::Kimi, KIMI_FLEET, "ai-accounts-kimi.toml")
        .expect("the fixture admits");
    let mine = fleet.accounts_for_machine("m16");
    assert_eq!(labels(&mine), ["kimi-1"]);
    assert_eq!(mine[0].vendor.region, Some(Region::Global));
    assert_eq!(
        labels(&fleet.accounts_for_machine("m22")),
        ["kimi-22"],
        "CONTROL: m22's row is real, and m22's alone"
    );
    let no_region = "[[account]]\nlabel = \"kimi-1\"\nemail = \"kimi-one@example.com\"\n\
                     machines = [\"m16\"]\n";
    let refused = FleetRoster::from_toml_for(Cli::Kimi, no_region, "ai-accounts-kimi.toml")
        .expect_err("a Kimi row names its region");
    assert!(refused.contains("region"), "{refused}");
    assert!(
        FleetRoster::from_toml_for(Cli::Codex, KIMI_FLEET, "ai-accounts-codex.toml").is_err(),
        "a Codex row takes no region"
    );

    let bed = Bed::new("kimi-fleet");
    bed.keep_roster(Cli::Kimi);
    bed.secret(&cli_fleet_file(Cli::Kimi), KIMI_FLEET);
    bed.secret(FLEET_ROSTER_FILE, CLAUDE_FLEET);
    let Ok(CliSync::Synced(synced)) = bed.sync(Cli::Kimi, None) else {
        panic!("Kimi's pool syncs from its fleet file");
    };
    assert_eq!(synced.machine, "m16");
    assert_eq!(synced.added, ["kimi-1"]);
    assert_eq!(synced.fleet_accounts, 2);
    let rows = bed.rows(Cli::Kimi);
    let listed: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
    assert_eq!(listed, [ACCOUNTS_OWN_LABEL, "kimi-1"]);
    assert_eq!(rows[1].vendor.region, Some(Region::Global));
    let text = fs::read_to_string(bed.roster(Cli::Kimi)).unwrap();
    assert!(
        !text.contains("kimi-other@"),
        "m22's row is not m16's: {text}"
    );
    assert!(
        !text.contains("claude-one@"),
        "Claude Code's row is not Kimi's: {text}"
    );

    let alone = Bed::new("claude-fleet-alone");
    alone.keep_roster(Cli::Kimi);
    alone.secret(FLEET_ROSTER_FILE, CLAUDE_FLEET);
    assert_eq!(alone.sync(Cli::Kimi, None), Ok(CliSync::NoSource));
    assert_eq!(fs::read_to_string(alone.roster(Cli::Kimi)).unwrap(), "");
}

const SHEET: &str = "\
m,email,computer,,claude,Open AI,xai,kimi,google,meta,Payment,notes
,one@example.com,m16,,1,1,1,global,,,,
,two@example.com,m16.2,,1,browser,,1,,,,kimi banned
,three@example.com,m16.3,,1,,device,mainland-cn,,,,banned
,four@example.com,m16.4,,,,,1,,,,
,five@example.com,m22,,,1,,kimi.com,,,,
";

fn fleet_labels(fleet: &FleetRoster) -> Vec<&str> {
    fleet.accounts.iter().map(|a| a.label.as_str()).collect()
}

/// THE SHEET'S COLUMNS fill each CLI's pool (`Open AI` Codex's, `xai` Grok Build's, `kimi`
/// Kimi Code's), each label `<cli>-<machine label>`, a cell's vendor word kept on its row; a
/// note that names Kimi drops Kimi's membership alone, and a bare `1` in Kimi's column is held.
/// CONTROL: an unscoped `banned` drops the row from every pool, and Claude Code's reading of the
/// same sheet is the one it always was (any disqualifying note drops its row).
#[test]
fn the_sheets_columns_fill_each_pool_and_a_vendor_note_drops_that_vendor_alone() {
    let codex = parse_accounting_csv_for(Cli::Codex, SHEET, "ai-accounts.csv").unwrap();
    assert_eq!(
        fleet_labels(&codex.roster),
        ["codex-m16", "codex-m16-2", "codex-m22"],
        "`kimi banned` leaves two's Codex membership"
    );
    assert_eq!(codex.roster.accounts[1].vendor.flow, Some(Flow::Browser));
    assert!(codex.held.is_empty());

    let grok = parse_accounting_csv_for(Cli::Grok, SHEET, "ai-accounts.csv").unwrap();
    assert_eq!(
        fleet_labels(&grok.roster),
        ["grok-m16"],
        "CONTROL: three's unscoped `banned` drops its Grok membership too"
    );

    let kimi = parse_accounting_csv_for(Cli::Kimi, SHEET, "ai-accounts.csv").unwrap();
    assert_eq!(
        fleet_labels(&kimi.roster),
        ["kimi-m16", "kimi-m22"],
        "two (`kimi banned`) and three (`banned`) are out of Kimi's pool"
    );
    assert_eq!(kimi.roster.accounts[0].vendor.region, Some(Region::Global));
    assert_eq!(
        kimi.roster.accounts[1].vendor.region,
        Some(Region::MainlandCn)
    );
    let held: Vec<&str> = kimi.held.iter().map(|a| a.label.as_str()).collect();
    assert_eq!(held, ["kimi-m16-4"], "a bare 1 names no region");

    assert!(csv_names_column(Cli::Kimi, SHEET));
    assert!(!csv_names_column(Cli::Kimi, "email,computer,claude\n"));

    let claude = parse_accounting_csv(SHEET, "ai-accounts.csv").unwrap();
    assert_eq!(
        fleet_labels(&claude),
        ["m16"],
        "CONTROL: Claude Code's reading drops two (`kimi banned`) as every older build does"
    );
    assert_eq!(
        parse_accounting_csv_for(Cli::Claude, SHEET, "ai-accounts.csv")
            .unwrap()
            .roster,
        claude,
        "Claude Code's column through the per-CLI reader is the same reading"
    );
}

/// The vault's sheet as the ALab vault publishes it (`ai-accounts.csv`, 2026-10-26): its xAI
/// column is headed `x`, between `kimi` and `notes`, after a blank-headed machine column.
const VAULT_SHEET: &str = "\
m,email,computer,,claude,Open AI,google,meta,kimi,x,notes,,
,one@example.com,m16,,1,,,,,1,,,
,two@example.com,m16.2,,1,1,,,,,,,
,three@example.com,m16.3,,,,,,,device,,,
,four@example.com,m22,,1,,,,,1,,,
";

/// THE VAULT'S SHEET heads Grok Build's column `x`: Grok Build's pool reads it, a cell's flow
/// word kept on its row, and a machine that keeps a Grok roster syncs its rows from the sheet
/// instead of finding no Grok column there. CONTROL: Codex's and Claude Code's readings of the
/// same sheet take no row from `x`, and a sheet that heads both `x` and `xai` is refused as
/// ambiguous rather than read by its first match.
#[test]
fn the_vault_sheets_x_column_is_grok_builds() {
    assert!(csv_names_column(Cli::Grok, VAULT_SHEET));
    let grok = parse_accounting_csv_for(Cli::Grok, VAULT_SHEET, "ai-accounts.csv").unwrap();
    assert_eq!(
        fleet_labels(&grok.roster),
        ["grok-m16", "grok-m16-3", "grok-m22"]
    );
    assert_eq!(grok.roster.accounts[1].vendor.flow, Some(Flow::Device));

    let bed = Bed::new("grok-x-column");
    bed.keep_roster(Cli::Grok);
    bed.secret(ACCOUNTS_CSV_FILE, VAULT_SHEET);
    let Ok(CliSync::Synced(synced)) = bed.sync(Cli::Grok, None) else {
        panic!("a Grok roster syncs from the vault's sheet");
    };
    assert!(
        synced.source.ends_with(ACCOUNTS_CSV_FILE),
        "{}",
        synced.source
    );
    assert_eq!(synced.added, ["grok-m16", "grok-m16-3"]);

    let codex = parse_accounting_csv_for(Cli::Codex, VAULT_SHEET, "ai-accounts.csv").unwrap();
    assert_eq!(
        fleet_labels(&codex.roster),
        ["codex-m16-2"],
        "CONTROL: Codex reads `Open AI` alone"
    );
    assert_eq!(
        fleet_labels(&parse_accounting_csv(VAULT_SHEET, "ai-accounts.csv").unwrap()),
        ["m16", "m16-2", "m22"],
        "CONTROL: Claude Code reads `claude` alone"
    );
    for other in ["xai", "grok"] {
        let both = format!("email,computer,{other},x\none@example.com,m16,1,0\n");
        assert!(
            parse_accounting_csv_for(Cli::Grok, &both, "ai-accounts.csv")
                .unwrap_err()
                .contains("ambiguous"),
            "CONTROL: `x` beside `{other}` is two Grok columns"
        );
    }
}

/// A NOTE'S `x` names no vendor, though the vault's sheet heads Grok Build's column so: `x
/// banned` drops the row from every pool, as an unscoped `banned` does, rather than from Grok
/// Build's alone, so a note's stray `x` never keeps a row a disqualifying note was written to
/// drop. CONTROL: `grok banned` on the same row drops Grok Build's membership alone.
#[test]
fn a_notes_x_names_no_vendor_so_x_banned_drops_every_pool() {
    let sheet = |note: &str| {
        format!("email,computer,Open AI,kimi,x,notes\none@example.com,m16,1,global,1,{note}\n")
    };
    let pools = |text: &str| {
        [Cli::Codex, Cli::Grok, Cli::Kimi].map(|cli| {
            parse_accounting_csv_for(cli, text, "ai-accounts.csv")
                .unwrap()
                .roster
                .accounts
                .len()
        })
    };
    assert_eq!(pools(&sheet("x banned")), [0, 0, 0]);
    assert_eq!(
        pools(&sheet("grok banned")),
        [1, 0, 1],
        "CONTROL: a note naming Grok Build drops its membership alone"
    );
}

/// A CLI'S SYNC writes its roster beside `aterm.toml` (`accounts-kimi.toml`) and its logins'
/// directories under `~/.aterm/logins/kimi/`, and nothing under `~/.secrets`. CONTROL: Claude
/// Code's sync in the same home still writes its own `fleet-accounts.toml` beside `aterm.toml`.
#[test]
fn a_clis_sync_writes_its_roster_beside_aterm_toml_and_nothing_under_secrets() {
    let bed = Bed::new("kimi-writes");
    bed.keep_roster(Cli::Kimi);
    bed.secret(&cli_fleet_file(Cli::Kimi), KIMI_FLEET);
    bed.secret(FLEET_ROSTER_FILE, CLAUDE_FLEET);
    let secrets_before = Bed::tree(&bed.secrets());
    let config_before: Vec<PathBuf> = Bed::tree(&bed.config).into_keys().collect();
    let Ok(CliSync::Synced(synced)) = bed.sync(Cli::Kimi, None) else {
        panic!("Kimi's pool syncs");
    };
    assert_eq!(synced.roster, bed.config.join("accounts-kimi.toml"));
    assert_eq!(
        Bed::tree(&bed.secrets()),
        secrets_before,
        "~/.secrets is unchanged"
    );
    let config_after: Vec<PathBuf> = Bed::tree(&bed.config).into_keys().collect();
    assert_eq!(
        config_after, config_before,
        "the roster was there, and is all it wrote"
    );
    assert!(
        !fs::read_to_string(bed.roster(Cli::Kimi))
            .unwrap()
            .is_empty(),
        "the roster holds the synced rows"
    );
    assert!(bed.home.join(".aterm/logins/kimi/kimi-1").is_dir());
    assert!(!bed.config.join(FLEET_ROSTER_FILE).exists());

    let kimi_roster = fs::read(bed.roster(Cli::Kimi)).unwrap();
    let claude = sync_accounts_for_init(
        &bed.home,
        &bed.state,
        &bed.config.join(accounts::ACCOUNTS_FILE),
        NOW,
    );
    assert!(claude.is_ok(), "{claude:?}");
    assert!(
        bed.config.join(FLEET_ROSTER_FILE).is_file(),
        "CONTROL: Claude Code's sync writes its fleet file beside aterm.toml"
    );
    assert_eq!(
        fs::read(bed.roster(Cli::Kimi)).unwrap(),
        kimi_roster,
        "Claude Code's sync leaves Kimi's roster alone"
    );
}

/// Fakes a headless browser under `home` where [`super::super::headless_oauth`] looks.
fn fake_headless_browser(home: &Path) {
    let dir = home.join(".cache/aterm/browser.noindex/chrome-headless-shell-1");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("chrome-headless-shell"), "not a browser").unwrap();
}

/// The window's upkeep over `bed`, with renewals queued (never run) and no fetch.
fn upkeep_over(bed: &Bed, step: UpkeepStep) -> usize {
    type Work = Box<dyn FnOnce() + Send>;
    let queued: Arc<Mutex<Vec<Work>>> = Arc::new(Mutex::new(Vec::new()));
    let into_queue = Arc::clone(&queued);
    let spawn = move |work| {
        into_queue.lock().unwrap().push(work);
        Ok(())
    };
    let fetch = |_: &Env, source: &str| -> Result<String, String> {
        Err(format!("no fetch in a test: {source}"))
    };
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let cooldown = Mutex::new(None);
    let fx = UpkeepEffects {
        fetch: &fetch,
        spawn_renewal: &spawn,
        sync_cooldown: &cooldown,
        rediscovery: &Mutex::new(Rediscovery::default()),
        renew_guard: &guard,
        find_browser: &|_: &Path| false,
        vault_sync: |_: &Path| crate::harness::sign_in_ladder::Vault::NotMember,
        spawn_vault: &|_work| Ok(()),
        mailbox: &ladder_mailbox_files,
        log: std::sync::Arc::new(|_: &str| {}),
        run_attempt: None,
    };
    let _ = upkeep(
        &bed.home,
        &bed.state,
        (&bed.config.join(accounts::ACCOUNTS_FILE), NOW, step),
        &fx,
    );
    let n = queued.lock().unwrap().len();
    queued.lock().unwrap().clear();
    n
}

/// Signs `label` of Kimi's pool in here (a placeholder login file) and marks it signed out, so
/// its renewal is due.
fn signed_out_kimi_login(bed: &Bed, label: &str) {
    let login = bed.home.join(".aterm/logins/kimi").join(label);
    fs::create_dir_all(login.join("credentials")).unwrap();
    fs::write(login.join("credentials/kimi-code.json"), "{}").unwrap();
    let store = accounts::Store::at_for(&bed.state, Cli::Kimi, &bed.home);
    let (mut state, _) = store.load(NOW).unwrap_or_default();
    let mut logins = accounts::Logins::default();
    let _ = accounts::note_login_end(&mut state, &mut logins, label, NOW);
    store.save(&state).unwrap();
}

/// OPT-IN: a vault file of Kimi's alone opts no machine in, so init's sync, the window's sync
/// and its renewal do nothing for Kimi there, and write nothing. CONTROL: the same home with a
/// roster of Kimi's syncs its row at init, and the window renews its due login.
#[test]
fn a_vault_file_alone_opts_no_machine_into_a_clis_pool() {
    let bed = Bed::new("opt-in-none");
    bed.secret(&cli_fleet_file(Cli::Kimi), KIMI_FLEET);
    fake_headless_browser(&bed.home);
    signed_out_kimi_login(&bed, "kimi-1");
    let roster_path = bed.config.join(accounts::ACCOUNTS_FILE);
    let init = sync_cli_accounts_for_init(&bed.home, &bed.state, &roster_path, NOW);
    let kimi = init
        .iter()
        .find(|(cli, _)| *cli == Cli::Kimi)
        .map(|(_, synced)| synced.clone());
    assert_eq!(kimi, Some(Ok(CliSync::Off)));
    assert_eq!(upkeep_over(&bed, UpkeepStep::Both), 0, "nothing renews");
    assert!(!bed.roster(Cli::Kimi).exists(), "no roster is made");
    assert!(
        cli_pools(&bed.aterm_toml(), &bed.state, &bed.home, NOW).is_empty(),
        "no pool is kept"
    );
    assert_eq!(
        bed.sync(Cli::Kimi, None),
        Ok(CliSync::Off),
        "the sync verb reads nothing unnamed"
    );

    bed.keep_roster(Cli::Kimi);
    let retained = sync_cli_accounts_for_init(&bed.home, &bed.state, &roster_path, NOW);
    assert!(
        matches!(retained.iter().find(|(cli, _)| *cli == Cli::Kimi), Some((_, Err(why))) if why.contains("retained local"))
    );

    // A fresh opt-in still imports automatically; retained labels require an
    // explicit recovery decision instead of inheriting their previous state.
    let bed = Bed::new("opt-in-fresh");
    bed.secret(&cli_fleet_file(Cli::Kimi), KIMI_FLEET);
    fake_headless_browser(&bed.home);
    bed.keep_roster(Cli::Kimi);
    let roster_path = bed.config.join(accounts::ACCOUNTS_FILE);
    let init = sync_cli_accounts_for_init(&bed.home, &bed.state, &roster_path, NOW);
    let Some((_, Ok(CliSync::Synced(synced)))) = init.iter().find(|(cli, _)| *cli == Cli::Kimi)
    else {
        panic!("CONTROL: a kept roster syncs at init: {init:?}");
    };
    assert_eq!(synced.added, ["kimi-1"]);
    signed_out_kimi_login(&bed, "kimi-1");
    assert_eq!(
        upkeep_over(&bed, UpkeepStep::Both),
        1,
        "CONTROL: the window renews the due login of a kept roster"
    );
    let pools = cli_pools(&bed.aterm_toml(), &bed.state, &bed.home, NOW);
    assert_eq!(cli_pools_words(&pools).as_deref(), Some("kimi 2"));
    fs::write(
        bed.aterm_toml(),
        "[fleet]\nmachine = \"m16\"\n[harness]\nlogins = \"off\"\n",
    )
    .unwrap();
    assert_eq!(
        upkeep_over(&bed, UpkeepStep::Both),
        0,
        "`[harness] logins = \"off\"` stops the background renewal"
    );
}

/// THE DASHBOARD shows each pool this machine keeps in a card of its own: its product, its rows
/// with their region and `--cli` renewal. CONTROL: a machine that keeps none gets no card, so a
/// Claude-Code-only dashboard is the page it was.
#[test]
fn the_dashboard_shows_a_card_per_kept_pool_and_none_where_none_is_kept() {
    let bed = Bed::new("dashboard");
    assert_eq!(
        pools_html(
            &cli_pools(&bed.aterm_toml(), &bed.state, &bed.home, NOW),
            NOW
        ),
        "",
        "CONTROL: no pool, no card"
    );
    bed.keep_roster(Cli::Kimi);
    bed.secret(&cli_fleet_file(Cli::Kimi), KIMI_FLEET);
    assert!(matches!(bed.sync(Cli::Kimi, None), Ok(CliSync::Synced(_))));
    let html = pools_html(
        &cli_pools(&bed.aterm_toml(), &bed.state, &bed.home, NOW),
        NOW,
    );
    assert!(html.contains("<h2>Kimi Code CLI Accounts</h2>"), "{html}");
    assert!(html.contains("<code>kimi-1</code>"), "{html}");
    assert!(html.contains("global"), "{html}");
    assert!(
        html.contains("aterm harness accounts renew kimi-1 --cli kimi"),
        "{html}"
    );
    assert!(!html.contains("Codex CLI"), "only kept pools: {html}");
    assert!(!html.contains('@'), "no email on the page: {html}");
}

#[test]
fn unreadable_pool_state_and_bindings_are_refused_by_inventory_and_dashboard() {
    for (damaged, kept) in ["state", "logins"]
        .into_iter()
        .flat_map(|damaged| [(damaged, false), (damaged, true)])
    {
        let bed = Bed::new("dashboard-refused");
        if kept {
            fs::write(
                bed.roster(Cli::Kimi),
                "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n",
            )
            .unwrap();
        }
        let store = accounts::Store::at_for(&bed.state, Cli::Kimi, &bed.home);
        store.ensure().unwrap();
        let path = if damaged == "state" {
            store.state_file()
        } else {
            store.logins_file()
        };
        fs::write(&path, "{").unwrap();
        let pools = cli_pools(&bed.aterm_toml(), &bed.state, &bed.home, NOW);
        assert_eq!(cli_pools_words(&pools).as_deref(), Some("kimi refused"));
        let html = pools_html(&pools, NOW);
        assert!(html.contains("The pool is refused:"), "{html}");
        assert!(!html.contains("<code>main</code>"), "{html}");
        assert_eq!(fs::read(&path).unwrap(), b"{");
    }
}

#[test]
fn pool_inventory_recovers_an_admitted_login_before_reading_the_card() {
    let bed = Bed::new("dashboard-recovery");
    fs::write(
        bed.roster(Cli::Kimi),
        "[[account]]\nlabel='main'\nclass='dedicated'\nemail='owner@example.com'\nregion='global'\n",
    )
    .unwrap();
    let store = accounts::Store::at_for(&bed.state, Cli::Kimi, &bed.home);
    let renew = |dir: &str| {
        store.transact_logins(NOW, &mut None, |state, logins| {
            Ok(accounts::note_renewed(
                state, logins, "main", None, dir, None, NOW,
            ))
        })
    };
    renew("/old").unwrap();
    let witness = store.state_witness_file();
    let retained = witness.with_extension("retained");
    fs::rename(&witness, &retained).unwrap();
    fs::create_dir(&witness).unwrap();
    assert!(renew("/new").is_err());
    assert!(store.load(NOW).is_err(), "publication was admitted");
    fs::rename(&witness, witness.with_extension("obstruction")).unwrap();
    fs::rename(&retained, &witness).unwrap();

    let pools = cli_pools(&bed.aterm_toml(), &bed.state, &bed.home, NOW);
    assert_eq!(cli_pools_words(&pools).as_deref(), Some("kimi 1"));
    assert_eq!(
        pools[0].rows.as_ref().unwrap().rows[0].dir.as_deref(),
        Some("/new")
    );
    assert_eq!(pools[0].state.account("main").auth, accounts::Auth::Ok);
}

/// A Grok row's background renewal starts on macOS too, now that its sign-in runs under the
/// no-desk sandbox there (`SignIn::no_desk`): a headless browser installed and the row's email
/// known, its work is handed off (never run here). CONTROL: a row with no email starts nothing.
#[test]
fn a_grok_rows_background_renewal_starts_on_macos() {
    let bed = Bed::new("grok-renewal");
    fake_headless_browser(&bed.home);
    let roster = Roster::parse_for(
        Cli::Grok,
        "[[account]]\nlabel = \"grok-1\"\nclass = \"dedicated\"\nemail = \"grok-one@example.com\"\n\n\
         [[account]]\nlabel = \"grok-2\"\nclass = \"dedicated\"\ndir = \"/x/grok-2\"\n",
    )
    .unwrap();
    let handed = Arc::new(Mutex::new(0usize));
    let count = Arc::clone(&handed);
    let spawn = move |_work: Box<dyn FnOnce() + Send>| {
        *count.lock().unwrap() += 1;
        Ok(())
    };
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let logins = accounts::Logins::default();
    let renew = |label: &str| {
        spawn_vendor_renewal(
            Cli::Grok,
            &bed.home,
            &bed.state,
            &bed.roster(Cli::Grok),
            &roster,
            &logins,
            roster.row(label).cloned().unwrap(),
            NOW,
            &guard,
            &spawn,
        )
    };
    assert_eq!(renew("grok-1"), RenewalDispatch::Started);
    assert_eq!(*handed.lock().unwrap(), 1);
    assert_eq!(
        renew("grok-2"),
        RenewalDispatch::Failed,
        "CONTROL: no email"
    );
    assert_eq!(*handed.lock().unwrap(), 1);
}

#[test]
fn interrupted_vendor_staging_renewal_recovers_binding_and_proof_only_when_complete_and_idle() {
    let bed = Bed::new("vendor-recovery");
    bed.keep_roster(Cli::Kimi);
    let store = accounts::Store::at_for(&bed.state, Cli::Kimi, &bed.home);
    fs::write(
        bed.roster(Cli::Kimi),
        format!(
            "[[account]]\nlabel = \"kimi-1\"\nclass = \"dedicated\"\n\
         email = \"Kimi-One@Example.com\"\nregion = \"global\"\ndir = {}\n",
            toml_quote(store.login_dir("kimi-1").unwrap().to_str().unwrap())
        ),
    )
    .unwrap();
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));

    // CONTROL 1: a checkpoint written for another CLI is never adopted by Kimi's pool.
    let wrong_cli = store.claim_staging_dir("kimi-1").unwrap();
    fs::create_dir_all(wrong_cli.join("credentials")).unwrap();
    fs::write(wrong_cli.join("credentials/kimi-code.json"), "{}").unwrap();
    accounts::write_renewal_checkpoint_for(
        Cli::Grok,
        None,
        "kimi-1",
        "kimi-one@example.com",
        wrong_cli.to_str().unwrap(),
        None,
        Some(accounts::Proof::Page),
        NOW - 20,
        &store.load_logins(NOW).unwrap().0,
    )
    .unwrap();

    // CONTROL 2: a Kimi checkpoint without Kimi's credential file is never adopted.
    let missing_creds = store.claim_staging_dir("kimi-1").unwrap();
    accounts::write_renewal_checkpoint_for(
        Cli::Kimi,
        Some(Region::Global),
        "kimi-1",
        "kimi-one@example.com",
        missing_creds.to_str().unwrap(),
        None,
        Some(accounts::Proof::Status),
        NOW - 15,
        &store.load_logins(NOW).unwrap().0,
    )
    .unwrap();

    assert!(
        !recover_interrupted_staging_renewals_for(
            Cli::Kimi,
            &store,
            &bed.roster(Cli::Kimi),
            &guard,
            NOW
        ),
        "CONTROLS: neither wrong-CLI checkpoint nor credential-less staging dir may recover"
    );

    // Valid completed staging dir interrupted before binding `logins.json`.
    let completed = store.claim_staging_dir("kimi-1").unwrap();
    fs::create_dir_all(completed.join("credentials")).unwrap();
    fs::write(completed.join("credentials/kimi-code.json"), "{}").unwrap();
    accounts::write_renewal_checkpoint_for(
        Cli::Kimi,
        Some(Region::Global),
        "kimi-1",
        "kimi-one@example.com",
        completed.to_str().unwrap(),
        None,
        Some(accounts::Proof::Status),
        NOW - 5,
        &store.load_logins(NOW).unwrap().0,
    )
    .unwrap();

    // CONTROL 3: while a live sign-in holds `signin.lock`, recovery yields instead of racing it.
    let live_lock = store.signin_now().unwrap().expect("signin lock free");
    assert!(
        !recover_interrupted_staging_renewals_for(
            Cli::Kimi,
            &store,
            &bed.roster(Cli::Kimi),
            &guard,
            NOW
        ),
        "CONTROL: recovery yields while signin.lock is held"
    );
    drop(live_lock);

    fake_headless_browser(&bed.home);
    assert_eq!(
        upkeep_over(&bed, UpkeepStep::Renew),
        0,
        "upkeep adopts the completed staging login instead of spawning a duplicate renewal"
    );
    let (state, _) = store.load(NOW).unwrap();
    let (logins, _) = store.load_logins(NOW).unwrap();
    assert_eq!(
        state.accounts.get("kimi-1").map(|a| a.auth),
        Some(accounts::Auth::Ok)
    );
    let binding = logins.get("kimi-1").expect("kimi-1 bound");
    assert_eq!(binding.dir.as_deref(), completed.to_str());
    assert_eq!(binding.renewed_at, Some(NOW - 5));
    assert_eq!(binding.proved_by, Some(accounts::Proof::Status));
}

#[test]
fn vendor_renewal_holds_without_cooldown_on_signin_lock_or_wrong_account_and_skips_limited_rows() {
    let bed = Bed::new("vendor-admission");
    fake_headless_browser(&bed.home);
    bed.keep_roster(Cli::Kimi);
    let kimi_fleet_toml = "\
[[account]]
label = \"kimi-1\"
email = \"kimi-one@example.com\"
machines = [\"m16\"]
region = \"global\"

[[account]]
label = \"kimi-2\"
email = \"kimi-two@example.com\"
machines = [\"m16\"]
region = \"global\"

[[account]]
label = \"kimi-3\"
email = \"kimi-three@example.com\"
machines = [\"m16\"]
region = \"global\"
";
    bed.secret(&cli_fleet_file(Cli::Kimi), kimi_fleet_toml);
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap()
        + 1_000;
    assert!(matches!(bed.sync(Cli::Kimi, None), Ok(CliSync::Synced(_))));
    let roster = Roster::load_for(Cli::Kimi, &bed.roster(Cli::Kimi)).unwrap();
    let store = accounts::Store::at_for(&bed.state, Cli::Kimi, &bed.home);

    let spawned: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
    let count = Arc::clone(&spawned);
    let spawn = move |_work: Box<dyn FnOnce() + Send>| {
        *count.lock().unwrap() += 1;
        Ok(())
    };
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));

    // 1. Contended `signin.lock` returns Held without stamping `last_attempt` cooldown.
    let held_lock = store.signin_now().unwrap().expect("signin lock free");
    let empty_logins = accounts::Logins::default();
    assert_eq!(
        spawn_vendor_renewal(
            Cli::Kimi,
            &bed.home,
            &bed.state,
            &bed.roster(Cli::Kimi),
            &roster,
            &empty_logins,
            roster.row("kimi-1").cloned().unwrap(),
            now,
            &guard,
            &spawn,
        ),
        RenewalDispatch::Held
    );
    assert!(
        !guard
            .lock()
            .unwrap()
            .last_attempt
            .contains_key("kimi:kimi-1"),
        "contended signin.lock must not burn the 15-minute cooldown"
    );
    drop(held_lock);

    // 2. Recent `wrong_at` holds without burning cooldown until mailbox config updates.
    let mut wrong_logins = accounts::Logins::default();
    accounts::note_wrong_account(&mut wrong_logins, "kimi-1", now - 60);
    assert_eq!(
        spawn_vendor_renewal(
            Cli::Kimi,
            &bed.home,
            &bed.state,
            &bed.roster(Cli::Kimi),
            &roster,
            &wrong_logins,
            roster.row("kimi-1").cloned().unwrap(),
            now,
            &guard,
            &spawn,
        ),
        RenewalDispatch::Held
    );
    assert!(
        !guard
            .lock()
            .unwrap()
            .last_attempt
            .contains_key("kimi:kimi-1"),
        "wrong_at cooldown must not stamp AutoRenewGuard::last_attempt"
    );

    let shared = store.shared_dir();
    aterm_types::fs_restricted::ensure_private_dir(&shared).unwrap();
    let mailbox_toml = shared.join("mailbox.toml");
    fs::write(&mailbox_toml, "# updated mailbox routing\n").unwrap();
    let updated_time =
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(u64::try_from(now - 10).unwrap());
    fs::File::options()
        .write(true)
        .open(&mailbox_toml)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(updated_time))
        .unwrap();
    assert_eq!(
        spawn_vendor_renewal(
            Cli::Kimi,
            &bed.home,
            &bed.state,
            &bed.roster(Cli::Kimi),
            &roster,
            &wrong_logins,
            roster.row("kimi-1").cloned().unwrap(),
            now,
            &guard,
            &spawn,
        ),
        RenewalDispatch::Started,
        "CONTROL: updated mailbox config clears the wrong_at hold"
    );
    assert_eq!(*spawned.lock().unwrap(), 1);

    // 3. `renew_pool_due` skips rate-limited rows (`kimi-1`) and prefers clean rows (`kimi-3`)
    // ahead of `wrong_at` rows (`kimi-2`).
    fs::remove_file(&mailbox_toml).unwrap();
    for label in ["kimi-1", "kimi-2", "kimi-3"] {
        signed_out_kimi_login(&bed, label);
    }
    let mut note = None;
    store
        .transact_logins(now, &mut note, |state, logins| {
            let _ = accounts::note_wall(state, "kimi-1", "weekly", Some(now + 3_600), now, now);
            accounts::note_wrong_account(logins, "kimi-2", now - 120);
            Ok(Vec::new())
        })
        .unwrap();
    let fresh_guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let cooldown = Mutex::new(None);
    let fetch = |_: &Env, s: &str| -> Result<String, String> { Err(s.to_owned()) };
    let fx = UpkeepEffects {
        fetch: &fetch,
        spawn_renewal: &spawn,
        sync_cooldown: &cooldown,
        rediscovery: &Mutex::new(Rediscovery::default()),
        renew_guard: &fresh_guard,
        find_browser: &|_: &Path| false,
        vault_sync: |_: &Path| crate::harness::sign_in_ladder::Vault::NotMember,
        spawn_vault: &|_work| Ok(()),
        mailbox: &ladder_mailbox_files,
        log: std::sync::Arc::new(|_: &str| {}),
        run_attempt: None,
    };
    assert!(renew_pool_due(
        Cli::Kimi,
        &bed.home,
        &bed.state,
        &bed.roster(Cli::Kimi),
        now,
        &fx,
    ));
    let keys: Vec<String> = fresh_guard
        .lock()
        .unwrap()
        .last_attempt
        .keys()
        .cloned()
        .collect();
    assert_eq!(
        keys,
        ["kimi:kimi-3"],
        "rate-limited kimi-1 is skipped and clean kimi-3 runs ahead of wrong_at kimi-2"
    );
}

/// ANOTHER CLI'S ROW WHOSE RENEWAL SIGNED IN ANOTHER ACCOUNT IS SIGNED IN AGAIN BY THE UPKEEP
/// ITSELF (the fact the window's wrong-account entry tells, `accounts_wrong_entry_for`): the row,
/// still signed out, stays its pool's candidate, and once the wrong account's hold has passed the
/// upkeep starts its renewal again with nothing a person does. CONTROL: within the hold nothing
/// starts, and no cooldown is burnt.
#[test]
fn a_pool_rows_wrong_account_is_signed_in_again_by_the_upkeep() {
    let bed = Bed::new("vendor-wrong-again");
    fake_headless_browser(&bed.home);
    bed.keep_roster(Cli::Kimi);
    bed.secret(
        &cli_fleet_file(Cli::Kimi),
        "\
[[account]]
label = \"kimi-1\"
email = \"kimi-one@example.com\"
machines = [\"m16\"]
region = \"global\"
",
    );
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap()
        + 1_000;
    assert!(matches!(bed.sync(Cli::Kimi, None), Ok(CliSync::Synced(_))));
    signed_out_kimi_login(&bed, "kimi-1");
    let store = accounts::Store::at_for(&bed.state, Cli::Kimi, &bed.home);
    let spawn = |_work: Box<dyn FnOnce() + Send>| Ok(());
    let fetch = |_: &Env, s: &str| -> Result<String, String> { Err(s.to_owned()) };
    for (ago, again) in [(60, false), (901, true)] {
        let mut note = None;
        store
            .transact_logins(now, &mut note, |_, logins| {
                Ok(accounts::note_wrong_account(logins, "kimi-1", now - ago))
            })
            .unwrap();
        let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
        let cooldown = Mutex::new(None);
        let fx = UpkeepEffects {
            fetch: &fetch,
            spawn_renewal: &spawn,
            sync_cooldown: &cooldown,
            rediscovery: &Mutex::new(Rediscovery::default()),
            renew_guard: &guard,
            find_browser: &|_: &Path| false,
            vault_sync: |_: &Path| crate::harness::sign_in_ladder::Vault::NotMember,
            spawn_vault: &|_work| Ok(()),
            mailbox: &ladder_mailbox_files,
            log: std::sync::Arc::new(|_: &str| {}),
            run_attempt: None,
        };
        assert_eq!(
            renew_pool_due(
                Cli::Kimi,
                &bed.home,
                &bed.state,
                &bed.roster(Cli::Kimi),
                now,
                &fx,
            ),
            again,
            "the wrong account {ago} s ago"
        );
        assert_eq!(
            guard
                .lock()
                .unwrap()
                .last_attempt
                .contains_key("kimi:kimi-1"),
            again,
            "the wrong account {ago} s ago"
        );
    }
}

#[test]
fn durable_sign_in_tasks_hold_across_restarts_sort_last_and_unpark_after_browser_sign_in() {
    let bed = Bed::new("durable-signin-task");
    fake_headless_browser(&bed.home);
    bed.keep_roster(Cli::Kimi);
    let kimi_fleet_toml = "\
[[account]]
label = \"kimi-1\"
email = \"kimi-one@example.com\"
machines = [\"m16\"]
region = \"global\"

[[account]]
label = \"kimi-2\"
email = \"kimi-two@example.com\"
machines = [\"m16\"]
region = \"global\"

[[account]]
label = \"kimi-3\"
email = \"kimi-three@example.com\"
machines = [\"m16\"]
region = \"global\"
";
    bed.secret(&cli_fleet_file(Cli::Kimi), kimi_fleet_toml);
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap()
        + 10_000;
    assert!(matches!(bed.sync(Cli::Kimi, None), Ok(CliSync::Synced(_))));
    let roster = Roster::load_for(Cli::Kimi, &bed.roster(Cli::Kimi)).unwrap();
    let store = accounts::Store::at_for(&bed.state, Cli::Kimi, &bed.home);

    let spawned: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
    let count = Arc::clone(&spawned);
    let spawn = move |_work: Box<dyn FnOnce() + Send>| {
        *count.lock().unwrap() += 1;
        Ok(())
    };

    // 1. Durable `sign_in_task` (`Browser` and `Login`) holds across a fresh `AutoRenewGuard`
    // (simulating restart) within 6h without stamping `last_attempt`.
    for action in [
        accounts::SignInAction::Browser,
        accounts::SignInAction::Login,
    ] {
        let mut task_logins = accounts::Logins::default();
        accounts::note_sign_in_task(&mut task_logins, "kimi-1", action, now - 300);
        let restarted_guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
        assert_eq!(
            spawn_vendor_renewal(
                Cli::Kimi,
                &bed.home,
                &bed.state,
                &bed.roster(Cli::Kimi),
                &roster,
                &task_logins,
                roster.row("kimi-1").cloned().unwrap(),
                now,
                &restarted_guard,
                &spawn,
            ),
            RenewalDispatch::Held,
            "durable {action:?} task must hold across restart within PERSON_BACKOFF"
        );
        assert!(
            !restarted_guard
                .lock()
                .unwrap()
                .last_attempt
                .contains_key("kimi:kimi-1"),
            "person-backoff hold must not stamp 15m last_attempt cooldown"
        );
    }
    assert_eq!(*spawned.lock().unwrap(), 0);

    // 2. `renew_pool_due` runs ready rows (`kimi-2`) ahead of person-parked rows (`kimi-1`).
    for label in ["kimi-1", "kimi-2", "kimi-3"] {
        signed_out_kimi_login(&bed, label);
    }
    let mut note = None;
    store
        .transact_logins(now, &mut note, |_state, logins| {
            accounts::note_sign_in_task(
                logins,
                "kimi-1",
                accounts::SignInAction::Browser,
                now - 120,
            );
            accounts::note_wrong_account(logins, "kimi-3", now - 60);
            Ok(vec![
                accounts::Event::new(now - 120, accounts::Kind::SignIn, "kimi-1")
                    .because("www.kimi.ai device needs-sign-in"),
            ])
        })
        .unwrap();
    let pool_guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let cooldown = Mutex::new(None);
    let fetch = |_: &Env, s: &str| -> Result<String, String> { Err(s.to_owned()) };
    let fx = UpkeepEffects {
        fetch: &fetch,
        spawn_renewal: &spawn,
        sync_cooldown: &cooldown,
        rediscovery: &Mutex::new(Rediscovery::default()),
        renew_guard: &pool_guard,
        find_browser: &|_: &Path| false,
        vault_sync: |_: &Path| crate::harness::sign_in_ladder::Vault::NotMember,
        spawn_vault: &|_work| Ok(()),
        mailbox: &ladder_mailbox_files,
        log: std::sync::Arc::new(|_: &str| {}),
        run_attempt: None,
    };
    assert!(renew_pool_due(
        Cli::Kimi,
        &bed.home,
        &bed.state,
        &bed.roster(Cli::Kimi),
        now,
        &fx,
    ));
    let keys: Vec<String> = pool_guard
        .lock()
        .unwrap()
        .last_attempt
        .keys()
        .cloned()
        .collect();
    assert_eq!(
        keys,
        ["kimi:kimi-2"],
        "ready kimi-2 must renew ahead of sign_in_task kimi-1"
    );

    // 3. Negative controls: `accounts browser` ending with `no-session` or `interrupted
    // session-unknown` keeps `kimi-1` held even on an in-memory parked guard.
    let live_guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    note_background_end(
        &live_guard,
        "kimi:kimi-1",
        super::super::cli::Renewal::NeedsPerson,
        Instant::now(),
    );
    live_guard
        .lock()
        .unwrap()
        .last_attempt
        .insert("kimi:kimi-1".to_owned(), Instant::now());
    let (logins_with_task, _) = store.load_logins(now).unwrap();
    for unsigned_why in [
        "www.kimi.ai window done no-session",
        "www.kimi.ai window interrupted session-unknown",
        "www.kimi.ai window done untidied",
    ] {
        store
            .append(&[
                accounts::Event::new(now - 10, accounts::Kind::SignIn, "kimi-1")
                    .because(unsigned_why),
            ])
            .unwrap();
        assert_eq!(
            spawn_vendor_renewal(
                Cli::Kimi,
                &bed.home,
                &bed.state,
                &bed.roster(Cli::Kimi),
                &roster,
                &logins_with_task,
                roster.row("kimi-1").cloned().unwrap(),
                now,
                &live_guard,
                &spawn,
            ),
            RenewalDispatch::Held,
            "CONTROL: `{unsigned_why}` must not unpark kimi-1"
        );
    }

    // 4. Positive: `accounts browser` recording `window done session-kept` unparks `live_guard`
    // immediately ONCE, and preserves normal 15m `last_attempt` cooldown if that attempt fails.
    store
        .append(&[
            accounts::Event::new(now - 5, accounts::Kind::SignIn, "kimi-1")
                .because("www.kimi.ai window done session-kept"),
        ])
        .unwrap();
    let before = *spawned.lock().unwrap();
    assert_eq!(
        spawn_vendor_renewal(
            Cli::Kimi,
            &bed.home,
            &bed.state,
            &bed.roster(Cli::Kimi),
            &roster,
            &logins_with_task,
            roster.row("kimi-1").cloned().unwrap(),
            now,
            &live_guard,
            &spawn,
        ),
        RenewalDispatch::Started,
        "signed-in `accounts browser` window must unpark step-2 headless renewal immediately"
    );
    assert_eq!(*spawned.lock().unwrap(), before + 1);
    assert_eq!(
        spawn_vendor_renewal(
            Cli::Kimi,
            &bed.home,
            &bed.state,
            &bed.roster(Cli::Kimi),
            &roster,
            &logins_with_task,
            roster.row("kimi-1").cloned().unwrap(),
            now + 5,
            &live_guard,
            &spawn,
        ),
        RenewalDispatch::Held,
        "CONTROL: the same browser window event must not bypass cooldown a second time"
    );

    // 5. Wall-clock expiry of `PERSON_BACKOFF` (`>= 6h`) unparks even if monotonic `Instant`
    // paused during sleep.
    let mut login_task_logins = accounts::Logins::default();
    accounts::note_sign_in_task(
        &mut login_task_logins,
        "kimi-1",
        accounts::SignInAction::Login,
        now,
    );
    let slept_guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    note_background_end(
        &slept_guard,
        "kimi:kimi-1",
        super::super::cli::Renewal::NeedsPerson,
        Instant::now(),
    );
    assert_eq!(
        spawn_vendor_renewal(
            Cli::Kimi,
            &bed.home,
            &bed.state,
            &bed.roster(Cli::Kimi),
            &roster,
            &login_task_logins,
            roster.row("kimi-1").cloned().unwrap(),
            now + PERSON_BACKOFF_SECS,
            &slept_guard,
            &spawn,
        ),
        RenewalDispatch::Started,
        "PERSON_BACKOFF elapsed on wall clock must admit retry after sleep"
    );
}

#[test]
fn walled_or_low_vendor_pool_enrolls_unenrolled_successor_ahead_of_due_rows() {
    let bed = Bed::new("vendor-wall-recovery");
    fake_headless_browser(&bed.home);
    bed.keep_roster(Cli::Kimi);
    let kimi_fleet_toml = "\
[[account]]
label = \"kimi-1\"
email = \"kimi-one@example.com\"
machines = [\"m16\"]
region = \"global\"

[[account]]
label = \"kimi-2\"
email = \"kimi-two@example.com\"
machines = [\"m16\"]
region = \"global\"
";
    bed.secret(&cli_fleet_file(Cli::Kimi), kimi_fleet_toml);
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap()
        + 2_000;
    assert!(matches!(bed.sync(Cli::Kimi, None), Ok(CliSync::Synced(_))));
    let store = accounts::Store::at_for(&bed.state, Cli::Kimi, &bed.home);
    let spawned: Arc<Mutex<usize>> = Arc::new(Mutex::new(0));
    let count = Arc::clone(&spawned);
    let spawn = move |_work: Box<dyn FnOnce() + Send>| {
        *count.lock().unwrap() += 1;
        Ok(())
    };
    let cooldown = Mutex::new(None);
    let fetch = |_: &Env, s: &str| -> Result<String, String> { Err(s.to_owned()) };

    // CONTROL 1: while `main` is healthy (`Unseen`), unenrolled `kimi-1` and `kimi-2` stay idle
    // and `accounts renew --cli kimi --due` reports nothing due.
    let idle_guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let idle_fx = UpkeepEffects {
        fetch: &fetch,
        spawn_renewal: &spawn,
        sync_cooldown: &cooldown,
        rediscovery: &Mutex::new(Rediscovery::default()),
        renew_guard: &idle_guard,
        find_browser: &|_: &Path| false,
        vault_sync: |_: &Path| crate::harness::sign_in_ladder::Vault::NotMember,
        spawn_vault: &|_work| Ok(()),
        mailbox: &ladder_mailbox_files,
        log: std::sync::Arc::new(|_: &str| {}),
        run_attempt: None,
    };
    assert!(
        !renew_pool_due(
            Cli::Kimi,
            &bed.home,
            &bed.state,
            &bed.roster(Cli::Kimi),
            now,
            &idle_fx
        ),
        "CONTROL: healthy unwalled pool must not auto-enroll secondary rows"
    );
    let env = quiet_env(&bed.home, &bed.state, &bed.aterm_toml(), now);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = super::super::cli::run_accounts_of(
        &super::super::cli::AccountsOf {
            cli: Cli::Kimi,
            form: super::super::cli::AccountsCmd::Renew {
                labels: Vec::new(),
                due: true,
                on: None,
            },
            vendor: accounts::Vendor::default(),
        },
        &env,
        &super::super::cli::LiveAccounts,
        &mut out,
        &mut err,
    );
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(
        String::from_utf8_lossy(&out),
        "nothing is due on this machine\n",
        "CONTROL: unwalled `renew --cli kimi --due` finds nothing due"
    );

    // Enroll `kimi-1` and mark it rate-limited + signed-out (`renew_due`, limited), while `kimi-2`
    // remains `NotEnrolled` with an older historical `wrong_at`. When `main` hits a weekly wall,
    // `renew_pool_due` must enroll `kimi-2` (tier 0) rather than stalling on limited `kimi-1`.
    signed_out_kimi_login(&bed, "kimi-1");
    let mut note = None;
    store
        .transact_logins(now, &mut note, |state, logins| {
            let _ = accounts::note_wall(
                state,
                ACCOUNTS_OWN_LABEL,
                "weekly",
                Some(now + 86_400),
                now,
                now,
            );
            let _ = accounts::note_wall(state, "kimi-1", "weekly", Some(now + 86_400), now, now);
            accounts::note_wrong_account(logins, "kimi-2", now - 3_600);
            Ok(Vec::new())
        })
        .unwrap();
    let wall_guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let wall_fx = UpkeepEffects {
        fetch: &fetch,
        spawn_renewal: &spawn,
        sync_cooldown: &cooldown,
        rediscovery: &Mutex::new(Rediscovery::default()),
        renew_guard: &wall_guard,
        find_browser: &|_: &Path| false,
        vault_sync: |_: &Path| crate::harness::sign_in_ladder::Vault::NotMember,
        spawn_vault: &|_work| Ok(()),
        mailbox: &ladder_mailbox_files,
        log: std::sync::Arc::new(|_: &str| {}),
        run_attempt: None,
    };
    assert!(renew_pool_due(
        Cli::Kimi,
        &bed.home,
        &bed.state,
        &bed.roster(Cli::Kimi),
        now,
        &wall_fx,
    ));
    let keys: Vec<String> = wall_guard
        .lock()
        .unwrap()
        .last_attempt
        .keys()
        .cloned()
        .collect();
    assert_eq!(
        keys,
        ["kimi:kimi-2"],
        "walled vendor pool must enroll unenrolled kimi-2 ahead of limited due rows"
    );

    // CONTROL 2: once an enrolled ready successor exists (`kimi-1` signed in and unwalled),
    // `needed_renew_candidates_for` stops emitting tier-0 `NotEnrolled` rows.
    let mut ready_note = None;
    store
        .transact_logins(now + 10, &mut ready_note, |state, _logins| {
            state.accounts.remove("kimi-1");
            let _ = accounts::note_auth(state, "kimi-1", accounts::Auth::Ok, now + 10);
            Ok(Vec::new())
        })
        .unwrap();
    let roster = Roster::load_for(Cli::Kimi, &bed.roster(Cli::Kimi)).unwrap();
    let (ready_state, _) = store.load(now + 10).unwrap();
    let (ready_logins, _) = store.load_logins(now + 10).unwrap();
    assert!(
        needed_renew_candidates_for(
            Cli::Kimi,
            Some(&store),
            &roster,
            &ready_state,
            &ready_logins,
            now + 10,
        )
        .is_empty(),
        "CONTROL: ready enrolled kimi-1 satisfies wall recovery without enrolling kimi-2"
    );
}

#[test]
fn explicit_vendor_account_selection_holds_wakes_and_resumes_to_completion() {
    let bed = Bed::new("vendor-pending-pin");
    fake_headless_browser(&bed.home);
    bed.keep_roster(Cli::Codex);
    let login_1 = bed.home.join(".aterm/logins/codex/codex-1");
    let login_2 = bed.home.join(".aterm/logins/codex/codex-2");
    let login_3 = bed.home.join(".aterm/logins/codex/codex-3");
    for d in [&login_1, &login_2, &login_3] {
        fs::create_dir_all(d).unwrap();
    }
    fs::write(login_1.join("auth.json"), "{}").unwrap();
    fs::write(
        bed.roster(Cli::Codex),
        format!(
            "[[account]]\nlabel = \"codex-1\"\nclass = \"dedicated\"\ndir = \"{}\"\n\
             email = \"codex-one@example.com\"\n\n\
             [[account]]\nlabel = \"codex-2\"\nclass = \"dedicated\"\ndir = \"{}\"\n\
             email = \"codex-two@example.com\"\n\n\
             [[account]]\nlabel = \"codex-3\"\nclass = \"dedicated\"\ndir = \"{}\"\n",
            login_1.display(),
            login_2.display(),
            login_3.display(),
        ),
    )
    .unwrap();
    // Even with `[harness] logins = "off"`, an explicit saved choice must resume.
    fs::write(
        bed.aterm_toml(),
        "[fleet]\nmachine = \"m16\"\n[harness]\nlogins = \"off\"\n",
    )
    .unwrap();
    let now = crate::supervise::limit::unix_now().max(NOW);
    let roster = Roster::load_for(Cli::Codex, &bed.roster(Cli::Codex)).unwrap();
    let codex_store = accounts::Store::at_for(&bed.state, Cli::Codex, &bed.home);
    let claude_store = accounts::Store::at(&bed.state);

    // CONTROL 1: unenrolled row without email refuses without saving `pending_pin`.
    assert!(
        codex_store
            .select_account_for(Cli::Codex, &roster, &bed.roster(Cli::Codex), "codex-3", now)
            .is_err()
    );
    assert!(codex_store.load(now).unwrap().0.pending_pin.is_none());

    // Enrolled row pins immediately in Codex's store alone.
    assert_eq!(
        codex_store
            .select_account_for(Cli::Codex, &roster, &bed.roster(Cli::Codex), "codex-1", now)
            .unwrap(),
        accounts::PinSelection::Pinned
    );
    let (pinned_one, _) = codex_store.load(now).unwrap();
    assert_eq!(pinned_one.active.as_deref(), Some("codex-1"));
    assert!(pinned_one.pinned);
    assert!(claude_store.load(now).unwrap().0.active.is_none());

    // Unenrolled row with email saves `PinSelection::Renew(request)`; a second selection
    // supersedes the earlier nonce.
    let stale = codex_store.request_pin("codex-2", now).unwrap();
    let accounts::PinSelection::Renew(request) = codex_store
        .select_account_for(Cli::Codex, &roster, &bed.roster(Cli::Codex), "codex-2", now)
        .unwrap()
    else {
        panic!("unenrolled codex-2 with email must save a renew request");
    };
    assert_ne!(stale, request);
    assert!(any_pending_pin(&bed.state, now));

    // Hold global admission via an in-flight Claude permit (`wake_pending = Some(bed.state)`).
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let mut claude_permit = AutoRenewPermit::claim(&guard, "m16", Instant::now(), false).unwrap();
    claude_permit.wake_pending = Some(bed.state.clone());
    let _ = fs::remove_file(codex_store.wake_path());
    let _ = fs::remove_file(claude_store.wake_path());

    let noop_spawn = |_: Box<dyn FnOnce() + Send>| Ok(());
    assert_eq!(
        renew_requested_pin_for(
            Cli::Codex,
            &bed.home,
            &bed.state,
            &bed.roster(Cli::Codex),
            request.clone(),
            now,
            &guard,
            &noop_spawn,
        ),
        RenewalDispatch::Held,
        "in-flight Claude renewal must hold Codex's request without cancelling it"
    );
    assert_eq!(
        codex_store.load(now).unwrap().0.pending_pin.as_ref(),
        Some(&request)
    );

    // Dropping the Claude permit wakes BOTH Codex's store wake and the root store wake.
    drop(claude_permit);
    assert!(codex_store.wake_path().is_file());
    assert!(claude_store.wake_path().is_file());

    // CONTROL 2: a stale nonce is reported `Superseded` without disturbing `request`.
    assert_eq!(
        renew_requested_pin_for(
            Cli::Codex,
            &bed.home,
            &bed.state,
            &bed.roster(Cli::Codex),
            stale,
            now,
            &guard,
            &noop_spawn,
        ),
        RenewalDispatch::Superseded
    );

    // Stage a completed Codex checkpoint for `codex-2`; `upkeep` resumes `pending_pin`, recovers
    // the login, pins `codex-2`, and clears `pending_pin` even with `[harness] logins = "off"`.
    let completed = codex_store.claim_staging_dir("codex-2").unwrap();
    fs::write(completed.join("auth.json"), "{}").unwrap();
    accounts::write_renewal_checkpoint_for(
        Cli::Codex,
        None,
        "codex-2",
        "codex-two@example.com",
        completed.to_str().unwrap(),
        None,
        Some(accounts::Proof::Status),
        now + 1,
        &codex_store.load_logins(now + 1).unwrap().0,
    )
    .unwrap();
    let cooldown = Mutex::new(None);
    let fetch = |_: &Env, s: &str| -> Result<String, String> { Err(s.to_owned()) };
    let fx = UpkeepEffects {
        fetch: &fetch,
        spawn_renewal: &noop_spawn,
        sync_cooldown: &cooldown,
        rediscovery: &Mutex::new(Rediscovery::default()),
        renew_guard: &guard,
        find_browser: &|_: &Path| false,
        vault_sync: |_: &Path| crate::harness::sign_in_ladder::Vault::NotMember,
        spawn_vault: &|_work| Ok(()),
        mailbox: &ladder_mailbox_files,
        log: std::sync::Arc::new(|_: &str| {}),
        run_attempt: None,
    };
    let _ = upkeep(
        &bed.home,
        &bed.state,
        (
            &bed.config.join(accounts::ACCOUNTS_FILE),
            now + 2,
            UpkeepStep::Renew,
        ),
        &fx,
    );
    let (after, _) = codex_store.load(now + 2).unwrap();
    assert_eq!(after.active.as_deref(), Some("codex-2"));
    assert!(after.pinned);
    assert!(after.pending_pin.is_none());
    assert!(!any_pending_pin(&bed.state, now + 2));
}
