use chrono::Utc;
use reqwest::blocking::{Client, Response};
use rodio::{Decoder, OutputStream, Sink};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Read;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tungstenite::client::IntoClientRequest;
use tungstenite::http::Request;
use tungstenite::{connect, Error as WsError, Message as WsMessage};
use uuid::Uuid;

const EDGE_TRUSTED_CLIENT_TOKEN: &str = "6A5AA1D4EAFF4E9FB37E23D68491D6F4";
const EDGE_WSS_URL: &str =
    "wss://speech.platform.bing.com/consumer/speech/synthesize/readaloud/edge/v1";
const EDGE_VOICE_LIST_URL: &str =
    "https://speech.platform.bing.com/consumer/speech/synthesize/readaloud/voices/list";
const EDGE_CHROMIUM_MAJOR_VERSION: &str = "143";
const EDGE_SEC_MS_GEC_VERSION: &str = "1-143.0.3650.75";
const EDGE_OUTPUT_FORMAT: &str = "audio-24khz-48kbitrate-mono-mp3";
const WIN_EPOCH_SECONDS: u64 = 11_644_473_600;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RemoteBackend {
    Edge,
    OpenAiCompatible,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteSettings {
    pub backend: RemoteBackend,
    pub output_device: String,
    pub api_url: String,
    pub api_key: String,
    pub model: String,
    pub voice: String,
    pub speed: f32,
    pub edge_voice: String,
    pub edge_rate: i32,
    pub edge_volume: i32,
    pub edge_pitch: i32,
}

pub enum RemoteTtsCommand {
    Speak(String, RemoteSettings),
    Stop,
    ListEdgeVoices,
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
    EdgeVoices(Vec<String>),
    EdgeVoicesFailed(String),
}

pub struct RemoteTts {
    cmd_tx: mpsc::Sender<RemoteTtsCommand>,
    event_rx: mpsc::Receiver<RemoteTtsEvent>,
}

#[derive(Debug, Deserialize)]
struct EdgeVoiceEntry {
    #[serde(rename = "ShortName")]
    short_name: String,
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
            let mut edge_clock_skew_seconds = 0i64;

            let client = Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap_or_else(|_| Client::new());

            loop {
                match cmd_rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(RemoteTtsCommand::Speak(text, settings)) => {
                        if settings.output_device != current_device_name || stream_info.is_none() {
                            current_device_name = settings.output_device.clone();
                            sink = None;
                            stream_info = Self::open_output_stream(&current_device_name);
                        }

                        let audio_result = match settings.backend {
                            RemoteBackend::OpenAiCompatible => {
                                Self::synthesize_openai_compatible(&client, &text, &settings)
                            }
                            RemoteBackend::Edge => Self::synthesize_edge(
                                &text,
                                &settings,
                                &mut edge_clock_skew_seconds,
                            ),
                        };

                        Self::handle_speak_result(
                            audio_result,
                            &stream_info,
                            &mut sink,
                            &mut is_playing,
                            &event_tx,
                            &mut consecutive_failures,
                        );
                    }
                    Ok(RemoteTtsCommand::Stop) => {
                        if let Some(old_sink) = sink.take() {
                            old_sink.stop();
                        }
                        is_playing = false;
                    }
                    Ok(RemoteTtsCommand::ListEdgeVoices) => {
                        match Self::list_edge_voices(&client, &mut edge_clock_skew_seconds) {
                            Ok(voices) => {
                                let _ = event_tx.send(RemoteTtsEvent::EdgeVoices(voices));
                            }
                            Err(message) => {
                                let _ = event_tx.send(RemoteTtsEvent::EdgeVoicesFailed(message));
                            }
                        }
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

    fn open_output_stream(device_name: &str) -> Option<(OutputStream, rodio::OutputStreamHandle)> {
        use rodio::cpal::traits::{DeviceTrait, HostTrait};

        let host = rodio::cpal::default_host();
        let mut target_device = None;

        if !device_name.is_empty() {
            if let Ok(mut devices) = host.output_devices() {
                target_device =
                    devices.find(|d| d.name().unwrap_or_default().contains(device_name));
            }
        }

        target_device
            .and_then(|d| OutputStream::try_from_device(&d).ok())
            .or_else(|| {
                eprintln!("[RemoteTTS] Falling back to default output device");
                OutputStream::try_default().ok()
            })
    }

    fn handle_speak_result(
        audio_result: Result<Vec<u8>, String>,
        stream_info: &Option<(OutputStream, rodio::OutputStreamHandle)>,
        sink: &mut Option<Sink>,
        is_playing: &mut bool,
        event_tx: &mpsc::Sender<RemoteTtsEvent>,
        consecutive_failures: &mut u32,
    ) {
        match audio_result {
            Ok(bytes) => {
                if bytes.is_empty() {
                    Self::emit_failure(
                        event_tx,
                        consecutive_failures,
                        "Server returned empty audio".to_string(),
                    );
                    return;
                }

                let cursor = std::io::Cursor::new(bytes);
                match Decoder::new(cursor) {
                    Ok(source) => {
                        if sink.is_none() {
                            if let Some((_, handle)) = stream_info {
                                match Sink::try_new(handle) {
                                    Ok(new_sink) => *sink = Some(new_sink),
                                    Err(e) => {
                                        Self::emit_failure(
                                            event_tx,
                                            consecutive_failures,
                                            format!("Failed to create sink: {e}"),
                                        );
                                        return;
                                    }
                                }
                            } else {
                                Self::emit_failure(
                                    event_tx,
                                    consecutive_failures,
                                    "No audio stream available".to_string(),
                                );
                                return;
                            }
                        }

                        if let Some(active_sink) = sink {
                            active_sink.append(source);
                            *is_playing = true;
                            if *consecutive_failures >= 3 {
                                let _ = event_tx.send(RemoteTtsEvent::ConnectionRecovered);
                            }
                            *consecutive_failures = 0;
                        }
                    }
                    Err(e) => {
                        Self::emit_failure(
                            event_tx,
                            consecutive_failures,
                            format!("Failed to decode audio: {e}"),
                        );
                    }
                }
            }
            Err(message) => {
                Self::emit_failure(event_tx, consecutive_failures, message);
            }
        }
    }

    fn synthesize_openai_compatible(
        client: &Client,
        text: &str,
        settings: &RemoteSettings,
    ) -> Result<Vec<u8>, String> {
        let url = Self::build_openai_url(&settings.api_url);
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

        let response = client
            .post(&url)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .header("Accept", "audio/mpeg, audio/wav, application/octet-stream")
            .json(&body)
            .send()
            .map_err(|e| format!("Request failed: {e}"))?;

        let status = response.status();
        if !status.is_success() {
            let err_body = response.text().unwrap_or_default();
            return Err(format!("Server returned {status}: {err_body}"));
        }

        Self::read_audio_response_streaming(response)
    }

    fn build_openai_url(api_url: &str) -> String {
        let base = api_url.trim_end_matches('/');
        if base.ends_with("/v1/audio/speech") {
            base.to_string()
        } else if base.ends_with("/v1") {
            format!("{base}/audio/speech")
        } else {
            format!("{base}/v1/audio/speech")
        }
    }

    fn synthesize_edge(
        text: &str,
        settings: &RemoteSettings,
        edge_clock_skew_seconds: &mut i64,
    ) -> Result<Vec<u8>, String> {
        let chunks = Self::split_edge_text(text, 4096)?;
        let mut combined_audio = Vec::new();

        for chunk in chunks {
            let chunk_audio =
                Self::synthesize_edge_chunk(&chunk, settings, edge_clock_skew_seconds)?;
            combined_audio.extend_from_slice(&chunk_audio);
        }

        if combined_audio.is_empty() {
            Err("Edge-TTS returned empty audio".to_string())
        } else {
            Ok(combined_audio)
        }
    }

    fn synthesize_edge_chunk(
        text: &str,
        settings: &RemoteSettings,
        edge_clock_skew_seconds: &mut i64,
    ) -> Result<Vec<u8>, String> {
        let ssml = Self::build_edge_ssml(
            &settings.edge_voice,
            settings.edge_rate,
            settings.edge_volume,
            settings.edge_pitch,
            text,
        );
        let mut retried_after_skew_adjustment = false;

        loop {
            let url = format!(
                "{EDGE_WSS_URL}?TrustedClientToken={EDGE_TRUSTED_CLIENT_TOKEN}&ConnectionId={}&Sec-MS-GEC={}&Sec-MS-GEC-Version={EDGE_SEC_MS_GEC_VERSION}",
                Self::connect_id(),
                Self::generate_edge_sec_ms_gec(*edge_clock_skew_seconds),
            );

            match connect(Self::build_edge_websocket_request(&url)?) {
                Ok((mut websocket, _)) => {
                    websocket
                        .send(WsMessage::Text(Self::edge_speech_config_message()))
                        .map_err(|e| format!("Edge-TTS send failed: {e}"))?;
                    websocket
                        .send(WsMessage::Text(Self::edge_ssml_message(&ssml)))
                        .map_err(|e| format!("Edge-TTS send failed: {e}"))?;

                    let mut audio = Vec::new();
                    let mut audio_was_received = false;

                    loop {
                        match websocket.read() {
                            Ok(WsMessage::Text(message)) => {
                                let (headers, _) =
                                    Self::parse_text_headers_and_body(message.as_bytes())?;
                                match headers.get("Path").map(String::as_str) {
                                    Some("turn.end") => break,
                                    Some("audio.metadata")
                                    | Some("response")
                                    | Some("turn.start") => {}
                                    Some(other) => {
                                        return Err(format!(
                                            "Unknown Edge-TTS response path: {other}"
                                        ));
                                    }
                                    None => {}
                                }
                            }
                            Ok(WsMessage::Binary(data)) => {
                                let (headers, body) =
                                    Self::parse_edge_binary_headers_and_body(&data)?;
                                if headers.get("Path").map(String::as_str) != Some("audio") {
                                    return Err(
                                        "Received binary Edge-TTS message with non-audio path"
                                            .to_string(),
                                    );
                                }

                                match headers.get("Content-Type").map(String::as_str) {
                                    Some("audio/mpeg") => {
                                        if body.is_empty() {
                                            return Err(
                                                "Received Edge-TTS audio message without audio data"
                                                    .to_string(),
                                            );
                                        }
                                        audio.extend_from_slice(&body);
                                        audio_was_received = true;
                                    }
                                    None => {
                                        if !body.is_empty() {
                                            return Err(
                                                "Received Edge-TTS payload without content type"
                                                    .to_string(),
                                            );
                                        }
                                    }
                                    Some(other) => {
                                        return Err(format!(
                                            "Unexpected Edge-TTS content type: {other}"
                                        ));
                                    }
                                }
                            }
                            Ok(WsMessage::Close(_)) => break,
                            Ok(WsMessage::Ping(_)) | Ok(WsMessage::Pong(_)) => {}
                            Ok(WsMessage::Frame(_)) => {}
                            Err(WsError::Http(response)) => {
                                if response.status() == 403 && !retried_after_skew_adjustment {
                                    if let Some(date) = response.headers().get("Date") {
                                        if let Ok(date) = date.to_str() {
                                            if let Ok(server_time) = httpdate::parse_http_date(date)
                                            {
                                                *edge_clock_skew_seconds =
                                                    Self::clock_skew_from_server_time(server_time);
                                                retried_after_skew_adjustment = true;
                                                continue;
                                            }
                                        }
                                    }
                                }
                                return Err(format!(
                                    "Edge-TTS websocket handshake failed: {}",
                                    response.status()
                                ));
                            }
                            Err(e) => return Err(format!("Edge-TTS websocket failed: {e}")),
                        }
                    }

                    if !audio_was_received {
                        return Err("Edge-TTS did not return any audio".to_string());
                    }

                    return Ok(audio);
                }
                Err(WsError::Http(response)) => {
                    if response.status() == 403 && !retried_after_skew_adjustment {
                        if let Some(date) = response.headers().get("Date") {
                            if let Ok(date) = date.to_str() {
                                if let Ok(server_time) = httpdate::parse_http_date(date) {
                                    *edge_clock_skew_seconds =
                                        Self::clock_skew_from_server_time(server_time);
                                    retried_after_skew_adjustment = true;
                                    continue;
                                }
                            }
                        }
                    }
                    return Err(format!(
                        "Edge-TTS websocket handshake failed: {}",
                        response.status()
                    ));
                }
                Err(e) => return Err(format!("Edge-TTS websocket connect failed: {e}")),
            }
        }
    }

    fn list_edge_voices(
        client: &Client,
        edge_clock_skew_seconds: &mut i64,
    ) -> Result<Vec<String>, String> {
        let mut retried_after_skew_adjustment = false;

        loop {
            let response = client
                .get(format!(
                    "{EDGE_VOICE_LIST_URL}?trustedclienttoken={EDGE_TRUSTED_CLIENT_TOKEN}&Sec-MS-GEC={}&Sec-MS-GEC-Version={EDGE_SEC_MS_GEC_VERSION}",
                    Self::generate_edge_sec_ms_gec(*edge_clock_skew_seconds),
                ))
                .header("Authority", "speech.platform.bing.com")
                .header(
                    "Sec-CH-UA",
                    format!(
                        "\" Not;A Brand\";v=\"99\", \"Microsoft Edge\";v=\"{EDGE_CHROMIUM_MAJOR_VERSION}\", \"Chromium\";v=\"{EDGE_CHROMIUM_MAJOR_VERSION}\""
                    ),
                )
                .header("Sec-CH-UA-Mobile", "?0")
                .header("Accept", "*/*")
                .header("Sec-Fetch-Site", "none")
                .header("Sec-Fetch-Mode", "cors")
                .header("Sec-Fetch-Dest", "empty")
                .header("User-Agent", Self::edge_user_agent())
                .header("Accept-Encoding", "gzip, deflate, br, zstd")
                .header("Accept-Language", "en-US,en;q=0.9")
                .header("Cookie", format!("muid={};", Self::generate_muid()))
                .send()
                .map_err(|e| format!("Failed to request Edge voice list: {e}"))?;

            let status = response.status();
            if status == reqwest::StatusCode::FORBIDDEN && !retried_after_skew_adjustment {
                if let Some(date) = response.headers().get("Date") {
                    if let Ok(date) = date.to_str() {
                        if let Ok(server_time) = httpdate::parse_http_date(date) {
                            *edge_clock_skew_seconds =
                                Self::clock_skew_from_server_time(server_time);
                            retried_after_skew_adjustment = true;
                            continue;
                        }
                    }
                }
            }

            if !status.is_success() {
                let body = response.text().unwrap_or_default();
                return Err(format!("Edge voice list failed with {status}: {body}"));
            }

            let mut names: Vec<String> = response
                .json::<Vec<EdgeVoiceEntry>>()
                .map_err(|e| format!("Failed to parse Edge voice list: {e}"))?
                .into_iter()
                .map(|voice| voice.short_name)
                .collect();
            names.sort();
            names.dedup();
            return Ok(names);
        }
    }

    fn build_edge_websocket_request(url: &str) -> Result<Request<()>, String> {
        let mut request = url
            .into_client_request()
            .map_err(|e| format!("Failed to build Edge websocket request: {e}"))?;

        let headers = request.headers_mut();
        headers.insert(
            "Pragma",
            "no-cache"
                .parse()
                .map_err(|e| format!("Failed to set Pragma header: {e}"))?,
        );
        headers.insert(
            "Cache-Control",
            "no-cache"
                .parse()
                .map_err(|e| format!("Failed to set Cache-Control header: {e}"))?,
        );
        headers.insert(
            "Origin",
            "chrome-extension://jdiccldimpdaibmpdkjnbmckianbfold"
                .parse()
                .map_err(|e| format!("Failed to set Origin header: {e}"))?,
        );
        headers.insert(
            "User-Agent",
            Self::edge_user_agent()
                .parse()
                .map_err(|e| format!("Failed to set User-Agent header: {e}"))?,
        );
        headers.insert(
            "Accept-Encoding",
            "gzip, deflate, br, zstd"
                .parse()
                .map_err(|e| format!("Failed to set Accept-Encoding header: {e}"))?,
        );
        headers.insert(
            "Accept-Language",
            "en-US,en;q=0.9"
                .parse()
                .map_err(|e| format!("Failed to set Accept-Language header: {e}"))?,
        );
        headers.insert(
            "Cookie",
            format!("muid={};", Self::generate_muid())
                .parse()
                .map_err(|e| format!("Failed to set Cookie header: {e}"))?,
        );

        Ok(request)
    }

    fn edge_speech_config_message() -> String {
        format!(
            "X-Timestamp:{}\r\nContent-Type:application/json; charset=utf-8\r\nPath:speech.config\r\n\r\n{{\"context\":{{\"synthesis\":{{\"audio\":{{\"metadataoptions\":{{\"sentenceBoundaryEnabled\":\"true\",\"wordBoundaryEnabled\":\"false\"}},\"outputFormat\":\"{}\"}}}}}}}}\r\n",
            Self::edge_timestamp(),
            EDGE_OUTPUT_FORMAT
        )
    }

    fn edge_ssml_message(ssml: &str) -> String {
        format!(
            "X-RequestId:{}\r\nContent-Type:application/ssml+xml\r\nX-Timestamp:{}Z\r\nPath:ssml\r\n\r\n{}",
            Self::connect_id(),
            Self::edge_timestamp(),
            ssml
        )
    }

    fn build_edge_ssml(voice: &str, rate: i32, volume: i32, pitch: i32, text: &str) -> String {
        let escaped_text = Self::escape_xml(&Self::remove_incompatible_characters(text));
        let voice_name = Self::canonical_edge_voice_name(voice);
        format!(
            "<speak version='1.0' xmlns='http://www.w3.org/2001/10/synthesis' xml:lang='en-US'><voice name='{voice_name}'><prosody pitch='{}Hz' rate='{}%' volume='{}%'>{}</prosody></voice></speak>",
            Self::signed_number(pitch),
            Self::signed_number(rate),
            Self::signed_number(volume),
            escaped_text
        )
    }

    fn edge_timestamp() -> String {
        Utc::now()
            .format("%a %b %d %Y %H:%M:%S GMT+0000 (Coordinated Universal Time)")
            .to_string()
    }

    fn edge_user_agent() -> String {
        format!(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{EDGE_CHROMIUM_MAJOR_VERSION}.0.0.0 Safari/537.36 Edg/{EDGE_CHROMIUM_MAJOR_VERSION}.0.0.0"
        )
    }

    fn connect_id() -> String {
        Uuid::new_v4().simple().to_string()
    }

    fn signed_number(value: i32) -> String {
        if value >= 0 {
            format!("+{value}")
        } else {
            value.to_string()
        }
    }

    fn canonical_edge_voice_name(voice: &str) -> String {
        if voice.starts_with("Microsoft Server Speech Text to Speech Voice (") {
            return voice.to_string();
        }

        let mut parts = voice.splitn(3, '-');
        let Some(language) = parts.next() else {
            return voice.to_string();
        };
        let Some(region) = parts.next() else {
            return voice.to_string();
        };
        let Some(mut name) = parts.next() else {
            return voice.to_string();
        };

        let mut full_region = region.to_string();
        if let Some((region_suffix, rest)) = name.split_once('-') {
            full_region = format!("{region}-{region_suffix}");
            name = rest;
        }

        format!("Microsoft Server Speech Text to Speech Voice ({language}-{full_region}, {name})")
    }

    fn generate_edge_sec_ms_gec(clock_skew_seconds: i64) -> String {
        let unix_seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or_default()
            + clock_skew_seconds;
        let rounded_seconds = unix_seconds - unix_seconds.rem_euclid(300);
        let windows_file_time_ticks = (rounded_seconds as u64 + WIN_EPOCH_SECONDS) * 10_000_000u64;
        let raw = format!("{windows_file_time_ticks}{EDGE_TRUSTED_CLIENT_TOKEN}");
        let mut hasher = Sha256::new();
        hasher.update(raw.as_bytes());
        format!("{:X}", hasher.finalize())
    }

    fn generate_muid() -> String {
        Uuid::new_v4().simple().to_string().to_uppercase()
    }

    fn clock_skew_from_server_time(server_time: SystemTime) -> i64 {
        let server = server_time
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or_default();
        let local = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or_default();
        server - local
    }

    fn split_edge_text(text: &str, max_bytes: usize) -> Result<Vec<String>, String> {
        let mut remaining = Self::remove_incompatible_characters(text).into_bytes();
        let mut chunks = Vec::new();

        while remaining.len() > max_bytes {
            let mut split_at = Self::find_last_newline_or_space_within_limit(&remaining, max_bytes);
            if split_at == 0 {
                split_at = Self::find_safe_utf8_split_point(&remaining[..max_bytes]);
            }
            split_at = Self::adjust_split_point_for_xml_entity(&remaining, split_at);
            if split_at == 0 {
                split_at = Self::find_safe_utf8_split_point(&remaining[..max_bytes]);
            }
            if split_at == 0 {
                return Err("Unable to split long text safely for Edge-TTS".to_string());
            }

            let chunk = String::from_utf8(remaining[..split_at].to_vec())
                .map_err(|e| format!("Invalid UTF-8 while splitting text: {e}"))?;
            let trimmed = chunk.trim();
            if !trimmed.is_empty() {
                chunks.push(trimmed.to_string());
            }
            remaining = remaining[split_at..].to_vec();
        }

        let tail = String::from_utf8(remaining)
            .map_err(|e| format!("Invalid UTF-8 while finalizing text split: {e}"))?;
        let trimmed = tail.trim();
        if !trimmed.is_empty() {
            chunks.push(trimmed.to_string());
        }

        Ok(chunks)
    }

    fn find_last_newline_or_space_within_limit(text: &[u8], limit: usize) -> usize {
        let limit = limit.min(text.len());
        for idx in (0..limit).rev() {
            if text[idx] == b'\n' || text[idx] == b' ' {
                return idx;
            }
        }
        0
    }

    fn find_safe_utf8_split_point(text: &[u8]) -> usize {
        for idx in (1..=text.len()).rev() {
            if std::str::from_utf8(&text[..idx]).is_ok() {
                return idx;
            }
        }
        0
    }

    fn adjust_split_point_for_xml_entity(text: &[u8], mut split_at: usize) -> usize {
        while split_at > 0 {
            let Some(ampersand_index) = text[..split_at].iter().rposition(|b| *b == b'&') else {
                break;
            };
            if text[ampersand_index..split_at].contains(&b';') {
                break;
            }
            split_at = ampersand_index;
        }
        split_at
    }

    fn remove_incompatible_characters(text: &str) -> String {
        text.chars()
            .map(|ch| match ch as u32 {
                0..=8 | 11..=12 | 14..=31 => ' ',
                _ => ch,
            })
            .collect()
    }

    fn escape_xml(text: &str) -> String {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;")
    }

    fn parse_text_headers_and_body(
        data: &[u8],
    ) -> Result<(HashMap<String, String>, Vec<u8>), String> {
        let separator = data
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .ok_or_else(|| "Edge-TTS text message missing header separator".to_string())?;
        let header_bytes = &data[..separator];
        let body = data[separator + 4..].to_vec();
        Ok((Self::parse_headers(header_bytes), body))
    }

    fn parse_edge_binary_headers_and_body(
        data: &[u8],
    ) -> Result<(HashMap<String, String>, Vec<u8>), String> {
        if data.len() < 2 {
            return Err("Edge-TTS binary message missing header length".to_string());
        }

        let header_length = u16::from_be_bytes([data[0], data[1]]) as usize;
        if header_length > data.len().saturating_sub(2) && header_length > data.len() {
            return Err("Edge-TTS binary header length exceeds payload size".to_string());
        }

        // Compatibility handling:
        // some implementations treat header_length as excluding the first 2 bytes,
        // while others effectively include them in slicing logic.
        // Try the protocol-correct branch first, then fall back to legacy parsing.
        let candidate_primary = if let Some(header_end) = 2usize.checked_add(header_length) {
            if header_end <= data.len() {
                let headers = Self::parse_headers(&data[2..header_end]);
                let body = data[header_end..].to_vec();
                Some((headers, body))
            } else {
                None
            }
        } else {
            None
        };

        if let Some((headers, body)) = candidate_primary {
            if !headers.is_empty() {
                return Ok((headers, body));
            }
        }

        if header_length + 2 <= data.len() {
            let headers = Self::parse_headers(&data[..header_length]);
            let body = data[header_length + 2..].to_vec();
            return Ok((headers, body));
        }

        Err("Edge-TTS binary frame parsing failed".to_string())
    }

    fn parse_headers(header_bytes: &[u8]) -> HashMap<String, String> {
        let header_text = String::from_utf8_lossy(header_bytes);
        let mut headers = HashMap::new();

        for line in header_text.split("\r\n") {
            if line.is_empty() {
                continue;
            }
            if let Some((key, value)) = line.split_once(':') {
                headers.insert(key.trim().to_string(), value.trim().to_string());
            }
        }

        headers
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

    fn read_audio_response_streaming(mut response: Response) -> Result<Vec<u8>, String> {
        let mut bytes = Vec::with_capacity(response.content_length().unwrap_or(0) as usize);
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
