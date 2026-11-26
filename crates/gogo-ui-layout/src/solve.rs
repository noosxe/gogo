//! IR → taffy → boxes (design 24 §2).

use gogo_ui_ir::{Kind, NodeId, Tree};
use gogo_ui_text::{FontSet, TextLayout};
use taffy::prelude::{AvailableSpace, Size, TaffyTree};
use taffy::{Layout as TaffyLayout, NodeId as TaffyId};

use crate::laid::{Laid, Placed, Rect, Scroll};
use crate::style::style;

/// The window's size in logical px. Build it with [`Viewport::new`]: more fields (a safe
/// area, say) may follow.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct Viewport {
    /// Width in logical px.
    pub width: f32,
    /// Height in logical px.
    pub height: f32,
}

impl Viewport {
    /// A viewport of this size.
    pub const fn new(width: f32, height: f32) -> Viewport {
        Viewport { width, height }
    }
}

/// Why a layout pass was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum LayoutError {
    /// The viewport is not a finite, non-negative size.
    #[error("the viewport must be finite and not negative")]
    BadViewport,
    /// A length in a node's layout or paint is NaN or infinite.
    #[error("node `{key}`: {field} is not a finite number")]
    NotFinite {
        /// The node's key.
        key: String,
        /// What is not finite, in words.
        field: &'static str,
    },
    /// The tree is nested deeper than [`MAX_DEPTH`]. taffy recurses once per level, so a
    /// deeper tree could overflow the stack.
    #[error("the tree is nested deeper than {MAX_DEPTH} levels")]
    TooDeep,
    /// taffy refused the tree or the pass; its message.
    #[error("the layout solver failed: {0}")]
    Solver(String),
}

impl From<taffy::TaffyError> for LayoutError {
    fn from(e: taffy::TaffyError) -> Self {
        LayoutError::Solver(e.to_string())
    }
}

/// No clip at all.
const EVERYWHERE: Rect = Rect::new(
    f64::NEG_INFINITY,
    f64::NEG_INFINITY,
    f64::INFINITY,
    f64::INFINITY,
);

/// The deepest nesting `layout` accepts (the root is level 1). The skin loader allows far
/// less (design 27 §8); this bounds hand-built trees, where about a thousand levels
/// overflow the stack inside taffy.
pub const MAX_DEPTH: usize = 128;

/// Slack added to a text node's final width before it is broken again, so a line that
/// measured exactly this wide never wraps on rounding.
const SLACK: f32 = 0.5;

/// Lays out `tree` in `viewport`: the root fills it, and every other box follows from the
/// nodes' [`LayoutStyle`](gogo_ui_ir::LayoutStyle). `scroll` moves the content of the
/// scroll containers it names.
///
/// # Errors
/// [`LayoutError::TooDeep`] past [`MAX_DEPTH`] levels, [`LayoutError::BadViewport`], [`LayoutError::NotFinite`] for a NaN or infinite length,
/// or [`LayoutError::Solver`] if taffy fails.
pub fn layout(
    tree: &Tree,
    fonts: &mut FontSet,
    viewport: Viewport,
    scroll: &Scroll,
) -> Result<Laid, LayoutError> {
    let ok = |v: f32| v.is_finite() && v >= 0.0;
    if !(ok(viewport.width) && ok(viewport.height)) {
        return Err(LayoutError::BadViewport);
    }
    let n = tree.len();
    let ids: Vec<NodeId> = nodes_by_index(tree);
    let mut depth = vec![0usize; n];
    for &id in &ids {
        let d = tree.parent(id).map_or(1, |p| depth[p.index()] + 1);
        if d > MAX_DEPTH {
            return Err(LayoutError::TooDeep);
        }
        depth[id.index()] = d;
    }

    // Shape the text leaves once; taffy's measure breaks them at the widths it asks for.
    let mut texts: Vec<Option<TextLayout>> = (0..n).map(|_| None).collect();
    for &id in &ids {
        if let Some(node) = tree.get(id)
            && let Kind::Text { text, style } = &node.kind
        {
            texts[id.index()] = Some(fonts.layout(text, style));
        }
    }

    // Children are created before their parents: the arena keeps parents before children
    // in insertion order, so walking it backwards visits every child first. A taffy node
    // is made once with its children and never re-parented (`add_child` on an attached
    // node panics later).
    let mut solver: TaffyTree<usize> = TaffyTree::with_capacity(n);
    let mut taffy_ids: Vec<Option<TaffyId>> = vec![None; n];
    for &id in ids.iter().rev() {
        let Some(node) = tree.get(id) else {
            return Err(LayoutError::Solver("a node vanished from its tree".into()));
        };
        let key = tree.key(id).unwrap_or_default();
        let mut st = style(key, node)?;
        if id == tree.root() {
            // The root is the window: it fills the viewport whatever its own style says.
            st.size = Size {
                width: taffy::Dimension::length(viewport.width),
                height: taffy::Dimension::length(viewport.height),
            };
            st.margin = taffy::Rect::zero();
            st.position = taffy::Position::Relative;
        }
        let made = if texts[id.index()].is_some() {
            solver.new_leaf_with_context(st, id.index())?
        } else {
            let kids: Vec<TaffyId> = tree
                .children(id)
                .iter()
                .filter_map(|c| taffy_ids[c.index()])
                .collect();
            if kids.len() != tree.children(id).len() {
                return Err(LayoutError::Solver(
                    "a child was built after its parent".into(),
                ));
            }
            solver.new_with_children(st, &kids)?
        };
        taffy_ids[id.index()] = Some(made);
    }
    let root = taffy_ids[tree.root().index()]
        .ok_or_else(|| LayoutError::Solver("the tree has no root".into()))?;

    solver.compute_layout_with_measure(
        root,
        Size {
            width: AvailableSpace::Definite(viewport.width),
            height: AvailableSpace::Definite(viewport.height),
        },
        |inputs, _id, ctx, st| {
            let Some(&mut ti) = ctx else {
                return taffy::compute_leaf_layout(inputs, st, |_, _| 0.0, |_, _| Size::ZERO);
            };
            let text = texts.get_mut(ti).and_then(Option::as_mut);
            taffy::compute_leaf_layout(
                inputs,
                st,
                |_, _| 0.0,
                |known, avail| {
                    let Some(t) = text else { return Size::ZERO };
                    let max = match (known.width, avail.width) {
                        (Some(w), _) => Some(w),
                        (None, AvailableSpace::Definite(w)) => Some(w),
                        (None, AvailableSpace::MinContent) => Some(0.0),
                        (None, AvailableSpace::MaxContent) => None,
                    };
                    t.break_lines(max);
                    let (w, h) = t.size();
                    Size {
                        width: known.width.unwrap_or(w.ceil()),
                        height: known.height.unwrap_or(h.ceil()),
                    }
                },
            )
        },
    )?;

    // Absolute boxes: each node sits at its parent's origin plus its own location, less the
    // parent's scroll. Parents come first, so a parent's box is final when a child needs it.
    let mut placed: Vec<Option<Placed>> = (0..n).map(|_| None).collect();
    let mut shift: Vec<(f64, f64)> = vec![(0.0, 0.0); n];
    let mut child_clip: Vec<Rect> = vec![EVERYWHERE; n];
    for &id in &ids {
        let Some(tid) = taffy_ids[id.index()] else {
            continue;
        };
        let l: &TaffyLayout = solver.layout(tid)?;
        let parent = tree.parent(id);
        let (px, py) = match parent.and_then(|p| placed[p.index()].as_ref().map(|pp| (p, pp))) {
            Some((p, pp)) => {
                let (sx, sy) = shift[p.index()];
                (pp.rect.x0 - sx, pp.rect.y0 - sy)
            }
            None => (0.0, 0.0),
        };
        let x0 = px + f64::from(l.location.x);
        let y0 = py + f64::from(l.location.y);
        let rect = Rect::new(
            x0,
            y0,
            x0 + f64::from(l.size.width),
            y0 + f64::from(l.size.height),
        );
        let content = Rect::new(
            rect.x0 + f64::from(l.padding.left + l.border.left),
            rect.y0 + f64::from(l.padding.top + l.border.top),
            (rect.x1 - f64::from(l.padding.right + l.border.right)).max(rect.x0),
            (rect.y1 - f64::from(l.padding.bottom + l.border.bottom)).max(rect.y0),
        );
        // A scroll container clips its descendants to its own visible box.
        let inherited = parent.map_or(EVERYWHERE, |p| child_clip[p.index()]);
        let visible = rect.intersect(&inherited);
        if let Some((sx, sy)) = scroll.get(id) {
            shift[id.index()] = (f64::from(sx), f64::from(sy));
            child_clip[id.index()] = visible;
        } else {
            child_clip[id.index()] = inherited;
        }
        let text = texts[id.index()].take().map(|mut t| {
            t.ellipsize(fonts, Some(content.width() as f32 + SLACK));
            t
        });
        placed[id.index()] = Some(Placed {
            rect,
            content,
            visible,
            text,
            clips: scroll.get(id).is_some(),
            parent,
        });
    }

    let placed: Vec<Placed> = placed
        .into_iter()
        .collect::<Option<_>>()
        .ok_or_else(|| LayoutError::Solver("a node was not placed".into()))?;
    Ok(Laid {
        placed,
        order: tree.iter().collect(),
        width: viewport.width,
        height: viewport.height,
    })
}

/// The tree's nodes in insertion order (parents before children).
fn nodes_by_index(tree: &Tree) -> Vec<NodeId> {
    let mut v: Vec<NodeId> = tree.iter().collect();
    v.sort_by_key(|id| id.index());
    v
}
