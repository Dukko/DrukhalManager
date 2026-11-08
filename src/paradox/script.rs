//! Parser for Paradox (Clausewitz) script: `key = value`, `key = { ... }`,
//! bare list items, quoted strings and `#` comments.

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Scalar(String),
    Block(Vec<Entry>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    /// `key <op> value`
    Assign { key: String, op: Op, value: Value },
    /// A bare item inside a list, e.g. `"Graphics"` in `tags = { "Graphics" }`.
    Item(Value),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Eq,
    Lt,
    Gt,
    Le,
    Ge,
    Ne,
    /// `?=`, the "exists and equals" operator.
    SafeEq,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for ParseError {}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Word(String),
    Quoted(String),
    Op(Op),
    Open,
    Close,
}

struct Lexer<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
    line: usize,
}

impl<'a> Lexer<'a> {
    fn new(src: &'a str) -> Self {
        Self { chars: src.chars().peekable(), line: 1 }
    }

    fn next_token(&mut self) -> Result<Option<(Token, usize)>, ParseError> {
        loop {
            match self.chars.peek() {
                None => return Ok(None),
                Some('\n') => {
                    self.line += 1;
                    self.chars.next();
                }
                Some(c) if c.is_whitespace() || *c == '\u{feff}' => {
                    self.chars.next();
                }
                Some('#') => {
                    while let Some(&c) = self.chars.peek() {
                        if c == '\n' {
                            break;
                        }
                        self.chars.next();
                    }
                }
                Some(_) => break,
            }
        }

        let line = self.line;
        let c = self.chars.next().unwrap();
        let token = match c {
            '{' => Token::Open,
            '}' => Token::Close,
            '"' => {
                let mut s = String::new();
                loop {
                    match self.chars.next() {
                        None => return Err(ParseError { line, message: "unterminated string".into() }),
                        Some('"') => break,
                        Some('\\') => match self.chars.next() {
                            Some('"') => s.push('"'),
                            Some('\\') => s.push('\\'),
                            Some(other) => {
                                s.push('\\');
                                s.push(other);
                            }
                            None => return Err(ParseError { line, message: "unterminated string".into() }),
                        },
                        Some('\n') => {
                            self.line += 1;
                            s.push('\n');
                        }
                        Some(other) => s.push(other),
                    }
                }
                Token::Quoted(s)
            }
            '=' => Token::Op(Op::Eq),
            '<' | '>' | '!' | '?' => {
                let followed_by_eq = self.chars.peek() == Some(&'=');
                if followed_by_eq {
                    self.chars.next();
                }
                match (c, followed_by_eq) {
                    ('<', false) => Token::Op(Op::Lt),
                    ('>', false) => Token::Op(Op::Gt),
                    ('<', true) => Token::Op(Op::Le),
                    ('>', true) => Token::Op(Op::Ge),
                    ('!', true) => Token::Op(Op::Ne),
                    ('?', true) => Token::Op(Op::SafeEq),
                    _ => return Err(ParseError { line, message: format!("unexpected '{c}'") }),
                }
            }
            _ => {
                let mut s = String::from(c);
                while let Some(&c) = self.chars.peek() {
                    if c.is_whitespace() || matches!(c, '{' | '}' | '=' | '<' | '>' | '#' | '"') {
                        break;
                    }
                    if (c == '!' || c == '?') && self.peek_second() == Some('=') {
                        break;
                    }
                    s.push(c);
                    self.chars.next();
                }
                Token::Word(s)
            }
        };
        Ok(Some((token, line)))
    }

    fn peek_second(&self) -> Option<char> {
        let mut it = self.chars.clone();
        it.next();
        it.next()
    }
}

struct Parser<'a> {
    lexer: Lexer<'a>,
    peeked: Option<(Token, usize)>,
}

impl<'a> Parser<'a> {
    fn peek(&mut self) -> Result<Option<&(Token, usize)>, ParseError> {
        if self.peeked.is_none() {
            self.peeked = self.lexer.next_token()?;
        }
        Ok(self.peeked.as_ref())
    }

    fn next(&mut self) -> Result<Option<(Token, usize)>, ParseError> {
        match self.peeked.take() {
            Some(t) => Ok(Some(t)),
            None => self.lexer.next_token(),
        }
    }

    fn parse_entries(&mut self, nested: bool) -> Result<Vec<Entry>, ParseError> {
        let mut entries = Vec::new();
        loop {
            let Some((token, line)) = self.next()? else {
                if nested {
                    return Err(ParseError { line: self.lexer.line, message: "missing '}'".into() });
                }
                return Ok(entries);
            };
            let first = match token {
                Token::Close if nested => return Ok(entries),
                Token::Close => return Err(ParseError { line, message: "unexpected '}'".into() }),
                Token::Op(_) => return Err(ParseError { line, message: "operator without a key".into() }),
                Token::Open => Value::Block(self.parse_entries(true)?),
                Token::Word(s) | Token::Quoted(s) => Value::Scalar(s),
            };

            if let Some((Token::Op(op), _)) = self.peek()? {
                let op = *op;
                self.next()?;
                let Value::Scalar(key) = first else {
                    return Err(ParseError { line, message: "a block cannot be used as a key".into() });
                };
                let value = self.parse_value(line)?;
                entries.push(Entry::Assign { key, op, value });
            } else {
                entries.push(Entry::Item(first));
            }
        }
    }

    fn parse_value(&mut self, key_line: usize) -> Result<Value, ParseError> {
        match self.next()? {
            Some((Token::Open, _)) => Ok(Value::Block(self.parse_entries(true)?)),
            Some((Token::Word(s) | Token::Quoted(s), _)) => Ok(Value::Scalar(s)),
            Some((_, line)) => Err(ParseError { line, message: "expected a value".into() }),
            None => Err(ParseError { line: key_line, message: "expected a value".into() }),
        }
    }
}

pub fn parse(src: &str) -> Result<Vec<Entry>, ParseError> {
    Parser { lexer: Lexer::new(src), peeked: None }.parse_entries(false)
}

impl Value {
    pub fn as_scalar(&self) -> Option<&str> {
        match self {
            Value::Scalar(s) => Some(s),
            Value::Block(_) => None,
        }
    }

    pub fn as_block(&self) -> Option<&[Entry]> {
        match self {
            Value::Block(b) => Some(b),
            Value::Scalar(_) => None,
        }
    }
}

/// Lookup helpers over a list of entries. Keys are matched case-insensitively,
/// as the game does.
pub trait Entries {
    fn values<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a Value> + 'a;

    fn value<'a>(&'a self, key: &'a str) -> Option<&'a Value> {
        self.values(key).next()
    }

    fn scalar<'a>(&'a self, key: &'a str) -> Option<&'a str> {
        self.values(key).find_map(Value::as_scalar)
    }

    /// All bare scalar items of every block assigned to `key`.
    fn scalar_items(&self, key: &str) -> Vec<String> {
        self.values(key)
            .filter_map(Value::as_block)
            .flatten()
            .filter_map(|e| match e {
                Entry::Item(Value::Scalar(s)) => Some(s.clone()),
                _ => None,
            })
            .collect()
    }
}

impl Entries for [Entry] {
    fn values<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a Value> + 'a {
        self.iter().filter_map(move |e| match e {
            Entry::Assign { key: k, value, .. } if k.eq_ignore_ascii_case(key) => Some(value),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_descriptor() {
        let src = "version=\"1.0\"\ntags={\n\t\"Graphics\"\n}\nname=\"! IBS Animated Map Icons\" # trailing\nremote_file_id=\"2633444661\"";
        let entries = parse(src).unwrap();
        assert_eq!(entries.scalar("name"), Some("! IBS Animated Map Icons"));
        assert_eq!(entries.scalar("VERSION"), Some("1.0"));
        assert_eq!(entries.scalar_items("tags"), vec!["Graphics"]);
    }

    #[test]
    fn parses_operators_and_nesting() {
        let src = "trigger = { years_passed >= 10 has_flag != foo exists?=bar }\nlist = { a b { c } }";
        let entries = parse(src).unwrap();
        let trigger = entries.value("trigger").unwrap().as_block().unwrap();
        assert_eq!(trigger.len(), 3);
        assert!(matches!(&trigger[0], Entry::Assign { op: Op::Ge, .. }));
        assert!(matches!(&trigger[1], Entry::Assign { op: Op::Ne, .. }));
        assert!(matches!(&trigger[2], Entry::Assign { op: Op::SafeEq, key, .. } if key == "exists"));
        assert_eq!(entries.value("list").unwrap().as_block().unwrap().len(), 3);
    }

    #[test]
    fn reports_unbalanced_braces() {
        assert_eq!(parse("a = {\n b = c").unwrap_err().message, "missing '}'");
        assert_eq!(parse("a = b\n}").unwrap_err().line, 2);
    }
}
