# LAN initialization retrospective

## Outcome

The intended contract was simple: a machine on this LAN should initialize itself, receive its shared credentials in `~/.secrets`, and become useful without a button, approval, phrase, or manual credential setup.

That contract is not yet met end to end on this Mac. The source fixes are committed and tested, and its credential subscriber runs a stable copy of the corrected build. However, the existing member machines still run the earlier admission protocol. No new version has been published, and `~/.secrets` has not materialized here. The final fresh LAN attempt is recorded below.

## What broke

There were two separate admission paths. The vault path retained an old policy opt-in, while the peer path required a member's Allow action and a typed phrase before handing over the credential bundle. Fixing only the vault path could not repair the peer handoff.

Other startup gates compounded the problem: some defaults waited for completed membership, the GUI waited for recent human input, developer tools could be handed off to an interactive installer, and failed setup stages could stop retrying after two attempts. Partial credential writes could also be treated as a completed handoff.

Discovery and readiness were conflated. This Mac could discover 35 member advertisements while remaining unadmitted, with no shared credentials or authenticated fleet-control route. A connected local message bus did not establish cross-machine access.

## What I got wrong

- I should have checked the real cold-start contract immediately: does `~/.secrets` exist, can `aterm agh` resolve the expected identity, and can this Mac reach an existing release agent? Those checks would have exposed the bootstrap problem earlier.
- I treated missing credentials too much like an external prerequisite. Under the requested design, obtaining those credentials is init's responsibility.
- I repeatedly described source-level fixes as fixed without making the deployment boundary prominent enough. Passing tests and a clean local commit did not mean the running LAN had changed.
- I audited and patched individual paths before tracing the complete peer-to-credential-to-source-to-release chain. This produced several rounds of discoveries and rebuilds instead of an early system-level diagnosis.
- I obtained publicly accessible source and could build it, but should have distinguished that immediately from having the authoritative development checkout, signing identity, and publishing access.
- Early toolchain probing touched an unprovisioned Apple shim and opened Apple's installer. The automatic path now avoids that fallback, but the initial probe should have verified provisioning first.

## What changed

- Default pinned-LAN vault requests no longer depend on the legacy join opt-in.
- Eligible LAN requests are not quarantined when the old open-policy setting changes.
- The peer bootstrap has a default LAN exchange that hands over credentials without an Allow action or a user-supplied phrase. It retains the existing encrypted bundle and device-binding machinery.
- Init and the subscriber use the automatic peer flow; unavailable peers leave a retryable wait.
- Incomplete credential files do not complete bootstrap, and the subscriber re-requests an incomplete bundle even when an Allowed record was saved.
- Secrets and workstation retries continue with capped backoff; verified independent work stays complete.
- Verified LAN discovery enables workstation defaults before credential delivery finishes.
- GUI admission no longer requires recent keyboard/mouse activity or a screen-presence query. Actual OS permission gestures keep their own checks.
- Developer tools install in the worker, before vault inputs, through the background update service with verification.
- Codex's installed instructions and the managed primer route GitHub operations through bundled `aterm agh`.
- System State has corrected waiting/failure behavior and a separate interactive redesign prototype. The full redesigned interface is not deployed.

## Evidence and limits

Relevant validation passed: 1,132 machine tests; 1,061 secrets tests with one ignored; 12 members CLI tests; 712 CLI tests and 448 System State/handler tests before the subsequent peer changes; 61 primer tests; and the executable build. These were explicitly unverified Trust runs, with no proof claim.

The peer tests include a no-approval credential write, rejection of a non-LAN automatic request, and recovery after a blocked credential path becomes writable. These tests do not substitute for a successful live join.

The live sweep found 25 endpoints rejecting the new exchange and ten unavailable endpoints. Fleet control refused this unadmitted Mac, SSH to m11 and m12 rejected the available credentials, and targeted Keychain checks found no matching credentials. No remote agent received the release task. No credential values were printed.

The new exchange requires an updated member. That rollout dependency must be explicit; a client-only update cannot make the old server protocol grant a new operation.

## How this should work

1. Make cold-start readiness an executable contract, not a collection of individually successful tasks. Include credential materialization, routed authentication, source readiness, and the absence of user interaction.
2. Exercise that contract with a fresh machine, an already-running member, no terminal input, and no manual approval. Include an older member version, offline/rejoin, partial writes, interrupted work, and process restarts.
3. Keep one persisted dependency graph and explicit states: discovered, joining, syncing, materializing, and ready. Surface the blocked dependency and the next automatic attempt concisely. Never label an attempted or waiting task done.
4. Ship member-side compatibility first and advertise supported bootstrap capabilities. A mixed-version LAN must have a deliberate migration path, rather than discovering incompatibility after the client is installed.
5. Establish one already-provisioned release route before relying on an unprovisioned client to repair the fleet. LAN discovery, credential handoff, and release capability must be tested separately.
6. Start verification with the live acceptance checks. Use focused tests for the changed boundary, then integration tests, and report source, running-local, and deployed-fleet outcomes separately.

## Remaining work

Obtain the bootstrap keys or use an already-provisioned release machine, land the changes in the development repository, and publish through the normal signing pipeline. After a member updates, verify that this Mac's running subscriber receives the credentials automatically and that `aterm agh` resolves the intended writer. Then verify fleet-wide rollout and replace the temporary pinned subscriber with the released program through the normal updater.

The prior subscriber configuration is saved at `/Users/ayates/.local/state/aterm-init-audit/subscriber-before-5b55fd73.plist`. Its current program is `/Users/ayates/.local/libexec/aterm-lan-init-5b55fd73`.

## Final fresh attempt

The final requested attempt used the updated executable and fresh LAN discovery. It returned `Waiting for automatic LAN credential sync: the other Mac asked for something this Mac does not serve`. A subsequent check still found no `~/.secrets` directory, no `aterm agh` profiles, and anonymous routing. No further manual peer-provisioning attempt was made after that requested final try. The installed subscriber remains active and retries automatically.


## Naming mismatch

The operator identifies this machine as m60. Its actual macOS ComputerName is `Andrew’s MacBook Pro`, LocalHostName is `Andrews-MacBook-Pro`, and HostName is unset. Its current LAN address was 192.168.1.84 when checked. Aterm's fallback name selection produced the pending join name `andrews` because no `m<number>` tag exists in those OS names. I should have stated the inventory label, OS discovery name, and pending join name separately instead of treating them as interchangeable. The operator's m60 label has not been independently resolved from the missing fleet inventory.
