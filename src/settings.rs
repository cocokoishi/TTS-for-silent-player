use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub voice_name: String,
    pub output_device: String,
    pub rate: i32,
    pub volume: i32,
    pub always_on_top: bool,
    pub speak_on_enter_only: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            voice_name: String::new(),
            output_device: String::new(),
            rate: 0,
            volume: 100,
            always_on_top: false,
            speak_on_enter_only: true,
        }
    }
}

impl Settings {
    fn config_path() -> Option<PathBuf> {
        std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("settings.json")))
    }

    pub fn load() -> Self {
        Self::config_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        if let Some(path) = Self::config_path() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(path, serde_json::to_string_pretty(self).unwrap_or_default());
        }
    }
}
