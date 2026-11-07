// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Failed builds use actual Cargo diagnostics and never infer success from stdout.

use atpkg_publish::artifact::tools::{HostTools, Ran, Run, Tools};

use super::*;

fn failure(world: &World, output: Ran) -> String {
    let meta = declared(world);
    let mut tools = FakeTools::new("aarch64-apple-darwin");
    tools.answer = Box::new(move |run| {
        let args = run.argv();
        if args.first().map(String::as_str) == Some("metadata") {
            Some(ran(0, &meta, ""))
        } else if args.iter().any(|arg| arg == "build") {
            Some(output.clone())
        } else {
            None
        }
    });
    let request = world.request(X86_64_UNKNOWN_LINUX_GNU);
    let error =
        pack::pack_with(&world.ctx, &request, &tools).expect_err("nonzero build must refuse");
    assert!(world.written("pkg-ay-9.toml").is_empty());
    assert!(world.written(".tar.zst").is_empty());
    error.to_string()
}

fn compiler_message(level: &str, rendered: &str) -> String {
    aterm_json::json!({
        "reason": "compiler-message",
        "message": {"level": level, "message": "linking failed", "rendered": rendered}
    })
    .to_string()
}

#[test]
fn actual_cargo_compiler_errors_are_persisted_and_shown_by_the_failed_pack() {
    let work = scratch("plain-real-compiler-diagnostic");
    std::fs::write(
        work.path().join("Cargo.toml"),
        "[package]\nname = \"diagnostic-probe\"\nversion = \"0.0.0\"\nedition = \"2021\"\n[workspace]\n",
    )
    .unwrap();
    std::fs::create_dir(work.path().join("src")).unwrap();
    std::fs::write(
        work.path().join("src/lib.rs"),
        "pub fn mismatch() { let _: u32 = \"not a number\"; }\n",
    )
    .unwrap();
    std::fs::copy(
        aterm_tempfile::manifest_dir().join("../../trust.lock"),
        work.path().join("trust.lock"),
    )
    .unwrap();
    let mut run = Run::new(
        "targo",
        [
            "--unverified",
            "build",
            "--offline",
            "--message-format=json",
        ],
    );
    run.cwd = Some(work.path().to_path_buf());
    run.deadline = Some(Duration::from_secs(60));
    run.env.push((
        "CARGO_TARGET_DIR".into(),
        work.path().join("target.noindex").into_os_string(),
    ));
    let output = HostTools.run(&run).expect("the actual Trust Cargo driver");
    assert!(!output.success());
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("compiler-message"),
        "the real driver must have emitted Cargo JSON"
    );
    let world = world("plain-real-compiler-failure");
    let error = failure(&world, output);
    assert!(error.contains("mismatched types"), "{error}");
    let persisted =
        std::fs::read_to_string(world.root.join("out/ay/build.compiler-errors")).unwrap();
    assert!(persisted.contains("mismatched types"), "{persisted}");
    assert!(error.contains("build.stderr") && error.contains("build.compiler-errors"));
}

#[test]
fn malformed_messages_and_artifacts_cannot_turn_a_nonzero_build_into_success() {
    let world = world("plain-malformed-compiler-failure");
    let output = Ran {
        code: Some(101),
        stdout: format!(
            "{{broken\n{{\"reason\":\"compiler-message\",\"message\":{{\"level\":\"error\",\"rendered\":17}}}}\n{}\n{{\"reason\":\"compiler-artifact\",\"executable\":\"/not/a/success\"}}\n",
            compiler_message("warning", "warning must not be presented as an error")
        )
        .into_bytes(),
        stderr: b"error: could not compile the fixture".to_vec(),
    };
    let error = failure(&world, output);
    assert!(error.contains("malformed Cargo diagnostic omitted"));
    assert!(!error.contains("warning must not"));
    assert!(error.contains("could not compile the fixture"));
}

#[test]
fn long_linker_errors_keep_the_final_symbol_and_redact_before_the_utf8_cut() {
    let world = world("plain-bounded-compiler-failure");
    let secret = format!("ghp_{}", "fixture7".repeat(8));
    let rendered = format!(
        "error: linking with cc failed\n{}\nAuthorization: Bearer {secret}\nhttps://user:password@example.invalid/path?token=secret\nundefined symbol: OrtGetApiBase\n",
        "長".repeat(24_000)
    );
    let output = Ran {
        code: Some(101),
        stdout: compiler_message("error", &rendered).into_bytes(),
        stderr: format!("build failed using {secret}; {}", "終".repeat(8_000)).into_bytes(),
    };
    let error = failure(&world, output);
    let compiler =
        std::fs::read_to_string(world.root.join("out/ay/build.compiler-errors")).unwrap();
    let stderr = std::fs::read_to_string(world.root.join("out/ay/build.stderr")).unwrap();
    assert!(compiler.len() <= 16 * 1024 && stderr.len() <= 4 * 1024);
    assert!(compiler.contains("error: linking") && compiler.contains("OrtGetApiBase"));
    assert!(compiler.contains("[diagnostic truncated]"));
    for text in [&error, &compiler, &stderr] {
        assert!(
            !text.contains(secret.as_str()),
            "a credential-shaped token leaked"
        );
        assert!(!text.contains("user:password") && !text.contains("?token="));
    }
}
