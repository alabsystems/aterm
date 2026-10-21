// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The GLSL ES 3.00 twins (`shaders/<library>.glsl`), compiled into the binary.
//! `pipelines` composes one stage of one row from them; the tests below hold
//! each row's stage to the row's bind column, as text, on every host.

use crate::pipeline_table::ShaderLibrary;

/// The GLSL twin of one library.
pub(crate) const fn source(library: ShaderLibrary) -> &'static str {
    match library {
        ShaderLibrary::Cell => include_str!("../../shaders/cell.glsl"),
        ShaderLibrary::Blit => include_str!("../../shaders/blit.glsl"),
        ShaderLibrary::HdrGlow => include_str!("../../shaders/hdr_glow.glsl"),
        ShaderLibrary::Tray => include_str!("../../shaders/tray.glsl"),
        ShaderLibrary::Bloom => include_str!("../../shaders/bloom.glsl"),
        ShaderLibrary::Shimmer => include_str!("../../shaders/shimmer.glsl"),
    }
}

/// The binding check. GLSL ES 3.00 has no binding qualifier, so `pipelines`
/// binds by name, sampler `aterm_t<n>` to unit n and block `aterm_b<n>` to
/// point n, for exactly the row's `BindSpec` slots. A sampler an entry point
/// reads that its row does not bind stays at unit 0 and reads whatever texture
/// is there, with no error from any driver; a slot no sampler declares binds
/// nothing. Each row's two stages are read here as the compiler sees them (the
/// twin under `compose`'s defines) and their reads held to the row. The
/// compiled half of the check is `pipelines::tests` on a GL host.
#[cfg(all(test, any(target_os = "macos", gl_arm)))]
mod tests {
    use super::source;
    use crate::pipeline_table::{ALL_PIPELINES, PipelineSpec};
    use std::collections::{BTreeMap, BTreeSet};

    /// The macros `compose` defines for one stage of one row.
    fn defines(vertex: bool, spec: &PipelineSpec) -> BTreeSet<String> {
        let stage = if vertex {
            "ATERM_VERTEX"
        } else {
            "ATERM_FRAGMENT"
        };
        BTreeSet::from([
            stage.to_owned(),
            format!("ATERM_VS_{}", spec.vs),
            format!("ATERM_FS_{}", spec.fs),
        ])
    }

    /// GLSL tokens: identifiers, numbers, `&&`, `||`, and every other
    /// character alone. Comments are stripped first.
    fn tokens(src: &str) -> Vec<String> {
        let mut out = Vec::new();
        for line in src.lines() {
            let line = line.split_once("//").map_or(line, |(code, _)| code);
            let mut chars = line.char_indices().peekable();
            while let Some((at, c)) = chars.next() {
                if c.is_whitespace() {
                    continue;
                }
                if c.is_ascii_alphanumeric() || c == '_' {
                    let mut end = at + c.len_utf8();
                    while let Some(&(i, d)) = chars.peek() {
                        // A number keeps its `.` and exponent: one token.
                        if d.is_ascii_alphanumeric() || d == '_' || (d == '.' && c.is_ascii_digit())
                        {
                            end = i + d.len_utf8();
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    out.push(line[at..end].to_owned());
                } else if (c == '&' || c == '|') && chars.peek().map(|&(_, d)| d) == Some(c) {
                    chars.next();
                    out.push(format!("{c}{c}"));
                } else {
                    out.push(c.to_string());
                }
            }
        }
        out
    }

    /// An `#if` expression over `defined`, `!`, `&&`, `||` and parentheses,
    /// by precedence; anything else is a construct this reader does not
    /// model, and fails the test rather than guess.
    struct IfExpr<'a> {
        toks: Vec<String>,
        at: usize,
        defs: &'a BTreeSet<String>,
    }

    impl IfExpr<'_> {
        fn eval(expr: &str, defs: &BTreeSet<String>) -> bool {
            let mut e = IfExpr {
                toks: tokens(expr),
                at: 0,
                defs,
            };
            let v = e.or();
            assert_eq!(e.at, e.toks.len(), "`#if {expr}`: trailing tokens");
            v
        }

        fn next(&mut self) -> &str {
            let t = self.toks.get(self.at).map_or("", String::as_str);
            self.at += 1;
            t
        }

        fn peek(&self) -> &str {
            self.toks.get(self.at).map_or("", String::as_str)
        }

        fn or(&mut self) -> bool {
            let mut v = self.and();
            while self.peek() == "||" {
                self.at += 1;
                v |= self.and();
            }
            v
        }

        fn and(&mut self) -> bool {
            let mut v = self.unary();
            while self.peek() == "&&" {
                self.at += 1;
                v &= self.unary();
            }
            v
        }

        fn unary(&mut self) -> bool {
            match self.next().to_owned().as_str() {
                "!" => !self.unary(),
                "(" => {
                    let v = self.or();
                    assert_eq!(self.next(), ")", "unbalanced `#if`");
                    v
                }
                "defined" => {
                    let paren = self.peek() == "(";
                    if paren {
                        self.at += 1;
                    }
                    let name = self.next().to_owned();
                    if paren {
                        assert_eq!(self.next(), ")", "unbalanced `defined(`");
                    }
                    self.defs.contains(&name)
                }
                other => panic!("`#if` operand `{other}` is not modelled"),
            }
        }
    }

    /// The lines of a twin a GLSL preprocessor keeps under `defs`: the
    /// conditional directives evaluated, `#define` recorded, and any other
    /// directive refused.
    fn active(src: &str, defs: &BTreeSet<String>) -> String {
        let mut defs = defs.clone();
        // Per open conditional: (the enclosing region is live, a branch
        // of this one has been taken).
        let mut open: Vec<(bool, bool)> = Vec::new();
        let mut live = true;
        let mut out = String::new();
        for line in src.lines() {
            let Some(directive) = line.trim_start().strip_prefix('#') else {
                if live {
                    out.push_str(line);
                    out.push('\n');
                }
                continue;
            };
            let directive = directive.split_once("//").map_or(directive, |(d, _)| d);
            let (word, rest) = directive
                .trim()
                .split_once(char::is_whitespace)
                .unwrap_or((directive.trim(), ""));
            let rest = rest.trim();
            match word {
                "if" | "ifdef" | "ifndef" => {
                    let v = match word {
                        "ifdef" => defs.contains(rest),
                        "ifndef" => !defs.contains(rest),
                        _ => IfExpr::eval(rest, &defs),
                    };
                    open.push((live, v));
                    live = live && v;
                }
                "else" => {
                    let top = open.last_mut().expect("`#else` with no `#if`");
                    live = top.0 && !top.1;
                    top.1 = true;
                }
                "endif" => live = open.pop().expect("`#endif` with no `#if`").0,
                "define" => {
                    if live {
                        let name = rest.split_whitespace().next().expect("`#define` names");
                        defs.insert(name.to_owned());
                    }
                }
                "version" => {}
                other => panic!("`#{other}` is not modelled; teach this reader it"),
            }
        }
        assert!(open.is_empty(), "an `#if` is never closed");
        out
    }

    /// An identifier token, not a number or punctuation.
    fn is_ident(t: &str) -> bool {
        t.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
    }

    /// The `n` of a resource name `<prefix><n>`.
    fn slot(name: &str, prefix: &str) -> Option<u32> {
        name.strip_prefix(prefix)?.parse().ok()
    }

    /// One stage's source as declarations: what the binding check reads.
    #[derive(Default)]
    struct Stage {
        /// Each function's body as identifiers (overloads merged).
        functions: BTreeMap<String, Vec<String>>,
        /// Each uniform block member, to its block's binding point.
        members: BTreeMap<String, u32>,
        /// The sampler units declared (`aterm_t<n>`).
        samplers: BTreeSet<u32>,
    }

    impl Stage {
        fn read(src: &str) -> Self {
            let toks = tokens(src);
            let mut stage = Self::default();
            let mut i = 0;
            while i < toks.len() {
                if toks[i] == "sampler2D"
                    && let Some(n) = toks.get(i + 1).and_then(|t| slot(t, "aterm_t"))
                {
                    stage.samplers.insert(n);
                }
                if toks[i] != "{" {
                    i += 1;
                    continue;
                }
                // A top-level brace: a function body, a uniform block or a
                // struct. Its extent first.
                let mut depth = 0usize;
                let mut end = i;
                for (j, t) in toks.iter().enumerate().skip(i) {
                    match t.as_str() {
                        "{" => depth += 1,
                        "}" => depth -= 1,
                        _ => {}
                    }
                    if depth == 0 {
                        end = j;
                        break;
                    }
                }
                assert!(end > i, "an unbalanced `{{`");
                let body = &toks[i + 1..end];
                let prev = i.checked_sub(1).map(|p| toks[p].as_str());
                if prev == Some(")") {
                    // `T name(params) {`: the name precedes the matching `(`.
                    let mut depth = 0usize;
                    let mut open = i - 1;
                    while let Some(p) = open.checked_sub(1) {
                        open = p;
                        match toks[open].as_str() {
                            ")" => depth += 1,
                            "(" if depth == 0 => break,
                            "(" => depth -= 1,
                            _ => {}
                        }
                    }
                    let name = toks[open - 1].clone();
                    stage
                        .functions
                        .entry(name)
                        .or_default()
                        .extend(body.iter().filter(|t| is_ident(t)).cloned());
                } else if let Some(n) = prev.and_then(|p| slot(p, "aterm_b")) {
                    // `uniform aterm_b<n> { T member; ... };`
                    for w in body.windows(2) {
                        if (w[1] == ";" || w[1] == "[") && is_ident(&w[0]) {
                            stage.members.insert(w[0].clone(), n);
                        }
                    }
                }
                i = end + 1;
            }
            stage
        }

        /// The sampler units and block points `entry` reads, through every
        /// function it calls.
        fn reads(&self, entry: &str) -> (BTreeSet<u32>, BTreeSet<u32>) {
            assert!(
                self.functions.contains_key(entry),
                "`{entry}` is not defined in this stage"
            );
            let mut seen = BTreeSet::from([entry.to_owned()]);
            let mut todo = vec![entry.to_owned()];
            let (mut samplers, mut blocks) = (BTreeSet::new(), BTreeSet::new());
            while let Some(f) = todo.pop() {
                for id in &self.functions[&f] {
                    if let Some(n) = slot(id, "aterm_t") {
                        samplers.insert(n);
                    } else if let Some(&n) = self.members.get(id) {
                        blocks.insert(n);
                    } else if self.functions.contains_key(id) && seen.insert(id.clone()) {
                        todo.push(id.clone());
                    }
                }
            }
            (samplers, blocks)
        }
    }

    /// The reader itself: nesting, `#else`, a parenthesised `||` under `&&`,
    /// a call chain, and a block member.
    #[test]
    fn the_reader_keeps_what_the_preprocessor_keeps() {
        let src = "#version 300 es\n\
            layout(std140) uniform aterm_b3 { vec2 u_a; float u_b[2]; };\n\
            uniform highp sampler2D aterm_t4;\n\
            #if defined(F) && (defined(X) || defined(Y))\n\
            vec4 leaf() { return texelFetch(aterm_t4, ivec2(0), 0) * u_b[1]; }\n\
            #else\n\
            vec4 leaf() { return vec4(u_a, 0.0, 1.0); }\n\
            #endif\n\
            #ifndef F\n\
            #define G 1\n\
            #endif\n\
            #ifdef G\n\
            void never() {}\n\
            #endif\n\
            void entry() { o = leaf(); } // aterm_t9 in a comment\n";
        let defs = |names: &[&str]| names.iter().map(|&n| n.to_owned()).collect();
        let lit = Stage::read(&active(src, &defs(&["F", "Y"])));
        assert_eq!(
            lit.reads("entry"),
            (BTreeSet::from([4]), BTreeSet::from([3]))
        );
        assert!(!lit.functions.contains_key("never"));
        let dark = Stage::read(&active(src, &defs(&["F"])));
        assert_eq!(dark.reads("entry"), (BTreeSet::new(), BTreeSet::from([3])));
        let unset = Stage::read(&active(src, &defs(&[])));
        assert!(
            unset.functions.contains_key("never"),
            "`#define` is recorded"
        );
        assert_eq!(lit.samplers, BTreeSet::from([4]));
    }

    /// Every row: the samplers and blocks each stage's entry point reads are
    /// the row's bind slots for that stage, every slot it binds is declared,
    /// and a row with a frame palette reads it.
    #[test]
    fn every_row_reads_only_what_its_bind_column_binds() {
        for p in ALL_PIPELINES {
            let spec = p.spec();
            let b = spec.binds;
            let src = source(spec.library);
            let frag = Stage::read(&active(src, &defines(false, spec)));
            let vert = Stage::read(&active(src, &defines(true, spec)));
            let (f_samplers, f_blocks) = frag.reads(spec.fs);
            let (v_samplers, v_blocks) = vert.reads(spec.vs);
            let units: BTreeSet<u32> = b
                .fragment_textures
                .iter()
                .chain(b.fragment_palette.iter())
                .copied()
                .collect();
            let row = p.name();
            assert!(
                f_samplers.is_subset(&units),
                "{row}: `{}` samples units {f_samplers:?}; the row binds {units:?}",
                spec.fs
            );
            assert!(
                units.is_subset(&frag.samplers),
                "{row}: the row binds units {units:?}; the twin declares {:?}",
                frag.samplers
            );
            if let Some(n) = b.fragment_palette {
                assert!(
                    f_samplers.contains(&n),
                    "{row}: the palette at unit {n} is never read by `{}`",
                    spec.fs
                );
            }
            assert!(
                v_samplers.is_empty(),
                "{row}: `{}` samples {v_samplers:?}",
                spec.vs
            );
            let f_points: BTreeSet<u32> = b.fragment_buffers.iter().copied().collect();
            assert!(
                f_blocks.is_subset(&f_points),
                "{row}: `{}` reads blocks {f_blocks:?}; the row binds {f_points:?}",
                spec.fs
            );
            let v_points: BTreeSet<u32> = b.vertex_uniform.into_iter().collect();
            assert!(
                v_blocks.is_subset(&v_points),
                "{row}: `{}` reads blocks {v_blocks:?}; the row binds {v_points:?}",
                spec.vs
            );
        }
    }
}
