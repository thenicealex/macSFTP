use gpui::{App, Context, KeyDownEvent, SharedString, Window};
use macsftp_core::{
    AuthCredential, AuthMethod, AuthMethodKind, ConnectionProfile, ConnectionSettings, ProfileId,
};
use macsftp_storage::ProfileMutationError;
use macsftp_ui::{InputKeyResult, InputState, SecretInputState};
use tracing::warn;

use crate::resources::ActiveResources;
use crate::workspace::helpers::expand_home;
use crate::workspace::profiles::profile_matches_filter;

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
    pub(crate) password: SecretInputState,
    pub(crate) key_path: InputState,
    pub(crate) passphrase: SecretInputState,
    pub(crate) focused_field: ConnectField,
    pub(crate) error: Option<SharedString>,
    /// Profile this form was prefilled from, if any. Carried into the
    /// connect command so the tab and the saved profile stay linked, and
    /// so "Save profile" updates the same entry instead of duplicating.
    pub(crate) source_profile_id: Option<ProfileId>,
    /// Name entered for saving the current connection as a profile.
    pub(crate) profile_name: InputState,
    /// Whether the inline profile picker panel is expanded.
    pub(crate) profile_picker_open: bool,
    /// Filter text for the inline profile picker list.
    pub(crate) profile_picker_filter: InputState,
    /// Whether the "Save as profile" name + Save row is expanded.
    pub(crate) save_as_expanded: bool,
}

impl ConnectForm {
    pub(crate) fn empty() -> Self {
        Self {
            host: InputState::new(),
            port: InputState::with_value("22"),
            username: InputState::new(),
            auth_method: AuthMethodKind::Password,
            password: SecretInputState::new(),
            key_path: InputState::new(),
            passphrase: SecretInputState::new(),
            focused_field: ConnectField::Host,
            error: None,
            source_profile_id: None,
            profile_name: InputState::new(),
            profile_picker_open: false,
            profile_picker_filter: InputState::new(),
            save_as_expanded: false,
        }
    }

    /// Label for the profile picker trigger: selected profile name, or manual entry.
    pub(crate) fn profile_trigger_label(&self, profiles: &[ConnectionProfile]) -> String {
        if let Some(id) = self.source_profile_id
            && let Some(profile) = profiles.iter().find(|profile| profile.id == id)
        {
            return profile.name.clone();
        }
        "Manual entry".into()
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
            ConnectField::Password => self.password.as_input_state_mut(),
            ConnectField::KeyPath => &mut self.key_path,
            ConnectField::Passphrase => self.passphrase.as_input_state_mut(),
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

    /// Whether auto-connect (e.g. open recent with a profile) has credentials.
    /// Password auth requires a non-empty password so a missing Keychain secret
    /// does not auto-connect with `""`. Private-key auth only requires a key
    /// path; an empty passphrase is valid.
    pub(crate) fn can_auto_connect(&self) -> bool {
        if self.build_settings().is_err() {
            return false;
        }
        match self.auth_method {
            AuthMethodKind::Password => !self.password.value().is_empty(),
            AuthMethodKind::PrivateKey => true,
        }
    }

    /// Detach from the selected profile and clear secret fields for manual
    /// editing. Host/port/username/key_path and auth method stay so the user
    /// can tweak without retyping connection metadata.
    pub(crate) fn switch_to_manual_entry(&mut self) {
        self.source_profile_id = None;
        self.password.clear();
        self.passphrase.clear();
        self.profile_picker_open = false;
        self.profile_picker_filter = InputState::new();
    }

    /// Close the profile picker and clear its filter (Esc / after Manual).
    pub(crate) fn close_profile_picker(&mut self) {
        self.profile_picker_open = false;
        self.profile_picker_filter = InputState::new();
    }
}

impl crate::workspace::Workspace {
    /// Open the connect form for the active tab, prefilling any cached
    /// session credentials or restored non-secret connection metadata.
    pub(crate) fn open_connect_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self
            .state
            .tabs
            .active_tab_id
            .and_then(|id| self.state.tabs.find_tab(id));
        let cached_settings = tab.and_then(|tab| tab.connection_settings.clone());
        let restored_target = tab.and_then(|tab| tab.restored_target.clone());
        let form = if let Some(settings) = cached_settings {
            Some(ConnectForm::prefilled(&settings))
        } else if let Some(restored) = restored_target {
            // Prefer live profile if still present.
            if let Some(profile_id) = restored.profile_id
                && let Some(profile) = cx.resources().profiles.find_profile(profile_id)
            {
                Some(ConnectForm::from_profile(profile))
            } else {
                let mut form = ConnectForm::empty();
                form.host = InputState::with_value(restored.host.clone());
                form.port = InputState::with_value(restored.port.to_string());
                form.username = InputState::with_value(restored.username.clone());
                form.source_profile_id = restored.profile_id;
                Some(form)
            }
        } else {
            None
        }
        .unwrap_or_else(ConnectForm::empty);
        self.connect_form_ui.form = Some(form);
        window.focus(&self.connect_form_ui.focus);
        cx.notify();
    }

    pub(crate) fn close_connect_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.connect_form_ui.form = None;
        self.focus_pane(self.focused_side, window, cx);
        cx.notify();
    }

    pub(crate) fn submit_connect_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = &mut self.connect_form_ui.form else {
            return;
        };
        match form.build_settings() {
            Ok(settings) => {
                let profile_id = form.source_profile_id;
                self.connect_form_ui.form = None;
                self.focus_pane(self.focused_side, window, cx);
                self.connect_with(settings, profile_id, window, cx);
            }
            Err(message) => {
                form.error = Some(message);
                cx.notify();
            }
        }
    }

    /// Allocate a fresh profile id: one past the current maximum so
    /// saved profiles never collide even after deletes.
    pub(crate) fn next_profile_id(&mut self, cx: &App) -> Option<ProfileId> {
        match cx.resources().profiles.next_profile_id() {
            Ok(profile_id) => Some(profile_id),
            Err(error) => {
                self.status_message =
                    Some(format!("Could not allocate profile id: {error}").into());
                None
            }
        }
    }

    /// Prefill the form from a saved profile, including the secret read
    /// back from the Keychain (plan §11). If the secret is missing or the
    /// Keychain is unavailable, the form still opens with metadata filled
    /// and the user re-enters the secret — saving is never blocked.
    pub(crate) fn use_profile(&mut self, profile_id: ProfileId, cx: &mut Context<Self>) {
        let Some(profile) = cx.resources().profiles.find_profile(profile_id).cloned() else {
            return;
        };
        let mut form = ConnectForm::from_profile(&profile);
        match cx.resources().profiles.load_connection_settings(profile_id) {
            Ok(settings) => match &settings.auth {
                AuthCredential::Password { password } => form.password.set_value(password.clone()),
                AuthCredential::PrivateKey { passphrase, .. } => {
                    if let Some(passphrase) = passphrase {
                        form.passphrase.set_value(passphrase.clone());
                    }
                }
            },
            Err(ProfileMutationError::CredentialRequired(AuthMethodKind::Password)) => {
                self.status_message = Some("Saved password not found — re-enter it.".into());
            }
            Err(ProfileMutationError::CredentialRequired(AuthMethodKind::PrivateKey)) => {
                self.status_message = Some("Saved passphrase not found — re-enter it.".into());
            }
            Err(error) => {
                warn!(?profile_id, %error, "could not load saved profile credential");
                self.status_message =
                    Some("Could not read saved credentials — re-enter them.".into());
            }
        }
        // from_profile starts from empty() which already closes the picker;
        // set explicitly so callers that mutate flags before assignment stay correct.
        form.profile_picker_open = false;
        self.connect_form_ui.form = Some(form);
        cx.notify();
    }

    /// Apply a profile from the Connect picker: prefill via `use_profile`,
    /// then ensure the popover is closed and the filter is cleared.
    pub(crate) fn select_connect_profile(&mut self, profile_id: ProfileId, cx: &mut Context<Self>) {
        self.use_profile(profile_id, cx);
        if let Some(form) = &mut self.connect_form_ui.form {
            form.profile_picker_open = false;
            form.profile_picker_filter = InputState::new();
        }
        cx.notify();
    }

    /// Profiles visible in the Connect picker under the current filter.
    pub(crate) fn filtered_connect_profiles<'a>(
        &'a self,
        cx: &'a App,
    ) -> Vec<&'a ConnectionProfile> {
        let query = self
            .connect_form_ui
            .form
            .as_ref()
            .map(|form| form.profile_picker_filter.value().to_string())
            .unwrap_or_default();
        cx.resources()
            .profiles
            .profiles()
            .iter()
            .filter(|profile| profile_matches_filter(profile, &query))
            .collect()
    }

    /// Persist the current form as a profile. If the form came from an
    /// existing profile, the same id is reused (update); otherwise a new
    /// id is allocated. The secret is mapped to a `SecretRef`, written to
    /// the macOS Keychain, and never written to disk (plan §5/§11). The
    /// profile is only flushed to `profiles.json` after the Keychain
    /// write succeeds, so the two never drift apart.
    pub(crate) fn save_current_profile(&mut self, cx: &mut Context<Self>) {
        let settings = match &self.connect_form_ui.form {
            Some(form) => match form.build_settings() {
                Ok(settings) => settings,
                Err(message) => {
                    if let Some(form) = self.connect_form_ui.form.as_mut() {
                        form.error = Some(message);
                    }
                    cx.notify();
                    return;
                }
            },
            None => return,
        };
        let name = self
            .connect_form_ui
            .form
            .as_ref()
            .map(|form| form.profile_name.value().trim().to_string())
            .unwrap_or_default();
        let source_profile_id = self
            .connect_form_ui
            .form
            .as_ref()
            .and_then(|form| form.source_profile_id);
        let name = if name.is_empty() {
            format!("{}@{}", settings.username, settings.host)
        } else {
            name
        };
        let profile_id = match source_profile_id {
            Some(id) if cx.resources().profiles.find_profile(id).is_some() => id,
            _ => {
                let Some(profile_id) = self.next_profile_id(cx) else {
                    cx.notify();
                    return;
                };
                profile_id
            }
        };

        match cx.resources_mut().profiles.save_connection_settings(
            profile_id,
            name.clone(),
            &settings,
        ) {
            Ok(outcome) => {
                for error in outcome.cleanup_warnings {
                    warn!(
                        ?profile_id,
                        %error,
                        "could not remove obsolete Keychain secret after profile update"
                    );
                }
                if let Some(form) = self.connect_form_ui.form.as_mut() {
                    form.source_profile_id = Some(outcome.profile.id);
                    form.profile_name = InputState::new();
                    form.save_as_expanded = false;
                    form.error = None;
                }
                self.status_message = Some(format!("Saved profile '{}'.", name).into());
            }
            Err(error) => {
                self.status_message = Some(format!("Could not save profile: {error}").into());
            }
        }
        cx.notify();
    }

    /// Remove a saved profile from the store and disk, and best-effort
    /// remove its Keychain entries. Drops the link from any open form
    /// that pointed at it.
    pub(crate) fn delete_profile(&mut self, profile_id: ProfileId, cx: &mut Context<Self>) {
        match cx
            .resources_mut()
            .profiles
            .delete_profile_and_credentials(profile_id)
        {
            Ok(outcome) => {
                if let Some(form) = self.connect_form_ui.form.as_mut()
                    && form.source_profile_id == Some(profile_id)
                {
                    form.source_profile_id = None;
                }
                for error in outcome.cleanup_warnings {
                    warn!(
                        ?profile_id,
                        %error,
                        "could not remove Keychain secret for deleted profile"
                    );
                }
                if outcome.deleted {
                    // Decouple cross-store references so no recents entry,
                    // live tab, or future session snapshot keeps pointing at
                    // the deleted id. Best-effort like the Keychain cleanup:
                    // a failure here must not hide the successful deletion.
                    if let Err(error) = cx.resources_mut().recents.forget_profile(profile_id.0) {
                        warn!(
                            ?profile_id,
                            %error,
                            "could not decouple recents from deleted profile"
                        );
                    }
                    for tab in self.state.tabs.tabs.iter_mut() {
                        if tab.profile_id == Some(profile_id) {
                            tab.profile_id = None;
                        }
                        if let Some(target) = tab.restored_target.as_mut()
                            && target.profile_id == Some(profile_id)
                        {
                            target.profile_id = None;
                        }
                    }
                    self.status_message = Some("Deleted profile.".into());
                }
            }
            Err(error) => {
                self.status_message = Some(format!("Could not delete profile: {error}").into());
            }
        }
        cx.notify();
    }

    /// Route keys typed while the connect form has focus to the
    /// focused field. Escape closes the profile picker first when open;
    /// otherwise it is left to the `CancelActiveModal` binding.
    pub(crate) fn handle_connect_form_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(form) = &mut self.connect_form_ui.form else {
            return;
        };
        let keystroke = &event.keystroke;

        if keystroke.key == "escape" {
            if form.profile_picker_open {
                form.close_profile_picker();
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }
        if keystroke.key == "enter" && !keystroke.modifiers.modified() {
            cx.stop_propagation();
            // When the picker is open, Enter selects the first filtered
            // profile (or no-ops if none) — never starts Connect.
            if form.profile_picker_open {
                let first_id = self
                    .filtered_connect_profiles(cx)
                    .first()
                    .map(|profile| profile.id);
                if let Some(profile_id) = first_id {
                    self.select_connect_profile(profile_id, cx);
                }
                return;
            }
            self.submit_connect_form(window, cx);
            return;
        }
        if keystroke.key == "tab" {
            form.cycle_focus(keystroke.modifiers.shift);
            cx.stop_propagation();
            cx.notify();
            return;
        }

        // While the picker is open, typeahead goes to the filter field.
        if form.profile_picker_open {
            if keystroke.modifiers.platform && keystroke.key == "v" {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    form.profile_picker_filter.insert(&text);
                    cx.stop_propagation();
                    cx.notify();
                }
                return;
            }
            if form.profile_picker_filter.handle_keystroke(keystroke) == InputKeyResult::Handled {
                cx.stop_propagation();
                cx.notify();
            }
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
