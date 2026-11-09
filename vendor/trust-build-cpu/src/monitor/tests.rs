use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use super::*;

const SYS: &str = r#"{"schema":1,"kind":"sys.overview","as_of":{"state":"fresh","age_ms":100,"window_ms":1000},"machine":{"cpus":28},"global":{"cpu_mc":20000,"mem_available_bytes":64000,"mem_available_basis":"free_plus_reclaimable_cache","mem_pressure":"normal","thermal":"nominal"}}"#;

fn fixture(body: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let program = dir.path().join("probe");
    fs::write(&program, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
    (dir, program)
}

fn wait_for<T>(label: &str, mut read: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(value) = read() {
            return value;
        }
        assert!(start.elapsed() < Duration::from_secs(60), "test hang detector: {label}");
        thread::sleep(Duration::from_millis(5));
    }
}

fn good_body() -> String {
    format!(
        "printf x >> \"$0.calls\"\nif [ -f \"$0.bad\" ]; then printf bad; exit 0; fi\ncase \"$1\" in\nctl) printf '%s\\n' '{SYS}' ;;\npkg) printf '%s\\n' '{{\"available\":32000}}' ;;\n*) exit 3 ;;\nesac"
    )
}

const REQUIRED_PROBE_HANG: Duration = Duration::from_secs(60);

fn required_probe_budget(started: Instant) -> Duration {
    REQUIRED_PROBE_HANG.saturating_sub(started.elapsed())
}

fn required_latest(monitor: &Monitor) -> (Observation, u64) {
    wait_for("required observation", || {
        assert!(
            monitor.unavailable().is_none(),
            "required successful probe failed: {:?}",
            monitor.unavailable()
        );
        monitor.latest()
    })
}

#[test]
fn successful_round_preserves_cache_age_and_does_not_probe_on_reads() {
    successful_round_with_age(Duration::ZERO);
}

#[test]
fn required_probe_keeps_its_original_start_without_a_latency_budget() {
    successful_round_with_age(Duration::from_secs(3));
}

fn successful_round_with_age(age: Duration) {
    let started = Instant::now() - age;
    let (_dir, program) = fixture(&good_body());
    let monitor =
        Monitor::spawn(program.clone(), Duration::from_secs(60), required_probe_budget(started)).unwrap();
    let first = required_latest(&monitor);
    assert_eq!(first.1, 32000);
    let again = monitor.latest().unwrap();
    assert_eq!(format!("{:?}", first.0), format!("{:?}", again.0));
    assert_eq!(fs::read(program.with_extension("calls")).unwrap(), b"xx");
    assert_eq!(
        again.0.admission_limit(
            Instant::now() + Duration::from_secs(4),
            1,
            again.1,
            std::num::NonZeroU64::new(1000).unwrap(),
            0
        ),
        0
    );
}

#[test]
fn a_failed_round_clears_the_previous_observation() {
    let started = Instant::now();
    let (_dir, program) = fixture(&good_body());
    let monitor =
        Monitor::spawn(program.clone(), Duration::from_millis(20), required_probe_budget(started)).unwrap();
    required_latest(&monitor);
    fs::write(program.with_extension("bad"), b"bad").unwrap();
    wait_for("malformed observation", || monitor.unavailable());
    assert!(monitor.latest().is_none());
}

fn pid(program: &Path) -> u32 {
    wait_for("probe pid", || fs::read_to_string(program.with_extension("pid")).ok()?.parse().ok())
}

fn assert_reaped(pid: u32) {
    // SAFETY: signal zero only observes whether the fixture's recorded process exists.
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, 0) }, -1);
    assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
}

#[test]
fn timeout_reaps_only_the_owned_probe() {
    let (_dir, program) = fixture("printf '%s' \"$$\" > \"$0.pid\"\nexec /bin/sleep 60");
    let monitor =
        Monitor::spawn(program.clone(), Duration::from_secs(60), Duration::from_secs(1)).unwrap();
    let child = pid(&program);
    assert!(wait_for("probe timeout", || monitor.unavailable()).contains("timed out"));
    assert!(monitor.latest().is_none());
    assert_reaped(child);
}

#[test]
fn cancellation_interrupts_a_probe_and_reaps_it() {
    let (_dir, program) = fixture("printf '%s' \"$$\" > \"$0.pid\"\nexec /bin/sleep 120");
    let monitor =
        Monitor::spawn(program.clone(), Duration::from_secs(120), Duration::from_secs(120))
            .unwrap();
    let child = pid(&program);
    let (done, receiver) = std::sync::mpsc::channel();
    thread::spawn(move || {
        drop(monitor);
        done.send(()).unwrap();
    });
    receiver.recv_timeout(Duration::from_secs(60)).expect("cancellation hang detector");
    assert_reaped(child);
}

#[test]
fn output_is_bounded_and_invalid_configuration_is_refused() {
    let started = Instant::now();
    let (_dir, program) = fixture("while :; do printf '%1024s' x; done");
    let monitor =
        Monitor::spawn(program.clone(), Duration::from_secs(60), required_probe_budget(started)).unwrap();
    assert!(wait_for("output limit", || monitor.unavailable()).contains("size bound"));
    assert!(monitor.latest().is_none());
    assert!(Monitor::spawn(program, Duration::ZERO, Duration::from_secs(1)).is_err());
    assert!(
        Monitor::spawn("relative".into(), Duration::from_secs(1), Duration::from_secs(1)).is_err()
    );
}
