//! The DOM on the wire: the browser sends the parsed document once;
//! `jsd` sends back batches of mutations that the browser applies to its
//! own copy (node ids are indices into `html::Document::nodes`).

use crate::json::Json;
use alloc::collections::BTreeMap;
use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;
use html::{Document, NodeId, NodeKind};

/// `[id, "tag", [[name, value], ...], [children...]]` for elements,
/// `[id, "#text", "data"]` for text.
pub fn serialize_node(doc: &Document, id: NodeId) -> Json {
    match &doc.nodes[id].kind {
        NodeKind::Text(t) => Json::Arr(vec![Json::from(id), Json::from("#text"), Json::from(t.as_str())]),
        NodeKind::Element { tag, attrs } => {
            let a = attrs.iter().map(|(k, v)| Json::Arr(vec![Json::from(k.as_str()), Json::from(v.as_str())])).collect();
            let kids = doc.nodes[id].children.iter().map(|&c| serialize_node(doc, c)).collect();
            Json::Arr(vec![Json::from(id), Json::from(tag.as_str()), Json::Arr(a), Json::Arr(kids)])
        }
        NodeKind::Document => {
            let kids = doc.nodes[id].children.iter().map(|&c| serialize_node(doc, c)).collect();
            Json::Arr(vec![Json::from(id), Json::from("#document"), Json::Arr(Vec::new()), Json::Arr(kids)])
        }
    }
}

/// The whole document, with the next free node id.
pub fn serialize_document(doc: &Document) -> Json {
    Json::obj([("tree", serialize_node(doc, 0)), ("next", Json::from(doc.nodes.len()))])
}

/// Apply mutation `ops` from `jsd`:
/// `{"op":"create","id":n,"tag":"div"}` / `{"op":"create","id":n,"text":"..."}`,
/// `{"op":"insert","parent":p,"child":c,"before":b|null}`,
/// `{"op":"remove","child":c}`, `{"op":"attr","id":n,"name":k,"value":v|null}`,
/// `{"op":"data","id":n,"data":"..."}`. Unknown ops are returned for the
/// caller (form values, focus, ...).
pub fn apply_ops<'j>(doc: &mut Document, ops: &'j [Json]) -> Vec<&'j Json> {
    let mut other = Vec::new();
    for op in ops {
        match op.str("op") {
            Some("create") => {
                let Some(id) = op.int("id") else { continue };
                if id <= 0 || id > 50_000_000 {
                    continue;
                }
                let kind = match (op.str("tag"), op.str("text")) {
                    (Some(t), _) => NodeKind::Element { tag: t.to_ascii_lowercase(), attrs: Vec::new() },
                    (None, Some(t)) => NodeKind::Text(t.to_string()),
                    _ => continue,
                };
                doc.create_with_id(id as NodeId, kind);
            }
            Some("insert") => {
                if let (Some(p), Some(c)) = (op.int("parent"), op.int("child")) {
                    let before = op.int("before").map(|b| b as NodeId);
                    doc.insert_before(p as NodeId, c as NodeId, before);
                }
            }
            Some("remove") => {
                if let Some(c) = op.int("child")
                    && (c as usize) < doc.nodes.len()
                {
                    doc.detach(c as NodeId);
                }
            }
            Some("attr") => {
                if let (Some(id), Some(name)) = (op.int("id"), op.str("name")) {
                    doc.set_attr(id as NodeId, &name.to_ascii_lowercase(), op.str("value"));
                }
            }
            Some("data") => {
                if let (Some(id), Some(d)) = (op.int("id"), op.str("data")) {
                    doc.set_text(id as NodeId, d);
                }
            }
            _ => other.push(op),
        }
    }
    other
}

/// A fragment parsed for `innerHTML`: the serialized top-level nodes with
/// fresh ids starting at `first_id`.
pub fn fragment(src: &str, first_id: usize) -> Json {
    let frag = Document::parse_fragment(src);
    let Some(body) = frag.find("body") else { return Json::Arr(Vec::new()) };
    renumbered(&frag, body, first_id)
}

/// A whole document parsed for `DOMParser` / `document.open()`: the
/// serialized children of the document node, with fresh ids.
pub fn document_tree(src: &str, first_id: usize) -> Json {
    renumbered(&html::parse(src), 0, first_id)
}

/// The children of `root`, numbered in document order from `first_id`.
fn renumbered(doc: &Document, root: NodeId, first_id: usize) -> Json {
    let mut map: BTreeMap<NodeId, usize> = BTreeMap::new();
    let mut next = first_id;
    for d in doc.descendants(root) {
        map.insert(d, next);
        next += 1;
    }
    fn ser(doc: &Document, id: NodeId, map: &BTreeMap<NodeId, usize>) -> Json {
        let nid = Json::from(map[&id]);
        match &doc.nodes[id].kind {
            NodeKind::Text(t) => Json::Arr(vec![nid, Json::from("#text"), Json::from(t.as_str())]),
            NodeKind::Element { tag, attrs } => {
                let a = attrs.iter().map(|(k, v)| Json::Arr(vec![Json::from(k.as_str()), Json::from(v.as_str())])).collect();
                let kids = doc.nodes[id].children.iter().map(|&c| ser(doc, c, map)).collect();
                Json::Arr(vec![nid, Json::from(tag.as_str()), Json::Arr(a), Json::Arr(kids)])
            }
            NodeKind::Document => Json::Null,
        }
    }
    Json::Arr(doc.nodes[root].children.iter().map(|&c| ser(doc, c, &map)).collect())
}

/// Create the nodes of a serialized subtree in `doc` (detached) — used
/// when the browser applies a fragment it produced itself.
pub fn materialize(doc: &mut Document, n: &Json) -> Option<NodeId> {
    let a = n.as_arr()?;
    let id = a.first()?.as_i64()? as NodeId;
    let tag = a.get(1)?.as_str()?;
    if tag == "#text" {
        doc.create_with_id(id, NodeKind::Text(a.get(2)?.as_str()?.to_string()));
        return Some(id);
    }
    let attrs = a.get(2)?.as_arr()?.iter().filter_map(|p| Some((p.as_arr()?.first()?.as_str()?.to_string(), p.as_arr()?.get(1)?.as_str()?.to_string()))).collect();
    doc.create_with_id(id, NodeKind::Element { tag: tag.to_string(), attrs });
    for c in a.get(3)?.as_arr()? {
        if let Some(cid) = materialize(doc, c) {
            doc.insert_before(id, cid, None);
        }
    }
    Some(id)
}
