//! Typed appearance failures, stable error codes, and source chaining.

use std::{fmt, path::PathBuf};

use gpui_kit::component::ThemeMode;

use super::config;

#[derive(Debug)]
#[expect(dead_code, reason = "Some error context is only used by derived Debug diagnostics.")]
pub(crate) enum AppearanceError {
    Config(config::ConfigError),
    ParseSettings {
        path: PathBuf,
        source: config::ConfigDiagnostic,
    },
    LoadBundledTheme {
        bundle: &'static str,
        source: Option<config::ConfigDiagnostic>,
    },
    ReadThemesDirectory {
        path: PathBuf,
        source: std::io::Error,
    },
    ReadThemeEntry {
        path: PathBuf,
        source: std::io::Error,
    },
    ReadCustomTheme {
        path: PathBuf,
        source: std::io::Error,
    },
    LoadCustomTheme {
        path: PathBuf,
        source: Option<config::ConfigDiagnostic>,
    },
    ThemeUnavailable {
        name: String,
        mode: ThemeMode,
    },
    ThemeModeMismatch {
        name: String,
        expected: ThemeMode,
        actual: ThemeMode,
    },
    AppearanceNotSaved {
        source: Box<AppearanceError>,
    },
    ThemeNotSaved {
        source: Box<AppearanceError>,
    },
    ReduceMotionNotSaved {
        source: Box<AppearanceError>,
    },
    InlinePayloadNotSaved {
        source: Box<AppearanceError>,
    },
}

impl AppearanceError {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::Config(error) => error.code(),
            Self::ParseSettings { .. } => "appearance.settings.parse_failed",
            Self::LoadBundledTheme { .. } => "appearance.bundled_theme.load_failed",
            Self::ReadThemesDirectory { .. } => "appearance.themes.directory_read_failed",
            Self::ReadThemeEntry { .. } => "appearance.themes.entry_read_failed",
            Self::ReadCustomTheme { .. } => "appearance.custom_theme.read_failed",
            Self::LoadCustomTheme { .. } => "appearance.custom_theme.load_failed",
            Self::ThemeUnavailable { .. } => "appearance.theme.unavailable",
            Self::ThemeModeMismatch { .. } => "appearance.theme.mode_mismatch",
            Self::AppearanceNotSaved { .. } => "appearance.mode.not_saved",
            Self::ThemeNotSaved { .. } => "appearance.theme.not_saved",
            Self::ReduceMotionNotSaved { .. } => "appearance.reduce_motion.not_saved",
            Self::InlinePayloadNotSaved { .. } => "appearance.inline_payload.not_saved",
        }
    }
}

impl fmt::Display for AppearanceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for AppearanceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Config(error) => Some(error),
            Self::ParseSettings { source, .. } => Some(source),
            Self::LoadBundledTheme { source, .. } | Self::LoadCustomTheme { source, .. } => source.as_ref().map(|source| source as _),
            Self::ReadThemesDirectory { source, .. } | Self::ReadThemeEntry { source, .. } | Self::ReadCustomTheme { source, .. } => {
                Some(source)
            }
            Self::AppearanceNotSaved { source }
            | Self::ThemeNotSaved { source }
            | Self::ReduceMotionNotSaved { source }
            | Self::InlinePayloadNotSaved { source } => Some(source.as_ref()),
            Self::ThemeUnavailable { .. } | Self::ThemeModeMismatch { .. } => None,
        }
    }
}

impl From<config::ConfigError> for AppearanceError {
    fn from(error: config::ConfigError) -> Self {
        Self::Config(error)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn appearance_error_codes_and_display_are_stable() {
        let path = || PathBuf::from("settings.json");
        let diagnostic = || config::ConfigDiagnostic::Json {
            category: serde_json::error::Category::Data,
            line: 2,
            column: 3,
        };
        let config_source = || Box::new(AppearanceError::from(config::ConfigError::ConfigDirectoryUnavailable));
        for (error, code) in [
            (
                AppearanceError::from(config::ConfigError::ConfigDirectoryUnavailable),
                "config.directory_unavailable",
            ),
            (
                AppearanceError::ParseSettings {
                    path: path(),
                    source: diagnostic(),
                },
                "appearance.settings.parse_failed",
            ),
            (
                AppearanceError::LoadBundledTheme {
                    bundle: "ayu.json",
                    source: Some(diagnostic()),
                },
                "appearance.bundled_theme.load_failed",
            ),
            (
                AppearanceError::ReadThemesDirectory {
                    path: path(),
                    source: std::io::Error::other("dependency prose"),
                },
                "appearance.themes.directory_read_failed",
            ),
            (
                AppearanceError::ReadThemeEntry {
                    path: path(),
                    source: std::io::Error::other("dependency prose"),
                },
                "appearance.themes.entry_read_failed",
            ),
            (
                AppearanceError::ReadCustomTheme {
                    path: path(),
                    source: std::io::Error::other("dependency prose"),
                },
                "appearance.custom_theme.read_failed",
            ),
            (
                AppearanceError::LoadCustomTheme {
                    path: path(),
                    source: Some(diagnostic()),
                },
                "appearance.custom_theme.load_failed",
            ),
            (
                AppearanceError::ThemeUnavailable {
                    name: "Removed".into(),
                    mode: ThemeMode::Light,
                },
                "appearance.theme.unavailable",
            ),
            (
                AppearanceError::ThemeModeMismatch {
                    name: "Ayu Dark".into(),
                    expected: ThemeMode::Light,
                    actual: ThemeMode::Dark,
                },
                "appearance.theme.mode_mismatch",
            ),
            (
                AppearanceError::AppearanceNotSaved { source: config_source() },
                "appearance.mode.not_saved",
            ),
            (
                AppearanceError::ThemeNotSaved { source: config_source() },
                "appearance.theme.not_saved",
            ),
            (
                AppearanceError::ReduceMotionNotSaved { source: config_source() },
                "appearance.reduce_motion.not_saved",
            ),
            (
                AppearanceError::InlinePayloadNotSaved { source: config_source() },
                "appearance.inline_payload.not_saved",
            ),
        ] {
            assert_eq!(error.code(), code);
            assert_eq!(error.to_string(), code);
            assert_eq!(format!("{error:#}"), code);
        }
    }

    #[test]
    fn io_sources_preserve_os_error_codes() {
        let error = AppearanceError::ReadThemeEntry {
            path: "themes".into(),
            source: std::io::Error::from_raw_os_error(13),
        };
        assert_eq!(
            std::error::Error::source(&error)
                .unwrap()
                .downcast_ref::<std::io::Error>()
                .unwrap()
                .raw_os_error(),
            Some(13)
        );
    }

    #[test]
    fn unsaved_changes_preserve_typed_configuration_and_io_sources() {
        use std::error::Error;

        let error = AppearanceError::ThemeNotSaved {
            source: Box::new(AppearanceError::from(config::ConfigError::ReadConfig {
                path: "settings.json".into(),
                source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            })),
        };
        let appearance = error.source().unwrap().downcast_ref::<AppearanceError>().unwrap();
        let config = appearance.source().unwrap().downcast_ref::<config::ConfigError>().unwrap();
        assert!(matches!(config, config::ConfigError::ReadConfig { path, .. } if path == Path::new("settings.json")));
        assert_eq!(
            config.source().unwrap().downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn theme_loader_sources_are_safe_diagnostics_or_absent() {
        use std::error::Error;

        let error = AppearanceError::LoadBundledTheme {
            bundle: "ayu.json",
            source: Some(config::ConfigDiagnostic::Json {
                category: serde_json::error::Category::Syntax,
                line: 2,
                column: 3,
            }),
        };
        assert!(matches!(error, AppearanceError::LoadBundledTheme { bundle: "ayu.json", .. }));
        assert!(matches!(
            error.source().unwrap().downcast_ref::<config::ConfigDiagnostic>(),
            Some(config::ConfigDiagnostic::Json {
                category: serde_json::error::Category::Syntax,
                line: 2,
                column: 3
            })
        ));
        assert!(
            AppearanceError::LoadCustomTheme {
                path: "custom.json".into(),
                source: None
            }
            .source()
            .is_none()
        );
    }
}
