//! Theme colors and the highlight-name vocabulary.
//!
//! Capture names follow nvim-treesitter's standard captures, which upstream
//! grammar queries also use, so adapted queries keep their names. A source
//! name resolves to the capture name with the longest dotted prefix
//! (`function.method.call.deep` → `function.method.call`), and names with no
//! such prefix are ignored. Decoration names are separate: only native
//! decorations produce them, so a query can never draw a diagnostic. The
//! `Theme` resolves every name to a style the same way, so a theme may style
//! `function` alone and have `function.method` follow it.
//!
//! UI colors are named by flat dotted keys. A color definition gives a key a
//! default per appearance, either a color or another key; native surfaces
//! paint only with keys from the native table here. A theme's values win
//! over the defaults, and with no theme installed the dark defaults apply.

use std::{collections::HashMap, sync::LazyLock};

use gpui::{
    App, FontStyle, FontWeight, Global, HighlightStyle, Hsla, StrikethroughStyle, UnderlineStyle,
    px, rgb, rgba,
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
    pub(crate) fn resolve(name: &str) -> Option<Self> {
        dotted_prefixes(name).find_map(|prefix| {
            CAPTURE_NAMES
                .iter()
                .position(|known| *known == prefix)
                .map(|index| Self(index as u16))
        })
    }

    /// Whether a theme may style `name`: a capture name or a dotted prefix
    /// of one, such as `markup` for every `markup.*` capture.
    pub(crate) fn styles_captures(name: &str) -> bool {
        CAPTURE_NAMES.iter().any(|capture| {
            capture
                .strip_prefix(name)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
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

/// A color definition's default for one appearance.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ColorDefault {
    Color(Hsla),
    /// The resolved color of another key.
    Key(String),
}

/// A color key with a description and a default per appearance.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ColorDefinition {
    pub(crate) key: String,
    pub(crate) description: String,
    pub(crate) dark: ColorDefault,
    pub(crate) light: ColorDefault,
}

impl ColorDefinition {
    fn default_for(&self, appearance: Appearance) -> &ColorDefault {
        match appearance {
            Appearance::Dark => &self.dark,
            Appearance::Light => &self.light,
        }
    }
}

enum NativeDefault {
    Rgb(u32),
    Rgba(u32),
    Key(&'static str),
}

impl NativeDefault {
    fn definition(&self) -> ColorDefault {
        match self {
            Self::Rgb(color) => ColorDefault::Color(rgb(*color).into()),
            Self::Rgba(color) => ColorDefault::Color(rgba(*color).into()),
            Self::Key(key) => ColorDefault::Key((*key).to_owned()),
        }
    }
}

macro_rules! native_colors {
    ($($name:ident = $key:literal, $description:literal, $dark:expr, $light:expr;)*) => {
        /// Color keys native surfaces paint with.
        pub(crate) mod color {
            $(pub(crate) const $name: &str = $key;)*
        }

        fn native_color_table() -> Vec<(&'static str, &'static str, NativeDefault, NativeDefault)> {
            use NativeDefault::{Key, Rgb, Rgba};
            use color::*;
            vec![$(($key, $description, $dark, $light),)*]
        }
    };
}

native_colors! {
    BACKGROUND = "background", "Window and pane background.", Rgb(0x1e1e1e), Rgb(0xffffff);
    BORDER = "border", "Dividers and panel borders.", Rgb(0x454545), Rgb(0xd4d4d4);
    BORDER_HOVER = "border.hover", "A resizable divider under the pointer.", Rgb(0x7fb5ff), Rgb(0x3b82d6);
    BUTTON_BACKGROUND = "button.background", "Inline action buttons.", Rgb(0x2d2d2d), Rgb(0xececec);
    BUTTON_HOVER_BACKGROUND = "button.hover.background", "An inline action button under the pointer.", Rgb(0x3a3a3a), Rgb(0xdcdcdc);
    DROP_TARGET_BACKGROUND = "dropTarget.background", "Where a dragged tab would land.", Rgba(0x7fb5ff55), Rgba(0x3b82d640);
    EDITOR_BACKGROUND = "editor.background", "Text editor background.", Key(BACKGROUND), Key(BACKGROUND);
    EDITOR_CURSOR = "editor.cursor", "Text cursor.", Rgb(0xe6e6e6), Rgb(0x1f1f1f);
    EDITOR_FOREGROUND = "editor.foreground", "Unhighlighted editor text.", Rgb(0xd4d4d4), Rgb(0x1f1f1f);
    EDITOR_MARKED_TEXT = "editor.markedText", "Underline of text being composed by an input method.", Rgba(0xb3b3b3cc), Rgba(0x595959cc);
    EDITOR_SELECTION = "editor.selection", "Selected text.", Rgba(0x3c7cdd59), Rgba(0x3b82d640);
    ERROR = "error", "Errors and failures.", Rgb(0xf48771), Rgb(0xc4314b);
    INFO = "info", "Informational markers.", Rgb(0x6cb6ff), Rgb(0x1a73c7);
    LIST_GROUP_BACKGROUND = "list.group.background", "Group headers in trees.", Rgb(0x262626), Rgb(0xf0f0f0);
    LIST_HOVER_BACKGROUND = "list.hover.background", "A list or tree row under the pointer.", Rgb(0x2a2a2a), Rgb(0xebebeb);
    LIST_SELECTED_BACKGROUND = "list.selected.background", "The selected list or tree row.", Rgb(0x2a4a7a), Rgb(0xcce0f7);
    LIST_SELECTED_FOREGROUND = "list.selected.foreground", "Text of the selected list or tree row.", Rgb(0xffffff), Key(TEXT);
    POPOVER_BACKGROUND = "popover.background", "Pickers, the command palette, completion, and reports.", Rgb(0x252526), Rgb(0xf8f8f8);
    POPOVER_BORDER = "popover.border", "Border of popovers.", Rgb(0x555555), Rgb(0xc8c8c8);
    SCRIM = "scrim", "Dims the window behind a modal surface.", Rgba(0x00000080), Rgba(0x00000040);
    SCROLLBAR_THUMB = "scrollbar.thumb", "Scrollbar thumb.", Rgba(0x99999966), Rgba(0x64646466);
    SUCCESS = "success", "Success markers.", Rgb(0x8fd18f), Rgb(0x2e8540);
    TAB_ACTIVE_BACKGROUND = "tab.active.background", "The active tab.", Key(EDITOR_BACKGROUND), Key(EDITOR_BACKGROUND);
    TAB_ACTIVE_FOREGROUND = "tab.active.foreground", "Title of the active tab.", Rgb(0xf0f0f0), Key(TEXT);
    TAB_BAR_BACKGROUND = "tab.bar.background", "The tab bar behind the tabs.", Rgb(0x2d2d2d), Rgb(0xf0f0f0);
    TAB_INACTIVE_BACKGROUND = "tab.inactive.background", "Inactive tabs.", Key(TAB_BAR_BACKGROUND), Key(TAB_BAR_BACKGROUND);
    TAB_INACTIVE_FOREGROUND = "tab.inactive.foreground", "Titles of inactive tabs.", Rgb(0xaaaaaa), Key(TEXT_MUTED);
    TERMINAL_ANSI_BLACK = "terminal.ansi.black", "ANSI black.", Rgb(0x000000), Rgb(0x000000);
    TERMINAL_ANSI_BLUE = "terminal.ansi.blue", "ANSI blue.", Rgb(0x2472c8), Rgb(0x0451a5);
    TERMINAL_ANSI_BRIGHT_BLACK = "terminal.ansi.brightBlack", "ANSI bright black.", Rgb(0x666666), Rgb(0x666666);
    TERMINAL_ANSI_BRIGHT_BLUE = "terminal.ansi.brightBlue", "ANSI bright blue.", Rgb(0x3b8eea), Rgb(0x0451a5);
    TERMINAL_ANSI_BRIGHT_CYAN = "terminal.ansi.brightCyan", "ANSI bright cyan.", Rgb(0x29b8db), Rgb(0x0598bc);
    TERMINAL_ANSI_BRIGHT_GREEN = "terminal.ansi.brightGreen", "ANSI bright green.", Rgb(0x23d18b), Rgb(0x14ce14);
    TERMINAL_ANSI_BRIGHT_MAGENTA = "terminal.ansi.brightMagenta", "ANSI bright magenta.", Rgb(0xd670d6), Rgb(0xbc05bc);
    TERMINAL_ANSI_BRIGHT_RED = "terminal.ansi.brightRed", "ANSI bright red.", Rgb(0xf14c4c), Rgb(0xcd3131);
    TERMINAL_ANSI_BRIGHT_WHITE = "terminal.ansi.brightWhite", "ANSI bright white.", Rgb(0xffffff), Rgb(0xa5a5a5);
    TERMINAL_ANSI_BRIGHT_YELLOW = "terminal.ansi.brightYellow", "ANSI bright yellow.", Rgb(0xf5f543), Rgb(0xb5ba00);
    TERMINAL_ANSI_CYAN = "terminal.ansi.cyan", "ANSI cyan.", Rgb(0x11a8cd), Rgb(0x0598bc);
    TERMINAL_ANSI_GREEN = "terminal.ansi.green", "ANSI green.", Rgb(0x0dbc79), Rgb(0x00bc00);
    TERMINAL_ANSI_MAGENTA = "terminal.ansi.magenta", "ANSI magenta.", Rgb(0xbc3fbc), Rgb(0xbc05bc);
    TERMINAL_ANSI_RED = "terminal.ansi.red", "ANSI red.", Rgb(0xcd3131), Rgb(0xcd3131);
    TERMINAL_ANSI_WHITE = "terminal.ansi.white", "ANSI white.", Rgb(0xe5e5e5), Rgb(0x555555);
    TERMINAL_ANSI_YELLOW = "terminal.ansi.yellow", "ANSI yellow.", Rgb(0xe5e510), Rgb(0x949800);
    TERMINAL_BACKGROUND = "terminal.background", "Terminal background and default cell background.", Rgb(0x181818), Key(BACKGROUND);
    TERMINAL_CURSOR = "terminal.cursor", "Terminal cursor.", Key(TERMINAL_FOREGROUND), Key(TERMINAL_FOREGROUND);
    TERMINAL_FOREGROUND = "terminal.foreground", "Default terminal text.", Key(EDITOR_FOREGROUND), Key(EDITOR_FOREGROUND);
    TERMINAL_SELECTION = "terminal.selection", "Selected terminal cells.", Rgb(0x355a88), Rgb(0xadd6ff);
    TEXT = "text", "Default interface text.", Rgb(0xe0e0e0), Rgb(0x1f1f1f);
    TEXT_ACCENT = "text.accent", "Interface text that acts, such as buttons and links.", Rgb(0x80c0ff), Rgb(0x1a73c7);
    TEXT_MUTED = "text.muted", "Secondary interface text.", Rgb(0x999999), Rgb(0x6e6e6e);
    WARNING = "warning", "Warnings.", Rgb(0xe2c08d), Rgb(0x9a6700);
}

/// The definitions of every native color key.
pub(crate) fn native_color_definitions() -> Vec<ColorDefinition> {
    native_color_table()
        .into_iter()
        .map(|(key, description, dark, light)| ColorDefinition {
            key: key.to_owned(),
            description: description.to_owned(),
            dark: dark.definition(),
            light: light.definition(),
        })
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Appearance {
    Dark,
    Light,
}

/// A `#rrggbb` or `#rrggbbaa` color.
pub(crate) fn parse_color(text: &str) -> Option<Hsla> {
    let digits = text.strip_prefix('#')?;
    if !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let value = u32::from_str_radix(digits, 16).ok()?;
    match digits.len() {
        6 => Some(rgb(value).into()),
        8 => Some(rgba(value).into()),
        _ => None,
    }
}

/// Resolved colors by key and a resolved style for every highlight name.
pub(crate) struct Theme {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) appearance: Appearance,
    colors: HashMap<String, Hsla>,
    /// Indexed by `Highlight`.
    highlights: Vec<HighlightStyle>,
}

impl Global for Theme {}

impl Default for Theme {
    fn default() -> Self {
        Self::defaults(Appearance::Dark, &native_color_definitions())
    }
}

/// Styles every theme shares for markup that carries no color; a theme's own
/// styles for these names win.
pub(crate) fn markup_styles() -> [(&'static str, HighlightStyle); 4] {
    [
        (
            "markup.italic",
            HighlightStyle {
                font_style: Some(FontStyle::Italic),
                ..Default::default()
            },
        ),
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
    ]
}

impl Theme {
    /// Color defaults and a minimal syntax palette, used when no registered
    /// theme applies.
    pub(crate) fn defaults(appearance: Appearance, definitions: &[ColorDefinition]) -> Self {
        let syntax: [(&str, u32); 8] = match appearance {
            Appearance::Dark => [
                ("comment", 0x6a9955),
                ("constant", 0x4fc1ff),
                ("function", 0xdcdcaa),
                ("keyword", 0x569cd6),
                ("markup.heading", 0x569cd6),
                ("number", 0xb5cea8),
                ("string", 0xce9178),
                ("type", 0x4ec9b0),
            ],
            Appearance::Light => [
                ("comment", 0x008000),
                ("constant", 0x0070c1),
                ("function", 0x795e26),
                ("keyword", 0x0000ff),
                ("markup.heading", 0x0000ff),
                ("number", 0x098658),
                ("string", 0xa31515),
                ("type", 0x267f99),
            ],
        };
        let syntax = syntax
            .into_iter()
            .map(|(name, color)| (name, HighlightStyle::color(rgb(color).into())))
            .chain(markup_styles())
            .collect::<Vec<_>>();
        Self::new(appearance, &HashMap::new(), definitions, &syntax)
    }

    /// A theme whose `values` win over each definition's default for
    /// `appearance`, whose capture styles resolve against `syntax` by longest
    /// dotted prefix, and whose decorations take the status colors.
    pub(crate) fn new(
        appearance: Appearance,
        values: &HashMap<String, Hsla>,
        definitions: &[ColorDefinition],
        syntax: &[(&str, HighlightStyle)],
    ) -> Self {
        let colors = resolve_colors(appearance, values, definitions);
        let wavy = |key: &str| HighlightStyle {
            underline: Some(UnderlineStyle {
                thickness: px(1.5),
                color: colors.get(key).copied(),
                wavy: true,
            }),
            ..Default::default()
        };
        let decorations = [
            ("diagnostic.error", wavy(color::ERROR)),
            ("diagnostic.info", wavy(color::INFO)),
            ("diagnostic.warning", wavy(color::WARNING)),
        ];
        let styles = syntax
            .iter()
            .copied()
            .chain(decorations)
            .collect::<Vec<_>>();
        let highlights = all_names()
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
        Self {
            appearance,
            colors,
            highlights,
        }
    }

    /// The resolved color of `key`; an undefined key paints as interface
    /// text, since its definition may arrive later.
    pub(crate) fn color(&self, key: &str) -> Hsla {
        self.colors
            .get(key)
            .or_else(|| self.colors.get(color::TEXT))
            .copied()
            .unwrap_or_default()
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
            .unwrap_or_else(|| self.color(color::EDITOR_FOREGROUND))
    }
}

/// Each defined key's color: its value in `values`, else its default for
/// `appearance`. A default naming an undefined key, or a reference cycle,
/// leaves the key unresolved.
fn resolve_colors(
    appearance: Appearance,
    values: &HashMap<String, Hsla>,
    definitions: &[ColorDefinition],
) -> HashMap<String, Hsla> {
    let definitions = definitions
        .iter()
        .map(|definition| (definition.key.as_str(), definition))
        .collect::<HashMap<_, _>>();
    let resolve = |key: &str| {
        let mut key = key;
        for _ in 0..=definitions.len() {
            if let Some(value) = values.get(key) {
                return Some(*value);
            }
            match definitions.get(key)?.default_for(appearance) {
                ColorDefault::Color(color) => return Some(*color),
                ColorDefault::Key(next) => key = next,
            }
        }
        None
    };
    definitions
        .keys()
        .filter_map(|key| Some(((*key).to_owned(), resolve(key)?)))
        .collect()
}

/// The application theme: the installed global, else the dark defaults.
pub(crate) fn theme(cx: &App) -> &Theme {
    static DEFAULTS: LazyLock<Theme> = LazyLock::new(Theme::default);
    cx.try_global::<Theme>().unwrap_or(&DEFAULTS)
}

/// Replace the application theme and repaint every window.
pub(crate) fn set_theme(theme: Theme, cx: &mut App) {
    cx.set_global(theme);
    cx.refresh_windows();
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

    /// The in-memory extension test host checks queries against the same names.
    #[test]
    fn fake_host_lists_every_capture_name() {
        let fake = include_str!("../../scripts/extensions/testing/host.ts")
            .split("export const CAPTURE_NAMES: readonly string[] = [")
            .nth(1)
            .unwrap()
            .split("];")
            .next()
            .unwrap()
            .split('"')
            .skip(1)
            .step_by(2)
            .collect::<Vec<_>>();
        assert_eq!(fake, CAPTURE_NAMES);
    }

    #[test]
    fn themes_style_capture_names_and_their_prefixes() {
        for name in ["markup", "markup.heading", "function.method.call", "type"] {
            assert!(Highlight::styles_captures(name), "{name}");
        }
        for name in ["", "mark", "function.method.call.deep", "diagnostic.error"] {
            assert!(!Highlight::styles_captures(name), "{name}");
        }
    }

    /// The declarations and the in-memory extension test host list the
    /// native color keys the table defines.
    #[test]
    fn declarations_and_fake_host_list_every_native_color_key() {
        let mut native = native_color_definitions()
            .into_iter()
            .map(|definition| definition.key)
            .collect::<Vec<_>>();
        native.sort_unstable();
        let fake = include_str!("../../scripts/extensions/testing/host.ts")
            .split("export const NATIVE_COLOR_KEYS: readonly string[] = [")
            .nth(1)
            .unwrap()
            .split("];")
            .next()
            .unwrap()
            .split('"')
            .skip(1)
            .step_by(2)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        assert_eq!(fake, native);
        let declared = include_str!("../host/js/knot.d.ts")
            .split("export interface ThemeColors {")
            .nth(1)
            .unwrap()
            .split("\n  }")
            .next()
            .unwrap()
            .lines()
            .filter_map(|line| line.trim().strip_prefix("readonly \""))
            .filter_map(|line| line.split('"').next())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        assert_eq!(declared, native);
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
        let theme = Theme::new(
            Appearance::Dark,
            &HashMap::new(),
            &native_color_definitions(),
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
    fn native_keys_are_unique_and_every_default_resolves() {
        let definitions = native_color_definitions();
        let mut keys = definitions
            .iter()
            .map(|definition| definition.key.as_str())
            .collect::<Vec<_>>();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), definitions.len());
        for appearance in [Appearance::Dark, Appearance::Light] {
            let colors = resolve_colors(appearance, &HashMap::new(), &definitions);
            for key in &keys {
                assert!(
                    colors.contains_key(*key),
                    "{key} has no {appearance:?} default"
                );
            }
        }
    }

    #[test]
    fn theme_values_win_over_defaults_and_references_follow_them() {
        let values = HashMap::from([(color::BACKGROUND.to_owned(), rgb(0x123456).into())]);
        let theme = Theme::new(Appearance::Dark, &values, &native_color_definitions(), &[]);
        assert_eq!(theme.color(color::BACKGROUND), rgb(0x123456).into());
        // editor.background defaults to background.
        assert_eq!(theme.color(color::EDITOR_BACKGROUND), rgb(0x123456).into());
        assert_eq!(theme.color("undefined.key"), theme.color(color::TEXT));
    }

    #[test]
    fn colors_parse_from_six_or_eight_hex_digits() {
        assert_eq!(parse_color("#1e1e1e"), Some(rgb(0x1e1e1e).into()));
        assert_eq!(parse_color("#1E1E1E80"), Some(rgba(0x1e1e1e80).into()));
        for invalid in ["1e1e1e", "#1e1e1", "#1e1e1e8", "#+1e1e1e", "#gggggg", ""] {
            assert_eq!(parse_color(invalid), None, "{invalid}");
        }
    }

    #[test]
    fn reference_cycles_and_dangling_references_stay_unresolved() {
        let definition = |key: &str, target: &str| ColorDefinition {
            key: key.to_owned(),
            description: String::new(),
            dark: ColorDefault::Key(target.to_owned()),
            light: ColorDefault::Key(target.to_owned()),
        };
        let colors = resolve_colors(
            Appearance::Dark,
            &HashMap::new(),
            &[
                definition("a", "b"),
                definition("b", "a"),
                definition("c", "missing"),
            ],
        );
        assert!(colors.is_empty());
    }

    #[test]
    fn decorations_take_the_status_colors() {
        for appearance in [Appearance::Dark, Appearance::Light] {
            let theme = Theme::defaults(appearance, &native_color_definitions());
            assert_eq!(theme.appearance, appearance);
            assert_eq!(
                theme.underline_color(Highlight::named("diagnostic.error")),
                theme.color(color::ERROR)
            );
            assert_eq!(
                theme.underline_color(Highlight::named("diagnostic.warning")),
                theme.color(color::WARNING)
            );
        }
    }

    /// Every surface takes its colors from the theme; tests may build colors.
    #[test]
    fn no_color_literals_outside_the_theme() {
        let mut offenders = Vec::new();
        let mut pending = vec![std::path::PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/app"
        ))];
        while let Some(path) = pending.pop() {
            if path.is_dir() {
                pending.extend(
                    std::fs::read_dir(&path)
                        .unwrap()
                        .map(|entry| entry.unwrap().path()),
                );
            } else if path.extension().is_some_and(|extension| extension == "rs")
                && !path.ends_with("theme.rs")
                && !path.ends_with("tests.rs")
                && !path
                    .components()
                    .any(|component| component.as_os_str() == "tests")
            {
                let source = std::fs::read_to_string(&path).unwrap();
                for (index, line) in source.lines().enumerate() {
                    if ["rgb(0x", "rgba(0x", "hsla("]
                        .iter()
                        .any(|literal| line.contains(literal))
                    {
                        offenders.push(format!("{}:{}", path.display(), index + 1));
                    }
                }
            }
        }
        assert!(offenders.is_empty(), "color literals: {offenders:#?}");
    }

    #[gpui::test]
    fn installing_a_theme_repaints_windows_and_replaces_the_defaults(
        cx: &mut gpui::TestAppContext,
    ) {
        struct Probe(std::rc::Rc<std::cell::Cell<usize>>);
        impl gpui::Render for Probe {
            fn render(
                &mut self,
                _: &mut gpui::Window,
                _: &mut gpui::Context<Self>,
            ) -> impl gpui::IntoElement {
                self.0.set(self.0.get() + 1);
                gpui::Empty
            }
        }
        let renders = std::rc::Rc::new(std::cell::Cell::new(0));
        cx.add_window({
            let renders = renders.clone();
            move |_, _| Probe(renders)
        });
        cx.run_until_parked();
        let before = renders.get();
        cx.update(|cx| {
            assert_eq!(theme(cx).appearance, Appearance::Dark);
            set_theme(
                Theme::defaults(Appearance::Light, &native_color_definitions()),
                cx,
            );
        });
        cx.run_until_parked();
        assert!(renders.get() > before);
        cx.update(|cx| assert_eq!(theme(cx).appearance, Appearance::Light));
    }

    #[test]
    #[should_panic(expected = "function.calls is not a highlight name")]
    fn named_requires_an_exact_vocabulary_entry() {
        Highlight::named("function.calls");
    }
}
