use macsftp_core::{ConnectionProfile, ProfileId};

use crate::resources::ActiveResources;

/// Settings sidebar section (General appearance vs Profiles management).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SettingsSection {
    #[default]
    General,
    Profiles,
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

impl crate::workspace::Workspace {
    pub(crate) fn set_settings_section(
        &mut self,
        section: SettingsSection,
        cx: &mut gpui::Context<Self>,
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
        }
        cx.notify();
    }

    pub(crate) fn select_profile_in_settings(
        &mut self,
        id: ProfileId,
        cx: &mut gpui::Context<Self>,
    ) {
        self.selected_profile_id = Some(id);
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
}
