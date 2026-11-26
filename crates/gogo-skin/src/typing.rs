//! Names, field paths and types in expressions (design 27 §7, "Names" and "What the
//! loader checks"; design 26 S16).
//!
//! The walk resolves each name once and keeps what it found in the tree: a source call
//! and a bare source name hold their `&'static` [`SourceSpec`]. It types an expression
//! only as far as it is certain: a type it can't tell is `None` and stops the check of
//! that path without an error (full inference is planned, #564).

use gogo_contract::{FieldType, Os, SourceSpec, Status, Value, shapes, sources};

use crate::ast::{Arg, Term};
use crate::diag::Code;
use crate::expr::{BinOp, Expr, Seg, Stage, Template};
use crate::manifest::UiType;
use crate::names::suggest;
use crate::resolve::Checker;
use crate::source::Span;

/// What is known of a value at one point of the walk.
///
/// This is a load-time approximation, not the value's type at run time: binding (design
/// 27 §9, B4) must not rely on `ft` being present or exact. `None` means "not known",
/// never "any", and every flag below defaults to the safe reading when it is unset.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Ty {
    /// The contract type, when it is known. A value whose type depends on the data
    /// (a `let` of an unknown source, a parameter nobody gave) has none, and is not checked.
    pub ft: Option<FieldType>,
    /// Derived from a component parameter, so a problem with it is reported at the
    /// argument that gave the parameter its type. Set only on a parameter's own entry in
    /// an instance walk; a `let` or `each` of the same name starts over without it.
    pub param: bool,
    /// A plain field path (possibly with `?.` and `else`), not a computed value: only
    /// these are checked against an action parameter's type, because a computed value's
    /// type here is a guess. Binding evaluates the value either way.
    pub plain: bool,
    /// The argument at the `use` that gave the parameter its type.
    pub origin: Option<Span>,
    /// A component parameter: a `use` may leave it out, so it may be null. A required
    /// action parameter given a nullable value passes the load check and can still fail
    /// at bind time (design 27 §7, "Failures").
    pub nullable: bool,
}

impl Ty {
    pub fn of(ft: FieldType) -> Ty {
        Ty {
            ft: Some(ft),
            ..Ty::default()
        }
    }

    pub fn text() -> Ty {
        Ty {
            plain: true,
            ..Ty::of(FieldType::Text)
        }
    }

    pub fn not_plain(self) -> Ty {
        Ty {
            plain: false,
            ..self
        }
    }

    /// The same provenance with another type.
    pub fn with(self, ft: Option<FieldType>) -> Ty {
        Ty { ft, ..self }
    }

    fn computed(ft: Option<FieldType>) -> Ty {
        Ty {
            ft,
            ..Ty::default()
        }
    }
}

/// Where the holes of one string are in its file.
pub(crate) struct Loc {
    /// The whole value, for a finding that can't point inside it.
    whole: Span,
    /// The file offset of the first byte of the template text, if it is as written.
    base: Option<usize>,
    /// The offset of each hole's expression, from `base`.
    holes: Vec<usize>,
}

impl Loc {
    fn span(&self, hole: usize, s: crate::expr::Span) -> Span {
        match (self.base, self.holes.get(hole)) {
            (Some(b), Some(h)) => Span::new(self.whole.file, b + h + s.offset, s.len),
            _ => self.whole,
        }
    }
}

/// The offset of each `{…}` hole's expression in `s`, scanned as `parse_template` does.
fn hole_starts(s: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        match c {
            '{' | '}' if matches!(it.peek(), Some(&(_, d)) if d == c) => {
                it.next();
            }
            '{' => {
                out.push(i + 1);
                let mut quote: Option<char> = None;
                for (_, d) in it.by_ref() {
                    match (quote, d) {
                        (Some(q), d) if d == q => quote = None,
                        (Some(_), _) => {}
                        (None, '\'' | '"') => quote = Some(d),
                        (None, '}') => break,
                        (None, _) => {}
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// The doc's name for a type, for a message.
pub(crate) fn type_name(t: &FieldType) -> String {
    use FieldType as F;
    match t {
        F::Text => "Text".into(),
        F::Number => "Number".into(),
        F::Bool => "Bool".into(),
        F::Bytes => "Bytes".into(),
        F::Time => "Time".into(),
        F::Duration => "Duration".into(),
        F::Fraction => "Fraction".into(),
        F::GameId => "GameId".into(),
        F::JobId => "JobId".into(),
        F::Image => "Image".into(),
        F::Color => "Color".into(),
        F::Status => "Status".into(),
        F::Os => "Os".into(),
        F::RichText => "RichText".into(),
        F::ActionRef => "ActionRef".into(),
        F::Scalar => "Scalar".into(),
        F::OneOf(w) => format!("one of {}", w.join(" | ")),
        F::Masked(t) => format!("Masked<{}>", type_name(t)),
        F::List(t) => format!("List<{}>", type_name(t)),
        F::Option(t) => format!("Option<{}>", type_name(t)),
        F::Record(s) => s.name.to_owned(),
        F::Ui => "the skin's `ui` state".into(),
        F::Opaque(w) => format!("a value only `{w}` draws"),
    }
}

/// The formatters and status helpers, resolved by name at evaluation (design 26 §2).
const HELPERS: &[&str] = &[
    "fmt.bytes",
    "fmt.rate",
    "fmt.hours",
    "fmt.pct",
    "fmt.date",
    "fmt.eta",
    "fmt.ago",
    "status.word",
    "status.tone",
];

/// A source with no required parameter: it can be named bare.
fn bare_source(name: &str) -> Option<&'static SourceSpec> {
    sources::source(name).filter(|s| {
        s.widget_only.is_none() && s.params.iter().all(|p| p.optional) && s.name != "plan.preview"
    })
}

enum Step {
    Found(FieldType),
    Missing(Vec<String>),
    NoFields,
}

impl Checker<'_> {
    pub fn loc_arg(&self, a: &Arg) -> Loc {
        let base = a.inner.map(|i| a.span.offset + i);
        self.loc(a.span, base)
    }

    pub fn loc_term(&self, t: &Term) -> Loc {
        let base = t
            .exact
            .then(|| t.span.offset + t.name.as_ref().map_or(0, |n| n.len() + 1));
        self.loc(t.span, base)
    }

    fn loc(&self, whole: Span, base: Option<usize>) -> Loc {
        let holes = base
            .and_then(|b| {
                let f = self.col.sources.get(whole.file)?;
                let end = whole.offset.checked_add(whole.len)?;
                f.text.get(b..end).map(hole_starts)
            })
            .unwrap_or_default();
        Loc { whole, base, holes }
    }

    /// Check every expression in a template; the type is that of a lone expression, else
    /// text.
    pub fn template(&mut self, t: &mut Template, loc: &Loc) -> Ty {
        match t {
            Template::Text(_) => Ty::text(),
            Template::Expr(e) => self.expr(e, loc, 0),
            Template::Interp(segs) => {
                let mut k = 0;
                for s in segs {
                    if let Seg::Expr(e) = s {
                        self.expr(e, loc, k);
                        k += 1;
                    }
                }
                Ty::computed(Some(FieldType::Text))
            }
        }
    }

    pub fn expr(&mut self, e: &mut Expr, loc: &Loc, k: usize) -> Ty {
        match e {
            Expr::Lit(v) => {
                use gogo_contract::Value as V;
                Ty::computed(match v {
                    V::Str(_) => Some(FieldType::Text),
                    V::Num(_) => Some(FieldType::Number),
                    V::Bool(_) => Some(FieldType::Bool),
                    _ => None,
                })
            }
            Expr::Param(_) => Ty::default(),
            Expr::Token { alpha, .. } => {
                if let Some(a) = alpha {
                    self.expr(a, loc, k);
                }
                Ty::default()
            }
            Expr::Name { .. } => self.name(e, loc, k, false),
            // Checked again for a fragment borrowed into a skin with other words.
            Expr::Word { name, span } => {
                if !self.words.contains(name.as_str()) {
                    let at = loc.span(k, *span);
                    self.unknown_name(&name.clone(), at);
                }
                Ty::text()
            }
            Expr::Field { .. } => self.field(e, loc, k),
            Expr::Call { .. } => self.call(e, loc, k),
            Expr::Not(a) => {
                self.expr(a, loc, k);
                Ty::computed(Some(FieldType::Bool))
            }
            Expr::Neg(a) => {
                self.expr(a, loc, k);
                Ty::computed(Some(FieldType::Number))
            }
            Expr::Bin(op, a, b) => {
                let equality = matches!(op, BinOp::Eq | BinOp::Ne);
                if equality {
                    self.word_operand(a, loc, k);
                    self.word_operand(b, loc, k);
                }
                let (ta, tb) = (self.expr(a, loc, k), self.expr(b, loc, k));
                if equality {
                    self.compared(a, b, tb, loc);
                    self.compared(b, a, ta, loc);
                }
                Ty::computed(Some(FieldType::Bool))
            }
            Expr::Ternary(c, a, b) => {
                self.expr(c, loc, k);
                let (ta, tb) = (self.expr(a, loc, k), self.expr(b, loc, k));
                Ty::computed(match (ta.ft, tb.ft) {
                    (Some(x), Some(y)) if x == y => Some(x),
                    _ => None,
                })
            }
            Expr::Else(a, b) => {
                let (ta, tb) = (self.expr(a, loc, k), self.expr(b, loc, k));
                match ta.ft {
                    Some(t) => Ty {
                        ft: Some(*t.inner()),
                        ..ta
                    },
                    None => Ty { plain: false, ..tb },
                }
            }
            Expr::Pipe(..) => self.pipe(e, loc, k),
        }
    }

    // ---- comparisons ----

    /// An operand of `==` or `!=` that is a bare enum word is that word, never the source
    /// of the same name (design 27 §7, "Names"). A variable of the name still wins. The
    /// tree keeps the word as a text literal, so evaluation never asks the scope for it.
    fn word_operand(&mut self, e: &mut Expr, loc: &Loc, k: usize) {
        let Expr::Name {
            name,
            span,
            source: None,
        } = e
        else {
            return;
        };
        if !self.words.contains(name.as_str()) {
            return;
        }
        match self.lookup(name) {
            (Some(_), _) => {}
            (None, false) => {
                *e = Expr::Word {
                    name: name.clone(),
                    span: *span,
                };
            }
            // A `where` over items of unknown type: the name may be one of their fields.
            // Only a word that is also a source could then be read two ways.
            (None, true) if bare_source(name).is_some() => {
                let at = loc.span(k, *span);
                let msg = format!("`{name}` may be a field, a source or the word here");
                let help = format!("write the word as `'{name}'`");
                self.emit(None, Code::UnknownName, at, msg, Some(help));
            }
            (None, true) => {}
        }
    }

    /// Check a text literal compared with a value of a known type: the value must be able
    /// to equal it.
    fn compared(&mut self, lit: &Expr, other: &Expr, ty: Ty, loc: &Loc) {
        let word = match lit {
            Expr::Lit(Value::Str(word)) => word,
            Expr::Word { name, .. } if self.words.contains(name.as_str()) => name,
            _ => return,
        };
        let Some(ft) = ty.ft else {
            return;
        };
        let ui_words = self.ui_words(other);
        let msg = match ft.inner() {
            _ if ui_words.is_some_and(|w| !w.iter().any(|w| w == word)) => {
                format!("`{word}` is no value of that `ui` variable")
            }
            FieldType::OneOf(words) if !words.contains(&word.as_str()) => {
                format!("{} is never `{word}`", type_name(&ft))
            }
            FieldType::Status if !Status::ALL.iter().any(|s| s.name() == word) => {
                format!("`{word}` is no status")
            }
            FieldType::Os if !Os::ALL.iter().any(|o| o.name() == word) => {
                format!("`{word}` is no OS")
            }
            FieldType::List(_)
            | FieldType::Bool
            | FieldType::Number
            | FieldType::Bytes
            | FieldType::Time
            | FieldType::Duration
            | FieldType::Fraction => format!("{} is never the text `{word}`", type_name(&ft)),
            _ => return,
        };
        self.emit(Some(&ty), Code::TypeMismatch, loc.whole, msg, None);
    }

    /// The words of the `enum` variable `e` reads, if it is `ui.<name>`.
    fn ui_words(&self, e: &Expr) -> Option<&[String]> {
        let Expr::Field { base, name, .. } = e else {
            return None;
        };
        if !matches!(&**base, Expr::Name { name, .. } if name == "ui") {
            return None;
        }
        match &self.ui_var(name)?.ty {
            UiType::Enum(words) => Some(words),
            _ => None,
        }
    }

    // ---- names ----

    /// A bare name: a variable, a source, an enum word, or an error.
    fn name(&mut self, e: &mut Expr, loc: &Loc, k: usize, root: bool) -> Ty {
        let Expr::Name { name, span, source } = e else {
            return Ty::default();
        };
        let (found, loose) = self.lookup(name);
        if let Some(ty) = found {
            return ty;
        }
        if let Some(s) = bare_source(name) {
            *source = Some(s);
            return source_ty(s.name);
        }
        if (!root && self.words.contains(name.as_str())) || loose {
            return Ty::default();
        }
        let at = loc.span(k, *span);
        let name = name.clone();
        self.unknown_name(&name, at);
        Ty::default()
    }

    fn unknown_name(&mut self, name: &str, at: Span) {
        let mut cands: Vec<&str> = self.env.iter().map(|(n, _)| n.as_str()).collect();
        cands.extend(sources::SOURCES.iter().map(|s| s.name));
        cands.extend(self.words.iter().map(String::as_str));
        let help = if sources::source(name).is_some() {
            format!("`{name}` takes arguments: write `{name}(…)`")
        } else {
            match suggest(name, cands) {
                Some(s) => format!("did you mean `{s}`?"),
                None => "a name is a `let`, a component parameter, an `each` variable, a \
                         source, or a word of the contract such as `ready` (design 27 §7)"
                    .to_owned(),
            }
        };
        self.emit(
            None,
            Code::UnknownName,
            at,
            format!("unknown name `{name}`"),
            Some(help),
        );
    }

    /// The variable called `name`: a field of a `where` item, else a bound variable. The
    /// flag says a `where` item of unknown type was passed, which could have had it.
    fn lookup(&self, name: &str) -> (Option<Ty>, bool) {
        let mut loose = false;
        for fr in self.items.iter().rev() {
            match fr.ft {
                None => loose = true,
                Some(ft) => {
                    if let Some(f) = ft.field(name) {
                        return (
                            Some(Ty {
                                ft: Some(f.ty),
                                plain: true,
                                ..*fr
                            }),
                            loose,
                        );
                    }
                }
            }
        }
        match self.env.iter().rev().find(|(n, _)| n == name) {
            Some((_, ty)) => (Some(*ty), loose),
            None => (None, loose),
        }
    }

    // ---- paths ----

    fn field(&mut self, e: &mut Expr, loc: &Loc, k: usize) -> Ty {
        // `library.stats`, `queue.summary`: a source whose name has a dot is read as a
        // name and a field, and merged back into the source here.
        if let Expr::Field {
            base,
            name,
            optional: false,
            span,
        } = &*e
            && let Expr::Name {
                name: root,
                span: rspan,
                source: None,
            } = &**base
            && self.lookup(root).0.is_none()
            && let Some(src) = bare_source(&format!("{root}.{name}"))
        {
            let merged = rspan.to(*span);
            *e = Expr::Name {
                name: src.name.to_owned(),
                span: merged,
                source: Some(src),
            };
            return source_ty(src.name);
        }
        let Expr::Field {
            base, name, span, ..
        } = e
        else {
            return Ty::default();
        };
        let bt = if matches!(**base, Expr::Name { .. }) {
            self.name(base, loc, k, true)
        } else {
            self.expr(base, loc, k)
        };
        let Some(ft) = bt.ft else {
            return Ty {
                ft: None,
                plain: false,
                ..bt
            };
        };
        match self.step(&ft, name) {
            Step::Found(t) => Ty {
                ft: Some(t),
                plain: true,
                ..bt
            },
            Step::Missing(known) => {
                let at = loc.span(k, *span);
                let help = match suggest(name, known.iter().map(String::as_str)) {
                    Some(s) => format!("did you mean `{s}`?"),
                    None => format!("the fields are {}", known.join(", ")),
                };
                let msg = format!("{} has no field `{name}` (S16)", type_name(&ft));
                self.emit(Some(&bt), Code::UnknownField, at, msg, Some(help));
                Ty {
                    ft: None,
                    plain: false,
                    ..bt
                }
            }
            Step::NoFields => {
                let at = loc.span(k, *span);
                let msg = format!("{} has no fields, so not `{name}` (S16)", type_name(&ft));
                self.emit(Some(&bt), Code::UnknownField, at, msg, None);
                Ty {
                    ft: None,
                    plain: false,
                    ..bt
                }
            }
        }
    }

    /// The field `name` of a value of type `ft`.
    fn step(&self, ft: &FieldType, name: &str) -> Step {
        if matches!(ft.inner(), FieldType::Ui) {
            return match self.ui_var(name) {
                Some(v) => Step::Found(match &v.ty {
                    UiType::Bool => FieldType::Bool,
                    UiType::Number => FieldType::Number,
                    UiType::Enum(_) | UiType::Section => FieldType::Text,
                }),
                None => Step::Missing(self.manifest.ui.iter().map(|v| v.name.clone()).collect()),
            };
        }
        match ft.inner() {
            FieldType::Record(s) => match s.field(name) {
                Some(f) => Step::Found(f.ty),
                None => Step::Missing(s.all_fields().iter().map(|f| f.name.to_owned()).collect()),
            },
            _ => Step::NoFields,
        }
    }

    // ---- calls ----

    fn call(&mut self, e: &mut Expr, loc: &Loc, k: usize) -> Ty {
        let Expr::Call {
            name,
            args,
            span,
            source,
        } = e
        else {
            return Ty::default();
        };
        for a in args.iter_mut() {
            self.expr(a, loc, k);
        }
        if HELPERS.contains(&name.as_str()) {
            return Ty::computed(Some(FieldType::Text));
        }
        let at = loc.span(k, *span);
        let Some(spec) = sources::source(name) else {
            let cands = sources::SOURCES
                .iter()
                .map(|s| s.name)
                .chain(HELPERS.iter().copied());
            let help = match suggest(name, cands) {
                Some(s) => format!("did you mean `{s}`?"),
                None => "design 26 §3 lists the sources and §2 the formatters".to_owned(),
            };
            let msg = format!("unknown source or helper `{name}` (S16)");
            self.emit(None, Code::UnknownSource, at, msg, Some(help));
            return Ty::default();
        };
        if let Some(w) = spec.widget_only {
            let msg = format!("`{name}` reaches a skin only through `{w}`, never as data (S16)");
            self.emit(None, Code::UnknownSource, at, msg, None);
            return Ty::default();
        }
        let need = spec.params.iter().filter(|p| !p.optional).count();
        if args.len() < need || args.len() > spec.params.len() {
            let names: Vec<&str> = spec.params.iter().map(|p| p.name).collect();
            let msg = format!(
                "`{name}` takes {} argument{}, found {} (S16)",
                if need == spec.params.len() {
                    need.to_string()
                } else {
                    format!("{need} to {}", spec.params.len())
                },
                if spec.params.len() == 1 { "" } else { "s" },
                args.len()
            );
            let help = format!("`{name}({})`", names.join(", "));
            self.emit(None, Code::SourceArgs, at, msg, Some(help));
        }
        *source = Some(spec);
        source_ty(spec.name)
    }

    // ---- pipes ----

    fn pipe(&mut self, e: &mut Expr, loc: &Loc, k: usize) -> Ty {
        let Expr::Pipe(base, stage) = e else {
            return Ty::default();
        };
        let bt = self.expr(base, loc, k);
        let item_ref: Option<&'static FieldType> = bt.ft.and_then(|f| f.item());
        let item = item_ref.copied();
        match stage {
            Stage::Where(p) => {
                self.items.push(Ty {
                    ft: item,
                    plain: false,
                    ..bt
                });
                self.expr(p, loc, k);
                self.items.pop();
                bt.not_plain()
            }
            Stage::Take(x) | Stage::Before(x) | Stage::After(x) => {
                self.expr(x, loc, k);
                bt.not_plain()
            }
            Stage::Trim(x) => {
                self.expr(x, loc, k);
                Ty::computed(Some(FieldType::Text))
            }
            Stage::Sort { key, .. } => {
                self.sort_key(key, item, &bt, loc);
                bt.not_plain()
            }
            Stage::Len => Ty::computed(Some(FieldType::Number)),
            Stage::First => Ty {
                ft: item_ref.map(FieldType::Option),
                plain: false,
                ..bt
            },
            Stage::Upper | Stage::Lower => Ty::computed(Some(FieldType::Text)),
        }
    }

    /// `sort a.b`: the path names fields of the list's items. A key carries no span of
    /// its own, so a mistake points at the whole value.
    fn sort_key(&mut self, key: &[String], item: Option<FieldType>, bt: &Ty, loc: &Loc) {
        let mut cur = item;
        for seg in key {
            let Some(ft) = cur else { return };
            match self.step(&ft, seg) {
                Step::Found(t) => cur = Some(t),
                Step::Missing(known) => {
                    let help = match suggest(seg, known.iter().map(String::as_str)) {
                        Some(s) => format!("did you mean `{s}`?"),
                        None => format!("the fields are {}", known.join(", ")),
                    };
                    let msg = format!("{} has no field `{seg}` to sort by (S16)", type_name(&ft));
                    self.emit(Some(bt), Code::UnknownField, loc.whole, msg, Some(help));
                    return;
                }
                Step::NoFields => {
                    let msg = format!("{} has no fields, so not `{seg}` (S16)", type_name(&ft));
                    self.emit(Some(bt), Code::UnknownField, loc.whole, msg, None);
                    return;
                }
            }
        }
    }
}

/// The type a source gives.
fn source_ty(name: &str) -> Ty {
    Ty {
        plain: true,
        ..Ty::computed(shapes::source_shape(name))
    }
}
