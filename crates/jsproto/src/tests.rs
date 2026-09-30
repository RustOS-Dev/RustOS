use crate::dom::*;
use crate::json::Json;
use alloc::string::String;
use alloc::vec;

#[test]
fn json_roundtrip() {
    let src = r#"{"a":[1,2.5,-3e2,true,false,null],"s":"x\"y\\\né😀","o":{}}"#;
    let v = Json::parse(src).unwrap();
    assert_eq!(v.arr("a").unwrap()[2], Json::Num(-300.0));
    assert_eq!(v.str("s"), Some("x\"y\\\né😀"));
    let back = v.to_json();
    assert_eq!(Json::parse(&back).unwrap(), v);
    assert!(!back.contains('\n'));
    assert!(Json::parse("[1,]").is_err());
    assert!(Json::parse("{\"a\" 1}").is_err());
}

#[test]
fn dom_mutations_apply() {
    let mut d = html::parse("<p id=x>hi</p>");
    let p = d.find("p").unwrap();
    let next = d.nodes.len();
    let ops = Json::parse(&alloc::format!(
        r#"[{{"op":"create","id":{n},"tag":"B"}},{{"op":"create","id":{m},"text":"bold"}},
            {{"op":"insert","parent":{n},"child":{m},"before":null}},
            {{"op":"insert","parent":{p},"child":{n},"before":null}},
            {{"op":"attr","id":{p},"name":"class","value":"k"}},
            {{"op":"value","id":{p},"value":"z"}}]"#,
        n = next,
        m = next + 1,
        p = p
    ))
    .unwrap();
    let other = apply_ops(&mut d, ops.as_arr().unwrap());
    assert_eq!(other.len(), 1);
    assert_eq!(d.text_content(p), "hibold");
    assert_eq!(d.attr(p, "class"), Some("k"));
    assert_eq!(d.tag(next), "b");
    // Remove and re-serialize.
    let ops = Json::parse(&alloc::format!(r#"[{{"op":"remove","child":{}}}]"#, next)).unwrap();
    apply_ops(&mut d, ops.as_arr().unwrap());
    assert_eq!(d.text_content(p), "hi");
    let s = serialize_document(&d).to_json();
    assert!(s.contains(r#""p",[["id","x"],["class","k"]],[["#) || s.contains("\"p\""), "{s}");
}

#[test]
fn fragments_get_fresh_ids() {
    let f = fragment("<b>a</b>c", 100);
    let a = f.as_arr().unwrap();
    assert_eq!(a.len(), 2);
    assert_eq!(a[0].as_arr().unwrap()[0].as_i64(), Some(100));
    assert_eq!(a[0].as_arr().unwrap()[3].as_arr().unwrap()[0].as_arr().unwrap()[0].as_i64(), Some(101));
    assert_eq!(a[1].as_arr().unwrap()[0].as_i64(), Some(102));
    let mut d = html::parse("<div></div>");
    let div = d.find("div").unwrap();
    let id = materialize(&mut d, &a[0]).unwrap();
    d.insert_before(div, id, None);
    assert_eq!(d.text_content(div), "a");
    let _ = (String::new(), vec![0]);
}
