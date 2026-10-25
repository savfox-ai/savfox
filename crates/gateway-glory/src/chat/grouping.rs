// Message grouping helpers (`group_messages`, `MessageGroup`, `should_start_new_group`)
// were removed: they had no remaining call sites in any render path. The chat view in
// `pages/sessions.rs` groups adjacent same-role messages inline via the `prev_same_role`
// flag instead. This module is retained only because `chat::mod` declares it.
