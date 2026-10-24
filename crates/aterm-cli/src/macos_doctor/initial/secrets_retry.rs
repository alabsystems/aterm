// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Backoff and retry tracking for the initial-setup Secrets stage.

use super::{PROFILE, Path, Row, Value, durable_directory, json, write_json};
use aterm_machines::init_secrets::SecretsOps;
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

/// What can change a Secrets try's answer without the person, read before and after it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Pace {
    /// This aterm build ([`aterm_types::version::build_id`]).
    pub(super) build: String,
    /// A digest of the network this Mac is on and of its vault state ([`marks`]).
    pub(super) marks: String,
    /// Now, in seconds since the epoch.
    pub(super) now: u64,
}

/// Where a [`Pace`] is read from: [`Live`], or a test's network and vault.
pub(super) trait Reads {
    /// This aterm build.
    fn build(&self) -> String;
    /// Now, in seconds since the epoch.
    fn now(&self) -> u64;
    /// Stable local interface facts as text.
    fn routes(&self) -> String;
    /// Vault handle for the single console account, if any.
    fn vault(&self) -> Option<Arc<dyn SecretsOps>>;
}

/// The pace `reads` reads now.
pub(super) fn pace(reads: &dyn Reads) -> Pace {
    Pace {
        build: reads.build(),
        marks: marks(reads),
        now: reads.now(),
    }
}

fn marks(reads: &dyn Reads) -> String {
    use aterm_secrets::client::Membership;
    use aterm_secrets::service::State;
    let mut marks = reads.routes();
    match reads.vault() {
        None => marks.push_str("; no single console account"),
        Some(vault) => {
            match vault.status() {
                Ok(mut status) => {
                    if let Membership::In { heard, .. } = &mut status.membership {
                        *heard = 0;
                    }
                    marks.push_str(&format!("; {status:?}"));
                }
                Err(error) => marks.push_str(&format!("; status: {error}")),
            }
            let running = vault
                .subscriber()
                .map(|state| matches!(state, State::Installed { pid: Some(_), .. }));
            marks.push_str(&format!("; subscriber running: {running:?}"));
            marks.push_str(&format!("; runs from: {:?}", vault.fleeting()));
        }
    }
    let digest = aterm_digest::Sha256::digest(marks.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
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

    fn routes(&self) -> String {
        format!("{:?}", aterm_secrets::network::interfaces().ok())
    }

    fn vault(&self) -> Option<Arc<dyn SecretsOps>> {
        let ctx =
            aterm_machines::init::initial_setup_context(&aterm_machines::port::Live::new()).ok()?;
        aterm_machines::init_secrets::unattended(&ctx).ok()
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
    marks: String,
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
        let text = |key: &str| record[key].as_str().map(str::to_owned);
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
            marks: text("marks")?,
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
        write_json(
            &root.join(RECORD),
            &json!({
                "profile": PROFILE, "revision": self.revision, "builds": self.builds,
                "marks": self.marks, "tries": self.tries, "failures": self.failures,
                "at": self.at, "rows": rows,
            }),
        )
    }

    fn same_row(&self, revision: u64, pace: &Pace) -> bool {
        self.revision == revision && self.marks == pace.marks
    }

    fn unchanged(&self, revision: u64, pace: &Pace) -> bool {
        self.same_row(revision, pace) && self.builds.contains(&pace.build)
    }

    const fn held(&self) -> bool {
        self.failures >= TRIES
    }
}

/// Returns the cached rows while the last unfinished attempt of `revision` is paused or held.
pub(super) fn paused(root: &Path, revision: u64, pace: &Pace) -> Option<Vec<Row>> {
    let last = Attempt::load(root)?;
    let due = last.at.saturating_add(pause(last.tries).as_secs());
    (last.unchanged(revision, pace) && (last.held() || (last.at..due).contains(&pace.now))).then(
        || {
            last.rows
                .into_iter()
                .map(|(name, status, detail)| Row {
                    name,
                    status,
                    detail,
                })
                .collect()
        },
    )
}

/// The deadline of a retryable try now due, for a parked worker. Waiting on a
/// person has no timer, and two failed changes stay held until a fact changes.
/// The worker consumes a deadline once; only a later actual try advances it.
pub(super) fn retry_due(root: &Path, revision: u64, now: u64) -> Option<u64> {
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
    let due = last.at.saturating_add(pause(last.tries).as_secs());
    (!(last.at..due).contains(&now)).then_some(due)
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
    let (tries, failures, builds) = match Attempt::load(root).filter(|l| l.same_row(revision, pace))
    {
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
        row.detail = format!("{}; {note}", row.detail);
    }
    Attempt {
        revision,
        builds,
        marks: pace.marks.clone(),
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

const HELD: &str = "aterm tried this twice by itself and it did not take, so it tries again only when this Mac's network or vault changes, after an update of aterm, or on Set up now or Join in System State";

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
mod tests {
    use super::*;

    fn pace(marks: &str, now: u64) -> Pace {
        Pace {
            build: "1.0.0+gabc".into(),
            marks: marks.into(),
            now,
        }
    }

    fn blocked(detail: &str) -> Vec<Row> {
        vec![Row {
            name: "Secrets vault".into(),
            status: "blocked".into(),
            detail: detail.into(),
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
}
