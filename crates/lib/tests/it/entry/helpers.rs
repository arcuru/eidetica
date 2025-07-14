use eidetica::entry::Entry;

/// Create a simple test entry with the given root
pub fn create_entry(root: &str) -> Entry {
    Entry::builder(root).build()
}

/// Create a root entry (top-level entry)
pub fn create_root_entry() -> Entry {
    Entry::root_builder().build()
}

/// Create an empty entry (with empty string root)
pub fn create_empty_entry() -> Entry {
    Entry::builder("").build()
}

/// Create a test entry with parents
pub fn create_entry_with_parents(root: &str, parents: &[&str]) -> Entry {
    Entry::builder(root)
        .set_parents(parents.iter().map(|p| (*p).into()).collect())
        .build()
}

/// Create a test entry with multiple subtrees
pub fn create_entry_with_subtrees(root: &str, subtrees: &[(&str, &str)]) -> Entry {
    let mut builder = Entry::builder(root);
    for (name, data) in subtrees {
        builder.set_subtree_data_mut(*name, *data);
    }
    builder.build()
}

/// Create a test entry with subtree and subtree parents
pub fn create_entry_with_subtree_parents(
    root: &str,
    subtree_name: &str,
    data: &str,
    parents: &[&str],
) -> Entry {
    Entry::builder(root)
        .set_subtree_data(subtree_name, data)
        .set_subtree_parents(subtree_name, parents.iter().map(|p| (*p).into()).collect())
        .build()
}

/// Create a test entry with a single subtree
pub fn create_entry_with_subtree(root: &str, subtree_name: &str, data: &str) -> Entry {
    Entry::builder(root)
        .set_subtree_data(subtree_name, data)
        .build()
}

/// Assert that two entries have the same ID (for determinism tests)
pub fn assert_same_id(entry1: &Entry, entry2: &Entry) {
    assert_eq!(entry1.id(), entry2.id(), "Entries should have the same ID");
}

/// Assert that two entries have different IDs
pub fn assert_different_id(entry1: &Entry, entry2: &Entry) {
    assert_ne!(
        entry1.id(),
        entry2.id(),
        "Entries should have different IDs"
    );
}

/// Assert that an entry has the expected parents
pub fn assert_has_parents(entry: &Entry, expected_parents: &[&str]) {
    let parents = entry.parents().unwrap();
    assert_eq!(parents.len(), expected_parents.len());
    for parent in expected_parents {
        assert!(
            parents.contains(&(*parent).into()),
            "Missing parent: {parent}"
        );
    }
}

/// Assert that a subtree has the expected parents
pub fn assert_subtree_has_parents(entry: &Entry, subtree_name: &str, expected_parents: &[&str]) {
    let parents = entry.subtree_parents(subtree_name).unwrap();
    assert_eq!(parents.len(), expected_parents.len());
    for parent in expected_parents {
        assert!(
            parents.contains(&(*parent).into()),
            "Missing subtree parent: {parent}"
        );
    }
}

/// Assert that an entry has the expected subtrees with their data
pub fn assert_has_subtrees(entry: &Entry, expected_subtrees: &[&str]) {
    let subtrees = entry.subtrees();
    assert_eq!(subtrees.len(), expected_subtrees.len());
    for subtree in expected_subtrees {
        assert!(
            subtrees.contains(&subtree.to_string()),
            "Missing subtree: {subtree}"
        );
    }
}

/// Assert that an entry has the expected subtrees with their data
pub fn assert_subtrees_with_data(entry: &Entry, expected: &[(&str, &str)]) {
    let subtrees = entry.subtrees();
    assert_eq!(subtrees.len(), expected.len());
    for (name, expected_data) in expected {
        assert!(
            subtrees.contains(&name.to_string()),
            "Missing subtree: {name}"
        );
        assert_eq!(
            entry.data(name).unwrap(),
            *expected_data,
            "Wrong data for subtree {name}"
        );
    }
}

/// Assert that parents are sorted correctly for both main tree and subtrees
pub fn assert_parents_sorted(
    entry: &Entry,
    expected_main: &[&str],
    subtree_checks: &[(&str, &[&str])],
) {
    // Check main tree parents are sorted
    let main_parents = entry.parents().unwrap();
    let expected_main_sorted: Vec<String> = expected_main.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        main_parents, expected_main_sorted,
        "Main tree parents not sorted correctly"
    );

    // Check subtree parents are sorted
    for (subtree_name, expected_parents) in subtree_checks {
        let subtree_parents = entry.subtree_parents(subtree_name).unwrap();
        let expected_sorted: Vec<String> = expected_parents.iter().map(|s| s.to_string()).collect();
        assert_eq!(
            subtree_parents, expected_sorted,
            "Subtree {subtree_name} parents not sorted correctly"
        );
    }
}

/// Assert that entry has no parents (empty parents list)
pub fn assert_no_parents(entry: &Entry) {
    assert!(
        entry.parents().unwrap().is_empty(),
        "Entry should have no parents"
    );
}

/// Assert that subtree has no parents (empty parents list)
pub fn assert_subtree_no_parents(entry: &Entry, subtree_name: &str) {
    assert!(
        entry.subtree_parents(subtree_name).unwrap().is_empty(),
        "Subtree {subtree_name} should have no parents"
    );
}

/// Create a complex entry with multiple subtrees, parents, and subtree parents for determinism testing
pub fn create_complex_entry_with_order(root: &str, reverse_order: bool) -> Entry {
    let mut builder = Entry::builder(root);

    if reverse_order {
        // Add everything in reverse order
        builder.set_parents_mut(vec!["p3".into(), "p2".into(), "p1".into()]);
        builder.set_subtree_data_mut("sub3", "data3");
        builder.set_subtree_data_mut("sub2", "data2");
        builder.set_subtree_data_mut("sub1", "data1");
        builder.set_subtree_parents_mut("sub2", vec!["sp3".into()]);
        builder.set_subtree_parents_mut("sub1", vec!["sp2".into(), "sp1".into()]);
    } else {
        // Add everything in normal order
        builder.set_parents_mut(vec!["p1".into(), "p2".into(), "p3".into()]);
        builder.set_subtree_data_mut("sub1", "data1");
        builder.set_subtree_data_mut("sub2", "data2");
        builder.set_subtree_data_mut("sub3", "data3");
        builder.set_subtree_parents_mut("sub1", vec!["sp1".into(), "sp2".into()]);
        builder.set_subtree_parents_mut("sub2", vec!["sp3".into()]);
    }

    builder.build()
}

/// Create an entry with unsorted parents for testing sorting behavior
pub fn create_entry_with_unsorted_parents(
    root: &str,
    parents: &[&str],
    subtree_parents: &[(&str, &[&str])],
) -> Entry {
    let mut builder = Entry::builder(root);

    // Add parents
    builder.set_parents_mut(parents.iter().map(|p| (*p).into()).collect());

    // Add subtrees with parents
    for (subtree_name, subtree_parent_list) in subtree_parents {
        builder.set_subtree_data_mut(*subtree_name, "{}");
        builder.set_subtree_parents_mut(
            *subtree_name,
            subtree_parent_list.iter().map(|p| (*p).into()).collect(),
        );
    }

    builder.build()
}

/// Create an entry with duplicate parents to test deduplication
pub fn create_entry_with_duplicate_parents(root: &str, parents_with_dupes: &[&str]) -> Entry {
    Entry::builder(root)
        .set_parents(parents_with_dupes.iter().map(|p| (*p).into()).collect())
        .build()
}
