// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::pipeline_table::ALL_PIPELINES;

#[test]
fn every_table_row_compiles_and_links_from_its_twin() {
    let dev = super::super::test_device();
    let mut built = Vec::new();
    for p in ALL_PIPELINES {
        let spec = p.spec();
        let pipe = dev
            .pipeline(spec)
            .unwrap_or_else(|e| panic!("row {}: {e}", p.name()));
        assert_eq!(
            pipe.layout.is_some(),
            spec.vertex != crate::pipeline_table::VertexLayout::None,
            "{}",
            p.name()
        );
        built.push(pipe);
    }
    assert_eq!(built.len(), crate::pipeline_table::PIPELINE_COUNT);
}

#[test]
fn a_twin_that_does_not_compile_is_refused_with_the_drivers_log() {
    let dev = super::super::test_device();
    let cur = dev.enter().expect("enter");
    let src = compose(
        shaders::source(crate::pipeline_table::ShaderLibrary::Cell),
        false,
        "vs_bg",
        "fs_bg",
    )
    .expect("compose")
    .replace(
        "o_frag = vec4(s2l(v_color.rgb), v_color.a);",
        "o_frag = undeclared;",
    );
    let err = compile(&cur, ffi::GL_FRAGMENT_SHADER, &src, "fs_bg").expect_err("must not compile");
    assert!(err.contains("does not compile"), "{err}");
    assert!(
        err.len() > "gl: fs_bg does not compile:\n".len(),
        "the driver log is carried: {err}"
    );
}

#[test]
fn compose_selects_the_stage_and_the_row() {
    let src = "#version 300 es\nprecision highp float;\n";
    let v = compose(src, true, "vs_x", "fs_y").expect("compose");
    assert!(
        v.starts_with("#version 300 es\n#define ATERM_VERTEX 1\n"),
        "{v}"
    );
    assert!(v.contains("#define ATERM_VS_vs_x 1\n#define ATERM_FS_fs_y 1\n#line 2\n"));
    assert!(v.contains("vs_x();\n    gl_Position.yz = vec2(-gl_Position.y"));
    let f = compose(src, false, "vs_x", "fs_y").expect("compose");
    assert!(
        f.contains("#define ATERM_FRAGMENT 1") && f.contains("fs_y();"),
        "{f}"
    );
    compose("precision highp float;\n", true, "a", "b").expect_err("no version line");
}
