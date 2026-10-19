//! Проверка, что закоммиченная JSON Schema соответствует Rust-типам.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

fn schema_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schema/rchat.schema.json")
}

#[test]
fn test_schema_is_up_to_date() {
    let generated = rchat_ffi::schema::schema_json().expect("schema is serializable");
    let actual = fs::read_to_string(schema_path()).unwrap_or_else(|error| {
        panic!(
            "cannot read {}: {error}; run `cargo run -p rchat-ffi --bin export_schema`",
            schema_path().display()
        )
    });
    assert_eq!(
        actual, generated,
        "schema/rchat.schema.json is stale; run \
         `cargo run -p rchat-ffi --bin export_schema`"
    );
}

#[test]
fn schema_is_valid_and_covers_protocol() {
    let text = rchat_ffi::schema::schema_json().expect("schema is serializable");
    let schema: Value = serde_json::from_str(&text).expect("schema is valid JSON");
    assert_eq!(schema["$schema"], "http://json-schema.org/draft-07/schema#");
    assert_eq!(schema["title"], "rchat-ffi JSON protocol");

    let defs = schema["definitions"]
        .as_object()
        .expect("schema has definitions");
    let commands = defs["Command"]["oneOf"]
        .as_array()
        .expect("Command is a union");
    assert_eq!(commands.len(), 34, "all documented commands are present");
    for command in commands {
        assert!(
            command["properties"]["@type"]["enum"]
                .as_array()
                .is_some_and(|values| values.len() == 1),
            "every command has a single @type discriminator: {command}"
        );
    }

    let events = defs["Event"]["anyOf"].as_array().expect("Event is a union");
    assert_eq!(events.len(), 18, "all documented events are present");

    for name in [
        "Avatar",
        "Chat",
        "ChatMember",
        "Session",
        "Account",
        "Message",
        "Call",
        "OkResponse",
        "ErrorResponse",
    ] {
        assert!(defs.contains_key(name), "definition {name} is missing");
    }
    for name in [
        "MessageReceivedEvent",
        "OutboxChangedEvent",
        "CallInvitedEvent",
        "EventLaggedEvent",
    ] {
        assert!(defs.contains_key(name), "event {name} is missing");
    }
}
