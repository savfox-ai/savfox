//! Voice-related gateway domain.
//!
//! This groups speech-to-text, text-to-speech, wake-word, and talk-mode
//! services behind one domain namespace while preserving the existing root
//! re-exports for compatibility.

#[path = "../stt.rs"]
pub mod stt;
#[path = "../talk_mode/mod.rs"]
pub mod talk_mode;
#[path = "../tts_deepgram.rs"]
pub(crate) mod tts_deepgram;
#[path = "../tts_edge.rs"]
pub(crate) mod tts_edge;
#[path = "../tts_service.rs"]
pub(crate) mod tts_service;
#[path = "../voice_store.rs"]
pub(crate) mod voice_store;
#[path = "../voice_wake/mod.rs"]
pub mod voice_wake;
