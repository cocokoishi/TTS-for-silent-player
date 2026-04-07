#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod settings;
mod tts_bridge;

use app::MugenTtsApp;
use eframe::egui;
use settings::Settings;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

fn main() -> eframe::Result<()> {
    let focus_flag = Arc::new(AtomicBool::new(false));
    let focus_flag_clone = focus_flag.clone();

    // Global hotkey thread: monitors Shift key for window focus
    thread::spawn(move || {
        hotkey_listener(focus_flag_clone);
    });

    let settings = Settings::load();

    // Load CJK font
    let mut fonts = egui::FontDefinitions::default();

    // Try to load Microsoft YaHei from system fonts
    let font_paths = [
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\msyhbd.ttc",
        r"C:\Windows\Fonts\simsun.ttc",
        r"C:\Windows\Fonts\simhei.ttf",
    ];

    for path in &font_paths {
        if let Ok(font_data) = std::fs::read(path) {
            fonts.font_data.insert(
                "cjk_font".to_string(),
                egui::FontData::from_owned(font_data),
            );
            // Add CJK font as fallback for proportional
            fonts
                .families
                .entry(egui::FontFamily::Proportional)
                .or_default()
                .push("cjk_font".to_string());
            // Also for monospace
            fonts
                .families
                .entry(egui::FontFamily::Monospace)
                .or_default()
                .push("cjk_font".to_string());
            break;
        }
    }

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([520.0, 260.0])
        .with_min_inner_size([300.0, 150.0])
        .with_title("Mugen TTS")
        .with_decorations(true);
        
    if settings.always_on_top {
        viewport = viewport.with_always_on_top();
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "Mugen TTS",
        options,
        Box::new(move |cc| {
            // Set fonts with CJK support
            cc.egui_ctx.set_fonts(fonts);

            // Light theme to match the text input area
            let mut visuals = egui::Visuals::light();
            let bg_color = egui::Color32::from_rgb(240, 240, 245);
            visuals.window_fill = bg_color;
            visuals.panel_fill = bg_color;
            cc.egui_ctx.set_visuals(visuals);

            Ok(Box::new(MugenTtsApp::new(focus_flag)))
        }),
    )
}

/// Monitors the Shift key globally and sets the focus flag
fn hotkey_listener(focus_flag: Arc<AtomicBool>) {
    #[cfg(windows)]
    {
        use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

        let mut was_pressed = false;
        let mut press_start = std::time::Instant::now();

        loop {
            let is_pressed = unsafe { GetAsyncKeyState(0xA1) } & (0x8000u16 as i16) != 0; // VK_RSHIFT

            if is_pressed && !was_pressed {
                press_start = std::time::Instant::now();
            }

            // Trigger on release of a short press (< 300ms, no other keys)
            if !is_pressed && was_pressed {
                let duration = press_start.elapsed();
                if duration < Duration::from_millis(300) {
                    focus_flag.store(true, Ordering::Relaxed);
                }
            }

            was_pressed = is_pressed;
            thread::sleep(Duration::from_millis(30));
        }
    }

    #[cfg(not(windows))]
    {
        // No-op on non-Windows
        loop {
            thread::sleep(Duration::from_secs(60));
        }
    }
}
