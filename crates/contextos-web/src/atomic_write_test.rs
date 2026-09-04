use tempfile::tempdir;

use super::*;

#[test]
fn writes_a_new_file_with_the_given_contents() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = dir.path().join("target.txt");

    write_atomically(&path, b"hello")?;

    assert_eq!(std::fs::read(&path)?, b"hello");
    Ok(())
}

#[test]
fn overwrites_an_existing_file_in_place() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = dir.path().join("target.txt");
    std::fs::write(&path, b"old")?;

    write_atomically(&path, b"new")?;

    assert_eq!(std::fs::read(&path)?, b"new");
    Ok(())
}

#[test]
fn leaves_no_temporary_file_behind_after_a_successful_write() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = dir.path().join("target.txt");

    write_atomically(&path, b"hello")?;

    let leftovers: Vec<_> = std::fs::read_dir(dir.path())?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name() != "target.txt")
        .collect();
    assert!(leftovers.is_empty(), "expected no leftover files, found {leftovers:?}");
    Ok(())
}

#[test]
fn fails_without_writing_when_the_target_directory_does_not_exist() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let path = dir.path().join("missing-subdir").join("target.txt");

    let result = write_atomically(&path, b"hello");

    assert!(result.is_err());
    assert!(!path.exists());
    Ok(())
}
