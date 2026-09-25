//! A small backtracking regular-expression engine (POSIX ERE syntax, with
//! BRE mode translating `\( \) \{ \} \| \+ \?`).
//!
//! Supports: literals, `.`, bracket expressions (ranges, negation, POSIX
//! classes), anchors `^ $`, quantifiers `* + ? {m,n}` (greedy), alternation,
//! groups (with capture for sed back-references), `\d \w \s \b` and escapes.

use rustos_rt::prelude::*;

#[derive(Debug, Clone)]
enum Node {
    Char(char),
    Any,
    Class(Vec<(char, char)>, bool),
    Start,
    End,
    WordBoundary,
    Group(Box<Node>, usize),
    Concat(Vec<Node>),
    Alt(Vec<Node>),
    Repeat(Box<Node>, u32, u32),
    BackRef(usize),
}

pub struct Regex {
    node: Node,
    groups: usize,
    icase: bool,
}

impl Regex {
    pub fn new(pat: &str, extended: bool, icase: bool) -> Result<Regex, String> {
        let pat = if extended {
            pat.to_string()
        } else {
            bre_to_ere(pat)
        };
        let chars: Vec<char> = pat.chars().collect();
        let mut p = Parser {
            c: &chars,
            i: 0,
            groups: 0,
        };
        let node = p.alt()?;
        if p.i != chars.len() {
            return Err(String::from("unmatched )"));
        }
        Ok(Regex {
            node,
            groups: p.groups,
            icase,
        })
    }

    /// Find the leftmost match: (start, end, capture groups).
    pub fn find_at(
        &self,
        text: &str,
        from: usize,
    ) -> Option<(usize, usize, Vec<Option<(usize, usize)>>)> {
        let chars: Vec<char> = text.chars().collect();
        let byte_idx: Vec<usize> = text
            .char_indices()
            .map(|(i, _)| i)
            .chain(core::iter::once(text.len()))
            .collect();
        let from_char = byte_idx
            .iter()
            .position(|&b| b >= from)
            .unwrap_or(chars.len());
        for start in from_char..=chars.len() {
            let mut caps = alloc::vec![None; self.groups + 1];
            let m = Matcher {
                t: &chars,
                icase: self.icase,
            };
            if let Some(end) = m.m(&self.node, start, &mut caps, &mut |e, _| Some(e)) {
                let conv = |c: Option<(usize, usize)>| c.map(|(a, b)| (byte_idx[a], byte_idx[b]));
                let caps = caps.into_iter().map(conv).collect();
                return Some((byte_idx[start], byte_idx[end], caps));
            }
        }
        None
    }

    pub fn is_match(&self, text: &str) -> bool {
        self.find_at(text, 0).is_some()
    }
}

fn bre_to_ere(p: &str) -> String {
    let mut out = String::new();
    let mut chars = p.chars().peekable();
    let mut in_bracket = false;
    while let Some(c) = chars.next() {
        if in_bracket {
            out.push(c);
            if c == ']' {
                in_bracket = false;
            }
            continue;
        }
        match c {
            '[' => {
                in_bracket = true;
                out.push(c);
                if chars.peek() == Some(&'^') {
                    out.push(chars.next().unwrap());
                }
                if chars.peek() == Some(&']') {
                    out.push(chars.next().unwrap());
                }
            }
            '\\' => match chars.next() {
                Some(n @ ('(' | ')' | '{' | '}' | '|' | '+' | '?')) => out.push(n),
                Some(n) => {
                    out.push('\\');
                    out.push(n);
                }
                None => out.push('\\'),
            },
            '(' | ')' | '{' | '}' | '|' | '+' | '?' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

struct Parser<'a> {
    c: &'a [char],
    i: usize,
    groups: usize,
}

impl Parser<'_> {
    fn alt(&mut self) -> Result<Node, String> {
        let mut alts = alloc::vec![self.concat()?];
        while self.i < self.c.len() && self.c[self.i] == '|' {
            self.i += 1;
            alts.push(self.concat()?);
        }
        Ok(if alts.len() == 1 {
            alts.pop().unwrap()
        } else {
            Node::Alt(alts)
        })
    }
    fn concat(&mut self) -> Result<Node, String> {
        let mut v = Vec::new();
        while self.i < self.c.len() && self.c[self.i] != '|' && self.c[self.i] != ')' {
            let atom = self.atom()?;
            let atom = self.quant(atom)?;
            v.push(atom);
        }
        Ok(Node::Concat(v))
    }
    fn quant(&mut self, mut n: Node) -> Result<Node, String> {
        loop {
            if self.i >= self.c.len() {
                return Ok(n);
            }
            let (min, max) = match self.c[self.i] {
                '*' => (0, u32::MAX),
                '+' => (1, u32::MAX),
                '?' => (0, 1),
                '{' => {
                    let start = self.i;
                    self.i += 1;
                    let mut a = String::new();
                    while self.i < self.c.len() && self.c[self.i].is_ascii_digit() {
                        a.push(self.c[self.i]);
                        self.i += 1;
                    }
                    let mut b = a.clone();
                    if self.i < self.c.len() && self.c[self.i] == ',' {
                        self.i += 1;
                        b.clear();
                        while self.i < self.c.len() && self.c[self.i].is_ascii_digit() {
                            b.push(self.c[self.i]);
                            self.i += 1;
                        }
                    }
                    if self.i >= self.c.len() || self.c[self.i] != '}' || a.is_empty() {
                        self.i = start;
                        return Ok(n);
                    }
                    let min: u32 = a.parse().unwrap_or(0);
                    let max: u32 = if b.is_empty() {
                        u32::MAX
                    } else {
                        b.parse().unwrap_or(min)
                    };
                    (min, max)
                }
                _ => return Ok(n),
            };
            self.i += 1;
            n = Node::Repeat(Box::new(n), min, max);
        }
    }
    fn atom(&mut self) -> Result<Node, String> {
        let c = self.c[self.i];
        self.i += 1;
        Ok(match c {
            '.' => Node::Any,
            '^' => Node::Start,
            '$' => Node::End,
            '(' => {
                self.groups += 1;
                let g = self.groups;
                let inner = self.alt()?;
                if self.i >= self.c.len() || self.c[self.i] != ')' {
                    return Err(String::from("unmatched ("));
                }
                self.i += 1;
                Node::Group(Box::new(inner), g)
            }
            '[' => self.bracket()?,
            '\\' => {
                let Some(&e) = self.c.get(self.i) else {
                    return Ok(Node::Char('\\'));
                };
                self.i += 1;
                match e {
                    'd' => Node::Class(alloc::vec![('0', '9')], false),
                    'D' => Node::Class(alloc::vec![('0', '9')], true),
                    'w' => Node::Class(
                        alloc::vec![('a', 'z'), ('A', 'Z'), ('0', '9'), ('_', '_')],
                        false,
                    ),
                    'W' => Node::Class(
                        alloc::vec![('a', 'z'), ('A', 'Z'), ('0', '9'), ('_', '_')],
                        true,
                    ),
                    's' => Node::Class(alloc::vec![(' ', ' '), ('\t', '\r')], false),
                    'S' => Node::Class(alloc::vec![(' ', ' '), ('\t', '\r')], true),
                    'b' => Node::WordBoundary,
                    'n' => Node::Char('\n'),
                    't' => Node::Char('\t'),
                    '1'..='9' => Node::BackRef(e as usize - '0' as usize),
                    o => Node::Char(o),
                }
            }
            c => Node::Char(c),
        })
    }
    fn bracket(&mut self) -> Result<Node, String> {
        let mut neg = false;
        let mut ranges = Vec::new();
        if self.c.get(self.i) == Some(&'^') {
            neg = true;
            self.i += 1;
        }
        let mut first = true;
        loop {
            let Some(&c) = self.c.get(self.i) else {
                return Err(String::from("unmatched ["));
            };
            if c == ']' && !first {
                self.i += 1;
                break;
            }
            first = false;
            if c == '[' && self.c.get(self.i + 1) == Some(&':') {
                let rest: String = self.c[self.i + 2..].iter().collect();
                if let Some(end) = rest.find(":]") {
                    let name = &rest[..end];
                    ranges.extend(match name {
                        "alpha" => alloc::vec![('a', 'z'), ('A', 'Z')],
                        "digit" => alloc::vec![('0', '9')],
                        "alnum" => alloc::vec![('a', 'z'), ('A', 'Z'), ('0', '9')],
                        "upper" => alloc::vec![('A', 'Z')],
                        "lower" => alloc::vec![('a', 'z')],
                        "space" => alloc::vec![(' ', ' '), ('\t', '\r')],
                        "blank" => alloc::vec![(' ', ' '), ('\t', '\t')],
                        "punct" => alloc::vec![('!', '/'), (':', '@'), ('[', '`'), ('{', '~')],
                        "xdigit" => alloc::vec![('0', '9'), ('a', 'f'), ('A', 'F')],
                        "print" => alloc::vec![(' ', '~')],
                        "cntrl" => alloc::vec![('\0', '\x1f')],
                        _ => Vec::new(),
                    });
                    self.i += 2 + end + 2;
                    continue;
                }
            }
            let lo = if c == '\\' && self.i + 1 < self.c.len() {
                self.i += 1;
                self.c[self.i]
            } else {
                c
            };
            self.i += 1;
            if self.c.get(self.i) == Some(&'-') && self.c.get(self.i + 1).is_some_and(|&n| n != ']')
            {
                let hi = self.c[self.i + 1];
                self.i += 2;
                ranges.push((lo, hi));
            } else {
                ranges.push((lo, lo));
            }
        }
        Ok(Node::Class(ranges, neg))
    }
}

struct Matcher<'a> {
    t: &'a [char],
    icase: bool,
}

type Caps = Vec<Option<(usize, usize)>>;

impl Matcher<'_> {
    fn eq(&self, a: char, b: char) -> bool {
        if self.icase {
            a.to_ascii_lowercase() == b.to_ascii_lowercase()
        } else {
            a == b
        }
    }

    fn is_word(c: char) -> bool {
        c.is_ascii_alphanumeric() || c == '_'
    }

    /// Match `n` at position `i`, then call `k` (the continuation).
    fn m(
        &self,
        n: &Node,
        i: usize,
        caps: &mut Caps,
        k: &mut dyn FnMut(usize, &mut Caps) -> Option<usize>,
    ) -> Option<usize> {
        match n {
            Node::Char(c) => {
                if i < self.t.len() && self.eq(self.t[i], *c) {
                    k(i + 1, caps)
                } else {
                    None
                }
            }
            Node::Any => {
                if i < self.t.len() && self.t[i] != '\n' {
                    k(i + 1, caps)
                } else {
                    None
                }
            }
            Node::Class(ranges, neg) => {
                if i >= self.t.len() {
                    return None;
                }
                let c = self.t[i];
                let mut hit = ranges.iter().any(|&(lo, hi)| lo <= c && c <= hi);
                if !hit && self.icase {
                    let (l, u) = (c.to_ascii_lowercase(), c.to_ascii_uppercase());
                    hit = ranges
                        .iter()
                        .any(|&(lo, hi)| (lo <= l && l <= hi) || (lo <= u && u <= hi));
                }
                if hit != *neg { k(i + 1, caps) } else { None }
            }
            Node::Start => {
                if i == 0 {
                    k(i, caps)
                } else {
                    None
                }
            }
            Node::End => {
                if i == self.t.len() {
                    k(i, caps)
                } else {
                    None
                }
            }
            Node::WordBoundary => {
                let before = i > 0 && Self::is_word(self.t[i - 1]);
                let after = i < self.t.len() && Self::is_word(self.t[i]);
                if before != after { k(i, caps) } else { None }
            }
            Node::Group(inner, g) => {
                let g = *g;
                let old = caps[g];
                let r = self.m(inner, i, caps, &mut |e, caps: &mut Caps| {
                    let saved = caps[g];
                    caps[g] = Some((i, e));
                    let r = k(e, caps);
                    if r.is_none() {
                        caps[g] = saved;
                    }
                    r
                });
                if r.is_none() {
                    caps[g] = old;
                }
                r
            }
            Node::Concat(v) => self.seq(v, 0, i, caps, k),
            Node::Alt(alts) => {
                for a in alts {
                    if let Some(r) = self.m(a, i, caps, k) {
                        return Some(r);
                    }
                }
                None
            }
            Node::Repeat(inner, min, max) => self.rep(inner, *min, *max, 0, i, caps, k),
            Node::BackRef(g) => {
                let (s, e) = caps.get(*g).copied().flatten()?;
                let len = e - s;
                if i + len <= self.t.len()
                    && (0..len).all(|j| self.eq(self.t[s + j], self.t[i + j]))
                {
                    k(i + len, caps)
                } else {
                    None
                }
            }
        }
    }

    fn seq(
        &self,
        v: &[Node],
        idx: usize,
        i: usize,
        caps: &mut Caps,
        k: &mut dyn FnMut(usize, &mut Caps) -> Option<usize>,
    ) -> Option<usize> {
        if idx == v.len() {
            return k(i, caps);
        }
        self.m(&v[idx], i, caps, &mut |e, caps: &mut Caps| {
            self.seq(v, idx + 1, e, caps, k)
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn rep(
        &self,
        n: &Node,
        min: u32,
        max: u32,
        count: u32,
        i: usize,
        caps: &mut Caps,
        k: &mut dyn FnMut(usize, &mut Caps) -> Option<usize>,
    ) -> Option<usize> {
        if count < max {
            // Greedy: try one more repetition first (guard against empty loops).
            let r = self.m(n, i, caps, &mut |e, caps: &mut Caps| {
                if e == i {
                    return None;
                }
                self.rep(n, min, max, count + 1, e, caps, k)
            });
            if r.is_some() {
                return r;
            }
        }
        if count >= min { k(i, caps) } else { None }
    }
}
