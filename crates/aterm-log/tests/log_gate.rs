// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Integration test for `log-gate` and `record-accessors` using this binary's fresh `OnceLock`.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use aterm_log::{Level, LevelFilter, Log, Metadata, Record, set_logger, set_max_level};

#[derive(Debug, Clone, PartialEq, Eq)]
struct Captured {
    level: Level,
    metadata_level: Level,
    target: String,
    body: String,
    file: Option<String>,
    has_line: bool,
}

struct GateLogger {
    admit_up_to: AtomicUsize,
    enabled_seen: Mutex<Vec<Level>>,
    logged: Mutex<Vec<Captured>>,
}

impl GateLogger {
    const fn new() -> Self {
        Self {
            admit_up_to: AtomicUsize::new(Level::Trace as usize),
            enabled_seen: Mutex::new(Vec::new()),
            logged: Mutex::new(Vec::new()),
        }
    }

    fn take(&self) -> (Vec<Level>, Vec<Captured>) {
        let enabled = std::mem::take(&mut *self.enabled_seen.lock().expect("enabled lock"));
        let logged = std::mem::take(&mut *self.logged.lock().expect("logged lock"));
        (enabled, logged)
    }
}

impl Log for GateLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        self.enabled_seen
            .lock()
            .expect("enabled lock")
            .push(metadata.level());
        (metadata.level() as usize) <= self.admit_up_to.load(Ordering::SeqCst)
    }

    fn log(&self, record: &Record<'_>) {
        self.logged.lock().expect("logged lock").push(Captured {
            level: record.level(),
            metadata_level: record.metadata().level(),
            target: record.target().to_string(),
            body: record.args().to_string(),
            file: record.file().map(str::to_string),
            has_line: record.line().is_some(),
        });
    }

    fn flush(&self) {}
}

static LOGGER: GateLogger = GateLogger::new();

fn emit_all_five() {
    aterm_log::error!("e={}", 1);
    aterm_log::warn!("w={}", 2);
    aterm_log::info!("i={}", 3);
    aterm_log::debug!("d={}", 4);
    aterm_log::trace!("t={}", 5);
}

#[test]
fn log_gate_and_record_accessors_hold_across_filters_and_logger_decisions() {
    // Before a logger is installed, even `Trace` max level drops records cleanly.
    set_max_level(LevelFilter::Trace);
    emit_all_five();
    assert_eq!(LOGGER.take(), (vec![], vec![]));

    set_logger(&LOGGER).expect("first logger install must succeed");
    assert!(
        set_logger(&LOGGER).is_err(),
        "second set_logger must return Err(SetLoggerError)"
    );

    let all_levels = [
        (Level::Error, "e=1"),
        (Level::Warn, "w=2"),
        (Level::Info, "i=3"),
        (Level::Debug, "d=4"),
        (Level::Trace, "t=5"),
    ];

    for (filter, expected_count) in [
        (LevelFilter::Off, 0usize),
        (LevelFilter::Error, 1),
        (LevelFilter::Warn, 2),
        (LevelFilter::Info, 3),
        (LevelFilter::Debug, 4),
        (LevelFilter::Trace, 5),
    ] {
        set_max_level(filter);
        emit_all_five();
        let (enabled, logged) = LOGGER.take();
        let want_levels: Vec<Level> = all_levels[..expected_count]
            .iter()
            .map(|(lvl, _)| *lvl)
            .collect();
        assert_eq!(
            enabled, want_levels,
            "{filter:?}: MAX_LEVEL gate must short-circuit before Log::enabled"
        );
        assert_eq!(logged.len(), expected_count, "{filter:?}");
        for (got, (want_lvl, want_body)) in logged.iter().zip(&all_levels[..expected_count]) {
            assert_eq!(got.level, *want_lvl);
            assert_eq!(got.metadata_level, *want_lvl);
            assert_eq!(got.target, module_path!());
            assert_eq!(got.body, *want_body);
            assert_eq!(got.file.as_deref(), Some(file!()));
            assert!(got.has_line);
        }
    }

    // Direct `__log` with `None` file/line preserves `None` through `Record` accessors,
    // and `Log::enabled` returning false suppresses `Log::log` even when `MAX_LEVEL` admits.
    set_max_level(LevelFilter::Trace);
    LOGGER
        .admit_up_to
        .store(Level::Warn as usize, Ordering::SeqCst);
    aterm_log::__log(
        Level::Warn,
        "custom::target",
        format_args!("manual {}", 7),
        None,
        None,
    );
    aterm_log::__log(
        Level::Info,
        "custom::target",
        format_args!("filtered {}", 8),
        Some("manual.rs"),
        Some(42),
    );
    let (enabled, logged) = LOGGER.take();
    assert_eq!(enabled, vec![Level::Warn, Level::Info]);
    assert_eq!(
        logged,
        vec![Captured {
            level: Level::Warn,
            metadata_level: Level::Warn,
            target: "custom::target".to_string(),
            body: "manual 7".to_string(),
            file: None,
            has_line: false,
        }]
    );
}
