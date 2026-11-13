// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A bounded launchd selector, refusing XML forms it does not understand.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

enum Value {
    Text(String),
    Strings(Vec<String>),
    Other,
}

/// Only top-level Program/ProgramArguments select an executable. Unknown syntax retains
/// the artifact instead of approximating a different launchd interpretation.
pub(super) fn program(text: &str) -> Result<PathBuf, &'static str> {
    let mut xml = Xml {
        rest: text,
        fields: 0,
    };
    xml.take("<?xml version=\"1.0\" encoding=\"UTF-8\"?>");
    xml.take("<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">");
    if !xml.take("<plist>") && !xml.take("<plist version=\"1.0\">") {
        return Err("unsupported plist wrapper");
    }
    if !xml.take("<dict>") {
        return Err("service plist is not a dictionary");
    }
    let fields = xml.dictionary(false)?;
    if !xml.take("</plist>") || !xml.space().is_empty() {
        return Err("incomplete or unsupported plist suffix");
    }
    if fields
        .get("ProgramArguments")
        .is_some_and(|value| !matches!(value, Value::Strings(_)))
    {
        return Err("ProgramArguments is not a string array");
    }
    // chdir does not change an absolute executable selection. Keep unsupported
    // directory forms unknown rather than guessing how launchd interprets them.
    if let Some(directory) = fields.get("WorkingDirectory") {
        match directory {
            Value::Text(directory) if Path::new(directory).is_absolute() => {}
            _ => return Err("WorkingDirectory is not an absolute string"),
        }
    }
    let selected = match fields.get("Program") {
        Some(Value::Text(program)) => program,
        Some(_) => return Err("Program is not a string"),
        None => match fields.get("ProgramArguments") {
            Some(Value::Strings(arguments)) => arguments.first().ok_or("empty ProgramArguments")?,
            _ => return Err("no supported executable selection"),
        },
    };
    if !Path::new(selected).is_absolute() {
        return Err("executable is not absolute");
    }
    Ok(PathBuf::from(selected))
}

struct Xml<'a> {
    rest: &'a str,
    fields: usize,
}

impl Xml<'_> {
    fn space(&mut self) -> &str {
        self.rest = self.rest.trim_start_matches([' ', '\t', '\r', '\n']);
        self.rest
    }

    fn take(&mut self, literal: &str) -> bool {
        self.space();
        if let Some(rest) = self.rest.strip_prefix(literal) {
            self.rest = rest;
            true
        } else {
            false
        }
    }

    fn text(&mut self, tag: &str) -> Result<String, &'static str> {
        let (open, close) = match tag {
            "key" => ("<key>", "</key>"),
            "string" => ("<string>", "</string>"),
            "integer" => ("<integer>", "</integer>"),
            "real" => ("<real>", "</real>"),
            "data" => ("<data>", "</data>"),
            "date" => ("<date>", "</date>"),
            _ => return Err("unsupported value or XML syntax"),
        };
        if !self.take(open) {
            return Err("unsupported value or XML syntax");
        }
        let (text, rest) = self.rest.split_once(close).ok_or("incomplete scalar")?;
        self.rest = rest;
        if text.contains('<') || text.chars().any(char::is_control) {
            return Err("unsupported scalar text");
        }
        let Some(first) = text.find('&') else {
            return Ok(text.to_owned());
        };
        let mut output = String::with_capacity(text.len());
        output.push_str(&text[..first]);
        let mut tail = &text[first..];
        loop {
            let (entity, decoded) = [
                ("&amp;", '&'),
                ("&lt;", '<'),
                ("&gt;", '>'),
                ("&quot;", '"'),
                ("&apos;", '\''),
            ]
            .into_iter()
            .find(|(entity, _)| tail.starts_with(entity))
            .ok_or("unsupported XML entity")?;
            output.push(decoded);
            tail = &tail[entity.len()..];
            let Some(at) = tail.find('&') else {
                output.push_str(tail);
                break;
            };
            output.push_str(&tail[..at]);
            tail = &tail[at..];
        }
        Ok(output)
    }

    fn dictionary(&mut self, nested: bool) -> Result<BTreeMap<String, Value>, &'static str> {
        let mut fields = BTreeMap::new();
        while !self.take("</dict>") {
            self.fields += 1;
            if self.fields > 1024 {
                return Err("service plist exceeds the field bound");
            }
            let key = self.text("key")?;
            if nested
                && matches!(
                    key.as_str(),
                    "Program" | "ProgramArguments" | "BundleProgram"
                )
            {
                return Err("nested executable selection is ambiguous");
            }
            // These are the ordinary producer's fields plus explicit Program. An unknown
            // launchd key may change path interpretation (RootDirectory, BundleProgram,
            // ...), so unsupported semantics retain instead of being silently ignored.
            if !nested
                && !matches!(
                    key.as_str(),
                    "Label"
                        | "Program"
                        | "ProgramArguments"
                        | "WorkingDirectory"
                        | "LimitLoadToSessionType"
                        | "AssociatedBundleIdentifiers"
                        | "RunAtLoad"
                        | "KeepAlive"
                        | "ProcessType"
                        | "Nice"
                        | "SoftResourceLimits"
                        | "EnvironmentVariables"
                        | "StandardOutPath"
                        | "StandardErrorPath"
                )
            {
                return Err("unsupported launchd key");
            }
            let value = self.value(nested)?;
            if fields.insert(key, value).is_some() {
                return Err("duplicate service key");
            }
        }
        Ok(fields)
    }

    fn value(&mut self, nested: bool) -> Result<Value, &'static str> {
        if self.space().starts_with("<string>") {
            return self.text("string").map(Value::Text);
        }
        if self.take("<string/>") {
            return Ok(Value::Text(String::new()));
        }
        if self.take("<true/>") || self.take("<false/>") {
            return Ok(Value::Other);
        }
        if self.space().starts_with("<integer>") {
            let text = self.text("integer")?;
            if text.starts_with('+') {
                return Err("unsupported integer");
            }
            text.parse::<i64>().map_err(|_| "unsupported integer")?;
            return Ok(Value::Other);
        }
        if !nested && self.take("<array>") {
            let mut strings = Vec::new();
            while !self.take("</array>") {
                self.fields += 1;
                if self.fields > 1024 {
                    return Err("service plist exceeds the field bound");
                }
                strings.push(self.text("string")?);
            }
            return Ok(Value::Strings(strings));
        }
        if !nested && self.take("<dict>") {
            self.dictionary(true)?;
            return Ok(Value::Other);
        }
        Err("unsupported nested value or XML syntax")
    }
}

#[cfg(test)]
#[path = "artifact_service_tests.rs"]
mod tests;
