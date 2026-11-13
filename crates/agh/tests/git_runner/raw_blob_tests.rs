// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real Git diagnosis: raw blob wants are not commit-history haves.

use super::*;

struct KeptWorld(Option<World>);
impl std::ops::Deref for KeptWorld {
    type Target = World;
    fn deref(&self) -> &World {
        self.0.as_ref().unwrap()
    }
}
impl Drop for KeptWorld {
    fn drop(&mut self) {
        if std::thread::panicking()
            && let Some(world) = self.0.take()
        {
            eprintln!("raw-blob world retained: {}", world.root.keep().display());
        }
    }
}

/// The actual native Git and its normal owned deadline collector. No Git stand-in,
/// anonymous network, replacement tool, or process-tree snapshot is supplied.
pub(super) fn git_input(dir: &Path, args: &[&str], input: Option<&[u8]>) -> std::process::Output {
    git_input_in(dir, args, input, &[])
}

/// [`git_input`] with `env` added to its environment.
fn git_input_in(
    dir: &Path,
    args: &[&str],
    input: Option<&[u8]>,
    env: &[(&str, &Path)],
) -> std::process::Output {
    let mut command = aterm_uds::spawnfd::command(host_git_path());
    command
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_GRAFT_FILE", "/dev/null")
        .envs(env.iter().copied());
    let stdin = if let Some(bytes) = input {
        let path = dir.join("request-data");
        std::fs::write(&path, bytes).unwrap();
        aterm_uds::spawnfd::Stdio::from(std::fs::File::open(path).unwrap())
    } else {
        aterm_uds::spawnfd::Stdio::null()
    };
    aterm_uds::deadline::output_until_from(
        &mut command,
        stdin,
        std::time::Instant::now() + common::HANG,
    )
    .unwrap_or_else(|error| panic!("native Git {args:?} in {}: {error:?}", dir.display()))
}

/// `input`'s objects as `scratch` holds them, read as the runner reads a checkout's blob
/// presence: through an empty repository beside it whose object store is `scratch`'s, which
/// names no promisor. A filtered fetch makes `scratch` a promisor of its source, and a promisor
/// read of an absent object under `GIT_NO_LAZY_FETCH` answers `missing` on git 2.56.0 but
/// stops (128) on Ubuntu 24.04's 2.43.0. A caller asks for an object `scratch` holds too, so
/// the answer shows the view sees it.
fn presence(scratch: &Path, input: &[u8]) -> std::process::Output {
    let view = scratch.with_extension("presence.git");
    if !view.exists() {
        host_git(
            scratch.parent().unwrap(),
            &[
                "init",
                "--quiet",
                "--bare",
                "--template=",
                view.to_str().unwrap(),
            ],
        );
    }
    let objects = scratch.join("objects");
    git_input_in(
        scratch,
        &["cat-file", "--batch-check=%(objectname) %(objecttype)"],
        Some(input),
        &[("GIT_DIR", &view), ("GIT_OBJECT_DIRECTORY", &objects)],
    )
}

/// A real provider may accept an exact historical commit but refuse raw blob
/// wants. Checkout and cold/mixed-cache FullSource must acquire their respective
/// closures through that same native protocol, without changing read-only DATA.
#[test]
fn exact_commit_acquisition_preserves_checkout_and_full_source_across_native_protocols() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{OnceLock, Weak};
    let world = KeptWorld(Some(World::canonical("git-hidden-checkout-body")));
    let bare = bare_repo(world.path(), "provider");
    let work = world.path().join("provider-work");
    let retired_body = b"historical bytes removed before the selected root\n";
    std::fs::write(work.join("retired"), retired_body).unwrap();
    host_git(&work, &["add", "retired"]);
    host_git(
        &work,
        &["commit", "--quiet", "-m", "retired historical body"],
    );
    let retired_blob = host_git(&work, &["rev-parse", "HEAD:retired"]);
    std::fs::remove_file(work.join("retired")).unwrap();
    let other_body = b"a second required body in the selected tree\n";
    std::fs::write(work.join("other"), other_body).unwrap();
    let body = b"selected historical bytes outside advertised main\n";
    std::fs::write(work.join("selected"), body).unwrap();
    host_git(&work, &["add", "-A"]);
    host_git(
        &work,
        &["commit", "--quiet", "-m", "selected historical source"],
    );
    let commit = host_git(&work, &["rev-parse", "HEAD"]);
    let blob = host_git(&work, &["rev-parse", "HEAD:selected"]);
    let other_blob = host_git(&work, &["rev-parse", "HEAD:other"]);
    host_git(
        &work,
        &[
            "push",
            "--quiet",
            bare.to_str().unwrap(),
            "HEAD:refs/heads/historical",
        ],
    );
    host_git(&bare, &["config", "uploadpack.allowFilter", "true"]);
    host_git(
        &work,
        &["commit", "--quiet", "--allow-empty", "-m", "main advances"],
    );
    host_git(&work, &["push", "--quiet", bare.to_str().unwrap(), "main"]);
    host_git(&bare, &["config", "uploadpack.allowTipSHA1InWant", "true"]);
    host_git(
        &bare,
        &["config", "uploadpack.allowReachableSHA1InWant", "false"],
    );
    host_git(&bare, &["config", "uploadpack.allowAnySHA1InWant", "false"]);
    assert_ne!(host_git(&bare, &["rev-parse", "main"]), commit);
    let raw = world.path().join("raw-blob.git");
    host_git(
        world.path(),
        &["init", "--quiet", "--bare", raw.to_str().unwrap()],
    );
    let remote = format!("file://{}", bare.display());
    let refused = git_input(
        &raw,
        &[
            "-c",
            "protocol.version=0",
            "fetch",
            "--quiet",
            "--no-tags",
            &remote,
            &blob,
        ],
        None,
    );
    assert!(
        !refused.status.success(),
        "the provider control must refuse the raw blob: {}",
        String::from_utf8_lossy(&refused.stderr)
    );
    let destination = world.path().join("destination.git");
    host_git(
        world.path(),
        &["init", "--quiet", "--bare", destination.to_str().unwrap()],
    );
    let gh = world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &bare)
        .build();
    let slug = common::slug("fixture-dev/app");
    gh.run_git(&GitRun::fetch_ref(
        &slug,
        &destination,
        &commit,
        "refs/agh/selected",
        false,
        agh::git::FetchScope::SelectedCheckout { cache: None },
    ))
    .unwrap_or_else(|error| panic!("exact hidden-commit checkout: {error:?}"));
    assert_eq!(
        host_git(&destination, &["rev-parse", "refs/agh/selected"]),
        commit
    );
    let received = git_input(&destination, &["cat-file", "blob", &blob], None);
    assert!(received.status.success());
    assert_eq!(received.stdout, body);
    let children = gh.fx().children_run();
    let body_fetch = children
        .iter()
        .find(|child| child.args.iter().any(|arg| arg == "--depth=1"))
        .expect("actual exact-commit body argv");
    let metadata_fetch = children
        .iter()
        .find(|child| child.args.iter().any(|arg| arg == "--filter=blob:none"))
        .expect("actual exact-root metadata argv");
    let v0 = world.path().join("commit-body-v0.git");
    host_git(
        world.path(),
        &["init", "--quiet", "--bare", v0.to_str().unwrap()],
    );
    let mut args = vec![OsString::from("-c"), OsString::from("protocol.version=0")];
    args.extend(body_fetch.args.clone());
    let replayed = emitted_git_input(&v0, body_fetch, &args, None, None);
    assert!(
        replayed.status.success(),
        "v0 exact commit: {}",
        String::from_utf8_lossy(&replayed.stderr)
    );
    assert_eq!(
        host_git(&v0, &["rev-parse", "refs/agh/checkout-body"]),
        commit
    );
    let v0_body = git_input(&v0, &["cat-file", "blob", &blob], None);
    assert!(v0_body.status.success());
    assert_eq!(v0_body.stdout, body);
    assert!(!destination.join("shallow").exists());
    assert!(
        !std::fs::read_dir(destination.join("objects/pack"))
            .unwrap()
            .any(|entry| entry
                .unwrap()
                .path()
                .extension()
                .is_some_and(|ext| ext == "promisor"))
    );

    // Negotiate real protocol v0 through the operation's OWN private worker,
    // before its first native fetch. The existing hook changes no command or
    // answer, and neither the cache nor receiver receives this configuration.
    // Every later native read of this worker therefore uses the same protocol
    // whose raw-blob refusal and exact-commit success were established above.
    let mut failures = Vec::new();
    for mode in ["cold", "mixed"] {
        let cache = world.path().join(format!("full-source-cache-{mode}.git"));
        let full = world.path().join(format!("full-source-{mode}.git"));
        for dir in [&cache, &full] {
            host_git(
                world.path(),
                &["init", "--quiet", "--bare", dir.to_str().unwrap()],
            );
        }
        if mode == "mixed" {
            gh.run_git(&GitRun::fetch_main(&slug, &cache, "refs/agh/metadata").without_blobs())
                .unwrap();
            let imported = git_input(&cache, &["hash-object", "-w", "--stdin"], Some(body));
            assert!(imported.status.success());
            assert_eq!(
                String::from_utf8(imported.stdout).unwrap(),
                format!("{blob}\n")
            );
            assert!(
                git_input(&cache, &["cat-file", "-e", &blob], None)
                    .status
                    .success()
            );
            for absent in [&other_blob, &retired_blob] {
                assert!(
                    !git_input(&cache, &["cat-file", "-e", absent], None)
                        .status
                        .success()
                );
            }
        }
        let default_policy = git_input(
            &cache,
            &["config", "--get", "uploadpack.allowAnySHA1InWant"],
            None,
        );
        assert_eq!(
            default_policy.status.code(),
            Some(1),
            "genuine default cache policy absence"
        );
        assert!(default_policy.stdout.is_empty());
        let before = super::source_cache_tests::store_bytes(&cache);
        let receiver_before = super::source_cache_tests::store_bytes(&full);
        let effects: Arc<OnceLock<Weak<Fx>>> = Arc::new(OnceLock::new());
        let observed = Arc::clone(&effects);
        let after_metadata = Arc::clone(&effects);
        let observed_children = Arc::clone(&effects);
        let private_parent: Arc<OnceLock<PathBuf>> = Arc::new(OnceLock::new());
        let parent_by_hook = Arc::clone(&private_parent);
        let parent_by_observer = Arc::clone(&private_parent);
        let remote_endpoint = remote.clone();
        let probe = format!("{commit}\n{other_blob}\n{retired_blob}\n");
        let expected_probe =
            format!("{commit} commit\n{other_blob} missing\n{retired_blob} missing\n");
        let set = Arc::new(AtomicBool::new(false));
        let set_by_hook = Arc::clone(&set);
        let fx = Arc::new(Fx::simulated(live_start())
            .with_git_child_observer_for_tests(move |span| {
                let fx = observed_children.get().and_then(Weak::upgrade).unwrap();
                let calls = fx.children_run();
                let spec = &calls[span.ordinal];
                if spec.args.first().is_some_and(|arg| arg == "init")
                    && spec.args.last().is_some_and(|arg| Path::new(arg).file_name()
                        .is_some_and(|name| name == "full-source-body.git")) {
                    assert_eq!(span.code, Some(0), "actual fresh body worker init");
                    let body = Path::new(spec.args.last().unwrap());
                    assert_eq!(body.parent(), parent_by_observer.get().map(PathBuf::as_path),
                        "body worker belongs to the SAME private input owner");
                    host_git(body, &["config", "protocol.version", "0"]);
                }
                if spec.args.iter().any(|arg| arg == remote_endpoint.as_str()) {
                    assert!(!spec.args.iter().any(|arg| arg.to_string_lossy()
                        .starts_with("protocol.version=")), "no protocol override can mask the route");
                    assert_eq!(host_git(spec.cwd.as_deref().unwrap(),
                        &["config", "--get", "protocol.version"]), "0",
                        "EVERY actual remote fetch, including a new worker, negotiates v0");
                    eprintln!("actual FullSource {mode} v0 remote argv {:?}, code {:?}", spec.args, span.code);
                }
            })
            .with_git_intercept_once_for_tests(
            metadata_fetch.program.clone(), metadata_fetch.args.clone(),
            move || {
                let fx = observed.get().and_then(Weak::upgrade).unwrap();
                let calls = fx.children_run();
                let worker = calls.last().unwrap().cwd.as_deref().unwrap();
                assert!(parent_by_hook.set(worker.parent().unwrap().to_path_buf()).is_ok());
                host_git(worker, &["config", "protocol.version", "0"]);
                assert_eq!(host_git(worker, &["config", "--get", "protocol.version"]), "0");
                set_by_hook.store(true, Ordering::SeqCst);
            }, move |result| {
                assert_eq!(result.as_ref().unwrap().code, Some(0), "native v0 metadata fetch");
                let fx = after_metadata.get().and_then(Weak::upgrade).unwrap();
                let calls = fx.children_run();
                let worker = calls.last().unwrap().cwd.as_deref().unwrap();
                let actual = presence(worker, probe.as_bytes());
                assert!(actual.status.success());
                assert_eq!(actual.stdout, expected_probe.as_bytes(),
                    "the real metadata worker has the root but lacks required current/historical bodies");
            },
        ));
        assert!(effects.set(Arc::downgrade(&fx)).is_ok());
        let source = world
            .client(git_env())
            .fx(fx)
            .git_remote_for_tests("fixture-dev/app", &bare)
            .build();
        let result = source.run_git(&GitRun::fetch_ref(
            &slug,
            &full,
            &commit,
            "refs/agh/full-source",
            false,
            agh::git::FetchScope::FullSource {
                cache: cache.clone(),
            },
        ));
        assert!(
            set.load(Ordering::SeqCst),
            "{mode}: real worker protocol control reached"
        );
        assert_eq!(
            super::source_cache_tests::store_bytes(&cache),
            before,
            "{mode}: read-only cache"
        );
        match result {
            Err(error) => {
                assert_eq!(
                    super::source_cache_tests::store_bytes(&full),
                    receiver_before,
                    "{mode}: failed hydration cannot publish objects or refs"
                );
                failures.push(format!("{mode}: {error:?}"));
            }
            Ok(_) => {
                assert_eq!(
                    host_git(&full, &["rev-parse", "refs/agh/full-source"]),
                    commit
                );
                for (id, bytes) in [
                    (&blob, body.as_slice()),
                    (&other_blob, other_body.as_slice()),
                    (&retired_blob, retired_body.as_slice()),
                ] {
                    let actual = git_input(&full, &["cat-file", "blob", id], None);
                    assert!(actual.status.success(), "{mode}: complete original history");
                    assert_eq!(actual.stdout, bytes);
                }
                host_git(
                    &full,
                    &["fsck", "--full", "--strict", "--no-dangling", &commit],
                );
                assert!(!full.join("shallow").exists());
                let calls = source.fx().children_run();
                let exact_root = calls
                    .iter()
                    .find(|spec| {
                        spec.args.iter().any(|arg| arg == remote.as_str())
                            && !spec.args.iter().any(|arg| arg == "--filter=blob:none")
                    })
                    .expect("actual authenticated full-root argv");
                assert!(!exact_root.args.iter().any(|arg| arg == "--stdin"
                    || arg.to_string_lossy().starts_with("--depth")),
                    "full history is requested by commit, with no shallow bound");
                // As in the original checkout control, independently replay the
                // ACTUAL emitted argv with native v0 packet tracing. This is a
                // wire control, never a retry or replacement for the API result.
                let wire = world.path().join(format!("full-source-wire-{mode}.git"));
                host_git(
                    world.path(),
                    &["init", "--quiet", "--bare", wire.to_str().unwrap()],
                );
                let trace = wire.join("native-packets.data");
                let mut args = vec![OsString::from("-c"), OsString::from("protocol.version=0")];
                args.extend(exact_root.args.clone());
                let actual = emitted_git_input(&wire, exact_root, &args, None, Some(&trace));
                assert!(
                    actual.status.success(),
                    "{mode}: native v0 exact-root replay: {}",
                    String::from_utf8_lossy(&actual.stderr)
                );
                let packets = std::fs::read_to_string(trace).unwrap();
                assert!(
                    packets
                        .lines()
                        .any(|line| line.contains(&format!("fetch> want {commit}")))
                );
                assert!(
                    !packets.lines().any(|line| line.contains("fetch> have ")),
                    "a filtered metadata commit is never a complete have"
                );
                for id in [&blob, &other_blob, &retired_blob] {
                    assert!(
                        !packets
                            .lines()
                            .any(|line| line.contains(&format!("fetch> want {id}"))),
                        "no raw blob permission is needed"
                    );
                }
                host_git(
                    &wire,
                    &["fsck", "--full", "--strict", "--no-dangling", &commit],
                );
            }
        }
    }
    assert!(
        failures.is_empty(),
        "actual FullSource protocol-v0 failures: {failures:#?}"
    );
}

/// Compare exactly the two native request shapes and two original scratch states.
/// The metadata state is produced by the same filtered native fetch as Agh uses,
/// and selected blob absence is established with lazy fetch disabled.
#[test]
fn raw_blob_wants_compare_filtered_and_fresh_native_scratch() {
    let mut world = KeptWorld(Some(World::canonical("git-raw-blob-matrix")));
    let bare = bare_repo(world.path(), "provider");
    let work = world.path().join("provider-work");
    let body = b"the exact selected blob body\n";
    std::fs::write(work.join("selected"), body).unwrap();
    host_git(&work, &["add", "selected"]);
    host_git(&work, &["commit", "--quiet", "-m", "selected body"]);
    host_git(&work, &["push", "--quiet", bare.to_str().unwrap(), "main"]);
    host_git(&bare, &["config", "uploadpack.allowFilter", "true"]);
    host_git(
        &bare,
        &["config", "uploadpack.allowReachableSHA1InWant", "true"],
    );
    let commit = host_git(&bare, &["rev-parse", "main"]);
    let blob = host_git(&bare, &["rev-parse", "main:selected"]);
    let remote = format!("file://{}", bare.display());
    let version = host_git(world.path(), &["--version"]);
    let mut rows = Vec::new();

    for filtered in [true, false] {
        for mapped in [false, true] {
            let label = format!("filtered-{filtered}-mapped-{mapped}");
            let scratch = world.path().join(&label);
            host_git(
                world.path(),
                &["init", "--quiet", "--bare", scratch.to_str().unwrap()],
            );
            if filtered {
                let spec = format!("+{commit}:refs/agh/source");
                let initial = git_input(
                    &scratch,
                    &[
                        "-c",
                        "protocol.version=2",
                        "fetch",
                        "--quiet",
                        "--no-tags",
                        "--no-write-fetch-head",
                        "--recurse-submodules=no",
                        "--filter=blob:none",
                        &remote,
                        &spec,
                    ],
                    None,
                );
                assert!(
                    initial.status.success(),
                    "metadata setup: {}",
                    String::from_utf8_lossy(&initial.stderr).into_owned()
                );
                assert_eq!(
                    host_git(&scratch, &["rev-parse", "refs/agh/source"]),
                    commit
                );
                assert!(
                    std::fs::read_dir(scratch.join("objects/pack"))
                        .unwrap()
                        .any(|entry| entry
                            .unwrap()
                            .path()
                            .extension()
                            .is_some_and(|extension| extension == "promisor"))
                );
            }
            let inspect = format!("{blob}\n");
            // The filtered scratch holds its commit, which the view must see.
            let (asked, seen) = if filtered {
                (format!("{commit}\n{blob}\n"), format!("{commit} commit\n"))
            } else {
                (inspect.clone(), String::new())
            };
            let before = presence(&scratch, asked.as_bytes());
            assert!(
                before.status.success(),
                "{}",
                String::from_utf8_lossy(&before.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&before.stdout).into_owned(),
                format!("{seen}{blob} missing\n")
            );
            let refs_before = host_git(&scratch, &["for-each-ref"]);
            let request = if mapped {
                format!("+{blob}:refs/agh/blob/0\n")
            } else {
                inspect.clone()
            };
            let fetched = git_input(
                &scratch,
                &[
                    "-c",
                    "protocol.version=2",
                    "fetch",
                    "--quiet",
                    "--no-tags",
                    "--no-write-fetch-head",
                    "--recurse-submodules=no",
                    "--stdin",
                    &remote,
                ],
                Some(request.as_bytes()),
            );
            let after = git_input(
                &scratch,
                &["cat-file", "--batch-check=%(objectname) %(objecttype)"],
                Some(inspect.as_bytes()),
            );
            let body_after = git_input(&scratch, &["cat-file", "blob", &blob], None);
            let refs_after = host_git(&scratch, &["for-each-ref"]);
            let complete = fetched.status.success()
                && after.status.success()
                && after.stdout == format!("{blob} blob\n").as_bytes()
                && body_after.status.success()
                && body_after.stdout == body;
            rows.push(aterm_json::json!({
                "label":label, "filtered":filtered, "mapped":mapped,
                "code":fetched.status.code(), "stdout":String::from_utf8_lossy(&fetched.stdout).into_owned(),
                "stderr":String::from_utf8_lossy(&fetched.stderr).into_owned(),
                "object_inspection":String::from_utf8_lossy(&after.stdout).into_owned(),
                "body_code":body_after.status.code(), "body":String::from_utf8_lossy(&body_after.stdout).into_owned(),
                "refs_before":refs_before, "refs_after":refs_after,
                "config":std::fs::read_to_string(scratch.join("config")).unwrap(),
                "complete":complete,
            }));
        }
    }
    let report = aterm_json::json!({
        "git":host_git_path(), "version":version, "protocol":2, "commit":commit, "blob":blob, "cases":rows
    });
    std::fs::write(
        world.path().join("matrix.json"),
        aterm_json::to_vec(&report).unwrap(),
    )
    .unwrap();
    eprintln!("real Git raw-blob matrix: {report}");
    assert!(
        rows.iter()
            .all(|row| row["complete"].as_bool() == Some(true)),
        "raw blob matrix differs; world={}",
        world.path().display()
    );
    let kept = world.0.take().unwrap();
    eprintln!("raw-blob matrix retained: {}", kept.root.keep().display());
}

/// Re-run the native argv emitted by the actual runner in another owned scratch.
/// The Git image and request bytes are unchanged; only a local packet-trace sink is added.
fn emitted_git_input(
    dir: &Path,
    child: &ChildSpec,
    args: &[OsString],
    input: Option<&[u8]>,
    trace: Option<&Path>,
) -> std::process::Output {
    let mut command = aterm_uds::spawnfd::command(&child.program);
    command
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_GRAFT_FILE", "/dev/null");
    if let Some(trace) = trace {
        command.env("GIT_TRACE_PACKET", trace);
    }
    let stdin = input.map_or_else(aterm_uds::spawnfd::Stdio::null, |bytes| {
        let path = dir.join("emitted-request-data");
        std::fs::write(&path, bytes).unwrap();
        aterm_uds::spawnfd::Stdio::from(std::fs::File::open(path).unwrap())
    });
    aterm_uds::deadline::output_until_from(
        &mut command,
        stdin,
        std::time::Instant::now() + common::HANG,
    )
    .unwrap_or_else(|error| panic!("emitted native Git in {}: {error:?}", dir.display()))
}

/// Bind packet evidence to the runner's real emitted metadata and commit-body
/// argv. The body repository is cold and requests the exact commit, while a
/// deliberately borrowed-history control establishes the wire's have observer.
#[test]
fn checkout_commit_bodies_do_not_advertise_filtered_history() {
    let world = KeptWorld(Some(World::canonical("git-checkout-blob-negotiation")));
    let bare = bare_repo(world.path(), "provider");
    let work = world.path().join("provider-work");
    let parent = host_git(&bare, &["rev-parse", "main"]);
    let body = b"selected source bytes for the native negotiation control\n";
    std::fs::write(work.join("selected"), body).unwrap();
    host_git(&work, &["add", "selected"]);
    host_git(&work, &["commit", "--quiet", "-m", "selected source"]);
    host_git(&work, &["push", "--quiet", bare.to_str().unwrap(), "main"]);
    host_git(&bare, &["config", "uploadpack.allowFilter", "true"]);
    host_git(
        &bare,
        &["config", "uploadpack.allowReachableSHA1InWant", "true"],
    );
    let commit = host_git(&bare, &["rev-parse", "main"]);
    let blob = host_git(&bare, &["rev-parse", "main:selected"]);
    let destination = world.path().join("destination.git");
    host_git(
        world.path(),
        &["init", "--quiet", "--bare", destination.to_str().unwrap()],
    );
    let gh = world
        .client(git_env())
        .git_remote_for_tests("fixture-dev/app", &bare)
        .build();
    let slug = common::slug("fixture-dev/app");
    gh.run_git(&GitRun::fetch_ref(
        &slug,
        &destination,
        &commit,
        "refs/agh/selected",
        false,
        agh::git::FetchScope::SelectedCheckout { cache: None },
    ))
    .unwrap_or_else(|error| panic!("actual checkout closure: {}", error.why));
    assert_eq!(
        host_git(&destination, &["rev-parse", "refs/agh/selected"]),
        commit
    );
    let children = gh.fx().children_run();
    let metadata = children
        .iter()
        .find(|child| child.args.iter().any(|arg| arg == "--filter=blob:none"))
        .expect("the actual filtered metadata fetch");
    let body_fetch = children
        .iter()
        .find(|child| child.args.iter().any(|arg| arg == "--depth=1"))
        .expect("the actual shallow exact-commit body fetch");
    assert!(!body_fetch.args.iter().any(|arg| arg == "--stdin"));
    assert!(
        body_fetch
            .args
            .iter()
            .any(|arg| arg == format!("+{commit}:refs/agh/checkout-body").as_str())
    );
    let mut observations = Vec::new();
    for production in [false, true] {
        let scratch = world
            .path()
            .join(format!("wire-production-{production}.git"));
        host_git(
            world.path(),
            &["init", "--quiet", "--bare", scratch.to_str().unwrap()],
        );
        let setup = emitted_git_input(&scratch, metadata, &metadata.args, None, None);
        assert!(
            setup.status.success(),
            "metadata setup: {}",
            String::from_utf8_lossy(&setup.stderr)
        );
        let before = presence(&scratch, format!("{commit}\n{blob}\n").as_bytes());
        assert!(
            before.status.success(),
            "{}",
            String::from_utf8_lossy(&before.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&before.stdout).into_owned(),
            format!("{commit} commit\n{blob} missing\n")
        );
        let body_scratch = world.path().join(format!("wire-body-{production}.git"));
        host_git(
            world.path(),
            &[
                "init",
                "--quiet",
                "--bare",
                "--template=",
                body_scratch.to_str().unwrap(),
            ],
        );
        let mut args = body_fetch.args.clone();
        if !production {
            let remote = format!("file://{}", bare.display());
            let spec = format!("+{parent}:refs/agh/borrowed-history");
            let borrowed = git_input(
                &body_scratch,
                &["fetch", "--quiet", "--no-tags", &remote, &spec],
                None,
            );
            assert!(borrowed.status.success());
            let at = args
                .windows(2)
                .position(|pair| pair[0] == "-c" && pair[1] == "fetch.negotiationAlgorithm=noop")
                .expect("production explicitly refuses negotiation");
            args.drain(at..at + 2);
            args.retain(|arg| arg != "--depth=1");
        }
        let trace = body_scratch.join("native-packets.data");
        let fetched = emitted_git_input(&body_scratch, body_fetch, &args, None, Some(&trace));
        assert!(
            fetched.status.success(),
            "commit body fetch: {}",
            String::from_utf8_lossy(&fetched.stderr)
        );
        let received = git_input(&body_scratch, &["cat-file", "blob", &blob], None);
        assert!(received.status.success());
        assert_eq!(received.stdout, body);
        let packets = std::fs::read_to_string(trace).unwrap();
        let wants = packets
            .lines()
            .filter(|line| line.contains(&format!("fetch> want {commit}")))
            .count();
        let haves = packets
            .lines()
            .filter(|line| line.contains("fetch> have "))
            .count();
        assert!(
            wants > 0,
            "the exact commit want must reach the native wire"
        );
        assert!(
            !packets
                .lines()
                .any(|line| line.contains(&format!("fetch> want {blob}"))),
            "the provider sees no raw blob want"
        );
        if production {
            assert_eq!(haves, 0, "filtered commits are not complete-history haves");
        } else {
            assert!(
                haves > 0,
                "the borrowed-history control must expose negotiation"
            );
        }
        assert!(host_git(&body_scratch, &["for-each-ref", "refs/agh/"]).contains(&commit));
        assert!(!body_scratch.join("objects/info/alternates").exists());
        observations.push((production, wants, haves));
    }
    eprintln!("actual emitted checkout commit negotiation: {observations:?}");
}
