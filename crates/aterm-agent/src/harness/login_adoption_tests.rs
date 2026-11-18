// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The liveness reading and the adoption step (plan package FL-174): the step conforms to
//! `AccountLoginAdoption` over every fixture the model reaches, evidence is only of the row's
//! own stored login and of a place a file proves the row's, served evidence is a request that
//! began after the sign-out and was answered by a response not already in flight then, a
//! `/login` is the command's own row, a file its process stopped writing shows nothing, a lost
//! baseline never turns old growth into evidence, a pass reads its files once and within
//! bounds, choosing its transcripts by their own time, and the writer is idempotent by the
//! order of its times.
//! Every fixture address is `example.test`.

use std::fs;
use std::path::{Path, PathBuf};

use super::*;
use crate::harness::accounts::{Class, Cli, Store};

/// When the fixture row's sign-out is recorded.
const T: i64 = 1_792_659_940;
/// The fixture tab and its Claude Code process.
pub(in crate::harness) const SID: &str = "td1b73bd013";
pub(in crate::harness) const PID: u32 = 68_537;
/// The conversation that process holds.
pub(in crate::harness) const CONVERSATION: &str = "6cf4169e-20c7-4bcb-a97f-e8a1387ed6ea";
/// The fixture row and its address.
const LABEL: &str = "main";
const OWN: &str = "main@example.test";

/// Writes the usage-tap file of the fixture process under the harness state `state_dir`, as its
/// status line leaves it: on the stored login of `login` (a login directory, or `None` for the
/// default login) unless `route` says otherwise, its figures first reported at `at`, its API
/// time `api_ms`, with a seven-day figure where `week`.
pub(in crate::harness) fn write_tap(
    state_dir: &Path,
    login: Option<&str>,
    route: Option<tap::Route>,
    figures: (i64, u64),
    week: bool,
) {
    write_tap_of(
        state_dir,
        (SID, PID, CONVERSATION),
        login,
        route,
        figures,
        week,
    );
}

/// [`write_tap`] for the process `pid` of the tab `sid`, which holds `conversation`; returns
/// the file.
fn write_tap_of(
    state_dir: &Path,
    (sid, pid, conversation): (&str, u32, &str),
    login: Option<&str>,
    route: Option<tap::Route>,
    (at, api_ms): (i64, u64),
    week: bool,
) -> PathBuf {
    let reading = tap::Reading {
        at,
        sid: sid.to_owned(),
        pid,
        session: Some(conversation.to_owned()),
        login: login.map(str::to_owned),
        home: None,
        api_ms,
        five_hour: None,
        seven_day: week.then_some(tap::Window {
            pct: 12,
            resets_at: at + 6 * 86_400,
            pct_x10: Some(123),
        }),
        warm: None,
        cache_expires_at: None,
        version: None,
        model: None,
        effort: None,
        ctx_left: None,
        dur_us: 0,
        row: None,
        route,
    };
    let dir = tap::rl_dir_in(state_dir);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(tap::file_name(sid, pid).unwrap());
    fs::write(&path, reading.to_json()).unwrap();
    path
}

/// The fixture process's usage-tap file under the harness state `state_dir`.
pub(in crate::harness) fn tap_path(state_dir: &Path) -> PathBuf {
    tap::rl_dir_in(state_dir).join(tap::file_name(SID, PID).unwrap())
}

/// Sets when the fixture process's usage-tap file was last written, as a tap that stopped
/// then leaves it.
pub(in crate::harness) fn tap_written_at(state_dir: &Path, at: i64) {
    written_at(&tap_path(state_dir), at);
}

/// Sets when the file `path` was last written.
fn written_at(path: &Path, at: i64) {
    let written =
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(u64::try_from(at).unwrap());
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(written)
        .unwrap();
}

/// The profile of the login place `dir` (the person's home for the default login, or a login
/// directory), as a sign-in of `email` leaves it.
pub(in crate::harness) fn write_profile(dir: &Path, email: &str) {
    fs::create_dir_all(dir).unwrap();
    fs::write(
        dir.join(".claude.json"),
        format!("{{\"oauthAccount\":{{\"emailAddress\":\"{email}\"}}}}\n"),
    )
    .unwrap();
}

/// A transcript row's `timestamp` for the Unix second `at`.
fn stamp(at: i64) -> String {
    aterm_types::rfc3339::format_rfc3339(u64::try_from(at).unwrap())
}

/// The fixture conversation's transcript under the default home of `home`.
pub(in crate::harness) fn transcript(home: &Path) -> PathBuf {
    transcript_of(home, CONVERSATION)
}

/// The transcript of `conversation` under the default home of `home`.
fn transcript_of(home: &Path, conversation: &str) -> PathBuf {
    let project = home.join(".claude/projects/-Users-someone-work");
    fs::create_dir_all(&project).unwrap();
    project.join(format!("{conversation}.jsonl"))
}

/// Appends the whole rows `rows` to the fixture conversation's transcript.
pub(in crate::harness) fn write_rows(home: &Path, rows: &[String]) {
    write_rows_to(&transcript(home), rows);
}

/// Appends the whole rows `rows` to the transcript `path`.
fn write_rows_to(path: &Path, rows: &[String]) {
    use std::io::Write as _;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    for row in rows {
        writeln!(file, "{row}").unwrap();
    }
}

/// A local command's row as Claude Code 2.1.325 writes it: a top-level `local_command` system
/// row whose content is `content`.
fn command_row(at: i64, content: &str) -> String {
    format!(
        "{{\"type\":\"system\",\"subtype\":\"local_command\",\"isSidechain\":false,\
         \"timestamp\":\"{}\",\"content\":{}}}",
        stamp(at),
        Value::from(content)
    )
}

/// The content of the `/login` command's own row.
const LOGIN_COMMAND_ROW: &str = "<command-name>/login</command-name>\n            \
     <command-message>login</command-message>\n            <command-args></command-args>";

/// Appends a `/login` that succeeded at `at` to the fixture conversation's transcript under the
/// default home of `home`: the command's row, then its `Login successful` output.
pub(in crate::harness) fn write_login_row(home: &Path, at: i64) {
    write_rows(
        home,
        &[
            command_row(at, LOGIN_COMMAND_ROW),
            command_row(at, LOGIN_MADE),
        ],
    );
}

/// A prompt a person typed at `at`, as the row `id`, after the row `parent`.
fn prompt_row(id: &str, parent: Option<&str>, at: i64, text: &str) -> String {
    format!(
        "{{\"type\":\"user\",\"uuid\":\"{id}\",\"parentUuid\":{},\"isSidechain\":false,\
         \"origin\":{{\"kind\":\"human\"}},\"turnOrigin\":\"human\",\"timestamp\":\"{}\",\
         \"message\":{{\"role\":\"user\",\"content\":{}}}}}",
        parent.map_or(Value::Null, Value::from),
        stamp(at),
        Value::from(text)
    )
}

/// A tool's result written at `at`, as the row `id`, after the row `parent`, saying `text`.
fn tool_result_row(id: &str, parent: &str, at: i64, text: &str) -> String {
    format!(
        "{{\"type\":\"user\",\"uuid\":\"{id}\",\"parentUuid\":\"{parent}\",\"isSidechain\":false,\
         \"timestamp\":\"{}\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\
         \"tool_result\",\"tool_use_id\":\"toolu_01\",\"content\":{}}}]}}}}",
        stamp(at),
        Value::from(text)
    )
}

/// The model's answer written at `at`, as the row `id`, after the row `parent`, the one row of
/// an API response of its own; `more` is further top-level fields (each ending in a comma).
fn answer_row(id: &str, parent: &str, at: i64, more: &str) -> String {
    response_row(id, parent, at, more, Some(&format!("msg_{id}")))
}

/// [`answer_row`] as one row of the API response `response` (its `message.id`, which every
/// row of one response carries), or of a row that carries none.
fn response_row(id: &str, parent: &str, at: i64, more: &str, response: Option<&str>) -> String {
    let response = response.map_or_else(String::new, |id| format!("\"id\":\"{id}\","));
    format!(
        "{{\"type\":\"assistant\",\"uuid\":\"{id}\",\"parentUuid\":\"{parent}\",{more}\
         \"timestamp\":\"{}\",\"message\":{{{response}\"role\":\"assistant\",\
         \"model\":\"claude-opus-4-7\",\"stop_reason\":\"end_turn\",\
         \"content\":[{{\"type\":\"text\",\"text\":\"done\"}}]}}}}",
        stamp(at)
    )
}

/// Appends to the fixture conversation's transcript a response in flight across the sign-out
/// that runs a tool while it streams, as Claude Code writes it: a prompt typed at `asked`,
/// the response's first row at `first`, its tool's result at `result`, and a later row of the
/// SAME response at `later`.
pub(in crate::harness) fn write_response_across_a_tool_result(
    home: &Path,
    asked: i64,
    (first, result, later): (i64, i64, i64),
) {
    let top = "\"isSidechain\":false,";
    write_rows(
        home,
        &[
            prompt_row("u-across", None, asked, "go on"),
            response_row("a-first", "u-across", first, top, Some("msg_across")),
            tool_result_row("u-result", "a-first", result, "ok"),
            response_row("a-later", "u-result", later, top, Some("msg_across")),
        ],
    );
}

/// Claude Code's lost-login answer written at `at`, after the row `parent`.
fn wall_row(parent: &str, at: i64) -> String {
    format!(
        "{{\"type\":\"assistant\",\"uuid\":\"wall-{at}\",\"parentUuid\":\"{parent}\",\
         \"isSidechain\":false,\"isApiErrorMessage\":true,\"error\":\"authentication_failed\",\
         \"timestamp\":\"{}\",\"message\":{{\"id\":\"msg_wall_{at}\",\"role\":\"assistant\",\
         \"model\":\"<synthetic>\",\
         \"content\":[{{\"type\":\"text\",\"text\":\"Please run /login\"}}]}}}}",
        stamp(at)
    )
}

/// Appends a request the model served to the fixture conversation's transcript: a prompt typed
/// at `asked` and the model's answer to it written at `answered`.
pub(in crate::harness) fn write_served_turn(home: &Path, asked: i64, answered: i64) {
    let (prompt, answer) = (format!("u-{asked}"), format!("a-{answered}"));
    write_rows(
        home,
        &[
            prompt_row(&prompt, None, asked, "go on"),
            answer_row(&answer, &prompt, answered, "\"isSidechain\":false,"),
        ],
    );
}

/// One Mac with one row, [`LABEL`], and a session running on it: the default login's row, or
/// one on a login directory of its own.
struct World {
    _dir: aterm_tempfile::TempDir,
    home: PathBuf,
    state: PathBuf,
    roster: PathBuf,
    /// The row's login directory, where it has one.
    login: Option<PathBuf>,
}

impl World {
    /// The default login's row, which recorded `word` at [`T`], with a sign-in task pending
    /// since then.
    fn new(word: Auth) -> Self {
        Self::with(word, false)
    }

    /// The same row on a login directory of its own.
    fn on_dir(word: Auth) -> Self {
        Self::with(word, true)
    }

    fn with(word: Auth, own_dir: bool) -> Self {
        let dir = aterm_tempfile::TempDir::new().unwrap();
        let root = dir.path().to_path_buf();
        let (home, state) = (root.join("home"), root.join("state"));
        fs::create_dir_all(&home).unwrap();
        let roster = root.join(accounts::ACCOUNTS_FILE);
        let store = Store::at(&state);
        let login = own_dir.then(|| store.logins_dir().join(LABEL));
        if let Some(login) = &login {
            fs::create_dir_all(login).unwrap();
        }
        let row = Row {
            label: LABEL.to_owned(),
            class: Class::Preferred,
            dir: login
                .as_ref()
                .map(|login| login.to_string_lossy().into_owned()),
            email: Some(OWN.to_owned()),
            ..Row::default()
        };
        fs::write(&roster, accounts::row_text(&row).unwrap() + "\n").unwrap();
        store
            .transact_pool_logins_within(&[], &roster, T, &mut None, |_, state, logins| {
                let mut events = accounts::note_session(state, "s-main", LABEL, None, T);
                events.extend(accounts::note_auth(state, LABEL, word, T));
                accounts::note_sign_in_task(logins, LABEL, accounts::SignInAction::Renew, T);
                Ok(events)
            })
            .unwrap();
        Self {
            _dir: dir,
            home,
            state,
            roster,
            login,
        }
    }

    /// The default login's row, signed out at [`T`], whose profile names it, as the upkeep
    /// first saw its tab after the sign-out (at `T + 10`: the baseline).
    fn seen() -> Self {
        let world = Self::new(Auth::SignedOut);
        write_profile(&world.home, OWN);
        world.tap(T - 5, 4_000);
        assert_eq!(world.pass(T + 10).0, Vec::new(), "nothing is newer yet");
        world
    }

    fn store(&self) -> Store {
        Store::at_for(&self.state, Cli::Claude, &self.home)
    }

    /// The row's login place as a tap names it: its directory, or nothing for the default
    /// login.
    fn place(&self) -> Option<String> {
        self.login
            .as_ref()
            .map(|login| login.to_string_lossy().into_owned())
    }

    /// The fixture tab's file on the row's stored login.
    fn tap(&self, at: i64, api_ms: u64) {
        write_tap(
            &self.state,
            self.place().as_deref(),
            Some(tap::Route::Stored),
            (at, api_ms),
            true,
        );
    }

    /// The tab was served: a request typed at `asked` was answered at `answered`, and its
    /// file reported `api_ms` then.
    fn served(&self, asked: i64, answered: i64, api_ms: u64) {
        write_served_turn(&self.home, asked, answered);
        self.tap(answered, api_ms);
    }

    /// One upkeep adoption step at `now`, a pass of its own.
    fn step(&self, now: i64) -> (Step, Vec<String>) {
        let logged = std::cell::RefCell::new(Vec::new());
        let step = adopt_live_logins(
            &self.home,
            &self.state,
            &self.roster,
            now,
            &Pass::default(),
            &|line| {
                logged.borrow_mut().push(line.to_owned());
            },
        );
        (step, logged.into_inner())
    }

    /// [`Self::step`]: what it adopted and what it logged.
    fn pass(&self, now: i64) -> (Vec<Adopted>, Vec<String>) {
        let (step, logged) = self.step(now);
        (step.adopted(), logged)
    }

    fn word(&self, now: i64) -> Auth {
        self.store().load(now).unwrap().0.account(LABEL).auth
    }

    fn task_pending(&self, now: i64) -> bool {
        self.store()
            .load_logins(now)
            .unwrap()
            .0
            .get(LABEL)
            .is_some_and(|binding| binding.sign_in_task.is_some())
    }
}

fn adopted(evidence: LoginEvidence) -> Vec<Adopted> {
    vec![Adopted {
        label: LABEL.to_owned(),
        evidence,
    }]
}

/// One fixture of the model: whether the step adopted the row, and whether its pending sign-in
/// task was cleared with the lift.
fn case(word: i64, order: i64, owner: i64, kind: i64, begun: i64) -> (bool, bool) {
    let word = if word == 1 { Auth::SignedOut } else { Auth::Ok };
    let world = if owner == 3 {
        World::on_dir(word)
    } else {
        World::new(word)
    };
    match owner {
        0 => write_profile(&world.home, OWN),
        1 => write_profile(&world.home, "another@example.test"),
        3 => {
            // Every profile there is names the row, and nothing proves the directory's place.
            write_profile(&world.home, OWN);
            write_profile(world.login.as_ref().unwrap(), OWN);
        }
        _ => {}
    }
    // The tab as it stood when the sign-out was recorded, and the upkeep's first look at it.
    world.tap(T - 5, 4_000);
    assert!(
        world.task_pending(T + 10),
        "the fixture's sign-in task is pending"
    );
    assert_eq!(world.pass(T + 10).0, Vec::new(), "nothing is newer yet");
    let now = T + 120;
    let at = match order {
        0 => T,
        1 => T + 97,
        _ => now + 380,
    };
    if kind == 0 && begun == 1 {
        // The request the row at `at` ends began before the sign-out (authorized by the login
        // that was then lost, and in flight across it), in the shape that hides it best: its
        // response ran a tool while it streamed, so the tool's result, two seconds before
        // `at`, and the row at `at` are both written after the request's own prompt.
        write_response_across_a_tool_result(&world.home, T - 20, (at - 4, at - 2, at));
        world.tap(at, 9_000);
    } else if kind == 0 {
        // The request the answer at `at` ends began two seconds before it.
        world.served(at - 2, at, 9_000);
    } else {
        write_login_row(&world.home, at);
    }
    let (lifts, logged) = world.pass(now);
    let lifted = !lifts.is_empty();
    assert_eq!(
        world.word(now) == Auth::Ok,
        lifted || word == Auth::Ok,
        "the record says what the step returned"
    );
    assert_eq!(logged.len(), usize::from(lifted), "{logged:?}");
    if lifted {
        let evidence = if kind == 0 {
            LoginEvidence::Served(at)
        } else {
            LoginEvidence::SignedIn(at)
        };
        assert_eq!(lifts, adopted(evidence));
        // A second fold of the same evidence, as another process's upkeep would make it,
        // changes nothing.
        assert_eq!(world.pass(now + 1).0, Vec::new());
    }
    (lifted, lifted && !world.task_pending(now))
}

pub(crate) fn adoption_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "NotSignedOut" => {
            next.insert("word", 0);
        }
        "LaterEvidence" => {
            next.insert("order", state["order"] + 1);
        }
        "AnotherOwner" => {
            next.insert("owner", state["owner"] + 1);
        }
        "SignedIn" => {
            next.insert("kind", 1);
        }
        "InFlight" => {
            next.insert("begun", 1);
        }
        "Adopt" => {
            let (lifted, cleared) = case(
                state["word"],
                state["order"],
                state["owner"],
                state["kind"],
                state["begun"],
            );
            next.insert("phase", 1);
            next.insert("lifted", i64::from(lifted));
            next.insert("cleared", i64::from(cleared));
        }
        _ => unreachable!(),
    }
    next
}

/// THE ADOPTION STEP LIFTS A SIGN-OUT ONLY ON EVIDENCE NEWER THAN IT, OF A REQUEST BEGUN AFTER
/// IT, AND OF A LOGIN PLACE A FILE PROVES THE ROW'S: the step conforms to
/// `AccountLoginAdoption` over every fixture the model reaches (a row signed out or not;
/// evidence not after the sign-out, after it, and dated after now; the default login's profile
/// naming the row, another account, or none, and a row on a login directory of its own; a
/// served request begun after the sign-out, one in flight across it, and a sign-in made in the
/// tab). NEGATIVE CONTROLS: each law fails on the state a broken step would leave.
#[test]
fn login_adopted_only_on_newer_evidence_of_the_rows_own_login() {
    let model = aterm_models_harness::account_login_adoption_model();
    assert_eq!(model.mutant_dials(), vec![1, 2, 3, 4, 5, 6, 7, 8, 9]);
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let mut cases = 0;
    let mut lifts = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = adoption_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{state:?} -> {next:?}"
                );
                cases += usize::from(action.name == "Adopt");
                lifts += usize::from(action.name == "Adopt" && next["lifted"] == 1);
            }
        }
    }
    assert_eq!(cases, 72);
    assert_eq!(
        lifts, 2,
        "a request served and a sign-in, each after the sign-out and of its own login"
    );
    let mut newer = model.init_state();
    newer.insert("order", 1);
    let good = adoption_transition(&newer, "Adopt");
    assert_eq!((good["lifted"], good["cleared"]), (1, 1));
    for (field, law) in [
        ("lifted", "NewerEvidenceOfItsLoginLifts"),
        ("cleared", "LiftClearsThePendingSignIn"),
    ] {
        let mut bad = good.clone();
        bad.insert(field, 0);
        assert!(!model.check_invariant(law, &bad), "{law}");
    }
    for (field, value, law) in [
        ("order", 0, "OlderEvidenceLiftsNothing"),
        ("order", 2, "FutureEvidenceLiftsNothing"),
        ("owner", 1, "AnothersLoginLiftsNothing"),
        ("owner", 2, "UnknownLoginLiftsNothing"),
        ("owner", 3, "UnprovedPlaceLiftsNothing"),
        ("begun", 1, "InFlightRequestLiftsNothing"),
        ("word", 0, "NothingSignedOutLiftsNothing"),
    ] {
        let mut bad = good.clone();
        bad.insert(field, value);
        assert!(!model.check_invariant(law, &bad), "{law}");
    }
}

/// A READING COUNTS ONLY FROM A PROCESS ON THE ROW'S OWN STORED LOGIN. CONTROL: the same file on
/// the row's stored login adopts it. A file whose tap named a token route, an outside route or
/// no route (an older build's file), one of another login place, and one in another Claude home
/// each adopt nothing, whatever their API time, their figures and the transcript say.
#[test]
fn login_adopted_evidence_is_only_of_the_rows_own_stored_login() {
    type Place = (Option<&'static str>, Option<tap::Route>);
    let places: [(&str, Place, bool); 5] = [
        (
            "its own stored login",
            (None, Some(tap::Route::Stored)),
            true,
        ),
        ("a token route", (None, Some(tap::Route::Token)), false),
        ("an outside route", (None, Some(tap::Route::Outside)), false),
        ("no route word", (None, None), false),
        (
            "a login directory",
            (
                Some("/Users//someone/.aterm/logins/w4"),
                Some(tap::Route::Stored),
            ),
            false,
        ),
    ];
    for (what, (login, route), lifts) in places {
        let world = World::new(Auth::SignedOut);
        write_profile(&world.home, OWN);
        write_tap(&world.state, login, route, (T - 5, 4_000), true);
        assert_eq!(world.pass(T + 10).0, Vec::new(), "{what}");
        write_served_turn(&world.home, T + 95, T + 97);
        write_tap(&world.state, login, route, (T + 97, 9_000), true);
        assert_eq!(!world.pass(T + 120).0.is_empty(), lifts, "{what}");
        assert_eq!(world.word(T + 120) == Auth::Ok, lifts, "{what}");
    }
    // Another Claude home: the reading is of whatever login that home holds.
    let world = World::new(Auth::SignedOut);
    write_profile(&world.home, OWN);
    write_served_turn(&world.home, T + 95, T + 97);
    for (at, api_ms) in [(T - 5, 4_000), (T + 97, 9_000)] {
        world.tap(at, api_ms);
        let path = tap_path(&world.state);
        let mut reading = tap::Reading::of_json(&fs::read_to_string(&path).unwrap()).unwrap();
        reading.home = Some("/Users//someone/other-home".to_owned());
        fs::write(&path, reading.to_json()).unwrap();
        assert_eq!(world.pass(at + 15).0, Vec::new(), "another Claude home");
    }
}

/// A ROW ON A LOGIN DIRECTORY OF ITS OWN IS DECLINED: no file without a credential says which
/// account that place holds, so its tab's `/login`, its served request and its grown API time
/// lift nothing, though the directory's profile and the default profile both name the row. The
/// step says the row is unproved, which the upkeep logs once. CONTROL: the same files for the
/// default login's row lift it.
#[test]
fn login_adopted_a_row_on_its_own_login_directory_is_declined_and_says_why() {
    for own_dir in [true, false] {
        let world = World::with(Auth::SignedOut, own_dir);
        write_profile(&world.home, OWN);
        if let Some(login) = &world.login {
            write_profile(login, OWN);
        }
        world.tap(T - 5, 4_000);
        let (first, _) = world.step(T + 10);
        assert_eq!(
            first,
            Step::default(),
            "{own_dir}: the first look says nothing"
        );
        write_login_row(&world.home, T + 38);
        world.served(T + 95, T + 97, 9_000);
        let (step, logged) = world.step(T + 120);
        if own_dir {
            assert_eq!(
                step.lifts,
                vec![(LABEL.to_owned(), Lift::Refused(Refusal::Unproved))]
            );
            assert_eq!(logged, Vec::<String>::new());
            assert_eq!(world.word(T + 120), Auth::SignedOut);
            assert!(world.task_pending(T + 120), "the ladder's task stands");
            assert!(!Refusal::Unproved.words().contains('@'));
        } else {
            assert_eq!(step.adopted(), adopted(LoginEvidence::SignedIn(T + 38)));
            assert_eq!(world.word(T + 120), Auth::Ok);
        }
    }
}

/// SERVED EVIDENCE IS A REQUEST THAT BEGAN AFTER THE SIGN-OUT. NEGATIVE CONTROL, the review's
/// case: a request authorized before the sign-out ends after it; the tab's API time grows past
/// the baseline, its figures are reported after the sign-out and the model's answer is dated
/// after it, and nothing is lifted. Nor is anything by: growth with no answer in the
/// transcript; a request begun after the sign-out but before the upkeep's first look; an answer
/// that is an API error; a subagent's answer; an answer to an earlier request than the one in
/// hand; and a served request a login wall follows.
///
/// NEGATIVE CONTROL, the second review's case: Claude Code runs a tool while its response
/// still streams, so the response in flight across the sign-out writes its tool's result after
/// the sign-out and then a later row of itself, chained to that result. That row was requested
/// before the result was written, and lifts nothing. Nor does: a tool's result with no response
/// read before it (the read's cut hid the response it belongs to); and an answer that carries
/// no response id, which nothing tells from the response before.
///
/// CONTROLS: a prompt typed after the sign-out and answered lifts the row at the answer's own
/// time; so does a tool result written after it and answered by the next response, though its
/// prompt was typed before the sign-out; and so does the response after the one in flight, at
/// its own first row's time.
#[test]
fn login_adopted_a_request_in_flight_across_the_sign_out_lifts_nothing() {
    let top = "\"isSidechain\":false,";
    let in_flight = |later: &[String]| {
        let mut rows = vec![
            prompt_row("u1", None, T - 20, "go on"),
            response_row("a1", "u1", T + 15, top, Some("msg_1")),
            tool_result_row("u2", "a1", T + 20, "ok"),
            response_row("a2", "u2", T + 22, top, Some("msg_1")),
        ];
        rows.extend_from_slice(later);
        rows
    };
    let cases: [(&str, Vec<String>, Option<i64>); 14] = [
        (
            "a later row of the response in flight, after its tool's result",
            in_flight(&[]),
            None,
        ),
        (
            "a third row of it",
            in_flight(&[response_row("a3", "a2", T + 24, top, Some("msg_1"))]),
            None,
        ),
        (
            "the response after the one in flight",
            in_flight(&[
                tool_result_row("u3", "a2", T + 30, "ok"),
                response_row("a3", "u3", T + 40, top, Some("msg_2")),
            ]),
            Some(T + 40),
        ),
        (
            "a tool's result with no response read before it",
            vec![
                tool_result_row("u2", "a1", T + 20, "ok"),
                answer_row("a2", "u2", T + 40, top),
            ],
            None,
        ),
        (
            "an answer that carries no response id",
            vec![
                prompt_row("u1", None, T + 20, "go on"),
                response_row("a1", "u1", T + 40, top, None),
            ],
            None,
        ),
        (
            "a request begun before the sign-out and answered after it",
            vec![
                prompt_row("u1", None, T - 20, "go on"),
                answer_row("a1", "u1", T + 40, top),
            ],
            None,
        ),
        ("growth with no answer", Vec::new(), None),
        (
            "a request begun before the upkeep's first look",
            vec![
                prompt_row("u1", None, T + 5, "go on"),
                answer_row("a1", "u1", T + 40, top),
            ],
            None,
        ),
        (
            "an API error in the answer's place",
            vec![
                prompt_row("u1", None, T + 20, "go on"),
                answer_row("a1", "u1", T + 40, "\"isApiErrorMessage\":true,"),
            ],
            None,
        ),
        (
            "a subagent's answer",
            vec![
                prompt_row("u1", None, T + 20, "go on"),
                answer_row("a1", "u1", T + 40, "\"isSidechain\":true,"),
            ],
            None,
        ),
        (
            "an answer to the request before the one in hand",
            vec![
                prompt_row("u1", None, T - 20, "go on"),
                prompt_row("u2", Some("u1"), T + 20, "and then"),
                answer_row("a1", "u1", T + 40, top),
            ],
            None,
        ),
        (
            "a served request a login wall follows",
            vec![
                prompt_row("u1", None, T + 20, "go on"),
                answer_row("a1", "u1", T + 40, top),
                prompt_row("u2", Some("a1"), T + 50, "and then"),
                wall_row("u2", T + 51),
            ],
            None,
        ),
        (
            "a prompt typed after the sign-out and answered",
            vec![
                prompt_row("u1", None, T + 20, "go on"),
                answer_row("a1", "u1", T + 40, top),
            ],
            Some(T + 40),
        ),
        (
            "a tool result written after the sign-out and answered",
            vec![
                prompt_row("u1", None, T - 20, "go on"),
                answer_row("a1", "u1", T - 5, top),
                tool_result_row("u2", "a1", T + 20, "ok"),
                answer_row("a2", "u2", T + 40, top),
            ],
            Some(T + 40),
        ),
    ];
    for (what, rows, lifts) in cases {
        let world = World::seen();
        write_rows(&world.home, &rows);
        world.tap(T + 60, 9_000);
        assert_eq!(
            world.pass(T + 120).0,
            lifts.map_or_else(Vec::new, |at| adopted(LoginEvidence::Served(at))),
            "{what}"
        );
        assert_eq!(world.word(T + 120) == Auth::Ok, lifts.is_some(), "{what}");
    }

    // A transcript read from a cut: a prompt with no response read before it begins nothing,
    // since the cut may hide the response it was written in the middle of. CONTROLS: the same
    // rows of a transcript read whole, and the cut's rows with a response before the prompt.
    let rows = [
        prompt_row("u1", Some("a0"), T + 20, "go on"),
        answer_row("a1", "u1", T + 40, top),
    ];
    let of = |rows: &[String], cut: bool| {
        Conversation::of_rows(rows.iter().map(String::as_bytes), cut).served
    };
    assert_eq!(of(&rows, true), Vec::new(), "the cut hid what came before");
    assert_eq!(of(&rows, false), vec![(T + 20, T + 40)], "read whole");
    let mut after = vec![answer_row("a0", "u0", T + 5, top)];
    after.extend_from_slice(&rows);
    assert_eq!(of(&after, true), vec![(T + 20, T + 40)]);
}

/// A `/login` THAT SUCCEEDED IS THE COMMAND'S OWN ROW, never words that quote it. NEGATIVE
/// CONTROLS, each lifting nothing: a tool's result that quotes the success marker (the
/// review's case: a `grep` over a transcript); a prompt a person typed or pasted that is the
/// marker; a subagent's row; an assistant's answer quoting it; the marker with more text
/// after it; and the marker after another command's row, with no `/login` before it.
/// CONTROLS: the command's row and its output lift the row, as Claude Code 2.1.325 writes them
/// (`local_command` system rows) and as an older build did (user rows of plain text).
#[test]
fn login_adopted_a_login_row_is_the_commands_own_row_and_never_a_quotation() {
    let at = T + 38;
    let user_row = |content: &str| {
        format!(
            "{{\"type\":\"user\",\"isSidechain\":false,\"timestamp\":\"{}\",\"message\":\
             {{\"role\":\"user\",\"content\":{}}}}}",
            stamp(at),
            Value::from(content)
        )
    };
    let login = command_row(at, LOGIN_COMMAND_ROW);
    let cases: [(&str, Vec<String>, bool); 8] = [
        (
            "a tool result quoting the marker",
            vec![login.clone(), tool_result_row("u2", "a1", at, LOGIN_MADE)],
            false,
        ),
        (
            "a typed prompt that is the marker",
            vec![login.clone(), prompt_row("u1", None, at, LOGIN_MADE)],
            false,
        ),
        (
            "a subagent's row",
            vec![
                login.clone(),
                command_row(at, LOGIN_MADE)
                    .replace("\"isSidechain\":false", "\"isSidechain\":true"),
            ],
            false,
        ),
        (
            "an assistant's answer quoting it",
            vec![
                login.clone(),
                answer_row("a1", "u1", at, "\"isSidechain\":false,").replace("done", LOGIN_MADE),
            ],
            false,
        ),
        (
            "the marker with more after it",
            vec![
                login.clone(),
                command_row(at, &format!("{LOGIN_MADE} and more")),
            ],
            false,
        ),
        (
            "the marker after another command",
            vec![
                command_row(at, "<command-name>/status</command-name>"),
                command_row(at, LOGIN_MADE),
            ],
            false,
        ),
        (
            "the command's row and its output",
            vec![login.clone(), command_row(at, LOGIN_MADE)],
            true,
        ),
        (
            "an older build's user rows",
            vec![user_row(LOGIN_COMMAND_ROW), user_row(LOGIN_MADE)],
            true,
        ),
    ];
    for (what, rows, lifts) in cases {
        let world = World::seen();
        write_rows(&world.home, &rows);
        assert_eq!(
            world.pass(T + 120).0,
            if lifts {
                adopted(LoginEvidence::SignedIn(at))
            } else {
                Vec::new()
            },
            "{what}"
        );
        assert_eq!(world.word(T + 120) == Auth::Ok, lifts, "{what}");
    }
}

/// A FILE ITS PROCESS STOPPED WRITING SHOWS NOTHING OF WHAT CAME AFTER. A live tap moves its
/// file's time every ten seconds; a file last written before the evidence's own time is of a
/// process that ended or a tab that moved on, and whoever resumed its conversation is not
/// shown to be it. NEGATIVE CONTROL: a `/login` at `T + 50` in the conversation of a file last
/// written at `T + 30` lifts nothing. CONTROL: the same file written again after it does. A
/// file not written for two minutes keeps no baseline at all: when it is written again its
/// baseline is taken then, and only what comes after that counts.
#[test]
fn login_adopted_a_file_no_longer_written_shows_nothing() {
    let world = World::seen();
    write_login_row(&world.home, T + 50);
    tap_written_at(&world.state, T + 30);
    assert_eq!(
        world.pass(T + 120).0,
        Vec::new(),
        "written before the login"
    );
    assert_eq!(world.word(T + 120), Auth::SignedOut);
    tap_written_at(&world.state, T + 60);
    assert_eq!(
        world.pass(T + 121).0,
        adopted(LoginEvidence::SignedIn(T + 50)),
        "written after it"
    );

    // Quiet for more than two minutes: the baseline goes, and with it what it could show.
    let world = World::seen();
    assert_eq!(Baselines::kept(&world.store().dir).0.len(), 1);
    tap_written_at(&world.state, T + 20);
    assert_eq!(world.pass(T + 20 + 121).0, Vec::new());
    assert_eq!(
        Baselines::kept(&world.store().dir),
        Baselines::default(),
        "a file no longer written keeps no baseline"
    );
    // Written again at `T + 200`: the look at `T + 210` takes its baseline, so a `/login`
    // from before that look is not this process's to show, and one after it is.
    write_login_row(&world.home, T + 150);
    tap_written_at(&world.state, T + 200);
    assert_eq!(world.pass(T + 210).0, Vec::new());
    assert_eq!(
        Baselines::kept(&world.store().dir)
            .0
            .values()
            .map(|base| base.taken_at)
            .collect::<Vec<_>>(),
        vec![T + 210]
    );
    tap_written_at(&world.state, T + 260);
    assert_eq!(
        world.pass(T + 270).0,
        Vec::new(),
        "a row from before the look"
    );
    write_login_row(&world.home, T + 280);
    tap_written_at(&world.state, T + 290);
    assert_eq!(
        world.pass(T + 300).0,
        adopted(LoginEvidence::SignedIn(T + 280))
    );
}

/// ONE RULE SAYS WHICH BASELINE STANDS, for the reading and for the keeper alike
/// ([`Baseline::stands`]): taken after the sign-out and not after now, of the conversation the
/// file still names, and not above the file's API time. NEGATIVE CONTROLS: a baseline dated
/// after now (a clock set back since), and one above the file's API time (a process that
/// began again), each show nothing to the reading, exactly as the keeper takes each again.
#[test]
fn login_adopted_one_rule_says_which_baseline_stands() {
    let now = T + 120;
    let stands = |change: fn(&mut Baseline)| {
        let world = World::seen();
        write_login_row(&world.home, T + 50);
        world.served(T + 95, T + 97, 9_000);
        let store = world.store();
        let mut kept = Baselines::kept(&store.dir);
        let base = kept.0.values_mut().next().unwrap();
        change(base);
        let base = base.clone();
        let held = store.acquire().unwrap();
        kept.keep(&store.dir).unwrap();
        drop(held);
        let records = store
            .read_account_records(Some(&world.roster), now)
            .unwrap();
        let struck = struck_rows(&records.roster, &records.state);
        let witnesses = Witnesses::read(&world.home, &world.state, &store, &struck, now);
        let reading = &witnesses.readings[0];
        let by_rule = base.stands(&reading.reading, T, now);
        assert_eq!(
            witnesses.live_login_here(&struck[0].0, T, now).is_some(),
            by_rule,
            "the reading follows the rule"
        );
        assert_eq!(
            kept.retaken(&witnesses.readings, T, now) == kept,
            by_rule,
            "the keeper follows the rule"
        );
        by_rule
    };
    assert!(stands(|_| {}), "the baseline the first look took");
    assert!(!stands(|base| base.taken_at = T + 500), "dated after now");
    assert!(
        !stands(|base| base.api_ms = 9_500),
        "above the file's API time"
    );
    assert!(!stands(|base| base.taken_at = T), "not after the sign-out");
    assert!(
        !stands(|base| base.session = Some("another".to_owned())),
        "of another conversation"
    );
}

/// A SERVED READING NEEDS GROWTH PAST A BASELINE TAKEN AFTER THE SIGN-OUT, AND A FIGURE. The
/// baseline is on disk, so a restart keeps it: a process that starts between the two looks
/// still adopts. NEGATIVE CONTROLS: with the baseline lost, the request served before the
/// next look is not evidence (that look takes the new floor, and only a request begun after
/// it counts); API time that did not grow adopts nothing; and growth with no rate-limit
/// figure adopts nothing.
#[test]
fn login_adopted_needs_growth_past_a_baseline_kept_on_disk() {
    // Kept: the second look is a new process's, which holds nothing in memory.
    let world = World::seen();
    let kept = Baselines::kept(&world.store().dir);
    assert_eq!(
        kept.0
            .values()
            .map(|b| (b.api_ms, b.taken_at))
            .collect::<Vec<_>>(),
        vec![(4_000, T + 10)]
    );
    world.served(T + 95, T + 97, 9_000);
    assert_eq!(
        world.pass(T + 120).0.len(),
        1,
        "growth past the kept baseline"
    );
    assert_eq!(
        Baselines::kept(&world.store().dir),
        kept,
        "a kept baseline is not moved up"
    );
    assert_eq!(world.pass(T + 180).0, Vec::new());
    assert_eq!(
        Baselines::kept(&world.store().dir),
        Baselines::default(),
        "no sign-out stands, so none is kept"
    );

    // Lost: what happened before the next look is under the new floor.
    let world = World::seen();
    fs::remove_file(baselines_dir(&world.store().dir).join(BASELINES_FILE)).unwrap();
    world.served(T + 95, T + 97, 9_000);
    assert_eq!(
        world.pass(T + 120).0,
        Vec::new(),
        "no baseline, no evidence"
    );
    assert_eq!(world.word(T + 120), Auth::SignedOut);
    world.served(T + 140, T + 150, 9_500);
    assert_eq!(
        world.pass(T + 180).0,
        adopted(LoginEvidence::Served(T + 150)),
        "a request begun after the new floor"
    );

    // Unchanged API time, and growth with no figure.
    for (api_ms, week) in [(4_000, true), (9_000, false)] {
        let world = World::seen();
        write_served_turn(&world.home, T + 95, T + 97);
        write_tap(
            &world.state,
            None,
            Some(tap::Route::Stored),
            (T + 97, api_ms),
            week,
        );
        assert_eq!(world.pass(T + 120).0, Vec::new(), "{api_ms} {week}");
        assert_eq!(world.word(T + 120), Auth::SignedOut, "{api_ms} {week}");
    }
}

/// A `/login` ROW COUNTS ONLY FOR A PROCESS SEEN HOLDING ITS CONVERSATION BEFORE THE ROW WAS
/// WRITTEN: a conversation resumed onto another login carries the rows of the login it ran on
/// before, and those are not this row's. A row written before the upkeep's first look at the
/// process after the sign-out adopts nothing. CONTROL: a row written after that look does.
#[test]
fn login_adopted_a_login_row_counts_only_after_its_process_was_seen_with_the_conversation() {
    let world = World::new(Auth::SignedOut);
    write_profile(&world.home, OWN);
    world.tap(T - 5, 4_000);
    write_login_row(&world.home, T + 5);
    assert_eq!(world.pass(T + 10).0, Vec::new());
    assert_eq!(
        world.pass(T + 120).0,
        Vec::new(),
        "a row from before the look"
    );
    assert_eq!(world.word(T + 120), Auth::SignedOut);
    write_login_row(&world.home, T + 50);
    assert_eq!(
        world.pass(T + 120).0,
        adopted(LoginEvidence::SignedIn(T + 50))
    );
}

/// THE WITNESSES ARE READ PAST THE WINDOW'S ACCOUNTS KEEPER'S TRACE, SO EVERY ASK OF THEM POISONS
/// THE SCOPE ARMED ON ITS THREAD: an adoption step over a row signed out on record asks them, and
/// so does the ladder's ask of that row through the same pass's witnesses, read already; neither
/// run is kept. A step and an ask over a row that is not signed out ask none and compare no
/// clock, so a step handed the reading through `trace::Now` is kept.
///
/// Every such ask gives the witnesses' reason first ([`PAST_THE_TRACE`]), before its asker takes
/// the reading raw.
///
/// NEGATIVE CONTROL (2026-11-16, at the merge of the keeper with FL-174): `Pass::witnesses`
/// without its `trace::poison` failed "a step that asked the witnesses is never kept", the run's
/// first reason then the raw reading's: left `Some(Poison("a raw read of now"))`, right the
/// witnesses' reason. Restored, the file touched.
#[test]
fn login_adopted_every_ask_of_the_witnesses_poisons_the_trace_and_no_sign_out_asks_none() {
    use aterm_uds::trace::{self, Now, Unkept};
    let later = std::time::Duration::from_secs(60);
    let traced = |world: &World, pass: &Pass| {
        trace::read_as_if_later(later, || {
            trace::scope(|| {
                adopt_live_logins(
                    &world.home,
                    &world.state,
                    &world.roster,
                    Now::at(T + 120),
                    pass,
                    &|_| {},
                )
            })
        })
        .1
    };
    let asked = |world: &World, pass: &Pass| {
        let store = world.store();
        let records = store
            .read_account_records(Some(&world.roster), T + 120)
            .unwrap();
        trace::read_as_if_later(later, || {
            trace::scope(|| {
                pass.live(
                    (&world.home, &world.state),
                    &store,
                    (&records.roster, &records.state),
                    LABEL,
                    Now::at(T + 120),
                )
            })
        })
        .1
    };
    let past = Some(Unkept::Poison(PAST_THE_TRACE.into()));
    let world = World::seen();
    let pass = Pass::default();
    assert_eq!(
        traced(&world, &pass).unkept(),
        past,
        "a step that asked the witnesses is never kept"
    );
    assert_eq!(
        asked(&world, &pass).unkept(),
        past,
        "an ask of witnesses the pass read already"
    );

    let world = World::new(Auth::Ok);
    let pass = Pass::default();
    assert_eq!(
        traced(&world, &pass).unkept(),
        None,
        "a step with no row signed out asks no witness"
    );
    assert_eq!(
        asked(&world, &pass).unkept(),
        None,
        "nor an ask of that row"
    );
}

/// A PASS READS ITS WITNESSES ONCE, AND ONLY THE TRANSCRIPTS ITS READINGS NAME, FROM THEIR END.
/// Every asker of one pass is handed the same reading: evidence written after the pass's first
/// ask is the next pass's. A transcript no usage-tap file names is never opened (one that
/// would lift the row sits beside the named one and lifts nothing), a transcript is read from
/// its end alone (evidence before the last four megabytes is not seen, evidence in them is),
/// and what one pass reads is taken off its budget.
#[test]
fn login_adopted_a_pass_reads_its_witnesses_once_and_within_bounds() {
    let live = |world: &World, pass: &Pass, now: i64| {
        let store = world.store();
        let records = store
            .read_account_records(Some(&world.roster), now)
            .unwrap();
        pass.live(
            (&world.home, &world.state),
            &store,
            (&records.roster, &records.state),
            LABEL,
            now,
        )
    };
    // Once per pass.
    let world = World::seen();
    let pass = Pass::default();
    assert_eq!(live(&world, &pass, T + 120), None);
    write_login_row(&world.home, T + 50);
    assert_eq!(
        live(&world, &pass, T + 120),
        None,
        "the pass read its files"
    );
    assert_eq!(
        live(&world, &Pass::default(), T + 120),
        Some(LoginEvidence::SignedIn(T + 50)),
        "the next pass reads them again"
    );
    // A row a step was refused is hidden from no ladder, whatever the files show.
    let mut ranked = vec![(0_u8, struck_row(&world))];
    let store = world.store();
    let records = store
        .read_account_records(Some(&world.roster), T + 120)
        .unwrap();
    let retain = |ranked: &mut Vec<(u8, Row)>, pass: &Pass| {
        retain_not_live(
            ranked,
            (&world.home, &world.state),
            &store,
            (&records.roster, &records.state),
            T + 120,
            pass,
        );
    };
    retain(&mut ranked, &Pass::knowing([LABEL.to_owned()]));
    assert_eq!(ranked.len(), 1, "a refused row stays the ladder's");
    retain(&mut ranked, &Pass::default());
    assert_eq!(ranked, Vec::new(), "a live row is ranked nowhere");

    // Only the transcript a reading names: another conversation's `/login` lifts nothing.
    let world = World::seen();
    let other =
        transcript(&world.home).with_file_name("0f6d2f0e-55a1-4b0f-9d53-0d1c0a8d1d11.jsonl");
    fs::write(
        &other,
        format!(
            "{}\n{}\n",
            command_row(T + 50, LOGIN_COMMAND_ROW),
            command_row(T + 50, LOGIN_MADE)
        ),
    )
    .unwrap();
    assert_eq!(world.pass(T + 120).0, Vec::new());

    // From its end: a `/login` before the last four megabytes is not read; one in them is.
    let world = World::seen();
    write_login_row(&world.home, T + 50);
    let filler = format!(
        "{{\"type\":\"progress\",\"timestamp\":\"{}\",\"data\":\"{}\"}}",
        stamp(T + 55),
        "x".repeat(64 * 1024)
    );
    let rows = usize::try_from(CONVERSATION_TAIL_BYTES).unwrap() / filler.len() + 2;
    write_rows(&world.home, &vec![filler; rows]);
    assert_eq!(world.pass(T + 120).0, Vec::new(), "before the tail");
    write_login_row(&world.home, T + 60);
    assert_eq!(
        world.pass(T + 121).0,
        adopted(LoginEvidence::SignedIn(T + 60)),
        "in the tail"
    );

    // The budget: a read takes what it read off it, and none is left for a read after it.
    let path = transcript(&world.home);
    let mut budget = CONVERSATION_TAIL_BYTES + 10;
    let read = Conversation::read(&path, T, &mut budget).unwrap();
    assert_eq!(read.logins, vec![T + 60]);
    assert_eq!(budget, 10);
    let read = Conversation::read(&path, T, &mut budget).unwrap();
    assert_eq!((read, budget), (Conversation::default(), 0));
    // A transcript not written since the floor is not read at all.
    let mut budget = PASS_BYTES;
    let later = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap()
        + 3_600;
    assert_eq!(
        Conversation::read(&path, later, &mut budget),
        Some(Conversation::default())
    );
    assert_eq!(budget, PASS_BYTES, "nothing was read");
}

/// THE TRANSCRIPTS A PASS READS ARE CHOSEN BY THE TRANSCRIPT'S OWN TIME, NOT THE USAGE-TAP
/// FILE'S: every live tab's tap moves its file's time at each refresh, idle or not, so that
/// time says nothing of which tab a person used. Nine live tabs share the default login, one
/// more than a pass reads, and every one has a standing baseline; the person types `/login`
/// in the one whose usage-tap file is the least recently written of the nine. Its transcript
/// is the only one written since the baselines, so it is the one read (NEGATIVE CONTROL: a
/// choice by the usage-tap file's time reads the other eight and lifts nothing), and the row
/// is lifted. With every transcript written since, the eight most recently written are read
/// and no more.
#[test]
fn login_adopted_the_tab_a_person_used_is_read_however_many_idle_tabs_share_its_login() {
    let world = World::new(Auth::SignedOut);
    write_profile(&world.home, OWN);
    let ends = ["00", "01", "03", "07", "0b", "0d", "10", "11", "13"];
    assert_eq!(ends.len(), MOST_CONVERSATIONS + 1);
    let tabs: Vec<(String, u32, String)> = (70_000..)
        .zip(ends)
        .map(|(pid, end)| {
            (
                format!("td1b73bd0{end}"),
                pid,
                format!("6cf4169e-20c7-4bcb-a97f-e8a1387ed6{end}"),
            )
        })
        .collect();
    let (used, idle) = tabs.split_last().unwrap();
    let taps: Vec<PathBuf> = tabs
        .iter()
        .map(|(sid, pid, conversation)| {
            // Each tab's transcript as it stood before the sign-out.
            let transcript = transcript_of(&world.home, conversation);
            write_rows_to(&transcript, &[prompt_row("u0", None, T - 60, "go on")]);
            written_at(&transcript, T - 60);
            write_tap_of(
                &world.state,
                (sid, *pid, conversation),
                None,
                Some(tap::Route::Stored),
                (T - 5, 4_000),
                true,
            )
        })
        .collect();
    assert_eq!(world.pass(T + 10).0, Vec::new(), "nothing is newer yet");
    assert_eq!(
        Baselines::kept(&world.store().dir).0.len(),
        tabs.len(),
        "every tab has a standing baseline"
    );
    // The person's `/login` in the last tab; the taps of the idle ones were refreshed after
    // its own.
    let used_transcript = transcript_of(&world.home, &used.2);
    write_rows_to(
        &used_transcript,
        &[
            command_row(T + 50, LOGIN_COMMAND_ROW),
            command_row(T + 50, LOGIN_MADE),
        ],
    );
    written_at(&used_transcript, T + 50);
    let (used_tap, idle_taps) = taps.split_last().unwrap();
    written_at(used_tap, T + 112);
    for tap in idle_taps {
        written_at(tap, T + 115);
    }
    let read = |now: i64| {
        let store = world.store();
        let records = store
            .read_account_records(Some(&world.roster), now)
            .unwrap();
        let struck = struck_rows(&records.roster, &records.state);
        Witnesses::read(&world.home, &world.state, &store, &struck, now)
            .conversations
            .into_keys()
            .collect::<Vec<_>>()
    };
    assert_eq!(
        read(T + 120),
        vec![used.2.clone()],
        "the one transcript written since the baselines, and no idle tab's"
    );
    // Every transcript written since: the eight most recently written, and no more.
    for (n, (_, _, conversation)) in idle.iter().enumerate() {
        let transcript = transcript_of(&world.home, conversation);
        written_at(&transcript, T + 20 + i64::try_from(n).unwrap());
    }
    let mut newest: Vec<String> = idle[1..].iter().map(|tab| tab.2.clone()).collect();
    newest.push(used.2.clone());
    newest.sort();
    assert_eq!(read(T + 120), newest, "the least recently written is left");
    assert_eq!(
        world.pass(T + 120).0,
        adopted(LoginEvidence::SignedIn(T + 50)),
        "the tab the person used is read, and its row lifted"
    );
}

/// The fixture row as the records hold it.
fn struck_row(world: &World) -> Row {
    world
        .store()
        .read_account_records(Some(&world.roster), T + 120)
        .unwrap()
        .roster
        .row(LABEL)
        .unwrap()
        .clone()
}

/// THE WRITER IS IDEMPOTENT BY THE ORDER OF ITS TIMES, so two aterm processes that fold the same
/// evidence agree: the first lift stamps the word at the evidence's own time, the second finds
/// the row no longer signed out, and a sign-out recorded after the evidence stands against it.
/// The lift is a journal note the lab is told. NEGATIVE CONTROL: evidence dated at the sign-out
/// itself, and evidence dated after now, lift nothing.
#[test]
fn login_adopted_writer_agrees_with_itself_and_notes_the_journal() {
    let row = Row {
        label: "w3".to_owned(),
        email: Some(OWN.to_owned()),
        ..Row::default()
    };
    let struck = || {
        let mut state = State::default();
        let mut logins = accounts::Logins::default();
        let _ = accounts::note_session(&mut state, "s-w3", "w3", None, T);
        let _ = accounts::note_auth(&mut state, "w3", Auth::SignedOut, T);
        accounts::note_sign_in_task(&mut logins, "w3", accounts::SignInAction::Renew, T);
        state.accounts.get_mut("w3").unwrap().pk = Some("ac-0123456789abcdef".to_owned());
        (state, logins)
    };
    for evidence in [
        LoginEvidence::Served(T + 97),
        LoginEvidence::SignedIn(T + 37),
    ] {
        let (mut state, mut logins) = struck();
        let before = state.clone();
        let mut events =
            accounts::adopt_login(&mut state, &mut logins, &row, evidence, T + 120).unwrap();
        assert_eq!(state.account("w3").auth, Auth::Ok);
        assert_eq!(state.account("w3").auth_at, Some(evidence.at()));
        assert!(!accounts::signed_out_here(&row, &state));
        assert!(logins.get("w3").is_none_or(|b| b.sign_in_task.is_none()));
        let notes = crate::harness::journal::observe(
            Cli::Claude,
            &before,
            &mut state.clone(),
            &mut events,
            T + 120,
        );
        assert!(
            notes.iter().any(|note| note.kind == "signed-in"),
            "{evidence:?}: {:?}",
            notes.iter().map(|note| note.kind).collect::<Vec<_>>()
        );
        // The same evidence again, from the same record another process read.
        let (mut again, mut again_logins) = struck();
        accounts::adopt_login(&mut again, &mut again_logins, &row, evidence, T + 300).unwrap();
        assert_eq!(again.account("w3").auth_at, state.account("w3").auth_at);
        assert_eq!(
            accounts::adopt_login(&mut state, &mut logins, &row, evidence, T + 300),
            Err(accounts::NotAdopted::NotSignedOut)
        );
        // A sign-out recorded after the evidence stands.
        let _ = accounts::note_auth(&mut state, "w3", Auth::SignedOut, T + 200);
        assert_eq!(
            accounts::adopt_login(&mut state, &mut logins, &row, evidence, T + 300),
            Err(accounts::NotAdopted::NotAfter(T + 200))
        );
    }
    let (mut state, mut logins) = struck();
    assert_eq!(
        accounts::adopt_login(
            &mut state,
            &mut logins,
            &row,
            LoginEvidence::Served(T),
            T + 120
        ),
        Err(accounts::NotAdopted::NotAfter(T))
    );
    assert_eq!(
        accounts::adopt_login(
            &mut state,
            &mut logins,
            &row,
            LoginEvidence::Served(T + 121),
            T + 120
        ),
        Err(accounts::NotAdopted::Future)
    );
    assert_eq!(state.account("w3").auth, Auth::SignedOut);
}

/// ON WINDOWS KEEPING THE BASELINES LEAVES THE STORE'S OTHER ENTRIES AS THEY WERE: the store's
/// folder there keeps the list it inherits, and the baselines go into a private folder of their
/// own made at its birth ([`baselines_dir`]), so a keep rewrites no access list of the store, of a
/// login's folder or of a file in it, and moves no file's stamp, which every watcher of the
/// store reads. The store's folder here carries one entry more than a private list holds (the
/// user's read, passed on), so a list passed on to its entries would show. NEGATIVE CONTROL: a
/// keep that makes the store's folder itself private (`baselines_dir` answering the store's own)
/// rewrites the login's file: its access list and its stamp move.
#[cfg(windows)]
#[test]
fn on_windows_keeping_the_baselines_leaves_the_stores_other_entries_as_they_were() {
    use aterm_uds::acl::{Descriptor, Parts, Sid};
    use aterm_uds::file_watch::FileStamp;
    let scratch = aterm_tempfile::TempDir::new().unwrap();
    let store = scratch.path().join("accounts");
    fs::create_dir_all(&store).unwrap();
    let user = Sid::token_user().unwrap();
    Descriptor::from_sddl(&format!("D:(A;OICI;FR;;;{user})"))
        .unwrap()
        .apply_to_path(&store, Parts::DACL)
        .unwrap();
    let login = store.join("logins").join(LABEL);
    fs::create_dir_all(&login).unwrap();
    let transcript = login.join("transcript.jsonl");
    fs::write(&transcript, b"{}\n").unwrap();
    let list = |path: &Path| {
        Descriptor::of_path(path, Parts::OWNER | Parts::DACL)
            .and_then(|d| d.to_sddl(Parts::OWNER | Parts::DACL))
            .unwrap()
    };
    let before: Vec<String> = [&store, &login, &transcript]
        .iter()
        .map(|path| list(path))
        .collect();
    let stamped = FileStamp::of(&transcript).expect("a stamp");

    let baselines = Baselines(BTreeMap::from([(
        format!("{SID}-{PID}"),
        Baseline {
            api_ms: 4_000,
            taken_at: T + 10,
            session: None,
        },
    )]));
    baselines.keep(&store).expect("the baselines are kept");
    assert_eq!(Baselines::kept(&store), baselines);

    let after: Vec<String> = [&store, &login, &transcript]
        .iter()
        .map(|path| list(path))
        .collect();
    assert_eq!(
        (after, FileStamp::of(&transcript)),
        (before, Some(stamped)),
        "no access list of the store's entries moved, and the login's file keeps its stamp"
    );
}

/// FL-116: A SERVED TURN IS DATED BY THE REQUEST ITS REPLY ANSWERED ([`served_request`]), so a
/// limit met while the reply streamed is not one it speaks against. A tool's result written at
/// `T`, after a response's tool call, answered by another response whose row is written ninety
/// seconds later, is served as asked at `T`, by the model that answer names; a prompt typed
/// after it starts a new request that nothing has answered yet.
/// CONTROLS: the reply that is a later row of the response in flight when the tool's result was
/// written (one API response that ran its tool while it streamed) shows no request served; nor
/// does a tail whose last request no assistant row read precedes (the read's cut hid the
/// response before it), an answer with no `message.id`, or one Claude Code wrote itself
/// (`<synthetic>`).
/// NEGATIVE CONTROL: dated by its reply's row, the first case gives `T + 90`; with the
/// in-flight rule dropped, the second gives `Some`.
#[test]
fn a_served_request_is_dated_by_its_request_and_never_by_its_reply() {
    let top = "\"isSidechain\":false,";
    let tail = |rows: &[String]| {
        rows.iter()
            .map(|row| format!("{row}\n"))
            .collect::<String>()
    };
    let asked = tail(&[
        response_row("a-call", "u-0", T - 30, top, Some("msg_call")),
        tool_result_row("u-result", "a-call", T, "ok"),
        response_row("a-reply", "u-result", T + 90, top, Some("msg_reply")),
    ]);
    assert_eq!(
        served_request(&asked),
        Some(ServedRequest {
            asked: T,
            model: "claude-opus-4-7".to_string(),
        })
    );
    let pending = format!(
        "{asked}{}\n",
        prompt_row("u-next", Some("a-reply"), T + 100, "next")
    );
    assert_eq!(
        served_request(&pending),
        None,
        "a request nothing answered yet"
    );

    let in_flight = tail(&[
        response_row("a-call", "u-0", T - 30, top, Some("msg_call")),
        tool_result_row("u-result", "a-call", T, "ok"),
        response_row("a-more", "u-result", T + 90, top, Some("msg_call")),
    ]);
    assert_eq!(
        served_request(&in_flight),
        None,
        "a row of the response in flight"
    );
    let cut = tail(&[
        tool_result_row("u-result", "a-call", T, "ok"),
        response_row("a-reply", "u-result", T + 90, top, Some("msg_reply")),
    ]);
    assert_eq!(
        served_request(&cut),
        None,
        "no assistant row read before the request"
    );
    let unnamed = tail(&[
        response_row("a-call", "u-0", T - 30, top, Some("msg_call")),
        tool_result_row("u-result", "a-call", T, "ok"),
        response_row("a-reply", "u-result", T + 90, top, None),
    ]);
    assert_eq!(
        served_request(&unnamed),
        None,
        "an answer with no response id"
    );
    let synthetic = tail(&[
        response_row("a-call", "u-0", T - 30, top, Some("msg_call")),
        tool_result_row("u-result", "a-call", T, "ok"),
        response_row("a-reply", "u-result", T + 90, top, Some("msg_reply"))
            .replace("claude-opus-4-7", accounts::SYNTHETIC_MODEL),
    ]);
    assert_eq!(
        served_request(&synthetic),
        None,
        "a row Claude Code wrote itself"
    );
}
