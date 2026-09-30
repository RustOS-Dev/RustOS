//! CSS Syntax Level 3 tokenizer (§4).

use alloc::string::String;
use alloc::vec::Vec;

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Ident(String),
    Function(String),
    AtKeyword(String),
    /// `#name`; `id` is true when the name is a valid identifier.
    Hash {
        value: String,
        id: bool,
    },
    String(String),
    BadString,
    Url(String),
    BadUrl,
    Delim(char),
    Number {
        value: f32,
        int: bool,
    },
    Percentage(f32),
    Dimension {
        value: f32,
        unit: String,
    },
    Whitespace,
    Cdo,
    Cdc,
    Colon,
    Semicolon,
    Comma,
    OpenSquare,
    CloseSquare,
    OpenParen,
    CloseParen,
    OpenCurly,
    CloseCurly,
}

struct Lexer<'a> {
    s: &'a [char],
    i: usize,
}

fn is_name_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || !c.is_ascii()
}

fn is_name(c: char) -> bool {
    is_name_start(c) || c.is_ascii_digit() || c == '-'
}

impl Lexer<'_> {
    fn peek(&self, k: usize) -> Option<char> {
        self.s.get(self.i + k).copied()
    }

    fn valid_escape(&self, k: usize) -> bool {
        self.peek(k) == Some('\\') && !matches!(self.peek(k + 1), None | Some('\n'))
    }

    fn starts_ident(&self, k: usize) -> bool {
        match self.peek(k) {
            Some('-') => {
                matches!(self.peek(k + 1), Some(c) if is_name_start(c) || c == '-')
                    || self.valid_escape(k + 1)
            }
            Some('\\') => self.valid_escape(k),
            Some(c) => is_name_start(c),
            None => false,
        }
    }

    fn starts_number(&self, k: usize) -> bool {
        match self.peek(k) {
            Some('+' | '-') => {
                matches!(self.peek(k + 1), Some(c) if c.is_ascii_digit())
                    || (self.peek(k + 1) == Some('.')
                        && matches!(self.peek(k + 2), Some(c) if c.is_ascii_digit()))
            }
            Some('.') => matches!(self.peek(k + 1), Some(c) if c.is_ascii_digit()),
            Some(c) => c.is_ascii_digit(),
            None => false,
        }
    }

    fn escape(&mut self) -> char {
        // After the backslash.
        let Some(c) = self.peek(0) else {
            return '\u{FFFD}';
        };
        if c.is_ascii_hexdigit() {
            let mut v = 0u32;
            let mut n = 0;
            while n < 6 && matches!(self.peek(0), Some(h) if h.is_ascii_hexdigit()) {
                v = v * 16 + self.peek(0).unwrap().to_digit(16).unwrap();
                self.i += 1;
                n += 1;
            }
            if matches!(self.peek(0), Some(' ' | '\t' | '\n')) {
                self.i += 1;
            }
            match char::from_u32(v) {
                Some(ch) if v != 0 => ch,
                _ => '\u{FFFD}',
            }
        } else {
            self.i += 1;
            c
        }
    }

    fn name(&mut self) -> String {
        let mut out = String::new();
        loop {
            match self.peek(0) {
                Some(c) if is_name(c) => {
                    out.push(c);
                    self.i += 1;
                }
                Some('\\') if self.valid_escape(0) => {
                    self.i += 1;
                    out.push(self.escape());
                }
                _ => return out,
            }
        }
    }

    fn number(&mut self) -> (f32, bool) {
        let start = self.i;
        let mut int = true;
        if matches!(self.peek(0), Some('+' | '-')) {
            self.i += 1;
        }
        while matches!(self.peek(0), Some(c) if c.is_ascii_digit()) {
            self.i += 1;
        }
        if self.peek(0) == Some('.') && matches!(self.peek(1), Some(c) if c.is_ascii_digit()) {
            int = false;
            self.i += 1;
            while matches!(self.peek(0), Some(c) if c.is_ascii_digit()) {
                self.i += 1;
            }
        }
        if matches!(self.peek(0), Some('e' | 'E')) {
            let k = if matches!(self.peek(1), Some('+' | '-')) {
                2
            } else {
                1
            };
            if matches!(self.peek(k), Some(c) if c.is_ascii_digit()) {
                int = false;
                self.i += k;
                while matches!(self.peek(0), Some(c) if c.is_ascii_digit()) {
                    self.i += 1;
                }
            }
        }
        let text: String = self.s[start..self.i].iter().collect();
        (parse_f32(&text), int)
    }

    fn numeric(&mut self) -> Token {
        let (value, int) = self.number();
        if self.starts_ident(0) {
            let unit = self.name();
            Token::Dimension { value, unit }
        } else if self.peek(0) == Some('%') {
            self.i += 1;
            Token::Percentage(value)
        } else {
            Token::Number { value, int }
        }
    }

    fn string(&mut self, quote: char) -> Token {
        let mut out = String::new();
        loop {
            match self.peek(0) {
                None => return Token::String(out),
                Some(c) if c == quote => {
                    self.i += 1;
                    return Token::String(out);
                }
                Some('\n') => return Token::BadString,
                Some('\\') => match self.peek(1) {
                    None => self.i += 1,
                    Some('\n') => self.i += 2,
                    Some(_) => {
                        self.i += 1;
                        out.push(self.escape());
                    }
                },
                Some(c) => {
                    out.push(c);
                    self.i += 1;
                }
            }
        }
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(0), Some(' ' | '\t' | '\n')) {
            self.i += 1;
        }
    }

    fn url(&mut self) -> Token {
        self.skip_ws();
        let mut out = String::new();
        loop {
            match self.peek(0) {
                None => return Token::Url(out),
                Some(')') => {
                    self.i += 1;
                    return Token::Url(out);
                }
                Some(' ' | '\t' | '\n') => {
                    self.skip_ws();
                    if matches!(self.peek(0), Some(')') | None) {
                        self.i += 1;
                        return Token::Url(out);
                    }
                    return self.bad_url();
                }
                Some('"' | '\'' | '(') => return self.bad_url(),
                Some('\\') => {
                    if self.valid_escape(0) {
                        self.i += 1;
                        out.push(self.escape());
                    } else {
                        return self.bad_url();
                    }
                }
                Some(c) => {
                    out.push(c);
                    self.i += 1;
                }
            }
        }
    }

    fn bad_url(&mut self) -> Token {
        loop {
            match self.peek(0) {
                None => return Token::BadUrl,
                Some(')') => {
                    self.i += 1;
                    return Token::BadUrl;
                }
                Some('\\') if self.valid_escape(0) => {
                    self.i += 1;
                    self.escape();
                }
                _ => self.i += 1,
            }
        }
    }

    fn ident_like(&mut self) -> Token {
        let name = self.name();
        if self.peek(0) == Some('(') {
            self.i += 1;
            if name.eq_ignore_ascii_case("url") {
                let save = self.i;
                self.skip_ws();
                if matches!(self.peek(0), Some('"' | '\'')) {
                    self.i = save;
                    return Token::Function(name);
                }
                self.i = save;
                return self.url();
            }
            return Token::Function(name);
        }
        Token::Ident(name)
    }

    fn next(&mut self) -> Option<Token> {
        // Comments.
        while self.peek(0) == Some('/') && self.peek(1) == Some('*') {
            self.i += 2;
            while self.i < self.s.len() && !(self.peek(0) == Some('*') && self.peek(1) == Some('/'))
            {
                self.i += 1;
            }
            self.i = (self.i + 2).min(self.s.len());
        }
        let c = self.peek(0)?;
        Some(match c {
            ' ' | '\t' | '\n' => {
                self.skip_ws();
                Token::Whitespace
            }
            '"' | '\'' => {
                self.i += 1;
                self.string(c)
            }
            '#' => {
                if matches!(self.peek(1), Some(n) if is_name(n)) || self.valid_escape(1) {
                    self.i += 1;
                    let id = self.starts_ident(0);
                    Token::Hash {
                        value: self.name(),
                        id,
                    }
                } else {
                    self.i += 1;
                    Token::Delim('#')
                }
            }
            '(' => {
                self.i += 1;
                Token::OpenParen
            }
            ')' => {
                self.i += 1;
                Token::CloseParen
            }
            '+' | '.' if self.starts_number(0) => self.numeric(),
            ',' => {
                self.i += 1;
                Token::Comma
            }
            '-' => {
                if self.starts_number(0) {
                    self.numeric()
                } else if self.peek(1) == Some('-') && self.peek(2) == Some('>') {
                    self.i += 3;
                    Token::Cdc
                } else if self.starts_ident(0) {
                    self.ident_like()
                } else {
                    self.i += 1;
                    Token::Delim('-')
                }
            }
            ':' => {
                self.i += 1;
                Token::Colon
            }
            ';' => {
                self.i += 1;
                Token::Semicolon
            }
            '<' if self.peek(1) == Some('!')
                && self.peek(2) == Some('-')
                && self.peek(3) == Some('-') =>
            {
                self.i += 4;
                Token::Cdo
            }
            '@' => {
                self.i += 1;
                if self.starts_ident(0) {
                    Token::AtKeyword(self.name())
                } else {
                    Token::Delim('@')
                }
            }
            '[' => {
                self.i += 1;
                Token::OpenSquare
            }
            '\\' => {
                if self.valid_escape(0) {
                    self.ident_like()
                } else {
                    self.i += 1;
                    Token::Delim('\\')
                }
            }
            ']' => {
                self.i += 1;
                Token::CloseSquare
            }
            '{' => {
                self.i += 1;
                Token::OpenCurly
            }
            '}' => {
                self.i += 1;
                Token::CloseCurly
            }
            c if c.is_ascii_digit() => self.numeric(),
            c if is_name_start(c) => self.ident_like(),
            c => {
                self.i += 1;
                Token::Delim(c)
            }
        })
    }
}

/// Parse a CSS number (digits, optional fraction and exponent).
pub fn parse_f32(s: &str) -> f32 {
    let b = s.as_bytes();
    let mut i = 0;
    let mut sign = 1.0f64;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        if b[i] == b'-' {
            sign = -1.0;
        }
        i += 1;
    }
    let mut v = 0f64;
    while i < b.len() && b[i].is_ascii_digit() {
        v = v * 10.0 + (b[i] - b'0') as f64;
        i += 1;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        let mut scale = 0.1;
        while i < b.len() && b[i].is_ascii_digit() {
            v += (b[i] - b'0') as f64 * scale;
            scale /= 10.0;
            i += 1;
        }
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        let mut esign = 1i32;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            if b[i] == b'-' {
                esign = -1;
            }
            i += 1;
        }
        let mut e = 0i32;
        while i < b.len() && b[i].is_ascii_digit() {
            e = (e * 10 + (b[i] - b'0') as i32).min(400);
            i += 1;
        }
        let mut p = 1f64;
        for _ in 0..e {
            p *= 10.0;
        }
        if esign < 0 {
            v /= p;
        } else {
            v *= p;
        }
    }
    (sign * v) as f32
}

/// Preprocess (§3.3: CR/FF/CRLF to LF, NUL to U+FFFD) and tokenize.
pub fn tokenize(src: &str) -> Vec<Token> {
    let chars: Vec<char> = {
        let mut v = Vec::with_capacity(src.len());
        let mut it = src.chars().peekable();
        while let Some(c) = it.next() {
            match c {
                '\r' => {
                    if it.peek() == Some(&'\n') {
                        it.next();
                    }
                    v.push('\n');
                }
                '\x0C' => v.push('\n'),
                '\0' => v.push('\u{FFFD}'),
                c => v.push(c),
            }
        }
        v
    };
    let mut lx = Lexer { s: &chars, i: 0 };
    let mut out = Vec::new();
    while let Some(t) = lx.next() {
        out.push(t);
    }
    out
}
