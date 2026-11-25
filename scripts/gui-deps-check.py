#!/usr/bin/env python3
"""Check the desktop GUI crates' dependency rules (docs/design/24-gui-architecture.md §2.1).

Reads `cargo metadata --format-version 1` (runs it, or reads a file given with
`--metadata FILE`, which the tests use) and checks every workspace member's direct
dependencies:

1. gogo-core and the other non-GUI members depend on no GUI crate; among the GUI
   crates only gogo-gui depends on gogo-core.
2-6. Each GUI crate depends only on the internal crates its layer allows, and on
   none of the external crates its layer must stay free of (for example: renderers
   never see winit, the model never sees tokio, data crates never see a renderer).
7. gogo-gui-testkit is only ever a dev-dependency.

Prints one line per broken rule and exits 1, or exits 0 when the graph is clean.
Run with `python3 -I`, from the workspace root.
"""

import argparse
import json
import subprocess
import sys

GUI = {
    "gogo-contract", "gogo-ui-ir", "gogo-skin", "gogo-skins", "gogo-ui-text",
    "gogo-ui-layout", "gogo-ui-scene", "gogo-render-cpu", "gogo-render-gpu",
    "gogo-ui-interact", "gogo-ui-a11y", "gogo-ui-shell", "gogo-gui-model",
    "gogo-gui-testkit", "gogo-gui",
}
TESTKIT = "gogo-gui-testkit"

# Internal (workspace) crates each GUI crate may use as a normal or build dependency.
# gogo-gui may use every GUI crate except the testkit, plus gogo-core.
ALLOWED_INTERNAL = {
    "gogo-contract": set(),
    "gogo-ui-ir": set(),
    "gogo-skin": {"gogo-contract", "gogo-ui-ir"},
    "gogo-skins": set(),
    "gogo-ui-text": {"gogo-ui-ir"},
    "gogo-ui-layout": {"gogo-ui-ir", "gogo-ui-text"},
    "gogo-ui-scene": {"gogo-ui-ir", "gogo-ui-layout", "gogo-ui-text"},
    "gogo-render-cpu": {"gogo-ui-scene"},
    "gogo-render-gpu": {"gogo-ui-scene"},
    "gogo-ui-interact": {"gogo-ui-ir", "gogo-ui-layout", "gogo-ui-text"},
    "gogo-ui-a11y": {"gogo-ui-ir", "gogo-ui-layout", "gogo-ui-text", "gogo-ui-interact"},
    "gogo-ui-shell": {
        "gogo-ui-ir", "gogo-ui-text", "gogo-ui-layout", "gogo-ui-scene",
        "gogo-ui-interact", "gogo-ui-a11y",
    },
    "gogo-gui-model": {"gogo-contract"},
    "gogo-gui-testkit": {
        "gogo-contract", "gogo-ui-ir", "gogo-skin", "gogo-skins", "gogo-ui-text",
        "gogo-ui-layout", "gogo-ui-scene", "gogo-render-cpu", "gogo-gui-model",
    },
    "gogo-gui": (GUI - {TESTKIT, "gogo-gui"}) | {"gogo-core"},
}

RENDER = {"vello_cpu", "vello_gpu", "wgpu"}
WINDOW = {"winit", "accesskit_winit", "softbuffer", "smithay-clipboard", "arboard", "notify"}
A11Y = {"accesskit", "accesskit_winit", "accesskit_consumer"}
ENGINE = {"taffy", "parley", "fontique", "kurbo", "peniko"}
ASYNC = {"tokio", "pollster"}
FORMAT = {"kdl", "miette"}

# External crates each GUI crate must not depend on directly.
FORBIDDEN_EXTERNAL = {
    "gogo-contract": RENDER | WINDOW | A11Y | ENGINE | ASYNC | FORMAT,
    "gogo-ui-ir": RENDER | WINDOW | A11Y | ENGINE | ASYNC | FORMAT,
    "gogo-skin": RENDER | WINDOW | A11Y | ENGINE | ASYNC,
    "gogo-skins": RENDER | WINDOW | A11Y | ENGINE | ASYNC,
    "gogo-ui-text": RENDER | WINDOW | A11Y | ASYNC | FORMAT,
    "gogo-ui-layout": RENDER | WINDOW | A11Y | ASYNC | FORMAT,
    "gogo-ui-scene": RENDER | WINDOW | A11Y | ASYNC | FORMAT,
    "gogo-render-cpu": WINDOW | A11Y | ASYNC | FORMAT | {"taffy", "parley", "fontique", "vello_gpu", "wgpu"},
    "gogo-render-gpu": WINDOW | A11Y | FORMAT | {"tokio", "taffy", "parley", "fontique", "vello_cpu"},
    "gogo-ui-interact": RENDER | WINDOW | A11Y | ASYNC | FORMAT,
    "gogo-ui-a11y": RENDER | WINDOW | ASYNC | FORMAT,
    "gogo-ui-shell": {"vello_cpu", "vello_gpu", "tokio"} | FORMAT,
    "gogo-gui-model": RENDER | WINDOW | A11Y | ENGINE | ASYNC | FORMAT,
    "gogo-gui-testkit": WINDOW | {"tokio", "vello_gpu", "wgpu"},
    "gogo-gui": set(),
}


def load(path):
    if path:
        with open(path, encoding="utf-8") as f:
            return json.load(f)
    out = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps"],
        check=True, capture_output=True, text=True,
    ).stdout
    return json.loads(out)


def check(meta):
    """Returns the list of broken rules, one message each."""
    members = set(meta.get("workspace_members", []))
    pkgs = [p for p in meta["packages"] if p["id"] in members]
    names = {p["name"] for p in pkgs}
    errors = []
    for gui in sorted(GUI - names):
        errors.append(f"{gui}: missing from the workspace (design 24 §2 lists it)")
    for p in sorted(pkgs, key=lambda p: p["name"]):
        name = p["name"]
        for d in p.get("dependencies", []):
            dep, kind = d["name"], d.get("kind")
            dev = kind == "dev"
            if dep == TESTKIT and not dev:
                errors.append(f"{name} -> {dep}: the testkit is dev-only (rule 7)")
                continue
            if dev:
                continue
            internal = dep in names
            if name not in GUI:
                if dep in GUI:
                    errors.append(f"{name} -> {dep}: non-GUI crates never depend on a GUI crate (rule 1)")
                continue
            if internal:
                if dep not in ALLOWED_INTERNAL[name]:
                    errors.append(f"{name} -> {dep}: not an allowed internal dependency (design 24 §2.1)")
            elif dep in FORBIDDEN_EXTERNAL[name]:
                errors.append(f"{name} -> {dep}: this layer must stay free of {dep} (design 24 §2.1)")
    return errors


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--metadata", help="read cargo metadata JSON from this file")
    args = ap.parse_args()
    errors = check(load(args.metadata))
    for e in errors:
        print(f"gui-deps-check: {e}")
    if errors:
        return 1
    print(f"gui-deps-check: ok ({len(GUI)} GUI crates)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
