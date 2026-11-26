//! The AST and the recursive-descent parser.

use gogo_contract::{SourceSpec, Value};

use super::error::{Found, ParseError, ParseErrorKind, Span};
use super::lex::{Lexed, Tok, lex};
use super::{MAX_EXPR_DEPTH, MAX_EXPR_LEN};

/// A parsed expression. Build it once with [`parse`]; evaluate it as often as needed.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Lit(Value),
    /// An enum word the loader settled as one: a bare operand of `==` or `!=` (design
    /// 27 §7). The parser never builds it. It is its own text whatever the scope answers.
    Word {
        name: String,
        span: Span,
    },
    /// A bare name: a bound variable, or else an enum word such as `ready`.
    Name {
        name: String,
        /// Where the name is in the expression text.
        span: Span,
        /// The source this name stands for (`ui`, `library.stats`), set by the loader's
        /// contract check. `None` for a variable or an enum word.
        source: Option<&'static SourceSpec>,
    },
    /// A route parameter, `$id`.
    Param(String),
    /// A token, `@accent`, with an optional `alpha=` override.
    Token {
        name: String,
        alpha: Option<Box<Expr>>,
    },
    Field {
        base: Box<Expr>,
        name: String,
        /// `?.`: null stays null instead of being an error.
        optional: bool,
        /// Where `.name` (or `?.name`) is in the expression text.
        span: Span,
    },
    /// A formatter, status helper, source or helper call: `fmt.bytes(x)`,
    /// `game.builds($id)`.
    Call {
        name: String,
        args: Vec<Expr>,
        /// Where the call is, from its name to its closing parenthesis.
        span: Span,
        /// The source it calls, set by the loader's contract check. `None` for a
        /// formatter or status helper.
        source: Option<&'static SourceSpec>,
    },
    Not(Box<Expr>),
    Neg(Box<Expr>),
    Bin(BinOp, Box<Expr>, Box<Expr>),
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    /// `a else b`: `b` when `a` is null.
    Else(Box<Expr>, Box<Expr>),
    Pipe(Box<Expr>, Stage),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

impl BinOp {
    pub(super) fn symbol(self) -> &'static str {
        match self {
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::And => "and",
            BinOp::Or => "or",
        }
    }
}

/// A pipe stage: `list | stage`.
#[derive(Debug, Clone, PartialEq)]
pub enum Stage {
    /// `sort key.path` or `sort -key.path`.
    Sort {
        key: Vec<String>,
        desc: bool,
    },
    Take(Box<Expr>),
    /// `text | trim n`: at most `n` characters, the last an ellipsis when cut.
    Trim(Box<Expr>),
    /// The predicate sees each item's fields as variables.
    Where(Box<Expr>),
    Before(Box<Expr>),
    After(Box<Expr>),
    Len,
    First,
    Upper,
    Lower,
}

/// A bare name used in an expression, for the loader's "unknown name" check (design 27
/// §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NameUse<'a> {
    pub name: &'a str,
    /// Where the name is in the expression text.
    pub span: Span,
    /// True inside a `where` predicate, where the name may also be a field of the list's
    /// items; the loader can't tell those from bound variables without the item type.
    pub in_item_scope: bool,
}

impl Expr {
    /// True if the expression mentions the bare name `name` (a variable): how the loader
    /// finds an action argument that uses an input variable (`value`, `on`).
    pub fn mentions(&self, name: &str) -> bool {
        match self {
            Expr::Name { name: n, .. } => n == name,
            Expr::Lit(_) | Expr::Param(_) | Expr::Word { .. } => false,
            Expr::Token { alpha, .. } => alpha.as_ref().is_some_and(|a| a.mentions(name)),
            Expr::Field { base, .. } => base.mentions(name),
            Expr::Call { args, .. } => args.iter().any(|a| a.mentions(name)),
            Expr::Not(e) | Expr::Neg(e) => e.mentions(name),
            Expr::Bin(_, a, b) | Expr::Else(a, b) => a.mentions(name) || b.mentions(name),
            Expr::Ternary(a, b, c) => a.mentions(name) || b.mentions(name) || c.mentions(name),
            Expr::Pipe(b, s) => b.mentions(name) || s.expr().is_some_and(|e| e.mentions(name)),
        }
    }

    /// Every bare name the expression uses, in source order. The loader checks each:
    /// a bound variable, else an enum word of the contract, else a load error
    /// (design 27 §7).
    pub fn names(&self) -> Vec<NameUse<'_>> {
        let mut out = Vec::new();
        self.collect_names(false, &mut out);
        out
    }

    pub(super) fn collect_names<'a>(&'a self, item: bool, out: &mut Vec<NameUse<'a>>) {
        match self {
            Expr::Name { name, span, .. } => out.push(NameUse {
                name,
                span: *span,
                in_item_scope: item,
            }),
            Expr::Lit(_) | Expr::Param(_) | Expr::Word { .. } => {}
            Expr::Token { alpha, .. } => {
                if let Some(a) = alpha {
                    a.collect_names(item, out);
                }
            }
            Expr::Field { base, .. } => base.collect_names(item, out),
            Expr::Call { args, .. } => args.iter().for_each(|a| a.collect_names(item, out)),
            Expr::Not(e) | Expr::Neg(e) => e.collect_names(item, out),
            Expr::Bin(_, a, b) | Expr::Else(a, b) => {
                a.collect_names(item, out);
                b.collect_names(item, out);
            }
            Expr::Ternary(a, b, c) => {
                a.collect_names(item, out);
                b.collect_names(item, out);
                c.collect_names(item, out);
            }
            Expr::Pipe(b, s) => {
                b.collect_names(item, out);
                if let Some(e) = s.expr() {
                    e.collect_names(item || matches!(s, Stage::Where(_)), out);
                }
            }
        }
    }

    /// How many levels the tree has, counting the root as 1. This is the number
    /// [`MAX_EXPR_DEPTH`] caps, so it bounds the recursion of everything that walks the
    /// tree: evaluation, [`Expr::mentions`], `Drop` and `PartialEq`.
    pub fn depth(&self) -> usize {
        1 + match self {
            Expr::Lit(_) | Expr::Param(_) | Expr::Name { .. } | Expr::Word { .. } => 0,
            Expr::Token { alpha, .. } => alpha.as_ref().map_or(0, |a| a.depth()),
            Expr::Field { base, .. } => base.depth(),
            Expr::Call { args, .. } => args.iter().map(Expr::depth).max().unwrap_or(0),
            Expr::Not(e) | Expr::Neg(e) => e.depth(),
            Expr::Bin(_, a, b) | Expr::Else(a, b) => a.depth().max(b.depth()),
            Expr::Ternary(a, b, c) => a.depth().max(b.depth()).max(c.depth()),
            Expr::Pipe(b, s) => b.depth().max(s.expr().map_or(0, Expr::depth)),
        }
    }
}

impl Stage {
    /// The expression argument of the stage, if it takes one.
    pub(super) fn expr(&self) -> Option<&Expr> {
        match self {
            Stage::Take(e)
            | Stage::Trim(e)
            | Stage::Where(e)
            | Stage::Before(e)
            | Stage::After(e) => Some(e),
            Stage::Sort { .. } | Stage::Len | Stage::First | Stage::Upper | Stage::Lower => None,
        }
    }
}

/// Parse one expression. Errors carry byte spans into `src`.
///
/// Rejects text longer than [`MAX_EXPR_LEN`] characters, and a tree (or a parenthesis
/// nesting) deeper than [`MAX_EXPR_DEPTH`] levels. Chains such as `a | len | len`,
/// `a.b.c` or `a and b and c` add a level per link.
pub fn parse(src: &str) -> Result<Expr, ParseError> {
    // Bytes are at least characters, so a short byte length settles it without counting.
    if src.len() > MAX_EXPR_LEN {
        let got = src.chars().count();
        if got > MAX_EXPR_LEN {
            return Err(ParseError::new(
                ParseErrorKind::TooLong {
                    got,
                    max: MAX_EXPR_LEN,
                },
                Span::new(0, src.len()),
            ));
        }
    }
    let mut p = Parser {
        toks: lex(src)?,
        i: 0,
        depth: 0,
        end: src.len(),
    };
    let e = p.ternary()?;
    if let Some((t, span)) = p.toks.get(p.i) {
        return Err(ParseError::new(
            ParseErrorKind::TrailingInput(t.found()),
            *span,
        ));
    }
    Ok(e)
}

struct Parser {
    toks: Lexed,
    i: usize,
    depth: usize,
    /// Byte length of the source: where "end of expression" points.
    end: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.i).map(|(t, _)| t)
    }

    fn peek_at(&self, off: usize) -> Option<&Tok> {
        self.toks.get(self.i + off).map(|(t, _)| t)
    }

    fn next(&mut self) -> Option<(Tok, Span)> {
        let t = self.toks.get(self.i).cloned();
        if t.is_some() {
            self.i += 1;
        }
        t
    }

    /// The span of the next token, or of the end of the text.
    fn here(&self) -> Span {
        self.toks
            .get(self.i)
            .map_or(Span::new(self.end, 0), |&(_, s)| s)
    }

    fn eat(&mut self, t: &Tok) -> bool {
        if self.peek() == Some(t) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn is_word(&self, w: &str) -> bool {
        matches!(self.peek(), Some(Tok::Ident(x)) if x == w)
    }

    fn expect(&mut self, t: &Tok, what: &'static str) -> Result<(), ParseError> {
        if self.eat(t) {
            Ok(())
        } else {
            Err(ParseError::new(ParseErrorKind::Expected(what), self.here()))
        }
    }

    fn ident(&mut self, what: &'static str) -> Result<(String, Span), ParseError> {
        match self.toks.get(self.i) {
            Some((Tok::Ident(w), span)) => {
                let out = (w.clone(), *span);
                self.i += 1;
                Ok(out)
            }
            _ => Err(ParseError::new(ParseErrorKind::Expected(what), self.here())),
        }
    }

    /// Run `f` one level deeper, failing past [`MAX_EXPR_DEPTH`].
    fn nested<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, ParseError>,
    ) -> Result<T, ParseError> {
        if self.depth >= MAX_EXPR_DEPTH {
            return Err(ParseError::new(
                ParseErrorKind::TooDeep {
                    max: MAX_EXPR_DEPTH,
                },
                self.here(),
            ));
        }
        self.depth += 1;
        let r = f(self);
        self.depth -= 1;
        r
    }

    /// The tree built so far, or a depth error if it has more than [`MAX_EXPR_DEPTH`]
    /// levels. Called after every node is built, so a tree is at most one level past
    /// the cap when it is checked and the check itself never recurses deeply.
    fn built(&self, e: Expr) -> Result<Expr, ParseError> {
        if e.depth() > MAX_EXPR_DEPTH {
            return Err(ParseError::new(
                ParseErrorKind::TooDeep {
                    max: MAX_EXPR_DEPTH,
                },
                self.here(),
            ));
        }
        Ok(e)
    }

    /// The span of the token just consumed.
    fn prev_span(&self) -> Span {
        self.i
            .checked_sub(1)
            .and_then(|i| self.toks.get(i))
            .map_or(Span::new(0, 0), |&(_, s)| s)
    }

    fn ternary(&mut self) -> Result<Expr, ParseError> {
        self.nested(|p| {
            let c = p.else_()?;
            if p.eat(&Tok::Question) {
                let a = p.ternary()?;
                p.expect(&Tok::Colon, "`:` in `cond ? a : b`")?;
                let b = p.ternary()?;
                return p.built(Expr::Ternary(Box::new(c), Box::new(a), Box::new(b)));
            }
            Ok(c)
        })
    }

    fn else_(&mut self) -> Result<Expr, ParseError> {
        let mut e = self.pipe()?;
        while self.is_word("else") {
            self.i += 1;
            let r = self.pipe()?;
            e = self.built(Expr::Else(Box::new(e), Box::new(r)))?;
        }
        Ok(e)
    }

    fn pipe(&mut self) -> Result<Expr, ParseError> {
        let mut e = self.or()?;
        while self.eat(&Tok::Pipe) {
            let span = self.here();
            let stage = match self.peek() {
                Some(Tok::Ident(w)) => w.clone(),
                _ => {
                    return Err(ParseError::new(
                        ParseErrorKind::Expected("a pipe stage after `|`"),
                        span,
                    ));
                }
            };
            self.i += 1;
            let st = match stage.as_str() {
                "sort" => {
                    let desc = self.eat(&Tok::Minus);
                    let mut key = vec![self.ident("a sort key")?.0];
                    while self.eat(&Tok::Dot) {
                        key.push(self.ident("a sort key")?.0);
                    }
                    Stage::Sort { key, desc }
                }
                "take" => Stage::Take(Box::new(self.postfix()?)),
                "trim" => Stage::Trim(Box::new(self.postfix()?)),
                "where" => Stage::Where(Box::new(self.or()?)),
                "before" => Stage::Before(Box::new(self.postfix()?)),
                "after" => Stage::After(Box::new(self.postfix()?)),
                "len" => Stage::Len,
                "first" => Stage::First,
                "upper" => Stage::Upper,
                "lower" => Stage::Lower,
                s => {
                    return Err(ParseError::new(
                        ParseErrorKind::UnknownStage(s.to_string()),
                        span,
                    ));
                }
            };
            e = self.built(Expr::Pipe(Box::new(e), st))?;
            e = self.fields(e)?;
        }
        Ok(e)
    }

    fn or(&mut self) -> Result<Expr, ParseError> {
        let mut e = self.and()?;
        while self.eat(&Tok::Op(BinOp::Or)) {
            let r = self.and()?;
            e = self.built(Expr::Bin(BinOp::Or, Box::new(e), Box::new(r)))?;
        }
        Ok(e)
    }

    fn and(&mut self) -> Result<Expr, ParseError> {
        let mut e = self.not()?;
        while self.eat(&Tok::Op(BinOp::And)) {
            let r = self.not()?;
            e = self.built(Expr::Bin(BinOp::And, Box::new(e), Box::new(r)))?;
        }
        Ok(e)
    }

    fn not(&mut self) -> Result<Expr, ParseError> {
        if self.is_word("not") {
            self.i += 1;
            return self.nested(|p| {
                let e = p.not()?;
                p.built(Expr::Not(Box::new(e)))
            });
        }
        self.cmp()
    }

    /// A comparison. Non-associative: `a < b < c` is an error, not `(a < b) < c`.
    fn cmp(&mut self) -> Result<Expr, ParseError> {
        let l = self.unary()?;
        if let Some(Tok::Op(op)) = self.peek().cloned()
            && !matches!(op, BinOp::And | BinOp::Or)
        {
            self.i += 1;
            let r = self.unary()?;
            if let Some(Tok::Op(next)) = self.peek()
                && !matches!(next, BinOp::And | BinOp::Or)
            {
                return Err(ParseError::new(
                    ParseErrorKind::ChainedComparison,
                    self.here(),
                ));
            }
            return self.built(Expr::Bin(op, Box::new(l), Box::new(r)));
        }
        Ok(l)
    }

    fn unary(&mut self) -> Result<Expr, ParseError> {
        if self.eat(&Tok::Minus) {
            return self.nested(|p| {
                let e = p.unary()?;
                p.built(Expr::Neg(Box::new(e)))
            });
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Result<Expr, ParseError> {
        let e = match self.next() {
            Some((Tok::Num(n), _)) => Expr::Lit(Value::Num(n)),
            Some((Tok::Str(s), _)) => Expr::Lit(Value::Str(s)),
            Some((Tok::Param(p), _)) => Expr::Param(p),
            Some((Tok::Token(name), _)) => {
                // `@warn alpha=.55`
                let alpha = if self.is_word("alpha") && self.peek_at(1) == Some(&Tok::Assign) {
                    self.i += 2;
                    Some(Box::new(self.nested(|p| p.postfix())?))
                } else {
                    None
                };
                self.built(Expr::Token { name, alpha })?
            }
            Some((Tok::LParen, _)) => {
                let e = self.ternary()?;
                self.expect(&Tok::RParen, "`)`")?;
                e
            }
            Some((Tok::Ident(w), span)) => match w.as_str() {
                "true" => Expr::Lit(Value::Bool(true)),
                "false" => Expr::Lit(Value::Bool(false)),
                "null" => Expr::Lit(Value::Null),
                _ => self.name_or_call(w, span)?,
            },
            Some((t, span)) => {
                return Err(ParseError::new(ParseErrorKind::Unexpected(t.found()), span));
            }
            None => {
                return Err(ParseError::new(
                    ParseErrorKind::Unexpected(Found::End),
                    Span::new(self.end, 0),
                ));
            }
        };
        self.fields(e)
    }

    /// A bare name, or a dotted name followed by `(`, which is a call:
    /// `game.builds($id)`.
    fn name_or_call(&mut self, first: String, first_span: Span) -> Result<Expr, ParseError> {
        let save = self.i;
        let mut path = first.clone();
        while self.peek() == Some(&Tok::Dot) && matches!(self.peek_at(1), Some(Tok::Ident(_))) {
            self.i += 1;
            path.push('.');
            path.push_str(&self.ident("a name")?.0);
        }
        if !self.eat(&Tok::LParen) {
            self.i = save;
            return Ok(Expr::Name {
                name: first,
                span: first_span,
                source: None,
            });
        }
        let mut args = Vec::new();
        if !self.eat(&Tok::RParen) {
            loop {
                args.push(self.ternary()?);
                if self.eat(&Tok::RParen) {
                    break;
                }
                self.expect(&Tok::Comma, "`,` or `)`")?;
            }
        }
        let span = first_span.to(self.prev_span());
        self.built(Expr::Call {
            name: path,
            args,
            span,
            source: None,
        })
    }

    fn fields(&mut self, mut e: Expr) -> Result<Expr, ParseError> {
        loop {
            let (optional, dot) = match self.toks.get(self.i) {
                Some((Tok::Dot, s)) => (false, *s),
                Some((Tok::QDot, s)) => (true, *s),
                _ => return Ok(e),
            };
            self.i += 1;
            let (name, name_span) = self.ident("a field name")?;
            e = self.built(Expr::Field {
                base: Box::new(e),
                name,
                optional,
                span: dot.to(name_span),
            })?;
        }
    }
}
