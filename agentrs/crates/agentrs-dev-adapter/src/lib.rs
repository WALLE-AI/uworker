//! # AgentRS Dev Adapter
//!
//! **这是开发工具，不是产品运行时。**
//!
//! 它存在的理由（架构 §3.1）：若严格止步于"真实执行必须由 Core 提供 adapter"，
//! 团队在 Phase A/B 期间将无法用 AgentRS 做任何真实工作，内核的实际可用性
//! 要等 AgentCore 就绪才被发现——那是一个可预见且可消除的反馈延迟。
//!
//! ## 它不放宽任何边界
//!
//! - grant 仍由 Policy 签发、由 Sandbox 独立复核（H1）；
//! - 隔离级别如实报告，达不到要求时失败而非降级（H2）；
//! - 事件写入幂等、epoch 围栏、checkpoint 写序（H3）；
//! - ChangeSet overlay 对同一 Run 一致，未提交不落盘（H7）。
//!
//! ## 它是 conformance suite 的第一个真实被测对象
//!
//! 因此套件本身也被验证——而不是只有一个内存 fake 陪跑。

#![forbid(unsafe_code)]

pub mod persistence;
pub mod policy;
pub mod sandbox;

pub use persistence::JsonlPersistence;
pub use policy::DevPolicy;
pub use sandbox::LocalFileSandbox;

/// 启动时打印的非生产横幅。
pub const NON_PRODUCTION_BANNER: &str = "\
┌──────────────────────────────────────────────────────────────┐
│  agentrs-dev-adapter —— 开发用参考实现，不是产品运行时        │
│  隔离级别仅 L0（基础围栏）；对外发布必须使用 SandboxRS        │
└──────────────────────────────────────────────────────────────┘";
