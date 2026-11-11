//! Safe save for remote files: pure logic, blocking, no network, no UI.
//!
//! # Public API
//! * [`RemoteFs`] / [`Caps`] / [`Stat`] / [`FsError`]: the minimal provider contract
//!   (stat, realpath, read, write_new, write_truncate, set_mode, rename, rename_replace,
//!   remove, optional hash). SFTP / FTPS / cloud adapters implement it.
//! * [`RemoteDoc`]: `RemoteDoc::open(fs, profile, path)` returns the bytes plus the
//!   base (size, mtime, mode, sha256 of the received bytes). `RemoteDoc::new_file` for new files.
//! * [`SafeSaver::save`]: Dirty -> Queued -> Preflight -> Uploading -> Verify -> Swap -> Clean.
//!   Bytes are written verbatim (encoding/BOM/EOL/final newline untouched). Symlinks are
//!   resolved to the real file. Temp `.<name>.brouillon-<rand>.tmp` in the same dir, mode
//!   copied, size (+hash if offered) verified, atomic `rename_replace` when `caps.posix_rename`
//!   else a 3-step swap with immediate undo (`non_atomic: true`). Never writes in place
//!   unless `SaveOptions::force_in_place` (which forces a local backup first).
//!   Outcomes: [`SaveOutcome`] (`Saved`, `Conflict`, `Stale`, `RemoteGone`, `Queued`, `Failed`).
//! * [`SafeSaver::resolve_conflict`] with [`ConflictChoice`] (Diff, Merge, Overwrite, SaveAs,
//!   Reload, Cancel); the UI drives it, nothing here shows anything.
//! * [`SafeSaver::retry`] re-saves a Queued/Failed doc from the journal snapshot.
//! * [`Journal`]: dirty buffers persisted atomically (`<id>.json` + `<id>.bin`) before any
//!   server work; `discard` keeps them 7 days. [`SafeSaver::recover`] / `recover_all` run at
//!   launch: they remove our orphan temp files, put a swapped-out file back and clear an entry
//!   whose content is already on the server; everything else becomes `Recovered`. Recovery
//!   never uploads the buffer.
//! * [`BackupStore`]: local per-profile versions (default 20 or 30 days). Rollback =
//!   read a version and pass its bytes to `save`.
//! * [`fake::FakeFs`]: fault-injecting in-memory provider used by the crash-loop tests.
//!
//! The saver's own state changes are also visible as `doc.state` ([`SyncState`]) after each call.
//! Not here (later): merge algorithm, deploy mode, resume, lock files.

pub mod backup;
pub mod doc;
pub mod fake;
pub mod fs;
pub mod journal;
pub mod saver;

pub use backup::{BackupSource, BackupStore, Version};
pub use doc::{doc_id, Base, Eol, FileMeta, RemoteDoc, SyncState};
pub use fs::{sha256_hex, Caps, FsError, FsResult, Kind, RemoteFs, Stat};
pub use journal::{Entry, Journal};
pub use saver::{
    ConflictChoice, Fail, RecoveryReport, Resolution, SafeSaver, SaveOptions, SaveOutcome, Verification,
};
