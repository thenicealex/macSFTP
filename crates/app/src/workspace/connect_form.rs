use std::ops::{Deref, DerefMut};

use gpui_kit::base::input::InputEvent;
use gpui_kit::{App, Context, KeyDownEvent, SharedString, Window};
use macsftp_core::{
    AuthCredential, AuthMethod, AuthMethodKind, ConnectionProfile, ConnectionSettings, ProfileId,
    ResolvedConnectionRoute,
};
use macsftp_storage::ProfileMutationError;
use macsftp_ui::{InputKeyResult, InputState, PlainInput};
use tracing::warn;

use crate::resources::ActiveResources;
use crate::workspace::connection_fields::{
    ConnectionFieldKind, ConnectionFields, step_field_focus,
};
use crate::workspace::profiles::profile_matches_filter;

/// One field of the connect form, in Tab-cycle order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ConnectField {
    Host,
    Port,
    Username,
    Password,
    KeyPath,
    Passphrase,
    AgentSocket,
    ProfileFilter,
}

impl ConnectField {
    pub(crate) const fn as_connection_field(self) -> Option<ConnectionFieldKind> {
        match self {
            Self::Host => Some(ConnectionFieldKind::Host),
            Self::Port => Some(ConnectionFieldKind::Port),
            Self::Username => Some(ConnectionFieldKind::Username),
            Self::Password => Some(ConnectionFieldKind::Password),
            Self::KeyPath => Some(ConnectionFieldKind::KeyPath),
            Self::Passphrase => Some(ConnectionFieldKind::Passphrase),
            Self::AgentSocket => Some(ConnectionFieldKind::AgentSocket),
            Self::ProfileFilter => None,
        }
    }

    pub(crate) fn placeholder(self) -> &'static str {
        match self.as_connection_field() {
            Some(kind) => kind.placeholder(),
            None => "Filter saved connections…",
        }
    }
}

/// Connect form state. Secrets live in the input fields only while the
/// form is open; submitting moves them into a zeroized
/// `ConnectionSettings` and drops the form.
pub(crate) struct ConnectForm {
    pub(crate) inputs: std::collections::HashMap<ConnectField, PlainInput>,
    pub(crate) fields: ConnectionFields,
    pub(crate) route: ResolvedConnectionRoute,
    pub(crate) focused_field: ConnectField,
    pub(crate) error: Option<SharedString>,
    /// Profile this form was prefilled from, if any. Carried into the connect
    /// command so the tab and the saved profile stay linked.
    pub(crate) source_profile_id: Option<ProfileId>,
    /// Whether the inline profile picker panel is expanded.
    pub(crate) profile_picker_open: bool,
    /// Filter text for the inline profile picker list.
    pub(crate) profile_picker_filter: InputState,
    /// Whether the password field currently shows characters instead of
    /// bullets. Deliberately per-form: a fresh form always starts masked.
    pub(crate) show_password: bool,
    /// Same toggle for the key passphrase field.
    pub(crate) show_passphrase: bool,
}

impl Deref for ConnectForm {
    type Target = ConnectionFields;

    fn deref(&self) -> &Self::Target {
        &self.fields
    }
}

impl DerefMut for ConnectForm {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.fields
    }
}

impl ConnectForm {
    pub(crate) fn empty() -> Self {
        Self {
            inputs: std::collections::HashMap::new(),
            fields: ConnectionFields::empty(),
            route: ResolvedConnectionRoute::Direct,
            focused_field: ConnectField::Host,
            error: None,
            source_profile_id: None,
            profile_picker_open: false,
            profile_picker_filter: InputState::new(),
            show_password: false,
            show_passphrase: false,
        }
    }

    /// Label for the profile picker trigger: selected profile name, or manual entry.
    pub(crate) fn profile_trigger_label(&self, profiles: &[ConnectionProfile]) -> String {
        if let Some(id) = self.source_profile_id
            && let Some(profile) = profiles.iter().find(|profile| profile.id == id)
        {
            return profile.name.clone();
        }
        "Enter Details".into()
    }

    pub(crate) fn prefilled(settings: &ConnectionSettings) -> Self {
        let mut form = Self::empty();
        form.fields.prefill_from_settings(settings);
        form.route = settings.route.clone();
        // Secrets are deliberately not prefilled.
        form
    }

    /// Prefill the non-secret fields from a saved profile. The secret
    /// itself is restored separately by `use_profile`, which reads it
    /// back from the Keychain (architecture §11).
    pub(crate) fn from_profile(profile: &ConnectionProfile) -> Self {
        let mut form = Self::empty();
        form.fields.prefill_from_profile(profile);
        form.source_profile_id = Some(profile.id);
        form
    }

    pub(crate) fn field_order(&self) -> &'static [ConnectField] {
        match self.auth_method {
            AuthMethodKind::Password => &[
                ConnectField::Host,
                ConnectField::Port,
                ConnectField::Username,
                ConnectField::Password,
            ],
            AuthMethodKind::PrivateKey => &[
                ConnectField::Host,
                ConnectField::Port,
                ConnectField::Username,
                ConnectField::KeyPath,
                ConnectField::Passphrase,
            ],
            AuthMethodKind::KeyboardInteractive => &[
                ConnectField::Host,
                ConnectField::Port,
                ConnectField::Username,
            ],
            AuthMethodKind::SshAgent => &[
                ConnectField::Host,
                ConnectField::Port,
                ConnectField::Username,
                ConnectField::AgentSocket,
            ],
        }
    }

    pub(crate) fn field_state_mut(&mut self, field: ConnectField) -> &mut InputState {
        match field.as_connection_field() {
            Some(kind) => self.fields.field_state_mut(kind),
            None => &mut self.profile_picker_filter,
        }
    }

    pub(crate) fn cycle_focus(&mut self, backwards: bool) {
        self.focused_field = step_field_focus(self.field_order(), self.focused_field, backwards);
    }

    pub(crate) fn set_auth_method(&mut self, method: AuthMethodKind) {
        if self.auth_method != method {
            if self.source_profile_id.is_some() {
                self.detach_from_profile();
            }
            self.auth_method = method;
            // A secret for a method the user just left stops living in the
            // form, and a reveal never survives the switch: coming back to
            // Password has to ask before it shows the characters again.
            self.fields.clear_inactive_secrets(method);
            match method {
                AuthMethodKind::Password => self.show_passphrase = false,
                AuthMethodKind::PrivateKey => self.show_password = false,
                AuthMethodKind::KeyboardInteractive | AuthMethodKind::SshAgent => {
                    self.show_password = false;
                    self.show_passphrase = false;
                }
            }
            if !self.field_order().contains(&self.focused_field) {
                self.focused_field = match method {
                    AuthMethodKind::Password => ConnectField::Password,
                    AuthMethodKind::PrivateKey => ConnectField::KeyPath,
                    AuthMethodKind::KeyboardInteractive | AuthMethodKind::SshAgent => {
                        ConnectField::Username
                    }
                };
            }
        }
    }

    /// Validate and build the settings. Returns `Err` with a short,
    /// user-facing message.
    pub(crate) fn build_settings(&self) -> Result<ConnectionSettings, SharedString> {
        let endpoint = self.fields.validate_endpoint()?;
        let auth = self.fields.build_auth_credential()?;

        Ok(ConnectionSettings {
            host: endpoint.host,
            port: endpoint.port,
            username: endpoint.username,
            auth,
            route: self.route.clone(),
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
            AuthMethodKind::KeyboardInteractive | AuthMethodKind::SshAgent => true,
        }
    }

    /// Detach the form from its source profile when an edit changes the
    /// connection destination or credentials. The Keychain secret restored
    /// for the profile must never travel to a different host or username,
    /// and the tab must not claim the saved profile's identity.
    pub(crate) fn detach_from_profile(&mut self) {
        self.source_profile_id = None;
        self.fields.clear_secrets();
    }

    /// Detach from the selected profile and clear secret fields for manual
    /// editing. Host/port/username/key_path and auth method stay so the user
    /// can tweak without retyping connection metadata.
    pub(crate) fn switch_to_manual_entry(&mut self) {
        self.detach_from_profile();
        self.route = ResolvedConnectionRoute::Direct;
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
    pub(crate) fn prepare_connect_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = self.connect_form_ui.form.as_mut() else {
            return;
        };
        let fresh = form.inputs.is_empty();
        let mut fields = form.field_order().to_vec();
        if form.profile_picker_open {
            fields.push(ConnectField::ProfileFilter);
        }
        for field in fields {
            if matches!(field, ConnectField::Password | ConnectField::Passphrase) {
                continue;
            }
            let value = form.field_state_mut(field).value().to_string();
            let input = form.inputs.entry(field).or_insert_with(|| {
                PlainInput::new(
                    &value,
                    field.placeholder(),
                    window,
                    cx,
                    move |workspace, input, event, window, cx| {
                        let Some(form) = workspace.connect_form_ui.form.as_mut() else {
                            return;
                        };
                        if form
                            .inputs
                            .get(&field)
                            .is_none_or(|binding| binding.state() != input)
                        {
                            return;
                        }
                        match event {
                            InputEvent::Change => {
                                form.field_state_mut(field)
                                    .set_value(input.read(cx).value().to_string());
                                form.error = None;
                                if field != ConnectField::ProfileFilter {
                                    form.focused_field = field;
                                }

                                if let Some(profile_id) = form.source_profile_id {
                                    let target_diverged = if let Some(profile) =
                                        cx.resources().profiles.find_profile(profile_id)
                                    {
                                        match field {
                                            ConnectField::Host => {
                                                form.host.value().trim() != profile.host
                                            }
                                            ConnectField::Port => {
                                                form.port.value().trim() != profile.port.to_string()
                                            }
                                            ConnectField::Username => {
                                                form.username.value().trim() != profile.username
                                            }
                                            ConnectField::KeyPath => match &profile.auth {
                                                AuthMethod::PrivateKey { key_path, .. } => {
                                                    form.key_path.value().trim() != key_path.as_str()
                                                }
                                                _ => true,
                                            },
                                            _ => false,
                                        }
                                    } else {
                                        true
                                    };
                                    if target_diverged {
                                        form.detach_from_profile();
                                        workspace.show_status(
                                            "Saved credentials cleared because connection details were changed."
                                                .into(),cx);
                                    }
                                }
                            }
                            InputEvent::Focus
                                if field != ConnectField::ProfileFilter
                                    && form
                                        .inputs
                                        .get(&field)
                                        .is_some_and(|binding| binding.is_focused(window, cx)) =>
                            {
                                form.focused_field = field
                            }
                            InputEvent::PressEnter {
                                secondary: false,
                                shift: false,
                            } => {
                                if field == ConnectField::ProfileFilter {
                                    let first_id = workspace
                                        .filtered_connect_profiles(cx)
                                        .first()
                                        .map(|profile| profile.id);
                                    if let Some(profile_id) = first_id {
                                        workspace.select_connect_profile(profile_id, cx);
                                    }
                                } else {
                                    workspace.submit_connect_form(window, cx);
                                }
                            }
                            _ => {}
                        }
                        cx.notify();
                    },
                )
            });
            input.sync(&value, window, cx);
        }
        if fresh || form.profile_picker_open || self.connect_form_ui.focus.is_focused(window) {
            let field = if form.profile_picker_open {
                ConnectField::ProfileFilter
            } else {
                form.focused_field
            };
            if let Some(input) = form.inputs.get(&field) {
                input.focus(window, cx);
            }
        }
    }

    pub(crate) fn cycle_connect_focus(
        &mut self,
        backwards: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(form) = self.connect_form_ui.form.as_mut() else {
            return;
        };
        if let Some((&field, _)) = form.inputs.iter().find(|(field, input)| {
            **field != ConnectField::ProfileFilter && input.is_focused(window, cx)
        }) {
            form.focused_field = field;
        }
        form.cycle_focus(backwards);
        self.focus_connect_field(window, cx);
        cx.notify();
    }

    pub(crate) fn focus_connect_field(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = self.connect_form_ui.form.as_ref() else {
            return;
        };
        if let Some(input) = form.inputs.get(&form.focused_field) {
            input.focus(window, cx);
        } else {
            window.focus(&self.connect_form_ui.focus, cx);
        }
    }

    /// Open the connect form for the active tab, prefilling any cached
    /// session credentials or restored non-secret connection metadata.
    pub(crate) fn open_connect_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self
            .state
            .tabs
            .active_tab_id()
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
        window.focus(&self.connect_form_ui.focus, cx);
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
                let profile_id = form.source_profile_id.filter(|&id| {
                    if let Some(profile) = cx.resources().profiles.find_profile(id) {
                        profile.host == settings.host
                            && profile.port == settings.port
                            && profile.username == settings.username
                    } else {
                        false
                    }
                });
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

    /// Prefill the form from a saved profile, including the secret read
    /// back from the Keychain (architecture §11). If the secret is missing or the
    /// Keychain is unavailable, the form still opens with metadata filled
    /// and the user re-enters the secret — saving is never blocked.
    pub(crate) fn use_profile(&mut self, profile_id: ProfileId, cx: &mut Context<Self>) {
        let Some(profile) = cx.resources().profiles.find_profile(profile_id).cloned() else {
            return;
        };
        let mut form = ConnectForm::from_profile(&profile);
        match cx.resources().profiles.load_connection_settings(profile_id) {
            Ok(settings) => {
                form.route = settings.route.clone();
                match &settings.auth {
                    AuthCredential::Password { password } => {
                        form.password.set_value(password.clone())
                    }
                    AuthCredential::PrivateKey { passphrase, .. } => {
                        if let Some(passphrase) = passphrase {
                            form.passphrase.set_value(passphrase.clone());
                        }
                    }
                    AuthCredential::KeyboardInteractive => {}
                    AuthCredential::SshAgent { socket_path } => {
                        if let Some(socket_path) = socket_path {
                            form.agent_socket.set_value(socket_path.clone());
                        }
                    }
                }
            }
            Err(ProfileMutationError::CredentialRequired(AuthMethodKind::Password)) => {
                self.status_message =
                    Some("Your saved password wasn’t found. Enter it again.".into());
            }
            Err(ProfileMutationError::CredentialRequired(AuthMethodKind::PrivateKey)) => {
                self.status_message =
                    Some("Your saved key password wasn’t found. Enter it again.".into());
            }
            Err(error) => {
                warn!(?profile_id, %error, "could not load saved profile credential");
                self.status_message =
                    Some("Your saved sign-in details couldn’t be read. Enter them again.".into());
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
            self.cycle_connect_focus(keystroke.modifiers.shift, window, cx);
            cx.stop_propagation();
            cx.notify();
            return;
        }

        if form.profile_picker_open
            || !matches!(
                form.focused_field,
                ConnectField::Password | ConnectField::Passphrase
            )
        {
            return;
        }

        if keystroke.modifiers.platform && keystroke.key == "v" {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                let focused_field = form.focused_field;
                form.field_state_mut(focused_field).insert(&text);
                form.source_profile_id = None;
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }

        let focused_field = form.focused_field;
        let old_val = form.field_state_mut(focused_field).value().to_string();
        if form
            .field_state_mut(focused_field)
            .handle_keystroke(keystroke)
            == InputKeyResult::Handled
        {
            if form.field_state_mut(focused_field).value() != old_val {
                form.source_profile_id = None;
            }
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
