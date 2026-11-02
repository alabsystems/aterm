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

struct Options {
    tag: String,
    out: PathBuf,
    text: String,
    cadence: Duration,
    no_images: bool,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut args = args.to_vec();
        let tag = crate::run::take_only_tag(&mut args)?;
        let (mut out, mut text, mut cadence) = (None, None, None);
        let mut no_images = false;
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
}

struct Live {
    wire: aterm_agent::RelayClient<aterm_uds::CtlStream>,
    epoch: Instant,
}

impl Wire for Live {
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
    ) -> Result<(), String> {
        let last = rows.last().map_or(0, |row| row.seq);
        if !baseline && last < self.latest {
            return Err("admission sequence reset; coverage is unobserved".into());
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
                    return Err("an admission sequence changed immutable facts".into());
                }
                continue;
            }
            if !baseline && row.seq > self.latest {
                if row.view != Some(target.view) || row.session != Some(target.session) {
                    return Err(format!(
                        "admission {} has an unbound or changed source",
                        row.seq
                    ));
                }
                if self.latest.checked_add(1) != Some(row.seq) {
                    self.gaps.push(format!("unobserved admission sequences after {} before {}; eviction or preservation, not a key-drop verdict", self.latest, row.seq));
                }
                self.latest = row.seq;
            } else if !baseline {
                return Err("an older uncollected admission appeared".into());
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
    replay_complete: bool,
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
            return Err("recorder retention bound exceeded; response was not retained".into());
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
            return Err("controller monotonic clock regressed".into());
        }
        let reply = reply?;
        if reply.code != 0 {
            return Err(format!(
                "control request refused: {}{}",
                reply.stdout, reply.stderr
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
        let (target, rows) = batch(&reply.stdout)?;
        if self.target.is_some_and(|held| held != target) {
            return Err("global-front window/view/session changed".into());
        }
        self.target = Some(target);
        self.admissions.observe(rows, target, baseline)?;
        if self.bytes.saturating_add(self.admissions.bytes) > MAX_RETAINED_BYTES {
            return Err("recorder retention bound exceeded".into());
        }
        Ok(())
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
        for byte in text.bytes() {
            clock.sleep_until(planned)?;
            recording.drain(clock, wire, "before-key", false)?;
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
    clock: &mut impl Clock,
    wire: &mut impl Wire,
    sink: &mut impl Sink,
) -> Result<Recording, String> {
    let mut recording = record(text, cadence, clock, wire);
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
        let summary = json!({
            "tag":self.options.tag,"text":self.options.text,"cadence_us":micros(self.options.cadence),
            "image_capture_mode":if self.options.no_images {"no-images"} else {"effect-and-clean"},
            "images_skipped":self.options.no_images,"images_retained":recording.images.len(),
            "instance_window":self.window,"capture_scope":match self.window {"1"=>"native instance; key acknowledgments are control acceptance only","0"=>"headless functional capture; no native display-latency evidence",_=>"instance window mode unknown; no native display-latency evidence"},
            "controller_replay_complete":recording.replay_complete,"error":recording.error,
            "keys_requested":self.options.text.len(),"keys_acknowledged":recording.events.iter().filter(|event|event.ack.is_some()).count(),
            "late_starts":recording.events.iter().filter(|event|event.start > event.planned).count(),"replay_end_us":micros(recording.finished),
            "target":recording.target.map(|target|json!({"window":target.window,"view":target.view,"local_session":target.session})),
            "baseline_admission_seq":recording.admissions.baseline,"replay_last_admission_seq":recording.replay_last_seq,"latest_observed_admission_seq":recording.admissions.latest,
            "admission_coverage_inconclusive":recording.error.is_some() || !recording.admissions.gaps.is_empty(),"coverage_notes":recording.admissions.gaps,
            "admission_scope":"Observed move verdicts, not one record per key; zero immediate records can be pending. Ring gaps are not key-drop verdicts. Front/source fences are observed, not atomic.",
            "timing_scope":"Controller monotonic planned/start/ack/drain times; acknowledgments do not establish glyph echo or input-to-display latency. No catch-up cadence.",
            "metrics_scope":"Retained instance counters and percentiles read after replay, before optional images. This recorder does not reset counters; document any preceding caller reset. No interval-specific latency attribution is inferred.",
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
    let wire = door
        .connect()
        .map_err(Fail::die)?
        .bounded(Duration::from_secs(60));
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
    println!(
        "trail-replay: {} recording; {} accepted keys, {} coverage notes; pixel continuity not judged; {}",
        if complete { "complete" } else { "inconclusive" },
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
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::rc::Rc;

    struct FakeClock {
        now: Rc<Cell<Duration>>,
        oversleep: Duration,
    }
    impl Clock for FakeClock {
        fn now(&self) -> Duration {
            self.now.get()
        }
        fn sleep_until(&mut self, due: Duration) -> Result<(), String> {
            if due > self.now() {
                self.now.set(due + self.oversleep);
            }
            Ok(())
        }
    }

    struct FakeWire {
        now: Rc<Cell<Duration>>,
        trace: Rc<RefCell<Vec<String>>>,
        ring: VecDeque<(u64, bool)>,
        seq: u64,
        keys: usize,
        drain_cost: Duration,
        key_costs: Vec<Duration>,
        gap_at: Option<usize>,
        fail_at: Option<usize>,
        transport_at: Option<usize>,
        target_at: Option<usize>,
        source_at: Option<usize>,
        emit: bool,
        emit_on_image: bool,
        image: String,
    }

    fn image_reply() -> String {
        let mut png = Vec::new();
        {
            let mut encoder = aterm_png::Encoder::new(&mut png, 1, 1);
            encoder.set_color(aterm_png::ColorType::Rgba);
            encoder.set_depth(aterm_png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[0, 0, 0, 255]).unwrap();
            writer.finish().unwrap();
        }
        format!(
            "image-meta frame-kind=terminal window=1 view=2\n1 1 {} {}\n",
            png.len(),
            aterm_codec::base64::encode(&png).unwrap()
        )
    }

    impl FakeWire {
        fn new(now: Rc<Cell<Duration>>, trace: Rc<RefCell<Vec<String>>>) -> Self {
            Self {
                now,
                trace,
                ring: VecDeque::new(),
                seq: 0,
                keys: 0,
                drain_cost: Duration::ZERO,
                key_costs: Vec::new(),
                gap_at: None,
                fail_at: None,
                transport_at: None,
                target_at: None,
                source_at: None,
                emit: true,
                emit_on_image: false,
                image: image_reply(),
            }
        }
        fn advance(&self, duration: Duration) {
            self.now.set(self.now.get() + duration);
        }
    }

    impl Wire for FakeWire {
        fn request(&mut self, request: &Request) -> Result<Reply, String> {
            self.trace.borrow_mut().push(request.words().join(" "));
            let mut reply = Reply {
                code: 0,
                stdout: String::new(),
                stderr: String::new(),
            };
            match request {
                Request::Key(session, name) => {
                    assert_eq!(*session, 3);
                    assert!(!["enter", "return", "ctrl+j", "ctrl+m"].contains(&name.as_str()));
                    self.keys += 1;
                    self.advance(
                        self.key_costs
                            .get(self.keys - 1)
                            .copied()
                            .unwrap_or_default(),
                    );
                    if self.fail_at == Some(self.keys) {
                        reply.code = 1;
                        reply.stderr = "ERR refused fixture key\n".into();
                    } else if self.emit {
                        self.seq += if self.gap_at == Some(self.keys) { 3 } else { 1 };
                        self.ring
                            .push_back((self.seq, self.source_at != Some(self.keys)));
                        if self.ring.len() > RING {
                            self.ring.pop_front();
                        }
                    }
                    reply.stdout = "OK seq=8\n".into();
                }
                Request::Drain => {
                    self.advance(self.drain_cost);
                    if self.keys > 0 && self.transport_at == Some(self.keys) {
                        return Err("fixture transport failed".into());
                    }
                    let view = if self.keys > 0 && self.target_at == Some(self.keys) {
                        9
                    } else {
                        2
                    };
                    reply.stdout =
                        format!("trail-target selection=front window=1 view={view} session=3\n");
                    for &(seq, source_ok) in &self.ring {
                        reply.stdout.push_str(&format!("admission seq={seq} phase=licensed reason=key age_ms={} origin=0,0 target=0,1 alt=0 licence=key source=bound source_view={} source_session=3\n", self.now.get().as_millis(), if source_ok { 2 } else { 9 }));
                    }
                }
                Request::Image(clean) => {
                    self.advance(Duration::from_secs(3));
                    if self.emit_on_image && !*clean {
                        self.seq += 1;
                        self.ring.push_back((self.seq, true));
                        if self.ring.len() > RING {
                            self.ring.pop_front();
                        }
                    }
                    reply.stdout.clone_from(&self.image);
                }
                Request::Metrics | Request::Percentiles => reply.stdout = "OK n_input=0\n".into(),
                Request::Status => reply.stdout = "later trail status\n".into(),
                Request::Cursor => reply.stdout = "OK 0 3\n".into(),
                Request::Text => reply.stdout = "later text\n".into(),
                Request::Dims => reply.stdout = "OK 2 4 4 2\n".into(),
            }
            Ok(reply)
        }
    }

    struct FakeSink {
        trace: Rc<RefCell<Vec<String>>>,
        now: Rc<Cell<Duration>>,
        saved: bool,
    }
    impl Sink for FakeSink {
        fn save(&mut self, _: &Recording) -> Result<(), String> {
            self.trace.borrow_mut().push("WRITE".into());
            self.now.set(self.now.get() + Duration::from_secs(3));
            self.saved = true;
            Ok(())
        }
    }

    fn rig() -> (FakeClock, FakeWire, FakeSink) {
        let now = Rc::new(Cell::new(Duration::ZERO));
        let trace = Rc::new(RefCell::new(Vec::new()));
        (
            FakeClock {
                now: Rc::clone(&now),
                oversleep: Duration::ZERO,
            },
            FakeWire::new(Rc::clone(&now), Rc::clone(&trace)),
            FakeSink {
                trace,
                now,
                saved: false,
            },
        )
    }

    fn output_options(out: PathBuf) -> Options {
        Options {
            tag: "test".into(),
            out,
            text: "A ".into(),
            cadence: Duration::from_millis(1),
            no_images: false,
        }
    }

    #[test]
    fn file_sink_writes_parseable_artifacts_once() {
        let scratch = aterm_tempfile::Builder::new()
            .prefix("trail-replay-output-")
            .tempdir()
            .unwrap();
        let options = output_options(scratch.path().join("take"));
        let mut files = Files {
            out: crate::sys::NewOutputDir::create(&options.out).unwrap(),
            options: &options,
            window: "0",
        };
        let (mut clock, mut wire, _) = rig();
        let recording = run(
            &options.text,
            options.cadence,
            !options.no_images,
            &mut clock,
            &mut wire,
            &mut files,
        )
        .unwrap();
        assert!(recording.error.is_none());
        let tsv = std::fs::read_to_string(options.out.join("events.tsv")).unwrap();
        let rows: Vec<Vec<_>> = tsv.lines().map(|line| line.split('\t').collect()).collect();
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|row| row.len() == 8));
        assert_eq!(rows[1][1], "\"shift+a\"");
        for row in &rows[1..] {
            for column in [0, 2, 3, 4, 5, 6, 7] {
                row[column].parse::<u64>().unwrap();
            }
        }
        let jsonl = std::fs::read_to_string(options.out.join("replies.jsonl")).unwrap();
        let replies: Vec<aterm_json::Value> = jsonl
            .lines()
            .map(|line| aterm_json::from_str(line).unwrap())
            .collect();
        assert_eq!(replies.len(), recording.exchanges.len());
        assert!(
            replies
                .iter()
                .any(|reply| reply["request"].as_str() == Some("@3 key shift+a"))
        );
        let summary = std::fs::read_to_string(options.out.join("summary.json")).unwrap();
        let parsed: aterm_json::Value = aterm_json::from_str(&summary).unwrap();
        assert_eq!(parsed["controller_replay_complete"].as_bool(), Some(true));
        assert_eq!(
            parsed["image_capture_mode"].as_str(),
            Some("effect-and-clean")
        );
        assert_eq!(parsed["images_skipped"].as_bool(), Some(false));
        assert_eq!(parsed["images_retained"].as_u64(), Some(2));
        assert!(files.save(&recording).is_err());
        assert_eq!(
            std::fs::read_to_string(options.out.join("replies.jsonl")).unwrap(),
            jsonl
        );
        assert_eq!(
            std::fs::read_to_string(options.out.join("summary.json")).unwrap(),
            summary
        );
    }

    #[test]
    fn no_images_keeps_observations_and_writes_an_explicit_capture_mode() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let out = scratch.path().join("take");
        let args = [
            "test",
            "--out",
            out.to_str().unwrap(),
            "--text",
            "A ",
            "--cadence-ms",
            "1",
            "--no-images",
        ]
        .map(String::from)
        .to_vec();
        let options = Options::parse(&args).unwrap();
        assert!(options.no_images);
        let mut duplicate = args.clone();
        duplicate.push("--no-images".into());
        assert!(Options::parse(&duplicate).is_err());
        let mut files = Files {
            out: crate::sys::NewOutputDir::create(&options.out).unwrap(),
            options: &options,
            window: "1",
        };
        let (mut clock, mut wire, _) = rig();
        wire.emit_on_image = true;
        let recording = run(
            &options.text,
            options.cadence,
            !options.no_images,
            &mut clock,
            &mut wire,
            &mut files,
        )
        .unwrap();
        assert!(recording.error.is_none());
        assert!(recording.replay_complete);
        assert!(recording.images.is_empty());
        let trace = wire.trace.borrow();
        let metrics = trace
            .iter()
            .position(|request| request == "metrics")
            .unwrap();
        assert_eq!(
            &trace[metrics..],
            [
                "metrics",
                "metrics percentiles",
                "trail 32",
                "trail status",
                "cursor",
                "text",
                "dims"
            ]
        );
        assert!(trace.iter().all(|request| !request.starts_with("image ")));
        assert!(
            recording
                .exchanges
                .iter()
                .any(|exchange| exchange.phase == "post-metrics-admissions")
        );
        assert!(recording.exchanges.iter().all(|exchange| !matches!(
            exchange.phase,
            "post-replay-image" | "post-capture-admissions"
        )));
        for stem in ["later-effect", "later-clean"] {
            assert!(!options.out.join(format!("{stem}.png")).exists());
            assert!(!options.out.join(format!("{stem}.image-meta.txt")).exists());
        }
        let summary: aterm_json::Value =
            aterm_json::from_slice(&std::fs::read(options.out.join("summary.json")).unwrap())
                .unwrap();
        assert_eq!(summary["image_capture_mode"].as_str(), Some("no-images"));
        assert_eq!(summary["images_skipped"].as_bool(), Some(true));
        assert_eq!(summary["images_retained"].as_u64(), Some(0));
        assert!(summary["error"].is_null());
        assert_eq!(summary["pixel_continuity"].as_str(), Some("not judged"));
    }

    #[test]
    fn file_sink_refuses_planted_symlinks_and_existing_artifacts() {
        let scratch = aterm_tempfile::Builder::new()
            .prefix("trail-replay-output-links-")
            .tempdir()
            .unwrap();
        let options = output_options(scratch.path().join("take"));
        let mut files = Files {
            out: crate::sys::NewOutputDir::create(&options.out).unwrap(),
            options: &options,
            window: "0",
        };
        let victim = scratch.path().join("outside.txt");
        let artifact = options.out.join("replies.jsonl");
        std::fs::write(&victim, b"preserve").unwrap();
        std::os::unix::fs::symlink(&victim, &artifact).unwrap();
        // The former pathname writer follows this link and truncates its target.
        std::fs::write(&artifact, b"legacy overwrite").unwrap();
        assert_eq!(std::fs::read(&victim).unwrap(), b"legacy overwrite");
        std::fs::write(&victim, b"preserve").unwrap();
        assert!(files.save(&Recording::default()).is_err());
        assert_eq!(std::fs::read(&victim).unwrap(), b"preserve");
        assert!(!options.out.join("events.tsv").exists());
        std::fs::remove_file(&artifact).unwrap();
        std::fs::write(&artifact, b"existing artifact").unwrap();
        assert!(files.save(&Recording::default()).is_err());
        assert_eq!(std::fs::read(&artifact).unwrap(), b"existing artifact");
        assert!(files.out.write("../outside.txt", b"escape").is_err());
        assert_eq!(std::fs::read(&victim).unwrap(), b"preserve");
        assert!(crate::sys::NewOutputDir::create(&options.out).is_err());
    }

    #[test]
    fn file_sink_refuses_replaced_output_directories_and_ancestors() {
        for replace_parent in [false, true] {
            let scratch = aterm_tempfile::Builder::new()
                .prefix("trail-replay-output-replaced-")
                .tempdir()
                .unwrap();
            let parent = scratch.path().join("parent");
            std::fs::create_dir(&parent).unwrap();
            let options = output_options(parent.join("take"));
            let mut files = Files {
                out: crate::sys::NewOutputDir::create(&options.out).unwrap(),
                options: &options,
                window: "0",
            };
            let replaced = if replace_parent {
                &parent
            } else {
                &options.out
            };
            let held = scratch.path().join("held");
            std::fs::rename(replaced, &held).unwrap();
            let outside = scratch.path().join("outside");
            std::fs::create_dir(&outside).unwrap();
            if replace_parent {
                std::fs::create_dir(outside.join("take")).unwrap();
            }
            std::os::unix::fs::symlink(&outside, replaced).unwrap();
            assert!(files.save(&Recording::default()).is_err());
            assert!(!options.out.join("replies.jsonl").exists());
            std::fs::remove_file(replaced).unwrap();
            std::fs::create_dir(replaced).unwrap();
            if replace_parent {
                std::fs::create_dir(&options.out).unwrap();
            }
            assert!(files.save(&Recording::default()).is_err());
            assert_eq!(std::fs::read_dir(&options.out).unwrap().count(), 0);
            let held_out = if replace_parent {
                held.join("take")
            } else {
                held
            };
            assert_eq!(std::fs::read_dir(held_out).unwrap().count(), 0);
        }
    }

    #[test]
    fn input_options_and_ascii_mapping_refuse_submission_routes_before_any_request() {
        let good = |text: &str| {
            ["tag", "--out", "take", "--text", text, "--cadence-ms", "70"]
                .map(String::from)
                .to_vec()
        };
        assert!(!Options::parse(&good("A z+!?\\\" ' ")).unwrap().no_images);
        for invalid in [
            "".to_string(),
            "a".repeat(MAX_KEYS + 1),
            "a\nb".into(),
            "a\rb".into(),
            "\x1b".into(),
            "\x7f".into(),
            "é".into(),
        ] {
            assert!(Options::parse(&good(&invalid)).is_err());
            let (mut clock, mut wire, _) = rig();
            assert!(
                record(&invalid, Duration::from_millis(70), &mut clock, &mut wire)
                    .error
                    .is_some()
            );
            assert!(wire.trace.borrow().is_empty());
        }
        for args in [
            vec!["tag", "--out", "take", "--text", "a"],
            vec!["tag", "--text", "a", "--cadence-ms", "70"],
            vec!["tag", "--out", "take", "--cadence-ms", "70"],
            vec!["tag", "--out", "take", "--text", "a", "--cadence-ms", "0"],
            vec![
                "tag",
                "--out",
                "take",
                "--text",
                "a",
                "--cadence-ms",
                "60001",
            ],
            vec![
                "tag",
                "--out",
                "take",
                "--text",
                "a",
                "--cadence-ms",
                "18446744073709551616",
            ],
            vec![
                "tag",
                "--out",
                "take",
                "--text",
                "a",
                "--text",
                "b",
                "--cadence-ms",
                "70",
            ],
        ] {
            assert!(
                Options::parse(&args.into_iter().map(String::from).collect::<Vec<_>>()).is_err()
            );
        }
        assert_eq!(
            b"AZ az+!?\\\""
                .iter()
                .map(|&byte| key_name(byte))
                .collect::<Vec<_>>(),
            [
                "shift+a", "shift+z", "space", "a", "z", "+", "!", "?", "\\", "\""
            ]
        );
        assert!(validate_text(&"a".repeat(MAX_KEYS)).is_ok());
    }

    #[test]
    fn actual_recording_flow_defers_images_and_writes_and_never_submits() {
        let (mut clock, mut wire, mut sink) = rig();
        wire.drain_cost = Duration::from_millis(1);
        let recording = run(
            "A +",
            Duration::from_millis(10),
            true,
            &mut clock,
            &mut wire,
            &mut sink,
        )
        .unwrap();
        assert!(recording.error.is_none());
        assert!(recording.replay_complete);
        assert_eq!(recording.events.len(), 3);
        assert_eq!(recording.admissions.rows.len(), 3);
        assert!(recording.admissions.gaps.is_empty());
        assert!(sink.saved);
        let trace = wire.trace.borrow();
        let metrics = trace
            .iter()
            .position(|request| request == "metrics")
            .unwrap();
        let last_key = trace
            .iter()
            .rposition(|request| request.starts_with("@3 key "))
            .unwrap();
        assert!(metrics > last_key);
        assert!(
            trace[..metrics]
                .iter()
                .all(|request| request == "trail 32" || request.starts_with("@3 key "))
        );
        assert_eq!(
            &trace[metrics..],
            [
                "metrics",
                "metrics percentiles",
                "image --meta --bytes",
                "image --meta --bytes clean",
                "trail 32",
                "trail status",
                "cursor",
                "text",
                "dims",
                "WRITE"
            ]
        );
        assert!(recording.finished < Duration::from_secs(1));
        assert!(clock.now() >= Duration::from_secs(9));
        assert!(
            recording
                .events
                .iter()
                .all(|event| event.ack.is_some_and(|ack| ack < recording.finished))
        );
        assert_eq!(recording.images.len(), 2);
    }

    #[test]
    fn capture_driven_admissions_are_retained_without_renewing_the_replay_window() {
        let (mut clock, mut wire, mut sink) = rig();
        wire.emit = false;
        wire.emit_on_image = true;
        let recording = run(
            "abc",
            Duration::from_millis(10),
            true,
            &mut clock,
            &mut wire,
            &mut sink,
        )
        .unwrap();
        assert!(recording.error.is_none());
        assert!(recording.replay_complete);
        assert_eq!(recording.replay_last_seq, 0);
        assert_eq!(recording.admissions.latest, 1);
        assert_eq!(recording.admissions.rows.len(), 1);
        assert_eq!(recording.finished, Duration::from_millis(20));
        let late = recording
            .exchanges
            .iter()
            .find(|exchange| exchange.phase == "post-capture-admissions")
            .unwrap();
        assert!(late.start >= recording.finished + Duration::from_secs(6));
        assert!(
            late.reply
                .as_ref()
                .unwrap()
                .stdout
                .contains("admission seq=1 ")
        );
        assert!(
            recording
                .events
                .iter()
                .all(|event| event.ack.is_some_and(|ack| ack <= recording.finished))
        );
    }

    #[test]
    fn pacing_rebases_on_actual_key_starts_without_catching_up_after_slow_io_or_oversleep() {
        for (slow_ack, drain_ms, oversleep_ms) in [(25, 0, 0), (0, 15, 0), (0, 0, 7)] {
            let (mut clock, mut wire, _) = rig();
            wire.key_costs = vec![Duration::from_millis(slow_ack)];
            wire.drain_cost = Duration::from_millis(drain_ms);
            clock.oversleep = Duration::from_millis(oversleep_ms);
            let cadence = Duration::from_millis(10);
            let recording = record("abcd", cadence, &mut clock, &mut wire);
            assert!(recording.error.is_none());
            assert!(
                recording
                    .events
                    .windows(2)
                    .all(|pair| pair[1].planned == pair[0].start + cadence
                        && pair[1].start >= pair[1].planned)
            );
            assert!(
                recording
                    .events
                    .iter()
                    .all(|event| event.planned <= event.start
                        && event.ack.is_some_and(|ack| ack >= event.start)
                        && event.drain_start >= event.ack
                        && event.drain_end >= event.drain_start)
            );
            assert!(
                recording
                    .events
                    .iter()
                    .any(|event| event.start > event.planned)
            );
        }
    }

    #[test]
    fn per_key_drains_keep_more_than_a_ring_and_final_only_negative_control_loses_history() {
        for count in [31, 32, 33, 65] {
            let (mut clock, mut wire, _) = rig();
            let recording = record(
                &"a".repeat(count),
                Duration::from_millis(1),
                &mut clock,
                &mut wire,
            );
            assert!(recording.error.is_none());
            assert_eq!(recording.admissions.rows.len(), count);
            assert!(recording.admissions.gaps.is_empty());
            let (_, mut former, _) = rig();
            let mut control = Admissions::default();
            let initial = former.request(&Request::Drain).unwrap();
            let (target, rows) = batch(&initial.stdout).unwrap();
            control.observe(rows, target, true).unwrap();
            for _ in 0..count {
                former.request(&Request::Key(3, "a".into())).unwrap();
            }
            let final_ring = former.request(&Request::Drain).unwrap();
            let (target, rows) = batch(&final_ring.stdout).unwrap();
            control.observe(rows, target, false).unwrap();
            assert_eq!(control.rows.len(), count.min(RING));
            assert_eq!(control.gaps.is_empty(), count <= RING);
        }
    }

    #[test]
    fn critical_tail_requires_every_fresh_verdict_and_its_bound_source() {
        let (_, mut wire, _) = rig();
        for _ in 0..60 {
            wire.request(&Request::Key(3, "a".into())).unwrap();
        }
        let before = wire.request(&Request::Drain).unwrap().stdout;
        for _ in 0..6 {
            wire.request(&Request::Key(3, "a".into())).unwrap();
        }
        let after = wire.request(&Request::Drain).unwrap().stdout;
        assert_eq!(licensed_tail(&before, &after, 6), Ok(true));
        assert_eq!(licensed_tail(&before, &before, 6), Ok(false));
        let missing = after
            .lines()
            .filter(|line| !line.starts_with("admission seq=66 "))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(licensed_tail(&before, &missing, 6), Ok(false));
        assert!(licensed_tail(&before, &after, 0).is_err());
        for changed in [
            after.replace("window=1", "window=2"),
            after.replace("source_view=2", "source_view=9"),
            after.replace("source=bound", "source=unknown"),
            after.replace("seq=66 ", "seq=67 "),
            after.replace("phase=licensed", "phase=declined"),
        ] {
            assert!(licensed_tail(&before, &changed, 6).is_err());
        }
        // A producer asked for 40 records cannot be silently fed to this 32-row decoder.
        let last = after.lines().last().unwrap();
        let mut oversized = after.clone();
        for seq in 67..75 {
            oversized.push_str(&last.replace("seq=66 ", &format!("seq={seq} ")));
            oversized.push('\n');
        }
        assert!(
            licensed_tail(&before, &oversized, 6)
                .unwrap_err()
                .contains("oversized")
        );
    }

    #[test]
    fn sequence_gaps_are_inconclusive_but_do_not_stop_keys_or_invent_drops() {
        let (mut clock, mut wire, _) = rig();
        wire.gap_at = Some(2);
        let recording = record("abcd", Duration::from_millis(1), &mut clock, &mut wire);
        assert!(recording.replay_complete);
        assert_eq!(wire.keys, 4);
        assert!(!recording.admissions.gaps.is_empty());
        assert_eq!(recording.admissions.rows.len(), 4);
        let (mut clock, mut wire, _) = rig();
        wire.emit = false;
        let pending = record("abc", Duration::from_millis(1), &mut clock, &mut wire);
        assert!(pending.replay_complete);
        assert!(pending.admissions.rows.is_empty());
    }

    #[test]
    fn refused_or_failed_requests_and_source_changes_stop_typing_and_keep_partial_replies() {
        for fault in 0..4 {
            let (mut clock, mut wire, mut sink) = rig();
            match fault {
                0 => wire.fail_at = Some(1),
                1 => wire.transport_at = Some(1),
                2 => wire.target_at = Some(1),
                _ => wire.source_at = Some(1),
            }
            let partial = run(
                "abc",
                Duration::from_millis(1),
                true,
                &mut clock,
                &mut wire,
                &mut sink,
            )
            .unwrap();
            assert_eq!(wire.keys, 1);
            assert!(partial.error.is_some());
            assert!(!partial.replay_complete);
            assert!(sink.saved);
            assert!(partial.images.is_empty());
            assert_eq!(wire.trace.borrow().last().unwrap(), "WRITE");
            assert!(partial.exchanges.last().is_some());
        }
    }
}
