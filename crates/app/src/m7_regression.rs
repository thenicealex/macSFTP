//! M7 回归套件（plan §M7 acceptance）。
//!
//! 运行方式：`cargo test -p macsftp-app m7_`
//!
//! 本模块覆盖**无需 GUI、无需预构建产物**即可自动验证的 M7 验收项：
//! - 版本号单一来源（About 与 plist 均派生自 Cargo package version，禁止分别维护）；
//! - 图标源资源随仓库提交；
//! - 包版本格式为 `x.y.z`。
//!
//! GUI 级验收由既有测试承接，可用下列命令运行：
//! - `cargo test -p macsftp-app about` → `about_action_opens_and_escape_closes_overlay`
//! - `cargo test -p macsftp-app settings` → `settings_action_switches_surface_and_persists_appearance`
//! - `cargo test -p macsftp-ui theme` → `dark_and_light_token_sets_are_both_defined_and_distinct`
//!
//! 与 `docs/m7-test-matrix.md`、`docs/m7-visual-polish.md` 共同构成 M7 验收证据。

#[cfg(test)]
mod tests {
    use std::path::Path;

    /// 解析到 `packaging/macos/<rel>`，与测试运行时的 cwd 无关。
    fn packaging_path(rel: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../packaging/macos")
            .join(rel)
    }

    #[test]
    fn m7_plist_template_uses_single_source_version() {
        let plist = packaging_path("Info.plist.in");
        let contents =
            std::fs::read_to_string(&plist).expect("packaging/macos/Info.plist.in must exist");
        // 版本不得硬编码，必须来自 Cargo package version 的 `@VERSION@` 占位符。
        assert!(
            contents.contains("@VERSION@"),
            "Info.plist.in must derive its version from the Cargo package version via @VERSION@"
        );
        let lines: Vec<&str> = contents.lines().collect();
        let short_idx = lines
            .iter()
            .position(|line| line.contains("CFBundleShortVersionString"))
            .expect("CFBundleShortVersionString key must be present");
        let version_idx = lines
            .iter()
            .position(|line| line.contains("CFBundleVersion"))
            .expect("CFBundleVersion key must be present");
        assert_eq!(
            lines[short_idx + 1].trim(),
            "<string>@VERSION@</string>",
            "CFBundleShortVersionString must use @VERSION@, not a hardcoded version"
        );
        assert_eq!(
            lines[version_idx + 1].trim(),
            "<string>@VERSION@</string>",
            "CFBundleVersion must use @VERSION@, not a hardcoded version"
        );
    }

    #[test]
    fn m7_appicon_source_asset_present() {
        let icon = packaging_path("AppIcon.png");
        let meta = std::fs::metadata(&icon)
            .expect("packaging/macos/AppIcon.png (icon source) must be committed to the repo");
        assert!(
            meta.is_file() && meta.len() > 0,
            "AppIcon.png source must be a non-empty committed file"
        );
    }

    #[test]
    fn m7_package_version_is_well_formed() {
        // 版本号单一来源且为 `x.y.z` 形式（About 与 plist 均派生自此）。
        let version = env!("CARGO_PKG_VERSION");
        assert!(
            is_semver(version),
            "CARGO_PKG_VERSION ({version}) must be in x.y.z form"
        );
    }

    /// 轻量 `x.y.z` 校验，避免引入 regex 依赖。
    fn is_semver(version: &str) -> bool {
        let parts: Vec<&str> = version.split('.').collect();
        parts.len() == 3
            && parts
                .iter()
                .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
    }
}
