//! The AST and the recursive-descent parser.

use gogo_contract::Value;

use super::error::{ParseError, ParseErrorKind, Span};
use super::lex::{Lexed, Tok, lex};
use super::{MAX_EXPR_DEPTH, MAX_EXPR_LEN};

/// A parsed expression. Build it once with [`parse`]; evaluate it as often as needed.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Lit(Value),
    /// A bare name: a bound variable, or else an enum word such as `ready`.
    Name(String),
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
    },
    /// A formatter, status helper, source or helper call: `fmt.bytes(x)`,
    /// `game.builds($id)`.
    Call {
        name: String,
        args: Vec<Expr>,
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

impl Expr {
    /// True if the expression mentions the bare name `name` (a variable): how the loader
    /// finds an action argument that uses an input variable (`value`, `on`).
    pub fn mentions(&self, name: &str) -> bool {
        match self {
            Expr::Name(n) => n == name,
            Expr::Lit(_) | Expr::Param(_) => false,
            Expr::Token { alpha, .. } => alpha.as_ref().is_some_and(|a| a.mentions(name)),
            Expr::Field { base, .. } => base.mentions(name),
            Expr::Call { args, .. } => args.iter().any(|a| a.mentions(name)),
            Expr::Not(e) | Expr::Neg(e) => e.mentions(name),
            Expr::Bin(_, a, b) | Expr::Else(a, b) => a.mentions(name) || b.mentions(name),
            Expr::Ternary(a, b, c) => a.mentions(name) || b.mentions(name) || c.mentions(name),
            Expr::Pipe(b, s) => {
                b.mentions(name)
                    || match s {
                        Stage::Take(e)
                        | Stage::Trim(e)
                        | Stage::Where(e)
                        | Stage::Before(e)
                        | Stage::After(e) => e.mentions(name),
                        Stage::Sort { .. }
                        | Stage::Len
                        | Stage::First
                        | Stage::Upper
                        | Stage::Lower => false,
                    }
            }
        }
    }
}

/// Parse one expression. Errors carry byte spans into `src`.
///
/// Rejects text longer than [`MAX_EXPR_LEN`] characters or nested deeper than
/// [`MAX_EXPR_DEPTH`] levels.
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
            ParseErrorKind::TrailingInput(t.describe()),
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

    fn ident(&mut self, what: &'static str) -> Result<String, ParseError> {
        match self.peek() {
            Some(Tok::Ident(w)) => {
                let w = w.clone();
                self.i += 1;
                Ok(w)
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

    fn ternary(&mut self) -> Result<Expr, ParseError> {
        self.nested(|p| {
            let c = p.else_()?;
            if p.eat(&Tok::Question) {
                let a = p.ternary()?;
                p.expect(&Tok::Colon, "`:` in `cond ? a : b`")?;
                let b = p.ternary()?;
                return Ok(Expr::Ternary(Box::new(c), Box::new(a), Box::new(b)));
            }
            Ok(c)
        })
    }

    fn else_(&mut self) -> Result<Expr, ParseError> {
        let mut e = self.pipe()?;
        while self.is_word("else") {
            self.i += 1;
            let r = self.pipe()?;
            e = Expr::Else(Box::new(e), Box::new(r));
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
                    let mut key = vec![self.ident("a sort key")?];
                    while self.eat(&Tok::Dot) {
                        key.push(self.ident("a sort key")?);
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
            e = Expr::Pipe(Box::new(e), st);
            e = self.fields(e)?;
        }
        Ok(e)
    }

    fn or(&mut self) -> Result<Expr, ParseError> {
        let mut e = self.and()?;
        while self.eat(&Tok::Op(BinOp::Or)) {
            e = Expr::Bin(BinOp::Or, Box::new(e), Box::new(self.and()?));
        }
        Ok(e)
    }

    fn and(&mut self) -> Result<Expr, ParseError> {
        let mut e = self.not()?;
        while self.eat(&Tok::Op(BinOp::And)) {
            e = Expr::Bin(BinOp::And, Box::new(e), Box::new(self.not()?));
        }
        Ok(e)
    }

    fn not(&mut self) -> Result<Expr, ParseError> {
        if self.is_word("not") {
            self.i += 1;
            return self.nested(|p| Ok(Expr::Not(Box::new(p.not()?))));
        }
        self.cmp()
    }

    fn cmp(&mut self) -> Result<Expr, ParseError> {
        let l = self.unary()?;
        if let Some(Tok::Op(op)) = self.peek().cloned()
            && !matches!(op, BinOp::And | BinOp::Or)
        {
            self.i += 1;
            let r = self.unary()?;
            return Ok(Expr::Bin(op, Box::new(l), Box::new(r)));
        }
        Ok(l)
    }

    fn unary(&mut self) -> Result<Expr, ParseError> {
        if self.eat(&Tok::Minus) {
            return self.nested(|p| Ok(Expr::Neg(Box::new(p.unary()?))));
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
                Expr::Token { name, alpha }
            }
            Some((Tok::LParen, _)) => {
                let e = self.ternary()?;
                self.expect(&Tok::RParen, "`)`")?;
                e
            }
            Some((Tok::Ident(w), _)) => match w.as_str() {
                "true" => Expr::Lit(Value::Bool(true)),
                "false" => Expr::Lit(Value::Bool(false)),
                "null" => Expr::Lit(Value::Null),
                _ => self.name_or_call(w)?,
            },
            Some((t, span)) => {
                return Err(ParseError::new(
                    ParseErrorKind::Unexpected(t.describe()),
                    span,
                ));
            }
            None => {
                return Err(ParseError::new(
                    ParseErrorKind::Unexpected("end of expression".into()),
                    Span::new(self.end, 0),
                ));
            }
        };
        self.fields(e)
    }

    /// A bare name, or a dotted name followed by `(`, which is a call:
    /// `game.builds($id)`.
    fn name_or_call(&mut self, first: String) -> Result<Expr, ParseError> {
        let save = self.i;
        let mut path = first.clone();
        while self.peek() == Some(&Tok::Dot) && matches!(self.peek_at(1), Some(Tok::Ident(_))) {
            self.i += 1;
            path.push('.');
            path.push_str(&self.ident("a name")?);
        }
        if !self.eat(&Tok::LParen) {
            self.i = save;
            return Ok(Expr::Name(first));
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
        Ok(Expr::Call { name: path, args })
    }

    fn fields(&mut self, mut e: Expr) -> Result<Expr, ParseError> {
        loop {
            let optional = match self.peek() {
                Some(Tok::Dot) => false,
                Some(Tok::QDot) => true,
                _ => return Ok(e),
            };
            self.i += 1;
            let name = self.ident("a field name")?;
            e = Expr::Field {
                base: Box::new(e),
                name,
                optional,
            };
        }
    }
}
