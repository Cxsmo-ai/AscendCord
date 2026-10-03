# Building on Windows

## Requirements

- Windows 10 or 11, x64 or ARM64
- [Rust](https://rustup.rs) via rustup. The repository pins its toolchain in
  `rust-toolchain.toml`, so the right version is installed on the first build.
- Visual Studio 2022 Build Tools with the **Desktop development with C++** workload
  (MSVC and the Windows SDK)
- [CMake](https://cmake.org/download/) on `PATH`
- Optional: [NSIS](https://nsis.sourceforge.io) to build the installer

No libclang or LLVM install is needed; the audio bindings are pregenerated.

## Build and run

```powershell
git clone https://github.com/Cxsmo-ai/AscendCord.git
cd AscendCord
cargo run --release
```

The executable is `target\release\ascendcord.exe`. It is self-contained; your data lives
in `%LOCALAPPDATA%\AscendCord` and the sign-in session in Windows Credential Manager.

## Package

```powershell
cargo xtask package
```

This stages a portable folder in `dist\` and, when NSIS is installed, a per-user installer
in `dist-installer\`. Releases are produced the same way by
`.github/workflows/release.yml` when a `v*` tag is pushed.

Set `ASCENDCORD_RELEASE_REPO=owner/repository` before building to enable in-app updates
from that repository's GitHub releases. Builds without it never check for updates.

## Checks

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

## Screenshots

The images in `docs/screenshots` come from the demo scenes, which use only made-up data:

```powershell
cargo build --release -p ascendcord --features demo
./scripts/capture-screenshots.ps1
```
