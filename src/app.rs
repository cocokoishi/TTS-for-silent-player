use crate::online_tts::{
    RemoteBackend, RemoteSettings, RemoteTts, RemoteTtsCommand, RemoteTtsEvent,
};
use crate::settings::{Settings, TtsMode};
use crate::tts_bridge::{TtsBridge, TtsCommand, TtsEvent};
use crate::vrchat_osc::{
    clamp_history_count, send_chatbox_input, truncate_for_chatbox, VRCHAT_CHATBOX_MAX_LINES,
};
use eframe::egui;
#[cfg(windows)]
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
#[cfg(windows)]
use windows::Win32::Foundation::{COLORREF, HWND};
#[cfg(windows)]
use windows::Win32::Globalization::GetUserDefaultUILanguage;
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowLongPtrW, SetLayeredWindowAttributes, SetWindowLongPtrW, SetWindowPos, GWL_EXSTYLE,
    LWA_ALPHA, SWP_FRAMECHANGED, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, WS_EX_LAYERED,
};

#[cfg(windows)]
pub(crate) fn apply_window_opacity<T: HasWindowHandle>(target: &T, opacity: u8) -> bool {
    let Ok(window_handle) = target.window_handle() else {
        return false;
    };

    let hwnd = match window_handle.as_raw() {
        RawWindowHandle::Win32(handle) => HWND(handle.hwnd.get() as *mut core::ffi::c_void),
        _ => return false,
    };

    unsafe {
        let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let layered_style = WS_EX_LAYERED.0 as isize;

        if opacity >= 100 {
            if ex_style & layered_style != 0 {
                let _ = SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex_style & !layered_style);
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED,
                );
            }

            return true;
        }

        if ex_style & layered_style == 0 {
            let _ = SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex_style | layered_style);
        }

        let alpha = ((opacity.clamp(1, 99) as u16 * 255 + 50) / 100) as u8;
        SetLayeredWindowAttributes(hwnd, COLORREF(0), alpha, LWA_ALPHA).is_ok()
    }
}

#[cfg(not(windows))]
pub(crate) fn apply_window_opacity<T>(_target: &T, _opacity: u8) -> bool {
    false
}

const VRCHAT_OSC_RESET_AFTER_SECONDS_MIN: u16 = 1;
const VRCHAT_OSC_RESET_AFTER_SECONDS_MAX: u16 = 120;
const VB_CABLE_DOWNLOAD_URL: &str =
    "https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack45.zip";

struct VrchatOscHistoryEntry {
    text: String,
    timestamp: Instant,
}

pub struct MugenTtsApp {
    text: String,
    read_end: usize,    // fully read (blue) boundary in bytes
    reading_end: usize, // currently reading (red) boundary in bytes
    pending_trigger_end: Option<usize>,
    is_speaking: bool,
    tts: TtsBridge,
    remote_tts: RemoteTts,
    settings: Settings,
    show_settings: bool,
    show_remote_settings: bool,
    voices: Vec<String>,
    edge_voices: Vec<String>,
    devices: Vec<String>,
    selected_voice_idx: usize,
    selected_edge_voice_idx: usize,
    selected_device_idx: usize,
    focus_flag: Arc<AtomicBool>,
    last_status_poll: Instant,
    speak_start_time: Instant,
    initialized: bool,
    pending_list: u8, // bit 0 = voices, bit 1 = devices
    scroll_to_bottom: bool,
    ime_composing: bool,
    show_remote_error_notice: bool,
    last_online_error_message: String,
    edge_voices_requested: bool,
    show_startup_guide_on_launch: bool,
    startup_guide_checked: bool,
    show_vbcable_notice: bool,
    show_quick_start_guide: bool,
    show_quick_start_guide_after_vbcable_notice: bool,
    chinese_ui_locale: bool,
    last_applied_window_opacity: Option<u8>,
    pending_window_opacity_reapply_frames: u8,
    vrchat_recent_red_chunks: VecDeque<VrchatOscHistoryEntry>,
}

impl MugenTtsApp {
    pub fn new(focus_flag: Arc<AtomicBool>, show_startup_guide_on_launch: bool) -> Self {
        let settings = Settings::load();
        let tts = TtsBridge::spawn();
        let remote_tts = RemoteTts::spawn();

        Self {
            text: String::new(),
            read_end: 0,
            reading_end: 0,
            pending_trigger_end: None,
            is_speaking: false,
            tts,
            remote_tts,
            settings,
            show_settings: false,
            show_remote_settings: false,
            voices: Vec::new(),
            edge_voices: Vec::new(),
            devices: Vec::new(),
            selected_voice_idx: 0,
            selected_edge_voice_idx: 0,
            selected_device_idx: 0,
            focus_flag,
            last_status_poll: Instant::now(),
            speak_start_time: Instant::now(),
            initialized: false,
            pending_list: 0,
            scroll_to_bottom: false,
            ime_composing: false,
            show_remote_error_notice: false,
            last_online_error_message: String::new(),
            edge_voices_requested: false,
            show_startup_guide_on_launch,
            startup_guide_checked: false,
            show_vbcable_notice: false,
            show_quick_start_guide: false,
            show_quick_start_guide_after_vbcable_notice: false,
            chinese_ui_locale: Self::is_chinese_ui_locale(),
            last_applied_window_opacity: None,
            pending_window_opacity_reapply_frames: 45,
            vrchat_recent_red_chunks: VecDeque::new(),
        }
    }

    fn apply_settings(&self) {
        if !self.settings.voice_name.is_empty() {
            self.tts
                .send(TtsCommand::SetVoice(self.settings.voice_name.clone()));
        }
        if !self.settings.output_device.is_empty() {
            self.tts
                .send(TtsCommand::SetDevice(self.settings.output_device.clone()));
        }
        self.tts.send(TtsCommand::SetRate(self.settings.rate));
        self.tts.send(TtsCommand::SetVolume(self.settings.volume));
        self.tts.send(TtsCommand::SetMirrorToDefault(
            self.should_mirror_to_default_speaker(),
        ));
    }

    fn using_windows_offline(&self) -> bool {
        matches!(self.settings.tts_mode, TtsMode::WindowsOffline)
    }

    fn using_edge_tts(&self) -> bool {
        matches!(self.settings.tts_mode, TtsMode::Edge)
    }

    fn using_online_tts(&self) -> bool {
        matches!(
            self.settings.tts_mode,
            TtsMode::Edge | TtsMode::OpenaiCompatibleRemote
        )
    }

    fn should_mirror_to_default_speaker(&self) -> bool {
        self.settings.play_on_default_speaker
            && !RemoteTts::is_matching_default_output(&self.settings.output_device)
    }

    fn request_edge_voices_if_needed(&mut self) {
        if !self.edge_voices_requested {
            self.remote_tts.send(RemoteTtsCommand::ListEdgeVoices);
            self.edge_voices_requested = true;
        }
    }

    fn stop_all_tts(&mut self) {
        self.tts.send(TtsCommand::Stop);
        self.remote_tts.send(RemoteTtsCommand::Stop);
        self.pending_trigger_end = None;
        self.reading_end = self.read_end;
        self.is_speaking = false;
    }

    fn clear_text_and_stop(&mut self) {
        self.text.clear();
        self.read_end = 0;
        self.reading_end = 0;
        self.pending_trigger_end = None;
        self.clear_vrchat_chatbox_history();
        self.tts.send(TtsCommand::Stop);
        self.remote_tts.send(RemoteTtsCommand::Stop);
        self.is_speaking = false;
    }

    fn apply_mode_change(&mut self) {
        self.stop_all_tts();
        self.show_remote_error_notice = false;
        self.last_online_error_message.clear();
        if self.using_edge_tts() {
            self.request_edge_voices_if_needed();
        }
        self.settings.save();
    }

    fn build_remote_settings(&self) -> RemoteSettings {
        RemoteSettings {
            backend: if self.using_edge_tts() {
                RemoteBackend::Edge
            } else {
                RemoteBackend::OpenAiCompatible
            },
            output_device: self.settings.output_device.clone(),
            play_on_default_speaker: self.settings.play_on_default_speaker,
            api_url: self.settings.remote_api_url.clone(),
            api_key: self.settings.remote_api_key.clone(),
            model: self.settings.remote_model.clone(),
            voice: self.settings.remote_voice.clone(),
            speed: self.settings.remote_speed,
            edge_voice: self.settings.edge_voice.clone(),
            edge_rate: self.settings.edge_rate,
            edge_volume: self.settings.edge_volume,
            edge_pitch: self.settings.edge_pitch,
        }
    }

    fn has_vbcable_device(devices: &[String]) -> bool {
        devices.iter().any(|device| {
            let lower = device.to_lowercase();
            lower.contains("cable") || lower.contains("vb-audio")
        })
    }

    #[cfg(windows)]
    fn is_chinese_ui_locale() -> bool {
        let lang_id = unsafe { GetUserDefaultUILanguage() };
        let primary_lang_id = (lang_id as u16) & 0x03ff;
        primary_lang_id == 0x04
    }

    #[cfg(not(windows))]
    fn is_chinese_ui_locale() -> bool {
        false
    }

    fn tutorial_button_label(&self) -> &'static str {
        if self.chinese_ui_locale {
            "快速使用教程"
        } else {
            "Tutorial"
        }
    }

    fn maybe_start_startup_guide(&mut self) {
        if !self.show_startup_guide_on_launch || self.startup_guide_checked {
            return;
        }

        self.startup_guide_checked = true;

        if Self::has_vbcable_device(&self.devices) {
            self.show_quick_start_guide = true;
        } else {
            self.show_vbcable_notice = true;
            self.show_quick_start_guide_after_vbcable_notice = true;
        }
    }

    fn close_vbcable_notice(&mut self) {
        self.show_vbcable_notice = false;

        if self.show_quick_start_guide_after_vbcable_notice {
            self.show_quick_start_guide_after_vbcable_notice = false;
            self.show_quick_start_guide = true;
        }
    }

    fn close_quick_start_guide(&mut self) {
        self.show_quick_start_guide = false;

        if !self.settings.quick_start_completed {
            self.settings.quick_start_completed = true;
            self.settings.save();
        }
    }

    fn get_safe_boundaries(text: &str, read_end: usize, reading_end: usize) -> (usize, usize) {
        let mut re = read_end.min(text.len());
        while re > 0 && !text.is_char_boundary(re) {
            re -= 1;
        }
        let mut rge = reading_end.min(text.len());
        while rge > 0 && !text.is_char_boundary(rge) {
            rge -= 1;
        }
        if rge < re {
            rge = re;
        }
        (re, rge)
    }

    fn clamp_vrchat_osc_reset_after_seconds(seconds: u16) -> u16 {
        seconds.clamp(
            VRCHAT_OSC_RESET_AFTER_SECONDS_MIN,
            VRCHAT_OSC_RESET_AFTER_SECONDS_MAX,
        )
    }

    fn queue_or_trigger_speak_up_to(&mut self, target_idx: usize) {
        let (_, safe_target) = Self::get_safe_boundaries(&self.text, target_idx, target_idx);
        if self.is_speaking {
            self.pending_trigger_end = Some(
                self.pending_trigger_end
                    .map(|pending| pending.max(safe_target))
                    .unwrap_or(safe_target),
            );
            return;
        }

        self.trigger_speak_up_to(safe_target);
    }

    fn trigger_speak_up_to(&mut self, target_idx: usize) {
        let (safe_read_end, _) =
            Self::get_safe_boundaries(&self.text, self.read_end, self.read_end);
        let (_, safe_target) = Self::get_safe_boundaries(&self.text, target_idx, target_idx);

        if safe_target <= safe_read_end {
            return;
        }

        let chunk = self.text[safe_read_end..safe_target].to_string();
        if chunk.trim().is_empty() {
            // Unread portion was just spaces, skip speaking but advance pointers
            self.read_end = safe_target;
            self.reading_end = safe_target;
            return;
        }

        self.reading_end = safe_target;
        self.is_speaking = true;
        self.speak_start_time = Instant::now();
        self.push_vrchat_chatbox_update(&chunk);

        if self.using_online_tts() {
            self.remote_tts
                .send(RemoteTtsCommand::Speak(chunk, self.build_remote_settings()));
        } else {
            self.tts.send(TtsCommand::Speak(chunk));
        }
    }

    fn finish_current_speech(&mut self) {
        if self.is_speaking {
            self.read_end = self.reading_end;
            self.is_speaking = false;
        }

        if let Some(next_target) = self.pending_trigger_end.take() {
            if next_target > self.read_end {
                self.trigger_speak_up_to(next_target);
            }
        }
    }

    fn fail_current_speech(&mut self) {
        self.reading_end = self.read_end;
        self.pending_trigger_end = None;
        self.is_speaking = false;
    }

    fn get_common_prefix_len(a: &str, b: &str) -> usize {
        let mut len = 0;
        for (ca, cb) in a.chars().zip(b.chars()) {
            if ca == cb {
                len += ca.len_utf8();
            } else {
                break;
            }
        }
        len
    }

    fn get_text_change_ranges(old_text: &str, new_text: &str) -> (usize, usize, usize) {
        let prefix_len = Self::get_common_prefix_len(old_text, new_text);
        let mut old_changed_end = old_text.len();
        let mut new_changed_end = new_text.len();

        while old_changed_end > prefix_len && new_changed_end > prefix_len {
            let old_char = old_text[..old_changed_end].chars().next_back().unwrap();
            let new_char = new_text[..new_changed_end].chars().next_back().unwrap();
            if old_char != new_char {
                break;
            }

            let char_len = old_char.len_utf8();
            old_changed_end -= char_len;
            new_changed_end -= char_len;
        }

        (prefix_len, old_changed_end, new_changed_end)
    }

    fn inserted_text_contains_newline(old_text: &str, new_text: &str) -> bool {
        let (prefix_len, _, new_changed_end) = Self::get_text_change_ranges(old_text, new_text);
        new_text[prefix_len..new_changed_end].contains('\n')
    }

    fn strip_spurious_ime_newline(old_text: &str, new_text: &str) -> Option<String> {
        let (prefix_len, old_changed_end, new_changed_end) =
            Self::get_text_change_ranges(old_text, new_text);
        let old_changed = &old_text[prefix_len..old_changed_end];
        let new_changed = &new_text[prefix_len..new_changed_end];

        if !new_changed.ends_with('\n') || old_changed.ends_with('\n') {
            return None;
        }

        let mut fixed_text = String::with_capacity(new_text.len().saturating_sub(1));
        fixed_text.push_str(&new_text[..prefix_len]);
        fixed_text.push_str(&new_changed[..new_changed.len() - 1]);
        fixed_text.push_str(&new_text[new_changed_end..]);
        Some(fixed_text)
    }

    fn push_vrchat_chatbox_update(&mut self, chunk: &str) {
        let line = chunk.trim();
        if line.is_empty() {
            return;
        }

        let now = Instant::now();
        let reset_after_seconds = Self::clamp_vrchat_osc_reset_after_seconds(
            self.settings.vrchat_osc_reset_after_seconds,
        );
        if self.settings.vrchat_osc_reset_after_enabled
            && self
                .vrchat_recent_red_chunks
                .back()
                .map(|entry| entry.timestamp.elapsed() >= Duration::from_secs(reset_after_seconds as u64))
                .unwrap_or(false)
        {
            self.vrchat_recent_red_chunks.clear();
        }

        self.vrchat_recent_red_chunks.push_back(VrchatOscHistoryEntry {
            text: line.to_string(),
            timestamp: now,
        });
        while self.vrchat_recent_red_chunks.len() > VRCHAT_CHATBOX_MAX_LINES as usize {
            self.vrchat_recent_red_chunks.pop_front();
        }

        if !self.settings.vrchat_osc_enabled {
            return;
        }

        if let Some(chatbox_text) = self.build_vrchat_chatbox_text() {
            let _ = send_chatbox_input(&chatbox_text);
        }
    }

    fn build_vrchat_chatbox_text(&self) -> Option<String> {
        let history_count = clamp_history_count(self.settings.vrchat_osc_history_count) as usize;
        let recent_lines: Vec<&str> = self
            .vrchat_recent_red_chunks
            .iter()
            .rev()
            .take(history_count)
            .map(|entry| entry.text.as_str())
            .collect();

        if recent_lines.is_empty() {
            return None;
        }

        let combined = recent_lines
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join(if self.settings.vrchat_osc_use_newlines {
                "\n"
            } else {
                " "
            });
        let truncated = truncate_for_chatbox(&combined);
        if truncated.is_empty() {
            None
        } else {
            Some(truncated)
        }
    }

    fn clear_vrchat_chatbox_history(&mut self) {
        self.vrchat_recent_red_chunks.clear();
    }

    fn is_trigger_char(c: char) -> bool {
        c == '\n'
            || c == ','
            || c == '.'
            || c == '!'
            || c == '?'
            || c == ';'
            || c == ':'
            || c == '、'
            || c == '。'
            || c == '！'
            || c == '？'
            || c == '，'
            || c == '；'
            || c == '：'
            || c == '…'
    }

    fn is_cjk_char(c: char) -> bool {
        matches!(c,
            '\u{4E00}'..='\u{9FFF}'     // CJK Unified Ideographs
            | '\u{3400}'..='\u{4DBF}'   // CJK Extension A
            | '\u{F900}'..='\u{FAFF}'   // CJK Compatibility Ideographs
            | '\u{2E80}'..='\u{2EFF}'   // CJK Radicals Supplement
            | '\u{20000}'..='\u{2A6DF}' // CJK Extension B
        )
    }
}

impl eframe::App for MugenTtsApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if self.using_edge_tts() && self.edge_voices.is_empty() {
            self.request_edge_voices_if_needed();
        }

        let should_retry_window_opacity = self.pending_window_opacity_reapply_frames > 0;
        if should_retry_window_opacity
            || self.last_applied_window_opacity != Some(self.settings.window_opacity)
        {
            if apply_window_opacity(frame, self.settings.window_opacity) {
                self.last_applied_window_opacity = Some(self.settings.window_opacity);
            }

            if should_retry_window_opacity {
                self.pending_window_opacity_reapply_frames =
                    self.pending_window_opacity_reapply_frames.saturating_sub(1);
                ctx.request_repaint();
            }
        }
        // Process TTS events
        for event in self.tts.poll_events() {
            match event {
                TtsEvent::Ready => {
                    self.apply_settings();
                    self.tts.send(TtsCommand::ListVoices);
                    self.pending_list = 1;
                }
                TtsEvent::Voices(v) => {
                    self.voices = v;
                    if self.settings.voice_name.is_empty() {
                        if let Some(cn_voice) = self.voices.iter().find(|x| {
                            x.contains("Chinese")
                                || x.contains("Han")
                                || x.contains("Huihui")
                                || x.contains("Yaoyao")
                                || x.contains("Kangkang")
                        }) {
                            self.settings.voice_name = cn_voice.clone();
                            self.tts.send(TtsCommand::SetVoice(cn_voice.clone()));
                        } else if let Some(first) = self.voices.first() {
                            self.settings.voice_name = first.clone();
                            self.tts.send(TtsCommand::SetVoice(first.clone()));
                        }
                        self.settings.save();
                    }
                    // Find selected index
                    self.selected_voice_idx = self
                        .voices
                        .iter()
                        .position(|n| n == &self.settings.voice_name)
                        .unwrap_or(0);
                    if self.pending_list == 1 {
                        self.tts.send(TtsCommand::ListDevices);
                        self.pending_list = 2;
                    }
                }
                TtsEvent::Devices(d) => {
                    self.devices = d;
                    if self.settings.output_device.is_empty() {
                        if let Some(cable) = self
                            .devices
                            .iter()
                            .find(|x| x.to_lowercase().contains("cable"))
                        {
                            self.settings.output_device = cable.clone();
                            self.tts.send(TtsCommand::SetDevice(cable.clone()));
                            self.tts.send(TtsCommand::SetMirrorToDefault(
                                self.should_mirror_to_default_speaker(),
                            ));
                            self.settings.save();
                        }
                    }

                    self.maybe_start_startup_guide();

                    self.selected_device_idx = self
                        .devices
                        .iter()
                        .position(|n| n.contains(&self.settings.output_device))
                        .unwrap_or(0);
                    self.pending_list = 0;
                    self.initialized = true;
                }

                TtsEvent::SpeakingState(speaking) => {
                    if self.using_windows_offline() && !speaking && self.is_speaking {
                        self.finish_current_speech();
                    }
                }
                TtsEvent::Error(_e) => {
                    if self.using_windows_offline() {
                        self.fail_current_speech();
                    }
                }
            }
        }

        for event in self.remote_tts.poll_events() {
            match event {
                RemoteTtsEvent::PlaybackFinished => {
                    if self.using_online_tts() {
                        self.finish_current_speech();
                    }
                }
                RemoteTtsEvent::SpeakFailed {
                    message,
                    consecutive_failures: _consecutive_failures,
                    sticky_error,
                } => {
                    if sticky_error {
                        self.show_remote_error_notice = true;
                        self.last_online_error_message = message;
                    }
                    if self.using_online_tts() {
                        self.fail_current_speech();
                    }
                }
                RemoteTtsEvent::ConnectionRecovered => {
                    self.show_remote_error_notice = false;
                    self.last_online_error_message.clear();
                }
                RemoteTtsEvent::EdgeVoices(voices) => {
                    self.edge_voices_requested = true;
                    self.edge_voices = voices;
                    if self.settings.edge_voice.is_empty() {
                        if let Some(chinese_voice) = self
                            .edge_voices
                            .iter()
                            .find(|name| name.starts_with("zh-"))
                            .cloned()
                        {
                            self.settings.edge_voice = chinese_voice;
                        } else if let Some(first) = self.edge_voices.first() {
                            self.settings.edge_voice = first.clone();
                        }
                        self.settings.save();
                    }
                    self.selected_edge_voice_idx = self
                        .edge_voices
                        .iter()
                        .position(|name| name == &self.settings.edge_voice)
                        .unwrap_or(0);
                }
                RemoteTtsEvent::EdgeVoicesFailed(message) => {
                    self.edge_voices_requested = false;
                    self.show_remote_error_notice = true;
                    self.last_online_error_message = message;
                }
            }
        }

        // Poll speaking status periodically (wait at least 400ms after starting to speak before polling to avoid queueing race condition)
        if self.using_windows_offline()
            && self.is_speaking
            && self.speak_start_time.elapsed().as_millis() > 400
            && self.last_status_poll.elapsed().as_millis() > 100
        {
            self.tts.send(TtsCommand::QueryStatus);
            self.last_status_poll = Instant::now();
        }

        // Check focus flag (global hotkey)
        if self.focus_flag.swap(false, Ordering::Relaxed) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }

        // Only poll and repaint continuously while the native TTS bridge is actively speaking.
        if self.using_windows_offline() && self.is_speaking {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }

        let bg = egui::Color32::from_rgb(240, 240, 245); // Light background for the entire window
        let panel_bg = egui::Color32::from_rgb(230, 230, 235);
        let accent = egui::Color32::from_rgb(80, 90, 200);

        // Main UI
        let frame = egui::Frame::default()
            .fill(bg)
            .inner_margin(egui::Margin::same(0.0));

        // Detect IME composition state from events this frame
        let mut ime_committed_this_frame = false;
        ctx.input(|i| {
            for event in &i.events {
                if let egui::Event::Ime(ime_event) = event {
                    match ime_event {
                        egui::ImeEvent::Preedit(text) => {
                            self.ime_composing = !text.is_empty();
                        }
                        egui::ImeEvent::Commit(_) => {
                            self.ime_composing = false;
                            ime_committed_this_frame = true;
                        }
                        _ => {}
                    }
                }
            }
        });

        // Capture old text before UI modifies it
        let old_text = self.text.clone();

        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            // Allow dragging the window from any unoccupied space in CentralPanel
            let drag_response =
                ui.interact(ui.max_rect(), ui.id().with("bg_drag"), egui::Sense::drag());
            if drag_response.dragged() {
                ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }

            if self.show_settings {
                self.render_settings(ui, panel_bg, accent);
                ui.add_space(4.0);
            }

            // Text area with highlighting inside a scroll area
            let available = ui.available_size();

            let read_end = self.read_end;
            let reading_end = self.reading_end;

            let font_id = egui::FontId::new(18.0, egui::FontFamily::Proportional);
            let mut layouter = move |ui: &egui::Ui, text: &str, wrap_width: f32| {
                let mut job = egui::text::LayoutJob::default();
                job.wrap.max_width = wrap_width;

                let (re, rge) = MugenTtsApp::get_safe_boundaries(text, read_end, reading_end);

                // Already read portion - light blue bg
                if re > 0 {
                    job.append(
                        &text[..re],
                        0.0,
                        egui::TextFormat {
                            font_id: font_id.clone(),
                            color: egui::Color32::BLACK,
                            background: egui::Color32::from_rgba_unmultiplied(80, 130, 220, 150),
                            ..Default::default()
                        },
                    );
                }

                // Currently reading portion - light red bg
                if rge > re {
                    job.append(
                        &text[re..rge],
                        0.0,
                        egui::TextFormat {
                            font_id: font_id.clone(),
                            color: egui::Color32::BLACK,
                            background: egui::Color32::from_rgba_unmultiplied(220, 60, 60, 150),
                            ..Default::default()
                        },
                    );
                }

                // Not yet read portion
                if rge < text.len() {
                    job.append(
                        &text[rge..],
                        0.0,
                        egui::TextFormat {
                            font_id: font_id.clone(),
                            color: egui::Color32::BLACK,
                            ..Default::default()
                        },
                    );
                }

                ui.fonts(|f| f.layout_job(job))
            };

            let scroll_to_bottom = self.scroll_to_bottom;
            self.scroll_to_bottom = false;

            let mut scroll_area = egui::ScrollArea::vertical()
                .max_height(available.y)
                .auto_shrink([false, false])
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible);

            if scroll_to_bottom {
                scroll_area = scroll_area.stick_to_bottom(true);
            }

            scroll_area.show(ui, |ui| {
                let text_edit = egui::TextEdit::multiline(&mut self.text)
                    .desired_width(available.x - 14.0) // leave room for scrollbar
                    .desired_rows(6)
                    .frame(false)
                    .margin(egui::vec2(10.0, 10.0))
                    .font(egui::FontId::new(18.0, egui::FontFamily::Proportional))
                    .text_color(egui::Color32::BLACK)
                    .layouter(&mut layouter)
                    .hint_text(
                        egui::RichText::new("Enter text")
                            .color(egui::Color32::from_rgb(150, 150, 160)),
                    );

                ui.add(text_edit);
            });
        });

        // Floating buttons overlaid in top right (70% transparent)
        egui::Area::new(egui::Id::new("floating_btns"))
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-10.0, 10.0))
            .interactable(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let reset_btn = ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("⏹ Reset")
                                    .size(13.0)
                                    .color(egui::Color32::from_rgba_unmultiplied(200, 80, 80, 180)),
                            )
                            .fill(egui::Color32::from_rgba_unmultiplied(230, 230, 235, 77))
                            .rounding(egui::Rounding::same(4.0)),
                        )
                        .on_hover_text("Stop speaking and clear text");

                    if reset_btn.clicked() {
                        self.clear_text_and_stop();
                    }

                    ui.add_space(4.0);

                    let settings_btn = ui.add(
                        egui::Button::new(egui::RichText::new("⚙").size(16.0).color(
                            if self.show_settings {
                                egui::Color32::from_rgba_unmultiplied(80, 90, 200, 180)
                            } else {
                                egui::Color32::from_rgba_unmultiplied(100, 100, 110, 180)
                            },
                        ))
                        .fill(egui::Color32::from_rgba_unmultiplied(230, 230, 235, 77))
                        .rounding(egui::Rounding::same(4.0)),
                    );

                    if settings_btn.clicked() {
                        self.show_settings = !self.show_settings;
                        if self.show_settings && !self.initialized {
                            self.tts.send(TtsCommand::ListVoices);
                            self.pending_list = 1;
                        }
                    }
                });
            });

        if self.show_remote_error_notice {
            egui::Area::new(egui::Id::new("remote_error_notice"))
                .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-12.0, -12.0))
                .interactable(false)
                .show(ctx, |ui| {
                    egui::Frame::default()
                        .fill(egui::Color32::from_rgba_unmultiplied(150, 30, 30, 150))
                        .rounding(egui::Rounding::same(8.0))
                        .inner_margin(egui::Margin::symmetric(12.0, 8.0))
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new("online tts error")
                                    .color(egui::Color32::from_rgb(255, 245, 245))
                                    .size(13.0),
                            );
                            if !self.last_online_error_message.is_empty() {
                                ui.label(
                                    egui::RichText::new(self.last_online_error_message.clone())
                                        .color(egui::Color32::from_rgb(255, 230, 230))
                                        .size(11.0),
                                );
                            }
                        });
                });
        }

        // Some IMEs emit both a commit and a trailing Enter into multiline TextEdit.
        // Strip only the synthetic newline from the actual edited span.
        if ime_committed_this_frame {
            if let Some(fixed_text) = Self::strip_spurious_ime_newline(&old_text, &self.text) {
                self.text = fixed_text;
            }
        }

        let enter_triggered = self.text != old_text
            && Self::inserted_text_contains_newline(&old_text, &self.text)
            && !self.ime_composing
            && !ime_committed_this_frame;

        // Handle text detection AFTER the UI has been drawn (no borrow conflict)
        if self.text != old_text || enter_triggered {
            self.scroll_to_bottom = true;
            if self.text != old_text {
                let cpl = Self::get_common_prefix_len(&old_text, &self.text);
                if cpl < self.read_end || cpl < self.reading_end {
                    self.clear_vrchat_chatbox_history();
                }
                self.read_end = self.read_end.min(cpl);
                self.reading_end = self.reading_end.min(cpl);
                self.pending_trigger_end = self.pending_trigger_end.map(|pending| pending.min(cpl));
            }

            let mut trigger_idx = None;
            let (_, rge) = Self::get_safe_boundaries(&self.text, self.read_end, self.reading_end);

            if enter_triggered {
                // Read everything on Enter
                trigger_idx = Some(self.text.len());
            } else if self.text != old_text && !self.settings.speak_on_enter_only {
                // Scan unread portion for trigger chars (punctuation, or space after CJK)
                let unread_text = &self.text[rge..];
                let mut prev_char: Option<char> = if rge > 0 {
                    self.text[..rge].chars().last()
                } else {
                    None
                };
                let mut last_trigger_end: Option<usize> = None;
                for (byte_idx, ch) in unread_text.char_indices() {
                    if Self::is_trigger_char(ch)
                        || (ch == ' ' && prev_char.map_or(false, Self::is_cjk_char))
                    {
                        last_trigger_end = Some(byte_idx + ch.len_utf8());
                    }
                    prev_char = Some(ch);
                }
                if let Some(offset) = last_trigger_end {
                    trigger_idx = Some(rge + offset);
                }
            }

            if let Some(idx) = trigger_idx {
                if idx > rge {
                    self.queue_or_trigger_speak_up_to(idx);
                }
            }
        }

        // Settings window (modal overlay)
        self.render_settings_window(ctx);
        self.render_vbcable_notice_window(ctx);
        self.render_quick_start_guide_window(ctx);
    }
}

impl MugenTtsApp {
    fn render_settings(
        &mut self,
        ui: &mut egui::Ui,
        panel_bg: egui::Color32,
        _accent: egui::Color32,
    ) {
        let settings_frame = egui::Frame::default()
            .fill(panel_bg)
            .rounding(egui::Rounding::same(8.0))
            .stroke(egui::Stroke::new(
                1.0,
                egui::Color32::from_rgb(200, 200, 210),
            ))
            .inner_margin(egui::Margin::same(12.0));

        settings_frame.show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Settings")
                        .color(egui::Color32::from_rgb(40, 40, 50))
                        .size(13.0)
                        .strong(),
                );

                ui.add_space(8.0);

                if ui
                    .button(egui::RichText::new(self.tutorial_button_label()).size(11.0))
                    .clicked()
                {
                    self.show_quick_start_guide = true;
                }
            });
            ui.add_space(4.0);

            ui.add_space(2.0);

            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Mode")
                        .color(egui::Color32::from_rgb(80, 80, 90))
                        .size(12.0),
                );

                let mut mode = self.settings.tts_mode;
                egui::ComboBox::from_id_salt("tts_mode_combo")
                    .selected_text(mode.label())
                    .width(ui.available_width() - 96.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut mode,
                            TtsMode::WindowsOffline,
                            TtsMode::WindowsOffline.label(),
                        );
                        ui.selectable_value(&mut mode, TtsMode::Edge, TtsMode::Edge.label());
                        ui.selectable_value(
                            &mut mode,
                            TtsMode::OpenaiCompatibleRemote,
                            TtsMode::OpenaiCompatibleRemote.label(),
                        );
                    });

                if mode != self.settings.tts_mode {
                    self.settings.tts_mode = mode;
                    self.apply_mode_change();
                }

                if self.using_online_tts()
                    && ui
                        .button(egui::RichText::new("Configure").size(11.0))
                        .clicked()
                {
                    if self.using_edge_tts() {
                        self.request_edge_voices_if_needed();
                    }
                    self.show_remote_settings = true;
                }
            });

            ui.add_space(2.0);

            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Output")
                        .color(egui::Color32::from_rgb(80, 80, 90))
                        .size(12.0),
                );
                let current = if self.selected_device_idx < self.devices.len() {
                    self.devices[self.selected_device_idx].clone()
                } else {
                    "Loading...".to_string()
                };
                egui::ComboBox::from_id_salt("device_combo")
                    .selected_text(&current)
                    .width(ui.available_width() - 10.0)
                    .show_ui(ui, |ui| {
                        for (i, d) in self.devices.iter().enumerate() {
                            if ui
                                .selectable_label(i == self.selected_device_idx, d)
                                .clicked()
                                {
                                    self.selected_device_idx = i;
                                    self.settings.output_device = d.clone();
                                    self.tts.send(TtsCommand::SetDevice(d.clone()));
                                    self.tts.send(TtsCommand::SetMirrorToDefault(
                                        self.should_mirror_to_default_speaker(),
                                    ));
                                    self.settings.save();
                                }
                            }
                    });
            });

            ui.add_space(2.0);

            ui.add_enabled_ui(self.using_windows_offline(), |ui| {
                // Voice selection
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Voice")
                            .color(egui::Color32::from_rgb(80, 80, 90))
                            .size(12.0),
                    );
                    let current = if self.selected_voice_idx < self.voices.len() {
                        self.voices[self.selected_voice_idx].clone()
                    } else {
                        "Loading...".to_string()
                    };
                    egui::ComboBox::from_id_salt("voice_combo")
                        .selected_text(&current)
                        .width(ui.available_width() - 10.0)
                        .show_ui(ui, |ui| {
                            for (i, v) in self.voices.iter().enumerate() {
                                if ui
                                    .selectable_label(i == self.selected_voice_idx, v)
                                    .clicked()
                                {
                                    self.selected_voice_idx = i;
                                    self.settings.voice_name = v.clone();
                                    self.tts.send(TtsCommand::SetVoice(v.clone()));
                                    self.settings.save();
                                }
                            }
                        });
                });

                ui.add_space(2.0);

                // Rate slider
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Rate")
                            .color(egui::Color32::from_rgb(80, 80, 90))
                            .size(12.0),
                    );
                    let slider = ui.add(
                        egui::Slider::new(&mut self.settings.rate, -5..=5)
                            .show_value(true)
                            .text(""),
                    );
                    if slider.changed() {
                        self.tts.send(TtsCommand::SetRate(self.settings.rate));
                        self.settings.save();
                    }
                });

                // Volume slider
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Vol")
                            .color(egui::Color32::from_rgb(80, 80, 90))
                            .size(12.0),
                    );
                    let slider = ui.add(
                        egui::Slider::new(&mut self.settings.volume, 0..=100)
                            .show_value(true)
                            .text(""),
                    );
                    if slider.changed() {
                        self.tts.send(TtsCommand::SetVolume(self.settings.volume));
                        self.settings.save();
                    }
                });
            });

            if self.using_edge_tts() {
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Edge Voice")
                            .color(egui::Color32::from_rgb(80, 80, 90))
                            .size(12.0),
                    );
                    let voice_label = if self.settings.edge_voice.is_empty() {
                        "Loading...".to_string()
                    } else {
                        self.settings.edge_voice.clone()
                    };
                    ui.label(
                        egui::RichText::new(voice_label)
                            .color(egui::Color32::from_rgb(60, 60, 70))
                            .size(12.0),
                    );
                    if ui
                        .button(egui::RichText::new("cocokoishi/TTS-for-silent-player").size(11.0))
                        .clicked()
                    {
                        self.edge_voices_requested = false;
                        self.request_edge_voices_if_needed();
                    }
                });
            } else if matches!(self.settings.tts_mode, TtsMode::OpenaiCompatibleRemote) {
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Remote Voice")
                            .color(egui::Color32::from_rgb(80, 80, 90))
                            .size(12.0),
                    );
                    ui.label(
                        egui::RichText::new(self.settings.remote_voice.clone())
                            .color(egui::Color32::from_rgb(60, 60, 70))
                            .size(12.0),
                    );
                });
            }

            ui.add_space(2.0);

            // Global window opacity
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Opacity")
                        .color(egui::Color32::from_rgb(80, 80, 90))
                        .size(12.0),
                );
                let slider = ui.add(
                    egui::Slider::new(&mut self.settings.window_opacity, 44..=100)
                        .show_value(false)
                        .text(""),
                );
                ui.label(
                    egui::RichText::new(format!("{}%", self.settings.window_opacity))
                        .color(egui::Color32::from_rgb(80, 80, 90))
                        .size(12.0),
                );
                if slider.changed() {
                    self.settings.save();
                }
            });

            ui.add_space(2.0);

            // Always on top toggle
            ui.horizontal(|ui| {
                let cb = ui.checkbox(
                    &mut self.settings.always_on_top,
                    egui::RichText::new("Always on top")
                        .color(egui::Color32::from_rgb(80, 80, 90))
                        .size(12.0),
                );
                if cb.changed() {
                    self.settings.save();
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::WindowLevel(
                            if self.settings.always_on_top {
                                egui::WindowLevel::AlwaysOnTop
                            } else {
                                egui::WindowLevel::Normal
                            },
                        ));
                }

                ui.add_space(8.0);

                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("Clear text")
                                .color(egui::Color32::from_rgb(60, 60, 70))
                                .size(12.0),
                        )
                        .fill(egui::Color32::from_rgb(200, 200, 210))
                        .rounding(egui::Rounding::same(4.0)),
                    )
                    .clicked()
                {
                    self.clear_text_and_stop();
                }

                ui.add_space(8.0);

                let mirror_cb = ui.checkbox(
                    &mut self.settings.play_on_default_speaker,
                    egui::RichText::new("Play on default speaker")
                        .color(egui::Color32::from_rgb(80, 80, 90))
                        .size(12.0),
                );
                if mirror_cb.changed() {
                    self.tts.send(TtsCommand::SetMirrorToDefault(
                        self.should_mirror_to_default_speaker(),
                    ));
                    self.settings.save();
                }
                mirror_cb.on_hover_text(
                    "Also mirror Windows Offline, Edge, and OpenAI-compatible playback to the current Windows default speaker.",
                );
                ui.add_space(8.0);

                let speak_on_enter_cb = ui.checkbox(
                    &mut self.settings.speak_on_enter_only,
                    egui::RichText::new("Speak on Enter")
                        .color(egui::Color32::from_rgb(80, 80, 90))
                        .size(12.0),
                );
                if speak_on_enter_cb.changed() {
                    self.settings.save();
                }
            });

            ui.add_space(2.0);

            ui.horizontal(|ui| {
                let cb = ui.checkbox(
                    &mut self.settings.vrchat_osc_enabled,
                    egui::RichText::new("VRChat OSC")
                        .color(egui::Color32::from_rgb(80, 80, 90))
                        .size(12.0),
                );
                if cb.changed() {
                    self.settings.save();
                }
                ui.add_space(8.0);
                ui.add_enabled_ui(self.settings.vrchat_osc_enabled, |ui| {
                    ui.label(
                        egui::RichText::new("OSC history")
                            .color(egui::Color32::from_rgb(80, 80, 90))
                            .size(12.0),
                    );
                    let slider = ui.add(
                        egui::Slider::new(
                            &mut self.settings.vrchat_osc_history_count,
                            1..=VRCHAT_CHATBOX_MAX_LINES,
                        )
                        .show_value(false)
                        .text(""),
                    );
                    ui.label(
                        egui::RichText::new(format!("{}", self.settings.vrchat_osc_history_count))
                            .color(egui::Color32::from_rgb(80, 80, 90))
                            .size(12.0),
                    );
                    if slider.changed() {
                        self.settings.vrchat_osc_history_count =
                            clamp_history_count(self.settings.vrchat_osc_history_count);
                        self.settings.save();
                    }

                    ui.add_space(8.0);

                    let newline_cb = ui.checkbox(
                        &mut self.settings.vrchat_osc_use_newlines,
                        egui::RichText::new("Use line breaks")
                            .color(egui::Color32::from_rgb(80, 80, 90))
                            .size(12.0),
                    );
                    if newline_cb.changed() {
                        self.settings.save();
                    }
                    newline_cb.on_hover_text(
                        "Off: join OSC history with spaces. On: show each history item on a new line.",
                    );
                });
            });

            ui.add_space(2.0);

            ui.horizontal(|ui| {
                ui.add_enabled_ui(self.settings.vrchat_osc_enabled, |ui| {
                    let reset_cb = ui.checkbox(
                        &mut self.settings.vrchat_osc_reset_after_enabled,
                        egui::RichText::new("Reset OSC history after")
                            .color(egui::Color32::from_rgb(80, 80, 90))
                            .size(12.0),
                    );
                    if reset_cb.changed() {
                        self.settings.save();
                    }
                    reset_cb.on_hover_text(
                        "If the previous OSC message is older than this limit, start the next OSC update from the new message only.",
                    );

                    ui.add_space(8.0);

                    let slider = ui.add_enabled(
                        self.settings.vrchat_osc_reset_after_enabled,
                        egui::Slider::new(
                            &mut self.settings.vrchat_osc_reset_after_seconds,
                            VRCHAT_OSC_RESET_AFTER_SECONDS_MIN
                                ..=VRCHAT_OSC_RESET_AFTER_SECONDS_MAX,
                        )
                        .show_value(false)
                        .text(""),
                    );
                    ui.label(
                        egui::RichText::new(format!(
                            "{}s",
                            Self::clamp_vrchat_osc_reset_after_seconds(
                                self.settings.vrchat_osc_reset_after_seconds,
                            )
                        ))
                        .color(egui::Color32::from_rgb(80, 80, 90))
                        .size(12.0),
                    );
                    if slider.changed() {
                        self.settings.vrchat_osc_reset_after_seconds =
                            Self::clamp_vrchat_osc_reset_after_seconds(
                                self.settings.vrchat_osc_reset_after_seconds,
                            );
                        self.settings.save();
                    }
                });
            });
        });
    }

    fn render_settings_window(&mut self, ctx: &egui::Context) {
        if !self.show_remote_settings || !self.using_online_tts() {
            return;
        }

        let title = if self.using_edge_tts() {
            "Edge TTS Settings"
        } else {
            "OpenAI-Compatible TTS Settings"
        };

        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.vertical(|ui| {
                    if self.using_edge_tts() {
                        ui.horizontal(|ui| {
                            ui.label("Voice:");
                            if self.edge_voices.is_empty() {
                                if ui
                                    .text_edit_singleline(&mut self.settings.edge_voice)
                                    .changed()
                                {
                                    self.settings.save();
                                }
                            } else {
                                let current =
                                    if self.selected_edge_voice_idx < self.edge_voices.len() {
                                        self.edge_voices[self.selected_edge_voice_idx].clone()
                                    } else {
                                        self.settings.edge_voice.clone()
                                    };
                                egui::ComboBox::from_id_salt("edge_voice_combo")
                                    .selected_text(current)
                                    .width(260.0)
                                    .show_ui(ui, |ui| {
                                        for (i, voice) in self.edge_voices.iter().enumerate() {
                                            if ui
                                                .selectable_label(
                                                    i == self.selected_edge_voice_idx,
                                                    voice,
                                                )
                                                .clicked()
                                            {
                                                self.selected_edge_voice_idx = i;
                                                self.settings.edge_voice = voice.clone();
                                                self.settings.save();
                                            }
                                        }
                                    });
                            }

                            if ui.button("cocokoishi/TTS-for-silent-player").clicked() {
                                self.edge_voices_requested = false;
                                self.request_edge_voices_if_needed();
                            }
                        });

                        ui.label(format!("Rate: {}%", self.settings.edge_rate));
                        if ui
                            .add(egui::Slider::new(&mut self.settings.edge_rate, -100..=100))
                            .changed()
                        {
                            self.settings.save();
                        }

                        ui.label(format!("Volume: {}%", self.settings.edge_volume));
                        if ui
                            .add(egui::Slider::new(
                                &mut self.settings.edge_volume,
                                -100..=100,
                            ))
                            .changed()
                        {
                            self.settings.save();
                        }

                        ui.label(format!("Pitch: {}Hz", self.settings.edge_pitch));
                        if ui
                            .add(egui::Slider::new(&mut self.settings.edge_pitch, -100..=100))
                            .changed()
                        {
                            self.settings.save();
                        }
                    } else {
                        ui.label("API Endpoint:");
                        if ui
                            .text_edit_singleline(&mut self.settings.remote_api_url)
                            .changed()
                        {
                            self.settings.save();
                        }

                        ui.label("API Key:");
                        if ui
                            .add(
                                egui::TextEdit::singleline(&mut self.settings.remote_api_key)
                                    .password(true),
                            )
                            .changed()
                        {
                            self.settings.save();
                        }

                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.label("Model:");
                                if ui
                                    .text_edit_singleline(&mut self.settings.remote_model)
                                    .changed()
                                {
                                    self.settings.save();
                                }
                            });
                            ui.vertical(|ui| {
                                ui.label("Voice:");
                                if ui
                                    .text_edit_singleline(&mut self.settings.remote_voice)
                                    .changed()
                                {
                                    self.settings.save();
                                }
                            });
                        });

                        ui.label(format!("Speed: {:.2}", self.settings.remote_speed));
                        if ui
                            .add(egui::Slider::new(
                                &mut self.settings.remote_speed,
                                0.25..=4.0,
                            ))
                            .changed()
                        {
                            self.settings.save();
                        }
                    }

                    ui.add_space(8.0);
                    if ui.button("Close").clicked() {
                        self.show_remote_settings = false;
                    }
                });
            });
    }

    fn render_vbcable_notice_window(&mut self, ctx: &egui::Context) {
        if !self.show_vbcable_notice {
            return;
        }

        let (title, lead, step_1, step_2, step_3, note, close_text) = if self.chinese_ui_locale {
            (
                "未检测到 VB-CABLE",
                "首次启动时没有检测到 VB-CABLE 音频设备。如果你想把语音送进 VRChat，请先安装驱动：",
                "1. 下载驱动压缩包：",
                "2. 解压下载好的 zip 压缩包。",
                "3. 运行 VBCABLE_Setup_x64.exe，建议使用管理员权限安装。",
                "安装完成后，重新启动本程序，输出设备列表中就会出现 CABLE 设备。",
                "我知道了",
            )
        } else {
            (
                "VB-CABLE Not Detected",
                "On first launch, no VB-CABLE audio device was found. To route TTS into VRChat, please install the driver:",
                "1. Download the driver package:",
                "2. Extract the downloaded zip archive.",
                "3. Run VBCABLE_Setup_x64.exe (recommended: Run as Administrator).",
                "After installation, restart this app so the CABLE device appears in the output list.",
                "Close",
            )
        };

        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .default_width(500.0)
            .min_width(500.0)
            .max_width(500.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.set_width(500.0);
                ui.add(egui::Label::new(lead).wrap());
                ui.add_space(6.0);
                ui.add(egui::Label::new(step_1).wrap());
                ui.hyperlink_to(VB_CABLE_DOWNLOAD_URL, VB_CABLE_DOWNLOAD_URL);
                ui.add_space(6.0);
                ui.add(egui::Label::new(step_2).wrap());
                ui.add(egui::Label::new(step_3).wrap());
                ui.add_space(8.0);
                ui.add(egui::Label::new(note).wrap());
                ui.add_space(10.0);
                if ui.button(close_text).clicked() {
                    self.close_vbcable_notice();
                }
            });
    }

    fn render_quick_start_guide_window(&mut self, ctx: &egui::Context) {
        if !self.show_quick_start_guide {
            return;
        }

        let (title, step_1, step_2, step_3, step_4, step_5, close_text) =
            if self.chinese_ui_locale {
                (
                    "快速使用指南",
                    "1.在使用前，请确保你的电脑已经安装VB-CABLE等软件。如果没有安装，会在第一次打开就提醒。安装完毕后重启程序会自动使用VB-CABLE的声音通道。",
                    "2.在游戏中，将输入麦克风设备改成CABLE Output，即可开始使用本软件",
                    "3.本软件内置3种语音模式，第一种是win自带的，离线可用。第二种是默认的Edge-TTS，需要联网。第三种是第三方语音大模型。",
                    "4.当您在游戏中游玩的时候，可以按下右Shift键快速切回本软件。设置里面也可以打开OSC来支持VRChat游戏",
                    "5.更多支持可以联系",
                    "关闭",
                )
            } else {
                (
                    "Quick Start Guide",
                    "1. Before using this app, please make sure VB-CABLE or a similar tool is installed on your PC. If it is not installed, the app will remind you the first time you open it. After installation, restart the app and it will automatically use the VB-CABLE audio channel.",
                    "2. In the game, change the microphone/input device to CABLE Output, and you can start using this software.",
                    "3. This software includes 3 voice modes. The first is the built-in Windows voice, which works offline. The second is the default Edge-TTS, which requires an internet connection. The third is a third-party large voice model.",
                    "4. While playing, you can press the Right Shift key to quickly switch back to this software. You can also enable OSC in Settings to support VRChat.",
                    "5. For more support, please visit",
                    "Close",
                )
            };

        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .default_width(520.0)
            .min_width(520.0)
            .max_width(520.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.set_width(520.0);
                ui.add(egui::Label::new(step_1).wrap());
                ui.add_space(6.0);
                ui.add(egui::Label::new(step_2).wrap());
                ui.add_space(6.0);
                ui.add(egui::Label::new(step_3).wrap());
                ui.add_space(6.0);
                ui.add(egui::Label::new(step_4).wrap());
                ui.add_space(6.0);
                ui.add(egui::Label::new(step_5).wrap());
                ui.hyperlink_to(
                    "https://space.bilibili.com/5145514",
                    "https://space.bilibili.com/5145514",
                );
                ui.add_space(10.0);
                if ui.button(close_text).clicked() {
                    self.close_quick_start_guide();
                }
            });
    }
}

