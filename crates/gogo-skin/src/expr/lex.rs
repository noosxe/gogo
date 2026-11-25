//! The lexer. Works on characters, reports byte spans.

use super::error::{ParseError, ParseErrorKind, Span};
use super::parse::BinOp;

#[derive(Debug, Clone, PartialEq)]
pub(super) enum Tok {
    Ident(String),
    Num(f64),
    Str(String),
    Param(String),
    Token(String),
    Dot,
    QDot,
    LParen,
    RParen,
    Comma,
    Pipe,
    Question,
    Colon,
    Assign,
    Op(BinOp),
    Minus,
}

impl Tok {
    /// How the token reads in an error message.
    pub(super) fn describe(&self) -> String {
        match self {
            Tok::Ident(w) => format!("`{w}`"),
            Tok::Num(_) => "a number".into(),
            Tok::Str(_) => "a string".into(),
            Tok::Param(p) => format!("`${p}`"),
            Tok::Token(t) => format!("`@{t}`"),
            Tok::Dot => "`.`".into(),
            Tok::QDot => "`?.`".into(),
            Tok::LParen => "`(`".into(),
            Tok::RParen => "`)`".into(),
            Tok::Comma => "`,`".into(),
            Tok::Pipe => "`|`".into(),
            Tok::Question => "`?`".into(),
            Tok::Colon => "`:`".into(),
            Tok::Assign => "`=`".into(),
            Tok::Op(op) => format!("`{}`", op.symbol()),
            Tok::Minus => "`-`".into(),
        }
    }
}

pub(super) type Lexed = Vec<(Tok, Span)>;

fn ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Lex `src`. The caller has already bounded its length.
pub(super) fn lex(src: &str) -> Result<Lexed, ParseError> {
    let cs: Vec<(usize, char)> = src.char_indices().collect();
    // Byte offset of char index `i`, with the end of the text for one past the last.
    let at = |i: usize| cs.get(i).map_or(src.len(), |&(b, _)| b);
    let ch = |i: usize| cs.get(i).map(|&(_, c)| c);
    let text = |a: usize, b: usize| -> String { cs[a..b].iter().map(|&(_, c)| c).collect() };

    let mut i = 0;
    let mut out: Lexed = Vec::new();
    while let Some(c) = ch(i) {
        let start = i;
        let tok = match c {
            ' ' | '\t' | '\n' | '\r' => {
                i += 1;
                continue;
            }
            '.' if ch(i + 1).is_some_and(|d| d.is_ascii_digit()) => {
                i += 1;
                while ch(i).is_some_and(|d| d.is_ascii_digit()) {
                    i += 1;
                }
                number(&text(start, i), Span::new(at(start), at(i) - at(start)))?
            }
            '.' => {
                i += 1;
                Tok::Dot
            }
            '?' if ch(i + 1) == Some('.') => {
                i += 2;
                Tok::QDot
            }
            '?' => {
                i += 1;
                Tok::Question
            }
            '(' => {
                i += 1;
                Tok::LParen
            }
            ')' => {
                i += 1;
                Tok::RParen
            }
            ',' => {
                i += 1;
                Tok::Comma
            }
            '|' => {
                i += 1;
                Tok::Pipe
            }
            ':' => {
                i += 1;
                Tok::Colon
            }
            '-' => {
                i += 1;
                Tok::Minus
            }
            '=' if ch(i + 1) == Some('=') => {
                i += 2;
                Tok::Op(BinOp::Eq)
            }
            '=' => {
                i += 1;
                Tok::Assign
            }
            '!' if ch(i + 1) == Some('=') => {
                i += 2;
                Tok::Op(BinOp::Ne)
            }
            '<' | '>' => {
                let eq = ch(i + 1) == Some('=');
                i += if eq { 2 } else { 1 };
                Tok::Op(match (c, eq) {
                    ('<', false) => BinOp::Lt,
                    ('<', true) => BinOp::Le,
                    ('>', false) => BinOp::Gt,
                    _ => BinOp::Ge,
                })
            }
            '\'' | '"' => {
                i += 1;
                let s = i;
                while ch(i).is_some_and(|d| d != c) {
                    i += 1;
                }
                if ch(i).is_none() {
                    return Err(ParseError::new(
                        ParseErrorKind::UnterminatedString,
                        Span::new(at(start), src.len() - at(start)),
                    ));
                }
                let t = Tok::Str(text(s, i));
                i += 1;
                t
            }
            '$' | '@' => {
                i += 1;
                let s = i;
                while let Some(d) = ch(i) {
                    let more = ident_char(d)
                        || (c == '@'
                            && (d == '-' || (d == '.' && ch(i + 1).is_some_and(ident_char))));
                    if !more {
                        break;
                    }
                    i += 1;
                }
                if s == i {
                    return Err(ParseError::new(
                        ParseErrorKind::MissingName(c),
                        Span::new(at(start), at(i) - at(start)),
                    ));
                }
                let name = text(s, i);
                if c == '$' {
                    Tok::Param(name)
                } else {
                    Tok::Token(name)
                }
            }
            c if c.is_ascii_digit() => {
                while let Some(d) = ch(i) {
                    if d.is_ascii_digit()
                        || (d == '.' && ch(i + 1).is_some_and(|e| e.is_ascii_digit()))
                    {
                        i += 1;
                    } else {
                        break;
                    }
                }
                number(&text(start, i), Span::new(at(start), at(i) - at(start)))?
            }
            c if ident_start(c) => {
                while ch(i).is_some_and(ident_char) {
                    i += 1;
                }
                match text(start, i).as_str() {
                    "and" => Tok::Op(BinOp::And),
                    "or" => Tok::Op(BinOp::Or),
                    w => Tok::Ident(w.to_string()),
                }
            }
            c => {
                return Err(ParseError::new(
                    ParseErrorKind::UnexpectedChar(c),
                    Span::new(at(start), c.len_utf8()),
                ));
            }
        };
        out.push((tok, Span::new(at(start), at(i) - at(start))));
    }
    Ok(out)
}

fn number(t: &str, span: Span) -> Result<Tok, ParseError> {
    match t.parse::<f64>() {
        Ok(n) if n.is_finite() => Ok(Tok::Num(n)),
        _ => Err(ParseError::new(
            ParseErrorKind::BadNumber(t.to_string()),
            span,
        )),
    }
}
