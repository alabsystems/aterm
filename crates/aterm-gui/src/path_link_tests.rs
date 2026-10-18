// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::*;

fn chars(s: &str) -> Vec<char> {
    s.chars().collect()
}

/// The span under every column of the citation, and `None` off it.
fn span_at(row: &str, col: usize) -> Option<(String, Option<u32>, Option<u32>)> {
    find_path_span(&chars(row), col).map(|s| (s.path, s.line, s.col))
}

fn some(
    path: &str,
    line: Option<u32>,
    col: Option<u32>,
) -> Option<(String, Option<u32>, Option<u32>)> {
    Some((path.to_string(), line, col))
}

/// A scratch directory with `src/foo.rs` (five lines), a directory and a
/// symlink, canonicalized so `/tmp` and `/private/tmp` agree.
struct Scratch {
    _dir: aterm_tempfile::TempDir,
    root: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let dir = aterm_tempfile::tempdir().expect("scratch");
        let root = std::fs::canonicalize(dir.path()).expect("canonical scratch");
        std::fs::create_dir_all(root.join("src/sub")).unwrap();
        std::fs::write(root.join("src/foo.rs"), "one\ntwo\nthree\nfour\nfive\n").unwrap();
        Self { _dir: dir, root }
    }

    fn bases(&self) -> Vec<PathBuf> {
        vec![self.root.clone()]
    }
}

#[test]
fn a_compiler_citation_names_the_file_line_and_column() {
    let row = "  --> src/main.rs:12:5";
    for col in 6..row.len() {
        assert_eq!(
            span_at(row, col),
            some("src/main.rs", Some(12), Some(5)),
            "{col}"
        );
    }
    assert_eq!(span_at(row, 3), None, "the arrow is not the citation");
    assert_eq!(span_at(row, 5), None, "nor the space before it");
}

#[test]
fn trailing_sentence_punctuation_and_wrapping_brackets_are_not_the_path() {
    assert_eq!(
        span_at("(see src/a.rs:12).", 6),
        some("src/a.rs", Some(12), None)
    );
    assert_eq!(
        span_at("see `src/a.rs`, then", 6),
        some("src/a.rs", None, None)
    );
    assert_eq!(
        span_at("grep: src/lib.rs:40: fn x", 8),
        some("src/lib.rs", Some(40), None)
    );
    assert_eq!(span_at("[README.md]", 3), some("README.md", None, None));
    assert_eq!(
        span_at("\"crates/x/y.toml\";", 4),
        some("crates/x/y.toml", None, None)
    );
    // The trimmed punctuation is not part of the link either.
    assert_eq!(span_at("src/a.rs.", 8), None);
}

#[test]
fn github_style_line_fragments_name_the_first_line() {
    assert_eq!(
        span_at("src/a.rs#L4-L9", 2),
        some("src/a.rs", Some(4), None)
    );
    assert_eq!(span_at("src/a.rs#L4-9", 2), some("src/a.rs", Some(4), None));
    assert_eq!(span_at("src/a.rs#L7", 0), some("src/a.rs", Some(7), None));
    assert_eq!(
        span_at("src/a.rs#L7C3", 0),
        some("src/a.rs", Some(7), Some(3))
    );
    // A fragment that names no line is part of the name.
    assert_eq!(span_at("src/a.rs#Lx", 0), some("src/a.rs#Lx", None, None));
    // Line zero is no line.
    assert_eq!(span_at("src/a.rs:0", 0), some("src/a.rs:0", None, None));
}

#[test]
fn a_path_never_spans_a_space() {
    let row = "open src/my file.rs:3 now";
    assert_eq!(span_at(row, 6), some("src/my", None, None));
    assert_eq!(span_at(row, 13), some("file.rs", Some(3), None));
    assert_eq!(span_at(row, 4), None);
}

#[test]
fn words_urls_and_numbers_are_not_citations() {
    assert_eq!(span_at("hello world", 2), None);
    assert_eq!(span_at("see https://x.example/a.rs:3", 10), None);
    assert_eq!(span_at("file:///etc/passwd", 10), None);
    assert_eq!(span_at("...", 1), None);
    assert_eq!(span_at("", 0), None);
}

#[test]
fn unicode_names_and_wide_glyphs_stay_one_citation() {
    let row = "→ ünï/fïle.rs:3";
    let col = row.chars().position(|c| c == 'f').unwrap();
    assert_eq!(span_at(row, col), some("ünï/fïle.rs", Some(3), None));
    // A wide glyph takes two columns: its continuation stays in the token.
    let mut cells = chars("x 文");
    cells.push(WIDE_CONTINUATION);
    cells.extend("件/a.rs:2".chars());
    cells.insert(5, WIDE_CONTINUATION);
    for col in 2..cells.len() {
        let span = find_path_span(&cells, col).expect("one token");
        assert_eq!(span.path, "文件/a.rs", "{col}");
        assert_eq!((span.start, span.end), (2, cells.len() - 1));
    }
}

#[test]
fn a_relative_citation_resolves_against_the_sessions_directories_in_order() {
    let s = Scratch::new();
    let other = aterm_tempfile::tempdir().unwrap();
    let other = std::fs::canonicalize(other.path()).unwrap();
    // The agent's cwd first: there is no src/foo.rs in `other`, so the OSC 7
    // directory answers.
    let found = resolve("src/foo.rs", &[other.clone(), s.root.clone()], None).unwrap();
    assert_eq!(found, s.root.join("src/foo.rs"));
    // With both holding one, the first base wins.
    std::fs::create_dir_all(other.join("src")).unwrap();
    std::fs::write(other.join("src/foo.rs"), "x").unwrap();
    let found = resolve("src/foo.rs", &[other.clone(), s.root.clone()], None).unwrap();
    assert_eq!(found, other.join("src/foo.rs"));
    // `./` and `..` are paths like any other.
    let found = resolve("./src/sub/../foo.rs", &s.bases(), None).unwrap();
    assert_eq!(found, s.root.join("src/foo.rs"));
    // An absolute path ignores the bases; `~/` uses home.
    let abs = s.root.join("src/foo.rs");
    assert_eq!(resolve(abs.to_str().unwrap(), &[], None).unwrap(), abs);
    assert_eq!(
        resolve("~/src/foo.rs", &[], Some(&s.root)).unwrap(),
        s.root.join("src/foo.rs")
    );
}

#[test]
fn refusals_missing_directory_bundle_unreadable_control_and_no_base() {
    let s = Scratch::new();
    let b = s.bases();
    assert_eq!(resolve("src/nope.rs", &b, None), Err(PathRefusal::Missing));
    assert_eq!(resolve("src/sub", &b, None), Err(PathRefusal::Directory));
    assert_eq!(resolve("src", &b, None), Err(PathRefusal::Directory));
    assert_eq!(
        resolve("src/foo.rs", &[], None),
        Err(PathRefusal::NoBase),
        "never aterm's own cwd"
    );
    assert_eq!(
        resolve("src/foo.rs", &[PathBuf::from("relative/base")], None),
        Err(PathRefusal::NoBase),
        "a relative base is no base"
    );
    assert_eq!(
        resolve("~other/x.rs", &b, Some(Path::new("/"))),
        Err(PathRefusal::Missing)
    );
    // An .app bundle, and anything inside one.
    std::fs::create_dir_all(s.root.join("Evil.app/Contents")).unwrap();
    std::fs::write(s.root.join("Evil.app/Contents/Info.plist"), "x").unwrap();
    assert_eq!(resolve("Evil.app", &b, None), Err(PathRefusal::AppBundle));
    assert_eq!(
        resolve("Evil.app/Contents/Info.plist", &b, None),
        Err(PathRefusal::AppBundle)
    );
    // Unreadable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let locked = s.root.join("locked.rs");
        std::fs::write(&locked, "secret").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root reads anything; the refusal is only observable for a user.
        if std::fs::File::open(&locked).is_err() {
            assert_eq!(resolve("locked.rs", &b, None), Err(PathRefusal::Unreadable));
        }
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    // Controls and bidi/format characters never resolve, even where a file
    // of that name exists.
    for hostile in [
        "src/foo.rs\u{7}",
        "src/\u{202e}foo.rs",
        "src/foo\u{200b}.rs",
        "\u{feff}src/foo.rs",
        "src/\nfoo.rs",
    ] {
        assert_eq!(
            resolve(hostile, &b, None),
            Err(PathRefusal::Control),
            "{hostile:?}"
        );
    }
}

/// Every control (`Cc`), format (`Cf`) and default-ignorable character is
/// refused, even where a file of that name exists on disk: one sample from
/// each listed range, at both ends of each. NEGATIVE CONTROL: visible
/// non-ASCII names (an accent, CJK, an emoji, Arabic and Hangul letters next
/// to the refused marks and fillers) resolve, so the rule is not "anything
/// outside ASCII".
#[test]
fn every_control_format_and_default_ignorable_character_is_refused() {
    let s = Scratch::new();
    let b = s.bases();
    let hostile = [
        '\u{0}',
        '\u{1f}',
        '\u{7f}',
        '\u{85}',
        '\u{9f}',
        '\u{ad}',
        '\u{34f}',
        '\u{600}',
        '\u{605}',
        '\u{61c}',
        '\u{6dd}',
        '\u{70f}',
        '\u{890}',
        '\u{8e2}',
        '\u{115f}',
        '\u{1160}',
        '\u{17b4}',
        '\u{180b}',
        '\u{180e}',
        '\u{180f}',
        '\u{200b}',
        '\u{200f}',
        '\u{202a}',
        '\u{202e}',
        '\u{2060}',
        '\u{2064}',
        '\u{2066}',
        '\u{2069}',
        '\u{206a}',
        '\u{206f}',
        '\u{3164}',
        '\u{fe00}',
        '\u{fe0f}',
        '\u{feff}',
        '\u{ffa0}',
        '\u{fff9}',
        '\u{fffb}',
        '\u{110bd}',
        '\u{13430}',
        '\u{1bca0}',
        '\u{1d173}',
        '\u{e0000}',
        '\u{e0001}',
        '\u{e0020}',
        '\u{e007f}',
        '\u{e0100}',
        '\u{e01ef}',
    ];
    for ch in hostile {
        assert!(is_hostile_char(ch), "{:04x}", ch as u32);
        let name = format!("a{ch}b.rs");
        // Best effort: a filesystem may refuse a name (NUL always), and the
        // refusal must not depend on whether the file is there.
        let _ = std::fs::write(s.root.join(&name), "x");
        assert_eq!(
            resolve(&name, &b, None),
            Err(PathRefusal::Control),
            "{:04x}",
            ch as u32
        );
    }
    for ch in ['\u{e9}', '\u{4e2d}', '\u{1f600}', '\u{628}', '\u{ac00}'] {
        assert!(!is_hostile_char(ch), "{:04x}", ch as u32);
        let name = format!("a{ch}b.rs");
        std::fs::write(s.root.join(&name), "x").unwrap();
        assert_eq!(
            resolve(&name, &b, None),
            Ok(s.root.join(&name)),
            "{:04x}",
            ch as u32
        );
    }
}

/// A file whose REAL name carries a hostile character is refused even when
/// the printed citation is clean: the caption names the symlink's target,
/// and could not show it honestly. NEGATIVE CONTROL: the same link to a
/// clean name resolves.
#[cfg(unix)]
#[test]
fn a_link_to_a_name_the_caption_could_not_show_is_refused() {
    let s = Scratch::new();
    let hidden = s.root.join("rea\u{200b}l.rs");
    std::fs::write(&hidden, "x").unwrap();
    std::os::unix::fs::symlink(&hidden, s.root.join("clean.rs")).unwrap();
    assert_eq!(
        resolve("clean.rs", &s.bases(), None),
        Err(PathRefusal::Control)
    );
    std::os::unix::fs::symlink(s.root.join("src/foo.rs"), s.root.join("fine.rs")).unwrap();
    assert_eq!(
        resolve("fine.rs", &s.bases(), None),
        Ok(s.root.join("src/foo.rs"))
    );
}

/// The remote shells by their program word, as `status program=` (an argv[0]
/// basename) and `detail=` (the running command) spell them. NEGATIVE
/// CONTROL: local shells, agents and look-alike names are not remote.
#[test]
fn the_remote_shells_are_named_by_their_program_word() {
    for remote in [
        "ssh",
        "SSH",
        "/usr/bin/ssh",
        "ssh build-box",
        "slogin",
        "autossh",
        "mosh",
        "mosh-client",
        "et",
        "-ssh",
    ] {
        assert!(is_remote_shell(remote), "{remote:?}");
    }
    for local in [
        "",
        "zsh",
        "sshd",
        "ssh-agent",
        "ssh-add",
        "codex",
        "claude",
        "etc",
        "mosher",
        "vim",
    ] {
        assert!(!is_remote_shell(local), "{local:?}");
    }
}

#[cfg(unix)]
#[test]
fn a_symlink_is_resolved_to_its_target_and_a_link_to_a_directory_is_refused() {
    let s = Scratch::new();
    std::os::unix::fs::symlink(s.root.join("src/foo.rs"), s.root.join("link.rs")).unwrap();
    std::os::unix::fs::symlink(s.root.join("src/sub"), s.root.join("dirlink.rs")).unwrap();
    let span = find_path_span(&chars("link.rs:2"), 0).unwrap();
    let target = PathTarget::resolve(&span, &s.bases(), None).unwrap();
    assert_eq!(
        target.path,
        s.root.join("src/foo.rs"),
        "the target, not the link"
    );
    assert_eq!(
        target.shown,
        format!("{}:2", s.root.join("src/foo.rs").display()),
        "and the caption names the target"
    );
    assert_eq!(
        resolve("dirlink.rs", &s.bases(), None),
        Err(PathRefusal::Directory)
    );
    std::os::unix::fs::symlink(s.root.join("gone.rs"), s.root.join("dangling.rs")).unwrap();
    assert_eq!(
        resolve("dangling.rs", &s.bases(), None),
        Err(PathRefusal::Missing)
    );
}

#[test]
fn the_caption_names_the_file_relative_to_the_session_or_home() {
    let s = Scratch::new();
    let file = s.root.join("src/foo.rs");
    assert_eq!(
        display(&file, Some(3), Some(2), Some(&s.root), None),
        "src/foo.rs:3:2"
    );
    assert_eq!(
        display(&file, None, None, None, Some(&s.root)),
        "~/src/foo.rs"
    );
    assert_eq!(
        display(&file, Some(9), None, None, None),
        format!("{}:9", file.display())
    );
}

/// The caption names a file where it really is, never relative to one of the
/// session's directories (which the band does not show): the same printed
/// citation found in two projects reads differently, so a caption that reads
/// alike names one file. NEGATIVE CONTROL: under home the name is `~/…`, and
/// an absolute citation under a directory of `/` is named in full.
#[test]
fn the_caption_names_the_file_where_it_is_never_relative_to_a_directory() {
    let s = Scratch::new();
    let other = Scratch::new();
    let file = s.root.join("src/foo.rs");
    let span = |row: &str| find_path_span(&chars(row), 0).expect("a citation");
    let here = PathTarget::resolve(&span("src/foo.rs:12"), &s.bases(), None).unwrap();
    assert_eq!(here.path, file);
    assert_eq!(here.shown, format!("{}:12", file.display()));
    let there = PathTarget::resolve(&span("src/foo.rs:12"), &other.bases(), None).unwrap();
    assert_ne!(here.shown, there.shown, "two projects' files read apart");
    let agent = aterm_tempfile::tempdir().unwrap();
    let agent = std::fs::canonicalize(agent.path()).unwrap();
    let elsewhere = PathTarget::resolve(&span("src/foo.rs:12"), &[agent, s.root.clone()], None)
        .expect("found through the second directory");
    assert_eq!(elsewhere.shown, here.shown);
    let home = PathTarget::resolve(&span("src/foo.rs:12"), &s.bases(), Some(&s.root)).unwrap();
    assert_eq!(home.shown, "~/src/foo.rs:12", "under home");
    let printed = format!("{}:3", file.display());
    let absolute = PathTarget::resolve(&span(&printed), &[PathBuf::from("/")], None).unwrap();
    assert_eq!(absolute.shown, printed);
}

/// An emoji presentation selector (U+FE0F) after a character it turns into
/// an emoji changes what is drawn, so a name keeps it (`☁️`, `❤️`, `1️⃣`).
/// NEGATIVE CONTROLS: the same selector after a letter or after an emoji
/// already drawn as one changes nothing on screen, and a zero-width joiner
/// can hide a character: all three are refused, on disk or not.
#[test]
fn an_emoji_selector_that_changes_the_glyph_is_kept_and_any_other_refused() {
    let s = Scratch::new();
    let b = s.bases();
    for name in [
        "a\u{2601}\u{fe0f}b.rs",
        "\u{2764}\u{fe0f}.rs",
        "1\u{fe0f}\u{20e3}.rs",
    ] {
        assert!(!hides_text(name), "{name:?}");
        std::fs::write(s.root.join(name), "x").unwrap();
        assert_eq!(resolve(name, &b, None), Ok(s.root.join(name)), "{name:?}");
    }
    for name in [
        "a\u{fe0f}b.rs",
        "\u{1f600}\u{fe0f}.rs",
        "\u{fe0f}.rs",
        "\u{1f469}\u{200d}\u{1f4bb}.rs",
        "\u{2601}\u{fe0f}\u{fe0f}.rs",
    ] {
        assert!(hides_text(name), "{name:?}");
        let _ = std::fs::write(s.root.join(name), "x");
        assert_eq!(
            resolve(name, &b, None),
            Err(PathRefusal::Control),
            "{name:?}"
        );
    }
}

/// A file whose real name is not UTF-8 would be captioned with replacement
/// characters two names can share, so it is refused. NEGATIVE CONTROL: the
/// same link to a UTF-8 name resolves. Linux only: macOS file systems refuse
/// such a name.
#[cfg(target_os = "linux")]
#[test]
fn a_link_to_a_name_that_is_not_utf8_is_refused() {
    use std::os::unix::ffi::OsStrExt as _;
    let s = Scratch::new();
    let raw = s.root.join(std::ffi::OsStr::from_bytes(b"na\xffme.rs"));
    std::fs::write(&raw, "x").unwrap();
    std::os::unix::fs::symlink(&raw, s.root.join("clean.rs")).unwrap();
    assert_eq!(
        resolve("clean.rs", &s.bases(), None),
        Err(PathRefusal::Control)
    );
    std::os::unix::fs::symlink(s.root.join("src/foo.rs"), s.root.join("fine.rs")).unwrap();
    assert_eq!(
        resolve("fine.rs", &s.bases(), None),
        Ok(s.root.join("src/foo.rs"))
    );
}

/// A cwd report stands under the group in front when the sweep first saw
/// it: an ssh's remote report says nothing once the shell is back in front.
/// A report newer than the sweep's last look stands (it may be the
/// remote's), and so does every report while no sweep has seen one.
/// NEGATIVE CONTROL: a carried cwd (no report this engine saw) never stands.
#[test]
fn a_cwd_report_stands_only_under_the_group_it_was_seen_under() {
    let (shell, ssh) = (100, 200);
    assert!(
        report_stands(Some(3), Some((3, ssh)), ssh),
        "ssh still in front"
    );
    assert!(
        !report_stands(Some(3), Some((3, ssh)), shell),
        "the shell is back"
    );
    assert!(
        report_stands(Some(4), Some((3, ssh)), shell),
        "a report since"
    );
    assert!(report_stands(Some(1), None, shell), "no sweep has looked");
    assert!(
        report_stands(Some(0), None, -1),
        "no report, nothing to doubt"
    );
    assert!(!report_stands(None, None, shell), "a carried cwd");
    assert!(
        !report_stands(None, Some((3, shell)), shell),
        "a carried cwd"
    );
}

/// The program in front runs here when aterm names it a shell or an agent:
/// an allow-list, so a remote shell, a multiplexer, a wrapper with its own
/// terminal and a name not resolved yet need no list to be refused.
/// NEGATIVE CONTROLS: each of those, and Claude Code's screen read under
/// `ssh` (a remote one), is not.
#[test]
fn only_a_shell_or_an_identified_agent_runs_here() {
    let front = |program: Option<&str>, reader: Option<aterm_phase::Program>| {
        crate::session_timeline::AgentPublication {
            program: program.map(String::from),
            reader,
            ..Default::default()
        }
    };
    for program in [
        "zsh", "bash", "fish", "sh", "nu", "pwsh", "claude", "codex", "agy",
    ] {
        assert!(runs_here(&front(Some(program), None)), "{program}");
    }
    assert!(
        runs_here(&front(Some("node"), Some(aterm_phase::Program::Claude))),
        "Claude Code started as node, by its screen"
    );
    for program in [
        "ssh",
        "mosh-client",
        "et",
        "tmux",
        "screen",
        "sudo",
        "sshpass",
        "docker",
        "kubectl",
        "telnet",
        "sftp",
        "kitten",
        "gcloud",
        "vim",
        "less",
        "node",
        "",
    ] {
        assert!(!runs_here(&front(Some(program), None)), "{program}");
    }
    assert!(!runs_here(&front(None, None)), "not named yet");
    assert!(
        !runs_here(&front(Some("ssh"), Some(aterm_phase::Program::Claude))),
        "a remote Claude Code's screen"
    );
}

/// The UI's look ([`Looks`]): one not back within its wait is no link yet,
/// and once back its answer is what the next ask reads. A zero wait is the
/// slow mount: the asker never waits for the filesystem at all.
#[test]
fn a_look_not_back_in_time_is_no_link_yet_and_its_answer_is_kept() {
    let s = Scratch::new();
    let looks = Looks::new(Duration::ZERO);
    let span = find_path_span(&chars("src/foo.rs:2"), 0).expect("a citation");
    assert!(
        looks.target(&span, &s.bases(), None).is_none(),
        "the look is not back: no link yet"
    );
    let target =
        aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the look answers", || {
            looks.target(&span, &s.bases(), None)
        })
        .expect("the file resolves");
    assert_eq!(target.path, s.root.join("src/foo.rs"));
    assert_eq!(
        target.shown,
        format!("{}:2", s.root.join("src/foo.rs").display())
    );
    // Text that refuses on its face is answered at once, with no look.
    let hostile = PathSpan {
        path: "src/\u{202e}foo.rs".to_string(),
        line: None,
        col: None,
        start: 0,
        end: 0,
    };
    assert_eq!(
        looks.target(&hostile, &s.bases(), None),
        Some(Err(PathRefusal::Control))
    );
}

/// A FIFO never qualifies, and the readability open never waits on one for a
/// writer (`O_NONBLOCK`): a FIFO swapped in under the look cannot hang it.
#[cfg(unix)]
#[test]
fn a_fifo_is_refused_and_its_open_never_waits_for_a_writer() {
    let s = Scratch::new();
    let fifo = s.root.join("pipe.rs");
    let c_path = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
    // SAFETY: a valid NUL-terminated path; mkfifo only creates the node.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
    assert_eq!(
        resolve("pipe.rs", &s.bases(), None),
        Err(PathRefusal::NotRegular)
    );
    let opened = open_for_look(&fifo).expect("opens at once, no writer needed");
    assert!(
        !opened.metadata().unwrap().is_file(),
        "and is seen for what it is"
    );
}

#[test]
fn a_line_and_column_land_on_the_right_byte() {
    let text = "one\ntwö\nthree";
    assert_eq!(line_col_offset(text, 1, None), 0);
    assert_eq!(line_col_offset(text, 2, None), 4);
    assert_eq!(
        line_col_offset(text, 2, Some(3)),
        4 + "tw".len(),
        "column counts chars"
    );
    assert_eq!(&text[line_col_offset(text, 2, Some(3))..][..2], "ö");
    assert_eq!(
        line_col_offset(text, 2, Some(99)),
        4 + "twö".len(),
        "never past the line"
    );
    assert_eq!(line_col_offset(text, 3, Some(2)), 4 + "twö\n".len() + 1);
    assert_eq!(
        line_col_offset(text, 99, None),
        4 + "twö\n".len(),
        "past the end: last line"
    );
    assert_eq!(line_col_offset("", 5, Some(5)), 0);
}

/// One xorshift64* step: deterministic, no dependency, no clock.
fn xorshift(mut state: u64) -> impl FnMut() -> u64 {
    move || {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        state.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

/// THE PROPERTY: no path link ever reaches `open_url_external`. Over random
/// rows mixing real citations (of files that exist), URLs of every scheme,
/// `file:` URIs, `.app` bundles, directories, words and punctuation, the one
/// decision a click takes ([`decide`]) returns [`LinkAction::External`] only
/// for the URL the URL reader found, and only when `is_safe_url` admits it:
/// whatever a row holds, a citation's only action is aterm's own editor. An
/// OSC 8 link with a refused scheme over a citation opens nothing at all.
#[test]
fn no_path_link_ever_reaches_open_url_external() {
    let s = Scratch::new();
    std::fs::create_dir_all(s.root.join("Tool.app/Contents/MacOS")).unwrap();
    std::fs::write(s.root.join("Tool.app/Contents/MacOS/tool"), "#!/bin/sh\n").unwrap();
    let abs = s.root.join("src/foo.rs");
    let abs = abs.to_str().unwrap().to_string();
    let vocabulary: Vec<String> = vec![
        "src/foo.rs".into(),
        "src/foo.rs:3".into(),
        "src/foo.rs:3:2".into(),
        "src/foo.rs#L2-L4".into(),
        format!("{abs}:1"),
        format!("file://{abs}"),
        "file:///Applications/Calculator.app".into(),
        "Tool.app".into(),
        "Tool.app/Contents/MacOS/tool".into(),
        "src/sub".into(),
        "https://example.com/src/foo.rs:3".into(),
        "http://localhost:8080/x".into(),
        "mailto:a@b.example".into(),
        "javascript:alert(1)".into(),
        "ssh://host/src/foo.rs".into(),
        "word".into(),
        "e.g.".into(),
        "(".into(),
        ")".into(),
        ":".into(),
        ",".into(),
        "`".into(),
        "\"".into(),
    ];
    let mut next = xorshift(0x9E37_79B9_7F4A_7C15);
    let mut documents = 0usize;
    let mut externals = 0usize;
    for _ in 0..400 {
        let mut row = String::new();
        for _ in 0..(1 + next() % 6) {
            let token = &vocabulary[(next() % vocabulary.len() as u64) as usize];
            row.push_str(token);
            if !next().is_multiple_of(3) {
                row.push(' ');
            }
        }
        let cells = chars(&row);
        for col in 0..cells.len() {
            let url = crate::find_url_span(&cells, col).map(|(url, _, _)| url);
            let path = find_path_span(&cells, col)
                .and_then(|span| PathTarget::resolve(&span, &s.bases(), None).ok());
            if let Some(target) = &path {
                assert!(target.path.starts_with(&s.root), "{row:?} @ {col}");
                assert!(target.path.is_file(), "{row:?} @ {col}");
            }
            match decide(url.clone(), path.clone()) {
                LinkAction::External(opened) => {
                    externals += 1;
                    assert_eq!(Some(&opened), url.as_ref(), "{row:?} @ {col}");
                    assert!(crate::is_safe_url(&opened), "{row:?} @ {col}");
                }
                LinkAction::Document(target) => {
                    documents += 1;
                    assert!(url.is_none(), "a URL always wins: {row:?} @ {col}");
                    assert_eq!(Some(target), path, "{row:?} @ {col}");
                }
                LinkAction::Nothing => {}
            }
            // A refused OSC 8 scheme over the same cell opens nothing, and in
            // particular not the citation beneath it.
            assert_eq!(
                decide(Some("file:///etc/passwd".into()), path.clone()),
                LinkAction::Nothing
            );
            // With no URL at all, nothing a row holds becomes an OS open.
            assert!(!matches!(decide(None, path), LinkAction::External(_)));
        }
    }
    assert!(
        documents > 0 && externals > 0,
        "both arms were exercised: {documents} {externals}"
    );
}
