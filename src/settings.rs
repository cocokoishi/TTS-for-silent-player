use serde::{Deserialize, Deserializer, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TtsMode {
    WindowsOffline,
    Edge,
    OpenaiCompatibleRemote,
}

impl TtsMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::WindowsOffline => "Windows Offline",
            Self::Edge => "Edge TTS",
            Self::OpenaiCompatibleRemote => "OpenAI-Compatible",
        }
    }
}

impl Default for TtsMode {
    fn default() -> Self {
        Self::Edge
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(default)]
pub struct Settings {
    pub voice_name: String,
    pub output_device: String,
    pub play_on_default_speaker: bool,
    pub rate: i32,
    pub volume: i32,
    pub window_opacity: u8,
    pub always_on_top: bool,
    pub speak_on_enter_only: bool,
    pub vrchat_osc_enabled: bool,
    pub vrchat_osc_history_count: u8,
    pub vrchat_osc_use_newlines: bool,
    pub tts_mode: TtsMode,
    pub edge_voice: String,
    pub edge_rate: i32,
    pub edge_volume: i32,
    pub edge_pitch: i32,
    pub remote_api_url: String,
    pub remote_api_key: String,
    pub remote_model: String,
    pub remote_voice: String,
    pub remote_speed: f32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
struct RawSettings {
    voice_name: String,
    output_device: String,
    play_on_default_speaker: bool,
    rate: i32,
    volume: i32,
    window_opacity: u8,
    always_on_top: bool,
    speak_on_enter_only: bool,
    vrchat_osc_enabled: bool,
    vrchat_osc_history_count: u8,
    vrchat_osc_use_newlines: bool,
    tts_mode: Option<TtsMode>,
    use_remote_tts: Option<bool>,
    edge_voice: String,
    edge_rate: i32,
    edge_volume: i32,
    edge_pitch: i32,
    remote_api_url: String,
    remote_api_key: String,
    remote_model: String,
    remote_voice: String,
    remote_speed: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            voice_name: String::new(),
            output_device: String::new(),
            play_on_default_speaker: true,
            rate: 0,
            volume: 100,
            window_opacity: 80,
            always_on_top: false,
            speak_on_enter_only: true,
            vrchat_osc_enabled: false,
            vrchat_osc_history_count: 5,
            vrchat_osc_use_newlines: false,
            tts_mode: TtsMode::Edge,
            edge_voice: "zh-CN-XiaoxiaoNeural".to_string(),
            edge_rate: 0,
            edge_volume: 0,
            edge_pitch: 0,
            remote_api_url: "https://api.openai.com/v1/audio/speech".to_string(),
            remote_api_key: String::new(),
            remote_model: "tts-1".to_string(),
            remote_voice: "alloy".to_string(),
            remote_speed: 1.0,
        }
    }
}

impl Default for RawSettings {
    fn default() -> Self {
        let defaults = Settings::default();
        Self {
            voice_name: defaults.voice_name,
            output_device: defaults.output_device,
            play_on_default_speaker: defaults.play_on_default_speaker,
            rate: defaults.rate,
            volume: defaults.volume,
            window_opacity: defaults.window_opacity,
            always_on_top: defaults.always_on_top,
            speak_on_enter_only: defaults.speak_on_enter_only,
            vrchat_osc_enabled: defaults.vrchat_osc_enabled,
            vrchat_osc_history_count: defaults.vrchat_osc_history_count,
            vrchat_osc_use_newlines: defaults.vrchat_osc_use_newlines,
            tts_mode: None,
            use_remote_tts: None,
            edge_voice: defaults.edge_voice,
            edge_rate: defaults.edge_rate,
            edge_volume: defaults.edge_volume,
            edge_pitch: defaults.edge_pitch,
            remote_api_url: defaults.remote_api_url,
            remote_api_key: defaults.remote_api_key,
            remote_model: defaults.remote_model,
            remote_voice: defaults.remote_voice,
            remote_speed: defaults.remote_speed,
        }
    }
}

impl<'de> Deserialize<'de> for Settings {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawSettings::deserialize(deserializer)?;
        let default_mode = match raw.use_remote_tts {
            Some(true) => TtsMode::OpenaiCompatibleRemote,
            Some(false) => TtsMode::WindowsOffline,
            None => TtsMode::default(),
        };

        Ok(Self {
            voice_name: raw.voice_name,
            output_device: raw.output_device,
            play_on_default_speaker: raw.play_on_default_speaker,
            rate: raw.rate,
            volume: raw.volume,
            window_opacity: raw.window_opacity,
            always_on_top: raw.always_on_top,
            speak_on_enter_only: raw.speak_on_enter_only,
            vrchat_osc_enabled: raw.vrchat_osc_enabled,
            vrchat_osc_history_count: raw.vrchat_osc_history_count,
            vrchat_osc_use_newlines: raw.vrchat_osc_use_newlines,
            tts_mode: raw.tts_mode.unwrap_or(default_mode),
            edge_voice: raw.edge_voice,
            edge_rate: raw.edge_rate,
            edge_volume: raw.edge_volume,
            edge_pitch: raw.edge_pitch,
            remote_api_url: raw.remote_api_url,
            remote_api_key: raw.remote_api_key,
            remote_model: raw.remote_model,
            remote_voice: raw.remote_voice,
            remote_speed: raw.remote_speed,
        })
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

    pub fn config_exists() -> bool {
        Self::config_path().map(|p| p.exists()).unwrap_or(false)
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
