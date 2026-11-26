//! The app's own error panel (design 25 §8): why the skin on screen is not the one on
//! disk, or why a route does not draw. It is drawn over the skin and uses nothing of it.

use std::hash::{DefaultHasher, Hash as _, Hasher as _};

use gogo_ui_interact::{Layer, LayerId, LayerKind};
use gogo_ui_ir::{
    Color, Dim, Kind, LayoutStyle, Node, Paint, Position, Sides, TextStyle, Tree, TreeError,
};
use gogo_ui_layout::{Laid, LayoutError, Scroll, Viewport, layout};
use gogo_ui_scene::{DisplayList, SceneState, build};
use gogo_ui_text::FontSet;

/// The most lines shown; the rest are counted.
const LINES: usize = 8;

const BACKGROUND: Color = Color([0x2a, 0x10, 0x14, 0xf2]);
const HEADING: Color = Color([0xff, 0x8a, 0x80, 0xff]);
const TEXT: Color = Color([0xf5, 0xf5, 0xf5, 0xff]);

/// Why the panel could not be drawn.
#[derive(Debug, thiserror::Error)]
pub(crate) enum PanelError {
    #[error(transparent)]
    Tree(#[from] TreeError),
    #[error(transparent)]
    Layout(#[from] LayoutError),
}

fn text(line: &str, size: f32, color: Color) -> Node {
    let style = TextStyle {
        size,
        color,
        wrap: true,
        max_lines: 2,
        ..TextStyle::default()
    };
    Node::new(Kind::Text {
        text: line.to_owned(),
        style,
    })
}

/// The panel, laid out over a window: drawn over the skin, and while it is up the only
/// layer that takes input (an alert, so a screen reader hears it).
pub(crate) struct Panel {
    tree: Tree,
    laid: Laid,
    /// A different set of errors is a different screen.
    id: LayerId,
}

impl Panel {
    /// The panel along the bottom of a window of `width` by `height` logical px.
    pub(crate) fn new(
        fonts: &mut FontSet,
        (width, height): (f32, f32),
        heading: &str,
        lines: &[String],
    ) -> Result<Panel, PanelError> {
        let mut root = Node::new(Kind::Box);
        root.layout.width = Dim::Px(width);
        root.layout.height = Dim::Px(height);
        root.a11y.name = Some(heading.to_owned());
        let mut tree = Tree::new("panel", root);

        let mut style = LayoutStyle::default();
        style.position = Position::Absolute;
        style.inset = Sides {
            top: Dim::Auto,
            right: Dim::Px(0.0),
            bottom: Dim::Px(0.0),
            left: Dim::Px(0.0),
        };
        style.row_gap = 6.0;
        style.padding = Sides {
            top: 14.0,
            right: 18.0,
            bottom: 14.0,
            left: 18.0,
        };
        let mut panel = Node::new(Kind::Box);
        panel.layout = style;
        panel.paint = Paint::solid(BACKGROUND);
        let root = tree.root();
        let panel = tree.add_child(root, "panel/box", panel)?;

        tree.add_child(panel, "panel/heading", text(heading, 15.0, HEADING))?;
        for (i, line) in lines.iter().take(LINES).enumerate() {
            tree.add_child(panel, format!("panel/{i}"), text(line, 13.0, TEXT))?;
        }
        if lines.len() > LINES {
            let more = format!("and {} more", lines.len() - LINES);
            tree.add_child(panel, "panel/more", text(&more, 13.0, TEXT))?;
        }

        let laid = layout(&tree, fonts, Viewport::new(width, height), &Scroll::none())?;
        let mut hasher = DefaultHasher::new();
        ("panel", heading, lines).hash(&mut hasher);
        Ok(Panel {
            tree,
            laid,
            id: LayerId(hasher.finish()),
        })
    }

    pub(crate) fn layer(&self) -> Layer<'_> {
        Layer::new(self.id, LayerKind::Alert, &self.tree, &self.laid)
    }

    /// Adds the panel's drawing to `list`.
    pub(crate) fn draw(&self, list: &mut DisplayList) {
        // The root paints nothing, so building adds the panel's ops alone.
        list.ops
            .extend(build(&self.tree, &self.laid, &SceneState::new()).ops);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_panel_is_added_over_what_is_there() {
        let tree = Tree::new("empty", Node::new(Kind::Box));
        let mut fonts = FontSet::bundled();
        let laid = layout(
            &tree,
            &mut fonts,
            Viewport::new(400.0, 300.0),
            &Scroll::none(),
        )
        .expect("lays out");
        let mut list = build(&tree, &laid, &SceneState::new());
        let before = list.ops.len();
        let lines: Vec<String> = (0..12).map(|i| format!("finding {i}")).collect();
        Panel::new(&mut fonts, (400.0, 300.0), "Broken", &lines)
            .expect("lays out")
            .draw(&mut list);
        assert!(list.ops.len() > before);
        // The panel has no images, so no image ids can clash.
        assert!(list.images.is_empty());
    }
}
