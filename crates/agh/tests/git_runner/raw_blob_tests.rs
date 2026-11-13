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
fn git_input(dir: &Path, args: &[&str], input: Option<&[u8]>) -> std::process::Output {
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

/// Filtered ancestry is readable object data, not a complete-have assertion.
/// Bind the native wire check to the runner's real emitted metadata/blob argv.
#[test]
fn checkout_blob_wants_do_not_advertise_filtered_history() {
    let world = KeptWorld(Some(World::canonical("git-checkout-blob-negotiation")));
    let bare = bare_repo(world.path(), "provider");
    let work = world.path().join("provider-work");
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
    gh.run_git(
        &GitRun::fetch_ref(&slug, &destination, &commit, "refs/agh/selected", false)
            .with_checkout_closure(None),
    )
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
    let blob_fetch = children
        .iter()
        .find(|child| child.args.iter().any(|arg| arg == "--stdin"))
        .expect("the actual explicit missing-blob fetch");
    let request = format!("{blob}\n");
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
        let mut args = blob_fetch.args.clone();
        if !production
            && let Some(at) = args
                .windows(2)
                .position(|pair| pair[0] == "-c" && pair[1] == "fetch.negotiationAlgorithm=noop")
        {
            args.drain(at..at + 2);
        }
        let trace = scratch.join("native-packets.data");
        let fetched = emitted_git_input(
            &scratch,
            blob_fetch,
            &args,
            Some(request.as_bytes()),
            Some(&trace),
        );
        assert!(
            fetched.status.success(),
            "blob fetch: {}",
            String::from_utf8_lossy(&fetched.stderr)
        );
        let received = git_input(&scratch, &["cat-file", "blob", &blob], None);
        assert!(received.status.success());
        assert_eq!(received.stdout, body);
        let packets = std::fs::read_to_string(trace).unwrap();
        let wants = packets
            .lines()
            .filter(|line| line.contains(&format!("fetch> want {blob}")))
            .count();
        let haves = packets
            .lines()
            .filter(|line| line.contains("fetch> have "))
            .count();
        assert!(wants > 0, "the exact blob want must reach the native wire");
        if production {
            assert_eq!(haves, 0, "filtered commits are not complete-history haves");
        } else {
            assert!(
                haves > 0,
                "the omitted-noop control must expose negotiation"
            );
        }
        assert!(host_git(&scratch, &["for-each-ref", "refs/agh/"]).contains(&commit));
        observations.push((production, wants, haves));
    }
    eprintln!("actual emitted checkout blob negotiation: {observations:?}");
}
