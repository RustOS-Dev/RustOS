//! Word expansion: tilde, parameters, command substitution, arithmetic,
//! field splitting and pathname (glob) expansion.

use crate::ast::*;
use crate::shell::Shell;
use rustos_rt::fs;
use rustos_rt::prelude::*;

/// A piece of an expanded word: text plus whether it came from a quoted
/// context (quoted text is neither split nor globbed).
#[derive(Clone)]
struct Piece {
    text: String,
    quoted: bool,
    /// Result of an unquoted expansion (subject to field splitting).
    splittable: bool,
}

/// Expand words into argument fields.
pub fn expand_words(sh: &mut Shell, words: &[Word]) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for w in words {
        out.extend(expand_word_fields(sh, w)?);
    }
    Ok(out)
}

/// Expand a word to a single string (no splitting or globbing), as for
/// assignments, redirection targets and `case` words.
pub fn expand_word_str(sh: &mut Shell, w: &Word) -> Result<String, String> {
    let pieces = expand_pieces(sh, w, false)?;
    Ok(pieces.into_iter().map(|p| p.text).collect())
}

/// Expand a word as a pattern (quoted parts are escaped).
pub fn expand_pattern(sh: &mut Shell, w: &Word) -> Result<String, String> {
    let pieces = expand_pieces(sh, w, false)?;
    let mut s = String::new();
    for p in pieces {
        if p.quoted {
            for c in p.text.chars() {
                if matches!(c, '*' | '?' | '[' | ']' | '\\') {
                    s.push('\\');
                }
                s.push(c);
            }
        } else {
            s.push_str(&p.text);
        }
    }
    Ok(s)
}

fn expand_word_fields(sh: &mut Shell, w: &Word) -> Result<Vec<String>, String> {
    // "$@" expands to one field per positional parameter.
    if w.len() == 1
        && let WordPart::Param(name, true) = &w[0]
        && name == "@"
    {
        return Ok(sh.positional.clone());
    }
    let pieces = expand_pieces(sh, w, true)?;
    // Field splitting.
    let ifs = sh.get_var("IFS").unwrap_or_else(|| String::from(" \t\n"));
    let mut fields: Vec<Vec<Piece>> = alloc::vec![Vec::new()];
    let mut any_quoted = false;
    for p in pieces {
        if p.quoted {
            any_quoted = true;
        }
        if p.splittable && !p.quoted {
            let mut cur = String::new();
            for c in p.text.chars() {
                if ifs.contains(c) {
                    if !cur.is_empty() {
                        fields.last_mut().unwrap().push(Piece {
                            text: core::mem::take(&mut cur),
                            quoted: false,
                            splittable: false,
                        });
                    }
                    if !fields.last().unwrap().is_empty() {
                        fields.push(Vec::new());
                    }
                } else {
                    cur.push(c);
                }
            }
            if !cur.is_empty() {
                fields.last_mut().unwrap().push(Piece {
                    text: cur,
                    quoted: false,
                    splittable: false,
                });
            }
        } else {
            fields.last_mut().unwrap().push(p);
        }
    }
    let mut out = Vec::new();
    for f in fields {
        if f.is_empty() {
            continue;
        }
        let has_glob = f
            .iter()
            .any(|p| !p.quoted && p.text.contains(['*', '?', '[']));
        let text: String = f.iter().map(|p| p.text.as_str()).collect();
        if has_glob {
            let mut pat = String::new();
            for p in &f {
                if p.quoted {
                    for c in p.text.chars() {
                        if matches!(c, '*' | '?' | '[' | ']' | '\\') {
                            pat.push('\\');
                        }
                        pat.push(c);
                    }
                } else {
                    pat.push_str(&p.text);
                }
            }
            let mut matches = glob(&pat);
            if matches.is_empty() {
                out.push(text);
            } else {
                matches.sort();
                out.extend(matches);
            }
        } else {
            out.push(text);
        }
    }
    if out.is_empty() && any_quoted {
        out.push(String::new());
    }
    Ok(out)
}

fn expand_pieces(sh: &mut Shell, w: &Word, split_ctx: bool) -> Result<Vec<Piece>, String> {
    let mut out = Vec::new();
    for part in w {
        match part {
            WordPart::Lit(s) => out.push(Piece {
                text: s.clone(),
                quoted: false,
                splittable: false,
            }),
            WordPart::Quoted(s) => out.push(Piece {
                text: s.clone(),
                quoted: true,
                splittable: false,
            }),
            WordPart::Tilde(user) => {
                let home = if user.is_empty() {
                    sh.get_var("HOME").unwrap_or_else(|| String::from("/"))
                } else if user == "root" {
                    String::from("/root")
                } else {
                    format!("/home/{}", user)
                };
                out.push(Piece {
                    text: home,
                    quoted: true,
                    splittable: false,
                });
            }
            WordPart::Param(expr, quoted) => {
                let v = param(sh, expr)?;
                out.push(Piece {
                    text: v,
                    quoted: *quoted,
                    splittable: split_ctx && !*quoted,
                });
            }
            WordPart::CmdSub(cmd, quoted) => {
                let v = sh.capture(cmd);
                out.push(Piece {
                    text: v.trim_end_matches('\n').to_string(),
                    quoted: *quoted,
                    splittable: split_ctx && !*quoted,
                });
            }
            WordPart::Arith(expr) => {
                // Arithmetic text may itself contain $vars.
                let expanded = expand_inline(sh, expr)?;
                let v = arith(sh, &expanded)?;
                out.push(Piece {
                    text: format!("{}", v),
                    quoted: true,
                    splittable: false,
                });
            }
        }
    }
    Ok(out)
}

/// Expand `$var`/`$(...)` references inside a string (for arithmetic and
/// here-documents).
pub fn expand_inline(sh: &mut Shell, s: &str) -> Result<String, String> {
    let b = s.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 1 < b.len() && matches!(b[i + 1], b'$' | b'`' | b'\\') {
            out.push(b[i + 1] as char);
            i += 2;
            continue;
        }
        if b[i] == b'$' {
            // Reuse the parser's word lexer on the remainder.
            let rest = &s[i..];
            let end = dollar_extent(rest);
            let word_src = format!("\"{}\"", &rest[..end].replace('"', "\\\""));
            if let Ok(list) = crate::parser::parse(&format!(": {}", word_src))
                && let Some(Command::Simple { words, .. }) =
                    list.first().and_then(|a| a.first.cmds.first())
                && words.len() == 2
            {
                out.push_str(&expand_word_str(sh, &words[1])?);
                i += end;
                continue;
            }
        }
        let ch_len = s[i..].chars().next().map_or(1, |c| c.len_utf8());
        out.push_str(&s[i..i + ch_len]);
        i += ch_len;
    }
    Ok(out)
}

fn dollar_extent(s: &str) -> usize {
    let b = s.as_bytes();
    if b.len() < 2 {
        return b.len();
    }
    match b[1] {
        b'{' | b'(' => {
            let (open, close) = if b[1] == b'{' {
                (b'{', b'}')
            } else {
                (b'(', b')')
            };
            let mut depth = 0;
            for (j, &c) in b.iter().enumerate().skip(1) {
                if c == open {
                    depth += 1;
                } else if c == close {
                    depth -= 1;
                    if depth == 0 {
                        return j + 1;
                    }
                }
            }
            b.len()
        }
        c if c == b'_' || c.is_ascii_alphabetic() => {
            let mut j = 1;
            while j < b.len() && (b[j] == b'_' || b[j].is_ascii_alphanumeric()) {
                j += 1;
            }
            j
        }
        _ => 2,
    }
}

fn param(sh: &mut Shell, expr: &str) -> Result<String, String> {
    // ${#name}
    if let Some(name) = expr.strip_prefix('#')
        && !name.is_empty()
    {
        return Ok(format!(
            "{}",
            simple_param(sh, name).unwrap_or_default().chars().count()
        ));
    }
    let name_end = expr
        .find(|c: char| !(c == '_' || c.is_ascii_alphanumeric()))
        .unwrap_or(expr.len());
    let (name, rest) = if name_end == 0 {
        expr.split_at(expr.len().min(1))
    } else {
        expr.split_at(name_end)
    };
    let value = simple_param(sh, name);
    if rest.is_empty() {
        if value.is_none() && sh.opt_nounset {
            return Err(format!("{}: unbound variable", name));
        }
        return Ok(value.unwrap_or_default());
    }
    let (op, arg) = if rest.starts_with(":-")
        || rest.starts_with(":=")
        || rest.starts_with(":+")
        || rest.starts_with(":?")
        || rest.starts_with("##")
        || rest.starts_with("%%")
        || rest.starts_with("//")
    {
        rest.split_at(2)
    } else {
        rest.split_at(1)
    };
    let arg_expanded = || -> String { arg.to_string() };
    let empty_or_unset = value.as_deref().is_none_or(|v| v.is_empty());
    match op {
        ":-" | "-" => {
            let use_default = if op == ":-" {
                empty_or_unset
            } else {
                value.is_none()
            };
            if use_default {
                let a = arg_expanded();
                return expand_inline(sh, &a);
            }
            Ok(value.unwrap_or_default())
        }
        ":=" | "=" => {
            let use_default = if op == ":=" {
                empty_or_unset
            } else {
                value.is_none()
            };
            if use_default {
                let a = arg_expanded();
                let v = expand_inline(sh, &a)?;
                sh.set_var(name, &v);
                return Ok(v);
            }
            Ok(value.unwrap_or_default())
        }
        ":+" | "+" => {
            let set = if op == ":+" {
                !empty_or_unset
            } else {
                value.is_some()
            };
            if set {
                let a = arg_expanded();
                return expand_inline(sh, &a);
            }
            Ok(String::new())
        }
        ":?" | "?" => {
            if empty_or_unset {
                let msg = if arg.is_empty() {
                    "parameter null or not set"
                } else {
                    arg
                };
                return Err(format!("{}: {}", name, msg));
            }
            Ok(value.unwrap_or_default())
        }
        "#" | "##" | "%" | "%%" => {
            let v = value.unwrap_or_default();
            let pat = expand_inline(sh, arg)?;
            let longest = op.len() == 2;
            Ok(if op.starts_with('#') {
                strip_prefix(&v, &pat, longest)
            } else {
                strip_suffix(&v, &pat, longest)
            })
        }
        "/" | "//" => {
            let v = value.unwrap_or_default();
            let (pat, rep) = arg.split_once('/').unwrap_or((arg, ""));
            let pat = expand_inline(sh, pat)?;
            let rep = expand_inline(sh, rep)?;
            Ok(if op == "//" {
                v.replace(&pat, &rep)
            } else {
                v.replacen(&pat, &rep, 1)
            })
        }
        ":" => {
            // ${var:offset:len}
            let v: Vec<char> = value.unwrap_or_default().chars().collect();
            let (off, len) = match arg.split_once(':') {
                Some((o, l)) => (o, Some(l)),
                None => (arg, None),
            };
            let off: i64 = arith(sh, off).unwrap_or(0);
            let start = if off < 0 {
                (v.len() as i64 + off).max(0) as usize
            } else {
                (off as usize).min(v.len())
            };
            let end = match len {
                Some(l) => (start + arith(sh, l).unwrap_or(0).max(0) as usize).min(v.len()),
                None => v.len(),
            };
            Ok(v[start..end].iter().collect())
        }
        _ => Err(format!("{}: bad substitution", expr)),
    }
}

fn strip_prefix(v: &str, pat: &str, longest: bool) -> String {
    let idxs: Vec<usize> = v
        .char_indices()
        .map(|(i, _)| i)
        .chain(core::iter::once(v.len()))
        .collect();
    let order: Vec<usize> = if longest {
        idxs.iter().rev().copied().collect()
    } else {
        idxs
    };
    for i in order {
        if fnmatch(pat, &v[..i]) {
            return v[i..].to_string();
        }
    }
    v.to_string()
}

fn strip_suffix(v: &str, pat: &str, longest: bool) -> String {
    let idxs: Vec<usize> = v
        .char_indices()
        .map(|(i, _)| i)
        .chain(core::iter::once(v.len()))
        .collect();
    let order: Vec<usize> = if longest {
        idxs
    } else {
        idxs.iter().rev().copied().collect()
    };
    for i in order {
        if fnmatch(pat, &v[i..]) {
            return v[..i].to_string();
        }
    }
    v.to_string()
}

fn simple_param(sh: &Shell, name: &str) -> Option<String> {
    match name {
        "?" => Some(format!("{}", sh.last_status)),
        "$" => Some(format!("{}", sh.shell_pid)),
        "!" => sh.last_bg.map(|p| format!("{}", p)),
        "#" => Some(format!("{}", sh.positional.len())),
        "@" | "*" => Some(sh.positional.join(" ")),
        "-" => Some(sh.option_flags()),
        "0" => Some(sh.arg0.clone()),
        n if n.chars().all(|c| c.is_ascii_digit()) => {
            let i: usize = n.parse().ok()?;
            sh.positional.get(i - 1).cloned()
        }
        n => sh.get_var(n),
    }
}

// ---------------------------------------------------------------------------
// Globbing
// ---------------------------------------------------------------------------

/// Match `name` against a shell pattern (`*`, `?`, `[...]`, `\` escapes).
pub fn fnmatch(pat: &str, name: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    let n: Vec<char> = name.chars().collect();
    fn m(p: &[char], n: &[char]) -> bool {
        let (mut pi, mut ni) = (0, 0);
        let (mut star_p, mut star_n) = (usize::MAX, 0);
        while ni < n.len() {
            if pi < p.len() {
                match p[pi] {
                    '*' => {
                        star_p = pi;
                        star_n = ni;
                        pi += 1;
                        continue;
                    }
                    '?' => {
                        pi += 1;
                        ni += 1;
                        continue;
                    }
                    '[' => {
                        if let Some((matched, len)) = class(&p[pi..], n[ni])
                            && matched
                        {
                            pi += len;
                            ni += 1;
                            continue;
                        } else if class(&p[pi..], n[ni]).is_none() && n[ni] == '[' {
                            pi += 1;
                            ni += 1;
                            continue;
                        }
                    }
                    '\\' if pi + 1 < p.len() => {
                        if p[pi + 1] == n[ni] {
                            pi += 2;
                            ni += 1;
                            continue;
                        }
                    }
                    c => {
                        if c == n[ni] {
                            pi += 1;
                            ni += 1;
                            continue;
                        }
                    }
                }
            }
            if star_p != usize::MAX {
                pi = star_p + 1;
                star_n += 1;
                ni = star_n;
                continue;
            }
            return false;
        }
        while pi < p.len() && p[pi] == '*' {
            pi += 1;
        }
        pi == p.len()
    }
    fn class(p: &[char], c: char) -> Option<(bool, usize)> {
        let mut i = 1;
        let negate = i < p.len() && (p[i] == '!' || p[i] == '^');
        if negate {
            i += 1;
        }
        let mut matched = false;
        let mut first = true;
        while i < p.len() {
            if p[i] == ']' && !first {
                return Some((matched != negate, i + 1));
            }
            first = false;
            if i + 2 < p.len() && p[i + 1] == '-' && p[i + 2] != ']' {
                if p[i] <= c && c <= p[i + 2] {
                    matched = true;
                }
                i += 3;
            } else {
                if p[i] == c {
                    matched = true;
                }
                i += 1;
            }
        }
        None
    }
    m(&p, &n)
}

fn has_meta(s: &str) -> bool {
    let mut esc = false;
    for c in s.chars() {
        if esc {
            esc = false;
            continue;
        }
        match c {
            '\\' => esc = true,
            '*' | '?' | '[' => return true,
            _ => {}
        }
    }
    false
}

fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut esc = false;
    for c in s.chars() {
        if esc {
            out.push(c);
            esc = false;
        } else if c == '\\' {
            esc = true;
        } else {
            out.push(c);
        }
    }
    out
}

/// Expand a glob pattern into matching paths.
pub fn glob(pattern: &str) -> Vec<String> {
    let absolute = pattern.starts_with('/');
    let comps: Vec<&str> = pattern.split('/').filter(|c| !c.is_empty()).collect();
    let mut paths: Vec<String> = alloc::vec![if absolute {
        String::from("/")
    } else {
        String::new()
    }];
    for (i, comp) in comps.iter().enumerate() {
        let last = i + 1 == comps.len();
        let mut next = Vec::new();
        for base in &paths {
            if !has_meta(comp) {
                let p = join_rel(base, &unescape(comp));
                if last || fs::is_dir(&p) {
                    if !last || fs::symlink_metadata(&p).is_ok() {
                        next.push(p);
                    }
                }
                continue;
            }
            let dir = if base.is_empty() { "." } else { base.as_str() };
            let Ok(entries) = fs::read_dir(dir) else {
                continue;
            };
            for e in entries {
                if e.name.starts_with('.') && !comp.starts_with('.') {
                    continue;
                }
                if fnmatch(comp, &e.name) {
                    let p = join_rel(base, &e.name);
                    if last || fs::is_dir(&p) {
                        next.push(p);
                    }
                }
            }
        }
        paths = next;
        if paths.is_empty() {
            break;
        }
    }
    if pattern.ends_with('/') {
        paths.iter_mut().for_each(|p| p.push('/'));
    }
    paths
}

fn join_rel(base: &str, name: &str) -> String {
    if base.is_empty() {
        name.to_string()
    } else if base.ends_with('/') {
        format!("{}{}", base, name)
    } else {
        format!("{}/{}", base, name)
    }
}

// ---------------------------------------------------------------------------
// Arithmetic
// ---------------------------------------------------------------------------

pub fn arith(sh: &mut Shell, s: &str) -> Result<i64, String> {
    let toks = arith_tokens(s)?;
    let mut p = Arith { t: &toks, i: 0, sh };
    let v = p.assign()?;
    if p.i != toks.len() {
        return Err(format!("{}: syntax error in expression", s.trim()));
    }
    Ok(v)
}

#[derive(Clone, Debug)]
enum AT {
    Num(i64),
    Var(String),
    Op(String),
}

fn arith_tokens(s: &str) -> Result<Vec<AT>, String> {
    let b = s.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() {
            let st = i;
            while i < b.len() && b[i].is_ascii_alphanumeric() {
                i += 1;
            }
            let t = &s[st..i];
            let v = if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
                i64::from_str_radix(h, 16)
            } else if t.len() > 1 && t.starts_with('0') {
                i64::from_str_radix(&t[1..], 8)
            } else {
                t.parse()
            };
            out.push(AT::Num(v.map_err(|_| format!("{}: invalid number", t))?));
        } else if c == b'_' || c.is_ascii_alphabetic() {
            let st = i;
            while i < b.len() && (b[i] == b'_' || b[i].is_ascii_alphanumeric()) {
                i += 1;
            }
            out.push(AT::Var(s[st..i].to_string()));
        } else {
            let three = ["**=", "<<=", ">>="];
            let two = [
                "**", "<=", ">=", "==", "!=", "&&", "||", "<<", ">>", "+=", "-=", "*=", "/=", "%=",
                "++", "--",
            ];
            let mut matched = false;
            for op in three.iter().chain(two.iter()) {
                if s[i..].starts_with(op) {
                    out.push(AT::Op(op.to_string()));
                    i += op.len();
                    matched = true;
                    break;
                }
            }
            if !matched {
                if "+-*/%()<>!~&|^?:=".contains(c as char) {
                    out.push(AT::Op((c as char).to_string()));
                    i += 1;
                } else {
                    return Err(format!("{}: syntax error in expression", s.trim()));
                }
            }
        }
    }
    Ok(out)
}

struct Arith<'a> {
    t: &'a [AT],
    i: usize,
    sh: &'a mut Shell,
}

impl Arith<'_> {
    fn peek_op(&self) -> Option<&str> {
        match self.t.get(self.i) {
            Some(AT::Op(o)) => Some(o.as_str()),
            _ => None,
        }
    }
    fn eat(&mut self, op: &str) -> bool {
        if self.peek_op() == Some(op) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn var_value(&self, n: &str) -> i64 {
        self.sh
            .get_var(n)
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0)
    }
    fn assign(&mut self) -> Result<i64, String> {
        if let (Some(AT::Var(name)), Some(AT::Op(op))) =
            (self.t.get(self.i), self.t.get(self.i + 1))
        {
            let name = name.clone();
            let op = op.clone();
            if matches!(op.as_str(), "=" | "+=" | "-=" | "*=" | "/=" | "%=") {
                self.i += 2;
                let rhs = self.assign()?;
                let cur = self.var_value(&name);
                let v = match op.as_str() {
                    "=" => rhs,
                    "+=" => cur.wrapping_add(rhs),
                    "-=" => cur.wrapping_sub(rhs),
                    "*=" => cur.wrapping_mul(rhs),
                    "/=" => {
                        if rhs == 0 {
                            return Err(String::from("division by 0"));
                        }
                        cur / rhs
                    }
                    _ => {
                        if rhs == 0 {
                            return Err(String::from("division by 0"));
                        }
                        cur % rhs
                    }
                };
                self.sh.set_var(&name, &format!("{}", v));
                return Ok(v);
            }
        }
        self.ternary()
    }
    fn ternary(&mut self) -> Result<i64, String> {
        let c = self.bin(0)?;
        if self.eat("?") {
            let a = self.assign()?;
            if !self.eat(":") {
                return Err(String::from("expected `:'"));
            }
            let b = self.assign()?;
            return Ok(if c != 0 { a } else { b });
        }
        Ok(c)
    }
    fn bin(&mut self, level: usize) -> Result<i64, String> {
        const LEVELS: [&[&str]; 10] = [
            &["||"],
            &["&&"],
            &["|"],
            &["^"],
            &["&"],
            &["==", "!="],
            &["<", "<=", ">", ">="],
            &["<<", ">>"],
            &["+", "-"],
            &["*", "/", "%"],
        ];
        if level == LEVELS.len() {
            return self.power();
        }
        let mut l = self.bin(level + 1)?;
        loop {
            let Some(op) = self.peek_op().map(String::from) else {
                break;
            };
            if !LEVELS[level].contains(&op.as_str()) {
                break;
            }
            self.i += 1;
            let r = self.bin(level + 1)?;
            l = match op.as_str() {
                "||" => ((l != 0) || (r != 0)) as i64,
                "&&" => ((l != 0) && (r != 0)) as i64,
                "|" => l | r,
                "^" => l ^ r,
                "&" => l & r,
                "==" => (l == r) as i64,
                "!=" => (l != r) as i64,
                "<" => (l < r) as i64,
                "<=" => (l <= r) as i64,
                ">" => (l > r) as i64,
                ">=" => (l >= r) as i64,
                "<<" => l.wrapping_shl(r as u32),
                ">>" => l.wrapping_shr(r as u32),
                "+" => l.wrapping_add(r),
                "-" => l.wrapping_sub(r),
                "*" => l.wrapping_mul(r),
                "/" | "%" => {
                    if r == 0 {
                        return Err(String::from("division by 0"));
                    }
                    if op == "/" {
                        l.wrapping_div(r)
                    } else {
                        l.wrapping_rem(r)
                    }
                }
                _ => unreachable!(),
            };
        }
        Ok(l)
    }
    fn power(&mut self) -> Result<i64, String> {
        let b = self.unary()?;
        if self.eat("**") {
            let e = self.power()?;
            return Ok(b.wrapping_pow(e.max(0) as u32));
        }
        Ok(b)
    }
    fn unary(&mut self) -> Result<i64, String> {
        if self.eat("-") {
            return Ok(self.unary()?.wrapping_neg());
        }
        if self.eat("+") {
            return self.unary();
        }
        if self.eat("!") {
            return Ok((self.unary()? == 0) as i64);
        }
        if self.eat("~") {
            return Ok(!self.unary()?);
        }
        self.primary()
    }
    fn primary(&mut self) -> Result<i64, String> {
        match self.t.get(self.i).cloned() {
            Some(AT::Num(n)) => {
                self.i += 1;
                Ok(n)
            }
            Some(AT::Var(v)) => {
                self.i += 1;
                if self.eat("++") {
                    let cur = self.var_value(&v);
                    self.sh.set_var(&v, &format!("{}", cur + 1));
                    return Ok(cur);
                }
                if self.eat("--") {
                    let cur = self.var_value(&v);
                    self.sh.set_var(&v, &format!("{}", cur - 1));
                    return Ok(cur);
                }
                Ok(self.var_value(&v))
            }
            Some(AT::Op(o)) if o == "(" => {
                self.i += 1;
                let v = self.assign()?;
                if !self.eat(")") {
                    return Err(String::from("expected `)'"));
                }
                Ok(v)
            }
            _ => Err(String::from("syntax error in expression")),
        }
    }
}
