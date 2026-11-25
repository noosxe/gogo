#!/usr/bin/env python3
"""Check the desktop GUI crates' dependency rules (docs/design/24-gui-architecture.md §2.1).

Reads `cargo metadata --format-version 1` (runs it, or reads a file given with
`--metadata FILE`, which the tests use) and checks every workspace member's direct
dependencies:

0. Every `gogo-*` member is known: a GUI crate or one of NON_GUI. A new crate must
   be classified here before it joins the workspace.
1. gogo-core and the other non-GUI members depend on no GUI crate; among the GUI
   crates only gogo-gui depends on gogo-core. This holds for dev-dependencies too.
2-6. Each GUI crate depends only on the internal crates its layer allows, and on
   none of the external crates its layer must stay free of (for example: renderers
   never see winit, the model never sees tokio, data crates never see a renderer).
   Dev-dependencies are free here: tests may use what the layer itself must not.
7. gogo-gui-testkit is only ever a dev-dependency.

It also checks that the lists of GUI crates elsewhere haven't drifted from GUI: every
GUI name (and no NON_GUI name) matches GUI_NAME, the pattern nix/app.nix uses; GUI is
exactly the GUI_CRATES list in .github/workflows/ci-jobs.yml; and that workflow's `gui`
path filter covers every GUI crate and gogo-core, but no other crate.

Prints one line per broken rule and exits 1, or exits 0 when the graph is clean.
Run with `python3 -I`, from the workspace root.
"""

import argparse
import fnmatch
import json
import os
import re
import subprocess
import sys

GUI = {
    "gogo-contract", "gogo-ui-ir", "gogo-skin", "gogo-skins", "gogo-ui-text",
    "gogo-ui-layout", "gogo-ui-scene", "gogo-render-cpu", "gogo-render-gpu",
    "gogo-ui-interact", "gogo-ui-a11y", "gogo-ui-shell", "gogo-gui-model",
    "gogo-gui-testkit", "gogo-gui",
}
TESTKIT = "gogo-gui-testkit"
# A GUI crate's name. nix/app.nix matches crate dirs with the same pattern.
GUI_NAME = "gogo-(contract|skins?|ui-.*|render-.*|gui|gui-.*)"
REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
WORKFLOW = os.path.join(".github", "workflows", "ci-jobs.yml")
APP_NIX = os.path.join("nix", "app.nix")
CORE = "gogo-core"
# The members that are not GUI crates.
NON_GUI = {"gogo-core", "gogo-cli", "gogo-shims", "gogo-tui"}

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
    # The GPU renderer creates its own wgpu surface from the window handle (design 24
    # §2 lists the shell's dependencies), so the shell never sees wgpu (rule 4).
    "gogo-ui-shell": {"vello_cpu", "vello_gpu", "wgpu", "tokio"} | FORMAT,
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
    for name in sorted(names - GUI - NON_GUI):
        if name.startswith("gogo-"):
            errors.append(f"{name}: unknown member, add it to GUI or NON_GUI in this script (rule 0)")
    for p in sorted(pkgs, key=lambda p: p["name"]):
        name = p["name"]
        for d in p.get("dependencies", []):
            dep, kind = d["name"], d.get("kind")
            dev = kind == "dev"
            if dep == TESTKIT and not dev:
                errors.append(f"{name} -> {dep}: the testkit is dev-only (rule 7)")
                continue
            how = " (dev)" if dev else ""
            if name not in GUI:
                if dep in GUI:
                    errors.append(f"{name} -> {dep}{how}: non-GUI crates never depend on a GUI crate (rule 1)")
                continue
            if dep == CORE and name != "gogo-gui":
                errors.append(f"{name} -> {dep}{how}: only gogo-gui depends on gogo-core (rule 1)")
                continue
            # Rules 2-6 are about what a layer ships with. A dev-dependency only
            # reaches the crate's own tests, which may use what the layer can't.
            if dev:
                continue
            internal = dep in names
            if internal:
                if dep not in ALLOWED_INTERNAL[name]:
                    errors.append(f"{name} -> {dep}: not an allowed internal dependency (design 24 §2.1)")
            elif dep in FORBIDDEN_EXTERNAL[name]:
                errors.append(f"{name} -> {dep}: this layer must stay free of {dep} (design 24 §2.1)")
    return errors


def workflow_gui_crates(text):
    """The words of the `GUI_CRATES: >-` block in the workflow's env."""
    lines = text.splitlines()
    for i, line in enumerate(lines):
        if line.strip() == "GUI_CRATES: >-":
            indent = len(line) - len(line.lstrip())
            words = []
            for more in lines[i + 1:]:
                if more.strip() and len(more) - len(more.lstrip()) <= indent:
                    break
                words += more.split()
            return words
    return None


def workflow_gui_filter(text):
    """The globs of the `gui:` path filter."""
    lines = text.splitlines()
    for i, line in enumerate(lines):
        if line.strip() == "gui:":
            indent = len(line) - len(line.lstrip())
            globs = []
            for more in lines[i + 1:]:
                stripped = more.strip()
                if stripped.startswith("#"):
                    continue
                if not stripped.startswith("- ") or len(more) - len(more.lstrip()) <= indent:
                    break
                globs.append(stripped[2:].strip().strip("'\""))
            return globs
    return None


def drift(workflow, app_nix):
    """Returns how the other lists of GUI crates differ from GUI, one message each."""
    errors = []
    for name in sorted(GUI):
        if not re.fullmatch(GUI_NAME, name):
            errors.append(f"{name}: a GUI crate that GUI_NAME doesn't match")
    for name in sorted(NON_GUI):
        if re.fullmatch(GUI_NAME, name):
            errors.append(f"{name}: a non-GUI crate that GUI_NAME matches")
    if f'"{GUI_NAME}"' not in app_nix:
        errors.append(f"{APP_NIX}: doesn't match crates with GUI_NAME ({GUI_NAME})")
    listed = workflow_gui_crates(workflow)
    if listed is None:
        errors.append(f"{WORKFLOW}: no GUI_CRATES list")
    else:
        if len(listed) != len(set(listed)):
            errors.append(f"{WORKFLOW}: GUI_CRATES names a crate twice")
        for name in sorted(GUI - set(listed)):
            errors.append(f"{WORKFLOW}: GUI_CRATES lacks {name}")
        for name in sorted(set(listed) - GUI):
            errors.append(f"{WORKFLOW}: GUI_CRATES names {name}, which isn't a GUI crate")
    globs = workflow_gui_filter(workflow)
    if globs is None:
        errors.append(f"{WORKFLOW}: no `gui` path filter")
    else:
        def covered(name):
            return any(fnmatch.fnmatchcase(f"crates/{name}/src/lib.rs", g) for g in globs)
        for name in sorted(GUI | {CORE}):
            if not covered(name):
                errors.append(f"{WORKFLOW}: the gui filter misses crates/{name}")
        for name in sorted(NON_GUI - {CORE}):
            if covered(name):
                errors.append(f"{WORKFLOW}: the gui filter covers crates/{name}")
    return errors


def read(repo, rel):
    with open(os.path.join(repo, rel), encoding="utf-8") as f:
        return f.read()


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--metadata", help="read cargo metadata JSON from this file")
    ap.add_argument("--repo", default=REPO, help="repository root for the drift checks")
    args = ap.parse_args()
    errors = check(load(args.metadata))
    errors += drift(read(args.repo, WORKFLOW), read(args.repo, APP_NIX))
    for e in errors:
        print(f"gui-deps-check: {e}")
    if errors:
        return 1
    print(f"gui-deps-check: ok ({len(GUI)} GUI crates)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
