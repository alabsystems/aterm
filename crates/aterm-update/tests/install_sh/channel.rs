// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The one transport — github.com's release download host, through
//! `web_curl`, stubbed here — and the Linux lane's head-only target choice.
//! Every request is a tag-specific URL, bounded by the size the host names.

use crate::support::{Lib, Scratch, follows_redirects, q, read, s};

const DOWNLOAD: &str = "https://github.com/example/aterm/releases/download";

/// A `web_curl` that records its argv and answers with the bytes of
/// `$STUB_FILE` and the status `$STUB_STATUS`.
fn stub_transport(lib: &mut Lib, scratch: &Scratch) -> std::path::PathBuf {
    let args = scratch.join("stub-args");
    lib.step(format!(
        "REPO_SLUG=example/aterm STUB_STATUS=0
		web_curl() {{ printf '%s' \"$*\" >{}; cat \"$STUB_FILE\"; return \"$STUB_STATUS\"; }}",
        q(s(&args))
    ));
    args
}

fn answer(lib: &mut Lib, scratch: &Scratch, name: &str, body: &str, status: i32) {
    let file = scratch.write(&format!("answer.{name}"), body);
    lib.step(format!("STUB_FILE={} STUB_STATUS={status}", q(s(&file))));
}

/// Presence is one redirect-refusing HEAD of the tag-specific URL: 302 is
/// published, 404 absent, anything else no answer.
#[test]
fn asset_presence_is_one_redirect_refusing_head() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    let args = stub_transport(&mut lib, &scratch);
    let call = "release_asset_present v0.92.0 aterm-0.92.0-mac.zip";
    answer(&mut lib, &scratch, "302", "302", 0);
    let present = lib.call(call, &[]);
    lib.step(format!(
        "cp {} {}",
        q(s(&args)),
        q(s(&scratch.join("present-args")))
    ));
    answer(&mut lib, &scratch, "404", "404", 0);
    let absent = lib.call(call, &[]);
    answer(&mut lib, &scratch, "502", "502", 0);
    let five = lib.call(call, &["ASSET_WHY"]);
    answer(&mut lib, &scratch, "000", "000", 7);
    let failed = lib.call(call, &["ASSET_WHY"]);
    lib.step(format!("rm -f {}", q(s(&args))));
    let evil = lib.call(
        "release_asset_present v0.92.0 aterm-0.92.0-mac.zip.evil",
        &[],
    );
    let recs = lib.run(&scratch);
    assert_eq!(recs[present].rc, 0);
    assert_eq!(recs[absent].rc, 1);
    assert_eq!(recs[five].rc, 2);
    assert!(!recs[five].var("ASSET_WHY").is_empty());
    assert_eq!(recs[failed].rc, 2);
    assert!(!recs[failed].var("ASSET_WHY").is_empty());
    assert_eq!(recs[evil].rc, 2);
    assert!(
        !args.exists(),
        "a noncanonical name is refused before any request"
    );
    let argv = read(&scratch.join("present-args"));
    let argv: Vec<&str> = argv.split(' ').collect();
    assert!(
        argv.contains(&"-I") && !follows_redirects(&argv),
        "{argv:?}"
    );
    assert_eq!(
        argv.last().copied(),
        Some(format!("{DOWNLOAD}/v0.92.0/aterm-0.92.0-mac.zip").as_str())
    );
}

/// The size is the FINAL hop's Content-Length of a HEAD that follows the
/// redirect, held to 1..<max>.
#[test]
fn asset_size_is_the_final_hops_content_length_within_its_bound() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    stub_transport(&mut lib, &scratch);
    let zip = "release_asset_size v0.92.0 aterm-0.92.0-mac.zip";
    let rows: &[(&str, &str, i32, &str, i32)] = &[
        (
            zip,
            "HTTP/2 302 \r\ncontent-length: 0\r\nlocation: https://release-assets.githubusercontent.com/x\r\n\r\nHTTP/2 200 \r\ncontent-length: 31725107\r\n\r\n",
            0,
            "31725107",
            0,
        ),
        (
            zip,
            "HTTP/2 302 \r\ncontent-length: 0\r\n\r\nHTTP/2 200 \r\n\r\n",
            0,
            "",
            2,
        ),
        (
            zip,
            "HTTP/2 200 \r\ncontent-length: 2147483649\r\n\r\n",
            0,
            "",
            2,
        ),
        (
            zip,
            "HTTP/2 200 \r\ncontent-length: 2147483648\r\n\r\n",
            0,
            "2147483648",
            0,
        ),
        (
            "release_asset_size v0.92.0 aterm-appcast.toml \"$MANIFEST_MAX_BYTES\"",
            "HTTP/2 200 \r\ncontent-length: 5000001\r\n\r\n",
            0,
            "",
            2,
        ),
        (
            "release_asset_size v0.92.0 aterm-appcast.toml.sig 64",
            "HTTP/2 200 \r\ncontent-length: 65\r\n\r\n",
            0,
            "",
            2,
        ),
        (zip, "", 22, "", 1),
    ];
    let calls: Vec<usize> = rows
        .iter()
        .enumerate()
        .map(|(n, (call, body, status, _, _))| {
            answer(&mut lib, &scratch, &format!("size{n}"), body, *status);
            lib.call(*call, &[])
        })
        .collect();
    let recs = lib.run(&scratch);
    for (row, i) in rows.iter().zip(calls) {
        assert_eq!(
            (recs[i].rc, recs[i].out.as_str()),
            (row.4, row.3),
            "{row:?}"
        );
    }
}

/// The GET is bounded by the named size and addressed to the tag-specific URL,
/// and the bytes written are held to exactly that size.
#[test]
fn a_download_is_held_to_the_named_size() {
    let scratch = Scratch::new();
    let mut lib = Lib::new();
    let args = stub_transport(&mut lib, &scratch);
    let get = |dest: &str, size: u32| {
        format!(
            "download_release_asset v0.92.0 aterm-appcast.toml {size} {}",
            q(s(&scratch.join(dest)))
        )
    };
    answer(&mut lib, &scratch, "abc", "abc", 0);
    let exact = lib.call(get("exact", 3), &[]);
    lib.step(format!(
        "cp {} {}",
        q(s(&args)),
        q(s(&scratch.join("get-args")))
    ));
    let short = lib.call(get("short", 4), &[]);
    answer(&mut lib, &scratch, "abcdef", "abcdef", 0);
    let long = lib.call(get("long", 3), &[]);
    answer(&mut lib, &scratch, "fail", "", 22);
    let failed = lib.call(get("failed", 3), &[]);
    let recs = lib.run(&scratch);
    assert_eq!(recs[exact].rc, 0);
    assert_eq!(read(&scratch.join("exact")), "abc");
    let argv = read(&scratch.join("get-args"));
    assert!(argv.contains("--max-filesize 3 "), "{argv}");
    assert!(
        argv.ends_with(&format!(" {DOWNLOAD}/v0.92.0/aterm-appcast.toml")),
        "{argv}"
    );
    assert_eq!(recs[short].rc, 2, "a short body fails closed");
    assert_eq!(recs[long].rc, 2, "an overlong body fails at the bound");
    assert_eq!(
        std::fs::metadata(scratch.join("long"))
            .expect("written")
            .len(),
        4,
        "an overlong body is read to at most the named size plus one byte"
    );
    assert_eq!(recs[failed].rc, 1, "a failed GET is a failed download");
}

/// One `linux_target_preflight` row: the tag, the head, the arch, the skip
/// reason's key fact (empty: installable), the appcast it binds, and the
/// requests it makes (`None`: not checked). A tag other than the head is a
/// `--version` pin; the head is the unpinned run's.
struct Target {
    tag: &'static str,
    head: &'static str,
    arch: &'static str,
    skip: &'static str,
    appcast: &'static str,
    requests: Option<&'static [&'static str]>,
}

/// The Linux target is chosen from the head alone: at or below it, this
/// architecture's signed binary published, and an appcast to bind it — the
/// current name, else the archived one. No answer is never read as absent.
#[test]
fn the_linux_target_is_chosen_from_the_head_alone() {
    let scratch = Scratch::new();
    let answers = scratch.write(
        "answers",
        "302 v0.101.0/aterm-appcast.toml
302 v0.101.0/aterm-0.101.0-linux-x86_64
302 v0.100.0/aterm-appcast-v0.100.0.toml
302 v0.100.0/aterm-appcast-v0.100.0.toml.sig
302 v0.100.0/aterm-0.100.0-linux-aarch64
302 v0.99.0/aterm-appcast.toml
302 v0.99.0/aterm-0.99.0-linux-x86_64
302 v0.98.0/aterm-0.98.0-linux-x86_64
502 v0.97.0/aterm-0.97.0-linux-x86_64
302 v0.96.0/aterm-0.96.0-linux-x86_64
fail v0.96.0/aterm-appcast.toml
",
    );
    let requests = scratch.join("requests");
    let rows = [
        Target {
            tag: "v0.101.0",
            head: "v0.101.0",
            arch: "x86_64",
            skip: "",
            appcast: "aterm-appcast.toml",
            requests: Some(&["v0.101.0/aterm-0.101.0-linux-x86_64"]),
        },
        Target {
            tag: "v0.101.0",
            head: "v0.101.0",
            arch: "aarch64",
            skip: "the latest release v0.101.0 publishes no Linux aarch64 executable \
                   (aterm-0.101.0-linux-aarch64)",
            appcast: "aterm-appcast.toml",
            requests: None,
        },
        Target {
            tag: "v0.99.0",
            head: "v0.101.0",
            arch: "x86_64",
            skip: "",
            appcast: "aterm-appcast.toml",
            requests: None,
        },
        Target {
            tag: "v0.100.0",
            head: "v0.101.0",
            arch: "aarch64",
            skip: "",
            appcast: "aterm-appcast-v0.100.0.toml",
            requests: Some(&[
                "v0.100.0/aterm-0.100.0-linux-aarch64",
                "v0.100.0/aterm-appcast.toml",
                "v0.100.0/aterm-appcast-v0.100.0.toml",
            ]),
        },
        Target {
            tag: "v0.100.0",
            head: "v0.101.0",
            arch: "x86_64",
            skip: "has no aterm-0.100.0-linux-x86_64",
            appcast: "aterm-appcast.toml",
            requests: None,
        },
        Target {
            tag: "v0.98.0",
            head: "v0.101.0",
            arch: "x86_64",
            skip: "has no signed manifest that covers its Linux binary",
            appcast: "aterm-appcast.toml",
            requests: None,
        },
        Target {
            tag: "v0.102.0",
            head: "v0.101.0",
            arch: "x86_64",
            skip: "is newer than the latest release v0.101.0",
            appcast: "aterm-appcast.toml",
            requests: Some(&[]),
        },
        Target {
            tag: "v0.101.1",
            head: "v0.101.0",
            arch: "x86_64",
            skip: "is newer than the latest release",
            appcast: "aterm-appcast.toml",
            requests: Some(&[]),
        },
        Target {
            tag: "v0.97.0",
            head: "v0.101.0",
            arch: "x86_64",
            skip: "github.com answered HTTP 502",
            appcast: "aterm-appcast.toml",
            requests: None,
        },
        Target {
            tag: "v0.96.0",
            head: "v0.101.0",
            arch: "x86_64",
            skip: "could not reach",
            appcast: "aterm-appcast.toml",
            requests: None,
        },
    ];
    let mut lib = Lib::new();
    // Answers by URL from the table above (`fail` is a curl that could not
    // connect), 404 for the rest, and every URL asked is logged.
    lib.step(format!(
        "REPO_SLUG=example/aterm
		web_curl() {{
			local url=\"${{!#}}\" code
			printf '%s\\n' \"$url\" >>{req}
			code=\"$(awk -v u=\"$url\" -v base={base}/ 'base $2 == u {{ print $1 }}' {ans})\"
			[[ \"$code\" != fail ]] || return 7
			printf '%s' \"${{code:-404}}\"
		}}",
        req = q(s(&requests)),
        base = q(DOWNLOAD),
        ans = q(s(&answers)),
    ));
    let calls: Vec<(usize, std::path::PathBuf)> = rows
        .iter()
        .enumerate()
        .map(|(n, row)| {
            let log = scratch.join(&format!("requests.{n}"));
            lib.step(format!(
                ": >{}; TAG_EXPLICIT={}",
                q(s(&requests)),
                u8::from(row.tag != row.head)
            ));
            let call = lib.call(
                format!(
                    "linux_target_preflight {} {} aterm-{}-linux-{}",
                    row.tag,
                    row.head,
                    row.tag.trim_start_matches('v'),
                    row.arch
                ),
                &["LINUX_TARGET_SKIP", "LINUX_MANIFEST_ASSET"],
            );
            lib.step(format!("cp {} {}", q(s(&requests)), q(s(&log))));
            (call, log)
        })
        .collect();
    let recs = lib.run(&scratch);
    for (row, (i, log)) in rows.iter().zip(&calls) {
        let i = *i;
        let label = format!("{} (head {}) {}", row.tag, row.head, row.arch);
        let skip = recs[i].var("LINUX_TARGET_SKIP");
        if row.skip.is_empty() {
            assert_eq!(skip, "", "{label}");
        } else {
            assert!(skip.contains(row.skip), "{label}: {skip}");
        }
        assert_eq!(recs[i].var("LINUX_MANIFEST_ASSET"), row.appcast, "{label}");
        if let Some(want) = row.requests {
            let asked = read(log);
            let asked: Vec<&str> = asked
                .lines()
                .map(|u| u.trim_start_matches(DOWNLOAD).trim_start_matches('/'))
                .collect();
            assert_eq!(asked, want, "{label}");
        }
    }
    // A missing binary never names a tarball. Unpinned, the head is the one
    // release the run inspected: it says that one publishes none and offers no
    // `--version` it cannot back; a pin names the pin as the remedy.
    let unpinned = recs[calls[1].0].var("LINUX_TARGET_SKIP");
    assert!(
        !unpinned.contains("--version") && !unpinned.contains("tarball"),
        "{unpinned}"
    );
    let pinned = recs[calls[4].0].var("LINUX_TARGET_SKIP");
    assert!(
        pinned.contains(
            "has no aterm-0.100.0-linux-x86_64 — pin a release that has one with --version"
        ) && !pinned.contains("tarball"),
        "{pinned}"
    );
}
