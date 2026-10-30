//! Theme registrations, extension color definitions, and selection of the
//! application theme.
//!
//! Extensions and personal config register themes by name; the latest
//! registration of a name defines it, and removing it reveals the previous
//! one. Extensions also define color keys beside the native ones, so a theme
//! and a contribution can name a key the native table does not know.
//!
//! The `theme.mode`, `theme.dark`, and `theme.light` settings select the
//! theme. It is resolved when startup completes and again whenever the
//! selection changes; registering or removing a theme later changes nothing
//! until the next selection change.

use std::collections::{BTreeMap, HashMap};

use gpui::{App, Global, HighlightStyle, UnderlineStyle, WindowAppearance, px};
use serde::{Deserialize, Serialize};

use crate::host::protocol::{ExtensionId, ExtensionLifecycleId};

use super::{
    settings::{self, ApplicationSettings, THEME_DARK, THEME_LIGHT, THEME_MODE},
    theme::{
        Appearance, ColorDefault, ColorDefinition, Highlight, Theme, markup_styles,
        native_color_definitions, parse_color, set_theme,
    },
};

/// The extension lifecycle that owns a registration or color definition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ThemeOwner {
    pub(crate) extension: ExtensionId,
    pub(crate) lifecycle: ExtensionLifecycleId,
}

/// How a theme styles one capture name.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SyntaxStyle {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) font_style: Option<FontStyle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) font_weight: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) underline: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) strikethrough: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum FontStyle {
    Normal,
    Italic,
}

impl SyntaxStyle {
    fn highlight(&self) -> HighlightStyle {
        HighlightStyle {
            color: self.color.as_deref().and_then(parse_color),
            font_style: self.font_style.map(|style| match style {
                FontStyle::Normal => gpui::FontStyle::Normal,
                FontStyle::Italic => gpui::FontStyle::Italic,
            }),
            font_weight: self
                .font_weight
                .map(|weight| gpui::FontWeight(f32::from(weight))),
            underline: self
                .underline
                .filter(|underline| *underline)
                .map(|_| UnderlineStyle {
                    thickness: px(1.),
                    color: None,
                    wavy: false,
                }),
            strikethrough: self
                .strikethrough
                .filter(|strikethrough| *strikethrough)
                .map(|_| gpui::StrikethroughStyle {
                    thickness: px(1.),
                    color: None,
                }),
            ..Default::default()
        }
    }
}

/// A theme as an extension registers it and `themes.get` returns it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ThemeDefinition {
    pub(crate) name: String,
    pub(crate) appearance: Appearance,
    #[serde(default)]
    pub(crate) colors: BTreeMap<String, String>,
    #[serde(default)]
    pub(crate) syntax: BTreeMap<String, SyntaxStyle>,
}

impl ThemeDefinition {
    /// Why a color or font weight is invalid, if one is.
    pub(crate) fn invalid_value(&self) -> Option<&'static str> {
        let colors = self.colors.values().map(String::as_str).chain(
            self.syntax
                .values()
                .filter_map(|style| style.color.as_deref()),
        );
        for color in colors {
            if parse_color(color).is_none() {
                return Some("theme colors must be #rrggbb or #rrggbbaa");
            }
        }
        self.syntax
            .values()
            .filter_map(|style| style.font_weight)
            .any(|weight| !(100..=900).contains(&weight))
            .then_some("theme font weights must be from 100 to 900")
    }

    /// A syntax key that names no capture or capture prefix; color keys
    /// stay open.
    pub(crate) fn unknown_capture(&self) -> Option<&str> {
        self.syntax
            .keys()
            .map(String::as_str)
            .find(|capture| !Highlight::styles_captures(capture))
    }

    fn theme(&self, definitions: &[ColorDefinition]) -> Theme {
        let values = self
            .colors
            .iter()
            .filter_map(|(key, value)| Some((key.clone(), parse_color(value)?)))
            .collect::<HashMap<_, _>>();
        let syntax = self
            .syntax
            .iter()
            .map(|(capture, style)| (capture.as_str(), style.highlight()))
            .chain(markup_styles())
            .collect::<Vec<_>>();
        Theme::new(self.appearance, &values, definitions, &syntax)
    }
}

/// A color default as written: `#rrggbb`, `#rrggbbaa`, or another key.
pub(crate) fn color_default(text: &str) -> Option<ColorDefault> {
    if text.starts_with('#') {
        parse_color(text).map(ColorDefault::Color)
    } else if text.is_empty() {
        None
    } else {
        Some(ColorDefault::Key(text.to_owned()))
    }
}

/// A registered theme's name and appearance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct ThemeInfo {
    pub(crate) name: String,
    pub(crate) appearance: Appearance,
}

struct Registration {
    id: u64,
    owner: ThemeOwner,
    definition: ThemeDefinition,
}

/// Which theme applies: a theme name and the appearance it was chosen for.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Selection {
    name: String,
    appearance: Appearance,
}

#[derive(Default)]
pub(crate) struct ThemeRegistry {
    /// In registration order, so a later index wins.
    registrations: Vec<Registration>,
    colors: Vec<(ThemeOwner, ColorDefinition)>,
    next_id: u64,
    /// The selection the installed theme was resolved from.
    applied: Option<Selection>,
}

impl ThemeRegistry {
    /// Add a validated registration, returning its ID.
    pub(crate) fn register(&mut self, owner: ThemeOwner, definition: ThemeDefinition) -> u64 {
        self.next_id += 1;
        self.registrations.push(Registration {
            id: self.next_id,
            owner,
            definition,
        });
        self.next_id
    }

    /// Remove an owned registration; false when it is not live.
    pub(crate) fn unregister(&mut self, owner: ThemeOwner, id: u64) -> bool {
        let before = self.registrations.len();
        self.registrations
            .retain(|registration| registration.id != id || registration.owner != owner);
        self.registrations.len() != before
    }

    /// Define a color key, which must not already be defined.
    pub(crate) fn define_color(
        &mut self,
        owner: ThemeOwner,
        definition: ColorDefinition,
    ) -> Result<(), DuplicateColor> {
        if self
            .color_definitions()
            .iter()
            .any(|existing| existing.key == definition.key)
        {
            return Err(DuplicateColor);
        }
        self.colors.push((owner, definition));
        Ok(())
    }

    pub(crate) fn remove_lifecycle(
        &mut self,
        extension: ExtensionId,
        lifecycle: ExtensionLifecycleId,
    ) {
        let owner = ThemeOwner {
            extension,
            lifecycle,
        };
        self.registrations
            .retain(|registration| registration.owner != owner);
        self.colors.retain(|(color_owner, _)| *color_owner != owner);
    }

    /// The latest registration of `name`.
    pub(crate) fn get(&self, name: &str) -> Option<&ThemeDefinition> {
        self.registrations
            .iter()
            .rev()
            .map(|registration| &registration.definition)
            .find(|definition| definition.name == name)
    }

    /// Each registered name's effective theme, sorted by name.
    pub(crate) fn list(&self) -> Vec<ThemeInfo> {
        let mut themes = BTreeMap::new();
        for registration in &self.registrations {
            themes.insert(
                registration.definition.name.clone(),
                registration.definition.appearance,
            );
        }
        themes
            .into_iter()
            .map(|(name, appearance)| ThemeInfo { name, appearance })
            .collect()
    }

    /// Native color definitions followed by extension ones.
    fn color_definitions(&self) -> Vec<ColorDefinition> {
        native_color_definitions()
            .into_iter()
            .chain(self.colors.iter().map(|(_, definition)| definition.clone()))
            .collect()
    }

    /// The theme `selection` names, else the defaults for its appearance.
    fn theme(&self, selection: &Selection) -> Theme {
        let definitions = self.color_definitions();
        match self.get(&selection.name) {
            Some(definition) => definition.theme(&definitions),
            None => Theme::defaults(selection.appearance, &definitions),
        }
    }
}

/// A color key that native code or a live lifecycle already defines.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DuplicateColor;

#[derive(Default)]
pub(crate) struct ApplicationThemes(pub(crate) ThemeRegistry);

impl Global for ApplicationThemes {}

/// The theme the selection settings name, for the current system appearance.
fn selection(cx: &App) -> Selection {
    let appearance = match settings::text(THEME_MODE, cx).as_str() {
        "light" => Appearance::Light,
        "system" => match cx.window_appearance() {
            WindowAppearance::Light | WindowAppearance::VibrantLight => Appearance::Light,
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Appearance::Dark,
        },
        _ => Appearance::Dark,
    };
    let name = settings::text(
        match appearance {
            Appearance::Dark => THEME_DARK,
            Appearance::Light => THEME_LIGHT,
        },
        cx,
    );
    Selection { name, appearance }
}

/// Resolve and install the selected theme. Startup calls this once every
/// theme is registered, and it starts following the selection settings.
pub(crate) fn apply_at_startup(cx: &mut App) {
    apply(cx, true);
    cx.observe_global::<ApplicationSettings>(|cx| apply(cx, false))
        .detach();
}

/// Install the selected theme again if the selection changed, such as after
/// a settings write or a change of system appearance.
pub(crate) fn selection_changed(cx: &mut App) {
    apply(cx, false);
}

fn apply(cx: &mut App, force: bool) {
    let selection = selection(cx);
    let registry = &mut cx.default_global::<ApplicationThemes>().0;
    if !force && registry.applied.as_ref() == Some(&selection) {
        return;
    }
    let theme = registry.theme(&selection);
    registry.applied = Some(selection);
    set_theme(theme, cx);
}

pub(crate) fn remove_lifecycle(
    extension: ExtensionId,
    lifecycle: ExtensionLifecycleId,
    cx: &mut App,
) {
    if cx.has_global::<ApplicationThemes>() {
        cx.global_mut::<ApplicationThemes>()
            .0
            .remove_lifecycle(extension, lifecycle);
    }
}

#[cfg(test)]
mod tests;
