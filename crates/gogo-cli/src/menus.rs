//! Which `gogo mac-app` menus a build shows (#76, design 11 §1, design 12
//! §5): none by default — the recipe is `none` and the runtime comes from
//! the game's L4 > L3 > the default track. A bare `--recipe` / `--runtime`
//! brings back its menu, `--interactive` both; a flag with a value pins it
//! with no menu. Menus need a terminal; headless runs never prompt.

use gogo::macapp::{RecipeChoice, RuntimeMode};

use crate::cli::RuntimeSource;

#[derive(Debug, PartialEq)]
pub struct Menus {
    /// `Auto` shows the recipe picker on a terminal.
    pub recipe: RecipeChoice,
    pub mode: RuntimeMode,
    pub runtime_menu: bool,
}

/// `recipe`: `--recipe`'s value (bare = `auto`); `runtime`: `--runtime`
/// (`Some(None)` = bare).
pub fn resolve(
    recipe: Option<&str>,
    runtime: Option<Option<RuntimeSource>>,
    interactive: bool,
) -> Menus {
    let recipe = match recipe {
        Some(r) => RecipeChoice::parse(r),
        None if interactive => RecipeChoice::Auto,
        None => RecipeChoice::None,
    };
    let (mode, runtime_menu) = match runtime {
        Some(Some(src)) => (src.into(), false),
        Some(None) => (RuntimeMode::Auto, true),
        None => (RuntimeMode::Auto, interactive),
    };
    Menus {
        recipe,
        mode,
        runtime_menu,
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::cli::{Cli, Command};

    fn menus(args: &[&str]) -> Menus {
        let argv = ["gogo", "mac-app", "setup_x.exe"].iter().chain(args);
        let Command::MacApp {
            recipe,
            runtime,
            interactive,
            ..
        } = Cli::try_parse_from(argv).unwrap().command
        else {
            panic!("expected mac-app");
        };
        resolve(recipe.as_deref(), runtime, interactive)
    }

    #[test]
    fn no_menus_by_default() {
        assert_eq!(
            menus(&[]),
            Menus {
                recipe: RecipeChoice::None,
                mode: RuntimeMode::Auto,
                runtime_menu: false,
            }
        );
    }

    #[test]
    fn bare_flags_bring_back_their_menu() {
        let m = menus(&["--recipe"]);
        assert_eq!((m.recipe, m.runtime_menu), (RecipeChoice::Auto, false));
        let m = menus(&["--runtime"]);
        assert_eq!((m.recipe, m.runtime_menu), (RecipeChoice::None, true));
        assert_eq!(m.mode, RuntimeMode::Auto);
        let m = menus(&["--interactive"]);
        assert_eq!((m.recipe, m.runtime_menu), (RecipeChoice::Auto, true));
    }

    #[test]
    fn values_pin_without_a_menu() {
        let m = menus(&["--recipe", "pharaoh-gold", "--runtime", "staging"]);
        assert_eq!(m.recipe, RecipeChoice::Slug("pharaoh-gold".into()));
        assert_eq!(m.mode, RuntimeMode::Catalog("wine-staging".into()));
        assert!(!m.runtime_menu);
        // --interactive leaves a pinned value alone.
        let m = menus(&["--interactive", "--recipe", "none", "--runtime", "brew"]);
        assert_eq!(
            (m.recipe, m.mode, m.runtime_menu),
            (RecipeChoice::None, RuntimeMode::Brew, false)
        );
        let m = menus(&["--interactive", "--runtime=auto"]);
        assert_eq!((m.recipe, m.runtime_menu), (RecipeChoice::Auto, false));
    }

    #[test]
    fn retarget_keeps_its_runtime_id() {
        let cli =
            Cli::try_parse_from(["gogo", "mac-app", "--retarget", "1", "--runtime", "winecx"])
                .unwrap();
        let Command::MacApp { runtime, .. } = cli.command else {
            panic!("expected mac-app");
        };
        assert_eq!(runtime, Some(Some(RuntimeSource::Winecx)));
        // Bare `--runtime` parses (main.rs rejects it for --retarget);
        // --interactive is build-only.
        let cli = Cli::try_parse_from(["gogo", "mac-app", "--retarget", "1", "--runtime"]).unwrap();
        let Command::MacApp { runtime, .. } = cli.command else {
            panic!("expected mac-app");
        };
        assert_eq!(runtime, Some(None));
        assert!(
            Cli::try_parse_from(["gogo", "mac-app", "--retarget", "1", "--interactive"]).is_err()
        );
    }
}
