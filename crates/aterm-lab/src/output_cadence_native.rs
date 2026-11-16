// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Functional native output-hold observations. No timing or throughput qualification.

use super::*;
use aterm_json::json;
use std::collections::BTreeMap;

const QUIET: usize = 200;
const SESSIONS: u64 = 202;
const HELP: &str = "usage: lab output-cadence --aterm PATH --aterm-source SHA40 --aterm-sha256 SHA64 --aterm-build-record JSONL --functional-memory-mib N --out NEW_ABSOLUTE_DIR\n\nOpens two private native windows with an output producer, a native typing pane and\n200 quiet raw-cat peers. Observe an idle pool, then a natural 1 ms output stream,\na dirty source hold and a later rearm between native input bursts. Require exact\noutput retention, input booking, stable native topology and retirement to an empty\ncadence index, followed by another idle observation. The app must be a genuine\ndebug host build bound to the supplied trusted compiler record; the source SHA\nis declared provenance, not a cryptographic source-to-artifact proof.\n\nThe explicit positive memory budget covers the whole owned process envelope plus\nthe ordinary separate spare. Uses ordinary Functional window admission. No Timing\nauthority, accepted CSV or CPU/latency gain is produced. Raw observations and exact\ncleanup results survive failure. Exit 0 functional checks passed; 2 usage; 3 failure.\n";

struct Options {
    build: Build,
    budget: u64,
    out: PathBuf,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self, Fail> {
        let mut args = args.to_vec();
        let budget = take_value(&mut args, "--functional-memory-mib")?
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|n| *n > 0)
            .and_then(|n| n.checked_mul(1024 * 1024))
            .ok_or_else(|| {
                Fail::usage("--functional-memory-mib requires positive MiB without overflow")
            })?;
        let out = PathBuf::from(
            take_value(&mut args, "--out")?.ok_or_else(|| Fail::usage("--out is required"))?,
        );
        if !out.is_absolute() {
            return Err(Fail::usage("--out must be an absolute new directory"));
        }
        let build = Build::parse(&mut args, "aterm")?;
        if !args.is_empty() {
            return Err(Fail::usage(format!("unknown arguments: {args:?}")));
        }
        Ok(Self { build, budget, out })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Cadence {
    now_ns: u64,
    sessions: u64,
    ordered: u64,
    indexed: u64,
    session: u64,
    pooled: bool,
    enrolled: bool,
    current: bool,
    held: bool,
    dirty: bool,
    due_ns: u64,
}

impl Cadence {
    fn parse(reply: &str) -> Result<Self, String> {
        let mut words = reply.split_whitespace();
        if words.next() != Some("OK") {
            return Err("cadence snapshot is not an OK record".into());
        }
        let mut f = BTreeMap::new();
        for word in words {
            let (key, value) = word.split_once('=').ok_or("invalid cadence record field")?;
            if f.insert(key, value).is_some() {
                return Err(format!("duplicate cadence field {key}"));
            }
        }
        let number = |key: &str| {
            f.get(key)
                .and_then(|s| s.parse::<u64>().ok())
                .ok_or_else(|| format!("missing or invalid cadence field {key}"))
        };
        let flag = |key: &str| match number(key)? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(format!("invalid cadence flag {key}")),
        };
        if number("cadence_schema")? != 1 {
            return Err("unknown cadence snapshot schema".into());
        }
        Ok(Self {
            now_ns: number("now_ns")?,
            sessions: number("sessions")?,
            ordered: number("ordered")?,
            indexed: number("indexed")?,
            session: number("session")?,
            pooled: flag("pooled")?,
            enrolled: flag("enrolled")?,
            current: flag("current")?,
            held: flag("held")?,
            dirty: flag("dirty")?,
            due_ns: number("due_ns")?,
        })
    }

    fn consistent(self, session: u64) -> Result<Self, String> {
        if self.sessions != SESSIONS
            || self.session != session
            || !self.pooled
            || self.ordered != self.indexed
            || self.indexed > 1
            || (self.enrolled && (!self.current || self.indexed != 1 || self.due_ns == 0))
            || (!self.enrolled && (self.current || self.due_ns != 0))
            || (self.held && !self.enrolled)
            || (self.dirty && !self.held)
        {
            return Err(format!(
                "cadence ownership or closed-world topology disagrees: {self:?}"
            ));
        }
        Ok(self)
    }

    fn idle(self) -> bool {
        self.ordered == 0 && self.indexed == 0 && !self.enrolled && !self.held && !self.dirty
    }
}

#[derive(Default)]
struct Witness {
    dirty_due: Option<u64>,
    rearmed_due: Option<u64>,
    positive_intervals: Vec<(u64, u64)>,
}

impl Witness {
    fn observe(&mut self, read: Cadence, interval: (u64, u64)) {
        if !read.held || !read.enrolled || !read.current {
            return;
        }
        if let Some(due) = self.dirty_due
            && read.due_ns > due
        {
            self.rearmed_due = Some(read.due_ns);
            self.positive_intervals.push(interval);
        }
        if read.dirty && self.dirty_due.is_none() {
            self.dirty_due = Some(read.due_ns);
            self.positive_intervals.push(interval);
        }
    }

    fn complete(&self) -> bool {
        self.dirty_due
            .zip(self.rearmed_due)
            .is_some_and(|(a, b)| b > a)
    }
}

fn read(
    wire: &mut Wire,
    sid: &str,
    local: u64,
    label: &str,
    raw: &mut Raw,
) -> Result<(Cadence, (u64, u64)), String> {
    let begin = crate::sys::monotonic_ns();
    let reply = ask(wire, &format!("@{sid} metrics cadence"))?;
    let end = crate::sys::monotonic_ns();
    raw.note(
        label,
        format!("begin_monotonic_ns={begin} end_monotonic_ns={end}\n{reply}"),
    );
    Ok((Cadence::parse(&reply)?.consistent(local)?, (begin, end)))
}

fn idle(wire: &mut Wire, sid: &str, local: u64, label: &str, raw: &mut Raw) -> Result<(), String> {
    let start = crate::sys::monotonic_ns();
    for n in 0..3 {
        if n != 0 {
            nap(Duration::from_millis(500))?;
        }
        let (state, _) = read(wire, sid, local, &format!("{label}_{n}"), raw)?;
        if !state.idle() {
            return Err(format!("{label} retained an output hold: {state:?}"));
        }
    }
    raw.note(
        &format!("{label}_span"),
        format!(
            "begin_monotonic_ns={start} end_monotonic_ns={}",
            crate::sys::monotonic_ns()
        ),
    );
    Ok(())
}

fn wait_held(
    wire: &mut Wire,
    source: &Session,
    files: &producer::Files,
    raw: &mut Raw,
) -> Result<Witness, String> {
    let start = Instant::now();
    let mut witness = Witness::default();
    let mut ordinal = 0u64;
    loop {
        let (state, interval) = read(
            wire,
            &source.sid,
            source.local,
            &format!("stream_{ordinal}"),
            raw,
        )?;
        witness.observe(state, interval);
        if witness.complete() {
            return Ok(witness);
        }
        if files
            .dir
            .join("done.json")
            .try_exists()
            .map_err(|e| e.to_string())?
        {
            return Err(
                "natural output completed without a dirty indexed hold and a later rearm".into(),
            );
        }
        if start.elapsed() >= HANG {
            return Err("cadence observation hang detector".into());
        }
        ordinal += 1;
        nap(Duration::from_millis(1))?;
    }
}

fn wait_retired(wire: &mut Wire, sid: &str, local: u64, raw: &mut Raw) -> Result<(), String> {
    let start = Instant::now();
    let mut ordinal = 0u64;
    loop {
        if read(wire, sid, local, &format!("retiring_{ordinal}"), raw)?
            .0
            .idle()
        {
            return Ok(());
        }
        if start.elapsed() >= HANG {
            return Err("output-hold retirement hang detector".into());
        }
        ordinal += 1;
        nap(Duration::from_millis(1))?;
    }
}

fn run(o: &Options, raw: &mut Raw) -> Result<aterm_json::Value, String> {
    let work = Work {
        profile: "functional-cadence".into(),
        duration_ms: 2000,
        typing_ms: 1600,
        block_bytes: 64,
        output_ms: 1,
        key_ms: 40,
    };
    work.validate()?;
    let case = Case::Tabs(QUIET + 1);
    raw.note(
        "purpose",
        "functional_output_cadence; timing_qualified=false",
    );
    raw.note(
        "workload",
        format!("quiet_peers={QUIET} sessions={SESSIONS} work={work:?}"),
    );
    raw.note("functional_memory_bytes", o.budget);
    let controller = crate::paths::paths().exe.clone();
    let controller_digest = aterm_update_core::sha256_file(&controller)?;
    raw.note("controller_path", controller.display());
    raw.note("controller_sha256", &controller_digest);
    let (mut world, files, launch) = fixture_world(&o.build, &work, false, raw)?;
    let mut inst =
        match Instance::start_functional_in(&world, launch, o.budget, crate::band::Band::App) {
            Ok(inst) => inst,
            Err(why) => {
                world.keep();
                return Err(why);
            }
        };
    let fixture = (|| {
        inst.await_socket(instance::BOOT)
            .map_err(|e| e.msg.unwrap_or_else(|| "native app did not boot".into()))?;
        if inst.timing_admission().is_ok() {
            return Err("functional instance acquired Timing authority".into());
        }
        let (identity, _) = cpu(&inst, &o.build, None, raw, "identity_before")?;
        let mut prepared = prepare_fixture(&inst, &o.build, case, &work, &files, raw)?;
        let sid = prepared.t.source.sid.clone();
        let local = prepared.t.source.local;
        idle(&mut prepared.source, &sid, local, "idle_before", raw)?;
        let mut observed = None;
        let taken = exercise_fixture_with_input(
            &inst,
            &o.build,
            case,
            &work,
            &files,
            prepared,
            raw,
            |typing, source, targets, work, raw| {
                let first = work.keys() / 2;
                raw.note("native_post_first", post(typing, first, work.key_ms)?);
                observed = Some(wait_held(source, &targets.source, &files, raw)?);
                post(typing, work.keys() - first, work.key_ms)
            },
        )?;
        let witness = observed.ok_or("no native cadence witness")?;
        if !witness.complete() || taken.cast.truncated || taken.cast.bytes as u64 != taken.bytes {
            return Err("incomplete hold/rearm witness or truncated source output".into());
        }
        for (begin, end) in &witness.positive_intervals {
            enclosed(taken.first_output_ns, taken.last_output_ns, *begin, *end)?;
        }
        let mut source = bounded_wire(&inst)?;
        wait_retired(&mut source, &sid, local, raw)?;
        idle(&mut source, &sid, local, "idle_after", raw)?;
        cpu(&inst, &o.build, Some(identity), raw, "identity_after")?;
        if aterm_update_core::sha256_file(&controller)? != controller_digest {
            return Err("controller image changed during the fixture".into());
        }
        Ok(
            json!({"schema":1,"kind":"functional_output_cadence","timing_qualified":false,
            "quiet_peers":QUIET,"sessions":SESSIONS,"source_sid":sid,"source_local":local,
            "dirty_due_ns":witness.dirty_due,"rearmed_due_ns":witness.rearmed_due,
            "positive_intervals_monotonic_ns":witness.positive_intervals,
            "retained_corpus_bytes":taken.cast.bytes,"cast_truncated":taken.cast.truncated,
            "n_input":taken.metrics.n,"input_p50_ms":taken.metrics.p50,
            "input_p95_ms":taken.metrics.p95,"input_p99_ms":taken.metrics.p99,
            "controller_sha256":controller_digest,"app_sha256":o.build.digest,
            "declared_app_source":o.build.source}),
        )
    })();
    let result = finish(fixture, || inst.stop(), raw);
    if result.is_err() {
        world.keep();
    }
    result
}

pub(crate) fn cmd(args: &[String]) -> Res {
    if args.iter().any(|s| s == "--help" || s == "-h") {
        print!("{HELP}");
        return Ok(0);
    }
    let o = Options::parse(args)?;
    std::fs::create_dir(&o.out).map_err(|e| Fail::exit(3, format!("new output directory: {e}")))?;
    stop::install();
    let mut raw = Raw::default();
    let result = run(&o, &mut raw);
    raw.note("result", format!("{result:?}"));
    std::fs::write(o.out.join("observations.txt"), raw.as_text())
        .map_err(|e| Fail::exit(3, e.to_string()))?;
    if let Some(cast) = &raw.1 {
        std::fs::write(o.out.join("source.cast"), cast)
            .map_err(|e| Fail::exit(3, e.to_string()))?;
    }
    let document = match &result {
        Ok(evidence) => json!({"passed":true,"cleanup_complete":true,"evidence":evidence}),
        Err(why) => json!({"passed":false,"timing_qualified":false,"why":why}),
    };
    std::fs::write(o.out.join("result.json"), document.to_string())
        .map_err(|e| Fail::exit(3, e.to_string()))?;
    result.map_err(|why| Fail::exit(3, why))?;
    println!("{document}");
    Ok(0)
}

#[cfg(test)]
#[path = "output_cadence_native_tests.rs"]
mod tests;
