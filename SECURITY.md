<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Andrew Yates -->

# Security Policy

## Supported release

Security maintenance is best-effort and applies to the newest `vX.Y.0` release
on the public channel — the macOS application published on this repository's
[Releases](https://github.com/alabsystems/aterm/releases) page, and the source
snapshot published at that release's version. (The page's "Latest" badge can be
held by a non-application release, such as a toolchain package index; the
newest `vX.Y.0` tag is the one that matters.) Earlier releases are superseded
rather than maintained.

Installed copies update themselves from that channel, so a running macOS
install converges on the supported version on its own. An install that has
turned the updater off (`ATERM_NO_AUTO_UPDATE=1`) or deferred applying
(`[update] auto_apply = false`) is supported only once it is brought current —
`aterm ctl update apply`, or Settings ▸ Software Update.

If a machine has stopped receiving updates, `aterm ctl update status` reports
the reason and Settings ▸ Software Update shows it in the app; re-running the
one-line installer from [README ▸ Install](README.md#install) restores a
machine whose updater cannot recover on its own. A binary you build yourself is
not updated by anything — the updater only ever replaces an installed
`aterm.app` — and tracks whatever commit you built.

## Reporting a vulnerability

Do not open a public issue or pull request with exploit details, credentials,
sensitive terminal contents, or private artifacts.

Email Andrew Yates at <andrewyates.name@gmail.com>. Please include:

- the affected public version or commit;
- the operating system and relevant configuration;
- the impact and realistic attack scenario;
- steps to reproduce and the smallest safe proof of concept; and
- any suggested mitigation.

If the report needs confidentiality beyond plain email, say so in the first
message so another channel can be arranged. There is no response-time
guarantee.

A fix ships as the next `MAJOR.MINOR.0` release on the public channel and
reaches installed copies through the ordinary update path; because the patch
slot is always `0`, there are no security-only patch releases. There is no
separate advisory feed, and no embargo process beyond one agreed by mail.

## In scope

Examples of issues that should be reported privately include:

- bypasses of control-socket authentication or scoped capabilities;
- command or input injection across the control boundary;
- memory-safety faults triggered by untrusted terminal input;
- updater, package-signature, or release-provenance failures — including
  anything that would let an unrostered or revoked machine key produce an
  artifact a client accepts, or roll a client back to a lower build number (the
  roster, its deny-list, and the forward-only build rule are described in
  [README ▸ Security model](README.md#security-model));
- escapes from the network and credential-directory sandbox profile that
  `--sandbox` applies to session processes on macOS, or a containment gate on
  any platform failing to enforce what it announces at startup (the platforms
  differ by design, so a difference aterm discloses is not itself a finding);
- capture-path escapes or unintended disclosure of terminal contents; and
- committed or emitted credentials.

Ordinary documentation errors, feature requests, and non-security bugs may use
[GitHub Issues](https://github.com/alabsystems/aterm/issues).
