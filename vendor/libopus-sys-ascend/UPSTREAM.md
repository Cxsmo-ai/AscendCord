# Pinned libopus source

This binding builds Xiph.Org libopus **v1.6.1** from the vendored `opus/` directory.
The source is the signed upstream tag `v1.6.1` (commit
`a5d6c1b6f4e582df97390f9ac5c6e7c51cbffffe`). The downloaded GitHub source archive
SHA-256 was `BF0B97EC7A65890B8DB90EF94C4D6C18DE12584C3085031953A10986F5917745`.

The Rust FFI surface remains based on `libopus_sys` 0.3.3; the public C ABI used
by `opus2` is unchanged. `Cargo.toml` at the workspace root patches crates.io to
this package so builds do not depend on a system-installed Opus version.

To update, obtain a signed upstream Xiph release, verify the tag and archive
hash, replace only `opus/`, and run the `discord-voice` tests on Windows and Linux.
