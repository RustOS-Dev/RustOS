//! Character references: the named entities that occur in practice (the
//! full HTML list has 2000+; these cover Latin-1, typography and common
//! symbols), plus numeric references with the HTML replacement rules.

/// Named entities, sorted by name for binary search.
static NAMED: &[(&str, &str)] = &[
    ("AElig", "Æ"), ("Aacute", "Á"), ("Acirc", "Â"), ("Agrave", "À"), ("Alpha", "Α"),
    ("Aring", "Å"), ("Atilde", "Ã"), ("Auml", "Ä"), ("Beta", "Β"), ("Ccedil", "Ç"),
    ("Chi", "Χ"), ("Dagger", "‡"), ("Delta", "Δ"), ("ETH", "Ð"), ("Eacute", "É"),
    ("Ecirc", "Ê"), ("Egrave", "È"), ("Epsilon", "Ε"), ("Eta", "Η"), ("Euml", "Ë"),
    ("Gamma", "Γ"), ("Iacute", "Í"), ("Icirc", "Î"), ("Igrave", "Ì"), ("Iota", "Ι"),
    ("Iuml", "Ï"), ("Kappa", "Κ"), ("Lambda", "Λ"), ("Mu", "Μ"), ("Ntilde", "Ñ"),
    ("Nu", "Ν"), ("OElig", "Œ"), ("Oacute", "Ó"), ("Ocirc", "Ô"), ("Ograve", "Ò"),
    ("Omega", "Ω"), ("Omicron", "Ο"), ("Oslash", "Ø"), ("Otilde", "Õ"), ("Ouml", "Ö"),
    ("Phi", "Φ"), ("Pi", "Π"), ("Prime", "″"), ("Psi", "Ψ"), ("Rho", "Ρ"),
    ("Scaron", "Š"), ("Sigma", "Σ"), ("THORN", "Þ"), ("Tau", "Τ"), ("Theta", "Θ"),
    ("Uacute", "Ú"), ("Ucirc", "Û"), ("Ugrave", "Ù"), ("Upsilon", "Υ"), ("Uuml", "Ü"),
    ("Xi", "Ξ"), ("Yacute", "Ý"), ("Yuml", "Ÿ"), ("Zeta", "Ζ"), ("aacute", "á"),
    ("acirc", "â"), ("acute", "´"), ("aelig", "æ"), ("agrave", "à"), ("alpha", "α"),
    ("amp", "&"), ("and", "∧"), ("ang", "∠"), ("apos", "'"), ("aring", "å"),
    ("asymp", "≈"), ("atilde", "ã"), ("auml", "ä"), ("bdquo", "„"), ("beta", "β"),
    ("brvbar", "¦"), ("bull", "•"), ("cap", "∩"), ("ccedil", "ç"), ("cedil", "¸"),
    ("cent", "¢"), ("check", "✓"), ("chi", "χ"), ("circ", "ˆ"), ("clubs", "♣"),
    ("copy", "©"), ("crarr", "↵"), ("cup", "∪"), ("curren", "¤"), ("dArr", "⇓"),
    ("dagger", "†"), ("darr", "↓"), ("deg", "°"), ("delta", "δ"), ("diams", "♦"),
    ("divide", "÷"), ("eacute", "é"), ("ecirc", "ê"), ("egrave", "è"), ("empty", "∅"),
    ("emsp", "\u{2003}"), ("ensp", "\u{2002}"), ("epsilon", "ε"), ("equiv", "≡"),
    ("eta", "η"), ("eth", "ð"), ("euml", "ë"), ("euro", "€"), ("exist", "∃"),
    ("forall", "∀"), ("frac12", "½"), ("frac14", "¼"), ("frac34", "¾"), ("frasl", "⁄"),
    ("gamma", "γ"), ("ge", "≥"), ("gt", ">"), ("hArr", "⇔"), ("harr", "↔"),
    ("hearts", "♥"), ("hellip", "…"), ("iacute", "í"), ("icirc", "î"), ("iexcl", "¡"),
    ("igrave", "ì"), ("infin", "∞"), ("int", "∫"), ("iota", "ι"), ("iquest", "¿"),
    ("isin", "∈"), ("iuml", "ï"), ("kappa", "κ"), ("lArr", "⇐"), ("lambda", "λ"),
    ("lang", "⟨"), ("laquo", "«"), ("larr", "←"), ("lceil", "⌈"), ("ldquo", "“"),
    ("le", "≤"), ("lfloor", "⌊"), ("lowast", "∗"), ("loz", "◊"), ("lrm", "\u{200E}"),
    ("lsaquo", "‹"), ("lsquo", "‘"), ("lt", "<"), ("macr", "¯"), ("mdash", "—"),
    ("micro", "µ"), ("middot", "·"), ("minus", "−"), ("mu", "μ"), ("nabla", "∇"),
    ("nbsp", "\u{a0}"), ("ndash", "–"), ("ne", "≠"), ("ni", "∋"), ("not", "¬"),
    ("notin", "∉"), ("nsub", "⊄"), ("ntilde", "ñ"), ("nu", "ν"), ("oacute", "ó"),
    ("ocirc", "ô"), ("oelig", "œ"), ("ograve", "ò"), ("oline", "‾"), ("omega", "ω"),
    ("omicron", "ο"), ("oplus", "⊕"), ("or", "∨"), ("ordf", "ª"), ("ordm", "º"),
    ("oslash", "ø"), ("otilde", "õ"), ("otimes", "⊗"), ("ouml", "ö"), ("para", "¶"),
    ("part", "∂"), ("permil", "‰"), ("perp", "⊥"), ("phi", "φ"), ("pi", "π"),
    ("piv", "ϖ"), ("plusmn", "±"), ("pound", "£"), ("prime", "′"), ("prod", "∏"),
    ("prop", "∝"), ("psi", "ψ"), ("quot", "\""), ("rArr", "⇒"), ("radic", "√"),
    ("rang", "⟩"), ("raquo", "»"), ("rarr", "→"), ("rceil", "⌉"), ("rdquo", "”"),
    ("reg", "®"), ("rfloor", "⌋"), ("rho", "ρ"), ("rlm", "\u{200F}"), ("rsaquo", "›"),
    ("rsquo", "’"), ("sbquo", "‚"), ("scaron", "š"), ("sdot", "⋅"), ("sect", "§"),
    ("shy", "\u{ad}"), ("sigma", "σ"), ("sigmaf", "ς"), ("sim", "∼"), ("spades", "♠"),
    ("sub", "⊂"), ("sube", "⊆"), ("sum", "∑"), ("sup", "⊃"), ("sup1", "¹"),
    ("sup2", "²"), ("sup3", "³"), ("supe", "⊇"), ("szlig", "ß"), ("tau", "τ"),
    ("there4", "∴"), ("theta", "θ"), ("thetasym", "ϑ"), ("thinsp", "\u{2009}"),
    ("thorn", "þ"), ("tilde", "˜"), ("times", "×"), ("trade", "™"), ("uArr", "⇑"),
    ("uacute", "ú"), ("uarr", "↑"), ("ucirc", "û"), ("ugrave", "ù"), ("uml", "¨"),
    ("upsih", "ϒ"), ("upsilon", "υ"), ("uuml", "ü"), ("weierp", "℘"), ("xi", "ξ"),
    ("yacute", "ý"), ("yen", "¥"), ("yuml", "ÿ"), ("zeta", "ζ"), ("zwj", "\u{200D}"),
    ("zwnj", "\u{200C}"),
];

pub fn named(name: &str) -> Option<&'static str> {
    NAMED
        .binary_search_by(|(n, _)| (*n).cmp(name))
        .ok()
        .map(|i| NAMED[i].1)
}

/// Code point for `&#N;`, with the HTML fix-ups (C1 controls map to
/// windows-1252, invalid values to U+FFFD).
pub fn numeric(n: u32) -> char {
    const C1: [u16; 32] = [
        0x20AC, 0x81, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160,
        0x2039, 0x0152, 0x8D, 0x017D, 0x8F, 0x90, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022,
        0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x9D, 0x017E, 0x0178,
    ];
    match n {
        0 => '\u{FFFD}',
        0x80..=0x9F => char::from_u32(C1[(n - 0x80) as usize] as u32).unwrap_or('\u{FFFD}'),
        _ => char::from_u32(n).unwrap_or('\u{FFFD}'),
    }
}

#[cfg(test)]
#[test]
fn table_is_sorted() {
    for w in NAMED.windows(2) {
        assert!(w[0].0 < w[1].0, "{} >= {}", w[0].0, w[1].0);
    }
}
