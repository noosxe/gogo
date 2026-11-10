#!/usr/bin/env python3
"""The half of scripts/linux-e2e.sh that isn't shell (docs/linux-e2e.md).

Run with `python3 -I`: stdlib only, nothing from the working directory.

    linux-e2e-helper.py manifest <file>        validate; print it as JSON (exit 2: bad)
    linux-e2e-helper.py image <png> [w h x y]  print image stats as JSON
    linux-e2e-helper.py diff <a.png> <b.png>   print the changed fraction as JSON
    linux-e2e-helper.py drive <config.json>    inside the sandbox: run a whole game
    linux-e2e-helper.py tree-check <tree> <run> <guard>…
                                               may an installed base be reused (exit 1: no)
    linux-e2e-helper.py manifest --live <file> the same, for a live session: no steps needed
    linux-e2e-helper.py live-call <timeout> <cmd> [arg…]
                                               one command to a live session's socket,
                                               in $GOGO_E2E_LIVE_DIR
    linux-e2e-helper.py live-wait <state> <secs> <owner-pid> <life>
                                               until a live session is ready (exit 4: still busy)
    linux-e2e-helper.py live-export <session.json>
                                               a live session's path as a manifest
    linux-e2e-helper.py subvol create <path>   a new btrfs subvolume (exit 1: couldn't)
    linux-e2e-helper.py subvol snapshot <src> <path>
                                               a writable snapshot of subvolume src

`drive` starts headless sway, proves the display is the headless one, launches
the game, runs the manifest's steps and writes result.json after every step, so
a killed run still has what it got that far. With `build_command` in its config
it runs that on the display instead (the build phase): it exits with the
command's status, or BUILD_NO_DISPLAY when it never got to run it. With
`live` in its config it serves a live session's socket instead of running
steps (#241), and exits LIVE_RELAUNCH when a restore wants the next life. A
`relaunch` step (#252) launches the game again in the same sandbox, after it
exited by itself.
"""

import fcntl
import hashlib
import json
import math
import os
import re
import shutil
import signal
import select
import shlex
import socket
import stat
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
# `hold`: the longest a key may be held down, in ms.
HOLD_MAX_MS = 10000
# press_until: the most presses, and the longest pause between two.
PRESS_MAX = 1000
PRESS_EVERY_MAX = 120.0
# How far a channel may be off `marked`'s colour, by default; and its frames in a
# row, so a frame in passing doesn't count.
MARKED_TOL = 40
MARKED_HITS = 2
MARKED_HIT_GAP = 0.2
# wait_settled: two frames this far apart, this close, count as holding still.
SETTLE_POLL_SECS = 2.0
SETTLE_MAX_CHANGE_PCT = 1.0
EXIT_HOW = ("self", "signal")
# How the game reaches the display (#264): through Xwayland, as on most desktops
# today, or as a Wayland client with no X server in the sandbox at all.
DISPLAYS = ("xwayland", "wayland")
# A manifest's [gpu] wayland: "unused" for a game that draws without the GPU
# on Wine's Wayland driver (GDI into shm buffers).
GPU_WAYLAND = ("used", "unused")
# gogo's own per-game display mode and HDR switch (#292), for [override].
GOGO_DISPLAYS = ("wayland", "x11")
GOGO_HDR = ("auto", "on", "force", "off")
# What a process that wanted X says when there is none: Wine's X11 driver
# loading, or Xlib, GTK, Qt and SDL failing to open a display. Proton's own
# "failed to run xrandr" (its primary-monitor guess) is noise, and not matched.
X_FALLBACK = re.compile(
    r"winex11|can(?:'|no)t open display|unable to open display|failed to open display"
    r"|Make sure that your X server is running|No protocol specified"
    r"|could not connect to (?:any )?X display|X11 connection rejected",
    re.I,
)
NULL_SINK = "e2e-null"
BUILD_NO_DISPLAY = 3
# The `move_relaunch` step's targets (#278): a dir on the scratch's filesystem, a
# tmpfs of the sandbox's own (another filesystem, outside $HOME), and `games/`.
MOVE_TARGETS = ("library", "other_fs", "default")
# A relaunch: how long what the game left behind (wineserver lingers) gets to go.
RELAUNCH_LEFT_SECS = 15.0
# How the build's log says the support installer ended (build.rs).
SUPPORT_LINES = (
    (re.compile(r"GOG support installer finished \(exit (\d+)\)"), "finished"),
    (re.compile(r"GOG support installer declined \(exit (-?\d+)"), "declined"),
    (re.compile(r"GOG support installer still running after \d+ s"), "stopped"),
)


def tmp_foreign(entries, bind_roots, tmp="/tmp"):
    """The /tmp entries that are neither X/sway sockets nor the parent
    directories bwrap made for the harness's own binds (#225: a checkout under
    /tmp). Each directory on the way to a bind root may hold nothing but the
    next step of that path; the root itself is a mount and not looked into."""
    roots = [os.path.normpath(r) for r in bind_roots]
    out = []

    def walk(d, names):
        for e in names:
            path = os.path.join(d, e)
            if d == tmp and e.startswith((".X", "sway-ipc")):
                continue
            if path in roots:
                continue
            if any(r.startswith(path + "/") for r in roots):
                try:
                    walk(path, sorted(os.listdir(path)))
                except OSError:
                    out.append(path)
                continue
            out.append(path)

    walk(tmp, entries)
    return [os.path.relpath(p, tmp) for p in out]


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


def _marked(raw, what):
    """press_until's `marked`: a region, in % of the window, and how much of it
    must be near a colour. A colour is what a manifest can say of a game's screen
    with no game art in git: a focus frame, say, that nothing else there has."""
    _only(raw, ("x", "y", "w", "h", "color", "tol", "min"), what)
    for key in ("x", "y", "w", "h", "color", "min"):
        if key not in raw:
            raise ManifestError(f"{what}: needs x, y, w, h, color and min")
    box = {k: _percent(raw[k], f"{what}.{k}") for k in ("x", "y", "w", "h")}
    if box["w"] <= 0 or box["h"] <= 0 or box["x"] + box["w"] > 100 or box["y"] + box["h"] > 100:
        raise ManifestError(f"{what}: the region must have a size and be inside the window")
    color = raw["color"]
    if not isinstance(color, str) or not re.fullmatch(r"#[0-9a-fA-F]{6}", color):
        raise ManifestError(f"{what}.color: want #rrggbb, got {color!r}")
    tol = raw.get("tol", MARKED_TOL)
    if isinstance(tol, bool) or not isinstance(tol, int) or not 0 <= tol <= 255:
        raise ManifestError(f"{what}.tol: a whole number, 0-255")
    least = _percent(raw["min"], f"{what}.min")
    if least <= 0:
        raise ManifestError(f"{what}.min: must be over 0")
    return {**box, "color": color.lower(), "tol": tol, "min": least}


def _press_until(value, what):
    """press_until: a key, again and again, until the screen says it took."""
    _only(value, ("key", "change", "marked", "every", "max"), what)
    if value.get("key") not in KEYS:
        raise ManifestError(f"{what}.key {value.get('key')!r} isn't a key of the us keymap the harness knows")
    if ("change" in value) == ("marked" in value):
        raise ManifestError(f"{what}: needs exactly one of change or marked")
    if "every" not in value or "max" not in value:
        raise ManifestError(f"{what}: needs every (s between presses) and max (presses): no limit is guessed")
    every = _number(value["every"], f"{what}.every")
    if not 0 < every <= PRESS_EVERY_MAX:
        raise ManifestError(f"{what}.every: over 0, at most {PRESS_EVERY_MAX:.0f} s")
    most = value["max"]
    if isinstance(most, bool) or not isinstance(most, int) or not 1 <= most <= PRESS_MAX:
        raise ManifestError(f"{what}.max: a whole number of presses, 1-{PRESS_MAX}")
    step = {"key": value["key"]}
    if "change" in value:
        step["change"] = _percent(value["change"], f"{what}.change")
        if step["change"] <= 0:
            raise ManifestError(f"{what}.change: must be over 0")
    else:
        step["marked"] = _marked(value["marked"], f"{what}.marked")
    step.update(every=every, max=most)
    return step


def _wait_until(value, what):
    """wait_until: as press_until's `marked`, with no key: `max` looks, `every` s apart."""
    _only(value, ("marked", "every", "max"), what)
    if "marked" not in value:
        raise ManifestError(f"{what}: needs marked")
    step = _press_until({"key": "space", **value}, what)
    del step["key"]
    return step


def tree_digest(prefix):
    """What a move must keep of a prefix: every path under `drive_c/users` with its
    bytes, the registry, and where every symlink outside the game's files leads."""
    h = hashlib.sha256()
    for top, dirs, files in os.walk(prefix):
        dirs.sort()
        rel = os.path.relpath(top, prefix)
        names = sorted(files + [d for d in dirs if os.path.islink(os.path.join(top, d))])
        if rel == os.path.join("drive_c", "GOG"):
            del dirs[:]
        for name in names:
            path = os.path.join(top, name)
            h.update(os.path.join(rel, name).encode())
            if os.path.islink(path):
                h.update(os.readlink(path).encode())
            elif rel == "." and name.endswith(".reg") or rel.startswith(os.path.join("drive_c", "users")):
                with open(path, "rb") as f:
                    h.update(f.read())
    return h.hexdigest()


def _step(raw, i, shots):
    """One `[[step]]`: exactly one action key (with `screenshot`'s `min_fill`)."""
    what = f"step {i + 1}"
    if not isinstance(raw, dict):
        raise ManifestError(f"{what}: must be a table")
    actions = [k for k in raw if k != "min_fill"]
    if len(actions) != 1:
        raise ManifestError(
            f"{what}: needs exactly one of click, click_here, move_to, move_by, scroll, key, "
            f"type, hold, press_until, wait, wait_until, wait_settled, screenshot, expect_change, relaunch, move_relaunch; got {sorted(actions)}"
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
    if action == "click_here":
        if value is not True:
            raise ManifestError(f"{what}: click_here must be true")
        return {"click_here": True}
    if action == "relaunch":
        if value is not True:
            raise ManifestError(f"{what}: relaunch must be true")
        return {"relaunch": True}
    if action == "move_relaunch":
        if value not in MOVE_TARGETS:
            raise ManifestError(f"{what}: move_relaunch must be one of {', '.join(MOVE_TARGETS)}")
        return {"move_relaunch": value}
    if action == "move_to":
        _only(value, ("x", "y"), f"{what}: move_to")
        if "x" not in value or "y" not in value:
            raise ManifestError(f"{what}: move_to needs x and y")
        return {
            "move_to": {
                "x": _percent(value["x"], f"{what}: move_to.x"),
                "y": _percent(value["y"], f"{what}: move_to.y"),
            }
        }
    if action == "move_by":
        _only(value, ("x", "y"), f"{what}: move_by")
        if "x" not in value or "y" not in value:
            raise ManifestError(f"{what}: move_by needs x and y")
        return {
            "move_by": {
                "x": _number(value["x"], f"{what}: move_by.x"),
                "y": _number(value["y"], f"{what}: move_by.y"),
            }
        }
    if action == "hold":
        _only(value, ("key", "ms"), f"{what}: hold")
        if value.get("key") not in KEYS:
            raise ManifestError(
                f"{what}: hold.key {value.get('key')!r} isn't a key of the us keymap the harness knows"
            )
        ms = value.get("ms")
        if isinstance(ms, bool) or not isinstance(ms, int) or not 1 <= ms <= HOLD_MAX_MS:
            raise ManifestError(f"{what}: hold.ms must be a whole number of ms, 1-{HOLD_MAX_MS}")
        return {"hold": {"key": value["key"], "ms": ms}}
    if action == "press_until":
        return {"press_until": _press_until(value, f"{what}: press_until")}
    if action == "wait_until":
        return {"wait_until": _wait_until(value, f"{what}: wait_until")}
    if action == "scroll":
        if isinstance(value, bool) or not isinstance(value, int) or value == 0:
            raise ManifestError(f"{what}: scroll must be a non-zero whole number of notches")
        return {"scroll": value}
    if action in ("key", "type"):
        if not isinstance(value, str) or not value:
            raise ManifestError(f"{what}: {action} must be a non-empty string")
        if action == "key" and value not in KEYS:
            raise ManifestError(
                f"{what}: key {value!r} isn't a key of the us keymap the harness knows "
                "(an XKB keysym name: Escape, Return, space, a, F1, Up, ...)"
            )
        if action == "type":
            bad = sorted({c for c in value if c not in TYPE_CHARS})
            if bad:
                raise ManifestError(f"{what}: type can't type {bad[0]!r} on a us keyboard")
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


def parse_manifest(text, live=False):
    """Validate a manifest's TOML and fill in its defaults. `live`: for a live
    session (#241), which drives the game itself: the steps may be left out,
    and any there are checked and dropped."""
    try:
        raw = tomllib.loads(text)
    except tomllib.TOMLDecodeError as e:
        raise ManifestError(f"not TOML: {e}") from e
    _only(
        raw,
        (
            "gogid",
            "title",
            "command",
            "build_command",
            "window",
            "gpu",
            "exit",
            "env",
            "override",
            "step",
        ),
        "manifest",
    )

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
    build_command = raw.get("build_command")
    if build_command is not None and (
        command is None
        or not isinstance(build_command, list)
        or not build_command
        or not all(isinstance(c, str) for c in build_command)
    ):
        raise ManifestError(
            "build_command is for self-tests: a non-empty list of strings beside command"
        )

    window = raw.get("window", {})
    _only(window, ("wait_secs", "resolution", "title", "min_fill", "display"), "window")
    display = window.get("display", "xwayland")
    if display not in DISPLAYS:
        raise ManifestError(f"window.display: want one of {DISPLAYS}, got {display!r}")
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

    # A game that never touches the GPU in Wayland mode: GDI drawn into shm
    # buffers (gogo's cnc-ddraw, `renderer=gdi`). With --gpu and no
    # --expect-gpu, gpu_used_by_game there wants it to hold no node.
    gpu = raw.get("gpu", {})
    if not isinstance(gpu, dict):
        raise ManifestError("gpu: must be a table")
    _only(gpu, ("wayland",), "gpu")
    gpu_wayland = gpu.get("wayland", "used")
    if gpu_wayland not in GPU_WAYLAND:
        raise ManifestError(f"gpu.wayland: want one of {GPU_WAYLAND}, got {gpu_wayland!r}")

    exit_ = raw.get("exit", {})
    _only(exit_, ("how", "wait_secs", "code"), "exit")
    how = exit_.get("how", "signal")
    if how not in EXIT_HOW:
        raise ManifestError(f"exit.how: want one of {EXIT_HOW}, got {how!r}")

    env = raw.get("env", {})
    if not isinstance(env, dict) or not all(isinstance(v, str) for v in env.values()):
        raise ManifestError("env: must be a table of strings")

    # Written into the game's override file through gogo (`linux-app env set`,
    # `args`, `wrapper` and `display`) in this run's scratch, right before the
    # launch (#234, #271, #292). In Wayland mode the harness sets `display`
    # itself unless the manifest does (linux-e2e.sh).
    override = raw.get("override", {})
    if not isinstance(override, dict):
        raise ManifestError("override: must be a table")
    _only(override, ("env", "extra_args", "wrapper", "display", "hdr", "monitor"), "override")
    if override and gogid is None:
        raise ManifestError("override is for a game (gogid), not a command")
    o_env = override.get("env", {})
    if not isinstance(o_env, dict) or not all(
        isinstance(v, str) and re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", k)
        for k, v in o_env.items()
    ):
        raise ManifestError("override.env: a table of NAME = \"string\"")
    o_args = override.get("extra_args")
    if o_args is not None and (
        not isinstance(o_args, list) or not o_args or not all(isinstance(a, str) for a in o_args)
    ):
        raise ManifestError("override.extra_args: a non-empty list of strings")
    o_wrapper = override.get("wrapper")
    if o_wrapper is not None and (
        not isinstance(o_wrapper, list)
        or not o_wrapper
        or not all(isinstance(a, str) for a in o_wrapper)
    ):
        raise ManifestError("override.wrapper: a non-empty list of strings")
    o_display = override.get("display")
    if o_display is not None and o_display not in GOGO_DISPLAYS:
        raise ManifestError(f"override.display: want one of {GOGO_DISPLAYS}, got {o_display!r}")
    o_hdr = override.get("hdr")
    if o_hdr is not None and o_hdr not in GOGO_HDR:
        raise ManifestError(f"override.hdr: want one of {GOGO_HDR}, got {o_hdr!r}")

    o_monitor = override.get("monitor")
    if o_monitor is not None and (
        not isinstance(o_monitor, str) or not re.fullmatch(r"[\x21-\x7e]{1,64}", o_monitor)
    ):
        raise ManifestError(f"override.monitor: an output name such as \"HEADLESS-1\", got {o_monitor!r}")

    steps_raw = raw.get("step", [])
    if not isinstance(steps_raw, list) or not (steps_raw or live):
        raise ManifestError("needs at least one [[step]]")
    shots = set()
    steps = [_step(s, i, shots) for i, s in enumerate(steps_raw)]
    if gogid is None and any("move_relaunch" in s for s in steps):
        raise ManifestError("move_relaunch is for a game (gogid), not a command")
    if live:
        steps = []

    return {
        "gogid": gogid,
        "title": raw.get("title", str(gogid) if gogid else "self-test"),
        "command": command,
        "build_command": build_command,
        "window": {
            "wait_secs": _number(window.get("wait_secs", 180), "window.wait_secs"),
            "width": int(m.group(1)),
            "height": int(m.group(2)),
            "title": title,
            "min_fill": _min_fill(window.get("min_fill", {}), "window.min_fill"),
            "display": display,
        },
        "gpu": {"wayland": gpu_wayland},
        "exit": {
            "how": how,
            "wait_secs": _number(exit_.get("wait_secs", 30), "exit.wait_secs"),
            "code": int(_number(exit_.get("code", 0), "exit.code")),
        },
        "env": env,
        "override": {
            "env": o_env,
            "extra_args": o_args,
            "wrapper": o_wrapper,
            "display": o_display,
            "hdr": o_hdr,
            "monitor": o_monitor,
        },
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


def parse_ppm(data):
    """An 8-bit binary PPM (grim -t ppm): the same (width, height, channels, rows)
    as read_png, with nothing to unfilter."""
    fields = data[:64].split(maxsplit=4)
    if len(fields) < 4 or fields[0] != b"P6" or fields[3] != b"255":
        raise ValueError("not an 8-bit P6 PPM")
    width, height = int(fields[1]), int(fields[2])
    # The header's four fields, each with the one space after it.
    start = sum(len(f) + 1 for f in fields[:4])
    stride = width * 3
    if len(data) - start != stride * height:
        raise ValueError("truncated PPM")
    rows = [data[start + y * stride : start + (y + 1) * stride] for y in range(height)]
    return width, height, 3, rows


def ppm_bytes(image):
    width, height, channels, rows = image
    if channels != 3:
        raise ValueError("PPM wants RGB")
    return b"P6\n%d %d\n255\n" % (width, height) + b"".join(rows)


def write_png(path, image):
    """An RGB PNG, unfiltered and lightly deflated: fast, and read_png reads it."""
    width, height, channels, rows = image
    if channels != 3:
        raise ValueError("write_png wants RGB")

    def chunk(kind, body):
        return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body))

    raw = zlib.compress(b"".join(b"\0" + row for row in rows), 1)
    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n")
        f.write(chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)))
        f.write(chunk(b"IDAT", raw))
        f.write(chunk(b"IEND", b""))


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


def marked_pct(image, rect, marked):
    """How much of press_until's region, in % of the window `rect`, is near its
    colour. Clipped to the image."""
    width, height, channels, rows = image
    x0 = max(0, rect["x"] + round(rect["width"] * marked["x"] / 100))
    y0 = max(0, rect["y"] + round(rect["height"] * marked["y"] / 100))
    x1 = min(width, rect["x"] + round(rect["width"] * (marked["x"] + marked["w"]) / 100))
    y1 = min(height, rect["y"] + round(rect["height"] * (marked["y"] + marked["h"]) / 100))
    if x1 <= x0 or y1 <= y0:
        return 0.0
    want = bytes.fromhex(marked["color"][1:])
    r, g, b = want
    tol = marked["tol"]
    hits = 0
    for row in rows[y0:y1]:
        for at in range(x0 * channels, x1 * channels, channels):
            if abs(row[at] - r) <= tol and abs(row[at + 1] - g) <= tol and abs(row[at + 2] - b) <= tol:
                hits += 1
    return round(100.0 * hits / ((x1 - x0) * (y1 - y0)), 2)


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


def tree_windows(node):
    """Every window-like node in a `swaymsg -t get_tree` tree, pid or not,
    visible or not: what sway knows of, for a window that never came up (#309)."""
    found = []
    for child in node.get("nodes", []) + node.get("floating_nodes", []):
        if child.get("type") in ("con", "floating_con") and not child.get("nodes"):
            rect = child.get("rect", {})
            found.append(
                {
                    "pid": child.get("pid"),
                    "name": child.get("name"),
                    "shell": child.get("shell"),
                    "visible": child.get("visible"),
                    "size": f"{rect.get('width', 0)}x{rect.get('height', 0)}",
                }
            )
        found += tree_windows(child)
    return found


def parse_xwininfo_children(text):
    """The root's children in `xwininfo -root -children`, (id, name) pairs,
    less the window manager's own: those of the client that made "wlroots wm"
    (an X client's ids share the bits above the server's 21-bit id mask)."""
    found = []
    for line in text.splitlines():
        m = re.match(r"\s+(0x[0-9a-f]+) (\(has no name\)|\"(.*?)\"):", line)
        if m:
            found.append((m.group(1), m.group(3)))
    wm = {int(wid, 16) >> 21 for wid, title in found if title == "wlroots wm"}
    return [(wid, title) for wid, title in found if int(wid, 16) >> 21 not in wm]


def parse_map_state(text):
    """`Map State:` in `xwininfo -id`: IsViewable, IsUnMapped or IsUnviewable."""
    m = re.search(r"Map State: (\w+)", text)
    return m.group(1) if m else None


def click_point(rect, x_pct, y_pct):
    """A click's output coordinates from its percentage of the window."""
    x = rect["x"] + rect["width"] * x_pct / 100.0
    y = rect["y"] + rect["height"] * y_pct / 100.0
    return (
        min(int(x), rect["x"] + rect["width"] - 1),
        min(int(y), rect["y"] + rect["height"] - 1),
    )


# --- whose display, whose process ----------------------------------------------


def xwayland_displays(cmdlines):
    """The `:N` each Xwayland command line serves (argv lists)."""
    found = []
    for argv in cmdlines:
        if argv and os.path.basename(argv[0]) == "Xwayland":
            found += [a for a in argv[1:] if re.fullmatch(r":\d+", a)]
    return found


def parse_ss_unix(text):
    """`ss -xapnH`'s Unix sockets by inode: {inode: {"state", "path", "peer",
    "users": [(comm, pid)]}}. "path" is None for an unnamed socket; a connected
    server-side socket carries its listener's path. Paths with spaces aren't read."""
    found = {}
    for line in text.splitlines():
        parts = line.split()
        if len(parts) < 8 or not parts[0].startswith("u_"):
            continue
        try:
            inode, peer = int(parts[5]), int(parts[7])
        except ValueError:
            continue
        users = re.findall(r'\("((?:[^"\\]|\\.)*)",pid=(\d+),fd=\d+\)', " ".join(parts[8:]))
        found[inode] = {
            "state": parts[1],
            "path": None if parts[4] == "*" else parts[4],
            "peer": peer,
            "users": [(comm, int(pid)) for comm, pid in users],
        }
    return found


def is_x_socket(path):
    """An X server's socket, on the filesystem or abstract (`@/tmp/.X11-unix/X0`)."""
    return bool(path) and "/.X11-unix/X" in path


def is_wayland_socket(path):
    return bool(path) and re.search(r"/wayland-[^/]*$", path) is not None and not path.endswith(".lock")


def display_listeners(sockets):
    """The listening display sockets: ({wayland path: [comm]}, {X path: [comm]})."""
    wl, x = {}, {}
    for s in sockets.values():
        if s["state"] != "LISTEN":
            continue
        kind = x if is_x_socket(s["path"]) else wl if is_wayland_socket(s["path"]) else None
        if kind is not None:
            kind.setdefault(s["path"], set()).update(c for c, _ in s["users"])
    return tuple({p: sorted(c) for p, c in sorted(d.items())} for d in (wl, x))


def display_peers(sockets, pids):
    """Which display servers the processes `pids` are connected to:
    {"wayland": {path: [server comm]}, "x11": {…}}. The path is the listener's,
    as its server bound it, whatever path the client dialled (pressure-vessel
    hands its container the socket at a path of its own)."""
    out = {"wayland": {}, "x11": {}}
    for s in sockets.values():
        if not any(pid in pids for _, pid in s["users"]):
            continue
        peer = sockets.get(s["peer"])
        path = peer["path"] if peer else None
        kind = "x11" if is_x_socket(path) else "wayland" if is_wayland_socket(path) else None
        if kind is not None:
            out[kind].setdefault(path, set()).update(c for c, _ in peer["users"])
    return {k: {p: sorted(c) for p, c in sorted(d.items())} for k, d in out.items()}


def x_fallback_line(text):
    """The first line of a log that says a process wanted an X server, or None."""
    for line in text.splitlines():
        if X_FALLBACK.search(line) and "xrandr" not in line:
            return line.strip()[:300]
    return None


def game_dir(exe):
    """Lowercase path of the exe's folder under drive_c (the run log's `# exe:`
    line), or None when it isn't in a folder there."""
    path = exe.replace("\\", "/").lower()
    _, found, rest = path.rpartition("/drive_c/")
    if not found or "/" not in rest:
        return None
    return rest.rpartition("/")[0]


def belongs_to_game(argv0, exe):
    """Is this process image the effective exe, or another one from its folder?

    Wine shows a process's Windows path as argv[0]. Wine's helpers (explorer.exe,
    xalia.exe, services.exe, steam.exe) live outside the game's folder.
    """
    if not argv0 or not exe:
        return False
    image = argv0.replace("\\", "/").lower()
    if image.rpartition("/")[2] == exe.replace("\\", "/").lower().rpartition("/")[2]:
        return True
    folder = game_dir(exe)
    if folder is None:
        return False
    return image.startswith(f"c:/{folder}/") or f"/drive_c/{folder}/" in image


def parse_gogo_display(log_text):
    """What gogo's launcher picked for the game (#292): under a wrapper the
    game step's own line (the wrapper's environment decides), else the run
    log's `# display:` header line; None when the game has no display mode."""
    header = None
    in_header = True
    for line in log_text.splitlines():
        if line == "# ---":
            in_header = False
        elif in_header and line.startswith("# display: "):
            header = line[len("# display: "):].strip()
        elif not in_header and line.startswith("gogo: display under the wrapper: "):
            return line[len("gogo: display under the wrapper: "):].strip()
    return header


def parse_exe(log_text):
    """The run log's `# exe:` header line."""
    m = re.search(r"^# exe: (.+)$", log_text, re.M)
    return m.group(1).strip() if m else None


def parse_dxvk(game_out):
    """What DXVK's log says (`DXVK_LOG_LEVEL=info`): the GPUs Vulkan gave it, in
    that order (before its own sort), and the ones it made a device on."""
    found = []
    created = []
    lines = game_out.splitlines()
    for i, line in enumerate(lines):
        m = re.match(r"info:\s+Found device: (.+) \(", line)
        if m and m.group(1) not in found:
            found.append(m.group(1))
        if re.match(r"info:\s+Creating device:\s*$", line) and i + 1 < len(lines):
            m = re.match(r"info:\s+(.+):\s*$", lines[i + 1])
            if m and m.group(1) not in created:
                created.append(m.group(1))
    return {"found": found, "created": created}


def parse_fdinfo(text):
    """A DRM fd's use, from /proc/<pid>/fdinfo: nanoseconds on its engines and
    memory in KiB. None when the driver keeps no such count (NVIDIA's)."""
    engines = re.findall(r"^drm-engine-[\w-]+:\s+(\d+) ns", text, re.M)
    if "drm-driver:" not in text:
        return None
    vram = re.search(r"^drm-(?:total|memory)-vram:\s+(\d+) KiB", text, re.M)
    return {"engine_ns": sum(map(int, engines)), "vram_kib": int(vram.group(1)) if vram else 0}


def vulkan_devices_ok(devices, expect, gpus):
    """Does vulkaninfo's list fit the sandbox? One GPU: every device is it. Several
    (no VK_DRIVER_FILES, as on a desktop): each is listed, and any other device is
    the host's software driver, which DXVK skips."""
    if not devices:
        return False
    if len(gpus) < 2:
        return all(re.search(expect, d, re.I) for d in devices)
    hardware = [d for d in devices if not re.search(r"llvmpipe|softpipe|lavapipe", d, re.I)]
    return all(any(re.search(g["expect"], d, re.I) for d in hardware) for g in gpus) and all(
        any(re.search(g["expect"], d, re.I) for g in gpus) for d in hardware
    )


def held_nodes(fds, gpus):
    """The render nodes of the GPUs these processes hold, whatever they do with them."""
    held = {n for nodes in fds.values() for n in nodes}
    return [g["node"] for g in gpus if g["held"] in held]


def renders_on(gpus, dxvk, drm_use):
    """The render nodes the game really uses. Holding one says nothing: a Vulkan
    driver keeps it open from enumeration on. So: the GPU DXVK made its device on.
    With no DXVK, a GPU whose kernel driver counts engine time for the game's fds;
    beside DXVK that's only crumbs (Anno 1701: 2 ms, on the compositor's GPU)."""
    if dxvk["created"]:
        return [
            g["node"]
            for g in gpus
            if any(re.search(g["expect"], name, re.I) for name in dxvk["created"])
        ]
    return [g["node"] for g in gpus if drm_use.get(g["held"], {}).get("engine_ns", 0) > 0]


def parse_support_installer(build_log):
    """How the support installer ended, from the build's log. None: it didn't run."""
    if "running GOG support installer" not in build_log:
        return None
    for pattern, outcome in SUPPORT_LINES:
        m = pattern.search(build_log)
        if m:
            return {"outcome": outcome, "exit": int(m.group(1)) if m.groups() else None}
    return {"outcome": "unknown", "exit": None}


def installer_problem(path, flags=None):
    """Why the build's --installer isn't what the harness asked for. None: it is.

    The mount must be read-only by the kernel's word, not by anyone's behaviour.
    """
    mount = os.path.dirname(path)
    try:
        if flags is None:
            flags = os.statvfs(mount).f_flag
    except OSError as e:
        return f"{mount}: {e.strerror}"
    if not flags & os.ST_RDONLY:
        return f"{mount} isn't read-only"
    if not os.path.isfile(path) or not os.access(path, os.R_OK):
        return f"can't read {path}"
    return None


def install_logs(games_dir):
    """The installer lane's logs, staged or moved in place: (log name, path)."""
    found = []
    if not os.path.isdir(games_dir):
        return found
    for game in sorted(os.listdir(games_dir)):
        logs = os.path.join(games_dir, game, "install")
        if not os.path.isdir(logs):
            continue
        for name in sorted(os.listdir(logs)):
            if name.endswith(".log"):
                found.append((name, os.path.join(logs, name)))
    return found


def parse_pactl_info(text):
    """`pactl info` as the two lines that say whose server and which sink."""
    server = re.search(r"^Server String: (.+)$", text, re.M)
    sink = re.search(r"^Default Sink: (.+)$", text, re.M)
    return {
        "server": server.group(1).strip() if server else None,
        "default_sink": sink.group(1).strip() if sink else None,
    }


def audio_streams(daemon_log):
    """Names of the streams the sound server took, from its `-v` log, in order."""
    return re.findall(r'Created input \d+ "([^"]*)" on ', daemon_log)


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
    found = _wl_globals(_wl_wait_done(conn, 3, "the Wayland connection"))
    iface = "zwlr_virtual_pointer_manager_v1"
    if iface not in found:
        raise RuntimeError(f"the compositor has no {iface}")
    name, _ = found[iface]
    # wl_registry.bind (new id 4), create_virtual_pointer(seat: null, new id 5),
    # sync (new id 6) so we return with the device in place.
    conn.sendall(
        _wl_bind(name, iface, 4)
        + _wl_message(4, 0, struct.pack("<II", 0, 5))
        + _wl_message(1, 0, struct.pack("<I", 6))
    )
    _wl_wait_done(conn, 6, "the virtual pointer")
    return conn


def _wl_fixed(value):
    """A wl_fixed_t's bits: signed 24.8."""
    return struct.pack("<i", round(value * 256))


def pointer_move_to(x, y, width, height, msec):
    """zwlr_virtual_pointer_v1 (object 5).motion_absolute, then frame: where the
    pointer goes as a fraction of the output.

    Sway turns it into a motion with a delta, so a client that reads relative
    motion sees a mouse move. `seat cursor set` is a warp: it has no delta, and
    Xwayland reports it as absolute raw motion.
    """
    body = struct.pack("<IIIII", msec & 0xFFFFFFFF, x, y, width, height)
    return _wl_message(5, 1, body) + _wl_message(5, 4)


def pointer_move_by(dx, dy, msec):
    """zwlr_virtual_pointer_v1 (object 5).motion, then frame: a delta in device units."""
    body = struct.pack("<I", msec & 0xFFFFFFFF) + _wl_fixed(dx) + _wl_fixed(dy)
    return _wl_message(5, 0, body) + _wl_message(5, 4)


def pointer_scroll(notches, msec):
    """One wheel event: axis_source(wheel), axis_discrete(vertical), frame, on
    zwlr_virtual_pointer_v1 (object 5). Positive is down, a notch is 15 units, as
    libinput reports a wheel's."""
    t = msec & 0xFFFFFFFF
    return (
        _wl_message(5, 5, struct.pack("<I", 0))
        + _wl_message(5, 7, struct.pack("<II", t, 0) + _wl_fixed(15 * notches)
                      + struct.pack("<i", notches))
        + _wl_message(5, 4)
    )  # fmt: skip


# --- a keyboard for the seat ---------------------------------------------------
#
# One zwp_virtual_keyboard_v1 for the whole run, with a standard pc105/us keymap
# and real evdev keycodes, so Xwayland and Wine see an ordinary keyboard.
# wtype, which this replaced (#236), made a keyboard per call whose keymap had
# just that call's keysyms from keycode 1 up: X keycode 9, Escape's scan code to
# Wine, for every key, so Cyberpunk 2077's title screen never saw a space.

KEYMAP_ARGS = ("--rules", "evdev", "--model", "pc105", "--layout", "us")
# Linux input-event-codes.h. The keysym names are XKB's, as the us keymap has
# them on level 1 (unshifted): `key` takes these.
KEYS = {
    "Escape": 1,
    **{str(n): 2 + (n - 1) % 10 for n in range(10)},
    "minus": 12,
    "equal": 13,
    "BackSpace": 14,
    "Tab": 15,
    **{c: 16 + i for i, c in enumerate("qwertyuiop")},
    "bracketleft": 26,
    "bracketright": 27,
    "Return": 28,
    "Control_L": 29,
    **{c: 30 + i for i, c in enumerate("asdfghjkl")},
    "semicolon": 39,
    "apostrophe": 40,
    "grave": 41,
    "Shift_L": 42,
    "backslash": 43,
    **{c: 44 + i for i, c in enumerate("zxcvbnm")},
    "comma": 51,
    "period": 52,
    "slash": 53,
    "Shift_R": 54,
    "Alt_L": 56,
    "space": 57,
    "Caps_Lock": 58,
    **{f"F{n}": 58 + n for n in range(1, 11)},
    "F11": 87,
    "F12": 88,
    "Control_R": 97,
    "Alt_R": 100,
    "Home": 102,
    "Up": 103,
    "Prior": 104,
    "Left": 105,
    "Right": 106,
    "End": 107,
    "Down": 108,
    "Next": 109,
    "Insert": 110,
    "Delete": 111,
    "Pause": 119,
    "Super_L": 125,
}
# Level 2 (with Shift) of the us keymap: keysym name, and the level-1 key it's on.
SHIFTED = {
    **{c.upper(): c for c in "abcdefghijklmnopqrstuvwxyz"},
    **dict(
        zip(
            "exclam at numbersign dollar percent asciicircum ampersand asterisk "
            "parenleft parenright underscore plus braceleft braceright colon "
            "quotedbl asciitilde bar less greater question".split(),
            "1 2 3 4 5 6 7 8 9 0 minus equal bracketleft bracketright semicolon "
            "apostrophe grave backslash comma period slash".split(),
        )
    ),
}
_CHAR_NAMES = {
    " ": "space", "-": "minus", "=": "equal", "[": "bracketleft", "]": "bracketright",
    ";": "semicolon", "'": "apostrophe", "`": "grave", "\\": "backslash", ",": "comma",
    ".": "period", "/": "slash", "!": "exclam", "@": "at", "#": "numbersign",
    "$": "dollar", "%": "percent", "^": "asciicircum", "&": "ampersand",
    "*": "asterisk", "(": "parenleft", ")": "parenright", "_": "underscore",
    "+": "plus", "{": "braceleft", "}": "braceright", ":": "colon", '"': "quotedbl",
    "~": "asciitilde", "|": "bar", "<": "less", ">": "greater", "?": "question",
    "\n": "Return", "\t": "Tab",
}  # fmt: skip
# What `type` can type: a character, its evdev keycode, and whether Shift is held.
TYPE_CHARS = {}
for _c in [chr(n) for n in range(0x21, 0x7F)] + list(_CHAR_NAMES):
    _name = _CHAR_NAMES.get(_c, _c)
    if _name in KEYS:
        TYPE_CHARS[_c] = (KEYS[_name], False)
    elif _name in SHIFTED:
        TYPE_CHARS[_c] = (KEYS[SHIFTED[_name]], True)
del _c, _name
SHIFT_KEY = KEYS["Shift_L"]
SHIFT_MASK = 1  # the us keymap's Shift modifier: the first real modifier


def keymap_levels(text):
    """{evdev keycode: [level-1 keysym, level-2 keysym, ...]} of a compiled XKB
    keymap (`xkbcli compile-keymap`), first group only."""
    codes = {
        name: int(code) - 8
        for name, code in re.findall(r"^\s*<(\w+)>\s*=\s*(\d+);", text, re.M)
    }
    levels = {}
    for name, body in re.findall(r"\bkey\s+<(\w+)>\s*\{([^}]*)\}", text):
        # `symbols[1]= [ F1, XF86Switch_VT_1 ]`: the group's index isn't a level.
        syms = re.search(r"\[([^\]]*)\]", re.sub(r"\w+\[\w+\]", "", body))
        if name in codes and syms:
            levels[codes[name]] = [s.strip() for s in syms.group(1).split(",")]
    return levels


def keymap_mismatches(text):
    """Where KEYS and SHIFTED disagree with the keymap: (keysym, why) pairs."""
    levels = keymap_levels(text)
    bad = []
    for sym, code in KEYS.items():
        got = levels.get(code, [])
        if not got or got[0] != sym:
            bad.append((sym, f"keycode {code} is {got[:1] or 'missing'}"))
    for sym, base in SHIFTED.items():
        got = levels.get(KEYS[base], [])
        if len(got) < 2 or got[1] != sym:
            bad.append((sym, f"Shift+{base} is {got[1:2] or 'missing'}"))
    return bad


def virtual_keyboard(socket_path, keymap):
    """A virtual keyboard on the seat with `keymap` (XKB text), for as long as
    the returned socket stays open. Keys go to it with keyboard_key()."""
    conn = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    conn.connect(socket_path)
    conn.sendall(
        _wl_message(1, 1, struct.pack("<I", 2)) + _wl_message(1, 0, struct.pack("<I", 3))
    )
    found = _wl_globals(_wl_wait_done(conn, 3, "the Wayland connection"))
    iface = "zwp_virtual_keyboard_manager_v1"
    for need in ("wl_seat", iface):
        if need not in found:
            raise RuntimeError(f"the compositor has no {need}")
    data = keymap.encode() + b"\0"
    fd = os.memfd_create("gogo-e2e-keymap")
    try:
        os.write(fd, data)
        # bind wl_seat (4) and the manager (5), create_virtual_keyboard(seat 4,
        # new id 6), keymap(XKB_V1, fd, size), then sync (7): the keymap is in
        # place before the first key.
        msg = (
            _wl_bind(found["wl_seat"][0], "wl_seat", 4)
            + _wl_bind(found[iface][0], iface, 5)
            + _wl_message(5, 0, struct.pack("<II", 4, 6))
            + _wl_message(6, 0, struct.pack("<II", 1, len(data)))
            + _wl_message(1, 0, struct.pack("<I", 7))
        )
        socket.send_fds(conn, [msg], [fd])
    finally:
        os.close(fd)
    _wl_wait_done(conn, 7, "the virtual keyboard")
    return conn


def _wl_bind(name, iface, new_id):
    """wl_registry (object 2).bind of a global at version 1."""
    body = struct.pack("<I", name) + _wl_string(iface) + struct.pack("<II", 1, new_id)
    return _wl_message(2, 0, body)


def _wl_wait_done(conn, callback, what):
    """Read until wl_callback `callback` is done; a wl_display.error is a failure."""
    data = b""
    pos = 0
    while True:
        chunk = conn.recv(65536)
        if not chunk:
            raise RuntimeError(f"the compositor closed {what}")
        data += chunk
        while pos + 8 <= len(data):
            obj, word = struct.unpack_from("<II", data, pos)
            size, opcode = word >> 16, word & 0xFFFF
            if size < 8 or pos + size > len(data):
                break
            pos += size
            if obj == 1 and opcode == 0:
                raise RuntimeError(f"the compositor refused {what} (wl_display.error)")
            if obj == callback and opcode == 0:
                return data


def keyboard_key(code, pressed, msec):
    """zwp_virtual_keyboard_v1 (object 6).key: an evdev keycode down or up."""
    return _wl_message(6, 1, struct.pack("<III", msec & 0xFFFFFFFF, code, int(pressed)))


def keyboard_modifiers(depressed):
    """zwp_virtual_keyboard_v1 (object 6).modifiers: wlroots doesn't work them
    out from a virtual keyboard's keys, so Shift is said in so many words."""
    return _wl_message(6, 2, struct.pack("<IIII", depressed, 0, 0, 0))


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
        self.building = "build_command" in config
        self.prefix = "build-" if self.building else ""
        # The build (an installer) always gets Xwayland; the game, the manifest's.
        self.display = "xwayland" if self.building else self.manifest["window"].get("display", "xwayland")
        # A live session's (#241): its control socket, once it listens.
        self.live = config.get("live")
        self.srv = None
        self.busy = None
        self.sway = None
        self.pulse = None
        self.audio_probe = None
        self.pointer = None
        self.keyboard = None
        self.game = None
        self.game_exit = None
        self.view = None
        # The launches after the first, in this sandbox (#252), and whether
        # this launch has shown what GPU it holds, and an earlier one has.
        self.relaunches = 0
        self.press_untils = 0
        # Who was up before the first launch: the harness's own.
        self.before_game = None
        self.gpu_seen = False
        self.gpu_checked = False
        self.shots = {}
        self.checks = []
        self.steps = []
        # How long the sandbox's start took, phase by phase (#257).
        self.phases = {}
        self.phase_at = time.time()
        self.result = {
            "display": {"mode": self.display},
            "phases": self.phases,
            "isolation": {},
            "gpu": {},
            "audio": {},
            "steps": self.steps,
            "checks": self.checks,
            "game_exit": None,
            "doctor": None,
            "stopped": False,
        }

    def save(self):
        path = os.path.join(self.results, f"{self.prefix}result.json")
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
            if self.srv is not None:
                # A live session answers while it waits: status, or busy.
                self.live_busy(min(left, 0.25))
            else:
                time.sleep(min(left, 0.25))

    def phase(self, name):
        """The time since the last phase ended is this one's."""
        now = time.time()
        self.phases[name] = round(now - self.phase_at, 2)
        self.phase_at = now

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
        env_file = os.path.join(self.run_dir, f"{self.prefix}sway.env")
        conf = os.path.join(self.run_dir, f"{self.prefix}sway.conf")
        # Wayland mode: no X server at all, so a fallback to X fails loudly
        # instead of passing on Xwayland (#264).
        xwayland = "force" if self.display == "xwayland" else "disable"
        with open(conf, "w") as f:
            f.write(
                f"xwayland {xwayland}\n"
                f"output HEADLESS-1 resolution {w}x{h} scale 1\n"
                "default_border none\n"
                "focus_follows_mouse no\n"
                f'exec env -0 > "{env_file}.tmp" && mv "{env_file}.tmp" "{env_file}"\n'
            )
        log = open(os.path.join(self.results, f"{self.prefix}sway.log"), "wb")
        # The build's session bus is for gogo's keyring, not for the compositor.
        sway_env = {k: v for k, v in self.env.items() if k != "DBUS_SESSION_BUS_ADDRESS"}
        self.sway = subprocess.Popen(
            ["sway", "--unsupported-gpu", "--debug", "--config", conf],
            env=sway_env,
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
        self.phase("sway")
        with open(env_file, "rb") as f:
            for item in f.read().split(b"\0"):
                key, _, value = item.decode(errors="replace").partition("=")
                if key in ("WAYLAND_DISPLAY", "SWAYSOCK", "DISPLAY"):
                    self.env[key] = value
        wl = os.path.join(self.env["XDG_RUNTIME_DIR"], self.env.get("WAYLAND_DISPLAY", ""))
        if self.display == "wayland":
            # Nobody serves one: a client that tries X finds no DISPLAY at all.
            self.env.pop("DISPLAY", None)
            self.result["display"].update(socket=wl, xwayland=False)
        # Xwayland comes up after sway's config: wait for the socket and for
        # an answer. The Wayland socket is there before the config runs.
        while time.monotonic() < deadline:
            if self.display == "wayland":
                ready = self.env.get("WAYLAND_DISPLAY") and os.path.exists(wl)
            else:
                ready = self.env.get("DISPLAY") and self.tool(
                    "xwininfo", "-root", "-display", self.env["DISPLAY"]
                ).returncode == 0
            if ready:
                self.phase(self.display)
                self.pointer = virtual_pointer(wl)
                self.keyboard = virtual_keyboard(wl, self.keymap())
                self.phase("input_devices")
                return
            self.sleep(0.2)
        if self.display == "wayland":
            raise RuntimeError(f"no Wayland socket at {wl!r} (see sway.log)")
        raise RuntimeError(f"no X server on {self.env.get('DISPLAY')!r} (see sway.log)")

    def keymap(self):
        """The virtual keyboard's pc105/us keymap, from the flake's xkbcli and
        xkeyboard-config; KEYS has to agree with it."""
        r = self.tool("xkbcli", "compile-keymap", *KEYMAP_ARGS)
        if r.returncode != 0 or "xkb_keymap" not in r.stdout:
            raise RuntimeError(f"xkbcli compile-keymap failed: {r.stderr.strip()}")
        bad = keymap_mismatches(r.stdout)
        if bad:
            raise RuntimeError(f"the us keymap disagrees with KEYS: {bad[:3]}")
        return r.stdout

    # audio ---------------------------------------------------------------

    def start_audio(self):
        """A sound server with one null sink, on a socket in this sandbox's own
        runtime dir. Its `-v` log says which streams the game opened."""
        pulse_dir = os.path.join(self.env["XDG_RUNTIME_DIR"], "pulse")
        os.makedirs(pulse_dir, exist_ok=True)
        sock = os.path.join(pulse_dir, "native")
        log = open(os.path.join(self.results, "audio.log"), "wb")
        self.pulse = subprocess.Popen(
            [
                "pulseaudio",
                "-n",
                "-v",
                "--daemonize=no",
                "--use-pid-file=no",
                "--exit-idle-time=-1",
                "--realtime=no",
                "--high-priority=no",
                "--log-target=stderr",
                f"--load=module-native-protocol-unix socket={sock} auth-anonymous=1",
                f"--load=module-null-sink sink_name={NULL_SINK} "
                f"sink_properties=device.description={NULL_SINK}",
            ],
            env=self.env,
            stdin=subprocess.DEVNULL,
            stdout=log,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        # Explicit: libpulse would otherwise also read the host's /etc/pulse.
        self.env["PULSE_SERVER"] = f"unix:{sock}"
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            if self.pulse.poll() is not None:
                break
            if self.tool("pactl", "info").returncode == 0:
                return
            self.sleep(0.1)
        raise RuntimeError("the sound server didn't start (see audio.log)")

    def probe_audio(self):
        """A test stream, in the background: it takes 2 s whatever its length
        (the null sink wakes every 2 s), with nothing to wait on until the next
        thing that needs the sound server, the game (#257). check_audio reads it."""
        probe = os.path.join(self.run_dir, "audio-probe.raw")
        with open(probe, "wb") as f:
            f.write(bytes(48000))
        self.audio_probe = subprocess.Popen(
            ["pacat", "--raw", "--rate=48000", "--format=s16le", "--channels=2", probe],
            env=self.env,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )

    def check_audio(self):
        """The default device is the sandbox's null sink, and it takes a stream."""
        audio = self.result["audio"]
        audio.update(parse_pactl_info(self.tool("pactl", "info").stdout))
        sinks = self.tool("pactl", "list", "sinks", "short").stdout
        audio["sinks"] = [line.split("\t")[1] for line in sinks.splitlines() if "\t" in line]
        try:
            played = self.audio_probe.wait(20)
        except subprocess.TimeoutExpired:
            self.audio_probe.kill()
            raise
        audio["probe_exit"] = played
        ours = os.path.join(self.env["XDG_RUNTIME_DIR"], "pulse", "native")
        self.check(
            "audio",
            audio["server"] in (ours, f"unix:{ours}")
            and audio["default_sink"] == NULL_SINK
            and audio["sinks"] == [NULL_SINK]
            and played == 0,
            f"{audio['default_sink']} on {audio['server']}, sinks {audio['sinks']}, "
            f"test stream exit {played}",
        )

    def record_audio_streams(self):
        if self.pulse is None:
            return
        with open(os.path.join(self.results, "audio.log"), errors="replace") as f:
            streams = audio_streams(f.read())
        # The first one is check_audio's own test stream.
        self.result["audio"]["game_streams"] = streams[1:]

    # isolation -----------------------------------------------------------

    def assert_isolation(self):
        iso = self.result["isolation"]
        c = self.cfg

        netns = os.readlink("/proc/self/ns/net")
        with open("/proc/net/dev") as f:
            links = [line.split(":")[0].strip() for line in f if ":" in line]
        iso["netns"] = {"ns": netns, "links": links}
        if self.building:
            # The build downloads: its netns has `lo` and pasta's tap, which
            # takes the name of the host's uplink, NATed out to the internet.
            ok = len(links) == 2 and links[0] == "lo"
            tap = links[1] if ok else ""
        else:
            ok, tap = links == ["lo"], ""
        iso["netns"]["tap"] = tap
        self.check(
            "isolation_netns",
            netns != c["host_netns"] and ok,
            f"{netns}, links {links}" + (f" (lo + pasta's {tap})" if tap else ""),
        )

        pidns = os.readlink("/proc/self/ns/pid")
        self.check("isolation_pidns", pidns != c["host_pidns"], pidns)

        tmp_fs = _mount_fstype("/tmp")
        tmp_entries = sorted(os.listdir("/tmp"))
        foreign = tmp_foreign(tmp_entries, c.get("bind_roots", []))
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

        if c.get("installer"):
            problem = installer_problem(c["installer"])
            self.check("isolation_installer", problem is None, problem or f"{c['installer']}, read-only")

        if self.display == "wayland":
            self.wayland_isolation()
        else:
            self.xwayland_isolation()

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

    def xwayland_isolation(self):
        """The display is our Xwayland, on our output's size."""
        iso = self.result["isolation"]
        w, h = self.manifest["window"]["width"], self.manifest["window"]["height"]
        display = self.env.get("DISPLAY", "")
        info = self.tool("xwininfo", "-root", "-display", display).stdout
        mw = re.search(r"Width:\s*(\d+)", info)
        mh = re.search(r"Height:\s*(\d+)", info)
        got = f"{mw.group(1)}x{mh.group(1)}" if mw and mh else "?"
        socket = f"/tmp/.X11-unix/X{display.lstrip(':').split('.')[0]}"
        # Only this sandbox's PID namespace is in /proc, so a match is our own
        # Xwayland, and it holds the abstract socket of that number.
        served = xwayland_displays(_cmdlines())
        iso["display"] = {"DISPLAY": display, "root": got, "socket": socket, "served": served}
        self.check(
            "isolation_display",
            got == f"{w}x{h}"
            and os.path.exists(socket)
            and served == [display]
            and display == ":0",
            f"{display} root {got}, socket {socket}, our Xwayland serves {served}",
        )

    def unix_sockets(self):
        """Every Unix socket of this sandbox's network namespace (`ss`), by inode."""
        r = self.tool("ss", "-xapnH")
        if r.returncode != 0:
            raise RuntimeError(f"ss: {r.stderr.strip() or r.returncode}")
        return parse_ss_unix(r.stdout)

    def wayland_isolation(self):
        """The only display is our sway's Wayland socket, on our output's size,
        and there's no X server: no Xwayland, no X socket, no DISPLAY."""
        iso = self.result["isolation"]
        w, h = self.manifest["window"]["width"], self.manifest["window"]["height"]
        socket_path = self.result["display"]["socket"]
        wl, x = display_listeners(self.unix_sockets())
        outputs = json.loads(self.swaymsg("-t", "get_outputs", "--raw"))
        sizes = [
            f"{o.get('current_mode', {}).get('width')}x{o.get('current_mode', {}).get('height')}"
            for o in outputs
        ]
        iso["display"] = {"WAYLAND_DISPLAY": self.env.get("WAYLAND_DISPLAY"), "socket": socket_path,
                          "listeners": wl, "outputs": sizes}  # fmt: skip
        self.check(
            "isolation_display",
            wl == {socket_path: ["sway"]} and sizes == [f"{w}x{h}"],
            f"Wayland listeners {wl}, outputs {sizes}",
        )
        xwayland = xwayland_displays(_cmdlines())
        try:
            x_dir = sorted(os.listdir("/tmp/.X11-unix"))
        except OSError:
            x_dir = []
        leaked = "DISPLAY" in self.env or any(
            "DISPLAY" in env for env in (self.cfg["game_env"], self.manifest["env"])
        )
        iso["no_x"] = {"xwayland": xwayland, "x_listeners": x, "x11_unix": x_dir, "DISPLAY": leaked}
        self.check(
            "isolation_no_x",
            not (xwayland or x or x_dir or leaked),
            "no X server, X socket or DISPLAY"
            if not (xwayland or x or x_dir or leaked)
            else f"Xwayland {xwayland}, X listeners {x}, /tmp/.X11-unix {x_dir}, DISPLAY set: {leaked}",
        )

    def check_wayland_client(self, name):
        """Wayland mode, once a window is up: it's an xdg-shell window, and what
        the launch started talks to our sway's socket and to no X server."""
        socket_path = self.result["display"]["socket"]
        shell = (self.view or {}).get("shell")
        pids = {pid for pid, _, _ in _proc_table()} - (self.before_game or set())
        peers = display_peers(self.unix_sockets(), pids)
        self.result["display"].setdefault("clients", {})[name] = {"shell": shell, **peers}
        self.save()
        return self.check(
            name,
            shell == "xdg_shell" and peers["wayland"] == {socket_path: ["sway"]} and not peers["x11"],
            f"window's shell {shell}; Wayland: {peers['wayland'] or 'none'}; X11: {peers['x11'] or 'none'}",
        )

    def x_fallback(self):
        if self.display == "wayland":
            if self.manifest["gogid"] is not None:
                self.check_gogo_display()
            self.check_x_fallback()

    def check_gogo_display(self):
        """Wayland mode: gogo's launcher put the game on Wine's Wayland driver
        itself (`# display: wayland …` in its run log), not the harness."""
        try:
            with open(self.cfg.get("game_log") or "", errors="replace") as f:
                got = parse_gogo_display(f.read())
        except OSError:
            got = None
        self.result["display"]["gogo"] = got
        return self.check(
            "gogo_display",
            bool(got) and got.startswith("wayland"),
            f"the launcher's run log: display {got}" if got else "no `# display:` line: gogo set no display mode",
        )

    def check_x_fallback(self):
        """Wayland mode: did any process of the game's say it wanted X? Names the
        line (game.out, then the launcher's run log)."""
        logs = sorted(
            os.path.join(self.results, f)
            for f in os.listdir(self.results)
            if f == "game.out" or re.fullmatch(r"launch\d+-game\.out", f)
        )
        if self.cfg.get("game_log"):
            logs.append(self.cfg["game_log"])
        for path in logs:
            try:
                with open(path, errors="replace") as f:
                    line = x_fallback_line(f.read())
            except OSError:
                continue
            if line:
                return self.check("no_x_fallback", False, f"{os.path.basename(path)}: {line}")
        return self.check("no_x_fallback", True, f"nothing asked for X in {len(logs)} log(s)")

    # GPU -----------------------------------------------------------------

    def probe_gpu(self):
        """What GL and Vulkan say they are, in the environment the game gets."""
        gpu = self.result["gpu"]
        gpus = self.cfg["gpu"]["gpus"]
        gpu["label"] = self.cfg["gpu"]["label"]
        gpu["compositor_node"] = next((g["node"] for g in gpus if g["compositor"]), None)
        env = dict(self.env)
        env.update(self.cfg["game_env"])
        gpu["gl_renderer"] = None
        if self.display == "wayland":
            # No X server to ask GLX. EGL's surfaceless platform says which
            # driver --gpu gives; EGL on our Wayland socket is recorded beside
            # it (NVIDIA's egl-wayland fails there with no primary DRM node).
            if shutil.which("eglinfo", path=env["PATH"]):
                for platform in ("surfaceless", "wayland"):
                    r = self.tool("eglinfo", "-B", "-p", platform, env=env)
                    m = re.search(r"OpenGL (?:core|compatibility) profile renderer:\s*(.+)", r.stdout)
                    err = re.search(r"eglinfo: (.+)", r.stdout + r.stderr)
                    found = m.group(1).strip() if m else None
                    if platform == "surfaceless":
                        gpu["gl_renderer"] = found
                    else:
                        gpu["egl_wayland"] = found or (err.group(1).strip() if err else f"exit {r.returncode}")
        elif shutil.which("glxinfo", path=env["PATH"]):
            r = self.tool("glxinfo", "-B", env=env)
            m = re.search(r"OpenGL renderer string:\s*(.+)", r.stdout)
            gpu["gl_renderer"] = m.group(1).strip() if m else None
        self.phase("gpu_gl")
        gpu["vulkan_devices"] = []
        if shutil.which("vulkaninfo", path=env["PATH"]):
            r = self.tool("vulkaninfo", "--summary", env=env)
            gpu["vulkan_devices"] = re.findall(r"deviceName\s*=\s*(.+)", r.stdout)
        self.phase("gpu_vulkan")
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
            vulkan_devices_ok(gpu["vulkan_devices"], expect, gpus),
            gpu["vulkan_devices"],
        )

    def effective_exe(self):
        """The exe the launcher ran, from the header of its log."""
        path = self.cfg.get("game_log")
        try:
            with open(path, errors="replace") as f:
                return parse_exe(f.read(16384))
        except (OSError, TypeError):
            return None

    def game_gpu_fds(self):
        """Which processes hold a GPU node open: (everybody, the game's own, and
        the use the kernel counts for the game's fds)."""
        held = {}
        game = {}
        use = {}
        exe = self.effective_exe()
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
                        if belongs_to_game(_argv(pid)[0], exe):
                            game.setdefault(comm, set()).add(target)
                            with open(f"/proc/{pid}/fdinfo/{fd}") as f:
                                counted = parse_fdinfo(f.read())
                            if counted:
                                total = use.setdefault(target, {"engine_ns": 0, "vram_kib": 0})
                                total["engine_ns"] += counted["engine_ns"]
                                total["vram_kib"] += counted["vram_kib"]
            except OSError:
                continue
        return *({k: sorted(v) for k, v in sorted(d.items())} for d in (held, game)), use

    def record_gpu_fds(self):
        """While something renders. Add to what's known: a game can open the
        node after the first frame anyone looked at."""
        gpu = self.result["gpu"]
        self.gpu_seen = True
        held, game, use = self.game_gpu_fds()
        for key, found in (("gpu_fds", held), ("game_gpu_fds", game)):
            known = gpu.setdefault(key, {})
            for comm, nodes in found.items():
                known[comm] = sorted(set(known.get(comm, [])) | set(nodes))
        known = gpu.setdefault("game_drm_use", {})
        for node, counted in use.items():
            before = known.setdefault(node, {"engine_ns": 0, "vram_kib": 0})
            for key, value in counted.items():
                before[key] = max(before[key], value)

    # game ----------------------------------------------------------------

    def launch(self):
        if self.before_game is None:
            self.before_game = {pid for pid, _, _ in _proc_table()}
        env = dict(self.env)
        env.update(self.cfg["game_env"])
        # Wayland mode doesn't set PROTON_ENABLE_WAYLAND here: gogo's own
        # display mode does (#292), written before the launch like the rest
        # of [override], and gogo_display checks that it did.
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

    def wait_window(self, name="window"):
        window = self.manifest["window"]
        start = time.monotonic()
        while time.monotonic() - start < window["wait_secs"]:
            tree = json.loads(self.swaymsg("-t", "get_tree", "--raw"))
            view = pick_view(tree, window["title"])
            if view:
                self.view = view
                self.check(
                    name,
                    True,
                    f"{view.get('name')!r} {view['rect']['width']}x"
                    f"{view['rect']['height']} after {time.monotonic() - start:.0f}s",
                )
                if self.display == "wayland":
                    self.check_wayland_client(name.replace("window", "wayland_client", 1))
                return True
            if self.game.poll() is not None:
                self.game_exit = self.game.returncode
                self.result["game_exit"] = self.game_exit
                return self.check(
                    name, False, f"game exited {self.game_exit} with no window"
                )
            self.sleep(0.5)
        try:
            diag = self.window_diagnosis(name)
        except Exception as e:  # the check still reports: only the diagnosis is lost
            diag = f"no window diagnosis: {type(e).__name__}: {e}"
        return self.check(name, False, f"none in {window['wait_secs']:.0f}s; {diag}")

    def window_diagnosis(self, name):
        """What each side saw of a window that never came up (#309): sway's
        views, the X server's mapped top-level windows, and the processes.
        In result.json under window_diagnosis; returns a one-line summary."""
        diag = {}
        try:
            diag["sway_windows"] = tree_windows(json.loads(self.swaymsg("-t", "get_tree", "--raw")))
        except Exception as e:  # a dead sway is itself the answer
            diag["sway_windows"] = f"{type(e).__name__}: {e}"
        diag["sway_alive"] = self.sway is not None and self.sway.poll() is None
        diag["xwayland"] = xwayland_displays(_cmdlines())
        x = None
        if self.display == "xwayland" and self.env.get("DISPLAY"):
            x = []
            try:
                r = self.tool("xwininfo", "-root", "-children", "-display", self.env["DISPLAY"], timeout=5)
                for wid, title in parse_xwininfo_children(r.stdout):
                    s = self.tool("xwininfo", "-id", wid, "-display", self.env["DISPLAY"], timeout=5)
                    x.append({"id": wid, "name": title, "map_state": parse_map_state(s.stdout)})
                if r.returncode != 0:
                    x = f"xwininfo exited {r.returncode}: {r.stderr.strip()}"
            except subprocess.TimeoutExpired:
                x = f"the X server didn't answer in 5 s, after {len(x)} windows"
            except OSError as e:
                x = f"xwininfo: {e}"
        diag["x_windows"] = x
        before = self.before_game or set()
        diag["processes"] = [
            {"pid": pid, "comm": comm, "state": state}
            for pid, comm, state in _proc_table()
            if pid not in before and pid != os.getpid()
        ]
        self.result.setdefault("window_diagnosis", {})[name] = diag
        self.save()
        sway_n = len(diag["sway_windows"]) if isinstance(diag["sway_windows"], list) else diag["sway_windows"]
        if isinstance(x, list):
            mapped = [w for w in x if w["map_state"] == "IsViewable"]
            x_text = f"{len(mapped)} of {len(x)} X top-level windows mapped"
        else:
            x_text = f"X: {x}"
        return (
            f"sway {'up' if diag['sway_alive'] else 'gone'} with {sway_n} windows, "
            f"Xwayland on {diag['xwayland'] or 'nothing'}, {x_text} (result.json window_diagnosis)"
        )

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
            self.press_key(KEYS[step["key"]], False)
        elif action == "type":
            for c in step["type"]:
                self.press_key(*TYPE_CHARS[c])
                time.sleep(KEY_GAP_MS / 1000)
        elif action == "click":
            self.do_click(step["click"], record)
        elif action == "click_here":
            self.press_button()
        elif action == "scroll":
            n = step["scroll"]
            for _ in range(abs(n)):
                self.pointer.sendall(pointer_scroll(1 if n > 0 else -1, _msec()))
                time.sleep(SCROLL_GAP_MS / 1000)
        elif action == "hold":
            self.press_key(KEYS[step["hold"]["key"]], False, step["hold"]["ms"])
        elif action in ("press_until", "wait_until"):
            self.do_press_until(step[action], record)
        elif action == "move_to":
            self.do_move_to(step["move_to"], record)
        elif action == "move_by":
            at = step["move_by"]
            self.pointer.sendall(pointer_move_by(at["x"], at["y"], _msec()))
        elif action == "screenshot":
            self.do_screenshot(step, record)
        elif action == "expect_change":
            self.do_expect_change(step["expect_change"], record)
        elif action == "relaunch":
            self.do_relaunch(record)
        elif action == "move_relaunch":
            self.do_relaunch(record, move=step["move_relaunch"])
        self.save()

    def press_until_marked(self, marked):
        """Is the region marked, in MARKED_HITS frames running? (yes, its %, a
        frame), with the frame that said no."""
        pct, image = 0.0, None
        for hit in range(MARKED_HITS):
            if hit:
                time.sleep(MARKED_HIT_GAP)
            image, _ = self.capture()
            view = self.refresh_view()
            if image is None or not view:
                return False, pct, image
            pct = marked_pct(image, view["rect"], marked)
            if pct < marked["min"]:
                return False, pct, image
        return True, pct, image

    def do_press_until(self, want, record):
        """Press until the screen says the key took, up to `max` presses, and fail
        if it doesn't: the steps after it stand on it, so a run ends there, and so
        does a replay. `change`: the frame after a press differs by that % from the
        one right before it, against which a game's own motion counts too, so it's
        for screens that hold still. `marked`: the region has the colour; looked at
        before the first press too, since a key that moves a focus would move it off."""
        self.press_untils += 1
        key = want.get("key")
        name = f"press_until_{self.press_untils}_{key}" if key else f"wait_until_{self.press_untils}"
        started = time.monotonic()
        presses, seen, frame = 0, None, None
        why = None
        while True:
            if "marked" in want:
                ok, seen, shot = self.press_until_marked(want["marked"])
                frame = shot if shot is not None else frame
                if ok:
                    break
            if self.game is not None and self.game.poll() is not None:
                why = f"the game exited {self.game.returncode}"
                break
            if presses >= want["max"]:
                why = "the screen never said so"
                break
            before = None
            if "change" in want:
                before, _ = self.capture()
            if key:
                self.press_key(KEYS[key], False)
            presses += 1
            self.sleep(want["every"])
            if "change" in want:
                frame, _ = self.capture()
                if before is not None and frame is not None:
                    seen = changed_pct(before, frame)
                    if seen >= want["change"]:
                        break
        record.update(presses=presses, secs=round(time.monotonic() - started, 1), seen_pct=seen)
        unit = "% changed" if "change" in want else "% marked"
        tries = f"{presses} press(es) of {key}" if key else f"{presses} look(s)"
        goal = f"want {want['change'] if 'change' in want else want['marked']['min']}"
        if why is None:
            detail = f"{tries} in {record['secs']} s: {seen}{unit}, {goal}"
            if self.live is None:
                self.check(name, True, detail)
            return
        record.update(ok=False, why=f"{why} after {tries} in {record['secs']} s (last {seen}{unit}, {goal})")
        if frame is not None:
            path = os.path.join(self.cfg["shots_dir"], f"press-until-{self.press_untils}-failed.png")
            write_png(path, frame)
            record["path"] = path
            record["why"] += f"; last frame {path}"
        if self.live is None:
            self.check(name, False, record["why"])

    def do_click(self, at, record):
        if self.do_move_to(at, record, "click"):
            time.sleep(0.2)
            self.press_button()

    def do_move_to(self, at, record, what="move_to"):
        """The pointer to a percent of the window, as a motion with a delta."""
        view = self.refresh_view()
        if not view:
            record["ok"] = False
            if self.live is None:
                self.check(f"input_{what}", False, f"no window to {what.replace('_', ' ')}")
            return False
        x, y = click_point(view["rect"], at["x"], at["y"])
        record["at"] = [x, y]
        window = self.manifest["window"]
        self.pointer.sendall(pointer_move_to(x, y, window["width"], window["height"], _msec()))
        return True

    def press_key(self, code, shift, hold_ms=None):
        """Down, held KEY_HOLD_MS (or `hold_ms`), up; Shift around it when `shift`."""
        if shift:
            self.keyboard.sendall(
                keyboard_key(SHIFT_KEY, True, _msec()) + keyboard_modifiers(SHIFT_MASK)
            )
        self.keyboard.sendall(keyboard_key(code, True, _msec()))
        time.sleep((hold_ms or KEY_HOLD_MS) / 1000)
        self.keyboard.sendall(keyboard_key(code, False, _msec()))
        if shift:
            self.keyboard.sendall(
                keyboard_key(SHIFT_KEY, False, _msec()) + keyboard_modifiers(0)
            )

    def press_button(self):
        self.swaymsg("seat", "seat0", "cursor", "press", "button1")
        time.sleep(0.1)
        self.swaymsg("seat", "seat0", "cursor", "release", "button1")

    def settle_wait(self, secs):
        """Poll until the picture fills the window and holds still: past any
        intro video, however long the renderer takes to get through it. Gives
        {settled, after_secs, last}; the last frame is left in settling.png.
        The manifest's wait_settled and a live session's settle both use it."""
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
                    return {"settled": True, "after_secs": round(time.monotonic() - start, 1), "last": last}
                prior = image
            self.sleep(SETTLE_POLL_SECS)
        return {"settled": False, "after_secs": round(time.monotonic() - start, 1), "last": last}

    def do_wait_settled(self, secs, record):
        got = self.settle_wait(secs)
        if got["settled"]:
            record["after_secs"] = got["after_secs"]
            self.check("settled", True, f"after {record['after_secs']}s")
            return
        record["ok"] = False
        self.check("settled", False, f"not in {secs:.0f}s; last frame {got['last']}")

    def grab(self, path):
        """A screenshot of the output to `path`: (image, stats over the window),
        or (None, grim's error)."""
        r = self.tool("grim", path)
        if r.returncode != 0:
            return None, r.stderr.strip() or f"grim exited {r.returncode}"
        self.refresh_view()
        image = read_png(path)
        rect = self.view["rect"] if self.view else None
        region = (rect["x"], rect["y"], rect["width"], rect["height"]) if rect else None
        stats = analyze(image, region)
        self.record_gpu_fds()
        return image, stats

    def capture(self):
        """A frame as it comes from grim, not through a PNG: (image, None) or
        (None, why). What a fast replay polls with: under 1 s, against 3-4 s."""
        try:
            r = subprocess.run(
                ["grim", "-t", "ppm", "-"], env=self.env, stdin=subprocess.DEVNULL,
                capture_output=True, timeout=20, check=False,
            )
            if r.returncode != 0:
                return None, r.stderr.decode(errors="replace").strip() or f"grim exited {r.returncode}"
            return parse_ppm(r.stdout), None
        except (subprocess.TimeoutExpired, ValueError) as e:
            return None, str(e)

    def grab_fast(self, path):
        """As grab, through capture: a fast replay's screenshot."""
        image, why = self.capture()
        if image is None:
            return None, why
        write_png(path, image)
        self.refresh_view()
        rect = self.view["rect"] if self.view else None
        region = (rect["x"], rect["y"], rect["width"], rect["height"]) if rect else None
        stats = analyze(image, region)
        self.record_gpu_fds()
        return image, stats

    def do_screenshot(self, step, record):
        name = step["screenshot"]
        path = os.path.join(self.cfg["shots_dir"], f"{name}.png")
        image, stats = self.grab(path)
        if image is None:
            record["ok"] = False
            self.check(f"screenshot_{name}", False, stats)
            return
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

    def check_gpu_used(self, name="gpu_used_by_game"):
        """--gpu. One GPU: the game's own process has the node open. Wine's helpers
        open it whether or not the game renders there. Several: it holds them all, so
        --expect-gpu needs it shown to render there; with none, holding one will do,
        and the summary says what it renders on, if anything shows. With relaunches
        it's asked per launch: one with no screenshot says nothing, and only fails
        if none of them had one."""
        gpus = self.cfg["gpu"]["gpus"]
        if not gpus or self.manifest["command"]:
            return
        if self.gpu_checked and not self.gpu_seen:
            return
        gpu = self.result["gpu"]
        game_fds = gpu.get("game_gpu_fds", {})
        gpu["game_nodes"] = held_nodes(game_fds, gpus)
        try:
            with open(os.path.join(self.results, "game.out"), errors="replace") as f:
                gpu["dxvk"] = parse_dxvk(f.read())
        except OSError:
            gpu["dxvk"] = parse_dxvk("")
        gpu["game_renders_on"] = renders_on(gpus, gpu["dxvk"], gpu.get("game_drm_use", {}))
        want = self.cfg["gpu"]["want"]
        declared = (self.manifest.get("gpu") or {}).get("wayland") == "unused"
        if self.display == "wayland" and declared and not self.cfg["gpu"].get("expect_given"):
            # Declared: GDI into shm buffers, no GPU on Wayland. Holding a node
            # now means the game changed how it draws: look again.
            held = gpu["game_nodes"]
            self.check(
                name,
                not held,
                f'[gpu] wayland = "unused": the game holds {held or "no GPU node"}, want none'
                f" (exe {self.effective_exe()})",
            )
            return
        if len(gpus) == 1:
            held = gpus[0]["held"]
            users = sorted(k for k, v in game_fds.items() if held in v)
            others = sorted(
                k for k, v in gpu.get("gpu_fds", {}).items() if held in v and k not in users
            )
            self.check(
                name,
                bool(users),
                f"{held}: game {users or 'nobody'}, Wine's helpers and others {others or 'nobody'}"
                f" (exe {self.effective_exe()})",
            )
            return
        on = gpu["game_renders_on"]
        self.check(
            name,
            want in on if want else bool(gpu["game_nodes"]),
            f"renders on {on or 'nothing DXVK or the kernel can show'}, want {want or 'any'}; "
            f"holds {gpu['game_nodes']}; DXVK found {gpu['dxvk']['found']}, made "
            f"{gpu['dxvk']['created']} (exe {self.effective_exe()})",
        )

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

    def left_of_game(self, wait=RELAUNCH_LEFT_SECS):
        """What's still up in this sandbox besides the harness, once the launcher
        has exited: ["pid comm"], after up to `wait` s for it to go."""
        end = time.monotonic() + wait
        while True:
            left = left_processes(_proc_table(), self.before_game)
            if not left or time.monotonic() >= end:
                return left
            self.sleep(0.25)

    def end_launch_gpu(self):
        """The launch that just ended: its own check, under its own name, and
        what it held out of the next one's way."""
        if self.gpu_seen:
            self.check_gpu_used(f"gpu_used_by_game_launch_{self.relaunches}")
            self.gpu_checked = True
        gpu = self.result["gpu"]
        held = {k: gpu.pop(k) for k in ("gpu_fds", "game_gpu_fds", "game_drm_use") if k in gpu}
        gpu.setdefault("launches", []).append(held)
        self.gpu_seen = False

    def do_relaunch(self, record, move=None):
        """The `relaunch` step (#252): the game has quit by itself, by [exit]'s
        wait_secs and with its code, nothing of it is left, and it's launched
        again in this sandbox, on this disk, up to its window. record["ok"] false
        ends a run's steps: the route has no way on from there. With `move` (#278)
        the game is moved first, and launched from its menu entry's new path."""
        self.relaunches += 1
        tag = f"relaunch_{self.relaunches}"
        want = self.manifest["exit"]
        started = time.monotonic()
        while self.game.poll() is None:
            if time.monotonic() - started >= want["wait_secs"]:
                self.tool("grim", os.path.join(self.cfg["shots_dir"], f"not-exited-{tag}.png"))
                record.update(ok=False, why=f"the game was still running {want['wait_secs']:.0f} s on")
                self.check(f"clean_exit_{tag}", False, record["why"])
                return
            self.sleep(0.25)
        self.game_exit = self.result["game_exit"] = self.game.returncode
        record.update(exit=self.game_exit, exit_secs=round(time.monotonic() - started, 1))
        ok = self.game_exit == want["code"]
        self.check(f"clean_exit_{tag}", ok, f"exit {self.game_exit}, want {want['code']}")
        if not ok:
            record.update(ok=False, why=f"the game exited {self.game_exit}, not {want['code']}")
            return
        left = self.left_of_game()
        self.check(f"nothing_left_{tag}", not left, f"swept: {'; '.join(left)}" if left else "no process of the launch left")
        for line in left:
            try:
                os.kill(int(line.split()[0]), signal.SIGKILL)
            except OSError:
                pass
        self.end_launch_gpu()
        # game.out is the launch's own, as a life's is.
        out = os.path.join(self.results, "game.out")
        if os.path.exists(out):
            os.replace(out, os.path.join(self.results, f"launch{self.relaunches}-game.out"))
        if move is not None and not self.do_move(move, record):
            return
        self.view = None
        self.game_exit = self.result["game_exit"] = None
        if self.live is not None:
            self.session["game_exit"] = None
        started = time.monotonic()
        self.launch()
        got = self.wait_window(f"window_{tag}")
        record["window_secs"] = round(time.monotonic() - started, 1)
        if not got:
            record.update(ok=False, why=f"no window after the relaunch: {self.checks[-1]['detail']}")

    def moved_game_root(self):
        """Where `gogo apps list` says the game is."""
        c = self.cfg
        r = self.tool(c["doctor"][0], "apps", "list", "--json", timeout=60)
        rows = json.loads(r.stdout)
        rows = rows if isinstance(rows, list) else rows.get("apps", [])
        return next(a["root"] for a in rows if str(a["gogid"]) == c["doctor"][3])

    def do_move(self, where, record):
        """`gogo linux-app move` between two launches: a save planted first, a real
        kill partway through a copy, then the checks of a game at its new place.
        Whether it worked, record["why"] saying what didn't."""
        c = self.cfg
        gogo, gogid = c["doctor"][0], c["doctor"][3]
        old = self.moved_game_root()
        users = os.path.join(old, "prefix/drive_c/users")
        sentinel = os.path.join("steamuser", "gogo-e2e-save.sav")
        with open(os.path.join(users, sentinel), "w") as f:
            f.write(f"a save from before the move to {where}\n")
        before = tree_digest(os.path.join(old, "prefix"))
        argv = [gogo, "linux-app", "move", gogid, "--json"]
        argv += ["--default"] if where == "default" else [c["move_targets"][where]]

        def failed(name, why):
            self.check(name, False, why)
            record.update(ok=False, why=f"{name}: {why}")
            return False

        if where == "other_fs":
            # Killed for real, as a power cut or a `kill -9` would. A copy this
            # fast may finish first: then there's nothing to check.
            p = subprocess.Popen(argv, env=self.env, stdin=subprocess.DEVNULL,
                                 stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            time.sleep(0.3)
            p.kill()
            p.wait()
            if self.moved_game_root() == old:
                if tree_digest(os.path.join(old, "prefix")) != before:
                    return failed("move_killed", "the kill changed the game's prefix")
                report = self.doctor(f"move-killed-{self.relaunches}") or {}
                noted = [f for f in report.get("findings", []) if f.get("check") == "move"]
                self.check("move_killed", True,
                           f"killed mid-copy: the game stays at {old}; doctor: "
                           + (noted[0]["message"] if noted else "nothing left to report"))
            else:
                self.check("move_killed", True, "the move finished before the kill")

        started = time.monotonic()
        r = self.tool(*argv, timeout=900)
        record["move_secs"] = round(time.monotonic() - started, 1)
        if r.returncode != 0 and self.moved_game_root() == old:
            return failed("move", f"exit {r.returncode}: {r.stderr.strip()[-400:]}")
        if r.returncode != 0:
            # Only what the kill above left: already there, nothing to do.
            if "already there" not in r.stderr:
                return failed("move", f"exit {r.returncode}: {r.stderr.strip()[-400:]}")
            out = {"method": "finished before the kill", "to": self.moved_game_root()}
        else:
            out = json.loads(r.stdout)
        record["move"] = {k: out.get(k) for k in ("method", "from", "to", "bytes", "warnings")}
        self.check("move", True, f"{old} -> {out['to']}, by {out['method']}, in {record['move_secs']} s")

        root = self.moved_game_root()
        if root != out["to"] or os.path.exists(old):
            return failed("move_place", f"apps list says {root}, wanted {out['to']}; {old} there: {os.path.exists(old)}")
        self.check("move_place", True, f"apps list says {root}, and {old} is gone")
        if tree_digest(os.path.join(root, "prefix")) != before:
            return failed("move_saves", "the prefix's links, registry or saves changed")
        self.check("move_saves", True, "the planted save, every other file under users/, the links and the registry match")
        report = self.doctor(f"moved-{self.relaunches}") or {}
        bad = [f for f in report.get("findings", [])
               if f.get("check") in ("links", "location", "move", "launcher", "menu") and f.get("level") != "ok"]
        if bad or "findings" not in report:
            return failed("move_doctor", json.dumps(bad or report)[:400])
        self.check("move_doctor", True, "links, location and the menu entry are ok at the new place")

        # What a user launches: the menu entry's own path.
        entry = os.path.join(self.env["GOGO_HOME"], "applications", f"gogo-{gogid}.desktop")
        with open(entry) as f:
            line = next(l for l in f if l.startswith("Exec="))
        launcher = shlex.split(line[len("Exec="):])[0]
        if launcher != os.path.join(root, "launch.sh"):
            return failed("move_menu_entry", f"it runs {launcher}")
        self.check("move_menu_entry", True, f"it runs {launcher}")
        c["launch"], c["launch_cwd"] = [launcher], root
        return True

    def apply_override(self):
        """The manifest's [override], through gogo in this run's scratch."""
        argvs = self.cfg.get("pre_launch") or []
        if not argvs:
            return
        failed = []
        for argv in argvs:
            r = self.tool(*argv, timeout=60)
            if r.returncode != 0:
                failed.append(f"{' '.join(argv[1:3])}: exit {r.returncode}: {r.stderr.strip()[-300:]}")
        self.check(
            "override",
            not failed,
            "; ".join(failed) or f"{len(argvs)} gogo call(s) wrote the override file",
        )
        if failed:
            raise RuntimeError(f"override failed: {failed[0]}")

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

    def stop_services(self):
        self.save()
        if self.sway and self.sway.poll() is None:
            self.tool("swaymsg", "exit")
            try:
                self.sway.wait(5)
            except subprocess.TimeoutExpired:
                self.sway.kill()
        if self.pulse and self.pulse.poll() is None:
            self.pulse.terminate()
            try:
                self.pulse.wait(5)
            except subprocess.TimeoutExpired:
                self.pulse.kill()

    # build ---------------------------------------------------------------

    def note_build_windows(self, windows, started):
        """A build isn't watched: what it opened is the only sign of a prompt."""
        try:
            tree = json.loads(self.swaymsg("-t", "get_tree", "--raw"))
        except (RuntimeError, ValueError, subprocess.TimeoutExpired):
            return
        for view in walk_views(tree):
            name = view.get("name") or ""
            if any(w["name"] == name for w in windows):
                continue
            shot = os.path.join(self.cfg["shots_dir"], f"build-window-{len(windows) + 1}.png")
            self.tool("grim", shot)
            windows.append(
                {"name": name, "after_secs": round(time.monotonic() - started), "shot": shot}
            )
            self.save()

    def run_build(self):
        env = dict(self.env)
        env.update(self.cfg["game_env"])
        windows = []
        self.result["windows"] = windows
        started = time.monotonic()
        log_path = os.path.join(self.results, "build.log")
        with open(log_path, "wb") as log:
            build = subprocess.Popen(
                self.cfg["build_command"],
                env=env,
                cwd=self.cfg["launch_cwd"],
                stdin=subprocess.DEVNULL,
                stdout=log,
                stderr=subprocess.STDOUT,
            )
        last_look = 0.0
        while build.poll() is None:
            if time.monotonic() - last_look >= 2:
                last_look = time.monotonic()
                self.note_build_windows(windows, started)
            time.sleep(0.2)
        self.result["exit"] = build.returncode
        if self.cfg.get("installer"):
            for name, path in install_logs(self.cfg["games_dir"]):
                shutil.copy(path, os.path.join(self.results, f"install-{name}"))
        self.result["secs"] = round(time.monotonic() - started)
        with open(log_path, errors="replace") as f:
            support = parse_support_installer(f.read())
        if support is not None:
            support.update(self.support_log())
        self.result["support_installer"] = support
        return build.returncode

    def support_log(self):
        """Did Inno write its log? A copy goes to the results: scratch is deleted."""
        path = self.cfg.get("support_log")
        if not path or not os.path.isfile(path):
            return {"inno_log": False, "inno_bytes": 0}
        shutil.copy(path, os.path.join(self.results, "support-inno.log"))
        return {"inno_log": True, "inno_bytes": os.path.getsize(path)}

    def main_build(self):
        """The build phase, on a display of its own (software: it has no GPU node)."""
        try:
            self.start_sway()
            self.assert_isolation()
            bad = [c for c in self.checks if not c["ok"]]
            if bad:
                raise RuntimeError(f"isolation failed: {bad[0]['name']}: {bad[0]['detail']}")
            code = self.run_build()
            self.check("finished", True, f"build exited {code}")
            return code
        except Stop:
            self.check("finished", False, "stopped from outside (signal)")
            return 1
        except Exception as e:  # recorded: the outer script reports it as a harness problem
            self.check("finished", False, f"{type(e).__name__}: {e}")
            return BUILD_NO_DISPLAY
        finally:
            self.stop_services()

    # game ----------------------------------------------------------------

    def main(self):
        how = self.manifest["exit"]["how"]
        try:
            self.start_sway()
            self.start_audio()
            self.phase("sound_server")
            self.assert_isolation()
            self.phase("isolation")
            self.probe_audio()
            bad = [c for c in self.checks if not c["ok"]]
            if bad:
                # A display that isn't provably ours: never launch.
                raise RuntimeError(f"isolation failed: {bad[0]['name']}: {bad[0]['detail']}")
            self.probe_gpu()
            self.apply_override()
            self.phase("override")
            self.doctor("before")
            self.phase("doctor")
            self.check_audio()
            self.phase("sound_check")
            bad = [c for c in self.checks if not c["ok"]]
            if bad:
                raise RuntimeError(f"isolation failed: {bad[0]['name']}: {bad[0]['detail']}")
            self.launch()
            if self.wait_window():
                self.phase("window")
                stuck = False
                for step in self.manifest["steps"]:
                    self.run_step(step)
                    if any(k in step for k in ("relaunch", "move_relaunch", *LIVE_UNTIL)) and not self.steps[-1]["ok"]:
                        stuck = True
                        break
                if stuck:
                    self.finish_game("cleanup")
                    self.record_audio_streams()
                    self.x_fallback()
                    self.check("finished", True, f"{self.steps[-1]['action']} failed, so no more steps: {self.steps[-1]['why']}")
                else:
                    self.check_gpu_used()
                    self.finish_game(how)
                    self.check_doctor()
                    self.record_audio_streams()
                    self.x_fallback()
                    self.check("finished", True, "every step ran")
            else:
                self.finish_game("cleanup")
                self.record_audio_streams()
                self.x_fallback()
                self.check("finished", True, "no window, so no steps")
        except Stop:
            self.check("finished", False, "stopped from outside (timeout or signal)")
            self.finish_game("cleanup")
        except Exception as e:  # recorded as a failed check: the run still reports
            self.check("finished", False, f"{type(e).__name__}: {e}")
            self.finish_game("cleanup")
        finally:
            self.stop_services()
        return 0

    # live (#241) ---------------------------------------------------------
    #
    # A live session is a game phase that takes its steps from a control
    # socket instead of a manifest, one command at a time. Each life is one
    # sandbox: `restore` ends it (LIVE_RELAUNCH), the outer script rolls the
    # disk back (to the launch, or to the last disk a clean stop kept on the
    # checkpoint's path, #252) and starts the next, which replays what's left of
    # the path before it listens again. The path, the checkpoints and every
    # command are in the results dir, which outlives the sandbox.

    def main_live(self):
        lv = self.live
        self.session = live_load_session(lv["session"], self.manifest, lv)
        self.life = lv["life"]
        self.last_end = None
        self.last_command = time.monotonic()
        outcome = "stop"
        replay = self.session.get("replay") or {}
        restored = self.session["restores"][-1] if replay else None
        started = self.phase_at
        if restored is not None:
            restored["rollback_secs"] = lv.get("rollback_secs")
        try:
            self.live_state("starting", "the display and the isolation checks")
            self.start_sway()
            self.start_audio()
            self.phase("sound_server")
            self.assert_isolation()
            self.phase("isolation")
            self.probe_audio()
            bad = [c for c in self.checks if not c["ok"]]
            if bad:
                raise RuntimeError(f"isolation failed: {bad[0]['name']}: {bad[0]['detail']}")
            self.probe_gpu()
            # A disk from a clean stop has them already, and the export's
            # relaunch doesn't write them again either.
            if not replay.get("disk"):
                self.apply_override()
            self.phase("override")
            self.live_doctor_before(replay.get("disk") or "launch")
            self.phase("doctor")
            self.check_audio()
            self.phase("sound_check")
            bad = [c for c in self.checks if not c["ok"]]
            if bad:
                raise RuntimeError(f"isolation failed: {bad[0]['name']}: {bad[0]['detail']}")
            self.live_listen()
            self.busy = "launching the game"
            self.live_state("launching", self.busy)
            launched = time.monotonic()
            if restored is not None:
                restored["sandbox_secs"] = round(time.time() - restored["at"] - restored.get("close_secs", 0) - (restored["rollback_secs"] or 0), 1)
                restored["phases"] = live_restore_phases(restored, lv.get("between"), self.phases, started)
            self.launch()
            self.busy = "waiting for the game's window"
            self.live_state("launching", self.busy)
            self.session["game_exit"] = None
            try:
                if self.wait_window():
                    self.last_end = time.monotonic()
                    if restored is not None:
                        restored["window_secs"] = round(self.last_end - launched, 1)
                    self.live_from_manifest()
                    self.live_replay()
                    if restored is not None:
                        restored["replay_secs"] = (self.session.get("last_replay") or {}).get("secs")
                        restored["total_secs"] = round(time.time() - restored["at"], 1)
                        self.live_save()
                elif lv.get("prefix") and self.session["from_manifest"] is None:
                    self.live_from_manifest_result(0, lv["prefix"], "the game's window never came up")
                outcome = self.live_serve()
            except LiveStop:
                outcome = "stop"
            except LiveRetry:
                outcome = "restore"
            if outcome == "restore":
                closing = time.monotonic()
                self.close_game()
                self.session["restores"][-1]["close_secs"] = round(time.monotonic() - closing, 1)
                self.session["restores"][-1]["closed_at"] = round(time.time(), 2)
                self.live_save()
                self.check("finished", True, f"closed for a restore of {self.session['replay']['checkpoint']}")
                return LIVE_RELAUNCH
            if outcome == "idle":
                self.check(
                    "live_idle",
                    False,
                    f"no command for {lv['idle_secs']:.0f} s: stopped by the idle limit",
                )
            self.busy = "stopping"
            self.live_state("stopping", f"{outcome}: closing the game")
            # A life can come to its stop with no screenshot now (a restore that
            # replays nothing): a frame while the game's up shows what it holds,
            # and with the game gone there's nothing to ask. Not a proof anyway.
            ended = self.game.poll() is not None
            if not ended and not self.gpu_seen:
                self.grab(os.path.join(self.run_dir, "motion.png"))
            if self.gpu_seen:
                self.check_gpu_used()
            # Quit from its own menu: it exits by itself, which the export keeps.
            self.manifest["exit"]["code"] = 0
            self.finish_game("self" if ended else "signal")
            self.session["ended"] = {"how": "self" if ended else "signal", "code": self.game_exit}
            self.live_save()
            self.check_doctor()
            self.record_audio_streams()
            self.x_fallback()
            self.check("finished", True, f"live session ended ({outcome})")
        except (Stop, LiveStop):
            self.check("finished", False, "stopped from outside (time limit or signal)")
            self.finish_game("cleanup")
        except Exception as e:  # recorded as a failed check: the session still reports
            self.check("finished", False, f"{type(e).__name__}: {e}")
            self.finish_game("cleanup")
        finally:
            if self.srv is not None:
                self.srv.close()
                self.srv = None
                try:
                    os.unlink(os.path.join(lv["dir"], LIVE_SOCKET))
                except OSError:
                    pass
            self.stop_services()
        return 0

    def live_doctor_before(self, disk):
        """doctor's report before the launch, once per disk: a record, no check
        reads it, and a life rolled back to a disk gets what the last one on it
        got (7 s on Cyberpunk, #257). doctor-after, at a stop, always runs."""
        seen = self.session.setdefault("doctored", {})
        if disk in seen:
            write_json(os.path.join(self.results, "doctor-before.json"), {"same_disk_as_life": seen[disk]})
            return
        self.doctor("before")
        seen[disk] = self.life

    def close_game(self):
        """Close the game for a restore: TERM to the launcher, its own teardown."""
        if self.game is None:
            return
        if self.game.poll() is None:
            self.game.send_signal(signal.SIGTERM)
            try:
                self.game.wait(30)
            except subprocess.TimeoutExpired:
                os.killpg(self.game.pid, signal.SIGKILL)
                self.game.wait()
        self.game_exit = self.result["game_exit"] = self.game.returncode

    def live_save(self):
        write_json(self.live["session"], self.session)

    def live_state(self, state, detail=""):
        write_json(
            self.live["state"],
            {"state": state, "detail": detail, "life": self.life, "at": round(time.time(), 1)},
        )

    def live_journal(self, cmd, args, reply):
        self.session["seq"] += 1
        entry = {
            "seq": self.session["seq"],
            "at": round(time.time(), 2),
            "life": self.life,
            "cmd": cmd,
            "args": args,
            "reply": {k: v for k, v in reply.items() if k not in ("status",)},
        }
        with open(self.live["journal"], "a") as f:
            f.write(json.dumps(entry) + "\n")

    def live_listen(self):
        """The control socket, `ctl.sock` in the session's dir, ours only. Bound
        through the dir's fd: the scratch path can pass a socket's 108 bytes."""
        d = self.live["dir"]
        os.makedirs(d, mode=0o700, exist_ok=True)
        os.chmod(d, 0o700)
        fd = os.open(d, os.O_PATH | os.O_DIRECTORY)
        try:
            path = f"/proc/self/fd/{fd}/{LIVE_SOCKET}"
            try:
                os.unlink(path)
            except FileNotFoundError:
                pass
            srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            old = os.umask(0o077)
            try:
                srv.bind(path)
            finally:
                os.umask(old)
        finally:
            os.close(fd)
        srv.listen(8)
        srv.setblocking(False)
        self.srv = srv

    def live_accept(self, timeout):
        """One request off the socket within `timeout`: (conn, request) or None."""
        ready, _, _ = select.select([self.srv], [], [], timeout)
        if not ready:
            return None
        try:
            conn, _ = self.srv.accept()
        except (BlockingIOError, InterruptedError):
            return None
        conn.setblocking(True)
        conn.settimeout(10)
        data = b""
        try:
            while not data.endswith(b"\n") and len(data) < 65536:
                chunk = conn.recv(4096)
                if not chunk:
                    break
                data += chunk
            req = json.loads(data)
            if (
                not isinstance(req, dict)
                or not isinstance(req.get("cmd"), str)
                or not isinstance(req.get("args", []), list)
                or not all(isinstance(a, str) for a in req.get("args", []))
            ):
                raise ValueError("want {cmd: string, args: [string]}")
        except (OSError, ValueError) as e:
            self.live_reply(conn, {"ok": False, "error": f"bad request: {e}"})
            return None
        req.setdefault("args", [])
        return conn, req

    @staticmethod
    def live_reply(conn, reply):
        try:
            conn.sendall((json.dumps(reply) + "\n").encode())
        except OSError:
            pass
        finally:
            conn.close()

    def live_busy(self, timeout):
        """While a command, the launch or a replay runs: status answers, stop
        stops, anything else is told to wait."""
        got = self.live_accept(timeout)
        if not got:
            return
        conn, req = got
        if req["cmd"] == "status":
            self.live_reply(conn, self.live_status())
        elif req["cmd"] == "stop":
            self.live_reply(conn, {"ok": True, "stopping": True, "while": self.busy})
            self.live_journal("stop", [], {"ok": True, "while": self.busy})
            raise LiveStop()
        else:
            self.live_reply(conn, {"ok": False, "busy": self.busy or "busy", "error": "busy: try again"})

    def live_serve(self):
        """Commands until stop, restore or the idle limit: "stop", "restore", "idle"."""
        self.busy = None
        self.last_command = time.monotonic()
        while True:
            if os.path.exists(self.cfg["stop_file"]):
                self.result["stopped"] = True
                raise Stop()
            self.live_watch_game()
            if self.last_end is None:
                self.last_end = time.monotonic()
            idle = time.monotonic() - self.last_command
            if idle >= self.live["idle_secs"]:
                return "idle"
            got = self.live_accept(0.5)
            if not got:
                continue
            conn, req = got
            self.last_command = time.monotonic()
            outcome = self.live_handle(conn, req)
            self.last_command = time.monotonic()
            if outcome:
                return outcome

    def live_watch_game(self):
        state = "ready"
        detail = ""
        if self.game is not None and self.game.poll() is not None:
            if self.session.get("game_exit") is None:
                self.game_exit = self.result["game_exit"] = self.game.returncode
                self.session["game_exit"] = {"code": self.game.returncode, "life": self.life}
                self.live_save()
            detail = f"the game exited {self.game.returncode}: checkpoint, relaunch, restore, or stop"
        elif self.view is None:
            detail = "no window: restore, or stop"
        replay = self.session.get("last_replay")
        if replay and replay.get("life") == self.life and not replay.get("ok"):
            detail = (detail + "; " if detail else "") + f"replay stopped: {replay.get('why')}"
        if getattr(self, "_shown", None) != (state, detail):
            self._shown = (state, detail)
            self.live_state(state, detail)

    def live_status(self):
        cps = {
            name: {
                "steps": live_count_steps(cp["path"]),
                "replay_secs": live_replay_secs(cp["path"], self.session),
                "clean_stop": "disk" in cp,
            }
            for name, cp in self.session["checkpoints"].items()
        }
        rect = self.view["rect"] if self.view else None
        running = self.game is not None and self.game.poll() is None
        return {
            "ok": True,
            "state": "busy" if self.busy else "ready",
            "busy": self.busy,
            "life": self.life,
            "game": {"running": running, "exit": None if running or self.game is None else self.game.returncode},
            "window": rect and {k: rect[k] for k in ("x", "y", "width", "height")},
            "steps": live_count_steps(self.session["path"]),
            "checkpoints": cps,
            "restores": len(self.session["restores"]),
            "idle_left": round(self.live["idle_secs"] - (time.monotonic() - self.last_command)),
            "last_replay": self.session.get("last_replay"),
            "last_restore": (self.session["restores"] or [None])[-1],
            "from_manifest": self.session.get("from_manifest"),
        }

    def live_handle(self, conn, req):
        """Run one command and answer it. Returns "stop" or "restore" when the
        life ends here."""
        cmd, args = req["cmd"], req["args"]
        outcome = None
        self.busy = None if cmd == "status" else f"running {cmd}"
        try:
            if cmd == "status":
                reply = self.live_status()
            elif cmd == "stop":
                reply, outcome = {"ok": True, "stopping": True}, "stop"
            elif cmd == "restore":
                reply = self.live_restore(args)
                outcome = "restore" if reply["ok"] else None
            elif cmd == "checkpoint":
                reply = self.live_checkpoint(args)
            elif cmd == "relaunch":
                reply = self.live_relaunch(args)
            elif cmd == "settle":
                reply = self.live_settle(args)
            elif cmd in LIVE_STEP_CMDS:
                reply = self.live_step(cmd, args)
            else:
                reply = {"ok": False, "error": f"unknown command {cmd!r}"}
        except LiveError as e:
            reply = {"ok": False, "error": str(e)}
        except LiveStop:
            reply, outcome = {"ok": False, "error": f"{cmd}: interrupted by a stop"}, "stop"
        except Stop:
            self.live_reply(conn, {"ok": False, "error": f"{cmd}: the session is being stopped"})
            raise
        except Exception as e:  # the session goes on: one command failed
            reply = {"ok": False, "error": f"{cmd}: {type(e).__name__}: {e}"}
        finally:
            self.busy = None
            # What a long command put in the state file goes on the next look.
            self._shown = None
        self.live_journal(cmd, args, reply)
        self.live_save()
        self.live_reply(conn, reply)
        return outcome

    def live_step(self, cmd, args):
        """An input, a wait or a screenshot: run it and put it on the path."""
        if self.game is None:
            raise LiveError("no game: restore, or stop")
        started = time.monotonic()
        gap = max(0.0, started - (self.last_end or started))
        rect = self.refresh_view()
        rect = rect and rect["rect"]
        raw = live_raw_step(cmd, args, rect)
        if cmd == "screenshot":
            raw = {"screenshot": live_unique_name(raw["screenshot"], self.session["names"])}
        try:
            step = _step(raw, 0, set())
        except ManifestError as e:
            raise LiveError(str(e).removeprefix("step 1: ")) from e
        entry = {"step": step, "gap": round(gap, 2), "life": self.life, "seq": self.session["seq"] + 1}
        if cmd == "screenshot":
            reply = self.live_screenshot(entry)
        else:
            self.run_step(step)
            record = self.steps[-1]
            if not record["ok"] and cmd in LIVE_UNTIL:
                return self.live_press_failed(step[cmd], record, gap)
            if not record["ok"]:
                raise LiveError(f"{cmd}: no window")
            reply = {"ok": True, "step": step}
            if cmd in LIVE_UNTIL:
                entry["press_secs"] = record["secs"]
                reply.update(presses=record["presses"], seen_pct=record["seen_pct"])
            if "at" in record:
                reply["at"] = record["at"]
        self.session["path"].append(entry)
        self.last_end = time.monotonic()
        reply["gap"] = entry["gap"]
        reply["secs"] = round(self.last_end - started, 2)
        reply["game_running"] = self.game.poll() is None
        return reply

    def live_press_failed(self, want, record, gap):
        """press_until ran out. Its keys did reach the game, so the path has them,
        as the plain keys they were: a press_until on the path means one that
        passed, and an export that says so would fail where this did."""
        for n in range(record["presses"] if "key" in want else 0):
            self.session["path"].append(
                {"step": {"key": want["key"]}, "gap": round(gap, 2) if n == 0 else want["every"],
                 "life": self.life, "seq": self.session["seq"] + 1}
            )
        self.last_end = time.monotonic()
        return {
            "ok": False, "error": record["why"], "presses": record["presses"],
            "seen_pct": record["seen_pct"], "path": record.get("path"),
            "game_running": self.game.poll() is None,
            "note": "the path has them as plain keys: restore to drop them",
        }

    def live_settle(self, args):
        """Wait until the screen holds still, at most --max s: it settled, or it
        never did (an animation, a video loop). On the path as wait_settled
        or, if it never settled, as the plain wait it took."""
        if self.game is None:
            raise LiveError("no game: restore, or stop")
        secs = live_settle_max(args)
        started = time.monotonic()
        gap = max(0.0, started - (self.last_end or started))
        self.refresh_view()
        if self.view is None:
            raise LiveError("settle: no window")
        got = self.settle_wait(secs)
        spent = round(time.monotonic() - started, 1)
        entry = {"gap": round(gap, 2), "life": self.life, "seq": self.session["seq"] + 1}
        if got["settled"]:
            entry["step"] = {"wait_settled": live_settle_timeout(spent)}
            entry["settled_secs"] = spent
        else:
            entry["step"] = {"wait": spent}
            entry["unsettled"] = True
        path = os.path.join(self.cfg["shots_dir"], f"settle-{entry['seq']}.png")
        shot = None
        try:
            shutil.copyfile(os.path.join(self.run_dir, "settling.png"), path)
            shot = path
        except OSError:
            pass
        entry["image"] = shot
        self.session["path"].append(entry)
        self.last_end = time.monotonic()
        return {
            "ok": True,
            "settled": got["settled"],
            "secs": spent,
            "max": secs,
            "path": shot,
            "step": entry["step"],
            "gap": entry["gap"],
            "game_running": self.game.poll() is None,
        }

    def live_screenshot(self, entry):
        name = entry["step"]["screenshot"]
        path = os.path.join(self.cfg["shots_dir"], f"{name}.png")
        image, stats = self.grab(path)
        if image is None:
            raise LiveError(f"screenshot: {stats}")
        self.session["names"].append(name)
        # How much the screen moves by itself: a video or an animated menu
        # can't be matched pixel for pixel in a replay (live_replay_shot).
        time.sleep(LIVE_MOTION_SECS)
        again, _ = self.grab(os.path.join(self.run_dir, "motion.png"))
        moving = None if again is None else changed_pct(image, again)
        since, inputs = live_last_shot(self.session["path"])
        change = None
        if since is not None and since in self.shots:
            change = changed_pct(self.shots[since], image)
        self.shots[name] = image
        self.live_keep_ref(name, image)
        entry.update(image=path, stats=stats, since=since, inputs_since=inputs, change_pct=change, moving_pct=moving)
        fill = self.manifest["window"]["min_fill"]
        rect = self.view["rect"] if self.view else None
        return {
            "ok": True,
            "name": name,
            "path": path,
            "change_pct": change,
            "since": since,
            "moving_pct": moving,
            "nonblack_pct": stats["nonblack_pct"],
            "fills_window": stats["box_w_pct"] >= fill["w"] and stats["box_h_pct"] >= fill["h"],
            "window": rect and {k: rect[k] for k in ("x", "y", "width", "height")},
        }

    def live_checkpoint(self, args):
        if len(args) != 1 or not re.fullmatch(r"[A-Za-z0-9_-]+", args[0]):
            raise LiveError("checkpoint NAME: a name of [A-Za-z0-9_-]")
        name = args[0]
        if self.game is not None and self.game.poll() is not None:
            return self.live_checkpoint_clean(name)
        shot = self.live_step("screenshot", [f"cp-{name}"])
        self.session["path"].append({"mark": "checkpoint", "name": name})
        replaced = name in self.session["checkpoints"]
        self.session["checkpoints"][name] = {
            "name": name,
            "path": json.loads(json.dumps(self.session["path"])),
            "life": self.life,
            "at": round(time.time(), 1),
            "shot": shot["path"],
        }
        return {
            "ok": True,
            "checkpoint": name,
            "replaced": replaced,
            "clean_stop": False,
            "steps": live_count_steps(self.session["path"]),
            "replay_secs": live_replay_secs(self.session["path"], self.session),
            "screenshot": shot["path"],
        }

    def live_clean_stop(self, cmd):
        """The game has exited, as a Quit from its menu does: with [exit]'s code,
        and with nothing of it left to write the disk. Else `cmd` is refused."""
        want = self.manifest["exit"]["code"]
        code = self.game.returncode
        if code != want:
            raise LiveError(
                f"{cmd}: the game exited {code}, not the {want} its [exit] expects: "
                "not a clean stop. restore goes back, stop ends it"
            )
        left = self.left_of_game()
        if left:
            raise LiveError(
                f"{cmd}: the launcher exited, but {'; '.join(left)} is still up {RELAUNCH_LEFT_SECS:.0f} s on: "
                "not a clean stop. restore goes back, stop ends it"
            )

    def live_keep_disk(self, disk_id):
        """Ask the supervisor, outside the sandbox, to keep the disk as it is: it
        copies the game's gogo, home and xdg as a base's are, without this
        sandbox's bind mounts in them. {copy, secs}, or a LiveError."""
        d = self.live["dir"]
        reply = os.path.join(d, LIVE_DISK_REPLY)
        try:
            os.unlink(reply)
        except FileNotFoundError:
            pass
        tmp = os.path.join(d, f"{LIVE_DISK_REQUEST}.tmp")
        with open(tmp, "w") as f:
            f.write(f"{disk_id}\n")
        os.replace(tmp, os.path.join(d, LIVE_DISK_REQUEST))
        end = time.monotonic() + LIVE_DISK_SECS
        while not os.path.exists(reply):
            if time.monotonic() >= end:
                raise LiveError(f"checkpoint: no copy of the disk in {LIVE_DISK_SECS:.0f} s")
            self.sleep(0.1)
        with open(reply) as f:
            got = json.load(f)
        os.unlink(reply)
        if not got.get("ok"):
            raise LiveError(f"checkpoint: couldn't keep the disk: {got.get('error')}")
        return got

    def live_checkpoint_clean(self, name):
        """A checkpoint at a clean stop (#252): the disk as the game left it. A
        restore to it relaunches on a copy of that, with no replay. No screenshot:
        there's no game to show."""
        self.live_clean_stop("checkpoint")
        self.busy = f"keeping the disk for {name}"
        self.live_state("keeping", self.busy)
        disk_id = self.session["seq"] + 1
        got = self.live_keep_disk(disk_id)
        self.session["path"].append({"mark": "checkpoint", "name": name, "disk": got["dir"]})
        replaced = name in self.session["checkpoints"]
        self.session["checkpoints"][name] = {
            "name": name,
            "path": json.loads(json.dumps(self.session["path"])),
            "life": self.life,
            "at": round(time.time(), 1),
            "shot": None,
            "disk": got["dir"],
        }
        return {
            "ok": True,
            "checkpoint": name,
            "replaced": replaced,
            "clean_stop": True,
            "steps": live_count_steps(self.session["path"]),
            "replay_secs": 0,
            "screenshot": None,
            "disk": {"copy": got["copy"], "secs": got["secs"]},
        }

    def live_relaunch(self, args):
        """The game exited by itself: launch it again here, on this disk. Refused
        while it runs: quitting is the game's to do, and restore is what closes it."""
        if args:
            raise LiveError("usage: relaunch")
        if self.game is None:
            raise LiveError("no game: restore, or stop")
        if self.game.poll() is None:
            raise LiveError(
                "relaunch: the game is still running. It follows a game that quit by "
                "itself (status says when it has); restore closes a running one"
            )
        self.live_clean_stop("relaunch")
        started = time.monotonic()
        gap = max(0.0, started - (self.last_end or started))
        self.busy = "relaunching the game"
        self.live_state("launching", self.busy)
        step = {"relaunch": True}
        entry = {"step": step, "gap": round(gap, 2), "life": self.life, "seq": self.session["seq"] + 1}
        self.run_step(step)
        record = self.steps[-1]
        if not record["ok"]:
            raise LiveError(f"relaunch: {record['why']}")
        entry["relaunch_secs"] = record["window_secs"]
        self.session["path"].append(entry)
        self.last_end = time.monotonic()
        return {"ok": True, "step": step, "gap": entry["gap"], "secs": record["window_secs"], "game_running": self.game.poll() is None}

    def live_restore(self, args):
        cps = self.session["checkpoints"]
        full = "--full-waits" in args
        args = [a for a in args if a != "--full-waits"]
        if len(args) != 1:
            raise LiveError("restore NAME [--full-waits]")
        cp = cps.get(args[0])
        if cp is None:
            raise LiveError(f"no checkpoint {args[0]!r}; there are {sorted(cps) or 'none'}")
        path, disk, keep = live_restore_plan(cp["path"], self.life)
        dropped = max(0, live_count_steps(self.session["path"]) - live_count_steps(path))
        self.session["path"] = path
        self.session["replay"] = {"checkpoint": cp["name"], "from_life": self.life, "disk": disk, "keep": keep, "full": full}
        self.session["restores"].append(
            {"checkpoint": cp["name"], "life": self.life, "at": round(time.time(), 1), "dropped": dropped, "disk": disk is not None}
        )
        return {
            "ok": True,
            "restoring": cp["name"],
            "disk": "kept" if disk else "launch",
            "replay_steps": live_count_steps(path[keep:]),
            "replay_secs": live_replay_secs(cp["path"], self.session),
        }

    def live_from_manifest_result(self, done, prefix, why=None, step=None):
        p = prefix["steps"]
        self.session["from_manifest"] = {
            "ok": why is None, "done": done, "of": len(p), "total": prefix["total"],
            "to_step": prefix["to_step"], "to_shot": prefix["to_shot"], "step": step, "why": why,
        }
        self.live_save()

    def live_from_manifest(self):
        """`live start --from-manifest` (#251), the first life only: the manifest's
        steps up to the chosen point, run as a `run` does, with the manifest's own
        checks (not_black, fills_window, expect_change, wait_settled). Each one
        goes on the path as the manifest wrote it, and a screenshot keeps its frame
        and stats as the frame a later restore matches. The first step that fails
        a check stops it: the path ends at the step before, and the session is
        ready there. (The failed check is told in status, not held against the
        session's summary: there is no run to fail.)"""
        prefix = self.live.get("prefix")
        if not prefix or self.session["from_manifest"] is not None:
            return
        steps = prefix["steps"]
        done = 0
        why = None
        failed_at = None
        for i, step in enumerate(steps, 1):
            self.busy = f"manifest step {i}/{len(steps)}"
            self.live_state("replaying", self.busy)
            before = len(self.checks)
            self.run_step(step)
            record = self.steps[-1]
            bad = [c for c in self.checks[before:] if not c["ok"]]
            if bad or not record["ok"]:
                why = "; ".join(f"{c['name']}: {c['detail']}" for c in bad) or record.get("why") or "no window for it"
                self.checks[before:] = [c for c in self.checks[before:] if c["ok"]]
                failed_at = i
                break
            entry = {"step": step, "gap": 0.0, "life": self.life, "manifest": True}
            if "screenshot" in step:
                name = step["screenshot"]
                time.sleep(LIVE_MOTION_SECS)
                again, _ = self.grab(os.path.join(self.run_dir, "motion.png"))
                image = self.shots.get(name)
                moving = None if again is None or image is None else changed_pct(image, again)
                if image is not None:
                    self.live_keep_ref(name, image)
                entry.update(image=record["path"], stats=record["stats"], moving_pct=moving)
                self.session["names"].append(name)
            elif "wait_settled" in step:
                entry["settled_secs"] = record.get("after_secs", 0.0)
            elif any(k in step for k in LIVE_UNTIL):
                entry["press_secs"] = record["secs"]
            self.session["path"].append(entry)
            done += 1
            self.live_save()
        self.live_from_manifest_result(done, prefix, why, failed_at)
        self.live_journal("from-manifest", [], dict(self.session["from_manifest"]))
        self.busy = None
        self.last_end = time.monotonic()

    def live_replay(self):
        """After a restore: the checkpoint's path again: from the launch, or on a
        kept disk (#252) from the relaunch after it, the path before that standing
        as it is. Each screenshot must match the one recorded (within match_pct,
        for up to LIVE_REPLAY_GRACE s more); the first that doesn't stops it, and
        the path ends there.

        By default a replay is fast (#257): a wait or a gap leading up to a
        screenshot ends once the screen has arrived there, a gap (the agent's
        thinking) after a matched screenshot is cut to LIVE_FAST_GAP, and frames
        don't go through a PNG. The path keeps every length as recorded, so the
        export is the same either way. If a fast replay fails, this may be why,
        so the next life replays it at the export's timing, whose verdict stands.
        With `restore --full-waits`, it's that from the start."""
        replay = self.session.get("replay")
        if not replay:
            return
        keep = replay.get("keep", 0)
        fast = not replay.get("full")
        entries = self.session["path"][keep:]
        whole = json.loads(json.dumps(self.session["path"]))
        self.session["path"] = self.session["path"][:keep]
        total = live_count_steps(entries)
        result = {
            "checkpoint": replay["checkpoint"], "life": self.life, "steps": total, "ok": True,
            "disk": "kept" if replay.get("disk") else "launch",
            "skipped": live_count_steps(self.session["path"]),
            "full_waits": not fast,
        }
        if replay.get("retry_of"):
            result["retried_because"] = replay["retry_of"]
        done = 0
        started = time.monotonic()
        timeline = result["timeline"] = []
        cut = 0.0
        shots = 0
        arriving = None  # the poll of the screenshot the waits lead up to
        last_frame = None  # the last matched frame, and the inputs since
        inputs = 0
        matched = False  # a matched screenshot, and no input after it yet
        for i, entry in enumerate(entries):
            if "mark" in entry:
                self.session["path"].append(entry)
                continue
            self.busy = f"replaying step {done + 1}/{total} to {replay['checkpoint']}"
            self.live_state("replaying", self.busy)
            step_at = time.monotonic()
            step = entry["step"]
            gap = live_kept_gap(entry, self.session["max_gap"])
            saved = 0.0
            target = live_wait_target(entries, i) if fast else None
            if target is not None:
                # The wait step's own length goes into the same poll.
                if arriving is None or arriving["at"] != target:
                    arriving = self.live_arrival(entries, i, target, last_frame, inputs)
                saved = self.live_wait_poll(arriving, gap + step.get("wait", 0.0))
            else:
                arriving = None
                if fast and matched and gap > LIVE_FAST_GAP:
                    saved = round(gap - LIVE_FAST_GAP, 1)
                    gap = LIVE_FAST_GAP
                if gap:
                    self.sleep(gap)
            cut += saved
            timeline.append({"step": live_step_label(step), "gap": gap})
            if saved:
                timeline[-1]["cut"] = round(saved, 1)
            if "screenshot" in step:
                why = self.live_replay_shot(entry, fast)
                if why:
                    result.update(ok=False, why=why)
                    break
                shots += 1
                last_frame, inputs, matched = self.shots.get(step["screenshot"]), 0, True
                arriving = None
            elif "expect_change" in step:
                # A manifest's check, made when it ran; the shots it names were
                # matched above.
                pass
            elif "wait_settled" in step:
                # Waits for the screen again, as long as this machine needs;
                # not a recorded sleep, and not a failed check of the run.
                if not self.settle_wait(step["wait_settled"])["settled"]:
                    result.update(ok=False, why=f"the screen didn't settle in {step['wait_settled']:.0f} s")
                    break
            elif any(k in step for k in LIVE_UNTIL):
                # As long as this life's screen needs, which is its point: a
                # limit reached stops the replay, as a frame that doesn't match does.
                self.run_step(step)
                timeline[-1]["presses"] = self.steps[-1]["presses"]
                if not self.steps[-1]["ok"]:
                    result.update(ok=False, why=f"{self.steps[-1]['action']}: {self.steps[-1]['why']}")
                    break
                if "press_until" in step:
                    inputs += 1
                    matched = False
            elif "relaunch" in step:
                # The route's own quit, again in this life. Its failure says why
                # here, and isn't a failed check of the session.
                before = len(self.checks)
                self.run_step(step)
                self.checks[before:] = [c for c in self.checks[before:] if c["ok"]]
                if not self.steps[-1]["ok"]:
                    result.update(ok=False, why=self.steps[-1]["why"])
                    break
                last_frame, inputs, matched = None, 0, False
            elif target is None or "wait" not in step:
                self.run_step(step)
                if any(k in step for k in LIVE_INPUTS):
                    inputs += 1
                    matched = False
            timeline[-1]["secs"] = round(time.monotonic() - step_at, 2)
            self.session["path"].append(entry)
            done += 1
            self.live_save()
        result["done"] = done
        result["cut_secs"] = round(cut, 1)
        result["secs"] = round(time.monotonic() - started, 1)
        self.session["replay"] = None
        self.session["last_replay"] = result
        self.live_save()
        if fast and not result["ok"] and (cut > 0 or shots > 0):
            # Anything past the first check ran ahead of the export's timing.
            restore = self.session["restores"][-1]
            self.session["path"] = whole
            self.session["replay"] = dict(replay, full=True, retry_of=result["why"])
            self.session["restores"].append(
                {"checkpoint": restore["checkpoint"], "life": self.life, "at": round(time.time(), 1),
                 "dropped": 0, "disk": restore["disk"], "retry": True}
            )
            self.live_save()
            raise LiveRetry()
        self.busy = None
        self.last_end = time.monotonic()

    def live_keep_ref(self, name, image):
        """A recorded frame, raw, in scratch: a replay reads it in no time, where
        a PNG takes it over a second to decode."""
        if image[2] != 3:
            return
        d = os.path.join(self.live["dir"], "refs")
        os.makedirs(d, exist_ok=True)
        with open(os.path.join(d, f"{name}.ppm"), "wb") as f:
            f.write(ppm_bytes(image))

    def live_load_ref(self, entry):
        """A screenshot's recorded frame, or None."""
        name = entry["step"]["screenshot"]
        try:
            with open(os.path.join(self.live["dir"], "refs", f"{name}.ppm"), "rb") as f:
                return parse_ppm(f.read())
        except (OSError, ValueError):
            pass
        try:
            return read_png(entry["image"])
        except (OSError, ValueError, KeyError):
            return None

    def live_arrival(self, entries, i, target, last_frame, inputs):
        """The poll for the screenshot at entries[target], which the waits from
        entries[i] on lead up to. Nothing to poll (a plain wait, then): a run of
        waits too short to gain from one, a frame that can't be compared, or one
        too like the screen before the input to tell the two apart."""
        arriving = {"at": target, "ref": None, "hits": 0, "done": False}
        entry = entries[target]
        allowed = live_arrived_allowance(entry, self.session["match_pct"])
        if allowed is None or live_wait_run_secs(entries, i, target, self.session["max_gap"]) < LIVE_POLL_MIN:
            return arriving
        ref = self.live_load_ref(entry)
        if ref is None:
            return arriving
        if last_frame is not None and inputs and changed_pct(last_frame, ref) <= allowed:
            return arriving
        arriving.update(ref=ref, allowed=allowed)
        return arriving

    def live_wait_poll(self, arriving, secs):
        """Wait up to `secs`, less once the screen has arrived: at the tight
        allowance in LIVE_ARRIVED_HITS frames running, so a frame in passing
        doesn't count. Returns the seconds it saved."""
        if arriving["done"]:
            return round(secs, 1)
        if arriving["ref"] is None:
            if secs:
                self.sleep(secs)
            return 0.0
        end = time.monotonic() + secs
        while True:
            polled = time.monotonic()
            if polled >= end:
                return 0.0
            image, _ = self.capture()
            if image is not None and changed_pct(image, arriving["ref"]) <= arriving["allowed"]:
                arriving["hits"] += 1
                if arriving["hits"] >= LIVE_ARRIVED_HITS:
                    arriving["done"] = True
                    return round(max(0.0, end - time.monotonic()), 1)
            else:
                arriving["hits"] = 0
            pause = min(end - time.monotonic(), LIVE_POLL_SECS - (time.monotonic() - polled))
            if pause > 0:
                self.sleep(pause)

    def live_replay_shot(self, entry, fast=False):
        """Retake a recorded screenshot until it matches; None, or why not. A late
        match lengthens the wait before it, for the next replay and the export."""
        name = entry["step"]["screenshot"]
        path = os.path.join(self.cfg["shots_dir"], f"{name}.life{self.life}.png")
        if fast:
            ref = self.live_load_ref(entry)
        else:
            try:
                ref = read_png(entry["image"])
            except (OSError, ValueError, KeyError):
                ref = None
        black = (entry.get("stats") or {}).get("nonblack_pct", 0) < MIN_NONBLACK_PCT
        allowed, loose = live_match_allowance(entry, self.session["match_pct"])
        first = time.monotonic()
        while True:
            at = time.monotonic()
            image, stats = self.grab_fast(path) if fast else self.grab(path)
            if image is None:
                return f"screenshot {name}: {stats}"
            if loose:
                # It moved too much to compare: a picture there is all it can show.
                pct = None
                if stats["nonblack_pct"] >= MIN_NONBLACK_PCT:
                    break
                why = f"{stats['nonblack_pct']}% non-black, and it was a moving picture"
            else:
                pct = None if ref is None or black else changed_pct(image, ref)
                if pct is None or pct <= allowed:
                    break
                why = f"{pct}% of pixels differ from the recorded one (want at most {allowed}%)"
            if at - first >= LIVE_REPLAY_GRACE:
                return (
                    f"screenshot {name}: {why} after {LIVE_REPLAY_GRACE:.0f} s more; "
                    f"recorded {entry['image']}, now {path}"
                )
            self.sleep(1.0)
        late = round(at - first, 1)
        if late > 0:
            entry["extra"] = round(entry.get("extra", 0) + late, 1)
        entry.update(replayed=path, match_pct=pct)
        self.shots[name] = image
        return None


# How long a key is held between press and release. A game that polls its input
# misses an instant press+release. (`click` already holds the button 100 ms between
# its swaymsg press and release.) And the pause between `type`'s characters.
KEY_HOLD_MS = 50
KEY_GAP_MS = 30
# Between a `scroll`'s notches: one at a time, like a wheel turned by hand.
SCROLL_GAP_MS = 40


def _msec():
    return int(time.monotonic() * 1000)


def _mount_fstype(path):
    found = None
    with open("/proc/self/mountinfo") as f:
        for line in f:
            fields = line.split()
            if fields[4] == path:
                found = fields[fields.index("-") + 1]
    return found


def _argv(pid):
    with open(f"/proc/{pid}/cmdline", "rb") as f:
        return f.read().decode(errors="replace").split("\0")


def _cmdlines():
    found = []
    for pid in filter(str.isdigit, os.listdir("/proc")):
        try:
            found.append(_argv(pid))
        except OSError:
            continue
    return found


def _proc_table():
    """(pid, comm, state) of every process this namespace sees."""
    table = []
    for pid in filter(str.isdigit, os.listdir("/proc")):
        try:
            with open(f"/proc/{pid}/stat") as f:
                text = f.read()
        except OSError:
            continue
        comm = text[text.index("(") + 1 : text.rindex(")")]
        table.append((int(pid), comm, text[text.rindex(")") + 2 :].split()[0]))
    return table


def leftovers(marks, run, pidns, exclude, proc="/proc"):
    """A run's processes still alive, as "pid comm" lines: an environment entry
    that is one of `marks`, `run` (the scratch path) on the command line, or the
    PID namespace `pidns`. Not those in `exclude`. One read per process, where a
    grep per mark takes seconds on a desktop's hundreds (#257)."""
    marks = {m.encode() for m in marks}
    run = run.encode()
    found = []
    for pid in os.listdir(proc):
        if not pid.isdigit() or pid in exclude:
            continue
        base = os.path.join(proc, pid)
        hit = False
        try:
            with open(os.path.join(base, "environ"), "rb") as f:
                hit = not marks.isdisjoint(f.read().split(b"\0"))
        except OSError:
            pass
        if not hit and pidns:
            try:
                hit = os.readlink(os.path.join(base, "ns/pid")) == pidns
            except OSError:
                pass
        if not hit:
            try:
                with open(os.path.join(base, "cmdline"), "rb") as f:
                    hit = run in f.read()
            except OSError:
                pass
        if not hit:
            continue
        try:
            with open(os.path.join(base, "comm"), errors="replace") as f:
                comm = f.read().strip()
        except OSError:
            comm = "?"
        found.append(f"{pid} {comm}")
    return found


def left_processes(table, harness):
    """What in the sandbox's process table isn't in `harness`, the pids that were
    up before the first launch: ["pid comm"]. All of it the game's, since the
    driver runs one tool at a time and asks between them. Zombies are gone."""
    return [f"{pid} {comm}" for pid, comm, state in sorted(table) if pid not in harness and state != "Z"]


def _device_nodes():
    nodes = []
    if os.path.isdir("/dev/dri"):
        nodes += [f"/dev/dri/{n}" for n in os.listdir("/dev/dri")]
    nodes += [f"/dev/{n}" for n in os.listdir("/dev") if n.startswith("nvidia")]
    return sorted(nodes)


# --- live sessions (#241) -------------------------------------------------------
#
# The path is what a fresh run would do to get where the session is: manifest
# steps, each with the gap of real time before it. A restore cuts it back to a
# checkpoint's; export writes it as a manifest. Replay and export time it the
# same way (live_kept_gap), so a replay that matches says the export's waits do.

# The driver's exit when a restore asked for the next life.
LIVE_RELAUNCH = 10
LIVE_SOCKET = "ctl.sock"
# A gap between two commands under this isn't kept: no wait step for it. Kept
# down to here because a game can drop keys pressed back to back (Cyberpunk's
# menu took Down, Up, Return 0.3 s apart, and is not asked to take them at once).
LIVE_GAP_MIN = 0.2
# Gaps over this are cut to it: the rest was the agent thinking, not the game.
LIVE_MAX_GAP = 10.0
# A replayed screenshot matches the recorded one when at most this % of pixels
# differ (an animated menu differs some), within LIVE_REPLAY_GRACE s more.
LIVE_MATCH_PCT = 15.0
LIVE_REPLAY_GRACE = 30.0
# A fast replay (#257). A run of waits this short has nothing to gain from a
# poll, which looks this often. The screen has arrived at a recorded frame when
# this many frames running are within its own motion, times LIVE_ARRIVED_MOTION,
# plus LIVE_ARRIVED_PCT: much tighter than match_pct, which takes a screen one
# step short of it (Cyberpunk's agreement, scrolled to its end, is 9% off).
LIVE_POLL_MIN = 3.0
LIVE_POLL_SECS = 1.0
LIVE_ARRIVED_HITS = 2
LIVE_ARRIVED_MOTION = 1.5
LIVE_ARRIVED_PCT = 1.0
# A gap after a matched screenshot is thinking: the screen is where it was then.
LIVE_FAST_GAP = 1.0
# A live screenshot takes a second frame this much later: how much the screen
# moves by itself, which a replay allows (live_match_allowance).
LIVE_MOTION_SECS = 0.5
# The export's expect_change: after an input that changed at least this %, a
# check of half of what it changed, at most LIVE_EXPECT_CAP.
LIVE_EXPECT_MIN_PCT = 2.0
LIVE_EXPECT_CAP = 20.0
# What a screenshot takes (grim, decode, stats), for a replay's estimate.
LIVE_SHOT_SECS = 2.5
# `settle` gives up after this long without --max: a static menu settles in a
# few polls; an animated one never will, and this is what finding out costs.
LIVE_SETTLE_MAX = 30.0
LIVE_SETTLE_CAP = 600.0
# A clean-stop checkpoint's request to the supervisor, and its answer, in the
# socket's dir; and how long a copy of the disk may take (a full one, on ext4).
LIVE_DISK_REQUEST = "keep-disk.request"
LIVE_DISK_REPLY = "keep-disk.reply"
LIVE_DISK_SECS = 900.0
LIVE_INPUTS = ("key", "type", "hold", "press_until", "click", "click_here", "move_to", "move_by", "scroll")
LIVE_STEP_CMDS = ("screenshot", "wait", "wait_until", "key", "type", "hold", "press_until", "click", "move_to", "move_by", "scroll")
LIVE_UNTIL = ("press_until", "wait_until")


class LiveError(Exception):
    """A live command that can't run; the session goes on."""


class LiveRetry(Exception):
    """A fast replay failed: this life ends, and the next replays at full timing."""


class LiveStop(Exception):
    """`stop` came while the session was busy."""


def write_json(path, data):
    tmp = f"{path}.tmp{os.getpid()}"
    with open(tmp, "w") as f:
        json.dump(data, f, indent=1)
    os.replace(tmp, path)


def live_load_session(path, manifest, live):
    """The session's state, kept in the results dir across lives; a new one the first time."""
    if os.path.exists(path):
        with open(path) as f:
            return json.load(f)
    session = {
        "id": live["id"],
        # Its own copy: the driver's own has its [exit] changed at a stop.
        "manifest": json.loads(json.dumps(manifest)),
        "max_gap": live["max_gap"],
        "match_pct": live["match_pct"],
        "path": [],
        "checkpoints": {},
        "names": [],
        "restores": [],
        "seq": 0,
        "replay": None,
        "last_replay": None,
        "from_manifest": None,
        "game_exit": None,
        "ended": None,
    }
    write_json(path, session)
    return session


def live_from_manifest(text, to_step=None, to_shot=None):
    """`live start --from-manifest` (#251): the steps of a manifest a session
    runs before it hands over, up to a point. `--to-step N` runs steps 1..N
    (every [[step]] counts: a wait, an input, a screenshot, an expect_change).
    `--to-shot NAME` runs through that screenshot and the expect_change checks
    right after it; with neither, through the manifest's last screenshot.
    Raises ManifestError (a usage error) for a point that isn't there."""
    steps = parse_manifest(text)["steps"]
    shots = [s["screenshot"] for s in steps if "screenshot" in s]
    if to_step is not None and to_shot is not None:
        raise ManifestError("--to-step and --to-shot are two ways to say where: use one")
    if to_step is not None:
        if not re.fullmatch(r"[0-9]+", str(to_step)) or not 1 <= int(to_step) <= len(steps):
            raise ManifestError(f"--to-step {to_step}: the manifest has {len(steps)} steps, 1 to {len(steps)}")
        upto = int(to_step)
    else:
        if to_shot is None:
            if not shots:
                raise ManifestError("no screenshot to stop at; give --to-step N")
            to_shot = shots[-1]
        if to_shot not in shots:
            raise ManifestError(f"--to-shot {to_shot}: no such screenshot; the manifest's are {shots or 'none'}")
        upto = next(i for i, s in enumerate(steps) if s.get("screenshot") == to_shot) + 1
        while upto < len(steps) and "expect_change" in steps[upto]:
            upto += 1
    return {"steps": steps[:upto], "to_step": upto, "total": len(steps), "to_shot": to_shot}


def live_kept_gap(entry, max_gap):
    """The wait a replay or an export keeps before a path entry."""
    gap = min(entry.get("gap", 0.0), max_gap)
    if gap < LIVE_GAP_MIN:
        gap = 0.0
    return round(gap + entry.get("extra", 0.0), 1)


def live_arrived_allowance(entry, match_pct):
    """How close a frame must be for a wait before this screenshot to end early;
    None when it can't say: a moving picture, or one recorded black."""
    allowed, loose = live_match_allowance(entry, match_pct)
    if loose or (entry.get("stats") or {}).get("nonblack_pct", 0) < MIN_NONBLACK_PCT:
        return None
    return min(allowed, round((entry.get("moving_pct") or 0.0) * LIVE_ARRIVED_MOTION + LIVE_ARRIVED_PCT, 2))


def live_wait_target(entries, i):
    """The index of the screenshot a wait at entries[i] leads up to (i itself, for
    a screenshot's own gap), or None: an input or anything else comes first, and
    there's no frame to say when it's over."""
    for j in range(i, len(entries)):
        step = entries[j].get("step")
        if step is None:
            continue
        if "screenshot" in step:
            return j
        if "wait" not in step:
            return None
    return None


def live_wait_run_secs(entries, i, target, max_gap):
    """How long entries[i..target] wait, gaps and all."""
    secs = 0.0
    for e in entries[i : target + 1]:
        if "step" in e:
            secs += live_kept_gap(e, max_gap) + e["step"].get("wait", 0.0)
    return secs


def live_match_allowance(entry, match_pct):
    """How different a replayed screenshot may be: (pct, loose). A frame that
    moved by itself at least match_pct in LIVE_MOTION_SECS (a video, a scene
    cut) is loose: only a picture there is checked. Otherwise its own motion
    is allowed on top."""
    moving = entry.get("moving_pct") or 0.0
    if moving >= match_pct:
        return None, True
    return round(match_pct + moving, 2), False


def live_settle_max(args):
    """`settle [--max SECS]`: the seconds to give it."""
    if not args:
        return LIVE_SETTLE_MAX
    if len(args) != 2 or args[0] != "--max":
        raise LiveError("usage: settle [--max SECS]")
    try:
        secs = float(args[1])
    except ValueError:
        raise LiveError(f"settle: --max must be a number, got {args[1]!r}") from None
    if not 0 < secs <= LIVE_SETTLE_CAP:
        raise LiveError(f"settle: --max must be over 0 and at most {LIVE_SETTLE_CAP:.0f} s")
    return secs


def live_settle_timeout(took):
    """The wait_settled an export writes for a settle that took `took` s: twice
    that and 10 s more, whole seconds, so a machine twice as slow still passes."""
    return float(math.ceil(2 * took + 10))


def live_restore_phases(restored, between, phases, started):
    """A restore up to the launch, phase by phase (#257): the closed game's life
    ending, the supervisor's part between the lives (`between`, its clock is
    this one's), and the new sandbox's own `phases`."""
    out = {"close": restored.get("close_secs")}
    between = between or {}
    points = [restored.get("closed_at")] + [between.get(k) for k in ("gone", "swept", "rolled")] + [started]
    names = ("leave", "sweep", "rollback", "start")
    for name, a, b in zip(names, points, points[1:]):
        if a is not None and b is not None:
            out[name] = round(b - a, 2)
    out.update(phases)
    return out


def live_step_label(step):
    """A path step, in a word or two, for a replay's timeline."""
    if "press_until" in step:
        return f"press_until {step['press_until']['key']}"
    if "wait_until" in step:
        return "wait_until"
    for key in ("wait", "wait_settled", "screenshot", "key", "scroll"):
        if key in step:
            return f"{key} {step[key]}"
    return next(iter(step), "?")


def live_count_steps(path):
    return sum(1 for e in path if "step" in e)


def live_restore_plan(path, life):
    """What a restore to a checkpoint's `path` does (#252): (path, disk, keep).
    `disk`: the last disk a clean stop kept on the path, or None: the launch's,
    with everything to replay. On a kept disk the next life's launch is the
    relaunch after it, so the replay starts behind that: the path's first `keep`
    entries stand. A checkpoint with no relaunch after it yet gets one."""
    path = json.loads(json.dumps(path))
    at = next((i for i in range(len(path) - 1, -1, -1) if path[i].get("disk")), None)
    if at is None:
        return path, None, 0
    disk = path[at]["disk"]
    for i in range(at + 1, len(path)):
        if "relaunch" in path[i].get("step", {}):
            return path, disk, i + 1
    path.append({"step": {"relaunch": True}, "gap": 0.0, "life": life})
    return path, disk, len(path)


def live_replay_secs(path, session):
    """About how long a replay of `path` takes after the window is up."""
    secs = 0.0
    _, disk, keep = live_restore_plan(path, 0)
    for e in path[keep:] if disk else path:
        if "step" not in e:
            continue
        secs += e.get("relaunch_secs", 0.0)
        secs += live_kept_gap(e, session["max_gap"])
        step = e["step"]
        if "wait" in step:
            secs += step["wait"]
        elif "wait_settled" in step:
            secs += e.get("settled_secs", 0.0)
        elif "screenshot" in step:
            secs += LIVE_SHOT_SECS
        elif "hold" in step:
            secs += step["hold"]["ms"] / 1000
        elif "press_until" in step or "wait_until" in step:
            secs += e.get("press_secs", 0.0)
    return round(secs)


def live_last_shot(path):
    """The last screenshot on the path, and how many inputs came after it. None
    past a relaunch: another launch's screen says nothing of what an input did."""
    inputs = 0
    for e in reversed(path):
        step = e.get("step", {})
        if "relaunch" in step:
            return None, 0
        if "screenshot" in step:
            return step["screenshot"], inputs
        if any(k in step for k in LIVE_INPUTS):
            inputs += 1
    return None, inputs


def live_unique_name(want, taken):
    """A screenshot name not used in the session yet: `want`, or want-2, -3..."""
    if want not in taken:
        return want
    n = 2
    while f"{want}-{n}" in taken:
        n += 1
    return f"{want}-{n}"


def _live_coord(text, origin, size, what):
    """A live coordinate as a percent of the window: `N` pixels of the output
    (as read off a screenshot), or `N%` of the window."""
    try:
        if text.endswith("%"):
            return float(text[:-1])
        if size is None:
            raise LiveError(f"{what}: no window to aim at")
        return 100.0 * (float(text) - origin) / size
    except ValueError:
        raise LiveError(f"{what}: want pixels (N) or a percent of the window (N%), got {text!r}") from None


def live_raw_step(cmd, args, rect):
    """A live command's words as a raw manifest step, for _step() to check.
    `rect`: the window's, for pixel coordinates."""

    def want(n, usage):
        if len(args) not in n:
            raise LiveError(f"usage: {cmd} {usage}")

    def coords():
        x0, y0, w, h = (rect["x"], rect["y"], rect["width"], rect["height"]) if rect else (0, 0, None, None)
        point = {"x": _live_coord(args[0], x0, w, cmd), "y": _live_coord(args[1], y0, h, cmd)}
        for k, v in point.items():
            if not 0 <= v <= 100:
                raise LiveError(f"{cmd}: {k} is outside the window ({v}% of it)")
        return point

    def number(text, what):
        try:
            return float(text) if "." in text or "e" in text.lower() else int(text)
        except ValueError:
            raise LiveError(f"{cmd}: {what} must be a number, got {text!r}") from None

    if cmd == "screenshot":
        want((0, 1), "[NAME]")
        return {"screenshot": args[0] if args else "shot"}
    if cmd == "wait":
        want((1,), "SECS")
        return {"wait": number(args[0], "SECS")}
    if cmd == "key":
        want((1,), "KEYSYM")
        return {"key": args[0]}
    if cmd == "type":
        want((1,), "TEXT")
        return {"type": args[0]}
    if cmd == "hold":
        want((2,), "KEYSYM MS")
        return {"hold": {"key": args[0], "ms": number(args[1], "MS")}}
    if cmd == "click":
        want((0, 2), "[X Y]")
        return {"click": coords()} if args else {"click_here": True}
    if cmd == "move_to":
        want((2,), "X Y")
        return {"move_to": coords()}
    if cmd == "move_by":
        want((2,), "DX DY")
        return {"move_by": {"x": number(args[0], "DX"), "y": number(args[1], "DY")}}
    if cmd == "scroll":
        want((1,), "NOTCHES")
        return {"scroll": number(args[0], "NOTCHES")}
    if cmd == "press_until":
        return {"press_until": live_press_args(args, number)}
    if cmd == "wait_until":
        raw = live_press_args(["space"] + args, number)
        del raw["key"]
        return {"wait_until": raw}
    raise LiveError(f"unknown command {cmd!r}")


def live_press_args(args, number):
    """`press_until KEY --every S --max N`, and `--change PCT` or `--marked X Y W H
    COLOR MIN [TOL]`, as a raw step's table, for _step() to check."""
    usage = "usage: press_until KEY --every SECS --max PRESSES (--change PCT | --marked X Y W H COLOR MIN [TOL])"
    if not args or args[0].startswith("--"):
        raise LiveError(usage)
    raw = {"key": args[0]}
    rest = args[1:]
    while rest:
        opt = rest.pop(0)
        if opt in ("--every", "--max", "--change"):
            if not rest:
                raise LiveError(usage)
            raw[opt[2:]] = number(rest.pop(0), opt)
        elif opt == "--marked":
            if len(rest) < 6:
                raise LiveError(usage)
            box = [number(rest.pop(0), f"--marked {k}") for k in "xywh"]
            raw["marked"] = dict(zip("xywh", box), color=rest.pop(0), min=number(rest.pop(0), "--marked MIN"))
            if rest and not rest[0].startswith("--"):
                raw["marked"]["tol"] = number(rest.pop(0), "--marked TOL")
        else:
            raise LiveError(usage)
    return raw


def _toml(value):
    """A TOML value: strings as basic strings (JSON's escapes are TOML's too),
    numbers without a needless .0, inline tables."""
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (int, float)):
        value = round(float(value), 2)
        return str(int(value)) if value == int(value) else repr(value)
    if isinstance(value, str):
        return json.dumps(value)
    if isinstance(value, list):
        return "[" + ", ".join(_toml(v) for v in value) + "]"
    if isinstance(value, dict):
        return "{ " + ", ".join(f"{k} = {_toml(v)}" for k, v in value.items()) + " }"
    raise TypeError(f"no TOML for {value!r}")


def live_export(session):
    """The session's path as a manifest (TOML text): the route that's left after
    the restores, with the gaps it was driven at (cut to max_gap), and an
    expect_change after each input that changed the screen. Black screenshots
    are dropped: a fresh run's not_black would fail on them."""
    m = session["manifest"]
    max_gap = session["max_gap"]
    out = [
        f"# Exported from live session {session['id']} (scripts/linux-e2e.sh live, #241).",
        f"# Gaps between commands are kept up to {_toml(max_gap)} s; "
        f"{len(session['restores'])} restore(s), their dead ends dropped.",
    ]
    fm = session.get("from_manifest")
    if fm and fm.get("done"):
        out.append(
            f"# Steps 1-{fm['done']} of {fm['total']} are the starting manifest's own (--from-manifest); "
            "the rest of its steps are not here."
        )
    out += [
        "# Prove it with a fresh `run` before it replaces a manifest.",
        f"title = {_toml(m['title'])}",
    ]
    if m.get("gogid") is not None:
        out.append(f"gogid = {m['gogid']}")
    else:
        if m.get("build_command"):
            out.append(f"build_command = {_toml(m['build_command'])}")
        out.append(f"command = {_toml(m['command'])}")
    w = m["window"]
    out += ["", "[window]", f"wait_secs = {_toml(w['wait_secs'])}", f'resolution = "{w["width"]}x{w["height"]}"']
    if w.get("title"):
        out.append(f"title = {_toml(w['title'])}")
    if w["min_fill"] != DEFAULT_MIN_FILL:
        out.append(f"min_fill = {_toml(w['min_fill'])}")
    if w.get("display", "xwayland") != "xwayland":
        out.append(f"display = {_toml(w['display'])}")
    if (m.get("gpu") or {}).get("wayland", "used") != "used":
        out += ["", "[gpu]", f"wayland = {_toml(m['gpu']['wayland'])}"]
    if m.get("env"):
        out += ["", "[env]"] + [f"{k} = {_toml(v)}" for k, v in m["env"].items()]
    o = m.get("override") or {}
    if o.get("env") or o.get("extra_args") or o.get("wrapper") or o.get("display") or o.get("hdr") or o.get("monitor"):
        out += ["", "[override]"]
        if o.get("extra_args"):
            out.append(f"extra_args = {_toml(o['extra_args'])}")
        if o.get("wrapper"):
            out.append(f"wrapper = {_toml(o['wrapper'])}")
        for k in ("display", "hdr", "monitor"):
            if o.get(k):
                out.append(f"{k} = {_toml(o[k])}")
        if o.get("env"):
            out += ["[override.env]"] + [f"{k} = {_toml(v)}" for k, v in o["env"].items()]
    # Quit from its own menu: it exits by itself, which the export keeps.
    ended = session.get("ended") or {}
    if not ended and session.get("game_exit"):
        ended = {"how": "self", "code": session["game_exit"]["code"]}
    # A relaunch step's quit goes by these too, whichever way the last one ends.
    relaunches = any("relaunch" in e.get("step", {}) for e in session["path"])
    out += ["", "[exit]"]
    if ended.get("how") == "self":
        out += ['how = "self"', f"wait_secs = {_toml(max(m['exit']['wait_secs'], 60))}"]
        if ended.get("code"):
            out.append(f"code = {ended['code']}")
    else:
        out += ['how = "signal"']
        if relaunches:
            out.append(f"wait_secs = {_toml(max(m['exit']['wait_secs'], 60))}")
            if m["exit"]["code"]:
                out.append(f"code = {m['exit']['code']}")

    steps = []
    comments = []
    wait = 0.0
    last_shot = None
    inputs = 0

    def emit(step):
        nonlocal wait, comments
        if wait >= LIVE_GAP_MIN:
            steps.append((comments, {"wait": round(wait, 1)}))
            comments = []
        wait = 0.0
        steps.append((comments, step))
        comments = []

    for e in session["path"]:
        if "mark" in e:
            kept = ": a clean stop, its disk kept" if e.get("disk") else ""
            comments.append(f"# checkpoint {e['name']!r}{kept}")
            continue
        wait += live_kept_gap(e, max_gap)
        step = e["step"]
        if "relaunch" in step:
            emit(step)
            last_shot, inputs = None, 0
            continue
        if e.get("manifest"):
            # A step of the manifest the session started from: as written, with
            # its own checks; the gaps a replay added are all that's kept.
            if wait >= LIVE_GAP_MIN:
                steps.append((comments, {"wait": round(wait, 1)}))
                comments = []
            wait = 0.0
            steps.append((comments, step))
            comments = []
            if "screenshot" in step:
                last_shot, inputs = step["screenshot"], 0
            elif any(k in step for k in LIVE_INPUTS):
                inputs += 1
            continue
        if "wait" in step:
            wait += step["wait"]
            if e.get("unsettled"):
                comments.append(f"# settle: the screen never settled; waited {_toml(step['wait'])} s instead")
            continue
        if "screenshot" in step:
            name = step["screenshot"]
            stats = e.get("stats") or {}
            if stats.get("nonblack_pct", 0) < MIN_NONBLACK_PCT:
                wait += LIVE_SHOT_SECS  # what grabbing it took
                continue
            shot = {"screenshot": name}
            fill = w["min_fill"]
            if stats.get("box_w_pct", 0) < fill["w"] or stats.get("box_h_pct", 0) < fill["h"]:
                shot["min_fill"] = False
            emit(shot)
            change = e.get("change_pct")
            if last_shot and e.get("since") == last_shot and inputs and change is not None and change >= LIVE_EXPECT_MIN_PCT:
                floor = int(min(change / 2, LIVE_EXPECT_CAP))
                steps.append(([], {"expect_change": {"from": last_shot, "to": name, "min": max(1, floor)}}))
            last_shot, inputs = name, 0
            continue
        emit(step)
        if any(k in step for k in LIVE_INPUTS):
            inputs += 1
    if not steps:
        emit({"wait": round(max(wait, 1.0), 1)})
    for notes, step in steps:
        out.append("")
        out += notes
        out.append("[[step]]")
        for k, v in step.items():
            out.append(f"{k} = {_toml(v)}")
    text = "\n".join(out) + "\n"
    parse_manifest(text)  # never write one the harness would refuse
    return text


def live_call(sock_dir, request, timeout):
    """One request to a live session's socket: its reply. The socket must be ours."""
    path = os.path.join(sock_dir, LIVE_SOCKET)
    st = os.lstat(path)
    if not stat.S_ISSOCK(st.st_mode) or st.st_uid != os.getuid():
        raise OSError(f"{path}: not our socket")
    fd = os.open(sock_dir, os.O_PATH | os.O_DIRECTORY)
    conn = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        conn.settimeout(timeout)
        conn.connect(f"/proc/self/fd/{fd}/{LIVE_SOCKET}")
        conn.sendall((json.dumps(request) + "\n").encode())
        data = b""
        while not data.endswith(b"\n"):
            chunk = conn.recv(65536)
            if not chunk:
                break
            data += chunk
    finally:
        conn.close()
        os.close(fd)
    return json.loads(data)


def live_registered_dir(reg):
    """The socket's dir from a registry entry's run=, once the session has one."""
    try:
        with open(reg) as f:
            for line in f:
                if line.startswith("run=") and line.strip() != "run=":
                    return os.path.join(line[4:].strip(), "live")
    except OSError:
        pass
    return ""


def live_wait(state_file, sock_dir, secs, owner, min_life, reg=""):
    """Until the session is ready in life `min_life` or later, or ended, or
    `secs` pass: (exit, reply). 0 ready, 1 ended, 4 still busy. A session
    still starting may not have a scratch yet: with no `sock_dir`, it's
    read from the registry entry `reg` as the session registers one."""
    deadline = time.monotonic() + secs
    state = {}
    why = ""
    while True:
        try:
            with open(state_file) as f:
                state = json.load(f)
        except (OSError, ValueError):
            state = {}
        alive = _pid_alive(owner, os.environ.get("GOGO_E2E_LIVE_START", ""))
        if not sock_dir and reg:
            sock_dir = live_registered_dir(reg)
        if state.get("state") == "ready" and state.get("life", 0) >= min_life and alive:
            try:
                reply = live_call(sock_dir, {"cmd": "status", "args": []}, 20)
                if reply.get("state") == "ready":
                    return 0, reply
                why = f"the socket answered {json.dumps(reply)[:300]}"
            except (OSError, ValueError) as e:
                why = f"the socket: {e}"
        if not alive:
            return 1, {"ok": False, "state": "ended", "last": state}
        if time.monotonic() >= deadline:
            return 4, {"ok": False, "state": state.get("state", "starting"), "busy": True,
                       "detail": state.get("detail", ""), "life": state.get("life"),
                       "why": why}
        time.sleep(1)


def _pid_alive(pid, start=""):
    """Whether pid runs, and with `start` (/proc/PID/stat field 22, as the
    registry keeps it) whether it is still that process, not a reuse."""
    try:
        os.kill(int(pid), 0)
    except (OSError, ValueError):
        return False
    if not start:
        return True
    try:
        with open(f"/proc/{int(pid)}/stat") as f:
            return f.read().rsplit(")", 1)[1].split()[19] == start
    except (OSError, IndexError, ValueError):
        return False


# --- installed bases (#240) ---------------------------------------------------


def _under(path, root):
    return path == root or path.startswith(root.rstrip("/") + "/")


def tree_problems(tree, run, guards, uid):
    """Why an installed base at `tree` mustn't be copied into the run at `run`.

    The tree is what the build left in `run` (its `gogo`, `home`, `xdg`), and a
    reused run sees it at that same path. Every entry must be `uid`'s, and no
    symlink may lead, from `run`, into a guard (the owner's home, the harness's
    root) but inside `run`: such a link would let a run write into the base
    or out of scratch. Links elsewhere (`z:` to `/`, the Nix store) see only
    the sandbox's whitelist, as in a fresh run. Empty: it may be reused.
    """
    problems = []
    tree, run = os.path.normpath(tree), os.path.normpath(run)
    top = os.lstat(tree)
    if os.path.islink(tree) or not os.path.isdir(tree):
        return [f"{tree}: not a directory"]
    if top.st_uid != uid:
        return [f"{tree}: not ours (uid {top.st_uid})"]
    for here, dirs, files in os.walk(tree):
        for name in dirs + files:
            path = os.path.join(here, name)
            st = os.lstat(path)
            if st.st_uid != uid:
                problems.append(f"{path}: not ours (uid {st.st_uid})")
            if not os.path.islink(path):
                continue
            seen_at = os.path.join(run, os.path.relpath(path, tree))
            target = os.path.normpath(
                os.path.join(os.path.dirname(seen_at), os.readlink(path))
            )
            if any(_under(target, g) for g in guards) and not _under(target, run):
                problems.append(f"{seen_at}: a link out of the run, to {target}")
        if len(problems) > 20:
            break
    return problems


# --- btrfs subvolumes (#255) ----------------------------------------------------
#
# The two ioctls the harness needs, unprivileged on any kernel it runs on, so
# btrfs-progs needn't be in the tools (whose store path is part of every
# installed base's key). Both take a struct btrfs_ioctl_vol_args: an fd and a
# name, 4096 bytes, on the new entry's parent dir. Deleting one needs no ioctl:
# `rm -rf` empties it and rmdir(2) takes an empty subvolume (kernel 4.18+),
# where BTRFS_IOC_SNAP_DESTROY would want root or user_subvol_rm_allowed.

BTRFS_IOCTL_MAGIC = 0x94
BTRFS_VOL_ARGS = struct.Struct("q4088s")


def _btrfs_iow(nr, size):
    return (1 << 30) | (size << 16) | (BTRFS_IOCTL_MAGIC << 8) | nr


BTRFS_IOC_SNAP_CREATE = _btrfs_iow(1, BTRFS_VOL_ARGS.size)
BTRFS_IOC_SUBVOL_CREATE = _btrfs_iow(14, BTRFS_VOL_ARGS.size)


def _subvol_ioctl(request, path, fd):
    parent, name = os.path.split(os.path.abspath(path))
    raw = os.fsencode(name)
    if not raw or raw in (b".", b"..") or len(raw) > 255:
        raise OSError(f"not a name for a subvolume: {name!r}")
    # A bytearray: fcntl.ioctl refuses an immutable argument over 1024 bytes.
    args = bytearray(BTRFS_VOL_ARGS.pack(fd, raw))
    pfd = os.open(parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        fcntl.ioctl(pfd, request, args)
    finally:
        os.close(pfd)


def subvol_create(path):
    _subvol_ioctl(BTRFS_IOC_SUBVOL_CREATE, path, 0)


def subvol_snapshot(src, path):
    sfd = os.open(src, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        _subvol_ioctl(BTRFS_IOC_SNAP_CREATE, path, sfd)
    finally:
        os.close(sfd)


# --- CLI ----------------------------------------------------------------------


def main(argv):
    # What to look for comes on stdin (the namespace, the scratch path, then one
    # mark a line): with a scratch path in argv, this would find itself.
    if len(argv) >= 2 and argv[1] == "leftovers":
        pidns, run, *marks = sys.stdin.read().split("\n")
        exclude = set(argv[2:]) | {str(os.getpid())}
        for line in leftovers([m for m in marks if m], run, pidns, exclude):
            print(line)
        return 0
    if len(argv) in (3, 4) and argv[1] == "manifest" and (len(argv) == 3 or argv[2] == "--live"):
        path = argv[-1]
        try:
            with open(path, encoding="utf-8") as f:
                print(json.dumps(parse_manifest(f.read(), live=len(argv) == 4)))
        except (OSError, ManifestError) as e:
            print(f"{path}: {e}", file=sys.stderr)
            return 2
        return 0
    # The session's dir comes in GOGO_E2E_LIVE_DIR, not argv: the session's sweep
    # takes a process with its scratch path on the command line for its own.
    if len(argv) >= 4 and argv[1] == "live-call":
        try:
            sock_dir = os.environ["GOGO_E2E_LIVE_DIR"]
            reply = live_call(sock_dir, {"cmd": argv[3], "args": argv[4:]}, float(argv[2]))
        except (OSError, ValueError, KeyError) as e:
            print(json.dumps({"ok": False, "error": f"the session doesn't answer: {e}"}))
            return 3
        print(json.dumps(reply, indent=1))
        return 0 if reply.get("ok") else 1
    if len(argv) == 6 and argv[1] == "live-wait":
        code, reply = live_wait(
            argv[2], os.environ.get("GOGO_E2E_LIVE_DIR", ""), float(argv[3]), argv[4], int(argv[5]),
            os.environ.get("GOGO_E2E_LIVE_REG", ""),
        )
        print(json.dumps(reply, indent=1))
        return code
    if len(argv) in (3, 5) and argv[1] == "live-from-manifest":
        try:
            with open(argv[2], encoding="utf-8") as f:
                text = f.read()
            pick = {argv[3]: argv[4]} if len(argv) == 5 else {}
            if set(pick) - {"--to-step", "--to-shot"}:
                raise ManifestError(f"unknown option {argv[3]}")
            print(json.dumps(live_from_manifest(text, pick.get("--to-step"), pick.get("--to-shot"))))
        except (OSError, ManifestError) as e:
            print(f"{argv[2]}: {e}", file=sys.stderr)
            return 2
        return 0
    if len(argv) == 3 and argv[1] == "live-export":
        try:
            with open(argv[2]) as f:
                print(live_export(json.load(f)), end="")
        except (OSError, ValueError, ManifestError) as e:
            print(f"{argv[2]}: {e}", file=sys.stderr)
            return 1
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
            driver = Driver(json.load(f))
        if driver.building:
            return driver.main_build()
        return driver.main_live() if driver.live else driver.main()
    if len(argv) >= 5 and argv[1] == "tree-check":
        problems = tree_problems(argv[2], argv[3], argv[4:], os.getuid())
        for p in problems:
            print(p)
        return 1 if problems else 0
    if (len(argv) == 4 and argv[1:3] == ["subvol", "create"]) or (
        len(argv) == 5 and argv[1:3] == ["subvol", "snapshot"]
    ):
        try:
            if argv[2] == "create":
                subvol_create(argv[3])
            else:
                subvol_snapshot(argv[3], argv[4])
        except OSError as e:
            print(f"subvol {argv[2]}: {e}", file=sys.stderr)
            return 1
        return 0
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
