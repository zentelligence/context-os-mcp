use tempfile::tempdir;

use super::*;

#[test]
fn macos_resolves_found_when_the_containing_directory_exists()
-> Result<(), Box<dyn std::error::Error>> {
    let home = tempdir()?;
    std::fs::create_dir_all(home.path().join("Library/Application Support/Claude"))?;

    let resolution = resolve_macos_config_path(home.path());

    assert_eq!(
        resolution,
        HostPathResolution::Found(
            home.path()
                .join("Library/Application Support/Claude/claude_desktop_config.json")
        )
    );
    Ok(())
}

#[test]
fn macos_resolves_not_found_when_no_claude_directory_exists()
-> Result<(), Box<dyn std::error::Error>> {
    let home = tempdir()?;

    let resolution = resolve_macos_config_path(home.path());

    assert!(matches!(resolution, HostPathResolution::NotFound { .. }));
    Ok(())
}

#[test]
fn windows_resolves_found_for_exactly_one_matching_package_folder()
-> Result<(), Box<dyn std::error::Error>> {
    let packages = tempdir()?;
    let package = packages.path().join("Claude_abc123xyz");
    std::fs::create_dir_all(package.join("LocalCache/Roaming/Claude"))?;

    let resolution = resolve_windows_config_path(packages.path())?;

    assert_eq!(
        resolution,
        HostPathResolution::Found(
            package.join("LocalCache/Roaming/Claude/claude_desktop_config.json")
        )
    );
    Ok(())
}

#[test]
fn windows_resolves_ambiguous_for_two_matching_package_folders()
-> Result<(), Box<dyn std::error::Error>> {
    let packages = tempdir()?;
    for suffix in ["Claude_abc123", "Claude_def456"] {
        std::fs::create_dir_all(
            packages
                .path()
                .join(suffix)
                .join("LocalCache/Roaming/Claude"),
        )?;
    }

    let resolution = resolve_windows_config_path(packages.path())?;

    assert!(matches!(
        resolution,
        HostPathResolution::Ambiguous { candidates } if candidates.len() == 2
    ));
    Ok(())
}

#[test]
fn windows_rejects_a_stale_package_folder_missing_the_real_subpath()
-> Result<(), Box<dyn std::error::Error>> {
    let packages = tempdir()?;
    // Name prefix matches, but the real `LocalCache/Roaming/Claude` subpath
    // is absent: the specific false-positive `D-29` calls out (a leftover
    // folder from an old install or a reinstall).
    std::fs::create_dir_all(packages.path().join("Claude_stale-leftover"))?;

    let resolution = resolve_windows_config_path(packages.path())?;

    assert!(matches!(resolution, HostPathResolution::NotFound { .. }));
    Ok(())
}

#[test]
fn windows_ignores_folders_that_do_not_match_the_name_prefix()
-> Result<(), Box<dyn std::error::Error>> {
    let packages = tempdir()?;
    std::fs::create_dir_all(
        packages
            .path()
            .join("SomeOtherApp_abc123")
            .join("LocalCache/Roaming/Claude"),
    )?;

    let resolution = resolve_windows_config_path(packages.path())?;

    assert!(matches!(resolution, HostPathResolution::NotFound { .. }));
    Ok(())
}

#[test]
fn windows_resolves_not_found_when_the_packages_directory_itself_is_missing()
-> Result<(), Box<dyn std::error::Error>> {
    let packages = tempdir()?;
    let missing = packages.path().join("does-not-exist");

    let resolution = resolve_windows_config_path(&missing)?;

    assert!(matches!(resolution, HostPathResolution::NotFound { .. }));
    Ok(())
}

#[test]
fn windows_roaming_resolves_found_when_the_containing_directory_exists()
-> Result<(), Box<dyn std::error::Error>> {
    let roaming = tempdir()?;
    std::fs::create_dir_all(roaming.path().join("Claude"))?;

    let resolution = resolve_windows_roaming_config_path(roaming.path());

    assert_eq!(
        resolution,
        HostPathResolution::Found(roaming.path().join("Claude/claude_desktop_config.json"))
    );
    Ok(())
}

#[test]
fn windows_roaming_resolves_not_found_when_no_claude_directory_exists()
-> Result<(), Box<dyn std::error::Error>> {
    let roaming = tempdir()?;

    let resolution = resolve_windows_roaming_config_path(roaming.path());

    assert!(matches!(resolution, HostPathResolution::NotFound { .. }));
    Ok(())
}

#[test]
fn windows_combined_finds_a_plain_roaming_install_with_no_packages_directory_at_all()
-> Result<(), Box<dyn std::error::Error>> {
    // The real-world case that motivated checking both layouts: a plain
    // installer put Claude Desktop under Roaming AppData, and
    // `%LOCALAPPDATA%\Packages` never existed at all on that device.
    let packages = tempdir()?;
    let missing_packages = packages.path().join("does-not-exist");
    let roaming = tempdir()?;
    std::fs::create_dir_all(roaming.path().join("Claude"))?;

    let resolution = resolve_windows_config_paths(&missing_packages, roaming.path())?;

    assert_eq!(
        resolution,
        HostPathResolution::Found(roaming.path().join("Claude/claude_desktop_config.json"))
    );
    Ok(())
}

#[test]
fn windows_combined_finds_an_msix_install_with_no_roaming_install_present()
-> Result<(), Box<dyn std::error::Error>> {
    let packages = tempdir()?;
    let package = packages.path().join("Claude_abc123xyz");
    std::fs::create_dir_all(package.join("LocalCache/Roaming/Claude"))?;
    let roaming = tempdir()?;

    let resolution = resolve_windows_config_paths(packages.path(), roaming.path())?;

    assert_eq!(
        resolution,
        HostPathResolution::Found(
            package.join("LocalCache/Roaming/Claude/claude_desktop_config.json")
        )
    );
    Ok(())
}

#[test]
fn windows_combined_reports_ambiguous_when_both_layouts_have_a_candidate()
-> Result<(), Box<dyn std::error::Error>> {
    let packages = tempdir()?;
    let package = packages.path().join("Claude_abc123xyz");
    std::fs::create_dir_all(package.join("LocalCache/Roaming/Claude"))?;
    let roaming = tempdir()?;
    std::fs::create_dir_all(roaming.path().join("Claude"))?;

    let resolution = resolve_windows_config_paths(packages.path(), roaming.path())?;

    assert!(matches!(
        resolution,
        HostPathResolution::Ambiguous { candidates } if candidates.len() == 2
    ));
    Ok(())
}

#[test]
fn windows_combined_resolves_not_found_only_when_neither_layout_has_a_candidate()
-> Result<(), Box<dyn std::error::Error>> {
    let packages = tempdir()?;
    let roaming = tempdir()?;

    let resolution = resolve_windows_config_paths(packages.path(), roaming.path())?;

    assert!(matches!(resolution, HostPathResolution::NotFound { .. }));
    Ok(())
}

#[test]
fn linux_always_resolves_not_found_with_an_explicit_reason() {
    let resolution = resolve_linux_config_path();

    assert!(matches!(
        resolution,
        HostPathResolution::NotFound { reason } if reason.contains("Linux")
    ));
}
