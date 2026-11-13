// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Buffered named-key recording against an existing isolated lab, without prompt submission.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use aterm_json::json;

use crate::instance::Reply;
use crate::trail_gallery::{ReplyShape, image_bytes, relay_reply};
use crate::{Fail, Res};

const MAX_KEYS: usize = 4096;
const MAX_RETAINED_BYTES: usize = 128 * 1024 * 1024;
const RING: usize = 32;
const REQUEST_BOUND: Duration = Duration::from_secs(60);
// A hang detector for an explicitly requested observation, never a latency budget.
const LANDING_HANG: Duration = Duration::from_secs(60);
const LANDING_POLL: Duration = Duration::from_millis(50);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Landing {
    row: u16,
    col: u16,
}

impl Landing {
    fn parse(value: &str) -> Result<Self, String> {
        let (row, col) = value
            .split_once(',')
            .ok_or("landing must be zero-based ROW,COL")?;
        let coordinate = |word: &str| {
            if word.is_empty() || !word.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err("landing coordinates must be integers in 0..=65535");
            }
            word.parse::<u16>()
                .map_err(|_| "landing coordinates must be integers in 0..=65535")
        };
        Ok(Self {
            row: coordinate(row)?,
            col: coordinate(col)?,
        })
    }
}

struct Options {
    tag: String,
    out: PathBuf,
    text: String,
    cadence: Duration,
    no_images: bool,
    expect_trail_landing: Option<Landing>,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut args = args.to_vec();
        let tag = crate::run::take_only_tag(&mut args)?;
        let (mut out, mut text, mut cadence) = (None, None, None);
        let mut no_images = false;
        let mut expect_trail_landing = None;
        let mut args = args.iter();
        while let Some(option) = args.next() {
            if option == "--no-images" {
                if no_images {
                    return Err("repeated argument --no-images".into());
                }
                no_images = true;
                continue;
            }
            let value = args
                .next()
                .ok_or_else(|| format!("{option} needs a value"))?;
            match option.as_str() {
                "--out" if out.is_none() && !value.is_empty() => out = Some(PathBuf::from(value)),
                "--text" if text.is_none() => text = Some(value.clone()),
                "--expect-trail-landing" if expect_trail_landing.is_none() => {
                    expect_trail_landing = Some(Landing::parse(value)?);
                }
                "--cadence-ms" if cadence.is_none() => {
                    let ms = value
                        .parse::<u64>()
                        .ok()
                        .filter(|ms| (1..=60_000).contains(ms));
                    cadence = Some(Duration::from_millis(
                        ms.ok_or("cadence must be 1..=60000 ms")?,
                    ));
                }
                _ => return Err(format!("unknown or repeated argument {option}")),
            }
        }
        let text = text.ok_or("--text is required")?;
        validate_text(&text)?;
        Ok(Self {
            tag,
            out: out.ok_or("--out is required")?,
            text,
            cadence: cadence.ok_or("--cadence-ms is required")?,
            no_images,
            expect_trail_landing,
        })
    }
}

fn validate_text(text: &str) -> Result<(), String> {
    if text.is_empty() || text.len() > MAX_KEYS || !text.bytes().all(|b| (b' '..=b'~').contains(&b))
    {
        return Err("text must contain 1..=4096 printable ASCII characters; controls and newlines are refused".into());
    }
    Ok(())
}

fn key_name(byte: u8) -> String {
    match byte {
        b' ' => "space".into(),
        b'A'..=b'Z' => format!("shift+{}", char::from(byte.to_ascii_lowercase())),
        _ => char::from(byte).to_string(),
    }
}

#[derive(Clone, Debug)]
enum Request {
    Drain,
    Key(u64, String),
    Metrics,
    Percentiles,
    Image(bool),
    Status,
    Cursor,
    Text,
    Dims,
}

impl Request {
    fn words(&self) -> Vec<String> {
        match self {
            Self::Drain => vec!["trail".into(), RING.to_string()],
            Self::Key(session, key) => vec![format!("@{session}"), "key".into(), key.clone()],
            Self::Metrics => vec!["metrics".into()],
            Self::Percentiles => vec!["metrics".into(), "percentiles".into()],
            Self::Image(clean) => {
                let mut words = vec!["image".into(), "--meta".into(), "--bytes".into()];
                if *clean {
                    words.push("clean".into());
                }
                words
            }
            Self::Status => vec!["trail".into(), "status".into()],
            Self::Cursor => vec!["cursor".into()],
            Self::Text => vec!["text".into()],
            Self::Dims => vec!["dims".into()],
        }
    }

    fn shape(&self) -> ReplyShape {
        match self {
            Self::Key(..) | Self::Metrics | Self::Percentiles | Self::Cursor | Self::Dims => {
                ReplyShape::Line
            }
            _ => ReplyShape::Counted,
        }
    }
}

trait Clock {
    fn now(&self) -> Duration;
    fn sleep_until(&mut self, due: Duration) -> Result<(), String>;
}

trait Wire {
    fn request(&mut self, request: &Request) -> Result<Reply, String>;
    fn bound_by(&mut self, remaining: Duration);
}

struct Live {
    wire: aterm_agent::RelayClient<aterm_uds::CtlStream>,
    epoch: Instant,
}

impl Wire for Live {
    fn bound_by(&mut self, remaining: Duration) {
        self.wire.bound_by(remaining);
    }

    fn request(&mut self, request: &Request) -> Result<Reply, String> {
        let words = request.words();
        let args: Vec<_> = words.iter().map(String::as_str).collect();
        relay_reply(&mut self.wire, &args, request.shape()).map_err(|e| e.to_string())
    }
}

struct LiveClock(Instant);

impl Clock for LiveClock {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }
    fn sleep_until(&mut self, due: Duration) -> Result<(), String> {
        crate::stop::check().map_err(|e| format!("interrupted by signal {}", e.0))?;
        if let Some(wait) = due.checked_sub(self.now()) {
            crate::stop::sleep(wait).map_err(|e| format!("interrupted by signal {}", e.0))?;
        }
        Ok(())
    }
}

fn field<'a>(line: &'a str, name: &str) -> Result<&'a str, String> {
    let prefix = format!("{name}=");
    let mut values = line
        .split_whitespace()
        .filter_map(|word| word.strip_prefix(&prefix));
    let value = values.next().ok_or_else(|| format!("missing {name}"))?;
    if values.next().is_some() {
        return Err(format!("repeated {name}"));
    }
    Ok(value)
}

fn number(line: &str, name: &str) -> Result<u64, String> {
    field(line, name)?
        .parse()
        .map_err(|_| format!("invalid {name}"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Target {
    window: u64,
    view: u64,
    session: u64,
}

impl Target {
    fn parse(line: &str) -> Result<Self, String> {
        if !line.starts_with("trail-target ") || field(line, "selection")? != "front" {
            return Err("missing global-front trail target".into());
        }
        Ok(Self {
            window: number(line, "window")?,
            view: number(line, "view")?,
            session: number(line, "session")?,
        })
    }
}

#[derive(Clone)]
struct Admission {
    seq: u64,
    immutable: String,
    view: Option<u64>,
    session: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FailureKind {
    Deadline,
    Source,
    Refused,
    Malformed,
    Transport,
    Retention,
    Clock,
    Interrupted,
    Superseded,
}

impl FailureKind {
    const fn name(self) -> &'static str {
        match self {
            Self::Deadline => "deadline",
            Self::Source => "source",
            Self::Refused => "refused",
            Self::Malformed => "malformed",
            Self::Transport => "transport",
            Self::Retention => "retention",
            Self::Clock => "clock",
            Self::Interrupted => "interrupted",
            Self::Superseded => "superseded",
        }
    }
}

#[derive(Debug)]
struct Failure {
    kind: FailureKind,
    detail: String,
}

impl Failure {
    fn new(kind: FailureKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LandingVerdict {
    Licensed,
    Declined,
}

impl LandingVerdict {
    const fn name(self) -> &'static str {
        match self {
            Self::Licensed => "licensed",
            Self::Declined => "declined",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LandingEvidence {
    seq: u64,
    verdict: LandingVerdict,
}

struct LandingObservation {
    expected: Landing,
    after_seq: u64,
    started: Duration,
    ended: Duration,
    evidence: Option<LandingEvidence>,
    failure: Option<Failure>,
}

fn closed_cursor(reply: &str) -> Result<Option<Landing>, Failure> {
    let words: Vec<_> = reply.split_whitespace().take(7).collect();
    let malformed = || Failure::new(FailureKind::Malformed, "invalid landing cursor reply");
    if reply.lines().count() != 1
        || !(words.len() == 5 || words.len() == 6)
        || words[0] != "OK"
        || !matches!(words[3], "0" | "1")
        || words[4].contains('=')
        || (words.len() == 6 && words[5] != "frame=open")
    {
        return Err(malformed());
    }
    let landing = Landing::parse(&format!("{},{}", words[1], words[2])).map_err(|_| malformed())?;
    Ok((words.len() == 5).then_some(landing))
}

fn batch(body: &str) -> Result<(Target, Vec<Admission>), String> {
    let mut lines = body.lines();
    let target = Target::parse(lines.next().ok_or("empty trail reply")?)?;
    let mut rows = Vec::new();
    for line in lines {
        if !line.starts_with("admission ") || rows.len() == RING {
            return Err("invalid or oversized trail ring reply".into());
        }
        let seq = number(line, "seq")?;
        if seq == 0 || rows.last().is_some_and(|last: &Admission| last.seq >= seq) {
            return Err("admission sequence is not strictly increasing".into());
        }
        if !matches!(field(line, "phase")?, "licensed" | "declined") {
            return Err("unknown admission phase".into());
        }
        let (view, session) = match field(line, "source")? {
            "bound" => (
                Some(number(line, "source_view")?),
                Some(number(line, "source_session")?),
            ),
            "unknown" => (None, None),
            _ => return Err("unknown admission source".into()),
        };
        rows.push(Admission {
            seq,
            immutable: line
                .split_whitespace()
                .filter(|word| !word.starts_with("age_ms="))
                .collect::<Vec<_>>()
                .join(" "),
            view,
            session,
        });
    }
    Ok((target, rows))
}

/// The gallery's critical suffix, using the recorder's strict protocol decoder.
/// Missing or coalesced records are unobserved, rather than a claim about pixels.
pub(super) fn licensed_tail(before: &str, after: &str, keys: usize) -> Result<bool, String> {
    if keys == 0 {
        return Err("a critical tail must contain keys".into());
    }
    let (baseline_target, baseline) = batch(before)?;
    let (target, rows) = batch(after)?;
    if target != baseline_target {
        return Err("critical-tail window/view/session changed".into());
    }
    let since = baseline.last().map_or(0, |row| row.seq);
    let fresh: Vec<_> = rows.iter().filter(|row| row.seq > since).collect();
    if fresh.len() != keys {
        return Ok(false);
    }
    for (i, row) in fresh.iter().enumerate() {
        if since.checked_add(i as u64 + 1) != Some(row.seq) {
            return Err("critical-tail admission sequences have a gap".into());
        }
        if row.view != Some(target.view) || row.session != Some(target.session) {
            return Err("critical-tail admission has an unbound or changed source".into());
        }
        if field(&row.immutable, "phase")? != "licensed" {
            return Err(format!("critical-tail admission {} was declined", row.seq));
        }
    }
    Ok(true)
}

#[derive(Default)]
struct Admissions {
    baseline: u64,
    latest: u64,
    rows: BTreeMap<u64, Admission>,
    gaps: Vec<String>,
    bytes: usize,
}

impl Admissions {
    fn observe(
        &mut self,
        rows: Vec<Admission>,
        target: Target,
        baseline: bool,
    ) -> Result<(), Failure> {
        let last = rows.last().map_or(0, |row| row.seq);
        if !baseline && last < self.latest {
            return Err(Failure::new(
                FailureKind::Source,
                "admission sequence reset; coverage is unobserved",
            ));
        }
        let previous = self.latest;
        if !baseline && last.saturating_sub(previous) > RING as u64 {
            self.gaps.push(format!(
                "ring rollover between sequence {previous} and {last}"
            ));
        }
        for row in rows {
            if let Some(held) = self.rows.get(&row.seq) {
                if held.immutable != row.immutable {
                    return Err(Failure::new(
                        FailureKind::Malformed,
                        "an admission sequence changed immutable facts",
                    ));
                }
                continue;
            }
            if !baseline && row.seq > self.latest {
                if row.view != Some(target.view) || row.session != Some(target.session) {
                    return Err(Failure::new(
                        FailureKind::Source,
                        format!("admission {} has an unbound or changed source", row.seq),
                    ));
                }
                if self.latest.checked_add(1) != Some(row.seq) {
                    self.gaps.push(format!("unobserved admission sequences after {} before {}; eviction or preservation, not a key-drop verdict", self.latest, row.seq));
                }
                self.latest = row.seq;
            } else if !baseline {
                return Err(Failure::new(
                    FailureKind::Malformed,
                    "an older uncollected admission appeared",
                ));
            }
            self.bytes += row.immutable.len();
            self.rows.insert(row.seq, row);
        }
        if baseline {
            self.latest = last;
            self.baseline = last;
        }
        Ok(())
    }
}

struct Exchange {
    phase: &'static str,
    request: String,
    start: Duration,
    end: Duration,
    reply: Result<Reply, String>,
}

struct KeyEvent {
    key: String,
    planned: Duration,
    start: Duration,
    ack: Option<Duration>,
    drain_start: Option<Duration>,
    drain_end: Option<Duration>,
}

#[derive(Default)]
struct Recording {
    target: Option<Target>,
    events: Vec<KeyEvent>,
    exchanges: Vec<Exchange>,
    admissions: Admissions,
    images: Vec<(bool, Vec<u8>, String)>,
    bytes: usize,
    finished: Duration,
    replay_last_seq: u64,
    pre_final_key_seq: u64,
    replay_complete: bool,
    landing: Option<LandingObservation>,
    error: Option<String>,
}

impl Recording {
    fn request(
        &mut self,
        clock: &impl Clock,
        wire: &mut impl Wire,
        request: Request,
        phase: &'static str,
    ) -> Result<Reply, String> {
        self.exchange(clock, wire, request, phase)
            .map_err(|failure| failure.detail)
    }

    fn exchange(
        &mut self,
        clock: &impl Clock,
        wire: &mut impl Wire,
        request: Request,
        phase: &'static str,
    ) -> Result<Reply, Failure> {
        let start = clock.now();
        let reply = wire.request(&request);
        let end = clock.now();
        let words = request.words().join(" ");
        let bytes = words.len()
            + match &reply {
                Ok(r) => r.stdout.len() + r.stderr.len(),
                Err(e) => e.len(),
            };
        if self
            .bytes
            .saturating_add(bytes)
            .saturating_add(self.admissions.bytes)
            > MAX_RETAINED_BYTES
        {
            return Err(Failure::new(
                FailureKind::Retention,
                "recorder retention bound exceeded; response was not retained",
            ));
        }
        self.bytes += bytes;
        self.exchanges.push(Exchange {
            phase,
            request: words,
            start,
            end,
            reply: reply.clone(),
        });
        if end < start {
            return Err(Failure::new(
                FailureKind::Clock,
                "controller monotonic clock regressed",
            ));
        }
        let reply = reply.map_err(|detail| Failure::new(FailureKind::Transport, detail))?;
        if reply.code != 0 {
            return Err(Failure::new(
                FailureKind::Refused,
                format!("control request refused: {}{}", reply.stdout, reply.stderr),
            ));
        }
        Ok(reply)
    }

    fn drain(
        &mut self,
        clock: &impl Clock,
        wire: &mut impl Wire,
        phase: &'static str,
        baseline: bool,
    ) -> Result<(), String> {
        let reply = self.request(clock, wire, Request::Drain, phase)?;
        self.receive_admissions(&reply.stdout, baseline)
            .map_err(|failure| failure.detail)
    }

    fn receive_admissions(&mut self, body: &str, baseline: bool) -> Result<(), Failure> {
        let (target, rows) =
            batch(body).map_err(|detail| Failure::new(FailureKind::Malformed, detail))?;
        if self.target.is_some_and(|held| held != target) {
            return Err(Failure::new(
                FailureKind::Source,
                "global-front window/view/session changed",
            ));
        }
        self.target = Some(target);
        self.admissions.observe(rows, target, baseline)?;
        if self.bytes.saturating_add(self.admissions.bytes) > MAX_RETAINED_BYTES {
            return Err(Failure::new(
                FailureKind::Retention,
                "recorder retention bound exceeded",
            ));
        }
        Ok(())
    }

    fn landing_request(
        &mut self,
        clock: &impl Clock,
        wire: &mut impl Wire,
        request: Request,
        phase: &'static str,
        deadline: Duration,
    ) -> Result<Reply, Failure> {
        let remaining = deadline
            .checked_sub(clock.now())
            .filter(|left| !left.is_zero())
            .ok_or_else(|| {
                Failure::new(
                    FailureKind::Deadline,
                    "expected trail landing remained unobserved at the hang detector",
                )
            })?;
        wire.bound_by(remaining.min(REQUEST_BOUND));
        let reply = self.exchange(clock, wire, request, phase);
        if clock.now() >= deadline {
            return Err(Failure::new(
                FailureKind::Deadline,
                "landing reply reached or exceeded the observation deadline",
            ));
        }
        reply
    }

    fn landing_drain(
        &mut self,
        clock: &impl Clock,
        wire: &mut impl Wire,
        phase: &'static str,
        deadline: Duration,
    ) -> Result<(), Failure> {
        let reply = self.landing_request(clock, wire, Request::Drain, phase, deadline)?;
        self.receive_admissions(&reply.stdout, false)
    }

    fn landing_candidate(&self, expected: Landing) -> Result<Option<LandingEvidence>, Failure> {
        for (&seq, row) in self.admissions.rows.range(self.pre_final_key_seq..) {
            if seq <= self.pre_final_key_seq {
                continue;
            }
            let endpoint = field(&row.immutable, "target")
                .and_then(Landing::parse)
                .map_err(|detail| Failure::new(FailureKind::Malformed, detail))?;
            if endpoint != expected {
                continue;
            }
            let verdict = match field(&row.immutable, "phase")
                .map_err(|detail| Failure::new(FailureKind::Malformed, detail))?
            {
                "licensed" => LandingVerdict::Licensed,
                "declined" => LandingVerdict::Declined,
                _ => {
                    return Err(Failure::new(
                        FailureKind::Malformed,
                        "invalid landing admission phase",
                    ));
                }
            };
            return Ok(Some(LandingEvidence { seq, verdict }));
        }
        Ok(None)
    }

    fn observe_landing(&mut self, expected: Landing, clock: &mut impl Clock, wire: &mut impl Wire) {
        let started = clock.now();
        let mut evidence = None;
        let result = (|| -> Result<Duration, Failure> {
            if started < self.finished {
                return Err(Failure::new(
                    FailureKind::Clock,
                    "landing observation clock precedes submission end",
                ));
            }
            let deadline = started
                .checked_add(LANDING_HANG)
                .ok_or_else(|| Failure::new(FailureKind::Clock, "landing deadline overflow"))?;
            loop {
                self.landing_drain(clock, wire, "landing-before-cursor", deadline)?;
                if evidence.is_none() {
                    evidence = self.landing_candidate(expected)?;
                }
                if evidence.is_some_and(|held| held.seq != self.admissions.latest) {
                    return Err(Failure::new(
                        FailureKind::Superseded,
                        "landing admission was superseded before closed-frame agreement",
                    ));
                }
                let before_cursor = self.admissions.latest;
                let reply =
                    self.landing_request(clock, wire, Request::Cursor, "landing-cursor", deadline)?;
                let cursor = closed_cursor(&reply.stdout)?;
                self.landing_drain(clock, wire, "landing-after-cursor", deadline)?;
                if evidence.is_none() {
                    evidence = self.landing_candidate(expected)?;
                }
                if evidence.is_some_and(|held| held.seq != self.admissions.latest) {
                    return Err(Failure::new(
                        FailureKind::Superseded,
                        "landing admission was superseded before closed-frame agreement",
                    ));
                }
                if evidence.is_some()
                    && before_cursor == self.admissions.latest
                    && cursor == Some(expected)
                {
                    let accepted = clock.now();
                    if accepted >= deadline {
                        return Err(Failure::new(
                            FailureKind::Deadline,
                            "landing agreement reached or exceeded the observation deadline",
                        ));
                    }
                    return Ok(accepted);
                }
                let now = clock.now();
                let due = now
                    .checked_add(LANDING_POLL)
                    .unwrap_or(deadline)
                    .min(deadline);
                clock
                    .sleep_until(due)
                    .map_err(|detail| Failure::new(FailureKind::Interrupted, detail))?;
            }
        })();
        let accepted = result.as_ref().ok().copied();
        wire.bound_by(REQUEST_BOUND);
        let failure = result.err();
        if let Some(failure) = &failure {
            self.error = Some(failure.detail.clone());
        }
        self.landing = Some(LandingObservation {
            expected,
            after_seq: self.pre_final_key_seq,
            started,
            ended: accepted.unwrap_or_else(|| clock.now()),
            evidence,
            failure,
        });
    }
}

fn record(
    text: &str,
    cadence: Duration,
    clock: &mut impl Clock,
    wire: &mut impl Wire,
) -> Recording {
    let mut recording = Recording::default();
    let result = (|| -> Result<(), String> {
        validate_text(text)?;
        cadence
            .checked_mul(u32::try_from(text.len()).map_err(|_| "too many keys")?)
            .ok_or("cadence duration overflow")?;
        recording.drain(clock, wire, "baseline", true)?;
        let target = recording.target.ok_or("no replay target")?;
        let mut planned = clock.now();
        for (index, byte) in text.bytes().enumerate() {
            clock.sleep_until(planned)?;
            recording.drain(clock, wire, "before-key", false)?;
            if index + 1 == text.len() {
                recording.pre_final_key_seq = recording.admissions.latest;
            }
            if clock.now() < planned {
                return Err("controller wait ended before the planned key time".into());
            }
            let key = key_name(byte);
            let result = recording.request(
                clock,
                wire,
                Request::Key(target.session, key.clone()),
                "key",
            );
            let exchange = recording
                .exchanges
                .last()
                .filter(|exchange| exchange.phase == "key")
                .ok_or("key exchange was not retained")?;
            let start = exchange.start;
            let ack = result.as_ref().ok().map(|_| exchange.end);
            recording.events.push(KeyEvent {
                key,
                planned,
                start,
                ack,
                drain_start: None,
                drain_end: None,
            });
            result?;
            if start < planned {
                return Err("controller wait ended before the planned key time".into());
            }
            let event = recording.events.last_mut().ok_or("missing key event")?;
            event.drain_start = Some(clock.now());
            recording.drain(clock, wire, "after-key", false)?;
            recording
                .events
                .last_mut()
                .ok_or("missing key event")?
                .drain_end = Some(clock.now());
            planned = start.checked_add(cadence).ok_or("key deadline overflow")?;
        }
        recording.drain(clock, wire, "final", false)?;
        Ok(())
    })();
    recording.finished = clock.now();
    recording.replay_last_seq = recording.admissions.latest;
    recording.replay_complete = result.is_ok();
    recording.error = result.err();
    recording
}

trait Sink {
    fn save(&mut self, recording: &Recording) -> Result<(), String>;
}

fn run(
    text: &str,
    cadence: Duration,
    capture_images: bool,
    expect_trail_landing: Option<Landing>,
    clock: &mut impl Clock,
    wire: &mut impl Wire,
    sink: &mut impl Sink,
) -> Result<Recording, String> {
    let mut recording = record(text, cadence, clock, wire);
    if recording.error.is_none()
        && let Some(expected) = expect_trail_landing
    {
        recording.observe_landing(expected, clock, wire);
    }
    if recording.error.is_none() {
        let post = (|| -> Result<(), String> {
            // These are retained instance metrics, read before PNG work can perturb them.
            for request in [Request::Metrics, Request::Percentiles] {
                recording.request(clock, wire, request, "post-replay-metrics")?;
            }
            if capture_images {
                for clean in [false, true] {
                    let reply = recording.request(
                        clock,
                        wire,
                        Request::Image(clean),
                        "post-replay-image",
                    )?;
                    let (png, metadata) = image_bytes(&reply.stdout)?;
                    if recording
                        .bytes
                        .saturating_add(recording.admissions.bytes)
                        .saturating_add(png.len())
                        .saturating_add(metadata.len())
                        > MAX_RETAINED_BYTES
                    {
                        return Err("image retention bound exceeded".into());
                    }
                    recording.bytes += png.len() + metadata.len();
                    recording.images.push((clean, png, metadata.to_string()));
                }
            }
            // Later verdicts stay outside the replay window, whether or not images were requested.
            recording.drain(
                clock,
                wire,
                if capture_images {
                    "post-capture-admissions"
                } else {
                    "post-metrics-admissions"
                },
                false,
            )?;
            // These reads describe later state; they are not substituted for either image.
            for request in [
                Request::Status,
                Request::Cursor,
                Request::Text,
                Request::Dims,
            ] {
                recording.request(clock, wire, request, "later-sidecar")?;
            }
            Ok(())
        })();
        recording.error = post.err();
    }
    sink.save(&recording)?;
    Ok(recording)
}

fn micros(value: Duration) -> u64 {
    u64::try_from(value.as_micros()).unwrap_or(u64::MAX)
}

struct Files<'a> {
    options: &'a Options,
    window: &'a str,
    out: crate::sys::NewOutputDir,
}

impl Sink for Files<'_> {
    fn save(&mut self, recording: &Recording) -> Result<(), String> {
        let write = |name: &str, bytes: &[u8]| {
            self.out
                .write(name, bytes)
                .map_err(|e| format!("cannot write {name}: {e}"))
        };
        let mut replies = String::new();
        for exchange in &recording.exchanges {
            let (code, stdout, stderr, error) = match &exchange.reply {
                Ok(reply) => (
                    Some(reply.code),
                    reply.stdout.as_str(),
                    reply.stderr.as_str(),
                    None,
                ),
                Err(error) => (None, "", "", Some(error)),
            };
            let value = json!({"phase":exchange.phase,"request":exchange.request,"start_us":micros(exchange.start),"end_us":micros(exchange.end),"code":code,"stdout":stdout,"stderr":stderr,"error":error});
            replies.push_str(&aterm_json::to_string(&value).map_err(|e| e.to_string())?);
            replies.push('\n');
        }
        write("replies.jsonl", replies.as_bytes())?;
        let mut events = String::from(
            "index\tkey\tplanned_us\tstart_us\tack_us\tdrain_start_us\tdrain_end_us\tlate_us\n",
        );
        let shown = |value: Option<Duration>| {
            value.map_or_else(|| "-".into(), |value| micros(value).to_string())
        };
        for (index, event) in recording.events.iter().enumerate() {
            events.push_str(&format!(
                "{}\t{:?}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                index + 1,
                event.key,
                micros(event.planned),
                micros(event.start),
                shown(event.ack),
                shown(event.drain_start),
                shown(event.drain_end),
                micros(event.start.saturating_sub(event.planned))
            ));
        }
        write("events.tsv", events.as_bytes())?;
        let admissions = format!(
            "# Deduplicated immutable fields; age_ms is retained in raw replies. Baseline sequence {} precedes replay.\n{}\n",
            recording.admissions.baseline,
            recording
                .admissions
                .rows
                .values()
                .map(|row| row.immutable.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        );
        write("admissions.txt", admissions.as_bytes())?;
        for (clean, png, metadata) in &recording.images {
            let stem = if *clean {
                "later-clean"
            } else {
                "later-effect"
            };
            write(&format!("{stem}.png"), png)?;
            write(&format!("{stem}.image-meta.txt"), metadata.as_bytes())?;
        }
        let landing = recording.landing.as_ref().map_or_else(
            || json!({
                "status":if self.options.expect_trail_landing.is_some() {"not-started"} else {"not-requested"},
                "expected":self.options.expect_trail_landing.map(|at|json!({"row":at.row,"col":at.col})),
            }),
            |observation| json!({
                "status":if observation.failure.is_some() {"unobserved"} else {"agreement-observed"},
                "expected":{"row":observation.expected.row,"col":observation.expected.col},
                "source":recording.target.map(|target|json!({"window":target.window,"view":target.view,"local_session":target.session})),
                "after_admission_seq":observation.after_seq,
                "observation_start_us":micros(observation.started),
                "observation_end_us":micros(observation.ended),
                "admission_seq":observation.evidence.map(|held|held.seq),
                "admission_phase":observation.evidence.map(|held|held.verdict.name()),
                "failure_kind":observation.failure.as_ref().map(|failure|failure.kind.name()),
                "failure":observation.failure.as_ref().map(|failure|failure.detail.as_str()),
                "scope":"Fresh source-bound admission and matching closed-frame cursor, bracketed by observed target/sequence fences. Not atomic, glyph delivery, input latency or display evidence.",
            }),
        );
        let summary = json!({
            "tag":self.options.tag,"text":self.options.text,"cadence_us":micros(self.options.cadence),
            "image_capture_mode":if self.options.no_images {"no-images"} else {"effect-and-clean"},
            "images_skipped":self.options.no_images,"images_retained":recording.images.len(),
            "instance_window":self.window,"capture_scope":match self.window {"1"=>"native instance; key acknowledgments are control acceptance only","0"=>"headless functional capture; no native display-latency evidence",_=>"instance window mode unknown; no native display-latency evidence"},
            "controller_replay_complete":recording.replay_complete,"error":recording.error,
            "trail_landing_observation":landing,
            "keys_requested":self.options.text.len(),"keys_acknowledged":recording.events.iter().filter(|event|event.ack.is_some()).count(),
            "late_starts":recording.events.iter().filter(|event|event.start > event.planned).count(),"replay_end_us":micros(recording.finished),
            "target":recording.target.map(|target|json!({"window":target.window,"view":target.view,"local_session":target.session})),
            "baseline_admission_seq":recording.admissions.baseline,"replay_last_admission_seq":recording.replay_last_seq,"latest_observed_admission_seq":recording.admissions.latest,
            "admission_coverage_inconclusive":recording.error.is_some() || !recording.admissions.gaps.is_empty(),"coverage_notes":recording.admissions.gaps,
            "admission_scope":"Observed move verdicts, not one record per key; zero immediate records can be pending. Ring gaps are not key-drop verdicts. Front/source fences are observed, not atomic.",
            "timing_scope":"Controller monotonic planned/start/ack/drain times; acknowledgments do not establish glyph echo or input-to-display latency. No catch-up cadence.",
            "metrics_scope":"Retained instance counters and percentiles read after replay and any requested landing observation, before optional images. This recorder does not reset counters; document any preceding caller reset. No interval-specific latency attribution is inferred.",
            "image_scope":if self.options.no_images {"Image requests skipped by --no-images. Cursor/text/status/dims are separate later state."} else {"Post-replay captures; raw metadata retained, effect/clean comparability not evaluated. Cursor/text/status/dims are separate later state."},
            "pixel_continuity":"not judged"
        });
        write(
            "summary.json",
            aterm_json::to_string(&summary)
                .map_err(|e| e.to_string())?
                .as_bytes(),
        )
    }
}

/// Record named edit keys against one existing lab; neither launch nor submit a prompt.
/// # Errors
/// Invalid options, an unavailable lab/socket, or output that cannot be created or retained.
pub fn cmd(args: &[String]) -> Res {
    let options = Options::parse(args).map_err(Fail::usage)?;
    let run_record = crate::run::Run::load(&options.tag).map_err(Fail::die)?;
    let door = crate::door::Door::new(&run_record).map_err(Fail::die)?;
    let wire = door.connect().map_err(Fail::die)?.bounded(REQUEST_BOUND);
    let out = crate::sys::NewOutputDir::create(&options.out).map_err(|e| {
        Fail::die(format!(
            "--out must name a new directory with an existing parent: {e}"
        ))
    })?;
    crate::stop::install();
    let epoch = Instant::now();
    let mut live = Live { wire, epoch };
    let mut clock = LiveClock(live.epoch);
    let recording = run(
        &options.text,
        options.cadence,
        !options.no_images,
        options.expect_trail_landing,
        &mut clock,
        &mut live,
        &mut Files {
            options: &options,
            window: &run_record.window,
            out,
        },
    )
    .map_err(Fail::die)?;
    let complete = recording.error.is_none() && recording.admissions.gaps.is_empty();
    let landing_status = if options.expect_trail_landing.is_some() {
        let outcome = recording.landing.as_ref().map_or_else(
            || "landing agreement not started".to_string(),
            |observation| {
                if let Some(failure) = &observation.failure {
                    format!("landing agreement unobserved ({})", failure.kind.name())
                } else if let Some(evidence) = observation.evidence {
                    format!(
                        "landing agreement observed, admission {}",
                        evidence.verdict.name()
                    )
                } else {
                    "landing agreement unobserved".to_string()
                }
            },
        );
        format!(
            "; controller replay {}; {outcome}",
            if recording.replay_complete {
                "complete"
            } else {
                "incomplete"
            }
        )
    } else {
        String::new()
    };
    println!(
        "trail-replay: {} recording{}; {} accepted keys, {} coverage notes; pixel continuity not judged; {}",
        if complete { "complete" } else { "inconclusive" },
        landing_status,
        recording
            .events
            .iter()
            .filter(|event| event.ack.is_some())
            .count(),
        recording.admissions.gaps.len(),
        options.out.display()
    );
    Ok(if complete { 0 } else { 1 })
}

#[cfg(test)]
#[path = "trail_replay_tests.rs"]
mod tests;
