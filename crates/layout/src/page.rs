//! The rendered page handed to the browser: styled lines of text with
//! the links, form fields, forms and anchors found on the page (the same
//! model the browser has always used).

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use html::NodeId;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Style {
    pub bold: bool,
    pub underline: bool,
    pub italic: bool,
    pub heading: bool,
    /// Link numbers and other decoration.
    pub dim: bool,
    pub strike: bool,
    /// Foreground color from CSS (None: the terminal default).
    pub fg: Option<(u8, u8, u8)>,
    /// Background color from CSS.
    pub bg: Option<(u8, u8, u8)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    None,
    Link(usize),
    Field(usize),
    /// Zero-width marker for a fragment identifier.
    Anchor(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: Style,
    pub target: Target,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Line {
    pub spans: Vec<Span>,
}

impl Line {
    pub fn width(&self) -> usize {
        self.spans.iter().map(|s| text_width(&s.text)).sum()
    }
    pub fn text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Link {
    pub href: String,
    /// The element (an `<a>`/`<area>`, or any element scripts made
    /// clickable, with an empty href).
    pub node: NodeId,
    pub text: String,
    /// First screen position (line, column).
    pub pos: Option<(usize, usize)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    Password,
    Hidden,
    Checkbox,
    Radio,
    Select,
    Textarea,
    Submit,
    Image,
    Reset,
    Button,
    File,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectOption {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub kind: FieldKind,
    /// The `type` attribute as written (`email`, `search`, ...).
    pub input_type: String,
    pub name: String,
    pub value: String,
    pub checked: bool,
    pub options: Vec<SelectOption>,
    pub selected: usize,
    /// Owning form (index into [`Page::forms`]).
    pub form: Option<usize>,
    /// Display width of the value area.
    pub size: usize,
    pub disabled: bool,
    pub readonly: bool,
    pub id: Option<String>,
    /// Text of an associated `<label>`, or the placeholder.
    pub label: String,
    /// Submit-button overrides.
    pub formaction: Option<String>,
    pub formmethod: Option<String>,
    pub formenctype: Option<String>,
    pub pos: Option<(usize, usize)>,
    /// The control's element.
    pub node: NodeId,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Form {
    pub action: String,
    /// `get` or `post`.
    pub method: String,
    pub enctype: String,
    pub id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Page {
    pub title: String,
    pub lines: Vec<Line>,
    pub links: Vec<Link>,
    pub fields: Vec<Field>,
    pub forms: Vec<Form>,
    /// Fragment identifiers and their line.
    pub anchors: Vec<(String, usize)>,
    /// Layout boxes of each node (x, y, width, height in CSS px: a cell
    /// is 8x16), for scripts' geometry queries and hit testing.
    pub boxes: BTreeMap<NodeId, Vec<[f32; 4]>>,
}

/// Columns a character occupies (wide East Asian characters take two;
/// combining marks and zero-width characters none).
pub fn char_width(c: char) -> usize {
    let u = c as u32;
    if u < 0x300 {
        return 1;
    }
    if (0x300..0x370).contains(&u) || (0x200B..=0x200F).contains(&u) || u == 0xFEFF {
        return 0;
    }
    if (0x1100..=0x115F).contains(&u)
        || (0x2E80..=0xA4CF).contains(&u)
        || (0xAC00..=0xD7A3).contains(&u)
        || (0xF900..=0xFAFF).contains(&u)
        || (0xFE30..=0xFE4F).contains(&u)
        || (0xFF00..=0xFF60).contains(&u)
        || (0xFFE0..=0xFFE6).contains(&u)
        || (0x1F300..=0x1FAFF).contains(&u)
        || (0x20000..=0x3FFFD).contains(&u)
    {
        return 2;
    }
    1
}

pub fn text_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

/// Cut `s` to at most `w` columns.
pub fn truncate(s: &str, w: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let cw = char_width(c);
        if used + cw > w {
            break;
        }
        used += cw;
        out.push(c);
    }
    out
}

fn pad(s: &str, w: usize, fill: char) -> String {
    let mut out = truncate(s, w);
    for _ in text_width(&out)..w {
        out.push(fill);
    }
    out
}

/// Display text of a form field with its current state.
pub fn field_text(f: &Field) -> String {
    match f.kind {
        FieldKind::Hidden => String::new(),
        FieldKind::Text | FieldKind::File => {
            let shown = if f.kind == FieldKind::File {
                if f.value.is_empty() {
                    String::from("(no file)")
                } else {
                    f.value.clone()
                }
            } else if f.value.is_empty() && !f.label.is_empty() {
                // Placeholder-ish hint is not shown; keep underscores.
                String::new()
            } else {
                // Show the end of long values (where the cursor is).
                let w = text_width(&f.value);
                if w > f.size {
                    let skip = w - f.size;
                    let mut acc = 0;
                    f.value
                        .chars()
                        .skip_while(|c| {
                            let r = acc < skip;
                            acc += char_width(*c);
                            r
                        })
                        .collect()
                } else {
                    f.value.clone()
                }
            };
            format!("[{}]", pad(&shown, f.size, '_'))
        }
        FieldKind::Password => {
            let n = f.value.chars().count().min(f.size);
            let mut s: String = core::iter::repeat_n('*', n).collect();
            s = pad(&s, f.size, '_');
            format!("[{}]", s)
        }
        FieldKind::Checkbox => String::from(if f.checked { "[X]" } else { "[ ]" }),
        FieldKind::Radio => String::from(if f.checked { "(*)" } else { "( )" }),
        FieldKind::Select => {
            let label = f.options.get(f.selected).map_or("", |o| o.label.as_str());
            format!("[{} v]", pad(label, f.size, ' '))
        }
        FieldKind::Textarea => {
            let first = f.value.lines().next().unwrap_or("");
            let more = f.value.lines().count() > 1;
            let shown = if more {
                format!("{}…", first)
            } else {
                first.to_string()
            };
            format!("[{}]", pad(&shown, f.size, '_'))
        }
        FieldKind::Submit | FieldKind::Reset | FieldKind::Button | FieldKind::Image => {
            let label = if !f.label.is_empty() {
                f.label.clone()
            } else if !f.value.is_empty() {
                f.value.clone()
            } else {
                String::from(match f.kind {
                    FieldKind::Reset => "Reset",
                    FieldKind::Button => "Button",
                    _ => "Submit",
                })
            };
            format!("[ {} ]", label)
        }
    }
}
