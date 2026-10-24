//! Inventory discovery from the ordinary product library, without cfg(test).

#[test]
fn product_library_links_every_facade_capability() {
    use std::collections::BTreeSet;
    let registry = knot::host_operation_registry();
    let bootstrap = include_str!("../src/host/js/bootstrap.js");
    let mut expected = BTreeSet::new();
    for tail in bootstrap.split("request(").skip(1) {
        let tail = tail.trim_start();
        if let Some(tail) = tail.strip_prefix('"') {
            expected.insert(tail.split('"').next().unwrap());
        }
    }
    for tail in bootstrap.split("requestForInvocation(").skip(1) {
        if let Some((_, arguments)) = tail.split_once(',')
            && let Some(tail) = arguments.trim_start().strip_prefix('"')
        {
            expected.insert(tail.split('"').next().unwrap());
        }
    }
    // Ordered keybinding batches originate in the native turn boundary.
    expected.insert("mutateKeybindings");
    assert_eq!(registry.names().collect::<BTreeSet<_>>(), expected);
    assert!(registry.error_codes().contains(&"ResourceWrongKind"));
}
