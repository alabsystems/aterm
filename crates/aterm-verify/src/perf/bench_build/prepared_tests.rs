// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::{Cell, RefCell};
use std::os::unix::fs::PermissionsExt as _;
use std::rc::Rc;

const ARGS: &[&str] = &[
    "--profile",
    "test",
    "-p",
    "aterm",
    "--bench",
    "fabric_bench",
];

struct Fixture {
    _directory: aterm_tempfile::TempDir,
    app: PathBuf,
    bench: PathBuf,
    state: Rc<State>,
}

struct State {
    context: RefCell<BuildContext>,
    messages: RefCell<String>,
    compilations: Cell<usize>,
}

#[derive(Clone)]
struct FakeCompiler(Rc<State>);

impl Fixture {
    fn new() -> Self {
        let directory = aterm_tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("debug")).unwrap();
        std::fs::create_dir(root.join("debug/deps")).unwrap();
        let app = root.join("debug/aterm");
        let bench = root.join("debug/deps/fabric_bench-1234567890abcdef");
        // Inert regular payloads: the injected runtime never executes either path.
        for path in [&app, &bench] {
            std::fs::write(path, b"inert preparation fixture").unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let messages = [
            artifact("aterm", "bin", &app),
            artifact("fabric_bench", "bench", &bench),
            r#"{"reason":"build-finished","success":true}"#.to_owned(),
        ]
        .join("\n");
        let state = Rc::new(State {
            context: RefCell::new(BuildContext {
                root_identity: {
                    let meta = std::fs::metadata(&root).unwrap();
                    (meta.dev(), meta.ino())
                },
                root,
                program: PathBuf::from("/not-executed/compiler"),
                build_dir: Some("captured build directory".into()),
            }),
            messages: RefCell::new(messages),
            compilations: Cell::new(0),
        });
        Self {
            _directory: directory,
            app,
            bench,
            state,
        }
    }

    fn prepare(&self) -> PreparedBench<FakeCompiler> {
        self.try_prepare().expect("valid preparation refused")
    }

    fn try_prepare(&self) -> Result<PreparedBench<FakeCompiler>, Halt> {
        PreparedBench::prepare(
            FakeCompiler(self.state.clone()),
            bench_profile::Profile::Dev,
            ARGS,
        )
        .map(|(ready, _)| ready)
    }
}

fn artifact(name: &str, kind: &str, path: &Path) -> String {
    format!(
        "  {}  ",
        aterm_json::json!({"reason":"compiler-artifact", "target":{"name":name,"kind":[kind]}, "executable":path})
    )
}

impl Compiler for FakeCompiler {
    fn context(&self) -> Result<BuildContext, Halt> {
        Ok(self.0.context.borrow().clone())
    }
    fn sources(&self) -> Result<Vec<flags::Source>, Halt> {
        Ok(Vec::new())
    }
    fn audit(&self, _: &[&str]) -> Result<units::Audit, Halt> {
        Ok(units::Audit::of(r#"{"version":1,"roots":[0],"units":[{"pkg_id":"aterm 0.1.0 (path)","mode":"build","target":{"kind":["bench"]},"profile":{"opt_level":"0","debug_assertions":true,"overflow_checks":true,"incremental":false,"lto":"false","codegen_units":256},"dependencies":[]}]}"#).unwrap())
    }
    fn compile(&self, args: &[&str]) -> Result<String, Halt> {
        let count = self.0.compilations.get() + 1;
        self.0.compilations.set(count);
        assert_eq!(args, ARGS);
        if count != 1 {
            return Err(Halt::Fail(
                "unexpected compiler admission after preparation".into(),
            ));
        }
        Ok(self.0.messages.borrow().clone())
    }
    fn runtime_command(&self, executable: &Path) -> Result<Command, Halt> {
        let mut command = crate::exec::check_command(executable);
        command.current_dir(&self.0.context.borrow().root);
        Ok(command)
    }
}

struct Capture {
    program: PathBuf,
    args: Vec<String>,
    env: Vec<(std::ffi::OsString, Option<std::ffi::OsString>)>,
}

#[derive(Default)]
struct FakeRuntime {
    admissions: usize,
    validations: Vec<usize>,
    captures: Vec<Capture>,
    during_admission: Option<Box<dyn FnMut()>>,
    during_capture: Option<Box<dyn FnMut()>>,
    refuse_admission: bool,
    expire_admission: bool,
}

impl Runtime for FakeRuntime {
    type Admission = usize;
    fn admit(&mut self) -> Result<usize, Halt> {
        self.admissions += 1;
        if let Some(action) = &mut self.during_admission {
            action();
        }
        if self.refuse_admission {
            return Err(Halt::Timing("injected entry refusal".into()));
        }
        Ok(self.admissions)
    }
    fn validate(&mut self, admitted: &usize) -> Result<(), Halt> {
        self.validations.push(*admitted);
        if self.expire_admission {
            return Err(Halt::Timing("injected lapsed entry".into()));
        }
        Ok(())
    }
    fn capture(&mut self, command: &mut Command, _: &str, _: &str) -> Result<String, Halt> {
        self.captures.push(Capture {
            program: command.get_program().into(),
            args: command
                .get_args()
                .map(|word| word.to_string_lossy().into_owned())
                .collect(),
            env: command
                .get_envs()
                .map(|(key, value)| (key.to_owned(), value.map(std::ffi::OsStr::to_owned)))
                .collect(),
        });
        if let Some(action) = &mut self.during_capture {
            action();
        }
        Ok("captured world".into())
    }
}

fn run(
    ready: &PreparedBench<FakeCompiler>,
    runtime: &mut FakeRuntime,
    entry: Entry,
    world: &str,
) -> Result<String, Halt> {
    ready.run(
        ("WORLD", world),
        ("OUT", &Path::new("/not-created").join(world)),
        "progress:",
        entry,
        runtime,
    )
}

#[test]
fn one_preparation_runs_three_worlds_with_one_compiler_capture() {
    let fixture = Fixture::new();
    let ready = fixture.prepare();
    let mut runtime = FakeRuntime::default();
    for world in ["attach", "pingpong", "messaging"] {
        ready
            .require_request(bench_profile::Profile::Dev, ARGS)
            .unwrap();
        let result = run(&ready, &mut runtime, Entry::Counted, world);
        assert_eq!(
            fixture.state.compilations.get(),
            1,
            "runtime world {world} requested another compiler admission"
        );
        assert!(
            result.is_ok(),
            "world {world} must use its preparation without another compiler call"
        );
    }
    assert_eq!(runtime.captures.len(), 3);
    assert_eq!(runtime.admissions, 0);
    assert!(runtime.validations.is_empty());
    for (capture, world) in runtime
        .captures
        .iter()
        .zip(["attach", "pingpong", "messaging"])
    {
        assert_eq!(capture.program, fixture.bench);
        assert_eq!(capture.args, ["--bench"]);
        assert!(capture.env.contains(&("WORLD".into(), Some(world.into()))));
        assert!(capture.env.contains(&(
            "ATERM_TEST_EXECUTABLE".into(),
            Some(fixture.app.as_os_str().to_owned())
        )));
        assert!(
            capture
                .env
                .contains(&("OUT".into(), Some(format!("/not-created/{world}").into())))
        );
    }
}

#[test]
fn timed_arms_obtain_and_close_their_own_entry() {
    let fixture = Fixture::new();
    let ready = fixture.prepare();
    let mut runtime = FakeRuntime::default();
    for world in ["first", "second", "third"] {
        run(&ready, &mut runtime, Entry::Timed, world).unwrap();
    }
    assert_eq!(runtime.admissions, 3);
    assert_eq!(runtime.validations, [1, 2, 3]);
    assert_eq!(runtime.captures.len(), 3);
    assert_eq!(fixture.state.compilations.get(), 1);
    let mut refused = FakeRuntime {
        refuse_admission: true,
        ..FakeRuntime::default()
    };
    assert!(matches!(
        run(&ready, &mut refused, Entry::Timed, "refused"),
        Err(Halt::Timing(_))
    ));
    assert!(refused.captures.is_empty());
    let mut expired = FakeRuntime {
        expire_admission: true,
        ..FakeRuntime::default()
    };
    assert!(matches!(
        run(&ready, &mut expired, Entry::Timed, "expired"),
        Err(Halt::Timing(_))
    ));
    assert_eq!(expired.captures.len(), 1);
    assert_eq!(expired.validations, [1]);
}

#[test]
fn every_world_must_match_the_prepared_request_and_context() {
    let fixture = Fixture::new();
    let ready = fixture.prepare();
    ready
        .require_request(
            bench_profile::Profile::Dev,
            &["--profile=test", "--package=aterm", "--bench=fabric_bench"],
        )
        .unwrap();
    for args in [
        vec![
            "--profile",
            "test",
            "-p",
            "aterm",
            "--bench",
            "restore_bench",
        ],
        vec![
            "--profile",
            "test",
            "-p",
            "aterm",
            "--bench",
            "fabric_bench",
            "--features",
            "dev-seams",
        ],
        vec![
            "--profile",
            "opt-test",
            "-p",
            "aterm",
            "--bench",
            "fabric_bench",
        ],
    ] {
        assert!(
            ready
                .require_request(bench_profile::Profile::Dev, &args)
                .is_err()
        );
    }
    fixture.state.context.borrow_mut().build_dir = Some("different build directory".into());
    let mut runtime = FakeRuntime::default();
    assert!(run(&ready, &mut runtime, Entry::Counted, "mismatch").is_err());
    assert!(runtime.captures.is_empty());
}

#[test]
fn preparation_requires_both_artifacts_from_one_successful_unambiguous_capture() {
    for bad in [
        artifact("aterm", "bin", Path::new("/missing/debug/aterm")),
        artifact("fabric_bench", "bench", Path::new("/missing/debug/fabric_bench")),
        r#"{"reason":"compiler-artifact","reason":"compiler-artifact","target":{"name":"unrelated","kind":["lib"]}}"#.into(),
        r#"{"reason":"build-finished","success":false}"#.into(),
        r#"{"reason":"build-finished","reason":"other","success":false}"#.into(),
    ] {
        let fixture = Fixture::new();
        fixture.state.messages.borrow_mut().push_str(&format!("\n{bad}"));
        assert!(fixture.try_prepare().is_err());
        assert_eq!(fixture.state.compilations.get(), 1);
    }
    let fixture = Fixture::new();
    let rows = artifact("aterm", "bin", &fixture.app)
        + "\n"
        + r#"{"reason":"build-finished","success":true}"#;
    *fixture.state.messages.borrow_mut() = rows;
    assert!(fixture.try_prepare().is_err());
    let fixture = Fixture::new();
    *fixture.state.messages.borrow_mut() = [
        artifact("aterm", "bin", &fixture.app),
        artifact("fabric_bench", "lib", &fixture.bench),
        r#"{"reason":"build-finished","success":true}"#.into(),
    ]
    .join("\n");
    assert!(
        fixture.try_prepare().is_err(),
        "a library is not the requested benchmark artifact"
    );
    let fixture = Fixture::new();
    fixture.state.messages.borrow_mut().push_str(&format!(
        "\n{}",
        artifact("fabric_bench", "bench", &fixture.bench)
    ));
    fixture.state.messages.borrow_mut().push_str(
        "\n{\"reason\":\"compiler-artifact\",\"target\":{\"name\":\"fabric_bench\",\"kind\":[\"bench\"]},\"executable\":null,\"fresh\":true}\n{\"reason\":\"compiler-artifact\",\"target\":{\"name\":\"aterm\",\"kind\":[\"bin\"]},\"fresh\":true}"
    );
    assert!(
        fixture.try_prepare().is_ok(),
        "duplicate identical artifact remains unambiguous"
    );
    let capture = fixture.state.messages.borrow();
    let originals: Vec<&str> = capture.lines().collect();
    let receipt = compiler_records(&capture, &fixture.app, "fabric_bench", &fixture.bench).unwrap();
    assert_eq!(
        receipt,
        [
            ("app", originals[0]),
            ("bench", originals[1]),
            ("bench", originals[3])
        ]
    );
    drop(capture);
    for hard_link in [false, true] {
        let fixture = Fixture::new();
        if hard_link {
            std::fs::remove_file(&fixture.bench).unwrap();
            std::fs::hard_link(&fixture.app, &fixture.bench).unwrap();
        } else {
            *fixture.state.messages.borrow_mut() = [
                artifact("aterm", "bin", &fixture.app),
                artifact("fabric_bench", "bench", &fixture.app),
                r#"{"reason":"build-finished","success":true}"#.into(),
            ]
            .join("\n");
        }
        assert!(
            fixture.try_prepare().is_err(),
            "one file cannot fill both executable roles"
        );
    }
}

#[test]
fn artifact_changes_during_admission_refuse_before_any_runtime() {
    for which in ["app", "bench"] {
        for change in ["replace", "bytes", "mode", "remove"] {
            let fixture = Fixture::new();
            let ready = fixture.prepare();
            let path = if which == "app" {
                fixture.app.clone()
            } else {
                fixture.bench.clone()
            };
            let mut runtime = FakeRuntime {
                during_admission: Some(Box::new(move || change_inert_file(&path, change))),
                ..FakeRuntime::default()
            };
            assert!(
                run(&ready, &mut runtime, Entry::Timed, "changed").is_err(),
                "{which}/{change}"
            );
            assert!(
                runtime.captures.is_empty(),
                "{which}/{change} launched a runtime"
            );
            assert_eq!(fixture.state.compilations.get(), 1);
        }
    }
}

#[test]
fn artifact_changes_during_execution_are_not_a_successful_take() {
    for which in ["app", "bench"] {
        let fixture = Fixture::new();
        let ready = fixture.prepare();
        let path = if which == "app" {
            fixture.app.clone()
        } else {
            fixture.bench.clone()
        };
        let mut runtime = FakeRuntime {
            during_capture: Some(Box::new(move || change_inert_file(&path, "replace"))),
            ..FakeRuntime::default()
        };
        assert!(run(&ready, &mut runtime, Entry::Timed, "replaced").is_err());
        assert_eq!(runtime.captures.len(), 1);
        assert_eq!(runtime.validations, [1]);
    }
}

fn change_inert_file(path: &Path, change: &str) {
    let before = std::fs::metadata(path).unwrap();
    match change {
        "replace" => {
            let copy = path.with_extension("inert-replacement");
            std::fs::write(&copy, std::fs::read(path).unwrap()).unwrap();
            std::fs::set_permissions(&copy, before.permissions()).unwrap();
            File::options()
                .write(true)
                .open(&copy)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(before.modified().unwrap()))
                .unwrap();
            std::fs::rename(copy, path).unwrap();
        }
        "bytes" => {
            let bytes = vec![b'x'; usize::try_from(before.len()).unwrap()];
            std::fs::write(path, bytes).unwrap();
            File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(before.modified().unwrap()))
                .unwrap();
        }
        "mode" => std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o500)).unwrap(),
        "remove" => std::fs::remove_file(path).unwrap(),
        _ => unreachable!(),
    }
}
