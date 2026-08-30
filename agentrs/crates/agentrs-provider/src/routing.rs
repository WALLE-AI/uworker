//! Authorized model retry and fallback routing.
//!
//! Routes are immutable Core input. The router never invents a provider or model,
//! and construction fails when a route is outside the current authorized set.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use agentrs_contracts::ids::{ModelId, ProviderId};
use agentrs_types::{LlmEvent, LlmRequest};

use crate::{ProviderError, ProviderPort};

/// One explicit model-to-provider route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// Model sent on this route.
    pub model: ModelId,
    /// Provider adapter used for the request.
    pub provider: ProviderId,
}

impl Route {
    /// Construct a route.
    pub fn new(model: impl Into<ModelId>, provider: impl Into<ProviderId>) -> Self {
        Self {
            model: model.into(),
            provider: provider.into(),
        }
    }
}

/// Invalid immutable routing configuration.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RoutingError {
    /// A route attempted to escape the allowed model set.
    #[error("route model is not allowed: {0}")]
    ModelNotAllowed(String),
    /// A route attempted to escape the allowed provider set.
    #[error("route provider is not allowed: {0}")]
    ProviderNotAllowed(String),
    /// Core did not supply an adapter for a declared route.
    #[error("route provider has no adapter: {0}")]
    MissingProvider(String),
}

/// A provider port that retries and falls back only within frozen routes.
pub struct RoutingProvider {
    routes: Vec<Route>,
    providers: BTreeMap<ProviderId, Arc<dyn ProviderPort>>,
    max_retries: u8,
}

impl RoutingProvider {
    /// Validate and construct a routing provider.
    pub fn new(
        primary: Route,
        fallbacks: Vec<Route>,
        allowed_models: &[ModelId],
        allowed_providers: &[ProviderId],
        providers: BTreeMap<ProviderId, Arc<dyn ProviderPort>>,
        max_retries: u8,
    ) -> Result<Self, RoutingError> {
        let models: BTreeSet<&ModelId> = allowed_models.iter().collect();
        let provider_ids: BTreeSet<&ProviderId> = allowed_providers.iter().collect();
        let routes: Vec<Route> = std::iter::once(primary).chain(fallbacks).collect();

        for route in &routes {
            if !models.contains(&route.model) {
                return Err(RoutingError::ModelNotAllowed(route.model.to_string()));
            }
            if !provider_ids.contains(&route.provider) {
                return Err(RoutingError::ProviderNotAllowed(route.provider.to_string()));
            }
            if !providers.contains_key(&route.provider) {
                return Err(RoutingError::MissingProvider(route.provider.to_string()));
            }
        }

        Ok(Self {
            routes,
            providers,
            max_retries,
        })
    }
}

#[async_trait::async_trait]
impl ProviderPort for RoutingProvider {
    async fn stream(&self, req: LlmRequest) -> Result<Vec<LlmEvent>, ProviderError> {
        let Some(primary) = self.routes.first() else {
            return Err(ProviderError::Other {
                code: "no_route".into(),
            });
        };
        if req.model != primary.model {
            return Err(ProviderError::Other {
                code: "route_model_mismatch".into(),
            });
        }

        for (route_index, route) in self.routes.iter().enumerate() {
            let provider = self
                .providers
                .get(&route.provider)
                .expect("constructor validated every route adapter");
            for attempt in 0..=self.max_retries {
                let mut routed = req.clone();
                routed.model = route.model.clone();
                match provider.stream(routed).await {
                    // A returned sequence may already contain visible deltas. It is
                    // therefore final from the router's perspective, even if its
                    // last item is an error event.
                    Ok(events) => return Ok(events),
                    Err(error) if error.is_terminal_for_routing() => return Err(error),
                    Err(error) if error.retry_same_route() && attempt < self.max_retries => continue,
                    Err(error) if error.allows_fallback() && route_index + 1 < self.routes.len() => break,
                    Err(error) => return Err(error),
                }
            }
        }

        Err(ProviderError::Other {
            code: "routes_exhausted".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use agentrs_types::{StopReason, TokenUsage};

    use super::*;

    struct ScriptedProvider {
        script: Mutex<VecDeque<Result<Vec<LlmEvent>, ProviderError>>>,
        models: Mutex<Vec<ModelId>>,
    }

    impl ScriptedProvider {
        fn new(script: Vec<Result<Vec<LlmEvent>, ProviderError>>) -> Arc<Self> {
            Arc::new(Self {
                script: Mutex::new(script.into()),
                models: Mutex::new(Vec::new()),
            })
        }
    }

    #[async_trait::async_trait]
    impl ProviderPort for ScriptedProvider {
        async fn stream(&self, req: LlmRequest) -> Result<Vec<LlmEvent>, ProviderError> {
            self.models.lock().unwrap().push(req.model);
            self.script.lock().unwrap().pop_front().expect("script exhausted")
        }
    }

    fn request() -> LlmRequest {
        LlmRequest {
            request_id: "q".into(),
            model: "primary-model".into(),
            system: String::new(),
            messages: Vec::new(),
            tools: Vec::new(),
            max_tokens: Some(10),
            thinking: None,
            reasoning_effort: None,
            cache_prefix_digest: None,
        }
    }

    fn done() -> Vec<LlmEvent> {
        vec![LlmEvent::Done {
            stop_reason: StopReason::EndTurn,
            usage: TokenUsage::default(),
        }]
    }

    fn router(
        primary: Arc<dyn ProviderPort>,
        fallback: Arc<dyn ProviderPort>,
        max_retries: u8,
    ) -> RoutingProvider {
        RoutingProvider::new(
            Route::new("primary-model", "primary-provider"),
            vec![Route::new("fallback-model", "fallback-provider")],
            &["primary-model".into(), "fallback-model".into()],
            &["primary-provider".into(), "fallback-provider".into()],
            [
                (ProviderId::new("primary-provider"), primary),
                (ProviderId::new("fallback-provider"), fallback),
            ]
            .into_iter()
            .collect(),
            max_retries,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn retries_then_falls_back_with_explicit_model_mapping() {
        let primary = ScriptedProvider::new(vec![
            Err(ProviderError::RateLimited),
            Err(ProviderError::RateLimited),
        ]);
        let fallback = ScriptedProvider::new(vec![Ok(done())]);
        let routed = router(primary.clone(), fallback.clone(), 1)
            .stream(request())
            .await
            .unwrap();
        assert_eq!(routed, done());
        assert_eq!(primary.models.lock().unwrap().len(), 2);
        assert_eq!(
            fallback.models.lock().unwrap().as_slice(),
            &[ModelId::new("fallback-model")]
        );
    }

    #[tokio::test]
    async fn context_too_long_is_reserved_for_compaction_not_fallback() {
        let primary = ScriptedProvider::new(vec![Err(ProviderError::ContextTooLong)]);
        let fallback = ScriptedProvider::new(vec![Ok(done())]);
        let error = router(primary, fallback.clone(), 3)
            .stream(request())
            .await
            .unwrap_err();
        assert_eq!(error, ProviderError::ContextTooLong);
        assert!(fallback.models.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn unauthorized_is_terminal_and_never_retried() {
        let primary = ScriptedProvider::new(vec![Err(ProviderError::Unauthorized)]);
        let fallback = ScriptedProvider::new(vec![Ok(done())]);
        assert_eq!(
            router(primary.clone(), fallback.clone(), 3)
                .stream(request())
                .await
                .unwrap_err(),
            ProviderError::Unauthorized
        );
        assert_eq!(primary.models.lock().unwrap().len(), 1);
        assert!(fallback.models.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn returned_partial_events_are_never_replayed() {
        let partial = vec![
            LlmEvent::TextDelta("visible".into()),
            LlmEvent::Error("stream_lost".into()),
        ];
        let primary = ScriptedProvider::new(vec![Ok(partial.clone())]);
        let fallback = ScriptedProvider::new(vec![Ok(done())]);
        assert_eq!(
            router(primary, fallback.clone(), 2)
                .stream(request())
                .await
                .unwrap(),
            partial
        );
        assert!(fallback.models.lock().unwrap().is_empty());
    }

    #[test]
    fn construction_rejects_routes_outside_current_capabilities() {
        let provider = ScriptedProvider::new(vec![Ok(done())]);
        let result = RoutingProvider::new(
            Route::new("primary-model", "primary-provider"),
            vec![Route::new("withdrawn-model", "primary-provider")],
            &["primary-model".into()],
            &["primary-provider".into()],
            [(
                ProviderId::new("primary-provider"),
                provider as Arc<dyn ProviderPort>,
            )]
            .into_iter()
            .collect(),
            0,
        );
        assert!(matches!(result, Err(RoutingError::ModelNotAllowed(model)) if model == "withdrawn-model"));
    }
}
