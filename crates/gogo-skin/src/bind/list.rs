//! Virtualised lists (design 27 §5.4): of a long list, only the rows around what is in
//! view are bound, with empty boxes where the others would be.

use std::collections::{BTreeSet, HashMap};
use std::ops::Range;

use gogo_contract::Value;
use gogo_ui_ir::{Dim, Direction, Node, NodeId, Role, VirtualList};

use super::elements::MAX_ROWS;
use super::error::BindErrorKind;
use super::style::TextCtx;
use super::{Binder, Cx, R, Var};
use crate::ast::{Node as AstNode, Val};
use crate::expr::{Expr, Template};
use crate::source::Span;

/// How many rows beyond what is in view are bound, on each side.
pub const OVERSCAN: u32 = 3;

/// The most items a virtualised list has: beyond it, positions in px stop being exact.
pub const MAX_ITEMS: usize = 100_000;

/// What the app knows of the lists on the screen, for [`bind_windowed`].
///
/// [`bind_windowed`]: crate::SkinPackage::bind_windowed
#[derive(Debug, Clone, Default)]
pub struct Lists {
    /// The height a list not laid out yet is taken to be seen at: the window's.
    pub height: f32,
    /// The rows in view of each list laid out before, by the list's key.
    pub shown: HashMap<String, Range<u32>>,
    /// The key of the focused control, whose item and its neighbours stay bound.
    pub focus: Option<String>,
}

/// Which rows of one list to bind, once its length is known.
pub(crate) struct Window {
    shown: Range<u32>,
    focus: Option<u32>,
    columns: u32,
}

/// The items of a list that are bound.
pub(crate) struct Picked {
    /// The length of the whole list.
    pub count: u32,
    /// `(index, item)`, in order.
    pub items: Vec<(u32, Value)>,
    /// The rows around what is in view, for a virtualised list.
    pub window: Option<Range<u32>>,
}

impl Picked {
    /// A list bound whole.
    pub(crate) fn all(items: Vec<Value>) -> Picked {
        Picked {
            count: items.len() as u32,
            items: (0..).zip(items).collect(),
            window: None,
        }
    }
}

impl Window {
    /// The window and the rows to bind of a list of `rows` rows: those in view and a few
    /// each side, the first and the last (so Tab enters at the right end), and those by
    /// the focused item.
    fn rows(&self, rows: u32) -> (Range<u32>, BTreeSet<u32>) {
        let most = (MAX_ROWS as u32).div_ceil(self.columns);
        let start = self.shown.start.saturating_sub(OVERSCAN).min(rows);
        let end = self
            .shown
            .end
            .saturating_add(OVERSCAN)
            .min(rows)
            .min(start.saturating_add(most));
        let mut bound: BTreeSet<u32> = (start..end).collect();
        if rows > 0 {
            bound.extend([0, rows - 1]);
        }
        if let Some(f) = self.focus {
            bound.extend((f.saturating_sub(1)..=f.saturating_add(1)).filter(|&r| r < rows));
        }
        (start..end, bound)
    }
}

impl<'a> Binder<'a> {
    /// Whether a list here is virtualised: the app asks for it, it is inside a scroll
    /// container, and not inside another list's item.
    pub(super) fn virtualises(&self) -> bool {
        self.lists.is_some() && self.scrolls > 0 && self.in_item == 0
    }

    /// What the app asks of the list at `key`, whose items' keys start with `item`.
    pub(super) fn window(
        &self,
        key: &str,
        item: &str,
        columns: u32,
        item_h: f32,
        gap: f32,
    ) -> Window {
        let lists = self.lists;
        let shown = lists
            .and_then(|l| l.shown.get(key).cloned())
            .unwrap_or_else(|| {
                let height = lists.map_or(0.0, |l| l.height);
                // `as` saturates.
                0..(height / (item_h + gap)).ceil() as u32
            });
        let focus = lists
            .and_then(|l| l.focus.as_deref())
            .and_then(|f| f.strip_prefix(item))
            .and_then(|rest| rest.split('/').next())
            .and_then(|i| i.parse::<u32>().ok())
            .map(|i| i / columns);
        Window {
            shown,
            focus,
            columns,
        }
    }

    /// The items `window` asks for of the list `v` evaluates to. Only those are copied
    /// out of the data.
    pub(super) fn picked(
        &self,
        v: &Val,
        at: Span,
        what: &'static str,
        window: Window,
    ) -> R<Picked> {
        let Val::Text(t) = v else {
            return Ok(Picked::all(self.items(v, at, what)?));
        };
        let sc = self.scope();
        let found = t.eval(&sc).map_err(|e| self.fail(e.into(), Some(at)))?;
        let list = match &*found {
            Value::List(l) => l.as_slice(),
            Value::Null => &[],
            other => {
                return Err(self.fail(
                    BindErrorKind::NotAList {
                        what,
                        got: other.type_name(),
                    },
                    Some(at),
                ));
            }
        };
        let count = list.len().min(MAX_ITEMS) as u32;
        let columns = window.columns;
        let (rows, bound) = window.rows(count.div_ceil(columns));
        let items = bound
            .into_iter()
            .flat_map(|r| r * columns..(r * columns + columns).min(count))
            .map(|i| (i, list[i as usize].clone()))
            .collect();
        Ok(Picked {
            count,
            items,
            window: Some(rows),
        })
    }

    /// The name of the app's data `v` is nothing but, if it is.
    pub(super) fn data_name(&self, v: &Val) -> Option<String> {
        let Val::Text(Template::Expr(Expr::Name { name, .. })) = v else {
            return None;
        };
        let hidden = name == "ui" || self.vars.iter().any(|var| &var.name == name);
        (!hidden && self.b.scope.var(name).is_some()).then(|| name.clone())
    }

    /// The value a variable stands for.
    pub(super) fn var_value<'v>(&'v self, var: &'v Var) -> Option<&'v Value> {
        match &var.alias {
            Some(source) => self.b.scope.var(source),
            None => Some(&var.value),
        }
    }

    /// Whether `v` counts as true, without copying what it names.
    pub(super) fn truthy(&self, v: &Val, at: Span) -> R<bool> {
        if let Val::Text(t) = v {
            let sc = self.scope();
            return t
                .eval(&sc)
                .map(|found| found.truthy())
                .map_err(|e| self.fail(e.into(), Some(at)));
        }
        Ok(self.val(v, at)?.truthy())
    }

    /// An empty box where rows are left out.
    pub(super) fn spacer(&mut self, parent: NodeId, key: &str, h: f32, at: Span) -> R<()> {
        let mut node = Node::container();
        node.layout.height = Dim::Px(h);
        node.layout.shrink = 0.0;
        self.add(parent, key, node, at)?;
        Ok(())
    }

    /// `list "g" in="{games}" columns=4 item-h=300`: the items in rows of equal height.
    pub(super) fn list(&mut self, n: &'a AstNode, parent: NodeId, key: &str, cx: &Cx) -> R<NodeId> {
        let at = n.span;
        if self.in_item > 0 {
            return Err(self.fail(
                BindErrorKind::Missing("a `list` can't be inside another list's item".to_owned()),
                Some(at),
            ));
        }
        let mut c = self.compute("list", Some(n), &[], cx, |_| {})?;
        let columns = (c.layout.columns.len() as u32).max(1);
        let (gap, column_gap) = (c.layout.row_gap, c.layout.column_gap);
        c.layout.columns = Vec::new();
        c.layout.direction = Direction::Column;
        c.layout.row_gap = 0.0;
        c.layout.shrink = 0.0;
        let item_h = match self.prop(n, "item-h")? {
            Some(Value::Num(h)) if (1.0..=10_000.0).contains(&h) => h as f32,
            _ => {
                return Err(self.property("item-h", "a height from 1 to 10000 px", at));
            }
        };
        let (Some(var), Some(p)) = (n.plain_arg(0), n.prop("in")) else {
            return self.add(parent, key, Self::node_of(&c), at);
        };
        let item_key = format!("{key}/i#");
        let virtualised = self.virtualises();
        let picked = if virtualised {
            let window = self.window(key, &item_key, columns, item_h, gap);
            self.picked(&p.arg.val, p.arg.span, "list", window)?
        } else {
            Picked::all(self.items(&p.arg.val, p.arg.span, "list")?)
        };
        let mut node = Self::node_of(&c);
        node.a11y.role = Role::List;
        node.a11y.name = self.prop_text(n, "label")?;
        node.list = picked.window.clone().map(|window| VirtualList {
            count: picked.count,
            columns,
            item_h,
            gap,
            window,
        });
        let id = self.add(parent, key, node, at)?;

        let rows = picked.count.div_ceil(columns);
        let pitch = item_h + gap;
        let mut next = 0;
        let mut items = picked.items.into_iter().peekable();
        while let Some(&(first, _)) = items.peek() {
            let r = first / columns;
            if r > next {
                // Each row left out takes its gap with it.
                self.spacer(
                    id,
                    &format!("{key}/s#{next}"),
                    (r - next) as f32 * pitch,
                    at,
                )?;
            }
            next = r + 1;
            let mut row = Node::container();
            row.layout.direction = Direction::Row;
            row.layout.column_gap = column_gap;
            row.layout.height = Dim::Px(item_h);
            row.layout.shrink = 0.0;
            if r + 1 < rows {
                row.layout.margin.bottom = gap;
            }
            let rid = self.add(id, &format!("{key}/r#{r}"), row, at)?;
            for col in 0..columns {
                let item = items.next_if(|&(i, _)| i == r * columns + col);
                let mut cell = Node::container();
                cell.layout.direction = Direction::Column;
                cell.layout.grow = 1.0;
                cell.layout.width = Dim::Px(0.0);
                cell.layout.min_width = Dim::Px(0.0);
                let Some((i, item)) = item else {
                    // The last row keeps the columns' widths.
                    self.add(rid, &format!("{key}/e#{col}"), cell, at)?;
                    continue;
                };
                self.step(at)?;
                cell.a11y.role = Role::ListItem;
                cell.item = virtualised.then_some(i);
                let path = format!("{item_key}{i}");
                let cell = self.add(rid, &path, cell, at)?;
                let mark = self.vars.len();
                self.vars.push(Var::plain(var, item));
                let inner = Cx {
                    text: c.text.clone(),
                    field: cx.field.clone(),
                };
                self.in_item += 1;
                let bound = self.children(&n.children, cell, &path, &inner);
                self.in_item -= 1;
                self.vars.truncate(mark);
                bound?;
            }
        }
        Ok(id)
    }

    /// The fixed height a skin gives the rows of its tables, if it does.
    pub(super) fn row_height(&self, text: &TextCtx) -> R<Option<f32>> {
        let layers = self.layers("table-row", None, &[])?;
        let c = self.compute_with(&layers, None, false, &[], text, |_| {})?;
        Ok(match c.layout.height {
            Dim::Px(h) if h >= 1.0 => Some(h),
            _ => None,
        })
    }
}
