use macsftp_core::LocalPath;
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::{file_lock::FileLock, write_private_file_atomically};

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
    /// When true, file panes show dotfiles (`name` starts with `.`).
    pub show_hidden_files: bool,
    /// 远程编辑用的外部编辑器应用名。None = 系统默认关联应用。
    #[serde(default)]
    pub external_editor: Option<String>,
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
            show_hidden_files: false,
            external_editor: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    RecoveryRequired { path: String },
    Io { path: String, message: String },
    Parse { message: String },
    UnsupportedVersion { found: u32, supported: u32 },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RecoveryRequired { path } => write!(
                formatter,
                "refusing to overwrite unreadable application data at {path}"
            ),
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
        let config = Self::load_from_disk(&path)?;
        Ok(Self {
            path,
            config,
            initial_error: None,
        })
    }

    /// Read the config file from disk, treating a missing file as defaults.
    /// Shared by `open` and the locked write transaction so both enforce the
    /// same version guard.
    fn load_from_disk(path: &LocalPath) -> Result<AppConfig, ConfigError> {
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
        Ok(config)
    }

    /// Fallback used after a read error. It keeps the original path but does
    /// not touch the unreadable file until a recovery action is implemented.
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

    /// Apply one field change through a serialized read-modify-write cycle:
    /// hold the advisory lock, reload what other app instances wrote, apply,
    /// save. On any failure the in-memory snapshot is rolled back so it
    /// never claims a state that did not reach disk.
    fn modify(&mut self, apply: impl FnOnce(&mut AppConfig)) -> Result<(), ConfigError> {
        let previous = self.config.clone();
        let result = self.modify_under_lock(apply);
        match result {
            Ok(()) => {
                self.initial_error = None;
                Ok(())
            }
            Err(error) => {
                self.config = previous;
                Err(error)
            }
        }
    }

    fn modify_under_lock(&mut self, apply: impl FnOnce(&mut AppConfig)) -> Result<(), ConfigError> {
        if self.initial_error.is_some() {
            return Err(ConfigError::RecoveryRequired {
                path: self.path.as_str().to_string(),
            });
        }
        let _lock =
            FileLock::acquire(Path::new(self.path.as_str())).map_err(|error| ConfigError::Io {
                path: self.path.as_str().to_string(),
                message: error.to_string(),
            })?;
        self.config = Self::load_from_disk(&self.path)?;
        apply(&mut self.config);
        self.save()
    }

    pub fn set_appearance(&mut self, appearance: AppearancePreference) -> Result<(), ConfigError> {
        self.modify(|config| config.appearance = appearance)
    }

    pub fn set_confirm_delete(&mut self, confirm_delete: bool) -> Result<(), ConfigError> {
        self.modify(|config| config.confirm_delete = confirm_delete)
    }

    pub fn set_show_hidden_files(&mut self, show_hidden_files: bool) -> Result<(), ConfigError> {
        self.modify(|config| config.show_hidden_files = show_hidden_files)
    }

    pub fn set_external_editor(&mut self, editor: Option<String>) -> Result<(), ConfigError> {
        self.modify(|config| config.external_editor = editor)
    }

    fn save(&self) -> Result<(), ConfigError> {
        if self.initial_error.is_some() {
            return Err(ConfigError::RecoveryRequired {
                path: self.path.as_str().to_string(),
            });
        }
        let json =
            serde_json::to_string_pretty(&self.config).map_err(|error| ConfigError::Parse {
                message: error.to_string(),
            })?;
        write_private_file_atomically(std::path::Path::new(self.path.as_str()), json.as_bytes())
            .map_err(|error| ConfigError::Io {
                path: self.path.as_str().to_string(),
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
    fn concurrent_instances_do_not_silently_lose_config_toggles() {
        let path = temp_config_path("concurrent");
        let _ = std::fs::remove_file(format!("{}.lock", path.as_str()));
        cleanup(&path);
        std::fs::write(
            path.as_str(),
            r#"{"version":1,"appearance":"system","confirm_delete":true,"show_hidden_files":false}"#,
        )
        .expect("seed config");

        // Both instances open before either writes: their snapshots predate
        // each other's change.
        let mut first = ConfigStore::open(path.clone()).expect("first instance");
        let mut second = ConfigStore::open(path.clone()).expect("second instance");

        first
            .set_appearance(AppearancePreference::Dark)
            .expect("first instance saves appearance");
        second
            .set_show_hidden_files(true)
            .expect("second instance saves hidden files");

        let final_config = ConfigStore::open(path.clone())
            .expect("reload after both writes")
            .config()
            .clone();
        assert_eq!(
            final_config.appearance,
            AppearancePreference::Dark,
            "second writer must not erase the first writer's change"
        );
        assert!(final_config.show_hidden_files);
        cleanup(&path);
        let _ = std::fs::remove_file(format!("{}.lock", path.as_str()));
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
    fn show_hidden_files_defaults_false_and_round_trips() {
        let path = temp_config_path("hidden");
        cleanup(&path);

        let store = ConfigStore::open(path.clone()).expect("defaults");
        assert!(!store.config().show_hidden_files);

        // Missing field on disk still defaults to false.
        std::fs::write(path.as_str(), r#"{"version":1,"appearance":"system"}"#)
            .expect("write partial config");
        let mut store = ConfigStore::open(path.clone()).expect("parse partial");
        assert!(!store.config().show_hidden_files);
        store
            .set_show_hidden_files(true)
            .expect("persist show_hidden_files");

        let restored = ConfigStore::open(path.clone()).expect("reload");
        assert!(restored.config().show_hidden_files);
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

    #[test]
    fn fallback_after_parse_error_blocks_writes_and_preserves_config() {
        let path = temp_config_path("corrupt-write-block");
        cleanup(&path);
        std::fs::write(path.as_str(), "{not json").expect("write corrupt config fixture");
        let error = ConfigStore::open(path.clone())
            .err()
            .expect("corrupt config must fail");
        let mut store = ConfigStore::with_defaults_after_error(path.clone(), error);

        assert!(matches!(
            store.set_show_hidden_files(true),
            Err(ConfigError::RecoveryRequired { .. })
        ));
        assert!(!store.config().show_hidden_files);
        assert_eq!(
            std::fs::read_to_string(path.as_str()).expect("read preserved corrupt config"),
            "{not json"
        );
        cleanup(&path);
    }

    #[test]
    fn external_editor_defaults_none_and_round_trips() {
        let dir = std::env::temp_dir().join(format!("macsftp-editor-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create editor config dir");
        let path = LocalPath::new(dir.join("config.json").to_string_lossy().to_string());
        let mut store = ConfigStore::with_defaults(path.clone());
        assert_eq!(store.config().external_editor, None);
        store
            .set_external_editor(Some("Visual Studio Code".to_string()))
            .expect("persist editor");
        let restored = ConfigStore::open(path).expect("reopen");
        assert_eq!(
            restored.config().external_editor.as_deref(),
            Some("Visual Studio Code")
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
