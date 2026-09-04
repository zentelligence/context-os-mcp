//! `NFR-05` (measured, non-gating): confirms `resources/list` and the three
//! filesystem listing tools (`fs_list_directory` plain and with `with_sizes`,
//! `fs_directory_tree`, `fs_search_files`)
//! remain viable once `hidden` filtering (`FR-84`) and the broadened
//! resource population (`FR-80`) are in place, at 10k-file vault scale
//! (Phase 7 Stage 6's benchmark note, mirroring Phase 4's own `NFR-05`
//! follow-up discipline). All ten thousand files sit flat in the vault
//! root, the worst case for every one of these five surfaces since none of
//! them may stop short of a complete listing. Also times `doctor`'s
//! frontmatter validity check (`FR-95`, Phase 8), the one doctor check that
//! reads every file's content rather than calling an existing status
//! method, per that phase's own `NFR-05` design note.
//!
//! Ignored by default: this is a measured note, not a CI gate
//! (`.claude/workflows/quality-gate.md`: `NFR-05` is recorded as a
//! "measured benchmark (non-gating)" row in the requirement matrix, not an
//! assertion with a pass/fail threshold). Run explicitly with:
//!
//! ```sh
//! cargo test -p contextos-mcp --test nfr_05_benchmark -- --ignored --nocapture
//! ```

use std::time::Instant;

use contextos_mcp::{Config, ContextOsServer};
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use serde_json::json;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

const FILE_COUNT: usize = 10_000;

#[tokio::test]
#[ignore = "measured NFR-05 benchmark note, not a CI gate; run explicitly with --ignored --nocapture"]
async fn nfr_05_enumeration_surfaces_remain_viable_on_a_10k_file_vault() -> Result<(), BoxError> {
    let vault = tempfile::Builder::new().prefix("vault").tempdir()?;
    for index in 0..FILE_COUNT {
        std::fs::write(vault.path().join(format!("file-{index:05}.md")), "content")?;
    }

    let mut config = Config::try_from(vec![vault.path().to_path_buf()])?;
    // `FR-107`: `resources/list` only enumerates configured include
    // patterns now; opt in broadly so this benchmark still exercises a
    // full 10k-file enumeration, its own actual purpose.
    config.vaults[0].resources_list_include = vec!["**/*".to_owned()];
    let server = ContextOsServer::try_from(config)?;
    let (server_transport, client_transport) = tokio::io::duplex(1 << 20);
    let server_handle = tokio::spawn(async move {
        server.serve(server_transport).await?.waiting().await?;
        Ok::<(), BoxError>(())
    });
    let client = ().serve(client_transport).await?;

    let started = Instant::now();
    let listed = client.list_resources(None).await?;
    let resources_list = started.elapsed();
    assert_eq!(
        listed.resources.len(),
        FILE_COUNT,
        "resources/list must enumerate every file"
    );

    let started = Instant::now();
    client
        .call_tool(
            CallToolRequestParams::new("fs_list_directory")
                .with_arguments(serde_json::from_value(json!({"path": "."}))?),
        )
        .await?;
    let fs_list_directory = started.elapsed();

    let started = Instant::now();
    client
        .call_tool(
            CallToolRequestParams::new("fs_list_directory").with_arguments(serde_json::from_value(
                json!({"path": ".", "with_sizes": true}),
            )?),
        )
        .await?;
    let fs_list_directory_with_sizes = started.elapsed();

    let started = Instant::now();
    client
        .call_tool(
            CallToolRequestParams::new("fs_directory_tree")
                .with_arguments(serde_json::from_value(json!({"path": "."}))?),
        )
        .await?;
    let fs_directory_tree = started.elapsed();

    let started = Instant::now();
    client
        .call_tool(
            CallToolRequestParams::new("fs_search_files").with_arguments(serde_json::from_value(
                json!({"path": ".", "pattern": "**/*", "max_results": FILE_COUNT * 2}),
            )?),
        )
        .await?;
    let fs_search_files = started.elapsed();

    let started = Instant::now();
    client
        .call_tool(CallToolRequestParams::new("doctor"))
        .await?;
    let doctor = started.elapsed();

    println!(
        "NFR-05 10k-file benchmark (release={}): resources/list={resources_list:?} \
         fs_list_directory={fs_list_directory:?} \
         fs_list_directory_with_sizes={fs_list_directory_with_sizes:?} \
         fs_directory_tree={fs_directory_tree:?} \
         fs_search_files={fs_search_files:?} \
         doctor={doctor:?}",
        !cfg!(debug_assertions)
    );

    drop(client);
    server_handle.await??;
    Ok(())
}
