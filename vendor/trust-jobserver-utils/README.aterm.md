# Trust scheduling descriptor handoff

Source: Trust `263e40c542b1efd7e4464e9ef191cdea0b7add8f`,
`src/jobserver_utils`, MIT OR Apache-2.0. Runtime and test sources are unchanged.

The manifest records origin, makes this an isolated vendored package, and
removes the upstream workspace-relative path from its optional GNU client
dependency. Aterm disables default features: the host uses only the existing
std/libc descriptor transport, without constructing a GNU token client.

This is the same authenticated scoped CPU/GNU handoff used by native Targo.
It preserves actual open descriptions and never grants CPU, memory, execution,
or settlement from an advertisement. The host must bind an actual stopped
direct child before GO and retain endpoints through the child's scope closure.
