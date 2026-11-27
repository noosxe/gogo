//! From a skin and the model's data to a screen: bind, lay out, build (design 24 §2,
//! design 25 §5), and which skin is on screen.

use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::path::Path;

use gogo_skin::{
    ActionTable, BindError, Binding, Bound, DefaultSkin, FallbackWhy, Mode, SkinPackage, UiState,
    UiStateError, Value,
};
use gogo_ui_interact::{Interaction, Layer, LayerId, LayerKind};
use gogo_ui_ir::{NodeId, Tree};
use gogo_ui_layout::{Laid, LayoutError, Scroll, Viewport, layout};
use gogo_ui_scene::{DisplayList, SceneState, build};
use gogo_ui_text::FontSet;

use crate::check::error_lines;
use crate::data::Data;

/// A bound on binding again for sources seen for the first time. A bind stops at the first
/// value it is missing, so a route's first frame takes one pass per source it reads.
const PASSES: usize = 256;

/// Why there is no frame.
#[derive(Debug, thiserror::Error)]
pub(crate) enum DrawError {
    #[error("the built-in skin does not load: {0}")]
    BuiltIn(String),
    #[error(transparent)]
    Bind(#[from] BindError),
    #[error(transparent)]
    Layout(#[from] LayoutError),
    #[error("the app's fallback layer could not be built: {0}")]
    Fallback(#[from] crate::panel::PanelError),
    /// A source the route reads has no value yet.
    #[error("waiting for data")]
    Loading,
}

/// A loaded skin and the values of its `ui` variables.
struct Skin {
    package: SkinPackage,
    ui: UiState,
}

impl Skin {
    fn new(package: SkinPackage) -> Skin {
        let ui = UiState::defaults(&package.manifest);
        Skin { package, ui }
    }
}

/// A route bound and laid out: what is drawn, and what input lands on.
pub(crate) struct Screen {
    pub(crate) tree: Tree,
    pub(crate) laid: Laid,
    pub(crate) actions: ActionTable,
    /// A hash of each action's call, so that a press on a control that came to do
    /// something else is dropped.
    identity: Vec<u64>,
    /// Another screen for another route.
    id: LayerId,
}

impl Screen {
    /// The page layer of this screen.
    pub(crate) fn layer(&self) -> Layer<'_> {
        Layer::new(self.id, LayerKind::Page, &self.tree, &self.laid).identity(&self.identity)
    }

    /// The display list, with the focus ring on `focus`.
    pub(crate) fn list(&self, focus: Option<NodeId>) -> DisplayList {
        let state = match focus {
            Some(node) => SceneState::new().with_focus(node),
            None => SceneState::new(),
        };
        build(&self.tree, &self.laid, &state)
    }
}

fn hash(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// The skins and the fonts.
pub(crate) struct Page {
    default: Skin,
    /// The skin asked for; `None` draws the built-in one.
    active: Option<Skin>,
    pub(crate) fonts: FontSet,
}

impl Page {
    /// The built-in Default skin.
    pub(crate) fn new() -> Result<Page, DrawError> {
        let built_in = gogo_skins::default();
        let package = SkinPackage::from_files(built_in.name, built_in.to_files())
            .map_err(|r| DrawError::BuiltIn(error_lines(&r).join("; ")))?;
        Ok(Page {
            default: Skin::new(package),
            active: None,
            fonts: FontSet::bundled(),
        })
    }

    /// Takes the skin in `dir`, if it loads without errors. Otherwise the skin on screen
    /// stays, and the errors come back, one line each.
    pub(crate) fn load(&mut self, dir: &Path) -> Result<(), Vec<String>> {
        let package = SkinPackage::load(dir).map_err(|r| error_lines(&r))?;
        if package.report().error_total() > 0 {
            return Err(error_lines(package.report()));
        }
        let mut skin = Skin::new(package);
        // A reload keeps the tab the user was on, where the skin still has it.
        if let Some(old) = &self.active {
            skin.ui = UiState::restore(
                &skin.package.manifest,
                old.ui.iter().map(|(k, v)| (k.to_owned(), v.clone())),
            );
        }
        self.active = Some(skin);
        Ok(())
    }

    /// Puts a scratch skin where the built-in one is, so a test can have a route with
    /// nothing to fall back to.
    #[cfg(test)]
    pub(crate) fn replace_built_in(&mut self, files: Vec<(String, Vec<u8>)>) {
        let package = SkinPackage::from_files("default", files).expect("the scratch skin loads");
        self.default = Skin::new(package);
    }

    /// A `ui` variable of the skin on screen.
    #[cfg(test)]
    pub(crate) fn ui(&self, var: &str) -> Option<&Value> {
        self.active.as_ref().unwrap_or(&self.default).ui.get(var)
    }

    /// `ui.set`: a variable of the skin on screen.
    pub(crate) fn set_ui(&mut self, var: &str, value: Value) -> Result<(), UiStateError> {
        let skin = self.active.as_mut().unwrap_or(&mut self.default);
        skin.ui.set(&skin.package.manifest, var, value)
    }

    /// Like [`load`](Self::load), for a reload: the new skin must also bind `route`.
    pub(crate) fn reload(
        &mut self,
        dir: &Path,
        data: &mut Data,
        route: &str,
    ) -> Result<(), Vec<String>> {
        let before = self.active.take();
        // `load` carries the `ui` values over from the skin it replaces.
        self.active = before.as_ref().map(|s| Skin {
            package: s.package.clone(),
            ui: s.ui.clone(),
        });
        let result = self.load(dir).and_then(|()| {
            let bound = match self.bind(data, route) {
                (Ok(bound), _) => bound,
                // What fails while data is on the way says nothing about the skin.
                (Err(_), true) => return Ok(()),
                (Err(e), false) => return Err(vec![e.to_string()]),
            };
            // A route the skin leaves to the default one is not a mistake.
            let broken: Vec<String> = bound
                .fallbacks
                .iter()
                .filter_map(|f| match &f.why {
                    FallbackWhy::NotCovered => None,
                    FallbackWhy::FailedToLoad => Some(format!("route {} failed to load", f.route)),
                    FallbackWhy::Bind(e) => Some(e.to_string()),
                })
                .collect();
            if broken.is_empty() {
                Ok(())
            } else {
                Err(broken)
            }
        });
        if result.is_err() {
            self.active = before;
        }
        result
    }

    /// Binds `route`, watching every source the skin reads along the way. The flag
    /// says that one of them has no value yet.
    fn bind(&self, data: &mut Data, route: &str) -> (Result<Bound, BindError>, bool) {
        let skin = self.active.as_ref().unwrap_or(&self.default);
        let default = DefaultSkin {
            skin: &self.default.package,
            ui: &self.default.ui,
        };
        let mut passes = 0;
        loop {
            let scope = data.scope();
            let bound = skin.package.bind_with_fallback(
                route,
                &Binding {
                    scope: &scope,
                    ui: &skin.ui,
                    mode: Mode::Dark,
                    default: Some(default),
                },
            );
            let asked = scope.asked();
            passes += 1;
            let missing = data.arrives_later() && !asked.is_empty();
            if !data.watch(asked) || passes == PASSES {
                return (bound, missing);
            }
        }
    }

    /// The display list of `route` for a window of `size` logical px.
    #[cfg(any(feature = "fixture", test))]
    pub(crate) fn draw(
        &mut self,
        data: &mut Data,
        route: &str,
        size: (f32, f32),
    ) -> Result<DisplayList, DrawError> {
        Ok(self.screen(data, route, size, None)?.list(None))
    }

    /// `route` bound and laid out for a window of `size` logical px, scrolled as `input`
    /// has it (at the top without one).
    pub(crate) fn screen(
        &mut self,
        data: &mut Data,
        route: &str,
        (width, height): (f32, f32),
        input: Option<&Interaction>,
    ) -> Result<Screen, DrawError> {
        let bound = match self.bind(data, route) {
            (_, true) => return Err(DrawError::Loading),
            (bound, false) => bound?,
        };
        let location = data.location();
        let id = LayerId(hash(&format!("{} {:?}", location.route, location.params)));
        let mut scroll = Scroll::none();
        for (key, y) in input.into_iter().flat_map(|input| input.offsets(id)) {
            if let Some(node) = bound.tree.by_key(key) {
                scroll.set(node, 0.0, y);
            }
        }
        let laid = layout(
            &bound.tree,
            &mut self.fonts,
            Viewport::new(width, height),
            &scroll,
        )?;
        let identity = bound
            .actions
            .iter()
            .map(|(_, call)| hash(&format!("{} {:?}", call.spec.name, call.args)))
            .collect();
        Ok(Screen {
            tree: bound.tree,
            laid,
            actions: bound.actions,
            identity,
            id,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;

    /// The built-in skin written out as a folder.
    fn copy_of_default(dir: &Path) -> PathBuf {
        let root = dir.join("default");
        for (path, data) in gogo_skins::default().files {
            let path = root.join(path);
            fs::create_dir_all(path.parent().expect("a file in a folder")).expect("folders");
            fs::write(path, data).expect("a write");
        }
        root
    }

    #[test]
    fn a_broken_edit_keeps_the_last_good_skin() {
        let dir = crate::core::tests::scratch();
        let skin = copy_of_default(dir.path());
        let mut data = Data::fixture(|| {}).expect("fixture data");
        let mut page = Page::new().expect("the built-in skin");
        let route = "shell";
        page.reload(&skin, &mut data, route).expect("a copy loads");
        let good = page.draw(&mut data, route, (960.0, 600.0)).expect("draws");

        let file = skin.join("routes/game.kdl");
        let original = fs::read_to_string(&file).expect("the route");
        fs::write(&file, format!("{original}\nnonsense {{")).expect("a write");
        let errors = page.reload(&skin, &mut data, route).expect_err("broken");
        assert!(errors[0].contains("routes/game.kdl"), "{errors:?}");
        let kept = page.draw(&mut data, route, (960.0, 600.0)).expect("draws");
        assert_eq!(kept, good);

        fs::write(&file, original).expect("a write");
        page.reload(&skin, &mut data, route).expect("fixed");
    }

    #[test]
    fn every_route_of_the_default_skin_draws_on_the_fixture() {
        let mut data = Data::fixture(|| {}).expect("fixture data");
        let mut page = Page::new().expect("the built-in skin");
        for route in gogo_contract::routes::ROUTE_NAMES {
            let list = page.draw(&mut data, route, (960.0, 600.0)).expect(route);
            assert!(!list.ops.is_empty(), "{route}");
        }
    }
}
