extern crate std;
use super::*;
use alloc::format;

/// Compact tree dump: `tag(children)` and quoted text.
fn dump(d: &Document, id: NodeId) -> String {
    let mut s = String::new();
    for &c in &d.nodes[id].children {
        if !s.is_empty() {
            s.push(' ');
        }
        match &d.nodes[c].kind {
            NodeKind::Text(t) => s.push_str(&format!("{:?}", t)),
            NodeKind::Element { tag, .. } => {
                let inner = dump(d, c);
                if inner.is_empty() {
                    s.push_str(tag);
                } else {
                    s.push_str(&format!("{}({})", tag, inner));
                }
            }
            NodeKind::Document => {}
        }
    }
    s
}

fn tree(src: &str) -> String {
    let d = parse(src);
    dump(&d, 0)
}

#[test]
fn entities() {
    assert_eq!(decode_entities("a &amp; b &lt;c&gt; &quot;q&quot;"), "a & b <c> \"q\"");
    assert_eq!(decode_entities("&eacute;&#233;&#xE9;&#XE9"), "éééé");
    assert_eq!(decode_entities("&copy 2024 &nbsp;x"), "© 2024 \u{a0}x");
    assert_eq!(decode_entities("AT&T &unknown; &;"), "AT&T &unknown; &;");
    assert_eq!(decode_entities("&#128; &#0; &#x110000;"), "€ \u{FFFD} \u{FFFD}");
    assert_eq!(decode_entities("&mdash;&hellip;&rsquo;&euro;"), "—…’€");
    // Named without ';' is only accepted for legacy names.
    assert_eq!(decode_entities("&eacutex"), "&eacutex");
}

#[test]
fn tokenizer_attributes() {
    let t = tokenize(r#"<A HREF="x.html" title='a "b"' data-x=1 checked disabled=disabled/><br/>"#);
    assert_eq!(
        t[0],
        Token::Start {
            name: "a".into(),
            attrs: vec![
                ("href".into(), "x.html".into()),
                ("title".into(), "a \"b\"".into()),
                ("data-x".into(), "1".into()),
                ("checked".into(), "".into()),
                ("disabled".into(), "disabled/".into()),
            ],
            self_closing: false,
        }
    );
    assert_eq!(t[1], Token::Start { name: "br".into(), attrs: vec![], self_closing: true });
    let t = tokenize("<input value=a&amp;b name = n >");
    assert_eq!(
        t[0],
        Token::Start {
            name: "input".into(),
            attrs: vec![("value".into(), "a&b".into()), ("name".into(), "n".into())],
            self_closing: false
        }
    );
    // Stray '<' is text; duplicate attributes keep the first.
    let t = tokenize("1 < 2 <p id=a id=b>");
    assert_eq!(t[0], Token::Text("1 < 2 ".into()));
    assert_eq!(t[1], Token::Start { name: "p".into(), attrs: vec![("id".into(), "a".into())], self_closing: false });
}

#[test]
fn raw_text_and_comments() {
    let t = tokenize("<script>if (a<b && c>d) x();</SCRIPT>after<!-- <b>hidden</b> --><!DOCTYPE html><![CDATA[x]]>");
    assert_eq!(t[1], Token::Text("if (a<b && c>d) x();".into()));
    assert_eq!(t[2], Token::End("script".into()));
    assert_eq!(t[3], Token::Text("after".into()));
    assert_eq!(t[4], Token::Comment(" <b>hidden</b> ".into()));
    assert_eq!(t[5], Token::Doctype("html".into()));
    let t = tokenize("<textarea>a &lt; b <b>no tags</b></textarea>");
    assert_eq!(t[1], Token::Text("a < b <b>no tags</b>".into()));
    // Unterminated comment runs to the end.
    assert_eq!(tokenize("x<!-- oops").len(), 2);
}

#[test]
fn implied_end_tags() {
    assert_eq!(tree("<p>one<p>two<div>three</div>"), r#"p("one") p("two") div("three")"#);
    assert_eq!(tree("<ul><li>a<li>b<ul><li>c</ul><li>d</ul>"), r#"ul(li("a") li("b" ul(li("c"))) li("d"))"#);
    assert_eq!(tree("<dl><dt>t<dd>d<dt>t2</dl>"), r#"dl(dt("t") dd("d") dt("t2"))"#);
    assert_eq!(
        tree("<table><tr><td>1<td>2<tr><th>h</table>after"),
        r#"table(tr(td("1") td("2")) tr(th("h"))) "after""#
    );
    assert_eq!(
        tree("<select><option>a<option selected>b</select>"),
        r#"select(option("a") option("b"))"#
    );
    assert_eq!(tree("<h1>a<h2>b</h2>"), r#"h1("a") h2("b")"#);
}

#[test]
fn misnested_and_stray_tags() {
    assert_eq!(tree("<b><i>x</b>y</i>"), r#"b(i("x")) "y""#);
    assert_eq!(tree("</div>text</span>"), r#""text""#);
    assert_eq!(tree("a</p>b"), r#""a" p "b""#);
    assert_eq!(tree("<a href=1>one<a href=2>two</a>"), r#"a("one") a("two")"#);
    assert_eq!(tree("<p>x<br>y</br>z"), r#"p("x" br "y" br "z")"#);
    assert_eq!(tree("<img src=a alt=b><hr/>t"), r#"img hr "t""#);
    // <td> content does not leak out of the table on a stray end tag.
    assert_eq!(tree("<table><tr><td><b>x</td><td>y</table>"), r#"table(tr(td(b("x")) td("y")))"#);
}

#[test]
fn metadata() {
    let d = parse(
        "<html><head><title>  My\n  Page </title><base href=\"http://h/dir/\">\
         <meta http-equiv=\"Refresh\" content=\"5; URL='/next'\">\
         <meta charset=ISO-8859-1></head><body>Hi</body></html>",
    );
    assert_eq!(d.title, "My Page");
    assert_eq!(d.base.as_deref(), Some("http://h/dir/"));
    assert_eq!(d.refresh, Some((5, "/next".into())));
    assert_eq!(d.charset.as_deref(), Some("iso-8859-1"));
    assert_eq!(dump(&d, 0), r#"base meta meta "Hi""#);
    assert_eq!(parse_refresh("0;url=http://a/"), Some((0, "http://a/".into())));
    assert_eq!(parse_refresh("3"), Some((3, "".into())));
    assert_eq!(parse_refresh(""), None);
    let d = parse("<meta http-equiv=content-type content='text/html; charset=windows-1252'>");
    assert_eq!(d.charset.as_deref(), Some("windows-1252"));
}

#[test]
fn charset_prescan() {
    assert_eq!(sniff_charset(b"<html><META CHARSET=\"Shift_JIS\">").as_deref(), Some("shift_jis"));
    assert_eq!(
        sniff_charset(b"<meta http-equiv=Content-Type content=\"text/html; charset=iso-8859-15\">").as_deref(),
        Some("iso-8859-15")
    );
    assert_eq!(sniff_charset(b"<p>no meta</p>"), None);
}

#[test]
fn queries() {
    let d = parse("<form action=/x><input name=a value=1><label>L <b>bold</b></label></form>");
    let f = d.find("form").unwrap();
    assert_eq!(d.attr(f, "action"), Some("/x"));
    let input = d.find("input").unwrap();
    assert_eq!(d.attr(input, "value"), Some("1"));
    assert_eq!(d.text_content(d.find("label").unwrap()), "L bold");
    assert_eq!(d.descendants(f).len(), 5);
}
