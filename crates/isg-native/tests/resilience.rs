//! Phase 7 (hardening): what the app does when the disk is not cooperating.
//!
//! Phase 1 proved the happy-path guarantee — *kill mid-save still opens*. This
//! suite is the other half: the failure was not a clean kill but a torn write, a
//! scratched sector, a vanished file, or a leftover temporary from a crash, and
//! the question is what the user loses.
//!
//! Three rules the code is held to here:
//!
//! 1. **Derived data never blocks work.** The cache holds re-computable payloads.
//!    A payload that will not decompress, or whose file vanished, is a *miss* —
//!    the job re-traces one icon instead of failing the sheet.
//! 2. **A damaged project is refused, never guessed at.** Opening must return an
//!    error rather than silently migrating a garbage file into an empty database
//!    (which is how a user's library disappears while the app reports success).
//! 3. **A crash leaves inert files, not broken ones.** A `*.tmp-*` sibling is
//!    never read as a project and is swept on the next save.
//!
//! The SQLite behaviours the assertions rest on were measured before they were
//! asserted (see the per-test comments): a garbage file fails `journal_mode`,
//! a 50 %-truncated file fails it too, and a 90 %-truncated file *opens* but
//! fails `PRAGMA quick_check` — which is why the project-open path verifies
//! integrity as well as opening (and why the app's `project_open` does the same).

use std::fs;
use std::path::{Path, PathBuf};

use isg_native::cache::CacheStore;
use isg_native::db::{Library, NewSheet};

fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("isg-resilience-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).expect("scratch dir");
    d
}

fn make_sheet(lib: &mut Library, i: u32) {
    let mut id = [0u8; 16];
    id[..4].copy_from_slice(&i.to_le_bytes());
    lib.insert_sheet(&NewSheet {
        id,
        source_path: format!("sheet-{i}.png"),
        content_hash: format!("hash-{i:09}"),
        width: 32,
        height: 32,
    })
    .expect("insert sheet");
}

/// A project with `n` sheets, saved to disk. Returns the path.
fn saved_project(dir: &Path, n: u32) -> PathBuf {
    let path = dir.join("library.isgproj");
    let mut lib = Library::open(&path).expect("fresh project");
    for i in 0..n {
        make_sheet(&mut lib, i);
    }
    lib.save_in_place().expect("save");
    path
}

/// The app's own open path: open, then verify (see `commands::project_open`).
fn open_verified(path: &Path) -> Result<Library, String> {
    let lib = Library::open(path).map_err(|e| e.to_string())?;
    lib.verify_integrity().map_err(|e| e.to_string())?;
    Ok(lib)
}

// ---- derived data: the cache heals --------------------------------------

#[test]
fn a_corrupt_cache_payload_is_a_miss_and_the_next_put_heals_it() {
    let dir = tmpdir("cache-corrupt");
    let lib = Library::open_in_memory().expect("library");
    let store = CacheStore::new(dir.join("cache"));
    let key = CacheStore::cache_key(b"sheet bytes", "flat-8", "{bg:255}", 1);

    let path = store.put(&lib, &key, b"traced document").expect("put");
    assert_eq!(
        store.get(&lib, &key).expect("get"),
        Some(b"traced document".to_vec())
    );

    // A scratched sector: the file is there, its bytes are not a zstd frame.
    // This must be a miss — an error here fails a vectorize job over one bad
    // file, which is the failure mode Phase 7 exists to remove.
    fs::write(&path, b"\x00\x01\x02not a zstd frame at all\xff\xfe").expect("corrupt");
    assert_eq!(
        store
            .get(&lib, &key)
            .expect("a corrupt payload is not an error"),
        None,
        "a payload that will not decompress is a miss"
    );

    // A torn write: the frame is valid at the start and cut in the middle.
    store
        .put(&lib, &key, b"traced document")
        .expect("put again");
    let good = fs::read(&path).expect("payload on disk");
    fs::write(&path, &good[..good.len() / 2]).expect("truncate");
    assert_eq!(
        store
            .get(&lib, &key)
            .expect("a truncated payload is not an error"),
        None
    );

    // Healing: the caller recomputes and puts again; the row is untouched and
    // now points at good bytes.
    assert_eq!(
        store.put(&lib, &key, b"retraced").expect("healing put"),
        path,
        "a re-put writes the same path the index already names"
    );
    assert_eq!(
        store.get(&lib, &key).expect("get after heal"),
        Some(b"retraced".to_vec())
    );
    eprintln!(
        "evidence: phase7 H1 cache corrupt payload good_bytes={} garbage_get=miss \
         truncated_get=miss heal=same_path recovered=true",
        good.len()
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_cache_row_whose_file_vanished_is_a_miss() {
    let dir = tmpdir("cache-vanished");
    let lib = Library::open_in_memory().expect("library");
    let store = CacheStore::new(dir.join("cache"));
    let key = CacheStore::cache_key(b"other bytes", "mono-clean", "{}", 1);

    let path = store.put(&lib, &key, b"payload").expect("put");
    assert!(store.get(&lib, &key).expect("get").is_some());
    fs::remove_file(&path).expect("remove payload");
    assert_eq!(
        store
            .get(&lib, &key)
            .expect("a missing file is not an error"),
        None
    );
    assert_eq!(
        store.put(&lib, &key, b"payload").expect("put repairs"),
        path
    );
    assert!(store.get(&lib, &key).expect("get").is_some());
    eprintln!(
        "evidence: phase7 H1b cache vanished file -> miss, re-put recreates the same path=true"
    );
    let _ = fs::remove_dir_all(&dir);
}

// ---- the project file itself --------------------------------------------

#[test]
fn a_garbage_project_file_is_refused_not_guessed_at() {
    let dir = tmpdir("garbage-project");
    let path = dir.join("junk.isgproj");

    // Random bytes: SQLite fails to read the header. Measured beforehand:
    // `PRAGMA journal_mode=WAL` — which `Library::open` runs — reports
    // "file is not a database", so open fails rather than creating a schema.
    let mut junk = Vec::with_capacity(8192);
    for i in 0..8192u32 {
        junk.push((i.wrapping_mul(2654435761) >> 13) as u8);
    }
    fs::write(&path, &junk).expect("write junk");
    let refused = open_verified(&path);
    assert!(
        refused.is_err(),
        "a garbage file must be refused, not opened as a new project"
    );
    let message = refused.err().expect("error");
    assert!(
        message.to_lowercase().contains("database")
            || message.to_lowercase().contains("corrupt")
            || message.to_lowercase().contains("malformed"),
        "the refusal names the reason: {message}"
    );
    assert_eq!(
        fs::metadata(&path).expect("the file is still there").len(),
        junk.len() as u64,
        "refusing to open must not rewrite or truncate the user's file"
    );
    eprintln!(
        "evidence: phase7 H2 garbage project bytes={} open=refused reason={message:?} \
         file_untouched=true",
        junk.len()
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_truncated_project_is_caught_by_the_open_path() {
    let dir = tmpdir("truncated-project");
    let path = saved_project(&dir, 3);
    let whole = fs::read(&path).expect("read project");
    let opened = Library::open(&path).expect("the good project opens");
    assert_eq!(opened.sheet_count().expect("count"), 3);

    // Half the file and nine tenths of it. Which of the two a *bare* open
    // refuses depends on whether the header pages survived the cut, so that is
    // reported rather than asserted — the property that matters is the app's
    // open path, which opens and then verifies.
    let half = dir.join("half.isgproj");
    fs::write(&half, &whole[..whole.len() / 2]).expect("write half");
    let half_bare = Library::open(&half).is_ok();
    let half_verified = open_verified(&half).is_err();
    assert!(
        half_verified,
        "a half-truncated project is refused by the open path (open + verify_integrity)"
    );

    // Ninety per cent is the case the verification exists for: the header is
    // intact, so a bare open succeeds, and only `quick_check` sees the damage.
    // Without `verify_integrity` this is a library that opens, looks empty, and
    // swallows the next save.
    let nine = dir.join("nine-tenths.isgproj");
    fs::write(&nine, &whole[..whole.len() * 9 / 10]).expect("write 90%");
    let nine_bare = Library::open(&nine).is_ok();
    let nine_verified = open_verified(&nine).is_err();
    assert!(
        nine_verified,
        "the app's open path refuses the 90 % file even when the bare open succeeded"
    );

    // A zero-byte file is a *new* project, not damage: the app must still be
    // able to create one, and an empty schema is the right answer for it.
    let empty = dir.join("empty.isgproj");
    fs::write(&empty, b"").expect("write empty");
    let fresh = open_verified(&empty).expect("an empty file is a fresh project");
    assert_eq!(fresh.sheet_count().expect("count"), 0);

    eprintln!(
        "evidence: phase7 H3 project bytes={} half_bare_open={} half_verified=refused \
         ninetenths_bare_open={nine_bare} ninetenths_verified=refused \
         empty_file=fresh_project(0 sheets)",
        whole.len(),
        if half_bare { "opened" } else { "refused" }
    );
    let _ = fs::remove_dir_all(&dir);
}

// ---- crash leftovers ----------------------------------------------------

#[test]
fn a_leftover_save_temporary_is_inert_and_swept() {
    let dir = tmpdir("stale-tmp");
    let path = saved_project(&dir, 3);

    // What a crash between `VACUUM INTO` and the rename leaves behind: a
    // complete-looking sibling named `<target>.tmp-<pid>`, from a process that
    // is no longer running.
    let stale = dir.join("library.isgproj.tmp-99999");
    let partial = dir.join("library.isgproj.tmp-99998");
    fs::copy(&path, &stale).expect("copy a complete tmp");
    fs::write(&partial, b"half a vacuum").expect("write a partial tmp");

    // Neither is a project: opening the target is unaffected by their presence.
    let lib = open_verified(&path).expect("the project opens beside a stale tmp");
    assert_eq!(lib.sheet_count().expect("count"), 3);
    drop(lib);
    assert!(
        Library::open(&partial).is_err() || open_verified(&partial).is_err(),
        "a partial temporary is not openable as a project"
    );

    // The next save sweeps them, and only then — a stale tmp never blocks a
    // save, and never replaces the real file.
    let mut lib = Library::open(&path).expect("reopen");
    make_sheet(&mut lib, 3);
    lib.save_in_place().expect("save beside stale temps");
    assert_eq!(lib.sheet_count().expect("count"), 4);
    let leftovers: Vec<String> = fs::read_dir(&dir)
        .expect("read dir")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".tmp-"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "the save swept every stale temporary: {leftovers:?}"
    );
    drop(lib);
    let after = open_verified(&path).expect("the saved project opens");
    assert_eq!(
        after.sheet_count().expect("count"),
        4,
        "and kept the new row"
    );

    eprintln!(
        "evidence: phase7 H4 stale_tmp=2 partial_tmp_open=refused swept_by_save=true \
         rows_before=3 rows_after=4 leftovers=0"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_crash_snapshot_with_its_wal_replays_on_open() {
    // A power loss leaves the main file plus whatever WAL frames were committed.
    // The committed rows are the contract: opening the snapshot must show them.
    let dir = tmpdir("wal-snapshot");
    let path = saved_project(&dir, 0);
    let mut lib = Library::open(&path).expect("open");
    for i in 0..5 {
        make_sheet(&mut lib, i);
    }
    // Deliberately *not* checked in: the rows live in the WAL right now.
    let wal = dir.join("library.isgproj-wal");
    let wal_bytes = fs::metadata(&wal).map(|m| m.len()).unwrap_or(0);

    // Freeze the state mid-flight and recover from the copy, exactly as a
    // reboot would.
    let snap = dir.join("snapshot.isgproj");
    fs::copy(&path, &snap).expect("copy main file");
    if wal_bytes > 0 {
        fs::copy(&wal, dir.join("snapshot.isgproj-wal")).expect("copy wal");
    }
    drop(lib);

    let recovered = open_verified(&snap).expect("the snapshot opens and verifies");
    assert_eq!(
        recovered.sheet_count().expect("count"),
        5,
        "every committed row survives the crash"
    );
    eprintln!(
        "evidence: phase7 H5 wal_bytes={wal_bytes} committed_rows=5 recovered_rows=5 \
         integrity=ok (a crash snapshot is recoverable, not merely openable)"
    );
    let _ = fs::remove_dir_all(&dir);
}
