<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 Andrew Yates -->

# Security Policy

## Supported release

Security maintenance is best-effort and applies to the release marked Latest on
this repository's [Releases](https://github.com/alabsystems/aterm/releases)
page: the newest application build and the source snapshot at its version. A
newer `vX.Y.0` prerelease with no application is source only. Earlier releases
are superseded rather than maintained.

Installed copies update themselves from that channel. An install with automatic
checks off (`[update] enabled = false`) or installs deferred
(`[update] auto_apply = false`) is supported once it is brought current: Check
for Updates and Update Now in Settings ▸ Software Update, or `aterm update
check` then `aterm ctl update apply`; both work with either switch off.

If a machine has stopped receiving updates, `aterm update status` and Settings ▸
Software Update say why; re-running the one-line installer from
[README ▸ Install](README.md#install) restores a machine whose updater cannot
recover on its own. A binary you build yourself is never updated and tracks
whatever commit you built.

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

A fix ships as the next `MAJOR.MINOR.0` release and reaches installed copies
through the ordinary update path; there are no security-only patch releases
([VERSIONING.md](VERSIONING.md)). There is no separate advisory feed, and no
embargo process beyond one agreed by mail.

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
- escapes from the sandbox profile that `--sandbox` applies to session
  processes on macOS (network, other processes' task ports, mach service
  lookups, write confinement, credential and private-data directories), a
  `containment` session starting on a platform with no OS sandbox (it must
  refuse), or a containment gate on any platform failing to enforce what it
  announces at startup;
- capture-path escapes or unintended disclosure of terminal contents; and
- committed or emitted credentials.

Ordinary documentation errors, feature requests, and non-security bugs may use
[GitHub Issues](https://github.com/alabsystems/aterm/issues).
