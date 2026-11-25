//! Elements and their properties (design 27 §5): which elements exist, what each takes,
//! and where it may sit.
//!
//! The checks here are structural. Resolving names against the contract (an action's
//! parameters, a route's sections, a widget's parts and variants, field paths and their
//! types, a component parameter's type) is PR B3c: this pass keeps names as strings with
//! spans in the tree, and B3c adds the resolution on top.

use std::collections::BTreeMap;

use gogo_contract::actions::{INPUT_ELEMENTS, INPUT_VARS};
use gogo_contract::widgets::{Depth, WIDGETS};

use crate::assets::{Asset, AssetKind};
use crate::ast::{Node, Prop, Val};
use crate::diag::{Code, Collector};
use crate::expr::Template;
use crate::icons;
use crate::names::{is_element_id, is_ident, suggest};
use crate::props::{self, Ctx, Dynamic, Facts};
use crate::source::Span;
use crate::styles::Style;
use crate::tokens::TokenSet;

/// How many arguments an element takes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Args {
    None,
    One,
    ZeroOrOne,
    OneOrTwo,
}

/// What an element may hold.
#[derive(Clone, Copy)]
enum Kids {
    None,
    Any,
    Only(&'static [&'static str]),
}

struct Spec {
    name: &'static str,
    args: Args,
    /// Properties of this element beyond the common ones and the style properties.
    props: &'static [&'static str],
    /// Properties the element can't do without.
    required: &'static [&'static str],
    kids: Kids,
    /// May carry style properties inline.
    styled: bool,
}

const fn el(name: &'static str, args: Args, props: &'static [&'static str], kids: Kids) -> Spec {
    Spec {
        name,
        args,
        props,
        required: &[],
        kids,
        styled: true,
    }
}

const fn req(mut s: Spec, required: &'static [&'static str]) -> Spec {
    s.required = required;
    s
}

/// Properties any element may carry.
const COMMON: &[&str] = &["id", "style", "when", "label"];

static ELEMENTS: &[Spec] = &[
    // 5.1 Layout
    el("column", Args::None, &[], Kids::Any),
    el("box", Args::None, &["progress"], Kids::Any),
    el("form", Args::None, &[], Kids::Any),
    el("row", Args::None, &[], Kids::Any),
    el("grid", Args::None, &[], Kids::Any),
    el("layer", Args::None, &[], Kids::Any),
    el("outlet", Args::None, &[], Kids::None),
    el("spacer", Args::None, &[], Kids::None),
    el("divider", Args::None, &[], Kids::None),
    // 5.2 Content
    el("text", Args::One, &["mono"], Kids::None),
    el("label", Args::One, &["mono"], Kids::None),
    el("icon", Args::One, &[], Kids::None),
    el("art", Args::One, &["fit"], Kids::None),
    req(el("image", Args::None, &["src"], Kids::None), &["src"]),
    el(
        "chip",
        Args::ZeroOrOne,
        &["tone", "icon", "mono"],
        Kids::None,
    ),
    el(
        "badge",
        Args::ZeroOrOne,
        &["tone", "icon", "mono"],
        Kids::None,
    ),
    el("display", Args::One, &["tone", "icon", "mono"], Kids::None),
    el("dot", Args::None, &["on", "tone"], Kids::None),
    el("led", Args::None, &["on", "tone"], Kids::None),
    el(
        "card",
        Args::None,
        &["title", "link", "link-action", "link-args"],
        Kids::Any,
    ),
    el(
        "module",
        Args::None,
        &["title", "link", "link-action", "link-args"],
        Kids::Any,
    ),
    // 5.3 Controls
    el(
        "button",
        Args::ZeroOrOne,
        &["action", "args", "icon", "key"],
        Kids::Any,
    ),
    req(
        el("toggle", Args::None, &["on", "action", "args"], Kids::None),
        &["on"],
    ),
    req(
        el(
            "select",
            Args::None,
            &["value", "options", "action", "args", "mono"],
            Kids::None,
        ),
        &["value", "options"],
    ),
    req(
        el("tabs", Args::None, &["bind", "sections"], Kids::None),
        &["bind", "sections"],
    ),
    el("menu", Args::One, &[], Kids::Only(&["item"])),
    el("field", Args::One, &[], Kids::Any),
    // 5.4 data-table
    req(
        el(
            "data-table",
            Args::None,
            &["rows", "highlight"],
            Kids::Only(&["col"]),
        ),
        &["rows"],
    ),
];

/// Elements that exist only inside one parent, with that parent.
static BOUND: &[(&str, &[&str])] = &[
    ("item", &["menu"]),
    ("col", &["data-table"]),
    ("case", &["switch"]),
    ("else", &["switch"]),
];

const ITEM: Spec = el(
    "item",
    Args::One,
    &["action", "args", "icon", "key"],
    Kids::None,
);
const COL: Spec = el("col", Args::OneOrTwo, &["w", "mono"], Kids::Any);

/// The widget properties besides its variants.
const WIDGET_PROPS: &[&str] = &["for", "args", "placement", "game", "title"];

/// Every element name, for a help line.
fn element_names() -> Vec<&'static str> {
    let mut v: Vec<&str> = ELEMENTS.iter().map(|s| s.name).collect();
    v.extend([
        "let", "each", "switch", "case", "else", "use", "part", "scroll", "item", "col",
    ]);
    v.extend(
        WIDGETS
            .iter()
            .filter(|w| w.depth == Depth::B)
            .map(|w| w.name),
    );
    v
}

/// A component's signature, for `use`.
#[derive(Debug, Clone, Default)]
pub(crate) struct Sig {
    pub params: Vec<String>,
}

/// What the checks look up.
pub(crate) struct World<'a> {
    pub tokens: &'a TokenSet,
    pub tokens_ok: bool,
    pub styles: &'a BTreeMap<String, Style>,
    /// False when `styles.kdl` had errors: an unknown style name is not reported again.
    pub styles_ok: bool,
    pub components: &'a BTreeMap<String, Sig>,
    /// False when a component failed to load, for the same reason.
    pub components_ok: bool,
    pub assets: &'a BTreeMap<String, Asset>,
}

/// Where a tree sits.
#[derive(Clone, Copy)]
pub(crate) struct Place<'a> {
    /// The route name, for a route file.
    pub route: Option<&'a str>,
    /// Inside a component.
    pub component: bool,
}

pub(crate) struct Walk<'a, 'c> {
    pub col: &'c mut Collector,
    pub world: &'a World<'a>,
    pub place: Place<'a>,
    /// `use "<component>"` seen, with where.
    pub uses: Vec<(String, Span)>,
}

#[derive(Clone, Copy)]
struct Parent<'p> {
    name: &'p str,
    in_field: bool,
}

impl Walk<'_, '_> {
    /// Check a list of top-level elements.
    pub fn tree(&mut self, nodes: &[Node]) {
        let root = Parent {
            name: "",
            in_field: false,
        };
        for n in nodes {
            self.node(n, root);
        }
    }

    fn node(&mut self, n: &Node, parent: Parent<'_>) {
        let in_field = parent.in_field || n.name == "field";
        let me = Parent {
            name: &n.name,
            in_field,
        };
        // Elements that only exist inside one parent.
        if let Some((_, allowed)) = BOUND.iter().find(|(name, _)| *name == n.name)
            && !allowed.contains(&parent.name)
        {
            self.col
                .error(
                    Code::ElementPlacement,
                    n.name_span,
                    format!("`{}` only goes inside `{}`", n.name, allowed.join("` or `")),
                )
                .help("design 27 §5 lists where each element goes");
        }
        match n.name.as_str() {
            "let" => self.let_(n),
            "each" => self.each(n, me),
            "switch" => self.switch(n, me),
            "case" | "else" => self.case(n, me),
            "use" => self.use_(n),
            "part" => self.part(n, parent),
            "scroll" => {
                self.col
                    .error(
                        Code::PlannedElement,
                        n.name_span,
                        "`scroll` is planned and not available yet (#545)",
                    )
                    .help("scrolling arrives with its own PR; until then, size the content");
            }
            "item" => self.element(&ITEM, n, me),
            "col" => self.element(&COL, n, me),
            name => {
                if let Some(spec) = ELEMENTS.iter().find(|s| s.name == name) {
                    if name == "outlet" {
                        self.outlet(n);
                    }
                    self.element(spec, n, me);
                } else if let Some(w) = WIDGETS.iter().find(|w| w.name == name) {
                    self.widget(n, w, me);
                } else {
                    self.unknown(n);
                }
            }
        }
    }

    fn unknown(&mut self, n: &Node) {
        let help = if WIDGETS
            .iter()
            .any(|w| w.name == n.name && w.depth == Depth::C)
        {
            format!(
                "`{}` is a stock component: write `use \"{}\"`",
                n.name, n.name
            )
        } else if WIDGETS
            .iter()
            .any(|w| w.name == n.name && w.depth == Depth::A)
        {
            format!(
                "`{}` is an app widget skins reach only through tokens (design 26 §5)",
                n.name
            )
        } else {
            match suggest(&n.name, element_names()) {
                Some(s) => format!("did you mean `{s}`?"),
                None => "design 27 §5 lists the elements".to_owned(),
            }
        };
        self.col
            .error(
                Code::UnknownElement,
                n.name_span,
                format!("unknown element `{}`", n.name),
            )
            .help(help);
    }

    fn outlet(&mut self, n: &Node) {
        if self.place.route != Some("shell") || self.place.component {
            self.col
                .error(
                    Code::ElementPlacement,
                    n.name_span,
                    "`outlet` only goes in the `shell` route",
                )
                .help("the shell draws the current route where its `outlet` is");
        }
    }

    // ---- ordinary elements ----

    fn element(&mut self, spec: &Spec, n: &Node, me: Parent<'_>) {
        self.args(n, spec.args);
        self.props(n, spec.props, spec.styled, spec.name);
        for r in spec.required {
            if n.prop(r).is_none() {
                self.col.error(
                    Code::ElementArguments,
                    n.span,
                    format!("`{}` needs `{r}=`", n.name),
                );
            }
        }
        self.kids(n, spec.kids);
        self.input_scope(n);
        self.name(n, me);
        self.status(n);
        match n.name.as_str() {
            "data-table" if !n.children.iter().any(|c| c.name == "col") => {
                self.col.error(
                    Code::ElementChildren,
                    n.span,
                    "a `data-table` needs at least one `col`",
                );
            }
            "col" => self.col_shape(n),
            _ => {}
        }
        if n.name == "icon"
            && let Some(name) = n.plain_arg(0)
        {
            self.icon_name(name, n.args[0].span);
        }
        for c in &n.children {
            self.node(c, me);
        }
    }

    fn col_shape(&mut self, n: &Node) {
        let two = n.args.len() == 2;
        if two && !n.children.is_empty() {
            self.col.error(
                Code::ElementChildren,
                n.span,
                "a `col` has a cell template or children, not both",
            );
        } else if !two && n.children.is_empty() {
            self.col.error(
                Code::ElementChildren,
                n.span,
                "a `col` needs a cell template (`col \"Title\" \"{row.x}\"`) or children",
            );
        }
    }

    fn icon_name(&mut self, name: &str, span: Span) {
        if !icons::is_icon(name) {
            let help = match suggest(name, icons::ICON_NAMES.iter().copied()) {
                Some(s) => format!("did you mean `{s}`?"),
                None => format!("the icons are {}", icons::ICON_NAMES.join(", ")),
            };
            self.col
                .error(Code::PropertyValue, span, format!("unknown icon `{name}`"))
                .help(help);
        }
    }

    fn args(&mut self, n: &Node, rule: Args) {
        let (lo, hi) = match rule {
            Args::None => (0, 0),
            Args::One => (1, 1),
            Args::ZeroOrOne => (0, 1),
            Args::OneOrTwo => (1, 2),
        };
        if n.args.len() < lo || n.args.len() > hi {
            let want = match rule {
                Args::None => "no arguments",
                Args::One => "one argument",
                Args::ZeroOrOne => "at most one argument",
                Args::OneOrTwo => "one or two arguments",
            };
            self.col.error(
                Code::ElementArguments,
                n.span,
                format!("`{}` takes {want}, found {}", n.name, n.args.len()),
            );
            return;
        }
        for a in &n.args {
            if !matches!(a.val, Val::Text(_) | Val::Invalid) {
                self.col.error(
                    Code::ElementArguments,
                    a.span,
                    format!("`{}` takes text, found {}", n.name, a.val.kind()),
                );
            }
        }
    }

    fn kids(&mut self, n: &Node, kids: Kids) {
        match kids {
            Kids::Any => {}
            Kids::None => {
                if let Some(c) = n.children.first() {
                    self.col.error(
                        Code::ElementChildren,
                        c.span,
                        format!("`{}` has no children", n.name),
                    );
                }
            }
            Kids::Only(names) => {
                for c in n
                    .children
                    .iter()
                    .filter(|c| !names.contains(&c.name.as_str()))
                {
                    self.col.error(
                        Code::ElementChildren,
                        c.name_span,
                        format!(
                            "`{}` holds only `{}`, found `{}`",
                            n.name,
                            names.join("`, `"),
                            c.name
                        ),
                    );
                }
            }
        }
    }

    /// Every property of `n`: the common ones, the element's own, and style properties.
    fn props(&mut self, n: &Node, own: &[&str], styled: bool, what: &str) {
        let ctx = Ctx {
            tokens: self.world.tokens,
            tokens_ok: self.world.tokens_ok,
            dynamic: Dynamic::Any,
        };
        let mut facts = Facts::default();
        let mut seen: Vec<&str> = Vec::new();
        for p in &n.props {
            if seen.contains(&p.name.as_str()) {
                self.col.warn(
                    Code::DuplicateProperty,
                    p.name_span,
                    format!("`{}` is set twice; the last one wins", p.name),
                );
            }
            seen.push(&p.name);
            let name = p.name.as_str();
            if COMMON.contains(&name) || own.contains(&name) {
                self.value(p);
            } else if let Some(spec) = props::style_prop(name).filter(|_| styled) {
                props::check(self.col, &ctx, &mut facts, spec, &[&p.arg], p.arg.span);
            } else {
                let extra: Vec<&str> = COMMON.iter().chain(own).copied().collect();
                let help = props::unknown_help(name, &extra);
                self.col
                    .error(
                        Code::UnknownProperty,
                        p.name_span,
                        format!("`{what}` has no property `{name}`"),
                    )
                    .help(help);
            }
        }
        facts.finish(self.col);
    }

    // ---- values ----

    fn value(&mut self, p: &Prop) {
        let a = &p.arg;
        let bad = |this: &mut Self, code: Code, msg: String| {
            this.col.error(code, a.span, msg);
        };
        if matches!(a.val, Val::Invalid) {
            return;
        }
        let name = p.name.as_str();
        let text = matches!(a.val, Val::Text(_));
        match name {
            "id" => match a.val.plain() {
                Some(s) if is_element_id(s) => {}
                _ => bad(
                    self,
                    Code::PropertyValue,
                    "`id` is plain text: letters, digits and `_-.:`".to_owned(),
                ),
            },
            "style" => self.style_list(a),
            "when" | "on" => {
                if !(a.val.is_expr() || matches!(a.val, Val::Bool(_))) {
                    bad(
                        self,
                        Code::PropertyValue,
                        format!("`{name}` takes one `{{expression}}`"),
                    );
                }
            }
            "value" | "rows" | "highlight" | "progress" => {
                if !a.val.is_expr() {
                    bad(
                        self,
                        Code::PropertyValue,
                        format!("`{name}` takes one `{{expression}}`"),
                    );
                }
            }
            "mono" => {
                if !(a.val.is_expr() || matches!(a.val, Val::Bool(_))) {
                    bad(
                        self,
                        Code::PropertyValue,
                        "`mono` takes `#true`, `#false` or an expression".to_owned(),
                    );
                }
            }
            "hidden" => {
                if !matches!(a.val, Val::Bool(_)) {
                    bad(
                        self,
                        Code::PropertyValue,
                        "`hidden` takes `#true` or `#false`".to_owned(),
                    );
                }
            }
            "action" | "link-action" => {
                match &a.val {
                    Val::Text(_) => {}
                    _ => bad(
                        self,
                        Code::PropertyValue,
                        format!("`{name}` takes an action name"),
                    ),
                }
                if let Some(action) = a.val.plain()
                    && gogo_contract::actions::skin_action(action).is_none()
                {
                    let names = gogo_contract::actions::ACTIONS.iter().map(|x| x.name);
                    let help = match suggest(action, names) {
                        Some(s) => format!("did you mean `{s}`?"),
                        None => "design 26 §4 lists the actions a skin may bind".to_owned(),
                    };
                    self.col
                        .error(
                            Code::UnknownAction,
                            a.span,
                            format!("unknown action `{action}` (S16)"),
                        )
                        .help(help);
                }
            }
            "args" | "link-args" => {
                if !matches!(a.val, Val::Terms(_)) {
                    bad(
                        self,
                        Code::PropertyValue,
                        format!("`{name}` takes the action's arguments as text"),
                    );
                }
            }
            "icon" => {
                if !text {
                    bad(
                        self,
                        Code::PropertyValue,
                        "`icon` takes an icon name".to_owned(),
                    );
                } else if let Some(i) = a.val.plain() {
                    self.icon_name(i, a.span);
                }
            }
            "fit" => {
                if let Some(f) = a.val.plain()
                    && !["cover", "contain", "fill"].contains(&f)
                {
                    bad(
                        self,
                        Code::PropertyValue,
                        "`fit` is `cover`, `contain` or `fill`".to_owned(),
                    );
                }
            }
            "src" => self.src(a),
            "bind" | "sections" => {
                if a.val.plain().is_none_or(|s| s.trim().is_empty()) {
                    bad(
                        self,
                        Code::PropertyValue,
                        format!("`{name}` takes plain text"),
                    );
                }
            }
            "label" | "title" | "link" | "key" | "tone" | "options" | "for" | "placement" => {
                if !text {
                    bad(
                        self,
                        Code::PropertyValue,
                        format!("`{name}` takes text, found {}", a.val.kind()),
                    );
                }
            }
            "w" => {
                // `col w=100`: a length like any other.
                let ctx = Ctx {
                    tokens: self.world.tokens,
                    tokens_ok: self.world.tokens_ok,
                    dynamic: Dynamic::Any,
                };
                if let Some(spec) = props::style_prop("w") {
                    props::check(self.col, &ctx, &mut Facts::default(), spec, &[a], a.span);
                }
            }
            _ => {}
        }
    }

    fn style_list(&mut self, a: &crate::ast::Arg) {
        let Some(list) = a.val.plain() else {
            self.col.error(
                Code::PropertyValue,
                a.span,
                "`style` names styles in plain text",
            );
            return;
        };
        for name in list.split_whitespace() {
            if !self.world.styles.contains_key(name) && self.world.styles_ok {
                let help = match suggest(name, self.world.styles.keys().map(String::as_str)) {
                    Some(s) => format!("did you mean `{s}`?"),
                    None => "styles are defined in styles.kdl".to_owned(),
                };
                self.col
                    .error(
                        Code::UnknownStyle,
                        a.span,
                        format!("unknown style `{name}`"),
                    )
                    .help(help);
            }
        }
    }

    /// `src="assets/x.png"`: relative, inside the skin, and a PNG or SVG the package has.
    fn src(&mut self, a: &crate::ast::Arg) {
        let Some(path) = a.val.plain() else {
            self.col
                .error(
                    Code::AssetDynamic,
                    a.span,
                    "`src` is a plain path, not an expression",
                )
                .help("a skin's images are fixed files in its folder");
            return;
        };
        if let Some(sch) = url_scheme(path) {
            self.col
                .error(
                    Code::UrlScheme,
                    a.span,
                    format!(
                        "`{path}` is a URL (`{sch}:`): skins load nothing from the network (S11)"
                    ),
                )
                .help("copy the file into the skin's `assets/` folder");
            return;
        }
        if let Err(why) = crate::package::check_rel_path(path) {
            self.col
                .error(Code::PathInvalid, a.span, format!("`{path}`: {why} (S10)"));
            return;
        }
        match self.world.assets.get(path) {
            Some(asset) if matches!(asset.kind, AssetKind::Png | AssetKind::Svg) => {}
            Some(_) => {
                self.col
                    .error(
                        Code::AssetMissing,
                        a.span,
                        format!("`{path}` is not an image"),
                    )
                    .help("an image is a PNG or an SVG");
            }
            None => {
                self.col
                    .error(
                        Code::AssetMissing,
                        a.span,
                        format!("`{path}` is not in the skin's assets"),
                    )
                    .help("write the path from the skin root, for example `assets/logo.png`");
            }
        }
    }

    // ---- structure ----

    fn input_scope(&mut self, n: &Node) {
        if INPUT_ELEMENTS.contains(&n.name.as_str()) {
            return;
        }
        for p in &n.props {
            let Val::Terms(terms) = &p.arg.val else {
                continue;
            };
            for t in terms {
                if let Some(v) = INPUT_VARS.iter().find(|v| t.value.mentions(v)) {
                    self.col
                        .error(
                            Code::InputScope,
                            t.span,
                            format!("`{}` has no input, so its args can't use `{v}`", n.name),
                        )
                        .help("only `toggle` and `select` bind `on` and `value`");
                }
            }
        }
    }

    /// S14: a control or an image has a name a screen reader can say.
    fn name(&mut self, n: &Node, me: Parent<'_>) {
        let non_empty = |v: Option<&Val>| match v {
            None | Some(Val::Invalid) => false,
            Some(Val::Text(Template::Text(s))) => !s.trim().is_empty(),
            Some(Val::Text(_)) => true,
            Some(_) => false,
        };
        let prop = |name: &str| n.prop(name).map(|p| &p.arg.val);
        let explicit = non_empty(prop("label")) || non_empty(prop("title"));
        let text = non_empty(n.args.first().map(|a| &a.val));
        let (needs, named, what) = match n.name.as_str() {
            "button" | "item" => (true, explicit || text || me.in_field, "text or `label=`"),
            "toggle" | "select" => (
                true,
                explicit || me.in_field,
                "`label=` or a surrounding `field`",
            ),
            "art" | "image" => (true, explicit || me.in_field, "`label=`"),
            "card" | "module" if n.prop("link-action").is_some() => {
                (true, explicit || non_empty(prop("link")), "`link=` text")
            }
            _ => (false, true, ""),
        };
        if needs && !named {
            self.col
                .error(
                    Code::AccessibleName,
                    n.name_span,
                    format!("`{}` has no accessible name: add {what} (S14)", n.name),
                )
                .help("every control and image is named for screen readers");
        }
    }

    /// S18: a status shown by colour needs its word on the same element.
    fn status(&mut self, n: &Node) {
        for p in &n.props {
            if !matches!(p.name.as_str(), "tone" | "fill" | "color")
                || matches!(p.arg.val, Val::Text(Template::Text(_)))
            {
                continue;
            }
            let Some(src) = self.col.sources.slice(p.arg.span) else {
                continue;
            };
            if !mentions_word(src, "status") {
                continue;
            }
            let whole = self.col.sources.slice(n.span).unwrap_or_default();
            if !whole.contains("status.word(") {
                self.col
                    .warn(
                        Code::StatusByColour,
                        p.arg.span,
                        format!("`{}` colours by status and shows no status word (S18)", n.name),
                    )
                    .help("show `status.word(…)` as text or in `label=`, so colour is never the only cue");
            }
        }
    }

    // ---- control flow ----

    fn let_(&mut self, n: &Node) {
        if !n.args.is_empty() || !n.children.is_empty() {
            self.col.error(
                Code::ElementArguments,
                n.span,
                "`let` takes `name=\"{…}\"` properties only",
            );
        }
        let mut seen: Vec<&str> = Vec::new();
        for p in &n.props {
            if !is_ident(&p.name) {
                self.col
                    .error(
                        Code::PropertyValue,
                        p.name_span,
                        format!("`{}` is not a name `let` can bind", p.name),
                    )
                    .help("use lowercase letters, digits and `_`");
            } else if seen.contains(&p.name.as_str()) {
                self.col.error(
                    Code::PropertyValue,
                    p.name_span,
                    format!("`{}` is bound twice", p.name),
                );
            }
            seen.push(&p.name);
        }
        if n.props.is_empty() {
            self.col.error(
                Code::ElementArguments,
                n.span,
                "`let` binds at least one name",
            );
        }
    }

    /// S20: the only loop is `each` over an expression.
    fn each(&mut self, n: &Node, me: Parent<'_>) {
        match (n.args.as_slice(), n.plain_arg(0)) {
            ([_], Some(v)) if is_ident(v) => {}
            _ => {
                self.col.error(
                    Code::ElementArguments,
                    n.span,
                    "`each` takes one variable name: `each \"s\" in=\"{list}\" { … }`",
                );
            }
        }
        for p in n.props.iter().filter(|p| p.name != "in") {
            self.col.error(
                Code::UnknownProperty,
                p.name_span,
                format!("`each` has `in=`, not `{}`", p.name),
            );
        }
        match n.prop("in") {
            None => {
                self.col
                    .error(
                        Code::EachSource,
                        n.name_span,
                        "`each` needs `in=\"{expression}\"` (S20)",
                    )
                    .help("a skin loops only over a list an expression computes");
            }
            Some(p) if matches!(p.arg.val, Val::Invalid) || p.arg.val.is_expr() => {}
            Some(p) => {
                self.col
                    .error(
                        Code::EachSource,
                        p.arg.span,
                        "`each` loops over one `{expression}`, nothing else (S20)",
                    )
                    .help("write `in=\"{list}\"`");
            }
        }
        for c in &n.children {
            self.node(c, me);
        }
    }

    fn switch(&mut self, n: &Node, me: Parent<'_>) {
        match n.args.as_slice() {
            [a] if matches!(a.val, Val::Text(Template::Expr(_)) | Val::Invalid) => {}
            _ => {
                self.col.error(
                    Code::ElementArguments,
                    n.span,
                    "`switch` takes one `\"{expression}\"`",
                );
            }
        }
        if let Some(p) = n.props.first() {
            self.col.error(
                Code::UnknownProperty,
                p.name_span,
                "`switch` takes no properties",
            );
        }
        let mut seen: Vec<String> = Vec::new();
        let mut else_at: Option<usize> = None;
        for (i, c) in n.children.iter().enumerate() {
            match c.name.as_str() {
                "case" => {
                    if else_at.is_some() {
                        self.col.error(
                            Code::ElementChildren,
                            c.name_span,
                            "`else` is the last branch of a `switch`",
                        );
                    }
                    if let Some(v) = c.plain_arg(0) {
                        if seen.iter().any(|s| s == v) {
                            self.col.warn(
                                Code::DuplicateCase,
                                c.args[0].span,
                                format!("`case \"{v}\"` comes twice; the first one wins"),
                            );
                        }
                        seen.push(v.to_owned());
                    }
                }
                "else" => {
                    if else_at.replace(i).is_some() {
                        self.col.error(
                            Code::ElementChildren,
                            c.name_span,
                            "a `switch` has one `else`",
                        );
                    }
                }
                _ => {
                    self.col.error(
                        Code::ElementChildren,
                        c.name_span,
                        format!("`switch` holds `case` and `else`, found `{}`", c.name),
                    );
                }
            }
        }
        for c in n
            .children
            .iter()
            .filter(|c| matches!(c.name.as_str(), "case" | "else"))
        {
            self.node(c, me);
        }
    }

    /// `case "text" { … }` compares the string with the switch value as text (design 27
    /// §5.8): so a case value is a plain string. `else { … }` takes none.
    fn case(&mut self, n: &Node, me: Parent<'_>) {
        if n.name == "case" {
            match n.args.as_slice() {
                [a] if matches!(a.val, Val::Text(Template::Text(_)) | Val::Invalid) => {}
                _ => {
                    self.col.error(
                        Code::ElementArguments,
                        n.span,
                        "`case` takes one plain string, compared with the value as text",
                    );
                }
            }
        } else if !n.args.is_empty() {
            self.col
                .error(Code::ElementArguments, n.span, "`else` takes no arguments");
        }
        if let Some(p) = n.props.first() {
            self.col.error(
                Code::UnknownProperty,
                p.name_span,
                format!("`{}` takes no properties", n.name),
            );
        }
        for c in &n.children {
            self.node(c, me);
        }
    }

    // ---- components ----

    fn use_(&mut self, n: &Node) {
        let (comp, name_span) = match n.args.as_slice() {
            [a] => (a.val.plain().map(str::to_owned), a.span),
            [d, f] if d.val.plain() == Some("default") => {
                if !matches!(f.val, Val::Text(_) | Val::Invalid) {
                    self.col.error(
                        Code::ElementArguments,
                        f.span,
                        "`use default` takes a fragment name",
                    );
                }
                self.use_props(n, None);
                return;
            }
            _ => {
                self.col.error(
                    Code::ElementArguments,
                    n.span,
                    "`use \"<component>\"` or `use default \"<fragment>\"`",
                );
                return;
            }
        };
        let Some(comp) = comp else {
            if !n.args.iter().any(|a| matches!(a.val, Val::Invalid)) {
                self.col.error(
                    Code::ElementArguments,
                    name_span,
                    "a component name is plain text",
                );
            }
            return;
        };
        if comp == "data-table" {
            self.col
                .error(Code::DataTableUse, name_span, "`data-table` is a built-in element: write `data-table …`, not `use \"data-table\"`")
                .help("see design 27 §5.4");
            return;
        }
        let Some(sig) = self.world.components.get(&comp) else {
            if !self.world.components_ok {
                return;
            }
            let help = match suggest(&comp, self.world.components.keys().map(String::as_str)) {
                Some(s) => format!("did you mean `{s}`?"),
                None => "components are declared in components/*.kdl; copy a stock one to use it"
                    .to_owned(),
            };
            self.col
                .error(
                    Code::UnknownComponent,
                    name_span,
                    format!("the skin has no component `{comp}`"),
                )
                .help(help);
            return;
        };
        self.uses.push((comp, n.span));
        let sig = sig.clone();
        self.use_props(n, Some(&sig));
    }

    fn use_props(&mut self, n: &Node, sig: Option<&Sig>) {
        for p in &n.props {
            match p.name.as_str() {
                "when" | "id" => self.value(p),
                name => {
                    if let Some(sig) = sig
                        && self.world.components_ok
                        && !sig.params.iter().any(|x| x == name)
                    {
                        let help = match suggest(name, sig.params.iter().map(String::as_str)) {
                            Some(s) => format!("did you mean `{s}`?"),
                            None if sig.params.is_empty() => {
                                "this component takes no parameters".to_owned()
                            }
                            None => format!("its parameters are {}", sig.params.join(", ")),
                        };
                        self.col
                            .error(
                                Code::ComponentParam,
                                p.name_span,
                                format!("the component has no parameter `{name}`"),
                            )
                            .help(help);
                    }
                }
            }
        }
        if let Some(c) = n.children.first() {
            self.col
                .error(Code::ElementChildren, c.span, "`use` has no children");
        }
    }

    // ---- app widgets ----

    fn widget(&mut self, n: &Node, w: &gogo_contract::AppWidget, me: Parent<'_>) {
        if w.depth != Depth::B {
            self.unknown(n);
            return;
        }
        self.args(n, Args::None);
        let own: Vec<&str> = WIDGET_PROPS
            .iter()
            .copied()
            .chain(w.variants.iter().map(|v| v.name))
            .collect();
        // Parts and variants are named here and resolved by PR B3c.
        self.props(n, &own, false, w.name);
        self.kids(n, Kids::Only(&["part"]));
        for c in &n.children {
            self.node(c, me);
        }
    }

    fn part(&mut self, n: &Node, parent: Parent<'_>) {
        let widget_parent = WIDGETS
            .iter()
            .any(|w| w.name == parent.name && w.depth == Depth::B);
        if !widget_parent {
            self.col
                .error(
                    Code::ElementPlacement,
                    n.name_span,
                    "`part` only goes inside an app widget that takes parts",
                )
                .help("parts style `plan-card`, `env-table` and the other depth-B widgets (design 26 §5)");
        }
        match n.args.as_slice() {
            [a] if a.val.plain().is_some_and(|s| !s.is_empty()) => {}
            [a] if matches!(a.val, Val::Invalid) => {}
            _ => {
                self.col.error(
                    Code::ElementArguments,
                    n.span,
                    "`part` takes one part name: `part \"frame\" style=\"…\"`",
                );
            }
        }
        for p in &n.props {
            if !matches!(p.name.as_str(), "style" | "hidden")
                && props::style_prop(&p.name).is_none()
            {
                let help = props::unknown_help(&p.name, &["style", "hidden"]);
                self.col
                    .error(
                        Code::UnknownProperty,
                        p.name_span,
                        format!("`part` has no property `{}`", p.name),
                    )
                    .help(help);
            }
        }
        self.props_of_part(n);
        self.kids(n, Kids::None);
    }

    fn props_of_part(&mut self, n: &Node) {
        let ctx = Ctx {
            tokens: self.world.tokens,
            tokens_ok: self.world.tokens_ok,
            dynamic: Dynamic::Any,
        };
        let mut facts = Facts::default();
        for p in &n.props {
            match p.name.as_str() {
                "style" | "hidden" => self.value(p),
                name => {
                    if let Some(spec) = props::style_prop(name) {
                        props::check(self.col, &ctx, &mut facts, spec, &[&p.arg], p.arg.span);
                    }
                }
            }
        }
        facts.finish(self.col);
    }
}

/// The scheme of a URL-like string: `https`, `data`, `file`.
fn url_scheme(s: &str) -> Option<&str> {
    let (scheme, _) = s.split_once(':')?;
    let ok = scheme
        .bytes()
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic())
        && scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'));
    ok.then_some(scheme)
}

/// Whether `word` appears in `s` as a whole identifier (`g.status`, `{status}`).
fn mentions_word(s: &str, word: &str) -> bool {
    let id = |c: char| c.is_alphanumeric() || c == '_';
    s.match_indices(word).any(|(i, _)| {
        let before = s[..i].chars().next_back();
        let after = s[i + word.len()..].chars().next();
        !before.is_some_and(id) && !after.is_some_and(id)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemes_and_words() {
        assert_eq!(url_scheme("https://e.example/x.png"), Some("https"));
        assert_eq!(url_scheme("data:image/png;base64,AA"), Some("data"));
        assert_eq!(url_scheme("assets/x.png"), None);
        assert!(mentions_word("{g.status}", "status"));
        assert!(mentions_word("\"{status}\"", "status"));
        assert!(!mentions_word("{statuses}", "status"));
        assert!(!mentions_word("{my_status}", "status"));
    }
}
