use crate::{OperationEvent, OperationWarning, Origin};

/// Port that dispatches a completed mutation to non-primary services.
pub trait RoutesOperations: Send + Sync {
    /// Returns every non-fatal service warning in dispatch order.
    fn route(&self, event: &OperationEvent) -> Vec<OperationWarning>;
}

/// Routing adapter used before substrate services are configured.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoSubstrateServices;

impl RoutesOperations for NoSubstrateServices {
    fn route(&self, _event: &OperationEvent) -> Vec<OperationWarning> {
        Vec::new()
    }
}

/// Port for managed `index.md` reconciliation after a completed mutation.
pub trait MaintainsIndexes: Send + Sync {
    /// Reconciles directories affected by the event.
    ///
    /// # Errors
    ///
    /// Returns a non-fatal warning when reconciliation must be healed later.
    fn reconcile(&self, event: &OperationEvent) -> Result<Vec<OperationEvent>, OperationWarning>;
}

/// Port for append-only operation logging after a completed mutation.
pub trait LogsOperations: Send + Sync {
    /// Appends one compatible operation-log entry.
    ///
    /// # Errors
    ///
    /// Returns a non-fatal warning when the entry must be retried later.
    fn append(&self, event: &OperationEvent) -> Result<Vec<OperationEvent>, OperationWarning>;
}

/// Port for staging completed operations in local version control.
pub trait VersionsVault: Send + Sync {
    /// Stages only the paths owned by this operation.
    ///
    /// # Errors
    ///
    /// Returns a non-fatal warning when versioning is degraded.
    fn stage(&self, event: &OperationEvent) -> Result<(), OperationWarning>;
}

/// Port for incremental search-index updates after a completed mutation.
///
/// Search receives tool and internal events alike: managed index blocks and
/// operation-log entries are vault markdown, so they remain searchable.
pub trait UpdatesSearch: Send + Sync {
    /// Applies the event to the derived search state.
    ///
    /// # Errors
    ///
    /// Returns a non-fatal warning when search state must be healed later.
    fn update(&self, event: &OperationEvent) -> Result<(), OperationWarning>;
}

/// Search adapter used before search services are configured.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoSearchUpdates;

impl UpdatesSearch for NoSearchUpdates {
    fn update(&self, _event: &OperationEvent) -> Result<(), OperationWarning> {
        Ok(())
    }
}

/// A downstream service class that may observe a completed mutation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationService {
    Index,
    OperationLog,
    Git,
    Search,
}

/// Immutable routing policy derived from an operation's trusted origin.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OperationRoute {
    kind: OperationRouteKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OperationRouteKind {
    Tool,
    Internal,
}

impl From<&Origin> for OperationRoute {
    fn from(value: &Origin) -> Self {
        match value {
            Origin::Tool(_) => Self {
                kind: OperationRouteKind::Tool,
            },
            Origin::Internal(_) => Self {
                kind: OperationRouteKind::Internal,
            },
        }
    }
}

impl OperationRoute {
    /// Reports whether this route delivers to the selected service class.
    #[must_use]
    pub const fn includes(self, service: OperationService) -> bool {
        match (self.kind, service) {
            (
                OperationRouteKind::Internal,
                OperationService::Index | OperationService::OperationLog,
            ) => false,
            (
                OperationRouteKind::Tool | OperationRouteKind::Internal,
                OperationService::Index
                | OperationService::OperationLog
                | OperationService::Git
                | OperationService::Search,
            ) => true,
        }
    }
}

/// Trusted dependencies for routing completed operation events.
#[derive(Clone, Debug)]
pub struct OperationRouterConfig<I, L, V, S> {
    pub indexes: I,
    pub operation_log: L,
    pub versions: V,
    pub search: S,
}

/// Central dispatcher that makes internal index/log recursion impossible.
#[derive(Clone, Debug)]
pub struct OperationRouter<I, L, V, S> {
    indexes: I,
    operation_log: L,
    versions: V,
    search: S,
}

impl<I, L, V, S> From<OperationRouterConfig<I, L, V, S>> for OperationRouter<I, L, V, S> {
    fn from(value: OperationRouterConfig<I, L, V, S>) -> Self {
        Self {
            indexes: value.indexes,
            operation_log: value.operation_log,
            versions: value.versions,
            search: value.search,
        }
    }
}

impl<I, L, V, S> OperationRouter<I, L, V, S>
where
    I: MaintainsIndexes,
    L: LogsOperations,
    V: VersionsVault,
    S: UpdatesSearch,
{
    /// Delivers an event to its allowed services and collects non-fatal failures.
    #[must_use]
    pub fn route(&self, event: &OperationEvent) -> Vec<OperationWarning> {
        let route = OperationRoute::from(&event.origin);
        let mut warnings = Vec::new();
        let mut internal_events = Vec::new();

        if route.includes(OperationService::Index) {
            match self.indexes.reconcile(event) {
                Ok(events) => internal_events.extend(events),
                Err(warning) => warnings.push(warning),
            }
        }
        if route.includes(OperationService::OperationLog) {
            match self.operation_log.append(event) {
                Ok(events) => internal_events.extend(events),
                Err(warning) => warnings.push(warning),
            }
        }
        if route.includes(OperationService::Git)
            && let Err(warning) = self.versions.stage(event)
        {
            warnings.push(warning);
        }
        if route.includes(OperationService::Search)
            && let Err(warning) = self.search.update(event)
        {
            warnings.push(warning);
        }
        for internal_event in internal_events {
            if let Err(warning) = self.versions.stage(&internal_event) {
                warnings.push(warning);
            }
            if let Err(warning) = self.search.update(&internal_event) {
                warnings.push(warning);
            }
        }

        warnings
    }
}

impl<I, L, V, S> RoutesOperations for OperationRouter<I, L, V, S>
where
    I: MaintainsIndexes,
    L: LogsOperations,
    V: VersionsVault,
    S: UpdatesSearch,
{
    fn route(&self, event: &OperationEvent) -> Vec<OperationWarning> {
        Self::route(self, event)
    }
}
