//! Application theme selection and GPUI Kit theme-set loading.

use std::{
    fs,
    path::{Path, PathBuf},
    rc::Rc,
};

use gpui_kit::component::{Theme, ThemeConfig, ThemeMode, ThemeRegistry, highlighter::HighlightTheme};
use gpui_kit::{App, Global, SharedString, Window};
use serde::{Deserialize, Serialize};

use crate::{config, error::appearance::AppearanceError};

const DEFAULT_LIGHT_THEME: &str = "Ayu Light";
const DEFAULT_DARK_THEME: &str = "Ayu Dark";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AppearanceMode {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReduceMotion {
    #[default]
    System,
    On,
    Off,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Appearance {
    mode: AppearanceMode,
    light_theme: String,
    dark_theme: String,
    reduce_motion: ReduceMotion,
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            mode: AppearanceMode::System,
            light_theme: DEFAULT_LIGHT_THEME.to_owned(),
            dark_theme: DEFAULT_DARK_THEME.to_owned(),
            reduce_motion: ReduceMotion::System,
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

    pub(crate) fn reduce_motion(cx: &App) -> ReduceMotion {
        cx.global::<Self>().reduce_motion
    }

    pub(crate) fn motion_reduced(cx: &App) -> bool {
        match Self::reduce_motion(cx) {
            ReduceMotion::System => cx.reduce_motion(),
            ReduceMotion::On => true,
            ReduceMotion::Off => false,
        }
    }

    fn theme(&self, mode: ThemeMode) -> &str {
        match mode {
            ThemeMode::Light => &self.light_theme,
            ThemeMode::Dark => &self.dark_theme,
        }
    }

    fn set_theme_for_mode(&mut self, mode: ThemeMode, name: String) {
        match mode {
            ThemeMode::Light => self.light_theme = name,
            ThemeMode::Dark => self.dark_theme = name,
        }
    }

    // Update the current preferences to use a valid theme for each mode
    // in case of custom theme renames of removals
    fn repair_invalid_themes(&mut self, cx: &App) {
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
                tracing::warn!(
                    error_code = error.code(),
                    diagnostic = ?error,
                    mode = ?mode,
                    theme = self.theme(mode),
                    fallback = %fallback.name,
                    "appearance.invalid_theme_repaired"
                );
                self.set_theme_for_mode(mode, fallback.name.to_string());
            }
        }
    }
}

pub(crate) fn register_bundled(cx: &mut App) -> Result<(), AppearanceError> {
    for (bundle, content) in crate::bundled_themes::THEMES {
        ThemeRegistry::global_mut(cx)
            .load_themes_from_str(content)
            .map_err(|error| AppearanceError::LoadBundledTheme {
                bundle,
                source: error.downcast::<serde_json::Error>().ok().map(Into::into),
            })?;
    }
    Ok(())
}

pub(crate) fn init(cx: &mut App) {
    if let Err(error) = register_bundled(cx) {
        tracing::error!(error_code = error.code(), diagnostic = ?error, "appearance.bundled_themes.load_failed");
    }
    if let Err(error) = load_custom_themes(cx) {
        tracing::warn!(error_code = error.code(), diagnostic = ?error, "appearance.custom_themes.load_failed");
    }
    let mut preferences = match config::application_config_read_path() {
        Ok(path) => load_or_default(&path, cx),
        Err(error) => {
            tracing::warn!(error_code = error.code(), "appearance.settings.path_unavailable");
            Appearance::default()
        }
    };
    preferences.repair_invalid_themes(cx);
    let mode = preferences.mode;
    cx.set_global(preferences);
    if let Err(error) = apply(mode, None, cx) {
        tracing::error!(error_code = error.code(), diagnostic = ?error, "appearance.settings.apply_failed");
    }
}

pub(crate) fn load(path: &Path, cx: &App) -> Result<Appearance, AppearanceError> {
    let mut preferences = match config::read_config_value(path)? {
        Some(value) => serde_json::from_value(value).map_err(|error| AppearanceError::ParseSettings {
            path: path.to_owned(),
            source: error.into(),
        })?,
        None => Appearance::default(),
    };
    preferences.path = Some(path.to_path_buf());
    preferences.repair_invalid_themes(cx);
    Ok(preferences)
}

fn load_or_default(path: &Path, cx: &App) -> Appearance {
    load(path, cx).unwrap_or_else(|error| {
        tracing::warn!(error_code = error.code(), diagnostic = ?error, path = %path.display(), "appearance.settings.load_failed");
        let mut preferences = Appearance {
            path: Some(path.to_path_buf()),
            ..Appearance::default()
        };
        preferences.repair_invalid_themes(cx);
        preferences
    })
}

pub(crate) fn load_custom_themes(cx: &mut App) -> Result<(), AppearanceError> {
    load_custom(&config::config_path()?.with_file_name("themes"), cx)
}

fn load_custom(directory: &Path, cx: &mut App) -> Result<(), AppearanceError> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(AppearanceError::ReadThemesDirectory {
                path: directory.to_owned(),
                source,
            });
        }
    };
    let mut paths = entries
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|source| AppearanceError::ReadThemeEntry {
            path: directory.to_owned(),
            source,
        })?;
    paths.sort();
    for path in paths {
        if !path.is_file() || path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        // One malformed custom theme must not prevent other themes from loading.
        if let Err(error) = load_custom_theme(&path, cx) {
            tracing::warn!(error_code = error.code(), diagnostic = ?error, path = %path.display(), "appearance.custom_theme.skipped");
        }
    }
    Ok(())
}

fn load_custom_theme(path: &Path, cx: &mut App) -> Result<(), AppearanceError> {
    let content = fs::read_to_string(path).map_err(|source| AppearanceError::ReadCustomTheme {
        path: path.to_owned(),
        source,
    })?;
    ThemeRegistry::global_mut(cx)
        .load_themes_from_str(&content)
        .map_err(|error| AppearanceError::LoadCustomTheme {
            path: path.to_owned(),
            // Dependency parse errors can echo theme values; retain only safe diagnostics.
            source: error.downcast::<serde_json::Error>().ok().map(Into::into),
        })
}

fn theme_config(mode: ThemeMode, name: &str, cx: &App) -> Result<Rc<ThemeConfig>, AppearanceError> {
    let theme = ThemeRegistry::global(cx)
        .themes()
        .get(&SharedString::from(name.to_owned()))
        .cloned()
        .ok_or_else(|| AppearanceError::ThemeUnavailable {
            name: name.to_owned(),
            mode,
        })?;
    if theme.mode != mode {
        return Err(AppearanceError::ThemeModeMismatch {
            name: name.to_owned(),
            expected: mode,
            actual: theme.mode,
        });
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

fn apply(mode: AppearanceMode, window: Option<&mut Window>, cx: &mut App) -> Result<(), AppearanceError> {
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

pub(crate) fn select_mode(mode: AppearanceMode, window: &mut Window, cx: &mut App) -> Result<(), AppearanceError> {
    apply(mode, Some(window), cx)?;
    cx.global_mut::<Appearance>().mode = mode;
    save(cx).map_err(|source| AppearanceError::AppearanceNotSaved { source: Box::new(source) })
}

pub(crate) fn select_theme(mode: ThemeMode, name: SharedString, window: &mut Window, cx: &mut App) -> Result<(), AppearanceError> {
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
    cx.global_mut::<Appearance>().set_theme_for_mode(mode, name.to_string());
    save(cx).map_err(|source| AppearanceError::ThemeNotSaved { source: Box::new(source) })
}

pub(crate) fn set_reduce_motion(reduce_motion: ReduceMotion, cx: &mut App) -> Result<(), AppearanceError> {
    cx.global_mut::<Appearance>().reduce_motion = reduce_motion;
    save(cx).map_err(|source| AppearanceError::ReduceMotionNotSaved { source: Box::new(source) })
}

fn save(cx: &mut App) -> Result<(), AppearanceError> {
    let preferred_path = match &cx.global::<Appearance>().path {
        Some(path) => path.clone(),
        None => config::config_path()?,
    };
    let path = config::update_application_config(&preferred_path, cx.global::<Appearance>())?;
    cx.global_mut::<Appearance>().path = Some(path);
    Ok(())
}

pub(crate) fn sync_system(window: &mut Window, cx: &mut App) {
    if Appearance::mode(cx) == AppearanceMode::System
        && let Err(error) = apply(AppearanceMode::System, Some(window), cx)
    {
        tracing::warn!(error_code = error.code(), diagnostic = ?error, "appearance.system.sync_failed");
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::{AppContext, Context, IntoElement, Render, TestAppContext, div, px};

    use super::*;
    use crate::error::config::{ConfigDiagnostic, ConfigError};

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
    fn defaults_and_modes_have_stable_serialization() {
        let preferences = Appearance::default();
        assert_eq!(AppearanceMode::default(), AppearanceMode::System);
        assert_eq!(
            serde_json::to_value(preferences).unwrap(),
            serde_json::json!({"mode": "system", "light_theme": "Ayu Light", "dark_theme": "Ayu Dark", "reduce_motion": "system"})
        );
        for (mode, serialized) in [
            (AppearanceMode::System, "system"),
            (AppearanceMode::Light, "light"),
            (AppearanceMode::Dark, "dark"),
        ] {
            assert_eq!(serde_json::to_value(mode).unwrap(), serialized);
            assert_eq!(serde_json::from_value::<AppearanceMode>(serialized.into()).unwrap(), mode);
        }
    }

    #[test]
    fn reduce_motion_states_have_stable_serialization() {
        assert_eq!(ReduceMotion::default(), ReduceMotion::System);
        for (state, serialized) in [
            (ReduceMotion::System, "system"),
            (ReduceMotion::On, "on"),
            (ReduceMotion::Off, "off"),
        ] {
            assert_eq!(serde_json::to_value(state).unwrap(), serialized);
            assert_eq!(serde_json::from_value::<ReduceMotion>(serialized.into()).unwrap(), state);
        }
    }

    #[test]
    fn reduce_motion_rejects_invalid_values() {
        for value in [
            serde_json::json!("invalid"),
            serde_json::json!("On"),
            serde_json::json!("true"),
            serde_json::json!("false"),
            serde_json::json!(true),
            serde_json::json!(false),
            serde_json::json!(null),
            serde_json::json!(0),
            serde_json::json!(1),
            serde_json::json!([]),
            serde_json::json!({}),
        ] {
            assert!(
                serde_json::from_value::<Appearance>(serde_json::json!({"reduce_motion": value})).is_err(),
                "Invalid reduce_motion: {value}"
            );
        }
    }

    #[gpui_kit::test]
    fn reduce_motion_defaults_to_system_when_unspecified(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("appearance.json");
        cx.update(|cx| {
            setup(cx, &path);
            assert_eq!(Appearance::reduce_motion(cx), ReduceMotion::System);
            assert_eq!(Appearance::default().reduce_motion, ReduceMotion::System);
            for content in ["{}", r#"{"mode":"light","light_theme":"Ayu Light","dark_theme":"Ayu Dark"}"#] {
                fs::write(&path, content).unwrap();
                cx.set_global(load(&path, cx).unwrap());
                assert_eq!(
                    Appearance::reduce_motion(cx),
                    ReduceMotion::System,
                    "Settings without reduce_motion: {content}"
                );
            }
        });
    }

    #[gpui_kit::test]
    fn reduce_motion_persists_all_three_states(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("appearance.json");
        cx.update(|cx| {
            setup(cx, &path);
            for (reduce_motion, serialized) in [
                (ReduceMotion::System, "system"),
                (ReduceMotion::On, "on"),
                (ReduceMotion::Off, "off"),
            ] {
                set_reduce_motion(reduce_motion, cx).unwrap();
                assert_eq!(Appearance::reduce_motion(cx), reduce_motion);
                let saved: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                assert_eq!(saved["reduce_motion"], serialized);
                let reloaded = load(&path, cx).unwrap();
                assert_eq!(reloaded.path.as_deref(), Some(path.as_path()));
                cx.set_global(reloaded);
                assert_eq!(Appearance::reduce_motion(cx), reduce_motion);
                assert_eq!(Appearance::mode(cx), AppearanceMode::System);
                assert_eq!(Appearance::selected_theme(ThemeMode::Light, cx), DEFAULT_LIGHT_THEME);
                assert_eq!(Appearance::selected_theme(ThemeMode::Dark, cx), DEFAULT_DARK_THEME);
            }
        });
    }

    #[gpui_kit::test]
    fn motion_reduced_resolves_saved_and_system_preferences(cx: &mut TestAppContext) {
        cx.update(|cx| {
            for (saved, system, expected) in [
                (ReduceMotion::System, false, false),
                (ReduceMotion::System, true, true),
                (ReduceMotion::On, false, true),
                (ReduceMotion::On, true, true),
                (ReduceMotion::Off, false, false),
                (ReduceMotion::Off, true, false),
            ] {
                cx.set_global(Appearance {
                    reduce_motion: saved,
                    ..Appearance::default()
                });
                cx.set_reduce_motion(system);
                assert_eq!(Appearance::reduce_motion(cx), saved);
                assert_eq!(Appearance::motion_reduced(cx), expected);
                assert_eq!(cx.reduce_motion(), system);
            }
        });
    }

    #[gpui_kit::test]
    fn reduce_motion_save_errors_do_not_revert_the_setting(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("appearance.json");
        cx.update(|cx| {
            setup(cx, &path);
            fs::create_dir(&path).unwrap();
            cx.set_reduce_motion(true);
            for (reduce_motion, expected) in [(ReduceMotion::On, true), (ReduceMotion::Off, false), (ReduceMotion::System, true)] {
                let error = set_reduce_motion(reduce_motion, cx).unwrap_err();
                assert_eq!(error.code(), "appearance.reduce_motion.not_saved");
                assert!(matches!(
                    error,
                    AppearanceError::ReduceMotionNotSaved { source }
                        if matches!(source.as_ref(), AppearanceError::Config(ConfigError::SaveConfig { path: saved, .. }) if saved == &path)
                ));
                assert_eq!(Appearance::reduce_motion(cx), reduce_motion);
                assert_eq!(Appearance::motion_reduced(cx), expected);
                assert!(cx.reduce_motion());
                assert_eq!(cx.global::<Appearance>().path.as_deref(), Some(path.as_path()));
            }
        });
    }

    #[gpui_kit::test]
    fn missing_or_empty_settings_use_system_appearance(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("appearance.json");
        cx.update(|cx| {
            setup(cx, &path);
            assert_eq!(Appearance::mode(cx), AppearanceMode::System);
            assert_eq!(Appearance::selected_theme(ThemeMode::Light, cx), DEFAULT_LIGHT_THEME);
            assert_eq!(Appearance::selected_theme(ThemeMode::Dark, cx), DEFAULT_DARK_THEME);
            let mode = ThemeMode::from(cx.window_appearance());
            let name = Appearance::selected_theme(mode, cx);
            assert_eq!(Theme::global(cx).mode, mode);
            assert_eq!(Theme::global(cx).theme_name().as_str(), name);
            assert_eq!(Theme::global(cx).highlight_theme.name, name);
            fs::write(&path, "{}").unwrap();
            let preferences = load(&path, cx).unwrap();
            assert_eq!(preferences.mode, AppearanceMode::System);
            assert_eq!(preferences.path.as_deref(), Some(path.as_path()));
        });
    }

    #[gpui_kit::test]
    fn unavailable_or_wrong_mode_themes_recover_independently(cx: &mut TestAppContext) {
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
        fs::write(&path, r#"{"mode":"dark"}"#).unwrap();
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
            assert_eq!(saved.as_object().unwrap().len(), 4);
            assert_eq!(saved["reduce_motion"], "system");
            select_mode(AppearanceMode::Dark, window, cx).unwrap();
            assert_eq!(Theme::global(cx).theme_name().as_str(), "Tokyo Night");
            select_theme(ThemeMode::Dark, "Gruvbox Dark".into(), window, cx).unwrap();
            assert_eq!(Theme::global(cx).theme_name().as_str(), "Gruvbox Dark");
            assert_eq!(Appearance::mode(cx), AppearanceMode::Dark);
            assert_eq!(Theme::global(cx).radius, Theme::default().radius);
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
            select_theme(ThemeMode::Dark, "Gruvbox Dark".into(), window, cx).unwrap();
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
            config::update_json(&path, preferences).unwrap();
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
        fs::write(&path, r#"{"mode":"dark"}"#).unwrap();
        cx.update(|cx| {
            setup(cx, &path);
            save(cx).unwrap();
        });
        let handle = cx.add_window(|_, _| TestView);
        cx.update_window(handle.into(), |_, window, cx| {
            let saved = fs::read(&path).unwrap();
            let before = serde_json::to_value(cx.global::<Appearance>()).unwrap();
            for (mode, name, code) in [
                (ThemeMode::Light, "Removed", "appearance.theme.unavailable"),
                (ThemeMode::Light, "Ayu Dark", "appearance.theme.mode_mismatch"),
                (ThemeMode::Dark, "Ayu Light", "appearance.theme.mode_mismatch"),
            ] {
                let error = select_theme(mode, name.into(), window, cx).unwrap_err();
                assert_eq!(error.code(), code);
                match error {
                    AppearanceError::ThemeUnavailable {
                        name: unavailable,
                        mode: requested,
                    } => {
                        assert_eq!(unavailable, name);
                        assert_eq!(requested, mode);
                    }
                    AppearanceError::ThemeModeMismatch {
                        name: mismatched,
                        expected,
                        actual,
                    } => {
                        assert_eq!(mismatched, name);
                        assert_eq!(expected, mode);
                        assert_ne!(actual, mode);
                    }
                    error => panic!("Unexpected error: {error:?}"),
                }
                assert_eq!(serde_json::to_value(cx.global::<Appearance>()).unwrap(), before);
                assert_eq!(Theme::global(cx).theme_name().as_str(), DEFAULT_DARK_THEME);
                assert_eq!(fs::read(&path).unwrap(), saved);
            }
            cx.global_mut::<Appearance>().light_theme = "Removed".to_owned();
            assert!(matches!(
                select_mode(AppearanceMode::Light, window, cx),
                Err(AppearanceError::ThemeUnavailable {
                    mode: ThemeMode::Light,
                    ..
                })
            ));
            assert_eq!(Appearance::mode(cx), AppearanceMode::Dark);
            assert_eq!(Theme::global(cx).theme_name().as_str(), DEFAULT_DARK_THEME);
            assert_eq!(fs::read(&path).unwrap(), saved);
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn default_save_falls_back_to_config_path_and_roundtrips(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let setup_path = directory.path().join("appearance.json");
        cx.update(|cx| {
            setup(cx, &setup_path);
            cx.set_global(Appearance::default());
            assert!(cx.global::<Appearance>().path.is_none());
            let expected_path = config::config_path().unwrap();
            assert_ne!(expected_path, setup_path);

            save(cx).unwrap();

            assert_eq!(cx.global::<Appearance>().path.as_deref(), Some(expected_path.as_path()));
            assert!(!setup_path.exists());
            let saved: serde_json::Value = serde_json::from_slice(&fs::read(&expected_path).unwrap()).unwrap();
            let expected = serde_json::to_value(Appearance::default()).unwrap();
            for (key, value) in expected.as_object().unwrap() {
                assert_eq!(&saved[key], value);
            }
            let reloaded = load(&expected_path, cx).unwrap();
            assert_eq!(reloaded.path.as_deref(), Some(expected_path.as_path()));
            assert_eq!(serde_json::to_value(reloaded).unwrap(), expected);
        });
    }

    #[gpui_kit::test]
    fn malformed_settings_recovery_retains_the_original_save_path(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("appearance.json");
        cx.update(|cx| {
            setup(cx, &path);
            for (content, code) in [
                ("invalid", "config.parse_failed"),
                (r#"{"mode":"invalid"}"#, "appearance.settings.parse_failed"),
                (r#"{"reduce_motion":true}"#, "appearance.settings.parse_failed"),
                (r#"{"light_theme":null}"#, "appearance.settings.parse_failed"),
            ] {
                fs::write(&path, content).unwrap();
                let error = load(&path, cx).unwrap_err();
                assert_eq!(error.code(), code);
                let recovered = load_or_default(&path, cx);
                assert_eq!(recovered.path.as_deref(), Some(path.as_path()));
                cx.set_global(recovered);
                save(cx).unwrap();
                assert_eq!(load(&path, cx).unwrap().mode, AppearanceMode::System);
            }
            let unreadable = directory.path().join("is-a-directory");
            fs::create_dir(&unreadable).unwrap();
            assert!(matches!(
                load(&unreadable, cx).unwrap_err(),
                AppearanceError::Config(ConfigError::ReadConfig { path, .. }) if path == unreadable
            ));
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
            assert_eq!(error.code(), "appearance.mode.not_saved");
            assert!(matches!(
                error,
                AppearanceError::AppearanceNotSaved { source }
                    if matches!(source.as_ref(), AppearanceError::Config(ConfigError::SaveConfig { path: saved, .. }) if saved == &path)
            ));
            assert_eq!(Appearance::mode(cx), AppearanceMode::Light);
            assert_eq!(Theme::global(cx).theme_name().as_str(), DEFAULT_LIGHT_THEME);
            let error = select_theme(ThemeMode::Light, "Gruvbox Light".into(), window, cx).unwrap_err();
            assert_eq!(error.code(), "appearance.theme.not_saved");
            assert!(matches!(error, AppearanceError::ThemeNotSaved { .. }));
            assert_eq!(Appearance::selected_theme(ThemeMode::Light, cx), "Gruvbox Light");
            assert_eq!(Theme::global(cx).theme_name().as_str(), "Gruvbox Light");
            assert_eq!(cx.global::<Appearance>().path.as_deref(), Some(path.as_path()));
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn startup_discovers_custom_themes_before_loading_saved_preferences(cx: &mut TestAppContext) {
        let themes = config::config_path().unwrap().with_file_name("themes");
        fs::create_dir_all(&themes).unwrap();
        fs::write(
            themes.join("custom.json"),
            r#"{"name":"Custom","themes":[
                {"name":"Custom Light","mode":"light","colors":{}},
                {"name":"Custom Dark","mode":"dark","colors":{}}
            ]}"#,
        )
        .unwrap();
        let settings = config::application_config_read_path().unwrap();
        fs::create_dir_all(settings.parent().unwrap()).unwrap();
        fs::write(
            &settings,
            r#"{"mode":"dark","light_theme":"Custom Light","dark_theme":"Custom Dark"}"#,
        )
        .unwrap();
        cx.update(|cx| {
            gpui_kit::init(cx);
            init(cx);
            assert_eq!(Appearance::selected_theme(ThemeMode::Light, cx), "Custom Light");
            assert_eq!(Appearance::selected_theme(ThemeMode::Dark, cx), "Custom Dark");
            assert_eq!(Theme::global(cx).theme_name().as_str(), "Custom Dark");
        });
        fs::remove_file(settings).unwrap();
        fs::remove_dir_all(themes).unwrap();
    }

    #[gpui_kit::test]
    fn custom_theme_scan_ignores_non_json_files_and_directories(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let content = r#"{"name":"Ignored","themes":[{"name":"Ignored Dark","mode":"dark","colors":{}}]}"#;
        fs::write(directory.path().join("ignored.txt"), content).unwrap();
        let nested = directory.path().join("nested.json");
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join("ignored.json"), content).unwrap();
        cx.update(|cx| {
            gpui_kit::init(cx);
            let count = ThemeRegistry::global(cx).themes().len();
            load_custom(directory.path(), cx).unwrap();
            load_custom(&directory.path().join("missing"), cx).unwrap();
            assert_eq!(ThemeRegistry::global(cx).themes().len(), count);
        });
    }

    #[gpui_kit::test]
    fn invalid_settings_preserve_paths_and_typed_diagnostics(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("appearance.json");
        cx.update(|cx| {
            setup(cx, &path);
            fs::write(&path, r#"{"mode":"private-parser-value"}"#).unwrap();
            let error = load(&path, cx).unwrap_err();
            assert_eq!(error.code(), "appearance.settings.parse_failed");
            assert!(matches!(
                error,
                AppearanceError::ParseSettings {
                    path: failed_path,
                    source: ConfigDiagnostic::Json { category: serde_json::error::Category::Data, .. },
                } if failed_path == path
            ));
        });
    }

    #[gpui_kit::test]
    fn custom_theme_errors_preserve_paths_and_safe_sources(cx: &mut TestAppContext) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("custom.json");
        fs::write(
            &path,
            r#"{"name":"Custom","themes":[{"name":"Custom","mode":"private-parser-value","colors":{}}]}"#,
        )
        .unwrap();
        cx.update(|cx| {
            gpui_kit::init(cx);
            let error = load_custom_theme(&path, cx).unwrap_err();
            assert_eq!(error.code(), "appearance.custom_theme.load_failed");
            assert!(matches!(
                error,
                AppearanceError::LoadCustomTheme {
                    path: failed_path,
                    source: Some(ConfigDiagnostic::Json { category: serde_json::error::Category::Data, line, column }),
                } if failed_path == path && line > 0 && column > 0
            ));
            fs::remove_file(&path).unwrap();
            assert!(matches!(
                load_custom_theme(&path, cx).unwrap_err(),
                AppearanceError::ReadCustomTheme { path: failed_path, source }
                    if failed_path == path && source.kind() == std::io::ErrorKind::NotFound
            ));
            assert!(load_custom(&directory.path().join("missing"), cx).is_ok());
            fs::write(&path, "not a directory").unwrap();
            assert!(matches!(
                load_custom(&path, cx).unwrap_err(),
                AppearanceError::ReadThemesDirectory { path: failed_path, .. } if failed_path == path
            ));
        });
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
