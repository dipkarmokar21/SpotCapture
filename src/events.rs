use serde_json::{Value, json};
use std::io::{self, Write};

pub fn emit(value: Value) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{value}");
    let _ = stdout.flush();
}

pub fn status(message: impl AsRef<str>) {
    emit(json!({"type": "status", "message": message.as_ref()}));
}

