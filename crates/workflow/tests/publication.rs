use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Barrier,
        atomic::{AtomicU64, Ordering},
    },
    thread,
};

use mcbtodo_workflow::{PublicationError, RevisionStore, ScriptSources, WorkflowDraft, load_draft};

const GRAPH: &str = r#"schema: mcbtodo.workflow/v1
id: publication-test
name: Publication test
entry: check
nodes:
  check:
    kind: script
    runner: nushell
    source: scripts/check.nu
    timeout_seconds: 30
    capabilities: []
    outputs: {summary: string}
  report: {kind: action, action: report.create}
edges:
  - {from: check, to: report}
bindings:
  - {from: {node: check, port: summary}, to: {node: report, port: summary}}
"#;
const SCRIPT: &[u8] = b"# original\n\xff\0\r\n";
const SOURCE: &str = "scripts/check.nu";

#[test]
fn published_revision_loads_the_exact_bytes_after_the_draft_is_deleted() {
    let fixture = Fixture::new();
    let draft = load_draft(&fixture.graph).unwrap();
    let published = fixture.store.publish(&fixture.graph).unwrap();
    assert_eq!(published.hash(), draft.hash());
    fs::remove_dir_all(fixture.graph.parent().unwrap()).unwrap();

    let loaded = RevisionStore::new(&fixture.state)
        .load(published.hash())
        .unwrap();
    assert_eq!(loaded.graph_bytes(), GRAPH.as_bytes());
    assert_eq!(loaded.script_sources()[SOURCE], SCRIPT);
    assert_eq!(loaded.plan().workflow_id(), "publication-test");
    assert_eq!(loaded.plan().node_order(), draft.plan().node_order());
    assert_eq!(loaded.plan().script_hashes(), draft.plan().script_hashes());
}

#[test]
fn captured_snapshot_publishes_exact_bytes_after_draft_files_change() {
    let fixture = Fixture::new();
    let mut sources = ScriptSources::new();
    sources.insert(SOURCE.to_owned(), SCRIPT.to_vec());
    let draft = WorkflowDraft::new(GRAPH.to_owned(), sources).unwrap();
    let captured_hash = draft.hash().unwrap();

    fs::write(&fixture.graph, "# changed after proposal\n").unwrap();
    fs::write(fixture.source(), b"changed after proposal\n").unwrap();

    let published = fixture.store.publish_draft(&draft).unwrap();
    assert_eq!(published.hash(), captured_hash);
    let loaded = fixture.store.load(&captured_hash).unwrap();
    assert_eq!(loaded.graph_bytes(), GRAPH.as_bytes());
    assert_eq!(loaded.script_sources()[SOURCE], SCRIPT);
}

#[test]
fn publishing_a_deserialized_snapshot_revalidates_source_paths() {
    let fixture = Fixture::new();
    let mut sources = ScriptSources::new();
    sources.insert(SOURCE.to_owned(), SCRIPT.to_vec());
    let draft = WorkflowDraft::new(GRAPH.to_owned(), sources).unwrap();
    let mut serialized = serde_json::to_value(draft).unwrap();
    serialized["snapshot"]["scripts"] = serde_json::json!({"../outside.nu": [1, 2, 3]});
    let tampered: WorkflowDraft = serde_json::from_value(serialized).unwrap();

    assert!(fixture.store.publish_draft(&tampered).is_err());
    assert!(fixture.store.list().unwrap().is_empty());
}

#[test]
fn workflow_draft_rejects_missing_or_unexpected_sources() {
    let mut sources = ScriptSources::new();
    sources.insert("scripts/missing.nu".to_owned(), SCRIPT.to_vec());
    assert!(WorkflowDraft::new(GRAPH.to_owned(), sources).is_err());
}

#[test]
fn comment_and_script_edits_each_create_a_distinct_revision() {
    let fixture = Fixture::new();
    let original = fixture.store.publish(&fixture.graph).unwrap();
    let edited_graph = format!("# changed comment\r\n{GRAPH}");
    fs::write(&fixture.graph, &edited_graph).unwrap();
    let comment = fixture.store.publish(&fixture.graph).unwrap();
    fs::write(fixture.source(), b"# changed script\n").unwrap();
    let script = fixture.store.publish(&fixture.graph).unwrap();

    let mut expected = vec![original.hash(), comment.hash(), script.hash()];
    expected.sort_unstable();
    assert_eq!(fixture.store.list().unwrap(), expected);
    assert_ne!(original.hash(), comment.hash());
    assert_ne!(comment.hash(), script.hash());
    let original = fixture.store.load(original.hash()).unwrap();
    assert_eq!(original.graph_bytes(), GRAPH.as_bytes());
    assert_eq!(original.script_sources()[SOURCE], SCRIPT);
    let comment = fixture.store.load(comment.hash()).unwrap();
    assert_eq!(comment.graph_bytes(), edited_graph.as_bytes());
    assert_eq!(comment.script_sources()[SOURCE], SCRIPT);
}

#[test]
fn executable_input_is_required_and_pinned_in_the_publication() {
    const EXECUTABLE: &str = "bin/check";
    let fixture = Fixture::new();
    let graph = GRAPH
        .replace(
            "entry: check",
            "entry: check\ncapabilities: [{kind: process.exec, scope: bin/check}]",
        )
        .replace(
            "capabilities: []",
            "capabilities: [{kind: process.exec, scope: bin/check}]",
        );
    fs::write(&fixture.graph, &graph).unwrap();
    let program = fixture.graph.parent().unwrap().join(EXECUTABLE);
    assert!(fixture.store.publish(&fixture.graph).is_err());
    fs::create_dir(program.parent().unwrap()).unwrap();
    fs::write(&program, b"\x7fELF original").unwrap();
    let first = fixture.store.publish(&fixture.graph).unwrap();
    fs::write(&program, b"\x7fELF replacement").unwrap();
    let second = fixture.store.publish(&fixture.graph).unwrap();
    assert_ne!(first.hash(), second.hash());
    assert_ne!(first.plan().revision_hash(), second.plan().revision_hash());
    let restored = fixture.store.load(first.hash()).unwrap();
    assert_eq!(&restored.script_sources()[EXECUTABLE], b"\x7fELF original");
    fs::write(&fixture.graph, graph.replace(EXECUTABLE, "../outside")).unwrap();
    assert!(load_draft(&fixture.graph).is_err());
}

#[test]
fn concurrent_publishers_of_identical_content_share_one_revision() {
    let fixture = Fixture::new();
    let barrier = Barrier::new(4);
    let mut hashes = thread::scope(|scope| {
        let threads: Vec<_> = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    fixture
                        .store
                        .publish(&fixture.graph)
                        .unwrap()
                        .hash()
                        .to_owned()
                })
            })
            .collect();
        threads
            .into_iter()
            .map(|publisher| publisher.join().unwrap())
            .collect::<Vec<_>>()
    });
    hashes.dedup();
    assert_eq!(hashes.len(), 1);
    assert_eq!(fixture.store.list().unwrap(), hashes);
    assert_eq!(
        fs::read_dir(fixture.revisions_dir()).unwrap().count(),
        1,
        "publishers must clean up their staged files"
    );
}

#[test]
fn corrupt_revision_is_rejected_and_cannot_be_overwritten_by_publish() {
    let fixture = Fixture::new();
    let revision = fixture.store.publish(&fixture.graph).unwrap();
    let mut entries = fs::read_dir(fixture.revisions_dir()).unwrap();
    let path = entries.next().unwrap().unwrap().path();
    assert!(entries.next().is_none());
    let corrupt = fs::read(&path).unwrap();
    fs::write(fixture.source(), b"# different revision\n").unwrap();
    let other = fixture.store.publish(&fixture.graph).unwrap();
    // Put another valid snapshot at the first hash to test verification before parsing.
    let other_path = fs::read_dir(fixture.revisions_dir())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|entry| *entry != path)
        .unwrap();
    let replacement = fs::read(other_path).unwrap();
    assert_ne!(corrupt, replacement);
    fs::write(&path, &replacement).unwrap();
    fs::write(fixture.source(), SCRIPT).unwrap();
    for result in [
        fixture.store.load(revision.hash()),
        fixture.store.publish(&fixture.graph),
    ] {
        assert!(matches!(
            result,
            Err(PublicationError::HashMismatch { expected, actual })
                if expected == revision.hash() && actual == other.hash()
        ));
    }
    assert_eq!(fs::read(path).unwrap(), replacement);
}

#[test]
fn malformed_hashes_are_rejected_before_lookup() {
    let fixture = Fixture::new();
    for hash in ["", "../graph.yaml", &"G".repeat(64), &"A".repeat(64)] {
        assert!(matches!(
            fixture.store.load(hash),
            Err(PublicationError::InvalidHash(_))
        ));
    }
}

#[test]
fn missing_script_leaves_no_revision() {
    let fixture = Fixture::new();
    fs::remove_file(fixture.source()).unwrap();
    let error = fixture.store.publish(&fixture.graph).unwrap_err();
    assert!(error.to_string().contains(SOURCE));
    assert!(fixture.store.list().unwrap().is_empty());
    assert!(!fixture.state.exists());
}

#[test]
fn invalid_graph_leaves_no_revision() {
    let fixture = Fixture::new();
    fs::write(&fixture.graph, GRAPH.replace("to: report", "to: absent")).unwrap();
    let error = fixture.store.publish(&fixture.graph).unwrap_err();
    assert!(error.to_string().contains("unknown node"));
    assert!(fixture.store.list().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn script_symlink_cannot_escape_the_graph_directory() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let outside = fixture.root.join("outside.nu");
    fs::write(&outside, SCRIPT).unwrap();
    let source = fixture.source();
    fs::remove_file(&source).unwrap();
    symlink(outside, source).unwrap();
    let error = fixture.store.publish(&fixture.graph).unwrap_err();
    assert!(error.to_string().contains("must be inside"));
    assert!(fixture.store.list().unwrap().is_empty());
}

#[test]
fn draft_limits_script_bytes() {
    let fixture = Fixture::new();
    fs::File::create(fixture.source())
        .unwrap()
        .set_len(1024 * 1024 + 1)
        .unwrap();
    let error = load_draft(&fixture.graph).unwrap_err();
    assert!(error.to_string().contains("size limit"));
}

struct Fixture {
    root: PathBuf,
    state: PathBuf,
    graph: PathBuf,
    store: RevisionStore,
}

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = loop {
            let path = std::env::temp_dir().join(format!(
                "mcbtodo-publication-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => break path,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!("{error}"),
            }
        };
        let state = root.join(".mcbtodo/state");
        let graph = root.join("draft/graph.yaml");
        let fixture = Self {
            root,
            store: RevisionStore::new(&state),
            state,
            graph,
        };
        fs::create_dir_all(fixture.source().parent().unwrap()).unwrap();
        fs::write(&fixture.graph, GRAPH).unwrap();
        fs::write(fixture.source(), SCRIPT).unwrap();
        fixture
    }

    fn source(&self) -> PathBuf {
        self.graph.parent().unwrap().join(SOURCE)
    }

    fn revisions_dir(&self) -> PathBuf {
        self.state.join(Path::new("workflows/revisions"))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
