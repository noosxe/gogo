//! The bundled fonts and the shaping contexts (design 24 decision 10, design 27 §3).

use std::borrow::Cow;
use std::sync::Arc;

use gogo_ui_ir::TextStyle;
use parley::fontique::{Blob, Collection, CollectionOptions, FontInfoOverride, SourceCache};
use parley::{
    FontContext, FontFamily, FontFamilyName, FontWeight, FontWidth, LayoutContext, LineHeight,
    StyleProperty,
};

use crate::TextLayout;

/// The family an empty or unknown [`TextStyle::family`] gets.
pub const DEFAULT_FAMILY: &str = "Noto Sans";

/// The bundled families, as their font files name themselves (design 27 §3), in fallback
/// order.
pub const BUNDLED_FAMILIES: &[&str] = &["Noto Sans", "JetBrains Mono"];

/// The bundled font files, by the family they register. The licence texts sit next to
/// them in `fonts/`.
const BUNDLED: &[(&str, &[u8])] = &[
    ("Noto Sans", include_bytes!("../fonts/NotoSans.ttf")),
    (
        "JetBrains Mono",
        include_bytes!("../fonts/JetBrainsMono.ttf"),
    ),
];

/// The private name a bundled family is registered under, so a system font of the same
/// name never mixes with it.
fn private_name(family: &str) -> String {
    format!("gogo-ui-text bundled {family}")
}

/// Fonts and the shaping caches behind every [`TextLayout`].
///
/// One per window (it is not `Sync`): shaping needs `&mut`. Building one parses only the
/// font tables it needs, so it is cheap, but the caches it keeps make reuse pay.
pub struct FontSet {
    fcx: FontContext,
    lcx: LayoutContext<()>,
    /// Public family name → private registered name, for the bundled families.
    families: Vec<(&'static str, String)>,
}

impl std::fmt::Debug for FontSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FontSet")
            .field("families", &self.families)
            .finish_non_exhaustive()
    }
}

impl FontSet {
    /// The bundled fonts only, with system fonts off: the same shapes on every machine.
    /// This is what tests and golden images use.
    pub fn bundled() -> FontSet {
        FontSet::new(false)
    }

    /// The bundled fonts, then the system's fonts for glyphs none of them has (a CJK game
    /// title, say). A bundled family always wins over a system font of the same name.
    #[cfg(feature = "system-fonts")]
    pub fn with_system_fallback() -> FontSet {
        FontSet::new(true)
    }

    fn new(system_fonts: bool) -> FontSet {
        let mut collection = Collection::new(CollectionOptions {
            shared: false,
            system_fonts,
        });
        let mut families = Vec::with_capacity(BUNDLED.len());
        for &(family, bytes) in BUNDLED {
            let name = private_name(family);
            collection.register_fonts(
                Blob::new(Arc::new(bytes)),
                Some(FontInfoOverride {
                    family_name: Some(&name),
                    ..FontInfoOverride::default()
                }),
            );
            families.push((family, name));
        }
        FontSet {
            fcx: FontContext {
                collection,
                source_cache: SourceCache::default(),
            },
            lcx: LayoutContext::new(),
            families,
        }
    }

    /// Whether `family` is a bundled family (exact, case-sensitive, as design 27 §3).
    pub fn has_family(&self, family: &str) -> bool {
        self.families.iter().any(|(f, _)| *f == family)
    }

    /// The registered (private) name of a bundled family, or the default family's.
    fn resolve(&self, family: &str) -> &str {
        let pick = if self.has_family(family) {
            family
        } else {
            DEFAULT_FAMILY
        };
        self.families
            .iter()
            .find(|(f, _)| *f == pick)
            .map_or("", |(_, n)| n.as_str())
    }

    /// The family stack for a run: its own family, then every other bundled family.
    fn stack(&self, family: &str) -> Vec<FontFamilyName<'static>> {
        let first = self.resolve(family).to_string();
        let mut out = vec![FontFamilyName::Named(Cow::Owned(first.clone()))];
        for (_, name) in &self.families {
            if *name != first {
                out.push(FontFamilyName::Named(Cow::Owned(name.clone())));
            }
        }
        out
    }

    /// Shapes `text` in `style`. The result is one unbroken line until
    /// [`TextLayout::break_lines`] gives it a width.
    pub fn layout(&mut self, text: &str, style: &TextStyle) -> TextLayout {
        let inner = self.shape(text, style);
        TextLayout::new(inner, text, style)
    }

    pub(crate) fn shape(&mut self, text: &str, style: &TextStyle) -> parley::Layout<()> {
        let properties = self.properties(style);
        let mut b = self.lcx.ranged_builder(&mut self.fcx, text, 1.0, true);
        for property in properties {
            b.push_default(property);
        }
        b.build(text)
    }

    /// What `style` means to parley. Drawn text and an [`EditBuffer`](crate::EditBuffer)
    /// shape with the same list, so a caret lands between the drawn glyphs.
    pub(crate) fn properties(&self, style: &TextStyle) -> Vec<StyleProperty<'static, ()>> {
        let mut out = vec![
            StyleProperty::FontFamily(FontFamily::List(Cow::Owned(self.stack(&style.family)))),
            StyleProperty::FontSize(style.size),
            StyleProperty::FontWeight(FontWeight::new(style.weight)),
            StyleProperty::FontWidth(FontWidth::from_percentage(style.width)),
            StyleProperty::LetterSpacing(style.tracking * style.size),
        ];
        if let Some(lh) = style.line_height {
            out.push(StyleProperty::LineHeight(LineHeight::FontSizeRelative(lh)));
        }
        out
    }

    /// parley's two contexts, for an editor to shape with.
    pub(crate) fn contexts(&mut self) -> (&mut FontContext, &mut LayoutContext<()>) {
        (&mut self.fcx, &mut self.lcx)
    }
}

impl Default for FontSet {
    fn default() -> Self {
        FontSet::bundled()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_bundled_font_registers_under_its_private_name() {
        let mut fs = FontSet::bundled();
        let names: Vec<String> = fs
            .fcx
            .collection
            .family_names()
            .map(str::to_string)
            .collect();
        for f in BUNDLED_FAMILIES {
            assert!(names.contains(&private_name(f)), "{f}: {names:?}");
            assert!(fs.has_family(f));
        }
        // System fonts are off: nothing but the bundled families.
        assert_eq!(names.len(), BUNDLED_FAMILIES.len(), "{names:?}");
    }

    #[test]
    fn the_bundled_list_matches_the_files() {
        let files: Vec<&str> = BUNDLED.iter().map(|(f, _)| *f).collect();
        assert_eq!(files, BUNDLED_FAMILIES);
        assert!(BUNDLED_FAMILIES.contains(&DEFAULT_FAMILY));
    }

    #[test]
    fn an_unknown_or_empty_family_gets_the_default_face() {
        let fs = FontSet::bundled();
        let default = fs.resolve(DEFAULT_FAMILY).to_string();
        assert_eq!(fs.resolve(""), default);
        assert_eq!(fs.resolve("Comic Sans"), default);
        assert_eq!(fs.resolve("noto sans"), default, "names are case-sensitive");
        assert_ne!(fs.resolve("JetBrains Mono"), default);
    }

    #[test]
    fn the_stack_tries_the_other_bundled_families_after_the_own() {
        let fs = FontSet::bundled();
        let stack = fs.stack("JetBrains Mono");
        let names: Vec<String> = stack
            .iter()
            .map(|n| match n {
                FontFamilyName::Named(s) => s.to_string(),
                FontFamilyName::Generic(g) => format!("{g:?}"),
            })
            .collect();
        assert_eq!(
            names,
            [private_name("JetBrains Mono"), private_name("Noto Sans")]
        );
    }
}
