# Native resource member controller

One private binary protocol shared by the native Targo/Trust proxy adapter and
aterm's cached immutable helper. It carries no new-root admission authority.

The actual helper is a local child of the compiler/driver, remaining in its
existing owned group. Its canonical entry is formatted by `HelperFds::arguments`: the private entry
and protocol version are fixed, with explicit decimal control/proof socket
locators. Production inherits actual held source numbers (at least 5) with
child-only CLOEXEC clearing, without overwriting parent or runtime-private
descriptors. `HELPER_ARGUMENTS` retains the convenient fixed 3/4 fixture shape.
Descriptors 0/1/2 are target stdio. Delegated source descriptors start at 5 and
must be disjoint from the actual control/proof sockets BEFORE duplication. Target CPU/GNU
descriptors belong to a different child namespace and may start at 3. The helper
must close its duplicate stdio ends after attachment and never pass control or
proof endpoints to the target.

Request and lifecycle frames carry distinct original-root and member attempts.
The command environment is a complete lossless Unix-byte snapshot, never merged
with helper ambient state. Capabilities have no Debug implementation. The host
must authenticate actual native producer identity/image and current scope,
verify original retained memory and actual CPU/GNU descriptions, and validate
its own recipe before launch. Descriptor integers, decoded bytes and environment
locators alone establish no authority.

The producer challenge binds one pending request, actual helper native identity
and a fresh host nonce. The native parent must match its actual owned helper
before answering. The host must independently verify received socket credentials
and recheck native parent/helper relationships before accepting the answer.
Challenge/answer decoding alone proves no process identity.

`Attached` is not launch. `Started` uses BSD wall-microsecond birth on macOS or
start ticks on Linux, plus actual host GO clocks. `Finished` is sent only after
authenticated target scope retirement and resource return; target runtime
excludes cleanup. `Refused` requires affirmative prelaunch proof. `Unknown`
preserves available Started facts and never authorizes replay. Helper exit is
helper health, never the target result. Unknown delivery poisons the borrowed
logical unit/root execution context: do not return that unit to dispatch, retry
with an argfile or start another attempt while a target may still execute.
Independent positively owned units can finish cleanup. Cancellation and control EOF must cause
the actual custodian to cancel the retained target scope.

`Channel` frames with a big-endian u32 payload length; payloads use magic
`TRPCV003`, u64 big-endian tags/counts, bounded byte fields and exact trailing-byte
refusal. It uses per-call nonblocking Unix I/O and monotonic deadlines without
changing shared descriptor flags. Writes use per-call `MSG_NOSIGNAL` on Linux and macOS (the native Apple
SDK socket header defines it as suppressing SIGPIPE), without mutating socket
options or process-wide signal disposition. Any failed exchange
poisons the channel because partial delivery may already have happened.

This library does not activate v3, launch processes, validate CPU ownership or
establish a memory quota. Metadata syntax may omit a CPU descriptor, but the
host must still validate actual original cooperative CPU coverage for the
version-probe phase; syntax never authorizes an unbacked execution. Current production v3 policy rejects lifetime fences
and unsupported foreign compiler phases even though the codec retains explicit
bounded fence representation for protocol validation.

`Call` classifies the first private frame as `Member(Request)` or
`Context(ContextQuery)` before any effect. Context queries use the same private
producer challenge and exact cached host boundary. A host converts its checked
live Inspection to `ContextReply`; native startup must also check current
host/boot and actual external-debt/native-manifest binding before any CPU wait.
`Context::may_acquire_initial_cpu` expresses only the identity rule AFTER these
checks: the actual producer must equal the original root and its root subject.
Nested callers and registered members borrow the existing CPU capability.
Retained-fixed contexts cannot start shared native workers. Every context
carries the same original memory claim, never a new per-member reservation.
`JobBinding` owns the strict private environment-locator codec, shared with the
binary requests; a locator never proves custody.

Optional `client` support links the canonical provenance reader. Its `Client`
retains one original pinned host selection and queries context DIRECTLY as the
actual kernel peer: canonical `native-context` HELLO, private binary query and
response, post-response native revalidation, then ACK. No process is spawned
for read-only lookup. `ContextUnavailable` is distinct from transport ambiguity
and says nothing about original-root execution or resource release. The native
consumer must validate debt/manifest and initial-vs-borrowed CPU rules before
admission. The optional path has no role in the default host codec dependency
closure and does not activate v3.
