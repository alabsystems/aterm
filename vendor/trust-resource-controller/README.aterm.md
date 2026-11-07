Exact Rust source from Trust b3046392072d1303de77d0ee99d98639a6cb883c, crates/trust-resource-controller. Only manifest libc redirects to maintained aterm-libc. Private codec and canonical native client; no allocation authority. Manifest redirects remain limited to maintained first-party dependencies.

The host selects the optional native client in its normal dependency so dev and test builds
share one library unit. Only development fixtures call it until the actual custody RPC is
qualified; selecting the feature activates no caller. The vendor's default remains std/libc.
