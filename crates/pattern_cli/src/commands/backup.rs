//! `pattern backup {create,list,restore,info}` subcommand implementations.
//!
//! Each function is a one-shot operation that attaches to the nearest mount,
//! calls the `pattern_memory::backup` library, prints results, and detaches.
//! Underlying snapshot/rotation/restore logic is fully tested at the library
//! level; these functions are thin wiring.

use std::path::PathBuf;

use miette::{IntoDiagnostic, Result as MietteResult};
use pattern_memory::backup::snapshot::format_snapshot_name;

// ---------------------------------------------------------------------------
// create
// ---------------------------------------------------------------------------

/// Create an immediate snapshot of `messages.db` for the nearest mount.
pub fn cmd_backup_create(path: Option<PathBuf>) -> MietteResult<()> {
    let start = resolve_start(path)?;
    let paths =
        pattern_memory::paths::PatternPaths::default_paths().map_err(miette::Report::new)?;
    let store = pattern_memory::mount::attach(&start, None, None).map_err(miette::Report::new)?;

    let messages_db = store.db.messages_path().to_owned();
    let project_id = store.config.project.name.clone();

    let info = pattern_memory::backup::snapshot::create_snapshot(&messages_db, &paths, &project_id)
        .map_err(miette::Report::new)?;

    store.detach();

    println!("snapshot created: {}", info.path.display());
    println!("  timestamp : {}", format_snapshot_name(&info.timestamp));
    println!("  size      : {} bytes", info.size_bytes);
    // Display first 8 bytes of the blake3 hash as 16 hex chars.
    let hash_hex: String = info.content_hash[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    println!("  blake3    : {hash_hex}…");

    Ok(())
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

/// List all snapshots for the nearest mount, newest first.
pub fn cmd_backup_list(path: Option<PathBuf>) -> MietteResult<()> {
    let start = resolve_start(path)?;
    let paths =
        pattern_memory::paths::PatternPaths::default_paths().map_err(miette::Report::new)?;
    let store = pattern_memory::mount::attach(&start, None, None).map_err(miette::Report::new)?;
    let project_id = store.config.project.name.clone();
    store.detach();

    let snapshots = pattern_memory::backup::rotation::list_snapshots(&paths, &project_id)
        .map_err(miette::Report::new)?;

    if snapshots.is_empty() {
        println!("no snapshots for project {project_id}");
        println!("  run `pattern backup create` to create the first snapshot.");
    } else {
        println!("{:<24} {:>12}", "TIMESTAMP", "SIZE");
        println!("{}", "-".repeat(38));
        for s in &snapshots {
            println!(
                "{:<24} {:>10} B",
                format_snapshot_name(&s.timestamp),
                s.size_bytes,
            );
        }
        println!();
        println!("{} snapshot(s)", snapshots.len());
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// restore
// ---------------------------------------------------------------------------

/// Restore `messages.db` from a snapshot. The current state is saved to a
/// `.pre-restore-<ts>` file as a rollback safety net before the swap.
pub fn cmd_backup_restore(spec: String, path: Option<PathBuf>) -> MietteResult<()> {
    let start = resolve_start(path)?;
    let paths =
        pattern_memory::paths::PatternPaths::default_paths().map_err(miette::Report::new)?;
    let store = pattern_memory::mount::attach(&start, None, None).map_err(miette::Report::new)?;

    let messages_db = store.db.messages_path().to_owned();
    let project_id = store.config.project.name.clone();

    // Detach BEFORE restore — restore must run with no active pool on
    // messages.db (documented in backup::restore module).
    store.detach();

    let snapshot = pattern_memory::backup::restore::resolve_snapshot(&paths, &project_id, &spec)
        .map_err(miette::Report::new)?;

    let pre_restore =
        pattern_memory::backup::restore::restore_snapshot(&messages_db, &snapshot.path)
            .map_err(miette::Report::new)?;

    println!("restored from {}", snapshot.path.display());
    println!(
        "  timestamp : {}",
        format_snapshot_name(&snapshot.timestamp)
    );
    println!();
    println!("pre-restore state saved at:");
    println!("  {}", pre_restore.display());
    println!();
    println!(
        "to roll back: pattern backup restore --path . $(basename {})",
        pre_restore.display()
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// info
// ---------------------------------------------------------------------------

/// Show metadata for a specific snapshot.
pub fn cmd_backup_info(spec: String, path: Option<PathBuf>) -> MietteResult<()> {
    let start = resolve_start(path)?;
    let paths =
        pattern_memory::paths::PatternPaths::default_paths().map_err(miette::Report::new)?;
    let store = pattern_memory::mount::attach(&start, None, None).map_err(miette::Report::new)?;
    let project_id = store.config.project.name.clone();
    store.detach();

    let snapshot = pattern_memory::backup::restore::resolve_snapshot(&paths, &project_id, &spec)
        .map_err(miette::Report::new)?;

    // Compute hash on demand (list_snapshots leaves it as [0u8; 32]).
    let hash = pattern_memory::backup::snapshot::compute_snapshot_hash(&snapshot.path)
        .map_err(miette::Report::new)?;
    let hash_hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();

    println!("snapshot: {}", snapshot.path.display());
    println!(
        "  timestamp : {}",
        format_snapshot_name(&snapshot.timestamp)
    );
    println!("  size      : {} bytes", snapshot.size_bytes);
    println!("  blake3    : {hash_hex}");

    Ok(())
}

// ---------------------------------------------------------------------------
// Helper
// ---------------------------------------------------------------------------

/// Resolve an optional path argument, defaulting to `$PATTERN_HOME` or the
/// current working directory for mount discovery.
fn resolve_start(path: Option<PathBuf>) -> MietteResult<PathBuf> {
    match path {
        Some(p) => Ok(p),
        None => std::env::current_dir().into_diagnostic(),
    }
}
