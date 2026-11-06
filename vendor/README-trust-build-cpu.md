# Trust native CPU leases

`trust-build-cpu` is original Andrew Yates code shared with the Trust compiler.
The complete Rust source and upstream README are byte-identical to Trust commit
`44356f40b6136199962c70933fa77d5012c3a5eb` (version 0.13.0). The upstream licence texts are retained.

The local manifest materializes workspace metadata/dependency requirements,
points the first-party hash dependency at `../trust-sha2`, preserves upstream
test targets, and declares a separate workspace. No runtime source is patched
locally. This keeps private and public source snapshots complete without an
unpublished git dependency or an alternate lease protocol.

Refresh all source, README and licence texts from one explicit qualified Trust
commit. Changes to the implementation belong in Trust first. Update this pin,
the manifest origin and source hashes together. Qualify changed behavior and
aterm-capacity consumers before selecting an import for release.

This import adds read-only native/retained CPU observation. Absence is a legacy
raw observation only; adopted mixed namespaces require the validated sidecar.
Held and crash-retained reservations remain unavailable until actual settlement.
Observing owners never grants CPU capacity or recovery/settlement authority.
No migration or host admission policy is activated by copying this source.
The optional worker membership helper follows its `jobserver` feature gate;
standalone native consumers keep the same allocation and ownership behavior.

Upstream SHA-256 values (Cargo.toml is the original workspace manifest):

- `Cargo.toml`: `0ca1082beda80c4a83f6730b3f65aa619e8ad294d5f05b1ff7020f0bae855183`
- `LICENSE-APACHE`: `62c7a1e35f56406896d7aa7ca52d0cc0d272ac022b5d2796e7d6905db8a3636a`
- `LICENSE-MIT`: `b71bd43a069ca0641a9ecfe585ca7b3c53b5cc1608f8b68321168698e28b5ea1`
- `README.md`: `932aefd5b5fcf1725268e2552379b232fc0cc0dc959b4f28f95531102be8a7dc`
- `src/admission_tests.rs`: `1061e9d528f2ac713e7dec0f3625c529b17ff16a41375801a40d7fd87f2d3ab3`
- `src/external.rs`: `aee07b822c09d10317e864b5885fb8dee23a30ca57f39330de03b59210c36f23`
- `src/external/tests.rs`: `8565f1fa2169239a9596160f519288960234c9c64267e2278617d2542b183dfe`
- `src/external_debt.rs`: `5c6fd2a880a6ca7143d94b9107106808563becd5ae96c5fbb027e6e4d75b86fe`
- `src/external_debt/tests.rs`: `ae6de4d49550a040be27657428e7741a2006808371a6a9b7efb39249f02271eb`
- `src/fence.rs`: `f2a5c2bef64e6d19526a864669b63ef736a1d04d435334b6866e325dab4bf0bb`
- `src/fence/tests.rs`: `4b7bc570cdfe7ef8bf254203faf129e14f26f11f7be24b9671f344187b721df3`
- `src/host_policy.rs`: `694db3f9795fa943b634da2df582d44480ca18c5309afd213ba4d04c5a587c3f`
- `src/kernel_memory.rs`: `defa36fe771c4e97026910386f9e6b25ccbd8269c3c8444d2d477a36b197c380`
- `src/legacy_retirement.rs`: `3e5af43d468a92110ce03d85d1d266092b6e3a5b0907d230f9fda0389f5f7d97`
- `src/legacy_retirement/boot.rs`: `39c4f6bc98a00bdb2292e214b17c5bb90ac9f0da917bfc0cab07fc1e6eacd2fd`
- `src/legacy_retirement/tests.rs`: `ba1a18231f2f6b4b7d5008a077e980fcb366a356526aa3a513205e4b19fd2a2b`
- `src/lib.rs`: `2c75022ef2eac6a4f49744560b4557e19b63b50626503a424981cd960f0b46a6`
- `src/monitor.rs`: `5ed46a3f00ab70d337967499e773d4593b1e6e7970639f30c97bc7603eb3522c`
- `src/monitor/tests.rs`: `db52543ddd0ce594ec0f19c14bc2509427fca68317763809e6ae68e79444ffec`
- `src/native_usage.rs`: `36d14caec23a36d668f33ecb4960294f17c46d70a7d365eccf55c68ea84ea395`
- `src/observation.rs`: `a5b5799e4707b1d3a872bb8777a68b6badb509480bd0b1648ee861891eb5adc2`
- `src/observation/tests.rs`: `91ccbc0458a37ad93880f2eab7f45eaba6332faba18559c72c6d86020c70414e`
- `src/process_cpu.rs`: `5581f4d136e6314216f1ae050e3d01d65ceda5b748df8093a9b88ad103324c87`
- `src/protocol.rs`: `d2cde35fece40f274ab2446191dabb0a40bca34890d217f50c813008208667aa`
- `src/recovery_material.rs`: `2258155f53a5e1e43b5604b13a5e910fdac4cc53940a57f765924566a7d460b5`
- `src/recovery_material/tests.rs`: `f7c54bf32df9eb94bcb4581bbef5f0133f7eb092c2aec6bfeedc0116f8164f0f`
- `src/retained.rs`: `1f84394e9026351c38c5f919a289090514888d2644d50517b2e8669e2f3ece60`
- `src/retained_recovery.rs`: `2e4944ae9af36723f7ecf14acc733a628cb3fc269438afc67dd81283d91a6106`
- `src/retained_tests.rs`: `10bae85147fc5e1e926178dfa060992e322155a58d8367c8ceae3985ba3aa150`
- `src/tests.rs`: `369dd9fe952fd32e3f3f7290b4209adb6b55ae59173a2af8bfe040f269b851a6`
- `src/unix.rs`: `985d60d95a3a584638eeb19cf19ab096b0b933cd695114e44c855ce3f9f460ee`
- `src/worker.rs`: `da0b6a3f06eebdee584c0ee2707a17249d63868c20b35ded58a54261b7472fd0`
- `src/worker/tests.rs`: `d81eec17a174af24c9a037f3c2341a47db8709c349cea91dcb4ff17552a71321`
