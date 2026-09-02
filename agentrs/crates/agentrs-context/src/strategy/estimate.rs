// Ported from aionrs (Apache-2.0), crates/aion-agent.
//   Source: crates/aion-agent/src/compact/estimate.rs @ f711174
//   Copied: 2026-09-01   Modified: yes
//   Changes: aion_types → agentrs_types；`pub(crate)` 提为 `pub`

use agentrs_types::message::{ContentBlock, ImageUrl};

const CHARS_PER_TOKEN_TEXT: usize = 4;

/// Estimate one final tool result that will be added after the provider's
/// exact usage measurement.
pub fn estimate_tokens_from_tool_result(block: &ContentBlock) -> u64 {
    match block {
        ContentBlock::ToolResult { content, .. } => (content.len() / CHARS_PER_TOKEN_TEXT) as u64,
        _ => 0,
    }
}

/// Estimate an image block emitted by a tool for the next provider request.
pub fn estimate_tokens_from_tool_image(block: &ContentBlock) -> u64 {
    match block {
        ContentBlock::Image { image_url } => estimate_image_tokens(image_url),
        _ => 0,
    }
}

fn estimate_image_tokens(image_url: &ImageUrl) -> u64 {
    // Image token cost is not proportional to base64 string length. Use a
    // provider-agnostic heuristic based on decoded byte size and clamp it to
    // reasonable per-image bounds.
    const BYTES_PER_TOKEN: usize = 750;
    const MIN_IMAGE_TOKENS: usize = 85;
    const MAX_IMAGE_TOKENS: usize = 2048;

    // 本仓库的 `ImageUrl` 只有 `url`，没有 aionrs 那个 `decoded_byte_size()`。
    // data URI 的 base64 段每 4 个字符解出 3 字节；远端 URL 估不出来，按 0 算
    // （随后会被 MIN_IMAGE_TOKENS 兜住）。
    let bytes = image_url
        .url
        .split_once(";base64,")
        .map(|(_, b64)| b64.len() / 4 * 3)
        .unwrap_or(0);
    (bytes / BYTES_PER_TOKEN).clamp(MIN_IMAGE_TOKENS, MAX_IMAGE_TOKENS) as u64
}

