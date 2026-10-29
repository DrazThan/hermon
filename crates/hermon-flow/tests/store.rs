use hermon_flow::store::RunStore;
use serde::{Serialize, Serializer, ser::SerializeMap};
use serde_json::{Value, json};
use std::{fs, io::Read};

struct BrokenSerialization;

impl Serialize for BrokenSerialization {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry("partial", "already serialized")?;
        Err(serde::ser::Error::custom(
            "intentional serialization failure",
        ))
    }
}

#[test]
fn flow_run_store_atomic_write() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("runs/example");
    let store = RunStore::new(&dir).unwrap();
    let initial = json!({"schema":1,"step":"plan","attempts":[],"nested":{"pid":42}});
    let next = json!({"schema":1,"step":"green","attempts":[1,2],"session":"opaque"});
    store.write_run(&initial).unwrap();
    let mut old_reader = fs::File::open(dir.join("run.json")).unwrap();
    let initial_bytes = fs::read(dir.join("run.json")).unwrap();
    store.write_run(&next).unwrap();
    assert_eq!(store.read_run::<Value>().unwrap(), Some(next.clone()));
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(dir.join("run.json")).unwrap()).unwrap(),
        next
    );
    // A descriptor opened before replacement must retain the previous file. An
    // in-place truncate/write can pass round-trip tests but fails this assertion.
    let mut original_snapshot = Vec::new();
    old_reader.read_to_end(&mut original_snapshot).unwrap();
    assert_eq!(original_snapshot, initial_bytes);
    drop(store);
    assert_eq!(
        RunStore::new(&dir).unwrap().read_run::<Value>().unwrap(),
        Some(next)
    );
}

#[test]
fn flow_run_store_recovers_from_partial_temp() {
    let root = tempfile::tempdir().unwrap();
    let store = RunStore::new(root.path()).unwrap();
    let committed = json!({"schema":1,"step":"owner_go","generation":7});
    store.write_run(&committed).unwrap();
    fs::write(root.path().join("run.json.tmp"), b"{\"step\":").unwrap();
    fs::write(root.path().join(".run.json.tmp"), b"{\"generation\":").unwrap();
    let reopened = RunStore::new(root.path()).unwrap();
    assert_eq!(reopened.read_run::<Value>().unwrap(), Some(committed));
    let advanced = json!({"schema":1,"step":"red","generation":8});
    reopened.write_run(&advanced).unwrap();
    assert_eq!(reopened.read_run::<Value>().unwrap(), Some(advanced));
}

#[test]
fn flow_run_store_does_not_promote_orphan_temp() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("run.json.tmp"), br#"{"uncommitted":true}"#).unwrap();
    fs::write(
        root.path().join(".run.json.tmp"),
        br#"{"uncommitted":true}"#,
    )
    .unwrap();
    let store = RunStore::new(root.path()).unwrap();
    assert_eq!(store.read_run::<Value>().unwrap(), None);
    assert!(!root.path().join("run.json").exists());
}

#[test]
fn flow_run_store_failed_serialization_preserves_committed_state() {
    let root = tempfile::tempdir().unwrap();
    let store = RunStore::new(root.path()).unwrap();
    let committed = json!({"step":"green","generation":2});
    store.write_run(&committed).unwrap();
    let before = fs::read(root.path().join("run.json")).unwrap();
    assert!(store.write_run(&BrokenSerialization).is_err());
    assert_eq!(fs::read(root.path().join("run.json")).unwrap(), before);
    assert_eq!(store.read_run::<Value>().unwrap(), Some(committed));
}

#[test]
fn flow_run_store_events_are_append_only_jsonl() {
    let root = tempfile::tempdir().unwrap();
    let store = RunStore::new(root.path()).unwrap();
    let first = json!({"event":"started","summary":"first\nsecond"});
    let second = json!({"event":"step_done","step":"plan"});
    store.append_event(&first).unwrap();
    let first_bytes = fs::read(root.path().join("events.jsonl")).unwrap();
    assert_eq!(first_bytes.iter().filter(|&&b| b == b'\n').count(), 1);
    assert_eq!(first_bytes.last(), Some(&b'\n'));
    let reopened = RunStore::new(root.path()).unwrap();
    reopened.append_event(&second).unwrap();
    let bytes = fs::read(root.path().join("events.jsonl")).unwrap();
    assert!(bytes.starts_with(&first_bytes));
    assert_eq!(bytes.last(), Some(&b'\n'));
    let lines: Vec<Value> = String::from_utf8(bytes.clone())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines, [first, second]);
    reopened.write_run(&json!({"step":"red"})).unwrap();
    assert_eq!(fs::read(root.path().join("events.jsonl")).unwrap(), bytes);
    assert!(reopened.append_event(&BrokenSerialization).is_err());
    assert_eq!(fs::read(root.path().join("events.jsonl")).unwrap(), bytes);
}

#[test]
fn flow_run_store_corrupt_committed_state_is_an_error() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("run.json"), b"{broken").unwrap();
    let result = RunStore::new(root.path()).and_then(|store| store.read_run::<Value>());
    let error = result.expect_err("corrupt committed state is not an absent run");
    assert!(format!("{error:#}").contains("run.json"));
}
