//! Phase D 组件 generation、Inventory 与事务式 profile 换代。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use agentrs_contracts::authority::CapabilityView;
use agentrs_contracts::component::{ComponentManifest, ComponentScope, ComponentTrust, Generation};
use agentrs_contracts::ids::{ComponentId, ScopeId};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::composition::{AsyncCleanup, CleanupError, RegisterError, ResourceOwner};

/// 当前支持的 Component API。
pub const COMPONENT_API_VERSION: u32 = 1;

/// 一个 profile 中的声明式组件配置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentCandidate {
    /// 组件清单。
    pub manifest: ComponentManifest,
    /// 已由 Core 去除凭据后的配置。
    pub config: serde_json::Value,
}

/// 带单调 revision 的完整目标 profile。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompositionProfile {
    /// Core 分配的单调 revision；旧 revision 不得覆盖新树。
    pub revision: u64,
    /// 完整目标集合，不在其中的当前组件会被撤回并 drain。
    pub components: Vec<ComponentCandidate>,
}

/// Candidate 准备端口。真实进程、凭据、网络与资源限制仍归 Core/Sandbox。
#[async_trait]
pub trait CandidateFactory: Send + Sync {
    /// 私下准备并完成健康检查。资源必须先登记到 owner 再对外可见。
    async fn prepare(
        &self,
        candidate: &ComponentCandidate,
        generation: Generation,
        owner: Arc<ResourceOwner>,
    ) -> Result<(), CandidateError>;
}

/// Candidate 失败，只携带稳定码。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("candidate preparation failed: {code}")]
pub struct CandidateError {
    /// 脱敏稳定码。
    pub code: String,
}

/// Profile 校验或换代错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GenerationError {
    /// revision 已过期或重复。
    #[error("stale profile revision: {revision} <= {committed}")]
    StaleRevision {
        /// 请求 revision。
        revision: u64,
        /// 当前 revision。
        committed: u64,
    },
    /// 清单字段无效。
    #[error("invalid component manifest {component}: {code}")]
    InvalidManifest {
        /// 组件。
        component: ComponentId,
        /// 稳定码。
        code: &'static str,
    },
    /// 依赖缺失。
    #[error("component {component} requires missing component {dependency}")]
    MissingDependency {
        /// 组件。
        component: ComponentId,
        /// 缺失依赖。
        dependency: ComponentId,
    },
    /// 依赖形成环。
    #[error("component dependency cycle")]
    DependencyCycle,
    /// 两个组件声明同一 capability。
    #[error("capability provided by multiple components: {0}")]
    CapabilityConflict(String),
    /// 清单请求了当前 Run 没有的能力。
    #[error("component {component} requests unavailable {field}")]
    CapabilityWidening {
        /// 组件。
        component: ComponentId,
        /// 能力维度。
        field: &'static str,
    },
    /// candidate 健康检查失败，旧树保持不变。
    #[error("candidate {component} failed: {code}")]
    CandidateFailed {
        /// 组件。
        component: ComponentId,
        /// 稳定失败码。
        code: String,
    },
    /// owner 已在停止。
    #[error("composition owner is stopping")]
    OwnerStopping,
    /// generation 空间耗尽。
    #[error("component generation exhausted")]
    GenerationExhausted,
    /// operation 请求了未启用组件。
    #[error("component is not active: {0}")]
    NotActive(ComponentId),
}

impl From<RegisterError> for GenerationError {
    fn from(_: RegisterError) -> Self {
        Self::OwnerStopping
    }
}

/// Inventory 中的生命周期。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InventoryState {
    /// 新 operation 可见。
    Active,
    /// 已撤回，等待旧 operation 结算。
    Draining,
}

/// 权威状态的只读库存项。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventoryEntry {
    /// 组件 id。
    pub component_id: ComponentId,
    /// 来源。
    pub source: String,
    /// 实现版本。
    pub version: String,
    /// 组件角色。
    pub kind: agentrs_contracts::component::ComponentKind,
    /// 信任边界。
    pub trust: ComponentTrust,
    /// 生命周期 scope。
    pub scope: ComponentScope,
    /// owner scope id。
    pub owner_scope: ScopeId,
    /// 当前状态。
    pub state: InventoryState,
    /// 代际。
    pub generation: Generation,
    /// 依赖。
    pub dependencies: Vec<ComponentId>,
    /// 注册能力。
    pub registrations: Vec<String>,
    /// 尚未结算的 operation 数。
    pub in_flight: u64,
}

/// Composition Kernel 的只读快照，不是第二份可写状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentInventory {
    /// 已提交 profile revision。
    pub revision: u64,
    /// active 与 draining 项。
    pub entries: Vec<InventoryEntry>,
    /// 最近一次 candidate 失败稳定码。
    pub last_failure: Option<String>,
}

struct ActiveComponent {
    candidate: ComponentCandidate,
    generation: Generation,
    owner: Arc<ResourceOwner>,
    in_flight: u64,
}

struct GenerationState {
    revision: u64,
    next_generation: Generation,
    active: BTreeMap<ComponentId, ActiveComponent>,
    draining: Vec<ActiveComponent>,
    last_failure: Option<String>,
}

/// 串行提交 profile、固定 operation 依赖视图的管理器。
pub struct GenerationManager {
    root_owner: Arc<ResourceOwner>,
    state: Mutex<GenerationState>,
}

impl GenerationManager {
    /// 创建空组合树。
    pub fn new(root_owner: Arc<ResourceOwner>) -> Arc<Self> {
        Arc::new(Self {
            root_owner,
            state: Mutex::new(GenerationState {
                revision: 0,
                next_generation: Generation(1),
                active: BTreeMap::new(),
                draining: Vec::new(),
                last_failure: None,
            }),
        })
    }

    /// 私下准备全部变化项并原子提交。失败时清理 candidate、保留旧树。
    pub async fn apply_profile(
        &self,
        profile: CompositionProfile,
        capabilities: &CapabilityView,
        factory: &dyn CandidateFactory,
    ) -> Result<ComponentInventory, GenerationError> {
        let mut state = self.state.lock().await;
        if profile.revision <= state.revision {
            return Err(GenerationError::StaleRevision {
                revision: profile.revision,
                committed: state.revision,
            });
        }
        let (desired, order) = validate_profile(&profile, capabilities)?;
        let mut changed: BTreeSet<ComponentId> = desired
            .iter()
            .filter(|(id, candidate)| {
                state
                    .active
                    .get(*id)
                    .is_none_or(|active| active.candidate != **candidate)
            })
            .map(|(id, _)| id.clone())
            .collect();
        loop {
            let dependents: Vec<ComponentId> = desired
                .iter()
                .filter(|(id, candidate)| {
                    !changed.contains(*id)
                        && candidate
                            .manifest
                            .requires
                            .iter()
                            .any(|dependency| changed.contains(dependency))
                })
                .map(|(id, _)| id.clone())
                .collect();
            if dependents.is_empty() {
                break;
            }
            changed.extend(dependents);
        }

        let changed_count = u64::try_from(changed.len()).map_err(|_| GenerationError::GenerationExhausted)?;
        state
            .next_generation
            .0
            .checked_add(changed_count)
            .ok_or(GenerationError::GenerationExhausted)?;

        let mut prepared = BTreeMap::new();
        for id in order.into_iter().filter(|id| changed.contains(id)) {
            let candidate = desired
                .get(&id)
                .expect("validated order references desired component");
            let generation = state.next_generation;
            state.next_generation = generation
                .checked_next()
                .expect("generation range reserved before prepare");
            let owner = self
                .root_owner
                .child(format!("component:{}:{}", id.as_str(), generation.0))
                .await?;
            if let Err(error) = factory.prepare(candidate, generation, owner.clone()).await {
                owner.shutdown().await;
                for item in prepared.values() {
                    let item: &ActiveComponent = item;
                    item.owner.shutdown().await;
                }
                let code = error.code;
                state.last_failure = Some(format!("{}:{code}", id.as_str()));
                return Err(GenerationError::CandidateFailed { component: id, code });
            }
            prepared.insert(
                id,
                ActiveComponent {
                    candidate: candidate.clone(),
                    generation,
                    owner,
                    in_flight: 0,
                },
            );
        }

        let old_ids: Vec<ComponentId> = state.active.keys().cloned().collect();
        for id in old_ids {
            if !desired.contains_key(&id) || changed.contains(&id) {
                if let Some(old) = state.active.remove(&id) {
                    state.draining.push(old);
                }
            }
        }
        state.active.extend(prepared);
        state.revision = profile.revision;
        state.last_failure = None;
        drain_ready(&mut state).await;
        Ok(inventory_from(&state))
    }

    /// 为一次 operation 固定依赖代际并增加引用计数。
    pub async fn begin_operation(
        self: &Arc<Self>,
        roots: &[ComponentId],
        operation_owner: &Arc<ResourceOwner>,
    ) -> Result<CommittedGenerationView, GenerationError> {
        let mut state = self.state.lock().await;
        let mut selected = BTreeSet::new();
        for root in roots {
            collect_dependencies(root, &state.active, &mut selected)?;
        }
        let mut generations = BTreeMap::new();
        for id in &selected {
            let active = state
                .active
                .get_mut(id)
                .ok_or_else(|| GenerationError::NotActive(id.clone()))?;
            active.in_flight += 1;
            generations.insert(id.clone(), active.generation);
        }
        if let Err(error) = operation_owner
            .register(
                "component-generation-lease",
                Box::new(OperationSettlement {
                    manager: self.clone(),
                    generations: generations.clone(),
                }),
            )
            .await
        {
            self.settle(&generations).await;
            return Err(error.into());
        }
        Ok(CommittedGenerationView { generations })
    }

    /// 从权威 live 状态即时生成 Inventory。
    pub async fn inventory(&self) -> ComponentInventory {
        let state = self.state.lock().await;
        inventory_from(&state)
    }

    async fn settle(&self, generations: &BTreeMap<ComponentId, Generation>) {
        let mut state = self.state.lock().await;
        for (id, generation) in generations {
            if let Some(active) = state
                .active
                .get_mut(id)
                .filter(|active| active.generation == *generation)
            {
                active.in_flight = active.in_flight.saturating_sub(1);
                continue;
            }
            if let Some(draining) = state
                .draining
                .iter_mut()
                .find(|item| item.candidate.manifest.id == *id && item.generation == *generation)
            {
                draining.in_flight = draining.in_flight.saturating_sub(1);
            }
        }
        drain_ready(&mut state).await;
    }
}

/// 一次 operation 的不可变 committed generation view。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommittedGenerationView {
    generations: BTreeMap<ComponentId, Generation>,
}

impl CommittedGenerationView {
    /// 固定的 generation 元组，可直接写入 ModelRequestManifest。
    pub fn generations(&self) -> &BTreeMap<ComponentId, Generation> {
        &self.generations
    }
}

struct OperationSettlement {
    manager: Arc<GenerationManager>,
    generations: BTreeMap<ComponentId, Generation>,
}

#[async_trait]
impl AsyncCleanup for OperationSettlement {
    async fn cleanup(self: Box<Self>) -> Result<(), CleanupError> {
        self.manager.settle(&self.generations).await;
        Ok(())
    }
}

fn validate_profile(
    profile: &CompositionProfile,
    capabilities: &CapabilityView,
) -> Result<(BTreeMap<ComponentId, ComponentCandidate>, Vec<ComponentId>), GenerationError> {
    let mut desired = BTreeMap::new();
    let mut provided = BTreeSet::new();
    for candidate in &profile.components {
        let manifest = &candidate.manifest;
        if manifest.id.as_str().trim().is_empty()
            || manifest.source.trim().is_empty()
            || manifest.version.trim().is_empty()
            || !manifest.config_schema.is_object()
        {
            return Err(invalid(&manifest.id, "required_field"));
        }
        if manifest.api_version != COMPONENT_API_VERSION {
            return Err(invalid(&manifest.id, "api_version"));
        }
        if manifest.kind == agentrs_contracts::component::ComponentKind::External
            && manifest.trust != ComponentTrust::ExternalSandboxed
        {
            return Err(invalid(&manifest.id, "external_trust"));
        }
        validate_config(&manifest.id, &manifest.config_schema, &candidate.config)?;
        ensure_subset(
            &manifest.id,
            "tools",
            &manifest.requested_capabilities.tools,
            &capabilities.tools,
        )?;
        ensure_subset(
            &manifest.id,
            "providers",
            &manifest.requested_capabilities.providers,
            &capabilities.providers,
        )?;
        ensure_subset(
            &manifest.id,
            "models",
            &manifest.requested_capabilities.models,
            &capabilities.models,
        )?;
        for registration in &manifest.provides {
            if registration.trim().is_empty() {
                return Err(invalid(&manifest.id, "empty_registration"));
            }
            if !provided.insert(registration.clone()) {
                return Err(GenerationError::CapabilityConflict(registration.clone()));
            }
        }
        if desired.insert(manifest.id.clone(), candidate.clone()).is_some() {
            return Err(invalid(&manifest.id, "duplicate_id"));
        }
    }
    for (id, candidate) in &desired {
        for dependency in &candidate.manifest.requires {
            let Some(required) = desired.get(dependency) else {
                return Err(GenerationError::MissingDependency {
                    component: id.clone(),
                    dependency: dependency.clone(),
                });
            };
            if candidate.manifest.scope == ComponentScope::Process
                && required.manifest.scope == ComponentScope::Run
            {
                return Err(invalid(id, "process_depends_on_run"));
            }
        }
    }
    let order = topological_order(&desired)?;
    Ok((desired, order))
}

fn topological_order(
    desired: &BTreeMap<ComponentId, ComponentCandidate>,
) -> Result<Vec<ComponentId>, GenerationError> {
    fn visit(
        id: &ComponentId,
        desired: &BTreeMap<ComponentId, ComponentCandidate>,
        visiting: &mut BTreeSet<ComponentId>,
        visited: &mut BTreeSet<ComponentId>,
        order: &mut Vec<ComponentId>,
    ) -> Result<(), GenerationError> {
        if visited.contains(id) {
            return Ok(());
        }
        if !visiting.insert(id.clone()) {
            return Err(GenerationError::DependencyCycle);
        }
        for dependency in &desired[id].manifest.requires {
            visit(dependency, desired, visiting, visited, order)?;
        }
        visiting.remove(id);
        visited.insert(id.clone());
        order.push(id.clone());
        Ok(())
    }
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    let mut order = Vec::new();
    for id in desired.keys() {
        visit(id, desired, &mut visiting, &mut visited, &mut order)?;
    }
    Ok(order)
}

fn validate_config(
    id: &ComponentId,
    schema: &serde_json::Value,
    config: &serde_json::Value,
) -> Result<(), GenerationError> {
    if schema.get("type").and_then(serde_json::Value::as_str) == Some("object") && !config.is_object() {
        return Err(invalid(id, "config_type"));
    }
    if let Some(required) = schema.get("required").and_then(serde_json::Value::as_array) {
        let object = config.as_object().ok_or_else(|| invalid(id, "config_type"))?;
        if required
            .iter()
            .filter_map(serde_json::Value::as_str)
            .any(|field| !object.contains_key(field))
        {
            return Err(invalid(id, "config_required"));
        }
    }
    Ok(())
}

fn ensure_subset<T: Ord>(
    component: &ComponentId,
    field: &'static str,
    requested: &[T],
    available: &[T],
) -> Result<(), GenerationError> {
    let available: BTreeSet<&T> = available.iter().collect();
    if requested.iter().all(|item| available.contains(item)) {
        Ok(())
    } else {
        Err(GenerationError::CapabilityWidening {
            component: component.clone(),
            field,
        })
    }
}

fn invalid(component: &ComponentId, code: &'static str) -> GenerationError {
    GenerationError::InvalidManifest {
        component: component.clone(),
        code,
    }
}

fn collect_dependencies(
    id: &ComponentId,
    active: &BTreeMap<ComponentId, ActiveComponent>,
    selected: &mut BTreeSet<ComponentId>,
) -> Result<(), GenerationError> {
    let component = active
        .get(id)
        .ok_or_else(|| GenerationError::NotActive(id.clone()))?;
    if selected.insert(id.clone()) {
        for dependency in &component.candidate.manifest.requires {
            collect_dependencies(dependency, active, selected)?;
        }
    }
    Ok(())
}

async fn drain_ready(state: &mut GenerationState) {
    let mut remaining = Vec::new();
    for item in std::mem::take(&mut state.draining) {
        if item.in_flight == 0 {
            item.owner.shutdown().await;
        } else {
            remaining.push(item);
        }
    }
    state.draining = remaining;
}

fn inventory_from(state: &GenerationState) -> ComponentInventory {
    let active = state
        .active
        .values()
        .map(|item| inventory_entry(item, InventoryState::Active));
    let draining = state
        .draining
        .iter()
        .map(|item| inventory_entry(item, InventoryState::Draining));
    ComponentInventory {
        revision: state.revision,
        entries: active.chain(draining).collect(),
        last_failure: state.last_failure.clone(),
    }
}

fn inventory_entry(item: &ActiveComponent, state: InventoryState) -> InventoryEntry {
    InventoryEntry {
        component_id: item.candidate.manifest.id.clone(),
        source: item.candidate.manifest.source.clone(),
        version: item.candidate.manifest.version.clone(),
        kind: item.candidate.manifest.kind,
        trust: item.candidate.manifest.trust,
        scope: item.candidate.manifest.scope,
        owner_scope: item.owner.scope().clone(),
        state,
        generation: item.generation,
        dependencies: item.candidate.manifest.requires.clone(),
        registrations: item.candidate.manifest.provides.clone(),
        in_flight: item.in_flight,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;

    use agentrs_contracts::component::{ComponentKind, ComponentTrust};

    use super::*;

    struct CountCleanup(Arc<AtomicUsize>);

    #[async_trait]
    impl AsyncCleanup for CountCleanup {
        async fn cleanup(self: Box<Self>) -> Result<(), CleanupError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeFactory {
        prepared: StdMutex<Vec<(String, Generation)>>,
        cleaned: Arc<AtomicUsize>,
        fail_version: StdMutex<Option<String>>,
    }

    #[async_trait]
    impl CandidateFactory for FakeFactory {
        async fn prepare(
            &self,
            candidate: &ComponentCandidate,
            generation: Generation,
            owner: Arc<ResourceOwner>,
        ) -> Result<(), CandidateError> {
            self.prepared
                .lock()
                .unwrap()
                .push((candidate.manifest.version.clone(), generation));
            owner
                .register("fake-resource", Box::new(CountCleanup(self.cleaned.clone())))
                .await
                .unwrap();
            if self.fail_version.lock().unwrap().as_deref() == Some(&candidate.manifest.version) {
                Err(CandidateError {
                    code: "unhealthy".into(),
                })
            } else {
                Ok(())
            }
        }
    }

    fn capabilities() -> CapabilityView {
        CapabilityView {
            tools: vec!["Read".into(), "mcp::github::search".into()],
            providers: vec!["p".into()],
            models: vec!["m".into()],
        }
    }

    fn candidate(id: &str, version: &str, requires: &[&str]) -> ComponentCandidate {
        ComponentCandidate {
            manifest: ComponentManifest {
                id: id.into(),
                source: format!("builtin:{id}"),
                version: version.into(),
                api_version: COMPONENT_API_VERSION,
                kind: ComponentKind::ToolCatalog,
                requires: requires.iter().copied().map(Into::into).collect(),
                provides: vec![format!("cap:{id}")],
                config_schema: serde_json::json!({
                    "type": "object",
                    "required": ["enabled"]
                }),
                scope: ComponentScope::Run,
                trust: ComponentTrust::TrustedBuiltin,
                requested_capabilities: CapabilityView {
                    tools: vec!["Read".into()],
                    providers: vec![],
                    models: vec![],
                },
                redacted_config_fields: vec![],
            },
            config: serde_json::json!({"enabled": true}),
        }
    }

    fn profile(revision: u64, components: Vec<ComponentCandidate>) -> CompositionProfile {
        CompositionProfile { revision, components }
    }

    #[tokio::test]
    async fn candidate_失败清理私有资源并保留旧树() {
        let manager = GenerationManager::new(ResourceOwner::new("root"));
        let factory = FakeFactory::default();
        manager
            .apply_profile(
                profile(1, vec![candidate("tools", "v1", &[])]),
                &capabilities(),
                &factory,
            )
            .await
            .unwrap();
        *factory.fail_version.lock().unwrap() = Some("v2".into());
        let result = manager
            .apply_profile(
                profile(2, vec![candidate("tools", "v2", &[])]),
                &capabilities(),
                &factory,
            )
            .await;
        assert!(matches!(result, Err(GenerationError::CandidateFailed { .. })));
        let inventory = manager.inventory().await;
        assert_eq!(inventory.revision, 1);
        assert_eq!(inventory.entries.len(), 1);
        assert_eq!(inventory.entries[0].generation, Generation(1));
        assert_eq!(factory.cleaned.load(Ordering::SeqCst), 1, "失败 candidate 已清理");
        assert_eq!(inventory.last_failure.as_deref(), Some("tools:unhealthy"));
    }

    #[tokio::test]
    async fn 旧_operation_固定旧代并阻止提前_cleanup() {
        let manager = GenerationManager::new(ResourceOwner::new("root"));
        let factory = FakeFactory::default();
        manager
            .apply_profile(
                profile(1, vec![candidate("tools", "v1", &[])]),
                &capabilities(),
                &factory,
            )
            .await
            .unwrap();
        let operation_owner = ResourceOwner::new("operation");
        let view = manager
            .begin_operation(&["tools".into()], &operation_owner)
            .await
            .unwrap();
        assert_eq!(view.generations()[&ComponentId::new("tools")], Generation(1));

        manager
            .apply_profile(
                profile(2, vec![candidate("tools", "v2", &[])]),
                &capabilities(),
                &factory,
            )
            .await
            .unwrap();
        let inventory = manager.inventory().await;
        assert_eq!(inventory.entries.len(), 2);
        assert!(inventory
            .entries
            .iter()
            .any(|entry| entry.state == InventoryState::Draining && entry.in_flight == 1));
        assert_eq!(
            factory.cleaned.load(Ordering::SeqCst),
            0,
            "旧代仍被 operation 使用"
        );

        operation_owner.shutdown().await;
        let inventory = manager.inventory().await;
        assert_eq!(inventory.entries.len(), 1);
        assert_eq!(inventory.entries[0].generation, Generation(2));
        assert_eq!(
            factory.cleaned.load(Ordering::SeqCst),
            1,
            "最后引用结算后释放旧代"
        );
    }

    #[tokio::test]
    async fn operation_递归固定依赖_generation() {
        let manager = GenerationManager::new(ResourceOwner::new("root"));
        let factory = FakeFactory::default();
        manager
            .apply_profile(
                profile(
                    1,
                    vec![
                        candidate("provider", "v1", &[]),
                        candidate("tools", "v1", &["provider"]),
                    ],
                ),
                &capabilities(),
                &factory,
            )
            .await
            .unwrap();
        let owner = ResourceOwner::new("operation");
        let view = manager.begin_operation(&["tools".into()], &owner).await.unwrap();
        assert_eq!(view.generations().len(), 2);
        owner.shutdown().await;
    }

    #[tokio::test]
    async fn 依赖换代会传递重建未改配置的依赖方() {
        let manager = GenerationManager::new(ResourceOwner::new("root"));
        let factory = FakeFactory::default();
        manager
            .apply_profile(
                profile(
                    1,
                    vec![
                        candidate("provider", "v1", &[]),
                        candidate("tools", "v1", &["provider"]),
                    ],
                ),
                &capabilities(),
                &factory,
            )
            .await
            .unwrap();
        manager
            .apply_profile(
                profile(
                    2,
                    vec![
                        candidate("provider", "v2", &[]),
                        candidate("tools", "v1", &["provider"]),
                    ],
                ),
                &capabilities(),
                &factory,
            )
            .await
            .unwrap();
        let inventory = manager.inventory().await;
        assert_eq!(inventory.entries.len(), 2);
        assert_eq!(
            inventory
                .entries
                .iter()
                .find(|entry| entry.component_id == ComponentId::new("provider"))
                .unwrap()
                .generation,
            Generation(3)
        );
        assert_eq!(
            inventory
                .entries
                .iter()
                .find(|entry| entry.component_id == ComponentId::new("tools"))
                .unwrap()
                .generation,
            Generation(4),
            "依赖方配置未变也必须重建"
        );
    }

    #[tokio::test]
    async fn 缺失依赖_循环_冲突_scope_与能力扩张全部_fail_loud() {
        let manager = GenerationManager::new(ResourceOwner::new("root"));
        let factory = FakeFactory::default();
        let missing = manager
            .apply_profile(
                profile(1, vec![candidate("a", "v1", &["missing"])]),
                &capabilities(),
                &factory,
            )
            .await;
        assert!(matches!(missing, Err(GenerationError::MissingDependency { .. })));

        let cycle = manager
            .apply_profile(
                profile(
                    2,
                    vec![candidate("a", "v1", &["b"]), candidate("b", "v1", &["a"])],
                ),
                &capabilities(),
                &factory,
            )
            .await;
        assert_eq!(cycle.unwrap_err(), GenerationError::DependencyCycle);

        let mut a = candidate("a", "v1", &[]);
        let mut b = candidate("b", "v1", &[]);
        b.manifest.provides = a.manifest.provides.clone();
        let conflict = manager
            .apply_profile(profile(3, vec![a.clone(), b]), &capabilities(), &factory)
            .await;
        assert!(matches!(conflict, Err(GenerationError::CapabilityConflict(_))));

        a.manifest.requested_capabilities.tools.push("Shell".into());
        let widening = manager
            .apply_profile(profile(4, vec![a]), &capabilities(), &factory)
            .await;
        assert!(matches!(
            widening,
            Err(GenerationError::CapabilityWidening { .. })
        ));
    }

    #[tokio::test]
    async fn 新合法_revision_可越过失败_revision_且旧_revision_不能回写() {
        let manager = GenerationManager::new(ResourceOwner::new("root"));
        let factory = FakeFactory::default();
        manager
            .apply_profile(
                profile(1, vec![candidate("tools", "v1", &[])]),
                &capabilities(),
                &factory,
            )
            .await
            .unwrap();
        let mut invalid = candidate("tools", "bad", &[]);
        invalid.config = serde_json::json!({});
        assert!(manager
            .apply_profile(profile(2, vec![invalid]), &capabilities(), &factory)
            .await
            .is_err());
        let committed = manager
            .apply_profile(
                profile(3, vec![candidate("tools", "v3", &[])]),
                &capabilities(),
                &factory,
            )
            .await
            .unwrap();
        assert_eq!(committed.revision, 3);
        assert!(matches!(
            manager
                .apply_profile(
                    profile(2, vec![candidate("tools", "v2", &[])]),
                    &capabilities(),
                    &factory
                )
                .await,
            Err(GenerationError::StaleRevision { .. })
        ));
    }

    #[tokio::test]
    async fn 并发_profile_最终收敛到最高合法_revision() {
        let manager = GenerationManager::new(ResourceOwner::new("root"));
        let factory = FakeFactory::default();
        let caps = capabilities();
        let second = manager.apply_profile(profile(2, vec![candidate("tools", "v2", &[])]), &caps, &factory);
        let third = manager.apply_profile(profile(3, vec![candidate("tools", "v3", &[])]), &caps, &factory);
        let _ = tokio::join!(second, third);
        let inventory = manager.inventory().await;
        assert_eq!(inventory.revision, 3);
        assert_eq!(inventory.entries.len(), 1);
        assert_eq!(inventory.entries[0].version, "v3");
    }
}
