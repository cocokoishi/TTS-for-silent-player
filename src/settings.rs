use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub voice_name: String,
    pub output_device: String,
    pub rate: i32,
    pub volume: i32,
    pub window_opacity: u8,
    pub always_on_top: bool,
    pub speak_on_enter_only: bool,
    pub vrchat_osc_enabled: bool,
    pub vrchat_osc_history_count: u8,
    // Remote TTS settings
    pub use_remote_tts: bool,
    pub remote_api_url: String,
    pub remote_api_key: String,
    pub remote_model: String,
    pub remote_voice: String,
    pub remote_speed: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            voice_name: String::new(),
            output_device: String::new(),
            rate: 0,
            volume: 100,
            window_opacity: 66,
            always_on_top: false,
            speak_on_enter_only: true,
            vrchat_osc_enabled: false,
            vrchat_osc_history_count: 2,
            use_remote_tts: false,
            remote_api_url: "https://api.openai.com/v1/audio/speech".to_string(),
            remote_api_key: String::new(),
            remote_model: "tts-1".to_string(),
            remote_voice: "alloy".to_string(),
            remote_speed: 1.0,
        }
    }
}

impl Settings {
    fn config_path() -> Option<PathBuf> {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("settings.json")))
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
