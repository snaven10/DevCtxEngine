//! The compaction lock (PLAN-009 TASK-018, invariant I1) and the names of the
//! files a compaction leaves next to a database.
//!
//! `<db>.compact.lock` is an ordinary file holding an operating-system lock:
//! a compaction takes it exclusively for its whole run, and every
//! [`Store::open`](crate::Store::open) of a database in a file takes it shared
//! and **keeps it** for as long as the store (or any clone of it) lives. So a
//! compaction cannot start while a store is open, and no store opens while a
//! compaction runs.
//!
//! Why a file of its own and not DuckDB's lock on the database: DuckDB's lock
//! is an `fcntl` lock, and POSIX drops every `fcntl` lock a process holds on a
//! file the moment the process closes *any* descriptor of that file. A
//! separate file, locked with `flock` (per open file description), has no such
//! trap. The file is never deleted (M-C): unlinking a lock file races with a
//! process that has just opened it and would lock an inode nobody else sees.
//!
//! Unix uses `flock(2)` through `libc` (`File::try_lock` needs Rust 1.89; the
//! MSRV is 1.82). Windows uses the share mode of the open itself, from `std`:
//! a shared holder opens allowing read and write sharing, the exclusive one
//! allows none, and either open fails with a sharing violation while the other
//! is held. Elsewhere there is no lock (the open always succeeds).

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

/// The files a compaction of `db` uses, all next to the real file (a symlink
/// is resolved first, so a `rename` stays on one filesystem).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactPaths {
    /// The database, canonical.
    pub db: PathBuf,
    /// Its write-ahead log.
    pub wal: PathBuf,
    /// `<db>.compact.tmp`: the copy being built.
    pub tmp: PathBuf,
    /// The copy's write-ahead log.
    pub tmp_wal: PathBuf,
    /// `<db>.compact.lock`.
    pub lock: PathBuf,
    /// `<db>.pre-compact.duckdb`: the memories sidecar.
    pub side: PathBuf,
    /// The sidecar while it is being written.
    pub side_tmp: PathBuf,
    /// Its write-ahead log.
    pub side_tmp_wal: PathBuf,
    /// `<db>.pre-compact`: the hard link `--keep-original` leaves.
    pub original: PathBuf,
}

impl CompactPaths {
    /// The names for `db`. Resolves a symlink when the file exists; otherwise
    /// resolves the directory, so a database about to be created gets the
    /// same names it will have once it exists.
    pub fn for_db(db: &Path) -> Self {
        let real =
            std::fs::canonicalize(db).unwrap_or_else(|_| match (db.parent(), db.file_name()) {
                (Some(dir), Some(name)) if !dir.as_os_str().is_empty() => {
                    std::fs::canonicalize(dir)
                        .map(|d| d.join(name))
                        .unwrap_or_else(|_| db.to_path_buf())
                }
                _ => db.to_path_buf(),
            });
        let with = |suffix: &str| {
            let mut s = real.clone().into_os_string();
            s.push(suffix);
            PathBuf::from(s)
        };
        Self {
            wal: with(".wal"),
            tmp: with(".compact.tmp"),
            tmp_wal: with(".compact.tmp.wal"),
            lock: with(".compact.lock"),
            side: with(".pre-compact.duckdb"),
            side_tmp: with(".pre-compact.duckdb.tmp"),
            side_tmp_wal: with(".pre-compact.duckdb.tmp.wal"),
            original: with(".pre-compact"),
            db: real,
        }
    }
}

/// How a [`FileLock`] is held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockMode {
    /// Any number of holders at once (an open store).
    Shared,
    /// One holder and no shared ones (a compaction).
    Exclusive,
}

/// An operating-system lock on a file, released when dropped (or when the
/// process dies, however it dies).
#[derive(Debug)]
pub struct FileLock {
    _file: File,
    mode: LockMode,
}

impl FileLock {
    /// The mode it is held in.
    pub fn mode(&self) -> LockMode {
        self.mode
    }

    /// Take the lock on `path` (created if absent) without waiting.
    /// `Ok(None)`: someone else holds it in a conflicting mode. `Err`: the file
    /// could not be opened at all (a read-only directory, say).
    pub fn try_acquire(path: &Path, mode: LockMode) -> io::Result<Option<Self>> {
        let file = match open_lock_file(path, mode) {
            Ok(f) => f,
            Err(e) if is_contention(&e) => return Ok(None),
            Err(e) => return Err(e),
        };
        match lock(&file, mode) {
            Ok(true) => Ok(Some(Self { _file: file, mode })),
            Ok(false) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

#[cfg(unix)]
fn open_lock_file(path: &Path, _mode: LockMode) -> io::Result<File> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .or_else(|e| {
            // A lock file someone else created and this user may only read:
            // `flock` works on a read-only descriptor too.
            if path.exists() {
                File::open(path)
            } else {
                Err(e)
            }
        })
}

#[cfg(unix)]
fn lock(file: &File, mode: LockMode) -> io::Result<bool> {
    use std::os::unix::io::AsRawFd;
    let op = match mode {
        LockMode::Shared => libc::LOCK_SH,
        LockMode::Exclusive => libc::LOCK_EX,
    } | libc::LOCK_NB;
    // SAFETY: `flock` on a descriptor this function borrows for the call.
    let rc = unsafe { libc::flock(file.as_raw_fd(), op) };
    if rc == 0 {
        return Ok(true);
    }
    let e = io::Error::last_os_error();
    if e.raw_os_error() == Some(libc::EWOULDBLOCK) {
        Ok(false)
    } else {
        Err(e)
    }
}

#[cfg(unix)]
fn is_contention(_e: &io::Error) -> bool {
    false
}

/// Windows: `FILE_SHARE_READ | FILE_SHARE_WRITE` for a shared holder, no
/// sharing for the exclusive one. Not verified on a Windows machine (PLAN-009
/// TASK-019 cross-check).
#[cfg(windows)]
fn open_lock_file(path: &Path, mode: LockMode) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_WRITE: u32 = 0x2;
    let share = match mode {
        LockMode::Shared => FILE_SHARE_READ | FILE_SHARE_WRITE,
        LockMode::Exclusive => 0,
    };
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(share)
        .open(path)
}

#[cfg(windows)]
fn lock(_file: &File, _mode: LockMode) -> io::Result<bool> {
    Ok(true)
}

/// `ERROR_SHARING_VIOLATION`: the open itself is the lock on Windows.
#[cfg(windows)]
fn is_contention(e: &io::Error) -> bool {
    e.raw_os_error() == Some(32)
}

#[cfg(not(any(unix, windows)))]
fn open_lock_file(path: &Path, _mode: LockMode) -> io::Result<File> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

#[cfg(not(any(unix, windows)))]
fn lock(_file: &File, _mode: LockMode) -> io::Result<bool> {
    Ok(true)
}

#[cfg(not(any(unix, windows)))]
fn is_contention(_e: &io::Error) -> bool {
    false
}

/// How long a sidecar is kept before an open removes it (M-D).
pub const SIDECAR_TTL: std::time::Duration = std::time::Duration::from_secs(30 * 24 * 3600);

/// What [`Store::open`](crate::Store::open) does about the compaction lock
/// before it opens a database in a file: take it shared and keep it, refuse
/// while a compaction holds it, and — holding it, so no compaction is alive —
/// remove what a dead one left (Paso 5).
///
/// Never deletes a temporary when the database itself is missing (I5): then
/// the temporary may be all there is, and the open is refused instead of
/// creating an empty database over the evidence. Never touches `<db>`,
/// `<db>.wal`, the lock file or a sidecar younger than [`SIDECAR_TTL`].
pub(crate) fn guard_open(db: &Path) -> crate::Result<Option<FileLock>> {
    let paths = CompactPaths::for_db(db);
    if !paths.db.exists() && paths.tmp.exists() {
        return Err(crate::StoreError::OrphanCompaction {
            db: paths.db,
            tmp: paths.tmp,
        });
    }
    let held = match FileLock::try_acquire(&paths.lock, LockMode::Shared) {
        Ok(Some(l)) => Some(l),
        Ok(None) => return Err(crate::StoreError::Compacting(paths.db)),
        // No lock file can be made (a read-only directory): no compaction can
        // run there either, since it needs to write its copy next to the base.
        Err(_) => None,
    };
    if held.is_some() && paths.db.exists() {
        for leftover in [
            &paths.tmp,
            &paths.tmp_wal,
            &paths.side_tmp,
            &paths.side_tmp_wal,
        ] {
            if leftover.exists() && std::fs::remove_file(leftover).is_ok() {
                eprintln!(
                    "· removed {}, left by a compaction that did not finish",
                    leftover.display()
                );
            }
        }
        let expired = std::fs::metadata(&paths.side)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > SIDECAR_TTL);
        if expired && std::fs::remove_file(&paths.side).is_ok() {
            eprintln!(
                "· removed {}, the memories saved by a compaction more than 30 days ago",
                paths.side.display()
            );
        }
    }
    Ok(held)
}

/// The lock a compaction holds, for callers outside this crate (a test that
/// must hold it, a command that must know whether one runs).
pub fn try_lock_exclusive(db: &Path) -> io::Result<Option<FileLock>> {
    FileLock::try_acquire(&CompactPaths::for_db(db).lock, LockMode::Exclusive)
}

/// Whether a compaction of `db` is running right now (its lock is held
/// exclusively). Takes and releases a shared lock; `false` when the lock file
/// cannot be opened.
pub fn compaction_running(db: &Path) -> bool {
    matches!(
        FileLock::try_acquire(&CompactPaths::for_db(db).lock, LockMode::Shared),
        Ok(None)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Store, StoreError};

    const DIM: usize = 3;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("devctx_oslock_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// TASK-018 test 4 (store part) and I1: with the compaction lock held, an
    /// open is refused with the typed error; with it released, it opens.
    #[test]
    fn an_open_is_refused_while_a_compaction_holds_the_lock() {
        let dir = scratch("refused");
        let db = dir.join("index.duckdb");
        drop(Store::open(&db, DIM).unwrap());
        let held = try_lock_exclusive(&db).unwrap().expect("nobody holds it");
        match Store::open(&db, DIM) {
            Err(StoreError::Compacting(p)) => assert_eq!(p, CompactPaths::for_db(&db).db),
            Err(e) => panic!("wrong error: {e}"),
            Ok(_) => panic!("opened during a compaction"),
        }
        let msg = StoreError::Compacting(db.clone()).to_string();
        assert!(msg.contains("being compacted"), "{msg}");
        assert!(compaction_running(&db));
        assert!(matches!(
            Store::check_unlocked(&db),
            Err(StoreError::Compacting(_))
        ));
        drop(held);
        assert!(!compaction_running(&db));
        Store::open(&db, DIM).expect("opens once the lock is released");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// M-C: an open store keeps the shared lock for as long as it — or any
    /// clone of it — lives, so a compaction cannot start under it; and the
    /// lock file is never deleted.
    #[test]
    fn an_open_store_keeps_the_shared_lock_until_its_last_clone_goes() {
        let dir = scratch("keeps");
        let db = dir.join("index.duckdb");
        let store = Store::open(&db, DIM).unwrap();
        let clone = store.try_clone().unwrap();
        assert!(try_lock_exclusive(&db).unwrap().is_none(), "store alive");
        drop(store);
        assert!(try_lock_exclusive(&db).unwrap().is_none(), "clone alive");
        drop(clone);
        let l = try_lock_exclusive(&db).unwrap();
        assert!(l.is_some(), "free once every connection is gone");
        drop(l);
        assert!(CompactPaths::for_db(&db).lock.exists(), "never deleted");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// M-B.1 in the small: on this filesystem a second exclusive attempt on
    /// another descriptor fails while the first is held.
    #[test]
    fn a_second_exclusive_lock_fails_while_the_first_is_held() {
        let dir = scratch("second");
        let db = dir.join("index.duckdb");
        let first = try_lock_exclusive(&db).unwrap().expect("free");
        assert_eq!(first.mode(), LockMode::Exclusive);
        assert!(try_lock_exclusive(&db).unwrap().is_none());
        let paths = CompactPaths::for_db(&db);
        assert!(FileLock::try_acquire(&paths.lock, LockMode::Shared)
            .unwrap()
            .is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Paso 5: an open (holding the lock, so no compaction is alive) removes
    /// what a dead compaction left — and never the database, its WAL, the lock
    /// or a fresh sidecar.
    #[test]
    fn an_open_removes_the_leftovers_of_a_dead_compaction() {
        let dir = scratch("leftovers");
        let db = dir.join("index.duckdb");
        drop(Store::open(&db, DIM).unwrap());
        let p = CompactPaths::for_db(&db);
        for f in [&p.tmp, &p.tmp_wal, &p.side_tmp, &p.side_tmp_wal, &p.side] {
            std::fs::write(f, b"x").unwrap();
        }
        drop(Store::open(&db, DIM).unwrap());
        for f in [&p.tmp, &p.tmp_wal, &p.side_tmp, &p.side_tmp_wal] {
            assert!(!f.exists(), "{} survived", f.display());
        }
        assert!(p.side.exists(), "a fresh sidecar is kept");
        assert!(p.db.exists() && p.lock.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// M-D: a sidecar older than 30 days goes at the next open.
    #[test]
    fn an_expired_sidecar_goes_at_the_next_open() {
        let dir = scratch("expired");
        let db = dir.join("index.duckdb");
        drop(Store::open(&db, DIM).unwrap());
        let p = CompactPaths::for_db(&db);
        std::fs::write(&p.side, b"x").unwrap();
        let old = std::time::SystemTime::now() - SIDECAR_TTL - std::time::Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(&p.side)
            .unwrap()
            .set_modified(old)
            .unwrap();
        // A sidecar kept under another name is never touched.
        let kept = dir.join("index.duckdb.pre-compact.1.duckdb");
        std::fs::write(&kept, b"x").unwrap();
        drop(Store::open(&db, DIM).unwrap());
        assert!(!p.side.exists());
        assert!(kept.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// TASK-018 test 8 (I5): a temporary next to a database that is not there
    /// is not deleted, and no empty database is created over it.
    #[test]
    fn a_temporary_without_its_database_is_neither_deleted_nor_covered() {
        let dir = scratch("orphan");
        let db = dir.join("index.duckdb");
        let p = CompactPaths::for_db(&db);
        std::fs::write(&p.tmp, b"the only copy").unwrap();
        match Store::open(&db, DIM) {
            Err(StoreError::OrphanCompaction { tmp, .. }) => assert_eq!(tmp, p.tmp),
            Err(e) => panic!("wrong error: {e}"),
            Ok(_) => panic!("created a database over an orphan temporary"),
        }
        assert_eq!(std::fs::read(&p.tmp).unwrap(), b"the only copy");
        assert!(!db.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// TASK-018 test 10 (names): a symlinked database gets its temporaries
    /// next to the real file.
    #[cfg(unix)]
    #[test]
    fn the_names_of_a_symlinked_database_are_next_to_the_real_file() {
        let dir = scratch("symlink");
        std::fs::create_dir_all(dir.join("real")).unwrap();
        let real = dir.join("real/index.duckdb");
        std::fs::write(&real, b"").unwrap();
        let link = dir.join("index.duckdb");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let p = CompactPaths::for_db(&link);
        let real = std::fs::canonicalize(&real).unwrap();
        assert_eq!(p.db, real);
        assert_eq!(p.tmp.parent(), real.parent());
        assert!(p
            .tmp
            .to_string_lossy()
            .ends_with("index.duckdb.compact.tmp"));
        assert!(p
            .lock
            .to_string_lossy()
            .ends_with("index.duckdb.compact.lock"));
        assert!(p
            .side
            .to_string_lossy()
            .ends_with("index.duckdb.pre-compact.duckdb"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
