// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `start` lane.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_json::{Map, Value, json};
use aterm_uds::spawnfd::{Stdio, command};

use super::{Args, Halt, Scratch, await_socket, private, sha256_file, without_aterm_env};
use crate::perf::LaneRun;
use crate::perf::stats::{median, tail_summary};

/// sound; the key is read once with the rest and costs the start nothing).
const CONFIG: &str = "restore_session = false\nbell_sound = false\n\n\
                      [update]\nenabled = false\nauto_apply = false\n\
                      [packages]\nenabled = false\n";

/// The grid every launch asks for, and checks it got.
pub(crate) const COLS: u64 = 110;
pub(crate) const ROWS: u64 = 32;

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

/// GPU schema 1: the renderer build's exclusive legs.
pub(crate) const GPU: [&str; 7] = [
    "startup_gpu_instance_ms",
    "startup_gpu_adapter_ms",
    "startup_gpu_device_ms",
    "startup_gpu_context_tail_ms",
    "startup_gpu_font_join_ms",
    "startup_gpu_pipelines_ms",
    "startup_gpu_tail_ms",
];
const GPU_PIPELINES: usize = 5;
/// Reported beside the partition, never in it: the font thread overlaps GPU init.
pub(crate) const GPU_PARALLEL: [&str; 1] = ["startup_gpu_font_thread_ms"];
pub(crate) const GPU_PIPES: [&str; 8] = [
    "startup_gpu_pipe_shader_ms",
    "startup_gpu_pipe_uniform_atlas_ms",
    "startup_gpu_pipe_cell_ms",
    "startup_gpu_pipe_blit_ms",
    "startup_gpu_pipe_tray_ms",
    "startup_gpu_pipe_bloom_ms",
    "startup_gpu_pipe_vbuf_ms",
    "startup_gpu_pipe_tail_ms",
];

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
    pub(crate) fn name(self) -> &'static str {
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
    pub pipes: Vec<f64>,
    pub cells: Vec<(String, f64)>,
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
    match m.get(key) {
        Some(v @ Value::Number(_)) if v.as_u64() == Some(1) => Ok(()),
        other => Err(format!(
            "invalid sample: {key}={}; expected 1",
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
            && m.get("startup_gpu_valid") == Some(&Value::Bool(true))
        {
            schema_one(m, "startup_gpu_schema")?;
            let legs = non_negative(m, &GPU)?;
            let parallel = non_negative(m, &GPU_PARALLEL)?;
            let pipes = non_negative(m, &GPU_PIPES)?;
            if let Some(w) = &worker {
                reconcile("gpu leg sum", legs.iter().sum(), w.legs[WORKER_GPU_BUILD])?;
            }
            let pipe_sum: f64 = pipes.iter().sum();
            if pipe_sum - legs[GPU_PIPELINES] > ROUNDING_MS {
                return Err(format!(
                    "gpu pipeline group sum {pipe_sum:.2}ms exceeds its parent {:.2}ms",
                    legs[GPU_PIPELINES]
                ));
            }
            let cells: Vec<(String, f64)> = m
                .get("startup_gpu_cell_pipeline_ms")
                .and_then(Value::as_object)
                .filter(|o| !o.is_empty())
                .ok_or_else(|| {
                    format!(
                        "invalid sample: startup_gpu_cell_pipeline_ms={}; expected a non-empty \
                         object",
                        shown(m.get("startup_gpu_cell_pipeline_ms"))
                    )
                })?
                .iter()
                .map(|(k, v)| {
                    v.as_f64()
                        .filter(|f| f.is_finite() && *f >= 0.0)
                        .map(|f| (k.clone(), f))
                        .ok_or_else(|| {
                            format!("invalid sample: cell pipeline {k}={}", shown(Some(v)))
                        })
                })
                .collect::<Result<_, _>>()?;
            Some(Gpu {
                legs,
                parallel,
                pipes,
                cells,
            })
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

    pub(crate) fn shape(&self) -> Shape {
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
            (&GPU_PIPES[..], |g: &Gpu| g.pipes.clone()),
        ] {
            for (i, k) in names.iter().enumerate() {
                put(k, json!(gpu.map(|g| pick(g)[i])));
            }
        }
        put(
            "startup_gpu_cell_pipeline_ms",
            gpu.map_or(Value::Null, |g| {
                Value::Object(g.cells.iter().map(|(k, v)| (k.clone(), json!(v))).collect())
            }),
        );
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
        std::fs::write(root.join("config/aterm/aterm.toml"), CONFIG)
            .map_err(|e| format!("could not write the config: {e}"))?;
        let _ = std::fs::remove_file(sock);
        let log_path = root.join("aterm.log");
        let log = std::fs::File::create(&log_path)
            .map_err(|e| format!("could not create {}: {e}", log_path.display()))?;
        let log_err = log
            .try_clone()
            .map_err(|e| format!("could not dup the log: {e}"))?;
        let thermal = super::thermal();

        let mut cmd = command(&self.binary);
        without_aterm_env(&mut cmd);
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

fn trim_ascii(b: &[u8]) -> &[u8] {
    b.trim_ascii()
}

fn tail(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
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
    std::fs::copy(&source, &frozen).map_err(|e| format!("{label}: could not stage: {e}"))?;
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

/// Write `value` to `path` atomically (a temp file in the same directory, then a rename).
pub(crate) fn write_json_atomic(path: &Path, value: &Value) -> Result<(), String> {
    let parent = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    let tmp = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default(),
        std::process::id()
    ));
    let mut text = aterm_json::to_string(value).map_err(|e| format!("JSON: {e}"))?;
    text.push('\n');
    std::fs::write(&tmp, text).map_err(|e| format!("could not write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("could not place {}: {e}", path.display()))?;
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
        if chip.contains("M4 Max") {
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
            "gpu renderer build (schema 1, exclusive)",
            named(samples, &GPU, gpu(|g| g.legs.clone())),
            "",
        );
        ranked(
            "  PARALLEL (overlaps the legs above, not part of the sum)",
            named(samples, &GPU_PARALLEL, gpu(|g| g.parallel.clone())),
            "",
        );
        ranked(
            "  pipeline construction, by group",
            named(samples, &GPU_PIPES, gpu(|g| g.pipes.clone())),
            "",
        );
        ranked(
            "  cell pipelines, one slot each",
            Some(cell_pipelines(samples)),
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

/// built it (the slots are the binary's, not this lane's).
fn cell_pipelines(samples: &[Sample]) -> Vec<(String, Vec<f64>)> {
    let mut slots: std::collections::BTreeMap<String, Vec<f64>> = Default::default();
    for (slot, ms) in samples
        .iter()
        .filter_map(|s| s.gpu.as_ref())
        .flat_map(|g| &g.cells)
    {
        slots.entry(slot.clone()).or_default().push(*ms);
    }
    slots.into_iter().collect()
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
        named(samples, &GPU_PIPES, gpu(|g| g.pipes.clone())),
    ]
    .into_iter()
    .collect::<Option<Vec<_>>>()
    .map(|groups| groups.concat());
    json!({
        "schema": "aterm-start-v2",
        "binary": artifact.source.display().to_string(),
        "version": artifact.version,
        "sha256": artifact.sha256,
        "chip": chip,
        "rule_zero_hardware_eligible": chip.contains("M4 Max"),
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
            "startup_gpu_cell_pipeline_ms": distributions(Some(cell_pipelines(samples))),
            "external_observation_ms": summary_of(samples, |s| Some(s.external_ms)),
        },
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// aterm's parser reads.
    #[test]
    fn a_start_launch_has_the_audible_bell_off() {
        let doc = aterm_toml::from_str::<aterm_toml::Value>(CONFIG).expect("one document");
        let flag = |v: Option<&aterm_toml::Value>| v.and_then(aterm_toml::Value::as_bool);
        assert_eq!(flag(doc.get("bell_sound")), Some(false));
        assert_eq!(flag(doc.get("restore_session")), Some(false));
        assert_eq!(
            flag(doc.get("update").and_then(|t| t.get("enabled"))),
            Some(false)
        );
        assert_eq!(
            flag(doc.get("packages").and_then(|t| t.get("enabled"))),
            Some(false)
        );
    }

    /// NEGATIVE CONTROL: the first half, whose root goes.
    #[cfg(unix)]
    #[test]
    fn a_frozen_copy_that_never_answered_keeps_its_run_root() {
        use std::os::unix::fs::PermissionsExt as _;
        let lay = |dir: &Path, frozen_says: &str| {
            let source = dir.join("source");
            std::fs::write(
                &source,
                format!(
                    "#!/bin/sh
case \"$0\" in *-aterm) echo '{frozen_says}' ;; \
                     *) echo 'aterm 9.9.9' ;; esac\n"
                ),
            )
            .expect("the stand-in");
            std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
            source
        };
        let sources = super::super::Scratch::new("atv-freeze-src").expect("scratch");

        let answered = super::super::Scratch::new("atv-freeze-ran").expect("scratch");
        let root = answered.path().to_path_buf();
        let source = lay(sources.path(), "aterm 9.9.9");
        let frozen = freeze("ran", &source, &root).expect("the copy answers as aterm");
        assert!(frozen.frozen.starts_with(&root));
        drop(answered);
        assert!(!root.exists(), "it ran: its root goes");

        let silent = super::super::Scratch::new("atv-freeze-kept").expect("scratch");
        let root = silent.path().to_path_buf();
        std::fs::remove_file(&source).expect("the next stand-in");
        let source = lay(sources.path(), "not aterm");
        let refused = freeze("kept", &source, &root);
        drop(silent);
        let kept = root.join("kept-aterm").is_file();
        let _ = std::fs::remove_dir_all(&root);
        assert!(refused.is_err(), "{refused:?}");
        assert!(
            kept,
            "a copy that never printed aterm's line keeps its root"
        );
    }

    /// window-setup child) so a slot offset survives every sum check.
    pub(crate) fn metrics(bias: f64) -> Value {
        json!({
            "frames": 3,
            "first_present_ms": 14.9 + bias,
            "rust_main_to_first_present_ms": 15.0 + bias,
            "rust_main_to_first_visible_ms": 9.0 + bias,
            "startup_phase_schema": 1,
            "startup_phase_valid": true,
            "startup_router_ms": 0.1,
            "startup_gui_prepare_ms": 1.0,
            "startup_winit_dispatch_ms": 1.0,
            "startup_initial_surface_attach_ms": 10.0 + bias,
            "startup_surface_to_successful_redraw_ms": 2.0,
            "startup_successful_compose_ms": 0.2,
            "startup_successful_surface_transaction_ms": 0.6,
            "startup_successful_finalize_ms": 0.1,
            "startup_attach_schema": 1,
            "startup_attach_valid": true,
            "startup_attach_dispatch_ms": 0.1,
            "startup_attach_prepare_ms": 0.1,
            "startup_attach_window_create_ms": 2.0,
            "startup_attach_window_setup_ms": 1.0 + bias,
            "startup_attach_backend_finalize_ms": 5.0,
            "startup_attach_chrome_geometry_ms": 1.0,
            "startup_attach_surface_create_ms": 0.7,
            "startup_attach_finish_ms": 0.1,
            "backend": "gpu",
            "rows": 32,
            "cols": 110,
            "present_drops": 0,
            "last_present_drop_reason": "none",
            "last_present_drop_parked": false,
        })
    }

    pub(crate) fn obs(m: Value) -> Observation {
        Observation {
            metrics: m,
            external_ms: 20.0,
            thermal: "unavailable".into(),
        }
    }

    pub(crate) const LAWS: Laws = Laws {
        settle_ms: 1500,
        request: BackendRequest::Auto,
        allow_legacy: false,
    };

    fn with(mut m: Value, key: &str, v: Value) -> Value {
        m.as_object_mut().expect("object").insert(key.into(), v);
        m
    }

    fn without(mut m: Value, keys: &[&str]) -> Value {
        for k in keys {
            m.as_object_mut().expect("object").remove(*k);
        }
        m
    }

    fn refused(m: Value, laws: &Laws, why: &str) {
        let e = Sample::from_observation(&obs(m), laws).expect_err("must be refused");
        assert!(e.contains(why), "expected `{why}` in: {e}");
    }

    #[test]
    fn a_current_sample_validates_with_its_reveal_below_first_present() {
        let s = Sample::from_observation(&obs(metrics(0.0)), &LAWS).expect("valid");
        assert_eq!(s.visible_ms, Some(9.0));
        assert!(s.visible_ms < s.rust_main_ms);
        assert_eq!(
            s.shape(),
            Shape {
                router: true,
                visible: true,
                phases: true,
                attach: true
            }
        );
        assert!(
            Sample::from_observation(&obs(metrics(2.0)), &LAWS).is_ok(),
            "bias reconciles"
        );
    }

    #[test]
    fn every_partition_must_reconcile() {
        refused(
            with(metrics(0.0), "startup_successful_finalize_ms", json!(0.2)),
            &LAWS,
            "startup phase sum mismatch",
        );
        refused(
            with(metrics(0.0), "startup_attach_finish_ms", json!(0.2)),
            &LAWS,
            "startup attach sum mismatch",
        );
    }

    #[test]
    fn a_schema_is_the_integer_one_never_a_bool_or_a_float() {
        refused(
            with(metrics(0.0), "startup_phase_schema", json!(true)),
            &LAWS,
            "startup_phase_schema=true",
        );
        refused(
            with(metrics(0.0), "startup_phase_schema", json!(1.0)),
            &LAWS,
            "startup_phase_schema=1",
        );
        refused(
            with(metrics(0.0), "startup_attach_schema", json!(true)),
            &LAWS,
            "expected 1",
        );
        refused(
            with(metrics(0.0), "startup_phase_valid", json!(false)),
            &LAWS,
            "expected true",
        );
    }

    #[test]
    fn missing_or_non_finite_totals_are_refused() {
        refused(
            without(metrics(0.0), &["first_present_ms"]),
            &LAWS,
            "first_present_ms is absent",
        );
        refused(
            with(metrics(0.0), "first_present_ms", json!("NaN")),
            &LAWS,
            "first_present_ms",
        );
        refused(with(metrics(0.0), "frames", json!(0)), &LAWS, "frames=0");
        refused(
            with(metrics(0.0), "first_present_ms", json!(1600.0)),
            &LAWS,
            "not safely below",
        );
    }

    #[test]
    fn the_observer_brackets_every_total() {
        refused(
            with(metrics(0.0), "rust_main_to_first_visible_ms", json!(25.0)),
            &LAWS,
            "first-visible 25.00ms exceeds the external observation",
        );
        let mut late = obs(metrics(0.0));
        late.external_ms = 14.0;
        let e = Sample::from_observation(&late, &LAWS).expect_err("internal above external");
        assert!(e.contains("exceeds the external observation"), "{e}");
    }

    #[test]
    fn a_forced_backend_must_be_the_one_negotiated() {
        let cpu = Laws {
            request: BackendRequest::Cpu,
            ..LAWS
        };
        refused(metrics(0.0), &cpu, "requested backend cpu, negotiated gpu");
        refused(with(metrics(0.0), "rows", json!(31)), &LAWS, "rows=31");
    }

    #[test]
    fn the_drop_ledger_agrees_with_itself() {
        refused(
            with(
                metrics(0.0),
                "last_present_drop_reason",
                json!("gpu_timeout"),
            ),
            &LAWS,
            "zero drops carried",
        );
        refused(
            with(metrics(0.0), "present_drops", json!(2)),
            &LAWS,
            "nonzero drops carried reason=none",
        );
        refused(
            with(metrics(0.0), "last_present_drop_reason", json!("bogus")),
            &LAWS,
            "last_present_drop_reason",
        );
    }

    #[test]
    fn a_legacy_binary_needs_the_explicit_switch() {
        let legacy = without(
            metrics(0.0),
            &[
                "rust_main_to_first_present_ms",
                "startup_phase_schema",
                "startup_attach_schema",
            ],
        );
        refused(
            legacy.clone(),
            &LAWS,
            "rust_main_to_first_present_ms is absent",
        );
        let allowed = Laws {
            allow_legacy: true,
            ..LAWS
        };
        let s = Sample::from_observation(&obs(legacy), &allowed).expect("legacy allowed");
        assert!(!s.shape().phases);
        refused(
            without(metrics(0.0), &["startup_phase_schema"]),
            &allowed,
            "attach drill-down is present without its parent",
        );
    }

    /// (0.1 each) fill their 0.8 parent.
    fn drilled() -> Value {
        let mut m = metrics(0.0);
        let fields = [
            ("startup_worker_schema", json!(1)),
            ("startup_worker_valid", json!(true)),
            ("startup_worker_total_ms", json!(8.0)),
            ("startup_worker_overlap_ms", json!(5.0)),
            ("startup_worker_after_join_ms", json!(3.0)),
            ("startup_worker_post_join_ms", json!(2.0)),
            ("startup_worker_prelude_ms", json!(0.5)),
            ("startup_worker_gpu_build_ms", json!(4.0)),
            ("startup_worker_font_admit_ms", json!(0.5)),
            ("startup_worker_font_apply_ms", json!(0.5)),
            ("startup_worker_font_seal_ms", json!(0.5)),
            ("startup_worker_epilogue_ms", json!(0.5)),
            ("startup_gpu_schema", json!(1)),
            ("startup_gpu_valid", json!(true)),
            ("startup_gpu_instance_ms", json!(0.5)),
            ("startup_gpu_adapter_ms", json!(0.5)),
            ("startup_gpu_device_ms", json!(1.0)),
            ("startup_gpu_context_tail_ms", json!(0.5)),
            ("startup_gpu_font_join_ms", json!(0.5)),
            ("startup_gpu_pipelines_ms", json!(0.8)),
            ("startup_gpu_tail_ms", json!(0.2)),
            ("startup_gpu_font_thread_ms", json!(2.5)),
            (
                "startup_gpu_cell_pipeline_ms",
                json!({"plain": 0.2, "glyph": 0.1}),
            ),
        ];
        for (k, v) in fields {
            m = with(m, k, v);
        }
        for k in GPU_PIPES {
            m = with(m, k, json!(0.1));
        }
        m
    }

    #[test]
    fn the_worker_and_gpu_drill_downs_reconcile_when_valid() {
        let m = drilled();
        let s = Sample::from_observation(&obs(m.clone()), &LAWS).expect("both reconcile");
        assert_eq!(s.worker.as_ref().map(|w| w.total_ms), Some(8.0));
        let g = s.gpu.expect("gpu drill-down");
        assert_eq!(
            g.parallel,
            vec![2.5],
            "the font thread rides beside the sum"
        );
        assert_eq!(g.cells.len(), 2);
        for (key, v, why) in [
            ("startup_worker_total_ms", json!(9.0), "worker split"),
            (
                "startup_worker_post_join_ms",
                json!(1.0),
                "worker join split",
            ),
            ("startup_gpu_tail_ms", json!(0.5), "gpu leg sum mismatch"),
            (
                "startup_gpu_pipe_tail_ms",
                json!(0.3),
                "gpu pipeline group sum 1.00ms exceeds its parent 0.80ms",
            ),
            (
                "startup_gpu_cell_pipeline_ms",
                json!({}),
                "expected a non-empty object",
            ),
            (
                "startup_gpu_cell_pipeline_ms",
                json!({"plain": -1.0}),
                "cell pipeline plain=-1",
            ),
            (
                "startup_gpu_font_thread_ms",
                json!(-1.0),
                "startup_gpu_font_thread_ms",
            ),
        ] {
            refused(with(m.clone(), key, v), &LAWS, why);
        }
        let s = Sample::from_observation(
            &obs(with(m.clone(), "startup_worker_valid", json!(false))),
            &LAWS,
        )
        .expect("invalid worker is skipped");
        assert!(s.worker.is_none());
        let s = Sample::from_observation(&obs(with(m, "startup_gpu_valid", json!(false))), &LAWS)
            .expect("invalid gpu is skipped");
        assert!(s.gpu.is_none());
    }

    #[test]
    fn the_summary_carries_the_worker_total_the_parallel_leg_and_every_cell_slot() {
        let a = Sample::from_observation(&obs(drilled()), &LAWS).expect("valid");
        let b = Sample::from_observation(
            &obs(with(
                drilled(),
                "startup_gpu_cell_pipeline_ms",
                json!({"plain": 0.3, "wide": 0.4}),
            )),
            &LAWS,
        )
        .expect("valid");
        let samples = [a, b];
        assert_eq!(
            cell_pipelines(&samples),
            vec![
                ("glyph".to_string(), vec![0.1]),
                ("plain".to_string(), vec![0.2, 0.3]),
                ("wide".to_string(), vec![0.4]),
            ]
        );
        assert_eq!(
            named(&samples, &GPU_PARALLEL, |s| {
                s.gpu.as_ref().map(|g| g.parallel.clone())
            }),
            Some(vec![(GPU_PARALLEL[0].to_string(), vec![2.5, 2.5])])
        );
        assert_eq!(
            column(&samples, |s| s.worker.as_ref().map(|w| w.total_ms)),
            Some(vec![8.0, 8.0])
        );
        let plain = Sample::from_observation(&obs(metrics(0.0)), &LAWS).expect("valid");
        assert_eq!(
            named(&[samples[0].clone(), plain], &GPU, |s| {
                s.gpu.as_ref().map(|g| g.legs.clone())
            }),
            None
        );
    }

    /// so the sum is exact).
    fn prepared() -> Value {
        let mut m = with(metrics(0.0), BIRTH, json!(3.5));
        m = with(m, "startup_prepare_schema", json!(1));
        m = with(m, "startup_prepare_valid", json!(true));
        let ms = [
            0.25, 0.125, 0.125, 0.0625, 0.0625, 0.125, 0.0625, 0.0625, 0.0625, 0.0625,
        ];
        for (k, v) in PREPARE.into_iter().zip(ms) {
            m = with(m, k, json!(v));
        }
        m
    }

    #[test]
    fn the_preparation_split_reconciles_with_the_phase_it_splits() {
        let s = Sample::from_observation(&obs(prepared()), &LAWS).expect("reconciles");
        assert_eq!(s.prepare.as_ref().map(|p| p.iter().sum::<f64>()), Some(1.0));
        assert_eq!(s.birth_ms, Some(3.5));
        for (key, v, why) in [
            (
                "startup_prepare_app_ms",
                json!(0.3),
                "startup prepare sum mismatch",
            ),
            (
                "startup_prepare_boot_ms",
                json!(-0.1),
                "startup_prepare_boot_ms",
            ),
            (
                "startup_prepare_schema",
                json!(2),
                "startup_prepare_schema=2",
            ),
        ] {
            refused(with(prepared(), key, v), &LAWS, why);
        }
        let invalid = with(prepared(), "startup_prepare_valid", json!(false));
        let s = Sample::from_observation(
            &obs(with(invalid, "startup_prepare_app_ms", json!(9.0))),
            &LAWS,
        )
        .expect("an invalid split is skipped");
        assert!(s.prepare.is_none());
    }

    #[test]
    fn the_loaders_share_is_null_or_inside_the_observers_bound() {
        let s = Sample::from_observation(&obs(with(prepared(), BIRTH, Value::Null)), &LAWS)
            .expect("null is unavailable");
        assert_eq!(s.birth_ms, None);
        refused(
            with(prepared(), BIRTH, json!(-1.0)),
            &LAWS,
            "process_birth_to_rust_main_ms=-1",
        );
        refused(
            with(prepared(), BIRTH, json!("3")),
            &LAWS,
            "process_birth_to_rust_main_ms",
        );
        refused(
            with(prepared(), BIRTH, json!(6.0)),
            &LAWS,
            "birth->main 6.00ms",
        );
    }

    #[test]
    fn a_missing_reveal_stamp_is_unavailable_not_invalid() {
        let s = Sample::from_observation(
            &obs(without(metrics(0.0), &["rust_main_to_first_visible_ms"])),
            &LAWS,
        )
        .expect("an older binary still measures");
        assert_eq!(s.visible_ms, None);
        assert!(!s.shape().visible);
    }

    #[test]
    fn one_run_holds_one_shape_and_one_backend() {
        let mut c = Consistency::default();
        let a = Sample::from_observation(&obs(metrics(0.0)), &LAWS).expect("valid");
        c.check(&a).expect("first");
        c.check(&a).expect("same");
        let b = Sample::from_observation(
            &obs(without(metrics(0.0), &["rust_main_to_first_visible_ms"])),
            &LAWS,
        )
        .expect("valid");
        assert!(
            c.check(&b)
                .unwrap_err()
                .contains("mixed startup-metric schemas")
        );
        let mut c = Consistency::default();
        c.check(&a).expect("first");
        let cpu =
            Sample::from_observation(&obs(with(metrics(0.0), "backend", json!("cpu"))), &LAWS)
                .expect("valid");
        assert!(c.check(&cpu).unwrap_err().contains("mixed CPU/GPU"));
    }

    #[test]
    fn a_record_carries_every_field_and_nulls_what_is_absent() {
        let s = Sample::from_observation(
            &obs(without(metrics(0.0), &["rust_main_to_first_visible_ms"])),
            &LAWS,
        )
        .expect("valid");
        let j = s.to_json();
        assert_eq!(j.get("rust_main_to_first_visible_ms"), Some(&Value::Null));
        assert_eq!(
            j.get("startup_attach_window_setup_ms")
                .and_then(Value::as_f64),
            Some(1.0)
        );
        assert_eq!(j.get("startup_gpu_cell_pipeline_ms"), Some(&Value::Null));
        assert_eq!(j.get(BIRTH), Some(&Value::Null));
        assert_eq!(j.get("startup_prepare_boot_ms"), Some(&Value::Null));
    }
}
