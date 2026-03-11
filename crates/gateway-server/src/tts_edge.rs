//! Edge TTS provider  - Microsoft Edge text-to-speech via WebSocket.
//! No API key required  - uses the same endpoint as Microsoft Edge browser.
//!
//! ## Protocol overview
//!
//! 1. Open a WSS connection to `speech.platform.bing.com` with a `TrustedClientToken` query
//!    parameter and a random `ConnectionId`.
//! 2. Send a **speech.config** text message (JSON with audio output format).
//! 3. Send an **ssml** text message containing the SSML payload.
//! 4. Receive a mix of text messages (`turn.start`, `audio.metadata`, `turn.end`) and binary
//!    messages (audio data with a 2-byte header-length prefix).
//! 5. Collect all binary audio payloads until `turn.end` is received.
//!
//! ## Dependency note
//!
//! The `synthesize()` function requires `tokio-tungstenite` with TLS support
//! (e.g. the `native-tls` feature) as a **regular** dependency. Currently it is
//! only present in `[dev-dependencies]`. To enable full WSS synthesis, promote
//! it to `[dependencies]` with:
//!
//! ```toml
//! tokio-tungstenite = { workspace = true, features = ["connect", "native-tls"] }
//! ```
//!
//! Until then, `synthesize()` returns the generated SSML and protocol metadata
//! in a structured error so callers can see exactly what *would* be sent.

use serde::{Deserialize, Serialize};

/// Edge TTS voice options
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeTtsVoice {
    pub name: String,
    pub short_name: String,
    pub locale: String,
    pub gender: String,
}

/// Common Edge TTS voices
pub fn default_voices() -> Vec<EdgeTtsVoice> {
    vec![
        EdgeTtsVoice {
            name: "Microsoft Server Speech Text to Speech Voice (en-US, AriaNeural)".into(),
            short_name: "en-US-AriaNeural".into(),
            locale: "en-US".into(),
            gender: "Female".into(),
        },
        EdgeTtsVoice {
            name: "Microsoft Server Speech Text to Speech Voice (en-US, GuyNeural)".into(),
            short_name: "en-US-GuyNeural".into(),
            locale: "en-US".into(),
            gender: "Male".into(),
        },
        EdgeTtsVoice {
            name: "Microsoft Server Speech Text to Speech Voice (en-GB, SoniaNeural)".into(),
            short_name: "en-GB-SoniaNeural".into(),
            locale: "en-GB".into(),
            gender: "Female".into(),
        },
        EdgeTtsVoice {
            name: "Microsoft Server Speech Text to Speech Voice (en-GB, RyanNeural)".into(),
            short_name: "en-GB-RyanNeural".into(),
            locale: "en-GB".into(),
            gender: "Male".into(),
        },
        EdgeTtsVoice {
            name: "Microsoft Server Speech Text to Speech Voice (zh-CN, XiaoxiaoNeural)".into(),
            short_name: "zh-CN-XiaoxiaoNeural".into(),
            locale: "zh-CN".into(),
            gender: "Female".into(),
        },
        EdgeTtsVoice {
            name: "Microsoft Server Speech Text to Speech Voice (zh-CN, YunxiNeural)".into(),
            short_name: "zh-CN-YunxiNeural".into(),
            locale: "zh-CN".into(),
            gender: "Male".into(),
        },
        EdgeTtsVoice {
            name: "Microsoft Server Speech Text to Speech Voice (ja-JP, NanamiNeural)".into(),
            short_name: "ja-JP-NanamiNeural".into(),
            locale: "ja-JP".into(),
            gender: "Female".into(),
        },
        EdgeTtsVoice {
            name: "Microsoft Server Speech Text to Speech Voice (ko-KR, SunHiNeural)".into(),
            short_name: "ko-KR-SunHiNeural".into(),
            locale: "ko-KR".into(),
            gender: "Female".into(),
        },
        EdgeTtsVoice {
            name: "Microsoft Server Speech Text to Speech Voice (fr-FR, DeniseNeural)".into(),
            short_name: "fr-FR-DeniseNeural".into(),
            locale: "fr-FR".into(),
            gender: "Female".into(),
        },
        EdgeTtsVoice {
            name: "Microsoft Server Speech Text to Speech Voice (de-DE, KatjaNeural)".into(),
            short_name: "de-DE-KatjaNeural".into(),
            locale: "de-DE".into(),
            gender: "Female".into(),
        },
        EdgeTtsVoice {
            name: "Microsoft Server Speech Text to Speech Voice (es-ES, ElviraNeural)".into(),
            short_name: "es-ES-ElviraNeural".into(),
            locale: "es-ES".into(),
            gender: "Female".into(),
        },
        EdgeTtsVoice {
            name: "Microsoft Server Speech Text to Speech Voice (vi-VN, HoaiMyNeural)".into(),
            short_name: "vi-VN-HoaiMyNeural".into(),
            locale: "vi-VN".into(),
            gender: "Female".into(),
        },
    ]
}

/// Edge TTS configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeTtsConfig {
    pub voice: String,
    #[serde(default = "default_rate")]
    pub rate: String,
    #[serde(default = "default_volume")]
    pub volume: String,
    #[serde(default = "default_pitch")]
    pub pitch: String,
}

fn default_rate() -> String {
    "+0%".into()
}
fn default_volume() -> String {
    "+0%".into()
}
fn default_pitch() -> String {
    "+0Hz".into()
}

impl Default for EdgeTtsConfig {
    fn default() -> Self {
        Self {
            voice: "en-US-AriaNeural".into(),
            rate: default_rate(),
            volume: default_volume(),
            pitch: default_pitch(),
        }
    }
}

/// Generate SSML for Edge TTS
pub fn generate_ssml(text: &str, config: &EdgeTtsConfig) -> String {
    format!(
        r#"<speak version='1.0' xmlns='http://www.w3.org/2001/10/synthesis' xml:lang='en-US'>
            <voice name='{}'>
                <prosody pitch='{}' rate='{}' volume='{}'>
                    {}
                </prosody>
            </voice>
        </speak>"#,
        config.voice,
        config.pitch,
        config.rate,
        config.volume,
        escape_xml(text),
    )
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Edge TTS WebSocket endpoint
pub const EDGE_TTS_WS_URL: &str =
    "wss://speech.platform.bing.com/consumer/speech/synthesize/readaloud/edge/v1";

/// Trusted client token (embedded in all Edge browser installations).
pub const TRUSTED_CLIENT_TOKEN: &str = "6A5AA1D4EAFF4E9FB37E23D68491D6F4";

/// Default audio output format requested from the Edge TTS service.
pub const DEFAULT_OUTPUT_FORMAT: &str = "audio-24khz-48kbitrate-mono-mp3";

/// Build the full WebSocket URL with token and connection id.
pub fn build_ws_url(connection_id: &str) -> String {
    format!(
        "{}?TrustedClientToken={}&ConnectionId={}",
        EDGE_TTS_WS_URL, TRUSTED_CLIENT_TOKEN, connection_id
    )
}

/// Build the `speech.config` text message sent at the start of a session.
///
/// This tells the Edge TTS service which audio format to return.
pub fn build_speech_config_message(output_format: &str) -> String {
    format!(
        "Content-Type:application/json; charset=utf-8\r\nPath:speech.config\r\n\r\n\
         {{\"context\":{{\"synthesis\":{{\"audio\":{{\"metadataoptions\":{{\
         \"sentenceBoundaryEnabled\":\"false\",\"wordBoundaryEnabled\":\"true\"}},\
         \"outputFormat\":\"{output_format}\"}}}}}}}}"
    )
}

/// Build the SSML text message for a synthesis request.
///
/// Each request carries its own `X-RequestId` so the server can correlate
/// binary audio chunks back to this turn.
pub fn build_ssml_message(request_id: &str, ssml: &str) -> String {
    format!(
        "X-RequestId:{request_id}\r\n\
         Content-Type:application/ssml+xml\r\n\
         Path:ssml\r\n\r\n\
         {ssml}"
    )
}

/// Extract audio bytes from a binary WebSocket message.
///
/// Binary messages from Edge TTS have a 2-byte big-endian header length prefix.
/// The first `header_len` bytes after those 2 bytes are a text header (metadata),
/// and the remainder is the raw audio payload.
pub fn extract_audio_from_binary(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < 2 {
        return None;
    }
    let header_len = u16::from_be_bytes([data[0], data[1]]) as usize;
    let audio_start = 2 + header_len;
    if audio_start > data.len() {
        return None;
    }
    Some(data[audio_start..].to_vec())
}

/// Synthesize text to audio bytes using the Edge TTS WebSocket protocol.
///
/// **Current status**: This is a *stub* implementation. The full WSS flow is
/// documented and all protocol messages are generated correctly, but the
/// actual WebSocket connection requires `tokio-tungstenite` with TLS support
/// to be added as a regular dependency (see module-level docs).
///
/// The stub returns an `Err` that contains a JSON-like description of the
/// request that would be made, including the SSML, the WebSocket URL, and the
/// speech config message. This lets callers (and tests) verify that all
/// protocol pieces are assembled correctly without needing a live connection.
///
/// # Arguments
/// * `text` - The text to synthesize.
/// * `config` - Edge TTS voice/prosody configuration.
///
/// # Errors
/// Always returns `Err` with a descriptive message until the WSS dependency
/// is promoted. The error payload includes the SSML, URL, and config message
/// for debugging or forwarding to an external Edge TTS proxy.
pub async fn synthesize(text: &str, config: &EdgeTtsConfig) -> Result<Vec<u8>, String> {
    if text.is_empty() {
        return Err("text must not be empty".to_string());
    }

    // Generate all the protocol artefacts.
    let connection_id = uuid::Uuid::now_v7().to_string().replace('-', "");
    let request_id = uuid::Uuid::now_v7().to_string().replace('-', "");
    let ws_url = build_ws_url(&connection_id);
    let speech_config = build_speech_config_message(DEFAULT_OUTPUT_FORMAT);
    let ssml = generate_ssml(text, config);
    let ssml_message = build_ssml_message(&request_id, &ssml);

    // ── WSS connection requires tokio-tungstenite with native-tls ────────
    //
    // The full implementation would:
    //   1. connect_async(ws_url) with Origin header
    //      "chrome-extension://jdiccldimpdaibmpdmdsshecamfdnail"
    //   2. send(Text(speech_config))
    //   3. send(Text(ssml_message))
    //   4. loop recv():
    //      - Text msg containing "Path:turn.end"  -> break
    //      - Binary msg -> extract_audio_from_binary() and append to buffer
    //   5. return collected audio buffer
    //
    // To enable, add to Cargo.toml [dependencies]:
    //   tokio-tungstenite = { workspace = true, features = ["connect", "native-tls"] }
    //
    // Then replace this stub with the connection loop above.

    Err(format!(
        "edge tts wss synthesis not yet wired (tokio-tungstenite needs TLS feature in [dependencies]). \
         Protocol artefacts generated successfully:\n\
         - ws_url: {ws_url}\n\
         - voice: {voice}\n\
         - output_format: {fmt}\n\
         - ssml_length: {ssml_len}\n\
         - speech_config_length: {cfg_len}\n\
         - ssml_message_length: {msg_len}",
        voice = config.voice,
        fmt = DEFAULT_OUTPUT_FORMAT,
        ssml_len = ssml.len(),
        cfg_len = speech_config.len(),
        msg_len = ssml_message.len(),
    ))
}

/// Edge TTS provider info
pub fn provider_info() -> serde_json::Value {
    serde_json::json!({
        "id": "edge",
        "name": "Microsoft Edge TTS",
        "requires_api_key": false,
        "voices": default_voices(),
        "note": "WSS synthesis requires tokio-tungstenite with native-tls feature",
        "description": "Free text-to-speech using Microsoft Edge neural voices. No API key required."
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_url_contains_token_and_id() {
        let url = build_ws_url("abc123");
        assert!(url.starts_with(EDGE_TTS_WS_URL));
        assert!(url.contains(TRUSTED_CLIENT_TOKEN));
        assert!(url.contains("ConnectionId=abc123"));
    }

    #[test]
    fn speech_config_message_format() {
        let msg = build_speech_config_message("audio-24khz-48kbitrate-mono-mp3");
        assert!(msg.contains("Content-Type:application/json"));
        assert!(msg.contains("Path:speech.config"));
        assert!(msg.contains("outputFormat"));
        assert!(msg.contains("audio-24khz-48kbitrate-mono-mp3"));
    }

    #[test]
    fn ssml_message_format() {
        let ssml = "<speak>hello</speak>";
        let msg = build_ssml_message("req123", ssml);
        assert!(msg.contains("X-RequestId:req123"));
        assert!(msg.contains("Content-Type:application/ssml+xml"));
        assert!(msg.contains("Path:ssml"));
        assert!(msg.contains(ssml));
    }

    #[test]
    fn extract_audio_valid() {
        // 2-byte header length (0x0003) + 3 header bytes + audio payload
        let mut data = vec![0x00, 0x03, b'H', b'D', b'R'];
        data.extend_from_slice(b"AUDIO_DATA");
        let audio = extract_audio_from_binary(&data).unwrap();
        assert_eq!(audio, b"AUDIO_DATA");
    }

    #[test]
    fn extract_audio_empty_payload() {
        // Header length = 2, total = 4, no audio bytes
        let data = vec![0x00, 0x02, b'A', b'B'];
        let audio = extract_audio_from_binary(&data).unwrap();
        assert!(audio.is_empty());
    }

    #[test]
    fn extract_audio_too_short() {
        let data = vec![0x00];
        assert!(extract_audio_from_binary(&data).is_none());
    }

    #[test]
    fn extract_audio_header_exceeds_data() {
        // Claims 255 header bytes but only 3 bytes total
        let data = vec![0x00, 0xFF, b'A'];
        assert!(extract_audio_from_binary(&data).is_none());
    }

    #[test]
    fn generate_ssml_escapes_special_chars() {
        let config = EdgeTtsConfig::default();
        let ssml = generate_ssml("Tom & Jerry <3 \"quotes\" 'apos'", &config);
        assert!(ssml.contains("Tom &amp; Jerry &lt;3 &quot;quotes&quot; &apos;apos&apos;"));
        assert!(ssml.contains("en-US-AriaNeural"));
    }

    #[tokio::test]
    async fn synthesize_rejects_empty_text() {
        let config = EdgeTtsConfig::default();
        let result = synthesize("", &config).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("text must not be empty"));
    }

    #[tokio::test]
    async fn synthesize_stub_returns_protocol_info() {
        let config = EdgeTtsConfig::default();
        let result = synthesize("Hello world", &config).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        // The stub error should contain protocol metadata
        assert!(err.contains("ws_url:"));
        assert!(err.contains("voice: en-US-AriaNeural"));
        assert!(err.contains("output_format:"));
        assert!(err.contains("ssml_length:"));
    }

    #[test]
    fn provider_info_fields() {
        let info = provider_info();
        assert_eq!(info["id"], "edge");
        assert_eq!(info["requires_api_key"], false);
        assert!(info["voices"].is_array());
    }
}
