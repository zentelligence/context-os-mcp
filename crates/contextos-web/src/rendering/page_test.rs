use super::*;

fn empty_nav() -> NavData {
    NavData {
        vaults: Vec::new(),
        current_vault: None,
        nav_target_vault: None,
        directory_breadcrumb: None,
        entries: Vec::new(),
        breadcrumb: vec![BreadcrumbSegment {
            label: "settings".to_owned(),
            href: None,
        }],
        active_vault_screen: false,
        active_apps_screen: false,
        active_settings_screen: false,
        rescan_href: None,
        appearance: Appearance::default(),
    }
}

#[test]
fn wraps_the_body_and_carries_the_title() {
    let html = render_page(&empty_nav(), "example.md", "<p>Body.</p>");
    assert!(html.contains("<title>example.md</title>"));
    assert!(html.contains("<p>Body.</p>"));
}

#[test]
fn renders_the_vault_switcher_with_every_configured_vault() {
    let nav = NavData {
        vaults: vec![
            NavVault {
                name: "vault".to_owned(),
                is_current: true,
            },
            NavVault {
                name: "other-vault".to_owned(),
                is_current: false,
            },
        ],
        ..empty_nav()
    };
    let html = render_page(&nav, "note.md", "<p>Body.</p>");
    assert!(html.contains("<option value=\"vault\" selected>vault</option>"));
    assert!(html.contains("<option value=\"other-vault\">other-vault</option>"));
}

#[test]
fn renders_the_current_directorys_entries_in_the_nav_tree() {
    let nav = NavData {
        current_vault: Some("vault".to_owned()),
        directory_breadcrumb: Some(vec![
            BreadcrumbSegment {
                label: "vault".to_owned(),
                href: Some("/vault/".to_owned()),
            },
            BreadcrumbSegment {
                label: "docs".to_owned(),
                href: Some("/vault/docs/".to_owned()),
            },
            BreadcrumbSegment {
                label: "guides".to_owned(),
                href: None,
            },
        ]),
        entries: vec![NavEntry {
            name: "reference".to_owned(),
            href: "/vault/docs/guides/reference/".to_owned(),
            is_dir: true,
        }],
        ..empty_nav()
    };
    let html = render_page(&nav, "note.md", "<p>Body.</p>");
    assert!(html.contains("/vault/docs/guides/reference/"));
}

#[test]
fn the_directory_breadcrumb_links_every_ancestor_but_the_current_directory() {
    let nav = NavData {
        current_vault: Some("vault".to_owned()),
        directory_breadcrumb: Some(vec![
            BreadcrumbSegment {
                label: "vault".to_owned(),
                href: Some("/vault/".to_owned()),
            },
            BreadcrumbSegment {
                label: "docs".to_owned(),
                href: Some("/vault/docs/".to_owned()),
            },
            BreadcrumbSegment {
                label: "guides".to_owned(),
                href: None,
            },
        ]),
        ..empty_nav()
    };
    let html = render_page(&nav, "docs/guides", "<p>Body.</p>");
    assert!(html.contains("<a href=\"/vault/\">vault</a>"));
    assert!(html.contains("<a href=\"/vault/docs/\">docs</a>"));
    assert!(html.contains("<span class=\"breadcrumb-current\">guides</span>"));
}

#[test]
fn the_top_bar_breadcrumb_links_every_ancestor_but_the_current_page() {
    let nav = NavData {
        current_vault: Some("vault".to_owned()),
        breadcrumb: vec![
            BreadcrumbSegment {
                label: "vault".to_owned(),
                href: Some("/vault/".to_owned()),
            },
            BreadcrumbSegment {
                label: "docs".to_owned(),
                href: Some("/vault/docs/".to_owned()),
            },
            BreadcrumbSegment {
                label: "note.md".to_owned(),
                href: None,
            },
        ],
        ..empty_nav()
    };
    let html = render_page(&nav, "note.md", "<p>Body.</p>");
    assert!(html.contains("<a href=\"/vault/\">vault</a>"));
    assert!(html.contains("<a href=\"/vault/docs/\">docs</a>"));
    assert!(html.contains("<span class=\"breadcrumb-current\">note.md</span>"));
}

#[test]
fn a_vault_independent_page_renders_no_tree_section() {
    let html = render_page(&empty_nav(), "Settings", "<p>Body.</p>");
    assert!(!html.contains("nav-tree"));
}

#[test]
fn marks_the_active_primary_nav_item() {
    let nav = NavData {
        current_vault: Some("vault".to_owned()),
        nav_target_vault: Some("vault".to_owned()),
        active_apps_screen: true,
        ..empty_nav()
    };
    let html = render_page(&nav, "Apps", "<p>Body.</p>");
    assert!(html.contains("href=\"/vault/apps/\" class=\"active\""));
}

#[test]
fn vault_browser_and_apps_stay_clickable_on_a_vault_independent_page() {
    let nav = NavData {
        current_vault: None,
        nav_target_vault: Some("vault".to_owned()),
        active_settings_screen: true,
        ..empty_nav()
    };
    let html = render_page(&nav, "Settings", "<p>Body.</p>");
    assert!(html.contains("<a href=\"/vault/\" class=\"\">📁 Vault browser</a>"));
    assert!(html.contains("<a href=\"/vault/apps/\" class=\"\">🧩 Apps</a>"));
    assert!(!html.contains("<span class=\"nav-dir\">📁 Vault browser</span>"));
}

#[test]
fn vault_browser_and_apps_are_inert_when_no_vault_is_configured_at_all() {
    let html = render_page(&empty_nav(), "Settings", "<p>Body.</p>");
    assert!(html.contains("<span class=\"nav-dir\">📁 Vault browser</span>"));
    assert!(html.contains("<span class=\"nav-dir\">🧩 Apps</span>"));
}

#[test]
fn appearance_is_applied_as_html_data_attributes() {
    let nav = NavData {
        appearance: Appearance {
            theme: Some("dark".to_owned()),
            font: Some("serif".to_owned()),
            size: Some("large".to_owned()),
        },
        ..empty_nav()
    };
    let html = render_page(&nav, "note.md", "<p>Body.</p>");
    assert!(html.contains("<html lang=\"en\" data-theme=\"dark\" data-font=\"serif\" data-size=\"large\">"));
}

#[test]
fn no_appearance_set_omits_every_data_attribute() {
    let html = render_page(&empty_nav(), "note.md", "<p>Body.</p>");
    assert!(html.contains("<html lang=\"en\">"));
    assert!(!html.contains("data-theme"));
    assert!(!html.contains("data-font"));
    assert!(!html.contains("data-size"));
}
