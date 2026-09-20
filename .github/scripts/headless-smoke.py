#!/usr/bin/env python3
"""Capture renderer smoke cases and retain PNGs/logs for visual review.

Build headless_snapshot plus the widget document/render_test examples first.
The CI job supplies software Vulkan; this script also works with a local GPU.
"""

import argparse
import json
import os
from pathlib import Path
import re
import struct
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[2]
ANSI_ESCAPE = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")


def png_dimensions(path):
    """Reject missing, empty, or malformed PNG headers without a Pillow dependency."""
    data = path.read_bytes()
    if (
        len(data) < 45
        or data[:8] != b"\x89PNG\r\n\x1a\n"
        or data[8:16] != b"\x00\x00\x00\rIHDR"
        or b"IDAT" not in data
        or data[-12:] != b"\x00\x00\x00\x00IEND\xaeB`\x82"
    ):
        raise RuntimeError(f"missing PNG image data: {path}")
    width, height = struct.unpack(">II", data[16:24])
    if width < 2 or height < 2:
        raise RuntimeError(f"empty PNG dimensions: {width}x{height}")
    return width, height


def write_config(path, font_dir, line_height=1.0, explicit=True):
    family = "DejaVu Sans Mono" if explicit else "Ratty Missing Smoke Font"
    config = [
        "[window]",
        "opacity = 1.0",
        "[font]",
        f"family = {json.dumps(family)}",
        "size = 14",
        f"line_height = {line_height}",
    ]
    if explicit:
        for face, suffix in (
            ("regular", ""),
            ("bold", "-Bold"),
            ("italic", "-Oblique"),
            ("bold_italic", "-BoldOblique"),
        ):
            font = font_dir / f"DejaVuSansMono{suffix}.ttf"
            if not font.is_file():
                raise RuntimeError(f"required font is missing: {font}")
            config.append(f"{face} = {json.dumps(str(font))}")
    config.extend(("[cursor.model]", "visible = false"))
    path.write_text("\n".join(config) + "\n")


def capture(snapshot, output, name, config, command, *, scene=False, sends=(), after=3):
    image = output / f"{name}.png"
    log = output / f"{name}.log"
    image.unlink(missing_ok=True)
    args = [
        str(snapshot), "--config-file", str(config), "--out", str(image),
        "--after", str(after), "--timeout", "35", "--diagnose",
        "--width", "1100" if scene else "110",
        "--height", "760" if scene else "30",
        "--send-after", "1", "--send-interval", "0.25",
    ]
    if scene:
        args.append("--scene")
    for value in sends:
        args.extend(("--send", value))
    args.extend(("--", *map(str, command)))
    env = dict(os.environ, RUST_LOG="info", TERM="xterm-256color")
    # NO_COLOR would also reach the PTY child and suppress the very colours
    # these captures exercise. Strip log escape sequences only after capture.
    env.pop("NO_COLOR", None)
    with log.open("w") as stream:
        stream.write(json.dumps(args) + "\n")
        stream.flush()
        result = subprocess.run(
            args, cwd=ROOT, env=env, stdout=stream, stderr=subprocess.STDOUT, timeout=90,
        )
    if result.returncode:
        raise RuntimeError(f"exit code {result.returncode}; see {log}")
    width, height = png_dimensions(image)
    text = ANSI_ESCAPE.sub("", log.read_text(errors="replace"))
    stale = re.findall(r"stale-cell check:\s*(\d+) mismatches", text)
    if not stale or any(int(count) for count in stale):
        raise RuntimeError(f"missing or failing stale-cell diagnostic: {stale}; see {log}")
    delivered = [tuple(map(int, match)) for match in re.findall(r"sent PTY input (\d+) of (\d+)", text)]
    if delivered != [(index, len(sends)) for index in range(1, len(sends) + 1)]:
        raise RuntimeError(f"not all {len(sends)} scheduled inputs were sent: {delivered}; see {log}")
    if scene and (width, height) != (1100, 760):
        raise RuntimeError(f"unexpected scene dimensions: {width}x{height}")
    if name == "missing-family" and "using the generic monospace family" not in text:
        raise RuntimeError("unavailable-family fallback was not exercised")
    if name == "scene-child-exit" and "scene-eof-ok" not in text:
        raise RuntimeError("short-lived child's final output was not captured")
    # render_test right-aligns labels to 18 columns; diagnostics keep only the
    # first 12 characters of each colour run, including that left padding.
    if name == "styles-end" and f"{'font faces':>18}"[:12] not in text:
        raise RuntimeError("End input did not reach the text-style samples")
    if name == "document-inline" and "Ratty Editor" not in text:
        raise RuntimeError("document example did not render its editor")
    if name == "scroll-30-rows" and f"{'decomposed':>18}"[:12] not in text:
        raise RuntimeError("scroll input did not reach the combining-character samples")
    return {"width": width, "height": height, "stale_cells": 0}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--snapshot", type=Path, default=ROOT / "target/debug/examples/headless_snapshot")
    parser.add_argument("--document", type=Path, default=ROOT / "target/debug/examples/document")
    parser.add_argument("--render-test", type=Path, default=ROOT / "target/debug/examples/render_test")
    parser.add_argument("--font-dir", type=Path, default=Path("/usr/share/fonts/truetype/dejavu"))
    options = parser.parse_args()
    output = options.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    for binary in (options.snapshot, options.document, options.render_test):
        if not binary.is_file() or not os.access(binary, os.X_OK):
            parser.error(f"build the example first: {binary}")
    normal = output / "normal.toml"
    compact = output / "compact.toml"
    fallback = output / "fallback.toml"
    write_config(normal, options.font_dir.resolve())
    write_config(compact, options.font_dir.resolve(), line_height=0.85)
    write_config(fallback, options.font_dir.resolve(), explicit=False)

    # No sleep: these children exit before --after, leaving final PTY contents.
    sample = (
        "line-height\n┌────┬────┐\n│box │draw│\n└────┴────┘\n"
        "▁▂▃▄▅▆▇█ ▏▎▍▌▋▊▉█ ▖▗▘▝ ▙▚▛▜▞▟\n"
        "\x1b[1mbold\x1b[0m \x1b[3mitalic\x1b[0m "
        "\x1b[1;3mbold italic\x1b[0m\n"
    )
    text_command = [sys.executable, "-c", f"print({sample!r}, end='')"]
    cases = [
        ("font-natural", normal, text_command, {}),
        ("font-compact", compact, text_command, {}),
        ("missing-family", fallback, text_command, {}),
        ("scene-child-exit", normal, [sys.executable, "-c", "print('scene-eof-ok')"], {"scene": True}),
        ("document-inline", normal, [options.document.resolve()], {"scene": True, "after": 6}),
        ("styles-end", normal, [options.render_test.resolve()], {"sends": [r"\x1b[F"], "after": 5}),
        # Separate events allow the retained buffer to render all intermediate rows.
        ("scroll-30-rows", normal, [options.render_test.resolve()], {"sends": ["j"] * 30, "after": 12}),
    ]
    results = {}
    failures = []
    for name, config, command, settings in cases:
        print(f"Capturing {name}...", flush=True)
        try:
            results[name] = capture(options.snapshot.resolve(), output, name, config, command, **settings)
            print(f"PASS {name}: {results[name]}", flush=True)
        except (OSError, RuntimeError, subprocess.TimeoutExpired) as error:
            failures.append(f"{name}: {error}")
            print(f"FAIL {failures[-1]}", file=sys.stderr, flush=True)
    if "font-natural" in results and "font-compact" in results:
        full, short = results["font-natural"], results["font-compact"]
        if full["width"] != short["width"] or not 0 < short["height"] < full["height"]:
            failures.append(f"line_height=0.85 must reduce only PNG height: {full} -> {short}")
    report = {"captures": results, "failures": failures, "visual_review": "Inspect inline images, box joins, and text styles in the PNG artifacts."}
    (output / "results.json").write_text(json.dumps(report, indent=2) + "\n")
    if failures:
        raise SystemExit("\n".join(failures))


if __name__ == "__main__":
    main()
