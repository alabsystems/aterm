# trust-resource-provenance

Read-only native discovery for the retained aterm resource custodian. Trust and
aterm share this reader and the version-one Book/Record/FileId wire schema.
The host remains responsible for publishing its immutable cache and account
instance book. This crate never creates or repairs account state.

`Store::open_account()?.connect_ancestor(deadline)` pins the original private
store directories, version, lock and immutable image; validates its complete
SHA-256; then connects to the recorded socket and checks native PID, platform
birth, UID, exec identity, loaded image path/inode, host, boot and bounded
ancestry. Socket and backing identities are checked again through the original
connection. A changed or ambiguous observation refuses. Original image handles
and complete metadata stamps avoid hashing the same immutable image at every
subsequent protocol check.

Callers keep `Host` alive, use its connected stream for a bounded exchange,
call `revalidate(deadline)` after the response, and send the protocol's ACK
before dropping it. The retained host keeps the connection open through that
ACK so Darwin's native peer credentials remain observable. The returned path
or JSON by itself grants no admission, memory entitlement, subgroup launch or
settlement. Those require the host's separate checked job/member protocol.

The native adapter supports macOS and Linux; unknown OS observations refuse.
The macOS source was execution-qualified with native identity/image/peer and
ancestry cases. Shared-reader cases exercise an actual cached ancestor with a
different loaded client image, post-reply image mutation, replaced backing,
strict duplicate-free book parsing, bounded private files and Apple ACLs.
Linux source has no execution qualification in this work.
