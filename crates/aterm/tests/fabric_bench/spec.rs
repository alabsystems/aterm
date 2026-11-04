// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What one run measures, read from `ENV`.

use std::path::PathBuf;

/// The variable that names the run.
pub const ENV: &str = "FABRIC_BENCH";

/// Maximum posts one sender session queues in a burst.
pub const PER_SENDER: usize = 100;

/// The largest burst a run takes.
pub const BURST_MAX: usize = 5_000;

/// The largest attach population on one headless instance.
pub const ATTACH_MAX: usize = 128;

/// One backlog filler record's bytes as astream's retention counts them
/// (`astream_broker::record_bytes`): the frame's 12-byte header, the
/// record's 34 fixed bytes, the 39-byte subject
/// `/f/f1/pub/n-<16 hex>/backlog/ev` and the 128-byte body. The bench's
/// backlog tests hold it to astream's count.
pub const FILL_RECORD_BYTES: usize = 213;

/// The bytes aterm's bulk `ev` rule keeps (`aterm_link::broker_rules::EV_BYTES`,
/// 256 MiB), which every population's backlog shares: each population fills
/// `backlog` records of its own, and a fill past this would be cut by the
/// broker's retention, which checks every 60 s from the open, inside the run.
pub const EV_RULE_BYTES: usize = 256 * 1024 * 1024;

/// The largest backlog a run of `populations` attach sizes takes: their fills,
/// [`FILL_RECORD_BYTES`] a record, inside [`EV_RULE_BYTES`] (420,086 records at
/// the default three sizes, 630,130 at two).
#[must_use]
pub const fn backlog_max(populations: usize) -> usize {
    EV_RULE_BYTES / (FILL_RECORD_BYTES * if populations == 0 { 1 } else { populations })
}

/// The most rows the ask world's asked table takes pre-filled: the table's
/// cap (`aterm_link::state::ASKED_KEEP`), which the ask world's tests hold it to.
pub const ASKED_PREFILL_MAX: usize = 1024;

/// The most paced notes a run takes: ten minutes of them at [`NOTE_PACE`].
pub const PACED_NOTES_MAX: usize = 545;

/// How far apart the paced notes go: past the mirror's one snapshot of a moved mail count a
/// second (`aterm_slog::MAIL_SNAP_GAP_MS`, 1,000 ms), so every delivery owes one.
pub const NOTE_PACE: std::time::Duration = std::time::Duration::from_millis(1_100);

/// One run's knobs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    /// Output directory for `fabric.json` and `fabric.md`.
    pub out: PathBuf,
    /// Scenario selection for the run.
    pub scenario: Scenario,
    /// Measured post-to-deliver round trips.
    pub samples: usize,
    /// Unmeasured warmup round trips.
    pub warmup: usize,
    /// Posts in the burst.
    pub burst: usize,
    /// Measured request/reply round trips in the ping-pong arm.
    pub pingpong: usize,
    /// Ascending attach populations.
    pub attach: Vec<usize>,
    /// Bridge restarts measured per population.
    pub rounds: usize,
    /// In-process broker durability tier.
    pub durability: Tier,
    /// Ordered broker flush horizon in milliseconds.
    pub horizon_ms: Option<u64>,
    /// Whether each world's broker retains its log by default rules.
    pub retention: bool,
    /// Whether each world's log is sealed at rest under a key, as `aterm link broker` opens a log
    /// with a `rest.key` beside it: on unless a run names it off, what a fabric root ships.
    pub at_rest: bool,
    /// Bridge `--log` mirror mode.
    pub log: Log,
    /// Idle bystander sessions spawned beside the talking sessions.
    pub bystanders: usize,
    /// Paced round trips in the ping-pong arm.
    pub paced: usize,
    /// Retaining broker tick interval in milliseconds.
    pub retention_interval_ms: Option<u64>,
    /// Measured posts per leg of the ask arm: each kind's idle releases, the
    /// busy ones, the receipts and the person-lane asks of each variant
    /// (without a pager and with one).
    pub asks: usize,
    /// The ask arm's escalated mix: one person-lane post before every
    /// `mix_every`th ordinary message.
    pub mix_every: usize,
    /// The attach world's backlog (B09): records filled on an `ev` face
    /// after every session's mail is seen, so each session's refill
    /// watermark trails the head by this many when the bridge restarts. 0
    /// (the default) is the attach world without it.
    pub backlog: usize,
    /// The ask world's asked table pre-filled to this many rows before its
    /// legs (C04): written as a build before the asks journal wrote the table,
    /// and taken in by a restarted bridge, as a node that upgraded with a full
    /// table holds it. 0 (the default) is none; at most [`ASKED_PREFILL_MAX`].
    pub asked_prefill: usize,
    /// The messaging world's paced notes (C13): one-way notes from A to B, [`NOTE_PACE`]
    /// apart, none answered, after the burst, so each delivery owes the bus mirror the
    /// session's snapshot, as an agent's delivery does. 0 (the default) is none; at most
    /// [`PACED_NOTES_MAX`].
    pub paced_notes: usize,
    /// Both tallies in the messaging, attach, handoff and ask worlds (D02): each boots with the
    /// bridge's wait tally (`aterm_link::wait_tally::SEAM`) and aterm's handler tally
    /// (`aterm_types::wait_tally::HANDLER_SEAM`) on, reads both at each round's end and reports
    /// every round's rows. Off (the default) boots those worlds as before, since the tally's own
    /// idle writes move what they measure; the ping-pong world counts with both always.
    pub tally: bool,
}

/// The scenarios measured in one run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Scenario {
    #[default]
    All,
    Messaging,
    Pingpong,
    Attach,
    Handoff,
    /// What agents and people wait on: an ask's release, a receipt, the
    /// person lane and its bystander cost, and a post across two nodes.
    Ask,
}

impl Scenario {
    /// The word the specification and report use.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Messaging => "messaging",
            Self::Pingpong => "pingpong",
            Self::Attach => "attach",
            Self::Handoff => "handoff",
            Self::Ask => "ask",
        }
    }

    /// Read a scenario name.
    ///
    /// # Errors
    /// A name outside the supported words.
    pub fn parse(word: &str) -> Result<Self, String> {
        match word {
            "all" => Ok(Self::All),
            "messaging" => Ok(Self::Messaging),
            "pingpong" => Ok(Self::Pingpong),
            "attach" => Ok(Self::Attach),
            "handoff" => Ok(Self::Handoff),
            "ask" => Ok(Self::Ask),
            _ => Err(format!(
                "{ENV}: `scenario=` is all, messaging, pingpong, attach, handoff or ask, not \
                 `{word}`"
            )),
        }
    }
}

/// The bridge's `--log` mode (`auto` or `off`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Log {
    #[default]
    Auto,
    Off,
}

impl Log {
    /// The word [`parse`] reads and the report prints.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Log::Auto => "auto",
            Log::Off => "off",
        }
    }

    /// Parse `auto` or `off`.
    ///
    /// # Errors
    /// Any other word.
    pub fn parse(word: &str) -> Result<Log, String> {
        match word {
            "auto" => Ok(Log::Auto),
            "off" => Ok(Log::Off),
            _ => Err(format!("{ENV}: `log=` is `auto` or `off`, not `{word}`")),
        }
    }
}

/// `on` or `off`, for the switch `key=`. Pure (unit-tested through [`parse`]).
fn on_off(key: &str, word: &str) -> Result<bool, String> {
    match word {
        "on" => Ok(true),
        "off" => Ok(false),
        _ => Err(format!("{ENV}: `{key}=` is `on` or `off`, not `{word}`")),
    }
}

/// Default ordered broker flush horizon in milliseconds.
pub const ORDERED_HORIZON_MS: u64 = 100;

/// The durability tier for the bench's in-process broker.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Tier {
    Strict,
    #[default]
    Ordered,
    Relaxed,
}

impl Tier {
    /// The word [`parse`] reads and the report prints.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Tier::Strict => "strict",
            Tier::Ordered => "ordered",
            Tier::Relaxed => "relaxed",
        }
    }

    /// Parse `strict`, `ordered` or `relaxed`.
    ///
    /// # Errors
    /// Any other word.
    pub fn parse(word: &str) -> Result<Tier, String> {
        match word {
            "strict" => Ok(Tier::Strict),
            "ordered" => Ok(Tier::Ordered),
            "relaxed" => Ok(Tier::Relaxed),
            _ => Err(format!(
                "{ENV}: `durability=` is `strict`, `ordered` or `relaxed`, not `{word}`"
            )),
        }
    }
}

/// Default sample count for `verify perf fabric`.
pub const DEFAULT_SAMPLES: usize = 200;
pub const DEFAULT_WARMUP: usize = 3;
pub const DEFAULT_BURST: usize = 500;
pub const DEFAULT_PINGPONG: usize = 200;
pub const DEFAULT_ATTACH: [usize; 3] = [1, 8, 32];
pub const DEFAULT_ROUNDS: usize = 3;
pub const DEFAULT_PACED: usize = 50;
pub const DEFAULT_ASKS: usize = 100;
pub const DEFAULT_MIX_EVERY: usize = 10;

impl Spec {
    /// The sender sessions the burst is split across.
    #[must_use]
    pub const fn burst_senders(&self) -> usize {
        self.burst.div_ceil(PER_SENDER)
    }

    /// Formats the spec as key=value words that [`parse`] reads.
    #[must_use]
    pub fn words(&self) -> String {
        let attach: Vec<String> = self.attach.iter().map(usize::to_string).collect();
        let mut words = format!(
            "samples={} warmup={} burst={} pingpong={} attach={} rounds={} bystanders={} paced={} \
             asks={} mix_every={} retention={} log={}",
            self.samples,
            self.warmup,
            self.burst,
            self.pingpong,
            attach.join(","),
            self.rounds,
            self.bystanders,
            self.paced,
            self.asks,
            self.mix_every,
            if self.retention { "on" } else { "off" },
            self.log.word()
        );
        if let Some(ms) = self.retention_interval_ms {
            words.push_str(&format!(" retention_interval_ms={ms}"));
        }
        // Always named: an entry from before the default changed names none and was plaintext.
        words.push_str(if self.at_rest {
            " at_rest=on"
        } else {
            " at_rest=off"
        });
        if self.backlog > 0 {
            words.push_str(&format!(" backlog={}", self.backlog));
        }
        if self.asked_prefill > 0 {
            words.push_str(&format!(" asked_prefill={}", self.asked_prefill));
        }
        if self.paced_notes > 0 {
            words.push_str(&format!(" paced_notes={}", self.paced_notes));
        }
        if self.tally {
            words.push_str(" tally=on");
        }
        if self.scenario != Scenario::All {
            words.push_str(&format!(" scenario={}", self.scenario.word()));
        }
        words.push_str(&format!(" durability={}", self.durability.word()));
        if let Some(ms) = self.horizon_ms {
            words.push_str(&format!(" horizon_ms={ms}"));
        }
        words
    }
}

/// Parse a `FABRIC_BENCH` specification string.
///
/// # Errors
/// A word or value it does not know, a count that is not a positive integer
/// (`warmup`, `bystanders`, `paced`, `backlog`, `paced_notes` and `horizon_ms` may be 0), a durability
/// that is not a [`Tier`], a horizon for a tier that is not ordered, a
/// retention interval on a broker that does not retain, a burst past
/// [`BURST_MAX`], an attach list that does not rise or passes [`ATTACH_MAX`],
/// a bystander count past [`ATTACH_MAX`], a backlog past [`backlog_max`] for
/// its attach sizes or on a run that measures no attach, an asked prefill past
/// [`ASKED_PREFILL_MAX`] or on a run that measures no ask, paced notes past
/// [`PACED_NOTES_MAX`] or on a run that measures no messaging, `tally=on` on a run that measures
/// only the ping-pong world (which counts with both tallies anyway), or no `out=`.
pub fn parse(raw: &str) -> Result<Spec, String> {
    let mut out = None;
    let mut spec = Spec {
        out: PathBuf::new(),
        scenario: Scenario::default(),
        samples: DEFAULT_SAMPLES,
        warmup: DEFAULT_WARMUP,
        burst: DEFAULT_BURST,
        pingpong: DEFAULT_PINGPONG,
        attach: DEFAULT_ATTACH.to_vec(),
        rounds: DEFAULT_ROUNDS,
        durability: Tier::default(),
        horizon_ms: None,
        retention: true,
        at_rest: true,
        log: Log::default(),
        retention_interval_ms: None,
        bystanders: 0,
        paced: DEFAULT_PACED,
        asks: DEFAULT_ASKS,
        mix_every: DEFAULT_MIX_EVERY,
        backlog: 0,
        asked_prefill: 0,
        paced_notes: 0,
        tally: false,
    };
    let count = |key: &str, v: &str, zero_ok: bool| -> Result<usize, String> {
        match v.parse::<usize>() {
            Ok(n) if n > 0 || zero_ok => Ok(n),
            _ => Err(format!(
                "{ENV}: `{key}=` takes a {} integer, not `{v}`",
                if zero_ok { "non-negative" } else { "positive" }
            )),
        }
    };
    for word in raw.split_whitespace() {
        let (key, v) = word
            .split_once('=')
            .ok_or_else(|| format!("{ENV}: `{word}` is not a key=value word"))?;
        match key {
            "out" if !v.is_empty() => out = Some(PathBuf::from(v)),
            "scenario" => spec.scenario = Scenario::parse(v)?,
            "samples" => spec.samples = count(key, v, false)?,
            "warmup" => spec.warmup = count(key, v, true)?,
            "burst" => spec.burst = count(key, v, false)?,
            "pingpong" => spec.pingpong = count(key, v, false)?,
            "rounds" => spec.rounds = count(key, v, false)?,
            "bystanders" => spec.bystanders = count(key, v, true)?,
            "paced" => spec.paced = count(key, v, true)?,
            "asks" => spec.asks = count(key, v, false)?,
            "mix_every" => spec.mix_every = count(key, v, false)?,
            "backlog" => spec.backlog = count(key, v, true)?,
            "asked_prefill" => spec.asked_prefill = count(key, v, true)?,
            "paced_notes" => spec.paced_notes = count(key, v, true)?,
            "durability" => spec.durability = Tier::parse(v)?,
            "retention" => spec.retention = on_off(key, v)?,
            "at_rest" => spec.at_rest = on_off(key, v)?,
            "tally" => spec.tally = on_off(key, v)?,
            "log" => spec.log = Log::parse(v)?,
            "retention_interval_ms" => {
                spec.retention_interval_ms = Some(count(key, v, false)? as u64);
            }
            "horizon_ms" => {
                spec.horizon_ms = Some(v.parse::<u64>().map_err(|_| {
                    format!("{ENV}: `horizon_ms=` takes a non-negative integer, not `{v}`")
                })?);
            }
            "attach" => {
                spec.attach = v
                    .split(',')
                    .map(|n| count(key, n, false))
                    .collect::<Result<_, _>>()?;
            }
            _ => return Err(format!("{ENV}: unknown word `{word}`")),
        }
    }
    spec.out = out.ok_or_else(|| format!("{ENV}: `out=<dir>` names where the report goes"))?;
    match (spec.durability, spec.horizon_ms) {
        (Tier::Ordered, None) => spec.horizon_ms = Some(ORDERED_HORIZON_MS),
        (Tier::Ordered, Some(_)) | (_, None) => {}
        (tier, Some(_)) => {
            return Err(format!(
                "{ENV}: horizon_ms= is the ordered tier's, and this run is {}",
                tier.word()
            ));
        }
    }
    if spec.retention_interval_ms.is_some() && !spec.retention {
        return Err(format!(
            "{ENV}: retention_interval_ms= is a retaining broker's tick, and this run is \
             retention=off"
        ));
    }
    if spec.burst > BURST_MAX {
        return Err(format!(
            "{ENV}: burst={} is past the {BURST_MAX} one instance's sender sessions carry",
            spec.burst
        ));
    }
    if !spec.attach.windows(2).all(|w| w[0] < w[1]) {
        return Err(format!(
            "{ENV}: attach= must rise (each population grows from the one before)"
        ));
    }
    if spec.attach.last().is_some_and(|&n| n > ATTACH_MAX) {
        return Err(format!("{ENV}: attach= tops out at {ATTACH_MAX} sessions"));
    }
    if spec.bystanders > ATTACH_MAX {
        return Err(format!(
            "{ENV}: bystanders= tops out at {ATTACH_MAX} sessions on one instance"
        ));
    }
    let most = backlog_max(spec.attach.len());
    if spec.backlog > most {
        return Err(format!(
            "{ENV}: backlog= tops out at {most} records at {} attach size(s): each fills its \
             own, {FILL_RECORD_BYTES} bytes a record, inside the ev rule's {EV_RULE_BYTES} \
             bytes, past which the broker's retention cuts the fill inside the run",
            spec.attach.len()
        ));
    }
    if spec.backlog > 0 && !matches!(spec.scenario, Scenario::All | Scenario::Attach) {
        return Err(format!(
            "{ENV}: backlog= is the attach world's, and this run measures {}",
            spec.scenario.word()
        ));
    }
    if spec.asked_prefill > ASKED_PREFILL_MAX {
        return Err(format!(
            "{ENV}: asked_prefill= tops out at the asked table's {ASKED_PREFILL_MAX} rows"
        ));
    }
    if spec.paced_notes > PACED_NOTES_MAX {
        return Err(format!(
            "{ENV}: paced_notes= tops out at {PACED_NOTES_MAX}, ten minutes of notes"
        ));
    }
    if spec.paced_notes > 0 && !matches!(spec.scenario, Scenario::All | Scenario::Messaging) {
        return Err(format!(
            "{ENV}: paced_notes= is the messaging world's, and this run measures {}",
            spec.scenario.word()
        ));
    }
    if spec.asked_prefill > 0 && !matches!(spec.scenario, Scenario::All | Scenario::Ask) {
        return Err(format!(
            "{ENV}: asked_prefill= is the ask world's, and this run measures {}",
            spec.scenario.word()
        ));
    }
    if spec.tally && spec.scenario == Scenario::Pingpong {
        return Err(format!(
            "{ENV}: tally=on is the messaging, attach, handoff and ask worlds' knob; the \
             ping-pong world counts with both tallies on in every run"
        ));
    }
    Ok(spec)
}

/// Parse [`Spec`] from [`ENV`].
///
/// # Errors
/// Missing, non-Unicode or malformed input.
pub fn from_env() -> Result<Spec, String> {
    let raw = std::env::var(ENV)
        .map_err(|e| format!("{ENV} is required (`verify perf fabric` sets it): {e}"))?;
    parse(&raw)
}
