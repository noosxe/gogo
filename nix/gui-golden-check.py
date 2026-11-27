"""Compare a `gogo-gui --render-png` image with a golden (nix/gui.nix).

Usage: gui-golden-check.py <linux-e2e-helper.py> <golden.png> <actual.png>

The tolerance is GOLDEN_TOLERANCE in crates/gogo-gui-testkit/src/golden.rs. The
goldens are RGB and `--render-png` writes RGBA, so pixels are compared, not bytes.
"""

import importlib.util
import sys

LEVEL, SHARE, MAX, MEAN = 2, 0.0005, 8, 0.1


def pixels(helper, path):
    width, height, channels, rows = helper.read_png(path)
    out = []
    for row in rows:
        for x in range(0, width * channels, channels):
            pixel = row[x : x + channels]
            out.append(bytes(pixel) if channels == 4 else bytes(pixel) + b"\xff")
    return (width, height), out


def main(helper_path, golden_path, actual_path):
    spec = importlib.util.spec_from_file_location("helper", helper_path)
    helper = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(helper)
    golden_size, golden = pixels(helper, golden_path)
    actual_size, actual = pixels(helper, actual_path)
    if golden_size != actual_size:
        sys.exit(f"size {actual_size}, the golden's {golden_size}")
    diffs = [max(abs(a - b) for a, b in zip(g, p)) for g, p in zip(golden, actual)]
    share = sum(d > LEVEL for d in diffs) / len(diffs)
    largest, mean = max(diffs), sum(diffs) / len(diffs)
    print(f"{share:.4%} of pixels over the level, largest difference {largest}, mean {mean:.4f}")
    if share > SHARE or largest > MAX or mean > MEAN:
        sys.exit(f"{actual_path} does not match {golden_path}")


if __name__ == "__main__":
    main(*sys.argv[1:])
