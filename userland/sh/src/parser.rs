//! Tokenizer and recursive-descent parser for a POSIX-style shell grammar.

use crate::ast::*;
use alloc::rc::Rc;
use core::cell::RefCell;
use rustos_rt::prelude::*;

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Word(Word, String), // parts + raw text
    Op(&'static str),
    Newline,
    Eof,
}

pub struct ParseError(pub String);

/// Parse shell source. `Err` with "incomplete" means more input is needed.
pub fn parse(src: &str) -> Result<List, ParseError> {
    let mut p = Parser {
        s: src.as_bytes(),
        i: 0,
        peeked: None,
        peeked_start: 0,
        pending_heredocs: Vec::new(),
    };
    let list = p.list_until(&[])?;
    match p.peek()? {
        Tok::Eof => Ok(list),
        t => Err(ParseError(format!("syntax error near {}", tok_desc(&t)))),
    }
}

fn tok_desc(t: &Tok) -> String {
    match t {
        Tok::Word(_, raw) => format!("`{}'", raw),
        Tok::Op(o) => format!("`{}'", o),
        Tok::Newline => String::from("newline"),
        Tok::Eof => String::from("end of file"),
    }
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    peeked: Option<Tok>,
    /// Source offset where the peeked token starts.
    peeked_start: usize,
    /// (delimiter, strip tabs, body) waiting for the next newline.
    pending_heredocs: Vec<(String, bool, Rc<RefCell<String>>)>,
}

const OPS: [&str; 18] = [
    "&&", "||", ";;", "<<-", "<<", ">>", "<&", ">&", "<>", ">|", "&>", "|", "&", ";", "<", ">",
    "(", ")",
];

fn is_word_end(c: u8) -> bool {
    matches!(
        c,
        b' ' | b'\t' | b'\n' | b';' | b'&' | b'|' | b'<' | b'>' | b'(' | b')'
    )
}

impl<'a> Parser<'a> {
    fn incomplete<T>(&self) -> Result<T, ParseError> {
        Err(ParseError(String::from("incomplete")))
    }

    fn peek(&mut self) -> Result<Tok, ParseError> {
        if self.peeked.is_none() {
            self.skip_blanks();
            self.peeked_start = self.i;
            let t = self.lex()?;
            self.peeked = Some(t);
        }
        Ok(self.peeked.clone().unwrap())
    }

    fn next(&mut self) -> Result<Tok, ParseError> {
        let t = self.peek()?;
        self.peeked = None;
        Ok(t)
    }

    fn skip_blanks(&mut self) {
        loop {
            while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t') {
                self.i += 1;
            }
            if self.i + 1 < self.s.len() && self.s[self.i] == b'\\' && self.s[self.i + 1] == b'\n' {
                self.i += 2;
                continue;
            }
            if self.i < self.s.len() && self.s[self.i] == b'#' {
                while self.i < self.s.len() && self.s[self.i] != b'\n' {
                    self.i += 1;
                }
            }
            break;
        }
    }

    fn lex(&mut self) -> Result<Tok, ParseError> {
        self.skip_blanks();
        if self.i >= self.s.len() {
            if !self.pending_heredocs.is_empty() {
                return self.incomplete();
            }
            return Ok(Tok::Eof);
        }
        let c = self.s[self.i];
        if c == b'\n' {
            self.i += 1;
            self.read_heredocs()?;
            return Ok(Tok::Newline);
        }
        // IO number: digits immediately followed by < or >.
        if c.is_ascii_digit() {
            let mut j = self.i;
            while j < self.s.len() && self.s[j].is_ascii_digit() {
                j += 1;
            }
            if j < self.s.len() && matches!(self.s[j], b'<' | b'>') {
                let n = core::str::from_utf8(&self.s[self.i..j])
                    .unwrap()
                    .to_string();
                self.i = j;
                return Ok(Tok::Word(
                    alloc::vec![WordPart::Lit(n.clone())],
                    format!("#io{}", n),
                ));
            }
        }
        for op in OPS {
            if self.s[self.i..].starts_with(op.as_bytes()) {
                self.i += op.len();
                return Ok(Tok::Op(op));
            }
        }
        self.word()
    }

    fn read_heredocs(&mut self) -> Result<(), ParseError> {
        let pending = core::mem::take(&mut self.pending_heredocs);
        for (delim, strip, target) in pending {
            let mut body = String::new();
            loop {
                if self.i >= self.s.len() {
                    return self.incomplete();
                }
                let end = self.s[self.i..]
                    .iter()
                    .position(|&b| b == b'\n')
                    .map(|p| self.i + p);
                let line_end = match end {
                    Some(e) => e,
                    None => return self.incomplete(),
                };
                let mut line = core::str::from_utf8(&self.s[self.i..line_end]).unwrap_or("");
                self.i = line_end + 1;
                if strip {
                    line = line.trim_start_matches('\t');
                }
                if line == delim {
                    break;
                }
                body.push_str(line);
                body.push('\n');
            }
            *target.borrow_mut() = body;
        }
        Ok(())
    }

    fn word(&mut self) -> Result<Tok, ParseError> {
        let start = self.i;
        let mut parts: Word = Vec::new();
        let mut lit = String::new();
        let flush = |parts: &mut Word, lit: &mut String| {
            if !lit.is_empty() {
                parts.push(WordPart::Lit(core::mem::take(lit)));
            }
        };
        // Tilde prefix.
        if self.s[self.i] == b'~' {
            let mut j = self.i + 1;
            while j < self.s.len() && !is_word_end(self.s[j]) && self.s[j] != b'/' {
                j += 1;
            }
            let user = core::str::from_utf8(&self.s[self.i + 1..j])
                .unwrap_or("")
                .to_string();
            if user.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                parts.push(WordPart::Tilde(user));
                self.i = j;
            }
        }
        while self.i < self.s.len() {
            let c = self.s[self.i];
            if is_word_end(c) {
                break;
            }
            match c {
                b'\\' => {
                    if self.i + 1 >= self.s.len() {
                        self.i += 1;
                        break;
                    }
                    let n = self.s[self.i + 1];
                    self.i += 2;
                    if n == b'\n' {
                        continue;
                    }
                    flush(&mut parts, &mut lit);
                    parts.push(WordPart::Quoted(String::from(n as char)));
                }
                b'\'' => {
                    let end = match self.s[self.i + 1..].iter().position(|&b| b == b'\'') {
                        Some(p) => self.i + 1 + p,
                        None => return self.incomplete(),
                    };
                    flush(&mut parts, &mut lit);
                    parts.push(WordPart::Quoted(
                        String::from_utf8_lossy(&self.s[self.i + 1..end]).into_owned(),
                    ));
                    self.i = end + 1;
                }
                b'"' => {
                    flush(&mut parts, &mut lit);
                    self.i += 1;
                    self.double_quoted(&mut parts)?;
                }
                b'$' => {
                    flush(&mut parts, &mut lit);
                    self.dollar(&mut parts, false)?;
                }
                b'`' => {
                    flush(&mut parts, &mut lit);
                    let body = self.backtick()?;
                    parts.push(WordPart::CmdSub(body, false));
                }
                _ => {
                    // Multi-byte UTF-8 is copied through untouched.
                    let ch_len = utf8_len(c);
                    let end = (self.i + ch_len).min(self.s.len());
                    lit.push_str(&String::from_utf8_lossy(&self.s[self.i..end]));
                    self.i = end;
                }
            }
        }
        flush(&mut parts, &mut lit);
        let raw = String::from_utf8_lossy(&self.s[start..self.i]).into_owned();
        Ok(Tok::Word(parts, raw))
    }

    fn double_quoted(&mut self, parts: &mut Word) -> Result<(), ParseError> {
        let mut lit = String::new();
        loop {
            if self.i >= self.s.len() {
                return self.incomplete();
            }
            let c = self.s[self.i];
            match c {
                b'"' => {
                    self.i += 1;
                    break;
                }
                b'\\' => {
                    if self.i + 1 >= self.s.len() {
                        return self.incomplete();
                    }
                    let n = self.s[self.i + 1];
                    if matches!(n, b'$' | b'`' | b'"' | b'\\') {
                        lit.push(n as char);
                    } else if n != b'\n' {
                        lit.push('\\');
                        lit.push(n as char);
                    }
                    self.i += 2;
                }
                b'$' => {
                    if !lit.is_empty() {
                        parts.push(WordPart::Quoted(core::mem::take(&mut lit)));
                    }
                    self.dollar(parts, true)?;
                }
                b'`' => {
                    if !lit.is_empty() {
                        parts.push(WordPart::Quoted(core::mem::take(&mut lit)));
                    }
                    let body = self.backtick()?;
                    parts.push(WordPart::CmdSub(body, true));
                }
                _ => {
                    let ch_len = utf8_len(c);
                    let end = (self.i + ch_len).min(self.s.len());
                    lit.push_str(&String::from_utf8_lossy(&self.s[self.i..end]));
                    self.i = end;
                }
            }
        }
        // Keep empty "" as an (empty) quoted part so it produces a field.
        parts.push(WordPart::Quoted(lit));
        Ok(())
    }

    fn backtick(&mut self) -> Result<String, ParseError> {
        self.i += 1;
        let mut body = String::new();
        loop {
            if self.i >= self.s.len() {
                return self.incomplete();
            }
            let c = self.s[self.i];
            if c == b'`' {
                self.i += 1;
                return Ok(body);
            }
            if c == b'\\'
                && self.i + 1 < self.s.len()
                && matches!(self.s[self.i + 1], b'`' | b'\\' | b'$')
            {
                body.push(self.s[self.i + 1] as char);
                self.i += 2;
                continue;
            }
            body.push(c as char);
            self.i += 1;
        }
    }

    /// Find the matching `close` for an already-consumed opener, honouring
    /// quotes and nesting.
    fn balanced(&mut self, open: u8, close: u8) -> Result<String, ParseError> {
        let start = self.i;
        let mut depth = 1;
        while self.i < self.s.len() {
            let c = self.s[self.i];
            match c {
                b'\\' => self.i += 1,
                b'\'' => match self.s[self.i + 1..].iter().position(|&b| b == b'\'') {
                    Some(p) => self.i += p + 1,
                    None => return self.incomplete(),
                },
                b'"' => {
                    self.i += 1;
                    while self.i < self.s.len() && self.s[self.i] != b'"' {
                        if self.s[self.i] == b'\\' {
                            self.i += 1;
                        }
                        self.i += 1;
                    }
                }
                _ if c == open => depth += 1,
                _ if c == close => {
                    depth -= 1;
                    if depth == 0 {
                        let s = String::from_utf8_lossy(&self.s[start..self.i]).into_owned();
                        self.i += 1;
                        return Ok(s);
                    }
                }
                _ => {}
            }
            self.i += 1;
        }
        self.incomplete()
    }

    fn dollar(&mut self, parts: &mut Word, quoted: bool) -> Result<(), ParseError> {
        self.i += 1;
        if self.i >= self.s.len() {
            parts.push(WordPart::Quoted(String::from("$")));
            return Ok(());
        }
        let c = self.s[self.i];
        if self.s[self.i..].starts_with(b"((") {
            self.i += 2;
            let body = self.balanced(b'(', b')')?;
            // Consume the second closing paren.
            if self.i < self.s.len() && self.s[self.i] == b')' {
                self.i += 1;
                parts.push(WordPart::Arith(body));
            } else {
                parts.push(WordPart::CmdSub(format!("({}", body), quoted));
            }
            return Ok(());
        }
        match c {
            b'(' => {
                self.i += 1;
                let body = self.balanced(b'(', b')')?;
                parts.push(WordPart::CmdSub(body, quoted));
            }
            b'{' => {
                self.i += 1;
                let body = self.balanced(b'{', b'}')?;
                parts.push(WordPart::Param(body, quoted));
            }
            b'?' | b'$' | b'!' | b'#' | b'@' | b'*' | b'-' | b'0'..=b'9' => {
                self.i += 1;
                parts.push(WordPart::Param(String::from(c as char), quoted));
            }
            c if c == b'_' || c.is_ascii_alphabetic() => {
                let start = self.i;
                while self.i < self.s.len()
                    && (self.s[self.i] == b'_' || self.s[self.i].is_ascii_alphanumeric())
                {
                    self.i += 1;
                }
                let name = String::from_utf8_lossy(&self.s[start..self.i]).into_owned();
                parts.push(WordPart::Param(name, quoted));
            }
            _ => parts.push(WordPart::Quoted(String::from("$"))),
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Grammar
    // ------------------------------------------------------------------

    fn skip_newlines(&mut self) -> Result<(), ParseError> {
        while self.peek()? == Tok::Newline {
            self.next()?;
        }
        Ok(())
    }

    fn is_keyword(t: &Tok, kw: &str) -> bool {
        matches!(t, Tok::Word(_, raw) if raw == kw)
    }

    /// Parse and-or lists until one of `stops` (reserved words) or EOF/`)`.
    fn list_until(&mut self, stops: &[&str]) -> Result<List, ParseError> {
        let mut list = Vec::new();
        loop {
            self.skip_newlines()?;
            let t = self.peek()?;
            if t == Tok::Eof || t == Tok::Op(")") || t == Tok::Op(";;") {
                break;
            }
            if stops.iter().any(|s| Self::is_keyword(&t, s)) {
                break;
            }
            let start = self.peeked_start;
            let mut ao = self.and_or()?;
            match self.peek()? {
                Tok::Op("&") => {
                    self.next()?;
                    ao.background = true;
                }
                Tok::Op(";") | Tok::Newline => {
                    self.next()?;
                }
                _ => {}
            }
            let end = self.i.min(self.s.len());
            ao.text = String::from_utf8_lossy(&self.s[start.min(end)..end])
                .trim()
                .trim_end_matches(['&', ';'])
                .trim()
                .to_string();
            list.push(ao);
        }
        Ok(list)
    }

    fn and_or(&mut self) -> Result<AndOr, ParseError> {
        let first = self.pipeline()?;
        let mut rest = Vec::new();
        loop {
            let c = match self.peek()? {
                Tok::Op("&&") => Connector::And,
                Tok::Op("||") => Connector::Or,
                _ => break,
            };
            self.next()?;
            self.skip_newlines()?;
            rest.push((c, self.pipeline()?));
        }
        Ok(AndOr {
            first,
            rest,
            background: false,
            text: String::new(),
        })
    }

    fn pipeline(&mut self) -> Result<Pipeline, ParseError> {
        let mut negate = false;
        if Self::is_keyword(&self.peek()?, "!") {
            self.next()?;
            negate = true;
        }
        let mut cmds = alloc::vec![self.command()?];
        while self.peek()? == Tok::Op("|") {
            self.next()?;
            self.skip_newlines()?;
            cmds.push(self.command()?);
        }
        Ok(Pipeline { negate, cmds })
    }

    fn expect_kw(&mut self, kw: &str) -> Result<(), ParseError> {
        self.skip_newlines()?;
        let t = self.next()?;
        if Self::is_keyword(&t, kw) {
            Ok(())
        } else if t == Tok::Eof {
            self.incomplete()
        } else {
            Err(ParseError(format!(
                "syntax error: expected `{}' near {}",
                kw,
                tok_desc(&t)
            )))
        }
    }

    fn redirs(&mut self, out: &mut Vec<Redir>) -> Result<(), ParseError> {
        while let Some(r) = self.redir()? {
            out.push(r);
        }
        Ok(())
    }

    fn redir(&mut self) -> Result<Option<Redir>, ParseError> {
        let mut fd: Option<i32> = None;
        if let Tok::Word(p, raw) = self.peek()?
            && let Some(n) = raw.strip_prefix("#io")
        {
            let _ = p;
            fd = n.parse().ok();
            self.next()?;
        }
        let op = match self.peek()? {
            Tok::Op(o @ ("<" | ">" | ">>" | "<<" | "<<-" | "<&" | ">&" | "<>" | ">|" | "&>")) => o,
            _ => {
                if fd.is_some() {
                    return Err(ParseError(String::from("syntax error: bad redirection")));
                }
                return Ok(None);
            }
        };
        self.next()?;
        let target = match self.next()? {
            Tok::Word(w, _) => w,
            Tok::Eof => return self.incomplete(),
            t => return Err(ParseError(format!("syntax error near {}", tok_desc(&t)))),
        };
        let r = match op {
            "<" => Redir {
                fd: fd.unwrap_or(0),
                kind: RedirKind::In,
                target,
            },
            ">" | ">|" => Redir {
                fd: fd.unwrap_or(1),
                kind: RedirKind::Out,
                target,
            },
            ">>" => Redir {
                fd: fd.unwrap_or(1),
                kind: RedirKind::Append,
                target,
            },
            "<>" => Redir {
                fd: fd.unwrap_or(0),
                kind: RedirKind::ReadWrite,
                target,
            },
            "<&" => Redir {
                fd: fd.unwrap_or(0),
                kind: RedirKind::Dup,
                target,
            },
            ">&" => Redir {
                fd: fd.unwrap_or(1),
                kind: RedirKind::Dup,
                target,
            },
            "&>" => Redir {
                fd: -1, // both stdout and stderr
                kind: RedirKind::Out,
                target,
            },
            _ => {
                // Here-document: body is read at the next newline.
                let quoted = target.iter().any(|p| matches!(p, WordPart::Quoted(_)));
                let delim: String = target
                    .iter()
                    .map(|p| match p {
                        WordPart::Lit(s) | WordPart::Quoted(s) => s.clone(),
                        _ => String::new(),
                    })
                    .collect();
                let body = Rc::new(RefCell::new(String::new()));
                self.pending_heredocs
                    .push((delim, op == "<<-", body.clone()));
                return Ok(Some(Redir {
                    fd: fd.unwrap_or(0),
                    kind: RedirKind::Here(body, !quoted),
                    target: Vec::new(),
                }));
            }
        };
        Ok(Some(r))
    }

    fn command(&mut self) -> Result<Command, ParseError> {
        let t = self.peek()?;
        if let Tok::Word(_, raw) = &t {
            match raw.as_str() {
                "if" => return self.if_cmd(),
                "while" | "until" => return self.while_cmd(raw == "until"),
                "for" => return self.for_cmd(),
                "case" => return self.case_cmd(),
                "{" => {
                    self.next()?;
                    let body = self.list_until(&["}"])?;
                    self.expect_kw("}")?;
                    let mut redirs = Vec::new();
                    self.redirs(&mut redirs)?;
                    return Ok(Command::Group(body, redirs));
                }
                "function" => {
                    self.next()?;
                    let name = match self.next()? {
                        Tok::Word(_, n) => n,
                        _ => return Err(ParseError(String::from("syntax error: function name"))),
                    };
                    if self.peek()? == Tok::Op("(") {
                        self.next()?;
                        if self.next()? != Tok::Op(")") {
                            return Err(ParseError(String::from("syntax error: expected )")));
                        }
                    }
                    self.skip_newlines()?;
                    let body = self.command()?;
                    return Ok(Command::FuncDef(name, Box::new(body)));
                }
                _ => {}
            }
        }
        if t == Tok::Op("(") {
            self.next()?;
            let body = self.list_until(&[])?;
            match self.next()? {
                Tok::Op(")") => {}
                Tok::Eof => return self.incomplete(),
                t => return Err(ParseError(format!("syntax error near {}", tok_desc(&t)))),
            }
            let mut redirs = Vec::new();
            self.redirs(&mut redirs)?;
            return Ok(Command::Subshell(body, redirs));
        }
        self.simple()
    }

    fn simple(&mut self) -> Result<Command, ParseError> {
        let mut assigns = Vec::new();
        let mut words: Vec<Word> = Vec::new();
        let mut redirs = Vec::new();
        loop {
            if let Some(r) = self.redir()? {
                redirs.push(r);
                continue;
            }
            match self.peek()? {
                Tok::Word(w, raw) => {
                    // name() { ... } function definition
                    if words.is_empty() && assigns.is_empty() {
                        let save_i = self.i;
                        self.next()?;
                        if self.peek()? == Tok::Op("(") {
                            let save2 = self.i;
                            self.next()?;
                            if self.peek()? == Tok::Op(")") {
                                self.next()?;
                                self.skip_newlines()?;
                                let body = self.command()?;
                                return Ok(Command::FuncDef(raw, Box::new(body)));
                            }
                            self.i = save2;
                            self.peeked = Some(Tok::Op("("));
                            words.push(w);
                            let _ = save_i;
                            continue;
                        }
                        if let Some((name, val)) = assignment(&w, &raw) {
                            assigns.push((name, val));
                        } else {
                            words.push(w);
                        }
                        continue;
                    }
                    self.next()?;
                    if words.is_empty()
                        && let Some((name, val)) = assignment(&w, &raw)
                    {
                        assigns.push((name, val));
                        continue;
                    }
                    words.push(w);
                }
                _ => break,
            }
        }
        if words.is_empty() && assigns.is_empty() && redirs.is_empty() {
            let t = self.peek()?;
            if t == Tok::Eof {
                return self.incomplete();
            }
            return Err(ParseError(format!("syntax error near {}", tok_desc(&t))));
        }
        Ok(Command::Simple {
            assigns,
            words,
            redirs,
        })
    }

    fn if_cmd(&mut self) -> Result<Command, ParseError> {
        self.next()?; // if
        let mut branches = Vec::new();
        let cond = self.list_until(&["then"])?;
        self.expect_kw("then")?;
        let body = self.list_until(&["elif", "else", "fi"])?;
        branches.push((cond, body));
        let mut otherwise = None;
        loop {
            self.skip_newlines()?;
            let t = self.next()?;
            if Self::is_keyword(&t, "elif") {
                let c = self.list_until(&["then"])?;
                self.expect_kw("then")?;
                let b = self.list_until(&["elif", "else", "fi"])?;
                branches.push((c, b));
            } else if Self::is_keyword(&t, "else") {
                otherwise = Some(self.list_until(&["fi"])?);
                self.expect_kw("fi")?;
                break;
            } else if Self::is_keyword(&t, "fi") {
                break;
            } else if t == Tok::Eof {
                return self.incomplete();
            } else {
                return Err(ParseError(format!("syntax error near {}", tok_desc(&t))));
            }
        }
        let mut redirs = Vec::new();
        self.redirs(&mut redirs)?;
        Ok(Command::If {
            branches,
            otherwise,
            redirs,
        })
    }

    fn while_cmd(&mut self, until: bool) -> Result<Command, ParseError> {
        self.next()?;
        let cond = self.list_until(&["do"])?;
        self.expect_kw("do")?;
        let body = self.list_until(&["done"])?;
        self.expect_kw("done")?;
        let mut redirs = Vec::new();
        self.redirs(&mut redirs)?;
        Ok(Command::While {
            cond,
            body,
            until,
            redirs,
        })
    }

    fn for_cmd(&mut self) -> Result<Command, ParseError> {
        self.next()?;
        let var = match self.next()? {
            Tok::Word(_, n) => n,
            Tok::Eof => return self.incomplete(),
            _ => return Err(ParseError(String::from("syntax error: for variable"))),
        };
        let mut items = None;
        self.skip_newlines()?;
        if Self::is_keyword(&self.peek()?, "in") {
            self.next()?;
            let mut v = Vec::new();
            while let Tok::Word(w, _) = self.peek()? {
                self.next()?;
                v.push(w);
            }
            items = Some(v);
        }
        if matches!(self.peek()?, Tok::Op(";")) {
            self.next()?;
        }
        self.expect_kw("do")?;
        let body = self.list_until(&["done"])?;
        self.expect_kw("done")?;
        let mut redirs = Vec::new();
        self.redirs(&mut redirs)?;
        Ok(Command::For {
            var,
            items,
            body,
            redirs,
        })
    }

    fn case_cmd(&mut self) -> Result<Command, ParseError> {
        self.next()?;
        let word = match self.next()? {
            Tok::Word(w, _) => w,
            Tok::Eof => return self.incomplete(),
            _ => return Err(ParseError(String::from("syntax error: case word"))),
        };
        self.expect_kw("in")?;
        let mut arms = Vec::new();
        loop {
            self.skip_newlines()?;
            let t = self.peek()?;
            if Self::is_keyword(&t, "esac") {
                self.next()?;
                break;
            }
            if t == Tok::Eof {
                return self.incomplete();
            }
            if t == Tok::Op("(") {
                self.next()?;
            }
            let mut pats = Vec::new();
            loop {
                match self.next()? {
                    Tok::Word(w, _) => pats.push(w),
                    Tok::Eof => return self.incomplete(),
                    t => return Err(ParseError(format!("syntax error near {}", tok_desc(&t)))),
                }
                match self.next()? {
                    Tok::Op("|") => continue,
                    Tok::Op(")") => break,
                    Tok::Eof => return self.incomplete(),
                    t => return Err(ParseError(format!("syntax error near {}", tok_desc(&t)))),
                }
            }
            let body = self.list_until(&["esac"])?;
            if self.peek()? == Tok::Op(";;") {
                self.next()?;
            }
            arms.push((pats, body));
        }
        let mut redirs = Vec::new();
        self.redirs(&mut redirs)?;
        Ok(Command::Case { word, arms, redirs })
    }
}

fn assignment(w: &Word, raw: &str) -> Option<(String, Word)> {
    let eq = raw.find('=')?;
    let name = &raw[..eq];
    if name.is_empty()
        || !name.chars().all(|c| c == '_' || c.is_ascii_alphanumeric())
        || name.as_bytes()[0].is_ascii_digit()
    {
        return None;
    }
    // Split the parsed parts at the first '=' in the leading literal.
    let mut parts = w.clone();
    match parts.first_mut() {
        Some(WordPart::Lit(s)) if s.len() > eq => {
            let rest = s[eq + 1..].to_string();
            if rest.is_empty() {
                parts.remove(0);
            } else {
                *s = rest;
            }
        }
        Some(WordPart::Lit(s)) if s.len() == eq + 1 => {
            parts.remove(0);
        }
        _ => return None,
    }
    Some((name.to_string(), parts))
}

fn utf8_len(c: u8) -> usize {
    match c {
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1,
    }
}
