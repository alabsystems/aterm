// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Backoff and retry tracking for the initial-setup Secrets stage.

use super::{PROFILE, Path, Row, Value, durable_directory, json, write_json};
use aterm_machines::init_secrets::SecretsOps;
use aterm_secrets::client::MaterializationMark;
use std::sync::Arc;
use std::time::Duration;

/// The record of the last unfinished try.
pub(super) const RECORD: &str = "secrets-attempt.json";

/// The pause after a first unfinished try: the worker's own cadence.
pub(super) const FIRST_PAUSE: Duration = Duration::from_secs(60);

/// Maximum backoff pause between unfinished Secrets attempts.
pub(super) const LONGEST: Duration = Duration::from_secs(4 * 60);

/// Maximum consecutive failed change attempts before waiting for an external change.
pub(super) const TRIES: u32 = 2;

const BUILDS: usize = 16;

/// Independently readable facts: an unknown component does not hide another's change.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Inputs {
    pub(super) routes: Option<String>,
    pub(super) vault: Option<String>,
    pub(super) execution: Option<String>,
    pub(super) subscriber: Option<bool>,
}

impl Inputs {
    /// Every known current fact agrees with an earlier known reading.
    fn known_matches(&self, previous: &Self) -> bool {
        self.routes
            .as_ref()
            .is_none_or(|value| previous.routes.as_ref() == Some(value))
            && self
                .vault
                .as_ref()
                .is_none_or(|value| previous.vault.as_ref() == Some(value))
            && self
                .execution
                .as_ref()
                .is_none_or(|value| previous.execution.as_ref() == Some(value))
            && self
                .subscriber
                .is_none_or(|value| previous.subscriber == Some(value))
    }

    /// A failed read leaves only its own last known fact in place.
    fn retain_known_from(&mut self, previous: &Self) {
        if self.routes.is_none() {
            self.routes.clone_from(&previous.routes);
        }
        if self.vault.is_none() {
            self.vault.clone_from(&previous.vault);
        }
        if self.execution.is_none() {
            self.execution.clone_from(&previous.execution);
        }
        if self.subscriber.is_none() {
            self.subscriber = previous.subscriber;
        }
    }
}

/// What can change a Secrets try's answer without the person, read before and after it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Pace {
    /// This aterm build ([`aterm_types::version::build_id`]).
    pub(super) build: String,
    /// A digest of known network/vault/service facts ([`marks`]); a failed
    /// local read supplies no new fact against an earlier hold.
    pub(super) marks: Option<String>,
    /// Named facts for modern attempts; absent only for a legacy opaque reading.
    pub(super) inputs: Option<Inputs>,
    /// A coherent local materialization reading; unknown never erases a known one.
    pub(super) materialization: Option<MaterializationMark>,
    /// Now, in seconds since the epoch.
    pub(super) now: u64,
}

impl Pace {
    /// Keep each input independently when its post-stage read is unknown.
    /// Materialization includes its source identity and supplies only change evidence.
    pub(super) fn retain_known_from(&mut self, before: &Self) {
        let compatible = match (&self.inputs, &before.inputs) {
            (Some(current), Some(previous)) => current.known_matches(previous),
            _ => self.marks.is_none() || self.marks == before.marks,
        };
        if let (Some(current), Some(previous)) = (&mut self.inputs, &before.inputs) {
            current.retain_known_from(previous);
        }
        if self.marks.is_none() && compatible {
            self.marks.clone_from(&before.marks);
        }
        if self.materialization.is_none() {
            self.materialization = before.materialization;
        }
    }
}

/// Where a [`Pace`] is read from: [`Live`], or a test's network and vault.
pub(super) trait Reads {
    /// This aterm build.
    fn build(&self) -> String;
    /// Now, in seconds since the epoch.
    fn now(&self) -> u64;
    /// Stable local interface facts as text.
    fn routes(&self) -> Option<String>;
    /// Vault handle for the single console account, if any.
    fn vault(&self) -> Option<Arc<dyn SecretsOps>>;
    /// Local vault files, when this reader can observe materialization.
    fn paths(&self) -> Option<aterm_secrets::client::Paths> {
        None
    }
}

/// The pace `reads` reads now.
pub(super) fn pace(reads: &dyn Reads) -> Pace {
    let (marks, inputs) = marks(reads);
    Pace {
        build: reads.build(),
        marks,
        inputs: Some(inputs),
        materialization: reads.paths().and_then(|paths| {
            aterm_secrets::client::materialization_mark(&paths)
                .ok()
                .flatten()
        }),
        now: reads.now(),
    }
}

fn marks(reads: &dyn Reads) -> (Option<String>, Inputs) {
    use aterm_secrets::client::Membership;
    let routes = reads.routes();
    let vault = reads.vault();
    // Read independently: a service error must not conceal a changed network or vault.
    let status = vault
        .as_ref()
        .and_then(|vault| vault.status().ok())
        .map(|mut status| {
            if let Membership::In { heard, .. } = &mut status.membership {
                *heard = 0;
            }
            format!("{status:?}")
        });
    let running = vault
        .as_ref()
        .and_then(|vault| vault.subscriber().ok())
        .map(subscriber_running);
    let execution = vault
        .as_ref()
        .map(|vault| format!("{:?}", vault.fleeting()));
    let inputs = Inputs {
        routes: routes.as_ref().map(|value| fingerprint("routes", value)),
        vault: status.as_ref().map(|value| fingerprint("vault", value)),
        execution: execution
            .as_ref()
            .map(|value| fingerprint("execution", value)),
        subscriber: running,
    };
    let marks = match (routes, status, running, execution) {
        // Preserve the complete known digest's encoding for legacy holds.
        (Some(routes), Some(status), Some(running), Some(execution)) => Some(digest(&format!(
            "{routes}; {status}; subscriber running: Ok({running:?}); runs from: {execution}"
        ))),
        _ => None,
    };
    (marks, inputs)
}

fn fingerprint(component: &str, value: &str) -> String {
    digest(&format!("aterm-initial-secrets-{component}\0{value}"))
}

fn digest(value: &str) -> String {
    aterm_digest::Sha256::digest(value.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(super) fn subscriber_running(state: aterm_secrets::service::State) -> bool {
    matches!(
        state,
        aterm_secrets::service::State::Installed { pid: Some(_), .. }
    )
}

/// This Mac, now.
pub(super) struct Live;

impl Reads for Live {
    fn build(&self) -> String {
        aterm_types::version::build_id()
    }

    fn now(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs())
    }

    fn routes(&self) -> Option<String> {
        let interfaces = aterm_secrets::network::interfaces().ok()?;
        Some(format!("{:?}", Some(interfaces)))
    }

    fn vault(&self) -> Option<Arc<dyn SecretsOps>> {
        let ctx =
            aterm_machines::init::initial_setup_context(&aterm_machines::port::Live::new()).ok()?;
        aterm_machines::init_secrets::unattended(&ctx).ok()
    }

    fn paths(&self) -> Option<aterm_secrets::client::Paths> {
        let ctx =
            aterm_machines::init::initial_setup_context(&aterm_machines::port::Live::new()).ok()?;
        match ctx.users.as_slice() {
            [user] => Some(aterm_secrets::client::Paths::for_home(Path::new(
                &user.home,
            ))),
            _ => None,
        }
    }
}

/// Returns the backoff pause after `tries` consecutive unfinished attempts.
pub(super) fn pause(tries: u32) -> Duration {
    let doublings = tries.saturating_sub(1).min(16);
    FIRST_PAUSE.saturating_mul(1 << doublings).min(LONGEST)
}

/// The last unfinished try of a row of them in setup run `revision`.
#[derive(Debug, PartialEq, Eq)]
struct Attempt {
    revision: u64,
    /// The builds that tried in this row, the latest last.
    builds: Vec<String>,
    marks: Option<String>,
    inputs: Option<Inputs>,
    materialization: Option<MaterializationMark>,
    tries: u32,
    /// How many of the last tries in a row failed (an error row: a change not made).
    failures: u32,
    at: u64,
    rows: Vec<(String, String, String)>,
}

impl Attempt {
    fn load(root: &Path) -> Option<Self> {
        let bytes = std::fs::read(root.join(RECORD)).ok()?;
        let record: Value = aterm_json::from_slice(&bytes).ok()?;
        if record["profile"].as_str() != Some(PROFILE) {
            return None;
        }
        let count = |key: &str| u32::try_from(record[key].as_u64()?).ok();
        let rows = record["rows"]
            .as_array()?
            .iter()
            .map(|row| {
                Some((
                    row["name"].as_str()?.to_owned(),
                    row["status"].as_str()?.to_owned(),
                    row["detail"].as_str()?.to_owned(),
                ))
            })
            .collect::<Option<Vec<_>>>()?;
        let builds = record["builds"]
            .as_array()?
            .iter()
            .map(|build| build.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            revision: record["revision"].as_u64()?,
            builds,
            marks: match record.get("marks")? {
                Value::Null => None,
                marks => Some(marks.as_str()?.to_owned()),
            },
            inputs: match record.get("inputs") {
                None => None,
                Some(inputs) => Some(Inputs::from_record(inputs)?),
            },
            materialization: record["materialization"]
                .as_str()
                .and_then(MaterializationMark::from_cache_key),
            tries: count("tries")?,
            failures: count("failures")?,
            at: record["at"].as_u64()?,
            rows,
        })
    }

    fn save(&self, root: &Path) -> Result<(), String> {
        let rows: Vec<Value> = self
            .rows
            .iter()
            .map(|(name, status, detail)| json!({"name": name, "status": status, "detail": detail}))
            .collect();
        let mut record = json!({
            "profile": PROFILE, "revision": self.revision, "builds": self.builds,
            "marks": self.marks, "tries": self.tries, "failures": self.failures,
            "materialization": self.materialization.map(|mark| mark.cache_key()),
            "at": self.at, "rows": rows,
        });
        if let Some(inputs) = &self.inputs {
            record
                .as_object_mut()
                .unwrap()
                .insert("inputs".into(), inputs.to_record());
        }
        write_json(&root.join(RECORD), &record)
    }

    fn same_inputs(&self, pace: &Pace) -> bool {
        match (&pace.inputs, &self.inputs) {
            (Some(current), Some(previous)) => current.known_matches(previous),
            _ => pace
                .marks
                .as_ref()
                .is_none_or(|marks| self.marks.as_ref() == Some(marks)),
        }
    }

    fn same_row(&self, revision: u64, pace: &Pace) -> bool {
        self.revision == revision
            && self.same_inputs(pace)
            && pace
                .materialization
                .is_none_or(|mark| self.materialization == Some(mark))
    }

    fn unchanged(&self, revision: u64, pace: &Pace) -> bool {
        self.same_row(revision, pace) && self.builds.contains(&pace.build)
    }

    const fn held(&self) -> bool {
        self.failures >= TRIES
    }
}

impl Inputs {
    fn from_record(record: &Value) -> Option<Self> {
        fn text(record: &Value, field: &str) -> Option<Option<String>> {
            match record.get(field)? {
                Value::Null => Some(None),
                value => Some(Some(value.as_str()?.to_owned())),
            }
        }
        Some(Self {
            routes: text(record, "routes")?,
            vault: text(record, "vault")?,
            execution: text(record, "execution")?,
            subscriber: match record.get("subscriber")? {
                Value::Null => None,
                value => Some(value.as_bool()?),
            },
        })
    }

    fn to_record(&self) -> Value {
        json!({
            "routes": self.routes, "vault": self.vault,
            "execution": self.execution, "subscriber": self.subscriber,
        })
    }

    fn fully_known(&self) -> bool {
        self.routes.is_some()
            && self.vault.is_some()
            && self.execution.is_some()
            && self.subscriber.is_some()
    }
}

/// Returns the cached rows while the last unfinished attempt of `revision` is paused or held.
/// The caller holds the profile writer lock. A legacy hold with an exactly
/// matching complete reading can gain component fingerprints without spending
/// or resetting an attempt; a failed cache update leaves that hold unchanged.
pub(super) fn paused(root: &Path, revision: u64, pace: &Pace) -> Option<Vec<Row>> {
    let mut last = Attempt::load(root)?;
    if last.revision == revision
        && last.inputs.is_none()
        && pace.marks.is_some()
        && pace.marks == last.marks
        && let Some(inputs) = pace.inputs.as_ref().filter(|inputs| inputs.fully_known())
    {
        last.inputs = Some(inputs.clone());
        let _ = last.save(root);
    }
    let due = last.at.saturating_add(pause(last.tries).as_secs());
    (last.unchanged(revision, pace) && (last.held() || (last.at..due).contains(&pace.now))).then(
        || {
            last.rows
                .into_iter()
                .map(|(name, status, detail)| Row {
                    name,
                    status,
                    detail,
                    elapsed_ms: None,
                })
                .collect()
        },
    )
}

/// The deadline of a retryable try now due, for a parked worker. A row that
/// waits ([`super::WAITING`]: a person, an approval, a vault this network does
/// not have) has no timer, and two failed changes stay held until a fact
/// changes; an unreached vault ([`super::UNREACHED`]) is asked again. The
/// worker consumes a deadline once; only a later actual try advances it.
pub(super) fn retry_due(root: &Path, revision: u64, now: u64) -> Option<u64> {
    let (at, due) = retry_window(root, revision)?;
    (!(at..due).contains(&now)).then_some(due)
}

/// The deadline of a retryable try not due yet ([`retry_due`]'s, while it is still ahead of
/// `now`): when a parked worker looks again by itself, with no timer before it.
pub(super) fn retry_ahead(root: &Path, revision: u64, now: u64) -> Option<u64> {
    let (at, due) = retry_window(root, revision)?;
    (at..due).contains(&now).then_some(due)
}

/// The last retryable try's time and the deadline of the next ([`retry_due`]).
fn retry_window(root: &Path, revision: u64) -> Option<(u64, u64)> {
    let last = Attempt::load(root)?;
    if last.revision != revision
        || last.held()
        || !last
            .rows
            .iter()
            .any(|(_, status, _)| !matches!(status.as_str(), "ok" | "applied" | super::WAITING))
    {
        return None;
    }
    Some((last.at, last.at.saturating_add(pause(last.tries).as_secs())))
}

/// Records the outcome of a Secrets attempt for `revision` at `pace`.
/// # Errors
/// The record cannot be written or removed.
pub(super) fn record(
    root: &Path,
    revision: u64,
    pace: &Pace,
    ready: bool,
    rows: &mut [Row],
) -> Result<(), String> {
    if ready {
        return match std::fs::remove_file(root.join(RECORD)) {
            Ok(()) => durable_directory(root).map_err(|error| error.to_string()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("cannot reset the secrets retry pause: {error}")),
        };
    }
    let failed = rows.iter().any(|row| row.status == "error");
    let previous = Attempt::load(root);
    let marks = pace.marks.clone().or_else(|| {
        previous
            .as_ref()
            .filter(|last| last.revision == revision && last.same_inputs(pace))
            .and_then(|last| last.marks.clone())
    });
    let inputs = match &pace.inputs {
        Some(current) => {
            let mut current = current.clone();
            if let Some(before) = previous
                .as_ref()
                .filter(|last| last.revision == revision)
                .and_then(|last| last.inputs.as_ref())
            {
                current.retain_known_from(before);
            }
            Some(current)
        }
        None => previous
            .as_ref()
            .filter(|last| last.revision == revision && last.same_inputs(pace))
            .and_then(|last| last.inputs.clone()),
    };
    let materialization = pace.materialization.or_else(|| {
        previous
            .as_ref()
            // The mark already binds source and version. Other changed facts
            // do not turn an unreadable materialization into new evidence.
            .filter(|last| last.revision == revision)
            .and_then(|last| last.materialization)
    });
    let (tries, failures, builds) = match previous.filter(|l| l.same_row(revision, pace)) {
        Some(last) if last.builds.contains(&pace.build) => {
            (last.tries.saturating_add(1), last.failures, last.builds)
        }
        Some(mut last) => {
            last.builds.push(pace.build.clone());
            let over = last.builds.len().saturating_sub(BUILDS);
            last.builds.drain(..over);
            (1, last.failures, last.builds)
        }
        None => (1, 0, vec![pace.build.clone()]),
    };
    let failures = if failed {
        failures.saturating_add(1)
    } else {
        0
    };
    let note = if failures >= TRIES {
        HELD.to_owned()
    } else {
        note(pause(tries))
    };
    for row in rows
        .iter_mut()
        .filter(|row| !matches!(row.status.as_str(), "ok" | "applied"))
    {
        // A row that waits has no timer ([`retry_due`]).
        let note = if row.status == super::WAITING && failures < TRIES {
            WAITS
        } else {
            &note
        };
        row.detail = format!("{}; {note}", row.detail);
    }
    Attempt {
        revision,
        builds,
        marks,
        inputs,
        materialization,
        tries,
        failures,
        at: pace.now,
        rows: rows
            .iter()
            .map(|row| (row.name.clone(), row.status.clone(), row.detail.clone()))
            .collect(),
    }
    .save(root)
}

/// The page's rows for the Secrets stage's `rows` (design §3.6): the vault's one row, named as the
/// page names it, in the page's words. A step that failed says what aterm does next, as
/// [`record`] last left the pause ([`PAGE_RETRIES`], or [`PAGE_HELD`] once two tries failed); one
/// that could not be confirmed says no more; the rest carry no detail, which the page never shows
/// for them. The process, the paths and the vault's own words stay in `rows`, for `aterm doctor`
/// and the log.
pub(super) fn page(root: &Path, rows: &[Row]) -> super::PassPage {
    let held = Attempt::load(root).is_some_and(|last| last.held());
    let mut page = super::PassPage::default();
    for row in rows {
        let detail = match row.status.as_str() {
            "error" if held => PAGE_HELD,
            "error" => PAGE_RETRIES,
            _ => "",
        };
        let row = Row {
            name: aterm_types::macos_setup::SECRETS_VAULT_ROW.into(),
            status: row.status.clone(),
            detail: detail.into(),
            elapsed_ms: None,
        };
        let said = page
            .rows
            .iter()
            .any(|seen| seen.status == row.status && seen.detail == row.detail);
        if !said {
            page.rows.push(row);
        }
    }
    page
}

/// What the page says after a failed Secrets try that aterm tries again on its pause: a
/// transient failure that waits for that try ([`aterm_types::macos_setup::FailureKind`]).
pub(super) const PAGE_RETRIES: &str = aterm_types::macos_setup::TRIES_AGAIN_BY_ITSELF;

/// What the page says once two tries in a row failed: its tries are spent, so it failed, and only
/// a change, or a person, asks again.
pub(super) const PAGE_HELD: &str = aterm_types::macos_setup::TRIED_TWICE;

const HELD: &str = "aterm tried this twice by itself and it did not take, so it tries again only when this Mac's network or vault changes, after an update of aterm, or on Set up now or Join in System State";

/// What a waiting row's try adds: no timer, only a change or a person.
const WAITS: &str = "aterm tries again by itself when this Mac's network or vault changes, or on Set up now or Join in System State";

fn note(pause: Duration) -> String {
    let minutes = pause.as_secs() / 60;
    let after = if minutes == 1 {
        "a minute".to_owned()
    } else {
        format!("{minutes} minutes")
    };
    format!(
        "aterm tries again by itself {after} after this try, and at once when this Mac's network or vault changes or on Set up now or Join in System State"
    )
}

#[cfg(test)]
mod unknown_inputs_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn pace(marks: &str, now: u64) -> Pace {
        Pace {
            build: "1.0.0+gabc".into(),
            marks: Some(marks.into()),
            inputs: None,
            materialization: None,
            now,
        }
    }

    fn blocked(detail: &str) -> Vec<Row> {
        vec![Row {
            name: "Secrets vault".into(),
            status: "blocked".into(),
            detail: detail.into(),
            elapsed_ms: None,
        }]
    }

    #[test]
    fn the_pause_doubles_to_four_minutes() {
        let minutes: Vec<u64> = (1..=6).map(|tries| pause(tries).as_secs() / 60).collect();
        assert_eq!(minutes, [1, 2, 4, 4, 4, 4]);
        assert_eq!(pause(u32::MAX), LONGEST);
        assert!(LONGEST < Duration::from_secs(5 * 60));
    }

    #[test]
    fn a_pause_ends_with_its_time_or_any_change() {
        let root = aterm_tempfile::tempdir().unwrap();
        let root = root.path();
        let mut rows = blocked("the vault does not answer");
        record(root, 3, &pace("m", 1_000), false, &mut rows).unwrap();
        assert_eq!(
            rows[0].detail,
            "the vault does not answer; aterm tries again by itself a minute after this try, and at once when this Mac's network or vault changes or on Set up now or Join in System State"
        );
        let shown = paused(root, 3, &pace("m", 1_059)).expect("paused");
        assert_eq!(shown[0].detail, rows[0].detail);
        assert_eq!(shown[0].status, "blocked");
        assert!(paused(root, 3, &pace("m", 1_060)).is_none(), "due");
        assert!(paused(root, 3, &pace("n", 1_001)).is_none(), "marks");
        assert!(paused(root, 4, &pace("m", 1_001)).is_none(), "revision");
        let mut other = pace("m", 1_001);
        other.build = "1.0.0+gdef".into();
        assert!(paused(root, 3, &other).is_none(), "build");
        assert!(paused(root, 3, &pace("m", 999)).is_none(), "clock set back");

        let mut rows = blocked("the vault does not answer");
        record(root, 3, &pace("m", 1_060), false, &mut rows).unwrap();
        assert!(rows[0].detail.contains("2 minutes after this try"));
        assert!(paused(root, 3, &pace("m", 1_060 + 119)).is_some());
        let mut rows = blocked("the vault does not answer");
        record(root, 3, &pace("n", 1_200), false, &mut rows).unwrap();
        assert!(rows[0].detail.contains("a minute after this try"));

        std::fs::write(root.join(RECORD), b"not json").unwrap();
        assert!(paused(root, 3, &pace("n", 1_201)).is_none(), "unreadable");

        let mut rows = blocked("the vault does not answer");
        record(root, 3, &pace("n", 1_300), false, &mut rows).unwrap();
        assert!(paused(root, 3, &pace("n", 1_301)).is_some());
        let mut done = vec![Row {
            name: "Secrets vault".into(),
            status: "ok".into(),
            detail: "in vault alab (seq 7)".into(),
            elapsed_ms: None,
        }];
        record(root, 3, &pace("n", 1_302), true, &mut done).unwrap();
        assert_eq!(done[0].detail, "in vault alab (seq 7)");
        assert!(!root.join(RECORD).exists());
        assert!(paused(root, 3, &pace("n", 1_303)).is_none());
    }

    #[test]
    fn parked_retry_deadlines_belong_to_the_unfinished_attempt_and_hold_failed_changes() {
        let root = aterm_tempfile::tempdir().unwrap();
        let root = root.path();
        assert_eq!(retry_due(root, 3, 2_000), None);
        record(root, 3, &pace("m", 1_000), false, &mut blocked("offline")).unwrap();
        assert_eq!(retry_due(root, 3, 1_059), None);
        assert_eq!(retry_due(root, 3, 1_060), Some(1_060));
        assert_eq!(retry_due(root, 3, 1_300), Some(1_060));
        assert_eq!(retry_due(root, 4, 1_300), None, "another run's deadline");
        record(root, 3, &pace("m", 1_300), false, &mut blocked("offline")).unwrap();
        assert_eq!(retry_due(root, 3, 1_419), None);
        assert_eq!(
            retry_due(root, 3, 1_420),
            Some(1_420),
            "a new try advances it"
        );
        let mut waiting = blocked("an administrator must approve");
        waiting[0].status = super::super::WAITING.into();
        record(root, 3, &pace("m", 1_500), false, &mut waiting).unwrap();
        assert_eq!(retry_due(root, 3, 9_000), None);
        assert_eq!(
            waiting[0].detail,
            format!("an administrator must approve; {WAITS}"),
            "a row that waits names no timer"
        );
        let mut unreached = blocked(super::super::scope::NOT_REACHED);
        unreached[0].status = super::super::UNREACHED.into();
        record(root, 3, &pace("u", 1_600), false, &mut unreached).unwrap();
        assert!(unreached[0].detail.contains("a minute after this try"));
        assert_eq!(retry_due(root, 3, 1_659), None);
        assert_eq!(
            retry_due(root, 3, 1_660),
            Some(1_660),
            "a vault that did not answer is asked again"
        );
        let failed = || {
            let mut rows = blocked("the change failed");
            rows[0].status = "error".into();
            rows
        };
        record(root, 3, &pace("m", 2_000), false, &mut failed()).unwrap();
        record(root, 3, &pace("m", 3_000), false, &mut failed()).unwrap();
        assert_eq!(retry_due(root, 3, 9_000), None, "failed changes are held");
        let mut rows = blocked("offline on another network");
        record(root, 3, &pace("n", 4_000), false, &mut rows).unwrap();
        assert_eq!(
            retry_due(root, 3, 4_060),
            Some(4_060),
            "external change re-arms it"
        );
    }

    #[test]
    fn builds_taking_turns_share_one_pause() {
        let root = aterm_tempfile::tempdir().unwrap();
        let root = root.path();
        let mut old = pace("m", 1_000);
        old.build = "1.0.0+gold".into();
        let mut new = pace("m", 1_010);
        new.build = "1.0.0+gnew".into();
        record(root, 3, &old, false, &mut blocked("offline")).unwrap();
        assert!(
            paused(root, 3, &new).is_none(),
            "a build that has not tried"
        );
        record(root, 3, &new, false, &mut blocked("offline")).unwrap();
        old.now = 1_020;
        assert!(
            paused(root, 3, &old).is_some(),
            "the old build is in the row"
        );
        new.now = 1_030;
        assert!(paused(root, 3, &new).is_some());
        let mut third = pace("m", 1_040);
        third.build = "1.0.0+gthird".into();
        assert!(paused(root, 3, &third).is_none(), "the control");
    }

    #[test]
    fn a_change_that_keeps_failing_is_tried_twice_then_held_until_a_change() {
        let root = aterm_tempfile::tempdir().unwrap();
        let root = root.path();
        let failed = || {
            vec![Row {
                name: "Secrets vault".into(),
                status: "error".into(),
                detail: "joining the vault: which nothing pins".into(),
                elapsed_ms: None,
            }]
        };
        let mut rows = failed();
        record(root, 3, &pace("m", 1_000), false, &mut rows).unwrap();
        assert!(
            rows[0].detail.contains("a minute after this try"),
            "{rows:?}"
        );
        assert!(
            paused(root, 3, &pace("m", 1_060)).is_none(),
            "the second try"
        );
        let mut rows = failed();
        record(root, 3, &pace("m", 1_060), false, &mut rows).unwrap();
        assert!(rows[0].detail.ends_with(HELD), "{rows:?}");
        for later in [1_061, 1_060 + 3_600, 1_060 + 86_400] {
            let shown = paused(root, 3, &pace("m", later)).expect("held");
            assert_eq!(shown[0].detail, rows[0].detail);
        }
        assert!(paused(root, 3, &pace("n", 1_100)).is_none(), "marks");
        assert!(paused(root, 4, &pace("m", 1_100)).is_none(), "run");
        let mut other = pace("m", 1_100);
        other.build = "1.0.0+gdef".into();
        assert!(paused(root, 3, &other).is_none(), "build");

        let mut rows = failed();
        record(root, 3, &other, false, &mut rows).unwrap();
        assert!(rows[0].detail.ends_with(HELD), "{rows:?}");
        assert!(
            paused(root, 3, &pace("m", 1_200)).is_some(),
            "held for both"
        );
        let mut third = pace("m", 1_200);
        third.build = "1.0.0+gthird".into();
        let mut rows = blocked("the vault does not answer");
        record(root, 3, &third, false, &mut rows).unwrap();
        assert!(
            rows[0].detail.contains("a minute after this try"),
            "{rows:?}"
        );
        assert!(paused(root, 3, &pace("m", 1_260)).is_none(), "the control");
    }

    #[test]
    fn an_unknown_materialization_read_never_erases_a_known_hold() {
        let root = aterm_tempfile::tempdir().unwrap();
        let root = root.path();
        let failed = || {
            vec![Row {
                name: "Secrets vault".into(),
                status: "error".into(),
                detail: "injected change failure".into(),
                elapsed_ms: None,
            }]
        };
        let unknown = pace("unchanged source and interface", 1_000);
        record(root, 3, &unknown, false, &mut failed()).unwrap();
        record(root, 3, &unknown, false, &mut failed()).unwrap();
        assert!(paused(root, 3, &unknown).is_some());

        let mut known = unknown.clone();
        known.materialization = MaterializationMark::from_cache_key(&"01".repeat(32));
        assert!(known.materialization.is_some());
        assert!(
            paused(root, 3, &known).is_none(),
            "the first conclusive local reading permits one new try"
        );
        record(root, 3, &known, false, &mut failed()).unwrap();
        known.now += FIRST_PAUSE.as_secs();
        record(root, 3, &known, false, &mut failed()).unwrap();
        assert!(paused(root, 3, &known).is_some());
        assert!(paused(root, 3, &unknown).is_some(), "busy is no new fact");
        // Even a recorded outcome whose post-stage read was contended keeps
        // the earlier identity; an unknown/known cycle never re-arms it.
        record(root, 3, &unknown, false, &mut failed()).unwrap();
        assert_eq!(
            Attempt::load(root).unwrap().materialization,
            known.materialization
        );
        assert!(paused(root, 3, &known).is_some());
        known.materialization = MaterializationMark::from_cache_key(&"02".repeat(32));
        assert!(
            paused(root, 3, &known).is_none(),
            "a real change releases it"
        );
    }
}
