// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::changed::{Member, Selection, Toolchain};

const ROOT: &str = "[workspace]\nresolver='2'\nmembers=['user','dev','downstream']\n";
const POLICY: &str = "vendor/trust-exec-policy";

struct Fixture {
    directory: std::path::PathBuf,
    path: std::ffi::OsString,
}
impl Fixture {
    fn new(existing: bool) -> Self {
        let directory = crate::mktemp_dir("atv-independent-exclusion").unwrap();
        let directory = directory.canonicalize().unwrap();
        let fixture = Self {
            directory,
            path: std::env::var_os("PATH").unwrap_or_default(),
        };
        fixture.write("Cargo.toml", ROOT);
        for (name, dependencies) in [
            ("user", ""),
            ("dev", "[dev-dependencies]\nuser={path='../user'}\n"),
            ("downstream", "[dependencies]\ndev={path='../dev'}\n"),
        ] {
            fixture.write(
                &format!("{name}/Cargo.toml"),
                &format!(
                    "[package]\nname='{name}'\nversion='0.1.0'\nedition='2021'\n{dependencies}"
                ),
            );
            fixture.write(&format!("{name}/src/lib.rs"), "");
        }
        if existing {
            fixture.write(&format!("{POLICY}/old.txt"), "already tracked");
        }
        fixture.git(&["init", "-q"]);
        fixture.git(&["add", "-A"]);
        fixture.git(&["commit", "-qm", "base"]);
        fixture.git(&["branch", "base"]);
        fixture
    }
    fn write(&self, path: &str, text: &str) {
        let path = self.directory.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    fn git(&self, args: &[&str]) {
        let output = git(&self.directory, &self.path)
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.invalid",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }
    fn added(&self, path: &str) -> String {
        self.write(
            &format!("{path}/Cargo.toml"),
            "[workspace]\n[package]\nname='trust-exec-policy'\nversion='0.13.0'\nedition='2021'\n",
        );
        self.write(&format!("{path}/src/lib.rs"), "pub fn policy() {}\n");
        format!("{ROOT}exclude=['{path}']\n")
    }
    fn tools(&self) -> Toolchain {
        let mut tools = Toolchain::discover(
            Some(Path::new("/nonexistent")),
            Path::new("/nonexistent"),
            OsStr::new(""),
            None,
            None,
        );
        tools.targo = env!("CARGO").into();
        tools.refused = None;
        tools
    }
    fn members(&self) -> Members {
        Members::new(
            ["user", "dev", "downstream"]
                .map(|name| Member {
                    name: name.into(),
                    dir: name.into(),
                    has_lib: true,
                })
                .into(),
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[test]
fn new_independent_exclusion_keeps_real_normal_and_dev_dependents() {
    let fixture = Fixture::new(false);
    let manifest = fixture.added(POLICY);
    fixture.write("Cargo.toml", &format!(
        "{manifest}[workspace.dependencies]\npolicy={{package='trust-exec-policy',path='{POLICY}'}}\n"));
    fixture.write("user/Cargo.toml", "[package]\nname='user'\nversion='0.1.0'\nedition='2021'\n[dependencies]\npolicy={workspace=true}\n");
    let selection = crate::changed::resolve_paths(
        &fixture.directory,
        &fixture.tools(),
        &fixture.path,
        "base",
        vec![
            "Cargo.toml".into(),
            "user/Cargo.toml".into(),
            format!("{POLICY}/src/lib.rs"),
        ],
        Some("base"),
    );
    match selection {
        Selection::Narrowed { crates, .. } => assert_eq!(crates, ["dev", "user"]),
        other => panic!("{other:?}"),
    }
    // Same declared package, but now only another target can reach it. The
    // existing target-all scan plus host inversion must still widen.
    fixture.write("user/Cargo.toml", "[package]\nname='user'\nversion='0.1.0'\nedition='2021'\n[target.'cfg(target_os=\"hermit\")'.dependencies]\npolicy={workspace=true}\n");
    assert!(matches!(
        crate::changed::resolve_paths(
            &fixture.directory,
            &fixture.tools(),
            &fixture.path,
            "base",
            vec!["Cargo.toml".into(), format!("{POLICY}/src/lib.rs")],
            Some("base"),
        ),
        Selection::Widened(_)
    ));
}

#[test]
fn new_independent_exclusion_requires_absence_membership_and_known_provenance() {
    let fixture = Fixture::new(false);
    let manifest = fixture.added(POLICY);
    let pick = |work: &str| {
        replan(
            &fixture.directory,
            &fixture.path,
            "base",
            Some(ROOT),
            Some(work),
            &fixture.members(),
        )
    };
    assert!(matches!(pick(&manifest), Some(Replan::Packages(_))));
    assert_eq!(
        replan(
            &fixture.directory,
            &fixture.path,
            "base",
            Some(ROOT),
            Some(&manifest),
            &Members::default()
        ),
        None,
        "an unreadable or empty membership graph proves nothing"
    );
    assert_eq!(
        replan(
            &fixture.directory,
            &fixture.path,
            "no-such-base",
            Some(ROOT),
            Some(&manifest),
            &fixture.members()
        ),
        None,
        "a failed base-tree read is not evidence of absence"
    );
    for altered in [
        manifest.replace(
            "members=['user','dev','downstream']",
            "members=['user','dev']",
        ),
        format!("{manifest}[profile.dev]\nopt-level=1\n"),
        manifest.replace(POLICY, "vendor/*"),
    ] {
        assert_eq!(pick(&altered), None, "{altered}");
    }
    let unknown = fixture.added("vendor/unreviewed");
    assert_eq!(
        pick(&unknown),
        None,
        "an unknown vendor root still receives the wide Forge verdict"
    );
    fixture.write(
        &format!("{POLICY}/Cargo.toml"),
        "[workspace]\nmembers=['nested']\n[package]\nname='trust-exec-policy'\nversion='0.13.0'\n",
    );
    assert_eq!(
        pick(&manifest),
        None,
        "a multi-package workspace is not this proof"
    );
    let existing = Fixture::new(true);
    let work = existing.added(POLICY);
    assert_eq!(
        replan(
            &existing.directory,
            &existing.path,
            "base",
            Some(ROOT),
            Some(&work),
            &existing.members()
        ),
        None,
        "excluding an existing directory can remove old members"
    );
}
