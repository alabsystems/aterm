// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn absent_prefix_and_refused_names_are_structured_without_a_lookup() {
    assert_eq!(read(None, "targo").status, Status::Missing);
    for name in ["../targo", "rustup", "cargo", "alab-sudo"] {
        let record = read(None, name);
        assert_eq!(record.status, Status::Refused, "{name}");
        assert!(record.executable.is_none());
        assert!(record.shim_env.is_empty());
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    fn file(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn shim(layout: &Layout, tool: &str, target: &Path) {
        std::fs::create_dir_all(layout.bin_dir()).unwrap();
        file(
            &layout.shim(&ToolName::new(tool).unwrap()),
            crate::platform::sh_shim_content(target).as_bytes(),
        );
    }

    fn codex_package(layout: &Layout, build: u64) -> PathBuf {
        let root = layout.build_dir("codex", build);
        let exe = root.join("bin/codex");
        file(&exe, b"#!/bin/sh\nexit 0\n");
        std::fs::write(
            root.join("codex-package.json"),
            r#"{"layoutVersion":1,"entrypoint":"bin/codex","version":"0.11.0"}"#,
        )
        .unwrap();
        std::fs::write(root.join("alab-build.json"), b"{}").unwrap();
        crate::store::mark_build_ready(&root).unwrap();
        exe
    }

    #[test]
    fn codex_adoption_resolves_the_guarded_primary_and_the_agent_twin() {
        use std::os::unix::fs::symlink;
        let scratch = aterm_tempfile::tempdir().unwrap();
        let layout = Layout {
            prefix: scratch.path().canonicalize().unwrap(),
        };
        let old = codex_package(&layout, 2026111601);
        let new = codex_package(&layout, 2026111701);
        let tool = ToolName::new("codex").unwrap();
        let primary = layout.shim(&tool);
        let twin = layout.agent_shim(&tool);
        let stable = crate::activate::exec_dir(&layout, "codex").join("codex");
        let read = || selected_alab_entry(&layout, "codex");
        let selected = Some((2026111701, new.clone()));
        file(
            &primary,
            crate::platform::sh_shim_content_managed(
                &new,
                &crate::shim_env::ShimEnv::NONE,
                &stable,
                "",
            )
            .as_bytes(),
        );
        assert_eq!(
            read(),
            selected,
            "guard absent, legacy primary execs its direct target"
        );
        std::fs::create_dir_all(stable.parent().unwrap()).unwrap();
        symlink(layout.prefix.as_path(), &stable).unwrap();
        assert!(
            read().is_none(),
            "the shell tries to exec a searchable directory and never reaches its fallback"
        );
        std::fs::remove_file(&stable).unwrap();
        symlink(&old, &stable).unwrap();
        assert!(
            read().is_none(),
            "guard would exec A even though the primary declares B"
        );
        std::fs::remove_file(&stable).unwrap();
        symlink(&new, &stable).unwrap();
        file(&twin, crate::platform::sh_shim_content(&old).as_bytes());
        assert!(read().is_none(), "an installed twin still launches A");
        file(&twin, crate::platform::sh_shim_content(&stable).as_bytes());
        let reroute = layout.reroute_dir().join("codex");
        file(
            &reroute,
            crate::reroute::agents_stub_body_sh("codex", &layout.agents_dir()).as_bytes(),
        );
        assert_eq!(
            read(),
            selected,
            "reroute, twin, stable entry and primary agree"
        );
        let foreign = scratch.path().join("foreign-codex");
        file(&foreign, b"#!/bin/sh\nexit 0\n");
        std::fs::remove_file(&stable).unwrap();
        symlink(foreign, &stable).unwrap();
        assert!(
            read().is_none(),
            "a foreign executable cannot inherit the store build"
        );
        std::fs::remove_file(&stable).unwrap();
        assert!(
            read().is_none(),
            "the unconditional twin is broken although bin can fall back"
        );
        std::fs::remove_file(&twin).unwrap();
        assert!(
            read().is_none(),
            "a reroute without a twin can choose an unknown PATH fallback"
        );
        std::fs::remove_file(&reroute).unwrap();
        assert_eq!(read(), selected);
    }

    #[test]
    fn codex_adoption_requires_positive_completion_and_admitted_launchers() {
        use std::os::unix::fs::symlink;
        let scratch = aterm_tempfile::tempdir().unwrap();
        let layout = Layout {
            prefix: scratch.path().canonicalize().unwrap(),
        };
        let build = 2026111701;
        let root = layout.build_dir("codex", build);
        let exe = codex_package(&layout, build);
        let marker = crate::store::ready_marker_path(&root).unwrap();
        shim(&layout, "codex", &exe);
        let read = || selected_alab_entry(&layout, "codex");
        assert_eq!(read(), Some((build, exe.clone())));
        std::fs::remove_file(&marker).unwrap();
        assert!(
            read().is_none() && ready_alab_entry(&layout, "codex", build).is_none(),
            "entrypoint and package metadata alone do not establish a complete install"
        );
        let other = scratch.path().join("other-marker");
        file(&other, b"ok\n");
        symlink(other, &marker).unwrap();
        assert!(
            read().is_none() && ready_alab_entry(&layout, "codex", build).is_none(),
            "an unreadable linked marker cannot establish a positive identity"
        );
        std::fs::remove_file(&marker).unwrap();
        crate::store::mark_build_ready(&root).unwrap();
        file(
            &layout.shim(&ToolName::new("codex").unwrap()),
            format!(
                "#!/bin/sh\nexit 1\nexec {} \"$@\"\n",
                crate::platform::sh_shim_quote(&exe)
            )
            .as_bytes(),
        );
        assert!(
            read().is_none(),
            "an arbitrary wrapper can contain a plausible unexecuted fallback"
        );
    }

    fn build(layout: &Layout) -> PathBuf {
        let root = layout.build_dir("trust", 41);
        file(&root.join("bin/targo"), b"#!/bin/sh\nexit 99\n");
        shim(layout, "targo", &root.join("bin/targo"));
        root
    }

    fn status(layout: &Layout, installed: u64, pinned: u64) {
        crate::status::write(
            layout,
            &crate::status::Status {
                schema: 1,
                updated_at: "2026-11-05T00:00:00Z".into(),
                index_source: "alabsystems/aterm".into(),
                programs: BTreeMap::from([(
                    "trust".into(),
                    crate::status::ProgramStatus {
                        installed_build: Some(installed),
                        state: crate::state::managed(pinned, 7),
                        tree_root: "signed-tree-root".into(),
                    },
                )]),
                ..Default::default()
            },
        )
        .unwrap();
    }

    fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut found = BTreeMap::new();
        if !root.exists() {
            return found;
        }
        for entry in std::fs::read_dir(root).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            let mut bytes =
                format!("{:?}", (meta.mode(), meta.mtime(), meta.mtime_nsec())).into_bytes();
            if entry.file_type().unwrap().is_dir() {
                found.insert(path.clone(), bytes);
                found.extend(snapshot(&path));
            } else if entry.file_type().unwrap().is_symlink() {
                bytes.extend_from_slice(
                    std::fs::read_link(&path)
                        .unwrap()
                        .as_os_str()
                        .as_encoded_bytes(),
                );
                found.insert(path, bytes);
            } else {
                bytes.extend_from_slice(&std::fs::read(&path).unwrap());
                found.insert(path, bytes);
            }
        }
        found
    }

    #[test]
    fn physical_paths_with_spaces_and_recorded_pin_survive_json() {
        let scratch = crate::testdir::layout("lookup spaces ");
        let root = build(&scratch);
        status(&scratch, 41, 41);
        let before = snapshot(&scratch.prefix);
        let record = read(Some(&scratch), "targo");
        assert_eq!(record.status, Status::Ready);
        assert!(record.managed);
        assert_eq!(record.package.as_deref(), Some("trust"));
        assert_eq!(record.build, Some(41));
        assert_eq!(record.executable, Some(root.join("bin/targo")));
        assert_eq!(record.physical_root, Some(root.canonicalize().unwrap()));
        let json: aterm_json::Value =
            aterm_json::from_str(&aterm_json::to_string(&record).unwrap()).unwrap();
        assert_eq!(json["schema"].as_u64(), Some(1));
        assert_eq!(json["status"].as_str(), Some("ready"));
        assert_eq!(json["provenance"]["index_build"].as_u64(), Some(7));
        assert_eq!(
            json["provenance"]["tree_root"].as_str(),
            Some("signed-tree-root")
        );
        assert_eq!(before, snapshot(&scratch.prefix));
    }

    #[test]
    fn missing_and_pending_entries_never_launch_or_create_state() {
        let scratch = crate::testdir::layout("lookup-pending-");
        std::fs::create_dir_all(scratch.bin_dir()).unwrap();
        assert_eq!(read(Some(&scratch), "targo").status, Status::Missing);
        crate::stub::write_pending_stub(&scratch, &ToolName::new("targo").unwrap()).unwrap();
        let marker = scratch.prefix.join("must-not-run");
        let stub = scratch.shim(&ToolName::new("targo").unwrap());
        let mut bytes = std::fs::read(&stub).unwrap();
        bytes.extend_from_slice(format!("\ntouch '{}'\n", marker.display()).as_bytes());
        std::fs::write(&stub, bytes).unwrap();
        let before = snapshot(&scratch.prefix);
        let record = read(Some(&scratch), "targo");
        assert_eq!(record.status, Status::Pending);
        assert_eq!(record.package.as_deref(), Some("trust"));
        assert!(record.executable.is_none() && record.shim_env.is_empty());
        assert!(!marker.exists());
        assert_eq!(before, snapshot(&scratch.prefix));
    }

    #[test]
    fn executable_and_shim_environment_are_read_without_executing_them() {
        let scratch = crate::testdir::layout("lookup-env-");
        let root = build(&scratch);
        let marker = scratch.prefix.join("must-not-run");
        file(
            &root.join("bin/targo"),
            format!("#!/bin/sh\ntouch '{}'\n", marker.display()).as_bytes(),
        );
        let env = crate::shim_env::ShimEnv::admit(&["DISABLE_AUTOUPDATER=1".into()]).unwrap();
        crate::shim_env::write_sidecar(&root, &env).unwrap();
        let before = snapshot(&scratch.prefix);
        let record = read(Some(&scratch), "targo");
        assert_eq!(record.status, Status::Ready);
        assert_eq!(
            record.shim_env,
            BTreeMap::from([("DISABLE_AUTOUPDATER".into(), "1".into())])
        );
        assert!(!marker.exists());
        assert_eq!(before, snapshot(&scratch.prefix));
    }

    #[test]
    fn compatibility_routing_reports_both_actual_and_payload_roots() {
        let scratch = crate::testdir::layout("lookup-compat-");
        let root = build(&scratch);
        for (name, bytes) in [
            ("trustc", b"trust compiler".as_slice()),
            ("rustc", b"stock copy"),
            ("tippy", b"lint"),
        ] {
            file(&root.join("bin").join(name), bytes);
        }
        crate::compat::ensure_root(&scratch, &root, crate::seam::Depth::Shallow).unwrap();
        let routed = crate::compat::root_dir(&scratch, 41);
        let before = snapshot(&scratch.prefix);
        let record = read(Some(&scratch), "targo");
        assert_eq!(record.status, Status::Ready);
        assert!(record.managed);
        assert_eq!(record.executable, Some(routed.join("bin/targo")));
        assert_eq!(record.physical_root, Some(routed.canonicalize().unwrap()));
        assert_eq!(record.store_root, Some(root.canonicalize().unwrap()));
        assert_eq!(before, snapshot(&scratch.prefix));
    }

    #[test]
    fn development_links_are_not_claimed_as_managed_or_index_pinned() {
        let scratch = crate::testdir::layout("lookup-dev-");
        let target = scratch.prefix.join("checkout/bin/targo");
        file(&target, b"development tool");
        shim(&scratch, "targo", &target);
        status(&scratch, 41, 41);
        let record = read(Some(&scratch), "targo");
        assert_eq!(record.status, Status::Ready);
        assert!(!record.managed);
        assert!(record.package.is_none() && record.build.is_none());
        assert!(record.store_root.is_none() && record.physical_root.is_none());
        assert!(record.provenance.is_none());
    }

    #[test]
    fn stale_or_unreadable_status_cannot_attribute_a_different_builds_pin() {
        let scratch = crate::testdir::layout("lookup-stale-");
        build(&scratch);
        for (installed, pinned) in [(40, 40), (40, 41), (41, 40)] {
            status(&scratch, installed, pinned);
            let record = read(Some(&scratch), "targo");
            assert_eq!(record.status, Status::Ready);
            assert_eq!(record.provenance.unwrap().index_build, None);
        }
        std::fs::write(scratch.status(), b"malformed and not echoed").unwrap();
        let record = read(Some(&scratch), "targo");
        assert_eq!(record.status, Status::Ready);
        assert!(record.provenance.is_none());
        assert_eq!(
            record.reason,
            Some("local package status provenance is unreadable")
        );
    }

    #[test]
    fn missing_non_executable_and_escaping_targets_do_not_report_ready() {
        let scratch = crate::testdir::layout("lookup-refused-");
        let root = build(&scratch);
        let target = root.join("bin/targo");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(read(Some(&scratch), "targo").status, Status::Refused);
        std::fs::remove_file(&target).unwrap();
        assert_eq!(read(Some(&scratch), "targo").status, Status::Missing);
        let outside = scratch.prefix.join("outside");
        file(&outside, b"unrelated executable");
        std::os::unix::fs::symlink(&outside, &target).unwrap();
        let record = read(Some(&scratch), "targo");
        assert_eq!(record.status, Status::Refused);
        assert!(!record.managed && record.executable.is_none());
    }

    #[test]
    fn only_the_owned_noindex_build_alias_is_admitted() {
        let scratch = crate::testdir::layout("lookup-aside-");
        let root = build(&scratch);
        let aside = aterm_toolchain::layout::aside_of(&root).unwrap();
        std::fs::rename(&root, &aside).unwrap();
        std::os::unix::fs::symlink(aside.file_name().unwrap(), &root).unwrap();
        let record = read(Some(&scratch), "targo");
        assert_eq!(record.status, Status::Ready);
        assert_eq!(record.executable, Some(root.join("bin/targo")));
        assert_eq!(
            record.physical_executable,
            Some(aside.join("bin/targo").canonicalize().unwrap())
        );
        assert_eq!(record.store_root, Some(aside.canonicalize().unwrap()));
        std::fs::remove_file(&root).unwrap();
        let outside = scratch.prefix.join("unowned");
        std::fs::rename(&aside, &outside).unwrap();
        std::os::unix::fs::symlink(&outside, &root).unwrap();
        let record = read(Some(&scratch), "targo");
        assert_eq!(record.status, Status::Refused);
        assert!(!record.managed && record.executable.is_none());
    }

    #[test]
    fn the_launch_path_preserves_a_tool_alias_while_identifying_its_bytes() {
        let scratch = crate::testdir::layout("lookup-tool-alias-");
        let root = build(&scratch);
        std::os::unix::fs::symlink("targo", root.join("bin/tippy")).unwrap();
        shim(&scratch, "tippy", &root.join("bin/tippy"));
        let record = read(Some(&scratch), "tippy");
        assert_eq!(record.status, Status::Ready);
        assert_eq!(record.executable, Some(root.join("bin/tippy")));
        assert_eq!(
            record.physical_executable,
            Some(root.join("bin/targo").canonicalize().unwrap())
        );
    }

    #[test]
    fn a_store_or_package_symlink_cannot_claim_an_unowned_tree() {
        for component in ["store", "store/trust"] {
            let scratch = crate::testdir::layout("lookup-parent-link-");
            build(&scratch);
            let source = scratch.prefix.join(component);
            let outside = scratch.prefix.join("unowned");
            std::fs::rename(&source, &outside).unwrap();
            std::os::unix::fs::symlink(&outside, &source).unwrap();
            let record = read(Some(&scratch), "targo");
            assert_eq!(record.status, Status::Refused, "{component}");
            assert!(!record.managed && record.executable.is_none());
        }
    }
}
