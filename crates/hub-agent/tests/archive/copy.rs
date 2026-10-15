use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::Path;

use hub_agent::archive::{
    ArchiveError, Entry, collect_tree, copy_plain, entry_under, export_files, place_all,
};
use hub_core::snapshot::{FILES_LIMIT, RelPath};

fn rel(raw: &str) -> RelPath {
    RelPath::parse(raw).unwrap()
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn pair(from: &str, to: &str) -> (RelPath, RelPath) {
    (rel(from), rel(to))
}

#[test]
fn listed_files_are_copied_under_the_same_paths() {
    let (root, dest) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    write(&rel("projects/p/id.jsonl").under(root.path()), "{}\n");
    write(&root.path().join(".credentials.json"), "secret");

    export_files(root.path(), &[rel("projects/p/id.jsonl")], dest.path()).unwrap();

    assert_eq!(fs::read_to_string(rel("projects/p/id.jsonl").under(dest.path())).unwrap(), "{}\n");
    assert!(!dest.path().join(".credentials.json").exists());
}

#[cfg(unix)]
#[test]
fn exported_files_and_directories_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let (root, dest) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    write(&rel("projects/p/id.jsonl").under(root.path()), "{}\n");
    export_files(root.path(), &[rel("projects/p/id.jsonl")], dest.path()).unwrap();
    let mode =
        |raw: &str| fs::metadata(rel(raw).under(dest.path())).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        (mode("projects"), mode("projects/p"), mode("projects/p/id.jsonl")),
        (0o700, 0o700, 0o600)
    );
}

#[cfg(unix)]
#[test]
fn a_linked_source_file_is_refused_not_followed() {
    let (root, dest, outside) =
        (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    write(&outside.path().join("key"), "secret");
    fs::create_dir_all(root.path().join("projects").join("p")).unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("key"),
        rel("projects/p/id.jsonl").under(root.path()),
    )
    .unwrap();

    let result = export_files(root.path(), &[rel("projects/p/id.jsonl")], dest.path());

    assert!(matches!(result, Err(ArchiveError::Link(_))));
    assert!(!rel("projects/p/id.jsonl").under(dest.path()).exists());
}

#[cfg(unix)]
#[test]
fn a_linked_parent_on_the_source_side_is_refused() {
    let (root, dest, outside) =
        (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    write(&rel("p/id.jsonl").under(outside.path()), "secret");
    std::os::unix::fs::symlink(outside.path(), root.path().join("projects")).unwrap();

    assert!(matches!(
        entry_under(root.path(), &rel("projects/p/id.jsonl")),
        Err(ArchiveError::Link(_))
    ));
    let result = export_files(root.path(), &[rel("projects/p/id.jsonl")], dest.path());

    assert!(matches!(result, Err(ArchiveError::Link(_))));
    assert_eq!(fs::read_dir(dest.path()).unwrap().count(), 0);
}

#[test]
fn entries_are_classified_without_following_anything() {
    let root = tempfile::tempdir().unwrap();
    write(&rel("a/b.txt").under(root.path()), "b");
    assert_eq!(entry_under(root.path(), &rel("a/b.txt")).unwrap(), Entry::File);
    assert_eq!(entry_under(root.path(), &rel("a")).unwrap(), Entry::Dir);
    assert_eq!(entry_under(root.path(), &rel("a/missing/c")).unwrap(), Entry::Missing);
    assert!(matches!(entry_under(root.path(), &rel("a/b.txt/c")), Err(ArchiveError::Malformed(_))));
}

#[test]
fn a_file_in_place_of_a_destination_directory_is_malformed() {
    let (src, dest) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    write(&src.path().join("a"), "a");
    write(&dest.path().join("x"), "a file, not a directory");

    let result = place_all(src.path(), dest.path(), &[pair("a", "x/a")], &rel("x/a"), copy_plain);

    assert!(matches!(result, Err(ArchiveError::Malformed(_))));
    assert_eq!(fs::read_to_string(dest.path().join("x")).unwrap(), "a file, not a directory");
}

#[test]
fn a_missing_source_file_is_malformed_and_nothing_is_created() {
    let (src, dest) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());

    let result = place_all(src.path(), dest.path(), &[pair("a", "x/a")], &rel("x/a"), copy_plain);

    assert!(matches!(result, Err(ArchiveError::Malformed(_))));
    assert_eq!(fs::read_dir(dest.path()).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn a_linked_last_component_is_classified_as_a_link() {
    let (root, outside) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    write(&outside.path().join("key"), "secret");
    std::os::unix::fs::symlink(outside.path(), root.path().join("dir-link")).unwrap();
    std::os::unix::fs::symlink(outside.path().join("key"), root.path().join("file-link")).unwrap();

    assert_eq!(entry_under(root.path(), &rel("dir-link")).unwrap(), Entry::Link);
    assert_eq!(entry_under(root.path(), &rel("file-link")).unwrap(), Entry::Link);
}

#[cfg(unix)]
#[test]
fn a_linked_destination_file_is_not_written_through() {
    let (src, dest, outside) =
        (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    write(&src.path().join("a"), "new");
    write(&outside.path().join("target"), "old");
    std::os::unix::fs::symlink(outside.path().join("target"), dest.path().join("a")).unwrap();

    let result = place_all(src.path(), dest.path(), &[pair("a", "a")], &rel("a"), copy_plain);

    assert!(
        matches!(result, Err(ArchiveError::Io(ref error)) if error.kind() == io::ErrorKind::AlreadyExists)
    );
    assert_eq!(fs::read_to_string(outside.path().join("target")).unwrap(), "old");
}

#[test]
fn trees_list_regular_files_sorted_and_missing_trees_are_empty() {
    let root = tempfile::tempdir().unwrap();
    write(&rel("s/id/b.txt").under(root.path()), "b");
    write(&rel("s/id/a/c.txt").under(root.path()), "c");
    assert_eq!(
        collect_tree(root.path(), &rel("s/id")).unwrap(),
        [rel("s/id/a/c.txt"), rel("s/id/b.txt")]
    );
    assert_eq!(collect_tree(root.path(), &rel("s/missing")).unwrap(), Vec::<RelPath>::new());
}

#[cfg(unix)]
#[test]
fn trees_skip_links_inside_and_refuse_links_above() {
    let (root, outside) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    write(&outside.path().join("secret"), "x");
    write(&rel("id/x.txt").under(outside.path()), "x");
    write(&rel("s/id/a.txt").under(root.path()), "a");
    std::os::unix::fs::symlink(
        outside.path().join("secret"),
        rel("s/id/file-link").under(root.path()),
    )
    .unwrap();
    std::os::unix::fs::symlink(outside.path(), rel("s/id/dir-link").under(root.path())).unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("linked")).unwrap();

    assert_eq!(collect_tree(root.path(), &rel("s/id")).unwrap(), [rel("s/id/a.txt")]);
    assert_eq!(collect_tree(root.path(), &rel("linked")).unwrap(), Vec::<RelPath>::new());
    assert!(matches!(collect_tree(root.path(), &rel("linked/id")), Err(ArchiveError::Link(_))));
}

#[test]
fn placing_never_overwrites_and_rolls_back() {
    let (src, dest) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    write(&src.path().join("a"), "new a");
    write(&src.path().join("b"), "new b");
    write(&rel("x/b").under(dest.path()), "old b");

    let result = place_all(
        src.path(),
        dest.path(),
        &[pair("a", "x/a"), pair("b", "x/b")],
        &rel("x/b"),
        copy_plain,
    );

    assert!(
        matches!(result, Err(ArchiveError::Io(ref error)) if error.kind() == io::ErrorKind::AlreadyExists)
    );
    assert!(!rel("x/a").under(dest.path()).exists());
    assert_eq!(fs::read_to_string(rel("x/b").under(dest.path())).unwrap(), "old b");
}

#[test]
fn a_failed_write_leaves_nothing_behind() {
    let (src, dest) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    write(&src.path().join("a"), "a");
    write(&src.path().join("b"), "b");

    let result = place_all(
        src.path(),
        dest.path(),
        &[pair("a", "x/a"), pair("b", "x/b")],
        &rel("x/b"),
        |file, from, out| {
            if file.as_str() == "b" {
                Err(io::Error::other("boom"))
            } else {
                copy_plain(file, from, out)
            }
        },
    );

    assert!(matches!(result, Err(ArchiveError::Io(_))));
    assert!(!rel("x/a").under(dest.path()).exists() && !rel("x/b").under(dest.path()).exists());
}

#[test]
fn placed_files_carry_what_the_writer_wrote() {
    let (src, dest) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    write(&src.path().join("a"), "abc");
    place_all(
        src.path(),
        dest.path(),
        &[pair("a", "y/a")],
        &rel("y/a"),
        |_file, mut from: File, out| {
            let mut text = String::new();
            from.read_to_string(&mut text)?;
            out.write_all(text.to_uppercase().as_bytes())
        },
    )
    .unwrap();
    assert_eq!(fs::read_to_string(rel("y/a").under(dest.path())).unwrap(), "ABC");
}

#[cfg(unix)]
#[test]
fn a_linked_parent_on_the_destination_side_is_refused() {
    let (src, dest, outside) =
        (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    write(&src.path().join("a"), "a");
    std::os::unix::fs::symlink(outside.path(), dest.path().join("x")).unwrap();

    let result =
        place_all(src.path(), dest.path(), &[pair("a", "x/sub/a")], &rel("x/sub/a"), copy_plain);

    assert!(matches!(result, Err(ArchiveError::Link(_))));
    assert_eq!(
        fs::read_dir(outside.path()).unwrap().count(),
        0,
        "nothing written outside the root"
    );
}

#[cfg(unix)]
#[test]
fn a_linked_parent_on_the_snapshot_side_is_refused() {
    let (src, dest, outside) =
        (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    write(&rel("p/id.jsonl").under(outside.path()), "planted");
    std::os::unix::fs::symlink(outside.path(), src.path().join("projects")).unwrap();

    let result = place_all(
        src.path(),
        dest.path(),
        &[pair("projects/p/id.jsonl", "projects/p/id.jsonl")],
        &rel("projects/p/id.jsonl"),
        copy_plain,
    );

    assert!(matches!(result, Err(ArchiveError::Link(_))));
    assert_eq!(fs::read_dir(dest.path()).unwrap().count(), 0);
}

/// The first half of a file a crash cut short.
fn half(out: &mut File) -> io::Result<()> {
    out.write_all(b"{\"half\"")?;
    out.sync_all()
}

fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn a_crash_mid_write_leaves_no_final_file_and_does_not_block_a_retry() {
    let (src, dest) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    write(&src.path().join("a"), "{\"whole\":true}\n");

    let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        place_all(src.path(), dest.path(), &[pair("a", "x/a")], &rel("x/a"), |_file, _from, out| {
            half(out)?;
            panic!("the hub was killed mid-import");
        })
    }));

    assert!(crashed.is_err());
    assert!(!rel("x/a").under(dest.path()).exists(), "a partial file under the final name");
    let leftovers = names_in(&dest.path().join("x"));
    assert_eq!(leftovers.len(), 1, "{leftovers:?}");
    place_all(src.path(), dest.path(), &[pair("a", "x/a")], &rel("x/a"), copy_plain).unwrap();
    assert_eq!(fs::read_to_string(rel("x/a").under(dest.path())).unwrap(), "{\"whole\":true}\n");
    assert!(names_in(&dest.path().join("x")).contains(&leftovers[0]), "not ours to remove");
}

#[test]
fn the_presence_file_is_placed_after_every_other_file() {
    let (src, dest) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    write(&src.path().join("a"), "a");
    write(&src.path().join("b"), "b");

    let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        place_all(
            src.path(),
            dest.path(),
            &[pair("a", "x/a"), pair("b", "x/b")],
            &rel("x/a"),
            |file, from, out| {
                if file.as_str() == "b" {
                    half(out)?;
                    panic!("the hub was killed mid-import");
                }
                copy_plain(file, from, out)
            },
        )
    }));

    assert!(crashed.is_err());
    assert!(!rel("x/a").under(dest.path()).exists(), "the session looks present");
    assert!(!rel("x/b").under(dest.path()).exists());
}

#[test]
fn placing_without_the_presence_file_is_refused() {
    let (src, dest) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    write(&src.path().join("a"), "a");

    let result = place_all(src.path(), dest.path(), &[pair("a", "x/a")], &rel("x/b"), copy_plain);

    assert!(matches!(result, Err(ArchiveError::Malformed(_))));
    assert_eq!(fs::read_dir(dest.path()).unwrap().count(), 0);
}

#[test]
fn export_refuses_more_files_than_a_manifest_holds() {
    let (root, dest) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    write(&root.path().join("a"), "a");

    let result = export_files(root.path(), &vec![rel("a"); FILES_LIMIT + 1], dest.path());

    assert!(matches!(result, Err(ArchiveError::Malformed(_))));
    assert_eq!(fs::read_dir(dest.path()).unwrap().count(), 0);
}

#[test]
fn a_retry_after_a_crash_keeps_the_files_already_placed() {
    let (src, dest) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    for name in ["a", "b", "c"] {
        write(&src.path().join(name), name);
    }
    let pairs = [pair("a", "x/a"), pair("b", "x/b"), pair("c", "x/c")];

    let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        place_all(src.path(), dest.path(), &pairs, &rel("x/a"), |file, from, out| {
            if file.as_str() == "c" {
                half(out)?;
                panic!("the hub was killed mid-import");
            }
            copy_plain(file, from, out)
        })
    }));
    assert!(crashed.is_err());
    assert!(rel("x/b").under(dest.path()).is_file() && !rel("x/a").under(dest.path()).exists());

    place_all(src.path(), dest.path(), &pairs, &rel("x/a"), copy_plain).unwrap();

    for name in ["a", "b", "c"] {
        assert_eq!(fs::read_to_string(rel(&format!("x/{name}")).under(dest.path())).unwrap(), name);
    }
}

#[test]
fn an_existing_file_with_other_bytes_is_refused_and_kept() {
    let (src, dest) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    write(&src.path().join("a"), "new a");
    write(&src.path().join("b"), "new b");
    write(&rel("x/b").under(dest.path()), "old b");

    let result = place_all(
        src.path(),
        dest.path(),
        &[pair("a", "x/a"), pair("b", "x/b")],
        &rel("x/a"),
        copy_plain,
    );

    assert!(
        matches!(result, Err(ArchiveError::Io(ref error)) if error.kind() == io::ErrorKind::AlreadyExists)
    );
    assert_eq!(fs::read_to_string(rel("x/b").under(dest.path())).unwrap(), "old b");
    assert_eq!(names_in(&dest.path().join("x")), ["b"], "no temporary file left");
}

#[test]
fn an_existing_presence_file_is_refused_even_with_the_same_bytes() {
    let (src, dest) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    write(&src.path().join("a"), "a");
    write(&rel("x/a").under(dest.path()), "a");

    let result = place_all(src.path(), dest.path(), &[pair("a", "x/a")], &rel("x/a"), copy_plain);

    assert!(
        matches!(result, Err(ArchiveError::Io(ref error)) if error.kind() == io::ErrorKind::AlreadyExists)
    );
}

#[cfg(unix)]
#[test]
fn an_existing_link_is_never_taken_for_a_placed_file() {
    let (src, dest, outside) =
        (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    write(&src.path().join("a"), "a");
    write(&src.path().join("b"), "b");
    write(&outside.path().join("b"), "b");
    fs::create_dir(dest.path().join("x")).unwrap();
    std::os::unix::fs::symlink(outside.path().join("b"), rel("x/b").under(dest.path())).unwrap();

    let result = place_all(
        src.path(),
        dest.path(),
        &[pair("a", "x/a"), pair("b", "x/b")],
        &rel("x/a"),
        copy_plain,
    );

    assert!(result.is_err());
    assert!(!rel("x/a").under(dest.path()).exists());
}
