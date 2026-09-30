//! Selectors Level 4: parsing, specificity and matching.

use crate::parser::Cv;
use crate::tokenizer::Token;
use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Combinator {
    /// Whitespace.
    Descendant,
    /// `>`
    Child,
    /// `+`
    NextSibling,
    /// `~`
    SubsequentSibling,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttrOp {
    Exists,
    /// `=`
    Equals,
    /// `~=`
    Includes,
    /// `|=`
    DashMatch,
    /// `^=`
    Prefix,
    /// `$=`
    Suffix,
    /// `*=`
    Substring,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Simple {
    Universal,
    Type(String),
    Id(String),
    Class(String),
    Attr {
        name: String,
        op: AttrOp,
        value: String,
        ci: bool,
    },
    Pseudo(PseudoClass),
    /// `&` (CSS Nesting); replaced by the parent selector when rules are
    /// flattened.
    Nesting,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PseudoClass {
    Root,
    Empty,
    FirstChild,
    LastChild,
    OnlyChild,
    FirstOfType,
    LastOfType,
    OnlyOfType,
    /// `an+b`, optional `of S`.
    NthChild(i32, i32, Option<Box<SelectorList>>),
    NthLastChild(i32, i32, Option<Box<SelectorList>>),
    NthOfType(i32, i32),
    NthLastOfType(i32, i32),
    Not(Box<SelectorList>),
    Is(Box<SelectorList>),
    Where(Box<SelectorList>),
    Has(Box<Vec<Selector>>),
    State(State),
    Lang(String),
    /// Unsupported pseudo-classes never match.
    Never,
}

/// Dynamic element states the DOM reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Link,
    Visited,
    AnyLink,
    Hover,
    Active,
    Focus,
    FocusWithin,
    FocusVisible,
    Target,
    Checked,
    Indeterminate,
    Disabled,
    Enabled,
    Required,
    Optional,
    ReadOnly,
    ReadWrite,
    PlaceholderShown,
    Default,
    Defined,
    Valid,
    Invalid,
    Open,
    Scope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PseudoElement {
    Before,
    After,
    Marker,
    FirstLine,
    FirstLetter,
    Placeholder,
    Selection,
    Backdrop,
    FileSelectorButton,
}

/// A compound selector: simple selectors with no combinator between them.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Compound(pub Vec<Simple>);

/// A complex selector. `parts[0]` is the leftmost compound; each later
/// compound is preceded by its combinator. For relative selectors (`:has`)
/// the first combinator relates to the anchor element.
#[derive(Debug, Clone, PartialEq)]
pub struct Selector {
    pub parts: Vec<(Combinator, Compound)>,
    pub pseudo_element: Option<PseudoElement>,
    /// Starts with a combinator (relative selector, `:has()` argument or a
    /// nested rule like `> a`).
    pub relative: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SelectorList(pub Vec<Selector>);

/// (id, class/attribute/pseudo-class, type/pseudo-element) counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Specificity(pub u32, pub u32, pub u32);

impl core::ops::Add for Specificity {
    type Output = Specificity;
    fn add(self, o: Specificity) -> Specificity {
        Specificity(self.0 + o.0, self.1 + o.1, self.2 + o.2)
    }
}

impl SelectorList {
    fn max_specificity(&self) -> Specificity {
        self.0
            .iter()
            .map(Selector::specificity)
            .max()
            .unwrap_or_default()
    }

    pub fn parse(cvs: &[Cv]) -> Option<SelectorList> {
        parse_list(cvs, false)
    }

    pub fn parse_str(s: &str) -> Option<SelectorList> {
        parse_list(&crate::parser::component_values(s), false)
    }

    pub fn contains_nesting(&self) -> bool {
        self.0.iter().any(|s| s.contains_nesting())
    }
}

impl Selector {
    pub fn specificity(&self) -> Specificity {
        let mut s = Specificity::default();
        for (_, c) in &self.parts {
            for simple in &c.0 {
                s = s + simple_specificity(simple);
            }
        }
        if self.pseudo_element.is_some() {
            s.2 += 1;
        }
        s
    }

    pub fn contains_nesting(&self) -> bool {
        self.parts.iter().any(|(_, c)| {
            c.0.iter().any(|s| match s {
                Simple::Nesting => true,
                Simple::Pseudo(
                    PseudoClass::Is(l) | PseudoClass::Not(l) | PseudoClass::Where(l),
                ) => l.contains_nesting(),
                _ => false,
            })
        })
    }

    /// Replace `&` with `:is(parent)`; a selector without `&` becomes a
    /// descendant (or, if relative, a relation) of the parent.
    pub fn resolve_nesting(&self, parent: &SelectorList) -> Selector {
        let is_parent = Simple::Pseudo(PseudoClass::Is(Box::new(parent.clone())));
        if self.contains_nesting() {
            let mut out = self.clone();
            for (_, c) in out.parts.iter_mut() {
                for s in c.0.iter_mut() {
                    replace_nesting(s, parent, &is_parent);
                }
            }
            out.relative = false;
            return out;
        }
        let mut parts = Vec::with_capacity(self.parts.len() + 1);
        parts.push((Combinator::Descendant, Compound(alloc::vec![is_parent])));
        parts.extend(self.parts.iter().cloned());
        Selector {
            parts,
            pseudo_element: self.pseudo_element,
            relative: false,
        }
    }
}

fn replace_nesting(s: &mut Simple, parent: &SelectorList, is_parent: &Simple) {
    match s {
        Simple::Nesting => *s = is_parent.clone(),
        Simple::Pseudo(PseudoClass::Is(l) | PseudoClass::Not(l) | PseudoClass::Where(l)) => {
            for sel in l.0.iter_mut() {
                for (_, c) in sel.parts.iter_mut() {
                    for x in c.0.iter_mut() {
                        replace_nesting(x, parent, is_parent);
                    }
                }
            }
        }
        _ => {}
    }
}

fn simple_specificity(s: &Simple) -> Specificity {
    match s {
        Simple::Universal | Simple::Nesting => Specificity::default(),
        Simple::Type(_) => Specificity(0, 0, 1),
        Simple::Id(_) => Specificity(1, 0, 0),
        Simple::Class(_) | Simple::Attr { .. } => Specificity(0, 1, 0),
        Simple::Pseudo(p) => match p {
            PseudoClass::Where(_) => Specificity::default(),
            PseudoClass::Is(l) | PseudoClass::Not(l) => l.max_specificity(),
            PseudoClass::Has(l) => l
                .iter()
                .map(Selector::specificity)
                .max()
                .unwrap_or_default(),
            PseudoClass::NthChild(_, _, Some(l)) | PseudoClass::NthLastChild(_, _, Some(l)) => {
                Specificity(0, 1, 0) + l.max_specificity()
            }
            _ => Specificity(0, 1, 0),
        },
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

fn split_commas(cvs: &[Cv]) -> Vec<&[Cv]> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, c) in cvs.iter().enumerate() {
        if matches!(c, Cv::Token(Token::Comma)) {
            out.push(&cvs[start..i]);
            start = i + 1;
        }
    }
    out.push(&cvs[start..]);
    out
}

/// Parse a selector list; `forgiving` drops invalid entries (as `:is()`
/// and `:where()` do) instead of failing the whole list.
fn parse_list(cvs: &[Cv], forgiving: bool) -> Option<SelectorList> {
    let mut out = Vec::new();
    for part in split_commas(cvs) {
        match parse_complex(part) {
            Some(s) => out.push(s),
            None if forgiving => {}
            None => return None,
        }
    }
    Some(SelectorList(out))
}

fn parse_complex(cvs: &[Cv]) -> Option<Selector> {
    let mut i = 0;
    let skip_ws = |i: &mut usize| {
        while *i < cvs.len() && cvs[*i].is_ws() {
            *i += 1;
        }
    };
    skip_ws(&mut i);
    let mut parts: Vec<(Combinator, Compound)> = Vec::new();
    let mut pseudo_element = None;
    let mut relative = false;
    let mut pending: Option<Combinator> = None;
    loop {
        skip_ws(&mut i);
        if i >= cvs.len() {
            break;
        }
        let comb = if cvs[i].is_delim('>') {
            Some(Combinator::Child)
        } else if cvs[i].is_delim('+') {
            Some(Combinator::NextSibling)
        } else if cvs[i].is_delim('~') {
            Some(Combinator::SubsequentSibling)
        } else {
            None
        };
        if let Some(c) = comb {
            if pending.is_some() && !(parts.is_empty()) {
                return None; // two combinators in a row
            }
            if parts.is_empty() {
                relative = true;
            }
            pending = Some(c);
            i += 1;
            continue;
        }
        if pseudo_element.is_some() {
            return None; // nothing may follow a pseudo-element (but its pseudo-classes, unsupported)
        }
        let before = i;
        let (compound, pe) = parse_compound(cvs, &mut i)?;
        if i == before {
            return None;
        }
        pseudo_element = pe;
        let c = pending.take().unwrap_or(Combinator::Descendant);
        parts.push((c, compound));
        // Whitespace followed by another compound is a descendant combinator.
    }
    if parts.is_empty() || pending.is_some() {
        return None;
    }
    Some(Selector {
        parts,
        pseudo_element,
        relative,
    })
}

fn parse_compound(cvs: &[Cv], i: &mut usize) -> Option<(Compound, Option<PseudoElement>)> {
    let mut out = Vec::new();
    let mut pe = None;
    while *i < cvs.len() {
        match &cvs[*i] {
            Cv::Token(Token::Whitespace) => break,
            c if c.is_delim('>') || c.is_delim('+') || c.is_delim('~') => break,
            Cv::Token(Token::Comma) => break,
            Cv::Token(Token::Ident(name)) => {
                out.push(Simple::Type(name.to_ascii_lowercase()));
                *i += 1;
            }
            c if c.is_delim('*') => {
                out.push(Simple::Universal);
                *i += 1;
            }
            c if c.is_delim('&') => {
                out.push(Simple::Nesting);
                *i += 1;
            }
            c if c.is_delim('|') => {
                // Namespace prefixes: accept `*|x` / `|x` as `x`.
                *i += 1;
                if matches!(out.last(), Some(Simple::Universal)) {
                    out.pop();
                }
            }
            Cv::Token(Token::Hash { value, .. }) => {
                out.push(Simple::Id(value.clone()));
                *i += 1;
            }
            c if c.is_delim('.') => {
                *i += 1;
                let name = cvs.get(*i)?.ident()?;
                out.push(Simple::Class(name.to_string()));
                *i += 1;
            }
            Cv::Block { open: '[', items } => {
                out.push(parse_attr(items)?);
                *i += 1;
            }
            Cv::Token(Token::Colon) => {
                *i += 1;
                if matches!(cvs.get(*i), Some(Cv::Token(Token::Colon))) {
                    *i += 1;
                    let name = match cvs.get(*i)? {
                        Cv::Token(Token::Ident(n)) => n.to_ascii_lowercase(),
                        Cv::Function { name, .. } => name.to_ascii_lowercase(),
                        _ => return None,
                    };
                    *i += 1;
                    pe = Some(match name.as_str() {
                        "before" => PseudoElement::Before,
                        "after" => PseudoElement::After,
                        "marker" => PseudoElement::Marker,
                        "first-line" => PseudoElement::FirstLine,
                        "first-letter" => PseudoElement::FirstLetter,
                        "placeholder" => PseudoElement::Placeholder,
                        "selection" => PseudoElement::Selection,
                        "backdrop" => PseudoElement::Backdrop,
                        "file-selector-button" => PseudoElement::FileSelectorButton,
                        _ => return None, // unknown pseudo-element: invalid selector
                    });
                    continue;
                }
                match cvs.get(*i)? {
                    Cv::Token(Token::Ident(name)) => {
                        let n = name.to_ascii_lowercase();
                        *i += 1;
                        // Legacy single-colon pseudo-elements.
                        match n.as_str() {
                            "before" => pe = Some(PseudoElement::Before),
                            "after" => pe = Some(PseudoElement::After),
                            "first-line" => pe = Some(PseudoElement::FirstLine),
                            "first-letter" => pe = Some(PseudoElement::FirstLetter),
                            _ => out.push(Simple::Pseudo(pseudo_class(&n))),
                        }
                    }
                    Cv::Function { name, args } => {
                        let n = name.to_ascii_lowercase();
                        *i += 1;
                        out.push(Simple::Pseudo(pseudo_function(&n, args)?));
                    }
                    _ => return None,
                }
            }
            _ => return None,
        }
    }
    Some((Compound(out), pe))
}

fn pseudo_class(n: &str) -> PseudoClass {
    use PseudoClass::*;
    match n {
        "root" => Root,
        "empty" => Empty,
        "first-child" => FirstChild,
        "last-child" => LastChild,
        "only-child" => OnlyChild,
        "first-of-type" => FirstOfType,
        "last-of-type" => LastOfType,
        "only-of-type" => OnlyOfType,
        "link" => PseudoClass::State(self::State::Link),
        "visited" => PseudoClass::State(self::State::Visited),
        "any-link" | "-webkit-any-link" => PseudoClass::State(self::State::AnyLink),
        "hover" => PseudoClass::State(self::State::Hover),
        "active" => PseudoClass::State(self::State::Active),
        "focus" => PseudoClass::State(self::State::Focus),
        "focus-within" => PseudoClass::State(self::State::FocusWithin),
        "focus-visible" => PseudoClass::State(self::State::FocusVisible),
        "target" => PseudoClass::State(self::State::Target),
        "checked" => PseudoClass::State(self::State::Checked),
        "indeterminate" => PseudoClass::State(self::State::Indeterminate),
        "disabled" => PseudoClass::State(self::State::Disabled),
        "enabled" => PseudoClass::State(self::State::Enabled),
        "required" => PseudoClass::State(self::State::Required),
        "optional" => PseudoClass::State(self::State::Optional),
        "read-only" => PseudoClass::State(self::State::ReadOnly),
        "read-write" => PseudoClass::State(self::State::ReadWrite),
        "placeholder-shown" => PseudoClass::State(self::State::PlaceholderShown),
        "default" => PseudoClass::State(self::State::Default),
        "defined" => PseudoClass::State(self::State::Defined),
        "valid" => PseudoClass::State(self::State::Valid),
        "invalid" => PseudoClass::State(self::State::Invalid),
        "open" => PseudoClass::State(self::State::Open),
        "scope" => PseudoClass::State(self::State::Scope),
        _ => Never,
    }
}

fn trim_ws(cvs: &[Cv]) -> &[Cv] {
    let mut a = 0;
    let mut b = cvs.len();
    while a < b && cvs[a].is_ws() {
        a += 1;
    }
    while b > a && cvs[b - 1].is_ws() {
        b -= 1;
    }
    &cvs[a..b]
}

fn pseudo_function(n: &str, args: &[Cv]) -> Option<PseudoClass> {
    use PseudoClass::*;
    Some(match n {
        "not" => Not(Box::new(parse_list(args, false)?)),
        "is" | "matches" | "-webkit-any" | "-moz-any" => Is(Box::new(parse_list(args, true)?)),
        "where" => Where(Box::new(parse_list(args, true)?)),
        "has" => {
            let mut v = Vec::new();
            for part in split_commas(args) {
                if let Some(mut s) = parse_complex(part) {
                    if !s.relative {
                        // `:has(x)` means `:has(x)` as a descendant.
                        s.relative = true;
                        s.parts[0].0 = Combinator::Descendant;
                    }
                    v.push(s);
                }
            }
            if v.is_empty() {
                return None;
            }
            Has(Box::new(v))
        }
        "nth-child" | "nth-last-child" => {
            // `an+b [of S]`
            let args = trim_ws(args);
            let of = args
                .iter()
                .position(|c| c.ident().is_some_and(|s| s.eq_ignore_ascii_case("of")));
            let (ab, sel) = match of {
                Some(k) => (
                    &args[..k],
                    Some(Box::new(parse_list(&args[k + 1..], false)?)),
                ),
                None => (args, None),
            };
            let (a, b) = parse_nth(ab)?;
            if n == "nth-child" {
                NthChild(a, b, sel)
            } else {
                NthLastChild(a, b, sel)
            }
        }
        "nth-of-type" => {
            let (a, b) = parse_nth(args)?;
            NthOfType(a, b)
        }
        "nth-last-of-type" => {
            let (a, b) = parse_nth(args)?;
            NthLastOfType(a, b)
        }
        "lang" => Lang(
            trim_ws(args)
                .first()?
                .ident()
                .map(|s| s.to_ascii_lowercase())
                .unwrap_or_default(),
        ),
        "dir" | "host" | "host-context" | "state" => Never,
        _ => Never,
    })
}

/// Parse `an+b` (`odd`, `even`, `3`, `-n+2`, `2n + 1`, ...).
fn parse_nth(cvs: &[Cv]) -> Option<(i32, i32)> {
    let text = crate::parser::serialize(trim_ws(cvs)).to_ascii_lowercase();
    let t: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    match t.as_str() {
        "odd" => return Some((2, 1)),
        "even" => return Some((2, 0)),
        _ => {}
    }
    if let Some(npos) = t.find('n') {
        let a = match &t[..npos] {
            "" | "+" => 1,
            "-" => -1,
            s => s.parse().ok()?,
        };
        let rest = &t[npos + 1..];
        let b = if rest.is_empty() {
            0
        } else {
            rest.trim_start_matches('+').parse().ok()?
        };
        Some((a, b))
    } else {
        Some((0, t.trim_start_matches('+').parse().ok()?))
    }
}

fn parse_attr(items: &[Cv]) -> Option<Simple> {
    let items = trim_ws(items);
    let mut i = 0;
    // Optional namespace `ns|` or `*|` or `|`.
    let mut name = items.get(i)?.ident().map(|s| s.to_ascii_lowercase());
    if items
        .get(i)
        .is_some_and(|c| c.is_delim('*') || c.is_delim('|'))
        || items.get(i + 1).is_some_and(|c| c.is_delim('|'))
            && !matches!(items.get(i + 2), Some(Cv::Token(Token::Delim('='))))
    {
        while i < items.len() && !items[i].is_delim('|') {
            i += 1;
        }
        i += 1;
        name = items.get(i)?.ident().map(|s| s.to_ascii_lowercase());
    }
    let name = name?;
    i += 1;
    while i < items.len() && items[i].is_ws() {
        i += 1;
    }
    if i >= items.len() {
        return Some(Simple::Attr {
            name,
            op: AttrOp::Exists,
            value: String::new(),
            ci: false,
        });
    }
    let op = if items[i].is_delim('=') {
        i += 1;
        AttrOp::Equals
    } else {
        let op = match &items[i] {
            c if c.is_delim('~') => AttrOp::Includes,
            c if c.is_delim('|') => AttrOp::DashMatch,
            c if c.is_delim('^') => AttrOp::Prefix,
            c if c.is_delim('$') => AttrOp::Suffix,
            c if c.is_delim('*') => AttrOp::Substring,
            _ => return None,
        };
        if !items.get(i + 1)?.is_delim('=') {
            return None;
        }
        i += 2;
        op
    };
    while i < items.len() && items[i].is_ws() {
        i += 1;
    }
    let value = match items.get(i)? {
        Cv::Token(Token::Ident(s) | Token::String(s)) => s.clone(),
        Cv::Token(Token::Number { .. } | Token::Dimension { .. }) => {
            crate::parser::serialize(&items[i..=i])
        }
        _ => return None,
    };
    i += 1;
    while i < items.len() && items[i].is_ws() {
        i += 1;
    }
    let ci = items
        .get(i)
        .and_then(Cv::ident)
        .is_some_and(|s| s.eq_ignore_ascii_case("i"));
    Some(Simple::Attr {
        name,
        op,
        value,
        ci,
    })
}

// ---------------------------------------------------------------------------
// Matching
// ---------------------------------------------------------------------------

/// What selector matching needs from a DOM element handle.
pub trait Element: Copy {
    fn parent_element(&self) -> Option<Self>;
    fn prev_sibling_element(&self) -> Option<Self>;
    fn next_sibling_element(&self) -> Option<Self>;
    fn first_child_element(&self) -> Option<Self>;
    /// Lower-case tag name.
    fn local_name(&self) -> &str;
    fn attr(&self, name: &str) -> Option<&str>;
    fn is_root(&self) -> bool {
        self.parent_element().is_none()
    }
    /// No element children and no text (`:empty`).
    fn is_empty(&self) -> bool;
    fn state(&self, s: State) -> bool;
    fn same(&self, other: &Self) -> bool;
    /// Language (from `lang` attributes up the tree), lower-case.
    fn lang(&self) -> Option<String> {
        let mut e = Some(*self);
        while let Some(x) = e {
            if let Some(l) = x.attr("lang") {
                return Some(l.to_string());
            }
            e = x.parent_element();
        }
        None
    }
}

fn has_class<E: Element>(e: &E, c: &str) -> bool {
    e.attr("class")
        .is_some_and(|v| v.split_ascii_whitespace().any(|x| x == c))
}

fn attr_matches(v: &str, op: AttrOp, want: &str, ci: bool) -> bool {
    let (v, want) = if ci {
        (v.to_ascii_lowercase(), want.to_ascii_lowercase())
    } else {
        (v.to_string(), want.to_string())
    };
    match op {
        AttrOp::Exists => true,
        AttrOp::Equals => v == want,
        AttrOp::Includes => !want.is_empty() && v.split_ascii_whitespace().any(|x| x == want),
        AttrOp::DashMatch => v == want || v.starts_with(&(want.clone() + "-")),
        AttrOp::Prefix => !want.is_empty() && v.starts_with(&want),
        AttrOp::Suffix => !want.is_empty() && v.ends_with(&want),
        AttrOp::Substring => !want.is_empty() && v.contains(&want),
    }
}

fn nth_matches(a: i32, b: i32, pos: i32) -> bool {
    // pos is 1-based; is there n >= 0 with a*n + b == pos?
    if a == 0 {
        return pos == b;
    }
    let d = pos - b;
    d % a == 0 && d / a >= 0
}

fn index_where<E: Element>(e: &E, forward: bool, pred: impl Fn(&E) -> bool) -> i32 {
    let mut n = 1;
    let mut cur = if forward {
        e.prev_sibling_element()
    } else {
        e.next_sibling_element()
    };
    while let Some(s) = cur {
        if pred(&s) {
            n += 1;
        }
        cur = if forward {
            s.prev_sibling_element()
        } else {
            s.next_sibling_element()
        };
    }
    n
}

fn matches_simple<E: Element>(s: &Simple, e: &E, scope: Option<&E>) -> bool {
    match s {
        Simple::Universal => true,
        Simple::Nesting => scope.is_none_or(|sc| sc.same(e)),
        Simple::Type(t) => e.local_name().eq_ignore_ascii_case(t),
        Simple::Id(id) => e.attr("id") == Some(id.as_str()),
        Simple::Class(c) => has_class(e, c),
        Simple::Attr {
            name,
            op,
            value,
            ci,
        } => e
            .attr(name)
            .is_some_and(|v| attr_matches(v, *op, value, *ci)),
        Simple::Pseudo(p) => matches_pseudo(p, e, scope),
    }
}

fn matches_pseudo<E: Element>(p: &PseudoClass, e: &E, scope: Option<&E>) -> bool {
    use PseudoClass::*;
    let tag = e.local_name();
    match p {
        Root => e.is_root(),
        Empty => e.is_empty(),
        FirstChild => e.prev_sibling_element().is_none(),
        LastChild => e.next_sibling_element().is_none(),
        OnlyChild => e.prev_sibling_element().is_none() && e.next_sibling_element().is_none(),
        FirstOfType => index_where(e, true, |s| s.local_name() == tag) == 1,
        LastOfType => index_where(e, false, |s| s.local_name() == tag) == 1,
        OnlyOfType => {
            index_where(e, true, |s| s.local_name() == tag) == 1
                && index_where(e, false, |s| s.local_name() == tag) == 1
        }
        NthChild(a, b, of) => match of {
            Some(l) => {
                matches_list(l, e, scope)
                    && nth_matches(*a, *b, index_where(e, true, |s| matches_list(l, s, scope)))
            }
            None => nth_matches(*a, *b, index_where(e, true, |_| true)),
        },
        NthLastChild(a, b, of) => match of {
            Some(l) => {
                matches_list(l, e, scope)
                    && nth_matches(*a, *b, index_where(e, false, |s| matches_list(l, s, scope)))
            }
            None => nth_matches(*a, *b, index_where(e, false, |_| true)),
        },
        NthOfType(a, b) => nth_matches(*a, *b, index_where(e, true, |s| s.local_name() == tag)),
        NthLastOfType(a, b) => {
            nth_matches(*a, *b, index_where(e, false, |s| s.local_name() == tag))
        }
        Not(l) => !matches_list(l, e, scope),
        Is(l) | Where(l) => matches_list(l, e, scope),
        Has(rel) => rel.iter().any(|s| matches_relative(s, e)),
        State(st) => {
            if *st == self::State::Scope {
                return match scope {
                    Some(sc) => sc.same(e),
                    None => e.is_root(),
                };
            }
            e.state(*st)
        }
        Lang(l) => e.lang().is_some_and(|x| {
            let x = x.to_ascii_lowercase();
            x == *l || x.starts_with(&(l.clone() + "-"))
        }),
        Never => false,
    }
}

fn matches_compound<E: Element>(c: &Compound, e: &E, scope: Option<&E>) -> bool {
    c.0.iter().all(|s| matches_simple(s, e, scope))
}

/// Match `sel` against `e`, right to left.
pub fn matches<E: Element>(sel: &Selector, e: &E) -> bool {
    matches_scoped(sel, e, None)
}

fn matches_scoped<E: Element>(sel: &Selector, e: &E, scope: Option<&E>) -> bool {
    let n = sel.parts.len();
    if n == 0 || !matches_compound(&sel.parts[n - 1].1, e, scope) {
        return false;
    }
    match_from(sel, n - 1, *e, scope)
}

/// `parts[idx]` matched `e`; match the parts to its left.
fn match_from<E: Element>(sel: &Selector, idx: usize, e: E, scope: Option<&E>) -> bool {
    if idx == 0 {
        return true;
    }
    let comb = sel.parts[idx].0;
    let prev = &sel.parts[idx - 1].1;
    match comb {
        Combinator::Child => e.parent_element().is_some_and(|p| {
            matches_compound(prev, &p, scope) && match_from(sel, idx - 1, p, scope)
        }),
        Combinator::Descendant => {
            let mut cur = e.parent_element();
            while let Some(p) = cur {
                if matches_compound(prev, &p, scope) && match_from(sel, idx - 1, p, scope) {
                    return true;
                }
                cur = p.parent_element();
            }
            false
        }
        Combinator::NextSibling => e.prev_sibling_element().is_some_and(|s| {
            matches_compound(prev, &s, scope) && match_from(sel, idx - 1, s, scope)
        }),
        Combinator::SubsequentSibling => {
            let mut cur = e.prev_sibling_element();
            while let Some(s) = cur {
                if matches_compound(prev, &s, scope) && match_from(sel, idx - 1, s, scope) {
                    return true;
                }
                cur = s.prev_sibling_element();
            }
            false
        }
    }
}

pub fn matches_list<E: Element>(l: &SelectorList, e: &E, scope: Option<&E>) -> bool {
    l.0.iter().any(|s| matches_scoped(s, e, scope))
}

/// `:has()` argument: does some element related to `anchor` by the
/// selector's first combinator match it?
fn matches_relative<E: Element>(sel: &Selector, anchor: &E) -> bool {
    // Candidates: descendants / children / siblings of the anchor, then
    // match the selector with the anchor as its implicit left end.
    let mut cands: Vec<E> = Vec::new();
    match sel.parts[0].0 {
        Combinator::Descendant | Combinator::Child => {
            let mut stack: Vec<E> = Vec::new();
            let mut c = anchor.first_child_element();
            while let Some(x) = c {
                stack.push(x);
                c = x.next_sibling_element();
            }
            while let Some(x) = stack.pop() {
                cands.push(x);
                let mut c = x.first_child_element();
                while let Some(y) = c {
                    stack.push(y);
                    c = y.next_sibling_element();
                }
            }
        }
        Combinator::NextSibling | Combinator::SubsequentSibling => {
            let mut c = anchor.next_sibling_element();
            while let Some(x) = c {
                cands.push(x);
                c = x.next_sibling_element();
            }
        }
    }
    cands.iter().any(|c| relative_match(sel, c, anchor))
}

fn relative_match<E: Element>(sel: &Selector, e: &E, anchor: &E) -> bool {
    let n = sel.parts.len();
    if !matches_compound(&sel.parts[n - 1].1, e, None) {
        return false;
    }
    rel_from(sel, n - 1, *e, anchor)
}

fn rel_from<E: Element>(sel: &Selector, idx: usize, e: E, anchor: &E) -> bool {
    let comb = sel.parts[idx].0;
    let step = |x: &E| -> bool {
        if idx == 0 {
            x.same(anchor)
        } else {
            matches_compound(&sel.parts[idx - 1].1, x, None) && rel_from(sel, idx - 1, *x, anchor)
        }
    };
    match comb {
        Combinator::Child => e.parent_element().is_some_and(|p| step(&p)),
        Combinator::Descendant => {
            let mut cur = e.parent_element();
            while let Some(p) = cur {
                if step(&p) {
                    return true;
                }
                if p.same(anchor) {
                    return false;
                }
                cur = p.parent_element();
            }
            false
        }
        Combinator::NextSibling => e.prev_sibling_element().is_some_and(|s| step(&s)),
        Combinator::SubsequentSibling => {
            let mut cur = e.prev_sibling_element();
            while let Some(s) = cur {
                if step(&s) {
                    return true;
                }
                cur = s.prev_sibling_element();
            }
            false
        }
    }
}
