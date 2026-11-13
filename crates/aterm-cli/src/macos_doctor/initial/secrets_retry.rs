// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Backoff and retry tracking for the initial-setup Secrets stage: every unfinished try comes due
//! again by itself on its pause ([`aterm_types::retry_schedule::Backoff`]), kept in the try's
//! record so a relaunch or a reboot reads the same deadline; no count of tries turns the pause
//! into a hold that waits for a fact to change or a person's press.

use super::{PROFILE, Path, Row, Value, durable_directory, json, write_json};
use aterm_machines::init_secrets::SecretsOps;
use aterm_secrets::client::MaterializationMark;
use aterm_types::retry_schedule::Backoff;
use std::sync::Arc;
use std::time::Duration;

/// The record of the last unfinished try.
pub(super) const RECORD: &str = "secrets-attempt.json";

/// The pause after a first unfinished try: the worker's own cadence.
pub(super) const FIRST_PAUSE: Duration = Duration::from_secs(60);

/// Maximum backoff pause between unfinished Secrets attempts.
pub(super) const LONGEST: Duration = Duration::from_secs(4 * 60);

/// Maximum backoff pause between tries the ALab vault did not answer at all ([`super::UNREACHED`]:
/// a Mac away from the ALab LAN, or the vault's server down), and between tries once
/// [`SLOWER_AFTER`] in a row failed to make their change: asked again less and less often, up to
/// once an hour, and at once when this Mac moves to another network ([`Reads::routes`]).
pub(super) const LONGEST_UNREACHED: Duration = Duration::from_secs(60 * 60);

/// Failed changes in a row (an error row: the subscriber would not install or stay running, a
/// write failed) after which the next tries come less often, on the hour-long pause of a vault
/// that did not answer: a change that keeps failing is not made every few minutes, yet is never
/// given up on.
pub(super) const SLOWER_AFTER: u32 = 2;

const BUILDS: usize = 16;

/// Independently readable facts: an unknown component does not hide another's change.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Inputs {
    pub(super) routes: Option<String>,
    /// The part of the routes that puts this Mac on its network, its interface and address
    /// ([`aterm_secrets::network::route_place`]): read while the gateway's hardware address is
    /// not resolved yet, so a move it tells is a move even then. `None` in an older build's
    /// record, which then compares by its routes alone.
    pub(super) place: Option<String>,
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
            && self.place.as_ref().is_none_or(|value| {
                previous
                    .place
                    .as_ref()
                    .is_none_or(|previous| previous == value)
            })
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
        if self.place.is_none() {
            self.place.clone_from(&previous.place);
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
    /// This aterm build ([`aterm_stamp::build_id`]).
    pub(super) build: String,
    /// A digest of known network/vault/service facts ([`marks`]); a failed
    /// local read supplies no new fact against an earlier hold.
    pub(super) marks: Option<String>,
    /// Named facts for modern attempts; absent only for a legacy opaque reading.
    pub(super) inputs: Option<Inputs>,
    /// A coherent local materialization reading; unknown never erases a known one.
    pub(super) materialization: Option<MaterializationMark>,
    /// The network it was read on ([`network`]), which the try's record keeps for System State's
    /// reading of what the try heard ([`last_rows`]); never a cue to try again.
    pub(super) network: Option<String>,
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
        let same_routes = match (&self.inputs, &before.inputs) {
            (Some(current), Some(previous)) => current
                .routes
                .as_ref()
                .is_none_or(|routes| previous.routes.as_ref() == Some(routes)),
            _ => compatible,
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
        if self.network.is_none() && same_routes {
            self.network.clone_from(&before.network);
        }
    }
}

/// Where a [`Pace`] is read from: [`Live`], or a test's network and vault.
pub(super) trait Reads {
    /// This aterm build.
    fn build(&self) -> String;
    /// Now, in seconds since the epoch.
    fn now(&self) -> u64;
    /// The network this Mac's traffic leaves on, as text: what a try's pause ends at once on
    /// ([`Inputs::routes`]). Live, the default route ([`aterm_secrets::network::default_route`]),
    /// which an address that comes and goes beside it (AirDrop, a VPN's `utun`, a temporary IPv6
    /// address) leaves as it was.
    fn routes(&self) -> Option<String>;
    /// The key a kept look at the ALab vault is held to ([`aterm_secrets::heard::network_key`]:
    /// the default gateway's hardware address and the IPv4 subnets), `None` where it cannot be
    /// formed.
    fn network_key(&self) -> Option<String> {
        None
    }
    /// Vault handle for the single console account, if any.
    fn vault(&self) -> Option<Arc<dyn SecretsOps>>;
    /// Local vault files, when this reader can observe materialization.
    fn paths(&self) -> Option<aterm_secrets::client::Paths> {
        None
    }
    /// Clone into a `Send` reader for the concurrent Secrets lane, when supported.
    fn boxed_clone(&self) -> Option<Box<dyn Reads + Send>> {
        None
    }
}

/// The pace `reads` reads now.
pub(super) fn pace(reads: &dyn Reads) -> Pace {
    let (marks, inputs) = marks(reads);
    let network = network_or(reads, || inputs.routes.clone());
    Pace {
        build: reads.build(),
        marks,
        inputs: Some(inputs),
        materialization: reads.paths().and_then(|paths| {
            aterm_secrets::client::materialization_mark(&paths)
                .ok()
                .flatten()
        }),
        network,
        now: reads.now(),
    }
}

/// The routes `reads` reads now, where every part of them was read
/// ([`aterm_secrets::network::route_known`]): a part not read (`-`: the gateway's hardware
/// address a wake has not resolved yet, the interface not found) is not known, so it is no move
/// to another network, and the last known routes stand ([`Inputs::retain_known_from`]).
fn known_routes(reads: &dyn Reads) -> Option<String> {
    reads
        .routes()
        .filter(|routes| aterm_secrets::network::route_known(routes))
}

fn marks(reads: &dyn Reads) -> (Option<String>, Inputs) {
    use aterm_secrets::client::Membership;
    let read = reads.routes();
    let place = read
        .as_deref()
        .and_then(aterm_secrets::network::route_place);
    let routes = read.filter(|routes| aterm_secrets::network::route_known(routes));
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
        place: place.as_ref().map(|value| fingerprint("place", value)),
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
#[derive(Clone, Copy)]
pub(super) struct Live;

impl Reads for Live {
    fn build(&self) -> String {
        aterm_stamp::build_id()
    }

    fn now(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs())
    }

    fn routes(&self) -> Option<String> {
        aterm_secrets::network::default_route()
    }

    fn network_key(&self) -> Option<String> {
        aterm_secrets::heard::network_key()
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

    fn boxed_clone(&self) -> Option<Box<dyn Reads + Send>> {
        Some(Box::new(*self))
    }
}

/// Returns the backoff pause after `tries` consecutive unfinished attempts.
pub(super) fn pause(tries: u32) -> Duration {
    Backoff::new(FIRST_PAUSE, LONGEST).after(tries.max(1))
}

/// The pause after `tries` consecutive unfinished attempts, the last `failures` of them failed
/// changes, whose last left the rows of `rows` (each its status and detail): up to
/// [`LONGEST_UNREACHED`] where every row it left to do asks after a vault that is not there now
/// ([`unreached`]) or once [`SLOWER_AFTER`] changes in a row failed, else up to [`LONGEST`].
fn pause_after<'a>(
    tries: u32,
    failures: u32,
    rows: impl IntoIterator<Item = (&'a str, &'a str)> + Clone,
) -> Duration {
    if failures >= SLOWER_AFTER || unreached(rows) {
        Backoff::new(FIRST_PAUSE, LONGEST_UNREACHED).after(tries.max(1))
    } else {
        pause(tries)
    }
}

/// Whether the rows of `rows` (each its status and detail) leave nothing to do but ask again
/// after a vault that is not there now: one is a vault that did not answer
/// ([`super::UNREACHED`]) or answered that none is there ([`super::ABSENT`]), and every other is
/// done or waits ([`super::WAITING`]). The statuses decide, never the words.
fn unreached<'a>(rows: impl IntoIterator<Item = (&'a str, &'a str)> + Clone) -> bool {
    rows.clone()
        .into_iter()
        .any(|(status, _)| matches!(status, super::UNREACHED | super::ABSENT))
        && rows.into_iter().all(|(status, _)| {
            matches!(
                status,
                "ok" | "applied" | super::WAITING | super::UNREACHED | super::ABSENT
            )
        })
}

/// Whether a later try asks again by itself, on the try's pause, about a row with `status`:
/// every row not in place but one that waits on someone's act ([`super::WAITING`]: an approval,
/// a person), whose own change (the vault's state, a grant) wakes the run instead, and which no
/// try of aterm's moves. A vault that answered that none is there ([`super::ABSENT`]) is asked
/// again, as one that did not answer is: nothing on this Mac changes when one comes.
fn asked_again(status: &str) -> bool {
    !matches!(status, "ok" | "applied" | super::WAITING)
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
    /// The network the try ran on ([`Pace::network`]); a try an older build recorded keeps none.
    network: Option<String>,
    tries: u32,
    /// How many of the last tries in a row failed (an error row: a change not made).
    failures: u32,
    at: u64,
    rows: Vec<(String, String, String)>,
}

impl Attempt {
    fn load(root: &Path) -> Option<Self> {
        let bytes = crate::macos_doctor::read_record_bytes(&root.join(RECORD)).ok()?;
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
            network: record["network"].as_str().map(str::to_owned),
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
        // A field older builds do not read, and drop when they write the record again.
        if let Some(network) = &self.network {
            record
                .as_object_mut()
                .unwrap()
                .insert("network".into(), network.as_str().into());
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

    /// [`SLOWER_AFTER`] changes in a row failed: the try is asked again less often.
    const fn failing(&self) -> bool {
        self.failures >= SLOWER_AFTER
    }

    /// The pause this try's rows leave before the next ([`pause_after`]).
    fn pause(&self) -> Duration {
        pause_after(
            self.tries,
            self.failures,
            self.rows
                .iter()
                .map(|(_, status, detail)| (status.as_str(), detail.as_str())),
        )
    }

    /// The rows this try left, as the stage gave them with its pause's note.
    fn rows(self) -> Vec<Row> {
        self.rows
            .into_iter()
            .map(|(name, status, detail)| Row {
                name,
                status,
                detail,
                elapsed_ms: None,
            })
            .collect()
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
            // A field older builds do not write.
            place: record
                .get("place")
                .and_then(Value::as_str)
                .map(str::to_owned),
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
            "routes": self.routes, "place": self.place, "vault": self.vault,
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

/// Returns the cached rows while the last unfinished attempt of `revision` is paused: until its
/// pause ends, which nothing outlasts. The caller holds the profile writer lock. A legacy hold with an exactly
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
    let due = last.at.saturating_add(last.pause().as_secs());
    (last.unchanged(revision, pace) && (last.at..due).contains(&pace.now)).then(|| last.rows())
}

/// The network `reads` reads now, as a try keeps the one it ran on ([`Pace::network`]) and System
/// State tells a look at the ALab vault there from one elsewhere: the key a kept look is held to
/// ([`Reads::network_key`]), which an address that comes and goes on the same network (an IPv6
/// one, AirDrop's link) leaves as it was, or where none can be formed, its routes
/// ([`Inputs::routes`]); fingerprinted, `None` where neither can be read.
pub(super) fn network(reads: &dyn Reads) -> Option<String> {
    network_or(reads, || routes(reads))
}

/// [`network`], with the routes' fingerprint `routes` gives where no key can be formed.
fn network_or(reads: &dyn Reads, routes: impl FnOnce() -> Option<String>) -> Option<String> {
    reads
        .network_key()
        .map(|key| fingerprint("network", &key))
        .or_else(routes)
}

/// The fingerprint of the routes `reads` reads now, as a try keeps them ([`Inputs::routes`]).
fn routes(reads: &dyn Reads) -> Option<String> {
    known_routes(reads).map(|routes| fingerprint("routes", &routes))
}

/// Whether what a look at the ALab vault heard on the network `heard_on` ([`network`]) speaks for
/// the network `reads` reads now: the same network, both read. A look heard only its own network,
/// so one on another, or one whose network or this one cannot be read, says nothing here.
pub(super) fn on_network(heard_on: Option<&str>, reads: &dyn Reads) -> bool {
    heard_on.is_some_and(|heard_on| network(reads).as_deref() == Some(heard_on))
}

/// The rows the last unfinished Secrets try of `revision` left, paused or not, which System
/// State's vault row reads where this process heard nothing of the ALab vault on this network
/// ([`super::scope::heard_at`]): only where that try ran on the network `reads` reads now
/// ([`on_network`]), or for a try an older build recorded, which kept only its routes, where they
/// are the routes read now. A stopped run keeps its record and asks nothing again, so a launch on
/// another network has heard nothing there.
pub(super) fn last_rows(root: &Path, revision: u64, reads: &dyn Reads) -> Option<Vec<Row>> {
    let last = Attempt::load(root).filter(|last| last.revision == revision)?;
    let heard_here = match last.network.as_deref() {
        Some(network) => on_network(Some(network), reads),
        None => last
            .inputs
            .as_ref()?
            .routes
            .as_ref()
            .is_some_and(|ran| routes(reads).as_ref() == Some(ran)),
    };
    heard_here.then(|| last.rows())
}

/// The deadline of a retryable try now due, for a parked worker. A row that
/// waits on someone's act ([`super::WAITING`]: a person, an approval) has no
/// timer, its own change waking the run; a vault nobody answered for
/// ([`super::UNREACHED`]), one not there now ([`super::ABSENT`]) and a change
/// that failed, however often, are asked again ([`asked_again`]). The worker
/// consumes a deadline once; only a later actual try advances it.
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
    if last.revision != revision || !last.rows.iter().any(|(_, status, _)| asked_again(status)) {
        return None;
    }
    Some((last.at, last.at.saturating_add(last.pause().as_secs())))
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
    let network = pace.network.clone().or_else(|| {
        previous
            .as_ref()
            .filter(|last| {
                last.revision == revision
                    && match (&pace.inputs, &last.inputs) {
                        (Some(current), Some(before)) => current
                            .routes
                            .as_ref()
                            .is_none_or(|routes| before.routes.as_ref() == Some(routes)),
                        _ => last.same_inputs(pace),
                    }
            })
            .and_then(|last| last.network.clone())
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
    let note = note(pause_after(
        tries,
        failures,
        rows.iter()
            .map(|row| (row.status.as_str(), row.detail.as_str())),
    ));
    // A try whose rows only wait on someone's act has no timer ([`retry_window`]); one beside
    // them that is asked again brings the waiting rows' next try too.
    let timed = rows.iter().any(|row| asked_again(&row.status));
    for row in rows
        .iter_mut()
        .filter(|row| !matches!(row.status.as_str(), "ok" | "applied"))
    {
        let note = if row.status == super::WAITING && !timed {
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
        network,
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
/// [`record`] last left the pause ([`PAGE_RETRIES`], or [`PAGE_SLOWER`] once two tries failed); one
/// that could not be confirmed says no more; the rest carry no detail, which the page never shows
/// for them. The process, the paths and the vault's own words stay in `rows`, for `aterm doctor`
/// and the log.
pub(super) fn page(root: &Path, revision: u64, rows: &[Row]) -> super::PassPage {
    let failing =
        Attempt::load(root).is_some_and(|last| last.revision == revision && last.failing());
    let mut page = super::PassPage::default();
    for row in rows {
        let detail = match row.status.as_str() {
            "error" if failing => PAGE_SLOWER,
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

/// What the page says once two tries in a row failed: aterm tries again by itself, less often.
pub(super) const PAGE_SLOWER: &str = aterm_types::macos_setup::TRIED_TWICE;

/// What a waiting row's try adds: no timer, only a change or a person.
const WAITS: &str = "aterm tries again by itself when this Mac's network or vault changes, or on Set up now or Join in System State";

fn note(pause: Duration) -> String {
    let minutes = pause.as_secs() / 60;
    let after = match minutes {
        1 => "a minute".to_owned(),
        60 => "an hour".to_owned(),
        _ => format!("{minutes} minutes"),
    };
    format!(
        "aterm tries again by itself {after} after this try, and at once when this Mac's network or vault changes or on Set up now or Join in System State"
    )
}

/// A person pressed Check again ([`super::tries_afresh`]): `revision`'s try starts afresh, so the
/// pass tries now, with no pause and no failure counted. Another run's record is left as it is.
///
/// # Errors
/// The record of `revision` cannot be removed.
pub(super) fn afresh(root: &Path, revision: u64) -> Result<(), String> {
    if Attempt::load(root).is_none_or(|last| last.revision != revision) {
        return Ok(());
    }
    match std::fs::remove_file(root.join(RECORD)) {
        Ok(()) => durable_directory(root).map_err(|error| error.to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("cannot reset the secrets retry pause: {error}")),
    }
}

#[cfg(test)]
mod unknown_inputs_tests;

#[cfg(test)]
#[path = "secrets_retry_tests.rs"]
mod tests;
