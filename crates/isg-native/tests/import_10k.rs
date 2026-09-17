//! Phase 1 exit criterion, part 1: **10,000 files, 0 crashes.**
//!
//! Generates 10,000 tiny PNGs across nested directories, streams them into
//! a real WAL database on disk, then re-imports to prove deduplication.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use isg_native::cancel::CancellationToken;
use isg_native::db::Library;
use isg_native::import::{import_folder, ImportOptions};

struct Watchdog {
    stop: Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Watchdog {
    fn arm(name: &str) -> Self {
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop2 = Arc::clone(&stop);
        let label = name.to_string();
        let handle = std::thread::spawn(move || {
            for _ in 0..600 {
                if stop2.load(std::sync::atomic::Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            eprintln!("WATCHDOG: test {label} exceeded 60s");
            std::process::exit(101);
        });
        Self {
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Encodes an 8×8 grayscale PNG (valid zlib stream: header + one stored
/// deflate block + adler32) with a pixel pattern varying per index —
/// 10,000 distinct, deterministic, decodable contents.
fn write_png(path: &Path, index: u32) {
    use std::io::Write;
    let w: u32 = 8;
    let h: u32 = 8;
    // Raw grayscale scanlines with filter byte 0.
    let mut raw = Vec::with_capacity((w as usize + 1) * h as usize);
    for y in 0..h {
        raw.push(0u8);
        for x in 0..w {
            // Pixels (0,0) and (1,0) carry the index low/high bytes so every
            // index below 65,536 produces distinct content; the rest follow
            // a simple formula. Without the index bytes the formula repeats
            // every 256 indices (31 * 256 ≡ 0 mod 256) and dedupe would
            // collapse 10,000 files to 256.
            let v = match (x, y) {
                (0, 0) => (index & 0xFF) as u8,
                (1, 0) => ((index >> 8) & 0xFF) as u8,
                _ => ((index as u64 * 31 + (x + y) as u64 * 7) % 256) as u8,
            };
            raw.push(v);
        }
    }

    // zlib wrapper around a single stored (uncompressed) deflate block.
    let mut idat = Vec::with_capacity(raw.len() + 16);
    idat.push(0x78); // CMF: deflate, 32K window
    idat.push(0x01); // FLG: check bits, no dict, fastest
    idat.push(0x01); // BFINAL=1, BTYPE=00 (stored)
    idat.extend_from_slice(&(raw.len() as u16).to_le_bytes());
    idat.extend_from_slice(&(!(raw.len() as u16)).to_le_bytes());
    idat.extend_from_slice(&raw);
    let mut a1: u32 = 1;
    let mut a2: u32 = 0;
    for &b in &raw {
        a1 = (a1 + b as u32) % 65521;
        a2 = (a2 + a1) % 65521;
    }
    idat.extend_from_slice(&((a2 << 16) | a1).to_be_bytes());

    let chunk = |tag: [u8; 4], data: &[u8]| -> Vec<u8> {
        let mut c = Vec::with_capacity(12 + data.len());
        c.extend_from_slice(&(data.len() as u32).to_be_bytes());
        c.extend_from_slice(&tag);
        c.extend_from_slice(data);
        let mut crc: u32 = 0xFFFF_FFFF;
        for b in tag.iter().chain(data.iter()) {
            crc ^= *b as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        c.extend_from_slice(&(!crc).to_be_bytes());
        c
    };

    let mut ihdr_data = Vec::new();
    ihdr_data.extend_from_slice(&w.to_be_bytes());
    ihdr_data.extend_from_slice(&h.to_be_bytes());
    ihdr_data.extend_from_slice(&[8, 0, 0, 0, 0]); // 8-bit, grayscale

    let mut png = Vec::with_capacity(100 + idat.len());
    png.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    png.extend_from_slice(&chunk(*b"IHDR", &ihdr_data));
    png.extend_from_slice(&chunk(*b"IDAT", &idat));
    png.extend_from_slice(&chunk(*b"IEND", &[]));

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let mut f = fs::File::create(path).unwrap();
    f.write_all(&png).unwrap();
}

#[test]
fn import_ten_thousand_files_no_crashes() {
    let _wd = Watchdog::arm("import_ten_thousand_files_no_crashes");
    let dir = std::env::temp_dir().join(format!("isg-import-10k-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let tree = dir.join("tree");
    let db_path = dir.join("library.db");

    // 100 directories × 100 files = 10,000 unique PNGs.
    for d in 0..100u32 {
        for f in 0..100u32 {
            let index = d * 100 + f;
            let p = tree
                .join(format!("day{d:03}"))
                .join(format!("icon{f:03}.png"));
            write_png(&p, index);
        }
    }
    let file_count = 10_000usize;
    assert_eq!(fs::read_dir(tree.join("day000")).unwrap().count(), 100);

    let mut lib = Library::open(&db_path).unwrap();
    let cancel = CancellationToken::new();
    let started = Instant::now();
    let stats = import_folder(
        &mut lib,
        &tree,
        &ImportOptions::default(),
        &cancel,
        &mut |_| {},
    )
    .unwrap();
    let elapsed = started.elapsed();

    assert_eq!(stats.scanned as usize, file_count, "all candidates scanned");
    assert_eq!(stats.imported as usize, file_count, "all unique → imported");
    assert_eq!(stats.skipped_duplicate, 0);
    assert_eq!(stats.corrupt, 0, "0 corrupt — every generated PNG decodes");
    assert!(!stats.cancelled);
    assert_eq!(lib.sheet_count().unwrap() as usize, file_count);

    // Re-import: every file must dedupe by content hash.
    let stats2 = import_folder(
        &mut lib,
        &tree,
        &ImportOptions::default(),
        &cancel,
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(stats2.imported, 0);
    assert_eq!(stats2.skipped_duplicate as usize, file_count);
    assert_eq!(lib.sheet_count().unwrap() as usize, file_count);

    // Library page listing covers the whole range.
    let page = lib.list_sheets(0, 50).unwrap();
    assert_eq!(page.len(), 50);
    let last = lib.list_sheets(9_950, 100).unwrap();
    assert_eq!(last.len(), 50);

    lib.verify_integrity().unwrap();
    println!(
        "10k import: first pass {:?}, re-import {:?} (total {:?})",
        elapsed,
        started.elapsed() - elapsed,
        started.elapsed()
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn import_is_cancellable_and_resumable() {
    let _wd = Watchdog::arm("import_is_cancellable_and_resumable");
    let dir = std::env::temp_dir().join(format!("isg-import-cancel-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let tree = dir.join("tree");
    let db_path = dir.join("library.db");
    for i in 0..600u32 {
        write_png(&tree.join(format!("f{i:04}.png")), i);
    }

    let mut lib = Library::open(&db_path).unwrap();
    let cancel = CancellationToken::new();
    let batch = ImportOptions {
        max_file_bytes: 1 << 20,
        batch_size: 64,
    };
    // Cancel before starting: every checkpoint observes it immediately.
    cancel.cancel();
    let stats = import_folder(&mut lib, &tree, &batch, &cancel, &mut |_| {}).unwrap();
    assert!(stats.cancelled, "run reported cancellation");
    assert_eq!(stats.imported, 0, "nothing imported after cancellation");

    // A fresh token completes the same folder.
    let stats2 = import_folder(
        &mut lib,
        &tree,
        &batch,
        &CancellationToken::new(),
        &mut |_| {},
    )
    .unwrap();
    assert!(!stats2.cancelled);
    assert_eq!(stats2.imported, 600);
    let _ = fs::remove_dir_all(&dir);
}
