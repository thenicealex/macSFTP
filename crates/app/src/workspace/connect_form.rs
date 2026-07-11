#![allow(unused_imports)]
use gpui::{Context, FocusHandle, KeyDownEvent, SharedString, Window};
use macsftp_core::{
    AuthCredential, AuthMethod, AuthMethodKind, ConnectionProfile, ConnectionSettings, ProfileId,
    SecretRef,
};
use macsftp_storage::KeychainError;
use macsftp_ui::{InputKeyResult, InputState};
use tracing::warn;

use crate::workspace::helpers::{
    expand_home, secret_refs_for_profile, secret_refs_for_settings,
};

/// One field of the connect form, in Tab-cycle order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConnectField {
    Host,
    Port,
    Username,
    Password,
    KeyPath,
    Passphrase,
    ProfileName,
}

/// Connect form state. Secrets live in the input fields only while the
/// form is open; submitting moves them into a zeroized
/// `ConnectionSettings` and drops the form.
pub(crate) struct ConnectForm {
    pub(crate) host: InputState,
    pub(crate) port: InputState,
    pub(crate) username: InputState,
    pub(crate) auth_method: AuthMethodKind,
    pub(crate) password: InputState,
    pub(crate) key_path: InputState,
    pub(crate) passphrase: InputState,
    pub(crate) focused_field: ConnectField,
    pub(crate) error: Option<SharedString>,
    /// Profile this form was prefilled from, if any. Carried into the
    /// connect command so the tab and the saved profile stay linked, and
    /// so "Save profile" updates the same entry instead of duplicating.
    pub(crate) source_profile_id: Option<ProfileId>,
    /// Name entered for saving the current connection as a profile.
    pub(crate) profile_name: InputState,
}

impl ConnectForm {
    pub(crate) fn empty() -> Self {
        Self {
            host: InputState::new(),
            port: InputState::with_value("22"),
            username: InputState::new(),
            auth_method: AuthMethodKind::Password,
            password: InputState::new(),
            key_path: InputState::new(),
            passphrase: InputState::new(),
            focused_field: ConnectField::Host,
            error: None,
            source_profile_id: None,
            profile_name: InputState::new(),
        }
    }

    pub(crate) fn prefilled(settings: &ConnectionSettings) -> Self {
        let mut form = Self::empty();
        form.host = InputState::with_value(settings.host.clone());
        form.port = InputState::with_value(settings.port.to_string());
        form.username = InputState::with_value(settings.username.clone());
        if let AuthCredential::PrivateKey { key_path, .. } = &settings.auth {
            form.auth_method = AuthMethodKind::PrivateKey;
            form.key_path = InputState::with_value(key_path.clone());
        }
        // Secrets are deliberately not prefilled.
        form
    }

    /// Prefill the non-secret fields from a saved profile. The secret
    /// itself is restored separately by `use_profile`, which reads it
    /// back from the Keychain (plan §11).
    pub(crate) fn from_profile(profile: &ConnectionProfile) -> Self {
        let mut form = Self::empty();
        form.host = InputState::with_value(profile.host.clone());
        form.port = InputState::with_value(profile.port.to_string());
        form.username = InputState::with_value(profile.username.clone());
        form.source_profile_id = Some(profile.id);
        match &profile.auth {
            AuthMethod::Password { .. } => {
                form.auth_method = AuthMethodKind::Password;
            }
            AuthMethod::PrivateKey { key_path, .. } => {
                form.auth_method = AuthMethodKind::PrivateKey;
                form.key_path = InputState::with_value(key_path.as_str().to_string());
            }
        }
        form
    }

    fn field_order(&self) -> &'static [ConnectField] {
        match self.auth_method {
            AuthMethodKind::Password => &[
                ConnectField::Host,
                ConnectField::Port,
                ConnectField::Username,
                ConnectField::Password,
                ConnectField::ProfileName,
            ],
            AuthMethodKind::PrivateKey => &[
                ConnectField::Host,
                ConnectField::Port,
                ConnectField::Username,
                ConnectField::KeyPath,
                ConnectField::Passphrase,
                ConnectField::ProfileName,
            ],
        }
    }

    pub(crate) fn field_state_mut(&mut self, field: ConnectField) -> &mut InputState {
        match field {
            ConnectField::Host => &mut self.host,
            ConnectField::Port => &mut self.port,
            ConnectField::Username => &mut self.username,
            ConnectField::Password => &mut self.password,
            ConnectField::KeyPath => &mut self.key_path,
            ConnectField::Passphrase => &mut self.passphrase,
            ConnectField::ProfileName => &mut self.profile_name,
        }
    }

    pub(crate) fn cycle_focus(&mut self, backwards: bool) {
        let order = self.field_order();
        let current_index = order
            .iter()
            .position(|field| *field == self.focused_field)
            .unwrap_or(0);
        let field_count = order.len() as isize;
        let offset: isize = if backwards { -1 } else { 1 };
        let next_index = (current_index as isize + offset).rem_euclid(field_count) as usize;
        self.focused_field = order[next_index];
    }

    pub(crate) fn set_auth_method(&mut self, method: AuthMethodKind) {
        self.auth_method = method;
        if !self.field_order().contains(&self.focused_field) {
            self.focused_field = match method {
                AuthMethodKind::Password => ConnectField::Password,
                AuthMethodKind::PrivateKey => ConnectField::KeyPath,
            };
        }
    }

    /// Validate and build the settings. Returns `Err` with a short,
    /// user-facing message.
    pub(crate) fn build_settings(&self) -> Result<ConnectionSettings, SharedString> {
        let host = self.host.value().trim().to_string();
        if host.is_empty() {
            return Err("Host is required.".into());
        }
        let port_text = self.port.value().trim();
        let port: u16 = if port_text.is_empty() {
            22
        } else {
            match port_text.parse() {
                Ok(port) if port > 0 => port,
                _ => return Err("Port must be a number between 1 and 65535.".into()),
            }
        };
        let username = self.username.value().trim().to_string();
        if username.is_empty() {
            return Err("Username is required.".into());
        }

        let auth = match self.auth_method {
            AuthMethodKind::Password => AuthCredential::Password {
                password: self.password.value().to_string(),
            },
            AuthMethodKind::PrivateKey => {
                let key_path = expand_home(self.key_path.value().trim());
                if key_path.is_empty() {
                    return Err("Private key path is required.".into());
                }
                let passphrase = self.passphrase.value();
                AuthCredential::PrivateKey {
                    key_path,
                    passphrase: if passphrase.is_empty() {
                        None
                    } else {
                        Some(passphrase.to_string())
                    },
                }
            }
        };

        Ok(ConnectionSettings {
            host,
            port,
            username,
            auth,
        })
    }
}

impl super::Workspace {
    /// Open the connect form for the active tab, prefilling any cached
    /// session credentials.
    pub(crate) fn open_connect_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let form = self
            .state
            .tabs
            .active_tab_id
            .and_then(|tab_id| self.tab_settings.get(&tab_id))
            .map(ConnectForm::prefilled)
            .unwrap_or_else(ConnectForm::empty);
        self.connect_form = Some(form);
        window.focus(&self.connect_form_focus);
        cx.notify();
    }

    pub(crate) fn close_connect_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.connect_form = None;
        self.focus_pane(self.focused_side, window, cx);
        cx.notify();
    }

    pub(crate) fn submit_connect_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = &mut self.connect_form else {
            return;
        };
        match form.build_settings() {
            Ok(settings) => {
                let profile_id = form.source_profile_id;
                self.connect_form = None;
                self.focus_pane(self.focused_side, window, cx);
                self.connect_with(settings, profile_id, cx);
            }
            Err(message) => {
                form.error = Some(message);
                cx.notify();
            }
        }
    }

    /// Allocate a fresh profile id: one past the current maximum so
    /// saved profiles never collide even after deletes.
    pub(crate) fn next_profile_id(&self) -> ProfileId {
        let max = self
            .profiles
            .profiles()
            .iter()
            .map(|profile| profile.id.0)
            .max()
            .unwrap_or(0);
        ProfileId(max + 1)
    }

    /// Prefill the form from a saved profile, including the secret read
    /// back from the Keychain (plan §11). If the secret is missing or the
    /// Keychain is unavailable, the form still opens with metadata filled
    /// and the user re-enters the secret — saving is never blocked.
    pub(crate) fn use_profile(&mut self, profile_id: ProfileId, cx: &mut Context<Self>) {
        let Some(profile) = self.profiles.find_profile(profile_id).cloned() else {
            return;
        };
        let mut form = ConnectForm::from_profile(&profile);
        match &profile.auth {
            AuthMethod::Password { secret_ref } => match self.keychain.load(secret_ref) {
                Ok(Some(password)) => form.password.set_value(password),
                Ok(None) => {
                    self.status_message = Some("Saved password not found — re-enter it.".into());
                }
                Err(error) => {
                    warn!("could not load password from Keychain for {profile_id:?}: {error}");
                    self.status_message =
                        Some("Could not read saved password — re-enter it.".into());
                }
            },
            AuthMethod::PrivateKey { passphrase_ref, .. } => {
                if let Some(secret_ref) = passphrase_ref {
                    match self.keychain.load(secret_ref) {
                        Ok(Some(passphrase)) => form.passphrase.set_value(passphrase),
                        Ok(None) => {
                            self.status_message =
                                Some("Saved passphrase not found — re-enter it.".into());
                        }
                        Err(error) => {
                            warn!(
                                "could not load passphrase from Keychain for {profile_id:?}: {error}"
                            );
                            self.status_message =
                                Some("Could not read saved passphrase — re-enter it.".into());
                        }
                    }
                }
            }
        }
        self.connect_form = Some(form);
        cx.notify();
    }

    /// Persist the current form as a profile. If the form came from an
    /// existing profile, the same id is reused (update); otherwise a new
    /// id is allocated. The secret is mapped to a `SecretRef`, written to
    /// the macOS Keychain, and never written to disk (plan §5/§11). The
    /// profile is only flushed to `profiles.json` after the Keychain
    /// write succeeds, so the two never drift apart.
    pub(crate) fn save_current_profile(&mut self, cx: &mut Context<Self>) {
        let settings = match &self.connect_form {
            Some(form) => match form.build_settings() {
                Ok(settings) => settings,
                Err(message) => {
                    if let Some(form) = self.connect_form.as_mut() {
                        form.error = Some(message);
                    }
                    cx.notify();
                    return;
                }
            },
            None => return,
        };
        let name = self
            .connect_form
            .as_ref()
            .map(|form| form.profile_name.value().trim().to_string())
            .unwrap_or_default();
        let source_profile_id = self
            .connect_form
            .as_ref()
            .and_then(|form| form.source_profile_id);
        let name = if name.is_empty() {
            format!("{}@{}", settings.username, settings.host)
        } else {
            name
        };
        let profile_id = match source_profile_id {
            Some(id) if self.profiles.find_profile(id).is_some() => id,
            _ => self.next_profile_id(),
        };

        // When updating, the previous profile may have used a different
        // auth method; capture it so we can drop its now-orphaned
        // Keychain entries after storing the new ones.
        let previous = source_profile_id.and_then(|id| self.profiles.find_profile(id).cloned());

        // Write the secret to the Keychain first. If this fails we do not
        // persist the profile, so a disk entry never points at a missing
        // secret.
        if let Err(error) = self.store_profile_secrets(profile_id, &settings) {
            self.status_message =
                Some(format!("Could not save credentials to Keychain: {error}").into());
            cx.notify();
            return;
        }

        // Remove Keychain entries from the previous auth method that the
        // new profile no longer references.
        if let Some(previous) = &previous {
            for orphan in secret_refs_for_profile(&previous.auth) {
                let still_used = secret_refs_for_settings(profile_id, &settings.auth)
                    .iter()
                    .any(|current| current == &orphan);
                if !still_used {
                    let _ = self.keychain.delete(&orphan);
                }
            }
        }

        let profile =
            ConnectionProfile::from_connection_settings(profile_id, name.clone(), &settings);
        match self.profiles.save_profile(profile) {
            Ok(saved) => {
                if let Some(form) = self.connect_form.as_mut() {
                    form.source_profile_id = Some(saved.id);
                    form.profile_name = InputState::new();
                    form.error = None;
                }
                self.status_message = Some(format!("Saved profile '{}'.", name).into());
            }
            Err(error) => {
                // The Keychain now holds a secret with no disk profile.
                // Best-effort cleanup so we don't leave an orphan
                // credential behind.
                for secret_ref in secret_refs_for_settings(profile_id, &settings.auth) {
                    let _ = self.keychain.delete(&secret_ref);
                }
                self.status_message = Some(format!("Could not save profile: {error}").into());
            }
        }
        cx.notify();
    }

    /// Write the connection secret(s) for `profile_id` to the Keychain,
    /// mirroring `ConnectionProfile::from_connection_settings` (plan §11).
    pub(crate) fn store_profile_secrets(
        &self,
        profile_id: ProfileId,
        settings: &ConnectionSettings,
    ) -> Result<(), KeychainError> {
        match &settings.auth {
            AuthCredential::Password { password } => {
                let secret_ref = SecretRef::keychain_ref(profile_id, "password");
                self.keychain.store(&secret_ref, password)
            }
            AuthCredential::PrivateKey { passphrase, .. } => match passphrase {
                Some(passphrase) => {
                    let secret_ref = SecretRef::keychain_ref(profile_id, "passphrase");
                    self.keychain.store(&secret_ref, passphrase)
                }
                None => Ok(()),
            },
        }
    }

    /// Remove the Keychain entry (or entries) behind a saved profile.
    pub(crate) fn delete_profile_secrets(
        &self,
        profile: &ConnectionProfile,
    ) -> Result<(), KeychainError> {
        match &profile.auth {
            AuthMethod::Password { secret_ref } => self.keychain.delete(secret_ref),
            AuthMethod::PrivateKey { passphrase_ref, .. } => match passphrase_ref {
                Some(secret_ref) => self.keychain.delete(secret_ref),
                None => Ok(()),
            },
        }
    }

    /// Remove a saved profile from the store and disk, and best-effort
    /// remove its Keychain entries. Drops the link from any open form
    /// that pointed at it.
    pub(crate) fn delete_profile(&mut self, profile_id: ProfileId, cx: &mut Context<Self>) {
        let previous = self.profiles.find_profile(profile_id).cloned();
        match self.profiles.delete_profile(profile_id) {
            Ok(()) => {
                if let Some(form) = self.connect_form.as_mut()
                    && form.source_profile_id == Some(profile_id)
                {
                    form.source_profile_id = None;
                }
                if let Some(profile) = previous
                    && let Err(error) = self.delete_profile_secrets(&profile)
                {
                    warn!(
                        "could not remove Keychain secret for deleted profile {profile_id:?}: {error}"
                    );
                }
                self.status_message = Some("Deleted profile.".into());
            }
            Err(error) => {
                self.status_message = Some(format!("Could not delete profile: {error}").into());
            }
        }
        cx.notify();
    }

    /// Route keys typed while the connect form has focus to the
    /// focused field. Escape is left to the `CancelActiveModal`
    /// binding; unhandled keys propagate to global bindings.
    pub(crate) fn handle_connect_form_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(form) = &mut self.connect_form else {
            return;
        };
        let keystroke = &event.keystroke;

        if keystroke.key == "enter" && !keystroke.modifiers.modified() {
            cx.stop_propagation();
            self.submit_connect_form(window, cx);
            return;
        }
        if keystroke.key == "tab" {
            form.cycle_focus(keystroke.modifiers.shift);
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if keystroke.modifiers.platform && keystroke.key == "v" {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                let focused_field = form.focused_field;
                form.field_state_mut(focused_field).insert(&text);
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }

        let focused_field = form.focused_field;
        if form
            .field_state_mut(focused_field)
            .handle_keystroke(keystroke)
            == InputKeyResult::Handled
        {
            form.error = None;
            cx.stop_propagation();
            cx.notify();
        }
    }
}

impl Default for ConnectForm {
    fn default() -> Self {
        Self::empty()
    }
}
