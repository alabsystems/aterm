// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `start` lane.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::exec::check_command as command;
use aterm_json::{Map, Value, json};
use aterm_uds::spawnfd::Stdio;

use super::{
    Args, Halt, Scratch, await_socket, in_scratch_world, private, scratch_config, sha256_file,
    without_aterm_env,
};
use crate::perf::LaneRun;
use crate::perf::stats::{median, tail_summary};

/// Every launch's scratch `aterm.toml`: no persisted window, the audible bell
/// off (this window is never the owner's terminal, so a BEL its shell rings
/// must not play their alert sound; the key is read once with the rest and
/// costs the start nothing), and the scratch world's lanes off
/// ([`scratch_config`]): no update or package cadence, no keeper, no swarm.
pub(crate) fn config() -> String {
    scratch_config("restore_session = false\nbell_sound = false\n")
}

/// The grid every launch asks for, and checks it got.
pub(crate) const COLS: u64 = 110;
pub(crate) const ROWS: u64 = 32;

/// The metrics JSON rounds every phase and total to 0.01 ms, so a sum of eight
/// reconciles within 0.05 ms; this is that wire-rounding allowance, not a
/// timing tolerance.
const ROUNDING_MS: f64 = 0.06;

/// Phase schema 1: the eight exclusive phases of `rust_main_to_first_present`.
pub(crate) const PHASES: [&str; 8] = [
    "startup_router_ms",
    "startup_gui_prepare_ms",
    "startup_winit_dispatch_ms",
    "startup_initial_surface_attach_ms",
    "startup_surface_to_successful_redraw_ms",
    "startup_successful_compose_ms",
    "startup_successful_surface_transaction_ms",
    "startup_successful_finalize_ms",
];
const PHASE_ROUTER: usize = 0;
const PHASE_GUI_PREPARE: usize = 1;
const PHASE_ATTACH: usize = 3;

/// Prepare schema 1: the ten exclusive phases of `startup_gui_prepare_ms`.
pub(crate) const PREPARE: [&str; 10] = [
    "startup_prepare_boot_ms",
    "startup_prepare_warm_ms",
    "startup_prepare_event_loop_ms",
    "startup_prepare_services_ms",
    "startup_prepare_journal_ms",
    "startup_prepare_session0_ms",
    "startup_prepare_control_ms",
    "startup_prepare_config_join_ms",
    "startup_prepare_app_ms",
    "startup_prepare_arm_ms",
];

/// The kernel's birth record to Rust `main`: the loader's share, outside every
/// partition. `null` where the binary cannot read it.
const BIRTH: &str = "process_birth_to_rust_main_ms";

/// Attach schema 1: the eight exclusive subphases of the initial attach.
pub(crate) const ATTACH: [&str; 8] = [
    "startup_attach_dispatch_ms",
    "startup_attach_prepare_ms",
    "startup_attach_window_create_ms",
    "startup_attach_window_setup_ms",
    "startup_attach_backend_finalize_ms",
    "startup_attach_chrome_geometry_ms",
    "startup_attach_surface_create_ms",
    "startup_attach_finish_ms",
];
const ATTACH_BACKEND_FINALIZE: usize = 4;

/// Worker schema 1: the backend-build worker's wall time split at the join, then its own legs.
pub(crate) const WORKER: [&str; 9] = [
    "startup_worker_overlap_ms",
    "startup_worker_after_join_ms",
    "startup_worker_post_join_ms",
    "startup_worker_prelude_ms",
    "startup_worker_gpu_build_ms",
    "startup_worker_font_admit_ms",
    "startup_worker_font_apply_ms",
    "startup_worker_font_seal_ms",
    "startup_worker_epilogue_ms",
];
const WORKER_OVERLAP: usize = 0;
const WORKER_AFTER_JOIN: usize = 1;
const WORKER_POST_JOIN: usize = 2;
const WORKER_GPU_BUILD: usize = 4;

/// The GPU schema this lane reads. Schema 1 was wgpu's legs (instance,
/// adapter, device, pipelines), which no binary has filled since GW.6.
const GPU_SCHEMA: u64 = 2;
/// GPU schema 2: the renderer build's exclusive legs.
pub(crate) const GPU: [&str; 4] = [
    "startup_gpu_context_ms",
    "startup_gpu_font_join_ms",
    "startup_gpu_assemble_ms",
    "startup_gpu_tail_ms",
];
/// Reported beside the partition, never in it: the font thread overlaps GPU init.
pub(crate) const GPU_PARALLEL: [&str; 1] = ["startup_gpu_font_thread_ms"];

const DROP_REASONS: [&str; 10] = [
    "none",
    "gpu_reconfigured",
    "gpu_timeout",
    "gpu_occluded",
    "gpu_validation",
    "cpu_resize",
    "cpu_acquire",
    "cpu_commit",
    "target_mismatch",
    "virtual",
];

/// The renderer a launch asks for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum BackendRequest {
    #[default]
    Auto,
    Cpu,
    Gpu,
}

impl BackendRequest {
    pub(crate) const fn name(self) -> &'static str {
        match self {
            BackendRequest::Auto => "auto",
            BackendRequest::Cpu => "cpu",
            BackendRequest::Gpu => "gpu",
        }
    }
}

/// What one launch saw.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Observation {
    pub metrics: Value,
    pub external_ms: f64,
    pub thermal: String,
}

/// The backend-build worker drill-down.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Worker {
    pub total_ms: f64,
    pub legs: Vec<f64>,
}

/// The GPU renderer-build drill-down.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Gpu {
    pub legs: Vec<f64>,
    pub parallel: Vec<f64>,
}

/// One validated launch.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Sample {
    pub thermal: String,
    pub first_present_ms: f64,
    pub rust_main_ms: Option<f64>,
    pub visible_ms: Option<f64>,
    pub birth_ms: Option<f64>,
    pub phases: Option<Vec<f64>>,
    pub prepare: Option<Vec<f64>>,
    pub attach: Option<Vec<f64>>,
    pub worker: Option<Worker>,
    pub gpu: Option<Gpu>,
    pub external_ms: f64,
    pub frames: u64,
    pub backend: String,
    pub present_drops: u64,
    pub last_drop: String,
    pub last_drop_parked: bool,
}

/// Which optional schemas a sample carried; every sample of one arm must carry the same ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Shape {
    pub router: bool,
    pub visible: bool,
    pub phases: bool,
    pub attach: bool,
}

/// The laws a sample is held to that come from the run, not the sample.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Laws {
    pub settle_ms: u64,
    pub request: BackendRequest,
    pub allow_legacy: bool,
}

fn shown(v: Option<&Value>) -> String {
    v.map_or_else(
        || "absent".to_string(),
        |v| aterm_json::to_string(v).unwrap_or_default(),
    )
}

/// A finite number, if `key` is present; an error if it is present but not one.
fn optional_number(m: &Value, key: &str) -> Result<Option<f64>, String> {
    match m.get(key) {
        None => Ok(None),
        Some(v) => match v.as_f64().filter(|f| f.is_finite()) {
            Some(f) if !matches!(v, Value::Bool(_)) => Ok(Some(f)),
            _ => Err(format!("invalid sample: {key}={}", shown(Some(v)))),
        },
    }
}

fn required_number(m: &Value, key: &str) -> Result<f64, String> {
    optional_number(m, key)?.ok_or_else(|| format!("invalid sample: {key} is absent"))
}

/// Every key finite and non-negative, in order.
fn non_negative(m: &Value, keys: &[&str]) -> Result<Vec<f64>, String> {
    keys.iter()
        .map(|k| match optional_number(m, k)? {
            Some(v) if v >= 0.0 => Ok(v),
            _ => Err(format!(
                "invalid sample: {k}={}; expected finite >= 0",
                shown(m.get(k))
            )),
        })
        .collect()
}

/// A schema field: the integer 1 (never `true`, never `1.0`).
fn schema_one(m: &Value, key: &str) -> Result<(), String> {
    schema_is(m, key, 1)
}

/// A schema field: the integer `want` (never `true`, never a float).
fn schema_is(m: &Value, key: &str, want: u64) -> Result<(), String> {
    match m.get(key) {
        Some(v @ Value::Number(_)) if v.as_u64() == Some(want) => Ok(()),
        other => Err(format!(
            "invalid sample: {key}={}; expected {want}",
            shown(other)
        )),
    }
}

fn valid_true(m: &Value, key: &str) -> Result<(), String> {
    match m.get(key) {
        Some(Value::Bool(true)) => Ok(()),
        other => Err(format!(
            "invalid sample: {key}={}; expected true",
            shown(other)
        )),
    }
}

fn reconcile(what: &str, sum: f64, total: f64) -> Result<(), String> {
    if (sum - total).abs() > ROUNDING_MS {
        Err(format!(
            "{what} mismatch: sum={sum:.2}ms total={total:.2}ms"
        ))
    } else {
        Ok(())
    }
}

const LEGACY_HINT: &str = "pass --allow-legacy-metric only for an explicit old-binary baseline";

impl Sample {
    /// Validate one launch's metrics against every law the lane holds.
    pub(crate) fn from_observation(obs: &Observation, laws: &Laws) -> Result<Sample, String> {
        let m = &obs.metrics;
        if !m.is_object() {
            return Err("metrics --json was not one JSON object".into());
        }
        let frames = m
            .get("frames")
            .and_then(Value::as_u64)
            .filter(|f| *f >= 1)
            .ok_or_else(|| {
                format!(
                    "invalid sample: frames={}; first present was not observed (increase \
                     --settle-ms or reduce system load)",
                    shown(m.get("frames"))
                )
            })?;
        let first = required_number(m, "first_present_ms")?;
        if first <= 0.0 {
            return Err(format!("invalid sample: first_present_ms={first}"));
        }
        if first >= laws.settle_ms as f64 {
            return Err(format!(
                "invalid sample: first_present_ms={first:.2}ms is not safely below the {}ms \
                 settle; the one observer read may have raced startup (increase --settle-ms)",
                laws.settle_ms
            ));
        }
        let rust_main = optional_number(m, "rust_main_to_first_present_ms")?;
        match rust_main {
            Some(v) if v <= 0.0 => {
                return Err(format!("invalid sample: rust_main_to_first_present_ms={v}"));
            }
            None if !laws.allow_legacy => {
                return Err(format!(
                    "rust_main_to_first_present_ms is absent; {LEGACY_HINT}"
                ));
            }
            _ => {}
        }

        let phases = if m.get("startup_phase_schema").is_some() {
            schema_one(m, "startup_phase_schema")?;
            valid_true(m, "startup_phase_valid")?;
            let p = non_negative(m, &PHASES)?;
            let total = rust_main
                .ok_or("startup phases are present without rust_main_to_first_present_ms")?;
            let sum: f64 = p.iter().sum();
            reconcile("startup phase sum", sum, total)?;
            reconcile("startup GUI phase sum", sum - p[PHASE_ROUTER], first)?;
            Some(p)
        } else if laws.allow_legacy {
            None
        } else {
            return Err(format!("startup_phase_schema is absent; {LEGACY_HINT}"));
        };

        let prepare = if m.get("startup_prepare_schema").is_some()
            && m.get("startup_prepare_valid") == Some(&Value::Bool(true))
        {
            schema_one(m, "startup_prepare_schema")?;
            let p = non_negative(m, &PREPARE)?;
            if let Some(parent) = &phases {
                reconcile(
                    "startup prepare sum",
                    p.iter().sum(),
                    parent[PHASE_GUI_PREPARE],
                )?;
            }
            Some(p)
        } else {
            None
        };

        let attach = if m.get("startup_attach_schema").is_some() {
            let parent = phases
                .as_ref()
                .ok_or("startup attach drill-down is present without its parent phase schema")?
                [PHASE_ATTACH];
            schema_one(m, "startup_attach_schema")?;
            valid_true(m, "startup_attach_valid")?;
            let a = non_negative(m, &ATTACH)?;
            reconcile("startup attach sum", a.iter().sum(), parent)?;
            Some(a)
        } else if laws.allow_legacy {
            None
        } else {
            return Err(format!("startup_attach_schema is absent; {LEGACY_HINT}"));
        };

        let worker = if m.get("startup_worker_schema").is_some()
            && m.get("startup_worker_valid") == Some(&Value::Bool(true))
        {
            schema_one(m, "startup_worker_schema")?;
            let legs = non_negative(m, &WORKER)?;
            let total = non_negative(m, &["startup_worker_total_ms"])?[0];
            reconcile(
                "worker split (overlap + after_join)",
                legs[WORKER_OVERLAP] + legs[WORKER_AFTER_JOIN],
                total,
            )?;
            if let Some(a) = &attach {
                reconcile(
                    "worker join split (after_join + post_join)",
                    legs[WORKER_AFTER_JOIN] + legs[WORKER_POST_JOIN],
                    a[ATTACH_BACKEND_FINALIZE],
                )?;
            }
            Some(Worker {
                total_ms: total,
                legs,
            })
        } else {
            None
        };

        let gpu = if m.get("startup_gpu_schema").is_some()
            && m.get("startup_gpu_schema").and_then(Value::as_u64) != Some(1)
            && m.get("startup_gpu_valid") == Some(&Value::Bool(true))
        {
            schema_is(m, "startup_gpu_schema", GPU_SCHEMA)?;
            let legs = non_negative(m, &GPU)?;
            let parallel = non_negative(m, &GPU_PARALLEL)?;
            if let Some(w) = &worker {
                reconcile("gpu leg sum", legs.iter().sum(), w.legs[WORKER_GPU_BUILD])?;
            }
            Some(Gpu { legs, parallel })
        } else {
            None
        };

        let visible = optional_number(m, "rust_main_to_first_visible_ms")?;
        if let Some(v) = visible {
            if v <= 0.0 {
                return Err(format!(
                    "invalid sample: rust_main_to_first_visible_ms={v} (a window that never \
                     became visible cannot be timed)"
                ));
            }
            if v > obs.external_ms {
                return Err(format!(
                    "clock/order invariant failed: Rust-main first-visible {v:.2}ms exceeds the \
                     external observation {:.2}ms",
                    obs.external_ms
                ));
            }
        }

        let birth = match m.get(BIRTH) {
            None | Some(Value::Null) => None,
            Some(_) => optional_number(m, BIRTH)?,
        };
        if let Some(b) = birth {
            if b < 0.0 {
                return Err(format!("invalid sample: {BIRTH}={b}"));
            }
            if let Some(r) = rust_main
                && b + r > obs.external_ms
            {
                return Err(format!(
                    "clock/order invariant failed: birth->main {b:.2}ms plus Rust-main \
                     first-present {r:.2}ms exceeds the external observation {:.2}ms",
                    obs.external_ms
                ));
            }
        }

        let backend = match m.get("backend").and_then(Value::as_str) {
            Some(b @ ("cpu" | "gpu")) => b.to_string(),
            other => {
                return Err(format!(
                    "invalid sample: backend={}",
                    shown(other.map(|s| Value::String(s.into())).as_ref())
                ));
            }
        };
        for (key, want) in [("rows", ROWS), ("cols", COLS)] {
            if m.get(key).and_then(Value::as_u64) != Some(want) {
                return Err(format!(
                    "invalid sample: {key}={}; expected {want}",
                    shown(m.get(key))
                ));
            }
        }
        let present_drops = m
            .get("present_drops")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                format!(
                    "invalid sample: present_drops={}",
                    shown(m.get("present_drops"))
                )
            })?;
        let last_drop = m
            .get("last_present_drop_reason")
            .and_then(Value::as_str)
            .filter(|r| DROP_REASONS.contains(r))
            .ok_or_else(|| {
                format!(
                    "invalid sample: last_present_drop_reason={}",
                    shown(m.get("last_present_drop_reason"))
                )
            })?
            .to_string();
        let last_drop_parked = m
            .get("last_present_drop_parked")
            .and_then(Value::as_bool)
            .ok_or_else(|| {
                format!(
                    "invalid sample: last_present_drop_parked={}",
                    shown(m.get("last_present_drop_parked"))
                )
            })?;
        if present_drops == 0 && (last_drop != "none" || last_drop_parked) {
            return Err(format!(
                "drop-ledger invariant failed: zero drops carried a last disposition \
                 ({last_drop}, parked={last_drop_parked})"
            ));
        }
        if present_drops > 0 && last_drop == "none" {
            return Err("drop-ledger invariant failed: nonzero drops carried reason=none".into());
        }
        if laws.request != BackendRequest::Auto && backend != laws.request.name() {
            return Err(format!(
                "requested backend {}, negotiated {backend}",
                laws.request.name()
            ));
        }
        if first > obs.external_ms {
            return Err(format!(
                "clock/order invariant failed: internal first-present {first:.2}ms exceeds the \
                 external observation {:.2}ms",
                obs.external_ms
            ));
        }
        if let Some(r) = rust_main
            && (r < first || r > obs.external_ms)
        {
            return Err(format!(
                "clock/order invariant failed: Rust-main first-present {r:.2}ms is outside \
                 [GUI-entry {first:.2}ms, external {:.2}ms]",
                obs.external_ms
            ));
        }
        Ok(Sample {
            thermal: obs.thermal.clone(),
            first_present_ms: first,
            rust_main_ms: rust_main,
            visible_ms: visible,
            birth_ms: birth,
            phases,
            prepare,
            attach,
            worker,
            gpu,
            external_ms: obs.external_ms,
            frames,
            backend,
            present_drops,
            last_drop,
            last_drop_parked,
        })
    }

    pub(crate) const fn shape(&self) -> Shape {
        Shape {
            router: self.rust_main_ms.is_some(),
            visible: self.visible_ms.is_some(),
            phases: self.phases.is_some(),
            attach: self.attach.is_some(),
        }
    }

    /// The raw record a JSON result carries for this launch.
    pub(crate) fn to_json(&self) -> Value {
        let mut o = Map::new();
        let mut put = |k: &str, v: Value| {
            o.insert(k.to_string(), v);
        };
        put("thermal", json!(self.thermal));
        put("internal_first_present_ms", json!(self.first_present_ms));
        put("rust_main_to_first_present_ms", json!(self.rust_main_ms));
        put("rust_main_to_first_visible_ms", json!(self.visible_ms));
        put(BIRTH, json!(self.birth_ms));
        put(
            "startup_phase_schema",
            json!(self.phases.as_ref().map(|_| 1)),
        );
        for (i, k) in PHASES.iter().enumerate() {
            put(k, json!(self.phases.as_ref().map(|p| p[i])));
        }
        put(
            "startup_prepare_schema",
            json!(self.prepare.as_ref().map(|_| 1)),
        );
        for (i, k) in PREPARE.iter().enumerate() {
            put(k, json!(self.prepare.as_ref().map(|p| p[i])));
        }
        put(
            "startup_attach_schema",
            json!(self.attach.as_ref().map(|_| 1)),
        );
        for (i, k) in ATTACH.iter().enumerate() {
            put(k, json!(self.attach.as_ref().map(|a| a[i])));
        }
        put(
            "startup_worker_total_ms",
            json!(self.worker.as_ref().map(|w| w.total_ms)),
        );
        for (i, k) in WORKER.iter().enumerate() {
            put(k, json!(self.worker.as_ref().map(|w| w.legs[i])));
        }
        let gpu = self.gpu.as_ref();
        for (names, pick) in [
            (&GPU[..], (|g: &Gpu| g.legs.clone()) as fn(&Gpu) -> Vec<f64>),
            (&GPU_PARALLEL[..], |g: &Gpu| g.parallel.clone()),
        ] {
            for (i, k) in names.iter().enumerate() {
                put(k, json!(gpu.map(|g| pick(g)[i])));
            }
        }
        put("external_observation_ms", json!(self.external_ms));
        put("frames", json!(self.frames));
        put("backend", json!(self.backend));
        put("rows", json!(ROWS));
        put("cols", json!(COLS));
        put("present_drops", json!(self.present_drops));
        put("last_present_drop_reason", json!(self.last_drop));
        put("last_present_drop_parked", json!(self.last_drop_parked));
        Value::Object(o)
    }
}

/// Every sample of one run carries one shape and one backend.
#[derive(Debug, Default)]
pub(crate) struct Consistency {
    shape: Option<Shape>,
    backend: Option<String>,
}

impl Consistency {
    pub(crate) fn check(&mut self, s: &Sample) -> Result<(), String> {
        let shape = s.shape();
        match self.shape {
            Some(prev) if prev != shape => {
                return Err(format!(
                    "mixed startup-metric schemas across samples: {prev:?} then {shape:?}"
                ));
            }
            _ => self.shape = Some(shape),
        }
        match &self.backend {
            Some(prev) if *prev != s.backend => Err(format!(
                "mixed CPU/GPU samples cannot share one distribution: {prev} then {}",
                s.backend
            )),
            _ => {
                self.backend = Some(s.backend.clone());
                Ok(())
            }
        }
    }
}

/// How the lane launches one aterm.
#[derive(Debug, Clone)]
pub(crate) struct Launcher {
    pub binary: PathBuf,
    pub backend: BackendRequest,
    pub settle: Duration,
    pub timeout: Duration,
}

/// The persisted cell box a primer must leave for the launch after it to be
/// warm.
fn cell_metrics_cache(home: &Path) -> PathBuf {
    home.join("Library/Application Support/aterm/cell-metrics.toml")
}

impl Launcher {
    /// One launch into `root`'s private HOME/XDG tree, its control socket at `sock`.
    pub(crate) fn launch(
        &self,
        root: &Path,
        sock: &Path,
        primer: bool,
    ) -> Result<Option<Observation>, String> {
        let home = root.join("home");
        let dirs = [
            ("XDG_CONFIG_HOME", root.join("config")),
            ("XDG_RUNTIME_DIR", root.join("runtime")),
            ("XDG_CACHE_HOME", root.join("cache")),
            ("XDG_STATE_HOME", root.join("state")),
            ("XDG_DATA_HOME", root.join("data")),
            ("TMPDIR", root.join("tmp")),
        ];
        for d in std::iter::once(&home)
            .chain(dirs.iter().map(|(_, d)| d))
            .chain(std::iter::once(&root.join("config/aterm")))
        {
            std::fs::create_dir_all(d)
                .map_err(|e| format!("could not create {}: {e}", d.display()))?;
            private(d)?;
        }
        crate::write_fresh(root.join("config/aterm/aterm.toml"), config())
            .map_err(|e| format!("could not write the config: {e}"))?;
        let _ = std::fs::remove_file(sock);
        let log_path = root.join("aterm.log");
        let log = crate::create_stdio_file(&log_path)
            .map_err(|e| format!("could not create {}: {e}", log_path.display()))?;
        let log_err = log
            .try_clone()
            .map_err(|e| format!("could not dup the log: {e}"))?;
        let thermal = super::thermal();

        let mut cmd = command(&self.binary);
        in_scratch_world(&mut cmd, &home)?;
        cmd.env("HOME", &home);
        for (k, d) in &dirs {
            cmd.env(k, d);
        }
        cmd.arg("--window").arg("--control-sock").arg(sock).args([
            "--columns",
            &COLS.to_string(),
            "--lines",
            &ROWS.to_string(),
        ]);
        match self.backend {
            BackendRequest::Cpu => {
                cmd.arg("--cpu");
            }
            BackendRequest::Gpu => {
                cmd.arg("--gpu");
            }
            BackendRequest::Auto => {}
        }
        cmd.args(["-e", "/bin/sleep", "600"])
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(log_err);

        let result = (|| {
            let started = Instant::now();
            let mut group = super::Group::spawn(&mut cmd)?;
            await_socket(&mut group, sock, self.timeout)?;
            std::thread::sleep(self.settle);
            let mut query = super::ctl(&self.binary, sock);
            without_aterm_env(&mut query).env("HOME", &home).args([
                "--timeout",
                &self.timeout.as_secs().to_string(),
                "metrics",
                "--json",
            ]);
            let out =
                aterm_uds::deadline::output_until(&mut query, Instant::now() + self.timeout + HUNG)
                    .map_err(|e| format!("the metrics query: {e}"))?;
            let external_ms = started.elapsed().as_secs_f64() * 1000.0;
            group.retire()?;
            if !out.status.success() {
                return Err(format!(
                    "metrics query failed ({}): {}",
                    out.status,
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            let metrics: Value = aterm_json::from_slice(trim_ascii(&out.stdout)).map_err(|e| {
                format!(
                    "metrics --json was not one JSON object ({e}): {:?}",
                    String::from_utf8_lossy(&out.stdout)
                )
            })?;
            Ok(Observation {
                metrics,
                external_ms,
                thermal,
            })
        })();
        let obs =
            result.map_err(|e| format!("{e}\n--- aterm log tail ---\n{}", tail(&log_path)))?;
        if primer {
            let cache = cell_metrics_cache(&home);
            if !cache.is_file() {
                return Err(format!(
                    "the priming launch left no cell-metrics cache at {}; the launches after it \
                     would be cold, not warm",
                    cache.display()
                ));
            }
            return Ok(None);
        }
        Ok(Some(obs))
    }

    /// One independent trial in a fresh `root`.
    pub(crate) fn trial(
        &self,
        root: &Path,
        sock: &Path,
        warm: bool,
    ) -> Result<Observation, String> {
        if warm {
            self.launch(root, sock, true)?;
        }
        self.launch(root, sock, false)?
            .ok_or_else(|| "internal: a measured launch returned no observation".to_string())
    }
}

const fn trim_ascii(b: &[u8]) -> &[u8] {
    b.trim_ascii()
}

fn tail(path: &Path) -> String {
    let text = crate::read_regular_text(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(30)..].join("\n")
}

/// An artifact copied to an immutable path before trial 1.
#[derive(Debug, Clone)]
pub(crate) struct Frozen {
    pub source: PathBuf,
    pub frozen: PathBuf,
    pub sha256: String,
    pub version: String,
}

/// How long a child may run past its own budget before it is called hung.
const HUNG: Duration = Duration::from_secs(60);

/// How long a new copy's first run may take: the system assesses a new file at
/// its first exec, which a loaded host has held for minutes (a fresh 167 MB
/// debug build took 64 s, `aterm_spec::wait::FIRST_EXEC`). A hang detector of
/// its own, never a budget.
const FIRST_EXEC: Duration = Duration::from_secs(10 * 60);

/// `aterm --version`'s first line, which must name aterm.
pub(crate) fn identity(path: &Path) -> Result<String, String> {
    identity_within(path, HUNG)
}

/// [`identity`], its run called hung after `hang`.
fn identity_within(path: &Path, hang: Duration) -> Result<String, String> {
    let out =
        aterm_uds::deadline::output_until(command(path).arg("--version"), Instant::now() + hang)
            .map_err(|e| format!("{} --version: {e}", path.display()))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let first = text.trim().lines().next().unwrap_or_default().to_string();
    if !out.status.success() || !first.starts_with("aterm ") {
        return Err(format!(
            "refusing non-aterm executable {}: {first:?}",
            path.display()
        ));
    }
    Ok(first)
}

/// Freeze `source` into `dir/<label>-aterm` (`dir` a [`super::Scratch`]'s).
pub(crate) fn freeze(label: &str, source: &Path, dir: &Path) -> Result<Frozen, String> {
    let source = source
        .canonicalize()
        .map_err(|e| format!("{label}: {}: {e}", source.display()))?;
    if source.to_string_lossy().contains(".app/Contents/MacOS/") {
        return Err(format!(
            "{label}: an installed .app binary must be measured in its bundle layout; this lane \
             stages an unbundled target artifact"
        ));
    }
    let before = sha256_file(&source)?;
    let version = identity(&source)?;
    let frozen = dir.join(format!("{label}-aterm"));
    crate::copy_regular_fresh(&source, &frozen)
        .map_err(|e| format!("{label}: could not stage: {e}"))?;
    let after = sha256_file(&source)?;
    let staged = sha256_file(&frozen)?;
    if before != after || before != staged {
        return Err(format!("{label}: the source changed while it was staged"));
    }
    super::live::starting_from(dir);
    let staged_identity = identity_within(&frozen, FIRST_EXEC);
    if staged_identity.is_ok() {
        super::live::ran_from(dir);
    }
    if staged_identity? != version {
        return Err(format!("{label}: the staged executable's identity changed"));
    }
    Ok(Frozen {
        source,
        frozen,
        sha256: staged,
        version,
    })
}

/// Without `--binary`: the profiling build when present, else the release
/// build, wherever `targo` puts the target directory.
fn default_binary() -> Result<PathBuf, String> {
    let target = super::target_dir()?;
    let profiling = target.join("profiling/aterm");
    Ok(if profiling.is_file() {
        profiling
    } else {
        target.join("release/aterm")
    })
}

/// Write `value` to the person's `path` (`--json-out`) by [`super::super::publication::publish_named`]:
/// a file, or a link to one, is replaced whole; a FIFO or `/dev/stdout` is written where it is.
pub(crate) fn write_json_atomic(path: &Path, value: &Value) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    let mut text = aterm_json::to_string(value).map_err(|e| format!("JSON: {e}"))?;
    text.push('\n');
    // The person's `--json-out`: a FIFO or `/dev/stdout` is written where it is, a file (or a
    // link to one) is replaced whole.
    super::super::publication::publish_named(path, &text)
        .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    eprintln!("json_result: {}", path.display());
    Ok(())
}

/// The `start` lane.
pub(crate) fn run(args: &Args) -> LaneRun {
    super::outcome(run_inner(args))
}

/// Where the numbers went, for the verdict.
pub(crate) fn reported(json_out: Option<&Path>) -> String {
    match json_out {
        Some(p) => format!("summary above; JSON at {}", p.display()),
        None => "summary above".into(),
    }
}

fn run_inner(args: &Args) -> Result<String, Halt> {
    let samples = args.samples.unwrap_or(7);
    let settle_ms = args.settle_ms.unwrap_or(1500);
    let timeout = Duration::from_secs(args.timeout_s.unwrap_or(15));
    let run_root = Scratch::new("arena-start")?;
    let sockets = Scratch::sockets()?;
    let source = match &args.binary {
        Some(b) => b.clone(),
        None => default_binary()?,
    };
    let artifact = freeze("start", &source, run_root.path())?;
    let chip = super::chip();
    let laws = Laws {
        settle_ms,
        request: args.backend,
        allow_legacy: args.allow_legacy_metric,
    };
    let launcher = Launcher {
        binary: artifact.frozen.clone(),
        backend: args.backend,
        settle: Duration::from_millis(settle_ms),
        timeout,
    };
    eprintln!("== ARENA-START: fresh process, real window ==");
    eprintln!("binary: {}", artifact.source.display());
    eprintln!("version: {}", artifact.version);
    eprintln!("sha256: {}", artifact.sha256);
    eprintln!("backend request: {}", args.backend.name());
    eprintln!("samples: {samples}");
    eprintln!(
        "isolation: {}",
        if args.warm {
            "WARM — one shared HOME/XDG root, primed by a discarded launch"
        } else {
            "fresh HOME/XDG root and private socket per sample"
        }
    );
    eprintln!(
        "workload: {COLS}x{ROWS} grid; child=/bin/sleep 600; observer: socket wait, \
         {settle_ms}ms settle, ONE metrics --json read"
    );
    eprintln!(
        "run class: {}",
        if super::rule_zero_hardware(&chip) {
            format!(
                "rule-zero hardware eligible ('{chip}'); manual display/focus conditions still apply"
            )
        } else {
            format!("DEV-SMOKE, NON-PUBLISHABLE ('{chip}')")
        }
    );

    let warm_root = run_root.path().join("warm");
    let warm_sock = sockets.sock("warm.sock")?;
    if args.warm {
        launcher.launch(&warm_root, &warm_sock, true)?;
        eprintln!("sample --: priming launch complete (discarded); persisted cell box present");
    }
    let mut consistency = Consistency::default();
    let mut taken: Vec<Sample> = Vec::new();
    for i in 1..=samples {
        let (root, sock) = if args.warm {
            (warm_root.clone(), warm_sock.clone())
        } else {
            (
                run_root.path().join(format!("sample-{i}")),
                sockets.sock(&format!("sample-{i}.sock"))?,
            )
        };
        let obs = launcher
            .launch(&root, &sock, false)?
            .ok_or("internal: a measured launch returned no observation")?;
        let s = Sample::from_observation(&obs, &laws)
            .map_err(|e| format!("sample {i:02}: INVALID: {e}"))?;
        consistency.check(&s)?;
        eprintln!(
            "sample {i:02}: internal_first_present_ms={:.2} rust_main_to_first_present_ms={} \
             rust_main_to_first_visible_ms={} external_observation_ms={:.2} frames={} backend={} \
             present_drops={} last_drop={} parked={} thermal={}",
            s.first_present_ms,
            opt_ms(s.rust_main_ms),
            opt_ms(s.visible_ms),
            s.external_ms,
            s.frames,
            s.backend,
            s.present_drops,
            s.last_drop,
            s.last_drop_parked,
            s.thermal
        );
        taken.push(s);
    }
    print_summary(&taken);
    if let Some(path) = &args.json_out {
        write_json_atomic(path, &start_json(&artifact, &chip, args, &taken))?;
    }
    Ok(reported(args.json_out.as_deref()))
}

fn opt_ms(v: Option<f64>) -> String {
    v.map_or_else(|| "unavailable".into(), |v| format!("{v:.2}"))
}

/// The values of one metric across samples, when every sample carries it.
fn column(samples: &[Sample], pick: impl Fn(&Sample) -> Option<f64>) -> Option<Vec<f64>> {
    samples.iter().map(pick).collect()
}

fn print_summary(samples: &[Sample]) {
    eprintln!("== summary ==");
    let line = |name: &str, values: Option<Vec<f64>>| match values {
        Some(v) if !v.is_empty() => {
            eprintln!("{name}: median={:.2} {}", median(&v), tail_summary(&v));
        }
        _ => eprintln!("{name}: unavailable (pre-field binary)"),
    };
    line(
        "internal_first_present_ms",
        column(samples, |s| Some(s.first_present_ms)),
    );
    line(
        "rust_main_to_first_present_ms",
        column(samples, |s| s.rust_main_ms),
    );
    line(
        "rust_main_to_first_visible_ms",
        column(samples, |s| s.visible_ms),
    );
    line(BIRTH, column(samples, |s| s.birth_ms));
    let ranked = |title: &str, cols: Option<Vec<(String, Vec<f64>)>>, missing: &str| {
        let Some(mut cols) = cols else {
            eprintln!("{title}: unavailable ({missing})");
            return;
        };
        eprintln!("{title}:");
        cols.sort_by(|a, b| median(&b.1).total_cmp(&median(&a.1)));
        for (n, v) in cols {
            eprintln!("  {n}: median={:.2} {}", median(&v), tail_summary(&v));
        }
    };
    ranked(
        "startup phases (schema 1, exclusive)",
        named(samples, &PHASES, |s| s.phases.clone()),
        "pre-schema binary",
    );
    ranked(
        "gui preparation drill-down (schema 1, exclusive)",
        named(samples, &PREPARE, |s| s.prepare.clone()),
        "pre-schema binary, or a launch whose split the binary marked invalid",
    );
    ranked(
        "startup attach drill-down (schema 1, exclusive)",
        named(samples, &ATTACH, |s| s.attach.clone()),
        "pre-schema binary",
    );
    match column(samples, |s| s.worker.as_ref().map(|w| w.total_ms)) {
        Some(total) if !total.is_empty() => {
            eprintln!(
                "backend-build worker (schema 1, exclusive) — worker wall time median={:.2} {}",
                median(&total),
                tail_summary(&total)
            );
            ranked(
                "  split at the join, then the worker's own legs",
                named(samples, &WORKER, |s| {
                    s.worker.as_ref().map(|w| w.legs.clone())
                }),
                "",
            );
        }
        _ => eprintln!("backend-build worker: unavailable (pre-schema binary)"),
    }
    if samples.iter().all(|s| s.gpu.is_some()) && !samples.is_empty() {
        let gpu = |pick: fn(&Gpu) -> Vec<f64>| move |s: &Sample| s.gpu.as_ref().map(pick);
        ranked(
            "gpu renderer build (schema 2, exclusive)",
            named(samples, &GPU, gpu(|g| g.legs.clone())),
            "",
        );
        ranked(
            "  PARALLEL (overlaps the legs above, not part of the sum)",
            named(samples, &GPU_PARALLEL, gpu(|g| g.parallel.clone())),
            "",
        );
    } else {
        eprintln!("gpu renderer build: unavailable (pre-schema or CPU backend)");
    }
    line(
        "external_observation_ms",
        column(samples, |s| Some(s.external_ms)),
    );
    eprintln!(
        "present_drop_observations: {}",
        samples
            .iter()
            .map(|s| format!(
                "{}:{}:parked={}",
                s.present_drops, s.last_drop, s.last_drop_parked
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );
    eprintln!(
        "scope: internal is GUI-entry -> the startup-metrics publication in the first \
         successful-present finalizer; rust_main adds router time; both exclude dyld, which \
         birth->main measures from the kernel's birth record; external is an observer upper \
         bound; compositor/scanout/photons are unobserved"
    );
}

/// Each of `names` with its values across samples, when every sample carries
/// the group `pick` reads (in `names` order).
fn named(
    samples: &[Sample],
    names: &[&str],
    pick: impl Fn(&Sample) -> Option<Vec<f64>>,
) -> Option<Vec<(String, Vec<f64>)>> {
    let rows: Vec<Vec<f64>> = samples.iter().map(pick).collect::<Option<_>>()?;
    if rows.is_empty() {
        return None;
    }
    Some(
        names
            .iter()
            .enumerate()
            .map(|(i, n)| ((*n).to_string(), rows.iter().map(|r| r[i]).collect()))
            .collect(),
    )
}

fn summary_of(samples: &[Sample], pick: impl Fn(&Sample) -> Option<f64>) -> Value {
    column(samples, pick).map_or(Value::Null, |v| crate::perf::stats::distribution(&v))
}

/// A summary object, one distribution per name; null when absent.
fn distributions(cols: Option<Vec<(String, Vec<f64>)>>) -> Value {
    cols.filter(|c| !c.is_empty()).map_or(Value::Null, |cols| {
        Value::Object(
            cols.into_iter()
                .map(|(n, v)| (n, crate::perf::stats::distribution(&v)))
                .collect(),
        )
    })
}

fn start_json(artifact: &Frozen, chip: &str, args: &Args, samples: &[Sample]) -> Value {
    let gpu = |pick: fn(&Gpu) -> Vec<f64>| move |s: &Sample| s.gpu.as_ref().map(pick);
    let gpu_groups = [
        named(samples, &GPU, gpu(|g| g.legs.clone())),
        named(samples, &GPU_PARALLEL, gpu(|g| g.parallel.clone())),
    ]
    .into_iter()
    .collect::<Option<Vec<_>>>()
    .map(|groups| groups.concat());
    json!({
        "schema": "aterm-start-v3",
        "binary": artifact.source.display().to_string(),
        "version": artifact.version,
        "sha256": artifact.sha256,
        "chip": chip,
        "rule_zero_hardware_eligible": super::rule_zero_hardware(chip),
        "backend_request": args.backend.name(),
        "actual_backend": samples.first().map(|s| s.backend.clone()),
        "warm": args.warm,
        "samples": samples.iter().map(Sample::to_json).collect::<Vec<_>>(),
        "summary": {
            "internal_first_present_ms": summary_of(samples, |s| Some(s.first_present_ms)),
            "rust_main_to_first_present_ms": summary_of(samples, |s| s.rust_main_ms),
            "rust_main_to_first_visible_ms": summary_of(samples, |s| s.visible_ms),
            "process_birth_to_rust_main_ms": summary_of(samples, |s| s.birth_ms),
            "startup_phases_ms": distributions(named(samples, &PHASES, |s| s.phases.clone())),
            "startup_prepare_phases_ms":
                distributions(named(samples, &PREPARE, |s| s.prepare.clone())),
            "startup_attach_phases_ms":
                distributions(named(samples, &ATTACH, |s| s.attach.clone())),
            "startup_worker_total_ms":
                summary_of(samples, |s| s.worker.as_ref().map(|w| w.total_ms)),
            "startup_worker_ms": distributions(named(samples, &WORKER, |s| {
                s.worker.as_ref().map(|w| w.legs.clone())
            })),
            "startup_gpu_ms": distributions(gpu_groups),
            "external_observation_ms": summary_of(samples, |s| Some(s.external_ms)),
        },
    })
}

#[cfg(test)]
#[path = "start_tests.rs"]
pub(crate) mod tests;
