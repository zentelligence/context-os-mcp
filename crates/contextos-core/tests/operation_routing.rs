use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use contextos_core::{
    LogsOperations, MaintainsIndexes, OpKind, OperationEvent, OperationRoute, OperationRouter,
    OperationRouterConfig, OperationService, OperationWarning, Origin, UpdatesSearch,
    VersionsVault,
};
use proptest::prelude::*;
use time::OffsetDateTime;

#[derive(Clone, Debug, Default)]
struct InvocationCount(Arc<AtomicUsize>);

impl InvocationCount {
    fn increment(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    fn observed(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Clone, Debug)]
struct IndexRecorder(InvocationCount);

impl MaintainsIndexes for IndexRecorder {
    fn reconcile(&self, _event: &OperationEvent) -> Result<Vec<OperationEvent>, OperationWarning> {
        self.0.increment();
        Ok(Vec::new())
    }
}

#[derive(Clone, Debug)]
struct OperationLogRecorder(InvocationCount);

impl LogsOperations for OperationLogRecorder {
    fn append(&self, _event: &OperationEvent) -> Result<Vec<OperationEvent>, OperationWarning> {
        self.0.increment();
        Ok(Vec::new())
    }
}

#[derive(Clone, Debug)]
struct GitRecorder(InvocationCount);

impl VersionsVault for GitRecorder {
    fn stage(&self, _event: &OperationEvent) -> Result<(), OperationWarning> {
        self.0.increment();
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct SearchRecorder(InvocationCount);

impl UpdatesSearch for SearchRecorder {
    fn update(&self, _event: &OperationEvent) -> Result<(), OperationWarning> {
        self.0.increment();
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct DerivingIndex(InvocationCount);

impl MaintainsIndexes for DerivingIndex {
    fn reconcile(&self, event: &OperationEvent) -> Result<Vec<OperationEvent>, OperationWarning> {
        self.0.increment();
        let mut internal = event.clone();
        internal.origin = Origin::Internal("index".to_owned());
        Ok(vec![internal])
    }
}

#[derive(Clone, Debug)]
struct DerivingOperationLog(InvocationCount);

impl LogsOperations for DerivingOperationLog {
    fn append(&self, event: &OperationEvent) -> Result<Vec<OperationEvent>, OperationWarning> {
        self.0.increment();
        let mut internal = event.clone();
        internal.origin = Origin::Internal("oplog".to_owned());
        Ok(vec![internal])
    }
}

proptest! {
    #[test]
    fn fr_25_internal_events_never_route_to_recursive_services(
        service_name in "[a-z][a-z0-9_-]{0,63}"
    ) {
        let origin = Origin::Internal(service_name);
        let route = OperationRoute::from(&origin);
        let index_count = InvocationCount::default();
        let log_count = InvocationCount::default();
        let git_count = InvocationCount::default();
        let search_count = InvocationCount::default();
        let router = OperationRouter::from(OperationRouterConfig {
            indexes: IndexRecorder(index_count.clone()),
            operation_log: OperationLogRecorder(log_count.clone()),
            versions: GitRecorder(git_count.clone()),
            search: SearchRecorder(search_count.clone()),
        });
        let event = OperationEvent {
            kind: OpKind::Modify,
            paths: Vec::new(),
            origin,
            summary: "internal write".to_owned(),
            at: OffsetDateTime::UNIX_EPOCH,
        };

        prop_assert!(!route.includes(OperationService::Index));
        prop_assert!(!route.includes(OperationService::OperationLog));
        prop_assert!(route.includes(OperationService::Git));
        prop_assert!(route.includes(OperationService::Search));
        prop_assert!(router.route(&event).is_empty());
        prop_assert_eq!(index_count.observed(), 0);
        prop_assert_eq!(log_count.observed(), 0);
        prop_assert_eq!(git_count.observed(), 1);
        prop_assert_eq!(search_count.observed(), 1);
    }
}

#[test]
fn fr_25_tool_events_route_to_every_substrate_service() {
    let route = OperationRoute::from(&Origin::Tool("fs_write_file".to_owned()));

    assert!(route.includes(OperationService::Index));
    assert!(route.includes(OperationService::OperationLog));
    assert!(route.includes(OperationService::Git));
    assert!(route.includes(OperationService::Search));
}

#[test]
fn fr_25_derived_internal_events_are_staged_without_recursive_redispatch() {
    let index_count = InvocationCount::default();
    let log_count = InvocationCount::default();
    let git_count = InvocationCount::default();
    let search_count = InvocationCount::default();
    let router = OperationRouter::from(OperationRouterConfig {
        indexes: DerivingIndex(index_count.clone()),
        operation_log: DerivingOperationLog(log_count.clone()),
        versions: GitRecorder(git_count.clone()),
        search: SearchRecorder(search_count.clone()),
    });
    let event = OperationEvent {
        kind: OpKind::Modify,
        paths: Vec::new(),
        origin: Origin::Tool("fs_write_file".to_owned()),
        summary: "user write".to_owned(),
        at: OffsetDateTime::UNIX_EPOCH,
    };

    let warnings = router.route(&event);

    assert!(warnings.is_empty());
    assert_eq!(index_count.observed(), 1);
    assert_eq!(log_count.observed(), 1);
    assert_eq!(git_count.observed(), 3);
    assert_eq!(search_count.observed(), 3);
}
