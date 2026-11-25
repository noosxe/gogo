#!/usr/bin/env python3
"""Tests of scripts/gui-deps-check.py: python3 -I scripts/tests/gui-deps-check-test.py"""

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
SCRIPT = os.path.join(REPO, "scripts", "gui-deps-check.py")
_spec = importlib.util.spec_from_file_location("check", SCRIPT)
check = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(check)

# The graph design 24 §2.1 draws, as normal dependencies.
CLEAN = {
    "gogo-core": [],
    "gogo-cli": ["gogo-core"],
    "gogo-contract": [],
    "gogo-ui-ir": [],
    "gogo-skin": ["gogo-contract", "gogo-ui-ir", "kdl", "miette"],
    "gogo-skins": [],
    "gogo-ui-text": ["gogo-ui-ir", "parley"],
    "gogo-ui-layout": ["gogo-ui-ir", "gogo-ui-text", "taffy"],
    "gogo-ui-scene": ["gogo-ui-ir", "gogo-ui-layout", "gogo-ui-text", "kurbo", "peniko"],
    "gogo-render-cpu": ["gogo-ui-scene", "vello_cpu"],
    "gogo-render-gpu": ["gogo-ui-scene", "vello_gpu", "wgpu"],
    "gogo-ui-interact": ["gogo-ui-ir", "gogo-ui-layout"],
    "gogo-ui-a11y": ["gogo-ui-ir", "gogo-ui-layout", "gogo-ui-interact", "accesskit"],
    "gogo-ui-shell": ["gogo-ui-scene", "gogo-ui-a11y", "winit", "accesskit_winit", "notify"],
    "gogo-gui-model": ["gogo-contract"],
    "gogo-gui-testkit": ["gogo-skin", "gogo-gui-model", "gogo-render-cpu", "png"],
    "gogo-gui": [
        "gogo-ui-shell", "gogo-skin", "gogo-skins", "gogo-gui-model", "gogo-render-cpu",
        "gogo-render-gpu", "gogo-core", "tokio", "image",
    ],
}


def metadata(graph, dev=None):
    """Synthesises `cargo metadata` output: `graph` maps a member to its normal
    dependencies, `dev` to extra dev-dependencies."""
    dev = dev or {}
    pkgs = []
    for name, deps in graph.items():
        ds = [{"name": d, "kind": None} for d in deps]
        ds += [{"name": d, "kind": "dev"} for d in dev.get(name, [])]
        pkgs.append({"id": f"{name} 0.1.0", "name": name, "dependencies": ds})
    return {"packages": pkgs, "workspace_members": [p["id"] for p in pkgs]}


def with_edge(src, dep):
    g = {k: list(v) for k, v in CLEAN.items()}
    g[src].append(dep)
    return g


class Rules(unittest.TestCase):
    def assertBroken(self, graph, needle, dev=None):
        errors = check.check(metadata(graph, dev))
        self.assertTrue(any(needle in e for e in errors), errors)

    def test_clean_graph_passes(self):
        self.assertEqual(check.check(metadata(CLEAN, {"gogo-gui": ["gogo-gui-testkit"]})), [])

    def test_core_never_depends_on_gui(self):
        self.assertBroken(with_edge("gogo-core", "gogo-ui-ir"), "gogo-core -> gogo-ui-ir")

    def test_cli_never_depends_on_gui(self):
        self.assertBroken(with_edge("gogo-cli", "gogo-gui-model"), "gogo-cli -> gogo-gui-model")

    def test_only_gogo_gui_depends_on_core(self):
        self.assertBroken(with_edge("gogo-gui-model", "gogo-core"), "gogo-gui-model -> gogo-core")

    def test_renderer_never_sees_the_ir(self):
        self.assertBroken(with_edge("gogo-render-cpu", "gogo-ui-ir"), "gogo-render-cpu -> gogo-ui-ir")

    def test_renderer_never_sees_winit(self):
        self.assertBroken(with_edge("gogo-render-gpu", "winit"), "gogo-render-gpu -> winit")

    def test_shell_never_depends_on_a_renderer(self):
        self.assertBroken(with_edge("gogo-ui-shell", "gogo-render-gpu"), "gogo-ui-shell -> gogo-render-gpu")

    def test_model_stays_runtime_free(self):
        self.assertBroken(with_edge("gogo-gui-model", "tokio"), "gogo-gui-model -> tokio")

    def test_engine_never_knows_the_contract(self):
        self.assertBroken(with_edge("gogo-ui-layout", "gogo-contract"), "gogo-ui-layout -> gogo-contract")

    def test_format_stays_free_of_layout(self):
        self.assertBroken(with_edge("gogo-skin", "taffy"), "gogo-skin -> taffy")

    def test_testkit_only_as_dev_dependency(self):
        self.assertBroken(with_edge("gogo-gui", "gogo-gui-testkit"), "testkit is dev-only")

    def test_missing_gui_crate_is_reported(self):
        g = {k: v for k, v in CLEAN.items() if k != "gogo-ui-a11y"}
        g["gogo-ui-shell"] = ["gogo-ui-scene"]
        self.assertBroken(g, "gogo-ui-a11y: missing")

    def test_cli_exit_codes(self):
        with tempfile.TemporaryDirectory() as d:
            for graph, code in ((CLEAN, 0), (with_edge("gogo-core", "gogo-skin"), 1)):
                path = os.path.join(d, "meta.json")
                with open(path, "w", encoding="utf-8") as f:
                    json.dump(metadata(graph), f)
                r = subprocess.run(
                    [sys.executable, "-I", SCRIPT, "--metadata", path],
                    capture_output=True, text=True,
                )
                self.assertEqual(r.returncode, code, r.stdout + r.stderr)


if __name__ == "__main__":
    unittest.main()
