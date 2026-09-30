//! Computed style: property values after the cascade, inheritance and
//! computation (lengths in px except percentages, colors resolved).

use crate::parser::Cv;
use crate::tokenizer::Token;
use crate::values::*;
use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Display {
    None,
    Contents,
    Block,
    #[default]
    Inline,
    InlineBlock,
    FlowRoot,
    ListItem,
    Flex,
    InlineFlex,
    Grid,
    InlineGrid,
    Table,
    InlineTable,
    TableRowGroup,
    TableHeaderGroup,
    TableFooterGroup,
    TableRow,
    TableCell,
    TableColumn,
    TableColumnGroup,
    TableCaption,
}

impl Display {
    pub fn is_inline_level(self) -> bool {
        matches!(
            self,
            Display::Inline
                | Display::InlineBlock
                | Display::InlineFlex
                | Display::InlineGrid
                | Display::InlineTable
        )
    }
    /// Blockified form (for floats, absolutely positioned boxes, flex and
    /// grid items, the root).
    pub fn blockify(self) -> Display {
        match self {
            Display::Inline | Display::InlineBlock => Display::Block,
            Display::InlineFlex => Display::Flex,
            Display::InlineGrid => Display::Grid,
            Display::InlineTable => Display::Table,
            Display::TableRowGroup
            | Display::TableHeaderGroup
            | Display::TableFooterGroup
            | Display::TableRow
            | Display::TableCell
            | Display::TableColumn
            | Display::TableColumnGroup
            | Display::TableCaption => Display::Block,
            d => d,
        }
    }
}

macro_rules! keyword_enum {
    ($name:ident { $($variant:ident = $kw:literal),+ $(,)? } default $def:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum $name { $($variant),+ }
        impl Default for $name { fn default() -> Self { $name::$def } }
        impl $name {
            pub fn parse(s: &str) -> Option<$name> {
                let l = s.to_ascii_lowercase();
                $(if l == $kw { return Some($name::$variant); })+
                None
            }
            /// The CSS keyword.
            pub fn as_str(self) -> &'static str {
                match self { $($name::$variant => $kw),+ }
            }
        }
    };
}

keyword_enum!(Position { Static = "static", Relative = "relative", Absolute = "absolute", Fixed = "fixed", Sticky = "sticky" } default Static);
keyword_enum!(Float { None = "none", Left = "left", Right = "right", InlineStart = "inline-start", InlineEnd = "inline-end" } default None);
keyword_enum!(Clear { None = "none", Left = "left", Right = "right", Both = "both", InlineStart = "inline-start", InlineEnd = "inline-end" } default None);
keyword_enum!(BorderStyle { None = "none", Hidden = "hidden", Solid = "solid", Dotted = "dotted", Dashed = "dashed", Double = "double", Groove = "groove", Ridge = "ridge", Inset = "inset", Outset = "outset" } default None);
keyword_enum!(BoxSizing { ContentBox = "content-box", BorderBox = "border-box" } default ContentBox);
keyword_enum!(Overflow { Visible = "visible", Hidden = "hidden", Clip = "clip", Scroll = "scroll", Auto = "auto" } default Visible);
keyword_enum!(Visibility { Visible = "visible", Hidden = "hidden", Collapse = "collapse" } default Visible);
keyword_enum!(FontStyle { Normal = "normal", Italic = "italic", Oblique = "oblique" } default Normal);
keyword_enum!(TextAlign { Start = "start", End = "end", Left = "left", Right = "right", Center = "center", Justify = "justify", MatchParent = "match-parent", WebkitCenter = "-webkit-center" } default Start);
keyword_enum!(TextTransform { None = "none", Uppercase = "uppercase", Lowercase = "lowercase", Capitalize = "capitalize", FullWidth = "full-width" } default None);
keyword_enum!(WhiteSpace { Normal = "normal", Pre = "pre", Nowrap = "nowrap", PreWrap = "pre-wrap", PreLine = "pre-line", BreakSpaces = "break-spaces" } default Normal);
keyword_enum!(WordBreak { Normal = "normal", BreakAll = "break-all", KeepAll = "keep-all", BreakWord = "break-word" } default Normal);
keyword_enum!(OverflowWrap { Normal = "normal", BreakWord = "break-word", Anywhere = "anywhere" } default Normal);
keyword_enum!(TextOverflow { Clip = "clip", Ellipsis = "ellipsis" } default Clip);
keyword_enum!(ListStylePosition { Outside = "outside", Inside = "inside" } default Outside);
keyword_enum!(FlexDirection { Row = "row", RowReverse = "row-reverse", Column = "column", ColumnReverse = "column-reverse" } default Row);
keyword_enum!(FlexWrap { Nowrap = "nowrap", Wrap = "wrap", WrapReverse = "wrap-reverse" } default Nowrap);
keyword_enum!(BorderCollapse { Separate = "separate", Collapse = "collapse" } default Separate);
keyword_enum!(TableLayout { Auto = "auto", Fixed = "fixed" } default Auto);
keyword_enum!(CaptionSide { Top = "top", Bottom = "bottom" } default Top);
keyword_enum!(Direction { Ltr = "ltr", Rtl = "rtl" } default Ltr);
keyword_enum!(PointerEvents { Auto = "auto", None = "none" } default Auto);
keyword_enum!(ObjectFit { Fill = "fill", Contain = "contain", Cover = "cover", None = "none", ScaleDown = "scale-down" } default Fill);
keyword_enum!(Cursor { Auto = "auto", Default = "default", Pointer = "pointer", Text = "text", Wait = "wait", Move = "move", NotAllowed = "not-allowed", Crosshair = "crosshair", Help = "help", Progress = "progress", Grab = "grab", Grabbing = "grabbing" } default Auto);

/// `align-items`/`justify-content`/... values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Normal,
    Auto,
    Stretch,
    Start,
    End,
    FlexStart,
    FlexEnd,
    Center,
    Baseline,
    LastBaseline,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
    Left,
    Right,
    SelfStart,
    SelfEnd,
}

impl Align {
    fn parse(items: &[&Cv]) -> Option<Align> {
        // Skip overflow-position and legacy keywords.
        let words: Vec<String> = items
            .iter()
            .map(|c| c.ident().map(|s| s.to_ascii_lowercase()))
            .collect::<Option<_>>()?;
        let w: Vec<&str> = words
            .iter()
            .map(String::as_str)
            .filter(|w| !matches!(*w, "safe" | "unsafe" | "legacy" | "first"))
            .collect();
        let last = *w.last()?;
        Some(match last {
            "normal" => Align::Normal,
            "auto" => Align::Auto,
            "stretch" => Align::Stretch,
            "start" => Align::Start,
            "end" => Align::End,
            "flex-start" => Align::FlexStart,
            "flex-end" => Align::FlexEnd,
            "center" => Align::Center,
            "baseline" => {
                if words.iter().any(|x| x == "last") {
                    Align::LastBaseline
                } else {
                    Align::Baseline
                }
            }
            "space-between" => Align::SpaceBetween,
            "space-around" => Align::SpaceAround,
            "space-evenly" => Align::SpaceEvenly,
            "left" => Align::Left,
            "right" => Align::Right,
            "self-start" => Align::SelfStart,
            "self-end" => Align::SelfEnd,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum VerticalAlign {
    Baseline,
    Sub,
    Super,
    Top,
    TextTop,
    Middle,
    Bottom,
    TextBottom,
    Length(LengthPercentage),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LineHeight {
    Normal,
    Number(f32),
    Px(f32),
}

impl LineHeight {
    pub fn px(&self, font_size: f32) -> f32 {
        match *self {
            LineHeight::Normal => font_size * 1.2,
            LineHeight::Number(n) => font_size * n,
            LineHeight::Px(p) => p,
        }
    }
}

pub const DECO_UNDERLINE: u8 = 1;
pub const DECO_OVERLINE: u8 = 2;
pub const DECO_LINE_THROUGH: u8 = 4;
pub const DECO_BLINK: u8 = 8;

#[derive(Debug, Clone, PartialEq)]
pub enum ListStyleType {
    None,
    Disc,
    Circle,
    Square,
    Decimal,
    DecimalLeadingZero,
    LowerAlpha,
    UpperAlpha,
    LowerRoman,
    UpperRoman,
    LowerGreek,
    DisclosureOpen,
    DisclosureClosed,
    /// A string marker (`list-style-type: "- "`).
    String(String),
}

impl ListStyleType {
    fn parse(cv: &Cv) -> Option<ListStyleType> {
        if let Cv::Token(Token::String(s)) = cv {
            return Some(ListStyleType::String(s.clone()));
        }
        Some(match cv.ident()?.to_ascii_lowercase().as_str() {
            "none" => ListStyleType::None,
            "disc" => ListStyleType::Disc,
            "circle" => ListStyleType::Circle,
            "square" => ListStyleType::Square,
            "decimal" | "arabic-indic" | "cjk-decimal" => ListStyleType::Decimal,
            "decimal-leading-zero" => ListStyleType::DecimalLeadingZero,
            "lower-alpha" | "lower-latin" => ListStyleType::LowerAlpha,
            "upper-alpha" | "upper-latin" => ListStyleType::UpperAlpha,
            "lower-roman" => ListStyleType::LowerRoman,
            "upper-roman" => ListStyleType::UpperRoman,
            "lower-greek" => ListStyleType::LowerGreek,
            "disclosure-open" => ListStyleType::DisclosureOpen,
            "disclosure-closed" => ListStyleType::DisclosureClosed,
            _ => ListStyleType::Decimal,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ContentItem {
    String(String),
    Attr(String),
    Counter(String, ListStyleType),
    Counters(String, String, ListStyleType),
    OpenQuote,
    CloseQuote,
    NoOpenQuote,
    NoCloseQuote,
    Url(String),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub enum Content {
    #[default]
    Normal,
    None,
    Items(Vec<ContentItem>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum FlexBasis {
    Auto,
    Content,
    Lp(LengthPercentage),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Size {
    Auto,
    Lp(LengthPercentage),
    MinContent,
    MaxContent,
    FitContent(Option<LengthPercentage>),
}

impl Size {
    pub fn is_auto(&self) -> bool {
        matches!(self, Size::Auto)
    }
    pub fn lp(&self) -> Option<&LengthPercentage> {
        match self {
            Size::Lp(l) => Some(l),
            _ => None,
        }
    }
    pub fn resolve(&self, basis: Option<f32>) -> Option<f32> {
        match self {
            Size::Lp(l) if !l.has_percent() || basis.is_some() => Some(l.resolve_opt(basis)),
            _ => None,
        }
    }
}

/// `max-width`/`max-height`.
#[derive(Debug, Clone, PartialEq)]
pub enum MaxSize {
    None,
    Size(Size),
}

impl MaxSize {
    pub fn resolve(&self, basis: Option<f32>) -> Option<f32> {
        match self {
            MaxSize::None => None,
            MaxSize::Size(s) => s.resolve(basis),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TrackSize {
    Lp(LengthPercentage),
    Fr(f32),
    Auto,
    MinContent,
    MaxContent,
    MinMax(Box<TrackSize>, Box<TrackSize>),
    FitContent(LengthPercentage),
}

#[derive(Debug, Clone, PartialEq)]
pub enum RepeatCount {
    Count(u32),
    AutoFill,
    AutoFit,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TrackItem {
    Size(TrackSize),
    Names(Vec<String>),
    Repeat(RepeatCount, Vec<TrackItem>),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub enum GridTemplate {
    #[default]
    None,
    Tracks(Vec<TrackItem>),
    Subgrid,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub enum GridLine {
    #[default]
    Auto,
    /// Line number (negative from the end) or the n-th line named.
    Line(i32, Option<String>),
    Span(u32, Option<String>),
    /// A named area/line (`grid-row: header`).
    Named(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GridAutoFlow {
    pub column: bool,
    pub dense: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Shadow {
    pub x: f32,
    pub y: f32,
    pub blur: f32,
    pub spread: f32,
    pub color: Color,
    pub inset: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Image {
    Url(String),
    /// `linear-gradient(angle, stops)`; angle in degrees (180 = to bottom).
    Linear {
        angle: f32,
        stops: Vec<(Color, Option<LengthPercentage>)>,
        repeating: bool,
    },
    Radial {
        stops: Vec<(Color, Option<LengthPercentage>)>,
        repeating: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum BgSize {
    Auto,
    Cover,
    Contain,
    Explicit(LengthAuto, LengthAuto),
}

#[derive(Debug, Clone, PartialEq)]
pub struct BackgroundLayer {
    pub image: Option<Image>,
    pub repeat_x: bool,
    pub repeat_y: bool,
    pub position: (LengthPercentage, LengthPercentage),
    pub size: BgSize,
}

impl Default for BackgroundLayer {
    fn default() -> Self {
        BackgroundLayer {
            image: None,
            repeat_x: true,
            repeat_y: true,
            position: (LengthPercentage::ZERO, LengthPercentage::ZERO),
            size: BgSize::Auto,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Transform {
    Translate(LengthPercentage, LengthPercentage),
    Scale(f32, f32),
    Rotate(f32),
    Skew(f32, f32),
    Matrix([f32; 6]),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ComputedStyle {
    pub display: Display,
    pub position: Position,
    pub float: Float,
    pub clear: Clear,
    pub top: LengthAuto,
    pub right: LengthAuto,
    pub bottom: LengthAuto,
    pub left: LengthAuto,
    pub z_index: Option<i32>,
    /// top, right, bottom, left.
    pub margin: [LengthAuto; 4],
    pub padding: [LengthPercentage; 4],
    pub border_width: [f32; 4],
    pub border_style: [BorderStyle; 4],
    pub border_color: [Color; 4],
    /// top-left, top-right, bottom-right, bottom-left (horizontal radii).
    pub border_radius: [LengthPercentage; 4],
    pub box_sizing: BoxSizing,
    pub width: Size,
    pub height: Size,
    pub min_width: Size,
    pub min_height: Size,
    pub max_width: MaxSize,
    pub max_height: MaxSize,
    pub aspect_ratio: Option<f32>,
    pub overflow_x: Overflow,
    pub overflow_y: Overflow,
    pub visibility: Visibility,
    pub opacity: f32,
    pub color: Rgba,
    pub background_color: Color,
    pub background: Vec<BackgroundLayer>,
    pub font_size: f32,
    pub font_weight: u16,
    pub font_style: FontStyle,
    pub font_family: Vec<String>,
    pub font_variant_small_caps: bool,
    pub line_height: LineHeight,
    pub text_align: TextAlign,
    pub text_indent: LengthPercentage,
    pub text_decoration_line: u8,
    pub text_decoration_color: Color,
    pub text_decoration_style: BorderStyle,
    pub text_transform: TextTransform,
    pub white_space: WhiteSpace,
    pub word_break: WordBreak,
    pub overflow_wrap: OverflowWrap,
    pub text_overflow: TextOverflow,
    pub letter_spacing: f32,
    pub word_spacing: f32,
    pub vertical_align: VerticalAlign,
    pub list_style_type: ListStyleType,
    pub list_style_position: ListStylePosition,
    pub list_style_image: Option<String>,
    pub content: Content,
    pub quotes: Option<Vec<(String, String)>>,
    pub counter_reset: Vec<(String, i32)>,
    pub counter_increment: Vec<(String, i32)>,
    pub counter_set: Vec<(String, i32)>,
    pub flex_direction: FlexDirection,
    pub flex_wrap: FlexWrap,
    pub flex_grow: f32,
    pub flex_shrink: f32,
    pub flex_basis: FlexBasis,
    pub order: i32,
    pub justify_content: Align,
    pub justify_items: Align,
    pub justify_self: Align,
    pub align_content: Align,
    pub align_items: Align,
    pub align_self: Align,
    pub row_gap: Option<LengthPercentage>,
    pub column_gap: Option<LengthPercentage>,
    pub grid_template_columns: GridTemplate,
    pub grid_template_rows: GridTemplate,
    pub grid_template_areas: Vec<Vec<String>>,
    pub grid_auto_columns: Vec<TrackSize>,
    pub grid_auto_rows: Vec<TrackSize>,
    pub grid_auto_flow: GridAutoFlow,
    pub grid_row_start: GridLine,
    pub grid_row_end: GridLine,
    pub grid_column_start: GridLine,
    pub grid_column_end: GridLine,
    pub border_collapse: BorderCollapse,
    pub border_spacing: (f32, f32),
    pub table_layout: TableLayout,
    pub caption_side: CaptionSide,
    pub empty_cells_hide: bool,
    pub direction: Direction,
    pub transform: Vec<Transform>,
    pub box_shadow: Vec<Shadow>,
    pub outline_width: f32,
    pub outline_style: BorderStyle,
    pub outline_color: Color,
    pub cursor: Cursor,
    pub pointer_events: PointerEvents,
    pub object_fit: ObjectFit,
    pub user_select_none: bool,
    pub column_count: Option<u32>,
    /// `appearance: none` (form controls drawn with CSS only).
    pub appearance_none: bool,
    /// Custom properties (inherited), as component values.
    pub custom: Vec<(String, Vec<Cv>)>,
}

impl Default for ComputedStyle {
    fn default() -> Self {
        let zero = LengthPercentage::ZERO;
        ComputedStyle {
            display: Display::Inline,
            position: Position::Static,
            float: Float::None,
            clear: Clear::None,
            top: LengthAuto::Auto,
            right: LengthAuto::Auto,
            bottom: LengthAuto::Auto,
            left: LengthAuto::Auto,
            z_index: None,
            margin: [
                LengthAuto::Lp(zero.clone()),
                LengthAuto::Lp(zero.clone()),
                LengthAuto::Lp(zero.clone()),
                LengthAuto::Lp(zero.clone()),
            ],
            padding: [zero.clone(), zero.clone(), zero.clone(), zero.clone()],
            border_width: [3.0; 4],
            border_style: [BorderStyle::None; 4],
            border_color: [Color::CurrentColor; 4],
            border_radius: [zero.clone(), zero.clone(), zero.clone(), zero.clone()],
            box_sizing: BoxSizing::ContentBox,
            width: Size::Auto,
            height: Size::Auto,
            min_width: Size::Auto,
            min_height: Size::Auto,
            max_width: MaxSize::None,
            max_height: MaxSize::None,
            aspect_ratio: None,
            overflow_x: Overflow::Visible,
            overflow_y: Overflow::Visible,
            visibility: Visibility::Visible,
            opacity: 1.0,
            color: Rgba::BLACK,
            background_color: Color::Rgba(Rgba::TRANSPARENT),
            background: Vec::new(),
            font_size: 16.0,
            font_weight: 400,
            font_style: FontStyle::Normal,
            font_family: vec![String::from("serif")],
            font_variant_small_caps: false,
            line_height: LineHeight::Normal,
            text_align: TextAlign::Start,
            text_indent: zero.clone(),
            text_decoration_line: 0,
            text_decoration_color: Color::CurrentColor,
            text_decoration_style: BorderStyle::Solid,
            text_transform: TextTransform::None,
            white_space: WhiteSpace::Normal,
            word_break: WordBreak::Normal,
            overflow_wrap: OverflowWrap::Normal,
            text_overflow: TextOverflow::Clip,
            letter_spacing: 0.0,
            word_spacing: 0.0,
            vertical_align: VerticalAlign::Baseline,
            list_style_type: ListStyleType::Disc,
            list_style_position: ListStylePosition::Outside,
            list_style_image: None,
            content: Content::Normal,
            quotes: None,
            counter_reset: Vec::new(),
            counter_increment: Vec::new(),
            counter_set: Vec::new(),
            flex_direction: FlexDirection::Row,
            flex_wrap: FlexWrap::Nowrap,
            flex_grow: 0.0,
            flex_shrink: 1.0,
            flex_basis: FlexBasis::Auto,
            order: 0,
            justify_content: Align::Normal,
            justify_items: Align::Normal,
            justify_self: Align::Auto,
            align_content: Align::Normal,
            align_items: Align::Normal,
            align_self: Align::Auto,
            row_gap: None,
            column_gap: None,
            grid_template_columns: GridTemplate::None,
            grid_template_rows: GridTemplate::None,
            grid_template_areas: Vec::new(),
            grid_auto_columns: vec![TrackSize::Auto],
            grid_auto_rows: vec![TrackSize::Auto],
            grid_auto_flow: GridAutoFlow::default(),
            grid_row_start: GridLine::Auto,
            grid_row_end: GridLine::Auto,
            grid_column_start: GridLine::Auto,
            grid_column_end: GridLine::Auto,
            border_collapse: BorderCollapse::Separate,
            border_spacing: (0.0, 0.0),
            table_layout: TableLayout::Auto,
            caption_side: CaptionSide::Top,
            empty_cells_hide: false,
            direction: Direction::Ltr,
            transform: Vec::new(),
            box_shadow: Vec::new(),
            outline_width: 3.0,
            outline_style: BorderStyle::None,
            outline_color: Color::CurrentColor,
            cursor: Cursor::Auto,
            pointer_events: PointerEvents::Auto,
            object_fit: ObjectFit::Fill,
            user_select_none: false,
            column_count: None,
            appearance_none: false,
            custom: Vec::new(),
        }
    }
}

/// Properties that inherit by default.
pub fn is_inherited(name: &str) -> bool {
    matches!(
        name,
        "color"
            | "font-size"
            | "font-weight"
            | "font-style"
            | "font-family"
            | "font-variant"
            | "font-variant-caps"
            | "line-height"
            | "text-align"
            | "text-indent"
            | "text-transform"
            | "white-space"
            | "word-break"
            | "overflow-wrap"
            | "word-wrap"
            | "letter-spacing"
            | "word-spacing"
            | "visibility"
            | "list-style-type"
            | "list-style-position"
            | "list-style-image"
            | "quotes"
            | "border-collapse"
            | "border-spacing"
            | "caption-side"
            | "empty-cells"
            | "direction"
            | "cursor"
            | "pointer-events"
            | "user-select"
    )
}

impl ComputedStyle {
    /// A child's starting style: inherited properties from `parent`, the
    /// rest initial.
    pub fn inherit_from(parent: &ComputedStyle) -> ComputedStyle {
        ComputedStyle {
            color: parent.color,
            font_size: parent.font_size,
            font_weight: parent.font_weight,
            font_style: parent.font_style,
            font_family: parent.font_family.clone(),
            font_variant_small_caps: parent.font_variant_small_caps,
            line_height: parent.line_height,
            text_align: parent.text_align,
            text_indent: parent.text_indent.clone(),
            text_transform: parent.text_transform,
            white_space: parent.white_space,
            word_break: parent.word_break,
            overflow_wrap: parent.overflow_wrap,
            letter_spacing: parent.letter_spacing,
            word_spacing: parent.word_spacing,
            visibility: parent.visibility,
            list_style_type: parent.list_style_type.clone(),
            list_style_position: parent.list_style_position,
            list_style_image: parent.list_style_image.clone(),
            quotes: parent.quotes.clone(),
            border_collapse: parent.border_collapse,
            border_spacing: parent.border_spacing,
            caption_side: parent.caption_side,
            empty_cells_hide: parent.empty_cells_hide,
            direction: parent.direction,
            cursor: parent.cursor,
            pointer_events: parent.pointer_events,
            user_select_none: parent.user_select_none,
            custom: parent.custom.clone(),
            ..ComputedStyle::default()
        }
    }

    pub fn line_height_px(&self) -> f32 {
        self.line_height.px(self.font_size)
    }

    pub fn is_positioned(&self) -> bool {
        self.position != Position::Static
    }

    pub fn is_out_of_flow(&self) -> bool {
        matches!(self.position, Position::Absolute | Position::Fixed) || self.float != Float::None
    }

    pub fn border_px(&self, side: usize) -> f32 {
        match self.border_style[side] {
            BorderStyle::None | BorderStyle::Hidden => 0.0,
            _ => self.border_width[side],
        }
    }

    pub fn background_color_rgba(&self) -> Rgba {
        self.background_color.resolve(self.color)
    }

    pub fn custom_property(&self, name: &str) -> Option<&[Cv]> {
        self.custom
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_slice())
    }

    pub fn set_custom(&mut self, name: &str, value: Vec<Cv>) {
        if let Some(e) = self.custom.iter_mut().find(|(n, _)| n == name) {
            e.1 = value;
        } else {
            self.custom.push((name.to_string(), value));
        }
    }
}

// ---------------------------------------------------------------------------
// Applying declarations
// ---------------------------------------------------------------------------

/// Values a declaration needs from its surroundings.
pub struct ApplyContext<'a> {
    pub parent: &'a ComputedStyle,
    pub root_font_size: f32,
    pub viewport_w: f32,
    pub viewport_h: f32,
}

impl ApplyContext<'_> {
    fn len_ctx(&self, style: &ComputedStyle) -> LengthContext {
        LengthContext {
            font_size: style.font_size,
            root_font_size: self.root_font_size,
            viewport_w: self.viewport_w,
            viewport_h: self.viewport_h,
            line_height: style.line_height_px(),
        }
    }
}

fn kw(cv: &Cv) -> Option<String> {
    cv.ident().map(|s| s.to_ascii_lowercase())
}

fn one<'a>(v: &'a [&'a Cv]) -> Option<&'a Cv> {
    if v.len() == 1 { Some(v[0]) } else { None }
}

/// 1-4 values for the four sides (top, right, bottom, left).
fn four<T: Clone>(v: &[&Cv], mut f: impl FnMut(&Cv) -> Option<T>) -> Option<[T; 4]> {
    let vals: Vec<T> = v.iter().map(|c| f(c)).collect::<Option<_>>()?;
    Some(match vals.len() {
        1 => [
            vals[0].clone(),
            vals[0].clone(),
            vals[0].clone(),
            vals[0].clone(),
        ],
        2 => [
            vals[0].clone(),
            vals[1].clone(),
            vals[0].clone(),
            vals[1].clone(),
        ],
        3 => [
            vals[0].clone(),
            vals[1].clone(),
            vals[2].clone(),
            vals[1].clone(),
        ],
        4 => [
            vals[0].clone(),
            vals[1].clone(),
            vals[2].clone(),
            vals[3].clone(),
        ],
        _ => return None,
    })
}

fn border_width_kw(cv: &Cv, ctx: &LengthContext) -> Option<f32> {
    match kw(cv).as_deref() {
        Some("thin") => Some(1.0),
        Some("medium") => Some(3.0),
        Some("thick") => Some(5.0),
        _ => length(cv, ctx).filter(|v| *v >= 0.0),
    }
}

fn size(cv: &Cv, ctx: &LengthContext) -> Option<Size> {
    match kw(cv).as_deref() {
        Some("auto") => Some(Size::Auto),
        Some("min-content" | "-webkit-min-content" | "-moz-min-content") => Some(Size::MinContent),
        Some("max-content" | "-webkit-max-content" | "-moz-max-content") => Some(Size::MaxContent),
        Some(
            "fit-content"
            | "-webkit-fit-content"
            | "-moz-fit-content"
            | "stretch"
            | "-webkit-fill-available"
            | "-moz-available",
        ) => Some(Size::FitContent(None)),
        _ => {
            if let Cv::Function { name, args } = cv
                && name.eq_ignore_ascii_case("fit-content")
            {
                let a = non_ws(args);
                return Some(Size::FitContent(Some(length_percentage(one(&a)?, ctx)?)));
            }
            let lp = length_percentage(cv, ctx)?;
            if lp.fixed().is_some_and(|v| v < 0.0) {
                return None;
            }
            Some(Size::Lp(lp))
        }
    }
}

fn track_size(cv: &Cv, ctx: &LengthContext) -> Option<TrackSize> {
    match cv {
        Cv::Token(Token::Dimension { value, unit }) if unit.eq_ignore_ascii_case("fr") => {
            Some(TrackSize::Fr(*value))
        }
        Cv::Function { name, args } if name.eq_ignore_ascii_case("minmax") => {
            let parts = split_commas(args);
            if parts.len() != 2 {
                return None;
            }
            let a = non_ws(parts[0]);
            let b = non_ws(parts[1]);
            Some(TrackSize::MinMax(
                Box::new(track_size(one(&a)?, ctx)?),
                Box::new(track_size(one(&b)?, ctx)?),
            ))
        }
        Cv::Function { name, args } if name.eq_ignore_ascii_case("fit-content") => {
            let a = non_ws(args);
            Some(TrackSize::FitContent(length_percentage(one(&a)?, ctx)?))
        }
        _ => match kw(cv).as_deref() {
            Some("auto") => Some(TrackSize::Auto),
            Some("min-content") => Some(TrackSize::MinContent),
            Some("max-content") => Some(TrackSize::MaxContent),
            _ => Some(TrackSize::Lp(length_percentage(cv, ctx)?)),
        },
    }
}

fn track_list(v: &[&Cv], ctx: &LengthContext) -> Option<Vec<TrackItem>> {
    let mut out = Vec::new();
    for cv in v {
        match cv {
            Cv::Block { open: '[', items } => {
                out.push(TrackItem::Names(
                    items
                        .iter()
                        .filter_map(|c| c.ident().map(ToString::to_string))
                        .collect(),
                ));
            }
            Cv::Function { name, args } if name.eq_ignore_ascii_case("repeat") => {
                let parts = split_commas(args);
                if parts.len() < 2 {
                    return None;
                }
                let count = non_ws(parts[0]);
                let count = match kw(one(&count)?).as_deref() {
                    Some("auto-fill") => RepeatCount::AutoFill,
                    Some("auto-fit") => RepeatCount::AutoFit,
                    _ => RepeatCount::Count(integer(one(&count)?)?.max(1) as u32),
                };
                // The track list may itself contain commas only inside
                // functions, so rejoin the remaining parts.
                let rest: Vec<Cv> = args
                    .iter()
                    .skip_while(|c| !matches!(c, Cv::Token(Token::Comma)))
                    .skip(1)
                    .cloned()
                    .collect();
                let inner = non_ws(&rest);
                out.push(TrackItem::Repeat(count, track_list(&inner, ctx)?));
            }
            _ => out.push(TrackItem::Size(track_size(cv, ctx)?)),
        }
    }
    Some(out)
}

fn grid_line(v: &[&Cv]) -> Option<GridLine> {
    if v.len() == 1 && kw(v[0]).as_deref() == Some("auto") {
        return Some(GridLine::Auto);
    }
    let mut span = false;
    let mut num: Option<i32> = None;
    let mut name: Option<String> = None;
    for cv in v {
        match cv {
            c if kw(c).as_deref() == Some("span") => span = true,
            Cv::Token(Token::Number { value, .. }) => num = Some(*value as i32),
            c => name = Some(c.ident()?.to_string()),
        }
    }
    Some(if span {
        GridLine::Span(num.unwrap_or(1).max(1) as u32, name)
    } else if let Some(n) = num {
        GridLine::Line(n, name)
    } else {
        GridLine::Named(name?)
    })
}

fn split_slash<'a>(v: &'a [&'a Cv]) -> (Vec<&'a Cv>, Option<Vec<&'a Cv>>) {
    match v.iter().position(|c| c.is_delim('/')) {
        Some(p) => (v[..p].to_vec(), Some(v[p + 1..].to_vec())),
        None => (v.to_vec(), None),
    }
}

fn shadow_list(cvs: &[Cv], ctx: &LengthContext) -> Option<Vec<Shadow>> {
    if non_ws(cvs).len() == 1 && kw(non_ws(cvs)[0]).as_deref() == Some("none") {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    for part in split_commas(cvs) {
        let mut lens = Vec::new();
        let mut color_v = Color::CurrentColor;
        let mut inset = false;
        for cv in non_ws(part) {
            if kw(cv).as_deref() == Some("inset") {
                inset = true;
            } else if let Some(l) = length(cv, ctx) {
                lens.push(l);
            } else {
                color_v = color(cv)?;
            }
        }
        if lens.len() < 2 {
            return None;
        }
        out.push(Shadow {
            x: lens[0],
            y: lens[1],
            blur: lens.get(2).copied().unwrap_or(0.0),
            spread: lens.get(3).copied().unwrap_or(0.0),
            color: color_v,
            inset,
        });
    }
    Some(out)
}

fn gradient_stops(
    parts: &[&[Cv]],
    ctx: &LengthContext,
) -> Option<Vec<(Color, Option<LengthPercentage>)>> {
    let mut stops = Vec::new();
    for p in parts {
        let it = non_ws(p);
        if it.is_empty() {
            continue;
        }
        let c = color(it[0]);
        match c {
            Some(c) => {
                let pos = it.get(1).and_then(|x| length_percentage(x, ctx));
                stops.push((c, pos));
                if let Some(second) = it.get(2).and_then(|x| length_percentage(x, ctx)) {
                    stops.push((c, Some(second)));
                }
            }
            None => {
                // A transition hint (a bare length): ignored.
                length_percentage(it[0], ctx)?;
            }
        }
    }
    if stops.len() < 2 {
        return None;
    }
    Some(stops)
}

pub fn image(cv: &Cv, ctx: &LengthContext) -> Option<Image> {
    match cv {
        Cv::Token(Token::Url(u)) => Some(Image::Url(u.clone())),
        Cv::Function { name, args } => {
            let n = name.to_ascii_lowercase();
            let n = n.trim_start_matches("-webkit-").trim_start_matches("-moz-");
            match n {
                "url" => {
                    let a = non_ws(args);
                    match one(&a)? {
                        Cv::Token(Token::String(s)) => Some(Image::Url(s.clone())),
                        _ => None,
                    }
                }
                "linear-gradient" | "repeating-linear-gradient" => {
                    let parts = split_commas(args);
                    let first = non_ws(parts[0]);
                    let (angle, skip) = if first
                        .first()
                        .is_some_and(|c| kw(c).as_deref() == Some("to"))
                    {
                        let words: Vec<String> = first[1..].iter().filter_map(|c| kw(c)).collect();
                        let has = |w: &str| words.iter().any(|x| x == w);
                        let a = match (has("top"), has("bottom"), has("left"), has("right")) {
                            (true, _, true, _) => 315.0,
                            (true, _, _, true) => 45.0,
                            (_, true, true, _) => 225.0,
                            (_, true, _, true) => 135.0,
                            (true, _, _, _) => 0.0,
                            (_, _, true, _) => 270.0,
                            (_, _, _, true) => 90.0,
                            _ => 180.0,
                        };
                        (a, 1)
                    } else if let Some(Cv::Token(Token::Dimension { value, unit })) = first.first()
                    {
                        let deg = match unit.to_ascii_lowercase().as_str() {
                            "deg" => *value,
                            "rad" => value.to_degrees(),
                            "turn" => value * 360.0,
                            "grad" => value * 0.9,
                            _ => return None,
                        };
                        (deg, 1)
                    } else {
                        (180.0, 0)
                    };
                    let stops = gradient_stops(&parts[skip..], ctx)?;
                    Some(Image::Linear {
                        angle,
                        stops,
                        repeating: n.starts_with("repeating"),
                    })
                }
                "radial-gradient"
                | "repeating-radial-gradient"
                | "conic-gradient"
                | "repeating-conic-gradient" => {
                    let parts = split_commas(args);
                    // Skip a shape/position description if the first part
                    // does not start with a color.
                    let skip =
                        usize::from(non_ws(parts[0]).first().is_some_and(|c| color(c).is_none()));
                    let stops = gradient_stops(&parts[skip..], ctx)?;
                    Some(Image::Radial {
                        stops,
                        repeating: n.starts_with("repeating"),
                    })
                }
                "image-set" => {
                    let parts = split_commas(args);
                    let first = non_ws(parts.first()?);
                    match first.first()? {
                        Cv::Token(Token::String(s) | Token::Url(s)) => Some(Image::Url(s.clone())),
                        c => image(c, ctx),
                    }
                }
                _ => None,
            }
        }
        _ => None,
    }
}

fn bg_position(v: &[&Cv], ctx: &LengthContext) -> Option<(LengthPercentage, LengthPercentage)> {
    let pos_kw = |c: &Cv| -> Option<(Option<bool>, f32)> {
        // (is_horizontal?, percent)
        Some(match kw(c)?.as_str() {
            "left" => (Some(true), 0.0),
            "right" => (Some(true), 100.0),
            "top" => (Some(false), 0.0),
            "bottom" => (Some(false), 100.0),
            "center" => (None, 50.0),
            _ => return None,
        })
    };
    match v.len() {
        1 => {
            if let Some((h, p)) = pos_kw(v[0]) {
                return Some(match h {
                    Some(false) => (
                        LengthPercentage::percent(50.0),
                        LengthPercentage::percent(p),
                    ),
                    _ => (
                        LengthPercentage::percent(p),
                        LengthPercentage::percent(50.0),
                    ),
                });
            }
            Some((
                length_percentage(v[0], ctx)?,
                LengthPercentage::percent(50.0),
            ))
        }
        2 => {
            let a = pos_kw(v[0]);
            let b = pos_kw(v[1]);
            let lp = |c: &Cv, k: Option<(Option<bool>, f32)>| {
                k.map(|(_, p)| LengthPercentage::percent(p))
                    .or_else(|| length_percentage(c, ctx))
            };
            // `top left` is allowed: swap when the first is vertical.
            if a.is_some_and(|x| x.0 == Some(false)) || b.is_some_and(|x| x.0 == Some(true)) {
                Some((lp(v[1], b)?, lp(v[0], a)?))
            } else {
                Some((lp(v[0], a)?, lp(v[1], b)?))
            }
        }
        _ => {
            // Four-value form `right 10px bottom 20px`: approximate.
            let mut x = LengthPercentage::percent(50.0);
            let mut y = LengthPercentage::percent(50.0);
            let mut i = 0;
            while i < v.len() {
                let k = pos_kw(v[i])?;
                let off = v.get(i + 1).and_then(|c| length_percentage(c, ctx));
                let from_end = matches!(kw(v[i]).as_deref(), Some("right" | "bottom"));
                let val = match (&off, from_end) {
                    (Some(LengthPercentage::Mix { px, pct }), true) => LengthPercentage::Mix {
                        px: -px,
                        pct: 100.0 - pct,
                    },
                    (Some(o), false) => o.clone(),
                    _ => LengthPercentage::percent(k.1),
                };
                if k.0 == Some(false) {
                    y = val
                } else {
                    x = val
                }
                i += if off.is_some() { 2 } else { 1 };
            }
            Some((x, y))
        }
    }
}

fn background_layer(
    part: &[Cv],
    ctx: &LengthContext,
    color_out: &mut Option<Color>,
) -> Option<BackgroundLayer> {
    let mut layer = BackgroundLayer::default();
    let it = non_ws(part);
    let mut i = 0;
    let mut pos: Vec<&Cv> = Vec::new();
    while i < it.len() {
        let c = it[i];
        if let Some(img) = image(c, ctx) {
            layer.image = Some(img);
        } else if kw(c).as_deref() == Some("none") {
            layer.image = None;
        } else if let Some(k) = kw(c).filter(|k| {
            matches!(
                k.as_str(),
                "repeat" | "no-repeat" | "repeat-x" | "repeat-y" | "space" | "round"
            )
        }) {
            match k.as_str() {
                "no-repeat" => {
                    layer.repeat_x = false;
                    layer.repeat_y = false;
                }
                "repeat-x" => layer.repeat_y = false,
                "repeat-y" => layer.repeat_x = false,
                _ => {}
            }
        } else if kw(c).is_some_and(|k| {
            matches!(
                k.as_str(),
                "scroll"
                    | "fixed"
                    | "local"
                    | "border-box"
                    | "padding-box"
                    | "content-box"
                    | "text"
            )
        }) {
        } else if c.is_delim('/') {
            // size follows
            let mut sz = Vec::new();
            i += 1;
            while i < it.len()
                && (length_percentage(it[i], ctx).is_some()
                    || kw(it[i])
                        .is_some_and(|k| matches!(k.as_str(), "auto" | "cover" | "contain")))
            {
                sz.push(it[i]);
                i += 1;
            }
            layer.size = bg_size(&sz, ctx)?;
            continue;
        } else if let Some(col) = color(c) {
            *color_out = Some(col);
        } else {
            pos.push(c);
        }
        i += 1;
    }
    if !pos.is_empty() {
        layer.position = bg_position(&pos, ctx)?;
    }
    Some(layer)
}

fn bg_size(v: &[&Cv], ctx: &LengthContext) -> Option<BgSize> {
    Some(match v {
        [c] if kw(c).as_deref() == Some("cover") => BgSize::Cover,
        [c] if kw(c).as_deref() == Some("contain") => BgSize::Contain,
        [a] => BgSize::Explicit(length_auto(a, ctx)?, LengthAuto::Auto),
        [a, b] => BgSize::Explicit(length_auto(a, ctx)?, length_auto(b, ctx)?),
        _ => return None,
    })
}

fn font_weight(cv: &Cv, parent: u16) -> Option<u16> {
    match kw(cv).as_deref() {
        Some("normal") => Some(400),
        Some("bold") => Some(700),
        Some("bolder") => Some(if parent < 400 {
            400
        } else if parent < 600 {
            700
        } else {
            900
        }),
        Some("lighter") => Some(if parent < 600 {
            100
        } else if parent < 800 {
            400
        } else {
            700
        }),
        _ => number(cv)
            .filter(|w| (1.0..=1000.0).contains(w))
            .map(|w| w as u16),
    }
}

fn font_size(cv: &Cv, parent: f32, ctx: &LengthContext) -> Option<f32> {
    let medium = 16.0;
    let v = match kw(cv).as_deref() {
        Some("xx-small") => medium * 0.6,
        Some("x-small") => medium * 0.75,
        Some("small") => medium * 0.889,
        Some("medium") => medium,
        Some("large") => medium * 1.2,
        Some("x-large") => medium * 1.5,
        Some("xx-large") => medium * 2.0,
        Some("xxx-large") => medium * 3.0,
        Some("smaller") => parent / 1.2,
        Some("larger") => parent * 1.2,
        Some("math") => parent,
        _ => {
            let pctx = LengthContext {
                font_size: parent,
                ..*ctx
            };
            length_percentage(cv, &pctx)?.resolve(parent)
        }
    };
    (v >= 0.0).then_some(v)
}

fn font_family(cvs: &[Cv]) -> Option<Vec<String>> {
    let mut out = Vec::new();
    for part in split_commas(cvs) {
        let it = non_ws(part);
        let name = match it.as_slice() {
            [Cv::Token(Token::String(s))] => s.clone(),
            words if !words.is_empty() => {
                let w: Vec<&str> = words.iter().map(|c| c.ident()).collect::<Option<_>>()?;
                w.join(" ")
            }
            _ => return None,
        };
        out.push(name);
    }
    Some(out)
}

fn line_height(cv: &Cv, ctx: &LengthContext) -> Option<LineHeight> {
    if kw(cv).as_deref() == Some("normal") {
        return Some(LineHeight::Normal);
    }
    if let Some(n) = number(cv) {
        return Some(LineHeight::Number(n));
    }
    let lp = length_percentage(cv, ctx)?;
    Some(LineHeight::Px(lp.resolve(ctx.font_size)))
}

fn counters(v: &[&Cv], default: i32) -> Option<Vec<(String, i32)>> {
    if v.len() == 1 && kw(v[0]).as_deref() == Some("none") {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < v.len() {
        let name = v[i].ident()?.to_string();
        let n = v.get(i + 1).and_then(|c| integer(c));
        out.push((name, n.unwrap_or(default)));
        i += if n.is_some() { 2 } else { 1 };
    }
    Some(out)
}

fn content(v: &[&Cv]) -> Option<Content> {
    if v.len() == 1 {
        match kw(v[0]).as_deref() {
            Some("normal") => return Some(Content::Normal),
            Some("none") => return Some(Content::None),
            _ => {}
        }
    }
    let mut items = Vec::new();
    for cv in v {
        if cv.is_delim('/') {
            break; // alt text follows
        }
        items.push(match cv {
            Cv::Token(Token::String(s)) => ContentItem::String(s.clone()),
            Cv::Token(Token::Url(u)) => ContentItem::Url(u.clone()),
            Cv::Function { name, args } => {
                let a = non_ws(args);
                match name.to_ascii_lowercase().as_str() {
                    "attr" => ContentItem::Attr(a.first()?.ident()?.to_ascii_lowercase()),
                    "counter" => {
                        let parts = split_commas(args);
                        let n = non_ws(parts[0]).first()?.ident()?.to_string();
                        let style = parts
                            .get(1)
                            .and_then(|p| non_ws(p).first().and_then(|c| ListStyleType::parse(c)))
                            .unwrap_or(ListStyleType::Decimal);
                        ContentItem::Counter(n, style)
                    }
                    "counters" => {
                        let parts = split_commas(args);
                        let n = non_ws(parts[0]).first()?.ident()?.to_string();
                        let sep = match non_ws(parts.get(1)?).first()? {
                            Cv::Token(Token::String(s)) => s.clone(),
                            _ => return None,
                        };
                        let style = parts
                            .get(2)
                            .and_then(|p| non_ws(p).first().and_then(|c| ListStyleType::parse(c)))
                            .unwrap_or(ListStyleType::Decimal);
                        ContentItem::Counters(n, sep, style)
                    }
                    "url" => match a.first()? {
                        Cv::Token(Token::String(s)) => ContentItem::Url(s.clone()),
                        _ => return None,
                    },
                    _ => return None,
                }
            }
            c => match kw(c)?.as_str() {
                "open-quote" => ContentItem::OpenQuote,
                "close-quote" => ContentItem::CloseQuote,
                "no-open-quote" => ContentItem::NoOpenQuote,
                "no-close-quote" => ContentItem::NoCloseQuote,
                _ => return None,
            },
        });
    }
    Some(Content::Items(items))
}

fn transform_list(v: &[&Cv], ctx: &LengthContext) -> Option<Vec<Transform>> {
    if v.len() == 1 && kw(v[0]).as_deref() == Some("none") {
        return Some(Vec::new());
    }
    let angle = |c: &Cv| -> Option<f32> {
        match c {
            Cv::Token(Token::Dimension { value, unit }) => {
                Some(match unit.to_ascii_lowercase().as_str() {
                    "deg" => *value,
                    "rad" => value.to_degrees(),
                    "turn" => value * 360.0,
                    "grad" => value * 0.9,
                    _ => return None,
                })
            }
            Cv::Token(Token::Number { value, .. }) if *value == 0.0 => Some(0.0),
            _ => None,
        }
    };
    let mut out = Vec::new();
    for cv in v {
        let Cv::Function { name, args } = cv else {
            return None;
        };
        let parts: Vec<Vec<&Cv>> = split_commas(args).into_iter().map(non_ws).collect();
        let a: Vec<&Cv> = parts.iter().flatten().copied().collect();
        let lp = |i: usize| a.get(i).and_then(|c| length_percentage(c, ctx));
        let num = |i: usize| a.get(i).and_then(|c| number(c));
        out.push(match name.to_ascii_lowercase().as_str() {
            "translate" => Transform::Translate(lp(0)?, lp(1).unwrap_or(LengthPercentage::ZERO)),
            "translatex" => Transform::Translate(lp(0)?, LengthPercentage::ZERO),
            "translatey" => Transform::Translate(LengthPercentage::ZERO, lp(0)?),
            "translate3d" => Transform::Translate(lp(0)?, lp(1)?),
            "translatez" => continue,
            "scale" | "scale3d" => {
                let x = num(0)?;
                Transform::Scale(x, num(1).unwrap_or(x))
            }
            "scalex" => Transform::Scale(num(0)?, 1.0),
            "scaley" => Transform::Scale(1.0, num(0)?),
            "rotate" | "rotatez" => Transform::Rotate(angle(a.first()?)?),
            "skew" => Transform::Skew(
                angle(a.first()?)?,
                a.get(1).and_then(|c| angle(c)).unwrap_or(0.0),
            ),
            "skewx" => Transform::Skew(angle(a.first()?)?, 0.0),
            "skewy" => Transform::Skew(0.0, angle(a.first()?)?),
            "matrix" => {
                let m: Vec<f32> = (0..6).map(num).collect::<Option<_>>()?;
                Transform::Matrix([m[0], m[1], m[2], m[3], m[4], m[5]])
            }
            "rotatex" | "rotatey" | "rotate3d" | "perspective" | "matrix3d" => continue,
            _ => return None,
        });
    }
    Some(out)
}

/// Properties accepted (so `@supports` and declarations know them) but
/// without an effect on layout or painting.
fn ignored_property(name: &str) -> bool {
    matches!(
        name,
        "transition"
            | "transition-property"
            | "transition-duration"
            | "transition-timing-function"
            | "transition-delay"
            | "animation"
            | "animation-name"
            | "animation-duration"
            | "animation-timing-function"
            | "animation-delay"
            | "animation-iteration-count"
            | "animation-direction"
            | "animation-fill-mode"
            | "animation-play-state"
            | "will-change"
            | "contain"
            | "content-visibility"
            | "isolation"
            | "mix-blend-mode"
            | "filter"
            | "backdrop-filter"
            | "clip-path"
            | "mask"
            | "mask-image"
            | "resize"
            | "scroll-behavior"
            | "scroll-margin"
            | "scroll-padding"
            | "scroll-snap-type"
            | "scroll-snap-align"
            | "overscroll-behavior"
            | "touch-action"
            | "-webkit-tap-highlight-color"
            | "-webkit-font-smoothing"
            | "-moz-osx-font-smoothing"
            | "text-rendering"
            | "font-feature-settings"
            | "font-kerning"
            | "font-display"
            | "font-stretch"
            | "font-optical-sizing"
            | "font-variation-settings"
            | "hyphens"
            | "tab-size"
            | "caret-color"
            | "accent-color"
            | "color-scheme"
            | "text-size-adjust"
            | "-webkit-text-size-adjust"
            | "text-shadow"
            | "text-underline-offset"
            | "text-decoration-thickness"
            | "text-underline-position"
            | "backface-visibility"
            | "perspective"
            | "transform-origin"
            | "transform-style"
            | "writing-mode"
            | "unicode-bidi"
            | "orphans"
            | "widows"
            | "page-break-before"
            | "page-break-after"
            | "page-break-inside"
            | "break-before"
            | "break-after"
            | "break-inside"
            | "image-rendering"
            | "shape-outside"
            | "counter-style"
            | "zoom"
            | "fill"
            | "stroke"
            | "stroke-width"
            | "print-color-adjust"
            | "-webkit-print-color-adjust"
            | "forced-color-adjust"
            | "line-clamp"
            | "-webkit-line-clamp"
            | "-webkit-box-orient"
            | "text-wrap"
            | "text-wrap-mode"
            | "text-wrap-style"
            | "column-width"
            | "column-rule"
            | "column-span"
            | "columns"
            | "inset-inline"
            | "inset-block"
            | "outline-offset"
            | "overflow-anchor"
            | "object-position"
            | "vertical-align-last"
            | "container"
            | "container-type"
            | "container-name"
            | "anchor-name"
            | "position-anchor"
            | "field-sizing"
            | "interpolate-size"
            | "view-transition-name"
    )
}

/// Apply one declaration (value already free of `var()`), returning
/// false when the property is unknown or the value invalid (the
/// declaration is then ignored, as the cascade requires).
pub fn apply(style: &mut ComputedStyle, name: &str, value: &[Cv], ctx: &ApplyContext) -> bool {
    apply_inner(style, name, value, ctx).is_some()
}

/// Whether `name: value` would be accepted (for `@supports`).
pub fn supported(name: &str, value: &[Cv]) -> bool {
    if name.starts_with("--") {
        return true;
    }
    let parent = ComputedStyle::default();
    let ctx = ApplyContext {
        parent: &parent,
        root_font_size: 16.0,
        viewport_w: 800.0,
        viewport_h: 600.0,
    };
    let mut s = ComputedStyle::default();
    apply(&mut s, name, value, &ctx)
}

/// Copy property `name` from `src` (for `inherit`).
fn copy_property(dst: &mut ComputedStyle, src: &ComputedStyle, name: &str) -> Option<()> {
    macro_rules! cp {
        ($($f:ident),+) => {{ $(dst.$f = src.$f.clone();)+ }};
    }
    let side = |n: &str| match n {
        "top" => Some(0),
        "right" => Some(1),
        "bottom" => Some(2),
        "left" => Some(3),
        _ => None,
    };
    match name {
        "display" => cp!(display),
        "position" => cp!(position),
        "float" => cp!(float),
        "clear" => cp!(clear),
        "top" => cp!(top),
        "right" => cp!(right),
        "bottom" => cp!(bottom),
        "left" => cp!(left),
        "z-index" => cp!(z_index),
        "margin" => cp!(margin),
        "padding" => cp!(padding),
        "border" => cp!(border_width, border_style, border_color),
        "border-width" => cp!(border_width),
        "border-style" => cp!(border_style),
        "border-color" => cp!(border_color),
        "border-radius" => cp!(border_radius),
        "box-sizing" => cp!(box_sizing),
        "width" => cp!(width),
        "height" => cp!(height),
        "min-width" => cp!(min_width),
        "min-height" => cp!(min_height),
        "max-width" => cp!(max_width),
        "max-height" => cp!(max_height),
        "overflow" => cp!(overflow_x, overflow_y),
        "overflow-x" => cp!(overflow_x),
        "overflow-y" => cp!(overflow_y),
        "visibility" => cp!(visibility),
        "opacity" => cp!(opacity),
        "color" => cp!(color),
        "background-color" => cp!(background_color),
        "background" => cp!(background_color, background),
        "font" => cp!(font_size, font_weight, font_style, font_family, line_height),
        "font-size" => cp!(font_size),
        "font-weight" => cp!(font_weight),
        "font-style" => cp!(font_style),
        "font-family" => cp!(font_family),
        "line-height" => cp!(line_height),
        "text-align" => cp!(text_align),
        "text-indent" => cp!(text_indent),
        "text-decoration" | "text-decoration-line" => cp!(text_decoration_line),
        "text-transform" => cp!(text_transform),
        "white-space" => cp!(white_space),
        "word-break" => cp!(word_break),
        "overflow-wrap" | "word-wrap" => cp!(overflow_wrap),
        "letter-spacing" => cp!(letter_spacing),
        "word-spacing" => cp!(word_spacing),
        "vertical-align" => cp!(vertical_align),
        "list-style" => cp!(list_style_type, list_style_position, list_style_image),
        "list-style-type" => cp!(list_style_type),
        "list-style-position" => cp!(list_style_position),
        "content" => cp!(content),
        "quotes" => cp!(quotes),
        "flex-direction" => cp!(flex_direction),
        "flex-wrap" => cp!(flex_wrap),
        "flex-grow" => cp!(flex_grow),
        "flex-shrink" => cp!(flex_shrink),
        "flex-basis" => cp!(flex_basis),
        "order" => cp!(order),
        "justify-content" => cp!(justify_content),
        "align-items" => cp!(align_items),
        "align-self" => cp!(align_self),
        "border-collapse" => cp!(border_collapse),
        "border-spacing" => cp!(border_spacing),
        "cursor" => cp!(cursor),
        "direction" => cp!(direction),
        "pointer-events" => cp!(pointer_events),
        _ => {
            if let Some(s) = name.strip_prefix("margin-").and_then(side) {
                dst.margin[s] = src.margin[s].clone();
            } else if let Some(s) = name.strip_prefix("padding-").and_then(side) {
                dst.padding[s] = src.padding[s].clone();
            } else if let Some(rest) = name.strip_prefix("border-") {
                let (sd, what) = rest.split_once('-').unwrap_or((rest, ""));
                let s = side(sd)?;
                match what {
                    "" => {
                        dst.border_width[s] = src.border_width[s];
                        dst.border_style[s] = src.border_style[s];
                        dst.border_color[s] = src.border_color[s];
                    }
                    "width" => dst.border_width[s] = src.border_width[s],
                    "style" => dst.border_style[s] = src.border_style[s],
                    "color" => dst.border_color[s] = src.border_color[s],
                    _ => return None,
                }
            } else {
                return None;
            }
        }
    }
    Some(())
}

fn apply_inner(
    style: &mut ComputedStyle,
    name: &str,
    value: &[Cv],
    ctx: &ApplyContext,
) -> Option<()> {
    let v = non_ws(value);
    // CSS-wide keywords.
    if v.len() == 1 {
        let k = kw(v[0]);
        match k.as_deref() {
            Some("inherit") => {
                copy_property(style, ctx.parent, name)
                    .or_else(|| ignored_property(name).then_some(()))?;
                return Some(());
            }
            Some("initial") => {
                let init = ComputedStyle::default();
                copy_property(style, &init, name)
                    .or_else(|| ignored_property(name).then_some(()))?;
                return Some(());
            }
            Some("unset" | "revert" | "revert-layer") => {
                let src = if is_inherited(name) {
                    ctx.parent.clone()
                } else {
                    ComputedStyle::default()
                };
                copy_property(style, &src, name)
                    .or_else(|| ignored_property(name).then_some(()))?;
                return Some(());
            }
            _ => {}
        }
    }
    let lc = ctx.len_ctx(style);
    let side = |n: &str| match n {
        "top" | "block-start" => Some(0),
        "right" | "inline-end" => Some(1),
        "bottom" | "block-end" => Some(2),
        "left" | "inline-start" => Some(3),
        _ => None,
    };
    let name = name
        .trim_start_matches("-webkit-")
        .trim_start_matches("-moz-")
        .trim_start_matches("-ms-");
    match name {
        "display" => {
            let words: Vec<String> = v.iter().map(|c| kw(c)).collect::<Option<_>>()?;
            let w: Vec<&str> = words.iter().map(String::as_str).collect();
            style.display = match w.as_slice() {
                ["none"] => Display::None,
                ["contents"] => Display::Contents,
                ["block"] | ["block", "flow"] => Display::Block,
                ["inline"] | ["inline", "flow"] => Display::Inline,
                ["inline-block"] | ["inline", "flow-root"] => Display::InlineBlock,
                ["flow-root"] | ["block", "flow-root"] => Display::FlowRoot,
                ["list-item"]
                | ["block", "list-item"]
                | ["list-item", "block"]
                | ["block", "flow", "list-item"] => Display::ListItem,
                ["inline", "list-item"] => Display::InlineBlock,
                ["flex"] | ["block", "flex"] | ["box"] | ["flexbox"] => Display::Flex,
                ["inline-flex"] | ["inline", "flex"] | ["inline-box"] => Display::InlineFlex,
                ["grid"] | ["block", "grid"] => Display::Grid,
                ["inline-grid"] | ["inline", "grid"] => Display::InlineGrid,
                ["table"] | ["block", "table"] => Display::Table,
                ["inline-table"] | ["inline", "table"] => Display::InlineTable,
                ["table-row-group"] => Display::TableRowGroup,
                ["table-header-group"] => Display::TableHeaderGroup,
                ["table-footer-group"] => Display::TableFooterGroup,
                ["table-row"] => Display::TableRow,
                ["table-cell"] => Display::TableCell,
                ["table-column"] => Display::TableColumn,
                ["table-column-group"] => Display::TableColumnGroup,
                ["table-caption"] => Display::TableCaption,
                ["run-in"] => Display::Block,
                ["ruby"] | ["ruby-text"] | ["ruby-base"] => Display::Inline,
                _ => return None,
            };
        }
        "position" => style.position = Position::parse(&kw(one(&v)?)?)?,
        "float" => style.float = Float::parse(&kw(one(&v)?)?)?,
        "clear" => style.clear = Clear::parse(&kw(one(&v)?)?)?,
        "top" | "right" | "bottom" | "left" | "inset-block-start" | "inset-block-end"
        | "inset-inline-start" | "inset-inline-end" => {
            let l = length_auto(one(&v)?, &lc)?;
            match name {
                "top" | "inset-block-start" => style.top = l,
                "right" | "inset-inline-end" => style.right = l,
                "bottom" | "inset-block-end" => style.bottom = l,
                _ => style.left = l,
            }
        }
        "inset" => {
            let [t, r, b, l] = four(&v, |c| length_auto(c, &lc))?;
            style.top = t;
            style.right = r;
            style.bottom = b;
            style.left = l;
        }
        "z-index" => {
            style.z_index = if kw(one(&v)?).as_deref() == Some("auto") {
                None
            } else {
                Some(integer(one(&v)?)?)
            };
        }
        "margin" => style.margin = four(&v, |c| length_auto(c, &lc))?,
        "margin-block" | "margin-inline" => {
            let a = length_auto(v.first()?, &lc)?;
            let b = match v.get(1) {
                Some(c) => length_auto(c, &lc)?,
                None => a.clone(),
            };
            if name == "margin-block" {
                style.margin[0] = a;
                style.margin[2] = b;
            } else {
                style.margin[3] = a;
                style.margin[1] = b;
            }
        }
        "padding" => {
            style.padding = four(&v, |c| {
                length_percentage(c, &lc).filter(|l| l.fixed().is_none_or(|x| x >= 0.0))
            })?
        }
        "padding-block" | "padding-inline" => {
            let a = length_percentage(v.first()?, &lc)?;
            let b = match v.get(1) {
                Some(c) => length_percentage(c, &lc)?,
                None => a.clone(),
            };
            if name == "padding-block" {
                style.padding[0] = a;
                style.padding[2] = b;
            } else {
                style.padding[3] = a;
                style.padding[1] = b;
            }
        }
        "border"
        | "border-top"
        | "border-right"
        | "border-bottom"
        | "border-left"
        | "border-block"
        | "border-inline"
        | "border-block-start"
        | "border-block-end"
        | "border-inline-start"
        | "border-inline-end"
        | "outline" => {
            let mut w = None;
            let mut s = None;
            let mut c = None;
            for cv in &v {
                if let Some(x) = border_width_kw(cv, &lc).filter(|_| w.is_none()) {
                    w = Some(x);
                } else if let Some(x) = kw(cv)
                    .and_then(|k| BorderStyle::parse(&k))
                    .filter(|_| s.is_none())
                {
                    s = Some(x);
                } else if let Some(x) = color(cv).filter(|_| c.is_none()) {
                    c = Some(x);
                } else {
                    return None;
                }
            }
            let (w, s, c) = (
                w.unwrap_or(3.0),
                s.unwrap_or(BorderStyle::None),
                c.unwrap_or(Color::CurrentColor),
            );
            if name == "outline" {
                style.outline_width = w;
                style.outline_style = s;
                style.outline_color = c;
                return Some(());
            }
            let sides: &[usize] = match name {
                "border" => &[0, 1, 2, 3],
                "border-block" => &[0, 2],
                "border-inline" => &[1, 3],
                _ => {
                    let sd = name.strip_prefix("border-")?;
                    &[[0usize, 1, 2, 3][side(sd)?]]
                }
            };
            for &i in sides {
                style.border_width[i] = w;
                style.border_style[i] = s;
                style.border_color[i] = c;
            }
        }
        "border-width" => style.border_width = four(&v, |c| border_width_kw(c, &lc))?,
        "border-style" => style.border_style = four(&v, |c| BorderStyle::parse(&kw(c)?))?,
        "border-color" => style.border_color = four(&v, color)?,
        "border-radius" => {
            let (h, _) = split_slash(&v);
            style.border_radius = four(&h, |c| length_percentage(c, &lc))?;
        }
        "outline-width" => style.outline_width = border_width_kw(one(&v)?, &lc)?,
        "outline-style" => {
            style.outline_style = if kw(one(&v)?).as_deref() == Some("auto") {
                BorderStyle::Solid
            } else {
                BorderStyle::parse(&kw(one(&v)?)?)?
            }
        }
        "outline-color" => {
            style.outline_color = if kw(one(&v)?).as_deref() == Some("invert") {
                Color::CurrentColor
            } else {
                color(one(&v)?)?
            }
        }
        "box-sizing" => style.box_sizing = BoxSizing::parse(&kw(one(&v)?)?)?,
        "width" | "inline-size" => style.width = size(one(&v)?, &lc)?,
        "height" | "block-size" => style.height = size(one(&v)?, &lc)?,
        "min-width" | "min-inline-size" => style.min_width = size(one(&v)?, &lc)?,
        "min-height" | "min-block-size" => style.min_height = size(one(&v)?, &lc)?,
        "max-width" | "max-inline-size" | "max-height" | "max-block-size" => {
            let m = if kw(one(&v)?).as_deref() == Some("none") {
                MaxSize::None
            } else {
                MaxSize::Size(size(one(&v)?, &lc)?)
            };
            if name.starts_with("max-w") || name == "max-inline-size" {
                style.max_width = m;
            } else {
                style.max_height = m;
            }
        }
        "aspect-ratio" => {
            let (a, b) = split_slash(&v);
            let a: Vec<&Cv> = a
                .into_iter()
                .filter(|c| kw(c).as_deref() != Some("auto"))
                .collect();
            style.aspect_ratio = match (a.first(), b) {
                (None, _) => None,
                (Some(x), None) => Some(number(x)?),
                (Some(x), Some(y)) => Some(number(x)? / number(y.first()?)?.max(f32::MIN_POSITIVE)),
            };
        }
        "overflow" => {
            let x = Overflow::parse(&kw(v.first()?)?)?;
            let y = match v.get(1) {
                Some(c) => Overflow::parse(&kw(c)?)?,
                None => x,
            };
            style.overflow_x = x;
            style.overflow_y = y;
        }
        "overflow-x" | "overflow-inline" => style.overflow_x = Overflow::parse(&kw(one(&v)?)?)?,
        "overflow-y" | "overflow-block" => style.overflow_y = Overflow::parse(&kw(one(&v)?)?)?,
        "visibility" => style.visibility = Visibility::parse(&kw(one(&v)?)?)?,
        "opacity" => {
            style.opacity = match one(&v)? {
                Cv::Token(Token::Percentage(p)) => p / 100.0,
                c => number(c)?,
            }
            .clamp(0.0, 1.0)
        }
        "color" => {
            style.color = match color(one(&v)?)? {
                Color::Rgba(c) => c,
                Color::CurrentColor => ctx.parent.color,
            }
        }
        "background-color" => style.background_color = color(one(&v)?)?,
        "background" => {
            let mut col = None;
            let mut layers = Vec::new();
            for part in split_commas(value) {
                layers.push(background_layer(part, &lc, &mut col)?);
            }
            style.background_color = col.unwrap_or(Color::Rgba(Rgba::TRANSPARENT));
            layers.retain(|l| l.image.is_some());
            style.background = layers;
        }
        "background-image" => {
            let mut layers = Vec::new();
            for part in split_commas(value) {
                let it = non_ws(part);
                let c = one(&it)?;
                if kw(c).as_deref() == Some("none") {
                    continue;
                }
                layers.push(BackgroundLayer {
                    image: Some(image(c, &lc)?),
                    ..BackgroundLayer::default()
                });
            }
            style.background = layers;
        }
        "background-repeat" => {
            let k = kw(v.first()?)?;
            for l in style.background.iter_mut() {
                l.repeat_x = matches!(k.as_str(), "repeat" | "repeat-x" | "space" | "round");
                l.repeat_y = matches!(k.as_str(), "repeat" | "repeat-y" | "space" | "round");
                if v.len() == 2 {
                    l.repeat_y = kw(v[1]).is_some_and(|k| k != "no-repeat");
                }
            }
        }
        "background-position" => {
            let p = bg_position(&non_ws(split_commas(value)[0]), &lc)?;
            for l in style.background.iter_mut() {
                l.position = p.clone();
            }
        }
        "background-size" => {
            let s = bg_size(&non_ws(split_commas(value)[0]), &lc)?;
            for l in style.background.iter_mut() {
                l.size = s.clone();
            }
        }
        "background-attachment"
        | "background-clip"
        | "background-origin"
        | "background-blend-mode"
        | "background-position-x"
        | "background-position-y" => {}
        "font-size" => {
            style.font_size = font_size(one(&v)?, ctx.parent.font_size, &lc)?;
        }
        "font-weight" => style.font_weight = font_weight(one(&v)?, ctx.parent.font_weight)?,
        "font-style" => {
            let k = kw(v.first()?)?;
            style.font_style = FontStyle::parse(&k)?;
        }
        "font-family" => style.font_family = font_family(value)?,
        "font-variant" | "font-variant-caps" => {
            style.font_variant_small_caps =
                v.iter().any(|c| kw(c).as_deref() == Some("small-caps"));
        }
        "font" => {
            // [style || variant || weight || stretch]? size[/line-height] family
            if v.len() == 1
                && kw(v[0]).is_some_and(|k| {
                    matches!(
                        k.as_str(),
                        "caption"
                            | "icon"
                            | "menu"
                            | "message-box"
                            | "small-caption"
                            | "status-bar"
                    )
                })
            {
                return Some(());
            }
            let mut i = 0;
            let mut fs = FontStyle::Normal;
            let mut fw = 400;
            let mut caps = false;
            while i < v.len() {
                let k = kw(v[i]);
                match k.as_deref() {
                    Some("normal") => {}
                    Some("italic" | "oblique") => fs = FontStyle::parse(k.as_deref()?)?,
                    Some("small-caps") => caps = true,
                    Some("bold" | "bolder" | "lighter") => {
                        fw = font_weight(v[i], ctx.parent.font_weight)?
                    }
                    Some(
                        "condensed" | "expanded" | "semi-condensed" | "semi-expanded"
                        | "ultra-condensed" | "extra-condensed" | "extra-expanded"
                        | "ultra-expanded",
                    ) => {}
                    _ => {
                        if let Cv::Token(Token::Number { value, .. }) = v[i] {
                            fw = *value as u16;
                        } else {
                            break;
                        }
                    }
                }
                i += 1;
            }
            let sz = font_size(v.get(i)?, ctx.parent.font_size, &lc)?;
            i += 1;
            let mut lh = LineHeight::Normal;
            if v.get(i).is_some_and(|c| c.is_delim('/')) {
                let lctx = LengthContext {
                    font_size: sz,
                    ..lc
                };
                lh = line_height(v.get(i + 1)?, &lctx)?;
                i += 2;
            }
            // The family is the rest of the original value.
            let start = {
                let target = v.get(i)?;
                value.iter().position(|c| core::ptr::eq(c, *target))?
            };
            let fam = font_family(&value[start..])?;
            style.font_style = fs;
            style.font_weight = fw;
            style.font_variant_small_caps = caps;
            style.font_size = sz;
            style.line_height = lh;
            style.font_family = fam;
        }
        "line-height" => style.line_height = line_height(one(&v)?, &lc)?,
        "text-align" => style.text_align = TextAlign::parse(&kw(one(&v)?)?)?,
        "text-indent" => style.text_indent = length_percentage(v.first()?, &lc)?,
        "text-decoration" | "text-decoration-line" => {
            let mut line = 0u8;
            for c in &v {
                match kw(c).as_deref() {
                    Some("none") => line = 0,
                    Some("underline") => line |= DECO_UNDERLINE,
                    Some("overline") => line |= DECO_OVERLINE,
                    Some("line-through") => line |= DECO_LINE_THROUGH,
                    Some("blink") => line |= DECO_BLINK,
                    Some(k) if name == "text-decoration" => {
                        if let Some(s) = BorderStyle::parse(k).or(if k == "wavy" {
                            Some(BorderStyle::Dashed)
                        } else {
                            None
                        }) {
                            style.text_decoration_style = s;
                        } else if let Some(col) = color(c) {
                            style.text_decoration_color = col;
                        } else {
                            return None;
                        }
                    }
                    _ if name == "text-decoration" => {
                        if let Some(col) = color(c) {
                            style.text_decoration_color = col;
                        } else if length(c, &lc).is_none() {
                            return None;
                        }
                    }
                    _ => return None,
                }
            }
            style.text_decoration_line = line;
        }
        "text-decoration-color" => style.text_decoration_color = color(one(&v)?)?,
        "text-decoration-style" => {
            let k = kw(one(&v)?)?;
            style.text_decoration_style = if k == "wavy" {
                BorderStyle::Dashed
            } else {
                BorderStyle::parse(&k)?
            };
        }
        "text-transform" => style.text_transform = TextTransform::parse(&kw(one(&v)?)?)?,
        "white-space" | "white-space-collapse" => {
            let words: Vec<String> = v.iter().map(|c| kw(c)).collect::<Option<_>>()?;
            style.white_space = match words
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .as_slice()
            {
                [w] => match *w {
                    "collapse" => WhiteSpace::Normal,
                    "preserve" => WhiteSpace::PreWrap,
                    "preserve-breaks" => WhiteSpace::PreLine,
                    _ => WhiteSpace::parse(w)?,
                },
                ["collapse", "nowrap"] => WhiteSpace::Nowrap,
                ["preserve", "nowrap"] => WhiteSpace::Pre,
                _ => return None,
            };
        }
        "word-break" => style.word_break = WordBreak::parse(&kw(one(&v)?)?)?,
        "overflow-wrap" | "word-wrap" => style.overflow_wrap = OverflowWrap::parse(&kw(one(&v)?)?)?,
        "text-overflow" => {
            style.text_overflow =
                TextOverflow::parse(&kw(v.first()?)?).unwrap_or(TextOverflow::Clip)
        }
        "letter-spacing" | "word-spacing" => {
            let px = if kw(one(&v)?).as_deref() == Some("normal") {
                0.0
            } else {
                length(one(&v)?, &lc)?
            };
            if name == "letter-spacing" {
                style.letter_spacing = px;
            } else {
                style.word_spacing = px;
            }
        }
        "vertical-align" => {
            let c = one(&v)?;
            style.vertical_align = match kw(c).as_deref() {
                Some("baseline") => VerticalAlign::Baseline,
                Some("sub") => VerticalAlign::Sub,
                Some("super") => VerticalAlign::Super,
                Some("top") => VerticalAlign::Top,
                Some("text-top") => VerticalAlign::TextTop,
                Some("middle") => VerticalAlign::Middle,
                Some("bottom") => VerticalAlign::Bottom,
                Some("text-bottom") => VerticalAlign::TextBottom,
                _ => VerticalAlign::Length(length_percentage(c, &lc)?),
            };
        }
        "list-style" => {
            let mut t = None;
            let mut p = None;
            let mut img = None;
            let mut nones = 0;
            for c in &v {
                if kw(c).as_deref() == Some("none") {
                    nones += 1;
                } else if let Some(x) = kw(c).and_then(|k| ListStylePosition::parse(&k)) {
                    p = Some(x);
                } else if let Some(Image::Url(u)) = image(c, &lc) {
                    img = Some(u);
                } else {
                    t = Some(ListStyleType::parse(c)?);
                }
            }
            if nones > 0 && t.is_none() {
                t = Some(ListStyleType::None);
            }
            style.list_style_type = t.unwrap_or(ListStyleType::Disc);
            style.list_style_position = p.unwrap_or(ListStylePosition::Outside);
            style.list_style_image = img;
        }
        "list-style-type" => style.list_style_type = ListStyleType::parse(one(&v)?)?,
        "list-style-position" => {
            style.list_style_position = ListStylePosition::parse(&kw(one(&v)?)?)?
        }
        "list-style-image" => {
            style.list_style_image = match image(one(&v)?, &lc) {
                Some(Image::Url(u)) => Some(u),
                _ => None,
            }
        }
        "content" => style.content = content(&v)?,
        "quotes" => {
            if v.len() == 1 && kw(v[0]).is_some_and(|k| k == "none" || k == "auto") {
                style.quotes = if kw(v[0]).as_deref() == Some("none") {
                    Some(Vec::new())
                } else {
                    None
                };
            } else {
                let strs: Vec<String> = v
                    .iter()
                    .map(|c| match c {
                        Cv::Token(Token::String(s)) => Some(s.clone()),
                        _ => None,
                    })
                    .collect::<Option<_>>()?;
                if strs.len() % 2 != 0 {
                    return None;
                }
                style.quotes = Some(
                    strs.chunks(2)
                        .map(|p| (p[0].clone(), p[1].clone()))
                        .collect(),
                );
            }
        }
        "counter-reset" => style.counter_reset = counters(&v, 0)?,
        "counter-increment" => style.counter_increment = counters(&v, 1)?,
        "counter-set" => style.counter_set = counters(&v, 0)?,
        "flex-direction" => style.flex_direction = FlexDirection::parse(&kw(one(&v)?)?)?,
        "flex-wrap" => style.flex_wrap = FlexWrap::parse(&kw(one(&v)?)?)?,
        "flex-flow" => {
            for c in &v {
                let k = kw(c)?;
                if let Some(d) = FlexDirection::parse(&k) {
                    style.flex_direction = d;
                } else {
                    style.flex_wrap = FlexWrap::parse(&k)?;
                }
            }
        }
        "flex-grow" | "box-flex" => style.flex_grow = number(one(&v)?).filter(|x| *x >= 0.0)?,
        "flex-shrink" => style.flex_shrink = number(one(&v)?).filter(|x| *x >= 0.0)?,
        "flex-basis" => style.flex_basis = flex_basis(one(&v)?, &lc)?,
        "flex" => match v.as_slice() {
            [c] if kw(c).as_deref() == Some("none") => {
                style.flex_grow = 0.0;
                style.flex_shrink = 0.0;
                style.flex_basis = FlexBasis::Auto;
            }
            [c] if kw(c).as_deref() == Some("auto") => {
                style.flex_grow = 1.0;
                style.flex_shrink = 1.0;
                style.flex_basis = FlexBasis::Auto;
            }
            [c] if kw(c).as_deref() == Some("initial") => {
                style.flex_grow = 0.0;
                style.flex_shrink = 1.0;
                style.flex_basis = FlexBasis::Auto;
            }
            _ => {
                let mut nums = Vec::new();
                let mut basis = None;
                for c in &v {
                    if let (Some(n), true) = (number(c), basis.is_none() || nums.len() < 2) {
                        if n == 0.0 && nums.len() >= 2 {
                            basis = Some(FlexBasis::Lp(LengthPercentage::ZERO));
                        } else {
                            nums.push(n);
                        }
                    } else {
                        basis = Some(flex_basis(c, &lc)?);
                    }
                }
                if nums.len() > 2 || (nums.is_empty() && basis.is_none()) {
                    return None;
                }
                style.flex_grow = nums.first().copied().unwrap_or(1.0);
                style.flex_shrink = nums.get(1).copied().unwrap_or(1.0);
                style.flex_basis = basis.unwrap_or(FlexBasis::Lp(LengthPercentage::ZERO));
            }
        },
        "order" | "box-ordinal-group" => style.order = integer(one(&v)?)?,
        "justify-content" | "box-pack" => style.justify_content = Align::parse(&v)?,
        "justify-items" => style.justify_items = Align::parse(&v)?,
        "justify-self" => style.justify_self = Align::parse(&v)?,
        "align-content" => style.align_content = Align::parse(&v)?,
        "align-items" | "box-align" => style.align_items = Align::parse(&v)?,
        "align-self" => style.align_self = Align::parse(&v)?,
        "place-content" | "place-items" | "place-self" => {
            let a = Align::parse(&v[..1])?;
            let b = if v.len() > 1 {
                Align::parse(&v[1..])?
            } else {
                a
            };
            match name {
                "place-content" => {
                    style.align_content = a;
                    style.justify_content = b;
                }
                "place-items" => {
                    style.align_items = a;
                    style.justify_items = b;
                }
                _ => {
                    style.align_self = a;
                    style.justify_self = b;
                }
            }
        }
        "gap" | "grid-gap" => {
            let g = |c: &Cv| {
                if kw(c).as_deref() == Some("normal") {
                    Some(None)
                } else {
                    length_percentage(c, &lc).map(Some)
                }
            };
            let r = g(v.first()?)?;
            let c = match v.get(1) {
                Some(x) => g(x)?,
                None => r.clone(),
            };
            style.row_gap = r;
            style.column_gap = c;
        }
        "row-gap" | "grid-row-gap" => {
            style.row_gap = if kw(one(&v)?).as_deref() == Some("normal") {
                None
            } else {
                Some(length_percentage(one(&v)?, &lc)?)
            }
        }
        "column-gap" | "grid-column-gap" => {
            style.column_gap = if kw(one(&v)?).as_deref() == Some("normal") {
                None
            } else {
                Some(length_percentage(one(&v)?, &lc)?)
            }
        }
        "grid-template-columns" | "grid-template-rows" => {
            let t = if v.len() == 1 && kw(v[0]).as_deref() == Some("none") {
                GridTemplate::None
            } else if v
                .first()
                .is_some_and(|c| kw(c).as_deref() == Some("subgrid"))
            {
                GridTemplate::Subgrid
            } else {
                GridTemplate::Tracks(track_list(&v, &lc)?)
            };
            if name.ends_with("columns") {
                style.grid_template_columns = t;
            } else {
                style.grid_template_rows = t;
            }
        }
        "grid-template-areas" => {
            if v.len() == 1 && kw(v[0]).as_deref() == Some("none") {
                style.grid_template_areas = Vec::new();
            } else {
                let rows: Vec<Vec<String>> = v
                    .iter()
                    .map(|c| match c {
                        Cv::Token(Token::String(s)) => Some(area_row(s)),
                        _ => None,
                    })
                    .collect::<Option<_>>()?;
                if rows.windows(2).any(|w| w[0].len() != w[1].len()) {
                    return None;
                }
                style.grid_template_areas = rows;
            }
        }
        "grid-template" | "grid" => {
            if v.len() == 1 && kw(v[0]).as_deref() == Some("none") {
                style.grid_template_columns = GridTemplate::None;
                style.grid_template_rows = GridTemplate::None;
                style.grid_template_areas = Vec::new();
                return Some(());
            }
            let (rows, cols) = split_slash(&v);
            // Area strings with row sizes: `"a b" 1fr "c d" auto / 1fr 2fr`.
            if rows
                .iter()
                .any(|c| matches!(c, Cv::Token(Token::String(_))))
            {
                let mut areas = Vec::new();
                let mut sizes = Vec::new();
                for c in &rows {
                    match c {
                        Cv::Token(Token::String(s)) => {
                            areas.push(area_row(s));
                            sizes.push(TrackItem::Size(TrackSize::Auto));
                        }
                        Cv::Block { open: '[', .. } => {}
                        c => {
                            let t = track_size(c, &lc)?;
                            if let Some(last) = sizes.last_mut() {
                                *last = TrackItem::Size(t);
                            }
                        }
                    }
                }
                style.grid_template_areas = areas;
                style.grid_template_rows = GridTemplate::Tracks(sizes);
            } else if rows
                .first()
                .is_some_and(|c| kw(c).is_some_and(|k| k == "auto-flow" || k == "dense"))
            {
                style.grid_auto_flow = GridAutoFlow {
                    column: false,
                    dense: rows.iter().any(|c| kw(c).as_deref() == Some("dense")),
                };
                let sizes: Vec<&Cv> = rows
                    .iter()
                    .copied()
                    .filter(|c| !kw(c).is_some_and(|k| k == "auto-flow" || k == "dense"))
                    .collect();
                if !sizes.is_empty() {
                    style.grid_auto_rows = sizes
                        .iter()
                        .map(|c| track_size(c, &lc))
                        .collect::<Option<_>>()?;
                }
                style.grid_template_rows = GridTemplate::None;
            } else {
                style.grid_template_rows = GridTemplate::Tracks(track_list(&rows, &lc)?);
            }
            if let Some(c) = cols {
                if c.first()
                    .is_some_and(|x| kw(x).is_some_and(|k| k == "auto-flow" || k == "dense"))
                {
                    style.grid_auto_flow = GridAutoFlow {
                        column: true,
                        dense: c.iter().any(|x| kw(x).as_deref() == Some("dense")),
                    };
                    let sizes: Vec<&Cv> = c
                        .iter()
                        .copied()
                        .filter(|x| !kw(x).is_some_and(|k| k == "auto-flow" || k == "dense"))
                        .collect();
                    if !sizes.is_empty() {
                        style.grid_auto_columns = sizes
                            .iter()
                            .map(|x| track_size(x, &lc))
                            .collect::<Option<_>>()?;
                    }
                    style.grid_template_columns = GridTemplate::None;
                } else {
                    style.grid_template_columns = GridTemplate::Tracks(track_list(&c, &lc)?);
                }
            }
        }
        "grid-auto-columns" => {
            style.grid_auto_columns = v
                .iter()
                .map(|c| track_size(c, &lc))
                .collect::<Option<_>>()?
        }
        "grid-auto-rows" => {
            style.grid_auto_rows = v
                .iter()
                .map(|c| track_size(c, &lc))
                .collect::<Option<_>>()?
        }
        "grid-auto-flow" => {
            let mut f = GridAutoFlow::default();
            for c in &v {
                match kw(c)?.as_str() {
                    "row" => f.column = false,
                    "column" => f.column = true,
                    "dense" => f.dense = true,
                    _ => return None,
                }
            }
            style.grid_auto_flow = f;
        }
        "grid-row-start" => style.grid_row_start = grid_line(&v)?,
        "grid-row-end" => style.grid_row_end = grid_line(&v)?,
        "grid-column-start" => style.grid_column_start = grid_line(&v)?,
        "grid-column-end" => style.grid_column_end = grid_line(&v)?,
        "grid-row" | "grid-column" => {
            let (a, b) = split_slash(&v);
            let start = grid_line(&a)?;
            let end = match b {
                Some(b) => grid_line(&b)?,
                None => match &start {
                    GridLine::Named(n) => GridLine::Named(n.clone()),
                    _ => GridLine::Auto,
                },
            };
            if name == "grid-row" {
                style.grid_row_start = start;
                style.grid_row_end = end;
            } else {
                style.grid_column_start = start;
                style.grid_column_end = end;
            }
        }
        "grid-area" => {
            let parts: Vec<Vec<&Cv>> = {
                let mut out = vec![Vec::new()];
                for c in &v {
                    if c.is_delim('/') {
                        out.push(Vec::new());
                    } else {
                        out.last_mut()?.push(*c);
                    }
                }
                out
            };
            let lines: Vec<GridLine> = parts.iter().map(|p| grid_line(p)).collect::<Option<_>>()?;
            let get = |i: usize, fallback: usize| -> GridLine {
                lines.get(i).cloned().unwrap_or_else(|| {
                    match &lines[fallback.min(lines.len() - 1)] {
                        GridLine::Named(n) => GridLine::Named(n.clone()),
                        _ => GridLine::Auto,
                    }
                })
            };
            style.grid_row_start = get(0, 0);
            style.grid_column_start = get(1, 0);
            style.grid_row_end = get(2, 0);
            style.grid_column_end = get(3, 1);
        }
        "border-collapse" => style.border_collapse = BorderCollapse::parse(&kw(one(&v)?)?)?,
        "border-spacing" => {
            let a = length(v.first()?, &lc)?;
            let b = match v.get(1) {
                Some(c) => length(c, &lc)?,
                None => a,
            };
            style.border_spacing = (a, b);
        }
        "table-layout" => style.table_layout = TableLayout::parse(&kw(one(&v)?)?)?,
        "caption-side" => style.caption_side = CaptionSide::parse(&kw(one(&v)?)?)?,
        "empty-cells" => style.empty_cells_hide = kw(one(&v)?)? == "hide",
        "direction" => style.direction = Direction::parse(&kw(one(&v)?)?)?,
        "transform" => style.transform = transform_list(&v, &lc)?,
        "translate" | "scale" | "rotate" => {
            if v.len() == 1 && kw(v[0]).as_deref() == Some("none") {
                return Some(());
            }
            let t = match name {
                "translate" => Transform::Translate(
                    length_percentage(v.first()?, &lc)?,
                    v.get(1)
                        .and_then(|c| length_percentage(c, &lc))
                        .unwrap_or(LengthPercentage::ZERO),
                ),
                "scale" => {
                    let x = number(v.first()?)?;
                    Transform::Scale(x, v.get(1).and_then(|c| number(c)).unwrap_or(x))
                }
                _ => {
                    let l = transform_list(
                        &[&Cv::Function {
                            name: "rotate".into(),
                            args: value.to_vec(),
                        }],
                        &lc,
                    )?;
                    l.into_iter().next()?
                }
            };
            style.transform.insert(0, t);
        }
        "box-shadow" => style.box_shadow = shadow_list(value, &lc)?,
        "cursor" => {
            // `url(...) x y, pointer`: the keyword at the end.
            let last = non_ws(split_commas(value).last()?);
            style.cursor = Cursor::parse(&kw(last.first()?)?).unwrap_or(Cursor::Auto);
        }
        "pointer-events" => {
            style.pointer_events =
                PointerEvents::parse(&kw(one(&v)?)?).unwrap_or(PointerEvents::Auto)
        }
        "object-fit" => style.object_fit = ObjectFit::parse(&kw(one(&v)?)?)?,
        "user-select" => style.user_select_none = kw(one(&v)?)? == "none",
        "appearance" => style.appearance_none = kw(one(&v)?)? == "none",
        "column-count" => {
            style.column_count = if kw(one(&v)?).as_deref() == Some("auto") {
                None
            } else {
                Some(integer(one(&v)?)?.max(1) as u32)
            }
        }
        _ => {
            if let Some(sd) = name.strip_prefix("margin-").and_then(side) {
                style.margin[sd] = length_auto(one(&v)?, &lc)?;
            } else if let Some(sd) = name.strip_prefix("padding-").and_then(side) {
                style.padding[sd] = length_percentage(one(&v)?, &lc)?;
            } else if let Some(rest) = name.strip_prefix("border-")
                && let Some((sd, what)) = rest.rsplit_once('-')
                && let Some(s) = side(sd)
            {
                match what {
                    "width" => style.border_width[s] = border_width_kw(one(&v)?, &lc)?,
                    "style" => style.border_style[s] = BorderStyle::parse(&kw(one(&v)?)?)?,
                    "color" => style.border_color[s] = color(one(&v)?)?,
                    _ => return None,
                }
            } else if let Some(corner) = name
                .strip_prefix("border-")
                .and_then(|r| r.strip_suffix("-radius"))
            {
                let i = match corner {
                    "top-left" | "start-start" => 0,
                    "top-right" | "start-end" => 1,
                    "bottom-right" | "end-end" => 2,
                    "bottom-left" | "end-start" => 3,
                    _ => return None,
                };
                style.border_radius[i] = length_percentage(v.first()?, &lc)?;
            } else if ignored_property(name) {
                // accepted, no effect
            } else {
                return None;
            }
        }
    }
    Some(())
}

fn flex_basis(cv: &Cv, lc: &LengthContext) -> Option<FlexBasis> {
    match kw(cv).as_deref() {
        Some("auto") => Some(FlexBasis::Auto),
        Some("content" | "max-content" | "min-content" | "fit-content") => Some(FlexBasis::Content),
        _ => Some(FlexBasis::Lp(length_percentage(cv, lc)?)),
    }
}

fn area_row(s: &str) -> Vec<String> {
    s.split_ascii_whitespace()
        .map(|w| {
            if w.chars().all(|c| c == '.') {
                String::from(".")
            } else {
                w.to_string()
            }
        })
        .collect()
}
