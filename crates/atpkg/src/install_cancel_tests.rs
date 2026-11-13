// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn cancelled_operation_preserves_the_live_build_and_releases_its_store_lock() {
    cancelled_replacement(false);
}

#[test]
fn expired_budget_preserves_the_live_build_and_releases_its_store_lock() {
    cancelled_replacement(true);
}

fn cancelled_replacement(budget: bool) {
    const CHILD: &str = "ATPKG_CANCEL_BOUNDARY_TEST";
    if let Some(root) = std::env::var_os(CHILD) {
        let layout = crate::store::Layout {
            prefix: root.into(),
        };
        let lock = crate::lock::try_lock_store(&layout).unwrap();
        let scope = if budget {
            aterm_update_core::OperationCancellation::until(std::time::Instant::now()).unwrap()
        } else {
            aterm_update_core::OperationCancellation::from_stdin().unwrap()
        };
        let _operation = crate::cli::StoreOperation::new(Some(lock), Some(scope));
        let build = layout.build_dir("probe", 1);
        let incoming = layout.prefix.join("incoming");
        std::fs::create_dir_all(&build).unwrap();
        std::fs::create_dir_all(&incoming).unwrap();
        std::fs::write(build.join("bytes"), b"old complete build").unwrap();
        std::fs::write(incoming.join("bytes"), b"incoming build").unwrap();
        crate::store::mark_build_ready(&build).unwrap();
        std::println!("ready");
        let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !aterm_update_core::operation_cancelled() && std::time::Instant::now() < until {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            aterm_update_core::operation_cancelled(),
            "cancellation hang detector"
        );
        let Err(error) = super::swap_into_place(&build, &incoming) else {
            panic!("a cancelled operation must not begin replacement")
        };
        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        assert_eq!(
            std::fs::read(build.join("bytes")).unwrap(),
            b"old complete build"
        );
        assert_eq!(
            std::fs::read(incoming.join("bytes")).unwrap(),
            b"incoming build"
        );
        assert!(crate::store::build_is_complete(&build));
        return;
    }
    let dir = aterm_tempfile::tempdir().unwrap();
    let layout = crate::store::Layout {
        prefix: dir.path().to_path_buf(),
    };
    let (reader, writer) = aterm_uds::pipe().unwrap();
    let mut writer = Some(writer);
    let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    let test = if budget {
        "install::cancel_tests::expired_budget_preserves_the_live_build_and_releases_its_store_lock"
    } else {
        "install::cancel_tests::cancelled_operation_preserves_the_live_build_and_releases_its_store_lock"
    };
    cmd.args(["--exact", test, "--nocapture"])
        .env(CHILD, dir.path());
    let mut output = Vec::new();
    let status = aterm_uds::deadline::stream_with_stdin_until_stopped(
        &mut cmd,
        aterm_uds::spawnfd::Stdio::from(reader),
        Some(std::time::Instant::now() + std::time::Duration::from_secs(60)),
        |_| {},
        |_, chunk| {
            output.extend_from_slice(chunk);
            if writer.is_some()
                && output
                    .windows(b"ready\n".len())
                    .any(|part| part == b"ready\n")
            {
                if !budget {
                    assert!(
                        matches!(
                            crate::lock::try_lock_store(&layout),
                            Err(crate::lock::StoreLockError::Contended(_))
                        ),
                        "the child still holds its real writer lock before cancellation"
                    );
                }
                writer.take();
            }
            Ok(())
        },
        || false,
        None,
    )
    .unwrap()
    .unwrap();
    assert!(status.success(), "{}", String::from_utf8_lossy(&output));
    assert!(
        writer.is_none(),
        "the child proved execution before the cut"
    );
    assert!(
        crate::lock::try_lock_store(&layout).is_ok(),
        "the owning child exited and released its store lock"
    );
}
