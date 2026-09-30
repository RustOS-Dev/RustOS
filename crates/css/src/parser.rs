//! CSS Syntax Level 3 parsing (§5): component values, rules and
//! declaration lists.

use crate::tokenizer::{Token, tokenize};
use alloc::string::String;
use alloc::vec::Vec;

/// A component value: a preserved token, a function or a simple block.
#[derive(Debug, Clone, PartialEq)]
pub enum Cv {
    Token(Token),
    Function {
        name: String,
        args: Vec<Cv>,
    },
    /// `open` is '(', '[' or '{'.
    Block {
        open: char,
        items: Vec<Cv>,
    },
}

impl Cv {
    pub fn is_ws(&self) -> bool {
        matches!(self, Cv::Token(Token::Whitespace))
    }

    pub fn ident(&self) -> Option<&str> {
        match self {
            Cv::Token(Token::Ident(s)) => Some(s),
            _ => None,
        }
    }

    pub fn is_delim(&self, c: char) -> bool {
        matches!(self, Cv::Token(Token::Delim(d)) if *d == c)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Declaration {
    /// Property name, lower-cased unless it is a custom property.
    pub name: String,
    pub value: Vec<Cv>,
    pub important: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Rule {
    Qualified {
        prelude: Vec<Cv>,
        block: Vec<Cv>,
    },
    At {
        name: String,
        prelude: Vec<Cv>,
        block: Option<Vec<Cv>>,
    },
}

struct Stream {
    toks: Vec<Token>,
    i: usize,
}

impl Stream {
    fn next(&mut self) -> Option<Token> {
        let t = self.toks.get(self.i).cloned();
        self.i += 1;
        t
    }

    fn component(&mut self, t: Token) -> Cv {
        match t {
            Token::OpenCurly => Cv::Block {
                open: '{',
                items: self.until(Token::CloseCurly),
            },
            Token::OpenSquare => Cv::Block {
                open: '[',
                items: self.until(Token::CloseSquare),
            },
            Token::OpenParen => Cv::Block {
                open: '(',
                items: self.until(Token::CloseParen),
            },
            Token::Function(name) => Cv::Function {
                name,
                args: self.until(Token::CloseParen),
            },
            t => Cv::Token(t),
        }
    }

    fn until(&mut self, end: Token) -> Vec<Cv> {
        let mut out = Vec::new();
        while let Some(t) = self.next() {
            if t == end {
                break;
            }
            out.push(self.component(t));
        }
        out
    }
}

/// Tokenize and parse `src` into component values.
pub fn component_values(src: &str) -> Vec<Cv> {
    let mut s = Stream {
        toks: tokenize(src),
        i: 0,
    };
    let mut out = Vec::new();
    while let Some(t) = s.next() {
        out.push(s.component(t));
    }
    out
}

/// Parse a stylesheet's top-level rules.
pub fn parse_stylesheet(src: &str) -> Vec<Rule> {
    rules_from(&component_values(src), true)
}

/// Rules in a list of component values (a stylesheet or an at-rule's
/// block). CDO/CDC are ignored at the top level.
pub fn rules_from(cvs: &[Cv], top: bool) -> Vec<Rule> {
    let mut rules = Vec::new();
    let mut i = 0;
    while i < cvs.len() {
        match &cvs[i] {
            Cv::Token(Token::Whitespace) => i += 1,
            Cv::Token(Token::Cdo | Token::Cdc) if top => i += 1,
            Cv::Token(Token::AtKeyword(name)) => {
                let (rule, next) = at_rule(name, cvs, i + 1);
                rules.push(rule);
                i = next;
            }
            _ => {
                let mut prelude = Vec::new();
                let mut done = false;
                while i < cvs.len() {
                    if let Cv::Block { open: '{', items } = &cvs[i] {
                        rules.push(Rule::Qualified {
                            prelude: core::mem::take(&mut prelude),
                            block: items.clone(),
                        });
                        i += 1;
                        done = true;
                        break;
                    }
                    prelude.push(cvs[i].clone());
                    i += 1;
                }
                if !done {
                    break; // parse error: a prelude without a block
                }
            }
        }
    }
    rules
}

fn at_rule(name: &str, cvs: &[Cv], mut i: usize) -> (Rule, usize) {
    let mut prelude = Vec::new();
    while i < cvs.len() {
        match &cvs[i] {
            Cv::Token(Token::Semicolon) => {
                return (
                    Rule::At {
                        name: name.to_ascii_lowercase(),
                        prelude,
                        block: None,
                    },
                    i + 1,
                );
            }
            Cv::Block { open: '{', items } => {
                return (
                    Rule::At {
                        name: name.to_ascii_lowercase(),
                        prelude,
                        block: Some(items.clone()),
                    },
                    i + 1,
                );
            }
            c => prelude.push(c.clone()),
        }
        i += 1;
    }
    (
        Rule::At {
            name: name.to_ascii_lowercase(),
            prelude,
            block: None,
        },
        i,
    )
}

/// Contents of a style block: declarations, and nested rules (CSS
/// Nesting) in order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BlockContents {
    pub declarations: Vec<Declaration>,
    pub rules: Vec<Rule>,
}

/// Parse a declaration list (a style block or a `style` attribute).
pub fn parse_block(cvs: &[Cv]) -> BlockContents {
    let mut out = BlockContents::default();
    let mut i = 0;
    while i < cvs.len() {
        match &cvs[i] {
            Cv::Token(Token::Whitespace | Token::Semicolon) => i += 1,
            Cv::Token(Token::AtKeyword(name)) => {
                let (rule, next) = at_rule(name, cvs, i + 1);
                out.rules.push(rule);
                i = next;
            }
            Cv::Token(Token::Ident(_)) => {
                // A declaration, unless it turns out to be a nested rule
                // (`ident ... { }` before any ';').
                let start = i;
                let mut j = i;
                while j < cvs.len() && !matches!(cvs[j], Cv::Token(Token::Semicolon)) {
                    if matches!(cvs[j], Cv::Block { open: '{', .. })
                        && !cvs[start + 1..j]
                            .iter()
                            .any(|c| matches!(c, Cv::Token(Token::Colon)))
                    {
                        break;
                    }
                    j += 1;
                }
                if j < cvs.len() && matches!(cvs[j], Cv::Block { open: '{', .. }) {
                    let r = rules_from(&cvs[start..=j], false);
                    out.rules.extend(r);
                    i = j + 1;
                    continue;
                }
                if let Some(d) = declaration(&cvs[start..j]) {
                    out.declarations.push(d);
                }
                i = j + 1;
            }
            _ => {
                // A nested rule starting with a selector token (`&`, `.a`,
                // `>`, ...), or junk up to the next ';'.
                let start = i;
                let mut j = i;
                while j < cvs.len()
                    && !matches!(cvs[j], Cv::Token(Token::Semicolon))
                    && !matches!(cvs[j], Cv::Block { open: '{', .. })
                {
                    j += 1;
                }
                if j < cvs.len() && matches!(cvs[j], Cv::Block { open: '{', .. }) {
                    out.rules.extend(rules_from(&cvs[start..=j], false));
                }
                i = j + 1;
            }
        }
    }
    out
}

fn declaration(cvs: &[Cv]) -> Option<Declaration> {
    let name = cvs.first()?.ident()?;
    let mut i = 1;
    while i < cvs.len() && cvs[i].is_ws() {
        i += 1;
    }
    if !matches!(cvs.get(i), Some(Cv::Token(Token::Colon))) {
        return None;
    }
    let mut value: Vec<Cv> = cvs[i + 1..].to_vec();
    // Trim whitespace, then `!important`.
    while value.last().is_some_and(Cv::is_ws) {
        value.pop();
    }
    let mut important = false;
    let n = value.len();
    if n >= 2
        && value[n - 1]
            .ident()
            .is_some_and(|s| s.eq_ignore_ascii_case("important"))
    {
        let mut k = n - 2;
        while k > 0 && value[k].is_ws() {
            k -= 1;
        }
        if value[k].is_delim('!') {
            important = true;
            value.truncate(k);
        }
    }
    while value.last().is_some_and(Cv::is_ws) {
        value.pop();
    }
    while value.first().is_some_and(Cv::is_ws) {
        value.remove(0);
    }
    let name = if name.starts_with("--") {
        String::from(name)
    } else {
        name.to_ascii_lowercase()
    };
    Some(Declaration {
        name,
        value,
        important,
    })
}

/// Parse a `style` attribute.
pub fn parse_style_attribute(src: &str) -> Vec<Declaration> {
    parse_block(&component_values(src)).declarations
}

/// Serialize component values back to CSS text (for custom properties,
/// `var()` substitution and diagnostics).
pub fn serialize(cvs: &[Cv]) -> String {
    let mut out = String::new();
    for c in cvs {
        serialize_one(c, &mut out);
    }
    out
}

fn serialize_one(c: &Cv, out: &mut String) {
    use core::fmt::Write;
    match c {
        Cv::Token(t) => match t {
            Token::Ident(s) => out.push_str(s),
            Token::Function(s) => {
                out.push_str(s);
                out.push('(');
            }
            Token::AtKeyword(s) => {
                out.push('@');
                out.push_str(s);
            }
            Token::Hash { value, .. } => {
                out.push('#');
                out.push_str(value);
            }
            Token::String(s) => {
                out.push('"');
                for ch in s.chars() {
                    if ch == '"' || ch == '\\' {
                        out.push('\\');
                    }
                    out.push(ch);
                }
                out.push('"');
            }
            Token::Url(s) => {
                let _ = write!(out, "url({})", s);
            }
            Token::BadString | Token::BadUrl => {}
            Token::Delim(c) => out.push(*c),
            Token::Number { value, .. } => {
                let _ = write!(out, "{}", value);
            }
            Token::Percentage(v) => {
                let _ = write!(out, "{}%", v);
            }
            Token::Dimension { value, unit } => {
                let _ = write!(out, "{}{}", value, unit);
            }
            Token::Whitespace => out.push(' '),
            Token::Cdo => out.push_str("<!--"),
            Token::Cdc => out.push_str("-->"),
            Token::Colon => out.push(':'),
            Token::Semicolon => out.push(';'),
            Token::Comma => out.push(','),
            Token::OpenSquare => out.push('['),
            Token::CloseSquare => out.push(']'),
            Token::OpenParen => out.push('('),
            Token::CloseParen => out.push(')'),
            Token::OpenCurly => out.push('{'),
            Token::CloseCurly => out.push('}'),
        },
        Cv::Function { name, args } => {
            out.push_str(name);
            out.push('(');
            for a in args {
                serialize_one(a, out);
            }
            out.push(')');
        }
        Cv::Block { open, items } => {
            out.push(*open);
            for a in items {
                serialize_one(a, out);
            }
            out.push(match open {
                '(' => ')',
                '[' => ']',
                _ => '}',
            });
        }
    }
}
