//! How long binding takes (design 27 §8, §9.1): the Default skin's game route, and a route
//! with a list of 1 000 rows. The bounds are far above what a release build needs, so a
//! slow machine or a debug build doesn't fail them; the numbers are printed. Run
//! `cargo nextest run --release -p gogo-gui-testkit bind_timing --no-capture` to see them.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use gogo_gui_testkit::{FixtureScope, GAME_ID};
use gogo_skin::{Binding, Mode, SkinPackage, UiState, Value};

fn files() -> Vec<(String, Vec<u8>)> {
    gogo_skins::default().to_files()
}

fn skin(extra_game_nodes: &str) -> SkinPackage {
    let mut f = files();
    for (p, d) in &mut f {
        if p == "routes/game.kdl" {
            let text = String::from_utf8(std::mem::take(d)).unwrap();
            *d = text
                .replacen(
                    "    tabs ",
                    &format!("    {extra_game_nodes}\n    tabs "),
                    1,
                )
                .into_bytes();
        }
    }
    match SkinPackage::from_files("default", f) {
        Ok(p) if p.report().findings().iter().all(|f| !f.is_error()) => p,
        Ok(p) => panic!("{}", p.report()),
        Err(r) => panic!("{r}"),
    }
}

fn rows(n: usize) -> Value {
    Value::List(
        (0..n)
            .map(|i| {
                let mut m = BTreeMap::new();
                m.insert("name".to_owned(), Value::Str(format!("ROW_{i}")));
                m.insert("value".to_owned(), Value::Str(format!("value {i}")));
                Value::Map(m)
            })
            .collect(),
    )
}

/// Bind `route` `times` times and give the mean.
fn mean(pkg: &SkinPackage, scope: &FixtureScope, route: &str, times: u32) -> Duration {
    let ui = UiState::defaults(&pkg.manifest);
    let b = Binding {
        scope,
        ui: &ui,
        mode: Mode::Dark,
        default: None,
    };
    // One to warm up, and to see that it binds.
    if let Err(e) = pkg.bind(route, &b) {
        panic!("{route}: {e}");
    }
    let started = Instant::now();
    for _ in 0..times {
        let _ = pkg.bind(route, &b);
    }
    started.elapsed() / times
}

#[test]
fn bind_timing_of_the_game_route_and_a_thousand_rows() {
    let scope = FixtureScope::new();
    let plain = skin("");
    let game = mean(&plain, &scope, "game", 40);
    eprintln!("bind_timing: the Default game route: {game:?} per bind");

    let list = skin(
        "each \"e\" in=\"{game.env($id)}\" { row { text \"{e.name}\" style=\"meta\"; button \"Verify\" action=\"game.verify\" args=\"{$id}\" } }\n    \
         data-table label=\"Rows\" rows=\"{game.env($id)}\" { col \"Name\" \"{row.name}\"; col \"Value\" \"{row.value}\" mono=#true }",
    );
    let mut big = FixtureScope::new();
    big.set_call("game.env", GAME_ID, rows(1000));
    let each = mean(&list, &big, "game", 5);
    eprintln!(
        "bind_timing: the game route with 1 000 each rows and 1 000 table rows: {each:?} per bind"
    );

    // Far above any real machine in release; this only catches a bind that has gone quadratic.
    assert!(game < Duration::from_secs(2), "{game:?}");
    assert!(each < Duration::from_secs(20), "{each:?}");
}
