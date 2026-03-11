//! Command history traversal and navigation.
//!
//! This module contains the `ChatComposer` methods that deal with
//! navigating the Up/Down history stack and integrating asynchronous
//! history entry responses.

use super::ChatComposer;

impl ChatComposer {
    /// Record the history metadata advertised by `SessionConfiguredEvent` so
    /// that the composer can navigate cross-session history.
    pub(crate) fn set_history_metadata(&mut self, log_id: u64, entry_count: usize) {
        self.history.set_metadata(log_id, entry_count);
    }

    /// Integrate an asynchronous response to an on-demand history lookup. If
    /// the entry is present and the offset matches the current cursor we
    /// immediately populate the textarea.
    pub(crate) fn on_history_entry_response(
        &mut self,
        log_id: u64,
        offset: usize,
        entry: Option<String>,
    ) -> bool {
        let Some(entry) = self.history.on_entry_response(log_id, offset, entry) else {
            return false;
        };
        // Persistent Up/Down history is text-only (backwards-compatible and avoids persisting
        // attachments), but local in-session Up/Down history can rehydrate elements and image
        // paths.
        self.set_text_content(entry.text, entry.text_elements, entry.local_image_paths);
        true
    }
}
