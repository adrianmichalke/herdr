#[derive(Clone, Copy)]
pub(crate) enum ConfigEdit<'a> {
    Theme(&'a str),
    StatusIndicators(super::StatusIndicatorStyle),
    Sound(bool),
    ToastDelivery(super::ToastDelivery),
    TabBar { wrap: bool, max_rows: u16 },
}

impl ConfigEdit<'_> {
    pub(crate) fn description(self) -> &'static str {
        match self {
            Self::Theme(_) => "theme",
            Self::StatusIndicators(_) => "status indicators",
            Self::Sound(_) => "sound setting",
            Self::ToastDelivery(_) => "toast setting",
            Self::TabBar { .. } => "tab bar settings",
        }
    }

    pub(crate) fn apply(self, content: &str) -> String {
        match self {
            Self::Theme(name) => {
                let content =
                    super::upsert_section_value(content, "theme", "name", &format!("\"{name}\""));
                super::upsert_section_bool(&content, "theme", "auto_switch", false)
            }
            Self::StatusIndicators(style) => super::upsert_section_value(
                content,
                "ui",
                "status_indicators",
                &format!("\"{}\"", style.as_str()),
            ),
            Self::Sound(enabled) => {
                super::upsert_section_bool(content, "ui.sound", "enabled", enabled)
            }
            Self::TabBar { wrap, max_rows } => {
                let content = super::upsert_section_bool(content, "ui", "tab_bar_wrap", wrap);
                super::upsert_section_value(
                    &content,
                    "ui",
                    "tab_bar_max_rows",
                    &max_rows.max(1).to_string(),
                )
            }
            Self::ToastDelivery(delivery) => {
                let value = match delivery {
                    super::ToastDelivery::Off => "\"off\"",
                    super::ToastDelivery::Herdr => "\"herdr\"",
                    super::ToastDelivery::Terminal => "\"terminal\"",
                    super::ToastDelivery::System => "\"system\"",
                };
                let content = super::upsert_section_value(content, "ui.toast", "delivery", value);
                super::remove_section_key(&content, "ui.toast", "enabled")
            }
        }
    }
}

pub(crate) fn update_file_at(
    path: &std::path::Path,
    description: &str,
    update: impl FnOnce(&str) -> Result<String, String>,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create config directory: {error}"))?;
    }
    let content = match super::io::read_optional_config(path) {
        Ok(Some(content)) => content,
        Ok(None) => String::new(),
        Err(error) => {
            return Err(format!(
                "failed to read config before saving {description}: {error}"
            ));
        }
    };
    let updated = update(&content)?;
    std::fs::write(path, updated).map_err(|error| format!("failed to save {description}: {error}"))
}

pub(crate) fn write_edit(edit: ConfigEdit<'_>) -> Result<(), String> {
    update_file_at(&super::config_path(), edit.description(), |content| {
        Ok(edit.apply(content))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_bar_edit_preserves_other_settings_and_is_idempotent() {
        let content = r#"onboarding = false

[theme]
name = "nord"

[ui]
# custom sidebar width
sidebar_width = 31
tab_bar_position = "bottom"
tab_bar_wrap = false
tab_bar_max_rows = 7
tab_bar_right = [{ type = "text", text = "prod" }]

[ui.sound]
enabled = false

[keys]
prefix = "ctrl+a"
"#;
        let edit = ConfigEdit::TabBar {
            wrap: true,
            max_rows: 5,
        };
        let updated = edit.apply(content);
        let mut expected: toml::Value = toml::from_str(content).unwrap();
        expected["ui"]["tab_bar_wrap"] = toml::Value::Boolean(true);
        expected["ui"]["tab_bar_max_rows"] = toml::Value::Integer(5);

        assert_eq!(toml::from_str::<toml::Value>(&updated).unwrap(), expected);
        assert!(updated.contains("# custom sidebar width\nsidebar_width = 31"));
        assert_eq!(updated.matches("tab_bar_wrap =").count(), 1);
        assert_eq!(updated.matches("tab_bar_max_rows =").count(), 1);
        assert_eq!(edit.apply(&updated), updated);
    }

    #[test]
    fn tab_bar_edit_adds_missing_keys_and_normalizes_zero() {
        for content in [
            "",
            "[theme]\nname = 'nord'\n",
            "[ui]\nsidebar_width = 31\n",
            "[ui.sound]\nenabled = false\n",
        ] {
            let edit = ConfigEdit::TabBar {
                wrap: false,
                max_rows: 0,
            };
            let updated = edit.apply(content);
            let config: super::super::Config = toml::from_str(&updated).unwrap();
            assert!(!config.ui.tab_bar_wrap);
            assert_eq!(config.ui.tab_bar_max_rows, 1);
            assert_eq!(edit.apply(&updated), updated);
        }
    }

    #[test]
    fn update_file_at_does_not_move_a_leading_bom_into_the_file() {
        let dir = std::env::temp_dir().join(format!("herdr-config-bom-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            b"\xEF\xBB\xBF[terminal]\ndefault_shell = \"pwsh.exe\"\n",
        )
        .unwrap();

        update_file_at(&path, "onboarding setting", |content| {
            Ok(crate::config::upsert_top_level_bool(
                content,
                "onboarding",
                false,
            ))
        })
        .unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_dir_all(dir);

        assert!(
            !written.contains('\u{feff}'),
            "unexpected BOM in {written:?}"
        );
        assert!(
            toml::from_str::<toml::Value>(&written).is_ok(),
            "written config is not valid TOML: {written:?}"
        );
    }
}
