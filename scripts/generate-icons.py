# /// script
# requires-python = ">=3.10"
# dependencies = ["pillow==11.3.0"]
# ///
"""Generate the original window mark and all bundled mobile icon sizes.

Run with: uv run scripts/generate-icons.py
The geometry and palette below are the source of truth. Generated assets are
committed, so Pillow and Python are not required to build or use gpui-cli.
"""

import json
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parents[1]
BACKGROUND = "#CE5633"
FOREGROUND = "#FFF9EF"
ACCENT = "#713125"
RECTS = (
    (34, 34, 60, 60, 12, FOREGROUND),
    (44, 44, 10, 40, 3, ACCENT),
    (62, 44, 22, 16, 3, BACKGROUND),
    (62, 68, 22, 16, 3, BACKGROUND),
)


def write(path, contents):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(contents, encoding="utf-8")


def png(path, size, shape="square"):
    factor = 4
    side = size * factor
    image = Image.new("RGBA", (side, side))
    draw = ImageDraw.Draw(image)
    bounds = (0, 0, side - 1, side - 1)
    if shape == "circle":
        draw.ellipse(bounds, fill=BACKGROUND)
    elif shape == "rounded":
        draw.rounded_rectangle(bounds, radius=side * 0.22, fill=BACKGROUND)
    else:
        draw.rectangle(bounds, fill=BACKGROUND)
    # Adaptive foregrounds use the smaller safe-zone geometry. Legacy and
    # iOS icons expand the same mark to fill their already-masked icon tile.
    scale = side / 128 * 1.25
    for x, y, w, h, r, color in RECTS:
        left = side / 2 + (x - 64) * scale
        top = side / 2 + (y - 64) * scale
        draw.rounded_rectangle(
            (left, top, left + w * scale, top + h * scale),
            radius=r * scale,
            fill=color,
        )
    image = image.resize((size, size), Image.Resampling.LANCZOS)
    if shape == "square":
        image = image.convert("RGB")  # App Store icons must be opaque.
    path.parent.mkdir(parents=True, exist_ok=True)
    image.save(path, optimize=True)


def rect_path(rect):
    x, y, w, h, r, _ = rect
    return (
        f"M{x+r},{y}H{x+w-r}Q{x+w},{y} {x+w},{y+r}"
        f"V{y+h-r}Q{x+w},{y+h} {x+w-r},{y+h}"
        f"H{x+r}Q{x},{y+h} {x},{y+h-r}"
        f"V{y+r}Q{x},{y} {x+r},{y}Z"
    )


def vector(paths):
    return (
        '<?xml version="1.0" encoding="utf-8"?>\n'
        '<vector xmlns:android="http://schemas.android.com/apk/res/android"\n'
        '    android:width="108dp" android:height="108dp"\n'
        '    android:viewportWidth="128" android:viewportHeight="128">\n'
        + paths
        + "</vector>\n"
    )


def main():
    svg = (
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 128 128">\n'
        f'  <rect width="128" height="128" rx="28" fill="{BACKGROUND}"/>\n'
        '  <g transform="translate(64 64) scale(1.25) translate(-64 -64)">\n'
        + "".join(
            f'    <rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{r}" fill="{color}"/>\n'
            for x, y, w, h, r, color in RECTS
        )
        + "  </g>\n</svg>\n"
    )
    write(ROOT / "assets/app-icon.svg", svg)
    png(ROOT / "assets/app-icon.png", 512, "rounded")

    res = ROOT / "templates/android/gradle/app/src/main/res"
    for density, size in (("mdpi", 48), ("hdpi", 72), ("xhdpi", 96), ("xxhdpi", 144), ("xxxhdpi", 192)):
        png(res / f"mipmap-{density}/ic_launcher.png", size, "rounded")
        png(res / f"mipmap-{density}/ic_launcher_round.png", size, "circle")
    foreground = "".join(
        f'    <path android:fillColor="{rect[-1]}" android:pathData="{rect_path(rect)}"/>\n'
        for rect in RECTS
    )
    write(res / "drawable/ic_launcher_foreground.xml", vector(foreground))
    monochrome = (
        '    <path android:fillColor="#000000" android:fillType="evenOdd"\n'
        f'        android:pathData="{" ".join(rect_path(rect) for rect in RECTS)}"/>\n'
    )
    write(res / "drawable/ic_launcher_monochrome.xml", vector(monochrome))
    write(
        res / "values/icon_colors.xml",
        '<?xml version="1.0" encoding="utf-8"?>\n<resources>\n'
        f'    <color name="ic_launcher_background">{BACKGROUND}</color>\n</resources>\n',
    )
    for api in (26, 33):
        xml = (
            '<?xml version="1.0" encoding="utf-8"?>\n'
            '<adaptive-icon xmlns:android="http://schemas.android.com/apk/res/android">\n'
            '    <background android:drawable="@color/ic_launcher_background"/>\n'
            '    <foreground android:drawable="@drawable/ic_launcher_foreground"/>\n'
        )
        if api >= 33:
            xml += '    <monochrome android:drawable="@drawable/ic_launcher_monochrome"/>\n'
        xml += "</adaptive-icon>\n"
        for name in ("ic_launcher", "ic_launcher_round"):
            write(res / f"mipmap-anydpi-v{api}/{name}.xml", xml)

    assets = ROOT / "templates/ios/Assets.xcassets"
    png(assets / "AppIcon.appiconset/AppIcon-1024.png", 1024)
    images = []
    for scale in (1, 2, 3):
        name = "AppMark.png" if scale == 1 else f"AppMark@{scale}x.png"
        png(assets / "AppMark.imageset" / name, 128 * scale, "rounded")
        images.append({"filename": name, "idiom": "universal", "scale": f"{scale}x"})
    write(
        assets / "AppMark.imageset/Contents.json",
        json.dumps({"images": images, "info": {"author": "xcode", "version": 1}}, indent=2) + "\n",
    )
    print("Generated the window mark, Android launcher icons and iOS assets.")


if __name__ == "__main__":
    main()
