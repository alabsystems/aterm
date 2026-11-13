// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::requests_host::set_dir_for_test;
use crate::session_store::{SessionHandle, new_store, test_handle};
use aterm_requests::{Form, From, Intake, Via, why_of};
use std::sync::Mutex;
use std::time::Duration;

/// 2026-11-09 14:11:00 UTC.
const AT: u64 = 1_794_233_460_000;
const NODE: &str = "87be5228c0ffee0087be5228c0ffee00";
const OTHER: &str = "c0ffee00c0ffee00c0ffee00c0ffee00";
const ASKER: &str = "s-0123456789abcdef0123";
const BY: &str = "fleet:87be5228:s-0123456789abcdef0123";
/// A hang detector for the requests' writer thread.
const MUST: Duration = Duration::from_secs(60);

/// This test thread's requests in a folder of its own until dropped.
struct Here(aterm_tempfile::TempDir);

impl Here {
    fn new() -> Self {
        let dir = aterm_tempfile::tempdir().expect("a folder");
        set_dir_for_test(Some(dir.path().to_path_buf()));
        Self(dir)
    }

    fn store(&self) -> aterm_requests::Store {
        aterm_requests::Store::open(self.0.path()).expect("the store")
    }

    fn journal(&self) -> String {
        std::fs::read_to_string(self.0.path().join(aterm_requests::JOURNAL)).unwrap_or_default()
    }
}

impl Drop for Here {
    fn drop(&mut self) {
        assert!(
            crate::requests_host::flush_within(MUST),
            "the writer flushed"
        );
        crate::requests_host::forget_for_test();
        set_dir_for_test(None);
    }
}

/// A drive's plain shell request from `node`'s `by`, its why `why`.
fn fleet_shell(node: &str, by: &str, why: &str) -> Intake {
    Intake {
        via: Via::FleetShell,
        form: Form::Shell,
        from: From {
            node: node.to_owned(),
            host: "m114".to_owned(),
            by: by.to_owned(),
            program: Some("claude".to_owned()),
            model: None,
            account: None,
        },
        why: why_of(Via::FleetShell, Some(why), None, None, None),
        brief: None,
        goal: None,
        asked: None,
    }
}

/// A local agent tab's spawn, its why the tab's goal (a fallback, content tier).
fn local_spawn(goal: &str) -> Intake {
    Intake {
        via: Via::CtlSpawn,
        form: Form::Agent("claude".to_owned()),
        from: From {
            node: String::new(),
            host: "this-mac".to_owned(),
            by: ASKER.to_owned(),
            program: Some("claude".to_owned()),
            model: None,
            account: None,
        },
        why: why_of(Via::CtlSpawn, None, None, Some(goal), None),
        brief: None,
        goal: Some(goal.to_owned()),
        asked: Some("the asked line".to_owned()),
    }
}

/// Request `intake` opened at `at` with tab `tab`, left (concluded) where `left` is its shell's
/// exit code.
fn request(
    store: &mut aterm_requests::Store,
    at: u64,
    intake: Intake,
    tab: &str,
    left: Option<i32>,
) -> String {
    let by = intake.from.by.clone();
    let node = intake.from.node.clone();
    let id = store.open_request(at, intake).expect("opened");
    store
        .tab(at + 1, &id, tab, &by, false, None)
        .expect("a tab");
    if let Some(code) = left {
        store
            .left(
                at + 2,
                tab,
                &aterm_requests::Left {
                    reason: "shell-exit".to_owned(),
                    code: Some(code),
                    task: None,
                    by: by.clone(),
                    node: Some(node),
                    spent: None,
                },
            )
            .expect("left");
    }
    id
}

/// A client in no tab: `ctl`.
fn no_client() -> crate::typed_by::Who {
    crate::typed_by::Who::client_with(None, 1, &|_| None, &|_| None, &|_| None)
}

/// What the stand-in main thread and fabric were asked.
#[derive(Default)]
struct Fake {
    kept: Mutex<Vec<(Vec<u64>, String)>>,
    posts: Mutex<Vec<(String, String, String, String)>>,
    unsent: Option<Unsent>,
}

impl Fake {
    fn run(
        &self,
        client: &dyn Fn() -> crate::typed_by::Who,
        verb: &str,
        rest: &str,
        store: &Store,
    ) -> String {
        let keep_tabs = |sessions: Vec<u64>, by: ExitActor| {
            let n = sessions.len();
            self.kept
                .lock()
                .unwrap()
                .push((sessions, by.as_wire().to_owned()));
            Ok(n)
        };
        let post = |from: &crate::SessionCtx, to: To<'_>, key: &str, text: &str| {
            let to = match to {
                To::Here(sid) => format!("@{sid}"),
                To::There { sid, node } => format!("@{sid}@{node}"),
            };
            self.posts.lock().unwrap().push((
                from.self_id.as_str().to_owned(),
                to,
                key.to_owned(),
                text.to_owned(),
            ));
            match &self.unsent {
                Some(unsent) => Err(unsent.clone()),
                None => Ok("OK f1".to_owned()),
            }
        };
        let places = Places::default();
        cmd_as(
            verb,
            rest,
            Scope::Owner,
            store,
            &Hands {
                client,
                keep_tabs: &keep_tabs,
                post: &post,
                places: &places,
            },
        )
    }
}

fn drive(node: &str) -> crate::fabric::DrivingAs {
    crate::fabric::DrivingAs::enter(crate::fabric::Driver {
        node: node.to_owned(),
        host: "m114".to_owned(),
    })
}

/// Each row of a reply after its `OK <n>`, its first word and its fields.
fn rows(reply: &str) -> Vec<(String, std::collections::HashMap<String, String>)> {
    let mut lines = reply.lines();
    let head = lines.next().unwrap_or_default();
    let n: usize = head
        .strip_prefix("OK ")
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("{reply}"));
    let rows: Vec<_> = lines
        .map(|line| {
            let mut words = line.split(' ');
            let kind = words.next().unwrap_or_default().to_owned();
            let fields = words
                .filter_map(|word| word.split_once('='))
                .map(|(k, v)| (k.to_owned(), aterm_codec::pct::decode(v)))
                .collect();
            (kind, fields)
        })
        .collect();
    assert_eq!(rows.len(), n, "{reply}");
    rows
}

/// The `id=` of each `request` row.
fn ids(reply: &str) -> Vec<String> {
    rows(reply)
        .into_iter()
        .filter(|(kind, _)| kind == "request")
        .map(|(_, fields)| fields["id"].clone())
        .collect()
}

#[test]
fn requests_lists_newest_first_and_filters_state_and_from() {
    let here = Here::new();
    let sessions = new_store();
    let fake = Fake::default();
    let mut store = here.store();
    let (a, b, c) = (test_handle(1), test_handle(2), test_handle(3));
    let old = request(
        &mut store,
        AT,
        fleet_shell(NODE, BY, "the oldest"),
        a.sid.as_str(),
        Some(0),
    );
    let running = request(
        &mut store,
        AT + 60_000,
        fleet_shell(NODE, BY, "still running"),
        b.sid.as_str(),
        None,
    );
    let other = request(
        &mut store,
        AT + 120_000,
        fleet_shell(OTHER, "fleet:c0ffee00:-", "another mac's"),
        c.sid.as_str(),
        Some(1),
    );
    let all = fake.run(&no_client, "requests", "", &sessions);
    assert_eq!(
        ids(&all),
        [other.as_str(), running.as_str(), old.as_str()],
        "newest first: {all}"
    );
    let header = &rows(&all)[0].1;
    assert_eq!(
        (
            header["total"].as_str(),
            header["unreviewed"].as_str(),
            header["shown"].as_str()
        ),
        ("3", "2", "3")
    );
    let row = &rows(&all)[3].1;
    assert_eq!(row["stage"], "concluded");
    assert_eq!(row["out"], "done");
    assert_eq!(row["why"], "the oldest");
    assert_eq!(row["why_src"], "given");
    assert_eq!(row["host"], "m114");
    assert_eq!(
        ids(&fake.run(&no_client, "requests", "state=unreviewed", &sessions)),
        [other.as_str(), old.as_str()]
    );
    assert_eq!(
        ids(&fake.run(&no_client, "requests", "state=running", &sessions)),
        [running.as_str()]
    );
    for from in ["87be5228", NODE, BY, ASKER] {
        assert_eq!(
            ids(&fake.run(
                &no_client,
                "requests",
                &format!("from={from} n=1"),
                &sessions
            )),
            [running.as_str()],
            "{from}"
        );
    }
    // NEGATIVE: a node no request came from lists none, and a word the grammar lacks is refused.
    assert!(ids(&fake.run(&no_client, "requests", "from=ffffffff", &sessions)).is_empty());
    assert_eq!(
        fake.run(&no_client, "requests", "state=cleaned", &sessions),
        LIST_USAGE
    );
    assert_eq!(
        fake.run(&no_client, "requests", "n=513", &sessions),
        LIST_USAGE
    );
    let json = fake.run(&no_client, "requests", "--json", &sessions);
    assert!(
        json.starts_with("OK 1\n{\"requests\":{\"total\":\"3\""),
        "{json}"
    );
    assert!(json.contains(&format!("\"id\":\"{old}\"")), "{json}");
}

#[test]
fn show_prints_tabs_claims_with_keep_words_and_timeline() {
    let here = Here::new();
    let sessions = new_store();
    let fake = Fake::default();
    let mut store = here.store();
    let tab = test_handle(1);
    // A claim is an absolute path in its normal form, which `/scratch/work` is only on Unix.
    let work = aterm_requests::claim::normal(if cfg!(windows) {
        "C:/scratch/work"
    } else {
        "/scratch/work"
    })
    .expect("a claimable path");
    let id = request(
        &mut store,
        AT,
        local_spawn("the requesting tab's goal"),
        tab.sid.as_str(),
        Some(3),
    );
    store
        .born(
            AT + 3,
            &[aterm_requests::Born {
                id: id.clone(),
                path: work.clone(),
                src: aterm_requests::ClaimSrc::Born,
                tab: Some(tab.sid.as_str().to_owned()),
                born_ms: AT + 1,
                alone: Some(true),
            }],
        )
        .expect("claimed");
    store
        .review(AT + 4, &id, "ctl", Some("seen"))
        .expect("reviewed");
    let shown = fake.run(&no_client, "request", &format!("show {id}"), &sessions);
    let rows = rows(&shown);
    let kinds: Vec<&str> = rows.iter().map(|(kind, _)| kind.as_str()).collect();
    assert_eq!(
        &kinds[..6],
        ["request", "page", "said", "tab", "claim", "review"],
        "{shown}"
    );
    let (_, request) = &rows[0];
    assert_eq!(
        request["why"], "the requesting tab's goal",
        "an owner reads the fallback"
    );
    assert_eq!(request["why_src"], "goal");
    assert_eq!(request["stage"], "reviewed");
    assert_eq!(rows[2].1["asked"], "the asked line");
    let tab_row = &rows[3].1;
    assert_eq!(
        (
            tab_row["left"].as_str(),
            tab_row["reason"].as_str(),
            tab_row["code"].as_str()
        ),
        ("failed", "shell-exit", "3")
    );
    assert_eq!(rows[4].1["path"], work);
    assert_eq!(
        rows[4].1["keep"], "unmeasured",
        "no measure has read it yet"
    );
    assert_eq!(rows[5].1["note"], "seen");
    let timeline: Vec<&str> = rows
        .iter()
        .filter(|(kind, _)| kind == "timeline")
        .map(|(_, fields)| fields["what"].as_str())
        .collect();
    assert_eq!(
        timeline,
        ["open", "tab", "left", "end", "claim", "review"],
        "{shown}"
    );
    let json = fake.run(
        &no_client,
        "request",
        &format!("show {id} --json"),
        &sessions,
    );
    assert!(json.starts_with("OK 1\n{\"request\":{"), "{json}");
    assert!(json.contains("\"tabs\":[{"), "{json}");
    // NEGATIVE: an id this Mac never minted, and one it holds no request of.
    assert!(
        fake.run(&no_client, "request", "show r-1", &sessions)
            .starts_with("ERR request show r-1: ")
    );
    assert_eq!(
        fake.run(
            &no_client,
            "request",
            "show r-20261109-1411-87be5228-09",
            &sessions
        ),
        "ERR request show: no request of that id on this Mac\n"
    );
}

#[test]
fn a_drive_reads_only_its_own_requests_and_no_said() {
    let here = Here::new();
    let sessions = new_store();
    let fake = Fake::default();
    let mut store = here.store();
    let (a, b) = (test_handle(1), test_handle(2));
    let mine = request(
        &mut store,
        AT,
        fleet_shell(NODE, BY, "the drive's own"),
        a.sid.as_str(),
        Some(0),
    );
    let local = request(
        &mut store,
        AT + 1,
        local_spawn("a goal only this Mac's owner reads"),
        b.sid.as_str(),
        Some(0),
    );
    let _driving = drive(NODE);
    let listed = fake.run(&no_client, "requests", "", &sessions);
    assert_eq!(ids(&listed), [mine.as_str()], "{listed}");
    assert_eq!(rows(&listed)[0].1["total"], "1");
    let shown = fake.run(&no_client, "request", &format!("show {mine}"), &sessions);
    assert!(
        !shown.lines().any(|line| line.starts_with("said ")),
        "{shown}"
    );
    // NEGATIVE: another node's request reads as unknown, and its goal never appears.
    let foreign = fake.run(&no_client, "request", &format!("show {local}"), &sessions);
    assert_eq!(
        foreign,
        "ERR request show: no request of that id on this Mac\n"
    );
    assert!(!listed.contains("goal"), "{listed}");
    drop(_driving);
    // An owner reads both, and a fallback why only an owner reads.
    let owner = fake.run(&no_client, "requests", "", &sessions);
    assert_eq!(ids(&owner), [local.as_str(), mine.as_str()]);
    {
        let _driving = drive(NODE);
        let mut store = here.store();
        let local_row = request(
            &mut store,
            AT + 2,
            Intake {
                from: From {
                    node: NODE.to_owned(),
                    ..local_spawn("x").from
                },
                ..local_spawn("a fallback goal the drive never reads")
            },
            test_handle(3).sid.as_str(),
            None,
        );
        let listed = fake.run(&no_client, "requests", "", &sessions);
        let row = rows(&listed)
            .into_iter()
            .find(|(_, fields)| fields.get("id") == Some(&local_row))
            .expect("its own node's request")
            .1;
        assert_eq!(row["why"], "1", "a fallback why is content tier: {listed}");
    }
}

/// `sid`'s tab, its shell process `pid`, registered; and a client run inside it.
fn shell_tab(
    sessions: &Store,
    local: u64,
    pid: i32,
) -> (SessionHandle, impl Fn() -> crate::typed_by::Who) {
    let mut tab = test_handle(local);
    tab.shell_identity.pid = pid;
    sessions.write().unwrap().register(tab.clone());
    let child = u32::try_from(pid).unwrap() + 1;
    let client = move || {
        crate::typed_by::Who::client_with(
            Some(child),
            1,
            &move |p| (p == i32::try_from(child).unwrap()).then_some(pid),
            &|_| None,
            &|_| Some("aterm".to_string()),
        )
    };
    (tab, client)
}

/// Make an agent run in `tab`, as its timeline reads one.
fn agent_in(tab: &SessionHandle) {
    let mut timeline = tab.ctx.timeline.lock().unwrap();
    let pgid = timeline.agent().program_pgid;
    timeline.set_program(pgid, Some("claude".to_owned()));
    timeline.publish_agent(
        "idle",
        None,
        None,
        Some(aterm_phase::Program::Claude),
        crate::session_timeline::AgentStamp {
            generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
            fp: 1,
        },
    );
}

#[test]
fn review_keep_reply_refuse_an_agent_tab_the_harness_and_a_drive() {
    let here = Here::new();
    let sessions = new_store();
    let fake = Fake::default();
    let mut store = here.store();
    let work = test_handle(9);
    let id = request(
        &mut store,
        AT,
        fleet_shell(NODE, BY, "judge me"),
        work.sid.as_str(),
        Some(0),
    );
    let (agent_tab, in_agent) = shell_tab(&sessions, 1, 6100);
    agent_in(&agent_tab);
    let before = here.journal();
    for act in [
        format!("review {id}"),
        format!("keep {id}"),
        format!("reply {id} -- hello"),
    ] {
        let verb = act.split(' ').next().unwrap();
        let refused = fake.run(&in_agent, "request", &act, &sessions);
        assert!(
            refused.starts_with(&format!("ERR request {verb}: a request is reviewed by"))
                && refused.contains("never an agent"),
            "{refused}"
        );
        {
            let _harness =
                crate::control::ServingPrincipal::enter(crate::control::Principal::Harness);
            let refused = fake.run(&no_client, "request", &act, &sessions);
            assert!(refused.contains("never aterm's harness"), "{refused}");
        }
        {
            let _driving = drive(NODE);
            let refused = fake.run(&no_client, "request", &act, &sessions);
            assert!(refused.contains("never a fleet drive"), "{refused}");
        }
    }
    assert_eq!(here.journal(), before, "a refused act writes nothing");
    assert!(fake.posts.lock().unwrap().is_empty());
    // NEGATIVE: a person's shell tab, and a ctl in no tab, are admitted.
    let (person, in_shell) = shell_tab(&sessions, 2, 6200);
    assert_eq!(
        fake.run(
            &in_shell,
            "request",
            &format!("review {id} note=fine"),
            &sessions
        ),
        format!("OK reviewed req={id}\n")
    );
    assert_eq!(
        fake.run(&no_client, "request", &format!("review {id}"), &sessions),
        format!("OK already req={id}\n")
    );
    let review = here.store().get(&id).unwrap().review.clone().unwrap();
    assert_eq!(review.by, person.sid.as_str());
    assert_eq!(review.note.as_deref(), Some("fine"));
}

#[test]
fn review_keep_reply_refuse_a_shell_tab_a_request_owns_spawn_started_or_a_client_typed_into() {
    let here = Here::new();
    let sessions = new_store();
    let fake = Fake::default();
    let mut store = here.store();
    let work = test_handle(9);
    let id = request(
        &mut store,
        AT,
        fleet_shell(NODE, BY, "judge me"),
        work.sid.as_str(),
        Some(0),
    );
    // A drive's fleet-shell tab: a plain shell with no agent, which the drive types into.
    let (drives, in_drives) = shell_tab(&sessions, 3, 6300);
    crate::requests_host::opened_for_test(
        true,
        NODE,
        "m114",
        BY,
        crate::requests_host::Made::Shell,
        drives.sid.as_str(),
    );
    // A tab a local agent's `ctl spawn` started.
    let (spawned, in_spawned) = shell_tab(&sessions, 4, 6400);
    spawned.ctx.spawn_mark.ctl();
    // A person's tab an agent's `@<sid> send` typed into.
    let (typed, in_typed) = shell_tab(&sessions, 5, 6500);
    typed
        .ctx
        .typed
        .admit(&crate::typed_by::Caller::new(crate::typed_by::Who::Word(
            "an agent".to_owned(),
        )));
    let before = here.journal();
    let callers: [(&dyn Fn() -> crate::typed_by::Who, &str); 3] = [
        (&in_drives, "never a tab a request owns"),
        (&in_spawned, "never a tab ctl spawn started"),
        (&in_typed, "never a tab a control client typed into"),
    ];
    for (client, never) in callers {
        for act in [
            format!("review {id}"),
            format!("keep {id}"),
            format!("reply {id} -- hello"),
        ] {
            let refused = fake.run(client, "request", &act, &sessions);
            assert!(refused.contains(never), "{refused}");
        }
    }
    assert_eq!(here.journal(), before, "a refused act writes nothing");
    assert!(fake.posts.lock().unwrap().is_empty());
    assert!(fake.kept.lock().unwrap().is_empty());
    // NEGATIVE: aterm's own typing (the harness's lines) leaves a person's tab theirs.
    let (person, in_person) = shell_tab(&sessions, 6, 6600);
    person
        .ctx
        .typed
        .admit(&crate::typed_by::Caller::new(crate::typed_by::Who::Aterm));
    assert_eq!(
        fake.run(&in_person, "request", &format!("review {id}"), &sessions),
        format!("OK reviewed req={id}\n")
    );
}

#[test]
fn review_refuses_a_running_request_and_keep_keeps_its_open_tabs() {
    let here = Here::new();
    let sessions = new_store();
    let fake = Fake::default();
    let mut store = here.store();
    let tab = test_handle(4);
    let local = tab.local_id;
    sessions.write().unwrap().register(tab.clone());
    let id = request(
        &mut store,
        AT,
        fleet_shell(NODE, BY, "running"),
        tab.sid.as_str(),
        None,
    );
    assert_eq!(
        fake.run(&no_client, "request", &format!("review {id}"), &sessions),
        "ERR request review: the request has not concluded, and a review judges how it came out\n"
    );
    assert_eq!(
        fake.run(&no_client, "request", &format!("keep {id}"), &sessions),
        format!("OK kept req={id} tabs=1\n")
    );
    assert_eq!(
        *fake.kept.lock().unwrap(),
        [(vec![local], "ctl".to_owned())],
        "its open tab goes to the main thread, by the keeper"
    );
    assert_eq!(
        here.store()
            .get(&id)
            .unwrap()
            .kept
            .as_ref()
            .map(|(by, _)| by.as_str()),
        Some("ctl")
    );
    assert!(
        fake.run(&no_client, "request", &format!("keep {id}"), &sessions)
            .starts_with(&format!("OK already req={id}"))
    );
}

#[test]
fn reply_mails_the_requester_there_and_here_through_report_post() {
    let here = Here::new();
    let sessions = new_store();
    let mut store = here.store();
    let (fleet_tab, local_tab, silent_tab) = (test_handle(1), test_handle(2), test_handle(3));
    for tab in [&fleet_tab, &local_tab, &silent_tab] {
        sessions.write().unwrap().register(tab.clone());
    }
    let there = request(
        &mut store,
        AT,
        fleet_shell(NODE, BY, "there"),
        fleet_tab.sid.as_str(),
        None,
    );
    let local = request(
        &mut store,
        AT + 1,
        local_spawn("here"),
        local_tab.sid.as_str(),
        None,
    );
    let silent = request(
        &mut store,
        AT + 2,
        fleet_shell(NODE, "fleet:87be5228:-", "named no session"),
        silent_tab.sid.as_str(),
        None,
    );
    let fake = Fake::default();
    let answer = fake.run(
        &no_client,
        "request",
        &format!("reply {there} -- thanks, cleaning it up"),
        &sessions,
    );
    assert_eq!(
        answer,
        format!("OK replied req={there} to=@{ASKER}@{NODE} OK f1\n")
    );
    let answer = fake.run(
        &no_client,
        "request",
        &format!("reply {local} done here"),
        &sessions,
    );
    assert_eq!(
        answer,
        format!("OK replied req={local} to=@{ASKER} OK f1\n")
    );
    {
        let posts = fake.posts.lock().unwrap();
        assert_eq!(posts.len(), 2);
        let (from, to, key, line) = &posts[0];
        assert_eq!(
            from,
            fleet_tab.sid.as_str(),
            "sent from a tab of the request"
        );
        assert_eq!(to, &format!("@{ASKER}@{NODE}"));
        assert!(
            key.starts_with(&format!("rq.{there}.")) && key.len() <= 64,
            "{key}"
        );
        assert!(
            line.starts_with(&format!(
                "reply to request {there} from this-mac: thanks, cleaning it up | req={there} \
                 stage=running"
            )),
            "{line}"
        );
    }
    let mut cold = here.store();
    let request = cold.get(&there).cloned().unwrap();
    assert_eq!(request.replies.len(), 1);
    assert!(request.replies[0].sent);
    assert_eq!(
        cold.said(&there).unwrap().replies[0].1,
        "thanks, cleaning it up"
    );
    // NEGATIVE: a requester that named no session is refused, and nothing is posted.
    let refused = fake.run(
        &no_client,
        "request",
        &format!("reply {silent} hi"),
        &sessions,
    );
    assert_eq!(
        refused,
        "ERR request reply: its requester named no session, so no mail reaches it\n"
    );
    assert_eq!(fake.posts.lock().unwrap().len(), 2);
    // A mail nothing took answers why and is recorded unsent.
    let unsent = Fake {
        unsent: Some(Unsent::NoBridge("absent".to_owned())),
        ..Fake::default()
    };
    let answer = unsent.run(
        &no_client,
        "request",
        &format!("reply {local} again"),
        &sessions,
    );
    assert!(
        answer.starts_with("ERR request reply: no fabric bridge runs here"),
        "{answer}"
    );
    let cold = here.store();
    let replies = cold.get(&local).cloned().unwrap().replies;
    assert_eq!(
        replies.iter().map(|reply| reply.sent).collect::<Vec<_>>(),
        [true, false]
    );
}

/// Session ids `s-…` for tab `n` of a test.
fn sid(n: u8) -> String {
    format!(
        "s-{}",
        format!("{n:x}").repeat(20).get(..20).unwrap_or_default()
    )
}

#[test]
fn requests_pages_by_opening_time_then_id_across_clock_reversal_and_ties() {
    let here = Here::new();
    let sessions = new_store();
    let fake = Fake::default();
    let mut store = here.store();
    // Opened in this order, at these times: the clock went back, then two share a time.
    let first = store
        .open_request(AT + 10, fleet_shell(NODE, BY, "first"))
        .unwrap();
    let second = store
        .open_request(AT, fleet_shell(NODE, BY, "second"))
        .unwrap();
    let third = store
        .open_request(AT + 10, fleet_shell(NODE, BY, "third"))
        .unwrap();
    let page = fake.run(&no_client, "requests", "n=2", &sessions);
    assert_eq!(ids(&page), [third.as_str(), first.as_str()], "{page}");
    let header = &rows(&page)[0].1;
    assert_eq!(header["next"], first, "{page}");
    assert_eq!(header["total"], "3");
    let tail = fake.run(
        &no_client,
        "requests",
        &format!("n=2 before={first}"),
        &sessions,
    );
    assert_eq!(ids(&tail), [second.as_str()], "{tail}");
    assert_eq!(rows(&tail)[0].1["next"], "-", "the last page names no next");
    // The cursor is kept by the filter's pages too.
    let open = fake.run(
        &no_client,
        "requests",
        &format!("state=open before={third}"),
        &sessions,
    );
    assert_eq!(ids(&open), [first.as_str(), second.as_str()], "{open}");
    // NEGATIVE: an unknown cursor, a malformed one and a second one are refused.
    assert_eq!(
        fake.run(
            &no_client,
            "requests",
            "before=r-20261109-1411-87be5228-09",
            &sessions
        ),
        "ERR requests: no such request cursor\n"
    );
    assert!(
        fake.run(&no_client, "requests", "before=r-1", &sessions)
            .starts_with("ERR requests before=r-1: ")
    );
    assert_eq!(
        fake.run(
            &no_client,
            "requests",
            &format!("before={first} before={first}"),
            &sessions
        ),
        LIST_USAGE
    );
    for n in ["n=0", "n=-1", "n=+1", "n="] {
        assert_eq!(fake.run(&no_client, "requests", n, &sessions), LIST_USAGE);
    }
    // NEGATIVE: a drive's cursor is one of its own node's requests; another's is no cursor.
    let foreign = store
        .open_request(
            AT + 20,
            fleet_shell(OTHER, "fleet:c0ffee00:-", "another mac's"),
        )
        .unwrap();
    let _driving = drive(NODE);
    assert_eq!(
        fake.run(
            &no_client,
            "requests",
            &format!("before={foreign}"),
            &sessions
        ),
        "ERR requests: no such request cursor\n"
    );
}

#[test]
fn show_pages_its_tabs_and_their_timeline() {
    let here = Here::new();
    let sessions = new_store();
    let fake = Fake::default();
    let mut store = here.store();
    let id = store
        .open_request(AT, fleet_shell(NODE, BY, "three tabs"))
        .unwrap();
    let tabs = [sid(10), sid(11), sid(12)];
    for (n, tab) in (0..).zip(&tabs) {
        store.tab(AT + 1 + n, &id, tab, BY, false, None).unwrap();
    }
    store
        .attach(AT + 5, &tabs[0], aterm_requests::Verb::Turn, NODE, BY)
        .unwrap();
    let head = fake.run(
        &no_client,
        "request",
        &format!("show {id} limit=2"),
        &sessions,
    );
    let rows_of = |reply: &str, kind: &str| -> Vec<std::collections::HashMap<String, String>> {
        rows(reply)
            .into_iter()
            .filter(|(k, _)| k == kind)
            .map(|(_, fields)| fields)
            .collect()
    };
    let page = &rows_of(&head, "page")[0];
    assert_eq!(
        (
            page["offset"].as_str(),
            page["shown"].as_str(),
            page["tabs"].as_str(),
            page["next_offset"].as_str(),
            page["skipped"].as_str(),
            page["window_complete"].as_str()
        ),
        ("0", "2", "3", "2", "0", "1"),
        "{head}"
    );
    let sids: Vec<String> = rows_of(&head, "tab")
        .into_iter()
        .map(|tab| tab["sid"].clone())
        .collect();
    assert_eq!(sids, tabs[..2], "{head}");
    assert_eq!(rows_of(&head, "tab")[0]["verbs"], "turn:1");
    // The timeline names the page's tabs only.
    assert!(!head.contains(&format!("sid={}", tabs[2])), "{head}");
    let tail = fake.run(
        &no_client,
        "request",
        &format!("show {id} offset=2 limit=2 --json"),
        &sessions,
    );
    assert!(tail.contains("\"next_offset\":\"-\""), "{tail}");
    assert!(tail.contains(&format!("\"sid\":\"{}\"", tabs[2])), "{tail}");
    assert!(
        !tail.contains(&format!("\"sid\":\"{}\"", tabs[0])),
        "{tail}"
    );
    // An offset at the end shows no tab; past it is refused, as are malformed words.
    let end = fake.run(
        &no_client,
        "request",
        &format!("show {id} offset=3"),
        &sessions,
    );
    assert!(rows_of(&end, "tab").is_empty(), "{end}");
    assert_eq!(
        fake.run(
            &no_client,
            "request",
            &format!("show {id} offset=4"),
            &sessions
        ),
        "ERR request show: offset=4 is past its 3 tabs\n"
    );
    for words in [
        "limit=0",
        "limit=513",
        "offset=-1",
        "offset=1 offset=1",
        "before=x",
    ] {
        assert_eq!(
            fake.run(
                &no_client,
                "request",
                &format!("show {id} {words}"),
                &sessions
            ),
            USAGE,
            "{words}"
        );
    }
}

#[test]
fn a_read_counts_skipped_rows_writes_nothing_and_refuses_a_broken_journal() {
    use std::io::Write as _;
    let here = Here::new();
    let sessions = new_store();
    let fake = Fake::default();
    let mut store = here.store();
    let id = request(
        &mut store,
        AT,
        fleet_shell(NODE, BY, "cached"),
        sid(10).as_str(),
        Some(0),
    );
    let journal = here.0.path().join(aterm_requests::JOURNAL);
    std::fs::OpenOptions::new()
        .append(true)
        .open(&journal)
        .unwrap()
        .write_all(b"foreign row\n")
        .unwrap();
    let before = here.journal();
    let listed = fake.run(&no_client, "requests", "", &sessions);
    let header = &rows(&listed)[0].1;
    assert_eq!(
        (
            header["skipped"].as_str(),
            header["window_complete"].as_str()
        ),
        ("1", "1"),
        "{listed}"
    );
    let shown = fake.run(&no_client, "request", &format!("show {id}"), &sessions);
    assert!(shown.contains(" skipped=1 window_complete=1"), "{shown}");
    assert_eq!(here.journal(), before, "a read writes nothing");
    // NEGATIVE: the process's store holds what it read; a journal it cannot read now refuses
    // every read rather than answer from that.
    std::fs::remove_file(&journal).unwrap();
    std::fs::create_dir(&journal).unwrap();
    for (verb, rest) in [
        ("requests", String::new()),
        ("request", format!("show {id}")),
    ] {
        let refused = fake.run(&no_client, verb, &rest, &sessions);
        assert!(refused.starts_with("ERR request store: "), "{refused}");
    }
    std::fs::remove_dir(&journal).unwrap();
}

#[test]
fn a_cold_process_reads_the_same_requests_and_a_broken_content_tier_hides_nothing_else() {
    let here = Here::new();
    let sessions = new_store();
    let fake = Fake::default();
    let mut store = here.store();
    let given = request(
        &mut store,
        AT,
        fleet_shell(NODE, BY, "a given why"),
        sid(10).as_str(),
        Some(0),
    );
    let fallback = request(
        &mut store,
        AT + 1,
        Intake {
            from: From {
                node: NODE.to_owned(),
                ..local_spawn("x").from
            },
            ..local_spawn("a private goal")
        },
        sid(11).as_str(),
        None,
    );
    drop(store);
    let warm = fake.run(&no_client, "requests", "", &sessions);
    // A process the update replaced: its store is gone, and the next reads the folder cold.
    crate::requests_host::forget_for_test();
    let cold = fake.run(&no_client, "requests", "", &sessions);
    assert_eq!(cold, warm);
    assert_eq!(ids(&cold), [fallback.as_str(), given.as_str()]);
    assert!(
        cold.contains("a%20private%20goal"),
        "an owner reads the fallback: {cold}"
    );
    // The content tier unreadable: the host tier still answers, with no fallback text.
    crate::requests_host::forget_for_test();
    let said = here.0.path().join(aterm_requests::SAID);
    std::fs::remove_file(&said).unwrap();
    std::fs::create_dir(&said).unwrap();
    let hosted = fake.run(
        &no_client,
        "request",
        &format!("show {fallback}"),
        &sessions,
    );
    assert!(hosted.starts_with("OK "), "{hosted}");
    assert!(!hosted.contains("private"), "{hosted}");
    // NEGATIVE: a drive is never served the content tier, whatever the owner's reads cached.
    std::fs::remove_dir(&said).unwrap();
    crate::requests_host::forget_for_test();
    let _ = fake.run(
        &no_client,
        "request",
        &format!("show {fallback}"),
        &sessions,
    );
    let _driving = drive(NODE);
    for (verb, rest) in [
        ("requests", String::new()),
        ("requests", "--json".to_owned()),
        ("request", format!("show {fallback}")),
        ("request", format!("show {fallback} --json")),
    ] {
        let read = fake.run(&no_client, verb, &rest, &sessions);
        assert!(read.starts_with("OK "), "{read}");
        assert!(!read.contains("private"), "{verb} {rest}: {read}");
        assert!(
            !read.contains("said why=") && !read.contains("\"said\":{"),
            "{verb} {rest}: {read}"
        );
    }
}

#[test]
fn a_missing_store_reads_empty_and_is_not_made() {
    let here = Here::new();
    let sessions = new_store();
    let fake = Fake::default();
    let missing = here.0.path().join("not-made");
    set_dir_for_test(Some(missing.clone()));
    assert_eq!(
        fake.run(&no_client, "requests", "", &sessions),
        "OK 1\nrequests total=0 unreviewed=0 shown=0 next=- skipped=0 window_complete=1\n"
    );
    assert_eq!(
        fake.run(
            &no_client,
            "request",
            "show r-20261109-1411-87be5228-09",
            &sessions
        ),
        "ERR request show: no request of that id on this Mac\n"
    );
    // NEGATIVE: a cursor into no store is no cursor.
    assert_eq!(
        fake.run(
            &no_client,
            "requests",
            "before=r-20261109-1411-87be5228-09",
            &sessions
        ),
        "ERR requests: no such request cursor\n"
    );
    assert!(!missing.try_exists().unwrap());
}

#[test]
fn a_journal_past_one_read_says_its_window_is_incomplete() {
    use std::io::{Seek as _, Write as _};
    let here = Here::new();
    let sessions = new_store();
    let fake = Fake::default();
    let mut store = here.store();
    let id = store
        .open_request(AT, fleet_shell(NODE, BY, "the retained tail"))
        .unwrap();
    drop(store);
    let retained = here.journal();
    let path = here.0.path().join(aterm_requests::JOURNAL);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&path)
        .unwrap();
    // A sparse head older than one read takes.
    file.set_len(aterm_requests::store::READ_MAX).unwrap();
    file.seek(std::io::SeekFrom::End(0)).unwrap();
    file.write_all(b"\n").unwrap();
    file.write_all(retained.as_bytes()).unwrap();
    drop(file);
    let listed = fake.run(&no_client, "requests", "", &sessions);
    assert_eq!(ids(&listed), [id.as_str()], "{listed}");
    assert_eq!(rows(&listed)[0].1["window_complete"], "0", "{listed}");
}
