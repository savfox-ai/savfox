//! Voice-related gateway domain.
//!
//! This groups speech-to-text, text-to-speech, wake-word, and talk-mode
//! services behind one domain namespace while preserving the existing root
//! re-exports for compatibility.

pub mod stt;
pub mod talk_mode;
pub(crate) mod tts_deepgram;
pub(crate) mod tts_edge;
pub(crate) mod tts_service;
pub(crate) mod voice_store;
pub mod voice_wake;
