//! Scoped tool registration and deferred schema discovery.
//!
//! Registration, authorization, and model visibility are deliberately separate.
//! A registered tool is eligible only when the immutable authority envelope and
//! current capability view both contain it. Deferred definitions expose their full
//! JSON schema only after explicit activation.

#![forbid(unsafe_code)]

pub mod builtin;
pub mod lint;
pub mod mcp;
/// 工具的运行期授权策略。
pub mod tool_policy;

use std::collections::{BTreeMap, BTreeSet};

use agentrs_contracts::component::Generation;
use agentrs_contracts::ids::ComponentId;
use agentrs_types::ToolDef;
use serde_json::json;

/// Initial model visibility of a registered tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exposure {
    /// The full schema is visible immediately.
    Eager,
    /// Only searchable metadata is visible until activation.
    Deferred,
}

/// A tool registration owned by Core.
#[derive(Debug, Clone, PartialEq)]
pub struct Registration {
    /// Model-facing definition and JSON schema.
    pub definition: ToolDef,
    /// Terms used by deterministic deferred discovery.
    pub keywords: Vec<String>,
    /// Whether the schema is initially visible.
    pub exposure: Exposure,
}

impl Registration {
    /// Register a tool whose schema is visible immediately.
    pub fn eager(definition: ToolDef) -> Self {
        Self {
            definition,
            keywords: Vec::new(),
            exposure: Exposure::Eager,
        }
    }

    /// Register a tool whose schema is loaded through discovery.
    pub fn deferred(definition: ToolDef, keywords: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            definition,
            keywords: keywords.into_iter().map(Into::into).collect(),
            exposure: Exposure::Deferred,
        }
    }
}

/// Registration or activation failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistryError {
    /// Tool names are unique stable identifiers.
    #[error("tool already registered: {0}")]
    Duplicate(String),
    /// Empty names cannot be authorized or called unambiguously.
    #[error("tool name must not be empty")]
    EmptyName,
    /// Empty descriptions make discovery unusable.
    #[error("tool description must not be empty: {0}")]
    EmptyDescription(String),
    /// Activation never widens the scoped capability set.
    #[error("tool is not available in this scope: {0}")]
    OutsideScope(String),
}

/// Process-wide registrations without per-run state.
#[derive(Debug, Clone, Default)]
pub struct ToolRegistry {
    registrations: BTreeMap<String, OwnedRegistration>,
}

#[derive(Debug, Clone)]
struct OwnedRegistration {
    registration: Registration,
    owner: Option<(ComponentId, Generation)>,
}

impl ToolRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a Core-owned registration.
    pub fn register(&mut self, registration: Registration) -> Result<(), RegistryError> {
        let name = registration.definition.name.trim();
        if name.is_empty() {
            return Err(RegistryError::EmptyName);
        }
        if registration.definition.description.trim().is_empty() {
            return Err(RegistryError::EmptyDescription(name.to_owned()));
        }
        if self.registrations.contains_key(name) {
            return Err(RegistryError::Duplicate(name.to_owned()));
        }
        self.registrations.insert(
            name.to_owned(),
            OwnedRegistration {
                registration,
                owner: None,
            },
        );
        Ok(())
    }

    /// 添加带 owner/generation 的组件注册。
    pub fn register_owned(
        &mut self,
        owner: ComponentId,
        generation: Generation,
        registration: Registration,
    ) -> Result<(), RegistryError> {
        let name = registration.definition.name.trim();
        if name.is_empty() {
            return Err(RegistryError::EmptyName);
        }
        if registration.definition.description.trim().is_empty() {
            return Err(RegistryError::EmptyDescription(name.to_owned()));
        }
        if self.registrations.contains_key(name) {
            return Err(RegistryError::Duplicate(name.to_owned()));
        }
        self.registrations.insert(
            name.to_owned(),
            OwnedRegistration {
                registration,
                owner: Some((owner, generation)),
            },
        );
        Ok(())
    }

    /// 只撤回指定 owner 的指定 generation；不关闭任何宿主连接。
    pub fn withdraw(&mut self, owner: &ComponentId, generation: Generation) -> Vec<String> {
        let removed: Vec<String> = self
            .registrations
            .iter()
            .filter(|(_, entry)| entry.owner.as_ref() == Some(&(owner.clone(), generation)))
            .map(|(name, _)| name.clone())
            .collect();
        for name in &removed {
            self.registrations.remove(name);
        }
        removed
    }

    /// Create isolated per-run visibility state.
    ///
    /// Eligibility is exactly `registered AND authority AND capability`.
    pub fn scope(&self, authority_tools: &[String], capability_tools: &[String]) -> ScopedRegistry {
        let authority: BTreeSet<&str> = authority_tools.iter().map(String::as_str).collect();
        let capability: BTreeSet<&str> = capability_tools.iter().map(String::as_str).collect();
        let eligible: BTreeMap<String, Registration> = self
            .registrations
            .iter()
            .filter(|(name, _)| authority.contains(name.as_str()) && capability.contains(name.as_str()))
            .map(|(name, entry)| (name.clone(), entry.registration.clone()))
            .collect();
        let active = eligible
            .iter()
            .filter(|(_, registration)| registration.exposure == Exposure::Eager)
            .map(|(name, _)| name.clone())
            .collect();
        ScopedRegistry { eligible, active }
    }
}

/// A schema-free discovery result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    /// Stable tool name.
    pub name: String,
    /// Short model-facing description.
    pub description: String,
    /// Deterministic relevance score. Higher is better.
    pub score: u32,
}

/// Per-run tool visibility. Activation can only reveal an eligible registration.
#[derive(Debug, Clone)]
pub struct ScopedRegistry {
    eligible: BTreeMap<String, Registration>,
    active: BTreeSet<String>,
}

impl ScopedRegistry {
    /// Full schemas currently visible to the model.
    pub fn catalog(&self) -> Vec<ToolDef> {
        let mut catalog: Vec<ToolDef> = self
            .active
            .iter()
            .filter_map(|name| self.eligible.get(name))
            .map(|registration| registration.definition.clone())
            .collect();
        if self.has_deferred() {
            catalog.push(tool_search_definition());
        }
        catalog
    }

    /// Search hidden eligible tools without exposing parameter schemas.
    pub fn search(&self, query: &str, limit: usize) -> Vec<SearchHit> {
        if limit == 0 {
            return Vec::new();
        }
        let query_terms = terms(query);
        let mut hits: Vec<SearchHit> = self
            .eligible
            .iter()
            .filter(|(name, registration)| {
                registration.exposure == Exposure::Deferred && !self.active.contains(*name)
            })
            .filter_map(|(name, registration)| {
                let score = score(name, registration, &query_terms);
                (score > 0).then(|| SearchHit {
                    name: name.clone(),
                    description: registration.definition.description.clone(),
                    score,
                })
            })
            .collect();
        hits.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then_with(|| left.name.cmp(&right.name))
        });
        hits.truncate(limit);
        hits
    }

    /// Activate full schemas by stable tool name.
    pub fn activate(&mut self, names: &[String]) -> Result<Vec<ToolDef>, RegistryError> {
        for name in names {
            if !self.eligible.contains_key(name) {
                return Err(RegistryError::OutsideScope(name.clone()));
            }
        }
        let mut activated = Vec::new();
        for name in names {
            if self.active.insert(name.clone()) {
                activated.push(
                    self.eligible
                        .get(name)
                        .expect("eligibility checked before mutation")
                        .definition
                        .clone(),
                );
            }
        }
        Ok(activated)
    }

    /// Search and activate the returned tools.
    pub fn discover(&mut self, query: &str, limit: usize) -> Vec<SearchHit> {
        let hits = self.search(query, limit);
        let names: Vec<String> = hits.iter().map(|hit| hit.name.clone()).collect();
        self.activate(&names)
            .expect("search only returns eligible registrations");
        hits
    }

    /// Whether an eligible deferred schema is still hidden.
    pub fn has_deferred(&self) -> bool {
        self.eligible.iter().any(|(name, registration)| {
            registration.exposure == Exposure::Deferred && !self.active.contains(name)
        })
    }
}

/// Core-handled discovery tool definition.
pub fn tool_search_definition() -> ToolDef {
    ToolDef::read_only(
        "ToolSearch",
        "Find and enable tools relevant to a task without loading every tool schema.",
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string" },
                "limit": { "type": "integer", "minimum": 1, "maximum": 10 }
            },
            "required": ["query"],
            "additionalProperties": false
        }),
    )
}

fn terms(query: &str) -> Vec<String> {
    query
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '-')
        .filter(|term| !term.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn score(name: &str, registration: &Registration, query_terms: &[String]) -> u32 {
    if query_terms.is_empty() {
        return 0;
    }
    let name = name.to_lowercase();
    let description = registration.definition.description.to_lowercase();
    let keywords: Vec<String> = registration
        .keywords
        .iter()
        .map(|value| value.to_lowercase())
        .collect();
    query_terms
        .iter()
        .map(|term| {
            if name == *term {
                100
            } else if name.contains(term) {
                40
            } else if keywords.iter().any(|keyword| keyword == term) {
                25
            } else if keywords.iter().any(|keyword| keyword.contains(term)) {
                15
            } else if description.contains(term) {
                5
            } else {
                0
            }
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> ToolRegistry {
        let mut registry = ToolRegistry::new();
        registry
            .register(Registration::eager(ToolDef::read_only(
                "Read",
                "Read a workspace file",
                json!({ "type": "object" }),
            )))
            .unwrap();
        registry
            .register(Registration::deferred(
                ToolDef::read_only("Grep", "Search file contents", json!({ "secret_schema": true })),
                ["search", "text"],
            ))
            .unwrap();
        registry
            .register(Registration::deferred(
                ToolDef::mutating("Deploy", "Deploy an application", json!({ "type": "object" })),
                ["release", "production"],
            ))
            .unwrap();
        registry
    }

    #[test]
    fn scope_is_registered_intersect_authority_intersect_capability() {
        let scoped = registry().scope(
            &["Read".into(), "Grep".into(), "Deploy".into(), "Ghost".into()],
            &["Read".into(), "Grep".into()],
        );
        let catalog = scoped.catalog();
        let names: Vec<&str> = catalog.iter().map(|tool| tool.name.as_str()).collect();
        assert_eq!(names, ["Read", "ToolSearch"]);
        assert!(scoped.search("production release", 10).is_empty());
    }

    #[test]
    fn search_does_not_expose_schema_until_activation() {
        let mut scoped = registry().scope(&["Read".into(), "Grep".into()], &["Read".into(), "Grep".into()]);
        let hits = scoped.search("search text", 5);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "Grep");
        assert!(!format!("{hits:?}").contains("secret_schema"));
        assert_eq!(scoped.catalog().len(), 2);

        scoped.activate(&["Grep".into()]).unwrap();
        let catalog = scoped.catalog();
        assert_eq!(
            catalog.len(),
            2,
            "ToolSearch disappears after the last activation"
        );
        assert!(catalog.iter().any(|tool| tool.name == "Grep"));
    }

    #[test]
    fn activation_cannot_widen_scope_and_is_atomic() {
        let mut scoped = registry().scope(&["Read".into()], &["Read".into()]);
        let before = scoped.catalog();
        assert_eq!(
            scoped.activate(&["Read".into(), "Deploy".into()]),
            Err(RegistryError::OutsideScope("Deploy".into()))
        );
        assert_eq!(
            scoped.catalog(),
            before,
            "failed activation must not partially mutate state"
        );
    }

    #[test]
    fn discovery_order_is_deterministic_and_limited() {
        let scoped = registry().scope(
            &["Grep".into(), "Deploy".into()],
            &["Grep".into(), "Deploy".into()],
        );
        let first = scoped.search("release search", 1);
        let second = scoped.search("release search", 1);
        assert_eq!(first, second);
        assert_eq!(first.len(), 1);
    }

    #[test]
    fn duplicate_and_invalid_registrations_fail_closed() {
        let mut registry = ToolRegistry::new();
        registry
            .register(Registration::eager(ToolDef::read_only("Read", "read", json!({}))))
            .unwrap();
        assert_eq!(
            registry.register(Registration::eager(ToolDef::read_only(
                "Read",
                "again",
                json!({})
            ))),
            Err(RegistryError::Duplicate("Read".into()))
        );
        assert_eq!(
            registry.register(Registration::eager(ToolDef::read_only(" ", "bad", json!({})))),
            Err(RegistryError::EmptyName)
        );
    }
}
