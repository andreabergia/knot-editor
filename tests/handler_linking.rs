//! Inventory discovery from the ordinary product library, without cfg(test).

#[test]
fn product_library_links_declared_handlers() {
    let registry = knot::host::declarations::registry();
    assert!(registry.get("openDocument").is_some());
    assert!(registry.get("writeClipboardText").is_some());
    assert!(registry.error_codes().contains(&"ResourceWrongKind"));
}
