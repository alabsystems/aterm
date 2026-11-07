// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Only MC's admitted, explicitly donated seal may replace its install link.

use super::*;

fn mc_tools() -> FakeTools {
    let mut t = tools();
    t.answer = Box::new(|run| {
        if run.argv().iter().any(|arg| arg == "--sysroot") {
            return Some(ran(0, "", ""));
        }
        if run.program.ends_with("hello") {
            return Some(ran(0, "hello, atpkg sysroot\n", ""));
        }
        None
    });
    t
}

#[test]
fn the_exact_admitted_donor_replaces_only_the_staged_mc_toolchain_link() {
    let w = world("bundle-mc-bound-toolchain");
    let (mut request, hello) = trust_mc(&w);
    request.donor_roots.push(hello.clone());
    std::os::unix::fs::symlink(&hello, request.sysroot.join("toolchain")).unwrap();
    let t = mc_tools();
    let packed = bundle::pack_bundle_with(&w.ctx, &request, &t).expect("bound MC pack");
    let extracted = w.extract(&packed.asset);
    assert!(std::fs::symlink_metadata(extracted.join("toolchain")).is_err());
    assert_eq!(
        std::fs::read_link(request.sysroot.join("toolchain")).unwrap(),
        hello
    );
    assert!(
        t.runs()
            .iter()
            .any(|run| run.argv().iter().any(|arg| arg == "--sysroot"))
    );
    assert_eq!(packed.exposes, ["trust-mc-driver"]);
}

#[test]
fn another_mc_toolchain_target_is_refused_even_with_the_correct_donor() {
    let w = world("bundle-mc-wrong-toolchain");
    let (mut request, hello) = trust_mc(&w);
    request.donor_roots.push(hello);
    std::os::unix::fs::symlink(&w.seal, request.sysroot.join("toolchain")).unwrap();
    let error = bundle::pack_bundle_with(&w.ctx, &request, &tools()).expect_err("wrong seal");
    assert!(
        error.to_string().contains("not its admitted hello sysroot"),
        "{error}"
    );
}

#[test]
fn a_missing_or_different_donor_cannot_authorize_the_mc_toolchain_link() {
    for different in [false, true] {
        let w = world("bundle-mc-unbound-donor");
        let (mut request, hello) = trust_mc(&w);
        if different {
            request.donor_roots.push(w.seal.clone());
        }
        std::os::unix::fs::symlink(&hello, request.sysroot.join("toolchain")).unwrap();
        let error = bundle::pack_bundle_with(&w.ctx, &request, &tools())
            .expect_err("no matching explicit donor");
        assert!(
            error.to_string().contains("as an explicit --donor-root"),
            "{error}"
        );
    }
}

#[test]
fn other_links_and_other_programs_keep_the_strict_symlink_refusal() {
    let w = world("bundle-mc-other-link");
    let (mut request, hello) = trust_mc(&w);
    request.donor_roots.push(hello.clone());
    std::os::unix::fs::symlink(&hello, request.sysroot.join("toolchain")).unwrap();
    std::os::unix::fs::symlink(&hello, request.sysroot.join("other-link")).unwrap();
    let error = bundle::pack_bundle_with(&w.ctx, &request, &tools()).expect_err("another link");
    assert!(error.to_string().contains("unexpected symlink"), "{error}");

    let w = world("bundle-trust-mc-rule-is-scoped");
    let mut request = w.request();
    request.donor_roots.push(w.seal.clone());
    std::os::unix::fs::symlink(&w.seal, request.sysroot.join("toolchain")).unwrap();
    let error = bundle::pack_bundle_with(&w.ctx, &request, &tools()).expect_err("not MC");
    assert!(error.to_string().contains("unexpected symlink"), "{error}");
}
