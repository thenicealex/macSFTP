use macsftp_core::LocalPath;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppearancePreference {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub version: u32,
    pub appearance: AppearancePreference,
    /// When true, delete actions open a confirmation modal first.
    pub confirm_delete: bool,
}

impl AppConfig {
    pub const CURRENT_VERSION: u32 = 1;
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: Self::CURRENT_VERSION,
            appearance: AppearancePreference::System,
            confirm_delete: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    Io { path: String, message: String },
    Parse { message: String },
    UnsupportedVersion { found: u32, supported: u32 },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, message } => write!(formatter, "could not access {path}: {message}"),
            Self::Parse { message } => write!(formatter, "could not parse config: {message}"),
            Self::UnsupportedVersion { found, supported } => write!(
                formatter,
                "config version {found} is newer than supported version {supported}"
            ),
        }
    }
}

/// Owns the config path and its current in-memory snapshot.
#[derive(Clone)]
pub struct ConfigStore {
    path: LocalPath,
    config: AppConfig,
    initial_error: Option<ConfigError>,
}

impl ConfigStore {
    pub fn open(path: LocalPath) -> Result<Self, ConfigError> {
        let config = match std::fs::read_to_string(path.as_str()) {
            Ok(contents) => {
                serde_json::from_str(&contents).map_err(|error| ConfigError::Parse {
                    message: error.to_string(),
                })?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => AppConfig::default(),
            Err(error) => {
                return Err(ConfigError::Io {
                    path: path.as_str().to_string(),
                    message: error.to_string(),
                });
            }
        };

        if config.version > AppConfig::CURRENT_VERSION {
            return Err(ConfigError::UnsupportedVersion {
                found: config.version,
                supported: AppConfig::CURRENT_VERSION,
            });
        }

        Ok(Self {
            path,
            config,
            initial_error: None,
        })
    }

    /// Fallback used after a read error. It keeps the original path but does
    /// not touch the unreadable file until the user explicitly changes a setting.
    pub fn with_defaults(path: LocalPath) -> Self {
        Self {
            path,
            config: AppConfig::default(),
            initial_error: None,
        }
    }

    pub fn with_defaults_after_error(path: LocalPath, error: ConfigError) -> Self {
        Self {
            path,
            config: AppConfig::default(),
            initial_error: Some(error),
        }
    }

    pub fn config(&self) -> &AppConfig {
        &self.config
    }

    pub fn initial_error(&self) -> Option<&ConfigError> {
        self.initial_error.as_ref()
    }

    pub fn set_appearance(&mut self, appearance: AppearancePreference) -> Result<(), ConfigError> {
        self.config.appearance = appearance;
        let result = self.save();
        if result.is_ok() {
            self.initial_error = None;
        }
        result
    }

    pub fn set_confirm_delete(&mut self, confirm_delete: bool) -> Result<(), ConfigError> {
        self.config.confirm_delete = confirm_delete;
        let result = self.save();
        if result.is_ok() {
            self.initial_error = None;
        }
        result
    }

    fn save(&self) -> Result<(), ConfigError> {
        let json =
            serde_json::to_string_pretty(&self.config).map_err(|error| ConfigError::Parse {
                message: error.to_string(),
            })?;
        let path = self.path.as_str();
        let temp_path = format!("{path}.tmp");
        std::fs::write(&temp_path, json).map_err(|error| ConfigError::Io {
            path: temp_path.clone(),
            message: error.to_string(),
        })?;
        std::fs::rename(&temp_path, path).map_err(|error| ConfigError::Io {
            path: path.to_string(),
            message: error.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use macsftp_core::LocalPath;

    use super::{AppConfig, AppearancePreference, ConfigError, ConfigStore};

    static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn temp_config_path(label: &str) -> LocalPath {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::SeqCst);
        LocalPath::new(format!(
            "{}/macsftp-config-{label}-{}-{sequence}.json",
            std::env::temp_dir().display(),
            std::process::id()
        ))
    }

    fn cleanup(path: &LocalPath) {
        let _result = std::fs::remove_file(path.as_str());
        let _result = std::fs::remove_file(format!("{}.tmp", path.as_str()));
    }

    #[test]
    fn missing_config_uses_defaults_without_creating_a_file() {
        let path = temp_config_path("missing");
        cleanup(&path);

        let store = ConfigStore::open(path.clone()).expect("missing config should use defaults");

        assert_eq!(store.config(), &AppConfig::default());
        assert!(!std::path::Path::new(path.as_str()).exists());
    }

    #[test]
    fn appearance_round_trips_and_unknown_fields_are_ignored() {
        let path = temp_config_path("round-trip");
        std::fs::write(
            path.as_str(),
            r#"{"version":1,"appearance":"light","future_setting":true}"#,
        )
        .expect("fixture config should be writable");

        let mut store = ConfigStore::open(path.clone()).expect("config should parse");
        assert_eq!(store.config().appearance, AppearancePreference::Light);
        store
            .set_appearance(AppearancePreference::Dark)
            .expect("config should save atomically");

        let restored = ConfigStore::open(path.clone()).expect("saved config should reload");
        assert_eq!(restored.config().appearance, AppearancePreference::Dark);
        cleanup(&path);
    }

    #[test]
    fn confirm_delete_defaults_true_and_round_trips() {
        let path = temp_config_path("confirm-delete");
        cleanup(&path);

        let store = ConfigStore::open(path.clone()).expect("defaults");
        assert!(store.config().confirm_delete);

        // Missing field on disk still defaults to true.
        std::fs::write(path.as_str(), r#"{"version":1,"appearance":"system"}"#)
            .expect("write partial config");
        let mut store = ConfigStore::open(path.clone()).expect("parse partial");
        assert!(store.config().confirm_delete);
        store
            .set_confirm_delete(false)
            .expect("persist confirm_delete");

        let restored = ConfigStore::open(path.clone()).expect("reload");
        assert!(!restored.config().confirm_delete);
        cleanup(&path);
    }

    #[test]
    fn malformed_and_future_configs_are_not_silently_accepted() {
        let malformed_path = temp_config_path("malformed");
        std::fs::write(malformed_path.as_str(), "{").expect("malformed fixture should be writable");
        assert!(matches!(
            ConfigStore::open(malformed_path.clone()),
            Err(ConfigError::Parse { .. })
        ));

        let future_path = temp_config_path("future");
        std::fs::write(
            future_path.as_str(),
            r#"{"version":2,"appearance":"system"}"#,
        )
        .expect("future-version fixture should be writable");
        assert_eq!(
            ConfigStore::open(future_path.clone()).err(),
            Some(ConfigError::UnsupportedVersion {
                found: 2,
                supported: 1,
            })
        );

        cleanup(&malformed_path);
        cleanup(&future_path);
    }
}
