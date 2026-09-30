//! Stylesheets and the cascade: rule collection (with `@media`,
//! `@supports`, `@layer`, `@import` and nesting), indexing, matching,
//! precedence, `var()` substitution and computed styles.

use crate::media::{self, Device};
use crate::parser::{self, Cv, Declaration, Rule};
use crate::selector::{self, Element, PseudoElement, Selector, SelectorList, Simple, Specificity};
use crate::style::{self, ApplyContext, ComputedStyle, Display, Float, Position};
use crate::tokenizer::Token;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Origin {
    UserAgent,
    User,
    Author,
}

/// A style rule after flattening: one selector, its declarations.
#[derive(Debug, Clone)]
struct CompiledRule {
    selector: Selector,
    specificity: Specificity,
    decls: usize,
    origin: Origin,
    /// Cascade layer rank (unlayered = u32::MAX).
    layer: u32,
    /// Source order across all sheets.
    order: u32,
}

#[derive(Debug, Clone)]
pub struct FontFace {
    pub family: String,
    pub sources: Vec<String>,
    pub weight: u16,
    pub italic: bool,
}

/// An `@import` found while parsing: fetch it and add it with
/// [`StyleSet::add`] (before the importing sheet's own rules).
#[derive(Debug, Clone)]
pub struct Import {
    pub url: String,
    /// Media condition of the import (already evaluated: true to load).
    pub applies: bool,
}

/// All stylesheets of a document, indexed for matching.
#[derive(Default)]
pub struct StyleSet {
    rules: Vec<CompiledRule>,
    decls: Vec<Vec<Declaration>>,
    by_id: BTreeMap<String, Vec<usize>>,
    by_class: BTreeMap<String, Vec<usize>>,
    by_tag: BTreeMap<String, Vec<usize>>,
    universal: Vec<usize>,
    layers: Vec<String>,
    next_order: u32,
    pub font_faces: Vec<FontFace>,
    pub device: Device,
    /// `:root` font size for `rem` (set by the caller after styling the
    /// root, default 16).
    pub root_font_size: f32,
}

impl StyleSet {
    pub fn new(device: Device) -> StyleSet {
        StyleSet {
            device,
            root_font_size: 16.0,
            ..StyleSet::default()
        }
    }

    /// The built-in HTML user-agent stylesheet.
    pub fn with_user_agent(device: Device) -> StyleSet {
        let mut s = StyleSet::new(device);
        s.add(crate::UA_CSS, Origin::UserAgent);
        s
    }

    /// Parse and add a stylesheet. Returns its `@import`s.
    pub fn add(&mut self, src: &str, origin: Origin) -> Vec<Import> {
        let rules = parser::parse_stylesheet(src);
        let mut imports = Vec::new();
        self.add_rules(&rules, origin, u32::MAX, None, &mut imports, true);
        imports
    }

    fn layer_rank(&mut self, name: &str) -> u32 {
        if let Some(i) = self.layers.iter().position(|l| l == name) {
            return i as u32;
        }
        self.layers.push(name.to_string());
        (self.layers.len() - 1) as u32
    }

    fn add_rules(
        &mut self,
        rules: &[Rule],
        origin: Origin,
        layer: u32,
        parent: Option<&SelectorList>,
        imports: &mut Vec<Import>,
        top: bool,
    ) {
        let mut anon = 0;
        for r in rules {
            match r {
                Rule::Qualified { prelude, block } => {
                    let Some(mut list) = SelectorList::parse(prelude) else {
                        continue;
                    };
                    if let Some(p) = parent {
                        list = SelectorList(list.0.iter().map(|s| s.resolve_nesting(p)).collect());
                    } else if list.contains_nesting() {
                        // `&` at the top level means :scope (the root).
                        list = SelectorList(
                            list.0
                                .iter()
                                .map(|s| {
                                    s.resolve_nesting(&SelectorList::parse_str(":root").unwrap())
                                })
                                .collect(),
                        );
                    }
                    let contents = parser::parse_block(block);
                    if !contents.declarations.is_empty() {
                        self.push_rule(&list, contents.declarations, origin, layer);
                    }
                    if !contents.rules.is_empty() {
                        self.add_rules(&contents.rules, origin, layer, Some(&list), imports, false);
                    }
                }
                Rule::At {
                    name,
                    prelude,
                    block,
                } => match name.as_str() {
                    "media" => {
                        if media::matches(prelude, &self.device)
                            && let Some(b) = block
                        {
                            let inner = parse_nested(b, parent.is_some());
                            self.add_rules(&inner, origin, layer, parent, imports, false);
                        }
                    }
                    "supports" => {
                        if media::supports(prelude, &style::supported)
                            && let Some(b) = block
                        {
                            let inner = parse_nested(b, parent.is_some());
                            self.add_rules(&inner, origin, layer, parent, imports, false);
                        }
                    }
                    "layer" => {
                        let names: Vec<String> = parser::serialize(prelude)
                            .split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect();
                        match block {
                            Some(b) => {
                                let lname = names.first().cloned().unwrap_or_else(|| {
                                    anon += 1;
                                    alloc::format!("\u{0}anon{}-{}", self.next_order, anon)
                                });
                                let rank = self.layer_rank(&lname);
                                let inner = parse_nested(b, parent.is_some());
                                self.add_rules(&inner, origin, rank, parent, imports, false);
                            }
                            None => {
                                for n in names {
                                    self.layer_rank(&n);
                                }
                            }
                        }
                    }
                    "import" if top => {
                        let it: Vec<&Cv> = prelude.iter().filter(|c| !c.is_ws()).collect();
                        let url = match it.first() {
                            Some(Cv::Token(Token::String(s) | Token::Url(s))) => s.clone(),
                            Some(Cv::Function { name, args })
                                if name.eq_ignore_ascii_case("url") =>
                            {
                                match args.iter().find(|c| !c.is_ws()) {
                                    Some(Cv::Token(Token::String(s))) => s.clone(),
                                    _ => continue,
                                }
                            }
                            _ => continue,
                        };
                        let rest_start = prelude
                            .iter()
                            .position(|c| core::ptr::eq(c, it[0]))
                            .map_or(prelude.len(), |p| p + 1);
                        let rest: Vec<Cv> = prelude[rest_start..]
                            .iter()
                            .filter(|c| !matches!(c, Cv::Function { name, .. } if name.eq_ignore_ascii_case("layer") || name.eq_ignore_ascii_case("supports")))
                            .filter(|c| c.ident().is_none_or(|s| !s.eq_ignore_ascii_case("layer")))
                            .cloned()
                            .collect();
                        imports.push(Import {
                            url,
                            applies: media::matches(&rest, &self.device),
                        });
                    }
                    "font-face" => {
                        if let Some(b) = block {
                            let decls = parser::parse_block(b).declarations;
                            let mut ff = FontFace {
                                family: String::new(),
                                sources: Vec::new(),
                                weight: 400,
                                italic: false,
                            };
                            for d in decls {
                                match d.name.as_str() {
                                    "font-family" => {
                                        ff.family = match d.value.iter().find(|c| !c.is_ws()) {
                                            Some(Cv::Token(Token::String(s) | Token::Ident(s))) => {
                                                s.clone()
                                            }
                                            _ => String::new(),
                                        }
                                    }
                                    "src" => {
                                        for c in &d.value {
                                            match c {
                                                Cv::Token(Token::Url(u)) => {
                                                    ff.sources.push(u.clone())
                                                }
                                                Cv::Function { name, args }
                                                    if name.eq_ignore_ascii_case("url") =>
                                                {
                                                    if let Some(Cv::Token(Token::String(s))) =
                                                        args.iter().find(|c| !c.is_ws())
                                                    {
                                                        ff.sources.push(s.clone());
                                                    }
                                                }
                                                _ => {}
                                            }
                                        }
                                    }
                                    "font-weight" => {
                                        if let Some(n) =
                                            d.value.iter().find_map(crate::values::number)
                                        {
                                            ff.weight = n as u16;
                                        } else if d.value.iter().any(|c| {
                                            c.ident()
                                                .is_some_and(|s| s.eq_ignore_ascii_case("bold"))
                                        }) {
                                            ff.weight = 700;
                                        }
                                    }
                                    "font-style" => {
                                        ff.italic = d.value.iter().any(|c| {
                                            c.ident().is_some_and(|s| {
                                                s.eq_ignore_ascii_case("italic")
                                                    || s.eq_ignore_ascii_case("oblique")
                                            })
                                        })
                                    }
                                    _ => {}
                                }
                            }
                            if !ff.family.is_empty() && !ff.sources.is_empty() {
                                self.font_faces.push(ff);
                            }
                        }
                    }
                    "scope" | "container" | "starting-style" => {
                        // Approximated: the rules apply unconditionally.
                        if let Some(b) = block {
                            let inner = parse_nested(b, parent.is_some());
                            self.add_rules(&inner, origin, layer, parent, imports, false);
                        }
                    }
                    _ => {} // @keyframes, @page, @namespace, @charset, @property ...
                },
            }
        }
    }

    fn push_rule(
        &mut self,
        list: &SelectorList,
        decls: Vec<Declaration>,
        origin: Origin,
        layer: u32,
    ) {
        let di = self.decls.len();
        self.decls.push(decls);
        for sel in &list.0 {
            let idx = self.rules.len();
            self.rules.push(CompiledRule {
                specificity: sel.specificity(),
                selector: sel.clone(),
                decls: di,
                origin,
                layer,
                order: self.next_order,
            });
            self.next_order += 1;
            // Index by the rightmost compound's most selective part.
            let right = &sel.parts.last().unwrap().1.0;
            if let Some(id) = right
                .iter()
                .find_map(|s| if let Simple::Id(i) = s { Some(i) } else { None })
            {
                self.by_id.entry(id.clone()).or_default().push(idx);
            } else if let Some(c) = right.iter().find_map(|s| {
                if let Simple::Class(c) = s {
                    Some(c)
                } else {
                    None
                }
            }) {
                self.by_class.entry(c.clone()).or_default().push(idx);
            } else if let Some(t) = right.iter().find_map(|s| {
                if let Simple::Type(t) = s {
                    Some(t)
                } else {
                    None
                }
            }) {
                self.by_tag.entry(t.clone()).or_default().push(idx);
            } else {
                self.universal.push(idx);
            }
        }
    }

    /// Rules that may match `e` (by the index), then filtered by the full
    /// selector.
    fn matching<E: Element>(&self, e: &E, pseudo: Option<PseudoElement>) -> Vec<usize> {
        let mut cands: Vec<usize> = Vec::new();
        if let Some(id) = e.attr("id")
            && let Some(v) = self.by_id.get(id)
        {
            cands.extend(v);
        }
        if let Some(cls) = e.attr("class") {
            for c in cls.split_ascii_whitespace() {
                if let Some(v) = self.by_class.get(c) {
                    cands.extend(v);
                }
            }
        }
        if let Some(v) = self.by_tag.get(e.local_name()) {
            cands.extend(v);
        }
        cands.extend(&self.universal);
        cands.sort_unstable();
        cands.dedup();
        cands.retain(|&i| {
            let r = &self.rules[i];
            r.selector.pseudo_element == pseudo && selector::matches(&r.selector, e)
        });
        cands
    }

    /// Whether any rule creates `pseudo` content for `e` (a quick check
    /// before computing a pseudo-element style).
    pub fn has_pseudo<E: Element>(&self, e: &E, pseudo: PseudoElement) -> bool {
        !self.matching(e, Some(pseudo)).is_empty()
    }

    /// Compute the style of `e` (or of its pseudo-element), given its
    /// parent's computed style (or the originating element's for
    /// pseudo-elements), the `style` attribute and presentational hints.
    pub fn compute<E: Element>(
        &self,
        e: &E,
        parent: Option<&ComputedStyle>,
        inline: Option<&str>,
        hints: &[Declaration],
        pseudo: Option<PseudoElement>,
    ) -> ComputedStyle {
        let default_parent = ComputedStyle::default();
        let parent_style = parent.unwrap_or(&default_parent);
        let mut st = ComputedStyle::inherit_from(parent_style);
        // (precedence key, declaration)
        let mut decls: Vec<((u8, u32, Specificity, u32), &Declaration)> = Vec::new();
        let rules = self.matching(e, pseudo);
        let inline_decls: Vec<Declaration> = inline
            .map(parser::parse_style_attribute)
            .unwrap_or_default();
        for i in rules {
            let r = &self.rules[i];
            for d in &self.decls[r.decls] {
                decls.push((
                    precedence(r.origin, d.important, r.layer, r.specificity, r.order),
                    d,
                ));
            }
        }
        for d in hints {
            // Presentational hints: author origin, zero specificity, before
            // every author rule.
            decls.push(((2, 0, Specificity::default(), 0), d));
        }
        for (k, d) in inline_decls.iter().enumerate() {
            // The style attribute beats every author rule of the same
            // importance.
            let key = if d.important {
                (4, u32::MAX, Specificity(u32::MAX, 0, 0), k as u32)
            } else {
                (3, u32::MAX, Specificity(u32::MAX, 0, 0), k as u32)
            };
            decls.push((key, d));
        }
        decls.sort_by(|a, b| a.0.cmp(&b.0));

        // 1. Custom properties (in order), resolving var() among them.
        for (_, d) in decls.iter().filter(|(_, d)| d.name.starts_with("--")) {
            let v = substitute_vars(&d.value, &st, 0).unwrap_or_default();
            st.set_custom(&d.name, v);
        }
        let ctx = ApplyContext {
            parent: parent_style,
            root_font_size: self.root_font_size,
            viewport_w: self.device.width,
            viewport_h: self.device.height,
        };
        // 2. Font size first (em units in other properties depend on it).
        let is_font = |n: &str| matches!(n, "font-size" | "font");
        for (_, d) in decls.iter().filter(|(_, d)| is_font(&d.name)) {
            apply_decl(&mut st, d, &ctx);
        }
        // 3. Everything else.
        for (_, d) in decls
            .iter()
            .filter(|(_, d)| !d.name.starts_with("--") && !is_font(&d.name))
        {
            apply_decl(&mut st, d, &ctx);
        }
        fixup(&mut st, parent, e.is_root() && pseudo.is_none());
        st
    }
}

fn parse_nested(block: &[Cv], nested: bool) -> Vec<Rule> {
    if nested {
        // Inside a style rule: a declaration list with nested rules.
        let c = parser::parse_block(block);
        let mut out = c.rules;
        if !c.declarations.is_empty() {
            // Bare declarations inside a nested @media apply to `&`.
            let mut toks = Vec::new();
            toks.push(Cv::Token(Token::Delim('&')));
            let body: Vec<Cv> = parser::component_values(&decls_to_css(&c.declarations));
            out.insert(
                0,
                Rule::Qualified {
                    prelude: toks,
                    block: body,
                },
            );
        }
        out
    } else {
        parser::rules_from(block, false)
    }
}

fn decls_to_css(d: &[Declaration]) -> String {
    let mut s = String::new();
    for x in d {
        s.push_str(&x.name);
        s.push(':');
        s.push_str(&parser::serialize(&x.value));
        if x.important {
            s.push_str(" !important");
        }
        s.push(';');
    }
    s
}

/// Sort key: later (greater) wins.
fn precedence(
    origin: Origin,
    important: bool,
    layer: u32,
    spec: Specificity,
    order: u32,
) -> (u8, u32, Specificity, u32) {
    let o = match (origin, important) {
        (Origin::UserAgent, false) => 0,
        (Origin::User, false) => 1,
        (Origin::Author, false) => 2,
        (Origin::Author, true) => 4,
        (Origin::User, true) => 5,
        (Origin::UserAgent, true) => 7,
    };
    // Important declarations reverse the layer order (earlier layers and
    // then unlayered styles win... reversed: unlayered lose).
    let l = if important { u32::MAX - layer } else { layer };
    (o, l, spec, order)
}

fn apply_decl(st: &mut ComputedStyle, d: &Declaration, ctx: &ApplyContext) {
    let has_var = contains_var(&d.value);
    if has_var {
        match substitute_vars(&d.value, st, 0) {
            Some(v) => {
                if !style::apply(st, &d.name, &v, ctx) {
                    // Invalid at computed-value time: behaves as `unset`.
                    let unset = [Cv::Token(Token::Ident("unset".into()))];
                    style::apply(st, &d.name, &unset, ctx);
                }
            }
            None => {
                let unset = [Cv::Token(Token::Ident("unset".into()))];
                style::apply(st, &d.name, &unset, ctx);
            }
        }
    } else {
        style::apply(st, &d.name, &d.value, ctx);
    }
}

fn contains_var(v: &[Cv]) -> bool {
    v.iter().any(|c| match c {
        Cv::Function { name, args } => name.eq_ignore_ascii_case("var") || contains_var(args),
        Cv::Block { items, .. } => contains_var(items),
        _ => false,
    })
}

/// Replace `var(--x[, fallback])` with the custom property's value.
pub fn substitute_vars(v: &[Cv], st: &ComputedStyle, depth: u32) -> Option<Vec<Cv>> {
    if depth > 16 {
        return None;
    }
    let mut out = Vec::with_capacity(v.len());
    for c in v {
        match c {
            Cv::Function { name, args } if name.eq_ignore_ascii_case("var") => {
                let comma = args
                    .iter()
                    .position(|x| matches!(x, Cv::Token(Token::Comma)));
                let (namepart, fallback) = match comma {
                    Some(p) => (&args[..p], Some(&args[p + 1..])),
                    None => (&args[..], None),
                };
                let var = namepart.iter().find_map(|x| x.ident())?;
                match st.custom_property(var).filter(|v| !v.iter().all(Cv::is_ws)) {
                    Some(val) => out.extend(substitute_vars(val, st, depth + 1)?),
                    None => out.extend(substitute_vars(fallback?, st, depth + 1)?),
                }
            }
            Cv::Function { name, args } => out.push(Cv::Function {
                name: name.clone(),
                args: substitute_vars(args, st, depth)?,
            }),
            Cv::Block { open, items } => out.push(Cv::Block {
                open: *open,
                items: substitute_vars(items, st, depth)?,
            }),
            other => out.push(other.clone()),
        }
    }
    Some(out)
}

/// Adjustments after the cascade (CSS 2 §9.7 and friends).
fn fixup(st: &mut ComputedStyle, parent: Option<&ComputedStyle>, root: bool) {
    if st.display == Display::None {
        return;
    }
    if matches!(st.position, Position::Absolute | Position::Fixed) {
        st.float = Float::None;
        st.display = st.display.blockify();
    } else if st.float != Float::None || root {
        st.display = st.display.blockify();
    }
    if let Some(p) = parent
        && matches!(
            p.display,
            Display::Flex | Display::InlineFlex | Display::Grid | Display::InlineGrid
        )
    {
        st.display = st.display.blockify();
        st.float = Float::None;
    }
    // Border widths of `none`/`hidden` borders compute to 0.
    for i in 0..4 {
        if matches!(
            st.border_style[i],
            style::BorderStyle::None | style::BorderStyle::Hidden
        ) {
            st.border_width[i] = 0.0;
        }
    }
    if st.outline_style == style::BorderStyle::None {
        st.outline_width = 0.0;
    }
    // Overflow: visible/clip pairs with scrollable values become auto.
    use style::Overflow::*;
    if matches!(st.overflow_x, Visible | Clip) && matches!(st.overflow_y, Hidden | Scroll | Auto) {
        st.overflow_x = if st.overflow_x == Visible {
            Auto
        } else {
            Hidden
        };
    }
    if matches!(st.overflow_y, Visible | Clip) && matches!(st.overflow_x, Hidden | Scroll | Auto) {
        st.overflow_y = if st.overflow_y == Visible {
            Auto
        } else {
            Hidden
        };
    }
}
