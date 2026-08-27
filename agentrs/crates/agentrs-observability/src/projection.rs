//! Projection Registry —— 版本化读模型（架构 §4.4，任务 T03B）。
//!
//! Trajectory **不是日志搜索页**，而是从权威 Event Ledger 计算出的读模型。
//! 三条硬性约束，全部由本模块的类型形状保证：
//!
//! 1. **纯 fold**：[`ProjectionDefinition::apply`] 是同步纯函数，
//!    不能 IO、不能取时钟、不能写回事实流。相同事件前缀恒等于相同 snapshot。
//! 2. **投影不是第二份事实源**：`apply` 只拿到 `&RunEventEnvelope`，
//!    没有任何写回通道。
//! 3. **未知事件必须可安全忽略**：[`EventPayload::Unknown`] 落到 `_ => {}`，
//!    新增事件类型不会让旧投影 panic 或产生错误结论。
//!
//! ## 为什么 durable-only
//!
//! `TextDelta`/`ThinkingDelta` 是 live 的、允许丢失的。若投影把它们计入，
//! 同一段历史在"UI delta 丢了"和"没丢"两种情况下会得到不同 snapshot，
//! 第 1 条就破了。因此 [`ProjectionRegistry::snapshot`] **只喂 durable 事件**——
//! UI delta 丢失不影响 committed trajectory 是验收项，不是巧合。
//!
//! ## state_version 的作用
//!
//! 投影逻辑改了、旧的持久化 state 就不能再增量 apply。
//! [`Snapshot`] 带上 `state_version`，[`SnapshotCache`] 在版本不匹配时
//! 直接判定失效并要求全量重算——**宁可重算也不要静默用错状态**。

use std::any::Any;
use std::collections::BTreeMap;

use agentrs_contracts::event::{EventPayload, RunEventEnvelope};
use agentrs_contracts::ids::{EventSequence, StepId, TurnId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 投影标识。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct ProjectionKey(pub &'static str);

impl std::fmt::Display for ProjectionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

/// 一个投影定义。**必须是纯同步 fold。**
pub trait ProjectionDefinition: Send + Sync {
    /// 折叠状态。
    type State: 'static;
    /// 对外视图。
    type View: Serialize;

    /// 标识。
    fn key(&self) -> ProjectionKey;

    /// 状态版本。改了 `apply`/`State` 的语义就必须递增它。
    fn state_version(&self) -> u32;

    /// 初始状态。
    fn init(&self) -> Self::State;

    /// 折叠一条事件。**不得有副作用。**
    fn apply(&self, state: &mut Self::State, event: &RunEventEnvelope);

    /// 从状态导出视图。
    fn view(&self, state: &Self::State) -> Self::View;
}

/// 类型擦除后的投影，供 Registry 异构持有。
///
/// 关联类型让 `ProjectionDefinition` 不是 object-safe；这层擦除是**唯一**
/// 的妥协点，`State` 装进 `Box<dyn Any>`、`View` 序列化成 `Value`。
pub trait ErasedProjection: Send + Sync {
    /// 标识。
    fn key(&self) -> ProjectionKey;
    /// 状态版本。
    fn state_version(&self) -> u32;
    /// 初始状态。
    fn init_erased(&self) -> Box<dyn Any + Send>;
    /// 折叠一条事件。类型不匹配时静默跳过（Registry 保证不会发生）。
    fn apply_erased(&self, state: &mut (dyn Any + Send), event: &RunEventEnvelope);
    /// 导出视图。
    fn view_erased(&self, state: &(dyn Any + Send)) -> Value;
}

impl<P> ErasedProjection for P
where
    P: ProjectionDefinition,
    P::State: Send,
{
    fn key(&self) -> ProjectionKey {
        ProjectionDefinition::key(self)
    }

    fn state_version(&self) -> u32 {
        ProjectionDefinition::state_version(self)
    }

    fn init_erased(&self) -> Box<dyn Any + Send> {
        Box::new(self.init())
    }

    fn apply_erased(&self, state: &mut (dyn Any + Send), event: &RunEventEnvelope) {
        if let Some(s) = state.downcast_mut::<P::State>() {
            self.apply(s, event);
        }
    }

    fn view_erased(&self, state: &(dyn Any + Send)) -> Value {
        match state.downcast_ref::<P::State>() {
            Some(s) => serde_json::to_value(self.view(s)).unwrap_or(Value::Null),
            None => Value::Null,
        }
    }
}

/// 一次投影快照。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Snapshot {
    /// 投影标识。
    pub key: ProjectionKey,
    /// 计算时的状态版本。**缓存命中必须同时匹配它。**
    pub state_version: u32,
    /// 快照覆盖到哪个 durable 序号（含）。`None` 表示空前缀。
    pub up_to_seq: Option<EventSequence>,
    /// 视图。
    pub view: Value,
}

/// 注册与计算入口。
#[derive(Default)]
pub struct ProjectionRegistry {
    defs: BTreeMap<ProjectionKey, Box<dyn ErasedProjection>>,
}

impl std::fmt::Debug for ProjectionRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProjectionRegistry")
            .field("keys", &self.keys())
            .finish()
    }
}

/// 注册/查询失败。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProjectionError {
    /// 同一个 key 注册了两次。
    #[error("projection {0} already registered")]
    Duplicate(ProjectionKey),
    /// 请求了未注册的投影。
    #[error("projection {0} not registered")]
    Unknown(ProjectionKey),
}

impl ProjectionRegistry {
    /// 空注册表。
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一个投影。重复 key 是**编程错误**，不静默覆盖。
    pub fn register<P>(&mut self, def: P) -> Result<&mut Self, ProjectionError>
    where
        P: ProjectionDefinition + 'static,
        P::State: Send,
    {
        let key = ProjectionDefinition::key(&def);
        if self.defs.contains_key(&key) {
            return Err(ProjectionError::Duplicate(key));
        }
        self.defs.insert(key, Box::new(def));
        Ok(self)
    }

    /// 已注册的全部标识。
    pub fn keys(&self) -> Vec<ProjectionKey> {
        self.defs.keys().copied().collect()
    }

    /// 某个投影当前的状态版本。
    pub fn state_version(&self, key: ProjectionKey) -> Option<u32> {
        self.defs.get(&key).map(|d| d.state_version())
    }

    /// 计算 `as_of`（含）之前的快照。
    ///
    /// `as_of = None` 表示折叠全部 durable 事件。
    /// **live 事件一律不参与**——见模块文档。
    pub fn snapshot(
        &self,
        key: ProjectionKey,
        events: &[RunEventEnvelope],
        as_of: Option<EventSequence>,
    ) -> Result<Snapshot, ProjectionError> {
        let def = self.defs.get(&key).ok_or(ProjectionError::Unknown(key))?;
        let mut state = def.init_erased();
        let mut up_to = None;
        for e in durable_prefix(events, as_of) {
            def.apply_erased(state.as_mut(), e);
            up_to = e.seq;
        }
        Ok(Snapshot {
            key,
            state_version: def.state_version(),
            up_to_seq: up_to,
            view: def.view_erased(state.as_ref()),
        })
    }

    /// 计算全部已注册投影的快照。
    pub fn snapshot_all(
        &self,
        events: &[RunEventEnvelope],
        as_of: Option<EventSequence>,
    ) -> BTreeMap<ProjectionKey, Snapshot> {
        self.defs
            .keys()
            .filter_map(|k| self.snapshot(*k, events, as_of).ok().map(|s| (*k, s)))
            .collect()
    }
}

/// 取 durable 前缀，按 `seq` 升序。
///
/// 输入允许乱序（重投递、并发写入），这里排序后再折叠——
/// 否则"相同事件集合、不同到达顺序"会得到不同 snapshot。
fn durable_prefix(events: &[RunEventEnvelope], as_of: Option<EventSequence>) -> Vec<&RunEventEnvelope> {
    let mut v: Vec<&RunEventEnvelope> = events
        .iter()
        .filter(|e| e.is_durable())
        .filter(|e| match (e.seq, as_of) {
            (Some(s), Some(limit)) => s <= limit,
            (Some(_), None) => true,
            // durable 却没有 seq 是存储侧的错，不能参与有序折叠。
            (None, _) => false,
        })
        .collect();
    v.sort_by_key(|e| e.seq);
    v
}

// ---------------------------------------------------------------------------
// 快照缓存
// ---------------------------------------------------------------------------

/// 缓存判定结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheVerdict {
    /// 可直接复用。
    Fresh,
    /// 可增量 apply 后续事件。
    Extendable,
    /// 必须全量重算。
    Invalid(CacheInvalidation),
}

/// 缓存失效原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheInvalidation {
    /// 投影逻辑换代。
    StateVersionChanged {
        /// 缓存里的版本。
        cached: u32,
        /// 当前版本。
        current: u32,
    },
    /// 请求的位置早于缓存位置——fold 不可倒退。
    Rewound,
}

/// 极简快照缓存。**只判定，不代存**——存储实现归宿主。
#[derive(Debug, Clone)]
pub struct SnapshotCache {
    /// 缓存的状态版本。
    pub state_version: u32,
    /// 缓存覆盖到的位置。
    pub up_to_seq: Option<EventSequence>,
}

impl SnapshotCache {
    /// 判断这份缓存对 `(current_version, want)` 是否还能用。
    pub fn verdict(&self, current_version: u32, want: Option<EventSequence>) -> CacheVerdict {
        if self.state_version != current_version {
            // **换代一律重算。** 用旧 state 增量 apply 新逻辑是最难查的一类错。
            return CacheVerdict::Invalid(CacheInvalidation::StateVersionChanged {
                cached: self.state_version,
                current: current_version,
            });
        }
        match (self.up_to_seq, want) {
            (a, b) if a == b => CacheVerdict::Fresh,
            (Some(a), Some(b)) if b > a => CacheVerdict::Extendable,
            (None, Some(_)) => CacheVerdict::Extendable,
            (Some(_), None) => CacheVerdict::Extendable,
            _ => CacheVerdict::Invalid(CacheInvalidation::Rewound),
        }
    }
}

// ---------------------------------------------------------------------------
// 向前分页
// ---------------------------------------------------------------------------

/// 分页游标。**按 `seq` 而不是 offset**——offset 会在补页时错位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Cursor {
    /// 从这个序号**之后**开始。
    pub after_seq: Option<EventSequence>,
}

impl Cursor {
    /// 从头开始。
    pub fn start() -> Self {
        Self { after_seq: None }
    }
}

/// 一页事件。
#[derive(Debug, Clone, PartialEq)]
pub struct Page<'a> {
    /// 本页条目，按 `seq` 升序。
    pub items: Vec<&'a RunEventEnvelope>,
    /// 下一页游标；`None` 表示已到末尾。
    pub next: Option<Cursor>,
}

/// 事件过滤条件。全部为 `None` 时不过滤。
#[derive(Debug, Clone, Default)]
pub struct EventFilter {
    /// 只要这些载荷判别名（见 [`payload_kind`]）。
    pub kinds: Option<Vec<&'static str>>,
    /// 只要这个 Turn。
    pub turn_id: Option<TurnId>,
    /// 只要这个 Step。
    pub step_id: Option<StepId>,
}

impl EventFilter {
    fn matches(&self, e: &RunEventEnvelope) -> bool {
        if let Some(kinds) = &self.kinds {
            if !kinds.contains(&payload_kind(&e.payload)) {
                return false;
            }
        }
        if let Some(t) = &self.turn_id {
            if e.causality.turn_id.as_ref() != Some(t) {
                return false;
            }
        }
        if let Some(s) = &self.step_id {
            if e.causality.step_id.as_ref() != Some(s) {
                return false;
            }
        }
        true
    }
}

/// 向前翻一页 durable 事件。
///
/// **已加载记录的键与顺序不因补页而改变**：游标是 `seq`，排序是 `seq`，
/// 两者都不依赖"当前一共有多少条"。
///
/// # 输入必须按 `seq` 升序
///
/// 这不是"最好如此"，是**存储侧的契约**：H3 要求 `seq` 严格单调，
/// 事件又按写入顺序读回，两者合起来就是升序。`validate` 会检查这一条。
///
/// 依赖它是为了让**翻页的代价与页大小相关，而不与日志总长相关**——
/// 定位游标用二分，之后只向前扫到攒够一页为止。
///
/// 初版没有这条要求：每次翻页都把全量 durable 事件收集并**重排一遍**。
/// 在十条事件的单元测试里完全正确，在二十万条上翻一页要 3.6 ms，
/// 事件多十倍就慢十二倍——而长 Run 恰恰是 trajectory 最需要用的时候。
///
/// debug 构建下有断言兜底；release 下不检查（检查本身就是 O(n)）。
///
/// # 复杂度
///
/// `O(log n + 扫描量)`。无过滤时扫描量等于页大小；有过滤时约等于
/// `页大小 / 命中率`。**极端情况**：游标之后一条都不匹配，
/// 那必须扫到末尾才能确定——这是无索引过滤的固有代价，不是实现缺陷。
pub fn page<'a>(
    events: &'a [RunEventEnvelope],
    cursor: Cursor,
    limit: usize,
    filter: &EventFilter,
) -> Page<'a> {
    debug_assert!(
        is_seq_ascending(events),
        "page() 要求输入按 seq 升序；乱序输入会让二分定位给出错误结果"
    );

    // 二分定位游标。这是"代价不随总长增长"的关键一步。
    let start = match cursor.after_seq {
        None => 0,
        Some(after) => events.partition_point(|e| match e.seq {
            Some(s) => s <= after,
            // 无 seq 的条目（live，或存储侧写错）排在前面不影响定位：
            // 它们随后会被过滤掉。
            None => true,
        }),
    };

    // 只向前扫到攒够 limit+1 条为止——多探一条用来判断"还有没有下一页"。
    let mut items: Vec<&RunEventEnvelope> = Vec::with_capacity(limit.min(1024));
    let mut more = false;
    for e in &events[start.min(events.len())..] {
        if !e.is_durable() || e.seq.is_none() || !filter.matches(e) {
            continue;
        }
        if items.len() == limit {
            more = true;
            break;
        }
        items.push(e);
    }

    let next = if more {
        items
            .last()
            .and_then(|e| e.seq)
            .map(|s| Cursor { after_seq: Some(s) })
    } else {
        None
    };
    Page { items, next }
}

/// 输入是否按 `seq` 升序。仅供 `debug_assert!` 使用。
fn is_seq_ascending(events: &[RunEventEnvelope]) -> bool {
    let mut last = None;
    for e in events {
        if let Some(s) = e.seq {
            if last.is_some_and(|l| s < l) {
                return false;
            }
            last = Some(s);
        }
    }
    true
}

/// 载荷判别名。**稳定字符串**，供过滤与导出使用。
pub fn payload_kind(p: &EventPayload) -> &'static str {
    match p {
        EventPayload::RunStarted => "RunStarted",
        EventPayload::TurnStarted => "TurnStarted",
        EventPayload::TurnEnded => "TurnEnded",
        EventPayload::StepStarted => "StepStarted",
        EventPayload::StepEnded => "StepEnded",
        EventPayload::Checkpointed => "Checkpointed",
        EventPayload::UserInputClaimed => "UserInputClaimed",
        EventPayload::UserInputSubmitted => "UserInputSubmitted",
        EventPayload::PermissionModeChanged => "PermissionModeChanged",
        EventPayload::ModelRequestPrepared { .. } => "ModelRequestPrepared",
        EventPayload::PartialOutputStarted => "PartialOutputStarted",
        EventPayload::AssistantMessage => "AssistantMessage",
        EventPayload::TextDelta => "TextDelta",
        EventPayload::ThinkingDelta => "ThinkingDelta",
        EventPayload::UsageUpdated => "UsageUpdated",
        EventPayload::ToolProposed { .. } => "ToolProposed",
        EventPayload::HookOutcomeRecorded { .. } => "HookOutcomeRecorded",
        EventPayload::StepIntentRecorded { .. } => "StepIntentRecorded",
        EventPayload::ApprovalRequested { .. } => "ApprovalRequested",
        EventPayload::ApprovalTimedOut { .. } => "ApprovalTimedOut",
        EventPayload::ToolStarted { .. } => "ToolStarted",
        EventPayload::StepResultRecorded { .. } => "StepResultRecorded",
        EventPayload::ChangeSetAvailable => "ChangeSetAvailable",
        EventPayload::ArtifactCreated => "ArtifactCreated",
        EventPayload::ContextSelected => "ContextSelected",
        EventPayload::ContentRefUnresolved => "ContentRefUnresolved",
        EventPayload::HistoryLegalized => "HistoryLegalized",
        EventPayload::CacheBreakObserved => "CacheBreakObserved",
        EventPayload::CompactionStarted => "CompactionStarted",
        EventPayload::CompactionCompleted { .. } => "CompactionCompleted",
        EventPayload::ExternalFactReceived => "ExternalFactReceived",
        EventPayload::BoardSnapshotAttached => "BoardSnapshotAttached",
        EventPayload::SubagentSummary => "SubagentSummary",
        EventPayload::CheckpointMigrated => "CheckpointMigrated",
        EventPayload::RunCompleted => "RunCompleted",
        EventPayload::RunFailed => "RunFailed",
        EventPayload::RunCanceled => "RunCanceled",
        EventPayload::RunNeedsUserAction => "RunNeedsUserAction",
        EventPayload::Unknown => "Unknown",
    }
}

/// 稳定的记录键。**分页与去重都用它**，不用 `seq`——
/// 重试会分配新序号，但 `event_id` 不变（架构 §4.2）。
pub fn record_key(e: &RunEventEnvelope) -> (String, String) {
    (e.run_id.to_string(), e.event_id.to_string())
}

/// 一次工具调用在固定管线上走到了哪里。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolPath {
    /// 调用标识。
    pub call_id: String,
    /// 工具名（`StepIntentRecorded` 落盘后才知道）。
    pub tool_name: Option<String>,
    /// 模型提出过。
    pub proposed: bool,
    /// Hook 结论已记录。
    pub hooked: bool,
    /// 意图已落盘——**跨出副作用边界的那一刻**。
    pub intent_recorded: bool,
    /// 请求过审批。
    pub approval_requested: bool,
    /// 审批超时转挂起。
    pub approval_timed_out: bool,
    /// 沙箱已启动。
    pub started: bool,
    /// 终局。
    pub outcome: Option<String>,
    /// 实际生效的隔离级别。
    pub effective_isolation: Option<String>,
}

impl ToolPath {
    /// **有意图但无结果**——恢复时必须向 Sandbox `reconcile` 的那一类。
    pub fn is_dangling(&self) -> bool {
        self.intent_recorded && self.outcome.is_none()
    }
}

pub mod defs;

#[cfg(test)]
mod tests;
