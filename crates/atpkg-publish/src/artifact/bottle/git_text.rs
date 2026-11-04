// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Git's shell and Perl helpers must find their data after a store move, too.

use std::path::Path;

use crate::artifact::relocate;
use crate::error::{Error, Result};

/// Replace Git's own bottled prefix in script literals with its runtime package root.
/// Libraries in `share/perl5` use their own file location; shell libraries inherit the
/// wrapper's root or derive it from Git's exec path when sourced outside the wrapper.
///
/// # Errors
/// An unreadable helper, an unknown script language or an unsupported prefix literal.
pub(super) fn rewrite(stage: &Path, version: &str) -> Result<()> {
    let prefix = format!("@@HOMEBREW_CELLAR@@/git/{version}");
    for rel in relocate::walk_files(stage).map_err(Error::refused)? {
        let path = stage.join(&rel);
        let bytes = crate::identity::read_regular_bytes(&path, 64 * 1024 * 1024)
            .map_err(|error| Error::io(format!("read {}", path.display()), &error))?;
        if bytes.contains(&0)
            || !bytes
                .windows(prefix.len())
                .any(|part| part == prefix.as_bytes())
        {
            continue;
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| Error::refused(format!("non-UTF-8 Git helper {}", rel.display())))?;
        let perl = text.starts_with("#!/usr/bin/perl")
            || rel.extension().is_some_and(|extension| extension == "pm");
        if !perl
            && !text.starts_with("#!/bin/sh")
            && rel.file_name().is_none_or(|name| name != "git-sh-i18n")
        {
            return Err(Error::refused(format!(
                "unknown Git helper language: {}",
                rel.display()
            )));
        }
        let rendered = literals(text, &prefix, perl)?;
        let levels = rel.parent().map_or(0, |parent| parent.components().count());
        let parent = (0..levels).map(|_| "..").collect::<Vec<_>>().join("/");
        let setup = if perl {
            format!(
                "BEGIN {{ require Cwd; require File::Basename; $ENV{{ATERM_GIT_ROOT}} = Cwd::abs_path(File::Basename::dirname(__FILE__) . '/{parent}'); }}\n"
            )
        } else {
            ": \"${ATERM_GIT_ROOT:=$(CDPATH='' cd -- \"$(git --exec-path)/../..\" && pwd -P)}\"\nexport ATERM_GIT_ROOT\n".to_string()
        };
        let at = if rendered.starts_with("#!") {
            rendered.find('\n').map_or(rendered.len(), |at| at + 1)
        } else {
            0
        };
        let (head, body) = rendered.split_at(at);
        let mode = std::fs::metadata(&path)
            .map_err(|error| Error::io("stat Git helper", &error))?
            .permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
                .map_err(|error| Error::io("make Git helper writable", &error))?;
        }
        std::fs::write(&path, format!("{head}{setup}{body}"))
            .map_err(|error| Error::io("write Git helper", &error))?;
        std::fs::set_permissions(&path, mode)
            .map_err(|error| Error::io("restore Git helper mode", &error))?;
    }
    Ok(())
}

fn literals(text: &str, prefix: &str, perl: bool) -> Result<String> {
    let root = if perl {
        "$ENV{ATERM_GIT_ROOT}"
    } else {
        "${ATERM_GIT_ROOT}"
    };
    let mut result = String::new();
    for line in text.split_inclusive('\n') {
        if line.trim_start().starts_with('#') {
            result.push_str(&line.replace(prefix, "the Git package"));
            continue;
        }
        let mut rest = line;
        while let Some(at) = rest.find(prefix) {
            let before = &rest[..at];
            let quote = before.as_bytes().last().copied();
            let Some(quote @ (b'\'' | b'"')) = quote else {
                return Err(Error::refused(
                    "Git prefix outside a supported string literal",
                ));
            };
            let tail = &rest[at + prefix.len()..];
            let end = tail
                .find(char::from(quote))
                .ok_or_else(|| Error::refused("unterminated Git prefix literal"))?;
            let suffix = &tail[..end];
            if suffix.contains(['\\', '"', '$', '`']) || (perl && suffix.contains('@')) {
                return Err(Error::refused("interpolated Git prefix literal"));
            }
            result.push_str(&before[..before.len() - 1]);
            result.push('"');
            result.push_str(root);
            result.push_str(suffix);
            result.push('"');
            rest = &tail[end + 1..];
        }
        result.push_str(rest);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_literals_relocate_and_unrecognized_uses_refuse() {
        let prefix = "@@HOMEBREW_CELLAR@@/git/2.56.0";
        assert_eq!(
            literals(&format!("our $GIT = \"{prefix}/bin/git\";\n"), prefix, true).unwrap(),
            "our $GIT = \"$ENV{ATERM_GIT_ROOT}/bin/git\";\n"
        );
        assert_eq!(
            literals(&format!("root='{prefix}/share/gitweb'\n"), prefix, false).unwrap(),
            "root=\"${ATERM_GIT_ROOT}/share/gitweb\"\n"
        );
        assert!(literals(&format!("root={prefix}/share/gitweb\n"), prefix, false).is_err());
        assert!(literals(&format!("root='{prefix}/$unsafe'\n"), prefix, false).is_err());
        assert!(literals(&format!("our $root='{prefix}/@unsafe';\n"), prefix, true).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn relocated_helpers_find_their_data_after_the_package_moves() {
        let temp = aterm_tempfile::tempdir().unwrap();
        let stage = temp.path().join("stage");
        std::fs::create_dir_all(stage.join("libexec/git-core")).unwrap();
        std::fs::create_dir_all(stage.join("share/perl5")).unwrap();
        std::fs::write(
            stage.join("share/perl5/PackageProbe.pm"),
            "package PackageProbe; sub value { 'loaded package module' } 1;\n",
        )
        .unwrap();
        std::fs::write(stage.join("libexec/git-core/probe"),
            "#!/usr/bin/perl\nuse strict; use warnings;\nuse lib ('@@HOMEBREW_CELLAR@@/git/2.56.0/share/perl5');\nuse PackageProbe; print PackageProbe::value();\n").unwrap();
        std::fs::write(stage.join("libexec/git-core/shell-probe"),
            "#!/bin/sh\nroot='@@HOMEBREW_CELLAR@@/git/2.56.0/share/gitweb'\nprintf '%s' \"$root\"\n").unwrap();
        rewrite(&stage, "2.56.0").unwrap();
        let moved = temp.path().join("moved Git package");
        std::fs::rename(&stage, &moved).unwrap();
        let output = aterm_uds::spawnfd::command("/usr/bin/perl")
            .arg(moved.join("libexec/git-core/probe"))
            .env_remove("ATERM_GIT_ROOT")
            .env_remove("PERL5LIB")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"loaded package module");
        let output = aterm_uds::spawnfd::command("/bin/sh")
            .arg(moved.join("libexec/git-core/shell-probe"))
            .env("ATERM_GIT_ROOT", &moved)
            .env("PATH", "/usr/bin:/bin")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            moved.join("share/gitweb").to_string_lossy()
        );
    }
}
