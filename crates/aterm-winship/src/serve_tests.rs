// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `release-serve` against a local bare origin: requests the release cutter would write,
//! a build injected in place of `release-build`, and the handoff read back the way the cutter
//! reads it.

use std::cell::Cell;
use std::time::{Duration, Instant};

use super::*;
use crate::handoff::{Lane, fixture};

const VERSION: &str = "0.200.0";
const BUILD: u64 = 1_794_421_328;
const BUILDER: &str = "m6-hp-32gb-1tb";
const HANG: Duration = Duration::from_secs(60);

/// git in `dir`, isolated from this machine's configuration, with `env` set; its output.
fn git_in(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Result<Vec<u8>, String> {
    let mut cmd = aterm_uds::fixture_git::git();
    cmd.arg("-C").arg(dir).args(args);
    cmd.envs(env.iter().copied());
    let out = aterm_uds::deadline::output_within(&mut cmd, HANG).map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(format!(
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        ))
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    String::from_utf8(git_in(dir, args, &[]).unwrap_or_else(|e| panic!("{e}")))
        .expect("utf8")
        .trim()
        .to_string()
}

fn write(dir: &Path, name: &str, text: &str) {
    let path = dir.join(name);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
    std::fs::write(path, text).expect("write");
}

fn pins(public: &str, not_after: &str) -> String {
    format!(
        "[[builder]]\nid = \"{BUILDER}\"\npublic_key = \"{public}\"\nnot_after = \"{not_after}\"\n"
    )
}

/// The release commit's edits of `version` at build `build`: the ledger line, the changelog
/// section, the shipped entry file deleted.
fn release_edits(dir: &Path, build: u64, version: &str) {
    let ledger = std::fs::read_to_string(dir.join("RELEASES.ledger")).expect("ledger");
    write(
        dir,
        "RELEASES.ledger",
        &format!("{ledger}{build} {version}\n"),
    );
    write(
        dir,
        "CHANGELOG.md",
        &format!("# Changelog\n\n## [{version}] - 2026-11-15\n\n- one\n"),
    );
    std::fs::remove_file(dir.join("changes").join("one.md")).expect("rm");
}

/// A bare origin whose `main` is a source commit S, a working repository `seed` that makes
/// commits, the release commit C (S plus the release edits, on no branch of origin), and a
/// clone `repo` the poll runs in.
struct Fixture {
    scratch: aterm_tempfile::TempDir,
    origin: PathBuf,
    seed: PathBuf,
    repo: PathBuf,
    layout: Layout,
    key: Vec<u8>,
    source: String,
    release: String,
    tree: String,
    polls: Cell<usize>,
}

impl Fixture {
    /// `pin`: whether the source pins this builder's key.
    fn new(pin: bool) -> Self {
        let scratch = aterm_tempfile::tempdir().expect("tempdir");
        let root = scratch.path().to_path_buf();
        let key = attest::generate().expect("key");
        let public = attest::public_key(&key).expect("public");
        let origin = root.join("origin.git");
        let seed = root.join("seed");
        git(
            &root,
            &[
                "init",
                "--quiet",
                "--bare",
                "--initial-branch=main",
                "origin.git",
            ],
        );
        git(&root, &["init", "--quiet", "--initial-branch=main", "seed"]);
        write(
            &seed,
            "Cargo.toml",
            "[workspace.package]\nversion = \"0.200.0\"\n",
        );
        write(
            &seed,
            "RELEASES.ledger",
            "# build version\n1783354739 0.25.0\n",
        );
        write(&seed, "CHANGELOG.md", "# Changelog\n");
        write(&seed, "changes/one.md", "- one\n");
        write(
            &seed,
            "crates/aterm-toolchain/src/stock_pin.rs",
            &format!("pub const STOCK_PIN: &str = \"{}\";\n", fixture::STOCK),
        );
        write(
            &seed,
            pull::BUILDERS_FILE,
            &if pin {
                pins(&public, "2999-12-31")
            } else {
                "# none\n".to_string()
            },
        );
        git(&seed, &["add", "-A"]);
        git(
            &seed,
            &["-c", "user.name=t", "commit", "--quiet", "-m", "source"],
        );
        let source = git(&seed, &["rev-parse", "HEAD"]);
        git(
            &seed,
            &["remote", "add", "origin", &origin.to_string_lossy()],
        );
        git(&seed, &["push", "--quiet", "origin", "main"]);
        release_edits(&seed, BUILD, VERSION);
        git(
            &seed,
            &[
                "-c",
                "user.name=t",
                "commit",
                "--quiet",
                "-a",
                "-m",
                "release",
            ],
        );
        let release = git(&seed, &["rev-parse", "HEAD"]);
        let tree = git(&seed, &["rev-parse", "HEAD^{tree}"]);
        git(&root, &["clone", "--quiet", "origin.git", "repo"]);
        Self {
            layout: Layout {
                local: root.join("local"),
            },
            repo: root.join("repo"),
            scratch,
            origin,
            seed,
            key,
            source,
            release,
            tree,
            polls: Cell::new(0),
        }
    }

    fn request(&self) -> Request {
        Request {
            version: VERSION.to_string(),
            build: BUILD,
            commit: self.release.clone(),
            tree: self.tree.clone(),
            reserved_at: crate::pointer::now_rfc3339(),
        }
    }

    /// A commit in `seed` of `files` at its root alone, with `parents`.
    fn commit_tree(&self, files: &[(&str, &str)], parents: &[&str]) -> String {
        let index = self.scratch.path().join("fixture.index");
        let _ = std::fs::remove_file(&index);
        let index_text = index.to_string_lossy().into_owned();
        let env = [("GIT_INDEX_FILE", index_text.as_str())];
        for (name, text) in files {
            let blob = self.scratch.path().join("blob.tmp");
            std::fs::write(&blob, text).expect("blob");
            let oid = git(
                &self.seed,
                &["hash-object", "-w", "--no-filters", &blob.to_string_lossy()],
            );
            git_in(
                &self.seed,
                &[
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    &format!("100644,{oid},{name}"),
                ],
                &env,
            )
            .expect("index");
        }
        let tree = String::from_utf8(git_in(&self.seed, &["write-tree"], &env).expect("tree"))
            .expect("utf8")
            .trim()
            .to_string();
        let mut args = vec!["-c", "user.name=t", "commit-tree", tree.as_str(), "-m", "t"];
        for parent in parents {
            args.extend(["-p", parent]);
        }
        git(&self.seed, &args)
    }

    /// A commit in `seed` on `base` with `edit`'s changes.
    fn commit_on(&self, base: &str, edit: impl FnOnce(&Path)) -> String {
        git(
            &self.seed,
            &["checkout", "--quiet", "--force", "--detach", base],
        );
        git(&self.seed, &["clean", "-fdq"]);
        edit(&self.seed);
        git(&self.seed, &["add", "-A"]);
        git(
            &self.seed,
            &["-c", "user.name=t", "commit", "--quiet", "-m", "variant"],
        );
        git(&self.seed, &["rev-parse", "HEAD"])
    }

    /// Writes `request` as origin's request, a commit with `parents` (C's when empty).
    fn ask(&self, request: &Request, parents: &[&str]) {
        let text = request.to_toml().expect("toml");
        let parents: Vec<&str> = if parents.is_empty() {
            vec![request.commit.as_str()]
        } else {
            parents.to_vec()
        };
        let commit = self.commit_tree(&[(pull::REQUEST_FILE, &text)], &parents);
        self.push_request(&commit);
    }

    fn push_request(&self, commit: &str) {
        git(
            &self.seed,
            &[
                "push",
                "--quiet",
                "--force",
                "origin",
                &format!("{commit}:{}", pull::REQUEST_REF),
            ],
        );
    }

    /// Origin's heartbeat, read as the cut reads it.
    fn alive(&self) -> Alive {
        git(
            &self.repo,
            &[
                "fetch",
                "--quiet",
                "origin",
                &format!("+{}:refs/test/alive", pull::ALIVE_REF),
            ],
        );
        Alive::parse(&git(
            &self.repo,
            &["show", &format!("refs/test/alive:{}", pull::ALIVE_FILE)],
        ))
        .expect("alive")
    }

    /// The request commit origin's request ref names.
    fn asked(&self) -> String {
        self.remote(pull::REQUEST_REF).expect("a request")
    }

    /// What origin's `refname` names.
    fn remote(&self, refname: &str) -> Option<String> {
        let listing = git(
            self.scratch.path(),
            &["ls-remote", &self.origin.to_string_lossy(), refname],
        );
        listing.lines().find_map(|l| {
            l.split_once('\t')
                .filter(|(_, n)| *n == refname)
                .map(|(o, _)| o.to_string())
        })
    }

    fn serve_git(&self) -> Git {
        Git {
            dir: self.repo.clone(),
            env: [
                ("GIT_CONFIG_NOSYSTEM", "1"),
                ("GIT_CONFIG_GLOBAL", "/dev/null"),
                ("XDG_CONFIG_HOME", "/dev/null"),
                ("GIT_TERMINAL_PROMPT", "0"),
            ]
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .to_vec(),
        }
    }

    /// One poll with `build`, the heartbeat every `every` while it builds: its exit code and
    /// what it said.
    fn poll(&self, build: &mut Stub, every: Duration) -> (i32, String) {
        let n = self.polls.get();
        self.polls.set(n + 1);
        let log = self.scratch.path().join(format!("poll-{n}.log"));
        let mut out = Out::logged(&log).expect("log");
        let mut serve = Serve {
            git: self.serve_git(),
            layout: &self.layout,
            key: &self.key,
            builder: BUILDER,
            heartbeat_every: every,
            build,
        };
        let code = serve_once(&mut serve, &mut out);
        drop(out);
        (code, std::fs::read_to_string(&log).expect("log"))
    }
}

/// The build `release-build` stands for: the fixture's portable zip and a receipt of the
/// request, written where it is asked.
#[derive(Default)]
struct Stub {
    calls: usize,
    fail: bool,
    /// Write the receipt of another build number.
    another_build: bool,
    /// Before writing, wait until origin's heartbeat moves past the one the poll pushed.
    beat_on: Option<(PathBuf, PathBuf)>,
}

impl Build for Stub {
    fn build(&mut self, request: &Request, out: &Path, _log: &mut Out) -> Result<(), String> {
        self.calls += 1;
        if self.fail {
            return Err("the stub failed".into());
        }
        if let Some((scratch, origin)) = &self.beat_on {
            let alive = || {
                git(
                    scratch,
                    &["ls-remote", &origin.to_string_lossy(), pull::ALIVE_REF],
                )
            };
            let first = alive();
            let by = Instant::now() + HANG;
            while alive() == first {
                assert!(Instant::now() < by, "no heartbeat while the build ran");
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
        let zip = fixture::portable();
        let build = if self.another_build {
            request.build + 1
        } else {
            request.build
        };
        let cut = Cut {
            version: &request.version,
            build,
            commit: &request.commit,
            source_tree: &request.tree,
        };
        let receipt = fixture::receipt(&zip, &cut, Lane::Stock, &"0".repeat(40));
        fixture::carry(out, &zip, &receipt)
    }
}

const QUIET: Duration = Duration::from_secs(3600);

#[test]
fn a_request_is_built_signed_handed_off_and_admitted_as_the_cutter_reads_it() {
    let fx = Fixture::new(true);
    let request = fx.request();
    fx.ask(&request, &[]);
    let mut stub = Stub::default();
    let (code, said) = fx.poll(&mut stub, QUIET);
    assert_eq!(code, PASS, "{said}");
    assert_eq!(stub.calls, 1, "{said}");
    assert!(said.contains("handed off as"), "{said}");

    // The handoff, written out and put back together as the cutter does.
    let refname = request.handoff_ref();
    assert!(fx.remote(&refname).is_some(), "{said}");
    git(
        &fx.repo,
        &[
            "fetch",
            "--quiet",
            "origin",
            &format!("{refname}:refs/test/handoff"),
        ],
    );
    let parts_dir = fx.scratch.path().join("parts");
    let carry = fx.scratch.path().join("carry");
    for dir in [&parts_dir, &carry] {
        std::fs::create_dir(dir).expect("dir");
    }
    let mut read = |args: &[&str]| git_in(&fx.repo, args, &[]);
    let names = pull::extract(&mut read, "refs/test/handoff", &parts_dir).expect("extracted");
    assert_eq!(names.len(), 4, "{names:?}");
    let parts = pull::assemble(&parts_dir, &carry).expect("assembled");
    assert_eq!(
        std::fs::read(carry.join(&parts.zip)).expect("zip"),
        fixture::portable()
    );
    let cut = Cut {
        version: VERSION,
        build: BUILD,
        commit: &fx.release,
        source_tree: &fx.tree,
    };
    let pins = Pins {
        stock: fixture::STOCK,
        trust_commit: None,
    };
    let receipt = handoff::admit(&carry, &cut, &pins).expect("admitted");
    assert_eq!(receipt.sha256, parts.sha256);
    let pinned = Builders::parse(&git(
        &fx.repo,
        &["show", &format!("{}:{}", fx.release, pull::BUILDERS_FILE)],
    ))
    .expect("pins");
    let attested =
        attest::verify_beside(&carry, &receipt.asset, &pinned, &pull::today()).expect("attested");
    assert_eq!(attested.builder, BUILDER);

    // The heartbeat: a parentless commit of alive.toml, fresh.
    git(
        &fx.repo,
        &[
            "fetch",
            "--quiet",
            "origin",
            &format!("+{}:refs/test/alive", pull::ALIVE_REF),
        ],
    );
    assert_eq!(
        git(
            &fx.repo,
            &["rev-list", "--parents", "-n", "1", "refs/test/alive"]
        )
        .split_whitespace()
        .count(),
        1
    );
    let alive = Alive::parse(&git(
        &fx.repo,
        &["show", &format!("refs/test/alive:{}", pull::ALIVE_FILE)],
    ))
    .expect("alive");
    assert_eq!(alive.builder, BUILDER);
    assert!(alive.fresh(pull::now_secs(), 600), "{alive:?}");
    // It names the request it read, which it builds.
    assert_eq!(alive.request.as_deref(), Some(fx.asked().as_str()));
    assert_eq!(alive.passed_over, None, "{alive:?}");

    // A second poll finds the handoff there and builds nothing.
    let (code, said) = fx.poll(&mut stub, QUIET);
    assert_eq!(code, PASS, "{said}");
    assert_eq!(stub.calls, 1, "{said}");
    assert!(said.contains("on origin already"), "{said}");
}

#[test]
fn with_no_request_a_pinned_builder_only_heartbeats() {
    let fx = Fixture::new(true);
    let mut stub = Stub::default();
    let (code, said) = fx.poll(&mut stub, QUIET);
    assert_eq!(code, PASS, "{said}");
    assert_eq!(stub.calls, 0);
    assert!(said.contains("no request"), "{said}");
    assert!(fx.remote(pull::ALIVE_REF).is_some(), "{said}");
}

#[test]
fn an_unpinned_builder_neither_serves_nor_heartbeats() {
    let fx = Fixture::new(false);
    fx.ask(&fx.request(), &[]);
    let mut stub = Stub::default();
    let (code, said) = fx.poll(&mut stub, QUIET);
    assert_eq!(code, NOT_RUN, "{said}");
    assert_eq!(stub.calls, 0);
    assert_eq!(fx.remote(pull::ALIVE_REF), None, "{said}");
    // It says what to pin.
    let public = attest::public_key(&fx.key).expect("public");
    assert!(said.contains(&public), "{said}");
    assert!(said.contains(pull::BUILDERS_FILE), "{said}");
}

/// A RELEASE COMMIT THAT DOES NOT PIN THIS BUILDER IS PASSED OVER, AND THE HEARTBEAT SAYS SO: the
/// builder serves (origin/main pins it), but a release cut from before the pin would never admit
/// its attestation, so it builds nothing and tells the cut, which then waits no longer.
/// NEGATIVE CONTROL: a heartbeat that said only "alive" kept such a cut waiting out the whole
/// wait for a handoff no poll would make.
#[test]
fn a_release_commit_that_does_not_pin_this_builder_is_passed_over() {
    let fx = Fixture::new(false);
    let public = attest::public_key(&fx.key).expect("public");
    let pinned = fx.commit_on(&fx.source, |dir| {
        write(dir, pull::BUILDERS_FILE, &pins(&public, "2999-12-31"));
    });
    git(
        &fx.seed,
        &[
            "push",
            "--quiet",
            "origin",
            &format!("{pinned}:refs/heads/main"),
        ],
    );
    // The release commit is cut from the source before the pin.
    fx.ask(&fx.request(), &[]);
    let mut stub = Stub::default();
    let (code, said) = fx.poll(&mut stub, QUIET);
    assert_eq!(code, NOT_RUN, "{said}");
    assert_eq!(stub.calls, 0, "{said}");
    assert!(said.contains("does not admit this builder"), "{said}");
    let alive = fx.alive();
    assert_eq!(
        alive.passed_over(&fx.asked()),
        Some(PassedOver::Unpinned),
        "{alive:?}"
    );
}

/// A BUILDER THAT TAKES A REQUEST DELETES OLDER REQUESTS' HANDOFFS: each holds a whole zip and no
/// cut reads one again, so origin keeps only the handoff of the request it serves; a request it
/// refuses deletes nothing, and the handoff it makes stays.
/// NEGATIVE CONTROL: every pump release left its zip on origin for good.
#[test]
fn a_builder_deletes_older_requests_handoffs() {
    let fx = Fixture::new(true);
    let older = format!("{}0.199.0-7-aaaaaaaaaaaa", pull::HANDOFF_REF_PREFIX);
    let stale = fx.commit_tree(&[("x", "an older request's zip")], &[]);
    git(
        &fx.seed,
        &["push", "--quiet", "origin", &format!("{stale}:{older}")],
    );
    // A refused request deletes nothing.
    let refused = Request {
        build: BUILD + 1,
        ..fx.request()
    };
    fx.ask(&refused, &[]);
    let mut stub = Stub::default();
    let (code, said) = fx.poll(&mut stub, QUIET);
    assert_eq!(code, FAILED, "{said}");
    assert_eq!(fx.remote(&older), Some(stale.clone()), "{said}");
    // The release's own request: built, handed off, and the older handoff deleted.
    let request = fx.request();
    fx.ask(&request, &[]);
    let (code, said) = fx.poll(&mut stub, QUIET);
    assert_eq!((code, stub.calls), (PASS, 1), "{said}");
    assert_eq!(fx.remote(&older), None, "{said}");
    assert!(said.contains("an older request's handoff"), "{said}");
    let handoff = fx.remote(&request.handoff_ref());
    assert!(handoff.is_some(), "{said}");
    // The next poll finds it there and leaves it.
    let (code, said) = fx.poll(&mut stub, QUIET);
    assert_eq!(code, PASS, "{said}");
    assert_eq!(fx.remote(&request.handoff_ref()), handoff, "{said}");
}

#[test]
fn a_request_of_another_shape_is_never_built() {
    let fx = Fixture::new(true);
    let good = fx.request();
    // Release commits that are not one the cutter makes of origin/main's code.
    let extra = fx.commit_on(&fx.source, |dir| {
        release_edits(dir, BUILD, VERSION);
        write(dir, "src/evil.rs", "fn evil() {}\n");
    });
    let edited = fx.commit_on(&fx.source, |dir| {
        release_edits(dir, BUILD, VERSION);
        write(
            dir,
            "Cargo.toml",
            "[workspace.package]\nversion = \"0.200.0\"\n# x\n",
        );
    });
    let unpublished = fx.commit_on(&fx.source, |dir| write(dir, "unpublished.txt", "x\n"));
    let off_main = fx.commit_on(&unpublished, |dir| release_edits(dir, BUILD, VERSION));
    let other_version = fx.commit_on(&fx.source, |dir| release_edits(dir, BUILD, "0.201.0"));
    let merge = fx.commit_tree(&[("a", "b")], &[&fx.source, &unpublished]);
    let release_of = |commit: &str| Request {
        commit: commit.to_string(),
        tree: git(&fx.seed, &["rev-parse", &format!("{commit}^{{tree}}")]),
        ..good.clone()
    };
    let mut cases: Vec<(&str, Request, Vec<String>)> = vec![
        ("a file besides the release's", release_of(&extra), vec![]),
        ("a changed Cargo.toml", release_of(&edited), vec![]),
        ("a parent not on origin/main", release_of(&off_main), vec![]),
        (
            "a version Cargo.toml does not stamp",
            Request {
                version: "0.201.0".to_string(),
                ..release_of(&other_version)
            },
            vec![],
        ),
        (
            "a release commit with two parents",
            release_of(&merge),
            vec![],
        ),
        (
            "a build number the ledger does not end in",
            Request {
                build: BUILD + 1,
                ..good.clone()
            },
            vec![],
        ),
        (
            "another tree",
            Request {
                tree: git(&fx.seed, &["rev-parse", &format!("{}^{{tree}}", fx.source)]),
                ..good.clone()
            },
            vec![],
        ),
        (
            "a commit that is not the request's parent",
            Request {
                commit: fx.source.clone(),
                ..good.clone()
            },
            vec![fx.release.clone()],
        ),
        (
            "a request with two parents",
            good.clone(),
            vec![fx.release.clone(), fx.source.clone()],
        ),
    ];
    // Request commits of another shape: another file beside it, no parent, no request.
    let text = good.to_toml().expect("toml");
    let shaped: [(&str, String); 3] = [
        (
            "a file beside the request",
            fx.commit_tree(
                &[(pull::REQUEST_FILE, &text), ("x.txt", "x")],
                &[&fx.release],
            ),
        ),
        (
            "a parentless request",
            fx.commit_tree(&[(pull::REQUEST_FILE, &text)], &[]),
        ),
        (
            "a request that does not read",
            fx.commit_tree(&[(pull::REQUEST_FILE, "version = 1\n")], &[&fx.release]),
        ),
    ];
    let mut stub = Stub::default();
    // Each is passed over, and the heartbeat says so of that very request: no cut waits on it.
    let passed_over = |what: &str| {
        let alive = fx.alive();
        assert_eq!(
            alive.request.as_deref(),
            Some(fx.asked().as_str()),
            "{what}"
        );
        assert_eq!(alive.passed_over, Some(PassedOver::Refused), "{what}");
    };
    for (what, request, parents) in cases.drain(..) {
        let parents: Vec<&str> = parents.iter().map(String::as_str).collect();
        fx.ask(&request, &parents);
        let (code, said) = fx.poll(&mut stub, QUIET);
        assert_eq!(code, FAILED, "{what}: {said}");
        assert!(said.contains("refused, nothing built"), "{what}: {said}");
        assert_eq!(stub.calls, 0, "{what}: {said}");
        passed_over(what);
    }
    for (what, commit) in shaped {
        fx.push_request(&commit);
        let (code, said) = fx.poll(&mut stub, QUIET);
        assert_eq!(code, FAILED, "{what}: {said}");
        assert_eq!(stub.calls, 0, "{what}: {said}");
        passed_over(what);
    }
    // Control: the cutter's own request is built.
    fx.ask(&good, &[]);
    let (code, said) = fx.poll(&mut stub, QUIET);
    assert_eq!((code, stub.calls), (PASS, 1), "{said}");
}

#[test]
fn a_build_that_fails_is_tried_twice_then_passed_over() {
    let fx = Fixture::new(true);
    let request = fx.request();
    fx.ask(&request, &[]);
    let mut stub = Stub {
        fail: true,
        ..Stub::default()
    };
    for attempt in 1..=MAX_ATTEMPTS {
        let (code, said) = fx.poll(&mut stub, QUIET);
        assert_eq!(code, FAILED, "{said}");
        assert_eq!(stub.calls, attempt as usize, "{said}");
        assert!(said.contains("the stub failed"), "{said}");
        // While it still builds the request, its heartbeat passes nothing over.
        assert_eq!(fx.alive().passed_over, None, "{said}");
    }
    let (code, said) = fx.poll(&mut stub, QUIET);
    assert_eq!(code, FAILED, "{said}");
    assert_eq!(stub.calls, MAX_ATTEMPTS as usize, "{said}");
    assert!(said.contains("passed over"), "{said}");
    assert_eq!(fx.remote(&request.handoff_ref()), None);
    let alive = fx.alive();
    assert_eq!(
        alive.passed_over(&fx.asked()),
        Some(PassedOver::Spent),
        "{alive:?}"
    );
    // A new reservation is a new request, and is built.
    let again = fx.commit_on(&fx.source, |dir| release_edits(dir, BUILD + 7, VERSION));
    let fresh = Request {
        build: BUILD + 7,
        commit: again.clone(),
        tree: git(&fx.seed, &["rev-parse", &format!("{again}^{{tree}}")]),
        ..request
    };
    fx.ask(&fresh, &[]);
    stub.fail = false;
    let (code, said) = fx.poll(&mut stub, QUIET);
    assert_eq!(code, PASS, "{said}");
    assert!(fx.remote(&fresh.handoff_ref()).is_some(), "{said}");
}

#[test]
fn a_pair_that_is_not_the_requests_is_not_handed_off() {
    let fx = Fixture::new(true);
    let request = fx.request();
    fx.ask(&request, &[]);
    let mut stub = Stub {
        another_build: true,
        ..Stub::default()
    };
    let (code, said) = fx.poll(&mut stub, QUIET);
    assert_eq!(code, FAILED, "{said}");
    assert!(said.contains("not admitted"), "{said}");
    assert_eq!(fx.remote(&request.handoff_ref()), None, "{said}");
}

#[test]
fn no_build_starts_while_update_holds_its_lock() {
    let fx = Fixture::new(true);
    let request = fx.request();
    fx.ask(&request, &[]);
    std::fs::create_dir_all(fx.layout.dist_root()).expect("dist");
    let held = aterm_uds::file_lock::LockFile::take(
        &fx.layout.update_lock(),
        Instant::now() + Duration::from_secs(2),
        "the test holds it",
    )
    .expect("lock");
    let mut stub = Stub::default();
    let (code, said) = fx.poll(&mut stub, QUIET);
    assert_eq!(code, NOT_RUN, "{said}");
    assert_eq!(stub.calls, 0, "{said}");
    assert!(
        !fx.layout.release_attempts(&request.key()).exists(),
        "a poll that did not build counted an attempt"
    );
    drop(held);
    let (code, said) = fx.poll(&mut stub, QUIET);
    assert_eq!((code, stub.calls), (PASS, 1), "{said}");
}

#[test]
fn the_heartbeat_goes_on_while_a_build_runs() {
    let fx = Fixture::new(true);
    fx.ask(&fx.request(), &[]);
    let mut stub = Stub {
        beat_on: Some((fx.scratch.path().to_path_buf(), fx.origin.clone())),
        ..Stub::default()
    };
    let (code, said) = fx.poll(&mut stub, Duration::from_millis(100));
    assert_eq!((code, stub.calls), (PASS, 1), "{said}");
}

#[test]
fn a_release_commit_changes_only_what_a_release_does() {
    for (status, path) in [
        ("M", "RELEASES.ledger"),
        ("M", "CHANGELOG.md"),
        ("D", "changes/one.md"),
        ("D", "changes/a/b.md"),
    ] {
        assert!(release_change(status, path), "{status} {path}");
    }
    for (status, path) in [
        ("A", "RELEASES.ledger"),
        ("D", "CHANGELOG.md"),
        ("M", "changes/one.md"),
        ("A", "changes/new.md"),
        ("D", "changes/"),
        ("D", "changesx/one.md"),
        ("M", "Cargo.toml"),
    ] {
        assert!(!release_change(status, path), "{status} {path}");
    }
    assert_eq!(
        ledger_tail("# c\n1 0.1.0\n\n2 0.2.0\n# after\n\n"),
        Some("2 0.2.0")
    );
    assert_eq!(ledger_tail("# only\n"), None);
}

#[test]
fn a_log_past_its_bound_is_moved_aside() {
    let scratch = aterm_tempfile::tempdir().expect("tempdir");
    let log = scratch.path().join("serve.log");
    std::fs::write(&log, vec![b'x'; LOG_MAX_BYTES as usize]).expect("write");
    rotate(&log);
    assert!(log.is_file(), "a log at its bound was moved");
    std::fs::write(&log, vec![b'x'; LOG_MAX_BYTES as usize + 1]).expect("write");
    rotate(&log);
    assert!(!log.exists());
    assert!(scratch.path().join("serve.log.old").is_file());
}
