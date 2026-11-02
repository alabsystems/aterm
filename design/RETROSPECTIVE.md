# LAN initialization retrospective

## Outcome

The intended contract was simple: a machine on this LAN should initialize itself, receive its shared credentials in `~/.secrets`, and become useful without a button, approval, phrase, or manual credential setup.

**Current policy: joining the trusted physical LAN is sufficient authentication and authorization to join aterm.** There is no second approval step for LAN membership. Device keys and encrypted transport are created and used automatically; they do not ask the person to authenticate again. Additional channels, internet exposure, and their protections are future work, not prerequisites for this LAN-only flow.

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

The live sweep found 25 endpoints rejecting the new exchange and ten unavailable endpoints. Fleet control refused this unadmitted Mac, SSH to existing members rejected the available credentials, and targeted Keychain checks found no matching credentials. No remote agent received the release task. No credential values were printed.

The new exchange requires an updated member. That rollout dependency must be explicit; a client-only update cannot make the old server protocol grant a new operation.

## How this should have worked, fully automatically

The person connects a new machine to the LAN and opens aterm. Initialization starts immediately, with no setup button, open panel, recent input, password, invitation phrase, or per-machine approval. The person can start working while independent setup tasks run.

| Step | Automatic behavior | Evidence of completion |
| --- | --- | --- |
| Discover | Find the existing aterm members through the existing LAN transport. Start all ready local setup tasks at once. | Reachable peers and their supported bootstrap capabilities are known. |
| Identify and admit | Create the new machine's own device identity, reconcile its recorded fleet name, and admit it under the LAN policy. Show the macOS hostname separately if it differs. | The same device has a consistent identity across discovery, membership, and System State. |
| Bootstrap credentials | A reachable member supplies the configured shared credentials over the existing protected peer connection. The bootstrap uses the app's bundled code and needs no Git, compiler, GitHub token, fleet secret, or completed tool installation first. | The bundle is received and bound to this device. |
| Materialize | Create `~/.secrets` with private permissions and write the required files safely. Verify each required file and retry any partial write. | The required credential files exist and their complete materialization is recorded; an accepted request alone is not success. |
| Activate services | Start the credential subscriber, fleet/node services, and agent communication automatically. Select the proper `aterm agh` identities from the synced files. | Sync is running, peer communication works, and routed authentication resolves the intended accounts. |
| Provision tools and source | Install and verify Apple Command Line Tools through the background service, provision the bundled tools, then obtain the authoritative development checkout once its credentials are ready. | Git and the compiler actually work; the correct source and toolchain are available. |
| Continue work across machines | Discover an already-running agent and route authorized work to an appropriate ready machine when that machine has the needed capabilities. Communication must not depend on the very credential handoff it is trying to repair. | A task is received, executed, and its result can be verified from this machine. |
| Publish when requested | Route an authorized release to an already-provisioned signing host, land the changes, publish the new version, and let the other Macs update through their normal updater. Keep device-specific signing identities on their owning machines. | Remote main contains the changes, a verified release exists, and peers report the updated version. |

These are dependencies, not one long serial script. Local preferences and developer-tool installation can run while credential sync is in progress. Private source access waits for credentials. A release waits for verified code and a ready publishing host. When a dependency becomes ready, its dependents start immediately without another command or click.

The bootstrap must not contain a circular requirement such as “receive the fleet credential before talking to the peer that supplies it,” “have GitHub credentials before obtaining bootstrap code,” or “publish the admission fix before this machine is allowed to ask an existing release agent to publish it.” Under the LAN policy, the existing peer transport must support discovery, bootstrap, and routing that initial request before service-specific credentials arrive.

A slow download is allowed to take time; starting it must be immediate and automatic. A sleeping peer, an unavailable vault, or a temporary network failure means waiting and retrying, not giving up after a fixed number of attempts. Reconnect, credential arrival, peer wake-up, and successful tool installation should trigger reconciliation at once. A restart resumes the recorded workflow and verifies interrupted operations instead of losing them or repeating completed side effects.

If macOS itself requires an unavoidable OS permission grant, isolate it to the task that needs it and let the rest continue. That platform requirement must not become a second LAN admission check or a blanket blocker for credential delivery.

System State should show verified progress, what is running, and each waiting task's actual dependency in a short line. For example: `Credentials — waiting for a LAN member` and `Private source — waiting for credentials`. Completed tasks count only after verification. Detailed diagnostics belong behind the row, not in paragraphs on the main panel. A missing credential in this flow is an initialization failure to repair, not a request for the person to fetch a token manually.

### What would prove this works

Start with a fresh machine on the LAN, an empty credential directory, no developer tools, and no previously granted aterm membership. Do not open System State or provide any input after launch. Verify that the machine is admitted, the shared credentials appear, `aterm agh` selects the intended identity, tools and source become ready, and the agent can contact an existing machine. Then request a release and verify automatic delegation to a provisioned release host, publication, and peer update.

Repeat that test with an older peer version, one unavailable peer, a network interruption, a partial file-write failure, and a process restart. Verify that ready work continues, incomplete work remains incomplete, and recovery requires no approval, phrase, “check again,” or repeated init command. Test real transitions, not only individual functions or mocked success counts.

Roll out a compatible member-side bootstrap before sending a new client that needs it. Keep at least one reachable bootstrap/release route working throughout an update. A mixed-version LAN must not strand a fresh machine behind the old approval gate.

### Process changes for the investigation

1. Check the cold-start contract first: actual credential files, actual routed identity, actual peer communication, and actual source access.
2. Trace the entire dependency chain before patching individual symptoms. Identify every independent admission implementation and every runtime version involved.
3. Maintain one persisted workflow with explicit dependencies and automatic event-driven continuation.
4. Verify the source change, the locally running behavior, and the deployed fleet behavior separately. Say which one is fixed in each report.
5. Treat the no-approval LAN policy as the requirement throughout the investigation. Do not quietly reintroduce manual keys, phrases, or approval as the proposed normal path.

## Remaining work

For this interrupted rollout, use the offered bootstrap keys or an already-provisioned release machine to land the changes in the development repository and publish through the normal signing pipeline. This is recovery from the present defect, not the intended onboarding process. After a member updates, verify that this Mac's running subscriber receives the credentials automatically and that `aterm agh` resolves the intended writer. Then verify fleet-wide rollout and replace the temporary pinned subscriber with the released program through the normal updater.

The private operational handoff records the subscriber backup and the stable executable used for this interrupted rollout.

## Final fresh attempt

The final requested attempt used the updated executable and fresh LAN discovery. It returned `Waiting for automatic LAN credential sync: the other Mac asked for something this Mac does not serve`. A subsequent check still found no `~/.secrets` directory, no `aterm agh` profiles, and anonymous routing. No further manual peer-provisioning attempt was made after that requested final try. The installed subscriber remains active and retries automatically.


## Naming mismatch

The machine's inventory label differed from its macOS discovery name and its pending admission name. The fallback selected a name from the operating system because no fleet label was available there. I should have reported these identities separately instead of treating them as interchangeable. Init should reconcile the inventory identity automatically while showing the OS hostname separately; it must not silently rename the operating system or claim an unverified inventory match.
