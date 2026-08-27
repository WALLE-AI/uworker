//! SSE 分帧。
//!
//! 纯函数、无 I/O：喂字节进去，吐出完整的 `data:` 载荷。
//! 这样分帧逻辑可以在没有网络的情况下被完整测试——传输由调用方负责。

/// 增量 SSE 解帧器。
///
/// 处理三件容易出错的事：跨 chunk 断裂的行、`[DONE]` 哨兵、多行 `data:` 拼接。
#[derive(Debug, Default)]
pub struct SseDecoder {
    buf: String,
    /// 当前事件累积的 data 行。
    data: Vec<String>,
}

/// 解帧产物。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SseFrame {
    /// 一条完整的 data 载荷。
    Data(String),
    /// 流结束哨兵 `[DONE]`。
    Done,
}

impl SseDecoder {
    /// 新建解帧器。
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂入一段字节，返回其中解出的完整帧。
    ///
    /// 半行会留在内部缓冲，等下一段字节到来——**跨 chunk 断裂是 SSE 最常见的 bug 源**。
    pub fn push(&mut self, chunk: &str) -> Vec<SseFrame> {
        self.buf.push_str(chunk);
        let mut out = Vec::new();

        while let Some(idx) = self.buf.find('\n') {
            let line = self.buf[..idx].trim_end_matches('\r').to_string();
            self.buf.drain(..=idx);

            if line.is_empty() {
                // 空行 = 事件边界
                if !self.data.is_empty() {
                    let payload = self.data.join("\n");
                    self.data.clear();
                    if payload.trim() == "[DONE]" {
                        out.push(SseFrame::Done);
                    } else {
                        out.push(SseFrame::Data(payload));
                    }
                }
                continue;
            }

            if let Some(rest) = line.strip_prefix("data:") {
                self.data.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
            }
            // 其余字段（event:/id:/retry:/注释行）当前不需要，安全忽略。
        }
        out
    }

    /// 流结束时冲刷残留。有些端点末帧不带空行。
    pub fn finish(&mut self) -> Vec<SseFrame> {
        if self.data.is_empty() {
            return Vec::new();
        }
        let payload = self.data.join("\n");
        self.data.clear();
        if payload.trim() == "[DONE]" {
            vec![SseFrame::Done]
        } else {
            vec![SseFrame::Data(payload)]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 单帧解析() {
        let mut d = SseDecoder::new();
        assert_eq!(
            d.push("data: {\"a\":1}\n\n"),
            vec![SseFrame::Data("{\"a\":1}".into())]
        );
    }

    #[test]
    fn 跨_chunk_断裂的行能正确拼接() {
        // 真实网络下一帧被切成两段是常态，这是 SSE 最常见的 bug 源。
        let mut d = SseDecoder::new();
        assert!(d.push("data: {\"a\":").is_empty(), "半行不得产出");
        assert_eq!(d.push("1}\n\n"), vec![SseFrame::Data("{\"a\":1}".into())]);
    }

    #[test]
    fn 一次喂入多帧() {
        let mut d = SseDecoder::new();
        let f = d.push("data: 1\n\ndata: 2\n\ndata: [DONE]\n\n");
        assert_eq!(
            f,
            vec![
                SseFrame::Data("1".into()),
                SseFrame::Data("2".into()),
                SseFrame::Done
            ]
        );
    }

    #[test]
    fn 忽略非_data_字段与注释() {
        let mut d = SseDecoder::new();
        let f = d.push(": keep-alive\nevent: message\nid: 7\ndata: x\n\n");
        assert_eq!(f, vec![SseFrame::Data("x".into())]);
    }

    #[test]
    fn 多行_data_按换行拼接() {
        let mut d = SseDecoder::new();
        assert_eq!(
            d.push("data: a\ndata: b\n\n"),
            vec![SseFrame::Data("a\nb".into())]
        );
    }

    #[test]
    fn crlf_行尾() {
        let mut d = SseDecoder::new();
        assert_eq!(d.push("data: x\r\n\r\n"), vec![SseFrame::Data("x".into())]);
    }

    #[test]
    fn 末帧无空行时由_finish_冲刷() {
        let mut d = SseDecoder::new();
        assert!(d.push("data: last\n").is_empty());
        assert_eq!(d.finish(), vec![SseFrame::Data("last".into())]);
    }
}
