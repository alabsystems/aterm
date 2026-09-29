// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! #8001: capability-token structural audit.
//!
//! Validates that the capability structs of each capability-token auth
//! module in `aterm-core` cannot be constructed outside their owning
//! module.
//!
//! # Why a source-scanning test (and not a `trybuild` compile-fail)
//!
//! The capability types are `pub(super)`/`pub(crate)` and carry a
//! private `_seal: ()` field. Rust's visibility rules already make them
//! unconstructable from outside the owning module; adding a `trybuild`
//! matrix would duplicate what rustc enforces. What a refactor *can*
//! silently regress is:
//!
//! 1. Making `_seal` public (downstream code can forge a capability).
//! 2. Making a capability struct `#[derive(Default)]` or adding a
//!    `new()` constructor outside the minting path.
//!
//! The test below scans the committed sources of the capability modules
//! listed in [`CAPABILITY_MODULES`] and fails on any of those
//! regressions, which closes the #8001 acceptance criterion that
//! "handler code cannot construct the token without going through
//! `authorize()`."

use std::fs;
use std::path::{Path, PathBuf};

/// All capability-token auth modules whose capability structs are audited.
///
/// Each entry is the module file name (under
/// `crates/aterm-core/src/terminal/`) paired with a list of capability
/// struct names that must carry a private `_seal: ()` field.
///
/// `hyperlink_auth.rs` is deliberately ABSENT: OSC 8 acceptance is not a
/// host decision — a URI that clears the byte cap, the control/BiDi scans
/// and the scheme allowlist is carried on every host, and what a click may
/// OPEN is decided in the GUI at press time. A capability whose only
/// reachable setting is "granted" audits as a decision nobody makes, so
/// that module keeps only its host-minted scheme set. Add a row here when
/// a capability gains a policy, never to restore ceremony for its own sake.
const CAPABILITY_MODULES: &[(&str, &[&str])] = &[
    // (file, capability struct names)
    (
        "clipboard_auth.rs",
        &["ClipboardWriteCapability", "ClipboardQueryCapability"],
    ),
    ("response_capability.rs", &["ResponseCapability"]),
    ("window_auth.rs", &["WindowOpsCapability"]),
    // `dcs_auth.rs` left 2026-09-25 with the raw DCS callback it gated: no
    // host could install that callback or revoke the gate any more, so the
    // capability had become ceremony over a policy that cannot refuse.
];

/// The modules whose ceremony would be a lie, paired with the reason. A
/// capability token is only meaningful while some reachable policy can
/// WITHHOLD it; minted from a bit that is always `true`, it audits as a
/// decision nobody makes, and a reader trusts a gate that gates nothing.
///
/// `hyperlink_auth` is that module: OSC 8 admission is decided by the URI
/// (byte cap, control/BiDi scans, scheme allowlist) plus the host-minted
/// extra-scheme set, and no host — on any platform — can turn OSC 8 off.
/// What a click may OPEN is a separate boundary in `aterm-gui`
/// (`is_safe_url` guarding `open_url_external`).
const CEREMONY_FREE_MODULES: &[(&str, &[&str])] = &[(
    "hyperlink_auth.rs",
    &[
        "struct HyperlinkCapability",
        "struct HyperlinkMintAuthority",
        "fn try_mint_capability",
        "fn try_mint_with_policy",
        "fn as_host_auth_token(",
        "authorized: bool",
    ],
)];

fn terminal_src_dir() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    Path::new(manifest_dir).join("src").join("terminal")
}

fn read_module(file: &str) -> String {
    let path = terminal_src_dir().join(file);
    fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()))
}

/// Each capability struct carries a private `_seal: ()` field. This is
/// the structural guarantee that handler code outside the module cannot
/// construct the capability — the type's only field is inaccessible.
#[test]
fn every_capability_struct_has_private_seal() {
    for (file, caps) in CAPABILITY_MODULES {
        let src = read_module(file);
        for cap in *caps {
            // Find the struct definition and verify it contains `_seal: ()`.
            let struct_header = format!("struct {cap}");
            let Some(struct_idx) = src.find(&struct_header) else {
                panic!("{file}: capability struct `{cap}` not found");
            };
            // Look within the next 400 bytes for the seal field.
            let window_end = (struct_idx + 400).min(src.len());
            let window = &src[struct_idx..window_end];
            assert!(
                window.contains("_seal: ()"),
                "{file}: capability `{cap}` must have a private `_seal: ()` field \
                 (blocks outside-module construction)"
            );
        }
    }
}

/// No capability struct derives `Default` or `Clone` — deriving either
/// would re-open construction-from-thin-air for any consumer who can
/// name the type. Clone is particularly dangerous for `#[must_use]`
/// capabilities: it would let a handler reuse a single mint across
/// multiple dispatches.
#[test]
fn capability_structs_do_not_derive_default_or_clone() {
    for (file, caps) in CAPABILITY_MODULES {
        let src = read_module(file);
        for cap in *caps {
            let struct_header = format!("struct {cap}");
            let Some(struct_idx) = src.find(&struct_header) else {
                panic!("{file}: capability struct `{cap}` not found");
            };
            // Scan the 200 bytes immediately *before* the struct header
            // for a `#[derive(...)]` attribute that includes `Default`
            // or `Clone`.
            let before_start = struct_idx.saturating_sub(200);
            let before = &src[before_start..struct_idx];
            assert!(
                !before.contains("Default") || !before.contains("#[derive"),
                "{file}: capability `{cap}` must not `#[derive(Default)]` — \
                 that would let any consumer mint a capability without authorization"
            );
            assert!(
                !before.contains("Clone") || !before.contains("#[derive"),
                "{file}: capability `{cap}` must not `#[derive(Clone)]` — \
                 that would let a handler replay a single mint across dispatches"
            );
        }
    }
}

/// P4 (`docs/HARDCORE_BACKLOG.md`) is carried by the capability tokens above,
/// not by relabelling tainted bytes: `aterm-provenance` declares no function
/// that takes a `Pty` provenance and hands back a `Host` one (or one of any
/// origin its caller picks), and no mint feature. The lift it once had
/// (`authorize_pty_to_host` over an `internal-mint`-sealed token) never had a
/// production caller and was deleted on 2026-09-27. A lift re-added without a
/// caller would be a second, unused gate beside the one every token-gated sink
/// crosses, so bringing it back is a decision: re-scope P4 to shape (b) in the
/// backlog first, then delete this test.
///
/// The scan reads SIGNATURES — free functions, inherent methods (whose
/// receiver takes its origin from the `impl` header) and trait impls such as
/// `From<Provenance<_, Pty>> for Provenance<_, Host>` — in every file under
/// `aterm-provenance/src`. It is lexical and it does not read bodies: the
/// public `Provenance::from_host(p.as_ref().clone())` relabels today, which
/// is why shape (b) would first have to seal `from_host`.
#[test]
fn provenance_exports_no_pty_to_host_lift() {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .join("aterm-provenance");
    let mut files = Vec::new();
    collect_rs_files(&crate_dir.join("src"), &mut files);
    assert!(
        !files.is_empty(),
        "no sources under aterm-provenance/src — the scan would be vacuous"
    );
    for file in &files {
        let src = fs::read_to_string(file)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", file.display()));
        let lifts = pty_to_host_lifts(&src);
        assert!(
            lifts.is_empty(),
            "{}: declares a Pty -> Host lift {lifts:?}: P4 is carried by the capability \
             sink tokens, see docs/HARDCORE_BACKLOG.md P4",
            file.display()
        );
    }
    let manifest = fs::read_to_string(crate_dir.join("Cargo.toml"))
        .unwrap_or_else(|err| panic!("failed to read aterm-provenance/Cargo.toml: {err}"));
    assert!(
        !manifest.contains("[features]"),
        "aterm-provenance declares a feature again: the `internal-mint` seal left with \
         the lift it guarded (docs/HARDCORE_BACKLOG.md P4)"
    );
}

/// Negative controls for [`pty_to_host_lifts`]: each shape of a lift is caught
/// under a name that says nothing about authorization, and the crate's real
/// constructors and accessors are not.
#[test]
fn the_lift_scan_catches_every_shape_of_a_relabel() {
    let caught = [
        // The shape the deleted lift had, under another name.
        "pub fn launder<T>(p: Provenance<T, Pty>, t: Token) -> Provenance<T, Host> { todo!() }",
        // An inherent method: the receiver's origin comes from the impl header.
        "impl<T> Provenance<T, Pty> { pub fn into_host(self) -> Provenance<T, Host> { todo!() } }",
        // A trait impl whose output is `Self`.
        "impl<T> From<Provenance<T, Pty>> for Provenance<T, Host> {\n    fn from(p: Provenance<T, Pty>) -> Self { todo!() }\n}",
        // A generic relabel: the caller picks the output origin.
        "impl<T, O: Origin> Provenance<T, O> {\n    pub fn relabel<P: Origin>(self) -> Provenance<T, P> { todo!() }\n}",
        // Any origin in, `Host` out.
        "pub fn wrap<T, O: Origin>(p: Provenance<T, O>) -> Provenance<T, Host> { todo!() }",
        // A borrowed relabel.
        "pub const fn trust_ref<T: ?Sized>(v: &Provenance<T, Pty>) -> &Provenance<T, Host> { todo!() }",
    ];
    for src in caught {
        assert_eq!(pty_to_host_lifts(src).len(), 1, "not caught: {src}");
    }
    let clean = [
        "impl<T> Provenance<T, Host> { pub const fn from_host(value: T) -> Self { todo!() } }",
        "impl<T> Provenance<T, Pty> { pub const fn from_pty(value: T) -> Self { todo!() } }",
        "impl<T: Clone, O: Origin> Clone for Provenance<T, O> { fn clone(&self) -> Self { todo!() } }",
        "impl<T> From<T> for Provenance<T, Host> { fn from(value: T) -> Self { todo!() } }",
        "pub const fn pty_wrap_ref<T: ?Sized>(value: &T) -> &Provenance<T, Pty> { todo!() }",
        // An origin-preserving generic.
        "pub fn keep<T, O: Origin>(p: Provenance<T, O>) -> Provenance<T, O> { todo!() }",
        // A `fn` pointer type and a lift spelled only in a comment or a string.
        "pub struct P<O> { o: PhantomData<fn() -> O> }\n// fn f(p: Provenance<T, Pty>) -> Provenance<T, Host>\nconst S: &str = \"fn g(p: Provenance<T, Pty>) -> Provenance<T, Host> {\";",
    ];
    for src in clean {
        assert_eq!(
            pty_to_host_lifts(src),
            Vec::<String>::new(),
            "false positive: {src}"
        );
    }
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        fs::read_dir(dir).unwrap_or_else(|err| panic!("failed to list {}: {err}", dir.display()));
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// The signatures in `src` that relabel a `Pty` provenance as `Host` (or as an
/// origin chosen by the caller). See [`provenance_exports_no_pty_to_host_lift`].
fn pty_to_host_lifts(src: &str) -> Vec<String> {
    let code: Vec<char> = code_only(src).chars().collect();
    // One entry per open brace: the `impl` header that opened it, if any.
    let mut blocks: Vec<Option<String>> = Vec::new();
    let mut pending_impl: Option<String> = None;
    let mut lifts = Vec::new();
    let mut i = 0;
    while i < code.len() {
        if keyword_at(&code, i, "impl") {
            let end = item_head_end(&code, i);
            pending_impl = Some(squash(&code[i..end]));
            i = end;
            continue;
        }
        if keyword_at(&code, i, "fn") && fn_item_follows(&code, i + 2) {
            let end = item_head_end(&code, i);
            let sig = squash(&code[i..end]);
            let header = blocks.iter().rev().find_map(Clone::clone);
            if is_lift(&sig, header.as_deref()) {
                lifts.push(sig);
            }
            i = end;
            continue;
        }
        match code[i] {
            '{' => blocks.push(pending_impl.take()),
            '}' => {
                blocks.pop();
            }
            _ => {}
        }
        i += 1;
    }
    lifts
}

/// `src` with comments, string literals and char literals blanked out, so
/// braces and signatures inside them are not read as code.
fn code_only(src: &str) -> String {
    let c: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < c.len() {
        if c[i] == '/' && c.get(i + 1) == Some(&'/') {
            while i < c.len() && c[i] != '\n' {
                i += 1;
            }
        } else if c[i] == '/' && c.get(i + 1) == Some(&'*') {
            i += 2;
            while i < c.len() && !(c[i] == '*' && c.get(i + 1) == Some(&'/')) {
                i += 1;
            }
            i += 2;
            out.push(' ');
        } else if c[i] == 'r'
            && matches!(c.get(i + 1), Some('"' | '#'))
            && !ident_char_before(&c, i)
        {
            let mut j = i + 1;
            let mut hashes = 0;
            while c.get(j) == Some(&'#') {
                hashes += 1;
                j += 1;
            }
            if c.get(j) != Some(&'"') {
                out.push(c[i]);
                i += 1;
                continue;
            }
            j += 1;
            while j < c.len() && !(c[j] == '"' && (1..=hashes).all(|k| c.get(j + k) == Some(&'#')))
            {
                j += 1;
            }
            i = j + 1 + hashes;
            out.push_str("\"\"");
        } else if c[i] == '"' {
            i += 1;
            while i < c.len() && c[i] != '"' {
                i += if c[i] == '\\' { 2 } else { 1 };
            }
            i += 1;
            out.push_str("\"\"");
        } else if c[i] == '\'' && (c.get(i + 2) == Some(&'\'') || c.get(i + 1) == Some(&'\\')) {
            // A char literal (`'{'`, `'\n'`, `'\u{7b}'`); a lifetime has no closing quote.
            i += 1;
            while i < c.len() && c[i] != '\'' {
                i += if c[i] == '\\' { 2 } else { 1 };
            }
            i += 1;
            out.push_str("' '");
        } else {
            out.push(c[i]);
            i += 1;
        }
    }
    out
}

/// `code` with every run of whitespace collapsed to one space.
fn squash(code: &[char]) -> String {
    code.iter()
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn ident_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

fn ident_char_before(c: &[char], i: usize) -> bool {
    i > 0 && ident_char(c[i - 1])
}

fn keyword_at(c: &[char], i: usize, kw: &str) -> bool {
    let kw: Vec<char> = kw.chars().collect();
    c.get(i..i + kw.len()) == Some(&kw[..])
        && !ident_char_before(c, i)
        && !c.get(i + kw.len()).copied().is_some_and(ident_char)
}

/// `fn name` (an item), not `fn(` (a function-pointer type).
fn fn_item_follows(c: &[char], mut i: usize) -> bool {
    while c.get(i).is_some_and(|ch| ch.is_whitespace()) {
        i += 1;
    }
    c.get(i).copied().is_some_and(ident_char)
}

/// Where an `impl`/`fn` head ends: its body's `{`, its `;`, or an enclosing `}`.
fn item_head_end(c: &[char], mut i: usize) -> usize {
    while i < c.len() && !matches!(c[i], '{' | ';' | '}') {
        i += 1;
    }
    i
}

/// Whether one signature relabels: some input origin may be `Pty` (`Pty` itself
/// or a generic parameter) and some output origin is `Host`, or is a parameter of
/// the function's own that no input fixes (so the caller may pick `Host`).
fn is_lift(sig: &str, impl_header: Option<&str>) -> bool {
    let Some(open) = sig.find('(') else {
        return false;
    };
    let close = matching(sig, open, '(', ')').unwrap_or(sig.len());
    let params = &sig[open + 1..close];
    let ret = sig[close..].split_once("->").map_or("", |(_, r)| r);
    let ret = ret.split(" where ").next().unwrap_or(ret).trim();
    let fn_generics = generic_names(&sig[..open]);
    let (impl_self, impl_generics) = impl_header.map_or((String::new(), Vec::new()), |h| {
        (impl_self_type(h), generic_names(h))
    });
    let first_param = params.split(',').next().unwrap_or("").trim();
    let receiver = first_param
        .trim_start_matches('&')
        .trim_start_matches("mut ")
        .trim_start();
    let has_receiver = receiver == "self" || receiver.starts_with("self:");
    let mut inputs = provenance_origins(params);
    if has_receiver {
        inputs.extend(provenance_origins(&impl_self));
    }
    let takes_pty = inputs
        .iter()
        .any(|o| o == "Pty" || impl_generics.contains(o) || fn_generics.contains(o));
    let ret = if ret.trim_start_matches('&') == "Self" {
        impl_self.as_str()
    } else {
        ret
    };
    let gives_host = provenance_origins(ret)
        .iter()
        .any(|o| o == "Host" || (fn_generics.contains(o) && !inputs.contains(o)));
    takes_pty && gives_host
}

/// The self type of an `impl` head: after its last ` for `, else after `impl<…>`.
fn impl_self_type(header: &str) -> String {
    if let Some((_, self_ty)) = header.rsplit_once(" for ") {
        return self_ty.to_string();
    }
    let body = header.trim_start_matches("impl").trim_start();
    let skip = if body.starts_with('<') {
        matching(body, 0, '<', '>').map_or(0, |e| e + 1)
    } else {
        0
    };
    body[skip..].to_string()
}

/// The origin (last) argument of every `Provenance<…>` spelled in `ty`.
fn provenance_origins(ty: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = ty[from..].find("Provenance") {
        let start = from + at + "Provenance".len();
        let rest = ty[start..].trim_start_matches("::");
        let open = ty.len() - rest.len();
        from = start;
        if !rest.starts_with('<') {
            continue;
        }
        let Some(close) = matching(ty, open, '<', '>') else {
            break;
        };
        let args = split_top_level(&ty[open + 1..close]);
        if let Some(last) = args.last() {
            out.push(last.trim().to_string());
        }
        from = close;
    }
    out
}

/// The names declared in the first `<…>` of an `impl<…>` or `fn name<…>` head.
fn generic_names(head: &str) -> Vec<String> {
    let Some(open) = head.find('<') else {
        return Vec::new();
    };
    let Some(close) = matching(head, open, '<', '>') else {
        return Vec::new();
    };
    split_top_level(&head[open + 1..close])
        .iter()
        .map(|g| {
            g.trim()
                .trim_start_matches("const ")
                .split(|ch: char| ch == ':' || ch == '=' || ch.is_whitespace())
                .next()
                .unwrap_or("")
                .to_string()
        })
        .filter(|g| !g.is_empty() && !g.starts_with('\''))
        .collect()
}

/// The index of the bracket closing the one at `open`, skipping the `>` of `->`.
fn matching(s: &str, open: usize, lo: char, hi: char) -> Option<usize> {
    let mut depth = 0usize;
    let mut prev = ' ';
    for (i, ch) in s.char_indices().skip_while(|&(i, _)| i < open) {
        if ch == lo {
            depth += 1;
        } else if ch == hi && !(hi == '>' && prev == '-') {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        prev = ch;
    }
    None
}

fn split_top_level(s: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut depth = 0i32;
    let mut prev = ' ';
    for ch in s.chars() {
        match ch {
            '<' | '(' | '[' => depth += 1,
            '>' if prev != '-' => depth -= 1,
            ')' | ']' => depth -= 1,
            ',' if depth == 0 => {
                out.push(String::new());
                prev = ch;
                continue;
            }
            _ => {}
        }
        out.last_mut().expect("non-empty").push(ch);
        prev = ch;
    }
    out
}

/// Every capability module listed in `CAPABILITY_MODULES` actually
/// exists on disk. Guards against a rename that silently drops coverage
/// from this test matrix.
#[test]
fn all_capability_modules_exist() {
    let dir = terminal_src_dir();
    for (file, _) in CAPABILITY_MODULES {
        let path = dir.join(file);
        assert!(
            path.is_file(),
            "capability module `{file}` does not exist at {}",
            path.display()
        );
    }
}

/// A module with no withholding policy carries no capability ceremony.
/// The two halves are one decision: a capability arriving without a
/// caller that can refuse it, or a policy arriving without a row in
/// `CAPABILITY_MODULES`, are both this test failing.
#[test]
fn a_module_with_no_policy_carries_no_capability_ceremony() {
    let listed: Vec<&str> = CAPABILITY_MODULES.iter().map(|(f, _)| *f).collect();
    for (file, forbidden) in CEREMONY_FREE_MODULES {
        assert!(
            !listed.contains(file),
            "{file} is in both CAPABILITY_MODULES and CEREMONY_FREE_MODULES — \
             a module either has a policy that can withhold its capability or it does not"
        );
        let src = read_module(file);
        for needle in *forbidden {
            assert!(
                !src.contains(needle),
                "{file}: `{needle}` is capability ceremony over a policy that cannot refuse. \
                 Give the capability a caller that can withhold it and add a \
                 CAPABILITY_MODULES row, or leave the module saying plainly what it decides"
            );
        }
    }
}

/// `hyperlink_auth`'s extra-scheme set is a decision that has callers:
/// `aterm-wasm` and `aterm-gpu-web` both export it so an embedding app
/// can deep-link its own scheme. Carrying no acceptance switch is not
/// the same as carrying no policy, and the scheme API is the policy.
#[test]
fn hyperlink_auth_keeps_the_host_minted_scheme_decision() {
    let src = read_module("hyperlink_auth.rs");
    for needle in [
        "fn authorize_scheme(",
        "fn revoke_scheme(",
        "fn extra_schemes(",
        "NEVER_ALLOW_SCHEMES",
    ] {
        assert!(
            src.contains(needle),
            "hyperlink_auth.rs must keep `{needle}` — the scheme set is the ONE \
             hyperlink decision a host makes, and hosts make it"
        );
    }
}
