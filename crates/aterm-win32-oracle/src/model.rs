// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//! Cargo's actual Windows consumers and the transitive declaration closure.
use crate::source::{Item, Key, Token, Upstream, ident};
use crate::{Result, TRIPLES, Tools, VERSION, read, run};
use aterm_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub struct Model {
    pub up: Upstream,
    pub consumers: BTreeMap<(String, String), PathBuf>,
    pub enabled: BTreeSet<String>,
    pub requested: BTreeSet<String>,
    pub seeds: BTreeMap<Key, BTreeSet<String>>,
    pub keep: BTreeSet<Key>,
    pub notes: Vec<String>,
}
fn metadata(root: &Path, tools: &Tools, triple: Option<&str>) -> Result<Value> {
    let mut args = vec!["metadata", "--format-version", "1", "--locked", "--offline"];
    if let Some(t) = triple {
        args.extend(["--filter-platform", t]);
    }
    let out = run(&tools.targo, &args, root, None)?;
    aterm_json::from_slice(&out.stdout).map_err(|e| e.to_string())
}
fn array<'a>(v: &'a Value, key: &str) -> Result<&'a Vec<Value>> {
    v.get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("metadata lacks {key}"))
}
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("metadata lacks string {key}"))
}
fn packages(meta: &Value) -> Result<BTreeMap<String, &Value>> {
    array(meta, "packages")?
        .iter()
        .map(|p| Ok((text(p, "id")?.to_string(), p)))
        .collect()
}
fn nodes(meta: &Value) -> Result<&Vec<Value>> {
    array(
        meta.get("resolve").ok_or("metadata lacks resolve")?,
        "nodes",
    )
}
fn is_target(p: &Value) -> bool {
    p.get("name").and_then(Value::as_str) == Some("windows-sys")
        && p.get("version")
            .and_then(Value::as_str)
            .is_some_and(|v| v.starts_with("0.61."))
}
impl Model {
    pub fn load(root: &Path, upstream: &Path, tools: &Tools) -> Result<Self> {
        let up = Upstream::load(upstream)?;
        let mut consumers = BTreeMap::new();
        let mut features = BTreeSet::new();
        for triple in TRIPLES {
            let meta = metadata(root, tools, Some(triple))?;
            let pkgs = packages(&meta)?;
            let targets: Vec<_> = pkgs
                .iter()
                .filter(|(_, p)| is_target(p))
                .map(|(id, _)| id)
                .collect();
            if targets.len() != 1 {
                return Err(format!(
                    "expected one windows-sys0.61 on {triple}, found {}",
                    targets.len()
                ));
            }
            let target = targets[0];
            for node in nodes(&meta)? {
                if array(node, "deps")?
                    .iter()
                    .any(|d| d.get("pkg").and_then(Value::as_str) == Some(target.as_str()))
                {
                    let p = pkgs
                        .get(text(node, "id")?)
                        .ok_or("resolve names absent package")?;
                    consumers.insert(
                        (
                            text(p, "name")?.to_string(),
                            text(p, "version")?.to_string(),
                        ),
                        Path::new(text(p, "manifest_path")?)
                            .parent()
                            .ok_or("manifest has no parent")?
                            .to_path_buf(),
                    );
                }
            }
            let spec = format!("windows-sys@{VERSION}");
            let out = run(
                &tools.targo,
                &[
                    "tree",
                    "--locked",
                    "--offline",
                    "--target",
                    triple,
                    "-e",
                    "normal,build,dev",
                    "-i",
                    &spec,
                    "--depth",
                    "0",
                    "-f",
                    "{f}",
                ],
                root,
                None,
            )?;
            features.extend(
                String::from_utf8(out.stdout)
                    .map_err(|e| e.to_string())?
                    .trim()
                    .split(',')
                    .filter(|f| !f.is_empty())
                    .map(str::to_string),
            );
        }
        let enabled = up.implied(&features)?;
        let meta = metadata(root, tools, None)?;
        let pkgs = packages(&meta)?;
        let mut requested = BTreeSet::new();
        for node in nodes(&meta)? {
            let p = pkgs
                .get(text(node, "id")?)
                .ok_or("resolve names absent package")?;
            if is_target(p) {
                for f in array(node, "features")? {
                    requested.insert(f.as_str().ok_or("nonstring feature")?.to_string());
                }
            }
        }
        if requested.is_empty() {
            return Err("lock resolves no windows-sys0.61 features".into());
        }
        let mut seeds = BTreeMap::new();
        let mut notes = vec![];
        for ((name, _), dir) in &consumers {
            for file in rust_files(dir)? {
                harvest_file(&up, &file, name, &enabled, &mut seeds, &mut notes)?
            }
        }
        let keep = closure(&up, seeds.keys().cloned().collect(), &enabled)?;
        Ok(Self {
            up,
            consumers,
            enabled,
            requested,
            seeds,
            keep,
            notes,
        })
    }
    pub fn variants(&self) -> Vec<&Item> {
        let mut out: Vec<_> = self
            .keep
            .iter()
            .flat_map(|k| self.up.variants(k, &self.enabled))
            .collect();
        out.sort_by(|a, b| (&a.module, a.order).cmp(&(&b.module, b.order)));
        out
    }
    pub fn summary(&self) -> String {
        let mut kinds = BTreeMap::<&str, usize>::new();
        for it in self.variants() {
            *kinds.entry(&it.kind).or_default() += 1
        }
        format!(
            "consumers: {:?}\nfeatures: {}; lock features: {}\nseeds: {}; closure: {} names, {} declarations ({kinds:?})",
            self.consumers.keys().collect::<Vec<_>>(),
            self.enabled.len(),
            self.requested.len(),
            self.seeds.len(),
            self.keep.len(),
            self.variants().len()
        )
    }
}
pub fn closure(
    up: &Upstream,
    seeds: BTreeSet<Key>,
    enabled: &BTreeSet<String>,
) -> Result<BTreeSet<Key>> {
    let mut keep = BTreeSet::new();
    let mut todo: Vec<_> = seeds.into_iter().collect();
    while let Some(key) = todo.pop() {
        if !keep.insert(key.clone()) {
            continue;
        }
        let variants = up.variants(&key, enabled);
        if variants.is_empty() {
            return Err(format!(
                "closure reaches a gated or absent item {}::{}",
                key.0.join("::"),
                key.1
            ));
        }
        for it in variants {
            todo.extend(up.refs(it));
        }
    }
    Ok(keep)
}
pub fn rust_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut todo = vec![root.to_path_buf()];
    let mut out = vec![];
    while let Some(dir) = todo.pop() {
        for e in std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let e = e.map_err(|e| e.to_string())?;
            let t = e.file_type().map_err(|e| e.to_string())?;
            if t.is_dir() && e.file_name() != "target" && e.file_name() != ".git" {
                todo.push(e.path())
            } else if t.is_file() && e.path().extension().is_some_and(|s| s == "rs") {
                out.push(e.path())
            }
        }
    }
    out.sort();
    Ok(out)
}
fn raw_tokens(src: &str) -> Vec<Token> {
    let b = src.as_bytes();
    let mut i = 0;
    let mut out = vec![];
    while i < b.len() {
        let start = i;
        if b[i].is_ascii_alphabetic() || b[i] == b'_' || b[i] == b'$' {
            i += 1;
            while i < b.len() && ident(b[i]) {
                i += 1
            }
        } else if b[i..].starts_with(b"::") {
            i += 2
        } else if b"{},*;".contains(&b[i]) {
            i += 1
        } else {
            i += 1;
            continue;
        }
        out.push(Token {
            text: src[start..i].to_string(),
            start,
            end: i,
        });
    }
    out
}
struct Harvest<'a> {
    up: &'a Upstream,
    file: &'a Path,
    source: &'a str,
    consumer: &'a str,
    enabled: &'a BTreeSet<String>,
    seeds: &'a mut BTreeMap<Key, BTreeSet<String>>,
    notes: &'a mut Vec<String>,
    aliases: BTreeMap<String, Vec<String>>,
}
impl Harvest<'_> {
    fn seed(
        &mut self,
        base: &[String],
        segs: &[String],
        pos: usize,
    ) -> Result<Option<Vec<String>>> {
        let where_ = format!(
            "{}:{}",
            self.file.display(),
            self.source[..pos].bytes().filter(|b| *b == b'\n').count() + 1
        );
        let Some(key) = self.up.resolve(base, segs) else {
            self.notes.push(format!(
                "{where_}: {} names no upstream item",
                segs.join("::")
            ));
            return Ok(None);
        };
        if key.1.is_empty() {
            return Ok(Some(key.0));
        }
        if self.up.variants(&key, self.enabled).is_empty() {
            return Err(format!("{where_}: {} is gated off", segs.join("::")));
        }
        self.seeds
            .entry(key)
            .or_default()
            .insert(self.consumer.to_string());
        Ok(None)
    }
    fn use_tree(&mut self, ts: &[Token], mut i: usize, prefix: &[String]) -> Result<usize> {
        let mut path = prefix.to_vec();
        while i < ts.len() {
            let t = &ts[i].text;
            if t == "{" {
                i += 1;
                while i < ts.len() && ts[i].text != "}" {
                    i = self.use_tree(ts, i, &path)?;
                    if ts.get(i).is_some_and(|t| t.text == ",") {
                        i += 1
                    }
                }
                return Ok(i + 1);
            }
            if t == "*" {
                if let Some(m) = self.seed(&[], &path, ts[i].start)? {
                    self.notes.push(format!(
                        "{}: glob import {}",
                        self.file.display(),
                        m.join("::")
                    ))
                }
                return Ok(i + 1);
            }
            if t.as_bytes()
                .first()
                .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
            {
                if t != "self" {
                    path.push(t.clone())
                }
                i += 1;
                if ts.get(i).is_some_and(|t| t.text == "::") {
                    i += 1;
                    continue;
                }
                let mut alias = path.last().ok_or("empty use path")?.clone();
                if ts.get(i).is_some_and(|t| t.text == "as") {
                    alias = ts.get(i + 1).ok_or("use alias missing")?.text.clone();
                    i += 2
                }
                if let Some(m) = self.seed(&[], &path, ts[i - 1].start)? {
                    self.aliases.insert(alias, m);
                }
                return Ok(i);
            }
            return Ok(i + 1);
        }
        Ok(i)
    }
}
pub fn harvest_file(
    up: &Upstream,
    file: &Path,
    consumer: &str,
    enabled: &BTreeSet<String>,
    seeds: &mut BTreeMap<Key, BTreeSet<String>>,
    notes: &mut Vec<String>,
) -> Result<()> {
    let src = read(file)?;
    if !src.contains("windows_sys") {
        return Ok(());
    }
    let ts = raw_tokens(&src);
    let mut h = Harvest {
        up,
        file,
        source: &src,
        consumer,
        enabled,
        seeds,
        notes,
        aliases: BTreeMap::new(),
    };
    for (idx, t) in ts.iter().enumerate() {
        if t.text != "windows_sys" || !ts.get(idx + 1).is_some_and(|t| t.text == "::") {
            continue;
        }
        let back = if idx > 0 && ts[idx - 1].text == "::" {
            idx.checked_sub(2)
        } else {
            idx.checked_sub(1)
        };
        if back.is_some_and(|b| ts[b].text == "use") {
            h.use_tree(&ts, idx + 2, &["windows_sys".into()])?;
            continue;
        }
        let mut segs = vec!["windows_sys".to_string()];
        let mut j = idx + 1;
        while j + 1 < ts.len()
            && ts[j].text == "::"
            && ts[j + 1]
                .text
                .as_bytes()
                .first()
                .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        {
            segs.push(ts[j + 1].text.clone());
            j += 2
        }
        h.seed(&[], &segs, t.start)?;
    }
    for (alias, base) in h.aliases.clone() {
        for (idx, t) in ts.iter().enumerate() {
            if t.text != alias
                || !ts.get(idx + 1).is_some_and(|t| t.text == "::")
                || (idx > 0 && ts[idx - 1].text == "::")
            {
                continue;
            }
            let mut segs = vec![];
            let mut j = idx + 1;
            while j + 1 < ts.len()
                && ts[j].text == "::"
                && ts[j + 1]
                    .text
                    .as_bytes()
                    .first()
                    .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
            {
                segs.push(ts[j + 1].text.clone());
                j += 2
            }
            h.seed(&base, &segs, t.start)?;
        }
    }
    Ok(())
}
