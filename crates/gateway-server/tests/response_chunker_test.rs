//! Verify actual channel response splitting rather than a local chunking imitation.

use pretty_assertions::assert_eq;
use savfox_gateway_server::response_chunker::chunk_message_for_channel;

#[test]
fn unicode_responses_remain_valid_and_complete_at_platform_limits() {
    let message = "中文🙂".repeat(1800);
    for (channel, limit) in [("discord:room", 2000), ("telegram:room", 4096)] {
        let chunks = chunk_message_for_channel(&message, channel, None, 0);
        assert!(chunks.len() > 1);
        for (index, chunk) in chunks.iter().enumerate() {
            assert!(chunk.text.chars().count() <= limit);
            assert_eq!(chunk.index, index + 1);
            assert_eq!(chunk.total, chunks.len());
        }
        assert_eq!(
            chunks
                .iter()
                .map(|chunk| {
                    let marker = format!("\n\n({}/{})", chunk.index, chunk.total);
                    chunk
                        .text
                        .strip_suffix(&marker)
                        .expect("numbered chunk footer")
                })
                .collect::<String>(),
            message
        );
    }
}
