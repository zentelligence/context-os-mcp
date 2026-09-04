use crate::apps::{AppKind, AppStatus, AppTarget, RegisteredApp};

use super::list_entry;

fn app(kind: AppKind, target: AppTarget, status: AppStatus) -> RegisteredApp {
    RegisteredApp {
        slug: "task-register".to_owned(),
        name: "Task Register Dashboard".to_owned(),
        kind,
        entry: "index.html".to_owned(),
        target,
        mcp_servers: vec!["contextos".to_owned()],
        status,
    }
}

#[test]
fn a_supported_spa_entry_is_servable_and_labelled_spa() {
    let entry = list_entry(&app(AppKind::Spa, AppTarget::Blank, AppStatus::Supported));
    assert!(entry.servable);
    assert_eq!(entry.kind_label, "spa");
    assert_eq!(entry.target_attr, "_blank");
    assert_eq!(entry.opens_label, "new tab");
    assert_eq!(entry.slug, "task-register");
    assert_eq!(entry.name, "Task Register Dashboard");
}

#[test]
fn a_not_yet_supported_htmx_entry_is_not_servable_and_labelled_htmx() {
    let entry = list_entry(&app(
        AppKind::Htmx,
        AppTarget::Embed,
        AppStatus::NotYetSupported,
    ));
    assert!(!entry.servable);
    assert_eq!(entry.kind_label, "htmx");
    assert_eq!(entry.target_attr, "_self");
    assert_eq!(entry.opens_label, "inline");
}
