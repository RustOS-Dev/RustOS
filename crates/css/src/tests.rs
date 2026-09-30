use crate::parser::*;
use crate::selector::*;
use crate::style::*;
use crate::values::*;
use crate::*;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// A tiny DOM for tests: (tag, attrs, parent, children).
struct Dom {
    nodes: Vec<(String, Vec<(String, String)>, Option<usize>, Vec<usize>)>,
    hover: Option<usize>,
}

impl Dom {
    fn new() -> Dom {
        Dom {
            nodes: vec![(String::from("html"), vec![], None, vec![])],
            hover: None,
        }
    }
    fn add(&mut self, parent: usize, tag: &str, attrs: &[(&str, &str)]) -> usize {
        let id = self.nodes.len();
        self.nodes.push((
            tag.into(),
            attrs
                .iter()
                .map(|(a, b)| (String::from(*a), String::from(*b)))
                .collect(),
            Some(parent),
            vec![],
        ));
        self.nodes[parent].3.push(id);
        id
    }
}

#[derive(Clone, Copy)]
struct E<'a>(&'a Dom, usize);

impl Element for E<'_> {
    fn parent_element(&self) -> Option<Self> {
        self.0.nodes[self.1].2.map(|p| E(self.0, p))
    }
    fn prev_sibling_element(&self) -> Option<Self> {
        let p = self.0.nodes[self.1].2?;
        let sibs = &self.0.nodes[p].3;
        let i = sibs.iter().position(|&x| x == self.1)?;
        (i > 0).then(|| E(self.0, sibs[i - 1]))
    }
    fn next_sibling_element(&self) -> Option<Self> {
        let p = self.0.nodes[self.1].2?;
        let sibs = &self.0.nodes[p].3;
        let i = sibs.iter().position(|&x| x == self.1)?;
        sibs.get(i + 1).map(|&x| E(self.0, x))
    }
    fn first_child_element(&self) -> Option<Self> {
        self.0.nodes[self.1].3.first().map(|&x| E(self.0, x))
    }
    fn local_name(&self) -> &str {
        &self.0.nodes[self.1].0
    }
    fn attr(&self, name: &str) -> Option<&str> {
        self.0.nodes[self.1]
            .1
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
    fn is_empty(&self) -> bool {
        self.0.nodes[self.1].3.is_empty()
    }
    fn state(&self, s: State) -> bool {
        match s {
            State::Hover => self.0.hover == Some(self.1),
            State::Link | State::AnyLink => self.local_name() == "a" && self.attr("href").is_some(),
            _ => false,
        }
    }
    fn same(&self, o: &Self) -> bool {
        self.1 == o.1
    }
}

fn sel_matches(dom: &Dom, n: usize, s: &str) -> bool {
    let l = SelectorList::parse_str(s).unwrap_or_else(|| panic!("parse {s}"));
    matches_list(&l, &E(dom, n), None)
}

#[test]
fn tokenizer_basics() {
    use crate::tokenizer::{Token, tokenize};
    let t = tokenize(
        "a.b > #c:hover{color:red !important;width:calc(100% - 2.5em)}/* x */ url(x.png) 10px 50% -.5e1",
    );
    assert!(t.contains(&Token::Hash {
        value: "c".into(),
        id: true
    }));
    assert!(t.contains(&Token::Dimension {
        value: 2.5,
        unit: "em".into()
    }));
    assert!(t.contains(&Token::Url("x.png".into())));
    assert!(t.contains(&Token::Percentage(50.0)));
    assert!(t.contains(&Token::Number {
        value: -5.0,
        int: false
    }));
    assert_eq!(tokenize("'a\\'b'"), vec![Token::String("a'b".into())]);
    assert_eq!(tokenize("\\31 x"), vec![Token::Ident("1x".into())]);
}

#[test]
fn parser_rules_and_declarations() {
    let rules = parse_stylesheet(
        "@media screen { a { color: red } } b{x:1;y : 2 !IMPORTANT} @import 'z.css';",
    );
    assert_eq!(rules.len(), 3);
    let Rule::Qualified { block, .. } = &rules[1] else {
        panic!()
    };
    let d = parse_block(block).declarations;
    assert_eq!(d.len(), 2);
    assert!(d[1].important && !d[0].important);
    // Nesting.
    let c = parse_block(&component_values(
        "color: red; & .x { color: blue } .y & { a: b } div { c: d }",
    ));
    assert_eq!(c.declarations.len(), 1);
    assert_eq!(c.rules.len(), 3);
}

#[test]
fn selector_matching() {
    let mut d = Dom::new();
    let body = d.add(0, "body", &[]);
    let ul = d.add(body, "ul", &[("class", "menu big"), ("id", "m")]);
    let li1 = d.add(ul, "li", &[("data-x", "Hello-world")]);
    let li2 = d.add(ul, "li", &[("class", "sel")]);
    let li3 = d.add(ul, "li", &[]);
    let a = d.add(li2, "a", &[("href", "/x")]);
    assert!(sel_matches(&d, li1, "ul > li:first-child"));
    assert!(sel_matches(&d, li2, ".menu.big li:nth-child(2)"));
    assert!(sel_matches(&d, li3, "li:last-child:nth-last-child(1)"));
    assert!(sel_matches(&d, li2, "li:nth-child(2n of li)"));
    assert!(sel_matches(&d, li3, "li.sel ~ li"));
    assert!(sel_matches(&d, li2, "li:first-child + li"));
    assert!(sel_matches(&d, li1, "[data-x|=hello i]"));
    assert!(sel_matches(
        &d,
        li1,
        "[data-x^=Hello][data-x$=world][data-x*=o-w]"
    ));
    assert!(!sel_matches(&d, li1, "[data-x=hello]"));
    assert!(sel_matches(&d, a, "#m a:link"));
    assert!(sel_matches(&d, li2, "li:has(> a[href])"));
    assert!(sel_matches(&d, ul, "ul:has(.sel)"));
    assert!(!sel_matches(&d, li1, "li:has(a)"));
    assert!(sel_matches(&d, li1, "li:not(.sel, :last-child)"));
    assert!(sel_matches(
        &d,
        li3,
        ":is(ul, ol) :where(li):nth-of-type(3)"
    ));
    assert!(sel_matches(&d, 0, ":root"));
    assert!(sel_matches(&d, li3, "li:empty"));
    d.hover = Some(li1);
    assert!(sel_matches(&d, li1, "li:hover"));
    // Specificity.
    let s = |x: &str| SelectorList::parse_str(x).unwrap().0[0].specificity();
    assert_eq!(s("#a .b c"), Specificity(1, 1, 1));
    assert_eq!(s(":where(#a) .b"), Specificity(0, 1, 0));
    assert_eq!(s(":is(#a, .b) c::before"), Specificity(1, 0, 2));
    assert_eq!(s("li:nth-child(2n of .x)"), Specificity(0, 2, 1));
    assert!(SelectorList::parse_str("a::unknown").is_none());
}

#[test]
fn values_lengths_and_calc() {
    let ctx = LengthContext {
        font_size: 20.0,
        root_font_size: 10.0,
        viewport_w: 1000.0,
        viewport_h: 500.0,
        line_height: 24.0,
    };
    let lp = |s: &str| length_percentage(&component_values(s)[0], &ctx).unwrap();
    assert_eq!(lp("2em").resolve(0.0), 40.0);
    assert_eq!(lp("3rem").resolve(0.0), 30.0);
    assert_eq!(lp("10vw").resolve(0.0), 100.0);
    assert_eq!(lp("calc(100% - 2em)").resolve(200.0), 160.0);
    assert_eq!(lp("calc((100% - 10px) / 2)").resolve(110.0), 50.0);
    assert_eq!(lp("min(50%, 300px)").resolve(1000.0), 300.0);
    assert_eq!(lp("clamp(10px, 5%, 40px)").resolve(1000.0), 40.0);
    assert_eq!(lp("max(1em, 12px)").resolve(0.0), 20.0);
    assert!((lp("1in").resolve(0.0) - 96.0).abs() < 0.01);
}

#[test]
fn colors() {
    let c = |s: &str| color(&component_values(s)[0]).unwrap();
    assert_eq!(c("red"), Color::Rgba(Rgba::rgb(255, 0, 0)));
    assert_eq!(
        c("#0f08"),
        Color::Rgba(Rgba {
            r: 0,
            g: 255,
            b: 0,
            a: 0x88
        })
    );
    assert_eq!(
        c("rgb(10 20 30 / 50%)"),
        Color::Rgba(Rgba {
            r: 10,
            g: 20,
            b: 30,
            a: 128
        })
    );
    assert_eq!(
        c("rgba(10, 20, 30, 0.5)"),
        Color::Rgba(Rgba {
            r: 10,
            g: 20,
            b: 30,
            a: 128
        })
    );
    assert_eq!(c("hsl(120deg 100% 50%)"), Color::Rgba(Rgba::rgb(0, 255, 0)));
    assert_eq!(c("currentColor"), Color::CurrentColor);
    assert_eq!(c("RebeccaPurple"), Color::Rgba(Rgba::rgb(0x66, 0x33, 0x99)));
    let Color::Rgba(ok) = c("oklch(62.8% 0.2577 29.23)") else {
        panic!()
    };
    assert!(ok.r > 240 && ok.g < 20 && ok.b < 20, "{:?}", ok);
    let Color::Rgba(lab) = c("lab(50% 0 0)") else {
        panic!()
    };
    assert!(
        (lab.r as i32 - 119).abs() <= 2 && lab.r == lab.g && lab.g == lab.b,
        "{:?}",
        lab
    );
}

#[test]
fn media_queries() {
    let dev = Device {
        width: 800.0,
        height: 600.0,
        dark: true,
        ..Device::default()
    };
    let m = |s: &str| media::matches(&component_values(s), &dev);
    assert!(m("screen and (min-width: 700px)"));
    assert!(!m("print"));
    assert!(m("not print"));
    assert!(m("(width >= 600px) and (width < 900px)"));
    assert!(m("(400px <= width <= 800px)"));
    assert!(!m("(max-width: 50em)") == false);
    assert!(m("(prefers-color-scheme: dark)"));
    assert!(m("(orientation: landscape), print"));
    assert!(!m("(min-aspect-ratio: 16/9)"));
    assert!(m("(hover) and (pointer: fine)"));
    assert!(m(
        "only screen and ((min-width: 100px) or (max-width: 10px))"
    ));
    assert!(m(""));
}

fn style_of(dom: &Dom, n: usize, css: &str, parent: Option<&ComputedStyle>) -> ComputedStyle {
    let mut set = StyleSet::with_user_agent(Device::default());
    set.add(css, Origin::Author);
    let inline = E(dom, n).attr("style").map(String::from);
    set.compute(&E(dom, n), parent, inline.as_deref(), &[], None)
}

#[test]
fn cascade_order_and_properties() {
    let mut d = Dom::new();
    let body = d.add(0, "body", &[]);
    let p = d.add(
        body,
        "p",
        &[("class", "x"), ("id", "i"), ("style", "margin-left: 7px")],
    );
    let css = "
        #i { color: blue }
        p.x { color: red; padding: 1px 2px 3px }
        p { color: green !important; margin: 5px auto; font: italic bold 20px/1.5 \"Helvetica Neue\", sans-serif }
        @layer base { p { border: 2px solid } }
        :root { --gap: 12px; --c: rgb(1 2 3) }
        p { column-gap: var(--gap); background: var(--c) url(a.png) no-repeat center / cover; }
        @supports (display: grid) { p { display: grid } }
        @supports not (display: bogus) { p { opacity: .5 } }
        @media (max-width: 10px) { p { display: none } }
    ";
    let root = style_of(&d, 0, css, None);
    let bs = style_of(&d, body, css, Some(&root));
    let s = style_of(&d, p, css, Some(&bs));
    assert_eq!(s.color, Rgba::rgb(0, 128, 0));
    assert_eq!(s.padding[3].resolve(0.0), 2.0);
    assert_eq!(s.padding[2].resolve(0.0), 3.0);
    assert_eq!(s.margin[3], LengthAuto::Lp(LengthPercentage::px(7.0)));
    assert!(s.margin[1].is_auto());
    assert_eq!(s.font_size, 20.0);
    assert_eq!(s.font_weight, 700);
    assert_eq!(s.font_style, FontStyle::Italic);
    assert_eq!(s.line_height_px(), 30.0);
    assert_eq!(
        s.font_family,
        vec![String::from("Helvetica Neue"), String::from("sans-serif")]
    );
    assert_eq!(s.border_width, [2.0; 4]);
    assert_eq!(s.column_gap, Some(LengthPercentage::px(12.0)));
    assert_eq!(s.background_color, Color::Rgba(Rgba::rgb(1, 2, 3)));
    assert_eq!(s.background.len(), 1);
    assert_eq!(s.background[0].size, BgSize::Cover);
    assert_eq!(s.display, Display::Grid);
    assert_eq!(s.opacity, 0.5);
    // Border color defaults to currentColor.
    assert_eq!(s.border_color[0].resolve(s.color), s.color);
}

#[test]
fn layers_and_important() {
    let mut d = Dom::new();
    let p = d.add(0, "p", &[]);
    let css = "@layer a, b; @layer b { p { color: red } } @layer a { p { color: blue } } p { color: black }";
    assert_eq!(style_of(&d, p, css, None).color, Rgba::BLACK);
    let css = "@layer a, b; @layer b { p { color: red } } @layer a { p { color: blue !important } } p { color: black !important }";
    assert_eq!(style_of(&d, p, css, None).color, Rgba::rgb(0, 0, 255));
}

#[test]
fn nesting_flattens() {
    let mut d = Dom::new();
    let nav = d.add(0, "nav", &[("class", "top")]);
    let a = d.add(nav, "a", &[("href", "#")]);
    let css = ".top { display: flex; & a { color: red } > a:link { font-weight: 700 } @media screen { a { opacity: 0.25 } } }";
    let ns = style_of(&d, nav, css, None);
    assert_eq!(ns.display, Display::Flex);
    let s = style_of(&d, a, css, Some(&ns));
    assert_eq!(s.color, Rgba::rgb(255, 0, 0));
    assert_eq!(s.font_weight, 700);
    assert_eq!(s.opacity, 0.25);
    // A flex item is blockified.
    assert_eq!(s.display, Display::Block);
}

#[test]
fn flex_and_grid_properties() {
    let mut d = Dom::new();
    let g = d.add(0, "div", &[]);
    let css = "div { flex: 2 1 30%; grid-template-columns: [a] repeat(auto-fill, minmax(100px, 1fr)) 2fr; grid-template-areas: 'h h' 'n m';
              grid-area: 2 / 1 / span 2 / 3; place-items: center end; gap: 1em 5% }";
    let s = style_of(&d, g, css, None);
    assert_eq!((s.flex_grow, s.flex_shrink), (2.0, 1.0));
    assert_eq!(s.flex_basis, FlexBasis::Lp(LengthPercentage::percent(30.0)));
    let GridTemplate::Tracks(t) = &s.grid_template_columns else {
        panic!()
    };
    assert_eq!(t.len(), 3);
    assert!(matches!(&t[1], TrackItem::Repeat(RepeatCount::AutoFill, _)));
    assert_eq!(s.grid_template_areas.len(), 2);
    assert_eq!(s.grid_row_start, GridLine::Line(2, None));
    assert_eq!(s.grid_row_end, GridLine::Span(2, None));
    assert_eq!(s.grid_column_end, GridLine::Line(3, None));
    assert_eq!(s.align_items, style::Align::Center);
    assert_eq!(s.justify_items, style::Align::End);
    assert_eq!(s.row_gap, Some(LengthPercentage::px(16.0)));
    assert_eq!(s.column_gap, Some(LengthPercentage::percent(5.0)));
}

#[test]
fn ua_defaults_and_pseudo_elements() {
    let mut d = Dom::new();
    let body = d.add(0, "body", &[]);
    let h1 = d.add(body, "h1", &[]);
    let ul = d.add(body, "ul", &[]);
    let li = d.add(ul, "li", &[]);
    let head = d.add(0, "head", &[]);
    let root = style_of(&d, 0, "", None);
    assert_eq!(root.display, Display::Block);
    let bs = style_of(&d, body, "", Some(&root));
    assert_eq!(bs.margin[0], LengthAuto::Lp(LengthPercentage::px(8.0)));
    let hs = style_of(&d, h1, "", Some(&bs));
    assert_eq!(hs.font_size, 32.0);
    assert_eq!(hs.font_weight, 700);
    assert_eq!(
        style_of(&d, li, "", Some(&style_of(&d, ul, "", Some(&bs)))).display,
        Display::ListItem
    );
    assert_eq!(style_of(&d, head, "", Some(&root)).display, Display::None);
    let mut set = StyleSet::with_user_agent(Device::default());
    set.add(
        "h1::before { content: \"§ \" counter(x) attr(id); color: red }",
        Origin::Author,
    );
    assert!(set.has_pseudo(&E(&d, h1), PseudoElement::Before));
    let ps = set.compute(
        &E(&d, h1),
        Some(&hs),
        None,
        &[],
        Some(PseudoElement::Before),
    );
    let Content::Items(items) = &ps.content else {
        panic!()
    };
    assert_eq!(items.len(), 3);
    assert_eq!(ps.color, Rgba::rgb(255, 0, 0));
}

#[test]
fn hints_map_attributes() {
    let attrs = vec![
        (String::from("width"), String::from("50%")),
        (String::from("bgcolor"), String::from("ff0000")),
        (String::from("align"), String::from("center")),
    ];
    let h = hints::presentational_hints("table", &attrs, &|_| None);
    let mut d = Dom::new();
    let t = d.add(0, "table", &[]);
    let set = StyleSet::with_user_agent(Device::default());
    let s = set.compute(&E(&d, t), None, None, &h, None);
    assert_eq!(s.width, Size::Lp(LengthPercentage::percent(50.0)));
    assert_eq!(s.background_color, Color::Rgba(Rgba::rgb(255, 0, 0)));
    assert!(s.margin[1].is_auto() && s.margin[3].is_auto());
}

#[test]
fn invalid_declarations_are_ignored() {
    let mut d = Dom::new();
    let p = d.add(0, "p", &[]);
    let s = style_of(
        &d,
        p,
        "p { color: blue; color: nonsense; width: -5px; width: 10px; width: 3 }",
        None,
    );
    assert_eq!(s.color, Rgba::rgb(0, 0, 255));
    assert_eq!(s.width, Size::Lp(LengthPercentage::px(10.0)));
    // var() with a missing variable and no fallback: unset.
    let s = style_of(&d, p, "p { color: red; color: var(--nope) }", None);
    assert_eq!(s.color, Rgba::BLACK);
    let s = style_of(
        &d,
        p,
        "p { color: var(--nope, var(--also-no, #00f)) }",
        None,
    );
    assert_eq!(s.color, Rgba::rgb(0, 0, 255));
}
