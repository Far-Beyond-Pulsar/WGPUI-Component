use ui::deduplicate_hierarchy_ids;

#[test]
fn duplicate_child_ids_are_emitted_once_in_first_seen_order() {
    let ids = deduplicate_hierarchy_ids(["a", "b", "a", "c", "b"]);
    assert_eq!(ids, vec!["a", "b", "c"]);
}

#[test]
fn empty_child_lists_remain_empty() {
    let ids: Vec<&str> = deduplicate_hierarchy_ids(std::iter::empty());
    assert!(ids.is_empty());
}
