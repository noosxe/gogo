//! The binary's commands that open no window (design 24 §5, design 25 §8). `--render-png`
//! must draw what the committed goldens hold (design 08 §7).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use gogo_gui_testkit::golden::{self, GOLDEN_TOLERANCE};

fn gui(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_gogo-gui"))
        .args(args)
        .output()
        .expect("gogo-gui runs")
}

fn text(path: &Path) -> &str {
    path.to_str().expect("a UTF-8 path")
}

fn goldens() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../gogo-gui-testkit/tests/goldens")
}

/// The built-in skin written out as a folder, with `edit` applied to one file.
fn skin(dir: &Path, edit: Option<(&str, &str)>) -> PathBuf {
    let root = dir.join("default");
    for (path, data) in gogo_skins::default().files {
        let mut data = data.to_vec();
        if let Some((file, extra)) = edit
            && file == *path
        {
            data.extend_from_slice(extra.as_bytes());
        }
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().expect("a file in a folder")).expect("folders");
        std::fs::write(path, data).expect("a write");
    }
    root
}

#[test]
fn render_png_draws_the_goldens() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join("golden");
    let mut failures = Vec::new();
    for (route, name) in [("shell", "default-shell"), ("game", "default-game")] {
        for scale in ["1", "2"] {
            let name = format!("{name}@{scale}x");
            let png = dir.path().join(format!("{name}.png"));
            let run = gui(&[
                "--render-png",
                text(&png),
                "--route",
                route,
                "--scale",
                scale,
            ]);
            assert!(run.status.success(), "{name}: {run:?}");
            let actual = golden::load_png(&png).expect("a PNG");
            let want = golden::load_png(&goldens().join(format!("{name}.png"))).expect("a golden");
            // Never writes the golden, whatever the environment says.
            match golden::compare(&out, &name, &want, &actual, &GOLDEN_TOLERANCE) {
                Ok(diff) => println!("{name}: {diff}"),
                Err(e) => failures.push(e.to_string()),
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn render_png_takes_a_skin_folder() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let skin = skin(dir.path(), None);
    let png = dir.path().join("out.png");
    let run = gui(&["--render-png", text(&png), "--skin", text(&skin)]);
    assert!(run.status.success(), "{run:?}");
    let actual = golden::load_png(&png).expect("a PNG");
    let want = golden::load_png(&goldens().join("default-shell@1x.png")).expect("a golden");
    let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join("golden");
    golden::compare(&out, "folder", &want, &actual, &GOLDEN_TOLERANCE).expect("the same skin");
}

#[test]
fn check_skin_exit_codes() {
    let dir = tempfile::tempdir().expect("a temporary folder");
    let good = skin(&dir.path().join("good"), None);
    let run = gui(&["--check-skin", text(&good)]);
    assert_eq!(run.status.code(), Some(0), "{run:?}");
    assert!(String::from_utf8_lossy(&run.stdout).contains("0 errors"));

    let route = skin(
        &dir.path().join("route"),
        Some(("routes/game.kdl", "\nnonsense {")),
    );
    let run = gui(&["--check-skin", text(&route)]);
    assert_eq!(run.status.code(), Some(1), "{run:?}");
    assert!(String::from_utf8_lossy(&run.stderr).contains("routes/game.kdl"));

    let run = gui(&["--check-skin", text(&dir.path().join("missing"))]);
    assert_eq!(run.status.code(), Some(1), "{run:?}");
}

#[test]
fn usage_errors_exit_2() {
    for args in [
        &[][..],
        &["--route", "game"],
        &["--render-png", "a.png", "--scale", "9"],
        &["--check-skin", "a", "--render-png", "b"],
        &["--unknown"],
    ] {
        let run = gui(args);
        assert_eq!(run.status.code(), Some(2), "{args:?}: {run:?}");
    }
}
