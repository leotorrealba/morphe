//! A crafted project must not be able to redirect design-memory writes outside
//! `.noos/` via symlinks.
//!
//! Each test plants a symlink (the `.noos` dir itself, the sidecar, the retry
//! queue, the graph or its temp file, the lockfile) pointing at a "victim"
//! file outside the design-memory directory, drives the real writer, and
//! asserts the victim is byte-for-byte untouched.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use morphe_ai::{
    enqueue_event, flush_queue, read_event_lines, rehydrate_events, write_event, DesignEvent,
    WriteError,
};

const VICTIM_CONTENT: &str = "# user's precious file\nexport PATH=/usr/bin\n";

/// A fresh scratch root holding a `.noos` dir and a victim file next to it.
/// Returns `(noos_dir, victim)`.
fn setup(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "morphe-ai-noos-symlink-{tag}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    let noos = root.join(".noos");
    fs::create_dir_all(&noos).expect("create .noos");
    let victim = root.join("victim.txt");
    fs::write(&victim, VICTIM_CONTENT).expect("seed victim");
    (noos, victim)
}

fn decision() -> DesignEvent {
    DesignEvent::Decision {
        path: "main.scad".to_string(),
        old: "a".to_string(),
        new: "b".to_string(),
    }
}

fn assert_untouched(victim: &Path) {
    assert_eq!(
        fs::read_to_string(victim).expect("read victim"),
        VICTIM_CONTENT,
        "the symlink target outside .noos must be untouched"
    );
}

#[test]
fn symlinked_sidecar_is_refused() {
    let (noos, victim) = setup("sidecar");
    symlink(&victim, noos.join("morphe-events.jsonl")).expect("plant link");

    let err = write_event(&decision(), &noos).expect_err("must refuse symlinked sidecar");
    assert!(matches!(err, WriteError::Events(_)), "got {err:?}");
    assert!(
        read_event_lines(&noos).is_err(),
        "reads must refuse it too (the digest renders sidecar text into the prompt)"
    );
    assert_untouched(&victim);
}

#[test]
fn dangling_sidecar_symlink_is_refused() {
    // A link to a not-yet-existing file must not be created through.
    let (noos, victim) = setup("dangling");
    let target = victim.with_file_name("created-through-link.txt");
    symlink(&target, noos.join("morphe-events.jsonl")).expect("plant link");

    write_event(&decision(), &noos).expect_err("must refuse dangling symlink");
    assert!(!target.exists(), "no file created through the link");
}

#[test]
fn symlinked_noos_dir_is_refused() {
    let (noos, victim) = setup("dir");
    // Replace `.noos` with a link to a directory outside the project.
    let outside = noos.with_file_name("outside");
    fs::create_dir_all(&outside).expect("outside dir");
    fs::remove_dir_all(&noos).expect("remove .noos");
    symlink(&outside, &noos).expect("plant dir link");

    let err = write_event(&decision(), &noos).expect_err("must refuse symlinked .noos");
    assert!(matches!(err, WriteError::Events(_)), "got {err:?}");
    assert!(read_event_lines(&noos).is_err());
    assert!(enqueue_event(&decision(), &noos).is_err());
    assert!(flush_queue(&noos).is_err());
    assert!(rehydrate_events(&noos).is_err());
    assert_eq!(
        fs::read_dir(&outside).expect("read outside").count(),
        0,
        "nothing written into the link target"
    );
    assert_untouched(&victim);
}

#[test]
fn symlinked_graph_is_refused() {
    let (noos, victim) = setup("graph");
    symlink(&victim, noos.join("graph.json")).expect("plant link");

    let err = write_event(&decision(), &noos).expect_err("must refuse symlinked graph");
    assert!(matches!(err, WriteError::Graph(_)), "got {err:?}");
    assert!(rehydrate_events(&noos).is_err());
    assert_untouched(&victim);
}

#[test]
fn symlinked_graph_tmp_is_not_written_through() {
    let (noos, victim) = setup("graph_tmp");
    let tmp = noos.join("graph.json.tmp");
    symlink(&victim, &tmp).expect("plant link");

    // The stale link is removed and a fresh temp file is created in its place;
    // the write succeeds without touching the link target.
    write_event(&decision(), &noos).expect("write succeeds");
    assert_untouched(&victim);
    assert!(
        fs::symlink_metadata(&tmp).is_err(),
        "temp file renamed into place"
    );
    let graph = fs::symlink_metadata(noos.join("graph.json")).expect("graph exists");
    assert!(graph.is_file(), "graph.json is a regular file");
}

#[test]
fn symlinked_lockfile_is_refused() {
    let (noos, victim) = setup("lock");
    symlink(&victim, noos.join("write.lock")).expect("plant link");

    let err = write_event(&decision(), &noos).expect_err("must refuse symlinked lock");
    assert!(matches!(err, WriteError::Lock(_)), "got {err:?}");
    assert_untouched(&victim);
}

#[test]
fn symlinked_queue_is_refused() {
    let (noos, victim) = setup("queue");
    symlink(&victim, noos.join("morphe-queue.jsonl")).expect("plant link");

    enqueue_event(&decision(), &noos).expect_err("enqueue must refuse symlinked queue");
    flush_queue(&noos).expect_err("flush must refuse symlinked queue");
    assert_untouched(&victim);
}

#[test]
fn non_regular_sidecar_is_refused() {
    let (noos, _victim) = setup("non_regular");
    fs::create_dir(noos.join("morphe-events.jsonl")).expect("sidecar as dir");

    let err = write_event(&decision(), &noos).expect_err("must refuse a directory sidecar");
    assert!(matches!(err, WriteError::Events(_)), "got {err:?}");
}
