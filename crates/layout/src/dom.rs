//! The document as the CSS engine sees it: element navigation (skipping
//! text nodes) through precomputed tables, and dynamic state.

use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use css::State;
use html::{Document, NodeId, NodeKind};

/// Dynamic element state supplied by the browser.
#[derive(Debug, Clone, Default)]
pub struct DomState {
    pub hover: Option<NodeId>,
    pub active: Option<NodeId>,
    pub focus: Option<NodeId>,
    /// Link targets already visited (resolved hrefs are compared by the
    /// browser; here: the elements).
    pub visited: BTreeSet<NodeId>,
    /// Checkbox/radio/option checkedness overriding the markup.
    pub checked: Vec<(NodeId, bool)>,
    /// Current values of text controls (for `:placeholder-shown`).
    pub values: Vec<(NodeId, String)>,
    /// The `:target` element (URL fragment).
    pub target: Option<NodeId>,
    /// Selected option of `<select>` controls, overriding the markup.
    pub selected: Vec<(NodeId, usize)>,
    /// Elements with script click handlers (made selectable like links).
    pub clickable: BTreeSet<NodeId>,
}

/// Navigation tables over a document.
pub struct Nav<'a> {
    pub doc: &'a Document,
    pub state: &'a DomState,
    parent: Vec<Option<NodeId>>,
    prev: Vec<Option<NodeId>>,
    next: Vec<Option<NodeId>>,
    first: Vec<Option<NodeId>>,
    /// Element has a text child with non-whitespace... or any child
    /// (`:empty` is false if there is any element or text).
    nonempty: Vec<bool>,
    /// The document element (`<html>`).
    pub root: Option<NodeId>,
}

impl<'a> Nav<'a> {
    pub fn new(doc: &'a Document, state: &'a DomState) -> Nav<'a> {
        let n = doc.nodes.len();
        let mut nav = Nav {
            doc,
            state,
            parent: vec![None; n],
            prev: vec![None; n],
            next: vec![None; n],
            first: vec![None; n],
            nonempty: vec![false; n],
            root: None,
        };
        for id in 0..n {
            let mut last: Option<NodeId> = None;
            for &c in &doc.nodes[id].children {
                match &doc.nodes[c].kind {
                    NodeKind::Element { .. } => {
                        if id != 0 || nav.root.is_none() {
                            nav.parent[c] = if id == 0 { None } else { Some(id) };
                        }
                        if id == 0 && nav.root.is_none() {
                            nav.root = Some(c);
                        }
                        if let Some(l) = last {
                            nav.next[l] = Some(c);
                            nav.prev[c] = Some(l);
                        } else {
                            nav.first[id] = Some(c);
                        }
                        last = Some(c);
                        nav.nonempty[id] = true;
                    }
                    NodeKind::Text(t) => {
                        if !t.is_empty() {
                            nav.nonempty[id] = true;
                        }
                    }
                    NodeKind::Document => {}
                }
            }
        }
        nav
    }

    pub fn el(&self, id: NodeId) -> El<'_, 'a> {
        El { nav: self, id }
    }
}

#[derive(Clone, Copy)]
pub struct El<'n, 'a> {
    pub nav: &'n Nav<'a>,
    pub id: NodeId,
}

fn is_control(tag: &str) -> bool {
    matches!(
        tag,
        "input" | "select" | "textarea" | "button" | "fieldset" | "optgroup" | "option"
    )
}

impl El<'_, '_> {
    fn has(&self, a: &str) -> bool {
        self.nav.doc.attr(self.id, a).is_some()
    }

    fn input_type(&self) -> String {
        self.nav
            .doc
            .attr(self.id, "type")
            .unwrap_or("text")
            .to_ascii_lowercase()
    }

    fn checked(&self) -> bool {
        if let Some((_, c)) = self.nav.state.checked.iter().find(|(n, _)| *n == self.id) {
            return *c;
        }
        match self.nav.doc.tag(self.id) {
            "input" => self.has("checked"),
            "option" => self.has("selected"),
            _ => false,
        }
    }

    fn disabled(&self) -> bool {
        if !is_control(self.nav.doc.tag(self.id)) {
            return false;
        }
        if self.has("disabled") {
            return true;
        }
        // Inside a disabled fieldset.
        let mut p = self.nav.parent[self.id];
        while let Some(x) = p {
            if self.nav.doc.tag(x) == "fieldset" && self.nav.doc.attr(x, "disabled").is_some() {
                return true;
            }
            p = self.nav.parent[x];
        }
        false
    }
}

impl css::Element for El<'_, '_> {
    fn parent_element(&self) -> Option<Self> {
        self.nav.parent[self.id].map(|id| El { nav: self.nav, id })
    }
    fn prev_sibling_element(&self) -> Option<Self> {
        self.nav.prev[self.id].map(|id| El { nav: self.nav, id })
    }
    fn next_sibling_element(&self) -> Option<Self> {
        self.nav.next[self.id].map(|id| El { nav: self.nav, id })
    }
    fn first_child_element(&self) -> Option<Self> {
        self.nav.first[self.id].map(|id| El { nav: self.nav, id })
    }
    fn local_name(&self) -> &str {
        self.nav.doc.tag(self.id)
    }
    fn attr(&self, name: &str) -> Option<&str> {
        self.nav.doc.attr(self.id, name)
    }
    fn is_root(&self) -> bool {
        self.nav.root == Some(self.id)
    }
    fn is_empty(&self) -> bool {
        !self.nav.nonempty[self.id]
    }
    fn same(&self, o: &Self) -> bool {
        self.id == o.id
    }
    fn state(&self, s: State) -> bool {
        let doc = self.nav.doc;
        let tag = doc.tag(self.id);
        let st = self.nav.state;
        let is_link = matches!(tag, "a" | "area") && self.has("href");
        match s {
            State::AnyLink => is_link,
            State::Link => is_link && !st.visited.contains(&self.id),
            State::Visited => is_link && st.visited.contains(&self.id),
            State::Hover
            | State::Active
            | State::Focus
            | State::FocusWithin
            | State::FocusVisible => {
                let who = match s {
                    State::Hover => st.hover,
                    State::Active => st.active,
                    _ => st.focus,
                };
                let Some(w) = who else { return false };
                if matches!(s, State::Focus | State::FocusVisible) {
                    return w == self.id;
                }
                // Hover/active/focus-within apply to ancestors too.
                let mut p = Some(w);
                while let Some(x) = p {
                    if x == self.id {
                        return true;
                    }
                    p = self.nav.parent[x];
                }
                false
            }
            State::Target => st.target == Some(self.id),
            State::Checked => self.checked() && matches!(tag, "input" | "option"),
            State::Indeterminate => false,
            State::Disabled => self.disabled(),
            State::Enabled => is_control(tag) && !self.disabled(),
            State::Required => {
                self.has("required") && matches!(tag, "input" | "select" | "textarea")
            }
            State::Optional => {
                !self.has("required") && matches!(tag, "input" | "select" | "textarea")
            }
            State::ReadOnly => {
                !(matches!(tag, "input" | "textarea") && !self.has("readonly") && !self.disabled())
                    && !self.has("contenteditable")
            }
            State::ReadWrite => {
                (matches!(tag, "input" | "textarea") && !self.has("readonly") && !self.disabled())
                    || self.has("contenteditable")
            }
            State::PlaceholderShown => {
                matches!(tag, "input" | "textarea")
                    && self.has("placeholder")
                    && st.values.iter().find(|(n, _)| *n == self.id).map_or_else(
                        || doc.attr(self.id, "value").unwrap_or("").is_empty(),
                        |(_, v)| v.is_empty(),
                    )
            }
            State::Default => {
                (tag == "input" && self.has("checked"))
                    || (tag == "option" && self.has("selected"))
                    || (matches!(tag, "button") && self.input_type() == "submit")
            }
            State::Defined => true,
            State::Valid => {
                !self.has("required") || !doc.attr(self.id, "value").unwrap_or("").is_empty()
            }
            State::Invalid => {
                self.has("required")
                    && doc.attr(self.id, "value").unwrap_or("").is_empty()
                    && matches!(tag, "input" | "textarea" | "select")
            }
            State::Open => matches!(tag, "details" | "dialog") && self.has("open"),
            State::Scope => self.is_root_el(),
        }
    }
}

impl El<'_, '_> {
    fn is_root_el(&self) -> bool {
        self.nav.root == Some(self.id)
    }
}
