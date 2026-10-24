// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The other pooled CLIs' fleet (`docs/DESIGN-ai-accounts-multi-cli-2026-10-23.md`): the vault's
//! `ai-accounts-<cli>.toml` and the sheet's columns read for one CLI's pool, the sync that writes
//! its roster beside `aterm.toml` alone, and the opt-in that keeps a machine with no roster of a
//! CLI's out of it. Every fixture email is `example.com`.

use std::sync::{Arc, Mutex};

use super::*;

const NOW: i64 = 1_792_660_000;

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
        renew_guard: &guard,
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
    let init = sync_cli_accounts_for_init(&bed.home, &bed.state, &roster_path, NOW);
    let Some((_, Ok(CliSync::Synced(synced)))) = init.iter().find(|(cli, _)| *cli == Cli::Kimi)
    else {
        panic!("CONTROL: a kept roster syncs at init: {init:?}");
    };
    assert_eq!(synced.added, ["kimi-1"]);
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
    fs::write(
        bed.roster(Cli::Kimi),
        "[[account]]\nlabel = \"kimi-1\"\nclass = \"dedicated\"\n\
         email = \"Kimi-One@Example.com\"\nregion = \"global\"\n",
    )
    .unwrap();
    let roster = Roster::load_for(Cli::Kimi, &bed.roster(Cli::Kimi)).unwrap();
    let store = accounts::Store::at_for(&bed.state, Cli::Kimi, &bed.home);
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));

    // CONTROL 1: a checkpoint written for another CLI is never adopted by Kimi's pool.
    let wrong_cli = store.claim_staging_dir("kimi-1").unwrap();
    fs::create_dir_all(wrong_cli.join("credentials")).unwrap();
    fs::write(wrong_cli.join("credentials/kimi-code.json"), "{}").unwrap();
    accounts::write_renewal_checkpoint_for(
        Cli::Grok,
        "kimi-1",
        "kimi-one@example.com",
        wrong_cli.to_str().unwrap(),
        None,
        Some(accounts::Proof::Page),
        NOW - 20,
    )
    .unwrap();

    // CONTROL 2: a Kimi checkpoint without Kimi's credential file is never adopted.
    let missing_creds = store.claim_staging_dir("kimi-1").unwrap();
    accounts::write_renewal_checkpoint_for(
        Cli::Kimi,
        "kimi-1",
        "kimi-one@example.com",
        missing_creds.to_str().unwrap(),
        None,
        Some(accounts::Proof::Status),
        NOW - 15,
    )
    .unwrap();

    assert!(
        !recover_interrupted_staging_renewals_for(Cli::Kimi, &store, &roster, &guard, NOW),
        "CONTROLS: neither wrong-CLI checkpoint nor credential-less staging dir may recover"
    );

    // Valid completed staging dir interrupted before binding `logins.json`.
    let completed = store.claim_staging_dir("kimi-1").unwrap();
    fs::create_dir_all(completed.join("credentials")).unwrap();
    fs::write(completed.join("credentials/kimi-code.json"), "{}").unwrap();
    accounts::write_renewal_checkpoint_for(
        Cli::Kimi,
        "kimi-1",
        "kimi-one@example.com",
        completed.to_str().unwrap(),
        None,
        Some(accounts::Proof::Status),
        NOW - 5,
    )
    .unwrap();

    // CONTROL 3: while a live sign-in holds `signin.lock`, recovery yields instead of racing it.
    let live_lock = store.signin_now().unwrap().expect("signin lock free");
    assert!(
        !recover_interrupted_staging_renewals_for(Cli::Kimi, &store, &roster, &guard, NOW),
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
        renew_guard: &fresh_guard,
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
