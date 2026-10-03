# Arch Linux

## Install with the PKGBUILD

Every release has a `PKGBUILD` attached that builds that release from source, with the
source checksum filled in, and installs the binary, desktop entry and icons:

```sh
mkdir ascendcord && cd ascendcord
curl -LO https://github.com/Cxsmo-ai/AscendCord/releases/latest/download/PKGBUILD
makepkg -si
```

The template lives in `packaging/aur/PKGBUILD`.

The same PKGBUILD is meant for the AUR as `ascendcord`; with an AUR helper that is
`paru -S ascendcord` or `yay -S ascendcord` once it is published there.

## Build from a checkout

```sh
sudo pacman -S --needed base-devel rust clang cmake pkgconf \
  gtk3 webkit2gtk-4.1 libsoup3 gstreamer gst-plugins-base-libs gst-plugins-base \
  gst-plugins-good gst-plugin-pipewire alsa-lib libpulse \
  libxkbcommon libxkbcommon-x11 wayland libx11 libxcursor libxi libxrandr \
  vulkan-icd-loader xdg-desktop-portal
cargo run --release
```

Optional extras:

- `gnome-keyring` (or another Secret Service provider) to stay signed in between launches
- `gst-plugins-bad` for hardware screen-share encoding
- `gst-libav` for inline video playback

Data is stored in `~/.local/share/ascendcord`. Wayland desktops take the window icon from
the installed `org.ascendcord.AscendCord.desktop` entry, so the in-app icon picker changes
the tray and X11 window icon but not the Wayland taskbar icon.

## Native packages

On an Arch host `cargo xtask package --format arch` builds a binary package from the
current checkout (`deb`, `rpm`, `dir` and `appimage` work on their own distributions).
