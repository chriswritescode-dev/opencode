use std::path::PathBuf;

use opencode_sidecar::protocol::{LocationRef, LogLevel, MAGIC, Message, RemoteError, VERSION};
use ts_rs::TS;

const HEADER: &str = "// Generated from packages/server-rs/crates/sidecar/src/protocol.rs by ts-rs. Do not edit.";

fn bindings() -> String {
    let config = ts_rs::Config::default();
    let declarations = [
        serde_json::Value::decl(&config),
        LocationRef::decl(&config),
        RemoteError::decl(&config),
        LogLevel::decl(&config),
        Message::decl(&config),
    ];
    let mut text = String::from(HEADER);
    text.push('\n');
    for declaration in declarations {
        text.push_str("export ");
        text.push_str(declaration.trim_end());
        text.push('\n');
    }
    text.push_str(&format!("export const VERSION = {VERSION};\n"));
    text.push_str(&format!("export const MAGIC = \"{MAGIC}\";\n"));
    text
}

#[test]
fn typescript_bindings_are_current() {
    let text = bindings();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../host/src/protocol.gen.ts");
    if std::env::var("OPENCODE_UPDATE_SIDECAR_PROTOCOL").as_deref() == Ok("1") {
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("a directory");
        std::fs::write(&path, &text).expect("a write");
        return;
    }
    let committed = std::fs::read_to_string(&path).expect("the committed bindings");
    assert_eq!(committed, text, "regenerate with OPENCODE_UPDATE_SIDECAR_PROTOCOL=1");
}
