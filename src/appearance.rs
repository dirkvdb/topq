//! Application theme selection and GPUI Kit theme-set loading.

use std::{
    fs,
    path::{Path, PathBuf},
    rc::Rc,
};

use anyhow::{Context, Result, anyhow};
use gpui_kit::component::{Theme, ThemeConfig, ThemeMode, ThemeRegistry, highlighter::HighlightTheme};
use gpui_kit::{App, Global, SharedString, Window};
use serde::{Deserialize, Serialize};

use crate::config;

const DEFAULT_LIGHT_THEME: &str = "Ayu Light";
const DEFAULT_DARK_THEME: &str = "Charcoal Grove";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AppearanceMode {
    System,
    Light,
    #[default]
    Dark,
}

impl AppearanceMode {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Appearance {
    mode: AppearanceMode,
    light_theme: String,
    dark_theme: String,
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            mode: AppearanceMode::Dark,
            light_theme: DEFAULT_LIGHT_THEME.to_owned(),
            dark_theme: DEFAULT_DARK_THEME.to_owned(),
            path: None,
        }
    }
}

impl Global for Appearance {}

impl Appearance {
    pub(crate) fn mode(cx: &App) -> AppearanceMode {
        cx.global::<Self>().mode
    }

    pub(crate) fn selected_theme(mode: ThemeMode, cx: &App) -> &str {
        cx.global::<Self>().theme(mode)
    }

    fn theme(&self, mode: ThemeMode) -> &str {
        match mode {
            ThemeMode::Light => &self.light_theme,
            ThemeMode::Dark => &self.dark_theme,
        }
    }

    fn theme_mut(&mut self, mode: ThemeMode) -> &mut String {
        match mode {
            ThemeMode::Light => &mut self.light_theme,
            ThemeMode::Dark => &mut self.dark_theme,
        }
    }

    fn recover_slots(&mut self, cx: &App) {
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            if let Err(error) = theme_config(mode, self.theme(mode), cx) {
                let name = match mode {
                    ThemeMode::Light => DEFAULT_LIGHT_THEME,
                    ThemeMode::Dark => DEFAULT_DARK_THEME,
                };
                let fallback = theme_config(mode, name, cx).unwrap_or_else(|_| {
                    let registry = ThemeRegistry::global(cx);
                    match mode {
                        ThemeMode::Light => registry.default_light_theme().clone(),
                        ThemeMode::Dark => registry.default_dark_theme().clone(),
                    }
                });
                eprintln!("{error:#} Using \"{}\" instead.", fallback.name);
                *self.theme_mut(mode) = fallback.name.to_string();
            }
        }
    }
}

pub(crate) fn register_bundled(cx: &mut App) -> Result<()> {
    for content in [
        include_str!("../themes/charcoal-grove.json"),
        include_str!("../themes/ayu.json"),
        include_str!("../themes/tokyonight.json"),
        include_str!("../themes/collection.json"),
    ] {
        ThemeRegistry::global_mut(cx)
            .load_themes_from_str(content)
            .context("Could not load a bundled theme.")?;
    }
    Ok(())
}

pub(crate) fn init(cx: &mut App) {
    if let Err(error) = register_bundled(cx) {
        eprintln!("{error:#}");
    }
    let mut preferences = match config::config_path() {
        Ok(path) => {
            if let Err(error) = load_custom(&path.with_file_name("themes"), cx) {
                eprintln!("Could not load custom themes: {error:#}");
            }
            load_or_default(&path.with_file_name("appearance.json"), cx)
        }
        Err(error) => {
            eprintln!("Could not locate appearance settings: {error:#}");
            Appearance::default()
        }
    };
    preferences.recover_slots(cx);
    let mode = preferences.mode;
    cx.set_global(preferences);
    if let Err(error) = apply(mode, None, cx) {
        eprintln!("Could not apply appearance settings: {error:#}");
    }
}

pub(crate) fn load(path: &Path, cx: &App) -> Result<Appearance> {
    let mut preferences = match fs::read(path) {
        Ok(data) => {
            let value: serde_json::Value = serde_json::from_slice(&data).context("Could not parse appearance.json.")?;
            let mut preferences: Appearance = serde_json::from_value(value.clone()).context("Could not parse appearance.json.")?;
            // New settings take precedence if a file contains both schemas.
            if !["mode", "light_theme", "dark_theme"].iter().any(|key| value.get(key).is_some()) {
                match value.get("theme") {
                    Some(serde_json::Value::Null) => {
                        preferences.mode = AppearanceMode::System;
                        preferences.dark_theme = "Ayu Dark".to_owned();
                    }
                    Some(serde_json::Value::String(name)) => {
                        if let Some(theme) = ThemeRegistry::global(cx).themes().get(&SharedString::from(name.clone())) {
                            preferences.mode = if theme.mode.is_dark() {
                                AppearanceMode::Dark
                            } else {
                                AppearanceMode::Light
                            };
                            *preferences.theme_mut(theme.mode) = name.clone();
                        } else {
                            eprintln!("Legacy theme \"{name}\" is unavailable. Using the default appearance.");
                        }
                    }
                    None => {}
                    Some(_) => return Err(anyhow!("Could not parse appearance.json: legacy theme must be a string or null.")),
                }
            }
            preferences
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Appearance::default(),
        Err(error) => return Err(error).context("Could not read appearance.json."),
    };
    preferences.path = Some(path.to_path_buf());
    preferences.recover_slots(cx);
    Ok(preferences)
}

fn load_or_default(path: &Path, cx: &App) -> Appearance {
    load(path, cx).unwrap_or_else(|error| {
        eprintln!("Could not load appearance settings: {error:#}");
        let mut preferences = Appearance {
            path: Some(path.to_path_buf()),
            ..Appearance::default()
        };
        preferences.recover_slots(cx);
        preferences
    })
}

fn load_custom(directory: &Path, cx: &mut App) -> Result<()> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("Could not read the themes directory."),
    };
    let mut paths = entries
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.sort();
    for path in paths {
        if !path.is_file() || path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        // One malformed custom theme must not prevent other themes from loading.
        let result = fs::read_to_string(&path)
            .map_err(anyhow::Error::from)
            .and_then(|content| ThemeRegistry::global_mut(cx).load_themes_from_str(&content));
        if let Err(error) = result {
            eprintln!("Could not load theme {}: {error:#}", path.display());
        }
    }
    Ok(())
}

fn theme_config(mode: ThemeMode, name: &str, cx: &App) -> Result<Rc<ThemeConfig>> {
    let theme = ThemeRegistry::global(cx)
        .themes()
        .get(&SharedString::from(name.to_owned()))
        .cloned()
        .ok_or_else(|| anyhow!("Theme \"{name}\" is unavailable."))?;
    if theme.mode != mode {
        return Err(anyhow!("Theme \"{name}\" is {}, not {}.", theme.mode.name(), mode.name()));
    }
    Ok(theme)
}

fn effective_mode(mode: AppearanceMode, window: Option<&Window>, cx: &App) -> ThemeMode {
    match mode {
        AppearanceMode::Light => ThemeMode::Light,
        AppearanceMode::Dark => ThemeMode::Dark,
        AppearanceMode::System => window.map_or_else(|| cx.window_appearance(), Window::appearance).into(),
    }
}

fn apply(mode: AppearanceMode, window: Option<&mut Window>, cx: &mut App) -> Result<()> {
    let light = theme_config(ThemeMode::Light, Appearance::selected_theme(ThemeMode::Light, cx), cx)?;
    let dark = theme_config(ThemeMode::Dark, Appearance::selected_theme(ThemeMode::Dark, cx), cx)?;
    apply_pair(effective_mode(mode, window.as_deref(), cx), light, dark, cx);
    Ok(())
}

fn apply_pair(mode: ThemeMode, light: Rc<ThemeConfig>, dark: Rc<ThemeConfig>, cx: &mut App) {
    Theme::update(cx, |theme| {
        theme.light_theme = light;
        theme.dark_theme = dark;
        let config = if mode.is_dark() {
            theme.dark_theme.clone()
        } else {
            theme.light_theme.clone()
        };
        // GPUI's system sync reapplies the config without clearing omitted fields.
        // Reset for the destination mode on every switch, including system events.
        reset_metrics(theme, mode);
        theme.apply_config(&config);
    });
}

fn reset_metrics(theme: &mut Theme, mode: ThemeMode) {
    // ThemeConfig's omitted fields inherit current values. Clear the previous
    // theme's font/radius overrides before installing the next theme.
    let defaults = Theme::default();
    theme.font_family = defaults.font_family;
    theme.font_size = defaults.font_size;
    theme.mono_font_family = defaults.mono_font_family;
    theme.mono_font_size = defaults.mono_font_size;
    theme.radius = defaults.radius;
    theme.radius_lg = defaults.radius_lg;
    theme.shadow = defaults.shadow;
    theme.highlight_theme = if mode.is_dark() {
        HighlightTheme::default_dark()
    } else {
        HighlightTheme::default_light()
    };
}

pub(crate) fn select_mode(mode: AppearanceMode, window: &mut Window, cx: &mut App) -> Result<()> {
    apply(mode, Some(window), cx)?;
    cx.global_mut::<Appearance>().mode = mode;
    save(cx).context("Appearance changed, but the preference could not be saved.")
}

pub(crate) fn select_theme(mode: ThemeMode, name: SharedString, window: &mut Window, cx: &mut App) -> Result<()> {
    let config = theme_config(mode, name.as_str(), cx)?;
    let active = effective_mode(Appearance::mode(cx), Some(window), cx) == mode;
    Theme::update(cx, |theme| {
        if active {
            reset_metrics(theme, mode);
            theme.apply_config(&config);
        } else {
            // Register the inactive choice without touching the current palette or metrics.
            match mode {
                ThemeMode::Light => theme.light_theme = config,
                ThemeMode::Dark => theme.dark_theme = config,
            }
        }
    });
    *cx.global_mut::<Appearance>().theme_mut(mode) = name.to_string();
    save(cx).context("Theme changed, but the preference could not be saved.")
}

fn save(cx: &mut App) -> Result<()> {
    let path = match &cx.global::<Appearance>().path {
        Some(path) => path.clone(),
        None => config::config_path()?.with_file_name("appearance.json"),
    };
    cx.global_mut::<Appearance>().path = Some(path.clone());
    config::write_json(&path, cx.global::<Appearance>())
}

pub(crate) fn sync_system(window: &mut Window, cx: &mut App) {
    if Appearance::mode(cx) == AppearanceMode::System
        && let Err(error) = apply(AppearanceMode::System, Some(window), cx)
    {
        eprintln!("Could not synchronize system appearance: {error:#}");
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::{AppContext, Context, IntoElement, Render, TestAppContext, div, px};

    use super::*;

    struct TestView;

    impl Render for TestView {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    fn setup(cx: &mut App, path: &Path) {
        gpui_kit::init(cx);
        register_bundled(cx).unwrap();
        cx.set_global(load(path, cx).unwrap());
        apply(Appearance::mode(cx), None, cx).unwrap();
    }

    fn assert_tokens(cx: &App) {
        let theme = Theme::global(cx);
        assert_eq!(theme.mode, theme.highlight_theme.appearance);
        assert_eq!(theme.sidebar, theme.tokens.sidebar.color);
        assert_eq!(theme.primary, theme.tokens.primary.color);
        assert_eq!(theme.background, theme.tokens.background.color);
        let base = gpui_kit::base::Theme::global(cx);
        assert_eq!(base.tokens.colors.background, theme.background);
        assert_eq!(base.tokens.colors.primary, theme.primary);
    }

    #[test]
    fn defaults_and_mode_labels_have_stable_serialization() {
        let preferences = Appearance::default();
        assert_eq!(
            serde_json::to_value(preferences).unwrap(),
            serde_json::json!({"mode": "dark", "light_theme": "Ayu Light", "dark_theme": "Charcoal Grove"})
        );
        for (mode, label, serialized) in [
            (AppearanceMode::System, "System", "system"),
            (AppearanceMode::Light, "Light", "light"),
            (AppearanceMode::Dark, "Dark", "dark"),
        ] {
            assert_eq!(mode.label(), label);
            assert_eq!(serde_json::to_value(mode).unwrap(), serialized);
            assert_eq!(serde_json::from_value::<AppearanceMode>(serialized.into()).unwrap(), mode);
        }
    }

    #[gpui_kit::test]
    fn missing_or_empty_settings_use_dark_charcoal_grove(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("appearance.json");
        cx.update(|cx| {
            setup(cx, &path);
            assert_eq!(Appearance::mode(cx), AppearanceMode::Dark);
            assert_eq!(Appearance::selected_theme(ThemeMode::Light, cx), DEFAULT_LIGHT_THEME);
            assert_eq!(Appearance::selected_theme(ThemeMode::Dark, cx), DEFAULT_DARK_THEME);
            assert_eq!(Theme::global(cx).theme_name().as_str(), DEFAULT_DARK_THEME);
            assert_eq!(Theme::global(cx).highlight_theme.name, DEFAULT_DARK_THEME);
            fs::write(&path, "{}").unwrap();
            let preferences = load(&path, cx).unwrap();
            assert_eq!(preferences.mode, AppearanceMode::Dark);
            assert_eq!(preferences.path.as_deref(), Some(path.as_path()));
        });
    }

    #[gpui_kit::test]
    fn legacy_named_themes_migrate_using_registry_mode(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("appearance.json");
        cx.update(|cx| {
            setup(cx, &path);
            ThemeRegistry::global_mut(cx)
                .load_themes_from_str(r#"{"name":"Custom","themes":[{"name":"Midnight","mode":"light","colors":{}}]}"#)
                .unwrap();
            for (name, mode, slot) in [
                ("Ayu Light", AppearanceMode::Light, ThemeMode::Light),
                ("Tokyo Night", AppearanceMode::Dark, ThemeMode::Dark),
                ("Midnight", AppearanceMode::Light, ThemeMode::Light),
            ] {
                fs::write(&path, serde_json::to_vec(&serde_json::json!({"theme":name})).unwrap()).unwrap();
                let preferences = load(&path, cx).unwrap();
                assert_eq!(preferences.mode, mode);
                assert_eq!(preferences.theme(slot), name);
                let other = if slot.is_dark() { ThemeMode::Light } else { ThemeMode::Dark };
                assert_eq!(preferences.theme(other), Appearance::default().theme(other));
                assert_eq!(preferences.path.as_deref(), Some(path.as_path()));
            }
            fs::write(&path, r#"{"theme":"Removed"}"#).unwrap();
            assert_eq!(load(&path, cx).unwrap().mode, AppearanceMode::Dark);
        });
    }

    #[gpui_kit::test]
    fn legacy_null_preserves_system_ayu_pair_and_new_schema_takes_precedence(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("appearance.json");
        cx.update(|cx| {
            setup(cx, &path);
            fs::write(&path, r#"{"theme":null}"#).unwrap();
            let preferences = load(&path, cx).unwrap();
            assert_eq!(preferences.mode, AppearanceMode::System);
            assert_eq!(preferences.light_theme, "Ayu Light");
            assert_eq!(preferences.dark_theme, "Ayu Dark");
            cx.set_global(preferences);
            apply(AppearanceMode::System, None, cx).unwrap();
            assert_eq!(Theme::global(cx).light_theme.name.as_str(), "Ayu Light");
            assert_eq!(Theme::global(cx).dark_theme.name.as_str(), "Ayu Dark");
            fs::write(&path, r#"{"mode":"light","dark_theme":"Tokyo Night","theme":null}"#).unwrap();
            let preferences = load(&path, cx).unwrap();
            assert_eq!(preferences.mode, AppearanceMode::Light);
            assert_eq!(preferences.dark_theme, "Tokyo Night");
        });
    }

    #[gpui_kit::test]
    fn unavailable_or_wrong_mode_slots_recover_independently(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("appearance.json");
        cx.update(|cx| {
            setup(cx, &path);
            for (light, dark, expected_light, expected_dark) in [
                ("Removed", "Tokyo Night", DEFAULT_LIGHT_THEME, "Tokyo Night"),
                ("Gruvbox Light", "Removed", "Gruvbox Light", DEFAULT_DARK_THEME),
                ("Ayu Dark", "Tokyo Night", DEFAULT_LIGHT_THEME, "Tokyo Night"),
                ("Gruvbox Light", "Ayu Light", "Gruvbox Light", DEFAULT_DARK_THEME),
            ] {
                for mode in [AppearanceMode::System, AppearanceMode::Light, AppearanceMode::Dark] {
                    fs::write(
                        &path,
                        serde_json::to_vec(&serde_json::json!({"mode":mode,"light_theme":light,"dark_theme":dark})).unwrap(),
                    )
                    .unwrap();
                    let preferences = load(&path, cx).unwrap();
                    assert_eq!(preferences.mode, mode);
                    assert_eq!(preferences.light_theme, expected_light);
                    assert_eq!(preferences.dark_theme, expected_dark);
                    assert_eq!(preferences.path.as_deref(), Some(path.as_path()));
                }
            }
        });
    }

    #[gpui_kit::test]
    fn inactive_choices_persist_without_switching_mode_palette_or_metrics(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("appearance.json");
        cx.update(|cx| setup(cx, &path));
        let handle = cx.add_window(|_, _| TestView);
        cx.update_window(handle.into(), |_, window, cx| {
            let before = Theme::global(cx).clone();
            select_theme(ThemeMode::Light, "Gruvbox Light".into(), window, cx).unwrap();
            let after = Theme::global(cx);
            assert_eq!(Appearance::mode(cx), AppearanceMode::Dark);
            assert_eq!(after.theme_name(), before.theme_name());
            assert_eq!(after.colors, before.colors);
            assert_eq!(after.tokens, before.tokens);
            assert_eq!(after.font_size, before.font_size);
            assert_eq!(after.radius, before.radius);
            assert!(std::sync::Arc::ptr_eq(&after.highlight_theme, &before.highlight_theme));
            assert_eq!(after.light_theme.name.as_str(), "Gruvbox Light");
            select_mode(AppearanceMode::Light, window, cx).unwrap();
            assert_eq!(Theme::global(cx).theme_name().as_str(), "Gruvbox Light");
            select_theme(ThemeMode::Dark, "Tokyo Night".into(), window, cx).unwrap();
            assert_eq!(Appearance::mode(cx), AppearanceMode::Light);
            assert_eq!(Theme::global(cx).theme_name().as_str(), "Gruvbox Light");
            let preferences = load(&path, cx).unwrap();
            assert_eq!(preferences.mode, AppearanceMode::Light);
            assert_eq!(preferences.light_theme, "Gruvbox Light");
            assert_eq!(preferences.dark_theme, "Tokyo Night");
            let saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            assert_eq!(saved.as_object().unwrap().len(), 3);
            select_mode(AppearanceMode::Dark, window, cx).unwrap();
            assert_eq!(Theme::global(cx).theme_name().as_str(), "Tokyo Night");
            select_theme(ThemeMode::Dark, "Matrix".into(), window, cx).unwrap();
            assert_eq!(Theme::global(cx).theme_name().as_str(), "Matrix");
            assert_eq!(Appearance::mode(cx), AppearanceMode::Dark);
            assert_eq!(Theme::global(cx).radius, px(0.));
            assert_tokens(cx);
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn system_uses_selected_pair_and_repeated_sync_resets_metrics(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("appearance.json");
        cx.update(|cx| setup(cx, &path));
        let handle = cx.add_window(|_, _| TestView);
        cx.update_window(handle.into(), |_, window, cx| {
            assert_eq!(ThemeMode::from(window.appearance()), ThemeMode::Light);
            select_theme(ThemeMode::Light, "Gruvbox Light".into(), window, cx).unwrap();
            select_theme(ThemeMode::Dark, "Matrix".into(), window, cx).unwrap();
            select_mode(AppearanceMode::System, window, cx).unwrap();
            assert_eq!(Appearance::mode(cx), AppearanceMode::System);
            assert_eq!(Theme::global(cx).theme_name().as_str(), "Gruvbox Light");
            assert_eq!(Theme::global(cx).radius, Theme::default().radius);
            let before = Theme::global(cx).clone();
            select_theme(ThemeMode::Dark, "Tokyo Night".into(), window, cx).unwrap();
            assert_eq!(Theme::global(cx).colors, before.colors);
            assert_eq!(Theme::global(cx).radius, before.radius);
            assert_eq!(Appearance::mode(cx), AppearanceMode::System);
            select_theme(ThemeMode::Light, "Ayu Light".into(), window, cx).unwrap();
            assert_eq!(Theme::global(cx).theme_name().as_str(), "Ayu Light");
            Theme::update(cx, |theme| {
                theme.font_size = px(40.);
                theme.mono_font_size = px(30.);
                theme.radius = px(99.);
                theme.radius_lg = px(99.);
                theme.shadow = !Theme::default().shadow;
                theme.highlight_theme = HighlightTheme::default_dark();
            });
            sync_system(window, cx);
            let theme = Theme::global(cx);
            assert_eq!(theme.font_size, Theme::default().font_size);
            assert_eq!(theme.mono_font_size, Theme::default().mono_font_size);
            assert_eq!(theme.radius, Theme::default().radius);
            assert_eq!(theme.radius_lg, Theme::default().radius_lg);
            assert_eq!(theme.shadow, Theme::default().shadow);
            assert_eq!(theme.theme_name().as_str(), "Ayu Light");
            assert_eq!(theme.dark_theme.name.as_str(), "Tokyo Night");
            assert_tokens(cx);
            assert_eq!(load(&path, cx).unwrap().mode, AppearanceMode::System);
            select_mode(AppearanceMode::Dark, window, cx).unwrap();
            sync_system(window, cx);
            assert_eq!(Theme::global(cx).theme_name().as_str(), "Tokyo Night");
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn custom_pairs_reset_omitted_metrics_and_highlights_in_both_directions(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("appearance.json");
        cx.update(|cx| {
            setup(cx, &path);
            ThemeRegistry::global_mut(cx)
                .load_themes_from_str(
                    r##"{"name":"Custom","themes":[
                {"name":"Custom Light","mode":"light","font.family":"Custom Sans","mono_font.family":"Custom Mono",
                 "font.size":22,"mono_font.size":19,"radius":2,"radius.lg":3,"shadow":false,
                 "colors":{"background":"#123456","sidebar":"#234567","primary":"#345678"}},
                {"name":"Custom Dark","mode":"dark","colors":{"background":"#654321","sidebar":"#765432","primary":"#876543"}}
            ]}"##,
                )
                .unwrap();
            cx.global_mut::<Appearance>().light_theme = "Custom Light".to_owned();
            cx.global_mut::<Appearance>().dark_theme = "Custom Dark".to_owned();
            let light = theme_config(ThemeMode::Light, "Custom Light", cx).unwrap();
            let dark = theme_config(ThemeMode::Dark, "Custom Dark", cx).unwrap();
            // Exercise the destination-mode path used by system events; the headless
            // platform exposes no public appearance-change simulator.
            for mode in [ThemeMode::Light, ThemeMode::Dark, ThemeMode::Light, ThemeMode::Dark] {
                apply_pair(mode, light.clone(), dark.clone(), cx);
                let theme = Theme::global(cx);
                if mode.is_dark() {
                    assert_eq!(theme.theme_name().as_str(), "Custom Dark");
                    assert_eq!(theme.font_size, Theme::default().font_size);
                    assert_eq!(theme.mono_font_size, Theme::default().mono_font_size);
                    assert_ne!(theme.font_family.as_str(), "Custom Sans");
                    assert_ne!(theme.mono_font_family.as_str(), "Custom Mono");
                    assert_eq!(theme.radius, Theme::default().radius);
                    assert_eq!(theme.radius_lg, Theme::default().radius_lg);
                    assert_eq!(theme.shadow, Theme::default().shadow);
                    assert_eq!(theme.highlight_theme.name, HighlightTheme::default_dark().name);
                } else {
                    assert_eq!(theme.theme_name().as_str(), "Custom Light");
                    assert_eq!(theme.font_family.as_str(), "Custom Sans");
                    assert_eq!(theme.mono_font_family.as_str(), "Custom Mono");
                    assert_eq!(theme.font_size, px(22.));
                    assert_eq!(theme.mono_font_size, px(19.));
                    assert_eq!(theme.radius, px(2.));
                    assert_eq!(theme.radius_lg, px(3.));
                    assert!(!theme.shadow);
                    assert_eq!(theme.highlight_theme.name, HighlightTheme::default_light().name);
                }
                assert_tokens(cx);
            }
            let preferences = cx.global::<Appearance>();
            config::write_json(&path, preferences).unwrap();
            let loaded = load(&path, cx).unwrap();
            assert_eq!(loaded.light_theme, "Custom Light");
            assert_eq!(loaded.dark_theme, "Custom Dark");
        });
    }

    #[gpui_kit::test]
    fn all_bundled_themes_update_editor_surface_and_base_tokens(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            setup(cx, &directory.path().join("appearance.json"));
            let configs: Vec<_> = ThemeRegistry::global(cx).sorted_themes().into_iter().cloned().collect();
            for config in configs {
                let mode = config.mode;
                let light = if mode.is_dark() {
                    theme_config(ThemeMode::Light, DEFAULT_LIGHT_THEME, cx).unwrap()
                } else {
                    config.clone()
                };
                let dark = if mode.is_dark() {
                    config.clone()
                } else {
                    theme_config(ThemeMode::Dark, DEFAULT_DARK_THEME, cx).unwrap()
                };
                apply_pair(mode, light, dark, cx);
                assert_eq!(Theme::global(cx).theme_name(), &config.name);
                assert_eq!(
                    Theme::global(cx).radius,
                    config.radius.map_or(Theme::default().radius, |radius| px(radius as f32))
                );
                assert_tokens(cx);
            }
        });
    }

    #[gpui_kit::test]
    fn invalid_selections_do_not_mutate_preferences_theme_or_disk(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("appearance.json");
        cx.update(|cx| {
            setup(cx, &path);
            save(cx).unwrap();
        });
        let handle = cx.add_window(|_, _| TestView);
        cx.update_window(handle.into(), |_, window, cx| {
            let saved = fs::read(&path).unwrap();
            let before = serde_json::to_value(cx.global::<Appearance>()).unwrap();
            for (mode, name, error) in [
                (ThemeMode::Light, "Removed", "Theme \"Removed\" is unavailable."),
                (ThemeMode::Light, "Ayu Dark", "Theme \"Ayu Dark\" is dark, not light."),
                (ThemeMode::Dark, "Ayu Light", "Theme \"Ayu Light\" is light, not dark."),
            ] {
                assert_eq!(select_theme(mode, name.into(), window, cx).unwrap_err().to_string(), error);
                assert_eq!(serde_json::to_value(cx.global::<Appearance>()).unwrap(), before);
                assert_eq!(Theme::global(cx).theme_name().as_str(), DEFAULT_DARK_THEME);
                assert_eq!(fs::read(&path).unwrap(), saved);
            }
            cx.global_mut::<Appearance>().light_theme = "Removed".to_owned();
            assert!(select_mode(AppearanceMode::Light, window, cx).is_err());
            assert_eq!(Appearance::mode(cx), AppearanceMode::Dark);
            assert_eq!(Theme::global(cx).theme_name().as_str(), DEFAULT_DARK_THEME);
            assert_eq!(fs::read(&path).unwrap(), saved);
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn malformed_settings_recovery_retains_the_original_save_path(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("appearance.json");
        cx.update(|cx| {
            setup(cx, &path);
            for content in ["invalid", r#"{"mode":"invalid"}"#, r#"{"theme":42}"#, r#"{"light_theme":null}"#] {
                fs::write(&path, content).unwrap();
                assert!(load(&path, cx).is_err());
                let recovered = load_or_default(&path, cx);
                assert_eq!(recovered.path.as_deref(), Some(path.as_path()));
                cx.set_global(recovered);
                save(cx).unwrap();
                assert_eq!(load(&path, cx).unwrap().mode, AppearanceMode::Dark);
            }
            let unreadable = directory.path().join("is-a-directory");
            fs::create_dir(&unreadable).unwrap();
            assert!(load(&unreadable, cx).unwrap_err().to_string().contains("Could not read"));
            assert_eq!(load_or_default(&unreadable, cx).path.as_deref(), Some(unreadable.as_path()));
        });
    }

    #[gpui_kit::test]
    fn persistence_errors_report_unsaved_changes_without_reverting_state(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("appearance.json");
        cx.update(|cx| {
            setup(cx, &path);
            fs::create_dir(&path).unwrap();
        });
        let handle = cx.add_window(|_, _| TestView);
        cx.update_window(handle.into(), |_, window, cx| {
            let error = select_mode(AppearanceMode::Light, window, cx).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("Appearance changed, but the preference could not be saved")
            );
            assert_eq!(Appearance::mode(cx), AppearanceMode::Light);
            assert_eq!(Theme::global(cx).theme_name().as_str(), DEFAULT_LIGHT_THEME);
            let error = select_theme(ThemeMode::Light, "Gruvbox Light".into(), window, cx).unwrap_err();
            assert!(error.to_string().contains("Theme changed, but the preference could not be saved"));
            assert_eq!(Appearance::selected_theme(ThemeMode::Light, cx), "Gruvbox Light");
            assert_eq!(Theme::global(cx).theme_name().as_str(), "Gruvbox Light");
            assert_eq!(cx.global::<Appearance>().path.as_deref(), Some(path.as_path()));
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn invalid_custom_file_does_not_block_valid_custom_theme(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("broken.json"), "invalid").unwrap();
        fs::write(
            directory.path().join("custom.json"),
            r##"{
            "name":"Custom","themes":[{"name":"Custom Dark","mode":"dark","colors":{"background":"#123456"}}]
        }"##,
        )
        .unwrap();
        cx.update(|cx| {
            gpui_kit::init(cx);
            register_bundled(cx).unwrap();
            load_custom(directory.path(), cx).unwrap();
            cx.set_global(Appearance {
                dark_theme: "Custom Dark".to_owned(),
                ..Appearance::default()
            });
            apply(AppearanceMode::Dark, None, cx).unwrap();
            assert_eq!(Theme::global(cx).theme_name().as_str(), "Custom Dark");
            assert_tokens(cx);
        });
    }
}
