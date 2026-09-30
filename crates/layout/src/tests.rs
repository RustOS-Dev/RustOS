extern crate std;
use crate::*;
use alloc::string::String;
use alloc::vec::Vec;

fn lay(src: &str, width: usize) -> Page {
    render(
        &html::parse(src),
        &Options {
            width,
            ..Options::default()
        },
    )
}

fn text(p: &Page) -> Vec<String> {
    p.lines
        .iter()
        .map(|l| String::from(l.text().trim_end()))
        .collect()
}

#[test]
fn paragraphs_wrap_and_collapse() {
    let p = lay(
        "<p>The   quick brown\n fox jumps over the lazy dog.</p><p>Second</p>",
        20,
    );
    assert_eq!(
        text(&p),
        [
            "The quick brown fox",
            "jumps over the lazy",
            "dog.",
            "",
            "Second"
        ]
    );
    let p = lay(
        "<div>a<b>b</b>c <i>supercalifragilisticexpialidocious</i></div>",
        20,
    );
    assert_eq!(text(&p), ["abc", "supercalifragilistic", "expialidocious"]);
    assert!(
        p.lines[0]
            .spans
            .iter()
            .any(|s| s.text == "b" && s.style.bold)
    );
    assert!(p.lines[1].spans.iter().all(|s| s.style.italic));
}

#[test]
fn headings_lists_quotes_rules() {
    let p = lay(
        "<h1>Title</h1><ul><li>one<li>two<ol start=3><li>three<li>four</ol></ul>\
         <blockquote>quoted text here</blockquote><hr><dl><dt>Term<dd>Definition</dl>",
        30,
    );
    assert_eq!(
        text(&p),
        [
            "Title",
            "",
            "   * one",
            "   * two",
            "       3. three",
            "       4. four",
            "",
            "     quoted text here",
            "",
            "──────────────────────────────",
            "",
            "Term",
            "     Definition",
        ]
    );
    assert!(p.lines[0].spans[0].style.heading && p.lines[0].spans[0].style.bold);
    // Hanging indent for wrapped list items.
    let p = lay("<ul><li>alpha beta gamma delta epsilon</ul>", 20);
    assert_eq!(
        text(&p),
        ["   * alpha beta", "     gamma delta", "     epsilon"]
    );
}

#[test]
fn preformatted() {
    let p = lay("<pre>\n  a  b\n\tc\n</pre>after", 40);
    assert_eq!(text(&p), ["  a  b", "        c", "", "after"]);
}

#[test]
fn links_numbered_with_positions() {
    let p = lay(
        "<p>See <a href='/a'>the docs</a> and <a href=\"b.html\"><img src=x alt=Logo></a>.\
         <a href='javascript:void(0)'>js</a> <a name=top></a></p>",
        40,
    );
    assert_eq!(text(&p), ["See [1]the docs and [2][Logo].[3]js"]);
    assert_eq!(p.links.len(), 3);
    assert_eq!(p.links[0].href, "/a");
    assert_eq!(p.links[0].text, "the docs");
    assert_eq!(p.links[0].pos, Some((0, 4)));
    assert_eq!(p.anchors, [(String::from("top"), 0)]);
    let spans: Vec<_> = p.lines[0]
        .spans
        .iter()
        .filter(|s| s.target == Target::Link(0))
        .collect();
    assert_eq!(
        spans.iter().map(|s| s.text.as_str()).collect::<String>(),
        "[1]the docs"
    );
}

#[test]
fn hidden_and_skipped_content() {
    let p = lay(
        "<head><style>p{}</style><script>alert(1)</script></head>\
         <p>visible</p><div style='display: none'>gone<input type=hidden name=t value=1></div>\
         <p hidden>also gone</p><noscript>Enable JS</noscript>",
        40,
    );
    assert_eq!(text(&p), ["visible", "", "Enable JS"]);
    assert_eq!(p.fields.len(), 1);
    assert_eq!(p.fields[0].kind, FieldKind::Hidden);
}

#[test]
fn forms_and_fields() {
    let p = lay(
        r#"<form action="/login" method=POST id=f1>
           <label for=u>User</label> <input id=u name=user size=8 value=bob>
           <input type=password name=pw size=6 value=abc>
           <input type=hidden name=token value=xyz>
           <input type=checkbox name=remember checked> <input type=radio name=r value=a> <input type=radio name=r value=b checked>
           <select name=lang><option value=en>English<option value=de selected>Deutsch</select>
           <textarea name=msg cols=10>hello
world</textarea>
           <input type=submit value=Login> <button name=b value=v>Go <b>now</b></button>
           </form><input form=f1 name=outside>"#,
        100,
    );
    let t = text(&p).join("\n");
    assert!(
        t.contains("User [bob_____] [***___] [X] ( ) (*) [Deutsch v] [hello…____]"),
        "{t}"
    );
    assert!(t.contains("[ Login ] [ Go now ]"), "{t}");
    assert_eq!(p.forms.len(), 1);
    assert_eq!(p.forms[0].action, "/login");
    assert_eq!(p.forms[0].method, "post");
    let names: Vec<&str> = p.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "user", "pw", "token", "remember", "r", "r", "lang", "msg", "", "b", "outside"
        ]
    );
    assert!(p.fields.iter().all(|f| f.form == Some(0)));
    assert_eq!(p.fields[0].label, "User");
    assert_eq!(p.fields[3].value, "on");
    assert_eq!(p.fields[6].options[p.fields[6].selected].value, "de");
    assert_eq!(p.fields[7].value, "hello\nworld");
    assert_eq!(p.fields[9].label, "Go now");
    assert!(p.fields[0].pos.is_some() && p.fields[2].pos.is_none());
    let mut f = p.fields[0].clone();
    f.value = String::from("a-very-long-user");
    assert_eq!(field_text(&f), "[ong-user]");
}

#[test]
fn tables_in_columns() {
    let p = lay(
        "<table><tr><th>Name<th>Size</tr><tr><td>kernel<td>4 MiB<tr><td colspan=2>total</table>",
        40,
    );
    assert_eq!(text(&p), [" Name   Size", "kernel 4 MiB", "total"]);
    assert!(
        p.lines[0]
            .spans
            .iter()
            .any(|s| s.text.contains("Name") && s.style.bold)
    );
    let p = lay("<table><tr><td>x<td><a href=/l>link</a></table>", 40);
    assert_eq!(text(&p), ["x [1]link"]);
    assert_eq!(p.links[0].pos, Some((0, 2)));
    // Narrow: cells wrap within the width.
    let p = lay(
        "<table><tr><td>alpha beta gamma delta epsilon<td>one two three four five six</table>",
        30,
    );
    let t = text(&p);
    assert!(t.len() > 1, "{t:?}");
    assert!(t.iter().all(|l| text_width(l) <= 30), "{t:?}");
    assert!(t[0].starts_with("alpha"));
}

#[test]
fn dump_lists_references() {
    let p = lay("<p>Go <a href=http://x/>there</a></p>", 40);
    assert_eq!(dump(&p), "Go [1]there\n\nReferences\n\n   1. http://x/\n");
}

#[test]
fn widths() {
    assert_eq!(text_width("abc"), 3);
    assert_eq!(text_width("日本"), 4);
    assert_eq!(truncate("日本語", 5), "日本");
}

// --- CSS ---

#[test]
fn css_display_and_media() {
    let p = lay(
        "<style>.x{display:none} span.b{display:block} @media (max-width: 400px) {.w{display:none}} @media (min-width: 401px) {.n{display:none}}</style>\
         <p>a<span class=x>HIDDEN</span><span class=b>block</span>c</p><p class=w>wide</p><p class=n>narrow</p>",
        80,
    );
    assert_eq!(text(&p), ["a", "block", "c", "", "wide"]);
    let p = lay(
        "<style>@media (max-width: 400px) {.w{display:none}} @media (min-width: 401px) {.n{display:none}}</style><p class=w>wide</p><p class=n>narrow</p>",
        40,
    );
    assert_eq!(text(&p), ["narrow"]);
}

#[test]
fn css_flexbox() {
    let css = "<style>.f{display:flex} .f > *{flex:1} .c{display:flex;justify-content:center} .sb{display:flex;justify-content:space-between} .col{display:flex;flex-direction:column}</style>";
    let p = lay(
        &alloc::format!("{css}<div class=f><div>aaaa</div><div>bbbb</div></div>"),
        20,
    );
    assert_eq!(text(&p), ["aaaa      bbbb"]);
    let p = lay(
        &alloc::format!("{css}<div class=c><span>mid</span></div>"),
        21,
    );
    assert_eq!(text(&p), ["         mid"]);
    let p = lay(
        &alloc::format!("{css}<div class=sb><span>L</span><span>R</span></div>"),
        20,
    );
    assert_eq!(text(&p), ["L                  R"]);
    let p = lay(
        &alloc::format!("{css}<div class=col><span>one</span><span>two</span></div>"),
        20,
    );
    assert_eq!(text(&p), ["one", "two"]);
    // Wrapping flex items.
    let p = lay(
        "<div style='display:flex;flex-wrap:wrap;gap:0 1ch'><span>alpha</span><span>beta</span><span>gamma</span></div>",
        12,
    );
    assert_eq!(text(&p), ["alpha beta", "gamma"]);
}

#[test]
fn css_grid() {
    let p = lay(
        "<div style='display:grid;grid-template-columns:10ch 1fr;grid-template-areas:\"h h\" \"n m\"'>\
         <div style='grid-area:h'>header</div><div style='grid-area:n'>nav</div><div style='grid-area:m'>main text</div></div>",
        30,
    );
    assert_eq!(text(&p), ["header", "nav       main text"]);
    let p = lay(
        "<div style='display:grid;grid-template-columns:repeat(3,1fr)'><div>a</div><div>b</div><div>c</div><div>d</div></div>",
        30,
    );
    assert_eq!(text(&p), ["a         b         c", "d"]);
}

#[test]
fn css_text_properties() {
    let p = lay(
        "<p style='text-transform:uppercase'>loud</p><p style='text-align:center;width:20ch'>mid</p><p style='text-align:right;width:10ch'>r</p>",
        40,
    );
    assert_eq!(text(&p), ["LOUD", "", "         mid", "", "         r"]);
    let p = lay(
        "<p style='white-space:nowrap'>one two three four five six</p>",
        10,
    );
    assert_eq!(text(&p), ["one two th"]);
}

#[test]
fn css_generated_content_and_counters() {
    let p = lay(
        "<style>body{counter-reset:s} h2{counter-increment:s} h2::before{content:counter(s) \". \"} q{quotes:'<' '>'} .n::after{content:' [' attr(data-x) ']'}</style>\
         <h2>Intro</h2><h2>Next</h2><p><q>quoted</q> <span class=n data-x=7>note</span></p>",
        40,
    );
    assert_eq!(
        text(&p),
        ["1. Intro", "", "2. Next", "", "<quoted> note [7]"]
    );
}

#[test]
fn css_positioning_and_floats() {
    let p = lay(
        "<div style='position:relative;height:32px'><span style='position:absolute;right:0;top:16px'>R</span>left</div>",
        20,
    );
    assert_eq!(text(&p), ["left", "                   R"]);
    let p = lay(
        "<img src=a alt=IMG style='float:right'>text flows left of the float",
        20,
    );
    assert_eq!(text(&p)[0], "text flows left[IMG]");
}

#[test]
fn css_colors_reach_spans() {
    let p = lay(
        "<p style='color:#ff8800'>orange</p><p style='background:#003366;color:white'>panel</p>",
        20,
    );
    let orange = &p.lines[0].spans[0];
    assert_eq!(orange.style.fg, Some((255, 136, 0)));
    let panel = &p.lines[2].spans[0];
    assert_eq!(panel.style.bg, Some((0, 51, 102)));
    assert_eq!(panel.style.fg, Some((255, 255, 255)));
    // Dark default text on the terminal's own background: no color.
    let p = lay("<p>plain</p>", 20);
    assert_eq!(p.lines[0].spans[0].style.fg, None);
}

#[test]
fn author_sheets_and_imports() {
    let doc =
        html::parse("<link rel=stylesheet href=a.css><p class=x>styled</p><p class=y>gone</p>");
    let opts = Options {
        width: 40,
        ..Options::default()
    };
    let sources = sheet_sources(&doc, &cell_device(&opts));
    assert!(matches!(&sources[0], SheetSource::Link(h) if h == "a.css"));
    let author = alloc::vec![String::from(
        "@import 'b.css'; .x { text-transform: uppercase }"
    )];
    let page = render_with(&doc, &opts, &author, &mut |u| {
        (u == "b.css").then(|| String::from(".y{display:none}"))
    });
    assert_eq!(text(&page), ["STYLED"]);
    assert_eq!(
        imports_of("@import url(q.css) screen; @import 'p.css' print; a{}"),
        ["q.css"]
    );
}
