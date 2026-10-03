//! Application theme selection and GPUI Kit theme-set loading.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow};
use gpui_kit::component::{Theme, ThemeMode, ThemeRegistry, highlighter::HighlightTheme};
use gpui_kit::{App, Global, SharedString, Window};
use serde::{Deserialize, Serialize};

use crate::config;

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Appearance {
    theme: Option<String>,
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl Global for Appearance {}

impl Appearance {
    pub(crate) fn selected(cx: &App) -> Option<&str> {
        cx.global::<Self>().theme.as_deref()
    }
}

pub(crate) fn register_bundled(cx: &mut App) -> Result<()> {
    for content in [
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
    let preferences = match config::config_path().and_then(|path| {
        load_custom(&path.with_file_name("themes"), cx)?;
        load(&path.with_file_name("appearance.json"))
    }) {
        Ok(preferences) => preferences,
        Err(error) => {
            eprintln!("Could not load appearance settings: {error:#}");
            Appearance::default()
        }
    };
    cx.set_global(preferences);
    let selected = Appearance::selected(cx).map(SharedString::from);
    if let Err(error) = apply(selected.as_ref(), None, cx) {
        eprintln!("{error:#}");
        cx.set_global(Appearance::default());
        _ = apply(None, None, cx);
    }
}

pub(crate) fn load(path: &Path) -> Result<Appearance> {
    let mut preferences: Appearance = match fs::read(path) {
        Ok(data) => serde_json::from_slice(&data).context("Could not parse appearance.json."),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Appearance::default()),
        Err(error) => Err(error).context("Could not read appearance.json."),
    }?;
    preferences.path = Some(path.to_path_buf());
    Ok(preferences)
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

fn apply(name: Option<&SharedString>, window: Option<&mut Window>, cx: &mut App) -> Result<()> {
    if let Some(name) = name {
        let theme = ThemeRegistry::global(cx)
            .themes()
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow!("Theme \"{name}\" is unavailable."))?;
        Theme::update(cx, |current| {
            reset_metrics(current, theme.mode);
            current.apply_config(&theme);
        });
    } else {
        let registry = ThemeRegistry::global(cx);
        let light = registry
            .themes()
            .get(&SharedString::from("Ayu Light"))
            .cloned()
            .unwrap_or_else(|| registry.default_light_theme().clone());
        let dark = registry
            .themes()
            .get(&SharedString::from("Ayu Dark"))
            .cloned()
            .unwrap_or_else(|| registry.default_dark_theme().clone());
        Theme::update(cx, |theme| {
            reset_metrics(theme, theme.mode);
            theme.light_theme = light;
            theme.dark_theme = dark;
        });
        Theme::sync_system_appearance(window, cx);
    }
    Ok(())
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

pub(crate) fn select(name: Option<SharedString>, window: &mut Window, cx: &mut App) -> Result<()> {
    apply(name.as_ref(), Some(window), cx)?;
    let preferences = cx.global_mut::<Appearance>();
    preferences.theme = name.map(|name| name.to_string());
    let path = match &preferences.path {
        Some(path) => path.clone(),
        None => config::config_path()?.with_file_name("appearance.json"),
    };
    config::write_json(&path, cx.global::<Appearance>())
        .context("Theme changed, but the preference could not be saved.")
}

pub(crate) fn sync_system(window: &mut Window, cx: &mut App) {
    if Appearance::selected(cx).is_none() {
        Theme::sync_system_appearance(Some(window), cx);
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::{TestAppContext, px};

    use super::*;

    #[gpui_kit::test]
    fn switching_themes_resets_geometry_and_updates_editor_and_surface_tokens(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            register_bundled(cx).unwrap();
            apply(Some(&"Matrix".into()), None, cx).unwrap();
            assert_eq!(Theme::global(cx).radius, px(0.));
            let names: Vec<_> = ThemeRegistry::global(cx)
                .sorted_themes()
                .iter()
                .map(|theme| theme.name.clone())
                .collect();
            for name in names {
                apply(Some(&name), None, cx).unwrap();
                let theme = Theme::global(cx);
                assert_eq!(theme.theme_name(), &name);
                assert_eq!(theme.mode, theme.highlight_theme.appearance);
                assert_eq!(theme.sidebar, theme.tokens.sidebar.color);
                assert_eq!(theme.primary, theme.tokens.primary.color);
                if name != "Matrix" {
                    assert_eq!(theme.radius, Theme::default().radius);
                }
            }
            apply(None, None, cx).unwrap();
            let theme = Theme::global(cx);
            assert!(matches!(
                theme.theme_name().as_str(),
                "Ayu Light" | "Ayu Dark"
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
            "name": "Custom", "themes": [{
                "name": "Custom Dark", "mode": "dark",
                "colors": { "background": "#123456" }
            }]
        }"##,
        )
        .unwrap();
        cx.update(|cx| {
            gpui_kit::init(cx);
            load_custom(directory.path(), cx).unwrap();
            apply(Some(&"Custom Dark".into()), None, cx).unwrap();
            let theme = Theme::global(cx);
            assert_eq!(theme.theme_name().as_str(), "Custom Dark");
            assert_eq!(theme.mode, theme.highlight_theme.appearance);
        });
    }
}
