// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_json::json;

#[test]
fn an_offline_cache_miss_is_the_machines_refusal() {
    for stderr in [
        "error: failed to download `wgpu-core-deps-emscripten v29.0.3`\n\nCaused by:\n  \
         attempting to make an HTTP request, but --offline was specified",
        "error: no matching package named `x` found\nAs a reminder, you're using offline \
         mode (--offline) which can sometimes cause surprising resolution failures",
    ] {
        assert!(offline_cache_miss(stderr), "{stderr}");
    }
    assert!(!offline_cache_miss(
        "error: failed to parse manifest at `Cargo.toml`"
    ));
    assert!(machine_refusal(&format!(
        "{CACHE_LACKS_A_LOCKED_CRATE}, so …"
    )));
    assert!(!machine_refusal(
        "Cargo metadata refused: failed to parse manifest"
    ));
    let (root, mut facts) = valid();
    facts.metadata = Err(format!("{CACHE_LACKS_A_LOCKED_CRATE}, so …"));
    let why = validate(&root, &facts).expect_err("no graph, no pin");
    assert!(machine_refusal(&why), "{why}");
}

const FIXTURE: [&str; 6] = [
    "astream-broker",
    "astream-cap",
    "astream-digest",
    "astream-effects",
    "astream-term",
    "astream-wire",
];

fn valid() -> (PathBuf, Facts) {
    let root = PathBuf::from("/w");
    let pin = "1".repeat(40);
    let path = |p: &Path| p.to_str().unwrap().to_owned();
    let id = |dir: &Path| format!("path+file://{}#0.1.0", path(dir));
    let crate_dir = |name: &str| root.join(SUBMODULE).join("crates").join(name);
    let mut packages: Vec<Value> = FIXTURE
        .iter()
        .map(|name| {
            let dir = crate_dir(name);
            json!({
                "id": id(&dir),
                "name": name,
                "source": null,
                "license": "Apache-2.0",
                "manifest_path": path(&dir.join("Cargo.toml")),
                "dependencies": [],
            })
        })
        .collect();
    let consumer = root.join(CONSUMER);
    let link = id(consumer.parent().unwrap());
    let deps: Vec<Value> = CONSUMER_DEPS
        .iter()
        .map(|name| json!({ "name": name, "path": path(&crate_dir(name)) }))
        .collect();
    packages.push(json!({
        "id": link.clone(),
        "name": "aterm-link",
        "source": null,
        "license": "Apache-2.0",
        "manifest_path": path(&consumer),
        "dependencies": deps,
    }));
    let facts = Facts {
        gitmodules: vec![
            (format!("submodule.{SUBMODULE}.path"), SUBMODULE.into()),
            (format!("submodule.{SUBMODULE}.url"), format!("{URL}.git")),
        ],
        index: vec![IndexEntry {
            mode: GITLINK_MODE.into(),
            object: pin.clone(),
            stage: "0".into(),
            path: SUBMODULE.into(),
        }],
        checkout: Checkout::At {
            head: pin,
            modified: Vec::new(),
        },
        motion: PinMotion::Unchanged,
        exclude: vec![SUBMODULE.into()],
        metadata: Ok(json!({ "packages": packages, "workspace_members": [link] })),
    };
    assert!(
        validate(&root, &facts).is_ok(),
        "{:?}",
        validate(&root, &facts)
    );
    (root, facts)
}

fn rejects(root: &Path, facts: &Facts, needles: &[&str]) {
    let why = validate(root, facts).expect_err("the mutation must be refused");
    for needle in needles {
        assert!(why.contains(needle), "expected `{needle}` in: {why}");
    }
}

fn package<'a>(facts: &'a mut Facts, name: &str) -> &'a mut aterm_json::Map {
    facts
        .metadata
        .as_mut()
        .unwrap()
        .as_object_mut()
        .unwrap()
        .get_mut("packages")
        .unwrap()
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|p| p["name"].as_str() == Some(name))
        .unwrap()
        .as_object_mut()
        .unwrap()
}

fn packages(facts: &mut Facts) -> &mut Vec<Value> {
    facts
        .metadata
        .as_mut()
        .unwrap()
        .as_object_mut()
        .unwrap()
        .get_mut("packages")
        .unwrap()
        .as_array_mut()
        .unwrap()
}

fn set_url(facts: &mut Facts, url: &str) {
    for (k, v) in &mut facts.gitmodules {
        if k.ends_with(".url") {
            *v = url.to_string();
        }
    }
}

type Plant = Box<dyn Fn(&mut Facts)>;

#[test]
fn every_planted_drift_from_the_pinned_submodule_is_refused() {
    let (root, facts) = valid();
    let base = "0123456789012345678901234567890123456789".to_owned();
    let mut refused: Vec<(String, Plant)> = vec![
        (
            "no .gitmodules".into(),
            Box::new(|f: &mut Facts| f.gitmodules.clear()),
        ),
        (
            "another URL".into(),
            Box::new(|f: &mut Facts| set_url(f, "https://github.com/someone-else/astream.git")),
        ),
        (
            "two entries at the path".into(),
            Box::new(|f: &mut Facts| {
                f.gitmodules
                    .push(("submodule.other.path".into(), SUBMODULE.into()));
            }),
        ),
        (
            "a tree, not a gitlink".into(),
            Box::new(|f: &mut Facts| f.index[0].mode = "100644".into()),
        ),
        (
            "a tracked file beneath the gitlink".into(),
            Box::new(|f: &mut Facts| {
                f.index.push(IndexEntry {
                    mode: "100644".into(),
                    object: "0".repeat(40),
                    stage: "0".into(),
                    path: format!("{SUBMODULE}/crates/astream-cap/src/lib.rs"),
                });
            }),
        ),
        (
            "no index entry".into(),
            Box::new(|f: &mut Facts| f.index.clear()),
        ),
        (
            "a checkout off the gitlink".into(),
            Box::new(|f: &mut Facts| {
                f.checkout = Checkout::At {
                    head: "0".repeat(40),
                    modified: Vec::new(),
                };
            }),
        ),
        (
            "an uninitialised submodule".into(),
            Box::new(|f: &mut Facts| f.checkout = Checkout::Uninitialised("empty".into())),
        ),
        (
            "a dirty checkout".into(),
            Box::new(|f: &mut Facts| {
                if let Checkout::At { modified, .. } = &mut f.checkout {
                    modified.push(" M crates/astream-cap/src/lib.rs".into());
                }
            }),
        ),
        (
            "an astream crate made a workspace member".into(),
            Box::new(|f: &mut Facts| {
                let id = package(f, "astream-wire")["id"].clone();
                f.metadata
                    .as_mut()
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .get_mut("workspace_members")
                    .unwrap()
                    .as_array_mut()
                    .unwrap()
                    .push(id);
            }),
        ),
        (
            "an astream crate from the registry".into(),
            Box::new(|f: &mut Facts| {
                package(f, "astream-broker").insert(
                    "source".into(),
                    Value::String("registry+https://github.com/rust-lang/crates.io-index".into()),
                );
            }),
        ),
        (
            "an astream crate under another license".into(),
            Box::new(|f: &mut Facts| {
                package(f, "astream-digest").insert("license".into(), Value::String("MIT".into()));
            }),
        ),
        (
            "an astream crate resolved twice".into(),
            Box::new(|f: &mut Facts| {
                let copy = Value::Object(package(f, "astream-cap").clone());
                packages(f).push(copy);
            }),
        ),
        (
            "cargo metadata refused".into(),
            Box::new(|f: &mut Facts| f.metadata = Err("Cargo metadata refused: planted".into())),
        ),
        (
            "no workspace exclude".into(),
            Box::new(|f: &mut Facts| f.exclude.clear()),
        ),
        (
            "only a sibling excluded".into(),
            Box::new(|f: &mut Facts| {
                f.exclude = vec!["vendor/astream-other".into(), "vendor/ast".into()];
            }),
        ),
        (
            "aterm-link redirected away".into(),
            Box::new(|f: &mut Facts| {
                let deps = package(f, "aterm-link")
                    .get_mut("dependencies")
                    .unwrap()
                    .as_array_mut()
                    .unwrap();
                for d in deps {
                    if d["name"].as_str() == Some("astream-cap") {
                        d.as_object_mut()
                            .unwrap()
                            .insert("path".into(), Value::String("/outside".into()));
                    }
                }
            }),
        ),
        (
            "an undeclared backward pin".into(),
            Box::new({
                let base = base.clone();
                move |f: &mut Facts| {
                    f.motion = PinMotion::Backward {
                        base: base.clone(),
                        declared: None,
                    };
                }
            }),
        ),
        (
            "a base the checkout never fetched".into(),
            Box::new({
                let base = base.clone();
                move |f: &mut Facts| f.motion = PinMotion::BaseNotFetched { base: base.clone() }
            }),
        ),
    ];
    for name in FIXTURE {
        refused.push((
            format!("{name} redirected"),
            Box::new(move |f: &mut Facts| {
                package(f, name).insert(
                    "manifest_path".into(),
                    Value::String("/outside/Cargo.toml".into()),
                );
            }),
        ));
    }
    for (name, plant) in &refused {
        let mut planted = facts.clone();
        plant(&mut planted);
        assert!(validate(&root, &planted).is_err(), "{name} must be refused");
    }

    let mut accepted: Vec<Facts> = Vec::new();
    for spelling in [URL.to_string(), format!("{URL}.git")] {
        let mut f = facts.clone();
        set_url(&mut f, &spelling);
        accepted.push(f);
    }
    let mut f = facts.clone();
    f.exclude = vec!["vendor/".into()];
    accepted.push(f);
    for motion in [
        PinMotion::Backward {
            base,
            declared: Some("astream 5021230 broke the sealed wire".into()),
        },
        PinMotion::Unchanged,
        PinMotion::Forward,
        PinMotion::Diverged,
        PinMotion::Undecided("no origin/main".into()),
    ] {
        let mut f = facts.clone();
        f.motion = motion;
        accepted.push(f);
    }
    for f in &accepted {
        assert!(validate(&root, f).is_ok(), "{f:?} must pass");
    }
}

#[test]
fn an_astream_crate_no_list_names_is_held_to_the_submodule_too() {
    let (root, facts) = valid();
    assert_eq!(validate(&root, &facts).unwrap().crates, FIXTURE);
    let add = |facts: &mut Facts, name: &str, manifest: &Path| {
        let id = format!("path+file://{}#0.1.0", manifest.parent().unwrap().display());
        facts
            .metadata
            .as_mut()
            .unwrap()
            .as_object_mut()
            .unwrap()
            .get_mut("packages")
            .unwrap()
            .as_array_mut()
            .unwrap()
            .push(json!({
                "id": id,
                "name": name,
                "source": null,
                "license": "Apache-2.0",
                "manifest_path": manifest.to_str().unwrap(),
                "dependencies": [],
            }));
    };
    let at_pin = |name: &str| {
        root.join(SUBMODULE)
            .join("crates")
            .join(name)
            .join("Cargo.toml")
    };
    let mut sibling = facts.clone();
    let elsewhere = Path::new("/astream/crates/astream-next/Cargo.toml");
    add(&mut sibling, "astream-next", elsewhere);
    rejects(
        &root,
        &sibling,
        &["redirects `astream-next`", "/astream/crates/astream-next"],
    );
    let mut licensed = facts.clone();
    add(&mut licensed, "astream-next", &at_pin("astream-next"));
    package(&mut licensed, "astream-next").insert("license".into(), Value::String("MIT".into()));
    rejects(
        &root,
        &licensed,
        &["`astream-next`", "declares license MIT"],
    );
    let mut pinned = facts;
    add(&mut pinned, "astream-next", &at_pin("astream-next"));
    add(&mut pinned, "asb", &at_pin("asb"));
    let crates = validate(&root, &pinned)
        .expect("both resolve from the pin")
        .crates;
    assert!(
        ["asb", "astream-next"]
            .iter()
            .all(|n| crates.iter().any(|c| c == n)),
        "{crates:?}"
    );
    let id = package(&mut pinned, "asb")["id"].clone();
    pinned
        .metadata
        .as_mut()
        .unwrap()
        .as_object_mut()
        .unwrap()
        .get_mut("workspace_members")
        .unwrap()
        .as_array_mut()
        .unwrap()
        .push(id);
    rejects(&root, &pinned, &["`asb` is an aterm workspace member"]);
}

struct Fixture {
    root: aterm_tempfile::TempDir,
}

impl Fixture {
    fn new(tag: &str) -> Fixture {
        let root = aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-forge-pin-{tag}-"))
            .tempdir()
            .unwrap();
        std::fs::create_dir_all(root.path().join(SUBMODULE)).unwrap();
        let f = Fixture { root };
        f.run(f.root.path(), &["init", "-q", "-b", "main"]);
        f.run(
            &f.root.path().join(SUBMODULE),
            &["init", "-q", "-b", "main"],
        );
        f
    }

    fn run(&self, dir: &Path, args: &[&str]) -> String {
        let mut cmd = command(aterm_uds::os_path!("git"));
        cmd.args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .current_dir(dir);
        for var in REPO_ENV {
            cmd.env_remove(var);
        }
        let out = cmd.output_owned().unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    fn astream_commit(&self, parent: Option<&str>, msg: &str) -> String {
        let sub = self.root.path().join(SUBMODULE);
        if let Some(p) = parent {
            self.run(&sub, &["checkout", "-q", "--detach", p]);
        }
        self.run(&sub, &["commit", "-q", "--allow-empty", "-m", msg]);
        self.run(&sub, &["rev-parse", "HEAD"])
    }

    fn commit_pin(&self, parent: Option<&str>, pin: &str, msg: &str) -> String {
        if let Some(p) = parent {
            self.run(self.root.path(), &["checkout", "-q", "--detach", p]);
        }
        let cacheinfo = format!("160000,{pin},{SUBMODULE}");
        self.run(
            self.root.path(),
            &["update-index", "--add", "--cacheinfo", &cacheinfo],
        );
        self.run(self.root.path(), &["commit", "-q", "-m", msg]);
        self.run(self.root.path(), &["rev-parse", "HEAD"])
    }

    fn motion(&self) -> PinMotion {
        for var in REPO_ENV {
            assert!(
                std::env::var_os(var).is_none(),
                "{var} is exported: unset it, or this test reads a real repository"
            );
        }
        pin_motion(self.root.path(), &index(self.root.path()).unwrap())
    }
}

#[test]
fn the_pin_is_judged_against_origin_main_at_the_merge_base_with_real_git() {
    let f = Fixture::new("motion");
    let c1 = f.astream_commit(None, "c1");
    let c2 = f.astream_commit(None, "c2");
    let c3 = f.astream_commit(None, "c3");
    let sibling = f.astream_commit(Some(&c1), "sibling of c2");

    let base = f.commit_pin(None, &c2, "base: the pin is c2");
    assert_eq!(
        f.motion(),
        PinMotion::Unchanged,
        "no {BASE_REF} to judge by"
    );
    f.run(
        f.root.path(),
        &["update-ref", "refs/remotes/origin/main", &base],
    );

    assert_eq!(f.motion(), PinMotion::Unchanged);

    f.commit_pin(Some(&base), &c3, "bump to c3");
    assert_eq!(f.motion(), PinMotion::Forward);

    f.commit_pin(Some(&base), &c1, "an unrelated fix that drags the pin back");
    assert_eq!(
        f.motion(),
        PinMotion::Backward {
            base: c2.clone(),
            declared: None
        }
    );

    f.commit_pin(
        Some(&base),
        &c1,
        "revert astream\n\nastream-pin-rollback: c2 broke the sealed wire",
    );
    let declared = PinMotion::Backward {
        base: c2.clone(),
        declared: Some("c2 broke the sealed wire".into()),
    };
    assert_eq!(f.motion(), declared);
    f.run(
        f.root.path(),
        &["commit", "-q", "--allow-empty", "-m", "adapt aterm-link"],
    );
    assert_eq!(
        f.motion(),
        declared,
        "the trailer is not confined to the tip"
    );
    for msg in [
        "revert\n\nastream-pin-rollback:   ",
        "revert\n\n    astream-pin-rollback: <reason> is the escape hatch",
        "revert\n\n> astream-pin-rollback: quoted",
    ] {
        f.commit_pin(Some(&base), &c1, msg);
        assert!(
            matches!(f.motion(), PinMotion::Backward { declared: None, .. }),
            "{msg:?} must not declare a rollback"
        );
    }

    f.commit_pin(Some(&base), &sibling, "sideways to a sibling of c2");
    assert_eq!(f.motion(), PinMotion::Diverged);

    f.commit_pin(Some(&base), &"f".repeat(40), "a pin nobody fetched");
    assert!(
        matches!(f.motion(), PinMotion::Undecided(_)),
        "{:?}",
        f.motion()
    );
}

#[test]
fn a_stale_checkout_that_never_fetched_the_base_is_caught_not_skipped() {
    let f = Fixture::new("stale");
    let c1 = f.astream_commit(None, "c1");
    let unfetched = "a".repeat(40);
    let base = f.commit_pin(None, &unfetched, "main: pinned to a commit not held here");
    f.run(
        f.root.path(),
        &["update-ref", "refs/remotes/origin/main", &base],
    );
    f.commit_pin(
        Some(&base),
        &c1,
        "an unrelated fix re-recording the old gitlink",
    );
    assert_eq!(
        f.motion(),
        PinMotion::BaseNotFetched { base: unfetched },
        "a pin the checkout holds, against a base it does not, is no bump from it"
    );
}

#[test]
fn no_gitlink_at_the_merge_base_means_no_earlier_pin_to_fall_behind() {
    let f = Fixture::new("nobase");
    let c1 = f.astream_commit(None, "c1");
    std::fs::write(f.root.path().join("README"), "x").unwrap();
    f.run(f.root.path(), &["add", "README"]);
    f.run(
        f.root.path(),
        &["commit", "-q", "-m", "base: no submodule yet"],
    );
    let base = f.run(f.root.path(), &["rev-parse", "HEAD"]);
    f.run(
        f.root.path(),
        &["update-ref", "refs/remotes/origin/main", &base],
    );
    f.commit_pin(Some(&base), &c1, "add the submodule");
    assert_eq!(f.motion(), PinMotion::Unchanged);
}

#[test]
fn a_local_branch_named_origin_main_is_not_the_base() {
    let f = Fixture::new("shadow");
    let c1 = f.astream_commit(None, "c1");
    let c2 = f.astream_commit(None, "c2");
    let base = f.commit_pin(None, &c2, "the pin is c2");
    f.run(f.root.path(), &["branch", "origin/main", &base]);
    f.commit_pin(Some(&base), &c1, "goes back to c1");
    assert_eq!(f.motion(), PinMotion::Unchanged);
}

#[test]
fn the_pin_judgments_verify_makes_first_hold_the_checkout_and_the_motion_with_real_git() {
    let f = Fixture::new("verify");
    let root = f.root.path();
    let a = f.astream_commit(None, "a");
    let b = f.astream_commit(None, "b");
    let c = f.astream_commit(None, "c");
    let at = |commit: &str| {
        f.run(
            &root.join(SUBMODULE),
            &["checkout", "-q", "--detach", commit],
        )
    };
    let judged = || {
        for var in REPO_ENV {
            assert!(
                std::env::var_os(var).is_none(),
                "{var} is exported: unset it, or this test reads a real repository"
            );
        }
        astream_pin(root)
    };

    std::fs::write(root.join("README"), "x").unwrap();
    f.run(root, &["add", "README"]);
    f.run(root, &["commit", "-q", "-m", "no submodule yet"]);
    assert_eq!(judged(), Ok(None), "no gitlink, nothing to judge");

    let base = f.commit_pin(None, &a, "main pins a");
    f.run(root, &["update-ref", "refs/remotes/origin/main", &base]);
    at(&a);
    assert_eq!(judged(), Ok(Some(a.clone())));

    // A bump that main later overtook still went forward: the pin is judged against main's at
    // the merge-base, never against main's tip.
    let topic = f.commit_pin(Some(&base), &b, "topic bumps to b");
    let main = f.commit_pin(Some(&base), &c, "main moves on to c");
    f.run(root, &["update-ref", "refs/remotes/origin/main", &main]);
    f.run(root, &["checkout", "-q", "--detach", &topic]);
    at(&b);
    assert_eq!(judged(), Ok(Some(b.clone())));

    // A merge of main that records the branch's old pin moves it back behind c.
    f.run(
        root,
        &[
            "merge",
            "-q",
            "--no-ff",
            "-s",
            "ours",
            "-m",
            "merge main",
            &main,
        ],
    );
    let why = judged().expect_err("b is a strict ancestor of c, main's pin at the merge-base");
    assert!(
        why.contains("ANCESTOR") && why.contains(&c) && why.contains(ROLLBACK_TRAILER),
        "{why}"
    );
    f.run(
        root,
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "keep b\n\nastream-pin-rollback: c broke the sealed wire",
        ],
    );
    assert_eq!(judged(), Ok(Some(b.clone())), "a declared rollback passes");

    // main's own tree with the submodule left at the branch's old pin: the build would compile b.
    f.run(root, &["checkout", "-q", "--detach", &main]);
    let why = judged().expect_err("the checkout is off the pin");
    assert!(
        why.contains(&format!("checked out at {b}")) && why.contains("git submodule update"),
        "{why}"
    );
    at(&c);
    assert_eq!(judged(), Ok(Some(c)));
}
