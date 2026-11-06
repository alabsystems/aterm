// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The metrics publisher on a scratch state (§11 Q4): a roster and an accounts store, an agy
//! reading, a Claude transcript and a Codex rollout, published on a log that keeps what it is
//! handed. Every pass runs on the test's thread inside `provider_read::captured`, which sees each
//! provider read that thread asks for.

use super::{EVERY_ROW, Homes, Log, Places, Publisher, Report};
use aterm_agent::harness::disk::discover::Running;
use aterm_agent::harness::{accounts, claude_homes, provider_read, usage::rfc3339_utc};
use aterm_metrics::row::{ManagerEntry, NeedEntry, NeedsRow, Row, Sid, Word};
use aterm_metrics::subject::{Class, Cli, Metric, Node, Now, Subject, Width};
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const NODE_KEY: [u8; 32] = [7; 32];
const POOL_A: &str = "ac-00000000000000a1";
const POOL_B: &str = "ac-00000000000000b2";

/// A log that keeps every record it is handed, in order.
struct Kept {
    node: Node,
    records: Mutex<Vec<(String, Vec<u8>)>>,
}

impl Kept {
    fn new() -> Self {
        Self {
            node: Node::of_key(&NODE_KEY),
            records: Mutex::new(Vec::new()),
        }
    }

    fn len(&self) -> usize {
        self.records.lock().expect("the records").len()
    }

    /// The records from the `from`-th on, each decoded at its subject.
    fn since(&self, from: usize) -> Vec<(Subject, Row)> {
        self.records.lock().expect("the records")[from..]
            .iter()
            .map(|(subject, body)| {
                let subject = Subject::parse(subject).expect("a subject the grammar takes");
                let row = Row::decode_at(&subject, body).expect("a row that reads back");
                (subject, row)
            })
            .collect()
    }
}

impl Log for Kept {
    fn node(&self) -> Node {
        self.node
    }

    fn publish(&self, subject: &str, body: &[u8]) -> io::Result<()> {
        self.records
            .lock()
            .expect("the records")
            .push((subject.to_owned(), body.to_vec()));
        Ok(())
    }

    fn published(&self) -> io::Result<Vec<(String, Vec<u8>)>> {
        Ok(self.records.lock().expect("the records").clone())
    }
}

/// A scratch state: `aterm.toml`'s folder with a Claude roster of two pool rows, the harness
/// state with their samples, an agy home's reading, a Claude home and a Codex home.
struct Scratch {
    dir: aterm_tempfile::TempDir,
    now: i64,
}

impl Scratch {
    fn new() -> Self {
        let dir = aterm_tempfile::TempDir::new().expect("a scratch dir");
        // Half a minute into the current minute, so a message at `now - 5` is in the open one.
        let real = i64::try_from(aterm_time::unix_secs().expect("the clock")).expect("a time");
        let now = real - real.rem_euclid(60) + 30;
        let scratch = Self { dir, now };
        scratch.roster(&["pool-a", "pool-b"]);
        scratch.sample("pool-a", 40, now - 60);
        scratch.sample("pool-b", 10, now - 60);
        let agy = scratch.harness().join("agy-quota");
        std::fs::create_dir_all(&agy).expect("the agy store");
        let doc = r#"{"status":"SUCCESS","command":{"name":"usage","data":{"groups":[{"name":"Gemini Models","description":"Models within this group: Gemini Flash, Gemini Pro","buckets":[{"id":"gemini-weekly","window":"weekly","remaining_fraction":0.75,"reset_time":"2026-12-01T00:00:00Z"},{"id":"gemini-5h","window":"5h","remaining_fraction":1}]}]}}}"#;
        let reading = format!(
            "{{\"schema\":1,\"as_of\":{},\"doc\":{}}}",
            now - 600,
            aterm_json::to_string(&aterm_json::Value::from(doc)).expect("a JSON string")
        );
        std::fs::write(agy.join("home.json"), reading).expect("the agy reading");
        std::fs::create_dir_all(scratch.claude().join("projects").join("-proj"))
            .expect("a project");
        std::fs::create_dir_all(scratch.path("home")).expect("a home");
        scratch
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    /// Claude's roster file beside the scratch's `aterm.toml`.
    fn roster_path(&self) -> PathBuf {
        accounts::roster_beside_for(&self.config(), accounts::Cli::Claude).expect("a path")
    }

    /// Write Claude's roster: one pool row per label, each with its own login directory.
    fn roster(&self, labels: &[&str]) {
        let mut roster = String::new();
        for label in labels {
            let login = self.login(label);
            roster = accounts::append_row(
                &roster,
                label,
                accounts::Class::Pool,
                Some(login.to_str().expect("utf-8")),
            )
            .expect("the row admits");
        }
        std::fs::write(self.roster_path(), roster).expect("the roster");
    }

    fn login(&self, label: &str) -> PathBuf {
        self.path("logins").join(label)
    }

    fn config(&self) -> PathBuf {
        self.path("aterm.toml")
    }

    fn harness(&self) -> PathBuf {
        self.path("harness")
    }

    fn claude(&self) -> PathBuf {
        self.path("claude")
    }

    fn codex(&self) -> PathBuf {
        self.path("codex")
    }

    /// Note `pct` as `label`'s five-hour figure, read at `at`, and key it.
    fn sample(&self, label: &str, pct: u32, at: i64) {
        let store = accounts::Store::at(&self.harness());
        let (mut state, _) = store.load(self.now).expect("the accounts store");
        accounts::note_sample(
            &mut state,
            label,
            accounts::Sample {
                session_pct: Some(pct),
                session_reset: Some(self.now + 2 * 3600),
                session_read_at: Some(at),
                at,
                ..accounts::Sample::default()
            },
            self.now,
        );
        let pk = if label == "pool-a" { POOL_A } else { POOL_B };
        state.accounts.entry(label.to_owned()).or_default().pk = Some(pk.to_owned());
        store.save(&state).expect("the accounts store saves");
    }

    fn places(&self) -> Places {
        self.places_with(vec![self.claude()], true)
    }

    /// The scratch's places with the Claude homes the node service lists
    /// ([`super::claude_homes_at`]) from the scratch's records, with no Claude Code running.
    fn places_listed(&self) -> Places {
        let records = (
            Some(self.config()),
            Some(self.harness()),
            Some(self.path("home")),
        );
        Places {
            claude_homes: Box::new(move |now| {
                super::claude_homes_at(&records, Ok(Running::default()), now)
            }),
            ..self.places()
        }
    }

    /// The scratch's places with `claude` as the Claude homes listed, `complete` or not.
    fn places_with(&self, claude: Vec<PathBuf>, complete: bool) -> Places {
        let codex = self.codex();
        Places {
            config: Some(self.config()),
            harness: Some(self.harness()),
            home: Some(self.path("home")),
            spool: Some(aterm_db::alab::journal::Spool::at(self.path("alab"))),
            claude_homes: Box::new(move |_| Homes {
                dirs: claude.clone(),
                complete,
            }),
            codex_homes: Box::new(move |_| Homes {
                dirs: vec![codex.clone()],
                complete: true,
            }),
        }
    }

    /// Append one assistant row of message `id` at `at` with `output` tokens to the transcript.
    fn message(&self, id: &str, at: i64, output: u64) {
        self.message_in(&self.claude(), id, at, output);
    }

    /// [`Scratch::message`] in the Claude home `home`.
    fn message_in(&self, home: &Path, id: &str, at: i64, output: u64) {
        let line = format!(
            "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"message\":{{\"id\":\"{id}\",\
             \"model\":\"claude-opus-5-5\",\"role\":\"assistant\",\"content\":[],\"usage\":{{\
             \"input_tokens\":3,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0,\
             \"output_tokens\":{output}}}}}}}\n",
            rfc3339_utc(at)
        );
        append(
            &home.join("projects").join("-proj").join("sess.jsonl"),
            &line,
        );
    }

    /// Append a turn of a GPT model at `at` to a rollout, starting it where it is new.
    fn turn(&self, at: i64, total: (u64, u64, u64), last: (u64, u64, u64)) {
        let path = self
            .codex()
            .join("sessions/2026/11/05/rollout-2026-11-05T10-00-00-01900000-0000-7000-8000-000000000001.jsonl");
        let stamp = |s: i64| format!("{}.000Z", &rfc3339_utc(s)[..19]);
        let usage = |(input, cached, output): (u64, u64, u64)| {
            format!(
                "{{\"input_tokens\":{input},\"cached_input_tokens\":{cached},\
                 \"cache_write_input_tokens\":0,\"output_tokens\":{output},\
                 \"reasoning_output_tokens\":0,\"total_tokens\":{}}}",
                input + output
            )
        };
        let mut lines = String::new();
        if !path.exists() {
            lines.push_str(&format!(
                "{{\"timestamp\":\"{at}\",\"type\":\"session_meta\",\"payload\":{{\"session_id\":\
                 \"01900000-0000-7000-8000-000000000001\",\"id\":\
                 \"01900000-0000-7000-8000-000000000001\",\"timestamp\":\"{at}\",\"cwd\":\
                 \"/stand-in/project\",\"originator\":\"codex-tui\",\"cli_version\":\"0.163.0\",\
                 \"source\":\"cli\",\"model_provider\":\"openai\"}}}}\n",
                at = stamp(at - 1)
            ));
            lines.push_str(&format!(
                "{{\"timestamp\":\"{}\",\"type\":\"turn_context\",\"payload\":{{\"turn_id\":\"t\",\
                 \"cwd\":\"/stand-in/project\",\"model\":\"gpt-5.6-sol\"}}}}\n",
                stamp(at - 1)
            ));
        }
        lines.push_str(&format!(
            "{{\"timestamp\":\"{}\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"token_count\",\
             \"info\":{{\"total_token_usage\":{},\"last_token_usage\":{},\
             \"model_context_window\":272000}},\"rate_limits\":null}}}}\n",
            stamp(at),
            usage(total),
            usage(last)
        ));
        append(&path, &lines);
    }
}

fn append(path: &Path, text: &str) {
    use std::io::Write as _;
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("its folder");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("the file opens");
    file.write_all(text.as_bytes()).expect("the append");
}

/// A pass at `now` with the journal on, asserting it asked no provider.
fn pass(publisher: &Publisher, log: &Kept, now: i64) -> Report {
    pass_as(publisher, log, now, true)
}

/// A pass at `now` with the journal on or off, asserting it asked no provider.
fn pass_as(publisher: &Publisher, log: &Kept, now: i64, publishing: bool) -> Report {
    let at = u64::try_from(now).expect("a time");
    let (report, reads) =
        provider_read::captured(|| publisher.pass(Some(log as &dyn Log), at, publishing));
    assert!(reads.is_empty(), "a pass asked a provider: {reads:?}");
    report
}

fn quota_rows(rows: &[(Subject, Row)]) -> Vec<&aterm_metrics::row::QuotaRow> {
    rows.iter()
        .filter_map(|(_, row)| match row {
            Row::Quota(q) => Some(q),
            _ => None,
        })
        .collect()
}

fn windows(rows: &[(Subject, Row)]) -> Vec<&aterm_metrics::row::WindowRecord> {
    rows.iter()
        .filter_map(|(_, row)| match row {
            Row::Window(w) => Some(w),
            _ => None,
        })
        .collect()
}

/// The first pass publishes one quota row per keyed account row, the agy home's row and the
/// windows of what was served; a second, unchanged pass publishes nothing; a one-point change
/// publishes one row; and none of it asks a provider.
#[test]
fn a_pass_publishes_each_row_that_changed_and_nothing_else() {
    let scratch = Scratch::new();
    let now = scratch.now;
    scratch.message("msg_1", now - 3700, 10);
    scratch.message("msg_2", now - 3700, 20);
    scratch.turn(now - 3700, (1000, 800, 50), (1000, 800, 50));
    let publisher = Publisher::new(scratch.places());
    let log = Kept::new();

    let first = pass(&publisher, &log, now);
    assert!(!first.unread && first.refused.is_none(), "{first:?}");
    let rows = log.since(0);
    let quota = quota_rows(&rows);
    assert_eq!(
        quota.len(),
        2,
        "one quota row per keyed account row: {quota:?}"
    );
    let pool_a = quota
        .iter()
        .find(|q| q.label.as_str() == "pool-a")
        .expect("pool-a's row");
    assert_eq!(pool_a.account.to_string(), POOL_A);
    assert_eq!(pool_a.cli, Cli::Claude);
    assert_eq!(pool_a.pct5, Some(40));
    let agy: Vec<_> = rows
        .iter()
        .filter_map(|(_, row)| match row {
            Row::Agy(a) => Some(a),
            _ => None,
        })
        .collect();
    assert_eq!(agy.len(), 1, "the agy home's row");
    assert_eq!(agy[0].groups.len(), 2);
    assert_eq!(agy[0].groups[0].remaining, Some(7500));
    let minute = windows(&rows)
        .into_iter()
        .find(|w| w.metric() == Metric::Tokens(Cli::Claude))
        .expect("the minute of the two messages");
    assert_eq!(minute.revision(), 0);
    assert_eq!(minute.summary().count(), 2, "two messages, two samples");
    assert_eq!(minute.summary().sum(), 2 * 3 + 10 + 20);
    assert!(
        windows(&rows)
            .iter()
            .any(|w| w.metric() == Metric::Tokens(Cli::Codex) && w.summary().count() == 1),
        "the rollout's turn has its minute"
    );
    for record in windows(&rows) {
        assert!(
            record.summary().count() > 0,
            "an empty window was published"
        );
    }

    let before = log.len();
    let second = pass(&publisher, &log, now);
    assert_eq!(
        (second.rows, second.windows, log.len()),
        (0, 0, before),
        "an unchanged pass published: {:?}",
        log.since(before)
    );

    scratch.sample("pool-a", 41, now - 30);
    let third = pass(&publisher, &log, now);
    let moved = log.since(before);
    assert_eq!((third.rows, third.windows), (1, 0), "{moved:?}");
    let quota = quota_rows(&moved);
    assert_eq!(quota.len(), 1);
    assert_eq!(
        (quota[0].label.as_str(), quota[0].pct5),
        ("pool-a", Some(41))
    );
}

/// NEGATIVE CONTROL (`EVERY_ROW`): with change detection off, the unchanged second pass
/// publishes every row again, so the assertion above that it publishes nothing fails.
#[test]
fn with_change_detection_off_an_unchanged_pass_publishes_again() {
    let scratch = Scratch::new();
    scratch.message("msg_1", scratch.now - 3700, 10);
    let publisher = Publisher::new(scratch.places());
    let log = Kept::new();
    pass(&publisher, &log, scratch.now);
    let before = log.len();
    EVERY_ROW.with(|on| on.set(true));
    let second = pass(&publisher, &log, scratch.now);
    EVERY_ROW.with(|on| on.set(false));
    assert!(
        second.rows >= 2 && second.windows > 0 && log.len() > before,
        "with change detection off the unchanged pass published nothing: {second:?}"
    );
}

/// A window is published once it closes, never while it is open and never with no sample: a
/// message in the open minute waits for its end, and the minutes around it, which served nothing,
/// have no record.
#[test]
fn a_window_waits_for_its_end_and_an_empty_one_is_never_published() {
    let scratch = Scratch::new();
    let now = scratch.now;
    scratch.message("msg_open", now - 5, 7);
    let publisher = Publisher::new(scratch.places());
    let log = Kept::new();
    pass(&publisher, &log, now);
    let open = now - 30;
    let in_open = |w: &&aterm_metrics::row::WindowRecord| {
        w.metric() == Metric::Tokens(Cli::Claude)
            && w.window().start_ms() == u64::try_from(open).expect("a time") * 1000
    };
    assert!(
        !windows(&log.since(0)).iter().any(in_open),
        "the open minute was published"
    );
    let before = log.len();
    let later = pass(&publisher, &log, now + 60);
    let published = log.since(before);
    let minutes: Vec<_> = windows(&published)
        .into_iter()
        .filter(|w| w.metric().width() == Width::Minute)
        .collect();
    assert_eq!(later.windows, windows(&published).len());
    assert_eq!(minutes.len(), 1, "only the minute that served: {minutes:?}");
    assert!(in_open(&minutes[0]));
    assert_eq!(minutes[0].summary().count(), 1);
}

/// A late sample of a published window (a streamed message's counts that grew, a turn read
/// after its window closed) republishes the window at one revision more; a restarted publisher
/// reads its own rows back, so it republishes nothing unchanged and goes on from the revision
/// the log holds.
#[test]
fn a_late_sample_republishes_its_window_at_the_next_revision_across_a_restart() {
    let scratch = Scratch::new();
    let now = scratch.now;
    scratch.message("msg_1", now - 3700, 10);
    let log = Kept::new();
    let publisher = Publisher::new(scratch.places());
    pass(&publisher, &log, now);
    let minute_of = |rows: &[(Subject, Row)]| {
        windows(rows)
            .into_iter()
            .filter(|w| w.metric() == Metric::Tokens(Cli::Claude))
            .map(|w| (w.revision(), w.summary().count(), w.summary().sum()))
            .collect::<Vec<_>>()
    };
    assert_eq!(minute_of(&log.since(0)), [(0, 1, 13)]);

    // The same message's later row, its output grown: a raise, in the same window.
    scratch.message("msg_1", now - 3690, 25);
    let before = log.len();
    pass(&publisher, &log, now);
    assert_eq!(minute_of(&log.since(before)), [(1, 1, 28)]);

    // A restarted publisher folds the transcript from its start and finds the log holds it.
    let restarted = Publisher::new(scratch.places());
    let before = log.len();
    let report = pass(&restarted, &log, now);
    assert_eq!(
        (report.rows, report.windows),
        (0, 0),
        "a restart republished: {:?}",
        log.since(before)
    );

    // A message read late into the same minute: one revision more than the log holds.
    scratch.message("msg_2", now - 3695, 5);
    pass(&restarted, &log, now);
    assert_eq!(minute_of(&log.since(before)), [(2, 2, 36)]);
}

fn sid(n: u8) -> Sid {
    Sid::parse(&format!("s-{n:020x}")).expect("a sid")
}

/// A needs change a window pushes publishes the Mac's one `now/needs` row, joined over its
/// windows; the same rows again publish nothing; a pass republishes nothing of it either.
#[test]
fn a_needs_change_pushed_by_a_window_publishes_one_needs_row() {
    let scratch = Scratch::new();
    let publisher = Publisher::new(scratch.places());
    let log = Kept::new();
    let now = u64::try_from(scratch.now).expect("a time");
    pass(&publisher, &log, scratch.now);
    let row = |need: &str, manager: u8| NeedsRow {
        at: now,
        needs: vec![NeedEntry {
            sid: sid(1),
            word: Word::new(need).expect("a word"),
            harness: false,
            worker_of: None,
            since: Some(now - 60),
        }],
        held: Vec::new(),
        managers: vec![ManagerEntry {
            sid: sid(manager),
            workers: 2,
        }],
    };
    let mut windows = BTreeMap::from([(101, row("ask", 9)), (102, row("ask", 9))]);
    let before = log.len();
    let (report, reads) =
        provider_read::captured(|| publisher.needs(Some(&log as &dyn Log), &windows, now));
    assert!(reads.is_empty(), "{reads:?}");
    let published = log.since(before);
    assert_eq!(report.rows, 1, "{published:?}");
    let [(subject, Row::Needs(needs))] = published.as_slice() else {
        panic!("one needs row: {published:?}");
    };
    assert!(matches!(
        subject,
        Subject::Now {
            now: Now::Needs,
            ..
        }
    ));
    assert_eq!(subject.class(), Class::Now);
    assert_eq!(
        needs.needs.len(),
        1,
        "a session two windows list is listed once"
    );
    assert_eq!(needs.managers[0].workers, 4, "a manager's workers summed");

    let before = log.len();
    publisher.needs(Some(&log as &dyn Log), &windows, now + 5);
    pass(&publisher, &log, scratch.now + 5);
    assert_eq!(
        log.len(),
        before,
        "unchanged needs published: {:?}",
        log.since(before)
    );

    windows.remove(&102);
    windows.insert(101, row("question", 9));
    publisher.needs(Some(&log as &dyn Log), &windows, now + 9);
    let published = log.since(before);
    assert_eq!(published.len(), 1, "{published:?}");
}

/// The node service's own `metrics.log` reads back what a pass published on it: the newest row of
/// each snapshot subject from its last-value face, and every window record from each width's
/// floor. A second publisher on it, as after a restart, publishes nothing unchanged, and a late
/// sample goes on from the revision the log holds.
#[test]
fn the_node_log_reads_back_what_a_pass_published_on_it() {
    use std::sync::Arc;
    // Short and in /tmp, as the mesh's own tests make it: its brokers' socket paths live there.
    let state = aterm_tempfile::Builder::new()
        .prefix("amq")
        .tempdir_in("/tmp")
        .expect("a scratch state dir");
    aterm_uds::private_file::ensure_private_dir(state.path()).expect("a private state dir");
    let seed = [11u8; 32];
    let mut text = aterm_codec::hex::encode_lower(&seed);
    text.push('\n');
    aterm_uds::private_file::write_private(
        state.path(),
        aterm_net::fleet_member::NODE_KEY_FILE,
        text.as_bytes(),
    )
    .expect("the node key");
    aterm_net::fleet_member::node_identity(state.path()).expect("the node identity");
    // This Mac seated alone, so its leases need no neighbour's word.
    let alone = aterm_mesh::Standings {
        current: [astream_broker::origin::public_key(&seed)]
            .into_iter()
            .collect(),
        ..aterm_mesh::Standings::default()
    };
    let mesh = aterm_mesh::Mesh::open(aterm_mesh::MeshConfig {
        state_dir: state.path().to_path_buf(),
        backups: aterm_uds::backup::Backups::INERT,
        clock: aterm_net::fleet::link::ManualClock::new()
            as Arc<dyn aterm_net::fleet::link::LinkClock>,
        wall: Arc::new(std::time::SystemTime::now),
        standings: Arc::new(move || Some(alone.clone())),
        changed: None,
        watch_every: std::time::Duration::from_secs(60),
        observe: None,
        stores: None,
    })
    .expect("the mesh opens");
    let log = super::NodeLog {
        node: Node::parse(mesh.principal()).expect("a node id"),
        mesh,
    };

    let scratch = Scratch::new();
    let now = scratch.now;
    scratch.message("msg_1", now - 3700, 10);
    let first = provider_read::captured(|| {
        Publisher::new(scratch.places()).pass(
            Some(&log as &dyn Log),
            u64::try_from(now).expect("a time"),
            true,
        )
    })
    .0;
    assert!(
        !first.unread && first.refused.is_none() && first.rows >= 3 && first.windows > 0,
        "{first:?}"
    );

    let restarted = Publisher::new(scratch.places());
    let again = provider_read::captured(|| {
        restarted.pass(
            Some(&log as &dyn Log),
            u64::try_from(now).expect("a time"),
            true,
        )
    })
    .0;
    assert_eq!(again, Report::default(), "a restart on the log republished");

    scratch.message("msg_2", now - 3695, 5);
    let late = provider_read::captured(|| {
        restarted.pass(
            Some(&log as &dyn Log),
            u64::try_from(now).expect("a time"),
            true,
        )
    })
    .0;
    assert!(late.windows > 0 && late.refused.is_none(), "{late:?}");
    let records = log.published().expect("the log reads back");
    let minute = records
        .iter()
        .filter_map(|(subject, body)| {
            let subject = Subject::parse(subject).ok()?;
            match Row::decode_at(&subject, body).ok()? {
                Row::Window(w) if w.metric() == Metric::Tokens(Cli::Claude) => {
                    Some((w.revision(), w.summary().count()))
                }
                _ => None,
            }
        })
        .max()
        .expect("the minute's records");
    assert_eq!(
        minute,
        (1, 2),
        "the late sample's revision follows the log's"
    );
}

/// Whether `record`'s window overlaps `[since, until)`, Unix seconds.
fn overlaps(record: &aterm_metrics::row::WindowRecord, since: i64, until: i64) -> bool {
    let ms = |s: i64| u64::try_from(s).expect("a time") * 1000;
    record.window().start_ms() < ms(until) && ms(since) < record.window().end_ms()
}

/// What was served while `[alab] journal` was off is never published: an off pass publishes
/// nothing, the message it served is dropped (its later raise too), every window that overlaps
/// the span is withheld once the journal is on again, and a restarted publisher, which folds the
/// transcript from its start, keeps the span from the file beside the spool.
#[test]
fn what_was_served_while_the_journal_was_off_is_never_published() {
    let scratch = Scratch::new();
    let now = scratch.now;
    scratch.message("msg_before", now - 3700, 10);
    let publisher = Publisher::new(scratch.places());
    let log = Kept::new();
    pass_as(&publisher, &log, now - 300, true);
    assert!(
        windows(&log.since(0))
            .iter()
            .any(|w| w.metric() == Metric::Tokens(Cli::Claude) && w.summary().count() == 1),
        "the message before the span has its minute"
    );

    // Off from the last pass that saw it on: `[now - 300, now)`.
    let before = log.len();
    pass_as(&publisher, &log, now - 200, false);
    scratch.message("msg_off", now - 150, 20);
    pass_as(&publisher, &log, now - 100, false);
    assert_eq!(
        log.len(),
        before,
        "an off pass published: {:?}",
        log.since(before)
    );

    pass_as(&publisher, &log, now, true);
    scratch.message("msg_off", now - 140, 50);
    scratch.message("msg_after", now + 40, 7);
    pass_as(&publisher, &log, now + 100, true);
    let published = log.since(0);
    for record in windows(&published) {
        assert!(
            !overlaps(record, now - 300, now),
            "a window of the off span was published: {record:?}"
        );
    }
    let after = |rows: &[(Subject, Row)]| {
        windows(rows)
            .into_iter()
            .filter(|w| {
                w.metric() == Metric::Tokens(Cli::Claude) && overlaps(w, now + 40, now + 41)
            })
            .map(|w| w.summary().count())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        after(&published),
        [1],
        "the minute after the span is published"
    );

    let restarted = Publisher::new(scratch.places());
    let before = log.len();
    pass_as(&restarted, &log, now + 100, true);
    for record in windows(&log.since(before)) {
        assert!(
            !overlaps(record, now - 300, now),
            "a restart published a window of the off span: {record:?}"
        );
    }
}

/// A place the log holds live that the book no longer names is published once as ended, at the
/// pass's time; a place already ended is left as it is.
#[test]
fn a_place_the_book_no_longer_holds_is_published_once_as_ended() {
    use aterm_metrics::row::PlaceRow;
    use aterm_metrics::subject::AccountKey;
    let scratch = Scratch::new();
    let now = u64::try_from(scratch.now).expect("a time");
    let log = Kept::new();
    let place = |account: &str, until: u64| PlaceRow {
        at: now - 600,
        account: AccountKey::parse(account).expect("an account key"),
        place: Word::new("fill").expect("a word"),
        since: now - 600,
        until,
    };
    for row in [place(POOL_A, now + 3600), place(POOL_B, now - 10)] {
        log.publish(
            &row.subject(log.node()).to_string(),
            &Row::Place(row).encode(),
        )
        .expect("the place");
    }
    let publisher = Publisher::new(scratch.places());
    let before = log.len();
    pass(&publisher, &log, scratch.now);
    let ended: Vec<_> = log
        .since(before)
        .into_iter()
        .filter_map(|(_, row)| match row {
            Row::Place(place) => Some(place),
            _ => None,
        })
        .collect();
    assert_eq!(ended.len(), 1, "{ended:?}");
    assert_eq!(ended[0].account.to_string(), POOL_A);
    assert_eq!((ended[0].at, ended[0].until), (now, now));

    let before = log.len();
    pass(&publisher, &log, scratch.now + 60);
    assert!(
        !log.since(before)
            .iter()
            .any(|(_, row)| matches!(row, Row::Place(_))),
        "an ended place was published again"
    );
}

/// A publisher that finds the journal off at its first pass cannot tell since when it was off,
/// so its span starts at the horizon's start: nothing the horizon served is published once the
/// journal is on again.
#[test]
fn a_journal_off_at_the_first_pass_withholds_the_whole_horizon() {
    let scratch = Scratch::new();
    let now = scratch.now;
    scratch.message("msg_1", now - 3700, 10);
    scratch.turn(now - 3700, (1000, 800, 50), (1000, 800, 50));
    let publisher = Publisher::new(scratch.places());
    let log = Kept::new();
    pass_as(&publisher, &log, now, false);
    let report = pass_as(&publisher, &log, now + 60, true);
    assert_eq!(
        report.windows,
        0,
        "a window of the horizon was published: {:?}",
        log.since(0)
    );
    assert!(
        report.rows > 0,
        "the snapshot rows are published once it is on"
    );
}

/// The login homes the accounts records name are the homes a pass folds beside the running ones:
/// the scratch roster's two rows' directories.
#[test]
fn the_login_homes_are_the_ones_the_roster_names() {
    let scratch = Scratch::new();
    let records = (
        Some(scratch.config()),
        Some(scratch.harness()),
        Some(scratch.path("home")),
    );
    let homes =
        super::login_homes(&records, accounts::Cli::Claude, scratch.now).expect("the records read");
    assert_eq!(
        homes,
        [scratch.path("logins/pool-a"), scratch.path("logins/pool-b")]
    );
}

/// While a home may be missing from the listing (the running processes or the account records
/// could not be read), Claude's windows wait: a window of part of its homes is never published.
/// Once the listing is whole the same window goes out.
#[test]
fn a_listing_that_may_miss_a_home_withholds_its_windows() {
    let scratch = Scratch::new();
    let now = scratch.now;
    scratch.message("msg_1", now - 3700, 10);
    let log = Kept::new();
    let claude_windows = |rows: &[(Subject, Row)]| {
        windows(rows)
            .into_iter()
            .filter(|w| w.metric().cli() == Cli::Claude)
            .count()
    };
    let partial = Publisher::new(scratch.places_with(vec![scratch.claude()], false));
    pass(&partial, &log, now);
    assert_eq!(claude_windows(&log.since(0)), 0, "{:?}", log.since(0));
    let whole = Publisher::new(scratch.places());
    pass(&whole, &log, now);
    assert!(
        claude_windows(&log.since(0)) > 0,
        "the whole listing's windows"
    );
}

/// Past [`super::HOMES_MAX`] listed homes, every one held listed still, Claude's windows of the
/// horizon are withheld: the home left out may hold samples of any of them.
#[test]
fn more_listed_homes_than_are_held_withhold_the_horizon() {
    let scratch = Scratch::new();
    let now = scratch.now;
    scratch.message("msg_1", now - 3700, 10);
    let mut homes = vec![scratch.claude()];
    for n in 0..super::HOMES_MAX {
        let home = scratch.path(&format!("homes/{n}"));
        std::fs::create_dir_all(home.join("projects")).expect("a home");
        homes.push(home);
    }
    let publisher = Publisher::new(scratch.places_with(homes, true));
    let log = Kept::new();
    pass(&publisher, &log, now);
    assert!(
        !windows(&log.since(0))
            .iter()
            .any(|w| w.metric().cli() == Cli::Claude),
        "a window was published with a listed home left out"
    );
}

/// Two account rows on one key project two rows on one subject: the first is published and the
/// next pass publishes nothing, never the two over each other on every pass.
#[test]
fn two_account_rows_on_one_key_publish_one_row_once() {
    let scratch = Scratch::new();
    let store = accounts::Store::at(&scratch.harness());
    let (mut state, _) = store.load(scratch.now).expect("the accounts store");
    state.accounts.entry("pool-b".to_owned()).or_default().pk = Some(POOL_A.to_owned());
    store.save(&state).expect("the accounts store saves");
    let publisher = Publisher::new(scratch.places());
    let log = Kept::new();
    pass(&publisher, &log, scratch.now);
    assert_eq!(quota_rows(&log.since(0)).len(), 1, "{:?}", log.since(0));
    let before = log.len();
    let second = pass(&publisher, &log, scratch.now + 60);
    assert_eq!(second.rows, 0, "{:?}", log.since(before));
}

/// A window that starts before the oldest row a Claude home's fold reads back to is never
/// published, as one that may miss samples served before it.
#[test]
fn no_window_older_than_the_fold_reads_is_published() {
    let scratch = Scratch::new();
    let now = scratch.now;
    let floor = now + super::FOLD_AHEAD_S - aterm_agent::harness::spend::HORIZON_S;
    scratch.message("msg_old", floor + 30, 10);
    let publisher = Publisher::new(scratch.places());
    let log = Kept::new();
    pass(&publisher, &log, now);
    let floor_ms = u64::try_from(floor).expect("a time") * 1000;
    for record in windows(&log.since(0)) {
        assert!(
            record.window().start_ms() >= floor_ms,
            "a window from before the fold's floor: {record:?}"
        );
    }
}

fn removed_rows(rows: &[(Subject, Row)]) -> Vec<&aterm_metrics::row::QuotaRow> {
    quota_rows(rows)
        .into_iter()
        .filter(|q| q.removed())
        .collect()
}

/// An account the roster no longer lists is published once as removed: one row on its subject,
/// at the pass's time, with no figure; the next pass publishes nothing of it, and neither does a
/// restarted publisher, which reads that row back. NEGATIVE CONTROLS: a pass whose roster does not
/// read, and one with no roster or store to read, remove no account, since a read that failed
/// says nothing of what the roster lists.
#[test]
fn an_account_the_roster_no_longer_lists_is_published_once_as_removed() {
    let scratch = Scratch::new();
    let now = scratch.now;
    let publisher = Publisher::new(scratch.places());
    let log = Kept::new();
    pass(&publisher, &log, now);
    assert_eq!(quota_rows(&log.since(0)).len(), 2, "{:?}", log.since(0));

    std::fs::write(scratch.roster_path(), "[[[ no roster").expect("a roster that does not read");
    let before = log.len();
    pass(&publisher, &log, now + 60);
    let unread = |rows: &[(Subject, Row)]| -> Vec<String> {
        rows.iter()
            .filter_map(|(_, row)| match row {
                Row::Unread(u) => Some(u.pools.iter().map(|p| p.as_str().to_owned()).collect()),
                _ => None,
            })
            .next_back()
            .unwrap_or_default()
    };
    assert_eq!(
        unread(&log.since(before)),
        ["claude"],
        "the pool that did not read"
    );
    let mid = log.len();
    let mut blind = scratch.places();
    blind.config = None;
    let blind = Publisher::new(blind);
    pass(&blind, &log, now + 60);
    let every: Vec<String> = accounts::Cli::ALL
        .iter()
        .map(|pool| pool.id().to_owned())
        .collect();
    assert_eq!(
        unread(&log.since(mid)),
        every,
        "with nothing to read, every pool"
    );
    let published = log.since(before);
    assert!(
        removed_rows(&published).is_empty(),
        "a failed read removed an account: {published:?}"
    );

    scratch.roster(&["pool-a"]);
    let before = log.len();
    let report = pass(&publisher, &log, now + 120);
    let published = log.since(before);
    let removed = removed_rows(&published);
    assert_eq!(removed.len(), 1, "{published:?}");
    let gone = removed[0];
    assert_eq!(
        (gone.label.as_str(), gone.account.to_string(), gone.at),
        (
            "pool-b",
            POOL_B.to_owned(),
            u64::try_from(now + 120).expect("a time")
        )
    );
    assert_eq!(
        (
            gone.pct5,
            gone.five_hour_reset,
            gone.walled_until,
            gone.enrolled
        ),
        (None, None, None, false),
        "a removed row carries no figure"
    );
    assert_eq!(quota_rows(&published).len(), 1, "pool-a's row is unchanged");
    assert_eq!(report.refused, None);

    let before = log.len();
    pass(&publisher, &log, now + 180);
    let restarted = Publisher::new(scratch.places());
    pass(&restarted, &log, now + 240);
    let published = log.since(before);
    assert!(
        quota_rows(&published).is_empty(),
        "a removed account was published again: {published:?}"
    );
}

/// A quota row carries the plan this Mac's accounts view names for its account: the stats
/// series' newest `plan` row with its tier (`max-20x`) where Claude Code's profile showed one,
/// else the `subscriptionType` its login's last renewal read (`pro`), whatever word that is. A
/// plan the profile shows later publishes the row again; an unchanged pass publishes nothing.
#[test]
fn a_pass_publishes_each_accounts_plan_from_this_macs_records() {
    use aterm_agent::harness::account_series::{self, PlanSaid};
    let scratch = Scratch::new();
    let now = scratch.now;
    let store = accounts::Store::at(&scratch.harness());
    let (mut state, _) = store.load(now).expect("the accounts store");
    let (mut logins, _) = store.load_logins(now).expect("the logins");
    let dir = scratch.login("pool-b");
    accounts::note_renewed(
        &mut state,
        &mut logins,
        "pool-b",
        None,
        dir.to_str().expect("utf-8"),
        Some("pro"),
        now - 600,
    );
    store.save_logins(&logins).expect("the logins save");
    store.save(&state).expect("the accounts store saves");
    let machine = state.machine.clone();
    let profile = |word: &str, tier: Option<&str>, at: i64| {
        let plan = PlanSaid {
            word: word.to_owned(),
            tier: tier.map(str::to_owned),
            src: "profile".to_owned(),
        };
        account_series::note_plan(&scratch.harness(), "pool-a", &machine, &plan, at)
            .expect("the series notes the plan");
    };
    profile("max", Some("20x"), now - 300);

    let publisher = Publisher::new(scratch.places());
    let log = Kept::new();
    pass(&publisher, &log, now);
    let rows = log.since(0);
    let plans: BTreeMap<&str, Option<&str>> = quota_rows(&rows)
        .into_iter()
        .map(|q| (q.label.as_str(), q.plan.as_ref().map(Word::as_str)))
        .collect();
    assert_eq!(
        plans,
        BTreeMap::from([("pool-a", Some("max-20x")), ("pool-b", Some("pro"))]),
        "{rows:?}"
    );

    let before = log.len();
    assert_eq!(
        pass(&publisher, &log, now).rows,
        0,
        "{:?}",
        log.since(before)
    );

    profile("ultra", None, now - 10);
    let before = log.len();
    pass(&publisher, &log, now + 60);
    let moved = log.since(before);
    let quota = quota_rows(&moved);
    assert_eq!(quota.len(), 1, "{moved:?}");
    assert_eq!(
        (
            quota[0].label.as_str(),
            quota[0].plan.as_ref().map(Word::as_str)
        ),
        ("pool-a", Some("ultra"))
    );
}

/// A Claude Code run in a home that is neither the default nor a login's, which started and
/// ended between two passes, is folded: its launch noted the home before it served, so a pass
/// that finds no Claude Code running still lists the home and the run's minute counts its
/// message beside the default home's. NEGATIVE CONTROL: with no note the same pass lists no such
/// home, and the minute goes out without the run.
#[test]
fn a_short_run_in_a_home_its_launch_noted_is_folded() {
    let minute = |noted: bool| {
        let scratch = Scratch::new();
        let now = scratch.now;
        let custom = scratch.path("custom-claude");
        if noted {
            claude_homes::note(&scratch.harness(), &custom, now - 3800).expect("the launch note");
        }
        scratch.message_in(&scratch.path("home/.claude"), "msg_here", now - 3700, 10);
        scratch.message_in(&custom, "msg_run", now - 3695, 20);
        let publisher = Publisher::new(scratch.places_listed());
        let log = Kept::new();
        pass(&publisher, &log, now);
        windows(&log.since(0))
            .into_iter()
            .filter(|w| w.metric() == Metric::Tokens(Cli::Claude))
            .map(|w| (w.summary().count(), w.summary().sum()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        minute(true),
        [(2, 3 + 10 + 3 + 20)],
        "the run's message counts"
    );
    assert_eq!(
        minute(false),
        [(1, 3 + 10)],
        "with no note the run is missed"
    );
}
