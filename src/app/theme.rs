//! Editor colors and the highlight-name vocabulary.
//!
//! One vocabulary of dotted highlight names serves every styled-text source:
//! syntax captures, contribution decorations, and later semantic tokens. A
//! source name resolves to the vocabulary entry with the longest dotted prefix
//! (`function.method.call` → `function.method`), and names with no such prefix
//! are ignored. The `Theme` resolves each vocabulary entry to a style the same
//! way, so a theme may style `function` alone and have `function.method`
//! follow it.

use gpui::{
    FontStyle, FontWeight, Global, HighlightStyle, Hsla, StrikethroughStyle, UnderlineStyle, hsla,
    px, rgb,
};

/// Every highlight name a styled-text source can produce.
pub(crate) const HIGHLIGHT_NAMES: &[&str] = &[
    "attribute",
    "comment",
    "comment.doc",
    "constant",
    "constant.builtin",
    "diagnostic.error",
    "diagnostic.info",
    "diagnostic.warning",
    "function",
    "function.macro",
    "function.method",
    "keyword",
    "label",
    "markup.heading",
    "markup.italic",
    "markup.link",
    "markup.link.text",
    "markup.link.url",
    "markup.list",
    "markup.quote",
    "markup.raw",
    "markup.strikethrough",
    "markup.strong",
    "namespace",
    "number",
    "operator",
    "property",
    "punctuation.bracket",
    "punctuation.delimiter",
    "string",
    "string.escape",
    "tag",
    "type",
    "type.builtin",
    "variable",
    "variable.builtin",
    "variable.parameter",
];

/// One entry of [`HIGHLIGHT_NAMES`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Highlight(u16);

impl Highlight {
    /// The vocabulary entry with the longest dotted prefix of `name`.
    pub(crate) fn resolve(name: &str) -> Option<Self> {
        dotted_prefixes(name).find_map(|prefix| {
            HIGHLIGHT_NAMES
                .iter()
                .position(|known| *known == prefix)
                .map(|index| Self(index as u16))
        })
    }

    /// The vocabulary entry named exactly `name`, which must exist.
    pub(crate) fn named(name: &str) -> Self {
        let highlight = Self::resolve(name).filter(|highlight| highlight.name() == name);
        highlight.unwrap_or_else(|| panic!("{name} is not a highlight name"))
    }

    pub(crate) fn name(self) -> &'static str {
        HIGHLIGHT_NAMES[self.0 as usize]
    }
}

/// `name`, then each shorter prefix ending before a `.`.
fn dotted_prefixes(name: &str) -> impl Iterator<Item = &str> {
    std::iter::successors(Some(name), |prefix| {
        prefix.rfind('.').map(|dot| &prefix[..dot])
    })
}

/// Editor surface colors and a resolved style for every highlight name.
pub(crate) struct Theme {
    pub(crate) background: Hsla,
    pub(crate) foreground: Hsla,
    pub(crate) selection: Hsla,
    pub(crate) caret: Hsla,
    pub(crate) marked_text: Hsla,
    pub(crate) scrollbar: Hsla,
    /// Indexed by `Highlight`.
    highlights: Vec<HighlightStyle>,
}

impl Global for Theme {}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

impl Theme {
    /// The built-in dark palette.
    pub(crate) fn dark() -> Self {
        let color = |hex: u32| HighlightStyle::color(rgb(hex).into());
        let bold = |hex: u32| HighlightStyle {
            font_weight: Some(FontWeight::BOLD),
            ..color(hex)
        };
        let italic = |hex: u32| HighlightStyle {
            font_style: Some(FontStyle::Italic),
            ..color(hex)
        };
        let wavy = |hex: u32| HighlightStyle {
            underline: Some(UnderlineStyle {
                thickness: px(1.5),
                color: Some(rgb(hex).into()),
                wavy: true,
            }),
            ..Default::default()
        };
        Self::with_styles(
            Self {
                background: rgb(0x1e1e1e).into(),
                foreground: rgb(0xd4d4d4).into(),
                selection: hsla(0.6, 0.7, 0.55, 0.35),
                caret: hsla(0., 0., 0.9, 1.0),
                marked_text: hsla(0.0, 0.0, 0.7, 0.8),
                scrollbar: hsla(0., 0., 0.6, 0.4),
                highlights: Vec::new(),
            },
            &[
                ("attribute", color(0x9cdcfe)),
                ("comment", italic(0x6a9955)),
                ("constant", color(0x4fc1ff)),
                ("constant.builtin", color(0x569cd6)),
                ("diagnostic.error", wavy(0xf48771)),
                ("diagnostic.info", wavy(0x6cb6ff)),
                ("diagnostic.warning", wavy(0xe2c08d)),
                ("function", color(0xdcdcaa)),
                ("function.macro", color(0x569cd6)),
                ("keyword", color(0x569cd6)),
                ("label", color(0xc8c8c8)),
                ("markup.heading", bold(0x569cd6)),
                (
                    "markup.italic",
                    HighlightStyle {
                        font_style: Some(FontStyle::Italic),
                        ..Default::default()
                    },
                ),
                ("markup.link", color(0x9cdcfe)),
                ("markup.link.url", color(0xce9178)),
                ("markup.list", color(0x6796e6)),
                ("markup.quote", italic(0x6a9955)),
                ("markup.raw", color(0xce9178)),
                (
                    "markup.strikethrough",
                    HighlightStyle {
                        strikethrough: Some(StrikethroughStyle {
                            thickness: px(1.),
                            color: None,
                        }),
                        ..Default::default()
                    },
                ),
                (
                    "markup.strong",
                    HighlightStyle {
                        font_weight: Some(FontWeight::BOLD),
                        ..Default::default()
                    },
                ),
                ("namespace", color(0x4ec9b0)),
                ("number", color(0xb5cea8)),
                ("property", color(0x9cdcfe)),
                ("string", color(0xce9178)),
                ("string.escape", color(0xd7ba7d)),
                ("tag", color(0x569cd6)),
                ("type", color(0x4ec9b0)),
                ("variable", color(0x9cdcfe)),
                ("variable.builtin", color(0x569cd6)),
            ],
        )
    }

    /// Resolve every vocabulary entry against `styles` by longest dotted
    /// prefix; entries with no styled prefix keep the default style.
    fn with_styles(mut theme: Self, styles: &[(&str, HighlightStyle)]) -> Self {
        theme.highlights = HIGHLIGHT_NAMES
            .iter()
            .map(|name| {
                dotted_prefixes(name)
                    .find_map(|prefix| {
                        styles
                            .iter()
                            .find(|(styled, _)| *styled == prefix)
                            .map(|(_, style)| *style)
                    })
                    .unwrap_or_default()
            })
            .collect();
        theme
    }

    pub(crate) fn style(&self, highlight: Highlight) -> HighlightStyle {
        self.highlights[highlight.0 as usize]
    }

    /// The underline color of `highlight`, used where a diagnostic marks a
    /// whole line rather than a span.
    pub(crate) fn underline_color(&self, highlight: Highlight) -> Hsla {
        self.style(highlight)
            .underline
            .and_then(|underline| underline.color)
            .unwrap_or(self.foreground)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_resolve_to_the_longest_vocabulary_prefix() {
        let name = |name| Highlight::resolve(name).map(Highlight::name);
        assert_eq!(name("function.method.call"), Some("function.method"));
        assert_eq!(name("function.method"), Some("function.method"));
        assert_eq!(name("function.builtin"), Some("function"));
        assert_eq!(name("keyword.control.flow"), Some("keyword"));
        assert_eq!(name("punctuation.special"), None);
        assert_eq!(name("embedded"), None);
        assert_eq!(name(""), None);
    }

    #[test]
    fn vocabulary_is_sorted_and_unique() {
        assert!(HIGHLIGHT_NAMES.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn theme_styles_follow_the_longest_styled_prefix() {
        let theme = Theme::with_styles(
            Theme::dark(),
            &[
                ("function", HighlightStyle::color(rgb(0x111111).into())),
                (
                    "function.macro",
                    HighlightStyle::color(rgb(0x222222).into()),
                ),
            ],
        );
        let color = |name| theme.style(Highlight::named(name)).color;
        assert_eq!(color("function.method"), Some(rgb(0x111111).into()));
        assert_eq!(color("function.macro"), Some(rgb(0x222222).into()));
        assert_eq!(color("keyword"), None);
    }

    #[test]
    #[should_panic(expected = "function.call is not a highlight name")]
    fn named_requires_an_exact_vocabulary_entry() {
        Highlight::named("function.call");
    }
}
