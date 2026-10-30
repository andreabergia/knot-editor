use gpui::{TestAppContext, rgb};

use super::*;
use crate::app::theme::{color, theme};

fn owner(lifecycle: u64) -> ThemeOwner {
    ThemeOwner {
        extension: ExtensionId::new(1),
        lifecycle: ExtensionLifecycleId::new(lifecycle),
    }
}

fn definition(name: &str, appearance: Appearance, background: &str) -> ThemeDefinition {
    ThemeDefinition {
        name: name.into(),
        appearance,
        colors: BTreeMap::from([(color::BACKGROUND.into(), background.into())]),
        syntax: BTreeMap::from([(
            "keyword".into(),
            SyntaxStyle {
                color: Some("#ff0000".into()),
                font_weight: Some(700),
                ..Default::default()
            },
        )]),
    }
}

fn dark(name: &str) -> Selection {
    Selection {
        name: name.into(),
        appearance: Appearance::Dark,
    }
}

#[test]
fn the_latest_registration_of_a_name_wins_and_removal_reveals_the_previous() {
    let mut registry = ThemeRegistry::default();
    registry.register(owner(1), definition("Night", Appearance::Dark, "#111111"));
    let later = registry.register(owner(2), definition("Night", Appearance::Dark, "#222222"));
    registry.register(owner(2), definition("Day", Appearance::Light, "#eeeeee"));
    assert_eq!(
        registry.get("Night").unwrap().colors[color::BACKGROUND],
        "#222222"
    );
    assert_eq!(
        registry.list(),
        [
            ThemeInfo {
                name: "Day".into(),
                appearance: Appearance::Light
            },
            ThemeInfo {
                name: "Night".into(),
                appearance: Appearance::Dark
            },
        ]
    );

    assert!(!registry.unregister(owner(1), later));
    assert!(registry.unregister(owner(2), later));
    assert_eq!(
        registry.get("Night").unwrap().colors[color::BACKGROUND],
        "#111111"
    );
    registry.remove_lifecycle(ExtensionId::new(1), ExtensionLifecycleId::new(1));
    assert!(registry.get("Night").is_none());
    assert_eq!(registry.list().len(), 1);
}

#[test]
fn a_registered_theme_fills_missing_keys_from_defaults_of_its_appearance() {
    let mut registry = ThemeRegistry::default();
    registry.register(owner(1), definition("Day", Appearance::Light, "#fafafa"));
    let theme = registry.theme(&dark("Day"));
    let defaults = Theme::defaults(Appearance::Light, &native_color_definitions());
    assert_eq!(theme.appearance, Appearance::Light);
    assert_eq!(theme.color(color::BACKGROUND), rgb(0xfafafa).into());
    // editor.background defaults to background, so it follows the theme.
    assert_eq!(theme.color(color::EDITOR_BACKGROUND), rgb(0xfafafa).into());
    assert_eq!(theme.color(color::TEXT), defaults.color(color::TEXT));
    let keyword = theme.style(Highlight::named("keyword.return"));
    assert_eq!(keyword.color, Some(rgb(0xff0000).into()));
    assert_eq!(keyword.font_weight, Some(gpui::FontWeight::BOLD));
    assert!(
        theme
            .style(Highlight::named("markup.strong"))
            .font_weight
            .is_some()
    );
}

#[test]
fn an_unregistered_name_uses_the_defaults_of_the_selected_appearance() {
    let registry = ThemeRegistry::default();
    let theme = registry.theme(&Selection {
        name: "Missing".into(),
        appearance: Appearance::Light,
    });
    assert_eq!(theme.appearance, Appearance::Light);
}

#[test]
fn extension_color_definitions_resolve_and_leave_with_their_lifecycle() {
    let mut registry = ThemeRegistry::default();
    let added = |dark: &str| ColorDefinition {
        key: "diff.add".into(),
        description: "Added lines".into(),
        dark: color_default(dark).unwrap(),
        light: color_default("#00ff00").unwrap(),
    };
    registry.define_color(owner(1), added("success")).unwrap();
    assert_eq!(
        registry.define_color(owner(2), added("#000000")),
        Err(DuplicateColor)
    );
    let native = ColorDefinition {
        key: color::TEXT.into(),
        ..added("#000000")
    };
    assert_eq!(registry.define_color(owner(2), native), Err(DuplicateColor));

    let mut themed = definition("Night", Appearance::Dark, "#111111");
    themed
        .colors
        .insert(color::SUCCESS.into(), "#123456".into());
    registry.register(owner(2), themed);
    assert_eq!(
        registry.theme(&dark("Night")).color("diff.add"),
        rgb(0x123456).into()
    );

    registry.remove_lifecycle(ExtensionId::new(1), ExtensionLifecycleId::new(1));
    let theme = registry.theme(&dark("Night"));
    assert_eq!(theme.color("diff.add"), theme.color(color::TEXT));
    registry.define_color(owner(3), added("#abcdef")).unwrap();
}

#[test]
fn validation_rejects_bad_colors_captures_and_weights_but_keeps_open_keys() {
    let mut theme = definition("Night", Appearance::Dark, "#111111");
    theme
        .colors
        .insert("some.extension.key".into(), "#22222280".into());
    theme.syntax.insert("markup".into(), SyntaxStyle::default());
    assert_eq!(theme.invalid_value(), None);
    assert_eq!(theme.unknown_capture(), None);

    let mut invalid = theme.clone();
    invalid.colors.insert("text".into(), "red".into());
    assert!(invalid.invalid_value().is_some());
    let mut invalid = theme.clone();
    invalid.syntax.insert(
        "type".into(),
        SyntaxStyle {
            font_weight: Some(950),
            ..Default::default()
        },
    );
    assert!(invalid.invalid_value().is_some());
    let mut invalid = theme;
    invalid
        .syntax
        .insert("keywords".into(), SyntaxStyle::default());
    assert_eq!(invalid.unknown_capture(), Some("keywords"));
    assert_eq!(color_default(""), None);
    assert_eq!(color_default("#12"), None);
}

#[gpui::test]
fn startup_applies_the_selection_and_settings_writes_switch_it(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.set_global(ApplicationSettings::default());
        let registry = &mut cx.default_global::<ApplicationThemes>().0;
        registry.register(
            owner(1),
            definition("One Dark", Appearance::Dark, "#101010"),
        );
        registry.register(owner(1), definition("Paper", Appearance::Light, "#fefefe"));
        apply_at_startup(cx);
        assert_eq!(theme(cx).color(color::BACKGROUND), rgb(0x101010).into());
    });
    cx.update(|cx| {
        let scope = settings::SettingsScope::default();
        let settings = &mut cx.global_mut::<ApplicationSettings>().0;
        settings
            .set(THEME_LIGHT, "Paper".into(), scope.clone())
            .unwrap();
        settings.set(THEME_MODE, "light".into(), scope).unwrap();
    });
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(theme(cx).appearance, Appearance::Light);
        assert_eq!(theme(cx).color(color::BACKGROUND), rgb(0xfefefe).into());
        // A registration after startup changes nothing until the selection does.
        cx.global_mut::<ApplicationThemes>()
            .0
            .register(owner(1), definition("Paper", Appearance::Light, "#eeeeee"));
        selection_changed(cx);
        assert_eq!(theme(cx).color(color::BACKGROUND), rgb(0xfefefe).into());
    });
}

#[gpui::test]
fn system_mode_follows_the_window_appearance(cx: &mut TestAppContext) {
    cx.update(|cx| {
        let mut settings = ApplicationSettings::default();
        settings
            .0
            .set(THEME_MODE, "system".into(), Default::default())
            .unwrap();
        cx.set_global(settings);
        let expected = match cx.window_appearance() {
            WindowAppearance::Light | WindowAppearance::VibrantLight => Appearance::Light,
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Appearance::Dark,
        };
        apply_at_startup(cx);
        assert_eq!(theme(cx).appearance, expected);
    });
}
