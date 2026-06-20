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
