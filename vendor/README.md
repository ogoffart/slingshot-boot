# Vendored crates

## acpi 7.0.0-pre1

[acpi](https://github.com/rust-osdev/acpi), the AML interpreter that reads
the batteries in the firmware (`src/firmware/battery/acpi.rs`). Version 6.1
panics (`todo!()`) on AML that battery methods commonly use: storing into a
package element. 7.0 handles it. Both need a nightly compiler, for the
allocator API.

Patched with three pull requests, which explain the changes:
[#375](https://github.com/rust-osdev/acpi/pull/375) (`Divide`),
[#377](https://github.com/rust-osdev/acpi/pull/377) (fixes found
running laptops' battery methods) and
[#378](https://github.com/rust-osdev/acpi/pull/378) (builds with a stable
compiler without the `platform` feature, which Slingshot turns off). Some
fixes here are earlier versions of the reviewed ones. One more, not yet
proposed: a `Store` into a buffer keeps the buffer's length, as in ACPICA
(Dell's `_BIF` failed with `IndexOutOfBounds` without it). The tests and the
toolchain file are left out.

Use a release instead of this copy once it has all four.

## argon2 0.6.0

[argon2](https://github.com/RustCrypto/password-hashes), the key derivation
of LUKS2 key slots (`src/parse/luks.rs`). cryptsetup tunes it for a second or
two with several threads (its lanes, usually 4); computing the lanes one
after the other took 4 times as long. The crate computes them in parallel
only with rayon (its `parallel` feature), which needs `std`.

Changes:

- `LaneRunner` (`src/memory.rs`), an unsafe trait that computes the lanes of
  each slice, and `Argon2::hash_password_into_with_memory_and_lanes`, which
  uses one. Slingshot's runs them on other processors
  (`src/firmware/luks.rs`). To propose upstream.
- `LaneRunner::stop`, asked before each slice: the passphrase check can be
  cancelled without waiting for the whole derivation.
- The tests are left out.

Drop this copy once a release has a way to run the lanes without `std`.
