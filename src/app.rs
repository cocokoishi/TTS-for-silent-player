use crate::settings::Settings;
use crate::tts_bridge::{TtsBridge, TtsCommand, TtsEvent};
use crate::remote_tts::{RemoteTts, RemoteTtsCommand, RemoteTtsEvent, RemoteSettings};
use eframe::egui;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

pub struct MugenTtsApp {
    text: String,
    read_end: usize,      // fully read (blue) boundary in bytes
    reading_end: usize,    // currently reading (red) boundary in bytes
    pending_trigger_end: Option<usize>,
    is_speaking: bool,
    tts: TtsBridge,
    remote_tts: RemoteTts,
    settings: Settings,
    show_settings: bool,
    show_remote_settings: bool,
    voices: Vec<String>,
    devices: Vec<String>,
    selected_voice_idx: usize,
    selected_device_idx: usize,
    focus_flag: Arc<AtomicBool>,
    last_status_poll: Instant,
    speak_start_time: Instant,
    initialized: bool,
    pending_list: u8, // bit 0 = voices, bit 1 = devices
    scroll_to_bottom: bool,
    ime_composing: bool,
    show_remote_error_notice: bool,
}

impl MugenTtsApp {
    pub fn new(focus_flag: Arc<AtomicBool>) -> Self {
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
            devices: Vec::new(),
            selected_voice_idx: 0,
            selected_device_idx: 0,
            focus_flag,
            last_status_poll: Instant::now(),
            speak_start_time: Instant::now(),
            initialized: false,
            pending_list: 0,
            scroll_to_bottom: false,
            ime_composing: false,
            show_remote_error_notice: false,
        }
    }

    fn apply_settings(&self) {
        if !self.settings.voice_name.is_empty() {
            self.tts.send(TtsCommand::SetVoice(self.settings.voice_name.clone()));
        }
        if !self.settings.output_device.is_empty() {
            self.tts.send(TtsCommand::SetDevice(self.settings.output_device.clone()));
        }
        self.tts.send(TtsCommand::SetRate(self.settings.rate));
        self.tts.send(TtsCommand::SetVolume(self.settings.volume));
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
        let (safe_read_end, _) = Self::get_safe_boundaries(&self.text, self.read_end, self.read_end);
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

        if self.settings.use_remote_tts {
            let remote_settings = RemoteSettings {
                api_url: self.settings.remote_api_url.clone(),
                api_key: self.settings.remote_api_key.clone(),
                model: self.settings.remote_model.clone(),
                voice: self.settings.remote_voice.clone(),
                speed: self.settings.remote_speed,
                output_device: self.settings.output_device.clone(),
            };
            self.remote_tts.send(RemoteTtsCommand::Speak(chunk, remote_settings));
            
            // For remote TTS, we don't have a direct status poll like SAPI, 
            // so we'll just roughly estimate completion or rely on manual reset.
            // But we can reset is_speaking after a short delay or if new text comes.
            // For now, let's keep is_speaking true until next trigger or manual reset.
            // A better way would be to have RemoteTts feedback, but that needs more async work.
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

    fn is_trigger_char(c: char) -> bool {
        c == '\n' || c == ',' || c == '.' || c == '!' || c == '?'
            || c == ';' || c == ':' || c == '、' || c == '。' || c == '！'
            || c == '？' || c == '，' || c == '；' || c == '：' || c == '…'
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
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
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
                        if let Some(cn_voice) = self.voices.iter().find(|x| x.contains("Chinese") || x.contains("Han") || x.contains("Huihui") || x.contains("Yaoyao") || x.contains("Kangkang")) {
                            self.settings.voice_name = cn_voice.clone();
                            self.tts.send(TtsCommand::SetVoice(cn_voice.clone()));
                        } else if let Some(first) = self.voices.first() {
                            self.settings.voice_name = first.clone();
                            self.tts.send(TtsCommand::SetVoice(first.clone()));
                        }
                        self.settings.save();
                    }
                    // Find selected index
                    self.selected_voice_idx = self.voices
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
                        if let Some(cable) = self.devices.iter().find(|x| x.to_lowercase().contains("cable")) {
                            self.settings.output_device = cable.clone();
                            self.tts.send(TtsCommand::SetDevice(cable.clone()));
                            self.settings.save();
                        }
                    }
                    self.selected_device_idx = self.devices
                        .iter()
                        .position(|n| n.contains(&self.settings.output_device))
                        .unwrap_or(0);
                    self.pending_list = 0;
                    self.initialized = true;
                }

                TtsEvent::SpeakingState(speaking) => {
                    // Only care about speaking state if NOT using remote TTS
                    if !self.settings.use_remote_tts && !speaking && self.is_speaking {
                        self.finish_current_speech();
                    }
                }
                TtsEvent::Error(_e) => {
                    if !self.settings.use_remote_tts {
                        self.fail_current_speech();
                    }
                }
            }
        }

        for event in self.remote_tts.poll_events() {
            match event {
                RemoteTtsEvent::PlaybackFinished => {
                    if self.settings.use_remote_tts {
                        self.finish_current_speech();
                    }
                }
                RemoteTtsEvent::SpeakFailed {
                    message: _message,
                    consecutive_failures: _consecutive_failures,
                    sticky_error,
                } => {
                    if sticky_error {
                        self.show_remote_error_notice = true;
                    }
                    if self.settings.use_remote_tts {
                        self.fail_current_speech();
                    }
                }
                RemoteTtsEvent::ConnectionRecovered => {
                    self.show_remote_error_notice = false;
                }
            }
        }

        // Poll speaking status periodically (wait at least 400ms after starting to speak before polling to avoid queueing race condition)
        if !self.settings.use_remote_tts && self.is_speaking && self.speak_start_time.elapsed().as_millis() > 400 && self.last_status_poll.elapsed().as_millis() > 100 {
            self.tts.send(TtsCommand::QueryStatus);
            self.last_status_poll = Instant::now();
        }

        // Check focus flag (global hotkey)
        if self.focus_flag.swap(false, Ordering::Relaxed) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }

        // Request continuous repaints for status polling
        ctx.request_repaint_after(std::time::Duration::from_millis(50));

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
        let mut enter_pressed = false;

        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            // Allow dragging the window from any unoccupied space in CentralPanel
            let drag_response = ui.interact(ui.max_rect(), ui.id().with("bg_drag"), egui::Sense::drag());
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

                let response = ui.add(text_edit);
                // Only treat Enter as a TTS trigger when NOT in IME composition
                // and NOT immediately after an IME commit (which also fires Enter)
                if response.has_focus()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter))
                    && !self.ime_composing
                    && !ime_committed_this_frame
                {
                    enter_pressed = true;
                }
            });

        });

        // Floating buttons overlaid in top right (70% transparent)
        egui::Area::new(egui::Id::new("floating_btns"))
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-10.0, 10.0))
            .interactable(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let reset_btn = ui.add(
                        egui::Button::new(
                            egui::RichText::new("⏹ Reset")
                                .size(13.0)
                                .color(egui::Color32::from_rgba_unmultiplied(200, 80, 80, 180)),
                        )
                        .fill(egui::Color32::from_rgba_unmultiplied(230, 230, 235, 77))
                        .rounding(egui::Rounding::same(4.0)),
                    ).on_hover_text("Stop speaking and clear text");

                    if reset_btn.clicked() {
                        self.text.clear();
                        self.read_end = 0;
                        self.reading_end = 0;
                        self.pending_trigger_end = None;
                        self.tts.send(TtsCommand::Stop);
                        self.remote_tts.send(RemoteTtsCommand::Stop);
                        self.is_speaking = false;
                    }

                    ui.add_space(4.0);

                    let settings_btn = ui.add(
                        egui::Button::new(
                            egui::RichText::new("⚙")
                                .size(16.0)
                                .color(if self.show_settings {
                                    egui::Color32::from_rgba_unmultiplied(80, 90, 200, 180)
                                } else {
                                    egui::Color32::from_rgba_unmultiplied(100, 100, 110, 180)
                                })
                        )
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
                                egui::RichText::new("remote server error")
                                    .color(egui::Color32::from_rgb(255, 245, 245))
                                    .size(13.0),
                            );
                        });
                });
        }

        // If IME just committed, the Enter key may have also inserted a spurious
        // newline into the multiline TextEdit. Strip it so the text stays on one line.
        if ime_committed_this_frame && self.text.ends_with('\n') && !old_text.ends_with('\n') {
            self.text.pop(); // remove the trailing '\n' inserted by Enter
        }

        // Handle text detection AFTER the UI has been drawn (no borrow conflict)
        if self.text != old_text || enter_pressed {
            self.scroll_to_bottom = true;
            if self.text != old_text {
                let cpl = Self::get_common_prefix_len(&old_text, &self.text);
                self.read_end = self.read_end.min(cpl);
                self.reading_end = self.reading_end.min(cpl);
                self.pending_trigger_end = self.pending_trigger_end.map(|pending| pending.min(cpl));
            }

            let mut trigger_idx = None;
            let (_, rge) = Self::get_safe_boundaries(&self.text, self.read_end, self.reading_end);

            if enter_pressed {
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
    }
}

impl MugenTtsApp {
    fn render_settings(&mut self, ui: &mut egui::Ui, panel_bg: egui::Color32, _accent: egui::Color32) {
        let settings_frame = egui::Frame::default()
            .fill(panel_bg)
            .rounding(egui::Rounding::same(8.0))
            .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(200, 200, 210)))
            .inner_margin(egui::Margin::same(12.0));

        settings_frame.show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Settings")
                        .color(egui::Color32::from_rgb(40, 40, 50))
                        .size(13.0)
                        .strong(),
                );
            });
            ui.add_space(4.0);

            ui.add_space(2.0);
            
            // Remote TTS toggle
            ui.horizontal(|ui| {
                let cb = ui.checkbox(&mut self.settings.use_remote_tts, egui::RichText::new("Remote TTS (OpenAI)").color(egui::Color32::from_rgb(80, 80, 90)).size(12.0));
                if cb.changed() {
                    self.settings.save();
                    self.pending_trigger_end = None;
                    self.reading_end = self.read_end;
                    self.is_speaking = false;
                    if self.settings.use_remote_tts {
                        self.tts.send(TtsCommand::Stop);
                    } else {
                        self.remote_tts.send(RemoteTtsCommand::Stop);
                    }
                }
                
                if self.settings.use_remote_tts {
                    if ui.button(egui::RichText::new("⚙ Configure Remote").size(11.0)).clicked() {
                        self.show_remote_settings = true;
                    }
                }
            });

            ui.add_space(2.0);

            // Windows SAPI basic settings (only if local)
            ui.add_enabled_ui(!self.settings.use_remote_tts, |ui| {
                // Voice selection
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Voice").color(egui::Color32::from_rgb(80, 80, 90)).size(12.0));
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
                                if ui.selectable_label(i == self.selected_voice_idx, v).clicked() {
                                    self.selected_voice_idx = i;
                                    self.settings.voice_name = v.clone();
                                    self.tts.send(TtsCommand::SetVoice(v.clone()));
                                    self.settings.save();
                                }
                            }
                        });
                });

                ui.add_space(2.0);

                // Output device selection
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Output").color(egui::Color32::from_rgb(80, 80, 90)).size(12.0));
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
                                if ui.selectable_label(i == self.selected_device_idx, d).clicked() {
                                    self.selected_device_idx = i;
                                    self.settings.output_device = d.clone();
                                    self.tts.send(TtsCommand::SetDevice(d.clone()));
                                    self.settings.save();
                                }
                            }
                        });
                });

                ui.add_space(2.0);

                // Rate slider
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Rate").color(egui::Color32::from_rgb(80, 80, 90)).size(12.0));
                    let slider = ui.add(
                        egui::Slider::new(&mut self.settings.rate, -5..=5)
                            .show_value(true)
                            .text("")
                    );
                    if slider.changed() {
                        self.tts.send(TtsCommand::SetRate(self.settings.rate));
                        self.settings.save();
                    }
                });

                // Volume slider
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Vol").color(egui::Color32::from_rgb(80, 80, 90)).size(12.0));
                    let slider = ui.add(
                        egui::Slider::new(&mut self.settings.volume, 0..=100)
                            .show_value(true)
                            .text("")
                    );
                    if slider.changed() {
                        self.tts.send(TtsCommand::SetVolume(self.settings.volume));
                        self.settings.save();
                    }
                });
            });

            ui.add_space(2.0);

            // Always on top toggle
            ui.horizontal(|ui| {
                let cb = ui.checkbox(
                    &mut self.settings.always_on_top,
                    egui::RichText::new("Always on top").color(egui::Color32::from_rgb(80, 80, 90)).size(12.0)
                );
                if cb.changed() {
                    self.settings.save();
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::WindowLevel(
                        if self.settings.always_on_top {
                            egui::WindowLevel::AlwaysOnTop
                        } else {
                            egui::WindowLevel::Normal
                        }
                    ));
                }
            });

            ui.add_space(2.0);

            // Speak on enter only toggle
            ui.horizontal(|ui| {
                let cb = ui.checkbox(
                    &mut self.settings.speak_on_enter_only,
                    egui::RichText::new("Speak on Enter only").color(egui::Color32::from_rgb(80, 80, 90)).size(12.0)
                );
                if cb.changed() {
                    self.settings.save();
                }
            });

            ui.add_space(2.0);

            // Clear text button
            ui.horizontal(|ui| {
                if ui.add(
                    egui::Button::new(
                        egui::RichText::new("Clear text")
                            .color(egui::Color32::from_rgb(60, 60, 70))
                            .size(12.0),
                    )
                    .fill(egui::Color32::from_rgb(200, 200, 210))
                    .rounding(egui::Rounding::same(4.0))
                ).clicked() {
                    self.text.clear();
                    self.read_end = 0;
                    self.reading_end = 0;
                    self.pending_trigger_end = None;
                    self.tts.send(TtsCommand::Stop);
                    self.remote_tts.send(RemoteTtsCommand::Stop);
                    self.is_speaking = false;
                }
            });
        });
    }

    fn render_settings_window(&mut self, ctx: &egui::Context) {
        if self.show_remote_settings {
            egui::Window::new("Remote TTS Settings")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    ui.vertical(|ui| {
                        ui.label("API Endpoint:");
                        if ui.text_edit_singleline(&mut self.settings.remote_api_url).changed() {
                            self.settings.save();
                        }
                        
                        ui.label("API Key:");
                        if ui.add(egui::TextEdit::singleline(&mut self.settings.remote_api_key).password(true)).changed() {
                            self.settings.save();
                        }

                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.label("Model:");
                                if ui.text_edit_singleline(&mut self.settings.remote_model).changed() {
                                    self.settings.save();
                                }
                            });
                            ui.vertical(|ui| {
                                ui.label("Voice:");
                                if ui.text_edit_singleline(&mut self.settings.remote_voice).changed() {
                                    self.settings.save();
                                }
                            });
                        });

                        ui.label(format!("Speed: {:.2}", self.settings.remote_speed));
                        if ui.add(egui::Slider::new(&mut self.settings.remote_speed, 0.25..=4.0)).changed() {
                            self.settings.save();
                        }

                        ui.add_space(8.0);
                        if ui.button("Close").clicked() {
                            self.show_remote_settings = false;
                        }
                    });
                });
        }
    }
}
