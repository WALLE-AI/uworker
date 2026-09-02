//! 把命令形状判定接到内核的 [`ToolGuard`] 扩展点上。
//!
//! **判定本身不在这里**，在 `agentrs_tools::builtin::shape_guard`——它是纯逻辑，
//! 属于工具语义的一部分，该和工具住在一起。本文件只剩接线：本 crate 依赖
//! `agentrs-runtime`（宿主装配方向），而 `agentrs-tools` 不该为了一个 trait 反过来
//! 依赖 runtime。
//!
//! guard 只有 `Deny` 与 `Abstain`，**没有 `Allow`**——这是内核不变量 15 的类型化
//! 表达：可插拔的东西只能让结论更严。

use agentrs_contracts::policy::DenyCode;
use agentrs_runtime::toolround::{GuardVerdict, ProposedCall, ToolGuard};

/// 命令形状 guard。
#[derive(Debug, Default, Clone, Copy)]
pub struct CommandShapeGuard;

impl ToolGuard for CommandShapeGuard {
    fn name(&self) -> &str {
        "command-shape"
    }

    fn check(&self, call: &ProposedCall) -> GuardVerdict {
        match agentrs_tools::builtin::bash::guard_verdict(&call.tool_name, &call.arguments) {
            // 不在这里补一句通用的建议：每条规则自己带的那句才对得上它拦的东西，
            // 通用句叠在后面只会变成一段自相重复的话，而模型与人都要读它。
            Some(why) => GuardVerdict::Deny {
                code: DenyCode::OutOfAuthority,
                message: why,
            },
            None => GuardVerdict::Abstain,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn 调用(tool: &str, args: serde_json::Value) -> ProposedCall {
        ProposedCall {
            call_id: "c1".into(),
            tool_name: tool.into(),
            arguments: args,
        }
    }

    #[test]
    fn 非_bash_工具一律弃权() {
        // guard 是全局装上去的，它必须对自己不懂的工具闭嘴。
        let call = 调用("Write", serde_json::json!({"path": "a", "content": "sudo rm -rf /"}));
        assert_eq!(CommandShapeGuard.check(&call), GuardVerdict::Abstain);
    }

    #[test]
    fn 正常命令弃权而不是放行() {
        // 弃权不等于放行——最终是否放行只由 Policy 决定。
        let call = 调用("Bash", serde_json::json!({"command": "cargo test"}));
        assert_eq!(CommandShapeGuard.check(&call), GuardVerdict::Abstain);
    }

    #[test]
    fn 拒绝带得出是哪个_guard_拦的以及为什么() {
        let call = 调用("Bash", serde_json::json!({"command": "sudo rm -rf /"}));
        assert_eq!(CommandShapeGuard.name(), "command-shape");
        let GuardVerdict::Deny { code, message } = CommandShapeGuard.check(&call) else {
            panic!("应当拒绝");
        };
        assert_eq!(code, DenyCode::OutOfAuthority);
        assert!(message.contains("提权"), "{message}");
        // 理由只说一遍该怎么办：调用方从前会在末尾再补一句通用建议。
        assert_eq!(message.matches("自己在终端").count(), 1, "{message}");
    }
}
