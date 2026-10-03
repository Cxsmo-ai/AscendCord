"""Derive the bundled app icon sizes from the source artwork.

Reads assets/brand/icon-styles/originals/<slug>.png (about 1254 px each) and writes
256 px window/taskbar/tray icons and 64 px picker thumbnails next to them. Only the
derived files are compiled into the app. Originals without transparency (a circular
badge painted on a flat square) get a smooth circular alpha mask so every style has the
same transparent corners.

Usage: python scripts/derive-app-icons.py
"""
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent.parent / "assets" / "brand" / "icon-styles"
SIZES = {"256": 256, "64": 64}
SUPERSAMPLE = 4


def badge_radius(image: Image.Image) -> float:
    """Half the width of the badge along the middle row, measured against the corner colour."""
    rgb = image.convert("RGB")
    width, height = rgb.size
    corner = rgb.getpixel((2, 2))
    row = height // 2

    def differs(x: int) -> bool:
        pixel = rgb.getpixel((x, row))
        return sum(abs(a - b) for a, b in zip(pixel, corner)) > 24

    left = next(x for x in range(width) if differs(x))
    right = next(x for x in range(width - 1, -1, -1) if differs(x))
    return (right - left + 1) / 2


def with_round_alpha(image: Image.Image) -> Image.Image:
    if image.mode == "RGBA" and image.getpixel((2, 2))[3] == 0:
        return image
    rgba = image.convert("RGBA")
    width, height = rgba.size
    radius = badge_radius(rgba)
    big = Image.new("L", (width * SUPERSAMPLE, height * SUPERSAMPLE), 0)
    cx, cy, r = width * SUPERSAMPLE / 2, height * SUPERSAMPLE / 2, radius * SUPERSAMPLE
    ImageDraw.Draw(big).ellipse((cx - r, cy - r, cx + r, cy + r), fill=255)
    rgba.putalpha(big.resize((width, height), Image.LANCZOS))
    return rgba


def main() -> None:
    originals = sorted((ROOT / "originals").glob("*.png"))
    classic = ROOT.parent / "classic.png"
    if not originals:
        raise SystemExit(f"no originals in {ROOT / 'originals'}")
    for folder in SIZES:
        (ROOT / folder).mkdir(exist_ok=True)
    for stem, source in [("classic", classic)] + [(path.stem, path) for path in originals]:
        image = Image.open(source).convert("RGBA") if stem == "classic" else with_round_alpha(Image.open(source))
        for folder, size in SIZES.items():
            image.resize((size, size), Image.LANCZOS).save(
                ROOT / folder / f"{stem}.png", optimize=True
            )
        print(f"{stem}: {image.size[0]} px -> {', '.join(SIZES)}")
    packaging_icons(with_round_alpha(Image.open(ROOT / "originals" / f"{PACKAGED}.png")))


# The style installed as the program icon (Windows .ico, Linux icon theme).
PACKAGED = "sakura"
REPO = ROOT.parent.parent.parent
LINUX_SIZES = [16, 22, 24, 32, 48, 64, 96, 128, 256, 512, 1024]
ICO_SIZES = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256]


def packaging_icons(image: Image.Image) -> None:
    hicolor = REPO / "packaging" / "linux" / "hicolor"
    for size in LINUX_SIZES:
        target = hicolor / f"{size}x{size}" / "apps" / "ascendcord.png"
        target.parent.mkdir(parents=True, exist_ok=True)
        image.resize((size, size), Image.LANCZOS).save(target, optimize=True)
    image.resize((256, 256), Image.LANCZOS).save(
        REPO / "packaging" / "windows" / "ascendcord.ico",
        sizes=[(size, size) for size in ICO_SIZES],
    )
    print(f"packaging icons: {PACKAGED} -> Linux {len(LINUX_SIZES)} sizes, Windows .ico")


if __name__ == "__main__":
    main()
