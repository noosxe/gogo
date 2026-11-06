#!/usr/bin/env python3
"""The half of scripts/linux-e2e.sh that isn't shell (docs/linux-e2e.md).

Run with `python3 -I`: stdlib only, nothing from the working directory.

    linux-e2e-helper.py manifest <file>        validate; print it as JSON (exit 2: bad)
    linux-e2e-helper.py image <png> [w h x y]  print image stats as JSON
    linux-e2e-helper.py diff <a.png> <b.png>   print the changed fraction as JSON
    linux-e2e-helper.py drive <config.json>    inside the sandbox: run a whole game

`drive` starts headless sway, proves the display is the headless one, launches
the game, runs the manifest's steps and writes result.json after every step, so
a killed run still has what it got that far.
"""

import json
import os
import re
import shutil
import signal
import socket
import struct
import subprocess
import sys
import time
import tomllib
import zlib

# A channel above this counts as "not black": dark video noise and compressed
# black aren't a picture. The same tolerance counts a pixel as changed.
BLACK_LEVEL = 16
CHANGE_LEVEL = 16
DEFAULT_MIN_FILL = {"w": 90.0, "h": 90.0}
# Below this share of non-black pixels a frame counts as all black: a cursor on
# an empty screen is a few hundred pixels.
MIN_NONBLACK_PCT = 1.0
DEFAULT_MIN_CHANGE_PCT = 0.5
# wait_settled: two frames this far apart, this close, count as holding still.
SETTLE_POLL_SECS = 2.0
SETTLE_MAX_CHANGE_PCT = 1.0
EXIT_HOW = ("self", "signal")


class ManifestError(Exception):
    pass


# --- manifest -----------------------------------------------------------------


def _number(value, what):
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ManifestError(f"{what}: must be a number, got {value!r}")
    return float(value)


def _percent(value, what):
    n = _number(value, what)
    if not 0 <= n <= 100:
        raise ManifestError(f"{what}: must be 0-100 (a percentage), got {value!r}")
    return n


def _only(table, allowed, what):
    if not isinstance(table, dict):
        raise ManifestError(f"{what}: must be a table")
    extra = sorted(set(table) - set(allowed))
    if extra:
        raise ManifestError(f"{what}: unknown key {extra[0]!r}")


def _min_fill(raw, what):
    _only(raw, ("w", "h"), what)
    return {
        "w": _percent(raw.get("w", DEFAULT_MIN_FILL["w"]), f"{what}.w"),
        "h": _percent(raw.get("h", DEFAULT_MIN_FILL["h"]), f"{what}.h"),
    }


def _step(raw, i, shots):
    """One `[[step]]`: exactly one action key (with `screenshot`'s `min_fill`)."""
    what = f"step {i + 1}"
    if not isinstance(raw, dict):
        raise ManifestError(f"{what}: must be a table")
    actions = [k for k in raw if k != "min_fill"]
    if len(actions) != 1:
        raise ManifestError(
            f"{what}: needs exactly one of click, key, type, wait, wait_settled, "
            f"screenshot, expect_change; got {sorted(actions)}"
        )
    action = actions[0]
    value = raw[action]
    if "min_fill" in raw and action != "screenshot":
        raise ManifestError(f"{what}: min_fill belongs on a screenshot")
    if action == "click":
        _only(value, ("x", "y"), f"{what}: click")
        if "x" not in value or "y" not in value:
            raise ManifestError(f"{what}: click needs x and y")
        return {
            "click": {
                "x": _percent(value["x"], f"{what}: click.x"),
                "y": _percent(value["y"], f"{what}: click.y"),
            }
        }
    if action in ("key", "type"):
        if not isinstance(value, str) or not value:
            raise ManifestError(f"{what}: {action} must be a non-empty string")
        return {action: value}
    if action in ("wait", "wait_settled"):
        n = _number(value, f"{what}: {action}")
        if n < 0:
            raise ManifestError(f"{what}: {action} can't be negative")
        return {action: n}
    if action == "screenshot":
        if not isinstance(value, str) or not re.fullmatch(r"[A-Za-z0-9_-]+", value):
            raise ManifestError(f"{what}: screenshot needs a name of [A-Za-z0-9_-]")
        if value in shots:
            raise ManifestError(f"{what}: screenshot {value!r} is taken twice")
        shots.add(value)
        step = {"screenshot": value}
        if "min_fill" in raw:
            fill = raw["min_fill"]
            step["min_fill"] = (
                False if fill is False else _min_fill(fill, f"{what}: min_fill")
            )
        return step
    if action == "expect_change":
        _only(value, ("from", "to", "min"), f"{what}: expect_change")
        for key in ("from", "to"):
            if value.get(key) not in shots:
                raise ManifestError(
                    f"{what}: expect_change.{key} {value.get(key)!r} "
                    "isn't an earlier screenshot"
                )
        return {
            "expect_change": {
                "from": value["from"],
                "to": value["to"],
                "min": _percent(
                    value.get("min", DEFAULT_MIN_CHANGE_PCT),
                    f"{what}: expect_change.min",
                ),
            }
        }
    raise ManifestError(f"{what}: unknown action {action!r}")


def parse_manifest(text):
    """Validate a manifest's TOML and fill in its defaults."""
    try:
        raw = tomllib.loads(text)
    except tomllib.TOMLDecodeError as e:
        raise ManifestError(f"not TOML: {e}") from e
    _only(raw, ("gogid", "title", "command", "window", "exit", "env", "step"), "manifest")

    gogid = raw.get("gogid")
    command = raw.get("command")
    if (gogid is None) == (command is None):
        raise ManifestError("needs exactly one of gogid (a game) or command (self-test)")
    if gogid is not None and (isinstance(gogid, bool) or not isinstance(gogid, int)):
        raise ManifestError("gogid must be an integer")
    if command is not None and (
        not isinstance(command, list)
        or not command
        or not all(isinstance(c, str) for c in command)
    ):
        raise ManifestError("command must be a non-empty list of strings")

    window = raw.get("window", {})
    _only(window, ("wait_secs", "resolution", "title", "min_fill"), "window")
    resolution = window.get("resolution", "1280x800")
    m = re.fullmatch(r"([1-9][0-9]{2,4})x([1-9][0-9]{2,4})", str(resolution))
    if not m:
        raise ManifestError(f"window.resolution: want WIDTHxHEIGHT, got {resolution!r}")
    title = window.get("title")
    if title is not None:
        try:
            re.compile(title)
        except (re.error, TypeError) as e:
            raise ManifestError(f"window.title: not a regex: {e}") from e

    exit_ = raw.get("exit", {})
    _only(exit_, ("how", "wait_secs", "code"), "exit")
    how = exit_.get("how", "signal")
    if how not in EXIT_HOW:
        raise ManifestError(f"exit.how: want one of {EXIT_HOW}, got {how!r}")

    env = raw.get("env", {})
    if not isinstance(env, dict) or not all(isinstance(v, str) for v in env.values()):
        raise ManifestError("env: must be a table of strings")

    steps_raw = raw.get("step", [])
    if not isinstance(steps_raw, list) or not steps_raw:
        raise ManifestError("needs at least one [[step]]")
    shots = set()
    steps = [_step(s, i, shots) for i, s in enumerate(steps_raw)]

    return {
        "gogid": gogid,
        "title": raw.get("title", str(gogid) if gogid else "self-test"),
        "command": command,
        "window": {
            "wait_secs": _number(window.get("wait_secs", 180), "window.wait_secs"),
            "width": int(m.group(1)),
            "height": int(m.group(2)),
            "title": title,
            "min_fill": _min_fill(window.get("min_fill", {}), "window.min_fill"),
        },
        "exit": {
            "how": how,
            "wait_secs": _number(exit_.get("wait_secs", 30), "exit.wait_secs"),
            "code": int(_number(exit_.get("code", 0), "exit.code")),
        },
        "env": env,
        "steps": steps,
    }


# --- PNG ----------------------------------------------------------------------


def read_png(path):
    """Decode an 8-bit RGB/RGBA PNG (what grim writes): (width, height, channels, rows)."""
    with open(path, "rb") as f:
        data = f.read()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError(f"{path}: not a PNG")
    pos = 8
    idat = []
    width = height = channels = None
    while pos < len(data):
        length, kind = struct.unpack(">I4s", data[pos : pos + 8])
        body = data[pos + 8 : pos + 8 + length]
        if kind == b"IHDR":
            width, height, depth, colour, _, _, interlace = struct.unpack(">IIBBBBB", body)
            channels = {2: 3, 6: 4}.get(colour)
            if depth != 8 or channels is None or interlace:
                raise ValueError(f"{path}: want 8-bit RGB(A), non-interlaced")
        elif kind == b"IDAT":
            idat.append(body)
        elif kind == b"IEND":
            break
        pos += 12 + length
    if width is None:
        raise ValueError(f"{path}: no IHDR")
    raw = zlib.decompress(b"".join(idat))
    stride = width * channels
    if len(raw) != (stride + 1) * height:
        raise ValueError(f"{path}: truncated image data")
    rows = []
    prior = bytes(stride)
    for y in range(height):
        start = y * (stride + 1)
        kind = raw[start]
        line = bytearray(raw[start + 1 : start + 1 + stride])
        if kind == 1:
            for i in range(channels, stride):
                line[i] = (line[i] + line[i - channels]) & 255
        elif kind == 2:
            for i in range(stride):
                line[i] = (line[i] + prior[i]) & 255
        elif kind == 3:
            for i in range(stride):
                left = line[i - channels] if i >= channels else 0
                line[i] = (line[i] + ((left + prior[i]) >> 1)) & 255
        elif kind == 4:
            for i in range(stride):
                a = line[i - channels] if i >= channels else 0
                b = prior[i]
                c = prior[i - channels] if i >= channels else 0
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                pred = a if pa <= pb and pa <= pc else (b if pb <= pc else c)
                line[i] = (line[i] + pred) & 255
        elif kind != 0:
            raise ValueError(f"{path}: unknown PNG filter {kind}")
        prior = bytes(line)
        rows.append(prior)
    return width, height, channels, rows


def analyze(image, region=None):
    """Non-black share and bounding box inside `region` (x, y, w, h; default all).

    The box is what "the picture fills the window" reads: 800x600 drawn in the
    top-left of a 1280x800 window is a box of 62% x 75%.
    """
    width, height, channels, rows = image
    x0, y0, w, h = region or (0, 0, width, height)
    x0, y0 = max(0, x0), max(0, y0)
    x1, y1 = min(width, x0 + w), min(height, y0 + h)
    w, h = x1 - x0, y1 - y0
    if w <= 0 or h <= 0:
        raise ValueError(f"region {region} is outside a {width}x{height} image")
    nonblack = 0
    left, right, top, bottom = None, None, None, None
    # A pixel is black when each colour channel is at most BLACK_LEVEL.
    for y in range(y0, y1):
        row = rows[y]
        count = 0
        first = last = None
        for x in range(x0, x1):
            p = x * channels
            if (
                row[p] > BLACK_LEVEL
                or row[p + 1] > BLACK_LEVEL
                or row[p + 2] > BLACK_LEVEL
            ):
                count += 1
                if first is None:
                    first = x
                last = x
        if count:
            nonblack += count
            left = first if left is None else min(left, first)
            right = last if right is None else max(right, last)
            top = y if top is None else top
            bottom = y
    total = w * h
    box = (
        None
        if left is None
        else {
            "x": left - x0,
            "y": top - y0,
            "w": right - left + 1,
            "h": bottom - top + 1,
        }
    )
    return {
        "width": w,
        "height": h,
        "nonblack_pct": round(100.0 * nonblack / total, 2),
        "box": box,
        "box_w_pct": round(100.0 * box["w"] / w, 2) if box else 0.0,
        "box_h_pct": round(100.0 * box["h"] / h, 2) if box else 0.0,
    }


def changed_pct(a, b):
    """Share of pixels that differ by more than CHANGE_LEVEL in any colour channel."""
    wa, ha, ca, rows_a = a
    wb, hb, cb, rows_b = b
    if (wa, ha) != (wb, hb):
        return 100.0
    changed = 0
    for ra, rb in zip(rows_a, rows_b):
        if ca == cb and ra == rb:
            continue
        for x in range(wa):
            pa, pb = x * ca, x * cb
            if (
                abs(ra[pa] - rb[pb]) > CHANGE_LEVEL
                or abs(ra[pa + 1] - rb[pb + 1]) > CHANGE_LEVEL
                or abs(ra[pa + 2] - rb[pb + 2]) > CHANGE_LEVEL
            ):
                changed += 1
    return round(100.0 * changed / (wa * ha), 2)


# --- sway views ---------------------------------------------------------------


def walk_views(node):
    """Mapped views (windows) in a `swaymsg -t get_tree` tree."""
    for child in node.get("nodes", []) + node.get("floating_nodes", []):
        if child.get("pid") and child.get("type") in ("con", "floating_con"):
            yield child
        yield from walk_views(child)


def pick_view(tree, title_regex=None):
    """The window under test: the largest visible one, or the largest whose title matches."""
    best = None
    for view in walk_views(tree):
        if not view.get("visible", True):
            continue
        rect = view.get("rect", {})
        if rect.get("width", 0) <= 0 or rect.get("height", 0) <= 0:
            continue
        name = view.get("name") or ""
        if title_regex and not re.search(title_regex, name):
            continue
        area = rect["width"] * rect["height"]
        if best is None or area > best[0]:
            best = (area, view)
    return best[1] if best else None


def click_point(rect, x_pct, y_pct):
    """A click's output coordinates from its percentage of the window."""
    x = rect["x"] + rect["width"] * x_pct / 100.0
    y = rect["y"] + rect["height"] * y_pct / 100.0
    return (
        min(int(x), rect["x"] + rect["width"] - 1),
        min(int(y), rect["y"] + rect["height"] - 1),
    )


# --- a pointer for the seat ----------------------------------------------------


def _wl_string(text):
    raw = text.encode() + b"\0"
    return struct.pack("<I", len(raw)) + raw + b"\0" * (-len(raw) % 4)


def _wl_message(obj, opcode, body=b""):
    return struct.pack("<II", obj, (8 + len(body)) << 16 | opcode) + body


def _wl_globals(data):
    """(name, version) of each wl_registry.global in the event bytes, by interface."""
    found = {}
    pos = 0
    while pos + 8 <= len(data):
        obj, word = struct.unpack_from("<II", data, pos)
        size, opcode = word >> 16, word & 0xFFFF
        if size < 8 or pos + size > len(data):
            break
        if obj == 2 and opcode == 0:
            name, length = struct.unpack_from("<II", data, pos + 8)
            iface = data[pos + 16 : pos + 16 + length - 1].decode()
            version = struct.unpack_from("<I", data, pos + 16 + (length + 3) // 4 * 4)[0]
            found[iface] = (name, version)
        pos += size
    return found


def virtual_pointer(socket_path):
    """Give the seat a pointer, for as long as the returned socket stays open.

    A headless sway has no input devices, so its seat offers no wl_pointer,
    Xwayland has no pointer, and `seat cursor press` reaches nobody. This asks
    for a wlr virtual pointer and just holds it: sway's seat commands do the rest.
    """
    conn = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    conn.connect(socket_path)
    # get_registry (new id 2), then sync (new id 3): the sync's done event ends
    # the globals.
    conn.sendall(
        _wl_message(1, 1, struct.pack("<I", 2)) + _wl_message(1, 0, struct.pack("<I", 3))
    )
    data = b""
    done = struct.pack("<II", 3, 12 << 16)
    while done not in data:
        chunk = conn.recv(65536)
        if not chunk:
            raise RuntimeError("the compositor closed the Wayland connection")
        data += chunk
    iface = "zwlr_virtual_pointer_manager_v1"
    if iface not in (found := _wl_globals(data)):
        raise RuntimeError(f"the compositor has no {iface}")
    name, _ = found[iface]
    # wl_registry.bind (new id 4), create_virtual_pointer(seat: null, new id 5),
    # sync (new id 6) so we return with the device in place.
    conn.sendall(
        _wl_message(
            2, 0, struct.pack("<I", name) + _wl_string(iface) + struct.pack("<II", 1, 4)
        )
        + _wl_message(4, 0, struct.pack("<II", 0, 5))
        + _wl_message(1, 0, struct.pack("<I", 6))
    )
    data = b""
    done = struct.pack("<II", 6, 12 << 16)
    while done not in data:
        chunk = conn.recv(65536)
        if not chunk:
            raise RuntimeError("the compositor refused the virtual pointer")
        data += chunk
    return conn


# --- drive: inside the sandbox ------------------------------------------------


class Stop(Exception):
    """The outer script asked us to wrap up (timeout, Ctrl-C)."""


class Driver:
    def __init__(self, config):
        self.cfg = config
        self.manifest = config["manifest"]
        self.run_dir = config["run_dir"]
        self.results = config["results_dir"]
        self.env = dict(os.environ)
        self.sway = None
        self.pointer = None
        self.game = None
        self.game_exit = None
        self.view = None
        self.shots = {}
        self.checks = []
        self.steps = []
        self.result = {
            "isolation": {},
            "gpu": {},
            "steps": self.steps,
            "checks": self.checks,
            "game_exit": None,
            "doctor": None,
            "stopped": False,
        }

    def save(self):
        path = os.path.join(self.results, "result.json")
        tmp = path + ".tmp"
        with open(tmp, "w") as f:
            json.dump(self.result, f, indent=2)
        os.replace(tmp, path)

    def check(self, name, ok, detail=""):
        self.checks.append({"name": name, "ok": bool(ok), "detail": str(detail)})
        self.save()
        return ok

    def sleep(self, secs):
        end = time.monotonic() + secs
        while True:
            if os.path.exists(self.cfg["stop_file"]):
                self.result["stopped"] = True
                raise Stop()
            left = end - time.monotonic()
            if left <= 0:
                return
            time.sleep(min(left, 0.25))

    def tool(self, *argv, timeout=20, env=None):
        return subprocess.run(
            argv,
            env=env or self.env,
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
        )

    def swaymsg(self, *args):
        r = self.tool("swaymsg", *args)
        if r.returncode != 0:
            raise RuntimeError(f"swaymsg {' '.join(args)}: {r.stdout}{r.stderr}".strip())
        return r.stdout

    # sway ----------------------------------------------------------------

    def start_sway(self):
        w, h = self.manifest["window"]["width"], self.manifest["window"]["height"]
        env_file = os.path.join(self.run_dir, "sway.env")
        conf = os.path.join(self.run_dir, "sway.conf")
        with open(conf, "w") as f:
            f.write(
                "xwayland force\n"
                f"output HEADLESS-1 resolution {w}x{h} scale 1\n"
                "default_border none\n"
                "focus_follows_mouse no\n"
                f'exec env -0 > "{env_file}.tmp" && mv "{env_file}.tmp" "{env_file}"\n'
            )
        log = open(os.path.join(self.results, "sway.log"), "wb")
        self.sway = subprocess.Popen(
            ["sway", "--unsupported-gpu", "--debug", "--config", conf],
            env=self.env,
            stdin=subprocess.DEVNULL,
            stdout=log,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        deadline = time.monotonic() + 30
        while not os.path.exists(env_file):
            if self.sway.poll() is not None:
                raise RuntimeError(
                    f"sway exited {self.sway.returncode} at start (see sway.log)"
                )
            if time.monotonic() > deadline:
                raise RuntimeError("sway didn't start in 30 s (see sway.log)")
            self.sleep(0.1)
        with open(env_file, "rb") as f:
            for item in f.read().split(b"\0"):
                key, _, value = item.decode(errors="replace").partition("=")
                if key in ("WAYLAND_DISPLAY", "SWAYSOCK", "DISPLAY"):
                    self.env[key] = value
        # Xwayland comes up after sway's config: wait for the socket and for
        # an answer.
        while time.monotonic() < deadline:
            if self.env.get("DISPLAY"):
                r = self.tool("xwininfo", "-root", "-display", self.env["DISPLAY"])
                if r.returncode == 0:
                    self.pointer = virtual_pointer(
                        os.path.join(self.env["XDG_RUNTIME_DIR"], self.env["WAYLAND_DISPLAY"])
                    )
                    return
            self.sleep(0.2)
        raise RuntimeError(f"no X server on {self.env.get('DISPLAY')!r} (see sway.log)")

    # isolation -----------------------------------------------------------

    def assert_isolation(self):
        iso = self.result["isolation"]
        c = self.cfg

        netns = os.readlink("/proc/self/ns/net")
        with open("/proc/net/dev") as f:
            links = [line.split(":")[0].strip() for line in f if ":" in line]
        iso["netns"] = {"ns": netns, "links": links}
        self.check(
            "isolation_netns",
            netns != c["host_netns"] and links == ["lo"],
            f"{netns}, links {links}",
        )

        pidns = os.readlink("/proc/self/ns/pid")
        self.check("isolation_pidns", pidns != c["host_pidns"], pidns)

        tmp_fs = _mount_fstype("/tmp")
        tmp_entries = sorted(os.listdir("/tmp"))
        foreign = [e for e in tmp_entries if not e.startswith((".X", "sway-ipc"))]
        self.check(
            "isolation_tmp",
            tmp_fs == "tmpfs" and not foreign,
            f"{tmp_fs}, entries {tmp_entries}",
        )

        runtime = self.env.get("XDG_RUNTIME_DIR", "")
        self.check(
            "isolation_runtime_dir",
            runtime == c["runtime_dir"] and _mount_fstype("/run") == "tmpfs",
            runtime,
        )

        leaked = sorted(k for k in os.environ if k.startswith("HYPRLAND_"))
        self.check("isolation_env", not leaked, f"inherited: {leaked}" if leaked else "")

        visible = [p for p in c["forbidden_paths"] if os.path.lexists(p)]
        self.check(
            "isolation_paths",
            not visible,
            f"visible: {visible}" if visible else f"{len(c['forbidden_paths'])} hidden",
        )

        w, h = self.manifest["window"]["width"], self.manifest["window"]["height"]
        display = self.env.get("DISPLAY", "")
        info = self.tool("xwininfo", "-root", "-display", display).stdout
        mw = re.search(r"Width:\s*(\d+)", info)
        mh = re.search(r"Height:\s*(\d+)", info)
        got = f"{mw.group(1)}x{mh.group(1)}" if mw and mh else "?"
        socket = f"/tmp/.X11-unix/X{display.lstrip(':').split('.')[0]}"
        iso["display"] = {"DISPLAY": display, "root": got, "socket": socket}
        self.check(
            "isolation_display",
            got == f"{w}x{h}" and os.path.exists(socket),
            f"{display} root {got}, socket {socket}",
        )

        outputs = json.loads(self.swaymsg("-t", "get_outputs", "--raw"))
        names = [o.get("name") for o in outputs]
        iso["outputs"] = names
        self.check(
            "isolation_headless",
            bool(names) and all(str(n).startswith("HEADLESS-") for n in names),
            names,
        )

        nodes = _device_nodes()
        iso["gpu_nodes"] = nodes
        self.check(
            "isolation_gpu_nodes",
            sorted(nodes) == sorted(c["gpu"]["nodes"]),
            f"{nodes or 'none'}",
        )

    # GPU -----------------------------------------------------------------

    def probe_gpu(self):
        """What GL and Vulkan say they are, in the environment the game gets."""
        gpu = self.result["gpu"]
        gpu["label"] = self.cfg["gpu"]["label"]
        env = dict(self.env)
        env.update(self.cfg["game_env"])
        gpu["gl_renderer"] = None
        if shutil.which("glxinfo", path=env["PATH"]):
            r = self.tool("glxinfo", "-B", env=env)
            m = re.search(r"OpenGL renderer string:\s*(.+)", r.stdout)
            gpu["gl_renderer"] = m.group(1).strip() if m else None
        gpu["vulkan_devices"] = []
        if shutil.which("vulkaninfo", path=env["PATH"]):
            r = self.tool("vulkaninfo", "--summary", env=env)
            gpu["vulkan_devices"] = re.findall(r"deviceName\s*=\s*(.+)", r.stdout)
        expect = self.cfg["gpu"]["expect"]
        if self.manifest["command"] and not gpu["gl_renderer"]:
            return  # self-test on a host with no GL driver to find: says nothing.
        self.check(
            "gpu_gl",
            bool(gpu["gl_renderer"]) and re.search(expect, gpu["gl_renderer"], re.I),
            gpu["gl_renderer"],
        )
        if self.manifest["command"] and not gpu["vulkan_devices"]:
            return
        self.check(
            "gpu_vulkan",
            bool(gpu["vulkan_devices"])
            and all(re.search(expect, d, re.I) for d in gpu["vulkan_devices"]),
            gpu["vulkan_devices"],
        )

    def game_gpu_fds(self):
        """Which processes hold a GPU node open: what the game really uses."""
        held = {}
        sway_side = {"sway", "Xwayland"}
        for pid in filter(str.isdigit, os.listdir("/proc")):
            try:
                with open(f"/proc/{pid}/comm") as f:
                    comm = f.read().strip()
                if comm in sway_side:
                    continue
                for fd in os.listdir(f"/proc/{pid}/fd"):
                    target = os.readlink(f"/proc/{pid}/fd/{fd}")
                    if target.startswith(("/dev/dri/", "/dev/nvidia")):
                        held.setdefault(comm, set()).add(target)
            except OSError:
                continue
        return {k: sorted(v) for k, v in sorted(held.items())}

    # game ----------------------------------------------------------------

    def launch(self):
        env = dict(self.env)
        env.update(self.cfg["game_env"])
        env.update(self.manifest["env"])
        out = open(os.path.join(self.results, "game.out"), "wb")
        self.game = subprocess.Popen(
            self.cfg["launch"],
            env=env,
            cwd=self.cfg["launch_cwd"],
            stdin=subprocess.DEVNULL,
            stdout=out,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )

    def wait_window(self):
        window = self.manifest["window"]
        start = time.monotonic()
        while time.monotonic() - start < window["wait_secs"]:
            tree = json.loads(self.swaymsg("-t", "get_tree", "--raw"))
            view = pick_view(tree, window["title"])
            if view:
                self.view = view
                return self.check(
                    "window",
                    True,
                    f"{view.get('name')!r} {view['rect']['width']}x"
                    f"{view['rect']['height']} after {time.monotonic() - start:.0f}s",
                )
            if self.game.poll() is not None:
                self.game_exit = self.game.returncode
                self.result["game_exit"] = self.game_exit
                return self.check(
                    "window", False, f"game exited {self.game_exit} with no window"
                )
            self.sleep(0.5)
        return self.check("window", False, f"none in {window['wait_secs']:.0f}s")

    def refresh_view(self):
        tree = json.loads(self.swaymsg("-t", "get_tree", "--raw"))
        view = pick_view(tree, self.manifest["window"]["title"])
        if view:
            self.view = view
        return view

    def run_step(self, step):
        action = next(k for k in step if k != "min_fill")
        record = {"action": action, "arg": step[action], "ok": True}
        self.steps.append(record)
        if action == "wait":
            self.sleep(step["wait"])
        elif action == "wait_settled":
            self.do_wait_settled(step["wait_settled"], record)
        elif action == "key":
            self.tool(*_wtype("-k", step["key"])).check_returncode()
        elif action == "type":
            self.tool(*_wtype(step["type"])).check_returncode()
        elif action == "click":
            self.do_click(step["click"], record)
        elif action == "screenshot":
            self.do_screenshot(step, record)
        elif action == "expect_change":
            self.do_expect_change(step["expect_change"], record)
        self.save()

    def do_click(self, at, record):
        view = self.refresh_view()
        if not view:
            record["ok"] = False
            self.check("input_click", False, "no window to click")
            return
        x, y = click_point(view["rect"], at["x"], at["y"])
        record["at"] = [x, y]
        self.swaymsg("seat", "seat0", "cursor", "set", str(x), str(y))
        time.sleep(0.2)
        self.swaymsg("seat", "seat0", "cursor", "press", "button1")
        time.sleep(0.1)
        self.swaymsg("seat", "seat0", "cursor", "release", "button1")

    def do_wait_settled(self, secs, record):
        """Poll until the picture fills the window and holds still: past any
        intro video, however long the renderer takes to get through it."""
        path = os.path.join(self.run_dir, "settling.png")
        fill = self.manifest["window"]["min_fill"]
        start = time.monotonic()
        prior = None
        last = None
        while time.monotonic() - start < secs:
            if self.game.poll() is not None:
                break
            r = self.tool("grim", path)
            if r.returncode == 0:
                self.refresh_view()
                image = read_png(path)
                rect = self.view["rect"]
                last = analyze(image, (rect["x"], rect["y"], rect["width"], rect["height"]))
                fills = last["box_w_pct"] >= fill["w"] and last["box_h_pct"] >= fill["h"]
                if fills and prior is not None and changed_pct(prior, image) <= SETTLE_MAX_CHANGE_PCT:
                    record["after_secs"] = round(time.monotonic() - start, 1)
                    self.check("settled", True, f"after {record['after_secs']}s")
                    return
                prior = image
            self.sleep(SETTLE_POLL_SECS)
        record["ok"] = False
        self.check("settled", False, f"not in {secs:.0f}s; last frame {last}")

    def do_screenshot(self, step, record):
        name = step["screenshot"]
        path = os.path.join(self.cfg["shots_dir"], f"{name}.png")
        r = self.tool("grim", path)
        if r.returncode != 0:
            record["ok"] = False
            self.check(f"screenshot_{name}", False, r.stderr.strip())
            return
        self.refresh_view()
        image = read_png(path)
        rect = self.view["rect"] if self.view else None
        region = (rect["x"], rect["y"], rect["width"], rect["height"]) if rect else None
        stats = analyze(image, region)
        record.update(path=path, stats=stats)
        self.shots[name] = image
        self.check(
            f"not_black_{name}",
            stats["nonblack_pct"] >= MIN_NONBLACK_PCT,
            f"{stats['nonblack_pct']}% non-black",
        )
        fill = step.get("min_fill", self.manifest["window"]["min_fill"])
        if fill is not False:
            self.check(
                f"fills_window_{name}",
                stats["box_w_pct"] >= fill["w"] and stats["box_h_pct"] >= fill["h"],
                f"picture {stats['box_w_pct']}% x {stats['box_h_pct']}% of the "
                f"window, want {fill['w']}% x {fill['h']}%",
            )
        # While something renders, record who holds a GPU node.
        if "gpu_fds" not in self.result["gpu"]:
            self.result["gpu"]["gpu_fds"] = self.game_gpu_fds()

    def do_expect_change(self, want, record):
        a, b = self.shots.get(want["from"]), self.shots.get(want["to"])
        name = f"changed_{want['from']}_{want['to']}"
        if a is None or b is None:
            record["ok"] = False
            self.check(name, False, "a screenshot it needs is missing")
            return
        pct = changed_pct(a, b)
        record["changed_pct"] = pct
        record["ok"] = pct >= want["min"]
        self.check(name, record["ok"], f"{pct}% of pixels changed, want {want['min']}%")

    def check_gpu_used(self):
        """--gpu: some game process has the chosen node open."""
        node = self.cfg["gpu"].get("game_node")
        if not node or self.manifest["command"]:
            return
        held = self.result["gpu"].get("gpu_fds", {})
        users = sorted(k for k, v in held.items() if node in v)
        self.check("gpu_used_by_game", bool(users), f"{node}: {users or held or 'nobody'}")

    def finish_game(self, how):
        """The game's end: on its own, or the launcher's SIGTERM teardown."""
        if self.game is None:
            return
        if self.game.poll() is None and how == "self":
            try:
                self.game.wait(self.manifest["exit"]["wait_secs"])
            except subprocess.TimeoutExpired:
                # What it shows instead of exiting: for a person to look at.
                self.tool("grim", os.path.join(self.cfg["shots_dir"], "not-exited.png"))
                self.check(
                    "clean_exit",
                    False,
                    f"still running {self.manifest['exit']['wait_secs']:.0f}s "
                    "after its last step",
                )
                how = "cleanup"
        elif self.game.poll() is not None and how == "signal":
            self.check("torn_down", False, f"exited {self.game.returncode} on its own")
            how = "exited"
        if self.game.poll() is None:
            self.game.send_signal(signal.SIGTERM)
            try:
                self.game.wait(30)
            except subprocess.TimeoutExpired:
                self.check("torn_down", False, "launcher still up 30 s after SIGTERM")
                os.killpg(self.game.pid, signal.SIGKILL)
                self.game.wait()
                how = "cleanup"
        self.game_exit = self.game.returncode
        self.result["game_exit"] = self.game_exit
        if how == "self":
            self.check(
                "clean_exit",
                self.game_exit == self.manifest["exit"]["code"],
                f"exit {self.game_exit}",
            )
        elif how == "signal":
            # The launcher's own footer for SIGTERM is 143.
            self.check(
                "torn_down", self.game_exit in (0, 143), f"launcher exit {self.game_exit}"
            )

    def doctor(self, name):
        argv = self.cfg.get("doctor")
        if not argv:
            return None
        r = self.tool(*argv, timeout=60)
        path = os.path.join(self.results, f"doctor-{name}.json")
        with open(path, "w") as f:
            f.write(r.stdout)
        try:
            return json.loads(r.stdout)
        except json.JSONDecodeError:
            return {"error": (r.stderr or r.stdout).strip()[-400:]}

    def check_doctor(self):
        report = self.doctor("after")
        if report is None:
            return
        self.result["doctor"] = report
        findings = report.get("findings", [])
        for check in ("links", "processes"):
            lines = [f for f in findings if f.get("check") == check]
            bad = [f for f in lines if f.get("level") != "ok"]
            self.check(
                f"doctor_{check}",
                bool(lines) and not bad,
                "; ".join(f.get("message", "") for f in lines) or report.get("error", "no line"),
            )

    def main(self):
        how = self.manifest["exit"]["how"]
        try:
            self.start_sway()
            self.assert_isolation()
            bad = [c for c in self.checks if not c["ok"]]
            if bad:
                # A display that isn't provably ours: never launch.
                raise RuntimeError(f"isolation failed: {bad[0]['name']}: {bad[0]['detail']}")
            self.probe_gpu()
            self.doctor("before")
            self.launch()
            if self.wait_window():
                for step in self.manifest["steps"]:
                    self.run_step(step)
                self.check_gpu_used()
                self.finish_game(how)
                self.check_doctor()
                self.check("finished", True, "every step ran")
            else:
                self.finish_game("cleanup")
                self.check("finished", True, "no window, so no steps")
        except Stop:
            self.check("finished", False, "stopped from outside (timeout or signal)")
            self.finish_game("cleanup")
        except Exception as e:  # recorded as a failed check: the run still reports
            self.check("finished", False, f"{type(e).__name__}: {e}")
            self.finish_game("cleanup")
        finally:
            self.save()
            if self.sway and self.sway.poll() is None:
                self.tool("swaymsg", "exit")
                try:
                    self.sway.wait(5)
                except subprocess.TimeoutExpired:
                    self.sway.kill()


def _wtype(*keys):
    """wtype is a new keyboard every time: Xwayland needs a moment for its keymap."""
    return ("wtype", "-s", "500", *keys, "-s", "200")


def _mount_fstype(path):
    found = None
    with open("/proc/self/mountinfo") as f:
        for line in f:
            fields = line.split()
            if fields[4] == path:
                found = fields[fields.index("-") + 1]
    return found


def _device_nodes():
    nodes = []
    if os.path.isdir("/dev/dri"):
        nodes += [f"/dev/dri/{n}" for n in os.listdir("/dev/dri")]
    nodes += [f"/dev/{n}" for n in os.listdir("/dev") if n.startswith("nvidia")]
    return sorted(nodes)


# --- CLI ----------------------------------------------------------------------


def main(argv):
    if len(argv) >= 3 and argv[1] == "manifest":
        try:
            with open(argv[2], encoding="utf-8") as f:
                print(json.dumps(parse_manifest(f.read())))
        except (OSError, ManifestError) as e:
            print(f"{argv[2]}: {e}", file=sys.stderr)
            return 2
        return 0
    if len(argv) in (3, 7) and argv[1] == "image":
        region = tuple(int(v) for v in argv[3:]) if len(argv) == 7 else None
        print(json.dumps(analyze(read_png(argv[2]), region)))
        return 0
    if len(argv) == 4 and argv[1] == "diff":
        print(json.dumps({"changed_pct": changed_pct(read_png(argv[2]), read_png(argv[3]))}))
        return 0
    if len(argv) == 3 and argv[1] == "drive":
        with open(argv[2], encoding="utf-8") as f:
            Driver(json.load(f)).main()
        return 0
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
