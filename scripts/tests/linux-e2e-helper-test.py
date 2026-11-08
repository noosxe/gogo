#!/usr/bin/env python3
"""Tests of scripts/linux-e2e-helper.py: python3 -I scripts/tests/linux-e2e-helper-test.py"""

import contextlib
import importlib.util
import io
import os
import struct
import sys
import tempfile
import time
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
    def test_steering_steps(self):
        m = helper.parse_manifest(
            """
            gogid = 1
            [[step]]
            move_by = { x = -3000, y = 12.5 }
            [[step]]
            click_here = true
            """
        )
        self.assertEqual(
            m["steps"], [{"move_by": {"x": -3000, "y": 12.5}}, {"click_here": True}]
        )

    def test_steering_steps_are_checked(self):
        for step in (
            'move_by = { x = 1 }',
            'move_by = { x = 1, y = 2, z = 3 }',
            'move_by = { x = "1", y = 2 }',
            'click_here = false',
            'click_here = { x = 1, y = 2 }',
        ):
            with self.subTest(step), self.assertRaises(helper.ManifestError):
                helper.parse_manifest(f"gogid = 1\n[[step]]\n{step}\n")

    def test_defaults(self):
        m = helper.parse_manifest(MINIMAL)
        self.assertEqual(m["gogid"], 42)
        self.assertEqual(m["title"], "42")
        self.assertEqual((m["window"]["width"], m["window"]["height"]), (1280, 800))
        self.assertEqual(m["window"]["min_fill"], {"w": 90.0, "h": 90.0})
        self.assertEqual(m["exit"], {"how": "signal", "wait_secs": 30.0, "code": 0})
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

    def test_press_until(self):
        m = helper.parse_manifest(
            """
            gogid = 1
            [[step]]
            press_until = { key = "space", change = 20, every = 2, max = 90 }
            [[step]]
            press_until = { key = "Down", every = 0.5, max = 8, marked = { x = 9, y = 57.5, w = 21.5, h = 6.5, color = "#F0F040", min = 1 } }
            [[step]]
            press_until = { key = "Up", every = 1, max = 1, marked = { x = 0, y = 0, w = 100, h = 100, color = "#000000", tol = 5, min = 50 } }
            """
        )
        self.assertEqual(m["steps"][0], {"press_until": {"key": "space", "change": 20.0, "every": 2.0, "max": 90}})
        self.assertEqual(
            m["steps"][1]["press_until"]["marked"],
            {"x": 9.0, "y": 57.5, "w": 21.5, "h": 6.5, "color": "#f0f040", "tol": helper.MARKED_TOL, "min": 1.0},
        )
        self.assertEqual(m["steps"][2]["press_until"]["marked"]["tol"], 5)

    def test_wait_until(self):
        box = 'marked = { x = 1, y = 1, w = 5, h = 5, color = "#ffffff", min = 1 }'
        m = helper.parse_manifest(f"gogid = 1\n[[step]]\nwait_until = {{ every = 1, max = 3, {box} }}\n")
        self.assertNotIn("key", m["steps"][0]["wait_until"])
        for table in ("every = 1, max = 3, change = 5", f'key = "a", every = 1, max = 3, {box}', f"max = 3, {box}", f"every = 1, {box}"):
            with self.subTest(table), self.assertRaises(helper.ManifestError):
                helper.parse_manifest(f"gogid = 1\n[[step]]\nwait_until = {{ {table} }}\n")

    def test_press_until_is_checked(self):
        box = 'x = 1, y = 1, w = 5, h = 5, color = "#ffffff", min = 1'
        for table in (
            'key = "space", every = 1, max = 3',  # no condition
            f'key = "space", change = 5, marked = {{ {box} }}, every = 1, max = 3',  # two
            'key = "space", change = 5, max = 3',  # no every
            'key = "space", change = 5, every = 1',  # no limit: never guessed
            'key = "nope", change = 5, every = 1, max = 3',
            'change = 5, every = 1, max = 3',
            'key = "space", change = 0, every = 1, max = 3',
            'key = "space", change = 101, every = 1, max = 3',
            'key = "space", change = 5, every = 0, max = 3',
            'key = "space", change = 5, every = 1, max = 0',
            'key = "space", change = 5, every = 1, max = 1.5',
            'key = "space", change = 5, every = 1, max = 3, soon = true',
            'key = "a", every = 1, max = 3, marked = { x = 1, y = 1, w = 5, h = 5, min = 1 }',
            'key = "a", every = 1, max = 3, marked = { x = 1, y = 1, w = 5, h = 5, color = "red", min = 1 }',
            'key = "a", every = 1, max = 3, marked = { x = 1, y = 1, w = 0, h = 5, color = "#ffffff", min = 1 }',
            'key = "a", every = 1, max = 3, marked = { x = 90, y = 1, w = 20, h = 5, color = "#ffffff", min = 1 }',
            'key = "a", every = 1, max = 3, marked = { x = 1, y = 1, w = 5, h = 5, color = "#ffffff", min = 0 }',
            f'key = "a", every = 1, max = 3, marked = {{ {box}, tol = 300 }}',
        ):
            with self.subTest(table), self.assertRaises(helper.ManifestError):
                helper.parse_manifest(f"gogid = 1\n[[step]]\npress_until = {{ {table} }}\n")

    def test_marked_pct_is_of_the_region_in_the_window(self):
        # A 100x100 output, the window at (0, 50) and 100x50; its top half blue-ish.
        def pixel(x, y):
            return (10, 20, 240) if 50 <= y < 75 else (0, 0, 0)

        img = image(100, 100, pixel)
        rect = {"x": 0, "y": 50, "width": 100, "height": 50}
        marked = {"x": 0, "y": 0, "w": 100, "h": 100, "color": "#0000ff", "tol": 40, "min": 1}
        self.assertEqual(helper.marked_pct(img, rect, marked), 50.0)
        self.assertEqual(helper.marked_pct(img, rect, dict(marked, h=50)), 100.0)
        self.assertEqual(helper.marked_pct(img, rect, dict(marked, y=50, h=50)), 0.0)
        self.assertEqual(helper.marked_pct(img, rect, dict(marked, tol=5)), 0.0)

    def test_env_and_exit_code(self):
        m = helper.parse_manifest(
            'gogid = 1\n[env]\nDXVK_LOG_LEVEL = "info"\n[exit]\ncode = 255\n[[step]]\nwait = 1\n'
        )
        self.assertEqual(m["env"], {"DXVK_LOG_LEVEL": "info"})
        self.assertEqual(m["exit"]["code"], 255)
        with self.assertRaises(helper.ManifestError):
            helper.parse_manifest('gogid = 1\n[env]\nX = 1\n[[step]]\nwait = 1\n')

    def test_override(self):
        m = helper.parse_manifest(
            'gogid = 1\n[override]\nextra_args = ["--launcher-skip", "a b"]\n'
            '[override.env]\nDXVK_HUD = "fps"\n[[step]]\nwait = 1\n'
        )
        self.assertEqual(
            m["override"],
            {"env": {"DXVK_HUD": "fps"}, "extra_args": ["--launcher-skip", "a b"]},
        )
        m = helper.parse_manifest(MINIMAL)
        self.assertEqual(m["override"], {"env": {}, "extra_args": None})
        step = "[[step]]\nwait = 1\n"
        for bad in (
            'gogid = 1\n[override]\nargs = ["-x"]\n',
            'gogid = 1\n[override]\nextra_args = []\n',
            'gogid = 1\n[override]\nextra_args = "-x"\n',
            'gogid = 1\n[override.env]\n"A-B" = "1"\n',
            'gogid = 1\n[override.env]\nA = 1\n',
            'command = ["x"]\n[override]\nextra_args = ["-x"]\n',
        ):
            with self.subTest(bad):
                with self.assertRaises(helper.ManifestError):
                    helper.parse_manifest(bad + step)

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


class WhoseProcess(unittest.TestCase):
    EXE = "games/1741027632/prefix/drive_c/GOG/Right and Down/Right and Down.exe"

    def test_the_exe_and_its_folder(self):
        for argv0 in (
            r"C:\GOG\Right and Down\Right and Down.exe",
            r"c:\gog\right and down\Tools\helper.exe",
            r"Z:\home\x\prefix\drive_c\GOG\Right and Down\Right and Down.exe",
        ):
            self.assertTrue(helper.belongs_to_game(argv0, self.EXE), argv0)

    def test_wines_helpers_arent_the_game(self):
        for argv0 in (
            r"C:\windows\system32\explorer.exe",
            r"C:\windows\system32\steam.exe",
            r"Z:\store\xalia\xalia.exe",
            "/nix/store/x/bin/python3",
            "",
        ):
            self.assertFalse(helper.belongs_to_game(argv0, self.EXE), argv0)

    def test_no_exe_known(self):
        self.assertFalse(helper.belongs_to_game(r"C:\GOG\Right and Down\a.exe", None))

    def test_exe_outside_a_folder_is_matched_by_name_only(self):
        exe = "games/1/prefix/drive_c/game.exe"
        self.assertIsNone(helper.game_dir(exe))
        self.assertTrue(helper.belongs_to_game(r"C:\game.exe", exe))
        self.assertFalse(helper.belongs_to_game(r"C:\windows\explorer.exe", exe))

    def test_exe_line(self):
        self.assertEqual(helper.parse_exe("# launcher: 2\n# exe: a/b.exe\n# args: \n"), "a/b.exe")
        self.assertIsNone(helper.parse_exe("no header"))


class Parsers(unittest.TestCase):
    def test_xwayland_displays(self):
        self.assertEqual(
            helper.xwayland_displays(
                [["/nix/store/x/bin/Xwayland", ":1", "-rootless"], ["sway", ":9"], []]
            ),
            [":1"],
        )

    def test_support_installer(self):
        start = "note: running GOG support installer into the game dir\n"
        self.assertIsNone(helper.parse_support_installer("nothing"))
        for line, outcome, code in (
            ("GOG support installer finished (exit 0)", "finished", 0),
            ("GOG support installer declined (exit 3; install-time", "declined", 3),
            ("GOG support installer still running after 180 s, stopped", "stopped", None),
            ("", "unknown", None),
        ):
            self.assertEqual(
                helper.parse_support_installer(start + line), {"outcome": outcome, "exit": code}
            )

    def test_installer_problem(self):
        with tempfile.TemporaryDirectory() as d:
            exe = os.path.join(d, "setup_x.exe")
            with open(exe, "w") as f:
                f.write("x")
            self.assertIsNone(helper.installer_problem(exe, os.ST_RDONLY))
            self.assertIn("isn't read-only", helper.installer_problem(exe, 0))
            self.assertIn("isn't read-only", helper.installer_problem(exe))  # a real rw dir
            self.assertIn("can't read", helper.installer_problem(exe + ".no", os.ST_RDONLY))
        self.assertIn("/gone", helper.installer_problem("/gone/setup_x.exe"))

    def test_tmp_foreign(self):
        with tempfile.TemporaryDirectory() as d:
            os.makedirs(os.path.join(d, "repo/target/run"))
            os.makedirs(os.path.join(d, "repo/target/results"))
            roots = [os.path.join(d, "repo/target/run"), os.path.join(d, "repo/target/results")]
            top = sorted(os.listdir(d))
            self.assertEqual(helper.tmp_foreign(top + [".X11-unix"], roots, d), [])
            # A bind root's own content is a mount, not looked into.
            open(os.path.join(roots[0], "x"), "w").close()
            self.assertEqual(helper.tmp_foreign(top, roots, d), [])
            # A stray beside the path to a root, or none of the roots: a leak.
            open(os.path.join(d, "repo/secret"), "w").close()
            self.assertEqual(helper.tmp_foreign(top, roots, d), ["repo/secret"])
            os.makedirs(os.path.join(d, "other"))
            self.assertEqual(
                helper.tmp_foreign(sorted(os.listdir(d)), roots, d), ["other", "repo/secret"]
            )
            self.assertEqual(helper.tmp_foreign(["other"], [], d), ["other"])

    def test_install_logs(self):
        with tempfile.TemporaryDirectory() as d:
            self.assertEqual(helper.install_logs(os.path.join(d, "none")), [])
            for game in ("1423049311", ".build-x-1", "nothing"):
                os.makedirs(os.path.join(d, game))
            for game in ("1423049311", ".build-x-1"):
                os.makedirs(os.path.join(d, game, "install", "support"))
                for name in ("inno.log", "keep.txt"):
                    open(os.path.join(d, game, "install", name), "w").close()
            self.assertEqual(
                helper.install_logs(d),
                [
                    ("inno.log", os.path.join(d, ".build-x-1", "install", "inno.log")),
                    ("inno.log", os.path.join(d, "1423049311", "install", "inno.log")),
                ],
            )

    def test_pactl_info(self):
        self.assertEqual(
            helper.parse_pactl_info(
                "Server String: /run/e2e-runtime/pulse/native\nDefault Sink: e2e-null\n"
            ),
            {"server": "/run/e2e-runtime/pulse/native", "default_sink": "e2e-null"},
        )
        self.assertEqual(helper.parse_pactl_info(""), {"server": None, "default_sink": None})

    def test_audio_streams(self):
        log = (
            'I: [p] sink-input.c: Created input 0 "pacat" on e2e-null with sample spec\n'
            'I: [p] sink-input.c: Created input 1 "Game" on e2e-null with sample spec\n'
        )
        self.assertEqual(helper.audio_streams(log), ["pacat", "Game"])


class BuildCommand(unittest.TestCase):
    STEP = 'screenshot = "a"'

    def test_beside_command(self):
        m = helper.parse_manifest(
            f'command = ["x"]\nbuild_command = ["y"]\n[[step]]\n{self.STEP}\n'
        )
        self.assertEqual(m["build_command"], ["y"])

    def test_not_for_a_game(self):
        with self.assertRaises(helper.ManifestError):
            helper.parse_manifest(f'gogid = 1\nbuild_command = ["y"]\n[[step]]\n{self.STEP}\n')


KEYMAP = """xkb_keymap {
xkb_keycodes "evdev+aliases(qwerty)" {
	minimum = 8;
	<ESC>                = 9;
	<AE01>               = 10;
	<AC01>               = 38;
	<FK01>               = 67;
	alias <LatA>         = <AC01>;
};
xkb_symbols "pc+us+inet(evdev)" {
	key <ESC>                {	[          Escape ] };
	key <AE01>               {	[               1,          exclam ] };
	key <AC01>               {	[               a,               A ] };
	key <FK01>               {
		type= "CTRL+ALT",
		symbols[1]= [              F1,  XF86Switch_VT_1 ]
	};
};
};
"""


class Keyboard(unittest.TestCase):
    def test_keymap_levels(self):
        self.assertEqual(
            helper.keymap_levels(KEYMAP),
            {1: ["Escape"], 2: ["1", "exclam"], 30: ["a", "A"], 59: ["F1", "XF86Switch_VT_1"]},
        )

    def test_mismatches_name_the_key(self):
        bad = dict(helper.keymap_mismatches(KEYMAP))
        for ok in ("Escape", "1", "a", "F1", "exclam", "A"):
            self.assertNotIn(ok, bad)
        self.assertIn("space", bad)  # not in this keymap
        wrong = KEYMAP.replace("<AC01>               = 38", "<AC01>               = 39")
        self.assertIn("a", dict(helper.keymap_mismatches(wrong)))

    def test_agrees_with_the_real_keymap(self):
        """The flake's xkbcli: on PATH in a run (and via .#linux-e2e-tools)."""
        import shutil
        import subprocess

        xkbcli = shutil.which("xkbcli")
        if not xkbcli:
            self.skipTest("no xkbcli on PATH")
        out = subprocess.run(
            [xkbcli, "compile-keymap", *helper.KEYMAP_ARGS],
            capture_output=True, text=True, check=True,
        ).stdout  # fmt: skip
        self.assertEqual(helper.keymap_mismatches(out), [])

    def test_evdev_codes(self):
        k = helper.KEYS
        self.assertEqual(
            (k["Escape"], k["1"], k["0"], k["q"], k["Return"], k["a"], k["z"], k["space"]),
            (1, 2, 11, 16, 28, 30, 44, 57),
        )
        self.assertEqual(
            (k["F1"], k["F10"], k["F11"], k["Up"], k["Delete"]), (59, 68, 87, 103, 111)
        )

    def test_type_chars(self):
        t = helper.TYPE_CHARS
        self.assertEqual(t["q"], (16, False))
        self.assertEqual(t["Q"], (16, True))
        self.assertEqual(t[" "], (57, False))
        self.assertEqual(t["!"], (2, True))
        self.assertEqual(t["?"], (53, True))
        self.assertEqual(t["\n"], (28, False))
        printable = [chr(n) for n in range(0x20, 0x7F)]
        self.assertEqual([c for c in printable if c not in t], [])

    def test_manifest_checks_keys(self):
        base = 'command = ["x"]\n[[step]]\n'
        steps = helper.parse_manifest(base + 'key = "space"\n')["steps"]
        self.assertEqual(steps, [{"key": "space"}])
        for bad in ('key = "spacebar"', 'key = "A"', 'type = "caf\u00e9"'):
            with self.subTest(bad), self.assertRaises(helper.ManifestError):
                helper.parse_manifest(base + bad + "\n")

    def test_key_messages(self):
        self.assertEqual(
            helper.keyboard_key(57, True, 2**32 + 7),
            helper._wl_message(6, 1, struct.pack("<III", 7, 57, 1)),
        )
        self.assertEqual(
            helper.keyboard_modifiers(1), helper._wl_message(6, 2, struct.pack("<IIII", 1, 0, 0, 0))
        )

    def test_wait_done(self):
        import socket

        a, b = socket.socketpair()
        with a, b:
            b.sendall(helper._wl_message(2, 0, b"x" * 4) + helper._wl_message(7, 0, b"\0" * 4))
            self.assertTrue(helper._wl_wait_done(a, 7, "it"))
            b.sendall(helper._wl_message(1, 0, struct.pack("<III", 6, 1, 0)))
            with self.assertRaises(RuntimeError):
                helper._wl_wait_done(a, 9, "it")


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

    def test_pointer_move_to(self):
        self.assertEqual(
            helper.pointer_move_to(640, 400, 1280, 800, 7),
            helper._wl_message(5, 1, struct.pack("<IIIII", 7, 640, 400, 1280, 800))
            + helper._wl_message(5, 4),
        )

    def test_pointer_move_by(self):
        # wl_fixed_t: signed 24.8. A frame ends it, or no client sees the motion.
        self.assertEqual(
            helper.pointer_move_by(-3000, 1.5, 7),
            helper._wl_message(5, 0, struct.pack("<Iii", 7, -3000 * 256, 384))
            + helper._wl_message(5, 4),
        )

    def test_pointer_scroll(self):
        self.assertEqual(
            helper.pointer_scroll(-1, 7),
            helper._wl_message(5, 5, struct.pack("<I", 0))
            + helper._wl_message(5, 7, struct.pack("<IIii", 7, 0, -15 * 256, -1))
            + helper._wl_message(5, 4),
        )

    def test_manifest_scroll(self):
        base = 'command = ["x"]\n[[step]]\n'
        self.assertEqual(helper.parse_manifest(base + "scroll = -3\n")["steps"], [{"scroll": -3}])
        for bad in ("scroll = 0", "scroll = 1.5", "scroll = true", 'scroll = "down"'):
            with self.subTest(bad), self.assertRaises(helper.ManifestError):
                helper.parse_manifest(base + bad + "\n")

    def test_pointer_time_wraps(self):
        self.assertEqual(
            helper.pointer_move_by(0, 0, 2**32 + 5), helper.pointer_move_by(0, 0, 5)
        )


class GpuChoice(unittest.TestCase):
    NV = {
        "node": "/dev/dri/renderD129",
        "label": "nvidia",
        "expect": "nvidia|geforce|quadro|rtx|gtx",
        "held": "/dev/nvidia0",
        "compositor": False,
    }
    AMD = {
        "node": "/dev/dri/renderD128",
        "label": "radv",
        "expect": "amd|radv|radeon",
        "held": "/dev/dri/renderD128",
        "compositor": True,
    }
    NV_NAME = "NVIDIA GeForce RTX 4090"
    AMD_NAME = "AMD Ryzen 7 9800X3D 8-Core Processor (RADV RAPHAEL_MENDOCINO)"
    # As DXVK 3.1.1 logs it, cut to what matters.
    DXVK_LOG = """info:  Found device: NVIDIA GeForce RTX 4090 (NVIDIA 595.112.7)
info:  Found device: AMD Ryzen 7 9800X3D 8-Core Processor (RADV RAPHAEL_MENDOCINO) (radv 26.2.6)
info:  Found device: llvmpipe (LLVM 21.1.8, 256 bits) (llvmpipe 26.2.6)
info:    Skipping: Software driver
info:  Found device: NVIDIA GeForce RTX 4090 (NVIDIA 595.112.7)
info:  Creating device:
info:  NVIDIA GeForce RTX 4090:
info:    Driver   : NVIDIA 595.112.7
info:  Creating device:
info:  NVIDIA GeForce RTX 4090:
"""

    def test_dxvk_found_as_enumerated_and_created_once(self):
        got = helper.parse_dxvk(self.DXVK_LOG)
        self.assertEqual(
            got["found"], [self.NV_NAME, self.AMD_NAME, "llvmpipe (LLVM 21.1.8, 256 bits)"]
        )
        self.assertEqual(got["created"], [self.NV_NAME])

    def test_dxvk_silent_without_its_log(self):
        self.assertEqual(helper.parse_dxvk("wine: starting\n"), {"found": [], "created": []})

    def test_fdinfo_counts_engines_and_vram(self):
        text = (
            "pos:\t0\ndrm-driver:\tamdgpu\ndrm-engine-gfx:\t1500 ns\n"
            "drm-engine-compute:\t20 ns\ndrm-total-vram:\t4096 KiB\ndrm-memory-vram:\t4096 KiB\n"
        )
        self.assertEqual(helper.parse_fdinfo(text), {"engine_ns": 1520, "vram_kib": 4096})

    def test_fdinfo_idle_amd_fd_is_no_engine_time(self):
        text = "drm-driver:\tamdgpu\ndrm-total-vram:\t12 KiB\n"
        self.assertEqual(helper.parse_fdinfo(text), {"engine_ns": 0, "vram_kib": 12})

    def test_fdinfo_nvidia_counts_nothing(self):
        self.assertIsNone(helper.parse_fdinfo("pos:\t0\nflags:\t02\nmnt_id:\t30\n"))

    def test_holding_both_rendering_on_the_one_dxvk_created(self):
        gpus = [self.AMD, self.NV]
        fds = {"game.exe": ["/dev/dri/renderD128", "/dev/dri/renderD129", "/dev/nvidia0"]}
        self.assertEqual(helper.held_nodes(fds, gpus), [self.AMD["node"], self.NV["node"]])
        dxvk = helper.parse_dxvk(self.DXVK_LOG)
        use = {"/dev/dri/renderD128": {"engine_ns": 0, "vram_kib": 12}}
        self.assertEqual(helper.renders_on(gpus, dxvk, use), [self.NV["node"]])

    def test_engine_time_shows_rendering_with_no_dxvk_log(self):
        gpus = [self.AMD, self.NV]
        use = {"/dev/dri/renderD128": {"engine_ns": 7, "vram_kib": 900}}
        self.assertEqual(
            helper.renders_on(gpus, {"found": [], "created": []}, use), [self.AMD["node"]]
        )

    def test_dxvks_device_outweighs_crumbs_of_engine_time(self):
        gpus = [self.AMD, self.NV]
        dxvk = helper.parse_dxvk(self.DXVK_LOG)
        use = {"/dev/dri/renderD128": {"engine_ns": 1798345, "vram_kib": 28852}}
        self.assertEqual(helper.renders_on(gpus, dxvk, use), [self.NV["node"]])

    def test_nothing_shows_rendering(self):
        self.assertEqual(
            helper.renders_on([self.AMD, self.NV], {"found": [], "created": []}, {}), []
        )

    def test_vulkan_one_gpu_lists_only_it(self):
        expect = self.NV["expect"]
        self.assertTrue(helper.vulkan_devices_ok([self.NV_NAME], expect, [self.NV]))
        self.assertFalse(
            helper.vulkan_devices_ok([self.NV_NAME, "llvmpipe (LLVM)"], expect, [self.NV])
        )
        self.assertFalse(helper.vulkan_devices_ok([], expect, [self.NV]))

    def test_vulkan_software_mode_has_no_gpus(self):
        self.assertTrue(helper.vulkan_devices_ok(["llvmpipe (LLVM)"], "llvmpipe", []))

    def test_vulkan_two_gpus_need_both_and_allow_the_software_driver(self):
        gpus = [self.AMD, self.NV]
        expect = "|".join(g["expect"] for g in gpus)
        both = [self.AMD_NAME, self.NV_NAME, "llvmpipe (LLVM 21.1.8, 256 bits)"]
        self.assertTrue(helper.vulkan_devices_ok(both, expect, gpus))
        self.assertFalse(helper.vulkan_devices_ok([self.NV_NAME], expect, gpus))
        self.assertFalse(helper.vulkan_devices_ok([*both, "Intel(R) Arc"], expect, gpus))


class TreeCheck(unittest.TestCase):
    """tree_problems: may an installed base be copied into a run (#240)."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = self.tmp.name
        self.tree = os.path.join(self.root, "cache", "installed", "x-key", "tree")
        self.run = os.path.join(self.root, "runs", "reuse-x")
        os.makedirs(os.path.join(self.tree, "gogo", "games"))
        self.uid = os.getuid()

    def tearDown(self):
        self.tmp.cleanup()

    def link(self, target, name="l"):
        os.symlink(target, os.path.join(self.tree, "gogo", name))

    def problems(self):
        return helper.tree_problems(self.tree, self.run, [self.root], self.uid)

    def test_plain_tree_and_links_inside_the_run_pass(self):
        self.link(os.path.join(self.run, "store", "runtimes"), "runtimes")
        self.link("games", "rel")
        self.link("/", "z")  # a prefix's z: sees only the sandbox's root
        self.assertEqual(self.problems(), [])

    def test_absolute_link_into_the_base_is_refused(self):
        self.link(os.path.join(self.tree, "gogo", "games"))
        self.assertIn("a link out of the run", self.problems()[0])

    def test_relative_link_climbing_out_of_the_run_is_refused(self):
        # Seen from the run's path: gogo/../../../cache is under the guard.
        self.link("../../../cache/installed")
        self.assertIn("a link out of the run", self.problems()[0])

    def test_someone_elses_entry_is_refused(self):
        self.assertIn("not ours", helper.tree_problems(self.tree, self.run, [self.root], self.uid + 1)[0])

    def test_the_tree_itself_must_be_a_dir(self):
        os.symlink(self.tree, os.path.join(self.root, "alias"))
        alias = os.path.join(self.root, "alias")
        self.assertIn("not a directory", helper.tree_problems(alias, self.run, [self.root], self.uid)[0])


class Subvol(unittest.TestCase):
    """The btrfs ioctls (#255): the numbers, and, on btrfs, a real round trip."""

    def test_ioctl_numbers(self):
        # linux/btrfs.h: _IOW(0x94, 1 | 14, struct btrfs_ioctl_vol_args), 4096 bytes.
        self.assertEqual(helper.BTRFS_VOL_ARGS.size, 4096)
        self.assertEqual(helper.BTRFS_IOC_SNAP_CREATE, 0x50009401)
        self.assertEqual(helper.BTRFS_IOC_SUBVOL_CREATE, 0x5000940E)

    def test_bad_names(self):
        with tempfile.TemporaryDirectory() as d:
            for name in ("x" * 256,):
                with self.assertRaises(OSError):
                    helper.subvol_create(os.path.join(d, name))
            with self.assertRaises(OSError):
                helper.subvol_create(d + "/..")

    def test_round_trip(self):
        # Under target/, where the harness works: btrfs on a dev box, else skipped.
        target = os.path.join(REPO, "target")
        os.makedirs(target, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=target, prefix="subvol-test-") as d:
            sv, snap = os.path.join(d, "sv"), os.path.join(d, "snap")
            try:
                helper.subvol_create(sv)
            except OSError as e:
                self.skipTest(f"no btrfs subvolumes here: {e}")
            self.assertEqual(os.stat(sv).st_ino, 256)
            with open(os.path.join(sv, "f"), "w") as f:
                f.write("one")
            helper.subvol_snapshot(sv, snap)
            with open(os.path.join(snap, "f"), "w") as f:
                f.write("two")
            with open(os.path.join(sv, "f")) as f:
                self.assertEqual(f.read(), "one")
            with self.assertRaises(OSError):
                helper.subvol_snapshot(sv, snap)
            with contextlib.redirect_stderr(io.StringIO()) as err:
                self.assertEqual(helper.main(["h", "subvol", "snapshot", sv, snap]), 1)
            self.assertIn("subvol snapshot", err.getvalue())
            # Emptied, then rmdir: what rm -rf does, and TemporaryDirectory too.
            os.unlink(os.path.join(snap, "f"))
            os.rmdir(snap)


class LiveSteps(unittest.TestCase):
    """Live sessions (#241): the manifest's new steps, and commands into steps."""

    RECT = {"x": 100, "y": 0, "width": 800, "height": 600}

    def test_hold_and_move_to(self):
        m = helper.parse_manifest(
            """
            gogid = 1
            [[step]]
            hold = { key = "w", ms = 500 }
            [[step]]
            move_to = { x = 25, y = 50.5 }
            """
        )
        self.assertEqual(
            m["steps"], [{"hold": {"key": "w", "ms": 500}}, {"move_to": {"x": 25.0, "y": 50.5}}]
        )

    def test_hold_and_move_to_are_checked(self):
        for step in (
            'hold = { key = "w" }',
            'hold = { key = "W", ms = 5 }',
            'hold = { key = "w", ms = 0 }',
            'hold = { key = "w", ms = 1.5 }',
            'hold = { key = "w", ms = 10001 }',
            'move_to = { x = 101, y = 1 }',
            'move_to = { x = 1 }',
        ):
            with self.subTest(step), self.assertRaises(helper.ManifestError):
                helper.parse_manifest(f"gogid = 1\n[[step]]\n{step}\n")

    def test_live_manifest_needs_no_steps(self):
        with self.assertRaises(helper.ManifestError):
            helper.parse_manifest("gogid = 1\n")
        self.assertEqual(helper.parse_manifest("gogid = 1\n", live=True)["steps"], [])
        # Steps there are checked, then dropped: the session finds its own.
        self.assertEqual(helper.parse_manifest(MINIMAL, live=True)["steps"], [])
        with self.assertRaises(helper.ManifestError):
            helper.parse_manifest("gogid = 1\n[[step]]\nkey = \"Nope\"\n", live=True)

    def test_pixels_are_a_percent_of_the_window(self):
        step = helper.live_raw_step("click", ["500", "150"], self.RECT)
        self.assertEqual(step, {"click": {"x": 50.0, "y": 25.0}})
        self.assertEqual(helper.live_raw_step("move_to", ["10%", "90%"], None), {"move_to": {"x": 10.0, "y": 90.0}})
        self.assertEqual(helper.live_raw_step("click", [], None), {"click_here": True})

    def test_outside_the_window_or_not_a_number(self):
        for cmd, args, rect in (
            ("click", ["50", "10"], RECT),
            ("click", ["500", "x"], RECT),
            ("click", ["500", "10"], None),
            ("move_to", ["1"], RECT),
            ("hold", ["w"], RECT),
            ("scroll", ["two"], RECT),
            ("wait", [], RECT),
            ("dance", [], RECT),
        ):
            with self.subTest(cmd=cmd, args=args), self.assertRaises(helper.LiveError):
                helper.live_raw_step(cmd, args, rect)

    def test_commands_become_steps(self):
        for cmd, args, want in (
            ("key", ["space"], {"key": "space"}),
            ("type", ["Hi!"], {"type": "Hi!"}),
            ("hold", ["w", "300"], {"hold": {"key": "w", "ms": 300}}),
            ("move_by", ["-3000", "2.5"], {"move_by": {"x": -3000, "y": 2.5}}),
            ("scroll", ["-3"], {"scroll": -3}),
            ("wait", ["1.5"], {"wait": 1.5}),
            ("screenshot", [], {"screenshot": "shot"}),
        ):
            with self.subTest(cmd=cmd):
                raw = helper.live_raw_step(cmd, args, self.RECT)
                self.assertEqual(raw, want)
                helper._step(raw, 0, set())  # and the manifest takes it

    def test_names_and_gaps(self):
        self.assertEqual(helper.live_unique_name("a", ["b"]), "a")
        self.assertEqual(helper.live_unique_name("a", ["a", "a-2"]), "a-3")
        self.assertEqual(helper.live_kept_gap({"gap": 0.1}, 10), 0)
        # A pause between two keys is kept: a game can drop keys sent at once.
        self.assertEqual(helper.live_kept_gap({"gap": 0.3}, 10), 0.3)
        self.assertEqual(helper.live_kept_gap({"gap": 3.21}, 10), 3.2)
        self.assertEqual(helper.live_kept_gap({"gap": 99}, 10), 10)
        # A late match in a replay waits longer, whatever the cap.
        self.assertEqual(helper.live_kept_gap({"gap": 99, "extra": 4}, 10), 14)
        self.assertEqual(helper.live_kept_gap({"gap": 0.1, "extra": 2}, 10), 2)

    def test_a_moving_picture_is_matched_loosely(self):
        self.assertEqual(helper.live_match_allowance({}, 15), (15, False))
        self.assertEqual(helper.live_match_allowance({"moving_pct": 3.5}, 15), (18.5, False))
        # A video: anything there will do, as long as it's a picture.
        self.assertEqual(helper.live_match_allowance({"moving_pct": 40}, 15), (None, True))

    def test_last_shot_counts_the_inputs_after_it(self):
        path = [
            {"step": {"screenshot": "a"}},
            {"step": {"key": "x"}},
            {"step": {"wait": 1}},
            {"mark": "checkpoint", "name": "c"},
            {"step": {"click_here": True}},
        ]
        self.assertEqual(helper.live_last_shot(path), ("a", 2))
        self.assertEqual(helper.live_last_shot([]), (None, 0))


RECT = LiveSteps.RECT


def live_session(path, **more):
    manifest = helper.parse_manifest(
        "gogid = 7\ntitle = \"Seven\"\n[window]\nresolution = \"800x600\"\n"
        "[override]\nextra_args = [\"-skip\"]\n[override.env]\nA = \"1\"\n",
        live=True,
    )
    session = {
        "id": "7-x", "manifest": manifest, "max_gap": 10.0, "match_pct": 15.0, "path": path,
        "checkpoints": {}, "names": [], "restores": [{"checkpoint": "c"}], "seq": 0,
        "replay": None, "last_replay": None, "game_exit": None, "ended": None,
    }
    session.update(more)
    return session


def shot(name, since=None, change=None, inputs=0, nonblack=50.0, fill=100.0, gap=0.0):
    return {
        "step": {"screenshot": name}, "gap": gap, "since": since, "change_pct": change,
        "inputs_since": inputs,
        "stats": {"nonblack_pct": nonblack, "box_w_pct": fill, "box_h_pct": fill},
    }


class LiveExport(unittest.TestCase):
    def steps(self, text):
        return helper.parse_manifest(text)["steps"]

    def test_route_with_waits_and_checks(self):
        text = helper.live_export(live_session([
            shot("black", nonblack=0.2, gap=30),            # dropped: not_black would fail
            shot("title", gap=4),
            {"step": {"key": "space"}, "gap": 25},          # thinking: cut to 10
            {"step": {"wait": 15}, "gap": 0.1},
            shot("agreement", since="title", change=80.0, inputs=1, gap=0.1),
            {"mark": "checkpoint", "name": "agreed"},
            {"step": {"key": "Down"}, "gap": 2, "extra": 1.5},
            shot("same", since="agreement", change=1.0, inputs=1),  # too small a change
            shot("small", since="same", change=30.0, fill=50.0),    # no input between
        ]))
        self.assertIn('extra_args = ["-skip"]', text)
        self.assertIn("# checkpoint 'agreed'", text)
        self.assertIn('how = "signal"', text)
        self.assertEqual(self.steps(text), [
            {"wait": 16.5},  # 30 cut to 10, the black shot's grab, 4
            {"screenshot": "title"},
            {"wait": 10.0},
            {"key": "space"},
            {"wait": 15.0},  # an explicit wait is kept whole
            {"screenshot": "agreement"},
            {"expect_change": {"from": "title", "to": "agreement", "min": 20.0}},
            {"wait": 3.5},
            {"key": "Down"},
            {"screenshot": "same"},
            {"screenshot": "small", "min_fill": False},
        ])
        m = helper.parse_manifest(text)
        self.assertEqual(m["override"], {"env": {"A": "1"}, "extra_args": ["-skip"]})
        self.assertEqual((m["window"]["width"], m["window"]["height"]), (800, 600))

    def test_press_until_exports_as_it_was_given(self):
        marked = {"x": 9.0, "y": 57.5, "w": 21.5, "h": 6.5, "color": "#f0f040", "tol": 40, "min": 1.0}
        session = live_session(
            [
                shot("a"),
                {"step": {"press_until": {"key": "space", "change": 20.0, "every": 2.0, "max": 90}}, "gap": 0.0, "press_secs": 31.0},
                {"step": {"press_until": {"key": "Down", "marked": marked, "every": 1.0, "max": 8}}, "gap": 3.0, "press_secs": 2.0},
                shot("b", since="a", change=60.0, inputs=2),
            ]
        )
        steps = helper.parse_manifest(helper.live_export(session))["steps"]
        self.assertEqual(steps[1], {"press_until": {"key": "space", "change": 20.0, "every": 2.0, "max": 90}})
        self.assertEqual(steps[2], {"wait": 3.0})
        self.assertEqual(steps[3]["press_until"]["marked"], marked)
        # An input: the screenshot after it gets its check.
        self.assertEqual(steps[5]["expect_change"]["from"], "a")
        self.assertEqual(helper.live_replay_secs(session["path"], session), 41)

    def test_live_press_args(self):
        def number(text, what):
            return float(text)

        self.assertEqual(
            helper.live_press_args(["space", "--every", "2", "--max", "90", "--change", "20"], number),
            {"key": "space", "every": 2.0, "max": 90.0, "change": 20.0},
        )
        got = helper.live_press_args(["Down", "--marked", "9", "57", "21", "6", "#f0f040", "1", "30", "--every", "1", "--max", "8"], number)
        self.assertEqual(got["marked"], {"x": 9.0, "y": 57.0, "w": 21.0, "h": 6.0, "color": "#f0f040", "min": 1.0, "tol": 30.0})
        self.assertEqual(
            helper.live_raw_step("wait_until", ["--every", "1", "--max", "3", "--marked", "1", "2", "3", "4", "#ffffff", "5"], None),
            {"wait_until": {"every": 1, "max": 3, "marked": {"x": 1, "y": 2, "w": 3, "h": 4, "color": "#ffffff", "min": 5}}},
        )
        for bad in ([], ["--every", "1"], ["a", "--marked", "1", "2"], ["a", "--soon"], ["a", "--every"]):
            with self.subTest(bad), self.assertRaises(helper.LiveError):
                helper.live_press_args(bad, number)

    def test_settle_exports_as_wait_settled_or_a_plain_wait(self):
        text = helper.live_export(live_session([
            shot("title", gap=1),
            {"step": {"wait_settled": helper.live_settle_timeout(4.0)}, "gap": 2, "settled_secs": 4.0},
            {"step": {"key": "space"}, "gap": 0.1},
            {"step": {"wait": 6.5}, "gap": 0.1, "unsettled": True},
            shot("menu", since="title", change=50.0, inputs=1),
        ]))
        self.assertEqual(self.steps(text), [
            {"wait": 1.0},
            {"screenshot": "title"},
            {"wait": 2.0},
            {"wait_settled": 18.0},  # 2 * 4 s + 10 s
            {"key": "space"},
            {"wait": 6.5},  # a settle that never did: the time it spent
            {"screenshot": "menu"},
            {"expect_change": {"from": "title", "to": "menu", "min": 20.0}},
        ])
        self.assertIn("# settle: the screen never settled", text)

    def test_settle_args(self):
        self.assertEqual(helper.live_settle_max([]), helper.LIVE_SETTLE_MAX)
        self.assertEqual(helper.live_settle_max(["--max", "5"]), 5.0)
        for bad in (["5"], ["--max"], ["--max", "x"], ["--max", "0"], ["--max", "9999"]):
            with self.subTest(bad), self.assertRaises(helper.LiveError):
                helper.live_settle_max(bad)
        self.assertEqual(helper.live_settle_timeout(0.0), 10.0)
        self.assertEqual(helper.live_settle_timeout(4.2), 19.0)

    def test_replay_estimate_counts_a_settle_by_how_long_it_took(self):
        path = [{"step": {"wait_settled": 18.0}, "gap": 0, "settled_secs": 4.0}]
        self.assertEqual(helper.live_replay_secs(path, {"max_gap": 10.0}), 4)

    def test_a_game_that_quit_exits_by_itself(self):
        path = [shot("a"), {"step": {"key": "q"}, "gap": 1}]
        for more in ({"game_exit": {"code": 0, "life": 1}}, {"ended": {"how": "self", "code": 0}}):
            with self.subTest(more):
                m = helper.parse_manifest(helper.live_export(live_session(path, **more)))
                self.assertEqual(m["exit"]["how"], "self")
        m = helper.parse_manifest(helper.live_export(live_session(path, ended={"how": "self", "code": 2})))
        self.assertEqual(m["exit"]["code"], 2)

    def test_an_empty_route_is_still_a_manifest(self):
        self.assertEqual(self.steps(helper.live_export(live_session([]))), [{"wait": 1.0}])

    def test_a_command_manifest_keeps_its_commands(self):
        session = live_session([shot("a")])
        session["manifest"] = helper.parse_manifest(
            'command = ["xterm", "-e", "say \\"hi\\"\\n"]\nbuild_command = ["true"]\n', live=True
        )
        m = helper.parse_manifest(helper.live_export(session))
        self.assertEqual(m["command"], ["xterm", "-e", 'say "hi"\n'])
        self.assertEqual(m["build_command"], ["true"])


class Relaunch(unittest.TestCase):
    """The relaunch step and clean-stop checkpoints (#252)."""

    KEPT = "/run/live/disks/9"

    def steps(self, text):
        return helper.parse_manifest(text)["steps"]

    def route(self):
        return [
            shot("a", gap=1),
            {"step": {"key": "Return"}, "gap": 1},
            {"mark": "checkpoint", "name": "stop", "disk": self.KEPT},
        ]

    def test_the_step(self):
        self.assertEqual(self.steps('gogid = 1\n[[step]]\nrelaunch = true\n'), [{"relaunch": True}])
        for bad in ("false", "1", '"yes"', "{ }"):
            with self.subTest(bad), self.assertRaises(helper.ManifestError):
                helper.parse_manifest(f"gogid = 1\n[[step]]\nrelaunch = {bad}\n")

    def test_a_checkpoint_with_no_disk_replays_from_the_launch(self):
        path = [shot("a"), {"mark": "checkpoint", "name": "a"}, {"step": {"relaunch": True}, "gap": 0}, shot("b")]
        got, disk, keep = helper.live_restore_plan(path, 3)
        self.assertEqual((got, disk, keep), (path, None, 0))

    def test_a_restore_to_the_stop_itself_gets_a_relaunch_and_replays_nothing(self):
        path = self.route()
        got, disk, keep = helper.live_restore_plan(path, 3)
        self.assertEqual(disk, self.KEPT)
        self.assertEqual(keep, len(got))
        self.assertEqual(got[:3], path)
        self.assertEqual(got[3], {"step": {"relaunch": True}, "gap": 0.0, "life": 3})
        self.assertEqual(len(path), 3, "the checkpoint's own path stays as it was")
        self.assertEqual(helper.live_replay_secs(path, {"max_gap": 10.0}), 0)

    def test_a_restore_past_the_stop_replays_only_what_is_after_the_relaunch(self):
        path = self.route() + [
            {"step": {"wait": 1}, "gap": 0},  # with the game down: stands
            {"step": {"relaunch": True}, "gap": 0, "relaunch_secs": 30.0},
            {"step": {"key": "x"}, "gap": 2},
            {"step": {"wait": 7}, "gap": 0},
            {"mark": "checkpoint", "name": "past"},
        ]
        got, disk, keep = helper.live_restore_plan(path, 3)
        self.assertEqual(disk, self.KEPT)
        self.assertEqual(got, path)
        self.assertEqual(keep, 5)
        self.assertEqual([e.get("step") or e["mark"] for e in got[keep:]], [{"key": "x"}, {"wait": 7}, "checkpoint"])
        self.assertEqual(helper.live_replay_secs(path, {"max_gap": 10.0}), 9)

    def test_the_last_kept_disk_counts(self):
        path = self.route() + [
            {"step": {"relaunch": True}, "gap": 0},
            {"step": {"key": "Return"}, "gap": 1},
            {"mark": "checkpoint", "name": "again", "disk": "/run/live/disks/12"},
        ]
        _, disk, keep = helper.live_restore_plan(path, 3)
        self.assertEqual((disk, keep), ("/run/live/disks/12", 7))

    def test_a_relaunch_with_no_kept_disk_is_replayed_estimated_by_its_start(self):
        path = [{"step": {"relaunch": True}, "gap": 0, "relaunch_secs": 28.0}]
        self.assertEqual(helper.live_replay_secs(path, {"max_gap": 10.0}), 28)

    def test_a_shot_past_a_relaunch_compares_with_nothing(self):
        path = [shot("a"), {"step": {"key": "x"}, "gap": 1}, {"step": {"relaunch": True}, "gap": 0}]
        self.assertEqual(helper.live_last_shot(path), (None, 0))
        self.assertEqual(helper.live_last_shot(path + [{"step": {"key": "x"}}]), (None, 0))

    def test_the_export_quits_relaunches_and_goes_on(self):
        text = helper.live_export(live_session(self.route() + [
            {"step": {"relaunch": True}, "gap": 0.0},
            shot("b", since=None, change=None, gap=2),
            {"step": {"key": "x"}, "gap": 1},
            shot("c", since="b", change=40.0, inputs=1, gap=1),
        ]))
        self.assertIn("# checkpoint 'stop': a clean stop, its disk kept", text)
        self.assertEqual(self.steps(text), [
            {"wait": 1.0},
            {"screenshot": "a"},
            {"wait": 1.0},
            {"key": "Return"},
            {"relaunch": True},
            {"wait": 2.0},
            {"screenshot": "b"},  # no expect_change from a: another launch
            {"wait": 1.0},
            {"key": "x"},
            {"wait": 1.0},
            {"screenshot": "c"},
            {"expect_change": {"from": "b", "to": "c", "min": 20.0}},
        ])
        # Its quit goes by [exit], whichever way the last launch ends.
        m = helper.parse_manifest(text)
        self.assertEqual((m["exit"]["how"], m["exit"]["wait_secs"]), ("signal", 60.0))

    def test_the_export_keeps_the_exit_code_a_relaunch_was_held_to(self):
        session = live_session(self.route() + [{"step": {"relaunch": True}, "gap": 0.0}])
        session["manifest"]["exit"]["code"] = 9
        self.assertEqual(helper.parse_manifest(helper.live_export(session))["exit"]["code"], 9)

    def test_a_stop_with_no_relaunch_exports_none(self):
        text = helper.live_export(live_session(self.route(), game_exit={"code": 0, "life": 1}))
        self.assertNotIn("relaunch", text.replace("relaunches", ""))
        self.assertEqual(helper.parse_manifest(text)["exit"]["how"], "self")

    def test_from_manifest_runs_up_to_a_shot_past_a_relaunch(self):
        text = 'gogid = 1\n[[step]]\nscreenshot = "a"\n[[step]]\nrelaunch = true\n[[step]]\nscreenshot = "b"\n[[step]]\nkey = "x"\n'
        got = helper.live_from_manifest(text)
        self.assertEqual(got["steps"], [{"screenshot": "a"}, {"relaunch": True}, {"screenshot": "b"}])

    def test_what_is_left_of_a_launch(self):
        table = [
            (1, "bwrap", "S"), (2, "python3", "S"), (6, ".swaybg-wrapped", "S"), (9, "sway", "S"),
            (30, "wineserver", "S"), (31, "game.exe", "Z"), (32, "explorer.exe", "R"),
        ]
        harness = {1, 2, 6, 9}
        self.assertEqual(helper.left_processes(table, harness), ["30 wineserver", "32 explorer.exe"])
        self.assertEqual(helper.left_processes(table[:4], harness), [])


class LiveFromManifest(unittest.TestCase):
    """live start --from-manifest (#251): the point, the path, the export."""

    TEXT = """
        gogid = 7
        [[step]]
        screenshot = "title"
        [[step]]
        key = "space"
        [[step]]
        wait = 3
        [[step]]
        screenshot = "menu"
        [[step]]
        expect_change = { from = "title", to = "menu", min = 30 }
        [[step]]
        key = "Down"
        [[step]]
        screenshot = "last"
        [[step]]
        key = "q"
    """

    def pick(self, **kw):
        return helper.live_from_manifest(self.TEXT, **kw)

    def test_default_is_the_last_screenshot(self):
        got = self.pick()
        self.assertEqual((got["to_shot"], got["to_step"], got["total"]), ("last", 7, 8))
        self.assertEqual(got["steps"][-1], {"screenshot": "last"})

    def test_a_shot_takes_its_expect_change_along(self):
        got = self.pick(to_shot="menu")
        self.assertEqual(got["to_step"], 5)
        self.assertEqual(got["steps"][-1]["expect_change"]["to"], "menu")

    def test_to_step_counts_every_step(self):
        self.assertEqual(self.pick(to_step="2")["steps"], [{"screenshot": "title"}, {"key": "space"}])
        self.assertEqual(self.pick(to_step=8)["to_step"], 8)

    def test_a_point_that_isnt_there_is_refused(self):
        for kw in ({"to_step": "0"}, {"to_step": "9"}, {"to_step": "x"}, {"to_shot": "nope"},
                   {"to_step": "2", "to_shot": "menu"}):
            with self.subTest(kw), self.assertRaises(helper.ManifestError):
                self.pick(**kw)
        with self.assertRaisesRegex(helper.ManifestError, "no screenshot"):
            helper.live_from_manifest("gogid = 7\n[[step]]\nwait = 1\n")

    def manifest_path(self):
        return [
            {"step": {"screenshot": "title"}, "gap": 0.0, "manifest": True,
             "stats": {"nonblack_pct": 50.0, "box_w_pct": 100.0, "box_h_pct": 100.0}},
            {"step": {"key": "space"}, "gap": 0.0, "manifest": True},
            {"step": {"wait_settled": 20.0}, "gap": 0.0, "manifest": True, "settled_secs": 3.0},
            {"step": {"screenshot": "menu"}, "gap": 0.0, "manifest": True, "extra": 1.5,
             "stats": {"nonblack_pct": 50.0, "box_w_pct": 100.0, "box_h_pct": 100.0}},
            {"step": {"expect_change": {"from": "title", "to": "menu", "min": 30.0}}, "gap": 0.0, "manifest": True},
        ]

    def test_export_has_the_old_steps_then_the_new(self):
        path = self.manifest_path() + [
            {"step": {"key": "Down"}, "gap": 2.0},
            shot("deeper", since="menu", change=40.0, inputs=1),
        ]
        text = helper.live_export(live_session(path, from_manifest={"done": 5, "total": 8, "ok": True}))
        self.assertIn("Steps 1-5 of 8", text)
        steps = helper.parse_manifest(text)["steps"]
        self.assertEqual(steps, [
            {"screenshot": "title"},
            {"key": "space"},
            {"wait_settled": 20.0},
            {"wait": 1.5},  # what a late match added
            {"screenshot": "menu"},
            {"expect_change": {"from": "title", "to": "menu", "min": 30.0}},
            {"wait": 2.0},
            {"key": "Down"},
            {"screenshot": "deeper"},
            {"expect_change": {"from": "menu", "to": "deeper", "min": 20.0}},
        ])
        self.assertNotIn('"q"', text)

    def test_a_manifest_shot_is_kept_even_if_black_or_small(self):
        path = self.manifest_path()
        path[0]["stats"] = {"nonblack_pct": 0.1, "box_w_pct": 10.0, "box_h_pct": 10.0}
        steps = helper.parse_manifest(helper.live_export(live_session(path)))["steps"]
        self.assertEqual(steps[0], {"screenshot": "title"})  # as the manifest wrote it

    def test_replay_estimate_counts_the_manifest_part(self):
        self.assertEqual(helper.live_replay_secs(self.manifest_path(), {"max_gap": 10.0}),
                         round(2 * helper.LIVE_SHOT_SECS + 3.0 + 1.5))


class Frames(unittest.TestCase):
    IMAGE = (3, 2, 3, [bytes(range(9)), bytes(range(9, 18))])

    def test_ppm_round_trip(self):
        self.assertEqual(helper.parse_ppm(helper.ppm_bytes(self.IMAGE)), self.IMAGE)

    def test_ppm_header_with_a_data_byte_like_whitespace(self):
        image = (1, 1, 3, [b"\n \n"])
        self.assertEqual(helper.parse_ppm(helper.ppm_bytes(image)), image)

    def test_a_bad_ppm_is_refused(self):
        for data in (b"", b"P5\n1 1\n255\nxxx", b"P6\n2 2\n255\nshort", b"P6\n1 1\n65535\nxxxxxx"):
            with self.assertRaises(ValueError):
                helper.parse_ppm(data)

    def test_write_png_reads_back(self):
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "x.png")
            helper.write_png(path, self.IMAGE)
            self.assertEqual(helper.read_png(path), self.IMAGE)


class Leftovers(unittest.TestCase):
    def proc(self, root, pid, environ=b"", cmdline=b"", comm="x\n", ns=None):
        d = os.path.join(root, pid)
        os.makedirs(os.path.join(d, "ns"))
        for name, data in (("environ", environ), ("cmdline", cmdline), ("comm", comm.encode())):
            with open(os.path.join(d, name), "wb") as f:
                f.write(data)
        if ns:
            os.symlink(ns, os.path.join(d, "ns/pid"))

    def test_what_counts(self):
        with tempfile.TemporaryDirectory() as root:
            marks = ["GOGO_E2E_RUN=r1", "WINEPREFIX=/p"]
            self.proc(root, "10", environ=b"A=1\0GOGO_E2E_RUN=r1\0B=2\0", comm="wine\n")
            self.proc(root, "11", environ=b"WINEPREFIX=/p/other\0GOGO_E2E_RUN=r10\0")  # not whole entries
            self.proc(root, "12", cmdline=b"python3\0--dir\0/scratch/run/x\0", comm="py\n")
            self.proc(root, "13", cmdline=b"python3\0/scratch/runner\0")
            self.proc(root, "14", ns="pid:[7]", comm="sway\n")
            self.proc(root, "15", ns="pid:[8]")
            self.proc(root, "16", environ=b"WINEPREFIX=/p\0")  # excluded
            os.makedirs(os.path.join(root, "self"))
            got = helper.leftovers(marks, "/scratch/run/", "pid:[7]", {"16"}, proc=root)
            self.assertEqual(sorted(got), ["10 wine", "12 py", "14 sway"])

    def test_a_process_that_went_away_is_no_error(self):
        with tempfile.TemporaryDirectory() as root:
            os.makedirs(os.path.join(root, "20"))
            self.assertEqual(helper.leftovers(["A=1"], "/run/", "", set(), proc=root), [])


class FastReplay(unittest.TestCase):
    @staticmethod
    def wait(secs, gap=0.0):
        return {"step": {"wait": secs}, "gap": gap}

    @staticmethod
    def shot(name, gap=0.0, **more):
        return {"step": {"screenshot": name}, "gap": gap, **more}

    def test_a_wait_leads_up_to_the_next_shot_unless_an_input_comes_first(self):
        key = {"step": {"key": "a"}, "gap": 0.0}
        path = [self.wait(5), {"mark": "checkpoint", "name": "x"}, self.wait(1), self.shot("a"), self.wait(5), key, self.shot("b")]
        self.assertEqual(helper.live_wait_target(path, 0), 3)
        self.assertEqual(helper.live_wait_target(path, 3), 3)
        self.assertIsNone(helper.live_wait_target(path, 4))
        self.assertIsNone(helper.live_wait_target(path, 5))
        self.assertEqual(helper.live_wait_target(path, 6), 6)
        self.assertIsNone(helper.live_wait_target([self.wait(5)], 0))

    def test_the_run_counts_gaps_and_waits(self):
        path = [self.wait(5, gap=20.0), {"mark": "checkpoint", "name": "x"}, self.shot("a", gap=2.0)]
        self.assertEqual(helper.live_wait_run_secs(path, 0, 2, 10.0), 17.0)

    def test_arriving_is_tighter_than_matching(self):
        entry = self.shot("a", moving_pct=2.0, stats={"nonblack_pct": 80.0})
        self.assertEqual(helper.live_arrived_allowance(entry, 15.0), 4.0)
        self.assertEqual(helper.live_match_allowance(entry, 15.0), (17.0, False))
        # And never looser than the match itself.
        entry = self.shot("a", moving_pct=1.0, stats={"nonblack_pct": 80.0})
        self.assertEqual(helper.live_arrived_allowance(entry, 1.2), 2.2)

    def test_a_moving_or_black_frame_cant_say(self):
        self.assertIsNone(helper.live_arrived_allowance(self.shot("a", moving_pct=20.0, stats={"nonblack_pct": 80.0}), 15.0))
        self.assertIsNone(helper.live_arrived_allowance(self.shot("a", moving_pct=0.0, stats={"nonblack_pct": 0.0}), 15.0))
        self.assertIsNone(helper.live_arrived_allowance(self.shot("a", moving_pct=0.0), 15.0))


class LiveSocket(unittest.TestCase):
    """The control socket: bound through the dir's fd (a long path is fine), ours
    only, one JSON line each way."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        # Past a socket address's 108 bytes.
        self.dir = os.path.join(self.tmp.name, "x" * 60, "y" * 60, "live")
        cfg = {"manifest": helper.parse_manifest(MINIMAL), "run_dir": self.tmp.name,
               "results_dir": self.tmp.name, "live": {"dir": self.dir}}
        self.driver = helper.Driver(cfg)
        self.driver.live_listen()

    def tearDown(self):
        self.driver.srv.close()
        self.tmp.cleanup()

    def test_a_wait_finds_the_socket_once_registered(self):
        # status --wait from before the session had a scratch: the registry
        # entry's run= comes later, and the wait reads it then.
        import threading

        reg = os.path.join(self.tmp.name, "reg")
        with open(reg, "w") as f:
            f.write("pid=1\nrun=\n")
        self.assertEqual(helper.live_registered_dir(reg), "")
        state = os.path.join(self.tmp.name, "state.json")
        helper.write_json(state, {"state": "ready", "life": 1})

        def register():
            time.sleep(0.5)
            with open(reg, "w") as f:
                f.write(f"pid=1\nrun={os.path.dirname(self.dir)}\n")

        def serve():
            for _ in range(100):
                accepted = self.driver.live_accept(0.1)
                if accepted:
                    self.driver.live_reply(accepted[0], {"ok": True, "state": "ready"})
                    return

        threads = [threading.Thread(target=register), threading.Thread(target=serve)]
        for t in threads:
            t.start()
        code, reply = helper.live_wait(state, "", 8, os.getpid(), 1, reg)
        for t in threads:
            t.join()
        self.assertEqual((code, reply.get("state")), (0, "ready"))

    def test_round_trip(self):
        import threading

        got = {}

        def client():
            got["reply"] = helper.live_call(self.dir, {"cmd": "key", "args": ["a"]}, 5)

        t = threading.Thread(target=client)
        t.start()
        conn, req = None, None
        for _ in range(50):
            accepted = self.driver.live_accept(0.1)
            if accepted:
                conn, req = accepted
                break
        self.assertEqual(req, {"cmd": "key", "args": ["a"]})
        self.driver.live_reply(conn, {"ok": True})
        t.join(5)
        self.assertEqual(got["reply"], {"ok": True})
        sock = os.path.join(self.dir, helper.LIVE_SOCKET)
        self.assertEqual(os.stat(sock).st_mode & 0o077, 0)
        self.assertEqual(os.stat(self.dir).st_mode & 0o777, 0o700)

    def test_not_a_socket_is_refused(self):
        os.unlink(os.path.join(self.dir, helper.LIVE_SOCKET))
        with open(os.path.join(self.dir, helper.LIVE_SOCKET), "w"):
            pass
        with self.assertRaises(OSError):
            helper.live_call(self.dir, {"cmd": "status", "args": []}, 1)


if __name__ == "__main__":
    unittest.main()
