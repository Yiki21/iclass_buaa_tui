//! The CLI must start without `HOME`, which Windows does not set (issue #18).

use std::{fs, path::PathBuf, process::Command};

fn scratch_dir(name: &str) -> PathBuf {

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);

    let _ = fs::remove_dir_all(&dir);

    fs::create_dir_all(&dir).expect("create scratch dir");

    dir
}

#[test]

fn schema_runs_without_home() {

    let dir = scratch_dir("no-home");

    let log = dir.join("events.jsonl");

    let output = Command::new(env!("CARGO_BIN_EXE_iclass_buaa_tui"))
        .arg("--log-file")
        .arg(&log)
        .arg("schema")
        .env_remove("HOME")
        .env_remove("XDG_STATE_HOME")
        .env_remove("XDG_CONFIG_HOME")
        .output()
        .expect("run binary");

    assert!(
        output.status.success(),
        "exit {:?}, stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let schema: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("schema prints JSON");

    assert!(schema.is_object() || schema.is_array());

    let written = fs::read_to_string(&log).expect("log written to --log-file");

    assert!(written.contains("CLI started"));
}
