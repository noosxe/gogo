//! A small reader for Valve's text KeyValues ("VDF"), enough for
//! `toolmanifest.vdf` and `libraryfolders.vdf` (design 18 §1, §2.2):
//! quoted or bare tokens, `{ }` nesting, `//` comments, and the `\\ \" \n
//! \t` escapes. Conditionals (`[$WIN32]`) and `#include` aren't used by
//! either file and are read as plain tokens. No crate does this with a
//! licence and footprint worth the dependency for two files.

/// A value: a string, or a block of key/value pairs in file order (keys
/// may repeat).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Str(String),
    Block(Vec<(String, Value)>),
}

impl Value {
    /// The first value under `key` (VDF keys are case-insensitive).
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Block(pairs) => pairs
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(key))
                .map(|(_, v)| v),
            Value::Str(_) => None,
        }
    }

    /// The string under `key`, if it is one.
    pub fn str(&self, key: &str) -> Option<&str> {
        match self.get(key)? {
            Value::Str(s) => Some(s),
            Value::Block(_) => None,
        }
    }

    /// A block's pairs; empty for a string.
    pub fn pairs(&self) -> &[(String, Value)] {
        match self {
            Value::Block(pairs) => pairs,
            Value::Str(_) => &[],
        }
    }
}

#[derive(Debug, PartialEq)]
enum Token {
    Str(String),
    Open,
    Close,
}

fn tokens(text: &str) -> Result<Vec<Token>, String> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            _ if c.is_whitespace() => {
                chars.next();
            }
            '/' => {
                chars.next();
                if chars.peek() == Some(&'/') {
                    for c in chars.by_ref() {
                        if c == '\n' {
                            break;
                        }
                    }
                } else {
                    out.push(Token::Str(bare(&mut chars, "/")));
                }
            }
            '{' => {
                chars.next();
                out.push(Token::Open);
            }
            '}' => {
                chars.next();
                out.push(Token::Close);
            }
            '"' => {
                chars.next();
                let mut s = String::new();
                loop {
                    match chars.next() {
                        None => return Err("unterminated string".into()),
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some('n') => s.push('\n'),
                            Some('t') => s.push('\t'),
                            Some(c @ ('\\' | '"')) => s.push(c),
                            Some(c) => {
                                s.push('\\');
                                s.push(c);
                            }
                            None => return Err("unterminated string".into()),
                        },
                        Some(c) => s.push(c),
                    }
                }
                out.push(Token::Str(s));
            }
            _ => out.push(Token::Str(bare(&mut chars, ""))),
        }
    }
    Ok(out)
}

/// A bare token: up to whitespace, a brace or a quote.
fn bare(chars: &mut std::iter::Peekable<std::str::Chars>, start: &str) -> String {
    let mut s = start.to_string();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() || matches!(c, '{' | '}' | '"') {
            break;
        }
        s.push(c);
        chars.next();
    }
    s
}

/// Parse a VDF document into its top-level block.
pub fn parse(text: &str) -> Result<Value, String> {
    let toks = tokens(text.trim_start_matches('\u{feff}'))?;
    let mut pos = 0;
    let pairs = block(&toks, &mut pos, false)?;
    Ok(Value::Block(pairs))
}

fn block(toks: &[Token], pos: &mut usize, nested: bool) -> Result<Vec<(String, Value)>, String> {
    let mut pairs = Vec::new();
    loop {
        match toks.get(*pos) {
            None if nested => return Err("unclosed '{'".into()),
            None => return Ok(pairs),
            Some(Token::Close) if nested => {
                *pos += 1;
                return Ok(pairs);
            }
            Some(Token::Close) => return Err("unexpected '}'".into()),
            Some(Token::Open) => return Err("a block without a key".into()),
            Some(Token::Str(key)) => {
                *pos += 1;
                let value = match toks.get(*pos) {
                    Some(Token::Str(v)) => {
                        *pos += 1;
                        Value::Str(v.clone())
                    }
                    Some(Token::Open) => {
                        *pos += 1;
                        Value::Block(block(toks, pos, true)?)
                    }
                    Some(Token::Close) | None => return Err(format!("'{key}' has no value")),
                };
                pairs.push((key.clone(), value));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GE-Proton11-11's `toolmanifest.vdf` (docs/21 §1.2), as shipped.
    const TOOLMANIFEST: &str = r#"// Generated file, do not edit
"manifest"
{
  "version" "2"
  "commandline" "/proton %verb%"
  "require_tool_appid" "4183110"
  "use_sessions" "1"
  "compatmanager_layer_name" "proton"
}
"#;

    #[test]
    fn toolmanifest_reads() {
        let v = parse(TOOLMANIFEST).unwrap();
        let m = v.get("manifest").unwrap();
        assert_eq!(m.str("require_tool_appid"), Some("4183110"));
        assert_eq!(m.str("commandline"), Some("/proton %verb%"));
        // Keys are case-insensitive.
        assert_eq!(m.str("Require_Tool_AppId"), Some("4183110"));
        assert_eq!(v.str("manifest"), None);
    }

    #[test]
    fn libraryfolders_reads_with_nesting_and_escapes() {
        let text = "\u{feff}\"libraryfolders\"\n{\n\t\"0\"\n\t{\n\t\t\"path\"\t\t\"/home/u/.local/share/Steam\"\n\
                    \t\t\"apps\"\n\t\t{\n\t\t\t\"228980\"\t\t\"123\"\n\t\t}\n\t}\n\
                    \t\"1\" { \"path\" \"/mnt/games/Steam \\\"Library\\\"\" \"label\" \"\" }\n}\n";
        let v = parse(text).unwrap();
        let lf = v.get("libraryfolders").unwrap();
        let paths: Vec<&str> = lf
            .pairs()
            .iter()
            .filter_map(|(_, f)| f.str("path"))
            .collect();
        assert_eq!(
            paths,
            ["/home/u/.local/share/Steam", "/mnt/games/Steam \"Library\""]
        );
        assert_eq!(
            lf.get("0").unwrap().get("apps").unwrap().str("228980"),
            Some("123")
        );
    }

    #[test]
    fn bare_tokens_comments_and_backslashes() {
        let v = parse("a b // c\nd { e \"C:\\\\x\\y\" } f/g h").unwrap();
        assert_eq!(v.str("a"), Some("b"));
        assert_eq!(v.get("d").unwrap().str("e"), Some("C:\\x\\y"));
        assert_eq!(v.str("f/g"), Some("h"));
    }

    #[test]
    fn malformed_input_is_an_error() {
        assert!(
            parse("\"a\" { \"b\" \"c\"")
                .unwrap_err()
                .contains("unclosed")
        );
        assert!(parse("\"a\"").unwrap_err().contains("no value"));
        assert!(parse("}").is_err());
        assert!(parse("\"a").unwrap_err().contains("unterminated"));
        assert!(parse("{ }").is_err());
        assert_eq!(parse("").unwrap(), Value::Block(vec![]));
    }
}
