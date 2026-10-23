// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! `eager-drop`: reject `bool::then_some` when constructing a type that implements `Drop`.

use super::rules::PRODUCT;
use super::{Finding, Rule, SourceFile, Tree, is_word};

pub(super) const EAGER_DROP: Rule = Rule {
    id: "eager-drop",
    why: "`then_some` builds its argument before the test, so on false a value whose type has \
          a Drop is built and dropped at once (a NULL CFRelease traps): write `then(|| …)`",
    scope: &[],
    check: eager_drop,
};

fn eager_drop(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    for file in tree.corpus.under(PRODUCT) {
        if file.mentions(&["then_some"]) && file.mentions(&["Drop for"]) {
            check_file(file, out);
        }
    }
}

fn check_file(file: &SourceFile, out: &mut Vec<Finding>) {
    let code: Vec<&str> = file.lexed().code.lines().collect();
    let dropped: Vec<&str> = code.iter().filter_map(|line| drop_impl(line)).collect();
    if dropped.is_empty() {
        return;
    }
    let mut self_ty: Option<&str> = None;
    for (i, line) in code.iter().enumerate() {
        if let Some(header) = item_header(line) {
            self_ty = header;
        }
        for (at, _) in line.match_indices("then_some(") {
            if at > 0 && is_word(line.as_bytes()[at - 1]) {
                continue;
            }
            let mut arg = line[at + "then_some(".len()..].trim_start();
            if arg.is_empty() {
                arg = code.get(i + 1).map_or("", |next| next.trim_start());
            }
            let Some(name) = constructed(arg) else {
                continue;
            };
            let ty = if name == "Self" { self_ty } else { Some(name) };
            if let Some(ty) = ty.filter(|ty| dropped.contains(ty)) {
                out.push(file.finding(
                    EAGER_DROP.id,
                    i + 1,
                    format!("`then_some` builds a `{ty}`, whose Drop then runs on false"),
                ));
            }
        }
    }
}

fn drop_impl(line: &str) -> Option<&str> {
    let t = line.trim_start();
    let t = t.strip_prefix("unsafe ").unwrap_or(t);
    if !(t.starts_with("impl ") || t.starts_with("impl<")) {
        return None;
    }
    let (at, _) = t
        .match_indices("Drop for ")
        .find(|&(at, _)| !is_word(t.as_bytes()[at - 1]))?;
    let name = last_segment(&t[at + "Drop for ".len()..]);
    (!name.is_empty()).then_some(name)
}

fn item_header(line: &str) -> Option<Option<&str>> {
    let mut t = line.trim_start();
    loop {
        let before = t;
        for word in ["pub(crate) ", "pub(super) ", "pub ", "unsafe "] {
            t = t.strip_prefix(word).unwrap_or(t);
        }
        if t == before {
            break;
        }
    }
    if t.starts_with("trait ") {
        return Some(None);
    }
    let rest = t.strip_prefix("impl")?;
    let rest = match rest.as_bytes().first() {
        Some(b'<') => skip_generics(rest)?,
        Some(b' ') => rest,
        _ => return None,
    };
    let ty = rest.split_once(" for ").map_or(rest, |(_, ty)| ty);
    let name = last_segment(ty);
    Some((!name.is_empty()).then_some(name))
}

fn skip_generics(s: &str) -> Option<&str> {
    let mut depth = 0usize;
    for (at, c) in s.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(&s[at + 1..]);
                }
            }
            _ => {}
        }
    }
    None
}

fn split_path(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    let end = s
        .bytes()
        .position(|b| !(is_word(b) || b == b':'))
        .unwrap_or(s.len());
    let path = &s[..end];
    (path.rsplit("::").next().unwrap_or(path), &s[end..])
}

fn last_segment(s: &str) -> &str {
    split_path(s).0
}

fn constructed(arg: &str) -> Option<&str> {
    let (name, after) = split_path(arg);
    let after = after.trim_start();
    (name.starts_with(|c: char| c.is_ascii_uppercase())
        && (after.starts_with('(') || after.starts_with('{')))
    .then_some(name)
}

#[cfg(test)]
mod tests {
    use super::super::tests::{run, spots};
    use super::*;

    const OWNER: &str = "struct Cf(Ref);\n\
                         impl Cf {\n    \
                             fn adopt(p: Ref) -> Option<Self> {\n        \
                                 (!p.is_null()).then_some(Self(p))\n    \
                             }\n\
                         }\n\
                         impl Drop for Cf {\n    \
                             fn drop(&mut self) { release(self.0) }\n\
                         }\n\
                         struct Plain { a: u8 }\n\
                         impl Plain {\n    \
                             fn new(a: u8) -> Option<Self> { (a > 0).then_some(Self { a }) }\n\
                         }\n";

    #[test]
    fn a_drop_type_built_eagerly_is_found_and_a_lazy_or_plain_one_is_not() {
        let lazy = OWNER.replace("then_some(Self(p))", "then(|| Self(p))");
        let named = "fn f(p: Ref, ok: bool) -> Option<Cf> {\n    \
                         // ok.then_some(Cf(p))\n    \
                         let s = \"ok.then_some(Cf(p))\";\n    \
                         let _ = ok.then_some(Plain { a: 1 });\n    \
                         ok.then_some(\n        Cf(p),\n    )\n\
                     }\n";
        let found = run(
            &EAGER_DROP,
            &[
                ("crates/x/src/eager.rs", OWNER),
                ("crates/x/src/lazy.rs", &lazy),
                ("crates/x/src/named.rs", &format!("{OWNER}{named}")),
                ("crates/x/src/no_drop.rs", named),
            ],
        );
        assert_eq!(
            spots(&found),
            [
                "crates/x/src/eager.rs:4",
                "crates/x/src/named.rs:4",
                "crates/x/src/named.rs:18",
            ]
        );
    }

    #[test]
    fn self_is_the_nearest_impl_and_a_trait_is_no_type() {
        assert_eq!(
            item_header("impl<'a, T: Copy> Drop for Guard<'a, T> {"),
            Some(Some("Guard"))
        );
        assert_eq!(
            item_header("    pub unsafe impl Send for crate::x::Accel {}"),
            Some(Some("Accel"))
        );
        assert_eq!(item_header("impl Cf {"), Some(Some("Cf")));
        assert_eq!(item_header("pub trait Adopt {"), Some(None));
        assert_eq!(item_header("let imply = 1;"), None);
        assert_eq!(
            drop_impl("    impl Drop for OwnedIcon {"),
            Some("OwnedIcon")
        );
        assert_eq!(
            drop_impl("impl std::ops::Drop for Release<'_> {"),
            Some("Release")
        );
        assert_eq!(drop_impl("impl NoDrop for Plain {"), None);
        assert_eq!(constructed("Self { kq: fd })"), Some("Self"));
        assert_eq!(constructed("crate::Group { pid })"), Some("Group"));
        assert_eq!(constructed("(a, b))"), None);
        assert_eq!(constructed("value)"), None);
    }
}
