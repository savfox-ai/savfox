# savfox-utils-string

Provides two utility functions for truncating Rust `&str` slices to a byte budget while respecting UTF-8 character boundaries.

`take_bytes_at_char_boundary(s, maxb)` returns the longest prefix of `s` that fits within `maxb` bytes without splitting a multi-byte character. `take_last_bytes_at_char_boundary(s, maxb)` does the same for the suffix. Both functions return the original slice unchanged when it already fits within the budget. These are used throughout the workspace to safely truncate user-visible or protocol strings without producing invalid UTF-8.
