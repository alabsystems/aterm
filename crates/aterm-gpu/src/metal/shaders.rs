// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The MSL twins of aterm's WGSL shaders. The entry-point roster is derived
//! from [`crate::pipeline_table`] (see `libraries`).
//!
//! The WGSL lives in `const &str` literals in `renderer.rs`; the MSL lives in
//! `crates/aterm-gpu/shaders/*.metal`, pulled in with `include_str!`, so the
//! compile test, editors and `xcrun metal` can read it.
//!
//! # Colour spaces
//!
//! `renderer.rs::pick_surface_format` chooses a non-sRGB surface format, never
//! an `*_sRGB` one:
//!
//! * Base OVER/REPLACE passes render into an sRGB view of the offscreen; the
//!   fragments emit linear light (`s2l`) and the view re-encodes on store, so
//!   blending composites in linear light like the CPU `blend`.
//! * Additive passes (`fs_glow`, `fs_rain_glow`, `fs_fire_add`, `fs_deco_add`)
//!   bind the Unorm view of the same texture and emit raw values, so One/One
//!   is byte-exact with the CPU `add_sat`.
//! * The blit writes already-encoded bytes; an sRGB swapchain would encode
//!   twice.
//!
//! Metal expresses the pairing with `newTextureViewWithPixelFormat:` on
//! Unorm/sRGB view-compatible formats. The formats used (`Bgra8Unorm`,
//! `Rgba8Unorm`, `R8Unorm`, `Rgba16Float`) are declared in
//! [`super::ffi::PixelFormat`] and exercised by [`super::tests`].

//! # WGSL to MSL mapping
//!
//! Structural differences: `@location` params become `[[attribute(n)]]` +
//! `[[stage_in]]` structs; `@interpolate(flat)` becomes `[[flat]]`;
//! `bitcast<i32>` becomes `as_type<int>`; `textureLoad` becomes `read`;
//! `textureSampleLevel(.., 0.0)` becomes `sample(.., level(0.0))`; uniforms,
//! textures and samplers become function arguments. In `cell.metal` the rain
//! weight and fire shading tails are shared `static inline` helpers so the
//! parity kernel calls the same code as the fragments. Integer math is
//! otherwise op for op. `parity_kernel.metal` is verification-only.
//!
//! Every construct maps exactly:
//!
//! * Arithmetic right shift of negative integers (`fire_core`'s
//!   `(body0 - 128) * edge >> 8`): sign-replicating in both WGSL and MSL for
//!   signed types; the fire differential in `super::tests` confirms it.
//! * Wrapping `u32` arithmetic (`fire_hash`): defined in both.
//! * Flat integer interpolation: MSL requires `[[flat]]`; every flat value is
//!   per instance, so the provoking vertex does not matter.
//! * Uniform layout: std140 in WGSL, natural C layout in MSL. Every member here
//!   is at most 16 bytes and naturally aligned, so they agree; recheck
//!   `BlitUniform` (112 bytes) and `ShimmerU`'s `float4[16]` when adding
//!   fields.

use crate::pipeline_table::ShaderLibrary;

/// The cell shader: backgrounds, aurora, rain halo, fire field, glyphs,
/// decorations and sprites. WGSL twin: `renderer.rs::SHADER`.
pub(crate) const CELL: &str = include_str!("../../shaders/cell.metal");
/// The offscreen-to-swapchain blit, with remainder bands, bell invert, drop
/// overlay and HDR encode. WGSL twin: `renderer.rs::BLIT_SHADER`.
pub(crate) const BLIT: &str = include_str!("../../shaders/blit.metal");
/// The swapchain-side aurora crown, HDR and SDR arms. WGSL twin:
/// `renderer.rs::HDR_GLOW_SHADER`.
pub(crate) const HDR_GLOW: &str = include_str!("../../shaders/hdr_glow.metal");
/// The tray/overlay texture blit. WGSL twin: `renderer.rs::TRAY_SHADER`.
pub(crate) const TRAY: &str = include_str!("../../shaders/tray.metal");
/// The 5x5 gaussian bloom tap. WGSL twin: `renderer.rs::BLOOM_SHADER`.
pub(crate) const BLOOM: &str = include_str!("../../shaders/bloom.metal");
/// The EMBERFORGE heat-haze displacement. WGSL twin:
/// `renderer.rs::SHIMMER_SHADER`.
pub(crate) const SHIMMER: &str = include_str!("../../shaders/shimmer.metal");

/// The MSL source for one [`ShaderLibrary`]. Which entry points it must export
/// comes from `crate::pipeline_table::entry_points`.
pub(crate) const fn source(library: ShaderLibrary) -> &'static str {
    match library {
        ShaderLibrary::Cell => CELL,
        ShaderLibrary::Blit => BLIT,
        ShaderLibrary::HdrGlow => HDR_GLOW,
        ShaderLibrary::Tray => TRAY,
        ShaderLibrary::Bloom => BLOOM,
        ShaderLibrary::Shimmer => SHIMMER,
    }
}

/// Every `(library, source, entry points)` triple, derived from the pipeline
/// table, so one test proves every library compiles and every entry point the
/// renderer requests resolves. Derived rather than listed so the roster cannot
/// agree with a rename the renderer does not know about.
#[cfg(test)]
pub(crate) fn libraries() -> Vec<(ShaderLibrary, &'static str, Vec<&'static str>)> {
    ShaderLibrary::ALL
        .into_iter()
        .map(|lib| (lib, source(lib), crate::pipeline_table::entry_points(lib)))
        .collect()
}

/// Verification-only state probes (fullscreen triangle, constant-colour and
/// sampling fragments) that let [`super::tests`] prove the write mask,
/// scissor, viewport and sampler filters in [`super::ffi`] are honoured.
#[cfg(test)]
pub(crate) const STATE_PROBE: &str = include_str!("../../shaders/state_probe.metal");

/// Verification-only compute kernels, concatenated onto [`CELL`] by the parity
/// test so the math under test is the shipped math.
#[cfg(test)]
pub(crate) const PARITY_KERNEL: &str = include_str!("../../shaders/parity_kernel.metal");

/// [`CELL`] plus [`PARITY_KERNEL`], the source the parity test compiles.
#[cfg(test)]
pub(crate) fn cell_with_parity_kernels() -> String {
    format!("{CELL}\n{PARITY_KERNEL}")
}

/// Every `vertex`/`fragment` function `src` defines, in source order. A narrow
/// scan: every entry point is declared at column zero as `vertex <ret> <name>(`
/// or `fragment <ret> <name>(`.
#[cfg(test)]
pub(crate) fn defined_entry_points(src: &str) -> Vec<&str> {
    src.lines()
        .filter_map(|line| {
            let rest = line
                .strip_prefix("vertex ")
                .or_else(|| line.strip_prefix("fragment "))?;
            // `<return type> <name>(` — the name is what precedes the paren.
            let (head, _) = rest.split_once('(')?;
            head.rsplit_once(char::is_whitespace).map(|(_, n)| n)
        })
        .collect()
}

/// What one MSL entry point declares in its parameter list, with the 1-based
/// line of each declaration; the scanned side of
/// [`crate::pipeline_table::BindSpec`].
#[derive(Debug)]
#[cfg(test)]
pub(crate) struct DeclaredBindings {
    /// The 1-based line of the `vertex`/`fragment` declaration itself.
    pub(crate) line: usize,
    /// Whether the parameter list takes a `[[stage_in]]` struct (the instance
    /// stream, bound through the vertex descriptor at `INSTANCE_STREAM_SLOT`).
    pub(crate) stage_in: bool,
    /// Every `[[buffer(n)]]` as `(n, line)`, in slot order.
    pub(crate) buffers: Vec<(u32, usize)>,
    /// Every `[[texture(n)]]` as `(n, line)`, in slot order.
    pub(crate) textures: Vec<(u32, usize)>,
    /// Every `[[sampler(n)]]` as `(n, line)`, in slot order.
    pub(crate) samplers: Vec<(u32, usize)>,
}

/// Read the binding attributes off `entry`'s parameter list in `src`, or `None`
/// if not defined. Same declaration shape as [`defined_entry_points`], then a
/// paren-balanced walk to the end of the parameters.
#[cfg(test)]
pub(crate) fn entry_point_bindings(src: &str, entry: &str) -> Option<DeclaredBindings> {
    // Byte offset of the declaration line's start.
    let mut offset = 0usize;
    let mut decl_off = None;
    for line in src.lines() {
        let is_it = line
            .strip_prefix("vertex ")
            .or_else(|| line.strip_prefix("fragment "))
            .and_then(|rest| rest.split_once('('))
            .and_then(|(head, _)| head.rsplit_once(char::is_whitespace))
            .is_some_and(|(_, n)| n == entry);
        if is_it {
            decl_off = Some(offset);
            break;
        }
        offset += line.len() + 1;
    }
    let decl_off = decl_off?;
    let open = decl_off + src[decl_off..].find('(')?;
    let mut depth = 0usize;
    let mut close = None;
    for (j, c) in src[open..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(open + j);
                    break;
                }
            }
            _ => {}
        }
    }
    let params = &src[open..close?];
    let line_of = |abs: usize| src[..abs].matches('\n').count() + 1;
    let scan = |needle: &str| -> Vec<(u32, usize)> {
        let mut out = Vec::new();
        let mut at = 0usize;
        while let Some(p) = params[at..].find(needle) {
            let num_at = at + p + needle.len();
            let digits: String = params[num_at..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            if let Ok(n) = digits.parse::<u32>() {
                out.push((n, line_of(open + at + p)));
            }
            at = num_at;
        }
        out.sort_unstable();
        out
    };
    Some(DeclaredBindings {
        line: line_of(decl_off),
        stage_in: params.contains("[[stage_in]]"),
        buffers: scan("[[buffer("),
        textures: scan("[[texture("),
        samplers: scan("[[sampler("),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// For every library, the entry points the MSL defines equal exactly the set
    /// the pipeline table requests, in both directions: nothing requested is
    /// missing, and nothing defined is unrequested. A text scan; no GPU needed.
    #[test]
    fn the_msl_defines_exactly_the_entry_points_the_table_asks_for() {
        for (lib, src, mut want) in libraries() {
            let mut have = defined_entry_points(src);
            assert!(
                !have.is_empty(),
                "{}.metal: the scan found no entry points at all — the file's \
                 declaration style changed and this guard went blind",
                lib.name()
            );
            want.sort_unstable();
            have.sort_unstable();
            assert_eq!(
                have,
                want,
                "{}.metal defines {have:?} but the pipeline table asks for {want:?}",
                lib.name()
            );
        }
    }

    /// For every table row, the `[[buffer(n)]]` / `[[texture(n)]]` /
    /// `[[sampler(n)]]` slots its MSL entry points declare equal the row's
    /// [`BindSpec`] per stage. An untabled declaration fails naming its
    /// file:line; a tabled but undeclared slot names the entry point's.
    ///
    /// Also: vertex stages declare no textures or samplers, and `[[stage_in]]`
    /// is present iff `vertex != VertexLayout::None` (a vertex uniform at
    /// `INSTANCE_STREAM_SLOT` is refused). Text scan only.
    #[test]
    fn the_msl_bindings_are_exactly_the_tables_bind_column() {
        use crate::pipeline_table::{ALL_PIPELINES, VertexLayout};

        // Direction-aware set diff: `declared` from the scan, `tabled` from
        // the row. Panics name the file:line of whichever side is wrong.
        fn check(
            file: &str,
            entry: &str,
            entry_line: usize,
            kind: &str,
            declared: &[(u32, usize)],
            tabled: &[u32],
            row: &str,
        ) {
            for &(slot, line) in declared {
                assert!(
                    tabled.contains(&slot),
                    "{file}:{line}: `{entry}` declares [[{kind}({slot})]] but row \
                     `{row}`'s BindSpec does not table it — declared-but-untabled"
                );
            }
            for &slot in tabled {
                assert!(
                    declared.iter().any(|&(s, _)| s == slot),
                    "{file}:{entry_line}: row `{row}`'s BindSpec tables {kind} slot \
                     {slot} but `{entry}` declares no [[{kind}({slot})]] — \
                     tabled-but-undeclared"
                );
            }
        }

        let mut scanned_any = (0usize, 0usize, 0usize, 0usize); // stage_in, buf, tex, samp
        for p in ALL_PIPELINES {
            let spec = p.spec();
            let src = source(spec.library);
            let file = format!("{}.metal", spec.library.name());
            let b = spec.binds;

            let vs = entry_point_bindings(src, spec.vs).unwrap_or_else(|| {
                panic!("{file}: the scan found no `{}` — it went blind", spec.vs)
            });
            let vs_tabled: Vec<u32> = b.vertex_uniform.into_iter().collect();
            check(
                &file,
                spec.vs,
                vs.line,
                "buffer",
                &vs.buffers,
                &vs_tabled,
                p.name(),
            );
            assert!(
                vs.textures.is_empty() && vs.samplers.is_empty(),
                "{file}:{}: `{}` declares a vertex-stage texture/sampler, which \
                 no row tables — the table needs a new column before this can pass",
                vs.line,
                spec.vs
            );
            assert_eq!(
                vs.stage_in,
                spec.vertex != VertexLayout::None,
                "{file}:{}: `{}` [[stage_in]] disagrees with row `{}`'s vertex \
                 layout ({:?})",
                vs.line,
                spec.vs,
                p.name(),
                spec.vertex
            );
            assert_ne!(
                b.vertex_uniform,
                Some(crate::metal::ffi::INSTANCE_STREAM_SLOT as u32),
                "{}: the vertex uniform may not sit on INSTANCE_STREAM_SLOT",
                p.name()
            );

            let fs = entry_point_bindings(src, spec.fs).unwrap_or_else(|| {
                panic!("{file}: the scan found no `{}` — it went blind", spec.fs)
            });
            check(
                &file,
                spec.fs,
                fs.line,
                "buffer",
                &fs.buffers,
                b.fragment_buffers,
                p.name(),
            );
            check(
                &file,
                spec.fs,
                fs.line,
                "texture",
                &fs.textures,
                b.fragment_textures,
                p.name(),
            );
            check(
                &file,
                spec.fs,
                fs.line,
                "sampler",
                &fs.samplers,
                b.fragment_samplers,
                p.name(),
            );

            scanned_any.0 += usize::from(vs.stage_in);
            scanned_any.1 += vs.buffers.len() + fs.buffers.len();
            scanned_any.2 += fs.textures.len();
            scanned_any.3 += fs.samplers.len();
        }
        // Positive control: the sweep must have read stage_ins, buffers,
        // textures and samplers somewhere.
        assert!(
            scanned_any.0 > 0 && scanned_any.1 > 0 && scanned_any.2 > 0 && scanned_any.3 > 0,
            "the binding scan found {scanned_any:?} (stage_in/buffers/textures/\
             samplers) across all rows — it went blind and this guard proves nothing"
        );
    }

    /// The binding scan compares sets per kind, so it cannot see a same-kind
    /// slot swap. No row has two bindings of one kind today; this fails the
    /// moment one does, saying what the scan must learn first.
    #[test]
    fn no_row_tables_two_bindings_of_one_kind_until_the_scan_learns_pairing() {
        for p in crate::pipeline_table::ALL_PIPELINES {
            let b = p.spec().binds;
            for (kind, slots) in [
                ("fragment_textures", b.fragment_textures),
                ("fragment_samplers", b.fragment_samplers),
                ("fragment_buffers", b.fragment_buffers),
            ] {
                assert!(
                    slots.len() <= 1,
                    "row `{}` tables {} {kind} slots; the MSL binding scan \
                     compares sets per kind and cannot see a same-kind slot \
                     swap. Teach it slot->meaning pairing (order or names), arm it with a \
                     transposed pair, then delete this pin.",
                    p.name(),
                    slots.len()
                );
            }
        }
    }
}
