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
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn the_ordinary_service_producer_remains_observable() {
        let root = aterm_tempfile::tempdir().unwrap();
        let executable = root.path().join("A&B <quoted>.app/Contents/MacOS/aterm");
        let text = aterm_secrets::service::plist_text(
            "systems.alab.aterm-node",
            &executable,
            &["node".into(), "serve".into()],
            &root.path().join("Library/Logs/aterm-secrets/service.log"),
        );
        assert_eq!(program(&text).unwrap(), executable);
    }

    #[test]
    fn a_working_directory_does_not_rebase_an_absolute_executable() {
        let wrap = |body: &str| format!("<plist><dict>{body}</dict></plist>");
        for selection in [
            "<key>Program</key><string>/artifact/bin/aterm</string>",
            "<key>ProgramArguments</key><array><string>/artifact/bin/aterm</string><string>serve</string></array>",
        ] {
            let directory = "<key>WorkingDirectory</key><string>/another/directory</string>";
            assert_eq!(
                program(&wrap(&format!("{selection}{directory}"))).unwrap(),
                PathBuf::from("/artifact/bin/aterm")
            );
            for value in ["<true/>", "<string>relative</string>", "<string/>"] {
                assert!(
                    program(&wrap(&format!(
                        "{selection}<key>WorkingDirectory</key>{value}"
                    )))
                    .is_err()
                );
            }
        }
        assert!(program(&wrap(
            "<key>Program</key><string>relative</string><key>WorkingDirectory</key><string>/artifact</string>"
        ))
        .is_err());
    }

    #[test]
    fn ambiguous_or_unsupported_selections_are_unknown() {
        let executable = std::env::temp_dir().join("artifact-image");
        let escaped = aterm_codec::xml::escape(&executable.display().to_string());
        let selection = format!("<key>Program</key><string>{escaped}</string>");
        let wrap = |body: &str| format!("<plist><dict>{body}</dict></plist>");
        assert_eq!(program(&wrap(&selection)).unwrap(), executable);
        for body in [
            format!("{selection}{selection}"),
            format!("<!-- {selection} -->{selection}"),
            format!("{selection}<key>EnvironmentVariables</key><dict>{selection}</dict>"),
            format!("{selection}<key>RootDirectory</key><string>/other</string>"),
            format!("{selection}<key>BundleProgram</key><string>other</string>"),
            format!("{selection}<key>ProgramArguments</key><string>wrong type</string>"),
            "<key>Program</key><string>/bad&unknown;entity</string>".to_owned(),
        ] {
            assert!(
                program(&wrap(&body)).is_err(),
                "unsupported selection accepted"
            );
        }
    }
}
