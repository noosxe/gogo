#!/usr/bin/env python3
"""The self-test's Wayland-native client (scripts/linux-e2e.sh self-test, #264).

A plain xdg-shell window, speaking the wire protocol itself so the harness's
tools need nothing new, that only passes its steps if input really reaches a
Wayland client with no Xwayland in between:

  white  waiting for a click
  blue   clicked: it asks for a pointer lock and relative motion, as a game
         that steers a camera by the mouse (Wine's Wayland driver) does
  green  at least MOVE_PX of relative motion arrived while locked
  exit   0 on `q`, 1 on any other key first

What it saw goes to stdout (the run's game.out): `locked`, `relative dx dy`.
Run with `python3 -I`; it reads WAYLAND_DISPLAY and XDG_RUNTIME_DIR only.
"""

import mmap
import os
import socket
import struct
import sys

TITLE = "self-test: wayland"
WHITE, BLUE, GREEN = (0xFF, 0xFF, 0xFF), (0x20, 0x60, 0xC0), (0x20, 0xA0, 0x40)
MOVE_PX = 100
BTN_LEFT = 0x110
KEY_Q = 16  # evdev
XRGB8888 = 1
LOCK_PERSISTENT = 2


def string(text):
    data = text.encode() + b"\0"
    return struct.pack("<I", len(data)) + data + b"\0" * (-len(data) % 4)


def fixed(raw):
    return struct.unpack("<i", raw)[0] / 256.0


class Client:
    def __init__(self, path):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(path)
        self.next_id = 2
        self.buf = b""
        self.handlers = {}
        self.globals = {}
        self.registry = None

    def new_id(self, handler=None):
        oid = self.next_id
        self.next_id += 1
        if handler:
            self.handlers[oid] = handler
        return oid

    def send(self, obj, opcode, body=b"", fds=()):
        msg = struct.pack("<II", obj, ((8 + len(body)) << 16) | opcode) + body
        if fds:
            socket.send_fds(self.sock, [msg], list(fds))
        else:
            self.sock.sendall(msg)

    def bind(self, iface, version, handler=None):
        name, have = self.globals[iface]
        oid = self.new_id(handler)
        self.send(self.registry, 0, struct.pack("<I", name) + string(iface) + struct.pack("<II", min(version, have), oid))
        return oid

    def dispatch(self):
        """Read once, handle every whole message."""
        data, fds, _, _ = socket.recv_fds(self.sock, 65536, 16)
        for fd in fds:  # only wl_keyboard.keymap sends one, and it isn't needed
            os.close(fd)
        if not data:
            raise SystemExit("the compositor closed the connection")
        self.buf += data
        while len(self.buf) >= 8:
            obj, word = struct.unpack_from("<II", self.buf)
            size, opcode = word >> 16, word & 0xFFFF
            if len(self.buf) < size:
                break
            body, self.buf = self.buf[8:size], self.buf[size:]
            if obj == 1 and opcode == 0:
                _, code = struct.unpack_from("<II", body)
                raise SystemExit(f"protocol error {code}: {body[12:].split(b'\0')[0].decode()}")
            handler = self.handlers.get(obj)
            if handler:
                handler(opcode, body)

    def roundtrip(self):
        done = []
        cb = self.new_id(lambda op, body: done.append(True))
        self.send(1, 0, struct.pack("<I", cb))
        while not done:
            self.dispatch()


class Window:
    def __init__(self, wl):
        self.wl = wl
        self.size = (640, 480)
        self.pending = None
        self.colour = WHITE
        self.buffer = None
        self.moved = 0.0
        self.locked = False
        self.lock = None
        self.pointer = None
        self.keyboard = None
        self.exit = None

        wl.registry = wl.new_id(self.on_registry)
        wl.send(1, 1, struct.pack("<I", wl.registry))  # get_registry
        wl.roundtrip()
        for need in ("wl_compositor", "wl_shm", "xdg_wm_base", "wl_seat"):
            if need not in wl.globals:
                raise SystemExit(f"the compositor has no {need}")
        self.compositor = wl.bind("wl_compositor", 4)
        self.shm = wl.bind("wl_shm", 1)
        self.wm = wl.bind("xdg_wm_base", 1, self.on_wm)
        self.seat = wl.bind("wl_seat", 5, self.on_seat)
        self.relative = wl.bind("zwp_relative_pointer_manager_v1", 1) if "zwp_relative_pointer_manager_v1" in wl.globals else None
        self.constraints = wl.bind("zwp_pointer_constraints_v1", 1) if "zwp_pointer_constraints_v1" in wl.globals else None
        print(f"relative pointer: {bool(self.relative)}, pointer constraints: {bool(self.constraints)}", flush=True)

        self.surface = wl.new_id()
        wl.send(self.compositor, 0, struct.pack("<I", self.surface))
        self.xdg_surface = wl.new_id(self.on_xdg_surface)
        wl.send(self.wm, 2, struct.pack("<II", self.xdg_surface, self.surface))
        self.toplevel = wl.new_id(self.on_toplevel)
        wl.send(self.xdg_surface, 1, struct.pack("<I", self.toplevel))
        wl.send(self.toplevel, 2, string(TITLE))
        wl.send(self.toplevel, 3, string("gogo-e2e-wayland-client"))
        wl.send(self.surface, 6)  # commit, no buffer: asks for the first configure

    # events ------------------------------------------------------------------

    def on_registry(self, op, body):
        if op == 0:
            name = struct.unpack_from("<I", body)[0]
            n = struct.unpack_from("<I", body, 4)[0]
            iface = body[8 : 8 + n - 1].decode()
            version = struct.unpack_from("<I", body, 8 + n + (-n % 4))[0]
            self.wl.globals[iface] = (name, version)

    def on_wm(self, op, body):
        if op == 0:  # ping
            self.wl.send(self.wm, 3, body[:4])

    def on_toplevel(self, op, body):
        if op == 0:
            w, h = struct.unpack_from("<ii", body)
            if w > 0 and h > 0:
                self.pending = (w, h)
        elif op == 1:
            self.exit = 0

    def on_xdg_surface(self, op, body):
        if op == 0:
            self.wl.send(self.xdg_surface, 4, body[:4])  # ack_configure
            if self.pending:
                self.size, self.pending = self.pending, None
            self.draw()

    def on_seat(self, op, body):
        if op == 0:
            caps = struct.unpack_from("<I", body)[0]
            if caps & 1 and self.pointer is None:
                self.pointer = self.wl.new_id(self.on_pointer)
                self.wl.send(self.seat, 0, struct.pack("<I", self.pointer))
            if caps & 2 and self.keyboard is None:
                self.keyboard = self.wl.new_id(self.on_keyboard)
                self.wl.send(self.seat, 1, struct.pack("<I", self.keyboard))

    def on_pointer(self, op, body):
        if op == 3:  # button
            _, _, button, state = struct.unpack_from("<IIII", body)
            print(f"button {button:#x} {'press' if state else 'release'}", flush=True)
            if button == BTN_LEFT and state == 1 and self.colour == WHITE:
                self.colour = BLUE
                self.draw()
                self.start_lock()

    def start_lock(self):
        if not (self.relative and self.constraints):
            print("no relative pointer or pointer constraints: staying blue", flush=True)
            return
        rel = self.wl.new_id(self.on_relative)
        self.wl.send(self.relative, 1, struct.pack("<II", rel, self.pointer))
        self.lock = self.wl.new_id(self.on_lock)
        self.wl.send(self.constraints, 1, struct.pack("<IIIII", self.lock, self.surface, self.pointer, 0, LOCK_PERSISTENT))

    def on_lock(self, op, body):
        self.locked = op == 0
        print("locked" if self.locked else "unlocked", flush=True)

    def on_relative(self, op, body):
        if op == 0:
            dx, dy = fixed(body[8:12]), fixed(body[12:16])
            print(f"relative {dx:g} {dy:g}{'' if self.locked else ' (not locked)'}", flush=True)
            if self.locked:
                self.moved += abs(dx) + abs(dy)
                if self.moved >= MOVE_PX and self.colour == BLUE:
                    self.colour = GREEN
                    self.draw()

    def on_keyboard(self, op, body):
        if op == 3:
            _, _, key, state = struct.unpack_from("<IIII", body)
            if state == 1:
                print(f"key {key}", flush=True)
                self.exit = 0 if key == KEY_Q else 1

    # drawing -------------------------------------------------------------------

    def draw(self):
        w, h = self.size
        stride = w * 4
        fd = os.memfd_create("e2e-wayland-client", os.MFD_CLOEXEC)
        try:
            os.ftruncate(fd, stride * h)
            with mmap.mmap(fd, stride * h) as m:
                r, g, b = self.colour
                m.write(bytes((b, g, r, 0xFF)) * (w * h))
            pool = self.wl.new_id()
            self.wl.send(self.shm, 0, struct.pack("<Ii", pool, stride * h), fds=[fd])
        finally:
            os.close(fd)
        buffer = self.wl.new_id()
        self.wl.send(pool, 0, struct.pack("<IiiiiI", buffer, 0, w, h, stride, XRGB8888))
        self.wl.send(pool, 1)  # destroy: the buffer keeps the memory
        self.wl.send(self.surface, 1, struct.pack("<Iii", buffer, 0, 0))
        self.wl.send(self.surface, 9, struct.pack("<iiii", 0, 0, w, h))
        self.wl.send(self.surface, 6)
        if self.buffer is not None:
            self.wl.send(self.buffer, 0)  # destroy the last one: the new one replaced it
        self.buffer = buffer
        print(f"drew {w}x{h} #{r:02x}{g:02x}{b:02x}", flush=True)


def main():
    display = os.environ.get("WAYLAND_DISPLAY")
    if not display:
        raise SystemExit("no WAYLAND_DISPLAY")
    path = display if display.startswith("/") else os.path.join(os.environ["XDG_RUNTIME_DIR"], display)
    win = Window(Client(path))
    while win.exit is None:
        win.wl.dispatch()
    print(f"exit {win.exit}", flush=True)
    return win.exit


if __name__ == "__main__":
    sys.exit(main())
