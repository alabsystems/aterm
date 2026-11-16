// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Assemble one GLSL stage directly from borrowed source pieces.

/// One stage of one row, without an owned intermediate entry wrapper.
pub(crate) fn compose(src: &str, vertex: bool, vs: &str, fs: &str) -> Result<String, String> {
    let (version, body) = src
        .split_once('\n')
        .filter(|(v, _)| v.starts_with("#version 300 es"))
        .ok_or("a GLSL twin starts with `#version 300 es`")?;
    let (stage, entry, clip) = if vertex {
        (
            "ATERM_VERTEX",
            vs,
            "    gl_Position.yz = vec2(-gl_Position.y, gl_Position.z * 2.0 - gl_Position.w);\n",
        )
    } else {
        ("ATERM_FRAGMENT", fs, "")
    };
    Ok(format!(
        "{version}\n#define {stage} 1\n#define ATERM_VS_{vs} 1\n#define ATERM_FS_{fs} 1\n#line 2\n{body}\nvoid main() {{\n    {entry}();\n{clip}}}\n"
    ))
}

#[cfg(test)]
#[path = "source_tests.rs"]
mod tests;
