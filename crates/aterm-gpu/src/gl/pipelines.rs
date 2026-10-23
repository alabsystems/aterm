// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! GL programs and fixed-function state built from [`crate::pipeline_table`].

use super::device::{Current, GlDevice};
use super::ffi::{self, GLenum, GLuint};
use super::shaders;
use crate::pipeline_table::{
    AttrFormat, Blend, BlendOp, Factor, PipelineSpec, Topology, VertexLayoutSpec, WriteMask,
};

/// The GL spelling of one [`Factor`].
#[inline]
const fn gl_factor(f: Factor) -> GLenum {
    match f {
        Factor::Zero => ffi::GL_ZERO,
        Factor::One => ffi::GL_ONE,
        Factor::SrcAlpha => ffi::GL_SRC_ALPHA,
        Factor::OneMinusSrcAlpha => ffi::GL_ONE_MINUS_SRC_ALPHA,
        Factor::OneMinusSrc => ffi::GL_ONE_MINUS_SRC_COLOR,
    }
}

/// The GL spelling of one [`BlendOp`].
#[inline]
const fn gl_op(op: BlendOp) -> GLenum {
    match op {
        BlendOp::Add => ffi::GL_FUNC_ADD,
    }
}

/// The GL primitive mode of a row's [`Topology`].
#[inline]
const fn gl_mode(t: Topology) -> GLenum {
    match t {
        Topology::TriangleList => ffi::GL_TRIANGLES,
        Topology::TriangleStrip => ffi::GL_TRIANGLE_STRIP,
    }
}

/// The blend state a row sets.
#[inline]
pub(crate) const fn gl_blend(blend: Option<Blend>) -> Option<[GLenum; 6]> {
    let Some(b) = blend else {
        return None;
    };
    if b.is_replace() {
        return None;
    }
    Some([
        gl_factor(b.color.src),
        gl_factor(b.color.dst),
        gl_factor(b.alpha.src),
        gl_factor(b.alpha.dst),
        gl_op(b.color.op),
        gl_op(b.alpha.op),
    ])
}

/// One stage of one row.
pub(crate) fn compose(src: &str, vertex: bool, vs: &str, fs: &str) -> Result<String, String> {
    let (version, body) = src
        .split_once('\n')
        .filter(|(v, _)| v.starts_with("#version 300 es"))
        .ok_or("a GLSL twin starts with `#version 300 es`")?;
    let stage = if vertex {
        "ATERM_VERTEX"
    } else {
        "ATERM_FRAGMENT"
    };
    let main = if vertex {
        format!(
            "void main() {{\n    {vs}();\n    gl_Position.yz = vec2(-gl_Position.y, \
             gl_Position.z * 2.0 - gl_Position.w);\n}}\n"
        )
    } else {
        format!("void main() {{\n    {fs}();\n}}\n")
    };
    Ok(format!(
        "{version}\n#define {stage} 1\n#define ATERM_VS_{vs} 1\n#define ATERM_FS_{fs} 1\n#line 2\n{body}\n{main}"
    ))
}

/// A row as GL draws it.
pub(crate) struct GlPipeline {
    pub(crate) program: GLuint,
    /// The row's own vertex array.
    pub(crate) vao: GLuint,
    pub(crate) blend: Option<[GLenum; 6]>,
    pub(crate) mask: [bool; 4],
    pub(crate) mode: GLenum,
    /// The instance layout, `None` for a `gl_VertexID`-only pass.
    pub(crate) layout: Option<VertexLayoutSpec>,
    /// The table label, for messages.
    pub(crate) label: &'static str,
    dev: GlDevice,
}

impl std::fmt::Debug for GlPipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlPipeline")
            .field("label", &self.label)
            .field("program", &self.program)
            .finish_non_exhaustive()
    }
}

impl Drop for GlPipeline {
    fn drop(&mut self) {
        if let Ok(cur) = self.dev.enter() {
            let gl = cur.gl();
            // SAFETY: names made on this context.
            unsafe {
                (gl.delete_vertex_arrays)(1, &raw const self.vao);
                (gl.delete_program)(self.program);
            }
        }
    }
}

/// Compile one stage.
fn compile(cur: &Current<'_>, kind: GLenum, src: &str, what: &str) -> Result<GLuint, String> {
    let gl = cur.gl();
    let len = i32::try_from(src.len()).map_err(|_| "gl: a shader over 2 GiB".to_owned())?;
    let ptr = src.as_ptr().cast::<ffi::GLchar>();
    // SAFETY: the context is current; one source string with its length.
    let shader = unsafe {
        let s = (gl.create_shader)(kind);
        (gl.shader_source)(s, 1, &raw const ptr, &raw const len);
        (gl.compile_shader)(s);
        s
    };
    let mut ok = 0;
    // SAFETY: a live shader name and out-pointer.
    unsafe { (gl.get_shaderiv)(shader, ffi::GL_COMPILE_STATUS, &raw mut ok) };
    if ok == 0 {
        let log = shader_log(cur, shader, true);
        // SAFETY: the shader made above.
        unsafe { (gl.delete_shader)(shader) };
        return Err(format!("gl: {what} does not compile:\n{log}"));
    }
    Ok(shader)
}

/// A shader's (`shader`) or program's info log.
fn shader_log(cur: &Current<'_>, name: GLuint, shader: bool) -> String {
    let gl = cur.gl();
    let mut len = 0;
    // SAFETY: a live name and out-pointer.
    unsafe {
        if shader {
            (gl.get_shaderiv)(name, ffi::GL_INFO_LOG_LENGTH, &raw mut len);
        } else {
            (gl.get_programiv)(name, ffi::GL_INFO_LOG_LENGTH, &raw mut len);
        }
    }
    let mut buf = vec![0u8; usize::try_from(len).unwrap_or(0).max(1)];
    let mut written = 0;
    // SAFETY: `buf` holds `len` bytes.
    unsafe {
        if shader {
            (gl.get_shader_info_log)(name, len, &raw mut written, buf.as_mut_ptr().cast());
        } else {
            (gl.get_program_info_log)(name, len, &raw mut written, buf.as_mut_ptr().cast());
        }
    }
    buf.truncate(usize::try_from(written).unwrap_or(0));
    String::from_utf8_lossy(&buf).into_owned()
}

impl GlDevice {
    /// Build the program and state for one table row.
    pub(crate) fn pipeline(&self, spec: &PipelineSpec) -> Result<GlPipeline, String> {
        let src = shaders::source(spec.library);
        let vs_src = compose(src, true, spec.vs, spec.fs)?;
        let fs_src = compose(src, false, spec.vs, spec.fs)?;
        let cur = self.enter()?;
        let gl = cur.gl();
        let vs = compile(&cur, ffi::GL_VERTEX_SHADER, &vs_src, spec.vs)?;
        let fs = match compile(&cur, ffi::GL_FRAGMENT_SHADER, &fs_src, spec.fs) {
            Ok(fs) => fs,
            Err(e) => {
                // SAFETY: the shader made above.
                unsafe { (gl.delete_shader)(vs) };
                return Err(e);
            }
        };
        // SAFETY: the context is current; two compiled shaders.
        let program = unsafe {
            let p = (gl.create_program)();
            (gl.attach_shader)(p, vs);
            (gl.attach_shader)(p, fs);
            (gl.link_program)(p);
            (gl.delete_shader)(vs);
            (gl.delete_shader)(fs);
            p
        };
        let mut ok = 0;
        // SAFETY: a live program and out-pointer.
        unsafe { (gl.get_programiv)(program, ffi::GL_LINK_STATUS, &raw mut ok) };
        if ok == 0 {
            let log = shader_log(&cur, program, false);
            // SAFETY: the program made above.
            unsafe { (gl.delete_program)(program) };
            return Err(format!(
                "gl: {} ({} + {}) does not link:\n{log}",
                spec.label, spec.vs, spec.fs
            ));
        }
        let b = spec.binds;
        // SAFETY: the context is current; the names are NUL-terminated (`CString`); the program is
        // linked and bound for the uniform writes.
        unsafe {
            (gl.use_program)(program);
            for n in b
                .vertex_uniform
                .into_iter()
                .chain(b.fragment_buffers.iter().copied())
            {
                let name = std::ffi::CString::new(format!("aterm_b{n}")).expect("no NUL");
                let idx = (gl.get_uniform_block_index)(program, name.as_ptr());
                if idx != ffi::GL_INVALID_INDEX {
                    (gl.uniform_block_binding)(program, idx, n);
                }
            }
            for &n in b.fragment_textures.iter().chain(b.fragment_palette.iter()) {
                let name = std::ffi::CString::new(format!("aterm_t{n}")).expect("no NUL");
                let loc = (gl.get_uniform_location)(program, name.as_ptr());
                if loc >= 0 {
                    (gl.uniform1i)(loc, n as i32);
                }
            }
            (gl.use_program)(0);
        }
        let mut vao = 0;
        // SAFETY: the context is current.
        unsafe { (gl.gen_vertex_arrays)(1, &raw mut vao) };
        let pipe = GlPipeline {
            program,
            vao,
            blend: gl_blend(spec.blend),
            mask: match spec.write_mask {
                WriteMask::Color => [true, true, true, false],
                WriteMask::All => [true; 4],
            },
            mode: gl_mode(spec.topology),
            layout: spec.vertex.spec(),
            label: spec.label,
            dev: self.clone(),
        };
        cur.check(spec.label)?;
        Ok(pipe)
    }
}

/// Specify `layout`'s attributes over the buffer bound to `GL_ARRAY_BUFFER`.
/// # Safety
/// The context is current, a vertex array and the stream buffer are bound.
pub(crate) unsafe fn specify_instance_layout(cur: &Current<'_>, layout: &VertexLayoutSpec) {
    let gl = cur.gl();
    let stride = layout.stride as i32;
    for a in layout.attrs {
        let offset = a.offset as usize as *const std::ffi::c_void;
        // SAFETY: the caller's contract; offsets and strides come from the table, whose strides
        // const asserts pin to the Rust instance structs.
        unsafe {
            (gl.enable_vertex_attrib_array)(a.location);
            match a.format {
                AttrFormat::Uint16x4 => (gl.vertex_attrib_i_pointer)(
                    a.location,
                    4,
                    ffi::GL_UNSIGNED_SHORT,
                    stride,
                    offset,
                ),
                AttrFormat::Unorm8x4 => (gl.vertex_attrib_pointer)(
                    a.location,
                    4,
                    ffi::GL_UNSIGNED_BYTE,
                    ffi::GL_TRUE,
                    stride,
                    offset,
                ),
                AttrFormat::Uint8x4 => (gl.vertex_attrib_i_pointer)(
                    a.location,
                    4,
                    ffi::GL_UNSIGNED_BYTE,
                    stride,
                    offset,
                ),
                AttrFormat::Float32x4 => (gl.vertex_attrib_pointer)(
                    a.location,
                    4,
                    ffi::GL_FLOAT,
                    ffi::GL_FALSE,
                    stride,
                    offset,
                ),
                AttrFormat::Uint32 => (gl.vertex_attrib_i_pointer)(
                    a.location,
                    1,
                    ffi::GL_UNSIGNED_INT,
                    stride,
                    offset,
                ),
                AttrFormat::Sint16x2 => {
                    (gl.vertex_attrib_i_pointer)(a.location, 2, ffi::GL_SHORT, stride, offset)
                }
                AttrFormat::Uint16x2 => (gl.vertex_attrib_i_pointer)(
                    a.location,
                    2,
                    ffi::GL_UNSIGNED_SHORT,
                    stride,
                    offset,
                ),
            }
            (gl.vertex_attrib_divisor)(a.location, 1);
        }
    }
}

#[cfg(test)]
mod tests {
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
        let err =
            compile(&cur, ffi::GL_FRAGMENT_SHADER, &src, "fs_bg").expect_err("must not compile");
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
}
