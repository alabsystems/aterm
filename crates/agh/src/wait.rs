// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Waits (design §12): conditions, backoff, heartbeats and the poller. Every clock read and
//! sleep is the client's `Fx`, so a test's wait takes no real time and a replay's none at all.

use std::time::Duration;

use aterm_json::{Value, json};

use crate::client::{Call, Github};
use crate::error::{Error, Kind, Result, Sub};
use crate::fx::Fx;
use crate::github::{How, Probe, TagObjects};
use crate::model::{ReleaseObject, Sha256Digest, from_json};
use crate::policy::{Access, RouteTarget};
use crate::slug::Slug;

/// One observation.
pub enum Verdict {
    /// The condition holds; what was observed.
    Holds(aterm_json::Value),
    /// Not yet; what was observed.
    NotYet(aterm_json::Value),
    /// It can never hold.
    Impossible(Error),
}

/// A condition that is polled.
pub trait Condition: Send {
    /// The condition as typed (`asset=SHA256SUMS`).
    fn describe(&self) -> String;
    /// One observation.
    ///
    /// # Errors
    ///
    /// A failed observation (a retriable one counts toward blindness).
    fn check(&mut self, gh: &Github) -> Result<Verdict>;
}

/// The poll interval: start, growth (per mille), cap and floor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Backoff {
    /// The first interval, and the interval after a change.
    pub start: Duration,
    /// The growth per round, per mille.
    pub factor_permille: u32,
    /// The longest interval.
    pub cap: Duration,
    /// The shortest interval.
    pub floor: Duration,
}

impl Backoff {
    /// 2 s, ×1.5, at most 30 s, at least 1 s.
    pub const STANDARD: Backoff = Backoff {
        start: Duration::from_secs(2),
        factor_permille: 1500,
        cap: Duration::from_secs(30),
        floor: Duration::from_secs(1),
    };

    /// `start` after a change; else `current × factor`, clamped to `[floor, cap]`.
    #[inline]
    #[must_use]
    pub fn next(&self, current: Duration, changed: bool) -> Duration {
        if changed {
            return self.start;
        }
        let grown = current
            .as_millis()
            .saturating_mul(u128::from(self.factor_permille))
            / 1000;
        let grown = Duration::from_millis(u64::try_from(grown).unwrap_or(u64::MAX));
        grown.clamp(self.floor, self.cap.max(self.floor))
    }
}

/// What the poller reports while it runs.
pub enum WaitEvent<'a> {
    /// Still waiting.
    Heartbeat {
        /// Time waited so far.
        elapsed: Duration,
        /// The not-yet conditions, as `{slug}:{observed}` joined by `; `, passed through the
        /// client's redactor whole and then cut to 200 characters, so no cut can split a
        /// secret the redactor knows only whole.
        last: &'a str,
    },
    /// An observation changed.
    Changed {
        /// What changed.
        what: String,
    },
}

/// How a wait ended. `Held` and `TimedOut` carry the state object
/// `{"conditions":[{"cond","holds","observed"}],"elapsed_s"}`.
pub enum WaitEnd {
    /// Every condition holds.
    Held(aterm_json::Value),
    /// The timeout passed.
    TimedOut {
        /// The last state.
        last: aterm_json::Value,
    },
    /// No observation succeeded for the blind limit.
    Blind {
        /// The last failure.
        last_error: Error,
    },
    /// A condition can never hold.
    Impossible(Error),
}

/// The poller. Time and sleeps are its client's `Fx` (`gh.fx()`), so a test's wait takes no
/// real time and a replay's takes none at all.
pub struct Poller<'a> {
    /// The effect seam.
    pub fx: &'a Fx,
    /// The poll interval.
    pub backoff: Backoff,
    /// The longest wait.
    pub timeout: Duration,
    /// How often a heartbeat is reported.
    pub heartbeat: Duration,
    /// How long without an observation counts as blind.
    pub blind_limit: Duration,
}

/// The longest `last` a heartbeat line prints, in characters, cut after redaction.
const LAST_MAX: usize = 200;

/// One condition's latest observation: whether it held, and what was observed.
type Seen = Option<(bool, Value)>;

/// The state object: each condition with its latest observation, and the seconds waited.
fn state(conditions: &[Box<dyn Condition>], seen: &[Seen], elapsed: Duration) -> Value {
    let listed: Vec<Value> = conditions
        .iter()
        .zip(seen)
        .map(|(condition, seen)| {
            let (holds, observed) = seen.clone().unwrap_or((false, Value::Null));
            json!({"cond": condition.describe(), "holds": holds, "observed": observed})
        })
        .collect();
    json!({"conditions": listed, "elapsed_s": elapsed.as_secs()})
}

/// The unmet conditions as `{slug}:{observed}`, joined by `; `, uncut (the poller redacts,
/// then cuts).
fn unmet(conditions: &[Box<dyn Condition>], seen: &[Seen]) -> String {
    let parts: Vec<String> = conditions
        .iter()
        .zip(seen)
        .filter(|(_, seen)| !matches!(seen, Some((true, _))))
        .map(|(condition, seen)| {
            let observed = seen
                .as_ref()
                .map_or_else(|| Value::Null.to_string(), |(_, v)| v.to_string());
            format!("{}:{observed}", condition.describe())
        })
        .collect();
    parts.join("; ")
}

impl Poller<'_> {
    /// A heartbeat: the unmet conditions redacted whole by the client's redactor, then cut
    /// to [`LAST_MAX`] characters.
    fn beat(
        gh: &Github,
        conditions: &[Box<dyn Condition>],
        seen: &[Seen],
        elapsed: Duration,
        on: &mut dyn FnMut(WaitEvent<'_>),
    ) {
        let last: String = gh
            .redactor()
            .apply(&unmet(conditions, seen))
            .chars()
            .take(LAST_MAX)
            .collect();
        on(WaitEvent::Heartbeat {
            elapsed,
            last: &last,
        });
    }

    /// Poll `conditions` until they all hold, one is impossible, the poller is blind, or the
    /// timeout passes (plan §11.4).
    ///
    /// A retriable error, a rate limit included, is no observation: rounds with none for
    /// `blind_limit` end the wait `Blind`, carrying the last failure (its reset and
    /// `Retry-After` with it), and the pacing is the backoff's alone.
    pub fn run(
        &self,
        gh: &Github,
        conditions: &mut [Box<dyn Condition>],
        on: &mut dyn FnMut(WaitEvent<'_>),
    ) -> WaitEnd {
        let start = match self.fx.mono() {
            Ok(start) => start,
            Err(e) => return WaitEnd::Impossible(e),
        };
        let mut interval = self.backoff.start;
        let mut last_success = start;
        let mut last_heartbeat = start;
        let mut last_error: Option<Error> = None;
        // `seen` is each condition's latest observation, which the state shows (plan §11.4 step
        // 3). A retriable failure is no observation (plan §4.6.22): it leaves `seen` as it was, so
        // a timeout prints the last observed state (design §12), and a failure and a recovery
        // to the same value are no change. It still withholds `Held` for its round.
        let mut seen: Vec<Seen> = vec![None; conditions.len()];
        let mut first_round = true;
        loop {
            let mut observed_any = false;
            let mut failed = false;
            let mut changed = false;
            let mut changes = Vec::new();
            for (condition, seen) in conditions.iter_mut().zip(seen.iter_mut()) {
                let (holds, value) = match condition.check(gh) {
                    Ok(Verdict::Holds(value)) => (true, value),
                    Ok(Verdict::NotYet(value)) => (false, value),
                    Ok(Verdict::Impossible(e)) => return WaitEnd::Impossible(e),
                    Err(e) if !e.retriable() => return WaitEnd::Impossible(e),
                    Err(e) => {
                        failed = true;
                        last_error = Some(e);
                        continue;
                    }
                };
                observed_any = true;
                // Any observation that differs from the condition's previous one is a change
                // (plan §11.4 step 5); a first round's observations all are, so its sleep is
                // `backoff.start`, and a first round that observed nothing grows the interval.
                if seen.as_ref().map(|(_, before)| before) != Some(&value) {
                    changed = true;
                    if !first_round {
                        changes.push(condition.describe());
                    }
                }
                *seen = Some((holds, value));
            }
            let now = match self.fx.mono() {
                Ok(now) => now,
                Err(e) => return WaitEnd::Impossible(e),
            };
            let elapsed = now.saturating_sub(start);
            let all_hold = seen.iter().all(|seen| matches!(seen, Some((true, _))));
            if all_hold && !failed {
                return WaitEnd::Held(state(conditions, &seen, elapsed));
            }
            if observed_any {
                last_success = now;
            } else if now.saturating_sub(last_success) >= self.blind_limit {
                let last_error = last_error.unwrap_or_else(|| {
                    Error::could_not_run("no condition was observed").with_sub(Sub::Local)
                });
                return WaitEnd::Blind { last_error };
            }
            for what in changes {
                on(WaitEvent::Changed { what });
            }
            interval = self.backoff.next(interval, changed);
            if now.saturating_sub(last_heartbeat) >= self.heartbeat {
                last_heartbeat = now;
                Self::beat(gh, conditions, &seen, elapsed, on);
            }
            if elapsed >= self.timeout {
                return WaitEnd::TimedOut {
                    last: state(conditions, &seen, elapsed),
                };
            }
            let pause = interval.min(self.timeout.saturating_sub(elapsed));
            if let Err(e) = self.fx.sleep(pause) {
                return WaitEnd::Impossible(e);
            }
            first_round = false;
        }
    }
}

/// A release condition (design §12).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReleaseCond {
    /// `exists`.
    Exists,
    /// `published`.
    Published,
    /// `prerelease`.
    Prerelease,
    /// `latest`.
    Latest,
    /// `not-latest`.
    NotLatest,
    /// `assets=N`.
    Assets(usize),
    /// `asset=NAME[,NAME…]`.
    Asset(Vec<String>),
    /// `digest=NAME:sha256:HEX`.
    Digest {
        /// The asset's name.
        name: String,
        /// Its digest.
        digest: Sha256Digest,
    },
    /// `anon-visible=NAME`.
    AnonVisible(String),
    /// `gone`.
    Gone,
}

/// The conditions `parse` reads, for its usage error's next.
const CONDITIONS: &str = "conditions: exists, published, prerelease, latest, not-latest, \
                          assets=N, asset=NAME[,NAME…], digest=NAME:sha256:HEX, \
                          anon-visible=NAME, gone";

/// Whether `name` can name an asset: not empty, no dot-segment (`.` or `..`), which agh
/// never sends in a download path, and no control byte (0x00-0x1F, 0x7F). No release asset
/// is called any of these.
#[inline]
fn asset_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.bytes().any(|b| b < 0x20 || b == 0x7f)
}

/// `path::refuse_bad_tag` for a wait on `tag` of `slug`, given before any request: no round
/// could ever observe such a tag.
///
/// # Errors
///
/// That usage, with next `agh help release wait`.
pub(crate) fn refuse_tag(tag: &str, slug: &Slug) -> Result<()> {
    crate::path::refuse_bad_tag(tag).map_err(|e| {
        e.with_next("agh help release wait")
            .with_repo(slug.to_string())
    })
}

impl ReleaseCond {
    /// `exists`, `published`, `prerelease`, `latest`, `not-latest`, `assets=N`,
    /// `asset=NAME[,NAME…]`, `digest=NAME:sha256:HEX`, `anon-visible=NAME`, `gone`.
    ///
    /// # Errors
    ///
    /// Usage: `not a release condition: {text}` (with the list of conditions as next).
    pub fn parse(text: &str) -> Result<ReleaseCond> {
        let refuse =
            || Error::usage(format!("not a release condition: {text}")).with_next(CONDITIONS);
        let cond = match text {
            "exists" => ReleaseCond::Exists,
            "published" => ReleaseCond::Published,
            "prerelease" => ReleaseCond::Prerelease,
            "latest" => ReleaseCond::Latest,
            "not-latest" => ReleaseCond::NotLatest,
            "gone" => ReleaseCond::Gone,
            _ => {
                let (key, value) = text.split_once('=').ok_or_else(refuse)?;
                if value.is_empty() {
                    return Err(refuse());
                }
                match key {
                    "assets" => ReleaseCond::Assets(crate::path::digits(value).ok_or_else(refuse)?),
                    "asset" => {
                        let names: Vec<String> = value.split(',').map(str::to_string).collect();
                        if names.iter().any(|name| !asset_name(name)) {
                            return Err(refuse());
                        }
                        ReleaseCond::Asset(names)
                    }
                    "digest" => {
                        let (name, digest) = value.split_once(':').ok_or_else(refuse)?;
                        let digest = Sha256Digest::parse(digest).ok_or_else(refuse)?;
                        if !asset_name(name) {
                            return Err(refuse());
                        }
                        ReleaseCond::Digest {
                            name: name.to_string(),
                            digest,
                        }
                    }
                    "anon-visible" if asset_name(value) => {
                        ReleaseCond::AnonVisible(value.to_string())
                    }
                    _ => return Err(refuse()),
                }
            }
        };
        Ok(cond)
    }

    /// The condition as `parse` reads it.
    #[must_use]
    pub fn slug(&self) -> String {
        match self {
            ReleaseCond::Exists => "exists".into(),
            ReleaseCond::Published => "published".into(),
            ReleaseCond::Prerelease => "prerelease".into(),
            ReleaseCond::Latest => "latest".into(),
            ReleaseCond::NotLatest => "not-latest".into(),
            ReleaseCond::Assets(n) => format!("assets={n}"),
            ReleaseCond::Asset(names) => format!("asset={}", names.join(",")),
            ReleaseCond::Digest { name, digest } => format!("digest={name}:{digest}"),
            ReleaseCond::AnonVisible(name) => format!("anon-visible={name}"),
            ReleaseCond::Gone => "gone".into(),
        }
    }
}

/// How one condition reads its release, with its own validator and last answer.
struct Observer {
    slug: Slug,
    tag: String,
    how: How,
    etag: Option<String>,
    /// The last answer: `Some(None)` is a confirmed "no release".
    last: Option<Option<ReleaseObject>>,
}

impl Observer {
    /// The release with the tag, or `None` when there is none.
    ///
    /// Under Read: `GET /repos/{o}/{r}/releases/tags/{tag}` with `If-None-Match` on the last
    /// ETag; a 304 reuses the last answer, and a probe-confirmed NotFound is "no release".
    /// Under WriterRead: `objects_for_tag`, where two releases with the tag are Conflict
    /// `AmbiguousTag`, which no later round can change. For `gone` (`for_gone`), which asks
    /// only whether a published release has the tag (design §12), several are no conflict:
    /// the answer is a published one when there is one, else a draft.
    fn release(&mut self, gh: &Github, for_gone: bool) -> Result<Option<ReleaseObject>> {
        refuse_tag(&self.tag, &self.slug)?;
        if self.how.access != Access::Read {
            return match gh
                .releases(&self.slug)
                .how(self.how.clone())
                .objects_for_tag(&self.tag)?
            {
                TagObjects::None => Ok(None),
                TagObjects::One(release) => Ok(Some(*release)),
                TagObjects::Many(releases) if for_gone => {
                    let published = releases.iter().position(|r| !r.draft).unwrap_or(0);
                    Ok(releases.into_iter().nth(published))
                }
                TagObjects::Many(releases) => {
                    let ids: Vec<u64> = releases.iter().map(|r| r.id).collect();
                    Err(Error::new(
                        Kind::Conflict,
                        format!(
                            "{} releases of {} have the tag {}",
                            ids.len(),
                            self.slug,
                            self.tag
                        ),
                    )
                    .with_sub(Sub::AmbiguousTag { ids })
                    .with_repo(self.slug.to_string()))
                }
            };
        }
        let path = crate::github::releases::tag_path(&self.slug, &self.tag);
        let call = Call::get(path, self.how.route(RouteTarget::Repo(self.slug.clone())))
            .if_none_match(self.etag.clone())
            .probe_404(!self.how.no_probe)
            .retry(self.how.retry);
        match gh.call(&call) {
            Ok(answered) if answered.status == 304 => match &self.last {
                Some(last) => Ok(last.clone()),
                None => Err(Error::protocol(format!(
                    "GitHub answered 304 for {} {} with no earlier answer",
                    self.slug, self.tag
                ))
                .with_repo(self.slug.to_string())),
            },
            Ok(answered) => {
                let release: ReleaseObject = from_json(&answered.json()?)?;
                self.etag.clone_from(&answered.etag);
                self.last = Some(Some(release.clone()));
                Ok(Some(release))
            }
            Err(e) if e.kind == Kind::NotFound => {
                self.etag = None;
                self.last = Some(None);
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }
}

/// `{"assets","draft","id","prerelease","tag_name"}`, or null.
fn summary(release: Option<&ReleaseObject>) -> Value {
    release.map_or(Value::Null, |r| {
        json!({
            "assets": r.assets.len(),
            "draft": r.draft,
            "id": r.id,
            "prerelease": r.prerelease,
            "tag_name": r.tag_name.as_str(),
        })
    })
}

/// A verdict: `observed`, held or not.
#[inline]
fn verdict(holds: bool, observed: Value) -> Verdict {
    if holds {
        Verdict::Holds(observed)
    } else {
        Verdict::NotYet(observed)
    }
}

/// One polled release condition.
struct ReleaseCondition {
    cond: ReleaseCond,
    observer: Observer,
}

impl Condition for ReleaseCondition {
    fn describe(&self) -> String {
        self.cond.slug()
    }

    fn check(&mut self, gh: &Github) -> Result<Verdict> {
        // Before any condition's request, `anon-visible`'s probe included: a tag that names
        // no release path is usage, which ends the wait at once.
        refuse_tag(&self.observer.tag, &self.observer.slug)?;
        if let ReleaseCond::AnonVisible(name) = &self.cond {
            let observer = &self.observer;
            return match gh.public().probe(&observer.slug, &observer.tag, name) {
                Probe::Present => Ok(Verdict::Holds(Value::from("present"))),
                Probe::Absent => Ok(Verdict::NotYet(Value::from("absent"))),
                Probe::Unknown(why) => Err(Error::could_not_run(format!("unknown: {why}"))
                    .with_repo(observer.slug.to_string())),
            };
        }
        let release = self
            .observer
            .release(gh, matches!(self.cond, ReleaseCond::Gone))?;
        let r = release.as_ref();
        let verdict = match &self.cond {
            ReleaseCond::Exists => verdict(r.is_some(), summary(r)),
            ReleaseCond::Published => verdict(r.is_some_and(|r| !r.draft), summary(r)),
            ReleaseCond::Prerelease => verdict(r.is_some_and(|r| r.prerelease), summary(r)),
            ReleaseCond::Latest | ReleaseCond::NotLatest => {
                let latest = match r {
                    Some(_) => gh
                        .releases(&self.observer.slug)
                        .how(self.observer.how.clone())
                        .latest_id()?,
                    None => None,
                };
                let id = r.map(|r| r.id);
                let is_latest = id.is_some() && id == latest;
                let holds = match self.cond {
                    ReleaseCond::Latest => is_latest,
                    _ => id.is_some() && !is_latest,
                };
                verdict(holds, json!({"id": id, "latest_id": latest}))
            }
            ReleaseCond::Assets(n) => {
                let count = r.map(|r| r.assets.len());
                verdict(count == Some(*n), json!(count))
            }
            ReleaseCond::Asset(names) => {
                let found: Vec<Value> = names
                    .iter()
                    .filter_map(|name| {
                        let asset = r?.assets.iter().find(|a| &a.name == name)?;
                        Some(json!({"name": asset.name.as_str(), "state": asset.state.as_str()}))
                    })
                    .collect();
                let holds = names.iter().all(|name| {
                    r.is_some_and(|r| {
                        r.assets
                            .iter()
                            .any(|a| &a.name == name && a.state == "uploaded")
                    })
                });
                verdict(holds, Value::Array(found))
            }
            ReleaseCond::Digest { name, digest } => {
                let named = r.and_then(|r| r.assets.iter().find(|a| &a.name == name));
                let got = named.and_then(|a| a.digest.clone());
                let holds = got
                    .as_deref()
                    .and_then(Sha256Digest::parse)
                    .is_some_and(|d| d == *digest);
                verdict(holds, got.map_or(Value::Null, Value::from))
            }
            // No published release has the tag: under --include-drafts a draft is no release
            // for `gone` (design §12), and a Read never sees one.
            ReleaseCond::Gone => verdict(r.is_none_or(|r| r.draft), summary(r)),
            ReleaseCond::AnonVisible(_) => verdict(false, Value::Null),
        };
        Ok(verdict)
    }
}

/// A polled release condition.
#[must_use]
pub fn release_condition(
    slug: &Slug,
    tag: &str,
    cond: ReleaseCond,
    how: How,
) -> Box<dyn Condition> {
    Box::new(ReleaseCondition {
        cond,
        observer: Observer {
            slug: slug.clone(),
            tag: tag.to_string(),
            how,
            etag: None,
            last: None,
        },
    })
}
