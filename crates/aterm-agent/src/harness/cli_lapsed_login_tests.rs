// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A login its CLI's own files show lapsed reads `signed-out` in the accounts report and the
//! survey, from local files alone (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.15, rule 6).

use super::*;

struct NoProvider;

impl AccountsHost for NoProvider {
    fn stdin_is_terminal(&self) -> bool {
        false
    }
    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        panic!("no provider IO")
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        panic!("no login")
    }
    fn now(&self) -> i64 {
        0
    }
}

/// The lab's dead stored login (`aterm-lab`'s `claude_login_proof::DEAD_LOGIN`): no token,
/// expired at the epoch.
const CLAUDE_DEAD: &str = "{\"claudeAiOauth\":{\"accessToken\":\"\",\"refreshToken\":\"\",\
                           \"expiresAt\":0,\"scopes\":[\"user:inference\",\"user:profile\"],\
                           \"subscriptionType\":null}}\n";
/// A live Claude Code login: both tokens, expiring far ahead.
const CLAUDE_LIVE: &str = "{\"claudeAiOauth\":{\"accessToken\":\"synthetic-access\",\
                           \"refreshToken\":\"synthetic-refresh\",\"expiresAt\":4102444800000}}\n";
/// A Claude Code login whose access token expired and whose refresh token renews it.
const CLAUDE_REFRESHABLE: &str = "{\"claudeAiOauth\":{\"accessToken\":\"\",\
                                  \"refreshToken\":\"synthetic-refresh\",\"expiresAt\":0}}\n";

/// One row's login: its credential file's bytes, and whether aterm marked it refused after.
struct Shape {
    label: &'static str,
    body: &'static str,
    refused: bool,
}

/// `cli`'s pool of `shapes`, each row naming a directory of its own that holds its credential.
fn pool(cli: Cli, shapes: &[Shape]) -> (Tmp, Env) {
    let root = tmp_dir("lapsed-login");
    let env = env_at(&root);
    let roster = env.accounts_roster_for(cli).expect("a roster path");
    std::fs::create_dir_all(roster.parent().expect("a folder")).expect("mkdir");
    let mut rows = Vec::new();
    for shape in shapes {
        let dir = root.path().join("logins").join(shape.label);
        let credential = dir.join(cli.connector().stores[0]);
        std::fs::create_dir_all(credential.parent().expect("a folder")).expect("mkdir");
        std::fs::write(&credential, shape.body).expect("write credential");
        if shape.refused {
            crate::harness::grok_signin::mark_refused(&credential).expect("mark refused");
        }
        rows.push(accounts::Row {
            label: shape.label.into(),
            dir: Some(dir.to_str().expect("a UTF-8 path").to_owned()),
            ..accounts::Row::default()
        });
    }
    std::fs::write(
        &roster,
        accounts::append_rows_for(cli, "", &rows).expect("the rows"),
    )
    .expect("write roster");
    (root, env)
}

/// Each row's (label, enrolled, condition) as the survey's account lines read it.
fn surveyed(cli: Cli, env: &Env) -> Vec<(String, bool, &'static str)> {
    let config = env.config.clone().expect("a config path");
    let places = (config.as_path(), env.state.as_path(), env.home.as_deref());
    survey_accounts(cli, places, env.now)
        .expect("the pool reads")
        .rows
        .into_iter()
        .map(|row| (row.label, row.enrolled, row.condition.as_str()))
        .collect()
}

/// Each row's (label, enrolled, condition) and the launch's next row, as
/// `aterm harness accounts --cli all --json` reports `cli`'s pool.
fn reported(cli: Cli, env: &Env) -> (Vec<(String, bool, String)>, Value) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run_accounts_all(true, env, &NoProvider, &mut out, &mut err);
    assert_eq!(code, ExitCode::SUCCESS, "{}", String::from_utf8_lossy(&err));
    let doc: Value = aterm_json::from_slice(&out).expect("one JSON document");
    let pool = doc["pools"]
        .as_array()
        .expect("pools")
        .iter()
        .find(|pool| pool["cli"] == Value::from(cli.id()))
        .unwrap_or_else(|| panic!("no {} pool: {doc}", cli.id()))
        .clone();
    let rows = pool["report"]["accounts"]
        .as_array()
        .expect("rows")
        .iter()
        .map(|row| {
            (
                row["label"].as_str().expect("a label").to_owned(),
                row["enrolled"].as_bool().expect("enrolled"),
                row["condition"].as_str().expect("a condition").to_owned(),
            )
        })
        .collect();
    (rows, pool["report"]["next"]["to"].clone())
}

fn owned(rows: &[(&str, bool, &str)]) -> Vec<(String, bool, String)> {
    rows.iter()
        .map(|(label, enrolled, condition)| {
            ((*label).to_owned(), *enrolled, (*condition).to_owned())
        })
        .collect()
}

/// Rows as `(label, enrolled, condition)`.
type Rows = Vec<(String, bool, String)>;

/// The survey's rows, the report's rows and the report's next row.
fn both(cli: Cli, env: &Env) -> (Rows, Rows, Value) {
    let survey = surveyed(cli, env)
        .into_iter()
        .map(|(label, enrolled, condition)| (label, enrolled, condition.to_owned()))
        .collect();
    let (report, next) = reported(cli, env);
    (survey, report, next)
}

/// A GROK LOGIN aterm MARKED REFUSED READS SIGNED-OUT, in the survey and the report alike, from
/// its `auth.json`'s refusal marker alone, and no launch takes it; the live row beside it stays
/// `unseen`, and no provider is read. NEGATIVE CONTROL: with the lapse unread
/// ([`crate::ai::connector::test_without_lapse`]) both readers say `unseen` for it, the defect.
#[test]
fn a_refused_grok_login_reads_signed_out_in_the_survey_and_the_report() {
    let (_root, env) = pool(
        Cli::Grok,
        &[
            Shape {
                label: "lapsed",
                body: "{\"opaque\":\"credential\"}",
                refused: true,
            },
            Shape {
                label: "live",
                body: "{\"opaque\":\"credential\"}",
                refused: false,
            },
        ],
    );
    let ((survey, report, next), reads) =
        crate::harness::provider_read::captured(|| both(Cli::Grok, &env));
    assert_eq!(reads, [], "a local read asks no provider");
    let want = owned(&[("lapsed", true, "signed-out"), ("live", true, "unseen")]);
    assert_eq!(survey, want, "the survey");
    assert_eq!(report, want, "the report");
    assert_eq!(next, Value::from("live"), "no launch takes a lapsed login");

    let (survey, report, next) = crate::ai::connector::test_without_lapse(|| both(Cli::Grok, &env));
    let defect = owned(&[("lapsed", true, "unseen"), ("live", true, "unseen")]);
    assert_eq!(
        (survey, report, next),
        (defect.clone(), defect, Value::from("lapsed")),
        "NEGATIVE CONTROL"
    );
}

/// A CLAUDE LOGIN WHOSE CREDENTIAL FILE HOLDS NO TOKEN (the lab's dead login) READS SIGNED-OUT
/// in the survey and the report; one with both tokens, and one whose access token expired beside
/// a refresh token (which Claude Code renews), stay `unseen`. NEGATIVE CONTROL: with the lapse
/// unread both readers say `unseen` for the dead one.
#[test]
fn a_tokenless_claude_login_reads_signed_out_and_a_refreshable_one_does_not() {
    let (_root, env) = pool(
        Cli::Claude,
        &[
            Shape {
                label: "dead",
                body: CLAUDE_DEAD,
                refused: false,
            },
            Shape {
                label: "live",
                body: CLAUDE_LIVE,
                refused: false,
            },
            Shape {
                label: "refreshable",
                body: CLAUDE_REFRESHABLE,
                refused: false,
            },
        ],
    );
    let ((survey, report, next), reads) =
        crate::harness::provider_read::captured(|| both(Cli::Claude, &env));
    assert_eq!(reads, [], "a local read asks no provider");
    let want = owned(&[
        ("dead", true, "signed-out"),
        ("live", true, "unseen"),
        ("refreshable", true, "unseen"),
    ]);
    assert_eq!(survey, want, "the survey");
    assert_eq!(report, want, "the report");
    assert_eq!(next, Value::from("live"), "no launch takes a lapsed login");

    let (survey, report, next) =
        crate::ai::connector::test_without_lapse(|| both(Cli::Claude, &env));
    let defect = owned(&[
        ("dead", true, "unseen"),
        ("live", true, "unseen"),
        ("refreshable", true, "unseen"),
    ]);
    assert_eq!(
        (survey, report, next),
        (defect.clone(), defect, Value::from("dead")),
        "NEGATIVE CONTROL"
    );
}

/// A PICK JUDGES A LAPSED ROW SIGNED OUT, as the report does: the state a window's account
/// advice, its account menu and a worker launch judge rows by ([`accounts::Store::load_judged`])
/// lists the refused grok login as lapsed, and `condition` says signed-out for it and nothing new
/// for the live one. NEGATIVE CONTROL: the plain [`accounts::Store::load`] those paths used
/// leaves the lapse unread, and the lapsed row is `unseen`, the defect E4 found in the GUI paths.
#[test]
fn a_pick_judges_a_lapsed_row_by_the_hydrated_state() {
    let (_root, env) = pool(
        Cli::Grok,
        &[
            Shape {
                label: "lapsed",
                body: "{\"opaque\":\"credential\"}",
                refused: true,
            },
            Shape {
                label: "live",
                body: "{\"opaque\":\"credential\"}",
                refused: false,
            },
        ],
    );
    let path = env.accounts_roster_for(Cli::Grok).expect("a roster path");
    let roster = accounts::Roster::load_for(Cli::Grok, &path).expect("the roster");
    let home = env.home.clone().unwrap_or_default();
    let store = accounts::Store::at_for(&env.state, Cli::Grok, &home);
    let conditions = |state: &accounts::State| {
        roster
            .rows
            .iter()
            .map(|row| {
                (
                    row.label.clone(),
                    accounts::condition(row, state, true, env.now).as_str(),
                )
            })
            .collect::<Vec<_>>()
    };
    let (judged, _) = store
        .load_judged(env.now, &roster)
        .expect("the judged state");
    assert_eq!(
        conditions(&judged),
        [
            ("lapsed".to_owned(), "signed-out"),
            ("live".to_owned(), "unseen")
        ]
    );
    let (plain, _) = store.load(env.now).expect("the plain state");
    assert_eq!(
        conditions(&plain),
        [
            ("lapsed".to_owned(), "unseen"),
            ("live".to_owned(), "unseen")
        ],
        "NEGATIVE CONTROL"
    );
}

/// Claude Code's pool of a default row (no `dir`) whose default home `<home>/.claude` holds the
/// lab's dead login, beside a live row with a directory of its own.
fn default_pool() -> (Tmp, Env) {
    let (root, env) = pool(
        Cli::Claude,
        &[Shape {
            label: "live",
            body: CLAUDE_LIVE,
            refused: false,
        }],
    );
    let home = env.home.clone().expect("a home").join(".claude");
    std::fs::create_dir_all(&home).expect("mkdir");
    std::fs::write(home.join(".credentials.json"), CLAUDE_DEAD).expect("write credential");
    let roster = env.accounts_roster_for(Cli::Claude).expect("a roster path");
    let live = root.path().join("logins").join("live");
    let rows = [
        accounts::Row {
            label: "default".into(),
            ..accounts::Row::default()
        },
        accounts::Row {
            label: "live".into(),
            dir: Some(live.to_str().expect("a UTF-8 path").to_owned()),
            ..accounts::Row::default()
        },
    ];
    std::fs::write(
        &roster,
        accounts::append_rows_for(Cli::Claude, "", &rows).expect("the rows"),
    )
    .expect("write roster");
    (root, env)
}

/// AN EXPIRED CLAUDE CODE DEFAULT LOGIN READS SIGNED-OUT WHERE THE PLATFORM KEEPS IT IN A FILE
/// (Linux: `<home>/.claude/.credentials.json`), in the survey and the report, and no launch takes
/// it; WHERE IT KEEPS IT IN THE KEYCHAIN (macOS) aterm reads nothing for it and it stays
/// `unseen`. Both platforms are picked on this thread
/// ([`crate::ai::connector::test_on_login_platform`]), so either OS runs both arms. NEGATIVE
/// CONTROL: on the file platform with the lapse unread
/// ([`crate::ai::connector::test_without_lapse`]) both readers say `unseen` for it, the defect.
#[test]
fn an_expired_claude_default_login_reads_signed_out_where_it_is_a_file() {
    use crate::ai::connector::{test_on_login_platform, test_without_lapse};
    let (_root, env) = default_pool();

    let ((survey, report, next), reads) = crate::harness::provider_read::captured(|| {
        test_on_login_platform(true, || both(Cli::Claude, &env))
    });
    assert_eq!(reads, [], "a local read asks no provider");
    let want = owned(&[("default", true, "signed-out"), ("live", true, "unseen")]);
    assert_eq!(survey, want, "the survey, file platform");
    assert_eq!(report, want, "the report, file platform");
    assert_eq!(next, Value::from("live"), "no launch takes a lapsed login");

    let unseen = owned(&[("default", true, "unseen"), ("live", true, "unseen")]);
    let (survey, report, _) = test_on_login_platform(false, || both(Cli::Claude, &env));
    assert_eq!(
        (survey, report),
        (unseen.clone(), unseen.clone()),
        "the keychain platform reads nothing for the default login"
    );

    let (survey, report, _) =
        test_on_login_platform(true, || test_without_lapse(|| both(Cli::Claude, &env)));
    assert_eq!(
        (survey, report),
        (unseen.clone(), unseen),
        "NEGATIVE CONTROL"
    );
}
