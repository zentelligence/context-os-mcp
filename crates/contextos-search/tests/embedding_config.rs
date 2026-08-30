//! FR-54, D-05: `EmbeddingProviderConfig`, the config-only provider
//! selection swap. Constructing a provider from this enum is the one
//! surface `contextos-server`'s configuration will eventually drive
//! (a later stage); these tests prove the swap needs no code change, only a
//! different enum variant, matching the plan's "gate's swap test is
//! config-only" requirement.
use contextos_search::{EmbeddingProviderConfig, EmbedsText};

#[test]
fn fr_54_openai_compatible_variant_selects_that_provider_and_surfaces_its_errors()
-> Result<(), Box<dyn std::error::Error>> {
    let variable = "CONTEXTOS_TEST_CONFIG_MISSING_KEY_FR54";
    assert!(
        std::env::var_os(variable).is_none(),
        "test precondition: {variable} must not already be set"
    );

    let provider: Result<Box<dyn EmbedsText>, _> =
        Box::<dyn EmbedsText>::try_from(EmbeddingProviderConfig::OpenAiCompatible {
            endpoint: "http://127.0.0.1:1/v1/embeddings".to_owned(),
            model: "test-model".to_owned(),
            api_key_env: variable.to_owned(),
        });

    let Err(error) = provider else {
        return Err("expected the missing api_key_env variable to reject construction".into());
    };
    assert_eq!(error.code(), "embedding/config");
    Ok(())
}

#[cfg(feature = "semantic-local")]
#[test]
fn fr_54_local_variant_selects_the_local_provider_and_surfaces_its_errors()
-> Result<(), Box<dyn std::error::Error>> {
    let vault = tempfile::tempdir()?;
    let missing = vault.path().join("does-not-exist");

    let provider: Result<Box<dyn EmbedsText>, _> =
        Box::<dyn EmbedsText>::try_from(EmbeddingProviderConfig::Local {
            model_directory: missing,
        });

    let Err(error) = provider else {
        return Err("expected the missing model directory to reject construction".into());
    };
    assert_eq!(error.code(), "embedding/local-unavailable");
    Ok(())
}

/// Only meaningful in the lean `--no-default-features` build: proves that
/// selecting `Local` without the `semantic-local` feature compiled in is a
/// typed rejection, never a compile error or a silent fallback to another
/// provider.
#[cfg(not(feature = "semantic-local"))]
#[test]
fn fr_54_local_variant_is_rejected_when_semantic_local_feature_is_off()
-> Result<(), Box<dyn std::error::Error>> {
    let provider: Result<Box<dyn EmbedsText>, _> =
        Box::<dyn EmbedsText>::try_from(EmbeddingProviderConfig::Local {
            model_directory: std::path::PathBuf::from("/nonexistent"),
        });

    let Err(error) = provider else {
        return Err("expected the local provider to be unavailable without semantic-local".into());
    };
    assert_eq!(error.code(), "embedding/local-unavailable");
    Ok(())
}
