// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The other pooled CLIs' fleet (`docs/DESIGN-ai-accounts-multi-cli-2026-10-23.md`): the vault's
//! `ai-accounts-<cli>.toml` and the sheet's columns read for one CLI's pool, the sync that writes
//! its roster beside `aterm.toml`, and the default import that needs no local roster. Every
//! fixture email is `example.com`.

use std::sync::{Arc, Mutex};

use super::*;

#[path = "accounts_sync_new_label_tests.rs"]
mod new_label_tests;

#[path = "accounts_provider_realm_tests.rs"]
mod provider_realm_tests;

#[path = "accounts_alias_admission_tests.rs"]
mod alias_admission_tests;

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
        // A stored `class` word is no input (design §0), so it refuses nothing:
        // `fleet_assignment_conforms_and_unsafe_coercions_are_detected` holds that.
        for (fields, diagnostic) in [
            ("machines=['m16', 17]", "`machines`"),
            ("machines=['m16', 'unassigned']", "`machines`"),
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

/// A SHEET ROW THAT CONFLICTS skips its address alone: rows that give one address another
/// machine, a disqualifying note, a `?` machine, a `0` with a note or a value the column does not
/// take, in either order, leave that address out of every pool while the sync takes the sheet's
/// other row, and [`sheet_conflicts`] names the rows (never the address) for the window's
/// digest; while the lab is pending (named this Mac before, not read yet) none is named. NEGATIVE
/// CONTROL: before, the sync refused the whole sheet ("conflicting duplicate"), so one bad row
/// stopped every account's sync on every Mac; with `sheet_conflicts_look` gated on a held lab
/// naming this Mac alone, a CLI pool's conflict is still named while pending (`left: [SheetConflict
/// { cli: Codex, .. }] right: []`).
#[test]
fn a_conflicting_sheet_row_skips_that_address_and_the_sync_reads_the_rest() {
    for cli in [Cli::Claude, Cli::Codex, Cli::Grok, Cli::Kimi] {
        let member = csv_columns(cli)[0];
        let value = if cli == Cli::Kimi { "global" } else { "1" };
        for second in [
            format!("NEW@example.test,m22,{value},\n"),
            format!("NEW@example.test,m16,{value},banned\n"),
            format!("NEW@example.test,?,{value},\n"),
            "NEW@example.test,m16,0,banned\n".to_owned(),
            "NEW@example.test,m16,unsupported-membership,\n".to_owned(),
        ] {
            let first = format!("new@example.test,m16,{value},\n");
            for rows in [format!("{first}{second}"), format!("{second}{first}")] {
                let bed = Bed::new("sheet-conflict");
                bed.secret(
                    ACCOUNTS_CSV_FILE,
                    &format!(
                        "email,computer,{member},notes\n{rows}other@example.test,m16,{value},\n"
                    ),
                );
                let synced = if cli == Cli::Claude {
                    sync_accounts_for_init(&bed.home, &bed.state, &bed.roster(cli), NOW).map(|_| ())
                } else {
                    bed.keep_roster(cli);
                    bed.sync(cli, None).map(|_| ())
                };
                assert!(synced.is_ok(), "{cli:?} {rows}: {synced:?}");
                let emails: Vec<String> = bed
                    .rows(cli)
                    .into_iter()
                    .filter_map(|row| row.email)
                    .collect();
                assert!(
                    emails
                        .iter()
                        .any(|e| accounts::email_matches(e, "other@example.test")),
                    "{cli:?} {rows}: the other row is synced: {emails:?}"
                );
                assert!(
                    !emails
                        .iter()
                        .any(|e| accounts::email_matches(e, "new@example.test")),
                    "{cli:?} {rows}: the conflicting address is in no row: {emails:?}"
                );
                let conflicts = sheet_conflicts(&bed.home, &bed.aterm_toml());
                assert_eq!(conflicts.len(), 1, "{cli:?} {rows}: {conflicts:?}");
                assert_eq!(
                    (conflicts[0].cli, conflicts[0].rows.clone()),
                    (cli, vec![2, 3])
                );
                assert_eq!(
                    crate::harness::alab_roster::while_pending(|| sheet_conflicts(
                        &bed.home,
                        &bed.aterm_toml()
                    )),
                    Vec::<SheetConflict>::new(),
                    "{cli:?} {rows}: a lab not read yet reads no sheet"
                );
            }
        }
    }
}

/// A CONFLICT KEEPS THE ROW THIS MACHINE ALREADY LISTS: Claude Code's sync rebuilds the roster
/// from the sheet, so an address whose rows come to conflict would lose its row, and a
/// signed-in account its login, at the next sync. The row stays as it was (label, login
/// directory, class) with the conflicting row below its member row or above it, and the
/// conflict is still named. NEGATIVE CONTROL: without [`carry_conflicting`] the second sync
/// drops the row.
#[test]
fn a_conflicting_sheet_address_this_machine_lists_keeps_its_row() {
    let head = "email,computer,claude,notes\nkeep@example.test,m16,1,\n";
    let member = "alt@example.test,m16,1,\n";
    let conflict = "ALT@example.test,m22,1,\n";
    for rows in [format!("{member}{conflict}"), format!("{conflict}{member}")] {
        let bed = Bed::new("sheet-conflict-kept");
        let roster = bed.roster(Cli::Claude);
        bed.secret(ACCOUNTS_CSV_FILE, &format!("{head}{member}"));
        let first = sync_accounts_for_init(&bed.home, &bed.state, &roster, NOW);
        assert!(first.is_ok(), "{first:?}");
        let alt = |bed: &Bed| {
            bed.rows(Cli::Claude).into_iter().find(|row| {
                row.email
                    .as_deref()
                    .is_some_and(|email| accounts::email_matches(email, "alt@example.test"))
            })
        };
        let before = alt(&bed).expect("the sheet's row is synced");
        assert!(
            before.dir.is_some(),
            "an alternate row of its own: {before:?}"
        );
        bed.secret(ACCOUNTS_CSV_FILE, &format!("{head}{rows}"));
        let synced = sync_accounts_for_init(&bed.home, &bed.state, &roster, NOW + 60);
        assert!(synced.is_ok(), "{rows}: {synced:?}");
        assert_eq!(
            alt(&bed),
            Some(before),
            "{rows}: the listed row is kept as it was"
        );
        assert!(
            bed.rows(Cli::Claude).iter().any(|row| row
                .email
                .as_deref()
                .is_some_and(|email| accounts::email_matches(email, "keep@example.test"))),
            "{rows}: the rest of the sheet is read"
        );
        let conflicts = sheet_conflicts(&bed.home, &bed.aterm_toml());
        assert_eq!(
            conflicts
                .iter()
                .map(|conflict| (conflict.cli, conflict.rows.clone()))
                .collect::<Vec<_>>(),
            [(Cli::Claude, vec![3, 4])],
            "{rows}"
        );
    }
}

/// A CONFLICT NAMING ONLY OTHER MACHINES DROPS THE ROW, as one row naming another machine does:
/// m16 synced `alt@` as `m16-2`, then the sheet lists it for m22 and m33, rows that conflict
/// with each other and agree it is not m16's. The second sync drops the row, exits 0 and names
/// no conflict, the plain drop of the CONTROL, one row `alt@,m22`. NEGATIVE CONTROL: a carry
/// that looked at the address alone kept `m16-2`, while nothing on m16 said why.
#[test]
fn a_conflict_naming_only_other_machines_drops_the_row_as_one_such_row_does() {
    let head = "email,computer,claude,notes\nkeep@example.test,m16,1,\n";
    for later in [
        "alt@example.test,m22,1,\n",
        "alt@example.test,m22,1,\nALT@example.test,m33,1,\n",
    ] {
        let bed = Bed::new("sheet-conflict-elsewhere");
        let roster = bed.roster(Cli::Claude);
        bed.secret(
            ACCOUNTS_CSV_FILE,
            &format!("{head}alt@example.test,m16,1,\n"),
        );
        let first = sync_accounts_for_init(&bed.home, &bed.state, &roster, NOW);
        assert!(first.is_ok(), "{first:?}");
        let alt = |bed: &Bed| {
            bed.rows(Cli::Claude).into_iter().find(|row| {
                row.email
                    .as_deref()
                    .is_some_and(|email| accounts::email_matches(email, "alt@example.test"))
            })
        };
        assert!(
            alt(&bed).is_some_and(|row| row.dir.is_some()),
            "the sheet's row is synced"
        );
        bed.secret(ACCOUNTS_CSV_FILE, &format!("{head}{later}"));
        let env = quiet_env(&bed.home, &bed.state, &bed.aterm_toml(), NOW + 60);
        let sheet = bed.secrets().join(ACCOUNTS_CSV_FILE);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run_accounts_sync(&env, None, sheet.to_str(), None, false, &mut out, &mut err);
        let err = String::from_utf8(err).unwrap();
        assert_eq!(code, ExitCode::SUCCESS, "{later}: {err}");
        assert_eq!(alt(&bed), None, "{later}: the row drops");
        assert!(
            !err.contains("conflicting assignments"),
            "{later}: no conflict of this machine's: {err}"
        );
        assert_eq!(
            sheet_conflicts(&bed.home, &bed.aterm_toml()),
            Vec::<SheetConflict>::new(),
            "{later}"
        );
        assert!(
            bed.rows(Cli::Claude).iter().any(|row| row
                .email
                .as_deref()
                .is_some_and(|email| accounts::email_matches(email, "keep@example.test"))),
            "{later}: the rest of the sheet is read"
        );
    }
}

/// A CARRIED ROW GETS ITS TRANSCRIPT WALLS: the walls a sync reads from Claude Code's transcripts
/// go to the rows it publishes by address, and a row a conflict carries ([`carry_conflicting`])
/// is one of them. A transcript that signed in as `alt@` and hit its weekly limit puts that
/// wall on `m16-2`, the row the sync kept. NEGATIVE CONTROL: the rows the walls were matched
/// against came from the sheet before the carry, which lists no `alt@`, so the wall went
/// nowhere.
#[test]
fn a_row_a_conflict_carries_gets_its_transcript_walls() {
    let head = "email,computer,claude,notes\nkeep@example.test,m16,1,\n";
    let bed = Bed::new("sheet-conflict-walls");
    let roster = bed.roster(Cli::Claude);
    bed.secret(
        ACCOUNTS_CSV_FILE,
        &format!("{head}alt@example.test,m16,1,\n"),
    );
    let first = sync_accounts_for_init(&bed.home, &bed.state, &roster, NOW);
    assert!(first.is_ok(), "{first:?}");
    let label = bed
        .rows(Cli::Claude)
        .into_iter()
        .find(|row| {
            row.email
                .as_deref()
                .is_some_and(|email| accounts::email_matches(email, "alt@example.test"))
        })
        .expect("the sheet's row is synced")
        .label;
    let projects = bed.home.join(".claude").join("projects").join("work");
    fs::create_dir_all(&projects).unwrap();
    fs::write(
        projects.join("session.jsonl"),
        "{\"type\":\"attachment\",\"attachment\":{\"type\":\"session_context\",\
         \"context\":{\"userEmail\":\"alt@example.test\",\"cwd\":\"/Users//someone/work\"}}}\n\
         {\"type\":\"assistant\",\"timestamp\":\"2026-10-22T06:00:00Z\",\"isApiErrorMessage\":true,\
         \"error\":\"rate_limit\",\"message\":{\"model\":\"<synthetic>\",\"content\":[{\"type\":\
         \"text\",\"text\":\"You've hit your weekly limit\"}]},\"quotaLimits\":{\"status\":\
         \"rejected\",\"resetsAt\":1792767600,\"rateLimitType\":\"seven_day\"}}\n",
    )
    .unwrap();
    bed.secret(
        ACCOUNTS_CSV_FILE,
        &format!("{head}alt@example.test,m16,1,\nALT@example.test,m22,1,\n"),
    );
    let synced = sync_accounts_for_init(&bed.home, &bed.state, &roster, NOW + 60);
    assert!(synced.is_ok(), "{synced:?}");
    assert!(
        bed.rows(Cli::Claude).iter().any(|row| row.label == label),
        "the conflict carries `{label}`"
    );
    let (state, _) = accounts::Store::at(&bed.state).load(NOW + 60).unwrap();
    let walls = state
        .accounts
        .get(&label)
        .map(|account| account.walls.clone())
        .unwrap_or_default();
    assert_eq!(
        walls.get("seven_day").and_then(|wall| wall.until),
        Some(1_792_767_600),
        "the transcript's wall is `{label}`'s: {walls:?}"
    );
}

/// THE SYNC VERBS NAME THE SHEET'S CONFLICTS ON THEIR EXITS THAT SYNC NOTHING, where a row of
/// this machine's that conflicts is often why: Claude Code's "no active Claude accounts found",
/// another pool's by-default sync that finds no row of this machine's
/// ([`CliSync::Unlisted`]), and its sync of a named source that lists none. Each exits 1 and
/// says the CLI and the rows, never the address. CONTROL: the same exits with no conflict say
/// none. NEGATIVE CONTROL: before, only init, doctor and the upkeep's log said it.
#[test]
fn a_sync_that_syncs_nothing_names_the_sheets_conflicts() {
    let mut wrong = Vec::new();
    for cli in [Cli::Claude, Cli::Codex, Cli::Grok, Cli::Kimi] {
        let member = csv_columns(cli)[0];
        let value = if cli == Cli::Kimi { "global" } else { "1" };
        let exits: &[bool] = if cli == Cli::Claude {
            &[true]
        } else {
            &[false, true]
        };
        for &named in exits {
            for conflicted in [true, false] {
                let bed = Bed::new("sheet-conflict-nothing");
                // Rows 2 and 3 give `new@` m16 and m22, so m16 lists none; the CONTROL's give
                // it m9 alone, which lists none of m16's either.
                let (first, second) = if conflicted {
                    ("m16", "NEW@example.test,m22")
                } else {
                    ("m9", "other@example.test,m9")
                };
                bed.secret(
                    ACCOUNTS_CSV_FILE,
                    &format!(
                        "email,computer,{member},notes\nnew@example.test,{first},{value},\n\
                         {second},{value},\nlast@example.test,m9,{value},\n"
                    ),
                );
                if cli != Cli::Claude && named {
                    bed.keep_roster(cli);
                }
                let env = quiet_env(&bed.home, &bed.state, &bed.aterm_toml(), NOW);
                let sheet = bed.secrets().join(ACCOUNTS_CSV_FILE);
                let (mut out, mut err) = (Vec::new(), Vec::new());
                let code = if cli == Cli::Claude {
                    run_accounts_sync(&env, None, sheet.to_str(), None, false, &mut out, &mut err)
                } else {
                    let source = named.then(|| sheet.to_str()).flatten();
                    run_cli_accounts_sync(
                        cli,
                        (&env, None),
                        (source, None, false),
                        &mut out,
                        &mut err,
                    )
                };
                let err = String::from_utf8(err).unwrap();
                assert_eq!(code, ExitCode::FAILURE, "{cli:?} {named}: {err}");
                assert!(
                    err.contains("accounts found for machine `m16`"),
                    "{cli:?} {named}: the exit is the one that syncs nothing: {err}"
                );
                let said = format!("conflicting assignments ({} rows 2, 3)", cli.id());
                if err.contains(&said) != conflicted {
                    wrong.push(format!(
                        "{cli:?} named={named} conflicted={conflicted}: {err}"
                    ));
                }
                assert!(
                    !err.to_ascii_lowercase().contains("new@example.test"),
                    "{cli:?}: no address is said: {err}"
                );
            }
        }
    }
    assert!(wrong.is_empty(), "each exit says its conflicts: {wrong:#?}");
}

/// `aterm harness accounts sync` SAYS THE SHEET'S CONFLICTS, Claude Code's and `--cli`'s, plain
/// and `--json`: the CLI and its rows, never the address, so a Mac with no window learns why an
/// account is missing. CONTROL: a sheet whose rows agree says none. NEGATIVE CONTROL: before,
/// only the window's digest said it.
#[test]
fn an_accounts_sync_names_the_sheets_conflicting_rows() {
    for cli in [Cli::Claude, Cli::Codex, Cli::Grok, Cli::Kimi] {
        let member = csv_columns(cli)[0];
        let value = if cli == Cli::Kimi { "global" } else { "1" };
        for (conflicted, json) in [(true, false), (true, true), (false, false)] {
            let bed = Bed::new("sheet-conflict-said");
            let second = if conflicted {
                format!("NEW@example.test,m22,{value},\n")
            } else {
                String::new()
            };
            bed.secret(
                ACCOUNTS_CSV_FILE,
                &format!(
                    "email,computer,{member},notes\nnew@example.test,m16,{value},\n{second}\
                     other@example.test,m16,{value},\n"
                ),
            );
            if cli != Cli::Claude {
                bed.keep_roster(cli);
            }
            let env = quiet_env(&bed.home, &bed.state, &bed.aterm_toml(), NOW);
            let sheet = bed.secrets().join(ACCOUNTS_CSV_FILE);
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let code = if cli == Cli::Claude {
                run_accounts_sync(&env, None, sheet.to_str(), None, json, &mut out, &mut err)
            } else {
                run_cli_accounts_sync(cli, (&env, None), (None, None, json), &mut out, &mut err)
            };
            let (out, err) = (
                String::from_utf8(out).unwrap(),
                String::from_utf8(err).unwrap(),
            );
            assert_eq!(code, ExitCode::SUCCESS, "{cli:?}: {err}");
            let said = format!("conflicting assignments ({} rows 2, 3)", cli.id());
            assert_eq!(err.contains(&said), conflicted, "{cli:?} {json}: {err}");
            assert!(
                !format!("{out}{err}")
                    .to_ascii_lowercase()
                    .contains("new@example.test"),
                "{cli:?}: no address is said: {out}{err}"
            );
            if json {
                let report: Value = aterm_json::from_str(out.trim()).unwrap();
                let listed = report.get("sheet_conflicts").and_then(Value::as_array);
                assert_eq!(
                    listed.map(Vec::len),
                    Some(1),
                    "{cli:?}: the report lists the conflict: {out}"
                );
            }
        }
    }
}

/// THE UPKEEP LOGS A SHEET CONFLICT ONCE PER CHANGE: a Mac with no window shows no digest, so the
/// window's upkeep, where it syncs, logs the conflicts the sheet's rows make in a pool it syncs
/// (the CLI and its rows, never the address) once, again when they change, and once when the
/// rows agree. NEGATIVE CONTROL: before, the upkeep logged none.
#[test]
fn the_upkeep_logs_a_sheet_conflict_once_per_change() {
    let bed = Bed::new("sheet-conflict-log");
    bed.keep_roster(Cli::Kimi);
    let sheet = |rows: &str| {
        bed.secret(
            ACCOUNTS_CSV_FILE,
            &format!("email,computer,kimi,notes\nother@example.test,m16,global,\n{rows}"),
        );
    };
    let lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let rediscovery = Mutex::new(Rediscovery::default());
    let upkeep_once = || {
        let into = Arc::clone(&lines);
        let log: SignInLog = Arc::new(move |line: &str| into.lock().unwrap().push(line.to_owned()));
        let fetch = |_: &Env, source: &str| -> Result<String, String> {
            Err(format!("no fetch in a test: {source}"))
        };
        with_logged_effects((&|_work| Ok(()), &fetch), (&rediscovery, log), |fx| {
            let _ = upkeep(
                &bed.home,
                &bed.state,
                (
                    &bed.config.join(accounts::ACCOUNTS_FILE),
                    NOW,
                    UpkeepStep::Sync,
                ),
                fx,
            );
        });
        let said: Vec<String> = lines
            .lock()
            .unwrap()
            .drain(..)
            .filter(|line| line.contains("account sheet"))
            .collect();
        said
    };
    sheet("new@example.test,m16,global,\nNEW@example.test,m22,global,\n");
    let said = upkeep_once();
    assert!(
        said.len() == 1 && said[0].contains("(kimi rows 3, 4)"),
        "{said:?}"
    );
    assert!(!said[0].to_ascii_lowercase().contains("new@example.test"));
    assert_eq!(
        upkeep_once(),
        Vec::<String>::new(),
        "said once, not per upkeep"
    );
    sheet(
        "new@example.test,m16,global,\nNEW@example.test,m22,global,\nnew@example.test,m9,global,\n",
    );
    let said = upkeep_once();
    assert!(
        said.len() == 1 && said[0].contains("(kimi rows 3, 4, 5)"),
        "a change is said again: {said:?}"
    );
    sheet("new@example.test,m16,global,\n");
    let said = upkeep_once();
    assert!(
        said.len() == 1 && said[0].contains("agree now"),
        "the end is said once: {said:?}"
    );
    assert_eq!(upkeep_once(), Vec::<String>::new());
}

/// THE UPKEEP TELLS A SHEET IT CANNOT READ FROM ROWS THAT AGREE: a conflict it logged ends with
/// "agree now" only where the sheet was read whole; where the sheet is gone or no longer parses,
/// the log says once that whether its rows conflict is not known. NEGATIVE CONTROL: before, a
/// removed or broken sheet was logged as rows that agree.
#[test]
fn the_upkeep_tells_an_unread_sheet_from_rows_that_agree() {
    let bed = Bed::new("sheet-conflict-unread");
    bed.keep_roster(Cli::Kimi);
    let conflict = "email,computer,kimi,notes\nother@example.test,m16,global,\n\
                    new@example.test,m16,global,\nNEW@example.test,m22,global,\n";
    let lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let rediscovery = Mutex::new(Rediscovery::default());
    let upkeep_once = || {
        let into = Arc::clone(&lines);
        let log: SignInLog = Arc::new(move |line: &str| into.lock().unwrap().push(line.to_owned()));
        let fetch = |_: &Env, source: &str| -> Result<String, String> {
            Err(format!("no fetch in a test: {source}"))
        };
        with_logged_effects((&|_work| Ok(()), &fetch), (&rediscovery, log), |fx| {
            let _ = upkeep(
                &bed.home,
                &bed.state,
                (
                    &bed.config.join(accounts::ACCOUNTS_FILE),
                    NOW,
                    UpkeepStep::Sync,
                ),
                fx,
            );
        });
        let said: Vec<String> = lines
            .lock()
            .unwrap()
            .drain(..)
            .filter(|line| line.starts_with("accounts sync: the account sheet"))
            .collect();
        said
    };
    let sheet = bed.secrets().join(ACCOUNTS_CSV_FILE);
    for unread in [
        None,
        Some("email,computer,kimi,notes\n\"broken@example.test,m16,global,\n"),
    ] {
        bed.secret(ACCOUNTS_CSV_FILE, conflict);
        let said = upkeep_once();
        assert!(
            said.len() == 1 && said[0].contains("(kimi rows 3, 4)"),
            "{unread:?}: {said:?}"
        );
        match unread {
            None => fs::remove_file(&sheet).unwrap(),
            Some(text) => bed.secret(ACCOUNTS_CSV_FILE, text),
        }
        let said = upkeep_once();
        assert!(
            said.len() == 1 && said[0].contains("not read whole") && !said[0].contains("agree"),
            "{unread:?}: an unread sheet is not rows that agree: {said:?}"
        );
        assert_eq!(upkeep_once(), Vec::<String>::new(), "{unread:?}: said once");
    }
}

/// A LOOK AT THE SHEET ASKS THE HOST ITS NAME ONCE: the window's look reads the sheet's conflicts
/// ([`sheet_conflicts`]) on every pass, and Claude Code's half reads [`fleet_origin`], which asks
/// [`sheet_names_this_machine`] whose machine comes from the host where `[fleet] machine` names
/// none. That is read once per process ([`detected_machine`]), never two `scutil`s and a
/// `hostname` per look. NEGATIVE CONTROL: [`machine_for`] asked the host on every call.
#[test]
fn a_look_at_the_sheet_asks_the_host_its_name_once() {
    let bed = Bed::new("host-once");
    crate::test_config::write(bed.aterm_toml(), "").unwrap();
    bed.secret(
        ACCOUNTS_CSV_FILE,
        "email,computer,claude,notes\nnew@example.test,m16,1,\n",
    );
    let sheet = bed.secrets().join(ACCOUNTS_CSV_FILE);
    let config = bed.aterm_toml();
    let _ = sheet_names_this_machine(Some(&config), &sheet);
    let asked = HOST_ASKED.with(std::cell::Cell::get);
    for _ in 0..3 {
        let _ = sheet_names_this_machine(Some(&config), &sheet);
    }
    assert_eq!(HOST_ASKED.with(std::cell::Cell::get), asked);
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

/// The empty persistent scheduling lock is not retained account or recovery state.
fn empty_queue_gate(root: &Path, path: &Path, bytes: &[u8]) -> bool {
    path == root.join("state/accounts/signin-queue/queue.lock")
        && bytes.is_empty()
        && fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_file())
}

#[test]
fn only_the_exact_empty_queue_gate_is_outside_account_snapshots() {
    let bed = Bed::new("queue-gate-snapshot");
    let gate = bed.state.join("accounts/signin-queue/queue.lock");
    fs::create_dir_all(gate.parent().unwrap()).unwrap();
    fs::write(&gate, []).unwrap();
    assert!(empty_queue_gate(bed._dir.path(), &gate, b""));
    fs::write(&gate, b"unknown").unwrap();
    assert!(!empty_queue_gate(bed._dir.path(), &gate, b"unknown"));
    let ticket = gate.with_file_name("ticket-unknown");
    fs::write(&ticket, []).unwrap();
    assert!(!empty_queue_gate(bed._dir.path(), &ticket, b""));
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
        aterm_uds::atomic_file::write(
            &config.join("aterm.toml"),
            b"[fleet]\nmachine = \"m16\"\n",
            0o600,
        )
        .unwrap();
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

    /// Keeps a roster of `cli`'s pool beside `aterm.toml`.
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
fn the_source_table_includes_pending_rows_without_a_sync_or_machine_id() {
    let bed = Bed::new("source-table");
    bed.secret(
        ACCOUNTS_CSV_FILE,
        "email,computer,kimi\nfirst@example.test,m16,global\nsecond@example.test,m22,1\n",
    );
    let env = quiet_env(&bed.home, &bed.state, &bed.aterm_toml(), NOW);
    let fleet = account_source_table(Cli::Kimi, &env, None).expect("source table");
    assert_eq!(fleet.roster.accounts.len(), 1);
    assert_eq!(fleet.roster.accounts[0].label, "kimi-m16");
    assert_eq!(fleet.roster.accounts[0].machines, ["m16"]);
    assert_eq!(fleet.held.len(), 1);
    assert_eq!(fleet.held[0].label, "kimi-m22");
    assert_eq!(fleet.held[0].machines, ["m22"]);
    assert!(!bed.roster(Cli::Kimi).exists());

    let pool_bed = Bed::new("source-table-ai-pool-no-machine");
    crate::test_config::write(pool_bed.aterm_toml(), "").unwrap();
    pool_bed.secret(
        super::super::ai_pool::AI_POOL_FILE,
        "version = 1\n\n[[account]]\nprovider = \"codex\"\nemail = \"codex-one@example.test\"\nwarm = [\"m16\"]\n",
    );
    let pool_env = quiet_env(&pool_bed.home, &pool_bed.state, &pool_bed.aterm_toml(), NOW);
    let pool_fleet =
        account_source_table(Cli::Codex, &pool_env, None).expect("ai-pool source table");
    assert_eq!(pool_fleet.roster.accounts.len(), 1);
    assert_eq!(pool_fleet.roster.accounts[0].label, "codex-m16");
    assert_eq!(pool_fleet.roster.accounts[0].machines, ["m16"]);
    assert!(matches!(
        pool_bed.sync(Cli::Codex, None),
        Ok(CliSync::Unlisted { machine: None, .. })
    ));
    assert!(cli_pools_due(&pool_bed.aterm_toml(), &pool_bed.state, &pool_bed.home, NOW).is_empty());
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
                (
                    row.clone(),
                    crate::harness::sign_in_ladder::asked_fixture(cli, &row.label),
                ),
                (None, true),
                NOW,
                &guard,
                &|_| panic!("unknown records cannot dispatch a sign-in"),
                None,
                None,
                &mut None,
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

#[test]
fn the_resync_source_check_keeps_each_pools_policy_and_reports_refused_sources() {
    for cli in OTHER_CLIS {
        let bed = Bed::new("resync-pool-source");
        let roster = bed.config.join(accounts::ACCOUNTS_FILE);
        let read = Mutex::new(Rediscovery::default());
        let probe = || {
            read_resync_with(
                &bed.home,
                &bed.state,
                &roster,
                NOW,
                &read,
                &read_local_source_text,
            )
        };
        assert_eq!(probe(), ResyncRead::NoSource);
        let region = if cli == Cli::Kimi {
            "region = 'global'\n"
        } else {
            ""
        };
        let fleet = format!(
            "[[account]]\nlabel='source-one'\nemail='source-one@example.com'\n\
             machines=['m16']\n{region}"
        );
        let source = bed.secrets().join(cli_fleet_file(cli));
        bed.secret(&cli_fleet_file(cli), &fleet);
        assert!(
            matches!(probe(), ResyncRead::Due(_)),
            "{cli:?}: a listed pool syncs by default without a local roster"
        );
        bed.keep_roster(cli);
        assert!(
            matches!(probe(), ResyncRead::Due(_)),
            "{cli:?}: a kept roster still has its source"
        );
        assert_eq!(fs::read_to_string(&source).unwrap(), fleet);
        assert!(fs::read(bed.roster(cli)).unwrap().is_empty());
        assert!(!bed.state.exists(), "a source read writes no account state");

        assert!(matches!(bed.sync(cli, None), Ok(CliSync::Synced(_))));
        assert_eq!(
            probe(),
            ResyncRead::Current,
            "{cli:?}: the actual sync settled its pool"
        );

        fs::write(&source, "[[account]\n").unwrap();
        assert!(
            matches!(probe(), ResyncRead::Due(_)),
            "{cli:?}: a refused source must be reported"
        );
        let before = Bed::tree(bed._dir.path());
        assert!(bed.sync(cli, None).is_err());
        assert_eq!(
            Bed::tree(bed._dir.path()),
            before,
            "a refused source changes no account records"
        );
        assert_eq!(fs::read_to_string(&source).unwrap(), "[[account]\n");
        fs::remove_file(bed.roster(cli)).unwrap();
        assert!(
            matches!(probe(), ResyncRead::Due(_)),
            "{cli:?}: a refused source is reported without a local roster"
        );
        crate::test_config::write(
            bed.aterm_toml(),
            "[fleet]\nmachine = 'm16'\n[harness]\naccounts = 'off'\n",
        )
        .unwrap();
        let before = Bed::tree(bed._dir.path());
        assert_eq!(
            resync_for_setup(&bed.home, &bed.state, &roster, NOW, false),
            Resync::default(),
            "{cli:?}: Off holds the automatic sync over a refused source"
        );
        assert_eq!(Bed::tree(bed._dir.path()), before);
    }
}

/// THE CONVERGE READ OF A POOL names the labels its sync adds, and nothing once the sync ran; a
/// row the vault adds for this machine reads due again. CONTROLS: a listed pool needs no local
/// roster; an explicit Off and another machine's fleet row each read nothing due.
/// NEGATIVE CONTROL: a read that never looks at the other CLIs' pools reads the added row as
/// nothing due.
#[test]
fn a_pools_converge_read_names_what_its_sync_adds_and_nothing_once_it_ran() {
    let bed = Bed::new("pool-due");
    bed.keep_roster(Cli::Kimi);
    bed.secret(&cli_fleet_file(Cli::Kimi), KIMI_FLEET);
    let read = Mutex::new(Rediscovery::default());
    let probe = || {
        read_resync_with(
            &bed.home,
            &bed.state,
            &bed.config.join(accounts::ACCOUNTS_FILE),
            NOW,
            &read,
            &read_local_source_text,
        )
    };
    assert!(matches!(probe(), ResyncRead::Due(why) if why.contains("kimi-1")));
    let Ok(CliSync::Synced(synced)) = bed.sync(Cli::Kimi, None) else {
        panic!("Kimi's pool syncs");
    };
    assert_eq!(
        synced.added,
        ["kimi-1"],
        "the read named what the sync added"
    );
    assert_eq!(probe(), ResyncRead::Current, "nothing left to add");

    bed.secret(
        &cli_fleet_file(Cli::Kimi),
        &format!(
            "{KIMI_FLEET}\n[[account]]\nlabel = \"kimi-2\"\nemail = \"kimi-two@example.com\"\n\
             machines = [\"m16\"]\nregion = \"global\"\n"
        ),
    );
    assert!(
        matches!(probe(), ResyncRead::Due(why) if why.contains("kimi-2")),
        "the vault's new row is due"
    );

    let off = Bed::new("pool-due-off");
    off.secret(&cli_fleet_file(Cli::Kimi), KIMI_FLEET);
    assert!(
        matches!(read_resync(&off.home, &off.state, &off.config.join(accounts::ACCOUNTS_FILE), NOW), ResyncRead::Due(why) if why.contains("kimi-1")),
        "CONTROL: a listed pool needs no local roster"
    );
    crate::test_config::write(
        off.aterm_toml(),
        "[fleet]\nmachine = 'm16'\n[harness]\naccounts = 'off'\n",
    )
    .unwrap();
    let before = Bed::tree(off._dir.path());
    assert_eq!(
        resync_for_setup(
            &off.home,
            &off.state,
            &off.config.join(accounts::ACCOUNTS_FILE),
            NOW,
            false
        ),
        Resync::default(),
        "CONTROL: Off holds a listed pool without a local roster"
    );
    assert_eq!(Bed::tree(off._dir.path()), before);

    let other = Bed::new("pool-due-other");
    other.keep_roster(Cli::Kimi);
    other.secret(
        &cli_fleet_file(Cli::Kimi),
        "[[account]]\nlabel = \"kimi-22\"\nemail = \"kimi-other@example.com\"\n\
         machines = [\"m22\"]\nregion = \"global\"\n",
    );
    assert_eq!(
        read_resync(
            &other.home,
            &other.state,
            &other.config.join(accounts::ACCOUNTS_FILE),
            NOW
        ),
        ResyncRead::Current,
        "CONTROL: m22's row is not m16's"
    );
}

/// The converge reader and a default Codex sync make the same admission: an unlisted machine
/// or retained row stays out, while a new machine or unrelated retained label can join.
#[test]
fn a_default_pools_converge_read_matches_the_syncs_admission() {
    for (tag, machine, retained, admitted) in [
        ("unlisted", "m22", None, false),
        ("left-main", "m16", Some(ACCOUNTS_OWN_LABEL), false),
        ("left-fleet", "m16", Some("source-one"), false),
        ("retired", "m16", Some("retired"), true),
        ("new", "m16", None, true),
    ] {
        let bed = Bed::new(tag);
        bed.secret(
            &cli_fleet_file(Cli::Codex),
            &format!(
                "[[account]]\nlabel='source-one'\nemail='source-one@example.com'\nmachines=['{machine}']\n"
            ),
        );
        if let Some(label) = retained {
            let store = accounts::Store::at_for(&bed.state, Cli::Codex, &bed.home);
            let mut state = accounts::State::default();
            state
                .accounts
                .insert(label.into(), accounts::Account::default());
            store.save(&state).unwrap();
        }
        let before = Bed::tree(bed._dir.path());
        let want = if admitted {
            vec!["source-one".to_owned()]
        } else {
            Vec::new()
        };
        let combined = read_resync_with(
            &bed.home,
            &bed.state,
            &claude_roster(&bed),
            NOW,
            &Mutex::new(Rediscovery::default()),
            &read_local_source_text,
        );
        assert!(
            if admitted {
                matches!(combined, ResyncRead::Due(_))
            } else {
                combined == ResyncRead::NoSource
            },
            "{tag}: the combined read preserves admission: {combined:?}"
        );
        assert_eq!(
            Bed::tree(bed._dir.path()),
            before,
            "{tag}: reads write nothing"
        );
        match bed.sync(Cli::Codex, None).unwrap() {
            CliSync::Synced(synced) if admitted => {
                assert_eq!(synced.added, want);
                assert_eq!(
                    read_resync_with(
                        &bed.home,
                        &bed.state,
                        &claude_roster(&bed),
                        NOW,
                        &Mutex::new(Rediscovery::default()),
                        &read_local_source_text
                    ),
                    ResyncRead::Current,
                    "{tag}: the applied rows are no longer due"
                );
            }
            CliSync::Unlisted { .. } if tag == "unlisted" => {}
            CliSync::Left { label } if !admitted => assert_eq!(Some(label.as_str()), retained),
            outcome => panic!("{tag}: unexpected sync outcome {outcome:?}"),
        }
        if !admitted {
            assert_eq!(
                Bed::tree(bed._dir.path()),
                before,
                "{tag}: sync writes nothing"
            );
        }
    }
}

/// The actual sync attempts pending recovery before reading its fleet or default admission.
/// Even a missing source must expose an unreadable receipt instead of silently skipping it.
#[test]
fn a_default_pools_converge_read_reports_recovery_before_its_source_gate() {
    let bed = Bed::new("default-pool-recovery");
    let store = accounts::Store::at_for(&bed.state, Cli::Codex, &bed.home);
    fs::create_dir_all(&store.dir).unwrap();
    let receipt = store.dir.join("sync-commit.json");
    fs::write(&receipt, "{").unwrap();
    let before = Bed::tree(bed._dir.path());
    assert!(store.sync_needs_recovery());
    assert!(matches!(
        read_resync_with(
            &bed.home,
            &bed.state,
            &claude_roster(&bed),
            NOW,
            &Mutex::new(Rediscovery::default()),
            &|_| panic!("pending recovery is observed before any source is opened"),
        ),
        ResyncRead::Due(_)
    ));
    assert_eq!(
        Bed::tree(bed._dir.path()),
        before,
        "reads retain the receipt"
    );
    assert!(
        bed.sync(Cli::Codex, None)
            .unwrap_err()
            .contains("invalid account sync receipt")
    );
    assert_eq!(fs::read_to_string(receipt).unwrap(), "{");
    assert!(!bed.roster(Cli::Codex).exists());
    assert!(!bed.home.join(".aterm/logins/codex").exists());
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
    with_effects(&spawn, &fetch, |fx| {
        let _ = upkeep(
            &bed.home,
            &bed.state,
            (&bed.config.join(accounts::ACCOUNTS_FILE), NOW, step),
            fx,
        );
    });
    let n = queued.lock().unwrap().len();
    queued.lock().unwrap().clear();
    n
}

/// `run` over the window's effects in a test: renewals handed to `spawn`, a named source read
/// by `fetch`, no browser, no vault.
fn with_effects<R>(
    spawn: &SpawnRenewal<'_>,
    fetch: &Fetch<'_>,
    run: impl FnOnce(&UpkeepEffects<'_>) -> R,
) -> R {
    with_logged_effects(
        (spawn, fetch),
        (
            &Mutex::new(Rediscovery::default()),
            std::sync::Arc::new(|_: &str| {}),
        ),
        run,
    )
}

/// [`with_effects`] keeping what the upkeep remembers in `rediscovery` and logging to `log`.
fn with_logged_effects<R>(
    (spawn, fetch): (&SpawnRenewal<'_>, &Fetch<'_>),
    (rediscovery, log): (&Mutex<Rediscovery>, SignInLog),
    run: impl FnOnce(&UpkeepEffects<'_>) -> R,
) -> R {
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let cooldown = Mutex::new(None);
    let lifetime = crate::harness::account_work::Lifetime::default();
    let fx = UpkeepEffects {
        scope: lifetime.enter(),
        fetch,
        spawn_renewal: spawn,
        sync_cooldown: &cooldown,
        rediscovery,
        renew_guard: &guard,
        find_browser: &|_: &Path| false,
        vault_sync: |_: &Path, _scope| crate::harness::sign_in_ladder::Vault::NotMember,
        spawn_vault: &|_work| Ok(()),
        default_check: |_: &Path, _scope| {
            crate::harness::cli::LoginCheck::CouldNotRun(String::new())
        },
        mailbox: &ladder_mailbox_files,
        log,
        run_attempt: None,
        save_oauth: |_, _| Ok("inert".into()),
        shared: shared_renewal::Effects::default(),
    };
    run(&fx)
}

/// `[harness] accounts = "off"` over a home whose `~/.secrets` holds Claude Code's fleet and
/// Kimi's, and which keeps a roster of Kimi's; with `off` false, the same home without it.
fn accounts_bed(tag: &str, off: bool) -> Bed {
    let bed = Bed::new(tag);
    if off {
        crate::test_config::write(
            bed.aterm_toml(),
            "[fleet]\nmachine = \"m16\"\n[harness]\naccounts = \"off\"\n",
        )
        .unwrap();
    }
    bed.secret(FLEET_ROSTER_FILE, CLAUDE_FLEET);
    bed.secret(&cli_fleet_file(Cli::Kimi), KIMI_FLEET);
    bed.keep_roster(Cli::Kimi);
    bed
}

/// THE ACCOUNTS' OFF SWITCH: `[harness] accounts = "off"` turns every automatic sync of every
/// pool off, so the window's upkeep, the setup's resync, `sync_accounts_for_init` and
/// `sync_cli_accounts_for_init` read no source and write no roster, Claude Code's or Kimi's.
/// CONTROL: the same home without the switch syncs both pools at each of them.
#[test]
fn harness_accounts_off_turns_every_automatic_sync_off() {
    // What each caller left: Claude Code's roster written, and Kimi's holding rows.
    let synced = |bed: &Bed| {
        (
            bed.config.join(accounts::ACCOUNTS_FILE).is_file(),
            !fs::read_to_string(bed.roster(Cli::Kimi))
                .unwrap()
                .is_empty(),
        )
    };
    let roster_path = |bed: &Bed| bed.config.join(accounts::ACCOUNTS_FILE);
    let inert: &SpawnRenewal<'_> = &|_work| Ok(());
    for off in [true, false] {
        let want = (!off, !off);
        let label = if off { "off" } else { "CONTROL: on" };

        let bed = accounts_bed("off-upkeep", off);
        upkeep_over(&bed, UpkeepStep::Sync);
        assert_eq!(synced(&bed), want, "{label}: the window's upkeep");

        // A configured source: the upkeep syncs a missing roster from it on a path of its own.
        let bed = Bed::new("off-source");
        let sheet = bed.home.join("fleet.csv");
        fs::write(&sheet, "email,computer,claude\nm16@example.com,m16,1\n").unwrap();
        let switch = if off {
            "[harness]\naccounts = \"off\"\n"
        } else {
            ""
        };
        crate::test_config::write(
            bed.aterm_toml(),
            format!(
                "{switch}[fleet]\nmachine = \"m16\"\naccounts_source = {}\n",
                crate::harness::host_path::toml(&sheet)
            ),
        )
        .unwrap();
        let upkept = with_effects(inert, &read_named, |fx| {
            upkeep(
                &bed.home,
                &bed.state,
                (&roster_path(&bed), NOW, UpkeepStep::Sync),
                fx,
            )
        });
        assert_eq!(
            roster_path(&bed).is_file(),
            !off,
            "{label}: the upkeep from a configured source: {upkept:?}"
        );

        let bed = accounts_bed("off-resync", off);
        let resync = with_effects(inert, &read_named, |fx| {
            resync_with(&bed.home, &bed.state, (&roster_path(&bed), NOW, false), fx)
        });
        assert_eq!(
            synced(&bed),
            want,
            "{label}: the setup's resync: {resync:?}"
        );
        assert_eq!(resync.synced, !off, "{label}: {resync:?}");

        let bed = accounts_bed("off-init", off);
        let claude = sync_accounts_for_init(&bed.home, &bed.state, &roster_path(&bed), NOW);
        let others = sync_cli_accounts_for_init(&bed.home, &bed.state, &roster_path(&bed), NOW);
        assert_eq!(
            synced(&bed),
            want,
            "{label}: init's syncs: {claude:?} {others:?}"
        );
        if off {
            assert_eq!(claude.unwrap_err(), ACCOUNTS_SYNC_OFF);
            assert!(others.is_empty(), "{others:?}");
        } else {
            assert!(claude.is_ok(), "{claude:?}");
            assert!(
                matches!(
                    others.iter().find(|(cli, _)| *cli == Cli::Kimi),
                    Some((_, Ok(CliSync::Synced(_))))
                ),
                "{others:?}"
            );
        }
    }
}

/// Signs `label` of Kimi's pool in here (a placeholder login file) and marks it signed out, so
/// its renewal is due; a session here ran on it an hour before `now` and has ended, so this Mac
/// uses it ([`accounts::used_here`]).
fn signed_out_kimi_login(bed: &Bed, label: &str, now: i64) {
    let login = bed.home.join(".aterm/logins/kimi").join(label);
    fs::create_dir_all(login.join("credentials")).unwrap();
    fs::write(login.join("credentials/kimi-code.json"), "{}").unwrap();
    let store = accounts::Store::at_for(&bed.state, Cli::Kimi, &bed.home);
    let (mut state, _) = store.load(now).unwrap_or_default();
    let mut logins = accounts::Logins::default();
    let _ = accounts::note_login_end(&mut state, &mut logins, label, NOW);
    used_here(&mut state, label, now);
    store.save(&state).unwrap();
}

/// A session here ran on `label` an hour before `now` and has ended: this Mac uses its login
/// ([`accounts::used_here`]), so the upkeep may renew it when it is due.
fn used_here(state: &mut accounts::State, label: &str, now: i64) {
    let _ = accounts::note_session(state, &format!("ran-{label}"), label, None, now - 3600);
    state
        .sessions
        .retain(|sid, _| *sid != format!("ran-{label}"));
}

#[test]
fn a_vault_pool_imports_without_a_local_roster() {
    let bed = Bed::new("default-import");
    bed.secret(&cli_fleet_file(Cli::Kimi), KIMI_FLEET);
    let Ok(CliSync::Synced(synced)) = bed.sync(Cli::Kimi, None) else {
        panic!("the vault pool imports by default")
    };
    assert_eq!(synced.added, ["kimi-1"]);
    assert!(bed.roster(Cli::Kimi).is_file());
}

/// A KEPT ROSTER'S DUE LOGIN RENEWS UNLESS ITS SIGN-INS ARE OFF: the window renews the due login
/// of Kimi's roster, unless `[harness] logins = "off"` or `[harness] enabled = false` turns its
/// sign-ins off; Claude Code's own switch leaves them on
/// ([`each_pools_sign_ins_read_the_harness_and_its_own_switch`] holds the rule for every CLI).
#[test]
fn a_kept_pools_due_login_renews_unless_its_sign_ins_are_off() {
    let bed = Bed::new("renew-switches");
    bed.secret(&cli_fleet_file(Cli::Kimi), KIMI_FLEET);
    fake_headless_browser(&bed.home);
    bed.keep_roster(Cli::Kimi);
    let roster_path = bed.config.join(accounts::ACCOUNTS_FILE);
    let init = sync_cli_accounts_for_init(&bed.home, &bed.state, &roster_path, NOW);
    let Some((_, Ok(CliSync::Synced(synced)))) = init.iter().find(|(cli, _)| *cli == Cli::Kimi)
    else {
        panic!("a kept roster syncs at init: {init:?}");
    };
    assert_eq!(synced.added, ["kimi-1"]);
    signed_out_kimi_login(&bed, "kimi-1", NOW);
    assert_eq!(
        upkeep_over(&bed, UpkeepStep::Both),
        1,
        "CONTROL: the window renews the due login of a kept roster"
    );
    let pools = cli_pools(&bed.aterm_toml(), &bed.state, &bed.home, NOW);
    assert_eq!(cli_pools_words(&pools).as_deref(), Some("kimi 2"));
    crate::test_config::write(
        bed.aterm_toml(),
        "[fleet]\nmachine = \"m16\"\n[harness]\nlogins = \"off\"\n",
    )
    .unwrap();
    assert_eq!(
        upkeep_over(&bed, UpkeepStep::Both),
        0,
        "`[harness] logins = \"off\"` stops the background renewal"
    );
    crate::test_config::write(
        bed.aterm_toml(),
        "[fleet]\nmachine = \"m16\"\n[harness]\nenabled = false\n",
    )
    .unwrap();
    assert_eq!(
        upkeep_over(&bed, UpkeepStep::Both),
        0,
        "`[harness] enabled = false` stops it too, as it stops the host's"
    );
    crate::test_config::write(
        bed.aterm_toml(),
        "[fleet]\nmachine = \"m16\"\n[harness.claude]\nenabled = false\n",
    )
    .unwrap();
    assert_eq!(
        upkeep_over(&bed, UpkeepStep::Both),
        1,
        "CONTROL: Claude Code switched off leaves Kimi Code's renewal on"
    );
}

/// EACH POOL'S SIGN-INS READ THE HARNESS AND ITS OWN SWITCH: the automatic sign-in of a CLI's
/// rows is off where `[harness] accounts = "off"`, `[harness] logins = "off"` or
/// `[harness] enabled = false`, and where that CLI's own `[harness.<cli>] enabled = false`
/// (Claude Code's, Codex's and agy's; Grok Build and Kimi Code have no table of their own), never
/// for another CLI's, exactly as the window reads it (`SupervisorConfig::logins_for` and
/// `accounts_for`). The setting `aterm ai` names for a CLI's rows ([`sign_ins_off_for`]) is the one
/// in aterm.toml that keeps them off, as it is written there: its own table's, the swarm switch,
/// the harness written off itself, a `harness` key below another table's header, and a value
/// other than `"off"` that reads as off included; the gates every pool shares
/// ([`harness_sign_ins_on`]) read no CLI's table. NEGATIVE CONTROLS: the rule that read the
/// harness-wide switches alone kept Codex's sign-ins on under `[harness.codex] enabled = false`,
/// and Claude Code's under `[harness.claude] enabled = false`, and named no switch for either; the
/// rule that read only those two CLIs' tables kept agy's on under `[harness.agy] enabled = false`,
/// which the window reads as agy's logins off; the
/// rule that named a switch by how it was read named `[harness] accounts = "off"` for the swarm
/// switch and a misplaced `harness.accounts`, `[harness] enabled = false` for `harness = false`,
/// and `[harness] logins = "off"` for `logins = false`, none of which aterm.toml says.
#[test]
fn each_pools_sign_ins_read_the_harness_and_its_own_switch() {
    let bed = Bed::new("sign-ins-switches");
    let all = [Cli::Claude, Cli::Codex, Cli::Grok, Cli::Kimi, Cli::Agy];
    for (config, off, said) in [
        ("", &[][..], None),
        (
            "[harness]\naccounts = \"off\"\n",
            &all[..],
            Some("`[harness] accounts = \"off\"`"),
        ),
        (
            "[harness]\nlogins = \"off\"\n",
            &all[..],
            Some("`[harness] logins = \"off\"`"),
        ),
        (
            "[harness]\nenabled = false\n",
            &all[..],
            Some("`[harness] enabled = false`"),
        ),
        (
            "[harness.codex]\nenabled = false\n",
            &[Cli::Codex][..],
            Some("`[harness.codex] enabled = false`"),
        ),
        (
            "[harness.claude]\nenabled = false\n",
            &[Cli::Claude][..],
            Some("`[harness.claude] enabled = false`"),
        ),
        (
            "[harness.agy]\nenabled = false\n",
            &[Cli::Agy][..],
            Some("`[harness.agy] enabled = false`"),
        ),
        (
            "[swarm]\nenabled = false\n",
            &all[..],
            Some("`[swarm] enabled = false`"),
        ),
        ("swarm = false\n", &all[..], Some("`swarm = false`")),
        ("harness = false\n", &all[..], Some("`harness = false`")),
        (
            "[harness]\nlogins = false\n",
            &all[..],
            Some("`[harness] logins = false`"),
        ),
        (
            "[theme]\nharness.accounts = \"off\"\n",
            &all[..],
            Some(
                "`theme.harness.accounts = \"off\"` (a `harness.accounts` below another table's \
                 header)",
            ),
        ),
        (
            "[theme]\nharness.codex.enabled = false\n",
            &[Cli::Codex][..],
            Some(
                "`theme.harness.codex.enabled = false` (a `harness.codex.enabled` below another \
                 table's header)",
            ),
        ),
        // The master switch outranks rotation's, which outranks the CLI's own.
        (
            "[harness]\nenabled = false\naccounts = \"off\"\n[harness.codex]\nenabled = false\n",
            &all[..],
            Some("`[harness] enabled = false`"),
        ),
        (
            "[harness]\naccounts = \"off\"\n[harness.codex]\nenabled = false\n",
            &all[..],
            Some("`[harness] accounts = \"off\"`"),
        ),
    ] {
        crate::test_config::write(bed.aterm_toml(), config).unwrap();
        let (read, _) = crate::supervise::config::SupervisorConfig::from_aterm_toml(config);
        for cli in all {
            let held = off.contains(&cli);
            assert_eq!(
                sign_ins_on(&bed.aterm_toml(), cli),
                !held,
                "{config}{cli:?}"
            );
            // The window's reading of the same switches agrees.
            assert_eq!(
                read.logins_for(cli.program())
                    && read.accounts_for(cli.program()) != accounts::Mode::Off,
                !held,
                "{config}{cli:?}: the window's reading"
            );
            assert_eq!(
                sign_ins_off_for(&bed.aterm_toml(), cli),
                said.filter(|_| held).map(str::to_string),
                "{config}{cli:?}: the switch named"
            );
        }
        assert_eq!(
            harness_sign_ins_on(&bed.aterm_toml()),
            off.len() != all.len(),
            "{config}: the harness-wide switches"
        );
    }
    // An aterm.toml that is not valid TOML, whose lines are read one by one, says so, and where
    // the parser stopped.
    crate::test_config::write(bed.aterm_toml(), "[harness]\naccounts = \"off\"\n[broken\n")
        .unwrap();
    for cli in all {
        assert!(!sign_ins_on(&bed.aterm_toml(), cli), "{cli:?}");
        let said = sign_ins_off_for(&bed.aterm_toml(), cli).expect("named");
        assert!(
            said.starts_with("an aterm.toml that is not valid TOML (TOML error at line 3, column ")
                && said.ends_with(") and is read line by line"),
            "{cli:?}: {said}"
        );
    }
    // CONTROL: the same lines read as valid TOML name the setting.
    crate::test_config::write(bed.aterm_toml(), "[harness]\naccounts = \"off\"\n").unwrap();
    assert_eq!(
        sign_ins_off_for(&bed.aterm_toml(), Cli::Claude).as_deref(),
        Some("`[harness] accounts = \"off\"`")
    );
    // An aterm.toml that cannot be read turns the harness off, and says so, not a switch it
    // does not hold (NEGATIVE CONTROL: the rule that named the reading said
    // `[harness] enabled = false`).
    fs::remove_file(bed.aterm_toml()).unwrap();
    fs::create_dir(bed.aterm_toml()).unwrap();
    assert!(!harness_sign_ins_on(&bed.aterm_toml()));
    for cli in all {
        assert!(!sign_ins_on(&bed.aterm_toml(), cli), "{cli:?}");
        let said = sign_ins_off_for(&bed.aterm_toml(), cli).expect("named");
        assert!(
            said.starts_with("an aterm.toml that cannot be read (") && said.ends_with(')'),
            "{cli:?}: {said}"
        );
    }
    fs::remove_dir(bed.aterm_toml()).unwrap();
    // No aterm.toml is the defaults: nothing keeps sign-ins off.
    for cli in all {
        assert!(sign_ins_on(&bed.aterm_toml(), cli), "{cli:?}");
        assert_eq!(sign_ins_off_for(&bed.aterm_toml(), cli), None, "{cli:?}");
    }
}

/// The roster `aterm.toml` stands beside in a bed: the path init, the upkeep and the resync take.
fn claude_roster(bed: &Bed) -> PathBuf {
    bed.config.join(accounts::ACCOUNTS_FILE)
}

/// The setup pass's resync over `bed`, renewals inert and no fetch.
fn resync_over(bed: &Bed) -> Resync {
    with_effects(&|_work| Ok(()), &read_named, |fx| {
        resync_with(&bed.home, &bed.state, (&claude_roster(bed), NOW, false), fx)
    })
}

/// Codex's rows of `bed`'s roster, by label.
fn codex_labels(bed: &Bed) -> Vec<String> {
    bed.rows(Cli::Codex).into_iter().map(|r| r.label).collect()
}

/// POOLS SYNC BY DEFAULT (`DESIGN-alab-swarm-2026-10-27.md` C15): with the vault's sheet in
/// `~/.secrets` and no `accounts-codex.toml` here, init's sync, the window's upkeep and the
/// setup's resync each take this machine's Codex rows into a roster they write, this machine's
/// own login (`main`) first, each fleet row with an empty login directory of its own (no login
/// is copied), and write nothing under `~/.secrets`. Grok Build and Kimi import their rows too.
#[test]
fn codex_rows_sync_by_default_with_no_roster_file() {
    let want = [ACCOUNTS_OWN_LABEL, "codex-m16", "codex-m16-2"];

    let bed = Bed::new("codex-default-init");
    bed.secret(ACCOUNTS_CSV_FILE, SHEET);
    let secrets = Bed::tree(&bed.secrets());
    assert!(!bed.roster(Cli::Codex).exists());
    let due = || cli_pools_due(&bed.aterm_toml(), &bed.state, &bed.home, NOW);
    assert_eq!(
        due(),
        [Cli::Codex, Cli::Grok, Cli::Kimi],
        "init's probe sees every pool due"
    );
    let init = sync_cli_accounts_for_init(&bed.home, &bed.state, &claude_roster(&bed), NOW);
    let Some((_, Ok(CliSync::Synced(synced)))) = init.iter().find(|(cli, _)| *cli == Cli::Codex)
    else {
        panic!("Codex syncs with no roster file: {init:?}");
    };
    assert_eq!(synced.machine, "m16");
    assert_eq!(synced.added, ["codex-m16", "codex-m16-2"]);
    assert_eq!(codex_labels(&bed), want);
    assert!(due().is_empty(), "a roster that stands is no longer due");
    assert_eq!(
        bed.rows(Cli::Codex)[0].dir,
        None,
        "`main` is this Mac's own login"
    );
    for label in ["codex-m16", "codex-m16-2"] {
        let dir = bed.home.join(".aterm/logins/codex").join(label);
        assert_eq!(
            fs::read_dir(&dir).map(Iterator::count).ok(),
            Some(0),
            "{label}'s login directory is made empty"
        );
    }
    assert_eq!(
        Bed::tree(&bed.secrets()),
        secrets,
        "~/.secrets is unchanged"
    );
    for cli in [Cli::Grok, Cli::Kimi] {
        assert!(
            matches!(
                init.iter().find(|(c, _)| *c == cli),
                Some((_, Ok(CliSync::Synced(_))))
            ),
            "{cli:?} also imports without a roster: {init:?}"
        );
        assert!(bed.roster(cli).is_file());
    }

    let bed = Bed::new("codex-default-upkeep");
    bed.secret(ACCOUNTS_CSV_FILE, SHEET);
    upkeep_over(&bed, UpkeepStep::Sync);
    assert_eq!(codex_labels(&bed), want, "the window's upkeep");

    let bed = Bed::new("codex-default-resync");
    bed.secret(ACCOUNTS_CSV_FILE, SHEET);
    let resync = resync_over(&bed);
    assert!(resync.cli_failed.is_empty(), "{resync:?}");
    assert_eq!(codex_labels(&bed), want, "the setup's resync");
    assert!(syncs_by_default(Cli::Codex));
    assert!(syncs_by_default(Cli::Grok) && syncs_by_default(Cli::Kimi));
}

/// THE OFF SWITCH STILL HOLDS CODEX: under `[harness] accounts = "off"`, init's sync, the
/// window's upkeep and the setup's resync take no Codex row from the sheet that gives m16 two:
/// no roster, no store, no login directory. A person's own `aterm harness accounts sync --cli
/// codex` still syncs. CONTROL: the same home without the switch syncs
/// ([`codex_rows_sync_by_default_with_no_roster_file`]).
#[test]
fn harness_accounts_off_still_stops_codex_rows_syncing_by_default() {
    let off = |tag: &str| {
        let bed = Bed::new(tag);
        crate::test_config::write(
            bed.aterm_toml(),
            "[fleet]\nmachine = \"m16\"\n[harness]\naccounts = \"off\"\n",
        )
        .unwrap();
        bed.secret(ACCOUNTS_CSV_FILE, SHEET);
        bed
    };
    let untouched = |bed: &Bed, by: &str| {
        assert!(!bed.roster(Cli::Codex).exists(), "{by}: no Codex roster");
        assert!(
            !accounts::Store::at_for(&bed.state, Cli::Codex, &bed.home)
                .dir
                .exists(),
            "{by}: no Codex store"
        );
        assert!(
            !bed.home.join(".aterm/logins/codex").exists(),
            "{by}: no login directory"
        );
    };

    let bed = off("codex-off-init");
    let init = sync_cli_accounts_for_init(&bed.home, &bed.state, &claude_roster(&bed), NOW);
    assert!(init.is_empty(), "{init:?}");
    untouched(&bed, "init");

    let bed = off("codex-off-upkeep");
    assert_eq!(upkeep_over(&bed, UpkeepStep::Both), 0);
    untouched(&bed, "the window's upkeep");

    let bed = off("codex-off-resync");
    let resync = resync_over(&bed);
    assert!(!resync.synced && resync.cli_failed.is_empty(), "{resync:?}");
    untouched(&bed, "the setup's resync");

    let Ok(CliSync::Synced(synced)) = bed.sync(Cli::Codex, None) else {
        panic!("a person's own sync still syncs under the switch");
    };
    assert_eq!(synced.added, ["codex-m16", "codex-m16-2"]);
}

/// A MAC THE FLEET DOES NOT LIST takes nothing by default: Codex's sync reads the sheet and
/// makes no roster, store, lock or login directory ([`CliSync::Unlisted`]), at init or in the
/// window's upkeep, and a sheet with no Codex column is no source ([`CliSync::NoSource`]).
/// CONTROL: the same sheet for m16 writes its roster
/// ([`codex_rows_sync_by_default_with_no_roster_file`]).
#[test]
fn codex_by_default_writes_nothing_where_the_fleet_lists_no_row_of_this_macs() {
    let bed = Bed::new("codex-unlisted");
    crate::test_config::write(bed.aterm_toml(), "[fleet]\nmachine = \"m9\"\n").unwrap();
    bed.secret(ACCOUNTS_CSV_FILE, SHEET);
    let before = Bed::tree(bed._dir.path());
    let unlisted = bed.sync(Cli::Codex, None);
    assert!(
        matches!(&unlisted, Ok(CliSync::Unlisted { machine: Some(m), source, conflicts })
            if m == "m9" && source.ends_with(ACCOUNTS_CSV_FILE) && conflicts.is_empty()),
        "{unlisted:?}"
    );
    let init = sync_cli_accounts_for_init(&bed.home, &bed.state, &claude_roster(&bed), NOW);
    assert!(
        matches!(
            init.iter().find(|(cli, _)| *cli == Cli::Codex),
            Some((_, Ok(CliSync::Unlisted { .. })))
        ),
        "{init:?}"
    );
    assert_eq!(Bed::tree(bed._dir.path()), before, "nothing was written");
    assert!(
        cli_pools_due(&bed.aterm_toml(), &bed.state, &bed.home, NOW).is_empty(),
        "nothing is due for init's probe"
    );
    upkeep_over(&bed, UpkeepStep::Both);
    assert!(!bed.roster(Cli::Codex).exists());
    assert!(
        !accounts::Store::at_for(&bed.state, Cli::Codex, &bed.home)
            .dir
            .exists()
    );
    assert!(!bed.home.join(".aterm/logins/codex").exists());

    let bed = Bed::new("codex-no-column");
    bed.secret(
        ACCOUNTS_CSV_FILE,
        "email,computer,claude\none@example.com,m16,1\n",
    );
    assert_eq!(bed.sync(Cli::Codex, None), Ok(CliSync::NoSource));
    assert!(!bed.roster(Cli::Codex).exists());
}

/// THE QUIET RULE: once the window's upkeep found the fleet lists no Codex row of this
/// machine's, it does not read the same sheet and config again (nor ask the host's name) until
/// [`REDISCOVER_EVERY`] passes. CONTROL: a changed input (this machine named m16, which the sheet
/// lists, in an `aterm.toml` kept as a link, as dotfiles keep it) syncs at the next upkeep, and
/// the hold is forgotten once rows are written.
#[cfg(unix)]
#[test]
fn an_unlisted_mac_syncs_codex_again_only_once_its_inputs_change() {
    let bed = Bed::new("codex-unlisted-quiet");
    let dotfile = bed.home.join("dotfiles-aterm.toml");
    aterm_uds::atomic_file::write(&dotfile, b"[fleet]\nmachine = \"m9\"\n", 0o600).unwrap();
    fs::remove_file(bed.aterm_toml()).unwrap();
    std::os::unix::fs::symlink(&dotfile, bed.aterm_toml()).unwrap();
    bed.secret(ACCOUNTS_CSV_FILE, SHEET);
    let inputs = || cli_sync_inputs(Cli::Codex, &bed.home, &bed.aterm_toml());
    with_effects(&|_work| Ok(()), &read_named, |fx| {
        let upkeep_pools_now = || {
            upkeep_pools(
                &bed.home,
                &bed.state,
                (&claude_roster(&bed), NOW, UpkeepStep::Sync),
                fx,
            );
        };
        assert!(!default_sync_holds(fx.rediscovery, Cli::Codex, &inputs()));
        upkeep_pools_now();
        assert!(!bed.roster(Cli::Codex).exists());
        assert!(
            default_sync_holds(fx.rediscovery, Cli::Codex, &inputs()),
            "the same inputs are held back"
        );

        fs::write(&dotfile, "[fleet]\nmachine = \"m16\"\n").unwrap();
        assert!(
            !default_sync_holds(fx.rediscovery, Cli::Codex, &inputs()),
            "CONTROL: changed inputs are not held"
        );
        upkeep_pools_now();
        assert_eq!(
            codex_labels(&bed),
            [ACCOUNTS_OWN_LABEL, "codex-m16", "codex-m16-2"]
        );
        assert!(
            !fx.rediscovery
                .lock()
                .unwrap()
                .default_held
                .contains_key(&Cli::Codex),
            "a sync that wrote rows forgets the hold"
        );
    });

    let pool_bed = Bed::new("codex-unlisted-ai-pool-quiet");
    crate::test_config::write(pool_bed.aterm_toml(), "[fleet]\nmachine = \"m9\"\n").unwrap();
    pool_bed.secret(
        super::super::ai_pool::AI_POOL_FILE,
        "version = 1\n\n[[account]]\nprovider = \"codex\"\nemail = \"codex-one@example.com\"\nwarm = [\"m16\"]\n",
    );
    let pool_inputs = || cli_sync_inputs(Cli::Codex, &pool_bed.home, &pool_bed.aterm_toml());
    with_effects(&|_work| Ok(()), &read_named, |fx| {
        let upkeep_pools_now = || {
            upkeep_pools(
                &pool_bed.home,
                &pool_bed.state,
                (&claude_roster(&pool_bed), NOW, UpkeepStep::Sync),
                fx,
            );
        };
        upkeep_pools_now();
        assert!(!pool_bed.roster(Cli::Codex).exists());
        assert!(default_sync_holds(
            fx.rediscovery,
            Cli::Codex,
            &pool_inputs()
        ));

        pool_bed.secret(
            super::super::ai_pool::AI_POOL_FILE,
            "version = 1\n\n[[account]]\nprovider = \"codex\"\nemail = \"codex-one@example.com\"\nwarm = [\"m9\"]\n",
        );
        assert!(
            !default_sync_holds(fx.rediscovery, Cli::Codex, &pool_inputs()),
            "ai-pool.toml edits lift the default sync hold"
        );
        upkeep_pools_now();
        assert_eq!(codex_labels(&pool_bed), [ACCOUNTS_OWN_LABEL, "codex-m9"]);
    });
}

/// A MAC THAT LEFT CODEX'S POOL stays out: one that kept `accounts-codex.toml` and removed it,
/// its store still holding the records of a row the sync would add (`main`, or the sheet's
/// `codex-m16`), takes nothing by default ([`CliSync::Left`], naming that row) where the sync
/// would otherwise refuse at every pass, writes nothing, is not due for init's probe, and the
/// window's upkeep holds it. CONTROL: the records of a row the sync would not add (`retired`)
/// leave the default sync free.
#[test]
fn a_mac_that_removed_its_codex_roster_stays_out_of_the_pool() {
    for (label, left) in [
        (ACCOUNTS_OWN_LABEL, true),
        ("codex-m16", true),
        ("retired", false),
    ] {
        let bed = Bed::new("codex-left");
        bed.secret(ACCOUNTS_CSV_FILE, SHEET);
        let store = accounts::Store::at_for(&bed.state, Cli::Codex, &bed.home);
        let mut state = accounts::State::default();
        state
            .accounts
            .insert(label.into(), accounts::Account::default());
        store.save(&state).unwrap();
        let before = Bed::tree(&store.dir);
        let due = cli_pools_due(&bed.aterm_toml(), &bed.state, &bed.home, NOW);
        let init = sync_cli_accounts_for_init(&bed.home, &bed.state, &claude_roster(&bed), NOW);
        let codex = init.iter().find(|(cli, _)| *cli == Cli::Codex);
        if left {
            assert!(!due.contains(&Cli::Codex), "{label}: {due:?}");
            assert!(
                matches!(codex, Some((_, Ok(CliSync::Left { label: held }))) if held == label),
                "{label}: {init:?}"
            );
            assert!(!bed.roster(Cli::Codex).exists());
            assert_eq!(
                Bed::tree(&store.dir),
                before,
                "{label}: Codex's store is unchanged"
            );
            with_effects(&|_work| Ok(()), &read_named, |fx| {
                upkeep_pools(
                    &bed.home,
                    &bed.state,
                    (&claude_roster(&bed), NOW, UpkeepStep::Sync),
                    fx,
                );
                let inputs = cli_sync_inputs(Cli::Codex, &bed.home, &bed.aterm_toml());
                assert!(default_sync_holds(fx.rediscovery, Cli::Codex, &inputs));
            });
            assert!(!bed.roster(Cli::Codex).exists());
        } else {
            assert_eq!(due, [Cli::Codex, Cli::Grok, Cli::Kimi], "CONTROL: {label}");
            assert!(
                matches!(codex, Some((_, Ok(CliSync::Synced(_))))),
                "CONTROL: {label}: {init:?}"
            );
            assert_eq!(
                codex_labels(&bed),
                [ACCOUNTS_OWN_LABEL, "codex-m16", "codex-m16-2"]
            );
        }
    }
}

/// `accounts sync --cli` names the sign-in of each row it added by the verb that signs it in
/// (`Cli::sign_in_verb`): a Kimi row's renewal, once a person has opened its browser, and a Grok
/// row's `accounts login`, since its renewals stop at accounts.x.ai's block page.
#[test]
fn a_sync_names_each_added_rows_sign_in_by_its_verb() {
    const GROK_FLEET: &str = "\
[[account]]
label = \"grok-1\"
email = \"grok-one@example.com\"
machines = [\"m16\"]
";
    for (cli, fleet, step) in [
        (
            Cli::Kimi,
            KIMI_FLEET,
            "`aterm harness accounts renew kimi-1 --cli kimi`",
        ),
        (
            Cli::Grok,
            GROK_FLEET,
            "`aterm harness accounts login <label> --cli grok`",
        ),
    ] {
        let bed = Bed::new("sync-step");
        bed.keep_roster(cli);
        bed.secret(&cli_fleet_file(cli), fleet);
        let env = quiet_env(&bed.home, &bed.state, &bed.aterm_toml(), NOW);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code =
            run_cli_accounts_sync(cli, (&env, None), (None, None, false), &mut out, &mut err);
        let (out, err) = (
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        );
        assert_eq!(code, ExitCode::SUCCESS, "{cli:?}: {err}");
        assert!(out.contains(step), "{cli:?}: {out}");
        assert!(!out.contains("by itself"), "{cli:?}: {out}");
        assert!(
            out.contains(if cli == Cli::Kimi { "kimi-1" } else { "grok-1" }),
            "{cli:?}: {out}"
        );
        assert_eq!(
            out.contains("accounts renew"),
            cli == Cli::Kimi,
            "{cli:?}: {out}"
        );
    }
}

/// THE DASHBOARD shows each pool this machine keeps in a card of its own: its product, its rows
/// with their region and the `--cli` verb that signs each in (`Cli::sign_in_verb`): a Kimi row's
/// renewal, a Grok row's `login` (its renewals stop at accounts.x.ai's block page), and `login`
/// for a row with no email, which no renewal signs in. CONTROL: a machine that keeps none gets no
/// card, so a Claude-Code-only dashboard is the page it was.
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
    fs::write(
        bed.roster(Cli::Grok),
        at!(
            "[[account]]\nlabel = \"grok-1\"\nclass = \"dedicated\"\n\
             email = \"grok-1@example.com\"\n\n\
             [[account]]\nlabel = \"grok-2\"\nclass = \"dedicated\"\ndir = \"",
            "/x/grok-2",
            "\"\n"
        ),
    )
    .unwrap();
    let html = pools_html(
        &cli_pools(&bed.aterm_toml(), &bed.state, &bed.home, NOW),
        NOW,
    );
    assert!(
        html.contains("aterm harness accounts login grok-1 --cli grok")
            && html.contains("aterm harness accounts login grok-2 --cli grok")
            && !html.contains("renew grok-"),
        "{html}"
    );
    // A Kimi row with no email: `login`, beside its emailed row's renewal.
    let mut kimi = fs::read_to_string(bed.roster(Cli::Kimi)).unwrap();
    kimi.push_str(at!(
        "\n[[account]]\nlabel = \"kimi-2\"\nclass = \"dedicated\"\ndir = \"",
        "/x/kimi-2",
        "\"\n"
    ));
    fs::write(bed.roster(Cli::Kimi), kimi).unwrap();
    let html = pools_html(
        &cli_pools(&bed.aterm_toml(), &bed.state, &bed.home, NOW),
        NOW,
    );
    assert!(
        html.contains("aterm harness accounts renew kimi-1 --cli kimi")
            && html.contains("aterm harness accounts login kimi-2 --cli kimi")
            && !html.contains("renew kimi-2"),
        "{html}"
    );
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
    renew(at!("/old")).unwrap();
    let witness = store.state_witness_file();
    let retained = witness.with_extension("retained");
    fs::rename(&witness, &retained).unwrap();
    fs::create_dir(&witness).unwrap();
    assert!(renew(at!("/new")).is_err());
    assert!(store.load(NOW).is_err(), "publication was admitted");
    fs::rename(&witness, witness.with_extension("obstruction")).unwrap();
    fs::rename(&retained, &witness).unwrap();

    let pools = cli_pools(&bed.aterm_toml(), &bed.state, &bed.home, NOW);
    assert_eq!(cli_pools_words(&pools).as_deref(), Some("kimi 1"));
    assert_eq!(
        pools[0].rows.as_ref().unwrap().rows[0].dir.as_deref(),
        Some(at!("/new"))
    );
    assert_eq!(pools[0].state.account("main").auth, accounts::Auth::Ok);
}

/// NO GROK ROW IS RENEWED IN THE BACKGROUND: accounts.x.ai answers aterm's headless browser
/// with its block page (`Cli::renews_headlessly` is false for Grok), so each background renewal
/// would request a live xAI device code and then load a page that is blocked, every
/// `PERSON_BACKOFF`. A Grok row with an email, a headless browser installed, is
/// `RenewalDispatch::Failed` and nothing is handed off or attributed; a saved selection of one
/// (a menu click) is cancelled by the window's upkeep, which starts nothing for it. A person's
/// `accounts login <label> --cli grok` signs it in. CONTROL: a Kimi row with an email, the same
/// headless browser installed, still starts, directly and from its saved selection, and a Kimi
/// row with no email starts nothing.
#[test]
fn a_grok_rows_background_renewal_hands_off_nothing_and_a_kimi_rows_still_starts() {
    const GROK_ROSTER: &str = at!(
        "[[account]]\nlabel = \"grok-1\"\nclass = \"dedicated\"\n\
         email = \"grok-one@example.com\"\ndir = \"",
        "/x/grok-1",
        "\"\n"
    );
    let kimi_roster = format!(
        "[[account]]\nlabel = \"kimi-1\"\nclass = \"dedicated\"\n\
         email = \"kimi-one@example.com\"\nregion = \"global\"\ndir = \"{}\"\n\n\
         [[account]]\nlabel = \"kimi-2\"\nclass = \"dedicated\"\ndir = \"{}\"\n",
        at!("/x/kimi-1"),
        at!("/x/kimi-2")
    );
    let bed = Bed::new("grok-renewal");
    fake_headless_browser(&bed.home);
    let grok = Roster::parse_for(Cli::Grok, GROK_ROSTER).unwrap();
    let kimi = Roster::parse_for(Cli::Kimi, &kimi_roster).unwrap();
    fs::write(bed.roster(Cli::Grok), GROK_ROSTER).unwrap();
    fs::write(bed.roster(Cli::Kimi), &kimi_roster).unwrap();
    let handed = Arc::new(Mutex::new(0usize));
    let count = Arc::clone(&handed);
    let spawn = move |_work: Box<dyn FnOnce() + Send>| {
        *count.lock().unwrap() += 1;
        Ok(())
    };
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let logins = accounts::Logins::default();
    let renew = |cli: Cli, roster: &Roster, label: &str| {
        spawn_vendor_renewal(
            cli,
            &bed.home,
            &bed.state,
            &bed.roster(cli),
            roster,
            &logins,
            roster.row(label).cloned().unwrap(),
            NOW,
            &guard,
            &spawn,
        )
    };
    assert_eq!(renew(Cli::Grok, &grok, "grok-1"), RenewalDispatch::Failed);
    assert_eq!(*handed.lock().unwrap(), 0, "no Grok renewal is handed off");
    assert!(
        guard.lock().unwrap().activity_owners.is_empty(),
        "no Grok row is attributed a background sign-in"
    );
    assert_eq!(
        renew(Cli::Kimi, &kimi, "kimi-1"),
        RenewalDispatch::Started,
        "CONTROL: a Kimi row with an email still starts"
    );
    assert_eq!(*handed.lock().unwrap(), 1);
    assert_eq!(
        renew(Cli::Kimi, &kimi, "kimi-2"),
        RenewalDispatch::Started,
        "an email-less Kimi device flow starts too"
    );
    assert_eq!(*handed.lock().unwrap(), 2);

    // A saved selection of each, as a menu click leaves it, served by the window's upkeep.
    #[cfg(unix)]
    {
        let bed = Bed::new("grok-selection");
        fake_headless_browser(&bed.home);
        fs::write(bed.roster(Cli::Grok), GROK_ROSTER).unwrap();
        fs::write(bed.roster(Cli::Kimi), &kimi_roster).unwrap();
        let pending = |cli: Cli| {
            let store = accounts::Store::at_for(&bed.state, cli, &bed.home);
            store.load(NOW).unwrap().0.pending_pin
        };
        for (cli, label) in [(Cli::Grok, "grok-1"), (Cli::Kimi, "kimi-1")] {
            accounts::Store::at_for(&bed.state, cli, &bed.home)
                .request_pin(label, NOW)
                .unwrap();
        }
        let manual_workers = std::sync::atomic::AtomicUsize::new(0);
        let run_grok = |work: Box<dyn FnOnce() + Send>| {
            manual_workers.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            work(); // Grok refuses before any provider handoff.
            Ok(())
        };
        let fetch = |_: &Env, _: &str| -> Result<String, String> { panic!("no source read") };
        with_effects(&run_grok, &fetch, |fx| {
            assert_eq!(
                resume_pending_pin_for(
                    Cli::Grok,
                    &bed.home,
                    &bed.state,
                    &bed.roster(Cli::Grok),
                    NOW,
                    fx
                ),
                Some(RenewalDispatch::Started),
                "the detached manual owner, not a provider, was started"
            );
            assert!(!fx.renew_guard.lock().unwrap().in_flight);
        });
        assert_eq!(manual_workers.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(pending(Cli::Grok), None, "the Grok selection is cancelled");
        let retain_kimi = |_: Box<dyn FnOnce() + Send>| {
            manual_workers.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(()) // Drop this separate manual owner unstarted; no real sign-in.
        };
        with_effects(&retain_kimi, &fetch, |fx| {
            assert_eq!(
                resume_pending_pin_for(
                    Cli::Kimi,
                    &bed.home,
                    &bed.state,
                    &bed.roster(Cli::Kimi),
                    NOW,
                    fx
                ),
                Some(RenewalDispatch::Started)
            );
            assert!(!fx.renew_guard.lock().unwrap().in_flight);
        });
        assert_eq!(
            manual_workers.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "two owners are distinct from the provider handoffs above"
        );
        assert!(
            pending(Cli::Kimi).is_some(),
            "CONTROL: the Kimi selection waits on its renewal"
        );
    }
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
            NOW,
            shared_renewal::CheckpointAuthority::Unavailable,
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
            NOW,
            shared_renewal::CheckpointAuthority::Unavailable,
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
    // The mailbox update below follows wrong_at and still precedes the real read cutoff.
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap()
        - 1_000;
    assert!(matches!(bed.sync(Cli::Kimi, None), Ok(CliSync::Synced(_))));
    // Initial input precedes both wrong-account marks; only the later mailbox is new.
    let initial_time =
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(u64::try_from(now - 180).unwrap());
    fs::File::options()
        .write(true)
        .open(bed.secrets().join(cli_fleet_file(Cli::Kimi)))
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(initial_time))
        .unwrap();
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
    let empty_logins = store.load_logins(now).unwrap().0;
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
    store
        .transact_pool_logins_within(
            &[],
            &bed.roster(Cli::Kimi),
            now,
            &mut None,
            |raw, _, logins| {
                assert_eq!(raw, &roster);
                Ok(accounts::note_wrong_account(logins, "kimi-1", now - 60))
            },
        )
        .unwrap();
    let wrong_logins = store.load_logins(now).unwrap().0;
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
        signed_out_kimi_login(&bed, label, now);
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
        scope: None,
        fetch: &fetch,
        spawn_renewal: &spawn,
        sync_cooldown: &cooldown,
        rediscovery: &Mutex::new(Rediscovery::default()),
        renew_guard: &fresh_guard,
        find_browser: &|_: &Path| false,
        vault_sync: |_: &Path, _scope| crate::harness::sign_in_ladder::Vault::NotMember,
        spawn_vault: &|_work| Ok(()),
        default_check: |_: &Path, _scope| {
            crate::harness::cli::LoginCheck::CouldNotRun(String::new())
        },
        mailbox: &ladder_mailbox_files,
        log: std::sync::Arc::new(|_: &str| {}),
        run_attempt: None,
        save_oauth: |_, _| Ok("inert".into()),
        shared: shared_renewal::Effects::default(),
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
    signed_out_kimi_login(&bed, "kimi-1", now);
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
            scope: None,
            fetch: &fetch,
            spawn_renewal: &spawn,
            sync_cooldown: &cooldown,
            rediscovery: &Mutex::new(Rediscovery::default()),
            renew_guard: &guard,
            find_browser: &|_: &Path| false,
            vault_sync: |_: &Path, _scope| crate::harness::sign_in_ladder::Vault::NotMember,
            spawn_vault: &|_work| Ok(()),
            default_check: |_: &Path, _scope| {
                crate::harness::cli::LoginCheck::CouldNotRun(String::new())
            },
            mailbox: &ladder_mailbox_files,
            log: std::sync::Arc::new(|_: &str| {}),
            run_attempt: None,
            save_oauth: |_, _| Ok("inert".into()),
            shared: shared_renewal::Effects::default(),
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

/// The upkeep's renewal of Kimi's pool over `bed` at `now` ([`renew_pool_due`]) under `guard`,
/// its renewals started by a seam that runs nothing; whether one started.
#[cfg(unix)]
fn kimi_pool_upkeep(bed: &Bed, guard: &Arc<Mutex<AutoRenewGuard>>, now: i64) -> bool {
    let spawn = |_work: Box<dyn FnOnce() + Send>| Ok(());
    let fetch = |_: &Env, s: &str| -> Result<String, String> { Err(s.to_owned()) };
    let cooldown = Mutex::new(None);
    let fx = UpkeepEffects {
        scope: None,
        fetch: &fetch,
        spawn_renewal: &spawn,
        sync_cooldown: &cooldown,
        rediscovery: &Mutex::new(Rediscovery::default()),
        renew_guard: guard,
        find_browser: &|_: &Path| false,
        vault_sync: |_: &Path, _scope| crate::harness::sign_in_ladder::Vault::NotMember,
        default_check: |_: &Path, _scope| {
            crate::harness::cli::LoginCheck::CouldNotRun(String::new())
        },
        spawn_vault: &|_work| Ok(()),
        mailbox: &ladder_mailbox_files,
        log: std::sync::Arc::new(|_: &str| {}),
        run_attempt: None,
        save_oauth: |_, _| Ok("inert".into()),
        shared: shared_renewal::Effects::default(),
    };
    renew_pool_due(
        Cli::Kimi,
        &bed.home,
        &bed.state,
        &bed.roster(Cli::Kimi),
        now,
        &fx,
    )
}

/// A renewal host standing in for [`super::super::cli::LiveAccounts`] under
/// `spawn_vendor_renewal_with`'s worker: its vendor sign-in runs no child and ends `outcome` on
/// www.kimi.ai at `now`, leaving Kimi's login file in the staging home where it signed in.
#[cfg(unix)]
struct EndsOn {
    outcome: super::super::login_drive::Outcome,
    now: i64,
}

#[cfg(unix)]
impl super::super::cli::AccountsHost for EndsOn {
    fn stdin_is_terminal(&self) -> bool {
        false
    }
    fn auth_status(&self, login: &str) -> Result<super::super::align::Captured, String> {
        panic!("a Kimi renewal runs no `claude` (asked under {login:?})")
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        Err("not here".to_owned())
    }
    fn now(&self) -> i64 {
        self.now
    }
    fn vendor_sign_in(
        &self,
        req: &super::super::login_drive::Request<'_>,
    ) -> super::super::login_drive::Drove {
        if matches!(
            self.outcome,
            super::super::login_drive::Outcome::SignedIn { .. }
        ) {
            fs::create_dir_all(req.staging.join("credentials")).unwrap();
            fs::write(req.staging.join("credentials/kimi-code.json"), "{}").unwrap();
        }
        super::super::login_drive::Drove {
            outcome: self.outcome.clone(),
            host: Some("www.kimi.ai".to_owned()),
        }
    }
}

/// One renewal of `bed`'s Kimi row `label` by [`super::super::cli::renew_one_for`], as
/// `spawn_vendor_renewal_with`'s worker runs it (the row and records [`renew_pool_due`] reads,
/// the roster's own directory), its sign-in ending `outcome` at `now`: what it ended, its line
/// on record written as a real renewal writes it.
#[cfg(unix)]
fn kimi_renewal_ending(
    bed: &Bed,
    label: &str,
    outcome: super::super::login_drive::Outcome,
    now: i64,
) -> super::super::cli::Renewal {
    let roster_path = bed.roster(Cli::Kimi);
    let store = accounts::Store::at_for(&bed.state, Cli::Kimi, &bed.home);
    let candidates = read_renewal_candidates(
        Cli::Kimi,
        &store,
        (&bed.home, &bed.state),
        &roster_path,
        now,
    )
    .expect("the records");
    let (_, row) = candidates
        .tiered
        .into_iter()
        .find(|(_, row)| row.label == label)
        .expect("the row is a renewal candidate");
    let own_dir = candidates
        .records
        .raw_roster
        .row(label)
        .and_then(|r| r.dir.clone());
    super::super::cli::renew_one_for(
        Cli::Kimi,
        now,
        &EndsOn { outcome, now },
        &store,
        (&row, &roster_path, &candidates.records.logins),
        (
            own_dir.as_deref(),
            crate::harness::sign_in_ladder::asked_fixture(Cli::Kimi, label),
        ),
        &mut io::sink(),
        &mut io::sink(),
    )
}

/// The bot check [`kimi_renewal_ending`] meets.
#[cfg(unix)]
fn kimi_bot_check() -> super::super::login_drive::Outcome {
    super::super::login_drive::Outcome::BotCheck(
        "Cloudflare's bot check on `www.kimi.ai/code/authorize_device` did not clear in 90 s"
            .to_owned(),
    )
}

/// Publish a scripted browser result with the row and workflow the window inspected.
fn record_kimi_browser_window(bed: &Bed, label: &str, at: i64, why: &str) {
    let store = accounts::Store::at_for(&bed.state, Cli::Kimi, &bed.home);
    let path = bed.roster(Cli::Kimi);
    let observed = store.read_account_records(Some(&path), at).unwrap();
    let row = observed.raw_roster.row(label).unwrap();
    let workflow = accounts::current_workflow(row, &observed.logins).unwrap();
    let parent = accounts::RenewalParent::capture(&observed.logins, label);
    store
        .transact_pool_logins_within(&[], &path, at, &mut None, |raw, _, logins| {
            assert_eq!(raw.row(label), Some(row));
            parent.require_current(logins, label)?;
            assert!(accounts::workflow_token_current(row, logins, workflow));
            accounts::admit_workflow(logins, label)?;
            let mut event = accounts::Event::new(at, accounts::Kind::SignIn, label).because(why);
            event.workflow = Some(workflow);
            assert!(accounts::workflow_event_matches(row, logins, &event));
            Ok(vec![event])
        })
        .unwrap();
}

/// A POOL ROW WHOSE BACKGROUND RENEWAL MET CLOUDFLARE'S BOT CHECK IS LEFT ALONE PAST THE
/// 15-MINUTE COOLDOWN, with its human login task retained quietly:
/// each try starts a live sign-in at the vendor that the check ends again, so the cooldown alone
/// tried it again every 15 minutes. Driven through the upkeep's own path ([`renew_pool_due`],
/// its seam running nothing), the try itself run by `renew_one_for` ([`kimi_renewal_ending`]),
/// which writes its end on record, its end noted as `spawn_vendor_renewal_with`'s worker notes
/// it and the guard's clocks set back to stand for the time passed, for a row never signed in
/// here and for one already bound here. A restart, which forgets the guard, keeps the hold
/// from the bot check and its human task on record.
/// CONTROL: a plain failure is tried again once its cooldown has passed; the bot-checked row
/// at once on a person's own selection ([`renew_requested_pin_for`]), and after a restart once
/// [`PERSON_BACKOFF_SECS`] has passed. A browser window alone does not complete the login task.
#[cfg(unix)]
#[test]
fn a_pool_rows_bot_check_is_left_alone_past_the_cooldown() {
    use super::super::cli::Renewal;
    use super::super::login_drive::Outcome;
    const KEY: &str = "kimi:kimi-1";
    // One background try of `kimi-1` that ended `end`, run and put on record by
    // `renew_one_for`, then the upkeep `ago` seconds later.
    let try_again =
        |bed: &Bed, guard: &Arc<Mutex<AutoRenewGuard>>, end: Renewal, now: i64, ago: u64| {
            assert!(kimi_pool_upkeep(bed, guard, now), "the first try starts");
            let outcome = if end == Renewal::BotCheck {
                kimi_bot_check()
            } else {
                Outcome::Failed
            };
            assert_eq!(kimi_renewal_ending(bed, "kimi-1", outcome, now), end);
            note_background_end(guard, KEY, end, Instant::now());
            {
                let mut held = guard.lock().unwrap();
                let held = &mut *held;
                for clock in [
                    &mut held.last_attempt,
                    &mut held.parked,
                    &mut held.bot_checked,
                ] {
                    if let Some(at) = clock.get_mut(KEY) {
                        *at = at
                            .checked_sub(Duration::from_secs(ago))
                            .expect("this machine has been up for 15 minutes");
                    }
                }
            }
            kimi_pool_upkeep(bed, guard, now + i64::try_from(ago).unwrap())
        };
    for bound in [false, true] {
        let bed = Bed::new(if bound {
            "vendor-bot-check-bound"
        } else {
            "vendor-bot-check"
        });
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
        signed_out_kimi_login(&bed, "kimi-1", now);
        let store = accounts::Store::at_for(&bed.state, Cli::Kimi, &bed.home);
        if bound {
            store
                .transact_logins(now, &mut None, |_, logins| {
                    accounts::note_sign_in_task(
                        logins,
                        "kimi-1",
                        accounts::SignInAction::Login,
                        now,
                    );
                    if let Some(binding) = logins.labels.get_mut("kimi-1") {
                        binding.sign_in_task = None;
                    }
                    Ok(Vec::new())
                })
                .unwrap();
        }
        let (logins, _) = store.load_logins(now).unwrap();
        assert_eq!(logins.get("kimi-1").is_some(), bound, "bound: {bound}");
        assert!(logins.get("kimi-1").and_then(|b| b.sign_in_task).is_none());

        // The bot check last: its line on record holds the next first try.
        for (end, ago, again) in [
            (Renewal::Failed, 600, false),
            (Renewal::Failed, 901, true),
            (Renewal::BotCheck, 901, false),
        ] {
            let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
            assert_eq!(
                try_again(&bed, &guard, end, now, ago),
                again,
                "{end:?}, then the upkeep {ago} s later (bound here: {bound})"
            );
        }

        // A restart forgets the guard, not the ledger's bot check.
        let restarted = || Arc::new(Mutex::new(AutoRenewGuard::default()));
        assert!(
            !kimi_pool_upkeep(&bed, &restarted(), now + 60),
            "a restart a minute after the bot check (bound here: {bound})"
        );
        assert!(
            kimi_pool_upkeep(&bed, &restarted(), now + PERSON_BACKOFF_SECS),
            "CONTROL: a restart once the backoff has passed (bound here: {bound})"
        );
        // That try's end, a plain failure, is the newest on record.
        assert_eq!(
            kimi_renewal_ending(&bed, "kimi-1", Outcome::Failed, now + PERSON_BACKOFF_SECS),
            Renewal::Failed
        );

        // Start a later challenge episode after the preceding task's wait has elapsed.
        let now = now + PERSON_BACKOFF_SECS + 1;
        let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
        assert!(!try_again(&bed, &guard, Renewal::BotCheck, now, 901));
        record_kimi_browser_window(
            &bed,
            "kimi-1",
            now + 901,
            "www.kimi.ai window done session-kept",
        );
        assert!(
            !kimi_pool_upkeep(&bed, &guard, now + 902),
            "a window alone does not complete the native login task (bound here: {bound})"
        );

        let now = now + PERSON_BACKOFF_SECS + 1;
        let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
        assert!(!try_again(&bed, &guard, Renewal::BotCheck, now, 901));
        let request = store.request_pin("kimi-1", now + 901).unwrap();
        let spawn = |_work: Box<dyn FnOnce() + Send>| Ok(());
        assert_eq!(
            renew_requested_pin_for(
                Cli::Kimi,
                &bed.home,
                &bed.state,
                &bed.roster(Cli::Kimi),
                request,
                now + 901,
                &guard,
                &spawn,
                None,
                None,
            ),
            RenewalDispatch::Started,
            "CONTROL: a person's own selection starts at once (bound here: {bound})"
        );
    }
}

/// A LATER END ON RECORD LIFTS THE BOT CHECK'S HOLD ACROSS A RESTART ([`bot_check_on_record_at`]):
/// after a background renewal of `kimi-1` met the bot check, a renewal by `renew_one_for` that
/// signed in another account (once that end's own 15-minute hold has passed) or one that bound a
/// new login (its login signed out again after) is the newest end on record, so a restarted
/// upkeep tries the row again before [`PERSON_BACKOFF_SECS`] has passed. CONTROL: with only the
/// bot check on record, a restart is held.
#[cfg(unix)]
#[test]
fn a_later_renewal_or_wrong_account_lifts_the_bot_check_hold_after_a_restart() {
    use super::super::cli::Renewal;
    use super::super::login_drive::Outcome;
    let signed_in_as = |email: &str| Outcome::SignedIn {
        email: Some(email.to_owned()),
        page: false,
    };
    // Whether an upkeep with a fresh guard, as after a restart, tries `kimi-1` at `at`.
    let restarted_tries = |bed: &Bed, at: i64| {
        let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
        kimi_pool_upkeep(bed, &guard, at);
        let held = guard.lock().unwrap();
        held.last_attempt.contains_key("kimi:kimi-1")
    };
    for (later, ends, after) in [
        (
            signed_in_as("someone-else@example.com"),
            Renewal::WrongAccount,
            901,
        ),
        (signed_in_as("kimi-one@example.com"), Renewal::Renewed, 60),
    ] {
        let bed = Bed::new(&format!("bot-check-lifted-{}", ends == Renewal::Renewed));
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
        signed_out_kimi_login(&bed, "kimi-1", now);
        assert_eq!(
            kimi_renewal_ending(&bed, "kimi-1", kimi_bot_check(), now),
            Renewal::BotCheck
        );
        assert!(
            !restarted_tries(&bed, now + 60),
            "CONTROL: the bot check on record holds a restart ({ends:?})"
        );
        assert_eq!(kimi_renewal_ending(&bed, "kimi-1", later, now + 120), ends);
        if ends == Renewal::Renewed {
            accounts::Store::at_for(&bed.state, Cli::Kimi, &bed.home)
                .transact_logins(now + 150, &mut None, |state, logins| {
                    Ok(accounts::note_login_end(state, logins, "kimi-1", now + 150))
                })
                .unwrap();
        }
        assert!(
            restarted_tries(&bed, now + 120 + after),
            "a later {ends:?} on record lifts the hold after a restart"
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
        store
            .transact_pool_logins_within(
                &[],
                &bed.roster(Cli::Kimi),
                now,
                &mut None,
                |raw, _, logins| {
                    assert_eq!(raw, &roster);
                    accounts::admit_workflow(logins, "kimi-1")?;
                    accounts::note_sign_in_task(logins, "kimi-1", action, now - 300);
                    Ok(Vec::new())
                },
            )
            .unwrap();
        let task_logins = store.load_logins(now).unwrap().0;
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
        signed_out_kimi_login(&bed, label, now);
    }
    let mut note = None;
    store
        .transact_pool_logins_within(
            &[],
            &bed.roster(Cli::Kimi),
            now,
            &mut note,
            |raw, _state, logins| {
                assert_eq!(raw, &roster);
                accounts::admit_workflow(logins, "kimi-1")?;
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
            },
        )
        .unwrap();
    let pool_guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let cooldown = Mutex::new(None);
    let fetch = |_: &Env, s: &str| -> Result<String, String> { Err(s.to_owned()) };
    let fx = UpkeepEffects {
        scope: None,
        fetch: &fetch,
        spawn_renewal: &spawn,
        sync_cooldown: &cooldown,
        rediscovery: &Mutex::new(Rediscovery::default()),
        renew_guard: &pool_guard,
        find_browser: &|_: &Path| false,
        vault_sync: |_: &Path, _scope| crate::harness::sign_in_ladder::Vault::NotMember,
        spawn_vault: &|_work| Ok(()),
        default_check: |_: &Path, _scope| {
            crate::harness::cli::LoginCheck::CouldNotRun(String::new())
        },
        mailbox: &ladder_mailbox_files,
        log: std::sync::Arc::new(|_: &str| {}),
        run_attempt: None,
        save_oauth: |_, _| Ok("inert".into()),
        shared: shared_renewal::Effects::default(),
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
    let bound = accounts::bound(&roster, &logins_with_task);
    live_guard.lock().unwrap().activity_owners.insert(
        "kimi:kimi-1".into(),
        LoginOwner::new(&store, Cli::Kimi, "kimi-1", now)
            .observing(&logins_with_task)
            .for_row(bound.row("kimi-1").unwrap())
            .for_workflow(roster.row("kimi-1").unwrap(), &logins_with_task),
    );
    for unsigned_why in [
        "www.kimi.ai window done no-session",
        "www.kimi.ai window interrupted session-unknown",
        "www.kimi.ai window done untidied",
    ] {
        record_kimi_browser_window(&bed, "kimi-1", now - 10, unsigned_why);
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
    record_kimi_browser_window(
        &bed,
        "kimi-1",
        now - 5,
        "www.kimi.ai window done session-kept",
    );
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
    store
        .transact_pool_logins_within(
            &[],
            &bed.roster(Cli::Kimi),
            now,
            &mut None,
            |_, _, logins| {
                accounts::admit_workflow(logins, "kimi-1")?;
                accounts::note_sign_in_task(logins, "kimi-1", accounts::SignInAction::Login, now);
                Ok(Vec::new())
            },
        )
        .unwrap();
    let login_task_records = store
        .read_account_records(Some(&bed.roster(Cli::Kimi)), now)
        .unwrap();
    let login_task_logins = &login_task_records.logins;
    assert_eq!(
        accounts::current_sign_in_task(
            login_task_records.raw_roster.row("kimi-1").unwrap(),
            login_task_logins,
        )
        .unwrap()
        .at,
        now,
    );
    let slept_guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    note_background_end(
        &slept_guard,
        "kimi:kimi-1",
        super::super::cli::Renewal::NeedsPerson,
        Instant::now(),
    );
    slept_guard.lock().unwrap().activity_owners.insert(
        "kimi:kimi-1".into(),
        LoginOwner::new(&store, Cli::Kimi, "kimi-1", now)
            .observing(login_task_logins)
            .for_row(login_task_records.roster.row("kimi-1").unwrap())
            .for_workflow(
                login_task_records.raw_roster.row("kimi-1").unwrap(),
                login_task_logins,
            ),
    );
    assert_eq!(
        spawn_vendor_renewal(
            Cli::Kimi,
            &bed.home,
            &bed.state,
            &bed.roster(Cli::Kimi),
            &roster,
            login_task_logins,
            login_task_records.roster.row("kimi-1").cloned().unwrap(),
            now + PERSON_BACKOFF_SECS,
            &slept_guard,
            &spawn,
        ),
        RenewalDispatch::Started,
        "PERSON_BACKOFF elapsed on wall clock must admit retry after sleep"
    );
}

/// A WALLED VENDOR POOL SIGNS IN NO SUCCESSOR NOBODY HERE USES (§4.15 rule 4): with `main` at a
/// wall and `kimi-1` limited, the upkeep leaves unenrolled `kimi-2` alone, since no session here
/// runs on it and it is no due login this Mac uses; a launch meanwhile refuses a row not signed
/// in here. Once a session here runs on `kimi-2` and its login is signed out, the upkeep signs it
/// in (CONTROL).
#[test]
fn a_walled_vendor_pool_leaves_an_unused_successor_to_a_session_or_a_person() {
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
        scope: None,
        fetch: &fetch,
        spawn_renewal: &spawn,
        sync_cooldown: &cooldown,
        rediscovery: &Mutex::new(Rediscovery::default()),
        renew_guard: &idle_guard,
        find_browser: &|_: &Path| false,
        vault_sync: |_: &Path, _scope| crate::harness::sign_in_ladder::Vault::NotMember,
        spawn_vault: &|_work| Ok(()),
        default_check: |_: &Path, _scope| {
            crate::harness::cli::LoginCheck::CouldNotRun(String::new())
        },
        mailbox: &ladder_mailbox_files,
        log: std::sync::Arc::new(|_: &str| {}),
        run_attempt: None,
        save_oauth: |_, _| Ok("inert".into()),
        shared: shared_renewal::Effects::default(),
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
        &super::super::cli::LiveAccounts::default(),
        &mut out,
        &mut err,
    );
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(
        String::from_utf8_lossy(&out),
        "nothing is due on this machine\n",
        "CONTROL: unwalled `renew --cli kimi --due` finds nothing due"
    );

    // `kimi-1` enrolled, used here, rate-limited and signed out (`renew_due`, limited); `kimi-2`
    // `NotEnrolled` with an older historical `wrong_at`; `main` at a weekly wall.
    signed_out_kimi_login(&bed, "kimi-1", now);
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
        scope: None,
        fetch: &fetch,
        spawn_renewal: &spawn,
        sync_cooldown: &cooldown,
        rediscovery: &Mutex::new(Rediscovery::default()),
        renew_guard: &wall_guard,
        find_browser: &|_: &Path| false,
        vault_sync: |_: &Path, _scope| crate::harness::sign_in_ladder::Vault::NotMember,
        spawn_vault: &|_work| Ok(()),
        default_check: |_: &Path, _scope| {
            crate::harness::cli::LoginCheck::CouldNotRun(String::new())
        },
        mailbox: &ladder_mailbox_files,
        log: std::sync::Arc::new(|_: &str| {}),
        run_attempt: None,
        save_oauth: |_, _| Ok("inert".into()),
        shared: shared_renewal::Effects::default(),
    };
    assert!(
        !renew_pool_due(
            Cli::Kimi,
            &bed.home,
            &bed.state,
            &bed.roster(Cli::Kimi),
            now,
            &wall_fx,
        ),
        "a wall is no use of kimi-2, and limited kimi-1 waits for its wall"
    );
    assert!(wall_guard.lock().unwrap().last_attempt.is_empty());
    assert_eq!(*spawned.lock().unwrap(), 0);

    // CONTROL: a session here runs on `kimi-2`, whose login is signed out here.
    store
        .transact(now + 10, &mut None, |state| {
            let mut events = accounts::note_session(state, "s-kimi-2", "kimi-2", None, now + 10);
            events.extend(accounts::note_auth(
                state,
                "kimi-2",
                accounts::Auth::SignedOut,
                now + 10,
            ));
            Ok(events)
        })
        .unwrap();
    assert!(renew_pool_due(
        Cli::Kimi,
        &bed.home,
        &bed.state,
        &bed.roster(Cli::Kimi),
        now + 10,
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
        "the row a session here runs on is signed in"
    );
}

/// A pin request on a Codex default row (no directory) whose `~/.codex/auth.json` is missing is
/// a sign-in, never a pin onto a row no launch would use: the renewal path reads the default
/// row's login file as `Store::complete_requested_pin_at` does. CONTROL: with the file there, the
/// same kind of request is recovered as a pin.
#[test]
fn a_pin_request_on_a_default_codex_row_with_no_login_file_is_not_pinned() {
    let bed = Bed::new("default-row-pin");
    fake_headless_browser(&bed.home);
    fs::write(
        bed.roster(Cli::Codex),
        "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\nemail = \"owner@example.test\"\n",
    )
    .unwrap();
    let now = crate::supervise::limit::unix_now().max(NOW);
    let store = accounts::Store::at_for(&bed.state, Cli::Codex, &bed.home);
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let noop_spawn = |_: Box<dyn FnOnce() + Send>| Ok(());
    let renew = |request| {
        renew_requested_pin_for(
            Cli::Codex,
            &bed.home,
            &bed.state,
            &bed.roster(Cli::Codex),
            request,
            now,
            &guard,
            &noop_spawn,
            None,
            None,
        )
    };
    let missing = renew(store.request_pin("main", now).unwrap());
    assert_ne!(missing, RenewalDispatch::Recovered);
    let (state, _) = store.load(now).unwrap();
    assert!(
        state.active.as_deref() != Some("main") || !state.pinned,
        "no pin on a default row whose login file is missing: {state:?}"
    );
    fs::create_dir_all(bed.home.join(".codex")).unwrap();
    fs::write(bed.home.join(".codex/auth.json"), "{}").unwrap();
    assert_eq!(
        renew(store.request_pin("main", now).unwrap()),
        RenewalDispatch::Recovered
    );
    let (state, _) = store.load(now).unwrap();
    assert_eq!(state.active.as_deref(), Some("main"));
    assert!(state.pinned);
}

#[test]
#[cfg(unix)]
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
            "[[account]]\nlabel = \"codex-1\"\nclass = \"dedicated\"\ndir = {}\n\
             email = \"codex-one@example.com\"\n\n\
             [[account]]\nlabel = \"codex-2\"\nclass = \"dedicated\"\ndir = {}\n\
             email = \"codex-two@example.com\"\n\n\
             [[account]]\nlabel = \"codex-3\"\nclass = \"dedicated\"\ndir = {}\n",
            crate::harness::host_path::toml(&login_1),
            crate::harness::host_path::toml(&login_2),
            crate::harness::host_path::toml(&login_3),
        ),
    )
    .unwrap();
    // Even with `[harness] logins = "off"`, an explicit saved choice must resume.
    crate::test_config::write(
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
            None,
            None,
        ),
        RenewalDispatch::Held,
        "in-flight Claude renewal must hold Codex's request without cancelling it"
    );
    assert_eq!(
        codex_store
            .load(now)
            .unwrap()
            .0
            .pending_pin
            .as_ref()
            .and_then(accounts::PendingSelection::account),
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
            None,
            None,
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
    let finish = |work: Box<dyn FnOnce() + Send>| {
        work();
        Ok(())
    };
    let lifetime = crate::harness::account_work::Lifetime::default();
    let fx = UpkeepEffects {
        scope: lifetime.enter(),
        fetch: &fetch,
        spawn_renewal: &finish,
        sync_cooldown: &cooldown,
        rediscovery: &Mutex::new(Rediscovery::default()),
        renew_guard: &guard,
        find_browser: &|_: &Path| false,
        vault_sync: |_: &Path, _scope| crate::harness::sign_in_ladder::Vault::NotMember,
        spawn_vault: &|_work| Ok(()),
        default_check: |_: &Path, _scope| {
            crate::harness::cli::LoginCheck::CouldNotRun(String::new())
        },
        mailbox: &ladder_mailbox_files,
        log: std::sync::Arc::new(|_: &str| {}),
        run_attempt: None,
        save_oauth: |_, _| Ok("inert".into()),
        shared: shared_renewal::Effects::default(),
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

/// C17a (`docs/DESIGN-alab-swarm-2026-10-27.md` §9, §12.3): `[harness] accounts = "off"` stops the
/// window's upkeep completely (returning [`Upkept::Off`], spawning no Claude or pooled-CLI
/// sign-in even when standing rosters, mailbox routes and `logins = "renew"` are present).
/// NEGATIVE CONTROL: the identical home with `accounts = "early"` dispatches both renewals.
#[test]
fn harness_accounts_off_stops_window_upkeep_sign_ins_as_well_as_sync() {
    for off in [true, false] {
        let bed = Bed::new(if off { "upkeep-off" } else { "upkeep-early" });
        fake_headless_browser(&bed.home);
        // Seed standing rosters first so upkeep has unenrolled/due rows ready without syncing.
        bed.secret(FLEET_ROSTER_FILE, CLAUDE_FLEET);
        bed.secret(&cli_fleet_file(Cli::Kimi), KIMI_FLEET);
        bed.keep_roster(Cli::Kimi);
        let claude_roster = bed.config.join(accounts::ACCOUNTS_FILE);
        assert!(sync_accounts_for_init(&bed.home, &bed.state, &claude_roster, NOW).is_ok());
        let _ = sync_cli_accounts_for_init(&bed.home, &bed.state, &claude_roster, NOW);
        signed_out_kimi_login(&bed, "kimi-1", NOW);
        let claude_store = accounts::Store::at(&bed.state);
        fs::create_dir_all(
            claude_store
                .dir
                .join(crate::harness::open_url::DEFAULT_MAILBOX_DIR),
        )
        .unwrap();

        let mode = if off { "off" } else { "early" };
        crate::test_config::write(
            bed.aterm_toml(),
            format!(
                "[fleet]\nmachine = \"m16\"\n[harness]\naccounts = \"{mode}\"\nlogins = \"renew\"\n"
            ),
        )
        .unwrap();

        type Work = Box<dyn FnOnce() + Send>;
        let queued: Arc<Mutex<Vec<Work>>> = Arc::new(Mutex::new(Vec::new()));
        let into_queue = Arc::clone(&queued);
        let spawn = move |work| {
            into_queue.lock().unwrap().push(work);
            Ok(())
        };
        let fetch = |_: &Env, source: &str| -> Result<String, String> {
            Err(format!("unexpected fetch: {source}"))
        };
        let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
        let cooldown = Mutex::new(None);
        let fx = UpkeepEffects {
            scope: None,
            fetch: &fetch,
            spawn_renewal: &spawn,
            sync_cooldown: &cooldown,
            rediscovery: &Mutex::new(Rediscovery::default()),
            renew_guard: &guard,
            find_browser: &|_: &Path| true,
            vault_sync: |_: &Path, _scope| crate::harness::sign_in_ladder::Vault::NotMember,
            default_check: |_: &Path, _scope| {
                crate::harness::cli::LoginCheck::CouldNotRun(String::new())
            },
            spawn_vault: &|_work| Ok(()),
            mailbox: &ladder_mailbox_files,
            log: std::sync::Arc::new(|_: &str| {}),
            run_attempt: None,
            save_oauth: |_, _| Ok("inert".into()),
            shared: shared_renewal::Effects::default(),
        };
        let upkept = upkeep(
            &bed.home,
            &bed.state,
            (&claude_roster, NOW + 10, UpkeepStep::Both),
            &fx,
        );
        let spawned = queued.lock().unwrap().len();
        queued.lock().unwrap().clear();
        if off {
            assert_eq!(upkept, Upkept::Off);
            assert_eq!(spawned, 0, "accounts = \"off\" must dispatch zero renewals");
        } else {
            assert_ne!(upkept, Upkept::Off, "CONTROL: early runs upkeep");
            assert!(
                spawned >= 1,
                "CONTROL: early dispatches due sign-in work (spawned={spawned}, upkept={upkept:?})"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn published_kimi_refresh_schedules_without_a_browser_and_keeps_identity_retry_holds() {
    for case in 0..5 {
        let bed = Bed::new("kimi-native-no-browser");
        let cli = Cli::Kimi;
        let store = accounts::Store::at_for(&bed.state, cli, &bed.home);
        let path = bed.roster(cli);
        let raw = accounts::Row {
            label: "native".into(),
            email: Some("owner@example.com".into()),
            vendor: accounts::Vendor {
                region: Some(crate::ai::connector::Region::Global),
                ..Default::default()
            },
            ..Default::default()
        };
        fs::write(&path, accounts::row_text(&raw).unwrap()).unwrap();
        if case == 4 {
            let home = store.default_home().unwrap();
            drop(
                crate::harness::cli::shared_provider::kimi_pending_proof_fixture(
                    &home,
                    crate::ai::connector::Region::Global,
                    "owner@example.com",
                    true,
                ),
            );
        }
        if matches!(case, 1..=3) {
            let dir = store.claim_staging_dir(&raw.label).unwrap();
            store
                .transact_pool_logins_within(
                    &accounts::LOCK_LADDER,
                    &path,
                    NOW,
                    &mut None,
                    |_, state, logins| {
                        let events = accounts::note_renewed(
                            state,
                            logins,
                            &raw.label,
                            None,
                            dir.to_str().unwrap(),
                            None,
                            NOW,
                        );
                        if case == 2 {
                            accounts::note_sign_in_task(
                                logins,
                                &raw.label,
                                accounts::SignInAction::Browser,
                                NOW,
                            );
                        }
                        if case == 3 {
                            accounts::note_wrong_account(logins, &raw.label, NOW - 5);
                        }
                        Ok(events)
                    },
                )
                .unwrap();
        }
        let records = store.read_account_records(Some(&path), NOW).unwrap();
        let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
        assert!(!guard_reads(&guard).has_browser(&bed.home));
        let handed = Arc::new(Mutex::new(0));
        let counted = handed.clone();
        let spawn = move |_work: Box<dyn FnOnce() + Send>| {
            *counted.lock().unwrap() += 1;
            Ok(())
        };
        let result = spawn_vendor_renewal(
            cli,
            &bed.home,
            &bed.state,
            &path,
            &records.raw_roster,
            &records.logins,
            records.roster.row(&raw.label).unwrap().clone(),
            NOW,
            &guard,
            &spawn,
        );
        assert_eq!(
            result,
            match case {
                0 => RenewalDispatch::Failed,
                3 => RenewalDispatch::Held,
                _ => RenewalDispatch::Started,
            },
            "case {case}"
        );
        assert_eq!(
            *handed.lock().unwrap(),
            usize::from(matches!(case, 1 | 2 | 4))
        );
    }
}

/// The sheet whose one line gives `address`'s account of `cli` to `computer`.
fn sheet_giving(cli: Cli, address: &str, computer: &str) -> String {
    let cells = match cli {
        Cli::Claude => "1,,,",
        Cli::Codex => ",1,,",
        Cli::Grok => ",,1,",
        _ => ",,,global",
    };
    format!(
        "m,email,computer,,claude,Open AI,xai,kimi,google,meta,Payment,notes\n\
         ,{address},{computer},,{cells},,,,\n"
    )
}

/// `cli`'s roster with this Mac's own login (`main`) and a row `other` for `other@example.test`,
/// no login of it made: what m159 keeps of m22's account.
fn keep_a_row_with_no_login(bed: &Bed, cli: Cli) {
    let dir = bed.home.join(".aterm/logins").join(cli.id()).join("other");
    let region = if cli == Cli::Kimi {
        "region = \"global\"\n"
    } else {
        ""
    };
    fs::write(
        bed.roster(cli),
        format!(
            "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n\n[[account]]\n\
             label = \"other\"\nclass = \"dedicated\"\ndir = \"{}\"\n\
             email = \"other@example.test\"\n{region}",
            dir.display()
        ),
    )
    .unwrap();
}

/// What [`rows_of_other_macs_at`] reads of `cli`'s roster in `bed`.
fn other_macs_of(bed: &Bed, cli: Cli) -> accounts::OtherMacs {
    let roster = bed.roster(cli);
    let records = accounts::Store::at_for(&bed.state, cli, &bed.home)
        .read_account_records(Some(&roster), NOW)
        .unwrap();
    rows_of_other_macs_at(
        cli,
        (&bed.home, &bed.state),
        &roster,
        (&records.raw_roster, &records.logins),
        NOW,
    )
}

/// The row `other`, held by `machine`.
fn other_held_by(machine: &str) -> accounts::OtherMacs {
    accounts::OtherMacs {
        named: BTreeMap::from([("other".to_owned(), machine.to_owned())]),
        ..accounts::OtherMacs::NONE
    }
}

/// The row `other`, whose source this Mac cannot compare with its own names, for `why`.
fn other_unread(why: accounts::UnreadWhy) -> accounts::OtherMacs {
    accounts::OtherMacs {
        unread: BTreeSet::from(["other".to_owned()]),
        unread_why: Some(why),
        ..accounts::OtherMacs::NONE
    }
}

/// ANOTHER MAC'S ACCOUNT (`ATERM-KI-1105-9`): a row with no login here whose account the source
/// gives other Macs alone reads as theirs on every CLI, follows the sheet's next edit since nothing
/// is kept, and stores no hold that would refuse a person naming it. The controls are everything
/// else: no source, an account the source does not know, and the source naming this Mac.
#[test]
fn a_row_its_source_gives_other_macs_alone_reads_as_theirs() {
    for cli in [Cli::Claude, Cli::Codex, Cli::Grok, Cli::Kimi] {
        let bed = Bed::new("other-macs");
        keep_a_row_with_no_login(&bed, cli);
        assert_eq!(
            other_macs_of(&bed, cli),
            accounts::OtherMacs::NONE,
            "{cli:?}: CONTROL: no source says nothing"
        );
        bed.secret(
            ACCOUNTS_CSV_FILE,
            &sheet_giving(cli, "other@example.test", "m22"),
        );
        assert_eq!(other_macs_of(&bed, cli), other_held_by("m22"), "{cli:?}");
        let records = accounts::Store::at_for(&bed.state, cli, &bed.home)
            .read_account_records(Some(&bed.roster(cli)), NOW)
            .unwrap();
        assert_eq!(
            records.raw_roster.row("other").unwrap().held_by,
            None,
            "{cli:?}: nothing is kept that would refuse a person naming the row"
        );
        bed.secret(
            ACCOUNTS_CSV_FILE,
            &sheet_giving(cli, "someone@example.test", "m22"),
        );
        assert_eq!(
            other_macs_of(&bed, cli),
            accounts::OtherMacs::NONE,
            "{cli:?}: CONTROL: an account it does not know"
        );
        bed.secret(
            ACCOUNTS_CSV_FILE,
            &sheet_giving(cli, "other@example.test", "m16"),
        );
        assert_eq!(
            other_macs_of(&bed, cli),
            accounts::OtherMacs::NONE,
            "{cli:?}: CONTROL: the source names this Mac"
        );
    }
}

/// `bed`'s Claude Code row `other`, which a session here ran on an hour ago and whose login has
/// gone since (recorded signed out), as the upkeep reads it: whether the renewal's ranking, the
/// due list and the renewal door each take it, what the read said of other Macs, and why a
/// report says the row waits ([`super::super::sign_in_ladder::upkeep_waits_because`]).
fn renewal_of_a_used_gone_row(
    bed: &Bed,
) -> ((bool, bool, bool), accounts::OtherMacs, &'static str) {
    let cli = Cli::Claude;
    let store = accounts::Store::at_for(&bed.state, cli, &bed.home);
    store
        .transact(NOW, &mut None, |state| {
            let mut events = accounts::note_session(state, "s-1", "other", None, NOW - 3600);
            state.sessions.clear();
            events.extend(accounts::note_auth(
                state,
                "other",
                accounts::Auth::SignedOut,
                NOW - 60,
            ));
            Ok(events)
        })
        .unwrap();
    let candidates =
        read_renewal_candidates(cli, &store, (&bed.home, &bed.state), &bed.roster(cli), NOW)
            .unwrap();
    let records = &candidates.records;
    let ranked = candidates
        .tiered
        .iter()
        .any(|(_, row)| row.label == "other");
    let due = accounts::renew_due(
        &records.roster,
        &records.logins,
        &records.state,
        &candidates.others,
        NOW,
    )
    .contains(&"other".to_owned());
    let row = records.roster.row("other").unwrap().clone();
    let admitted = upkeep_sign_in(cli, (records, &candidates.others), &row, NOW).is_some();
    let waits = super::super::sign_in_ladder::upkeep_waits_because(
        &row,
        &records.state,
        &candidates.others,
        NOW,
    );
    ((ranked, due, admitted), candidates.others, waits)
}

/// ANOTHER MAC'S ACCOUNT IS NOT RENEWED HERE (§4.15 rule 3): a row a session here ran on within
/// the week whose login has gone since (recorded signed out) is the upkeep's renewal while the
/// sheet gives its account to this Mac, and neither ranked, due nor admitted by the renewal door
/// once the sheet gives it to m22 alone. CONTROL: the sheet naming this Mac.
#[test]
fn a_used_row_whose_login_went_is_not_renewed_where_its_source_gives_it_to_another_mac() {
    for (computer, ours) in [("m16", true), ("m22", false)] {
        let bed = Bed::new("other-macs-renewal");
        keep_a_row_with_no_login(&bed, Cli::Claude);
        bed.secret(
            ACCOUNTS_CSV_FILE,
            &sheet_giving(Cli::Claude, "other@example.test", computer),
        );
        let (taken, others, _) = renewal_of_a_used_gone_row(&bed);
        assert_eq!(
            others.named.contains_key("other"),
            !ours,
            "the sheet gives it to {computer}"
        );
        assert_eq!(
            taken,
            (ours, ours, ours),
            "the sheet gives it to {computer}"
        );
    }
}

/// A SOURCE THIS MAC CANNOT COMPARE KEEPS THE DOOR SHUT (§4.15 rule 3): where `[fleet]
/// accounts_source` is a sheet only the network could read, no read here says whether the used
/// row whose login went is this Mac's, so it is unread, and neither ranked, due nor admitted.
/// CONTROL: the same sheet at a local path, naming this Mac, admits it.
#[test]
fn a_used_row_whose_login_went_is_not_renewed_where_its_source_cannot_be_read_here() {
    for (source, readable) in [
        ("sheet.csv", true),
        ("https://example.test/sheet.csv", false),
    ] {
        let bed = Bed::new("other-macs-unread");
        keep_a_row_with_no_login(&bed, Cli::Claude);
        crate::test_config::write(
            bed.aterm_toml(),
            format!("[fleet]\nmachine = \"m16\"\naccounts_source = \"{source}\"\n"),
        )
        .unwrap();
        fs::write(
            bed.config.join("sheet.csv"),
            sheet_giving(Cli::Claude, "other@example.test", "m16"),
        )
        .unwrap();
        let (taken, others, waits) = renewal_of_a_used_gone_row(&bed);
        if !readable {
            assert_eq!(
                waits,
                accounts::UnreadWhy::Network.words(),
                "a report names the network as the cause"
            );
        }
        assert_eq!(
            others,
            if readable {
                accounts::OtherMacs::NONE
            } else {
                other_unread(accounts::UnreadWhy::Network)
            },
            "{source}"
        );
        assert_eq!(
            taken,
            (readable, readable, readable),
            "{source}: the door renews it only where it can read whose it is"
        );
    }
}

/// [`only_another_machine`]: a machine among others is still named, whichever spelling the
/// machine goes by, and an account the source gives nobody is not another Mac's.
#[test]
fn an_account_named_for_this_mac_among_others_is_not_another_macs() {
    let row = Row {
        email: Some("other@example.test".into()),
        ..Row::default()
    };
    let fleet = |machines: &[&str]| FleetRoster {
        accounts: vec![FleetAccount {
            label: "other".into(),
            machines: machines.iter().map(|m| (*m).to_owned()).collect(),
            class: Class::Pool,
            email: "OTHER@example.test".into(),
            browser: None,
            vendor: accounts::Vendor::default(),
        }],
        ..FleetRoster::default()
    };
    let of = |machines: &[&str], me: &str| {
        only_another_machine(Cli::Claude, &fleet(machines), &[machine_spelling(me)], &row)
            .map(str::to_owned)
    };
    assert_eq!(of(&["m22", "m29"], "m16").as_deref(), Some("m22"));
    assert_eq!(of(&["m22", "m16"], "m16"), None);
    assert_eq!(of(&["m22", "m16"], "M16"), None);
    assert_eq!(of(&[], "m16"), None);
}

/// A LOGIN ALREADY HERE IS THIS MAC'S, whoever the source gives it to: a person's own borrowing
/// keeps working once made, signed out included.
#[test]
fn a_login_already_here_is_this_macs_whoever_the_source_gives_it_to() {
    let bed = Bed::new("other-macs-login");
    keep_a_row_with_no_login(&bed, Cli::Kimi);
    bed.secret(
        ACCOUNTS_CSV_FILE,
        &sheet_giving(Cli::Kimi, "other@example.test", "m22"),
    );
    assert_eq!(
        other_macs_of(&bed, Cli::Kimi),
        other_held_by("m22"),
        "CONTROL"
    );
    signed_out_kimi_login(&bed, "other", NOW);
    assert_eq!(other_macs_of(&bed, Cli::Kimi), accounts::OtherMacs::NONE);
}

/// A ROW ON THIS MAC'S DEFAULT LOGIN IS THIS MAC'S WHILE A LOGIN STANDS THERE, whoever the sheet
/// gives its account to. With no login there it has none here at all, as `aterm ai` reads it, and
/// is another Mac's like any other row.
#[test]
fn the_row_on_the_default_login_is_this_macs_while_its_login_stands() {
    let bed = Bed::new("other-macs-default");
    fs::write(
        bed.roster(Cli::Kimi),
        "[[account]]\nlabel = \"own\"\nclass = \"dedicated\"\n\
         email = \"other@example.test\"\nregion = \"global\"\n",
    )
    .unwrap();
    bed.secret(
        ACCOUNTS_CSV_FILE,
        &sheet_giving(Cli::Kimi, "other@example.test", "m22"),
    );
    assert_eq!(
        other_macs_of(&bed, Cli::Kimi),
        accounts::OtherMacs {
            named: BTreeMap::from([("own".to_owned(), "m22".to_owned())]),
            ..accounts::OtherMacs::NONE
        }
    );
    let login = bed
        .home
        .join(Cli::Kimi.accounts().default_home)
        .join("credentials/kimi-code.json");
    fs::create_dir_all(login.parent().unwrap()).unwrap();
    fs::write(&login, "{}").unwrap();
    assert_eq!(other_macs_of(&bed, Cli::Kimi), accounts::OtherMacs::NONE);
}

/// A POOL'S DEFAULT ROW IS ENROLLED ONLY WHILE ITS LOGIN STANDS ([`CliPool::enrolled`], the set
/// [`rows_of_other_macs`] reads), so the window's due line never names a login no longer on this
/// Mac, nor one whose account the sheet gives another Mac. CONTROL: with its login file there,
/// it is enrolled.
#[test]
fn a_pools_default_row_is_enrolled_only_while_its_login_stands() {
    let bed = Bed::new("pool-default-enrolled");
    fs::write(
        bed.roster(Cli::Kimi),
        "[[account]]\nlabel = \"own\"\nclass = \"dedicated\"\n\
         email = \"other@example.test\"\nregion = \"global\"\n",
    )
    .unwrap();
    let enrolled = || {
        cli_pools(&bed.aterm_toml(), &bed.state, &bed.home, NOW)
            .into_iter()
            .find(|pool| pool.cli == Cli::Kimi)
            .expect("the kept roster's pool")
            .enrolled
    };
    assert!(
        !enrolled().contains("own"),
        "no login stands on the default home"
    );
    let login = bed
        .home
        .join(Cli::Kimi.accounts().default_home)
        .join("credentials/kimi-code.json");
    fs::create_dir_all(login.parent().unwrap()).unwrap();
    fs::write(&login, "{}").unwrap();
    assert!(enrolled().contains("own"), "CONTROL: its login stands");
}

/// CLAUDE CODE READS WHOSE AN ACCOUNT IS FROM THE SOURCE ITS SYNC READS, a fleet file beside its
/// roster where no sheet stands. `CONTROL`: the same file naming this Mac.
#[test]
fn a_fleet_file_beside_claude_codes_roster_says_whose_an_account_is() {
    let bed = Bed::new("other-macs-beside");
    keep_a_row_with_no_login(&bed, Cli::Claude);
    let other_macs = |machine: &str| {
        fs::write(
            bed.config.join(FLEET_ROSTER_FILE),
            format!(
                "opt_in = \"named\"\n\n[[account]]\nlabel = \"other\"\n\
                 email = \"other@example.test\"\nmachines = [\"{machine}\"]\n"
            ),
        )
        .unwrap();
        other_macs_of(&bed, Cli::Claude)
    };
    assert_eq!(other_macs("m22"), other_held_by("m22"));
    assert_eq!(
        other_macs("m16"),
        accounts::OtherMacs::NONE,
        "CONTROL: this Mac's"
    );
}

/// UNDER A POOL FILE THIS MAC GOES BY THE NAME THE POOL RESOLVES: `m100`, its roster's label, though
/// `aterm.toml` calls it by its alias `m32`, so an account warm on `m100` is its own and one warm on
/// `m9` alone is another Mac's, for each CLI whose sync reads a pool.
#[cfg(unix)]
#[test]
fn under_a_pool_file_this_mac_goes_by_the_name_the_pool_resolves() {
    for cli in [Cli::Claude, Cli::Codex] {
        let bed = Bed::new("other-macs-pool");
        keep_a_row_with_no_login(&bed, cli);
        crate::test_config::write(bed.aterm_toml(), "[fleet]\nmachine = \"m32\"\n").unwrap();
        super::super::ai_pool::tests::seed_local_swarm_roster(&bed.home, "m100", &["m32"]);
        let other_macs = |warm: &str| {
            bed.secret(
                super::super::ai_pool::AI_POOL_FILE,
                &format!(
                    "version = 1\n\n[last_heard]\nm100 = \"2026-10-27\"\n\n[[account]]\n\
                     provider = \"{}\"\nemail = \"other@example.test\"\nwarm = [\"{warm}\"]\n",
                    cli.id()
                ),
            );
            other_macs_of(&bed, cli)
        };
        assert_eq!(
            other_macs("m100"),
            accounts::OtherMacs::NONE,
            "{cli:?}: this Mac's"
        );
        assert_eq!(other_macs("m9"), other_held_by("m9"), "{cli:?}");
    }
}

/// A SOURCE `[fleet] accounts_source` NAMES says whose an account is, a relative path read from
/// the config's folder as a sync reads it. One only a network could read cannot say, since no
/// report fetches, so its row with no login here is unread. `CONTROL`: the same sheet naming
/// this Mac.
#[test]
fn a_configured_source_says_whose_an_account_is() {
    let bed = Bed::new("other-macs-key");
    keep_a_row_with_no_login(&bed, Cli::Claude);
    crate::test_config::write(
        bed.aterm_toml(),
        "[fleet]\nmachine = \"m16\"\naccounts_source = \"sheet.csv\"\n",
    )
    .unwrap();
    let sheet = bed.config.join("sheet.csv");
    fs::write(
        &sheet,
        sheet_giving(Cli::Claude, "other@example.test", "m22"),
    )
    .unwrap();
    assert_eq!(other_macs_of(&bed, Cli::Claude), other_held_by("m22"));
    fs::write(
        &sheet,
        sheet_giving(Cli::Claude, "other@example.test", "m16"),
    )
    .unwrap();
    assert_eq!(
        other_macs_of(&bed, Cli::Claude),
        accounts::OtherMacs::NONE,
        "CONTROL"
    );
    crate::test_config::write(
        bed.aterm_toml(),
        "[fleet]\nmachine = \"m16\"\naccounts_source = \"https://example.test/sheet.csv\"\n",
    )
    .unwrap();
    assert_eq!(
        other_macs_of(&bed, Cli::Claude),
        other_unread(accounts::UnreadWhy::Network),
        "a network's"
    );
}

/// A POOL THAT REFUSES THIS MAC'S NAME LEAVES ITS ROWS UNREAD: where the swarm roster denies
/// `m100`, the name `aterm.toml` gives this Mac, the pool gives this Mac no identity, and no other
/// name stands in for it, so a row the pool keeps warm on `m100` is not read as this Mac's
/// ([`accounts::UnreadWhy::NoName`]). CONTROL: with nothing denied, the pool names this Mac and
/// the row is its own.
#[cfg(unix)]
#[test]
fn a_pool_that_refuses_this_macs_name_leaves_its_rows_unread() {
    let cli = Cli::Codex;
    for denied in [false, true] {
        let bed = Bed::new("other-macs-pool-denied");
        keep_a_row_with_no_login(&bed, cli);
        crate::test_config::write(bed.aterm_toml(), "[fleet]\nmachine = \"m100\"\n").unwrap();
        let deny: &[&str] = if denied { &["m100"] } else { &[] };
        super::super::ai_pool::tests::seed_local_swarm_roster_denying(&bed.home, "m100", &[], deny);
        bed.secret(
            super::super::ai_pool::AI_POOL_FILE,
            &format!(
                "version = 1\n\n[last_heard]\nm100 = \"2026-10-27\"\n\n[[account]]\n\
                 provider = \"{}\"\nemail = \"other@example.test\"\nwarm = [\"m100\"]\n",
                cli.id()
            ),
        );
        let others = other_macs_of(&bed, cli);
        if denied {
            assert!(others.unread.contains("other"), "{others:?}");
            assert_eq!(others.unread_why, Some(accounts::UnreadWhy::NoName));
        } else {
            assert_eq!(others, accounts::OtherMacs::NONE, "CONTROL");
        }
    }
}

/// WITH NO HOME THE VAULT'S SOURCES CANNOT BE READ, and they outrank a roster of its own, so a
/// Claude Code row with no login here is unread ([`accounts::UnreadWhy::Source`]). CONTROL: the
/// same roster read with its home, which holds no source, is nobody else's.
#[test]
fn with_no_home_a_claude_row_with_no_login_is_unread() {
    let bed = Bed::new("other-macs-no-home");
    keep_a_row_with_no_login(&bed, Cli::Claude);
    let roster = bed.roster(Cli::Claude);
    let records = accounts::Store::at_for(&bed.state, Cli::Claude, &bed.home)
        .read_account_records(Some(&roster), NOW)
        .unwrap();
    let mut env = quiet_env(&bed.home, &bed.state, &bed.aterm_toml(), NOW);
    let read =
        |env: &Env| rows_of_other_macs(Cli::Claude, env, &records.raw_roster, &records.logins);
    assert_eq!(read(&env), accounts::OtherMacs::NONE, "CONTROL");
    env.home = None;
    let others = read(&env);
    assert!(others.unread.contains("other"), "{others:?}");
    assert_eq!(others.unread_why, Some(accounts::UnreadWhy::Source));
}

/// A VAULT THAT CANNOT BE READ LEAVES THE ROW UNREAD, never another Mac's: the swarm roster in it
/// may name this Mac by a name it had, so a source's row could be this Mac's own. CONTROL: the same
/// source and the same row read as another Mac's while the vault reads.
#[cfg(unix)]
#[test]
fn a_vault_that_cannot_be_read_leaves_the_row_unread() {
    let bed = Bed::new("other-macs-vault");
    keep_a_row_with_no_login(&bed, Cli::Claude);
    crate::test_config::write(bed.aterm_toml(), "[fleet]\nmachine = \"m16\"\n").unwrap();
    fs::write(
        bed.config.join(FLEET_ROSTER_FILE),
        "opt_in = \"named\"\n\n[[account]]\nlabel = \"other\"\n\
         email = \"other@example.test\"\nmachines = [\"m22\"]\n",
    )
    .unwrap();
    assert_eq!(
        other_macs_of(&bed, Cli::Claude),
        other_held_by("m22"),
        "CONTROL"
    );
    // A vault with no lock file is "no copy applied here", which is no roster and no error.
    let vault = aterm_secrets::client::Paths::for_home(&bed.home).dir;
    fs::create_dir_all(vault.join(".lock")).unwrap();
    let unread = other_macs_of(&bed, Cli::Claude);
    assert_eq!(
        unread,
        other_unread(accounts::UnreadWhy::Source),
        "{unread:?}"
    );
}

/// Sheet rows that conflict over an address say whose it is as one row would. Each names another
/// Mac: the sheet moved it off this one ([`carry_conflicting`]), so the first it names holds it.
/// `CONTROL`: one of them names this Mac, and the row stays this Mac's while they disagree.
#[test]
fn sheet_rows_that_conflict_over_an_account_say_whose_it_is() {
    let bed = Bed::new("other-macs-conflict");
    keep_a_row_with_no_login(&bed, Cli::Claude);
    crate::test_config::write(bed.aterm_toml(), "[fleet]\nmachine = \"m16\"\n").unwrap();
    let conflict = |first: &str, second: &str| {
        bed.secret(
            ACCOUNTS_CSV_FILE,
            &format!(
                "m,email,computer,,claude,Open AI,xai,kimi,google,meta,Payment,notes\n\
                 ,other@example.test,{first},,1,,,,,,,\n\
                 ,other@example.test,{second},,1,,,,,,,\n\
                 ,mine@example.test,m16,,1,,,,,,,\n"
            ),
        );
        other_macs_of(&bed, Cli::Claude)
    };
    assert_eq!(conflict("m20", "m22"), other_held_by("m20"));
    assert_eq!(
        conflict("m16", "m22"),
        accounts::OtherMacs::NONE,
        "CONTROL: names this Mac"
    );
}

/// A NAME THIS MAC HAD IS STILL THIS MAC'S: its swarm roster row is `m100` with the alias `m32`,
/// and a sheet naming `m32` does not make the account another Mac's. `CONTROL`: the sheet naming
/// `m22` does.
#[cfg(unix)]
#[test]
fn a_sheet_naming_a_name_this_mac_had_gives_it_to_this_mac() {
    let bed = Bed::new("other-macs-alias");
    keep_a_row_with_no_login(&bed, Cli::Claude);
    crate::test_config::write(bed.aterm_toml(), "[fleet]\nmachine = \"m100\"\n").unwrap();
    super::super::ai_pool::tests::seed_local_swarm_roster(&bed.home, "m100", &["m32"]);
    let named = |computer: &str| {
        bed.secret(
            ACCOUNTS_CSV_FILE,
            &sheet_giving(Cli::Claude, "other@example.test", computer),
        );
        other_macs_of(&bed, Cli::Claude)
    };
    assert_eq!(named("m22"), other_held_by("m22"), "CONTROL");
    assert_eq!(named("m32"), accounts::OtherMacs::NONE, "its own old name");
}

/// A LAB THAT COULD NOT BE READ MAY NAME THIS MAC OR NOT, so what a sync would refuse to read
/// meanwhile leaves the row with no login here unread. `CONTROL`: the same fleet file says whose
/// it is once the lab is not pending.
#[test]
fn a_pending_lab_leaves_whose_an_account_is_unread() {
    let bed = Bed::new("other-macs-pending");
    keep_a_row_with_no_login(&bed, Cli::Claude);
    crate::test_config::write(bed.aterm_toml(), "[fleet]\nmachine = \"m16\"\n").unwrap();
    fs::write(
        bed.config.join(FLEET_ROSTER_FILE),
        "opt_in = \"named\"\n\n[[account]]\nlabel = \"other\"\n\
         email = \"other@example.test\"\nmachines = [\"m22\"]\n",
    )
    .unwrap();
    assert_eq!(
        other_macs_of(&bed, Cli::Claude),
        other_held_by("m22"),
        "CONTROL"
    );
    let pending = crate::harness::alab_roster::while_pending(|| other_macs_of(&bed, Cli::Claude));
    assert_eq!(
        pending,
        other_unread(accounts::UnreadWhy::Source),
        "{pending:?}"
    );
}

/// A SYNC'S RECEIPT LISTS NO SIGN-IN OF ANOTHER MAC'S ACCOUNT: under a fleet discovery made, whose
/// sync keeps the rows a roster has, its `needs_renew` leaves the row out. `CONTROL`: it lists the
/// row while the fleet names this Mac.
#[test]
fn a_syncs_receipt_lists_no_sign_in_of_another_macs_account() {
    let needs = |machine: &str| {
        let bed = Bed::new("other-macs-receipt");
        keep_a_row_with_no_login(&bed, Cli::Claude);
        crate::test_config::write(bed.aterm_toml(), "[fleet]\nmachine = \"m16\"\n").unwrap();
        fs::write(
            bed.config.join(FLEET_ROSTER_FILE),
            format!(
                "opt_in = \"discovered\"\n\n[[account]]\nlabel = \"other\"\n\
                 email = \"other@example.test\"\nmachines = [\"{machine}\"]\n"
            ),
        )
        .unwrap();
        let env = quiet_env(&bed.home, &bed.state, &bed.aterm_toml(), NOW);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run_accounts_sync(&env, None, None, None, true, &mut out, &mut err);
        assert_eq!(code, ExitCode::SUCCESS, "{}", String::from_utf8_lossy(&err));
        let doc: Value = aterm_json::from_str(&String::from_utf8(out).unwrap()).unwrap();
        doc["needs_renew"].as_array().unwrap().len()
    };
    assert_eq!(needs("m16"), 1, "CONTROL");
    assert_eq!(needs("m22"), 0);
}
