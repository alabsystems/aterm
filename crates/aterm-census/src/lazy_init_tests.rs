// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

/// The self-reentrant `debug_seamless_reexec_armed` shape that shipped in
/// v0.65 (fn-local `OnceLock`, multi-statement closure, self-call first)
/// and its main-thread caller.
const V065_SHIPPED_SHAPE: &str = r#"
pub(crate) fn debug_seamless_reexec_armed() -> bool {
    static ARMED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ARMED.get_or_init(|| {
        let on = crate::app_update_screen::debug_seamless_reexec_armed();
        if on {
            aterm_log::warn!("the QA seam is armed");
        }
        on
    })
}

pub(crate) fn apply_staged_update_now() -> bool {
    let debug_seamless = debug_seamless_reexec_armed();
    debug_seamless
}
"#;

/// One synthetic tree: its name, its `(path, source)` files, and `None`
/// for GREEN or `Some(tokens)` for RED with every token in the log.
type Case = (String, Vec<(String, String)>, Option<Vec<&'static str>>);

fn case(name: &str, files: &[(&str, &str)], red: Option<&[&'static str]>) -> Case {
    (
        name.to_string(),
        files
            .iter()
            .map(|(path, body)| ((*path).to_string(), (*body).to_string()))
            .collect(),
        red.map(<[&str]>::to_vec),
    )
}

/// Every shape the derivation must judge: each red one a re-entry it
/// must reach (naming its obligation and cell), each green one a shape
/// that must not fabricate a finding.
#[test]
fn every_reentry_shape_is_judged() {
    let mut cases: Vec<Case> = vec![
        case(
            "a qualified receiver call keeps its conservative edge",
            &[(
                "crates/demo/src/lib.rs",
                r#"
struct Caps;
impl Caps {
    fn standby(&self) -> bool {
        static READ: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *READ.get_or_init(|| Self::standby(self))
    }
}
"#,
            )],
            Some(&["[OB-19]", "`READ`"]),
        ),
        case(
            "the v0.65 self-recursive fn-local OnceLock",
            &[(
                "crates/aterm-gui/src/app_update_screen.rs",
                V065_SHIPPED_SHAPE,
            )],
            Some(&["[OB-19]", "`ARMED`", "debug_seamless_reexec_armed"]),
        ),
        case(
            "the shipped repair of that site (it reads the environment)",
            &[(
                "crates/aterm-gui/src/app_update_screen.rs",
                r#"
pub(crate) fn debug_seamless_reexec_armed() -> bool {
    static ARMED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ARMED.get_or_init(|| {
        let on = std::env::var_os("ATERM_DEBUG_SEAMLESS_REEXEC").is_some();
        if on {
            aterm_log::warn!("the QA seam is armed");
        }
        on
    })
}
"#,
            )],
            None,
        ),
        case(
            "OB-20: two accessors that initialise through each other",
            &[(
                "crates/aterm-gui/src/pair.rs",
                r#"
fn alpha() -> bool {
    static A: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *A.get_or_init(|| beta())
}

fn beta() -> bool {
    static B: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *B.get_or_init(|| alpha())
}
"#,
            )],
            Some(&["[OB-20]", "`A`", "`B`"]),
        ),
        case(
            "a non-blocking get() peek at the own cell",
            &[(
                "crates/aterm-gui/src/peek.rs",
                r#"
fn armed() -> bool {
    static ARMED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ARMED.get_or_init(|| {
        let previously = ARMED.get().copied().unwrap_or(false);
        previously || std::env::var_os("KNOB").is_some()
    })
}
"#,
            )],
            None,
        ),
        case(
            "a deref LazyLock re-entered through a reader",
            &[(
                "crates/aterm-gui/src/table.rs",
                r#"
static TABLE: std::sync::LazyLock<Vec<u8>> = std::sync::LazyLock::new(|| build());

fn build() -> Vec<u8> {
    seed()
}

fn seed() -> Vec<u8> {
    TABLE.clone()
}
"#,
            )],
            Some(&["`TABLE`"]),
        ),
        case(
            "OB-21: an empty tree is not vacuously green",
            &[],
            Some(&["[OB-21]"]),
        ),
        case(
            "a body-less extern declaration absorbs no fn body",
            &[(
                "crates/aterm-gui/src/keymap.rs",
                r#"
unsafe extern "C" {
    fn IOServiceMatching(name: *const u8) -> *mut u8;
    fn IOServiceOpen(
        service: u32,
        task: u32,
    ) -> i32;
}

fn connection() -> Option<u32> {
    static CONN: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
    *CONN.get_or_init(|| unsafe { open_port(IOServiceMatching(b"x\0".as_ptr())) })
}

fn open_port(dict: *mut u8) -> Option<u32> {
    let _ = dict;
    None
}
"#,
            )],
            None,
        ),
        case(
            "a qualified ctor reference (`Type::new`) is not followed",
            &[
                (
                    "crates/aterm-core/src/terminal/color_resolve.rs",
                    r#"
fn transfer_tables() -> &'static TransferTables {
    static TABLES: std::sync::OnceLock<TransferTables> = std::sync::OnceLock::new();
    TABLES.get_or_init(TransferTables::new)
}
"#,
                ),
                (
                    "crates/aterm-core/src/other.rs",
                    r#"
fn new() -> u8 {
    transfer_tables_reader()
}

fn transfer_tables_reader() -> u8 {
    0
}
"#,
                ),
            ],
            None,
        ),
        case(
            "an unqualified fn-reference initializer is followed",
            &[(
                "crates/aterm-gui/src/terse.rs",
                r#"
fn rules() -> &'static Rules {
    static RULES: std::sync::OnceLock<Rules> = std::sync::OnceLock::new();
    RULES.get_or_init(build_rules)
}

fn build_rules() -> Rules {
    rules().clone()
}
"#,
            )],
            Some(&["[OB-19]"]),
        ),
        case(
            "a cfg(test) self-recursion never ships",
            &[(
                "crates/aterm-gui/src/gated.rs",
                r#"
#[cfg(test)]
fn probe() -> bool {
    static P: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *P.get_or_init(|| probe())
}

fn live() -> bool {
    static L: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *L.get_or_init(|| std::env::var_os("K").is_some())
}
"#,
            )],
            None,
        ),
        case(
            "a self-recursive Once::call_once",
            &[(
                "crates/aterm-http/src/tls.rs",
                r#"
fn init_crypto() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        init_crypto();
    });
}
"#,
            )],
            Some(&["[OB-19]"]),
        ),
        case(
            "a OnceLock struct field re-entered from its initializer",
            &[(
                "crates/demo/src/session.rs",
                r#"
pub struct Session {
    cache: std::sync::OnceLock<bool>,
}

impl Session {
    pub fn armed(&self) -> bool {
        *self.cache.get_or_init(|| self.armed())
    }
}
"#,
            )],
            Some(&["[OB-19]", "FIELD"]),
        ),
        case(
            "an ordinary OnceLock field",
            &[(
                "crates/demo/src/session.rs",
                r#"
pub struct Session {
    cache: std::sync::OnceLock<bool>,
    name: String,
}

impl Session {
    pub fn armed(&self) -> bool {
        *self.cache.get_or_init(|| self.name.is_empty())
    }
}
"#,
            )],
            None,
        ),
        case(
            "a receiver rustfmt wrapped onto its own line",
            &[(
                "crates/aterm-gui/src/app_update_screen.rs",
                r#"
pub fn debug_seamless_reexec_armed() -> bool {
    static ARMED_SEAMLESS_REEXEC_FLAG: std::sync::OnceLock<bool> =
        std::sync::OnceLock::new();
    *ARMED_SEAMLESS_REEXEC_FLAG
        .get_or_init(|| crate::app_update_screen::debug_seamless_reexec_armed())
}
"#,
            )],
            Some(&["[OB-19]"]),
        ),
        case(
            "an array length in a signature keeps the fn in the graph",
            &[(
                "crates/demo/src/lut.rs",
                r#"
static TABLE: std::sync::OnceLock<[f32; 256]> = std::sync::OnceLock::new();

pub fn table() -> &'static [f32; 256] {
    TABLE.get_or_init(|| build())
}

fn build() -> [f32; 256] {
    let t = table();
    *t
}
"#,
            )],
            Some(&["[OB-19]"]),
        ),
        case(
            "a wrapped LazyLock ctor after a `;` in the type",
            &[(
                "crates/demo/src/lut.rs",
                r#"
static SRGB_TO_LINEAR_LUT: std::sync::LazyLock<[f32; 256]> =
    std::sync::LazyLock::new(|| build());

fn build() -> [f32; 256] {
    *SRGB_TO_LINEAR_LUT
}
"#,
            )],
            Some(&["[OB-19]"]),
        ),
        case(
            "a turbofished get_or_init",
            &[(
                "crates/demo/src/lib.rs",
                r#"
static ARMED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

pub fn armed() -> bool {
    *ARMED.get_or_init::<_>(|| armed())
}
"#,
            )],
            Some(&["[OB-19]"]),
        ),
    ];
    for vis in ["pub ", "pub(crate) ", "pub(super) "] {
        let body = format!(
            "\n{vis}static ARMED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();\n\n\
             pub fn armed() -> bool {{\n    *ARMED.get_or_init(|| armed())\n}}\n"
        );
        cases.push((
            format!("a `{vis}static` cell"),
            vec![("crates/demo/src/lib.rs".to_string(), body)],
            Some(vec!["[OB-19]"]),
        ));
    }
    let mut multibyte = String::from("(");
    multibyte.push_str(&"x".repeat(39_998));
    multibyte.push('\u{00a0}');
    multibyte.push(')');
    assert!(!balanced(&multibyte, 0).is_empty());
    for (name, sources, red) in &cases {
        let out = run_synth_sources(sources);
        match red {
            None => assert!(out.ok, "{name}: must be GREEN:\n{}", out.log),
            Some(tokens) => {
                assert!(!out.ok, "{name}: must be RED:\n{}", out.log);
                for token in tokens {
                    assert!(
                        out.log.contains(token),
                        "{name}: no `{token}`:\n{}",
                        out.log
                    );
                }
            }
        }
    }
}

/// A borrowed `&OnceLock<T>` parameter is not a declaration; the owner
/// declares the cell.
#[test]
fn a_borrowed_cell_parameter_is_not_a_declaration() {
    assert!(field_cell_name("    cache: std::sync::OnceLock<bool>,").is_some());
    assert!(field_cell_name("    cell: &OnceLock<T>,").is_none());
    assert!(field_cell_name("    let cache: OnceLock<bool> = OnceLock::new(),").is_none());
    assert!(field_cell_name("    pub(crate) cache: OnceLock<bool>,").is_some());
    assert!(field_cell_name("    pub cache: OnceLock<bool>,").is_some());
    assert!(field_cell_name("    name: String,").is_none());
}

/// The name must end at the method: `get_or_init_with` is its own
/// vocabulary entry, not `get_or_init` with a suffix.
#[test]
fn a_longer_method_name_is_not_matched_as_a_shorter_one() {
    let hay = "CELL.get_or_init_with(|| 1)";
    assert!(find_method_call(hay, "get_or_init", 0).is_none());
    assert!(find_method_call(hay, "get_or_init_with", 0).is_some());
    assert!(find_method_call("C.call_once::<F>(f)", "call_once", 0).is_some());
    assert!(find_method_call("C.get_or_init ( f )", "get_or_init", 0).is_some());
    assert!(find_method_call("// see .get_or_init above", "get_or_init", 0).is_none());
}
