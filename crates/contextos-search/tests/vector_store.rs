//! FR-53, D-04: `SqliteVecStore` CRUD, similarity ranking, and persistence.
//!
//! Test vectors are deliberately orthogonal or parallel so the expected
//! cosine-distance ranking is unambiguous (module docs on
//! `contextos_search::vector_store` explain the cosine distance formula
//! `sqlite-vec` returns and how this crate turns it into a `[-1.0, 1.0]`
//! similarity score).

use std::error::Error;

use contextos_core::ContentHash;
use contextos_search::{
    SearchError, SimilarityQuery, SqliteVecConfig, SqliteVecStore, StoresVectors, VectorRecord,
};
use proptest::prelude::*;
use proptest::test_runner::TestCaseError;
use tempfile::tempdir;

fn hash(seed: u8) -> ContentHash {
    ContentHash::from([seed; 32])
}

fn store(dimension: usize) -> Result<(SqliteVecStore, tempfile::TempDir), Box<dyn Error>> {
    let directory = tempdir()?;
    let path = directory.path().join("vectors.db");
    let store = SqliteVecStore::try_from(SqliteVecConfig { path, dimension })?;
    Ok((store, directory))
}

#[test]
fn upsert_one_chunk_is_retrievable_by_similar() -> Result<(), Box<dyn Error>> {
    let (store, _directory) = store(4)?;
    let record = VectorRecord {
        path: "a/one.md",
        ordinal: 0,
        heading_context: &["Intro".to_owned()],
        content_hash: &hash(1),
        vector: &[1.0, 0.0, 0.0, 0.0],
    };
    store.upsert(std::slice::from_ref(&record))?;

    let hits = store.similar(&SimilarityQuery {
        vector: &[1.0, 0.0, 0.0, 0.0],
        k: 5,
        path_prefix: None,
        exclude_paths: &[],
    })?;

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, "a/one.md");
    assert_eq!(hits[0].ordinal, 0);
    assert_eq!(hits[0].heading_context, vec!["Intro".to_owned()]);
    assert_eq!(hits[0].content_hash, hash(1));
    assert!((hits[0].score - 1.0).abs() < 1e-4);
    Ok(())
}

#[test]
fn similar_ranks_by_cosine_distance_not_insertion_order() -> Result<(), Box<dyn Error>> {
    let (store, _directory) = store(4)?;
    // Inserted deliberately out of expected-rank order.
    let (hash2, hash3, hash4, hash5) = (hash(2), hash(3), hash(4), hash(5));
    let records = vec![
        VectorRecord {
            path: "orthogonal.md",
            ordinal: 0,
            heading_context: &[],
            content_hash: &hash2,
            vector: &[0.0, 1.0, 0.0, 0.0],
        },
        VectorRecord {
            path: "opposite.md",
            ordinal: 0,
            heading_context: &[],
            content_hash: &hash3,
            vector: &[-1.0, 0.0, 0.0, 0.0],
        },
        VectorRecord {
            path: "parallel.md",
            ordinal: 0,
            heading_context: &[],
            content_hash: &hash4,
            vector: &[2.0, 0.0, 0.0, 0.0], // same direction, different magnitude
        },
        VectorRecord {
            path: "identical.md",
            ordinal: 0,
            heading_context: &[],
            content_hash: &hash5,
            vector: &[1.0, 0.0, 0.0, 0.0],
        },
    ];
    store.upsert(&records)?;

    let hits = store.similar(&SimilarityQuery {
        vector: &[1.0, 0.0, 0.0, 0.0],
        k: 10,
        path_prefix: None,
        exclude_paths: &[],
    })?;

    let ranked_paths: Vec<&str> = hits.iter().map(|hit| hit.path.as_str()).collect();
    // "identical" and "parallel" are both cosine-similarity 1.0 (score 1.0);
    // "orthogonal" is score 0.0; "opposite" is score -1.0. Ties between
    // identical/parallel are not ordered by this assertion, only their
    // joint precedence over orthogonal and opposite.
    assert_eq!(ranked_paths.len(), 4);
    assert!(ranked_paths[0..2].contains(&"identical.md"));
    assert!(ranked_paths[0..2].contains(&"parallel.md"));
    assert_eq!(ranked_paths[2], "orthogonal.md");
    assert_eq!(ranked_paths[3], "opposite.md");

    assert!((hits[0].score - 1.0).abs() < 1e-4);
    assert!((hits[2].score - 0.0).abs() < 1e-4);
    assert!((hits[3].score - (-1.0)).abs() < 1e-4);
    Ok(())
}

#[test]
fn upsert_of_existing_path_and_ordinal_replaces_rather_than_duplicates()
-> Result<(), Box<dyn Error>> {
    let (store, _directory) = store(4)?;
    let first = VectorRecord {
        path: "a/one.md",
        ordinal: 0,
        heading_context: &["Old".to_owned()],
        content_hash: &hash(1),
        vector: &[1.0, 0.0, 0.0, 0.0],
    };
    store.upsert(std::slice::from_ref(&first))?;

    let second = VectorRecord {
        path: "a/one.md",
        ordinal: 0,
        heading_context: &["New".to_owned()],
        content_hash: &hash(9),
        vector: &[0.0, 1.0, 0.0, 0.0],
    };
    store.upsert(std::slice::from_ref(&second))?;

    let hits = store.similar(&SimilarityQuery {
        vector: &[0.0, 1.0, 0.0, 0.0],
        k: 10,
        path_prefix: None,
        exclude_paths: &[],
    })?;

    assert_eq!(hits.len(), 1, "replace must not duplicate the row");
    assert_eq!(hits[0].heading_context, vec!["New".to_owned()]);
    assert_eq!(hits[0].content_hash, hash(9));
    Ok(())
}

#[test]
fn delete_removes_only_the_given_paths_chunks() -> Result<(), Box<dyn Error>> {
    let (store, _directory) = store(4)?;
    let (hash1, hash2, hash3) = (hash(1), hash(2), hash(3));
    let records = vec![
        VectorRecord {
            path: "a/keep.md",
            ordinal: 0,
            heading_context: &[],
            content_hash: &hash1,
            vector: &[1.0, 0.0, 0.0, 0.0],
        },
        VectorRecord {
            path: "a/remove.md",
            ordinal: 0,
            heading_context: &[],
            content_hash: &hash2,
            vector: &[0.0, 1.0, 0.0, 0.0],
        },
        VectorRecord {
            path: "a/remove.md",
            ordinal: 1,
            heading_context: &[],
            content_hash: &hash3,
            vector: &[0.0, 0.0, 1.0, 0.0],
        },
    ];
    store.upsert(&records)?;

    store.delete("a/remove.md")?;

    let hits = store.similar(&SimilarityQuery {
        vector: &[1.0, 0.0, 0.0, 0.0],
        k: 10,
        path_prefix: None,
        exclude_paths: &[],
    })?;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, "a/keep.md");

    assert_eq!(store.existing_hash("a/remove.md", 0)?, None);
    assert_eq!(store.existing_hash("a/remove.md", 1)?, None);
    assert_eq!(store.existing_hash("a/keep.md", 0)?, Some(hash(1)));
    Ok(())
}

#[test]
fn prune_ordinals_at_or_beyond_removes_only_trailing_ordinals_for_the_given_path()
-> Result<(), Box<dyn Error>> {
    let (store, _directory) = store(4)?;
    let (hash1, hash2, hash3, hash4) = (hash(1), hash(2), hash(3), hash(4));
    store.upsert(&[
        VectorRecord {
            path: "a.md",
            ordinal: 0,
            heading_context: &[],
            content_hash: &hash1,
            vector: &[1.0, 0.0, 0.0, 0.0],
        },
        VectorRecord {
            path: "a.md",
            ordinal: 1,
            heading_context: &[],
            content_hash: &hash2,
            vector: &[0.0, 1.0, 0.0, 0.0],
        },
        VectorRecord {
            path: "a.md",
            ordinal: 2,
            heading_context: &[],
            content_hash: &hash3,
            vector: &[0.0, 0.0, 1.0, 0.0],
        },
        // A different path's ordinal 1 must survive pruning scoped to
        // "a.md".
        VectorRecord {
            path: "b.md",
            ordinal: 1,
            heading_context: &[],
            content_hash: &hash4,
            vector: &[0.0, 0.0, 0.0, 1.0],
        },
    ])?;

    store.prune_ordinals_at_or_beyond("a.md", 1)?;

    assert_eq!(store.existing_hash("a.md", 0)?, Some(hash(1)));
    assert_eq!(store.existing_hash("a.md", 1)?, None);
    assert_eq!(store.existing_hash("a.md", 2)?, None);
    assert_eq!(store.existing_hash("b.md", 1)?, Some(hash(4)));
    Ok(())
}

#[test]
fn prune_ordinals_at_or_beyond_a_path_with_no_stored_chunks_is_not_an_error()
-> Result<(), Box<dyn Error>> {
    let (store, _directory) = store(4)?;
    store.prune_ordinals_at_or_beyond("missing.md", 0)?;
    Ok(())
}

#[test]
fn path_prefix_filters_similar_results_to_matching_segments() -> Result<(), Box<dyn Error>> {
    let (store, _directory) = store(4)?;
    let (hash1, hash2, hash3, hash4) = (hash(1), hash(2), hash(3), hash(4));
    let records = vec![
        VectorRecord {
            path: "notes/a.md",
            ordinal: 0,
            heading_context: &[],
            content_hash: &hash1,
            vector: &[1.0, 0.0, 0.0, 0.0],
        },
        VectorRecord {
            path: "notes/sub/b.md",
            ordinal: 0,
            heading_context: &[],
            content_hash: &hash2,
            vector: &[1.0, 0.0, 0.0, 0.0],
        },
        VectorRecord {
            path: "other/c.md",
            ordinal: 0,
            heading_context: &[],
            content_hash: &hash3,
            vector: &[1.0, 0.0, 0.0, 0.0],
        },
        // A path that merely starts with the same characters as the
        // prefix, but not as a whole path segment, must NOT match.
        VectorRecord {
            path: "notesextra.md",
            ordinal: 0,
            heading_context: &[],
            content_hash: &hash4,
            vector: &[1.0, 0.0, 0.0, 0.0],
        },
    ];
    store.upsert(&records)?;

    let hits = store.similar(&SimilarityQuery {
        vector: &[1.0, 0.0, 0.0, 0.0],
        k: 10,
        path_prefix: Some("notes"),
        exclude_paths: &[],
    })?;

    let mut paths: Vec<&str> = hits.iter().map(|hit| hit.path.as_str()).collect();
    paths.sort_unstable();
    assert_eq!(paths, vec!["notes/a.md", "notes/sub/b.md"]);
    Ok(())
}

#[test]
fn fr_116_exclude_paths_filters_out_matching_segments() -> Result<(), Box<dyn Error>> {
    let (store, _directory) = store(4)?;
    let (hash1, hash2, hash3, hash4) = (hash(1), hash(2), hash(3), hash(4));
    let records = vec![
        VectorRecord {
            path: "notes/a.md",
            ordinal: 0,
            heading_context: &[],
            content_hash: &hash1,
            vector: &[1.0, 0.0, 0.0, 0.0],
        },
        VectorRecord {
            path: "archive/b.md",
            ordinal: 0,
            heading_context: &[],
            content_hash: &hash2,
            vector: &[1.0, 0.0, 0.0, 0.0],
        },
        VectorRecord {
            path: "archive/sub/c.md",
            ordinal: 0,
            heading_context: &[],
            content_hash: &hash3,
            vector: &[1.0, 0.0, 0.0, 0.0],
        },
        // Merely starts with the same characters as the excluded segment,
        // but is not that segment itself: must NOT be excluded.
        VectorRecord {
            path: "archiveextra.md",
            ordinal: 0,
            heading_context: &[],
            content_hash: &hash4,
            vector: &[1.0, 0.0, 0.0, 0.0],
        },
    ];
    store.upsert(&records)?;

    let hits = store.similar(&SimilarityQuery {
        vector: &[1.0, 0.0, 0.0, 0.0],
        k: 10,
        path_prefix: None,
        exclude_paths: &["archive".to_owned()],
    })?;

    let mut paths: Vec<&str> = hits.iter().map(|hit| hit.path.as_str()).collect();
    paths.sort_unstable();
    assert_eq!(paths, vec!["archiveextra.md", "notes/a.md"]);
    Ok(())
}

#[test]
fn fr_116_exclude_paths_composes_with_path_prefix() -> Result<(), Box<dyn Error>> {
    let (store, _directory) = store(4)?;
    let (hash1, hash2) = (hash(1), hash(2));
    let records = vec![
        VectorRecord {
            path: "notes/keep.md",
            ordinal: 0,
            heading_context: &[],
            content_hash: &hash1,
            vector: &[1.0, 0.0, 0.0, 0.0],
        },
        VectorRecord {
            path: "notes/old/superseded.md",
            ordinal: 0,
            heading_context: &[],
            content_hash: &hash2,
            vector: &[1.0, 0.0, 0.0, 0.0],
        },
    ];
    store.upsert(&records)?;

    let hits = store.similar(&SimilarityQuery {
        vector: &[1.0, 0.0, 0.0, 0.0],
        k: 10,
        path_prefix: Some("notes"),
        exclude_paths: &["notes/old".to_owned()],
    })?;

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, "notes/keep.md");
    Ok(())
}

#[test]
fn path_prefix_exact_match_is_included() -> Result<(), Box<dyn Error>> {
    let (store, _directory) = store(4)?;
    let record = VectorRecord {
        path: "notes",
        ordinal: 0,
        heading_context: &[],
        content_hash: &hash(1),
        vector: &[1.0, 0.0, 0.0, 0.0],
    };
    store.upsert(std::slice::from_ref(&record))?;

    let hits = store.similar(&SimilarityQuery {
        vector: &[1.0, 0.0, 0.0, 0.0],
        k: 10,
        path_prefix: Some("notes"),
        exclude_paths: &[],
    })?;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, "notes");
    Ok(())
}

#[test]
fn existing_hash_reports_none_for_an_unknown_chunk() -> Result<(), Box<dyn Error>> {
    let (store, _directory) = store(4)?;
    assert_eq!(store.existing_hash("never/seen.md", 0)?, None);
    Ok(())
}

#[test]
fn existing_hash_reports_the_stored_hash() -> Result<(), Box<dyn Error>> {
    let (store, _directory) = store(4)?;
    let record = VectorRecord {
        path: "a/one.md",
        ordinal: 3,
        heading_context: &[],
        content_hash: &hash(7),
        vector: &[1.0, 0.0, 0.0, 0.0],
    };
    store.upsert(std::slice::from_ref(&record))?;
    assert_eq!(store.existing_hash("a/one.md", 3)?, Some(hash(7)));
    Ok(())
}

#[test]
fn data_persists_across_store_close_and_reopen() -> Result<(), Box<dyn Error>> {
    let directory = tempdir()?;
    let path = directory.path().join("vectors.db");

    {
        let store = SqliteVecStore::try_from(SqliteVecConfig {
            path: path.clone(),
            dimension: 4,
        })?;
        let record = VectorRecord {
            path: "a/one.md",
            ordinal: 0,
            heading_context: &["Heading".to_owned()],
            content_hash: &hash(1),
            vector: &[1.0, 0.0, 0.0, 0.0],
        };
        store.upsert(std::slice::from_ref(&record))?;
    }

    let reopened = SqliteVecStore::try_from(SqliteVecConfig { path, dimension: 4 })?;
    let hits = reopened.similar(&SimilarityQuery {
        vector: &[1.0, 0.0, 0.0, 0.0],
        k: 10,
        path_prefix: None,
        exclude_paths: &[],
    })?;
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, "a/one.md");
    assert_eq!(hits[0].heading_context, vec!["Heading".to_owned()]);
    Ok(())
}

#[test]
fn two_stores_in_one_process_both_work_after_extension_registration() -> Result<(), Box<dyn Error>>
{
    let (first, _first_dir) = store(3)?;
    let (second, _second_dir) = store(3)?;

    first.upsert(&[VectorRecord {
        path: "first.md",
        ordinal: 0,
        heading_context: &[],
        content_hash: &hash(1),
        vector: &[1.0, 0.0, 0.0],
    }])?;
    second.upsert(&[VectorRecord {
        path: "second.md",
        ordinal: 0,
        heading_context: &[],
        content_hash: &hash(2),
        vector: &[0.0, 1.0, 0.0],
    }])?;

    let first_hits = first.similar(&SimilarityQuery {
        vector: &[1.0, 0.0, 0.0],
        k: 10,
        path_prefix: None,
        exclude_paths: &[],
    })?;
    let second_hits = second.similar(&SimilarityQuery {
        vector: &[0.0, 1.0, 0.0],
        k: 10,
        path_prefix: None,
        exclude_paths: &[],
    })?;

    assert_eq!(first_hits.len(), 1);
    assert_eq!(first_hits[0].path, "first.md");
    assert_eq!(second_hits.len(), 1);
    assert_eq!(second_hits[0].path, "second.md");
    Ok(())
}

#[test]
fn upsert_rejects_a_vector_of_the_wrong_dimension() -> Result<(), Box<dyn Error>> {
    let (store, _directory) = store(4)?;
    let record = VectorRecord {
        path: "a/one.md",
        ordinal: 0,
        heading_context: &[],
        content_hash: &hash(1),
        vector: &[1.0, 0.0, 0.0], // 3 components, store configured for 4
    };
    let Err(error) = store.upsert(std::slice::from_ref(&record)) else {
        return Err("dimension mismatch must be rejected".into());
    };
    assert!(matches!(error, SearchError::VectorDimensionMismatch { .. }));
    assert_eq!(error.code(), "vector/config");
    Ok(())
}

#[test]
fn construction_rejects_a_zero_dimension() -> Result<(), Box<dyn Error>> {
    let directory = tempdir()?;
    let path = directory.path().join("vectors.db");
    let Err(error) = SqliteVecStore::try_from(SqliteVecConfig { path, dimension: 0 }) else {
        return Err("zero dimension must be rejected".into());
    };
    assert!(matches!(error, SearchError::VectorDimensionInvalid { .. }));
    Ok(())
}

#[test]
fn k_zero_returns_no_hits_without_error() -> Result<(), Box<dyn Error>> {
    let (store, _directory) = store(4)?;
    store.upsert(&[VectorRecord {
        path: "a/one.md",
        ordinal: 0,
        heading_context: &[],
        content_hash: &hash(1),
        vector: &[1.0, 0.0, 0.0, 0.0],
    }])?;
    let hits = store.similar(&SimilarityQuery {
        vector: &[1.0, 0.0, 0.0, 0.0],
        k: 0,
        path_prefix: None,
        exclude_paths: &[],
    })?;
    assert!(hits.is_empty());
    Ok(())
}

#[test]
fn stats_counts_distinct_documents_and_total_chunks() -> Result<(), Box<dyn Error>> {
    let (store, _directory) = store(4)?;
    let empty = store.stats()?;
    assert_eq!(empty.documents, 0);
    assert_eq!(empty.chunks, 0);

    store.upsert(&[
        VectorRecord {
            path: "a.md",
            ordinal: 0,
            heading_context: &[],
            content_hash: &hash(1),
            vector: &[1.0, 0.0, 0.0, 0.0],
        },
        VectorRecord {
            path: "a.md",
            ordinal: 1,
            heading_context: &[],
            content_hash: &hash(2),
            vector: &[0.0, 1.0, 0.0, 0.0],
        },
        VectorRecord {
            path: "b.md",
            ordinal: 0,
            heading_context: &[],
            content_hash: &hash(3),
            vector: &[0.0, 0.0, 1.0, 0.0],
        },
    ])?;

    let stats = store.stats()?;
    assert_eq!(stats.documents, 2);
    assert_eq!(stats.chunks, 3);

    store.delete("a.md")?;
    let after_delete = store.stats()?;
    assert_eq!(after_delete.documents, 1);
    assert_eq!(after_delete.chunks, 1);
    Ok(())
}

proptest! {
    /// Property: across many synthetic path/chunk combinations, deleting one
    /// path never removes any chunk belonging to a different path.
    #[test]
    fn delete_by_path_never_touches_other_paths(
        target_index in 0usize..6,
        ordinals in prop::collection::vec(0usize..4, 6),
    ) {
        let (store, _directory) = match store(3) {
            Ok(value) => value,
            Err(error) => return Err(TestCaseError::fail(error.to_string())),
        };
        let paths = ["a/one.md", "a/two.md", "b/three.md", "b/four.md", "c/five.md", "c/six.md"];
        let zero_hash = hash(0);
        let vectors: Vec<[f32; 3]> = (0..paths.len())
            .map(|index| {
                let component = f32::from(u8::try_from(index).unwrap_or(0));
                [component, 1.0, 0.0]
            })
            .collect();

        let owned_records: Vec<VectorRecord<'_>> = paths
            .iter()
            .zip(ordinals.iter())
            .zip(vectors.iter())
            .map(|((path, ordinal), vector)| VectorRecord {
                path,
                ordinal: *ordinal,
                heading_context: &[],
                content_hash: &zero_hash,
                vector,
            })
            .collect();
        if let Err(error) = store.upsert(&owned_records) {
            return Err(TestCaseError::fail(error.to_string()));
        }

        let target_path = paths[target_index];
        if let Err(error) = store.delete(target_path) {
            return Err(TestCaseError::fail(error.to_string()));
        }

        for (index, path) in paths.iter().enumerate() {
            let ordinal = ordinals[index];
            let stored = match store.existing_hash(path, ordinal) {
                Ok(value) => value,
                Err(error) => return Err(TestCaseError::fail(error.to_string())),
            };
            if index == target_index {
                prop_assert_eq!(stored, None, "deleted path must have no remaining chunk");
            } else {
                prop_assert_eq!(stored, Some(zero_hash.clone()), "untouched path must keep its chunk");
            }
        }
    }
}
