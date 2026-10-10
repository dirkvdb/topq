//! English fallback copy for appearance settings; backend displays remain stable codes.

use gpui_kit::component::ThemeMode;

use crate::{
    appearance::{AppearanceMode, ReduceMotion},
    error::appearance::AppearanceError,
    ui::messages::LocalizedError,
};

impl AppearanceMode {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }
}

impl ReduceMotion {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::On => "On",
            Self::Off => "Off",
        }
    }
}

fn theme_mode_label(mode: ThemeMode) -> &'static str {
    match mode {
        ThemeMode::Light => "light",
        ThemeMode::Dark => "dark",
    }
}

impl LocalizedError for AppearanceError {
    fn localized_message(&self) -> String {
        match self {
            Self::Config(error) => error.localized_message(),
            Self::ParseSettings { .. } => "Could not parse appearance settings.".into(),
            Self::LoadBundledTheme { .. } => "Could not load a bundled theme.".into(),
            Self::ReadThemesDirectory { .. } => "Could not read the themes directory.".into(),
            Self::ReadThemeEntry { .. } => "Could not read an entry in the themes directory.".into(),
            Self::ReadCustomTheme { .. } => "Could not read a custom theme file.".into(),
            Self::LoadCustomTheme { .. } => "Could not load a custom theme.".into(),
            Self::ThemeUnavailable { name, .. } => format!("Theme \"{name}\" is unavailable."),
            Self::ThemeModeMismatch { name, expected, actual } => {
                let expected = theme_mode_label(*expected);
                let actual = theme_mode_label(*actual);
                format!("Theme \"{name}\" is {actual}, not {expected}.")
            }
            Self::AppearanceNotSaved { .. } => "Appearance changed, but the preference could not be saved.".into(),
            Self::ThemeNotSaved { .. } => "Theme changed, but the preference could not be saved.".into(),
            Self::ReduceMotionNotSaved { .. } => "Reduce motion changed, but the preference could not be saved.".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::config::{ConfigDiagnostic, ConfigError};

    #[test]
    fn appearance_mode_labels_preserve_the_ui_method_api() {
        for (mode, label) in [
            (AppearanceMode::System, "System"),
            (AppearanceMode::Light, "Light"),
            (AppearanceMode::Dark, "Dark"),
        ] {
            assert_eq!(mode.label(), label);
        }
    }

    #[test]
    fn reduce_motion_labels_preserve_the_ui_method_api() {
        for (state, label) in [
            (ReduceMotion::System, "System"),
            (ReduceMotion::On, "On"),
            (ReduceMotion::Off, "Off"),
        ] {
            assert_eq!(state.label(), label);
        }
    }

    #[test]
    fn selection_fallbacks_use_theme_data_and_ui_owned_mode_names() {
        let errors = [
            (
                AppearanceError::ThemeUnavailable {
                    name: "Removed".into(),
                    mode: ThemeMode::Light,
                },
                "Theme \"Removed\" is unavailable.",
            ),
            (
                AppearanceError::ThemeModeMismatch {
                    name: "Ayu Dark".into(),
                    expected: ThemeMode::Light,
                    actual: ThemeMode::Dark,
                },
                "Theme \"Ayu Dark\" is dark, not light.",
            ),
            (
                AppearanceError::ThemeModeMismatch {
                    name: "Ayu Light".into(),
                    expected: ThemeMode::Dark,
                    actual: ThemeMode::Light,
                },
                "Theme \"Ayu Light\" is light, not dark.",
            ),
        ];
        for (error, message) in errors {
            assert_eq!(error.localized_message(), message);
            assert_eq!(error.to_string(), error.code());
        }
    }

    #[test]
    fn unsaved_change_fallbacks_do_not_concatenate_source_messages() {
        let source = || {
            Box::new(AppearanceError::Config(ConfigError::ReadConfig {
                path: "/private/settings".into(),
                source: std::io::Error::other("dependency prose"),
            }))
        };
        for (error, message) in [
            (
                AppearanceError::AppearanceNotSaved { source: source() },
                "Appearance changed, but the preference could not be saved.",
            ),
            (
                AppearanceError::ThemeNotSaved { source: source() },
                "Theme changed, but the preference could not be saved.",
            ),
            (
                AppearanceError::ReduceMotionNotSaved { source: source() },
                "Reduce motion changed, but the preference could not be saved.",
            ),
        ] {
            assert_eq!(error.localized_message(), message);
            assert_eq!(error.to_string(), error.code());
            assert!(std::error::Error::source(&error).is_some());
        }
    }

    #[test]
    fn loading_fallbacks_do_not_display_paths_or_dependency_sources() {
        let path = "/private/settings";
        let diagnostic = || ConfigDiagnostic::Json {
            category: serde_json::error::Category::Data,
            line: 2,
            column: 3,
        };
        for (error, message) in [
            (
                AppearanceError::Config(ConfigError::ReadConfig {
                    path: path.into(),
                    source: std::io::Error::other("dependency prose"),
                }),
                "Could not read the configuration file.",
            ),
            (
                AppearanceError::ParseSettings {
                    path: path.into(),
                    source: diagnostic(),
                },
                "Could not parse appearance settings.",
            ),
            (
                AppearanceError::LoadBundledTheme {
                    bundle: path,
                    source: Some(diagnostic()),
                },
                "Could not load a bundled theme.",
            ),
            (
                AppearanceError::ReadThemesDirectory {
                    path: path.into(),
                    source: std::io::Error::other("dependency prose"),
                },
                "Could not read the themes directory.",
            ),
            (
                AppearanceError::ReadThemeEntry {
                    path: path.into(),
                    source: std::io::Error::other("dependency prose"),
                },
                "Could not read an entry in the themes directory.",
            ),
            (
                AppearanceError::ReadCustomTheme {
                    path: path.into(),
                    source: std::io::Error::other("dependency prose"),
                },
                "Could not read a custom theme file.",
            ),
            (
                AppearanceError::LoadCustomTheme {
                    path: path.into(),
                    source: Some(diagnostic()),
                },
                "Could not load a custom theme.",
            ),
        ] {
            assert_eq!(error.localized_message(), message);
            assert_eq!(error.to_string(), error.code());
        }
    }
}
