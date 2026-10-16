// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The shader-twin scan: every hand-written twin of aterm's WGSL under
//! `crates/aterm-gpu/shaders/` defines exactly the entry points the pipeline
//! table asks for and declares exactly the bindings its rows'
//! [`BindSpec`]s table. A text scan, with no GPU and no shader compiler, so it
//! runs on every host whatever backend the build compiles.
//!
//! A twin is `shaders/<library>.<ext>`, one file per [`ShaderLibrary`]; the
//! extension names the dialect. A dialect with any twin must have one for
//! every library, since its backend runs every pipeline row.
//!
//! | ext | dialect | backend | an entry point is | bindings, read from |
//! |---|---|---|---|---|
//! | `.metal` | MSL | Metal (macOS) | a column-zero `vertex` / `fragment` function | its own parameter list: `[[buffer(n)]]`, `[[texture(n)]]`, `[[sampler(n)]]`, `[[stage_in]]` |
//! | `.glsl` | GLSL ES 3.00 | GL (docs/AUDIT-third-party-2026-10-15.md GW.4) | a column-zero function named `vs_*` / `fs_*` | module scope: uniform block `aterm_b<n>`, sampler uniform `aterm_t<n>` |
//! | `.hlsl` | HLSL SM 5.0 | D3D11 (GW.5) | a column-zero function named `vs_*` / `fs_*` | module scope: `register(b<n>)`, `register(t<n>)`, `register(s<n>)` |
//!
//! GLSL ES 3.00 (the WebGL2 dialect) has no binding layout qualifier, so the
//! GL arm binds by name: uniform block `aterm_b<n>` to binding point `n`, and
//! sampler uniform `aterm_t<n>` to texture unit `n` with its sampler object at
//! the same unit (GL pairs them, so a GLSL row's texture and sampler slots are
//! one slot). HLSL resources and GLSL uniforms live at module scope, so for
//! those two dialects the check is the per-library union of the rows'
//! BindSpec halves (as the WGSL map check in `renderer.rs` is); MSL declares
//! per entry point and is checked per row and stage. In GLSL and HLSL a helper
//! function must not take a `vs_` or `fs_` name: the scan would read it as an
//! entry point the table never asked for.
//!
//! WGSL, wgpu's dialect, lives in `renderer.rs` constants and has its own two
//! scans there; it leaves with wgpu (GW.6).

use std::collections::BTreeSet;

use crate::pipeline_table::{ALL_PIPELINES, BindSpec, ShaderLibrary, VertexLayout, entry_points};

/// A twin dialect, by file extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Dialect {
    Msl,
    Glsl,
    Hlsl,
}

impl Dialect {
    pub(crate) const ALL: [Self; 3] = [Self::Msl, Self::Glsl, Self::Hlsl];

    /// The twin file extension.
    pub(crate) const fn ext(self) -> &'static str {
        match self {
            Self::Msl => "metal",
            Self::Glsl => "glsl",
            Self::Hlsl => "hlsl",
        }
    }

    /// The entry point a source line declares, if it declares one: the name
    /// before the line's first `(`.
    fn entry_name(self, line: &str) -> Option<&str> {
        let decl = match self {
            Self::Msl => line
                .strip_prefix("vertex ")
                .or_else(|| line.strip_prefix("fragment "))?,
            Self::Glsl | Self::Hlsl => {
                if line.starts_with(|c: char| c.is_whitespace() || c == '#' || c == '/') {
                    return None;
                }
                line
            }
        };
        let (head, _) = decl.split_once('(')?;
        let (_, name) = head.trim_end().rsplit_once(char::is_whitespace)?;
        match self {
            Self::Msl => Some(name),
            Self::Glsl | Self::Hlsl => {
                (name.starts_with("vs_") || name.starts_with("fs_")).then_some(name)
            }
        }
    }

    /// Every entry point `src` defines, in source order.
    pub(crate) fn entry_points(self, src: &str) -> Vec<&str> {
        src.lines().filter_map(|l| self.entry_name(l)).collect()
    }
}

/// One twin on disk.
#[derive(Debug)]
pub(crate) struct Twin {
    pub(crate) dialect: Dialect,
    /// `None` when the file stem names no library (the roster check fails it).
    pub(crate) library: Option<ShaderLibrary>,
    /// `shaders/<stem>.<ext>`, for messages.
    pub(crate) file: String,
    pub(crate) src: String,
}

/// Every twin under `crates/aterm-gpu/shaders/`, by dialect extension; other
/// files there are not twins.
pub(crate) fn twins_on_disk() -> Vec<Twin> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("shaders");
    let mut out = Vec::new();
    let entries = std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    for entry in entries {
        let path = entry
            .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
            .path();
        let (Some(stem), Some(ext)) = (
            path.file_stem().and_then(|s| s.to_str()),
            path.extension().and_then(|s| s.to_str()),
        ) else {
            continue;
        };
        let Some(dialect) = Dialect::ALL.into_iter().find(|d| d.ext() == ext) else {
            continue;
        };
        let src =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        out.push(Twin {
            dialect,
            library: ShaderLibrary::ALL.into_iter().find(|l| l.name() == stem),
            file: format!("shaders/{stem}.{ext}"),
            src,
        });
    }
    out.sort_by(|a, b| a.file.cmp(&b.file));
    out
}

/// The verification-only MSL files beside the twins (`metal::shaders`'s state
/// probe and parity kernels): `.metal` files that no pipeline row draws with.
const MSL_TEST_ONLY: [&str; 2] = ["state_probe", "parity_kernel"];

/// `have` (a twin's scan) equals the table's entry points for `library`, in
/// both directions: nothing requested is missing, nothing defined is
/// unrequested.
pub(crate) fn check_entry_points(
    library: ShaderLibrary,
    file: &str,
    have: &[&str],
) -> Result<(), String> {
    if have.is_empty() {
        return Err(format!(
            "{file}: the scan found no entry points at all — the file's declaration style \
             changed and this guard went blind"
        ));
    }
    let mut want = entry_points(library);
    let mut have = have.to_vec();
    want.sort_unstable();
    have.sort_unstable();
    if have == want {
        Ok(())
    } else {
        Err(format!(
            "{file} defines {have:?} but the pipeline table asks for {want:?}"
        ))
    }
}

/// What one MSL entry point declares in its parameter list, with the 1-based
/// line of each declaration.
#[derive(Debug)]
pub(crate) struct MslEntryBindings {
    /// The 1-based line of the `vertex`/`fragment` declaration itself.
    pub(crate) line: usize,
    /// Whether the parameter list takes a `[[stage_in]]` struct (the instance
    /// stream, bound through the vertex descriptor).
    pub(crate) stage_in: bool,
    /// Every `[[buffer(n)]]` as `(n, line)`, in slot order.
    pub(crate) buffers: Vec<(u32, usize)>,
    /// Every `[[texture(n)]]` as `(n, line)`, in slot order.
    pub(crate) textures: Vec<(u32, usize)>,
    /// Every `[[sampler(n)]]` as `(n, line)`, in slot order.
    pub(crate) samplers: Vec<(u32, usize)>,
}

/// Read the binding attributes off MSL `entry`'s parameter list in `src`, or
/// `None` if not defined: the declaration line, then a paren-balanced walk to
/// the end of the parameters.
pub(crate) fn msl_entry_bindings(src: &str, entry: &str) -> Option<MslEntryBindings> {
    // Byte offset of the declaration line's start.
    let mut offset = 0usize;
    let mut decl_off = None;
    for line in src.lines() {
        if Dialect::Msl.entry_name(line) == Some(entry) {
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
            if let Some(n) = leading_number(&params[num_at..]) {
                out.push((n, line_of(open + at + p)));
            }
            at = num_at;
        }
        out.sort_unstable();
        out
    };
    Some(MslEntryBindings {
        line: line_of(decl_off),
        stage_in: params.contains("[[stage_in]]"),
        buffers: scan("[[buffer("),
        textures: scan("[[texture("),
        samplers: scan("[[sampler("),
    })
}

/// The decimal number `s` starts with.
fn leading_number(s: &str) -> Option<u32> {
    let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Direction-aware set diff of one MSL entry point's declared slots of one
/// kind against the row's tabled slots, naming the file:line of whichever side
/// is wrong.
fn check_msl_kind(
    file: &str,
    entry: &str,
    entry_line: usize,
    kind: &str,
    declared: &[(u32, usize)],
    tabled: &[u32],
    row: &str,
) -> Result<(), String> {
    for &(slot, line) in declared {
        if !tabled.contains(&slot) {
            return Err(format!(
                "{file}:{line}: `{entry}` declares [[{kind}({slot})]] but row `{row}`'s \
                 BindSpec does not table it — declared-but-untabled"
            ));
        }
    }
    for &slot in tabled {
        if !declared.iter().any(|&(s, _)| s == slot) {
            return Err(format!(
                "{file}:{entry_line}: row `{row}`'s BindSpec tables {kind} slot {slot} but \
                 `{entry}` declares no [[{kind}({slot})]] — tabled-but-undeclared"
            ));
        }
    }
    Ok(())
}

/// For every row drawn from `library`, the `[[buffer(n)]]` / `[[texture(n)]]`
/// / `[[sampler(n)]]` slots its MSL entry points declare equal the row's
/// BindSpec per stage; vertex stages declare no texture or sampler; and
/// `[[stage_in]]` is present iff the row has a vertex layout. Returns the
/// count of `(stage_in, buffers, textures, samplers)` read, for the caller's
/// blindness check.
pub(crate) fn check_msl_bindings(
    library: ShaderLibrary,
    file: &str,
    src: &str,
) -> Result<(usize, usize, usize, usize), String> {
    let mut seen = (0usize, 0usize, 0usize, 0usize);
    for p in ALL_PIPELINES {
        let spec = p.spec();
        if spec.library != library {
            continue;
        }
        let b = spec.binds;
        let vs = msl_entry_bindings(src, spec.vs)
            .ok_or_else(|| format!("{file}: the scan found no `{}` — it went blind", spec.vs))?;
        let vs_tabled: Vec<u32> = b.vertex_uniform.into_iter().collect();
        check_msl_kind(
            file,
            spec.vs,
            vs.line,
            "buffer",
            &vs.buffers,
            &vs_tabled,
            p.name(),
        )?;
        if !(vs.textures.is_empty() && vs.samplers.is_empty()) {
            return Err(format!(
                "{file}:{}: `{}` declares a vertex-stage texture/sampler, which no row \
                 tables — the table needs a new column before this can pass",
                vs.line, spec.vs
            ));
        }
        if vs.stage_in != (spec.vertex != VertexLayout::None) {
            return Err(format!(
                "{file}:{}: `{}` [[stage_in]] disagrees with row `{}`'s vertex layout ({:?})",
                vs.line,
                spec.vs,
                p.name(),
                spec.vertex
            ));
        }
        let fs = msl_entry_bindings(src, spec.fs)
            .ok_or_else(|| format!("{file}: the scan found no `{}` — it went blind", spec.fs))?;
        for (kind, declared, tabled) in [
            ("buffer", &fs.buffers, b.fragment_buffers),
            ("texture", &fs.textures, b.fragment_textures),
            ("sampler", &fs.samplers, b.fragment_samplers),
        ] {
            check_msl_kind(file, spec.fs, fs.line, kind, declared, tabled, p.name())?;
        }
        seen.0 += usize::from(vs.stage_in);
        seen.1 += vs.buffers.len() + fs.buffers.len();
        seen.2 += fs.textures.len();
        seen.3 += fs.samplers.len();
    }
    Ok(seen)
}

/// The slots a GLSL or HLSL twin declares at module scope, each with the
/// 1-based line of its first declaration.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ModuleBindings {
    pub(crate) buffers: Vec<(u32, usize)>,
    pub(crate) textures: Vec<(u32, usize)>,
    pub(crate) samplers: Vec<(u32, usize)>,
}

impl ModuleBindings {
    fn add(list: &mut Vec<(u32, usize)>, slot: u32, line: usize) {
        if !list.iter().any(|&(s, _)| s == slot) {
            list.push((slot, line));
            list.sort_unstable();
        }
    }
}

/// Every `needle<n>` in `line`, as `n`.
fn numbered(line: &str, needle: &str) -> Vec<u32> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while let Some(p) = line[at..].find(needle) {
        let num_at = at + p + needle.len();
        if let Some(n) = leading_number(&line[num_at..]) {
            out.push(n);
        }
        at = num_at;
    }
    out
}

/// Read a GLSL or HLSL twin's module-scope bindings (see the module header for
/// each dialect's spelling). `None` for MSL, which binds per entry point.
pub(crate) fn module_bindings(dialect: Dialect, src: &str) -> Option<ModuleBindings> {
    let mut out = ModuleBindings::default();
    for (i, line) in src.lines().enumerate() {
        let n = i + 1;
        let code = line.split("//").next().unwrap_or("");
        match dialect {
            Dialect::Msl => return None,
            Dialect::Glsl => {
                if !code.contains("uniform") {
                    continue;
                }
                for slot in numbered(code, "aterm_b") {
                    ModuleBindings::add(&mut out.buffers, slot, n);
                }
                // A GL texture unit carries its texture and its sampler.
                for slot in numbered(code, "aterm_t") {
                    ModuleBindings::add(&mut out.textures, slot, n);
                    ModuleBindings::add(&mut out.samplers, slot, n);
                }
            }
            Dialect::Hlsl => {
                for (prefix, list) in [
                    ("register(b", &mut out.buffers),
                    ("register(t", &mut out.textures),
                    ("register(s", &mut out.samplers),
                ] {
                    for slot in numbered(code, prefix) {
                        ModuleBindings::add(list, slot, n);
                    }
                }
            }
        }
    }
    Some(out)
}

/// The per-library union of the rows' BindSpec halves: buffers (vertex
/// uniform and fragment buffers), textures, samplers.
fn library_union(library: ShaderLibrary) -> [BTreeSet<u32>; 3] {
    let mut want: [BTreeSet<u32>; 3] = Default::default();
    for p in ALL_PIPELINES {
        let spec = p.spec();
        if spec.library != library {
            continue;
        }
        let b: BindSpec = spec.binds;
        want[0].extend(b.vertex_uniform);
        want[0].extend(b.fragment_buffers);
        want[1].extend(b.fragment_textures);
        want[2].extend(b.fragment_samplers);
    }
    want
}

/// A GLSL or HLSL twin's module-scope slots equal the per-library union of
/// its rows' BindSpec halves, per kind and in both directions.
pub(crate) fn check_module_bindings(
    library: ShaderLibrary,
    file: &str,
    have: &ModuleBindings,
) -> Result<(), String> {
    let want = library_union(library);
    for ((kind, declared), tabled) in [
        ("buffer", &have.buffers),
        ("texture", &have.textures),
        ("sampler", &have.samplers),
    ]
    .into_iter()
    .zip(&want)
    {
        for &(slot, line) in declared {
            if !tabled.contains(&slot) {
                return Err(format!(
                    "{file}:{line}: declares {kind} slot {slot}, which no `{}` row's \
                     BindSpec tables — declared-but-untabled",
                    library.name()
                ));
            }
        }
        for &slot in tabled {
            if !declared.iter().any(|&(s, _)| s == slot) {
                return Err(format!(
                    "{file}: a `{}` row's BindSpec tables {kind} slot {slot}, which the twin \
                     never declares — tabled-but-undeclared",
                    library.name()
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The twins actually under `shaders/`, less the verification-only MSL.
    fn twins() -> Vec<Twin> {
        twins_on_disk()
            .into_iter()
            .filter(|t| {
                !(t.dialect == Dialect::Msl
                    && MSL_TEST_ONLY
                        .iter()
                        .any(|s| t.file == format!("shaders/{s}.metal")))
            })
            .collect()
    }

    /// Every twin is named for a library, and a dialect with any twin has one
    /// for every library. MSL is complete today, so the scan is never blind.
    #[test]
    fn every_dialect_with_a_twin_covers_every_library() {
        let twins = twins();
        for t in &twins {
            assert!(
                t.library.is_some(),
                "{}: the stem names no ShaderLibrary ({:?}) — a twin is `<library>.<ext>`",
                t.file,
                ShaderLibrary::ALL.map(ShaderLibrary::name)
            );
        }
        for dialect in Dialect::ALL {
            let have: Vec<ShaderLibrary> = twins
                .iter()
                .filter(|t| t.dialect == dialect)
                .filter_map(|t| t.library)
                .collect();
            if have.is_empty() {
                continue;
            }
            for lib in ShaderLibrary::ALL {
                assert!(
                    have.contains(&lib),
                    "{dialect:?} has twins {have:?} but none for `{}` — its backend draws \
                     every row, so a dialect is all or nothing",
                    lib.name()
                );
            }
        }
        assert_eq!(
            twins.iter().filter(|t| t.dialect == Dialect::Msl).count(),
            ShaderLibrary::ALL.len(),
            "the MSL twins (shipping on macOS) are one per library"
        );
    }

    /// Every twin, in every dialect, defines exactly the entry points the
    /// pipeline table asks for, no more, no fewer, no renames.
    #[test]
    fn every_twin_defines_exactly_the_entry_points_the_table_asks_for() {
        for t in twins() {
            let lib = t.library.expect("named for a library (the roster test)");
            let have = t.dialect.entry_points(&t.src);
            if let Err(e) = check_entry_points(lib, &t.file, &have) {
                panic!("{e}");
            }
        }
    }

    /// Every twin's bindings agree with the table's bind column: MSL per row
    /// and stage, GLSL and HLSL per library at module scope.
    #[test]
    fn every_twin_binds_exactly_the_tables_bind_column() {
        let mut seen = (0usize, 0usize, 0usize, 0usize);
        for t in twins() {
            let lib = t.library.expect("named for a library (the roster test)");
            let result = match module_bindings(t.dialect, &t.src) {
                None => check_msl_bindings(lib, &t.file, &t.src).map(|s| {
                    seen = (seen.0 + s.0, seen.1 + s.1, seen.2 + s.2, seen.3 + s.3);
                }),
                Some(have) => check_module_bindings(lib, &t.file, &have),
            };
            if let Err(e) = result {
                panic!("{e}");
            }
        }
        // The MSL sweep must have read stage_ins, buffers, textures and
        // samplers somewhere, or this guard proves nothing.
        assert!(
            seen.0 > 0 && seen.1 > 0 && seen.2 > 0 && seen.3 > 0,
            "the MSL binding scan found {seen:?} (stage_in/buffers/textures/samplers) across \
             all rows — it went blind"
        );
    }

    /// The binding scans compare sets per kind, so they cannot see a same-kind
    /// slot swap. No row has two bindings of one kind today; this fails the
    /// moment one does, saying what the scans must learn first.
    #[test]
    fn no_row_tables_two_bindings_of_one_kind_until_the_scan_learns_pairing() {
        for p in ALL_PIPELINES {
            let b = p.spec().binds;
            for (kind, slots) in [
                ("fragment_textures", b.fragment_textures),
                ("fragment_samplers", b.fragment_samplers),
                ("fragment_buffers", b.fragment_buffers),
            ] {
                assert!(
                    slots.len() <= 1,
                    "row `{}` tables {} {kind} slots; the twin binding scans compare sets \
                     per kind and cannot see a same-kind slot swap. Teach them slot->meaning \
                     pairing (order or names), arm them with a transposed pair, then delete \
                     this pin.",
                    p.name(),
                    slots.len()
                );
            }
        }
    }

    /// The MSL scan bites: the real `blit.metal` passes, and the same file with
    /// its fragment uniform moved off `[[buffer(2)]]`, or with an extra entry
    /// point, fails naming the cause.
    #[test]
    fn the_msl_scan_refuses_a_moved_slot_and_an_extra_entry() {
        let blit = twins()
            .into_iter()
            .find(|t| t.file == "shaders/blit.metal")
            .expect("blit.metal is a twin");
        check_msl_bindings(ShaderLibrary::Blit, &blit.file, &blit.src).expect("the real twin");
        assert!(blit.src.contains("[[buffer(2)]]"), "the fixture's premise");
        let moved = blit.src.replacen("[[buffer(2)]]", "[[buffer(3)]]", 1);
        let err = check_msl_bindings(ShaderLibrary::Blit, &blit.file, &moved)
            .expect_err("a moved fragment buffer must fail");
        assert!(err.contains("buffer(3)"), "{err}");
        let extra = format!(
            "{}\nfragment float4 fs_stray(float4 p [[position]]) {{ return p; }}\n",
            blit.src
        );
        let err = check_entry_points(
            ShaderLibrary::Blit,
            &blit.file,
            &Dialect::Msl.entry_points(&extra),
        )
        .expect_err("an unrequested entry point must fail");
        assert!(err.contains("fs_stray"), "{err}");
    }

    /// A GLSL ES twin of the blit library, in the convention the module header
    /// states: `vs_blit` + `fs_blit`, the pass uniform block at `aterm_b2`, the
    /// frame at unit `aterm_t0`, and a helper outside the `vs_`/`fs_` names.
    const GLSL_BLIT: &str = r"#version 300 es
precision highp float;
layout(std140) uniform aterm_b2 { // BlitUniform
    vec4 src_rect;
};
uniform highp sampler2D aterm_t0;
vec4 blit_sample(vec2 uv) {
    return texture(aterm_t0, uv);
}
void vs_blit() {
    gl_Position = vec4(0.0);
}
void fs_blit() {
    vec4 c = blit_sample(src_rect.xy);
}
";

    /// An HLSL twin of the blit library in the same convention.
    const HLSL_BLIT: &str = r"cbuffer BlitUniform : register(b2) {
    float4 src_rect;
};
Texture2D<float4> frame : register(t0);
SamplerState frame_sampler : register(s0);
float4 blit_sample(float2 uv) {
    return frame.Sample(frame_sampler, uv);
}
float4 vs_blit(uint id : SV_VertexID) : SV_Position {
    return float4(0, 0, 0, 1);
}
float4 fs_blit(float4 pos : SV_Position) : SV_Target {
    return blit_sample(src_rect.xy);
}
";

    /// The GLSL and HLSL scans read exactly the fixture's entry points and
    /// slots, accept the conforming twin, and refuse a moved slot, a missing
    /// declaration, an extra entry point and a missing one (negative controls),
    /// so the first `.glsl` / `.hlsl` twin lands under a scan that bites.
    #[test]
    fn the_glsl_and_hlsl_scans_read_their_convention_and_refuse_drift() {
        let lib = ShaderLibrary::Blit;
        for (dialect, src, moved_from, moved_to) in [
            (Dialect::Glsl, GLSL_BLIT, "aterm_b2", "aterm_b3"),
            (Dialect::Hlsl, HLSL_BLIT, "register(b2)", "register(b3)"),
        ] {
            let file = format!("fixture.{}", dialect.ext());
            assert_eq!(dialect.entry_points(src), ["vs_blit", "fs_blit"], "{file}");
            check_entry_points(lib, &file, &dialect.entry_points(src)).expect("conforming entries");
            let have = module_bindings(dialect, src).expect("module-scope dialect");
            assert_eq!(
                have,
                ModuleBindings {
                    buffers: vec![(2, if dialect == Dialect::Glsl { 3 } else { 1 })],
                    textures: vec![(0, if dialect == Dialect::Glsl { 6 } else { 4 })],
                    samplers: vec![(0, if dialect == Dialect::Glsl { 6 } else { 5 })],
                },
                "{file}"
            );
            check_module_bindings(lib, &file, &have).expect("conforming bindings");

            let moved = src.replacen(moved_from, moved_to, 1);
            let err = check_module_bindings(lib, &file, &module_bindings(dialect, &moved).unwrap())
                .expect_err("a moved buffer slot must fail");
            assert!(err.contains("buffer slot 3"), "{file}: {err}");

            let no_sampler = match dialect {
                Dialect::Glsl => src.replace("uniform highp sampler2D aterm_t0;", ""),
                _ => src.replace("register(s0)", ""),
            };
            let err =
                check_module_bindings(lib, &file, &module_bindings(dialect, &no_sampler).unwrap())
                    .expect_err("an undeclared tabled slot must fail");
            assert!(err.contains("tabled-but-undeclared"), "{file}: {err}");

            let extra = src.replace("blit_sample(", "fs_blit_sample(");
            let err = check_entry_points(lib, &file, &dialect.entry_points(&extra))
                .expect_err("a helper under an entry-point name must fail");
            assert!(err.contains("fs_blit_sample"), "{file}: {err}");

            let missing = src.replace("fs_blit(", "fragment_blit(");
            check_entry_points(lib, &file, &dialect.entry_points(&missing))
                .expect_err("a missing entry point must fail");
        }
        assert_eq!(
            module_bindings(Dialect::Msl, "fragment float4 fs(x) {}"),
            None,
            "MSL binds per entry point, not at module scope"
        );
    }
}
