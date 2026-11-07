// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Failed Cargo builds keep compiler errors without dumping their JSON stream.

use std::path::Path;

use agh::redact::Redactor;

use super::super::tools::{Ran, Run};

const INPUT_BYTES: usize = 4 * 1024 * 1024;
const ERROR_BYTES: usize = 16 * 1024;
const STDERR_BYTES: usize = 4 * 1024;
const ERROR_COUNT: usize = 8;
const CUT: &str = "\n[diagnostic truncated]\n";

fn bounded(text: String, limit: usize) -> String {
    if text.len() <= limit {
        return text;
    }
    let room = limit - CUT.len();
    let mut head = room / 4;
    let mut tail = text.len() - (room - head);
    while !text.is_char_boundary(head) {
        head -= 1;
    }
    while !text.is_char_boundary(tail) {
        tail += 1;
    }
    format!("{}{CUT}{}", &text[..head], &text[tail..])
}

fn secret_key(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    [
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "CREDENTIAL",
        "API_KEY",
        "ACCESS_KEY",
        "PRIVATE_KEY",
    ]
    .iter()
    .any(|word| name.contains(word))
}

fn redactor(run: &Run) -> Redactor {
    let redactor = Redactor::new();
    if !run.clear_env {
        for (name, value) in std::env::vars_os() {
            if secret_key(&name.to_string_lossy()) {
                redactor.learn(value.to_string_lossy().as_bytes());
            }
        }
    }
    for (name, value) in &run.env {
        if secret_key(name) {
            redactor.learn(value.to_string_lossy().as_bytes());
        }
    }
    redactor
}

fn sanitized(text: &str, redactor: &Redactor) -> String {
    // Redact before either display cut, including a credential straddling one.
    atpkg::packages_log::sanitize_capped(&redactor.apply(text), usize::MAX)
}

pub(super) struct Failure {
    compiler: String,
    stderr: String,
}

impl Failure {
    pub(super) fn capture(run: &Run, ran: &Ran) -> Self {
        let redactor = redactor(run);
        let mut errors = Vec::new();
        let mut consumed = 0usize;
        let mut malformed = false;
        let mut omitted = false;
        for line in ran.stdout.rsplit(|byte| *byte == b'\n') {
            if line.is_empty() {
                continue;
            }
            consumed = consumed.saturating_add(line.len());
            if consumed > INPUT_BYTES || errors.len() == ERROR_COUNT {
                omitted = true;
                break;
            }
            let Ok(value) = aterm_json::from_slice::<aterm_json::Value>(line) else {
                malformed = true;
                continue;
            };
            if value.get("reason").and_then(aterm_json::Value::as_str) != Some("compiler-message") {
                continue;
            }
            let Some(message) = value.get("message") else {
                malformed = true;
                continue;
            };
            let Some(level) = message.get("level").and_then(aterm_json::Value::as_str) else {
                malformed = true;
                continue;
            };
            if level != "error" {
                continue;
            }
            let rendered = match message.get("rendered") {
                Some(aterm_json::Value::String(text)) => Some(text.as_str()),
                None | Some(aterm_json::Value::Null) => {
                    message.get("message").and_then(aterm_json::Value::as_str)
                }
                _ => None,
            };
            match rendered.filter(|text| !text.trim().is_empty()) {
                Some(text) => errors.push(sanitized(text, &redactor)),
                None => malformed = true,
            }
        }
        errors.reverse();
        if malformed {
            errors.push("[malformed Cargo diagnostic omitted]".into());
        }
        if omitted {
            errors.push("[additional Cargo diagnostics omitted by the input/count bound]".into());
        }
        Self {
            compiler: bounded(errors.join("\n\n"), ERROR_BYTES),
            stderr: bounded(
                sanitized(&String::from_utf8_lossy(&ran.stderr), &redactor),
                STDERR_BYTES,
            ),
        }
    }

    pub(super) fn refusal(&self, stage: &Path) -> String {
        let compiler = stage.join("build.compiler-errors");
        let stderr = stage.join("build.stderr");
        let saved = write(&compiler, &self.compiler).and_then(|()| write(&stderr, &self.stderr));
        let detail = match saved {
            Ok(()) => format!("see {} and {}", compiler.display(), stderr.display()),
            Err(error) => format!("could not retain build diagnostics: {error}"),
        };
        format!(
            "the build FAILED ({detail}):\ncompiler errors:\n{}\nstderr:\n{}",
            self.compiler, self.stderr
        )
    }
}

fn write(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let _ = std::fs::remove_file(path);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    options.open(path)?.write_all(text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_credentials_and_overbound_records_never_reach_diagnostics() {
        let secret = concat!("opaque-private-", "build-credential-", "123456789");
        let mut run = Run::new("fixture", ["build"]);
        run.clear_env = true;
        run.env
            .push(("FIXTURE_PRIVATE_TOKEN".into(), secret.into()));
        let message = aterm_json::json!({
            "reason": "compiler-message",
            "message": {"level": "error", "rendered": format!("error: {secret}")}
        });
        let mut output = Ran {
            code: Some(101),
            stdout: message.to_string().into_bytes(),
            stderr: format!("stderr echoed {secret}").into_bytes(),
        };
        let failure = Failure::capture(&run, &output);
        assert!(!failure.compiler.contains(secret));
        assert!(!failure.stderr.contains(secret));
        assert!(failure.compiler.contains("[redacted]"));

        output.stdout = vec![b'x'; INPUT_BYTES + 1];
        let failure = Failure::capture(&run, &output);
        assert!(failure.compiler.contains("input/count bound"));
        assert!(failure.compiler.len() <= ERROR_BYTES);

        output.stdout = (0..ERROR_COUNT + 3)
            .map(|index| {
                aterm_json::json!({
                    "reason": "compiler-message",
                    "message": {"level": "error", "rendered": format!("error: fixture-{index}")}
                })
                .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
            .into_bytes();
        let failure = Failure::capture(&run, &output);
        assert_eq!(
            failure.compiler.matches("error: fixture-").count(),
            ERROR_COUNT
        );
        assert!(failure.compiler.contains("input/count bound"));
    }
}
