// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use agh::git::FetchScope;
use agh_fake::git_backend::GitHttpBackend;
#[cfg(target_os = "macos")]
use std::os::unix::fs::MetadataExt as _;

struct Fixture {
    world: World,
    _backend: GitHttpBackend,
    gh: Github,
    oid: String,
    donor: PathBuf,
}

fn fixture(label: &str, count: usize) -> Fixture {
    let world = reader_world(label);
    let bare = bare_repo(world.path(), "origin");
    let before = host_git(&bare, &["rev-parse", "main"]);
    let mut input = format!(
        "commit refs/heads/main\ncommitter fixture <fixture> 1700000000 +0000\ndata 7\nbatches\nfrom {before}\ndeleteall\n"
    ).into_bytes();
    for n in 0..count {
        let body = format!("tiny retained blob {n}\n");
        input.extend_from_slice(
            format!("M 100644 inline payload/{n}\ndata {}\n{body}\n", body.len()).as_bytes(),
        );
    }
    input.extend_from_slice(b"done\n");
    let mut command = aterm_uds::spawnfd::command(host_git_path());
    command
        .args(["fast-import", "--quiet", "--done"])
        .current_dir(&bare)
        .env_clear()
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    let imported = aterm_uds::deadline::output_with_input_until(
        &mut command,
        aterm_uds::secret::Secret::new(&input),
        std::time::Instant::now() + common::HANG,
        1 << 20,
    )
    .unwrap();
    assert!(imported.status.success(), "{imported:?}");
    host_git(&bare, &["config", "uploadpack.allowFilter", "true"]);
    let oid = host_git(&bare, &["rev-parse", "main"]);
    let backend = GitHttpBackend::start(
        "fixture-dev/app",
        &bare,
        &agh_fake::token("reader"),
        &agh_fake::token("dev"),
    )
    .unwrap();
    let ports = world.fake.ports();
    let gh = git_builder(
        world.router.clone(),
        Endpoints::loopback(ports.api, ports.uploads, backend.port(), ports.cdn),
        &world.dirs,
        git_env(),
        live_start(),
    )
    .fx(Arc::new(Fx::simulated(live_start())))
    .build();
    let donor = world.path().join("donor.git");
    host_git(
        world.path(),
        &["init", "--quiet", "--bare", donor.to_str().unwrap()],
    );
    let request = GitRun::fetch_ref(
        &common::slug("fixture-dev/app"),
        &donor,
        &oid,
        "refs/agh/selected",
        false,
        FetchScope::SelectedCheckout { cache: None },
    );
    gh.run_git(&request).unwrap();
    Fixture {
        world,
        _backend: backend,
        gh,
        oid,
        donor,
    }
}

fn body_indices(donor: &Path, counts: &[usize]) -> Vec<PathBuf> {
    let mut indices: Vec<_> = std::fs::read_dir(donor.join("objects/pack"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "idx"))
        .filter(|path| {
            let bytes = std::fs::read(path).unwrap();
            let count = u32::from_be_bytes(bytes[1028..1032].try_into().unwrap()) as usize;
            counts.contains(&count)
        })
        .collect();
    indices.sort();
    indices
}

fn request(fixture: &Fixture, target: &Path) -> GitRun {
    GitRun::fetch_ref(
        &common::slug("fixture-dev/app"),
        target,
        &fixture.oid,
        "refs/agh/selected",
        false,
        FetchScope::SelectedCheckout {
            cache: Some(fixture.donor.clone()),
        },
    )
}

#[cfg(target_os = "macos")]
fn client(fixture: &Fixture, fx: Fx) -> Github {
    let ports = fixture.world.fake.ports();
    git_builder(
        fixture.world.router.clone(),
        Endpoints::loopback(ports.api, ports.uploads, fixture._backend.port(), ports.cdn),
        &fixture.world.dirs,
        git_env(),
        live_start(),
    )
    .fx(Arc::new(fx))
    .build()
}

fn git_input(directory: &Path, args: &[&str], input: &[u8]) -> String {
    let mut command = aterm_uds::spawnfd::command(host_git_path());
    command
        .args(args)
        .current_dir(directory)
        .env_clear()
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    let out = aterm_uds::deadline::output_with_input_until(
        &mut command,
        aterm_uds::secret::Secret::new(input),
        std::time::Instant::now() + common::HANG,
        1 << 20,
    )
    .unwrap();
    assert!(out.status.success(), "{out:?}");
    String::from_utf8(out.stdout).unwrap().trim_end().to_owned()
}

#[test]
fn a_valid_same_count_wrong_member_pack_keeps_the_original_narrow_producer() {
    let fixture = fixture("checkout-other-members", 2);
    let names: Vec<_> = host_git(
        &fixture.donor,
        &["ls-tree", "-r", "--format=%(objectname)", &fixture.oid],
    )
    .lines()
    .map(str::to_owned)
    .collect();
    assert_eq!(names.len(), 2);
    let bodies: Vec<_> = names
        .iter()
        .map(|oid| host_git(&fixture.donor, &["cat-file", "blob", oid]) + "\n")
        .collect();
    let old = body_indices(&fixture.donor, &[2]);
    assert_eq!(old.len(), 1);
    for extension in ["pack", "idx"] {
        let path = old[0].with_extension(extension);
        std::fs::rename(&path, fixture.world.path().join(path.file_name().unwrap())).unwrap();
    }
    // Keep all requested DATA valid, but supply no pack whose membership is
    // this batch. The unused blob must never enter the receiving repository.
    for (oid, body) in names.iter().zip(&bodies) {
        assert_eq!(
            git_input(
                &fixture.donor,
                &["hash-object", "-w", "--stdin"],
                body.as_bytes()
            ),
            *oid
        );
    }
    let extra = git_input(
        &fixture.donor,
        &["hash-object", "-w", "--stdin"],
        b"unused private cache body\n",
    );
    let prefix = fixture.donor.join("objects/pack/pack");
    git_input(
        &fixture.donor,
        &[
            "pack-objects",
            "--quiet",
            "--threads=1",
            prefix.to_str().unwrap(),
        ],
        format!("{}\n{extra}\n", names[0]).as_bytes(),
    );
    let target = fixture.world.path().join("receiver.git");
    host_git(
        fixture.world.path(),
        &["init", "--quiet", "--bare", target.to_str().unwrap()],
    );
    let before = fixture.gh.fx().children_run().len();
    fixture.gh.run_git(&request(&fixture, &target)).unwrap();
    let children = fixture.gh.fx().children_run();
    let delta = &children[before..];
    assert_eq!(
        delta
            .iter()
            .filter(|c| c.args.iter().any(|a| a == "pack-objects")
                && !c.args.iter().any(|a| a == "--filter=blob:none"))
            .count(),
        1
    );
    assert!(
        !delta
            .iter()
            .any(|c| c.args.iter().any(|a| a == "--depth=1"))
    );
    assert_eq!(
        git_input(
            &target,
            &["cat-file", "--batch-check"],
            format!("{extra}\n").as_bytes()
        ),
        format!("{extra} missing")
    );
    assert_eq!(
        host_git(&target, &["show", &format!("{}:payload/1", fixture.oid)]),
        "tiny retained blob 1"
    );
}

#[test]
fn complete_loose_cache_without_pack_advice_keeps_the_original_producer() {
    let fixture = fixture("checkout-loose-only", 2);
    let directory = fixture.donor.join("objects/pack");
    let paths: Vec<_> = std::fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    let packs: Vec<_> = paths
        .iter()
        .filter(|p| p.extension().is_some_and(|e| e == "pack"))
        .map(|p| std::fs::read(p).unwrap())
        .collect();
    // Remove only this completed fixture's packed representation, then import
    // every original object as loose data. No requested object becomes absent.
    for path in paths {
        std::fs::remove_file(path).unwrap();
    }
    for pack in packs {
        git_input(&fixture.donor, &["unpack-objects", "-r"], &pack);
    }
    std::fs::remove_dir(&directory).unwrap();
    for name in ["absent.git", "empty.git"] {
        if name == "empty.git" {
            std::fs::create_dir(&directory).unwrap();
        }
        let target = fixture.world.path().join(name);
        host_git(
            fixture.world.path(),
            &["init", "--quiet", "--bare", target.to_str().unwrap()],
        );
        let before = fixture.gh.fx().children_run().len();
        fixture.gh.run_git(&request(&fixture, &target)).unwrap();
        let children = fixture.gh.fx().children_run();
        let delta = &children[before..];
        assert_eq!(
            delta
                .iter()
                .filter(|c| c.args.iter().any(|a| a == "pack-objects")
                    && !c.args.iter().any(|a| a == "--filter=blob:none"))
                .count(),
            1
        );
        assert!(
            !delta
                .iter()
                .any(|c| c.args.iter().any(|a| a == "--depth=1"))
        );
        assert_eq!(
            host_git(&target, &["show", &format!("{}:payload/1", fixture.oid)]),
            "tiny retained blob 1"
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn a_matching_member_hint_cannot_authorize_a_corrupted_native_index() {
    use std::os::unix::fs::PermissionsExt as _;
    for field in ["crc", "checksum"] {
        let fixture = fixture(&format!("checkout-index-{field}"), 2);
        let index = body_indices(&fixture.donor, &[2]).pop().unwrap();
        let mut bytes = std::fs::read(&index).unwrap();
        let offset = if field == "crc" {
            1032 + 2 * 20
        } else {
            bytes.len() - 1
        };
        bytes[offset] ^= 1;
        std::fs::set_permissions(&index, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(index, bytes).unwrap();
        let target = fixture.world.path().join("receiver.git");
        host_git(
            fixture.world.path(),
            &["init", "--quiet", "--bare", target.to_str().unwrap()],
        );
        let before = fixture.gh.fx().children_run().len();
        let error = fixture.gh.run_git(&request(&fixture, &target)).unwrap_err();
        assert_eq!(error.side_effect, SideEffect::None);
        let children = fixture.gh.fx().children_run();
        assert!(
            children[before..]
                .iter()
                .any(|c| c.args.iter().any(|a| a == "--strict")
                    && c.cwd
                        .as_ref()
                        .is_some_and(|p| p.to_string_lossy().contains("checkout-pack-verifier-"))),
            "{field}: native existing-pair verification must decide"
        );
        assert_eq!(host_git(&target, &["for-each-ref"]), "");
    }
}

#[cfg(target_os = "macos")]
#[test]
fn two_fresh_authenticated_receivers_reuse_exact_multi_batch_blob_pairs() {
    let fixture = fixture("checkout-retained-pairs", 16_385);
    let bodies = body_indices(&fixture.donor, &[16_384, 1]);
    assert_eq!(bodies.len(), 2);
    let snapshots: Vec<_> = bodies
        .iter()
        .flat_map(|index| [index.with_extension("pack"), index.clone()])
        .map(|path| {
            let metadata = std::fs::metadata(&path).unwrap();
            (
                path,
                metadata.ino(),
                metadata.nlink(),
                metadata.len(),
                metadata.ctime(),
                metadata.ctime_nsec(),
            )
        })
        .collect();
    for name in ["second.git", "third.git"] {
        let target = fixture.world.path().join(name);
        host_git(
            fixture.world.path(),
            &["init", "--quiet", "--bare", target.to_str().unwrap()],
        );
        let before = fixture.gh.fx().children_run().len();
        fixture.gh.run_git(&request(&fixture, &target)).unwrap();
        let children = fixture.gh.fx().children_run();
        let delta = &children[before..];
        assert_eq!(
            delta
                .iter()
                .filter(|c| c.args.iter().any(|a| a == "fetch")
                    && c.args.iter().any(|a| a == "--filter=blob:none"))
                .count(),
            1
        );
        assert!(
            !delta
                .iter()
                .any(|c| c.args.iter().any(|a| a == "--depth=1"))
        );
        assert!(
            !delta
                .iter()
                .any(|c| c.args.iter().any(|a| a == "pack-objects")
                    && !c.args.iter().any(|a| a == "--filter=blob:none"))
        );
        assert_eq!(
            delta
                .iter()
                .filter(|c| c
                    .args
                    .iter()
                    .any(|a| a == "--batch-check=%(objectname) %(objecttype)")
                    && c.cwd
                        .as_ref()
                        .is_some_and(|p| p.to_string_lossy().contains("checkout-pack-verifier-")))
                .count(),
            2
        );
        assert_eq!(
            host_git(
                &target,
                &["show", &format!("{}:payload/16384", fixture.oid)]
            ),
            "tiny retained blob 16384"
        );
        assert_eq!(
            host_git(&target, &["rev-parse", "refs/agh/selected"]),
            fixture.oid
        );
        for (source, inode, links, bytes, ctime, ctime_ns) in &snapshots {
            let unchanged = std::fs::metadata(source).unwrap();
            assert_eq!(
                (
                    unchanged.ino(),
                    unchanged.nlink(),
                    unchanged.len(),
                    unchanged.ctime(),
                    unchanged.ctime_nsec()
                ),
                (*inode, *links, *bytes, *ctime, *ctime_ns)
            );
            let destination = target
                .join("objects/pack")
                .join(source.file_name().unwrap());
            let installed = std::fs::metadata(&destination).unwrap();
            assert_ne!(installed.ino(), *inode);
            assert_eq!(installed.nlink(), 1);
            assert_eq!(installed.len(), *bytes);
            assert_eq!(
                std::fs::read(&destination).unwrap(),
                std::fs::read(source).unwrap()
            );
        }
    }
    // A later donor write must not alter either completed receiver. These are
    // fixture files, after every acquisition and its native children settled.
    for index in &bodies {
        let pack = index.with_extension("pack");
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&pack, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(pack, b"later fixture donor mutation").unwrap();
    }
    for name in ["second.git", "third.git"] {
        assert_eq!(
            host_git(
                &fixture.world.path().join(name),
                &["show", &format!("{}:payload/16384", fixture.oid)]
            ),
            "tiny retained blob 16384"
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn failed_native_pair_integrity_or_type_checks_never_enter_the_cold_path() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let fixture = fixture("checkout-retained-authority", 2);
    let target = fixture.world.path().join("probe.git");
    host_git(
        fixture.world.path(),
        &["init", "--quiet", "--bare", target.to_str().unwrap()],
    );
    let before = fixture.gh.fx().children_run().len();
    fixture.gh.run_git(&request(&fixture, &target)).unwrap();
    let specs = fixture.gh.fx().children_run();
    let verify = specs[before..]
        .iter()
        .find(|s| {
            s.args.iter().any(|a| a == "index-pack") && s.args.iter().any(|a| a == "--strict")
        })
        .unwrap();
    let types = specs[before..]
        .iter()
        .find(|s| {
            s.args
                .iter()
                .any(|a| a == "--batch-check=%(objectname) %(objecttype)")
                && s.cwd
                    .as_ref()
                    .is_some_and(|p| p.to_string_lossy().contains("checkout-pack-verifier-"))
        })
        .unwrap();
    for fault in [
        "reject",
        "io",
        "deadline",
        "cancel",
        "wrong-type",
        "extra",
        "partial",
        "utf8",
        "saturated",
    ] {
        let reached = Arc::new(AtomicBool::new(false));
        let happened = reached.clone();
        let spec = if matches!(fault, "reject" | "io" | "deadline" | "cancel") {
            verify
        } else {
            types
        };
        let fx = Fx::simulated(live_start()).with_git_intercept_once_for_tests(
            spec.program.clone(),
            spec.args.clone(),
            || {},
            move |result| {
                happened.store(true, Ordering::Release);
                match fault {
                    "reject" => result.as_mut().unwrap().code = Some(128),
                    "io" => {
                        *result = Err(agh::Error::could_not_run("owned pair verifier I/O failed"))
                    }
                    "deadline" => {
                        *result = Err(agh::Error::could_not_run(
                            "owned pair verifier deadline expired",
                        ))
                    }
                    "cancel" => {
                        *result = Err(agh::Error::new(
                            Kind::Interrupted,
                            "owned pair verifier interrupted",
                        ))
                    }
                    "wrong-type" => {
                        let out = result.as_mut().unwrap();
                        out.stdout = String::from_utf8(out.stdout.clone())
                            .unwrap()
                            .replace(" blob\n", " tree\n")
                            .into_bytes();
                    }
                    "extra" => result
                        .as_mut()
                        .unwrap()
                        .stdout
                        .extend_from_slice(format!("{} blob\n", "0".repeat(40)).as_bytes()),
                    "partial" => {
                        result.as_mut().unwrap().stdout.pop();
                    }
                    "utf8" => {
                        result.as_mut().unwrap().stdout[0] = 255;
                    }
                    _ => result.as_mut().unwrap().stdout = vec![b'x'; 1024 * 1024],
                }
            },
        );
        let gh = client(&fixture, fx);
        let target = fixture.world.path().join(format!("failed-{fault}.git"));
        host_git(
            fixture.world.path(),
            &["init", "--quiet", "--bare", target.to_str().unwrap()],
        );
        let error = gh.run_git(&request(&fixture, &target)).unwrap_err();
        assert!(
            reached.load(Ordering::Acquire),
            "{fault}: exact real verifier child must execute"
        );
        if fault == "cancel" {
            assert_eq!(error.kind, Kind::Interrupted);
        }
        assert_eq!(error.side_effect, SideEffect::None);
        assert_eq!(host_git(&target, &["for-each-ref"]), "");
        assert!(
            !gh.fx()
                .children_run()
                .iter()
                .any(|c| c.args.iter().any(|a| a == "pack-objects")
                    && !c.args.iter().any(|a| a == "--filter=blob:none"))
        );
        assert!(
            !gh.fx()
                .children_run()
                .iter()
                .any(|c| c.args.iter().any(|a| a == "--depth=1"))
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn retained_pair_custody_survives_neither_source_drift_nor_postcheck_mutation() {
    use std::os::unix::fs::PermissionsExt as _;
    use std::sync::{
        OnceLock, Weak,
        atomic::{AtomicBool, Ordering},
    };
    for fault in [
        "source",
        "owned",
        "installed",
        "parent",
        "private-parent",
        "private-alternates",
        "private-config",
        "quarantine",
        "published-source",
        "published-installed",
    ] {
        let fixture = fixture(&format!("checkout-custody-{fault}"), 2);
        let index = body_indices(&fixture.donor, &[2]).pop().unwrap();
        let filename = index.file_name().unwrap().to_owned();
        let target = fixture.world.path().join("receiver.git");
        host_git(
            fixture.world.path(),
            &["init", "--quiet", "--bare", target.to_str().unwrap()],
        );
        let destination = std::fs::canonicalize(target.join("objects/pack")).unwrap();
        let moved = fixture.world.path().join("moved-pack-directory");
        let weak: Arc<OnceLock<Weak<Fx>>> = Arc::new(OnceLock::new());
        let observed = weak.clone();
        let reached = Arc::new(AtomicBool::new(false));
        let happened = reached.clone();
        let receiver = destination.clone();
        let fx = Arc::new(
            Fx::simulated(live_start()).with_git_child_observer_for_tests(move |span| {
                let fx = observed.get().and_then(Weak::upgrade).unwrap();
                let specs = fx.children_run();
                let spec = &specs[span.ordinal];
                let types =
                    spec.args
                        .iter()
                        .any(|a| a == "--batch-check=%(objectname) %(objecttype)")
                        && spec.cwd.as_ref().is_some_and(|p| {
                            p.to_string_lossy().contains("checkout-pack-verifier-")
                        });
                let installed = spec.args.iter().any(|a| a == "index-pack")
                    && spec
                        .args
                        .last()
                        .is_some_and(|p| Path::new(p).parent() == Some(receiver.as_path()))
                    && spec.args.last().is_some_and(|p| {
                        Path::new(p).file_stem() == Path::new(&filename).file_stem()
                    });
                let private_proof = spec.args.iter().any(|a| a == "--strict")
                    && spec
                        .cwd
                        .as_ref()
                        .is_some_and(|p| p.to_string_lossy().contains("checkout-pack-verifier-"));
                let published = spec.args.iter().any(|a| a == "update-ref")
                    && spec.args.iter().any(|a| a == "refs/agh/selected");
                let selected = if fault.starts_with("private-") || fault == "quarantine" {
                    private_proof
                } else if fault.starts_with("published-") {
                    published
                } else if fault == "installed" {
                    installed
                } else {
                    types
                };
                if span.code != Some(0) || !selected || happened.swap(true, Ordering::AcqRel) {
                    return;
                }
                if fault.starts_with("private-") || fault == "quarantine" {
                    let root = spec.cwd.as_ref().unwrap();
                    match fault {
                        "private-alternates" => std::fs::write(
                            root.join("objects/info/alternates"),
                            fixture_cache_objects(&index),
                        )
                        .unwrap(),
                        "private-config" => std::fs::write(
                            root.join("config"),
                            b"[core]\n bare = true\n[extensions]\n objectformat = sha256\n",
                        )
                        .unwrap(),
                        _ => {
                            let old = if fault == "quarantine" {
                                root.parent().unwrap().to_path_buf()
                            } else {
                                root.join("objects")
                            };
                            std::fs::rename(&old, &moved).unwrap();
                            std::fs::create_dir(&old).unwrap();
                            std::fs::write(old.join("victim"), b"untouched").unwrap();
                        }
                    }
                    return;
                }
                if fault == "parent" {
                    std::fs::rename(&receiver, &moved).unwrap();
                    std::fs::create_dir(&receiver).unwrap();
                    std::fs::write(receiver.join("victim"), b"untouched").unwrap();
                    return;
                }
                let victim = match fault {
                    "source" | "published-source" => index.clone(),
                    "owned" => spec
                        .cwd
                        .as_ref()
                        .unwrap()
                        .join("objects/pack")
                        .join(&filename),
                    _ => receiver.join(&filename),
                };
                std::fs::set_permissions(&victim, std::fs::Permissions::from_mode(0o600)).unwrap();
                std::fs::write(victim, b"changed after actual native verification").unwrap();
            }),
        );
        weak.set(Arc::downgrade(&fx)).unwrap();
        let ports = fixture.world.fake.ports();
        let gh = git_builder(
            fixture.world.router.clone(),
            Endpoints::loopback(ports.api, ports.uploads, fixture._backend.port(), ports.cdn),
            &fixture.world.dirs,
            git_env(),
            live_start(),
        )
        .fx(fx)
        .build();
        let error = gh.run_git(&request(&fixture, &target)).unwrap_err();
        assert!(
            reached.load(Ordering::Acquire),
            "{fault}: actual verification boundary must be reached"
        );
        if fault.starts_with("published-") {
            assert_eq!(error.side_effect, SideEffect::Applied);
            assert_eq!(
                host_git(&target, &["rev-parse", "refs/agh/selected"]),
                fixture.oid
            );
        } else {
            assert_eq!(error.side_effect, SideEffect::None);
            assert_eq!(host_git(&target, &["for-each-ref"]), "");
        }
        if fault == "parent" {
            assert_eq!(
                std::fs::read(destination.join("victim")).unwrap(),
                b"untouched"
            );
        }
        if fault == "quarantine" {
            let replacement = std::fs::read_dir(destination)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .find(|path| {
                    path.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with(".agh-checkout-")
                })
                .unwrap();
            assert_eq!(
                std::fs::read(replacement.join("victim")).unwrap(),
                b"untouched"
            );
        }
    }
}

#[cfg(target_os = "macos")]
fn fixture_cache_objects(index: &Path) -> String {
    format!("{}\n", index.parent().unwrap().parent().unwrap().display())
}
