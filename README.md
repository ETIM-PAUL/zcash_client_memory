# zcash_client_memory

An in-memory wallet backend for the [Zcash](https://z.cash/) Rust crates. It
implements the wallet data-access traits from
[`zcash_client_backend`](https://crates.io/crates/zcash_client_backend) entirely
in memory, with no persistent database. This is useful for testing, for
ephemeral wallets, and for environments where a SQLite backend is unavailable
(such as WebAssembly).

This crate was extracted from the
[librustzcash](https://github.com/zcash/librustzcash) workspace, where it was
developed as the `zcash_client_memory` member crate.

## redbridge fork — NU6.2

This branch (`feat/snap-nu62`) is redbridge's maintainer fork of the standalone
crate, carrying ChainSafe's WebZjs Snap patches (originally authored against the
NU6.1 librustzcash fork) rebased onto the upstream standalone repository for the
NU6.2 upgrade.

- **Base:** upstream `zcash/zcash_client_memory` @ `ce94a19` ("Set up standalone
  repository").
- **Applied on top:** ChainSafe's 10 patch commits (`list_addresses`,
  `get_transparent_address_metadata`, `TransparentKeyScope` serialization,
  transaction-history accessors, sync optimisations, and test coverage). These
  fix panics and expose accessors the web wallet needs; upstream has not
  upstreamed them.
- **Consensus/crypto pin:** the `[patch.crates-io]` directives pin the
  librustzcash family to rev `97416db85512d7aa81d7f48a93f78d097bffd6b8`, which
  carries the NU6.2 consensus change (`Nu6_2 = 0x5437_f330`; mainnet activation
  3,364,600, testnet 4,052,000). Consumers of this fork **must** pin the same
  rev in their own root `[patch.crates-io]`, or cargo will build two copies of
  `zcash_primitives` and fail with trait mismatches.

The NU6.2-specific consumer changes (the `orchard 0.14` bump and the matching
`[patch.crates-io]` block) live in the consuming workspace, not in this crate.

## Dependencies

This crate depends on several crates that are developed in the librustzcash
workspace (`zcash_client_backend`, `zcash_keys`, `zcash_primitives`, and
others). Until matching releases of those crates are published to crates.io, the
manifest uses `[patch.crates-io]` directives that pin them to a specific
librustzcash revision. Building the crate therefore fetches those dependencies
from git.

## License

Licensed under either of

 * Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or
   http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or
   http://opensource.org/licenses/MIT)

at your option.

## Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.
