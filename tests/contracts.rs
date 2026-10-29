use rulescan::cli::{SchemaKind, schema};

#[test]
fn published_contracts_match_the_executable() {
    for (kind, name) in [
        (SchemaKind::Config, "config.schema.json"),
        (SchemaKind::Output, "output.schema.json"),
        (SchemaKind::Plan, "plan.schema.json"),
        (SchemaKind::Quarantine, "quarantine.schema.json"),
        (SchemaKind::Event, "event.schema.json"),
        (SchemaKind::Commands, "commands.json"),
    ] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("schemas")
            .join(name);
        let checked_in: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(
            schema(kind),
            checked_in,
            "public contract changed: regenerate {name} and review compatibility"
        );
    }
}
