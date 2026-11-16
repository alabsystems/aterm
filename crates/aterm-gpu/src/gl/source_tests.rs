// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::compose;

#[test]
fn stages_preserve_version_defines_line_mapping_and_entry_wrappers() {
    let src = "#version 300 es\nprecision highp float;\n";
    assert_eq!(
        compose(src, true, "vs_x", "fs_y").unwrap(),
        "#version 300 es\n#define ATERM_VERTEX 1\n#define ATERM_VS_vs_x 1\n#define ATERM_FS_fs_y 1\n#line 2\nprecision highp float;\n\nvoid main() {\n    vs_x();\n    gl_Position.yz = vec2(-gl_Position.y, gl_Position.z * 2.0 - gl_Position.w);\n}\n"
    );
    assert_eq!(
        compose(src, false, "vs_x", "fs_y").unwrap(),
        "#version 300 es\n#define ATERM_FRAGMENT 1\n#define ATERM_VS_vs_x 1\n#define ATERM_FS_fs_y 1\n#line 2\nprecision highp float;\n\nvoid main() {\n    fs_y();\n}\n"
    );
    for invalid in [
        "",
        "#version 300 es",
        "precision highp float;\n",
        "#version 100\n",
    ] {
        assert_eq!(
            compose(invalid, true, "v", "f").unwrap_err(),
            "a GLSL twin starts with `#version 300 es`"
        );
    }
}
