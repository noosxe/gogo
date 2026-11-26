//! The built-in Default skin binds its `game` and `shell` routes against the fixture scope
//! (design 27 §9 step 4, issue #526): no errors, and a tree that passes `Tree::validate`.

use gogo_gui_testkit::FixtureScope;
use gogo_skin::{Binding, Mode, SkinPackage, UiState};

fn skin() -> SkinPackage {
    let d = gogo_skins::default();
    match SkinPackage::from_files(d.name, d.to_files()) {
        Ok(p) => p,
        Err(r) => panic!("{r}"),
    }
}

#[test]
fn the_default_skin_binds_its_game_and_shell_routes() {
    let pkg = skin();
    let scope = FixtureScope::new();
    let ui = UiState::defaults(&pkg.manifest);
    let b = Binding {
        scope: &scope,
        ui: &ui,
        mode: Mode::Dark,
        default: None,
    };
    for route in ["game", "shell"] {
        let bound = match pkg.bind(route, &b) {
            Ok(b) => b,
            Err(e) => panic!("{route}: {e:?}"),
        };
        bound.tree.validate().expect("a valid tree");
        assert!(bound.tree.len() > 20, "{route}: {}", bound.tree.len());
        assert!(bound.fallbacks.is_empty());
    }
}
