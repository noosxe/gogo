#!/usr/bin/env python3
"""Tests of scripts/linux-e2e-helper.py: python3 -I scripts/tests/linux-e2e-helper-test.py"""

import importlib.util
import os
import struct
import sys
import tempfile
import unittest
import zlib

sys.dont_write_bytecode = True  # no __pycache__ beside the helper
HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
_spec = importlib.util.spec_from_file_location(
    "helper", os.path.join(REPO, "scripts", "linux-e2e-helper.py")
)
helper = importlib.util.module_from_spec(_spec)
sys.modules["helper"] = helper
_spec.loader.exec_module(helper)


def chunk(kind, body):
    return (
        struct.pack(">I", len(body))
        + kind
        + body
        + struct.pack(">I", zlib.crc32(kind + body))
    )


def apply_filter(kind, line, prior, channels):
    out = bytearray()
    for i, value in enumerate(line):
        a = line[i - channels] if i >= channels else 0
        b = prior[i]
        c = prior[i - channels] if i >= channels else 0
        if kind == 0:
            pred = 0
        elif kind == 1:
            pred = a
        elif kind == 2:
            pred = b
        elif kind == 3:
            pred = (a + b) >> 1
        else:
            p = a + b - c
            pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
            pred = a if pa <= pb and pa <= pc else (b if pb <= pc else c)
        out.append((value - pred) & 255)
    return bytes(out)


def write_png(path, width, height, pixel, channels=3, filters=(0,)):
    """`pixel(x, y)` gives (r, g, b); rows cycle through `filters`."""
    raw = bytearray()
    prior = bytes(width * channels)
    for y in range(height):
        line = bytearray()
        for x in range(width):
            line.extend(pixel(x, y))
            if channels == 4:
                line.append(255)
        line = bytes(line)
        kind = filters[y % len(filters)]
        raw.append(kind)
        raw.extend(apply_filter(kind, line, prior, channels))
        prior = line
    colour = {3: 2, 4: 6}[channels]
    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n")
        f.write(chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, colour, 0, 0, 0)))
        f.write(chunk(b"IDAT", zlib.compress(bytes(raw))))
        f.write(chunk(b"IEND", b""))


MINIMAL = '''
gogid = 42
[[step]]
screenshot = "a"
'''


class Manifest(unittest.TestCase):
    def test_defaults(self):
        m = helper.parse_manifest(MINIMAL)
        self.assertEqual(m["gogid"], 42)
        self.assertEqual(m["title"], "42")
        self.assertEqual((m["window"]["width"], m["window"]["height"]), (1280, 800))
        self.assertEqual(m["window"]["min_fill"], {"w": 90.0, "h": 90.0})
        self.assertEqual(m["exit"], {"how": "signal", "wait_secs": 30.0})
        self.assertEqual(m["steps"], [{"screenshot": "a"}])

    def test_all_steps(self):
        m = helper.parse_manifest(
            """
            gogid = 1
            [window]
            resolution = "1920x1080"
            title = "^Game"
            min_fill = { w = 80 }
            [exit]
            how = "self"
            [[step]]
            wait = 1.5
            [[step]]
            wait_settled = 60
            [[step]]
            screenshot = "a"
            [[step]]
            click = { x = 50, y = 12.5 }
            [[step]]
            key = "Escape"
            [[step]]
            type = "hello"
            [[step]]
            screenshot = "b"
            min_fill = false
            [[step]]
            expect_change = { from = "a", to = "b", min = 3 }
            """
        )
        self.assertEqual(m["window"]["width"], 1920)
        self.assertEqual(m["window"]["min_fill"], {"w": 80.0, "h": 90.0})
        self.assertEqual(m["exit"]["how"], "self")
        self.assertEqual(m["steps"][3], {"click": {"x": 50.0, "y": 12.5}})
        self.assertEqual(m["steps"][6], {"screenshot": "b", "min_fill": False})
        self.assertEqual(
            m["steps"][7], {"expect_change": {"from": "a", "to": "b", "min": 3.0}}
        )

    def test_command_instead_of_gogid(self):
        m = helper.parse_manifest('command = ["xterm"]\n[[step]]\nwait = 1\n')
        self.assertEqual(m["command"], ["xterm"])
        self.assertIsNone(m["gogid"])

    def test_refusals(self):
        step = '[[step]]\nwait = 1\n'
        cases = {
            "both ids": 'gogid = 1\ncommand = ["x"]\n' + step,
            "no id": step,
            "gogid string": 'gogid = "1"\n' + step,
            "unknown key": 'gogid = 1\nnope = 1\n' + step,
            "no steps": "gogid = 1\n",
            "not toml": "gogid = = 1",
            "two actions": 'gogid = 1\n[[step]]\nwait = 1\nkey = "a"\n',
            "unknown action": 'gogid = 1\n[[step]]\njump = 1\n',
            "click over 100": 'gogid = 1\n[[step]]\nclick = { x = 101, y = 1 }\n',
            "click without y": 'gogid = 1\n[[step]]\nclick = { x = 1 }\n',
            "negative wait": 'gogid = 1\n[[step]]\nwait = -1\n',
            "bad resolution": 'gogid = 1\n[window]\nresolution = "big"\n' + step,
            "bad exit": 'gogid = 1\n[exit]\nhow = "maybe"\n' + step,
            "bad regex": 'gogid = 1\n[window]\ntitle = "("\n' + step,
            "shot path": 'gogid = 1\n[[step]]\nscreenshot = "../x"\n',
            "shot twice": 'gogid = 1\n[[step]]\nscreenshot = "a"\n[[step]]\nscreenshot = "a"\n',
            "change before shot": (
                'gogid = 1\n[[step]]\nexpect_change = { from = "a", to = "b" }\n'
                '[[step]]\nscreenshot = "a"\n[[step]]\nscreenshot = "b"\n'
            ),
            "fill on a click": 'gogid = 1\n[[step]]\nclick = { x = 1, y = 1 }\nmin_fill = false\n',
        }
        for name, text in cases.items():
            with self.subTest(name):
                with self.assertRaises(helper.ManifestError):
                    helper.parse_manifest(text)

    def test_repo_manifests_parse(self):
        e2e = os.path.join(REPO, "e2e")
        names = [n for n in os.listdir(e2e) if n.endswith(".toml")]
        self.assertIn("2086050016.toml", names)
        for name in names:
            with self.subTest(name), open(os.path.join(e2e, name), encoding="utf-8") as f:
                helper.parse_manifest(f.read())


class Png(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)

    def path(self, name="a.png"):
        return os.path.join(self.dir.name, name)

    def test_every_filter_decodes(self):
        pixel = lambda x, y: ((x * 13) % 256, (y * 7) % 256, (x * y) % 256)
        for channels in (3, 4):
            with self.subTest(channels=channels):
                write_png(self.path(), 17, 11, pixel, channels, filters=(0, 1, 2, 3, 4))
                w, h, c, rows = helper.read_png(self.path())
                self.assertEqual((w, h, c), (17, 11, channels))
                for y in range(h):
                    for x in range(w):
                        got = tuple(rows[y][x * c : x * c + 3])
                        self.assertEqual(got, pixel(x, y), (x, y))

    def test_refuses_other_files(self):
        with open(self.path(), "wb") as f:
            f.write(b"P6 1 1 255 ...")
        with self.assertRaises(ValueError):
            helper.read_png(self.path())


def image(width, height, pixel):
    rows = [
        bytes(v for x in range(width) for v in pixel(x, y)) for y in range(height)
    ]
    return width, height, 3, rows


BLACK = (0, 0, 0)
WHITE = (255, 255, 255)


class Analyze(unittest.TestCase):
    def test_all_black(self):
        stats = helper.analyze(image(40, 30, lambda x, y: BLACK))
        self.assertEqual(stats["nonblack_pct"], 0)
        self.assertIsNone(stats["box"])
        self.assertEqual(stats["box_w_pct"], 0)

    def test_noise_under_the_level_is_black(self):
        stats = helper.analyze(image(40, 30, lambda x, y: (helper.BLACK_LEVEL,) * 3))
        self.assertEqual(stats["nonblack_pct"], 0)

    def test_fills(self):
        stats = helper.analyze(image(40, 30, lambda x, y: WHITE))
        self.assertEqual(stats["nonblack_pct"], 100)
        self.assertEqual((stats["box_w_pct"], stats["box_h_pct"]), (100, 100))

    def test_800x600_in_the_top_left_of_1280x800(self):
        # #199's symptom, at a tenth of the size.
        stats = helper.analyze(
            image(128, 80, lambda x, y: WHITE if x < 80 and y < 60 else BLACK)
        )
        self.assertEqual(stats["box"], {"x": 0, "y": 0, "w": 80, "h": 60})
        self.assertEqual((stats["box_w_pct"], stats["box_h_pct"]), (62.5, 75))

    def test_pillarboxed(self):
        stats = helper.analyze(
            image(128, 80, lambda x, y: WHITE if 11 <= x < 117 else BLACK)
        )
        self.assertEqual(stats["box"]["x"], 11)
        self.assertEqual(stats["box_w_pct"], 82.81)
        self.assertEqual(stats["box_h_pct"], 100)

    def test_region_is_the_window(self):
        # A window at (10, 5) that the picture fills, on a black output.
        stats = helper.analyze(
            image(
                60,
                40,
                lambda x, y: WHITE if 10 <= x < 30 and 5 <= y < 25 else BLACK,
            ),
            (10, 5, 20, 20),
        )
        self.assertEqual(stats["nonblack_pct"], 100)
        self.assertEqual(stats["box"], {"x": 0, "y": 0, "w": 20, "h": 20})

    def test_region_outside(self):
        with self.assertRaises(ValueError):
            helper.analyze(image(10, 10, lambda x, y: WHITE), (20, 20, 5, 5))

    def test_changed(self):
        a = image(10, 10, lambda x, y: BLACK)
        self.assertEqual(helper.changed_pct(a, a), 0)
        half = image(10, 10, lambda x, y: WHITE if x < 5 else BLACK)
        self.assertEqual(helper.changed_pct(a, half), 50)
        slight = image(10, 10, lambda x, y: (helper.CHANGE_LEVEL,) * 3)
        self.assertEqual(helper.changed_pct(a, slight), 0)
        self.assertEqual(helper.changed_pct(a, image(5, 5, lambda x, y: BLACK)), 100)


def view(pid, name, width, height, **more):
    return {
        "type": "con",
        "pid": pid,
        "name": name,
        "visible": True,
        "rect": {"x": 0, "y": 0, "width": width, "height": height},
        "nodes": [],
        "floating_nodes": [],
        **more,
    }


class Views(unittest.TestCase):
    def tree(self, *views):
        return {
            "type": "root",
            "nodes": [
                {
                    "type": "output",
                    "nodes": [
                        {"type": "workspace", "nodes": list(views), "floating_nodes": []}
                    ],
                }
            ],
        }

    def test_largest_visible(self):
        got = helper.pick_view(
            self.tree(
                view(1, "small", 10, 10),
                view(2, "big", 100, 100),
                view(3, "hidden", 500, 500, visible=False),
                view(4, "empty", 0, 0),
            )
        )
        self.assertEqual(got["name"], "big")

    def test_title_regex(self):
        tree = self.tree(view(1, "Game", 10, 10), view(2, "Launcher", 100, 100))
        self.assertEqual(helper.pick_view(tree, "^Game")["name"], "Game")
        self.assertIsNone(helper.pick_view(tree, "^Nope"))

    def test_none(self):
        self.assertIsNone(helper.pick_view(self.tree()))

    def test_click_point(self):
        rect = {"x": 100, "y": 50, "width": 200, "height": 100}
        self.assertEqual(helper.click_point(rect, 50, 50), (200, 100))
        self.assertEqual(helper.click_point(rect, 0, 0), (100, 50))
        self.assertEqual(helper.click_point(rect, 100, 100), (299, 149))


class Wayland(unittest.TestCase):
    def test_globals(self):
        name = b"wl_seat\0"
        body = struct.pack("<II", 7, len(name)) + name + struct.pack("<I", 9)
        event = helper._wl_message(2, 0, body)
        other = helper._wl_message(3, 0, struct.pack("<I", 1))
        self.assertEqual(helper._wl_globals(other + event), {"wl_seat": (7, 9)})

    def test_string_is_padded(self):
        self.assertEqual(helper._wl_string("abc"), struct.pack("<I", 4) + b"abc\0")
        self.assertEqual(helper._wl_string("abcd"), struct.pack("<I", 5) + b"abcd\0\0\0\0")


if __name__ == "__main__":
    unittest.main()
