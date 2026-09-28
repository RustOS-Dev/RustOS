extern crate std;
use super::*;
use alloc::vec::Vec;

fn lay(src: &str, width: usize) -> Page {
    render(&html::parse(src), &Options { width, number_links: true })
}

fn text(p: &Page) -> Vec<String> {
    p.lines.iter().map(|l| l.text()).collect()
}

#[test]
fn paragraphs_wrap_and_collapse() {
    let p = lay("<p>The   quick brown\n fox jumps over the lazy dog.</p><p>Second</p>", 20);
    assert_eq!(text(&p), ["The quick brown fox", "jumps over the lazy", "dog.", "", "Second"]);
    // Long words are split; inline elements do not break lines.
    // (Widths below 20 are raised to 20.)
    let p = lay("<div>a<b>b</b>c <i>supercalifragilisticexpialidocious</i></div>", 10);
    assert_eq!(text(&p), ["abc", "supercalifragilistic", "expialidocious"]);
    assert!(p.lines[0].spans.iter().any(|s| s.text == "b" && s.style.bold));
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
            "* one",
            "* two",
            "  3. three",
            "  4. four",
            "",
            "    quoted text here",
            "",
            "──────────────────────────────",
            "",
            "Term",
            "    Definition",
        ]
    );
    assert!(p.lines[0].spans[0].style.heading && p.lines[0].spans[0].style.underline);
    // Hanging indent for wrapped list items.
    let p = lay("<ul><li>alpha beta gamma delta epsilon</ul>", 20);
    assert_eq!(text(&p), ["* alpha beta gamma", "  delta epsilon"]);
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
    assert_eq!(text(&p), ["See [1]the docs and [2][Logo].js"]);
    assert_eq!(p.links.len(), 2);
    assert_eq!(p.links[0].href, "/a");
    assert_eq!(p.links[0].text, "the docs");
    assert_eq!(p.links[0].pos, Some((0, 4)));
    assert_eq!(p.links[1].text, "[Logo]");
    assert_eq!(p.anchors, [(String::from("top"), 0)]);
    // A link's words are one target, including the space between them.
    let spans: Vec<_> = p.lines[0].spans.iter().filter(|s| s.target == Target::Link(0)).collect();
    assert_eq!(spans.iter().map(|s| s.text.as_str()).collect::<String>(), "[1]the docs");
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
    assert!(t.contains("User [bob_____] [***___] [X] ( ) (*) [Deutsch v] [hello…____]"), "{t}");
    assert!(t.contains("[ Login ] [ Go now ]"), "{t}");
    assert_eq!(p.forms.len(), 1);
    assert_eq!(p.forms[0].action, "/login");
    assert_eq!(p.forms[0].method, "post");
    let names: Vec<&str> = p.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["user", "pw", "token", "remember", "r", "r", "lang", "msg", "", "b", "outside"]);
    assert!(p.fields.iter().all(|f| f.form == Some(0)));
    assert_eq!(p.fields[0].label, "User");
    assert_eq!(p.fields[3].value, "on");
    assert_eq!(p.fields[6].options[p.fields[6].selected].value, "de");
    assert_eq!(p.fields[7].value, "hello\nworld");
    assert_eq!(p.fields[9].label, "Go now");
    assert!(p.fields[0].pos.is_some() && p.fields[2].pos.is_none());
    // field_text follows state changes.
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
    assert_eq!(text(&p), ["Name    Size", "kernel  4 MiB", "total"]);
    assert!(p.lines[0].spans[0].style.bold);
    // Links inside cells keep correct positions.
    let p = lay("<table><tr><td>x<td><a href=/l>link</a></table>", 40);
    assert_eq!(text(&p), ["x  [1]link"]);
    assert_eq!(p.links[0].pos, Some((0, 3)));
    assert_eq!(p.links.len(), 1);
}

#[test]
fn narrow_tables_wrap_cells() {
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
