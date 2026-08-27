//! # AgentRS Testkit
//!
//! fake port、确定性时钟、故障注入，以及**host conformance suite**。
//!
//! 内核单测不需要网络、文件系统、真实时钟或 OS 进程——这条既是测试要求，
//! 也是架构 §1.1 的边界判据。本 crate 提供做到这一点所需的替身。
//!
//! ## 当前进度
//!
//! - ✅ [`persistence`] —— fake Persistence，含幂等、epoch 围栏、写序（宿主义务 H3）
//! - ✅ [`clock`] —— 确定性时钟
//! - ✅ [`content`] —— fake ContentStore，含 retain/GC 与三类降级故障（宿主义务 H4）
//! - ✅ [`policy`] —— 可编排 fake PolicyEnforcer
//! - ✅ [`sandbox`] —— fake Sandbox，含 hash 复核、grant 一次性消费、
//!   三态 reconcile 与崩溃注入（宿主义务 H1/H2）
//! - ⬜ interleaving scheduler、完整 conformance suite

#![forbid(unsafe_code)]

pub mod clock;
pub mod conformance;
pub mod content;
pub mod hooks;
pub mod persistence;
pub mod policy;
pub mod sandbox;

pub use clock::FakeClock;
pub use conformance::content::{check_content, ContentSubject};
pub use conformance::persistence::{check_persistence, PersistenceSubject};
pub use conformance::policy::{check_policy, PolicySubject};
pub use conformance::{check_sandbox, Check, Outcome, Report, SandboxSubject};
pub use content::{ContentFault, FakeContentStore};
pub use hooks::FakeHooks;
pub use persistence::{FakePersistence, InjectedFault};
pub use policy::FakePolicy;
pub use sandbox::{FakeSandbox, ScriptedExecution};
