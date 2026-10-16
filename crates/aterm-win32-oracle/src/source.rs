// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//! The pinned generated upstream vocabulary, preserving source bodies verbatim.
use crate::{Result, read};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub type Key = (Vec<String>, String);
#[derive(Clone, Debug)]
pub struct Item {
    pub module: Vec<String>,
    pub name: String,
    pub kind: String,
    pub text: String,
    pub order: usize,
    pub impls: Vec<(Vec<String>, String)>,
    pub features: BTreeSet<String>,
    pub arch: Vec<String>,
    pub other: Vec<String>,
}
impl Item {
    pub fn new(
        module: Vec<String>,
        name: String,
        kind: String,
        attrs: Vec<String>,
        text: String,
        order: usize,
    ) -> Result<Self> {
        let (features, arch, other) = attributes(&attrs)?;
        Ok(Self {
            module,
            name,
            kind,
            text,
            order,
            impls: vec![],
            features,
            arch,
            other,
        })
    }
    pub fn key(&self) -> Key {
        (self.module.clone(), self.name.clone())
    }
}
#[derive(Clone, Debug)]
pub struct Module {
    pub file: PathBuf,
    pub feature: Option<String>,
    pub children: Vec<(String, String)>,
    pub items: Vec<Item>,
}
pub struct Upstream {
    pub root: PathBuf,
    pub modules: BTreeMap<Vec<String>, Module>,
    pub items: BTreeMap<Key, Vec<Item>>,
    pub features: BTreeMap<String, Vec<String>>,
}

pub fn ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}
#[derive(Clone, Debug)]
pub struct Token {
    pub text: String,
    pub start: usize,
    pub end: usize,
}
/// Token offsets remain offsets into the original UTF-8 source. String payloads
/// are retained only when requested; comments use the census's tested lexer.
pub fn tokens(src: &str, literals: bool) -> Vec<Token> {
    let views = aterm_census::rust_source_views(src);
    let view = if literals { views.1 } else { views.0 };
    let b = view.as_bytes();
    let mut i = 0;
    let mut out = vec![];
    while i < b.len() {
        if b[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        if b[i] == b'"' {
            i += 1;
            while i < b.len() {
                if b[i] == b'\\' {
                    i = (i + 2).min(b.len())
                } else if b[i] == b'"' {
                    i += 1;
                    break;
                } else {
                    i += 1
                }
            }
        } else if ident(b[i]) || b[i] == b'$' {
            i += 1;
            while i < b.len() && ident(b[i]) {
                i += 1
            }
        } else if b[i..].starts_with(b"::") {
            i += 2
        } else {
            i += view[i..].chars().next().map_or(1, char::len_utf8)
        }
        out.push(Token {
            text: view[start..i].to_string(),
            start,
            end: i,
        });
    }
    out
}
pub fn quoted(src: &str) -> Vec<String> {
    tokens(src, true)
        .into_iter()
        .filter_map(|t| {
            t.text
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .map(str::to_string)
        })
        .collect()
}
pub fn attributes(attrs: &[String]) -> Result<(BTreeSet<String>, Vec<String>, Vec<String>)> {
    let mut features = BTreeSet::new();
    let mut arch = vec![];
    let mut other = vec![];
    for a in attrs {
        if !a.starts_with("#[cfg(") {
            other.push(a.clone());
            continue;
        }
        if a.contains("feature = ") {
            let ts = tokens(a, true);
            let mut rest = vec![];
            let mut i = 0;
            while i < ts.len() {
                if ts[i].text == "feature" && ts.get(i + 1).is_some_and(|t| t.text == "=") {
                    let s = ts
                        .get(i + 2)
                        .ok_or("missing feature name")?
                        .text
                        .trim_matches('"')
                        .to_string();
                    features.insert(s);
                    i += 3;
                } else {
                    rest.push(ts[i].text.as_str());
                    i += 1
                }
            }
            let s = rest.join("");
            if s != "#[cfg()]" && s != "#[cfg(all())]" && !s.replace(',', "").eq("#[cfg(all())]") {
                return Err(format!("mixed feature/architecture cfg: {a}"));
            }
        } else {
            arch.push(a.clone())
        }
    }
    Ok((features, arch, other))
}
pub fn split_top(text: &str, separator: &str) -> Vec<String> {
    let ts = tokens(text, true);
    let mut depth = 0i32;
    let mut start = 0;
    let mut out = vec![];
    for (i, t) in ts.iter().enumerate() {
        let arrow = t.text == ">" && i > 0 && ts[i - 1].text == "-";
        if ["(", "[", "{", "<"].contains(&t.text.as_str()) {
            depth += 1
        } else if [")", "]", "}", ">"].contains(&t.text.as_str()) && !arrow {
            depth -= 1
        }
        if t.text == separator && depth == 0 {
            out.push(text[start..t.start].trim().to_string());
            start = t.end
        }
    }
    if !text[start..].trim().is_empty() {
        out.push(text[start..].trim().to_string())
    }
    out
}
fn children(text: &str) -> Vec<(String, String)> {
    let mut feature = None;
    let mut out = vec![];
    for line in text.lines() {
        if line.starts_with("#[cfg(feature = ") {
            feature = quoted(line).first().cloned()
        } else if let Some(name) = line
            .strip_prefix("pub mod ")
            .and_then(|s| s.strip_suffix(';'))
        {
            if let Some(f) = feature.take() {
                out.push((name.to_string(), f))
            }
        } else if !line.trim().is_empty() {
            feature = None
        }
    }
    out
}
fn parse_file(path: &Path, module: &[String]) -> Result<Vec<Item>> {
    let src = read(path)?;
    let code = aterm_census::rust_source_views(&src).0;
    let lines: Vec<&str> = src.lines().collect();
    let codes: Vec<&str> = code.lines().collect();
    let mut attrs = vec![];
    let mut items: Vec<Item> = vec![];
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if line.trim().is_empty() {
            i += 1;
            continue;
        }
        if line.starts_with("#[") || line.starts_with("///") {
            attrs.push(line.to_string());
            i += 1;
            continue;
        }
        if codes[i].trim().is_empty() {
            i += 1;
            continue;
        }
        let start = i;
        let mut depth = 0i32;
        loop {
            for c in codes[i].chars() {
                if "([{ ".contains(c) && c != ' ' {
                    depth += 1
                } else if ")] }".contains(c) && c != ' ' {
                    depth -= 1
                }
            }
            let end = lines[i].trim_end();
            i += 1;
            if depth == 0 && (end.ends_with(';') || end.ends_with('}')) {
                break;
            }
            if i == lines.len() {
                return Err(format!(
                    "{}:{} unterminated item",
                    path.display(),
                    start + 1
                ));
            }
        }
        let text = lines[start..i].join("\n");
        let ts = tokens(&text, true);
        let words: Vec<&str> = ts.iter().map(|t| t.text.as_str()).collect();
        if words.starts_with(&["pub", "mod"])
            || text == "mod literals;"
            || text == "pub use literals::*;"
            || (words.len() == 4
                && words.starts_with(&["pub", "use"])
                && words[3] == ";"
                && items
                    .iter()
                    .any(|item| item.kind == "macro" && item.name == words[2]))
        {
            attrs.clear();
            continue;
        }
        if words.first() == Some(&"impl") {
            let name = if words.get(1) == Some(&"Default") {
                words.get(3)
            } else {
                words.get(1)
            }
            .ok_or("impl lacks owner")?;
            let owner = items
                .iter_mut()
                .rev()
                .find(|it| it.name == *name)
                .ok_or_else(|| format!("unknown impl owner {name}"))?;
            owner.impls.push((std::mem::take(&mut attrs), text));
            continue;
        }
        let (kind, name) = if words.starts_with(&["windows_link", "::", "link", "!"]) {
            (
                "fn",
                words
                    .iter()
                    .position(|w| *w == "fn")
                    .and_then(|j| words.get(j + 1))
                    .copied()
                    .ok_or("import lacks function")?,
            )
        } else if words.starts_with(&["pub", "const", "fn"]) {
            ("constfn", *words.get(3).ok_or("const fn lacks name")?)
        } else if words.starts_with(&["pub", "use"]) {
            (
                "use",
                *words
                    .iter()
                    .position(|w| *w == "as")
                    .and_then(|j| words.get(j + 1))
                    .ok_or_else(|| {
                        format!("{}:{} use lacks alias: {text}", path.display(), start + 1)
                    })?,
            )
        } else if words.first() == Some(&"macro_rules") {
            ("macro", *words.get(2).ok_or("macro lacks name")?)
        } else if words.first() == Some(&"pub")
            && ["const", "type", "struct", "union"].contains(words.get(1).unwrap_or(&""))
        {
            (words[1], *words.get(2).ok_or("item lacks name")?)
        } else {
            return Err(format!(
                "{}:{} unrecognized upstream item {line}",
                path.display(),
                start + 1
            ));
        };
        items.push(Item::new(
            module.to_vec(),
            name.to_string(),
            kind.to_string(),
            std::mem::take(&mut attrs),
            text,
            start,
        )?);
    }
    Ok(items)
}
impl Upstream {
    pub fn load(root: &Path) -> Result<Self> {
        let manifest: aterm_toml::Value =
            aterm_toml::from_str(&read(&root.join("Cargo.toml"))?).map_err(|e| e.to_string())?;
        let mut features = BTreeMap::new();
        for (k, v) in manifest
            .get("features")
            .and_then(aterm_toml::Value::as_table)
            .ok_or("upstream lacks features")?
        {
            features.insert(
                k.clone(),
                v.as_array()
                    .ok_or("feature isn't an array")?
                    .iter()
                    .map(|s| {
                        s.as_str()
                            .map(str::to_string)
                            .ok_or_else(|| "feature edge isn't a string".to_string())
                    })
                    .collect::<Result<Vec<_>>>()?,
            );
        }
        let mut up = Self {
            root: root.to_path_buf(),
            modules: BTreeMap::new(),
            items: BTreeMap::new(),
            features,
        };
        let core = vec!["core".to_string()];
        let mut items = parse_file(&root.join("src/core/mod.rs"), &core)?;
        let mut literals = parse_file(&root.join("src/core/literals.rs"), &core)?;
        for it in &mut literals {
            it.order += 10000
        }
        items.extend(literals);
        up.modules.insert(
            core,
            Module {
                file: root.join("src/core/mod.rs"),
                feature: None,
                children: vec![],
                items,
            },
        );
        for (name, feature) in children(&read(&root.join("src/Windows/mod.rs"))?) {
            up.load_module(
                vec![name.clone()],
                feature,
                root.join("src/Windows").join(name).join("mod.rs"),
            )?
        }
        for m in up.modules.values() {
            for it in &m.items {
                up.items.entry(it.key()).or_default().push(it.clone());
            }
        }
        Ok(up)
    }
    fn load_module(&mut self, path: Vec<String>, feature: String, file: PathBuf) -> Result<()> {
        let kids = children(&read(&file)?);
        let items = parse_file(&file, &path)?;
        self.modules.insert(
            path.clone(),
            Module {
                file: file.clone(),
                feature: Some(feature),
                children: kids.clone(),
                items,
            },
        );
        for (kid, f) in kids {
            let mut p = path.clone();
            p.push(kid.clone());
            self.load_module(
                p,
                f,
                file.parent()
                    .ok_or("module has no parent")?
                    .join(kid)
                    .join("mod.rs"),
            )?
        }
        Ok(())
    }
    pub fn implied(&self, features: &BTreeSet<String>) -> Result<BTreeSet<String>> {
        let mut out = BTreeSet::new();
        let mut todo: Vec<_> = features.iter().cloned().collect();
        while let Some(f) = todo.pop() {
            if !out.insert(f.clone()) {
                continue;
            }
            if f != "default" && !self.features.contains_key(&f) {
                return Err(format!("upstream lacks feature {f}"));
            }
            todo.extend(self.features.get(&f).into_iter().flatten().cloned());
        }
        Ok(out)
    }
    pub fn variants(&self, key: &Key, enabled: &BTreeSet<String>) -> Vec<&Item> {
        let mut path = key.0.clone();
        while !path.is_empty() {
            let Some(m) = self.modules.get(&path) else {
                return vec![];
            };
            if m.feature.as_ref().is_some_and(|f| !enabled.contains(f)) {
                return vec![];
            }
            path.pop();
        }
        self.items
            .get(key)
            .into_iter()
            .flatten()
            .filter(|it| it.features.is_subset(enabled))
            .collect()
    }
    pub fn resolve(&self, base: &[String], segments: &[String]) -> Option<Key> {
        if segments.is_empty() {
            return None;
        }
        let mut segs = segments.to_vec();
        let mut cur = base.to_vec();
        if ["windows_sys", "$crate", "crate"].contains(&segs[0].as_str()) {
            cur.clear();
            segs.remove(0);
        } else if segs[0] == "super" {
            while segs.first().is_some_and(|s| s == "super") {
                cur.pop();
                segs.remove(0);
            }
        } else if segs[0] == "core" && base != ["core"] {
            return None;
        }
        while segs.len() > 1
            && self
                .modules
                .contains_key(&[cur.clone(), vec![segs[0].clone()]].concat())
        {
            cur.push(segs.remove(0));
        }
        if segs.is_empty() {
            return Some((cur, String::new()));
        }
        if segs.len() == 1
            && !self.items.contains_key(&(cur.clone(), segs[0].clone()))
            && self
                .modules
                .contains_key(&[cur.clone(), segs.clone()].concat())
        {
            cur.extend(segs);
            return Some((cur, String::new()));
        }
        self.items
            .contains_key(&(cur.clone(), segs[0].clone()))
            .then(|| (cur, segs[0].clone()))
    }
    pub fn refs(&self, item: &Item) -> BTreeSet<Key> {
        let mut out = BTreeSet::new();
        for text in std::iter::once(&item.text).chain(item.impls.iter().map(|(_, t)| t)) {
            for segs in paths(text) {
                let key = if segs.len() == 1 {
                    Some((item.module.clone(), segs[0].clone()))
                } else {
                    self.resolve(&item.module, &segs)
                };
                if let Some(key) = key
                    && key != item.key()
                    && self.items.contains_key(&key)
                {
                    out.insert(key);
                }
            }
        }
        if item.kind == "use" {
            let ts = tokens(&item.text, false);
            out.insert((item.module.clone(), ts[2].text.clone()));
        }
        out
    }
}
pub fn paths(text: &str) -> Vec<Vec<String>> {
    let ts = tokens(text, false);
    paths_tokens(&ts)
}
pub fn paths_tokens(ts: &[Token]) -> Vec<Vec<String>> {
    let mut out = vec![];
    let mut i = 0;
    while i < ts.len() {
        if ts[i]
            .text
            .as_bytes()
            .first()
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_' || *b == b'$')
        {
            let mut p = vec![ts[i].text.clone()];
            i += 1;
            while i + 1 < ts.len()
                && ts[i].text == "::"
                && ts[i + 1]
                    .text
                    .as_bytes()
                    .first()
                    .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
            {
                p.push(ts[i + 1].text.clone());
                i += 2
            }
            out.push(p);
        } else {
            i += 1
        }
    }
    out
}

pub fn fields(it: &Item) -> Result<Vec<(String, String)>> {
    let body = it
        .text
        .split_once('{')
        .and_then(|(_, s)| s.rsplit_once('}').map(|(s, _)| s))
        .ok_or("record lacks braces")?;
    body.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let l = l.trim().strip_prefix("pub ").ok_or("nonpublic field")?;
            let (name, ty) = l.split_once(':').ok_or("field lacks type")?;
            Ok((
                name.to_string(),
                ty.trim()
                    .strip_suffix(',')
                    .ok_or("field lacks trailing comma")?
                    .to_string(),
            ))
        })
        .collect()
}
pub fn alias(it: &Item) -> Result<&str> {
    it.text
        .split_once('=')
        .and_then(|(_, s)| s.trim().strip_suffix(';'))
        .ok_or_else(|| format!("{} is not an alias", it.name))
}
pub fn constant(it: &Item) -> Result<(&str, &str)> {
    let (_, s) = it.text.split_once(':').ok_or("constant lacks type")?;
    let (ty, value) = s.split_once('=').ok_or("constant lacks value")?;
    Ok((
        ty.trim(),
        value
            .trim()
            .strip_suffix(';')
            .ok_or("constant lacks semicolon")?,
    ))
}
#[derive(Debug)]
pub struct Function {
    pub abi: String,
    pub params: Vec<(String, String)>,
    pub ret: String,
}
pub fn function(it: &Item) -> Result<Function> {
    let ts = tokens(&it.text, true);
    let f = ts
        .iter()
        .position(|t| t.text == "fn")
        .ok_or("function lacks fn")?;
    let literals: Vec<_> = ts[..f]
        .iter()
        .filter_map(|t| t.text.strip_prefix('"').and_then(|s| s.strip_suffix('"')))
        .collect();
    if literals.len() < 2 {
        return Err("import lacks DLL/ABI".into());
    }
    let open = ts.get(f + 2).ok_or("missing function args")?;
    let mut depth = 1;
    let mut close = f + 3;
    while close < ts.len() {
        match ts[close].text.as_str() {
            "(" => depth += 1,
            ")" => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
        close += 1
    }
    let args = &it.text[open.end..ts.get(close).ok_or("unclosed args")?.start];
    let mut params = vec![];
    for p in split_top(args, ",") {
        if p == "..." {
            params.push((p, String::new()));
        } else {
            let (n, t) = p.split_once(':').ok_or("arg lacks type")?;
            params.push((n.trim().to_string(), t.trim().to_string()));
        }
    }
    let rest = it.text[ts[close].end..].trim();
    let ret = rest
        .strip_prefix("->")
        .map(|s| s.trim().strip_suffix(");").unwrap_or(s).trim().to_string())
        .unwrap_or("()".into());
    Ok(Function {
        abi: literals[1].into(),
        params,
        ret,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_source_shapes_keep_gates_aliases_and_nested_types() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let file = dir.path().join("mod.rs");
        std::fs::write(&file, r#"
#[cfg(feature = "Win32_Foundation")]
windows_link::link!("test.dll" "system" fn Query(value : *const ITEM, done : Option<unsafe extern "system" fn(*mut core::ffi::c_void, u32)>) -> i32);
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ITEM {
    pub callback: Option<unsafe extern "system" fn(u32, *mut u8)>,
    pub bytes: [u8; 4],
}
impl Default for ITEM {
    fn default() -> Self { unsafe { core::mem::zeroed() } }
}
pub use Query as QueryAlias;
#[macro_export]
macro_rules! literal { ($s:literal) => { $s }; }
pub use literal;
"#).unwrap();
        let items = parse_file(&file, &["Win32".into()]).unwrap();
        assert_eq!(items.len(), 4);
        assert!(items[0].features.contains("Win32_Foundation"));
        let query = function(&items[0]).unwrap();
        assert_eq!(query.abi, "system");
        assert_eq!(query.params.len(), 2);
        assert!(query.params[1].1.ends_with("c_void, u32)>"));
        assert_eq!(query.ret, "i32");
        assert_eq!(fields(&items[1]).unwrap().len(), 2);
        assert_eq!(items[1].impls.len(), 1);
        assert_eq!(
            (items[2].kind.as_str(), items[2].name.as_str()),
            ("use", "QueryAlias")
        );
        assert_eq!(items[3].kind, "macro");
        std::fs::write(&file, "pub enum Unknown { A }\n").unwrap();
        assert!(
            parse_file(&file, &[]).is_err(),
            "unknown declaration forms refuse generation"
        );
    }
}
