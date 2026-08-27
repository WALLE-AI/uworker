//! `ProviderCompat`：厂商差异的**数据表**（架构 §7，ADR 3）。
//!
//! 厂商差异集中在这张表与投影器里，**绝不进主循环分支**。新增一个
//! OpenAI 兼容端点应当只需要加一行 compat，不改任何逻辑代码。
//!
//! 部分字段自 aionrs 的 `aion-config/src/compat.rs` 借鉴形态，但剥离了
//! TOML 读取——配置由宿主转换为不可变值传入，内核不读配置文件。

use agentrs_types::ImageInputCapability;

/// 目标 provider 的兼容性描述。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderCompat {
    /// 每个 `tool_use` 是否必须**紧跟**其 `tool_result`。
    ///
    /// Anthropic 族要求如此：残缺的 `tool_use` 会让请求直接 400。
    /// 取消或崩溃后历史天然残缺，因此这条是 `HistoryLegalization` 的主要工作来源。
    pub requires_tool_result_adjacency: bool,

    /// 是否接受 thinking / reasoning 块。
    ///
    /// 即便接受，其签名也是**有生命周期的不透明令牌**——跨 provider、
    /// 跨压缩、跨长时间挂起后会失效。
    pub accepts_thinking_blocks: bool,

    /// 图像输入能力。
    pub image_input: ImageInputCapability,

    /// 是否要求相邻同角色消息合并。
    pub merges_adjacent_same_role: bool,

    /// 是否接受不透明的 provider 输出项重放。
    pub accepts_provider_items: bool,
}

impl ProviderCompat {
    /// Anthropic 族的默认兼容性。
    pub const ANTHROPIC: Self = Self {
        requires_tool_result_adjacency: true,
        accepts_thinking_blocks: true,
        image_input: ImageInputCapability::Supported,
        merges_adjacent_same_role: true,
        accepts_provider_items: false,
    };

    /// OpenAI Chat Completions 兼容端点（含 vLLM、DeepSeek 等）的默认兼容性。
    pub const OPENAI_COMPAT: Self = Self {
        requires_tool_result_adjacency: false,
        accepts_thinking_blocks: false,
        image_input: ImageInputCapability::Supported,
        merges_adjacent_same_role: false,
        accepts_provider_items: false,
    };

    /// 纯文本端点：不支持图像与 thinking。
    pub const TEXT_ONLY: Self = Self {
        requires_tool_result_adjacency: false,
        accepts_thinking_blocks: false,
        image_input: ImageInputCapability::Unsupported,
        merges_adjacent_same_role: false,
        accepts_provider_items: false,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 预置档位在同一处对照，避免逐条常量断言被优化成恒真。
    #[test]
    fn 预置档位的关键差异符合各厂商实际约束() {
        let 档位 = [
            ("anthropic", ProviderCompat::ANTHROPIC),
            ("openai_compat", ProviderCompat::OPENAI_COMPAT),
            ("text_only", ProviderCompat::TEXT_ONLY),
        ];
        let 需要紧邻: Vec<&str> = 档位
            .iter()
            .filter(|(_, c)| c.requires_tool_result_adjacency)
            .map(|(n, _)| *n)
            .collect();
        assert_eq!(需要紧邻, ["anthropic"], "只有 Anthropic 族要求 tool_result 紧邻");

        let 支持图像: Vec<&str> = 档位
            .iter()
            .filter(|(_, c)| c.image_input.supports_images())
            .map(|(n, _)| *n)
            .collect();
        assert_eq!(支持图像, ["anthropic", "openai_compat"]);

        let 接受思考: Vec<&str> = 档位
            .iter()
            .filter(|(_, c)| c.accepts_thinking_blocks)
            .map(|(n, _)| *n)
            .collect();
        assert_eq!(接受思考, ["anthropic"], "thinking 块只有 Anthropic 接受重放");
    }

    #[test]
    fn compat_是纯数据可复制() {
        // 它必须是值——配置由宿主传入，内核不读文件、不持有可变状态。
        let a = ProviderCompat::ANTHROPIC;
        let b = a;
        assert_eq!(a, b);
    }
}
