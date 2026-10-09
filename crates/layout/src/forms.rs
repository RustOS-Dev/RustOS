//! Links, forms and form fields collected from the DOM before layout
//! (hidden and `display: none` controls still take part in submission).

use crate::page::{Field, FieldKind, Form, Link, SelectOption, text_width};
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use html::{Document, NodeId};

#[derive(Default)]
pub struct Controls {
    pub links: Vec<Link>,
    pub fields: Vec<Field>,
    pub forms: Vec<Form>,
    pub link_of: BTreeMap<NodeId, usize>,
    pub field_of: BTreeMap<NodeId, usize>,
}

fn new_field(
    doc: &Document,
    id: NodeId,
    kind: FieldKind,
    ty: &str,
    form: Option<usize>,
    forms: &[Form],
) -> Field {
    let a = |n: &str| doc.attr(id, n).map(String::from);
    Field {
        kind,
        input_type: ty.to_string(),
        name: a("name").unwrap_or_default(),
        value: a("value").unwrap_or_default(),
        checked: a("checked").is_some(),
        options: Vec::new(),
        selected: 0,
        form: match a("form") {
            Some(f) => forms
                .iter()
                .position(|x| x.id.as_deref() == Some(f.as_str()))
                .or(form),
            None => form,
        },
        size: 20,
        disabled: a("disabled").is_some(),
        readonly: a("readonly").is_some(),
        id: a("id"),
        label: a("placeholder")
            .or_else(|| a("aria-label"))
            .unwrap_or_default(),
        formaction: a("formaction"),
        formmethod: a("formmethod").map(|m| m.to_ascii_lowercase()),
        formenctype: a("formenctype").map(|m| m.to_ascii_lowercase()),
        pos: None,
        node: id,
    }
}

/// Walk the document in order, collecting links and controls; elements in
/// `clickable` (script event handlers) become links with an empty href.
pub fn collect(doc: &Document, clickable: &BTreeSet<NodeId>) -> Controls {
    let mut c = Controls::default();
    let mut labels: Vec<(String, String)> = Vec::new();
    // Forms first, so `form=` attributes can refer to later forms.
    for d in doc.descendants(0) {
        if doc.tag(d) == "form" {
            c.forms.push(Form {
                action: doc.attr(d, "action").unwrap_or("").trim().to_string(),
                method: doc
                    .attr(d, "method")
                    .unwrap_or("get")
                    .trim()
                    .to_ascii_lowercase(),
                enctype: doc
                    .attr(d, "enctype")
                    .unwrap_or("application/x-www-form-urlencoded")
                    .trim()
                    .to_ascii_lowercase(),
                id: doc.attr(d, "id").map(String::from),
            });
        }
    }
    let mut form_nodes: Vec<NodeId> = Vec::new();
    walk(
        doc,
        0,
        &mut c,
        &mut labels,
        &mut form_nodes,
        None,
        clickable,
    );
    for (id, label) in labels {
        if let Some(f) = c
            .fields
            .iter_mut()
            .find(|f| f.id.as_deref() == Some(id.as_str()))
            && (f.label.is_empty()
                || matches!(
                    f.kind,
                    FieldKind::Text
                        | FieldKind::Password
                        | FieldKind::Checkbox
                        | FieldKind::Radio
                        | FieldKind::Select
                        | FieldKind::Textarea
                ))
        {
            f.label = label;
        }
    }
    c
}

fn walk(
    doc: &Document,
    id: NodeId,
    c: &mut Controls,
    labels: &mut Vec<(String, String)>,
    form_nodes: &mut Vec<NodeId>,
    form: Option<usize>,
    clickable: &BTreeSet<NodeId>,
) {
    for &ch in &doc.nodes[id].children {
        let tag = doc.tag(ch);
        let mut form = form;
        match tag {
            "form" => {
                form_nodes.push(ch);
                form = Some(form_nodes.len() - 1);
            }
            "a" | "area" => {
                if let Some(href) = doc.attr(ch, "href") {
                    c.link_of.insert(ch, c.links.len());
                    c.links.push(Link {
                        href: href.trim().to_string(),
                        node: ch,
                        text: html::collapse_ws(&doc.text_content(ch)),
                        pos: None,
                    });
                } else if clickable.contains(&ch) {
                    c.link_of.insert(ch, c.links.len());
                    c.links.push(Link {
                        href: String::new(),
                        node: ch,
                        text: html::collapse_ws(&doc.text_content(ch)),
                        pos: None,
                    });
                }
            }
            "label" => {
                if let Some(f) = doc.attr(ch, "for") {
                    labels.push((f.to_string(), html::collapse_ws(&doc.text_content(ch))));
                }
            }
            "input" => {
                let ty = doc
                    .attr(ch, "type")
                    .unwrap_or("text")
                    .trim()
                    .to_ascii_lowercase();
                let kind = match ty.as_str() {
                    "password" => FieldKind::Password,
                    "hidden" => FieldKind::Hidden,
                    "checkbox" => FieldKind::Checkbox,
                    "radio" => FieldKind::Radio,
                    "submit" => FieldKind::Submit,
                    "image" => FieldKind::Image,
                    "reset" => FieldKind::Reset,
                    "button" => FieldKind::Button,
                    "file" => FieldKind::File,
                    _ => FieldKind::Text,
                };
                let mut f = new_field(doc, ch, kind, &ty, form, &c.forms);
                match kind {
                    FieldKind::Text | FieldKind::Password => {
                        f.size = doc
                            .attr(ch, "size")
                            .and_then(|s| s.trim().parse().ok())
                            .unwrap_or(20usize)
                            .clamp(4, 200);
                    }
                    FieldKind::Checkbox | FieldKind::Radio => {
                        if f.value.is_empty() {
                            f.value = String::from("on");
                        }
                    }
                    FieldKind::Image => {
                        f.label = doc.attr(ch, "alt").unwrap_or("Submit").to_string()
                    }
                    FieldKind::Submit | FieldKind::Reset | FieldKind::Button => {
                        f.label = String::new()
                    }
                    _ => {}
                }
                c.field_of.insert(ch, c.fields.len());
                c.fields.push(f);
            }
            "button" => {
                let ty = doc
                    .attr(ch, "type")
                    .unwrap_or("submit")
                    .to_ascii_lowercase();
                let kind = match ty.as_str() {
                    "reset" => FieldKind::Reset,
                    "button" => FieldKind::Button,
                    _ => FieldKind::Submit,
                };
                let mut f = new_field(doc, ch, kind, &ty, form, &c.forms);
                let label = html::collapse_ws(&doc.text_content(ch));
                if !label.is_empty() {
                    f.label = label;
                }
                c.field_of.insert(ch, c.fields.len());
                c.fields.push(f);
                continue; // its content is the label
            }
            "select" => {
                let mut f = new_field(doc, ch, FieldKind::Select, "select", form, &c.forms);
                let mut sel = None;
                for d in doc.descendants(ch) {
                    if doc.tag(d) == "option" {
                        let label = doc
                            .attr(d, "label")
                            .map(String::from)
                            .unwrap_or_else(|| html::collapse_ws(&doc.text_content(d)));
                        let value = doc
                            .attr(d, "value")
                            .map(String::from)
                            .unwrap_or_else(|| label.clone());
                        if doc.attr(d, "selected").is_some() && sel.is_none() {
                            sel = Some(f.options.len());
                        }
                        f.options.push(SelectOption { value, label });
                    }
                }
                f.selected = sel.unwrap_or(0);
                f.size = f
                    .options
                    .iter()
                    .map(|o| text_width(&o.label))
                    .max()
                    .unwrap_or(1)
                    .clamp(1, 60);
                c.field_of.insert(ch, c.fields.len());
                c.fields.push(f);
                continue;
            }
            "textarea" => {
                let value = doc.text_content(ch);
                let value = value.strip_prefix('\n').unwrap_or(&value).to_string();
                let cols = doc
                    .attr(ch, "cols")
                    .and_then(|c| c.parse().ok())
                    .unwrap_or(40usize);
                let mut f = new_field(doc, ch, FieldKind::Textarea, "textarea", form, &c.forms);
                f.value = value;
                f.size = cols.clamp(8, 200);
                c.field_of.insert(ch, c.fields.len());
                c.fields.push(f);
                continue;
            }
            _ => {
                // A clickable element not inside another link.
                if clickable.contains(&ch) && !c.link_of.keys().any(|&l| doc.is_ancestor(l, ch)) {
                    c.link_of.insert(ch, c.links.len());
                    c.links.push(Link {
                        href: String::new(),
                        node: ch,
                        text: html::collapse_ws(&doc.text_content(ch)),
                        pos: None,
                    });
                }
            }
        }
        walk(doc, ch, c, labels, form_nodes, form, clickable);
    }
}
