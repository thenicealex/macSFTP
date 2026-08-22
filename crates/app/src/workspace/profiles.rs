use gpui::{App, Context, KeyDownEvent, SharedString, Window};
use macsftp_core::{
    AuthMethod, AuthMethodKind, ConnectionProfile, LocalPath, ProfileId, RemotePath,
};
use macsftp_storage::{ProfileAuthUpdate, ProfileMutationError, ProfileSaveRequest};
use macsftp_ui::{InputKeyResult, InputState, SecretInputState};
use tracing::warn;

use crate::resources::ActiveResources;
use crate::workspace::WorkspaceSurface;
use crate::workspace::helpers::expand_home;

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
    pub password: SecretInputState,
    pub key_path: InputState,
    pub passphrase: SecretInputState,
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
            password: SecretInputState::new(),
            key_path: InputState::new(),
            passphrase: SecretInputState::new(),
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
            ProfileEditorField::Password => self.password.as_input_state_mut(),
            ProfileEditorField::KeyPath => &mut self.key_path,
            ProfileEditorField::Passphrase => self.passphrase.as_input_state_mut(),
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

fn profile_secret_present(profile_id: ProfileId, cx: &App) -> bool {
    match cx.resources().profiles.has_saved_secret(profile_id) {
        Ok(present) => present,
        Err(error) => {
            warn!(?profile_id, %error, "could not inspect saved profile credential");
            false
        }
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
                    .map(|profile| profile_secret_present(profile.id, cx))
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

    pub(crate) fn select_profile_in_settings(&mut self, id: ProfileId, cx: &mut Context<Self>) {
        self.profile_filter_focused = false;
        self.load_profile_editor(id, cx);
    }

    pub(crate) fn start_new_profile(&mut self, cx: &mut Context<Self>) {
        self.profile_filter_focused = false;
        self.selected_profile_id = None;
        self.profile_editor = Some(ProfileEditorState::blank());
        cx.notify();
    }

    pub(crate) fn load_profile_editor(&mut self, id: ProfileId, cx: &mut Context<Self>) {
        let Some(profile) = cx.resources().profiles.find_profile(id).cloned() else {
            return;
        };
        let secret_present = profile_secret_present(profile.id, cx);
        self.profile_filter_focused = false;
        self.selected_profile_id = Some(id);
        self.profile_editor = Some(ProfileEditorState::from_profile(&profile, secret_present));
        cx.notify();
    }

    /// Validate the profile editor and submit one storage transaction. An
    /// empty password on edit asks storage to keep the existing credential.
    pub(crate) fn save_profile_editor(&mut self, cx: &mut Context<Self>) {
        let (
            host,
            port_text,
            username,
            is_new,
            editor_profile_id,
            entered_name,
            default_remote_path_text,
            auth_method,
            password,
            key_path_raw,
            passphrase,
        ) = {
            let Some(editor) = self.profile_editor.as_ref() else {
                return;
            };
            (
                editor.host.value().trim().to_string(),
                editor.port.value().trim().to_string(),
                editor.username.value().trim().to_string(),
                editor.is_new,
                editor.profile_id,
                editor.name.value().trim().to_string(),
                editor.default_remote_path.value().trim().to_string(),
                editor.auth_method,
                editor.password.value().to_string(),
                editor.key_path.value().trim().to_string(),
                editor.passphrase.value().to_string(),
            )
        };

        if host.is_empty() {
            if let Some(editor) = self.profile_editor.as_mut() {
                editor.error = Some("Host is required.".into());
            }
            cx.notify();
            return;
        }
        let port: u16 = if port_text.is_empty() {
            22
        } else {
            match port_text.parse() {
                Ok(port) if port > 0 => port,
                _ => {
                    if let Some(editor) = self.profile_editor.as_mut() {
                        editor.error = Some("Port must be a number between 1 and 65535.".into());
                    }
                    cx.notify();
                    return;
                }
            }
        };
        if username.is_empty() {
            if let Some(editor) = self.profile_editor.as_mut() {
                editor.error = Some("Username is required.".into());
            }
            cx.notify();
            return;
        }

        let profile_id = match (is_new, editor_profile_id) {
            (false, Some(id)) if cx.resources().profiles.find_profile(id).is_some() => id,
            _ => {
                let Some(profile_id) = self.next_profile_id(cx) else {
                    cx.notify();
                    return;
                };
                profile_id
            }
        };

        let name = {
            if entered_name.is_empty() {
                format!("{username}@{host}")
            } else {
                entered_name
            }
        };

        let default_remote_path = (!default_remote_path_text.is_empty())
            .then(|| RemotePath::new(default_remote_path_text));

        let auth = match auth_method {
            AuthMethodKind::Password => ProfileAuthUpdate::Password {
                password: (!password.is_empty()).then_some(password),
            },
            AuthMethodKind::PrivateKey => {
                let key_path = expand_home(&key_path_raw);
                if key_path.is_empty() {
                    if let Some(editor) = self.profile_editor.as_mut() {
                        editor.error = Some("Private key path is required.".into());
                    }
                    cx.notify();
                    return;
                }
                ProfileAuthUpdate::PrivateKey {
                    key_path: LocalPath::new(key_path),
                    passphrase: (!passphrase.is_empty()).then_some(passphrase),
                }
            }
        };

        let request = ProfileSaveRequest {
            profile_id,
            name: name.clone(),
            host,
            port,
            username,
            auth,
            default_remote_path,
        };
        match cx.resources_mut().profiles.save_request(request) {
            Ok(outcome) => {
                for error in outcome.cleanup_warnings {
                    warn!(
                        ?profile_id,
                        %error,
                        "could not remove obsolete Keychain secret after profile update"
                    );
                }
                let secret_present = profile_secret_present(outcome.profile.id, cx);
                self.selected_profile_id = Some(outcome.profile.id);
                self.profile_editor = Some(ProfileEditorState::from_profile(
                    &outcome.profile,
                    secret_present,
                ));
                self.status_message = Some(format!("Saved profile '{}'.", name).into());
            }
            Err(error) => {
                let message = match error {
                    ProfileMutationError::CredentialRequired(AuthMethodKind::Password) => {
                        "Password is required.".to_string()
                    }
                    other => format!("Could not save profile: {other}"),
                };
                if let Some(editor) = self.profile_editor.as_mut() {
                    editor.error = Some(message.clone().into());
                }
                self.status_message = Some(message.into());
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
            .filter(|profile| profile_matches_filter(profile, self.profile_filter.value()))
            .collect()
    }

    /// Focus the Profiles list filter field (click or explicit focus).
    pub(crate) fn focus_profile_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.profile_filter_focused = true;
        window.focus(&self.workspace_focus);
        cx.notify();
    }

    pub(crate) fn handle_profile_filter_key(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.profile_filter_focused {
            return;
        }
        let keystroke = &event.keystroke;
        if keystroke.modifiers.platform && keystroke.key == "v" {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                self.profile_filter.insert(&text);
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }
        if self.profile_filter.handle_keystroke(keystroke) == InputKeyResult::Handled {
            cx.stop_propagation();
            cx.notify();
        }
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
    pub(crate) fn confirm_delete_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        self.restore_focus_after_profile_delete(window, cx);
        cx.notify();
    }

    /// Dismiss the profile-delete confirmation without deleting.
    pub(crate) fn cancel_delete_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.profile_delete_confirm = None;
        self.restore_focus_after_profile_delete(window, cx);
        cx.notify();
    }

    /// After dismissing the delete confirm, return keyboard focus to Connect
    /// or Settings when those surfaces are still open (not always the file pane).
    fn restore_focus_after_profile_delete(&mut self, window: &mut Window, _cx: &mut Context<Self>) {
        if self.connect_form_ui.form.is_some() {
            window.focus(&self.connect_form_ui.focus);
        } else if self.surface == WorkspaceSurface::Settings {
            window.focus(&self.workspace_focus);
        } else {
            window.focus(self.pane_focus(self.focused_side));
        }
    }

    /// Leave Settings: discard unsaved editor drafts (including New Profile)
    /// and return to the Files surface.
    pub(crate) fn leave_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.profile_editor = None;
        self.selected_profile_id = None;
        self.profile_filter = InputState::new();
        self.profile_filter_focused = false;
        self.profile_delete_confirm = None;
        self.settings_section = SettingsSection::General;
        self.surface = WorkspaceSurface::Files;
        self.focus_pane(self.focused_side, window, cx);
        cx.notify();
    }

    pub(crate) fn handle_profile_editor_key(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.profile_filter_focused {
            return;
        }
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
