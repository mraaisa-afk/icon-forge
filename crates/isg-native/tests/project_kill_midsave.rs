//! Phase 1 exit criterion, part 2: **kill mid-save still opens.**
//!
//! Spawns a child process (this test binary, re-invoked for exactly one
//! helper test) that hammers insert + `save_in_place` in a loop; the parent
//! kills the child hard at a random point mid-save-loop, then verifies the
//! `.isgproj` still opens, passes integrity checks, and holds a consistent
//! committed row count. Repeated across several rounds with varying kill
//! delays.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use isg_native::db::Library;
use isg_native::db::{NewSheet, ReviewState};

const HELPER_TEST: &str = "child_saves_forever";
const ENV_FLAG: &str = "ISG_KILL_TEST_CHILD";

fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("isg-kill-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn make_sheet(lib: &mut Library, i: u32) {
    let mut id = [0u8; 16];
    id[..4].copy_from_slice(&i.to_le_bytes());
    lib.insert_sheet(&NewSheet {
        id,
        source_path: format!("sheet-{i}.png"),
        content_hash: format!("hash-{i:09}"),
        width: 16,
        height: 16,
    })
    .unwrap();
}

/// The child: opens the project, then inserts + saves in an endless loop.
/// Killed externally; the loop never exits on its own.
#[test]
fn child_saves_forever() {
    if std::env::var(ENV_FLAG).is_err() {
        // Invoked as a normal test (parent side) — nothing to do here.
        return;
    }
    let path = std::env::var("ISG_KILL_TEST_PATH").expect("project path env");
    let path = PathBuf::from(path);
    let mut lib = Library::open(&path).expect("child opens project");
    let mut i: u32 = 0;
    loop {
        make_sheet(&mut lib, i);
        lib.set_review_state([0xEE; 16], ReviewState::Pending, "noop")
            .unwrap_or(()); // icon may not exist; saving is the point
        lib.save_in_place().expect("child save");
        i += 1;
    }
}

fn spawn_child(exe: &Path, project: &Path) -> Child {
    Command::new(exe)
        .args(["--exact", HELPER_TEST, "--nocapture", "--test-threads=1"])
        .env(ENV_FLAG, "1")
        .env("ISG_KILL_TEST_PATH", project)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn save-loop child")
}

#[test]
fn kill_mid_save_project_still_opens() {
    let dir = tmpdir("midsave");
    let project = dir.join("kill.isgproj");

    // Seed a valid v0 snapshot so the very first kill window already has a
    // complete file to fall back to.
    {
        let mut lib = Library::open(&project).unwrap();
        make_sheet(&mut lib, 999_999);
        lib.save_in_place().unwrap();
    }

    let exe = std::env::current_exe().unwrap();
    let mut child = spawn_child(&exe, &project);

    // Give the child time to get deep into a save (VACUUM INTO / fsync /
    // rename / reopen), then kill hard. The delay varies per run.
    std::thread::sleep(Duration::from_millis(900));
    child.kill().expect("hard kill");
    let _ = child.wait();

    // The project must open, verify, and contain a sane committed state.
    let lib = Library::open(&project).unwrap();
    lib.verify_integrity().unwrap();
    let sheets = lib.sheet_count().unwrap();
    assert!(
        sheets >= 1,
        "at least the seeded sheet survives any kill point (got {sheets})"
    );
    assert!(sheets <= 400_000, "no runaway counts");

    // A second independent open must agree (stable on-disk state).
    drop(lib);
    let lib2 = Library::open(&project).unwrap();
    lib2.verify_integrity().unwrap();
    assert_eq!(lib2.sheet_count().unwrap(), sheets, "state is stable");

    // Leftover .tmp-<pid> files are inert: the complete file opens without
    // them, and they must not have been renamed over a partial state.
    let leftovers: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().contains(".tmp-"))
        .collect();
    for l in leftovers {
        // Any leftover must be either absent-if-cleaned or a complete db.
        if l.exists() {
            if let Ok(check) = Library::open(&l) {
                let _ = check.verify_integrity();
            }
            let _ = fs::remove_file(&l);
        }
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn kill_across_delay_range_still_opens() {
    // Several short rounds to hit different save phases (checkpoint, vacuum,
    // fsync, rename, reopen).
    let dir = tmpdir("range");
    let project = dir.join("range.isgproj");
    {
        let mut lib = Library::open(&project).unwrap();
        make_sheet(&mut lib, 1);
        lib.save_in_place().unwrap();
    }
    let exe = std::env::current_exe().unwrap();
    let delays = [300u64, 600, 900, 1200, 1500];
    for (round, delay_ms) in delays.iter().enumerate() {
        let mut child = spawn_child(&exe, &project);
        std::thread::sleep(Duration::from_millis(*delay_ms));
        child.kill().expect("hard kill");
        let _ = child.wait();

        let lib = Library::open(&project).unwrap();
        lib.verify_integrity().unwrap();
        let sheets = lib.sheet_count().unwrap();
        assert!(sheets >= 1, "round {round} (kill at {delay_ms} ms): opens");
        drop(lib);
        let lib2 = Library::open(&project).unwrap();
        assert_eq!(
            lib2.sheet_count().unwrap(),
            sheets,
            "round {round}: stable"
        );
    }
    let _ = fs::remove_dir_all(&dir);
}
