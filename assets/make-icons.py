"""Собирает иконки AiPanel: веб-интерфейс и exe агента.

Рисуется одна геометрия в сетке 1024x1024, из неё получаются все размеры.
Мелкие размеры (до 24 px) рисуются упрощённо: на них не видны тонкие штрихи.

    python assets/make-icons.py

Что перезаписывается:
    crates/server/static/icon.svg            значок вкладки (масштабируемый)
    crates/server/static/favicon.ico         запасной вариант для старых браузеров
    crates/server/static/icon-192.png        значок на домашнем экране телефона
    crates/server/static/icon-512.png        значок в манифесте
    crates/server/static/apple-touch-icon.png  iOS, 180 px
    crates/agent/icon.ico                    значок exe агента
"""

from __future__ import annotations

import io
import struct
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent.parent
STATIC = ROOT / "crates/server/static"
AGENT = ROOT / "crates/agent"

GRID = 1024  # координатная сетка рисунка
SS = 4       # суперсэмплинг: рисуем крупнее и уменьшаем

BLUE_TOP = (61, 123, 240)
BLUE_BOTTOM = (40, 92, 200)
WHITE = (255, 255, 255)
WHITE_DIM = (255, 255, 255, 158)
GREEN = (53, 192, 107)

# Геометрия в сетке 1024. Одна и та же для растра и для SVG.
PLATE = dict(x0=28, y0=28, x1=996, y1=996, r=232)
FRAME = dict(x0=244, y0=286, x1=780, y1=738, r=92, stroke=58)
ROW_PAD = 30          # отступ строк от внутреннего края рамки
ROW_BAR_H = 100       # толщина полосы строки
ROW_GAP = 60          # просвет между строками
ROW_WIDTHS = (1.0, 0.66)
DOT_GAP = 30          # от точки состояния до полосы


def layout():
    """Возвращает прямоугольники рамки и строк в сетке 1024."""
    f = FRAME
    inner_x0 = f["x0"] + f["stroke"] + ROW_PAD
    inner_x1 = f["x1"] - f["stroke"] - ROW_PAD
    inner_y0 = f["y0"] + f["stroke"] + ROW_PAD
    inner_y1 = f["y1"] - f["stroke"] - ROW_PAD

    dot_d = ROW_BAR_H
    bar_x = inner_x0 + dot_d + DOT_GAP
    bar_w = inner_x1 - bar_x

    rows = []
    total = len(ROW_WIDTHS) * ROW_BAR_H + (len(ROW_WIDTHS) - 1) * ROW_GAP
    y = (inner_y0 + inner_y1 - total) / 2
    for i, k in enumerate(ROW_WIDTHS):
        rows.append(dict(
            dot=(inner_x0, y, inner_x0 + dot_d, y + dot_d),
            bar=(bar_x, y, bar_x + bar_w * k, y + ROW_BAR_H),
            active=(i == 0),
        ))
        y += ROW_BAR_H + ROW_GAP
    return dict(plate=PLATE, frame=f, rows=rows, inner=(inner_x0, inner_y0, inner_x1, inner_y1))


def lerp(a: int, b: int, t: float) -> int:
    return round(a + (b - a) * t)


def render(size: int, maskable: bool = False) -> Image.Image:
    """Значок в квадрате `size` с прозрачным фоном.

    `maskable` — плашка во весь квадрат без скруглений: такую иконку Android
    обрезает по своей форме, поэтому прозрачных полей быть не должно.
    """
    lay = layout()
    k = size * SS / GRID

    def box(b):
        return tuple(v * k for v in b)

    img = Image.new("RGBA", (size * SS, size * SS), (0, 0, 0, 0))

    # Плашка: плавный вертикальный градиент внутри скруглённого квадрата.
    p = dict(lay["plate"])
    if maskable:
        p.update(x0=0, y0=0, x1=GRID, y1=GRID, r=0)
    plate = Image.new("RGBA", img.size, (0, 0, 0, 0))
    pd = ImageDraw.Draw(plate)
    for y in range(round(p["y0"] * k), round(p["y1"] * k) + 1):
        t = (y - p["y0"] * k) / max(1.0, (p["y1"] - p["y0"]) * k)
        pd.line([(0, y), (img.size[0], y)],
                fill=(lerp(BLUE_TOP[0], BLUE_BOTTOM[0], t),
                      lerp(BLUE_TOP[1], BLUE_BOTTOM[1], t),
                      lerp(BLUE_TOP[2], BLUE_BOTTOM[2], t), 255))
    mask = Image.new("L", img.size, 0)
    ImageDraw.Draw(mask).rounded_rectangle(box((p["x0"], p["y0"], p["x1"], p["y1"])),
                                           radius=p["r"] * k, fill=255)
    img.paste(plate, (0, 0), mask)

    d = ImageDraw.Draw(img)
    f = lay["frame"]
    d.rounded_rectangle(box((f["x0"], f["y0"], f["x1"], f["y1"])), radius=f["r"] * k,
                        outline=WHITE, width=round(f["stroke"] * k))

    for row in lay["rows"]:
        x0, y0, x1, y1 = row["dot"]
        d.ellipse(box((x0, y0, x1, y1)), fill=GREEN if row["active"] else WHITE_DIM)
        x0, y0, x1, y1 = row["bar"]
        d.rounded_rectangle(box((x0, y0, x1, y1)), radius=(y1 - y0) / 2 * k,
                            fill=WHITE if row["active"] else WHITE_DIM)

    return img.resize((size, size), Image.LANCZOS)


def ico_bytes(frames: dict[int, Image.Image]) -> bytes:
    """ICO из PNG-кадров: заголовок 6 байт, по 16 байт на кадр, затем данные."""
    entries, blobs, offset = [], [], 6 + 16 * len(frames)
    for size, frame in sorted(frames.items()):
        buf = io.BytesIO()
        frame.save(buf, format="PNG", optimize=True)
        data = buf.getvalue()
        entries.append(struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32,
                                   len(data), offset))
        blobs.append(data)
        offset += len(data)
    return struct.pack("<HHH", 0, 1, len(frames)) + b"".join(entries) + b"".join(blobs)


def svg() -> str:
    """Тот же рисунок в SVG для значка вкладки."""
    lay = layout()
    p, f = lay["plate"], lay["frame"]
    out = [
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024" width="1024" height="1024">',
        '  <defs><linearGradient id="b" x1="0" y1="0" x2="0" y2="1">',
        f'    <stop offset="0" stop-color="#{BLUE_TOP[0]:02x}{BLUE_TOP[1]:02x}{BLUE_TOP[2]:02x}"/>',
        f'    <stop offset="1" stop-color="#{BLUE_BOTTOM[0]:02x}{BLUE_BOTTOM[1]:02x}{BLUE_BOTTOM[2]:02x}"/>',
        '  </linearGradient></defs>',
        f'  <rect x="{p["x0"]}" y="{p["y0"]}" width="{p["x1"] - p["x0"]}" height="{p["y1"] - p["y0"]}" rx="{p["r"]}" fill="url(#b)"/>',
        f'  <rect x="{f["x0"]}" y="{f["y0"]}" width="{f["x1"] - f["x0"]}" height="{f["y1"] - f["y0"]}" rx="{f["r"]}" fill="none" stroke="#fff" stroke-width="{f["stroke"]}"/>',
    ]
    dim = f"{WHITE_DIM[3] / 255:.3g}"
    for row in lay["rows"]:
        x0, y0, x1, y1 = row["dot"]
        color, opacity = ("#35c06b", "1") if row["active"] else ("#fff", dim)
        out.append(f'  <circle cx="{(x0 + x1) / 2:.0f}" cy="{(y0 + y1) / 2:.0f}" r="{(x1 - x0) / 2:.0f}" fill="{color}"' + (f' fill-opacity="{opacity}"' if opacity != "1" else '') + '/>')
        x0, y0, x1, y1 = row["bar"]
        out.append(f'  <rect x="{x0:.0f}" y="{y0:.0f}" width="{x1 - x0:.0f}" height="{y1 - y0:.0f}" rx="{(y1 - y0) / 2:.0f}" fill="#fff"' + (f' fill-opacity="{dim}"' if not row["active"] else '') + '/>')
    out.append('</svg>')
    return "\n".join(out) + "\n"


def write(path: Path, data: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
    print(f"{path.relative_to(ROOT)}  {len(data)} B")


def main() -> None:
    STATIC.mkdir(parents=True, exist_ok=True)

    sizes = [16, 20, 24, 32, 40, 48, 64, 128, 256]
    frames = {sz: render(sz) for sz in sizes}

    write(AGENT / "icon.ico", ico_bytes(frames))
    write(STATIC / "favicon.ico", ico_bytes({sz: frames[sz] for sz in (16, 32, 48)}))
    write(STATIC / "icon.svg", svg().encode("utf-8"))

    for name, size in (("icon-192.png", 192), ("icon-512.png", 512),
                       ("icon-maskable-512.png", 512), ("apple-touch-icon.png", 180)):
        buf = io.BytesIO()
        render(size, maskable=name.startswith("icon-maskable")).save(buf, format="PNG", optimize=True)
        write(STATIC / name, buf.getvalue())


if __name__ == "__main__":
    main()
