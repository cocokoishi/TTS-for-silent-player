use rodio::{Decoder, OutputStream, Sink};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteSettings {
    pub api_url: String,
    pub api_key: String,
    pub model: String,
    pub voice: String,
    pub speed: f32,
    pub output_device: String,
}

pub enum RemoteTtsCommand {
    Speak(String, RemoteSettings),
    Stop,
}

#[derive(Debug, Clone)]
pub enum RemoteTtsEvent {
    PlaybackFinished,
    SpeakFailed {
        message: String,
        consecutive_failures: u32,
        sticky_error: bool,
    },
    ConnectionRecovered,
}

pub struct RemoteTts {
    cmd_tx: mpsc::Sender<RemoteTtsCommand>,
    event_rx: mpsc::Receiver<RemoteTtsEvent>,
}

impl RemoteTts {
    pub fn spawn() -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel::<RemoteTtsCommand>();
        let (event_tx, event_rx) = mpsc::channel::<RemoteTtsEvent>();

        thread::spawn(move || {
            let mut current_device_name = String::new();
            let mut stream_info: Option<(OutputStream, rodio::OutputStreamHandle)> = None;
            let mut sink: Option<Sink> = None;
            let mut is_playing = false;
            let mut consecutive_failures = 0u32;

            let client = reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap_or_else(|_| reqwest::blocking::Client::new());

            loop {
                match cmd_rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(RemoteTtsCommand::Speak(text, settings)) => {
                        if settings.output_device != current_device_name || stream_info.is_none() {
                            current_device_name = settings.output_device.clone();
                            sink = None;

                            use rodio::cpal::traits::{DeviceTrait, HostTrait};
                            let host = rodio::cpal::default_host();
                            let mut target_device = None;

                            if !current_device_name.is_empty() {
                                if let Ok(mut devices) = host.output_devices() {
                                    target_device = devices.find(|d| {
                                        d.name().unwrap_or_default().contains(&current_device_name)
                                    });
                                }
                            }

                            stream_info = target_device
                                .and_then(|d| OutputStream::try_from_device(&d).ok())
                                .or_else(|| {
                                    eprintln!("[RemoteTTS] Falling back to default output device");
                                    OutputStream::try_default().ok()
                                });
                        }

                        let url = Self::build_url(&settings.api_url);
                        let body = serde_json::json!({
                            "model": settings.model,
                            "input": text,
                            "voice": settings.voice,
                            "speed": settings.speed,
                        });

                        let api_key = if settings.api_key.trim().is_empty() {
                            "none".to_string()
                        } else {
                            settings.api_key.clone()
                        };

                        eprintln!("[RemoteTTS] POST {url} voice={}", settings.voice);

                        let response = client
                            .post(&url)
                            .header("Authorization", format!("Bearer {api_key}"))
                            .header("Content-Type", "application/json")
                            .header("Accept", "audio/mpeg, audio/wav, application/octet-stream")
                            .json(&body)
                            .send();

                        match response {
                            Ok(res) => {
                                let status = res.status();
                                if !status.is_success() {
                                    let err_body = res.text().unwrap_or_default();
                                    let message = format!("Server returned {status}: {err_body}");
                                    Self::emit_failure(
                                        &event_tx,
                                        &mut consecutive_failures,
                                        message,
                                    );
                                    continue;
                                }

                                match Self::read_audio_response_streaming(res) {
                                    Ok(bytes) => {
                                        if bytes.is_empty() {
                                            Self::emit_failure(
                                                &event_tx,
                                                &mut consecutive_failures,
                                                "Server returned empty audio".to_string(),
                                            );
                                            continue;
                                        }

                                        let cursor = std::io::Cursor::new(bytes);
                                        match Decoder::new(cursor) {
                                            Ok(source) => {
                                                if sink.is_none() {
                                                    if let Some((_, handle)) = &stream_info {
                                                        match Sink::try_new(handle) {
                                                            Ok(new_sink) => sink = Some(new_sink),
                                                            Err(e) => {
                                                                Self::emit_failure(
                                                                    &event_tx,
                                                                    &mut consecutive_failures,
                                                                    format!("Failed to create sink: {e}"),
                                                                );
                                                                continue;
                                                            }
                                                        }
                                                    } else {
                                                        Self::emit_failure(
                                                            &event_tx,
                                                            &mut consecutive_failures,
                                                            "No audio stream available".to_string(),
                                                        );
                                                        continue;
                                                    }
                                                }

                                                if let Some(active_sink) = &sink {
                                                    active_sink.append(source);
                                                    is_playing = true;
                                                    eprintln!("[RemoteTTS] Queued audio");
                                                    if consecutive_failures >= 3 {
                                                        let _ =
                                                            event_tx.send(RemoteTtsEvent::ConnectionRecovered);
                                                    }
                                                    consecutive_failures = 0;
                                                }
                                            }
                                            Err(e) => {
                                                Self::emit_failure(
                                                    &event_tx,
                                                    &mut consecutive_failures,
                                                    format!("Failed to decode audio: {e}"),
                                                );
                                            }
                                        }
                                    }
                                    Err(message) => {
                                        Self::emit_failure(
                                            &event_tx,
                                            &mut consecutive_failures,
                                            message,
                                        );
                                    }
                                }
                            }
                            Err(e) => {
                                Self::emit_failure(
                                    &event_tx,
                                    &mut consecutive_failures,
                                    format!("Request failed: {e}"),
                                );
                            }
                        }
                    }
                    Ok(RemoteTtsCommand::Stop) => {
                        if let Some(old_sink) = sink.take() {
                            old_sink.stop();
                        }
                        is_playing = false;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }

                if is_playing && sink.as_ref().map(|s| s.empty()).unwrap_or(false) {
                    is_playing = false;
                    let _ = event_tx.send(RemoteTtsEvent::PlaybackFinished);
                }
            }
        });

        Self { cmd_tx, event_rx }
    }

    pub fn send(&self, cmd: RemoteTtsCommand) {
        let _ = self.cmd_tx.send(cmd);
    }

    pub fn poll_events(&self) -> Vec<RemoteTtsEvent> {
        let mut events = Vec::new();
        while let Ok(ev) = self.event_rx.try_recv() {
            events.push(ev);
        }
        events
    }

    fn build_url(api_url: &str) -> String {
        let base = api_url.trim_end_matches('/');
        if base.ends_with("/v1/audio/speech") {
            base.to_string()
        } else if base.ends_with("/v1") {
            format!("{base}/audio/speech")
        } else {
            format!("{base}/v1/audio/speech")
        }
    }

    fn emit_failure(
        event_tx: &mpsc::Sender<RemoteTtsEvent>,
        consecutive_failures: &mut u32,
        message: String,
    ) {
        eprintln!("[RemoteTTS] {message}");
        *consecutive_failures += 1;
        let _ = event_tx.send(RemoteTtsEvent::SpeakFailed {
            message,
            consecutive_failures: *consecutive_failures,
            sticky_error: *consecutive_failures >= 3,
        });
    }

    fn read_audio_response_streaming(
        mut response: reqwest::blocking::Response,
    ) -> Result<Vec<u8>, String> {
        let is_chunked = response
            .headers()
            .get(reqwest::header::TRANSFER_ENCODING)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.to_ascii_lowercase().contains("chunked"))
            .unwrap_or(false);
        let content_length = response.content_length();

        eprintln!(
            "[RemoteTTS] Reading audio response (chunked={}, content_length={:?})",
            is_chunked,
            content_length
        );

        let mut bytes = Vec::with_capacity(content_length.unwrap_or(0) as usize);
        let mut buf = [0u8; 8192];

        loop {
            match response.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => bytes.extend_from_slice(&buf[..n]),
                Err(e) => return Err(format!("Failed while streaming response body: {e}")),
            }
        }

        Ok(bytes)
    }
}
