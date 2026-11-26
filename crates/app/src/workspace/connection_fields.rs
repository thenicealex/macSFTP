use gpui_kit::SharedString;
use macsftp_core::{
    AuthCredential, AuthMethod, AuthMethodKind, ConnectionProfile, ConnectionSettings, LocalPath,
    is_bidi_or_control,
};
use macsftp_storage::{PrivateKeyPassphraseUpdate, ProfileAuthUpdate};
use macsftp_ui::{InputState, SecretInputState};

use crate::workspace::helpers::expand_home;
use crate::workspace::profiles::PassphrasePolicy;

/// Shared SSH endpoint and credential field kinds common to `ConnectForm` and
/// `ProfileEditorState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ConnectionFieldKind {
    Host,
    Port,
    Username,
    Password,
    KeyPath,
    Passphrase,
    AgentSocket,
}

impl ConnectionFieldKind {
    pub(crate) const fn placeholder(self) -> &'static str {
        match self {
            Self::Host => "example.com",
            Self::Port => "22",
            Self::Username => "user",
            Self::Password | Self::Passphrase => "",
            Self::KeyPath => "~/.ssh/id_ed25519",
            Self::AgentSocket => "SSH_AUTH_SOCK (optional)",
        }
    }
}

/// Validated non-secret SSH endpoint fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ValidatedEndpoint {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) username: String,
}

/// Shared SSH endpoint and authentication draft state composed by both the
/// quick-connect modal (`ConnectForm`) and Settings → Saved Connections
/// (`ProfileEditorState`).
pub(crate) struct ConnectionFields {
    pub(crate) host: InputState,
    pub(crate) port: InputState,
    pub(crate) username: InputState,
    pub(crate) auth_method: AuthMethodKind,
    pub(crate) password: SecretInputState,
    pub(crate) key_path: InputState,
    pub(crate) passphrase: SecretInputState,
    pub(crate) agent_socket: InputState,
}

impl ConnectionFields {
    pub(crate) fn empty() -> Self {
        Self {
            host: InputState::new(),
            port: InputState::with_value("22"),
            username: InputState::new(),
            auth_method: AuthMethodKind::Password,
            password: SecretInputState::new(),
            key_path: InputState::new(),
            passphrase: SecretInputState::new(),
            agent_socket: InputState::new(),
        }
    }

    pub(crate) fn field_state_mut(&mut self, field: ConnectionFieldKind) -> &mut InputState {
        match field {
            ConnectionFieldKind::Host => &mut self.host,
            ConnectionFieldKind::Port => &mut self.port,
            ConnectionFieldKind::Username => &mut self.username,
            ConnectionFieldKind::Password => self.password.as_input_state_mut(),
            ConnectionFieldKind::KeyPath => &mut self.key_path,
            ConnectionFieldKind::Passphrase => self.passphrase.as_input_state_mut(),
            ConnectionFieldKind::AgentSocket => &mut self.agent_socket,
        }
    }

    fn prefill_endpoint(&mut self, host: &str, port: u16, username: &str) {
        self.host = InputState::with_value(host.to_string());
        self.port = InputState::with_value(port.to_string());
        self.username = InputState::with_value(username.to_string());
    }

    /// Prefill non-secret endpoint and auth fields from an active session's
    /// `ConnectionSettings`. Secrets are deliberately not prefilled.
    pub(crate) fn prefill_from_settings(&mut self, settings: &ConnectionSettings) {
        self.prefill_endpoint(&settings.host, settings.port, &settings.username);
        match &settings.auth {
            AuthCredential::PrivateKey { key_path, .. } => {
                self.auth_method = AuthMethodKind::PrivateKey;
                self.key_path = InputState::with_value(key_path.clone());
            }
            AuthCredential::KeyboardInteractive => {
                self.auth_method = AuthMethodKind::KeyboardInteractive;
            }
            AuthCredential::SshAgent { socket_path } => {
                self.auth_method = AuthMethodKind::SshAgent;
                if let Some(socket_path) = socket_path {
                    self.agent_socket = InputState::with_value(socket_path.clone());
                }
            }
            AuthCredential::Password { .. } => {
                self.auth_method = AuthMethodKind::Password;
            }
        }
    }

    /// Prefill non-secret endpoint and auth fields from a saved profile.
    pub(crate) fn prefill_from_profile(&mut self, profile: &ConnectionProfile) {
        self.prefill_endpoint(&profile.host, profile.port, &profile.username);
        match &profile.auth {
            AuthMethod::Password { .. } => {
                self.auth_method = AuthMethodKind::Password;
            }
            AuthMethod::PrivateKey { key_path, .. } => {
                self.auth_method = AuthMethodKind::PrivateKey;
                self.key_path = InputState::with_value(key_path.as_str().to_string());
            }
            AuthMethod::KeyboardInteractive => {
                self.auth_method = AuthMethodKind::KeyboardInteractive;
            }
            AuthMethod::SshAgent { socket_path } => {
                self.auth_method = AuthMethodKind::SshAgent;
                if let Some(socket_path) = socket_path {
                    self.agent_socket = InputState::with_value(socket_path.as_str().to_string());
                }
            }
        }
    }

    pub(crate) fn clear_secrets(&mut self) {
        self.password.clear();
        self.passphrase.clear();
    }

    /// Clear secrets belonging to auth methods other than `active_method`.
    pub(crate) fn clear_inactive_secrets(&mut self, active_method: AuthMethodKind) {
        match active_method {
            AuthMethodKind::Password => self.passphrase.clear(),
            AuthMethodKind::PrivateKey => self.password.clear(),
            AuthMethodKind::KeyboardInteractive | AuthMethodKind::SshAgent => {
                self.clear_secrets();
            }
        }
    }

    pub(crate) fn validate_endpoint(&self) -> Result<ValidatedEndpoint, SharedString> {
        let host = self.host.value().trim().to_string();
        if host.is_empty() {
            return Err("Enter a server address.".into());
        }
        if host
            .chars()
            .any(|c| c.is_whitespace() || is_bidi_or_control(c))
        {
            return Err("Server address contains invalid characters.".into());
        }
        let port_text = self.port.value().trim();
        let port: u16 = if port_text.is_empty() {
            22
        } else {
            match port_text.parse() {
                Ok(port) if port > 0 => port,
                _ => return Err("Enter a whole number from 1 to 65535.".into()),
            }
        };
        let username = self.username.value().trim().to_string();
        if username.is_empty() {
            return Err("Enter a username.".into());
        }
        if username
            .chars()
            .any(|c| c.is_whitespace() || is_bidi_or_control(c))
        {
            return Err("Username contains invalid characters.".into());
        }
        Ok(ValidatedEndpoint {
            host,
            port,
            username,
        })
    }

    pub(crate) fn validate_key_path(&self) -> Result<String, SharedString> {
        let key_path = expand_home(self.key_path.value().trim());
        if key_path.is_empty() {
            return Err("Enter the full path to your key file.".into());
        }
        if key_path.chars().any(is_bidi_or_control) {
            return Err("Key path contains invalid characters.".into());
        }
        Ok(key_path)
    }

    pub(crate) fn validate_agent_socket(&self) -> Result<Option<String>, SharedString> {
        let path = expand_home(self.agent_socket.value().trim());
        if path.chars().any(is_bidi_or_control) {
            return Err("Agent socket path contains invalid characters.".into());
        }
        Ok((!path.is_empty()).then_some(path))
    }

    pub(crate) fn build_auth_credential(&self) -> Result<AuthCredential, SharedString> {
        match self.auth_method {
            AuthMethodKind::Password => Ok(AuthCredential::Password {
                password: self.password.value().to_string(),
            }),
            AuthMethodKind::PrivateKey => {
                let key_path = self.validate_key_path()?;
                let passphrase = self.passphrase.value();
                Ok(AuthCredential::PrivateKey {
                    key_path,
                    passphrase: (!passphrase.is_empty()).then(|| passphrase.to_string()),
                })
            }
            AuthMethodKind::KeyboardInteractive => Ok(AuthCredential::KeyboardInteractive),
            AuthMethodKind::SshAgent => Ok(AuthCredential::SshAgent {
                socket_path: self.validate_agent_socket()?,
            }),
        }
    }

    pub(crate) fn build_profile_auth(
        &self,
        passphrase_policy: PassphrasePolicy,
    ) -> Result<ProfileAuthUpdate, SharedString> {
        match self.auth_method {
            AuthMethodKind::Password => {
                let password = self.password.value().to_string();
                Ok(ProfileAuthUpdate::Password {
                    password: (!password.is_empty()).then_some(password),
                })
            }
            AuthMethodKind::PrivateKey => {
                let key_path = self.validate_key_path()?;
                let passphrase = self.passphrase.value().to_string();
                Ok(ProfileAuthUpdate::PrivateKey {
                    key_path: LocalPath::new(key_path),
                    passphrase: match passphrase_policy {
                        // The policy wins over typed text: choosing "no
                        // passphrase" clears whatever is saved.
                        PassphrasePolicy::NoPassphrase => PrivateKeyPassphraseUpdate::NoPassphrase,
                        PassphrasePolicy::AskEveryTime => {
                            if passphrase.is_empty() {
                                // Dropping a remembered secret without
                                // retyping it still leaves the has-passphrase
                                // flag set; storage discards this value.
                                PrivateKeyPassphraseUpdate::NotRemembered(String::new())
                            } else {
                                PrivateKeyPassphraseUpdate::NotRemembered(passphrase)
                            }
                        }
                        PassphrasePolicy::Remember => {
                            if passphrase.is_empty() {
                                PrivateKeyPassphraseUpdate::KeepExisting
                            } else {
                                PrivateKeyPassphraseUpdate::Remember(passphrase)
                            }
                        }
                    },
                })
            }
            AuthMethodKind::KeyboardInteractive => Ok(ProfileAuthUpdate::KeyboardInteractive),
            AuthMethodKind::SshAgent => Ok(ProfileAuthUpdate::SshAgent {
                socket_path: self.validate_agent_socket()?.map(LocalPath::new),
            }),
        }
    }
}

/// Step forward or backward through an ordered slice of focusable fields,
/// wrapping around at the ends.
pub(crate) fn step_field_focus<F: Copy + PartialEq>(order: &[F], current: F, backwards: bool) -> F {
    if order.is_empty() {
        return current;
    }
    let current_index = order
        .iter()
        .position(|field| *field == current)
        .unwrap_or(0);
    let field_count = order.len() as isize;
    let offset: isize = if backwards { -1 } else { 1 };
    let next_index = (current_index as isize + offset).rem_euclid(field_count) as usize;
    order.get(next_index).copied().unwrap_or(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_validation_rejects_empty_and_invalid_values() {
        let mut fields = ConnectionFields::empty();
        assert_eq!(
            fields
                .validate_endpoint()
                .expect_err("empty host must fail"),
            "Enter a server address."
        );

        fields.host.set_value("bad host.example");
        assert_eq!(
            fields
                .validate_endpoint()
                .expect_err("whitespace host must fail"),
            "Server address contains invalid characters."
        );

        fields.host.set_value("sftp.example.com");
        fields.port.set_value("0");
        assert_eq!(
            fields.validate_endpoint().expect_err("port 0 must fail"),
            "Enter a whole number from 1 to 65535."
        );

        fields.port.set_value("");
        assert_eq!(
            fields
                .validate_endpoint()
                .expect_err("empty username must fail"),
            "Enter a username."
        );

        fields.username.set_value("deploy");
        assert_eq!(
            fields.validate_endpoint().expect("valid endpoint"),
            ValidatedEndpoint {
                host: "sftp.example.com".into(),
                port: 22,
                username: "deploy".into(),
            }
        );
    }

    #[test]
    fn key_and_agent_validation_checks_bidi_and_builds_credentials() {
        let mut fields = ConnectionFields::empty();
        fields.auth_method = AuthMethodKind::PrivateKey;
        assert_eq!(
            fields
                .build_auth_credential()
                .err()
                .expect("empty key path must fail"),
            "Enter the full path to your key file."
        );

        fields.key_path.set_value("/keys/id\u{202e}ed25519");
        assert_eq!(
            fields
                .build_auth_credential()
                .err()
                .expect("bidi key path must fail"),
            "Key path contains invalid characters."
        );

        fields.key_path.set_value("/keys/id_ed25519");
        fields.passphrase.set_value("secret");
        assert!(matches!(
            fields
                .build_profile_auth(PassphrasePolicy::NoPassphrase)
                .expect("profile auth builds"),
            ProfileAuthUpdate::PrivateKey {
                passphrase: PrivateKeyPassphraseUpdate::NoPassphrase,
                ..
            }
        ));

        fields.auth_method = AuthMethodKind::SshAgent;
        fields.agent_socket.set_value("/tmp/agent\u{0000}.sock");
        assert_eq!(
            fields
                .build_profile_auth(PassphrasePolicy::Remember)
                .expect_err("control char socket must fail"),
            "Agent socket path contains invalid characters."
        );
    }

    #[test]
    fn clear_inactive_secrets_clears_only_other_methods() {
        let mut fields = ConnectionFields::empty();
        fields.password.set_value("pw");
        fields.passphrase.set_value("pp");
        fields.clear_inactive_secrets(AuthMethodKind::Password);
        assert_eq!(fields.password.value(), "pw");
        assert!(fields.passphrase.value().is_empty());

        fields.passphrase.set_value("pp");
        fields.clear_inactive_secrets(AuthMethodKind::SshAgent);
        assert!(fields.password.value().is_empty());
        assert!(fields.passphrase.value().is_empty());
    }
}
