# Future Improvements

## Import/Export Progress Reporting

Currently, the CAR import/export operations emit tracing logs to show progress, but this is awkward for CLI integration. A better approach would be to provide progress-aware variants of these methods.

### Proposed Design

```rust
pub enum ImportProgress {
    StartingMemories { total: usize },
    MemoryStored { current: usize, total: usize },
    StartingMessages { total: usize },
    MessageStored { current: usize, total: usize },
    StartingAgents { total: usize },
    AgentStored { current: usize, total: usize },
    Complete,
}

pub async fn import_agent_from_car_with_progress(
    &self,
    input: impl AsyncRead + Unpin + Send,
    options: ImportOptions,
) -> Result<(ImportResult, mpsc::Receiver<ImportProgress>)> {
    let (tx, rx) = mpsc::channel(100);
    
    // Spawn task that performs import and sends progress updates
    let handle = tokio::spawn(async move {
        // ... import logic ...
        tx.send(ImportProgress::StartingMemories { total }).await;
        // ... store memories ...
        tx.send(ImportProgress::MemoryStored { current, total }).await;
        // etc
    });
    
    Ok((result, rx))
}
```

### Benefits

1. **Separation of concerns**: Core library doesn't need to know about UI
2. **Flexibility**: CLI can display progress however it wants (progress bar via `indicatif`, simple prints, etc)
3. **Testability**: Easy to test progress reporting independently
4. **Non-blocking**: Progress updates don't slow down the import

### CLI Integration Example

```rust
use indicatif::{ProgressBar, ProgressStyle};

let (result, mut progress_rx) = importer
    .import_agent_from_car_with_progress(input, options)
    .await?;

let pb = ProgressBar::new(0);
pb.set_style(ProgressStyle::default_bar()
    .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} {msg}")
    .progress_chars("##-"));

while let Some(progress) = progress_rx.recv().await {
    match progress {
        ImportProgress::StartingMessages { total } => {
            pb.set_length(total as u64);
            pb.set_message("Importing messages");
        }
        ImportProgress::MessageStored { current, .. } => {
            pb.set_position(current as u64);
        }
        // ... handle other progress events
    }
}

pb.finish_with_message("Import complete");
```

### Implementation Notes

- Should work for both import and export operations
- Consider batching progress updates to avoid overwhelming the channel (e.g., only send every N items or every X milliseconds)
- Could extend to group and constellation imports as well
- Progress events should include enough context for meaningful display without requiring state tracking in the CLI