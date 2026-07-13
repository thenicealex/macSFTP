use gpui::{App, Context, KeyDownEvent, SharedString, Window};
use macsftp_core::{
    AuthMethod, AuthMethodKind, ConnectionProfile, LocalPath, ProfileId, RemotePath, SecretRef,
};
use macsftp_ui::{InputKeyResult, InputState};
use tracing::warn;

use crate::resources::ActiveResources;
use crate::workspace::helpers::{expand_home, secret_refs_for_profile};

/// Settings sidebar section (General appearance vs Profiles management).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SettingsSection {
    #[default]
    General,
    Profiles,
}

/// Focusable fields in the Settings → Profiles editor (Tab order).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProfileEditorField {
    Name,
    Host,
    Port,
    Username,
    Password,
    KeyPath,
    Passphrase,
    DefaultRemotePath,
}

/// Editable profile draft shown in Settings → Profiles (right pane).
/// Secrets are never prefilled from Keychain; leave password blank on edit to keep.
pub(crate) struct ProfileEditorState {
    pub is_new: bool,
    pub profile_id: Option<ProfileId>,
    pub name: InputState,
    pub host: InputState,
    pub port: InputState,
    pub username: InputState,
    pub auth_method: AuthMethodKind,
    pub password: InputState,
    pub key_path: InputState,
    pub passphrase: InputState,
    pub default_remote_path: InputState,
    pub error: Option<SharedString>,
    pub secret_present_hint: bool,
    pub focused_field: ProfileEditorField,
}

impl ProfileEditorState {
    pub fn blank() -> Self {
        Self {
            is_new: true,
            profile_id: None,
            name: InputState::new(),
            host: InputState::new(),
            port: InputState::with_value("22"),
            username: InputState::new(),
            auth_method: AuthMethodKind::Password,
            password: InputState::new(),
            key_path: InputState::new(),
            passphrase: InputState::new(),
            default_remote_path: InputState::new(),
            error: None,
            secret_present_hint: false,
            focused_field: ProfileEditorField::Host,
        }
    }

    /// Prefill non-secret fields. Password/passphrase stay empty (no Keychain prefill).
    pub fn from_profile(profile: &ConnectionProfile, secret_present: bool) -> Self {
        let mut editor = Self::blank();
        editor.is_new = false;
        editor.profile_id = Some(profile.id);
        editor.name = InputState::with_value(profile.name.clone());
        editor.host = InputState::with_value(profile.host.clone());
        editor.port = InputState::with_value(profile.port.to_string());
        editor.username = InputState::with_value(profile.username.clone());
        editor.secret_present_hint = secret_present;
        if let Some(path) = &profile.default_remote_path {
            editor.default_remote_path = InputState::with_value(path.as_str().to_string());
        }
        match &profile.auth {
            AuthMethod::Password { .. } => {
                editor.auth_method = AuthMethodKind::Password;
            }
            AuthMethod::PrivateKey { key_path, .. } => {
                editor.auth_method = AuthMethodKind::PrivateKey;
                editor.key_path = InputState::with_value(key_path.as_str().to_string());
            }
        }
        editor
    }

    fn field_order(&self) -> &'static [ProfileEditorField] {
        match self.auth_method {
            AuthMethodKind::Password => &[
                ProfileEditorField::Name,
                ProfileEditorField::Host,
                ProfileEditorField::Port,
                ProfileEditorField::Username,
                ProfileEditorField::Password,
                ProfileEditorField::DefaultRemotePath,
            ],
            AuthMethodKind::PrivateKey => &[
                ProfileEditorField::Name,
                ProfileEditorField::Host,
                ProfileEditorField::Port,
                ProfileEditorField::Username,
                ProfileEditorField::KeyPath,
                ProfileEditorField::Passphrase,
                ProfileEditorField::DefaultRemotePath,
            ],
        }
    }

    fn field_state_mut(&mut self, field: ProfileEditorField) -> &mut InputState {
        match field {
            ProfileEditorField::Name => &mut self.name,
            ProfileEditorField::Host => &mut self.host,
            ProfileEditorField::Port => &mut self.port,
            ProfileEditorField::Username => &mut self.username,
            ProfileEditorField::Password => &mut self.password,
            ProfileEditorField::KeyPath => &mut self.key_path,
            ProfileEditorField::Passphrase => &mut self.passphrase,
            ProfileEditorField::DefaultRemotePath => &mut self.default_remote_path,
        }
    }

    pub(crate) fn set_auth_method(&mut self, method: AuthMethodKind) {
        self.auth_method = method;
        if !self.field_order().contains(&self.focused_field) {
            self.focused_field = match method {
                AuthMethodKind::Password => ProfileEditorField::Password,
                AuthMethodKind::PrivateKey => ProfileEditorField::KeyPath,
            };
        }
    }

    fn cycle_focus(&mut self, backwards: bool) {
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
}

/// Case-insensitive match on profile name, host, or username.
/// Empty / whitespace-only query matches every profile.
pub(crate) fn profile_matches_filter(profile: &ConnectionProfile, query: &str) -> bool {
    if query.trim().is_empty() {
        return true;
    }
    let q = query.to_lowercase();
    profile.name.to_lowercase().contains(&q)
        || profile.host.to_lowercase().contains(&q)
        || profile.username.to_lowercase().contains(&q)
}

/// One-line label for the settings profile list.
pub(crate) fn profile_list_label(profile: &ConnectionProfile) -> String {
    format!(
        "{} · {}@{}:{}",
        profile.name, profile.username, profile.host, profile.port
    )
}

fn profile_secret_present(profile: &ConnectionProfile, cx: &App) -> bool {
    match &profile.auth {
        AuthMethod::Password { secret_ref } => {
            matches!(cx.resources().keychain.load(secret_ref), Ok(Some(_)))
        }
        AuthMethod::PrivateKey { passphrase_ref, .. } => match passphrase_ref {
            Some(secret_ref) => {
                matches!(cx.resources().keychain.load(secret_ref), Ok(Some(_)))
            }
            None => false,
        },
    }
}

impl crate::workspace::Workspace {
    pub(crate) fn set_settings_section(
        &mut self,
        section: SettingsSection,
        cx: &mut Context<Self>,
    ) {
        self.settings_section = section;
        if section == SettingsSection::Profiles {
            let profiles = cx.resources().profiles.profiles();
            if self
                .selected_profile_id
                .is_none_or(|id| profiles.iter().all(|p| p.id != id))
            {
                self.selected_profile_id = profiles.first().map(|p| p.id);
            }
            if let Some(id) = self.selected_profile_id {
                let secret_present = cx
                    .resources()
                    .profiles
                    .find_profile(id)
                    .map(|profile| profile_secret_present(profile, cx))
                    .unwrap_or(false);
                if let Some(profile) = cx.resources().profiles.find_profile(id).cloned() {
                    self.profile_editor =
                        Some(ProfileEditorState::from_profile(&profile, secret_present));
                }
            } else if self.profile_editor.as_ref().is_none_or(|e| !e.is_new) {
                self.profile_editor = None;
            }
        }
        cx.notify();
    }

    pub(crate) fn select_profile_in_settings(
        &mut self,
        id: ProfileId,
        cx: &mut Context<Self>,
    ) {
        self.load_profile_editor(id, cx);
    }

    pub(crate) fn start_new_profile(&mut self, cx: &mut Context<Self>) {
        self.selected_profile_id = None;
        self.profile_editor = Some(ProfileEditorState::blank());
        cx.notify();
    }

    pub(crate) fn load_profile_editor(&mut self, id: ProfileId, cx: &mut Context<Self>) {
        let Some(profile) = cx.resources().profiles.find_profile(id).cloned() else {
            return;
        };
        let secret_present = profile_secret_present(&profile, cx);
        self.selected_profile_id = Some(id);
        self.profile_editor = Some(ProfileEditorState::from_profile(&profile, secret_present));
        cx.notify();
    }

    /// Validate and persist the profile editor. Keychain is written before
    /// JSON; empty password on edit keeps the existing secret_ref.
    pub(crate) fn save_profile_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.profile_editor.as_ref() else {
            return;
        };

        let host = editor.host.value().trim().to_string();
        if host.is_empty() {
            if let Some(editor) = self.profile_editor.as_mut() {
                editor.error = Some("Host is required.".into());
            }
            cx.notify();
            return;
        }
        let port_text = editor.port.value().trim();
        let port: u16 = if port_text.is_empty() {
            22
        } else {
            match port_text.parse() {
                Ok(port) if port > 0 => port,
                _ => {
                    if let Some(editor) = self.profile_editor.as_mut() {
                        editor.error =
                            Some("Port must be a number between 1 and 65535.".into());
                    }
                    cx.notify();
                    return;
                }
            }
        };
        let username = editor.username.value().trim().to_string();
        if username.is_empty() {
            if let Some(editor) = self.profile_editor.as_mut() {
                editor.error = Some("Username is required.".into());
            }
            cx.notify();
            return;
        }

        let is_new = editor.is_new;
        let profile_id = match (is_new, editor.profile_id) {
            (false, Some(id)) if cx.resources().profiles.find_profile(id).is_some() => id,
            _ => self.next_profile_id(cx),
        };

        let previous = if is_new {
            None
        } else {
            cx.resources().profiles.find_profile(profile_id).cloned()
        };

        let name = {
            let entered = editor.name.value().trim().to_string();
            if entered.is_empty() {
                format!("{username}@{host}")
            } else {
                entered
            }
        };

        let default_remote_path = {
            let path = editor.default_remote_path.value().trim();
            if path.is_empty() {
                None
            } else {
                Some(RemotePath::new(path))
            }
        };

        let auth_method = editor.auth_method;
        let password = editor.password.value().to_string();
        let key_path_raw = editor.key_path.value().trim().to_string();
        let passphrase = editor.passphrase.value().to_string();

        let auth = match auth_method {
            AuthMethodKind::Password => {
                if password.is_empty() {
                    if is_new {
                        if let Some(editor) = self.profile_editor.as_mut() {
                            editor.error = Some("Password is required.".into());
                        }
                        cx.notify();
                        return;
                    }
                    match previous.as_ref().map(|p| &p.auth) {
                        Some(AuthMethod::Password { secret_ref }) => AuthMethod::Password {
                            secret_ref: secret_ref.clone(),
                        },
                        _ => {
                            if let Some(editor) = self.profile_editor.as_mut() {
                                editor.error = Some("Password is required.".into());
                            }
                            cx.notify();
                            return;
                        }
                    }
                } else {
                    let secret_ref = SecretRef::keychain_ref(profile_id, "password");
                    if let Err(error) = cx.resources().keychain.store(&secret_ref, &password) {
                        self.status_message = Some(
                            format!("Could not save credentials to Keychain: {error}").into(),
                        );
                        cx.notify();
                        return;
                    }
                    AuthMethod::Password { secret_ref }
                }
            }
            AuthMethodKind::PrivateKey => {
                let key_path = expand_home(&key_path_raw);
                if key_path.is_empty() {
                    if let Some(editor) = self.profile_editor.as_mut() {
                        editor.error = Some("Private key path is required.".into());
                    }
                    cx.notify();
                    return;
                }
                let passphrase_ref = if passphrase.is_empty() {
                    if is_new {
                        None
                    } else {
                        match previous.as_ref().map(|p| &p.auth) {
                            Some(AuthMethod::PrivateKey { passphrase_ref, .. }) => {
                                passphrase_ref.clone()
                            }
                            _ => None,
                        }
                    }
                } else {
                    let secret_ref = SecretRef::keychain_ref(profile_id, "passphrase");
                    if let Err(error) = cx.resources().keychain.store(&secret_ref, &passphrase) {
                        self.status_message = Some(
                            format!("Could not save credentials to Keychain: {error}").into(),
                        );
                        cx.notify();
                        return;
                    }
                    Some(secret_ref)
                };
                AuthMethod::PrivateKey {
                    key_path: LocalPath::new(key_path),
                    passphrase_ref: passphrase_ref.clone(),
                    remember_passphrase: passphrase_ref.is_some(),
                }
            }
        };

        let new_secret_refs = secret_refs_for_profile(&auth);

        // Drop Keychain entries from a previous auth method that the new
        // profile no longer references.
        if let Some(previous) = &previous {
            for orphan in secret_refs_for_profile(&previous.auth) {
                if !new_secret_refs.iter().any(|current| current == &orphan)
                    && let Err(error) = cx.resources().keychain.delete(&orphan)
                {
                    warn!(
                        "could not remove orphaned Keychain secret for profile {profile_id:?}: {error}"
                    );
                }
            }
        }

        let mut profile =
            ConnectionProfile::new(profile_id, name.clone(), host, username, auth);
        profile.port = port;
        profile.default_remote_path = default_remote_path;
        if let Some(previous) = &previous {
            profile.group_id = previous.group_id;
            profile.last_local_path = previous.last_local_path.clone();
        }

        match cx.resources_mut().profiles.save_profile(profile) {
            Ok(saved) => {
                self.selected_profile_id = Some(saved.id);
                self.profile_editor = Some(ProfileEditorState::from_profile(&saved, true));
                self.status_message = Some(format!("Saved profile '{}'.", name).into());
            }
            Err(error) => {
                // New profile never landed on disk — drop secrets we just wrote.
                if previous.is_none() {
                    for secret_ref in &new_secret_refs {
                        let _ = cx.resources().keychain.delete(secret_ref);
                    }
                }
                if let Some(editor) = self.profile_editor.as_mut() {
                    editor.error = Some(format!("Could not save profile: {error}").into());
                }
                self.status_message = Some(format!("Could not save profile: {error}").into());
            }
        }
        cx.notify();
    }

    pub(crate) fn filtered_profiles<'a>(
        &self,
        profiles: &'a [ConnectionProfile],
    ) -> Vec<&'a ConnectionProfile> {
        profiles
            .iter()
            .filter(|profile| profile_matches_filter(profile, &self.profile_filter))
            .collect()
    }

    /// Arm the profile-delete confirmation modal. Does not remove anything yet.
    pub(crate) fn request_delete_profile(
        &mut self,
        id: ProfileId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.profile_delete_confirm = Some(id);
        window.focus(&self.modal_focus);
        cx.notify();
    }

    /// User confirmed: delete the profile (store + Keychain) and refresh settings selection.
    pub(crate) fn confirm_delete_profile(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.profile_delete_confirm.take() else {
            return;
        };
        self.delete_profile(id, cx);
        if self.selected_profile_id == Some(id) {
            self.selected_profile_id = None;
            self.profile_editor = None;
            // Reselect the first remaining profile when Profiles is the active section.
            if self.settings_section == SettingsSection::Profiles {
                self.set_settings_section(SettingsSection::Profiles, cx);
            }
        }
        self.focus_pane(self.focused_side, window, cx);
        cx.notify();
    }

    /// Dismiss the profile-delete confirmation without deleting.
    pub(crate) fn cancel_delete_profile(&mut self, cx: &mut Context<Self>) {
        self.profile_delete_confirm = None;
        cx.notify();
    }

    pub(crate) fn handle_profile_editor_key(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.profile_editor.as_mut() else {
            return;
        };
        let keystroke = &event.keystroke;

        if keystroke.key == "enter" && !keystroke.modifiers.modified() {
            cx.stop_propagation();
            self.save_profile_editor(cx);
            return;
        }
        if keystroke.key == "tab" {
            editor.cycle_focus(keystroke.modifiers.shift);
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if keystroke.modifiers.platform && keystroke.key == "v" {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                let focused_field = editor.focused_field;
                editor.field_state_mut(focused_field).insert(&text);
                editor.error = None;
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }

        let focused_field = editor.focused_field;
        if editor
            .field_state_mut(focused_field)
            .handle_keystroke(keystroke)
            == InputKeyResult::Handled
        {
            editor.error = None;
            cx.stop_propagation();
            cx.notify();
        }
    }
}
