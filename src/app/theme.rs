//! Editor colors and the highlight-name vocabulary.
//!
//! Capture names follow nvim-treesitter's standard captures, which upstream
//! grammar queries also use, so adapted queries keep their names. A source
//! name resolves to the capture name with the longest dotted prefix
//! (`function.method.call.deep` → `function.method.call`), and names with no
//! such prefix are ignored. Decoration names are separate: only native
//! decorations produce them, so a query can never draw a diagnostic. The
//! `Theme` resolves every name to a style the same way, so a theme may style
//! `function` alone and have `function.method` follow it.

use gpui::{
    FontStyle, FontWeight, Global, HighlightStyle, Hsla, StrikethroughStyle, UnderlineStyle, hsla,
    px, rgb,
};

/// Every name a syntax capture or semantic token can resolve to.
pub(crate) const CAPTURE_NAMES: &[&str] = &[
    "attribute",
    "attribute.builtin",
    "boolean",
    "character",
    "character.special",
    "comment",
    "comment.documentation",
    "comment.error",
    "comment.note",
    "comment.todo",
    "comment.warning",
    "constant",
    "constant.builtin",
    "constant.macro",
    "constructor",
    "diff.delta",
    "diff.minus",
    "diff.plus",
    "function",
    "function.builtin",
    "function.call",
    "function.macro",
    "function.method",
    "function.method.call",
    "keyword",
    "keyword.conditional",
    "keyword.conditional.ternary",
    "keyword.coroutine",
    "keyword.debug",
    "keyword.directive",
    "keyword.directive.define",
    "keyword.exception",
    "keyword.function",
    "keyword.import",
    "keyword.modifier",
    "keyword.operator",
    "keyword.repeat",
    "keyword.return",
    "keyword.type",
    "label",
    "markup.heading",
    "markup.heading.1",
    "markup.heading.2",
    "markup.heading.3",
    "markup.heading.4",
    "markup.heading.5",
    "markup.heading.6",
    "markup.italic",
    "markup.link",
    "markup.link.label",
    "markup.link.url",
    "markup.list",
    "markup.list.checked",
    "markup.list.unchecked",
    "markup.math",
    "markup.quote",
    "markup.raw",
    "markup.raw.block",
    "markup.strikethrough",
    "markup.strong",
    "markup.underline",
    "module",
    "module.builtin",
    "number",
    "number.float",
    "operator",
    "property",
    "punctuation.bracket",
    "punctuation.delimiter",
    "punctuation.special",
    "string",
    "string.documentation",
    "string.escape",
    "string.regexp",
    "string.special",
    "string.special.path",
    "string.special.symbol",
    "string.special.url",
    "tag",
    "tag.attribute",
    "tag.builtin",
    "tag.delimiter",
    "type",
    "type.builtin",
    "type.definition",
    "variable",
    "variable.builtin",
    "variable.member",
    "variable.parameter",
    "variable.parameter.builtin",
];

/// Names only native decorations produce.
const DECORATION_NAMES: &[&str] = &["diagnostic.error", "diagnostic.info", "diagnostic.warning"];

/// One capture or decoration name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Highlight(u16);

impl Highlight {
    /// The capture name with the longest dotted prefix of `name`.
    // No syntax source resolves captures yet.
    #[allow(dead_code)]
    pub(crate) fn resolve(name: &str) -> Option<Self> {
        dotted_prefixes(name).find_map(|prefix| {
            CAPTURE_NAMES
                .iter()
                .position(|known| *known == prefix)
                .map(|index| Self(index as u16))
        })
    }

    /// The capture or decoration name `name`, which must exist exactly.
    pub(crate) fn named(name: &str) -> Self {
        all_names()
            .position(|known| known == name)
            .map(|index| Self(index as u16))
            .unwrap_or_else(|| panic!("{name} is not a highlight name"))
    }

    #[cfg(test)]
    pub(crate) fn name(self) -> &'static str {
        all_names().nth(self.0 as usize).unwrap()
    }
}

/// Capture names followed by decoration names, in `Highlight` index order.
fn all_names() -> impl Iterator<Item = &'static str> {
    CAPTURE_NAMES.iter().chain(DECORATION_NAMES).copied()
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
                ("boolean", color(0x569cd6)),
                ("character", color(0xce9178)),
                ("character.special", color(0xd7ba7d)),
                ("comment", italic(0x6a9955)),
                ("comment.error", bold(0xf48771)),
                ("comment.note", bold(0x6cb6ff)),
                ("comment.todo", bold(0xe2c08d)),
                ("comment.warning", bold(0xe2c08d)),
                ("constant", color(0x4fc1ff)),
                ("constant.builtin", color(0x569cd6)),
                ("constructor", color(0x4ec9b0)),
                ("diagnostic.error", wavy(0xf48771)),
                ("diagnostic.info", wavy(0x6cb6ff)),
                ("diagnostic.warning", wavy(0xe2c08d)),
                ("diff.delta", color(0xe2c08d)),
                ("diff.minus", color(0xf48771)),
                ("diff.plus", color(0x81b88b)),
                ("function", color(0xdcdcaa)),
                ("function.macro", color(0x569cd6)),
                ("keyword", color(0x569cd6)),
                ("keyword.conditional", color(0xc586c0)),
                ("keyword.coroutine", color(0xc586c0)),
                ("keyword.exception", color(0xc586c0)),
                ("keyword.import", color(0xc586c0)),
                ("keyword.repeat", color(0xc586c0)),
                ("keyword.return", color(0xc586c0)),
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
                (
                    "markup.underline",
                    HighlightStyle {
                        underline: Some(UnderlineStyle {
                            thickness: px(1.),
                            color: None,
                            wavy: false,
                        }),
                        ..Default::default()
                    },
                ),
                ("module", color(0x4ec9b0)),
                ("number", color(0xb5cea8)),
                ("property", color(0x9cdcfe)),
                ("punctuation.special", color(0x569cd6)),
                ("string", color(0xce9178)),
                ("string.escape", color(0xd7ba7d)),
                ("string.regexp", color(0xd16969)),
                ("string.special", color(0xd7ba7d)),
                ("tag", color(0x569cd6)),
                ("tag.attribute", color(0x9cdcfe)),
                ("tag.delimiter", color(0x808080)),
                ("type", color(0x4ec9b0)),
                ("variable", color(0x9cdcfe)),
                ("variable.builtin", color(0x569cd6)),
            ],
        )
    }

    /// Resolve every capture and decoration name against `styles` by longest
    /// dotted prefix; names with no styled prefix keep the default style.
    fn with_styles(mut theme: Self, styles: &[(&str, HighlightStyle)]) -> Self {
        theme.highlights = all_names()
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
    fn names_resolve_to_the_longest_capture_prefix() {
        let name = |name| Highlight::resolve(name).map(Highlight::name);
        assert_eq!(
            name("function.method.call.deep"),
            Some("function.method.call")
        );
        assert_eq!(name("function.method"), Some("function.method"));
        assert_eq!(name("tag.attribute"), Some("tag.attribute"));
        assert_eq!(name("keyword.control.flow"), Some("keyword"));
        assert_eq!(name("spell"), None);
        assert_eq!(name(""), None);
    }

    #[test]
    fn captures_never_resolve_to_decorations() {
        assert_eq!(Highlight::resolve("diagnostic.error"), None);
        assert_eq!(
            Highlight::named("diagnostic.error").name(),
            "diagnostic.error"
        );
    }

    #[test]
    fn names_are_sorted_and_unique() {
        for names in [CAPTURE_NAMES, DECORATION_NAMES] {
            assert!(names.windows(2).all(|pair| pair[0] < pair[1]));
        }
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
    #[should_panic(expected = "function.calls is not a highlight name")]
    fn named_requires_an_exact_vocabulary_entry() {
        Highlight::named("function.calls");
    }
}
