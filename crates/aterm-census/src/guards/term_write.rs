// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! term-write: a host writes into a session's terminal only under a `TermGuard`.

use super::{Bound, Finding, Rule, Tree, has, hits, is_word};

const GUI: &str = "crates/aterm-gui/src/";

const GUARD_HOME: &str = "crates/aterm-gui/src/lib.rs";

const LOCKS: &[(&str, bool)] = &[
    ("lock", false),
    ("try_lock", false),
    ("lock_or_recover", true),
];

const UNWRAPS: &[&str] = &["unwrap", "expect", "unwrap_or_else", "into_inner"];

pub(super) const TERM_WRITE: Rule = Rule {
    id: "term-write",
    why: "a host's write into a session's terminal rings its screen, cursor and cells \
          subscribers only as a TermGuard hold ends (term_lock, term_try_lock, \
          term_lock_ui); a write through a raw guard is pushed to nobody until something \
          else rings",
    scope: &[GUI, GUARD_HOME],
    check: term_write,
};

fn term_write(tree: &Tree<'_>, out: &mut Vec<Finding>) {
    for file in tree.corpus.under(&[GUI]) {
        if tree.corpus.test_code().holds(&file.rel) || !file.mentions(&["term", "MutexGuard"]) {
            continue;
        }
        let code = file.shipped_code();
        let mut found = writes(&code);
        if file.rel != GUARD_HOME {
            found.extend(raw_guard_types(&code));
        }
        found.sort_unstable();
        let mut last = 0;
        for (at, what) in found {
            let no = code[..at].matches('\n').count() + 1;
            if no != last {
                out.push(file.finding(TERM_WRITE.id, no, what.to_string()));
                last = no;
            }
        }
    }
}

fn writes(code: &str) -> Vec<(usize, &'static str)> {
    let b = code.as_bytes();
    let mut out = Vec::new();
    for &(method, bare) in LOCKS {
        for at in hits(code, method, Bound::Word) {
            let dot = ws_back(b, at);
            if dot == 0 || b[dot - 1] != b'.' {
                continue;
            }
            let recv_end = ws_back(b, dot - 1);
            let recv = word_start(b, recv_end);
            if &code[recv..recv_end] != "term" {
                continue;
            }
            let open = ws_fwd(b, at + method.len());
            let close = ws_fwd(b, open + 1);
            if b.get(open) != Some(&b'(') || b.get(close) != Some(&b')') {
                continue;
            }
            let start = receiver_start(b, recv);
            match chain(code, close + 1, bare) {
                Chain::Writes(at, what) => out.push((at, what)),
                Chain::Read => {}
                Chain::Flows(end) => {
                    if let Some(what) = flows_into(code, start, end) {
                        out.push((start, what));
                    }
                }
            }
        }
    }
    out
}

enum Chain {
    Writes(usize, &'static str),
    Read,
    Flows(usize),
}

fn chain(code: &str, mut from: usize, bare: bool) -> Chain {
    let b = code.as_bytes();
    let mut guard = bare;
    loop {
        let at = ws_fwd(b, from);
        match b.get(at) {
            Some(b'?') => {
                guard = true;
                from = at + 1;
                continue;
            }
            Some(b'.') => {}
            _ => return Chain::Flows(from),
        }
        let name = ws_fwd(b, at + 1);
        let name_end = word_end(b, name);
        let open = ws_fwd(b, name_end);
        if name_end == name || b.get(open) != Some(&b'(') {
            return Chain::Flows(from);
        }
        let Some(close) = closer(b, open) else {
            return Chain::Flows(from);
        };
        let method = &code[name..name_end];
        if UNWRAPS.contains(&method) {
            guard = true;
        } else if method == "ok" {
            guard = false;
        } else if guard {
            return Chain::Writes(name, "calls a method on the raw guard itself");
        } else if closure_takes_mut(&code[open + 1..close]) {
            return Chain::Writes(name, "hands the raw guard to a closure that takes it `mut`");
        } else {
            return Chain::Read;
        }
        from = close + 1;
    }
}

fn flows_into(code: &str, start: usize, end: usize) -> Option<&'static str> {
    let b = code.as_bytes();
    let after = ws_fwd(b, end);
    if b.get(after) == Some(&b'=') && !matches!(b.get(after + 1), Some(b'=' | b'>')) {
        return Some("assigns through the raw guard");
    }
    let mut lead = ws_back(b, start);
    while lead > 0 && b[lead - 1] == b'*' {
        lead = ws_back(b, lead - 1);
    }
    if ends_with_word(code, lead, "mut") && b[..ws_back(b, lead - 3)].ends_with(b"&") {
        return Some("borrows the raw guard `&mut`");
    }
    while lead > 0 && matches!(b[lead - 1], b'&' | b'(') {
        lead = ws_back(b, lead - 1);
    }
    if ends_with_word(code, lead, "match") {
        if let Some(arm) = arm_binding_mut(code, ws_fwd(b, end)) {
            return Some(arm);
        }
        lead = ws_back(b, lead - "match".len());
    }
    let eq = lead.checked_sub(1)?;
    if b[eq] != b'=' || eq == 0 || b"=!<>+-*/%^&|".contains(&b[eq - 1]) {
        return None;
    }
    let head = code[..eq].rfind([';', '{', '}']).map_or(0, |at| at + 1);
    let pattern = code[head..eq].split_once("let ")?.1;
    binds_mut(pattern).then_some("binds the raw guard `mut`")
}

fn arm_binding_mut(code: &str, open: usize) -> Option<&'static str> {
    let b = code.as_bytes();
    if b.get(open) != Some(&b'{') {
        return None;
    }
    let close = closer(b, open)?;
    let mut depth = 0usize;
    let mut in_pattern = true;
    let mut pattern = open + 1;
    let mut i = open + 1;
    while i < close {
        match b[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' => depth = depth.saturating_sub(1),
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 && !in_pattern && !word_at(code, ws_fwd(b, i + 1), "else") {
                    in_pattern = true;
                    pattern = i + 1;
                }
            }
            b',' if depth == 0 && !in_pattern => {
                in_pattern = true;
                pattern = i + 1;
            }
            b'=' if depth == 0 && in_pattern && b.get(i + 1) == Some(&b'>') => {
                if binds_mut(&code[pattern..i]) {
                    return Some("matches the raw guard with an arm that binds it `mut`");
                }
                in_pattern = false;
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn closure_takes_mut(args: &str) -> bool {
    let b = args.as_bytes();
    let mut i = 0;
    while let Some(rel) = args[i..].find('|') {
        let bar = i + rel;
        let before = ws_back(b, bar);
        let opens = before == 0 || matches!(b[before - 1], b'(' | b',') || {
            ends_with_word(args, before, "move")
        };
        let Some(len) = args[bar + 1..].find('|') else {
            return false;
        };
        if opens && binds_mut(&args[bar + 1..bar + 1 + len]) {
            return true;
        }
        i = bar + 1 + len + 1;
    }
    false
}

fn binds_mut(pattern: &str) -> bool {
    let b = pattern.as_bytes();
    hits(pattern, "mut", Bound::Word).any(|at| !b[..ws_back(b, at)].ends_with(b"&"))
}

fn raw_guard_types(code: &str) -> Vec<(usize, &'static str)> {
    let b = code.as_bytes();
    hits(code, "MutexGuard", Bound::Word)
        .filter(|&at| {
            let open = ws_fwd(b, at + "MutexGuard".len());
            b.get(open) == Some(&b'<')
                && angle_closer(b, open)
                    .is_some_and(|close| has(&code[open..close], "Terminal", Bound::Word))
        })
        .map(|at| {
            (
                at,
                "names a raw guard of a Terminal outside lib.rs's TermGuard",
            )
        })
        .collect()
}

fn receiver_start(b: &[u8], mut start: usize) -> usize {
    loop {
        let dot = ws_back(b, start);
        if dot == 0 || b[dot - 1] != b'.' || (dot >= 2 && b[dot - 2] == b'.') {
            return start;
        }
        let seg_end = ws_back(b, dot - 1);
        let mut at = seg_end;
        while at > 0 {
            match b[at - 1] {
                b'?' => at -= 1,
                b')' | b']' => match opener(b, at - 1) {
                    Some(open) => at = open,
                    None => return start,
                },
                _ => break,
            }
        }
        loop {
            at = word_start(b, at);
            if at >= 2 && &b[at - 2..at] == b"::" {
                at -= 2;
            } else {
                break;
            }
        }
        if at == seg_end {
            return start;
        }
        start = at;
    }
}

const fn ws_back(b: &[u8], mut at: usize) -> usize {
    while at > 0 && b[at - 1].is_ascii_whitespace() {
        at -= 1;
    }
    at
}

fn ws_fwd(b: &[u8], mut at: usize) -> usize {
    while b.get(at).is_some_and(u8::is_ascii_whitespace) {
        at += 1;
    }
    at
}

const fn word_start(b: &[u8], mut at: usize) -> usize {
    while at > 0 && is_word(b[at - 1]) {
        at -= 1;
    }
    at
}

fn word_end(b: &[u8], mut at: usize) -> usize {
    while b.get(at).is_some_and(|&c| is_word(c)) {
        at += 1;
    }
    at
}

fn ends_with_word(code: &str, end: usize, word: &str) -> bool {
    code[..end].ends_with(word) && word_start(code.as_bytes(), end) == end - word.len()
}

fn word_at(code: &str, at: usize, word: &str) -> bool {
    code[at..].starts_with(word) && word_end(code.as_bytes(), at) == at + word.len()
}

fn closer(b: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, &c) in b.iter().enumerate().skip(open) {
        match c {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn opener(b: &[u8], close: usize) -> Option<usize> {
    let mut depth = 0usize;
    for i in (0..=close).rev() {
        match b[i] {
            b')' | b']' | b'}' => depth += 1,
            b'(' | b'[' | b'{' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn angle_closer(b: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, &c) in b.iter().enumerate().skip(open) {
        match c {
            b'<' => depth += 1,
            b'>' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            b';' | b'{' => return None,
            _ => {}
        }
    }
    None
}

#[cfg(test)]
#[path = "term_write_tests.rs"]
mod tests;
